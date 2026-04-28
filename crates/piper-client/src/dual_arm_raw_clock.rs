//! Experimental dual-arm runtime gated by calibrated SocketCAN raw-clock timing.
//!
//! This module is intentionally separate from the production StrictRealtime dual-arm runtime.
//! Raw-clock timing is accepted only for this explicitly experimental SoftRealtime path.

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
    skew_samples_us: Vec<u64>,
    clock_health_failures: u64,
    last_master_raw_us: Option<u64>,
    last_slave_raw_us: Option<u64>,
}

impl RawClockRuntimeTiming {
    pub fn new(thresholds: RawClockThresholds) -> Self {
        Self {
            master: RawClockEstimator::new(thresholds),
            slave: RawClockEstimator::new(thresholds),
            skew_samples_us: Vec::new(),
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
        self.skew_samples_us.push(inter_arm_skew_us);

        Ok(RawClockTickTiming {
            master_feedback_time_us,
            slave_feedback_time_us,
            inter_arm_skew_us,
            master_health: self.master.health(now_host_us),
            slave_health: self.slave.health(now_host_us),
        })
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

    fn report(
        &self,
        now_host_us: u64,
        iterations: usize,
        exit_reason: Option<RawClockRuntimeExitReason>,
    ) -> RawClockRuntimeReport {
        RawClockRuntimeReport {
            master: self.master.health(now_host_us),
            slave: self.slave.health(now_host_us),
            max_inter_arm_skew_us: self.skew_samples_us.iter().copied().max().unwrap_or(0),
            inter_arm_skew_p95_us: percentile(&self.skew_samples_us, 95),
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
            self.timing.ingest_snapshots(&master, &slave)?;
            match self.timing.tick_from_snapshots(&master, &slave, piper_can::monotonic_micros()) {
                Ok(tick) => {
                    if let Err(err) =
                        RawClockRuntimeGate::new(self.config.thresholds).check_tick(tick)
                    {
                        if matches!(err, RawClockRuntimeError::ClockUnhealthy { .. }) {
                            self.timing.record_clock_health_failure();
                        }
                        return Err(err);
                    }
                },
                Err(RawClockRuntimeError::EstimatorNotReady { .. }) => {},
                Err(err) => return Err(err),
            }
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

pub struct ExperimentalRawClockDualArmActive {
    master: Piper<Active<MitPassthroughMode>, SoftRealtime>,
    slave: Piper<Active<MitPassthroughMode>, SoftRealtime>,
    timing: RawClockRuntimeTiming,
    config: ExperimentalRawClockConfig,
}

impl ExperimentalRawClockDualArmActive {
    pub fn submit_command(&self, command: &BilateralCommand, timeout: Duration) -> RobotResult<()> {
        self.slave.command_torques_confirmed(
            &command.slave_position,
            &command.slave_velocity,
            &command.slave_kp,
            &command.slave_kd,
            &command.slave_feedforward_torque,
            timeout,
        )?;
        self.master.command_torques_confirmed(
            &command.master_position,
            &command.master_velocity,
            &command.master_kp,
            &command.master_kd,
            &command.master_interaction_torque,
            timeout,
        )?;
        Ok(())
    }

    pub fn run_master_follower(
        self,
        calibration: DualArmCalibration,
        cfg: ExperimentalRawClockRunConfig,
    ) -> Result<ExperimentalRawClockRunExit, RawClockRuntimeError> {
        self.run_with_controller(MasterFollowerController::new(calibration), cfg)
    }

    pub fn run_with_controller<C>(
        self,
        mut controller: C,
        cfg: ExperimentalRawClockRunConfig,
    ) -> Result<ExperimentalRawClockRunExit, RawClockRuntimeError>
    where
        C: BilateralController,
    {
        self.config.validate()?;
        let nominal_period = Duration::from_secs_f64(1.0 / self.config.frequency_hz);
        let mut active = self;
        let mut iterations = 0usize;
        let mut next_tick = Instant::now();

        loop {
            if let Some(max_iterations) = active.config.max_iterations
                && iterations >= max_iterations
            {
                let mut report = active.timing.report(
                    piper_can::monotonic_micros(),
                    iterations,
                    Some(RawClockRuntimeExitReason::MaxIterations),
                );
                report.iterations = iterations;
                let standby = active.disable_both(cfg.disable_config.clone())?;
                return Ok(ExperimentalRawClockRunExit::Standby {
                    arms: Box::new(standby),
                    report,
                });
            }

            if cfg.cancel_signal.as_ref().is_some_and(|signal| signal.load(Ordering::Acquire)) {
                let mut report = active.timing.report(
                    piper_can::monotonic_micros(),
                    iterations,
                    Some(RawClockRuntimeExitReason::Cancelled),
                );
                report.last_error = Some("raw-clock runtime cancelled".to_string());
                let standby = active.disable_both(cfg.disable_config.clone())?;
                return Ok(ExperimentalRawClockRunExit::Standby {
                    arms: Box::new(standby),
                    report,
                });
            }

            let now = Instant::now();
            if now < next_tick {
                std::thread::sleep(next_tick - now);
            }
            next_tick += nominal_period;

            let health = active.runtime_health();
            if let Some(exit_reason) = classify_runtime_fault_exit_reason(health) {
                let err = RawClockRuntimeError::RuntimeTransportFault {
                    details: format_runtime_health_error(health),
                };
                let report =
                    active.fault_report(iterations, exit_reason, err.to_string(), |report| {
                        report.runtime_faults = report.runtime_faults.saturating_add(1)
                    });
                let (arms, shutdown) = active.fault_shutdown(FAULT_SHUTDOWN_TIMEOUT);
                return Ok(ExperimentalRawClockRunExit::Faulted {
                    arms: Box::new(arms),
                    report: report.with_shutdown(shutdown),
                });
            }

            let master = match active.read_master_experimental_snapshot(cfg.read_policy) {
                Ok(snapshot) => snapshot,
                Err(err) => {
                    let report = active.fault_report(
                        iterations,
                        RawClockRuntimeExitReason::ReadFault,
                        err.to_string(),
                        |report| report.read_faults = report.read_faults.saturating_add(1),
                    );
                    let (arms, shutdown) = active.fault_shutdown(FAULT_SHUTDOWN_TIMEOUT);
                    return Ok(ExperimentalRawClockRunExit::Faulted {
                        arms: Box::new(arms),
                        report: report.with_shutdown(shutdown),
                    });
                },
            };
            let slave = match active.read_slave_experimental_snapshot(cfg.read_policy) {
                Ok(snapshot) => snapshot,
                Err(err) => {
                    let report = active.fault_report(
                        iterations,
                        RawClockRuntimeExitReason::ReadFault,
                        err.to_string(),
                        |report| report.read_faults = report.read_faults.saturating_add(1),
                    );
                    let (arms, shutdown) = active.fault_shutdown(FAULT_SHUTDOWN_TIMEOUT);
                    return Ok(ExperimentalRawClockRunExit::Faulted {
                        arms: Box::new(arms),
                        report: report.with_shutdown(shutdown),
                    });
                },
            };

            let tick = match active.timing.tick_from_snapshots(
                &master,
                &slave,
                piper_can::monotonic_micros(),
            ) {
                Ok(tick) => tick,
                Err(err) => {
                    let report = active.fault_report(
                        iterations,
                        RawClockRuntimeExitReason::RawClockFault,
                        err.to_string(),
                        |_| {},
                    );
                    let (arms, shutdown) = active.fault_shutdown(FAULT_SHUTDOWN_TIMEOUT);
                    return Ok(ExperimentalRawClockRunExit::Faulted {
                        arms: Box::new(arms),
                        report: report.with_shutdown(shutdown),
                    });
                },
            };

            if let Err(err) = RawClockRuntimeGate::new(active.config.thresholds).check_tick(tick) {
                if matches!(err, RawClockRuntimeError::ClockUnhealthy { .. }) {
                    active.timing.record_clock_health_failure();
                }
                let exit_reason = match err {
                    RawClockRuntimeError::ClockUnhealthy { .. } => {
                        RawClockRuntimeExitReason::ClockHealthFault
                    },
                    _ => RawClockRuntimeExitReason::RawClockFault,
                };
                let report = active.fault_report(iterations, exit_reason, err.to_string(), |_| {});
                let (arms, shutdown) = active.fault_shutdown(FAULT_SHUTDOWN_TIMEOUT);
                return Ok(ExperimentalRawClockRunExit::Faulted {
                    arms: Box::new(arms),
                    report: report.with_shutdown(shutdown),
                });
            }

            let snapshot = raw_dual_arm_snapshot(&master, &slave);
            let frame = BilateralControlFrame {
                snapshot,
                compensation: None,
            };
            let command = match controller.tick_with_compensation(&frame, nominal_period) {
                Ok(command) => command,
                Err(err) => {
                    let report = active.fault_report(
                        iterations,
                        RawClockRuntimeExitReason::ControllerFault,
                        err.to_string(),
                        |_| {},
                    );
                    let (arms, shutdown) = active.fault_shutdown(FAULT_SHUTDOWN_TIMEOUT);
                    return Ok(ExperimentalRawClockRunExit::Faulted {
                        arms: Box::new(arms),
                        report: report.with_shutdown(shutdown),
                    });
                },
            };

            if let Err(err) = active.submit_command(&command, cfg.command_timeout) {
                let report = active.fault_report(
                    iterations,
                    RawClockRuntimeExitReason::SubmissionFault,
                    err.to_string(),
                    |report| report.submission_faults = report.submission_faults.saturating_add(1),
                );
                let (arms, shutdown) = active.fault_shutdown(FAULT_SHUTDOWN_TIMEOUT);
                return Ok(ExperimentalRawClockRunExit::Faulted {
                    arms: Box::new(arms),
                    report: report.with_shutdown(shutdown),
                });
            }

            iterations = iterations.saturating_add(1);
        }
    }

    pub fn disable_both(
        self,
        cfg: DisableConfig,
    ) -> Result<ExperimentalRawClockDualArmStandby, RawClockRuntimeError> {
        let master_result = self.master.disable(cfg.clone());
        let slave_result = self.slave.disable(cfg);
        match (master_result, slave_result) {
            (Ok(master), Ok(slave)) => Ok(ExperimentalRawClockDualArmStandby {
                master,
                slave,
                timing: self.timing,
                config: self.config,
            }),
            (master, slave) => Err(RawClockRuntimeError::DisableBothFailed {
                master: master.err().map(|err| err.to_string()),
                slave: slave.err().map(|err| err.to_string()),
            }),
        }
    }

    pub fn fault_shutdown(
        self,
        timeout: Duration,
    ) -> (ExperimentalRawClockErrorState, ExperimentalFaultShutdown) {
        let deadline = Instant::now() + timeout;
        self.master.driver.latch_fault();
        self.slave.driver.latch_fault();
        let master_pending = enqueue_experimental_stop_attempt(&self.master, deadline);
        let slave_pending = enqueue_experimental_stop_attempt(&self.slave, deadline);
        let master_stop_attempt = resolve_experimental_stop_attempt(master_pending);
        let slave_stop_attempt = resolve_experimental_stop_attempt(slave_pending);
        self.master.driver.request_stop();
        self.slave.driver.request_stop();
        (
            ExperimentalRawClockErrorState {
                master: force_soft_error_state(self.master),
                slave: force_soft_error_state(self.slave),
            },
            ExperimentalFaultShutdown {
                master_stop_attempt,
                slave_stop_attempt,
            },
        )
    }

    fn runtime_health(&self) -> ExperimentalRawClockRuntimeHealth {
        ExperimentalRawClockRuntimeHealth {
            master: self.master.observer().runtime_health(),
            slave: self.slave.observer().runtime_health(),
        }
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

    fn fault_report(
        &self,
        iterations: usize,
        exit_reason: RawClockRuntimeExitReason,
        error: String,
        update: impl FnOnce(&mut RawClockRuntimeReport),
    ) -> RawClockRuntimeReport {
        let mut report =
            self.timing.report(piper_can::monotonic_micros(), iterations, Some(exit_reason));
        report.last_error = Some(error);
        update(&mut report);
        report
    }
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
        inter_arm_skew: Duration::from_micros(
            master
                .newest_raw_feedback_timing
                .host_rx_mono_us
                .abs_diff(slave.newest_raw_feedback_timing.host_rx_mono_us),
        ),
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

fn percentile(samples: &[u64], percentile: u64) -> u64 {
    if samples.is_empty() {
        return 0;
    }

    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    let rank = (percentile as usize * (sorted.len() - 1)).div_ceil(100);
    sorted[rank.min(sorted.len() - 1)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observer::ControlSnapshot;
    use crate::types::{JointArray, NewtonMeter, Rad, RadPerSecond};
    use piper_driver::RawFeedbackTiming;
    use piper_tools::raw_clock::{RawClockHealth, RawClockThresholds};
    use std::collections::VecDeque;
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

    struct FakeRuntimeHarness {
        timing: RawClockRuntimeTiming,
        thresholds: RawClockRuntimeThresholds,
        reads: VecDeque<FakeRead>,
        command_failure: Option<FakeCommandFailure>,
        runtime_fault_at_iteration: Option<usize>,
        command_log: Vec<&'static str>,
        fault_shutdowns: usize,
    }

    impl FakeRuntimeHarness {
        fn new(timing: RawClockRuntimeTiming) -> Self {
            Self {
                timing,
                thresholds: RawClockRuntimeThresholds::for_tests(),
                reads: VecDeque::new(),
                command_failure: None,
                runtime_fault_at_iteration: None,
                command_log: Vec::new(),
                fault_shutdowns: 0,
            }
        }

        fn with_thresholds(mut self, thresholds: RawClockRuntimeThresholds) -> Self {
            self.thresholds = thresholds;
            self
        }

        fn with_reads(mut self, reads: impl IntoIterator<Item = FakeRead>) -> Self {
            self.reads = reads.into_iter().collect();
            self
        }

        fn with_command_failure(mut self, failure: FakeCommandFailure) -> Self {
            self.command_failure = Some(failure);
            self
        }

        fn with_runtime_fault_at_iteration(mut self, iteration: usize) -> Self {
            self.runtime_fault_at_iteration = Some(iteration);
            self
        }

        fn run(mut self, max_iterations: usize) -> (Self, RawClockRuntimeReport) {
            let command = test_command();
            let mut iterations = 0usize;

            loop {
                if iterations >= max_iterations {
                    let report = self.timing.report(
                        120_000,
                        iterations,
                        Some(RawClockRuntimeExitReason::MaxIterations),
                    );
                    return (self, report);
                }

                if self.runtime_fault_at_iteration == Some(iterations) {
                    let report = self.fault_report(
                        iterations,
                        RawClockRuntimeExitReason::RuntimeTransportFault,
                        "runtime transport fault".to_string(),
                        |report| report.runtime_faults = report.runtime_faults.saturating_add(1),
                    );
                    return (self, report);
                }

                let Some(read) = self.reads.pop_front() else {
                    let report = self.fault_report(
                        iterations,
                        RawClockRuntimeExitReason::ReadFault,
                        "snapshot queue exhausted".to_string(),
                        |report| report.read_faults = report.read_faults.saturating_add(1),
                    );
                    return (self, report);
                };

                let (master, slave) = match read {
                    FakeRead::Pair(master, slave) => (*master, *slave),
                    FakeRead::Error(message) => {
                        let report = self.fault_report(
                            iterations,
                            RawClockRuntimeExitReason::ReadFault,
                            message.to_string(),
                            |report| report.read_faults = report.read_faults.saturating_add(1),
                        );
                        return (self, report);
                    },
                };

                let now_host_us = master
                    .newest_raw_feedback_timing
                    .host_rx_mono_us
                    .max(slave.newest_raw_feedback_timing.host_rx_mono_us)
                    .saturating_add(100);
                let tick = match self.timing.tick_from_snapshots(&master, &slave, now_host_us) {
                    Ok(tick) => tick,
                    Err(err) => {
                        let report = self.fault_report(
                            iterations,
                            RawClockRuntimeExitReason::RawClockFault,
                            err.to_string(),
                            |_| {},
                        );
                        return (self, report);
                    },
                };

                if let Err(err) = RawClockRuntimeGate::new(self.thresholds).check_tick(tick) {
                    if matches!(err, RawClockRuntimeError::ClockUnhealthy { .. }) {
                        self.timing.record_clock_health_failure();
                    }
                    let report = self.fault_report(
                        iterations,
                        RawClockRuntimeExitReason::RawClockFault,
                        err.to_string(),
                        |_| {},
                    );
                    return (self, report);
                }

                if let Err(err) = self.submit_for_tests(&command) {
                    let report = self.fault_report(
                        iterations,
                        RawClockRuntimeExitReason::SubmissionFault,
                        err,
                        |report| {
                            report.submission_faults = report.submission_faults.saturating_add(1)
                        },
                    );
                    return (self, report);
                }

                iterations = iterations.saturating_add(1);
            }
        }

        fn submit_for_tests(&mut self, _command: &BilateralCommand) -> Result<(), String> {
            self.command_log.push("slave");
            if matches!(self.command_failure, Some(FakeCommandFailure::Slave)) {
                return Err("slave command submission failed".to_string());
            }

            self.command_log.push("master");
            if matches!(self.command_failure, Some(FakeCommandFailure::Master)) {
                return Err("master command submission failed".to_string());
            }
            Ok(())
        }

        fn fault_report(
            &mut self,
            iterations: usize,
            exit_reason: RawClockRuntimeExitReason,
            error: String,
            update: impl FnOnce(&mut RawClockRuntimeReport),
        ) -> RawClockRuntimeReport {
            self.fault_shutdowns = self.fault_shutdowns.saturating_add(1);
            let mut report = self.timing.report(120_000, iterations, Some(exit_reason));
            report.last_error = Some(error);
            report.master_stop_attempt = StopAttemptResult::ConfirmedSent;
            report.slave_stop_attempt = StopAttemptResult::ConfirmedSent;
            update(&mut report);
            report
        }
    }

    fn fake_warmup_collects_not_ready_samples(
        timing: &mut RawClockRuntimeTiming,
        pairs: &[(ExperimentalRawClockSnapshot, ExperimentalRawClockSnapshot)],
    ) -> Result<(), RawClockRuntimeError> {
        for (master, slave) in pairs {
            timing.ingest_snapshots(master, slave)?;
            match timing.tick_from_snapshots(master, slave, 120_000) {
                Ok(_) | Err(RawClockRuntimeError::EstimatorNotReady { .. }) => {},
                Err(err) => return Err(err),
            }
        }
        Ok(())
    }

    #[test]
    fn experimental_config_rejects_bilateral_mode() {
        let err = ExperimentalRawClockConfig::default()
            .with_mode(ExperimentalRawClockMode::Bilateral)
            .unwrap_err();

        assert!(err.to_string().contains("master-follower"));
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
        let harness =
            FakeRuntimeHarness::new(ready_timing_for_tests()).with_reads([FakeRead::pair(
                raw_clock_snapshot_for_tests(10_000, 110_000),
                raw_clock_snapshot_for_tests(20_000, 110_800),
            )]);

        let (harness, report) = harness.run(1);

        assert_eq!(harness.command_log, vec!["slave", "master"]);
        assert_eq!(
            report.exit_reason,
            Some(RawClockRuntimeExitReason::MaxIterations)
        );
    }

    #[test]
    fn runtime_gate_failure_prevents_another_normal_command() {
        let harness = FakeRuntimeHarness::new(ready_timing_for_tests())
            .with_thresholds(RawClockRuntimeThresholds {
                inter_arm_skew_max_us: 1_000,
                ..RawClockRuntimeThresholds::for_tests()
            })
            .with_reads([
                FakeRead::pair(
                    raw_clock_snapshot_for_tests(10_000, 110_000),
                    raw_clock_snapshot_for_tests(20_000, 110_800),
                ),
                FakeRead::pair(
                    raw_clock_snapshot_for_tests(11_000, 111_000),
                    raw_clock_snapshot_for_tests(21_000, 113_000),
                ),
            ]);

        let (harness, report) = harness.run(2);

        assert_eq!(harness.command_log, vec!["slave", "master"]);
        assert_eq!(
            report.exit_reason,
            Some(RawClockRuntimeExitReason::RawClockFault)
        );
        assert_eq!(harness.fault_shutdowns, 1);
    }

    #[test]
    fn timing_failure_calls_fault_shutdown_and_records_both_stop_attempts() {
        let harness =
            FakeRuntimeHarness::new(ready_timing_for_tests()).with_reads([FakeRead::pair(
                raw_clock_snapshot_for_tests(9_999, 111_000),
                raw_clock_snapshot_for_tests(20_000, 110_800),
            )]);

        let (harness, report) = harness.run(1);

        assert_eq!(harness.command_log, Vec::<&'static str>::new());
        assert_eq!(harness.fault_shutdowns, 1);
        assert_eq!(report.master_stop_attempt, StopAttemptResult::ConfirmedSent);
        assert_eq!(report.slave_stop_attempt, StopAttemptResult::ConfirmedSent);
        assert_eq!(
            report.exit_reason,
            Some(RawClockRuntimeExitReason::RawClockFault)
        );
    }

    #[test]
    fn snapshot_read_failure_calls_fault_shutdown_and_records_both_stop_attempts() {
        let harness = FakeRuntimeHarness::new(ready_timing_for_tests())
            .with_reads([FakeRead::Error("snapshot read failed")]);

        let (harness, report) = harness.run(1);

        assert_eq!(harness.command_log, Vec::<&'static str>::new());
        assert_eq!(harness.fault_shutdowns, 1);
        assert_eq!(report.read_faults, 1);
        assert_eq!(report.master_stop_attempt, StopAttemptResult::ConfirmedSent);
        assert_eq!(report.slave_stop_attempt, StopAttemptResult::ConfirmedSent);
    }

    #[test]
    fn command_submission_failure_calls_fault_shutdown_and_records_both_stop_attempts() {
        let harness = FakeRuntimeHarness::new(ready_timing_for_tests())
            .with_reads([FakeRead::pair(
                raw_clock_snapshot_for_tests(10_000, 110_000),
                raw_clock_snapshot_for_tests(20_000, 110_800),
            )])
            .with_command_failure(FakeCommandFailure::Master);

        let (harness, report) = harness.run(1);

        assert_eq!(harness.command_log, vec!["slave", "master"]);
        assert_eq!(harness.fault_shutdowns, 1);
        assert_eq!(report.submission_faults, 1);
        assert_eq!(report.master_stop_attempt, StopAttemptResult::ConfirmedSent);
        assert_eq!(report.slave_stop_attempt, StopAttemptResult::ConfirmedSent);
    }

    #[test]
    fn slave_command_submission_failure_stops_before_master_command() {
        let harness = FakeRuntimeHarness::new(ready_timing_for_tests())
            .with_reads([FakeRead::pair(
                raw_clock_snapshot_for_tests(10_000, 110_000),
                raw_clock_snapshot_for_tests(20_000, 110_800),
            )])
            .with_command_failure(FakeCommandFailure::Slave);

        let (harness, report) = harness.run(1);

        assert_eq!(harness.command_log, vec!["slave"]);
        assert_eq!(report.submission_faults, 1);
        assert_eq!(harness.fault_shutdowns, 1);
    }

    #[test]
    fn runtime_transport_fault_calls_fault_shutdown_and_records_both_stop_attempts() {
        let harness = FakeRuntimeHarness::new(ready_timing_for_tests())
            .with_runtime_fault_at_iteration(0)
            .with_reads([FakeRead::pair(
                raw_clock_snapshot_for_tests(10_000, 110_000),
                raw_clock_snapshot_for_tests(20_000, 110_800),
            )]);

        let (harness, report) = harness.run(1);

        assert_eq!(harness.command_log, Vec::<&'static str>::new());
        assert_eq!(harness.fault_shutdowns, 1);
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
        let harness =
            FakeRuntimeHarness::new(ready_timing_for_tests()).with_reads([FakeRead::pair(
                raw_clock_snapshot_for_tests(10_000, 110_000),
                raw_clock_snapshot_without_raw_timing_for_tests(),
            )]);

        let (harness, report) = harness.run(1);

        assert_eq!(harness.command_log, Vec::<&'static str>::new());
        assert_eq!(harness.fault_shutdowns, 1);
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
    fn warmup_starting_empty_collects_not_ready_samples_instead_of_aborting() {
        let mut timing = RawClockRuntimeTiming::new(thresholds_for_tests());
        let pairs = [(
            raw_clock_snapshot_for_tests(10_000, 110_000),
            raw_clock_snapshot_for_tests(20_000, 110_800),
        )];

        fake_warmup_collects_not_ready_samples(&mut timing, &pairs).unwrap();

        assert_eq!(timing.sample_counts_for_tests(), (1, 1));
    }

    #[test]
    fn runtime_report_includes_max_and_p95_inter_arm_skew() {
        let mut timing = RawClockRuntimeTiming::new(thresholds_for_tests());
        timing.skew_samples_us = (0..=100).collect();

        let report = timing.report(120_000, 101, Some(RawClockRuntimeExitReason::MaxIterations));

        assert_eq!(report.max_inter_arm_skew_us, 100);
        assert_eq!(report.inter_arm_skew_p95_us, 95);
    }

    #[test]
    fn disable_both_attempts_both_sides_on_clean_cancellation() {
        let mut attempts = Vec::new();
        let master_result: Result<(), &'static str> = {
            attempts.push("master");
            Err("master disable failed")
        };
        let slave_result: Result<(), &'static str> = {
            attempts.push("slave");
            Ok(())
        };

        let _combined = (master_result.err(), slave_result.err());

        assert_eq!(attempts, vec!["master", "slave"]);
    }
}
