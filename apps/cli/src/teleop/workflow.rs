use anyhow::{Context, Result, bail};
use piper_client::PiperBuilder;
use piper_client::dual_arm::{
    BilateralExitReason, BilateralLoopConfig, BilateralRunReport, DualArmActiveMit, DualArmBuilder,
    DualArmCalibration, DualArmLoopExit, DualArmReadPolicy, DualArmSnapshot, DualArmStandby,
    JointMirrorMap,
};
use piper_client::state::{DisableConfig, MitModeConfig};
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
use crate::teleop::config::{ResolvedTeleopConfig, TeleopConfigFile, TeleopMode, TeleopProfile};
use crate::teleop::controller::{
    RuntimeTeleopController, RuntimeTeleopSettings, RuntimeTeleopSettingsHandle,
};
use crate::teleop::report::{
    ReportCalibration, TeleopExitStatus, TeleopJsonReport, TeleopReportInput, classify_exit,
    print_human_report,
};
use crate::teleop::target::{RoleTargets, TeleopPlatform, resolve_role_targets};

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
    pub report_path: Option<PathBuf>,
    pub yes: bool,
}

#[allow(dead_code)]
pub fn run_workflow<B: TeleopBackend>(
    args: TeleopDualArmArgs,
    backend: &mut B,
    io: &mut dyn TeleopIo,
) -> Result<TeleopExitStatus> {
    run_workflow_on_platform(args, backend, io, TeleopPlatform::current())
}

pub(crate) fn run_workflow_on_platform<B: TeleopBackend>(
    args: TeleopDualArmArgs,
    backend: &mut B,
    io: &mut dyn TeleopIo,
    platform: TeleopPlatform,
) -> Result<TeleopExitStatus> {
    let config_file = args.config.as_deref().map(TeleopConfigFile::load).transpose()?;
    let resolved = ResolvedTeleopConfig::resolve(args.clone(), config_file.clone())?;

    let loaded_calibration =
        resolved.calibration.file.as_deref().map(load_calibration).transpose()?;

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
            let calibration = backend.capture_calibration(JointMirrorMap::left_right_mirror())?;
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
        report_path: args.report_json.clone(),
        yes: args.yes,
    };
    if !io.confirm_start(&summary)? {
        if io.cancel_requested() || cancel_signal.load(Ordering::SeqCst) {
            return Ok(TeleopExitStatus::Success);
        }
        bail!("operator confirmation declined");
    }

    let standby_snapshot = backend.standby_snapshot(DualArmReadPolicy::default())?;
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

    let active_snapshot = match backend.active_snapshot(DualArmReadPolicy::default()) {
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
    use crate::teleop::target::TeleopPlatform;
    use anyhow::{Result, bail};
    use piper_client::dual_arm::{
        BilateralExitReason, BilateralLoopConfig, BilateralRunReport, DualArmCalibration,
        DualArmReadPolicy, DualArmSnapshot, JointMirrorMap,
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

    #[derive(Debug, Clone)]
    struct FakeTeleopBackend {
        trace: WorkflowTrace,
        health_failure: bool,
        standby_mismatch: bool,
        active_mismatch: bool,
        active_snapshot_error: bool,
        enable_error: bool,
        disabled_or_faulted: Arc<AtomicBool>,
        loop_exit: TeleopLoopExit,
    }

    impl Default for FakeTeleopBackend {
        fn default() -> Self {
            Self {
                trace: WorkflowTrace::default(),
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

        fn calls(&self) -> Vec<WorkflowCall> {
            self.trace.calls()
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
            self.disabled_or_faulted.store(true, Ordering::SeqCst);
            Ok(self.loop_exit.clone())
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
