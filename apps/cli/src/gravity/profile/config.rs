#![allow(dead_code)]

use std::{collections::BTreeMap, fs, path::Path};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::gravity::profile::manifest::ProfileConfigSectionHashes;

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
    #[serde(default = "default_stable_tracking_error_rad")]
    pub stable_tracking_error_rad: f64,
    pub bidirectional: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum SampleReductionMode {
    RawRows,
    BidirectionalPairMeanV1,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FitConfig {
    pub ridge_lambda: f64,
    pub holdout_ratio: f64,
    pub holdout_group_key: String,
    #[serde(default = "default_sample_reduction")]
    pub sample_reduction: SampleReductionMode,
    #[serde(default = "default_pair_q_error_max_rad")]
    pub pair_q_error_max_rad: f64,
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
    #[serde(default = "default_min_train_effective_pairs")]
    pub min_train_effective_pairs: usize,
    #[serde(default = "default_min_validation_effective_pairs")]
    pub min_validation_effective_pairs: usize,
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
            stable_tracking_error_rad: default_stable_tracking_error_rad(),
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
            sample_reduction: SampleReductionMode::RawRows,
            pair_q_error_max_rad: default_pair_q_error_max_rad(),
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
            min_train_effective_pairs: default_min_train_effective_pairs(),
            min_validation_effective_pairs: default_min_validation_effective_pairs(),
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
        let fit = FitConfig {
            sample_reduction: SampleReductionMode::BidirectionalPairMeanV1,
            ..FitConfig::default()
        };

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
            fit,
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
        validate_positive_f64(
            "replay.stable_tracking_error_rad",
            self.replay.stable_tracking_error_rad,
        )?;
        if self.replay.settle_ms == 0 {
            bail!("replay.settle_ms must be > 0");
        }
        if self.replay.sample_ms == 0 {
            bail!("replay.sample_ms must be > 0");
        }
        validate_positive_f64("fit.ridge_lambda", self.fit.ridge_lambda)?;
        validate_holdout_ratio("fit.holdout_ratio", self.fit.holdout_ratio)?;
        validate_non_empty("fit.holdout_group_key", &self.fit.holdout_group_key)?;
        validate_positive_f64("fit.pair_q_error_max_rad", self.fit.pair_q_error_max_rad)?;

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
        validate_positive_usize(
            "gate.strict_v1.min_train_effective_pairs",
            strict.min_train_effective_pairs,
        )?;
        validate_positive_usize(
            "gate.strict_v1.min_validation_effective_pairs",
            strict.min_validation_effective_pairs,
        )?;
        validate_positive_f64_array(
            "gate.strict_v1.max_validation_p95_residual_nm",
            strict.max_validation_p95_residual_nm,
        )?;
        validate_positive_f64_array(
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
        if self.is_legacy_raw_row_hash_compatible() {
            sha256_canonical_json(&legacy_profile_hash(self))
        } else {
            sha256_canonical_json(self)
        }
    }

    pub fn section_sha256(&self) -> Result<ProfileConfigSectionHashes> {
        let legacy_raw_row_hash_compatible = self.is_legacy_raw_row_hash_compatible();
        let fit = if legacy_raw_row_hash_compatible {
            sha256_canonical_json(&legacy_fit_hash(&self.fit))?
        } else {
            sha256_canonical_json(&self.fit)?
        };
        let gate_strict_v1 = if legacy_raw_row_hash_compatible {
            sha256_canonical_json(&legacy_strict_gate_hash(&self.gate.strict_v1))?
        } else {
            sha256_canonical_json(&self.gate.strict_v1)?
        };

        Ok(ProfileConfigSectionHashes {
            name: sha256_canonical_json(&self.name)?,
            target: sha256_canonical_json(&self.target)?,
            replay: sha256_canonical_json(&self.replay)?,
            fit,
            gate_strict_v1,
        })
    }

    fn is_legacy_raw_row_hash_compatible(&self) -> bool {
        self.fit.sample_reduction == SampleReductionMode::RawRows
            && self.fit.pair_q_error_max_rad == default_pair_q_error_max_rad()
            && self.gate.strict_v1.min_train_effective_pairs == default_min_train_effective_pairs()
            && self.gate.strict_v1.min_validation_effective_pairs
                == default_min_validation_effective_pairs()
    }
}

#[derive(Serialize)]
struct ProfileIdentityHash<'a> {
    role: &'a str,
    arm_id: &'a str,
    joint_map: &'a str,
    load_profile: &'a str,
    torque_convention: &'a str,
    basis: &'a str,
}

#[derive(Serialize)]
struct LegacyProfileConfigHash<'a> {
    name: &'a str,
    role: &'a str,
    arm_id: &'a str,
    target: &'a str,
    joint_map: &'a str,
    load_profile: &'a str,
    torque_convention: &'a str,
    basis: &'a str,
    replay: &'a ReplayConfig,
    fit: LegacyFitConfigHash<'a>,
    gate: LegacyGateConfigHash,
}

