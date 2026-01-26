//! Type State Machine - 编译期状态安全
//!
//! 使用零大小类型（ZST）标记实现状态机，在编译期防止非法状态转换。

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::types::*;
use crate::{observer::Observer, raw_commander::RawCommander};
use piper_protocol::control::InstallPosition;
use tracing::{info, trace};

// ==================== 状态类型（零大小类型）====================

/// 未连接状态
///
/// 这是初始状态，在此状态下无法进行任何操作。
pub struct Disconnected;

/// 待机状态
///
/// 已连接但未使能。可以读取状态，但不能发送运动命令。
pub struct Standby;

/// 活动状态（带控制模式）
///
/// 机械臂已使能，可以发送运动命令。
pub struct Active<Mode>(Mode);

// ==================== 控制模式类型（零大小类型）====================

/// MIT 模式
///
/// 支持位置、速度、力矩的混合控制。
pub struct MitMode;

/// 位置模式
///
/// 纯位置控制模式。
pub struct PositionMode {
    /// 发送策略配置
    pub(crate) send_strategy: SendStrategy,
}

impl PositionMode {
    /// 使用指定策略创建位置模式
    pub(crate) fn with_strategy(send_strategy: SendStrategy) -> Self {
        Self { send_strategy }
    }
}

/// 错误状态
///
/// 急停或其他错误发生后进入此状态。
/// 在此状态下，不允许发送任何运动控制命令。
pub struct ErrorState;

// ==================== 运动类型 ====================

/// 运动类型
///
/// 决定机械臂如何规划运动轨迹。
///
/// **注意**：此枚举用于配置 `PositionModeConfig`，与 `MoveMode` 协议枚举对应。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MotionType {
    /// 关节空间运动
    ///
    /// 各关节独立运动到目标角度，末端轨迹不可预测。
    /// 对应 MoveMode::MoveJ (0x01)，使用指令 0x155-0x157。
    #[default]
    Joint,

    /// 笛卡尔空间运动（点位模式）
    ///
    /// 末端从当前位置运动到目标位姿，轨迹由机械臂内部规划。
    /// 对应 MoveMode::MoveP (0x00)，使用指令 0x152-0x154。
    Cartesian,

    /// 直线运动
    ///
    /// 末端沿直线轨迹运动到目标位姿。
    /// 对应 MoveMode::MoveL (0x02)，使用指令 0x152-0x154。
    Linear,

    /// 圆弧运动
    ///
    /// 末端沿圆弧轨迹运动，需要指定起点、中点、终点。
    /// 对应 MoveMode::MoveC (0x03)，使用指令 0x152-0x154 + 0x158。
    Circular,

    /// 连续位置速度模式（V1.8-1+）
    ///
    /// 连续的位置和速度控制，适用于轨迹跟踪等场景。
    /// 对应 MoveMode::MoveCpv (0x05)。
    ///
    /// **注意**：此模式也属于 `Active<PositionMode>` 状态。
    ContinuousPositionVelocity,
}

impl From<MotionType> for piper_protocol::feedback::MoveMode {
    fn from(motion_type: MotionType) -> Self {
        use piper_protocol::feedback::MoveMode;
        match motion_type {
            MotionType::Joint => MoveMode::MoveJ,
            MotionType::Cartesian => MoveMode::MoveP,
            MotionType::Linear => MoveMode::MoveL,
            MotionType::Circular => MoveMode::MoveC,
            MotionType::ContinuousPositionVelocity => MoveMode::MoveCpv,
        }
    }
}

// ==================== 连接配置 ====================

/// 连接配置
#[derive(Debug, Clone)]
pub struct ConnectionConfig {
    /// CAN 接口名称（如 "can0"）
    pub interface: String,
    /// 连接超时
    pub timeout: Duration,
}

impl Default for ConnectionConfig {
    fn default() -> Self {
        ConnectionConfig {
            interface: "can0".to_string(),
            timeout: Duration::from_secs(5),
        }
    }
}

/// MIT 模式配置（带 Debounce 参数）
#[derive(Debug, Clone)]
pub struct MitModeConfig {
    /// 使能超时
    pub timeout: Duration,
    /// Debounce 阈值：连续 N 次读到 Enabled 才认为成功
    pub debounce_threshold: usize,
    /// 轮询间隔
    pub poll_interval: Duration,
    /// 运动速度百分比（0-100）
    ///
    /// 用于设置 0x151 指令的 Byte 2（speed_percent）。
    /// 默认值为 100，表示 100% 的运动速度。
    /// **重要**：不应设为 0，否则某些固件版本可能会锁死关节或报错。
    /// 虽然在纯 MIT 模式下（0x15A-0x15F），速度通常由控制指令本身携带，
    /// 但在发送 0x151 切换模式时，speed_percent 可能会作为安全限速或预设速度生效。
    pub speed_percent: u8,
}

impl Default for MitModeConfig {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(2),
            debounce_threshold: 3,
            poll_interval: Duration::from_millis(10),
            speed_percent: 100,
        }
    }
}

