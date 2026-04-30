use crate::commands::gravity::GravityReplaySampleArgs;
use crate::gravity::artifact::{LoadedPath, PathHeader, read_path};
use anyhow::{Result, anyhow, bail};
use piper_control::TargetSpec;
use std::time::Duration;

const MAX_SAFE_STEP_RAD: f64 = 0.2;
const MAX_SAFE_VELOCITY_RAD_S: f64 = 1.0;
const COMPLETE_JOINT_MASK: u8 = 0x3f;

pub async fn run(args: GravityReplaySampleArgs) -> Result<()> {
    validate_replay_args(&args)?;

    if !args.dry_run {
        bail!(
            "active hardware gravity replay is implemented in Task 10; rerun with --dry-run to inspect the replay plan"
        );
    }

    let loaded = read_path(&args.path)?;
    let report = build_dry_run_plan(&args, &loaded)?;
    print_dry_run_report(&args, &loaded.header, &report);

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
    pub waypoint_count: usize,
    pub joint_min_rad: [f64; 6],
    pub joint_max_rad: [f64; 6],
    pub estimated_duration: Duration,
}

pub(crate) fn simplify_path(samples: &[[f64; 6]], max_deviation_rad: f64) -> Result<Vec<[f64; 6]>> {
    validate_path(samples)?;
    validate_positive_finite("max_deviation_rad", max_deviation_rad)?;

    if samples.len() <= 2 {
        return Ok(samples.to_vec());
    }

    let mut keep = vec![false; samples.len()];
    keep[0] = true;
    keep[samples.len() - 1] = true;
    simplify_segment(samples, 0, samples.len() - 1, max_deviation_rad, &mut keep);

    Ok(samples
        .iter()
        .zip(keep)
        .filter_map(|(sample, keep)| keep.then_some(*sample))
        .collect())
}

pub(crate) fn interpolate_waypoints(
    start: [f64; 6],
    end: [f64; 6],
    max_step_rad: f64,
) -> Result<Vec<[f64; 6]>> {
    validate_joint_vector("start", &start)?;
    validate_joint_vector("end", &end)?;
    validate_positive_finite("max_step_rad", max_step_rad)?;
    if max_step_rad > MAX_SAFE_STEP_RAD {
        bail!("max_step_rad must be <= {MAX_SAFE_STEP_RAD}");
    }

    let max_delta = max_abs_delta(&start, &end);
    if max_delta == 0.0 {
        return Ok(vec![start]);
    }

    let segment_count = (max_delta / max_step_rad).ceil() as usize;
    let mut waypoints = Vec::with_capacity(segment_count + 1);
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
    validate_positive_finite("max_step_rad", args.max_step_rad)?;
    if args.max_step_rad > MAX_SAFE_STEP_RAD {
        bail!("max_step_rad must be <= {MAX_SAFE_STEP_RAD}");
    }
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
    validate_replay_args(args)?;
    let target = resolve_replay_target(args)?;
    validate_path_header_matches_args(args, &target, &loaded.header)?;

    if loaded.rows.is_empty() {
        bail!("path must contain at least one sample");
    }

    let raw_samples = loaded.rows.iter().map(|row| row.q_rad).collect::<Vec<_>>();
    let simplified = simplify_path(&raw_samples, args.max_step_rad)?;
    let mut waypoints = waypoint_plan_from_simplified_path(&simplified, args.max_step_rad)?;
    if args.bidirectional {
        append_reverse_pass(&mut waypoints);
    }
    let (joint_min_rad, joint_max_rad) = joint_ranges(&waypoints)?;
    let estimated_duration = estimate_duration(
        waypoints.len(),
        args.max_velocity_rad_s,
        args.settle_ms,
        args.sample_ms,
    )?;

    Ok(DryRunReport {
        role: loaded.header.role.clone(),
        target,
        joint_map: loaded.header.joint_map.clone(),
        load_profile: loaded.header.load_profile.clone(),
        raw_sample_count: loaded.rows.len(),
        simplified_count: simplified.len(),
        waypoint_count: waypoints.len(),
        joint_min_rad,
        joint_max_rad,
        estimated_duration,
    })
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

fn resolve_replay_target(args: &GravityReplaySampleArgs) -> Result<String> {
    match (args.target.as_deref(), args.interface.as_deref()) {
        (Some(_), Some(_)) => bail!("use either --target or --interface, not both"),
        (Some(target), None) => {
            let target_spec = target
                .parse::<TargetSpec>()
                .map_err(|error| anyhow!("invalid --target {target:?}: {error}"))?;
            Ok(target_spec.to_string())
        },
        (None, Some(interface)) => {
            if interface.trim().is_empty() {
                bail!("--interface must not be empty");
            }
            Ok(TargetSpec::SocketCan {
                iface: interface.to_string(),
            }
            .to_string())
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

fn append_reverse_pass(plan: &mut Vec<[f64; 6]>) {
    if plan.len() <= 1 {
        return;
    }
    let reverse = plan.iter().rev().copied().skip(1).collect::<Vec<_>>();
    plan.extend(reverse);
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
    fn snapshot(&mut self) -> Result<ReplaySnapshot>;
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

    let mut snapshots = Vec::with_capacity(criteria.sample_count);
    for _ in 0..criteria.sample_count {
        let snapshot = stepper.snapshot()?;
        validate_snapshot(&snapshot)?;
        snapshots.push(snapshot);
    }

    validate_stable_snapshots(&snapshots, &target, criteria)?;
    Ok(*snapshots.last().expect("sample_count is positive"))
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

fn validate_stable_snapshots(
    snapshots: &[ReplaySnapshot],
    target: &[f64; 6],
    criteria: StableSampleCriteria,
) -> Result<()> {
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
        if max_velocity > criteria.stable_velocity_rad_s {
            bail!(
                "unstable velocity {max_velocity:.6} rad/s exceeds {:.6}",
                criteria.stable_velocity_rad_s
            );
        }

        let tracking_error = max_abs_delta(&snapshot.q_rad, target);
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

    Ok(())
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
    use std::collections::VecDeque;
    use std::path::PathBuf;

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
        assert_eq!(report.joint_min_rad[0], 0.0);
        assert_eq!(report.joint_max_rad[0], 0.05);
        assert_eq!(report.estimated_duration.as_millis(), 5_600);
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
