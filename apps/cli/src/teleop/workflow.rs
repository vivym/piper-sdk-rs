use anyhow::{Context, Result, bail};
use piper_client::dual_arm::{
    BilateralExitReason, BilateralLoopConfig, BilateralRunReport, DualArmActiveMit, DualArmBuilder,
    DualArmCalibration, DualArmLoopExit, DualArmReadPolicy, DualArmSnapshot, DualArmStandby,
    JointMirrorMap, StopAttemptResult,
};
use piper_client::dual_arm_raw_clock::{
    ExperimentalRawClockConfig, ExperimentalRawClockDualArmActive,
    ExperimentalRawClockDualArmStandby, ExperimentalRawClockMode, ExperimentalRawClockRunConfig,
    ExperimentalRawClockRunExit as PiperRawClockRunExit, RawClockRuntimeExitReason,
    RawClockRuntimeReport, RawClockRuntimeThresholds,
};
use piper_client::observer::{ControlSnapshotFull, Observer};
use piper_client::state::{DisableConfig, MitModeConfig, Piper, SoftRealtime, Standby};
use piper_client::types::{JointArray, NewtonMeter, Rad, RadPerSecond};
use piper_client::{MotionConnectedPiper, MotionConnectedState, PiperBuilder};
use piper_tools::raw_clock::{RawClockHealth, RawClockThresholds};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, OnceLock,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::commands::teleop::TeleopDualArmArgs;
use crate::teleop::calibration::{CalibrationFile, check_posture_compatibility};
use crate::teleop::config::{
    ResolvedTeleopConfig, TeleopConfigFile, TeleopMode, TeleopProfile, TeleopRawClockSettings,
};
use crate::teleop::controller::{
    RuntimeTeleopController, RuntimeTeleopSettings, RuntimeTeleopSettingsHandle,
};
use crate::teleop::report::{
    ReportCalibration, ReportTiming, TeleopExitStatus, TeleopJsonReport, TeleopReportInput,
    classify_exit, print_human_report,
};
use crate::teleop::target::{
    ConcreteTeleopTarget, RoleTargets, TeleopPlatform, resolve_role_targets,
};

const CALIBRATED_HW_RAW_TIMING_SOURCE: &str = "calibrated_hw_raw";

pub trait TeleopBackend {
    fn connect(&mut self, targets: &RoleTargets, baud_rate: u32) -> Result<()>;
    fn runtime_health_ok(&self) -> Result<()>;
    fn standby_snapshot(&self, policy: DualArmReadPolicy) -> Result<DualArmSnapshot>;
    fn capture_calibration(&self, map: JointMirrorMap) -> Result<DualArmCalibration>;

    /// Enables both arms in MIT mode and transfers backend ownership into active state.
    ///
    /// If this returns `Err`, the workflow must not call `disable_active`: backend implementations
    /// are responsible for cleaning up any partially-dispatched enable attempt before returning.
    fn enable_mit(&mut self, master: MitModeConfig, slave: MitModeConfig) -> Result<EnableOutcome>;

    fn disable_active(&mut self) -> Result<()>;
    fn active_snapshot(&self, policy: DualArmReadPolicy) -> Result<DualArmSnapshot>;

    /// Runs the bilateral loop until it reaches a disabled standby or faulted/no-active state.
    ///
    /// Report and console I/O happen after this method returns, so implementations must not return
    /// while torque-producing active control is still enabled.
    fn run_loop(
        &mut self,
        controller: RuntimeTeleopController,
        cfg: BilateralLoopConfig,
    ) -> Result<TeleopLoopExit>;
}

pub trait ExperimentalRawClockTeleopBackend {
    fn connect_soft(&mut self, targets: &RoleTargets, baud_rate: u32) -> Result<()>;

    fn warmup_raw_clock(
        &mut self,
        settings: &TeleopRawClockSettings,
        frequency_hz: f64,
        max_iterations: Option<usize>,
        cancel_signal: Arc<AtomicBool>,
    ) -> Result<RawClockWarmupSummary>;

    fn capture_calibration(&self, map: JointMirrorMap) -> Result<DualArmCalibration>;

    fn standby_snapshot(&self, policy: DualArmReadPolicy) -> Result<DualArmSnapshot>;

    /// Enables both arms in SoftRealtime MIT passthrough mode.
    ///
    /// If this returns `Err`, implementations are responsible for cleaning up any partially
    /// dispatched enable attempt before returning.
    fn enable_mit_passthrough(&mut self, master: MitModeConfig, slave: MitModeConfig)
    -> Result<()>;

    fn active_snapshot(&self, policy: DualArmReadPolicy) -> Result<DualArmSnapshot>;

    fn disable_active_passthrough(&mut self) -> Result<()>;

    fn run_master_follower_raw_clock(
        &mut self,
        settings: RuntimeTeleopSettingsHandle,
        raw_clock: TeleopRawClockSettings,
        cancel_signal: Arc<AtomicBool>,
    ) -> Result<ExperimentalRawClockRunExit>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum EnableOutcome {
    Active,
}

#[derive(Debug, Clone)]
pub struct TeleopLoopExit {
    pub faulted: bool,
    pub report: BilateralRunReport,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct RawClockWarmupSummary {
    pub master_clock_drift_ppm: Option<f64>,
    pub slave_clock_drift_ppm: Option<f64>,
    pub master_residual_p95_us: Option<u64>,
    pub slave_residual_p95_us: Option<u64>,
    pub max_estimated_inter_arm_skew_us: Option<u64>,
    pub estimated_inter_arm_skew_p95_us: Option<u64>,
    pub clock_health_failures: u64,
}

#[derive(Debug, Clone)]
pub struct ExperimentalRawClockRunExit {
    pub faulted: bool,
    pub report: RawClockRuntimeReport,
}

pub trait TeleopIo {
    fn cancel_signal(&self) -> Arc<AtomicBool>;
    fn confirm_start(&mut self, summary: &StartupSummary) -> Result<bool>;
    fn cancel_requested(&self) -> bool;
    fn start_console(
        &mut self,
        settings: RuntimeTeleopSettingsHandle,
        started_at: Instant,
    ) -> Result<Option<JoinHandle<Result<()>>>>;
    fn write_json_report(&mut self, path: &Path, report: &TeleopJsonReport) -> Result<()>;
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct StartupSummary {
    pub targets: RoleTargets,
    pub mode: TeleopMode,
    pub profile: TeleopProfile,
    pub frequency_hz: f64,
    pub track_kp: f64,
    pub track_kd: f64,
    pub master_damping: f64,
    pub reflection_gain: f64,
    pub calibration_source: String,
    pub calibration_path: Option<PathBuf>,
    pub gripper_mirror: bool,
    pub experimental: bool,
    pub strict_realtime: bool,
    pub timing_source: Option<String>,
    pub report_path: Option<PathBuf>,
    pub yes: bool,
}

impl StartupSummary {
    #[cfg(test)]
    fn experimental_raw_clock_for_tests(targets: RoleTargets) -> Self {
        Self {
            targets,
            mode: TeleopMode::MasterFollower,
            profile: TeleopProfile::Debug,
            frequency_hz: 200.0,
            track_kp: 8.0,
            track_kd: 1.0,
            master_damping: 0.4,
            reflection_gain: 0.25,
            calibration_source: "captured".to_string(),
            calibration_path: None,
            gripper_mirror: false,
            experimental: true,
            strict_realtime: false,
            timing_source: Some(CALIBRATED_HW_RAW_TIMING_SOURCE.to_string()),
            report_path: None,
            yes: true,
        }
    }
}

#[allow(dead_code)]
pub fn run_workflow<B>(
    args: TeleopDualArmArgs,
    backend: &mut B,
    io: &mut dyn TeleopIo,
) -> Result<TeleopExitStatus>
where
    B: TeleopBackend + ExperimentalRawClockTeleopBackend,
{
    run_workflow_on_platform(args, backend, io, TeleopPlatform::current())
}

pub(crate) fn run_workflow_on_platform<B>(
    args: TeleopDualArmArgs,
    backend: &mut B,
    io: &mut dyn TeleopIo,
    platform: TeleopPlatform,
) -> Result<TeleopExitStatus>
where
    B: TeleopBackend + ExperimentalRawClockTeleopBackend,
{
    let config_file = args.config.as_deref().map(TeleopConfigFile::load).transpose()?;
    let resolved = ResolvedTeleopConfig::resolve(args.clone(), config_file.clone())?;

    if let Some(path) = &args.save_calibration
        && path.exists()
    {
        bail!(
            "save calibration path already exists: {}; refusing to overwrite",
            path.display()
        );
    }

    let targets = resolve_role_targets(&args, config_file.as_ref(), platform)?;
    let cancel_signal = io.cancel_signal();

    if resolved.raw_clock.experimental_calibrated_raw {
        return run_experimental_raw_clock_workflow(
            args,
            backend,
            io,
            platform,
            resolved,
            targets,
            cancel_signal,
        );
    }

    let loaded_calibration =
        resolved.calibration.file.as_deref().map(load_calibration).transpose()?;

    backend.connect(&targets, args.baud_rate)?;
    backend.runtime_health_ok().context("runtime health check failed")?;

    let (calibration, report_calibration) = match loaded_calibration {
        Some((file, calibration)) => (
            calibration,
            ReportCalibration {
                source: "file".to_string(),
                path: resolved.calibration.file.as_ref().map(|path| path.display().to_string()),
                created_at_unix_ms: Some(file.created_at_unix_ms),
                max_error_rad: resolved.calibration.max_error_rad,
            },
        ),
        None => {
            let calibration =
                TeleopBackend::capture_calibration(backend, JointMirrorMap::left_right_mirror())?;
            let created_at_unix_ms = current_unix_ms();
            if let Some(path) = &args.save_calibration {
                // The Task 8 startup order saves captured calibration before operator confirmation.
                CalibrationFile::from_calibration(&calibration, None, created_at_unix_ms)
                    .save_new(path)?;
            }
            (
                calibration,
                ReportCalibration {
                    source: "captured".to_string(),
                    path: args.save_calibration.as_ref().map(|path| path.display().to_string()),
                    created_at_unix_ms: Some(created_at_unix_ms),
                    max_error_rad: resolved.calibration.max_error_rad,
                },
            )
        },
    };

    let summary = StartupSummary {
        targets: targets.clone(),
        mode: resolved.control.mode,
        profile: resolved.safety.profile,
        frequency_hz: resolved.control.frequency_hz,
        track_kp: resolved.control.track_kp,
        track_kd: resolved.control.track_kd,
        master_damping: resolved.control.master_damping,
        reflection_gain: resolved.control.reflection_gain,
        calibration_source: report_calibration.source.clone(),
        calibration_path: resolved
            .calibration
            .file
            .clone()
            .or_else(|| args.save_calibration.clone()),
        gripper_mirror: resolved.safety.gripper_mirror,
        experimental: false,
        strict_realtime: true,
        timing_source: None,
        report_path: args.report_json.clone(),
        yes: args.yes,
    };
    if !io.confirm_start(&summary)? {
        if io.cancel_requested() || cancel_signal.load(Ordering::SeqCst) {
            return Ok(TeleopExitStatus::Success);
        }
        bail!("operator confirmation declined");
    }

    let standby_snapshot = TeleopBackend::standby_snapshot(backend, DualArmReadPolicy::default())?;
    check_snapshot_posture(
        &calibration,
        &standby_snapshot,
        resolved.calibration.max_error_rad,
    )
    .context("pre-enable posture compatibility check failed")?;

    if io.cancel_requested() || cancel_signal.load(Ordering::SeqCst) {
        return Ok(TeleopExitStatus::Success);
    }

    match backend.enable_mit(MitModeConfig::default(), MitModeConfig::default())? {
        EnableOutcome::Active => {},
    }

    if io.cancel_requested() || cancel_signal.load(Ordering::SeqCst) {
        backend
            .disable_active()
            .context("failed to disable active teleop after cancellation")?;
        return Ok(TeleopExitStatus::Success);
    }

    let active_snapshot =
        match TeleopBackend::active_snapshot(backend, DualArmReadPolicy::default()) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                return disable_active_after_error(backend, "active snapshot read failed", error);
            },
        };
    if let Err(error) = check_snapshot_posture(
        &calibration,
        &active_snapshot,
        resolved.calibration.max_error_rad,
    ) {
        backend
            .disable_active()
            .context("failed to disable active teleop after posture mismatch")?;
        return Err(error).context("post-enable posture compatibility check failed");
    }

