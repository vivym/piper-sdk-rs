use anyhow::{Context, Result, bail};
use piper_client::dual_arm::{
    BilateralLoopConfig, BilateralRunReport, DualArmCalibration, DualArmReadPolicy,
    DualArmSnapshot, JointMirrorMap,
};
use piper_client::state::MitModeConfig;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread::JoinHandle;
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
    fn enable_mit(&mut self, master: MitModeConfig, slave: MitModeConfig) -> Result<EnableOutcome>;
    fn disable_active(&mut self) -> Result<()>;
    fn active_snapshot(&self, policy: DualArmReadPolicy) -> Result<DualArmSnapshot>;
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
        bail!("teleop cancelled before enable");
    }

    match backend.enable_mit(MitModeConfig::default(), MitModeConfig::default())? {
        EnableOutcome::Active => {},
    }

    if io.cancel_requested() || cancel_signal.load(Ordering::SeqCst) {
        backend
            .disable_active()
            .context("failed to disable active teleop after cancellation")?;
        bail!("teleop cancelled during enable");
    }

    let active_snapshot = backend.active_snapshot(DualArmReadPolicy::default())?;
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

    let settings = RuntimeTeleopSettings::production(calibration.clone())
        .with_mode(resolved.control.mode)?
        .with_track_gains(resolved.control.track_kp, resolved.control.track_kd)?
        .with_master_damping(resolved.control.master_damping)?
        .with_reflection_gain(resolved.control.reflection_gain)?;
    let settings_handle = RuntimeTeleopSettingsHandle::new(settings)?;
    let initial_mode = settings_handle.snapshot().mode;
    let started_at = Instant::now();
    let _console = io.start_console(settings_handle.clone(), started_at)?;

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
        faulted: loop_exit.faulted,
        report: &loop_exit.report,
    });
    print_human_report(&report, started_at.elapsed());

    if let Some(path) = &args.report_json {
        io.write_json_report(path, &report)?;
    }

    Ok(classify_exit(loop_exit.faulted, &loop_exit.report))
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

pub async fn run_dual_arm(_args: TeleopDualArmArgs) -> Result<()> {
    bail!("teleop dual-arm is not implemented yet")
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

        fn cancel_before_confirmation() -> Self {
            Self {
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
    fn cancel_before_enable_exits_without_enable() {
        let backend = FakeTeleopBackend::default();
        let io = FakeTeleopIo::cancel_before_confirmation();

        let err = run_workflow_for_test_with_io(valid_args(), backend.clone(), io)
            .expect_err("Ctrl+C before enable must stop before enable");

        assert!(err.to_string().contains("cancel"));
        assert!(backend.calls().contains(&WorkflowCall::Connect));
        assert!(!backend.calls().contains(&WorkflowCall::Enable));
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
    fn cancel_during_enable_disables_active_before_exit() {
        let backend = FakeTeleopBackend::cancel_during_enable_after_active();
        let io = FakeTeleopIo::cancel_during_enable();

        let err = run_workflow_for_test_with_io(valid_args(), backend.clone(), io)
            .expect_err("Ctrl+C during enable must exit safely");

        assert!(err.to_string().contains("cancel"));
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
