use std::time::Duration;

use anyhow::{Result, bail};
use piper_client::dual_arm::{
    BilateralLoopConfig, BilateralOutputShapingConfig, GripperTeleopConfig,
};
use piper_client::dual_arm_raw_clock::{
    ExperimentalRawClockConfig, ExperimentalRawClockMode, ExperimentalRawClockRunConfig,
    RawClockRuntimeExitReason, RawClockRuntimeReport, RawClockRuntimeThresholds,
};
use piper_client::observer::ControlReadPolicy;
use piper_tools::raw_clock::RawClockThresholds;

use crate::args::Args;
use crate::episode::manifest::{RawClockManifest, RawClockReportJson};
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
            warmup_cycles: loop_config.warmup_cycles,
            safety: loop_config.safety.clone(),
            telemetry_sink: loop_config.telemetry_sink.clone(),
            gripper: GripperTeleopConfig {
                enabled: false,
                ..loop_config.gripper
            },
            output_shaping: Some(BilateralOutputShapingConfig::from_loop_config(loop_config)),
        }
    }

    pub fn to_manifest(&self) -> RawClockManifest {
        RawClockManifest {
            timing_source: "calibrated_hw_raw".to_string(),
            strict_realtime: false,
            experimental: true,
            warmup_secs: self.warmup_secs,
            residual_p95_us: self.residual_p95_us,
            residual_max_us: self.residual_max_us,
            drift_abs_ppm: self.drift_abs_ppm,
            sample_gap_max_ms: self.sample_gap_max_ms,
            last_sample_age_ms: self.last_sample_age_ms,
            selected_sample_age_ms: self.selected_sample_age_ms,
            inter_arm_skew_max_us: self.inter_arm_skew_max_us,
            state_skew_max_us: self.state_skew_max_us,
            residual_max_consecutive_failures: self.residual_max_consecutive_failures,
            alignment_lag_us: self.alignment_lag_us,
            alignment_search_window_us: self.alignment_search_window_us,
            alignment_buffer_miss_consecutive_failures: self
                .alignment_buffer_miss_consecutive_failures,
        }
    }
}

pub fn raw_clock_report_json(
    report: &RawClockRuntimeReport,
    settings: &SvsRawClockSettings,
) -> RawClockReportJson {
    RawClockReportJson {
        timing_source: "calibrated_hw_raw".to_string(),
        strict_realtime: false,
        experimental: true,
        warmup_secs: settings.warmup_secs,
        residual_p95_us: settings.residual_p95_us,
        residual_max_us: settings.residual_max_us,
        drift_abs_ppm: settings.drift_abs_ppm,
        sample_gap_max_ms: settings.sample_gap_max_ms,
        last_sample_age_ms: settings.last_sample_age_ms,
        selected_sample_age_ms: settings.selected_sample_age_ms,
        inter_arm_skew_max_us: settings.inter_arm_skew_max_us,
        state_skew_max_us: settings.state_skew_max_us,
        residual_max_consecutive_failures: settings.residual_max_consecutive_failures,
        alignment_buffer_miss_consecutive_failure_threshold: settings
            .alignment_buffer_miss_consecutive_failures,
        master_clock_drift_ppm: report.master.drift_ppm,
        slave_clock_drift_ppm: report.slave.drift_ppm,
        master_residual_p95_us: report.master.residual_p95_us,
        slave_residual_p95_us: report.slave.residual_p95_us,
        selected_inter_arm_skew_max_us: report.selected_inter_arm_skew_max_us,
        selected_inter_arm_skew_p95_us: report.selected_inter_arm_skew_p95_us,
        latest_inter_arm_skew_max_us: report.latest_inter_arm_skew_max_us,
        latest_inter_arm_skew_p95_us: report.latest_inter_arm_skew_p95_us,
        alignment_lag_us: report.alignment_lag_us,
        alignment_search_window_us: settings.alignment_search_window_us,
        alignment_buffer_misses: report.alignment_buffer_misses,
        alignment_buffer_miss_consecutive_max: report.alignment_buffer_miss_consecutive_max,
        alignment_buffer_miss_consecutive_failures: report
            .alignment_buffer_miss_consecutive_failures,
        master_residual_max_spikes: report.master_residual_max_spikes,
        slave_residual_max_spikes: report.slave_residual_max_spikes,
        master_residual_max_consecutive_failures: report.master_residual_max_consecutive_failures,
        slave_residual_max_consecutive_failures: report.slave_residual_max_consecutive_failures,
        clock_health_failures: report.clock_health_failures,
        read_faults: report.read_faults,
        submission_faults: report.submission_faults,
        runtime_faults: report.runtime_faults,
        compensation_faults: report.compensation_faults,
        controller_faults: report.controller_faults,
        telemetry_sink_faults: report.telemetry_sink_faults,
        final_failure_kind: report.exit_reason.and_then(|reason| match reason {
            RawClockRuntimeExitReason::MaxIterations | RawClockRuntimeExitReason::Cancelled => None,
            other => Some(format!("{other:?}")),
        }),
    }
}

