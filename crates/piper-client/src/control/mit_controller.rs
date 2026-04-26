//! MIT 模式高层控制器
//!
//! 简化 MIT 模式的使用，提供：
//! - 阻塞式位置控制
//! - 自动状态管理
//! - 循环锚点机制（消除累积漂移，保证精确 200Hz）
//! - 容错性增强（允许偶发丢帧）
//!
//! # 设计理念
//!
//! - **Option 模式**：使用 `Option<Piper<Active<MitMode>>>` 允许安全提取
//! - **状态流转**：`park()` 返还 `Piper<Standby>`，支持继续使用
//! - **循环锚点**：使用绝对时间锚点，消除累积漂移
//! - **发送容错性**：允许最多连续 5 个发送失败控制周期（25ms @ 200Hz）
//! - **诊断摘要**：热路径只输出限速故障告警，恢复信息由异步后台诊断线程输出
//!
//! # 使用示例
//!
//! ```rust,no_run
//! use piper_client::ControlReadPolicy;
//! use piper_client::control::MitController;
//! use piper_client::control::MitControllerConfig;
//! use piper_client::state::*;
//! use piper_client::types::*;
//! use std::time::Duration;
//!
//! # fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
//! # // 注意：此示例展示了API用法，实际运行需要硬件连接
//! # // 以下代码为伪代码，仅展示API调用方式
//! #
//! # // 假设已经连接并使能
//! # // let piper = ...;
//! #
//! // 创建控制器配置
//! let config = MitControllerConfig {
//!     kp_gains: [5.0; 6],        // Nm/rad
//!     kd_gains: [0.8; 6],        // Nm/(rad/s)
//!     safe_hold_kp_gains: [5.0; 6],
//!     safe_hold_kd_gains: [0.8; 6],
//!     rest_position: None,
//!     control_rate: 200.0,
//!     read_policy: ControlReadPolicy::default(), // 默认严格控制级新鲜度（15ms）
//! };
//! # // let mut controller = MitController::new(piper, config);
//!
//! // 运动到目标位置（使用锚点机制，保证精确 200Hz）
//! let target = [
//!     Rad(0.5), Rad(0.7), Rad(-0.4),
//!     Rad(0.2), Rad(0.3), Rad(0.5),
//! ];
//! # // let reached = controller.move_to_position(
//! # //     target,
//! # //     Rad(0.01),
//! # //     Duration::from_secs(5),
//! # // )?;
//!
//! // 可选：先显式回位，再显式停车
//! # // let reached_rest = controller.move_to_rest(Rad(0.01), Duration::from_secs(3))?;
//!
//! // 显式停车（返还 Piper<Standby>）
//! # // let piper_standby = controller.park(DisableConfig::default())?;
//! # Ok(())
//! # }
//! ```

use std::time::{Duration, Instant};
use tracing::{error, warn};

use super::hot_path_diagnostics::{FaultLogDecision, HotPathDiagnostics, RecoverySummary};
use super::mit_diagnostic_dispatcher::{
    MitDiagnosticDispatchError, MitDiagnosticDispatcher, MitDiagnosticEvent, global_dispatcher,
};
use super::snapshot_ready::{
    CONTROL_SNAPSHOT_POLL_INTERVAL, CONTROL_SNAPSHOT_READY_TIMEOUT, wait_for_control_snapshot_ready,
};
use crate::observer::{ControlReadPolicy, Observer};
use crate::raw_commander::RawCommander;
use crate::state::StrictRealtime;
use crate::state::machine::{Active, DisableConfig, MitMode, Piper, Standby};
use crate::types::*;
use piper_driver::BackendCapability;

/// MIT 控制器配置
#[derive(Debug, Clone)]
pub struct MitControllerConfig {
    /// PD 控制器 Kp 增益（每个关节独立）
    ///
    /// 单位：Nm/rad
    /// 典型值：3.0 - 10.0
    pub kp_gains: [f64; 6],

    /// PD 控制器 Kd 增益（每个关节独立）
    ///
    /// 单位：Nm/(rad/s)
    /// 典型值：0.3 - 1.5
    pub kd_gains: [f64; 6],

    /// 故障收口时使用的位置保持 Kp 增益（每个关节独立）
    ///
    /// 仅用于 fail-closed safe-hold，不会覆盖正常控制回路的用户增益。
    pub safe_hold_kp_gains: [f64; 6],

    /// 故障收口时使用的位置保持 Kd 增益（每个关节独立）
    ///
    /// 仅用于 fail-closed safe-hold，不会覆盖正常控制回路的用户增益。
    pub safe_hold_kd_gains: [f64; 6],

    /// 显式回位使用的休息位置目标
    ///
    /// `None` 表示未配置休息位置；调用 `move_to_rest()` 时会返回错误。
    /// `park()` 和 `Drop` 都不会隐式移动到该位置。
    pub rest_position: Option<[Rad; 6]>,

    /// 控制循环频率（Hz）
    ///
    /// 使用绝对时间锚点机制，实际频率将精确锁定在此值。
    /// 推荐值：200.0 Hz（与固件更新频率一致）
    pub control_rate: f64,

    /// 控制闭环读取策略
    ///
    /// 默认建议使用 `ControlReadPolicy::default()`，其最大反馈年龄为 15ms。
    pub read_policy: ControlReadPolicy,
}

impl Default for MitControllerConfig {
    fn default() -> Self {
        Self {
            kp_gains: [5.0; 6],
            kd_gains: [0.8; 6],
            safe_hold_kp_gains: [5.0; 6],
            safe_hold_kd_gains: [0.8; 6],
            rest_position: None,
            control_rate: 200.0,
            read_policy: ControlReadPolicy::default(),
        }
    }
}

/// Safe-state action taken before the controller latched into terminal fail-closed mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SafeAction {
    /// Sent a zero-velocity MIT hold command at the latest valid joint anchor.
    HoldPosition,
    /// Enqueued a bounded shutdown-lane emergency stop.
    EmergencyStop,
}

/// 控制错误
#[derive(Debug, thiserror::Error)]
pub enum ControlError {
    /// Controller was already parked
    #[error("Controller was already parked, cannot execute commands")]
    AlreadyParked,

    /// 休息位置未配置
    #[error(
        "Rest position is not configured; call move_to_position() explicitly or set MitControllerConfig::rest_position"
    )]
    RestPositionNotConfigured,

    /// 控制器已进入 fail-closed 终态，只允许 `park()` 或 drop 安全退出
    #[error("Controller entered fail-closed safe state via {action:?}: {source}")]
    SafedOut {
        action: SafeAction,
        #[source]
        source: Box<RobotError>,
    },

    /// 连续错误超过阈值
    #[error("Consecutive CAN failures: {count}, last error: {last_error}")]
    ConsecutiveFailures {
        count: u32,
        #[source]
        last_error: Box<RobotError>,
    },

    /// 其他机器人错误
    #[error("Robot error: {0}")]
    RobotError(#[from] RobotError),

    /// 超时
    #[error("Operation timeout: {timeout_ms}ms")]
    Timeout { timeout_ms: u64 },
}

/// MIT 模式高层控制器
///
/// **核心特性**：
/// - ✅ Option 模式：允许安全提取 Piper
/// - ✅ 循环锚点：消除累积漂移，保证精确 200Hz
/// - ✅ 容错性：允许最多连续 5 个失败控制周期
/// - ✅ 状态流转：`park()` 返还 `Piper<Standby>`
pub struct MitController {
    /// ⚠️ Option 包装，允许 park() 时安全提取
    piper: Option<Piper<Active<MitMode>, StrictRealtime>>,

    /// 状态观察器
    observer: Observer<StrictRealtime>,

    /// 控制器配置
    config: MitControllerConfig,

    /// 最近一次成功闭环快照锚点，用于故障收口 safe-hold
    last_hold_anchor: Option<JointArray<Rad>>,

    /// 是否已进入终态 safe-out
    safed_out: bool,

    /// safe-out 终态的行为与原因
    safe_state: Option<SafeStateLatch>,

    /// 控制发送错误诊断限速状态
    send_failure_diagnostics: HotPathDiagnostics,

    /// 控制循环 overrun 诊断限速状态
    overrun_diagnostics: HotPathDiagnostics,

    /// 异步诊断派发器，确保 recovery summary 不在实时线程直接记录日志
    diagnostic_dispatcher: MitDiagnosticDispatcher,