/// 位置模式配置（带 Debounce 参数）
///
/// **术语说明**：虽然名为 "PositionMode"，但实际支持多种运动规划模式
/// （关节空间、笛卡尔空间、直线、圆弧等），与 MIT 混合控制模式相对。
#[derive(Debug, Clone)]
pub struct PositionModeConfig {
    /// 使能超时
    pub timeout: Duration,
    /// Debounce 阈值：连续 N 次读到 Enabled 才认为成功
    pub debounce_threshold: usize,
    /// 轮询间隔
    pub poll_interval: Duration,
    /// 运动速度百分比（0-100）
    ///
    /// 用于设置 0x151 指令的 Byte 2（speed_percent）。
    /// 默认值为 50，表示 50% 的运动速度。
    /// 设置为 0 会导致机械臂不运动。
    pub speed_percent: u8,
    /// 安装位置
    ///
    /// 用于设置 0x151 指令的 Byte 5（installation_pos）。
    /// 默认值为 `InstallPosition::Invalid` (0x00)，表示无效值（不设置安装位置）。
    ///
    /// 根据官方 Python SDK，安装位置选项：
    /// - `InstallPosition::Invalid` (0x00): 无效值（默认）
    /// - `InstallPosition::Horizontal` (0x01): 水平正装
    /// - `InstallPosition::SideLeft` (0x02): 侧装左
    /// - `InstallPosition::SideRight` (0x03): 侧装右
    ///
    /// 注意：此参数基于 V1.5-2 版本后支持，注意接线朝后。
    pub install_position: InstallPosition,
    /// 运动类型（新增）
    ///
    /// 默认为 `Joint`（关节空间运动），保持向后兼容。
    ///
    /// **重要**：必须根据 `motion_type` 使用对应的控制方法：
    /// - `Joint`: 使用 `command_joint_positions()` 或 `motion_commander().send_position_command()`
    /// - `Cartesian`/`Linear`: 使用 `command_cartesian_pose()`
    /// - `Circular`: 使用 `move_circular()` 方法
    /// - `ContinuousPositionVelocity`: 待实现
    pub motion_type: MotionType,
    /// 发送策略（新增）
    ///
    /// 默认为 `SendStrategy::Auto`，根据命令类型自动选择：
    /// - 位置命令：使用 Reliable（队列模式，不丢失）
    /// - MIT 力控命令：使用 Realtime（邮箱模式，零延迟）
    ///
    /// **配置建议**：
    /// - 轨迹控制：保持 `Auto` 或显式设置为 `Reliable`
    /// - 高频力控：仅在 MIT 模式下设置为 `Realtime`
    pub send_strategy: SendStrategy,
}

impl Default for PositionModeConfig {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(2),
            debounce_threshold: 3,
            poll_interval: Duration::from_millis(10),
            speed_percent: 50,                          // 默认 50% 速度
            install_position: InstallPosition::Invalid, // 默认无效值（不设置安装位置）
            motion_type: MotionType::Joint,             // ✅ 默认关节模式，向后兼容
            send_strategy: SendStrategy::Auto,          // 默认自动选择策略
        }
    }
}

/// 发送策略配置
///
/// 决定不同类型命令的发送方式：
/// - **Realtime**：邮箱模式，零延迟，可覆盖（适用于高频力控）
/// - **Reliable**：队列模式，按顺序，不丢失（适用于轨迹控制）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SendStrategy {
    /// 自动选择（推荐）
    ///
    /// 根据控制模式自动选择最优策略：
    /// - MIT 模式：使用 Realtime
    /// - Position 模式：使用 Reliable
    #[default]
    Auto,

    /// 强制实时模式
    ///
    /// **使用场景**：超高频力控（>1kHz）
    /// **风险**：命令可能被覆盖
    Realtime,

    /// 强制可靠模式
    ///
    /// **使用场景**：轨迹控制、序列指令
    /// **保证**：命令按顺序发送，不丢失
    /// **配置**：可设置超时和到达确认
    Reliable {
        /// 单个命令发送超时（默认 10ms）
        timeout: Duration,

        /// 是否确认到达（默认 true）
        ///
        /// 如果启用，会阻塞等待每个命令完成（增加延迟）
        /// 如果禁用，只保证进入队列，不保证已发送
        check_arrival: bool,
    },
}

/// 失能配置（带 Debounce 参数）
#[derive(Debug, Clone)]
pub struct DisableConfig {
    /// 失能超时
    pub timeout: Duration,
    /// Debounce 阈值：连续 N 次读到 Disabled 才认为成功
    pub debounce_threshold: usize,
    /// 轮询间隔
    pub poll_interval: Duration,
}

impl Default for DisableConfig {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(2),
            debounce_threshold: 3,
            poll_interval: Duration::from_millis(10),
        }
    }
}

// ==================== Piper 状态机 ====================

/// Piper 机械臂（Type State Pattern）
///
/// 使用泛型参数 `State` 在编译期强制执行正确的状态转换。
///
/// # 类型参数
///
/// - `State`: 当前状态（`Disconnected`, `Standby`, `Active<Mode>`）
///
/// # 内存开销
///
/// 大部分状态是零大小类型（ZST），除了 `Active<PositionMode>` 包含 `send_strategy` 配置。
pub struct Piper<State = Disconnected> {
    pub(crate) driver: Arc<piper_driver::Piper>,
    pub(crate) observer: Observer,
    pub(crate) _state: State, // 改为直接存储状态（不再使用 PhantomData）
}

// ==================== Disconnected 状态 ====================

impl Piper<Disconnected> {
    /// 连接到机械臂
    ///
    /// # 参数
    ///
    /// - `can_adapter`: 可分离的 CAN 适配器（必须已启动）
    /// - `config`: 连接配置
    ///
    /// # 错误
    ///
    /// - `HighLevelError::Infrastructure`: CAN 设备初始化失败
    /// - `HighLevelError::Timeout`: 等待反馈超时
    pub fn connect<C>(can_adapter: C, config: ConnectionConfig) -> Result<Piper<Standby>>
    where
        C: piper_can::SplittableAdapter + Send + 'static,
        C::RxAdapter: Send + 'static,
        C::TxAdapter: Send + 'static,
    {
        use piper_driver::Piper as RobotPiper;

        // ✅ 使用 driver 模块创建双线程模式的 Piper
        let driver = Arc::new(RobotPiper::new_dual_thread(can_adapter, None)?);

        // 等待接收到第一个有效反馈
        driver.wait_for_feedback(config.timeout)?;

        // 创建 Observer（View 模式）
        let observer = Observer::new(driver.clone());

        Ok(Piper {
            driver,
            observer,
            _state: Standby,
        })
    }

