//! Type State Machine - 编译期状态安全
//!
//! 使用零大小类型（ZST）标记实现状态机，在编译期防止非法状态转换。

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::connection::{InitialMotionState, InitializedConnection, initialize_connected_driver};
use crate::state::capability::{
    CapabilityMarker, MonitorOnly, MotionCapability, SoftRealtime, StrictCapability,
    StrictRealtime, UnspecifiedCapability,
};
use crate::types::*;
use crate::{
    observer::{CollisionProtectionSnapshot, MonitorReadPolicy, Observer, RuntimeHealthSnapshot},
    raw_commander::RawCommander,
};
use piper_driver::{
    BackendCapability, DriverError, ManualFaultRecoveryResult, Piper as DriverPiper, QueryError,
    RuntimeFaultKind, SettingResponseState,
};
use piper_protocol::control::{InstallPosition, MitControlCommand, MitMode as ProtocolMitMode};
use piper_protocol::feedback::{ControlMode, MoveMode, RobotStatus};
use tracing::{debug, info, trace, warn};

const COLLISION_QUERY_POLL_INTERVAL: Duration = Duration::from_millis(10);
const ZERO_SETTING_CONFIRM_TIMEOUT: Duration = Duration::from_secs(2);
const ZERO_SETTING_POLL_INTERVAL: Duration = Duration::from_millis(10);
const STATE_TRANSITION_SEND_TIMEOUT: Duration = Duration::from_millis(50);
const EMERGENCY_STOP_LANE_TIMEOUT: Duration = Duration::from_millis(20);
const RECOVERY_STATE_POLL_INTERVAL: Duration = Duration::from_millis(10);

// ==================== 状态类型（零大小类型）====================

/// 未连接状态
///
/// 这是初始状态，在此状态下无法进行任何操作。
pub struct Disconnected;

/// 待机状态
///
/// 已连接但未使能。可以读取状态，但不能发送运动命令。
pub struct Standby;

/// 维护状态
///
/// 已连接但不保证全关节失能，允许执行局部使能/失能等维护操作。
pub struct Maintenance {
    pending_disable_commit_host_mono_us: Option<u64>,
}

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
/// # use piper_client::{MotionConnectedPiper, MotionConnectedState, PiperBuilder};
/// # fn main() -> anyhow::Result<()> {
/// let connected = PiperBuilder::new()
///     .socketcan("can0")
///     .build()?
///     .require_motion()?;
/// let standby = match connected {
///     MotionConnectedPiper::Strict(MotionConnectedState::Standby(robot)) => robot,
///     MotionConnectedPiper::Soft(MotionConnectedState::Standby(robot)) => robot,
///     MotionConnectedPiper::Strict(MotionConnectedState::Maintenance(_))
///     | MotionConnectedPiper::Soft(MotionConnectedState::Maintenance(_)) => {
///         anyhow::bail!("robot is not in confirmed Standby");
///     }
/// };
///
/// // 进入回放模式
/// let replay = standby.enter_replay_mode()?;
///
/// // 回放录制（1.0x 速度，原始速度）
/// let _standby = replay.replay_recording("recording.bin", 1.0)?;
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

#[derive(Clone, Copy)]
struct SettingResponseSnapshot {
    hardware_timestamp_us: u64,
    host_rx_mono_us: u64,
    response_index: u8,
    zero_point_success: bool,
    is_valid: bool,
}

impl SettingResponseSnapshot {
    fn is_newer_than(self, hardware_timestamp_us: u64, host_rx_mono_us: u64) -> bool {
        self.hardware_timestamp_us > hardware_timestamp_us || self.host_rx_mono_us > host_rx_mono_us
    }
}

impl From<SettingResponseState> for SettingResponseSnapshot {
    fn from(value: SettingResponseState) -> Self {
        Self {
            hardware_timestamp_us: value.hardware_timestamp_us,
            host_rx_mono_us: value.host_rx_mono_us,
            response_index: value.response_index,
            zero_point_success: value.zero_point_success,
            is_valid: value.is_valid,
        }
    }
}

fn format_joint_mask(mask: u8) -> String {
    format!("{mask:06b}")
}

fn format_optional_joint_mask(mask: Option<u8>) -> String {
    match mask {
        Some(mask) => format!("Some({})", format_joint_mask(mask)),
        None => "None".to_string(),
    }
}

fn enabled_mask_from_low_speed_complete(driver_state: &piper_driver::JointDriverLowSpeed) -> u8 {
    driver_state.joints.iter().enumerate().fold(0, |mask, (index, joint)| {
        if joint.enabled {
            mask | (1 << index)
        } else {
            mask
        }
    })
}

fn enabled_mask_from_low_speed_partial(partial: &piper_driver::PartialJointDriverLowSpeed) -> u8 {
    partial.joints.iter().enumerate().fold(0, |mask, (index, joint)| {
        if joint.map(|joint| joint.enabled).unwrap_or(false) {
            mask | (1 << index)
        } else {
            mask
        }
    })
}

fn summarize_low_speed_observation(
    observation: &piper_driver::observation::Observation<
        piper_driver::JointDriverLowSpeed,
        piper_driver::PartialJointDriverLowSpeed,
    >,
) -> String {
    match observation {
        piper_driver::observation::Observation::Unavailable => "unavailable".to_string(),
        piper_driver::observation::Observation::Available(available) => match &available.payload {
            piper_driver::observation::ObservationPayload::Complete(driver_state) => format!(
                "complete freshness={:?} meta={:?} enabled_mask={}",
                available.freshness,
                available.meta,
                format_joint_mask(enabled_mask_from_low_speed_complete(driver_state))
            ),
            piper_driver::observation::ObservationPayload::Partial { partial, missing } => format!(
                "partial freshness={:?} meta={:?} enabled_mask={} missing={:?}",
                available.freshness,
                available.meta,
                format_joint_mask(enabled_mask_from_low_speed_partial(partial)),
                missing.missing_indices
            ),
        },
    }
}

fn enable_timeout_diagnostics_summary(driver: &DriverPiper, commit_host_mono_us: u64) -> String {
    let robot_control = driver.get_robot_control();
    let confirmed_after_commit =
        driver.confirmed_driver_enabled_mask_after_host_mono(commit_host_mono_us);
    let low_speed = driver.get_joint_driver_low_speed();
    let health = driver.health();

    format!(
        "commit_host_mono_us={commit_host_mono_us} driver_enabled_mask={} confirmed_mask={} confirmed_after_commit={} robot_control={robot_control:?} low_speed={} health={health:?}",
        format_joint_mask(robot_control.driver_enabled_mask),
        format_optional_joint_mask(robot_control.confirmed_driver_enabled_mask),
        format_optional_joint_mask(confirmed_after_commit),
        summarize_low_speed_observation(&low_speed),
    )
}

fn mode_confirmation_timeout_diagnostics_summary(
    driver: &DriverPiper,
    commit_host_mono_us: u64,
    expected: ModeConfirmationExpectation,
) -> String {
    let robot_control = driver.get_robot_control();
    let mode_echo = driver.get_control_mode_echo();
    let low_speed = driver.get_joint_driver_low_speed();
    let health = driver.health();

    let robot_state_fresh = robot_control.host_rx_mono_us > commit_host_mono_us;
    let robot_state_match = robot_control.is_fully_enabled_confirmed()
        && robot_control.robot_status == RobotStatus::Normal as u8
        && robot_control.control_mode == expected.control_mode
        && robot_control.move_mode == expected.move_mode;
    let mode_echo_fresh = mode_echo.is_valid && mode_echo.host_rx_mono_us > commit_host_mono_us;
    let mode_echo_match = mode_echo.control_mode == expected.control_mode
        && mode_echo.move_mode == expected.move_mode
        && mode_echo.speed_percent == expected.speed_percent
        && mode_echo.mit_mode == expected.mit_mode
        && mode_echo.install_position == expected.install_position
        && mode_echo.trajectory_stay_time == expected.trajectory_stay_time;

    format!(
        "commit_host_mono_us={commit_host_mono_us} expected={{control_mode={}, move_mode={}, speed_percent={}, mit_mode={}, install_position={}, trajectory_stay_time={}}} robot_state_fresh={} robot_state_match={} mode_echo_fresh={} mode_echo_match={} robot_control={robot_control:?} mode_echo={mode_echo:?} low_speed={} health={health:?}",
        expected.control_mode,
        expected.move_mode,
        expected.speed_percent,
        expected.mit_mode,
        expected.install_position,
        expected.trajectory_stay_time,
        robot_state_fresh,
        robot_state_match,
        mode_echo_fresh,
        mode_echo_match,
        summarize_low_speed_observation(&low_speed),
    )
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
    /// - `Joint`: 使用 `send_position_command()` 或 `command_position_from_snapshot()`
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
    Strict(MotionConnectedState<StrictRealtime>),
    Soft(MotionConnectedState<SoftRealtime>),
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
            Self::Strict(state) => state.require_standby(),
            Self::Soft(_) | Self::Monitor(_) => Err(RobotError::realtime_unsupported(
                "this connection is not StrictRealtime; match on ConnectedPiper or request AutoStrict",
            )),
        }
    }

    pub fn require_motion(self) -> Result<MotionConnectedPiper> {
        match self {
            Self::Strict(state) => Ok(MotionConnectedPiper::Strict(state)),
            Self::Soft(state) => Ok(MotionConnectedPiper::Soft(state)),
            Self::Monitor(_) => Err(RobotError::realtime_unsupported(
                "monitor-only connections cannot enter motion control states",
            )),
        }
    }
}

pub enum MotionConnectedState<Capability>
where
    Capability: MotionCapability,
{
    Standby(Piper<Standby, Capability>),
    Maintenance(Piper<Maintenance, Capability>),
}

impl<Capability> MotionConnectedState<Capability>
where
    Capability: MotionCapability,
{
    pub fn observer(&self) -> &Observer<Capability> {
        match self {
            Self::Standby(piper) => piper.observer(),
            Self::Maintenance(piper) => piper.observer(),
        }
    }

    pub fn require_standby(self) -> Result<Piper<Standby, Capability>> {
        match self {
            Self::Standby(piper) => Ok(piper),
            Self::Maintenance(piper) => Err(RobotError::maintenance_required(
                piper.observer().joint_enabled_mask_confirmed(),
            )),
        }
    }

    pub fn into_maintenance(self) -> Piper<Maintenance, Capability> {
        match self {
            Self::Standby(piper) => piper.into_maintenance(),
            Self::Maintenance(piper) => piper,
        }
    }
}