    /// 因异步派发队列满/不可用而暂存的丢失恢复事件计数
    dropped_recovery_events: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TickSchedule {
    sleep_duration: Option<Duration>,
    next_cycle_deadline: Instant,
    overrun_lateness: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CommandCycleDisposition {
    CheckReached { next_error_count: u32 },
    MissedCycle { next_error_count: u32 },
    Abort { failure_count: u32 },
}

const DIAGNOSTIC_LOG_INTERVAL: Duration = Duration::from_secs(1);
const SAFE_STATE_DEADLINE: Duration = Duration::from_millis(50);
const FORCED_DIAGNOSTIC_SEND_TIMEOUT: Duration = Duration::from_millis(1);

#[derive(Debug, Clone)]
struct SafeStateLatch {
    action: SafeAction,
    reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct PendingDiagnosticFlush {
    send_failure: Option<RecoverySummary>,
    overrun: Option<RecoverySummary>,
}

fn finalize_tick_schedule(now: Instant, cycle_deadline: Instant, period: Duration) -> TickSchedule {
    if cycle_deadline > now {
        TickSchedule {
            sleep_duration: Some(cycle_deadline.duration_since(now)),
            next_cycle_deadline: cycle_deadline + period,
            overrun_lateness: Duration::ZERO,
        }
    } else {
        let overrun_lateness = now.saturating_duration_since(cycle_deadline);
        let mut next_cycle_deadline = cycle_deadline + period;
        while next_cycle_deadline <= now {
            next_cycle_deadline += period;
        }
        TickSchedule {
            sleep_duration: None,
            next_cycle_deadline,
            overrun_lateness,
        }
    }
}

fn classify_command_cycle(
    command_succeeded: bool,
    error_count: u32,
    max_tolerance: u32,
) -> CommandCycleDisposition {
    if command_succeeded {
        return CommandCycleDisposition::CheckReached {
            next_error_count: 0,
        };
    }

    let failure_count = error_count.saturating_add(1);
    if failure_count > max_tolerance {
        CommandCycleDisposition::Abort { failure_count }
    } else {
        CommandCycleDisposition::MissedCycle {
            next_error_count: failure_count,
        }
    }
}

impl MitController {
    /// 创建新的 MIT 控制器
    ///
    /// # 参数
    ///
    /// - `piper`: 已使能 MIT 模式的 Piper
    /// - `config`: 控制器配置
    ///
    /// 构造时会短暂等待第一份完整且对齐的控制快照，避免刚进入 MIT 模式时
    /// 第一拍控制循环因冷数据而立即 fail-closed。
    ///
    /// # 示例
    ///
    /// ```rust,no_run
    /// # use piper_client::control::MitController;
    /// # use piper_client::control::MitControllerConfig;
    /// # use piper_client::state::*;
    /// #
    /// # // 假设已经使能 MIT 模式
    /// # // let piper = ...;
    /// #
    /// let config = MitControllerConfig::default();
    /// # // let controller = MitController::new(piper, config)?;
    /// # // 使用 controller 进行控制...
    /// ```
    pub fn new(
        piper: Piper<Active<MitMode>, StrictRealtime>,
        config: MitControllerConfig,
    ) -> Result<Self> {
        if piper.driver.backend_capability() != BackendCapability::StrictRealtime {
            return Err(RobotError::realtime_unsupported(
                "MIT controller requires a StrictRealtime backend with trusted alignment timestamps",
            ));
        }

        Self::validate_config(&config)?;

        let diagnostic_dispatcher = match global_dispatcher() {
            Ok(dispatcher) => dispatcher,
            Err(error) => {
                if error.should_warn() {
                    warn!(
                        "Failed to initialize MIT async diagnostic logger; recovery summaries will be dropped until a later dispatcher rebuild succeeds: {}",
                        error.message(),
                    );
                }
                MitDiagnosticDispatcher::disabled()
            },
        };
        Self::new_with_dispatcher(piper, config, diagnostic_dispatcher)
    }

    fn new_with_dispatcher(
        piper: Piper<Active<MitMode>, StrictRealtime>,
        config: MitControllerConfig,
        diagnostic_dispatcher: MitDiagnosticDispatcher,
    ) -> Result<Self> {
        if piper.driver.backend_capability() != BackendCapability::StrictRealtime {
            return Err(RobotError::realtime_unsupported(
                "MIT controller requires a StrictRealtime backend with trusted alignment timestamps",
            ));
        }

        Self::validate_config(&config)?;

        // 提取 observer（Clone 是轻量的，Arc 指针）
        let observer = piper.observer().clone();
        let _initial_snapshot = wait_for_control_snapshot_ready(
            CONTROL_SNAPSHOT_READY_TIMEOUT,
            CONTROL_SNAPSHOT_POLL_INTERVAL,
            || observer.control_snapshot(config.read_policy),
        )?;

        Ok(Self {
            piper: Some(piper),
            observer,
            config,
            last_hold_anchor: None,
            safed_out: false,
            safe_state: None,
            send_failure_diagnostics: HotPathDiagnostics::default(),
            overrun_diagnostics: HotPathDiagnostics::default(),
            diagnostic_dispatcher,
            dropped_recovery_events: 0,
        })
    }

    fn validate_config(config: &MitControllerConfig) -> Result<()> {
        if !config.control_rate.is_finite() || config.control_rate <= 0.0 {
            return Err(RobotError::ConfigError(
                "MitControllerConfig.control_rate must be finite and > 0.0".to_string(),
            ));
        }

        Self::validate_gain_array("MitControllerConfig.kp_gains", &config.kp_gains)?;
        Self::validate_gain_array("MitControllerConfig.kd_gains", &config.kd_gains)?;
        Self::validate_gain_array(
            "MitControllerConfig.safe_hold_kp_gains",
            &config.safe_hold_kp_gains,
        )?;
        Self::validate_gain_array(
            "MitControllerConfig.safe_hold_kd_gains",
            &config.safe_hold_kd_gains,
        )?;

        Ok(())
    }

    fn validate_gain_array(name: &str, gains: &[f64; 6]) -> Result<()> {
        for (index, gain) in gains.iter().copied().enumerate() {
            if !gain.is_finite() || gain < 0.0 {
                return Err(RobotError::ConfigError(format!(
                    "{name}[{}] must be finite and non-negative",
                    index + 1
                )));
            }
        }

        Ok(())
    }

    fn active_piper(&self) -> crate::types::Result<&Piper<Active<MitMode>, StrictRealtime>> {
        self.piper.as_ref().ok_or(RobotError::InvalidTransition {
            from: "Active<MitMode>".to_string(),
            to: "Standby".to_string(),
        })
    }

    fn ensure_motion_allowed(&self) -> core::result::Result<(), ControlError> {
        if self.safed_out {
            return Err(self.safed_out_error());
        }
        if self.piper.is_none() {
            return Err(ControlError::AlreadyParked);
        }
        Ok(())
    }

    fn safed_out_error(&self) -> ControlError {
        let (action, reason) = match &self.safe_state {
            Some(latch) => (latch.action, latch.reason.clone()),
            None => (
                SafeAction::EmergencyStop,
                "controller is already latched in fail-closed mode".to_string(),
            ),
        };

        ControlError::SafedOut {
            action,
            source: Box::new(RobotError::StatePoisoned { reason }),
        }
    }

    fn latch_safe_state(&mut self, action: SafeAction, reason: impl Into<String>) {
        self.safed_out = true;
        self.safe_state = Some(SafeStateLatch {
            action,
            reason: reason.into(),
        });
    }

    fn log_transient_send_failure(&mut self, error_count: u32, error: &RobotError) {
        match self
            .send_failure_diagnostics
            .record_fault(Instant::now(), DIAGNOSTIC_LOG_INTERVAL)
        {
            FaultLogDecision::Emit { suppressed_repeats } => warn!(
                "Transient CAN error ({}): {:?}. Preserving anchored schedule. Suppressed {} repeated warning(s) in the last window.",
                error_count, error, suppressed_repeats,
            ),
            FaultLogDecision::Suppress => {},
        }
    }

    fn note_send_failure_recovered(&mut self) {
        self.send_failure_diagnostics.record_recovery(Instant::now());
    }

    fn run_cycle_epilogue(&mut self, next_tick: &mut Instant, period: Duration) {
        let schedule = finalize_tick_schedule(Instant::now(), *next_tick, period);
        if let Some(sleep_duration) = schedule.sleep_duration {
            self.overrun_diagnostics.record_recovery(Instant::now());
            spin_sleep::sleep(sleep_duration);
        } else {
            match self.overrun_diagnostics.record_fault(Instant::now(), DIAGNOSTIC_LOG_INTERVAL) {
                FaultLogDecision::Emit { suppressed_repeats } => warn!(
                    "Control loop overrun: operation missed deadline by {:?} (period {:?}); skipping sleep and preserving anchored schedule. Suppressed {} repeated warning(s) in the last window.",
                    schedule.overrun_lateness, period, suppressed_repeats,
                ),
                FaultLogDecision::Suppress => {},
            }
        }
        *next_tick = schedule.next_cycle_deadline;
    }

    fn poll_windowed_diagnostics_at(&mut self, now: Instant) -> PendingDiagnosticFlush {
        PendingDiagnosticFlush {
            send_failure: self
                .send_failure_diagnostics
                .poll_recovery_summary(now, DIAGNOSTIC_LOG_INTERVAL),
            overrun: self.overrun_diagnostics.poll_recovery_summary(now, DIAGNOSTIC_LOG_INTERVAL),
        }
    }

    fn collect_forced_pending_diagnostics_at(&mut self, now: Instant) -> PendingDiagnosticFlush {
        PendingDiagnosticFlush {
            send_failure: self.send_failure_diagnostics.force_flush_recovery_summary(now),
            overrun: self.overrun_diagnostics.force_flush_recovery_summary(now),
        }
    }

    fn submit_diagnostic_event(
        &mut self,
        event: MitDiagnosticEvent,
        forced: bool,
    ) -> core::result::Result<(), ()> {
        match self.try_submit_diagnostic_event(event, forced) {
            Ok(()) => Ok(()),
            Err(MitDiagnosticDispatchError::Disconnected) => {
                if let Ok(dispatcher) = global_dispatcher() {
                    self.diagnostic_dispatcher = dispatcher;
                    self.try_submit_diagnostic_event(event, forced).map_err(|_| ())
                } else {
                    Err(())
                }
            },
            Err(_) => Err(()),
        }
    }

    fn try_submit_diagnostic_event(
        &self,
        event: MitDiagnosticEvent,
        forced: bool,
    ) -> core::result::Result<(), MitDiagnosticDispatchError> {
        if forced {
            self.diagnostic_dispatcher
                .submit_cold_path_event(event, FORCED_DIAGNOSTIC_SEND_TIMEOUT)
        } else {
            self.diagnostic_dispatcher.submit_runtime_event(event)
        }
    }

    fn submit_pending_diagnostics(&mut self, pending: PendingDiagnosticFlush, forced: bool) -> u32 {
        let mut newly_dropped: u32 = 0;

        if self.dropped_recovery_events > 0
            && self
                .submit_diagnostic_event(
                    MitDiagnosticEvent::DroppedRecoveryEvents {
                        count: self.dropped_recovery_events,
                    },
                    forced,
                )
                .is_ok()
        {
            self.dropped_recovery_events = 0;
        }

        if let Some(summary) = pending.send_failure
            && self
                .submit_diagnostic_event(MitDiagnosticEvent::SendFailureRecovery(summary), forced)
                .is_err()
        {
            self.dropped_recovery_events = self.dropped_recovery_events.saturating_add(1);
            newly_dropped = newly_dropped.saturating_add(1);
        }

        if let Some(summary) = pending.overrun
            && self
                .submit_diagnostic_event(MitDiagnosticEvent::OverrunRecovery(summary), forced)
                .is_err()
        {
            self.dropped_recovery_events = self.dropped_recovery_events.saturating_add(1);
            newly_dropped = newly_dropped.saturating_add(1);
        }

        newly_dropped
    }

    fn submit_windowed_diagnostics_at(&mut self, now: Instant) -> u32 {
        let pending = self.poll_windowed_diagnostics_at(now);
        self.submit_pending_diagnostics(pending, false)
    }

    fn submit_windowed_diagnostics(&mut self) {
        let _ = self.submit_windowed_diagnostics_at(Instant::now());
    }

    fn force_flush_pending_diagnostics(&mut self) {
        let _ = self.force_flush_pending_diagnostics_at(Instant::now());
    }

    fn force_flush_pending_diagnostics_at(&mut self, now: Instant) -> u32 {
        let pending = self.collect_forced_pending_diagnostics_at(now);
        self.submit_pending_diagnostics(pending, true)
    }

    fn finish_safe_state_transition(&mut self, error: ControlError) -> ControlError {
        self.force_flush_pending_diagnostics();
        error
    }

    fn command_joints_with_gains(
        &self,
        target: JointArray<Rad>,
        feedforward: Option<JointArray<NewtonMeter>>,
        kp: JointArray<f64>,
        kd: JointArray<f64>,
    ) -> crate::types::Result<()> {
        let velocities = JointArray::from([0.0; 6]);
        let torques = feedforward.unwrap_or(JointArray::from([NewtonMeter(0.0); 6]));
        self.active_piper()?.command_torques(&target, &velocities, &kp, &kd, &torques)
    }

    /// 阻塞式运动到目标位置
    ///
    /// **循环锚点机制**：
    /// - ✅ 使用绝对时间锚点，消除累积漂移
    /// - ✅ 无论 CAN 通信耗时多少，频率都锁定在 200Hz
    /// - ✅ 自动处理任务超时（Overrun）
    ///
    /// # 容错性
    ///
    /// 控制循环仅对**发送路径**提供有限容错：
    /// 允许偶尔的 CAN 发送错误（最多连续 5 个失败控制周期）。
    ///
    /// 控制快照读取一旦失败，会立即进入 fail-closed safe-out，
    /// 不会像发送路径那样继续容忍多个周期。
    ///
    /// # 参数
    ///
    /// - `target`: 目标关节位置（弧度）
    /// - `threshold`: 到达阈值（弧度）
    /// - `timeout`: 超时时间
    ///
    /// # 返回
    ///
    /// - `Ok(true)`: 到达目标
    /// - `Ok(false)`: 超时未到达
    /// - `Err(ControlError::SafedOut)`: 控制器已执行 safe-hold 或 emergency-stop 并锁成终态
    ///
    /// # 示例
    ///
    /// ```rust,no_run
    /// # use piper_client::control::MitController;
    /// # use piper_client::types::*;
    /// # use std::time::Duration;
    /// #
    /// # let mut controller: MitController = unsafe { std::mem::zeroed() };
    /// // 运动到目标位置，5秒超时
    /// # // let reached = controller.move_to_position(
    /// # //     [Rad(0.5), Rad(0.7), Rad(-0.4), Rad(0.2), Rad(0.3), Rad(0.5)],
    /// # //     Rad(0.01),  // 1cm 阈值
    /// # //     Duration::from_secs(5),
    /// # // )?;
    /// ```
    pub fn move_to_position(
        &mut self,
        target: [Rad; 6],
        threshold: Rad,
        timeout: Duration,
    ) -> core::result::Result<bool, ControlError> {
        const MAX_TOLERANCE: u32 = 5; // 允许连续 5 个发送失败控制周期（25ms @ 200Hz）
        self.ensure_motion_allowed()?;

        let mut error_count = 0;

        let start = Instant::now();

        // 使用绝对时间锚点机制消除累积漂移，保证精确的 200Hz 控制频率
        let period = Duration::from_secs_f64(1.0 / self.config.control_rate); // 5ms @ 200Hz
        let mut next_tick = Instant::now() + period;

        let result = loop {
            if start.elapsed() >= timeout {
                break Ok(false);
            }

            let command_result = self.command_joints(JointArray::from(target), None);
            let cycle_disposition =
                classify_command_cycle(command_result.is_ok(), error_count, MAX_TOLERANCE);

            match (command_result, cycle_disposition) {
                (Ok(()), CommandCycleDisposition::CheckReached { next_error_count }) => {
                    error_count = next_error_count;
                    self.note_send_failure_recovered();

                    let current = match self.observer.control_snapshot(self.config.read_policy) {
                        Ok(snapshot) => snapshot.position,
                        Err(error) => break Err(self.enter_safe_state(error)),
                    };
                    self.last_hold_anchor = Some(current);
                    let reached =
                        current.iter().zip(target.iter()).all(|(c, t)| (*c - *t).abs() < threshold);

                    if reached {
                        break Ok(true);
                    }
                },
                (Err(e), CommandCycleDisposition::MissedCycle { next_error_count }) => {
                    error_count = next_error_count;
                    self.log_transient_send_failure(error_count, &e);
                },
                (Err(e), CommandCycleDisposition::Abort { failure_count }) => {
                    error!(
                        "Consecutive CAN failures ({}): {:?}. Entering fail-closed safe state.",
                        failure_count, e
                    );
                    break Err(self.enter_safe_state(e));
                },
                _ => unreachable!("command result classification must stay consistent"),
            }

            self.run_cycle_epilogue(&mut next_tick, period);
            self.submit_windowed_diagnostics();
        };

        self.force_flush_pending_diagnostics();
        result
    }

    /// 阻塞式运动到配置中的休息位置
    ///
    /// 此方法只负责显式回位，不会自动失能；如果需要安全停机，
    /// 应在回位成功后继续调用 `park()`。
    pub fn move_to_rest(
        &mut self,
        threshold: Rad,
        timeout: Duration,
    ) -> core::result::Result<bool, ControlError> {
        self.ensure_motion_allowed()?;
        let target = self.config.rest_position.ok_or(ControlError::RestPositionNotConfigured)?;
        self.move_to_position(target, threshold, timeout)
    }

    /// 发送关节命令（MIT 模式 PD 控制）
    ///
    /// 直接传递每个关节的 kp/kd 增益到固件，让固件进行 PD 计算，
    /// 而不是在软件中计算 PD 输出。这样可以充分利用硬件的实时性能。
    ///
    /// # 参数
    ///
    /// - `target`: 目标位置
    /// - `feedforward`: 前馈力矩（可选）
    fn command_joints(
        &self,
        target: JointArray<Rad>,
        feedforward: Option<JointArray<NewtonMeter>>,
    ) -> crate::types::Result<()> {
        self.command_joints_with_gains(
            target,
            feedforward,
            JointArray::from(self.config.kp_gains),
            JointArray::from(self.config.kd_gains),
        )
    }

    fn send_safe_hold(&self, anchor: JointArray<Rad>) -> crate::types::Result<()> {
        let velocities = JointArray::from([0.0; 6]);
        let torques = JointArray::from([NewtonMeter(0.0); 6]);
        self.active_piper()?.command_torques_confirmed(
            &anchor,
            &velocities,
            &JointArray::from(self.config.safe_hold_kp_gains),
            &JointArray::from(self.config.safe_hold_kd_gains),
            &torques,
            SAFE_STATE_DEADLINE,
        )
    }

    fn enqueue_emergency_stop(&self) -> crate::types::Result<()> {
        let receipt = RawCommander::new(&self.active_piper()?.driver)
            .emergency_stop_enqueue(Instant::now() + SAFE_STATE_DEADLINE)?;
        receipt.wait()?;
        Ok(())
    }

    fn enter_safe_state(&mut self, cause: RobotError) -> ControlError {
        let cause_text = cause.to_string();

        if let Some(anchor) = self.last_hold_anchor {
            match self.send_safe_hold(anchor) {
                Ok(()) => {
                    self.latch_safe_state(
                        SafeAction::HoldPosition,
                        format!("safe-hold latched after {cause_text}"),
                    );
                    return self.finish_safe_state_transition(ControlError::SafedOut {
                        action: SafeAction::HoldPosition,
                        source: Box::new(cause),
                    });
                },
                Err(hold_error) => {
                    error!(
                        "MIT controller safe-hold send failed after {}: {:?}. Falling back to emergency stop.",
                        cause_text, hold_error,
                    );
                    match self.enqueue_emergency_stop() {
                        Ok(()) => {
                            self.latch_safe_state(
                                    SafeAction::EmergencyStop,
                                    format!(
                                        "emergency stop latched after {cause_text} and safe-hold failure {hold_error}"
                                    ),
                                );
                            return self.finish_safe_state_transition(ControlError::SafedOut {
                                action: SafeAction::EmergencyStop,
                                source: Box::new(cause),
                            });
                        },
                        Err(stop_error) => {
                            self.latch_safe_state(
                                SafeAction::EmergencyStop,
                                format!(
                                    "emergency stop failed after {cause_text}; safe-hold failure: {hold_error}; stop failure: {stop_error}"
                                ),
                            );
                            return self.finish_safe_state_transition(ControlError::SafedOut {
                                action: SafeAction::EmergencyStop,
                                source: Box::new(stop_error),
                            });
                        },
                    }
                },
            }
        }

        match self.enqueue_emergency_stop() {
            Ok(()) => {
                self.latch_safe_state(
                    SafeAction::EmergencyStop,
                    format!("emergency stop latched after {cause_text}"),
                );
                self.finish_safe_state_transition(ControlError::SafedOut {
                    action: SafeAction::EmergencyStop,
                    source: Box::new(cause),
                })
            },
            Err(stop_error) => {
                self.latch_safe_state(
                    SafeAction::EmergencyStop,
                    format!("emergency stop failed after {cause_text}: {stop_error}"),
                );
                self.finish_safe_state_transition(ControlError::SafedOut {
                    action: SafeAction::EmergencyStop,
                    source: Box::new(stop_error),
                })
            },
        }
    }

    /// 放松关节（发送零力矩命令）
    ///
    /// **注意**：此方法只发送一次零力矩命令，不会阻塞。
    /// 如果需要让关节自然下垂，应该多次调用或在循环中调用。
    ///
    /// 如果控制器已经进入 `SafedOut` 终态，则会拒绝继续发命令。
    ///
    /// # 示例
    ///
    /// ```rust,no_run
    /// # use piper_client::control::MitController;
    /// # let mut controller: MitController = unsafe { std::mem::zeroed() };
    /// # // controller.relax_joints()?;
    /// ```
    pub fn relax_joints(&mut self) -> core::result::Result<(), ControlError> {
        self.ensure_motion_allowed()?;

        let zero_pos = JointArray::from([Rad(0.0); 6]);
        let zero_vel = JointArray::from([0.0; 6]);
        let zero_kp = JointArray::from([0.0; 6]);
        let zero_kd = JointArray::from([0.0; 6]);
        let zero_torques = JointArray::from([NewtonMeter(0.0); 6]);

        self.active_piper()?
            .command_torques(&zero_pos, &zero_vel, &zero_kp, &zero_kd, &zero_torques)
            .map_err(ControlError::from)
    }

    /// 停车（只失能并返还 `Piper<Standby>`）
    ///
    /// **资源管理**：
    /// - ✅ 返还 `Piper<Standby>`，支持继续使用
    /// - ✅ 使用 Option 模式，安全提取 Piper
    ///
    /// # 安全保证
    ///
    /// **显式调用 park()**（推荐）：
    /// - 提取 Piper，调用 disable()，等待完成
    /// - 返回 `Piper<Standby>` 可继续使用
    /// - 不会触发 Piper 的 Drop
    /// - 不会自动移动到 `rest_position`
    ///
    /// **忘记调用 park()**（安全网）：
    /// - MitController 被 drop 时，`Piper<Active>::drop()` 自动触发
    /// - 只发送 bounded disable 命令，不执行任何回位运动
    /// - 电机被安全失能
    ///
    /// # 示例
    ///
    /// ```rust,no_run
    /// # use piper_client::control::MitController;
    /// # use piper_client::state::*;
    /// # use piper_client::types::Rad;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let mut controller: MitController = unsafe { std::mem::zeroed() };
    ///
    /// // 方式 1：先显式回位，再显式停车
    /// let _reached_rest = controller.move_to_rest(Rad(0.01), std::time::Duration::from_secs(3))?;
    /// let piper_standby = controller.park(DisableConfig::default())?;
    /// // 现在 piper_standby 可以重新使能或做其他操作
    ///
    /// // 方式 2：直接丢弃（触发自动安全失能）
    /// // drop(controller);  // 自动调用 Piper::drop()，发送 disable 命令
    /// # Ok(())
    /// # }
    /// ```
    pub fn park(
        mut self,
        config: DisableConfig,
    ) -> crate::types::Result<Piper<Standby, StrictRealtime>> {
        self.force_flush_pending_diagnostics();

        // 安全提取 piper（Option 变为 None）
        let piper =
            self.piper
                .take()
                .ok_or(ControlError::AlreadyParked)
                .map_err(|error| match error {
                    ControlError::AlreadyParked => crate::RobotError::InvalidTransition {
                        from: "Active<MitMode>".to_string(),
                        to: "Standby".to_string(),
                    },
                    _ => crate::RobotError::StatePoisoned {
                        reason: format!("Controller error: {:?}", error),
                    },
                })?;

        // 失能并返回到 Standby 状态
        piper.disable(config)
    }

    /// 获取 Observer（只读）
    pub fn observer(&self) -> &Observer<StrictRealtime> {
        &self.observer
    }

    /// Returns whether this controller has already latched into fail-closed terminal state.
    pub fn is_safed_out(&self) -> bool {
        self.safed_out
    }
}

impl Drop for MitController {
    fn drop(&mut self) {
        self.force_flush_pending_diagnostics();
    }
}

#[cfg(test)]
mod tests {
    use super::super::mit_diagnostic_dispatcher::{MitDiagnosticDispatcher, MitDiagnosticEvent};
    use super::*;
    use crate::observer::Observer;
    use crate::state::machine::{DriverModeDropPolicy, DropPolicy};
    use crossbeam_channel::bounded;
    use piper_can::{CanError, PiperFrame, RealtimeTxAdapter, RxAdapter};
    use piper_driver::Piper as RobotPiper;
    use piper_protocol::control::{EmergencyStopCommand, MitControlCommand, MotorEnableCommand};
    use piper_protocol::ids::{
        ID_JOINT_DRIVER_HIGH_SPEED_BASE, ID_JOINT_DRIVER_LOW_SPEED_BASE, ID_JOINT_FEEDBACK_12,
        ID_JOINT_FEEDBACK_34, ID_JOINT_FEEDBACK_56,
    };
    use semver::Version;
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread;

    #[derive(Default)]
    struct DisableFeedbackHarness {
        pending_disabled_frames: AtomicUsize,
        next_timestamp_us: AtomicU64,
    }

    impl DisableFeedbackHarness {
        fn arm_disabled_cycle(&self) {
            self.pending_disabled_frames.store(6, Ordering::Release);
        }

        fn next_disabled_joint(&self) -> Option<u8> {
            loop {
                let pending = self.pending_disabled_frames.load(Ordering::Acquire);
                if pending == 0 {
                    return None;
                }
                let next = pending - 1;
                if self
                    .pending_disabled_frames
                    .compare_exchange(pending, next, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
                {
                    return Some((6 - next) as u8);
                }
            }
        }

        fn next_timestamp_us(&self) -> u64 {
            self.next_timestamp_us.fetch_add(1, Ordering::Relaxed) + 10_000
        }
    }

    struct DisableAwareRxAdapter<R> {
        inner: R,
        harness: Arc<DisableFeedbackHarness>,
    }

    impl<R> DisableAwareRxAdapter<R> {
        fn new(inner: R, harness: Arc<DisableFeedbackHarness>) -> Self {
            Self { inner, harness }
        }
    }

    impl<R> RxAdapter for DisableAwareRxAdapter<R>
    where
        R: RxAdapter,
    {
        fn receive(&mut self) -> std::result::Result<PiperFrame, CanError> {
            match self.inner.receive() {
                Ok(frame) => Ok(frame),
                Err(CanError::Timeout) => {
                    let Some(joint_index) = self.harness.next_disabled_joint() else {
                        return Err(CanError::Timeout);
                    };
                    Ok(joint_driver_low_speed_frame(
                        joint_index,
                        false,
                        self.harness.next_timestamp_us(),
                    ))
                },
                Err(error) => Err(error),
            }
        }

        fn backend_capability(&self) -> piper_can::BackendCapability {
            self.inner.backend_capability()
        }

        fn startup_probe_until(
            &mut self,
            deadline: Instant,
        ) -> std::result::Result<Option<piper_can::BackendCapability>, CanError> {
            self.inner.startup_probe_until(deadline)
        }
    }

    struct DisableAwareTxAdapter<T> {
        inner: T,
        harness: Arc<DisableFeedbackHarness>,
    }

    impl<T> DisableAwareTxAdapter<T> {
        fn new(inner: T, harness: Arc<DisableFeedbackHarness>) -> Self {
            Self { inner, harness }
        }

        fn is_disable_all_frame(&self, frame: PiperFrame) -> bool {
            let disable_all = MotorEnableCommand::disable_all().to_frame();
            frame.id == disable_all.id && frame.data == disable_all.data
        }
    }

    impl<T> RealtimeTxAdapter for DisableAwareTxAdapter<T>
    where
        T: RealtimeTxAdapter,
    {
        fn send_control(
            &mut self,
            frame: PiperFrame,
            budget: Duration,
        ) -> std::result::Result<(), CanError> {
            let should_arm = self.is_disable_all_frame(frame);
            let result = self.inner.send_control(frame, budget);
            if should_arm && result.is_ok() {
                self.harness.arm_disabled_cycle();
            }
            result
        }

        fn send_shutdown_until(
            &mut self,
            frame: PiperFrame,
            deadline: Instant,
        ) -> std::result::Result<(), CanError> {
            let should_arm = self.is_disable_all_frame(frame);
            let result = self.inner.send_shutdown_until(frame, deadline);
            if should_arm && result.is_ok() {
                self.harness.arm_disabled_cycle();
            }
            result
        }
    }

    struct ScriptedRxAdapter {
        frames: VecDeque<PiperFrame>,
    }

    impl ScriptedRxAdapter {
        fn new(frames: Vec<PiperFrame>) -> Self {
            Self {
                frames: frames.into(),
            }
        }
    }

    impl RxAdapter for ScriptedRxAdapter {
        fn receive(&mut self) -> std::result::Result<PiperFrame, CanError> {
            self.frames.pop_front().ok_or(CanError::Timeout)
        }
    }

    struct DelayedScriptedRxAdapter {
        frames: VecDeque<(Duration, PiperFrame)>,
    }

    impl DelayedScriptedRxAdapter {
        fn new(frames: Vec<(Duration, PiperFrame)>) -> Self {
            Self {
                frames: frames.into(),
            }
        }
    }

    impl RxAdapter for DelayedScriptedRxAdapter {
        fn receive(&mut self) -> std::result::Result<PiperFrame, CanError> {
            let (delay, frame) = self.frames.pop_front().ok_or(CanError::Timeout)?;
            if !delay.is_zero() {
                thread::sleep(delay);
            }
            Ok(frame)
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

    impl RealtimeTxAdapter for RecordingTxAdapter {
        fn send_control(
            &mut self,
            frame: PiperFrame,
            budget: Duration,
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
            deadline: Instant,
        ) -> std::result::Result<(), CanError> {
            if deadline <= Instant::now() {
                return Err(CanError::Timeout);
            }
            self.sent_frames.lock().expect("sent frames lock").push(frame);
            Ok(())
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
            (ID_JOINT_DRIVER_HIGH_SPEED_BASE + u32::from(joint_index - 1)) as u16,
            &data,
        );
        frame.timestamp_us = timestamp_us;
        frame
    }

    fn joint_driver_low_speed_frame(
        joint_index: u8,
        enabled: bool,
        timestamp_us: u64,
    ) -> PiperFrame {
        let mut data = [0u8; 8];
        data[0..2].copy_from_slice(&240u16.to_be_bytes());
        data[2..4].copy_from_slice(&45i16.to_be_bytes());
        data[4] = 50;
        data[5] = if enabled { 0x40 } else { 0x00 };
        data[6..8].copy_from_slice(&5000u16.to_be_bytes());
        let mut frame = PiperFrame::new_standard(
            (ID_JOINT_DRIVER_LOW_SPEED_BASE + u32::from(joint_index - 1)) as u16,
            &data,
        );
        frame.timestamp_us = timestamp_us;
        frame
    }

    fn scripted_frames(timestamp_us: u64) -> Vec<PiperFrame> {
        vec![
            joint_feedback_frame(ID_JOINT_FEEDBACK_12 as u16, 0, 0, timestamp_us),
            joint_feedback_frame(ID_JOINT_FEEDBACK_34 as u16, 0, 0, timestamp_us),
            joint_feedback_frame(ID_JOINT_FEEDBACK_56 as u16, 0, 0, timestamp_us),
            joint_dynamic_frame(1, 0, 0, timestamp_us),
            joint_dynamic_frame(2, 0, 0, timestamp_us),
            joint_dynamic_frame(3, 0, 0, timestamp_us),
            joint_dynamic_frame(4, 0, 0, timestamp_us),
            joint_dynamic_frame(5, 0, 0, timestamp_us),
            joint_dynamic_frame(6, 0, 0, timestamp_us),
            joint_feedback_frame(ID_JOINT_FEEDBACK_12 as u16, 0, 0, timestamp_us + 1),
            joint_feedback_frame(ID_JOINT_FEEDBACK_34 as u16, 0, 0, timestamp_us + 1),
            joint_feedback_frame(ID_JOINT_FEEDBACK_56 as u16, 0, 0, timestamp_us + 1),
            joint_dynamic_frame(1, 0, 0, timestamp_us + 1),
            joint_dynamic_frame(2, 0, 0, timestamp_us + 1),
            joint_dynamic_frame(3, 0, 0, timestamp_us + 1),
            joint_dynamic_frame(4, 0, 0, timestamp_us + 1),
            joint_dynamic_frame(5, 0, 0, timestamp_us + 1),
            joint_dynamic_frame(6, 0, 0, timestamp_us + 1),
        ]
    }

    fn delayed_control_snapshot_frames(
        timestamp_us: u64,
        dynamic_delay: Duration,
    ) -> Vec<(Duration, PiperFrame)> {
        vec![
            (
                Duration::ZERO,
                joint_feedback_frame(ID_JOINT_FEEDBACK_12 as u16, 0, 0, timestamp_us),
            ),
            (
                Duration::ZERO,
                joint_feedback_frame(ID_JOINT_FEEDBACK_34 as u16, 0, 0, timestamp_us),
            ),
            (
                Duration::ZERO,
                joint_feedback_frame(ID_JOINT_FEEDBACK_56 as u16, 0, 0, timestamp_us),
            ),
            (dynamic_delay, joint_dynamic_frame(1, 0, 0, timestamp_us)),
            (Duration::ZERO, joint_dynamic_frame(2, 0, 0, timestamp_us)),
            (Duration::ZERO, joint_dynamic_frame(3, 0, 0, timestamp_us)),
            (Duration::ZERO, joint_dynamic_frame(4, 0, 0, timestamp_us)),
            (Duration::ZERO, joint_dynamic_frame(5, 0, 0, timestamp_us)),
            (Duration::ZERO, joint_dynamic_frame(6, 0, 0, timestamp_us)),
        ]
    }

    fn build_active_mit_piper_with_adapters<R, T>(
        rx_adapter: R,
        tx_adapter: T,
        post_feedback_delay: Duration,
    ) -> Piper<Active<MitMode>, StrictRealtime>
    where
        R: RxAdapter + Send + 'static,
        T: RealtimeTxAdapter + Send + 'static,
    {
        let disable_feedback = Arc::new(DisableFeedbackHarness::default());
        let driver = Arc::new(
            RobotPiper::new_dual_thread_parts(
                DisableAwareRxAdapter::new(rx_adapter, disable_feedback.clone()),
                DisableAwareTxAdapter::new(tx_adapter, disable_feedback),
                None,
            )
            .expect("driver should start"),
        );
        let observer = Observer::<StrictRealtime>::new(driver.clone());
        driver
            .wait_for_feedback(Duration::from_millis(200))
            .expect("feedback should arrive");
        if !post_feedback_delay.is_zero() {
            thread::sleep(post_feedback_delay);
        }

        Piper {
            driver,
            observer,
            quirks: DeviceQuirks::from_firmware_version(Version::new(1, 8, 3)),
            drop_policy: DropPolicy::Noop,
            driver_mode_drop_policy: DriverModeDropPolicy::Preserve,
            _state: unsafe { std::mem::zeroed() },
        }
    }

    fn build_active_mit_piper(
        sent_frames: Arc<Mutex<Vec<PiperFrame>>>,
        post_feedback_delay: Duration,
    ) -> Piper<Active<MitMode>, StrictRealtime> {
        build_active_mit_piper_with_adapters(
            ScriptedRxAdapter::new(scripted_frames(1_000)),
            RecordingTxAdapter::new(sent_frames),
            post_feedback_delay,
        )
    }

    fn wait_for_sent_frames(
        sent_frames: &Arc<Mutex<Vec<PiperFrame>>>,
        at_least: usize,
    ) -> Vec<PiperFrame> {
        let deadline = Instant::now() + Duration::from_millis(200);
        loop {
            let frames = sent_frames.lock().expect("sent frames lock").clone();
            if frames.len() >= at_least {
                return frames;
            }
            if Instant::now() >= deadline {
                panic!(
                    "expected at least {at_least} sent frame(s), got {}",
                    frames.len()
                );
            }
            thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn test_config_default() {
        let config = MitControllerConfig::default();
        assert_eq!(config.kp_gains[0], 5.0);
        assert_eq!(config.kd_gains[0], 0.8);
        assert_eq!(config.safe_hold_kp_gains, [5.0; 6]);
        assert_eq!(config.safe_hold_kd_gains, [0.8; 6]);
        assert!(config.rest_position.is_none());
        assert_eq!(config.control_rate, 200.0);
        assert_eq!(config.read_policy, ControlReadPolicy::default());
        assert_eq!(
            config.read_policy.max_feedback_age,
            crate::observer::DEFAULT_CONTROL_MAX_FEEDBACK_AGE
        );
    }

    #[test]
    fn test_finalize_tick_schedule_sleeps_until_future_deadline() {
        let now = Instant::now();
        let period = Duration::from_millis(5);
        let deadline = now + Duration::from_millis(2);

        let schedule = finalize_tick_schedule(now, deadline, period);

        assert_eq!(schedule.sleep_duration, Some(Duration::from_millis(2)));
        assert_eq!(schedule.next_cycle_deadline, deadline + period);
        assert_eq!(schedule.overrun_lateness, Duration::ZERO);
    }

    #[test]
    fn test_finalize_tick_schedule_advances_without_rephasing_after_overrun() {
        let now = Instant::now();
        let period = Duration::from_millis(5);
        let deadline = now - Duration::from_millis(7);

        let schedule = finalize_tick_schedule(now, deadline, period);

        assert!(schedule.sleep_duration.is_none());
        assert!(schedule.overrun_lateness >= Duration::from_millis(7));
        assert!(schedule.next_cycle_deadline > now);
        assert_eq!(
            schedule.next_cycle_deadline.duration_since(deadline).as_millis() % 5,
            0
        );
    }

    #[test]
    fn test_classify_command_cycle_resets_error_count_after_success() {
        assert_eq!(
            classify_command_cycle(true, 4, 5),
            CommandCycleDisposition::CheckReached {
                next_error_count: 0
            }
        );
    }

    #[test]
    fn test_transient_failure_cycle_still_advances_anchored_deadline() {
        let now = Instant::now();
        let period = Duration::from_millis(5);
        let deadline = now + Duration::from_millis(2);

        assert_eq!(
            classify_command_cycle(false, 0, 5),
            CommandCycleDisposition::MissedCycle {
                next_error_count: 1
            }
        );

        let schedule = finalize_tick_schedule(now, deadline, period);
        assert_eq!(schedule.next_cycle_deadline, deadline + period);
    }

    #[test]
    fn test_classify_command_cycle_aborts_on_sixth_consecutive_failure() {
        assert_eq!(
            classify_command_cycle(false, 5, 5),
            CommandCycleDisposition::Abort { failure_count: 6 }
        );
    }

    #[test]
    fn repeated_warning_state_keeps_faults_rate_limited_across_recovery_edges() {
        let mut warnings = HotPathDiagnostics::default();
        let interval = Duration::from_secs(1);
        let start = Instant::now();

        assert_eq!(
            warnings.record_fault(start, interval),
            FaultLogDecision::Emit {
                suppressed_repeats: 0
            }
        );
        let recovered_at = start + Duration::from_millis(5);
        warnings.record_recovery(recovered_at);

        assert_eq!(
            warnings.record_fault(start + Duration::from_millis(5), interval),
            FaultLogDecision::Suppress,
            "fault warnings must stay rate-limited even when the loop briefly recovers",
        );
    }

    #[test]
    fn repeated_warning_state_recovery_does_not_emit_hot_path_signal() {
        let mut warnings = HotPathDiagnostics::default();
        let interval = Duration::from_secs(1);
        let start = Instant::now();

        assert_eq!(
            warnings.record_fault(start, interval),
            FaultLogDecision::Emit {
                suppressed_repeats: 0
            }
        );
        let recovered_at = start + Duration::from_millis(5);
        warnings.record_recovery(recovered_at);
        assert_eq!(
            warnings.poll_recovery_summary(recovered_at, interval),
            None,
            "recovery summaries must wait for the diagnostics window even for the first batch",
        );
        assert_eq!(
            warnings.poll_recovery_summary(recovered_at + interval, interval),
            Some(RecoverySummary {
                recovery_count: 1,
                suppressed_fault_warnings: 0,
            }),
            "recovery should only be summarized later from a cold path",
        );
    }

    #[test]
    fn collect_pending_diagnostics_summarizes_send_failure_recovery_once() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let active = build_active_mit_piper(sent_frames, Duration::ZERO);
        let (event_tx, event_rx) = bounded(4);
        let mut controller = MitController::new_with_dispatcher(
            active,
            MitControllerConfig::default(),
            MitDiagnosticDispatcher::for_test(event_tx),
        )
        .expect("controller should build");
        let start = Instant::now();

        let _ = controller.send_failure_diagnostics.record_fault(start, DIAGNOSTIC_LOG_INTERVAL);
        let recovered_at = start + Duration::from_millis(5);
        controller.send_failure_diagnostics.record_recovery(recovered_at);

        assert_eq!(controller.submit_windowed_diagnostics_at(start), 0);
        assert_eq!(
            controller.submit_windowed_diagnostics_at(recovered_at + DIAGNOSTIC_LOG_INTERVAL),
            0
        );
        assert_eq!(
            event_rx.recv().expect("summary should be submitted asynchronously"),
            MitDiagnosticEvent::SendFailureRecovery(RecoverySummary {
                recovery_count: 1,
                suppressed_fault_warnings: 0,
            })
        );
        assert!(event_rx.try_recv().is_err());
    }

    #[test]
    fn enter_safe_state_force_flushes_pending_diagnostics_inside_summary_window() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let active = build_active_mit_piper(sent_frames, Duration::ZERO);
        let (event_tx, event_rx) = bounded(8);
        let mut controller = MitController::new_with_dispatcher(
            active,
            MitControllerConfig::default(),
            MitDiagnosticDispatcher::for_test(event_tx),
        )
        .expect("controller should build");
        let start = Instant::now();

        let _ = controller.send_failure_diagnostics.record_fault(start, DIAGNOSTIC_LOG_INTERVAL);
        let recovered_at = start + Duration::from_millis(5);
        controller.send_failure_diagnostics.record_recovery(recovered_at);
        assert_eq!(controller.submit_windowed_diagnostics_at(start), 0);
        assert_eq!(
            controller.submit_windowed_diagnostics_at(recovered_at + DIAGNOSTIC_LOG_INTERVAL),
            0
        );
        assert_eq!(
            event_rx.recv().expect("matured summary should reach async sink"),
            MitDiagnosticEvent::SendFailureRecovery(RecoverySummary {
                recovery_count: 1,
                suppressed_fault_warnings: 0,
            })
        );

        let _ = controller
            .send_failure_diagnostics
            .record_fault(start + Duration::from_millis(10), DIAGNOSTIC_LOG_INTERVAL);
        let recovered_at = start + Duration::from_millis(10);
        controller.send_failure_diagnostics.record_recovery(recovered_at);
        assert_eq!(
            controller.submit_windowed_diagnostics_at(start + Duration::from_millis(20)),
            0,
            "normal flush must stay silent inside the summary window",
        );

        let error = controller.enter_safe_state(RobotError::feedback_stale(
            Duration::from_millis(20),
            Duration::from_millis(1),
        ));
        assert!(matches!(error, ControlError::SafedOut { .. }));
        assert_eq!(
            controller.force_flush_pending_diagnostics_at(Instant::now()),
            0,
            "safe-state transition must force-flush pending recovery summaries",
        );
        assert_eq!(
            event_rx
                .recv()
                .expect("forced flush should submit pending summary to async sink"),
            MitDiagnosticEvent::SendFailureRecovery(RecoverySummary {
                recovery_count: 1,
                suppressed_fault_warnings: 1,
            })
        );
    }

    #[test]
    fn poll_windowed_diagnostics_emits_recovery_before_controller_exit() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let active = build_active_mit_piper(sent_frames, Duration::ZERO);
        let (event_tx, event_rx) = bounded(4);
        let dispatcher = MitDiagnosticDispatcher::for_test(event_tx);
        let mut controller =
            MitController::new_with_dispatcher(active, MitControllerConfig::default(), dispatcher)
                .expect("controller should build");
        let start = Instant::now();

        let _ = controller.send_failure_diagnostics.record_fault(start, DIAGNOSTIC_LOG_INTERVAL);
        let _ = controller
            .send_failure_diagnostics
            .record_fault(start + Duration::from_millis(5), DIAGNOSTIC_LOG_INTERVAL);
        let recovered_at = start + Duration::from_millis(5);
        controller.send_failure_diagnostics.record_recovery(recovered_at);

        assert_eq!(
            controller.submit_windowed_diagnostics_at(start + Duration::from_millis(10)),
            0,
            "the first runtime recovery summary must stay silent until the diagnostics window matures",
        );
        assert_eq!(
            controller.submit_windowed_diagnostics_at(recovered_at + DIAGNOSTIC_LOG_INTERVAL),
            0,
            "mature window submission should enqueue without dropping diagnostics",
        );
        assert_eq!(
            event_rx.recv().expect("runtime polling should submit a recovery event"),
            MitDiagnosticEvent::SendFailureRecovery(RecoverySummary {
                recovery_count: 1,
                suppressed_fault_warnings: 1,
            }),
            "long-running motion must surface recovery summaries before the controller exits",
        );
        assert_eq!(
            controller.force_flush_pending_diagnostics_at(start + Duration::from_millis(20)),
            0,
            "once runtime polling emitted the summary, forced flush should have nothing left to drain",
        );
        assert!(event_rx.try_recv().is_err());
    }

    #[test]
    fn queued_dropped_runtime_summaries_are_reported_before_next_successful_submission() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let active = build_active_mit_piper(sent_frames, Duration::ZERO);
        let (event_tx, event_rx) = bounded(1);
        let dispatcher = MitDiagnosticDispatcher::for_test(event_tx);
        let mut controller =
            MitController::new_with_dispatcher(active, MitControllerConfig::default(), dispatcher)
                .expect("controller should build");
        let start = Instant::now();

        let _ = controller.send_failure_diagnostics.record_fault(start, DIAGNOSTIC_LOG_INTERVAL);
        controller
            .send_failure_diagnostics
            .record_recovery(start + Duration::from_millis(5));

        let _ = controller.submit_windowed_diagnostics_at(start + DIAGNOSTIC_LOG_INTERVAL);

        let _ = controller
            .overrun_diagnostics
            .record_fault(start + Duration::from_millis(10), DIAGNOSTIC_LOG_INTERVAL);
        controller
            .overrun_diagnostics
            .record_recovery(start + Duration::from_millis(15));
        let _ = controller.submit_windowed_diagnostics_at(
            start + Duration::from_millis(15) + DIAGNOSTIC_LOG_INTERVAL,
        );
        assert_eq!(controller.dropped_recovery_events, 1);

        drop(event_rx);
        let (event_tx, event_rx) = bounded(4);
        controller.diagnostic_dispatcher = MitDiagnosticDispatcher::for_test(event_tx);
        let _ = controller
            .send_failure_diagnostics
            .record_fault(start + Duration::from_millis(20), DIAGNOSTIC_LOG_INTERVAL);
        controller
            .send_failure_diagnostics
            .record_recovery(start + Duration::from_millis(25));

        let _ = controller.submit_windowed_diagnostics_at(
            start + Duration::from_millis(25) + DIAGNOSTIC_LOG_INTERVAL,
        );
        assert_eq!(
            event_rx.recv().expect("dropped count must be replayed first"),
            MitDiagnosticEvent::DroppedRecoveryEvents { count: 1 }
        );
        assert_eq!(
            event_rx.recv().expect("current recovery must follow the dropped count"),
            MitDiagnosticEvent::SendFailureRecovery(RecoverySummary {
                recovery_count: 1,
                suppressed_fault_warnings: 1,
            })
        );
    }

    #[test]
    fn disabled_dispatcher_does_not_restore_sync_hot_path_logging() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let active = build_active_mit_piper(sent_frames, Duration::ZERO);
        let mut controller = MitController::new_with_dispatcher(
            active,
            MitControllerConfig::default(),
            MitDiagnosticDispatcher::disabled_for_test(),
        )
        .expect("controller should build");
        let start = Instant::now();

        let _ = controller.send_failure_diagnostics.record_fault(start, DIAGNOSTIC_LOG_INTERVAL);
        controller
            .send_failure_diagnostics
            .record_recovery(start + Duration::from_millis(5));
        let recovered_at = start + Duration::from_millis(5);

        assert_eq!(controller.submit_windowed_diagnostics_at(recovered_at), 0);
        assert_eq!(
            controller.submit_windowed_diagnostics_at(recovered_at + DIAGNOSTIC_LOG_INTERVAL),
            1,
            "runtime maturity must degrade to dropped accounting instead of synchronous logging",
        );
    }

    #[test]
    fn runtime_disconnect_refreshes_dispatcher_and_retries_once() {
        let _guard = crate::control::mit_diagnostic_dispatcher::global_test_guard();
        crate::control::mit_diagnostic_dispatcher::reset_global_dispatcher_for_test();

        let (first_tx, first_rx) = bounded(1);
        let (second_tx, second_rx) = bounded(4);
        let attempts = Arc::new(Mutex::new(VecDeque::from([
            Ok(MitDiagnosticDispatcher::for_test(first_tx)),
            Ok(MitDiagnosticDispatcher::for_test(second_tx)),
        ])));
        crate::control::mit_diagnostic_dispatcher::set_global_builder_for_test({
            let attempts = Arc::clone(&attempts);
            move || {
                attempts
                    .lock()
                    .expect("test builder attempts")
                    .pop_front()
                    .expect("expected another builder attempt")
            }
        });

        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let active = build_active_mit_piper(sent_frames, Duration::ZERO);
        let dispatcher = crate::control::mit_diagnostic_dispatcher::global_dispatcher()
            .expect("dispatcher should build");
        let mut controller =
            MitController::new_with_dispatcher(active, MitControllerConfig::default(), dispatcher)
                .expect("controller should build");
        let start = Instant::now();

        let _ = controller.send_failure_diagnostics.record_fault(start, DIAGNOSTIC_LOG_INTERVAL);
        let recovered_at = start + Duration::from_millis(5);
        controller.send_failure_diagnostics.record_recovery(recovered_at);

        drop(first_rx);
        assert_eq!(
            controller.submit_windowed_diagnostics_at(recovered_at + DIAGNOSTIC_LOG_INTERVAL),
            0,
            "controller should refresh the dispatcher and replay the matured summary once",
        );
        assert_eq!(
            second_rx
                .recv_timeout(Duration::from_millis(50))
                .expect("rebuilt dispatcher must receive the retried summary"),
            MitDiagnosticEvent::SendFailureRecovery(RecoverySummary {
                recovery_count: 1,
                suppressed_fault_warnings: 0,
            })
        );
        assert_eq!(
            controller.dropped_recovery_events, 0,
            "successful refresh-and-retry must not degrade into dropped diagnostics",
        );
    }

    #[test]
    fn mit_controller_new_rejects_non_positive_or_non_finite_control_rate() {
        for control_rate in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            let active = build_active_mit_piper(Arc::new(Mutex::new(Vec::new())), Duration::ZERO);
            let error = MitController::new(
                active,
                MitControllerConfig {
                    control_rate,
                    ..MitControllerConfig::default()
                },
            )
            .err()
            .expect("invalid control_rate must be rejected during construction");
            assert!(matches!(error, RobotError::ConfigError(_)));
        }
    }

    #[test]
    fn mit_controller_new_rejects_invalid_motion_gains() {
        for invalid_config in [
            MitControllerConfig {
                kp_gains: [-0.1, 5.0, 5.0, 5.0, 5.0, 5.0],
                ..MitControllerConfig::default()
            },
            MitControllerConfig {
                kd_gains: [0.8, 0.8, f64::NAN, 0.8, 0.8, 0.8],
                ..MitControllerConfig::default()
            },
        ] {
            let active = build_active_mit_piper(Arc::new(Mutex::new(Vec::new())), Duration::ZERO);
            let error = MitController::new(active, invalid_config)
                .err()
                .expect("invalid motion gains must be rejected during construction");
            assert!(matches!(error, RobotError::ConfigError(_)));
        }
    }

    #[test]
    fn mit_controller_new_rejects_invalid_safe_hold_gains() {
        for invalid_config in [
            MitControllerConfig {
                safe_hold_kp_gains: [5.0, 5.0, 5.0, f64::INFINITY, 5.0, 5.0],
                ..MitControllerConfig::default()
            },
            MitControllerConfig {
                safe_hold_kd_gains: [0.8, 0.8, 0.8, 0.8, -0.1, 0.8],
                ..MitControllerConfig::default()
            },
        ] {
            let active = build_active_mit_piper(Arc::new(Mutex::new(Vec::new())), Duration::ZERO);
            let error = MitController::new(active, invalid_config)
                .err()
                .expect("invalid safe-hold gains must be rejected during construction");
            assert!(matches!(error, RobotError::ConfigError(_)));
        }
    }

    #[test]
    fn move_to_position_read_failure_with_anchor_enters_safe_hold_and_latches_safed_out() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let active = build_active_mit_piper(sent_frames.clone(), Duration::ZERO);
        let mut controller = MitController::new(
            active,
            MitControllerConfig {
                kp_gains: [1.2; 6],
                kd_gains: [0.3; 6],
                safe_hold_kp_gains: [6.0; 6],
                safe_hold_kd_gains: [0.9; 6],
                rest_position: Some([Rad(0.0); 6]),
                read_policy: ControlReadPolicy {
                    max_state_skew_us: 2_000,
                    max_feedback_age: Duration::from_millis(50),
                },
                ..MitControllerConfig::default()
            },
        )
        .expect("strict realtime driver should support MitController");
        controller.config.read_policy.max_feedback_age = Duration::from_millis(5);
        thread::sleep(Duration::from_millis(10));
        controller.last_hold_anchor = Some(JointArray::from([Rad(0.0); 6]));

        let error = controller
            .move_to_position([Rad(0.0); 6], Rad(0.01), Duration::from_millis(50))
            .expect_err("stale control snapshot must safe-out the controller");

        match &error {
            ControlError::SafedOut { action, source } => {
                assert_eq!(*action, SafeAction::HoldPosition);
                assert!(matches!(source.as_ref(), RobotError::FeedbackStale { .. }));
            },
            other => panic!("expected SafedOut, got {other:?}"),
        }
        assert!(controller.is_safed_out());

        let frames = wait_for_sent_frames(&sent_frames, 6);
        let safe_hold = MitControlCommand::try_new(1, 0.0, 0.0, 6.0, 0.9, 0.0)
            .expect("safe-hold command should build")
            .to_frame();
        assert!(
            frames
                .iter()
                .any(|frame| frame.id == safe_hold.id && frame.data == safe_hold.data),
            "safe-hold package must reach the TX path before returning SafedOut"
        );
        assert!(
            !frames.contains(&EmergencyStopCommand::emergency_stop().to_frame()),
            "successful safe-hold must not fall back to emergency stop",
        );

        assert!(matches!(
            controller
                .move_to_position([Rad(0.0); 6], Rad(0.01), Duration::from_millis(10))
                .expect_err("safed-out controller must reject future motion"),
            ControlError::SafedOut {
                action: SafeAction::HoldPosition,
                ..
            }
        ));
        assert!(matches!(
            controller
                .move_to_rest(Rad(0.01), Duration::from_millis(10))
                .expect_err("safed-out controller must reject move_to_rest"),
            ControlError::SafedOut {
                action: SafeAction::HoldPosition,
                ..
            }
        ));
        assert!(matches!(
            controller
                .relax_joints()
                .expect_err("safed-out controller must reject relax_joints"),
            ControlError::SafedOut {
                action: SafeAction::HoldPosition,
                ..
            }
        ));

        let standby = controller
            .park(DisableConfig::default())
            .expect("park should remain available after safe-out");
        let frames = wait_for_sent_frames(&sent_frames, 7);
        assert!(frames.contains(&MotorEnableCommand::disable_all().to_frame()));
        drop(standby);
    }

    #[test]
    fn mit_controller_new_waits_for_initial_control_snapshot_ready() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let active = build_active_mit_piper_with_adapters(
            DelayedScriptedRxAdapter::new(delayed_control_snapshot_frames(
                1_000,
                Duration::from_millis(25),
            )),
            RecordingTxAdapter::new(sent_frames),
            Duration::ZERO,
        );

        let mut controller = MitController::new(
            active,
            MitControllerConfig {
                read_policy: ControlReadPolicy {
                    max_feedback_age: Duration::from_millis(50),
                    ..ControlReadPolicy::default()
                },
                ..MitControllerConfig::default()
            },
        )
        .expect("controller construction should wait for the first aligned control snapshot");
        let reached = controller
            .move_to_position([Rad(0.0); 6], Rad(0.01), Duration::from_millis(50))
            .expect("fresh controller should not safe-out on an initial cold snapshot gap");

        assert!(reached);
    }

