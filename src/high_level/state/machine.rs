//! Type State Machine - 编译期状态安全
//!
//! 使用零大小类型（ZST）标记实现状态机，在编译期防止非法状态转换。

use std::marker::PhantomData;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::high_level::client::{
    motion_commander::MotionCommander, observer::Observer, raw_commander::RawCommander,
};
use crate::high_level::types::*;

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
pub struct Active<Mode>(PhantomData<Mode>);

// ==================== 控制模式类型（零大小类型）====================

/// MIT 模式
///
/// 支持位置、速度、力矩的混合控制。
pub struct MitMode;

/// 位置模式
///
/// 纯位置控制模式。
pub struct PositionMode;

/// 错误状态
///
/// 急停或其他错误发生后进入此状态。
/// 在此状态下，不允许发送任何运动控制命令。
pub struct ErrorState;

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
}

impl Default for MitModeConfig {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(2),
            debounce_threshold: 3,
            poll_interval: Duration::from_millis(10),
        }
    }
}

/// 位置模式配置（带 Debounce 参数）
#[derive(Debug, Clone)]
pub struct PositionModeConfig {
    /// 使能超时
    pub timeout: Duration,
    /// Debounce 阈值：连续 N 次读到 Enabled 才认为成功
    pub debounce_threshold: usize,
    /// 轮询间隔
    pub poll_interval: Duration,
}

