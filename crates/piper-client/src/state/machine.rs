//! Type State Machine - 编译期状态安全
//!
//! 使用零大小类型（ZST）标记实现状态机，在编译期防止非法状态转换。

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::connection::initialize_connected_driver;
use crate::state::capability::{
    CapabilityMarker, MonitorOnly, MotionCapability, SoftRealtime, StrictCapability,
    StrictRealtime, UnspecifiedCapability,
};
use crate::types::*;
use crate::{
    observer::{CollisionProtectionSnapshot, MonitorReadPolicy, Observer, RuntimeHealthSnapshot},
    raw_commander::RawCommander,
};
use piper_driver::BackendCapability;
use piper_protocol::control::{InstallPosition, MitControlCommand, MitMode as ProtocolMitMode};
use piper_protocol::feedback::{ControlMode, MoveMode, RobotStatus};
use tracing::{debug, info, trace};

const COLLISION_QUERY_POLL_INTERVAL: Duration = Duration::from_millis(10);

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

/// SoftRealtime MIT 透传模式
///
/// 只允许提交原始 6 关节 MIT 批命令，不提供宿主机闭环 helper。
pub struct MitPassthroughMode;

/// 位置模式
///
/// 纯位置控制模式。
pub struct PositionMode {
    pub(crate) command_timeout: Duration,
    pub(crate) motion_type: MotionType,
}

/// 错误状态
///
/// 急停或其他错误发生后进入此状态。
/// 在此状态下，不允许发送任何运动控制命令。
pub struct ErrorState;

/// 回放模式状态
///
/// 用于安全地回放预先录制的 CAN 帧。
///
/// # 设计目的
///
/// - 暂停 TX 线程的周期性发送
/// - 避免双控制流冲突
/// - 允许精确控制帧发送时机
///
/// # 转换规则
///
/// - **进入**: 从 `Standby` 通过 `enter_replay_mode()` 进入
/// - **退出**: 通过 `stop_replay()` 返回到 `Standby`
///
/// # 安全特性
///
/// - 在 ReplayMode 下，无法调用 `enable_*()` 方法
/// - 所有周期性发送的控制指令都会被暂停
/// - 只能通过 `replay_recording()` 发送预先录制的帧
///
/// # 使用场景
///
/// - 回放预先录制的运动轨迹
/// - 测试和验证录制的 CAN 帧序列
/// - 调试和分析工具
///
/// # 示例
///
/// ```rust,ignore
/// # use piper_client::{PiperBuilder};
/// # fn main() -> anyhow::Result<()> {
/// let robot = PiperBuilder::new()
///     .socketcan("can0")
///     .build()?;
///
/// let standby = robot.connect()?;
///
/// // 进入回放模式
/// let replay = standby.enter_replay_mode()?;
///
/// // 回放录制（1.0x 速度，原始速度）
/// let standby = replay.replay_recording("recording.bin", 1.0)?;
///
/// // 回放完成后自动返回 Standby 状态
/// # Ok(())
/// # }
/// ```
pub struct ReplayMode;

#[derive(Clone, Copy)]
struct ModeConfirmationExpectation {
    control_mode: u8,
    move_mode: u8,
    speed_percent: u8,
    mit_mode: u8,
    install_position: u8,
    trajectory_stay_time: u8,
}

#[derive(Clone)]
struct ModeConfirmationBaseline {
    robot_control: piper_driver::RobotControlState,
    mode_echo: piper_driver::MasterSlaveControlModeState,
}

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
    /// 等待第一帧反馈的超时。
    pub feedback_timeout: Duration,
    /// 固件版本握手超时。
    pub firmware_timeout: Duration,
}