    /// 重新连接到机械臂（用于连接丢失后重新建立连接）
    ///
    /// # 参数
    ///
    /// - `can_adapter`: 新的 CAN 适配器（或重用现有的）
    /// - `config`: 连接配置
    ///
    /// # 返回
    ///
    /// - `Ok(Piper<Standby>)`: 成功重新连接
    /// - `Err(RobotError)`: 重新连接失败
    ///
    /// # 示例
    ///
    /// ```rust,ignore
    /// # use piper_client::state::*;
    /// # fn example() -> Result<()> {
    /// let robot = Piper::connect(can_adapter, config)?;
    /// // ... 连接丢失 ...
    /// // 在某些情况下，你可能需要手动重新连接
    /// // 注意：这需要一个处于 Disconnected 状态的 Piper 实例
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// **注意**: 由于 `Disconnected` 是 ZST，`self` 参数本质上只是类型标记。
    /// 此方法与 `connect()` 功能相同，但语义上表示"重新连接"操作。
    pub fn reconnect<C>(self, can_adapter: C, config: ConnectionConfig) -> Result<Piper<Standby>>
    where
        C: piper_can::SplittableAdapter + Send + 'static,
        C::RxAdapter: Send + 'static,
        C::TxAdapter: Send + 'static,
    {
        info!("Attempting to reconnect to robot");

        // 1. 创建新的 driver 实例
        use piper_driver::Piper as RobotPiper;
        let driver = Arc::new(RobotPiper::new_dual_thread(can_adapter, None)?);

        // 2. 等待反馈
        driver.wait_for_feedback(config.timeout)?;

        // 3. 创建 observer
        let observer = Observer::new(driver.clone());

        // 4. 返回到 Standby 状态
        info!("Reconnection successful");
        Ok(Piper {
            driver,
            observer,
            _state: Standby,
        })
    }
}

// ==================== Standby 状态 ====================

impl Piper<Standby> {
    /// 使能 MIT 模式
    ///
    /// # 错误
    ///
    /// - `RobotError::Timeout`: 使能超时
    /// - `RobotError::HardwareError`: 硬件响应异常
    ///
    /// # 示例
    ///
    /// ```rust,ignore
    /// # use piper_client::state::*;
    /// # use piper_client::types::*;
    /// # fn example(robot: Piper<Standby>) -> Result<()> {
    /// let robot = robot.enable_mit_mode(MitModeConfig::default())?;
    /// // 现在可以发送力矩命令
    /// # Ok(())
    /// # }
    /// ```
    pub fn enable_mit_mode(self, config: MitModeConfig) -> Result<Piper<Active<MitMode>>> {
        use piper_protocol::control::*;
        use piper_protocol::feedback::MoveMode;

        // === PHASE 1: All operations that can panic ===

        // 1. 发送使能指令
        let enable_cmd = MotorEnableCommand::enable_all();
        self.driver.send_reliable(enable_cmd.to_frame())?;

        // 2. 等待使能完成（带 Debounce）
        self.wait_for_enabled(
            config.timeout,
            config.debounce_threshold,
            config.poll_interval,
        )?;

        // 3. 设置 MIT 模式
        // ✅ 关键修正：MoveMode 必须设为 MoveM (0x04)
        // 注意：需要固件版本 >= V1.5-2
        let control_cmd = ControlModeCommandFrame::new(
            ControlModeCommand::CanControl,
            MoveMode::MoveM,      // ✅ 修正：从 MoveP 改为 MoveM
            config.speed_percent, // ✅ 修正：使用配置的速度（默认100），避免设为0导致锁死
            MitMode::Mit,         // MIT 控制器 (0xAD)
            0,
            InstallPosition::Invalid,
        );
        self.driver.send_reliable(control_cmd.to_frame())?;

        // === PHASE 2: No-panic zone - must not panic after this point ===

        // Use ManuallyDrop to prevent Drop, then extract fields without cloning
        let this = std::mem::ManuallyDrop::new(self);

        // SAFETY: `this.driver` is a valid Arc<piper_driver::Piper>.
        // We're moving it out of ManuallyDrop, which prevents the original
        // `self` from being dropped. This is safe because:
        // 1. `this.driver` is immediately moved into the returned Piper
        // 2. No other access to `this.driver` occurs after this read
        // 3. The original `self` is never dropped (ManuallyDrop guarantees this)
        let driver = unsafe { std::ptr::read(&this.driver) };

        // SAFETY: `this.observer` is a valid Arc<Observer>.
        // Same safety reasoning as driver above.
        let observer = unsafe { std::ptr::read(&this.observer) };

        // `this` is dropped here, but since it's ManuallyDrop,
        // the inner `self` is NOT dropped, preventing double-disable

        // Construct new state (no Arc ref count increase!)
        Ok(Piper {
            driver,
            observer,
            _state: Active(MitMode),
        })
    }

    /// 使能位置模式
    ///
    /// # 错误
    ///
    /// - `RobotError::Timeout`: 使能超时
    /// - `RobotError::HardwareError`: 硬件响应异常
    pub fn enable_position_mode(
        self,
        config: PositionModeConfig,
    ) -> Result<Piper<Active<PositionMode>>> {
        use piper_protocol::control::*;
        use piper_protocol::feedback::MoveMode;

        // === PHASE 1: All operations that can panic ===

        // 1. 发送使能指令
        let enable_cmd = MotorEnableCommand::enable_all();
        self.driver.send_reliable(enable_cmd.to_frame())?;

        // 2. 等待使能完成（带 Debounce）
        self.wait_for_enabled(
            config.timeout,
            config.debounce_threshold,
            config.poll_interval,
        )?;

        // 3. 设置位置模式
        // ✅ 修改：使用配置的 motion_type
        let move_mode: MoveMode = config.motion_type.into();

        let control_cmd = ControlModeCommandFrame::new(
            ControlModeCommand::CanControl,
            move_mode, // ✅ 使用配置的运动类型
            config.speed_percent,
            MitMode::PositionVelocity,
            0,
            config.install_position,
        );
        self.driver.send_reliable(control_cmd.to_frame())?;

        // === PHASE 2: No-panic zone - must not panic after this point ===

        // Use ManuallyDrop to prevent Drop, then extract fields without cloning
        let this = std::mem::ManuallyDrop::new(self);

        // SAFETY: `this.driver` is a valid Arc<piper_driver::Piper>.
        // We're moving it out of ManuallyDrop, which prevents the original
        // `self` from being dropped. This is safe because:
        // 1. `this.driver` is immediately moved into the returned Piper
        // 2. No other access to `this.driver` occurs after this read
        // 3. The original `self` is never dropped (ManuallyDrop guarantees this)
        let driver = unsafe { std::ptr::read(&this.driver) };

        // SAFETY: `this.observer` is a valid Arc<Observer>.
        // Same safety reasoning as driver above.
        let observer = unsafe { std::ptr::read(&this.observer) };

        // `this` is dropped here, but since it's ManuallyDrop,
        // the inner `self` is NOT dropped, preventing double-disable

        // Construct new state with send_strategy from config (no Arc ref count increase!)
        let position_mode = PositionMode::with_strategy(config.send_strategy);
        Ok(Piper {
            driver,
            observer,
            _state: Active(position_mode),
        })
    }