    let settings = match (|| {
        RuntimeTeleopSettings::production(calibration.clone())
            .with_mode(resolved.control.mode)?
            .with_track_gains(resolved.control.track_kp, resolved.control.track_kd)?
            .with_master_damping(resolved.control.master_damping)?
            .with_reflection_gain(resolved.control.reflection_gain)
    })() {
        Ok(settings) => settings,
        Err(error) => {
            return disable_active_after_error(
                backend,
                "failed to build runtime teleop settings",
                error,
            );
        },
    };
    let settings_handle = match RuntimeTeleopSettingsHandle::new(settings) {
        Ok(handle) => handle,
        Err(error) => {
            return disable_active_after_error(
                backend,
                "failed to initialize runtime teleop settings",
                error,
            );
        },
    };
    let initial_mode = settings_handle.snapshot().mode;
    let started_at = Instant::now();
    let console_handle = match io.start_console(settings_handle.clone(), started_at) {
        Ok(handle) => handle,
        Err(error) => {
            return disable_active_after_error(backend, "failed to start teleop console", error);
        },
    };

    let loop_exit = backend.run_loop(
        RuntimeTeleopController::new(settings_handle.clone()),
        resolved.loop_config(cancel_signal),
    )?;

    let final_mode = settings_handle.snapshot().mode;
    let report = TeleopJsonReport::from_run(TeleopReportInput {
        platform,
        targets,
        profile: resolved.safety.profile,
        initial_mode,
        final_mode,
        control: resolved.control.clone(),
        safety: resolved.safety.clone(),
        calibration: report_calibration,
        timing: None,
        faulted: loop_exit.faulted,
        report: &loop_exit.report,
    });
    print_human_report(&report, started_at.elapsed());

    if let Some(path) = &args.report_json {
        io.write_json_report(path, &report)?;
    }
    inspect_finished_console(console_handle)?;

    Ok(classify_exit(loop_exit.faulted, &loop_exit.report))
}

fn run_experimental_raw_clock_workflow<B>(
    args: TeleopDualArmArgs,
    backend: &mut B,
    io: &mut dyn TeleopIo,
    platform: TeleopPlatform,
    resolved: ResolvedTeleopConfig,
    targets: RoleTargets,
    cancel_signal: Arc<AtomicBool>,
) -> Result<TeleopExitStatus>
where
    B: ExperimentalRawClockTeleopBackend,
{
    if platform != TeleopPlatform::Linux {
        bail!("experimental calibrated raw clock requires Linux SocketCAN targets");
    }
    targets.ensure_experimental_raw_clock_supported(platform)?;

    backend.connect_soft(&targets, args.baud_rate)?;
    let warmup = match backend.warmup_raw_clock(
        &resolved.raw_clock,
        resolved.control.frequency_hz,
        resolved.max_iterations,
        cancel_signal.clone(),
    ) {
        Ok(summary) => summary,
        Err(error) if io.cancel_requested() || cancel_signal.load(Ordering::SeqCst) => {
            return Ok(TeleopExitStatus::Success);
        },
        Err(error) => return Err(error).context("raw-clock warmup failed"),
    };

    if io.cancel_requested() || cancel_signal.load(Ordering::SeqCst) {
        return Ok(TeleopExitStatus::Success);
    }

    let loaded_calibration =
        resolved.calibration.file.as_deref().map(load_calibration).transpose()?;
    let (calibration, report_calibration) = match loaded_calibration {
        Some((file, calibration)) => (
            calibration,
            ReportCalibration {
                source: "file".to_string(),
                path: resolved.calibration.file.as_ref().map(|path| path.display().to_string()),
                created_at_unix_ms: Some(file.created_at_unix_ms),
                max_error_rad: resolved.calibration.max_error_rad,
            },
        ),
        None => {
            let calibration = ExperimentalRawClockTeleopBackend::capture_calibration(
                backend,
                JointMirrorMap::left_right_mirror(),
            )?;
            let created_at_unix_ms = current_unix_ms();
            if let Some(path) = &args.save_calibration {
                CalibrationFile::from_calibration(&calibration, None, created_at_unix_ms)
                    .save_new(path)?;
            }
            (
                calibration,
                ReportCalibration {
                    source: "captured".to_string(),
                    path: args.save_calibration.as_ref().map(|path| path.display().to_string()),
                    created_at_unix_ms: Some(created_at_unix_ms),
                    max_error_rad: resolved.calibration.max_error_rad,
                },
            )
        },
    };

    let summary = StartupSummary {
        targets: targets.clone(),
        mode: resolved.control.mode,
        profile: resolved.safety.profile,
        frequency_hz: resolved.control.frequency_hz,
        track_kp: resolved.control.track_kp,
        track_kd: resolved.control.track_kd,
        master_damping: resolved.control.master_damping,
        reflection_gain: resolved.control.reflection_gain,
        calibration_source: report_calibration.source.clone(),
        calibration_path: resolved
            .calibration
            .file
            .clone()
            .or_else(|| args.save_calibration.clone()),
        gripper_mirror: resolved.safety.gripper_mirror,
        experimental: true,
        strict_realtime: false,
        timing_source: Some(CALIBRATED_HW_RAW_TIMING_SOURCE.to_string()),
        report_path: args.report_json.clone(),
        yes: args.yes,
    };
    if !io.confirm_start(&summary)? {
        if io.cancel_requested() || cancel_signal.load(Ordering::SeqCst) {
            return Ok(TeleopExitStatus::Success);
        }
        bail!("operator confirmation declined");
    }

    let standby_snapshot =
        ExperimentalRawClockTeleopBackend::standby_snapshot(backend, DualArmReadPolicy::default())?;
    check_snapshot_posture(
        &calibration,
        &standby_snapshot,
        resolved.calibration.max_error_rad,
    )
    .context("pre-enable posture compatibility check failed")?;

    if io.cancel_requested() || cancel_signal.load(Ordering::SeqCst) {
        return Ok(TeleopExitStatus::Success);
    }

    let settings = RuntimeTeleopSettings::production(calibration.clone())
        .with_mode(resolved.control.mode)?
        .with_track_gains(resolved.control.track_kp, resolved.control.track_kd)?
        .with_master_damping(resolved.control.master_damping)?
        .with_reflection_gain(resolved.control.reflection_gain)?;
    let settings_handle = RuntimeTeleopSettingsHandle::new(settings)?;
    let initial_mode = settings_handle.snapshot().mode;

    backend.enable_mit_passthrough(MitModeConfig::default(), MitModeConfig::default())?;

    if io.cancel_requested() || cancel_signal.load(Ordering::SeqCst) {
        backend
            .disable_active_passthrough()
            .context("failed to disable experimental raw-clock teleop after cancellation")?;
        return Ok(TeleopExitStatus::Success);
    }

    let active_snapshot = match ExperimentalRawClockTeleopBackend::active_snapshot(
        backend,
        DualArmReadPolicy::default(),
    ) {
        Ok(snapshot) => snapshot,
        Err(error) => {
            return disable_experimental_after_error(backend, "active snapshot read failed", error);
        },
    };
    if let Err(error) = check_snapshot_posture(
        &calibration,
        &active_snapshot,
        resolved.calibration.max_error_rad,
    ) {
        backend
            .disable_active_passthrough()
            .context("failed to disable experimental raw-clock teleop after posture mismatch")?;
        return Err(error).context("post-enable posture compatibility check failed");
    }

    let started_at = Instant::now();
    let console_handle = match io.start_console(settings_handle.clone(), started_at) {
        Ok(handle) => handle,
        Err(error) => {
            return disable_experimental_after_error(
                backend,
                "failed to start teleop console",
                error,
            );
        },
    };

    let loop_exit = backend.run_master_follower_raw_clock(
        settings_handle.clone(),
        resolved.raw_clock.clone(),
        cancel_signal,
    )?;

    let final_mode = settings_handle.snapshot().mode;
    let timing = report_timing_from_raw_clock(&warmup, &loop_exit.report);
    let faulted = loop_exit.faulted || timing.clock_health_failures > 0;
    let bilateral_report = bilateral_report_from_raw_clock(&loop_exit.report);
    let report = TeleopJsonReport::from_run(TeleopReportInput {
        platform,
        targets,
        profile: resolved.safety.profile,
        initial_mode,
        final_mode,
        control: resolved.control.clone(),
        safety: resolved.safety.clone(),
        calibration: report_calibration,
        timing: Some(timing.clone()),
        faulted,
        report: &bilateral_report,
    });
    print_human_report(&report, started_at.elapsed());

    if let Some(path) = &args.report_json {
        io.write_json_report(path, &report)?;
    }
    inspect_finished_console(console_handle)?;

    let status = classify_experimental_raw_clock_exit(faulted, &loop_exit.report, &timing);
    if status == TeleopExitStatus::Failure {
        let message = loop_exit
            .report
            .last_error
            .clone()
            .unwrap_or_else(|| "experimental raw-clock timing health failure".to_string());
        bail!("{message}");
    }
    Ok(status)
}

fn classify_experimental_raw_clock_exit(
    faulted: bool,
    report: &RawClockRuntimeReport,
    timing: &ReportTiming,
) -> TeleopExitStatus {
    if !faulted
        && timing.clock_health_failures == 0
        && matches!(
            report.exit_reason,
            Some(RawClockRuntimeExitReason::Cancelled | RawClockRuntimeExitReason::MaxIterations)
        )
    {
        TeleopExitStatus::Success
    } else {
        TeleopExitStatus::Failure
    }
}

fn report_timing_from_raw_clock(
    warmup: &RawClockWarmupSummary,
    report: &RawClockRuntimeReport,
) -> ReportTiming {
    ReportTiming {
        timing_source: CALIBRATED_HW_RAW_TIMING_SOURCE.to_string(),
        experimental: true,
        strict_realtime: false,
        master_clock_drift_ppm: health_drift_ppm(&report.master).or(warmup.master_clock_drift_ppm),
        slave_clock_drift_ppm: health_drift_ppm(&report.slave).or(warmup.slave_clock_drift_ppm),
        master_residual_p95_us: health_residual_p95_us(&report.master)
            .or(warmup.master_residual_p95_us),
        slave_residual_p95_us: health_residual_p95_us(&report.slave)
            .or(warmup.slave_residual_p95_us),
        max_estimated_inter_arm_skew_us: max_optional_u64(
            runtime_max_inter_arm_skew_us(report),
            warmup.max_estimated_inter_arm_skew_us,
        ),
        estimated_inter_arm_skew_p95_us: runtime_inter_arm_skew_p95_us(report)
            .or(warmup.estimated_inter_arm_skew_p95_us),
        clock_health_failures: report
            .clock_health_failures
            .saturating_add(warmup.clock_health_failures),
    }
}

fn bilateral_report_from_raw_clock(report: &RawClockRuntimeReport) -> BilateralRunReport {
    BilateralRunReport {
        iterations: report.iterations,
        read_faults: report.read_faults,
        submission_faults: report.submission_faults,
        max_inter_arm_skew: Duration::from_micros(report.max_inter_arm_skew_us),
        exit_reason: report.exit_reason.map(map_raw_clock_exit_reason),
        left_stop_attempt: report.master_stop_attempt,
        right_stop_attempt: report.slave_stop_attempt,
        last_error: report.last_error.clone(),
        ..BilateralRunReport::default()
    }
}

fn map_raw_clock_exit_reason(reason: RawClockRuntimeExitReason) -> BilateralExitReason {
    match reason {
        RawClockRuntimeExitReason::MaxIterations => BilateralExitReason::MaxIterations,
        RawClockRuntimeExitReason::Cancelled => BilateralExitReason::Cancelled,
        RawClockRuntimeExitReason::ReadFault => BilateralExitReason::ReadFault,
        RawClockRuntimeExitReason::ControllerFault => BilateralExitReason::ControllerFault,
        RawClockRuntimeExitReason::SubmissionFault => BilateralExitReason::SubmissionFault,
        RawClockRuntimeExitReason::RuntimeManualFault => BilateralExitReason::RuntimeManualFault,
        RawClockRuntimeExitReason::RuntimeTransportFault
        | RawClockRuntimeExitReason::RawClockFault
        | RawClockRuntimeExitReason::ClockHealthFault => BilateralExitReason::RuntimeTransportFault,
    }
}

fn health_drift_ppm(health: &RawClockHealth) -> Option<f64> {
    (health.sample_count > 0 && health.drift_ppm.is_finite()).then_some(health.drift_ppm)
}

fn health_residual_p95_us(health: &RawClockHealth) -> Option<u64> {
    (health.sample_count > 0).then_some(health.residual_p95_us)
}

fn runtime_max_inter_arm_skew_us(report: &RawClockRuntimeReport) -> Option<u64> {
    runtime_has_skew_evidence(report).then_some(report.max_inter_arm_skew_us)
}

fn runtime_inter_arm_skew_p95_us(report: &RawClockRuntimeReport) -> Option<u64> {
    runtime_has_skew_evidence(report).then_some(report.inter_arm_skew_p95_us)
}

fn runtime_has_skew_evidence(report: &RawClockRuntimeReport) -> bool {
    report.iterations > 0 || report.max_inter_arm_skew_us > 0 || report.inter_arm_skew_p95_us > 0
}

fn max_optional_u64(left: Option<u64>, right: Option<u64>) -> Option<u64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.max(right)),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