impl Default for PositionModeConfig {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(2),
            debounce_threshold: 3,
            poll_interval: Duration::from_millis(10),
        }
    }
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
/// # 零开销
///
/// 状态类型是零大小类型（ZST），不占用任何运行时内存。
pub struct Piper<State = Disconnected> {
    pub(crate) robot: Arc<crate::robot::Piper>,
    pub(crate) observer: Observer,
    // 注意：StateMonitor 已移除（Observer 直接访问 robot::Piper）
    // HeartbeatManager 已确认不需要（根据 HEARTBEAT_ANALYSIS_REPORT.md）
    _state: PhantomData<State>,
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
        C: crate::can::SplittableAdapter + Send + 'static,
        C::RxAdapter: Send + 'static,
        C::TxAdapter: Send + 'static,
    {
        use crate::robot::Piper as RobotPiper;

        // ✅ 使用 robot 模块创建双线程模式的 Piper
        let robot = Arc::new(RobotPiper::new_dual_thread(can_adapter, None)?);

        // 等待接收到第一个有效反馈
        robot.wait_for_feedback(config.timeout)?;

        // 创建 Observer（View 模式）
        let observer = Observer::new(robot.clone());

        Ok(Piper {
            robot,
            observer,
            _state: PhantomData,
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
    /// # use piper_sdk::high_level::state::*;
    /// # use piper_sdk::high_level::types::*;
    /// # fn example(robot: Piper<Standby>) -> Result<()> {
    /// let robot = robot.enable_mit_mode(MitModeConfig::default())?;
    /// // 现在可以发送力矩命令
    /// # Ok(())
    /// # }
    /// ```
    pub fn enable_mit_mode(self, config: MitModeConfig) -> Result<Piper<Active<MitMode>>> {
        use crate::protocol::control::*;
        use crate::protocol::feedback::MoveMode;

        // 1. 发送使能指令
        let enable_cmd = MotorEnableCommand::enable_all();
        self.robot.send_reliable(enable_cmd.to_frame())?;

        // 2. 等待使能完成（带 Debounce）
        self.wait_for_enabled(
            config.timeout,
            config.debounce_threshold,
            config.poll_interval,
        )?;

        // 3. 设置 MIT 模式
        let control_cmd = ControlModeCommandFrame::new(
            ControlModeCommand::CanControl,
            MoveMode::MoveP,
            0,
            MitMode::Mit,
            0,
            InstallPosition::Invalid,
        );
        self.robot.send_reliable(control_cmd.to_frame())?;

        // 4. 状态转移（先 clone 字段，然后 forget self，避免 Drop 被调用）
        // 注意：由于 Piper 实现了 Drop，我们不能直接移动字段，需要先 clone
        let robot = self.robot.clone();
        let observer = self.observer.clone();

        // 5. 阻止 Drop 执行（避免状态转换时自动 disable）
        // 注意：所有可能 panic 的操作（如 send_reliable, wait_for_enabled）都已经完成
        std::mem::forget(self);

        // 6. 构造新结构体
        Ok(Piper {
            robot,
            observer,
            _state: PhantomData,
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
        use crate::protocol::control::*;
        use crate::protocol::feedback::MoveMode;

        // 1. 发送使能指令
        let enable_cmd = MotorEnableCommand::enable_all();
        self.robot.send_reliable(enable_cmd.to_frame())?;

        // 2. 等待使能完成（带 Debounce）
        self.wait_for_enabled(
            config.timeout,
            config.debounce_threshold,
            config.poll_interval,
        )?;

        // 3. 设置位置模式
        let control_cmd = ControlModeCommandFrame::new(
            ControlModeCommand::CanControl,
            MoveMode::MoveP,
            0,
            MitMode::PositionVelocity, // 位置模式
            0,
            InstallPosition::Invalid,
        );
        self.robot.send_reliable(control_cmd.to_frame())?;

        // 4. 状态转移（先 clone 字段，然后 forget self，避免 Drop 被调用）
        // 注意：由于 Piper 实现了 Drop，我们不能直接移动字段，需要先 clone
        let robot = self.robot.clone();
        let observer = self.observer.clone();

        // 5. 阻止 Drop 执行（避免状态转换时自动 disable）
        // 注意：所有可能 panic 的操作（如 send_reliable, wait_for_enabled）都已经完成
        std::mem::forget(self);

        // 6. 构造新结构体
        Ok(Piper {
            robot,
            observer,
            _state: PhantomData,
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
        use crate::protocol::control::MotorEnableCommand;

        for &joint in joints {
            let cmd = MotorEnableCommand::enable(joint.index() as u8);
            let frame = cmd.to_frame();
            self.robot.send_reliable(frame)?;
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
        use crate::protocol::control::MotorEnableCommand;

        let cmd = MotorEnableCommand::enable(joint.index() as u8);
        let frame = cmd.to_frame();
        self.robot.send_reliable(frame)?;

        Ok(self)
    }

    /// 失能全部关节
    ///
    /// # 返回
    ///
    /// 返回 `()`，因为失能后仍然保持 Standby 状态。
    pub fn disable_all(self) -> Result<()> {
        use crate::protocol::control::MotorEnableCommand;

        self.robot.send_reliable(MotorEnableCommand::disable_all().to_frame())?;
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
        use crate::protocol::control::MotorEnableCommand;

        for &joint in joints {
            let cmd = MotorEnableCommand::disable(joint.index() as u8);
            let frame = cmd.to_frame();
            self.robot.send_reliable(frame)?;
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
        // 发送急停指令（可靠队列，安全优先）
        let raw_commander = RawCommander::new(&self.robot);
        raw_commander.emergency_stop()?;

        // 状态转移：消耗旧 self，返回 ErrorState
        // 注意：所有可能 panic 的操作（如 send_reliable）都已经完成
        let robot = self.robot.clone();
        let observer = self.observer.clone();
        std::mem::forget(self);

        Ok(Piper {
            robot,
            observer,
            _state: PhantomData,
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

// ==================== Active<MitMode> 状态 ====================

impl Piper<Active<MitMode>> {
    /// 发送 MIT 模式力矩指令
    ///
    /// # 参数
    ///
    /// - `joint`: 目标关节
    /// - `position`: 目标位置（Rad）
    /// - `velocity`: 目标速度（rad/s）
    /// - `kp`: 位置增益
    /// - `kd`: 速度增益
    /// - `torque`: 前馈力矩（NewtonMeter）
    ///
    /// # 示例
    ///
    /// ```rust,ignore
    /// # use piper_sdk::high_level::state::*;
    /// # use piper_sdk::high_level::types::*;
    /// # fn example(robot: Piper<Active<MitMode>>) -> Result<()> {
    /// robot.command_torques(
    ///     Joint::J1,
    ///     Rad(1.0),
    ///     0.5,
    ///     10.0,
    ///     2.0,
    ///     NewtonMeter(5.0),
    /// )?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn command_torques(
        &self,
        joint: Joint,
        position: Rad,
        velocity: f64,
        kp: f64,
        kd: f64,
        torque: NewtonMeter,
    ) -> Result<()> {
        // ✅ 优化：使用引用而不是 Arc::clone，零开销
        let raw_commander = RawCommander::new(&self.robot);
        raw_commander.send_mit_command(joint, position, velocity, kp, kd, torque)
    }

    /// 获取 MotionCommander（受限权限）
    ///
    /// 返回一个可以克隆和传递的命令接口，但无法修改状态机。
    pub fn motion_commander(&self) -> MotionCommander {
        MotionCommander::new(self.robot.clone())
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
    /// # use piper_sdk::high_level::state::*;
    /// # fn example(robot: Piper<Active<MitMode>>) -> Result<()> {
    /// let robot = robot.disable(DisableConfig::default())?;  // Piper<Standby>
    /// # Ok(())
    /// # }
    /// ```
    pub fn disable(self, config: DisableConfig) -> Result<Piper<Standby>> {
        use crate::protocol::control::*;

        // 1. 失能机械臂
        let disable_cmd = MotorEnableCommand::disable_all();
        self.robot.send_reliable(disable_cmd.to_frame())?;

        // 2. 等待失能完成（带 Debounce）
        self.wait_for_disabled(
            config.timeout,
            config.debounce_threshold,
            config.poll_interval,
        )?;

        // 3. 状态转移（先 clone 字段，然后 forget self，避免 Drop 被调用）
        // 注意：由于 Piper 实现了 Drop，我们不能直接移动字段，需要先 clone
        let robot = self.robot.clone();
        let observer = self.observer.clone();

        // 4. 阻止 Drop 执行（避免重复 disable）
        // 注意：所有可能 panic 的操作（如 send_reliable, wait_for_disabled）都已经完成
        std::mem::forget(self);

        // 5. 构造新结构体
        Ok(Piper {
            robot,
            observer,
            _state: PhantomData,
        })
    }
}

// ==================== Active<PositionMode> 状态 ====================

impl Piper<Active<PositionMode>> {
    /// 发送位置指令
    ///
    /// **注意：** 位置控制指令（0x155、0x156、0x157）只包含位置信息，不包含速度。
    /// 速度需要通过控制模式指令（0x151）的 Byte 2（speed_percent）来设置。
    ///
    /// # 参数
    ///
    /// - `joint`: 目标关节
    /// - `position`: 目标位置（Rad）
    ///
    /// # 示例
    ///
    /// ```rust,ignore
    /// // 设置速度（通过控制模式指令）
    /// // 然后发送位置指令
    /// robot.command_position(Joint::J1, Rad(1.57))?;
    /// ```
    pub fn command_position(&self, joint: Joint, position: Rad) -> Result<()> {
        // ✅ 优化：使用引用而不是 Arc::clone，零开销
        let raw_commander = RawCommander::new(&self.robot);
        raw_commander.send_position_command(joint, position)
    }

    /// 获取 MotionCommander（受限权限）
    pub fn motion_commander(&self) -> MotionCommander {
        MotionCommander::new(self.robot.clone())
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
        use crate::protocol::control::*;

        // 1. 失能机械臂
        let disable_cmd = MotorEnableCommand::disable_all();
        self.robot.send_reliable(disable_cmd.to_frame())?;

        // 2. 等待失能完成（带 Debounce）
        self.wait_for_disabled(
            config.timeout,
            config.debounce_threshold,
            config.poll_interval,
        )?;

        // 3. 状态转移（先 clone 字段，然后 forget self，避免 Drop 被调用）
        // 注意：由于 Piper 实现了 Drop，我们不能直接移动字段，需要先 clone
        let robot = self.robot.clone();
        let observer = self.observer.clone();

        // 4. 阻止 Drop 执行（避免重复 disable）
        // 注意：所有可能 panic 的操作（如 send_reliable, wait_for_disabled）都已经完成
        std::mem::forget(self);

        // 5. 构造新结构体
        Ok(Piper {
            robot,
            observer,
            _state: PhantomData,
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
        use crate::protocol::control::MotorEnableCommand;
        let _ = self.robot.send_reliable(MotorEnableCommand::disable_all().to_frame());

        // 注意：HeartbeatManager 已确认不需要（根据 HEARTBEAT_ANALYSIS_REPORT.md）
        // StateMonitor 已移除
    }
}

// ==================== 测试 ====================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_state_types_are_zero_sized() {
        assert_eq!(std::mem::size_of::<Disconnected>(), 0);
        assert_eq!(std::mem::size_of::<Standby>(), 0);
        assert_eq!(std::mem::size_of::<Active<MitMode>>(), 0);
        assert_eq!(std::mem::size_of::<Active<PositionMode>>(), 0);
        assert_eq!(std::mem::size_of::<ErrorState>(), 0);
        assert_eq!(std::mem::size_of::<MitMode>(), 0);
        assert_eq!(std::mem::size_of::<PositionMode>(), 0);
    }

    #[test]
    fn test_mode_types_are_zero_sized() {
        assert_eq!(std::mem::size_of::<MitMode>(), 0);
        assert_eq!(std::mem::size_of::<PositionMode>(), 0);
    }

    #[test]
    fn test_phantom_data_overhead() {
        assert_eq!(std::mem::size_of::<PhantomData<Disconnected>>(), 0);
        assert_eq!(std::mem::size_of::<PhantomData<Active<MitMode>>>(), 0);
    }

    // 注意：集成测试位于 tests/ 目录
}
