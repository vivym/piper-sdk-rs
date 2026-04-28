use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow};
use piper_client::PiperBuilder;
use piper_client::dual_arm::{
    BilateralCommand, BilateralControlFrame, BilateralController, BilateralDynamicsCompensation,
    BilateralExitReason, BilateralFinalTorques, BilateralGripperCommandStatus, BilateralLoopConfig,
    BilateralLoopGripperTelemetry, BilateralLoopTelemetry, BilateralLoopTelemetrySink,
    BilateralLoopTimingTelemetry, BilateralRunReport, BilateralSubmissionMode,
    BilateralTelemetrySinkError, DualArmActiveMit, DualArmBuilder, DualArmCalibration,
    DualArmLoopExit, DualArmReadPolicy, DualArmSnapshot, DualArmStandby, GripperTeleopConfig,
    JointMirrorMap, LoopTimingMode,
};
use piper_client::observer::{ControlSnapshot, ControlSnapshotFull};
use piper_client::state::{DisableConfig, MitModeConfig};
use piper_client::types::{Joint, JointArray, NewtonMeter, Rad, RadPerSecond};
use piper_physics::{
    DualArmMujocoCompensatorConfig, DynamicsMode, EndEffectorKinematics, PayloadSpec,
    SharedModeState, SharedPayloadState,
};

use crate::args::Args;
use crate::calibration::{
    CalibrationFile, MirrorMapFile, ResolvedCalibration, load_file_backed_mirror_map_bytes,
    resolve_episode_calibration, sha256_hex, validate_current_posture,
};
use crate::cancel::{CollectorCancelToken, install_ctrlc_handler};
use crate::controller::SvsController;
use crate::cue::{AppliedMasterFeedback, AppliedMasterFeedbackHistory};
use crate::episode::manifest::{
    ArmDynamicsManifest, CalibrationManifest, CollectorIdentityManifest, DualArmReportJson,
    DynamicsManifest, EffectiveProfileManifest, EpisodeId, EpisodeStatus, FixedEpisodeRng,
    GripperMirrorManifest, HashedArtifactManifest, JointMirrorMapManifest, ManifestV1,
    MirrorMapManifest, MujocoManifest, OsEpisodeRng, PayloadManifest, ProfileSourceKind,
    RawCanManifest, ReportJson, SourceArtifactManifest, StepFileManifest, TargetSpecManifest,
    TargetSpecsManifest, TaskManifest, UtcTimestamp, WriterFlushResultJson, WriterReportJson,
    slugify_task_name,
};
use crate::episode::wire::{
    SvsArmStepV1, SvsCommandStepV1, SvsGripperStepV1, SvsHeaderV1, SvsStepV1,
};
use crate::episode::writer::{
    EpisodeWriter, WriterBackpressureMonitor, WriterFlushSummary, WriterStats,
};
use crate::model_hash::{current_mujoco_runtime_identity, hash_embedded_model, hash_model_dir};
use crate::mujoco_bridge::{SvsMujocoBridge, SvsMujocoBridgeConfig, SvsMujocoModelSource};
use crate::profile::EffectiveProfile;
use crate::raw_can::{
    RawCanCaptureStatus, RawCanRecordingHandle, RawCanStatusSource, RawCanStatusTracker,
};
use crate::target::{SocketCanTarget, validate_targets};
use crate::tick_frame::{
    SnapshotKey, SvsDynamicsFrame, SvsDynamicsSlot, SvsPendingTick, SvsTickStager,
};

const DEFAULT_TASK_NAME: &str = "Surface Following Demo!";
const DEFAULT_MUJOCO_ROOT_XML: &str = "piper_no_gripper.xml";
const EPISODE_ID_ATTEMPTS: usize = 16;
const STARTED_UNIX_NS: u64 = 1_777_292_523_000_000_000;
const ENDED_UNIX_NS: u64 = 1_777_292_524_000_000_000;
const EPISODE_START_HOST_MONO_US: u64 = 1_000_000;
const DEFAULT_WRITER_CAPACITY: usize = 64;
const EMBEDDED_MUJOCO_XML: &[u8] =
    include_bytes!("../../piper-physics-mujoco/assets/piper_no_gripper.xml");