fn experimental_raw_clock_config_from_settings(
    raw_clock: &TeleopRawClockSettings,
    frequency_hz: f64,
    max_iterations: Option<usize>,
) -> ExperimentalRawClockConfig {
    let estimator_thresholds = RawClockThresholds {
        warmup_samples: (raw_clock.warmup_secs * 400).max(4) as usize,
        warmup_window_us: raw_clock.warmup_secs * 1_000_000,
        residual_p95_us: raw_clock.residual_p95_us,
        residual_max_us: raw_clock.residual_max_us,
        drift_abs_ppm: raw_clock.drift_abs_ppm,
        sample_gap_max_us: raw_clock.sample_gap_max_ms * 1_000,
        last_sample_age_us: raw_clock.last_sample_age_ms * 1_000,
    };
    let thresholds = RawClockRuntimeThresholds {
        inter_arm_skew_max_us: raw_clock.inter_arm_skew_max_us,
        last_sample_age_us: raw_clock.last_sample_age_ms * 1_000,
    };

    ExperimentalRawClockConfig {
        mode: ExperimentalRawClockMode::MasterFollower,
        frequency_hz,
        max_iterations,
        thresholds,
        estimator_thresholds,
    }
}

fn disable_experimental_after_error<B, T>(
    backend: &mut B,
    context: &'static str,
    error: anyhow::Error,
) -> Result<T>
where
    B: ExperimentalRawClockTeleopBackend,
{
    if let Err(disable_error) = backend.disable_active_passthrough() {
        return Err(error.context(format!(
            "{context}; additionally failed to disable experimental raw-clock teleop: {disable_error:#}"
        )));
    }
    Err(error.context(context))
}

fn disable_active_after_error<B, T>(
    backend: &mut B,
    context: &'static str,
    error: anyhow::Error,
) -> Result<T>
where
    B: TeleopBackend,
{
    if let Err(disable_error) = backend.disable_active() {
        return Err(error.context(format!(
            "{context}; additionally failed to disable active teleop: {disable_error:#}"
        )));
    }
    Err(error.context(context))
}

fn load_calibration(path: &Path) -> Result<(CalibrationFile, DualArmCalibration)> {
    let file = CalibrationFile::load(path)?;
    let calibration = file.to_calibration()?;
    Ok((file, calibration))
}

fn check_snapshot_posture(
    calibration: &DualArmCalibration,
    snapshot: &DualArmSnapshot,
    max_error_rad: f64,
) -> Result<()> {
    check_posture_compatibility(
        calibration,
        snapshot.left.state.position,
        snapshot.right.state.position,
        max_error_rad,
    )
    .context("posture is incompatible with calibration")
}

fn current_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}

fn inspect_finished_console(handle: Option<JoinHandle<Result<()>>>) -> Result<()> {
    let Some(handle) = handle else {
        return Ok(());
    };
    if !handle.is_finished() {
        return Ok(());
    }

    match handle.join() {
        Ok(result) => result.context("teleop console thread failed"),
        Err(_) => bail!("teleop console thread panicked"),
    }
}

#[derive(Default)]
pub struct RealTeleopBackend {
    standby: Option<DualArmStandby>,
    active: Option<DualArmActiveMit>,
    experimental_pending: Option<ExperimentalSoftRealtimeStandbyArms>,
    experimental_standby: Option<ExperimentalRawClockDualArmStandby>,
    experimental_active: Option<ExperimentalRawClockDualArmActive>,
    experimental_observers: Option<ExperimentalSoftRealtimeObservers>,
}

struct ExperimentalSoftRealtimeStandbyArms {
    master: Piper<Standby, SoftRealtime>,
    slave: Piper<Standby, SoftRealtime>,
}

#[derive(Clone)]
struct ExperimentalSoftRealtimeObservers {
    master: Observer<SoftRealtime>,
    slave: Observer<SoftRealtime>,
}

impl TeleopBackend for RealTeleopBackend {
    fn connect(&mut self, targets: &RoleTargets, baud_rate: u32) -> Result<()> {
        let master_builder = PiperBuilder::new()
            .target(targets.master.to_connection_target())
            .baud_rate(baud_rate);
        let slave_builder = PiperBuilder::new()
            .target(targets.slave.to_connection_target())
            .baud_rate(baud_rate);

        self.standby = Some(DualArmBuilder::new(master_builder, slave_builder).build()?);
        self.active = None;
        self.experimental_pending = None;
        self.experimental_standby = None;
        self.experimental_active = None;
        self.experimental_observers = None;
        Ok(())
    }

    fn runtime_health_ok(&self) -> Result<()> {
        let standby = self.standby.as_ref().context("dual-arm backend is not connected")?;
        let health = standby.observer().runtime_health();
        if health.any_unhealthy() {
            bail!("dual-arm runtime is unhealthy before teleop start: {health:?}");
        }
        Ok(())
    }

    fn standby_snapshot(&self, policy: DualArmReadPolicy) -> Result<DualArmSnapshot> {
        let standby = self.standby.as_ref().context("dual-arm backend is not in standby")?;
        Ok(standby.observer().snapshot(policy)?)
    }

    fn capture_calibration(&self, map: JointMirrorMap) -> Result<DualArmCalibration> {
        let standby = self.standby.as_ref().context("dual-arm backend is not in standby")?;
        Ok(standby.capture_calibration(map)?)
    }

    fn enable_mit(&mut self, master: MitModeConfig, slave: MitModeConfig) -> Result<EnableOutcome> {
        let standby = self.standby.take().context("dual-arm backend is not in standby")?;
        let active = standby.enable_mit(master, slave)?;
        self.active = Some(active);
        Ok(EnableOutcome::Active)
    }

    fn disable_active(&mut self) -> Result<()> {
        let active = self.active.take().context("dual-arm backend is not active")?;
        let standby = active.disable_both(DisableConfig::default())?;
        self.standby = Some(standby);
        Ok(())
    }

    fn active_snapshot(&self, policy: DualArmReadPolicy) -> Result<DualArmSnapshot> {
        let active = self.active.as_ref().context("dual-arm backend is not active")?;
        Ok(active.observer().snapshot(policy)?)
    }

    fn run_loop(
        &mut self,
        controller: RuntimeTeleopController,
        cfg: BilateralLoopConfig,
    ) -> Result<TeleopLoopExit> {
        let active = self.active.take().context("dual-arm backend is not active")?;

        match active.run_bilateral(controller, cfg) {
            Err(error) => {
                // The SDK consumed the active state before returning this error. Classify this as
                // a runtime transport fault so the CLI still emits a non-success report instead of
                // dropping report generation entirely.
                Ok(TeleopLoopExit {
                    faulted: true,
                    report: BilateralRunReport {
                        exit_reason: Some(BilateralExitReason::RuntimeTransportFault),
                        last_error: Some(error.to_string()),
                        ..BilateralRunReport::default()
                    },
                })
            },
            Ok(DualArmLoopExit::Standby { arms, report }) => {
                self.standby = Some(arms);
                Ok(TeleopLoopExit {
                    faulted: false,
                    report,
                })
            },
            Ok(DualArmLoopExit::Faulted { arms: _, report }) => Ok(TeleopLoopExit {
                faulted: true,
                report,
            }),
        }
    }
}

