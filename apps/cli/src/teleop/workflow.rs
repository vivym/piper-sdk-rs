use anyhow::{Context, Result, bail};
use piper_client::dual_arm::{
    BilateralExitReason, BilateralLoopConfig, BilateralRunReport, DualArmActiveMit, DualArmBuilder,
    DualArmCalibration, DualArmLoopExit, DualArmReadPolicy, DualArmSnapshot, DualArmStandby,
    JointMirrorMap, StopAttemptResult,
};
use piper_client::dual_arm_raw_clock::{
    ExperimentalRawClockConfig, ExperimentalRawClockDualArmActive,
    ExperimentalRawClockDualArmStandby, ExperimentalRawClockMasterFollowerGains,
    ExperimentalRawClockMode, ExperimentalRawClockRunConfig,
    ExperimentalRawClockRunExit as PiperRawClockRunExit, RawClockRuntimeExitReason,
    RawClockRuntimeReport, RawClockRuntimeThresholds, RawClockSide,
};
use piper_client::observer::{ControlReadPolicy, ControlSnapshotFull, Observer};
use piper_client::state::{DisableConfig, MitModeConfig, Piper, SoftRealtime, Standby};
use piper_client::types::{JointArray, NewtonMeter, Rad, RadPerSecond, RobotError};
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
    ResolvedTeleopConfig, TeleopConfigFile, TeleopJointMap, TeleopMode, TeleopProfile,
    TeleopRawClockSettings,
};
use crate::teleop::controller::{
    RuntimeTeleopController, RuntimeTeleopSettings, RuntimeTeleopSettingsHandle,
};
use crate::teleop::report::{
    ReportCalibration, ReportJointMotion, ReportTiming, TeleopExitStatus, TeleopJsonReport,
    TeleopReportInput, classify_exit, print_human_report,
};
use crate::teleop::target::{
    ConcreteTeleopTarget, RoleTargets, TeleopPlatform, resolve_role_targets,
};