#[derive(Serialize)]
struct LegacyFitConfigHash<'a> {
    ridge_lambda: f64,
    holdout_ratio: f64,
    holdout_group_key: &'a str,
}

#[derive(Serialize)]
struct LegacyGateConfigHash {
    strict_v1: LegacyStrictGateConfigHash,
}

#[derive(Serialize)]
struct LegacyStrictGateConfigHash {
    min_train_samples: usize,
    min_validation_samples: usize,
    min_train_waypoints: usize,
    min_validation_waypoints: usize,
    max_validation_p95_residual_nm: [f64; 6],
    max_validation_rms_residual_nm: [f64; 6],
    max_validation_train_p95_ratio: f64,
    max_validation_train_rms_ratio: f64,
    max_compensated_delta_ratio: f64,
    max_training_range_violations: usize,
    good_margin_fraction: f64,
    torque_delta_epsilon_nm: f64,
}

fn legacy_profile_hash(config: &ProfileConfig) -> LegacyProfileConfigHash<'_> {
    LegacyProfileConfigHash {
        name: &config.name,
        role: &config.role,
        arm_id: &config.arm_id,
        target: &config.target,
        joint_map: &config.joint_map,
        load_profile: &config.load_profile,
        torque_convention: &config.torque_convention,
        basis: &config.basis,
        replay: &config.replay,
        fit: legacy_fit_hash(&config.fit),
        gate: legacy_gate_hash(&config.gate),
    }
}

fn legacy_fit_hash(fit: &FitConfig) -> LegacyFitConfigHash<'_> {
    LegacyFitConfigHash {
        ridge_lambda: fit.ridge_lambda,
        holdout_ratio: fit.holdout_ratio,
        holdout_group_key: &fit.holdout_group_key,
    }
}

fn legacy_gate_hash(gate: &GateConfig) -> LegacyGateConfigHash {
    LegacyGateConfigHash {
        strict_v1: legacy_strict_gate_hash(&gate.strict_v1),
    }
}

fn legacy_strict_gate_hash(strict: &StrictGateConfig) -> LegacyStrictGateConfigHash {
    LegacyStrictGateConfigHash {
        min_train_samples: strict.min_train_samples,
        min_validation_samples: strict.min_validation_samples,
        min_train_waypoints: strict.min_train_waypoints,
        min_validation_waypoints: strict.min_validation_waypoints,
        max_validation_p95_residual_nm: strict.max_validation_p95_residual_nm,
        max_validation_rms_residual_nm: strict.max_validation_rms_residual_nm,
        max_validation_train_p95_ratio: strict.max_validation_train_p95_ratio,
        max_validation_train_rms_ratio: strict.max_validation_train_rms_ratio,
        max_compensated_delta_ratio: strict.max_compensated_delta_ratio,
        max_training_range_violations: strict.max_training_range_violations,
        good_margin_fraction: strict.good_margin_fraction,
        torque_delta_epsilon_nm: strict.torque_delta_epsilon_nm,
    }
}

fn default_torque_convention() -> String {
    crate::gravity::TORQUE_CONVENTION.to_string()
}

fn default_basis() -> String {
    crate::gravity::BASIS_TRIG_V1.to_string()
}

fn default_stable_tracking_error_rad() -> f64 {
    crate::gravity::replay_sample::DEFAULT_STABLE_TRACKING_ERROR_RAD
}

fn default_sample_reduction() -> SampleReductionMode {
    SampleReductionMode::RawRows
}

fn default_pair_q_error_max_rad() -> f64 {
    0.05
}

fn default_min_train_effective_pairs() -> usize {
    300
}

