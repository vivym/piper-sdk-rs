use crate::commands::gravity::GravityFitArgs;
use crate::gravity::{
    artifact::{QuasiStaticSampleRow, SamplesHeader, read_quasi_static_samples},
    model::{
        FitMetadata, FitQuality, JOINT_COUNT, LinearModelSection, QuasiStaticTorqueModel,
        TRIG_V1_FEATURE_COUNT, TrainingRange, trig_v1_feature_names, trig_v1_features,
    },
};
use anyhow::{Context, Result, bail};
use nalgebra::DMatrix;
use std::{
    collections::BTreeSet,
    fs::{self, OpenOptions},
    io::Write,
    time::{SystemTime, UNIX_EPOCH},
};

const MIN_TRAINING_WAYPOINTS_PER_FEATURE: usize = 10;

#[derive(Debug, Clone, Copy)]
pub struct FitOptions {
    pub ridge_lambda: f64,
    pub holdout_ratio: f64,
    pub regularize_bias: bool,
}

struct NormalEquations {
    g: DMatrix<f64>,
    b: DMatrix<f64>,
}

struct GroupSplit {
    train_group_ids: Vec<String>,
    holdout_group_ids: Vec<String>,
}

struct ResidualMetrics {
    rms_residual_nm: [f64; JOINT_COUNT],
    p95_residual_nm: [f64; JOINT_COUNT],
    max_residual_nm: [f64; JOINT_COUNT],
}

pub fn run(args: GravityFitArgs) -> Result<()> {
    let basis = args.basis.as_deref().unwrap_or(crate::gravity::BASIS_TRIG_V1);
    if basis != crate::gravity::BASIS_TRIG_V1 {
        bail!("unsupported gravity basis {basis}");
    }
    if !args.ridge_lambda.is_finite() || args.ridge_lambda <= 0.0 {
        bail!("ridge_lambda must be finite and > 0.0");
    }

    if args.out.exists() {
        bail!(
            "{} already exists; refusing to overwrite",
            args.out.display()
        );
    }

    let loaded = read_quasi_static_samples(&args.samples)?;
    let options = FitOptions {
        ridge_lambda: args.ridge_lambda,
        holdout_ratio: args.holdout_ratio,
        regularize_bias: false,
    };
    let model = fit_from_rows(loaded.header, loaded.rows, options)?;

    if let Some(parent) = args.out.parent().filter(|parent| !parent.as_os_str().is_empty()) {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    let toml = toml::to_string_pretty(&model).context("failed to serialize gravity model")?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&args.out)
        .with_context(|| format!("failed to create {}", args.out.display()))?;
    file.write_all(toml.as_bytes())
        .with_context(|| format!("failed to write {}", args.out.display()))?;

    println!("gravity fit complete");
    println!("  samples: {}", model.sample_count);
    println!(
        "  groups: train={} holdout={}",
        model.fit.train_group_ids.len(),
        model.fit.holdout_group_ids.len()
    );
    println!("  condition_number: {}", model.fit_quality.condition_number);
    println!("  model: {}", args.out.display());

    Ok(())
}

