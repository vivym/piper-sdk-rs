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

use thiserror::Error;

use crate::builder::PiperBuilder;
use crate::observer::{ControlReadPolicy, ControlSnapshotFull, Observer, RuntimeHealthSnapshot};
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
        map.validate()?;
        Ok(DualArmCalibration {
            master_zero: self.left.observer().joint_positions(),
            slave_zero: self.right.observer().joint_positions(),
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

    pub fn safe_hold(&self, cfg: &DualArmSafetyConfig) -> Result<()> {
        let left_positions = self.left.observer().joint_positions();
        let right_positions = self.right.observer().joint_positions();
        let zero_velocities = JointArray::splat(0.0);
        let zero_torques = JointArray::splat(NewtonMeter::ZERO);

        self.right.command_torques(
            &right_positions,
            &zero_velocities,
            &cfg.safe_hold_kp,
            &cfg.safe_hold_kd,
            &zero_torques,
        )?;
        self.left.command_torques(
            &left_positions,
            &zero_velocities,
            &cfg.safe_hold_kp,
            &cfg.safe_hold_kd,
            &zero_torques,
        )?;
        Ok(())
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
        let mut last_time = Instant::now();
        let mut iteration = 0usize;
        let mut read_failure_streak = 0u32;
        let mut read_failure_since: Option<Instant> = None;
        let mut compensation_failure_streak = 0u32;
        let mut gripper_counter = 0usize;

        loop {
            if let Some(max_iterations) = cfg.max_iterations
                && iteration >= max_iterations
            {
                let arms =
                    active.disable_both(cfg.disable_config.clone()).map_err(DualArmError::from)?;
                update_report_metrics(&mut report, &arms.left.driver, &arms.right.driver);
                return Ok(DualArmLoopExit::Standby { arms, report });
            }

            if let Some(cancel_signal) = &cfg.cancel_signal
                && cancel_signal.load(Ordering::Acquire)
            {
                report.last_error = Some("bilateral loop cancelled".to_string());
                let arms =
                    active.disable_both(cfg.disable_config.clone()).map_err(DualArmError::from)?;
                update_report_metrics(&mut report, &arms.left.driver, &arms.right.driver);
                return Ok(DualArmLoopExit::Standby { arms, report });
            }

            let now = Instant::now();
            let real_dt = now.saturating_duration_since(last_time);
            let mut dt = real_dt;
            if real_dt > max_dt {
                controller
                    .on_time_jump(real_dt)
                    .map_err(|err| DualArmError::Controller(err.to_string()))?;
                if let Some(compensator) = compensator.as_deref_mut()
                    && let Err(err) = compensator.on_time_jump(real_dt)
                {
                    compensation_failure_streak += 1;
                    report.last_error = Some(err);
                    if active.safe_hold(&cfg.safety).is_err() || compensation_failure_streak > 1 {
                        let arms = active
                            .disable_both(cfg.disable_config.clone())
                            .map_err(DualArmError::from)?;
                        update_report_metrics(&mut report, &arms.left.driver, &arms.right.driver);
                        return Ok(DualArmLoopExit::Standby { arms, report });
                    }

                    report.iterations += 1;
                    iteration += 1;
                    last_time = now;
                    sleep_until_next_cycle(cfg.timing_mode, nominal_period);
                    continue;
                }
                dt = max_dt;
            }

            let health = active.observer().runtime_health();
            if health.any_unhealthy() {
                report.last_error = Some("runtime health unhealthy".to_string());
                let arms = active.emergency_stop_both().map_err(DualArmError::from)?;
                update_report_metrics(&mut report, &arms.left.driver, &arms.right.driver);
                return Ok(DualArmLoopExit::EmergencyStopped { arms, report });
            }

            if iteration < cfg.warmup_cycles {
                active.safe_hold(&cfg.safety).map_err(DualArmError::from)?;
                report.iterations += 1;
                iteration += 1;
                last_time = now;
                sleep_until_next_cycle(cfg.timing_mode, nominal_period);
                continue;
            }

            let snapshot = match active.observer().snapshot(cfg.read_policy) {
                Ok(snapshot) => {
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
                    if active.safe_hold(&cfg.safety).is_err()
                        || read_failure_streak
                            >= cfg.safety.consecutive_read_failures_before_disable
                        || failure_start.elapsed() >= cfg.safety.safe_hold_max_duration
                    {
                        let arms = active
                            .disable_both(cfg.disable_config.clone())
                            .map_err(DualArmError::from)?;
                        update_report_metrics(&mut report, &arms.left.driver, &arms.right.driver);
                        return Ok(DualArmLoopExit::Standby { arms, report });
                    }

                    report.iterations += 1;
                    iteration += 1;
                    last_time = now;
                    sleep_until_next_cycle(cfg.timing_mode, nominal_period);
                    continue;
                },
            };

            let compensation = if let Some(compensator) = compensator.as_deref_mut() {
                match compensator.compute(&snapshot, dt) {
                    Ok(compensation) => {
                        compensation_failure_streak = 0;
                        Some(compensation)
                    },
                    Err(err) => {
                        compensation_failure_streak += 1;
                        report.last_error = Some(err);
                        if active.safe_hold(&cfg.safety).is_err() || compensation_failure_streak > 1
                        {
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
                        last_time = now;
                        sleep_until_next_cycle(cfg.timing_mode, nominal_period);
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
                    report.last_error = Some(err.to_string());
                    if active.safe_hold(&cfg.safety).is_err() {
                        let arms = active.emergency_stop_both().map_err(DualArmError::from)?;
                        update_report_metrics(&mut report, &arms.left.driver, &arms.right.driver);
                        return Ok(DualArmLoopExit::EmergencyStopped { arms, report });
                    }
                    let arms = active
                        .disable_both(cfg.disable_config.clone())
                        .map_err(DualArmError::from)?;
                    update_report_metrics(&mut report, &arms.left.driver, &arms.right.driver);
                    return Ok(DualArmLoopExit::Standby { arms, report });
                },
            };

            if let Some(compensation) = frame.compensation {
                apply_model_compensation(&mut command, compensation);
            }

            apply_output_shaping(&cfg, &frame.snapshot, dt, &mut shaping_state, &mut command);

            if let Err(err) = active.right.command_torques(
                &command.slave_position,
                &command.slave_velocity,
                &command.slave_kp,
                &command.slave_kd,
                &command.slave_torque,
            ) {
                report.command_faults += 1;
                report.last_error = Some(err.to_string());
                let _ = active.left.command_torques(
                    &frame.snapshot.left.state.position,
                    &JointArray::splat(0.0),
                    &cfg.safety.safe_hold_kp,
                    &cfg.safety.safe_hold_kd,
                    &JointArray::splat(NewtonMeter::ZERO),
                );
                let arms = active.emergency_stop_both().map_err(DualArmError::from)?;
                update_report_metrics(&mut report, &arms.left.driver, &arms.right.driver);
                return Ok(DualArmLoopExit::EmergencyStopped { arms, report });
            }

            if let Err(err) = active.left.command_torques(
                &command.master_position,
                &command.master_velocity,
                &command.master_kp,
                &command.master_kd,
                &command.master_torque,
            ) {
                report.command_faults += 1;
                report.last_error = Some(err.to_string());
                let _ = active.right.command_torques(
                    &frame.snapshot.right.state.position,
                    &JointArray::splat(0.0),
                    &cfg.safety.safe_hold_kp,
                    &cfg.safety.safe_hold_kd,
                    &JointArray::splat(NewtonMeter::ZERO),
                );
                let arms = active.emergency_stop_both().map_err(DualArmError::from)?;
                update_report_metrics(&mut report, &arms.left.driver, &arms.right.driver);
                return Ok(DualArmLoopExit::EmergencyStopped { arms, report });
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
            last_time = now;
            sleep_until_next_cycle(cfg.timing_mode, nominal_period);
        }
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

    pub fn snapshot(&self, policy: DualArmReadPolicy) -> Result<DualArmSnapshot> {
        let left = self.left.control_snapshot_full(policy.per_arm)?;
        let right = self.right.control_snapshot_full(policy.per_arm)?;

        let left_us = left.latest_system_timestamp_us();
        let right_us = right.latest_system_timestamp_us();
        let diff_us = left_us.abs_diff(right_us);
        let inter_arm_skew = Duration::from_micros(diff_us);
        if inter_arm_skew > policy.max_inter_arm_skew {
            return Err(RobotError::state_misaligned(
                left_us as i64 - right_us as i64,
                policy.max_inter_arm_skew.as_micros() as u64,
            ));
        }

        Ok(DualArmSnapshot {
            left,
            right,
            inter_arm_skew,
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

/// 双臂安全策略
#[derive(Debug, Clone)]
pub struct DualArmSafetyConfig {
    pub safe_hold_kp: JointArray<f64>,
    pub safe_hold_kd: JointArray<f64>,
    pub safe_hold_max_duration: Duration,
    pub consecutive_read_failures_before_disable: u32,
    pub consecutive_command_failures_before_emergency_stop: u32,
    pub runtime_fault_action: RuntimeFaultAction,
    pub read_fault_action: ReadFaultAction,
    pub controller_error_action: ControllerFaultAction,
    pub compensation_error_action: CompensationFaultAction,
}

impl Default for DualArmSafetyConfig {
    fn default() -> Self {
        Self {
            safe_hold_kp: JointArray::splat(5.0),
            safe_hold_kd: JointArray::splat(0.8),
            safe_hold_max_duration: Duration::from_millis(100),
            consecutive_read_failures_before_disable: 3,
            consecutive_command_failures_before_emergency_stop: 1,
            runtime_fault_action: RuntimeFaultAction::EmergencyStopBoth,
            read_fault_action: ReadFaultAction::SafeHoldThenDisable,
            controller_error_action: ControllerFaultAction::SafeHoldThenDisable,
            compensation_error_action: CompensationFaultAction::SafeHoldThenDisable,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeFaultAction {
    EmergencyStopBoth,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadFaultAction {
    SafeHoldThenDisable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControllerFaultAction {
    SafeHoldThenDisable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompensationFaultAction {
    SafeHoldThenDisable,
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
    pub reflection_lpf_cutoff_hz: f64,
    pub master_reflection_limit: JointArray<NewtonMeter>,
    pub slave_feedforward_limit: JointArray<NewtonMeter>,
    pub reflection_slew_limit: JointArray<NewtonMeter>,
    pub passivity_enabled: bool,
    pub passivity_max_damping: JointArray<f64>,
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
            reflection_lpf_cutoff_hz: 20.0,
            master_reflection_limit: JointArray::splat(NewtonMeter(1.5)),
            slave_feedforward_limit: JointArray::splat(NewtonMeter(4.0)),
            reflection_slew_limit: JointArray::splat(NewtonMeter(0.25)),
            passivity_enabled: true,
            passivity_max_damping: JointArray::splat(1.0),
        }
    }
}

/// 双臂运行统计
#[derive(Debug, Clone, PartialEq)]
pub struct BilateralRunReport {
    pub iterations: usize,
    pub read_faults: u32,
    pub command_faults: u32,
    pub max_inter_arm_skew: Duration,
    pub left_tx_realtime_overwrites: u64,
    pub right_tx_realtime_overwrites: u64,
    pub left_tx_frames_total: u64,
    pub right_tx_frames_total: u64,
    pub last_error: Option<String>,
}

impl Default for BilateralRunReport {
    fn default() -> Self {
        Self {
            iterations: 0,
            read_faults: 0,
            command_faults: 0,
            max_inter_arm_skew: Duration::ZERO,
            left_tx_realtime_overwrites: 0,
            right_tx_realtime_overwrites: 0,
            left_tx_frames_total: 0,
            right_tx_frames_total: 0,
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
    EmergencyStopped {
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
    pub slave_torque: JointArray<NewtonMeter>,
    pub master_position: JointArray<Rad>,
    pub master_velocity: JointArray<f64>,
    pub master_kp: JointArray<f64>,
    pub master_kd: JointArray<f64>,
    pub master_torque: JointArray<NewtonMeter>,
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
            slave_torque: JointArray::splat(NewtonMeter::ZERO),
            master_position: snapshot.left.state.position,
            master_velocity: JointArray::splat(0.0),
            master_kp: JointArray::splat(0.0),
            master_kd: self.master_damping,
            master_torque: JointArray::splat(NewtonMeter::ZERO),
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
            slave_torque: JointArray::splat(NewtonMeter::ZERO),
            master_position: frame.snapshot.left.state.position,
            master_velocity: JointArray::splat(0.0),
            master_kp: JointArray::splat(0.0),
            master_kd: self.master_damping,
            master_torque: mapped_slave_torque,
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
    reflection_filtered: JointArray<NewtonMeter>,
    last_master_reflection: JointArray<NewtonMeter>,
    passivity_energy: f64,
}

impl Default for OutputShapingState {
    fn default() -> Self {
        Self {
            reflection_filtered: JointArray::splat(NewtonMeter::ZERO),
            last_master_reflection: JointArray::splat(NewtonMeter::ZERO),
            passivity_energy: 0.0,
        }
    }
}

fn apply_model_compensation(
    command: &mut BilateralCommand,
    compensation: BilateralDynamicsCompensation,
) {
    for joint in Joint::ALL {
        command.master_torque[joint] =
            NewtonMeter(command.master_torque[joint].0 + compensation.master_model_torque[joint].0);
        command.slave_torque[joint] =
            NewtonMeter(command.slave_torque[joint].0 + compensation.slave_model_torque[joint].0);
    }
}

fn apply_output_shaping(
    cfg: &BilateralLoopConfig,
    snapshot: &DualArmSnapshot,
    dt: Duration,
    state: &mut OutputShapingState,
    command: &mut BilateralCommand,
) {
    let dt_sec = dt.as_secs_f64().max(f64::EPSILON);
    let rc = if cfg.reflection_lpf_cutoff_hz > 0.0 {
        1.0 / (2.0 * std::f64::consts::PI * cfg.reflection_lpf_cutoff_hz)
    } else {
        0.0
    };
    let alpha = if rc > 0.0 {
        dt_sec / (rc + dt_sec)
    } else {
        1.0
    };

    for joint in Joint::ALL {
        let raw = command.master_torque[joint];
        let filtered = NewtonMeter(
            state.reflection_filtered[joint].0
                + alpha * (raw.0 - state.reflection_filtered[joint].0),
        );
        state.reflection_filtered[joint] = filtered;

        let last = state.last_master_reflection[joint];
        let limit = cfg.reflection_slew_limit[joint].0;
        let delta = (filtered.0 - last.0).clamp(-limit, limit);
        let shaped = NewtonMeter(last.0 + delta).clamp(
            -cfg.master_reflection_limit[joint],
            cfg.master_reflection_limit[joint],
        );
        command.master_torque[joint] = shaped;
    }

    let power: f64 = Joint::ALL
        .into_iter()
        .map(|joint| command.master_torque[joint].0 * snapshot.left.state.velocity[joint].0)
        .sum();
    state.passivity_energy = (state.passivity_energy + power * dt_sec).max(0.0);

    if cfg.passivity_enabled && state.passivity_energy > 0.0 {
        let velocity_sq: f64 = Joint::ALL
            .into_iter()
            .map(|joint| snapshot.left.state.velocity[joint].0.powi(2))
            .sum();
        if velocity_sq > f64::EPSILON {
            let target_damping = (state.passivity_energy / (velocity_sq * dt_sec)).max(0.0);
            let damping = Joint::ALL.into_iter().fold(JointArray::splat(0.0), |mut acc, joint| {
                acc[joint] = target_damping.min(cfg.passivity_max_damping[joint]);
                acc
            });

            let mut dissipated = 0.0;
            for joint in Joint::ALL {
                let tau_damp = -damping[joint] * snapshot.left.state.velocity[joint].0;
                command.master_torque[joint] =
                    NewtonMeter(command.master_torque[joint].0 + tau_damp).clamp(
                        -cfg.master_reflection_limit[joint],
                        cfg.master_reflection_limit[joint],
                    );
                dissipated +=
                    damping[joint] * snapshot.left.state.velocity[joint].0.powi(2) * dt_sec;
            }
            state.passivity_energy = (state.passivity_energy - dissipated).max(0.0);
        }
    }

    for joint in Joint::ALL {
        command.slave_torque[joint] = command.slave_torque[joint].clamp(
            -cfg.slave_feedforward_limit[joint],
            cfg.slave_feedforward_limit[joint],
        );
        state.last_master_reflection[joint] = command.master_torque[joint];
    }
}

fn sleep_until_next_cycle(mode: LoopTimingMode, period: Duration) {
    match mode {
        LoopTimingMode::Sleep => std::thread::sleep(period),
        LoopTimingMode::Spin => spin_sleep::SpinSleeper::default().sleep(period),
    }
}

fn update_report_metrics(
    report: &mut BilateralRunReport,
    left: &piper_driver::Piper,
    right: &piper_driver::Piper,
) {
    let left_metrics = left.get_metrics();
    let right_metrics = right.get_metrics();
    report.left_tx_realtime_overwrites = left_metrics.tx_realtime_overwrites;
    report.right_tx_realtime_overwrites = right_metrics.tx_realtime_overwrites;
    report.left_tx_frames_total = left_metrics.tx_frames_total;
    report.right_tx_frames_total = right_metrics.tx_frames_total;
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

    struct RecordingTxAdapter {
        sent_frames: Arc<Mutex<Vec<PiperFrame>>>,
    }

    impl RecordingTxAdapter {
        fn new(sent_frames: Arc<Mutex<Vec<PiperFrame>>>) -> Self {
            Self { sent_frames }
        }
    }

    impl TxAdapter for RecordingTxAdapter {
        fn send(&mut self, frame: PiperFrame) -> std::result::Result<(), CanError> {
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

    fn build_active_mit_piper(
        timestamp_us: u64,
        sent_frames: Arc<Mutex<Vec<PiperFrame>>>,
    ) -> Piper<Active<MitMode>> {
        let driver = Arc::new(
            RobotPiper::new_dual_thread_parts(
                ScriptedRxAdapter::new(scripted_frames(timestamp_us)),
                RecordingTxAdapter::new(sent_frames),
                None,
            )
            .expect("driver should start"),
        );
        let observer = Observer::new(driver.clone());
        driver
            .wait_for_feedback(Duration::from_millis(200))
            .expect("feedback should arrive");
        thread::sleep(Duration::from_millis(20));

        Piper {
            driver,
            observer,
            quirks: crate::types::DeviceQuirks::from_firmware_version(Version::new(1, 8, 3)),
            _state: active_mit_marker(),
        }
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

        fn time_jump_counter(&self) -> Arc<AtomicUsize> {
            self.time_jump_calls.clone()
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
                slave_torque: JointArray::splat(NewtonMeter::ZERO),
                master_position: JointArray::splat(Rad(0.0)),
                master_velocity: JointArray::splat(0.0),
                master_kp: JointArray::splat(0.0),
                master_kd: JointArray::splat(0.0),
                master_torque: JointArray::splat(NewtonMeter::ZERO),
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

        assert_eq!(output.master_torque[Joint::J1], NewtonMeter(1.0));
        assert_eq!(output.master_torque[Joint::J2], NewtonMeter(-1.0));
        assert_eq!(output.master_torque[Joint::J4], NewtonMeter(1.0));
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

        assert_eq!(output.master_torque[Joint::J1], NewtonMeter(1.0));
        assert_eq!(output.master_torque[Joint::J2], NewtonMeter(-1.0));
    }

    #[test]
    fn test_apply_output_shaping_limits_slew_and_clamps_slave_feedforward() {
        let snapshot = snapshot_with_state(
            JointArray::splat(Rad(0.0)),
            JointArray::splat(RadPerSecond(0.0)),
            JointArray::splat(NewtonMeter::ZERO),
        );
        let cfg = BilateralLoopConfig {
            reflection_lpf_cutoff_hz: 0.0,
            reflection_slew_limit: JointArray::splat(NewtonMeter(0.25)),
            master_reflection_limit: JointArray::splat(NewtonMeter(0.5)),
            slave_feedforward_limit: JointArray::splat(NewtonMeter(4.0)),
            passivity_enabled: false,
            ..Default::default()
        };
        let mut state = OutputShapingState::default();
        let mut command = BilateralCommand {
            slave_position: JointArray::splat(Rad(0.0)),
            slave_velocity: JointArray::splat(0.0),
            slave_kp: JointArray::splat(0.0),
            slave_kd: JointArray::splat(0.0),
            slave_torque: JointArray::splat(NewtonMeter(10.0)),
            master_position: JointArray::splat(Rad(0.0)),
            master_velocity: JointArray::splat(0.0),
            master_kp: JointArray::splat(0.0),
            master_kd: JointArray::splat(0.0),
            master_torque: JointArray::splat(NewtonMeter(2.0)),
        };

        apply_output_shaping(
            &cfg,
            &snapshot,
            Duration::from_millis(5),
            &mut state,
            &mut command,
        );
        assert_eq!(command.master_torque, JointArray::splat(NewtonMeter(0.25)));
        assert_eq!(command.slave_torque, JointArray::splat(NewtonMeter(4.0)));

        command.master_torque = JointArray::splat(NewtonMeter(2.0));
        apply_output_shaping(
            &cfg,
            &snapshot,
            Duration::from_millis(5),
            &mut state,
            &mut command,
        );
        assert_eq!(command.master_torque, JointArray::splat(NewtonMeter(0.5)));
    }

    #[test]
    fn test_apply_output_shaping_injects_passivity_damping() {
        let snapshot = snapshot_with_state(
            JointArray::splat(Rad(0.0)),
            JointArray::splat(RadPerSecond(1.0)),
            JointArray::splat(NewtonMeter::ZERO),
        );
        let cfg = BilateralLoopConfig {
            reflection_lpf_cutoff_hz: 0.0,
            reflection_slew_limit: JointArray::splat(NewtonMeter(10.0)),
            master_reflection_limit: JointArray::splat(NewtonMeter(10.0)),
            passivity_enabled: true,
            passivity_max_damping: JointArray::splat(1.0),
            ..Default::default()
        };
        let mut state = OutputShapingState::default();
        let mut command = BilateralCommand {
            slave_position: JointArray::splat(Rad(0.0)),
            slave_velocity: JointArray::splat(0.0),
            slave_kp: JointArray::splat(0.0),
            slave_kd: JointArray::splat(0.0),
            slave_torque: JointArray::splat(NewtonMeter::ZERO),
            master_position: JointArray::splat(Rad(0.0)),
            master_velocity: JointArray::splat(0.0),
            master_kp: JointArray::splat(0.0),
            master_kd: JointArray::splat(0.0),
            master_torque: JointArray::splat(NewtonMeter(1.0)),
        };

        apply_output_shaping(
            &cfg,
            &snapshot,
            Duration::from_secs(1),
            &mut state,
            &mut command,
        );

        assert_eq!(command.master_torque, JointArray::splat(NewtonMeter(0.0)));
        assert_eq!(state.passivity_energy, 0.0);
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
                    max_iterations: Some(2),
                    gripper: GripperTeleopConfig {
                        enabled: false,
                        ..Default::default()
                    },
                    reflection_lpf_cutoff_hz: 0.0,
                    reflection_slew_limit: JointArray::splat(NewtonMeter(10.0)),
                    master_reflection_limit: JointArray::splat(NewtonMeter(10.0)),
                    passivity_enabled: false,
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
                assert_eq!(report.iterations, 2);
            },
            DualArmLoopExit::EmergencyStopped { .. } => {
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
    fn test_safe_hold_sends_current_position_zero_velocity_and_default_gains() {
        let left_sent = Arc::new(Mutex::new(Vec::new()));
        let right_sent = Arc::new(Mutex::new(Vec::new()));
        let arms = DualArmActiveMit {
            left: build_active_mit_piper(1_000, left_sent.clone()),
            right: build_active_mit_piper(1_000, right_sent.clone()),
        };

        arms.safe_hold(&DualArmSafetyConfig::default())
            .expect("safe hold should succeed");

        let left_frames = wait_for_sent_frames(&left_sent, 6);
        let expected = MitControlCommand::try_new(1, 0.0, 0.0, 5.0, 0.8, 0.0)
            .expect("expected command should be valid")
            .to_frame();
        assert_eq!(left_frames[0].id, expected.id);
        assert_eq!(left_frames[0].data, expected.data);
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
            DualArmLoopExit::EmergencyStopped { .. } => {
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
                assert_eq!(
                    report.last_error.as_deref(),
                    Some("bilateral loop cancelled")
                );
            },
            DualArmLoopExit::EmergencyStopped { .. } => {
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
                    report.last_error.as_deref(),
                    Some("second compensation failure")
                );
            },
            DualArmLoopExit::EmergencyStopped { .. } => {
                panic!("expected standby exit");
            },
        }

        let left_frames = wait_for_sent_frames(&left_sent, 7);
        assert!(left_frames.iter().any(|frame| frame.id == 0x15A));
    }

    #[test]
    fn test_run_bilateral_with_compensation_propagates_time_jump_to_compensator() {
        let left_sent = Arc::new(Mutex::new(Vec::new()));
        let right_sent = Arc::new(Mutex::new(Vec::new()));
        let arms = DualArmActiveMit {
            left: build_active_mit_piper(1_000, left_sent),
            right: build_active_mit_piper(1_000, right_sent),
        };

        let controller = ForwardingController::default();
        let compensator = FakeCompensator::from_results([Ok(BilateralDynamicsCompensation {
            master_model_torque: JointArray::splat(NewtonMeter::ZERO),
            slave_model_torque: JointArray::splat(NewtonMeter::ZERO),
            master_external_torque_est: JointArray::splat(NewtonMeter::ZERO),
            slave_external_torque_est: JointArray::splat(NewtonMeter::ZERO),
        })]);
        let time_jump_counter = compensator.time_jump_counter();
        let reset_counter = compensator.reset_counter();

        let exit = arms
            .run_bilateral_with_compensation(
                controller,
                compensator,
                BilateralLoopConfig {
                    warmup_cycles: 0,
                    max_iterations: Some(2),
                    frequency_hz: 1_000_000.0,
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
            DualArmLoopExit::Standby { report, .. } => {
                assert!(report.iterations >= 1);
            },
            DualArmLoopExit::EmergencyStopped { .. } => {
                panic!("expected standby exit");
            },
        }

        assert_eq!(reset_counter.load(AtomicOrdering::Relaxed), 1);
        assert!(
            time_jump_counter.load(AtomicOrdering::Relaxed) >= 1,
            "expected compensator on_time_jump to be called at least once"
        );
    }
}
