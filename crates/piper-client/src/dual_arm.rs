//! 双臂 MIT 协调控制模块
//!
//! 采用软件协调的双臂架构：
//! - 两条机械臂各自保留独立 driver runtime
//! - 双臂层只负责高层状态协调、控制循环和安全策略
//! - 首版只支持两条独立 CAN 适配器/总线

use std::convert::Infallible;
use std::error::Error;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use piper_driver::{DriverError, RuntimeFaultKind};
use thiserror::Error;

use crate::builder::PiperBuilder;
use crate::control::scheduler::{CycleScheduler, SleepStrategy};
use crate::observer::{ControlReadPolicy, ControlSnapshotFull, Observer, RuntimeHealthSnapshot};
use crate::raw_commander::RawCommander;
use crate::state::machine::ErrorState;
use crate::state::{Active, DisableConfig, MitMode, MitModeConfig, Piper, Standby};
use crate::types::{Joint, JointArray, NewtonMeter, Rad, Result, RobotError};

/// 双臂构建器
pub struct DualArmBuilder {
    left: PiperBuilder,
    right: PiperBuilder,
}

impl DualArmBuilder {
    pub fn new(left: PiperBuilder, right: PiperBuilder) -> Self {
        Self { left, right }
    }

    pub fn build(self) -> Result<DualArmStandby> {
        let left = self.left.build()?;
        let right = self.right.build()?;
        Ok(DualArmStandby { left, right })
    }
}

/// 双臂待机态
pub struct DualArmStandby {
    left: Piper<Standby>,
    right: Piper<Standby>,
}

impl DualArmStandby {
    pub fn enable_mit(
        self,
        left_cfg: MitModeConfig,
        right_cfg: MitModeConfig,
    ) -> Result<DualArmActiveMit> {
        let left = self.left.enable_mit_mode(left_cfg)?;
        let right = self.right.enable_mit_mode(right_cfg)?;
        Ok(DualArmActiveMit { left, right })
    }

    pub fn observer(&self) -> DualArmObserver {
        DualArmObserver::new(self.left.observer().clone(), self.right.observer().clone())
    }

    pub fn capture_calibration(&self, map: JointMirrorMap) -> Result<DualArmCalibration> {
        self.capture_calibration_with_policy(map, calibration_read_policy())
    }

    pub fn capture_calibration_with_policy(
        &self,
        map: JointMirrorMap,
        policy: DualArmReadPolicy,
    ) -> Result<DualArmCalibration> {
        map.validate()?;
        let snapshot = self.observer().snapshot(policy)?;
        Ok(DualArmCalibration {
            master_zero: snapshot.left.state.position,
            slave_zero: snapshot.right.state.position,
            map,
        })
    }
}

/// 双臂 MIT 运行态
pub struct DualArmActiveMit {
    left: Piper<Active<MitMode>>,
    right: Piper<Active<MitMode>>,
}

impl DualArmActiveMit {
    pub fn observer(&self) -> DualArmObserver {
        DualArmObserver::new(self.left.observer().clone(), self.right.observer().clone())
    }

    pub fn safe_hold(&self, anchor: &DualArmHoldAnchor, cfg: &DualArmSafetyConfig) -> Result<()> {
        self.safe_hold_from_anchor(anchor, cfg, Instant::now())
    }

    pub fn disable_both(self, cfg: DisableConfig) -> Result<DualArmStandby> {
        let left = self.left.disable(cfg.clone())?;
        let right = self.right.disable(cfg)?;
        Ok(DualArmStandby { left, right })
    }

    pub fn emergency_stop_both(self) -> Result<DualArmErrorState> {
        let left = self.left.emergency_stop()?;
        let right = self.right.emergency_stop()?;
        Ok(DualArmErrorState { left, right })
    }

    pub fn run_bilateral<C>(
        self,
        controller: C,
        cfg: BilateralLoopConfig,
    ) -> std::result::Result<DualArmLoopExit, DualArmError>
    where
        C: BilateralController,
    {
        self.run_bilateral_inner(controller, None, cfg)
    }

    pub fn run_bilateral_with_compensation<C, D>(
        self,
        controller: C,
        compensator: D,
        cfg: BilateralLoopConfig,
    ) -> std::result::Result<DualArmLoopExit, DualArmError>
    where
        C: BilateralController,
        D: BilateralDynamicsCompensator,
    {
        let mut adapter = CompensatorAdapter::new(compensator);
        adapter.reset().map_err(DualArmError::Compensation)?;
        self.run_bilateral_inner(controller, Some(&mut adapter), cfg)
    }

    fn run_bilateral_inner<C>(
        self,
        mut controller: C,
        mut compensator: Option<&mut dyn InternalBilateralDynamicsCompensator>,
        cfg: BilateralLoopConfig,
    ) -> std::result::Result<DualArmLoopExit, DualArmError>
    where
        C: BilateralController,
    {
        if cfg.frequency_hz <= 0.0 {
            return Err(DualArmError::Config("frequency_hz must be > 0".to_string()));
        }
        if cfg.dt_clamp_multiplier <= 0.0 {
            return Err(DualArmError::Config(
                "dt_clamp_multiplier must be > 0".to_string(),
            ));
        }
        if cfg.gripper.update_divider == 0 {
            return Err(DualArmError::Config(
                "gripper.update_divider must be >= 1".to_string(),
            ));
        }

        let nominal_period = Duration::from_secs_f64(1.0 / cfg.frequency_hz);
        let max_dt = nominal_period.mul_f64(cfg.dt_clamp_multiplier);
        let active = self;
        let mut report = BilateralRunReport::default();
        let mut shaping_state = OutputShapingState::default();
        let mut scheduler = CycleScheduler::new(
            nominal_period,
            sleep_strategy_from_loop_timing(cfg.timing_mode),
        );
        let mut iteration = 0usize;
        let mut read_failure_streak = 0u32;
        let mut read_failure_since: Option<Instant> = None;
        let mut compensation_failure_streak = 0u32;
        let mut gripper_counter = 0usize;
        let mut hold_anchor: Option<DualArmHoldAnchor> = None;

        loop {
            if let Some(max_iterations) = cfg.max_iterations
                && iteration >= max_iterations
            {
                report.exit_reason = Some(BilateralExitReason::MaxIterations);
                let arms =
                    active.disable_both(cfg.disable_config.clone()).map_err(DualArmError::from)?;
                update_report_metrics(&mut report, &arms.left.driver, &arms.right.driver);
                return Ok(DualArmLoopExit::Standby { arms, report });
            }

            if let Some(cancel_signal) = &cfg.cancel_signal
                && cancel_signal.load(Ordering::Acquire)
            {
                report.exit_reason = Some(BilateralExitReason::Cancelled);
                report.last_error = Some("bilateral loop cancelled".to_string());
                let arms =
                    active.disable_both(cfg.disable_config.clone()).map_err(DualArmError::from)?;
                update_report_metrics(&mut report, &arms.left.driver, &arms.right.driver);
                return Ok(DualArmLoopExit::Standby { arms, report });
            }

            let cycle = scheduler.wait_next();
            report.deadline_misses += cycle.missed_deadlines;
            report.max_real_dt = report.max_real_dt.max(cycle.real_dt);
            report.max_cycle_lag = report.max_cycle_lag.max(cycle.lag);

            let now = cycle.tick_start;
            let real_dt = cycle.real_dt;
            let mut dt = real_dt;
            if real_dt > max_dt {
                if let Err(err) = controller.on_time_jump(real_dt) {
                    report.exit_reason = Some(BilateralExitReason::ControllerFault);
                    report.last_error = Some(err.to_string());
                    let _ = best_effort_hold_from_anchor(
                        &active,
                        hold_anchor,
                        Instant::now(),
                        &cfg.safety,
                    );
                    let arms = active
                        .disable_both(cfg.disable_config.clone())
                        .map_err(DualArmError::from)?;
                    update_report_metrics(&mut report, &arms.left.driver, &arms.right.driver);
                    return Ok(DualArmLoopExit::Standby { arms, report });
                }
                if let Some(compensator) = compensator.as_deref_mut()
                    && let Err(err) = compensator.on_time_jump(real_dt)
                {
                    compensation_failure_streak += 1;
                    report.last_error = Some(err);
                    let hold_succeeded = best_effort_hold_from_anchor(
                        &active,
                        hold_anchor,
                        Instant::now(),
                        &cfg.safety,
                    );
                    if !hold_succeeded || compensation_failure_streak > 1 {
                        report.exit_reason = Some(BilateralExitReason::CompensationFault);
                        let arms = active
                            .disable_both(cfg.disable_config.clone())
                            .map_err(DualArmError::from)?;
                        update_report_metrics(&mut report, &arms.left.driver, &arms.right.driver);
                        return Ok(DualArmLoopExit::Standby { arms, report });
                    }

                    report.iterations += 1;
                    iteration += 1;
                    continue;
                }
                dt = max_dt;
            }

            // Steady-state MIT commands stay unconfirmed to minimize loop jitter.
            // Real TX/transport failures are expected to surface here on the next cycle.
            let health = active.observer().runtime_health();
            if classify_runtime_transport_fault(health) {
                report.exit_reason = Some(BilateralExitReason::RuntimeTransportFault);
                report.last_runtime_fault_left = health.left.fault;
                report.last_runtime_fault_right = health.right.fault;
                report.last_error = Some(format_runtime_health_error(health));
                let (arms, shutdown) = active.fault_shutdown(FAULT_SHUTDOWN_TIMEOUT);
                report.left_stop_attempt = shutdown.left_stop_attempt;
                report.right_stop_attempt = shutdown.right_stop_attempt;
                update_report_metrics(&mut report, &arms.left.driver, &arms.right.driver);
                return Ok(DualArmLoopExit::Faulted { arms, report });
            }

            let snapshot = match active.observer().snapshot(cfg.read_policy) {
                Ok(snapshot) => {
                    hold_anchor = Some(DualArmHoldAnchor::from_snapshot(&snapshot));
                    read_failure_streak = 0;
                    read_failure_since = None;
                    report.max_inter_arm_skew =
                        report.max_inter_arm_skew.max(snapshot.inter_arm_skew);
                    snapshot
                },
                Err(err) => {
                    read_failure_streak += 1;
                    report.read_faults += 1;
                    report.last_error = Some(err.to_string());
                    let failure_start = read_failure_since.get_or_insert(now);
                    let hold_succeeded = best_effort_hold_from_anchor(
                        &active,
                        hold_anchor,
                        Instant::now(),
                        &cfg.safety,
                    );
                    if !hold_succeeded
                        || read_failure_streak
                            >= cfg.safety.consecutive_read_failures_before_disable
                        || now.saturating_duration_since(*failure_start)
                            >= cfg.safety.safe_hold_max_duration
                    {
                        report.exit_reason = Some(BilateralExitReason::ReadFault);
                        let arms = active
                            .disable_both(cfg.disable_config.clone())
                            .map_err(DualArmError::from)?;
                        update_report_metrics(&mut report, &arms.left.driver, &arms.right.driver);
                        return Ok(DualArmLoopExit::Standby { arms, report });
                    }

                    report.iterations += 1;
                    iteration += 1;
                    continue;
                },
            };

            if iteration < cfg.warmup_cycles {
                let anchor = DualArmHoldAnchor::from_snapshot(&snapshot);
                hold_anchor = Some(anchor);
                active.safe_hold(&anchor, &cfg.safety).map_err(DualArmError::from)?;
                report.iterations += 1;
                iteration += 1;
                continue;
            }

            let compensation = if let Some(compensator) = compensator.as_deref_mut() {
                match compensator.compute(&snapshot, dt) {
                    Ok(compensation) => {
                        compensation_failure_streak = 0;
                        Some(compensation)
                    },
                    Err(err) => {
                        compensation_failure_streak += 1;
                        report.last_error = Some(err);
                        let hold_succeeded = best_effort_hold_from_anchor(
                            &active,
                            hold_anchor,
                            Instant::now(),
                            &cfg.safety,
                        );
                        if !hold_succeeded || compensation_failure_streak > 1 {
                            report.exit_reason = Some(BilateralExitReason::CompensationFault);
                            let arms = active
                                .disable_both(cfg.disable_config.clone())
                                .map_err(DualArmError::from)?;
                            update_report_metrics(
                                &mut report,
                                &arms.left.driver,
                                &arms.right.driver,
                            );
                            return Ok(DualArmLoopExit::Standby { arms, report });
                        }

                        report.iterations += 1;
                        iteration += 1;
                        continue;
                    },
                }
            } else {
                None
            };

            let frame = BilateralControlFrame {
                snapshot,
                compensation,
            };

            let mut command = match controller.tick_with_compensation(&frame, dt) {
                Ok(command) => command,
                Err(err) => {
                    report.exit_reason = Some(BilateralExitReason::ControllerFault);
                    report.last_error = Some(err.to_string());
                    let _ = best_effort_hold_from_anchor(
                        &active,
                        hold_anchor,
                        Instant::now(),
                        &cfg.safety,
                    );
                    let arms = active
                        .disable_both(cfg.disable_config.clone())
                        .map_err(DualArmError::from)?;
                    update_report_metrics(&mut report, &arms.left.driver, &arms.right.driver);
                    return Ok(DualArmLoopExit::Standby { arms, report });
                },
            };

            apply_output_shaping(&cfg, &frame.snapshot, dt, &mut shaping_state, &mut command);
            let final_torques = assemble_final_torques(&command, frame.compensation);

            if let Err(err) = active.right.command_torques(
                &command.slave_position,
                &command.slave_velocity,
                &command.slave_kp,
                &command.slave_kd,
                &final_torques.slave,
            ) {
                report.submission_faults += 1;
                report.exit_reason = Some(BilateralExitReason::SubmissionFault);
                report.last_error = Some(err.to_string());
                let (arms, shutdown) = active.fault_shutdown(FAULT_SHUTDOWN_TIMEOUT);
                report.left_stop_attempt = shutdown.left_stop_attempt;
                report.right_stop_attempt = shutdown.right_stop_attempt;
                update_report_metrics(&mut report, &arms.left.driver, &arms.right.driver);
                return Ok(DualArmLoopExit::Faulted { arms, report });
            }

            if let Err(err) = active.left.command_torques(
                &command.master_position,
                &command.master_velocity,
                &command.master_kp,
                &command.master_kd,
                &final_torques.master,
            ) {
                report.submission_faults += 1;
                report.exit_reason = Some(BilateralExitReason::SubmissionFault);
                report.last_error = Some(err.to_string());
                let (arms, shutdown) = active.fault_shutdown(FAULT_SHUTDOWN_TIMEOUT);
                report.left_stop_attempt = shutdown.left_stop_attempt;
                report.right_stop_attempt = shutdown.right_stop_attempt;
                update_report_metrics(&mut report, &arms.left.driver, &arms.right.driver);
                return Ok(DualArmLoopExit::Faulted { arms, report });
            }

            gripper_counter += 1;
            if cfg.gripper.enabled && gripper_counter.is_multiple_of(cfg.gripper.update_divider) {
                let master_gripper = active.left.observer().gripper_state();
                let slave_gripper = active.right.observer().gripper_state();
                if (master_gripper.position - slave_gripper.position).abs()
                    >= cfg.gripper.position_deadband
                {
                    let _ = active.right.set_gripper(
                        master_gripper.position,
                        (master_gripper.effort * cfg.gripper.effort_scale).clamp(0.0, 1.0),
                    );
                }
            }

            report.iterations += 1;
            iteration += 1;
        }
    }