fn fit_from_rows(
    header: SamplesHeader,
    rows: Vec<QuasiStaticSampleRow>,
    options: FitOptions,
) -> Result<QuasiStaticTorqueModel> {
    validate_fit_inputs(&header, &rows, options)?;

    let group_split = split_train_holdout_groups(&rows, options.holdout_ratio)?;
    if group_split.train_group_ids.len() < 2 {
        bail!("expected at least 2 training groups");
    }
    let holdout_groups: BTreeSet<&str> =
        group_split.holdout_group_ids.iter().map(String::as_str).collect();

    let mut normal = NormalEquations::new();
    let mut train_count = 0usize;
    let mut training_waypoint_ids = BTreeSet::new();
    for row in &rows {
        let group_id = group_id_for_row(row);
        if holdout_groups.contains(group_id.as_str()) {
            continue;
        }
        normal.accumulate(row);
        train_count += 1;
        training_waypoint_ids.insert(row.waypoint_id);
    }
    if train_count == 0 {
        bail!("holdout split left no training samples");
    }
    let minimum_training_waypoints = TRIG_V1_FEATURE_COUNT * MIN_TRAINING_WAYPOINTS_PER_FEATURE;
    if training_waypoint_ids.len() < minimum_training_waypoints {
        bail!(
            "expected at least {minimum_training_waypoints} training waypoints, got {}",
            training_waypoint_ids.len()
        );
    }

    normal.add_ridge(options);
    let condition_number = condition_number(&normal.g);
    let (solution, solver, fallback_solver) = solve_normal_equations(&normal)?;
    let coefficients_nm = solution_to_coefficients(&solution)?;

    let train_metrics = residual_metrics(
        rows.iter()
            .filter(|row| !holdout_groups.contains(group_id_for_row(row).as_str())),
        &coefficients_nm,
    )?;
    let holdout_metrics = residual_metrics(
        rows.iter()
            .filter(|row| holdout_groups.contains(group_id_for_row(row).as_str())),
        &coefficients_nm,
    )?;
    let training_range = training_range(&rows);

    Ok(QuasiStaticTorqueModel {
        schema_version: 1,
        model_kind: crate::gravity::MODEL_KIND.to_string(),
        basis: crate::gravity::BASIS_TRIG_V1.to_string(),
        role: header.role,
        joint_map: header.joint_map,
        load_profile: header.load_profile,
        torque_convention: header.torque_convention,
        created_at_unix_ms: current_unix_ms()?,
        sample_count: rows.len(),
        frequency_hz: header.frequency_hz,
        fit: FitMetadata {
            ridge_lambda: options.ridge_lambda,
            regularize_bias: options.regularize_bias,
            solver,
            fallback_solver,
            holdout_strategy: "deterministic-group-stride-v1".to_string(),
            holdout_ratio: options.holdout_ratio,
            train_group_ids: group_split.train_group_ids,
            holdout_group_ids: group_split.holdout_group_ids,
        },
        training_range,
        fit_quality: FitQuality {
            rms_residual_nm: train_metrics.rms_residual_nm,
            p95_residual_nm: train_metrics.p95_residual_nm,
            max_residual_nm: train_metrics.max_residual_nm,
            holdout_rms_residual_nm: holdout_metrics.rms_residual_nm,
            holdout_p95_residual_nm: holdout_metrics.p95_residual_nm,
            holdout_max_residual_nm: holdout_metrics.max_residual_nm,
            condition_number,
        },
        model: LinearModelSection {
            feature_names: trig_v1_feature_names(),
            coefficients_nm,
        },
        coverage: None,
    })
}

impl NormalEquations {
    fn new() -> Self {
        Self {
            g: DMatrix::zeros(TRIG_V1_FEATURE_COUNT, TRIG_V1_FEATURE_COUNT),
            b: DMatrix::zeros(TRIG_V1_FEATURE_COUNT, JOINT_COUNT),
        }
    }

    fn accumulate(&mut self, row: &QuasiStaticSampleRow) {
        let phi = trig_v1_features(row.q_rad);
        for i in 0..TRIG_V1_FEATURE_COUNT {
            for j in 0..TRIG_V1_FEATURE_COUNT {
                self.g[(i, j)] += phi[i] * phi[j];
            }
            for out in 0..JOINT_COUNT {
                self.b[(i, out)] += phi[i] * row.tau_nm[out];
            }
        }
    }

    fn add_ridge(&mut self, options: FitOptions) {
        for i in 0..TRIG_V1_FEATURE_COUNT {
            if i == 0 && !options.regularize_bias {
                continue;
            }
            self.g[(i, i)] += options.ridge_lambda;
        }
    }
}

