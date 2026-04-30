use std::time::Duration;

use anyhow::{Result, bail};
use piper_client::dual_arm::{
    BilateralLoopConfig, BilateralOutputShapingConfig, GripperTeleopConfig,
};
use piper_client::dual_arm_raw_clock::{
    ExperimentalRawClockConfig, ExperimentalRawClockMode, ExperimentalRawClockRunConfig,
    RawClockRuntimeThresholds,
};
use piper_client::observer::ControlReadPolicy;
use piper_tools::raw_clock::RawClockThresholds;

use crate::args::Args;
use crate::profile::EffectiveProfile;

pub const DEFAULT_RAW_CLOCK_WARMUP_SECS: u64 = 10;
pub const DEFAULT_RAW_CLOCK_RESIDUAL_P95_US: u64 = 2000;
pub const DEFAULT_RAW_CLOCK_RESIDUAL_MAX_US: u64 = 3000;
pub const DEFAULT_RAW_CLOCK_DRIFT_ABS_PPM: f64 = 500.0;
pub const DEFAULT_RAW_CLOCK_SAMPLE_GAP_MAX_MS: u64 = 50;
pub const DEFAULT_RAW_CLOCK_LAST_SAMPLE_AGE_MS: u64 = 20;
pub const DEFAULT_RAW_CLOCK_SELECTED_SAMPLE_AGE_MS: u64 = 50;
pub const DEFAULT_RAW_CLOCK_STATE_SKEW_MAX_US: u64 = 10_000;
pub const DEFAULT_RAW_CLOCK_INTER_ARM_SKEW_MAX_US: u64 = 20_000;
pub const DEFAULT_RAW_CLOCK_RESIDUAL_MAX_CONSECUTIVE_FAILURES: u32 = 3;
pub const DEFAULT_RAW_CLOCK_ALIGNMENT_LAG_US: u64 = 5_000;
pub const DEFAULT_RAW_CLOCK_ALIGNMENT_SEARCH_WINDOW_US: u64 = 25_000;
pub const DEFAULT_RAW_CLOCK_ALIGNMENT_BUFFER_MISS_CONSECUTIVE_FAILURES: u32 = 3;