fn default_min_validation_effective_pairs() -> usize {
    80
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

fn validate_holdout_ratio(field: &str, value: f64) -> Result<()> {
    if !value.is_finite() || !(0.0..1.0).contains(&value) {
        bail!("{field} must be finite and in [0.0, 1.0)");
    }
    Ok(())
}

fn validate_positive_f64_array(field: &str, values: [f64; 6]) -> Result<()> {
    if values.iter().any(|value| !value.is_finite() || *value <= 0.0) {
        bail!("{field} values must be finite and > 0.0");
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
    fn missing_pair_mean_fields_default_to_raw_rows_for_legacy_profiles() {
        let input = r#"
name = "legacy"
role = "slave"
arm_id = "piper-follower"
target = "socketcan:can1"
joint_map = "identity"
load_profile = "normal-gripper-d405"
"#;

        let config = ProfileConfig::from_toml_str(input).unwrap();

        assert_eq!(config.fit.sample_reduction, SampleReductionMode::RawRows);
        assert_eq!(config.fit.pair_q_error_max_rad, 0.05);
        assert_eq!(config.gate.strict_v1.min_train_effective_pairs, 300);
        assert_eq!(config.gate.strict_v1.min_validation_effective_pairs, 80);
    }

    #[test]
    fn raw_row_default_hashes_match_legacy_serialization_shape() {
        let mut config = config_for_tests();
        config.fit = FitConfig::default();

        let expected_config_hash = legacy_profile_hash(&config);
        let section_hashes = config.section_sha256().unwrap();

        assert_eq!(
            config.config_sha256().unwrap(),
            sha256_canonical_json(&expected_config_hash).unwrap()
        );
        assert_eq!(
            section_hashes.fit,
            sha256_canonical_json(&legacy_fit_hash(&config.fit)).unwrap()
        );
        assert_eq!(
            section_hashes.gate_strict_v1,
            sha256_canonical_json(&legacy_strict_gate_hash(&config.gate.strict_v1)).unwrap()
        );
    }

    #[test]
    fn pair_mean_config_round_trips_in_kebab_case() {
        let mut config = ProfileConfig::new(
            "pair-mean",
            "slave",
            "piper-follower",
            "socketcan:can1",
            "identity",
            "normal-gripper-d405",
        );
        config.fit.sample_reduction = SampleReductionMode::BidirectionalPairMeanV1;
        let toml = toml::to_string_pretty(&config).unwrap();

        assert!(toml.contains("sample_reduction = \"bidirectional-pair-mean-v1\""));

        let decoded = ProfileConfig::from_toml_str(&toml).unwrap();
        assert_eq!(
            decoded.fit.sample_reduction,
            SampleReductionMode::BidirectionalPairMeanV1
        );
    }

    #[test]
    fn pair_mean_config_serializes_behavioral_defaults() {
        let config = ProfileConfig::new(
            "pair-mean",
            "slave",
            "piper-follower",
            "socketcan:can1",
            "identity",
            "normal-gripper-d405",
        );

        let toml = toml::to_string_pretty(&config).unwrap();

        assert!(toml.contains("pair_q_error_max_rad = 0.05"));
        assert!(toml.contains("min_train_effective_pairs = 300"));
        assert!(toml.contains("min_validation_effective_pairs = 80"));
    }

    #[test]
    fn pair_mean_config_hash_changes_when_pair_q_error_max_rad_changes() {
        let default_config = ProfileConfig::new(
            "pair-mean",
            "slave",
            "piper-follower",
            "socketcan:can1",
            "identity",
            "normal-gripper-d405",
        );
        let mut changed_config = default_config.clone();
        changed_config.fit.pair_q_error_max_rad = 0.06;

        assert_ne!(
            default_config.config_sha256().unwrap(),
            changed_config.config_sha256().unwrap()
        );
        assert_ne!(
            default_config.section_sha256().unwrap().fit,
            changed_config.section_sha256().unwrap().fit
        );
    }

    #[test]
    fn new_profiles_explicitly_default_to_pair_mean() {
        let config = ProfileConfig::new(
            "new-profile",
            "slave",
            "piper-follower",
            "socketcan:can1",
            "identity",
            "normal-gripper-d405",
        );

        assert_eq!(
            config.fit.sample_reduction,
            SampleReductionMode::BidirectionalPairMeanV1
        );
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
    fn identity_hash_ignores_name_but_config_hash_does_not() {
        let left = config_for_tests();
        let mut right = config_for_tests();
        right.name = "display-name-only".to_string();

        assert_eq!(
            left.identity_sha256().unwrap(),
            right.identity_sha256().unwrap()
        );
        assert_ne!(
            left.config_sha256().unwrap(),
            right.config_sha256().unwrap()
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
        assert_eq!(config.replay.stable_tracking_error_rad, 0.05);
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
    fn config_rejects_holdout_ratio_one() {
        let mut config = config_for_tests();
        config.fit.holdout_ratio = 1.0;

        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("fit.holdout_ratio"));
    }

    #[test]
    fn config_rejects_non_positive_residual_thresholds() {
        let mut p95_config = config_for_tests();
        p95_config.gate.strict_v1.max_validation_p95_residual_nm[0] = 0.0;

        let err = p95_config.validate().unwrap_err();
        assert!(err.to_string().contains("gate.strict_v1.max_validation_p95_residual_nm"));

        let mut rms_config = config_for_tests();
        rms_config.gate.strict_v1.max_validation_rms_residual_nm[0] = -0.1;

        let err = rms_config.validate().unwrap_err();
        assert!(err.to_string().contains("gate.strict_v1.max_validation_rms_residual_nm"));
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
            stable_tracking_error_rad = 0.05

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
            stable_tracking_error_rad = 0.05

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

    #[test]
    fn config_section_hashes_identify_changed_sections() {
        let left = config_for_tests();
        let mut right = config_for_tests();
        right.target = "socketcan:can0".to_string();

        let left_sections = left.section_sha256().unwrap();
        let right_sections = right.section_sha256().unwrap();

        assert_eq!(left_sections.name, right_sections.name);
        assert_ne!(left_sections.target, right_sections.target);
        assert_eq!(left_sections.replay, right_sections.replay);
        assert_eq!(left_sections.fit, right_sections.fit);
        assert_eq!(left_sections.gate_strict_v1, right_sections.gate_strict_v1);
    }
}
