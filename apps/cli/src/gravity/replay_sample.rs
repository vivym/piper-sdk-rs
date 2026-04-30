use crate::commands::gravity::GravityReplaySampleArgs;
use crate::connection::{client_builder, wait_for_initial_monitor_snapshot};
use crate::gravity::artifact::{
    LoadedPath, PassDirection, PathHeader, QuasiStaticSampleRow, SamplesHeader, read_path,
    write_jsonl_row,
};
use anyhow::{Context, Result, anyhow, bail};
use piper_client::observer::Observer;
use piper_client::state::{
    Active, CapabilityMarker, DisableConfig, MitMode, MitModeConfig, MitPassthroughMode, Piper,
    SoftRealtime, Standby, StrictCapability, StrictRealtime,
};
use piper_client::types::{JointArray, NewtonMeter, Rad};
use piper_client::{ConnectedPiper, MotionConnectedState};
use piper_control::TargetSpec;
use sha2::{Digest, Sha256};
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::process;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::task::spawn_blocking;

const MAX_SAFE_STEP_RAD: f64 = 0.2;
const MIN_SAFE_STEP_RAD: f64 = 1e-6;
const MAX_INTERPOLATED_WAYPOINTS: usize = 100_000;
const MAX_SAFE_VELOCITY_RAD_S: f64 = 1.0;
const DEFAULT_SIMPLIFICATION_TOLERANCE_RAD: f64 = 0.005;
const MAX_SIMPLIFICATION_TOLERANCE_RAD: f64 = 0.01;
const COMPLETE_JOINT_MASK: u8 = 0x3f;
const SAMPLE_ROW_TYPE: &str = "quasi-static-sample";
const SAMPLE_HEADER_ROW_TYPE: &str = "header";
const SAMPLE_ARTIFACT_KIND: &str = "quasi-static-samples";
const SAMPLE_SCHEMA_VERSION: u32 = 1;
const REPLAY_SAMPLE_PERIOD_MS: u64 = 10;
const REPLAY_SAMPLE_FREQUENCY_HZ: f64 = 100.0;
const DEFAULT_STABLE_VELOCITY_RAD_S: f64 = 0.01;
const DEFAULT_STABLE_TRACKING_ERROR_RAD: f64 = 0.03;
const DEFAULT_STABLE_TORQUE_STD_NM: f64 = 0.08;
const CONSERVATIVE_MIT_SPEED_PERCENT: u8 = 10;
const CONSERVATIVE_KP: f64 = 8.0;
const CONSERVATIVE_KD: f64 = 1.0;
const ACTIVE_COMMAND_TIMEOUT: Duration = Duration::from_millis(200);
const ACTIVE_MAX_FEEDBACK_AGE: Duration = Duration::from_millis(200);
const ACTIVE_TRACKING_ERROR_LIMIT_RAD: f64 = 0.15;
const ACTIVE_TORQUE_LIMIT_NM: f64 = 25.0;
const CANCEL_POLL_INTERVAL: Duration = Duration::from_millis(20);

pub async fn run(args: GravityReplaySampleArgs) -> Result<()> {
    validate_replay_args(&args)?;
    let loaded = read_path(&args.path)?;

    if args.dry_run {
        let report = build_dry_run_plan(&args, &loaded)?;
        print_dry_run_report(&args, &loaded.header, &report);
        return Ok(());
    }

    let plan = build_replay_plan(&args, &loaded)?;
    ensure_output_path_available(&args.out)?;
    print_startup_summary(&args, &plan);
    if !confirm_operator()? {
        bail!("operator confirmation not received; aborted before connecting");
    }

    let running = Arc::new(AtomicBool::new(true));
    let signal_running = Arc::clone(&running);
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            println!();
            println!("Ctrl-C received; disabling gravity replay...");
            signal_running.store(false, Ordering::SeqCst);
        }
    });

    let output = PendingSamplesArtifact::create(&args.out)?;
    let target_spec = resolve_replay_target_spec(&args)?;
    let source_sha256 = sha256_file_hex(&args.path)?;
    let task = spawn_blocking(move || {
        replay_sample_sync(
            args,
            loaded.header,
            plan,
            target_spec,
            source_sha256,
            running,
            output,
        )
    });
    let stats = task
        .await
        .map_err(|error| anyhow!("gravity replay-sample task failed: {error}"))??;

    println!();
    println!(
        "Saved gravity samples: {} (accepted {}, rejected {})",
        stats.output_path.display(),
        stats.accepted_waypoint_count,
        stats.rejected_waypoint_count
    );

    Ok(())
}

#[derive(Debug, Clone)]
pub(crate) struct DryRunReport {
    pub role: String,
    pub target: String,
    pub joint_map: String,
    pub load_profile: String,
    pub raw_sample_count: usize,
    pub simplified_count: usize,
    pub simplification_tolerance_rad: f64,
    pub max_simplification_deviation_rad: f64,
    pub waypoint_count: usize,
    pub joint_min_rad: [f64; 6],
    pub joint_max_rad: [f64; 6],
    pub estimated_duration: Duration,
}