pub const MAX_RAW_CLOCK_WARMUP_SECS: u64 = 3600;
pub const MAX_RAW_CLOCK_RESIDUAL_P95_US: u64 = 100_000;
pub const MAX_RAW_CLOCK_RESIDUAL_MAX_US: u64 = 250_000;
pub const MAX_RAW_CLOCK_DRIFT_ABS_PPM: f64 = 1000.0;
pub const MAX_RAW_CLOCK_SAMPLE_GAP_MAX_MS: u64 = 1000;
pub const MAX_RAW_CLOCK_LAST_SAMPLE_AGE_MS: u64 = 1000;
pub const MAX_RAW_CLOCK_SELECTED_SAMPLE_AGE_MS: u64 = 1000;
pub const MAX_RAW_CLOCK_STATE_SKEW_MAX_US: u64 = 100_000;
pub const MAX_RAW_CLOCK_INTER_ARM_SKEW_MAX_US: u64 = 100_000;
pub const MAX_RAW_CLOCK_RESIDUAL_MAX_CONSECUTIVE_FAILURES: u32 = 100;
pub const MAX_RAW_CLOCK_ALIGNMENT_LAG_US: u64 = 100_000;
pub const MAX_RAW_CLOCK_ALIGNMENT_SEARCH_WINDOW_US: u64 = 100_000;
pub const MAX_RAW_CLOCK_ALIGNMENT_BUFFER_MISS_CONSECUTIVE_FAILURES: u32 = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SvsRuntimeKind {
    StrictRealtime,
    CalibratedRawClock,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SvsRawClockSettings {
    pub warmup_secs: u64,
    pub residual_p95_us: u64,
    pub residual_max_us: u64,
    pub drift_abs_ppm: f64,
    pub sample_gap_max_ms: u64,
    pub last_sample_age_ms: u64,
    pub selected_sample_age_ms: u64,
    pub inter_arm_skew_max_us: u64,
    pub state_skew_max_us: u64,
    pub residual_max_consecutive_failures: u32,
    pub alignment_lag_us: u64,
    pub alignment_search_window_us: u64,
    pub alignment_buffer_miss_consecutive_failures: u32,
}

impl SvsRuntimeKind {
    pub fn from_args(args: &Args) -> Self {
        if args.experimental_calibrated_raw {
            Self::CalibratedRawClock
        } else {
            Self::StrictRealtime
        }
    }
}

impl SvsRawClockSettings {
    pub fn resolve(args: &Args, profile: &EffectiveProfile) -> Result<Self> {
        let raw = &profile.raw_clock;
        let settings = Self {
            warmup_secs: args.raw_clock_warmup_secs.unwrap_or(raw.warmup_secs),
            residual_p95_us: args.raw_clock_residual_p95_us.unwrap_or(raw.residual_p95_us),
            residual_max_us: args.raw_clock_residual_max_us.unwrap_or(raw.residual_max_us),
            drift_abs_ppm: args.raw_clock_drift_abs_ppm.unwrap_or(raw.drift_abs_ppm),
            sample_gap_max_ms: args.raw_clock_sample_gap_max_ms.unwrap_or(raw.sample_gap_max_ms),
            last_sample_age_ms: args.raw_clock_last_sample_age_ms.unwrap_or(raw.last_sample_age_ms),
            selected_sample_age_ms: args
                .raw_clock_selected_sample_age_ms
                .unwrap_or(raw.selected_sample_age_ms),
            inter_arm_skew_max_us: args
                .raw_clock_inter_arm_skew_max_us
                .unwrap_or(raw.inter_arm_skew_max_us),
            state_skew_max_us: args.raw_clock_state_skew_max_us.unwrap_or(raw.state_skew_max_us),
            residual_max_consecutive_failures: args
                .raw_clock_residual_max_consecutive_failures
                .unwrap_or(raw.residual_max_consecutive_failures),
            alignment_lag_us: args.raw_clock_alignment_lag_us.unwrap_or(raw.alignment_lag_us),
            alignment_search_window_us: args
                .raw_clock_alignment_search_window_us
                .unwrap_or(raw.alignment_search_window_us),
            alignment_buffer_miss_consecutive_failures: args
                .raw_clock_alignment_buffer_miss_consecutive_failures
                .unwrap_or(raw.alignment_buffer_miss_consecutive_failures),
        };
        settings.validate()?;
        Ok(settings)
    }

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
            bail!("raw-clock residual_p95_us must be <= residual_max_us");
        }
        if !self.drift_abs_ppm.is_finite()
            || self.drift_abs_ppm <= 0.0
            || self.drift_abs_ppm > MAX_RAW_CLOCK_DRIFT_ABS_PPM
        {
            bail!(
                "raw-clock drift_abs_ppm must be finite and in 0..={MAX_RAW_CLOCK_DRIFT_ABS_PPM}"
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
            "selected_sample_age_ms",
            self.selected_sample_age_ms,
            1,
            MAX_RAW_CLOCK_SELECTED_SAMPLE_AGE_MS,
        )?;
        validate_u64_range(
            "inter_arm_skew_max_us",
            self.inter_arm_skew_max_us,
            1,
            MAX_RAW_CLOCK_INTER_ARM_SKEW_MAX_US,
        )?;
        validate_u64_range(
            "state_skew_max_us",
            self.state_skew_max_us,
            1,
            MAX_RAW_CLOCK_STATE_SKEW_MAX_US,
        )?;
        validate_u32_range(
            "residual_max_consecutive_failures",
            self.residual_max_consecutive_failures,
            1,
            MAX_RAW_CLOCK_RESIDUAL_MAX_CONSECUTIVE_FAILURES,
        )?;
        validate_u64_range(
            "alignment_lag_us",
            self.alignment_lag_us,
            1,
            MAX_RAW_CLOCK_ALIGNMENT_LAG_US,
        )?;
        validate_u64_range(
            "alignment_search_window_us",
            self.alignment_search_window_us,
            1,
            MAX_RAW_CLOCK_ALIGNMENT_SEARCH_WINDOW_US,
        )?;
        validate_u32_range(
            "alignment_buffer_miss_consecutive_failures",
            self.alignment_buffer_miss_consecutive_failures,
            1,
            MAX_RAW_CLOCK_ALIGNMENT_BUFFER_MISS_CONSECUTIVE_FAILURES,
        )?;
        if self.selected_sample_age_ms < self.last_sample_age_ms {
            bail!("raw-clock selected_sample_age_ms must be >= last_sample_age_ms");
        }
        if self.alignment_lag_us >= self.selected_sample_age_ms.saturating_mul(1_000) {
            bail!("raw-clock alignment_lag_us must be less than selected_sample_age_ms");
        }
        if self.alignment_lag_us >= self.last_sample_age_ms.saturating_mul(1_000) {
            bail!("raw-clock alignment_lag_us must be less than last_sample_age_ms");
        }
        if self.inter_arm_skew_max_us > self.alignment_search_window_us {
            bail!("raw-clock inter_arm_skew_max_us must be <= alignment_search_window_us");
        }
        Ok(())
    }

    pub fn to_experimental_config(
        &self,
        frequency_hz: f64,
        max_iterations: Option<usize>,
    ) -> ExperimentalRawClockConfig {
        ExperimentalRawClockConfig {
            mode: ExperimentalRawClockMode::Bilateral,
            frequency_hz,
            max_iterations,
            estimator_thresholds: RawClockThresholds {
                warmup_samples: raw_clock_warmup_sample_threshold(self),
                warmup_window_us: self.warmup_secs * 1_000_000,
                residual_p95_us: self.residual_p95_us,
                residual_max_us: self.residual_max_us,
                drift_abs_ppm: self.drift_abs_ppm,
                sample_gap_max_us: self.sample_gap_max_ms * 1_000,
                last_sample_age_us: self.last_sample_age_ms * 1_000,
            },
            thresholds: RawClockRuntimeThresholds {
                inter_arm_skew_max_us: self.inter_arm_skew_max_us,
                last_sample_age_us: self.last_sample_age_ms * 1_000,
                selected_sample_age_us: self.selected_sample_age_ms * 1_000,
                residual_max_consecutive_failures: self.residual_max_consecutive_failures,
                alignment_lag_us: self.alignment_lag_us,
                alignment_search_window_us: self.alignment_search_window_us,
                alignment_buffer_miss_consecutive_failures: self
                    .alignment_buffer_miss_consecutive_failures,
            },
        }
    }

    pub fn read_policy(&self) -> ControlReadPolicy {
        ControlReadPolicy {
            max_state_skew_us: self.state_skew_max_us,
            max_feedback_age: Duration::from_millis(self.last_sample_age_ms),
        }
    }

    pub fn to_run_config(
        &self,
        loop_config: &BilateralLoopConfig,
    ) -> ExperimentalRawClockRunConfig {
        ExperimentalRawClockRunConfig {
            read_policy: self.read_policy(),
            command_timeout: Duration::from_millis(20),
            disable_config: loop_config.disable_config.clone(),
            cancel_signal: loop_config.cancel_signal.clone(),
            dt_clamp_multiplier: loop_config.dt_clamp_multiplier,
            telemetry_sink: loop_config.telemetry_sink.clone(),
            gripper: GripperTeleopConfig {
                enabled: false,
                ..loop_config.gripper
            },
            output_shaping: Some(BilateralOutputShapingConfig::from_loop_config(loop_config)),
        }
    }
}

