#![allow(dead_code)]

use std::{collections::BTreeMap, fs, path::Path};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ProfileConfig {
    pub name: String,
    pub role: String,
    pub arm_id: String,
    pub target: String,
    pub joint_map: String,
    pub load_profile: String,
    #[serde(default = "default_torque_convention")]
    pub torque_convention: String,
    #[serde(default = "default_basis")]
    pub basis: String,
    #[serde(default)]
    pub replay: ReplayConfig,
    #[serde(default)]
    pub fit: FitConfig,
    #[serde(default)]
    pub gate: GateConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ReplayConfig {
    pub sample_ms: u64,
    pub settle_ms: u64,
    pub max_step_rad: f64,
    pub max_velocity_rad_s: f64,
    pub bidirectional: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FitConfig {
    pub ridge_lambda: f64,
    pub holdout_ratio: f64,
    pub holdout_group_key: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct GateConfig {
    pub strict_v1: StrictGateConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct StrictGateConfig {
    pub min_train_samples: usize,
    pub min_validation_samples: usize,
    pub min_train_waypoints: usize,
    pub min_validation_waypoints: usize,
    pub max_validation_p95_residual_nm: [f64; 6],
    pub max_validation_rms_residual_nm: [f64; 6],
    pub max_validation_train_p95_ratio: f64,
    pub max_validation_train_rms_ratio: f64,
    pub max_compensated_delta_ratio: f64,
    pub max_training_range_violations: usize,
    pub good_margin_fraction: f64,
    pub torque_delta_epsilon_nm: f64,
}

impl Default for ReplayConfig {
    fn default() -> Self {
        Self {
            sample_ms: 300,
            settle_ms: 500,
            max_step_rad: 0.02,
            max_velocity_rad_s: 0.08,
            bidirectional: true,
        }
    }
}

impl Default for FitConfig {
    fn default() -> Self {
        Self {
            ridge_lambda: 1e-4,
            holdout_ratio: 0.2,
            holdout_group_key: "source_path_id".to_string(),
        }
    }
}

impl Default for StrictGateConfig {
    fn default() -> Self {
        Self {
            min_train_samples: 300,
            min_validation_samples: 80,
            min_train_waypoints: 150,
            min_validation_waypoints: 40,
            max_validation_p95_residual_nm: [0.8, 1.2, 1.2, 0.8, 0.6, 0.4],
            max_validation_rms_residual_nm: [0.4, 0.7, 0.7, 0.4, 0.3, 0.2],
            max_validation_train_p95_ratio: 2.0,
            max_validation_train_rms_ratio: 2.0,
            max_compensated_delta_ratio: 0.65,
            max_training_range_violations: 0,
            good_margin_fraction: 0.25,
            torque_delta_epsilon_nm: 0.05,
        }
    }
}

impl ProfileConfig {
    pub fn new(
        name: impl Into<String>,
        role: impl Into<String>,
        arm_id: impl Into<String>,
        target: impl Into<String>,
        joint_map: impl Into<String>,
        load_profile: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            role: role.into(),
            arm_id: arm_id.into(),
            target: target.into(),
            joint_map: joint_map.into(),
            load_profile: load_profile.into(),
            torque_convention: crate::gravity::TORQUE_CONVENTION.to_string(),
            basis: crate::gravity::BASIS_TRIG_V1.to_string(),
            replay: ReplayConfig::default(),
            fit: FitConfig::default(),
            gate: GateConfig::default(),
        }
    }

    pub fn from_toml_str(input: &str) -> Result<Self> {
        let config: Self = toml::from_str(input).context("failed to parse profile config TOML")?;
        config.validate()?;
        Ok(config)
    }

    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let input = fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        Self::from_toml_str(&input).with_context(|| format!("failed to load {}", path.display()))
    }

    pub fn save(&self, path: impl AsRef<Path>) -> Result<()> {
        self.validate()?;
        let path = path.as_ref();
        if let Some(parent) = path.parent().filter(|parent| !parent.as_os_str().is_empty()) {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        let output = toml::to_string_pretty(self).context("failed to serialize profile config")?;
        fs::write(path, output).with_context(|| format!("failed to write {}", path.display()))
    }

    pub fn validate(&self) -> Result<()> {
        validate_non_empty("name", &self.name)?;
        validate_non_empty("role", &self.role)?;
        validate_non_empty("arm_id", &self.arm_id)?;
        validate_non_empty("target", &self.target)?;
        validate_non_empty("joint_map", &self.joint_map)?;
        validate_non_empty("load_profile", &self.load_profile)?;
        validate_non_empty("torque_convention", &self.torque_convention)?;
        validate_non_empty("basis", &self.basis)?;

        if self.torque_convention != crate::gravity::TORQUE_CONVENTION {
            bail!("unsupported torque_convention {}", self.torque_convention);
        }
        if self.basis != crate::gravity::BASIS_TRIG_V1 {
            bail!("unsupported basis {}", self.basis);
        }
        validate_positive_f64("replay.max_velocity_rad_s", self.replay.max_velocity_rad_s)?;
        validate_positive_f64("replay.max_step_rad", self.replay.max_step_rad)?;
        if self.replay.settle_ms == 0 {
            bail!("replay.settle_ms must be > 0");
        }
        if self.replay.sample_ms == 0 {
            bail!("replay.sample_ms must be > 0");
        }
        validate_positive_f64("fit.ridge_lambda", self.fit.ridge_lambda)?;
        validate_ratio("fit.holdout_ratio", self.fit.holdout_ratio)?;
        validate_non_empty("fit.holdout_group_key", &self.fit.holdout_group_key)?;

        let strict = &self.gate.strict_v1;
        validate_positive_usize("gate.strict_v1.min_train_samples", strict.min_train_samples)?;
        validate_positive_usize(
            "gate.strict_v1.min_validation_samples",
            strict.min_validation_samples,
        )?;
        validate_positive_usize(
            "gate.strict_v1.min_train_waypoints",
            strict.min_train_waypoints,
        )?;
        validate_positive_usize(
            "gate.strict_v1.min_validation_waypoints",
            strict.min_validation_waypoints,
        )?;
        validate_finite_array(
            "gate.strict_v1.max_validation_p95_residual_nm",
            strict.max_validation_p95_residual_nm,
        )?;
        validate_finite_array(
            "gate.strict_v1.max_validation_rms_residual_nm",
            strict.max_validation_rms_residual_nm,
        )?;
        validate_positive_f64(
            "gate.strict_v1.max_validation_train_p95_ratio",
            strict.max_validation_train_p95_ratio,
        )?;
        validate_positive_f64(
            "gate.strict_v1.max_validation_train_rms_ratio",
            strict.max_validation_train_rms_ratio,
        )?;
        validate_ratio(
            "gate.strict_v1.max_compensated_delta_ratio",
            strict.max_compensated_delta_ratio,
        )?;
        validate_ratio(
            "gate.strict_v1.good_margin_fraction",
            strict.good_margin_fraction,
        )?;
        validate_positive_f64(
            "gate.strict_v1.torque_delta_epsilon_nm",
            strict.torque_delta_epsilon_nm,
        )?;
        Ok(())
    }

    pub fn identity_sha256(&self) -> Result<String> {
        let identity = ProfileIdentityHash {
            name: &self.name,
            role: &self.role,
            arm_id: &self.arm_id,
            joint_map: &self.joint_map,
            load_profile: &self.load_profile,
            torque_convention: &self.torque_convention,
            basis: &self.basis,
        };
        sha256_canonical_json(&identity)
    }

    pub fn config_sha256(&self) -> Result<String> {
        sha256_canonical_json(self)
    }
}

#[derive(Serialize)]
struct ProfileIdentityHash<'a> {
    name: &'a str,
    role: &'a str,
    arm_id: &'a str,
    joint_map: &'a str,
    load_profile: &'a str,
    torque_convention: &'a str,
    basis: &'a str,
}

fn default_torque_convention() -> String {
    crate::gravity::TORQUE_CONVENTION.to_string()
}

fn default_basis() -> String {
    crate::gravity::BASIS_TRIG_V1.to_string()
}

fn validate_non_empty(field: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        bail!("{field} must not be empty");
    }
    Ok(())
}

fn validate_positive_usize(field: &str, value: usize) -> Result<()> {
    if value == 0 {
        bail!("{field} must be > 0");
    }
    Ok(())
}

fn validate_positive_f64(field: &str, value: f64) -> Result<()> {
    if !value.is_finite() || value <= 0.0 {
        bail!("{field} must be finite and > 0.0");
    }
    Ok(())
}

fn validate_ratio(field: &str, value: f64) -> Result<()> {
    if !value.is_finite() || !(0.0..=1.0).contains(&value) {
        bail!("{field} must be finite and between 0.0 and 1.0");
    }
    Ok(())
}

fn validate_finite_array(field: &str, values: [f64; 6]) -> Result<()> {
    if values.iter().any(|value| !value.is_finite()) {
        bail!("{field} values must be finite");
    }
    Ok(())
}

fn sha256_canonical_json<T: Serialize>(value: &T) -> Result<String> {
    let value = serde_json::to_value(value).context("failed to convert profile config to JSON")?;
    let mut canonical = String::new();
    write_canonical_json(&value, &mut canonical)?;

    let mut hasher = Sha256::new();
    hasher.update(canonical.as_bytes());
    Ok(format!("{:x}", hasher.finalize()))
}

fn write_canonical_json(value: &Value, out: &mut String) -> Result<()> {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(value) => out.push_str(if *value { "true" } else { "false" }),
        Value::Number(value) => out.push_str(&value.to_string()),
        Value::String(value) => out.push_str(
            &serde_json::to_string(value).context("failed to serialize canonical JSON string")?,
        ),
        Value::Array(values) => {
            out.push('[');
            for (index, value) in values.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_canonical_json(value, out)?;
            }
            out.push(']');
        },
        Value::Object(values) => {
            out.push('{');
            let sorted: BTreeMap<_, _> = values.iter().collect();
            for (index, (key, value)) in sorted.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                out.push_str(
                    &serde_json::to_string(key)
                        .context("failed to serialize canonical JSON object key")?,
                );
                out.push(':');
                write_canonical_json(value, out)?;
            }
            out.push('}');
        },
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_for_tests() -> ProfileConfig {
        ProfileConfig::new(
            "slave-piper-left-normal-gripper-d405",
            "slave",
            "piper-left",
            "socketcan:can1",
            "identity",
            "normal-gripper-d405",
        )
    }

    #[test]
    fn identity_hash_ignores_target_but_config_hash_does_not() {
        let left = config_for_tests();
        let mut right = config_for_tests();
        right.target = "socketcan:can0".to_string();

        assert_eq!(
            left.identity_sha256().unwrap(),
            right.identity_sha256().unwrap()
        );
        assert_ne!(
            left.config_sha256().unwrap(),
            right.config_sha256().unwrap()
        );

        right.load_profile = "other-load".to_string();
        assert_ne!(
            left.identity_sha256().unwrap(),
            right.identity_sha256().unwrap()
        );
    }

    #[test]
    fn config_defaults_match_spec() {
        let config = config_for_tests();

        assert_eq!(config.torque_convention, crate::gravity::TORQUE_CONVENTION);
        assert_eq!(config.basis, crate::gravity::BASIS_TRIG_V1);
        assert_eq!(config.replay.max_velocity_rad_s, 0.08);
        assert_eq!(config.replay.max_step_rad, 0.02);
        assert_eq!(config.replay.settle_ms, 500);
        assert_eq!(config.replay.sample_ms, 300);
        assert_eq!(config.fit.ridge_lambda, 1e-4);
        assert_eq!(config.fit.holdout_group_key, "source_path_id");
        assert_eq!(config.gate.strict_v1.min_train_samples, 300);
        assert_eq!(config.gate.strict_v1.torque_delta_epsilon_nm, 0.05);
    }

    #[test]
    fn config_rejects_empty_identity_fields() {
        let mut config = config_for_tests();
        config.arm_id.clear();

        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("arm_id"));
    }

    #[test]
    fn profile_hashes_ignore_toml_comments_and_key_order() {
        let first = r#"
            # operator note
            name = "slave-piper-left-normal-gripper-d405"
            role = "slave"
            arm_id = "piper-left"
            target = "socketcan:can1"
            joint_map = "identity"
            load_profile = "normal-gripper-d405"
            torque_convention = "piper-sdk-normalized-nm-v1"
            basis = "trig-v1"

            [fit]
            ridge_lambda = 0.0001
            holdout_ratio = 0.2
            holdout_group_key = "source_path_id"

            [replay]
            sample_ms = 300
            settle_ms = 500
            max_step_rad = 0.02
            max_velocity_rad_s = 0.08
            bidirectional = true

            [gate.strict_v1]
            min_train_samples = 300
            min_validation_samples = 80
            min_train_waypoints = 150
            min_validation_waypoints = 40
            max_validation_p95_residual_nm = [0.8, 1.2, 1.2, 0.8, 0.6, 0.4]
            max_validation_rms_residual_nm = [0.4, 0.7, 0.7, 0.4, 0.3, 0.2]
            max_validation_train_p95_ratio = 2.0
            max_validation_train_rms_ratio = 2.0
            max_compensated_delta_ratio = 0.65
            max_training_range_violations = 0
            good_margin_fraction = 0.25
            torque_delta_epsilon_nm = 0.05
        "#;
        let second = r#"
            basis = "trig-v1"
            torque_convention = "piper-sdk-normalized-nm-v1"
            load_profile = "normal-gripper-d405"
            joint_map = "identity"
            target = "socketcan:can1"
            arm_id = "piper-left"
            role = "slave"
            name = "slave-piper-left-normal-gripper-d405"

            [gate.strict_v1]
            torque_delta_epsilon_nm = 0.05
            good_margin_fraction = 0.25
            max_training_range_violations = 0
            max_compensated_delta_ratio = 0.65
            max_validation_train_rms_ratio = 2.0
            max_validation_train_p95_ratio = 2.0
            max_validation_rms_residual_nm = [0.4, 0.7, 0.7, 0.4, 0.3, 0.2]
            max_validation_p95_residual_nm = [0.8, 1.2, 1.2, 0.8, 0.6, 0.4]
            min_validation_waypoints = 40
            min_train_waypoints = 150
            min_validation_samples = 80
            min_train_samples = 300

            [replay]
            bidirectional = true
            max_velocity_rad_s = 0.08
            max_step_rad = 0.02
            settle_ms = 500
            sample_ms = 300

            [fit]
            holdout_group_key = "source_path_id"
            holdout_ratio = 0.2
            ridge_lambda = 0.0001
        "#;

        let first = ProfileConfig::from_toml_str(first).unwrap();
        let second = ProfileConfig::from_toml_str(second).unwrap();

        assert_eq!(
            first.identity_sha256().unwrap(),
            second.identity_sha256().unwrap()
        );
        assert_eq!(
            first.config_sha256().unwrap(),
            second.config_sha256().unwrap()
        );
    }

    #[test]
    fn canonical_json_writes_arrays_once() {
        let mut canonical = String::new();
        write_canonical_json(&serde_json::json!([1, 2]), &mut canonical).unwrap();

        assert_eq!(canonical, "[1,2]");
    }
}