    #[test]
    fn move_to_position_read_failure_without_anchor_falls_back_to_emergency_stop() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let active = build_active_mit_piper(sent_frames.clone(), Duration::ZERO);
        let mut controller = MitController::new(
            active,
            MitControllerConfig {
                read_policy: ControlReadPolicy {
                    max_state_skew_us: 2_000,
                    max_feedback_age: Duration::from_millis(50),
                },
                ..MitControllerConfig::default()
            },
        )
        .expect("strict realtime driver should support MitController");
        controller.config.read_policy.max_feedback_age = Duration::from_millis(5);
        thread::sleep(Duration::from_millis(10));

        let error = controller
            .move_to_position([Rad(0.0); 6], Rad(0.01), Duration::from_millis(50))
            .expect_err("missing anchor must fall back to emergency stop");

        assert!(matches!(
            error,
            ControlError::SafedOut {
                action: SafeAction::EmergencyStop,
                ..
            }
        ));
        assert!(controller.is_safed_out());

        let frames = wait_for_sent_frames(&sent_frames, 7);
        assert!(frames.contains(&EmergencyStopCommand::emergency_stop().to_frame()));
    }

    #[test]
    fn move_to_position_safe_hold_send_failure_falls_back_to_emergency_stop() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let active = build_active_mit_piper(sent_frames.clone(), Duration::from_millis(5));
        let mut controller = MitController::new(active, MitControllerConfig::default())
            .expect("strict realtime driver should support MitController");
        controller.last_hold_anchor = Some(JointArray::from([Rad(100.0); 6]));

        let error = controller.enter_safe_state(RobotError::feedback_stale(
            Duration::from_millis(20),
            Duration::from_millis(1),
        ));

        assert!(matches!(
            error,
            ControlError::SafedOut {
                action: SafeAction::EmergencyStop,
                ..
            }
        ));

        let frames = wait_for_sent_frames(&sent_frames, 1);
        assert!(frames.contains(&EmergencyStopCommand::emergency_stop().to_frame()));
    }

    #[test]
    fn move_to_position_sixth_consecutive_send_failure_safes_out_instead_of_returning_consecutive_failures()
     {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let active = build_active_mit_piper(sent_frames.clone(), Duration::from_millis(5));
        let mut controller = MitController::new(active, MitControllerConfig::default())
            .expect("strict realtime driver should support MitController");
        controller.last_hold_anchor = Some(JointArray::from([Rad(0.0); 6]));
        controller
            .piper
            .as_ref()
            .expect("controller should still hold active piper")
            .driver
            .latch_fault();

        let start = Instant::now();

        let error = controller
            .move_to_position([Rad(0.1); 6], Rad(0.001), Duration::from_millis(80))
            .expect_err("sixth consecutive send failure must safe-out the controller");

        assert!(!matches!(error, ControlError::ConsecutiveFailures { .. }));
        assert!(matches!(error, ControlError::SafedOut { .. }));
        assert!(controller.is_safed_out());
        assert!(
            start.elapsed() >= Duration::from_millis(20),
            "controller should tolerate the first five failed cycles instead of bailing out immediately"
        );

        let frames = wait_for_sent_frames(&sent_frames, 1);
        assert!(
            frames.contains(&EmergencyStopCommand::emergency_stop().to_frame()),
            "safe-out after repeated control failures must end on shutdown-lane emergency stop once the control path is faulted",
        );
    }
}