    /// 使能全部关节并切换到 MIT 模式
    ///
    /// 这是 `enable_mit_mode` 的便捷方法，使用默认配置。
    pub fn enable_all(self) -> Result<Piper<Active<MitMode>>> {
        self.enable_mit_mode(MitModeConfig::default())
    }

    /// 使能指定关节（保持 Standby 状态）
    ///
    /// # 参数
    ///
    /// - `joints`: 要使能的关节列表
    ///
    /// # 返回
    ///
    /// 返回 `Piper<Standby>`，因为只是部分使能，不转换到 Active 状态。
    pub fn enable_joints(self, joints: &[Joint]) -> Result<Piper<Standby>> {
        use piper_protocol::control::MotorEnableCommand;

        for &joint in joints {
            let cmd = MotorEnableCommand::enable(joint.index() as u8);
            let frame = cmd.to_frame();
            self.driver.send_reliable(frame)?;
        }

        // 不转换状态，仍保持 Standby（部分使能）
        Ok(self)
    }

    /// 使能单个关节（保持 Standby 状态）
    ///
    /// # 参数
    ///
    /// - `joint`: 要使能的关节
    ///
    /// # 返回
    ///
    /// 返回 `Piper<Standby>`，因为只是部分使能，不转换到 Active 状态。
    pub fn enable_joint(self, joint: Joint) -> Result<Piper<Standby>> {
        use piper_protocol::control::MotorEnableCommand;

        let cmd = MotorEnableCommand::enable(joint.index() as u8);
        let frame = cmd.to_frame();
        self.driver.send_reliable(frame)?;

        Ok(self)
    }

    /// 失能全部关节
    ///
    /// # 返回
    ///
    /// 返回 `()`，因为失能后仍然保持 Standby 状态。
    pub fn disable_all(self) -> Result<()> {
        use piper_protocol::control::MotorEnableCommand;

        self.driver.send_reliable(MotorEnableCommand::disable_all().to_frame())?;
        Ok(())
    }

    /// 失能指定关节
    ///
    /// # 参数
    ///
    /// - `joints`: 要失能的关节列表
    ///
    /// # 返回
    ///
    /// 返回 `()`，因为失能后仍然保持 Standby 状态。
    pub fn disable_joints(self, joints: &[Joint]) -> Result<()> {
        use piper_protocol::control::MotorEnableCommand;

        for &joint in joints {
            let cmd = MotorEnableCommand::disable(joint.index() as u8);
            let frame = cmd.to_frame();
            self.driver.send_reliable(frame)?;
        }

        Ok(())
    }

    /// 获取 Observer（只读）
    ///
    /// 即使在 Standby 状态，也可以读取机械臂状态。
    pub fn observer(&self) -> &Observer {
        &self.observer
    }

    /// 等待机械臂使能完成（带 Debounce 机制）
    ///
    /// # 参数
    ///
    /// - `timeout`: 超时时间
    /// - `debounce_threshold`: Debounce 阈值：连续 N 次读到 Enabled 才认为成功
    /// - `poll_interval`: 轮询间隔
    ///
    /// # 错误
    ///
    /// - `RobotError::Timeout`: 等待超时
    /// - `RobotError::HardwareFailure`: 硬件反馈异常
    ///
    /// # 阻塞行为
    ///
    /// 此方法是**阻塞的 (Blocking)**，会阻塞当前线程直到使能完成或超时。
    /// 请不要在 `async` 上下文（如 Tokio）中直接调用此方法。
    fn wait_for_enabled(
        &self,
        timeout: Duration,
        debounce_threshold: usize,
        poll_interval: Duration,
    ) -> Result<()> {
        let start = Instant::now();
        let mut stable_count = 0;

        loop {
            // 细粒度超时检查
            if start.elapsed() > timeout {
                return Err(RobotError::Timeout {
                    timeout_ms: timeout.as_millis() as u64,
                });
            }

            // ✅ 直接从 Observer 读取状态（View 模式，零延迟）
            let enabled_mask = self.observer.joint_enabled_mask();

            if enabled_mask == 0b111111 {
                // ✅ Debounce：连续 N 次读到 Enabled 才认为成功
                stable_count += 1;
                if stable_count >= debounce_threshold {
                    return Ok(());
                }
            } else {
                // 状态跳变，重置计数器
                stable_count = 0;
            }

            // 检查剩余时间，避免不必要的 sleep
            let remaining = timeout.saturating_sub(start.elapsed());
            let sleep_duration = poll_interval.min(remaining);

            if sleep_duration.is_zero() {
                return Err(RobotError::Timeout {
                    timeout_ms: timeout.as_millis() as u64,
                });
            }

            std::thread::sleep(sleep_duration);
        }
    }
}

// ==================== 所有状态共享的辅助方法 ====================