impl ExperimentalRawClockTeleopBackend for RealTeleopBackend {
    fn connect_soft(&mut self, targets: &RoleTargets, baud_rate: u32) -> Result<()> {
        let master = connect_soft_socketcan_standby("master", &targets.master, baud_rate)?;
        let slave = connect_soft_socketcan_standby("slave", &targets.slave, baud_rate)?;
        let observers = ExperimentalSoftRealtimeObservers {
            master: master.observer().clone(),
            slave: slave.observer().clone(),
        };

        self.standby = None;
        self.active = None;
        self.experimental_pending = Some(ExperimentalSoftRealtimeStandbyArms { master, slave });
        self.experimental_standby = None;
        self.experimental_active = None;
        self.experimental_observers = Some(observers);
        Ok(())
    }

    fn warmup_raw_clock(
        &mut self,
        settings: &TeleopRawClockSettings,
        frequency_hz: f64,
        max_iterations: Option<usize>,
        cancel_signal: Arc<AtomicBool>,
    ) -> Result<RawClockWarmupSummary> {
        let config =
            experimental_raw_clock_config_from_settings(settings, frequency_hz, max_iterations);
        let standby = match self.experimental_standby.take() {
            Some(standby) => standby,
            None => {
                let arms = self
                    .experimental_pending
                    .take()
                    .context("experimental raw-clock backend is not connected")?;
                ExperimentalRawClockDualArmStandby::new(arms.master, arms.slave, config)?
            },
        };

        let warmed = standby.warmup(
            DualArmReadPolicy::default().per_arm,
            Duration::from_secs(settings.warmup_secs),
            cancel_signal.as_ref(),
        )?;
        self.experimental_standby = Some(warmed);

        Ok(RawClockWarmupSummary::default())
    }

    fn capture_calibration(&self, map: JointMirrorMap) -> Result<DualArmCalibration> {
        let observers = self
            .experimental_observers
            .as_ref()
            .context("experimental raw-clock backend is not connected")?;
        let snapshot = experimental_soft_snapshot(observers, DualArmReadPolicy::default())?;
        Ok(DualArmCalibration {
            master_zero: snapshot.left.state.position,
            slave_zero: snapshot.right.state.position,
            map,
        })
    }

    fn standby_snapshot(&self, policy: DualArmReadPolicy) -> Result<DualArmSnapshot> {
        let observers = self
            .experimental_observers
            .as_ref()
            .context("experimental raw-clock backend is not connected")?;
        experimental_soft_snapshot(observers, policy)
    }

    fn enable_mit_passthrough(
        &mut self,
        master: MitModeConfig,
        slave: MitModeConfig,
    ) -> Result<()> {
        let standby = self
            .experimental_standby
            .take()
            .context("experimental raw-clock backend is not in warmed standby")?;
        let active = standby.enable_mit_passthrough(master, slave)?;
        self.experimental_active = Some(active);
        Ok(())
    }

    fn active_snapshot(&self, policy: DualArmReadPolicy) -> Result<DualArmSnapshot> {
        let observers = self
            .experimental_observers
            .as_ref()
            .context("experimental raw-clock backend is not connected")?;
        experimental_soft_snapshot(observers, policy)
    }

    fn disable_active_passthrough(&mut self) -> Result<()> {
        let active = self
            .experimental_active
            .take()
            .context("experimental raw-clock backend is not active")?;
        let standby = active.disable_both(DisableConfig::default())?;
        self.experimental_standby = Some(standby);
        Ok(())
    }

    fn run_master_follower_raw_clock(
        &mut self,
        settings: RuntimeTeleopSettingsHandle,
        _raw_clock: TeleopRawClockSettings,
        cancel_signal: Arc<AtomicBool>,
    ) -> Result<ExperimentalRawClockRunExit> {
        let runtime_settings = settings.snapshot();
        if runtime_settings.mode != TeleopMode::MasterFollower {
            bail!("experimental calibrated raw clock currently supports master-follower mode only");
        }

        let active = self
            .experimental_active
            .take()
            .context("experimental raw-clock backend is not active")?;
        let run_config = ExperimentalRawClockRunConfig {
            read_policy: DualArmReadPolicy::default().per_arm,
            command_timeout: Duration::from_millis(20),
            disable_config: DisableConfig::default(),
            cancel_signal: Some(cancel_signal),
        };

        match active.run_master_follower(runtime_settings.calibration, run_config) {
            Ok(PiperRawClockRunExit::Standby { arms, report }) => {
                self.experimental_standby = Some(*arms);
                Ok(ExperimentalRawClockRunExit {
                    faulted: false,
                    report,
                })
            },
            Ok(PiperRawClockRunExit::Faulted { arms: _, report }) => {
                Ok(ExperimentalRawClockRunExit {
                    faulted: true,
                    report,
                })
            },
            Err(error) => Ok(ExperimentalRawClockRunExit {
                faulted: true,
                report: raw_clock_error_report(
                    RawClockRuntimeExitReason::RuntimeTransportFault,
                    error.to_string(),
                ),
            }),
        }
    }
}

fn connect_soft_socketcan_standby(
    role: &'static str,
    target: &ConcreteTeleopTarget,
    baud_rate: u32,
) -> Result<Piper<Standby, SoftRealtime>> {
    let ConcreteTeleopTarget::SocketCan { iface } = target else {
        bail!("experimental calibrated raw clock requires explicit SocketCAN target for {role}");
    };

    let connected = PiperBuilder::new()
        .socketcan(iface.clone())
        .baud_rate(baud_rate)
        .build()
        .with_context(|| {
            format!("failed to connect experimental {role} SocketCAN target socketcan:{iface}")
        })?;

    match connected.require_motion()? {
        MotionConnectedPiper::Soft(MotionConnectedState::Standby(standby)) => Ok(standby),
        MotionConnectedPiper::Soft(MotionConnectedState::Maintenance(_))
        | MotionConnectedPiper::Strict(MotionConnectedState::Maintenance(_)) => {
            bail!("maintenance required before experimental teleop")
        },
        MotionConnectedPiper::Strict(MotionConnectedState::Standby(_)) => {
            bail!(
                "experimental calibrated raw path expected SoftRealtime; use normal StrictRealtime teleop"
            )
        },
    }
}

fn experimental_soft_snapshot(
    observers: &ExperimentalSoftRealtimeObservers,
    policy: DualArmReadPolicy,
) -> Result<DualArmSnapshot> {
    let left = experimental_soft_control_snapshot_full(&observers.master, policy, "master")?;
    let right = experimental_soft_control_snapshot_full(&observers.slave, policy, "slave")?;
    let position_skew_us = left.position_host_rx_mono_us.abs_diff(right.position_host_rx_mono_us);
    let dynamic_skew_us = left.dynamic_host_rx_mono_us.abs_diff(right.dynamic_host_rx_mono_us);
    let inter_arm_skew = Duration::from_micros(position_skew_us.max(dynamic_skew_us));
    if inter_arm_skew > policy.max_inter_arm_skew {
        bail!(
            "experimental raw-clock dual-arm snapshot inter-arm skew {}us exceeds {}us",
            inter_arm_skew.as_micros(),
            policy.max_inter_arm_skew.as_micros()
        );
    }

    Ok(DualArmSnapshot {
        left,
        right,
        inter_arm_skew,
        host_cycle_timestamp: Instant::now(),
    })
}

fn experimental_soft_control_snapshot_full(
    observer: &Observer<SoftRealtime>,
    policy: DualArmReadPolicy,
    role: &'static str,
) -> Result<ControlSnapshotFull> {
    let health = observer.runtime_health();
    if !health.connected || !health.rx_alive || !health.tx_alive || health.fault.is_some() {
        bail!(
            "experimental raw-clock {role} runtime unhealthy: connected={}, rx_alive={}, tx_alive={}, fault={:?}",
            health.connected,
            health.rx_alive,
            health.tx_alive,
            health.fault
        );
    }
    if health.last_feedback_age > policy.per_arm.max_feedback_age {
        bail!(
            "experimental raw-clock {role} feedback age {:?} exceeds {:?}",
            health.last_feedback_age,
            policy.per_arm.max_feedback_age
        );
    }

    let position = observer.raw_joint_position_state();
    let dynamic = observer.raw_joint_dynamic_state();
    if !position.is_fully_valid() {
        bail!(
            "experimental raw-clock {role} position feedback incomplete: mask=0x{:02x}",
            position.frame_valid_mask
        );
    }
    if !dynamic.is_complete() {
        bail!(
            "experimental raw-clock {role} dynamic feedback incomplete: mask=0x{:02x}",
            dynamic.valid_mask
        );
    }

    let skew_us = signed_us_diff(dynamic.group_timestamp_us, position.hardware_timestamp_us);
    if position.hardware_timestamp_us.abs_diff(dynamic.group_timestamp_us)
        > policy.per_arm.max_state_skew_us
    {
        bail!(
            "experimental raw-clock {role} state skew {}us exceeds {}us",
            skew_us,
            policy.per_arm.max_state_skew_us
        );
    }

    Ok(ControlSnapshotFull {
        state: piper_client::observer::ControlSnapshot {
            position: JointArray::new(position.joint_pos.map(Rad)),
            velocity: JointArray::new(dynamic.joint_vel.map(RadPerSecond)),
            torque: JointArray::new(dynamic.get_all_torques().map(NewtonMeter)),
            position_timestamp_us: position.hardware_timestamp_us,
            dynamic_timestamp_us: dynamic.group_timestamp_us,
            skew_us,
        },
        position_host_rx_mono_us: position.host_rx_mono_us,
        dynamic_host_rx_mono_us: dynamic.group_host_rx_mono_us,
        feedback_age: health.last_feedback_age,
    })
}

fn signed_us_diff(left: u64, right: u64) -> i64 {
    let diff = left as i128 - right as i128;
    diff.clamp(i64::MIN as i128, i64::MAX as i128) as i64
}

fn raw_clock_error_report(
    exit_reason: RawClockRuntimeExitReason,
    last_error: String,
) -> RawClockRuntimeReport {
    RawClockRuntimeReport {
        master: empty_raw_clock_health_for_error(),
        slave: empty_raw_clock_health_for_error(),
        max_inter_arm_skew_us: 0,
        inter_arm_skew_p95_us: 0,
        clock_health_failures: 1,
        read_faults: 0,
        submission_faults: 0,
        runtime_faults: 1,
        iterations: 0,
        exit_reason: Some(exit_reason),
        master_stop_attempt: StopAttemptResult::NotAttempted,
        slave_stop_attempt: StopAttemptResult::NotAttempted,
        last_error: Some(last_error),
    }
}

fn empty_raw_clock_health_for_error() -> RawClockHealth {
    RawClockHealth {
        healthy: false,
        sample_count: 0,
        window_duration_us: 0,
        drift_ppm: f64::NAN,
        residual_p50_us: 0,
        residual_p95_us: 0,
        residual_p99_us: 0,
        residual_max_us: 0,
        sample_gap_max_us: 0,
        last_sample_age_us: u64::MAX,
        raw_timestamp_regressions: 0,
        reason: Some("runtime did not produce raw-clock health".to_string()),
    }
}

pub struct RealTeleopIo {
    cancel: Arc<AtomicBool>,
}

#[derive(Clone)]
enum CtrlcInstallState {
    Installed(Arc<AtomicBool>),
    Failed(String),
}

