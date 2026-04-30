use crate::{
    commands::gravity::GravityEvalArgs,
    gravity::{
        artifact::{QuasiStaticSampleRow, SamplesHeader, read_quasi_static_samples},
        model::{JOINT_COUNT, QuasiStaticTorqueModel},
    },
};
use anyhow::{Context, Result, bail};
use serde::Serialize;
use std::fs;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct GravityEvalReport {
    pub sample_count: usize,
    pub rms_residual_nm: [f64; JOINT_COUNT],
    pub p95_residual_nm: [f64; JOINT_COUNT],
    pub max_residual_nm: [f64; JOINT_COUNT],
    pub raw_torque_delta_nm: [f64; JOINT_COUNT],
    pub compensated_external_torque_delta_nm: [f64; JOINT_COUNT],
    pub training_range_violations: usize,
    pub max_range_violation_rad: f64,
}

pub fn run(args: GravityEvalArgs) -> Result<()> {
    let model_text = fs::read_to_string(&args.model)
        .with_context(|| format!("failed to read {}", args.model.display()))?;
    let model: QuasiStaticTorqueModel = toml::from_str(&model_text).with_context(|| {
        format!(
            "failed to parse gravity model TOML {}",
            args.model.display()
        )
    })?;
    model.validate_for_eval()?;

    let loaded = read_quasi_static_samples(&args.samples)?;
    validate_model_matches_samples(&model, &loaded.header)?;

    let report = evaluate_rows(&model, &loaded.rows)?;
    let json =
        serde_json::to_string_pretty(&report).context("failed to serialize eval report JSON")?;
    println!("{json}");
    Ok(())
}

fn validate_model_matches_samples(
    model: &QuasiStaticTorqueModel,
    header: &SamplesHeader,
) -> Result<()> {
    validate_metadata_field("role", &model.role, &header.role)?;
    validate_metadata_field("joint_map", &model.joint_map, &header.joint_map)?;
    validate_metadata_field("load_profile", &model.load_profile, &header.load_profile)?;
    validate_metadata_field(
        "torque_convention",
        &model.torque_convention,
        &header.torque_convention,
    )?;
    Ok(())
}

fn validate_metadata_field(name: &str, model_value: &str, samples_value: &str) -> Result<()> {
    if model_value != samples_value {
        bail!(
            "gravity model {name} {:?} does not match samples {name} {:?}",
            model_value,
            samples_value
        );
    }
    Ok(())
}

fn evaluate_rows(
    model: &QuasiStaticTorqueModel,
    rows: &[QuasiStaticSampleRow],
) -> Result<GravityEvalReport> {
    if rows.is_empty() {
        bail!("expected at least one quasi-static sample row");
    }
    model.validate_for_eval()?;
    validate_training_range(model)?;

    let mut sum_squares = [0.0; JOINT_COUNT];
    let mut max_residual_nm: [f64; JOINT_COUNT] = [0.0; JOINT_COUNT];
    let mut absolute_residuals =
        (0..JOINT_COUNT).map(|_| Vec::with_capacity(rows.len())).collect::<Vec<_>>();
    let mut raw_min_nm = [f64::INFINITY; JOINT_COUNT];
    let mut raw_max_nm = [f64::NEG_INFINITY; JOINT_COUNT];
    let mut compensated_min_nm = [f64::INFINITY; JOINT_COUNT];
    let mut compensated_max_nm = [f64::NEG_INFINITY; JOINT_COUNT];
    let mut training_range_violations = 0usize;
    let mut max_range_violation_rad = 0.0_f64;

    for (index, row) in rows.iter().enumerate() {
        validate_row_values(index, row)?;
        let model_torque_nm = model.eval(row.q_rad)?;
        let mut row_range_violation = false;

        for joint in 0..JOINT_COUNT {
            let q = row.q_rad[joint];
            let q_min = model.training_range.q_min_rad[joint];
            let q_max = model.training_range.q_max_rad[joint];
            let range_violation = if q < q_min {
                q_min - q
            } else if q > q_max {
                q - q_max
            } else {
                0.0
            };
            if range_violation > 0.0 {
                row_range_violation = true;
                max_range_violation_rad = max_range_violation_rad.max(range_violation);
            }

            let raw_torque_nm = row.tau_nm[joint];
            let residual_nm = raw_torque_nm - model_torque_nm[joint];
            if !residual_nm.is_finite() {
                bail!("row {index} residual must be finite");
            }
            let squared = residual_nm * residual_nm;
            if !squared.is_finite() {
                bail!("row {index} residual metric overflowed");
            }

            let absolute_residual = residual_nm.abs();
            sum_squares[joint] += squared;
            if !sum_squares[joint].is_finite() {
                bail!("residual metric overflowed");
            }
            max_residual_nm[joint] = max_residual_nm[joint].max(absolute_residual);
            absolute_residuals[joint].push(absolute_residual);
            raw_min_nm[joint] = raw_min_nm[joint].min(raw_torque_nm);
            raw_max_nm[joint] = raw_max_nm[joint].max(raw_torque_nm);
            compensated_min_nm[joint] = compensated_min_nm[joint].min(residual_nm);
            compensated_max_nm[joint] = compensated_max_nm[joint].max(residual_nm);
        }

        if row_range_violation {
            training_range_violations += 1;
        }
    }

    let sample_count = rows.len();
    let mut rms_residual_nm = [0.0; JOINT_COUNT];
    let mut p95_residual_nm = [0.0; JOINT_COUNT];
    let mut raw_torque_delta_nm = [0.0; JOINT_COUNT];
    let mut compensated_external_torque_delta_nm = [0.0; JOINT_COUNT];
    for joint in 0..JOINT_COUNT {
        rms_residual_nm[joint] = (sum_squares[joint] / sample_count as f64).sqrt();
        if !rms_residual_nm[joint].is_finite() {
            bail!("rms residual must be finite");
        }
        absolute_residuals[joint].sort_by(|left, right| {
            left.partial_cmp(right).expect("non-finite residuals are rejected")
        });
        p95_residual_nm[joint] = percentile_from_sorted(&absolute_residuals[joint], 0.95);
        raw_torque_delta_nm[joint] = raw_max_nm[joint] - raw_min_nm[joint];
        compensated_external_torque_delta_nm[joint] =
            compensated_max_nm[joint] - compensated_min_nm[joint];
        if !raw_torque_delta_nm[joint].is_finite()
            || !compensated_external_torque_delta_nm[joint].is_finite()
        {
            bail!("torque delta metrics must be finite");
        }
    }

    Ok(GravityEvalReport {
        sample_count,
        rms_residual_nm,
        p95_residual_nm,
        max_residual_nm,
        raw_torque_delta_nm,
        compensated_external_torque_delta_nm,
        training_range_violations,
        max_range_violation_rad,
    })
}