pub fn raw_clock_startup_report_json(
    settings: &SvsRawClockSettings,
    final_failure_kind: Option<String>,
) -> RawClockReportJson {
    RawClockReportJson {
        timing_source: "calibrated_hw_raw".to_string(),
        strict_realtime: false,
        experimental: true,
        warmup_secs: settings.warmup_secs,
        residual_p95_us: settings.residual_p95_us,
        residual_max_us: settings.residual_max_us,
        drift_abs_ppm: settings.drift_abs_ppm,
        sample_gap_max_ms: settings.sample_gap_max_ms,
        last_sample_age_ms: settings.last_sample_age_ms,
        selected_sample_age_ms: settings.selected_sample_age_ms,
        inter_arm_skew_max_us: settings.inter_arm_skew_max_us,
        state_skew_max_us: settings.state_skew_max_us,
        residual_max_consecutive_failures: settings.residual_max_consecutive_failures,
        alignment_buffer_miss_consecutive_failure_threshold: settings
            .alignment_buffer_miss_consecutive_failures,
        master_clock_drift_ppm: 0.0,
        slave_clock_drift_ppm: 0.0,
        master_residual_p95_us: 0,
        slave_residual_p95_us: 0,
        selected_inter_arm_skew_max_us: 0,
        selected_inter_arm_skew_p95_us: 0,
        latest_inter_arm_skew_max_us: 0,
        latest_inter_arm_skew_p95_us: 0,
        alignment_lag_us: settings.alignment_lag_us,
        alignment_search_window_us: settings.alignment_search_window_us,
        alignment_buffer_misses: 0,
        alignment_buffer_miss_consecutive_max: 0,
        alignment_buffer_miss_consecutive_failures: 0,
        master_residual_max_spikes: 0,
        slave_residual_max_spikes: 0,
        master_residual_max_consecutive_failures: 0,
        slave_residual_max_consecutive_failures: 0,
        clock_health_failures: 0,
        read_faults: 0,
        submission_faults: 0,
        runtime_faults: 0,
        compensation_faults: 0,
        controller_faults: 0,
        telemetry_sink_faults: 0,
        final_failure_kind,
    }
}