pub enum MotionConnectedPiper {
    Strict(MotionConnectedState<StrictRealtime>),
    Soft(MotionConnectedState<SoftRealtime>),
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

#[cfg(test)]
fn wait_for_fresh_collision_protection_update<SendQuery, ReadCached>(
    timeout: Duration,
    poll_interval: Duration,
    min_host_rx_mono_us: u64,
    mut send_query: SendQuery,
    mut read_cached: ReadCached,
) -> Result<CollisionProtectionSnapshot>
where
    SendQuery: FnMut() -> Result<()>,
    ReadCached: FnMut() -> Result<CollisionProtectionSnapshot>,
{
    send_query()?;

    let start = Instant::now();
    loop {
        let state = read_cached()?;
        if state.host_rx_mono_us > min_host_rx_mono_us {
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

fn wait_for_fresh_setting_response<ReadCached, CheckHealth>(
    timeout: Duration,
    poll_interval: Duration,
    baseline: Option<SettingResponseSnapshot>,
    expected_response_index: u8,
    mut read_cached: ReadCached,
    mut check_health: CheckHealth,
) -> Result<SettingResponseSnapshot>
where
    ReadCached: FnMut() -> Result<SettingResponseSnapshot>,
    CheckHealth: FnMut() -> Result<()>,
{
    let baseline_hw = baseline.as_ref().map_or(0, |state| state.hardware_timestamp_us);
    let baseline_sys = baseline.as_ref().map_or(0, |state| state.host_rx_mono_us);
    let start = Instant::now();

    loop {
        check_health()?;

        let state = read_cached()?;
        if state.is_valid
            && state.response_index == expected_response_index
            && state.is_newer_than(baseline_hw, baseline_sys)
        {
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

fn build_motion_connected_state<Capability>(
    driver: Arc<piper_driver::Piper>,
    quirks: DeviceQuirks,
    initial_state: InitialMotionState,
) -> MotionConnectedState<Capability>
where
    Capability: MotionCapability,
{
    match initial_state {
        InitialMotionState::Standby => MotionConnectedState::Standby(Piper {
            observer: Observer::<Capability>::new(driver.clone()),
            driver,
            quirks,
            drop_policy: DropPolicy::Noop,
            driver_mode_drop_policy: DriverModeDropPolicy::Preserve,
            _state: Standby,
        }),
        InitialMotionState::Maintenance { .. } => MotionConnectedState::Maintenance(Piper {
            observer: Observer::<Capability>::new(driver.clone()),
            driver,
            quirks,
            drop_policy: DropPolicy::DisableAll,
            driver_mode_drop_policy: DriverModeDropPolicy::Preserve,
            _state: Maintenance {
                pending_disable_commit_host_mono_us: None,
            },
        }),
    }
}

pub(crate) fn connected_piper_from_driver(
    driver: Arc<piper_driver::Piper>,
    initialized: InitializedConnection,
) -> Result<ConnectedPiper> {
    let InitializedConnection {
        quirks,
        initial_state,
    } = initialized;

    match driver.backend_capability() {
        BackendCapability::StrictRealtime => {
            Ok(ConnectedPiper::Strict(build_motion_connected_state::<
                StrictRealtime,
            >(
                driver, quirks, initial_state
            )))
        },
        BackendCapability::SoftRealtime => {
            Ok(ConnectedPiper::Soft(build_motion_connected_state::<
                SoftRealtime,
            >(
                driver, quirks, initial_state
            )))
        },
        BackendCapability::MonitorOnly => match initial_state {
            InitialMotionState::Standby => Ok(ConnectedPiper::Monitor(Piper {
                observer: Observer::<MonitorOnly>::new(driver.clone()),
                driver,
                quirks,
                drop_policy: DropPolicy::Noop,
                driver_mode_drop_policy: DriverModeDropPolicy::Preserve,
                _state: Standby,
            })),
            InitialMotionState::Maintenance { .. } => Err(RobotError::maintenance_required(
                initial_state.confirmed_mask(),
            )),
        },
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

    // SAFETY: `this` is ManuallyDrop, and each field is moved exactly once into
    // the replacement state wrapper.
    let driver = unsafe { std::ptr::read(&this.driver) };
    // SAFETY: see `driver` above.
    let observer = unsafe { std::ptr::read(&this.observer) };
    // SAFETY: see `driver` above. Moving avoids leaking the original Version allocation.
    let quirks = unsafe { std::ptr::read(&this.quirks) };

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
        let initialized = initialize_connected_driver(
            driver.clone(),
            config.feedback_timeout,
            config.firmware_timeout,
        )?;

        connected_piper_from_driver(driver, initialized)
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
        let initialized = initialize_connected_driver(
            driver.clone(),
            config.feedback_timeout,
            config.firmware_timeout,
        )?;

        // 5. 返回到 Standby 状态
        info!("Reconnection successful");
        connected_piper_from_driver(driver, initialized)
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
        let enable_commit_host_mono_us = self
            .driver
            .send_reliable_frame_confirmed_commit_marker(enable_cmd.to_frame(), config.timeout)?;
        debug!("Motor enable command sent");

        // 2. 等待使能完成（带 Debounce）
        self.wait_for_enabled(
            enable_commit_host_mono_us,
            config.timeout,
            config.debounce_threshold,
            config.poll_interval,
        )?;
        debug!("All motors enabled (debounced)");

        // 3. 设置 MIT 模式
        let control_cmd = ControlModeCommandFrame::new(
            ControlModeCommand::CanControl,
            MoveMode::MoveM,
            config.speed_percent,
            piper_protocol::control::MitMode::Mit,
            0,
            InstallPosition::Invalid,
        );
        let commit_host_mono_us = self
            .driver
            .send_reliable_frame_confirmed_commit_marker(control_cmd.to_frame(), config.timeout)?;
        self.wait_for_mode_confirmation(
            commit_host_mono_us,
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
        let enable_cmd = piper_protocol::control::MotorEnableCommand::enable_all();
        let enable_commit_host_mono_us = self
            .driver
            .send_reliable_frame_confirmed_commit_marker(enable_cmd.to_frame(), config.timeout)?;
        debug!("Motor enable command sent");

        // 2. 等待使能完成（带 Debounce）
        self.wait_for_enabled(
            enable_commit_host_mono_us,
            config.timeout,
            config.debounce_threshold,
            config.poll_interval,
        )?;
        debug!("All motors enabled (debounced)");

        // 3. 设置位置模式
        self.apply_position_mode_control_config(&config)?;
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

    /// 进入维护状态。
    ///
    /// Maintenance 明确表示当前连接不再承诺“全关节确认失能”。
    pub fn into_maintenance(self) -> Piper<Maintenance, Capability>
    where
        Capability: MotionCapability,
    {
        transition_piper_state(
            self,
            Maintenance {
                pending_disable_commit_host_mono_us: None,
            },
            DropPolicy::DisableAll,
            DriverModeDropPolicy::Preserve,
        )
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
        commit_host_mono_us: u64,
        timeout: Duration,
        debounce_threshold: usize,
        poll_interval: Duration,
    ) -> Result<()> {
        let start = Instant::now();
        let mut stable_count = 0;

        loop {
            self.ensure_runtime_health_healthy()?;

            // 细粒度超时检查
            if start.elapsed() > timeout {
                warn!(
                    "Timed out waiting for all motors enabled: {}",
                    enable_timeout_diagnostics_summary(&self.driver, commit_host_mono_us)
                );
                return Err(RobotError::Timeout {
                    timeout_ms: timeout.as_millis() as u64,
                });
            }

            // ✅ 直接从 Observer 读取状态（View 模式，零延迟）
            if self.driver.confirmed_driver_enabled_mask_after_host_mono(commit_host_mono_us)
                == Some(0b11_1111)
            {
                // ✅ Debounce：连续 N 次读到 Enabled 才认为成功
                stable_count += 1;
                if stable_count >= debounce_threshold {
                    return Ok(());
                }
            } else {
                // 状态跳变，重置计数器
                stable_count = 0;
            }

            self.sleep_with_fail_fast(start, timeout, poll_interval)?;
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
    ///     .build()?
    ///     .require_strict()?;
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
        use crate::recording::{
            RecordingHandle, RecordingHandleParts, RecordingStopCondition, StopCondition,
        };

        let stop_on_id = match &config.stop_condition {
            StopCondition::OnCanId(id) => Some(*id),
            _ => None,
        };
        let stop_duration = match &config.stop_condition {
            StopCondition::Duration(seconds) => Some(Duration::from_secs(*seconds)),
            _ => None,
        };
        let stop_after_frame_count = match &config.stop_condition {
            StopCondition::FrameCount(count) => Some(*count as u64),
            _ => None,
        };

        let (hook, rx) = piper_driver::recording::AsyncRecordingHook::with_auto_stop(
            stop_on_id,
            stop_duration,
            stop_after_frame_count,
        );

        let dropped = hook.dropped_frames().clone();
        let counter = hook.frame_counter().clone();
        let stop_requested = hook.stop_requested().clone();

        // 注册钩子
        let callback = std::sync::Arc::new(hook) as std::sync::Arc<dyn piper_driver::FrameCallback>;
        let hook_manager = self.driver.hooks();
        let hook_handle = hook_manager
            .write()
            .map_err(|_e| {
                crate::RobotError::Infrastructure(piper_driver::DriverError::PoisonedLock)
            })?
            .add_callback(callback);

        let stop_condition = match &config.stop_condition {
            StopCondition::Manual => RecordingStopCondition::Manual,
            StopCondition::OnCanId(_) => RecordingStopCondition::OnCanId,
            StopCondition::Duration(seconds) => {
                RecordingStopCondition::Duration(std::time::Duration::from_secs(*seconds))
            },
            StopCondition::FrameCount(count) => RecordingStopCondition::FrameCount(*count as u64),
        };
        let handle = RecordingHandle::new(RecordingHandleParts {
            rx,
            dropped_frames: dropped,
            frame_counter: counter,
            stop_requested,
            output_path: config.output_path.clone(),
            metadata: config.metadata.clone(),
            start_time_unix_secs: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            start_time: std::time::Instant::now(),
            hook_manager,
            hook_handle,
            stop_condition,
        });

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
    ///     .build()?
    ///     .require_strict()?;
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

        handle.stop();
        handle.detach_hook();

        // 创建录制对象
        let mut recording = PiperRecording::new(piper_tools::RecordingMetadata::new(
            self.driver.interface(),
            self.driver.bus_speed(),
        ));
        recording.metadata.start_time = handle.start_time_unix_secs();
        recording.metadata.notes = handle.metadata().notes.clone();
        recording.metadata.operator = handle.metadata().operator.clone();

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
    ///     .build()?
    ///     .require_strict()?;
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
    /// let standby = PiperBuilder::new().socketcan("can0").build()?.require_strict()?;
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

    /// 主动查询当前关节角度/速度限位并等待设备反馈。
    pub fn query_joint_limit_config(
        &self,
        timeout: Duration,
    ) -> Result<piper_driver::observation::Complete<piper_driver::state::JointLimitConfig>> {
        self.ensure_runtime_health_healthy()?;

        match self.driver.query_joint_limit_config(timeout) {
            Ok(complete) => Ok(complete),
            Err(QueryError::Busy) => Err(RobotError::ConfigError(
                "joint limit query already in flight".to_string(),
            )),
            Err(QueryError::Timeout) => Err(RobotError::Timeout {
                timeout_ms: timeout.as_millis() as u64,
            }),
            Err(QueryError::DiagnosticsOnlyTimeout) => Err(RobotError::ConfigError(
                "joint limit query produced diagnostics but no publishable value".to_string(),
            )),
            Err(QueryError::Driver(error)) => Err(error.into()),
        }
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
    /// let standby = PiperBuilder::new().socketcan("can0").build()?.require_strict()?;
    ///
    /// // 设置 J1 的当前位置为零点
    /// // 注意：确保 J1 已移动到预期的零点位置
    /// standby.set_joint_zero_positions(&[0])?;
    ///
    /// // 设置所有关节的零位
    /// standby.set_joint_zero_positions(&[0, 1, 2, 3, 4, 5])?;
    ///
    /// // 注意：任意 2~5 个关节的子集批量回零会被拒绝，
    /// // 因为协议没有提供原子确认语义。
    /// # Ok(())
    /// # }
    /// ```
    pub fn set_joint_zero_positions(&self, joints: &[usize]) -> Result<()> {
        if joints.is_empty() {
            return Ok(());
        }

        // Invalidate any cached 0x476 before issuing the new request so the subsequent
        // wait only observes a response that arrived after this call started.
        self.driver.clear_setting_response()?;
        let raw = RawCommander::new(&self.driver);
        raw.request_joint_zero_positions_confirmed(joints, ZERO_SETTING_CONFIRM_TIMEOUT)?;

        let response = wait_for_fresh_setting_response(
            ZERO_SETTING_CONFIRM_TIMEOUT,
            ZERO_SETTING_POLL_INTERVAL,
            None,
            0x75,
            || self.setting_response_cached_inner(),
            || self.ensure_runtime_health_healthy(),
        )?;

        if response.zero_point_success {
            Ok(())
        } else {
            Err(RobotError::ConfigError(
                "controller rejected joint zero-point setting".to_string(),
            ))
        }
    }
}

impl<Capability> Piper<Maintenance, Capability>
where
    Capability: MotionCapability,
{
    /// 获取 Observer（只读）
    pub fn observer(&self) -> &Observer<Capability> {
        &self.observer
    }

    /// 使能指定关节，并保持在 Maintenance。
    pub fn enable_joints(self, joints: &[Joint]) -> Result<Piper<Maintenance, Capability>> {
        use piper_protocol::control::MotorEnableCommand;

        for &joint in joints {
            let cmd = MotorEnableCommand::enable((joint.index() + 1) as u8);
            self.driver.send_reliable(cmd.to_frame())?;
        }

        Ok(self)
    }

    /// 使能单个关节，并保持在 Maintenance。
    pub fn enable_joint(self, joint: Joint) -> Result<Piper<Maintenance, Capability>> {
        self.enable_joints(&[joint])
    }

    /// 失能指定关节，并保持在 Maintenance。
    pub fn disable_joints(self, joints: &[Joint]) -> Result<Piper<Maintenance, Capability>> {
        use piper_protocol::control::MotorEnableCommand;

        for &joint in joints {
            let cmd = MotorEnableCommand::disable((joint.index() + 1) as u8);
            self.driver.send_reliable(cmd.to_frame())?;
        }

        Ok(self)
    }

    /// 失能单个关节，并保持在 Maintenance。
    pub fn disable_joint(self, joint: Joint) -> Result<Piper<Maintenance, Capability>> {
        self.disable_joints(&[joint])
    }

    /// 请求全关节失能，但仍保持在 Maintenance，直到确认完成。
    pub fn request_disable_all(mut self) -> Result<Piper<Maintenance, Capability>> {
        self._state.pending_disable_commit_host_mono_us = Some(self.send_disable_request()?);
        Ok(self)
    }

    /// 等待确认全关节失能，并返回 Standby。
    pub fn wait_until_disabled(self, config: DisableConfig) -> Result<Piper<Standby, Capability>> {
        self.wait_for_disabled(
            self._state.pending_disable_commit_host_mono_us,
            config.timeout,
            config.debounce_threshold,
            config.poll_interval,
        )?;

        Ok(transition_piper_state(
            self,
            Standby,
            DropPolicy::Noop,
            DriverModeDropPolicy::Preserve,
        ))
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
        let enable_commit_host_mono_us = self
            .driver
            .send_reliable_frame_confirmed_commit_marker(enable_cmd.to_frame(), config.timeout)?;
        self.wait_for_enabled(
            enable_commit_host_mono_us,
            config.timeout,
            config.debounce_threshold,
            config.poll_interval,
        )?;

        let control_cmd = ControlModeCommandFrame::new(
            ControlModeCommand::CanControl,
            MoveMode::MoveM,
            config.speed_percent,
            piper_protocol::control::MitMode::Mit,
            0,
            InstallPosition::Invalid,
        );
        let commit_host_mono_us = self
            .driver
            .send_reliable_frame_confirmed_commit_marker(control_cmd.to_frame(), config.timeout)?;
        self.wait_for_mode_confirmation(
            commit_host_mono_us,
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
    fn ensure_runtime_health_healthy(&self) -> Result<()> {
        let health = self.runtime_health();
        if health.fault.is_some() || !health.rx_alive || !health.tx_alive {
            return Err(RobotError::runtime_health_unhealthy(
                health.rx_alive,
                health.tx_alive,
                health.fault,
            ));
        }
        Ok(())
    }

    fn sleep_with_fail_fast(
        &self,
        start: Instant,
        timeout: Duration,
        poll_interval: Duration,
    ) -> Result<()> {
        let remaining = timeout.saturating_sub(start.elapsed());
        let sleep_duration = poll_interval.min(remaining);
        if sleep_duration.is_zero() {
            return Err(RobotError::Timeout {
                timeout_ms: timeout.as_millis() as u64,
            });
        }

        std::thread::sleep(sleep_duration);
        Ok(())
    }

    fn send_disable_request(&self) -> Result<u64> {
        use piper_protocol::control::MotorEnableCommand;

        self.driver
            .send_local_state_transition_frame_confirmed_commit_marker(
                MotorEnableCommand::disable_all().to_frame(),
                STATE_TRANSITION_SEND_TIMEOUT,
            )
            .map_err(Into::into)
    }

    fn wait_for_mode_confirmation(
        &self,
        commit_host_mono_us: u64,
        timeout: Duration,
        poll_interval: Duration,
        expected: ModeConfirmationExpectation,
    ) -> Result<()> {
        let start = Instant::now();

        loop {
            self.ensure_runtime_health_healthy()?;

            if start.elapsed() > timeout {
                warn!(
                    "Timed out waiting for mode confirmation: {}",
                    mode_confirmation_timeout_diagnostics_summary(
                        &self.driver,
                        commit_host_mono_us,
                        expected,
                    )
                );
                return Err(RobotError::Timeout {
                    timeout_ms: timeout.as_millis() as u64,
                });
            }

            let current = self.driver.get_robot_control();
            let mode_echo = self.driver.get_control_mode_echo();
            let is_robot_state_fresh = current.host_rx_mono_us > commit_host_mono_us;
            let is_robot_state_match = current.is_fully_enabled_confirmed()
                && current.robot_status == RobotStatus::Normal as u8
                && current.control_mode == expected.control_mode
                && current.move_mode == expected.move_mode;
            let is_mode_echo_fresh =
                mode_echo.is_valid && mode_echo.host_rx_mono_us > commit_host_mono_us;
            let is_mode_echo_match = mode_echo.control_mode == expected.control_mode
                && mode_echo.move_mode == expected.move_mode
                && mode_echo.speed_percent == expected.speed_percent
                && mode_echo.mit_mode == expected.mit_mode
                && mode_echo.install_position == expected.install_position
                && mode_echo.trajectory_stay_time == expected.trajectory_stay_time;
            let is_mode_echo_satisfied = !is_mode_echo_fresh || is_mode_echo_match;
            if is_robot_state_fresh && is_robot_state_match && is_mode_echo_satisfied {
                return Ok(());
            }

            self.sleep_with_fail_fast(start, timeout, poll_interval)?;
        }
    }

    fn apply_position_mode_control_config(&self, config: &PositionModeConfig) -> Result<()>
    where
        Capability: MotionCapability,
    {
        use piper_protocol::control::{ControlModeCommand, ControlModeCommandFrame, MitMode};

        if config.motion_type == MotionType::ContinuousPositionVelocity {
            return Err(RobotError::ConfigError(
                "MotionType::ContinuousPositionVelocity is not implemented yet".to_string(),
            ));
        }

        let move_mode: MoveMode = config.motion_type.into();
        let control_cmd = ControlModeCommandFrame::new(
            ControlModeCommand::CanControl,
            move_mode,
            config.speed_percent,
            MitMode::PositionVelocity,
            0,
            config.install_position,
        );
        let commit_host_mono_us = self
            .driver
            .send_reliable_frame_confirmed_commit_marker(control_cmd.to_frame(), config.timeout)?;
        self.wait_for_mode_confirmation(
            commit_host_mono_us,
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
        )
    }

    fn into_state<NextState>(
        self,
        next_state: NextState,
        drop_policy: DropPolicy,
        driver_mode_drop_policy: DriverModeDropPolicy,
    ) -> Piper<NextState, Capability> {
        let this = std::mem::ManuallyDrop::new(self);
        // SAFETY: `this` is ManuallyDrop, and each field is moved exactly once
        // into the replacement state wrapper.
        let driver = unsafe { std::ptr::read(&this.driver) };
        // SAFETY: see `driver` above.
        let observer = unsafe { std::ptr::read(&this.observer) };
        // SAFETY: see `driver` above. Moving avoids leaking the original Version allocation.
        let quirks = unsafe { std::ptr::read(&this.quirks) };

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
        _poll_interval: Duration,
    ) -> Result<CollisionProtectionSnapshot> {
        self.ensure_runtime_health_healthy()?;

        match self.driver.query_collision_protection(timeout) {
            Ok(complete) => Ok(CollisionProtectionSnapshot::from_driver_observation(
                complete.value,
                complete.meta,
            )),
            Err(QueryError::Busy) => Err(RobotError::ConfigError(
                "collision protection query already in flight".to_string(),
            )),
            Err(QueryError::Timeout) => Err(RobotError::Timeout {
                timeout_ms: timeout.as_millis() as u64,
            }),
            Err(QueryError::DiagnosticsOnlyTimeout) => Err(RobotError::ConfigError(
                "collision protection query produced diagnostics but no publishable value"
                    .to_string(),
            )),
            Err(QueryError::Driver(error)) => Err(error.into()),
        }
    }

    fn collision_protection_cached_inner(&self) -> Result<CollisionProtectionSnapshot> {
        match self.driver.get_collision_protection() {
            piper_driver::observation::Observation::Available(available) => match available.payload
            {
                piper_driver::observation::ObservationPayload::Complete(value) => Ok(
                    CollisionProtectionSnapshot::from_driver_observation(value, available.meta),
                ),
                piper_driver::observation::ObservationPayload::Partial { .. } => {
                    Err(RobotError::ConfigError(
                        "collision protection observation unexpectedly partial".to_string(),
                    ))
                },
            },
            piper_driver::observation::Observation::Unavailable => Err(RobotError::ConfigError(
                "collision protection observation unavailable".to_string(),
            )),
        }
    }

    fn setting_response_cached_inner(&self) -> Result<SettingResponseSnapshot> {
        self.driver
            .get_setting_response()
            .map(SettingResponseSnapshot::from)
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
    /// let robot = PiperBuilder::new().socketcan("can0").build()?.require_strict()?;
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
            raw_commander.emergency_stop_enqueue(Instant::now() + EMERGENCY_STOP_LANE_TIMEOUT)?;
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
        commit_host_mono_us: Option<u64>,
        timeout: Duration,
        debounce_threshold: usize,
        poll_interval: Duration,
    ) -> Result<()> {
        let start = Instant::now();
        let mut stable_count = 0;

        loop {
            self.ensure_runtime_health_healthy()?;

            if start.elapsed() > timeout {
                return Err(RobotError::Timeout {
                    timeout_ms: timeout.as_millis() as u64,
                });
            }

            let confirmed_mask = match commit_host_mono_us {
                Some(commit_host_mono_us) => {
                    self.driver.confirmed_driver_enabled_mask_after_host_mono(commit_host_mono_us)
                },
                None => self.driver.get_robot_control().confirmed_driver_enabled_mask,
            };
            if confirmed_mask == Some(0) {
                stable_count += 1;
                if stable_count >= debounce_threshold {
                    return Ok(());
                }
            } else {
                stable_count = 0;
            }

            self.sleep_with_fail_fast(start, timeout, poll_interval)?;
        }
    }
}

// ==================== Active<Mode> 状态（通用方法） ====================

impl<M, Capability> Piper<Active<M>, Capability>
where
    Capability: MotionCapability,
{
    /// 请求立即失能全部关节，并进入 Maintenance。
    ///
    /// 这是急停/人工接管路径：只发送 disable 请求，不伪装成已确认失能的 Standby。
    /// 如果 state-transition disable 在进入 TX commit 前或发送点超时，
    /// 调用会返回错误，同时底层 driver 会 fail-closed 锁存 `TransportError`。
    pub fn request_disable_all(self) -> Result<Piper<Maintenance, Capability>> {
        let disable_commit_host_mono_us = self.send_disable_request()?;
        Ok(self.into_state(
            Maintenance {
                pending_disable_commit_host_mono_us: Some(disable_commit_host_mono_us),
            },
            DropPolicy::DisableAll,
            DriverModeDropPolicy::Preserve,
        ))
    }

    /// 优雅关闭机械臂
    ///
    /// 执行受控的 disable-only 关闭序列：
    /// 1. 终止待发送的普通控制命令
    /// 2. 线性化发送 `disable_all`
    /// 3. 等待失能确认
    /// 4. 返回到 Standby 状态
    ///
    /// 如果第 2 步在进入 TX commit 前或发送点超时，调用会立即返回错误，
    /// 同时底层 driver 会 fail-closed 锁存 `TransportError`，后续普通控制会被拒绝。
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
        info!("Starting graceful robot shutdown");

        trace!("Disabling motors");
        let disable_commit_host_mono_us = self.send_disable_request()?;

        // 2. 等待失能确认
        trace!("Waiting for disable confirmation");
        self.wait_for_disabled(
            Some(disable_commit_host_mono_us),
            Duration::from_secs(1),
            1, // debounce_threshold
            Duration::from_millis(10),
        )?;

        info!("Robot shutdown complete");
        Ok(self.into_state(Standby, DropPolicy::Noop, DriverModeDropPolicy::Preserve))
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
    /// # use piper_client::{MotionConnectedPiper, MotionConnectedState, PiperBuilder};
    /// # use piper_client::state::{MotionCapability, Piper, Standby};
    /// # fn run_example<C: MotionCapability>(
    /// #     standby: Piper<Standby, C>,
    /// # ) -> Result<(), Box<dyn std::error::Error>> {
    /// let active = standby.enable_position_mode(Default::default())?;
    ///
    /// // 获取诊断接口
    /// let diag = active.diagnostics();
    ///
    /// // diag 可以安全地移动到其他线程
    /// std::thread::spawn(move || {
    ///     // 在这里使用 diag...
    /// });
    /// # Ok(())
    /// # }
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let robot = PiperBuilder::new()
    ///     .socketcan("can0")
    ///     .build()?;
    ///
    /// match robot.require_motion()? {
    ///     MotionConnectedPiper::Strict(MotionConnectedState::Standby(standby)) => run_example(standby)?,
    ///     MotionConnectedPiper::Soft(MotionConnectedState::Standby(standby)) => run_example(standby)?,
    ///     MotionConnectedPiper::Strict(MotionConnectedState::Maintenance(_))
    ///     | MotionConnectedPiper::Soft(MotionConnectedState::Maintenance(_)) => {
    ///         return Err("robot is not in confirmed Standby".into());
    ///     }
    /// }
    ///
    /// // 真实代码里，active 仍然可以正常使用
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
    ///     .build()?
    ///     .require_strict()?;
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
        use crate::recording::{
            RecordingHandle, RecordingHandleParts, RecordingStopCondition, StopCondition,
        };

        let stop_on_id = match &config.stop_condition {
            StopCondition::OnCanId(id) => Some(*id),
            _ => None,
        };
        let stop_duration = match &config.stop_condition {
            StopCondition::Duration(seconds) => Some(Duration::from_secs(*seconds)),
            _ => None,
        };
        let stop_after_frame_count = match &config.stop_condition {
            StopCondition::FrameCount(count) => Some(*count as u64),
            _ => None,
        };

        let (hook, rx) = piper_driver::recording::AsyncRecordingHook::with_auto_stop(
            stop_on_id,
            stop_duration,
            stop_after_frame_count,
        );

        let dropped = hook.dropped_frames().clone();
        let counter = hook.frame_counter().clone();
        let stop_requested = hook.stop_requested().clone();

        // 注册钩子
        let callback = std::sync::Arc::new(hook) as std::sync::Arc<dyn piper_driver::FrameCallback>;
        let hook_manager = self.driver.hooks();
        let hook_handle = hook_manager
            .write()
            .map_err(|_e| {
                crate::RobotError::Infrastructure(piper_driver::DriverError::PoisonedLock)
            })?
            .add_callback(callback);

        let stop_condition = match &config.stop_condition {
            StopCondition::Manual => RecordingStopCondition::Manual,
            StopCondition::OnCanId(_) => RecordingStopCondition::OnCanId,
            StopCondition::Duration(seconds) => {
                RecordingStopCondition::Duration(std::time::Duration::from_secs(*seconds))
            },
            StopCondition::FrameCount(count) => RecordingStopCondition::FrameCount(*count as u64),
        };
        let handle = RecordingHandle::new(RecordingHandleParts {
            rx,
            dropped_frames: dropped,
            frame_counter: counter,
            stop_requested,
            output_path: config.output_path.clone(),
            metadata: config.metadata.clone(),
            start_time_unix_secs: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            start_time: std::time::Instant::now(),
            hook_manager,
            hook_handle,
            stop_condition,
        });

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
    ///     .build()?
    ///     .require_strict()?;
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

        handle.stop();
        handle.detach_hook();

        // 创建录制对象
        let mut recording = PiperRecording::new(piper_tools::RecordingMetadata::new(
            self.driver.interface(),
            self.driver.bus_speed(),
        ));
        recording.metadata.start_time = handle.start_time_unix_secs();
        recording.metadata.notes = handle.metadata().notes.clone();
        recording.metadata.operator = handle.metadata().operator.clone();

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
    /// let standby = PiperBuilder::new().socketcan("can0").build()?.require_strict()?;
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
        debug!("Disabling robot");

        // === PHASE 1: All operations that can panic ===

        // 1. 失能机械臂
        let disable_commit_host_mono_us = self.send_disable_request()?;
        debug!("Motor disable command sent");

        // 2. 等待失能完成（带 Debounce）
        self.wait_for_disabled(
            Some(disable_commit_host_mono_us),
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
        debug!("Disabling robot");

        let disable_commit_host_mono_us = self.send_disable_request()?;
        self.wait_for_disabled(
            Some(disable_commit_host_mono_us),
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

    /// 重新应用位置模式的控制配置（0x151），保持当前 `Active<PositionMode>` 不变。
    ///
    /// 借用态重新配置只能更新由控制器保存并可通过 0x151 确认的字段，例如
    /// `speed_percent` 和 `install_position`。`motion_type` 与 `command_timeout`
    /// 仍由当前 `Active<PositionMode>` 的本地状态定义。
    ///
    /// `motion_type` 必须与当前 Active 状态一致；传入的 `command_timeout`
    /// 不会发送到控制器，因此会被忽略，并继续沿用当前 Active 状态里的本地值。
    pub fn reapply_position_mode_config(&self, config: PositionModeConfig) -> Result<()> {
        let position_mode = &self._state.0;
        if config.motion_type != position_mode.motion_type {
            return Err(RobotError::ConfigError(format!(
                "reapply_position_mode_config requires MotionType::{expected:?}, but config requested MotionType::{actual:?}",
                expected = position_mode.motion_type,
                actual = config.motion_type,
            )));
        }

        let effective_config = PositionModeConfig {
            command_timeout: position_mode.command_timeout,
            ..config
        };

        debug!(
            "Reapplying Position mode config (motion_type={:?}, speed_percent={}, install_position={:?})",
            effective_config.motion_type,
            effective_config.speed_percent,
            effective_config.install_position
        );
        self.apply_position_mode_control_config(&effective_config)
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
        debug!("Disabling robot");

        // === PHASE 1: All operations that can panic ===

        // 1. 失能机械臂
        let disable_commit_host_mono_us = self.send_disable_request()?;
        debug!("Motor disable command sent");

        // 2. 等待失能完成（带 Debounce）
        self.wait_for_disabled(
            Some(disable_commit_host_mono_us),
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
            // Replay timing is enforced by the inter-frame delay schedule, not by carrying the
            // historical capture timestamp into a new TX event.
            timestamp_us: 0,
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
    ///     .build()?
    ///     .require_strict()?;
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
    /// # use piper_client::{MotionConnectedPiper, MotionConnectedState, PiperBuilder};
    /// # use piper_client::state::{MotionCapability, Piper, Standby};
    /// # use std::sync::atomic::{AtomicBool, Ordering};
    /// # use std::sync::Arc;
    /// # fn run_example<C: MotionCapability>(
    /// #     standby: Piper<Standby, C>,
    /// # ) -> Result<(), Box<dyn std::error::Error>> {
    /// let replay = standby.enter_replay_mode()?;
    ///
    /// // 创建停止信号
    /// let running = Arc::new(AtomicBool::new(true));
    ///
    /// // 在另一个线程中设置停止信号（例如 Ctrl-C 处理器）
    /// // running.store(false, Ordering::SeqCst);
    ///
    /// // 回放（可被取消）
    /// let standby = replay.replay_recording_with_cancel("recording.bin", 1.0, &running)?;
    /// # let _ = standby;
    /// # Ok(())
    /// # }
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let robot = PiperBuilder::new()
    ///     .socketcan("can0")
    ///     .build()?;
    ///
    /// match robot.require_motion()? {
    ///     MotionConnectedPiper::Strict(MotionConnectedState::Standby(standby)) => run_example(standby)?,
    ///     MotionConnectedPiper::Soft(MotionConnectedState::Standby(standby)) => run_example(standby)?,
    ///     MotionConnectedPiper::Strict(MotionConnectedState::Maintenance(_))
    ///     | MotionConnectedPiper::Soft(MotionConnectedState::Maintenance(_)) => {
    ///         return Err("robot is not in confirmed Standby".into());
    ///     }
    /// }
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
    ///     .build()?
    ///     .require_strict()?;
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

impl<Capability> Piper<ErrorState, Capability>
where
    Capability: MotionCapability,
{
    /// 从手动急停中恢复。
    ///
    /// 恢复序列固定为：
    /// 1. 验证当前 runtime fault 确实是 `ManualFault`
    /// 2. 通过 shutdown lane 发送 `0x150 resume`
    /// 3. 要求 resume 时已经持有一份完整的 pre-resume 6 轴低速基线
    /// 4. 等待一份严格新于 resume 发送确认时刻的完整 6 轴低速反馈
    /// 5. 仅在拿到 fresh confirmed mask 后，driver 才清除 fault latch 并重新打开 normal gate
    /// 6. `mask == 0` 返回 `Standby`，否则返回 `Maintenance`
    ///
    /// `timeout` 覆盖 resume 发送、确认以及 fresh post-resume 反馈等待的总预算。
    /// 如果缺少完整 pre-resume 基线，或在预算内拿不到 fresh post-resume 反馈，
    /// 则保持 fault latched 并返回超时错误。
    pub fn recover_from_emergency_stop(
        self,
        timeout: Duration,
    ) -> Result<MotionConnectedState<Capability>> {
        let health = self.runtime_health();
        if health.fault != Some(RuntimeFaultKind::ManualFault)
            || !health.rx_alive
            || !health.tx_alive
        {
            return Err(RobotError::runtime_health_unhealthy(
                health.rx_alive,
                health.tx_alive,
                health.fault,
            ));
        }

        let deadline = Instant::now() + timeout;
        let raw_commander = RawCommander::new(&self.driver);
        let receipt = raw_commander.emergency_stop_resume_enqueue(deadline)?;
        match receipt.wait() {
            Ok(()) => {},
            Err(DriverError::Timeout) => {
                return Err(RobotError::Timeout {
                    timeout_ms: timeout.as_millis() as u64,
                });
            },
            Err(error) => return Err(error.into()),
        }

        let recovery = match self.driver.complete_manual_fault_recovery_after_resume_until(
            deadline,
            RECOVERY_STATE_POLL_INTERVAL,
        ) {
            Ok(recovery) => recovery,
            Err(DriverError::Timeout) => {
                return Err(RobotError::Timeout {
                    timeout_ms: timeout.as_millis() as u64,
                });
            },
            Err(error) => return Err(error.into()),
        };

        Ok(match recovery {
            ManualFaultRecoveryResult::Standby => MotionConnectedState::Standby(self.into_state(
                Standby,
                DropPolicy::Noop,
                DriverModeDropPolicy::Preserve,
            )),
            ManualFaultRecoveryResult::Maintenance { .. } => {
                MotionConnectedState::Maintenance(self.into_state(
                    Maintenance {
                        pending_disable_commit_host_mono_us: None,
                    },
                    DropPolicy::DisableAll,
                    DriverModeDropPolicy::Preserve,
                ))
            },
        })
    }
}

// ==================== Drop 实现（安全关闭）====================

impl<State, Capability> Drop for Piper<State, Capability> {
    fn drop(&mut self) {
        if self.driver_mode_drop_policy == DriverModeDropPolicy::RestoreNormal {
            self.driver.set_mode(piper_driver::mode::DriverMode::Normal);
        }

        if self.drop_policy != DropPolicy::DisableAll {
            return;
        }

        self.driver.best_effort_disable_or_shutdown_on_drop(EMERGENCY_STOP_LANE_TIMEOUT);
    }
}

// ==================== 测试 ====================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::{ControlError, MitController, MitControllerConfig};
    use crate::observer::CollisionProtectionSnapshot;
    use crate::observer::Observer;
    use piper_can::{CanError, PiperFrame, RealtimeTxAdapter, RxAdapter};
    use piper_driver::observation::{Observation, ObservationPayload};
    use piper_driver::{DriverMode, Piper as RobotPiper, RuntimeFaultKind};
    use piper_protocol::control::MitControlCommand;
    use piper_protocol::ids::{
        ID_JOINT_CONTROL_12, ID_JOINT_CONTROL_34, ID_JOINT_CONTROL_56, ID_JOINT_FEEDBACK_12,
        ID_JOINT_FEEDBACK_34, ID_JOINT_FEEDBACK_56,
    };
    use piper_tools::{PiperRecording, RecordingMetadata, TimestampSource, TimestampedFrame};
    use semver::Version;
    use std::collections::VecDeque;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex, mpsc};
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

    struct DelayOnNthShutdownTxAdapter {
        sent_frames: Arc<Mutex<Vec<PiperFrame>>>,
        shutdown_sends: usize,
        delay_on: usize,
        delay: Duration,
    }

    struct TimeoutControlTxAdapter {
        sent_frames: Arc<Mutex<Vec<PiperFrame>>>,
    }

    struct BlockingFirstControlTxAdapter {
        sent_frames: Arc<Mutex<Vec<PiperFrame>>>,
        started_tx: mpsc::Sender<()>,
        release_rx: mpsc::Receiver<()>,
        sends: usize,
    }

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

    impl RealtimeTxAdapter for DelayOnNthShutdownTxAdapter {
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
            self.shutdown_sends += 1;
            if self.shutdown_sends == self.delay_on && !self.delay.is_zero() {
                thread::sleep(self.delay);
            }
            if deadline <= std::time::Instant::now() {
                return Err(CanError::Timeout);
            }
            self.sent_frames.lock().expect("sent frames lock").push(frame);
            Ok(())
        }
    }

    impl RealtimeTxAdapter for TimeoutControlTxAdapter {
        fn send_control(
            &mut self,
            _frame: PiperFrame,
            _budget: std::time::Duration,
        ) -> std::result::Result<(), CanError> {
            Err(CanError::Timeout)
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

    impl RealtimeTxAdapter for BlockingFirstControlTxAdapter {
        fn send_control(
            &mut self,
            frame: PiperFrame,
            budget: std::time::Duration,
        ) -> std::result::Result<(), CanError> {
            if budget.is_zero() {
                return Err(CanError::Timeout);
            }
            self.sends += 1;
            self.sent_frames.lock().expect("sent frames lock").push(frame);
            if self.sends == 1 {
                let _ = self.started_tx.send(());
                let _ = self.release_rx.recv();
            }
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

    fn temp_recording_path(prefix: &str) -> PathBuf {
        static NEXT_RECORDING_ID: AtomicUsize = AtomicUsize::new(0);

        let id = NEXT_RECORDING_ID.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "piper-client-{prefix}-{}-{}.bin",
            std::process::id(),
            id
        ))
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

    fn latest_joint_6_low_speed_hardware_timestamp(driver: &RobotPiper) -> Option<u64> {
        match driver.get_joint_driver_low_speed() {
            Observation::Available(available) => match available.payload {
                ObservationPayload::Complete(driver_state) => {
                    driver_state.joints[5].hardware_timestamp_us
                },
                ObservationPayload::Partial { partial, .. } => {
                    partial.joints[5].and_then(|joint| joint.hardware_timestamp_us)
                },
            },
            Observation::Unavailable => None,
        }
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
        build_active_mit_piper_with_driver(driver, quirks)
    }

    fn build_active_mit_piper_with_driver(
        driver: Arc<RobotPiper>,
        quirks: DeviceQuirks,
    ) -> Piper<Active<MitMode>, StrictRealtime> {
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
        build_standby_piper_with_config(rx_adapter, sent_frames, None)
    }

    fn build_standby_piper_with_config<R>(
        rx_adapter: R,
        sent_frames: Arc<Mutex<Vec<PiperFrame>>>,
        config: Option<piper_driver::PipelineConfig>,
    ) -> Piper<Standby, StrictRealtime>
    where
        R: RxAdapter + Send + 'static,
    {
        let driver = Arc::new(
            RobotPiper::new_dual_thread_parts(
                rx_adapter,
                RecordingTxAdapter::new(sent_frames),
                config,
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

    fn joint_driver_state_frame(joint_index: u8, enabled: bool, timestamp_us: u64) -> PiperFrame {
        let id = piper_protocol::ids::ID_JOINT_DRIVER_LOW_SPEED_BASE + (joint_index as u32) - 1;
        let mut data = [0u8; 8];
        data[0..2].copy_from_slice(&240u16.to_be_bytes());
        data[2..4].copy_from_slice(&45i16.to_be_bytes());
        data[4] = 50;
        data[5] = if enabled { 0x40 } else { 0x00 };
        data[6..8].copy_from_slice(&5000u16.to_be_bytes());
        let mut frame = PiperFrame::new_standard(id as u16, &data);
        frame.timestamp_us = timestamp_us;
        frame
    }

    fn joint_driver_enabled_frame(joint_index: u8, timestamp_us: u64) -> PiperFrame {
        joint_driver_state_frame(joint_index, true, timestamp_us)
    }

    fn joint_driver_disabled_frame(joint_index: u8, timestamp_us: u64) -> PiperFrame {
        joint_driver_state_frame(joint_index, false, timestamp_us)
    }

    fn joint_dynamic_frame(
        joint_index: u8,
        speed_millirad_per_sec: i16,
        current_milliamp: i16,
        timestamp_us: u64,
    ) -> PiperFrame {
        let mut data = [0u8; 8];
        data[0..2].copy_from_slice(&speed_millirad_per_sec.to_be_bytes());
        data[2..4].copy_from_slice(&current_milliamp.to_be_bytes());
        data[4..8].copy_from_slice(&0i32.to_be_bytes());
        let mut frame = PiperFrame::new_standard(
            (piper_protocol::ids::ID_JOINT_DRIVER_HIGH_SPEED_BASE + u32::from(joint_index - 1))
                as u16,
            &data,
        );
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

    fn setting_response_frame(
        response_index: u8,
        zero_point_success: bool,
        timestamp_us: u64,
    ) -> PiperFrame {
        let mut frame = PiperFrame::new_standard(
            piper_protocol::ids::ID_SETTING_RESPONSE as u16,
            &[
                response_index,
                if zero_point_success { 0x01 } else { 0x00 },
                0,
                0,
                0,
                0,
                0,
                0,
            ],
        );
        frame.timestamp_us = timestamp_us;
        frame
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

    fn enabled_joint_frames_after(delay: Duration) -> Vec<TimedFrame> {
        delayed_joint_state_frames(true, delay, Duration::ZERO, 1)
    }

    fn disabled_joint_frames_after(first_delay: Duration, timestamp_base: u64) -> Vec<TimedFrame> {
        delayed_joint_state_frames(false, first_delay, Duration::ZERO, timestamp_base)
    }

    fn delayed_joint_state_frames(
        enabled: bool,
        first_delay: Duration,
        inter_frame_delay: Duration,
        timestamp_base: u64,
    ) -> Vec<TimedFrame> {
        (1..=6)
            .map(|joint_index| TimedFrame {
                delay: if joint_index == 1 {
                    first_delay
                } else {
                    inter_frame_delay
                },
                frame: if enabled {
                    joint_driver_enabled_frame(joint_index, timestamp_base + joint_index as u64)
                } else {
                    joint_driver_disabled_frame(joint_index, timestamp_base + joint_index as u64)
                },
            })
            .collect()
    }

    fn control_snapshot_frames(timestamp_us: u64) -> Vec<TimedFrame> {
        vec![
            TimedFrame {
                delay: Duration::ZERO,
                frame: joint_feedback_frame(ID_JOINT_FEEDBACK_12 as u16, 0, 0, timestamp_us),
            },
            TimedFrame {
                delay: Duration::ZERO,
                frame: joint_feedback_frame(ID_JOINT_FEEDBACK_34 as u16, 0, 0, timestamp_us),
            },
            TimedFrame {
                delay: Duration::ZERO,
                frame: joint_feedback_frame(ID_JOINT_FEEDBACK_56 as u16, 0, 0, timestamp_us),
            },
            TimedFrame {
                delay: Duration::ZERO,
                frame: joint_dynamic_frame(1, 0, 0, timestamp_us),
            },
            TimedFrame {
                delay: Duration::ZERO,
                frame: joint_dynamic_frame(2, 0, 0, timestamp_us),
            },
            TimedFrame {
                delay: Duration::ZERO,
                frame: joint_dynamic_frame(3, 0, 0, timestamp_us),
            },
            TimedFrame {
                delay: Duration::ZERO,
                frame: joint_dynamic_frame(4, 0, 0, timestamp_us),
            },
            TimedFrame {
                delay: Duration::ZERO,
                frame: joint_dynamic_frame(5, 0, 0, timestamp_us),
            },
            TimedFrame {
                delay: Duration::ZERO,
                frame: joint_dynamic_frame(6, 0, 0, timestamp_us),
            },
        ]
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
    fn socketcan_without_control_mode_echo_allows_enable_mit_mode() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let mut frames = enabled_joint_frames_after(Duration::from_millis(10));
        frames.push(TimedFrame {
            delay: Duration::from_millis(15),
            frame: robot_status_frame(ControlMode::CanControl, MoveMode::MoveM, 100),
        });

        let standby = build_standby_piper(PacedRxAdapter::new(frames), sent_frames.clone());
        let active = standby
            .enable_mit_mode(MitModeConfig {
                timeout: Duration::from_millis(50),
                debounce_threshold: 1,
                poll_interval: Duration::from_millis(1),
                speed_percent: 100,
            })
            .expect("fresh matching 0x2A1 should allow Active<MitMode> even without 0x151 echo");

        assert!(active.observer().is_all_enabled());
        assert!(active.observer().is_all_enabled_confirmed());
        assert!(
            sent_frames
                .lock()
                .expect("sent frames lock")
                .iter()
                .any(|frame| frame.id == piper_protocol::ids::ID_CONTROL_MODE),
            "mode switch command should still be sent before confirmation succeeds"
        );
    }

    #[test]
    fn socketcan_without_control_mode_echo_allows_enable_position_mode() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let mut frames = enabled_joint_frames_after(Duration::from_millis(10));
        frames.push(TimedFrame {
            delay: Duration::from_millis(15),
            frame: robot_status_frame(ControlMode::CanControl, MoveMode::MoveJ, 100),
        });

        let standby = build_standby_piper(PacedRxAdapter::new(frames), sent_frames.clone());
        let active = standby
            .enable_position_mode(PositionModeConfig {
                timeout: Duration::from_millis(50),
                debounce_threshold: 1,
                poll_interval: Duration::from_millis(1),
                speed_percent: 10,
                install_position: InstallPosition::Invalid,
                motion_type: MotionType::Joint,
                command_timeout: Duration::from_millis(20),
            })
            .expect(
                "fresh matching 0x2A1 should allow Active<PositionMode> even without 0x151 echo",
            );

        assert!(active.observer().is_all_enabled());
        assert!(active.observer().is_all_enabled_confirmed());
        assert!(
            sent_frames
                .lock()
                .expect("sent frames lock")
                .iter()
                .any(|frame| frame.id == piper_protocol::ids::ID_CONTROL_MODE),
            "mode switch command should still be sent before confirmation succeeds"
        );
    }

    #[test]
    fn socketcan_without_control_mode_echo_relies_on_robot_status_confirmation() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let mut frames = enabled_joint_frames_after(Duration::from_millis(10));
        frames.push(TimedFrame {
            delay: Duration::from_millis(15),
            frame: robot_status_frame(ControlMode::CanControl, MoveMode::MoveJ, 100),
        });

        let standby = build_standby_piper(PacedRxAdapter::new(frames), sent_frames);
        let active = standby
            .enable_position_mode(PositionModeConfig {
                timeout: Duration::from_millis(50),
                debounce_threshold: 1,
                poll_interval: Duration::from_millis(1),
                speed_percent: 10,
                install_position: InstallPosition::Invalid,
                motion_type: MotionType::Joint,
                command_timeout: Duration::from_millis(20),
            })
            .expect("matching robot status should be sufficient when no 0x151 echo is observable");

        let control = active.driver.get_robot_control();
        let mode_echo = active.driver.get_control_mode_echo();

        assert_eq!(control.control_mode, ControlMode::CanControl as u8);
        assert_eq!(control.move_mode, MoveMode::MoveJ as u8);
        assert!(control.is_fully_enabled_confirmed());
        assert!(
            !mode_echo.is_valid,
            "SocketCAN loopback-disabled regression should not observe a 0x151 echo"
        );
    }

    #[test]
    fn active_position_mode_reapply_config_sends_confirmed_control_mode_update() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let mut frames = enabled_joint_frames_after(Duration::from_millis(10));
        frames.push(TimedFrame {
            delay: Duration::from_millis(15),
            frame: robot_status_frame(ControlMode::CanControl, MoveMode::MoveJ, 100),
        });
        frames.push(TimedFrame {
            delay: Duration::from_millis(5),
            frame: robot_status_frame(ControlMode::CanControl, MoveMode::MoveJ, 200),
        });
        frames.push(TimedFrame {
            delay: Duration::from_millis(1),
            frame: control_mode_echo_frame(
                piper_protocol::control::ControlModeCommand::CanControl,
                MoveMode::MoveJ,
                5,
                piper_protocol::control::MitMode::PositionVelocity,
                InstallPosition::SideLeft,
                201,
            ),
        });

        let standby = build_standby_piper(PacedRxAdapter::new(frames), sent_frames.clone());
        let active = standby
            .enable_position_mode(PositionModeConfig {
                timeout: Duration::from_millis(80),
                debounce_threshold: 1,
                poll_interval: Duration::from_millis(1),
                speed_percent: 10,
                install_position: InstallPosition::Invalid,
                motion_type: MotionType::Joint,
                command_timeout: Duration::from_millis(20),
            })
            .expect("matching 0x2A1 should allow Active<PositionMode>");

        active
            .reapply_position_mode_config(PositionModeConfig {
                timeout: Duration::from_millis(80),
                debounce_threshold: 1,
                poll_interval: Duration::from_millis(1),
                speed_percent: 5,
                install_position: InstallPosition::SideLeft,
                motion_type: MotionType::Joint,
                command_timeout: Duration::from_millis(20),
            })
            .expect("borrowed Active<PositionMode> should confirm a fresh 0x151 update");

        let control_mode_frames: Vec<_> = sent_frames
            .lock()
            .expect("sent frames lock")
            .iter()
            .filter(|frame| frame.id == piper_protocol::ids::ID_CONTROL_MODE)
            .copied()
            .collect();

        assert_eq!(control_mode_frames.len(), 2);
        assert_eq!(
            control_mode_frames[1],
            piper_protocol::control::ControlModeCommandFrame::new(
                piper_protocol::control::ControlModeCommand::CanControl,
                MoveMode::MoveJ,
                5,
                piper_protocol::control::MitMode::PositionVelocity,
                0,
                InstallPosition::SideLeft,
            )
            .to_frame()
        );
    }

    #[test]
    fn active_position_mode_reapply_config_allows_incoming_command_timeout_mismatch() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let mut frames = enabled_joint_frames_after(Duration::from_millis(10));
        frames.push(TimedFrame {
            delay: Duration::from_millis(15),
            frame: robot_status_frame(ControlMode::CanControl, MoveMode::MoveJ, 100),
        });
        frames.push(TimedFrame {
            delay: Duration::from_millis(5),
            frame: robot_status_frame(ControlMode::CanControl, MoveMode::MoveJ, 200),
        });
        frames.push(TimedFrame {
            delay: Duration::from_millis(1),
            frame: control_mode_echo_frame(
                piper_protocol::control::ControlModeCommand::CanControl,
                MoveMode::MoveJ,
                5,
                piper_protocol::control::MitMode::PositionVelocity,
                InstallPosition::SideRight,
                201,
            ),
        });

        let standby = build_standby_piper(PacedRxAdapter::new(frames), sent_frames.clone());
        let active = standby
            .enable_position_mode(PositionModeConfig {
                timeout: Duration::from_millis(80),
                debounce_threshold: 1,
                poll_interval: Duration::from_millis(1),
                speed_percent: 10,
                install_position: InstallPosition::Invalid,
                motion_type: MotionType::Joint,
                command_timeout: Duration::from_millis(75),
            })
            .expect(
                "matching 0x2A1 should allow Active<PositionMode> with non-default command_timeout",
            );

        active
            .reapply_position_mode_config(PositionModeConfig {
                timeout: Duration::from_millis(80),
                debounce_threshold: 1,
                poll_interval: Duration::from_millis(1),
                speed_percent: 5,
                install_position: InstallPosition::SideRight,
                motion_type: MotionType::Joint,
                command_timeout: Duration::from_millis(20),
            })
            .expect("borrowed reapply should ignore incoming command_timeout mismatch");

        let control_mode_frames: Vec<_> = sent_frames
            .lock()
            .expect("sent frames lock")
            .iter()
            .filter(|frame| frame.id == piper_protocol::ids::ID_CONTROL_MODE)
            .copied()
            .collect();

        assert_eq!(control_mode_frames.len(), 2);
        assert_eq!(
            control_mode_frames[1],
            piper_protocol::control::ControlModeCommandFrame::new(
                piper_protocol::control::ControlModeCommand::CanControl,
                MoveMode::MoveJ,
                5,
                piper_protocol::control::MitMode::PositionVelocity,
                0,
                InstallPosition::SideRight,
            )
            .to_frame()
        );
    }

    #[test]
    fn active_position_mode_reapply_config_rejects_motion_type_change() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let mut frames = enabled_joint_frames_after(Duration::from_millis(10));
        frames.push(TimedFrame {
            delay: Duration::from_millis(15),
            frame: robot_status_frame(ControlMode::CanControl, MoveMode::MoveJ, 100),
        });

        let standby = build_standby_piper(PacedRxAdapter::new(frames), sent_frames.clone());
        let active = standby
            .enable_position_mode(PositionModeConfig {
                timeout: Duration::from_millis(80),
                debounce_threshold: 1,
                poll_interval: Duration::from_millis(1),
                speed_percent: 10,
                install_position: InstallPosition::Invalid,
                motion_type: MotionType::Joint,
                command_timeout: Duration::from_millis(20),
            })
            .expect("matching 0x2A1 should allow Active<PositionMode>");

        let error = active
            .reapply_position_mode_config(PositionModeConfig {
                motion_type: MotionType::Linear,
                ..PositionModeConfig::default()
            })
            .expect_err("borrowed Active<PositionMode> must not silently change motion type");

        assert!(matches!(error, RobotError::ConfigError(_)));
        let control_mode_frames = sent_frames
            .lock()
            .expect("sent frames lock")
            .iter()
            .filter(|frame| frame.id == piper_protocol::ids::ID_CONTROL_MODE)
            .count();
        assert_eq!(
            control_mode_frames, 1,
            "rejected reconfiguration must not send an extra 0x151 frame"
        );
    }

    fn assert_enable_position_mode_succeeds_for_motion_type(
        motion_type: MotionType,
        expected_move_mode: MoveMode,
    ) {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let mut frames = enabled_joint_frames_after(Duration::from_millis(10));
        frames.push(TimedFrame {
            delay: Duration::from_millis(15),
            frame: robot_status_frame(ControlMode::CanControl, expected_move_mode, 100),
        });

        let standby = build_standby_piper(PacedRxAdapter::new(frames), sent_frames.clone());
        let active = standby
            .enable_position_mode(PositionModeConfig {
                timeout: Duration::from_millis(50),
                debounce_threshold: 1,
                poll_interval: Duration::from_millis(1),
                speed_percent: 10,
                install_position: InstallPosition::Invalid,
                motion_type,
                command_timeout: Duration::from_millis(20),
            })
            .expect("matching 0x2A1 should allow Active<PositionMode>");

        assert_eq!(active._state.0.motion_type, motion_type);
        let control = active.driver.get_robot_control();
        assert_eq!(control.control_mode, ControlMode::CanControl as u8);
        assert_eq!(control.move_mode, expected_move_mode as u8);
        assert!(control.is_fully_enabled_confirmed());
        assert!(
            sent_frames
                .lock()
                .expect("sent frames lock")
                .iter()
                .any(|frame| frame.id == piper_protocol::ids::ID_CONTROL_MODE),
            "mode switch command should be sent before confirmation succeeds"
        );
    }

    #[test]
    fn enable_position_mode_cartesian_succeeds_with_matching_robot_status_move_p() {
        assert_enable_position_mode_succeeds_for_motion_type(
            MotionType::Cartesian,
            MoveMode::MoveP,
        );
    }

    #[test]
    fn enable_position_mode_linear_succeeds_with_matching_robot_status_move_l() {
        assert_enable_position_mode_succeeds_for_motion_type(MotionType::Linear, MoveMode::MoveL);
    }

    #[test]
    fn enable_position_mode_circular_succeeds_with_matching_robot_status_move_c() {
        assert_enable_position_mode_succeeds_for_motion_type(MotionType::Circular, MoveMode::MoveC);
    }

    #[test]
    fn enable_position_mode_rejects_mismatched_control_mode_echo() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let mut frames = enabled_joint_frames_after(Duration::from_millis(5));
        frames.push(TimedFrame {
            delay: Duration::from_millis(15),
            frame: control_mode_echo_frame(
                piper_protocol::control::ControlModeCommand::CanControl,
                MoveMode::MoveL,
                40,
                piper_protocol::control::MitMode::PositionVelocity,
                InstallPosition::Invalid,
                101,
            ),
        });
        frames.push(TimedFrame {
            delay: Duration::ZERO,
            frame: robot_status_frame(ControlMode::CanControl, MoveMode::MoveL, 100),
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
    fn enable_timeout_diagnostics_summary_reports_partial_low_speed_state() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let standby = build_standby_piper(
            PacedRxAdapter::new(vec![TimedFrame {
                delay: Duration::ZERO,
                frame: joint_driver_enabled_frame(1, 1_000),
            }]),
            sent_frames,
        );

        wait_until(
            Duration::from_millis(100),
            || standby.observer().joint_enabled_mask() == 0b000001,
            "partial low-speed state should become visible",
        );

        let summary = enable_timeout_diagnostics_summary(&standby.driver, 500);

        assert!(summary.contains("driver_enabled_mask=000001"), "{summary}");
        assert!(summary.contains("confirmed_after_commit=None"), "{summary}");
        assert!(summary.contains("low_speed=partial"), "{summary}");
    }

    #[test]
    fn mode_confirmation_timeout_diagnostics_summary_reports_echo_predicates() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let mut frames = enabled_joint_frames_after(Duration::from_millis(5));
        frames.push(TimedFrame {
            delay: Duration::from_millis(15),
            frame: robot_status_frame(ControlMode::CanControl, MoveMode::MoveJ, 100),
        });

        let standby = build_standby_piper(PacedRxAdapter::new(frames), sent_frames);
        wait_until(
            Duration::from_millis(200),
            || standby.observer().is_all_enabled_confirmed(),
            "enabled low-speed feedback should be confirmed",
        );
        wait_until(
            Duration::from_millis(200),
            || standby.driver.get_robot_control().control_mode == ControlMode::CanControl as u8,
            "robot status frame should update control mode",
        );

        let summary = mode_confirmation_timeout_diagnostics_summary(
            &standby.driver,
            0,
            ModeConfirmationExpectation {
                control_mode: ControlMode::CanControl as u8,
                move_mode: MoveMode::MoveJ as u8,
                speed_percent: 10,
                mit_mode: ProtocolMitMode::PositionVelocity as u8,
                install_position: InstallPosition::Invalid as u8,
                trajectory_stay_time: 0,
            },
        );

        assert!(summary.contains("robot_state_match=true"), "{summary}");
        assert!(summary.contains("mode_echo_fresh=false"), "{summary}");
        assert!(summary.contains("mode_echo_match=false"), "{summary}");
    }

    #[test]
    fn enable_mit_mode_succeeds_with_fresh_matching_robot_and_echo() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let mut frames = enabled_joint_frames_after(Duration::from_millis(10));
        frames.push(TimedFrame {
            delay: Duration::from_millis(20),
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
        assert!(active.observer().is_all_enabled_confirmed());
    }

    #[test]
    fn enable_mit_mode_rejects_stale_historical_enabled_feedback_before_confirmation() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let mut frames = enabled_joint_frames_after(Duration::from_millis(5));
        frames.push(TimedFrame {
            delay: Duration::from_millis(80),
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

        let standby = build_standby_piper_with_config(
            PacedRxAdapter::new(frames),
            sent_frames,
            Some(piper_driver::PipelineConfig {
                low_speed_drive_state_freshness_ms: 20,
                ..piper_driver::PipelineConfig::default()
            }),
        );
        wait_until(
            Duration::from_millis(100),
            || standby.observer().joint_enabled_mask() == 0b11_1111,
            "historical enabled frames should arrive before staleness check",
        );
        thread::sleep(Duration::from_millis(40));

        let error = match standby.enable_mit_mode(MitModeConfig {
            timeout: Duration::from_millis(120),
            debounce_threshold: 1,
            poll_interval: Duration::from_millis(1),
            speed_percent: 80,
        }) {
            Ok(_) => panic!("stale historical enabled bits must not satisfy wait_for_enabled"),
            Err(error) => error,
        };

        assert!(matches!(error, RobotError::Timeout { .. }));
    }

    #[test]
    fn enable_mit_mode_rejects_stale_enabled_state_during_mode_confirmation() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let mut frames = enabled_joint_frames_after(Duration::from_millis(5));
        frames.push(TimedFrame {
            delay: Duration::from_millis(40),
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

        let standby = build_standby_piper_with_config(
            PacedRxAdapter::new(frames),
            sent_frames,
            Some(piper_driver::PipelineConfig {
                low_speed_drive_state_freshness_ms: 20,
                ..piper_driver::PipelineConfig::default()
            }),
        );

        let error = match standby.enable_mit_mode(MitModeConfig {
            timeout: Duration::from_millis(120),
            debounce_threshold: 1,
            poll_interval: Duration::from_millis(1),
            speed_percent: 80,
        }) {
            Ok(_) => panic!(
                "stale enabled bits must not satisfy wait_for_mode_confirmation after wait_for_enabled"
            ),
            Err(error) => error,
        };

        assert!(matches!(error, RobotError::Timeout { .. }));
    }

    #[test]
    fn enable_mit_mode_rejects_matching_mode_feedback_that_arrives_before_commit() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let mut frames = enabled_joint_frames();
        frames.push(TimedFrame {
            delay: Duration::from_millis(5),
            frame: robot_status_frame(ControlMode::CanControl, MoveMode::MoveM, 100),
        });
        frames.push(TimedFrame {
            delay: Duration::ZERO,
            frame: control_mode_echo_frame(
                piper_protocol::control::ControlModeCommand::CanControl,
                MoveMode::MoveM,
                80,
                piper_protocol::control::MitMode::Mit,
                InstallPosition::Invalid,
                101,
            ),
        });

        let standby = build_standby_piper_with_tx(
            PacedRxAdapter::new(frames),
            BlockingFirstControlTxAdapter {
                sent_frames: sent_frames.clone(),
                started_tx,
                release_rx,
                sends: 0,
            },
        );
        let driver = Arc::clone(&standby.driver);
        driver
            .send_reliable(PiperFrame::new_standard(0x221, &[0x01]))
            .expect("blocking reliable frame should enqueue before mode switch");

        let standby_handle = thread::spawn(move || {
            standby.enable_mit_mode(MitModeConfig {
                timeout: Duration::from_millis(120),
                debounce_threshold: 1,
                poll_interval: Duration::from_millis(1),
                speed_percent: 80,
            })
        });

        started_rx
            .recv_timeout(Duration::from_millis(200))
            .expect("mode command should block at tx.send_control before commit");
        thread::sleep(Duration::from_millis(20));
        let _ = release_tx.send(());

        let error = match standby_handle.join().expect("enable_mit_mode thread should finish") {
            Ok(_) => panic!("pre-commit matching feedback must not satisfy mode confirmation"),
            Err(error) => error,
        };
        assert!(matches!(error, RobotError::Timeout { .. }));
    }

    #[test]
    fn enable_mit_mode_rejects_enabled_feedback_that_arrives_before_enable_commit() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();

        let standby = build_standby_piper_with_tx(
            PacedRxAdapter::new(enabled_joint_frames()),
            BlockingFirstControlTxAdapter {
                sent_frames: sent_frames.clone(),
                started_tx,
                release_rx,
                sends: 0,
            },
        );
        let driver = Arc::clone(&standby.driver);
        driver
            .send_reliable(PiperFrame::new_standard(0x221, &[0x02]))
            .expect("blocking reliable frame should enqueue before enable_all");

        let standby_handle = thread::spawn(move || {
            standby.enable_mit_mode(MitModeConfig {
                timeout: Duration::from_millis(120),
                debounce_threshold: 1,
                poll_interval: Duration::from_millis(1),
                speed_percent: 80,
            })
        });

        started_rx
            .recv_timeout(Duration::from_millis(200))
            .expect("blocking reliable frame should reach the adapter first");
        wait_until(
            Duration::from_millis(200),
            || driver.get_robot_control().confirmed_driver_enabled_mask == Some(0b11_1111),
            "enabled low-speed feedback should land before enable_all reaches TX commit",
        );

        release_tx.send(()).expect("blocked reliable frame should release");
        let error = match standby_handle.join().expect("enable_mit_mode thread should finish") {
            Ok(_) => panic!("pre-commit enabled feedback must not satisfy wait_for_enabled"),
            Err(error) => error,
        };

        assert!(matches!(error, RobotError::Timeout { .. }));
        assert!(
            !sent_frames
                .lock()
                .expect("sent frames lock")
                .iter()
                .any(|frame| frame.id == piper_protocol::ids::ID_CONTROL_MODE),
            "mode switch must not start when enable confirmation only existed before commit",
        );
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
        let mut frames = enabled_joint_frames_after(Duration::from_millis(5));
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
    fn disable_rejects_unknown_drive_state_without_confirmed_feedback() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let active = build_active_mit_piper(
            DeviceQuirks::from_firmware_version(Version::new(1, 8, 3)),
            sent_frames,
        );

        let error = match active.disable(DisableConfig {
            timeout: Duration::from_millis(30),
            debounce_threshold: 1,
            poll_interval: Duration::from_millis(1),
        }) {
            Ok(_) => panic!("unknown low-speed disable state must not satisfy wait_for_disabled"),
            Err(error) => error,
        };

        assert!(matches!(error, RobotError::Timeout { .. }));
    }

    #[test]
    fn disable_succeeds_with_fresh_confirmed_disabled_feedback() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let frames = disabled_joint_frames_after(Duration::from_millis(10), 1);
        let driver = Arc::new(
            RobotPiper::new_dual_thread_parts(
                PacedRxAdapter::new(frames),
                RecordingTxAdapter::new(sent_frames),
                None,
            )
            .expect("driver should start"),
        );
        let active = build_active_mit_piper_with_driver(
            driver,
            DeviceQuirks::from_firmware_version(Version::new(1, 8, 3)),
        );

        active
            .disable(DisableConfig {
                timeout: Duration::from_millis(80),
                debounce_threshold: 1,
                poll_interval: Duration::from_millis(1),
            })
            .expect("fresh confirmed disabled feedback should allow disable transition");
    }

    #[test]
    fn active_request_disable_all_transitions_via_maintenance_to_standby() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let frames = disabled_joint_frames_after(Duration::from_millis(10), 1);
        let driver = Arc::new(
            RobotPiper::new_dual_thread_parts(
                PacedRxAdapter::new(frames),
                RecordingTxAdapter::new(sent_frames.clone()),
                None,
            )
            .expect("driver should start"),
        );
        let active = build_active_mit_piper_with_driver(
            driver,
            DeviceQuirks::from_firmware_version(Version::new(1, 8, 3)),
        );

        let maintenance =
            active.request_disable_all().expect("disable request should enter maintenance");
        let standby = maintenance
            .wait_until_disabled(DisableConfig {
                timeout: Duration::from_millis(80),
                debounce_threshold: 1,
                poll_interval: Duration::from_millis(1),
            })
            .expect("fresh disabled feedback should converge to standby");

        assert!(matches!(standby._state, Standby));
        let sent = sent_frames.lock().expect("sent frames lock");
        assert_eq!(
            sent.first().copied(),
            Some(piper_protocol::control::MotorEnableCommand::disable_all().to_frame())
        );
    }

    #[test]
    fn active_request_disable_all_keeps_shared_driver_control_closed_until_disabled_feedback() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let frames = disabled_joint_frames_after(Duration::from_millis(10), 1);
        let driver = Arc::new(
            RobotPiper::new_dual_thread_parts(
                PacedRxAdapter::new(frames),
                RecordingTxAdapter::new(sent_frames),
                None,
            )
            .expect("driver should start"),
        );
        let active = build_active_mit_piper_with_driver(
            driver.clone(),
            DeviceQuirks::from_firmware_version(Version::new(1, 8, 3)),
        );

        let maintenance =
            active.request_disable_all().expect("disable request should enter maintenance");

        assert!(matches!(
            maintenance.driver.send_reliable(PiperFrame::new_standard(0x151, &[0x09])),
            Err(piper_driver::DriverError::ControlPathClosed)
        ));

        maintenance
            .wait_until_disabled(DisableConfig {
                timeout: Duration::from_millis(80),
                debounce_threshold: 1,
                poll_interval: Duration::from_millis(1),
            })
            .expect("fresh disabled feedback should eventually reopen the control path");

        driver
            .send_reliable(PiperFrame::new_standard(0x151, &[0x0A]))
            .expect("shared driver should reopen once disabled feedback is confirmed");
    }

    #[test]
    fn active_request_disable_all_reopens_driver_when_disabled_feedback_arrives_before_send_returns()
     {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let frames = disabled_joint_frames_after(Duration::from_millis(20), 100);
        let driver = Arc::new(
            RobotPiper::new_dual_thread_parts(
                PacedRxAdapter::new(frames),
                BlockingFirstControlTxAdapter {
                    sent_frames: sent_frames.clone(),
                    started_tx,
                    release_rx,
                    sends: 0,
                },
                None,
            )
            .expect("driver should start"),
        );
        let active = build_active_mit_piper_with_driver(
            driver.clone(),
            DeviceQuirks::from_firmware_version(Version::new(1, 8, 3)),
        );

        let disable_handle = thread::spawn(move || active.request_disable_all());

        started_rx
            .recv_timeout(Duration::from_millis(200))
            .expect("disable frame should enter the adapter before the release gate opens");
        wait_until(
            Duration::from_millis(200),
            || driver.get_robot_control().confirmed_driver_enabled_mask == Some(0),
            "disabled low-speed feedback should land before the control send is released",
        );

        release_tx.send(()).expect("blocked control send should release cleanly");
        let maintenance = disable_handle
            .join()
            .expect("disable request thread should finish")
            .expect("disable request should still enter maintenance");

        let standby = maintenance
            .wait_until_disabled(DisableConfig {
                timeout: Duration::from_millis(80),
                debounce_threshold: 1,
                poll_interval: Duration::from_millis(1),
            })
            .expect("already-confirmed disabled feedback should still converge to standby");

        assert!(matches!(standby._state, Standby));
        driver
            .send_reliable(PiperFrame::new_standard(0x151, &[0x0B]))
            .expect(
                "driver should not remain stuck in StateTransitionClosed after request_disable_all returns against already-confirmed disabled feedback",
            );
    }

    #[test]
    fn maintenance_partial_joint_power_commands_send_expected_frames() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let standby = build_standby_piper(IdleRxAdapter::new(), sent_frames.clone());

        let maintenance = standby
            .into_maintenance()
            .enable_joint(Joint::J2)
            .expect("maintenance enable should succeed");
        let _maintenance = maintenance
            .disable_joint(Joint::J2)
            .expect("maintenance disable should succeed");

        wait_until(
            Duration::from_millis(200),
            || sent_frames.lock().expect("sent frames lock").len() >= 2,
            "maintenance partial power commands should be sent",
        );

        let sent = sent_frames.lock().expect("sent frames lock").clone();
        assert_eq!(
            sent,
            vec![
                piper_protocol::control::MotorEnableCommand::enable(2).to_frame(),
                piper_protocol::control::MotorEnableCommand::disable(2).to_frame(),
            ]
        );
    }

    #[test]
    fn maintenance_wait_until_disabled_fails_fast_when_runtime_fault_latches() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let active = build_active_mit_piper(
            DeviceQuirks::from_firmware_version(Version::new(1, 8, 3)),
            sent_frames,
        );
        let maintenance =
            active.request_disable_all().expect("disable request should enter maintenance");
        let driver = Arc::clone(&maintenance.driver);
        let latch_handle = thread::spawn(move || {
            thread::sleep(Duration::from_millis(5));
            driver.latch_fault();
        });

        let error = match maintenance.wait_until_disabled(DisableConfig {
            timeout: Duration::from_millis(200),
            debounce_threshold: 1,
            poll_interval: Duration::from_millis(1),
        }) {
            Ok(_) => panic!("runtime fault should fail fast during disable wait"),
            Err(error) => error,
        };
        latch_handle.join().expect("fault latch helper should finish cleanly");

        assert!(matches!(
            error,
            RobotError::RuntimeHealthUnhealthy {
                fault: Some(RuntimeFaultKind::ManualFault),
                ..
            }
        ));
    }

    #[test]
    fn shutdown_only_sends_disable_all_for_active_mit() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let driver = Arc::new(
            RobotPiper::new_dual_thread_parts(
                PacedRxAdapter::new(disabled_joint_frames_after(Duration::from_millis(10), 10)),
                RecordingTxAdapter::new(sent_frames.clone()),
                None,
            )
            .expect("driver should start"),
        );
        let active = build_active_mit_piper_with_driver(
            driver,
            DeviceQuirks::from_firmware_version(Version::new(1, 8, 3)),
        );

        active
            .shutdown()
            .expect("shutdown should succeed once disabled feedback is confirmed");

        let sent = sent_frames.lock().expect("sent frames lock").clone();
        assert_eq!(
            sent,
            vec![piper_protocol::control::MotorEnableCommand::disable_all().to_frame()]
        );
        assert!(
            !sent.contains(
                &piper_protocol::control::EmergencyStopCommand::emergency_stop().to_frame()
            ),
            "normal shutdown must not emit emergency-stop semantics",
        );
    }

    #[test]
    fn shutdown_only_sends_disable_all_for_active_position() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let driver = Arc::new(
            RobotPiper::new_dual_thread_parts(
                PacedRxAdapter::new(disabled_joint_frames_after(Duration::from_millis(10), 40)),
                RecordingTxAdapter::new(sent_frames.clone()),
                None,
            )
            .expect("driver should start"),
        );
        let active = build_active_position_piper(driver);

        active
            .shutdown()
            .expect("position-mode shutdown should succeed once disabled feedback is confirmed");

        let sent = sent_frames.lock().expect("sent frames lock").clone();
        assert_eq!(
            sent,
            vec![piper_protocol::control::MotorEnableCommand::disable_all().to_frame()]
        );
        assert!(
            !sent.contains(
                &piper_protocol::control::EmergencyStopCommand::emergency_stop().to_frame()
            ),
            "normal shutdown must not emit emergency-stop semantics",
        );
    }

    #[test]
    fn shutdown_timeout_latches_transport_fault_and_closes_control_path() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let driver = Arc::new(
            RobotPiper::new_dual_thread_parts(
                IdleRxAdapter::new(),
                TimeoutControlTxAdapter {
                    sent_frames: sent_frames.clone(),
                },
                None,
            )
            .expect("driver should start"),
        );
        let active = build_active_mit_piper_with_driver(
            driver.clone(),
            DeviceQuirks::from_firmware_version(Version::new(1, 8, 3)),
        );

        let error = match active.shutdown() {
            Ok(_) => panic!("state-transition disable timeout must fail closed"),
            Err(error) => error,
        };

        assert!(matches!(
            error,
            RobotError::Infrastructure(piper_driver::DriverError::Timeout)
        ));
        assert_eq!(
            driver.health().fault,
            Some(RuntimeFaultKind::TransportError),
            "shutdown timeout must latch a transport fault before returning control",
        );
        assert!(matches!(
            driver.send_reliable(PiperFrame::new_standard(0x151, &[0x08])),
            Err(piper_driver::DriverError::ControlPathClosed)
        ));
        assert_eq!(
            driver.maintenance_lease_snapshot().state(),
            piper_driver::MaintenanceGateState::DeniedFaulted
        );
    }

    #[test]
    fn request_disable_all_timeout_latches_transport_fault_and_closes_control_path() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let driver = Arc::new(
            RobotPiper::new_dual_thread_parts(
                IdleRxAdapter::new(),
                TimeoutControlTxAdapter {
                    sent_frames: sent_frames.clone(),
                },
                None,
            )
            .expect("driver should start"),
        );
        let active = build_active_mit_piper_with_driver(
            driver.clone(),
            DeviceQuirks::from_firmware_version(Version::new(1, 8, 3)),
        );

        let error = match active.request_disable_all() {
            Ok(_) => panic!("disable request timeout must fail closed"),
            Err(error) => error,
        };

        assert!(matches!(
            error,
            RobotError::Infrastructure(piper_driver::DriverError::Timeout)
        ));
        assert_eq!(
            driver.health().fault,
            Some(RuntimeFaultKind::TransportError),
            "disable request timeout must latch a transport fault before returning control",
        );
        assert!(matches!(
            driver.send_reliable(PiperFrame::new_standard(0x151, &[0x09])),
            Err(piper_driver::DriverError::ControlPathClosed)
        ));
        assert_eq!(
            driver.maintenance_lease_snapshot().state(),
            piper_driver::MaintenanceGateState::DeniedFaulted
        );
    }

    #[test]
    fn mit_controller_move_to_rest_requires_explicit_rest_position() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let driver = Arc::new(
            RobotPiper::new_dual_thread_parts(
                PacedRxAdapter::new(control_snapshot_frames(1_000)),
                RecordingTxAdapter::new(sent_frames.clone()),
                None,
            )
            .expect("driver should start"),
        );
        let active = build_active_mit_piper_with_driver(
            driver,
            DeviceQuirks::from_firmware_version(Version::new(1, 8, 3)),
        );
        let mut controller = MitController::new(active, MitControllerConfig::default())
            .expect("strict realtime driver should support MitController");

        let error = controller
            .move_to_rest(Rad(0.01), Duration::from_millis(50))
            .expect_err("move_to_rest must fail closed when rest_position is absent");

        assert!(matches!(error, ControlError::RestPositionNotConfigured));
        assert!(
            sent_frames.lock().expect("sent frames lock").is_empty(),
            "missing rest_position must not emit any motion command",
        );
    }

    #[test]
    fn mit_controller_drop_with_rest_position_only_sends_disable_all() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let driver = Arc::new(
            RobotPiper::new_dual_thread_parts(
                PacedRxAdapter::new(control_snapshot_frames(1_000)),
                RecordingTxAdapter::new(sent_frames.clone()),
                None,
            )
            .expect("driver should start"),
        );
        let active = build_active_mit_piper_with_driver(
            driver,
            DeviceQuirks::from_firmware_version(Version::new(1, 8, 3)),
        );
        let controller = MitController::new(
            active,
            MitControllerConfig {
                rest_position: Some([Rad(0.0), Rad(0.1), Rad(-0.2), Rad(0.3), Rad(0.4), Rad(0.5)]),
                ..MitControllerConfig::default()
            },
        )
        .expect("strict realtime driver should support MitController");

        drop(controller);

        wait_until(
            Duration::from_millis(200),
            || !sent_frames.lock().expect("sent frames lock").is_empty(),
            "dropping MitController should still emit the bounded disable safety net",
        );

        let sent = sent_frames.lock().expect("sent frames lock").clone();
        assert_eq!(
            sent,
            vec![piper_protocol::control::MotorEnableCommand::disable_all().to_frame()]
        );
    }

    #[test]
    fn recover_from_emergency_stop_returns_standby_after_resume() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let mut low_speed_frames =
            delayed_joint_state_frames(false, Duration::ZERO, Duration::ZERO, 100);
        low_speed_frames.extend(delayed_joint_state_frames(
            false,
            Duration::from_millis(50),
            Duration::from_millis(1),
            200,
        ));
        let driver = Arc::new(
            RobotPiper::new_dual_thread_parts(
                PacedRxAdapter::new(low_speed_frames),
                RecordingTxAdapter::new(sent_frames.clone()),
                None,
            )
            .expect("driver should start"),
        );
        let active = build_active_mit_piper_with_driver(
            driver.clone(),
            DeviceQuirks::from_firmware_version(Version::new(1, 8, 3)),
        );
        wait_until(
            Duration::from_millis(200),
            || latest_joint_6_low_speed_hardware_timestamp(&driver) == Some(106),
            "pre-resume low-speed feedback should establish the disabled hardware baseline",
        );

        let error = active.emergency_stop().expect("manual emergency stop should enter ErrorState");
        let recovered = error
            .recover_from_emergency_stop(Duration::from_millis(200))
            .expect("manual emergency stop should be recoverable");

        match &recovered {
            MotionConnectedState::Standby(standby) => {
                assert!(standby.runtime_health().fault.is_none());
                assert!(standby.observer().is_all_disabled_confirmed());
            },
            MotionConnectedState::Maintenance(_) => {
                panic!("confirmed disabled feedback should recover into Standby")
            },
        }

        let sent = sent_frames.lock().expect("sent frames lock").clone();
        assert_eq!(
            sent,
            vec![
                piper_protocol::control::EmergencyStopCommand::emergency_stop().to_frame(),
                piper_protocol::control::EmergencyStopCommand::resume().to_frame(),
            ]
        );
    }

    #[test]
    fn recover_from_emergency_stop_times_out_without_fresh_post_resume_drive_state() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let active = build_active_mit_piper(
            DeviceQuirks::from_firmware_version(Version::new(1, 8, 3)),
            sent_frames.clone(),
        );

        let error = active.emergency_stop().expect("manual emergency stop should enter ErrorState");
        let driver = Arc::clone(&error.driver);
        let recover_error = match error.recover_from_emergency_stop(Duration::from_millis(20)) {
            Ok(_) => {
                panic!("resume must fail closed without fresh post-resume drive confirmation")
            },
            Err(error) => error,
        };

        assert!(matches!(recover_error, RobotError::Timeout { .. }));
        assert_eq!(driver.health().fault, Some(RuntimeFaultKind::ManualFault));
        assert_eq!(
            driver.maintenance_lease_snapshot().state(),
            piper_driver::MaintenanceGateState::DeniedFaulted
        );
        assert!(matches!(
            driver.send_reliable(PiperFrame::new_standard(0x472, &[0x02])),
            Err(piper_driver::DriverError::ControlPathClosed)
        ));

        let sent = sent_frames.lock().expect("sent frames lock").clone();
        assert_eq!(
            sent,
            vec![
                piper_protocol::control::EmergencyStopCommand::emergency_stop().to_frame(),
                piper_protocol::control::EmergencyStopCommand::resume().to_frame(),
            ]
        );
    }

    #[test]
    fn recover_from_emergency_stop_treats_timeout_as_total_budget() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let driver = Arc::new(
            RobotPiper::new_dual_thread_parts(
                IdleRxAdapter::new(),
                DelayOnNthShutdownTxAdapter {
                    sent_frames: sent_frames.clone(),
                    shutdown_sends: 0,
                    delay_on: 2,
                    delay: Duration::from_millis(15),
                },
                None,
            )
            .expect("driver should start"),
        );
        let active = build_active_mit_piper_with_driver(
            driver,
            DeviceQuirks::from_firmware_version(Version::new(1, 8, 3)),
        );

        let error = active.emergency_stop().expect("manual emergency stop should enter ErrorState");
        let driver = Arc::clone(&error.driver);
        let recover_error = match error.recover_from_emergency_stop(Duration::from_millis(5)) {
            Ok(_) => panic!("resume ack beyond the caller budget must time out"),
            Err(error) => error,
        };

        assert!(matches!(
            recover_error,
            RobotError::Timeout { timeout_ms: 5 }
        ));
        assert_eq!(driver.health().fault, Some(RuntimeFaultKind::ManualFault));
        assert_eq!(
            driver.maintenance_lease_snapshot().state(),
            piper_driver::MaintenanceGateState::DeniedFaulted
        );

        let sent = sent_frames.lock().expect("sent frames lock").clone();
        assert_eq!(
            sent,
            vec![piper_protocol::control::EmergencyStopCommand::emergency_stop().to_frame()]
        );
    }

    #[test]
    fn recover_from_emergency_stop_deadline_expiry_wins_over_ready_feedback_after_resume() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let driver = Arc::new(
            RobotPiper::new_dual_thread_parts(
                PacedRxAdapter::new(vec![
                    TimedFrame {
                        delay: Duration::from_millis(20),
                        frame: joint_driver_disabled_frame(1, 1_000),
                    },
                    TimedFrame {
                        delay: Duration::ZERO,
                        frame: joint_driver_disabled_frame(2, 1_001),
                    },
                    TimedFrame {
                        delay: Duration::ZERO,
                        frame: joint_driver_disabled_frame(3, 1_002),
                    },
                    TimedFrame {
                        delay: Duration::ZERO,
                        frame: joint_driver_disabled_frame(4, 1_003),
                    },
                    TimedFrame {
                        delay: Duration::ZERO,
                        frame: joint_driver_disabled_frame(5, 1_004),
                    },
                    TimedFrame {
                        delay: Duration::ZERO,
                        frame: joint_driver_disabled_frame(6, 1_005),
                    },
                ]),
                DelayOnNthShutdownTxAdapter {
                    sent_frames: sent_frames.clone(),
                    shutdown_sends: 0,
                    delay_on: 2,
                    delay: Duration::ZERO,
                },
                None,
            )
            .expect("driver should start"),
        );
        let active = build_active_mit_piper_with_driver(
            driver,
            DeviceQuirks::from_firmware_version(Version::new(1, 8, 3)),
        );

        let error = active.emergency_stop().expect("manual emergency stop should enter ErrorState");
        let driver = Arc::clone(&error.driver);
        let recover_error = match error.recover_from_emergency_stop(Duration::from_millis(20)) {
            Ok(_) => panic!("expired total budget must win over ready feedback"),
            Err(error) => error,
        };

        assert!(matches!(
            recover_error,
            RobotError::Timeout { timeout_ms: 20 }
        ));
        assert_eq!(driver.health().fault, Some(RuntimeFaultKind::ManualFault));
        assert_eq!(
            driver.maintenance_lease_snapshot().state(),
            piper_driver::MaintenanceGateState::DeniedFaulted
        );

        let sent = sent_frames.lock().expect("sent frames lock").clone();
        assert_eq!(
            sent,
            vec![
                piper_protocol::control::EmergencyStopCommand::emergency_stop().to_frame(),
                piper_protocol::control::EmergencyStopCommand::resume().to_frame(),
            ]
        );
    }

    #[test]
    fn recover_from_emergency_stop_rejects_when_runtime_is_no_longer_healthy() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let active = build_active_mit_piper(
            DeviceQuirks::from_firmware_version(Version::new(1, 8, 3)),
            sent_frames,
        );
        let error = active.emergency_stop().expect("manual emergency stop should enter ErrorState");
        let driver = Arc::clone(&error.driver);
        driver.request_stop();
        wait_until(
            Duration::from_millis(200),
            || !driver.health().tx_alive,
            "request_stop should eventually stop the TX worker",
        );

        let recover_error = match error.recover_from_emergency_stop(Duration::from_millis(20)) {
            Ok(_) => panic!("resume must fail once the runtime is no longer healthy"),
            Err(error) => error,
        };

        assert!(matches!(
            recover_error,
            RobotError::RuntimeHealthUnhealthy {
                tx_alive: false,
                ..
            }
        ));
    }

    #[test]
    fn recover_from_emergency_stop_returns_maintenance_after_resume_with_fresh_enabled_feedback() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let mut low_speed_frames =
            delayed_joint_state_frames(true, Duration::ZERO, Duration::ZERO, 200);
        low_speed_frames.extend(delayed_joint_state_frames(
            true,
            Duration::from_millis(50),
            Duration::from_millis(1),
            300,
        ));
        let driver = Arc::new(
            RobotPiper::new_dual_thread_parts(
                PacedRxAdapter::new(low_speed_frames),
                RecordingTxAdapter::new(sent_frames.clone()),
                None,
            )
            .expect("driver should start"),
        );
        let active = build_active_mit_piper_with_driver(
            driver.clone(),
            DeviceQuirks::from_firmware_version(Version::new(1, 8, 3)),
        );
        wait_until(
            Duration::from_millis(200),
            || latest_joint_6_low_speed_hardware_timestamp(&driver) == Some(206),
            "pre-resume low-speed feedback should establish the enabled hardware baseline",
        );

        let error = active.emergency_stop().expect("manual emergency stop should enter ErrorState");
        let sent = match error
            .recover_from_emergency_stop(Duration::from_millis(200))
            .expect("fresh enabled feedback should recover into Maintenance")
        {
            MotionConnectedState::Maintenance(maintenance) => {
                assert!(maintenance.runtime_health().fault.is_none());
                assert_eq!(
                    maintenance.observer().joint_enabled_mask_confirmed(),
                    Some(0b11_1111)
                );
                sent_frames.lock().expect("sent frames lock").clone()
            },
            MotionConnectedState::Standby(_) => {
                panic!("fresh enabled feedback should not recover into Standby")
            },
        };

        assert_eq!(
            sent,
            vec![
                piper_protocol::control::EmergencyStopCommand::emergency_stop().to_frame(),
                piper_protocol::control::EmergencyStopCommand::resume().to_frame(),
            ]
        );
    }

    #[test]
    fn recover_from_emergency_stop_times_out_on_host_late_stale_post_resume_feedback() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let stale_frames = vec![
            TimedFrame {
                delay: Duration::ZERO,
                frame: joint_driver_disabled_frame(1, 1_000),
            },
            TimedFrame {
                delay: Duration::ZERO,
                frame: joint_driver_disabled_frame(2, 1_001),
            },
            TimedFrame {
                delay: Duration::ZERO,
                frame: joint_driver_disabled_frame(3, 1_002),
            },
            TimedFrame {
                delay: Duration::ZERO,
                frame: joint_driver_disabled_frame(4, 1_003),
            },
            TimedFrame {
                delay: Duration::ZERO,
                frame: joint_driver_disabled_frame(5, 1_004),
            },
            TimedFrame {
                delay: Duration::ZERO,
                frame: joint_driver_disabled_frame(6, 1_005),
            },
            TimedFrame {
                delay: Duration::from_millis(15),
                frame: joint_driver_disabled_frame(1, 1_000),
            },
            TimedFrame {
                delay: Duration::ZERO,
                frame: joint_driver_disabled_frame(2, 1_001),
            },
            TimedFrame {
                delay: Duration::ZERO,
                frame: joint_driver_disabled_frame(3, 1_002),
            },
            TimedFrame {
                delay: Duration::ZERO,
                frame: joint_driver_disabled_frame(4, 1_003),
            },
            TimedFrame {
                delay: Duration::ZERO,
                frame: joint_driver_disabled_frame(5, 1_004),
            },
            TimedFrame {
                delay: Duration::ZERO,
                frame: joint_driver_disabled_frame(6, 1_005),
            },
        ];
        let driver = Arc::new(
            RobotPiper::new_dual_thread_parts(
                PacedRxAdapter::new(stale_frames),
                RecordingTxAdapter::new(sent_frames.clone()),
                None,
            )
            .expect("driver should start"),
        );
        let active = build_active_mit_piper_with_driver(
            driver.clone(),
            DeviceQuirks::from_firmware_version(Version::new(1, 8, 3)),
        );

        let error = active.emergency_stop().expect("manual emergency stop should enter ErrorState");
        let driver = Arc::clone(&error.driver);
        wait_until(
            Duration::from_millis(200),
            || latest_joint_6_low_speed_hardware_timestamp(&driver) == Some(1_005),
            "pre-resume low-speed feedback should establish the hardware timestamp baseline",
        );

        let recover_error = match error.recover_from_emergency_stop(Duration::from_millis(40)) {
            Ok(_) => panic!("host-late stale post-resume feedback must not reopen control"),
            Err(error) => error,
        };

        assert!(matches!(recover_error, RobotError::Timeout { .. }));
        assert_eq!(driver.health().fault, Some(RuntimeFaultKind::ManualFault));
        assert_eq!(
            driver.maintenance_lease_snapshot().state(),
            piper_driver::MaintenanceGateState::DeniedFaulted
        );
    }

    #[test]
    fn recover_from_emergency_stop_times_out_without_pre_resume_low_speed_baseline() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let post_resume_frames = vec![
            TimedFrame {
                delay: Duration::from_millis(15),
                frame: joint_driver_disabled_frame(1, 1_000),
            },
            TimedFrame {
                delay: Duration::ZERO,
                frame: joint_driver_disabled_frame(2, 1_001),
            },
            TimedFrame {
                delay: Duration::ZERO,
                frame: joint_driver_disabled_frame(3, 1_002),
            },
            TimedFrame {
                delay: Duration::ZERO,
                frame: joint_driver_disabled_frame(4, 1_003),
            },
            TimedFrame {
                delay: Duration::ZERO,
                frame: joint_driver_disabled_frame(5, 1_004),
            },
            TimedFrame {
                delay: Duration::ZERO,
                frame: joint_driver_disabled_frame(6, 1_005),
            },
        ];
        let driver = Arc::new(
            RobotPiper::new_dual_thread_parts(
                PacedRxAdapter::new(post_resume_frames),
                RecordingTxAdapter::new(sent_frames.clone()),
                None,
            )
            .expect("driver should start"),
        );
        let active = build_active_mit_piper_with_driver(
            driver.clone(),
            DeviceQuirks::from_firmware_version(Version::new(1, 8, 3)),
        );

        let error = active.emergency_stop().expect("manual emergency stop should enter ErrorState");
        let driver = Arc::clone(&error.driver);

        let recover_error = match error.recover_from_emergency_stop(Duration::from_millis(40)) {
            Ok(_) => panic!(
                "recovery must fail closed when no complete pre-resume low-speed baseline exists"
            ),
            Err(error) => error,
        };

        assert!(matches!(recover_error, RobotError::Timeout { .. }));
        assert_eq!(driver.health().fault, Some(RuntimeFaultKind::ManualFault));
        assert_eq!(
            driver.maintenance_lease_snapshot().state(),
            piper_driver::MaintenanceGateState::DeniedFaulted
        );
    }

    #[test]
    fn enter_replay_mode_is_rejected_while_driver_disable_is_in_flight() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let standby = build_standby_piper_with_tx(
            IdleRxAdapter::new(),
            BlockingFirstControlTxAdapter {
                sent_frames: sent_frames.clone(),
                started_tx,
                release_rx,
                sends: 0,
            },
        );
        let driver = Arc::clone(&standby.driver);
        let disable_frame = piper_protocol::control::MotorEnableCommand::disable_all().to_frame();

        let driver_for_disable = Arc::clone(&driver);
        let disable_handle = thread::spawn(move || {
            driver_for_disable.send_local_state_transition_frame_confirmed(
                disable_frame,
                Duration::from_millis(200),
            )
        });

        started_rx
            .recv_timeout(Duration::from_millis(200))
            .expect("disable frame should enter the adapter while state transition is in flight");

        let replay_error = match standby.enter_replay_mode() {
            Ok(_) => panic!("Replay must fail while a safety disable is in flight"),
            Err(error) => error,
        };
        assert!(matches!(
            replay_error,
            RobotError::Infrastructure(piper_driver::DriverError::ControlPathClosed)
        ));

        release_tx.send(()).expect("blocked control send should release cleanly");
        disable_handle
            .join()
            .expect("disable sender thread should finish")
            .expect("disable frame must still complete after Replay is rejected");

        assert_eq!(driver.mode(), DriverMode::Normal);
        let sent = sent_frames.lock().expect("sent frames lock").clone();
        assert_eq!(
            sent,
            vec![piper_protocol::control::MotorEnableCommand::disable_all().to_frame()]
        );
    }

