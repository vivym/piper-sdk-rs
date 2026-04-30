use std::path::PathBuf;

use clap::Parser;

#[derive(Debug, Parser)]
#[command(name = "piper-svs-collect")]
pub struct Args {
    #[arg(long)]
    pub master_target: String,
    #[arg(long)]
    pub slave_target: String,
    #[arg(long)]
    pub baud_rate: Option<u32>,
    #[arg(long)]
    pub model_dir: Option<PathBuf>,
    #[arg(long)]
    pub use_standard_model_path: bool,
    #[arg(long)]
    pub use_embedded_model: bool,
    #[arg(long)]
    pub task_profile: Option<PathBuf>,
    #[arg(long)]
    pub output_dir: PathBuf,
    #[arg(long)]
    pub calibration_file: Option<PathBuf>,
    #[arg(long)]
    pub save_calibration: Option<PathBuf>,
    #[arg(long)]
    pub calibration_max_error_rad: Option<f64>,
    #[arg(long)]
    pub mirror_map: Option<String>,
    #[arg(long)]
    pub operator: Option<String>,
    #[arg(long)]
    pub task: Option<String>,
    #[arg(long)]
    pub notes: Option<String>,
    #[arg(long)]
    pub raw_can: bool,
    #[arg(long)]
    pub disable_gripper_mirror: bool,
    #[arg(long)]
    pub max_iterations: Option<u64>,
    #[arg(long, default_value = "spin")]
    pub timing_mode: String,
    #[arg(long)]
    pub yes: bool,
    #[arg(long)]
    pub experimental_calibrated_raw: bool,
    #[arg(long)]
    pub raw_clock_warmup_secs: Option<u64>,
    #[arg(long)]
    pub raw_clock_residual_p95_us: Option<u64>,
    #[arg(long)]
    pub raw_clock_residual_max_us: Option<u64>,
    #[arg(long)]
    pub raw_clock_drift_abs_ppm: Option<f64>,
    #[arg(long)]
    pub raw_clock_sample_gap_max_ms: Option<u64>,
    #[arg(long)]
    pub raw_clock_last_sample_age_ms: Option<u64>,
    #[arg(long)]
    pub raw_clock_selected_sample_age_ms: Option<u64>,
    #[arg(long)]
    pub raw_clock_inter_arm_skew_max_us: Option<u64>,
    #[arg(long)]
    pub raw_clock_state_skew_max_us: Option<u64>,
    #[arg(long)]
    pub raw_clock_residual_max_consecutive_failures: Option<u32>,
    #[arg(long)]
    pub raw_clock_alignment_lag_us: Option<u64>,
    #[arg(long)]
    pub raw_clock_alignment_search_window_us: Option<u64>,
    #[arg(long)]
    pub raw_clock_alignment_buffer_miss_consecutive_failures: Option<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use std::path::{Path, PathBuf};

    #[test]
    fn parses_required_svs_collect_args() {
        let args = Args::try_parse_from([
            "piper-svs-collect",
            "--master-target",
            "socketcan:can0",
            "--slave-target",
            "socketcan:can1",
            "--model-dir",
            "/tmp/model",
            "--output-dir",
            "/tmp/out",
            "--task",
            "wiping",
            "--operator",
            "viv",
            "--experimental-calibrated-raw",
            "--raw-clock-warmup-secs",
            "12",
            "--raw-clock-residual-p95-us",
            "2100",
            "--raw-clock-residual-max-us",
            "3200",
            "--raw-clock-drift-abs-ppm",
            "750",
            "--raw-clock-residual-max-consecutive-failures",
            "4",
            "--raw-clock-sample-gap-max-ms",
            "60",
            "--raw-clock-last-sample-age-ms",
            "25",
            "--raw-clock-selected-sample-age-ms",
            "55",
            "--raw-clock-inter-arm-skew-max-us",
            "22000",
            "--raw-clock-state-skew-max-us",
            "11000",
            "--raw-clock-alignment-lag-us",
            "6000",
            "--raw-clock-alignment-search-window-us",
            "26000",
            "--raw-clock-alignment-buffer-miss-consecutive-failures",
            "5",
            "--yes",
        ])
        .expect("args should parse");

        assert_eq!(args.master_target, "socketcan:can0");
        assert_eq!(args.slave_target, "socketcan:can1");
        assert_eq!(args.model_dir.as_deref(), Some(Path::new("/tmp/model")));
        assert_eq!(args.output_dir, PathBuf::from("/tmp/out"));
        assert_eq!(args.operator.as_deref(), Some("viv"));
        assert_eq!(args.task.as_deref(), Some("wiping"));
        assert_eq!(args.timing_mode, "spin");
        assert!(args.experimental_calibrated_raw);
        assert_eq!(args.raw_clock_warmup_secs, Some(12));
        assert_eq!(args.raw_clock_residual_p95_us, Some(2100));
        assert_eq!(args.raw_clock_residual_max_us, Some(3200));
        assert_eq!(args.raw_clock_drift_abs_ppm, Some(750.0));
        assert_eq!(args.raw_clock_residual_max_consecutive_failures, Some(4));
        assert_eq!(args.raw_clock_sample_gap_max_ms, Some(60));
        assert_eq!(args.raw_clock_last_sample_age_ms, Some(25));
        assert_eq!(args.raw_clock_selected_sample_age_ms, Some(55));
        assert_eq!(args.raw_clock_inter_arm_skew_max_us, Some(22_000));
        assert_eq!(args.raw_clock_state_skew_max_us, Some(11_000));
        assert_eq!(args.raw_clock_alignment_lag_us, Some(6_000));
        assert_eq!(args.raw_clock_alignment_search_window_us, Some(26_000));
        assert_eq!(
            args.raw_clock_alignment_buffer_miss_consecutive_failures,
            Some(5)
        );
        assert!(args.yes);
    }
}