pub(crate) fn apply_cli_profile_overrides(args: &Args, profile: &mut EffectiveProfile) {
    let raw = &mut profile.raw_clock;
    if let Some(value) = args.raw_clock_warmup_secs {
        raw.warmup_secs = value;
    }
    if let Some(value) = args.raw_clock_residual_p95_us {
        raw.residual_p95_us = value;
    }
    if let Some(value) = args.raw_clock_residual_max_us {
        raw.residual_max_us = value;
    }
    if let Some(value) = args.raw_clock_drift_abs_ppm {
        raw.drift_abs_ppm = value;
    }
    if let Some(value) = args.raw_clock_sample_gap_max_ms {
        raw.sample_gap_max_ms = value;
    }
    if let Some(value) = args.raw_clock_last_sample_age_ms {
        raw.last_sample_age_ms = value;
    }
    if let Some(value) = args.raw_clock_selected_sample_age_ms {
        raw.selected_sample_age_ms = value;
    }
    if let Some(value) = args.raw_clock_inter_arm_skew_max_us {
        raw.inter_arm_skew_max_us = value;
    }
    if let Some(value) = args.raw_clock_state_skew_max_us {
        raw.state_skew_max_us = value;
    }
    if let Some(value) = args.raw_clock_residual_max_consecutive_failures {
        raw.residual_max_consecutive_failures = value;
    }
    if let Some(value) = args.raw_clock_alignment_lag_us {
        raw.alignment_lag_us = value;
    }
    if let Some(value) = args.raw_clock_alignment_search_window_us {
        raw.alignment_search_window_us = value;
    }
    if let Some(value) = args.raw_clock_alignment_buffer_miss_consecutive_failures {
        raw.alignment_buffer_miss_consecutive_failures = value;
    }
}