fn validate_fit_inputs(
    header: &SamplesHeader,
    rows: &[QuasiStaticSampleRow],
    options: FitOptions,
) -> Result<()> {
    if rows.is_empty() {
        bail!("expected at least one quasi-static sample row");
    }
    if !header.frequency_hz.is_finite() {
        bail!("sample header frequency_hz must be finite");
    }
    if !options.ridge_lambda.is_finite() || options.ridge_lambda <= 0.0 {
        bail!("ridge_lambda must be finite and > 0.0");
    }
    if !options.holdout_ratio.is_finite()
        || options.holdout_ratio < 0.0
        || options.holdout_ratio >= 1.0
    {
        bail!("holdout_ratio must be finite and in [0.0, 1.0)");
    }
    for (index, row) in rows.iter().enumerate() {
        if row.q_rad.iter().any(|value| !value.is_finite()) {
            bail!("row {index} q_rad must be finite");
        }
        if row.dq_rad_s.iter().any(|value| !value.is_finite()) {
            bail!("row {index} dq_rad_s must be finite");
        }
        if row.tau_nm.iter().any(|value| !value.is_finite()) {
            bail!("row {index} tau_nm must be finite");
        }
    }
    Ok(())
}

fn split_train_holdout_groups(
    rows: &[QuasiStaticSampleRow],
    holdout_ratio: f64,
) -> Result<GroupSplit> {
    let mut all_groups = BTreeSet::new();
    for row in rows {
        all_groups.insert(group_id_for_row(row));
    }

    let mut holdout_groups = BTreeSet::new();
    if holdout_ratio > 0.0 {
        if all_groups.len() < 2 {
            bail!("holdout requires at least 2 distinct groups");
        }
        let stride = (1.0 / holdout_ratio).round().max(1.0) as usize;
        for (index, group_id) in all_groups.iter().enumerate() {
            if index % stride == 0 {
                holdout_groups.insert(group_id.clone());
            }
        }
        if holdout_groups.is_empty()
            && let Some(first_group_id) = all_groups.first()
        {
            holdout_groups.insert(first_group_id.clone());
        }
    }

    let train_group_ids = all_groups
        .iter()
        .filter(|group_id| !holdout_groups.contains(*group_id))
        .cloned()
        .collect::<Vec<_>>();
    if train_group_ids.is_empty() {
        bail!("holdout split left no training groups; choose a smaller holdout_ratio");
    }

    Ok(GroupSplit {
        train_group_ids,
        holdout_group_ids: holdout_groups.into_iter().collect(),
    })
}

fn group_id_for_row(row: &QuasiStaticSampleRow) -> String {
    match &row.segment_id {
        Some(segment_id) => format!("segment:{segment_id}"),
        None => format!("waypoint-block:{}", row.waypoint_id / 10),
    }
}

fn solve_normal_equations(
    normal: &NormalEquations,
) -> Result<(DMatrix<f64>, String, Option<String>)> {
    match normal.g.clone().cholesky() {
        Some(cholesky) => Ok((cholesky.solve(&normal.b), "cholesky".to_string(), None)),
        None => {
            let svd = normal.g.clone().svd(true, true);
            let solution = svd
                .solve(&normal.b, 1e-12)
                .map_err(|err| anyhow::anyhow!("SVD solve failed: {err}"))?;
            Ok((solution, "svd".to_string(), Some("svd".to_string())))
        },
    }
}

fn solution_to_coefficients(solution: &DMatrix<f64>) -> Result<Vec<Vec<f64>>> {
    if solution.nrows() != TRIG_V1_FEATURE_COUNT || solution.ncols() != JOINT_COUNT {
        bail!(
            "expected solution shape {}x{}, got {}x{}",
            TRIG_V1_FEATURE_COUNT,
            JOINT_COUNT,
            solution.nrows(),
            solution.ncols()
        );
    }

    let mut coefficients_nm = vec![vec![0.0; TRIG_V1_FEATURE_COUNT]; JOINT_COUNT];
    for joint in 0..JOINT_COUNT {
        for feature in 0..TRIG_V1_FEATURE_COUNT {
            let coefficient = solution[(feature, joint)];
            if !coefficient.is_finite() {
                bail!("fit produced non-finite coefficient");
            }
            coefficients_nm[joint][feature] = coefficient;
        }
    }
    Ok(coefficients_nm)
}