impl DryRunReport {
    fn from_plan(plan: &ReplayPlan) -> Self {
        Self {
            role: plan.role.clone(),
            target: plan.target.clone(),
            joint_map: plan.joint_map.clone(),
            load_profile: plan.load_profile.clone(),
            raw_sample_count: plan.raw_sample_count,
            simplified_count: plan.simplified_count,
            simplification_tolerance_rad: plan.simplification_tolerance_rad,
            max_simplification_deviation_rad: plan.max_simplification_deviation_rad,
            waypoint_count: plan.waypoints.len(),
            joint_min_rad: plan.joint_min_rad,
            joint_max_rad: plan.joint_max_rad,
            estimated_duration: plan.estimated_duration,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ReplayPlan {
    pub role: String,
    pub target: String,
    pub joint_map: String,
    pub load_profile: String,
    pub raw_sample_count: usize,
    pub simplified_count: usize,
    pub simplification_tolerance_rad: f64,
    pub max_simplification_deviation_rad: f64,
    pub waypoints: Vec<ReplayWaypoint>,
    pub joint_min_rad: [f64; 6],
    pub joint_max_rad: [f64; 6],
    pub estimated_duration: Duration,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ReplayWaypoint {
    pub waypoint_id: u64,
    pub q_rad: [f64; 6],
    pub pass_direction: PassDirection,
}

#[derive(Debug, Clone)]
struct ReplaySampleStats {
    output_path: PathBuf,
    accepted_waypoint_count: usize,
    rejected_waypoint_count: usize,
}

struct PendingSamplesArtifact {
    final_path: PathBuf,
    temp_path: PathBuf,
    file: Option<File>,
    cleanup_temp: bool,
    #[cfg(test)]
    force_persist_error: bool,
}

impl PendingSamplesArtifact {
    fn create(final_path: &Path) -> Result<Self> {
        ensure_output_path_available(final_path)?;

        let parent = artifact_parent(final_path);
        let file_name = final_path
            .file_name()
            .ok_or_else(|| anyhow!("--out must include a file name"))?;
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();

        for attempt in 0..100 {
            let mut temp_name = OsString::from(".");
            temp_name.push(file_name);
            temp_name.push(format!(".tmp.{}.{}.{}", process::id(), nonce, attempt));
            let temp_path = parent.join(temp_name);

            match OpenOptions::new().write(true).create_new(true).open(&temp_path) {
                Ok(file) => {
                    return Ok(Self {
                        final_path: final_path.to_path_buf(),
                        temp_path,
                        file: Some(file),
                        cleanup_temp: true,
                        #[cfg(test)]
                        force_persist_error: false,
                    });
                },
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    return Err(error).with_context(|| {
                        format!(
                            "failed to create temporary samples artifact in {}",
                            parent.display()
                        )
                    });
                },
            }
        }

        bail!(
            "failed to allocate a unique temporary samples artifact for {}",
            final_path.display()
        );
    }

    fn temp_path(&self) -> &Path {
        &self.temp_path
    }

    fn take_file(&mut self) -> Result<File> {
        self.file.take().ok_or_else(|| {
            anyhow!(
                "temporary samples artifact file is already closed: {}",
                self.temp_path.display()
            )
        })
    }

    fn preserve_temp(&mut self) {
        self.cleanup_temp = false;
    }

    fn persist(&mut self) -> Result<()> {
        #[cfg(test)]
        if self.force_persist_error {
            bail!(
                "forced samples artifact persist failure; temporary samples artifact retained at {}",
                self.temp_path.display()
            );
        }

        if self.final_path.exists() {
            bail!(
                "{} already exists; temporary samples artifact retained at {}",
                self.final_path.display(),
                self.temp_path.display()
            );
        }

        fs::hard_link(&self.temp_path, &self.final_path).with_context(|| {
            format!(
                "failed to persist temporary samples artifact {} to {}; temporary file retained",
                self.temp_path.display(),
                self.final_path.display()
            )
        })?;
        sync_parent_directory(&self.final_path)?;
        let _ = fs::remove_file(&self.temp_path);
        self.cleanup_temp = false;
        Ok(())
    }

    #[cfg(test)]
    fn force_persist_error_for_test(&mut self) {
        self.force_persist_error = true;
    }
}

impl Drop for PendingSamplesArtifact {
    fn drop(&mut self) {
        if self.cleanup_temp {
            let _ = fs::remove_file(&self.temp_path);
        }
    }
}

fn artifact_parent(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

#[cfg(unix)]
fn sync_parent_directory(path: &Path) -> Result<()> {
    let parent = artifact_parent(path);
    let dir = File::open(parent)
        .with_context(|| format!("failed to open output directory {}", parent.display()))?;
    dir.sync_all()
        .with_context(|| format!("failed to sync output directory {}", parent.display()))
}

#[cfg(not(unix))]
fn sync_parent_directory(_path: &Path) -> Result<()> {
    Ok(())
}

#[allow(dead_code)]
pub(crate) fn simplify_path(samples: &[[f64; 6]], max_deviation_rad: f64) -> Result<Vec<[f64; 6]>> {
    let keep_indices = simplify_path_indices(samples, max_deviation_rad)?;
    Ok(keep_indices.into_iter().map(|index| samples[index]).collect())
}

fn simplify_path_indices(samples: &[[f64; 6]], max_deviation_rad: f64) -> Result<Vec<usize>> {
    validate_path(samples)?;
    validate_positive_finite("max_deviation_rad", max_deviation_rad)?;

    if samples.len() <= 2 {
        return Ok((0..samples.len()).collect());
    }

    let mut keep = vec![false; samples.len()];
    keep[0] = true;
    keep[samples.len() - 1] = true;
    simplify_segment(samples, 0, samples.len() - 1, max_deviation_rad, &mut keep);

    Ok(keep
        .into_iter()
        .enumerate()
        .filter_map(|(index, keep)| keep.then_some(index))
        .collect())
}

pub(crate) fn interpolate_waypoints(
    start: [f64; 6],
    end: [f64; 6],
    max_step_rad: f64,
) -> Result<Vec<[f64; 6]>> {
    validate_joint_vector("start", &start)?;
    validate_joint_vector("end", &end)?;
    validate_max_step_rad(max_step_rad)?;

    let max_delta = max_abs_delta(&start, &end);
    if max_delta == 0.0 {
        return Ok(vec![start]);
    }

    let segment_count_f64 = (max_delta / max_step_rad).ceil();
    if !segment_count_f64.is_finite() {
        bail!("waypoint count must be finite");
    }
    let waypoint_count_f64 = segment_count_f64 + 1.0;
    if !waypoint_count_f64.is_finite() || waypoint_count_f64 > MAX_INTERPOLATED_WAYPOINTS as f64 {
        bail!("waypoint count must be <= {MAX_INTERPOLATED_WAYPOINTS}");
    }

    let segment_count = segment_count_f64 as usize;
    let waypoint_count = segment_count
        .checked_add(1)
        .ok_or_else(|| anyhow!("waypoint count overflows"))?;
    if waypoint_count > MAX_INTERPOLATED_WAYPOINTS {
        bail!("waypoint count must be <= {MAX_INTERPOLATED_WAYPOINTS}");
    }

    let mut waypoints = Vec::with_capacity(waypoint_count);
    for index in 0..=segment_count {
        let ratio = index as f64 / segment_count as f64;
        let mut waypoint = [0.0; 6];
        for joint in 0..6 {
            waypoint[joint] = start[joint] + (end[joint] - start[joint]) * ratio;
        }
        waypoints.push(waypoint);
    }
    Ok(waypoints)
}

pub(crate) fn estimate_duration(
    waypoint_count: usize,
    max_velocity_rad_s: f64,
    settle_ms: u64,
    sample_ms: u64,
) -> Result<Duration> {
    if waypoint_count == 0 {
        bail!("waypoint_count must be positive");
    }
    validate_positive_finite("max_velocity_rad_s", max_velocity_rad_s)?;
    if max_velocity_rad_s > MAX_SAFE_VELOCITY_RAD_S {
        bail!("max_velocity_rad_s must be <= {MAX_SAFE_VELOCITY_RAD_S}");
    }
    if settle_ms == 0 {
        bail!("settle_ms must be positive");
    }
    if sample_ms == 0 {
        bail!("sample_ms must be positive");
    }

    let per_waypoint_ms = settle_ms
        .checked_add(sample_ms)
        .ok_or_else(|| anyhow::anyhow!("settle_ms + sample_ms overflows"))?;
    let total_ms = per_waypoint_ms
        .checked_mul(waypoint_count as u64)
        .ok_or_else(|| anyhow::anyhow!("estimated duration overflows"))?;
    Ok(Duration::from_millis(total_ms))
}

pub(crate) fn validate_replay_args(args: &GravityReplaySampleArgs) -> Result<()> {
    if args.role.trim().is_empty() {
        bail!("--role must not be empty");
    }
    if args.path.as_os_str().is_empty() {
        bail!("--path must not be empty");
    }
    if args.out.as_os_str().is_empty() {
        bail!("--out must not be empty");
    }
    validate_positive_finite("max_velocity_rad_s", args.max_velocity_rad_s)?;
    if args.max_velocity_rad_s > MAX_SAFE_VELOCITY_RAD_S {
        bail!("max_velocity_rad_s must be <= {MAX_SAFE_VELOCITY_RAD_S}");
    }
    validate_max_step_rad(args.max_step_rad)?;
    if args.settle_ms == 0 {
        bail!("settle_ms must be positive");
    }
    if args.sample_ms == 0 {
        bail!("sample_ms must be positive");
    }
    resolve_replay_target(args)?;
    Ok(())
}

pub(crate) fn build_dry_run_plan(
    args: &GravityReplaySampleArgs,
    loaded: &LoadedPath,
) -> Result<DryRunReport> {
    Ok(DryRunReport::from_plan(&build_replay_plan(args, loaded)?))
}

pub(crate) fn build_replay_plan(
    args: &GravityReplaySampleArgs,
    loaded: &LoadedPath,
) -> Result<ReplayPlan> {
    validate_replay_args(args)?;
    let target = resolve_replay_target(args)?;
    validate_path_header_matches_args(args, &target, &loaded.header)?;

    if loaded.rows.is_empty() {
        bail!("path must contain at least one sample");
    }
    validate_replay_path_rows(loaded)?;

    let raw_samples = loaded.rows.iter().map(|row| row.q_rad).collect::<Vec<_>>();
    let simplification_tolerance_rad = replay_simplification_tolerance_rad()?;
    let simplified_indices = simplify_path_indices(&raw_samples, simplification_tolerance_rad)?;
    let max_simplification_deviation_rad =
        max_simplification_deviation(&raw_samples, &simplified_indices)?;
    let simplified = simplified_indices.iter().map(|&index| raw_samples[index]).collect::<Vec<_>>();
    let forward_waypoints = waypoint_plan_from_simplified_path(&simplified, args.max_step_rad)?;
    let waypoints = replay_waypoints_from_forward_plan(forward_waypoints, args.bidirectional)?;
    let waypoint_positions = waypoints.iter().map(|waypoint| waypoint.q_rad).collect::<Vec<_>>();
    let (joint_min_rad, joint_max_rad) = joint_ranges(&waypoint_positions)?;
    let estimated_duration = estimate_duration(
        waypoints.len(),
        args.max_velocity_rad_s,
        args.settle_ms,
        args.sample_ms,
    )?;

    Ok(ReplayPlan {
        role: loaded.header.role.clone(),
        target,
        joint_map: loaded.header.joint_map.clone(),
        load_profile: loaded.header.load_profile.clone(),
        raw_sample_count: loaded.rows.len(),
        simplified_count: simplified.len(),
        simplification_tolerance_rad,
        max_simplification_deviation_rad,
        waypoints,
        joint_min_rad,
        joint_max_rad,
        estimated_duration,
    })
}

fn validate_replay_path_rows(loaded: &LoadedPath) -> Result<()> {
    for row in &loaded.rows {
        if row.position_valid_mask != COMPLETE_JOINT_MASK {
            bail!(
                "path row {} position_valid_mask must be {COMPLETE_JOINT_MASK:#04x}, got {:#04x}",
                row.sample_index,
                row.position_valid_mask
            );
        }
    }
    Ok(())
}

fn validate_path_header_matches_args(
    args: &GravityReplaySampleArgs,
    target: &str,
    header: &PathHeader,
) -> Result<()> {
    if header.role != args.role {
        bail!(
            "--role {:?} does not match path header role {:?}",
            args.role,
            header.role
        );
    }
    if header.target != target {
        bail!(
            "resolved target {:?} does not match path header target {:?}",
            target,
            header.target
        );
    }
    Ok(())
}

fn replay_sample_sync(
    args: GravityReplaySampleArgs,
    path_header: PathHeader,
    plan: ReplayPlan,
    target_spec: TargetSpec,
    source_sha256: String,
    running: Arc<AtomicBool>,
    output: PendingSamplesArtifact,
) -> Result<ReplaySampleStats> {
    ensure_replay_running(&running, "connect")?;
    let target = target_spec.clone().into_connection_target();
    let connected = client_builder(&target)
        .build()
        .context("failed to connect to robot for active gravity replay sampling")?;

    let criteria = stable_sample_criteria_from_args(&args)?;
    let settle_duration = Duration::from_millis(args.settle_ms);
    let rows = match connected {
        ConnectedPiper::Strict(MotionConnectedState::Standby(standby)) => {
            run_strict_replay(standby, &plan, criteria, &args, settle_duration, running)?
        },
        ConnectedPiper::Soft(MotionConnectedState::Standby(standby)) => {
            run_soft_replay(standby, &plan, criteria, &args, settle_duration, running)?
        },
        ConnectedPiper::Monitor(_) => bail!("monitor-only connections cannot run active replay"),
        ConnectedPiper::Strict(MotionConnectedState::Maintenance(_))
        | ConnectedPiper::Soft(MotionConnectedState::Maintenance(_)) => {
            bail!("robot is not in confirmed Standby; run stop before active replay sampling")
        },
    };

    let accepted_waypoint_count = rows.len();
    let rejected_waypoint_count = plan.waypoints.len().saturating_sub(accepted_waypoint_count);
    let header = build_samples_header(
        &args,
        &path_header,
        &plan,
        source_sha256,
        accepted_waypoint_count,
        rejected_waypoint_count,
        criteria,
    );
    write_samples_artifact(output, &header, &rows)?;

    Ok(ReplaySampleStats {
        output_path: args.out,
        accepted_waypoint_count,
        rejected_waypoint_count,
    })
}

fn run_strict_replay(
    standby: Piper<Standby, StrictRealtime>,
    plan: &ReplayPlan,
    criteria: StableSampleCriteria,
    args: &GravityReplaySampleArgs,
    settle_duration: Duration,
    running: Arc<AtomicBool>,
) -> Result<Vec<QuasiStaticSampleRow>> {
    wait_for_initial_monitor_snapshot(|| standby.observer().joint_positions())
        .context("timed out waiting for initial joint position feedback")?;
    let active = enable_replay_arm(
        &running,
        || {
            standby
                .enable_mit_mode(conservative_mit_config())
                .context("failed to enable strict MIT mode for active replay sampling")
        },
        |active| active.disable(DisableConfig::default()).map(|_| ()).map_err(Into::into),
    )?;
    let mut stepper = HardwareReplayStepper::new(active, args, Arc::clone(&running));
    let rows =
        replay_quasi_static_samples(&mut stepper, &plan.waypoints, criteria, settle_duration)?;
    stepper.disable().context("failed to disable robot after replay sampling")?;
    Ok(rows)
}

fn run_soft_replay(
    standby: Piper<Standby, SoftRealtime>,
    plan: &ReplayPlan,
    criteria: StableSampleCriteria,
    args: &GravityReplaySampleArgs,
    settle_duration: Duration,
    running: Arc<AtomicBool>,
) -> Result<Vec<QuasiStaticSampleRow>> {
    wait_for_initial_monitor_snapshot(|| standby.observer().joint_positions())
        .context("timed out waiting for initial joint position feedback")?;
    let active = enable_replay_arm(
        &running,
        || {
            standby
                .enable_mit_passthrough(conservative_mit_config())
                .context("failed to enable soft MIT passthrough mode for active replay sampling")
        },
        |active| active.disable(DisableConfig::default()).map(|_| ()).map_err(Into::into),
    )?;
    let mut stepper = HardwareReplayStepper::new(active, args, Arc::clone(&running));
    let rows =
        replay_quasi_static_samples(&mut stepper, &plan.waypoints, criteria, settle_duration)?;
    stepper.disable().context("failed to disable robot after replay sampling")?;
    Ok(rows)
}

fn ensure_replay_running(running: &Arc<AtomicBool>, phase: &str) -> Result<()> {
    if !running.load(Ordering::SeqCst) {
        bail!("gravity replay sampling cancelled before {phase}");
    }
    Ok(())
}

fn enable_replay_arm<ActiveArm, Enable, Disable>(
    running: &Arc<AtomicBool>,
    enable: Enable,
    disable: Disable,
) -> Result<ActiveArm>
where
    Enable: FnOnce() -> Result<ActiveArm>,
    Disable: FnOnce(ActiveArm) -> Result<()>,
{
    ensure_replay_running(running, "enable")?;
    let active = enable()?;
    if !running.load(Ordering::SeqCst) {
        disable(active).context("failed to disable robot after cancellation raced with enable")?;
        bail!("gravity replay sampling cancelled after enable");
    }
    Ok(active)
}

fn conservative_mit_config() -> MitModeConfig {
    MitModeConfig {
        speed_percent: CONSERVATIVE_MIT_SPEED_PERCENT,
        ..Default::default()
    }
}

fn stable_sample_criteria_from_args(
    args: &GravityReplaySampleArgs,
) -> Result<StableSampleCriteria> {
    Ok(StableSampleCriteria {
        stable_velocity_rad_s: DEFAULT_STABLE_VELOCITY_RAD_S,
        stable_tracking_error_rad: DEFAULT_STABLE_TRACKING_ERROR_RAD,
        stable_torque_std_nm: DEFAULT_STABLE_TORQUE_STD_NM,
        sample_count: sample_count_from_sample_ms(args.sample_ms)?,
    })
}

fn sample_count_from_sample_ms(sample_ms: u64) -> Result<usize> {
    if sample_ms == 0 {
        bail!("sample_ms must be positive");
    }
    let count = sample_ms.div_ceil(REPLAY_SAMPLE_PERIOD_MS).max(1);
    usize::try_from(count).map_err(|_| anyhow!("sample_count overflows usize"))
}

fn build_samples_header(
    args: &GravityReplaySampleArgs,
    path_header: &PathHeader,
    plan: &ReplayPlan,
    source_sha256: String,
    accepted_waypoint_count: usize,
    rejected_waypoint_count: usize,
    criteria: StableSampleCriteria,
) -> SamplesHeader {
    SamplesHeader {
        row_type: SAMPLE_HEADER_ROW_TYPE.to_string(),
        artifact_kind: SAMPLE_ARTIFACT_KIND.to_string(),
        schema_version: SAMPLE_SCHEMA_VERSION,
        source_path: args.path.display().to_string(),
        source_sha256,
        role: plan.role.clone(),
        target: plan.target.clone(),
        joint_map: plan.joint_map.clone(),
        load_profile: plan.load_profile.clone(),
        torque_convention: path_header.torque_convention.clone(),
        frequency_hz: REPLAY_SAMPLE_FREQUENCY_HZ,
        max_velocity_rad_s: args.max_velocity_rad_s,
        max_step_rad: args.max_step_rad,
        settle_ms: args.settle_ms,
        sample_ms: args.sample_ms,
        stable_velocity_rad_s: criteria.stable_velocity_rad_s,
        stable_tracking_error_rad: criteria.stable_tracking_error_rad,
        stable_torque_std_nm: criteria.stable_torque_std_nm,
        waypoint_count: plan.waypoints.len(),
        accepted_waypoint_count,
        rejected_waypoint_count,
    }
}

fn write_samples_artifact(
    mut pending: PendingSamplesArtifact,
    header: &SamplesHeader,
    rows: &[QuasiStaticSampleRow],
) -> Result<()> {
    let temp_path = pending.temp_path().to_path_buf();
    match write_samples_artifact_inner(&mut pending, header, rows) {
        Ok(()) => Ok(()),
        Err(error) => {
            pending.preserve_temp();
            Err(error.context(format!(
                "samples temp artifact retained at {}",
                temp_path.display()
            )))
        },
    }
}

fn write_samples_artifact_inner(
    pending: &mut PendingSamplesArtifact,
    header: &SamplesHeader,
    rows: &[QuasiStaticSampleRow],
) -> Result<()> {
    let file = pending.take_file()?;
    let mut writer = BufWriter::new(file);

    write_jsonl_row(&mut writer, header)?;
    for row in rows {
        write_jsonl_row(&mut writer, row)?;
    }
    writer.flush()?;
    let file = writer
        .into_inner()
        .map_err(|error| anyhow!("failed to finish writing samples artifact: {}", error))?;
    file.sync_all()
        .with_context(|| format!("failed to sync {}", pending.temp_path().display()))?;
    drop(file);
    pending.persist()?;
    Ok(())
}

fn ensure_output_path_available(path: &Path) -> Result<()> {
    if path.as_os_str().is_empty() {
        bail!("--out must not be empty");
    }
    if path.exists() {
        bail!(
            "{} already exists; refusing to overwrite before connecting",
            path.display()
        );
    }
    Ok(())
}

fn confirm_operator() -> Result<bool> {
    print!("Enable robot and run active gravity replay sampling? Type yes to continue: ");
    io::stdout().flush()?;

    let mut input = String::new();
    io::stdin()
        .read_line(&mut input)
        .context("failed to read operator confirmation")?;
    let normalized = input.trim().to_ascii_lowercase();
    Ok(matches!(normalized.as_str(), "yes" | "y"))
}

fn sha256_file_hex(path: &Path) -> Result<String> {
    let mut file =
        File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 8192];

    loop {
        let read = file
            .read(&mut buffer)
            .with_context(|| format!("failed to read {}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }

    Ok(format!("{:x}", hasher.finalize()))
}

fn resolve_replay_target(args: &GravityReplaySampleArgs) -> Result<String> {
    Ok(resolve_replay_target_spec(args)?.to_string())
}

fn resolve_replay_target_spec(args: &GravityReplaySampleArgs) -> Result<TargetSpec> {
    match (args.target.as_deref(), args.interface.as_deref()) {
        (Some(_), Some(_)) => bail!("use either --target or --interface, not both"),
        (Some(target), None) => target
            .parse::<TargetSpec>()
            .map_err(|error| anyhow!("invalid --target {target:?}: {error}")),
        (None, Some(interface)) => {
            if interface.trim().is_empty() {
                bail!("--interface must not be empty");
            }
            Ok(TargetSpec::SocketCan {
                iface: interface.to_string(),
            })
        },
        (None, None) => bail!("pass --target or --interface for gravity replay"),
    }
}

fn waypoint_plan_from_simplified_path(
    simplified_path: &[[f64; 6]],
    max_step_rad: f64,
) -> Result<Vec<[f64; 6]>> {
    validate_path(simplified_path)?;
    if simplified_path.len() == 1 {
        return Ok(vec![simplified_path[0]]);
    }

    let mut forward = Vec::new();
    for pair in simplified_path.windows(2) {
        let segment = interpolate_waypoints(pair[0], pair[1], max_step_rad)?;
        append_segment_waypoints(&mut forward, segment);
    }

    if forward.is_empty() {
        bail!("waypoint plan must contain at least one waypoint");
    }

    Ok(forward)
}

fn append_segment_waypoints(plan: &mut Vec<[f64; 6]>, segment: Vec<[f64; 6]>) {
    let skip_count = usize::from(!plan.is_empty());
    plan.extend(segment.into_iter().skip(skip_count));
}

fn replay_waypoints_from_forward_plan(
    forward: Vec<[f64; 6]>,
    bidirectional: bool,
) -> Result<Vec<ReplayWaypoint>> {
    if forward.is_empty() {
        bail!("waypoint plan must contain at least one waypoint");
    }

    let mut plan = Vec::with_capacity(if bidirectional {
        forward.len().saturating_mul(2).saturating_sub(1)
    } else {
        forward.len()
    });

    for (index, q_rad) in forward.iter().copied().enumerate() {
        plan.push(ReplayWaypoint {
            waypoint_id: u64::try_from(index).map_err(|_| anyhow!("waypoint_id overflows u64"))?,
            q_rad,
            pass_direction: PassDirection::Forward,
        });
    }

    if bidirectional {
        for (index, q_rad) in forward.iter().copied().enumerate().rev().skip(1) {
            plan.push(ReplayWaypoint {
                waypoint_id: u64::try_from(index)
                    .map_err(|_| anyhow!("waypoint_id overflows u64"))?,
                q_rad,
                pass_direction: PassDirection::Backward,
            });
        }
    }

    Ok(plan)
}

fn joint_ranges(waypoints: &[[f64; 6]]) -> Result<([f64; 6], [f64; 6])> {
    validate_path(waypoints)?;
    let mut min_values = [f64::INFINITY; 6];
    let mut max_values = [f64::NEG_INFINITY; 6];

    for waypoint in waypoints {
        for joint in 0..6 {
            min_values[joint] = min_values[joint].min(waypoint[joint]);
            max_values[joint] = max_values[joint].max(waypoint[joint]);
        }
    }

    Ok((min_values, max_values))
}

fn replay_simplification_tolerance_rad() -> Result<f64> {
    let tolerance = DEFAULT_SIMPLIFICATION_TOLERANCE_RAD.min(MAX_SIMPLIFICATION_TOLERANCE_RAD);
    validate_positive_finite("simplification_tolerance_rad", tolerance)?;
    Ok(tolerance)
}

fn max_simplification_deviation(samples: &[[f64; 6]], keep_indices: &[usize]) -> Result<f64> {
    validate_path(samples)?;
    if keep_indices.is_empty() {
        bail!("simplified path must contain at least one sample");
    }
    if keep_indices[0] != 0 {
        bail!("simplified path must keep the first sample");
    }
    if *keep_indices.last().expect("checked non-empty") != samples.len() - 1 {
        bail!("simplified path must keep the last sample");
    }

    let mut previous = 0;
    let mut max_deviation: f64 = 0.0;
    for &index in keep_indices.iter().skip(1) {
        if index <= previous || index >= samples.len() {
            bail!("simplified path indices must be strictly increasing");
        }
        for sample in &samples[previous..=index] {
            max_deviation = max_deviation.max(point_segment_deviation(
                sample,
                &samples[previous],
                &samples[index],
            ));
        }
        previous = index;
    }

    Ok(max_deviation)
}

fn print_startup_summary(args: &GravityReplaySampleArgs, plan: &ReplayPlan) {
    println!("gravity replay-sample startup summary");
    println!("  target: {}", plan.target);
    println!("  role: {}", plan.role);
    println!("  load_profile: {}", plan.load_profile);
    println!("  max_velocity_rad_s: {}", args.max_velocity_rad_s);
    println!("  waypoint_count: {}", plan.waypoints.len());
    println!("  output: {}", args.out.display());
}

fn print_dry_run_report(
    args: &GravityReplaySampleArgs,
    header: &PathHeader,
    report: &DryRunReport,
) {
    println!("gravity replay dry-run");
    println!("  path: {}", args.path.display());
    println!("  output: {}", args.out.display());
    println!("  role: {}", report.role);
    println!("  target: {}", report.target);
    println!("  joint_map: {}", report.joint_map);
    println!("  load_profile: {}", report.load_profile);
    println!("  torque_convention: {}", header.torque_convention);
    println!("  bidirectional: {}", args.bidirectional);
    println!("  raw samples: {}", report.raw_sample_count);
    println!("  simplified keypoints: {}", report.simplified_count);
    println!(
        "  simplification tolerance rad: {:.6}",
        report.simplification_tolerance_rad
    );
    println!(
        "  max simplification deviation rad: {:.6}",
        report.max_simplification_deviation_rad
    );
    println!("  waypoints: {}", report.waypoint_count);
    println!(
        "  joint min rad: {}",
        format_joint_array(report.joint_min_rad)
    );
    println!(
        "  joint max rad: {}",
        format_joint_array(report.joint_max_rad)
    );
    println!(
        "  estimated duration: {:.3}s",
        report.estimated_duration.as_secs_f64()
    );
}

fn format_joint_array(values: [f64; 6]) -> String {
    format!(
        "[{:.6}, {:.6}, {:.6}, {:.6}, {:.6}, {:.6}]",
        values[0], values[1], values[2], values[3], values[4], values[5]
    )
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ReplaySnapshot {
    pub host_mono_us: u64,
    pub raw_timestamp_us: Option<u64>,
    pub q_rad: [f64; 6],
    pub dq_rad_s: [f64; 6],
    pub tau_nm: [f64; 6],
    pub position_valid_mask: u8,
    pub dynamic_valid_mask: u8,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy)]
pub(crate) struct StableSampleCriteria {
    pub stable_velocity_rad_s: f64,
    pub stable_tracking_error_rad: f64,
    pub stable_torque_std_nm: f64,
    pub sample_count: usize,
}

#[allow(dead_code)]
pub(crate) trait ReplayStepper {
    fn move_toward(&mut self, target: [f64; 6]) -> Result<()>;
    fn hold(&mut self, duration: Duration) -> Result<()> {
        if !duration.is_zero() {
            std::thread::sleep(duration);
        }
        Ok(())
    }
    fn wait_between_samples(&mut self) -> Result<()> {
        Ok(())
    }
    fn snapshot(&mut self) -> Result<ReplaySnapshot>;
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct StabilityMetrics {
    pub stable_velocity_rad_s: f64,
    pub stable_tracking_error_rad: f64,
    pub stable_torque_std_nm: f64,
}

struct StableSampleWindow {
    latest: ReplaySnapshot,
    metrics: StabilityMetrics,
}

trait ActiveMitArm {
    fn command_torques_confirmed(
        &self,
        positions: &JointArray<Rad>,
        velocities: &JointArray<f64>,
        kp: &JointArray<f64>,
        kd: &JointArray<f64>,
        torques: &JointArray<NewtonMeter>,
        timeout: Duration,
    ) -> Result<()>;
    fn snapshot(&self) -> Result<ReplaySnapshot>;
    fn disable_arm(self, config: DisableConfig) -> Result<()>;
}

impl<Capability> ActiveMitArm for Piper<Active<MitMode>, Capability>
where
    Capability: StrictCapability,
{
    fn command_torques_confirmed(
        &self,
        positions: &JointArray<Rad>,
        velocities: &JointArray<f64>,
        kp: &JointArray<f64>,
        kd: &JointArray<f64>,
        torques: &JointArray<NewtonMeter>,
        timeout: Duration,
    ) -> Result<()> {
        Piper::<Active<MitMode>, Capability>::command_torques_confirmed(
            self, positions, velocities, kp, kd, torques, timeout,
        )
        .map_err(Into::into)
    }

    fn snapshot(&self) -> Result<ReplaySnapshot> {
        replay_snapshot_from_observer(self.observer())
    }

    fn disable_arm(self, config: DisableConfig) -> Result<()> {
        self.disable(config).map(|_| ()).map_err(Into::into)
    }
}

impl ActiveMitArm for Piper<Active<MitPassthroughMode>, SoftRealtime> {
    fn command_torques_confirmed(
        &self,
        positions: &JointArray<Rad>,
        velocities: &JointArray<f64>,
        kp: &JointArray<f64>,
        kd: &JointArray<f64>,
        torques: &JointArray<NewtonMeter>,
        timeout: Duration,
    ) -> Result<()> {
        Piper::<Active<MitPassthroughMode>, SoftRealtime>::command_torques_confirmed(
            self, positions, velocities, kp, kd, torques, timeout,
        )
        .map_err(Into::into)
    }

    fn snapshot(&self) -> Result<ReplaySnapshot> {
        replay_snapshot_from_observer(self.observer())
    }

    fn disable_arm(self, config: DisableConfig) -> Result<()> {
        self.disable(config).map(|_| ()).map_err(Into::into)
    }
}

struct HardwareReplayStepper<A>
where
    A: ActiveMitArm,
{
    active: Option<A>,
    running: Arc<AtomicBool>,
    max_step_rad: f64,
    max_velocity_rad_s: f64,
    command_timeout: Duration,
    sample_period: Duration,
    velocities: JointArray<f64>,
    kp: JointArray<f64>,
    kd: JointArray<f64>,
    torques: JointArray<NewtonMeter>,
    last_target: Option<[f64; 6]>,
}

impl<A> HardwareReplayStepper<A>
where
    A: ActiveMitArm,
{
    fn new(active: A, args: &GravityReplaySampleArgs, running: Arc<AtomicBool>) -> Self {
        Self {
            active: Some(active),
            running,
            max_step_rad: args.max_step_rad,
            max_velocity_rad_s: args.max_velocity_rad_s,
            command_timeout: ACTIVE_COMMAND_TIMEOUT,
            sample_period: Duration::from_millis(REPLAY_SAMPLE_PERIOD_MS),
            velocities: JointArray::splat(0.0),
            kp: JointArray::splat(CONSERVATIVE_KP),
            kd: JointArray::splat(CONSERVATIVE_KD),
            torques: JointArray::splat(NewtonMeter::ZERO),
            last_target: None,
        }
    }

    fn disable(mut self) -> Result<()> {
        if let Some(active) = self.active.take() {
            active.disable_arm(DisableConfig::default())?;
        }
        Ok(())
    }

    fn active(&self) -> Result<&A> {
        self.active
            .as_ref()
            .ok_or_else(|| anyhow!("active replay stepper is already disabled"))
    }

    fn ensure_running(&self) -> Result<()> {
        if !self.running.load(Ordering::SeqCst) {
            bail!("gravity replay sampling cancelled");
        }
        Ok(())
    }

    fn command_target(&self, target: [f64; 6]) -> Result<()> {
        self.ensure_running()?;
        validate_joint_vector("target", &target)?;
        let positions = JointArray::new(target.map(Rad));
        self.active()?
            .command_torques_confirmed(
                &positions,
                &self.velocities,
                &self.kp,
                &self.kd,
                &self.torques,
                self.command_timeout,
            )
            .context("failed to submit zero-feedforward MIT hold command")
    }

    fn require_complete_snapshot(&self) -> Result<ReplaySnapshot> {
        let snapshot = self.active()?.snapshot()?;
        ensure_snapshot_complete(&snapshot)?;
        Ok(snapshot)
    }

    fn check_active_safety(&self, snapshot: &ReplaySnapshot, target: &[f64; 6]) -> Result<()> {
        if (snapshot.position_valid_mask & COMPLETE_JOINT_MASK) == COMPLETE_JOINT_MASK {
            let tracking_error = max_abs_delta(&snapshot.q_rad, target);
            if tracking_error > ACTIVE_TRACKING_ERROR_LIMIT_RAD {
                bail!(
                    "active replay tracking error {tracking_error:.6} rad exceeds {:.6}",
                    ACTIVE_TRACKING_ERROR_LIMIT_RAD
                );
            }
        }

        if (snapshot.dynamic_valid_mask & COMPLETE_JOINT_MASK) == COMPLETE_JOINT_MASK {
            let max_torque =
                snapshot.tau_nm.iter().map(|value| value.abs()).fold(0.0_f64, f64::max);
            if max_torque > ACTIVE_TORQUE_LIMIT_NM {
                bail!(
                    "active replay observed torque {max_torque:.6} Nm exceeds {:.6}",
                    ACTIVE_TORQUE_LIMIT_NM
                );
            }
        }

        Ok(())
    }

    fn sleep_or_cancelled(&self, duration: Duration) -> Result<()> {
        let started_at = Instant::now();
        while started_at.elapsed() < duration {
            self.ensure_running()?;
            let remaining = duration.saturating_sub(started_at.elapsed());
            std::thread::sleep(remaining.min(CANCEL_POLL_INTERVAL));
        }
        self.ensure_running()
    }

    fn rate_limit_duration(&self, delta_rad: f64) -> Result<Duration> {
        if delta_rad <= 0.0 {
            return Ok(Duration::ZERO);
        }
        let seconds = delta_rad / self.max_velocity_rad_s;
        Duration::try_from_secs_f64(seconds)
            .map_err(|_| anyhow!("rate-limit duration is invalid for delta {delta_rad:.6} rad"))
    }

    fn hold_last_target_for(&mut self, duration: Duration) -> Result<()> {
        let target = self
            .last_target
            .ok_or_else(|| anyhow!("cannot hold before a replay target has been commanded"))?;
        let started_at = Instant::now();
        while started_at.elapsed() < duration {
            self.command_target(target)?;
            let remaining = duration.saturating_sub(started_at.elapsed());
            self.sleep_or_cancelled(remaining.min(self.sample_period))?;
            let snapshot = self.active()?.snapshot()?;
            self.check_active_safety(&snapshot, &target)?;
        }
        Ok(())
    }
}

impl<A> Drop for HardwareReplayStepper<A>
where
    A: ActiveMitArm,
{
    fn drop(&mut self) {
        if let Some(active) = self.active.take() {
            let _ = active.disable_arm(DisableConfig::default());
        }
    }
}

impl<A> ReplayStepper for HardwareReplayStepper<A>
where
    A: ActiveMitArm,
{
    fn move_toward(&mut self, target: [f64; 6]) -> Result<()> {
        self.ensure_running()?;
        let current = self.require_complete_snapshot()?.q_rad;
        let increments = interpolate_waypoints(current, target, self.max_step_rad)?;

        if increments.len() <= 1 {
            self.command_target(target)?;
            self.last_target = Some(target);
            let snapshot = self.require_complete_snapshot()?;
            self.check_active_safety(&snapshot, &target)?;
            return Ok(());
        }

        let mut previous = current;
        for increment in increments.into_iter().skip(1) {
            let delta_rad = max_abs_delta(&previous, &increment);
            self.command_target(increment)?;
            self.last_target = Some(increment);
            self.sleep_or_cancelled(self.rate_limit_duration(delta_rad)?)?;
            let snapshot = self.require_complete_snapshot()?;
            self.check_active_safety(&snapshot, &increment)?;
            previous = increment;
        }

        Ok(())
    }

    fn hold(&mut self, duration: Duration) -> Result<()> {
        self.hold_last_target_for(duration)
    }

    fn wait_between_samples(&mut self) -> Result<()> {
        let target = self
            .last_target
            .ok_or_else(|| anyhow!("cannot sample before a replay target has been commanded"))?;
        self.command_target(target)?;
        self.sleep_or_cancelled(self.sample_period)?;
        let snapshot = self.active()?.snapshot()?;
        self.check_active_safety(&snapshot, &target)?;
        Ok(())
    }

    fn snapshot(&mut self) -> Result<ReplaySnapshot> {
        self.ensure_running()?;
        self.active()?.snapshot()
    }
}

fn replay_snapshot_from_observer<Capability>(
    observer: &Observer<Capability>,
) -> Result<ReplaySnapshot>
where
    Capability: CapabilityMarker,
{
    let position = observer.raw_joint_position_state();
    let dynamic = observer.raw_joint_dynamic_state();
    let now_us = piper_sdk::driver::heartbeat::monotonic_micros();
    replay_snapshot_from_states(position, dynamic, now_us, ACTIVE_MAX_FEEDBACK_AGE)
}

fn replay_snapshot_from_states(
    position: piper_sdk::driver::JointPositionState,
    dynamic: piper_sdk::driver::JointDynamicState,
    now_us: u64,
    max_feedback_age: Duration,
) -> Result<ReplaySnapshot> {
    if (position.frame_valid_mask & COMPLETE_JOINT_MASK) != COMPLETE_JOINT_MASK {
        bail!(
            "position feedback incomplete: mask={:#04x}",
            position.frame_valid_mask
        );
    }
    if (dynamic.valid_mask & COMPLETE_JOINT_MASK) != COMPLETE_JOINT_MASK {
        bail!(
            "dynamic feedback incomplete: mask={:#04x}",
            dynamic.valid_mask
        );
    }
    ensure_feedback_stream_fresh(
        "position",
        position.host_rx_mono_us,
        now_us,
        max_feedback_age,
    )?;
    ensure_feedback_stream_fresh(
        "dynamic",
        dynamic.group_host_rx_mono_us,
        now_us,
        max_feedback_age,
    )?;

    let latest_host_mono_us = position.host_rx_mono_us.max(dynamic.group_host_rx_mono_us);
    let latest_raw_timestamp_us = position.hardware_timestamp_us.max(dynamic.group_timestamp_us);

    Ok(ReplaySnapshot {
        host_mono_us: latest_host_mono_us,
        raw_timestamp_us: (latest_raw_timestamp_us != 0).then_some(latest_raw_timestamp_us),
        q_rad: position.joint_pos,
        dq_rad_s: dynamic.joint_vel,
        tau_nm: dynamic.get_all_torques(),
        position_valid_mask: position.frame_valid_mask,
        dynamic_valid_mask: dynamic.valid_mask,
    })
}

fn ensure_feedback_stream_fresh(
    stream_name: &str,
    host_rx_mono_us: u64,
    now_us: u64,
    max_feedback_age: Duration,
) -> Result<()> {
    if host_rx_mono_us == 0 {
        bail!("{stream_name} feedback is not available yet");
    }

    let max_age_us = u64::try_from(max_feedback_age.as_micros()).unwrap_or(u64::MAX);
    let age_us = now_us.saturating_sub(host_rx_mono_us);
    if age_us > max_age_us {
        bail!(
            "stale {stream_name} feedback age {:.3}ms exceeds {:.3}ms",
            age_us as f64 / 1000.0,
            max_feedback_age.as_secs_f64() * 1000.0
        );
    }

    Ok(())
}

fn ensure_snapshot_complete(snapshot: &ReplaySnapshot) -> Result<()> {
    if (snapshot.position_valid_mask & COMPLETE_JOINT_MASK) != COMPLETE_JOINT_MASK {
        bail!(
            "position feedback incomplete: mask={:#04x}",
            snapshot.position_valid_mask
        );
    }
    if (snapshot.dynamic_valid_mask & COMPLETE_JOINT_MASK) != COMPLETE_JOINT_MASK {
        bail!(
            "dynamic feedback incomplete: mask={:#04x}",
            snapshot.dynamic_valid_mask
        );
    }
    Ok(())
}

#[allow(dead_code)]
pub(crate) fn sample_stable_waypoint<S>(
    stepper: &mut S,
    target: [f64; 6],
    criteria: StableSampleCriteria,
) -> Result<ReplaySnapshot>
where
    S: ReplayStepper,
{
    validate_joint_vector("target", &target)?;
    validate_stable_sample_criteria(criteria)?;

    stepper.move_toward(target)?;
    let window = sample_stable_window(stepper, target, criteria)?;
    Ok(window.latest)
}

pub(crate) fn replay_quasi_static_samples<S>(
    stepper: &mut S,
    waypoints: &[ReplayWaypoint],
    criteria: StableSampleCriteria,
    settle_duration: Duration,
) -> Result<Vec<QuasiStaticSampleRow>>
where
    S: ReplayStepper,
{
    if waypoints.is_empty() {
        bail!("waypoint plan must contain at least one waypoint");
    }
    validate_stable_sample_criteria(criteria)?;

    let mut rows = Vec::new();
    for waypoint in waypoints {
        validate_joint_vector("waypoint q_rad", &waypoint.q_rad)?;
        stepper.move_toward(waypoint.q_rad)?;
        stepper.hold(settle_duration)?;

        let snapshots = collect_sample_window(stepper, criteria)?;
        if let Ok(window) = stable_window_from_snapshots(snapshots, waypoint.q_rad, criteria) {
            rows.push(row_from_stable_window(waypoint, window));
        }
    }

    Ok(rows)
}

fn row_from_stable_window(
    waypoint: &ReplayWaypoint,
    window: StableSampleWindow,
) -> QuasiStaticSampleRow {
    QuasiStaticSampleRow {
        row_type: SAMPLE_ROW_TYPE.to_string(),
        waypoint_id: waypoint.waypoint_id,
        segment_id: None,
        pass_direction: waypoint.pass_direction.clone(),
        host_mono_us: window.latest.host_mono_us,
        raw_timestamp_us: window.latest.raw_timestamp_us,
        q_rad: window.latest.q_rad,
        dq_rad_s: window.latest.dq_rad_s,
        tau_nm: window.latest.tau_nm,
        position_valid_mask: window.latest.position_valid_mask,
        dynamic_valid_mask: window.latest.dynamic_valid_mask,
        stable_velocity_rad_s: window.metrics.stable_velocity_rad_s,
        stable_tracking_error_rad: window.metrics.stable_tracking_error_rad,
        stable_torque_std_nm: window.metrics.stable_torque_std_nm,
    }
}

fn sample_stable_window<S>(
    stepper: &mut S,
    target: [f64; 6],
    criteria: StableSampleCriteria,
) -> Result<StableSampleWindow>
where
    S: ReplayStepper,
{
    let snapshots = collect_sample_window(stepper, criteria)?;
    stable_window_from_snapshots(snapshots, target, criteria)
}

fn collect_sample_window<S>(
    stepper: &mut S,
    criteria: StableSampleCriteria,
) -> Result<Vec<ReplaySnapshot>>
where
    S: ReplayStepper,
{
    let mut snapshots = Vec::with_capacity(criteria.sample_count);
    for index in 0..criteria.sample_count {
        if index > 0 {
            stepper.wait_between_samples()?;
        }
        let snapshot = stepper.snapshot()?;
        validate_snapshot(&snapshot)?;
        snapshots.push(snapshot);
    }
    Ok(snapshots)
}

fn stable_window_from_snapshots(
    snapshots: Vec<ReplaySnapshot>,
    target: [f64; 6],
    criteria: StableSampleCriteria,
) -> Result<StableSampleWindow> {
    let metrics = evaluate_stable_snapshots(&snapshots, &target, criteria)?;
    Ok(StableSampleWindow {
        latest: *snapshots.last().expect("sample_count is positive"),
        metrics,
    })
}

fn validate_stable_sample_criteria(criteria: StableSampleCriteria) -> Result<()> {
    validate_positive_finite("stable_velocity_rad_s", criteria.stable_velocity_rad_s)?;
    validate_positive_finite(
        "stable_tracking_error_rad",
        criteria.stable_tracking_error_rad,
    )?;
    validate_positive_finite("stable_torque_std_nm", criteria.stable_torque_std_nm)?;
    if criteria.sample_count == 0 {
        bail!("sample_count must be positive");
    }
    Ok(())
}

fn validate_snapshot(snapshot: &ReplaySnapshot) -> Result<()> {
    validate_joint_vector("snapshot q_rad", &snapshot.q_rad)?;
    validate_joint_vector("snapshot dq_rad_s", &snapshot.dq_rad_s)?;
    validate_joint_vector("snapshot tau_nm", &snapshot.tau_nm)?;
    Ok(())
}

fn evaluate_stable_snapshots(
    snapshots: &[ReplaySnapshot],
    target: &[f64; 6],
    criteria: StableSampleCriteria,
) -> Result<StabilityMetrics> {
    let mut observed_max_velocity: f64 = 0.0;
    let mut observed_max_tracking_error: f64 = 0.0;

    for snapshot in snapshots {
        if (snapshot.position_valid_mask & COMPLETE_JOINT_MASK) != COMPLETE_JOINT_MASK {
            bail!(
                "position_valid_mask must include all joints, got {:#04x}",
                snapshot.position_valid_mask
            );
        }
        if (snapshot.dynamic_valid_mask & COMPLETE_JOINT_MASK) != COMPLETE_JOINT_MASK {
            bail!(
                "dynamic_valid_mask must include all joints, got {:#04x}",
                snapshot.dynamic_valid_mask
            );
        }

        let max_velocity =
            snapshot.dq_rad_s.iter().map(|value| value.abs()).fold(0.0_f64, f64::max);
        observed_max_velocity = observed_max_velocity.max(max_velocity);
        if max_velocity > criteria.stable_velocity_rad_s {
            bail!(
                "unstable velocity {max_velocity:.6} rad/s exceeds {:.6}",
                criteria.stable_velocity_rad_s
            );
        }

        let tracking_error = max_abs_delta(&snapshot.q_rad, target);
        observed_max_tracking_error = observed_max_tracking_error.max(tracking_error);
        if tracking_error > criteria.stable_tracking_error_rad {
            bail!(
                "tracking error {tracking_error:.6} rad exceeds {:.6}",
                criteria.stable_tracking_error_rad
            );
        }
    }

    let max_torque_std = max_torque_std_nm(snapshots);
    if max_torque_std > criteria.stable_torque_std_nm {
        bail!(
            "torque std {max_torque_std:.6} Nm exceeds {:.6}",
            criteria.stable_torque_std_nm
        );
    }

    Ok(StabilityMetrics {
        stable_velocity_rad_s: observed_max_velocity,
        stable_tracking_error_rad: observed_max_tracking_error,
        stable_torque_std_nm: max_torque_std,
    })
}

fn max_torque_std_nm(snapshots: &[ReplaySnapshot]) -> f64 {
    let sample_count = snapshots.len() as f64;
    let mut max_std: f64 = 0.0;

    for joint in 0..6 {
        let mean =
            snapshots.iter().map(|snapshot| snapshot.tau_nm[joint]).sum::<f64>() / sample_count;
        let variance = snapshots
            .iter()
            .map(|snapshot| {
                let delta = snapshot.tau_nm[joint] - mean;
                delta * delta
            })
            .sum::<f64>()
            / sample_count;
        max_std = max_std.max(variance.sqrt());
    }

    max_std
}

fn simplify_segment(
    samples: &[[f64; 6]],
    start_index: usize,
    end_index: usize,
    max_deviation_rad: f64,
    keep: &mut [bool],
) {
    if end_index <= start_index + 1 {
        return;
    }

    let mut worst_index = None;
    let mut worst_deviation = 0.0;
    for index in (start_index + 1)..end_index {
        let deviation =
            point_segment_deviation(&samples[index], &samples[start_index], &samples[end_index]);
        if deviation > worst_deviation {
            worst_deviation = deviation;
            worst_index = Some(index);
        }
    }

    if worst_deviation <= max_deviation_rad {
        return;
    }

    let split_index = worst_index.expect("interior point exists");
    keep[split_index] = true;
    simplify_segment(samples, start_index, split_index, max_deviation_rad, keep);
    simplify_segment(samples, split_index, end_index, max_deviation_rad, keep);
}

fn point_segment_deviation(point: &[f64; 6], start: &[f64; 6], end: &[f64; 6]) -> f64 {
    let mut segment_norm_sq = 0.0;
    let mut point_dot_segment = 0.0;
    for joint in 0..6 {
        let segment_delta = end[joint] - start[joint];
        segment_norm_sq += segment_delta * segment_delta;
        point_dot_segment += (point[joint] - start[joint]) * segment_delta;
    }

    let ratio = if segment_norm_sq == 0.0 {
        0.0
    } else {
        (point_dot_segment / segment_norm_sq).clamp(0.0, 1.0)
    };

    let mut max_deviation: f64 = 0.0;
    for joint in 0..6 {
        let projected = start[joint] + (end[joint] - start[joint]) * ratio;
        max_deviation = max_deviation.max((point[joint] - projected).abs());
    }
    max_deviation
}

fn validate_path(samples: &[[f64; 6]]) -> Result<()> {
    if samples.is_empty() {
        bail!("path must contain at least one sample");
    }
    for (index, sample) in samples.iter().enumerate() {
        validate_joint_vector(&format!("path sample {index}"), sample)?;
    }
    Ok(())
}

fn validate_joint_vector(name: &str, values: &[f64; 6]) -> Result<()> {
    if values.iter().any(|value| !value.is_finite()) {
        bail!("{name} must contain only finite values");
    }
    Ok(())
}

fn validate_positive_finite(name: &str, value: f64) -> Result<()> {
    if !value.is_finite() {
        bail!("{name} must be finite");
    }
    if value <= 0.0 {
        bail!("{name} must be positive");
    }
    Ok(())
}

fn validate_max_step_rad(max_step_rad: f64) -> Result<()> {
    validate_positive_finite("max_step_rad", max_step_rad)?;
    if max_step_rad < MIN_SAFE_STEP_RAD {
        bail!("max_step_rad must be >= {MIN_SAFE_STEP_RAD}");
    }
    if max_step_rad > MAX_SAFE_STEP_RAD {
        bail!("max_step_rad must be <= {MAX_SAFE_STEP_RAD}");
    }
    Ok(())
}

fn max_abs_delta(left: &[f64; 6], right: &[f64; 6]) -> f64 {
    left.iter()
        .zip(right.iter())
        .map(|(left, right)| (right - left).abs())
        .fold(0.0_f64, f64::max)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gravity::artifact::{LoadedPath, PathHeader, PathSampleRow};
    use std::cell::Cell;
    use std::collections::VecDeque;
    use std::path::PathBuf;
    use tempfile::tempdir;

    #[test]
    fn simplify_path_preserves_order_and_max_deviation() {
        let path = vec![
            [0.0; 6],
            [0.001, 0.0, 0.0, 0.0, 0.0, 0.0],
            [0.02, 0.0, 0.0, 0.0, 0.0, 0.0],
        ];

        let simplified = simplify_path(&path, 0.005).unwrap();

        assert_eq!(simplified.first(), path.first());
        assert_eq!(simplified.last(), path.last());
        assert!(simplified.len() < path.len());
    }

    #[test]
    fn waypoint_plan_respects_max_step_rad() {
        let waypoints =
            interpolate_waypoints([0.0; 6], [0.05, 0.0, 0.0, 0.0, 0.0, 0.0], 0.02).unwrap();

        for pair in waypoints.windows(2) {
            let max_delta = pair[0]
                .iter()
                .zip(pair[1].iter())
                .map(|(left, right)| (right - left).abs())
                .fold(0.0_f64, f64::max);
            assert!(max_delta <= 0.0200001, "{max_delta}");
        }
    }

    #[test]
    fn interpolate_waypoints_rejects_tiny_step_without_panicking() {
        let result = std::panic::catch_unwind(|| {
            interpolate_waypoints([0.0; 6], [1.0, 0.0, 0.0, 0.0, 0.0, 0.0], f64::MIN_POSITIVE)
        });

        assert!(
            result.is_ok(),
            "tiny max_step_rad must return Err, not panic"
        );
        let err = result.unwrap().unwrap_err();
        assert!(err.to_string().contains("max_step_rad"), "{err:#}");
    }

    #[test]
    fn interpolate_waypoints_rejects_excessive_waypoint_count() {
        let result = interpolate_waypoints([0.0; 6], [100.0, 0.0, 0.0, 0.0, 0.0, 0.0], 0.001);

        match result {
            Ok(waypoints) => panic!(
                "expected excessive waypoint count error, got {} waypoints",
                waypoints.len()
            ),
            Err(err) => assert!(err.to_string().contains("waypoint"), "{err:#}"),
        }
    }

    #[test]
    fn stable_sample_gate_accepts_only_complete_stable_snapshots() {
        let target = [0.1, 0.0, 0.0, 0.0, 0.0, 0.0];
        let criteria = StableSampleCriteria {
            stable_velocity_rad_s: 0.02,
            stable_tracking_error_rad: 0.005,
            stable_torque_std_nm: 0.05,
            sample_count: 3,
        };

        let mut stable_stepper = FakeStepper::new(vec![
            stable_snapshot(target, [1.00; 6]),
            stable_snapshot(target, [1.01; 6]),
            stable_snapshot(target, [0.99; 6]),
        ]);
        let accepted = sample_stable_waypoint(&mut stable_stepper, target, criteria).unwrap();
        assert_eq!(accepted.q_rad, target);
        assert_eq!(stable_stepper.moved_targets, vec![target]);

        let rejection_cases = [
            (
                "velocity",
                vec![
                    snapshot_with(
                        target,
                        [0.021, 0.0, 0.0, 0.0, 0.0, 0.0],
                        [1.0; 6],
                        0x3f,
                        0x3f
                    );
                    3
                ],
            ),
            (
                "tracking",
                vec![
                    snapshot_with(
                        [0.106, 0.0, 0.0, 0.0, 0.0, 0.0],
                        [0.0; 6],
                        [1.0; 6],
                        0x3f,
                        0x3f,
                    );
                    3
                ],
            ),
            (
                "torque_std",
                vec![
                    stable_snapshot(target, [1.0; 6]),
                    stable_snapshot(target, [1.2; 6]),
                    stable_snapshot(target, [0.8; 6]),
                ],
            ),
            (
                "position_mask",
                vec![snapshot_with(target, [0.0; 6], [1.0; 6], 0x1f, 0x3f); 3],
            ),
            (
                "dynamic_mask",
                vec![snapshot_with(target, [0.0; 6], [1.0; 6], 0x3f, 0x1f); 3],
            ),
        ];

        for (name, snapshots) in rejection_cases {
            let mut stepper = FakeStepper::new(snapshots);
            let err = sample_stable_waypoint(&mut stepper, target, criteria).unwrap_err();
            assert!(!err.to_string().is_empty(), "{name}");
        }
    }

    #[test]
    fn replay_snapshot_rejects_stale_required_feedback_streams() {
        let now_us = 1_000_000;
        let max_age = Duration::from_millis(200);
        let fresh_host_us = now_us;
        let stale_host_us = now_us - max_age.as_micros() as u64 - 1;

        let stale_position = replay_snapshot_from_states(
            replay_position_state(stale_host_us, 0x3f),
            replay_dynamic_state(fresh_host_us, 0x3f),
            now_us,
            max_age,
        )
        .expect_err("stale position feedback must be rejected even when dynamic is fresh");
        assert!(stale_position.to_string().contains("position"));

        let stale_dynamic = replay_snapshot_from_states(
            replay_position_state(fresh_host_us, 0x3f),
            replay_dynamic_state(stale_host_us, 0x3f),
            now_us,
            max_age,
        )
        .expect_err("stale dynamic feedback must be rejected even when position is fresh");
        assert!(stale_dynamic.to_string().contains("dynamic"));

        replay_snapshot_from_states(
            replay_position_state(fresh_host_us, 0x3f),
            replay_dynamic_state(fresh_host_us, 0x3f),
            now_us,
            max_age,
        )
        .expect("fresh complete feedback streams should be accepted");
    }

    #[test]
    fn samples_artifact_prepares_same_directory_temp_before_hardware() {
        let dir = tempdir().unwrap();
        let final_path = dir.path().join("samples.jsonl");

        let pending = PendingSamplesArtifact::create(&final_path).unwrap();

        assert!(!final_path.exists());
        assert!(pending.temp_path().exists());
        assert_eq!(pending.temp_path().parent(), Some(dir.path()));
    }

    #[test]
    fn samples_artifact_persist_failure_keeps_recoverable_temp_without_final_output() {
        let dir = tempdir().unwrap();
        let final_path = dir.path().join("samples.jsonl");
        let mut pending = PendingSamplesArtifact::create(&final_path).unwrap();
        let temp_path = pending.temp_path().to_path_buf();
        pending.force_persist_error_for_test();

        let err = write_samples_artifact(
            pending,
            &sample_header_for_tests(),
            &[sample_row_for_tests()],
        )
        .expect_err("forced persist failure should not create the final artifact");

        assert!(!final_path.exists());
        assert!(temp_path.exists());
        assert!(
            err.to_string().contains(&temp_path.display().to_string()),
            "{err:#}"
        );
    }

    #[test]
    fn samples_artifact_persist_does_not_overwrite_existing_final_path() {
        let dir = tempdir().unwrap();
        let final_path = dir.path().join("samples.jsonl");
        let pending = PendingSamplesArtifact::create(&final_path).unwrap();
        let temp_path = pending.temp_path().to_path_buf();
        std::fs::write(&final_path, "operator data\n").unwrap();

        let err = write_samples_artifact(
            pending,
            &sample_header_for_tests(),
            &[sample_row_for_tests()],
        )
        .expect_err("final path race should not overwrite existing data");

        assert_eq!(
            std::fs::read_to_string(&final_path).unwrap(),
            "operator data\n"
        );
        assert!(temp_path.exists());
        assert!(
            err.to_string().contains(&temp_path.display().to_string()),
            "{err:#}"
        );
    }

    #[test]
    fn replay_enable_gate_skips_enable_when_cancelled_before_enable() {
        let running = Arc::new(AtomicBool::new(false));
        let enable_calls = Cell::new(0);
        let disable_calls = Cell::new(0);

        let err = enable_replay_arm(
            &running,
            || {
                enable_calls.set(enable_calls.get() + 1);
                Ok(42_u8)
            },
            |_active| {
                disable_calls.set(disable_calls.get() + 1);
                Ok(())
            },
        )
        .expect_err("pre-enable cancellation should abort");

        assert!(err.to_string().contains("cancelled"), "{err:#}");
        assert_eq!(enable_calls.get(), 0);
        assert_eq!(disable_calls.get(), 0);
    }

    #[test]
    fn replay_enable_gate_disables_when_cancelled_after_enable() {
        let running = Arc::new(AtomicBool::new(true));
        let enable_calls = Cell::new(0);
        let disable_calls = Cell::new(0);

        let err = enable_replay_arm(
            &running,
            || {
                enable_calls.set(enable_calls.get() + 1);
                running.store(false, Ordering::SeqCst);
                Ok(42_u8)
            },
            |active| {
                assert_eq!(active, 42);
                disable_calls.set(disable_calls.get() + 1);
                Ok(())
            },
        )
        .expect_err("post-enable cancellation should disable and abort");

        assert!(err.to_string().contains("cancelled"), "{err:#}");
        assert_eq!(enable_calls.get(), 1);
        assert_eq!(disable_calls.get(), 1);
    }

    #[test]
    fn replay_records_only_stable_hold_samples() {
        let criteria = StableSampleCriteria {
            stable_velocity_rad_s: 0.02,
            stable_tracking_error_rad: 0.005,
            stable_torque_std_nm: 0.05,
            sample_count: 3,
        };
        let waypoints = vec![
            ReplayWaypoint {
                waypoint_id: 0,
                q_rad: [0.0; 6],
                pass_direction: PassDirection::Forward,
            },
            ReplayWaypoint {
                waypoint_id: 1,
                q_rad: [0.1, 0.0, 0.0, 0.0, 0.0, 0.0],
                pass_direction: PassDirection::Forward,
            },
            ReplayWaypoint {
                waypoint_id: 0,
                q_rad: [0.0; 6],
                pass_direction: PassDirection::Backward,
            },
        ];
        let mut stepper = FakeStepper::new(vec![
            stable_snapshot(waypoints[0].q_rad, [1.00; 6]),
            stable_snapshot(waypoints[0].q_rad, [1.01; 6]),
            stable_snapshot(waypoints[0].q_rad, [0.99; 6]),
            snapshot_with(
                [0.075, 0.0, 0.0, 0.0, 0.0, 0.0],
                [0.03, 0.0, 0.0, 0.0, 0.0, 0.0],
                [2.0; 6],
                0x3f,
                0x3f,
            ),
            snapshot_with(
                [0.09, 0.0, 0.0, 0.0, 0.0, 0.0],
                [0.025, 0.0, 0.0, 0.0, 0.0, 0.0],
                [2.2; 6],
                0x3f,
                0x3f,
            ),
            snapshot_with(
                [0.1, 0.0, 0.0, 0.0, 0.0, 0.0],
                [0.021, 0.0, 0.0, 0.0, 0.0, 0.0],
                [1.8; 6],
                0x3f,
                0x3f,
            ),
            stable_snapshot(waypoints[2].q_rad, [-0.50; 6]),
            stable_snapshot(waypoints[2].q_rad, [-0.49; 6]),
            stable_snapshot(waypoints[2].q_rad, [-0.51; 6]),
        ]);

        let rows = replay_quasi_static_samples(
            &mut stepper,
            &waypoints,
            criteria,
            Duration::from_millis(0),
        )
        .unwrap();

        assert_eq!(
            stepper.moved_targets,
            vec![waypoints[0].q_rad, waypoints[1].q_rad, waypoints[2].q_rad,]
        );
        assert_eq!(rows.len(), 2);

        assert_eq!(rows[0].waypoint_id, 0);
        assert_eq!(rows[0].pass_direction, PassDirection::Forward);
        assert_eq!(rows[0].q_rad, waypoints[0].q_rad);
        assert_eq!(rows[0].dq_rad_s, [0.0; 6]);
        assert_eq!(rows[0].tau_nm, [0.99; 6]);
        assert_eq!(rows[0].position_valid_mask, 0x3f);
        assert_eq!(rows[0].dynamic_valid_mask, 0x3f);
        assert_eq!(rows[0].stable_velocity_rad_s, 0.0);
        assert_eq!(rows[0].stable_tracking_error_rad, 0.0);
        assert!(rows[0].stable_torque_std_nm > 0.0);

        assert_eq!(rows[1].waypoint_id, 0);
        assert_eq!(rows[1].pass_direction, PassDirection::Backward);
        assert_eq!(rows[1].q_rad, waypoints[2].q_rad);
        assert_eq!(rows[1].dq_rad_s, [0.0; 6]);
        assert_eq!(rows[1].tau_nm, [-0.51; 6]);
        assert_eq!(rows[1].position_valid_mask, 0x3f);
        assert_eq!(rows[1].dynamic_valid_mask, 0x3f);
        assert_eq!(rows[1].stable_velocity_rad_s, 0.0);
        assert_eq!(rows[1].stable_tracking_error_rad, 0.0);
        assert!(rows[1].stable_torque_std_nm > 0.0);
    }

    #[test]
    fn dry_run_plan_validates_header_and_summarizes_waypoints() {
        let args = replay_args("slave", Some("socketcan:can0"), None);
        let loaded = loaded_path(vec![
            path_row(0, [0.0; 6]),
            path_row(1, [0.001, 0.0, 0.0, 0.0, 0.0, 0.0]),
            path_row(2, [0.05, 0.0, 0.0, 0.0, 0.0, 0.0]),
        ]);

        let report = build_dry_run_plan(&args, &loaded).unwrap();

        assert_eq!(report.role, "slave");
        assert_eq!(report.target, "socketcan:can0");
        assert_eq!(report.joint_map, "identity");
        assert_eq!(report.load_profile, "load");
        assert_eq!(report.raw_sample_count, 3);
        assert_eq!(report.simplified_count, 2);
        assert_eq!(report.waypoint_count, 7);
        assert!(report.max_simplification_deviation_rad <= 1e-12);
        assert!(report.simplification_tolerance_rad <= 0.005);
        assert_eq!(report.joint_min_rad[0], 0.0);
        assert_eq!(report.joint_max_rad[0], 0.05);
        assert_eq!(report.estimated_duration.as_millis(), 5_600);
    }

    #[test]
    fn dry_run_simplification_preserves_corners_independent_of_max_step() {
        let mut args = replay_args("slave", Some("socketcan:can0"), None);
        args.max_step_rad = MAX_SAFE_STEP_RAD;
        args.bidirectional = false;
        let loaded = loaded_path(vec![
            path_row(0, [0.0; 6]),
            path_row(1, [0.05, 0.05, 0.0, 0.0, 0.0, 0.0]),
            path_row(2, [0.1, 0.0, 0.0, 0.0, 0.0, 0.0]),
        ]);

        let report = build_dry_run_plan(&args, &loaded).unwrap();

        assert_eq!(report.simplified_count, 3);
        assert!(report.max_simplification_deviation_rad <= report.simplification_tolerance_rad);
    }

    #[test]
    fn dry_run_plan_rejects_role_and_target_mismatches() {
        let loaded = loaded_path(vec![path_row(0, [0.0; 6])]);

        let role_args = replay_args("master", Some("socketcan:can0"), None);
        let role_err = build_dry_run_plan(&role_args, &loaded).unwrap_err();
        assert!(role_err.to_string().contains("role"));

        let target_args = replay_args("slave", Some("socketcan:can1"), None);
        let target_err = build_dry_run_plan(&target_args, &loaded).unwrap_err();
        assert!(target_err.to_string().contains("target"));
    }

    #[test]
    fn dry_run_plan_rejects_incomplete_position_masks() {
        let args = replay_args("slave", Some("socketcan:can0"), None);
        let mut row = path_row(0, [0.0; 6]);
        row.position_valid_mask = 0x1f;
        let loaded = loaded_path(vec![row]);

        let err = build_dry_run_plan(&args, &loaded).unwrap_err();

        assert!(err.to_string().contains("position_valid_mask"), "{err:#}");
    }

    #[test]
    fn validate_replay_args_rejects_nonfinite_and_unsafe_values() {
        let mut args = replay_args("slave", Some("socketcan:can0"), None);
        args.max_velocity_rad_s = f64::NAN;
        assert!(validate_replay_args(&args).unwrap_err().to_string().contains("finite"));

        args = replay_args("slave", Some("socketcan:can0"), None);
        args.max_velocity_rad_s = MAX_SAFE_VELOCITY_RAD_S + 0.001;
        assert!(validate_replay_args(&args).unwrap_err().to_string().contains("<="));

        args = replay_args("slave", Some("socketcan:can0"), None);
        args.max_step_rad = MAX_SAFE_STEP_RAD + 0.001;
        assert!(validate_replay_args(&args).unwrap_err().to_string().contains("<="));

        args = replay_args("slave", Some("socketcan:can0"), None);
        args.sample_ms = 0;
        assert!(validate_replay_args(&args).unwrap_err().to_string().contains("sample_ms"));
    }

    struct FakeStepper {
        snapshots: VecDeque<ReplaySnapshot>,
        moved_targets: Vec<[f64; 6]>,
    }

    impl FakeStepper {
        fn new(snapshots: Vec<ReplaySnapshot>) -> Self {
            Self {
                snapshots: snapshots.into(),
                moved_targets: Vec::new(),
            }
        }
    }

    impl ReplayStepper for FakeStepper {
        fn move_toward(&mut self, target: [f64; 6]) -> Result<()> {
            self.moved_targets.push(target);
            Ok(())
        }

        fn snapshot(&mut self) -> Result<ReplaySnapshot> {
            self.snapshots
                .pop_front()
                .ok_or_else(|| anyhow::anyhow!("no fake snapshot available"))
        }
    }

    fn replay_position_state(
        host_rx_mono_us: u64,
        frame_valid_mask: u8,
    ) -> piper_sdk::driver::JointPositionState {
        piper_sdk::driver::JointPositionState {
            hardware_timestamp_us: host_rx_mono_us,
            host_rx_mono_us,
            joint_pos: [0.0; 6],
            frame_valid_mask,
            ..Default::default()
        }
    }

    fn replay_dynamic_state(
        group_host_rx_mono_us: u64,
        valid_mask: u8,
    ) -> piper_sdk::driver::JointDynamicState {
        piper_sdk::driver::JointDynamicState {
            group_timestamp_us: group_host_rx_mono_us,
            group_host_rx_mono_us,
            joint_vel: [0.0; 6],
            joint_current: [0.0; 6],
            valid_mask,
            ..Default::default()
        }
    }

    fn sample_header_for_tests() -> SamplesHeader {
        SamplesHeader {
            row_type: "header".to_string(),
            artifact_kind: "quasi-static-samples".to_string(),
            schema_version: 1,
            source_path: "path.jsonl".to_string(),
            source_sha256: "abc".to_string(),
            role: "slave".to_string(),
            target: "socketcan:can0".to_string(),
            joint_map: "identity".to_string(),
            load_profile: "load".to_string(),
            torque_convention: crate::gravity::TORQUE_CONVENTION.to_string(),
            frequency_hz: REPLAY_SAMPLE_FREQUENCY_HZ,
            max_velocity_rad_s: 0.08,
            max_step_rad: 0.02,
            settle_ms: 500,
            sample_ms: 300,
            stable_velocity_rad_s: DEFAULT_STABLE_VELOCITY_RAD_S,
            stable_tracking_error_rad: DEFAULT_STABLE_TRACKING_ERROR_RAD,
            stable_torque_std_nm: DEFAULT_STABLE_TORQUE_STD_NM,
            waypoint_count: 1,
            accepted_waypoint_count: 1,
            rejected_waypoint_count: 0,
        }
    }

    fn sample_row_for_tests() -> QuasiStaticSampleRow {
        QuasiStaticSampleRow {
            row_type: "quasi-static-sample".to_string(),
            waypoint_id: 0,
            segment_id: None,
            pass_direction: PassDirection::Forward,
            host_mono_us: 1,
            raw_timestamp_us: None,
            q_rad: [0.0; 6],
            dq_rad_s: [0.0; 6],
            tau_nm: [0.0; 6],
            position_valid_mask: 0x3f,
            dynamic_valid_mask: 0x3f,
            stable_velocity_rad_s: 0.0,
            stable_tracking_error_rad: 0.0,
            stable_torque_std_nm: 0.0,
        }
    }

    fn stable_snapshot(q_rad: [f64; 6], tau_nm: [f64; 6]) -> ReplaySnapshot {
        snapshot_with(q_rad, [0.0; 6], tau_nm, 0x3f, 0x3f)
    }

    fn snapshot_with(
        q_rad: [f64; 6],
        dq_rad_s: [f64; 6],
        tau_nm: [f64; 6],
        position_valid_mask: u8,
        dynamic_valid_mask: u8,
    ) -> ReplaySnapshot {
        ReplaySnapshot {
            host_mono_us: 1,
            raw_timestamp_us: Some(1),
            q_rad,
            dq_rad_s,
            tau_nm,
            position_valid_mask,
            dynamic_valid_mask,
        }
    }

    fn replay_args(
        role: impl Into<String>,
        target: Option<&str>,
        interface: Option<&str>,
    ) -> GravityReplaySampleArgs {
        GravityReplaySampleArgs {
            role: role.into(),
            target: target.map(str::to_string),
            interface: interface.map(str::to_string),
            path: PathBuf::from("path.jsonl"),
            out: PathBuf::from("samples.jsonl"),
            max_velocity_rad_s: 0.08,
            max_step_rad: 0.02,
            settle_ms: 500,
            sample_ms: 300,
            bidirectional: true,
            dry_run: true,
        }
    }

    fn loaded_path(rows: Vec<PathSampleRow>) -> LoadedPath {
        LoadedPath {
            header: PathHeader {
                row_type: "header".to_string(),
                artifact_kind: "path".to_string(),
                schema_version: 1,
                role: "slave".to_string(),
                target: "socketcan:can0".to_string(),
                joint_map: "identity".to_string(),
                load_profile: "load".to_string(),
                torque_convention: crate::gravity::TORQUE_CONVENTION.to_string(),
                notes: None,
            },
            rows,
        }
    }

    fn path_row(sample_index: u64, q_rad: [f64; 6]) -> PathSampleRow {
        PathSampleRow {
            row_type: "path-sample".to_string(),
            sample_index,
            host_mono_us: sample_index,
            raw_timestamp_us: None,
            q_rad,
            dq_rad_s: [0.0; 6],
            tau_nm: [0.0; 6],
            position_valid_mask: 0x3f,
            dynamic_valid_mask: 0x3f,
            segment_id: None,
        }
    }
}