pub(crate) fn effective_gripper_mirror_enabled(args: &Args, profile: &EffectiveProfile) -> bool {
    profile.gripper.mirror_enabled && !args.disable_gripper_mirror
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
        BilateralOutputShapingConfig, GripperTeleopConfig, StopAttemptResult,
    };
    use piper_client::dual_arm_raw_clock::{RawClockRuntimeExitReason, RawClockRuntimeReport};
    use piper_client::types::{JointArray, NewtonMeter};
    use piper_tools::raw_clock::RawClockHealth;
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
    fn runtime_kind_requires_explicit_raw_clock_opt_in() {
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
            warmup_cycles: 11,
            cancel_signal: Some(cancel_signal.clone()),
            telemetry_sink: Some(telemetry_sink.clone()),
            safety: piper_client::dual_arm::DualArmSafetyConfig {
                safe_hold_kp: JointArray::splat(8.0),
                safe_hold_kd: JointArray::splat(0.6),
                safe_hold_max_duration: Duration::from_millis(345),
                consecutive_read_failures_before_disable: 6,
            },
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
                ..GripperTeleopConfig::default()
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
        assert_eq!(run_config.warmup_cycles, 11);
        assert_eq!(
            run_config.safety.safe_hold_kp,
            loop_config.safety.safe_hold_kp
        );
        assert_eq!(
            run_config.safety.safe_hold_kd,
            loop_config.safety.safe_hold_kd
        );
        assert_eq!(
            run_config.safety.safe_hold_max_duration,
            loop_config.safety.safe_hold_max_duration
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

    #[test]
    fn raw_clock_settings_conversion_copies_manifest_fields() {
        let mut args = args_for_raw_clock_resolve_tests();
        args.raw_clock_warmup_secs = Some(12);
        args.raw_clock_alignment_buffer_miss_consecutive_failures = Some(5);
        let profile = EffectiveProfile::default_for_tests();

        let settings = SvsRawClockSettings::resolve(&args, &profile).unwrap();
        let manifest = settings.to_manifest();

        assert_eq!(manifest.timing_source, "calibrated_hw_raw");
        assert!(!manifest.strict_realtime);
        assert!(manifest.experimental);
        assert_eq!(manifest.warmup_secs, 12);
        assert_eq!(manifest.alignment_buffer_miss_consecutive_failures, 5);
    }

    #[test]
    fn raw_clock_report_json_copies_runtime_counters_and_failure_kind() {
        let args = args_for_raw_clock_resolve_tests();
        let profile = EffectiveProfile::default_for_tests();
        let settings = SvsRawClockSettings::resolve(&args, &profile).unwrap();
        let report = RawClockRuntimeReport {
            master: RawClockHealth {
                healthy: true,
                sample_count: 2_000,
                window_duration_us: 20_000_000,
                drift_ppm: -12.5,
                residual_p50_us: 10,
                residual_p95_us: 111,
                residual_p99_us: 120,
                residual_max_us: 130,
                sample_gap_max_us: 10_000,
                last_sample_age_us: 500,
                raw_timestamp_regressions: 0,
                failure_kind: None,
                reason: None,
            },
            slave: RawClockHealth {
                drift_ppm: 34.5,
                residual_p95_us: 222,
                ..RawClockHealth {
                    healthy: true,
                    sample_count: 2_000,
                    window_duration_us: 20_000_000,
                    drift_ppm: 0.0,
                    residual_p50_us: 10,
                    residual_p95_us: 100,
                    residual_p99_us: 120,
                    residual_max_us: 130,
                    sample_gap_max_us: 10_000,
                    last_sample_age_us: 500,
                    raw_timestamp_regressions: 0,
                    failure_kind: None,
                    reason: None,
                }
            },
            joint_motion: None,
            torque_diagnostics: None,
            max_inter_arm_skew_us: 4_321,
            inter_arm_skew_p95_us: 2_345,
            alignment_lag_us: 5_000,
            latest_inter_arm_skew_max_us: 4_000,
            latest_inter_arm_skew_p95_us: 2_000,
            selected_inter_arm_skew_max_us: 4_321,
            selected_inter_arm_skew_p95_us: 2_345,
            clock_health_failures: 7,
            compensation_faults: 1,
            controller_faults: 2,
            telemetry_sink_faults: 3,
            alignment_buffer_misses: 11,
            alignment_buffer_miss_consecutive_max: 4,
            alignment_buffer_miss_consecutive_failures: 5,
            master_residual_max_spikes: 6,
            slave_residual_max_spikes: 8,
            master_residual_max_consecutive_failures: 2,
            slave_residual_max_consecutive_failures: 3,
            read_faults: 9,
            submission_faults: 10,
            last_submission_failed_side: None,
            peer_command_may_have_applied: false,
            runtime_faults: 12,
            master_tx_realtime_overwrites_total: 0,
            slave_tx_realtime_overwrites_total: 0,
            master_tx_frames_sent_total: 100,
            slave_tx_frames_sent_total: 100,
            master_tx_fault_aborts_total: 0,
            slave_tx_fault_aborts_total: 0,
            last_runtime_fault_master: None,
            last_runtime_fault_slave: None,
            iterations: 100,
            exit_reason: Some(RawClockRuntimeExitReason::TelemetrySinkFault),
            master_stop_attempt: StopAttemptResult::NotAttempted,
            slave_stop_attempt: StopAttemptResult::NotAttempted,
            last_error: Some("sink failed".to_string()),
        };

        let json = raw_clock_report_json(&report, &settings);

        assert_eq!(json.master_clock_drift_ppm, -12.5);
        assert_eq!(json.slave_clock_drift_ppm, 34.5);
        assert_eq!(json.master_residual_p95_us, 111);
        assert_eq!(json.slave_residual_p95_us, 222);
        assert_eq!(json.selected_inter_arm_skew_max_us, 4_321);
        assert_eq!(json.latest_inter_arm_skew_p95_us, 2_000);
        assert_eq!(json.alignment_buffer_misses, 11);
        assert_eq!(json.alignment_buffer_miss_consecutive_max, 4);
        assert_eq!(json.alignment_buffer_miss_consecutive_failures, 5);
        assert_eq!(json.compensation_faults, 1);
        assert_eq!(json.controller_faults, 2);
        assert_eq!(json.telemetry_sink_faults, 3);
        assert_eq!(json.clock_health_failures, 7);
        assert_eq!(
            json.final_failure_kind.as_deref(),
            Some("TelemetrySinkFault")
        );
    }

    #[test]
    fn raw_clock_report_json_omits_failure_kind_for_clean_max_iterations() {
        let args = args_for_raw_clock_resolve_tests();
        let profile = EffectiveProfile::default_for_tests();
        let settings = SvsRawClockSettings::resolve(&args, &profile).unwrap();
        let report = raw_clock_report_for_tests(RawClockRuntimeExitReason::MaxIterations);

        let json = raw_clock_report_json(&report, &settings);
        let serialized = serde_json::to_string(&json).unwrap();

        assert!(json.final_failure_kind.is_none());
        assert!(
            !serialized.contains("final_failure_kind"),
            "clean raw-clock report should omit final_failure_kind instead of serializing null: {serialized}"
        );
    }

    #[test]
    fn raw_clock_report_json_omits_failure_kind_for_cancelled() {
        let args = args_for_raw_clock_resolve_tests();
        let profile = EffectiveProfile::default_for_tests();
        let settings = SvsRawClockSettings::resolve(&args, &profile).unwrap();
        let report = raw_clock_report_for_tests(RawClockRuntimeExitReason::Cancelled);

        let json = raw_clock_report_json(&report, &settings);
        let serialized = serde_json::to_string(&json).unwrap();

        assert!(json.final_failure_kind.is_none());
        assert!(
            !serialized.contains("final_failure_kind"),
            "cancelled raw-clock report should omit final_failure_kind instead of serializing null: {serialized}"
        );
    }

    fn raw_clock_report_for_tests(exit_reason: RawClockRuntimeExitReason) -> RawClockRuntimeReport {
        fn health() -> RawClockHealth {
            RawClockHealth {
                healthy: true,
                sample_count: 2_000,
                window_duration_us: 20_000_000,
                drift_ppm: 0.0,
                residual_p50_us: 0,
                residual_p95_us: 0,
                residual_p99_us: 0,
                residual_max_us: 0,
                sample_gap_max_us: 10_000,
                last_sample_age_us: 1_000,
                raw_timestamp_regressions: 0,
                failure_kind: None,
                reason: None,
            }
        }

        RawClockRuntimeReport {
            master: health(),
            slave: health(),
            joint_motion: None,
            torque_diagnostics: None,
            max_inter_arm_skew_us: 0,
            inter_arm_skew_p95_us: 0,
            alignment_lag_us: 0,
            latest_inter_arm_skew_max_us: 0,
            latest_inter_arm_skew_p95_us: 0,
            selected_inter_arm_skew_max_us: 0,
            selected_inter_arm_skew_p95_us: 0,
            clock_health_failures: 0,
            compensation_faults: 0,
            controller_faults: 0,
            telemetry_sink_faults: 0,
            alignment_buffer_misses: 0,
            alignment_buffer_miss_consecutive_max: 0,
            alignment_buffer_miss_consecutive_failures: 0,
            master_residual_max_spikes: 0,
            slave_residual_max_spikes: 0,
            master_residual_max_consecutive_failures: 0,
            slave_residual_max_consecutive_failures: 0,
            read_faults: 0,
            submission_faults: 0,
            last_submission_failed_side: None,
            peer_command_may_have_applied: false,
            runtime_faults: 0,
            master_tx_realtime_overwrites_total: 0,
            slave_tx_realtime_overwrites_total: 0,
            master_tx_frames_sent_total: 0,
            slave_tx_frames_sent_total: 0,
            master_tx_fault_aborts_total: 0,
            slave_tx_fault_aborts_total: 0,
            last_runtime_fault_master: None,
            last_runtime_fault_slave: None,
            iterations: 0,
            exit_reason: Some(exit_reason),
            master_stop_attempt: StopAttemptResult::NotAttempted,
            slave_stop_attempt: StopAttemptResult::NotAttempted,
            last_error: None,
        }
    }
}