fn condition_number(g: &DMatrix<f64>) -> f64 {
    let singular_values = g.clone().svd(false, false).singular_values;
    let largest = singular_values.iter().copied().fold(0.0_f64, f64::max);
    if !largest.is_finite() || largest <= 0.0 {
        return f64::INFINITY;
    }

    let tolerance = f64::EPSILON * largest.max(1.0) * TRIG_V1_FEATURE_COUNT as f64;
    let smallest_nonzero = singular_values
        .iter()
        .copied()
        .filter(|value| value.is_finite() && *value > 0.0)
        .fold(f64::INFINITY, f64::min);
    if !smallest_nonzero.is_finite() || smallest_nonzero <= tolerance {
        f64::INFINITY
    } else {
        largest / smallest_nonzero
    }
}

fn residual_metrics<'a>(
    rows: impl IntoIterator<Item = &'a QuasiStaticSampleRow>,
    coefficients_nm: &[Vec<f64>],
) -> Result<ResidualMetrics> {
    let mut count = 0usize;
    let mut sum_squares = [0.0; JOINT_COUNT];
    let mut max_residual_nm: [f64; JOINT_COUNT] = [0.0; JOINT_COUNT];
    let mut absolute_residuals = vec![Vec::new(); JOINT_COUNT];

    for row in rows {
        let features = trig_v1_features(row.q_rad);
        for joint in 0..JOINT_COUNT {
            let prediction: f64 = coefficients_nm[joint]
                .iter()
                .zip(features.iter())
                .map(|(coefficient, feature)| coefficient * feature)
                .sum();
            let residual = row.tau_nm[joint] - prediction;
            if !residual.is_finite() {
                bail!("fit produced non-finite residual");
            }
            let absolute_residual = residual.abs();
            sum_squares[joint] += residual * residual;
            max_residual_nm[joint] = max_residual_nm[joint].max(absolute_residual);
            absolute_residuals[joint].push(absolute_residual);
        }
        count += 1;
    }

    if count == 0 {
        return Ok(ResidualMetrics {
            rms_residual_nm: [0.0; JOINT_COUNT],
            p95_residual_nm: [0.0; JOINT_COUNT],
            max_residual_nm,
        });
    }

    let mut rms_residual_nm = [0.0; JOINT_COUNT];
    let mut p95_residual_nm = [0.0; JOINT_COUNT];
    for joint in 0..JOINT_COUNT {
        rms_residual_nm[joint] = (sum_squares[joint] / count as f64).sqrt();
        absolute_residuals[joint].sort_by(|left, right| {
            left.partial_cmp(right).expect("non-finite residuals are rejected")
        });
        p95_residual_nm[joint] = percentile_from_sorted(&absolute_residuals[joint], 0.95);
    }

    Ok(ResidualMetrics {
        rms_residual_nm,
        p95_residual_nm,
        max_residual_nm,
    })
}

fn percentile_from_sorted(values: &[f64], quantile: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let index = ((values.len() as f64 * quantile).ceil() as usize)
        .saturating_sub(1)
        .min(values.len() - 1);
    values[index]
}