impl RealTeleopIo {
    pub fn install_ctrlc() -> Result<Self> {
        static CTRL_C_INSTALL: OnceLock<CtrlcInstallState> = OnceLock::new();

        let cancel = ctrlc_cancel_signal(&CTRL_C_INSTALL, || {
            let cancel = Arc::new(AtomicBool::new(false));
            let handler_cancel = cancel.clone();
            ctrlc::set_handler(move || {
                handler_cancel.store(true, Ordering::SeqCst);
            })
            .map_err(|error| error.to_string())?;
            Ok(cancel)
        })?;

        Ok(Self { cancel })
    }
}

fn ctrlc_cancel_signal(
    install: &'static OnceLock<CtrlcInstallState>,
    installer: impl FnOnce() -> std::result::Result<Arc<AtomicBool>, String>,
) -> Result<Arc<AtomicBool>> {
    if let Some(state) = install.get() {
        return ctrlc_signal_from_state(state, true);
    }

    let state = install.get_or_init(|| match installer() {
        Ok(cancel) => CtrlcInstallState::Installed(cancel),
        Err(error) => CtrlcInstallState::Failed(error),
    });
    ctrlc_signal_from_state(state, false)
}

fn ctrlc_signal_from_state(state: &CtrlcInstallState, reset: bool) -> Result<Arc<AtomicBool>> {
    match state {
        CtrlcInstallState::Installed(cancel) => {
            if reset {
                cancel.store(false, Ordering::SeqCst);
            }
            Ok(cancel.clone())
        },
        CtrlcInstallState::Failed(error) => {
            bail!("failed to install Ctrl+C handler: {error}");
        },
    }
}

impl TeleopIo for RealTeleopIo {
    fn cancel_signal(&self) -> Arc<AtomicBool> {
        self.cancel.clone()
    }

    fn confirm_start(&mut self, summary: &StartupSummary) -> Result<bool> {
        print_startup_summary(summary)?;
        if summary.yes {
            return Ok(true);
        }
        read_yes_from_stdin_unless_cancelled(&self.cancel)
    }

    fn cancel_requested(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }

    fn start_console(
        &mut self,
        settings: RuntimeTeleopSettingsHandle,
        started_at: Instant,
    ) -> Result<Option<JoinHandle<Result<()>>>> {
        Ok(Some(crate::teleop::console::spawn_console_thread(
            settings,
            started_at,
            self.cancel.clone(),
        )))
    }

    fn write_json_report(&mut self, path: &Path, report: &TeleopJsonReport) -> Result<()> {
        crate::teleop::report::write_json_report(path, report)
    }
}

fn print_startup_summary(summary: &StartupSummary) -> Result<()> {
    let mut stderr = io::stderr().lock();
    writeln!(stderr, "teleop dual-arm startup summary")?;
    writeln!(
        stderr,
        "  master target: {}",
        format_target_for_operator(&summary.targets.master)
    )?;
    writeln!(
        stderr,
        "  slave target: {}",
        format_target_for_operator(&summary.targets.slave)
    )?;
    writeln!(stderr, "  mode: {}", format_mode_for_operator(summary.mode))?;
    writeln!(
        stderr,
        "  profile: {}",
        format_profile_for_operator(summary.profile)
    )?;
    writeln!(stderr, "  frequency: {:.1} Hz", summary.frequency_hz)?;
    writeln!(
        stderr,
        "  gains: track_kp={:.3}, track_kd={:.3}, master_damping={:.3}, reflection_gain={:.3}",
        summary.track_kp, summary.track_kd, summary.master_damping, summary.reflection_gain
    )?;
    writeln!(stderr, "  calibration: {}", summary.calibration_source)?;
    if let Some(path) = &summary.calibration_path {
        writeln!(stderr, "  calibration path: {}", path.display())?;
    }
    writeln!(stderr, "  gripper mirror: {}", summary.gripper_mirror)?;
    writeln!(stderr, "  experimental={}", summary.experimental)?;
    writeln!(stderr, "  strict_realtime={}", summary.strict_realtime)?;
    if let Some(timing_source) = &summary.timing_source {
        writeln!(stderr, "  timing_source={timing_source}")?;
    }
    if let Some(path) = &summary.report_path {
        writeln!(stderr, "  report json: {}", path.display())?;
    }
    if !summary.yes {
        write!(stderr, "Type 'yes' or 'y' to enable MIT teleop: ")?;
        stderr.flush()?;
    }
    Ok(())
}

fn read_yes_from_stdin_unless_cancelled(cancel: &Arc<AtomicBool>) -> Result<bool> {
    let (tx, rx) = mpsc::channel();
    // If Ctrl+C wins this race, this one-shot CLI path may leave one stdin reader thread blocked
    // until process exit or terminal input arrives. That is preferable to blocking orchestration.
    thread::spawn(move || {
        let mut line = String::new();
        let result = io::stdin().read_line(&mut line).map(|_| line);
        let _ = tx.send(result);
    });

    loop {
        if cancel.load(Ordering::SeqCst) {
            return Ok(false);
        }

        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(Ok(line)) => {
                let value = line.trim();
                return Ok(value.eq_ignore_ascii_case("y") || value.eq_ignore_ascii_case("yes"));
            },
            Ok(Err(error)) => return Err(error).context("failed to read operator confirmation"),
            Err(mpsc::RecvTimeoutError::Timeout) => {},
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                bail!("operator confirmation reader stopped unexpectedly");
            },
        }
    }
}

fn format_target_for_operator(target: &crate::teleop::target::ConcreteTeleopTarget) -> String {
    match target {
        crate::teleop::target::ConcreteTeleopTarget::SocketCan { iface } => {
            format!("socketcan:{iface}")
        },
        crate::teleop::target::ConcreteTeleopTarget::GsUsbSerial { serial } => {
            format!("gs-usb-serial:{serial}")
        },
        crate::teleop::target::ConcreteTeleopTarget::GsUsbBusAddress { bus, address } => {
            format!("gs-usb-bus-address:{bus}:{address}")
        },
    }
}

fn format_mode_for_operator(mode: TeleopMode) -> &'static str {
    match mode {
        TeleopMode::MasterFollower => "master-follower",
        TeleopMode::Bilateral => "bilateral",
    }
}

fn format_profile_for_operator(profile: TeleopProfile) -> &'static str {
    match profile {
        TeleopProfile::Production => "production",
        TeleopProfile::Debug => "debug",
    }
}

pub fn run_dual_arm_blocking(
    args: TeleopDualArmArgs,
    backend: &mut RealTeleopBackend,
) -> Result<()> {
    let mut io = RealTeleopIo::install_ctrlc()?;
    match run_workflow(args, backend, &mut io)? {
        TeleopExitStatus::Success => Ok(()),
        TeleopExitStatus::Failure => bail!("teleop dual-arm failed"),
    }
}

