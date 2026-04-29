#![allow(dead_code)]

use anyhow::{Context, Result, bail};
use clap::ValueEnum;
use piper_client::dual_arm::{
    BilateralLoopConfig, BilateralSubmissionMode, JointMirrorMap, LoopTimingMode,
};
use piper_client::types::{Joint, JointArray, NewtonMeter};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Arc, atomic::AtomicBool};

use crate::commands::teleop::TeleopDualArmArgs;

pub const DEFAULT_FREQUENCY_HZ: f64 = 200.0;
pub const MAX_CALIBRATION_ERROR_RAD: f64 = 0.05;
pub const DEFAULT_RAW_CLOCK_WARMUP_SECS: u64 = 10;
pub const DEFAULT_RAW_CLOCK_RESIDUAL_P95_US: u64 = 500;
pub const DEFAULT_RAW_CLOCK_RESIDUAL_MAX_US: u64 = 2000;
pub const DEFAULT_RAW_CLOCK_DRIFT_ABS_PPM: f64 = 500.0;
pub const DEFAULT_RAW_CLOCK_SAMPLE_GAP_MAX_MS: u64 = 20;
pub const DEFAULT_RAW_CLOCK_LAST_SAMPLE_AGE_MS: u64 = 20;
pub const DEFAULT_RAW_CLOCK_INTER_ARM_SKEW_MAX_US: u64 = 2000;
pub const DEFAULT_RAW_CLOCK_RESIDUAL_MAX_CONSECUTIVE_FAILURES: u32 = 3;
// Lab-experiment guardrails: generous enough for setup/debugging, low enough
// that raw-clock health gates cannot be configured into practical no-ops.
pub const MAX_RAW_CLOCK_WARMUP_SECS: u64 = 3600;
pub const MAX_RAW_CLOCK_RESIDUAL_P95_US: u64 = 100_000;
pub const MAX_RAW_CLOCK_RESIDUAL_MAX_US: u64 = 250_000;
pub const MAX_RAW_CLOCK_DRIFT_ABS_PPM: f64 = 1000.0;
pub const MAX_RAW_CLOCK_SAMPLE_GAP_MAX_MS: u64 = 1000;
pub const MAX_RAW_CLOCK_LAST_SAMPLE_AGE_MS: u64 = 1000;
pub const MAX_RAW_CLOCK_INTER_ARM_SKEW_MAX_US: u64 = 100_000;
pub const MAX_RAW_CLOCK_RESIDUAL_MAX_CONSECUTIVE_FAILURES: u32 = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TeleopMode {
    MasterFollower,
    Bilateral,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TeleopProfile {
    Production,
    Debug,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TeleopTimingMode {
    Sleep,
    Spin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TeleopJointMap {
    Identity,
    #[default]
    LeftRightMirror,
}

impl TeleopJointMap {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Identity => "identity",
            Self::LeftRightMirror => "left-right-mirror",
        }
    }

    pub fn to_joint_mirror_map(self) -> JointMirrorMap {
        match self {
            Self::Identity => JointMirrorMap {
                permutation: Joint::ALL,
                position_sign: [1.0; 6],
                velocity_sign: [1.0; 6],
                torque_sign: [1.0; 6],
            },
            Self::LeftRightMirror => JointMirrorMap::left_right_mirror(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct TeleopConfigFile {
    pub arms: Option<TeleopArmsConfig>,
    pub control: Option<TeleopControlConfig>,
    pub safety: Option<TeleopSafetyConfig>,
    pub calibration: Option<TeleopCalibrationConfig>,
    pub raw_clock: Option<TeleopRawClockConfig>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct TeleopArmsConfig {
    pub master: Option<TeleopRoleTargetConfig>,
    pub slave: Option<TeleopRoleTargetConfig>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct TeleopRoleTargetConfig {
    pub target: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct TeleopControlConfig {
    pub mode: Option<TeleopMode>,
    pub frequency_hz: Option<f64>,
    pub track_kp: Option<f64>,
    pub track_kd: Option<f64>,
    pub master_damping: Option<f64>,
    pub reflection_gain: Option<f64>,
    pub max_iterations: Option<usize>,
    pub timing_mode: Option<TeleopTimingMode>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct TeleopSafetyConfig {
    pub profile: Option<TeleopProfile>,
    pub gripper_mirror: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct TeleopCalibrationConfig {
    pub file: Option<PathBuf>,
    pub max_error_rad: Option<f64>,
    pub joint_map: Option<TeleopJointMap>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct TeleopRawClockConfig {
    pub warmup_secs: Option<u64>,
    pub residual_p95_us: Option<u64>,
    pub residual_max_us: Option<u64>,
    pub drift_abs_ppm: Option<f64>,
    pub sample_gap_max_ms: Option<u64>,
    pub last_sample_age_ms: Option<u64>,
    pub inter_arm_skew_max_us: Option<u64>,
    pub residual_max_consecutive_failures: Option<u32>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TeleopControlSettings {
    pub mode: TeleopMode,
    pub frequency_hz: f64,
    pub track_kp: f64,
    pub track_kd: f64,
    pub master_damping: f64,
    pub reflection_gain: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TeleopSafetySettings {
    pub profile: TeleopProfile,
    pub gripper_mirror: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TeleopCalibrationSettings {
    pub file: Option<PathBuf>,
    pub max_error_rad: f64,
    pub joint_map: TeleopJointMap,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TeleopRawClockSettings {
    pub experimental_calibrated_raw: bool,
    pub warmup_secs: u64,
    pub residual_p95_us: u64,
    pub residual_max_us: u64,
    pub drift_abs_ppm: f64,
    pub sample_gap_max_ms: u64,
    pub last_sample_age_ms: u64,
    pub inter_arm_skew_max_us: u64,
    pub residual_max_consecutive_failures: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedTeleopConfig {
    pub control: TeleopControlSettings,
    pub safety: TeleopSafetySettings,
    pub calibration: TeleopCalibrationSettings,
    pub raw_clock: TeleopRawClockSettings,
    pub max_iterations: Option<usize>,
    pub timing_mode: Option<TeleopTimingMode>,
}

impl TeleopConfigFile {
    pub fn load(path: &Path) -> Result<Self> {
        let contents = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read teleop config {}", path.display()))?;
        toml::from_str(&contents)
            .with_context(|| format!("failed to parse teleop config {}", path.display()))
    }
}

impl Default for TeleopControlSettings {
    fn default() -> Self {
        Self {
            mode: TeleopMode::MasterFollower,
            frequency_hz: DEFAULT_FREQUENCY_HZ,
            track_kp: 8.0,
            track_kd: 1.0,
            master_damping: 0.4,
            reflection_gain: 0.25,
        }
    }
}

impl TeleopControlSettings {
    pub fn validate(&self) -> Result<()> {
        validate_range("frequency_hz", self.frequency_hz, 10.0, 500.0)?;
        validate_range("track_kp", self.track_kp, 0.0, 20.0)?;
        validate_range("track_kd", self.track_kd, 0.0, 5.0)?;
        validate_range("master_damping", self.master_damping, 0.0, 2.0)?;
        validate_range("reflection_gain", self.reflection_gain, 0.0, 0.5)?;
        Ok(())
    }
}

impl TeleopRawClockSettings {
    pub fn validate(&self) -> Result<()> {
        validate_u64_range(
            "warmup_secs",
            self.warmup_secs,
            1,
            MAX_RAW_CLOCK_WARMUP_SECS,
        )?;
        validate_u64_range(
            "residual_p95_us",
            self.residual_p95_us,
            1,
            MAX_RAW_CLOCK_RESIDUAL_P95_US,
        )?;
        validate_u64_range(
            "residual_max_us",
            self.residual_max_us,
            1,
            MAX_RAW_CLOCK_RESIDUAL_MAX_US,
        )?;
        if self.residual_p95_us > self.residual_max_us {
            bail!(
                "residual_p95_us must be less than or equal to residual_max_us; got residual_p95_us={} and residual_max_us={}",
                self.residual_p95_us,
                self.residual_max_us
            );
        }
        if !self.drift_abs_ppm.is_finite()
            || self.drift_abs_ppm <= 0.0
            || self.drift_abs_ppm > MAX_RAW_CLOCK_DRIFT_ABS_PPM
        {
            bail!(
                "drift_abs_ppm must be finite and greater than 0.0 up to {MAX_RAW_CLOCK_DRIFT_ABS_PPM}; got {}",
                self.drift_abs_ppm
            );
        }
        validate_u64_range(
            "sample_gap_max_ms",
            self.sample_gap_max_ms,
            1,
            MAX_RAW_CLOCK_SAMPLE_GAP_MAX_MS,
        )?;
        validate_u64_range(
            "last_sample_age_ms",
            self.last_sample_age_ms,
            1,
            MAX_RAW_CLOCK_LAST_SAMPLE_AGE_MS,
        )?;
        validate_u64_range(
            "inter_arm_skew_max_us",
            self.inter_arm_skew_max_us,
            1,
            MAX_RAW_CLOCK_INTER_ARM_SKEW_MAX_US,
        )?;
        validate_u32_range(
            "residual_max_consecutive_failures",
            self.residual_max_consecutive_failures,
            1,
            MAX_RAW_CLOCK_RESIDUAL_MAX_CONSECUTIVE_FAILURES,
        )?;
        Ok(())
    }
}

impl ResolvedTeleopConfig {
    pub fn resolve(args: TeleopDualArmArgs, file: Option<TeleopConfigFile>) -> Result<Self> {
        let file_control = file.as_ref().and_then(|file| file.control.as_ref());
        let file_safety = file.as_ref().and_then(|file| file.safety.as_ref());
        let file_calibration = file.as_ref().and_then(|file| file.calibration.as_ref());
        let file_raw_clock = file.as_ref().and_then(|file| file.raw_clock.as_ref());

        let control = TeleopControlSettings {
            mode: args
                .mode
                .or_else(|| file_control.and_then(|control| control.mode))
                .unwrap_or(TeleopMode::MasterFollower),
            frequency_hz: args
                .frequency_hz
                .or_else(|| file_control.and_then(|control| control.frequency_hz))
                .unwrap_or(DEFAULT_FREQUENCY_HZ),
            track_kp: args
                .track_kp
                .or_else(|| file_control.and_then(|control| control.track_kp))
                .unwrap_or(8.0),
            track_kd: args
                .track_kd
                .or_else(|| file_control.and_then(|control| control.track_kd))
                .unwrap_or(1.0),
            master_damping: args
                .master_damping
                .or_else(|| file_control.and_then(|control| control.master_damping))
                .unwrap_or(0.4),
            reflection_gain: args
                .reflection_gain
                .or_else(|| file_control.and_then(|control| control.reflection_gain))
                .unwrap_or(0.25),
        };
        control.validate()?;

        if args.experimental_calibrated_raw && control.mode != TeleopMode::MasterFollower {
            bail!("experimental calibrated raw clock requires master-follower mode");
        }

        let raw_clock = TeleopRawClockSettings {
            experimental_calibrated_raw: args.experimental_calibrated_raw,
            warmup_secs: args
                .raw_clock_warmup_secs
                .or_else(|| file_raw_clock.and_then(|raw_clock| raw_clock.warmup_secs))
                .unwrap_or(DEFAULT_RAW_CLOCK_WARMUP_SECS),
            residual_p95_us: args
                .raw_clock_residual_p95_us
                .or_else(|| file_raw_clock.and_then(|raw_clock| raw_clock.residual_p95_us))
                .unwrap_or(DEFAULT_RAW_CLOCK_RESIDUAL_P95_US),
            residual_max_us: args
                .raw_clock_residual_max_us
                .or_else(|| file_raw_clock.and_then(|raw_clock| raw_clock.residual_max_us))
                .unwrap_or(DEFAULT_RAW_CLOCK_RESIDUAL_MAX_US),
            drift_abs_ppm: args
                .raw_clock_drift_abs_ppm
                .or_else(|| file_raw_clock.and_then(|raw_clock| raw_clock.drift_abs_ppm))
                .unwrap_or(DEFAULT_RAW_CLOCK_DRIFT_ABS_PPM),
            sample_gap_max_ms: args
                .raw_clock_sample_gap_max_ms
                .or_else(|| file_raw_clock.and_then(|raw_clock| raw_clock.sample_gap_max_ms))
                .unwrap_or(DEFAULT_RAW_CLOCK_SAMPLE_GAP_MAX_MS),
            last_sample_age_ms: args
                .raw_clock_last_sample_age_ms
                .or_else(|| file_raw_clock.and_then(|raw_clock| raw_clock.last_sample_age_ms))
                .unwrap_or(DEFAULT_RAW_CLOCK_LAST_SAMPLE_AGE_MS),
            inter_arm_skew_max_us: args
                .raw_clock_inter_arm_skew_max_us
                .or_else(|| file_raw_clock.and_then(|raw_clock| raw_clock.inter_arm_skew_max_us))
                .unwrap_or(DEFAULT_RAW_CLOCK_INTER_ARM_SKEW_MAX_US),
            residual_max_consecutive_failures: args
                .raw_clock_residual_max_consecutive_failures
                .or_else(|| {
                    file_raw_clock.and_then(|raw_clock| raw_clock.residual_max_consecutive_failures)
                })
                .unwrap_or(DEFAULT_RAW_CLOCK_RESIDUAL_MAX_CONSECUTIVE_FAILURES),
        };
        raw_clock.validate()?;

        let calibration_max_error_rad = args
            .calibration_max_error_rad
            .or_else(|| file_calibration.and_then(|calibration| calibration.max_error_rad))
            .unwrap_or(MAX_CALIBRATION_ERROR_RAD);
        validate_calibration_max_error_rad(calibration_max_error_rad)?;

        Ok(Self {
            control,
            safety: TeleopSafetySettings {
                profile: args
                    .profile
                    .or_else(|| file_safety.and_then(|safety| safety.profile))
                    .unwrap_or(TeleopProfile::Production),
                gripper_mirror: if args.disable_gripper_mirror {
                    false
                } else {
                    file_safety.and_then(|safety| safety.gripper_mirror).unwrap_or(true)
                },
            },
            calibration: TeleopCalibrationSettings {
                file: args
                    .calibration_file
                    .or_else(|| file_calibration.and_then(|calibration| calibration.file.clone())),
                max_error_rad: calibration_max_error_rad,
                joint_map: args
                    .joint_map
                    .or_else(|| file_calibration.and_then(|calibration| calibration.joint_map))
                    .unwrap_or_default(),
            },
            raw_clock,
            max_iterations: args
                .max_iterations
                .or_else(|| file_control.and_then(|control| control.max_iterations)),
            timing_mode: args
                .timing_mode
                .or_else(|| file_control.and_then(|control| control.timing_mode)),
        })
    }

    pub fn loop_config(&self, cancel_signal: Arc<AtomicBool>) -> BilateralLoopConfig {
        let mut config = BilateralLoopConfig {
            frequency_hz: self.control.frequency_hz,
            max_iterations: self.max_iterations,
            cancel_signal: Some(cancel_signal),
            ..BilateralLoopConfig::default()
        };
        config.submission_mode = BilateralSubmissionMode::Unconfirmed;
        config.master_interaction_slew_limit_nm_per_s =
            JointArray::splat(NewtonMeter(0.25 * self.control.frequency_hz));
        if let Some(timing_mode) = self.timing_mode {
            config.timing_mode = match timing_mode {
                TeleopTimingMode::Sleep => LoopTimingMode::Sleep,
                TeleopTimingMode::Spin => LoopTimingMode::Spin,
            };
        }
        config.gripper.enabled = self.safety.gripper_mirror;
        config
    }
}

fn validate_range(name: &str, value: f64, min: f64, max: f64) -> Result<()> {
    if !value.is_finite() || value < min || value > max {
        bail!("{name} must be finite and between {min} and {max}; got {value}");
    }
    Ok(())
}

fn validate_u64_range(name: &str, value: u64, min: u64, max: u64) -> Result<()> {
    if value < min || value > max {
        bail!("{name} must be between {min} and {max}; got {value}");
    }
    Ok(())
}

fn validate_u32_range(name: &str, value: u32, min: u32, max: u32) -> Result<()> {
    if !(min..=max).contains(&value) {
        bail!("{name} must be between {min} and {max}; got {value}");
    }
    Ok(())
}

fn validate_calibration_max_error_rad(value: f64) -> Result<()> {
    if !value.is_finite() || value <= 0.0 || value > MAX_CALIBRATION_ERROR_RAD {
        bail!(
            "calibration_max_error_rad must be finite and greater than 0.0 up to {MAX_CALIBRATION_ERROR_RAD}; got {value}"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::teleop::TeleopDualArmArgs;
    use std::io::Write;
    use std::sync::atomic::Ordering;

    #[test]
    fn cli_values_override_file_values() {
        let file = TeleopConfigFile {
            control: Some(TeleopControlConfig {
                mode: Some(TeleopMode::Bilateral),
                frequency_hz: Some(100.0),
                ..Default::default()
            }),
            ..Default::default()
        };
        let args = TeleopDualArmArgs {
            mode: Some(TeleopMode::MasterFollower),
            frequency_hz: Some(200.0),
            ..TeleopDualArmArgs::default_for_tests()
        };

        let resolved = ResolvedTeleopConfig::resolve(args, Some(file)).unwrap();

        assert_eq!(resolved.control.mode, TeleopMode::MasterFollower);
        assert_eq!(resolved.control.frequency_hz, 200.0);
    }

    #[test]
    fn hard_limits_reject_unsafe_gains() {
        let err = TeleopControlSettings {
            track_kp: 21.0,
            ..TeleopControlSettings::default()
        }
        .validate()
        .expect_err("track_kp above hard cap must fail");

        assert!(err.to_string().contains("track_kp"));
    }

    #[test]
    fn hard_limits_reject_non_finite_values() {
        let nan_err = TeleopControlSettings {
            frequency_hz: f64::NAN,
            ..TeleopControlSettings::default()
        }
        .validate()
        .expect_err("NaN frequency must fail");
        assert!(nan_err.to_string().contains("frequency_hz"));

        let infinity_err = TeleopControlSettings {
            reflection_gain: f64::INFINITY,
            ..TeleopControlSettings::default()
        }
        .validate()
        .expect_err("infinite reflection gain must fail");
        assert!(infinity_err.to_string().contains("reflection_gain"));
    }

    #[test]
    fn calibration_max_error_hard_limit_is_enforced() {
        let err = ResolvedTeleopConfig::resolve(
            TeleopDualArmArgs {
                calibration_max_error_rad: Some(0.06),
                ..TeleopDualArmArgs::default_for_tests()
            },
            None,
        )
        .expect_err("calibration max error above cap must fail");

        assert!(err.to_string().contains("calibration_max_error_rad"));
    }

    #[test]
    fn default_mode_is_master_follower() {
        let resolved =
            ResolvedTeleopConfig::resolve(TeleopDualArmArgs::default_for_tests(), None).unwrap();

        assert_eq!(resolved.control.mode, TeleopMode::MasterFollower);
    }

    #[test]
    fn experimental_raw_clock_rejects_bilateral_mode() {
        let args = TeleopDualArmArgs {
            experimental_calibrated_raw: true,
            mode: Some(TeleopMode::Bilateral),
            ..TeleopDualArmArgs::default_for_tests()
        };

        let err = ResolvedTeleopConfig::resolve(args, None).unwrap_err();
        assert!(err.to_string().contains("master-follower"));
    }

    #[test]
    fn cli_raw_clock_values_override_file_values() {
        let file = TeleopConfigFile {
            raw_clock: Some(TeleopRawClockConfig {
                warmup_secs: Some(30),
                ..Default::default()
            }),
            ..Default::default()
        };
        let args = TeleopDualArmArgs {
            experimental_calibrated_raw: true,
            raw_clock_warmup_secs: Some(10),
            ..TeleopDualArmArgs::default_for_tests()
        };

        let resolved = ResolvedTeleopConfig::resolve(args, Some(file)).unwrap();
        assert!(resolved.raw_clock.experimental_calibrated_raw);
        assert_eq!(resolved.raw_clock.warmup_secs, 10);
    }

    #[test]
    fn cli_joint_map_overrides_file_joint_map() {
        let file = TeleopConfigFile {
            calibration: Some(TeleopCalibrationConfig {
                joint_map: Some(TeleopJointMap::LeftRightMirror),
                ..Default::default()
            }),
            ..Default::default()
        };
        let args = TeleopDualArmArgs {
            joint_map: Some(TeleopJointMap::Identity),
            ..TeleopDualArmArgs::default_for_tests()
        };

        let resolved = ResolvedTeleopConfig::resolve(args, Some(file)).unwrap();

        assert_eq!(resolved.calibration.joint_map, TeleopJointMap::Identity);
    }

    #[test]
    fn config_file_cannot_enable_experimental_raw_clock_without_cli_flag() {
        let config_text = r#"
        [raw_clock]
        experimental_calibrated_raw = true
        "#;

        let err = toml::from_str::<TeleopConfigFile>(config_text).unwrap_err();
        assert!(err.to_string().contains("unknown field"));
    }

    #[test]
    fn raw_clock_threshold_file_does_not_enable_experimental_mode() {
        let file = TeleopConfigFile {
            raw_clock: Some(TeleopRawClockConfig {
                warmup_secs: Some(30),
                ..Default::default()
            }),
            ..Default::default()
        };
        let args = TeleopDualArmArgs {
            experimental_calibrated_raw: false,
            ..TeleopDualArmArgs::default_for_tests()
        };

        let resolved = ResolvedTeleopConfig::resolve(args, Some(file)).unwrap();
        assert!(!resolved.raw_clock.experimental_calibrated_raw);
        assert_eq!(resolved.raw_clock.warmup_secs, 30);
    }

    #[test]
    fn raw_clock_validation_rejects_non_finite_drift() {
        let args = TeleopDualArmArgs {
            raw_clock_drift_abs_ppm: Some(f64::INFINITY),
            ..TeleopDualArmArgs::default_for_tests()
        };

        let err = ResolvedTeleopConfig::resolve(args, None).unwrap_err();
        assert!(err.to_string().contains("drift_abs_ppm"));
    }

    #[test]
    fn raw_clock_validation_rejects_p95_above_max() {
        let args = TeleopDualArmArgs {
            raw_clock_residual_p95_us: Some(3000),
            raw_clock_residual_max_us: Some(2000),
            ..TeleopDualArmArgs::default_for_tests()
        };

        let err = ResolvedTeleopConfig::resolve(args, None).unwrap_err();
        assert!(err.to_string().contains("residual_p95_us"));
        assert!(err.to_string().contains("residual_max_us"));
    }

    #[test]
    fn cli_residual_max_consecutive_failures_overrides_file_value() {
        let file = TeleopConfigFile {
            raw_clock: Some(TeleopRawClockConfig {
                residual_max_consecutive_failures: Some(9),
                ..Default::default()
            }),
            ..Default::default()
        };
        let args = TeleopDualArmArgs {
            raw_clock_residual_max_consecutive_failures: Some(3),
            ..TeleopDualArmArgs::default_for_tests()
        };

        let resolved = ResolvedTeleopConfig::resolve(args, Some(file)).unwrap();

        assert_eq!(resolved.raw_clock.residual_max_consecutive_failures, 3);
    }

    #[test]
    fn file_residual_max_consecutive_failures_is_used_when_cli_missing() {
        let file = TeleopConfigFile {
            raw_clock: Some(TeleopRawClockConfig {
                residual_max_consecutive_failures: Some(4),
                ..Default::default()
            }),
            ..Default::default()
        };

        let resolved =
            ResolvedTeleopConfig::resolve(TeleopDualArmArgs::default_for_tests(), Some(file))
                .unwrap();

        assert_eq!(resolved.raw_clock.residual_max_consecutive_failures, 4);
    }

    #[test]
    fn raw_clock_validation_rejects_zero_residual_max_consecutive_failures() {
        let args = TeleopDualArmArgs {
            raw_clock_residual_max_consecutive_failures: Some(0),
            ..TeleopDualArmArgs::default_for_tests()
        };

        let err = ResolvedTeleopConfig::resolve(args, None).unwrap_err();
        assert!(err.to_string().contains("residual_max_consecutive_failures"));
    }

    #[test]
    fn raw_clock_validation_rejects_large_residual_max_consecutive_failures() {
        let args = TeleopDualArmArgs {
            raw_clock_residual_max_consecutive_failures: Some(101),
            ..TeleopDualArmArgs::default_for_tests()
        };

        let err = ResolvedTeleopConfig::resolve(args, None).unwrap_err();
        assert!(err.to_string().contains("residual_max_consecutive_failures"));
    }

    #[test]
    fn raw_clock_validation_rejects_zero_warmup() {
        let args = TeleopDualArmArgs {
            raw_clock_warmup_secs: Some(0),
            ..TeleopDualArmArgs::default_for_tests()
        };

        let err = ResolvedTeleopConfig::resolve(args, None).unwrap_err();
        assert!(err.to_string().contains("warmup_secs"));
    }

    #[test]
    fn raw_clock_validation_rejects_zero_inter_arm_skew() {
        let args = TeleopDualArmArgs {
            raw_clock_inter_arm_skew_max_us: Some(0),
            ..TeleopDualArmArgs::default_for_tests()
        };

        let err = ResolvedTeleopConfig::resolve(args, None).unwrap_err();
        assert!(err.to_string().contains("inter_arm_skew_max_us"));
    }

    #[test]
    fn raw_clock_validation_rejects_effectively_disabled_thresholds() {
        let args = TeleopDualArmArgs {
            raw_clock_residual_max_us: Some(u64::MAX),
            ..TeleopDualArmArgs::default_for_tests()
        };

        let err = ResolvedTeleopConfig::resolve(args, None).unwrap_err();
        assert!(err.to_string().contains("residual_max_us"));
    }

    #[test]
    fn rejects_unknown_top_level_config_keys() {
        let err = toml::from_str::<TeleopConfigFile>(
            r#"
            unexpected = true
            "#,
        )
        .expect_err("unknown top-level keys must fail");

        assert!(err.to_string().contains("unexpected"));
    }

    #[test]
    fn rejects_unknown_nested_control_keys() {
        let err = toml::from_str::<TeleopConfigFile>(
            r#"
            [control]
            frequncy_hz = 200.0
            "#,
        )
        .expect_err("unknown control keys must fail");

        assert!(err.to_string().contains("frequncy_hz"));
    }

    #[test]
    fn loads_toml_config_from_path() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        write!(
            file,
            r#"
            [control]
            frequency_hz = 250.0
            "#
        )
        .unwrap();

        let loaded = TeleopConfigFile::load(file.path()).unwrap();

        assert_eq!(
            loaded.control.as_ref().and_then(|control| control.frequency_hz),
            Some(250.0)
        );
    }

    #[test]
    fn loop_config_applies_resolved_fields() {
        let args = TeleopDualArmArgs {
            frequency_hz: Some(150.0),
            max_iterations: Some(3),
            timing_mode: Some(TeleopTimingMode::Sleep),
            disable_gripper_mirror: true,
            ..TeleopDualArmArgs::default_for_tests()
        };
        let resolved = ResolvedTeleopConfig::resolve(args, None).unwrap();
        let cancel_signal = Arc::new(AtomicBool::new(false));

        let loop_config = resolved.loop_config(cancel_signal.clone());

        assert_eq!(loop_config.frequency_hz, 150.0);
        assert_eq!(loop_config.max_iterations, Some(3));
        assert_eq!(loop_config.timing_mode, LoopTimingMode::Sleep);
        assert_eq!(
            loop_config.submission_mode,
            BilateralSubmissionMode::Unconfirmed
        );
        assert_eq!(
            loop_config.master_interaction_slew_limit_nm_per_s,
            JointArray::splat(NewtonMeter(37.5))
        );
        assert!(!loop_config.gripper.enabled);
        assert!(!cancel_signal.load(Ordering::Relaxed));
        assert!(loop_config.cancel_signal.is_some());
    }
}
