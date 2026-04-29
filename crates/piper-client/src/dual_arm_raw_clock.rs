//! Experimental dual-arm runtime gated by calibrated SocketCAN raw-clock timing.
//!
//! This module is intentionally separate from the production StrictRealtime dual-arm runtime.
//! Raw-clock timing is accepted only for this explicitly experimental SoftRealtime path.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use piper_driver::{AlignmentResult, DriverError, RuntimeFaultKind};
use piper_tools::raw_clock::{
    RawClockError, RawClockEstimator, RawClockHealth, RawClockSample, RawClockThresholds,
    RawClockUnhealthyKind,
};
use thiserror::Error;

use crate::dual_arm::{
    BilateralCommand, BilateralControlFrame, BilateralController, DualArmCalibration,
    DualArmSnapshot, MasterFollowerController, StopAttemptResult,
};
use crate::observer::{
    ControlReadPolicy, ControlSnapshot, ControlSnapshotFull, DEFAULT_CONTROL_MAX_FEEDBACK_AGE,
    RuntimeHealthSnapshot,
};
use crate::raw_commander::RawCommander;
use crate::state::machine::{DriverModeDropPolicy, DropPolicy};
use crate::state::{
    Active, DisableConfig, ErrorState, MitModeConfig, MitPassthroughMode, Piper, SoftRealtime,
    Standby,
};
use crate::types::{JointArray, NewtonMeter, Rad, RadPerSecond, Result as RobotResult, RobotError};

const FAULT_SHUTDOWN_TIMEOUT: Duration = Duration::from_millis(20);
const RAW_CLOCK_SKEW_RETAINED_SAMPLE_CAPACITY: usize = 4096;
const RAW_CLOCK_WARMUP_FINAL_GRACE_MIN_US: u64 = 100_000;
const RAW_CLOCK_WARMUP_FINAL_GRACE_MAX_US: u64 = 1_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExperimentalRawClockMode {
    MasterFollower,
    Bilateral,
}

#[derive(Debug, Clone)]
pub struct ExperimentalRawClockConfig {
    pub mode: ExperimentalRawClockMode,
    pub frequency_hz: f64,
    pub max_iterations: Option<usize>,
    pub thresholds: RawClockRuntimeThresholds,
    pub estimator_thresholds: RawClockThresholds,
}

impl Default for ExperimentalRawClockConfig {
    fn default() -> Self {
        Self {
            mode: ExperimentalRawClockMode::MasterFollower,
            frequency_hz: 200.0,
            max_iterations: None,
            thresholds: RawClockRuntimeThresholds::default(),
            estimator_thresholds: RawClockThresholds {
                warmup_samples: 8,
                warmup_window_us: 10_000,
                residual_p95_us: 100,
                residual_max_us: 250,
                drift_abs_ppm: 500.0,
                sample_gap_max_us: 5_000,
                last_sample_age_us: 20_000,
            },
        }
    }
}

impl ExperimentalRawClockConfig {
    pub fn with_mode(
        mut self,
        mode: ExperimentalRawClockMode,
    ) -> Result<Self, RawClockRuntimeError> {
        if matches!(mode, ExperimentalRawClockMode::Bilateral) {
            return Err(RawClockRuntimeError::Config(
                "experimental raw-clock runtime currently supports master-follower mode only"
                    .to_string(),
            ));
        }
        self.mode = mode;
        Ok(self)
    }