    fn safe_hold_from_anchor(
        &self,
        anchor: &DualArmHoldAnchor,
        cfg: &DualArmSafetyConfig,
        now: Instant,
    ) -> Result<()> {
        validate_hold_anchor(anchor, now, cfg.safe_hold_max_duration)?;
        command_hold(&self.right, &anchor.right_position, cfg)?;
        command_hold(&self.left, &anchor.left_position, cfg)?;
        Ok(())
    }

    fn fault_shutdown(self, timeout: Duration) -> (DualArmErrorState, FaultShutdown) {
        let deadline = Instant::now() + timeout;
        self.left.driver.latch_fault();
        self.right.driver.latch_fault();
        let left_pending = enqueue_stop_attempt(&self.left, deadline);
        let right_pending = enqueue_stop_attempt(&self.right, deadline);
        let left_stop_attempt = resolve_stop_attempt(left_pending);
        let right_stop_attempt = resolve_stop_attempt(right_pending);
        self.left.driver.request_stop();
        self.right.driver.request_stop();
        (
            DualArmErrorState {
                left: force_error_state(self.left),
                right: force_error_state(self.right),
            },
            FaultShutdown {
                left_stop_attempt,
                right_stop_attempt,
            },
        )
    }
}

/// 双臂错误态
pub struct DualArmErrorState {
    left: Piper<ErrorState>,
    right: Piper<ErrorState>,
}

impl DualArmErrorState {
    pub fn observer(&self) -> DualArmObserver {
        DualArmObserver::new(self.left.observer().clone(), self.right.observer().clone())
    }
}

/// 双臂观察器
#[derive(Clone)]
pub struct DualArmObserver {
    left: Observer,
    right: Observer,
}

impl DualArmObserver {
    pub fn new(left: Observer, right: Observer) -> Self {
        Self { left, right }
    }

    pub fn hold_anchor(&self, policy: DualArmReadPolicy) -> Result<DualArmHoldAnchor> {
        self.snapshot(policy)
            .map(|snapshot| DualArmHoldAnchor::from_snapshot(&snapshot))
    }

    pub fn snapshot(&self, policy: DualArmReadPolicy) -> Result<DualArmSnapshot> {
        let left = self.left.control_snapshot_full(policy.per_arm)?;
        let right = self.right.control_snapshot_full(policy.per_arm)?;

        let skew = compute_inter_arm_skew(&left, &right);
        if skew.effective > policy.max_inter_arm_skew {
            return Err(RobotError::state_misaligned(
                skew.signed_effective_skew_us,
                policy.max_inter_arm_skew.as_micros() as u64,
            ));
        }

        Ok(DualArmSnapshot {
            left,
            right,
            inter_arm_skew: skew.effective,
            host_cycle_timestamp: Instant::now(),
        })
    }

    pub fn runtime_health(&self) -> DualArmRuntimeHealth {
        DualArmRuntimeHealth {
            left: self.left.runtime_health(),
            right: self.right.runtime_health(),
        }
    }
}

/// 双臂读数策略
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DualArmReadPolicy {
    pub per_arm: ControlReadPolicy,
    pub max_inter_arm_skew: Duration,
}

impl Default for DualArmReadPolicy {
    fn default() -> Self {
        Self {
            per_arm: ControlReadPolicy {
                max_state_skew_us: 2_000,
                max_feedback_age: Duration::from_millis(15),
            },
            max_inter_arm_skew: Duration::from_millis(10),
        }
    }
}

/// 双臂控制快照
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DualArmSnapshot {
    pub left: ControlSnapshotFull,
    pub right: ControlSnapshotFull,
    pub inter_arm_skew: Duration,
    pub host_cycle_timestamp: Instant,
}

/// 双臂运行时健康状态
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DualArmRuntimeHealth {
    pub left: RuntimeHealthSnapshot,
    pub right: RuntimeHealthSnapshot,
}

impl DualArmRuntimeHealth {
    /// Diagnostic aggregate only.
    /// Control-loop fault exits use a narrower runtime transport classification.
    pub fn any_unhealthy(self) -> bool {
        !self.left.connected
            || !self.right.connected
            || !self.left.rx_alive
            || !self.left.tx_alive
            || !self.right.rx_alive
            || !self.right.tx_alive
            || self.left.fault.is_some()
            || self.right.fault.is_some()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BilateralExitReason {
    MaxIterations,
    Cancelled,
    ReadFault,
    ControllerFault,
    CompensationFault,
    SubmissionFault,
    RuntimeTransportFault,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StopAttemptResult {
    #[default]
    NotAttempted,
    ConfirmedSent,
    Timeout,
    ChannelClosed,
    QueueRejected,
    TransportFailed,
}

/// 双臂安全策略
#[derive(Debug, Clone)]
pub struct DualArmSafetyConfig {
    pub safe_hold_kp: JointArray<f64>,
    pub safe_hold_kd: JointArray<f64>,
    pub safe_hold_max_duration: Duration,
    pub consecutive_read_failures_before_disable: u32,
}

impl Default for DualArmSafetyConfig {
    fn default() -> Self {
        Self {
            safe_hold_kp: JointArray::splat(5.0),
            safe_hold_kd: JointArray::splat(0.8),
            safe_hold_max_duration: Duration::from_millis(100),
            consecutive_read_failures_before_disable: 3,
        }
    }
}

/// 夹爪镜像策略
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GripperTeleopConfig {
    pub enabled: bool,
    pub update_divider: usize,
    pub position_deadband: f64,
    pub effort_scale: f64,
}

impl Default for GripperTeleopConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            update_divider: 4,
            position_deadband: 0.02,
            effort_scale: 1.0,
        }
    }
}

/// 控制循环定时模式
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LoopTimingMode {
    #[cfg_attr(not(target_os = "linux"), default)]
    Sleep,
    #[cfg_attr(target_os = "linux", default)]
    Spin,
}

/// 双臂循环配置
#[derive(Debug, Clone)]
pub struct BilateralLoopConfig {
    pub frequency_hz: f64,
    pub dt_clamp_multiplier: f64,
    pub timing_mode: LoopTimingMode,
    pub warmup_cycles: usize,
    pub max_iterations: Option<usize>,
    pub cancel_signal: Option<Arc<AtomicBool>>,
    pub read_policy: DualArmReadPolicy,
    pub safety: DualArmSafetyConfig,
    pub disable_config: DisableConfig,
    pub gripper: GripperTeleopConfig,
    pub master_interaction_lpf_cutoff_hz: f64,
    pub master_interaction_limit: JointArray<NewtonMeter>,
    pub slave_feedforward_limit: JointArray<NewtonMeter>,
    pub master_interaction_slew_limit: JointArray<NewtonMeter>,
    pub master_passivity_enabled: bool,
    pub master_passivity_max_damping: JointArray<f64>,
}

impl Default for BilateralLoopConfig {
    fn default() -> Self {
        Self {
            frequency_hz: 200.0,
            dt_clamp_multiplier: 2.0,
            timing_mode: LoopTimingMode::default(),
            warmup_cycles: 3,
            max_iterations: None,
            cancel_signal: None,
            read_policy: DualArmReadPolicy::default(),
            safety: DualArmSafetyConfig::default(),
            disable_config: DisableConfig::default(),
            gripper: GripperTeleopConfig::default(),
            master_interaction_lpf_cutoff_hz: 20.0,
            master_interaction_limit: JointArray::splat(NewtonMeter(1.5)),
            slave_feedforward_limit: JointArray::splat(NewtonMeter(4.0)),
            master_interaction_slew_limit: JointArray::splat(NewtonMeter(0.25)),
            master_passivity_enabled: true,
            master_passivity_max_damping: JointArray::splat(1.0),
        }
    }
}

/// 双臂运行统计
#[derive(Debug, Clone, PartialEq)]
pub struct BilateralRunReport {
    pub iterations: usize,
    pub read_faults: u32,
    pub submission_faults: u32,
    pub deadline_misses: u64,
    pub max_inter_arm_skew: Duration,
    pub max_real_dt: Duration,
    pub max_cycle_lag: Duration,
    pub left_tx_realtime_overwrites_total: u64,
    pub right_tx_realtime_overwrites_total: u64,
    pub left_tx_frames_sent_total: u64,
    pub right_tx_frames_sent_total: u64,
    pub left_tx_fault_aborts_total: u64,
    pub right_tx_fault_aborts_total: u64,
    pub last_runtime_fault_left: Option<RuntimeFaultKind>,
    pub last_runtime_fault_right: Option<RuntimeFaultKind>,
    pub exit_reason: Option<BilateralExitReason>,
    pub left_stop_attempt: StopAttemptResult,
    pub right_stop_attempt: StopAttemptResult,
    pub last_error: Option<String>,
}

impl Default for BilateralRunReport {
    fn default() -> Self {
        Self {
            iterations: 0,
            read_faults: 0,
            submission_faults: 0,
            deadline_misses: 0,
            max_inter_arm_skew: Duration::ZERO,
            max_real_dt: Duration::ZERO,
            max_cycle_lag: Duration::ZERO,
            left_tx_realtime_overwrites_total: 0,
            right_tx_realtime_overwrites_total: 0,
            left_tx_frames_sent_total: 0,
            right_tx_frames_sent_total: 0,
            left_tx_fault_aborts_total: 0,
            right_tx_fault_aborts_total: 0,
            last_runtime_fault_left: None,
            last_runtime_fault_right: None,
            exit_reason: None,
            left_stop_attempt: StopAttemptResult::NotAttempted,
            right_stop_attempt: StopAttemptResult::NotAttempted,
            last_error: None,
        }
    }
}

/// 双臂循环退出结果
pub enum DualArmLoopExit {
    Standby {
        arms: DualArmStandby,
        report: BilateralRunReport,
    },
    Faulted {
        arms: DualArmErrorState,
        report: BilateralRunReport,
    },
}

/// 双臂模块错误
#[derive(Debug, Error)]
pub enum DualArmError {
    #[error("robot error: {0}")]
    Robot(#[from] RobotError),
    #[error("dual-arm configuration error: {0}")]
    Config(String),
    #[error("bilateral controller error: {0}")]
    Controller(String),
    #[error("bilateral compensator error: {0}")]
    Compensation(String),
}

/// 关节镜像映射
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct JointMirrorMap {
    pub permutation: [Joint; 6],
    pub position_sign: [f64; 6],
    pub velocity_sign: [f64; 6],
    pub torque_sign: [f64; 6],
}

impl JointMirrorMap {
    pub fn left_right_mirror() -> Self {
        Self {
            permutation: Joint::ALL,
            position_sign: [-1.0, 1.0, 1.0, -1.0, 1.0, -1.0],
            velocity_sign: [-1.0, 1.0, 1.0, -1.0, 1.0, -1.0],
            torque_sign: [-1.0, 1.0, 1.0, -1.0, 1.0, -1.0],
        }
    }

    fn validate(&self) -> Result<()> {
        let mut seen = [false; 6];
        for joint in self.permutation {
            let index = joint.index();
            if seen[index] {
                return Err(RobotError::ConfigError(
                    "mirror permutation must contain each joint exactly once".to_string(),
                ));
            }
            seen[index] = true;
        }

        for signs in [self.position_sign, self.velocity_sign, self.torque_sign] {
            for sign in signs {
                if !sign.is_finite() || (sign.abs() - 1.0).abs() > f64::EPSILON {
                    return Err(RobotError::ConfigError(
                        "mirror signs must be finite and equal to ±1.0".to_string(),
                    ));
                }
            }
        }

        Ok(())
    }
}

/// 双臂标定结果
#[derive(Debug, Clone, PartialEq)]
pub struct DualArmCalibration {
    pub master_zero: JointArray<Rad>,
    pub slave_zero: JointArray<Rad>,
    pub map: JointMirrorMap,
}

impl DualArmCalibration {
    pub fn master_to_slave_position(&self, master: JointArray<Rad>) -> JointArray<Rad> {
        JointArray::new(std::array::from_fn(|slave_index| {
            let source_joint = self.map.permutation[slave_index];
            let source_index = source_joint.index();
            Rad(self.map.position_sign[slave_index]
                * (master[source_index] - self.master_zero[source_index]).0
                + self.slave_zero[slave_index].0)
        }))
    }

    pub fn master_to_slave_velocity(
        &self,
        master: JointArray<crate::types::RadPerSecond>,
    ) -> JointArray<f64> {
        JointArray::new(std::array::from_fn(|slave_index| {
            let source_joint = self.map.permutation[slave_index];
            self.map.velocity_sign[slave_index] * master[source_joint].0
        }))
    }