impl<State> Piper<State> {
    /// 急停：发送急停指令，并转换到 ErrorState（之后不允许继续 command_*）
    ///
    /// # 设计说明
    ///
    /// - 急停属于"立即禁止后续指令"的软状态，若依赖硬件反馈会有窗口期
    /// - Type State 能在编译期/所有权层面强制禁止继续使用旧实例
    /// - 通过消耗 `self` 并返回 `Piper<ErrorState>`，确保无法继续发送控制命令
    ///
    /// # 参数
    ///
    /// - `self`: 消耗当前状态实例
    ///
    /// # 返回
    ///
    /// - `Ok(Piper<ErrorState>)`: 成功发送急停指令，返回错误状态
    /// - `Err(RobotError)`: 发送急停指令失败
    ///
    /// # 示例
    ///
    /// ```rust,ignore
    /// let robot = robot.enable_all()?;
    /// // 发生紧急情况，立即急停
    /// let robot = robot.emergency_stop()?;
    /// // robot 现在是 Piper<ErrorState>，无法调用 command_torques 等方法
    /// // robot.command_torques(...); // ❌ 编译错误
    /// ```
    pub fn emergency_stop(self) -> Result<Piper<ErrorState>> {
        // === PHASE 1: All operations that can panic ===

        // 发送急停指令（可靠队列，安全优先）
        let raw_commander = RawCommander::new(&self.driver);
        raw_commander.emergency_stop()?;

        // === PHASE 2: No-panic zone - must not panic after this point ===

        // Use ManuallyDrop to prevent Drop, then extract fields without cloning
        let this = std::mem::ManuallyDrop::new(self);

        // SAFETY: `this.driver` is a valid Arc<piper_driver::Piper>.
        // We're moving it out of ManuallyDrop, which prevents the original
        // `self` from being dropped. This is safe because:
        // 1. `this.driver` is immediately moved into the returned Piper
        // 2. No other access to `this.driver` occurs after this read
        // 3. The original `self` is never dropped (ManuallyDrop guarantees this)
        let driver = unsafe { std::ptr::read(&this.driver) };

        // SAFETY: `this.observer` is a valid Arc<Observer>.
        // Same safety reasoning as driver above.
        let observer = unsafe { std::ptr::read(&this.observer) };

        // `this` is dropped here, but since it's ManuallyDrop,
        // the inner `self` is NOT dropped, preventing double-disable

        // Construct new state (no Arc ref count increase!)
        Ok(Piper {
            driver,
            observer,
            _state: ErrorState,
        })
    }

    /// 等待机械臂失能完成（带 Debounce 机制）
    ///
    /// # 参数
    ///
    /// - `timeout`: 超时时间
    /// - `debounce_threshold`: Debounce 阈值：连续 N 次读到 Disabled 才认为成功
    /// - `poll_interval`: 轮询间隔
    ///
    /// # 错误
    ///
    /// - `RobotError::Timeout`: 等待超时
    /// - `RobotError::HardwareFailure`: 硬件反馈异常
    ///
    /// # 阻塞行为
    ///
    /// 此方法是**阻塞的 (Blocking)**，会阻塞当前线程直到失能完成或超时。
    /// 请不要在 `async` 上下文（如 Tokio）中直接调用此方法。
    fn wait_for_disabled(
        &self,
        timeout: Duration,
        debounce_threshold: usize,
        poll_interval: Duration,
    ) -> Result<()> {
        let start = Instant::now();
        let mut stable_count = 0;

        loop {
            if start.elapsed() > timeout {
                return Err(RobotError::Timeout {
                    timeout_ms: timeout.as_millis() as u64,
                });
            }

            let enabled_mask = self.observer.joint_enabled_mask();

            if enabled_mask == 0 {
                stable_count += 1;
                if stable_count >= debounce_threshold {
                    return Ok(());
                }
            } else {
                stable_count = 0;
            }

            let remaining = timeout.saturating_sub(start.elapsed());
            let sleep_duration = poll_interval.min(remaining);

            if sleep_duration.is_zero() {
                return Err(RobotError::Timeout {
                    timeout_ms: timeout.as_millis() as u64,
                });
            }

            std::thread::sleep(sleep_duration);
        }
    }
}

// ==================== Active<Mode> 状态（通用方法） ====================

impl<M> Piper<Active<M>> {
    /// 优雅关闭机械臂
    ///
    /// 执行完整的关闭序列：
    /// 1. 停止运动
    /// 2. 等待机器人停止（允许 CAN 命令传播）
    /// 3. 失能电机
    /// 4. 等待失能确认
    /// 5. 返回到 Standby 状态
    ///
    /// # 示例
    ///
    /// ```rust,ignore
    /// # use piper_client::state::*;
    /// # fn example(robot: Piper<Active<MitMode>>) -> Result<()> {
    /// let standby_robot = robot.shutdown()?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn shutdown(self) -> Result<Piper<Standby>> {
        use piper_protocol::control::*;
        use std::time::Duration;

        info!("Starting graceful robot shutdown");

        // === PHASE 1: All operations that can panic ===

        // 1. 停止运动
        trace!("Sending stop command");
        let raw = RawCommander::new(&self.driver);
        raw.stop_motion()?;

        // 2. 等待机器人停止
        //
        // ⚠️ INTENTIONAL HARD WAIT:
        // 这 100ms 的 sleep 允许 CAN 命令通过总线传播，
        // 并让机器人硬件处理停止命令，然后我们才失能电机。
        // 在关闭上下文中，硬等待是可接受的，因为：
        // - 关闭不是性能关键路径
        // - 我们需要确保硬件达到安全状态
        // - 替代方案（轮询"已停止"状态）不可靠
        trace!("Waiting for robot to stop (allowing CAN command propagation)");
        std::thread::sleep(Duration::from_millis(100));

        // 3. 失能电机
        trace!("Disabling motors");
        let disable_cmd = MotorEnableCommand::disable_all();
        self.driver.send_reliable(disable_cmd.to_frame())?;

        // 4. 等待失能确认
        trace!("Waiting for disable confirmation");
        self.wait_for_disabled(
            Duration::from_secs(1),
            1, // debounce_threshold
            Duration::from_millis(10),
        )?;

        info!("Robot shutdown complete");

        // === PHASE 2: No-panic zone - must not panic after this point ===

        // 使用 ManuallyDrop 模式转换到 Standby 状态
        let this = std::mem::ManuallyDrop::new(self);

        // SAFETY: `this.driver` is a valid Arc<piper_driver::Piper>.
        // We're moving it out of ManuallyDrop, which prevents the original
        // `self` from being dropped. This is safe because:
        // 1. `this.driver` is immediately moved into the returned Piper
        // 2. No other access to `this.driver` occurs after this read
        // 3. The original `self` is never dropped (ManuallyDrop guarantees this)
        let driver = unsafe { std::ptr::read(&this.driver) };

        // SAFETY: `this.observer` is a valid Arc<Observer>.
        // Same safety reasoning as driver above.
        let observer = unsafe { std::ptr::read(&this.observer) };

        Ok(Piper {
            driver,
            observer,
            _state: Standby,
        })
    }
}

// ==================== Active<MitMode> 状态 ====================