#[derive(Debug, Clone, PartialEq)]
pub struct FakeMujocoFrame {
    pub master_dynamic_host_rx_mono_us: u64,
    pub slave_dynamic_host_rx_mono_us: u64,
    pub master_residual_nm: [f64; 6],
    pub slave_residual_nm: [f64; 6],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GripperTiming {
    offset_us: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CollectorRunResult {
    pub status: EpisodeStatus,
    pub path: PathBuf,
    pub dual_arm_exit_reason: Option<BilateralExitReason>,
    pub loop_stopped_before_requested_iterations: bool,
    pub enable_mit_calls: u32,
    pub disable_called: bool,
}

#[derive(Debug, Clone)]
pub struct FakeCollectorHarness {
    targets_configured: bool,
    mujoco_configured: bool,
    mujoco_sequence: Vec<FakeMujocoFrame>,
    iterations: usize,
    master_tx_finished_timeout_at_step: Option<usize>,
    writer_capacity: usize,
    pause_writer_until_shutdown: bool,
    writer_flush_failure: bool,
    gripper_feedback: BTreeMap<usize, GripperTiming>,
    raw_can_requested: bool,
    raw_can_degraded_after_step: Option<usize>,
    startup_fault_before_enable_mit: bool,
    cancel_before_enable_mit: bool,
    operator_confirmation: bool,
    cancel_during_active_control_after_steps: Option<usize>,
}

#[derive(Debug, thiserror::Error)]
pub enum SvsTelemetrySinkFault {
    #[error("missing {arm} TX-finished timestamp")]
    MissingTxFinished { arm: &'static str },
    #[error("SVS pending tick staging failed: {0}")]
    TickFrame(#[from] crate::tick_frame::SvsTickFrameError),
    #[error("SVS writer failed: {0}")]
    Writer(#[from] anyhow::Error),
    #[error("applied master feedback history lock is poisoned")]
    FeedbackHistoryPoisoned,
    #[error("{arm} feedback timestamp is unavailable or newer than the control frame")]
    InvalidFeedbackTimestamp { arm: &'static str },
    #[error("writer backpressure monitor lock is poisoned")]
    BackpressureMonitorPoisoned,
    #[error("writer backpressure threshold tripped")]
    WriterBackpressureThreshold,
    #[error("control-frame timestamp predates episode start")]
    ControlFrameBeforeEpisodeStart,
}

pub trait SvsStepEnqueuer: Send + Sync {
    fn try_enqueue_step(&self, step: SvsStepV1) -> Result<()>;
    fn writer_stats(&self) -> WriterStats;
    fn record_backpressure_threshold_tripped(&self);
}

pub struct SharedEpisodeWriter {
    inner: Mutex<Option<EpisodeWriter>>,
    last_stats: Mutex<WriterStats>,
}

#[derive(Clone)]
pub struct SvsTelemetrySink<W = Arc<SharedEpisodeWriter>> {
    stager: Arc<SvsTickStager>,
    feedback_history: Arc<Mutex<AppliedMasterFeedbackHistory>>,
    writer: W,
    raw_can: Arc<dyn RawCanStatusSource>,
    backpressure_monitor: Arc<Mutex<WriterBackpressureMonitor>>,
    episode_start_host_mono_us: u64,
    next_step_index: Arc<AtomicU64>,
}

struct StepBuildContext {
    step_index: u64,
    raw_can_status: RawCanCaptureStatus,
    episode_start_host_mono_us: u64,
    master_tx_finished_host_mono_us: u64,
    slave_tx_finished_host_mono_us: u64,
}

#[derive(Debug, Clone)]
struct ResolvedProfile {
    profile: EffectiveProfile,
    effective_profile_bytes: Vec<u8>,
    effective_profile_hash: String,
    task_profile: SourceArtifactManifest,
    mirror_map: MirrorMapManifest,
    runtime_mirror_map: JointMirrorMap,
}

#[derive(Debug, Clone)]
struct ResolvedMujoco {
    bridge_source: SvsMujocoModelSource,
    manifest: MujocoManifest,
    compensator: DualArmMujocoCompensatorConfig,
}

#[derive(Debug, Clone)]
struct ResolvedRealCalibration {
    resolved: ResolvedCalibration,
    runtime: DualArmCalibration,
}

#[derive(Debug, Clone)]
struct RealEpisodeBase {
    manifest: ManifestV1,
    started_unix_ns: u64,
}

#[derive(Debug, Clone)]
struct RealRunContext {
    episode_id: String,
    episode_dir: PathBuf,
    base: RealEpisodeBase,
    raw_can: Arc<RawCanStatusTracker>,
}

trait CollectorBackend {
    type Standby;
    type Active;
    type Runtime;

    fn connect(&self) -> Result<Self::Standby>;
    fn enable_mit(&self, standby: Self::Standby) -> Result<Self::Active>;
    fn run_loop(&self, active: Self::Active, runtime: Self::Runtime) -> Result<LoopOutcome>;
}

struct LoopOutcome {
    status: EpisodeStatus,
    report: BilateralRunReport,
    attempted_iterations: u64,
    loop_stopped_before_requested_iterations: bool,
    disable_called: bool,
}

struct RealCollectorBackend {
    master_target: SocketCanTarget,
    slave_target: SocketCanTarget,
    baud_rate: Option<u32>,
}

struct RealCollectorRuntime {
    controller: SvsController,
    bridge: SvsMujocoBridge,
    loop_config: BilateralLoopConfig,
}

struct FakeCollectorBackend<'a> {
    harness: &'a FakeCollectorHarness,
}

struct FakeStandby;
struct FakeActive;

struct FakeCollectorRuntime<W = Arc<SharedEpisodeWriter>> {
    dynamics_slot: Arc<SvsDynamicsSlot>,
    controller: SvsController,
    sink: SvsTelemetrySink<W>,
    raw_can: Arc<RawCanStatusTracker>,
}

struct RealManifestInputs<'a> {
    episode_id: String,
    task_raw_name: String,
    task_slug: String,
    operator: Option<String>,
    notes: Option<String>,
    started_unix_ns: u64,
    episode_start_host_mono_us: u64,
    master_requested: String,
    slave_requested: String,
    master_target: SocketCanTarget,
    slave_target: SocketCanTarget,
    mujoco: MujocoManifest,
    profile: &'a EffectiveProfile,
    task_profile: SourceArtifactManifest,
    effective_profile_hash: String,
    mirror_map: MirrorMapManifest,
    calibration: &'a CalibrationFile,
    calibration_hash: String,
    raw_can_enabled: bool,
    disable_gripper_requested: bool,
}

impl FakeMujocoFrame {
    pub fn new(master_dynamic_host_rx_mono_us: u64, slave_dynamic_host_rx_mono_us: u64) -> Self {
        Self {
            master_dynamic_host_rx_mono_us,
            slave_dynamic_host_rx_mono_us,
            master_residual_nm: [0.0; 6],
            slave_residual_nm: [0.0; 6],
        }
    }

    pub fn with_master_residual_nm(mut self, residual_nm: [f64; 6]) -> Self {
        self.master_residual_nm = residual_nm;
        self
    }

    pub fn with_slave_residual_nm(mut self, residual_nm: [f64; 6]) -> Self {
        self.slave_residual_nm = residual_nm;
        self
    }
}

impl GripperTiming {
    pub fn stale_by_ms(milliseconds: u64) -> Self {
        Self {
            offset_us: -(milliseconds.min(i64::MAX as u64 / 1_000) as i64 * 1_000),
        }
    }

    pub fn future_by_ms(milliseconds: u64) -> Self {
        Self {
            offset_us: (milliseconds.min(i64::MAX as u64 / 1_000) as i64 * 1_000),
        }
    }
}

impl Default for FakeCollectorHarness {
    fn default() -> Self {
        Self {
            targets_configured: false,
            mujoco_configured: false,
            mujoco_sequence: Vec::new(),
            iterations: 2,
            master_tx_finished_timeout_at_step: None,
            writer_capacity: DEFAULT_WRITER_CAPACITY,
            pause_writer_until_shutdown: false,
            writer_flush_failure: false,
            gripper_feedback: BTreeMap::new(),
            raw_can_requested: false,
            raw_can_degraded_after_step: None,
            startup_fault_before_enable_mit: false,
            cancel_before_enable_mit: false,
            operator_confirmation: true,
            cancel_during_active_control_after_steps: None,
        }
    }
}

impl CollectorBackend for RealCollectorBackend {
    type Standby = DualArmStandby;
    type Active = DualArmActiveMit;
    type Runtime = RealCollectorRuntime;

    fn connect(&self) -> Result<Self::Standby> {
        DualArmBuilder::new(
            piper_builder_for_target(&self.master_target, self.baud_rate),
            piper_builder_for_target(&self.slave_target, self.baud_rate),
        )
        .build()
        .context("failed to connect both Piper arms")
    }

    fn enable_mit(&self, standby: Self::Standby) -> Result<Self::Active> {
        standby
            .enable_mit(MitModeConfig::default(), MitModeConfig::default())
            .context("failed to enable MIT mode on both arms")
    }

    fn run_loop(&self, active: Self::Active, runtime: Self::Runtime) -> Result<LoopOutcome> {
        let loop_exit = active.run_bilateral_with_compensation(
            runtime.controller,
            runtime.bridge,
            runtime.loop_config,
        );
        Ok(match loop_exit {
            Ok(DualArmLoopExit::Standby { report, .. }) => LoopOutcome {
                status: episode_status_from_report(&report, false),
                attempted_iterations: report.iterations as u64,
                loop_stopped_before_requested_iterations: !matches!(
                    report.exit_reason,
                    Some(BilateralExitReason::MaxIterations) | None
                ),
                report,
                disable_called: true,
            },
            Ok(DualArmLoopExit::Faulted { report, .. }) => LoopOutcome {
                status: EpisodeStatus::Faulted,
                attempted_iterations: report.iterations as u64,
                loop_stopped_before_requested_iterations: true,
                report,
                disable_called: true,
            },
            Err(error) => LoopOutcome {
                status: EpisodeStatus::Faulted,
                report: BilateralRunReport {
                    last_error: Some(error.to_string()),
                    ..BilateralRunReport::default()
                },
                attempted_iterations: 0,
                loop_stopped_before_requested_iterations: true,
                disable_called: true,
            },
        })
    }
}

impl<'a> CollectorBackend for FakeCollectorBackend<'a> {
    type Standby = FakeStandby;
    type Active = FakeActive;
    type Runtime = FakeCollectorRuntime;

    fn connect(&self) -> Result<Self::Standby> {
        self.harness.validate()?;
        Ok(FakeStandby)
    }

    fn enable_mit(&self, _standby: Self::Standby) -> Result<Self::Active> {
        Ok(FakeActive)
    }

    fn run_loop(&self, _active: Self::Active, mut runtime: Self::Runtime) -> Result<LoopOutcome> {
        let mut report = BilateralRunReport::default();
        let mut attempted_iterations = 0_u64;

        for step in 0..self.harness.iterations {
            if self
                .harness
                .cancel_during_active_control_after_steps
                .is_some_and(|limit| step >= limit)
            {
                report.exit_reason = Some(BilateralExitReason::Cancelled);
                report.iterations = attempted_iterations as usize;
                return Ok(LoopOutcome {
                    status: EpisodeStatus::Cancelled,
                    report,
                    attempted_iterations,
                    loop_stopped_before_requested_iterations: true,
                    disable_called: true,
                });
            }

            if self
                .harness
                .raw_can_degraded_after_step
                .is_some_and(|last_ok_step| step > last_ok_step)
            {
                runtime.raw_can.mark_degraded();
            }

            let frame = self.harness.frame_for_step(step);
            let host_mono_us = EPISODE_START_HOST_MONO_US + step as u64 * 5_000 + 5_000;
            let snapshot = sample_snapshot(&frame, step, host_mono_us);
            let dynamics = fake_dynamics_for_step(step, &frame);
            runtime.dynamics_slot.store_dynamics(dynamics)?;
            let control_frame = BilateralControlFrame {
                snapshot,
                compensation: Some(fake_compensation_for_frame(&frame)),
            };
            let controller_command = runtime
                .controller
                .tick_with_compensation(&control_frame, Duration::from_micros(5_000))?;
            attempted_iterations = attempted_iterations.saturating_add(1);
            let telemetry = telemetry_for_fake_control_frame(
                step,
                control_frame,
                controller_command,
                self.harness.master_tx_finished_timeout_at_step == Some(step),
                self.harness.gripper_feedback.get(&step).copied(),
            );

            if let Err(error) = runtime.sink.handle_tick(&telemetry) {
                report.exit_reason = Some(BilateralExitReason::TelemetrySinkFault);
                report.iterations = attempted_iterations as usize;
                if !matches!(
                    error,
                    SvsTelemetrySinkFault::MissingTxFinished { arm: "master" }
                ) {
                    tracing::debug!(%error, "fake collector telemetry sink faulted");
                }
                return Ok(LoopOutcome {
                    status: EpisodeStatus::Faulted,
                    report,
                    attempted_iterations,
                    loop_stopped_before_requested_iterations: true,
                    disable_called: false,
                });
            }
        }

        report.iterations = attempted_iterations as usize;
        Ok(LoopOutcome {
            status: EpisodeStatus::Complete,
            report,
            attempted_iterations,
            loop_stopped_before_requested_iterations: false,
            disable_called: false,
        })
    }
}

impl FakeCollectorHarness {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_two_socketcan_targets(mut self) -> Self {
        self.targets_configured = true;
        self
    }

    pub fn with_fake_mujoco(mut self) -> Self {
        self.mujoco_configured = true;
        self
    }

    pub fn with_fake_mujoco_sequence<I>(mut self, sequence: I) -> Self
    where
        I: IntoIterator<Item = FakeMujocoFrame>,
    {
        self.mujoco_configured = true;
        self.mujoco_sequence = sequence.into_iter().collect();
        self
    }

    pub fn with_iterations(mut self, iterations: usize) -> Self {
        self.iterations = iterations;
        self
    }

    pub fn with_master_tx_finished_timeout_at_step(mut self, step: usize) -> Self {
        self.master_tx_finished_timeout_at_step = Some(step);
        self.iterations = self.iterations.max(step.saturating_add(1));
        self
    }

    pub fn with_writer_capacity(mut self, capacity: usize) -> Self {
        self.writer_capacity = capacity;
        self
    }

    pub fn with_paused_writer_until_shutdown(mut self) -> Self {
        self.pause_writer_until_shutdown = true;
        self
    }

    pub fn with_writer_flush_failure(mut self) -> Self {
        self.writer_flush_failure = true;
        self
    }

    pub fn with_gripper_feedback_at_step(mut self, step: usize, timing: GripperTiming) -> Self {
        self.gripper_feedback.insert(step, timing);
        self
    }

    pub fn with_raw_can_requested(mut self) -> Self {
        self.raw_can_requested = true;
        self
    }

    pub fn with_raw_can_degraded_after_step(mut self, step: usize) -> Self {
        self.raw_can_degraded_after_step = Some(step);
        self
    }

    pub fn with_startup_fault_before_enable_mit(mut self) -> Self {
        self.startup_fault_before_enable_mit = true;
        self
    }

    pub fn with_cancel_before_enable_mit(mut self) -> Self {
        self.cancel_before_enable_mit = true;
        self
    }

    pub fn with_operator_confirmation(mut self, confirmed: bool) -> Self {
        self.operator_confirmation = confirmed;
        self
    }

    pub fn with_cancel_during_active_control_after_steps(mut self, steps: usize) -> Self {
        self.cancel_during_active_control_after_steps = Some(steps);
        self.iterations = self.iterations.max(steps);
        self
    }

    pub fn run(&self, output_dir: impl AsRef<Path>) -> Result<CollectorRunResult> {
        let backend = FakeCollectorBackend { harness: self };
        let standby = backend.connect()?;
        let reservation = EpisodeId::reserve_directory(
            output_dir,
            DEFAULT_TASK_NAME,
            UtcTimestamp::for_tests(),
            &FixedEpisodeRng::from_hex("a1b2c3d4e5f6"),
            EPISODE_ID_ATTEMPTS,
        )?;
        let episode_id = reservation.id.episode_id.clone();
        let episode_dir = reservation.absolute_dir;

        let artifacts = persist_episode_inputs(&episode_dir)?;
        let initial_summary = crate::episode::wire::StepFileSummary {
            episode_id: episode_id.clone(),
            step_count: 0,
            last_step_index: None,
        };
        write_manifest(FakeManifestWrite {
            episode_dir: &episode_dir,
            episode_id: &episode_id,
            status: EpisodeStatus::Running,
            ended_unix_ns: None,
            summary: &initial_summary,
            artifacts: &artifacts,
            raw_can_enabled: self.raw_can_requested(),
            raw_can_finalizer_status: "running".to_string(),
        })?;
        let header = SvsHeaderV1::new(
            &episode_id,
            STARTED_UNIX_NS / 1_000_000,
            EPISODE_START_HOST_MONO_US,
        )?;
        let fake_profile = fake_effective_profile();
        let fake_monitor = writer_backpressure_monitor_from_profile(&fake_profile);
        let mut writer = EpisodeWriter::new_with_backpressure_thresholds(
            &episode_dir,
            header,
            self.writer_capacity,
            &fake_monitor,
        )?;
        if self.pause_writer_until_shutdown {
            writer.pause_worker_for_test();
        }
        let writer = Arc::new(SharedEpisodeWriter::new(writer));

        let mut status: EpisodeStatus;
        let mut dual_arm_exit_reason = None;
        let mut loop_stopped_before_requested_iterations = false;
        let mut enable_mit_calls = 0_u32;
        let mut disable_called = false;
        let mut attempted_iterations = 0_u64;
        let raw_can = Arc::new(if self.raw_can_degraded_after_step.is_some() {
            RawCanStatusTracker::ok()
        } else if self.raw_can_requested {
            RawCanStatusTracker::requested()
        } else {
            RawCanStatusTracker::disabled()
        });
        let stager = Arc::new(SvsTickStager::new());
        let dynamics_slot = Arc::new(SvsDynamicsSlot::new());
        let feedback_history = Arc::new(Mutex::new(AppliedMasterFeedbackHistory::default()));
        let sink = SvsTelemetrySink::new(
            Arc::clone(&stager),
            Arc::clone(&feedback_history),
            Arc::clone(&writer),
            raw_can.clone(),
            writer_backpressure_monitor_from_profile(&fake_profile),
            EPISODE_START_HOST_MONO_US,
        );
        let controller = SvsController::with_shared(
            fake_profile,
            fake_dual_arm_calibration(),
            Arc::clone(&stager),
            Arc::clone(&dynamics_slot),
            Arc::clone(&feedback_history),
        )?;

        if self.startup_fault_before_enable_mit {
            status = EpisodeStatus::Faulted;
            loop_stopped_before_requested_iterations = true;
        } else if self.cancel_before_enable_mit || !self.operator_confirmation {
            status = EpisodeStatus::Cancelled;
        } else {
            let active = backend.enable_mit(standby)?;
            enable_mit_calls = 1;
            let outcome = backend.run_loop(
                active,
                FakeCollectorRuntime {
                    dynamics_slot,
                    controller,
                    sink,
                    raw_can: Arc::clone(&raw_can),
                },
            )?;
            status = outcome.status;
            dual_arm_exit_reason = outcome.report.exit_reason;
            loop_stopped_before_requested_iterations =
                outcome.loop_stopped_before_requested_iterations;
            disable_called = outcome.disable_called;
            attempted_iterations = outcome.attempted_iterations;
        }

        if self.writer_flush_failure {
            fs::write(episode_dir.join("steps.bin"), b"forced flush collision")?;
        }
        let writer_stats_before_finish = writer.writer_stats();
        let flush_summary = writer.finish();
        let mut writer_stats =
            merge_writer_stats(writer_stats_before_finish, writer.writer_stats());
        let final_flush_result = match flush_summary {
            Ok(_summary) => WriterFlushResultJson {
                success: true,
                error: None,
            },
            Err(error) => {
                status = EpisodeStatus::Faulted;
                writer_stats.flush_failed = true;
                writer_stats.flush_error = Some(error.to_string());
                WriterFlushResultJson {
                    success: false,
                    error: Some(error.to_string()),
                }
            },
        };

        if writer_stats.queue_full_events > 0 || writer_stats.dropped_step_count > 0 {
            status = EpisodeStatus::Faulted;
            dual_arm_exit_reason = Some(BilateralExitReason::TelemetrySinkFault);
            loop_stopped_before_requested_iterations = true;
        }

        let summary = crate::episode::wire::read_steps_file(episode_dir.join("steps.bin"))
            .map(|decoded| decoded.summary)
            .unwrap_or_else(|_| crate::episode::wire::StepFileSummary {
                episode_id: episode_id.clone(),
                step_count: writer_stats.encoded_step_count,
                last_step_index: writer_stats.last_step_index,
            });
        let raw_finalizer_status = raw_can.finalizer_status();
        write_manifest(FakeManifestWrite {
            episode_dir: &episode_dir,
            episode_id: &episode_id,
            status,
            ended_unix_ns: Some(ENDED_UNIX_NS),
            summary: &summary,
            artifacts: &artifacts,
            raw_can_enabled: raw_can.raw_can_status() != RawCanCaptureStatus::Disabled,
            raw_can_finalizer_status: raw_finalizer_status,
        })?;
        write_report(FakeReportWrite {
            episode_dir: &episode_dir,
            episode_id: &episode_id,
            status,
            raw_can_enabled: raw_can.raw_can_status() != RawCanCaptureStatus::Disabled,
            raw_can_degraded: raw_can.raw_can_status() == RawCanCaptureStatus::Degraded,
            raw_can_finalizer_status: Some(raw_can.finalizer_status()),
            attempted_iterations,
            summary: &summary,
            exit_reason: dual_arm_exit_reason,
            final_flush_result,
            writer_stats: &writer_stats,
        })?;

        Ok(CollectorRunResult {
            status,
            path: episode_dir,
            dual_arm_exit_reason,
            loop_stopped_before_requested_iterations,
            enable_mit_calls,
            disable_called,
        })
    }

    fn validate(&self) -> Result<()> {
        if !self.targets_configured {
            return Err(anyhow!("fake collector requires two socketcan targets"));
        }
        if !self.mujoco_configured {
            return Err(anyhow!("fake collector requires fake MuJoCo"));
        }
        Ok(())
    }

    fn raw_can_requested(&self) -> bool {
        self.raw_can_requested || self.raw_can_degraded_after_step.is_some()
    }

    fn frame_for_step(&self, step: usize) -> FakeMujocoFrame {
        self.mujoco_sequence.get(step).cloned().unwrap_or_else(|| {
            FakeMujocoFrame::new(
                10_000_u64.saturating_add(step as u64 * 10_000),
                10_100_u64.saturating_add(step as u64 * 10_000),
            )
            .with_slave_residual_nm([step as f64 + 1.0, 0.0, 0.0, 0.0, 0.0, 0.0])
        })
    }
}

impl SharedEpisodeWriter {
    pub fn new(writer: EpisodeWriter) -> Self {
        let stats = writer.stats();
        Self {
            inner: Mutex::new(Some(writer)),
            last_stats: Mutex::new(stats),
        }
    }

    pub fn finish(&self) -> Result<WriterFlushSummary> {
        let writer = self
            .inner
            .lock()
            .map_err(|_| anyhow!("episode writer lock is poisoned"))?
            .take()
            .ok_or_else(|| anyhow!("episode writer has already been finished"))?;
        match writer.finish() {
            Ok(summary) => Ok(summary),
            Err(error) => Err(error.into()),
        }
    }

    fn remember_stats(&self, stats: WriterStats) {
        if let Ok(mut guard) = self.last_stats.lock() {
            *guard = stats;
        }
    }
}

impl SvsStepEnqueuer for SharedEpisodeWriter {
    fn try_enqueue_step(&self, step: SvsStepV1) -> Result<()> {
        let guard = self.inner.lock().map_err(|_| anyhow!("episode writer lock is poisoned"))?;
        let writer = guard
            .as_ref()
            .ok_or_else(|| anyhow!("episode writer has already been finished"))?;
        writer.try_enqueue(step)?;
        self.remember_stats(writer.stats());
        Ok(())
    }

    fn writer_stats(&self) -> WriterStats {
        match self.inner.lock() {
            Ok(guard) => {
                if let Some(writer) = guard.as_ref() {
                    let stats = writer.stats();
                    drop(guard);
                    self.remember_stats(stats.clone());
                    stats
                } else {
                    self.last_stats.lock().map(|stats| stats.clone()).unwrap_or_default()
                }
            },
            Err(_) => self.last_stats.lock().map(|stats| stats.clone()).unwrap_or_default(),
        }
    }

    fn record_backpressure_threshold_tripped(&self) {
        if let Ok(guard) = self.inner.lock()
            && let Some(writer) = guard.as_ref()
        {
            writer.record_backpressure_threshold_tripped();
            self.remember_stats(writer.stats());
            return;
        }
        if let Ok(mut stats) = self.last_stats.lock() {
            stats.record_backpressure_threshold_tripped();
        }
    }
}

impl SvsStepEnqueuer for EpisodeWriter {
    fn try_enqueue_step(&self, step: SvsStepV1) -> Result<()> {
        self.try_enqueue(step)?;
        Ok(())
    }

    fn writer_stats(&self) -> WriterStats {
        self.stats()
    }

    fn record_backpressure_threshold_tripped(&self) {
        EpisodeWriter::record_backpressure_threshold_tripped(self);
    }
}

impl<T> SvsStepEnqueuer for Arc<T>
where
    T: SvsStepEnqueuer + ?Sized,
{
    fn try_enqueue_step(&self, step: SvsStepV1) -> Result<()> {
        (**self).try_enqueue_step(step)
    }

    fn writer_stats(&self) -> WriterStats {
        (**self).writer_stats()
    }

    fn record_backpressure_threshold_tripped(&self) {
        (**self).record_backpressure_threshold_tripped();
    }
}

impl<W> SvsTelemetrySink<W>
where
    W: SvsStepEnqueuer,
{
    pub fn new(
        stager: Arc<SvsTickStager>,
        feedback_history: Arc<Mutex<AppliedMasterFeedbackHistory>>,
        writer: W,
        raw_can: Arc<dyn RawCanStatusSource>,
        backpressure_monitor: WriterBackpressureMonitor,
        episode_start_host_mono_us: u64,
    ) -> Self {
        Self {
            stager,
            feedback_history,
            writer,
            raw_can,
            backpressure_monitor: Arc::new(Mutex::new(backpressure_monitor)),
            episode_start_host_mono_us,
            next_step_index: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn handle_tick(
        &self,
        telemetry: &BilateralLoopTelemetry,
    ) -> std::result::Result<(), SvsTelemetrySinkFault> {
        let master_tx_finished_host_mono_us =
            required_tx_finished("master", telemetry.master_tx_finished_host_mono_us)?;
        let slave_tx_finished_host_mono_us =
            required_tx_finished("slave", telemetry.slave_tx_finished_host_mono_us)?;
        let pending = self.stager.take_for_telemetry(telemetry)?;
        let stats = self.writer.writer_stats();
        let step_index = self.next_step_index.load(Ordering::Acquire);
        let step = step_from_telemetry(
            telemetry,
            pending,
            StepBuildContext {
                step_index,
                raw_can_status: self.raw_can.raw_can_status(),
                episode_start_host_mono_us: self.episode_start_host_mono_us,
                master_tx_finished_host_mono_us,
                slave_tx_finished_host_mono_us,
            },
            &stats,
        )?;
        if let Err(error) = self.writer.try_enqueue_step(step) {
            let latest_stats = self.writer.writer_stats();
            if latest_stats.queue_full_events > stats.queue_full_events {
                let mut monitor = self
                    .backpressure_monitor
                    .lock()
                    .map_err(|_| SvsTelemetrySinkFault::BackpressureMonitorPoisoned)?;
                if monitor.record_queue_full(Instant::now()) {
                    self.writer.record_backpressure_threshold_tripped();
                    return Err(SvsTelemetrySinkFault::WriterBackpressureThreshold);
                }
            }
            return Err(SvsTelemetrySinkFault::Writer(error));
        }
        self.next_step_index.store(step_index.saturating_add(1), Ordering::Release);
        self.feedback_history
            .lock()
            .map_err(|_| SvsTelemetrySinkFault::FeedbackHistoryPoisoned)?
            .push(AppliedMasterFeedback {
                master_tx_finished_host_mono_us,
                shaped_master_interaction_nm: torque_array(
                    telemetry.shaped_command.master_interaction_torque,
                ),
            });
        Ok(())
    }
}

impl<W> BilateralLoopTelemetrySink for SvsTelemetrySink<W>
where
    W: SvsStepEnqueuer,
{
    fn on_tick(
        &self,
        telemetry: &BilateralLoopTelemetry,
    ) -> std::result::Result<(), BilateralTelemetrySinkError> {
        self.handle_tick(telemetry).map_err(|error| BilateralTelemetrySinkError {
            message: error.to_string(),
        })
    }
}

pub fn read_manifest_toml(path: &Path) -> Result<ManifestV1> {
    let text =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let manifest: ManifestV1 = toml::from_str(&text)?;
    manifest.validate()?;
    Ok(manifest)
}

pub fn read_report_json(path: &Path) -> Result<ReportJson> {
    let text =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let report: ReportJson = serde_json::from_str(&text)?;
    report.validate()?;
    Ok(report)
}

pub fn run_from_args(args: Args) -> Result<CollectorRunResult> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init();
    let cancel = CollectorCancelToken::new();
    install_ctrlc_handler(cancel.clone()).context("failed to install Ctrl-C handler")?;
    run_real_collector(args, cancel)
}

fn run_real_collector(args: Args, cancel: CollectorCancelToken) -> Result<CollectorRunResult> {
    let (master_target, slave_target) = validate_targets(&args.master_target, &args.slave_target)?;
    let resolved_profile = resolve_profile_from_args(&args)?;
    let started_unix_ns = current_unix_ns();
    let timestamp = utc_timestamp_from_unix_ns(started_unix_ns)?;
    let episode_start_host_mono_us = piper_driver::heartbeat::monotonic_micros();
    let task_name = args.task.clone().unwrap_or_else(|| DEFAULT_TASK_NAME.to_string());
    validate_static_startup_inputs(&args, &task_name)?;
    let stager = Arc::new(SvsTickStager::new());
    let dynamics_slot = Arc::new(SvsDynamicsSlot::new());
    let feedback_history = Arc::new(Mutex::new(AppliedMasterFeedbackHistory::default()));
    let resolved_mujoco = resolve_mujoco_from_args(&args, &resolved_profile.profile)?;
    let bridge = SvsMujocoBridge::new(
        SvsMujocoBridgeConfig {
            model_source: resolved_mujoco.bridge_source.clone(),
            compensator: resolved_mujoco.compensator.clone(),
            master_ee_site: resolved_profile.profile.mujoco.master_ee_site.clone(),
            slave_ee_site: resolved_profile.profile.mujoco.slave_ee_site.clone(),
        },
        Arc::clone(&dynamics_slot),
    )
    .context("failed to initialize MuJoCo bridge before MIT enable")?;

    let backend = RealCollectorBackend {
        master_target: master_target.clone(),
        slave_target: slave_target.clone(),
        baud_rate: args.baud_rate,
    };
    let standby = backend.connect()?;
    let resolved_calibration = resolve_real_calibration(
        &args,
        &standby,
        resolved_profile.runtime_mirror_map,
        started_unix_ns / 1_000_000,
        resolved_profile.profile.calibration.calibration_max_error_rad,
    )?;

    let reservation = EpisodeId::reserve_directory(
        &args.output_dir,
        &task_name,
        timestamp,
        &OsEpisodeRng,
        EPISODE_ID_ATTEMPTS,
    )?;
    let episode_id = reservation.id.episode_id.clone();
    let episode_dir = reservation.absolute_dir;

    write_new_file(
        episode_dir.join("effective_profile.toml"),
        &resolved_profile.effective_profile_bytes,
    )?;
    write_new_file(
        episode_dir.join("calibration.toml"),
        &resolved_calibration.resolved.canonical_bytes,
    )?;
    if let Some(path) = args.save_calibration.as_ref() {
        crate::calibration::persist_calibration_no_overwrite(
            path,
            &resolved_calibration.resolved.canonical_bytes,
        )
        .with_context(|| format!("failed to save calibration to {}", path.display()))?;
    }

    let raw_can = Arc::new(if args.raw_can {
        RawCanStatusTracker::requested()
    } else {
        RawCanStatusTracker::disabled()
    });
    let base = build_real_manifest_base(RealManifestInputs {
        episode_id: episode_id.clone(),
        task_raw_name: task_name,
        task_slug: reservation.id.task_slug,
        operator: args.operator.clone(),
        notes: args.notes.clone(),
        started_unix_ns,
        episode_start_host_mono_us,
        master_requested: args.master_target.clone(),
        slave_requested: args.slave_target.clone(),
        master_target: master_target.clone(),
        slave_target: slave_target.clone(),
        mujoco: resolved_mujoco.manifest,
        profile: &resolved_profile.profile,
        task_profile: resolved_profile.task_profile,
        effective_profile_hash: resolved_profile.effective_profile_hash,
        mirror_map: resolved_profile.mirror_map,
        calibration: &resolved_calibration.resolved.calibration,
        calibration_hash: resolved_calibration.resolved.sha256_hex,
        raw_can_enabled: args.raw_can,
        disable_gripper_requested: args.disable_gripper_mirror,
    })?;
    let context = RealRunContext {
        episode_id: episode_id.clone(),
        episode_dir: episode_dir.clone(),
        base,
        raw_can: Arc::clone(&raw_can),
    };
    write_real_manifest(
        &context,
        EpisodeStatus::Running,
        None,
        &crate::episode::wire::StepFileSummary {
            episode_id: episode_id.clone(),
            step_count: 0,
            last_step_index: None,
        },
        "running".to_string(),
        true,
    )?;

    let header = SvsHeaderV1::new(
        &episode_id,
        started_unix_ns / 1_000_000,
        episode_start_host_mono_us,
    )?;
    let writer_monitor = writer_backpressure_monitor_from_profile(&resolved_profile.profile);
    let writer = Arc::new(SharedEpisodeWriter::new(
        EpisodeWriter::new_with_backpressure_thresholds(
            &episode_dir,
            header,
            resolved_profile.profile.writer.queue_capacity,
            &writer_monitor,
        )?,
    ));
    let sink = Arc::new(SvsTelemetrySink::new(
        Arc::clone(&stager),
        Arc::clone(&feedback_history),
        Arc::clone(&writer),
        raw_can.clone(),
        writer_backpressure_monitor_from_profile(&resolved_profile.profile),
        episode_start_host_mono_us,
    ));

    let controller = match SvsController::with_shared(
        resolved_profile.profile.clone(),
        resolved_calibration.runtime,
        stager,
        dynamics_slot,
        feedback_history,
    ) {
        Ok(controller) => controller,
        Err(error) => {
            let mut report = BilateralRunReport {
                last_error: Some(error.to_string()),
                ..BilateralRunReport::default()
            };
            return finish_writer_and_finalize(
                &context,
                &writer,
                EpisodeStatus::Faulted,
                &mut report,
                0,
                false,
                None,
            );
        },
    };
    let telemetry_sink: Arc<dyn BilateralLoopTelemetrySink> = sink;
    let loop_config = match bilateral_loop_config_from_profile(
        &resolved_profile.profile,
        &args,
        &cancel,
        telemetry_sink,
    ) {
        Ok(loop_config) => loop_config,
        Err(error) => {
            let mut report = BilateralRunReport {
                last_error: Some(error.to_string()),
                ..BilateralRunReport::default()
            };
            return finish_writer_and_finalize(
                &context,
                &writer,
                EpisodeStatus::Faulted,
                &mut report,
                0,
                false,
                None,
            );
        },
    };

    let raw_can_recording = match start_raw_can_side_recording(
        args.raw_can,
        &episode_dir,
        &master_target,
        &slave_target,
        cancel.loop_signal(),
        Arc::clone(&raw_can),
    ) {
        Ok(recording) => recording,
        Err(error) => {
            let mut report = BilateralRunReport {
                last_error: Some(error.to_string()),
                ..BilateralRunReport::default()
            };
            return finish_writer_and_finalize(
                &context,
                &writer,
                EpisodeStatus::Faulted,
                &mut report,
                0,
                false,
                None,
            );
        },
    };

    let operator_confirmed = match confirm_start_if_needed(&args, &cancel) {
        Ok(confirmed) => confirmed,
        Err(error) => {
            let mut report = BilateralRunReport {
                last_error: Some(error.to_string()),
                ..BilateralRunReport::default()
            };
            return finish_writer_and_finalize(
                &context,
                &writer,
                EpisodeStatus::Faulted,
                &mut report,
                0,
                false,
                raw_can_recording,
            );
        },
    };

    if cancel.is_cancelled() || !operator_confirmed {
        let mut report = BilateralRunReport {
            exit_reason: Some(BilateralExitReason::Cancelled),
            last_error: Some("collector cancelled before MIT enable".to_string()),
            ..BilateralRunReport::default()
        };
        return finish_writer_and_finalize(
            &context,
            &writer,
            EpisodeStatus::Cancelled,
            &mut report,
            0,
            false,
            raw_can_recording,
        );
    }

    let active = match backend.enable_mit(standby) {
        Ok(active) => active,
        Err(error) => {
            let mut report = BilateralRunReport {
                last_error: Some(error.to_string()),
                ..BilateralRunReport::default()
            };
            return finish_writer_and_finalize(
                &context,
                &writer,
                EpisodeStatus::Faulted,
                &mut report,
                0,
                false,
                raw_can_recording,
            );
        },
    };

    let mut outcome = match backend.run_loop(
        active,
        RealCollectorRuntime {
            controller,
            bridge,
            loop_config,
        },
    ) {
        Ok(outcome) => outcome,
        Err(error) => {
            let mut report = BilateralRunReport {
                last_error: Some(error.to_string()),
                ..BilateralRunReport::default()
            };
            return finish_writer_and_finalize(
                &context,
                &writer,
                EpisodeStatus::Faulted,
                &mut report,
                1,
                false,
                raw_can_recording,
            );
        },
    };

    finish_writer_and_finalize(
        &context,
        &writer,
        outcome.status,
        &mut outcome.report,
        1,
        outcome.disable_called,
        raw_can_recording,
    )
}

fn resolve_profile_from_args(args: &Args) -> Result<ResolvedProfile> {
    let (mut profile, task_profile) = if let Some(path) = args.task_profile.as_ref() {
        let bytes = fs::read(path)
            .with_context(|| format!("failed to read task profile {}", path.display()))?;
        let text = std::str::from_utf8(&bytes)
            .with_context(|| format!("task profile is not UTF-8: {}", path.display()))?;
        let profile = load_effective_profile_overlay(text)
            .with_context(|| format!("failed to resolve task profile {}", path.display()))?;
        (
            profile,
            SourceArtifactManifest {
                source_kind: ProfileSourceKind::File,
                source_path: Some(path.clone()),
                hash_algorithm: Some("sha256".to_string()),
                sha256_hex: Some(sha256_hex(&bytes)),
            },
        )
    } else {
        (
            EffectiveProfile::default(),
            SourceArtifactManifest {
                source_kind: ProfileSourceKind::BuiltInDefaults,
                source_path: None,
                hash_algorithm: None,
                sha256_hex: None,
            },
        )
    };

    let mirror_map = apply_mirror_map_override(args, &mut profile)?;
    if let Some(max_error_rad) = args.calibration_max_error_rad {
        profile.calibration.calibration_max_error_rad = max_error_rad;
    }
    if args.disable_gripper_mirror {
        profile.gripper.mirror_enabled = false;
    }
    profile.validate()?;
    let runtime_mirror_map = joint_mirror_map_from_profile(&profile)?;
    let effective_profile_bytes = profile.to_canonical_toml_bytes()?;
    let effective_profile_hash = sha256_hex(&effective_profile_bytes);

    Ok(ResolvedProfile {
        profile,
        effective_profile_bytes,
        effective_profile_hash,
        task_profile,
        mirror_map,
        runtime_mirror_map,
    })
}

fn load_effective_profile_overlay(overlay_text: &str) -> Result<EffectiveProfile> {
    let default_text = String::from_utf8(EffectiveProfile::default().to_canonical_toml_bytes()?)
        .context("default effective profile is not UTF-8")?;
    let mut merged: toml::Value = toml::from_str(&default_text)?;
    let overlay: toml::Value = toml::from_str(overlay_text)?;
    merge_toml_overlay(&mut merged, overlay)?;
    let profile: EffectiveProfile = merged.try_into()?;
    Ok(profile)
}

fn merge_toml_overlay(base: &mut toml::Value, overlay: toml::Value) -> Result<()> {
    match (base, overlay) {
        (toml::Value::Table(base_table), toml::Value::Table(overlay_table)) => {
            for (key, value) in overlay_table {
                match base_table.get_mut(&key) {
                    Some(base_value) => merge_toml_overlay(base_value, value)
                        .with_context(|| format!("invalid task profile override at {key}"))?,
                    None => {
                        return Err(anyhow!(
                            "task profile override contains unknown key {key:?}"
                        ));
                    },
                }
            }
            Ok(())
        },
        (base_value, overlay_value) => {
            *base_value = overlay_value;
            Ok(())
        },
    }
}

fn apply_mirror_map_override(
    args: &Args,
    profile: &mut EffectiveProfile,
) -> Result<MirrorMapManifest> {
    let Some(value) = args.mirror_map.as_deref() else {
        return Ok(MirrorMapManifest {
            source_kind: Some(ProfileSourceKind::Generated),
            source_path: None,
            hash_algorithm: None,
            sha256_hex: None,
            effective: joint_mirror_map_manifest(&joint_mirror_map_from_profile(profile)?),
        });
    };

    if value == "left-right" {
        profile.calibration.mirror_map_kind = "left-right".to_string();
        profile.calibration.mirror_map = crate::profile::MirrorMapProfile::left_right_mirror();
        return Ok(MirrorMapManifest {
            source_kind: Some(ProfileSourceKind::BuiltInDefaults),
            source_path: None,
            hash_algorithm: None,
            sha256_hex: None,
            effective: joint_mirror_map_manifest(&JointMirrorMap::left_right_mirror()),
        });
    }

    let path = PathBuf::from(value);
    let bytes =
        fs::read(&path).with_context(|| format!("failed to read mirror map {}", path.display()))?;
    let loaded = load_file_backed_mirror_map_bytes(&bytes)
        .with_context(|| format!("failed to load mirror map {}", path.display()))?;
    profile.calibration.mirror_map_kind = "custom".to_string();
    profile.calibration.mirror_map = crate::profile::MirrorMapProfile {
        permutation: loaded.mirror_map.permutation,
        position_sign: loaded.mirror_map.position_sign,
        velocity_sign: loaded.mirror_map.velocity_sign,
        torque_sign: loaded.mirror_map.torque_sign,
    };

    Ok(MirrorMapManifest {
        source_kind: Some(ProfileSourceKind::File),
        source_path: Some(path),
        hash_algorithm: Some("sha256".to_string()),
        sha256_hex: Some(loaded.sha256_hex),
        effective: joint_mirror_map_manifest(&loaded.runtime_map),
    })
}

fn resolve_mujoco_from_args(args: &Args, profile: &EffectiveProfile) -> Result<ResolvedMujoco> {
    let source_count = usize::from(args.model_dir.is_some())
        + usize::from(args.use_standard_model_path)
        + usize::from(args.use_embedded_model);
    if source_count > 1 {
        return Err(anyhow!(
            "choose exactly one MuJoCo model source: --model-dir, --use-standard-model-path, or --use-embedded-model"
        ));
    }

    let (bridge_source, source_kind, source_path, model_hash) = if let Some(model_dir) =
        args.model_dir.as_ref()
    {
        let model_hash = hash_model_dir(model_dir, DEFAULT_MUJOCO_ROOT_XML)
            .with_context(|| format!("failed to hash MuJoCo model dir {}", model_dir.display()))?;
        (
            SvsMujocoModelSource::ModelDir(model_dir.clone()),
            ProfileSourceKind::File,
            Some(model_dir.clone()),
            model_hash,
        )
    } else if args.use_embedded_model {
        (
            SvsMujocoModelSource::Embedded,
            ProfileSourceKind::BuiltInDefaults,
            None,
            hash_embedded_model(EMBEDDED_MUJOCO_XML)?,
        )
    } else {
        let model_dir = find_standard_model_dir()?;
        let model_hash =
            hash_model_dir(&model_dir, DEFAULT_MUJOCO_ROOT_XML).with_context(|| {
                format!(
                    "failed to hash standard MuJoCo model dir {}",
                    model_dir.display()
                )
            })?;
        (
            SvsMujocoModelSource::StandardPath,
            ProfileSourceKind::File,
            Some(model_dir),
            model_hash,
        )
    };

    let runtime = current_mujoco_runtime_identity()
        .context("failed to prove MuJoCo runtime identity before MIT enable")?;
    let compensator = mujoco_compensator_config_from_profile(profile)?;
    let manifest = MujocoManifest {
        source_kind,
        source_path: source_path.clone(),
        root_xml_relative_path: model_hash.root_xml_relative_path.clone(),
        model: HashedArtifactManifest {
            source_kind,
            source_path,
            hash_algorithm: model_hash.hash_algorithm,
            sha256_hex: model_hash.sha256_hex,
        },
        runtime,
        master_ee_site: profile.mujoco.master_ee_site.clone(),
        slave_ee_site: profile.mujoco.slave_ee_site.clone(),
    };

    Ok(ResolvedMujoco {
        bridge_source,
        manifest,
        compensator,
    })
}

fn resolve_real_calibration(
    args: &Args,
    standby: &DualArmStandby,
    runtime_map: JointMirrorMap,
    created_unix_ms: u64,
    max_error_rad: f64,
) -> Result<ResolvedRealCalibration> {
    let loaded = if let Some(path) = args.calibration_file.as_ref() {
        let bytes = fs::read(path)
            .with_context(|| format!("failed to read calibration {}", path.display()))?;
        Some(
            CalibrationFile::from_canonical_bytes(&bytes)
                .with_context(|| format!("failed to parse calibration {}", path.display()))?,
        )
    } else {
        None
    };

    let captured = if loaded.is_none() {
        let captured = standby
            .capture_calibration(runtime_map)
            .context("failed to capture calibration")?;
        Some(calibration_file_from_dual_arm(&captured, created_unix_ms))
    } else {
        None
    };
    let resolved = resolve_episode_calibration(loaded, runtime_map, captured)?;
    let runtime = DualArmCalibration {
        master_zero: JointArray::new(resolved.calibration.master_zero_rad.map(Rad)),
        slave_zero: JointArray::new(resolved.calibration.slave_zero_rad.map(Rad)),
        map: runtime_map,
    };

    let snapshot = standby
        .observer()
        .snapshot(DualArmReadPolicy::default())
        .context("failed to read current posture for calibration validation")?;
    validate_current_posture(
        &resolved.calibration,
        snapshot.left.state.position.into_array().map(|value| value.0),
        snapshot.right.state.position.into_array().map(|value| value.0),
        max_error_rad,
    )?;

    Ok(ResolvedRealCalibration { resolved, runtime })
}

fn build_real_manifest_base(inputs: RealManifestInputs<'_>) -> Result<RealEpisodeBase> {
    let profile = inputs.profile;
    let effective_map = inputs.mirror_map.effective;
    let manifest = ManifestV1 {
        schema_version: 1,
        episode_id: inputs.episode_id,
        task: TaskManifest {
            raw_name: inputs.task_raw_name,
            slug: inputs.task_slug,
            operator: inputs.operator,
            notes: inputs.notes,
        },
        timestamps: crate::episode::manifest::EpisodeTimestamps {
            started_unix_ns: inputs.started_unix_ns,
            ended_unix_ns: None,
            episode_start_host_mono_us: inputs.episode_start_host_mono_us,
        },
        status: EpisodeStatus::Running,
        targets: TargetSpecsManifest {
            master: TargetSpecManifest {
                requested: inputs.master_requested,
                resolved: format!("socketcan:{}", inputs.master_target.iface),
                backend: "socketcan".to_string(),
                identifier: inputs.master_target.iface,
            },
            slave: TargetSpecManifest {
                requested: inputs.slave_requested,
                resolved: format!("socketcan:{}", inputs.slave_target.iface),
                backend: "socketcan".to_string(),
                identifier: inputs.slave_target.iface,
            },
        },
        mujoco: inputs.mujoco,
        dynamics: DynamicsManifest {
            master: ArmDynamicsManifest {
                mode: profile.dynamics.master_mode.clone(),
                payload: PayloadManifest {
                    mass_kg: profile.dynamics.master_payload.mass_kg,
                    com_m: profile.dynamics.master_payload.com_m,
                },
            },
            slave: ArmDynamicsManifest {
                mode: profile.dynamics.slave_mode.clone(),
                payload: PayloadManifest {
                    mass_kg: profile.dynamics.slave_payload.mass_kg,
                    com_m: profile.dynamics.slave_payload.com_m,
                },
            },
            qacc_lpf_cutoff_hz: profile.dynamics.qacc_lpf_cutoff_hz,
            acceleration_clamp: profile.dynamics.max_abs_qacc,
        },
        task_profile: inputs.task_profile,
        effective_profile: EffectiveProfileManifest {
            path: PathBuf::from("effective_profile.toml"),
            hash_algorithm: "sha256".to_string(),
            sha256_hex: inputs.effective_profile_hash,
        },
        gripper: GripperMirrorManifest {
            mirror_enabled: profile.gripper.mirror_enabled,
            disable_gripper_requested: inputs.disable_gripper_requested,
            disable_gripper_effective: !profile.gripper.mirror_enabled,
            update_divider: profile.gripper.update_divider,
            position_deadband: profile.gripper.position_deadband,
            effort_scale: profile.gripper.effort_scale,
            max_feedback_age_ms: profile.gripper.max_feedback_age_ms,
        },
        calibration: CalibrationManifest {
            source_path: Some(PathBuf::from("calibration.toml")),
            hash_algorithm: "sha256".to_string(),
            sha256_hex: inputs.calibration_hash,
            master_zero_rad: inputs.calibration.master_zero_rad,
            slave_zero_rad: inputs.calibration.slave_zero_rad,
            effective_mirror_map: effective_map,
        },
        mirror_map: inputs.mirror_map,
        collector: CollectorIdentityManifest {
            version: Some(env!("CARGO_PKG_VERSION").to_string()),
            revision: option_env!("GIT_COMMIT").map(str::to_string),
        },
        raw_can: RawCanManifest {
            enabled: inputs.raw_can_enabled,
            finalizer_status: Some("running".to_string()),
        },
        step_file: StepFileManifest {
            relative_path: PathBuf::from("steps.bin"),
            step_count: 0,
            last_step_index: None,
        },
    };
    manifest.validate()?;

    Ok(RealEpisodeBase {
        manifest,
        started_unix_ns: inputs.started_unix_ns,
    })
}

fn write_real_manifest(
    context: &RealRunContext,
    status: EpisodeStatus,
    ended_unix_ns: Option<u64>,
    summary: &crate::episode::wire::StepFileSummary,
    raw_can_finalizer_status: String,
    create_new: bool,
) -> Result<()> {
    let mut manifest = context.base.manifest.clone();
    manifest.status = status;
    manifest.timestamps.ended_unix_ns = ended_unix_ns;
    manifest.raw_can.finalizer_status = Some(raw_can_finalizer_status);
    manifest.step_file.step_count = summary.step_count;
    manifest.step_file.last_step_index = summary.last_step_index;
    manifest.validate()?;

    let text = toml::to_string_pretty(&manifest)?;
    let path = context.episode_dir.join("manifest.toml");
    if create_new {
        write_new_file(path, text.as_bytes())
    } else {
        fs::write(&path, text).with_context(|| format!("failed to write {}", path.display()))
    }
}

fn finish_writer_and_finalize(
    context: &RealRunContext,
    writer: &Arc<SharedEpisodeWriter>,
    mut status: EpisodeStatus,
    report: &mut BilateralRunReport,
    enable_mit_calls: u32,
    disable_called: bool,
    raw_can_recording: Option<RawCanRecordingHandle>,
) -> Result<CollectorRunResult> {
    if let Some(recording) = raw_can_recording {
        recording.stop();
    }

    let writer_stats_before_finish = writer.writer_stats();
    let flush_summary = writer.finish();
    let mut writer_stats = merge_writer_stats(writer_stats_before_finish, writer.writer_stats());
    let final_flush_result = match flush_summary {
        Ok(_summary) => WriterFlushResultJson {
            success: true,
            error: None,
        },
        Err(error) => {
            status = EpisodeStatus::Faulted;
            writer_stats.flush_failed = true;
            writer_stats.flush_error = Some(error.to_string());
            if report.last_error.is_none() {
                report.last_error = Some(error.to_string());
            }
            WriterFlushResultJson {
                success: false,
                error: Some(error.to_string()),
            }
        },
    };

    if writer_stats.queue_full_events > 0 || writer_stats.dropped_step_count > 0 {
        status = EpisodeStatus::Faulted;
        report.exit_reason = Some(BilateralExitReason::TelemetrySinkFault);
    }

    let summary = crate::episode::wire::read_steps_file(context.episode_dir.join("steps.bin"))
        .map(|decoded| decoded.summary)
        .unwrap_or_else(|_| crate::episode::wire::StepFileSummary {
            episode_id: context.episode_id.clone(),
            step_count: writer_stats.encoded_step_count,
            last_step_index: writer_stats.last_step_index,
        });
    let ended_unix_ns = current_unix_ns().max(context.base.started_unix_ns);
    write_real_manifest(
        context,
        status,
        Some(ended_unix_ns),
        &summary,
        context.raw_can.finalizer_status(),
        false,
    )?;
    write_real_report(
        context,
        status,
        ended_unix_ns,
        &summary,
        report,
        final_flush_result,
        &writer_stats,
    )?;

    Ok(CollectorRunResult {
        status,
        path: context.episode_dir.clone(),
        dual_arm_exit_reason: report.exit_reason,
        loop_stopped_before_requested_iterations: status != EpisodeStatus::Complete,
        enable_mit_calls,
        disable_called,
    })
}

fn write_real_report(
    context: &RealRunContext,
    status: EpisodeStatus,
    ended_unix_ns: u64,
    summary: &crate::episode::wire::StepFileSummary,
    report: &BilateralRunReport,
    final_flush_result: WriterFlushResultJson,
    writer_stats: &WriterStats,
) -> Result<()> {
    let report_json = ReportJson {
        schema_version: 1,
        episode_id: context.episode_id.clone(),
        status,
        fault_classification: (status == EpisodeStatus::Faulted).then(|| {
            if writer_stats.queue_full_events > 0 {
                "writer_queue_full".to_string()
            } else if writer_stats.flush_failed {
                "writer_flush_failed".to_string()
            } else {
                report
                    .exit_reason
                    .map(|reason| format!("{reason:?}"))
                    .unwrap_or_else(|| "collector_fault".to_string())
            }
        }),
        raw_can_enabled: context.raw_can.raw_can_status() != RawCanCaptureStatus::Disabled,
        raw_can_degraded: context.raw_can.raw_can_status() == RawCanCaptureStatus::Degraded,
        raw_can_finalizer_status: Some(context.raw_can.finalizer_status()),
        final_flush_result,
        started_unix_ns: context.base.started_unix_ns,
        ended_unix_ns,
        step_count: summary.step_count,
        last_step_index: summary.last_step_index,
        dual_arm: DualArmReportJson {
            iterations: report.iterations as u64,
            read_faults: report.read_faults,
            submission_faults: report.submission_faults,
            last_submission_failed_arm: report
                .last_submission_failed_arm
                .map(|arm| format!("{arm:?}")),
            peer_command_may_have_applied: report.peer_command_may_have_applied,
            deadline_misses: report.deadline_misses,
            max_inter_arm_skew_ns: duration_nanos_u64(report.max_inter_arm_skew),
            max_real_dt_ns: duration_nanos_u64(report.max_real_dt),
            max_cycle_lag_ns: duration_nanos_u64(report.max_cycle_lag),
            left_tx_realtime_overwrites_total: report.left_tx_realtime_overwrites_total,
            right_tx_realtime_overwrites_total: report.right_tx_realtime_overwrites_total,
            left_tx_frames_sent_total: report.left_tx_frames_sent_total,
            right_tx_frames_sent_total: report.right_tx_frames_sent_total,
            left_tx_fault_aborts_total: report.left_tx_fault_aborts_total,
            right_tx_fault_aborts_total: report.right_tx_fault_aborts_total,
            last_runtime_fault_left: report
                .last_runtime_fault_left
                .map(|fault| format!("{fault:?}")),
            last_runtime_fault_right: report
                .last_runtime_fault_right
                .map(|fault| format!("{fault:?}")),
            exit_reason: report.exit_reason.map(|reason| format!("{reason:?}")),
            left_stop_attempt: format!("{:?}", report.left_stop_attempt),
            right_stop_attempt: format!("{:?}", report.right_stop_attempt),
            last_error: report.last_error.clone(),
        },
        writer: WriterReportJson::from(writer_stats),
    };
    report_json.validate()?;
    let text = serde_json::to_string_pretty(&report_json)?;
    fs::write(context.episode_dir.join("report.json"), text)?;
    Ok(())
}

fn bilateral_loop_config_from_profile(
    profile: &EffectiveProfile,
    args: &Args,
    cancel: &CollectorCancelToken,
    telemetry_sink: Arc<dyn BilateralLoopTelemetrySink>,
) -> Result<BilateralLoopConfig> {
    let warmup_cycles = profile.control.warmup_cycles as usize;
    let mut config = BilateralLoopConfig {
        frequency_hz: profile.control.loop_frequency_hz,
        dt_clamp_multiplier: profile.control.dt_clamp_multiplier,
        timing_mode: parse_timing_mode(&args.timing_mode)?,
        warmup_cycles,
        max_iterations: args.max_iterations.map(|value| {
            usize::try_from(value).unwrap_or(usize::MAX).saturating_add(warmup_cycles)
        }),
        cancel_signal: Some(cancel.loop_signal()),
        submission_mode: BilateralSubmissionMode::ConfirmedTxFinished,
        telemetry_sink: Some(telemetry_sink),
        read_policy: DualArmReadPolicy::default(),
        safety: piper_client::DualArmSafetyConfig::default(),
        disable_config: DisableConfig::default(),
        gripper: GripperTeleopConfig {
            enabled: profile.gripper.mirror_enabled,
            update_divider: profile.gripper.update_divider as usize,
            position_deadband: profile.gripper.position_deadband,
            effort_scale: profile.gripper.effort_scale,
            max_feedback_age: Duration::from_millis(profile.gripper.max_feedback_age_ms),
        },
        master_interaction_lpf_cutoff_hz: profile.control.master_interaction_lpf_cutoff_hz,
        master_interaction_limit: JointArray::new(
            profile.control.master_interaction_limit_nm.map(NewtonMeter),
        ),
        slave_feedforward_limit: JointArray::new(
            profile.control.slave_feedforward_limit_nm.map(NewtonMeter),
        ),
        master_interaction_slew_limit_nm_per_s: JointArray::new(
            profile.control.master_interaction_slew_limit_nm_per_s.map(NewtonMeter),
        ),
        master_passivity_enabled: profile.control.master_passivity_enabled,
        master_passivity_max_damping: JointArray::new(profile.control.master_passivity_max_damping),
    };
    if args.disable_gripper_mirror {
        config.gripper.enabled = false;
    }
    Ok(config)
}

fn writer_backpressure_monitor_from_profile(
    profile: &EffectiveProfile,
) -> WriterBackpressureMonitor {
    WriterBackpressureMonitor::new(
        profile.writer.queue_full_stop_events as u64,
        Duration::from_millis(profile.writer.queue_full_stop_duration_ms),
    )
}

fn mujoco_compensator_config_from_profile(
    profile: &EffectiveProfile,
) -> Result<DualArmMujocoCompensatorConfig> {
    Ok(DualArmMujocoCompensatorConfig {
        master_mode: SharedModeState::new(DynamicsMode::from_str(&profile.dynamics.master_mode)?),
        slave_mode: SharedModeState::new(DynamicsMode::from_str(&profile.dynamics.slave_mode)?),
        master_payload: SharedPayloadState::try_new(PayloadSpec::try_new(
            profile.dynamics.master_payload.mass_kg,
            profile.dynamics.master_payload.com_m,
        )?)?,
        slave_payload: SharedPayloadState::try_new(PayloadSpec::try_new(
            profile.dynamics.slave_payload.mass_kg,
            profile.dynamics.slave_payload.com_m,
        )?)?,
        qacc_lpf_cutoff_hz: profile.dynamics.qacc_lpf_cutoff_hz,
        max_abs_qacc: profile.dynamics.max_abs_qacc,
    })
}

fn piper_builder_for_target(target: &SocketCanTarget, baud_rate: Option<u32>) -> PiperBuilder {
    let builder = PiperBuilder::new().socketcan(target.iface.clone());
    if let Some(baud_rate) = baud_rate {
        builder.baud_rate(baud_rate)
    } else {
        builder
    }
}

fn confirm_start_if_needed(args: &Args, cancel: &CollectorCancelToken) -> Result<bool> {
    if args.yes {
        return Ok(true);
    }
    if cancel.is_cancelled() {
        return Ok(false);
    }

    eprint!("Start SVS collection and enable MIT mode? Type 'yes' to continue: ");
    io::stderr().flush()?;
    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    Ok(answer.trim() == "yes" && !cancel.is_cancelled())
}

fn validate_static_startup_inputs(args: &Args, task_name: &str) -> Result<()> {
    slugify_task_name(task_name)?;
    parse_timing_mode(&args.timing_mode)?;

    if args.output_dir.as_os_str().is_empty() {
        return Err(anyhow!("--output-dir must not be empty"));
    }
    if args.output_dir.exists() {
        if !args.output_dir.is_dir() {
            return Err(anyhow!(
                "--output-dir exists but is not a directory: {}",
                args.output_dir.display()
            ));
        }
        return Ok(());
    }
    if let Some(parent) = args.output_dir.parent()
        && !parent.as_os_str().is_empty()
        && !parent.is_dir()
    {
        return Err(anyhow!(
            "--output-dir parent does not exist or is not a directory: {}",
            parent.display()
        ));
    }
    Ok(())
}

fn start_raw_can_side_recording(
    requested: bool,
    episode_dir: &Path,
    master_target: &SocketCanTarget,
    slave_target: &SocketCanTarget,
    cancel_signal: Arc<std::sync::atomic::AtomicBool>,
    status: Arc<RawCanStatusTracker>,
) -> Result<Option<RawCanRecordingHandle>> {
    RawCanRecordingHandle::start(
        requested,
        episode_dir,
        &master_target.iface,
        &slave_target.iface,
        cancel_signal,
        status,
    )
}

fn episode_status_from_report(report: &BilateralRunReport, faulted_variant: bool) -> EpisodeStatus {
    if faulted_variant {
        return EpisodeStatus::Faulted;
    }
    match report.exit_reason {
        Some(BilateralExitReason::Cancelled) => EpisodeStatus::Cancelled,
        Some(BilateralExitReason::MaxIterations) | None => EpisodeStatus::Complete,
        Some(_) => EpisodeStatus::Faulted,
    }
}

fn joint_mirror_map_from_profile(profile: &EffectiveProfile) -> Result<JointMirrorMap> {
    Ok(JointMirrorMap {
        permutation: profile
            .calibration
            .mirror_map
            .permutation
            .map(|index| Joint::from_index(index).expect("profile validation ensures 0..6")),
        position_sign: profile.calibration.mirror_map.position_sign,
        velocity_sign: profile.calibration.mirror_map.velocity_sign,
        torque_sign: profile.calibration.mirror_map.torque_sign,
    })
}

fn joint_mirror_map_manifest(map: &JointMirrorMap) -> JointMirrorMapManifest {
    JointMirrorMapManifest {
        permutation: map.permutation.map(Joint::index),
        position_sign: map.position_sign,
        velocity_sign: map.velocity_sign,
        torque_sign: map.torque_sign,
    }
}

fn calibration_file_from_dual_arm(
    calibration: &DualArmCalibration,
    created_unix_ms: u64,
) -> CalibrationFile {
    CalibrationFile {
        schema_version: 1,
        created_unix_ms,
        master_zero_rad: calibration.master_zero.into_array().map(|value| value.0),
        slave_zero_rad: calibration.slave_zero.into_array().map(|value| value.0),
        mirror_map: MirrorMapFile {
            schema_version: 1,
            permutation: calibration.map.permutation.map(Joint::index),
            position_sign: calibration.map.position_sign,
            velocity_sign: calibration.map.velocity_sign,
            torque_sign: calibration.map.torque_sign,
        },
    }
}

fn parse_timing_mode(value: &str) -> Result<LoopTimingMode> {
    match value {
        "spin" => Ok(LoopTimingMode::Spin),
        "sleep" => Ok(LoopTimingMode::Sleep),
        _ => Err(anyhow!(
            "unsupported timing mode {value:?}; expected 'spin' or 'sleep'"
        )),
    }
}

fn find_standard_model_dir() -> Result<PathBuf> {
    if let Ok(path) = std::env::var("PIPER_MODEL_PATH") {
        let dir = PathBuf::from(path);
        if dir.is_dir() {
            return Ok(dir);
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        let dir = PathBuf::from(home).join(".piper/models");
        if dir.is_dir() {
            return Ok(dir);
        }
    }
    for candidate in [
        PathBuf::from("/usr/local/share/piper/models"),
        PathBuf::from("./assets"),
    ] {
        if candidate.is_dir() {
            return Ok(candidate);
        }
    }
    Err(anyhow!(
        "standard MuJoCo model directory not found; set PIPER_MODEL_PATH or pass --model-dir"
    ))
}

fn current_unix_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
        .min(u128::from(u64::MAX)) as u64
}

fn utc_timestamp_from_unix_ns(unix_ns: u64) -> Result<UtcTimestamp> {
    let seconds = unix_ns / 1_000_000_000;
    let days = (seconds / 86_400) as i64;
    let seconds_of_day = seconds % 86_400;
    let (year, month, day) = civil_from_days(days);
    let hour = seconds_of_day / 3_600;
    let minute = (seconds_of_day % 3_600) / 60;
    let second = seconds_of_day % 60;
    UtcTimestamp::parse(&format!(
        "{year:04}{month:02}{day:02}T{hour:02}{minute:02}{second:02}Z"
    ))
    .map_err(Into::into)
}

fn civil_from_days(days_since_unix_epoch: i64) -> (i32, u32, u32) {
    let z = days_since_unix_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let mut year = yoe as i32 + (era as i32) * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = (mp + if mp < 10 { 3 } else { -9 }) as u32;
    if month <= 2 {
        year += 1;
    }
    (year, month, day)
}

fn duration_nanos_u64(duration: Duration) -> u64 {
    duration.as_nanos().min(u128::from(u64::MAX)) as u64
}

fn required_tx_finished(
    arm: &'static str,
    value: Option<u64>,
) -> std::result::Result<u64, SvsTelemetrySinkFault> {
    value
        .filter(|timestamp| *timestamp > 0)
        .ok_or(SvsTelemetrySinkFault::MissingTxFinished { arm })
}

fn step_from_telemetry(
    telemetry: &BilateralLoopTelemetry,
    pending: SvsPendingTick,
    context: StepBuildContext,
    writer_stats: &WriterStats,
) -> std::result::Result<SvsStepV1, SvsTelemetrySinkFault> {
    let snapshot = &telemetry.control_frame.snapshot;
    let host_mono_us = telemetry.timing.control_frame_host_mono_us;
    let episode_elapsed_us = host_mono_us
        .checked_sub(context.episode_start_host_mono_us)
        .ok_or(SvsTelemetrySinkFault::ControlFrameBeforeEpisodeStart)?;
    Ok(SvsStepV1 {
        step_index: context.step_index,
        host_mono_us,
        episode_elapsed_us,
        dt_us: telemetry.timing.clamped_dt_us,
        inter_arm_skew_us: snapshot.inter_arm_skew.as_micros().min(u128::from(u64::MAX)) as u64,
        deadline_missed: u8::from(telemetry.timing.deadline_missed),
        contact_state: pending.stiffness.contact_state.as_u8(),
        raw_can_status: context.raw_can_status.as_step_code(),
        master: arm_step(
            "master",
            &snapshot.left,
            &pending.dynamics.master_ee,
            pending.dynamics.master_model_torque_nm,
            host_mono_us,
        )?,
        slave: arm_step(
            "slave",
            &snapshot.right,
            &pending.dynamics.slave_ee,
            pending.dynamics.slave_model_torque_nm,
            host_mono_us,
        )?,
        tau_master_effort_residual_nm: pending.cues.tau_master_effort_residual_nm,
        tau_master_feedback_subtracted_nm: pending.cues.tau_master_feedback_subtracted_nm,
        u_ee_raw: pending.cues.u_ee_raw,
        r_ee_raw: pending.cues.r_ee_raw,
        u_ee: pending.cues.u_ee,
        r_ee: pending.cues.r_ee,
        k_state_raw_n_per_m: pending.stiffness.k_state_raw_n_per_m,
        k_state_clipped_n_per_m: pending.stiffness.k_state_clipped_n_per_m,
        k_tele_n_per_m: pending.stiffness.k_tele_n_per_m,
        reflection_gain_xyz: pending.controller_output.reflection_gain_xyz,
        reflection_residual_scale: pending.controller_output.reflection_residual_scale,
        command: command_step(
            telemetry,
            &pending,
            context.master_tx_finished_host_mono_us,
            context.slave_tx_finished_host_mono_us,
        ),
        gripper: gripper_step(&telemetry.gripper),
        writer_queue_depth: writer_stats.final_queue_depth,
        writer_queue_full_events: writer_stats.queue_full_events,
        dropped_step_count: writer_stats.dropped_step_count,
    })
}

fn arm_step(
    arm: &'static str,
    snapshot: &ControlSnapshotFull,
    ee: &EndEffectorKinematics,
    model_torque_nm: [f64; 6],
    control_frame_host_mono_us: u64,
) -> std::result::Result<SvsArmStepV1, SvsTelemetrySinkFault> {
    if snapshot.position_host_rx_mono_us == 0
        || snapshot.dynamic_host_rx_mono_us == 0
        || snapshot.position_host_rx_mono_us > control_frame_host_mono_us
        || snapshot.dynamic_host_rx_mono_us > control_frame_host_mono_us
    {
        return Err(SvsTelemetrySinkFault::InvalidFeedbackTimestamp { arm });
    }
    let latest_feedback_host_rx_mono_us =
        snapshot.position_host_rx_mono_us.max(snapshot.dynamic_host_rx_mono_us);

    let tau_measured_nm = torque_array(snapshot.state.torque);
    Ok(SvsArmStepV1 {
        position_hw_timestamp_us: snapshot.state.position_timestamp_us,
        dynamic_hw_timestamp_us: snapshot.state.dynamic_timestamp_us,
        position_host_rx_mono_us: snapshot.position_host_rx_mono_us,
        dynamic_host_rx_mono_us: snapshot.dynamic_host_rx_mono_us,
        feedback_age_us: control_frame_host_mono_us - latest_feedback_host_rx_mono_us,
        state_skew_us: snapshot.state.skew_us,
        q_rad: rad_array(snapshot.state.position),
        dq_rad_s: rad_per_second_array(snapshot.state.velocity),
        tau_measured_nm,
        tau_model_mujoco_nm: model_torque_nm,
        tau_residual_nm: subtract6(tau_measured_nm, model_torque_nm),
        ee_position_base_m: ee.position_base_m,
        rotation_base_from_ee_row_major: flatten3x3(ee.rotation_base_from_ee),
        translational_jacobian_base_row_major: flatten3x6(ee.translational_jacobian_base),
        jacobian_condition: ee.jacobian_condition,
    })
}

fn command_step(
    telemetry: &BilateralLoopTelemetry,
    pending: &SvsPendingTick,
    master_tx_finished_host_mono_us: u64,
    slave_tx_finished_host_mono_us: u64,
) -> SvsCommandStepV1 {
    SvsCommandStepV1 {
        master_tx_finished_host_mono_us,
        slave_tx_finished_host_mono_us,
        controller_slave_position_rad: pending.controller_output.controller_slave_position_rad,
        controller_slave_velocity_rad_s: pending.controller_output.controller_slave_velocity_rad_s,
        controller_slave_kp: pending.controller_output.controller_slave_kp,
        controller_slave_kd: pending.controller_output.controller_slave_kd,
        controller_slave_feedforward_nm: pending.controller_output.controller_slave_feedforward_nm,
        controller_master_position_rad: pending.controller_output.controller_master_position_rad,
        controller_master_velocity_rad_s: pending
            .controller_output
            .controller_master_velocity_rad_s,
        controller_master_kp: pending.controller_output.controller_master_kp,
        controller_master_kd: pending.controller_output.controller_master_kd,
        controller_master_interaction_nm: pending
            .controller_output
            .controller_master_interaction_nm,
        shaped_master_interaction_nm: torque_array(
            telemetry.shaped_command.master_interaction_torque,
        ),
        shaped_slave_feedforward_nm: torque_array(
            telemetry.shaped_command.slave_feedforward_torque,
        ),
        sdk_master_feedforward_nm: torque_array(telemetry.final_torques.master),
        sdk_slave_feedforward_nm: torque_array(telemetry.final_torques.slave),
        mit_master_t_ref_nm: telemetry
            .master_t_ref_nm
            .unwrap_or_else(|| torque_array(telemetry.final_torques.master)),
        mit_slave_t_ref_nm: telemetry
            .slave_t_ref_nm
            .unwrap_or_else(|| torque_array(telemetry.final_torques.slave)),
    }
}

fn gripper_step(gripper: &BilateralLoopGripperTelemetry) -> SvsGripperStepV1 {
    let include_command_values = matches!(
        gripper.command_status,
        BilateralGripperCommandStatus::Sent | BilateralGripperCommandStatus::Failed
    );
    SvsGripperStepV1 {
        mirror_enabled: u8::from(gripper.mirror_enabled),
        master_available: u8::from(gripper.master_available),
        slave_available: u8::from(gripper.slave_available),
        command_status: match gripper.command_status {
            BilateralGripperCommandStatus::None => 0,
            BilateralGripperCommandStatus::Sent => 1,
            BilateralGripperCommandStatus::Skipped => 2,
            BilateralGripperCommandStatus::Failed => 3,
        },
        master_hw_timestamp_us: gripper.master_hw_timestamp_us,
        slave_hw_timestamp_us: gripper.slave_hw_timestamp_us,
        master_host_rx_mono_us: gripper.master_host_rx_mono_us,
        slave_host_rx_mono_us: gripper.slave_host_rx_mono_us,
        master_age_us: gripper.master_age_us,
        slave_age_us: gripper.slave_age_us,
        master_position: gripper.master_position,
        master_effort: gripper.master_effort,
        slave_position: gripper.slave_position,
        slave_effort: gripper.slave_effort,
        command_position: if include_command_values {
            gripper.command_position
        } else {
            0.0
        },
        command_effort: if include_command_values {
            gripper.command_effort
        } else {
            0.0
        },
    }
}

#[derive(Debug)]
struct PersistedArtifacts {
    effective_profile_hash: String,
    calibration_hash: String,
}

struct FakeManifestWrite<'a> {
    episode_dir: &'a Path,
    episode_id: &'a str,
    status: EpisodeStatus,
    ended_unix_ns: Option<u64>,
    summary: &'a crate::episode::wire::StepFileSummary,
    artifacts: &'a PersistedArtifacts,
    raw_can_enabled: bool,
    raw_can_finalizer_status: String,
}

struct FakeReportWrite<'a> {
    episode_dir: &'a Path,
    episode_id: &'a str,
    status: EpisodeStatus,
    raw_can_enabled: bool,
    raw_can_degraded: bool,
    raw_can_finalizer_status: Option<String>,
    attempted_iterations: u64,
    summary: &'a crate::episode::wire::StepFileSummary,
    exit_reason: Option<BilateralExitReason>,
    final_flush_result: WriterFlushResultJson,
    writer_stats: &'a WriterStats,
}

fn persist_episode_inputs(episode_dir: &Path) -> Result<PersistedArtifacts> {
    let profile = fake_effective_profile();
    let effective_profile_bytes = profile.to_canonical_toml_bytes()?;
    let effective_profile_hash = sha256_hex(&effective_profile_bytes);
    write_new_file(
        episode_dir.join("effective_profile.toml"),
        &effective_profile_bytes,
    )?;

    let calibration = CalibrationFile::identity_for_tests();
    let calibration_bytes = calibration.to_canonical_toml_bytes()?;
    let calibration_hash = sha256_hex(&calibration_bytes);
    write_new_file(episode_dir.join("calibration.toml"), &calibration_bytes)?;

    Ok(PersistedArtifacts {
        effective_profile_hash,
        calibration_hash,
    })
}

fn write_manifest(input: FakeManifestWrite<'_>) -> Result<()> {
    let mut manifest = ManifestV1::for_test_complete();
    manifest.episode_id = input.episode_id.to_string();
    manifest.status = input.status;
    manifest.timestamps.episode_start_host_mono_us = EPISODE_START_HOST_MONO_US;
    manifest.timestamps.started_unix_ns = STARTED_UNIX_NS;
    manifest.timestamps.ended_unix_ns = input.ended_unix_ns;
    manifest.effective_profile.path = PathBuf::from("effective_profile.toml");
    manifest.effective_profile.sha256_hex = input.artifacts.effective_profile_hash.clone();
    manifest.calibration.source_path = Some(PathBuf::from("calibration.toml"));
    manifest.calibration.sha256_hex = input.artifacts.calibration_hash.clone();
    manifest.raw_can.enabled = input.raw_can_enabled;
    manifest.raw_can.finalizer_status = Some(input.raw_can_finalizer_status);
    manifest.step_file.step_count = input.summary.step_count;
    manifest.step_file.last_step_index = input.summary.last_step_index;
    manifest.validate()?;

    let text = toml::to_string_pretty(&manifest)?;
    fs::write(input.episode_dir.join("manifest.toml"), text)?;
    Ok(())
}

fn write_report(input: FakeReportWrite<'_>) -> Result<()> {
    let report = ReportJson {
        schema_version: 1,
        episode_id: input.episode_id.to_string(),
        status: input.status,
        fault_classification: (input.status == EpisodeStatus::Faulted).then(|| {
            if input.writer_stats.queue_full_events > 0 {
                "writer_queue_full".to_string()
            } else if input.writer_stats.flush_failed {
                "writer_flush_failed".to_string()
            } else {
                "collector_fault".to_string()
            }
        }),
        raw_can_enabled: input.raw_can_enabled,
        raw_can_degraded: input.raw_can_degraded,
        raw_can_finalizer_status: input.raw_can_finalizer_status,
        final_flush_result: input.final_flush_result,
        started_unix_ns: STARTED_UNIX_NS,
        ended_unix_ns: ENDED_UNIX_NS,
        step_count: input.summary.step_count,
        last_step_index: input.summary.last_step_index,
        dual_arm: DualArmReportJson {
            iterations: input.attempted_iterations.max(input.summary.step_count),
            read_faults: 0,
            submission_faults: 0,
            last_submission_failed_arm: None,
            peer_command_may_have_applied: false,
            deadline_misses: 0,
            max_inter_arm_skew_ns: 0,
            max_real_dt_ns: 0,
            max_cycle_lag_ns: 0,
            left_tx_realtime_overwrites_total: 0,
            right_tx_realtime_overwrites_total: 0,
            left_tx_frames_sent_total: input.summary.step_count,
            right_tx_frames_sent_total: input.summary.step_count,
            left_tx_fault_aborts_total: 0,
            right_tx_fault_aborts_total: 0,
            last_runtime_fault_left: None,
            last_runtime_fault_right: None,
            exit_reason: input.exit_reason.map(|reason| format!("{reason:?}")),
            left_stop_attempt: "ConfirmedSent".to_string(),
            right_stop_attempt: "ConfirmedSent".to_string(),
            last_error: input.writer_stats.flush_error.clone(),
        },
        writer: WriterReportJson::from(input.writer_stats),
    };
    report.validate()?;
    let text = serde_json::to_string_pretty(&report)?;
    fs::write(input.episode_dir.join("report.json"), text)?;
    Ok(())
}

fn write_new_file(path: impl AsRef<Path>, bytes: &[u8]) -> Result<()> {
    let path = path.as_ref();
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .with_context(|| format!("failed to create {}", path.display()))?;
    file.write_all(bytes)
        .with_context(|| format!("failed to write {}", path.display()))?;
    file.flush().with_context(|| format!("failed to flush {}", path.display()))?;
    Ok(())
}

fn fake_dynamics_for_step(step: usize, frame: &FakeMujocoFrame) -> SvsDynamicsFrame {
    let key = SnapshotKey {
        master_dynamic_host_rx_mono_us: frame.master_dynamic_host_rx_mono_us,
        slave_dynamic_host_rx_mono_us: frame.slave_dynamic_host_rx_mono_us,
    };
    SvsDynamicsFrame {
        key,
        master_model_torque_nm: [0.1; 6],
        slave_model_torque_nm: [0.2; 6],
        master_residual_nm: frame.master_residual_nm,
        slave_residual_nm: frame.slave_residual_nm,
        master_ee: fake_ee([0.1 + step as f64 * 0.01, 0.0, 0.0]),
        slave_ee: fake_ee([0.2 + step as f64 * 0.01, 0.0, 0.0]),
    }
}

fn fake_compensation_for_frame(frame: &FakeMujocoFrame) -> BilateralDynamicsCompensation {
    BilateralDynamicsCompensation {
        master_model_torque: JointArray::new([NewtonMeter(0.1); 6]),
        slave_model_torque: JointArray::new([NewtonMeter(0.2); 6]),
        master_external_torque_est: JointArray::new(frame.master_residual_nm.map(NewtonMeter)),
        slave_external_torque_est: JointArray::new(frame.slave_residual_nm.map(NewtonMeter)),
    }
}

fn telemetry_for_fake_control_frame(
    step: usize,
    control_frame: BilateralControlFrame,
    controller_command: BilateralCommand,
    master_tx_timeout: bool,
    gripper_timing: Option<GripperTiming>,
) -> BilateralLoopTelemetry {
    let host_mono_us = EPISODE_START_HOST_MONO_US + step as u64 * 5_000 + 5_000;
    let shaped_command = fake_command([0.4; 6], [0.25; 6]);
    BilateralLoopTelemetry {
        control_frame,
        controller_command,
        shaped_command,
        compensation: control_frame.compensation,
        gripper: fake_gripper(host_mono_us, gripper_timing),
        final_torques: BilateralFinalTorques {
            master: JointArray::new([NewtonMeter(0.5); 6]),
            slave: JointArray::new([NewtonMeter(0.6); 6]),
        },
        master_t_ref_nm: (!master_tx_timeout).then_some([0.5; 6]),
        slave_t_ref_nm: Some([0.6; 6]),
        master_tx_finished_host_mono_us: (!master_tx_timeout).then_some(host_mono_us + 100),
        slave_tx_finished_host_mono_us: Some(host_mono_us + 150),
        timing: BilateralLoopTimingTelemetry {
            scheduler_tick_start_host_mono_us: host_mono_us - 100,
            control_frame_host_mono_us: host_mono_us,
            previous_control_frame_host_mono_us: step
                .checked_sub(1)
                .map(|previous| EPISODE_START_HOST_MONO_US + previous as u64 * 5_000 + 5_000),
            raw_dt_us: 5_000,
            clamped_dt_us: 5_000,
            nominal_period_us: 5_000,
            submission_deadline_mono_us: host_mono_us + 4_000,
            deadline_missed: false,
        },
    }
}

fn sample_snapshot(frame: &FakeMujocoFrame, step: usize, host_mono_us: u64) -> DualArmSnapshot {
    DualArmSnapshot {
        left: control_snapshot_full(
            frame.master_dynamic_host_rx_mono_us,
            host_mono_us,
            step as f64,
            std::array::from_fn(|joint| 0.1 + frame.master_residual_nm[joint]),
        ),
        right: control_snapshot_full(
            frame.slave_dynamic_host_rx_mono_us,
            host_mono_us,
            step as f64,
            std::array::from_fn(|joint| 0.2 + frame.slave_residual_nm[joint]),
        ),
        inter_arm_skew: Duration::from_micros(100),
        host_cycle_timestamp: Instant::now(),
    }
}

fn control_snapshot_full(
    dynamic_host_rx_mono_us: u64,
    host_mono_us: u64,
    step_value: f64,
    torque: [f64; 6],
) -> ControlSnapshotFull {
    ControlSnapshotFull {
        state: ControlSnapshot {
            position: JointArray::new([Rad(0.1 + step_value * 0.001); 6]),
            velocity: JointArray::new([RadPerSecond(0.01); 6]),
            torque: JointArray::new(torque.map(NewtonMeter)),
            position_timestamp_us: dynamic_host_rx_mono_us.saturating_sub(50),
            dynamic_timestamp_us: dynamic_host_rx_mono_us,
            skew_us: 50,
        },
        position_host_rx_mono_us: dynamic_host_rx_mono_us.saturating_sub(50),
        dynamic_host_rx_mono_us,
        feedback_age: Duration::from_micros(host_mono_us.saturating_sub(dynamic_host_rx_mono_us)),
    }
}

fn fake_command(
    master_interaction_nm: [f64; 6],
    slave_feedforward_nm: [f64; 6],
) -> BilateralCommand {
    BilateralCommand {
        slave_position: JointArray::new([Rad(0.1); 6]),
        slave_velocity: JointArray::new([0.0; 6]),
        slave_kp: JointArray::new([2.0; 6]),
        slave_kd: JointArray::new([0.5; 6]),
        slave_feedforward_torque: JointArray::new(slave_feedforward_nm.map(NewtonMeter)),
        master_position: JointArray::new([Rad(0.1); 6]),
        master_velocity: JointArray::new([0.0; 6]),
        master_kp: JointArray::new([0.0; 6]),
        master_kd: JointArray::new([0.3; 6]),
        master_interaction_torque: JointArray::new(master_interaction_nm.map(NewtonMeter)),
    }
}

fn fake_gripper(host_mono_us: u64, timing: Option<GripperTiming>) -> BilateralLoopGripperTelemetry {
    let max_age_us = 100_000_i64;
    let offset_us = timing.map_or(-1_000, |value| value.offset_us);
    let available = offset_us <= 0 && -offset_us <= max_age_us;
    let rx_mono_us = if available {
        host_mono_us.saturating_sub((-offset_us) as u64)
    } else {
        0
    };
    let age_us = if available { (-offset_us) as u64 } else { 0 };
    BilateralLoopGripperTelemetry {
        mirror_enabled: true,
        master_available: available,
        slave_available: available,
        master_hw_timestamp_us: if available { rx_mono_us } else { 0 },
        slave_hw_timestamp_us: if available { rx_mono_us } else { 0 },
        master_host_rx_mono_us: rx_mono_us,
        slave_host_rx_mono_us: rx_mono_us,
        master_age_us: age_us,
        slave_age_us: age_us,
        master_position: if available { 0.25 } else { 0.0 },
        master_effort: if available { 0.1 } else { 0.0 },
        slave_position: if available { 0.25 } else { 0.0 },
        slave_effort: if available { 0.1 } else { 0.0 },
        command_status: BilateralGripperCommandStatus::Sent,
        command_position: if available { 0.25 } else { 0.0 },
        command_effort: if available { 0.1 } else { 0.0 },
    }
}

fn fake_ee(position_base_m: [f64; 3]) -> EndEffectorKinematics {
    EndEffectorKinematics {
        position_base_m,
        rotation_base_from_ee: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        translational_jacobian_base: [
            [1.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0, 0.0, 0.0],
        ],
        jacobian_condition: 1.0,
    }
}

fn fake_dual_arm_calibration() -> DualArmCalibration {
    DualArmCalibration {
        master_zero: JointArray::new([Rad(0.0); 6]),
        slave_zero: JointArray::new([Rad(0.0); 6]),
        map: JointMirrorMap::left_right_mirror(),
    }
}

fn fake_effective_profile() -> EffectiveProfile {
    let mut profile = EffectiveProfile::default_for_tests();
    profile.cue.w_u = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    profile.cue.w_r = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    profile
}

fn merge_writer_stats(mut first: WriterStats, second: WriterStats) -> WriterStats {
    first.queue_full_stop_events = first.queue_full_stop_events.max(second.queue_full_stop_events);
    first.queue_full_stop_duration_ms =
        first.queue_full_stop_duration_ms.max(second.queue_full_stop_duration_ms);
    first.queue_full_events = first.queue_full_events.max(second.queue_full_events);
    first.dropped_step_count = first.dropped_step_count.max(second.dropped_step_count);
    first.first_queue_full_host_mono_us =
        first.first_queue_full_host_mono_us.or(second.first_queue_full_host_mono_us);
    first.latest_queue_full_host_mono_us =
        second.latest_queue_full_host_mono_us.or(first.latest_queue_full_host_mono_us);
    first.max_queue_depth = first.max_queue_depth.max(second.max_queue_depth);
    first.final_queue_depth = second.final_queue_depth.max(first.final_queue_depth);
    first.encoded_step_count = first.encoded_step_count.max(second.encoded_step_count);
    first.last_step_index = second.last_step_index.or(first.last_step_index);
    first.backpressure_threshold_tripped |= second.backpressure_threshold_tripped;
    first.flush_failed |= second.flush_failed;
    first.flush_error = second.flush_error.or(first.flush_error);
    first
}

fn rad_array(values: JointArray<Rad>) -> [f64; 6] {
    values.into_array().map(|value| value.0)
}

fn rad_per_second_array(values: JointArray<RadPerSecond>) -> [f64; 6] {
    values.into_array().map(|value| value.0)
}

fn torque_array(values: JointArray<NewtonMeter>) -> [f64; 6] {
    values.into_array().map(|value| value.0)
}

fn subtract6(lhs: [f64; 6], rhs: [f64; 6]) -> [f64; 6] {
    std::array::from_fn(|index| lhs[index] - rhs[index])
}

fn flatten3x3(values: [[f64; 3]; 3]) -> [f64; 9] {
    [
        values[0][0],
        values[0][1],
        values[0][2],
        values[1][0],
        values[1][1],
        values[1][2],
        values[2][0],
        values[2][1],
        values[2][2],
    ]
}

fn flatten3x6(values: [[f64; 6]; 3]) -> [f64; 18] {
    [
        values[0][0],
        values[0][1],
        values[0][2],
        values[0][3],
        values[0][4],
        values[0][5],
        values[1][0],
        values[1][1],
        values[1][2],
        values[1][3],
        values[1][4],
        values[1][5],
        values[2][0],
        values[2][1],
        values[2][2],
        values[2][3],
        values[2][4],
        values[2][5],
    ]
}