pub async fn run_dual_arm(args: TeleopDualArmArgs) -> Result<()> {
    tokio::task::spawn_blocking(move || {
        let mut backend = RealTeleopBackend::default();
        run_dual_arm_blocking(args, &mut backend)
    })
    .await?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::teleop::report::TeleopExitStatus;
    use crate::teleop::target::{ConcreteTeleopTarget, TeleopPlatform};
    use anyhow::{Result, bail};
    use piper_client::dual_arm::{
        BilateralExitReason, BilateralLoopConfig, BilateralRunReport, DualArmCalibration,
        DualArmReadPolicy, DualArmSnapshot, JointMirrorMap, StopAttemptResult,
    };
    use piper_client::observer::{ControlSnapshot, ControlSnapshotFull};
    use piper_client::state::MitModeConfig;
    use piper_client::types::{JointArray, NewtonMeter, Rad, RadPerSecond};
    use std::path::{Path, PathBuf};
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    };
    use std::thread::JoinHandle;
    use std::time::{Duration, Instant};

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum WorkflowCall {
        Connect,
        RuntimeHealth,
        RawClockWarmup,
        ConfirmStart,
        StandbySnapshot,
        CaptureCalibration,
        Enable,
        DisableActive,
        ActiveSnapshot,
        StartConsole,
        RunLoop,
        WriteReport,
    }

    #[derive(Debug, Clone, Default)]
    struct WorkflowTrace {
        calls: Arc<Mutex<Vec<WorkflowCall>>>,
    }

    impl WorkflowTrace {
        fn push(&self, call: WorkflowCall) {
            self.calls.lock().expect("trace lock poisoned").push(call);
        }

        fn calls(&self) -> Vec<WorkflowCall> {
            self.calls.lock().expect("trace lock poisoned").clone()
        }
    }

    #[derive(Debug, Clone, Default, PartialEq, Eq)]
    struct FakeBackendCallCounts {
        experimental_raw_clock_runs: usize,
        strict_runs: usize,
        fault_shutdown_attempted: bool,
        master_stop_attempts: usize,
        slave_stop_attempts: usize,
    }

    #[derive(Debug, Clone)]
    struct FakeTeleopBackend {
        trace: WorkflowTrace,
        call_counts: Arc<Mutex<FakeBackendCallCounts>>,
        health_failure: bool,
        standby_mismatch: bool,
        active_mismatch: bool,
        active_snapshot_error: bool,
        enable_error: bool,
        disabled_or_faulted: Arc<AtomicBool>,
        loop_exit: TeleopLoopExit,
        experimental_run_exit: ExperimentalRawClockRunExit,
    }

    impl Default for FakeTeleopBackend {
        fn default() -> Self {
            Self {
                trace: WorkflowTrace::default(),
                call_counts: Arc::new(Mutex::new(FakeBackendCallCounts::default())),
                health_failure: false,
                standby_mismatch: false,
                active_mismatch: false,
                active_snapshot_error: false,
                enable_error: false,
                disabled_or_faulted: Arc::new(AtomicBool::new(false)),
                loop_exit: TeleopLoopExit {
                    faulted: false,
                    report: BilateralRunReport {
                        exit_reason: Some(BilateralExitReason::MaxIterations),
                        ..BilateralRunReport::default()
                    },
                },
                experimental_run_exit: ExperimentalRawClockRunExit {
                    faulted: false,
                    report: raw_clock_report_success(),
                },
            }
        }
    }

    impl FakeTeleopBackend {
        fn with_trace(trace: WorkflowTrace) -> Self {
            Self {
                trace,
                ..Self::default()
            }
        }

        fn with_unhealthy_runtime() -> Self {
            Self {
                health_failure: true,
                ..Self::default()
            }
        }

        fn with_standby_snapshot_mismatch() -> Self {
            Self {
                standby_mismatch: true,
                ..Self::default()
            }
        }

        fn with_active_snapshot_mismatch() -> Self {
            Self {
                active_mismatch: true,
                ..Self::default()
            }
        }

        fn with_active_snapshot_error() -> Self {
            Self {
                active_snapshot_error: true,
                ..Self::default()
            }
        }

        fn enable_confirmation_failure_after_dispatch() -> Self {
            Self {
                enable_error: true,
                ..Self::default()
            }
        }

        fn cancel_during_enable_after_active() -> Self {
            Self::default()
        }

        fn loop_exits_cancelled_with_trace(trace: WorkflowTrace) -> Self {
            Self {
                trace,
                loop_exit: TeleopLoopExit {
                    faulted: false,
                    report: BilateralRunReport {
                        exit_reason: Some(BilateralExitReason::Cancelled),
                        ..BilateralRunReport::default()
                    },
                },
                ..Self::default()
            }
        }

        fn with_experimental_raw_clock_success(self) -> Self {
            self
        }

        fn with_experimental_timing_failure(mut self, message: &str) -> Self {
            self.experimental_run_exit = ExperimentalRawClockRunExit {
                faulted: true,
                report: raw_clock_report_timing_failure(message),
            };
            self
        }

        fn calls(&self) -> Vec<WorkflowCall> {
            self.trace.calls()
        }

        fn call_counts(&self) -> FakeBackendCallCounts {
            self.call_counts.lock().expect("call counts lock poisoned").clone()
        }

        fn was_disabled_or_faulted_before_report(&self) -> bool {
            self.disabled_or_faulted.load(Ordering::SeqCst)
        }
    }

    impl TeleopBackend for FakeTeleopBackend {
        fn connect(
            &mut self,
            _targets: &crate::teleop::target::RoleTargets,
            _baud_rate: u32,
        ) -> Result<()> {
            self.trace.push(WorkflowCall::Connect);
            Ok(())
        }

        fn runtime_health_ok(&self) -> Result<()> {
            self.trace.push(WorkflowCall::RuntimeHealth);
            if self.health_failure {
                bail!("runtime health check failed");
            }
            Ok(())
        }

        fn standby_snapshot(&self, _policy: DualArmReadPolicy) -> Result<DualArmSnapshot> {
            self.trace.push(WorkflowCall::StandbySnapshot);
            Ok(sample_snapshot(self.standby_mismatch))
        }

        fn capture_calibration(&self, _map: JointMirrorMap) -> Result<DualArmCalibration> {
            self.trace.push(WorkflowCall::CaptureCalibration);
            Ok(sample_calibration())
        }

        fn enable_mit(
            &mut self,
            _master: MitModeConfig,
            _slave: MitModeConfig,
        ) -> Result<EnableOutcome> {
            self.trace.push(WorkflowCall::Enable);
            if self.enable_error {
                bail!("enable confirmation failed");
            }
            Ok(EnableOutcome::Active)
        }

        fn disable_active(&mut self) -> Result<()> {
            self.trace.push(WorkflowCall::DisableActive);
            self.disabled_or_faulted.store(true, Ordering::SeqCst);
            Ok(())
        }

        fn active_snapshot(&self, _policy: DualArmReadPolicy) -> Result<DualArmSnapshot> {
            self.trace.push(WorkflowCall::ActiveSnapshot);
            if self.active_snapshot_error {
                bail!("active snapshot read failed");
            }
            Ok(sample_snapshot(self.active_mismatch))
        }

        fn run_loop(
            &mut self,
            _controller: crate::teleop::controller::RuntimeTeleopController,
            _cfg: BilateralLoopConfig,
        ) -> Result<TeleopLoopExit> {
            self.trace.push(WorkflowCall::RunLoop);
            self.call_counts.lock().expect("call counts lock poisoned").strict_runs += 1;
            self.disabled_or_faulted.store(true, Ordering::SeqCst);
            Ok(self.loop_exit.clone())
        }
    }

    impl ExperimentalRawClockTeleopBackend for FakeTeleopBackend {
        fn connect_soft(
            &mut self,
            _targets: &crate::teleop::target::RoleTargets,
            _baud_rate: u32,
        ) -> Result<()> {
            self.trace.push(WorkflowCall::Connect);
            Ok(())
        }

        fn warmup_raw_clock(
            &mut self,
            _settings: &TeleopRawClockSettings,
            _frequency_hz: f64,
            _max_iterations: Option<usize>,
            cancel_signal: Arc<AtomicBool>,
        ) -> Result<RawClockWarmupSummary> {
            self.trace.push(WorkflowCall::RawClockWarmup);
            if cancel_signal.load(Ordering::SeqCst) {
                bail!("raw-clock warmup cancelled");
            }
            Ok(RawClockWarmupSummary {
                master_clock_drift_ppm: Some(1.0),
                slave_clock_drift_ppm: Some(1.5),
                master_residual_p95_us: Some(12),
                slave_residual_p95_us: Some(14),
                max_estimated_inter_arm_skew_us: Some(100),
                estimated_inter_arm_skew_p95_us: Some(80),
                clock_health_failures: 0,
            })
        }

        fn capture_calibration(&self, _map: JointMirrorMap) -> Result<DualArmCalibration> {
            self.trace.push(WorkflowCall::CaptureCalibration);
            Ok(sample_calibration())
        }

        fn standby_snapshot(&self, _policy: DualArmReadPolicy) -> Result<DualArmSnapshot> {
            self.trace.push(WorkflowCall::StandbySnapshot);
            Ok(sample_snapshot(self.standby_mismatch))
        }

        fn enable_mit_passthrough(
            &mut self,
            _master: MitModeConfig,
            _slave: MitModeConfig,
        ) -> Result<()> {
            self.trace.push(WorkflowCall::Enable);
            if self.enable_error {
                bail!("enable confirmation failed");
            }
            Ok(())
        }

        fn active_snapshot(&self, _policy: DualArmReadPolicy) -> Result<DualArmSnapshot> {
            self.trace.push(WorkflowCall::ActiveSnapshot);
            if self.active_snapshot_error {
                bail!("active snapshot read failed");
            }
            Ok(sample_snapshot(self.active_mismatch))
        }

        fn disable_active_passthrough(&mut self) -> Result<()> {
            self.trace.push(WorkflowCall::DisableActive);
            self.disabled_or_faulted.store(true, Ordering::SeqCst);
            Ok(())
        }

        fn run_master_follower_raw_clock(
            &mut self,
            _settings: RuntimeTeleopSettingsHandle,
            _raw_clock: TeleopRawClockSettings,
            _cancel_signal: Arc<AtomicBool>,
        ) -> Result<ExperimentalRawClockRunExit> {
            self.trace.push(WorkflowCall::RunLoop);
            let mut counts = self.call_counts.lock().expect("call counts lock poisoned");
            counts.experimental_raw_clock_runs += 1;
            if self.experimental_run_exit.faulted {
                counts.fault_shutdown_attempted = true;
                if self.experimental_run_exit.report.master_stop_attempt
                    != StopAttemptResult::NotAttempted
                {
                    counts.master_stop_attempts += 1;
                }
                if self.experimental_run_exit.report.slave_stop_attempt
                    != StopAttemptResult::NotAttempted
                {
                    counts.slave_stop_attempts += 1;
                }
            }
            drop(counts);

            self.disabled_or_faulted.store(true, Ordering::SeqCst);
            Ok(self.experimental_run_exit.clone())
        }
    }

    struct FakeTeleopIo {
        trace: WorkflowTrace,
        cancel: Arc<AtomicBool>,
        confirm: bool,
        cancel_on_confirm: bool,
        cancel_during_enable: bool,
        cancel_checks: Arc<Mutex<usize>>,
        report_write_error: bool,
        console_start_error: bool,
        console_finished_error: bool,
    }

    impl Default for FakeTeleopIo {
        fn default() -> Self {
            Self {
                trace: WorkflowTrace::default(),
                cancel: Arc::new(AtomicBool::new(false)),
                confirm: true,
                cancel_on_confirm: false,
                cancel_during_enable: false,
                cancel_checks: Arc::new(Mutex::new(0)),
                report_write_error: false,
                console_start_error: false,
                console_finished_error: false,
            }
        }
    }

    impl FakeTeleopIo {
        fn decline_confirmation() -> Self {
            Self {
                confirm: false,
                ..Self::default()
            }
        }

        fn cancel_after_confirmation_before_enable() -> Self {
            Self {
                cancel_on_confirm: true,
                ..Self::default()
            }
        }

        fn cancel_during_confirmation_input() -> Self {
            Self {
                confirm: false,
                cancel_on_confirm: true,
                ..Self::default()
            }
        }

        fn cancel_during_enable() -> Self {
            Self {
                cancel_during_enable: true,
                ..Self::default()
            }
        }

        fn report_write_error_with_trace(trace: WorkflowTrace) -> Self {
            Self {
                trace,
                report_write_error: true,
                ..Self::default()
            }
        }

        fn finished_console_error_with_trace(trace: WorkflowTrace) -> Self {
            Self {
                trace,
                console_finished_error: true,
                ..Self::default()
            }
        }

        fn start_console_error() -> Self {
            Self {
                console_start_error: true,
                ..Self::default()
            }
        }

        fn report_and_console_error_with_trace(trace: WorkflowTrace) -> Self {
            Self {
                trace,
                report_write_error: true,
                console_finished_error: true,
                ..Self::default()
            }
        }
    }

    impl TeleopIo for FakeTeleopIo {
        fn cancel_signal(&self) -> Arc<AtomicBool> {
            self.cancel.clone()
        }

        fn confirm_start(&mut self, _summary: &StartupSummary) -> Result<bool> {
            self.trace.push(WorkflowCall::ConfirmStart);
            if self.cancel_on_confirm {
                self.cancel.store(true, Ordering::SeqCst);
            }
            Ok(self.confirm)
        }

        fn cancel_requested(&self) -> bool {
            if self.cancel_during_enable {
                let mut checks = self.cancel_checks.lock().expect("cancel check lock poisoned");
                *checks += 1;
                if *checks >= 2 {
                    self.cancel.store(true, Ordering::SeqCst);
                }
            }
            self.cancel.load(Ordering::SeqCst)
        }

        fn start_console(
            &mut self,
            _settings: crate::teleop::controller::RuntimeTeleopSettingsHandle,
            _started_at: Instant,
        ) -> Result<Option<JoinHandle<Result<()>>>> {
            self.trace.push(WorkflowCall::StartConsole);
            if self.console_start_error {
                bail!("console failed to start");
            }
            if self.console_finished_error {
                let handle = std::thread::spawn(|| bail!("console finished with error"));
                while !handle.is_finished() {
                    std::thread::yield_now();
                }
                return Ok(Some(handle));
            }
            Ok(None)
        }

        fn write_json_report(
            &mut self,
            _path: &Path,
            _report: &crate::teleop::report::TeleopJsonReport,
        ) -> Result<()> {
            self.trace.push(WorkflowCall::WriteReport);
            if self.report_write_error {
                bail!("report write failed");
            }
            Ok(())
        }
    }

    #[test]
    fn ctrlc_initializer_preserves_first_signal_and_resets_reused_flag() {
        static TEST_CTRL_C_INSTALL: OnceLock<CtrlcInstallState> = OnceLock::new();

        let first =
            ctrlc_cancel_signal(&TEST_CTRL_C_INSTALL, || Ok(Arc::new(AtomicBool::new(true))))
                .expect("test ctrlc installer should initialize");
        assert!(first.load(Ordering::SeqCst));

        first.store(true, Ordering::SeqCst);
        let second = ctrlc_cancel_signal(&TEST_CTRL_C_INSTALL, || {
            panic!("installer must not run after OnceLock is initialized")
        })
        .expect("initialized ctrlc signal should be reused");

        assert!(Arc::ptr_eq(&first, &second));
        assert!(!second.load(Ordering::SeqCst));
    }

    #[test]
    fn malformed_calibration_fails_before_connect() {
        let temp = tempfile::tempdir().unwrap();
        let cal_path = temp.path().join("bad.toml");
        std::fs::write(&cal_path, "not toml").unwrap();
        let backend = FakeTeleopBackend::default();

        let err = run_workflow_for_test(args_with_calibration(&cal_path), backend.clone())
            .expect_err("malformed calibration must fail");

        assert!(err.to_string().contains("calibration"));
        assert_eq!(backend.calls(), Vec::<WorkflowCall>::new());
    }

    #[test]
    fn save_calibration_existing_file_fails_before_connect() {
        let temp = tempfile::tempdir().unwrap();
        let save_path = temp.path().join("calibration.toml");
        std::fs::write(&save_path, "existing").unwrap();
        let backend = FakeTeleopBackend::default();

        let err = run_workflow_for_test(args_with_save_calibration(&save_path), backend.clone())
            .expect_err("existing save path must fail before connect");

        assert!(err.to_string().contains("exists"));
        assert_eq!(backend.calls(), Vec::<WorkflowCall>::new());
    }

    #[test]
    fn declined_confirmation_exits_without_enable() {
        let backend = FakeTeleopBackend::default();
        let io = FakeTeleopIo::decline_confirmation();

        let err = run_workflow_for_test_with_io(valid_args(), backend.clone(), io)
            .expect_err("declining confirmation must stop before enable");

        assert!(err.to_string().contains("confirmation"));
        assert!(backend.calls().contains(&WorkflowCall::Connect));
        assert!(!backend.calls().contains(&WorkflowCall::Enable));
        assert!(!backend.calls().contains(&WorkflowCall::RunLoop));
    }

    #[test]
    fn cancel_after_confirmation_before_enable_returns_success_without_enable() {
        let backend = FakeTeleopBackend::default();
        let io = FakeTeleopIo::cancel_after_confirmation_before_enable();

        let status = run_workflow_for_test_with_io(valid_args(), backend.clone(), io)
            .expect("Ctrl+C before enable must stop cleanly");

        assert_eq!(status, TeleopExitStatus::Success);
        assert!(backend.calls().contains(&WorkflowCall::Connect));
        assert!(!backend.calls().contains(&WorkflowCall::Enable));
        assert!(!backend.calls().contains(&WorkflowCall::DisableActive));
        assert!(!backend.calls().contains(&WorkflowCall::RunLoop));
    }

    #[test]
    fn cancel_during_confirmation_input_returns_success_without_enable() {
        let backend = FakeTeleopBackend::default();
        let io = FakeTeleopIo::cancel_during_confirmation_input();

        let status = run_workflow_for_test_with_io(valid_args(), backend.clone(), io)
            .expect("Ctrl+C during confirmation input must stop cleanly");

        assert_eq!(status, TeleopExitStatus::Success);
        assert!(backend.calls().contains(&WorkflowCall::Connect));
        assert!(!backend.calls().contains(&WorkflowCall::Enable));
        assert!(!backend.calls().contains(&WorkflowCall::DisableActive));
        assert!(!backend.calls().contains(&WorkflowCall::RunLoop));
    }

    #[test]
    fn pre_enable_mismatch_fails_before_enable() {
        let backend = FakeTeleopBackend::with_standby_snapshot_mismatch();

        let err = run_workflow_for_test(valid_args(), backend.clone())
            .expect_err("pre-enable mismatch should fail");

        assert!(err.to_string().contains("posture"));
        assert!(backend.calls().contains(&WorkflowCall::StandbySnapshot));
        assert!(!backend.calls().contains(&WorkflowCall::Enable));
        assert!(!backend.calls().contains(&WorkflowCall::RunLoop));
    }

    #[test]
    fn unhealthy_runtime_health_fails_before_calibration_or_enable() {
        let backend = FakeTeleopBackend::with_unhealthy_runtime();

        let err = run_workflow_for_test(valid_args(), backend.clone())
            .expect_err("unhealthy runtime must fail before calibration or enable");

        assert!(err.to_string().contains("runtime health"));
        assert_call_order(
            backend.calls(),
            &[WorkflowCall::Connect, WorkflowCall::RuntimeHealth],
        );
        assert!(!backend.calls().contains(&WorkflowCall::CaptureCalibration));
        assert!(!backend.calls().contains(&WorkflowCall::Enable));
        assert!(!backend.calls().contains(&WorkflowCall::RunLoop));
    }

    #[test]
    fn post_enable_mismatch_disables_and_does_not_run_loop() {
        let backend = FakeTeleopBackend::with_active_snapshot_mismatch();

        let err = run_workflow_for_test(valid_args(), backend.clone())
            .expect_err("post-enable mismatch should fail");

        assert!(err.to_string().contains("posture"));
        assert!(backend.calls().contains(&WorkflowCall::DisableActive));
        assert!(!backend.calls().contains(&WorkflowCall::RunLoop));
    }

    #[test]
    fn active_snapshot_read_error_after_enable_disables_and_does_not_run_loop() {
        let backend = FakeTeleopBackend::with_active_snapshot_error();

        let err = run_workflow_for_test(valid_args(), backend.clone())
            .expect_err("active snapshot read failure should fail");

        assert!(err.to_string().contains("active snapshot"));
        assert_call_order(
            backend.calls(),
            &[
                WorkflowCall::Enable,
                WorkflowCall::ActiveSnapshot,
                WorkflowCall::DisableActive,
            ],
        );
        assert!(!backend.calls().contains(&WorkflowCall::RunLoop));
    }

    #[test]
    fn start_console_error_after_enable_disables_and_does_not_run_loop() {
        let trace = WorkflowTrace::default();
        let backend = FakeTeleopBackend::with_trace(trace.clone());
        let mut io = FakeTeleopIo::start_console_error();
        io.trace = trace.clone();

        let err = run_workflow_for_test_with_io(valid_args(), backend.clone(), io)
            .expect_err("console startup failure should fail");

        assert!(err.to_string().contains("console"));
        assert_call_order(
            trace.calls(),
            &[
                WorkflowCall::Enable,
                WorkflowCall::StartConsole,
                WorkflowCall::DisableActive,
            ],
        );
        assert!(!backend.calls().contains(&WorkflowCall::RunLoop));
    }

    #[test]
    fn enable_confirmation_failure_is_returned_without_cli_cleanup_call() {
        let backend = FakeTeleopBackend::enable_confirmation_failure_after_dispatch();

        let err =
            run_workflow_for_test(valid_args(), backend.clone()).expect_err("enable should fail");

        assert!(err.to_string().contains("enable"));
        assert!(backend.calls().contains(&WorkflowCall::Enable));
        assert!(!backend.calls().contains(&WorkflowCall::DisableActive));
        assert!(!backend.calls().contains(&WorkflowCall::RunLoop));
    }

    #[test]
    fn cancel_during_enable_disables_active_and_returns_success() {
        let backend = FakeTeleopBackend::cancel_during_enable_after_active();
        let io = FakeTeleopIo::cancel_during_enable();

        let status = run_workflow_for_test_with_io(valid_args(), backend.clone(), io)
            .expect("Ctrl+C during enable must exit safely");

        assert_eq!(status, TeleopExitStatus::Success);
        assert_call_order(
            backend.calls(),
            &[WorkflowCall::Enable, WorkflowCall::DisableActive],
        );
        assert!(!backend.calls().contains(&WorkflowCall::RunLoop));
    }

    #[test]
    fn report_write_failure_happens_after_disabled_or_faulted_state() {
        let trace = WorkflowTrace::default();
        let backend = FakeTeleopBackend::loop_exits_cancelled_with_trace(trace.clone());
        let io = FakeTeleopIo::report_write_error_with_trace(trace.clone());

        let err = run_workflow_for_test_with_io(args_with_report_json(), backend.clone(), io)
            .expect_err("report write failure should be surfaced");

        assert!(err.to_string().contains("report"));
        assert_call_order(
            trace.calls(),
            &[WorkflowCall::RunLoop, WorkflowCall::WriteReport],
        );
        assert!(backend.was_disabled_or_faulted_before_report());
    }

    #[test]
    fn finished_console_error_surfaces_after_run_loop_and_report_write() {
        let trace = WorkflowTrace::default();
        let backend = FakeTeleopBackend::loop_exits_cancelled_with_trace(trace.clone());
        let io = FakeTeleopIo::finished_console_error_with_trace(trace.clone());

        let err = run_workflow_for_test_with_io(args_with_report_json(), backend.clone(), io)
            .expect_err("finished console error should be surfaced");

        assert!(err.to_string().contains("console"));
        assert_call_order(
            trace.calls(),
            &[WorkflowCall::RunLoop, WorkflowCall::WriteReport],
        );
    }

    #[test]
    fn report_write_failure_takes_precedence_over_finished_console_error() {
        let trace = WorkflowTrace::default();
        let backend = FakeTeleopBackend::loop_exits_cancelled_with_trace(trace.clone());
        let io = FakeTeleopIo::report_and_console_error_with_trace(trace.clone());

        let err = run_workflow_for_test_with_io(args_with_report_json(), backend.clone(), io)
            .expect_err("report write failure should be surfaced before console error");

        assert!(err.to_string().contains("report"));
        assert!(!err.to_string().contains("console"));
        assert_call_order(
            trace.calls(),
            &[WorkflowCall::RunLoop, WorkflowCall::WriteReport],
        );
    }

    #[test]
    fn gs_usb_runtime_target_is_rejected_before_backend_connect() {
        let args = TeleopDualArmArgs {
            master_target: Some("gs-usb-serial:A".to_string()),
            slave_target: Some("gs-usb-serial:B".to_string()),
            ..TeleopDualArmArgs::default_for_tests()
        };
        let backend = FakeTeleopBackend::default();

        let err = run_workflow_for_test(args, backend.clone())
            .expect_err("gs-usb runtime should be rejected in v1");

        assert!(err.to_string().contains("SoftRealtime dual-arm"));
        assert_eq!(backend.calls(), Vec::<WorkflowCall>::new());
    }

    #[test]
    fn socketcan_runtime_target_is_rejected_on_non_linux_before_backend_connect() {
        let args = TeleopDualArmArgs {
            master_target: Some("socketcan:can0".to_string()),
            slave_target: Some("socketcan:can1".to_string()),
            ..TeleopDualArmArgs::default_for_tests()
        };
        let backend = FakeTeleopBackend::default();

        let err = run_workflow_for_test_on_platform(args, backend.clone(), TeleopPlatform::Other)
            .expect_err("SocketCAN runtime is Linux-only in v1");

        assert!(err.to_string().contains("Linux"));
        assert_eq!(backend.calls(), Vec::<WorkflowCall>::new());
    }

    #[test]
    fn experimental_raw_clock_uses_experimental_backend_path() {
        let trace = WorkflowTrace::default();
        let backend =
            FakeTeleopBackend::with_trace(trace.clone()).with_experimental_raw_clock_success();
        let io = FakeTeleopIo {
            trace: trace.clone(),
            ..FakeTeleopIo::default()
        };

        let status = run_workflow_for_test_with_io(experimental_args(), backend.clone(), io)
            .expect("experimental raw-clock workflow should succeed");

        assert_eq!(status, TeleopExitStatus::Success);
        assert_eq!(backend.call_counts().experimental_raw_clock_runs, 1);
        assert_eq!(backend.call_counts().strict_runs, 0);
        assert_call_order(
            trace.calls(),
            &[
                WorkflowCall::Connect,
                WorkflowCall::RawClockWarmup,
                WorkflowCall::ConfirmStart,
                WorkflowCall::Enable,
                WorkflowCall::RunLoop,
            ],
        );
    }

    #[test]
    fn real_experimental_backend_formats_socketcan_targets_for_startup_summary() {
        let targets = RoleTargets {
            master: ConcreteTeleopTarget::SocketCan {
                iface: "can0".to_string(),
            },
            slave: ConcreteTeleopTarget::SocketCan {
                iface: "can1".to_string(),
            },
        };

        let summary = StartupSummary::experimental_raw_clock_for_tests(targets);

        assert_eq!(summary.timing_source.as_deref(), Some("calibrated_hw_raw"));
        assert!(summary.experimental);
        assert!(!summary.strict_realtime);
    }

    #[test]
    fn experimental_raw_clock_health_failure_reports_failure() {
        let backend =
            FakeTeleopBackend::default().with_experimental_timing_failure("inter-arm skew");

        let err = run_workflow_for_test(experimental_args(), backend.clone())
            .expect_err("experimental timing health failure should fail workflow");

        assert!(err.to_string().contains("inter-arm skew"));
        assert!(backend.call_counts().fault_shutdown_attempted);
        assert_eq!(backend.call_counts().master_stop_attempts, 1);
        assert_eq!(backend.call_counts().slave_stop_attempts, 1);
    }

    #[test]
    fn experimental_raw_clock_pre_enable_posture_mismatch_fails_before_enable() {
        let backend = FakeTeleopBackend::with_standby_snapshot_mismatch();

        let err = run_workflow_for_test(experimental_args(), backend.clone())
            .expect_err("pre-enable mismatch should stop experimental raw-clock workflow");

        assert!(err.to_string().contains("posture"));
        assert!(backend.calls().contains(&WorkflowCall::StandbySnapshot));
        assert!(!backend.calls().contains(&WorkflowCall::Enable));
        assert!(!backend.calls().contains(&WorkflowCall::RunLoop));
    }

    #[test]
    fn experimental_raw_clock_post_enable_posture_mismatch_disables_without_run() {
        let backend = FakeTeleopBackend::with_active_snapshot_mismatch();

        let err = run_workflow_for_test(experimental_args(), backend.clone())
            .expect_err("post-enable mismatch should stop experimental raw-clock workflow");

        assert!(err.to_string().contains("posture"));
        assert_call_order(
            backend.calls(),
            &[
                WorkflowCall::Enable,
                WorkflowCall::ActiveSnapshot,
                WorkflowCall::DisableActive,
            ],
        );
        assert!(!backend.calls().contains(&WorkflowCall::RunLoop));
    }

    #[test]
    fn raw_clock_report_timing_uses_warmup_p95_when_runtime_has_no_skew_samples() {
        let warmup = RawClockWarmupSummary {
            estimated_inter_arm_skew_p95_us: Some(77),
            max_estimated_inter_arm_skew_us: Some(100),
            ..RawClockWarmupSummary::default()
        };
        let report = RawClockRuntimeReport {
            iterations: 0,
            max_inter_arm_skew_us: 0,
            inter_arm_skew_p95_us: 0,
            ..raw_clock_report_success()
        };

        let timing = report_timing_from_raw_clock(&warmup, &report);

        assert_eq!(timing.estimated_inter_arm_skew_p95_us, Some(77));
        assert_eq!(timing.max_estimated_inter_arm_skew_us, Some(100));
    }

    #[test]
    fn experimental_raw_clock_settings_convert_to_runtime_thresholds() {
        let settings = TeleopRawClockSettings {
            experimental_calibrated_raw: true,
            warmup_secs: 3,
            residual_p95_us: 120,
            residual_max_us: 300,
            drift_abs_ppm: 42.0,
            sample_gap_max_ms: 7,
            last_sample_age_ms: 9,
            inter_arm_skew_max_us: 1500,
        };

        let config = experimental_raw_clock_config_from_settings(&settings, 333.0, Some(12));

        assert_eq!(config.frequency_hz, 333.0);
        assert_eq!(config.max_iterations, Some(12));
        assert_eq!(config.estimator_thresholds.warmup_samples, 1200);
        assert_eq!(config.estimator_thresholds.warmup_window_us, 3_000_000);
        assert_eq!(config.estimator_thresholds.residual_p95_us, 120);
        assert_eq!(config.estimator_thresholds.residual_max_us, 300);
        assert_eq!(config.estimator_thresholds.drift_abs_ppm, 42.0);
        assert_eq!(config.estimator_thresholds.sample_gap_max_us, 7_000);
        assert_eq!(config.estimator_thresholds.last_sample_age_us, 9_000);
        assert_eq!(config.thresholds.inter_arm_skew_max_us, 1500);
        assert_eq!(config.thresholds.last_sample_age_us, 9_000);
    }

    fn run_workflow_for_test(
        args: TeleopDualArmArgs,
        backend: FakeTeleopBackend,
    ) -> Result<TeleopExitStatus> {
        run_workflow_for_test_with_io(args, backend.clone(), FakeTeleopIo::default())
    }

    fn run_workflow_for_test_with_io(
        args: TeleopDualArmArgs,
        mut backend: FakeTeleopBackend,
        mut io: FakeTeleopIo,
    ) -> Result<TeleopExitStatus> {
        run_workflow_on_platform(args, &mut backend, &mut io, TeleopPlatform::Linux)
    }

    fn run_workflow_for_test_on_platform(
        args: TeleopDualArmArgs,
        mut backend: FakeTeleopBackend,
        platform: TeleopPlatform,
    ) -> Result<TeleopExitStatus> {
        let mut io = FakeTeleopIo::default();
        run_workflow_on_platform(args, &mut backend, &mut io, platform)
    }

    fn valid_args() -> TeleopDualArmArgs {
        TeleopDualArmArgs {
            master_interface: Some("can0".to_string()),
            slave_interface: Some("can1".to_string()),
            yes: true,
            max_iterations: Some(1),
            ..TeleopDualArmArgs::default_for_tests()
        }
    }

    fn experimental_args() -> TeleopDualArmArgs {
        TeleopDualArmArgs {
            experimental_calibrated_raw: true,
            mode: Some(TeleopMode::MasterFollower),
            ..valid_args()
        }
    }

    fn args_with_calibration(path: &Path) -> TeleopDualArmArgs {
        TeleopDualArmArgs {
            calibration_file: Some(path.to_path_buf()),
            ..valid_args()
        }
    }

    fn args_with_save_calibration(path: &Path) -> TeleopDualArmArgs {
        TeleopDualArmArgs {
            save_calibration: Some(path.to_path_buf()),
            ..valid_args()
        }
    }

    fn args_with_report_json() -> TeleopDualArmArgs {
        TeleopDualArmArgs {
            report_json: Some(PathBuf::from("teleop-report.json")),
            ..valid_args()
        }
    }

    fn assert_call_order(actual: Vec<WorkflowCall>, expected: &[WorkflowCall]) {
        let mut search_from = 0;
        for call in expected {
            let Some(offset) =
                actual[search_from..].iter().position(|actual_call| actual_call == call)
            else {
                panic!("missing call {call:?} in {actual:?}");
            };
            search_from += offset + 1;
        }
    }

    fn raw_clock_report_success() -> RawClockRuntimeReport {
        RawClockRuntimeReport {
            master: raw_clock_health_for_tests(1.0, 12),
            slave: raw_clock_health_for_tests(1.5, 14),
            max_inter_arm_skew_us: 100,
            inter_arm_skew_p95_us: 80,
            clock_health_failures: 0,
            read_faults: 0,
            submission_faults: 0,
            runtime_faults: 0,
            iterations: 1,
            exit_reason: Some(RawClockRuntimeExitReason::MaxIterations),
            master_stop_attempt: StopAttemptResult::NotAttempted,
            slave_stop_attempt: StopAttemptResult::NotAttempted,
            last_error: None,
        }
    }

    fn raw_clock_report_timing_failure(message: &str) -> RawClockRuntimeReport {
        RawClockRuntimeReport {
            clock_health_failures: 1,
            exit_reason: Some(RawClockRuntimeExitReason::RawClockFault),
            master_stop_attempt: StopAttemptResult::ConfirmedSent,
            slave_stop_attempt: StopAttemptResult::ConfirmedSent,
            last_error: Some(message.to_string()),
            ..raw_clock_report_success()
        }
    }

    fn raw_clock_health_for_tests(drift_ppm: f64, residual_p95_us: u64) -> RawClockHealth {
        RawClockHealth {
            healthy: true,
            sample_count: 8,
            window_duration_us: 10_000,
            drift_ppm,
            residual_p50_us: residual_p95_us / 2,
            residual_p95_us,
            residual_p99_us: residual_p95_us,
            residual_max_us: residual_p95_us,
            sample_gap_max_us: 1_000,
            last_sample_age_us: 100,
            raw_timestamp_regressions: 0,
            reason: None,
        }
    }

    fn sample_calibration() -> DualArmCalibration {
        DualArmCalibration {
            master_zero: JointArray::splat(Rad(0.0)),
            slave_zero: JointArray::splat(Rad(0.0)),
            map: JointMirrorMap {
                permutation: piper_client::types::Joint::ALL,
                position_sign: [1.0; 6],
                velocity_sign: [1.0; 6],
                torque_sign: [1.0; 6],
            },
        }
    }

    fn sample_snapshot(mismatch: bool) -> DualArmSnapshot {
        let master = JointArray::splat(Rad(0.0));
        let slave = if mismatch {
            JointArray::splat(Rad(1.0))
        } else {
            master
        };

        DualArmSnapshot {
            left: control_snapshot(master),
            right: control_snapshot(slave),
            inter_arm_skew: Duration::ZERO,
            host_cycle_timestamp: Instant::now(),
        }
    }

    fn control_snapshot(position: JointArray<Rad>) -> ControlSnapshotFull {
        ControlSnapshotFull {
            state: ControlSnapshot {
                position,
                velocity: JointArray::splat(RadPerSecond(0.0)),
                torque: JointArray::splat(NewtonMeter::ZERO),
                position_timestamp_us: 0,
                dynamic_timestamp_us: 0,
                skew_us: 0,
            },
            position_host_rx_mono_us: 0,
            dynamic_host_rx_mono_us: 0,
            feedback_age: Duration::ZERO,
        }
    }
}