fn training_range(rows: &[QuasiStaticSampleRow]) -> TrainingRange {
    let mut q_min_rad = [f64::INFINITY; JOINT_COUNT];
    let mut q_max_rad = [f64::NEG_INFINITY; JOINT_COUNT];
    let mut tau_min_nm = [f64::INFINITY; JOINT_COUNT];
    let mut tau_max_nm = [f64::NEG_INFINITY; JOINT_COUNT];
    let mut dq_abs_values =
        (0..JOINT_COUNT).map(|_| Vec::with_capacity(rows.len())).collect::<Vec<_>>();
    let mut waypoint_ids = BTreeSet::new();
    let mut segment_ids = BTreeSet::new();

    for row in rows {
        waypoint_ids.insert(row.waypoint_id);
        if let Some(segment_id) = &row.segment_id {
            segment_ids.insert(segment_id.as_str());
        }
        for joint in 0..JOINT_COUNT {
            q_min_rad[joint] = q_min_rad[joint].min(row.q_rad[joint]);
            q_max_rad[joint] = q_max_rad[joint].max(row.q_rad[joint]);
            tau_min_nm[joint] = tau_min_nm[joint].min(row.tau_nm[joint]);
            tau_max_nm[joint] = tau_max_nm[joint].max(row.tau_nm[joint]);
            dq_abs_values[joint].push(row.dq_rad_s[joint].abs());
        }
    }

    let mut dq_abs_p95_rad_s = [0.0; JOINT_COUNT];
    for joint in 0..JOINT_COUNT {
        dq_abs_values[joint].sort_by(|left, right| {
            left.partial_cmp(right).expect("non-finite velocities are rejected")
        });
        dq_abs_p95_rad_s[joint] = percentile_from_sorted(&dq_abs_values[joint], 0.95);
    }

    TrainingRange {
        q_min_rad,
        q_max_rad,
        dq_abs_p95_rad_s,
        tau_min_nm,
        tau_max_nm,
        waypoint_count: waypoint_ids.len(),
        segment_count: segment_ids.len(),
    }
}

