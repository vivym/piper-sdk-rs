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
                last_sample_age_us: 5_000,
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
}

impl Default for RawClockRuntimeThresholds {
    fn default() -> Self {
        Self {
            inter_arm_skew_max_us: 2_000,
            last_sample_age_us: 5_000,
        }
    }
}

impl RawClockRuntimeThresholds {
    #[cfg(test)]
    const fn for_tests() -> Self {
        Self {
            inter_arm_skew_max_us: 2_000,
            last_sample_age_us: 2_000,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RawClockTickTiming {
    pub master_feedback_time_us: u64,
    pub slave_feedback_time_us: u64,
    pub inter_arm_skew_us: u64,
    pub master_health: RawClockHealth,
    pub slave_health: RawClockHealth,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RawClockRuntimeReport {
    pub master: RawClockHealth,
    pub slave: RawClockHealth,
    pub max_inter_arm_skew_us: u64,
    pub inter_arm_skew_p95_us: u64,
    pub clock_health_failures: u64,
    pub read_faults: u32,
    pub submission_faults: u32,
    pub runtime_faults: u32,
    pub iterations: usize,
    pub exit_reason: Option<RawClockRuntimeExitReason>,
    pub master_stop_attempt: StopAttemptResult,
    pub slave_stop_attempt: StopAttemptResult,
    pub last_error: Option<String>,
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

pub struct RawClockRuntimeTiming {
    master: RawClockEstimator,
    slave: RawClockEstimator,
    skew_samples_us: VecDeque<u64>,
    max_inter_arm_skew_us: u64,
    clock_health_failures: u64,
    last_master_raw_us: Option<u64>,
    last_slave_raw_us: Option<u64>,
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
        }
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
        let inter_arm_skew_us = master_feedback_time_us.abs_diff(slave_feedback_time_us);
        self.record_skew_sample(inter_arm_skew_us);

        Ok(RawClockTickTiming {
            master_feedback_time_us,
            slave_feedback_time_us,
            inter_arm_skew_us,
            master_health: self.master.health(now_host_us),
            slave_health: self.slave.health(now_host_us),
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
                Err(RawClockRuntimeError::ClockUnhealthy { .. }) => {
                    self.record_clock_health_failure();
                    Ok(())
                },
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

        let sample = RawClockSample {
            raw_us,
            host_rx_mono_us: snapshot.newest_raw_feedback_timing.host_rx_mono_us,
        };
        let push_result = match side {
            RawClockSide::Master => self.master.push(sample),
            RawClockSide::Slave => self.slave.push(sample),
        };
        map_raw_clock_push_error(side, push_result)?;
        *previous = Some(raw_us);
        Ok(())
    }

    fn record_clock_health_failure(&mut self) {
        self.clock_health_failures = self.clock_health_failures.saturating_add(1);
    }

    fn record_skew_sample(&mut self, inter_arm_skew_us: u64) {
        self.max_inter_arm_skew_us = self.max_inter_arm_skew_us.max(inter_arm_skew_us);
        if self.skew_samples_us.len() == RAW_CLOCK_SKEW_RETAINED_SAMPLE_CAPACITY {
            self.skew_samples_us.pop_front();
        }
        self.skew_samples_us.push_back(inter_arm_skew_us);
    }

    fn report(
        &self,
        now_host_us: u64,
        iterations: usize,
        exit_reason: Option<RawClockRuntimeExitReason>,
    ) -> RawClockRuntimeReport {
        RawClockRuntimeReport {
            master: self.master.health(now_host_us),
            slave: self.slave.health(now_host_us),
            max_inter_arm_skew_us: self.max_inter_arm_skew_us,
            inter_arm_skew_p95_us: percentile(self.skew_samples_us.iter().copied(), 95),
            clock_health_failures: self.clock_health_failures,
            read_faults: 0,
            submission_faults: 0,
            runtime_faults: 0,
            iterations,
            exit_reason,
            master_stop_attempt: StopAttemptResult::NotAttempted,
            slave_stop_attempt: StopAttemptResult::NotAttempted,
            last_error: None,
        }
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

        let master = self.read_master_experimental_snapshot(policy)?;
        let slave = self.read_slave_experimental_snapshot(policy)?;
        let final_tick =
            self.timing
                .tick_from_snapshots(&master, &slave, piper_can::monotonic_micros())?;
        RawClockRuntimeGate::new(self.config.thresholds).check_tick(final_tick)?;
        Ok(self)
    }

    pub fn enable_mit_passthrough(
        self,
        master_cfg: MitModeConfig,
        slave_cfg: MitModeConfig,
    ) -> Result<ExperimentalRawClockDualArmActive, RawClockRuntimeError> {
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

    fn submit_command(&mut self, command: &BilateralCommand, timeout: Duration) -> RobotResult<()>;

    fn disable_both(self, cfg: DisableConfig) -> Result<Self::StandbyArms, RawClockRuntimeError>;

    fn fault_shutdown(self, timeout: Duration) -> (Self::ErrorArms, ExperimentalFaultShutdown);
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

    fn submit_command(&mut self, command: &BilateralCommand, timeout: Duration) -> RobotResult<()> {
        submit_soft_realtime_command(&self.slave, &self.master, command, timeout)
    }

    fn disable_both(self, cfg: DisableConfig) -> Result<Self::StandbyArms, RawClockRuntimeError> {
        disable_soft_realtime_both(self.master, self.slave, cfg)
    }

    fn fault_shutdown(self, timeout: Duration) -> (Self::ErrorArms, ExperimentalFaultShutdown) {
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
        let arms = disable_soft_realtime_both(self.master, self.slave, cfg)?;
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
        fault_shutdown_soft_realtime(self.master, self.slave, timeout)
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
            let arms = io.disable_both(cfg.disable_config.clone())?;
            return Ok(RawClockCoreExit::Standby {
                arms,
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
            let arms = io.disable_both(cfg.disable_config.clone())?;
            return Ok(RawClockCoreExit::Standby {
                arms,
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
                exit_reason,
                err.to_string(),
                |report| report.runtime_faults = report.runtime_faults.saturating_add(1),
            );
            let (arms, shutdown) = io.fault_shutdown(FAULT_SHUTDOWN_TIMEOUT);
            return Ok(RawClockCoreExit::Faulted {
                arms,
                report: report.with_shutdown(shutdown),
            });
        }

        let master = match io.read_master_experimental_snapshot(cfg.read_policy) {
            Ok(snapshot) => snapshot,
            Err(err) => {
                let report = fault_report_from_timing(
                    &timing,
                    iterations,
                    RawClockRuntimeExitReason::ReadFault,
                    err.to_string(),
                    |report| report.read_faults = report.read_faults.saturating_add(1),
                );
                let (arms, shutdown) = io.fault_shutdown(FAULT_SHUTDOWN_TIMEOUT);
                return Ok(RawClockCoreExit::Faulted {
                    arms,
                    report: report.with_shutdown(shutdown),
                });
            },
        };
        let slave = match io.read_slave_experimental_snapshot(cfg.read_policy) {
            Ok(snapshot) => snapshot,
            Err(err) => {
                let report = fault_report_from_timing(
                    &timing,
                    iterations,
                    RawClockRuntimeExitReason::ReadFault,
                    err.to_string(),
                    |report| report.read_faults = report.read_faults.saturating_add(1),
                );
                let (arms, shutdown) = io.fault_shutdown(FAULT_SHUTDOWN_TIMEOUT);
                return Ok(RawClockCoreExit::Faulted {
                    arms,
                    report: report.with_shutdown(shutdown),
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
                    RawClockRuntimeExitReason::RawClockFault,
                    err.to_string(),
                    |_| {},
                );
                let (arms, shutdown) = io.fault_shutdown(FAULT_SHUTDOWN_TIMEOUT);
                return Ok(RawClockCoreExit::Faulted {
                    arms,
                    report: report.with_shutdown(shutdown),
                });
            },
        };

        let inter_arm_skew_us = tick.inter_arm_skew_us;
        if let Err(err) = RawClockRuntimeGate::new(config.thresholds).check_tick(tick) {
            if matches!(err, RawClockRuntimeError::ClockUnhealthy { .. }) {
                timing.record_clock_health_failure();
            }
            let exit_reason = if matches!(err, RawClockRuntimeError::ClockUnhealthy { .. }) {
                RawClockRuntimeExitReason::ClockHealthFault
            } else {
                RawClockRuntimeExitReason::RawClockFault
            };
            let report =
                fault_report_from_timing(&timing, iterations, exit_reason, err.to_string(), |_| {});
            let (arms, shutdown) = io.fault_shutdown(FAULT_SHUTDOWN_TIMEOUT);
            return Ok(RawClockCoreExit::Faulted {
                arms,
                report: report.with_shutdown(shutdown),
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
                    RawClockRuntimeExitReason::ControllerFault,
                    err.to_string(),
                    |_| {},
                );
                let (arms, shutdown) = io.fault_shutdown(FAULT_SHUTDOWN_TIMEOUT);
                return Ok(RawClockCoreExit::Faulted {
                    arms,
                    report: report.with_shutdown(shutdown),
                });
            },
        };

        if let Err(err) = io.submit_command(&command, cfg.command_timeout) {
            let report = fault_report_from_timing(
                &timing,
                iterations,
                RawClockRuntimeExitReason::SubmissionFault,
                err.to_string(),
                |report| report.submission_faults = report.submission_faults.saturating_add(1),
            );
            let (arms, shutdown) = io.fault_shutdown(FAULT_SHUTDOWN_TIMEOUT);
            return Ok(RawClockCoreExit::Faulted {
                arms,
                report: report.with_shutdown(shutdown),
            });
        }

        iterations = iterations.saturating_add(1);
    }
}

fn fault_report_from_timing(
    timing: &RawClockRuntimeTiming,
    iterations: usize,
    exit_reason: RawClockRuntimeExitReason,
    error: String,
    update: impl FnOnce(&mut RawClockRuntimeReport),
) -> RawClockRuntimeReport {
    let mut report = timing.report(piper_can::monotonic_micros(), iterations, Some(exit_reason));
    report.last_error = Some(error);
    update(&mut report);
    report
}

fn submit_soft_realtime_command(
    slave: &Piper<Active<MitPassthroughMode>, SoftRealtime>,
    master: &Piper<Active<MitPassthroughMode>, SoftRealtime>,
    command: &BilateralCommand,
    timeout: Duration,
) -> RobotResult<()> {
    slave.command_torques_confirmed(
        &command.slave_position,
        &command.slave_velocity,
        &command.slave_kp,
        &command.slave_kd,
        &command.slave_feedforward_torque,
        timeout,
    )?;
    master.command_torques_confirmed(
        &command.master_position,
        &command.master_velocity,
        &command.master_kp,
        &command.master_kd,
        &JointArray::splat(NewtonMeter::ZERO),
        timeout,
    )?;
    Ok(())
}

fn disable_soft_realtime_both(
    master: Piper<Active<MitPassthroughMode>, SoftRealtime>,
    slave: Piper<Active<MitPassthroughMode>, SoftRealtime>,
    cfg: DisableConfig,
) -> Result<RealRawClockStandbyArms, RawClockRuntimeError> {
    let master_result = master.disable(cfg.clone());
    let slave_result = slave.disable(cfg);
    match (master_result, slave_result) {
        (Ok(master), Ok(slave)) => Ok(RealRawClockStandbyArms { master, slave }),
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
) -> (ExperimentalRawClockErrorState, ExperimentalFaultShutdown) {
    let deadline = Instant::now() + timeout;
    master.driver.latch_fault();
    slave.driver.latch_fault();
    let master_pending = enqueue_experimental_stop_attempt(&master, deadline);
    let slave_pending = enqueue_experimental_stop_attempt(&slave, deadline);
    let master_stop_attempt = resolve_experimental_stop_attempt(master_pending);
    let slave_stop_attempt = resolve_experimental_stop_attempt(slave_pending);
    master.driver.request_stop();
    slave.driver.request_stop();
    (
        ExperimentalRawClockErrorState {
            master: force_soft_error_state(master),
            slave: force_soft_error_state(slave),
        },
        ExperimentalFaultShutdown {
            master_stop_attempt,
            slave_stop_attempt,
        },
    )
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
    use piper_tools::raw_clock::{RawClockHealth, RawClockThresholds};
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
            last_sample_age_us: 2_000,
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
            reason: None,
        }
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
        ExperimentalRawClockSnapshot {
            state: control_snapshot_for_tests(),
            newest_raw_feedback_timing: RawFeedbackTiming {
                can_id: 0x251,
                host_rx_mono_us,
                system_ts_us: Some(host_rx_mono_us),
                hw_trans_us: None,
                hw_raw_us: Some(raw_us),
            },
            feedback_age: Duration::from_micros(100),
        }
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
        fn receive(&mut self) -> std::result::Result<ReceivedFrame, CanError> {
            if let Some(frame) = self.bootstrap.take() {
                return Ok(received(frame));
            }
            match self.frames.pop_front() {
                Some(timed) => {
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

    fn enabled_joint_frames_after(delay: Duration) -> Vec<TimedFrame> {
        (1..=6)
            .map(|joint_index| TimedFrame {
                delay: if joint_index == 1 {
                    delay
                } else {
                    Duration::ZERO
                },
                frame: joint_driver_state_frame(joint_index, true, joint_index as u64),
            })
            .collect()
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

    fn active_feedback_script() -> Vec<TimedFrame> {
        let mut frames = enabled_joint_frames_after(Duration::from_millis(1));
        frames.push(TimedFrame {
            delay: Duration::from_millis(5),
            frame: robot_status_frame(ControlMode::CanControl, MoveMode::MoveM, 100),
        });
        frames.push(TimedFrame {
            delay: Duration::ZERO,
            frame: control_mode_echo_frame(
                ControlModeCommand::CanControl,
                MoveMode::MoveM,
                70,
                ProtocolMitMode::Mit,
                InstallPosition::Invalid,
                101,
            ),
        });
        frames
    }

    fn build_active_raw_clock_piper(
        label: &'static str,
        events: Arc<Mutex<Vec<TxEvent>>>,
    ) -> Piper<Active<MitPassthroughMode>, SoftRealtime> {
        let standby = build_soft_standby_piper(
            PacedRxAdapter::new(active_feedback_script()),
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
    }

    impl FakeRuntimeIo {
        fn new() -> Self {
            Self {
                state: Arc::new(FakeIoState::default()),
                reads: VecDeque::new(),
                pending_slave: None,
                command_failure: None,
                health: healthy_runtime_for_tests(),
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
        ) -> RobotResult<()> {
            self.state.commands.lock().expect("commands lock").push(command.clone());
            self.state.command_log.lock().expect("command log lock").push("slave");
            if matches!(self.command_failure, Some(FakeCommandFailure::Slave)) {
                return Err(RobotError::ConfigError(
                    "slave command submission failed".to_string(),
                ));
            }

            self.state.command_log.lock().expect("command log lock").push("master");
            if matches!(self.command_failure, Some(FakeCommandFailure::Master)) {
                return Err(RobotError::ConfigError(
                    "master command submission failed".to_string(),
                ));
            }
            Ok(())
        }

        fn disable_both(
            self,
            _cfg: DisableConfig,
        ) -> Result<Self::StandbyArms, RawClockRuntimeError> {
            let mut attempts = self.state.disable_attempts.lock().expect("disable attempts lock");
            attempts.push("master");
            attempts.push("slave");
            Ok(FakeStandbyArms)
        }

        fn fault_shutdown(
            self,
            _timeout: Duration,
        ) -> (Self::ErrorArms, ExperimentalFaultShutdown) {
            self.state.fault_shutdowns.fetch_add(1, AtomicOrdering::SeqCst);
            (
                FakeErrorArms,
                ExperimentalFaultShutdown {
                    master_stop_attempt: StopAttemptResult::ConfirmedSent,
                    slave_stop_attempt: StopAttemptResult::ConfirmedSent,
                },
            )
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
                master_health: healthy_for_tests(),
                slave_health: healthy_for_tests(),
            })
            .unwrap_err();

        assert!(matches!(err, RawClockRuntimeError::InterArmSkew { .. }));
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
    fn dual_wrapper_slave_enable_failure_stops_already_enabled_master() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let master = build_soft_standby_piper(
            PacedRxAdapter::new(active_feedback_script()),
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
        assert_eq!(state.fault_shutdowns.load(AtomicOrdering::SeqCst), 1);
    }

    #[test]
    fn runtime_transport_fault_calls_fault_shutdown_and_records_both_stop_attempts() {
        let io = FakeRuntimeIo::new().with_runtime_fault().with_reads([FakeRead::pair(
            raw_clock_snapshot_for_tests(10_000, 110_000),
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
        assert_eq!(report.runtime_faults, 1);
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
                    last_sample_age_us: u64::MAX,
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
        assert_eq!(timing.clock_health_failures, 1);
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