impl Piper<Active<MitMode>> {
    /// 发送 MIT 模式控制指令
    ///
    /// 对所有关节发送位置、速度、力矩的混合控制指令。
    ///
    /// # 参数
    ///
    /// - `positions`: 各关节目标位置（Rad）
    /// - `velocities`: 各关节目标速度（rad/s）
    /// - `kp`: 位置增益（所有关节相同）
    /// - `kd`: 速度增益（所有关节相同）
    /// - `torques`: 各关节前馈力矩（NewtonMeter）
    ///
    /// # 示例
    ///
    /// ```rust,ignore
    /// # use piper_client::state::*;
    /// # use piper_client::types::*;
    /// # fn example(robot: Piper<Active<MitMode>>) -> Result<()> {
    /// let positions = JointArray::from([
    ///     Rad(1.0), Rad(0.5), Rad(0.0), Rad(0.0), Rad(0.0), Rad(0.0)
    /// ]);
    /// let velocities = JointArray::from([0.5, 0.0, 0.0, 0.0, 0.0, 0.0]);
    /// let torques = JointArray::from([
    ///     NewtonMeter(5.0), NewtonMeter(0.0), NewtonMeter(0.0),
    ///     NewtonMeter(0.0), NewtonMeter(0.0), NewtonMeter(0.0)
    /// ]);
    /// robot.command_torques(&positions, &velocities, 10.0, 2.0, &torques)?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn command_torques(
        &self,
        positions: &JointArray<Rad>,
        velocities: &JointArray<f64>,
        kp: f64,
        kd: f64,
        torques: &JointArray<NewtonMeter>,
    ) -> Result<()> {
        // ✅ 直接使用 RawCommander，避免创建 MotionCommander
        let raw = RawCommander::new(&self.driver);
        raw.send_mit_command_batch(positions, velocities, kp, kd, torques)
    }

    /// 控制夹爪
    ///
    /// # 参数
    ///
    /// - `position`: 夹爪开口（0.0-1.0，1.0 = 完全打开）
    /// - `effort`: 夹持力度（0.0-1.0，1.0 = 最大力度）
    ///
    /// # 示例
    ///
    /// ```rust,ignore
    /// # use piper_client::state::*;
    /// # use piper_client::types::*;
    /// # fn example(robot: Piper<Active<MitMode>>) -> Result<()> {
    /// // 完全打开，低力度
    /// robot.set_gripper(1.0, 0.3)?;
    ///
    /// // 夹取物体，中等力度
    /// robot.set_gripper(0.2, 0.5)?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn set_gripper(&self, position: f64, effort: f64) -> Result<()> {
        // 参数验证
        if !(0.0..=1.0).contains(&position) {
            return Err(RobotError::ConfigError(
                "Gripper position must be in [0.0, 1.0]".to_string(),
            ));
        }
        if !(0.0..=1.0).contains(&effort) {
            return Err(RobotError::ConfigError(
                "Gripper effort must be in [0.0, 1.0]".to_string(),
            ));
        }

        let raw = RawCommander::new(&self.driver);
        raw.send_gripper_command(position, effort)
    }

    /// 打开夹爪
    ///
    /// 便捷方法，相当于 `set_gripper(1.0, 0.3)`
    pub fn open_gripper(&self) -> Result<()> {
        self.set_gripper(1.0, 0.3)
    }

    /// 关闭夹爪
    ///
    /// 便捷方法，相当于 `set_gripper(0.0, effort)`
    pub fn close_gripper(&self, effort: f64) -> Result<()> {
        self.set_gripper(0.0, effort)
    }

    /// 获取 Observer（只读）
    pub fn observer(&self) -> &Observer {
        &self.observer
    }

    /// 失能机械臂（返回 Standby 状态）
    ///
    /// # 参数
    ///
    /// - `config`: 失能配置（包含超时、Debounce 参数等）
    ///
    /// # 示例
    ///
    /// ```rust,ignore
    /// # use piper_client::state::*;
    /// # fn example(robot: Piper<Active<MitMode>>) -> Result<()> {
    /// let robot = robot.disable(DisableConfig::default())?;  // Piper<Standby>
    /// # Ok(())
    /// # }
    /// ```
    pub fn disable(self, config: DisableConfig) -> Result<Piper<Standby>> {
        use piper_protocol::control::*;

        // === PHASE 1: All operations that can panic ===

        // 1. 失能机械臂
        let disable_cmd = MotorEnableCommand::disable_all();
        self.driver.send_reliable(disable_cmd.to_frame())?;

        // 2. 等待失能完成（带 Debounce）
        self.wait_for_disabled(
            config.timeout,
            config.debounce_threshold,
            config.poll_interval,
        )?;

        // === PHASE 2: No-panic zone - must not panic after this point ===

        // Use ManuallyDrop to prevent Drop, then extract fields without cloning
        let this = std::mem::ManuallyDrop::new(self);

        // SAFETY: `this.driver` is a valid Arc<piper_driver::Piper>.
        // We're moving it out of ManuallyDrop, which prevents the original
        // `self` from being dropped. This is safe because:
        // 1. `this.driver` is immediately moved into the returned Piper
        // 2. No other access to `this.driver` occurs after this read
        // 3. The original `self` is never dropped (ManuallyDrop guarantees this)
        let driver = unsafe { std::ptr::read(&this.driver) };

        // SAFETY: `this.observer` is a valid Arc<Observer>.
        // Same safety reasoning as driver above.
        let observer = unsafe { std::ptr::read(&this.observer) };

        // `this` is dropped here, but since it's ManuallyDrop,
        // the inner `self` is NOT dropped, preventing double-disable

        // Construct new state (no Arc ref count increase!)
        Ok(Piper {
            driver,
            observer,
            _state: Standby,
        })
    }
}

// ==================== Active<PositionMode> 状态 ====================