impl Default for ConnectionConfig {
    fn default() -> Self {
        ConnectionConfig {
            feedback_timeout: Duration::from_secs(5),
            firmware_timeout: Duration::from_millis(100),
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
    /// 多帧任务型运动命令的整包发送超时。
    pub command_timeout: Duration,
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
            command_timeout: Duration::from_millis(20),
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
/// # 内存开销
///
/// 大部分状态是零大小类型（ZST）。
pub struct Piper<State = Disconnected, Capability = UnspecifiedCapability> {
    pub(crate) driver: Arc<piper_driver::Piper>,
    pub(crate) observer: Observer<Capability>,
    pub(crate) quirks: DeviceQuirks,
    pub(crate) drop_policy: DropPolicy,
    pub(crate) driver_mode_drop_policy: DriverModeDropPolicy,
    pub(crate) _state: State, // 改为直接存储状态（不再使用 PhantomData）
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DropPolicy {
    Noop,
    DisableAll,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DriverModeDropPolicy {
    Preserve,
    RestoreNormal,
}

pub enum ConnectedPiper {
    Strict(Piper<Standby, StrictRealtime>),
    Soft(Piper<Standby, SoftRealtime>),
    Monitor(Piper<Standby, MonitorOnly>),
}

impl ConnectedPiper {
    pub fn backend_capability(&self) -> BackendCapability {
        match self {
            Self::Strict(_) => BackendCapability::StrictRealtime,
            Self::Soft(_) => BackendCapability::SoftRealtime,
            Self::Monitor(_) => BackendCapability::MonitorOnly,
        }
    }

    pub fn require_strict(self) -> Result<Piper<Standby, StrictRealtime>> {
        match self {
            Self::Strict(piper) => Ok(piper),
            Self::Soft(_) | Self::Monitor(_) => Err(RobotError::realtime_unsupported(
                "this connection is not StrictRealtime; match on ConnectedPiper or request AutoStrict",
            )),
        }
    }

    pub fn require_motion(self) -> Result<MotionConnectedPiper> {
        match self {
            Self::Strict(piper) => Ok(MotionConnectedPiper::Strict(piper)),
            Self::Soft(piper) => Ok(MotionConnectedPiper::Soft(piper)),
            Self::Monitor(_) => Err(RobotError::realtime_unsupported(
                "monitor-only connections cannot enter motion control states",
            )),
        }
    }
}

pub enum MotionConnectedPiper {
    Strict(Piper<Standby, StrictRealtime>),
    Soft(Piper<Standby, SoftRealtime>),
}

impl<State, Capability> Piper<State, Capability>
where
    Capability: CapabilityMarker,
{
    /// Attach the non-realtime bridge host to this controller-owned Piper instance.
    pub fn attach_bridge_host(
        &self,
        config: crate::bridge_host::BridgeHostConfig,
    ) -> crate::bridge_host::PiperBridgeHost {
        crate::bridge_host::PiperBridgeHost::attach_to_driver(Arc::clone(&self.driver), config)
    }
}

fn wait_for_fresh_collision_protection_update<SendQuery, ReadCached>(
    timeout: Duration,
    poll_interval: Duration,
    baseline: Option<CollisionProtectionSnapshot>,
    mut send_query: SendQuery,
    mut read_cached: ReadCached,
) -> Result<CollisionProtectionSnapshot>
where
    SendQuery: FnMut() -> Result<()>,
    ReadCached: FnMut() -> Result<CollisionProtectionSnapshot>,
{
    let baseline_hw = baseline.as_ref().map_or(0, |state| state.hardware_timestamp_us);
    let baseline_sys = baseline.as_ref().map_or(0, |state| state.host_rx_mono_us);

    send_query()?;

    let start = Instant::now();
    loop {
        let state = read_cached()?;
        if state.is_newer_than(baseline_hw, baseline_sys) {
            return Ok(state);
        }

        if start.elapsed() > timeout {
            return Err(RobotError::Timeout {
                timeout_ms: timeout.as_millis() as u64,
            });
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

pub(crate) fn connected_piper_from_driver(
    driver: Arc<piper_driver::Piper>,
    quirks: DeviceQuirks,
) -> ConnectedPiper {
    match driver.backend_capability() {
        BackendCapability::StrictRealtime => ConnectedPiper::Strict(Piper {
            observer: Observer::<StrictRealtime>::new(driver.clone()),
            driver,
            quirks,
            drop_policy: DropPolicy::Noop,
            driver_mode_drop_policy: DriverModeDropPolicy::Preserve,
            _state: Standby,
        }),
        BackendCapability::SoftRealtime => ConnectedPiper::Soft(Piper {
            observer: Observer::<SoftRealtime>::new(driver.clone()),
            driver,
            quirks,
            drop_policy: DropPolicy::Noop,
            driver_mode_drop_policy: DriverModeDropPolicy::Preserve,
            _state: Standby,
        }),
        BackendCapability::MonitorOnly => ConnectedPiper::Monitor(Piper {
            observer: Observer::<MonitorOnly>::new(driver.clone()),
            driver,
            quirks,
            drop_policy: DropPolicy::Noop,
            driver_mode_drop_policy: DriverModeDropPolicy::Preserve,
            _state: Standby,
        }),
    }
}

fn transition_piper_state<State, NewState, Capability>(
    this: Piper<State, Capability>,
    new_state: NewState,
    drop_policy: DropPolicy,
    driver_mode_drop_policy: DriverModeDropPolicy,
) -> Piper<NewState, Capability>
where
    Capability: CapabilityMarker,
{
    let this = std::mem::ManuallyDrop::new(this);

    let driver = unsafe { std::ptr::read(&this.driver) };
    let observer = unsafe { std::ptr::read(&this.observer) };
    let quirks = this.quirks.clone();

    Piper {
        driver,
        observer,
        quirks,
        drop_policy,
        driver_mode_drop_policy,
        _state: new_state,
    }
}

// ==================== Disconnected 状态 ====================

impl Piper<Disconnected, UnspecifiedCapability> {
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
    pub fn connect<C>(can_adapter: C, config: ConnectionConfig) -> Result<ConnectedPiper>
    where
        C: piper_can::SplittableAdapter + Send + 'static,
        C::RxAdapter: Send + 'static,
        C::TxAdapter: Send + 'static,
    {
        use piper_driver::Piper as RobotPiper;

        let driver = Arc::new(RobotPiper::new_dual_thread_with_startup_timeout(
            can_adapter,
            None,
            config.feedback_timeout,
        )?);
        let quirks = initialize_connected_driver(
            driver.clone(),
            config.feedback_timeout,
            config.firmware_timeout,
        )?;

        Ok(connected_piper_from_driver(driver, quirks))
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
    pub fn reconnect<C>(self, can_adapter: C, config: ConnectionConfig) -> Result<ConnectedPiper>
    where
        C: piper_can::SplittableAdapter + Send + 'static,
        C::RxAdapter: Send + 'static,
        C::TxAdapter: Send + 'static,
    {
        info!("Attempting to reconnect to robot");

        // 1. 创建新的 driver 实例
        use piper_driver::Piper as RobotPiper;
        let driver = Arc::new(RobotPiper::new_dual_thread_with_startup_timeout(
            can_adapter,
            None,
            config.feedback_timeout,
        )?);
        let quirks = initialize_connected_driver(
            driver.clone(),
            config.feedback_timeout,
            config.firmware_timeout,
        )?;

        // 5. 返回到 Standby 状态
        info!("Reconnection successful");
        Ok(connected_piper_from_driver(driver, quirks))
    }
}

// ==================== Standby 状态 ====================

impl<Capability> Piper<Standby, Capability>
where
    Capability: CapabilityMarker,
{
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
    pub fn enable_mit_mode(
        self,
        config: MitModeConfig,
    ) -> Result<Piper<Active<MitMode>, Capability>>
    where
        Capability: StrictCapability,
    {
        use piper_protocol::control::*;

        debug!("Enabling MIT mode (speed_percent={})", config.speed_percent);

        // === PHASE 1: All operations that can panic ===

        // 1. 发送使能指令
        let enable_cmd = MotorEnableCommand::enable_all();
        self.driver.send_reliable(enable_cmd.to_frame())?;
        debug!("Motor enable command sent");

        // 2. 等待使能完成（带 Debounce）
        self.wait_for_enabled(
            config.timeout,
            config.debounce_threshold,
            config.poll_interval,
        )?;
        debug!("All motors enabled (debounced)");

        let baseline = self.capture_mode_confirmation_baseline();

        // 3. 设置 MIT 模式
        let control_cmd = ControlModeCommandFrame::new(
            ControlModeCommand::CanControl,
            MoveMode::MoveM,
            config.speed_percent,
            piper_protocol::control::MitMode::Mit,
            0,
            InstallPosition::Invalid,
        );
        self.driver.send_reliable(control_cmd.to_frame())?;
        self.wait_for_mode_confirmation(
            baseline,
            config.timeout,
            config.poll_interval,
            ModeConfirmationExpectation {
                control_mode: ControlMode::CanControl as u8,
                move_mode: MoveMode::MoveM as u8,
                speed_percent: config.speed_percent,
                mit_mode: ProtocolMitMode::Mit as u8,
                install_position: InstallPosition::Invalid as u8,
                trajectory_stay_time: 0,
            },
        )?;
        info!("Robot enabled - Active<MitMode>");

        Ok(transition_piper_state(
            self,
            Active(MitMode),
            DropPolicy::DisableAll,
            DriverModeDropPolicy::Preserve,
        ))
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
    ) -> Result<Piper<Active<PositionMode>, Capability>>
    where
        Capability: MotionCapability,
    {
        use piper_protocol::control::*;
        debug!(
            "Enabling Position mode (motion_type={:?}, speed_percent={})",
            config.motion_type, config.speed_percent
        );

        if config.motion_type == MotionType::ContinuousPositionVelocity {
            return Err(RobotError::ConfigError(
                "MotionType::ContinuousPositionVelocity is not implemented yet".to_string(),
            ));
        }

        // === PHASE 1: All operations that can panic ===

        // 1. 发送使能指令
        let enable_cmd = MotorEnableCommand::enable_all();
        self.driver.send_reliable(enable_cmd.to_frame())?;
        debug!("Motor enable command sent");

        // 2. 等待使能完成（带 Debounce）
        self.wait_for_enabled(
            config.timeout,
            config.debounce_threshold,
            config.poll_interval,
        )?;
        debug!("All motors enabled (debounced)");
        let baseline = self.capture_mode_confirmation_baseline();

        // 3. 设置位置模式
        // ✅ 修改：使用配置的 motion_type
        let move_mode: MoveMode = config.motion_type.into();

        let control_cmd = ControlModeCommandFrame::new(
            ControlModeCommand::CanControl,
            move_mode,
            config.speed_percent,
            piper_protocol::control::MitMode::PositionVelocity,
            0,
            config.install_position,
        );
        self.driver.send_reliable(control_cmd.to_frame())?;
        self.wait_for_mode_confirmation(
            baseline,
            config.timeout,
            config.poll_interval,
            ModeConfirmationExpectation {
                control_mode: ControlMode::CanControl as u8,
                move_mode: move_mode as u8,
                speed_percent: config.speed_percent,
                mit_mode: ProtocolMitMode::PositionVelocity as u8,
                install_position: config.install_position as u8,
                trajectory_stay_time: 0,
            },
        )?;
        info!("Robot enabled - Active<PositionMode>");
        Ok(transition_piper_state(
            self,
            Active(PositionMode {
                command_timeout: config.command_timeout,
                motion_type: config.motion_type,
            }),
            DropPolicy::DisableAll,
            DriverModeDropPolicy::Preserve,
        ))
    }

    /// 使能全部关节并切换到 MIT 模式
    ///
    /// 这是 `enable_mit_mode` 的便捷方法，使用默认配置。
    pub fn enable_all(self) -> Result<Piper<Active<MitMode>, Capability>>
    where
        Capability: StrictCapability,
    {
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
    pub fn enable_joints(self, joints: &[Joint]) -> Result<Piper<Standby, Capability>>
    where
        Capability: MotionCapability,
    {
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
    pub fn enable_joint(self, joint: Joint) -> Result<Piper<Standby, Capability>>
    where
        Capability: MotionCapability,
    {
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
    pub fn disable_all(&self) -> Result<()>
    where
        Capability: MotionCapability,
    {
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
    pub fn disable_joints(self, joints: &[Joint]) -> Result<()>
    where
        Capability: MotionCapability,
    {
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
    pub fn observer(&self) -> &Observer<Capability> {
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

    fn wait_for_mode_confirmation(
        &self,
        baseline: ModeConfirmationBaseline,
        timeout: Duration,
        poll_interval: Duration,
        expected: ModeConfirmationExpectation,
    ) -> Result<()> {
        let start = Instant::now();

        loop {
            if start.elapsed() > timeout {
                return Err(RobotError::Timeout {
                    timeout_ms: timeout.as_millis() as u64,
                });
            }

            let current = self.driver.get_robot_control();
            let mode_echo = self.driver.get_control_mode_echo();
            let is_robot_state_fresh = current.hardware_timestamp_us
                > baseline.robot_control.hardware_timestamp_us
                || current.host_rx_mono_us > baseline.robot_control.host_rx_mono_us;
            let is_robot_state_match = current.is_enabled
                && current.robot_status == RobotStatus::Normal as u8
                && current.control_mode == expected.control_mode
                && current.move_mode == expected.move_mode;
            let is_mode_echo_fresh = mode_echo.is_valid
                && (mode_echo.hardware_timestamp_us > baseline.mode_echo.hardware_timestamp_us
                    || mode_echo.host_rx_mono_us > baseline.mode_echo.host_rx_mono_us);
            let is_mode_echo_match = mode_echo.control_mode == expected.control_mode
                && mode_echo.move_mode == expected.move_mode
                && mode_echo.speed_percent == expected.speed_percent
                && mode_echo.mit_mode == expected.mit_mode
                && mode_echo.install_position == expected.install_position
                && mode_echo.trajectory_stay_time == expected.trajectory_stay_time;
            if is_robot_state_fresh
                && is_robot_state_match
                && is_mode_echo_fresh
                && is_mode_echo_match
            {
                return Ok(());
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

    /// 启动录制（Standby 状态）
    ///
    /// # 参数
    ///
    /// - `config`: 录制配置
    ///
    /// # 返回
    ///
    /// 返回 `(Piper<Standby>, RecordingHandle)`
    ///
    /// # 示例
    ///
    /// ```rust,ignore
    /// # use piper_client::{PiperBuilder, recording::{RecordingConfig, StopCondition}};
    /// # fn example() -> Result<()> {
    /// let standby = PiperBuilder::new()
    ///     .socketcan("can0")
    ///     .build()?;
    ///
    /// // 启动录制
    /// let (standby, handle) = standby.start_recording(RecordingConfig {
    ///     output_path: "demo.bin".into(),
    ///     stop_condition: StopCondition::Duration(10),
    ///     metadata: RecordingMetadata {
    ///         notes: "Test recording".to_string(),
    ///         operator: "Alice".to_string(),
    ///     },
    /// })?;
    ///
    /// // 执行操作（会被录制）
    /// // ...
    ///
    /// // 停止录制并保存
    /// let _standby = standby.stop_recording(handle)?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn start_recording(
        self,
        config: crate::recording::RecordingConfig,
    ) -> Result<(Self, crate::recording::RecordingHandle)> {
        use crate::recording::{RecordingHandle, StopCondition};

        // ✅ 提取 stop_on_id 从 StopCondition
        let stop_on_id = match &config.stop_condition {
            StopCondition::OnCanId(id) => Some(*id),
            _ => None,
        };

        // ✅ 根据是否需要停止条件选择构造函数
        let (hook, rx) = if let Some(id) = stop_on_id {
            tracing::info!("Recording with stop condition: CAN ID 0x{:X}", id);
            piper_driver::recording::AsyncRecordingHook::with_stop_condition(Some(id))
        } else {
            piper_driver::recording::AsyncRecordingHook::new()
        };

        let dropped = hook.dropped_frames().clone();
        let counter = hook.frame_counter().clone();
        let stop_requested = hook.stop_requested().clone(); // ✅ 获取停止标志引用

        // 注册钩子
        let callback = std::sync::Arc::new(hook) as std::sync::Arc<dyn piper_driver::FrameCallback>;
        self.driver
            .hooks()
            .write()
            .map_err(|_e| {
                crate::RobotError::Infrastructure(piper_driver::DriverError::PoisonedLock)
            })?
            .add_callback(callback);

        // ✅ 传递 stop_requested（OnCanId 时使用 Driver 层的标志，其他情况使用 None）
        let handle = RecordingHandle::new(
            rx,
            dropped,
            counter,
            config.output_path.clone(),
            std::time::Instant::now(),
            if stop_on_id.is_some() {
                Some(stop_requested)
            } else {
                None
            },
        );

        tracing::info!("Recording started: {:?}", config.output_path);

        Ok((self, handle))
    }

    /// 停止录制并保存文件
    ///
    /// # 参数
    ///
    /// - `handle`: 录制句柄
    ///
    /// # 返回
    ///
    /// 返回 `(Piper<Standby>, 录制统计)`
    ///
    /// # 示例
    ///
    /// ```rust,ignore
    /// # use piper_client::PiperBuilder;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let standby = PiperBuilder::new()
    ///     .socketcan("can0")
    ///     .build()?;
    ///
    /// let (standby, handle) = standby.start_recording(config)?;
    ///
    /// // ... 等待一段时间 ...
    ///
    /// // 停止录制并保存
    /// let (standby, stats) = standby.stop_recording(handle)?;
    ///
    /// println!("录制完成: {} 帧", stats.frame_count);
    /// # Ok(())
    /// # }
    /// ```
    pub fn stop_recording(
        self,
        handle: crate::recording::RecordingHandle,
    ) -> Result<(Self, crate::recording::RecordingStats)> {
        use piper_tools::{PiperRecording, TimestampSource, TimestampedFrame};

        // 创建录制对象
        let mut recording = PiperRecording::new(piper_tools::RecordingMetadata::new(
            self.driver.interface(),
            self.driver.bus_speed(),
        ));

        // 收集所有帧（转换为 piper_tools 格式）
        let mut frame_count = 0;
        while let Ok(driver_frame) = handle.receiver().try_recv() {
            // 转换 piper_driver::TimestampedFrame -> piper_tools::TimestampedFrame
            let tools_frame = TimestampedFrame::new(
                driver_frame.timestamp_us,
                driver_frame.id,
                driver_frame.data,
                TimestampSource::Hardware, // 使用硬件时间戳
            );
            recording.add_frame(tools_frame);
            frame_count += 1;
        }

        // 保存文件
        recording.save(handle.output_path()).map_err(|e| {
            crate::RobotError::Infrastructure(piper_driver::DriverError::IoThread(e.to_string()))
        })?;

        let stats = crate::recording::RecordingStats {
            frame_count,
            duration: handle.elapsed(),
            dropped_frames: handle.dropped_count(),
            output_path: handle.output_path().clone(),
        };

        tracing::info!(
            "Recording saved: {} frames, {:.2}s, {} dropped",
            stats.frame_count,
            stats.duration.as_secs_f64(),
            stats.dropped_frames
        );

        Ok((self, stats))
    }

    /// 进入回放模式
    ///
    /// # 功能
    ///
    /// 将 Driver 切换到 Replay 模式，暂停 TX 线程的周期性发送，
    /// 准备回放预先录制的 CAN 帧。
    ///
    /// # 安全保证
    ///
    /// - Driver 进入 Replay 模式后，TX 线程暂停周期性发送
    /// - 避免双控制流冲突
    /// - 只能通过 `replay_recording()` 发送预先录制的帧
    ///
    /// # ⚠️ 安全警告
    ///
    /// - 进入 Replay 模式前，应确保机器人处于 Standby 状态
    /// - 回放时应遵守安全速度限制（建议 ≤ 2.0x）
    /// - 回放过程中应有人工急停准备
    ///
    /// # 返回
    ///
    /// 返回 `Piper<ReplayMode>` 实例
    ///
    /// # 示例
    ///
    /// ```rust,ignore
    /// # use piper_client::PiperBuilder;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let standby = PiperBuilder::new()
    ///     .socketcan("can0")
    ///     .build()?;
    ///
    /// // 进入回放模式
    /// let replay = standby.enter_replay_mode()?;
    ///
    /// // 回放录制（1.0x 速度，原始速度）
    /// let standby = replay.replay_recording("recording.bin", 1.0)?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn enter_replay_mode(self) -> Result<Piper<ReplayMode, Capability>>
    where
        Capability: MotionCapability,
    {
        use piper_driver::mode::DriverMode;
        use std::time::Duration;
        const REPLAY_MODE_SWITCH_TIMEOUT: Duration = Duration::from_millis(100);

        // 切换 Driver 到 Replay 模式
        self.driver.try_set_mode(DriverMode::Replay, REPLAY_MODE_SWITCH_TIMEOUT)?;

        tracing::info!("Entered ReplayMode - TX thread periodic sending paused");

        Ok(transition_piper_state(
            self,
            ReplayMode,
            DropPolicy::Noop,
            DriverModeDropPolicy::RestoreNormal,
        ))
    }

    /// 设置碰撞保护级别
    ///
    /// 设置6个关节的碰撞防护等级（0~8，等级0代表不检测碰撞）。
    ///
    /// # 参数
    ///
    /// - `levels`: 6个关节的碰撞防护等级数组，每个值范围 0~8
    ///
    /// # 示例
    ///
    /// ```rust,ignore
    /// # use piper_client::PiperBuilder;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let standby = PiperBuilder::new().socketcan("can0").build()?;
    ///
    /// // 所有关节设置为等级 5（中等保护）
    /// standby.set_collision_protection([5, 5, 5, 5, 5, 5])?;
    ///
    /// // 为不同关节设置不同等级
    /// // J1-J3 基座关节使用较高保护，J4-J6 末端关节使用较低保护
    /// standby.set_collision_protection([6, 6, 6, 4, 4, 4])?;
    ///
    /// // 禁用碰撞保护（谨慎使用）
    /// standby.set_collision_protection([0, 0, 0, 0, 0, 0])?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn set_collision_protection(&self, levels: [u8; 6]) -> Result<()> {
        let raw = RawCommander::new(&self.driver);
        raw.set_collision_protection(levels)
    }

    /// 主动查询当前碰撞保护等级并等待设备反馈。
    pub fn query_collision_protection(&self, timeout: Duration) -> Result<[u8; 6]> {
        self.query_collision_protection_with_poll(timeout, COLLISION_QUERY_POLL_INTERVAL)
            .map(|snapshot| snapshot.levels)
    }

    /// 读取 driver 当前缓存的碰撞保护快照，不触发设备 query。
    pub fn collision_protection_cached(&self) -> Result<CollisionProtectionSnapshot> {
        self.collision_protection_cached_inner()
    }

    /// 设置关节零位
    ///
    /// 设置指定关节的当前位置为零点。
    ///
    /// **⚠️ 安全警告**：
    /// - 设置零位前，确保关节已移动到预期的零点位置
    /// - 建议在机械臂安装或重新校准时使用
    /// - 设置后应验证关节位置是否正确
    ///
    /// # 参数
    ///
    /// - `joints`: 要设置零位的关节数组（0-based，0-5 对应 J1-J6）
    ///
    /// # 示例
    ///
    /// ```rust,ignore
    /// # use piper_client::PiperBuilder;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let standby = PiperBuilder::new().socketcan("can0").build()?;
    ///
    /// // 设置 J1 的当前位置为零点
    /// // 注意：确保 J1 已移动到预期的零点位置
    /// standby.set_joint_zero_positions(&[0])?;
    ///
    /// // 设置多个关节的零位
    /// standby.set_joint_zero_positions(&[0, 1, 2])?;
    ///
    /// // 设置所有关节的零位
    /// standby.set_joint_zero_positions(&[0, 1, 2, 3, 4, 5])?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn set_joint_zero_positions(&self, joints: &[usize]) -> Result<()> {
        let raw = RawCommander::new(&self.driver);
        raw.set_joint_zero_positions(joints)
    }
}

impl Piper<Standby, SoftRealtime> {
    pub fn enable_mit_passthrough(
        self,
        config: MitModeConfig,
    ) -> Result<Piper<Active<MitPassthroughMode>, SoftRealtime>> {
        use piper_protocol::control::*;

        debug!(
            "Enabling MIT passthrough mode (speed_percent={})",
            config.speed_percent
        );

        let enable_cmd = MotorEnableCommand::enable_all();
        self.driver.send_reliable(enable_cmd.to_frame())?;
        self.wait_for_enabled(
            config.timeout,
            config.debounce_threshold,
            config.poll_interval,
        )?;

        let baseline = self.capture_mode_confirmation_baseline();
        let control_cmd = ControlModeCommandFrame::new(
            ControlModeCommand::CanControl,
            MoveMode::MoveM,
            config.speed_percent,
            piper_protocol::control::MitMode::Mit,
            0,
            InstallPosition::Invalid,
        );
        self.driver.send_reliable(control_cmd.to_frame())?;
        self.wait_for_mode_confirmation(
            baseline,
            config.timeout,
            config.poll_interval,
            ModeConfirmationExpectation {
                control_mode: ControlMode::CanControl as u8,
                move_mode: MoveMode::MoveM as u8,
                speed_percent: config.speed_percent,
                mit_mode: ProtocolMitMode::Mit as u8,
                install_position: InstallPosition::Invalid as u8,
                trajectory_stay_time: 0,
            },
        )?;

        Ok(transition_piper_state(
            self,
            Active(MitPassthroughMode),
            DropPolicy::DisableAll,
            DriverModeDropPolicy::Preserve,
        ))
    }
}

// ==================== 所有状态共享的辅助方法 ====================

impl<State, Capability> Piper<State, Capability>
where
    Capability: CapabilityMarker,
{
    fn capture_mode_confirmation_baseline(&self) -> ModeConfirmationBaseline {
        ModeConfirmationBaseline {
            robot_control: self.driver.get_robot_control(),
            mode_echo: self.driver.get_control_mode_echo(),
        }
    }

    fn into_state<NextState>(
        self,
        next_state: NextState,
        drop_policy: DropPolicy,
        driver_mode_drop_policy: DriverModeDropPolicy,
    ) -> Piper<NextState, Capability> {
        let this = std::mem::ManuallyDrop::new(self);
        let driver = unsafe { std::ptr::read(&this.driver) };
        let observer = unsafe { std::ptr::read(&this.observer) };
        let quirks = this.quirks.clone();

        Piper {
            driver,
            observer,
            quirks,
            drop_policy,
            driver_mode_drop_policy,
            _state: next_state,
        }
    }

    fn query_collision_protection_with_poll(
        &self,
        timeout: Duration,
        poll_interval: Duration,
    ) -> Result<CollisionProtectionSnapshot> {
        let baseline = self.collision_protection_cached_inner().ok();
        let raw = RawCommander::new(&self.driver);
        wait_for_fresh_collision_protection_update(
            timeout,
            poll_interval,
            baseline,
            || raw.query_collision_protection(),
            || self.collision_protection_cached_inner(),
        )
    }

    fn collision_protection_cached_inner(&self) -> Result<CollisionProtectionSnapshot> {
        self.driver
            .get_collision_protection()
            .map(CollisionProtectionSnapshot::from)
            .map_err(Into::into)
    }

    /// 获取 driver 运行时健康快照。
    pub fn runtime_health(&self) -> RuntimeHealthSnapshot {
        self.observer.runtime_health()
    }

    fn build_validated_mit_command_batch(
        &self,
        positions: &JointArray<Rad>,
        velocities: &JointArray<f64>,
        kp: &JointArray<f64>,
        kd: &JointArray<f64>,
        torques: &JointArray<NewtonMeter>,
    ) -> Result<[MitControlCommand; 6]> {
        let mut commands = [MitControlCommand::try_new(1, 0.0, 0.0, 0.0, 0.0, 0.0)?; 6];

        for (index, joint) in Joint::ALL.into_iter().enumerate() {
            let joint_index = joint.index() as u8 + 1;
            let (position, flipped_torque) =
                self.quirks.apply_flip(joint, positions[joint].0, torques[joint].0);
            let torque = self.quirks.scale_torque(joint, flipped_torque);

            commands[index] = MitControlCommand::try_new(
                joint_index,
                position as f32,
                velocities[joint] as f32,
                kp[joint] as f32,
                kd[joint] as f32,
                torque as f32,
            )?;
        }

        Ok(commands)
    }

    /// 获取固件特性（DeviceQuirks）
    ///
    /// # 返回
    ///
    /// 返回当前机械臂的固件特性，包括：
    /// - `firmware_version`: 固件版本号
    /// - `joint_flip_map`: 关节 flip 标志
    /// - `torque_scaling`: 力矩缩放因子
    ///
    /// # 示例
    ///
    /// ```rust,ignore
    /// # use piper_client::PiperBuilder;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let robot = PiperBuilder::new().socketcan("can0").build()?;
    ///
    /// // 获取固件特性
    /// let quirks = robot.quirks();
    /// println!("Firmware version: {}", quirks.firmware_version);
    ///
    /// // 仅用于诊断；常规 MIT 控制已自动应用这些修正
    /// for joint in [Joint::J1, Joint::J2, Joint::J3, Joint::J4, Joint::J5, Joint::J6] {
    ///     let needs_flip = quirks.needs_flip(joint);
    ///     let scaling = quirks.torque_scaling_factor(joint);
    ///     println!("Joint {:?}: flip={}, scaling={}", joint, needs_flip, scaling);
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn quirks(&self) -> DeviceQuirks {
        self.quirks.clone()
    }

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
    pub fn emergency_stop(self) -> Result<Piper<ErrorState, Capability>> {
        self.driver.latch_fault();
        let raw_commander = RawCommander::new(&self.driver);
        let receipt =
            raw_commander.emergency_stop_enqueue(Instant::now() + Duration::from_millis(20))?;
        receipt.wait()?;

        Ok(transition_piper_state(
            self,
            ErrorState,
            DropPolicy::Noop,
            DriverModeDropPolicy::Preserve,
        ))
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

impl<M, Capability> Piper<Active<M>, Capability>
where
    Capability: MotionCapability,
{
    /// 立即失能全部关节并返回 Standby。
    ///
    /// 这是急停/人工接管路径，发送 `disable_all()` 后立即转换回 Standby，
    /// 不等待 debounce 或关闭序列完成。
    pub fn disable_all(self) -> Result<Piper<Standby, Capability>> {
        use piper_protocol::control::MotorEnableCommand;

        self.driver.send_reliable(MotorEnableCommand::disable_all().to_frame())?;
        Ok(self.into_state(Standby, DropPolicy::Noop, DriverModeDropPolicy::Preserve))
    }

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
    pub fn shutdown(self) -> Result<Piper<Standby, Capability>> {
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

        // SAFETY: `this.quirks` is a valid DeviceQuirks (Copy type).
        let quirks = this.quirks.clone();

        Ok(Piper {
            driver,
            observer,
            quirks,
            drop_policy: DropPolicy::Noop,
            driver_mode_drop_policy: DriverModeDropPolicy::Preserve,
            _state: Standby,
        })
    }

    /// 获取诊断接口（逃生舱）
    ///
    /// # 返回值
    ///
    /// 返回的 `PiperDiagnostics` 持有 `Arc<piper_driver::Piper>`：
    /// - ✅ 独立于当前 `Piper` 实例的生命周期
    /// - ✅ 可以安全地移动到其他线程
    /// - ✅ 可以在后台线程中长期持有
    ///
    /// # 使用场景
    ///
    /// - 自定义诊断工具
    /// - 高级抓包和调试
    /// - 性能分析和优化
    /// - 后台监控线程
    ///
    /// # 示例
    ///
    /// ```rust,no_run
    /// # use piper_client::{Piper, PiperBuilder};
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let robot = PiperBuilder::new()
    ///     .socketcan("can0")
    ///     .build()?;
    ///
    /// let active = robot.enable_position_mode(Default::default())?;
    ///
    /// // 获取诊断接口
    /// let diag = active.diagnostics();
    ///
    /// // diag 可以安全地移动到其他线程
    /// std::thread::spawn(move || {
    ///     // 在这里使用 diag...
    /// });
    ///
    /// // active 仍然可以正常使用
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # 安全注意事项
    ///
    /// 诊断接口提供了底层访问能力，使用时需注意：
    /// 1. **不要在 Active 状态下发送控制指令帧**（会导致双控制流冲突）
    /// 2. **确保回调执行时间 <1μs**（否则会影响实时性能）
    /// 3. **注意生命周期**：即使持有 `Arc`，也要确保关联的 `Piper` 实例未被销毁
    ///
    /// # 参考
    ///
    /// - [`PiperDiagnostics`](crate::PiperDiagnostics) - 诊断接口文档
    /// - [架构分析报告](../../../docs/architecture/piper-driver-client-mixing-analysis.md) - 方案 B 设计
    pub fn diagnostics(&self) -> crate::PiperDiagnostics {
        crate::PiperDiagnostics::new(self)
    }

    /// 启动录制（Active 状态）
    ///
    /// # 参数
    ///
    /// - `config`: 录制配置
    ///
    /// # 返回
    ///
    /// 返回 `(Piper<Active<M>>, RecordingHandle)`
    ///
    /// # 注意
    ///
    /// Active 状态下的录制会包含控制指令帧（0x1A1-0x1FF）。
    ///
    /// # 示例
    ///
    /// ```rust,ignore
    /// # use piper_client::{PiperBuilder, recording::{RecordingConfig, StopCondition}};
    /// # fn example() -> Result<()> {
    /// let standby = PiperBuilder::new()
    ///     .socketcan("can0")
    ///     .build()?;
    /// let active = standby.enable_mit_mode(Default::default())?;
    ///
    /// // 启动录制（Active 状态）
    /// let (active, handle) = active.start_recording(RecordingConfig {
    ///     output_path: "demo.bin".into(),
    ///     stop_condition: StopCondition::Duration(10),
    ///     metadata: RecordingMetadata {
    ///         notes: "Test recording".to_string(),
    ///         operator: "Alice".to_string(),
    ///     },
    /// })?;
    ///
    /// // 执行操作（会被录制，包含控制指令帧）
    /// active.command_torques(...)?;
    ///
    /// // 停止录制并保存
    /// let (active, _stats) = active.stop_recording(handle)?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn start_recording(
        self,
        config: crate::recording::RecordingConfig,
    ) -> Result<(Self, crate::recording::RecordingHandle)> {
        use crate::recording::{RecordingHandle, StopCondition};

        // ✅ 提取 stop_on_id 从 StopCondition
        let stop_on_id = match &config.stop_condition {
            StopCondition::OnCanId(id) => Some(*id),
            _ => None,
        };

        // ✅ 根据是否需要停止条件选择构造函数
        let (hook, rx) = if let Some(id) = stop_on_id {
            tracing::info!("Recording with stop condition: CAN ID 0x{:X}", id);
            piper_driver::recording::AsyncRecordingHook::with_stop_condition(Some(id))
        } else {
            piper_driver::recording::AsyncRecordingHook::new()
        };

        let dropped = hook.dropped_frames().clone();
        let counter = hook.frame_counter().clone();
        let stop_requested = hook.stop_requested().clone(); // ✅ 获取停止标志引用

        // 注册钩子
        let callback = std::sync::Arc::new(hook) as std::sync::Arc<dyn piper_driver::FrameCallback>;
        self.driver
            .hooks()
            .write()
            .map_err(|_e| {
                crate::RobotError::Infrastructure(piper_driver::DriverError::PoisonedLock)
            })?
            .add_callback(callback);

        // ✅ 传递 stop_requested（OnCanId 时使用 Driver 层的标志，其他情况使用 None）
        let handle = RecordingHandle::new(
            rx,
            dropped,
            counter,
            config.output_path.clone(),
            std::time::Instant::now(),
            if stop_on_id.is_some() {
                Some(stop_requested)
            } else {
                None
            },
        );

        tracing::info!("Recording started (Active): {:?}", config.output_path);

        Ok((self, handle))
    }

    /// 停止录制并保存文件
    ///
    /// # 参数
    ///
    /// - `handle`: 录制句柄
    ///
    /// # 返回
    ///
    /// 返回 `(Piper<Active<M>>, 录制统计)`
    ///
    /// # 示例
    ///
    /// ```rust,ignore
    /// # use piper_client::PiperBuilder;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let standby = PiperBuilder::new()
    ///     .socketcan("can0")
    ///     .build()?;
    /// let active = standby.enable_mit_mode(Default::default())?;
    ///
    /// let (active, handle) = active.start_recording(config)?;
    ///
    /// // ... 执行操作 ...
    ///
    /// // 停止录制并保存
    /// let (active, stats) = active.stop_recording(handle)?;
    ///
    /// println!("录制完成: {} 帧", stats.frame_count);
    /// # Ok(())
    /// # }
    /// ```
    pub fn stop_recording(
        self,
        handle: crate::recording::RecordingHandle,
    ) -> Result<(Self, crate::recording::RecordingStats)> {
        use piper_tools::{PiperRecording, TimestampSource, TimestampedFrame};

        // 创建录制对象
        let mut recording = PiperRecording::new(piper_tools::RecordingMetadata::new(
            self.driver.interface(),
            self.driver.bus_speed(),
        ));

        // 收集所有帧（转换为 piper_tools 格式）
        let mut frame_count = 0;
        while let Ok(driver_frame) = handle.receiver().try_recv() {
            // 转换 piper_driver::TimestampedFrame -> piper_tools::TimestampedFrame
            let tools_frame = TimestampedFrame::new(
                driver_frame.timestamp_us,
                driver_frame.id,
                driver_frame.data,
                TimestampSource::Hardware, // 使用硬件时间戳
            );
            recording.add_frame(tools_frame);
            frame_count += 1;
        }

        // 保存文件
        recording.save(handle.output_path()).map_err(|e| {
            crate::RobotError::Infrastructure(piper_driver::DriverError::IoThread(e.to_string()))
        })?;

        let stats = crate::recording::RecordingStats {
            frame_count,
            duration: handle.elapsed(),
            dropped_frames: handle.dropped_count(),
            output_path: handle.output_path().clone(),
        };

        tracing::info!(
            "Recording saved: {} frames, {:.2}s, {} dropped",
            stats.frame_count,
            stats.duration.as_secs_f64(),
            stats.dropped_frames
        );

        Ok((self, stats))
    }

    /// 设置碰撞保护级别
    ///
    /// 设置6个关节的碰撞防护等级（0~8，等级0代表不检测碰撞）。
    ///
    /// **注意**：此方法在 Active 状态下也可调用，允许运行时调整碰撞保护级别。
    ///
    /// # 参数
    ///
    /// - `levels`: 6个关节的碰撞防护等级数组，每个值范围 0~8
    ///
    /// # 示例
    ///
    /// ```rust,ignore
    /// # use piper_client::PiperBuilder;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let standby = PiperBuilder::new().socketcan("can0").build()?;
    /// let active = standby.enable_position_mode(Default::default())?;
    ///
    /// // 运行时提高碰撞保护级别（例如进入精密操作区域）
    /// active.set_collision_protection([7, 7, 7, 7, 7, 7])?;
    ///
    /// // 运行时降低碰撞保护级别（例如需要更大的力矩）
    /// active.set_collision_protection([3, 3, 3, 3, 3, 3])?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn set_collision_protection(&self, levels: [u8; 6]) -> Result<()> {
        let raw = RawCommander::new(&self.driver);
        raw.set_collision_protection(levels)
    }

    /// 主动查询当前碰撞保护等级并等待设备反馈。
    pub fn query_collision_protection(&self, timeout: Duration) -> Result<[u8; 6]> {
        self.query_collision_protection_with_poll(timeout, COLLISION_QUERY_POLL_INTERVAL)
            .map(|snapshot| snapshot.levels)
    }

    /// 读取 driver 当前缓存的碰撞保护快照，不触发设备 query。
    pub fn collision_protection_cached(&self) -> Result<CollisionProtectionSnapshot> {
        self.collision_protection_cached_inner()
    }
}

// ==================== Active<MitMode> 状态 ====================

impl<Capability> Piper<Active<MitMode>, Capability>
where
    Capability: StrictCapability,
{
    /// 发送 MIT 模式控制指令
    ///
    /// 对所有关节发送位置、速度、力矩的混合控制指令。
    ///
    /// # 参数
    ///
    /// - `positions`: 各关节目标位置（Rad）
    /// - `velocities`: 各关节目标速度（rad/s）
    /// - `kp`: 位置增益（每个关节独立）
    /// - `kd`: 速度增益（每个关节独立）
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
    /// let kp = JointArray::from([10.0; 6]);  // 每个关节独立的 kp
    /// let kd = JointArray::from([2.0; 6]);   // 每个关节独立的 kd
    /// let torques = JointArray::from([
    ///     NewtonMeter(5.0), NewtonMeter(0.0), NewtonMeter(0.0),
    ///     NewtonMeter(0.0), NewtonMeter(0.0), NewtonMeter(0.0)
    /// ]);
    /// robot.command_torques(&positions, &velocities, &kp, &kd, &torques)?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn command_torques(
        &self,
        positions: &JointArray<Rad>,
        velocities: &JointArray<f64>,
        kp: &JointArray<f64>,
        kd: &JointArray<f64>,
        torques: &JointArray<NewtonMeter>,
    ) -> Result<()> {
        let raw = RawCommander::new(&self.driver);
        let commands =
            self.build_validated_mit_command_batch(positions, velocities, kp, kd, torques)?;
        raw.send_validated_mit_command_batch(commands)
    }

    /// 发送 MIT 模式控制指令，并等待 TX 线程确认实际发送结果。
    pub fn command_torques_confirmed(
        &self,
        positions: &JointArray<Rad>,
        velocities: &JointArray<f64>,
        kp: &JointArray<f64>,
        kd: &JointArray<f64>,
        torques: &JointArray<NewtonMeter>,
        timeout: Duration,
    ) -> Result<()> {
        let raw = RawCommander::new(&self.driver);
        let commands =
            self.build_validated_mit_command_batch(positions, velocities, kp, kd, torques)?;
        raw.send_validated_mit_command_batch_confirmed(commands, timeout)
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
    pub fn observer(&self) -> &Observer<Capability> {
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
    pub fn disable(self, config: DisableConfig) -> Result<Piper<Standby, Capability>> {
        use piper_protocol::control::*;

        debug!("Disabling robot");

        // === PHASE 1: All operations that can panic ===

        // 1. 失能机械臂
        let disable_cmd = MotorEnableCommand::disable_all();
        self.driver.send_reliable(disable_cmd.to_frame())?;
        debug!("Motor disable command sent");

        // 2. 等待失能完成（带 Debounce）
        self.wait_for_disabled(
            config.timeout,
            config.debounce_threshold,
            config.poll_interval,
        )?;
        info!("Robot disabled - Standby mode");

        Ok(transition_piper_state(
            self,
            Standby,
            DropPolicy::Noop,
            DriverModeDropPolicy::Preserve,
        ))
    }
}

impl Piper<Active<MitPassthroughMode>, SoftRealtime> {
    /// 发送原始 MIT 批命令，并等待 SoftRealtime TX 线程确认发送结果。
    ///
    /// 如果批命令只部分落到硬件，driver 会立即锁存传输故障并返回
    /// `DriverError::RealtimeDeliveryFailed`；上层不应将这类错误视为普通超时重试。
    pub fn command_mit_raw_confirmed(
        &self,
        commands: [MitControlCommand; 6],
        timeout: Duration,
    ) -> Result<()> {
        let frames = commands.map(MitControlCommand::to_frame);
        self.driver.send_soft_realtime_package_confirmed(frames, timeout)?;
        Ok(())
    }

    pub fn observer(&self) -> &Observer<SoftRealtime> {
        &self.observer
    }

    pub fn disable(self, config: DisableConfig) -> Result<Piper<Standby, SoftRealtime>> {
        use piper_protocol::control::*;

        debug!("Disabling robot");

        let disable_cmd = MotorEnableCommand::disable_all();
        self.driver.send_reliable(disable_cmd.to_frame())?;
        self.wait_for_disabled(
            config.timeout,
            config.debounce_threshold,
            config.poll_interval,
        )?;
        info!("Robot disabled - Standby mode");

        Ok(transition_piper_state(
            self,
            Standby,
            DropPolicy::Noop,
            DriverModeDropPolicy::Preserve,
        ))
    }
}

// ==================== Active<PositionMode> 状态 ====================

impl<Capability> Piper<Active<PositionMode>, Capability>
where
    Capability: MotionCapability,
{
    fn ensure_position_motion_type(
        &self,
        expected: MotionType,
        operation: &str,
    ) -> Result<&PositionMode> {
        let position_mode = &self._state.0;
        if position_mode.motion_type != expected {
            return Err(RobotError::ConfigError(format!(
                "{operation} requires MotionType::{expected:?}, but this PositionMode is MotionType::{actual:?}",
                actual = position_mode.motion_type
            )));
        }

        Ok(position_mode)
    }

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
        let position_mode =
            self.ensure_position_motion_type(MotionType::Joint, "send_position_command")?;
        let raw = RawCommander::new(&self.driver);
        raw.send_position_command_batch(positions, position_mode.command_timeout)
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
        let position_mode =
            self.ensure_position_motion_type(MotionType::Cartesian, "command_cartesian_pose")?;
        let raw = RawCommander::new(&self.driver);
        raw.send_end_pose_command(position, orientation, position_mode.command_timeout)
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
        let position_mode = self.ensure_position_motion_type(MotionType::Linear, "move_linear")?;
        let raw = RawCommander::new(&self.driver);
        raw.send_end_pose_command(position, orientation, position_mode.command_timeout)
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
        let position_mode =
            self.ensure_position_motion_type(MotionType::Circular, "move_circular")?;
        let raw = RawCommander::new(&self.driver);
        raw.send_circular_motion(
            via_position,
            via_orientation,
            target_position,
            target_orientation,
            position_mode.command_timeout,
        )
    }

    /// 基于调用方已持有的 6 关节快照更新单个关节目标。
    ///
    /// 这是位置模式下最安全的单关节便捷入口：不会读取 driver，也不会隐式重放陈旧反馈。
    pub fn command_position_from_snapshot(
        &self,
        joint: Joint,
        position: Rad,
        current_positions: &JointArray<Rad>,
    ) -> Result<()> {
        let mut positions = *current_positions;
        positions[joint] = position;
        self.send_position_command(&positions)
    }

    /// 按显式 freshness 策略读取当前位置并更新单个关节目标。
    ///
    /// 该方法只会使用完整且新鲜的关节位置监控快照做 read-modify-write。
    /// 如需更新多个关节，或者调用方已经持有一份新鲜的 6 关节目标，请优先使用
    /// `send_position_command()` 或 `command_position_from_snapshot()`。
    pub fn command_position_with_policy(
        &self,
        joint: Joint,
        position: Rad,
        policy: MonitorReadPolicy,
    ) -> Result<()> {
        let positions = self.observer.joint_positions_with_policy(policy)?;
        self.command_position_from_snapshot(joint, position, &positions)
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
    pub fn observer(&self) -> &Observer<Capability> {
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
    pub fn disable(self, config: DisableConfig) -> Result<Piper<Standby, Capability>> {
        use piper_protocol::control::*;

        debug!("Disabling robot");

        // === PHASE 1: All operations that can panic ===

        // 1. 失能机械臂
        let disable_cmd = MotorEnableCommand::disable_all();
        self.driver.send_reliable(disable_cmd.to_frame())?;
        debug!("Motor disable command sent");

        // 2. 等待失能完成（带 Debounce）
        self.wait_for_disabled(
            config.timeout,
            config.debounce_threshold,
            config.poll_interval,
        )?;
        info!("Robot disabled - Standby mode");

        // === PHASE 2: No-panic zone - must not panic after this point ===

        Ok(transition_piper_state(
            self,
            Standby,
            DropPolicy::Noop,
            DriverModeDropPolicy::Preserve,
        ))
    }
}

// ==================== ReplayMode 状态 ====================

impl<Capability> Piper<ReplayMode, Capability>
where
    Capability: MotionCapability,
{
    fn recording_frame_to_piper_frame(
        frame: &piper_tools::TimestampedFrame,
    ) -> Result<piper_can::PiperFrame> {
        if frame.data.len() > 8 {
            return Err(RobotError::ConfigError(format!(
                "recording frame 0x{:X} has {} data bytes; CAN 2.0 frames support at most 8",
                frame.can_id,
                frame.data.len()
            )));
        }

        let mut data = [0u8; 8];
        data[..frame.data.len()].copy_from_slice(&frame.data);

        Ok(piper_can::PiperFrame {
            id: frame.can_id,
            data,
            len: frame.data.len() as u8,
            is_extended: frame.can_id > 0x7FF,
            timestamp_us: frame.timestamp_us,
        })
    }

    fn replay_cancel_requested(cancel_signal: &std::sync::atomic::AtomicBool) -> bool {
        !cancel_signal.load(std::sync::atomic::Ordering::Acquire)
    }

    fn wait_replay_delay_or_cancel(
        delay: std::time::Duration,
        cancel_signal: &std::sync::atomic::AtomicBool,
    ) -> bool {
        use std::thread;
        use std::time::Duration;

        const CANCEL_POLL_INTERVAL: Duration = Duration::from_millis(1);

        if Self::replay_cancel_requested(cancel_signal) {
            return false;
        }

        let wait_started_at = std::time::Instant::now();
        while wait_started_at.elapsed() < delay {
            let remaining = delay.saturating_sub(wait_started_at.elapsed());
            thread::sleep(remaining.min(CANCEL_POLL_INTERVAL));
            if Self::replay_cancel_requested(cancel_signal) {
                return false;
            }
        }

        true
    }

    fn exit_replay_mode_to_standby(self) -> Piper<Standby, Capability> {
        use piper_driver::mode::DriverMode;

        self.driver.set_mode(DriverMode::Normal);
        tracing::info!("Exited ReplayMode - TX thread normal operation resumed");

        transition_piper_state(
            self,
            Standby,
            DropPolicy::Noop,
            DriverModeDropPolicy::Preserve,
        )
    }

    /// 回放预先录制的 CAN 帧
    ///
    /// # 参数
    ///
    /// - `recording_path`: 录制文件路径
    /// - `speed_factor`: 速度倍数（1.0 = 原始速度，建议范围 0.1 ~ 2.0）
    ///
    /// # 功能
    ///
    /// 从录制文件中读取 CAN 帧序列，并按照原始时间间隔发送。
    /// 支持变速回放，但建议速度 ≤ 2.0x 以确保安全。
    ///
    /// # 安全保证
    ///
    /// - Driver 处于 Replay 模式，TX 线程暂停周期性发送
    /// - 按照录制的时间戳顺序发送帧
    /// - 速度限制：建议 ≤ 2.0x，最大值 5.0x
    ///
    /// # ⚠️ 安全警告
    ///
    /// - **速度限制**: 建议使用 1.0x（原始速度），最高不超过 2.0x
    /// - **人工监控**: 回放过程中应有人工急停准备
    /// - **环境确认**: 确保回放环境安全，无人员/障碍物
    /// - **文件验证**: 只回放可信来源的录制文件
    ///
    /// # 返回
    ///
    /// 返回 `Piper<Standby>`，自动退出 Replay 模式
    ///
    /// # 示例
    ///
    /// ```rust,ignore
    /// # use piper_client::PiperBuilder;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let standby = PiperBuilder::new()
    ///     .socketcan("can0")
    ///     .build()?;
    /// let replay = standby.enter_replay_mode()?;
    ///
    /// // 回放录制（1.0x 速度，原始速度）
    /// let standby = replay.replay_recording("recording.bin", 1.0)?;
    ///
    /// // 回放完成后自动返回 Standby 状态
    /// # Ok(())
    /// # }
    /// ```
    pub fn replay_recording(
        self,
        recording_path: impl AsRef<std::path::Path>,
        speed_factor: f64,
    ) -> Result<Piper<Standby, Capability>> {
        use piper_tools::PiperRecording;
        use std::thread;
        use std::time::Duration;
        const REPLAY_FRAME_COMMIT_TIMEOUT: Duration = Duration::from_millis(100);

        // === 安全检查 ===

        // 速度限制验证
        const MAX_SPEED_FACTOR: f64 = 5.0;
        const RECOMMENDED_SPEED_FACTOR: f64 = 2.0;

        if speed_factor <= 0.0 {
            return Err(crate::RobotError::InvalidParameter {
                param: "speed_factor".to_string(),
                reason: "must be positive".to_string(),
            });
        }

        if speed_factor > MAX_SPEED_FACTOR {
            return Err(crate::RobotError::InvalidParameter {
                param: "speed_factor".to_string(),
                reason: format!("exceeds maximum {}", MAX_SPEED_FACTOR),
            });
        }

        if speed_factor > RECOMMENDED_SPEED_FACTOR {
            tracing::warn!(
                "Speed factor {} exceeds recommended limit {}. \
                 Ensure safe environment and emergency stop ready.",
                speed_factor,
                RECOMMENDED_SPEED_FACTOR
            );
        }

        tracing::info!(
            "Starting replay: file={:?}, speed={:.2}x",
            recording_path.as_ref(),
            speed_factor
        );

        // === 加载录制文件 ===

        let recording = PiperRecording::load(recording_path.as_ref()).map_err(|e| {
            crate::RobotError::Infrastructure(piper_driver::DriverError::IoThread(e.to_string()))
        })?;

        if recording.frames.is_empty() {
            tracing::warn!("Recording file is empty");
            // 即使是空录制，也要正常退出 Replay 模式
        } else {
            tracing::info!(
                "Loaded {} frames, duration: {:.2}s",
                recording.frames.len(),
                recording.duration().map(|d| d.as_secs_f64()).unwrap_or(0.0)
            );
        }

        // === 回放帧序列 ===

        let mut first_frame = true;
        let mut last_timestamp_us = 0u64;

        for frame in recording.frames {
            // 计算时间间隔（考虑速度因子）
            let delay_us = if first_frame {
                first_frame = false;
                0 // 第一帧立即发送
            } else {
                let elapsed_us = frame.timestamp_us.saturating_sub(last_timestamp_us);
                // 应用速度因子：速度越快，延迟越短
                (elapsed_us as f64 / speed_factor) as u64
            };

            last_timestamp_us = frame.timestamp_us;

            // 等待适当的延迟
            if delay_us > 0 {
                let delay = Duration::from_micros(delay_us);
                thread::sleep(delay);
            }

            // 发送帧
            let piper_frame = Self::recording_frame_to_piper_frame(&frame)?;

            self.driver
                .send_replay_frame_confirmed(piper_frame, REPLAY_FRAME_COMMIT_TIMEOUT)
                .map_err(|e| {
                    crate::RobotError::Infrastructure(piper_driver::DriverError::IoThread(
                        e.to_string(),
                    ))
                })?;

            // 跟踪进度（每 1000 帧打印一次）
            if frame.timestamp_us % 1_000_000 < 1000 {
                trace!(
                    "Replayed frame at {:.3}s",
                    frame.timestamp_us as f64 / 1_000_000.0
                );
            }
        }

        tracing::info!("Replay completed successfully");
        Ok(self.exit_replay_mode_to_standby())
    }

    /// 回放录制（带取消支持）
    ///
    /// # 功能
    ///
    /// 回放预先录制的 CAN 帧序列，支持协作式取消。
    ///
    /// # 参数
    ///
    /// * `recording_path` - 录制文件路径
    /// * `speed_factor` - 回放速度倍数（1.0 = 原始速度）
    /// * `cancel_signal` - 停止信号（`AtomicBool`），检查是否需要取消
    ///
    /// # 返回
    ///
    /// * `Ok(Piper<Standby>)` - 回放完成或被取消后返回 Standby 状态
    /// * `Err(RobotError)` - 回放失败
    ///
    /// # 取消机制
    ///
    /// 此方法支持协作式取消：
    /// - 每一帧都会检查 `cancel_signal`
    /// - 如果 `cancel_signal` 为 `false`，立即停止回放
    /// - 停止后会安全退出回放模式（恢复 Driver 到 Normal 模式）
    ///
    /// # 示例
    ///
    /// ```rust,no_run
    /// # use piper_client::PiperBuilder;
    /// # use std::sync::atomic::{AtomicBool, Ordering};
    /// # use std::sync::Arc;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let robot = PiperBuilder::new()
    ///     .socketcan("can0")
    ///     .build()?;
    ///
    /// let replay = robot.enter_replay_mode()?;
    ///
    /// // 创建停止信号
    /// let running = Arc::new(AtomicBool::new(true));
    ///
    /// // 在另一个线程中设置停止信号（例如 Ctrl-C 处理器）
    /// // running.store(false, Ordering::SeqCst);
    ///
    /// // 回放（可被取消）
    /// let standby = replay.replay_recording_with_cancel(
    ///     "recording.bin",
    ///     1.0,
    ///     &running
    /// )?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn replay_recording_with_cancel(
        self,
        recording_path: impl AsRef<std::path::Path>,
        speed_factor: f64,
        cancel_signal: &std::sync::atomic::AtomicBool,
    ) -> Result<Piper<Standby, Capability>> {
        use piper_tools::PiperRecording;
        use std::time::Duration;
        const REPLAY_FRAME_COMMIT_TIMEOUT: Duration = Duration::from_millis(100);

        // === 安全检查 ===

        // 速度限制验证
        const MAX_SPEED_FACTOR: f64 = 5.0;
        const RECOMMENDED_SPEED_FACTOR: f64 = 2.0;

        if speed_factor <= 0.0 {
            return Err(crate::RobotError::InvalidParameter {
                param: "speed_factor".to_string(),
                reason: "must be positive".to_string(),
            });
        }

        if speed_factor > MAX_SPEED_FACTOR {
            return Err(crate::RobotError::InvalidParameter {
                param: "speed_factor".to_string(),
                reason: format!("exceeds maximum {}", MAX_SPEED_FACTOR),
            });
        }

        if speed_factor > RECOMMENDED_SPEED_FACTOR {
            tracing::warn!(
                "Speed factor {} exceeds recommended limit {}. \
                 Ensure safe environment and emergency stop ready.",
                speed_factor,
                RECOMMENDED_SPEED_FACTOR
            );
        }

        tracing::info!(
            "Starting replay (with cancel support): file={:?}, speed={:.2}x",
            recording_path.as_ref(),
            speed_factor
        );

        // === 加载录制文件 ===

        let recording = PiperRecording::load(recording_path.as_ref()).map_err(|e| {
            crate::RobotError::Infrastructure(piper_driver::DriverError::IoThread(e.to_string()))
        })?;

        if recording.frames.is_empty() {
            tracing::warn!("Recording file is empty");
            // 即使是空录制，也要正常退出 Replay 模式
        } else {
            tracing::info!(
                "Loaded {} frames, duration: {:.2}s",
                recording.frames.len(),
                recording.duration().map(|d| d.as_secs_f64()).unwrap_or(0.0)
            );
        }

        // === 回放帧序列（带取消检查） ===

        let mut first_frame = true;
        let mut last_timestamp_us = 0u64;

        for frame in recording.frames {
            // ✅ 每一帧都检查取消信号
            if Self::replay_cancel_requested(cancel_signal) {
                tracing::warn!("Replay cancelled by user signal");
                return Ok(self.exit_replay_mode_to_standby());
            }

            // 计算时间间隔（考虑速度因子）
            let delay_us = if first_frame {
                first_frame = false;
                0 // 第一帧立即发送
            } else {
                let elapsed_us = frame.timestamp_us.saturating_sub(last_timestamp_us);
                // 应用速度因子：速度越快，延迟越短
                (elapsed_us as f64 / speed_factor) as u64
            };

            last_timestamp_us = frame.timestamp_us;

            // 等待适当的延迟
            if delay_us > 0 {
                let delay = Duration::from_micros(delay_us);
                if !Self::wait_replay_delay_or_cancel(delay, cancel_signal) {
                    tracing::warn!("Replay cancelled by user signal");
                    return Ok(self.exit_replay_mode_to_standby());
                }
            }

            if Self::replay_cancel_requested(cancel_signal) {
                tracing::warn!("Replay cancelled by user signal");
                return Ok(self.exit_replay_mode_to_standby());
            }

            // 发送帧
            let piper_frame = Self::recording_frame_to_piper_frame(&frame)?;

            self.driver
                .send_replay_frame_confirmed(piper_frame, REPLAY_FRAME_COMMIT_TIMEOUT)
                .map_err(|e| {
                    crate::RobotError::Infrastructure(piper_driver::DriverError::IoThread(
                        e.to_string(),
                    ))
                })?;

            // 跟踪进度（每 1000 帧打印一次）
            if frame.timestamp_us % 1_000_000 < 1000 {
                trace!(
                    "Replayed frame at {:.3}s",
                    frame.timestamp_us as f64 / 1_000_000.0
                );
            }
        }

        tracing::info!("Replay completed successfully");
        Ok(self.exit_replay_mode_to_standby())
    }

    /// 退出回放模式（返回 Standby）
    ///
    /// # 功能
    ///
    /// 提前终止回放，恢复 Driver 到 Normal 模式。
    ///
    /// # 示例
    ///
    /// ```rust,ignore
    /// # use piper_client::PiperBuilder;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let standby = PiperBuilder::new()
    ///     .socketcan("can0")
    ///     .build()?;
    /// let replay = standby.enter_replay_mode()?;
    ///
    /// // 提前退出回放模式
    /// let standby = replay.stop_replay()?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn stop_replay(self) -> Result<Piper<Standby, Capability>> {
        tracing::info!("Stopping replay - exiting ReplayMode");
        Ok(self.exit_replay_mode_to_standby())
    }
}

// ==================== ErrorState 状态 ====================

impl<Capability> Piper<ErrorState, Capability>
where
    Capability: CapabilityMarker,
{
    /// 获取 Observer（只读）
    ///
    /// 即使在错误状态，也可以读取机械臂状态。
    pub fn observer(&self) -> &Observer<Capability> {
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

impl<State, Capability> Drop for Piper<State, Capability> {
    fn drop(&mut self) {
        if self.driver_mode_drop_policy == DriverModeDropPolicy::RestoreNormal {
            self.driver.set_mode(piper_driver::mode::DriverMode::Normal);
        }

        if self.drop_policy != DropPolicy::DisableAll || !self.driver.auto_disable_on_drop_allowed()
        {
            return;
        }

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
    use crate::observer::CollisionProtectionSnapshot;
    use crate::observer::Observer;
    use piper_can::{CanError, PiperFrame, RealtimeTxAdapter, RxAdapter};
    use piper_driver::Piper as RobotPiper;
    use piper_protocol::control::MitControlCommand;
    use piper_protocol::ids::{ID_JOINT_FEEDBACK_12, ID_JOINT_FEEDBACK_34, ID_JOINT_FEEDBACK_56};
    use piper_tools::{PiperRecording, RecordingMetadata, TimestampSource, TimestampedFrame};
    use semver::Version;
    use std::collections::VecDeque;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::Duration;

    struct IdleRxAdapter {
        bootstrap_emitted: bool,
    }

    impl IdleRxAdapter {
        fn new() -> Self {
            Self {
                bootstrap_emitted: false,
            }
        }
    }

    impl RxAdapter for IdleRxAdapter {
        fn receive(&mut self) -> std::result::Result<PiperFrame, CanError> {
            if !self.bootstrap_emitted {
                self.bootstrap_emitted = true;
                return Ok(bootstrap_timestamp_frame());
            }
            Err(CanError::Timeout)
        }
    }

    struct ScriptedRxAdapter {
        bootstrap: Option<PiperFrame>,
        frames: VecDeque<PiperFrame>,
    }

    impl ScriptedRxAdapter {
        fn new(frames: Vec<PiperFrame>) -> Self {
            Self {
                bootstrap: Some(bootstrap_timestamp_frame()),
                frames: frames.into(),
            }
        }
    }

    impl RxAdapter for ScriptedRxAdapter {
        fn receive(&mut self) -> std::result::Result<PiperFrame, CanError> {
            if let Some(frame) = self.bootstrap.take() {
                return Ok(frame);
            }
            self.frames.pop_front().ok_or(CanError::Timeout)
        }
    }

    struct TimedFrame {
        delay: Duration,
        frame: PiperFrame,
    }

    struct PacedRxAdapter {
        bootstrap: Option<PiperFrame>,
        frames: VecDeque<TimedFrame>,
    }

    impl PacedRxAdapter {
        fn new(frames: Vec<TimedFrame>) -> Self {
            Self {
                bootstrap: Some(bootstrap_timestamp_frame()),
                frames: frames.into(),
            }
        }
    }

    impl RxAdapter for PacedRxAdapter {
        fn receive(&mut self) -> std::result::Result<PiperFrame, CanError> {
            if let Some(frame) = self.bootstrap.take() {
                return Ok(frame);
            }
            match self.frames.pop_front() {
                Some(timed) => {
                    if !timed.delay.is_zero() {
                        thread::sleep(timed.delay);
                    }
                    Ok(timed.frame)
                },
                None => Err(CanError::Timeout),
            }
        }
    }

    struct RecordingTxAdapter {
        sent_frames: Arc<Mutex<Vec<PiperFrame>>>,
    }

    impl RecordingTxAdapter {
        fn new(sent_frames: Arc<Mutex<Vec<PiperFrame>>>) -> Self {
            Self { sent_frames }
        }
    }

    struct FailSendTxAdapter;

    impl RealtimeTxAdapter for FailSendTxAdapter {
        fn send_control(
            &mut self,
            _frame: PiperFrame,
            _budget: std::time::Duration,
        ) -> std::result::Result<(), CanError> {
            Err(CanError::BusOff)
        }

        fn send_shutdown_until(
            &mut self,
            _frame: PiperFrame,
            _deadline: std::time::Instant,
        ) -> std::result::Result<(), CanError> {
            Err(CanError::BusOff)
        }
    }

    impl RealtimeTxAdapter for RecordingTxAdapter {
        fn send_control(
            &mut self,
            frame: PiperFrame,
            budget: std::time::Duration,
        ) -> std::result::Result<(), CanError> {
            if budget.is_zero() {
                return Err(CanError::Timeout);
            }
            self.sent_frames.lock().expect("sent frames lock").push(frame);
            Ok(())
        }

        fn send_shutdown_until(
            &mut self,
            frame: PiperFrame,
            deadline: std::time::Instant,
        ) -> std::result::Result<(), CanError> {
            if deadline <= std::time::Instant::now() {
                return Err(CanError::Timeout);
            }
            self.sent_frames.lock().expect("sent frames lock").push(frame);
            Ok(())
        }
    }

    struct SoftCapabilityRx<R> {
        inner: R,
    }

    impl<R> SoftCapabilityRx<R> {
        fn new(inner: R) -> Self {
            Self { inner }
        }
    }

    impl<R> RxAdapter for SoftCapabilityRx<R>
    where
        R: RxAdapter,
    {
        fn receive(&mut self) -> std::result::Result<PiperFrame, CanError> {
            self.inner.receive()
        }

        fn backend_capability(&self) -> piper_driver::BackendCapability {
            piper_driver::BackendCapability::SoftRealtime
        }
    }

    struct MonitorCapabilityRx<R> {
        inner: R,
    }

    impl<R> MonitorCapabilityRx<R> {
        fn new(inner: R) -> Self {
            Self { inner }
        }
    }

    impl<R> RxAdapter for MonitorCapabilityRx<R>
    where
        R: RxAdapter,
    {
        fn receive(&mut self) -> std::result::Result<PiperFrame, CanError> {
            self.inner.receive()
        }

        fn backend_capability(&self) -> piper_driver::BackendCapability {
            piper_driver::BackendCapability::MonitorOnly
        }
    }

    fn bootstrap_timestamp_frame() -> PiperFrame {
        let mut frame = PiperFrame::new_standard(0x251, &[0; 8]);
        frame.timestamp_us = 1;
        frame
    }

    fn write_test_recording(frames: &[(u64, u32, &[u8])]) -> PathBuf {
        static NEXT_RECORDING_ID: AtomicUsize = AtomicUsize::new(0);

        let mut recording =
            PiperRecording::new(RecordingMetadata::new("can0".to_string(), 1_000_000));
        for (timestamp_us, can_id, data) in frames {
            recording.add_frame(TimestampedFrame::new(
                *timestamp_us,
                *can_id,
                data.to_vec(),
                TimestampSource::Hardware,
            ));
        }

        let path = std::env::temp_dir().join(format!(
            "piper-client-replay-test-{}-{}.bin",
            std::process::id(),
            NEXT_RECORDING_ID.fetch_add(1, Ordering::Relaxed),
        ));
        recording.save(&path).expect("test recording should be written successfully");
        path
    }

    fn wait_until(timeout: Duration, mut predicate: impl FnMut() -> bool, message: &str) {
        let deadline = std::time::Instant::now() + timeout;
        while std::time::Instant::now() < deadline {
            if predicate() {
                return;
            }
            thread::sleep(Duration::from_millis(1));
        }
        panic!("{message}");
    }

    fn build_active_mit_piper(
        quirks: DeviceQuirks,
        sent_frames: Arc<Mutex<Vec<PiperFrame>>>,
    ) -> Piper<Active<MitMode>, StrictRealtime> {
        let driver = Arc::new(
            RobotPiper::new_dual_thread_parts(
                IdleRxAdapter::new(),
                RecordingTxAdapter::new(sent_frames),
                None,
            )
            .expect("driver should start"),
        );
        let observer = Observer::<StrictRealtime>::new(driver.clone());

        Piper {
            driver,
            observer,
            quirks,
            drop_policy: DropPolicy::DisableAll,
            driver_mode_drop_policy: DriverModeDropPolicy::Preserve,
            _state: Active(MitMode),
        }
    }

    fn build_active_position_piper(
        driver: Arc<RobotPiper>,
    ) -> Piper<Active<PositionMode>, StrictRealtime> {
        build_active_position_piper_with_motion_type(driver, MotionType::Joint)
    }

    fn build_active_position_piper_with_motion_type(
        driver: Arc<RobotPiper>,
        motion_type: MotionType,
    ) -> Piper<Active<PositionMode>, StrictRealtime> {
        let observer = Observer::<StrictRealtime>::new(driver.clone());

        Piper {
            driver,
            observer,
            quirks: DeviceQuirks::from_firmware_version(Version::new(1, 8, 3)),
            drop_policy: DropPolicy::DisableAll,
            driver_mode_drop_policy: DriverModeDropPolicy::Preserve,
            _state: Active(PositionMode {
                command_timeout: Duration::from_millis(20),
                motion_type,
            }),
        }
    }

    fn build_standby_piper<R>(
        rx_adapter: R,
        sent_frames: Arc<Mutex<Vec<PiperFrame>>>,
    ) -> Piper<Standby, StrictRealtime>
    where
        R: RxAdapter + Send + 'static,
    {
        let driver = Arc::new(
            RobotPiper::new_dual_thread_parts(
                rx_adapter,
                RecordingTxAdapter::new(sent_frames),
                None,
            )
            .expect("driver should start"),
        );
        let observer = Observer::<StrictRealtime>::new(driver.clone());

        Piper {
            driver,
            observer,
            quirks: DeviceQuirks::from_firmware_version(Version::new(1, 8, 3)),
            drop_policy: DropPolicy::Noop,
            driver_mode_drop_policy: DriverModeDropPolicy::Preserve,
            _state: Standby,
        }
    }

    fn build_standby_piper_with_tx<R, T>(
        rx_adapter: R,
        tx_adapter: T,
    ) -> Piper<Standby, StrictRealtime>
    where
        R: RxAdapter + Send + 'static,
        T: RealtimeTxAdapter + Send + 'static,
    {
        let driver = Arc::new(
            RobotPiper::new_dual_thread_parts(rx_adapter, tx_adapter, None)
                .expect("driver should start"),
        );
        let observer = Observer::<StrictRealtime>::new(driver.clone());

        Piper {
            driver,
            observer,
            quirks: DeviceQuirks::from_firmware_version(Version::new(1, 8, 3)),
            drop_policy: DropPolicy::Noop,
            driver_mode_drop_policy: DriverModeDropPolicy::Preserve,
            _state: Standby,
        }
    }

    fn build_soft_standby_piper<R>(
        rx_adapter: R,
        sent_frames: Arc<Mutex<Vec<PiperFrame>>>,
    ) -> Piper<Standby, SoftRealtime>
    where
        R: RxAdapter + Send + 'static,
    {
        let driver = Arc::new(
            RobotPiper::new_dual_thread_parts(
                SoftCapabilityRx::new(rx_adapter),
                RecordingTxAdapter::new(sent_frames),
                None,
            )
            .expect("driver should start"),
        );
        let observer = Observer::<SoftRealtime>::new(driver.clone());

        Piper {
            driver,
            observer,
            quirks: DeviceQuirks::from_firmware_version(Version::new(1, 8, 3)),
            drop_policy: DropPolicy::Noop,
            driver_mode_drop_policy: DriverModeDropPolicy::Preserve,
            _state: Standby,
        }
    }

    fn joint_feedback_frame(
        can_id: u16,
        first_deg_milli: i32,
        second_deg_milli: i32,
        timestamp_us: u64,
    ) -> PiperFrame {
        let mut data = [0u8; 8];
        data[0..4].copy_from_slice(&first_deg_milli.to_be_bytes());
        data[4..8].copy_from_slice(&second_deg_milli.to_be_bytes());
        let mut frame = PiperFrame::new_standard(can_id, &data);
        frame.timestamp_us = timestamp_us;
        frame
    }

    fn joint_driver_enabled_frame(joint_index: u8, timestamp_us: u64) -> PiperFrame {
        let id = piper_protocol::ids::ID_JOINT_DRIVER_LOW_SPEED_BASE + (joint_index as u32) - 1;
        let mut data = [0u8; 8];
        data[0..2].copy_from_slice(&240u16.to_be_bytes());
        data[2..4].copy_from_slice(&45i16.to_be_bytes());
        data[4] = 50;
        data[5] = 0x40;
        data[6..8].copy_from_slice(&5000u16.to_be_bytes());
        let mut frame = PiperFrame::new_standard(id as u16, &data);
        frame.timestamp_us = timestamp_us;
        frame
    }

    fn robot_status_frame_with_status(
        control_mode: ControlMode,
        robot_status: RobotStatus,
        move_mode: MoveMode,
        timestamp_us: u64,
    ) -> PiperFrame {
        let mut frame = PiperFrame::new_standard(
            piper_protocol::ids::ID_ROBOT_STATUS as u16,
            &[
                control_mode as u8,
                robot_status as u8,
                move_mode as u8,
                piper_protocol::feedback::TeachStatus::Closed as u8,
                piper_protocol::feedback::MotionStatus::Arrived as u8,
                0,
                0,
                0,
            ],
        );
        frame.timestamp_us = timestamp_us;
        frame
    }

    fn robot_status_frame(
        control_mode: ControlMode,
        move_mode: MoveMode,
        timestamp_us: u64,
    ) -> PiperFrame {
        robot_status_frame_with_status(control_mode, RobotStatus::Normal, move_mode, timestamp_us)
    }

    fn control_mode_echo_frame(
        control_mode: piper_protocol::control::ControlModeCommand,
        move_mode: MoveMode,
        speed_percent: u8,
        mit_mode: piper_protocol::control::MitMode,
        install_position: InstallPosition,
        timestamp_us: u64,
    ) -> PiperFrame {
        let mut frame = piper_protocol::control::ControlModeCommandFrame::new(
            control_mode,
            move_mode,
            speed_percent,
            mit_mode,
            0,
            install_position,
        )
        .to_frame();
        frame.timestamp_us = timestamp_us;
        frame
    }

    fn enabled_joint_frames() -> Vec<TimedFrame> {
        (1..=6)
            .map(|joint_index| TimedFrame {
                delay: Duration::ZERO,
                frame: joint_driver_enabled_frame(joint_index, joint_index as u64),
            })
            .collect()
    }

    #[test]
    fn test_state_type_sizes() {
        // 大部分状态类型是 ZST（零大小类型）
        assert_eq!(std::mem::size_of::<Disconnected>(), 0);
        assert_eq!(std::mem::size_of::<Standby>(), 0);
        assert_eq!(std::mem::size_of::<MitMode>(), 0);
        assert_eq!(std::mem::size_of::<ErrorState>(), 0);

        // Active<MitMode> 包含 MitMode（ZST），所以也是 ZST
        assert_eq!(std::mem::size_of::<Active<MitMode>>(), 0);

        assert_eq!(
            std::mem::size_of::<PositionMode>(),
            std::mem::size_of::<(Duration, MotionType)>()
        );
        assert_eq!(
            std::mem::size_of::<Active<PositionMode>>(),
            std::mem::size_of::<(Duration, MotionType)>()
        );
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
        assert_eq!(config.command_timeout, Duration::from_millis(20));
    }

    #[test]
    fn test_motion_type_default() {
        assert_eq!(MotionType::default(), MotionType::Joint);
    }

    #[test]
    fn enable_mit_mode_times_out_without_fresh_control_mode_echo() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let mut frames = enabled_joint_frames();
        frames.push(TimedFrame {
            delay: Duration::from_millis(15),
            frame: robot_status_frame(ControlMode::CanControl, MoveMode::MoveM, 100),
        });

        let standby = build_standby_piper(PacedRxAdapter::new(frames), sent_frames.clone());
        let error = match standby.enable_mit_mode(MitModeConfig {
            timeout: Duration::from_millis(50),
            debounce_threshold: 1,
            poll_interval: Duration::from_millis(1),
            speed_percent: 100,
        }) {
            Ok(_) => panic!("missing 0x151 echo must block Active<MitMode>"),
            Err(error) => error,
        };

        assert!(matches!(error, RobotError::Timeout { .. }));
        assert!(
            sent_frames
                .lock()
                .expect("sent frames lock")
                .iter()
                .any(|frame| frame.id == piper_protocol::ids::ID_CONTROL_MODE),
            "mode switch command should still be sent before confirmation times out"
        );
    }

    #[test]
    fn enable_position_mode_rejects_mismatched_control_mode_echo() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let mut frames = enabled_joint_frames();
        frames.push(TimedFrame {
            delay: Duration::from_millis(15),
            frame: robot_status_frame(ControlMode::CanControl, MoveMode::MoveL, 100),
        });
        frames.push(TimedFrame {
            delay: Duration::from_millis(5),
            frame: control_mode_echo_frame(
                piper_protocol::control::ControlModeCommand::CanControl,
                MoveMode::MoveL,
                40,
                piper_protocol::control::MitMode::PositionVelocity,
                InstallPosition::Invalid,
                101,
            ),
        });

        let standby = build_standby_piper(PacedRxAdapter::new(frames), sent_frames);
        let error = match standby.enable_position_mode(PositionModeConfig {
            timeout: Duration::from_millis(50),
            debounce_threshold: 1,
            poll_interval: Duration::from_millis(1),
            speed_percent: 40,
            install_position: InstallPosition::Horizontal,
            motion_type: MotionType::Linear,
            command_timeout: Duration::from_millis(20),
        }) {
            Ok(_) => panic!("mismatched 0x151 echo must reject Active<PositionMode>"),
            Err(error) => error,
        };

        assert!(matches!(error, RobotError::Timeout { .. }));
    }

    #[test]
    fn enable_mit_mode_succeeds_with_fresh_matching_robot_and_echo() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let mut frames = enabled_joint_frames();
        frames.push(TimedFrame {
            delay: Duration::from_millis(15),
            frame: robot_status_frame(ControlMode::CanControl, MoveMode::MoveM, 100),
        });
        frames.push(TimedFrame {
            delay: Duration::from_millis(5),
            frame: control_mode_echo_frame(
                piper_protocol::control::ControlModeCommand::CanControl,
                MoveMode::MoveM,
                80,
                piper_protocol::control::MitMode::Mit,
                InstallPosition::Invalid,
                101,
            ),
        });

        let standby = build_standby_piper(PacedRxAdapter::new(frames), sent_frames);
        let active = standby
            .enable_mit_mode(MitModeConfig {
                timeout: Duration::from_millis(80),
                debounce_threshold: 1,
                poll_interval: Duration::from_millis(1),
                speed_percent: 80,
            })
            .expect("fresh matching 0x2A1 + 0x151 should allow Active<MitMode>");

        assert!(active.observer().is_all_enabled());
    }

    #[test]
    fn enable_mit_mode_rejects_non_normal_robot_status_even_if_drives_are_enabled() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let mut frames = enabled_joint_frames();
        frames.push(TimedFrame {
            delay: Duration::from_millis(15),
            frame: robot_status_frame_with_status(
                ControlMode::CanControl,
                RobotStatus::EmergencyStop,
                MoveMode::MoveM,
                100,
            ),
        });
        frames.push(TimedFrame {
            delay: Duration::from_millis(5),
            frame: control_mode_echo_frame(
                piper_protocol::control::ControlModeCommand::CanControl,
                MoveMode::MoveM,
                80,
                piper_protocol::control::MitMode::Mit,
                InstallPosition::Invalid,
                101,
            ),
        });

        let standby = build_standby_piper(PacedRxAdapter::new(frames), sent_frames);
        let error = match standby.enable_mit_mode(MitModeConfig {
            timeout: Duration::from_millis(80),
            debounce_threshold: 1,
            poll_interval: Duration::from_millis(1),
            speed_percent: 80,
        }) {
            Ok(_) => panic!("non-Normal 0x2A1 must prevent Active<MitMode>"),
            Err(error) => error,
        };

        assert!(matches!(error, RobotError::Timeout { .. }));
    }

    #[test]
    fn enable_mit_passthrough_succeeds_with_fresh_matching_robot_and_echo() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let mut frames = enabled_joint_frames();
        frames.push(TimedFrame {
            delay: Duration::from_millis(15),
            frame: robot_status_frame(ControlMode::CanControl, MoveMode::MoveM, 100),
        });
        frames.push(TimedFrame {
            delay: Duration::ZERO,
            frame: control_mode_echo_frame(
                piper_protocol::control::ControlModeCommand::CanControl,
                MoveMode::MoveM,
                70,
                piper_protocol::control::MitMode::Mit,
                InstallPosition::Invalid,
                101,
            ),
        });

        let standby = build_soft_standby_piper(PacedRxAdapter::new(frames), sent_frames);
        let active = standby
            .enable_mit_passthrough(MitModeConfig {
                timeout: Duration::from_millis(80),
                debounce_threshold: 1,
                poll_interval: Duration::from_millis(1),
                speed_percent: 70,
            })
            .expect("fresh matching 0x2A1 + 0x151 should allow MIT passthrough");

        assert!(matches!(active._state, Active(MitPassthroughMode)));
    }

    #[test]
    fn command_position_from_snapshot_does_not_require_driver_feedback() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let driver = Arc::new(
            RobotPiper::new_dual_thread_parts(
                IdleRxAdapter::new(),
                RecordingTxAdapter::new(sent_frames.clone()),
                None,
            )
            .expect("driver should start"),
        );
        let robot = build_active_position_piper(driver);

        robot
            .command_position_from_snapshot(Joint::J1, Rad(1.57), &JointArray::splat(Rad(0.0)))
            .expect("command_position_from_snapshot should succeed without feedback");

        thread::sleep(Duration::from_millis(50));
        assert_eq!(sent_frames.lock().expect("sent frames lock").len(), 3);
    }

    #[test]
    fn command_position_with_policy_rejects_stale_feedback_without_sending() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let driver = Arc::new(
            RobotPiper::new_dual_thread_parts(
                ScriptedRxAdapter::new(vec![
                    joint_feedback_frame(ID_JOINT_FEEDBACK_12 as u16, 0, 0, 1_000),
                    joint_feedback_frame(ID_JOINT_FEEDBACK_34 as u16, 0, 0, 1_000),
                    joint_feedback_frame(ID_JOINT_FEEDBACK_56 as u16, 0, 0, 1_000),
                ]),
                RecordingTxAdapter::new(sent_frames.clone()),
                None,
            )
            .expect("driver should start"),
        );
        let robot = build_active_position_piper(driver.clone());

        driver
            .wait_for_feedback(Duration::from_millis(200))
            .expect("feedback should arrive");
        thread::sleep(Duration::from_millis(25));

        let error = robot
            .command_position_with_policy(
                Joint::J1,
                Rad(1.0),
                MonitorReadPolicy {
                    max_feedback_age: Duration::from_millis(10),
                },
            )
            .expect_err("stale joint positions must reject read-modify-write");

        assert!(matches!(
            error,
            RobotError::MonitorStateStale {
                state_source: MonitorStateSource::JointPosition,
                max_age_ms: 10,
                ..
            }
        ));
        assert!(
            sent_frames.lock().expect("sent frames lock").is_empty(),
            "stale read-modify-write helper must not send frames"
        );
    }

    #[test]
    fn active_drop_sends_disable_all_but_standby_replay_error_and_monitor_do_not() {
        use piper_driver::mode::DriverMode;
        use piper_protocol::control::MotorEnableCommand;

        let disable_all_frame = MotorEnableCommand::disable_all().to_frame();

        let standby_sent = Arc::new(Mutex::new(Vec::new()));
        let standby = build_standby_piper(IdleRxAdapter::new(), standby_sent.clone());
        let standby_driver = standby.driver.clone();
        drop(standby);
        thread::sleep(Duration::from_millis(20));
        assert!(
            standby_sent.lock().expect("standby sent frames lock").is_empty(),
            "dropping Standby must not disable motors"
        );
        drop(standby_driver);

        let replay_sent = Arc::new(Mutex::new(Vec::new()));
        let replay = build_standby_piper(IdleRxAdapter::new(), replay_sent.clone())
            .enter_replay_mode()
            .expect("enter_replay_mode should succeed");
        let replay_driver = replay.driver.clone();
        assert_eq!(replay_driver.mode(), DriverMode::Replay);
        drop(replay);
        thread::sleep(Duration::from_millis(20));
        assert!(
            replay_sent.lock().expect("replay sent frames lock").is_empty(),
            "dropping ReplayMode must not disable motors"
        );
        assert_eq!(
            replay_driver.mode(),
            DriverMode::Normal,
            "dropping ReplayMode must restore driver mode to Normal"
        );
        drop(replay_driver);

        let error_sent = Arc::new(Mutex::new(Vec::new()));
        let active = build_active_mit_piper(
            DeviceQuirks::from_firmware_version(Version::new(1, 8, 3)),
            error_sent.clone(),
        );
        let error = transition_piper_state(
            active,
            ErrorState,
            DropPolicy::Noop,
            DriverModeDropPolicy::Preserve,
        );
        let error_driver = error.driver.clone();
        drop(error);
        thread::sleep(Duration::from_millis(20));
        assert!(
            error_sent.lock().expect("error sent frames lock").is_empty(),
            "dropping ErrorState must not disable motors"
        );
        drop(error_driver);

        let active_sent = Arc::new(Mutex::new(Vec::new()));
        let active = build_active_mit_piper(
            DeviceQuirks::from_firmware_version(Version::new(1, 8, 3)),
            active_sent.clone(),
        );
        let active_driver = active.driver.clone();
        drop(active);
        thread::sleep(Duration::from_millis(20));
        assert_eq!(
            active_sent.lock().expect("active sent frames lock").as_slice(),
            &[disable_all_frame],
            "dropping Active<MitMode> must best-effort disable all motors once"
        );
        drop(active_driver);

        let monitor_sent = Arc::new(Mutex::new(Vec::new()));
        let monitor_driver = Arc::new(
            RobotPiper::new_dual_thread_parts(
                MonitorCapabilityRx::new(IdleRxAdapter::new()),
                RecordingTxAdapter::new(monitor_sent.clone()),
                None,
            )
            .expect("monitor driver should start"),
        );
        let monitor = connected_piper_from_driver(
            monitor_driver,
            DeviceQuirks::from_firmware_version(Version::new(1, 8, 3)),
        );
        match monitor {
            ConnectedPiper::Monitor(piper) => {
                let monitor_driver = piper.driver.clone();
                drop(piper);
                thread::sleep(Duration::from_millis(20));
                drop(monitor_driver);
            },
            other => panic!(
                "expected monitor connection, got {:?}",
                other.backend_capability()
            ),
        }
        assert!(
            monitor_sent.lock().expect("monitor sent frames lock").is_empty(),
            "dropping monitor handle must not disable motors"
        );
    }

    #[test]
    fn monitor_connection_cannot_require_motion() {
        let driver = Arc::new(
            RobotPiper::new_dual_thread_parts(
                MonitorCapabilityRx::new(IdleRxAdapter::new()),
                RecordingTxAdapter::new(Arc::new(Mutex::new(Vec::new()))),
                None,
            )
            .expect("monitor driver should start"),
        );
        let connected = connected_piper_from_driver(
            driver,
            DeviceQuirks::from_firmware_version(Version::new(1, 8, 3)),
        );

        let error = match connected.require_motion() {
            Ok(_) => panic!("monitor-only connection must remain read-only"),
            Err(error) => error,
        };
        assert!(matches!(error, RobotError::RealtimeUnsupported { .. }));
    }

    #[test]
    fn replay_recording_load_failure_restores_driver_mode() {
        use piper_driver::mode::DriverMode;

        static NEXT_MISSING_RECORDING_ID: AtomicUsize = AtomicUsize::new(0);

        let replay = build_standby_piper(IdleRxAdapter::new(), Arc::new(Mutex::new(Vec::new())))
            .enter_replay_mode()
            .expect("enter_replay_mode should succeed");
        let driver = replay.driver.clone();
        let missing_path = std::env::temp_dir().join(format!(
            "piper-client-missing-replay-{}-{}.bin",
            std::process::id(),
            NEXT_MISSING_RECORDING_ID.fetch_add(1, Ordering::Relaxed),
        ));

        let error = match replay.replay_recording(&missing_path, 1.0) {
            Ok(_) => panic!("missing recording file must fail"),
            Err(error) => error,
        };
        assert!(matches!(error, RobotError::Infrastructure(_)));
        assert_eq!(
            driver.mode(),
            DriverMode::Normal,
            "failed replay load must restore driver mode to Normal"
        );
    }

    #[test]
    fn replay_recording_send_failure_restores_driver_mode() {
        use piper_driver::mode::DriverMode;

        let recording_path = write_test_recording(&[(1_000, 0x155, &[0x01])]);
        let standby = build_standby_piper_with_tx(IdleRxAdapter::new(), FailSendTxAdapter);
        let driver = standby.driver.clone();
        let replay = standby.enter_replay_mode().expect("enter_replay_mode should succeed");

        let error = match replay.replay_recording(&recording_path, 1.0) {
            Ok(_) => panic!("transport failure during replay must surface as error"),
            Err(error) => error,
        };
        assert!(matches!(error, RobotError::Infrastructure(_)));
        assert_eq!(
            driver.mode(),
            DriverMode::Normal,
            "failed replay send must restore driver mode to Normal"
        );

        let _ = std::fs::remove_file(recording_path);
    }

    #[test]
    fn replay_recording_with_cancel_returns_standby_and_restores_driver_mode() {
        use piper_driver::mode::DriverMode;
        use std::sync::atomic::AtomicBool;

        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let recording_path = write_test_recording(&[(1_000, 0x155, &[0x01])]);
        let replay = build_standby_piper(IdleRxAdapter::new(), sent_frames.clone())
            .enter_replay_mode()
            .expect("enter_replay_mode should succeed");
        let driver = replay.driver.clone();
        let cancel_signal = AtomicBool::new(false);

        let standby = replay
            .replay_recording_with_cancel(&recording_path, 1.0, &cancel_signal)
            .expect("cancelled replay should return to Standby instead of error");

        assert_eq!(
            driver.mode(),
            DriverMode::Normal,
            "cancelled replay must restore driver mode to Normal"
        );
        assert!(
            sent_frames.lock().expect("sent frames lock").is_empty(),
            "cancelled replay before first frame must not emit any frame"
        );
        drop(standby);
        let _ = std::fs::remove_file(recording_path);
    }

    #[test]
    fn replay_recording_with_cancel_stops_before_next_frame_after_inter_frame_cancel() {
        use piper_driver::mode::DriverMode;
        use std::sync::atomic::AtomicBool;

        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let recording_path =
            write_test_recording(&[(1_000, 0x155, &[0x01]), (101_000, 0x156, &[0x02])]);
        let replay = build_standby_piper(IdleRxAdapter::new(), sent_frames.clone())
            .enter_replay_mode()
            .expect("enter_replay_mode should succeed");
        let driver = replay.driver.clone();
        let cancel_signal = Arc::new(AtomicBool::new(true));
        let cancel_signal_worker = Arc::clone(&cancel_signal);
        let recording_path_worker = recording_path.clone();

        let handle = thread::spawn(move || {
            replay.replay_recording_with_cancel(
                &recording_path_worker,
                1.0,
                cancel_signal_worker.as_ref(),
            )
        });

        wait_until(
            Duration::from_millis(200),
            || sent_frames.lock().expect("sent frames lock").len() == 1,
            "first replay frame should be emitted before cancellation",
        );
        cancel_signal.store(false, std::sync::atomic::Ordering::Release);

        let standby = handle
            .join()
            .expect("replay worker should finish")
            .expect("cancelled replay should return Standby");

        assert_eq!(
            driver.mode(),
            DriverMode::Normal,
            "cancelled replay must restore driver mode to Normal"
        );
        let sent = sent_frames.lock().expect("sent frames lock");
        assert_eq!(
            sent.len(),
            1,
            "cancel observed during inter-frame wait must prevent the next replay frame"
        );
        assert_eq!(sent[0].id, 0x155);
        assert_eq!(sent[0].len, 1);
        assert_eq!(sent[0].data[0], 0x01);
        assert_eq!(sent[0].timestamp_us, 1_000);

        drop(standby);
        let _ = std::fs::remove_file(recording_path);
    }

    #[test]
    fn enable_position_mode_rejects_continuous_position_velocity_without_sending_any_frame() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let standby = build_standby_piper(IdleRxAdapter::new(), sent_frames.clone());

        let error = match standby.enable_position_mode(PositionModeConfig {
            motion_type: MotionType::ContinuousPositionVelocity,
            ..Default::default()
        }) {
            Ok(_) => panic!("ContinuousPositionVelocity should be rejected before mode switching"),
            Err(error) => error,
        };

        assert!(matches!(error, RobotError::ConfigError(_)));
        assert!(
            sent_frames.lock().expect("sent frames lock").is_empty(),
            "unsupported motion type must not emit enable or mode-switch frames"
        );
    }

    #[test]
    fn position_mode_runtime_motion_type_guard_rejects_mismatched_helpers_without_sending() {
        let joint_sent = Arc::new(Mutex::new(Vec::new()));
        let joint_driver = Arc::new(
            RobotPiper::new_dual_thread_parts(
                IdleRxAdapter::new(),
                RecordingTxAdapter::new(joint_sent.clone()),
                None,
            )
            .expect("joint driver should start"),
        );
        let joint_robot =
            build_active_position_piper_with_motion_type(joint_driver, MotionType::Joint);
        let zero_position = Position3D::new(0.0, 0.0, 0.0);
        let zero_orientation = EulerAngles::new(0.0, 0.0, 0.0);

        assert!(matches!(
            joint_robot.command_cartesian_pose(zero_position, zero_orientation),
            Err(RobotError::ConfigError(_))
        ));
        assert!(matches!(
            joint_robot.move_linear(zero_position, zero_orientation),
            Err(RobotError::ConfigError(_))
        ));
        assert!(matches!(
            joint_robot.move_circular(
                zero_position,
                zero_orientation,
                zero_position,
                zero_orientation
            ),
            Err(RobotError::ConfigError(_))
        ));
        assert!(
            joint_sent.lock().expect("joint sent frames lock").is_empty(),
            "mismatched Cartesian helpers must not emit any CAN frame in joint mode"
        );

        let cartesian_sent = Arc::new(Mutex::new(Vec::new()));
        let cartesian_driver = Arc::new(
            RobotPiper::new_dual_thread_parts(
                IdleRxAdapter::new(),
                RecordingTxAdapter::new(cartesian_sent.clone()),
                None,
            )
            .expect("cartesian driver should start"),
        );
        let cartesian_robot =
            build_active_position_piper_with_motion_type(cartesian_driver, MotionType::Cartesian);

        assert!(matches!(
            cartesian_robot.send_position_command(&JointArray::splat(Rad(0.0))),
            Err(RobotError::ConfigError(_))
        ));
        assert!(
            cartesian_sent.lock().expect("cartesian sent frames lock").is_empty(),
            "joint helper must not emit frames in cartesian mode"
        );
    }

    #[test]
    fn command_torques_applies_firmware_quirks_before_encoding() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let robot = build_active_mit_piper(
            DeviceQuirks::from_firmware_version(Version::new(1, 7, 2)),
            sent_frames.clone(),
        );

        let positions =
            JointArray::from([Rad(1.0), Rad(0.0), Rad(0.0), Rad(0.0), Rad(0.0), Rad(0.0)]);
        let velocities = JointArray::splat(0.0);
        let kp = JointArray::splat(0.0);
        let kd = JointArray::splat(0.0);
        let torques = JointArray::from([
            NewtonMeter(4.0),
            NewtonMeter(0.0),
            NewtonMeter(0.0),
            NewtonMeter(0.0),
            NewtonMeter(0.0),
            NewtonMeter(0.0),
        ]);

        robot
            .command_torques(&positions, &velocities, &kp, &kd, &torques)
            .expect("command_torques should succeed");

        thread::sleep(Duration::from_millis(50));

        let frames = sent_frames.lock().expect("sent frames lock").clone();
        assert_eq!(frames.len(), 6);

        let expected = MitControlCommand::try_new(1, -1.0, 0.0, 0.0, 0.0, -1.0)
            .expect("expected command should be valid")
            .to_frame();
        assert_eq!(frames[0].id, expected.id);
        assert_eq!(frames[0].data, expected.data);
    }

    #[test]
    fn command_torques_is_atomic_when_any_joint_is_invalid() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let robot = build_active_mit_piper(
            DeviceQuirks::from_firmware_version(Version::new(1, 8, 3)),
            sent_frames.clone(),
        );

        let positions = JointArray::splat(Rad(0.0));
        let velocities = JointArray::splat(0.0);
        let kp = JointArray::splat(0.0);
        let kd = JointArray::splat(0.0);
        let torques = JointArray::from([
            NewtonMeter(0.0),
            NewtonMeter(0.0),
            NewtonMeter(9.0),
            NewtonMeter(0.0),
            NewtonMeter(0.0),
            NewtonMeter(0.0),
        ]);

        let error = robot
            .command_torques(&positions, &velocities, &kp, &kd, &torques)
            .expect_err("invalid joint torque should fail the whole batch");

        assert!(matches!(
            error,
            RobotError::TorqueLimitExceeded {
                joint: Joint::J3,
                ..
            }
        ));

        thread::sleep(Duration::from_millis(50));
        assert!(
            sent_frames.lock().expect("sent frames lock").is_empty(),
            "no frames should be sent when the batch fails validation"
        );
    }

    #[test]
    fn command_torques_confirmed_applies_firmware_quirks_before_encoding() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let robot = build_active_mit_piper(
            DeviceQuirks::from_firmware_version(Version::new(1, 7, 2)),
            sent_frames.clone(),
        );

        let positions =
            JointArray::from([Rad(1.0), Rad(0.0), Rad(0.0), Rad(0.0), Rad(0.0), Rad(0.0)]);
        let velocities = JointArray::splat(0.0);
        let kp = JointArray::splat(0.0);
        let kd = JointArray::splat(0.0);
        let torques = JointArray::from([
            NewtonMeter(4.0),
            NewtonMeter(0.0),
            NewtonMeter(0.0),
            NewtonMeter(0.0),
            NewtonMeter(0.0),
            NewtonMeter(0.0),
        ]);

        robot
            .command_torques_confirmed(
                &positions,
                &velocities,
                &kp,
                &kd,
                &torques,
                Duration::from_millis(200),
            )
            .expect("command_torques_confirmed should succeed");

        let frames = sent_frames.lock().expect("sent frames lock").clone();
        assert_eq!(frames.len(), 6);

        let expected = MitControlCommand::try_new(1, -1.0, 0.0, 0.0, 0.0, -1.0)
            .expect("expected command should be valid")
            .to_frame();
        assert_eq!(frames[0].id, expected.id);
        assert_eq!(frames[0].data, expected.data);
    }

    #[test]
    fn command_torques_confirmed_is_atomic_when_any_joint_is_invalid() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let robot = build_active_mit_piper(
            DeviceQuirks::from_firmware_version(Version::new(1, 8, 3)),
            sent_frames.clone(),
        );

        let positions = JointArray::splat(Rad(0.0));
        let velocities = JointArray::splat(0.0);
        let kp = JointArray::splat(0.0);
        let kd = JointArray::splat(0.0);
        let torques = JointArray::from([
            NewtonMeter(0.0),
            NewtonMeter(0.0),
            NewtonMeter(9.0),
            NewtonMeter(0.0),
            NewtonMeter(0.0),
            NewtonMeter(0.0),
        ]);

        let error = robot
            .command_torques_confirmed(
                &positions,
                &velocities,
                &kp,
                &kd,
                &torques,
                Duration::from_millis(200),
            )
            .expect_err("invalid joint torque should fail the whole confirmed batch");

        assert!(matches!(
            error,
            RobotError::TorqueLimitExceeded {
                joint: Joint::J3,
                ..
            }
        ));
        assert!(
            sent_frames.lock().expect("sent frames lock").is_empty(),
            "no frames should be sent when confirmed batch validation fails"
        );
    }

    #[test]
    fn fresh_collision_query_rejects_stale_cached_snapshot() {
        let baseline = CollisionProtectionSnapshot {
            hardware_timestamp_us: 10,
            host_rx_mono_us: 10,
            levels: [4; 6],
        };
        let reads = AtomicUsize::new(0);

        let error = wait_for_fresh_collision_protection_update(
            Duration::from_millis(3),
            Duration::from_millis(1),
            Some(baseline),
            || Ok(()),
            || {
                reads.fetch_add(1, Ordering::SeqCst);
                Ok(baseline)
            },
        )
        .unwrap_err();

        assert!(matches!(error, RobotError::Timeout { .. }));
        assert!(reads.load(Ordering::SeqCst) >= 1);
    }

    #[test]
    fn fresh_collision_query_accepts_newer_snapshot_after_query() {
        let baseline = CollisionProtectionSnapshot {
            hardware_timestamp_us: 10,
            host_rx_mono_us: 10,
            levels: [1; 6],
        };
        let reads = AtomicUsize::new(0);
        let sent = AtomicUsize::new(0);

        let snapshot = wait_for_fresh_collision_protection_update(
            Duration::from_millis(10),
            Duration::from_millis(1),
            Some(baseline),
            || {
                sent.fetch_add(1, Ordering::SeqCst);
                Ok(())
            },
            || {
                let read = reads.fetch_add(1, Ordering::SeqCst);
                if read == 0 {
                    Ok(baseline)
                } else {
                    Ok(CollisionProtectionSnapshot {
                        hardware_timestamp_us: 11,
                        host_rx_mono_us: 11,
                        levels: [5; 6],
                    })
                }
            },
        )
        .unwrap();

        assert_eq!(sent.load(Ordering::SeqCst), 1);
        assert_eq!(snapshot.levels, [5; 6]);
        assert!(snapshot.is_newer_than(10, 10));
    }

    #[test]
    fn fresh_collision_query_times_out_without_new_feedback() {
        let error = wait_for_fresh_collision_protection_update(
            Duration::from_millis(3),
            Duration::from_millis(1),
            Some(CollisionProtectionSnapshot {
                hardware_timestamp_us: 7,
                host_rx_mono_us: 7,
                levels: [2; 6],
            }),
            || Ok(()),
            || {
                Ok(CollisionProtectionSnapshot {
                    hardware_timestamp_us: 7,
                    host_rx_mono_us: 7,
                    levels: [9; 6],
                })
            },
        )
        .unwrap_err();

        assert!(matches!(error, RobotError::Timeout { .. }));
    }

    // 注意：集成测试位于 tests/ 目录
}