    pub fn slave_to_master_torque(
        &self,
        slave: JointArray<NewtonMeter>,
    ) -> JointArray<NewtonMeter> {
        let mut master = JointArray::splat(NewtonMeter::ZERO);
        for slave_index in 0..6 {
            let master_joint = self.map.permutation[slave_index];
            master[master_joint] =
                NewtonMeter(self.map.torque_sign[slave_index] * slave[slave_index].0);
        }
        master
    }
}

/// 双臂控制器输出
#[derive(Debug, Clone, PartialEq)]
pub struct BilateralCommand {
    pub slave_position: JointArray<Rad>,
    pub slave_velocity: JointArray<f64>,
    pub slave_kp: JointArray<f64>,
    pub slave_kd: JointArray<f64>,
    pub slave_feedforward_torque: JointArray<NewtonMeter>,
    pub master_position: JointArray<Rad>,
    pub master_velocity: JointArray<f64>,
    pub master_kp: JointArray<f64>,
    pub master_kd: JointArray<f64>,
    pub master_interaction_torque: JointArray<NewtonMeter>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BilateralDynamicsCompensation {
    pub master_model_torque: JointArray<NewtonMeter>,
    pub slave_model_torque: JointArray<NewtonMeter>,
    pub master_external_torque_est: JointArray<NewtonMeter>,
    pub slave_external_torque_est: JointArray<NewtonMeter>,
}

impl Default for BilateralDynamicsCompensation {
    fn default() -> Self {
        Self {
            master_model_torque: JointArray::splat(NewtonMeter::ZERO),
            slave_model_torque: JointArray::splat(NewtonMeter::ZERO),
            master_external_torque_est: JointArray::splat(NewtonMeter::ZERO),
            slave_external_torque_est: JointArray::splat(NewtonMeter::ZERO),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BilateralControlFrame {
    pub snapshot: DualArmSnapshot,
    pub compensation: Option<BilateralDynamicsCompensation>,
}

pub trait BilateralDynamicsCompensator {
    type Error: Error + Send + 'static;

    fn compute(
        &mut self,
        snapshot: &DualArmSnapshot,
        dt: Duration,
    ) -> std::result::Result<BilateralDynamicsCompensation, Self::Error>;

    fn on_time_jump(&mut self, _dt: Duration) -> std::result::Result<(), Self::Error> {
        Ok(())
    }

    fn reset(&mut self) -> std::result::Result<(), Self::Error> {
        Ok(())
    }
}

/// 双臂控制器 trait
pub trait BilateralController {
    type Error: Error + Send + 'static;

    fn tick(
        &mut self,
        snapshot: &DualArmSnapshot,
        dt: Duration,
    ) -> std::result::Result<BilateralCommand, Self::Error>;

    fn tick_with_compensation(
        &mut self,
        frame: &BilateralControlFrame,
        dt: Duration,
    ) -> std::result::Result<BilateralCommand, Self::Error> {
        self.tick(&frame.snapshot, dt)
    }

    fn on_time_jump(&mut self, _dt: Duration) -> std::result::Result<(), Self::Error> {
        Ok(())
    }

    fn reset(&mut self) -> std::result::Result<(), Self::Error> {
        Ok(())
    }
}

/// 主从跟随控制器
#[derive(Debug, Clone)]
pub struct MasterFollowerController {
    calibration: DualArmCalibration,
    track_kp: JointArray<f64>,
    track_kd: JointArray<f64>,
    master_damping: JointArray<f64>,
}

impl MasterFollowerController {
    pub fn new(calibration: DualArmCalibration) -> Self {
        Self {
            calibration,
            track_kp: JointArray::splat(5.0),
            track_kd: JointArray::splat(0.8),
            master_damping: JointArray::splat(0.2),
        }
    }

    pub fn with_track_gains(mut self, kp: JointArray<f64>, kd: JointArray<f64>) -> Self {
        self.track_kp = kp;
        self.track_kd = kd;
        self
    }

    pub fn with_master_damping(mut self, damping: JointArray<f64>) -> Self {
        self.master_damping = damping;
        self
    }
}

impl BilateralController for MasterFollowerController {
    type Error = Infallible;

    fn tick(
        &mut self,
        snapshot: &DualArmSnapshot,
        _dt: Duration,
    ) -> std::result::Result<BilateralCommand, Self::Error> {
        Ok(BilateralCommand {
            slave_position: self.calibration.master_to_slave_position(snapshot.left.state.position),
            slave_velocity: self.calibration.master_to_slave_velocity(snapshot.left.state.velocity),
            slave_kp: self.track_kp,
            slave_kd: self.track_kd,
            slave_feedforward_torque: JointArray::splat(NewtonMeter::ZERO),
            master_position: snapshot.left.state.position,
            master_velocity: JointArray::splat(0.0),
            master_kp: JointArray::splat(0.0),
            master_kd: self.master_damping,
            master_interaction_torque: JointArray::splat(NewtonMeter::ZERO),
        })
    }
}

/// 关节空间双边控制器
#[derive(Debug, Clone)]
pub struct JointSpaceBilateralController {
    calibration: DualArmCalibration,
    track_kp: JointArray<f64>,
    track_kd: JointArray<f64>,
    master_damping: JointArray<f64>,
    reflection_gain: JointArray<f64>,
}

impl JointSpaceBilateralController {
    pub fn new(calibration: DualArmCalibration) -> Self {
        Self {
            calibration,
            track_kp: JointArray::splat(5.0),
            track_kd: JointArray::splat(0.8),
            master_damping: JointArray::splat(0.2),
            reflection_gain: JointArray::splat(0.3),
        }
    }

    pub fn with_track_gains(mut self, kp: JointArray<f64>, kd: JointArray<f64>) -> Self {
        self.track_kp = kp;
        self.track_kd = kd;
        self
    }

    pub fn with_master_damping(mut self, damping: JointArray<f64>) -> Self {
        self.master_damping = damping;
        self
    }

    pub fn with_reflection_gain(mut self, gain: JointArray<f64>) -> Self {
        self.reflection_gain = gain;
        self
    }
}

impl BilateralController for JointSpaceBilateralController {
    type Error = Infallible;

    fn tick(
        &mut self,
        snapshot: &DualArmSnapshot,
        _dt: Duration,
    ) -> std::result::Result<BilateralCommand, Self::Error> {
        self.tick_with_compensation(
            &BilateralControlFrame {
                snapshot: *snapshot,
                compensation: None,
            },
            Duration::ZERO,
        )
    }

    fn tick_with_compensation(
        &mut self,
        frame: &BilateralControlFrame,
        _dt: Duration,
    ) -> std::result::Result<BilateralCommand, Self::Error> {
        let mapped_slave_torque = self
            .calibration
            .slave_to_master_torque(
                frame
                    .compensation
                    .map(|compensation| compensation.slave_external_torque_est)
                    .unwrap_or(frame.snapshot.right.state.torque),
            )
            .map_with(self.reflection_gain, |tau, gain| NewtonMeter(-tau.0 * gain));

        Ok(BilateralCommand {
            slave_position: self
                .calibration
                .master_to_slave_position(frame.snapshot.left.state.position),
            slave_velocity: self
                .calibration
                .master_to_slave_velocity(frame.snapshot.left.state.velocity),
            slave_kp: self.track_kp,
            slave_kd: self.track_kd,
            slave_feedforward_torque: JointArray::splat(NewtonMeter::ZERO),
            master_position: frame.snapshot.left.state.position,
            master_velocity: JointArray::splat(0.0),
            master_kp: JointArray::splat(0.0),
            master_kd: self.master_damping,
            master_interaction_torque: mapped_slave_torque,
        })
    }
}

trait InternalBilateralDynamicsCompensator {
    fn compute(
        &mut self,
        snapshot: &DualArmSnapshot,
        dt: Duration,
    ) -> std::result::Result<BilateralDynamicsCompensation, String>;

    fn on_time_jump(&mut self, dt: Duration) -> std::result::Result<(), String>;

    fn reset(&mut self) -> std::result::Result<(), String>;
}

struct CompensatorAdapter<C> {
    inner: C,
}

impl<C> CompensatorAdapter<C> {
    fn new(inner: C) -> Self {
        Self { inner }
    }
}

impl<C> InternalBilateralDynamicsCompensator for CompensatorAdapter<C>
where
    C: BilateralDynamicsCompensator,
{
    fn compute(
        &mut self,
        snapshot: &DualArmSnapshot,
        dt: Duration,
    ) -> std::result::Result<BilateralDynamicsCompensation, String> {
        self.inner.compute(snapshot, dt).map_err(|error| error.to_string())
    }

    fn on_time_jump(&mut self, dt: Duration) -> std::result::Result<(), String> {
        self.inner.on_time_jump(dt).map_err(|error| error.to_string())
    }

    fn reset(&mut self) -> std::result::Result<(), String> {
        self.inner.reset().map_err(|error| error.to_string())
    }
}

#[derive(Debug)]
struct OutputShapingState {
    master_interaction_filtered: JointArray<NewtonMeter>,
    last_master_interaction: JointArray<NewtonMeter>,
    passivity_energy: f64,
}

impl Default for OutputShapingState {
    fn default() -> Self {
        Self {
            master_interaction_filtered: JointArray::splat(NewtonMeter::ZERO),
            last_master_interaction: JointArray::splat(NewtonMeter::ZERO),
            passivity_energy: 0.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct FinalTorques {
    master: JointArray<NewtonMeter>,
    slave: JointArray<NewtonMeter>,
}

#[derive(Debug, Clone, Copy)]
pub struct DualArmHoldAnchor {
    left_position: JointArray<Rad>,
    right_position: JointArray<Rad>,
    captured_at: Instant,
}

impl DualArmHoldAnchor {
    fn from_snapshot(snapshot: &DualArmSnapshot) -> Self {
        Self {
            left_position: snapshot.left.state.position,
            right_position: snapshot.right.state.position,
            captured_at: snapshot.host_cycle_timestamp,
        }
    }

    fn age(self, now: Instant) -> Duration {
        now.saturating_duration_since(self.captured_at)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct InterArmSkew {
    position: Duration,
    dynamic: Duration,
    effective: Duration,
    signed_effective_skew_us: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FaultShutdown {
    left_stop_attempt: StopAttemptResult,
    right_stop_attempt: StopAttemptResult,
}

const FAULT_SHUTDOWN_TIMEOUT: Duration = Duration::from_millis(10);

fn compute_inter_arm_skew(left: &ControlSnapshotFull, right: &ControlSnapshotFull) -> InterArmSkew {
    let signed_position_skew_us = signed_us_diff(
        left.position_system_timestamp_us,
        right.position_system_timestamp_us,
    );
    let signed_dynamic_skew_us = signed_us_diff(
        left.dynamic_system_timestamp_us,
        right.dynamic_system_timestamp_us,
    );
    let position = Duration::from_micros(
        left.position_system_timestamp_us.abs_diff(right.position_system_timestamp_us),
    );
    let dynamic = Duration::from_micros(
        left.dynamic_system_timestamp_us.abs_diff(right.dynamic_system_timestamp_us),
    );

    let (effective, signed_effective_skew_us) = if position >= dynamic {
        (position, signed_position_skew_us)
    } else {
        (dynamic, signed_dynamic_skew_us)
    };

    InterArmSkew {
        position,
        dynamic,
        effective,
        signed_effective_skew_us,
    }
}

fn calibration_read_policy() -> DualArmReadPolicy {
    DualArmReadPolicy {
        per_arm: ControlReadPolicy {
            max_state_skew_us: DualArmReadPolicy::default().per_arm.max_state_skew_us,
            max_feedback_age: Duration::from_millis(100),
        },
        max_inter_arm_skew: Duration::from_millis(25),
    }
}

fn signed_us_diff(left: u64, right: u64) -> i64 {
    let diff = left as i128 - right as i128;
    diff.clamp(i64::MIN as i128, i64::MAX as i128) as i64
}

fn validate_hold_anchor(anchor: &DualArmHoldAnchor, now: Instant, max_age: Duration) -> Result<()> {
    let age = anchor.age(now);
    if age > max_age {
        return Err(RobotError::feedback_stale(age, max_age));
    }
    Ok(())
}

fn format_runtime_health_error(health: DualArmRuntimeHealth) -> String {
    format!(
        "runtime health unhealthy: left(connected={}, rx_alive={}, tx_alive={}, fault={:?}), right(connected={}, rx_alive={}, tx_alive={}, fault={:?})",
        health.left.connected,
        health.left.rx_alive,
        health.left.tx_alive,
        health.left.fault,
        health.right.connected,
        health.right.rx_alive,
        health.right.tx_alive,
        health.right.fault,
    )
}

fn classify_runtime_transport_fault(health: DualArmRuntimeHealth) -> bool {
    !health.left.rx_alive
        || !health.left.tx_alive
        || !health.right.rx_alive
        || !health.right.tx_alive
        || health.left.fault.is_some()
        || health.right.fault.is_some()
}

fn stop_attempt_from_driver_error(err: &DriverError) -> StopAttemptResult {
    match err {
        DriverError::ChannelClosed => StopAttemptResult::ChannelClosed,
        DriverError::ControlPathClosed => StopAttemptResult::ChannelClosed,
        DriverError::ChannelFull => StopAttemptResult::QueueRejected,
        DriverError::Timeout => StopAttemptResult::Timeout,
        DriverError::ReliableDeliveryFailed { .. } => StopAttemptResult::TransportFailed,
        DriverError::RealtimeDeliveryAbortedByFault { .. } => StopAttemptResult::TransportFailed,
        DriverError::CommandAbortedByFault => StopAttemptResult::TransportFailed,
        _ => StopAttemptResult::TransportFailed,
    }
}

fn stop_attempt_from_robot_error(err: &RobotError) -> StopAttemptResult {
    match err {
        RobotError::Infrastructure(driver) => stop_attempt_from_driver_error(driver),
        _ => StopAttemptResult::TransportFailed,
    }
}

enum PendingStopAttempt {
    Receipt(piper_driver::ShutdownReceipt),
    Immediate(StopAttemptResult),
}

fn enqueue_stop_attempt(piper: &Piper<Active<MitMode>>, deadline: Instant) -> PendingStopAttempt {
    match RawCommander::new(&piper.driver).emergency_stop_enqueue(deadline) {
        Ok(receipt) => PendingStopAttempt::Receipt(receipt),
        Err(err) => PendingStopAttempt::Immediate(stop_attempt_from_robot_error(&err)),
    }
}

fn resolve_stop_attempt(pending: PendingStopAttempt) -> StopAttemptResult {
    match pending {
        PendingStopAttempt::Receipt(receipt) => match receipt.wait() {
            Ok(()) => StopAttemptResult::ConfirmedSent,
            Err(err) => stop_attempt_from_driver_error(&err),
        },
        PendingStopAttempt::Immediate(result) => result,
    }
}

fn force_error_state(piper: Piper<Active<MitMode>>) -> Piper<ErrorState> {
    let piper = std::mem::ManuallyDrop::new(piper);

    Piper {
        // SAFETY: `piper.driver` remains valid and is moved exactly once into the new state wrapper.
        driver: unsafe { std::ptr::read(&piper.driver) },
        // SAFETY: `piper.observer` remains valid and is moved exactly once into the new state wrapper.
        observer: unsafe { std::ptr::read(&piper.observer) },
        // SAFETY: `piper.quirks` is moved exactly once into the new state wrapper.
        quirks: unsafe { std::ptr::read(&piper.quirks) },
        _state: ErrorState,
    }
}

fn assemble_final_torques(
    command: &BilateralCommand,
    compensation: Option<BilateralDynamicsCompensation>,
) -> FinalTorques {
    let compensation = compensation.unwrap_or_default();
    let mut master = command.master_interaction_torque;
    let mut slave = command.slave_feedforward_torque;
    for joint in Joint::ALL {
        master[joint] = NewtonMeter(master[joint].0 + compensation.master_model_torque[joint].0);
        slave[joint] = NewtonMeter(slave[joint].0 + compensation.slave_model_torque[joint].0);
    }
    FinalTorques { master, slave }
}

fn apply_output_shaping(
    cfg: &BilateralLoopConfig,
    snapshot: &DualArmSnapshot,
    dt: Duration,
    state: &mut OutputShapingState,
    command: &mut BilateralCommand,
) {
    let dt_sec = dt.as_secs_f64().max(f64::EPSILON);
    let rc = if cfg.master_interaction_lpf_cutoff_hz > 0.0 {
        1.0 / (2.0 * std::f64::consts::PI * cfg.master_interaction_lpf_cutoff_hz)
    } else {
        0.0
    };
    let alpha = if rc > 0.0 {
        dt_sec / (rc + dt_sec)
    } else {
        1.0
    };

    for joint in Joint::ALL {
        let raw = command.master_interaction_torque[joint];
        let filtered = NewtonMeter(
            state.master_interaction_filtered[joint].0
                + alpha * (raw.0 - state.master_interaction_filtered[joint].0),
        );
        state.master_interaction_filtered[joint] = filtered;

        let last = state.last_master_interaction[joint];
        let limit = cfg.master_interaction_slew_limit[joint].0;
        let delta = (filtered.0 - last.0).clamp(-limit, limit);
        let shaped = NewtonMeter(last.0 + delta).clamp(
            -cfg.master_interaction_limit[joint],
            cfg.master_interaction_limit[joint],
        );
        command.master_interaction_torque[joint] = shaped;
    }

    let power: f64 = Joint::ALL
        .into_iter()
        .map(|joint| {
            command.master_interaction_torque[joint].0 * snapshot.left.state.velocity[joint].0
        })
        .sum();
    state.passivity_energy = (state.passivity_energy + power * dt_sec).max(0.0);

    if cfg.master_passivity_enabled && state.passivity_energy > 0.0 {
        let velocity_sq: f64 = Joint::ALL
            .into_iter()
            .map(|joint| snapshot.left.state.velocity[joint].0.powi(2))
            .sum();
        if velocity_sq > f64::EPSILON {
            let target_damping = (state.passivity_energy / (velocity_sq * dt_sec)).max(0.0);
            let damping = Joint::ALL.into_iter().fold(JointArray::splat(0.0), |mut acc, joint| {
                acc[joint] = target_damping.min(cfg.master_passivity_max_damping[joint]);
                acc
            });

            let mut dissipated = 0.0;
            for joint in Joint::ALL {
                let tau_damp = -damping[joint] * snapshot.left.state.velocity[joint].0;
                command.master_interaction_torque[joint] =
                    NewtonMeter(command.master_interaction_torque[joint].0 + tau_damp).clamp(
                        -cfg.master_interaction_limit[joint],
                        cfg.master_interaction_limit[joint],
                    );
                dissipated +=
                    damping[joint] * snapshot.left.state.velocity[joint].0.powi(2) * dt_sec;
            }
            state.passivity_energy = (state.passivity_energy - dissipated).max(0.0);
        }
    }

    for joint in Joint::ALL {
        command.slave_feedforward_torque[joint] = command.slave_feedforward_torque[joint].clamp(
            -cfg.slave_feedforward_limit[joint],
            cfg.slave_feedforward_limit[joint],
        );
        state.last_master_interaction[joint] = command.master_interaction_torque[joint];
    }
}

fn sleep_strategy_from_loop_timing(mode: LoopTimingMode) -> SleepStrategy {
    match mode {
        LoopTimingMode::Sleep => SleepStrategy::Sleep,
        LoopTimingMode::Spin => SleepStrategy::Spin,
    }
}

fn command_hold(
    arm: &Piper<Active<MitMode>>,
    position: &JointArray<Rad>,
    cfg: &DualArmSafetyConfig,
) -> Result<()> {
    arm.command_torques(
        position,
        &JointArray::splat(0.0),
        &cfg.safe_hold_kp,
        &cfg.safe_hold_kd,
        &JointArray::splat(NewtonMeter::ZERO),
    )
}

fn best_effort_hold_from_anchor(
    active: &DualArmActiveMit,
    anchor: Option<DualArmHoldAnchor>,
    now: Instant,
    cfg: &DualArmSafetyConfig,
) -> bool {
    let Some(anchor) = anchor else {
        return false;
    };

    active.safe_hold_from_anchor(&anchor, cfg, now).is_ok()
}

fn update_report_metrics(
    report: &mut BilateralRunReport,
    left: &piper_driver::Piper,
    right: &piper_driver::Piper,
) {
    let left_metrics = left.get_metrics();
    let right_metrics = right.get_metrics();
    report.left_tx_realtime_overwrites_total = left_metrics.tx_realtime_overwrites_total;
    report.right_tx_realtime_overwrites_total = right_metrics.tx_realtime_overwrites_total;
    report.left_tx_frames_sent_total = left_metrics.tx_frames_sent_total;
    report.right_tx_frames_sent_total = right_metrics.tx_frames_sent_total;
    report.left_tx_fault_aborts_total = left_metrics.tx_fault_aborts_total;
    report.right_tx_fault_aborts_total = right_metrics.tx_fault_aborts_total;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observer::Observer;
    use crate::types::RadPerSecond;
    use piper_can::{CanError, PiperFrame, RxAdapter, TxAdapter};
    use piper_driver::Piper as RobotPiper;
    use piper_protocol::control::MitControlCommand;
    use piper_protocol::ids::{
        ID_JOINT_DRIVER_HIGH_SPEED_BASE, ID_JOINT_FEEDBACK_12, ID_JOINT_FEEDBACK_34,
        ID_JOINT_FEEDBACK_56,
    };
    use semver::Version;
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
    use std::sync::{Arc, Mutex};
    use std::thread;
    use thiserror::Error;

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

    struct FailAfterFramesRxAdapter {
        frames: VecDeque<PiperFrame>,
        tripped: bool,
    }

    impl FailAfterFramesRxAdapter {
        fn new(frames: Vec<PiperFrame>) -> Self {
            Self {
                frames: frames.into(),
                tripped: false,
            }
        }
    }

    impl RxAdapter for FailAfterFramesRxAdapter {
        fn receive(&mut self) -> std::result::Result<PiperFrame, CanError> {
            if let Some(frame) = self.frames.pop_front() {
                return Ok(frame);
            }
            if !self.tripped {
                self.tripped = true;
                Err(CanError::BufferOverflow)
            } else {
                Err(CanError::Timeout)
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

    impl TxAdapter for RecordingTxAdapter {
        fn send_until(
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

    struct SlowRecordingTxAdapter {
        delay: Duration,
        sent_frames: Arc<Mutex<Vec<PiperFrame>>>,
    }

    impl TxAdapter for SlowRecordingTxAdapter {
        fn send_until(
            &mut self,
            frame: PiperFrame,
            deadline: Instant,
        ) -> std::result::Result<(), CanError> {
            let now = Instant::now();
            let Some(remaining) = deadline.checked_duration_since(now) else {
                return Err(CanError::Timeout);
            };
            if remaining < self.delay {
                thread::sleep(remaining);
                return Err(CanError::Timeout);
            }
            thread::sleep(self.delay);
            self.sent_frames.lock().expect("sent frames lock").push(frame);
            Ok(())
        }
    }

    struct FailOnNthFatalTxAdapter {
        fail_on: usize,
        sends: usize,
    }

    impl TxAdapter for FailOnNthFatalTxAdapter {
        fn send_until(
            &mut self,
            _frame: PiperFrame,
            deadline: Instant,
        ) -> std::result::Result<(), CanError> {
            if deadline <= Instant::now() {
                return Err(CanError::Timeout);
            }
            self.sends += 1;
            if self.sends == self.fail_on {
                return Err(CanError::BufferOverflow);
            }
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
        ]
    }

    fn incomplete_scripted_frames(timestamp_us: u64) -> Vec<PiperFrame> {
        vec![
            joint_feedback_frame(ID_JOINT_FEEDBACK_12 as u16, 0, 0, timestamp_us),
            joint_feedback_frame(ID_JOINT_FEEDBACK_34 as u16, 0, 0, timestamp_us),
            joint_feedback_frame(ID_JOINT_FEEDBACK_56 as u16, 0, 0, timestamp_us),
            joint_dynamic_frame(1, 0, 0, timestamp_us),
            joint_dynamic_frame(2, 0, 0, timestamp_us),
            joint_dynamic_frame(3, 0, 0, timestamp_us),
            joint_dynamic_frame(4, 0, 0, timestamp_us),
            joint_dynamic_frame(5, 0, 0, timestamp_us),
        ]
    }

    fn build_piper_with_frames_and_tx_adapter<T, State>(
        frames: Vec<PiperFrame>,
        tx_adapter: T,
        post_feedback_delay: Duration,
        state: State,
    ) -> Piper<State>
    where
        T: TxAdapter + Send + 'static,
    {
        build_piper_with_adapters(
            ScriptedRxAdapter::new(frames),
            tx_adapter,
            post_feedback_delay,
            state,
        )
    }

    fn build_piper_with_adapters<R, T, State>(
        rx_adapter: R,
        tx_adapter: T,
        post_feedback_delay: Duration,
        state: State,
    ) -> Piper<State>
    where
        R: RxAdapter + Send + 'static,
        T: TxAdapter + Send + 'static,
    {
        let driver = Arc::new(
            RobotPiper::new_dual_thread_parts(rx_adapter, tx_adapter, None)
                .expect("driver should start"),
        );
        let observer = Observer::new(driver.clone());
        driver
            .wait_for_feedback(Duration::from_millis(200))
            .expect("feedback should arrive");
        if !post_feedback_delay.is_zero() {
            thread::sleep(post_feedback_delay);
        }

        Piper {
            driver,
            observer,
            quirks: crate::types::DeviceQuirks::from_firmware_version(Version::new(1, 8, 3)),
            _state: state,
        }
    }

    fn build_active_mit_piper_with_rx_and_tx_adapter<R, T>(
        rx_adapter: R,
        tx_adapter: T,
        post_feedback_delay: Duration,
    ) -> Piper<Active<MitMode>>
    where
        R: RxAdapter + Send + 'static,
        T: TxAdapter + Send + 'static,
    {
        build_piper_with_adapters(
            rx_adapter,
            tx_adapter,
            post_feedback_delay,
            active_mit_marker(),
        )
    }

    fn build_piper_with_tx_adapter<T, State>(
        timestamp_us: u64,
        tx_adapter: T,
        post_feedback_delay: Duration,
        state: State,
    ) -> Piper<State>
    where
        T: TxAdapter + Send + 'static,
    {
        build_piper_with_frames_and_tx_adapter(
            scripted_frames(timestamp_us),
            tx_adapter,
            post_feedback_delay,
            state,
        )
    }

    fn build_active_mit_piper(
        timestamp_us: u64,
        sent_frames: Arc<Mutex<Vec<PiperFrame>>>,
    ) -> Piper<Active<MitMode>> {
        build_piper_with_tx_adapter(
            timestamp_us,
            RecordingTxAdapter::new(sent_frames),
            Duration::from_millis(20),
            active_mit_marker(),
        )
    }

    fn build_active_mit_piper_with_delay(
        timestamp_us: u64,
        sent_frames: Arc<Mutex<Vec<PiperFrame>>>,
        post_feedback_delay: Duration,
    ) -> Piper<Active<MitMode>> {
        build_piper_with_tx_adapter(
            timestamp_us,
            RecordingTxAdapter::new(sent_frames),
            post_feedback_delay,
            active_mit_marker(),
        )
    }

    fn build_standby_piper(
        timestamp_us: u64,
        sent_frames: Arc<Mutex<Vec<PiperFrame>>>,
        post_feedback_delay: Duration,
    ) -> Piper<Standby> {
        build_piper_with_tx_adapter(
            timestamp_us,
            RecordingTxAdapter::new(sent_frames),
            post_feedback_delay,
            Standby,
        )
    }

    fn build_active_mit_piper_with_frames(
        frames: Vec<PiperFrame>,
        sent_frames: Arc<Mutex<Vec<PiperFrame>>>,
    ) -> Piper<Active<MitMode>> {
        build_piper_with_frames_and_tx_adapter(
            frames,
            RecordingTxAdapter::new(sent_frames),
            Duration::ZERO,
            active_mit_marker(),
        )
    }

    fn active_mit_marker() -> Active<MitMode> {
        // SAFETY: `Active<MitMode>` is a zero-sized state marker wrapping another
        // zero-sized type, so any bit pattern is valid.
        unsafe { std::mem::MaybeUninit::zeroed().assume_init() }
    }

    fn wait_for_sent_frames(
        sent_frames: &Arc<Mutex<Vec<PiperFrame>>>,
        expected: usize,
    ) -> Vec<PiperFrame> {
        let start = Instant::now();
        loop {
            let frames = sent_frames.lock().expect("sent frames lock").clone();
            if frames.len() >= expected {
                return frames;
            }

            assert!(
                start.elapsed() < Duration::from_millis(400),
                "timed out waiting for {} sent frames, got {}",
                expected,
                frames.len()
            );
            thread::sleep(Duration::from_millis(5));
        }
    }

    fn snapshot_with_state(
        left_position: JointArray<Rad>,
        left_velocity: JointArray<RadPerSecond>,
        right_torque: JointArray<NewtonMeter>,
    ) -> DualArmSnapshot {
        DualArmSnapshot {
            left: ControlSnapshotFull {
                state: crate::observer::ControlSnapshot {
                    position: left_position,
                    velocity: left_velocity,
                    torque: JointArray::splat(NewtonMeter::ZERO),
                    position_timestamp_us: 1,
                    dynamic_timestamp_us: 1,
                    skew_us: 0,
                },
                position_system_timestamp_us: 1,
                dynamic_system_timestamp_us: 1,
                feedback_age: Duration::from_millis(1),
            },
            right: ControlSnapshotFull {
                state: crate::observer::ControlSnapshot {
                    position: JointArray::splat(Rad(0.0)),
                    velocity: JointArray::splat(RadPerSecond(0.0)),
                    torque: right_torque,
                    position_timestamp_us: 1,
                    dynamic_timestamp_us: 1,
                    skew_us: 0,
                },
                position_system_timestamp_us: 1,
                dynamic_system_timestamp_us: 1,
                feedback_age: Duration::from_millis(1),
            },
            inter_arm_skew: Duration::ZERO,
            host_cycle_timestamp: Instant::now(),
        }
    }

    fn control_snapshot_full_with_timestamps(
        position_system_timestamp_us: u64,
        dynamic_system_timestamp_us: u64,
    ) -> ControlSnapshotFull {
        ControlSnapshotFull {
            state: crate::observer::ControlSnapshot {
                position: JointArray::splat(Rad(0.0)),
                velocity: JointArray::splat(RadPerSecond(0.0)),
                torque: JointArray::splat(NewtonMeter::ZERO),
                position_timestamp_us: position_system_timestamp_us,
                dynamic_timestamp_us: dynamic_system_timestamp_us,
                skew_us: signed_us_diff(position_system_timestamp_us, dynamic_system_timestamp_us),
            },
            position_system_timestamp_us,
            dynamic_system_timestamp_us,
            feedback_age: Duration::from_millis(1),
        }
    }

    #[derive(Debug, Error)]
    #[error("{message}")]
    struct FakeCompensationError {
        message: &'static str,
    }

    struct FakeCompensator {
        results:
            VecDeque<std::result::Result<BilateralDynamicsCompensation, FakeCompensationError>>,
        time_jump_calls: Arc<AtomicUsize>,
        reset_calls: Arc<AtomicUsize>,
    }

    impl FakeCompensator {
        fn from_results(
            results: impl Into<
                VecDeque<std::result::Result<BilateralDynamicsCompensation, FakeCompensationError>>,
            >,
        ) -> Self {
            Self {
                results: results.into(),
                time_jump_calls: Arc::new(AtomicUsize::new(0)),
                reset_calls: Arc::new(AtomicUsize::new(0)),
            }
        }

        fn reset_counter(&self) -> Arc<AtomicUsize> {
            self.reset_calls.clone()
        }
    }

    impl BilateralDynamicsCompensator for FakeCompensator {
        type Error = FakeCompensationError;

        fn compute(
            &mut self,
            _snapshot: &DualArmSnapshot,
            _dt: Duration,
        ) -> std::result::Result<BilateralDynamicsCompensation, Self::Error> {
            self.results.pop_front().unwrap_or_else(|| {
                Ok(BilateralDynamicsCompensation {
                    master_model_torque: JointArray::splat(NewtonMeter::ZERO),
                    slave_model_torque: JointArray::splat(NewtonMeter::ZERO),
                    master_external_torque_est: JointArray::splat(NewtonMeter::ZERO),
                    slave_external_torque_est: JointArray::splat(NewtonMeter::ZERO),
                })
            })
        }

        fn on_time_jump(&mut self, _dt: Duration) -> std::result::Result<(), Self::Error> {
            self.time_jump_calls.fetch_add(1, AtomicOrdering::Relaxed);
            Ok(())
        }

        fn reset(&mut self) -> std::result::Result<(), Self::Error> {
            self.reset_calls.fetch_add(1, AtomicOrdering::Relaxed);
            Ok(())
        }
    }

    #[derive(Default)]
    struct ForwardingController {
        tick_calls: usize,
    }

    impl BilateralController for ForwardingController {
        type Error = Infallible;

        fn tick(
            &mut self,
            _snapshot: &DualArmSnapshot,
            _dt: Duration,
        ) -> std::result::Result<BilateralCommand, Self::Error> {
            self.tick_calls += 1;
            Ok(BilateralCommand {
                slave_position: JointArray::splat(Rad(0.0)),
                slave_velocity: JointArray::splat(0.0),
                slave_kp: JointArray::splat(0.0),
                slave_kd: JointArray::splat(0.0),
                slave_feedforward_torque: JointArray::splat(NewtonMeter::ZERO),
                master_position: JointArray::splat(Rad(0.0)),
                master_velocity: JointArray::splat(0.0),
                master_kp: JointArray::splat(0.0),
                master_kd: JointArray::splat(0.0),
                master_interaction_torque: JointArray::splat(NewtonMeter::ZERO),
            })
        }
    }

    struct SlowForwardingController {
        sleep_duration: Duration,
    }

    impl BilateralController for SlowForwardingController {
        type Error = Infallible;

        fn tick(
            &mut self,
            _snapshot: &DualArmSnapshot,
            _dt: Duration,
        ) -> std::result::Result<BilateralCommand, Self::Error> {
            thread::sleep(self.sleep_duration);
            Ok(BilateralCommand {
                slave_position: JointArray::splat(Rad(0.0)),
                slave_velocity: JointArray::splat(0.0),
                slave_kp: JointArray::splat(0.0),
                slave_kd: JointArray::splat(0.0),
                slave_feedforward_torque: JointArray::splat(NewtonMeter::ZERO),
                master_position: JointArray::splat(Rad(0.0)),
                master_velocity: JointArray::splat(0.0),
                master_kp: JointArray::splat(0.0),
                master_kd: JointArray::splat(0.0),
                master_interaction_torque: JointArray::splat(NewtonMeter::ZERO),
            })
        }
    }

    struct InvalidSlavePositionController {
        sleep_duration: Duration,
    }

    impl BilateralController for InvalidSlavePositionController {
        type Error = Infallible;

        fn tick(
            &mut self,
            snapshot: &DualArmSnapshot,
            _dt: Duration,
        ) -> std::result::Result<BilateralCommand, Self::Error> {
            if !self.sleep_duration.is_zero() {
                thread::sleep(self.sleep_duration);
            }

            Ok(BilateralCommand {
                slave_position: JointArray::from([
                    Rad(100.0),
                    Rad(0.0),
                    Rad(0.0),
                    Rad(0.0),
                    Rad(0.0),
                    Rad(0.0),
                ]),
                slave_velocity: JointArray::splat(0.0),
                slave_kp: JointArray::splat(0.0),
                slave_kd: JointArray::splat(0.0),
                slave_feedforward_torque: JointArray::splat(NewtonMeter::ZERO),
                master_position: snapshot.left.state.position,
                master_velocity: JointArray::splat(0.0),
                master_kp: JointArray::splat(0.0),
                master_kd: JointArray::splat(0.0),
                master_interaction_torque: JointArray::splat(NewtonMeter::ZERO),
            })
        }
    }

    #[test]
    fn test_joint_mirror_map_default_mapping() {
        let map = JointMirrorMap::left_right_mirror();
        assert_eq!(map.permutation, Joint::ALL);
        assert_eq!(map.position_sign, [-1.0, 1.0, 1.0, -1.0, 1.0, -1.0]);
        map.validate().expect("default map should be valid");
    }

    #[test]
    fn test_dual_arm_calibration_maps_position_velocity_and_torque() {
        let calibration = DualArmCalibration {
            master_zero: JointArray::splat(Rad(0.0)),
            slave_zero: JointArray::splat(Rad(0.0)),
            map: JointMirrorMap::left_right_mirror(),
        };

        let master_position =
            JointArray::from([Rad(1.0), Rad(2.0), Rad(3.0), Rad(4.0), Rad(5.0), Rad(6.0)]);
        let slave_position = calibration.master_to_slave_position(master_position);
        assert_eq!(
            slave_position,
            JointArray::from([
                Rad(-1.0),
                Rad(2.0),
                Rad(3.0),
                Rad(-4.0),
                Rad(5.0),
                Rad(-6.0),
            ])
        );

        let slave_torque = JointArray::from([
            NewtonMeter(1.0),
            NewtonMeter(2.0),
            NewtonMeter(3.0),
            NewtonMeter(4.0),
            NewtonMeter(5.0),
            NewtonMeter(6.0),
        ]);
        let master_torque = calibration.slave_to_master_torque(slave_torque);
        assert_eq!(
            master_torque,
            JointArray::from([
                NewtonMeter(-1.0),
                NewtonMeter(2.0),
                NewtonMeter(3.0),
                NewtonMeter(-4.0),
                NewtonMeter(5.0),
                NewtonMeter(-6.0),
            ])
        );
    }

    #[test]
    fn test_dual_arm_observer_rejects_inter_arm_skew() {
        let left_sent = Arc::new(Mutex::new(Vec::new()));
        let right_sent = Arc::new(Mutex::new(Vec::new()));
        let left = build_active_mit_piper(1_000, left_sent);
        let right = build_active_mit_piper(30_000, right_sent);
        let observer = DualArmObserver::new(left.observer().clone(), right.observer().clone());

        let error = observer
            .snapshot(DualArmReadPolicy {
                per_arm: ControlReadPolicy {
                    max_state_skew_us: 2_000,
                    max_feedback_age: Duration::from_secs(1),
                },
                max_inter_arm_skew: Duration::from_millis(1),
            })
            .expect_err("inter-arm skew should fail");

        assert!(matches!(error, RobotError::StateMisaligned { .. }));
    }

    #[test]
    fn test_dual_arm_observer_snapshot_reports_public_effective_inter_arm_skew() {
        let left = build_active_mit_piper(1_000, Arc::new(Mutex::new(Vec::new())));
        thread::sleep(Duration::from_millis(4));
        let right = build_active_mit_piper(1_000, Arc::new(Mutex::new(Vec::new())));
        let observer = DualArmObserver::new(left.observer().clone(), right.observer().clone());

        let snapshot = observer
            .snapshot(DualArmReadPolicy {
                per_arm: ControlReadPolicy {
                    max_state_skew_us: 2_000,
                    max_feedback_age: Duration::from_secs(1),
                },
                max_inter_arm_skew: Duration::from_secs(1),
            })
            .expect("aligned snapshot should succeed");

        let expected = compute_inter_arm_skew(&snapshot.left, &snapshot.right);
        assert_eq!(snapshot.inter_arm_skew, expected.effective);
    }

    #[test]
    fn test_dual_arm_observer_public_misalignment_uses_effective_signed_skew() {
        let left = build_active_mit_piper(1_000, Arc::new(Mutex::new(Vec::new())));
        thread::sleep(Duration::from_millis(4));
        let right = build_active_mit_piper(1_000, Arc::new(Mutex::new(Vec::new())));
        let observer = DualArmObserver::new(left.observer().clone(), right.observer().clone());
        let per_arm = ControlReadPolicy {
            max_state_skew_us: 2_000,
            max_feedback_age: Duration::from_secs(1),
        };
        let left_snapshot = left
            .observer()
            .control_snapshot_full(per_arm)
            .expect("left snapshot should succeed");
        let right_snapshot = right
            .observer()
            .control_snapshot_full(per_arm)
            .expect("right snapshot should succeed");
        let expected = compute_inter_arm_skew(&left_snapshot, &right_snapshot);

        let error = observer
            .snapshot(DualArmReadPolicy {
                per_arm,
                max_inter_arm_skew: Duration::from_millis(1),
            })
            .expect_err("inter-arm skew should exceed strict threshold");

        assert!(matches!(
            error,
            RobotError::StateMisaligned {
                skew_us,
                max_skew_us
            } if skew_us == expected.signed_effective_skew_us && max_skew_us == 1_000
        ));
    }

    #[test]
    fn test_compute_inter_arm_skew_rejects_cross_cancelled_timestamps() {
        let left = control_snapshot_full_with_timestamps(100_000, 95_000);
        let right = control_snapshot_full_with_timestamps(95_000, 100_000);

        let skew = compute_inter_arm_skew(&left, &right);

        assert_eq!(skew.position, Duration::from_millis(5));
        assert_eq!(skew.dynamic, Duration::from_millis(5));
        assert_eq!(skew.effective, Duration::from_millis(5));
        assert_eq!(skew.signed_effective_skew_us, 5_000);
    }

    #[test]
    fn test_compute_inter_arm_skew_uses_larger_channel_skew() {
        let left = control_snapshot_full_with_timestamps(100_000, 100_000);
        let right = control_snapshot_full_with_timestamps(98_500, 99_500);

        let skew = compute_inter_arm_skew(&left, &right);

        assert_eq!(skew.position, Duration::from_micros(1_500));
        assert_eq!(skew.dynamic, Duration::from_micros(500));
        assert_eq!(skew.effective, Duration::from_micros(1_500));
        assert_eq!(skew.signed_effective_skew_us, 1_500);
    }

    #[test]
    fn test_compute_inter_arm_skew_prefers_signed_larger_channel() {
        let left = control_snapshot_full_with_timestamps(100_000, 99_000);
        let right = control_snapshot_full_with_timestamps(99_750, 101_000);

        let skew = compute_inter_arm_skew(&left, &right);

        assert_eq!(skew.position, Duration::from_micros(250));
        assert_eq!(skew.dynamic, Duration::from_micros(2_000));
        assert_eq!(skew.effective, Duration::from_micros(2_000));
        assert_eq!(skew.signed_effective_skew_us, -2_000);
    }

    #[test]
    fn test_master_follower_controller_output() {
        let controller = MasterFollowerController::new(DualArmCalibration {
            master_zero: JointArray::splat(Rad(0.0)),
            slave_zero: JointArray::splat(Rad(0.0)),
            map: JointMirrorMap::left_right_mirror(),
        });
        let snapshot = snapshot_with_state(
            JointArray::splat(Rad(1.0)),
            JointArray::splat(RadPerSecond(0.5)),
            JointArray::splat(NewtonMeter::ZERO),
        );

        let mut controller = controller;
        let output = controller
            .tick(&snapshot, Duration::from_millis(5))
            .expect("controller should succeed");
        assert_eq!(output.slave_position[Joint::J1], Rad(-1.0));
        assert_eq!(output.master_kp, JointArray::splat(0.0));
    }

    #[test]
    fn test_tick_with_compensation_default_forwards_to_tick() {
        let snapshot = snapshot_with_state(
            JointArray::splat(Rad(0.0)),
            JointArray::splat(RadPerSecond(0.0)),
            JointArray::splat(NewtonMeter::ZERO),
        );
        let frame = BilateralControlFrame {
            snapshot,
            compensation: Some(BilateralDynamicsCompensation::default()),
        };
        let mut controller = ForwardingController::default();

        controller
            .tick_with_compensation(&frame, Duration::from_millis(5))
            .expect("controller should succeed");

        assert_eq!(controller.tick_calls, 1);
    }

    #[test]
    fn test_joint_space_bilateral_controller_reflects_slave_torque() {
        let calibration = DualArmCalibration {
            master_zero: JointArray::splat(Rad(0.0)),
            slave_zero: JointArray::splat(Rad(0.0)),
            map: JointMirrorMap::left_right_mirror(),
        };
        let snapshot = snapshot_with_state(
            JointArray::splat(Rad(0.0)),
            JointArray::splat(RadPerSecond(0.0)),
            JointArray::splat(NewtonMeter(2.0)),
        );

        let mut controller = JointSpaceBilateralController::new(calibration)
            .with_reflection_gain(JointArray::splat(0.5));
        let output = controller
            .tick(&snapshot, Duration::from_millis(5))
            .expect("controller should succeed");

        assert_eq!(
            output.master_interaction_torque[Joint::J1],
            NewtonMeter(1.0)
        );
        assert_eq!(
            output.master_interaction_torque[Joint::J2],
            NewtonMeter(-1.0)
        );
        assert_eq!(
            output.master_interaction_torque[Joint::J4],
            NewtonMeter(1.0)
        );
    }

    #[test]
    fn test_joint_space_bilateral_controller_prefers_external_torque_estimate() {
        let calibration = DualArmCalibration {
            master_zero: JointArray::splat(Rad(0.0)),
            slave_zero: JointArray::splat(Rad(0.0)),
            map: JointMirrorMap::left_right_mirror(),
        };
        let snapshot = snapshot_with_state(
            JointArray::splat(Rad(0.0)),
            JointArray::splat(RadPerSecond(0.0)),
            JointArray::splat(NewtonMeter(10.0)),
        );
        let frame = BilateralControlFrame {
            snapshot,
            compensation: Some(BilateralDynamicsCompensation {
                master_model_torque: JointArray::splat(NewtonMeter::ZERO),
                slave_model_torque: JointArray::splat(NewtonMeter::ZERO),
                master_external_torque_est: JointArray::splat(NewtonMeter::ZERO),
                slave_external_torque_est: JointArray::splat(NewtonMeter(2.0)),
            }),
        };

        let mut controller = JointSpaceBilateralController::new(calibration)
            .with_reflection_gain(JointArray::splat(0.5));
        let output = controller
            .tick_with_compensation(&frame, Duration::from_millis(5))
            .expect("controller should succeed");

        assert_eq!(
            output.master_interaction_torque[Joint::J1],
            NewtonMeter(1.0)
        );
        assert_eq!(
            output.master_interaction_torque[Joint::J2],
            NewtonMeter(-1.0)
        );
    }

    #[test]
    fn test_apply_output_shaping_limits_slew_and_clamps_slave_feedforward() {
        let snapshot = snapshot_with_state(
            JointArray::splat(Rad(0.0)),
            JointArray::splat(RadPerSecond(0.0)),
            JointArray::splat(NewtonMeter::ZERO),
        );
        let cfg = BilateralLoopConfig {
            master_interaction_lpf_cutoff_hz: 0.0,
            master_interaction_slew_limit: JointArray::splat(NewtonMeter(0.25)),
            master_interaction_limit: JointArray::splat(NewtonMeter(0.5)),
            slave_feedforward_limit: JointArray::splat(NewtonMeter(4.0)),
            master_passivity_enabled: false,
            ..Default::default()
        };
        let mut state = OutputShapingState::default();
        let mut command = BilateralCommand {
            slave_position: JointArray::splat(Rad(0.0)),
            slave_velocity: JointArray::splat(0.0),
            slave_kp: JointArray::splat(0.0),
            slave_kd: JointArray::splat(0.0),
            slave_feedforward_torque: JointArray::splat(NewtonMeter(10.0)),
            master_position: JointArray::splat(Rad(0.0)),
            master_velocity: JointArray::splat(0.0),
            master_kp: JointArray::splat(0.0),
            master_kd: JointArray::splat(0.0),
            master_interaction_torque: JointArray::splat(NewtonMeter(2.0)),
        };

        apply_output_shaping(
            &cfg,
            &snapshot,
            Duration::from_millis(5),
            &mut state,
            &mut command,
        );
        assert_eq!(
            command.master_interaction_torque,
            JointArray::splat(NewtonMeter(0.25))
        );
        assert_eq!(
            command.slave_feedforward_torque,
            JointArray::splat(NewtonMeter(4.0))
        );

        command.master_interaction_torque = JointArray::splat(NewtonMeter(2.0));
        apply_output_shaping(
            &cfg,
            &snapshot,
            Duration::from_millis(5),
            &mut state,
            &mut command,
        );
        assert_eq!(
            command.master_interaction_torque,
            JointArray::splat(NewtonMeter(0.5))
        );
    }

    #[test]
    fn test_apply_output_shaping_injects_passivity_damping() {
        let snapshot = snapshot_with_state(
            JointArray::splat(Rad(0.0)),
            JointArray::splat(RadPerSecond(1.0)),
            JointArray::splat(NewtonMeter::ZERO),
        );
        let cfg = BilateralLoopConfig {
            master_interaction_lpf_cutoff_hz: 0.0,
            master_interaction_slew_limit: JointArray::splat(NewtonMeter(10.0)),
            master_interaction_limit: JointArray::splat(NewtonMeter(10.0)),
            master_passivity_enabled: true,
            master_passivity_max_damping: JointArray::splat(1.0),
            ..Default::default()
        };
        let mut state = OutputShapingState::default();
        let mut command = BilateralCommand {
            slave_position: JointArray::splat(Rad(0.0)),
            slave_velocity: JointArray::splat(0.0),
            slave_kp: JointArray::splat(0.0),
            slave_kd: JointArray::splat(0.0),
            slave_feedforward_torque: JointArray::splat(NewtonMeter::ZERO),
            master_position: JointArray::splat(Rad(0.0)),
            master_velocity: JointArray::splat(0.0),
            master_kp: JointArray::splat(0.0),
            master_kd: JointArray::splat(0.0),
            master_interaction_torque: JointArray::splat(NewtonMeter(1.0)),
        };

        apply_output_shaping(
            &cfg,
            &snapshot,
            Duration::from_secs(1),
            &mut state,
            &mut command,
        );

        assert_eq!(
            command.master_interaction_torque,
            JointArray::splat(NewtonMeter(0.0))
        );
        assert_eq!(state.passivity_energy, 0.0);
    }

    #[test]
    fn test_assemble_final_torques_keeps_model_compensation_outside_interaction_limits() {
        let snapshot = snapshot_with_state(
            JointArray::splat(Rad(0.0)),
            JointArray::splat(RadPerSecond(0.0)),
            JointArray::splat(NewtonMeter::ZERO),
        );
        let cfg = BilateralLoopConfig {
            master_interaction_lpf_cutoff_hz: 0.0,
            master_interaction_slew_limit: JointArray::splat(NewtonMeter(0.5)),
            master_interaction_limit: JointArray::splat(NewtonMeter(0.5)),
            slave_feedforward_limit: JointArray::splat(NewtonMeter(4.0)),
            master_passivity_enabled: false,
            ..Default::default()
        };
        let mut state = OutputShapingState::default();
        let mut command = BilateralCommand {
            slave_position: JointArray::splat(Rad(0.0)),
            slave_velocity: JointArray::splat(0.0),
            slave_kp: JointArray::splat(0.0),
            slave_kd: JointArray::splat(0.0),
            slave_feedforward_torque: JointArray::splat(NewtonMeter(10.0)),
            master_position: JointArray::splat(Rad(0.0)),
            master_velocity: JointArray::splat(0.0),
            master_kp: JointArray::splat(0.0),
            master_kd: JointArray::splat(0.0),
            master_interaction_torque: JointArray::splat(NewtonMeter(2.0)),
        };

        apply_output_shaping(
            &cfg,
            &snapshot,
            Duration::from_millis(5),
            &mut state,
            &mut command,
        );

        let final_torques = assemble_final_torques(
            &command,
            Some(BilateralDynamicsCompensation {
                master_model_torque: JointArray::splat(NewtonMeter(3.0)),
                slave_model_torque: JointArray::splat(NewtonMeter(2.0)),
                master_external_torque_est: JointArray::splat(NewtonMeter::ZERO),
                slave_external_torque_est: JointArray::splat(NewtonMeter::ZERO),
            }),
        );

        assert_eq!(
            command.master_interaction_torque,
            JointArray::splat(NewtonMeter(0.5))
        );
        assert_eq!(final_torques.master, JointArray::splat(NewtonMeter(3.5)));
        assert_eq!(final_torques.slave, JointArray::splat(NewtonMeter(6.0)));
    }

    #[test]
    fn test_capture_calibration_uses_aligned_snapshot() {
        let standby = DualArmStandby {
            left: build_standby_piper(1_000, Arc::new(Mutex::new(Vec::new())), Duration::ZERO),
            right: build_standby_piper(1_000, Arc::new(Mutex::new(Vec::new())), Duration::ZERO),
        };

        let calibration = standby
            .capture_calibration(JointMirrorMap::left_right_mirror())
            .expect("calibration should succeed with a fresh aligned snapshot");

        assert_eq!(calibration.master_zero, JointArray::splat(Rad(0.0)));
        assert_eq!(calibration.slave_zero, JointArray::splat(Rad(0.0)));
    }

    #[test]
    fn test_capture_calibration_with_policy_uses_explicit_thresholds() {
        let standby = DualArmStandby {
            left: build_standby_piper(1_000, Arc::new(Mutex::new(Vec::new())), Duration::ZERO),
            right: build_standby_piper(1_000, Arc::new(Mutex::new(Vec::new())), Duration::ZERO),
        };
        thread::sleep(Duration::from_millis(20));

        standby
            .capture_calibration_with_policy(
                JointMirrorMap::left_right_mirror(),
                DualArmReadPolicy {
                    per_arm: ControlReadPolicy {
                        max_state_skew_us: 2_000,
                        max_feedback_age: Duration::from_millis(100),
                    },
                    max_inter_arm_skew: Duration::from_secs(1),
                },
            )
            .expect("relaxed calibration policy should tolerate this feedback age");

        let error = standby
            .capture_calibration_with_policy(
                JointMirrorMap::left_right_mirror(),
                DualArmReadPolicy {
                    per_arm: ControlReadPolicy {
                        max_state_skew_us: 2_000,
                        max_feedback_age: Duration::from_millis(5),
                    },
                    max_inter_arm_skew: Duration::from_secs(1),
                },
            )
            .expect_err("strict calibration policy should reject the same feedback age");

        assert!(matches!(error, RobotError::FeedbackStale { .. }));
    }

    #[test]
    fn test_capture_calibration_rejects_stale_or_misaligned_snapshot() {
        let stale = DualArmStandby {
            left: build_standby_piper(
                1_000,
                Arc::new(Mutex::new(Vec::new())),
                Duration::from_millis(125),
            ),
            right: build_standby_piper(
                1_000,
                Arc::new(Mutex::new(Vec::new())),
                Duration::from_millis(125),
            ),
        };
        let stale_error = stale
            .capture_calibration(JointMirrorMap::left_right_mirror())
            .expect_err("stale calibration snapshot should fail");
        assert!(matches!(stale_error, RobotError::FeedbackStale { .. }));

        let left = build_standby_piper(1_000, Arc::new(Mutex::new(Vec::new())), Duration::ZERO);
        thread::sleep(Duration::from_millis(40));
        let right = build_standby_piper(1_000, Arc::new(Mutex::new(Vec::new())), Duration::ZERO);
        let skewed = DualArmStandby { left, right };
        let skew_error = skewed
            .capture_calibration(JointMirrorMap::left_right_mirror())
            .expect_err("misaligned calibration snapshot should fail");
        assert!(matches!(skew_error, RobotError::StateMisaligned { .. }));
    }

    #[test]
    fn test_run_bilateral_with_compensation_adds_model_torque() {
        let left_sent = Arc::new(Mutex::new(Vec::new()));
        let right_sent = Arc::new(Mutex::new(Vec::new()));
        let arms = DualArmActiveMit {
            left: build_active_mit_piper(1_000, left_sent.clone()),
            right: build_active_mit_piper(1_000, right_sent.clone()),
        };

        let controller = ForwardingController::default();
        let compensator = FakeCompensator::from_results([Ok(BilateralDynamicsCompensation {
            master_model_torque: JointArray::splat(NewtonMeter(0.4)),
            slave_model_torque: JointArray::splat(NewtonMeter(0.6)),
            master_external_torque_est: JointArray::splat(NewtonMeter::ZERO),
            slave_external_torque_est: JointArray::splat(NewtonMeter::ZERO),
        })]);

        let exit = arms
            .run_bilateral_with_compensation(
                controller,
                compensator,
                BilateralLoopConfig {
                    warmup_cycles: 0,
                    max_iterations: Some(1),
                    frequency_hz: 20.0,
                    gripper: GripperTeleopConfig {
                        enabled: false,
                        ..Default::default()
                    },
                    master_interaction_lpf_cutoff_hz: 0.0,
                    master_interaction_slew_limit: JointArray::splat(NewtonMeter(10.0)),
                    master_interaction_limit: JointArray::splat(NewtonMeter(10.0)),
                    master_passivity_enabled: false,
                    read_policy: DualArmReadPolicy {
                        per_arm: ControlReadPolicy {
                            max_state_skew_us: 2_000,
                            max_feedback_age: Duration::from_secs(1),
                        },
                        max_inter_arm_skew: Duration::from_secs(1),
                    },
                    ..Default::default()
                },
            )
            .expect("bilateral run with compensation should succeed");

        match exit {
            DualArmLoopExit::Standby { report, .. } => {
                assert_eq!(report.iterations, 1);
            },
            DualArmLoopExit::Faulted { .. } => {
                panic!("expected standby exit");
            },
        }

        let right_frames = wait_for_sent_frames(&right_sent, 7);
        let left_frames = wait_for_sent_frames(&left_sent, 7);
        let expected_slave = MitControlCommand::try_new(1, 0.0, 0.0, 0.0, 0.0, 0.6)
            .expect("expected slave command")
            .to_frame();
        let expected_master = MitControlCommand::try_new(1, 0.0, 0.0, 0.0, 0.0, 0.4)
            .expect("expected master command")
            .to_frame();
        assert!(
            right_frames.iter().any(|frame| {
                frame.id == expected_slave.id && frame.data == expected_slave.data
            })
        );
        assert!(
            left_frames.iter().any(|frame| {
                frame.id == expected_master.id && frame.data == expected_master.data
            })
        );
    }

    #[test]
    fn test_hold_anchor_returns_only_on_successful_snapshot() {
        let fresh_observer = DualArmObserver::new(
            build_active_mit_piper(1_000, Arc::new(Mutex::new(Vec::new())))
                .observer()
                .clone(),
            build_active_mit_piper(1_000, Arc::new(Mutex::new(Vec::new())))
                .observer()
                .clone(),
        );
        fresh_observer
            .hold_anchor(DualArmReadPolicy {
                per_arm: ControlReadPolicy {
                    max_state_skew_us: 2_000,
                    max_feedback_age: Duration::from_secs(1),
                },
                max_inter_arm_skew: Duration::from_secs(1),
            })
            .expect("fresh aligned snapshot should yield hold anchor");

        let stale_observer = DualArmObserver::new(
            build_active_mit_piper_with_delay(
                1_000,
                Arc::new(Mutex::new(Vec::new())),
                Duration::from_millis(125),
            )
            .observer()
            .clone(),
            build_active_mit_piper_with_delay(
                1_000,
                Arc::new(Mutex::new(Vec::new())),
                Duration::from_millis(125),
            )
            .observer()
            .clone(),
        );
        let stale_error = stale_observer
            .hold_anchor(DualArmReadPolicy {
                per_arm: ControlReadPolicy {
                    max_state_skew_us: 2_000,
                    max_feedback_age: Duration::from_millis(50),
                },
                max_inter_arm_skew: Duration::from_secs(1),
            })
            .expect_err("stale snapshot should not yield hold anchor");
        assert!(matches!(stale_error, RobotError::FeedbackStale { .. }));

        let misaligned_observer = DualArmObserver::new(
            build_active_mit_piper(1_000, Arc::new(Mutex::new(Vec::new())))
                .observer()
                .clone(),
            build_active_mit_piper(30_000, Arc::new(Mutex::new(Vec::new())))
                .observer()
                .clone(),
        );
        let misaligned_error = misaligned_observer
            .hold_anchor(DualArmReadPolicy {
                per_arm: ControlReadPolicy {
                    max_state_skew_us: 2_000,
                    max_feedback_age: Duration::from_secs(1),
                },
                max_inter_arm_skew: Duration::from_millis(1),
            })
            .expect_err("misaligned snapshot should not yield hold anchor");
        assert!(matches!(
            misaligned_error,
            RobotError::StateMisaligned { .. }
        ));

        let incomplete_observer = DualArmObserver::new(
            build_active_mit_piper_with_frames(
                incomplete_scripted_frames(1_000),
                Arc::new(Mutex::new(Vec::new())),
            )
            .observer()
            .clone(),
            build_active_mit_piper(1_000, Arc::new(Mutex::new(Vec::new())))
                .observer()
                .clone(),
        );
        let incomplete_error = incomplete_observer
            .hold_anchor(DualArmReadPolicy {
                per_arm: ControlReadPolicy {
                    max_state_skew_us: 2_000,
                    max_feedback_age: Duration::from_secs(1),
                },
                max_inter_arm_skew: Duration::from_secs(1),
            })
            .expect_err("incomplete snapshot should not yield hold anchor");
        assert!(matches!(
            incomplete_error,
            RobotError::ControlStateIncomplete { .. }
        ));
    }

    #[test]
    fn test_safe_hold_sends_current_position_zero_velocity_and_default_gains() {
        let left_sent = Arc::new(Mutex::new(Vec::new()));
        let right_sent = Arc::new(Mutex::new(Vec::new()));
        let arms = DualArmActiveMit {
            left: build_active_mit_piper(1_000, left_sent.clone()),
            right: build_active_mit_piper(1_000, right_sent.clone()),
        };

        let anchor = arms
            .observer()
            .hold_anchor(DualArmReadPolicy {
                per_arm: ControlReadPolicy {
                    max_state_skew_us: 2_000,
                    max_feedback_age: Duration::from_secs(1),
                },
                max_inter_arm_skew: Duration::from_secs(1),
            })
            .expect("fresh hold anchor should succeed");
        arms.safe_hold(&anchor, &DualArmSafetyConfig::default())
            .expect("safe hold should succeed");

        let left_frames = wait_for_sent_frames(&left_sent, 1);
        let expected = MitControlCommand::try_new(1, 0.0, 0.0, 5.0, 0.8, 0.0)
            .expect("expected command should be valid")
            .to_frame();
        assert_eq!(left_frames[0].id, expected.id);
        assert_eq!(left_frames[0].data, expected.data);
    }

    #[test]
    fn test_safe_hold_rejects_expired_anchor_without_sending_commands() {
        let left_sent = Arc::new(Mutex::new(Vec::new()));
        let right_sent = Arc::new(Mutex::new(Vec::new()));
        let arms = DualArmActiveMit {
            left: build_active_mit_piper(1_000, left_sent.clone()),
            right: build_active_mit_piper(1_000, right_sent.clone()),
        };
        let anchor = arms
            .observer()
            .hold_anchor(DualArmReadPolicy {
                per_arm: ControlReadPolicy {
                    max_state_skew_us: 2_000,
                    max_feedback_age: Duration::from_secs(1),
                },
                max_inter_arm_skew: Duration::from_secs(1),
            })
            .expect("fresh hold anchor should succeed");

        thread::sleep(Duration::from_millis(30));
        let error = arms
            .safe_hold(
                &anchor,
                &DualArmSafetyConfig {
                    safe_hold_max_duration: Duration::from_millis(5),
                    ..Default::default()
                },
            )
            .expect_err("expired hold anchor should be rejected");

        assert!(matches!(error, RobotError::FeedbackStale { .. }));
        assert!(
            left_sent.lock().expect("sent frames lock").is_empty(),
            "expired anchor must not send hold commands",
        );
        assert!(
            right_sent.lock().expect("sent frames lock").is_empty(),
            "expired anchor must not send hold commands",
        );
    }

    #[test]
    fn test_run_bilateral_read_fault_uses_anchor_hold_then_disables() {
        let left_sent = Arc::new(Mutex::new(Vec::new()));
        let right_sent = Arc::new(Mutex::new(Vec::new()));
        let arms = DualArmActiveMit {
            left: build_active_mit_piper_with_delay(1_000, left_sent.clone(), Duration::ZERO),
            right: build_active_mit_piper_with_delay(1_000, right_sent, Duration::ZERO),
        };

        let exit = arms
            .run_bilateral(
                ForwardingController::default(),
                BilateralLoopConfig {
                    frequency_hz: 5.0,
                    warmup_cycles: 0,
                    gripper: GripperTeleopConfig {
                        enabled: false,
                        ..Default::default()
                    },
                    safety: DualArmSafetyConfig {
                        safe_hold_max_duration: Duration::from_millis(500),
                        consecutive_read_failures_before_disable: 2,
                        ..Default::default()
                    },
                    read_policy: DualArmReadPolicy {
                        per_arm: ControlReadPolicy {
                            max_state_skew_us: 2_000,
                            max_feedback_age: Duration::from_millis(100),
                        },
                        max_inter_arm_skew: Duration::from_secs(1),
                    },
                    ..Default::default()
                },
            )
            .expect("read fault should converge to standby");

        match exit {
            DualArmLoopExit::Standby { report, .. } => {
                assert!(report.read_faults >= 1);
                assert_eq!(report.exit_reason, Some(BilateralExitReason::ReadFault));
            },
            DualArmLoopExit::Faulted { .. } => {
                panic!("expected standby exit");
            },
        }

        let left_frames = wait_for_sent_frames(&left_sent, 12);
        let hold = MitControlCommand::try_new(1, 0.0, 0.0, 5.0, 0.8, 0.0)
            .expect("expected hold command")
            .to_frame();
        assert!(
            left_frames.iter().any(|frame| frame.id == hold.id && frame.data == hold.data),
            "expected anchor-based hold command after read fault",
        );
    }

    #[test]
    fn test_run_bilateral_warmup_uses_anchor_hold() {
        let left_sent = Arc::new(Mutex::new(Vec::new()));
        let right_sent = Arc::new(Mutex::new(Vec::new()));
        let arms = DualArmActiveMit {
            left: build_active_mit_piper(1_000, left_sent.clone()),
            right: build_active_mit_piper(1_000, right_sent),
        };

        let exit = arms
            .run_bilateral(
                ForwardingController::default(),
                BilateralLoopConfig {
                    warmup_cycles: 1,
                    max_iterations: Some(1),
                    gripper: GripperTeleopConfig {
                        enabled: false,
                        ..Default::default()
                    },
                    read_policy: DualArmReadPolicy {
                        per_arm: ControlReadPolicy {
                            max_state_skew_us: 2_000,
                            max_feedback_age: Duration::from_secs(1),
                        },
                        max_inter_arm_skew: Duration::from_secs(1),
                    },
                    ..Default::default()
                },
            )
            .expect("warmup hold should succeed");

        match exit {
            DualArmLoopExit::Standby { report, .. } => assert_eq!(report.iterations, 1),
            DualArmLoopExit::Faulted { .. } => panic!("expected standby exit"),
        }

        let left_frames = wait_for_sent_frames(&left_sent, 1);
        let hold = MitControlCommand::try_new(1, 0.0, 0.0, 5.0, 0.8, 0.0)
            .expect("expected hold command")
            .to_frame();
        assert!(
            left_frames.iter().any(|frame| frame.id == hold.id && frame.data == hold.data),
            "expected warmup hold command",
        );
    }

    #[test]
    fn test_run_bilateral_submission_failure_does_not_hold_other_arm_even_with_fresh_anchor() {
        let left_sent = Arc::new(Mutex::new(Vec::new()));
        let right_sent = Arc::new(Mutex::new(Vec::new()));
        let arms = DualArmActiveMit {
            left: build_active_mit_piper(1_000, left_sent.clone()),
            right: build_active_mit_piper(1_000, right_sent),
        };

        let exit = arms
            .run_bilateral(
                InvalidSlavePositionController {
                    sleep_duration: Duration::ZERO,
                },
                BilateralLoopConfig {
                    warmup_cycles: 0,
                    max_iterations: Some(1),
                    gripper: GripperTeleopConfig {
                        enabled: false,
                        ..Default::default()
                    },
                    read_policy: DualArmReadPolicy {
                        per_arm: ControlReadPolicy {
                            max_state_skew_us: 2_000,
                            max_feedback_age: Duration::from_secs(1),
                        },
                        max_inter_arm_skew: Duration::from_secs(1),
                    },
                    safety: DualArmSafetyConfig {
                        safe_hold_max_duration: Duration::from_millis(100),
                        ..Default::default()
                    },
                    ..Default::default()
                },
            )
            .expect("invalid command should converge to faulted exit");

        match exit {
            DualArmLoopExit::Standby { .. } => panic!("expected faulted exit"),
            DualArmLoopExit::Faulted { report, .. } => {
                assert_eq!(report.submission_faults, 1);
                assert_eq!(
                    report.exit_reason,
                    Some(BilateralExitReason::SubmissionFault)
                );
                assert_eq!(report.left_stop_attempt, StopAttemptResult::ConfirmedSent);
                assert_eq!(report.right_stop_attempt, StopAttemptResult::ConfirmedSent);
            },
        }

        let left_frames = wait_for_sent_frames(&left_sent, 1);
        let hold = MitControlCommand::try_new(1, 0.0, 0.0, 5.0, 0.8, 0.0)
            .expect("expected hold command")
            .to_frame();
        assert!(
            !left_frames.iter().any(|frame| frame.id == hold.id && frame.data == hold.data),
            "submission fault should go straight to fault shutdown",
        );
    }

    #[test]
    fn test_run_bilateral_submission_failure_skips_hold_when_anchor_is_expired() {
        let left_sent = Arc::new(Mutex::new(Vec::new()));
        let right_sent = Arc::new(Mutex::new(Vec::new()));
        let arms = DualArmActiveMit {
            left: build_active_mit_piper(1_000, left_sent.clone()),
            right: build_active_mit_piper(1_000, right_sent),
        };

        let exit = arms
            .run_bilateral(
                InvalidSlavePositionController {
                    sleep_duration: Duration::from_millis(40),
                },
                BilateralLoopConfig {
                    frequency_hz: 50.0,
                    warmup_cycles: 0,
                    max_iterations: Some(1),
                    gripper: GripperTeleopConfig {
                        enabled: false,
                        ..Default::default()
                    },
                    read_policy: DualArmReadPolicy {
                        per_arm: ControlReadPolicy {
                            max_state_skew_us: 2_000,
                            max_feedback_age: Duration::from_secs(1),
                        },
                        max_inter_arm_skew: Duration::from_secs(1),
                    },
                    safety: DualArmSafetyConfig {
                        safe_hold_max_duration: Duration::from_millis(5),
                        ..Default::default()
                    },
                    ..Default::default()
                },
            )
            .expect("invalid command should converge to faulted exit");

        match exit {
            DualArmLoopExit::Standby { .. } => panic!("expected faulted exit"),
            DualArmLoopExit::Faulted { report, .. } => {
                assert_eq!(report.submission_faults, 1);
                assert_eq!(
                    report.exit_reason,
                    Some(BilateralExitReason::SubmissionFault)
                );
                assert_eq!(report.left_stop_attempt, StopAttemptResult::ConfirmedSent);
                assert_eq!(report.right_stop_attempt, StopAttemptResult::ConfirmedSent);
            },
        }

        let left_frames = wait_for_sent_frames(&left_sent, 1);
        let hold = MitControlCommand::try_new(1, 0.0, 0.0, 5.0, 0.8, 0.0)
            .expect("expected hold command")
            .to_frame();
        assert!(
            !left_frames.iter().any(|frame| frame.id == hold.id && frame.data == hold.data),
            "expired anchor must not trigger best-effort hold",
        );
    }

    #[test]
    fn test_run_bilateral_runtime_transport_fault_exits_without_submission_fault() {
        let left_sent = Arc::new(Mutex::new(Vec::new()));
        let arms = DualArmActiveMit {
            left: build_active_mit_piper(1_000, left_sent.clone()),
            right: build_piper_with_tx_adapter(
                1_000,
                FailOnNthFatalTxAdapter {
                    fail_on: 1,
                    sends: 0,
                },
                Duration::ZERO,
                active_mit_marker(),
            ),
        };

        let exit = arms
            .run_bilateral(
                ForwardingController::default(),
                BilateralLoopConfig {
                    frequency_hz: 50.0,
                    warmup_cycles: 0,
                    max_iterations: Some(10),
                    gripper: GripperTeleopConfig {
                        enabled: false,
                        ..Default::default()
                    },
                    read_policy: DualArmReadPolicy {
                        per_arm: ControlReadPolicy {
                            max_state_skew_us: 2_000,
                            max_feedback_age: Duration::from_secs(1),
                        },
                        max_inter_arm_skew: Duration::from_secs(1),
                    },
                    ..Default::default()
                },
            )
            .expect("transport failure should converge to faulted exit");

        match exit {
            DualArmLoopExit::Standby { .. } => panic!("expected faulted exit"),
            DualArmLoopExit::Faulted { report, .. } => {
                assert_eq!(report.submission_faults, 0);
                assert_eq!(
                    report.exit_reason,
                    Some(BilateralExitReason::RuntimeTransportFault)
                );
                assert_eq!(
                    report.last_runtime_fault_right,
                    Some(RuntimeFaultKind::TransportError)
                );
                assert_eq!(report.left_stop_attempt, StopAttemptResult::ConfirmedSent);
                assert_eq!(report.right_stop_attempt, StopAttemptResult::ChannelClosed);
                assert!(
                    report
                        .last_error
                        .as_deref()
                        .map(|last_error: &str| last_error.contains("TransportError"))
                        .unwrap_or(false),
                    "last_error should preserve the transport failure cause",
                );
            },
        }

        let left_frames = wait_for_sent_frames(&left_sent, 1);
        let hold = MitControlCommand::try_new(1, 0.0, 0.0, 5.0, 0.8, 0.0)
            .expect("expected hold command")
            .to_frame();
        assert!(
            !left_frames.iter().any(|frame| frame.id == hold.id && frame.data == hold.data),
            "runtime unhealthy exit should not inject an extra hold",
        );
    }

    #[test]
    fn test_run_bilateral_runtime_transport_fault_from_rx_fatal_keeps_stop_attempt_confirmed() {
        let left_sent = Arc::new(Mutex::new(Vec::new()));
        let right_sent = Arc::new(Mutex::new(Vec::new()));
        let arms = DualArmActiveMit {
            left: build_active_mit_piper(1_000, left_sent.clone()),
            right: build_active_mit_piper_with_rx_and_tx_adapter(
                FailAfterFramesRxAdapter::new(scripted_frames(1_000)),
                RecordingTxAdapter::new(right_sent),
                Duration::from_millis(20),
            ),
        };

        let exit = arms
            .run_bilateral(
                ForwardingController::default(),
                BilateralLoopConfig {
                    frequency_hz: 50.0,
                    warmup_cycles: 0,
                    max_iterations: Some(10),
                    gripper: GripperTeleopConfig {
                        enabled: false,
                        ..Default::default()
                    },
                    read_policy: DualArmReadPolicy {
                        per_arm: ControlReadPolicy {
                            max_state_skew_us: 2_000,
                            max_feedback_age: Duration::from_secs(1),
                        },
                        max_inter_arm_skew: Duration::from_secs(1),
                    },
                    ..Default::default()
                },
            )
            .expect("rx fatal should converge to faulted exit");

        match exit {
            DualArmLoopExit::Standby { .. } => panic!("expected faulted exit"),
            DualArmLoopExit::Faulted { report, .. } => {
                assert_eq!(report.submission_faults, 0);
                assert_eq!(
                    report.exit_reason,
                    Some(BilateralExitReason::RuntimeTransportFault)
                );
                assert_eq!(
                    report.last_runtime_fault_right,
                    Some(RuntimeFaultKind::TransportError)
                );
                assert_eq!(report.left_stop_attempt, StopAttemptResult::ConfirmedSent);
                assert_eq!(report.right_stop_attempt, StopAttemptResult::ConfirmedSent);
            },
        }

        let left_frames = wait_for_sent_frames(&left_sent, 1);
        let hold = MitControlCommand::try_new(1, 0.0, 0.0, 5.0, 0.8, 0.0)
            .expect("expected hold command")
            .to_frame();
        assert!(
            !left_frames.iter().any(|frame| frame.id == hold.id && frame.data == hold.data),
            "runtime fault path should skip hold injection even when tx remains alive",
        );
    }

    #[test]
    fn test_fault_shutdown_preserves_ready_stop_ack_after_shared_deadline_passes() {
        let left_sent = Arc::new(Mutex::new(Vec::new()));
        let right_sent = Arc::new(Mutex::new(Vec::new()));
        let arms = DualArmActiveMit {
            left: build_piper_with_tx_adapter(
                1_000,
                SlowRecordingTxAdapter {
                    delay: Duration::from_millis(50),
                    sent_frames: left_sent.clone(),
                },
                Duration::ZERO,
                active_mit_marker(),
            ),
            right: build_active_mit_piper(1_000, right_sent.clone()),
        };

        let (_arms, shutdown) = arms.fault_shutdown(FAULT_SHUTDOWN_TIMEOUT);

        assert_eq!(shutdown.left_stop_attempt, StopAttemptResult::Timeout);
        assert_eq!(
            shutdown.right_stop_attempt,
            StopAttemptResult::ConfirmedSent
        );
        let right_frames = wait_for_sent_frames(&right_sent, 1);
        assert_eq!(right_frames.len(), 1);
    }

    #[test]
    fn test_run_bilateral_ignores_connected_false_for_faulted_exit() {
        let left_sent = Arc::new(Mutex::new(Vec::new()));
        let right_sent = Arc::new(Mutex::new(Vec::new()));
        let stale_but_acceptable_delay = Duration::from_millis(1_100);
        let arms = DualArmActiveMit {
            left: build_active_mit_piper_with_delay(1_000, left_sent, stale_but_acceptable_delay),
            right: build_active_mit_piper_with_delay(1_000, right_sent, stale_but_acceptable_delay),
        };

        let exit = arms
            .run_bilateral(
                ForwardingController::default(),
                BilateralLoopConfig {
                    warmup_cycles: 0,
                    max_iterations: Some(1),
                    gripper: GripperTeleopConfig {
                        enabled: false,
                        ..Default::default()
                    },
                    read_policy: DualArmReadPolicy {
                        per_arm: ControlReadPolicy {
                            max_state_skew_us: 2_000,
                            max_feedback_age: Duration::from_secs(5),
                        },
                        max_inter_arm_skew: Duration::from_secs(1),
                    },
                    ..Default::default()
                },
            )
            .expect("connected=false alone should not trigger faulted exit");

        match exit {
            DualArmLoopExit::Standby { report, .. } => {
                assert_eq!(report.exit_reason, Some(BilateralExitReason::ReadFault));
                assert!(report.read_faults >= 1);
                assert_eq!(report.left_stop_attempt, StopAttemptResult::NotAttempted);
                assert_eq!(report.right_stop_attempt, StopAttemptResult::NotAttempted);
                assert_eq!(report.last_runtime_fault_left, None);
                assert_eq!(report.last_runtime_fault_right, None);
            },
            DualArmLoopExit::Faulted { .. } => {
                panic!("connected=false should remain on the non-faulted path");
            },
        }
    }

    #[test]
    fn test_run_bilateral_executes_single_iteration_and_returns_standby() {
        let left_sent = Arc::new(Mutex::new(Vec::new()));
        let right_sent = Arc::new(Mutex::new(Vec::new()));
        let arms = DualArmActiveMit {
            left: build_active_mit_piper(1_000, left_sent.clone()),
            right: build_active_mit_piper(1_000, right_sent.clone()),
        };

        let calibration = DualArmCalibration {
            master_zero: JointArray::splat(Rad(0.0)),
            slave_zero: JointArray::splat(Rad(0.0)),
            map: JointMirrorMap::left_right_mirror(),
        };
        let controller = MasterFollowerController::new(calibration);
        let exit = arms
            .run_bilateral(
                controller,
                BilateralLoopConfig {
                    warmup_cycles: 0,
                    max_iterations: Some(1),
                    read_policy: DualArmReadPolicy {
                        per_arm: ControlReadPolicy {
                            max_state_skew_us: 2_000,
                            max_feedback_age: Duration::from_secs(1),
                        },
                        max_inter_arm_skew: Duration::from_secs(1),
                    },
                    ..Default::default()
                },
            )
            .expect("bilateral run should succeed");

        match exit {
            DualArmLoopExit::Standby { report, .. } => {
                assert_eq!(report.iterations, 1);
            },
            DualArmLoopExit::Faulted { .. } => {
                panic!("expected standby exit");
            },
        }

        let left_frames = wait_for_sent_frames(&left_sent, 7);
        assert_eq!(left_frames[0].id, 0x15A);
    }

    #[test]
    fn test_run_bilateral_respects_cancel_signal() {
        let left_sent = Arc::new(Mutex::new(Vec::new()));
        let right_sent = Arc::new(Mutex::new(Vec::new()));
        let arms = DualArmActiveMit {
            left: build_active_mit_piper(1_000, left_sent),
            right: build_active_mit_piper(1_000, right_sent),
        };

        let calibration = DualArmCalibration {
            master_zero: JointArray::splat(Rad(0.0)),
            slave_zero: JointArray::splat(Rad(0.0)),
            map: JointMirrorMap::left_right_mirror(),
        };
        let controller = MasterFollowerController::new(calibration);
        let cancel_signal = Arc::new(AtomicBool::new(true));
        let exit = arms
            .run_bilateral(
                controller,
                BilateralLoopConfig {
                    cancel_signal: Some(cancel_signal),
                    ..Default::default()
                },
            )
            .expect("bilateral run should exit cleanly");

        match exit {
            DualArmLoopExit::Standby { report, .. } => {
                assert_eq!(report.iterations, 0);
                assert_eq!(report.exit_reason, Some(BilateralExitReason::Cancelled));
                assert_eq!(
                    report.last_error.as_deref(),
                    Some("bilateral loop cancelled")
                );
            },
            DualArmLoopExit::Faulted { .. } => {
                panic!("expected standby exit");
            },
        }
    }

    #[test]
    fn test_run_bilateral_with_compensation_disables_after_second_failure() {
        let left_sent = Arc::new(Mutex::new(Vec::new()));
        let right_sent = Arc::new(Mutex::new(Vec::new()));
        let arms = DualArmActiveMit {
            left: build_active_mit_piper(1_000, left_sent.clone()),
            right: build_active_mit_piper(1_000, right_sent.clone()),
        };

        let controller = ForwardingController::default();
        let compensator = FakeCompensator::from_results([
            Err(FakeCompensationError {
                message: "first compensation failure",
            }),
            Err(FakeCompensationError {
                message: "second compensation failure",
            }),
        ]);

        let exit = arms
            .run_bilateral_with_compensation(
                controller,
                compensator,
                BilateralLoopConfig {
                    warmup_cycles: 0,
                    read_policy: DualArmReadPolicy {
                        per_arm: ControlReadPolicy {
                            max_state_skew_us: 2_000,
                            max_feedback_age: Duration::from_secs(1),
                        },
                        max_inter_arm_skew: Duration::from_secs(1),
                    },
                    ..Default::default()
                },
            )
            .expect("bilateral run should converge to standby");

        match exit {
            DualArmLoopExit::Standby { report, .. } => {
                assert!(report.iterations >= 1);
                assert_eq!(
                    report.exit_reason,
                    Some(BilateralExitReason::CompensationFault)
                );
                assert_eq!(
                    report.last_error.as_deref(),
                    Some("second compensation failure")
                );
            },
            DualArmLoopExit::Faulted { .. } => {
                panic!("expected standby exit");
            },
        }

        let left_frames = wait_for_sent_frames(&left_sent, 7);
        assert!(left_frames.iter().any(|frame| frame.id == 0x15A));
    }

    #[test]
    fn test_run_bilateral_with_compensation_resets_compensator_before_run() {
        let left_sent = Arc::new(Mutex::new(Vec::new()));
        let right_sent = Arc::new(Mutex::new(Vec::new()));
        let arms = DualArmActiveMit {
            left: build_active_mit_piper(1_000, left_sent),
            right: build_active_mit_piper(1_000, right_sent),
        };

        let controller = SlowForwardingController {
            sleep_duration: Duration::from_millis(10),
        };
        let compensator = FakeCompensator::from_results([Ok(BilateralDynamicsCompensation {
            master_model_torque: JointArray::splat(NewtonMeter::ZERO),
            slave_model_torque: JointArray::splat(NewtonMeter::ZERO),
            master_external_torque_est: JointArray::splat(NewtonMeter::ZERO),
            slave_external_torque_est: JointArray::splat(NewtonMeter::ZERO),
        })]);
        let reset_counter = compensator.reset_counter();

        let exit = arms
            .run_bilateral_with_compensation(
                controller,
                compensator,
                BilateralLoopConfig {
                    warmup_cycles: 0,
                    max_iterations: Some(3),
                    frequency_hz: 2_000.0,
                    dt_clamp_multiplier: 0.01,
                    read_policy: DualArmReadPolicy {
                        per_arm: ControlReadPolicy {
                            max_state_skew_us: 2_000,
                            max_feedback_age: Duration::from_secs(1),
                        },
                        max_inter_arm_skew: Duration::from_millis(10),
                    },
                    ..Default::default()
                },
            )
            .expect("bilateral run should succeed");

        match exit {
            DualArmLoopExit::Standby { .. } => {},
            DualArmLoopExit::Faulted { .. } => {
                panic!("expected standby exit");
            },
        }

        assert_eq!(reset_counter.load(AtomicOrdering::Relaxed), 1);
    }
}