pub fn raw_clock_warmup_sample_threshold(settings: &SvsRawClockSettings) -> usize {
    let warmup_ms = settings.warmup_secs.saturating_mul(1_000);
    let sample_gap_max_ms = settings.sample_gap_max_ms.max(1);
    let samples = warmup_ms.saturating_add(sample_gap_max_ms - 1) / sample_gap_max_ms;

    usize::try_from(samples.max(4)).unwrap_or(usize::MAX)
}

fn validate_u64_range(name: &str, value: u64, min: u64, max: u64) -> Result<()> {
    if !(min..=max).contains(&value) {
        bail!("raw-clock {name} must be in {min}..={max}; got {value}");
    }
    Ok(())
}

fn validate_u32_range(name: &str, value: u32, min: u32, max: u32) -> Result<()> {
    if !(min..=max).contains(&value) {
        bail!("raw-clock {name} must be in {min}..={max}; got {value}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{args::Args, profile::EffectiveProfile};
    use piper_client::dual_arm::{
        BilateralLoopConfig, BilateralLoopTelemetry, BilateralLoopTelemetrySink,
        BilateralOutputShapingConfig, GripperTeleopConfig,
    };
    use piper_client::types::{JointArray, NewtonMeter};
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    use std::time::Duration;

    struct NoopTelemetrySink;

    impl BilateralLoopTelemetrySink for NoopTelemetrySink {
        fn on_tick(
            &self,
            _telemetry: &BilateralLoopTelemetry,
        ) -> std::result::Result<(), piper_client::dual_arm::BilateralTelemetrySinkError> {
            Ok(())
        }
    }

    fn args_for_raw_clock_resolve_tests() -> Args {
        Args {
            master_target: "socketcan:can0".to_string(),
            slave_target: "socketcan:can1".to_string(),
            baud_rate: None,
            model_dir: Some(PathBuf::from("/tmp/model")),
            use_standard_model_path: false,
            use_embedded_model: false,
            task_profile: None,
            output_dir: PathBuf::from("/tmp/out"),
            calibration_file: None,
            save_calibration: None,
            calibration_max_error_rad: None,
            mirror_map: None,
            operator: None,
            task: Some("test".to_string()),
            notes: None,
            raw_can: false,
            disable_gripper_mirror: true,
            max_iterations: None,
            timing_mode: "spin".to_string(),
            yes: true,
            experimental_calibrated_raw: true,
            raw_clock_warmup_secs: None,
            raw_clock_residual_p95_us: None,
            raw_clock_residual_max_us: None,
            raw_clock_drift_abs_ppm: None,
            raw_clock_sample_gap_max_ms: None,
            raw_clock_last_sample_age_ms: None,
            raw_clock_selected_sample_age_ms: None,
            raw_clock_inter_arm_skew_max_us: None,
            raw_clock_state_skew_max_us: None,
            raw_clock_residual_max_consecutive_failures: None,
            raw_clock_alignment_lag_us: None,
            raw_clock_alignment_search_window_us: None,
            raw_clock_alignment_buffer_miss_consecutive_failures: None,
        }
    }

    #[test]
    fn raw_clock_settings_cli_overrides_profile_values() {
        let mut args = args_for_raw_clock_resolve_tests();
        let mut profile = EffectiveProfile::default_for_tests();
        profile.raw_clock.warmup_secs = 11;
        profile.raw_clock.residual_p95_us = 2_100;
        args.raw_clock_warmup_secs = Some(12);
        args.raw_clock_residual_p95_us = Some(2_200);

        let settings = SvsRawClockSettings::resolve(&args, &profile).unwrap();

        assert_eq!(settings.warmup_secs, 12);
        assert_eq!(settings.residual_p95_us, 2_200);
    }

    #[test]
    fn raw_clock_settings_profile_overrides_builtin_defaults() {
        let args = args_for_raw_clock_resolve_tests();
        let mut profile = EffectiveProfile::default_for_tests();
        profile.raw_clock.warmup_secs = 17;
        profile.raw_clock.alignment_lag_us = 7_000;

        let settings = SvsRawClockSettings::resolve(&args, &profile).unwrap();

        assert_eq!(settings.warmup_secs, 17);
        assert_eq!(settings.alignment_lag_us, 7_000);
    }

    #[test]
    fn raw_clock_runtime_kind_requires_explicit_cli_opt_in() {
        let mut args = args_for_raw_clock_resolve_tests();
        args.experimental_calibrated_raw = false;

        assert_eq!(
            SvsRuntimeKind::from_args(&args),
            SvsRuntimeKind::StrictRealtime
        );

        args.experimental_calibrated_raw = true;

        assert_eq!(
            SvsRuntimeKind::from_args(&args),
            SvsRuntimeKind::CalibratedRawClock
        );
    }

    #[test]
    fn raw_clock_settings_reject_invalid_resolved_values() {
        let mut args = args_for_raw_clock_resolve_tests();
        let profile = EffectiveProfile::default_for_tests();
        args.raw_clock_residual_p95_us = Some(0);

        let err = SvsRawClockSettings::resolve(&args, &profile).unwrap_err();

        assert!(err.to_string().contains("residual_p95_us"));
    }

    #[test]
    fn raw_clock_settings_reject_alignment_lag_at_estimator_last_sample_age() {
        let mut args = args_for_raw_clock_resolve_tests();
        let profile = EffectiveProfile::default_for_tests();
        args.raw_clock_last_sample_age_ms = Some(20);
        args.raw_clock_selected_sample_age_ms = Some(50);
        args.raw_clock_alignment_lag_us = Some(20_000);

        let err = SvsRawClockSettings::resolve(&args, &profile).unwrap_err();

        assert!(err.to_string().contains("last_sample_age_ms"));
    }

    #[test]
    fn raw_clock_settings_conversion_copies_runtime_threshold_fields() {
        let mut args = args_for_raw_clock_resolve_tests();
        args.raw_clock_warmup_secs = Some(12);
        args.raw_clock_residual_p95_us = Some(2_100);
        args.raw_clock_residual_max_us = Some(3_200);
        args.raw_clock_drift_abs_ppm = Some(750.0);
        args.raw_clock_sample_gap_max_ms = Some(60);
        args.raw_clock_last_sample_age_ms = Some(25);
        args.raw_clock_selected_sample_age_ms = Some(55);
        args.raw_clock_inter_arm_skew_max_us = Some(22_000);
        args.raw_clock_state_skew_max_us = Some(11_000);
        args.raw_clock_residual_max_consecutive_failures = Some(4);
        args.raw_clock_alignment_lag_us = Some(6_000);
        args.raw_clock_alignment_search_window_us = Some(26_000);
        args.raw_clock_alignment_buffer_miss_consecutive_failures = Some(5);
        let profile = EffectiveProfile::default_for_tests();

        let settings = SvsRawClockSettings::resolve(&args, &profile).unwrap();
        let cfg = settings.to_experimental_config(100.0, Some(123));
        let read_policy = settings.read_policy();

        assert_eq!(cfg.frequency_hz, 100.0);
        assert_eq!(cfg.max_iterations, Some(123));
        assert_eq!(cfg.estimator_thresholds.warmup_window_us, 12_000_000);
        assert_eq!(cfg.estimator_thresholds.residual_p95_us, 2_100);
        assert_eq!(cfg.estimator_thresholds.residual_max_us, 3_200);
        assert_eq!(cfg.estimator_thresholds.drift_abs_ppm, 750.0);
        assert_eq!(cfg.estimator_thresholds.sample_gap_max_us, 60_000);
        assert_eq!(cfg.estimator_thresholds.last_sample_age_us, 25_000);
        assert_eq!(cfg.thresholds.inter_arm_skew_max_us, 22_000);
        assert_eq!(cfg.thresholds.last_sample_age_us, 25_000);
        assert_eq!(cfg.thresholds.selected_sample_age_us, 55_000);
        assert_eq!(cfg.thresholds.residual_max_consecutive_failures, 4);
        assert_eq!(cfg.thresholds.alignment_lag_us, 6_000);
        assert_eq!(cfg.thresholds.alignment_search_window_us, 26_000);
        assert_eq!(cfg.thresholds.alignment_buffer_miss_consecutive_failures, 5);
        assert_eq!(read_policy.max_state_skew_us, 11_000);
        assert_eq!(read_policy.max_feedback_age, Duration::from_millis(25));
    }

    #[test]
    fn raw_clock_run_config_copies_loop_config_and_disables_gripper_mirror() {
        let args = args_for_raw_clock_resolve_tests();
        let profile = EffectiveProfile::default_for_tests();
        let settings = SvsRawClockSettings::resolve(&args, &profile).unwrap();
        let cancel_signal = Arc::new(AtomicBool::new(false));
        let telemetry_sink: Arc<dyn BilateralLoopTelemetrySink> = Arc::new(NoopTelemetrySink);
        let loop_config = BilateralLoopConfig {
            dt_clamp_multiplier: 3.5,
            cancel_signal: Some(cancel_signal.clone()),
            telemetry_sink: Some(telemetry_sink.clone()),
            disable_config: piper_client::state::DisableConfig {
                timeout: Duration::from_millis(123),
                debounce_threshold: 7,
                poll_interval: Duration::from_millis(8),
            },
            gripper: GripperTeleopConfig {
                enabled: true,
                update_divider: 9,
                position_deadband: 0.03,
                effort_scale: 1.25,
                max_feedback_age: Duration::from_millis(250),
            },
            master_interaction_lpf_cutoff_hz: 11.0,
            master_interaction_limit: JointArray::splat(NewtonMeter(1.2)),
            slave_feedforward_limit: JointArray::splat(NewtonMeter(3.4)),
            master_interaction_slew_limit_nm_per_s: JointArray::splat(NewtonMeter(5.6)),
            master_passivity_enabled: false,
            master_passivity_max_damping: JointArray::splat(0.7),
            ..BilateralLoopConfig::default()
        };

        let run_config = settings.to_run_config(&loop_config);
        let shaping =
            run_config.output_shaping.expect("raw-clock SVS should enable output shaping");

        assert_eq!(run_config.read_policy, settings.read_policy());
        assert_eq!(run_config.command_timeout, Duration::from_millis(20));
        assert_eq!(
            run_config.disable_config.timeout,
            loop_config.disable_config.timeout
        );
        assert_eq!(
            run_config.disable_config.debounce_threshold,
            loop_config.disable_config.debounce_threshold
        );
        assert_eq!(
            run_config.disable_config.poll_interval,
            loop_config.disable_config.poll_interval
        );
        assert!(Arc::ptr_eq(
            run_config.cancel_signal.as_ref().unwrap(),
            &cancel_signal
        ));
        assert_eq!(run_config.dt_clamp_multiplier, 3.5);
        assert!(Arc::ptr_eq(
            run_config.telemetry_sink.as_ref().unwrap(),
            &telemetry_sink
        ));
        assert!(!run_config.gripper.enabled);
        assert_eq!(
            run_config.gripper.max_feedback_age,
            loop_config.gripper.max_feedback_age
        );
        assert_eq!(
            run_config.gripper.update_divider,
            loop_config.gripper.update_divider
        );
        assert_eq!(
            shaping,
            BilateralOutputShapingConfig::from_loop_config(&loop_config)
        );
    }
}