    #[test]
    fn set_joint_zero_positions_waits_for_confirmed_setting_response() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let standby = build_standby_piper(
            PacedRxAdapter::new(vec![TimedFrame {
                delay: Duration::from_millis(5),
                frame: setting_response_frame(0x75, true, 10),
            }]),
            sent_frames.clone(),
        );

        standby
            .set_joint_zero_positions(&[0])
            .expect("single-joint zeroing should wait for 0x476 confirmation");

        wait_until(
            Duration::from_millis(200),
            || !sent_frames.lock().expect("sent frames lock").is_empty(),
            "zero-point request frame should be sent",
        );
        let sent = sent_frames.lock().expect("sent frames lock").clone();
        assert_eq!(
            sent[0],
            piper_protocol::config::JointSettingCommand::set_zero_point(1).to_frame()
        );
    }

    #[test]
    fn set_joint_zero_positions_supports_all_joint_broadcast_confirmation() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let standby = build_standby_piper(
            PacedRxAdapter::new(vec![TimedFrame {
                delay: Duration::from_millis(5),
                frame: setting_response_frame(0x75, true, 10),
            }]),
            sent_frames.clone(),
        );

        standby
            .set_joint_zero_positions(&[0, 1, 2, 3, 4, 5])
            .expect("all-joint zeroing should use broadcast confirmation");

        wait_until(
            Duration::from_millis(200),
            || !sent_frames.lock().expect("sent frames lock").is_empty(),
            "broadcast zero-point request frame should be sent",
        );
        let sent = sent_frames.lock().expect("sent frames lock").clone();
        assert_eq!(
            sent[0],
            piper_protocol::config::JointSettingCommand::set_zero_point(7).to_frame()
        );
    }

    #[test]
    fn set_joint_zero_positions_fails_fast_when_runtime_fault_latches() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let standby = build_standby_piper(IdleRxAdapter::new(), sent_frames);
        let driver = Arc::clone(&standby.driver);
        let latch_handle = thread::spawn(move || {
            thread::sleep(Duration::from_millis(5));
            driver.latch_fault();
        });

        let error = standby
            .set_joint_zero_positions(&[0])
            .expect_err("runtime fault should fail zero-point confirmation wait fast");
        latch_handle.join().expect("fault latch helper should finish cleanly");

        assert!(matches!(
            error,
            RobotError::RuntimeHealthUnhealthy {
                fault: Some(RuntimeFaultKind::ManualFault),
                ..
            }
        ));
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
    fn standby_stop_recording_removes_registered_hook() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let standby = build_standby_piper(IdleRxAdapter::new(), sent_frames);
        let output_path = temp_recording_path("recording-hook-remove");

        let hooks_len_before = standby.driver.hooks().read().expect("hooks read lock").len();
        assert_eq!(hooks_len_before, 0);

        let (standby, handle) = standby
            .start_recording(crate::recording::RecordingConfig {
                output_path: output_path.clone(),
                stop_condition: crate::recording::StopCondition::Manual,
                metadata: crate::recording::RecordingMetadata {
                    notes: "test".to_string(),
                    operator: "tester".to_string(),
                },
            })
            .expect("recording should start");

        let hooks_len_during = standby.driver.hooks().read().expect("hooks read lock").len();
        assert_eq!(hooks_len_during, 1);

        let driver = Arc::clone(&standby.driver);

        let (_standby, _stats) =
            standby.stop_recording(handle).expect("recording should stop cleanly");

        let hooks_len_after = driver.hooks().read().expect("hooks read lock").len();
        assert_eq!(hooks_len_after, 0);

        let _ = std::fs::remove_file(output_path);
    }

    #[test]
    fn dropping_recording_handle_removes_registered_hook() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let standby = build_standby_piper(IdleRxAdapter::new(), sent_frames);
        let output_path = temp_recording_path("recording-handle-drop");

        let hooks_len_before = standby.driver.hooks().read().expect("hooks read lock").len();
        assert_eq!(hooks_len_before, 0);

        let driver = Arc::clone(&standby.driver);
        let (_standby, handle) = standby
            .start_recording(crate::recording::RecordingConfig {
                output_path: output_path.clone(),
                stop_condition: crate::recording::StopCondition::Manual,
                metadata: crate::recording::RecordingMetadata {
                    notes: "test".to_string(),
                    operator: "tester".to_string(),
                },
            })
            .expect("recording should start");

        assert_eq!(
            driver.hooks().read().expect("hooks read lock").len(),
            1,
            "recording hook should be registered while handle is alive"
        );

        drop(handle);

        assert_eq!(
            driver.hooks().read().expect("hooks read lock").len(),
            0,
            "dropping the handle must automatically unregister the hook"
        );

        let _ = std::fs::remove_file(output_path);
    }

    #[test]
    fn manual_recording_stop_prevents_later_frames_from_being_recorded() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let standby = build_standby_piper(IdleRxAdapter::new(), sent_frames);
        let driver = Arc::clone(&standby.driver);
        let output_path = temp_recording_path("recording-manual-stop");

        let (standby, handle) = standby
            .start_recording(crate::recording::RecordingConfig {
                output_path: output_path.clone(),
                stop_condition: crate::recording::StopCondition::Manual,
                metadata: crate::recording::RecordingMetadata {
                    notes: "test".to_string(),
                    operator: "tester".to_string(),
                },
            })
            .expect("recording should start");

        handle.stop();
        driver
            .send_reliable(PiperFrame::new_standard(0x151, &[0x44]))
            .expect("driver should send a frame after manual stop");
        thread::sleep(Duration::from_millis(20));

        let (_standby, stats) = standby
            .stop_recording(handle)
            .expect("recording should stop cleanly after manual stop");
        assert_eq!(
            stats.frame_count, 0,
            "frames sent after manual stop must not be recorded"
        );

        let _ = std::fs::remove_file(output_path);
    }

    #[test]
    fn stop_recording_persists_user_metadata_to_recording_file() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let standby = build_standby_piper(IdleRxAdapter::new(), sent_frames);
        let driver = Arc::clone(&standby.driver);
        let output_path = temp_recording_path("recording-metadata");

        let (standby, handle) = standby
            .start_recording(crate::recording::RecordingConfig {
                output_path: output_path.clone(),
                stop_condition: crate::recording::StopCondition::Manual,
                metadata: crate::recording::RecordingMetadata {
                    notes: "metadata note".to_string(),
                    operator: "metadata operator".to_string(),
                },
            })
            .expect("recording should start");

        driver
            .send_reliable(PiperFrame::new_standard(0x151, &[0x66]))
            .expect("driver should emit a frame for recording metadata test");
        wait_until(
            Duration::from_millis(200),
            || handle.frame_count() >= 1,
            "recording should capture the test frame before saving",
        );

        let (_standby, stats) =
            standby.stop_recording(handle).expect("recording should stop cleanly");
        assert_eq!(stats.frame_count, 1);

        let saved = PiperRecording::load(&output_path).expect("saved recording should load");
        assert_eq!(saved.metadata.notes, "metadata note");
        assert_eq!(saved.metadata.operator, "metadata operator");
        assert_eq!(saved.frame_count(), 1);

        let _ = std::fs::remove_file(output_path);
    }

    #[test]
    fn stop_recording_persists_recording_start_time_not_save_time() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let standby = build_standby_piper(IdleRxAdapter::new(), sent_frames);
        let output_path = temp_recording_path("recording-start-time");

        let before_start = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let (standby, handle) = standby
            .start_recording(crate::recording::RecordingConfig {
                output_path: output_path.clone(),
                stop_condition: crate::recording::StopCondition::Manual,
                metadata: crate::recording::RecordingMetadata {
                    notes: "start-time".to_string(),
                    operator: "tester".to_string(),
                },
            })
            .expect("recording should start");

        std::thread::sleep(Duration::from_millis(1_100));

        let before_stop = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let (_standby, _stats) =
            standby.stop_recording(handle).expect("recording should stop cleanly");
        let saved = PiperRecording::load(&output_path).expect("saved recording should load");

        assert!(
            saved.metadata.start_time >= before_start,
            "recording start time must be captured when recording begins"
        );
        assert!(
            saved.metadata.start_time < before_stop,
            "recording start time must not collapse to the later save time"
        );

        let _ = std::fs::remove_file(output_path);
    }

    #[test]
    fn duration_recording_stop_condition_sets_stop_requested_after_deadline() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let standby = build_standby_piper(IdleRxAdapter::new(), sent_frames);
        let output_path = temp_recording_path("recording-duration-stop");

        let (_standby, handle) = standby
            .start_recording(crate::recording::RecordingConfig {
                output_path: output_path.clone(),
                stop_condition: crate::recording::StopCondition::Duration(0),
                metadata: crate::recording::RecordingMetadata {
                    notes: "test".to_string(),
                    operator: "tester".to_string(),
                },
            })
            .expect("recording should start");

        assert!(
            handle.is_stop_requested(),
            "zero-duration recordings should request stop immediately once observed"
        );

        let _ = std::fs::remove_file(output_path);
    }

    #[test]
    fn frame_count_recording_stop_condition_sets_stop_requested_after_threshold() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let standby = build_standby_piper(IdleRxAdapter::new(), sent_frames);
        let driver = Arc::clone(&standby.driver);
        let output_path = temp_recording_path("recording-framecount-stop");

        let (_standby, handle) = standby
            .start_recording(crate::recording::RecordingConfig {
                output_path: output_path.clone(),
                stop_condition: crate::recording::StopCondition::FrameCount(1),
                metadata: crate::recording::RecordingMetadata {
                    notes: "test".to_string(),
                    operator: "tester".to_string(),
                },
            })
            .expect("recording should start");

        driver
            .send_reliable(PiperFrame::new_standard(0x151, &[0x55]))
            .expect("driver should send a frame for frame-count stop");
        wait_until(
            Duration::from_millis(200),
            || handle.is_stop_requested(),
            "frame-count stop should request stop after the first recorded frame",
        );

        let _ = std::fs::remove_file(output_path);
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
            InitializedConnection {
                quirks: DeviceQuirks::from_firmware_version(Version::new(1, 8, 3)),
                initial_state: InitialMotionState::Standby,
            },
        )
        .expect("monitor connection should initialize as standby");
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
    fn active_drop_fault_latched_sends_bounded_shutdown_lane_emergency_stop() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let active = build_active_mit_piper(
            DeviceQuirks::from_firmware_version(Version::new(1, 8, 3)),
            sent_frames.clone(),
        );
        let driver = Arc::clone(&active.driver);
        driver.latch_fault();

        drop(active);
        wait_until(
            Duration::from_millis(200),
            || !sent_frames.lock().expect("sent frames lock").is_empty(),
            "fault-latched drop should issue a bounded shutdown command",
        );

        assert_eq!(
            sent_frames.lock().expect("sent frames lock").as_slice(),
            &[piper_protocol::control::EmergencyStopCommand::emergency_stop().to_frame()],
            "fault-latched drop must use the shutdown lane emergency-stop path",
        );

        let metrics = driver.get_metrics();
        assert_eq!(metrics.tx_drop_shutdown_attempt_total, 1);
        assert_eq!(metrics.tx_drop_shutdown_success_total, 1);
        assert_eq!(metrics.tx_drop_shutdown_timeout_total, 0);
        assert_eq!(metrics.tx_drop_shutdown_skipped_total, 0);
    }

    #[test]
    fn active_drop_skips_fault_shutdown_when_runtime_is_already_stopping() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let active = build_active_mit_piper(
            DeviceQuirks::from_firmware_version(Version::new(1, 8, 3)),
            sent_frames.clone(),
        );
        let driver = Arc::clone(&active.driver);
        driver.latch_fault();
        driver.request_stop();
        wait_until(
            Duration::from_millis(200),
            || !driver.health().tx_alive,
            "request_stop should eventually stop the TX worker",
        );

        drop(active);
        thread::sleep(Duration::from_millis(20));

        assert!(
            sent_frames.lock().expect("sent frames lock").is_empty(),
            "stopping runtime must not attempt a second drop-time shutdown",
        );

        let metrics = driver.get_metrics();
        assert_eq!(metrics.tx_drop_shutdown_attempt_total, 0);
        assert_eq!(metrics.tx_drop_shutdown_success_total, 0);
        assert_eq!(metrics.tx_drop_shutdown_timeout_total, 0);
        assert_eq!(metrics.tx_drop_shutdown_skipped_total, 1);
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
            InitializedConnection {
                quirks: DeviceQuirks::from_firmware_version(Version::new(1, 8, 3)),
                initial_state: InitialMotionState::Standby,
            },
        )
        .expect("monitor connection should initialize");

        let error = match connected.require_motion() {
            Ok(_) => panic!("monitor-only connection must remain read-only"),
            Err(error) => error,
        };
        assert!(matches!(error, RobotError::RealtimeUnsupported { .. }));
    }

    #[test]
    fn strict_connection_returns_maintenance_when_initial_mask_is_not_fully_disabled() {
        let driver = Arc::new(
            RobotPiper::new_dual_thread_parts(
                IdleRxAdapter::new(),
                RecordingTxAdapter::new(Arc::new(Mutex::new(Vec::new()))),
                None,
            )
            .expect("strict driver should start"),
        );
        let connected = connected_piper_from_driver(
            driver,
            InitializedConnection {
                quirks: DeviceQuirks::from_firmware_version(Version::new(1, 8, 3)),
                initial_state: InitialMotionState::Maintenance {
                    confirmed_mask: Some(0b000001),
                },
            },
        )
        .expect("strict connection should expose maintenance instead of failing");

        match connected {
            ConnectedPiper::Strict(MotionConnectedState::Maintenance(piper)) => {
                assert_eq!(piper.observer().joint_enabled_mask_confirmed(), None);
            },
            _ => panic!("expected strict maintenance connection"),
        }
    }

    #[test]
    fn monitor_connection_requires_confirmed_standby() {
        let driver = Arc::new(
            RobotPiper::new_dual_thread_parts(
                MonitorCapabilityRx::new(IdleRxAdapter::new()),
                RecordingTxAdapter::new(Arc::new(Mutex::new(Vec::new()))),
                None,
            )
            .expect("monitor driver should start"),
        );
        let error = match connected_piper_from_driver(
            driver,
            InitializedConnection {
                quirks: DeviceQuirks::from_firmware_version(Version::new(1, 8, 3)),
                initial_state: InitialMotionState::Maintenance {
                    confirmed_mask: None,
                },
            },
        ) {
            Ok(_) => panic!("monitor connection must reject non-standby initial state"),
            Err(error) => error,
        };

        assert!(matches!(
            error,
            RobotError::MaintenanceRequired {
                confirmed_mask: None
            }
        ));
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
        assert_eq!(sent[0].timestamp_us, 0);

        drop(standby);
        let _ = std::fs::remove_file(recording_path);
    }

    #[test]
    fn replay_recording_clears_source_timestamps_before_sending() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let recording_path = write_test_recording(&[(123_456, 0x155, &[0x01, 0x02])]);
        let replay = build_standby_piper(IdleRxAdapter::new(), sent_frames.clone())
            .enter_replay_mode()
            .expect("enter_replay_mode should succeed");

        let standby = replay
            .replay_recording(&recording_path, 1.0)
            .expect("replay should complete successfully");
        assert!(matches!(standby._state, Standby));

        let sent = sent_frames.lock().expect("sent frames lock");
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].id, 0x155);
        assert_eq!(sent[0].len, 2);
        assert_eq!(sent[0].data[0], 0x01);
        assert_eq!(sent[0].data[1], 0x02);
        assert_eq!(sent[0].timestamp_us, 0);

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
    fn position_mode_runtime_motion_type_guard_allows_matching_helpers_and_emits_expected_frames() {
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

        joint_robot
            .send_position_command(&JointArray::splat(Rad(0.0)))
            .expect("joint mode should allow joint commands");
        thread::sleep(Duration::from_millis(50));

        let joint_ids: Vec<u32> = joint_sent
            .lock()
            .expect("joint sent frames lock")
            .iter()
            .map(|frame| frame.id)
            .collect();
        assert_eq!(
            joint_ids,
            vec![
                ID_JOINT_CONTROL_12,
                ID_JOINT_CONTROL_34,
                ID_JOINT_CONTROL_56
            ]
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

        cartesian_robot
            .command_cartesian_pose(
                Position3D::new(0.1, 0.0, 0.2),
                EulerAngles::new(0.0, 0.0, 0.0),
            )
            .expect("Cartesian mode should allow end-pose commands");
        thread::sleep(Duration::from_millis(50));

        let cartesian_ids: Vec<u32> = cartesian_sent
            .lock()
            .expect("cartesian sent frames lock")
            .iter()
            .map(|frame| frame.id)
            .collect();
        assert_eq!(cartesian_ids, vec![0x152, 0x153, 0x154]);

        let linear_sent = Arc::new(Mutex::new(Vec::new()));
        let linear_driver = Arc::new(
            RobotPiper::new_dual_thread_parts(
                IdleRxAdapter::new(),
                RecordingTxAdapter::new(linear_sent.clone()),
                None,
            )
            .expect("linear driver should start"),
        );
        let linear_robot =
            build_active_position_piper_with_motion_type(linear_driver, MotionType::Linear);

        linear_robot
            .move_linear(
                Position3D::new(0.1, 0.0, 0.2),
                EulerAngles::new(0.0, 0.0, 0.0),
            )
            .expect("Linear mode should allow linear commands");
        thread::sleep(Duration::from_millis(50));

        let linear_ids: Vec<u32> = linear_sent
            .lock()
            .expect("linear sent frames lock")
            .iter()
            .map(|frame| frame.id)
            .collect();
        assert_eq!(linear_ids, vec![0x152, 0x153, 0x154]);

        let circular_sent = Arc::new(Mutex::new(Vec::new()));
        let circular_driver = Arc::new(
            RobotPiper::new_dual_thread_parts(
                IdleRxAdapter::new(),
                RecordingTxAdapter::new(circular_sent.clone()),
                None,
            )
            .expect("circular driver should start"),
        );
        let circular_robot =
            build_active_position_piper_with_motion_type(circular_driver, MotionType::Circular);

        circular_robot
            .move_circular(
                Position3D::new(0.1, 0.0, 0.2),
                EulerAngles::new(0.0, 0.0, 0.0),
                Position3D::new(0.12, 0.02, 0.2),
                EulerAngles::new(0.0, 0.0, 0.0),
            )
            .expect("Circular mode should allow circular commands");
        thread::sleep(Duration::from_millis(50));

        let circular_ids: Vec<u32> = circular_sent
            .lock()
            .expect("circular sent frames lock")
            .iter()
            .map(|frame| frame.id)
            .collect();
        assert_eq!(
            circular_ids,
            vec![0x152, 0x153, 0x154, 0x158, 0x152, 0x153, 0x154, 0x158]
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
            baseline.host_rx_mono_us,
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
            baseline.host_rx_mono_us,
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
            7,
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
