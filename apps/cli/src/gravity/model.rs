#![allow(dead_code)]

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

pub const TRIG_V1_FEATURE_COUNT: usize = 23;
pub const JOINT_COUNT: usize = 6;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct QuasiStaticTorqueModel {
    pub schema_version: u32,
    pub model_kind: String,
    pub basis: String,
    pub role: String,
    pub joint_map: String,
    pub load_profile: String,
    pub torque_convention: String,
    pub created_at_unix_ms: u64,
    pub sample_count: usize,
    pub frequency_hz: f64,
    pub fit: FitMetadata,
    pub training_range: TrainingRange,
    pub fit_quality: FitQuality,
    pub model: LinearModelSection,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coverage: Option<CoverageSection>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FitMetadata {
    pub ridge_lambda: f64,
    pub regularize_bias: bool,
    pub solver: String,
    pub fallback_solver: Option<String>,
    pub holdout_strategy: String,
    pub holdout_ratio: f64,
    pub train_group_ids: Vec<String>,
    pub holdout_group_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct TrainingRange {
    pub q_min_rad: [f64; JOINT_COUNT],
    pub q_max_rad: [f64; JOINT_COUNT],
    pub dq_abs_p95_rad_s: [f64; JOINT_COUNT],
    pub tau_min_nm: [f64; JOINT_COUNT],
    pub tau_max_nm: [f64; JOINT_COUNT],
    pub waypoint_count: usize,
    pub segment_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FitQuality {
    pub rms_residual_nm: [f64; JOINT_COUNT],
    pub p95_residual_nm: [f64; JOINT_COUNT],
    pub max_residual_nm: [f64; JOINT_COUNT],
    pub holdout_rms_residual_nm: [f64; JOINT_COUNT],
    pub holdout_p95_residual_nm: [f64; JOINT_COUNT],
    pub condition_number: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct LinearModelSection {
    pub feature_names: Vec<String>,
    pub coefficients_nm: Vec<Vec<f64>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CoverageSection {
    pub anchor_q_rad: Vec<[f64; JOINT_COUNT]>,
}

pub fn trig_v1_features(q: [f64; JOINT_COUNT]) -> Vec<f64> {
    let mut features = Vec::with_capacity(TRIG_V1_FEATURE_COUNT);
    features.push(1.0);
    for value in q {
        features.push(value.sin());
        features.push(value.cos());
    }
    for i in 0..(JOINT_COUNT - 1) {
        let sum = q[i] + q[i + 1];
        features.push(sum.sin());
        features.push(sum.cos());
    }
    features
}

pub fn trig_v1_feature_names() -> Vec<String> {
    let mut names = Vec::with_capacity(TRIG_V1_FEATURE_COUNT);
    names.push("bias".to_string());
    for joint in 1..=JOINT_COUNT {
        names.push(format!("sin_q{joint}"));
        names.push(format!("cos_q{joint}"));
    }
    for joint in 1..JOINT_COUNT {
        names.push(format!("sin_q{joint}_plus_q{}", joint + 1));
        names.push(format!("cos_q{joint}_plus_q{}", joint + 1));
    }
    names
}

impl QuasiStaticTorqueModel {
    pub fn validate_for_eval(&self) -> Result<()> {
        if self.schema_version != 1 {
            bail!(
                "unsupported gravity model schema_version {}",
                self.schema_version
            );
        }
        if self.model_kind != crate::gravity::MODEL_KIND {
            bail!("unsupported gravity model kind {}", self.model_kind);
        }
        if self.basis != crate::gravity::BASIS_TRIG_V1 {
            bail!("unsupported gravity basis {}", self.basis);
        }
        if self.torque_convention != crate::gravity::TORQUE_CONVENTION {
            bail!("unsupported torque convention {}", self.torque_convention);
        }
        if self.model.coefficients_nm.len() != JOINT_COUNT {
            bail!("expected {JOINT_COUNT} output coefficient rows");
        }
        for row in &self.model.coefficients_nm {
            if row.len() != TRIG_V1_FEATURE_COUNT {
                bail!("expected {TRIG_V1_FEATURE_COUNT} coefficients per joint");
            }
        }
        Ok(())
    }

    pub fn eval(&self, q: [f64; JOINT_COUNT]) -> Result<[f64; JOINT_COUNT]> {
        self.validate_for_eval()?;
        let features = trig_v1_features(q);
        let mut out = [0.0; JOINT_COUNT];
        for (joint, row) in self.model.coefficients_nm.iter().enumerate() {
            out[joint] = row
                .iter()
                .zip(features.iter())
                .map(|(coefficient, feature)| coefficient * feature)
                .sum();
        }
        Ok(out)
    }
}

#[cfg(test)]
impl QuasiStaticTorqueModel {
    fn for_tests_with_constant_output(output_nm: [f64; JOINT_COUNT]) -> Self {
        let mut coefficients_nm = vec![vec![0.0; TRIG_V1_FEATURE_COUNT]; JOINT_COUNT];
        for (joint, output) in output_nm.into_iter().enumerate() {
            coefficients_nm[joint][0] = output;
        }

        Self {
            schema_version: 1,
            model_kind: crate::gravity::MODEL_KIND.to_string(),
            basis: crate::gravity::BASIS_TRIG_V1.to_string(),
            role: "gravity_compensation".to_string(),
            joint_map: "piper_default".to_string(),
            load_profile: "unloaded".to_string(),
            torque_convention: crate::gravity::TORQUE_CONVENTION.to_string(),
            created_at_unix_ms: 0,
            sample_count: 1,
            frequency_hz: 100.0,
            fit: FitMetadata {
                ridge_lambda: 1e-4,
                regularize_bias: false,
                solver: "test".to_string(),
                fallback_solver: None,
                holdout_strategy: "none".to_string(),
                holdout_ratio: 0.0,
                train_group_ids: Vec::new(),
                holdout_group_ids: Vec::new(),
            },
            training_range: TrainingRange {
                q_min_rad: [0.0; JOINT_COUNT],
                q_max_rad: [0.0; JOINT_COUNT],
                dq_abs_p95_rad_s: [0.0; JOINT_COUNT],
                tau_min_nm: output_nm,
                tau_max_nm: output_nm,
                waypoint_count: 1,
                segment_count: 1,
            },
            fit_quality: FitQuality {
                rms_residual_nm: [0.0; JOINT_COUNT],
                p95_residual_nm: [0.0; JOINT_COUNT],
                max_residual_nm: [0.0; JOINT_COUNT],
                holdout_rms_residual_nm: [0.0; JOINT_COUNT],
                holdout_p95_residual_nm: [0.0; JOINT_COUNT],
                condition_number: 1.0,
            },
            model: LinearModelSection {
                feature_names: trig_v1_feature_names(),
                coefficients_nm,
            },
            coverage: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trig_v1_feature_vector_has_expected_order() {
        let q = [0.0, std::f64::consts::FRAC_PI_2, 0.0, 0.0, 0.0, 0.0];
        let features = trig_v1_features(q);
        assert_eq!(features.len(), TRIG_V1_FEATURE_COUNT);
        assert_eq!(features[0], 1.0);
        assert!((features[1] - 0.0).abs() < 1e-12); // sin(q1)
        assert!((features[2] - 1.0).abs() < 1e-12); // cos(q1)
        assert!((features[3] - 1.0).abs() < 1e-12); // sin(q2)
        assert!(features.iter().all(|value| value.is_finite()));
    }

    #[test]
    fn model_toml_round_trip_preserves_coefficients_and_fit_metadata() {
        let model =
            QuasiStaticTorqueModel::for_tests_with_constant_output([1.0, 2.0, 3.0, 4.0, 5.0, 6.0]);
        let toml_text = toml::to_string(&model).expect("model serializes");
        let decoded: QuasiStaticTorqueModel =
            toml::from_str(&toml_text).expect("model deserializes");
        assert_eq!(decoded.model.coefficients_nm, model.model.coefficients_nm);
        assert_eq!(decoded.fit.ridge_lambda, 1e-4);
        assert_eq!(decoded.torque_convention, crate::gravity::TORQUE_CONVENTION);
    }

    #[test]
    fn eval_constant_bias_model_returns_bias_for_each_joint() {
        let model = QuasiStaticTorqueModel::for_tests_with_constant_output([
            1.0, -2.0, 0.5, 0.0, 3.0, -4.0,
        ]);
        assert_eq!(
            model.eval([0.3; 6]).unwrap(),
            [1.0, -2.0, 0.5, 0.0, 3.0, -4.0]
        );
    }
}