    pub fn validate(&self) -> Result<(), RawClockRuntimeError> {
        if matches!(self.mode, ExperimentalRawClockMode::Bilateral) {
            return Err(RawClockRuntimeError::Config(
                "experimental raw-clock runtime currently supports master-follower mode only"
                    .to_string(),
            ));
        }
        if !self.frequency_hz.is_finite() || self.frequency_hz <= 0.0 {
            return Err(RawClockRuntimeError::Config(
                "frequency_hz must be finite and > 0".to_string(),
            ));
        }
        if self.thresholds.alignment_lag_us == 0 {
            return Err(RawClockRuntimeError::Config(
                "alignment_lag_us must be greater than 0".to_string(),
            ));
        }
        if self.thresholds.alignment_buffer_miss_consecutive_failures == 0 {
            return Err(RawClockRuntimeError::Config(
                "alignment_buffer_miss_consecutive_failures must be greater than 0".to_string(),
            ));
        }
        if self.thresholds.alignment_lag_us >= self.thresholds.last_sample_age_us {
            return Err(RawClockRuntimeError::Config(format!(
                "alignment_lag_us {} must be less than runtime last_sample_age_us {}",
                self.thresholds.alignment_lag_us, self.thresholds.last_sample_age_us
            )));
        }
        if self.thresholds.alignment_lag_us >= self.estimator_thresholds.last_sample_age_us {
            return Err(RawClockRuntimeError::Config(format!(
                "alignment_lag_us {} must be less than estimator last_sample_age_us {}",
                self.thresholds.alignment_lag_us, self.estimator_thresholds.last_sample_age_us
            )));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ExperimentalRawClockMasterFollowerGains {
    pub track_kp: JointArray<f64>,
    pub track_kd: JointArray<f64>,
    pub master_damping: JointArray<f64>,
}

impl Default for ExperimentalRawClockMasterFollowerGains {
    fn default() -> Self {
        Self {
            track_kp: JointArray::splat(5.0),
            track_kd: JointArray::splat(0.8),
            master_damping: JointArray::splat(0.2),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawClockRuntimeThresholds {
    pub inter_arm_skew_max_us: u64,
    pub last_sample_age_us: u64,
    pub residual_max_consecutive_failures: u32,
    pub alignment_lag_us: u64,
    pub alignment_buffer_miss_consecutive_failures: u32,
}

impl Default for RawClockRuntimeThresholds {
    fn default() -> Self {
        Self {
            inter_arm_skew_max_us: 2_000,
            last_sample_age_us: 20_000,
            residual_max_consecutive_failures: 1,
            alignment_lag_us: 5_000,
            alignment_buffer_miss_consecutive_failures: 3,
        }
    }
}

impl RawClockRuntimeThresholds {
    #[cfg(test)]
    const fn for_tests() -> Self {
        Self {
            inter_arm_skew_max_us: 2_000,
            // Fake runtime tests use synthetic receive timestamps with the real
            // monotonic clock; dedicated gate tests cover age rejection.
            last_sample_age_us: u64::MAX,
            residual_max_consecutive_failures: 1,
            alignment_lag_us: 5_000,
            alignment_buffer_miss_consecutive_failures: 3,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RawClockTickTiming {
    pub master_feedback_time_us: u64,
    pub slave_feedback_time_us: u64,
    pub inter_arm_skew_us: u64,
    pub master_selected_sample_age_us: u64,
    pub slave_selected_sample_age_us: u64,
    pub master_health: RawClockHealth,
    pub slave_health: RawClockHealth,
}

#[derive(Debug, Clone)]
struct RawClockLatestTiming {
    master_feedback_time_us: u64,
    slave_feedback_time_us: u64,
    #[allow(dead_code)]
    latest_inter_arm_skew_us: u64,
    master_health: RawClockHealth,
    slave_health: RawClockHealth,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RawClockRuntimeReport {
    pub master: RawClockHealth,
    pub slave: RawClockHealth,
    pub joint_motion: Option<RawClockJointMotionStats>,
    pub max_inter_arm_skew_us: u64,
    pub inter_arm_skew_p95_us: u64,
    pub clock_health_failures: u64,
    pub master_residual_max_spikes: u64,
    pub slave_residual_max_spikes: u64,
    pub master_residual_max_consecutive_failures: u32,
    pub slave_residual_max_consecutive_failures: u32,
    pub read_faults: u32,
    pub submission_faults: u32,
    pub last_submission_failed_side: Option<RawClockSide>,
    pub peer_command_may_have_applied: bool,
    pub runtime_faults: u32,
    pub master_tx_realtime_overwrites_total: u64,
    pub slave_tx_realtime_overwrites_total: u64,
    pub master_tx_frames_sent_total: u64,
    pub slave_tx_frames_sent_total: u64,
    pub master_tx_fault_aborts_total: u64,
    pub slave_tx_fault_aborts_total: u64,
    pub last_runtime_fault_master: Option<RuntimeFaultKind>,
    pub last_runtime_fault_slave: Option<RuntimeFaultKind>,
    pub iterations: usize,
    pub exit_reason: Option<RawClockRuntimeExitReason>,
    pub master_stop_attempt: StopAttemptResult,
    pub slave_stop_attempt: StopAttemptResult,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RawClockJointMotionStats {
    pub master_feedback_min_rad: [f64; 6],
    pub master_feedback_max_rad: [f64; 6],
    pub master_feedback_delta_rad: [f64; 6],
    pub slave_command_min_rad: [f64; 6],
    pub slave_command_max_rad: [f64; 6],
    pub slave_command_delta_rad: [f64; 6],
    pub slave_feedback_min_rad: [f64; 6],
    pub slave_feedback_max_rad: [f64; 6],
    pub slave_feedback_delta_rad: [f64; 6],
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct RawClockJointMotionBounds {
    master_feedback_min_rad: [f64; 6],
    master_feedback_max_rad: [f64; 6],
    slave_command_min_rad: [f64; 6],
    slave_command_max_rad: [f64; 6],
    slave_feedback_min_rad: [f64; 6],
    slave_feedback_max_rad: [f64; 6],
}

impl RawClockJointMotionBounds {
    fn new(snapshot: &DualArmSnapshot, command: &BilateralCommand) -> Self {
        let master_feedback = rad_array(snapshot.left.state.position);
        let slave_command = rad_array(command.slave_position);
        let slave_feedback = rad_array(snapshot.right.state.position);
        Self {
            master_feedback_min_rad: master_feedback,
            master_feedback_max_rad: master_feedback,
            slave_command_min_rad: slave_command,
            slave_command_max_rad: slave_command,
            slave_feedback_min_rad: slave_feedback,
            slave_feedback_max_rad: slave_feedback,
        }
    }

    fn record(&mut self, snapshot: &DualArmSnapshot, command: &BilateralCommand) {
        update_bounds(
            &mut self.master_feedback_min_rad,
            &mut self.master_feedback_max_rad,
            rad_array(snapshot.left.state.position),
        );
        update_bounds(
            &mut self.slave_command_min_rad,
            &mut self.slave_command_max_rad,
            rad_array(command.slave_position),
        );
        update_bounds(
            &mut self.slave_feedback_min_rad,
            &mut self.slave_feedback_max_rad,
            rad_array(snapshot.right.state.position),
        );
    }

    fn snapshot(self) -> RawClockJointMotionStats {
        RawClockJointMotionStats {
            master_feedback_min_rad: self.master_feedback_min_rad,
            master_feedback_max_rad: self.master_feedback_max_rad,
            master_feedback_delta_rad: delta_array(
                self.master_feedback_min_rad,
                self.master_feedback_max_rad,
            ),
            slave_command_min_rad: self.slave_command_min_rad,
            slave_command_max_rad: self.slave_command_max_rad,
            slave_command_delta_rad: delta_array(
                self.slave_command_min_rad,
                self.slave_command_max_rad,
            ),
            slave_feedback_min_rad: self.slave_feedback_min_rad,
            slave_feedback_max_rad: self.slave_feedback_max_rad,
            slave_feedback_delta_rad: delta_array(
                self.slave_feedback_min_rad,
                self.slave_feedback_max_rad,
            ),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
struct RawClockJointMotionAccumulator {
    bounds: Option<RawClockJointMotionBounds>,
}

impl RawClockJointMotionAccumulator {
    fn record(&mut self, snapshot: &DualArmSnapshot, command: &BilateralCommand) {
        match &mut self.bounds {
            Some(bounds) => bounds.record(snapshot, command),
            None => self.bounds = Some(RawClockJointMotionBounds::new(snapshot, command)),
        }
    }

    fn snapshot(&self) -> Option<RawClockJointMotionStats> {
        self.bounds.map(RawClockJointMotionBounds::snapshot)
    }
}

fn rad_array(values: JointArray<Rad>) -> [f64; 6] {
    values.map(|value| value.0).into_array()
}

fn update_bounds(min: &mut [f64; 6], max: &mut [f64; 6], values: [f64; 6]) {
    for index in 0..6 {
        min[index] = min[index].min(values[index]);
        max[index] = max[index].max(values[index]);
    }
}

fn delta_array(min: [f64; 6], max: [f64; 6]) -> [f64; 6] {
    std::array::from_fn(|index| max[index] - min[index])
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RawClockRuntimeExitReason {
    MaxIterations,
    Cancelled,
    ReadFault,
    RawClockFault,
    ClockHealthFault,
    ControllerFault,
    SubmissionFault,
    RuntimeTransportFault,
    RuntimeManualFault,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RawClockSide {
    Master,
    Slave,
}

impl RawClockSide {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Master => "master",
            Self::Slave => "slave",
        }
    }

    fn from_str(side: &str) -> Option<Self> {
        match side {
            "master" => Some(Self::Master),
            "slave" => Some(Self::Slave),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct RawClockRuntimeTelemetry {
    master_tx_realtime_overwrites_total: u64,
    slave_tx_realtime_overwrites_total: u64,
    master_tx_frames_sent_total: u64,
    slave_tx_frames_sent_total: u64,
    master_tx_fault_aborts_total: u64,
    slave_tx_fault_aborts_total: u64,
    last_runtime_fault_master: Option<RuntimeFaultKind>,
    last_runtime_fault_slave: Option<RuntimeFaultKind>,
}

#[derive(Debug)]
struct RawClockDebouncedHealthError {
    error: RawClockRuntimeError,
    priority: RawClockDebouncedHealthErrorPriority,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RawClockDebouncedHealthErrorPriority {
    FailFast,
    ResidualMaxThreshold,
}

impl RawClockRuntimeReport {
    fn apply_telemetry(&mut self, telemetry: RawClockRuntimeTelemetry) {
        self.master_tx_realtime_overwrites_total = telemetry.master_tx_realtime_overwrites_total;
        self.slave_tx_realtime_overwrites_total = telemetry.slave_tx_realtime_overwrites_total;
        self.master_tx_frames_sent_total = telemetry.master_tx_frames_sent_total;
        self.slave_tx_frames_sent_total = telemetry.slave_tx_frames_sent_total;
        self.master_tx_fault_aborts_total = telemetry.master_tx_fault_aborts_total;
        self.slave_tx_fault_aborts_total = telemetry.slave_tx_fault_aborts_total;
        self.last_runtime_fault_master =
            self.last_runtime_fault_master.or(telemetry.last_runtime_fault_master);
        self.last_runtime_fault_slave =
            self.last_runtime_fault_slave.or(telemetry.last_runtime_fault_slave);
    }

    fn with_telemetry(mut self, telemetry: RawClockRuntimeTelemetry) -> Self {
        self.apply_telemetry(telemetry);
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExperimentalFaultShutdown {
    pub master_stop_attempt: StopAttemptResult,
    pub slave_stop_attempt: StopAttemptResult,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExperimentalRawClockRuntimeHealth {
    pub master: RuntimeHealthSnapshot,
    pub slave: RuntimeHealthSnapshot,
}

#[derive(Debug, Error)]
pub enum RawClockRuntimeError {
    #[error("experimental raw-clock configuration error: {0}")]
    Config(String),
    #[error("raw feedback timing missing for {side}")]
    MissingRawFeedbackTiming { side: &'static str },
    #[error("raw timestamp regression on {side}: previous={previous_raw_us}, current={raw_us}")]
    RawTimestampRegression {
        side: &'static str,
        previous_raw_us: u64,
        raw_us: u64,
    },
    #[error("raw-clock estimator is not ready for {side}")]
    EstimatorNotReady { side: &'static str },
    #[error("raw-clock estimator unhealthy for {side}: {health:?}")]
    ClockUnhealthy {
        side: &'static str,
        health: Box<RawClockHealth>,
    },
    #[error("inter-arm raw-clock skew {inter_arm_skew_us}us exceeds {max_us}us")]
    InterArmSkew { inter_arm_skew_us: u64, max_us: u64 },
    #[error("snapshot read failed on {side}: {source}")]
    ReadFault {
        side: &'static str,
        #[source]
        source: RobotError,
    },
    #[error("command submission failed on {side}: {source}")]
    SubmissionFault {
        side: &'static str,
        peer_command_may_have_applied: bool,
        #[source]
        source: RobotError,
    },
    #[error("runtime transport fault: {details}")]
    RuntimeTransportFault { details: String },
    #[error("controller fault: {0}")]
    Controller(String),
    #[error("raw-clock runtime cancelled")]
    Cancelled,
    #[error(
        "failed to enable {failed_side}: {source}; enabled side stop attempt={enabled_side_stop_attempt:?}"
    )]
    EnableFailed {
        failed_side: &'static str,
        #[source]
        source: RobotError,
        enabled_side_stop_attempt: Option<StopAttemptResult>,
    },
    #[error("disable_both failed: master={master:?}, slave={slave:?}")]
    DisableBothFailed {
        master: Option<String>,
        slave: Option<String>,
    },
}

#[derive(Debug, Clone, Copy)]
pub struct RawClockRuntimeGate {
    thresholds: RawClockRuntimeThresholds,
}

impl RawClockRuntimeGate {
    pub const fn new(thresholds: RawClockRuntimeThresholds) -> Self {
        Self { thresholds }
    }

    pub fn check_tick(&self, tick: RawClockTickTiming) -> Result<(), RawClockRuntimeError> {
        if tick.master_selected_sample_age_us > self.thresholds.last_sample_age_us {
            let mut health = tick.master_health.clone();
            health.last_sample_age_us = tick.master_selected_sample_age_us;
            return Err(RawClockRuntimeError::ClockUnhealthy {
                side: "master",
                health: Box::new(health),
            });
        }
        if tick.slave_selected_sample_age_us > self.thresholds.last_sample_age_us {
            let mut health = tick.slave_health.clone();
            health.last_sample_age_us = tick.slave_selected_sample_age_us;
            return Err(RawClockRuntimeError::ClockUnhealthy {
                side: "slave",
                health: Box::new(health),
            });
        }
        if !tick.master_health.healthy {
            return Err(RawClockRuntimeError::ClockUnhealthy {
                side: "master",
                health: Box::new(tick.master_health),
            });
        }
        if !tick.slave_health.healthy {
            return Err(RawClockRuntimeError::ClockUnhealthy {
                side: "slave",
                health: Box::new(tick.slave_health),
            });
        }
        if tick.master_health.last_sample_age_us > self.thresholds.last_sample_age_us {
            return Err(RawClockRuntimeError::ClockUnhealthy {
                side: "master",
                health: Box::new(tick.master_health),
            });
        }
        if tick.slave_health.last_sample_age_us > self.thresholds.last_sample_age_us {
            return Err(RawClockRuntimeError::ClockUnhealthy {
                side: "slave",
                health: Box::new(tick.slave_health),
            });
        }
        if tick.inter_arm_skew_us > self.thresholds.inter_arm_skew_max_us {
            return Err(RawClockRuntimeError::InterArmSkew {
                inter_arm_skew_us: tick.inter_arm_skew_us,
                max_us: self.thresholds.inter_arm_skew_max_us,
            });
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ExperimentalRawClockSnapshot {
    pub state: ControlSnapshot,
    pub newest_raw_feedback_timing: piper_driver::RawFeedbackTiming,
    pub feedback_age: Duration,
}

#[derive(Debug, Clone, PartialEq)]
#[allow(dead_code)]
struct RawClockAlignedSnapshot {
    feedback_time_us: u64,
    snapshot: ExperimentalRawClockSnapshot,
}

#[allow(dead_code)]
impl RawClockAlignedSnapshot {
    fn new(feedback_time_us: u64, snapshot: ExperimentalRawClockSnapshot) -> Self {
        Self {
            feedback_time_us,
            snapshot,
        }
    }
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
struct RawClockSnapshotBuffer {
    samples: VecDeque<RawClockAlignedSnapshot>,
    retention_us: u64,
}

#[allow(dead_code)]
impl RawClockSnapshotBuffer {
    fn new(retention_us: u64) -> Self {
        Self {
            samples: VecDeque::new(),
            retention_us,
        }
    }

    fn clear(&mut self) {
        self.samples.clear();
    }

    fn push(&mut self, sample: RawClockAlignedSnapshot) {
        let newest = sample.feedback_time_us;
        self.samples.push_back(sample);
        while self
            .samples
            .front()
            .is_some_and(|front| newest.saturating_sub(front.feedback_time_us) > self.retention_us)
        {
            self.samples.pop_front();
        }
    }

    fn latest_before_or_at(&self, target_time_us: u64) -> Option<&RawClockAlignedSnapshot> {
        self.samples
            .iter()
            .rev()
            .find(|sample| sample.feedback_time_us <= target_time_us)
    }
}

pub struct RawClockRuntimeTiming {
    master: RawClockEstimator,
    slave: RawClockEstimator,
    skew_samples_us: VecDeque<u64>,
    max_inter_arm_skew_us: u64,
    clock_health_failures: u64,
    last_master_raw_us: Option<u64>,
    last_slave_raw_us: Option<u64>,
    master_raw_timestamp_regressions: u64,
    slave_raw_timestamp_regressions: u64,
    master_residual_max_spikes: u64,
    slave_residual_max_spikes: u64,
    master_residual_max_consecutive_failures: u32,
    slave_residual_max_consecutive_failures: u32,
}

impl RawClockRuntimeTiming {
    pub fn new(thresholds: RawClockThresholds) -> Self {
        Self {
            master: RawClockEstimator::new(thresholds),
            slave: RawClockEstimator::new(thresholds),
            skew_samples_us: VecDeque::with_capacity(RAW_CLOCK_SKEW_RETAINED_SAMPLE_CAPACITY),
            max_inter_arm_skew_us: 0,
            clock_health_failures: 0,
            last_master_raw_us: None,
            last_slave_raw_us: None,
            master_raw_timestamp_regressions: 0,
            slave_raw_timestamp_regressions: 0,
            master_residual_max_spikes: 0,
            slave_residual_max_spikes: 0,
            master_residual_max_consecutive_failures: 0,
            slave_residual_max_consecutive_failures: 0,
        }
    }

    fn reset_for_warmup(&mut self) {
        self.master.reset();
        self.slave.reset();
        self.skew_samples_us.clear();
        self.max_inter_arm_skew_us = 0;
        self.clock_health_failures = 0;
        self.last_master_raw_us = None;
        self.last_slave_raw_us = None;
        self.master_raw_timestamp_regressions = 0;
        self.slave_raw_timestamp_regressions = 0;
        self.reset_runtime_residual_max_counters();
    }

    fn reset_runtime_residual_max_counters(&mut self) {
        self.master_residual_max_spikes = 0;
        self.slave_residual_max_spikes = 0;
        self.master_residual_max_consecutive_failures = 0;
        self.slave_residual_max_consecutive_failures = 0;
    }

    fn mark_continuity_boundary(&mut self) {
        self.master.mark_continuity_boundary();
        self.slave.mark_continuity_boundary();
    }

    pub fn ingest_snapshots(
        &mut self,
        master: &ExperimentalRawClockSnapshot,
        slave: &ExperimentalRawClockSnapshot,
    ) -> Result<(), RawClockRuntimeError> {
        self.ingest_one(RawClockSide::Master, master)?;
        self.ingest_one(RawClockSide::Slave, slave)?;
        Ok(())
    }

    pub fn tick_from_snapshots(
        &mut self,
        master: &ExperimentalRawClockSnapshot,
        slave: &ExperimentalRawClockSnapshot,
        now_host_us: u64,
    ) -> Result<RawClockTickTiming, RawClockRuntimeError> {
        let latest = self.ingest_latest_snapshots(master, slave, now_host_us)?;
        self.selected_tick_from_buffered_times(
            &latest,
            latest.master_feedback_time_us,
            latest.slave_feedback_time_us,
            now_host_us,
        )
    }

    fn ingest_latest_snapshots(
        &mut self,
        master: &ExperimentalRawClockSnapshot,
        slave: &ExperimentalRawClockSnapshot,
        now_host_us: u64,
    ) -> Result<RawClockLatestTiming, RawClockRuntimeError> {
        self.ingest_snapshots(master, slave)?;

        let master_raw_us = raw_hw_us(RawClockSide::Master, master)?;
        let slave_raw_us = raw_hw_us(RawClockSide::Slave, slave)?;
        let master_feedback_time_us = self.master.map_raw_us(master_raw_us).ok_or(
            RawClockRuntimeError::EstimatorNotReady {
                side: RawClockSide::Master.as_str(),
            },
        )?;
        let slave_feedback_time_us =
            self.slave
                .map_raw_us(slave_raw_us)
                .ok_or(RawClockRuntimeError::EstimatorNotReady {
                    side: RawClockSide::Slave.as_str(),
                })?;
        let latest_inter_arm_skew_us = master_feedback_time_us.abs_diff(slave_feedback_time_us);
        self.record_latest_skew_sample(latest_inter_arm_skew_us);

        Ok(RawClockLatestTiming {
            master_feedback_time_us,
            slave_feedback_time_us,
            latest_inter_arm_skew_us,
            master_health: self.master.health(now_host_us),
            slave_health: self.slave.health(now_host_us),
        })
    }

    fn selected_tick_from_buffered_times(
        &self,
        latest: &RawClockLatestTiming,
        selected_master_time_us: u64,
        selected_slave_time_us: u64,
        now_host_us: u64,
    ) -> Result<RawClockTickTiming, RawClockRuntimeError> {
        let master_age = now_host_us.saturating_sub(selected_master_time_us);
        let slave_age = now_host_us.saturating_sub(selected_slave_time_us);
        Ok(RawClockTickTiming {
            master_feedback_time_us: selected_master_time_us,
            slave_feedback_time_us: selected_slave_time_us,
            inter_arm_skew_us: selected_master_time_us.abs_diff(selected_slave_time_us),
            master_selected_sample_age_us: master_age,
            slave_selected_sample_age_us: slave_age,
            master_health: latest.master_health.clone(),
            slave_health: latest.slave_health.clone(),
        })
    }

    fn warmup_tick_from_snapshots(
        &mut self,
        master: &ExperimentalRawClockSnapshot,
        slave: &ExperimentalRawClockSnapshot,
        now_host_us: u64,
        thresholds: RawClockRuntimeThresholds,
    ) -> Result<(), RawClockRuntimeError> {
        match self.tick_from_snapshots(master, slave, now_host_us) {
            Ok(tick) => match RawClockRuntimeGate::new(thresholds).check_tick(tick) {
                Ok(()) | Err(RawClockRuntimeError::InterArmSkew { .. }) => Ok(()),
                Err(RawClockRuntimeError::ClockUnhealthy { .. }) => Ok(()),
                Err(err) => Err(err),
            },
            Err(RawClockRuntimeError::EstimatorNotReady { .. }) => Ok(()),
            Err(err) => Err(err),
        }
    }

    #[cfg(test)]
    fn seed_ready_for_tests(
        &mut self,
        master: &[ExperimentalRawClockSnapshot],
        slave: &[ExperimentalRawClockSnapshot],
    ) {
        for (master, slave) in master.iter().zip(slave) {
            self.ingest_snapshots(master, slave).expect("test raw-clock seed should ingest");
        }
    }

    #[cfg(test)]
    fn sample_counts_for_tests(&self) -> (usize, usize) {
        (
            self.master.health(0).sample_count,
            self.slave.health(0).sample_count,
        )
    }

    fn ingest_one(
        &mut self,
        side: RawClockSide,
        snapshot: &ExperimentalRawClockSnapshot,
    ) -> Result<(), RawClockRuntimeError> {
        let raw_us = raw_hw_us(side, snapshot)?;
        let previous = match side {
            RawClockSide::Master => &mut self.last_master_raw_us,
            RawClockSide::Slave => &mut self.last_slave_raw_us,
        };

        if let Some(previous_raw_us) = *previous {
            if raw_us < previous_raw_us {
                self.record_raw_timestamp_regression(side);
                return Err(RawClockRuntimeError::RawTimestampRegression {
                    side: side.as_str(),
                    previous_raw_us,
                    raw_us,
                });
            }
            if raw_us == previous_raw_us {
                return Ok(());
            }
        }

        let anchor_us = raw_clock_anchor_us(side, snapshot)?;
        let sample = RawClockSample {
            raw_us,
            host_rx_mono_us: anchor_us,
        };
        let push_result = match side {
            RawClockSide::Master => self.master.push_with_receive_mono_us(
                sample,
                snapshot.newest_raw_feedback_timing.host_rx_mono_us,
            ),
            RawClockSide::Slave => self.slave.push_with_receive_mono_us(
                sample,
                snapshot.newest_raw_feedback_timing.host_rx_mono_us,
            ),
        };
        map_raw_clock_push_error(side, push_result)?;
        *previous = Some(raw_us);
        Ok(())
    }

    fn record_raw_timestamp_regression(&mut self, side: RawClockSide) {
        let counter = match side {
            RawClockSide::Master => &mut self.master_raw_timestamp_regressions,
            RawClockSide::Slave => &mut self.slave_raw_timestamp_regressions,
        };
        *counter = counter.saturating_add(1);
    }

    fn record_clock_health_failure(&mut self) {
        self.clock_health_failures = self.clock_health_failures.saturating_add(1);
    }

    fn check_tick_with_debounce(
        &mut self,
        tick: RawClockTickTiming,
        thresholds: RawClockRuntimeThresholds,
    ) -> Result<(), RawClockRuntimeError> {
        if tick.master_selected_sample_age_us > thresholds.last_sample_age_us {
            self.reset_residual_max_consecutive_failures(RawClockSide::Master);
            let mut health = tick.master_health.clone();
            health.last_sample_age_us = tick.master_selected_sample_age_us;
            return Err(RawClockRuntimeError::ClockUnhealthy {
                side: RawClockSide::Master.as_str(),
                health: Box::new(health),
            });
        }
        if tick.slave_selected_sample_age_us > thresholds.last_sample_age_us {
            self.reset_residual_max_consecutive_failures(RawClockSide::Slave);
            let mut health = tick.slave_health.clone();
            health.last_sample_age_us = tick.slave_selected_sample_age_us;
            return Err(RawClockRuntimeError::ClockUnhealthy {
                side: RawClockSide::Slave.as_str(),
                health: Box::new(health),
            });
        }

        if tick.inter_arm_skew_us > thresholds.inter_arm_skew_max_us {
            return Err(RawClockRuntimeError::InterArmSkew {
                inter_arm_skew_us: tick.inter_arm_skew_us,
                max_us: thresholds.inter_arm_skew_max_us,
            });
        }

        let master_error =
            self.apply_health_with_debounce(RawClockSide::Master, &tick.master_health, thresholds);
        let slave_error =
            self.apply_health_with_debounce(RawClockSide::Slave, &tick.slave_health, thresholds);

        match (master_error, slave_error) {
            (Some(master_error), Some(slave_error))
                if master_error.priority
                    == RawClockDebouncedHealthErrorPriority::ResidualMaxThreshold
                    && slave_error.priority == RawClockDebouncedHealthErrorPriority::FailFast =>
            {
                Err(slave_error.error)
            },
            (Some(master_error), _) => Err(master_error.error),
            (None, Some(slave_error)) => Err(slave_error.error),
            (None, None) => Ok(()),
        }
    }

    fn apply_health_with_debounce(
        &mut self,
        side: RawClockSide,
        health: &RawClockHealth,
        thresholds: RawClockRuntimeThresholds,
    ) -> Option<RawClockDebouncedHealthError> {
        if health.last_sample_age_us > thresholds.last_sample_age_us {
            self.reset_residual_max_consecutive_failures(side);
            return Some(RawClockDebouncedHealthError {
                error: RawClockRuntimeError::ClockUnhealthy {
                    side: side.as_str(),
                    health: Box::new(health.clone()),
                },
                priority: RawClockDebouncedHealthErrorPriority::FailFast,
            });
        }

        if health.healthy {
            self.reset_residual_max_consecutive_failures(side);
            return None;
        }

        if health.failure_kind == Some(RawClockUnhealthyKind::ResidualMax) {
            let consecutive_failures = self.record_residual_max_failure(side);
            if consecutive_failures >= thresholds.residual_max_consecutive_failures {
                return Some(RawClockDebouncedHealthError {
                    error: RawClockRuntimeError::ClockUnhealthy {
                        side: side.as_str(),
                        health: Box::new(health.clone()),
                    },
                    priority: RawClockDebouncedHealthErrorPriority::ResidualMaxThreshold,
                });
            }
            return None;
        }

        self.reset_residual_max_consecutive_failures(side);
        Some(RawClockDebouncedHealthError {
            error: RawClockRuntimeError::ClockUnhealthy {
                side: side.as_str(),
                health: Box::new(health.clone()),
            },
            priority: RawClockDebouncedHealthErrorPriority::FailFast,
        })
    }

    fn record_residual_max_failure(&mut self, side: RawClockSide) -> u32 {
        let (spikes, consecutive_failures) = match side {
            RawClockSide::Master => (
                &mut self.master_residual_max_spikes,
                &mut self.master_residual_max_consecutive_failures,
            ),
            RawClockSide::Slave => (
                &mut self.slave_residual_max_spikes,
                &mut self.slave_residual_max_consecutive_failures,
            ),
        };
        *spikes = spikes.saturating_add(1);
        *consecutive_failures = consecutive_failures.saturating_add(1);
        *consecutive_failures
    }

    fn reset_residual_max_consecutive_failures(&mut self, side: RawClockSide) {
        match side {
            RawClockSide::Master => self.master_residual_max_consecutive_failures = 0,
            RawClockSide::Slave => self.slave_residual_max_consecutive_failures = 0,
        }
    }

    fn record_skew_sample(&mut self, inter_arm_skew_us: u64) {
        self.max_inter_arm_skew_us = self.max_inter_arm_skew_us.max(inter_arm_skew_us);
        if self.skew_samples_us.len() == RAW_CLOCK_SKEW_RETAINED_SAMPLE_CAPACITY {
            self.skew_samples_us.pop_front();
        }
        self.skew_samples_us.push_back(inter_arm_skew_us);
    }

    fn record_latest_skew_sample(&mut self, latest_inter_arm_skew_us: u64) {
        self.record_skew_sample(latest_inter_arm_skew_us);
    }

    fn report(
        &self,
        now_host_us: u64,
        iterations: usize,
        exit_reason: Option<RawClockRuntimeExitReason>,
    ) -> RawClockRuntimeReport {
        RawClockRuntimeReport {
            master: self.report_health(RawClockSide::Master, now_host_us),
            slave: self.report_health(RawClockSide::Slave, now_host_us),
            joint_motion: None,
            max_inter_arm_skew_us: self.max_inter_arm_skew_us,
            inter_arm_skew_p95_us: percentile(self.skew_samples_us.iter().copied(), 95),
            clock_health_failures: self.clock_health_failures,
            master_residual_max_spikes: self.master_residual_max_spikes,
            slave_residual_max_spikes: self.slave_residual_max_spikes,
            master_residual_max_consecutive_failures: self.master_residual_max_consecutive_failures,
            slave_residual_max_consecutive_failures: self.slave_residual_max_consecutive_failures,
            read_faults: 0,
            submission_faults: 0,
            last_submission_failed_side: None,
            peer_command_may_have_applied: false,
            runtime_faults: 0,
            master_tx_realtime_overwrites_total: 0,
            slave_tx_realtime_overwrites_total: 0,
            master_tx_frames_sent_total: 0,
            slave_tx_frames_sent_total: 0,
            master_tx_fault_aborts_total: 0,
            slave_tx_fault_aborts_total: 0,
            last_runtime_fault_master: None,
            last_runtime_fault_slave: None,
            iterations,
            exit_reason,
            master_stop_attempt: StopAttemptResult::NotAttempted,
            slave_stop_attempt: StopAttemptResult::NotAttempted,
            last_error: None,
        }
    }

    fn report_health(&self, side: RawClockSide, now_host_us: u64) -> RawClockHealth {
        let (estimator, external_regressions) = match side {
            RawClockSide::Master => (&self.master, self.master_raw_timestamp_regressions),
            RawClockSide::Slave => (&self.slave, self.slave_raw_timestamp_regressions),
        };
        let mut health = estimator.health(now_host_us);
        if external_regressions > 0 {
            health.raw_timestamp_regressions =
                health.raw_timestamp_regressions.saturating_add(external_regressions);
            health.healthy = false;
            health.failure_kind = Some(RawClockUnhealthyKind::RawTimestampRegression);
            health.reason = Some(format!(
                "raw timestamp regressions observed: {}",
                health.raw_timestamp_regressions
            ));
        }
        health
    }
}

pub struct ExperimentalRawClockDualArmStandby {
    master: Piper<Standby, SoftRealtime>,
    slave: Piper<Standby, SoftRealtime>,
    timing: RawClockRuntimeTiming,
    config: ExperimentalRawClockConfig,
}

impl ExperimentalRawClockDualArmStandby {
    pub fn new(
        master: Piper<Standby, SoftRealtime>,
        slave: Piper<Standby, SoftRealtime>,
        config: ExperimentalRawClockConfig,
    ) -> Result<Self, RawClockRuntimeError> {
        config.validate()?;
        Ok(Self {
            master,
            slave,
            timing: RawClockRuntimeTiming::new(config.estimator_thresholds),
            config,
        })
    }

    pub fn warmup(
        mut self,
        policy: ControlReadPolicy,
        warmup: Duration,
        cancel_signal: &AtomicBool,
    ) -> Result<Self, RawClockRuntimeError> {
        self.timing.reset_for_warmup();
        let deadline = Instant::now() + warmup;
        while Instant::now() < deadline {
            if cancel_signal.load(Ordering::Acquire) {
                return Err(RawClockRuntimeError::Cancelled);
            }

            let master = self.read_master_experimental_snapshot(policy)?;
            let slave = self.read_slave_experimental_snapshot(policy)?;
            self.timing.warmup_tick_from_snapshots(
                &master,
                &slave,
                piper_can::monotonic_micros(),
                self.config.thresholds,
            )?;
            std::thread::sleep(Duration::from_millis(1));
        }

        let final_grace_deadline =
            Instant::now() + raw_clock_warmup_final_grace(self.config.estimator_thresholds);
        loop {
            if cancel_signal.load(Ordering::Acquire) {
                return Err(RawClockRuntimeError::Cancelled);
            }

            let master = self.read_master_experimental_snapshot(policy)?;
            let slave = self.read_slave_experimental_snapshot(policy)?;
            let final_result = self
                .timing
                .tick_from_snapshots(&master, &slave, piper_can::monotonic_micros())
                .and_then(|final_tick| {
                    RawClockRuntimeGate::new(self.config.thresholds).check_tick(final_tick)
                });

            match final_result {
                Ok(()) => break,
                Err(err)
                    if warmup_final_error_is_retryable(&err)
                        && Instant::now() < final_grace_deadline =>
                {
                    std::thread::sleep(Duration::from_millis(1));
                },
                Err(err) => return Err(err),
            }
        }
        self.timing.reset_runtime_residual_max_counters();
        Ok(self)
    }

    pub fn enable_mit_passthrough(
        mut self,
        master_cfg: MitModeConfig,
        slave_cfg: MitModeConfig,
    ) -> Result<ExperimentalRawClockDualArmActive, RawClockRuntimeError> {
        self.timing.mark_continuity_boundary();
        let master = match self.master.enable_mit_passthrough(master_cfg) {
            Ok(master) => master,
            Err(source) => {
                return Err(RawClockRuntimeError::EnableFailed {
                    failed_side: "master",
                    source,
                    enabled_side_stop_attempt: None,
                });
            },
        };

        let slave = match self.slave.enable_mit_passthrough(slave_cfg) {
            Ok(slave) => slave,
            Err(source) => {
                let stop_attempt = fault_shutdown_single(&master, Duration::from_millis(20));
                return Err(RawClockRuntimeError::EnableFailed {
                    failed_side: "slave",
                    source,
                    enabled_side_stop_attempt: Some(stop_attempt),
                });
            },
        };

        Ok(ExperimentalRawClockDualArmActive {
            master,
            slave,
            timing: self.timing,
            config: self.config,
        })
    }

    fn read_master_experimental_snapshot(
        &self,
        policy: ControlReadPolicy,
    ) -> Result<ExperimentalRawClockSnapshot, RawClockRuntimeError> {
        read_experimental_snapshot_from_driver(&self.master.driver, RawClockSide::Master, policy)
    }

    fn read_slave_experimental_snapshot(
        &self,
        policy: ControlReadPolicy,
    ) -> Result<ExperimentalRawClockSnapshot, RawClockRuntimeError> {
        read_experimental_snapshot_from_driver(&self.slave.driver, RawClockSide::Slave, policy)
    }
}

struct RealRawClockRuntimeIo {
    master: Piper<Active<MitPassthroughMode>, SoftRealtime>,
    slave: Piper<Active<MitPassthroughMode>, SoftRealtime>,
}

struct RealRawClockStandbyArms {
    master: Piper<Standby, SoftRealtime>,
    slave: Piper<Standby, SoftRealtime>,
}

struct RawClockRuntimeCore<I> {
    io: I,
    timing: RawClockRuntimeTiming,
    config: ExperimentalRawClockConfig,
}

struct RawClockStandbyExit<Arms> {
    arms: Arms,
    telemetry: RawClockRuntimeTelemetry,
}

struct RawClockFaultExit<Arms> {
    arms: Arms,
    shutdown: ExperimentalFaultShutdown,
    telemetry: RawClockRuntimeTelemetry,
}

enum RawClockCoreExit<StandbyArms, ErrorArms> {
    Standby {
        arms: StandbyArms,
        timing: Box<RawClockRuntimeTiming>,
        config: Box<ExperimentalRawClockConfig>,
        report: RawClockRuntimeReport,
    },
    Faulted {
        arms: ErrorArms,
        report: RawClockRuntimeReport,
    },
}

trait RawClockRuntimeIo: Sized {
    type StandbyArms;
    type ErrorArms;

    fn read_master_experimental_snapshot(
        &mut self,
        policy: ControlReadPolicy,
    ) -> Result<ExperimentalRawClockSnapshot, RawClockRuntimeError>;

    fn read_slave_experimental_snapshot(
        &mut self,
        policy: ControlReadPolicy,
    ) -> Result<ExperimentalRawClockSnapshot, RawClockRuntimeError>;

    fn runtime_health(&mut self) -> ExperimentalRawClockRuntimeHealth;

    fn submit_command(
        &mut self,
        command: &BilateralCommand,
        timeout: Duration,
    ) -> Result<(), RawClockRuntimeError>;

    fn disable_both(
        self,
        cfg: DisableConfig,
    ) -> Result<RawClockStandbyExit<Self::StandbyArms>, RawClockRuntimeError>;

    fn fault_shutdown(self, timeout: Duration) -> RawClockFaultExit<Self::ErrorArms>;
}

impl RawClockRuntimeIo for RealRawClockRuntimeIo {
    type StandbyArms = RealRawClockStandbyArms;
    type ErrorArms = ExperimentalRawClockErrorState;

    fn read_master_experimental_snapshot(
        &mut self,
        policy: ControlReadPolicy,
    ) -> Result<ExperimentalRawClockSnapshot, RawClockRuntimeError> {
        read_experimental_snapshot_from_driver(&self.master.driver, RawClockSide::Master, policy)
    }

    fn read_slave_experimental_snapshot(
        &mut self,
        policy: ControlReadPolicy,
    ) -> Result<ExperimentalRawClockSnapshot, RawClockRuntimeError> {
        read_experimental_snapshot_from_driver(&self.slave.driver, RawClockSide::Slave, policy)
    }

    fn runtime_health(&mut self) -> ExperimentalRawClockRuntimeHealth {
        ExperimentalRawClockRuntimeHealth {
            master: self.master.observer().runtime_health(),
            slave: self.slave.observer().runtime_health(),
        }
    }

    fn submit_command(
        &mut self,
        command: &BilateralCommand,
        timeout: Duration,
    ) -> Result<(), RawClockRuntimeError> {
        submit_soft_realtime_command_detailed(&self.slave, &self.master, command, timeout)
    }

    fn disable_both(
        self,
        cfg: DisableConfig,
    ) -> Result<RawClockStandbyExit<Self::StandbyArms>, RawClockRuntimeError> {
        disable_soft_realtime_both(self.master, self.slave, cfg)
    }

    fn fault_shutdown(self, timeout: Duration) -> RawClockFaultExit<Self::ErrorArms> {
        fault_shutdown_soft_realtime(self.master, self.slave, timeout)
    }
}

pub struct ExperimentalRawClockDualArmActive {
    master: Piper<Active<MitPassthroughMode>, SoftRealtime>,
    slave: Piper<Active<MitPassthroughMode>, SoftRealtime>,
    timing: RawClockRuntimeTiming,
    config: ExperimentalRawClockConfig,
}

impl ExperimentalRawClockDualArmActive {
    pub fn submit_command(&self, command: &BilateralCommand, timeout: Duration) -> RobotResult<()> {
        submit_soft_realtime_command(&self.slave, &self.master, command, timeout)
    }

    pub fn run_master_follower(
        self,
        calibration: DualArmCalibration,
        cfg: ExperimentalRawClockRunConfig,
    ) -> Result<ExperimentalRawClockRunExit, RawClockRuntimeError> {
        self.run_master_follower_with_gains(
            calibration,
            ExperimentalRawClockMasterFollowerGains::default(),
            cfg,
        )
    }

    pub fn run_master_follower_with_gains(
        self,
        calibration: DualArmCalibration,
        gains: ExperimentalRawClockMasterFollowerGains,
        cfg: ExperimentalRawClockRunConfig,
    ) -> Result<ExperimentalRawClockRunExit, RawClockRuntimeError> {
        self.run_with_controller(
            master_follower_controller_with_gains(calibration, gains),
            cfg,
        )
    }

    fn run_with_controller<C>(
        self,
        controller: C,
        cfg: ExperimentalRawClockRunConfig,
    ) -> Result<ExperimentalRawClockRunExit, RawClockRuntimeError>
    where
        C: BilateralController,
    {
        let ExperimentalRawClockDualArmActive {
            master,
            slave,
            timing,
            config,
        } = self;
        let core = RawClockRuntimeCore {
            io: RealRawClockRuntimeIo { master, slave },
            timing,
            config,
        };

        match run_raw_clock_runtime_core(core, controller, cfg)? {
            RawClockCoreExit::Standby {
                arms,
                timing,
                config,
                report,
            } => Ok(ExperimentalRawClockRunExit::Standby {
                arms: Box::new(ExperimentalRawClockDualArmStandby {
                    master: arms.master,
                    slave: arms.slave,
                    timing: *timing,
                    config: *config,
                }),
                report,
            }),
            RawClockCoreExit::Faulted { arms, report } => {
                Ok(ExperimentalRawClockRunExit::Faulted {
                    arms: Box::new(arms),
                    report,
                })
            },
        }
    }

    pub fn disable_both(
        self,
        cfg: DisableConfig,
    ) -> Result<ExperimentalRawClockDualArmStandby, RawClockRuntimeError> {
        let exit = disable_soft_realtime_both(self.master, self.slave, cfg)?;
        let arms = exit.arms;
        Ok(ExperimentalRawClockDualArmStandby {
            master: arms.master,
            slave: arms.slave,
            timing: self.timing,
            config: self.config,
        })
    }

    pub fn fault_shutdown(
        self,
        timeout: Duration,
    ) -> (ExperimentalRawClockErrorState, ExperimentalFaultShutdown) {
        let exit = fault_shutdown_soft_realtime(self.master, self.slave, timeout);
        (exit.arms, exit.shutdown)
    }
}

fn master_follower_controller_with_gains(
    calibration: DualArmCalibration,
    gains: ExperimentalRawClockMasterFollowerGains,
) -> MasterFollowerController {
    MasterFollowerController::new(calibration)
        .with_track_gains(gains.track_kp, gains.track_kd)
        .with_master_damping(gains.master_damping)
}

#[derive(Debug, Clone)]
pub struct ExperimentalRawClockRunConfig {
    pub read_policy: ControlReadPolicy,
    pub command_timeout: Duration,
    pub disable_config: DisableConfig,
    pub cancel_signal: Option<Arc<AtomicBool>>,
}

impl Default for ExperimentalRawClockRunConfig {
    fn default() -> Self {
        Self {
            read_policy: ControlReadPolicy {
                max_state_skew_us: 2_000,
                max_feedback_age: DEFAULT_CONTROL_MAX_FEEDBACK_AGE,
            },
            command_timeout: Duration::from_millis(20),
            disable_config: DisableConfig::default(),
            cancel_signal: None,
        }
    }
}

pub enum ExperimentalRawClockRunExit {
    Standby {
        arms: Box<ExperimentalRawClockDualArmStandby>,
        report: RawClockRuntimeReport,
    },
    Faulted {
        arms: Box<ExperimentalRawClockErrorState>,
        report: RawClockRuntimeReport,
    },
}

pub struct ExperimentalRawClockErrorState {
    master: Piper<ErrorState, SoftRealtime>,
    slave: Piper<ErrorState, SoftRealtime>,
}

impl ExperimentalRawClockErrorState {
    pub fn runtime_health(&self) -> ExperimentalRawClockRuntimeHealth {
        ExperimentalRawClockRuntimeHealth {
            master: self.master.observer().runtime_health(),
            slave: self.slave.observer().runtime_health(),
        }
    }
}

trait ReportShutdownExt {
    fn with_shutdown(self, shutdown: ExperimentalFaultShutdown) -> Self;
}

impl ReportShutdownExt for RawClockRuntimeReport {
    fn with_shutdown(mut self, shutdown: ExperimentalFaultShutdown) -> Self {
        self.master_stop_attempt = shutdown.master_stop_attempt;
        self.slave_stop_attempt = shutdown.slave_stop_attempt;
        self
    }
}

fn run_raw_clock_runtime_core<I, C>(
    core: RawClockRuntimeCore<I>,
    mut controller: C,
    cfg: ExperimentalRawClockRunConfig,
) -> Result<RawClockCoreExit<I::StandbyArms, I::ErrorArms>, RawClockRuntimeError>
where
    I: RawClockRuntimeIo,
    C: BilateralController,
{
    let RawClockRuntimeCore {
        mut io,
        mut timing,
        config,
    } = core;
    config.validate()?;
    let nominal_period = Duration::from_secs_f64(1.0 / config.frequency_hz);
    let mut iterations = 0usize;
    let mut next_tick = Instant::now();
    let mut joint_motion = RawClockJointMotionAccumulator::default();

    loop {
        if let Some(max_iterations) = config.max_iterations
            && iterations >= max_iterations
        {
            let mut report = timing.report(
                piper_can::monotonic_micros(),
                iterations,
                Some(RawClockRuntimeExitReason::MaxIterations),
            );
            report.iterations = iterations;
            attach_joint_motion(&mut report, &joint_motion);
            let standby = io.disable_both(cfg.disable_config.clone())?;
            report.apply_telemetry(standby.telemetry);
            return Ok(RawClockCoreExit::Standby {
                arms: standby.arms,
                timing: Box::new(timing),
                config: Box::new(config),
                report,
            });
        }

        if cfg.cancel_signal.as_ref().is_some_and(|signal| signal.load(Ordering::Acquire)) {
            let mut report = timing.report(
                piper_can::monotonic_micros(),
                iterations,
                Some(RawClockRuntimeExitReason::Cancelled),
            );
            report.last_error = Some("raw-clock runtime cancelled".to_string());
            attach_joint_motion(&mut report, &joint_motion);
            let standby = io.disable_both(cfg.disable_config.clone())?;
            report.apply_telemetry(standby.telemetry);
            return Ok(RawClockCoreExit::Standby {
                arms: standby.arms,
                timing: Box::new(timing),
                config: Box::new(config),
                report,
            });
        }

        let now = Instant::now();
        if now < next_tick {
            std::thread::sleep(next_tick - now);
        }
        next_tick += nominal_period;

        let health = io.runtime_health();
        if let Some(exit_reason) = classify_runtime_fault_exit_reason(health) {
            let err = RawClockRuntimeError::RuntimeTransportFault {
                details: format_runtime_health_error(health),
            };
            let report = fault_report_from_timing(
                &timing,
                iterations,
                &joint_motion,
                exit_reason,
                err.to_string(),
                |report| {
                    report.runtime_faults = report.runtime_faults.saturating_add(1);
                    report.last_runtime_fault_master = health.master.fault;
                    report.last_runtime_fault_slave = health.slave.fault;
                },
            );
            let fault = io.fault_shutdown(FAULT_SHUTDOWN_TIMEOUT);
            return Ok(RawClockCoreExit::Faulted {
                arms: fault.arms,
                report: report.with_shutdown(fault.shutdown).with_telemetry(fault.telemetry),
            });
        }

        let master = match io.read_master_experimental_snapshot(cfg.read_policy) {
            Ok(snapshot) => snapshot,
            Err(err) => {
                let report = fault_report_from_timing(
                    &timing,
                    iterations,
                    &joint_motion,
                    RawClockRuntimeExitReason::ReadFault,
                    err.to_string(),
                    |report| report.read_faults = report.read_faults.saturating_add(1),
                );
                let fault = io.fault_shutdown(FAULT_SHUTDOWN_TIMEOUT);
                return Ok(RawClockCoreExit::Faulted {
                    arms: fault.arms,
                    report: report.with_shutdown(fault.shutdown).with_telemetry(fault.telemetry),
                });
            },
        };
        let slave = match io.read_slave_experimental_snapshot(cfg.read_policy) {
            Ok(snapshot) => snapshot,
            Err(err) => {
                let report = fault_report_from_timing(
                    &timing,
                    iterations,
                    &joint_motion,
                    RawClockRuntimeExitReason::ReadFault,
                    err.to_string(),
                    |report| report.read_faults = report.read_faults.saturating_add(1),
                );
                let fault = io.fault_shutdown(FAULT_SHUTDOWN_TIMEOUT);
                return Ok(RawClockCoreExit::Faulted {
                    arms: fault.arms,
                    report: report.with_shutdown(fault.shutdown).with_telemetry(fault.telemetry),
                });
            },
        };

        let tick = match timing.tick_from_snapshots(&master, &slave, piper_can::monotonic_micros())
        {
            Ok(tick) => tick,
            Err(err) => {
                let report = fault_report_from_timing(
                    &timing,
                    iterations,
                    &joint_motion,
                    RawClockRuntimeExitReason::RawClockFault,
                    err.to_string(),
                    |_| {},
                );
                let fault = io.fault_shutdown(FAULT_SHUTDOWN_TIMEOUT);
                return Ok(RawClockCoreExit::Faulted {
                    arms: fault.arms,
                    report: report.with_shutdown(fault.shutdown).with_telemetry(fault.telemetry),
                });
            },
        };

        let inter_arm_skew_us = tick.inter_arm_skew_us;
        if let Err(err) = timing.check_tick_with_debounce(tick, config.thresholds) {
            if matches!(err, RawClockRuntimeError::ClockUnhealthy { .. }) {
                timing.record_clock_health_failure();
            }
            let exit_reason = if matches!(err, RawClockRuntimeError::ClockUnhealthy { .. }) {
                RawClockRuntimeExitReason::ClockHealthFault
            } else {
                RawClockRuntimeExitReason::RawClockFault
            };
            let report = fault_report_from_timing(
                &timing,
                iterations,
                &joint_motion,
                exit_reason,
                err.to_string(),
                |_| {},
            );
            let fault = io.fault_shutdown(FAULT_SHUTDOWN_TIMEOUT);
            return Ok(RawClockCoreExit::Faulted {
                arms: fault.arms,
                report: report.with_shutdown(fault.shutdown).with_telemetry(fault.telemetry),
            });
        }

        let snapshot = raw_dual_arm_snapshot(&master, &slave, inter_arm_skew_us);
        let frame = BilateralControlFrame {
            snapshot,
            compensation: None,
        };
        let command = match controller.tick_with_compensation(&frame, nominal_period) {
            Ok(command) => command,
            Err(err) => {
                let report = fault_report_from_timing(
                    &timing,
                    iterations,
                    &joint_motion,
                    RawClockRuntimeExitReason::ControllerFault,
                    err.to_string(),
                    |_| {},
                );
                let fault = io.fault_shutdown(FAULT_SHUTDOWN_TIMEOUT);
                return Ok(RawClockCoreExit::Faulted {
                    arms: fault.arms,
                    report: report.with_shutdown(fault.shutdown).with_telemetry(fault.telemetry),
                });
            },
        };
        joint_motion.record(&snapshot, &command);

        if let Err(err) = io.submit_command(&command, cfg.command_timeout) {
            let report = fault_report_from_timing(
                &timing,
                iterations,
                &joint_motion,
                RawClockRuntimeExitReason::SubmissionFault,
                err.to_string(),
                |report| {
                    report.submission_faults = report.submission_faults.saturating_add(1);
                    if let RawClockRuntimeError::SubmissionFault {
                        side,
                        peer_command_may_have_applied,
                        ..
                    } = &err
                    {
                        report.last_submission_failed_side = RawClockSide::from_str(side);
                        report.peer_command_may_have_applied = *peer_command_may_have_applied;
                    }
                },
            );
            let fault = io.fault_shutdown(FAULT_SHUTDOWN_TIMEOUT);
            return Ok(RawClockCoreExit::Faulted {
                arms: fault.arms,
                report: report.with_shutdown(fault.shutdown).with_telemetry(fault.telemetry),
            });
        }

        iterations = iterations.saturating_add(1);
    }
}

fn fault_report_from_timing(
    timing: &RawClockRuntimeTiming,
    iterations: usize,
    joint_motion: &RawClockJointMotionAccumulator,
    exit_reason: RawClockRuntimeExitReason,
    error: String,
    update: impl FnOnce(&mut RawClockRuntimeReport),
) -> RawClockRuntimeReport {
    let mut report = timing.report(piper_can::monotonic_micros(), iterations, Some(exit_reason));
    report.last_error = Some(error);
    attach_joint_motion(&mut report, joint_motion);
    update(&mut report);
    report
}

fn attach_joint_motion(
    report: &mut RawClockRuntimeReport,
    joint_motion: &RawClockJointMotionAccumulator,
) {
    report.joint_motion = joint_motion.snapshot();
}

fn submit_soft_realtime_command(
    slave: &Piper<Active<MitPassthroughMode>, SoftRealtime>,
    master: &Piper<Active<MitPassthroughMode>, SoftRealtime>,
    command: &BilateralCommand,
    timeout: Duration,
) -> RobotResult<()> {
    submit_soft_realtime_command_detailed(slave, master, command, timeout).map_err(
        |err| match err {
            RawClockRuntimeError::SubmissionFault { source, .. } => source,
            err => RobotError::ConfigError(err.to_string()),
        },
    )
}

fn submit_soft_realtime_command_detailed(
    slave: &Piper<Active<MitPassthroughMode>, SoftRealtime>,
    master: &Piper<Active<MitPassthroughMode>, SoftRealtime>,
    command: &BilateralCommand,
    timeout: Duration,
) -> Result<(), RawClockRuntimeError> {
    if let Err(source) = slave.command_torques_confirmed(
        &command.slave_position,
        &command.slave_velocity,
        &command.slave_kp,
        &command.slave_kd,
        &command.slave_feedforward_torque,
        timeout,
    ) {
        return Err(RawClockRuntimeError::SubmissionFault {
            side: RawClockSide::Slave.as_str(),
            peer_command_may_have_applied: false,
            source,
        });
    }
    if let Err(source) = master.command_torques_confirmed(
        &command.master_position,
        &command.master_velocity,
        &command.master_kp,
        &command.master_kd,
        &JointArray::splat(NewtonMeter::ZERO),
        timeout,
    ) {
        return Err(RawClockRuntimeError::SubmissionFault {
            side: RawClockSide::Master.as_str(),
            peer_command_may_have_applied: true,
            source,
        });
    }
    Ok(())
}

fn disable_soft_realtime_both(
    master: Piper<Active<MitPassthroughMode>, SoftRealtime>,
    slave: Piper<Active<MitPassthroughMode>, SoftRealtime>,
    cfg: DisableConfig,
) -> Result<RawClockStandbyExit<RealRawClockStandbyArms>, RawClockRuntimeError> {
    let master_result = master.disable(cfg.clone());
    let slave_result = slave.disable(cfg);
    match (master_result, slave_result) {
        (Ok(master), Ok(slave)) => {
            let telemetry = telemetry_from_drivers(&master.driver, &slave.driver);
            Ok(RawClockStandbyExit {
                arms: RealRawClockStandbyArms { master, slave },
                telemetry,
            })
        },
        (master, slave) => Err(RawClockRuntimeError::DisableBothFailed {
            master: master.err().map(|err| err.to_string()),
            slave: slave.err().map(|err| err.to_string()),
        }),
    }
}

fn fault_shutdown_soft_realtime(
    master: Piper<Active<MitPassthroughMode>, SoftRealtime>,
    slave: Piper<Active<MitPassthroughMode>, SoftRealtime>,
    timeout: Duration,
) -> RawClockFaultExit<ExperimentalRawClockErrorState> {
    let deadline = Instant::now() + timeout;
    let pre_shutdown_master_fault = RuntimeHealthSnapshot::from(master.driver.health()).fault;
    let pre_shutdown_slave_fault = RuntimeHealthSnapshot::from(slave.driver.health()).fault;
    master.driver.latch_fault();
    slave.driver.latch_fault();
    let master_pending = enqueue_experimental_stop_attempt(&master, deadline);
    let slave_pending = enqueue_experimental_stop_attempt(&slave, deadline);
    let master_stop_attempt = resolve_experimental_stop_attempt(master_pending);
    let slave_stop_attempt = resolve_experimental_stop_attempt(slave_pending);
    master.driver.request_stop();
    slave.driver.request_stop();
    let mut telemetry = telemetry_from_drivers(&master.driver, &slave.driver);
    telemetry.last_runtime_fault_master = pre_shutdown_master_fault;
    telemetry.last_runtime_fault_slave = pre_shutdown_slave_fault;
    RawClockFaultExit {
        arms: ExperimentalRawClockErrorState {
            master: force_soft_error_state(master),
            slave: force_soft_error_state(slave),
        },
        shutdown: ExperimentalFaultShutdown {
            master_stop_attempt,
            slave_stop_attempt,
        },
        telemetry,
    }
}

fn telemetry_from_drivers(
    master: &piper_driver::Piper,
    slave: &piper_driver::Piper,
) -> RawClockRuntimeTelemetry {
    let master_metrics = master.get_metrics();
    let slave_metrics = slave.get_metrics();
    let master_health = RuntimeHealthSnapshot::from(master.health());
    let slave_health = RuntimeHealthSnapshot::from(slave.health());
    RawClockRuntimeTelemetry {
        master_tx_realtime_overwrites_total: master_metrics.tx_realtime_overwrites_total,
        slave_tx_realtime_overwrites_total: slave_metrics.tx_realtime_overwrites_total,
        master_tx_frames_sent_total: master_metrics.tx_frames_sent_total,
        slave_tx_frames_sent_total: slave_metrics.tx_frames_sent_total,
        master_tx_fault_aborts_total: master_metrics.tx_fault_aborts_total,
        slave_tx_fault_aborts_total: slave_metrics.tx_fault_aborts_total,
        last_runtime_fault_master: master_health.fault,
        last_runtime_fault_slave: slave_health.fault,
    }
}

fn read_experimental_snapshot_from_driver(
    driver: &piper_driver::Piper,
    side: RawClockSide,
    policy: ControlReadPolicy,
) -> Result<ExperimentalRawClockSnapshot, RawClockRuntimeError> {
    match driver.get_aligned_motion(policy.max_state_skew_us, policy.max_feedback_age) {
        AlignmentResult::Ok(state) => experimental_snapshot_from_aligned(side, state),
        AlignmentResult::Incomplete {
            position_candidate_mask,
            dynamic_candidate_mask,
        } => Err(RawClockRuntimeError::ReadFault {
            side: side.as_str(),
            source: RobotError::control_state_incomplete(
                position_candidate_mask,
                dynamic_candidate_mask,
            ),
        }),
        AlignmentResult::Stale { age, .. } => Err(RawClockRuntimeError::ReadFault {
            side: side.as_str(),
            source: RobotError::feedback_stale(age, policy.max_feedback_age),
        }),
        AlignmentResult::Misaligned { state, .. } => Err(RawClockRuntimeError::ReadFault {
            side: side.as_str(),
            source: RobotError::state_misaligned(state.skew_us, policy.max_state_skew_us),
        }),
    }
}

fn experimental_snapshot_from_aligned(
    side: RawClockSide,
    state: piper_driver::AlignedMotionState,
) -> Result<ExperimentalRawClockSnapshot, RawClockRuntimeError> {
    let newest_raw_feedback_timing = state.newest_raw_feedback_timing().ok_or(
        RawClockRuntimeError::MissingRawFeedbackTiming {
            side: side.as_str(),
        },
    )?;
    if newest_raw_feedback_timing.hw_raw_us.is_none() {
        return Err(RawClockRuntimeError::MissingRawFeedbackTiming {
            side: side.as_str(),
        });
    }
    if raw_feedback_anchor_us(newest_raw_feedback_timing).is_none() {
        return Err(RawClockRuntimeError::MissingRawFeedbackTiming {
            side: side.as_str(),
        });
    }

    Ok(ExperimentalRawClockSnapshot {
        state: ControlSnapshot {
            position: JointArray::new(state.joint_pos.map(Rad)),
            velocity: JointArray::new(state.joint_vel.map(RadPerSecond)),
            torque: JointArray::new(std::array::from_fn(|index| {
                NewtonMeter(piper_driver::JointDynamicState::calculate_torque(
                    index,
                    state.joint_current[index],
                ))
            })),
            position_timestamp_us: state.position_timestamp_us,
            dynamic_timestamp_us: state.dynamic_timestamp_us,
            skew_us: state.skew_us,
        },
        newest_raw_feedback_timing,
        feedback_age: state.feedback_age(),
    })
}

fn raw_hw_us(
    side: RawClockSide,
    snapshot: &ExperimentalRawClockSnapshot,
) -> Result<u64, RawClockRuntimeError> {
    snapshot.newest_raw_feedback_timing.hw_raw_us.ok_or(
        RawClockRuntimeError::MissingRawFeedbackTiming {
            side: side.as_str(),
        },
    )
}

fn raw_clock_anchor_us(
    side: RawClockSide,
    snapshot: &ExperimentalRawClockSnapshot,
) -> Result<u64, RawClockRuntimeError> {
    raw_feedback_anchor_us(snapshot.newest_raw_feedback_timing).ok_or(
        RawClockRuntimeError::MissingRawFeedbackTiming {
            side: side.as_str(),
        },
    )
}

fn raw_feedback_anchor_us(timing: piper_driver::RawFeedbackTiming) -> Option<u64> {
    timing.hw_trans_us.or(timing.system_ts_us)
}

fn map_raw_clock_push_error(
    side: RawClockSide,
    result: Result<(), RawClockError>,
) -> Result<(), RawClockRuntimeError> {
    result.map_err(|err| match err {
        RawClockError::RawTimestampRegression {
            previous_raw_us,
            raw_us,
        } => RawClockRuntimeError::RawTimestampRegression {
            side: side.as_str(),
            previous_raw_us,
            raw_us,
        },
    })
}

fn raw_dual_arm_snapshot(
    master: &ExperimentalRawClockSnapshot,
    slave: &ExperimentalRawClockSnapshot,
    inter_arm_skew_us: u64,
) -> DualArmSnapshot {
    DualArmSnapshot {
        left: ControlSnapshotFull {
            state: master.state,
            position_host_rx_mono_us: master.newest_raw_feedback_timing.host_rx_mono_us,
            dynamic_host_rx_mono_us: master.newest_raw_feedback_timing.host_rx_mono_us,
            feedback_age: master.feedback_age,
        },
        right: ControlSnapshotFull {
            state: slave.state,
            position_host_rx_mono_us: slave.newest_raw_feedback_timing.host_rx_mono_us,
            dynamic_host_rx_mono_us: slave.newest_raw_feedback_timing.host_rx_mono_us,
            feedback_age: slave.feedback_age,
        },
        inter_arm_skew: Duration::from_micros(inter_arm_skew_us),
        host_cycle_timestamp: Instant::now(),
    }
}

fn classify_runtime_fault_exit_reason(
    health: ExperimentalRawClockRuntimeHealth,
) -> Option<RawClockRuntimeExitReason> {
    if matches!(health.master.fault, Some(RuntimeFaultKind::ManualFault))
        || matches!(health.slave.fault, Some(RuntimeFaultKind::ManualFault))
    {
        Some(RawClockRuntimeExitReason::RuntimeManualFault)
    } else if !health.master.connected
        || !health.slave.connected
        || !health.master.rx_alive
        || !health.master.tx_alive
        || !health.slave.rx_alive
        || !health.slave.tx_alive
        || health.master.fault.is_some()
        || health.slave.fault.is_some()
    {
        Some(RawClockRuntimeExitReason::RuntimeTransportFault)
    } else {
        None
    }
}

fn format_runtime_health_error(health: ExperimentalRawClockRuntimeHealth) -> String {
    format!(
        "runtime health unhealthy: master(connected={}, rx_alive={}, tx_alive={}, fault={:?}), slave(connected={}, rx_alive={}, tx_alive={}, fault={:?})",
        health.master.connected,
        health.master.rx_alive,
        health.master.tx_alive,
        health.master.fault,
        health.slave.connected,
        health.slave.rx_alive,
        health.slave.tx_alive,
        health.slave.fault,
    )
}

fn stop_attempt_from_driver_error(err: &DriverError) -> StopAttemptResult {
    match err {
        DriverError::ChannelClosed => StopAttemptResult::ChannelClosed,
        DriverError::ControlPathClosed => StopAttemptResult::ChannelClosed,
        DriverError::ChannelFull => StopAttemptResult::QueueRejected,
        DriverError::Timeout => StopAttemptResult::Timeout,
        DriverError::ReliableDeliveryFailed { .. }
        | DriverError::RealtimeDeliveryAbortedByFault { .. }
        | DriverError::CommandAbortedByFault => StopAttemptResult::TransportFailed,
        _ => StopAttemptResult::TransportFailed,
    }
}

fn stop_attempt_from_robot_error(err: &RobotError) -> StopAttemptResult {
    match err {
        RobotError::Infrastructure(driver) => stop_attempt_from_driver_error(driver),
        _ => StopAttemptResult::TransportFailed,
    }
}

enum PendingExperimentalStopAttempt {
    Receipt(piper_driver::ShutdownReceipt),
    Immediate(StopAttemptResult),
}

fn enqueue_experimental_stop_attempt(
    piper: &Piper<Active<MitPassthroughMode>, SoftRealtime>,
    deadline: Instant,
) -> PendingExperimentalStopAttempt {
    match RawCommander::new(&piper.driver).emergency_stop_enqueue(deadline) {
        Ok(receipt) => PendingExperimentalStopAttempt::Receipt(receipt),
        Err(err) => PendingExperimentalStopAttempt::Immediate(stop_attempt_from_robot_error(&err)),
    }
}

fn resolve_experimental_stop_attempt(pending: PendingExperimentalStopAttempt) -> StopAttemptResult {
    match pending {
        PendingExperimentalStopAttempt::Receipt(receipt) => match receipt.wait() {
            Ok(()) => StopAttemptResult::ConfirmedSent,
            Err(err) => stop_attempt_from_driver_error(&err),
        },
        PendingExperimentalStopAttempt::Immediate(result) => result,
    }
}

fn fault_shutdown_single(
    piper: &Piper<Active<MitPassthroughMode>, SoftRealtime>,
    timeout: Duration,
) -> StopAttemptResult {
    let deadline = Instant::now() + timeout;
    piper.driver.latch_fault();
    let pending = enqueue_experimental_stop_attempt(piper, deadline);
    let result = resolve_experimental_stop_attempt(pending);
    piper.driver.request_stop();
    result
}

fn force_soft_error_state(
    piper: Piper<Active<MitPassthroughMode>, SoftRealtime>,
) -> Piper<ErrorState, SoftRealtime> {
    let piper = std::mem::ManuallyDrop::new(piper);

    Piper {
        // SAFETY: each field is moved exactly once into the replacement state wrapper.
        driver: unsafe { std::ptr::read(&piper.driver) },
        // SAFETY: each field is moved exactly once into the replacement state wrapper.
        observer: unsafe { std::ptr::read(&piper.observer) },
        // SAFETY: each field is moved exactly once into the replacement state wrapper.
        quirks: unsafe { std::ptr::read(&piper.quirks) },
        drop_policy: DropPolicy::Noop,
        driver_mode_drop_policy: DriverModeDropPolicy::Preserve,
        _state: ErrorState,
    }
}

fn percentile(samples: impl IntoIterator<Item = u64>, percentile: u64) -> u64 {
    let mut sorted: Vec<_> = samples.into_iter().collect();
    if sorted.is_empty() {
        return 0;
    }

    sorted.sort_unstable();
    let rank = (percentile as usize * (sorted.len() - 1)).div_ceil(100);
    sorted[rank.min(sorted.len() - 1)]
}

fn raw_clock_warmup_final_grace(thresholds: RawClockThresholds) -> Duration {
    let grace_us = thresholds.sample_gap_max_us.saturating_mul(2).clamp(
        RAW_CLOCK_WARMUP_FINAL_GRACE_MIN_US,
        RAW_CLOCK_WARMUP_FINAL_GRACE_MAX_US,
    );
    Duration::from_micros(grace_us)
}

fn warmup_final_error_is_retryable(error: &RawClockRuntimeError) -> bool {
    match error {
        RawClockRuntimeError::EstimatorNotReady { .. } => true,
        RawClockRuntimeError::ClockUnhealthy { health, .. } => matches!(
            health.failure_kind,
            Some(
                RawClockUnhealthyKind::FitUnavailable
                    | RawClockUnhealthyKind::WarmupSamples
                    | RawClockUnhealthyKind::WarmupWindow
            )
        ),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dual_arm::JointMirrorMap;
    use crate::observer::{ControlSnapshot, Observer};
    use crate::types::{DeviceQuirks, JointArray, NewtonMeter, Rad, RadPerSecond};
    use piper_can::{
        CanError, PiperFrame, RealtimeTxAdapter, ReceivedFrame, RxAdapter, TimestampProvenance,
    };
    use piper_driver::{Piper as DriverPiper, RawFeedbackTiming};
    use piper_protocol::control::{
        ControlModeCommand, ControlModeCommandFrame, InstallPosition, MitControlCommand,
        MitMode as ProtocolMitMode,
    };
    use piper_protocol::feedback::{ControlMode, MotionStatus, MoveMode, RobotStatus, TeachStatus};
    use piper_protocol::ids::{ID_JOINT_DRIVER_LOW_SPEED_1, ID_ROBOT_STATUS};
    use piper_tools::raw_clock::{RawClockHealth, RawClockThresholds, RawClockUnhealthyKind};
    use semver::Version;
    use std::collections::VecDeque;
    use std::convert::Infallible;
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    fn thresholds_for_tests() -> RawClockThresholds {
        RawClockThresholds {
            warmup_samples: 4,
            warmup_window_us: 3_000,
            residual_p95_us: 100,
            residual_max_us: 250,
            drift_abs_ppm: 500.0,
            sample_gap_max_us: 10_000,
            // Fake runtime tests drive timing with synthetic host timestamps while
            // the runtime itself samples the real monotonic clock.
            last_sample_age_us: u64::MAX,
        }
    }

    fn healthy_for_tests() -> RawClockHealth {
        RawClockHealth {
            healthy: true,
            sample_count: 4,
            window_duration_us: 3_000,
            drift_ppm: 0.0,
            residual_p50_us: 0,
            residual_p95_us: 0,
            residual_p99_us: 0,
            residual_max_us: 0,
            sample_gap_max_us: 1_000,
            last_sample_age_us: 100,
            raw_timestamp_regressions: 0,
            failure_kind: None,
            reason: None,
        }
    }

    fn unhealthy_for_tests(kind: RawClockUnhealthyKind, reason: &str) -> RawClockHealth {
        RawClockHealth {
            healthy: false,
            failure_kind: Some(kind),
            reason: Some(reason.to_string()),
            ..healthy_for_tests()
        }
    }

    fn unhealthy_without_kind_for_tests(reason: &str) -> RawClockHealth {
        RawClockHealth {
            healthy: false,
            failure_kind: None,
            reason: Some(reason.to_string()),
            ..healthy_for_tests()
        }
    }

    fn tick_for_health(
        master_health: RawClockHealth,
        slave_health: RawClockHealth,
    ) -> RawClockTickTiming {
        RawClockTickTiming {
            master_feedback_time_us: 100_000,
            slave_feedback_time_us: 100_500,
            inter_arm_skew_us: 500,
            master_selected_sample_age_us: 100,
            slave_selected_sample_age_us: 100,
            master_health,
            slave_health,
        }
    }

    fn assert_close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() < 1e-9,
            "expected {actual} to be close to {expected}"
        );
    }

    fn control_snapshot_for_tests() -> ControlSnapshot {
        ControlSnapshot {
            position: JointArray::splat(Rad(0.0)),
            velocity: JointArray::splat(RadPerSecond(0.0)),
            torque: JointArray::splat(NewtonMeter::ZERO),
            position_timestamp_us: 1,
            dynamic_timestamp_us: 1,
            skew_us: 0,
        }
    }

    fn raw_clock_snapshot_for_tests(
        raw_us: u64,
        host_rx_mono_us: u64,
    ) -> ExperimentalRawClockSnapshot {
        raw_clock_snapshot_with_timing_for_tests(raw_us, host_rx_mono_us, host_rx_mono_us)
    }

    fn raw_clock_snapshot_with_timing_for_tests(
        raw_us: u64,
        host_rx_mono_us: u64,
        system_ts_us: u64,
    ) -> ExperimentalRawClockSnapshot {
        ExperimentalRawClockSnapshot {
            state: control_snapshot_for_tests(),
            newest_raw_feedback_timing: RawFeedbackTiming {
                can_id: 0x251,
                host_rx_mono_us,
                system_ts_us: Some(system_ts_us),
                hw_trans_us: None,
                hw_raw_us: Some(raw_us),
            },
            feedback_age: Duration::from_micros(100),
        }
    }

    fn raw_clock_snapshot_with_positions_for_tests(
        raw_us: u64,
        host_rx_mono_us: u64,
        positions: [f64; 6],
    ) -> ExperimentalRawClockSnapshot {
        let mut snapshot = raw_clock_snapshot_for_tests(raw_us, host_rx_mono_us);
        snapshot.state.position = JointArray::new(positions.map(Rad));
        snapshot
    }

    fn raw_clock_snapshot_without_raw_timing_for_tests() -> ExperimentalRawClockSnapshot {
        ExperimentalRawClockSnapshot {
            state: control_snapshot_for_tests(),
            newest_raw_feedback_timing: RawFeedbackTiming {
                can_id: 0x251,
                host_rx_mono_us: 110_800,
                system_ts_us: Some(110_800),
                hw_trans_us: None,
                hw_raw_us: None,
            },
            feedback_age: Duration::from_micros(100),
        }
    }

    #[test]
    fn snapshot_buffer_selects_latest_before_target_time() {
        let mut buffer = RawClockSnapshotBuffer::new(20_000);
        buffer.push(RawClockAlignedSnapshot::new(
            100_000,
            raw_clock_snapshot_for_tests(10_000, 100_000),
        ));
        buffer.push(RawClockAlignedSnapshot::new(
            105_000,
            raw_clock_snapshot_for_tests(15_000, 105_000),
        ));
        buffer.push(RawClockAlignedSnapshot::new(
            110_000,
            raw_clock_snapshot_for_tests(20_000, 110_000),
        ));

        let selected = buffer
            .latest_before_or_at(107_000)
            .expect("sample before target should be selected");

        assert_eq!(selected.feedback_time_us, 105_000);
    }

    #[test]
    fn snapshot_buffer_never_selects_future_sample() {
        let mut buffer = RawClockSnapshotBuffer::new(20_000);
        buffer.push(RawClockAlignedSnapshot::new(
            105_000,
            raw_clock_snapshot_for_tests(15_000, 105_000),
        ));

        assert!(buffer.latest_before_or_at(104_999).is_none());
    }

    #[test]
    fn snapshot_buffer_prunes_by_retention_window() {
        let mut buffer = RawClockSnapshotBuffer::new(10_000);
        buffer.push(RawClockAlignedSnapshot::new(
            100_000,
            raw_clock_snapshot_for_tests(10_000, 100_000),
        ));
        buffer.push(RawClockAlignedSnapshot::new(
            111_000,
            raw_clock_snapshot_for_tests(21_000, 111_000),
        ));

        assert!(buffer.latest_before_or_at(100_000).is_none());
        assert_eq!(
            buffer.latest_before_or_at(111_000).map(|sample| sample.feedback_time_us),
            Some(111_000)
        );
    }

    fn ready_timing_for_tests() -> RawClockRuntimeTiming {
        let mut timing = RawClockRuntimeTiming::new(thresholds_for_tests());
        timing.seed_ready_for_tests(
            &[
                raw_clock_snapshot_for_tests(7_000, 107_000),
                raw_clock_snapshot_for_tests(8_000, 108_000),
                raw_clock_snapshot_for_tests(9_000, 109_000),
                raw_clock_snapshot_for_tests(10_000, 110_000),
            ],
            &[
                raw_clock_snapshot_for_tests(17_000, 107_800),
                raw_clock_snapshot_for_tests(18_000, 108_800),
                raw_clock_snapshot_for_tests(19_000, 109_800),
                raw_clock_snapshot_for_tests(20_000, 110_800),
            ],
        );
        timing
    }

    fn ready_timing_near_now_for_tests() -> (RawClockRuntimeTiming, u64) {
        let base_host_us = piper_can::monotonic_micros().saturating_sub(5_000);
        let mut timing = RawClockRuntimeTiming::new(thresholds_for_tests());
        timing.seed_ready_for_tests(
            &[
                raw_clock_snapshot_for_tests(7_000, base_host_us),
                raw_clock_snapshot_for_tests(8_000, base_host_us + 1_000),
                raw_clock_snapshot_for_tests(9_000, base_host_us + 2_000),
                raw_clock_snapshot_for_tests(10_000, base_host_us + 3_000),
            ],
            &[
                raw_clock_snapshot_for_tests(17_000, base_host_us + 500),
                raw_clock_snapshot_for_tests(18_000, base_host_us + 1_500),
                raw_clock_snapshot_for_tests(19_000, base_host_us + 2_500),
                raw_clock_snapshot_for_tests(20_000, base_host_us + 3_500),
            ],
        );
        (timing, base_host_us + 4_000)
    }

    fn test_command() -> BilateralCommand {
        BilateralCommand {
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
        }
    }

    type TxEvent = (&'static str, PiperFrame);

    fn received(frame: PiperFrame) -> ReceivedFrame {
        ReceivedFrame::new(frame, TimestampProvenance::None)
    }

    struct TxCountGate {
        label: &'static str,
        events: Arc<Mutex<Vec<TxEvent>>>,
        expected_count: usize,
    }

    impl TxCountGate {
        fn new(
            label: &'static str,
            events: Arc<Mutex<Vec<TxEvent>>>,
            expected_count: usize,
        ) -> Self {
            Self {
                label,
                events,
                expected_count,
            }
        }

        fn is_open(&self) -> bool {
            self.events
                .lock()
                .expect("tx events lock")
                .iter()
                .filter(|(label, _)| *label == self.label)
                .count()
                >= self.expected_count
        }
    }

    struct TimedFrame {
        gate: Option<TxCountGate>,
        gate_open_seen: bool,
        delay: Duration,
        frame: PiperFrame,
    }

    impl TimedFrame {
        fn new(delay: Duration, frame: PiperFrame) -> Self {
            Self {
                gate: None,
                gate_open_seen: false,
                delay,
                frame,
            }
        }

        fn gated(gate: TxCountGate, frame: PiperFrame) -> Self {
            Self {
                gate: Some(gate),
                gate_open_seen: false,
                delay: Duration::ZERO,
                frame,
            }
        }
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
        fn receive(&mut self) -> std::result::Result<ReceivedFrame, CanError> {
            if let Some(frame) = self.bootstrap.take() {
                return Ok(received(frame));
            }
            match self.frames.front_mut() {
                Some(timed) => {
                    if let Some(gate) = &timed.gate {
                        if !gate.is_open() {
                            std::thread::sleep(Duration::from_millis(1));
                            return Err(CanError::Timeout);
                        }
                        if !timed.gate_open_seen {
                            timed.gate_open_seen = true;
                            std::thread::sleep(Duration::from_millis(1));
                            return Err(CanError::Timeout);
                        }
                    }

                    let timed = self.frames.pop_front().expect("front frame should exist");
                    if !timed.delay.is_zero() {
                        std::thread::sleep(timed.delay);
                    }
                    Ok(received(timed.frame))
                },
                None => Err(CanError::Timeout),
            }
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
        fn receive(&mut self) -> std::result::Result<ReceivedFrame, CanError> {
            self.inner.receive()
        }

        fn backend_capability(&self) -> piper_can::BackendCapability {
            piper_can::BackendCapability::SoftRealtime
        }
    }

    struct LabeledRecordingTxAdapter {
        label: &'static str,
        events: Arc<Mutex<Vec<TxEvent>>>,
    }

    impl LabeledRecordingTxAdapter {
        fn new(label: &'static str, events: Arc<Mutex<Vec<TxEvent>>>) -> Self {
            Self { label, events }
        }
    }

    impl RealtimeTxAdapter for LabeledRecordingTxAdapter {
        fn send_control(
            &mut self,
            frame: PiperFrame,
            budget: Duration,
        ) -> std::result::Result<(), CanError> {
            if budget.is_zero() {
                return Err(CanError::Timeout);
            }
            self.events.lock().expect("tx events lock").push((self.label, frame));
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
            self.events.lock().expect("tx events lock").push((self.label, frame));
            Ok(())
        }
    }

    fn bootstrap_timestamp_frame() -> PiperFrame {
        PiperFrame::new_standard(0x251, [0; 8]).unwrap().with_timestamp_us(1)
    }

    fn joint_driver_state_frame(joint_index: u8, enabled: bool, timestamp_us: u64) -> PiperFrame {
        let id = u32::from(ID_JOINT_DRIVER_LOW_SPEED_1.raw()) + u32::from(joint_index) - 1;
        let mut data = [0u8; 8];
        data[0..2].copy_from_slice(&240u16.to_be_bytes());
        data[2..4].copy_from_slice(&45i16.to_be_bytes());
        data[4] = 50;
        data[5] = if enabled { 0x40 } else { 0x00 };
        data[6..8].copy_from_slice(&5000u16.to_be_bytes());
        PiperFrame::new_standard(id, data).unwrap().with_timestamp_us(timestamp_us)
    }

    fn enabled_joint_frames_after_gate(gate: TxCountGate) -> Vec<TimedFrame> {
        let mut frames = vec![TimedFrame::gated(
            gate,
            joint_driver_state_frame(1, true, 1),
        )];
        frames.extend((2..=6).map(|joint_index| {
            TimedFrame::new(
                Duration::ZERO,
                joint_driver_state_frame(joint_index, true, joint_index as u64),
            )
        }));
        frames
    }

    fn robot_status_frame(
        control_mode: ControlMode,
        move_mode: MoveMode,
        timestamp_us: u64,
    ) -> PiperFrame {
        PiperFrame::new_standard(
            ID_ROBOT_STATUS.raw().into(),
            [
                control_mode as u8,
                RobotStatus::Normal as u8,
                move_mode as u8,
                TeachStatus::Closed as u8,
                MotionStatus::Arrived as u8,
                0,
                0,
                0,
            ],
        )
        .unwrap()
        .with_timestamp_us(timestamp_us)
    }

    fn control_mode_echo_frame(
        control_mode: ControlModeCommand,
        move_mode: MoveMode,
        speed_percent: u8,
        mit_mode: ProtocolMitMode,
        install_position: InstallPosition,
        timestamp_us: u64,
    ) -> PiperFrame {
        ControlModeCommandFrame::new(
            control_mode,
            move_mode,
            speed_percent,
            mit_mode,
            0,
            install_position,
        )
        .to_frame()
        .with_timestamp_us(timestamp_us)
    }

    fn build_soft_standby_piper(
        rx_adapter: impl RxAdapter + Send + 'static,
        tx_adapter: impl RealtimeTxAdapter + Send + 'static,
    ) -> Piper<Standby, SoftRealtime> {
        let driver = Arc::new(
            DriverPiper::new_dual_thread_parts(SoftCapabilityRx::new(rx_adapter), tx_adapter, None)
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

    fn active_feedback_script(
        label: &'static str,
        events: Arc<Mutex<Vec<TxEvent>>>,
    ) -> Vec<TimedFrame> {
        let mut frames =
            enabled_joint_frames_after_gate(TxCountGate::new(label, events.clone(), 1));
        frames.push(TimedFrame::gated(
            TxCountGate::new(label, events, 2),
            robot_status_frame(ControlMode::CanControl, MoveMode::MoveM, 100),
        ));
        frames.push(TimedFrame::new(
            Duration::ZERO,
            control_mode_echo_frame(
                ControlModeCommand::CanControl,
                MoveMode::MoveM,
                70,
                ProtocolMitMode::Mit,
                InstallPosition::Invalid,
                101,
            ),
        ));
        frames
    }

    fn build_active_raw_clock_piper(
        label: &'static str,
        events: Arc<Mutex<Vec<TxEvent>>>,
    ) -> Piper<Active<MitPassthroughMode>, SoftRealtime> {
        let standby = build_soft_standby_piper(
            PacedRxAdapter::new(active_feedback_script(label, events.clone())),
            LabeledRecordingTxAdapter::new(label, events),
        );
        let mut active = standby
            .enable_mit_passthrough(MitModeConfig {
                timeout: Duration::from_millis(100),
                debounce_threshold: 1,
                poll_interval: Duration::from_millis(1),
                speed_percent: 70,
            })
            .expect("fake feedback should enable MIT passthrough");
        active.drop_policy = DropPolicy::Noop;
        active
    }

    fn build_active_runtime_for_tests(
        events: Arc<Mutex<Vec<TxEvent>>>,
    ) -> ExperimentalRawClockDualArmActive {
        let master = build_active_raw_clock_piper("master", events.clone());
        let slave = build_active_raw_clock_piper("slave", events.clone());
        events.lock().expect("tx events lock").clear();
        ExperimentalRawClockDualArmActive {
            master,
            slave,
            timing: ready_timing_for_tests(),
            config: ExperimentalRawClockConfig::default(),
        }
    }

    fn wait_for_tx_events(events: &Arc<Mutex<Vec<TxEvent>>>, expected: usize) -> Vec<TxEvent> {
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            let current = events.lock().expect("tx events lock").clone();
            if current.len() >= expected {
                return current;
            }
            assert!(Instant::now() < deadline, "timed out waiting for tx events");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn active_feedback_script_waits_for_matching_label_command_count() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let mut rx = PacedRxAdapter::new(active_feedback_script("slave", events.clone()));

        rx.receive().expect("bootstrap timestamp frame should be immediate");

        events
            .lock()
            .expect("tx events lock")
            .push(("master", PiperFrame::new_standard(0x121, [0]).unwrap()));
        assert!(matches!(rx.receive(), Err(CanError::Timeout)));

        events
            .lock()
            .expect("tx events lock")
            .push(("slave", PiperFrame::new_standard(0x122, [0]).unwrap()));
        assert!(matches!(rx.receive(), Err(CanError::Timeout)));
        let enabled = rx
            .receive()
            .expect("matching slave enable command should release enabled feedback");
        assert_eq!(enabled.frame.id(), ID_JOINT_DRIVER_LOW_SPEED_1.into());

        for _ in 0..5 {
            rx.receive().expect("remaining enabled joint feedback should stay in order");
        }

        events
            .lock()
            .expect("tx events lock")
            .push(("master", PiperFrame::new_standard(0x123, [0]).unwrap()));
        assert!(matches!(rx.receive(), Err(CanError::Timeout)));

        events
            .lock()
            .expect("tx events lock")
            .push(("slave", PiperFrame::new_standard(0x124, [0]).unwrap()));
        assert!(matches!(rx.receive(), Err(CanError::Timeout)));
        let status =
            rx.receive().expect("matching slave mode command should release mode feedback");
        assert_eq!(status.frame.id(), ID_ROBOT_STATUS.into());
    }

    enum FakeRead {
        Pair(
            Box<ExperimentalRawClockSnapshot>,
            Box<ExperimentalRawClockSnapshot>,
        ),
        Error(&'static str),
    }

    impl FakeRead {
        fn pair(master: ExperimentalRawClockSnapshot, slave: ExperimentalRawClockSnapshot) -> Self {
            Self::Pair(Box::new(master), Box::new(slave))
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum FakeCommandFailure {
        Slave,
        Master,
    }

    struct TestCommandController;

    impl BilateralController for TestCommandController {
        type Error = Infallible;

        fn tick(
            &mut self,
            _snapshot: &DualArmSnapshot,
            _dt: Duration,
        ) -> std::result::Result<BilateralCommand, Self::Error> {
            Ok(test_command())
        }
    }

    struct RecordingSkewController {
        seen_skew: Arc<Mutex<Vec<Duration>>>,
    }

    impl BilateralController for RecordingSkewController {
        type Error = Infallible;

        fn tick(
            &mut self,
            snapshot: &DualArmSnapshot,
            _dt: Duration,
        ) -> std::result::Result<BilateralCommand, Self::Error> {
            self.seen_skew.lock().expect("seen skew lock").push(snapshot.inter_arm_skew);
            Ok(test_command())
        }
    }

    struct MirrorJ4CommandController;

    impl BilateralController for MirrorJ4CommandController {
        type Error = Infallible;

        fn tick(
            &mut self,
            snapshot: &DualArmSnapshot,
            _dt: Duration,
        ) -> std::result::Result<BilateralCommand, Self::Error> {
            let mut command = test_command();
            command.slave_position[3] = Rad(-snapshot.left.state.position[3].0);
            Ok(command)
        }
    }

    struct FakeStandbyArms;

    struct FakeErrorArms;

    #[derive(Default)]
    struct FakeIoState {
        command_log: Mutex<Vec<&'static str>>,
        commands: Mutex<Vec<BilateralCommand>>,
        disable_attempts: Mutex<Vec<&'static str>>,
        fault_shutdowns: AtomicUsize,
    }

    struct FakeRuntimeIo {
        state: Arc<FakeIoState>,
        reads: VecDeque<FakeRead>,
        pending_slave: Option<ExperimentalRawClockSnapshot>,
        command_failure: Option<FakeCommandFailure>,
        health: ExperimentalRawClockRuntimeHealth,
        telemetry: RawClockRuntimeTelemetry,
    }

    impl FakeRuntimeIo {
        fn new() -> Self {
            Self {
                state: Arc::new(FakeIoState::default()),
                reads: VecDeque::new(),
                pending_slave: None,
                command_failure: None,
                health: healthy_runtime_for_tests(),
                telemetry: RawClockRuntimeTelemetry::default(),
            }
        }

        fn with_reads(mut self, reads: impl IntoIterator<Item = FakeRead>) -> Self {
            self.reads = reads.into_iter().collect();
            self
        }

        fn with_command_failure(mut self, failure: FakeCommandFailure) -> Self {
            self.command_failure = Some(failure);
            self
        }

        fn with_runtime_fault(mut self) -> Self {
            self.health.master.connected = false;
            self
        }

        fn with_telemetry(mut self, telemetry: RawClockRuntimeTelemetry) -> Self {
            self.telemetry = telemetry;
            self
        }

        fn telemetry(&self) -> RawClockRuntimeTelemetry {
            let mut telemetry = self.telemetry;
            telemetry.last_runtime_fault_master =
                telemetry.last_runtime_fault_master.or(self.health.master.fault);
            telemetry.last_runtime_fault_slave =
                telemetry.last_runtime_fault_slave.or(self.health.slave.fault);
            telemetry
        }
    }

    impl RawClockRuntimeIo for FakeRuntimeIo {
        type StandbyArms = FakeStandbyArms;
        type ErrorArms = FakeErrorArms;

        fn read_master_experimental_snapshot(
            &mut self,
            _policy: ControlReadPolicy,
        ) -> Result<ExperimentalRawClockSnapshot, RawClockRuntimeError> {
            let Some(read) = self.reads.pop_front() else {
                return Err(RawClockRuntimeError::ReadFault {
                    side: "master",
                    source: RobotError::ConfigError("snapshot queue exhausted".to_string()),
                });
            };

            match read {
                FakeRead::Pair(master, slave) => {
                    self.pending_slave = Some(*slave);
                    Ok(*master)
                },
                FakeRead::Error(message) => Err(RawClockRuntimeError::ReadFault {
                    side: "master",
                    source: RobotError::ConfigError(message.to_string()),
                }),
            }
        }

        fn read_slave_experimental_snapshot(
            &mut self,
            _policy: ControlReadPolicy,
        ) -> Result<ExperimentalRawClockSnapshot, RawClockRuntimeError> {
            self.pending_slave.take().ok_or_else(|| RawClockRuntimeError::ReadFault {
                side: "slave",
                source: RobotError::ConfigError("missing pending slave snapshot".to_string()),
            })
        }

        fn runtime_health(&mut self) -> ExperimentalRawClockRuntimeHealth {
            self.health
        }

        fn submit_command(
            &mut self,
            command: &BilateralCommand,
            _timeout: Duration,
        ) -> Result<(), RawClockRuntimeError> {
            self.state.commands.lock().expect("commands lock").push(command.clone());
            self.state.command_log.lock().expect("command log lock").push("slave");
            if matches!(self.command_failure, Some(FakeCommandFailure::Slave)) {
                return Err(RawClockRuntimeError::SubmissionFault {
                    side: RawClockSide::Slave.as_str(),
                    peer_command_may_have_applied: false,
                    source: RobotError::ConfigError("slave command submission failed".to_string()),
                });
            }

            self.state.command_log.lock().expect("command log lock").push("master");
            if matches!(self.command_failure, Some(FakeCommandFailure::Master)) {
                return Err(RawClockRuntimeError::SubmissionFault {
                    side: RawClockSide::Master.as_str(),
                    peer_command_may_have_applied: true,
                    source: RobotError::ConfigError("master command submission failed".to_string()),
                });
            }
            Ok(())
        }

        fn disable_both(
            self,
            _cfg: DisableConfig,
        ) -> Result<RawClockStandbyExit<Self::StandbyArms>, RawClockRuntimeError> {
            let mut attempts = self.state.disable_attempts.lock().expect("disable attempts lock");
            attempts.push("master");
            attempts.push("slave");
            Ok(RawClockStandbyExit {
                arms: FakeStandbyArms,
                telemetry: self.telemetry(),
            })
        }

        fn fault_shutdown(self, _timeout: Duration) -> RawClockFaultExit<Self::ErrorArms> {
            self.state.fault_shutdowns.fetch_add(1, AtomicOrdering::SeqCst);
            RawClockFaultExit {
                arms: FakeErrorArms,
                shutdown: ExperimentalFaultShutdown {
                    master_stop_attempt: StopAttemptResult::ConfirmedSent,
                    slave_stop_attempt: StopAttemptResult::ConfirmedSent,
                },
                telemetry: self.telemetry(),
            }
        }
    }

    fn healthy_runtime_for_tests() -> ExperimentalRawClockRuntimeHealth {
        ExperimentalRawClockRuntimeHealth {
            master: RuntimeHealthSnapshot {
                connected: true,
                last_feedback_age: Duration::from_millis(1),
                rx_alive: true,
                tx_alive: true,
                fault: None,
            },
            slave: RuntimeHealthSnapshot {
                connected: true,
                last_feedback_age: Duration::from_millis(1),
                rx_alive: true,
                tx_alive: true,
                fault: None,
            },
        }
    }

    fn run_fake_runtime(
        io: FakeRuntimeIo,
        timing: RawClockRuntimeTiming,
        max_iterations: usize,
        thresholds: RawClockRuntimeThresholds,
    ) -> (Arc<FakeIoState>, RawClockRuntimeReport) {
        run_fake_runtime_with_controller(
            io,
            timing,
            max_iterations,
            thresholds,
            TestCommandController,
        )
    }

    fn run_fake_runtime_with_controller<C>(
        io: FakeRuntimeIo,
        timing: RawClockRuntimeTiming,
        max_iterations: usize,
        thresholds: RawClockRuntimeThresholds,
        controller: C,
    ) -> (Arc<FakeIoState>, RawClockRuntimeReport)
    where
        C: BilateralController,
    {
        let state = io.state.clone();
        let core = RawClockRuntimeCore {
            io,
            timing,
            config: ExperimentalRawClockConfig {
                mode: ExperimentalRawClockMode::MasterFollower,
                frequency_hz: 10_000.0,
                max_iterations: Some(max_iterations),
                thresholds,
                estimator_thresholds: thresholds_for_tests(),
            },
        };
        let exit =
            run_raw_clock_runtime_core(core, controller, ExperimentalRawClockRunConfig::default())
                .expect("fake runtime core should not return outer error");
        let report = match exit {
            RawClockCoreExit::Standby { report, .. } | RawClockCoreExit::Faulted { report, .. } => {
                report
            },
        };
        (state, report)
    }

    fn calibration_for_raw_clock_gains_test() -> DualArmCalibration {
        DualArmCalibration {
            master_zero: JointArray::splat(Rad(0.0)),
            slave_zero: JointArray::splat(Rad(0.0)),
            map: JointMirrorMap::left_right_mirror(),
        }
    }

    #[test]
    fn experimental_config_rejects_bilateral_mode() {
        let err = ExperimentalRawClockConfig::default()
            .with_mode(ExperimentalRawClockMode::Bilateral)
            .unwrap_err();

        assert!(err.to_string().contains("master-follower"));
    }

    #[test]
    fn experimental_raw_clock_config_default_alignment_is_internally_valid() {
        let config = ExperimentalRawClockConfig::default();

        assert_eq!(config.thresholds.alignment_lag_us, 5_000);
        assert_eq!(
            config.thresholds.alignment_buffer_miss_consecutive_failures,
            3
        );
        assert!(config.thresholds.alignment_lag_us < config.thresholds.last_sample_age_us);
        assert!(
            config.thresholds.alignment_lag_us < config.estimator_thresholds.last_sample_age_us
        );
        config.validate().expect("default raw-clock config should validate");
    }

    #[test]
    fn experimental_raw_clock_config_rejects_zero_alignment_lag() {
        let config = ExperimentalRawClockConfig {
            thresholds: RawClockRuntimeThresholds {
                alignment_lag_us: 0,
                ..RawClockRuntimeThresholds::default()
            },
            ..ExperimentalRawClockConfig::default()
        };

        assert!(config.validate().is_err());
    }

    #[test]
    fn experimental_raw_clock_config_rejects_zero_alignment_buffer_miss_limit() {
        let config = ExperimentalRawClockConfig {
            thresholds: RawClockRuntimeThresholds {
                alignment_buffer_miss_consecutive_failures: 0,
                ..RawClockRuntimeThresholds::default()
            },
            ..ExperimentalRawClockConfig::default()
        };

        assert!(config.validate().is_err());
    }

    #[test]
    fn experimental_raw_clock_config_rejects_alignment_lag_at_runtime_freshness() {
        let config = ExperimentalRawClockConfig {
            thresholds: RawClockRuntimeThresholds {
                alignment_lag_us: 20_000,
                last_sample_age_us: 20_000,
                ..RawClockRuntimeThresholds::default()
            },
            estimator_thresholds: RawClockThresholds {
                last_sample_age_us: 25_000,
                ..thresholds_for_tests()
            },
            ..ExperimentalRawClockConfig::default()
        };

        assert!(config.validate().is_err());
    }

    #[test]
    fn experimental_raw_clock_config_rejects_alignment_lag_at_estimator_freshness() {
        let config = ExperimentalRawClockConfig {
            thresholds: RawClockRuntimeThresholds {
                alignment_lag_us: 20_000,
                last_sample_age_us: 25_000,
                ..RawClockRuntimeThresholds::default()
            },
            estimator_thresholds: RawClockThresholds {
                last_sample_age_us: 20_000,
                ..thresholds_for_tests()
            },
            ..ExperimentalRawClockConfig::default()
        };

        assert!(config.validate().is_err());
    }

    #[test]
    fn raw_clock_master_follower_runtime_uses_configured_gains() {
        let gains = ExperimentalRawClockMasterFollowerGains {
            track_kp: JointArray::splat(9.25),
            track_kd: JointArray::splat(1.75),
            master_damping: JointArray::splat(0.65),
        };
        let controller =
            master_follower_controller_with_gains(calibration_for_raw_clock_gains_test(), gains);
        let io = FakeRuntimeIo::new().with_reads([FakeRead::pair(
            raw_clock_snapshot_for_tests(10_000, 110_000),
            raw_clock_snapshot_for_tests(20_000, 110_800),
        )]);

        let (state, report) = run_fake_runtime_with_controller(
            io,
            ready_timing_for_tests(),
            1,
            RawClockRuntimeThresholds::for_tests(),
            controller,
        );

        assert_eq!(report.iterations, 1);
        assert_eq!(report.submission_faults, 0);
        let commands = state.commands.lock().expect("commands lock");
        assert_eq!(commands.len(), 1);
        let command = &commands[0];
        assert_eq!(command.slave_kp, gains.track_kp);
        assert_eq!(command.slave_kd, gains.track_kd);
        assert_eq!(command.master_kd, gains.master_damping);
    }

    #[test]
    fn skew_above_threshold_fails_health_gate() {
        let gate = RawClockRuntimeGate::new(RawClockRuntimeThresholds {
            inter_arm_skew_max_us: 2_000,
            ..RawClockRuntimeThresholds::for_tests()
        });

        let err = gate
            .check_tick(RawClockTickTiming {
                master_feedback_time_us: 100_000,
                slave_feedback_time_us: 103_000,
                inter_arm_skew_us: 3_000,
                master_selected_sample_age_us: 100,
                slave_selected_sample_age_us: 100,
                master_health: healthy_for_tests(),
                slave_health: healthy_for_tests(),
            })
            .unwrap_err();

        assert!(matches!(err, RawClockRuntimeError::InterArmSkew { .. }));
    }

    #[test]
    fn residual_max_single_spike_is_counted_without_faulting_before_limit() {
        let mut timing = RawClockRuntimeTiming::new(thresholds_for_tests());
        let thresholds = RawClockRuntimeThresholds {
            residual_max_consecutive_failures: 3,
            ..RawClockRuntimeThresholds::for_tests()
        };

        let result = timing.check_tick_with_debounce(
            tick_for_health(
                unhealthy_for_tests(
                    RawClockUnhealthyKind::ResidualMax,
                    "residual max 3001us exceeds threshold 3000us",
                ),
                healthy_for_tests(),
            ),
            thresholds,
        );

        assert!(result.is_ok());
        assert_eq!(timing.master_residual_max_spikes, 1);
        assert_eq!(timing.master_residual_max_consecutive_failures, 1);
    }

    #[test]
    fn residual_max_faults_after_configured_consecutive_failures() {
        let mut timing = RawClockRuntimeTiming::new(thresholds_for_tests());
        let thresholds = RawClockRuntimeThresholds {
            residual_max_consecutive_failures: 2,
            ..RawClockRuntimeThresholds::for_tests()
        };

        let first = timing.check_tick_with_debounce(
            tick_for_health(
                unhealthy_for_tests(
                    RawClockUnhealthyKind::ResidualMax,
                    "residual max 3001us exceeds threshold 3000us",
                ),
                healthy_for_tests(),
            ),
            thresholds,
        );
        assert!(first.is_ok());

        let err = timing
            .check_tick_with_debounce(
                tick_for_health(
                    unhealthy_for_tests(
                        RawClockUnhealthyKind::ResidualMax,
                        "residual max 3002us exceeds threshold 3000us",
                    ),
                    healthy_for_tests(),
                ),
                thresholds,
            )
            .unwrap_err();

        assert!(matches!(
            err,
            RawClockRuntimeError::ClockUnhealthy { side: "master", .. }
        ));
        assert_eq!(timing.master_residual_max_spikes, 2);
        assert_eq!(timing.master_residual_max_consecutive_failures, 2);
    }

    #[test]
    fn healthy_tick_resets_residual_max_consecutive_counter() {
        let mut timing = RawClockRuntimeTiming::new(thresholds_for_tests());
        let thresholds = RawClockRuntimeThresholds {
            residual_max_consecutive_failures: 2,
            ..RawClockRuntimeThresholds::for_tests()
        };

        timing
            .check_tick_with_debounce(
                tick_for_health(
                    unhealthy_for_tests(RawClockUnhealthyKind::ResidualMax, "residual max"),
                    healthy_for_tests(),
                ),
                thresholds,
            )
            .unwrap();
        timing
            .check_tick_with_debounce(
                tick_for_health(healthy_for_tests(), healthy_for_tests()),
                thresholds,
            )
            .unwrap();

        assert_eq!(timing.master_residual_max_consecutive_failures, 0);
        assert_eq!(timing.master_residual_max_spikes, 1);
    }

    #[test]
    fn non_residual_max_failure_resets_counter_and_fails_immediately() {
        let mut timing = RawClockRuntimeTiming::new(thresholds_for_tests());
        let thresholds = RawClockRuntimeThresholds {
            residual_max_consecutive_failures: 3,
            ..RawClockRuntimeThresholds::for_tests()
        };

        timing
            .check_tick_with_debounce(
                tick_for_health(
                    unhealthy_for_tests(RawClockUnhealthyKind::ResidualMax, "residual max"),
                    healthy_for_tests(),
                ),
                thresholds,
            )
            .unwrap();

        let err = timing
            .check_tick_with_debounce(
                tick_for_health(
                    unhealthy_for_tests(RawClockUnhealthyKind::ResidualP95, "residual p95"),
                    healthy_for_tests(),
                ),
                thresholds,
            )
            .unwrap_err();

        assert!(matches!(
            err,
            RawClockRuntimeError::ClockUnhealthy { side: "master", .. }
        ));
        assert_eq!(timing.master_residual_max_consecutive_failures, 0);
    }

    #[test]
    fn mixed_same_tick_failure_counts_residual_max_side_before_fault() {
        let mut timing = RawClockRuntimeTiming::new(thresholds_for_tests());
        let thresholds = RawClockRuntimeThresholds {
            residual_max_consecutive_failures: 3,
            ..RawClockRuntimeThresholds::for_tests()
        };

        let err = timing
            .check_tick_with_debounce(
                tick_for_health(
                    unhealthy_for_tests(RawClockUnhealthyKind::ResidualMax, "residual max"),
                    unhealthy_for_tests(RawClockUnhealthyKind::ResidualP95, "residual p95"),
                ),
                thresholds,
            )
            .unwrap_err();

        assert!(matches!(
            err,
            RawClockRuntimeError::ClockUnhealthy { side: "slave", .. }
        ));
        assert_eq!(timing.master_residual_max_spikes, 1);
        assert_eq!(timing.master_residual_max_consecutive_failures, 1);
    }

    #[test]
    fn mixed_same_tick_failure_prefers_fail_fast_error_over_residual_max_threshold() {
        let mut timing = RawClockRuntimeTiming::new(thresholds_for_tests());
        let thresholds = RawClockRuntimeThresholds {
            residual_max_consecutive_failures: 1,
            ..RawClockRuntimeThresholds::for_tests()
        };

        let err = timing
            .check_tick_with_debounce(
                tick_for_health(
                    unhealthy_for_tests(RawClockUnhealthyKind::ResidualMax, "residual max"),
                    unhealthy_for_tests(RawClockUnhealthyKind::ResidualP95, "residual p95"),
                ),
                thresholds,
            )
            .unwrap_err();

        assert!(matches!(
            err,
            RawClockRuntimeError::ClockUnhealthy { side: "slave", .. }
        ));
        assert_eq!(timing.master_residual_max_spikes, 1);
        assert_eq!(timing.master_residual_max_consecutive_failures, 1);
    }

    #[test]
    fn dual_residual_max_tick_updates_both_sides_before_threshold_error() {
        let mut timing = RawClockRuntimeTiming::new(thresholds_for_tests());
        let thresholds = RawClockRuntimeThresholds {
            residual_max_consecutive_failures: 1,
            ..RawClockRuntimeThresholds::for_tests()
        };

        let err = timing
            .check_tick_with_debounce(
                tick_for_health(
                    unhealthy_for_tests(RawClockUnhealthyKind::ResidualMax, "master residual max"),
                    unhealthy_for_tests(RawClockUnhealthyKind::ResidualMax, "slave residual max"),
                ),
                thresholds,
            )
            .unwrap_err();

        assert!(matches!(err, RawClockRuntimeError::ClockUnhealthy { .. }));
        assert_eq!(timing.master_residual_max_spikes, 1);
        assert_eq!(timing.slave_residual_max_spikes, 1);
        assert_eq!(timing.master_residual_max_consecutive_failures, 1);
        assert_eq!(timing.slave_residual_max_consecutive_failures, 1);
    }

    #[test]
    fn final_pre_run_gate_bypasses_residual_max_debounce() {
        let gate = RawClockRuntimeGate::new(RawClockRuntimeThresholds {
            residual_max_consecutive_failures: 3,
            ..RawClockRuntimeThresholds::for_tests()
        });

        let err = gate
            .check_tick(tick_for_health(
                unhealthy_for_tests(RawClockUnhealthyKind::ResidualMax, "residual max"),
                healthy_for_tests(),
            ))
            .unwrap_err();

        assert!(matches!(
            err,
            RawClockRuntimeError::ClockUnhealthy { side: "master", .. }
        ));
    }

    #[test]
    fn unknown_unhealthy_kind_fails_immediately_and_resets_counter() {
        let mut timing = RawClockRuntimeTiming::new(thresholds_for_tests());
        let thresholds = RawClockRuntimeThresholds {
            residual_max_consecutive_failures: 3,
            ..RawClockRuntimeThresholds::for_tests()
        };

        timing
            .check_tick_with_debounce(
                tick_for_health(
                    unhealthy_for_tests(RawClockUnhealthyKind::ResidualMax, "residual max"),
                    healthy_for_tests(),
                ),
                thresholds,
            )
            .unwrap();

        let err = timing
            .check_tick_with_debounce(
                tick_for_health(
                    unhealthy_without_kind_for_tests("legacy unknown unhealthy reason"),
                    healthy_for_tests(),
                ),
                thresholds,
            )
            .unwrap_err();

        assert!(matches!(
            err,
            RawClockRuntimeError::ClockUnhealthy { side: "master", .. }
        ));
        assert_eq!(timing.master_residual_max_consecutive_failures, 0);
    }

    #[test]
    fn inter_arm_skew_remains_fail_fast_with_debounce_gate() {
        let mut timing = RawClockRuntimeTiming::new(thresholds_for_tests());
        let thresholds = RawClockRuntimeThresholds {
            inter_arm_skew_max_us: 2_000,
            residual_max_consecutive_failures: 3,
            ..RawClockRuntimeThresholds::for_tests()
        };

        let err = timing
            .check_tick_with_debounce(
                RawClockTickTiming {
                    master_feedback_time_us: 100_000,
                    slave_feedback_time_us: 103_001,
                    inter_arm_skew_us: 3_001,
                    master_selected_sample_age_us: 100,
                    slave_selected_sample_age_us: 100,
                    master_health: healthy_for_tests(),
                    slave_health: healthy_for_tests(),
                },
                thresholds,
            )
            .unwrap_err();

        assert!(matches!(err, RawClockRuntimeError::InterArmSkew { .. }));
    }

    #[test]
    fn stale_sample_age_remains_fail_fast_even_when_health_is_otherwise_healthy() {
        let mut timing = RawClockRuntimeTiming::new(thresholds_for_tests());
        let thresholds = RawClockRuntimeThresholds {
            last_sample_age_us: 20,
            residual_max_consecutive_failures: 3,
            ..RawClockRuntimeThresholds::for_tests()
        };
        let mut stale_master = healthy_for_tests();
        stale_master.last_sample_age_us = 21;

        let err = timing
            .check_tick_with_debounce(
                tick_for_health(stale_master, healthy_for_tests()),
                thresholds,
            )
            .unwrap_err();

        assert!(matches!(
            err,
            RawClockRuntimeError::ClockUnhealthy { side: "master", .. }
        ));
        assert_eq!(timing.master_residual_max_consecutive_failures, 0);
    }

    #[test]
    fn reset_runtime_residual_max_counters_clears_warmup_state() {
        let mut timing = RawClockRuntimeTiming::new(thresholds_for_tests());
        let thresholds = RawClockRuntimeThresholds {
            residual_max_consecutive_failures: 3,
            ..RawClockRuntimeThresholds::for_tests()
        };

        timing
            .check_tick_with_debounce(
                tick_for_health(
                    unhealthy_for_tests(RawClockUnhealthyKind::ResidualMax, "residual max"),
                    healthy_for_tests(),
                ),
                thresholds,
            )
            .unwrap();
        timing.reset_runtime_residual_max_counters();

        assert_eq!(timing.master_residual_max_spikes, 0);
        assert_eq!(timing.master_residual_max_consecutive_failures, 0);
    }

    #[test]
    fn tick_timing_uses_newest_raw_feedback_from_control_snapshots() {
        let mut timing = RawClockRuntimeTiming::new(thresholds_for_tests());
        let master = raw_clock_snapshot_for_tests(10_000, 110_000);
        let slave = raw_clock_snapshot_for_tests(20_000, 110_800);

        timing.seed_ready_for_tests(
            &[
                raw_clock_snapshot_for_tests(7_000, 107_000),
                raw_clock_snapshot_for_tests(8_000, 108_000),
                raw_clock_snapshot_for_tests(9_000, 109_000),
                raw_clock_snapshot_for_tests(10_000, 110_000),
            ],
            &[
                raw_clock_snapshot_for_tests(17_000, 107_800),
                raw_clock_snapshot_for_tests(18_000, 108_800),
                raw_clock_snapshot_for_tests(19_000, 109_800),
                raw_clock_snapshot_for_tests(20_000, 110_800),
            ],
        );
        let tick = timing.tick_from_snapshots(&master, &slave, 110_900).unwrap();

        assert_eq!(tick.master_feedback_time_us, 110_000);
        assert_eq!(tick.slave_feedback_time_us, 110_800);
        assert_eq!(tick.inter_arm_skew_us, 800);
    }

    #[test]
    fn selected_tick_uses_selected_skew_without_reingesting_raw_timestamps() {
        let mut timing = RawClockRuntimeTiming::new(thresholds_for_tests());
        timing.seed_ready_for_tests(
            &[
                raw_clock_snapshot_for_tests(7_000, 107_000),
                raw_clock_snapshot_for_tests(8_000, 108_000),
                raw_clock_snapshot_for_tests(9_000, 109_000),
                raw_clock_snapshot_for_tests(10_000, 110_000),
            ],
            &[
                raw_clock_snapshot_for_tests(17_000, 107_800),
                raw_clock_snapshot_for_tests(18_000, 108_800),
                raw_clock_snapshot_for_tests(19_000, 109_800),
                raw_clock_snapshot_for_tests(20_000, 110_800),
            ],
        );
        let latest = timing
            .ingest_latest_snapshots(
                &raw_clock_snapshot_for_tests(11_000, 111_000),
                &raw_clock_snapshot_for_tests(21_000, 111_800),
                112_000,
            )
            .unwrap();
        let selected = timing
            .selected_tick_from_buffered_times(&latest, 109_000, 109_800, 112_000)
            .unwrap();

        assert_eq!(selected.inter_arm_skew_us, 800);
        assert_eq!(selected.master_health.raw_timestamp_regressions, 0);
        assert_eq!(selected.slave_health.raw_timestamp_regressions, 0);
    }

    #[test]
    fn selected_tick_uses_selected_sample_age_for_runtime_gate() {
        let mut timing = ready_timing_for_tests();
        let latest = timing
            .ingest_latest_snapshots(
                &raw_clock_snapshot_for_tests(11_000, 111_000),
                &raw_clock_snapshot_for_tests(21_000, 111_800),
                112_000,
            )
            .unwrap();
        let selected = timing
            .selected_tick_from_buffered_times(&latest, 100_000, 111_800, 112_000)
            .unwrap();

        let err = timing
            .check_tick_with_debounce(
                selected,
                RawClockRuntimeThresholds {
                    last_sample_age_us: 5_000,
                    ..RawClockRuntimeThresholds::default()
                },
            )
            .unwrap_err();

        assert!(matches!(
            err,
            RawClockRuntimeError::ClockUnhealthy { side: "master", .. }
        ));
    }

    #[test]
    fn timing_uses_kernel_timestamp_anchor_for_fit_health() {
        let mut timing = RawClockRuntimeTiming::new(thresholds_for_tests());
        timing.seed_ready_for_tests(
            &[
                raw_clock_snapshot_with_timing_for_tests(10_000, 110_000, 1_010_000),
                raw_clock_snapshot_with_timing_for_tests(11_000, 116_000, 1_011_000),
                raw_clock_snapshot_with_timing_for_tests(12_000, 117_000, 1_012_000),
                raw_clock_snapshot_with_timing_for_tests(13_000, 118_000, 1_013_000),
            ],
            &[
                raw_clock_snapshot_with_timing_for_tests(20_000, 110_500, 1_010_800),
                raw_clock_snapshot_with_timing_for_tests(21_000, 116_500, 1_011_800),
                raw_clock_snapshot_with_timing_for_tests(22_000, 117_500, 1_012_800),
                raw_clock_snapshot_with_timing_for_tests(23_000, 118_500, 1_013_800),
            ],
        );
        let master = raw_clock_snapshot_with_timing_for_tests(14_000, 119_000, 1_014_000);
        let slave = raw_clock_snapshot_with_timing_for_tests(24_000, 119_500, 1_014_800);

        let tick = timing.tick_from_snapshots(&master, &slave, 119_600).unwrap();

        assert_eq!(tick.master_feedback_time_us, 1_014_000);
        assert_eq!(tick.slave_feedback_time_us, 1_014_800);
        assert_eq!(tick.inter_arm_skew_us, 800);
        assert!(tick.master_health.healthy, "{:?}", tick.master_health);
        assert!(tick.slave_health.healthy, "{:?}", tick.slave_health);
        assert_eq!(tick.master_health.residual_p95_us, 0);
        assert_eq!(tick.slave_health.residual_p95_us, 0);
        assert_eq!(tick.master_health.last_sample_age_us, 600);
        assert_eq!(tick.slave_health.last_sample_age_us, 100);
    }

    #[test]
    fn missing_raw_feedback_timing_fails_closed() {
        let mut timing = RawClockRuntimeTiming::new(thresholds_for_tests());
        let master = raw_clock_snapshot_for_tests(10_000, 110_000);
        let slave = raw_clock_snapshot_without_raw_timing_for_tests();

        let err = timing.tick_from_snapshots(&master, &slave, 110_900).unwrap_err();
        assert!(matches!(
            err,
            RawClockRuntimeError::MissingRawFeedbackTiming { .. }
        ));
    }

    #[test]
    fn warmup_not_ready_samples_do_not_abort() {
        let mut timing = RawClockRuntimeTiming::new(thresholds_for_tests());
        let master = raw_clock_snapshot_for_tests(10_000, 110_000);
        let slave = raw_clock_snapshot_for_tests(20_000, 110_800);

        timing.ingest_snapshots(&master, &slave).unwrap();
        let err = timing.tick_from_snapshots(&master, &slave, 110_900).unwrap_err();
        assert!(matches!(
            err,
            RawClockRuntimeError::EstimatorNotReady { .. }
        ));
        assert_eq!(timing.sample_counts_for_tests(), (1, 1));
    }

    #[test]
    fn slave_command_submits_before_master_command() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let active = build_active_runtime_for_tests(events.clone());
        let mut command = test_command();
        command.master_interaction_torque[0] = NewtonMeter(3.0);

        active
            .submit_command(&command, Duration::from_millis(100))
            .expect("real SoftRealtime submit_command should succeed");

        let events = wait_for_tx_events(&events, 12);
        let labels: Vec<_> = events.iter().map(|(label, _)| *label).collect();
        assert_eq!(&labels[0..6], &["slave"; 6]);
        assert_eq!(&labels[6..12], &["master"; 6]);

        let expected_master_zero = MitControlCommand::try_new(1, 0.0, 0.0, 0.0, 0.0, 0.0)
            .expect("zero master command should be valid")
            .to_frame();
        assert_eq!(events[6].1.id(), expected_master_zero.id());
        assert_eq!(events[6].1.data(), expected_master_zero.data());
    }

    #[test]
    fn raw_clock_joint_motion_stats_capture_feedback_and_command_ranges() {
        let mut motion = RawClockJointMotionAccumulator::default();
        let mut command = test_command();

        let mut first_master = raw_clock_snapshot_for_tests(10_000, 110_000);
        first_master.state.position = JointArray::new([0.0, 0.0, 0.0, -1.2, 0.0, 0.0].map(Rad));
        let mut first_slave = raw_clock_snapshot_for_tests(20_000, 110_500);
        first_slave.state.position = JointArray::new([0.0, 0.0, 0.0, -1.1, 0.0, 0.0].map(Rad));
        command.slave_position = JointArray::new([0.0, 0.0, 0.0, 1.1, 0.0, 0.0].map(Rad));
        motion.record(
            &raw_dual_arm_snapshot(&first_master, &first_slave, 500),
            &command,
        );

        let mut second_master = raw_clock_snapshot_for_tests(11_000, 111_000);
        second_master.state.position = JointArray::new([0.0, 0.0, 0.0, -0.7, 0.0, 0.0].map(Rad));
        let mut second_slave = raw_clock_snapshot_for_tests(21_000, 111_500);
        second_slave.state.position = JointArray::new([0.0, 0.0, 0.0, -1.1, 0.0, 0.0].map(Rad));
        command.slave_position = JointArray::new([0.0, 0.0, 0.0, 0.6, 0.0, 0.0].map(Rad));
        motion.record(
            &raw_dual_arm_snapshot(&second_master, &second_slave, 500),
            &command,
        );

        let stats = motion.snapshot().expect("motion stats should be present");
        assert_close(stats.master_feedback_delta_rad[3], 0.5);
        assert_close(stats.slave_command_delta_rad[3], 0.5);
        assert_close(stats.slave_feedback_delta_rad[3], 0.0);
        assert_close(stats.master_feedback_min_rad[3], -1.2);
        assert_close(stats.master_feedback_max_rad[3], -0.7);
        assert_close(stats.slave_command_min_rad[3], 0.6);
        assert_close(stats.slave_command_max_rad[3], 1.1);
    }

    #[test]
    fn raw_clock_runtime_report_includes_joint_motion_stats() {
        let (timing, first_host_us) = ready_timing_near_now_for_tests();
        let io = FakeRuntimeIo::new().with_reads([
            FakeRead::pair(
                raw_clock_snapshot_with_positions_for_tests(
                    11_000,
                    first_host_us,
                    [0.0, 0.0, 0.0, -1.2, 0.0, 0.0],
                ),
                raw_clock_snapshot_with_positions_for_tests(
                    21_000,
                    first_host_us + 500,
                    [0.0, 0.0, 0.0, -1.1, 0.0, 0.0],
                ),
            ),
            FakeRead::pair(
                raw_clock_snapshot_with_positions_for_tests(
                    12_000,
                    first_host_us + 1_000,
                    [0.0, 0.0, 0.0, -0.7, 0.0, 0.0],
                ),
                raw_clock_snapshot_with_positions_for_tests(
                    22_000,
                    first_host_us + 1_500,
                    [0.0, 0.0, 0.0, -1.1, 0.0, 0.0],
                ),
            ),
        ]);

        let (_state, report) = run_fake_runtime_with_controller(
            io,
            timing,
            2,
            RawClockRuntimeThresholds::for_tests(),
            MirrorJ4CommandController,
        );

        let stats = report.joint_motion.expect("joint motion stats should be present");
        assert_close(stats.master_feedback_delta_rad[3], 0.5);
        assert_close(stats.slave_command_delta_rad[3], 0.5);
        assert_close(stats.slave_feedback_delta_rad[3], 0.0);
    }

    #[test]
    fn dual_wrapper_slave_enable_failure_stops_already_enabled_master() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let master = build_soft_standby_piper(
            PacedRxAdapter::new(active_feedback_script("master", events.clone())),
            LabeledRecordingTxAdapter::new("master", events.clone()),
        );
        let slave = build_soft_standby_piper(
            PacedRxAdapter::new(Vec::new()),
            LabeledRecordingTxAdapter::new("slave", events.clone()),
        );
        let standby = ExperimentalRawClockDualArmStandby {
            master,
            slave,
            timing: ready_timing_for_tests(),
            config: ExperimentalRawClockConfig::default(),
        };

        let err = match standby.enable_mit_passthrough(
            MitModeConfig {
                timeout: Duration::from_millis(100),
                debounce_threshold: 1,
                poll_interval: Duration::from_millis(1),
                speed_percent: 70,
            },
            MitModeConfig {
                timeout: Duration::from_millis(20),
                debounce_threshold: 1,
                poll_interval: Duration::from_millis(1),
                speed_percent: 70,
            },
        ) {
            Ok(_) => panic!("slave enable timeout must fail the dual wrapper enable"),
            Err(err) => err,
        };

        assert!(matches!(
            err,
            RawClockRuntimeError::EnableFailed {
                failed_side: "slave",
                enabled_side_stop_attempt: Some(StopAttemptResult::ConfirmedSent),
                ..
            }
        ));
        let events = events.lock().expect("tx events lock").clone();
        let master_events = events.iter().filter(|(label, _)| *label == "master").count();
        assert!(
            master_events >= 3,
            "master TX path should receive bounded stop after slave enable failure"
        );
    }

    #[test]
    fn runtime_gate_failure_prevents_another_normal_command() {
        let io = FakeRuntimeIo::new().with_reads([
            FakeRead::pair(
                raw_clock_snapshot_for_tests(10_000, 110_000),
                raw_clock_snapshot_for_tests(20_000, 110_800),
            ),
            FakeRead::pair(
                raw_clock_snapshot_for_tests(11_000, 111_000),
                raw_clock_snapshot_for_tests(21_000, 113_000),
            ),
        ]);

        let (state, report) = run_fake_runtime(
            io,
            ready_timing_for_tests(),
            2,
            RawClockRuntimeThresholds {
                inter_arm_skew_max_us: 1_000,
                ..RawClockRuntimeThresholds::for_tests()
            },
        );

        assert_eq!(
            *state.command_log.lock().expect("command log lock"),
            vec!["slave", "master"]
        );
        assert!(matches!(
            report.exit_reason,
            Some(
                RawClockRuntimeExitReason::RawClockFault
                    | RawClockRuntimeExitReason::ClockHealthFault
            )
        ));
        assert_eq!(state.fault_shutdowns.load(AtomicOrdering::SeqCst), 1);
    }

    #[test]
    fn timing_failure_calls_fault_shutdown_and_records_both_stop_attempts() {
        let io = FakeRuntimeIo::new().with_reads([FakeRead::pair(
            raw_clock_snapshot_for_tests(9_999, 111_000),
            raw_clock_snapshot_for_tests(20_000, 110_800),
        )]);

        let (state, report) = run_fake_runtime(
            io,
            ready_timing_for_tests(),
            1,
            RawClockRuntimeThresholds::for_tests(),
        );

        assert_eq!(
            *state.command_log.lock().expect("command log lock"),
            Vec::<&'static str>::new()
        );
        assert_eq!(state.fault_shutdowns.load(AtomicOrdering::SeqCst), 1);
        assert_eq!(report.master_stop_attempt, StopAttemptResult::ConfirmedSent);
        assert_eq!(report.slave_stop_attempt, StopAttemptResult::ConfirmedSent);
        assert_eq!(
            report.exit_reason,
            Some(RawClockRuntimeExitReason::RawClockFault)
        );
        assert_eq!(report.master.raw_timestamp_regressions, 1);
        assert_eq!(report.slave.raw_timestamp_regressions, 0);
    }

    #[test]
    fn snapshot_read_failure_calls_fault_shutdown_and_records_both_stop_attempts() {
        let io = FakeRuntimeIo::new().with_reads([FakeRead::Error("snapshot read failed")]);

        let (state, report) = run_fake_runtime(
            io,
            ready_timing_for_tests(),
            1,
            RawClockRuntimeThresholds::for_tests(),
        );

        assert_eq!(
            *state.command_log.lock().expect("command log lock"),
            Vec::<&'static str>::new()
        );
        assert_eq!(state.fault_shutdowns.load(AtomicOrdering::SeqCst), 1);
        assert_eq!(report.read_faults, 1);
        assert_eq!(report.master_stop_attempt, StopAttemptResult::ConfirmedSent);
        assert_eq!(report.slave_stop_attempt, StopAttemptResult::ConfirmedSent);
    }

    #[test]
    fn command_submission_failure_calls_fault_shutdown_and_records_both_stop_attempts() {
        let io = FakeRuntimeIo::new()
            .with_reads([FakeRead::pair(
                raw_clock_snapshot_for_tests(10_000, 110_000),
                raw_clock_snapshot_for_tests(20_000, 110_800),
            )])
            .with_command_failure(FakeCommandFailure::Master);

        let (state, report) = run_fake_runtime(
            io,
            ready_timing_for_tests(),
            1,
            RawClockRuntimeThresholds::for_tests(),
        );

        assert_eq!(
            *state.command_log.lock().expect("command log lock"),
            vec!["slave", "master"]
        );
        assert_eq!(state.fault_shutdowns.load(AtomicOrdering::SeqCst), 1);
        assert_eq!(report.submission_faults, 1);
        assert_eq!(
            report.last_submission_failed_side,
            Some(RawClockSide::Master)
        );
        assert!(report.peer_command_may_have_applied);
        assert_eq!(report.master_stop_attempt, StopAttemptResult::ConfirmedSent);
        assert_eq!(report.slave_stop_attempt, StopAttemptResult::ConfirmedSent);
    }

    #[test]
    fn slave_command_submission_failure_stops_before_master_command() {
        let io = FakeRuntimeIo::new()
            .with_reads([FakeRead::pair(
                raw_clock_snapshot_for_tests(10_000, 110_000),
                raw_clock_snapshot_for_tests(20_000, 110_800),
            )])
            .with_command_failure(FakeCommandFailure::Slave);

        let (state, report) = run_fake_runtime(
            io,
            ready_timing_for_tests(),
            1,
            RawClockRuntimeThresholds::for_tests(),
        );

        assert_eq!(
            *state.command_log.lock().expect("command log lock"),
            vec!["slave"]
        );
        assert_eq!(report.submission_faults, 1);
        assert_eq!(
            report.last_submission_failed_side,
            Some(RawClockSide::Slave)
        );
        assert!(!report.peer_command_may_have_applied);
        assert_eq!(state.fault_shutdowns.load(AtomicOrdering::SeqCst), 1);
    }

    #[test]
    fn runtime_transport_fault_calls_fault_shutdown_and_records_both_stop_attempts() {
        let mut io = FakeRuntimeIo::new().with_runtime_fault().with_reads([FakeRead::pair(
            raw_clock_snapshot_for_tests(10_000, 110_000),
            raw_clock_snapshot_for_tests(20_000, 110_800),
        )]);
        io.health.master.fault = Some(RuntimeFaultKind::TransportError);

        let (state, report) = run_fake_runtime(
            io,
            ready_timing_for_tests(),
            1,
            RawClockRuntimeThresholds::for_tests(),
        );

        assert_eq!(
            *state.command_log.lock().expect("command log lock"),
            Vec::<&'static str>::new()
        );
        assert_eq!(state.fault_shutdowns.load(AtomicOrdering::SeqCst), 1);
        assert_eq!(report.runtime_faults, 1);
        assert_eq!(
            report.last_runtime_fault_master,
            Some(RuntimeFaultKind::TransportError)
        );
        assert_eq!(report.last_runtime_fault_slave, None);
        assert_eq!(
            report.exit_reason,
            Some(RawClockRuntimeExitReason::RuntimeTransportFault)
        );
        assert_eq!(report.master_stop_attempt, StopAttemptResult::ConfirmedSent);
        assert_eq!(report.slave_stop_attempt, StopAttemptResult::ConfirmedSent);
    }

    #[test]
    fn missing_raw_feedback_timing_fails_closed_before_command_submission() {
        let io = FakeRuntimeIo::new().with_reads([FakeRead::pair(
            raw_clock_snapshot_for_tests(10_000, 110_000),
            raw_clock_snapshot_without_raw_timing_for_tests(),
        )]);

        let (state, report) = run_fake_runtime(
            io,
            ready_timing_for_tests(),
            1,
            RawClockRuntimeThresholds::for_tests(),
        );

        assert_eq!(
            *state.command_log.lock().expect("command log lock"),
            Vec::<&'static str>::new()
        );
        assert_eq!(state.fault_shutdowns.load(AtomicOrdering::SeqCst), 1);
        assert_eq!(
            report.exit_reason,
            Some(RawClockRuntimeExitReason::RawClockFault)
        );
    }

    #[test]
    fn per_tick_skew_is_mapped_from_raw_feedback_not_host_receive_or_command_time() {
        let mut timing = RawClockRuntimeTiming::new(thresholds_for_tests());
        timing.seed_ready_for_tests(
            &[
                raw_clock_snapshot_for_tests(7_000, 107_000),
                raw_clock_snapshot_for_tests(8_000, 108_000),
                raw_clock_snapshot_for_tests(9_000, 109_000),
                raw_clock_snapshot_for_tests(10_000, 110_000),
            ],
            &[
                raw_clock_snapshot_for_tests(17_000, 117_000),
                raw_clock_snapshot_for_tests(18_000, 118_000),
                raw_clock_snapshot_for_tests(19_000, 119_000),
                raw_clock_snapshot_for_tests(20_000, 120_000),
            ],
        );
        let mut master = raw_clock_snapshot_for_tests(10_000, 999_000);
        master.newest_raw_feedback_timing.host_rx_mono_us = 999_000;
        let mut slave = raw_clock_snapshot_for_tests(20_000, 999_000);
        slave.newest_raw_feedback_timing.host_rx_mono_us = 999_000;

        let tick = timing.tick_from_snapshots(&master, &slave, 999_500).unwrap();

        assert_eq!(tick.master_feedback_time_us, 110_000);
        assert_eq!(tick.slave_feedback_time_us, 120_000);
        assert_eq!(tick.inter_arm_skew_us, 10_000);
    }

    #[test]
    fn controller_snapshot_receives_calibrated_raw_skew_not_host_receive_skew() {
        let seen_skew = Arc::new(Mutex::new(Vec::new()));
        let controller = RecordingSkewController {
            seen_skew: seen_skew.clone(),
        };
        let io = FakeRuntimeIo::new().with_reads([FakeRead::pair(
            raw_clock_snapshot_for_tests(10_000, 999_000),
            raw_clock_snapshot_for_tests(20_000, 999_000),
        )]);
        let core = RawClockRuntimeCore {
            io,
            timing: ready_timing_for_tests(),
            config: ExperimentalRawClockConfig {
                mode: ExperimentalRawClockMode::MasterFollower,
                frequency_hz: 10_000.0,
                max_iterations: Some(1),
                thresholds: RawClockRuntimeThresholds {
                    inter_arm_skew_max_us: 20_000,
                    ..RawClockRuntimeThresholds::for_tests()
                },
                estimator_thresholds: thresholds_for_tests(),
            },
        };

        let exit =
            run_raw_clock_runtime_core(core, controller, ExperimentalRawClockRunConfig::default())
                .expect("fake runtime core should run");
        assert!(matches!(exit, RawClockCoreExit::Standby { .. }));

        assert_eq!(
            *seen_skew.lock().expect("seen skew lock"),
            vec![Duration::from_micros(800)]
        );
    }

    #[test]
    fn warmup_starting_empty_collects_not_ready_samples_instead_of_aborting() {
        let mut timing = RawClockRuntimeTiming::new(thresholds_for_tests());
        timing
            .warmup_tick_from_snapshots(
                &raw_clock_snapshot_for_tests(10_000, 110_000),
                &raw_clock_snapshot_for_tests(20_000, 110_800),
                110_900,
                RawClockRuntimeThresholds::for_tests(),
            )
            .unwrap();

        assert_eq!(timing.sample_counts_for_tests(), (1, 1));
    }

    #[test]
    fn warmup_reset_clears_previous_estimator_window() {
        let mut timing = RawClockRuntimeTiming::new(thresholds_for_tests());
        timing
            .warmup_tick_from_snapshots(
                &raw_clock_snapshot_for_tests(10_000, 110_000),
                &raw_clock_snapshot_for_tests(20_000, 110_800),
                110_900,
                RawClockRuntimeThresholds::for_tests(),
            )
            .unwrap();

        timing.reset_for_warmup();

        assert_eq!(timing.sample_counts_for_tests(), (0, 0));
        timing
            .warmup_tick_from_snapshots(
                &raw_clock_snapshot_for_tests(100_000, 300_000),
                &raw_clock_snapshot_for_tests(110_000, 300_800),
                300_900,
                RawClockRuntimeThresholds::for_tests(),
            )
            .unwrap();
        assert_eq!(timing.sample_counts_for_tests(), (1, 1));
    }

    #[test]
    fn continuity_boundary_ignores_mode_transition_gap_for_health_gate() {
        let thresholds = RawClockThresholds {
            warmup_samples: 4,
            warmup_window_us: 12_000,
            residual_p95_us: 20,
            residual_max_us: 50,
            drift_abs_ppm: 500.0,
            sample_gap_max_us: 2_000,
            last_sample_age_us: 2_000,
        };
        let mut timing = RawClockRuntimeTiming::new(thresholds);
        timing.seed_ready_for_tests(
            &[
                raw_clock_snapshot_for_tests(10_000, 110_000),
                raw_clock_snapshot_for_tests(11_000, 111_000),
                raw_clock_snapshot_for_tests(12_000, 112_000),
                raw_clock_snapshot_for_tests(13_000, 113_000),
            ],
            &[
                raw_clock_snapshot_for_tests(20_000, 110_800),
                raw_clock_snapshot_for_tests(21_000, 111_800),
                raw_clock_snapshot_for_tests(22_000, 112_800),
                raw_clock_snapshot_for_tests(23_000, 113_800),
            ],
        );

        timing.mark_continuity_boundary();

        let tick = timing
            .tick_from_snapshots(
                &raw_clock_snapshot_for_tests(23_000, 123_000),
                &raw_clock_snapshot_for_tests(33_000, 123_800),
                123_900,
            )
            .expect("mode transition boundary should preserve fit while ignoring boundary gap");

        assert!(tick.master_health.healthy, "{:?}", tick.master_health);
        assert!(tick.slave_health.healthy, "{:?}", tick.slave_health);
        assert_eq!(tick.master_health.sample_gap_max_us, 1_000);
        assert_eq!(tick.slave_health.sample_gap_max_us, 1_000);
    }

    #[test]
    fn warmup_mapped_but_unhealthy_tick_keeps_collecting_until_final_gate() {
        let mut timing = RawClockRuntimeTiming::new(RawClockThresholds {
            warmup_samples: 2,
            warmup_window_us: 10_000,
            residual_p95_us: 100,
            residual_max_us: 250,
            drift_abs_ppm: 500.0,
            sample_gap_max_us: 10_000,
            last_sample_age_us: 5_000,
        });
        let thresholds = RawClockRuntimeThresholds::for_tests();

        timing
            .warmup_tick_from_snapshots(
                &raw_clock_snapshot_for_tests(10_000, 110_000),
                &raw_clock_snapshot_for_tests(20_000, 110_000),
                110_100,
                thresholds,
            )
            .unwrap();
        timing
            .warmup_tick_from_snapshots(
                &raw_clock_snapshot_for_tests(16_000, 116_000),
                &raw_clock_snapshot_for_tests(26_000, 116_000),
                116_100,
                thresholds,
            )
            .unwrap();

        assert_eq!(timing.sample_counts_for_tests(), (2, 2));
        assert_eq!(timing.clock_health_failures, 0);
    }

    #[test]
    fn warmup_final_retry_is_limited_to_collection_incomplete_errors() {
        let estimator_not_ready = RawClockRuntimeError::EstimatorNotReady { side: "master" };
        assert!(warmup_final_error_is_retryable(&estimator_not_ready));

        for kind in [
            RawClockUnhealthyKind::FitUnavailable,
            RawClockUnhealthyKind::WarmupSamples,
            RawClockUnhealthyKind::WarmupWindow,
        ] {
            let error = RawClockRuntimeError::ClockUnhealthy {
                side: "slave",
                health: Box::new(unhealthy_for_tests(kind, "warmup incomplete")),
            };
            assert!(warmup_final_error_is_retryable(&error));
        }

        let residual_p95 = RawClockRuntimeError::ClockUnhealthy {
            side: "slave",
            health: Box::new(unhealthy_for_tests(
                RawClockUnhealthyKind::ResidualP95,
                "residual p95",
            )),
        };
        assert!(!warmup_final_error_is_retryable(&residual_p95));

        let inter_arm_skew = RawClockRuntimeError::InterArmSkew {
            inter_arm_skew_us: 6_001,
            max_us: 6_000,
        };
        assert!(!warmup_final_error_is_retryable(&inter_arm_skew));
    }

    #[test]
    fn runtime_raw_timestamp_regression_is_reflected_in_report_health() {
        let mut timing = ready_timing_for_tests();

        let err = timing
            .tick_from_snapshots(
                &raw_clock_snapshot_for_tests(9_999, 111_000),
                &raw_clock_snapshot_for_tests(20_000, 110_800),
                111_100,
            )
            .unwrap_err();

        assert!(matches!(
            err,
            RawClockRuntimeError::RawTimestampRegression {
                side: "master",
                previous_raw_us: 10_000,
                raw_us: 9_999
            }
        ));
        let report = timing.report(111_100, 0, Some(RawClockRuntimeExitReason::RawClockFault));
        assert_eq!(report.master.raw_timestamp_regressions, 1);
        assert_eq!(
            report.master.failure_kind,
            Some(RawClockUnhealthyKind::RawTimestampRegression)
        );
        assert!(
            report
                .master
                .reason
                .as_deref()
                .is_some_and(|reason| reason.contains("raw timestamp regression"))
        );
        assert_eq!(report.slave.raw_timestamp_regressions, 0);
    }

    #[test]
    fn runtime_raw_timestamp_regression_overrides_residual_max_report_health() {
        let mut timing = RawClockRuntimeTiming::new(RawClockThresholds {
            residual_p95_us: 10_000,
            residual_max_us: 100,
            drift_abs_ppm: 1_000_000.0,
            ..thresholds_for_tests()
        });

        for i in 0..8 {
            timing
                .master
                .push(RawClockSample {
                    raw_us: 10_000 + i * 1_000,
                    host_rx_mono_us: 110_000 + i * 1_000,
                })
                .unwrap();
        }
        timing
            .master
            .push(RawClockSample {
                raw_us: 18_500,
                host_rx_mono_us: 118_800,
            })
            .unwrap();
        timing.master_raw_timestamp_regressions = 1;

        let health = timing.report_health(RawClockSide::Master, 118_800);

        assert!(!health.healthy);
        assert_eq!(health.raw_timestamp_regressions, 1);
        assert_eq!(
            health.failure_kind,
            Some(RawClockUnhealthyKind::RawTimestampRegression)
        );
        assert!(
            health
                .reason
                .as_deref()
                .is_some_and(|reason| reason.contains("raw timestamp regression"))
        );
    }

    #[test]
    fn duplicate_raw_timestamp_is_ignored_without_regression_counter() {
        let mut timing = ready_timing_for_tests();

        timing
            .tick_from_snapshots(
                &raw_clock_snapshot_for_tests(10_000, 111_000),
                &raw_clock_snapshot_for_tests(20_000, 111_800),
                111_900,
            )
            .expect("duplicate raw timestamp should reuse existing estimator sample");

        let report = timing.report(111_900, 1, Some(RawClockRuntimeExitReason::MaxIterations));
        assert_eq!(timing.sample_counts_for_tests(), (4, 4));
        assert_eq!(report.master.raw_timestamp_regressions, 0);
        assert_eq!(report.slave.raw_timestamp_regressions, 0);
    }

    #[test]
    fn runtime_report_includes_transport_telemetry_from_io() {
        let telemetry = RawClockRuntimeTelemetry {
            master_tx_realtime_overwrites_total: 2,
            slave_tx_realtime_overwrites_total: 3,
            master_tx_frames_sent_total: 11,
            slave_tx_frames_sent_total: 13,
            master_tx_fault_aborts_total: 5,
            slave_tx_fault_aborts_total: 7,
            last_runtime_fault_master: None,
            last_runtime_fault_slave: None,
        };
        let io = FakeRuntimeIo::new()
            .with_reads([FakeRead::pair(
                raw_clock_snapshot_for_tests(10_000, 110_000),
                raw_clock_snapshot_for_tests(20_000, 110_800),
            )])
            .with_telemetry(telemetry);

        let (_state, report) = run_fake_runtime(
            io,
            ready_timing_for_tests(),
            1,
            RawClockRuntimeThresholds::for_tests(),
        );

        assert_eq!(report.master_tx_realtime_overwrites_total, 2);
        assert_eq!(report.slave_tx_realtime_overwrites_total, 3);
        assert_eq!(report.master_tx_frames_sent_total, 11);
        assert_eq!(report.slave_tx_frames_sent_total, 13);
        assert_eq!(report.master_tx_fault_aborts_total, 5);
        assert_eq!(report.slave_tx_fault_aborts_total, 7);
    }

    #[test]
    fn telemetry_merge_preserves_runtime_fault_observed_before_shutdown_latch() {
        let mut report = ready_timing_for_tests().report(
            120_000,
            0,
            Some(RawClockRuntimeExitReason::RuntimeTransportFault),
        );
        report.last_runtime_fault_master = Some(RuntimeFaultKind::TransportError);

        report.apply_telemetry(RawClockRuntimeTelemetry {
            last_runtime_fault_master: Some(RuntimeFaultKind::ManualFault),
            ..RawClockRuntimeTelemetry::default()
        });

        assert_eq!(
            report.last_runtime_fault_master,
            Some(RuntimeFaultKind::TransportError)
        );
    }

    #[test]
    fn runtime_report_includes_max_and_p95_inter_arm_skew() {
        let mut timing = RawClockRuntimeTiming::new(thresholds_for_tests());
        for sample in 0..=100 {
            timing.record_skew_sample(sample);
        }

        let report = timing.report(120_000, 101, Some(RawClockRuntimeExitReason::MaxIterations));

        assert_eq!(report.max_inter_arm_skew_us, 100);
        assert_eq!(report.inter_arm_skew_p95_us, 95);
    }

    #[test]
    fn skew_sample_retention_is_bounded_while_max_tracks_full_run() {
        let mut timing = RawClockRuntimeTiming::new(thresholds_for_tests());
        let sample_count = RAW_CLOCK_SKEW_RETAINED_SAMPLE_CAPACITY as u64 + 64;

        for sample in 0..sample_count {
            timing.record_skew_sample(sample);
        }

        assert_eq!(
            timing.skew_samples_us.len(),
            RAW_CLOCK_SKEW_RETAINED_SAMPLE_CAPACITY
        );
        assert_eq!(timing.max_inter_arm_skew_us, sample_count - 1);

        let report = timing.report(120_000, sample_count as usize, None);
        assert_eq!(report.max_inter_arm_skew_us, sample_count - 1);
        assert!(report.inter_arm_skew_p95_us >= sample_count - 256);
    }

    #[test]
    fn real_fault_shutdown_attempts_both_sides_and_records_results() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let active = build_active_runtime_for_tests(events.clone());

        let (_arms, shutdown) = active.fault_shutdown(Duration::from_millis(50));

        assert_eq!(
            shutdown.master_stop_attempt,
            StopAttemptResult::ConfirmedSent
        );
        assert_eq!(
            shutdown.slave_stop_attempt,
            StopAttemptResult::ConfirmedSent
        );
        let events = wait_for_tx_events(&events, 2);
        let labels: Vec<_> = events.iter().map(|(label, _)| *label).collect();
        assert!(labels.contains(&"master"));
        assert!(labels.contains(&"slave"));
    }

    #[test]
    fn disable_both_attempts_both_sides_on_clean_cancellation() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let active = build_active_runtime_for_tests(events.clone());

        let err = match active.disable_both(DisableConfig {
            timeout: Duration::from_millis(5),
            debounce_threshold: 1,
            poll_interval: Duration::from_millis(1),
        }) {
            Ok(_) => panic!("missing disabled feedback should fail clean disable"),
            Err(err) => err,
        };

        assert!(matches!(
            err,
            RawClockRuntimeError::DisableBothFailed { .. }
        ));
        let events = wait_for_tx_events(&events, 2);
        let labels: Vec<_> = events.iter().map(|(label, _)| *label).collect();
        assert!(labels.contains(&"master"));
        assert!(labels.contains(&"slave"));
    }
}
