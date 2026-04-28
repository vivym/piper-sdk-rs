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
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

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
            "--yes",
        ])
        .expect("args should parse");

        assert_eq!(args.master_target, "socketcan:can0");
        assert_eq!(args.slave_target, "socketcan:can1");
        assert_eq!(args.task.as_deref(), Some("wiping"));
        assert!(args.yes);
    }
}