impl Piper<Active<PositionMode>> {
    /// 发送位置命令（批量发送所有关节）
    ///
    /// 一次性发送所有 6 个关节的目标位置。
    ///
    /// # 参数
    ///
    /// - `positions`: 各关节目标位置
    ///
    /// # 示例
    ///
    /// ```rust,ignore
    /// # use piper_client::state::*;
    /// # use piper_client::types::*;
    /// # fn example(robot: Piper<Active<PositionMode>>) -> Result<()> {
    /// let positions = JointArray::from([
    ///     Rad(1.0), Rad(0.5), Rad(0.0), Rad(0.0), Rad(0.0), Rad(0.0)
    /// ]);
    /// robot.send_position_command(&positions)?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn send_position_command(&self, positions: &JointArray<Rad>) -> Result<()> {
        let raw = RawCommander::new(&self.driver);
        raw.send_position_command_batch(positions, self._state.0.send_strategy)
    }

    /// 发送末端位姿命令（笛卡尔空间控制）
    ///
    /// **前提条件**：必须使用 `MotionType::Cartesian` 或 `MotionType::Linear` 配置。
    ///
    /// # 参数
    ///
    /// - `position`: 末端位置（米）
    /// - `orientation`: 末端姿态（欧拉角，度）
    ///
    /// # 示例
    ///
    /// ```rust,ignore
    /// let config = PositionModeConfig {
    ///     motion_type: MotionType::Cartesian,
    ///     ..Default::default()
    /// };
    /// let robot = robot.enable_position_mode(config)?;
    ///
    /// // 发送末端位姿
    /// robot.command_cartesian_pose(
    ///     Position3D::new(0.3, 0.0, 0.2),           // x, y, z (米)
    ///     EulerAngles::new(0.0, 180.0, 0.0),        // roll, pitch, yaw (度)
    /// )?;
    /// ```
    pub fn command_cartesian_pose(
        &self,
        position: Position3D,
        orientation: EulerAngles,
    ) -> Result<()> {
        let raw = RawCommander::new(&self.driver);
        raw.send_end_pose_command(position, orientation, self._state.0.send_strategy)
    }

    /// 发送直线运动命令
    ///
    /// 末端沿直线轨迹运动到目标位姿。
    ///
    /// **前提条件**：必须使用 `MotionType::Linear` 配置。
    ///
    /// # 参数
    ///
    /// - `position`: 目标位置（米）
    /// - `orientation`: 目标姿态（欧拉角，度）
    ///
    /// # 示例
    ///
    /// ```rust,ignore
    /// let config = PositionModeConfig {
    ///     motion_type: MotionType::Linear,
    ///     ..Default::default()
    /// };
    /// let robot = robot.enable_position_mode(config)?;
    ///
    /// // 发送直线运动
    /// robot.move_linear(
    ///     Position3D::new(0.3, 0.0, 0.2),           // x, y, z (米)
    ///     EulerAngles::new(0.0, 180.0, 0.0),        // roll, pitch, yaw (度)
    /// )?;
    /// ```
    pub fn move_linear(&self, position: Position3D, orientation: EulerAngles) -> Result<()> {
        let raw = RawCommander::new(&self.driver);
        raw.send_end_pose_command(position, orientation, self._state.0.send_strategy)
    }

    /// 发送圆弧运动命令
    ///
    /// 末端沿圆弧轨迹运动，需要指定中间点和终点。
    ///
    /// **前提条件**：必须使用 `MotionType::Circular` 配置。
    ///
    /// # 参数
    ///
    /// - `via_position`: 中间点位置（米）
    /// - `via_orientation`: 中间点姿态（欧拉角，度）
    /// - `target_position`: 终点位置（米）
    /// - `target_orientation`: 终点姿态（欧拉角，度）
    ///
    /// # 示例
    ///
    /// ```rust,ignore
    /// let config = PositionModeConfig {
    ///     motion_type: MotionType::Circular,
    ///     ..Default::default()
    /// };
    /// let robot = robot.enable_position_mode(config)?;
    ///
    /// // 发送圆弧运动
    /// robot.move_circular(
    ///     Position3D::new(0.2, 0.1, 0.2),          // via: 中间点
    ///     EulerAngles::new(0.0, 90.0, 0.0),
    ///     Position3D::new(0.3, 0.0, 0.2),          // target: 终点
    ///     EulerAngles::new(0.0, 180.0, 0.0),
    /// )?;
    /// ```
    pub fn move_circular(
        &self,
        via_position: Position3D,
        via_orientation: EulerAngles,
        target_position: Position3D,
        target_orientation: EulerAngles,
    ) -> Result<()> {
        let raw = RawCommander::new(&self.driver);
        raw.send_circular_motion(
            via_position,
            via_orientation,
            target_position,
            target_orientation,
            self._state.0.send_strategy,
        )
    }
    /// 更新单个关节位置（保持其他关节不变）
    ///
    /// **注意**：此方法会先读取当前所有关节位置，然后只更新目标关节。
    /// 如果需要更新多个关节，请使用 `motion_commander().send_position_command_batch()` 方法。
    ///
    /// **为什么需要读取当前位置？**
    /// - 每个 CAN 帧（0x155, 0x156, 0x157）包含两个关节的角度
    /// - 如果只发送单个关节，另一个关节会被错误地设置为 0.0
    /// - 因此必须先读取当前位置，然后更新目标关节，最后批量发送
    ///
    /// # 参数
    ///
    /// - `joint`: 目标关节
    /// - `position`: 目标位置（Rad）
    ///
    /// # 示例
    ///
    /// ```rust,ignore
    /// // 只更新 J1，保持其他关节不变
    /// robot.command_position(Joint::J1, Rad(1.57))?;
    ///
    /// // 更新多个关节，使用批量方法
    /// let mut positions = robot.observer().joint_positions();
    /// positions[Joint::J1] = Rad(1.0);
    /// positions[Joint::J2] = Rad(0.5);
    /// robot.motion_commander().send_position_command(&positions)?;
    /// ```
    pub fn command_position(&self, joint: Joint, position: Rad) -> Result<()> {
        // 读取当前所有关节位置
        let mut positions = self.observer.joint_positions();
        // 只更新目标关节
        positions[joint] = position;
        // 批量发送所有关节（包括更新的和未更新的）
        self.send_position_command(&positions)
    }

    /// 控制夹爪
    ///
    /// # 参数
    ///
    /// - `position`: 夹爪开口（0.0-1.0，1.0 = 完全打开）
    /// - `effort`: 夹持力度（0.0-1.0，1.0 = 最大力度）
    pub fn set_gripper(&self, position: f64, effort: f64) -> Result<()> {
        // 参数验证
        if !(0.0..=1.0).contains(&position) {
            return Err(RobotError::ConfigError(
                "Gripper position must be in [0.0, 1.0]".to_string(),
            ));
        }
        if !(0.0..=1.0).contains(&effort) {
            return Err(RobotError::ConfigError(
                "Gripper effort must be in [0.0, 1.0]".to_string(),
            ));
        }

        let raw = RawCommander::new(&self.driver);
        raw.send_gripper_command(position, effort)
    }

    /// 打开夹爪
    ///
    /// 便捷方法，相当于 `set_gripper(1.0, 0.3)`
    pub fn open_gripper(&self) -> Result<()> {
        self.set_gripper(1.0, 0.3)
    }

    /// 关闭夹爪
    ///
    /// 便捷方法，相当于 `set_gripper(0.0, effort)`
    pub fn close_gripper(&self, effort: f64) -> Result<()> {
        self.set_gripper(0.0, effort)
    }

    /// 获取 Observer（只读）
    pub fn observer(&self) -> &Observer {
        &self.observer
    }

    /// 失能机械臂（返回 Standby 状态）
    ///
    /// # 参数
    ///
    /// - `config`: 失能配置（包含超时、Debounce 参数等）
    ///
    /// # 示例
    ///
    /// ```rust,ignore
    /// let robot = robot.disable(DisableConfig::default())?;
    /// ```
    pub fn disable(self, config: DisableConfig) -> Result<Piper<Standby>> {
        use piper_protocol::control::*;

        // === PHASE 1: All operations that can panic ===

        // 1. 失能机械臂
        let disable_cmd = MotorEnableCommand::disable_all();
        self.driver.send_reliable(disable_cmd.to_frame())?;

        // 2. 等待失能完成（带 Debounce）
        self.wait_for_disabled(
            config.timeout,
            config.debounce_threshold,
            config.poll_interval,
        )?;

        // === PHASE 2: No-panic zone - must not panic after this point ===

        // Use ManuallyDrop to prevent Drop, then extract fields without cloning
        let this = std::mem::ManuallyDrop::new(self);

        // SAFETY: `this.driver` is a valid Arc<piper_driver::Piper>.
        // We're moving it out of ManuallyDrop, which prevents the original
        // `self` from being dropped. This is safe because:
        // 1. `this.driver` is immediately moved into the returned Piper
        // 2. No other access to `this.driver` occurs after this read
        // 3. The original `self` is never dropped (ManuallyDrop guarantees this)
        let driver = unsafe { std::ptr::read(&this.driver) };

        // SAFETY: `this.observer` is a valid Arc<Observer>.
        // Same safety reasoning as driver above.
        let observer = unsafe { std::ptr::read(&this.observer) };

        // `this` is dropped here, but since it's ManuallyDrop,
        // the inner `self` is NOT dropped, preventing double-disable

        // Construct new state (no Arc ref count increase!)
        Ok(Piper {
            driver,
            observer,
            _state: Standby,
        })
    }
}