const CALIBRATED_HW_RAW_TIMING_SOURCE: &str = "calibrated_hw_raw";
const EXPERIMENTAL_PRE_ENABLE_SOFT_SNAPSHOT_READY_TIMEOUT: Duration = Duration::from_secs(2);
const EXPERIMENTAL_ACTIVE_SOFT_SNAPSHOT_READY_TIMEOUT: Duration = Duration::from_secs(2);
const EXPERIMENTAL_SOFT_SNAPSHOT_POLL_INTERVAL: Duration = Duration::from_millis(5);

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
        mode: TeleopMode,
        frequency_hz: f64,
        max_iterations: Option<usize>,
        cancel_signal: Arc<AtomicBool>,
    ) -> Result<RawClockWarmupSummary>;

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
    fn startup_status(&mut self, stage: StartupStage) -> Result<()>;
    fn cancel_requested(&self) -> bool;
    fn start_console(
        &mut self,
        settings: RuntimeTeleopSettingsHandle,
        started_at: Instant,
    ) -> Result<Option<JoinHandle<Result<()>>>>;
    fn write_json_report(&mut self, path: &Path, report: &TeleopJsonReport) -> Result<()>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartupStage {
    RefreshingRawClock { warmup_secs: u64 },
    CheckingPreEnablePosture,
    EnablingMitPassthrough,
    ReadingActiveSnapshot,
    CapturingActiveZeroCalibration,
    CheckingPostEnablePosture,
    StartingControlLoop,
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
    pub calibration_joint_map: Option<TeleopJointMap>,
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
            calibration_joint_map: Some(TeleopJointMap::LeftRightMirror),
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
            let calibration = TeleopBackend::capture_calibration(
                backend,
                resolved.calibration.joint_map.to_joint_mirror_map(),
            )?;
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
        calibration_joint_map: (report_calibration.source == "captured")
            .then_some(resolved.calibration.joint_map),
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
        joint_motion: None,
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
    match backend.warmup_raw_clock(
        &resolved.raw_clock,
        resolved.control.mode,
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
    let summary_calibration_source = if loaded_calibration.is_some() {
        "file"
    } else {
        "captured"
    };
    let summary_calibration_path =
        resolved.calibration.file.clone().or_else(|| args.save_calibration.clone());
    let summary_calibration_joint_map =
        loaded_calibration.is_none().then_some(resolved.calibration.joint_map);

    let summary = StartupSummary {
        targets: targets.clone(),
        mode: resolved.control.mode,
        profile: resolved.safety.profile,
        frequency_hz: resolved.control.frequency_hz,
        track_kp: resolved.control.track_kp,
        track_kd: resolved.control.track_kd,
        master_damping: resolved.control.master_damping,
        reflection_gain: resolved.control.reflection_gain,
        calibration_source: summary_calibration_source.to_string(),
        calibration_path: summary_calibration_path,
        calibration_joint_map: summary_calibration_joint_map,
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

    io.startup_status(StartupStage::RefreshingRawClock {
        warmup_secs: resolved.raw_clock.warmup_secs,
    })?;
    let warmup = match backend.warmup_raw_clock(
        &resolved.raw_clock,
        resolved.control.mode,
        resolved.control.frequency_hz,
        resolved.max_iterations,
        cancel_signal.clone(),
    ) {
        Ok(summary) => summary,
        Err(error) if io.cancel_requested() || cancel_signal.load(Ordering::SeqCst) => {
            return Ok(TeleopExitStatus::Success);
        },
        Err(error) => return Err(error).context("post-confirmation raw-clock refresh failed"),
    };

    if let Some((_, calibration)) = &loaded_calibration {
        io.startup_status(StartupStage::CheckingPreEnablePosture)?;
        let standby_snapshot = ExperimentalRawClockTeleopBackend::standby_snapshot(
            backend,
            experimental_raw_clock_dual_arm_read_policy(&resolved.raw_clock),
        )?;
        check_snapshot_posture(
            calibration,
            &standby_snapshot,
            resolved.calibration.max_error_rad,
        )
        .context("pre-enable posture compatibility check failed")?;
    }

    if io.cancel_requested() || cancel_signal.load(Ordering::SeqCst) {
        return Ok(TeleopExitStatus::Success);
    }

    io.startup_status(StartupStage::EnablingMitPassthrough)?;
    backend.enable_mit_passthrough(MitModeConfig::default(), MitModeConfig::default())?;

    if io.cancel_requested() || cancel_signal.load(Ordering::SeqCst) {
        backend
            .disable_active_passthrough()
            .context("failed to disable experimental raw-clock teleop after cancellation")?;
        return Ok(TeleopExitStatus::Success);
    }

    io.startup_status(StartupStage::ReadingActiveSnapshot)?;
    let active_snapshot = match ExperimentalRawClockTeleopBackend::active_snapshot(
        backend,
        experimental_raw_clock_dual_arm_read_policy(&resolved.raw_clock),
    ) {
        Ok(snapshot) => snapshot,
        Err(error) => {
            return disable_experimental_after_error(backend, "active snapshot read failed", error);
        },
    };
    let (calibration, report_calibration) = match loaded_calibration {
        Some((file, calibration)) => {
            io.startup_status(StartupStage::CheckingPostEnablePosture)?;
            if let Err(error) = check_snapshot_posture(
                &calibration,
                &active_snapshot,
                resolved.calibration.max_error_rad,
            ) {
                backend.disable_active_passthrough().context(
                    "failed to disable experimental raw-clock teleop after posture mismatch",
                )?;
                return Err(error).context("post-enable posture compatibility check failed");
            }
            (
                calibration,
                ReportCalibration {
                    source: "file".to_string(),
                    path: resolved.calibration.file.as_ref().map(|path| path.display().to_string()),
                    created_at_unix_ms: Some(file.created_at_unix_ms),
                    max_error_rad: resolved.calibration.max_error_rad,
                },
            )
        },
        None => {
            io.startup_status(StartupStage::CapturingActiveZeroCalibration)?;
            let calibration = calibration_from_snapshot(
                &active_snapshot,
                resolved.calibration.joint_map.to_joint_mirror_map(),
            );
            let created_at_unix_ms = current_unix_ms();
            if let Some(path) = &args.save_calibration
                && let Err(error) =
                    CalibrationFile::from_calibration(&calibration, None, created_at_unix_ms)
                        .save_new(path)
            {
                return disable_experimental_after_error(
                    backend,
                    "failed to save experimental raw-clock calibration",
                    error,
                );
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

    let settings = match (|| {
        RuntimeTeleopSettings::production(calibration.clone())
            .with_mode(resolved.control.mode)?
            .with_track_gains(resolved.control.track_kp, resolved.control.track_kd)?
            .with_master_damping(resolved.control.master_damping)?
            .with_reflection_gain(resolved.control.reflection_gain)
    })() {
        Ok(settings) => settings,
        Err(error) => {
            return disable_experimental_after_error(
                backend,
                "failed to build runtime teleop settings",
                error,
            );
        },
    };
    let settings_handle = match RuntimeTeleopSettingsHandle::new(settings) {
        Ok(handle) => handle,
        Err(error) => {
            return disable_experimental_after_error(
                backend,
                "failed to initialize runtime teleop settings",
                error,
            );
        },
    };
    let initial_mode = settings_handle.snapshot().mode;

    let started_at = Instant::now();
    let console_handle = None;

    io.startup_status(StartupStage::StartingControlLoop)?;
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
        joint_motion: report_joint_motion_from_raw_clock(&loop_exit.report),
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
    let alignment_diagnostics_known = runtime_has_alignment_evidence(report);
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
        alignment_lag_us: alignment_diagnostics_known.then_some(report.alignment_lag_us),
        latest_inter_arm_skew_max_us: alignment_diagnostics_known
            .then_some(report.latest_inter_arm_skew_max_us),
        latest_inter_arm_skew_p95_us: alignment_diagnostics_known
            .then_some(report.latest_inter_arm_skew_p95_us),
        selected_inter_arm_skew_max_us: alignment_diagnostics_known
            .then_some(report.selected_inter_arm_skew_max_us),
        selected_inter_arm_skew_p95_us: alignment_diagnostics_known
            .then_some(report.selected_inter_arm_skew_p95_us),
        alignment_buffer_misses: report.alignment_buffer_misses,
        alignment_buffer_miss_consecutive_max: report.alignment_buffer_miss_consecutive_max,
        alignment_buffer_miss_consecutive_failures: report
            .alignment_buffer_miss_consecutive_failures,
        master_residual_max_spikes: report.master_residual_max_spikes,
        slave_residual_max_spikes: report.slave_residual_max_spikes,
        master_residual_max_consecutive_failures: report.master_residual_max_consecutive_failures,
        slave_residual_max_consecutive_failures: report.slave_residual_max_consecutive_failures,
    }
}

fn report_joint_motion_from_raw_clock(report: &RawClockRuntimeReport) -> Option<ReportJointMotion> {
    let stats = report.joint_motion?;
    Some(ReportJointMotion {
        master_feedback_min_rad: stats.master_feedback_min_rad,
        master_feedback_max_rad: stats.master_feedback_max_rad,
        master_feedback_delta_rad: stats.master_feedback_delta_rad,
        slave_command_min_rad: stats.slave_command_min_rad,
        slave_command_max_rad: stats.slave_command_max_rad,
        slave_command_delta_rad: stats.slave_command_delta_rad,
        slave_feedback_min_rad: stats.slave_feedback_min_rad,
        slave_feedback_max_rad: stats.slave_feedback_max_rad,
        slave_feedback_delta_rad: stats.slave_feedback_delta_rad,
    })
}

fn bilateral_report_from_raw_clock(report: &RawClockRuntimeReport) -> BilateralRunReport {
    BilateralRunReport {
        iterations: report.iterations,
        read_faults: report.read_faults,
        submission_faults: report.submission_faults,
        last_submission_failed_arm: report
            .last_submission_failed_side
            .map(map_raw_clock_submission_side),
        peer_command_may_have_applied: report.peer_command_may_have_applied,
        max_inter_arm_skew: Duration::from_micros(report.max_inter_arm_skew_us),
        left_tx_realtime_overwrites_total: report.master_tx_realtime_overwrites_total,
        right_tx_realtime_overwrites_total: report.slave_tx_realtime_overwrites_total,
        left_tx_frames_sent_total: report.master_tx_frames_sent_total,
        right_tx_frames_sent_total: report.slave_tx_frames_sent_total,
        left_tx_fault_aborts_total: report.master_tx_fault_aborts_total,
        right_tx_fault_aborts_total: report.slave_tx_fault_aborts_total,
        last_runtime_fault_left: report.last_runtime_fault_master,
        last_runtime_fault_right: report.last_runtime_fault_slave,
        exit_reason: report.exit_reason.map(map_raw_clock_exit_reason),
        left_stop_attempt: report.master_stop_attempt,
        right_stop_attempt: report.slave_stop_attempt,
        last_error: report.last_error.clone(),
        ..BilateralRunReport::default()
    }
}

fn map_raw_clock_submission_side(side: RawClockSide) -> piper_client::dual_arm::SubmissionArm {
    match side {
        RawClockSide::Master => piper_client::dual_arm::SubmissionArm::Left,
        RawClockSide::Slave => piper_client::dual_arm::SubmissionArm::Right,
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

fn runtime_has_alignment_evidence(report: &RawClockRuntimeReport) -> bool {
    report.iterations > 0
        || report.latest_inter_arm_skew_max_us > 0
        || report.latest_inter_arm_skew_p95_us > 0
        || report.selected_inter_arm_skew_max_us > 0
        || report.selected_inter_arm_skew_p95_us > 0
        || report.alignment_buffer_misses > 0
        || report.alignment_buffer_miss_consecutive_max > 0
        || report.alignment_buffer_miss_consecutive_failures > 0
}

fn max_optional_u64(left: Option<u64>, right: Option<u64>) -> Option<u64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.max(right)),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

fn experimental_raw_clock_mode_from_teleop_mode(mode: TeleopMode) -> ExperimentalRawClockMode {
    match mode {
        TeleopMode::MasterFollower => ExperimentalRawClockMode::MasterFollower,
        TeleopMode::Bilateral => ExperimentalRawClockMode::Bilateral,
    }
}

fn experimental_raw_clock_config_from_settings(
    raw_clock: &TeleopRawClockSettings,
    mode: TeleopMode,
    frequency_hz: f64,
    max_iterations: Option<usize>,
) -> ExperimentalRawClockConfig {
    let estimator_thresholds = RawClockThresholds {
        warmup_samples: raw_clock_warmup_sample_threshold(raw_clock),
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
        selected_sample_age_us: raw_clock.selected_sample_age_ms * 1_000,
        residual_max_consecutive_failures: raw_clock.residual_max_consecutive_failures,
        alignment_lag_us: raw_clock.alignment_lag_us,
        alignment_search_window_us: raw_clock.alignment_search_window_us,
        alignment_buffer_miss_consecutive_failures: raw_clock
            .alignment_buffer_miss_consecutive_failures,
    };

    ExperimentalRawClockConfig {
        mode: experimental_raw_clock_mode_from_teleop_mode(mode),
        frequency_hz,
        max_iterations,
        thresholds,
        estimator_thresholds,
    }
}

fn raw_clock_warmup_sample_threshold(raw_clock: &TeleopRawClockSettings) -> usize {
    let warmup_ms = raw_clock.warmup_secs.saturating_mul(1_000);
    let sample_gap_max_ms = raw_clock.sample_gap_max_ms.max(1);
    let samples = warmup_ms.saturating_add(sample_gap_max_ms - 1) / sample_gap_max_ms;

    usize::try_from(samples.max(4)).unwrap_or(usize::MAX)
}

fn experimental_raw_clock_control_read_policy(
    raw_clock: &TeleopRawClockSettings,
) -> ControlReadPolicy {
    ControlReadPolicy {
        max_state_skew_us: raw_clock.state_skew_max_us,
        max_feedback_age: Duration::from_millis(raw_clock.last_sample_age_ms),
    }
}

fn experimental_raw_clock_dual_arm_read_policy(
    raw_clock: &TeleopRawClockSettings,
) -> DualArmReadPolicy {
    DualArmReadPolicy {
        per_arm: experimental_raw_clock_control_read_policy(raw_clock),
        max_inter_arm_skew: DualArmReadPolicy::default().max_inter_arm_skew,
    }
}

fn wait_for_experimental_soft_snapshot_ready<Read>(
    phase: &'static str,
    timeout: Duration,
    poll_interval: Duration,
    mut read: Read,
) -> Result<DualArmSnapshot>
where
    Read: FnMut() -> Result<DualArmSnapshot>,
{
    let start = Instant::now();

    loop {
        match read() {
            Ok(snapshot) => return Ok(snapshot),
            Err(error) if is_retryable_experimental_soft_snapshot_error(&error) => {
                if start.elapsed() >= timeout {
                    return Err(error).with_context(|| {
                        format!(
                            "experimental raw-clock {phase} snapshot not ready after {}ms",
                            timeout.as_millis()
                        )
                    });
                }

                let remaining = timeout.saturating_sub(start.elapsed());
                let sleep_duration = poll_interval.min(remaining);
                if sleep_duration.is_zero() {
                    return Err(error).with_context(|| {
                        format!(
                            "experimental raw-clock {phase} snapshot not ready after {}ms",
                            timeout.as_millis()
                        )
                    });
                }

                std::thread::sleep(sleep_duration);
            },
            Err(error) => return Err(error),
        }
    }
}

fn is_retryable_experimental_soft_snapshot_error(error: &anyhow::Error) -> bool {
    matches!(
        error.downcast_ref::<RobotError>(),
        Some(RobotError::ControlStateIncomplete { .. } | RobotError::StateMisaligned { .. })
    )
}

fn experimental_raw_clock_master_follower_gains(
    settings: &RuntimeTeleopSettings,
) -> ExperimentalRawClockMasterFollowerGains {
    ExperimentalRawClockMasterFollowerGains {
        track_kp: JointArray::splat(settings.track_kp),
        track_kd: JointArray::splat(settings.track_kd),
        master_damping: JointArray::splat(settings.master_damping),
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

fn calibration_from_snapshot(
    snapshot: &DualArmSnapshot,
    map: JointMirrorMap,
) -> DualArmCalibration {
    DualArmCalibration {
        master_zero: snapshot.left.state.position,
        slave_zero: snapshot.right.state.position,
        map,
    }
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
    experimental_phase: ExperimentalBackendPhase,
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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum ExperimentalBackendPhase {
    #[default]
    Disconnected,
    PendingWarmup,
    WarmedStandby,
    Active,
    TerminalError,
}

impl ExperimentalBackendPhase {
    const fn label(self) -> &'static str {
        match self {
            Self::Disconnected => "disconnected",
            Self::PendingWarmup => "pending warmup",
            Self::WarmedStandby => "warmed standby",
            Self::Active => "active",
            Self::TerminalError => "terminal error",
        }
    }
}

impl RealTeleopBackend {
    fn clear_experimental_state(&mut self, phase: ExperimentalBackendPhase) {
        self.experimental_pending = None;
        self.experimental_standby = None;
        self.experimental_active = None;
        self.experimental_observers = None;
        self.experimental_phase = phase;
    }

    fn mark_experimental_terminal(&mut self) {
        self.clear_experimental_state(ExperimentalBackendPhase::TerminalError);
    }

    fn require_experimental_phase(
        &self,
        expected: ExperimentalBackendPhase,
        expected_label: &'static str,
    ) -> Result<()> {
        if self.experimental_phase == expected {
            return Ok(());
        }
        bail!(
            "experimental raw-clock backend is not in {expected_label}; current phase is {}",
            self.experimental_phase.label()
        );
    }
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
        self.clear_experimental_state(ExperimentalBackendPhase::Disconnected);
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
        self.clear_experimental_state(ExperimentalBackendPhase::Disconnected);
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
        self.experimental_phase = ExperimentalBackendPhase::PendingWarmup;
        Ok(())
    }

    fn warmup_raw_clock(
        &mut self,
        settings: &TeleopRawClockSettings,
        mode: TeleopMode,
        frequency_hz: f64,
        max_iterations: Option<usize>,
        cancel_signal: Arc<AtomicBool>,
    ) -> Result<RawClockWarmupSummary> {
        let config = experimental_raw_clock_config_from_settings(
            settings,
            mode,
            frequency_hz,
            max_iterations,
        );
        config.validate()?;
        if !matches!(
            self.experimental_phase,
            ExperimentalBackendPhase::PendingWarmup | ExperimentalBackendPhase::WarmedStandby
        ) {
            bail!(
                "experimental raw-clock backend is not in pending warmup or warmed standby; current phase is {}",
                self.experimental_phase.label()
            );
        }
        let standby = match self.experimental_standby.take() {
            Some(standby) => standby,
            None => {
                let arms = self
                    .experimental_pending
                    .take()
                    .context("experimental raw-clock backend is not connected")?;
                match ExperimentalRawClockDualArmStandby::new(arms.master, arms.slave, config) {
                    Ok(standby) => standby,
                    Err(error) => {
                        self.mark_experimental_terminal();
                        return Err(error.into());
                    },
                }
            },
        };

        let warmed = match standby.warmup(
            experimental_raw_clock_control_read_policy(settings),
            Duration::from_secs(settings.warmup_secs),
            cancel_signal.as_ref(),
        ) {
            Ok(warmed) => warmed,
            Err(error) => {
                self.mark_experimental_terminal();
                return Err(error.into());
            },
        };
        self.experimental_standby = Some(warmed);
        self.experimental_phase = ExperimentalBackendPhase::WarmedStandby;

        Ok(RawClockWarmupSummary::default())
    }

    fn standby_snapshot(&self, policy: DualArmReadPolicy) -> Result<DualArmSnapshot> {
        self.require_experimental_phase(ExperimentalBackendPhase::WarmedStandby, "warmed standby")?;
        self.experimental_standby
            .as_ref()
            .context("experimental raw-clock backend is not in warmed standby")?;
        let observers = self
            .experimental_observers
            .as_ref()
            .context("experimental raw-clock backend is not connected")?;
        wait_for_experimental_soft_snapshot_ready(
            "pre-enable standby",
            EXPERIMENTAL_PRE_ENABLE_SOFT_SNAPSHOT_READY_TIMEOUT,
            EXPERIMENTAL_SOFT_SNAPSHOT_POLL_INTERVAL,
            || experimental_soft_snapshot(observers, policy),
        )
    }

    fn enable_mit_passthrough(
        &mut self,
        master: MitModeConfig,
        slave: MitModeConfig,
    ) -> Result<()> {
        self.require_experimental_phase(ExperimentalBackendPhase::WarmedStandby, "warmed standby")?;
        let standby = self
            .experimental_standby
            .take()
            .context("experimental raw-clock backend is not in warmed standby")?;
        let active = match standby.enable_mit_passthrough(master, slave) {
            Ok(active) => active,
            Err(error) => {
                self.mark_experimental_terminal();
                return Err(error.into());
            },
        };
        self.experimental_active = Some(active);
        self.experimental_phase = ExperimentalBackendPhase::Active;
        Ok(())
    }

    fn active_snapshot(&self, policy: DualArmReadPolicy) -> Result<DualArmSnapshot> {
        self.require_experimental_phase(ExperimentalBackendPhase::Active, "active")?;
        self.experimental_active
            .as_ref()
            .context("experimental raw-clock backend is not active")?;
        let observers = self
            .experimental_observers
            .as_ref()
            .context("experimental raw-clock backend is not connected")?;
        wait_for_experimental_soft_snapshot_ready(
            "post-enable active",
            EXPERIMENTAL_ACTIVE_SOFT_SNAPSHOT_READY_TIMEOUT,
            EXPERIMENTAL_SOFT_SNAPSHOT_POLL_INTERVAL,
            || experimental_soft_snapshot(observers, policy),
        )
    }

    fn disable_active_passthrough(&mut self) -> Result<()> {
        self.require_experimental_phase(ExperimentalBackendPhase::Active, "active")?;
        let active = self
            .experimental_active
            .take()
            .context("experimental raw-clock backend is not active")?;
        let standby = match active.disable_both(DisableConfig::default()) {
            Ok(standby) => standby,
            Err(error) => {
                self.mark_experimental_terminal();
                return Err(error.into());
            },
        };
        self.experimental_standby = Some(standby);
        self.experimental_phase = ExperimentalBackendPhase::WarmedStandby;
        Ok(())
    }

    fn run_master_follower_raw_clock(
        &mut self,
        settings: RuntimeTeleopSettingsHandle,
        raw_clock: TeleopRawClockSettings,
        cancel_signal: Arc<AtomicBool>,
    ) -> Result<ExperimentalRawClockRunExit> {
        let runtime_settings = settings.snapshot();
        if runtime_settings.mode != TeleopMode::MasterFollower {
            bail!("experimental calibrated raw clock currently supports master-follower mode only");
        }

        self.require_experimental_phase(ExperimentalBackendPhase::Active, "active")?;
        let active = self
            .experimental_active
            .take()
            .context("experimental raw-clock backend is not active")?;
        let run_config = ExperimentalRawClockRunConfig {
            read_policy: experimental_raw_clock_control_read_policy(&raw_clock),
            command_timeout: Duration::from_millis(20),
            disable_config: DisableConfig::default(),
            cancel_signal: Some(cancel_signal),
        };

        let gains = experimental_raw_clock_master_follower_gains(&runtime_settings);
        match active.run_master_follower_with_gains(runtime_settings.calibration, gains, run_config)
        {
            Ok(PiperRawClockRunExit::Standby { arms, report }) => {
                self.experimental_standby = Some(*arms);
                self.experimental_phase = ExperimentalBackendPhase::WarmedStandby;
                Ok(ExperimentalRawClockRunExit {
                    faulted: false,
                    report,
                })
            },
            Ok(PiperRawClockRunExit::Faulted { arms: _, report }) => {
                self.mark_experimental_terminal();
                Ok(ExperimentalRawClockRunExit {
                    faulted: true,
                    report,
                })
            },
            Err(error) => {
                self.mark_experimental_terminal();
                Ok(ExperimentalRawClockRunExit {
                    faulted: true,
                    report: raw_clock_error_report(
                        RawClockRuntimeExitReason::RuntimeTransportFault,
                        error.to_string(),
                    ),
                })
            },
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
        let skew_us = i64::try_from(inter_arm_skew.as_micros()).unwrap_or(i64::MAX);
        let max_skew_us = u64::try_from(policy.max_inter_arm_skew.as_micros()).unwrap_or(u64::MAX);
        return Err(RobotError::state_misaligned(skew_us, max_skew_us)).with_context(|| {
            format!(
                "experimental raw-clock dual-arm snapshot inter-arm skew {}us exceeds {}us",
                inter_arm_skew.as_micros(),
                policy.max_inter_arm_skew.as_micros()
            )
        });
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
        return Err(RobotError::feedback_stale(
            health.last_feedback_age,
            policy.per_arm.max_feedback_age,
        )
        .into());
    }

    let position = observer.raw_joint_position_state();
    let dynamic = observer.raw_joint_dynamic_state();
    if !position.is_fully_valid() {
        return Err(RobotError::control_state_incomplete(
            position.frame_valid_mask,
            dynamic.valid_mask,
        ))
        .with_context(|| {
            format!(
                "experimental raw-clock {role} position feedback incomplete: mask=0x{:02x}",
                position.frame_valid_mask
            )
        });
    }
    if !dynamic.is_complete() {
        return Err(RobotError::control_state_incomplete(
            position.frame_valid_mask,
            dynamic.valid_mask,
        ))
        .with_context(|| {
            format!(
                "experimental raw-clock {role} dynamic feedback incomplete: mask=0x{:02x}",
                dynamic.valid_mask
            )
        });
    }

    let skew_us = signed_us_diff(dynamic.group_timestamp_us, position.hardware_timestamp_us);
    if position.hardware_timestamp_us.abs_diff(dynamic.group_timestamp_us)
        > policy.per_arm.max_state_skew_us
    {
        return Err(RobotError::state_misaligned(
            skew_us,
            policy.per_arm.max_state_skew_us,
        ))
        .with_context(|| {
            format!(
                "experimental raw-clock {role} state skew {}us exceeds {}us",
                skew_us, policy.per_arm.max_state_skew_us
            )
        });
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
        joint_motion: None,
        max_inter_arm_skew_us: 0,
        inter_arm_skew_p95_us: 0,
        alignment_lag_us: 0,
        latest_inter_arm_skew_max_us: 0,
        latest_inter_arm_skew_p95_us: 0,
        selected_inter_arm_skew_max_us: 0,
        selected_inter_arm_skew_p95_us: 0,
        clock_health_failures: 0,
        alignment_buffer_misses: 0,
        alignment_buffer_miss_consecutive_max: 0,
        alignment_buffer_miss_consecutive_failures: 0,
        master_residual_max_spikes: 0,
        slave_residual_max_spikes: 0,
        master_residual_max_consecutive_failures: 0,
        slave_residual_max_consecutive_failures: 0,
        read_faults: 0,
        submission_faults: 0,
        last_submission_failed_side: None,
        peer_command_may_have_applied: false,
        runtime_faults: 1,
        master_tx_realtime_overwrites_total: 0,
        slave_tx_realtime_overwrites_total: 0,
        master_tx_frames_sent_total: 0,
        slave_tx_frames_sent_total: 0,
        master_tx_fault_aborts_total: 0,
        slave_tx_fault_aborts_total: 0,
        last_runtime_fault_master: None,
        last_runtime_fault_slave: None,
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
        failure_kind: None,
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

    fn startup_status(&mut self, stage: StartupStage) -> Result<()> {
        print_startup_status(stage)
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
    write_startup_summary(io::stderr().lock(), summary)
}

fn print_startup_status(stage: StartupStage) -> Result<()> {
    writeln!(io::stderr().lock(), "{}", format_startup_status(stage))?;
    Ok(())
}

fn format_startup_status(stage: StartupStage) -> String {
    match stage {
        StartupStage::RefreshingRawClock { warmup_secs } => {
            format!("startup: refreshing raw-clock timing (~{warmup_secs}s)...")
        },
        StartupStage::CheckingPreEnablePosture => {
            "startup: checking pre-enable posture...".to_string()
        },
        StartupStage::EnablingMitPassthrough => "startup: enabling MIT passthrough...".to_string(),
        StartupStage::ReadingActiveSnapshot => "startup: reading active snapshot...".to_string(),
        StartupStage::CapturingActiveZeroCalibration => {
            "startup: capturing active-zero calibration...".to_string()
        },
        StartupStage::CheckingPostEnablePosture => {
            "startup: checking post-enable posture...".to_string()
        },
        StartupStage::StartingControlLoop => "startup: starting teleop control loop...".to_string(),
    }
}

fn write_startup_summary<W: Write>(mut writer: W, summary: &StartupSummary) -> Result<()> {
    writeln!(writer, "teleop dual-arm startup summary")?;
    writeln!(
        writer,
        "  master target: {}",
        format_target_for_operator(&summary.targets.master)
    )?;
    writeln!(
        writer,
        "  slave target: {}",
        format_target_for_operator(&summary.targets.slave)
    )?;
    writeln!(writer, "  mode: {}", format_mode_for_operator(summary.mode))?;
    writeln!(
        writer,
        "  profile: {}",
        format_profile_for_operator(summary.profile)
    )?;
    writeln!(writer, "  frequency: {:.1} Hz", summary.frequency_hz)?;
    writeln!(
        writer,
        "  gains: track_kp={:.3}, track_kd={:.3}, master_damping={:.3}, reflection_gain={:.3}",
        summary.track_kp, summary.track_kd, summary.master_damping, summary.reflection_gain
    )?;
    writeln!(writer, "  calibration: {}", summary.calibration_source)?;
    if let Some(path) = &summary.calibration_path {
        writeln!(writer, "  calibration path: {}", path.display())?;
    }
    if let Some(joint_map) = summary.calibration_joint_map {
        writeln!(
            writer,
            "  calibration joint map: {}",
            format_joint_map_for_operator(joint_map)
        )?;
    }
    writeln!(writer, "  gripper mirror: {}", summary.gripper_mirror)?;
    writeln!(writer, "  experimental={}", summary.experimental)?;
    writeln!(writer, "  strict_realtime={}", summary.strict_realtime)?;
    if let Some(timing_source) = &summary.timing_source {
        writeln!(writer, "  timing_source={timing_source}")?;
    }
    if let Some(path) = &summary.report_path {
        writeln!(writer, "  report json: {}", path.display())?;
    }
    if !summary.yes {
        write!(writer, "Type 'yes' or 'y' to enable MIT teleop: ")?;
        writer.flush()?;
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

fn format_joint_map_for_operator(joint_map: TeleopJointMap) -> &'static str {
    joint_map.as_str()
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
    use piper_client::RuntimeFaultKind;
    use piper_client::dual_arm::{
        BilateralExitReason, BilateralLoopConfig, BilateralRunReport, DualArmCalibration,
        DualArmReadPolicy, DualArmSnapshot, JointMirrorMap, StopAttemptResult, SubmissionArm,
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
        StartupStatus(StartupStage),
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
        experimental_run_settings: Arc<Mutex<Option<RuntimeTeleopSettings>>>,
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
                experimental_run_settings: Arc::new(Mutex::new(None)),
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

        fn experimental_run_settings(&self) -> Option<RuntimeTeleopSettings> {
            self.experimental_run_settings
                .lock()
                .expect("experimental settings lock poisoned")
                .clone()
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
            _mode: TeleopMode,
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
            settings: RuntimeTeleopSettingsHandle,
            _raw_clock: TeleopRawClockSettings,
            _cancel_signal: Arc<AtomicBool>,
        ) -> Result<ExperimentalRawClockRunExit> {
            self.trace.push(WorkflowCall::RunLoop);
            *self
                .experimental_run_settings
                .lock()
                .expect("experimental settings lock poisoned") = Some(settings.snapshot());
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
        console_switches_to_bilateral: bool,
        last_report: Arc<Mutex<Option<TeleopJsonReport>>>,
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
                console_switches_to_bilateral: false,
                last_report: Arc::new(Mutex::new(None)),
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

        fn console_switches_to_bilateral_with_trace(trace: WorkflowTrace) -> Self {
            Self {
                trace,
                console_switches_to_bilateral: true,
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

        fn startup_status(&mut self, stage: StartupStage) -> Result<()> {
            self.trace.push(WorkflowCall::StartupStatus(stage));
            Ok(())
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
            settings: crate::teleop::controller::RuntimeTeleopSettingsHandle,
            _started_at: Instant,
        ) -> Result<Option<JoinHandle<Result<()>>>> {
            self.trace.push(WorkflowCall::StartConsole);
            if self.console_start_error {
                bail!("console failed to start");
            }
            if self.console_switches_to_bilateral {
                settings.update_mode(TeleopMode::Bilateral)?;
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
            report: &crate::teleop::report::TeleopJsonReport,
        ) -> Result<()> {
            self.trace.push(WorkflowCall::WriteReport);
            *self.last_report.lock().expect("last report lock poisoned") = Some(report.clone());
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
        assert!(!trace.calls().contains(&WorkflowCall::StartConsole));
        assert_call_order(
            trace.calls(),
            &[
                WorkflowCall::Connect,
                WorkflowCall::RawClockWarmup,
                WorkflowCall::ConfirmStart,
                WorkflowCall::RawClockWarmup,
                WorkflowCall::Enable,
                WorkflowCall::RunLoop,
            ],
        );
    }

    #[test]
    fn experimental_raw_clock_captures_calibration_from_active_snapshot_after_enable() {
        let temp = tempfile::tempdir().unwrap();
        let save_path = temp.path().join("calibration.toml");
        let trace = WorkflowTrace::default();
        let backend = FakeTeleopBackend {
            trace: trace.clone(),
            active_mismatch: true,
            ..FakeTeleopBackend::default()
        };
        let io = FakeTeleopIo {
            trace: trace.clone(),
            ..FakeTeleopIo::default()
        };
        let args = TeleopDualArmArgs {
            save_calibration: Some(save_path.clone()),
            ..experimental_args()
        };

        let status = run_workflow_for_test_with_io(args, backend, io)
            .expect("experimental raw-clock workflow should succeed");

        assert_eq!(status, TeleopExitStatus::Success);
        assert_call_order(
            trace.calls(),
            &[
                WorkflowCall::Connect,
                WorkflowCall::RawClockWarmup,
                WorkflowCall::ConfirmStart,
                WorkflowCall::RawClockWarmup,
                WorkflowCall::Enable,
                WorkflowCall::ActiveSnapshot,
                WorkflowCall::RunLoop,
            ],
        );
        assert!(!trace.calls().contains(&WorkflowCall::CaptureCalibration));
        assert!(!trace.calls().contains(&WorkflowCall::StandbySnapshot));

        let saved = CalibrationFile::load(&save_path).expect("calibration should be saved");
        assert_eq!(saved.zero.master, [0.0; 6]);
        assert_eq!(saved.zero.slave, [1.0; 6]);
    }

    #[test]
    fn experimental_raw_clock_captured_calibration_uses_configured_joint_map() {
        let temp = tempfile::tempdir().unwrap();
        let save_path = temp.path().join("calibration.toml");
        let args = TeleopDualArmArgs {
            save_calibration: Some(save_path.clone()),
            joint_map: Some(TeleopJointMap::Identity),
            ..experimental_args()
        };

        let status = run_workflow_for_test(args, FakeTeleopBackend::default())
            .expect("experimental raw-clock workflow should succeed");

        assert_eq!(status, TeleopExitStatus::Success);
        let saved = CalibrationFile::load(&save_path).expect("calibration should be saved");
        assert_eq!(saved.map.position_sign, [1.0; 6]);
        assert_eq!(saved.map.velocity_sign, [1.0; 6]);
        assert_eq!(saved.map.torque_sign, [1.0; 6]);
    }

    #[test]
    fn experimental_raw_clock_reports_post_confirmation_startup_stages() {
        let trace = WorkflowTrace::default();
        let backend =
            FakeTeleopBackend::with_trace(trace.clone()).with_experimental_raw_clock_success();
        let io = FakeTeleopIo {
            trace: trace.clone(),
            ..FakeTeleopIo::default()
        };

        let status = run_workflow_for_test_with_io(experimental_args(), backend, io)
            .expect("experimental raw-clock workflow should succeed");

        assert_eq!(status, TeleopExitStatus::Success);
        assert_call_order(
            trace.calls(),
            &[
                WorkflowCall::ConfirmStart,
                WorkflowCall::StartupStatus(StartupStage::RefreshingRawClock { warmup_secs: 10 }),
                WorkflowCall::RawClockWarmup,
                WorkflowCall::StartupStatus(StartupStage::EnablingMitPassthrough),
                WorkflowCall::Enable,
                WorkflowCall::StartupStatus(StartupStage::ReadingActiveSnapshot),
                WorkflowCall::ActiveSnapshot,
                WorkflowCall::StartupStatus(StartupStage::CapturingActiveZeroCalibration),
                WorkflowCall::StartupStatus(StartupStage::StartingControlLoop),
                WorkflowCall::RunLoop,
            ],
        );
    }

    #[test]
    fn experimental_raw_clock_run_receives_initial_master_follower_gains() {
        let backend = FakeTeleopBackend::default().with_experimental_raw_clock_success();
        let args = TeleopDualArmArgs {
            track_kp: Some(9.25),
            track_kd: Some(1.75),
            master_damping: Some(0.65),
            ..experimental_args()
        };

        let status = run_workflow_for_test(args, backend.clone())
            .expect("experimental raw-clock workflow should succeed");

        assert_eq!(status, TeleopExitStatus::Success);
        let settings = backend
            .experimental_run_settings()
            .expect("raw-clock run should receive runtime settings");
        assert_eq!(settings.mode, TeleopMode::MasterFollower);
        assert_eq!(settings.track_kp, 9.25);
        assert_eq!(settings.track_kd, 1.75);
        assert_eq!(settings.master_damping, 0.65);
    }

    #[test]
    fn experimental_raw_clock_does_not_report_console_bilateral_mode_update() {
        let trace = WorkflowTrace::default();
        let backend =
            FakeTeleopBackend::with_trace(trace.clone()).with_experimental_raw_clock_success();
        let io = FakeTeleopIo::console_switches_to_bilateral_with_trace(trace.clone());
        let report_slot = io.last_report.clone();
        let args = TeleopDualArmArgs {
            report_json: Some(PathBuf::from("teleop-report.json")),
            ..experimental_args()
        };

        let status = run_workflow_for_test_with_io(args, backend, io)
            .expect("experimental raw-clock workflow should succeed");

        assert_eq!(status, TeleopExitStatus::Success);
        assert!(!trace.calls().contains(&WorkflowCall::StartConsole));
        let report = report_slot
            .lock()
            .expect("last report lock poisoned")
            .clone()
            .expect("report should be written");
        assert_eq!(report.mode.initial, "master_follower");
        assert_eq!(report.mode.final_, "master_follower");
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
        let mut output = Vec::new();

        write_startup_summary(&mut output, &summary).expect("startup summary should render");
        let output = String::from_utf8(output).expect("startup summary should be UTF-8");

        assert!(output.contains("master target: socketcan:can0"));
        assert!(output.contains("slave target: socketcan:can1"));
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
    fn experimental_raw_clock_loaded_calibration_pre_enable_posture_mismatch_fails_before_enable() {
        let temp = tempfile::tempdir().unwrap();
        let cal_path = temp.path().join("calibration.toml");
        write_sample_calibration_file(&cal_path);
        let backend = FakeTeleopBackend::with_standby_snapshot_mismatch();

        let err = run_workflow_for_test(
            experimental_args_with_calibration(&cal_path),
            backend.clone(),
        )
        .expect_err("pre-enable mismatch should stop experimental raw-clock workflow");

        assert!(err.to_string().contains("posture"));
        assert!(backend.calls().contains(&WorkflowCall::StandbySnapshot));
        assert!(!backend.calls().contains(&WorkflowCall::Enable));
        assert!(!backend.calls().contains(&WorkflowCall::RunLoop));
    }

    #[test]
    fn experimental_raw_clock_loaded_calibration_post_enable_posture_mismatch_disables_without_run()
    {
        let temp = tempfile::tempdir().unwrap();
        let cal_path = temp.path().join("calibration.toml");
        write_sample_calibration_file(&cal_path);
        let backend = FakeTeleopBackend::with_active_snapshot_mismatch();

        let err = run_workflow_for_test(
            experimental_args_with_calibration(&cal_path),
            backend.clone(),
        )
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
    fn experimental_raw_clock_loaded_calibration_reports_posture_startup_stages() {
        let temp = tempfile::tempdir().unwrap();
        let cal_path = temp.path().join("calibration.toml");
        write_sample_calibration_file(&cal_path);
        let trace = WorkflowTrace::default();
        let backend =
            FakeTeleopBackend::with_trace(trace.clone()).with_experimental_raw_clock_success();
        let io = FakeTeleopIo {
            trace: trace.clone(),
            ..FakeTeleopIo::default()
        };

        let status = run_workflow_for_test_with_io(
            experimental_args_with_calibration(&cal_path),
            backend,
            io,
        )
        .expect("loaded calibration raw-clock workflow should succeed");

        assert_eq!(status, TeleopExitStatus::Success);
        assert_call_order(
            trace.calls(),
            &[
                WorkflowCall::StartupStatus(StartupStage::RefreshingRawClock { warmup_secs: 10 }),
                WorkflowCall::StartupStatus(StartupStage::CheckingPreEnablePosture),
                WorkflowCall::StandbySnapshot,
                WorkflowCall::StartupStatus(StartupStage::EnablingMitPassthrough),
                WorkflowCall::Enable,
                WorkflowCall::StartupStatus(StartupStage::ReadingActiveSnapshot),
                WorkflowCall::ActiveSnapshot,
                WorkflowCall::StartupStatus(StartupStage::CheckingPostEnablePosture),
                WorkflowCall::StartupStatus(StartupStage::StartingControlLoop),
                WorkflowCall::RunLoop,
            ],
        );
    }

    #[test]
    fn real_experimental_standby_snapshot_requires_warmed_standby_phase() {
        let backend = RealTeleopBackend {
            experimental_phase: ExperimentalBackendPhase::PendingWarmup,
            ..RealTeleopBackend::default()
        };

        let err = ExperimentalRawClockTeleopBackend::standby_snapshot(
            &backend,
            DualArmReadPolicy::default(),
        )
        .expect_err("pending warmup phase should reject standby snapshots");

        assert!(err.to_string().contains("warmed standby"));
    }

    #[test]
    fn real_experimental_active_snapshot_requires_active_phase() {
        let backend = RealTeleopBackend {
            experimental_phase: ExperimentalBackendPhase::WarmedStandby,
            ..RealTeleopBackend::default()
        };

        let err = ExperimentalRawClockTeleopBackend::active_snapshot(
            &backend,
            DualArmReadPolicy::default(),
        )
        .expect_err("warmed standby phase should reject active snapshots");

        assert!(err.to_string().contains("active"));
    }

    #[test]
    fn raw_clock_runtime_error_report_does_not_increment_clock_health_failures() {
        let report = raw_clock_error_report(
            RawClockRuntimeExitReason::RuntimeTransportFault,
            "runtime API returned before report".to_string(),
        );

        assert_eq!(report.clock_health_failures, 0);
        assert_eq!(report.runtime_faults, 1);
        assert_eq!(
            report.exit_reason,
            Some(RawClockRuntimeExitReason::RuntimeTransportFault)
        );
        assert_eq!(
            report.last_error.as_deref(),
            Some("runtime API returned before report")
        );
    }

    #[test]
    fn raw_clock_error_report_timing_keeps_alignment_diagnostics_unknown() {
        let report = raw_clock_error_report(
            RawClockRuntimeExitReason::RuntimeTransportFault,
            "runtime API returned before report".to_string(),
        );

        let timing = report_timing_from_raw_clock(&RawClockWarmupSummary::default(), &report);

        assert_eq!(timing.alignment_lag_us, None);
        assert_eq!(timing.latest_inter_arm_skew_max_us, None);
        assert_eq!(timing.latest_inter_arm_skew_p95_us, None);
        assert_eq!(timing.selected_inter_arm_skew_max_us, None);
        assert_eq!(timing.selected_inter_arm_skew_p95_us, None);
        assert_eq!(timing.alignment_buffer_misses, 0);
        assert_eq!(timing.alignment_buffer_miss_consecutive_failures, 0);
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
    fn raw_clock_report_joint_motion_maps_to_cli_report_shape() {
        let report = RawClockRuntimeReport {
            joint_motion: Some(piper_client::dual_arm_raw_clock::RawClockJointMotionStats {
                master_feedback_min_rad: [0.0, 0.0, 0.0, -1.2, 0.0, 0.0],
                master_feedback_max_rad: [0.0, 0.0, 0.0, -0.7, 0.0, 0.0],
                master_feedback_delta_rad: [0.0, 0.0, 0.0, 0.5, 0.0, 0.0],
                slave_command_min_rad: [0.0, 0.0, 0.0, 0.6, 0.0, 0.0],
                slave_command_max_rad: [0.0, 0.0, 0.0, 1.1, 0.0, 0.0],
                slave_command_delta_rad: [0.0, 0.0, 0.0, 0.5, 0.0, 0.0],
                slave_feedback_min_rad: [0.0, 0.0, 0.0, -1.1, 0.0, 0.0],
                slave_feedback_max_rad: [0.0, 0.0, 0.0, -1.1, 0.0, 0.0],
                slave_feedback_delta_rad: [0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            }),
            ..raw_clock_report_success()
        };

        let motion =
            report_joint_motion_from_raw_clock(&report).expect("joint motion should be mapped");

        assert_eq!(motion.master_feedback_delta_rad[3], 0.5);
        assert_eq!(motion.slave_command_delta_rad[3], 0.5);
        assert_eq!(motion.slave_feedback_delta_rad[3], 0.0);
    }

    #[test]
    fn raw_clock_bilateral_report_preserves_submission_fault_contract() {
        let slave_failed = RawClockRuntimeReport {
            submission_faults: 1,
            last_submission_failed_side: Some(RawClockSide::Slave),
            peer_command_may_have_applied: false,
            exit_reason: Some(RawClockRuntimeExitReason::SubmissionFault),
            ..raw_clock_report_success()
        };
        let converted = bilateral_report_from_raw_clock(&slave_failed);
        assert_eq!(converted.submission_faults, 1);
        assert_eq!(
            converted.last_submission_failed_arm,
            Some(SubmissionArm::Right)
        );
        assert!(!converted.peer_command_may_have_applied);

        let master_failed = RawClockRuntimeReport {
            submission_faults: 1,
            last_submission_failed_side: Some(RawClockSide::Master),
            peer_command_may_have_applied: true,
            exit_reason: Some(RawClockRuntimeExitReason::SubmissionFault),
            ..raw_clock_report_success()
        };
        let converted = bilateral_report_from_raw_clock(&master_failed);
        assert_eq!(
            converted.last_submission_failed_arm,
            Some(SubmissionArm::Left)
        );
        assert!(converted.peer_command_may_have_applied);
    }

    #[test]
    fn raw_clock_bilateral_report_preserves_transport_telemetry() {
        let report = RawClockRuntimeReport {
            master_tx_realtime_overwrites_total: 2,
            slave_tx_realtime_overwrites_total: 3,
            master_tx_frames_sent_total: 11,
            slave_tx_frames_sent_total: 13,
            master_tx_fault_aborts_total: 5,
            slave_tx_fault_aborts_total: 7,
            last_runtime_fault_master: Some(RuntimeFaultKind::ManualFault),
            last_runtime_fault_slave: Some(RuntimeFaultKind::TransportError),
            ..raw_clock_report_success()
        };

        let converted = bilateral_report_from_raw_clock(&report);

        assert_eq!(converted.left_tx_realtime_overwrites_total, 2);
        assert_eq!(converted.right_tx_realtime_overwrites_total, 3);
        assert_eq!(converted.left_tx_frames_sent_total, 11);
        assert_eq!(converted.right_tx_frames_sent_total, 13);
        assert_eq!(converted.left_tx_fault_aborts_total, 5);
        assert_eq!(converted.right_tx_fault_aborts_total, 7);
        assert_eq!(
            converted.last_runtime_fault_left,
            Some(RuntimeFaultKind::ManualFault)
        );
        assert_eq!(
            converted.last_runtime_fault_right,
            Some(RuntimeFaultKind::TransportError)
        );
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
            state_skew_max_us: 10_000,
            residual_max_consecutive_failures: 5,
            alignment_lag_us: 4_000,
            selected_sample_age_ms: 50,
            alignment_search_window_us: 25_000,
            alignment_buffer_miss_consecutive_failures: 6,
        };

        let config = experimental_raw_clock_config_from_settings(
            &settings,
            TeleopMode::MasterFollower,
            333.0,
            Some(12),
        );

        assert_eq!(config.frequency_hz, 333.0);
        assert_eq!(config.max_iterations, Some(12));
        assert_eq!(config.estimator_thresholds.warmup_samples, 429);
        assert_eq!(config.estimator_thresholds.warmup_window_us, 3_000_000);
        assert_eq!(config.estimator_thresholds.residual_p95_us, 120);
        assert_eq!(config.estimator_thresholds.residual_max_us, 300);
        assert_eq!(config.estimator_thresholds.drift_abs_ppm, 42.0);
        assert_eq!(config.estimator_thresholds.sample_gap_max_us, 7_000);
        assert_eq!(config.estimator_thresholds.last_sample_age_us, 9_000);
        assert_eq!(config.thresholds.inter_arm_skew_max_us, 1500);
        assert_eq!(config.thresholds.last_sample_age_us, 9_000);
        assert_eq!(config.thresholds.residual_max_consecutive_failures, 5);
        assert_eq!(config.thresholds.alignment_lag_us, 4_000);
        assert_eq!(config.thresholds.selected_sample_age_us, 50_000);
        assert_eq!(config.thresholds.alignment_search_window_us, 25_000);
        assert_eq!(
            config.thresholds.alignment_buffer_miss_consecutive_failures,
            6
        );
    }

    #[test]
    fn experimental_raw_clock_config_maps_bilateral_mode() {
        let settings = TeleopRawClockSettings {
            experimental_calibrated_raw: true,
            warmup_secs: 10,
            residual_p95_us: 500,
            residual_max_us: 2000,
            drift_abs_ppm: 500.0,
            sample_gap_max_ms: 20,
            last_sample_age_ms: 20,
            inter_arm_skew_max_us: 20_000,
            state_skew_max_us: 10_000,
            selected_sample_age_ms: 50,
            residual_max_consecutive_failures: 3,
            alignment_lag_us: 5_000,
            alignment_search_window_us: 25_000,
            alignment_buffer_miss_consecutive_failures: 3,
        };

        let config = experimental_raw_clock_config_from_settings(
            &settings,
            TeleopMode::Bilateral,
            100.0,
            None,
        );

        assert_eq!(config.mode, ExperimentalRawClockMode::Bilateral);
    }

    #[test]
    fn experimental_raw_clock_warmup_sample_threshold_uses_sample_gap() {
        let settings = TeleopRawClockSettings {
            experimental_calibrated_raw: true,
            warmup_secs: 10,
            residual_p95_us: 500,
            residual_max_us: 2000,
            drift_abs_ppm: 500.0,
            sample_gap_max_ms: 20,
            last_sample_age_ms: 20,
            inter_arm_skew_max_us: 5000,
            state_skew_max_us: 10_000,
            selected_sample_age_ms: 50,
            residual_max_consecutive_failures: 3,
            alignment_lag_us: 5_000,
            alignment_search_window_us: 25_000,
            alignment_buffer_miss_consecutive_failures: 3,
        };

        let config = experimental_raw_clock_config_from_settings(
            &settings,
            TeleopMode::MasterFollower,
            100.0,
            Some(300),
        );

        assert_eq!(config.estimator_thresholds.warmup_samples, 500);
    }

    #[test]
    fn experimental_raw_clock_read_policy_allows_one_control_tick_state_skew() {
        let settings = TeleopRawClockSettings {
            experimental_calibrated_raw: true,
            warmup_secs: 10,
            residual_p95_us: 500,
            residual_max_us: 2000,
            drift_abs_ppm: 500.0,
            sample_gap_max_ms: 20,
            last_sample_age_ms: 35,
            inter_arm_skew_max_us: 20_000,
            state_skew_max_us: 10_000,
            selected_sample_age_ms: 50,
            residual_max_consecutive_failures: 3,
            alignment_lag_us: 5_000,
            alignment_search_window_us: 25_000,
            alignment_buffer_miss_consecutive_failures: 3,
        };
        let policy = experimental_raw_clock_control_read_policy(&settings);

        assert_eq!(
            policy.max_state_skew_us,
            crate::teleop::config::DEFAULT_RAW_CLOCK_STATE_SKEW_MAX_US
        );
        assert_eq!(
            policy.max_feedback_age,
            Duration::from_millis(settings.last_sample_age_ms)
        );
        assert!(policy.max_state_skew_us > DualArmReadPolicy::default().per_arm.max_state_skew_us);
        assert!(5_471 <= policy.max_state_skew_us);
    }

    #[test]
    fn experimental_snapshot_wait_budgets_allow_feedback_startup_jitter() {
        assert!(
            EXPERIMENTAL_PRE_ENABLE_SOFT_SNAPSHOT_READY_TIMEOUT >= Duration::from_secs(2),
            "pre-enable snapshot wait budget is too short: {EXPERIMENTAL_PRE_ENABLE_SOFT_SNAPSHOT_READY_TIMEOUT:?}"
        );
        assert!(
            EXPERIMENTAL_ACTIVE_SOFT_SNAPSHOT_READY_TIMEOUT >= Duration::from_secs(2),
            "post-enable active snapshot wait budget is too short: {EXPERIMENTAL_ACTIVE_SOFT_SNAPSHOT_READY_TIMEOUT:?}"
        );
    }

    #[test]
    fn experimental_soft_snapshot_retry_classifier_accepts_contextual_incomplete_state() {
        let error = anyhow::Error::from(piper_client::types::RobotError::control_state_incomplete(
            0b111, 0x0f,
        ))
        .context("experimental raw-clock master dynamic feedback incomplete: mask=0x0f");

        assert!(is_retryable_experimental_soft_snapshot_error(&error));
    }

    #[test]
    fn experimental_soft_snapshot_ready_wait_reports_timeout_context() {
        let error =
            wait_for_experimental_soft_snapshot_ready(
                "pre-enable standby",
                Duration::from_millis(1),
                Duration::from_millis(1),
                || -> Result<DualArmSnapshot> {
                    Err(anyhow::Error::from(
                        piper_client::types::RobotError::control_state_incomplete(0b111, 0x3c),
                    )
                    .context("experimental raw-clock slave dynamic feedback incomplete: mask=0x3c"))
                },
            )
            .expect_err("timeout should include snapshot wait context");

        let error = format!("{error:#}");
        assert!(
            error
                .contains("experimental raw-clock pre-enable standby snapshot not ready after 1ms"),
            "missing timeout context: {error}"
        );
    }

    #[test]
    fn experimental_soft_snapshot_ready_wait_retries_incomplete_and_misaligned_states() {
        let attempts = Arc::new(Mutex::new(0usize));

        let snapshot = wait_for_experimental_soft_snapshot_ready(
            "test",
            Duration::from_millis(50),
            Duration::from_millis(1),
            {
                let attempts = Arc::clone(&attempts);
                move || {
                    let mut attempts = attempts.lock().unwrap();
                    *attempts += 1;
                    match *attempts {
                        1 => Err(anyhow::Error::from(
                            piper_client::types::RobotError::control_state_incomplete(0b111, 0x0f),
                        )
                        .context(
                            "experimental raw-clock master dynamic feedback incomplete: mask=0x0f",
                        )),
                        2 => Err(anyhow::Error::from(
                            piper_client::types::RobotError::state_misaligned(2_063, 2_000),
                        )),
                        _ => Ok(sample_snapshot(false)),
                    }
                }
            },
        )
        .expect("readiness wait should retry until a complete aligned snapshot is available");

        assert_eq!(
            snapshot.left.state.position,
            sample_snapshot(false).left.state.position
        );
        assert_eq!(*attempts.lock().unwrap(), 3);
    }

    #[test]
    fn experimental_raw_clock_settings_convert_to_client_controller_gains() {
        let settings = RuntimeTeleopSettings::production(sample_calibration())
            .with_track_gains(9.25, 1.75)
            .unwrap()
            .with_master_damping(0.65)
            .unwrap();

        let gains = experimental_raw_clock_master_follower_gains(&settings);

        assert_eq!(gains.track_kp, JointArray::splat(9.25));
        assert_eq!(gains.track_kd, JointArray::splat(1.75));
        assert_eq!(gains.master_damping, JointArray::splat(0.65));
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

    fn experimental_args_with_calibration(path: &Path) -> TeleopDualArmArgs {
        TeleopDualArmArgs {
            calibration_file: Some(path.to_path_buf()),
            ..experimental_args()
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
            joint_motion: None,
            max_inter_arm_skew_us: 100,
            inter_arm_skew_p95_us: 80,
            alignment_lag_us: 5_000,
            latest_inter_arm_skew_max_us: 300,
            latest_inter_arm_skew_p95_us: 120,
            selected_inter_arm_skew_max_us: 100,
            selected_inter_arm_skew_p95_us: 80,
            clock_health_failures: 0,
            alignment_buffer_misses: 0,
            alignment_buffer_miss_consecutive_max: 0,
            alignment_buffer_miss_consecutive_failures: 0,
            master_residual_max_spikes: 0,
            slave_residual_max_spikes: 0,
            master_residual_max_consecutive_failures: 0,
            slave_residual_max_consecutive_failures: 0,
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
            failure_kind: None,
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

    fn write_sample_calibration_file(path: &Path) {
        CalibrationFile::from_calibration(&sample_calibration(), None, 123)
            .save_new(path)
            .expect("sample calibration file should be saved");
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