fn current_unix_ms() -> Result<u64> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system time is before UNIX_EPOCH")?;
    Ok(duration.as_millis() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gravity::{
        artifact::{PassDirection, QuasiStaticSampleRow, SamplesHeader},
        model::{TRIG_V1_FEATURE_COUNT, trig_v1_features},
    };

    #[test]
    fn fitter_recovers_known_trig_v1_coefficients_from_synthetic_samples() {
        let mut truth = vec![vec![0.0; TRIG_V1_FEATURE_COUNT]; 6];
        truth[0][0] = 0.5;
        truth[0][1] = 2.0;
        truth[1][3] = -1.25;
        truth[2][22] = 0.75;

        let rows = synthetic_rows_from_coefficients(&truth, 600);
        let options = FitOptions {
            ridge_lambda: 1e-8,
            holdout_ratio: 0.2,
            regularize_bias: false,
        };

        let fitted = fit_from_rows(sample_header_for_tests(), rows, options).unwrap();
        assert_eq!(fitted.model.coefficients_nm.len(), 6);
        assert!((fitted.model.coefficients_nm[0][1] - 2.0).abs() < 1e-6);
        assert!((fitted.model.coefficients_nm[1][3] + 1.25).abs() < 1e-6);
        assert!(fitted.fit_quality.condition_number.is_finite());
        assert!(!fitted.fit.holdout_group_ids.is_empty());
    }

    #[test]
    fn fitter_rejects_fewer_than_ten_training_waypoints_per_feature() {
        let truth = vec![vec![0.0; TRIG_V1_FEATURE_COUNT]; 6];
        let required_waypoints = TRIG_V1_FEATURE_COUNT * 10;
        let rows = synthetic_rows_from_coefficients(&truth, required_waypoints - 1);

        let err = fit_from_rows(sample_header_for_tests(), rows, no_holdout_options()).unwrap_err();

        let message = err.to_string();
        assert!(message.contains("training waypoints"), "{err:#}");
        assert!(message.contains(&required_waypoints.to_string()), "{err:#}");
    }

    #[test]
    fn fitter_rejects_single_training_group_without_holdout() {
        let truth = vec![vec![0.0; TRIG_V1_FEATURE_COUNT]; 6];
        let mut rows = synthetic_rows_from_coefficients(&truth, 120);
        force_single_segment(&mut rows);

        let err = fit_from_rows(sample_header_for_tests(), rows, no_holdout_options()).unwrap_err();

        assert!(err.to_string().contains("training groups"), "{err:#}");
    }

    #[test]
    fn fitter_rejects_requested_holdout_when_only_one_group_exists() {
        let truth = vec![vec![0.0; TRIG_V1_FEATURE_COUNT]; 6];
        let mut rows = synthetic_rows_from_coefficients(&truth, 120);
        force_single_segment(&mut rows);
        let options = FitOptions {
            holdout_ratio: 0.2,
            ..no_holdout_options()
        };

        let err = fit_from_rows(sample_header_for_tests(), rows, options).unwrap_err();

        assert!(err.to_string().contains("holdout"), "{err:#}");
    }

    #[test]
    fn run_refuses_existing_output_before_loading_samples() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("model.toml");
        std::fs::write(&out, "existing").unwrap();
        let missing_samples = dir.path().join("missing.samples.jsonl");
        let args = GravityFitArgs {
            samples: vec![missing_samples],
            out,
            basis: Some(crate::gravity::BASIS_TRIG_V1.to_string()),
            ridge_lambda: 1e-4,
            holdout_ratio: 0.2,
        };

        let err = run(args).unwrap_err();

        assert!(err.to_string().contains("already exists"), "{err:#}");
    }

    #[test]
    fn fit_run_writes_reproducible_model() {
        let dir = tempfile::tempdir().unwrap();
        let samples = dir.path().join("synthetic.samples.jsonl");
        let out = dir.path().join("synthetic.model.toml");

        let mut truth = vec![vec![0.0; TRIG_V1_FEATURE_COUNT]; 6];
        truth[0][0] = 0.5;
        truth[0][1] = 2.0;
        truth[1][3] = -1.25;
        truth[2][22] = 0.75;

        let rows = synthetic_rows_from_coefficients(&truth, 320);
        let mut header = sample_header_for_tests();
        header.source_path = samples.display().to_string();
        header.waypoint_count = rows.len();
        header.accepted_waypoint_count = rows.len();
        write_samples_jsonl(&samples, &header, &rows);

        run(GravityFitArgs {
            samples: vec![samples],
            out: out.clone(),
            basis: Some(crate::gravity::BASIS_TRIG_V1.to_string()),
            ridge_lambda: 1e-8,
            holdout_ratio: 0.25,
        })
        .unwrap();

        let model_text = std::fs::read_to_string(out).unwrap();
        let model: QuasiStaticTorqueModel = toml::from_str(&model_text).unwrap();

        assert_eq!(
            model.fit.train_group_ids,
            [
                "segment:segment-1",
                "segment:segment-10",
                "segment:segment-11",
                "segment:segment-13",
                "segment:segment-14",
                "segment:segment-15",
                "segment:segment-3",
                "segment:segment-4",
                "segment:segment-5",
                "segment:segment-7",
                "segment:segment-8",
                "segment:segment-9",
            ]
        );
        assert_eq!(
            model.fit.holdout_group_ids,
            [
                "segment:segment-0",
                "segment:segment-12",
                "segment:segment-2",
                "segment:segment-6",
            ]
        );
        assert_eq!(model.fit.solver, "cholesky");
        assert_eq!(model.fit.fallback_solver, None);
    }

    #[test]
    fn svd_fallback_records_actual_solver_used() {
        let mut g = nalgebra::DMatrix::identity(TRIG_V1_FEATURE_COUNT, TRIG_V1_FEATURE_COUNT);
        g[(0, 0)] = -1.0;
        let normal = NormalEquations {
            g,
            b: nalgebra::DMatrix::zeros(TRIG_V1_FEATURE_COUNT, 6),
        };

        let (_solution, solver, fallback_solver) = solve_normal_equations(&normal).unwrap();

        assert_eq!(solver, "svd");
        assert_eq!(fallback_solver.as_deref(), Some("svd"));
    }

    fn no_holdout_options() -> FitOptions {
        FitOptions {
            ridge_lambda: 1e-8,
            holdout_ratio: 0.0,
            regularize_bias: false,
        }
    }

    fn force_single_segment(rows: &mut [QuasiStaticSampleRow]) {
        for row in rows {
            row.segment_id = Some("single-segment".to_string());
        }
    }

    fn synthetic_rows_from_coefficients(
        coefficients_nm: &[Vec<f64>],
        sample_count: usize,
    ) -> Vec<QuasiStaticSampleRow> {
        (0..sample_count)
            .map(|sample_index| {
                let q_rad = synthetic_q(sample_index);
                let features = trig_v1_features(q_rad);
                let mut tau_nm = [0.0; 6];
                for joint in 0..6 {
                    tau_nm[joint] = coefficients_nm[joint]
                        .iter()
                        .zip(features.iter())
                        .map(|(coefficient, feature)| coefficient * feature)
                        .sum();
                }

                QuasiStaticSampleRow {
                    row_type: "quasi-static-sample".to_string(),
                    waypoint_id: sample_index as u64,
                    segment_id: Some(format!("segment-{}", sample_index / 20)),
                    pass_direction: PassDirection::Forward,
                    host_mono_us: sample_index as u64 * 10_000,
                    raw_timestamp_us: None,
                    q_rad,
                    dq_rad_s: [0.0; 6],
                    tau_nm,
                    position_valid_mask: 0x3f,
                    dynamic_valid_mask: 0x3f,
                    stable_velocity_rad_s: 0.0,
                    stable_tracking_error_rad: 0.0,
                    stable_torque_std_nm: 0.0,
                }
            })
            .collect()
    }

    fn write_samples_jsonl(
        path: &std::path::Path,
        header: &SamplesHeader,
        rows: &[QuasiStaticSampleRow],
    ) {
        let mut file = std::fs::File::create(path).unwrap();
        writeln!(file, "{}", serde_json::to_string(header).unwrap()).unwrap();
        for row in rows {
            writeln!(file, "{}", serde_json::to_string(row).unwrap()).unwrap();
        }
    }

    fn synthetic_q(sample_index: usize) -> [f64; 6] {
        let i = sample_index as f64;
        [
            ((i * 0.017) + (i * 0.003).sin()).sin() * 1.2,
            ((i * 0.023) + 0.4).cos() * 1.1,
            ((i * 0.031) + (i * 0.007).cos()).sin(),
            ((i * 0.037) + 0.8).cos() * 0.9,
            ((i * 0.041) + (i * 0.011).sin()).sin() * 1.3,
            ((i * 0.047) + 1.2).cos(),
        ]
    }

    fn sample_header_for_tests() -> SamplesHeader {
        SamplesHeader {
            row_type: "header".to_string(),
            artifact_kind: "quasi-static-samples".to_string(),
            schema_version: 1,
            source_path: "synthetic".to_string(),
            source_sha256: "synthetic".to_string(),
            role: "slave".to_string(),
            arm_id: None,
            target: "synthetic".to_string(),
            joint_map: "piper_default".to_string(),
            load_profile: "unloaded".to_string(),
            torque_convention: crate::gravity::TORQUE_CONVENTION.to_string(),
            frequency_hz: 100.0,
            max_velocity_rad_s: 0.08,
            max_step_rad: 0.02,
            settle_ms: 500,
            sample_ms: 300,
            stable_velocity_rad_s: 0.01,
            stable_tracking_error_rad: 0.03,
            stable_torque_std_nm: 0.08,
            waypoint_count: 600,
            accepted_waypoint_count: 600,
            rejected_waypoint_count: 0,
        }
    }
}