fn validate_training_range(model: &QuasiStaticTorqueModel) -> Result<()> {
    for joint in 0..JOINT_COUNT {
        let q_min = model.training_range.q_min_rad[joint];
        let q_max = model.training_range.q_max_rad[joint];
        if !q_min.is_finite() || !q_max.is_finite() {
            bail!("gravity model training range must be finite");
        }
        if q_min > q_max {
            bail!("gravity model training range q_min must be <= q_max");
        }
    }
    Ok(())
}

fn validate_row_values(index: usize, row: &QuasiStaticSampleRow) -> Result<()> {
    if row.q_rad.iter().any(|value| !value.is_finite()) {
        bail!("row {index} q_rad must be finite");
    }
    if row.tau_nm.iter().any(|value| !value.is_finite()) {
        bail!("row {index} tau_nm must be finite");
    }
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gravity::{
        artifact::{PassDirection, QuasiStaticSampleRow},
        model::QuasiStaticTorqueModel,
    };

    #[test]
    fn eval_reports_range_violations() {
        let model = QuasiStaticTorqueModel::for_tests_with_training_range([-0.1; 6], [0.1; 6]);
        let rows = vec![sample_row_for_tests(
            [0.2, 0.0, 0.0, 0.0, 0.0, 0.0],
            [0.0; 6],
        )];
        let report = evaluate_rows(&model, &rows).unwrap();
        assert_eq!(report.training_range_violations, 1);
        assert!(report.max_range_violation_rad > 0.09);
    }

    #[test]
    fn eval_reports_residual_metrics_for_known_model() {
        let model = QuasiStaticTorqueModel::for_tests_with_constant_output([1.0; 6]);
        let rows = vec![sample_row_for_tests([0.0; 6], [1.0; 6])];
        let report = evaluate_rows(&model, &rows).unwrap();
        assert_eq!(report.rms_residual_nm, [0.0; 6]);
    }

    #[test]
    fn eval_reports_p95_max_and_delta_metrics() {
        let model = QuasiStaticTorqueModel::for_tests_with_constant_output([1.0; 6]);
        let rows = vec![
            sample_row_for_tests([0.0; 6], [1.0; 6]),
            sample_row_for_tests([0.0; 6], [2.0; 6]),
            sample_row_for_tests([0.0; 6], [4.0; 6]),
        ];

        let report = evaluate_rows(&model, &rows).unwrap();

        assert_eq!(report.sample_count, 3);
        assert_eq!(report.p95_residual_nm, [3.0; 6]);
        assert_eq!(report.max_residual_nm, [3.0; 6]);
        assert_eq!(report.raw_torque_delta_nm, [3.0; 6]);
        assert_eq!(report.compensated_external_torque_delta_nm, [3.0; 6]);
    }

    #[test]
    fn eval_rejects_empty_rows() {
        let model = QuasiStaticTorqueModel::for_tests_with_constant_output([0.0; 6]);

        let err = evaluate_rows(&model, &[]).unwrap_err();

        assert!(err.to_string().contains("at least one"));
    }

    fn sample_row_for_tests(q_rad: [f64; 6], tau_nm: [f64; 6]) -> QuasiStaticSampleRow {
        QuasiStaticSampleRow {
            row_type: "quasi-static-sample".to_string(),
            waypoint_id: 1,
            segment_id: Some("test-segment".to_string()),
            pass_direction: PassDirection::Forward,
            host_mono_us: 1,
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
    }
}
