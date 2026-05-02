#![allow(dead_code)]

use serde::Serialize;

use crate::gravity::{
    eval::GravityEvalReport,
    model::{JOINT_COUNT, QuasiStaticTorqueModel},
    profile::config::StrictGateConfig,
};

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct AssessmentReport {
    pub train: MetricsSection,
    pub validation: ValidationMetricsSection,
    pub fit_internal_holdout: HoldoutMetricsSection,
    pub model: Option<ModelMetricsSection>,
    pub derived: DerivedMetrics,
    pub decision: AssessmentDecision,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct MetricsSection {
    pub sample_count: usize,
    pub waypoint_count: usize,
    #[serde(rename = "rms_residual_nm")]
    pub residual_rms_nm: Option<[f64; JOINT_COUNT]>,
    #[serde(rename = "p95_residual_nm")]
    pub residual_p95_nm: Option<[f64; JOINT_COUNT]>,
    #[serde(rename = "max_residual_nm")]
    pub residual_max_nm: Option<[f64; JOINT_COUNT]>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ValidationMetricsSection {
    pub sample_count: usize,
    pub waypoint_count: usize,
    #[serde(rename = "rms_residual_nm")]
    pub residual_rms_nm: Option<[f64; JOINT_COUNT]>,
    #[serde(rename = "p95_residual_nm")]
    pub residual_p95_nm: Option<[f64; JOINT_COUNT]>,
    #[serde(rename = "max_residual_nm")]
    pub residual_max_nm: Option<[f64; JOINT_COUNT]>,
    pub raw_torque_delta_nm: Option<[f64; JOINT_COUNT]>,
    pub compensated_external_torque_delta_nm: Option<[f64; JOINT_COUNT]>,
    pub training_range_violations: Option<usize>,
    pub max_range_violation_rad: Option<f64>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct HoldoutMetricsSection {
    pub available: bool,
    pub sample_count: Option<usize>,
    #[serde(rename = "rms_residual_nm")]
    pub residual_rms_nm: Option<[f64; JOINT_COUNT]>,
    #[serde(rename = "p95_residual_nm")]
    pub residual_p95_nm: Option<[f64; JOINT_COUNT]>,
    #[serde(rename = "max_residual_nm")]
    pub residual_max_nm: Option<[f64; JOINT_COUNT]>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ModelMetricsSection {
    pub sample_count: usize,
    pub waypoint_count: usize,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct DerivedMetrics {
    pub validation_train_rms_ratio: [Option<f64>; JOINT_COUNT],
    pub validation_train_p95_ratio: [Option<f64>; JOINT_COUNT],
    pub compensated_delta_ratio: [Option<f64>; JOINT_COUNT],
    pub compensated_delta_ratio_meaningful: [bool; JOINT_COUNT],
    pub meaningful_compensated_delta_joint_count: usize,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct AssessmentDecision {
    pub pass: bool,
    pub grade: AssessmentGrade,
    pub failed_checks: Vec<AssessmentCheck>,
    pub skipped_checks: Vec<AssessmentCheck>,
    pub next_action: String,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AssessmentGrade {
    Good,
    Usable,
    Risky,
    Bad,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct AssessmentCheck {
    pub check: String,
    pub joint: Option<usize>,
    pub value: Option<f64>,
    pub threshold: Option<f64>,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AssessmentCounts {
    pub train_samples: usize,
    pub train_waypoints: usize,
    pub validation_samples: usize,
    pub validation_waypoints: usize,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct DiagnosticHoldoutMetrics {
    pub available: bool,
    pub sample_count: Option<usize>,
    pub rms_residual_nm: Option<[f64; JOINT_COUNT]>,
    pub p95_residual_nm: Option<[f64; JOINT_COUNT]>,
    pub max_residual_nm: Option<[f64; JOINT_COUNT]>,
}

impl DiagnosticHoldoutMetrics {
    pub fn unavailable() -> Self {
        Self {
            available: false,
            sample_count: None,
            rms_residual_nm: None,
            p95_residual_nm: None,
            max_residual_nm: None,
        }
    }
}

pub fn build_assessment_report(
    gate: &StrictGateConfig,
    counts: AssessmentCounts,
    train_eval: &GravityEvalReport,
    validation_eval: &GravityEvalReport,
    diagnostic_holdout: &DiagnosticHoldoutMetrics,
    model: &QuasiStaticTorqueModel,
) -> AssessmentReport {
    let derived = derive_metrics(gate, train_eval, validation_eval);
    let mut report = AssessmentReport {
        train: MetricsSection {
            sample_count: counts.train_samples,
            waypoint_count: counts.train_waypoints,
            residual_rms_nm: Some(train_eval.rms_residual_nm),
            residual_p95_nm: Some(train_eval.p95_residual_nm),
            residual_max_nm: Some(train_eval.max_residual_nm),
        },
        validation: ValidationMetricsSection {
            sample_count: counts.validation_samples,
            waypoint_count: counts.validation_waypoints,
            residual_rms_nm: Some(validation_eval.rms_residual_nm),
            residual_p95_nm: Some(validation_eval.p95_residual_nm),
            residual_max_nm: Some(validation_eval.max_residual_nm),
            raw_torque_delta_nm: Some(validation_eval.raw_torque_delta_nm),
            compensated_external_torque_delta_nm: Some(
                validation_eval.compensated_external_torque_delta_nm,
            ),
            training_range_violations: Some(validation_eval.training_range_violations),
            max_range_violation_rad: Some(validation_eval.max_range_violation_rad),
        },
        fit_internal_holdout: HoldoutMetricsSection {
            available: diagnostic_holdout.available,
            sample_count: diagnostic_holdout.sample_count,
            residual_rms_nm: diagnostic_holdout.rms_residual_nm,
            residual_p95_nm: diagnostic_holdout.p95_residual_nm,
            residual_max_nm: diagnostic_holdout.max_residual_nm,
        },
        model: Some(ModelMetricsSection {
            sample_count: model.sample_count,
            waypoint_count: model.training_range.waypoint_count,
        }),
        derived,
        decision: undecided(),
    };
    report.decision = decide_strict_v1(gate, &report);
    report
}

pub fn build_count_only_assessment_report(
    gate: &StrictGateConfig,
    counts: AssessmentCounts,
    reason: &str,
) -> AssessmentReport {
    let mut report = AssessmentReport {
        train: MetricsSection {
            sample_count: counts.train_samples,
            waypoint_count: counts.train_waypoints,
            residual_rms_nm: None,
            residual_p95_nm: None,
            residual_max_nm: None,
        },
        validation: ValidationMetricsSection {
            sample_count: counts.validation_samples,
            waypoint_count: counts.validation_waypoints,
            residual_rms_nm: None,
            residual_p95_nm: None,
            residual_max_nm: None,
            raw_torque_delta_nm: None,
            compensated_external_torque_delta_nm: None,
            training_range_violations: None,
            max_range_violation_rad: None,
        },
        fit_internal_holdout: HoldoutMetricsSection {
            available: false,
            sample_count: None,
            residual_rms_nm: None,
            residual_p95_nm: None,
            residual_max_nm: None,
        },
        model: None,
        derived: DerivedMetrics {
            validation_train_rms_ratio: [None; JOINT_COUNT],
            validation_train_p95_ratio: [None; JOINT_COUNT],
            compensated_delta_ratio: [None; JOINT_COUNT],
            compensated_delta_ratio_meaningful: [false; JOINT_COUNT],
            meaningful_compensated_delta_joint_count: 0,
        },
        decision: undecided(),
    };
    report.decision = decide_strict_v1(gate, &report);
    report.decision.pass = false;
    report.decision.grade = AssessmentGrade::Bad;
    report.decision.failed_checks.push(reason_check("count_only_report", reason));
    report.decision.next_action = reason.to_string();
    report
}

pub fn decide_strict_v1(gate: &StrictGateConfig, report: &AssessmentReport) -> AssessmentDecision {
    let mut failed_checks = Vec::new();
    let mut skipped_checks = Vec::new();

    check_min_count(
        &mut failed_checks,
        "train_sample_count",
        report.train.sample_count,
        gate.min_train_samples,
    );
    check_min_count(
        &mut failed_checks,
        "validation_sample_count",
        report.validation.sample_count,
        gate.min_validation_samples,
    );
    check_min_count(
        &mut failed_checks,
        "train_waypoint_count",
        report.train.waypoint_count,
        gate.min_train_waypoints,
    );
    check_min_count(
        &mut failed_checks,
        "validation_waypoint_count",
        report.validation.waypoint_count,
        gate.min_validation_waypoints,
    );

    check_array_max(
        &mut failed_checks,
        &mut skipped_checks,
        "validation_rms_residual_nm",
        report.validation.residual_rms_nm,
        gate.max_validation_rms_residual_nm,
    );
    check_array_max(
        &mut failed_checks,
        &mut skipped_checks,
        "validation_p95_residual_nm",
        report.validation.residual_p95_nm,
        gate.max_validation_p95_residual_nm,
    );
    check_optional_ratio_max(
        &mut failed_checks,
        &mut skipped_checks,
        "validation_train_rms_ratio",
        report.derived.validation_train_rms_ratio,
        gate.max_validation_train_rms_ratio,
    );
    check_optional_ratio_max(
        &mut failed_checks,
        &mut skipped_checks,
        "validation_train_p95_ratio",
        report.derived.validation_train_p95_ratio,
        gate.max_validation_train_p95_ratio,
    );

    if report.derived.meaningful_compensated_delta_joint_count == 0 {
        skipped_checks.push(skip_check(
            "compensated_delta_ratio",
            "no validation joint had a meaningful raw torque delta",
        ));
    } else {
        check_optional_ratio_max(
            &mut failed_checks,
            &mut skipped_checks,
            "compensated_delta_ratio",
            report.derived.compensated_delta_ratio,
            gate.max_compensated_delta_ratio,
        );
    }

    match report.validation.training_range_violations {
        Some(violations) if violations > gate.max_training_range_violations => {
            failed_checks.push(AssessmentCheck {
                check: "training_range_violations".to_string(),
                joint: None,
                value: Some(violations as f64),
                threshold: Some(gate.max_training_range_violations as f64),
                message: Some(format!(
                    "{violations} validation rows exceed training range, max allowed {}",
                    gate.max_training_range_violations
                )),
            });
        },
        Some(_) => {},
        None => skipped_checks.push(skip_check(
            "training_range_violations",
            "validation range metrics are unavailable",
        )),
    }

    let insufficient_data = failed_checks.iter().any(|check| {
        matches!(
            check.check.as_str(),
            "train_sample_count"
                | "validation_sample_count"
                | "train_waypoint_count"
                | "validation_waypoint_count"
        )
    });
    let range_failed = failed_checks.iter().any(|check| check.check == "training_range_violations");
    let large_residual = failed_checks.iter().any(|check| {
        matches!(
            check.check.as_str(),
            "validation_rms_residual_nm" | "validation_p95_residual_nm"
        )
    });

    let grade = if failed_checks.is_empty() {
        if passes_good_margin(gate, report) {
            AssessmentGrade::Good
        } else {
            AssessmentGrade::Usable
        }
    } else if insufficient_data || range_failed || large_residual {
        AssessmentGrade::Bad
    } else {
        AssessmentGrade::Risky
    };

    AssessmentDecision {
        pass: matches!(grade, AssessmentGrade::Good | AssessmentGrade::Usable),
        grade,
        failed_checks,
        skipped_checks,
        next_action: next_action_for_grade(grade).to_string(),
    }
}

fn derive_metrics(
    gate: &StrictGateConfig,
    train_eval: &GravityEvalReport,
    validation_eval: &GravityEvalReport,
) -> DerivedMetrics {
    let mut rms_ratio = [None; JOINT_COUNT];
    let mut p95_ratio = [None; JOINT_COUNT];
    let mut compensated_delta_ratio = [None; JOINT_COUNT];
    let mut compensated_delta_ratio_meaningful = [false; JOINT_COUNT];
    let mut meaningful_compensated_delta_joint_count = 0;

    for joint in 0..JOINT_COUNT {
        rms_ratio[joint] = ratio(
            validation_eval.rms_residual_nm[joint],
            train_eval.rms_residual_nm[joint],
        );
        p95_ratio[joint] = ratio(
            validation_eval.p95_residual_nm[joint],
            train_eval.p95_residual_nm[joint],
        );

        let raw_delta = validation_eval.raw_torque_delta_nm[joint];
        if raw_delta >= gate.torque_delta_epsilon_nm {
            compensated_delta_ratio_meaningful[joint] = true;
            compensated_delta_ratio[joint] =
                Some(validation_eval.compensated_external_torque_delta_nm[joint] / raw_delta);
            meaningful_compensated_delta_joint_count += 1;
        }
    }

    DerivedMetrics {
        validation_train_rms_ratio: rms_ratio,
        validation_train_p95_ratio: p95_ratio,
        compensated_delta_ratio,
        compensated_delta_ratio_meaningful,
        meaningful_compensated_delta_joint_count,
    }
}

fn ratio(numerator: f64, denominator: f64) -> Option<f64> {
    if denominator > 0.0 {
        Some(numerator / denominator)
    } else if numerator == 0.0 {
        Some(0.0)
    } else {
        Some(f64::INFINITY)
    }
}

fn check_min_count(
    failed_checks: &mut Vec<AssessmentCheck>,
    check: &str,
    value: usize,
    minimum: usize,
) {
    if value < minimum {
        failed_checks.push(AssessmentCheck {
            check: check.to_string(),
            joint: None,
            value: Some(value as f64),
            threshold: Some(minimum as f64),
            message: Some(format!("{value} is below minimum {minimum}")),
        });
    }
}

fn check_array_max(
    failed_checks: &mut Vec<AssessmentCheck>,
    skipped_checks: &mut Vec<AssessmentCheck>,
    check: &str,
    values: Option<[f64; JOINT_COUNT]>,
    limits: [f64; JOINT_COUNT],
) {
    let Some(values) = values else {
        skipped_checks.push(skip_check(check, "metric is unavailable"));
        return;
    };

    for joint in 0..JOINT_COUNT {
        if values[joint] > limits[joint] {
            failed_checks.push(AssessmentCheck {
                check: check.to_string(),
                joint: Some(joint),
                value: Some(values[joint]),
                threshold: Some(limits[joint]),
                message: Some(format!(
                    "joint {} value {} exceeds limit {}",
                    joint + 1,
                    values[joint],
                    limits[joint]
                )),
            });
        }
    }
}

fn check_optional_ratio_max(
    failed_checks: &mut Vec<AssessmentCheck>,
    skipped_checks: &mut Vec<AssessmentCheck>,
    check: &str,
    values: [Option<f64>; JOINT_COUNT],
    limit: f64,
) {
    let mut checked_any = false;
    for (joint, value) in values.into_iter().enumerate() {
        let Some(value) = value else {
            continue;
        };
        checked_any = true;
        if value > limit {
            failed_checks.push(AssessmentCheck {
                check: check.to_string(),
                joint: Some(joint),
                value: value.is_finite().then_some(value),
                threshold: Some(limit),
                message: Some(format!(
                    "joint {} ratio {value} exceeds limit {limit}",
                    joint + 1
                )),
            });
        }
    }
    if !checked_any {
        skipped_checks.push(skip_check(check, "no meaningful ratio is available"));
    }
}

fn passes_good_margin(gate: &StrictGateConfig, report: &AssessmentReport) -> bool {
    let margin = 1.0 - gate.good_margin_fraction;
    let count_margin = 1.0 + gate.good_margin_fraction;

    if report.derived.meaningful_compensated_delta_joint_count == 0 {
        return false;
    }

    if report.train.sample_count < scaled_min(gate.min_train_samples, count_margin)
        || report.validation.sample_count < scaled_min(gate.min_validation_samples, count_margin)
        || report.train.waypoint_count < scaled_min(gate.min_train_waypoints, count_margin)
        || report.validation.waypoint_count
            < scaled_min(gate.min_validation_waypoints, count_margin)
    {
        return false;
    }

    array_within_margin(
        report.validation.residual_rms_nm,
        gate.max_validation_rms_residual_nm,
        margin,
    ) && array_within_margin(
        report.validation.residual_p95_nm,
        gate.max_validation_p95_residual_nm,
        margin,
    ) && optional_ratios_within_margin(
        report.derived.validation_train_rms_ratio,
        gate.max_validation_train_rms_ratio,
        margin,
    ) && optional_ratios_within_margin(
        report.derived.validation_train_p95_ratio,
        gate.max_validation_train_p95_ratio,
        margin,
    ) && optional_ratios_within_margin(
        report.derived.compensated_delta_ratio,
        gate.max_compensated_delta_ratio,
        margin,
    )
}

fn scaled_min(minimum: usize, margin: f64) -> usize {
    ((minimum as f64) * margin).ceil() as usize
}

fn array_within_margin(
    values: Option<[f64; JOINT_COUNT]>,
    limits: [f64; JOINT_COUNT],
    margin: f64,
) -> bool {
    let Some(values) = values else {
        return false;
    };
    (0..JOINT_COUNT).all(|joint| values[joint] <= limits[joint] * margin)
}

fn optional_ratios_within_margin(
    values: [Option<f64>; JOINT_COUNT],
    limit: f64,
    margin: f64,
) -> bool {
    values.into_iter().flatten().all(|value| value <= limit * margin)
}

fn skip_check(check: &str, message: &str) -> AssessmentCheck {
    AssessmentCheck {
        check: check.to_string(),
        joint: None,
        value: None,
        threshold: None,
        message: Some(message.to_string()),
    }
}

fn reason_check(check: &str, message: &str) -> AssessmentCheck {
    AssessmentCheck {
        check: check.to_string(),
        joint: None,
        value: None,
        threshold: None,
        message: Some(message.to_string()),
    }
}

fn next_action_for_grade(grade: AssessmentGrade) -> &'static str {
    match grade {
        AssessmentGrade::Good => "ready_to_use",
        AssessmentGrade::Usable => "ready_to_use_with_caution",
        AssessmentGrade::Risky => "collect_more_validation_or_refit",
        AssessmentGrade::Bad => "collect_more_training_and_validation",
    }
}

fn undecided() -> AssessmentDecision {
    AssessmentDecision {
        pass: false,
        grade: AssessmentGrade::Bad,
        failed_checks: Vec::new(),
        skipped_checks: Vec::new(),
        next_action: String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gravity::{
        eval::GravityEvalReport,
        model::{JOINT_COUNT, QuasiStaticTorqueModel},
        profile::config::StrictGateConfig,
    };

    #[test]
    fn strict_gate_passes_usable_report() {
        let gate = StrictGateConfig::default();
        let report = assessment_report_for_tests()
            .with_train_counts(400, 200)
            .with_validation_counts(100, 50)
            .with_validation_p95([0.1; 6])
            .with_validation_rms([0.1; 6])
            .with_compensated_delta_ratio([Some(0.2); 6]);

        let decision = decide_strict_v1(&gate, &report);

        assert!(decision.pass);
        assert!(matches!(
            decision.grade,
            AssessmentGrade::Good | AssessmentGrade::Usable
        ));
        assert!(decision.failed_checks.is_empty());
    }

    #[test]
    fn compensated_delta_all_near_zero_is_skipped_not_failed() {
        let gate = StrictGateConfig::default();
        let report = assessment_report_for_tests()
            .with_meaningful_compensated_delta_count(0)
            .with_compensated_delta_ratio([None; 6]);

        let decision = decide_strict_v1(&gate, &report);

        assert!(
            decision
                .skipped_checks
                .iter()
                .any(|check| check.check == "compensated_delta_ratio")
        );
        assert!(
            !decision
                .failed_checks
                .iter()
                .any(|check| check.check == "compensated_delta_ratio")
        );
    }

    #[test]
    fn insufficient_counts_grade_bad() {
        let gate = StrictGateConfig::default();
        let report = assessment_report_for_tests().with_train_counts(1, 1);

        let decision = decide_strict_v1(&gate, &report);

        assert!(!decision.pass);
        assert_eq!(decision.grade, AssessmentGrade::Bad);
    }

    #[test]
    fn report_serializes_holdout_availability_range_and_skipped_checks() {
        let gate = StrictGateConfig::default();
        let train_eval = eval_report_for_tests();
        let validation_eval = eval_report_for_tests()
            .with_training_range_violations(2)
            .with_raw_torque_delta([0.0; 6]);
        let holdout = DiagnosticHoldoutMetrics::unavailable();

        let report = build_assessment_report(
            &gate,
            assessment_counts_for_tests(),
            &train_eval,
            &validation_eval,
            &holdout,
            &QuasiStaticTorqueModel::for_tests_with_constant_output([0.0; 6]),
        );
        let json = serde_json::to_value(&report).unwrap();

        assert_eq!(json["fit_internal_holdout"]["available"], false);
        assert_eq!(json["validation"]["training_range_violations"], 2);
        assert_eq!(
            json["derived"]["meaningful_compensated_delta_joint_count"],
            0
        );
        assert_eq!(
            json["derived"]["compensated_delta_ratio_meaningful"],
            serde_json::json!([false, false, false, false, false, false])
        );
        assert!(
            json["decision"]["skipped_checks"]
                .as_array()
                .unwrap()
                .iter()
                .any(|check| { check["check"] == "compensated_delta_ratio" })
        );
    }

    #[test]
    fn count_only_insufficient_data_report_serializes_without_model_or_eval() {
        let gate = StrictGateConfig::default();

        let report = build_count_only_assessment_report(
            &gate,
            AssessmentCounts {
                train_samples: 3,
                train_waypoints: 1,
                validation_samples: 0,
                validation_waypoints: 0,
            },
            "validation samples below minimum",
        );
        let json = serde_json::to_value(&report).unwrap();

        assert_eq!(json["decision"]["grade"], "bad");
        assert_eq!(json["decision"]["pass"], false);
        assert_eq!(json["train"]["sample_count"], 3);
        assert!(json["validation"]["residual_p95_nm"].is_null());
        assert!(json["model"].is_null());
    }

    #[test]
    fn report_serializes_spec_metric_field_names() {
        let gate = StrictGateConfig::default();
        let train_eval = eval_report_for_tests();
        let validation_eval = eval_report_for_tests();
        let holdout = DiagnosticHoldoutMetrics {
            available: true,
            sample_count: Some(25),
            rms_residual_nm: Some([0.2; JOINT_COUNT]),
            p95_residual_nm: Some([0.3; JOINT_COUNT]),
            max_residual_nm: Some([0.4; JOINT_COUNT]),
        };

        let report = build_assessment_report(
            &gate,
            assessment_counts_for_tests(),
            &train_eval,
            &validation_eval,
            &holdout,
            &QuasiStaticTorqueModel::for_tests_with_constant_output([0.0; 6]),
        );
        let json = serde_json::to_value(&report).unwrap();

        assert_eq!(
            json["train"]["rms_residual_nm"],
            serde_json::json!([0.1, 0.1, 0.1, 0.1, 0.1, 0.1])
        );
        assert!(json["train"].get("residual_rms_nm").is_none());
        assert_eq!(
            json["validation"]["p95_residual_nm"],
            serde_json::json!([0.1, 0.1, 0.1, 0.1, 0.1, 0.1])
        );
        assert!(json["validation"].get("residual_p95_nm").is_none());
        assert_eq!(
            json["fit_internal_holdout"]["max_residual_nm"],
            serde_json::json!([0.4, 0.4, 0.4, 0.4, 0.4, 0.4])
        );
        assert!(json["fit_internal_holdout"].get("residual_max_nm").is_none());
    }

    #[test]
    fn count_only_sufficient_counts_still_fails_with_reason() {
        let gate = StrictGateConfig::default();
        let report = build_count_only_assessment_report(
            &gate,
            assessment_counts_for_tests(),
            "model has not been fitted",
        );
        let json = serde_json::to_value(&report).unwrap();

        assert_eq!(json["decision"]["pass"], false);
        assert_eq!(json["decision"]["grade"], "bad");
        assert_eq!(json["decision"]["next_action"], "model has not been fitted");
        assert!(
            json["decision"]["failed_checks"]
                .as_array()
                .unwrap()
                .iter()
                .any(|check| check["check"] == "count_only_report"
                    && check["message"] == "model has not been fitted")
        );
    }

    #[test]
    fn zero_train_residual_positive_validation_residual_fails_ratio_check() {
        let gate = StrictGateConfig::default();
        let train_eval = eval_report_for_tests().with_rms([0.0; JOINT_COUNT]);
        let validation_eval = eval_report_for_tests().with_rms([0.1; JOINT_COUNT]);

        let report = build_assessment_report(
            &gate,
            assessment_counts_for_tests(),
            &train_eval,
            &validation_eval,
            &DiagnosticHoldoutMetrics::unavailable(),
            &QuasiStaticTorqueModel::for_tests_with_constant_output([0.0; 6]),
        );
        let json = serde_json::to_value(&report).unwrap();

        assert_eq!(json["decision"]["pass"], false);
        assert!(
            json["decision"]["failed_checks"]
                .as_array()
                .unwrap()
                .iter()
                .any(|check| check["check"] == "validation_train_rms_ratio"
                    && check["threshold"] == gate.max_validation_train_rms_ratio)
        );
    }

    #[test]
    fn all_compensated_delta_skipped_cannot_grade_good() {
        let gate = StrictGateConfig::default();
        let report = assessment_report_for_tests()
            .with_meaningful_compensated_delta_count(0)
            .with_compensated_delta_ratio([None; JOINT_COUNT]);

        let decision = decide_strict_v1(&gate, &report);

        assert!(decision.pass);
        assert_eq!(decision.grade, AssessmentGrade::Usable);
        assert_eq!(decision.next_action, "ready_to_use_with_caution");
    }

    #[test]
    fn build_report_uses_explicit_validation_waypoint_count_for_gate() {
        let gate = StrictGateConfig::default();
        let train_eval = eval_report_for_tests();
        let validation_eval = eval_report_for_tests();
        let counts = AssessmentCounts {
            validation_samples: validation_eval.sample_count,
            validation_waypoints: gate.min_validation_waypoints - 1,
            ..assessment_counts_for_tests()
        };

        let report = build_assessment_report(
            &gate,
            counts,
            &train_eval,
            &validation_eval,
            &DiagnosticHoldoutMetrics::unavailable(),
            &QuasiStaticTorqueModel::for_tests_with_constant_output([0.0; 6]),
        );
        let json = serde_json::to_value(&report).unwrap();

        assert_eq!(
            report.validation.sample_count, validation_eval.sample_count,
            "test setup requires validation sample count to remain sufficient"
        );
        assert_eq!(
            report.validation.waypoint_count,
            gate.min_validation_waypoints - 1
        );
        assert_eq!(report.decision.grade, AssessmentGrade::Bad);
        let failed_check = json["decision"]["failed_checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|check| check["check"] == "validation_waypoint_count")
            .expect("validation waypoint count should fail");
        assert_eq!(
            failed_check["value"],
            (gate.min_validation_waypoints - 1) as f64
        );
        assert_eq!(
            failed_check["threshold"],
            gate.min_validation_waypoints as f64
        );
    }

    #[test]
    fn good_margin_count_threshold_uses_one_plus_fraction() {
        let gate = StrictGateConfig {
            min_validation_samples: 80,
            ..StrictGateConfig::default()
        };

        let good_report = assessment_report_for_tests().with_validation_counts(100, 50);
        let usable_report = assessment_report_for_tests().with_validation_counts(99, 50);

        assert_eq!(
            decide_strict_v1(&gate, &good_report).grade,
            AssessmentGrade::Good
        );
        assert_eq!(
            decide_strict_v1(&gate, &usable_report).grade,
            AssessmentGrade::Usable
        );
    }

    fn assessment_report_for_tests() -> AssessmentReport {
        let gate = StrictGateConfig::default();
        let train_eval = eval_report_for_tests();
        let validation_eval = eval_report_for_tests();
        build_assessment_report(
            &gate,
            assessment_counts_for_tests(),
            &train_eval,
            &validation_eval,
            &DiagnosticHoldoutMetrics::unavailable(),
            &QuasiStaticTorqueModel::for_tests_with_constant_output([0.0; 6]),
        )
    }

    fn assessment_counts_for_tests() -> AssessmentCounts {
        AssessmentCounts {
            train_samples: 400,
            train_waypoints: 200,
            validation_samples: 100,
            validation_waypoints: 50,
        }
    }

    fn eval_report_for_tests() -> GravityEvalReport {
        GravityEvalReport {
            sample_count: 400,
            rms_residual_nm: [0.1; JOINT_COUNT],
            p95_residual_nm: [0.1; JOINT_COUNT],
            max_residual_nm: [0.2; JOINT_COUNT],
            raw_torque_delta_nm: [1.0; JOINT_COUNT],
            compensated_external_torque_delta_nm: [0.2; JOINT_COUNT],
            training_range_violations: 0,
            max_range_violation_rad: 0.0,
        }
    }

    trait EvalReportForTests {
        fn with_training_range_violations(self, training_range_violations: usize) -> Self;
        fn with_raw_torque_delta(self, raw_torque_delta_nm: [f64; JOINT_COUNT]) -> Self;
        fn with_rms(self, rms_residual_nm: [f64; JOINT_COUNT]) -> Self;
    }

    impl EvalReportForTests for GravityEvalReport {
        fn with_training_range_violations(mut self, training_range_violations: usize) -> Self {
            self.training_range_violations = training_range_violations;
            self
        }

        fn with_raw_torque_delta(mut self, raw_torque_delta_nm: [f64; JOINT_COUNT]) -> Self {
            self.raw_torque_delta_nm = raw_torque_delta_nm;
            self
        }

        fn with_rms(mut self, rms_residual_nm: [f64; JOINT_COUNT]) -> Self {
            self.rms_residual_nm = rms_residual_nm;
            self
        }
    }

    trait AssessmentReportForTests {
        fn with_train_counts(self, sample_count: usize, waypoint_count: usize) -> Self;
        fn with_validation_counts(self, sample_count: usize, waypoint_count: usize) -> Self;
        fn with_validation_p95(self, residual_p95_nm: [f64; JOINT_COUNT]) -> Self;
        fn with_validation_rms(self, residual_rms_nm: [f64; JOINT_COUNT]) -> Self;
        fn with_compensated_delta_ratio(
            self,
            compensated_delta_ratio: [Option<f64>; JOINT_COUNT],
        ) -> Self;
        fn with_meaningful_compensated_delta_count(self, count: usize) -> Self;
    }

    impl AssessmentReportForTests for AssessmentReport {
        fn with_train_counts(mut self, sample_count: usize, waypoint_count: usize) -> Self {
            self.train.sample_count = sample_count;
            self.train.waypoint_count = waypoint_count;
            self
        }

        fn with_validation_counts(mut self, sample_count: usize, waypoint_count: usize) -> Self {
            self.validation.sample_count = sample_count;
            self.validation.waypoint_count = waypoint_count;
            self
        }

        fn with_validation_p95(mut self, residual_p95_nm: [f64; JOINT_COUNT]) -> Self {
            self.validation.residual_p95_nm = Some(residual_p95_nm);
            self
        }

        fn with_validation_rms(mut self, residual_rms_nm: [f64; JOINT_COUNT]) -> Self {
            self.validation.residual_rms_nm = Some(residual_rms_nm);
            self
        }

        fn with_compensated_delta_ratio(
            mut self,
            compensated_delta_ratio: [Option<f64>; JOINT_COUNT],
        ) -> Self {
            self.derived.compensated_delta_ratio = compensated_delta_ratio;
            self
        }

        fn with_meaningful_compensated_delta_count(mut self, count: usize) -> Self {
            self.derived.meaningful_compensated_delta_joint_count = count;
            self
        }
    }
}