// ==================== ErrorState 状态 ====================

impl Piper<ErrorState> {
    /// 获取 Observer（只读）
    ///
    /// 即使在错误状态，也可以读取机械臂状态。
    pub fn observer(&self) -> &Observer {
        &self.observer
    }

    /// 检查是否处于错误状态
    ///
    /// 此方法总是返回 `true`，因为 `Piper<ErrorState>` 类型本身就表示错误状态。
    pub fn is_error_state(&self) -> bool {
        true
    }

    // 注意：ErrorState 不实现任何 command_* 方法，确保无法继续发送控制命令
    // 如果需要恢复，可以添加 `recover()` 方法返回 `Piper<Standby>`
}

// ==================== Drop 实现（安全关闭）====================

impl<State> Drop for Piper<State> {
    fn drop(&mut self) {
        // 尝试失能（忽略错误，因为可能已经失能）
        use piper_protocol::control::MotorEnableCommand;
        let _ = self.driver.send_reliable(MotorEnableCommand::disable_all().to_frame());

        // 注意：HeartbeatManager 已确认不需要（根据 HEARTBEAT_ANALYSIS_REPORT.md）
        // StateMonitor 已移除
    }
}

// ==================== 测试 ====================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_state_type_sizes() {
        // 大部分状态类型是 ZST（零大小类型）
        assert_eq!(std::mem::size_of::<Disconnected>(), 0);
        assert_eq!(std::mem::size_of::<Standby>(), 0);
        assert_eq!(std::mem::size_of::<MitMode>(), 0);
        assert_eq!(std::mem::size_of::<ErrorState>(), 0);

        // Active<MitMode> 包含 MitMode（ZST），所以也是 ZST
        assert_eq!(std::mem::size_of::<Active<MitMode>>(), 0);

        // PositionMode 包含 SendStrategy，不是 ZST
        assert!(std::mem::size_of::<PositionMode>() > 0);
        assert!(std::mem::size_of::<Active<PositionMode>>() > 0);
    }

    #[test]
    fn test_mit_mode_config_default() {
        let config = MitModeConfig::default();
        assert_eq!(config.timeout, Duration::from_secs(2));
        assert_eq!(config.debounce_threshold, 3);
        assert_eq!(config.poll_interval, Duration::from_millis(10));
        assert_eq!(config.speed_percent, 100);
    }

    #[test]
    fn test_motion_type_to_move_mode() {
        use piper_protocol::feedback::MoveMode;

        assert_eq!(MoveMode::from(MotionType::Joint), MoveMode::MoveJ);
        assert_eq!(MoveMode::from(MotionType::Cartesian), MoveMode::MoveP);
        assert_eq!(MoveMode::from(MotionType::Linear), MoveMode::MoveL);
        assert_eq!(MoveMode::from(MotionType::Circular), MoveMode::MoveC);
        assert_eq!(
            MoveMode::from(MotionType::ContinuousPositionVelocity),
            MoveMode::MoveCpv
        );
    }

    #[test]
    fn test_position_mode_config_default() {
        let config = PositionModeConfig::default();
        assert_eq!(config.motion_type, MotionType::Joint); // 向后兼容
        assert_eq!(config.speed_percent, 50);
    }

    #[test]
    fn test_motion_type_default() {
        assert_eq!(MotionType::default(), MotionType::Joint);
    }

    // 注意：集成测试位于 tests/ 目录
}
