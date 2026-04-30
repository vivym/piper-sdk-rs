use anyhow::Result;
use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(name = "gravity")]
pub struct GravityCommand {
    #[command(subcommand)]
    pub action: GravityAction,
}

#[derive(Debug, Subcommand)]
pub enum GravityAction {
    RecordPath(GravityRecordPathArgs),
    ReplaySample(GravityReplaySampleArgs),
    Fit(GravityFitArgs),
    Eval(GravityEvalArgs),
}

#[derive(Debug, Args, Clone)]
pub struct GravityRecordPathArgs {
    #[arg(long)]
    pub role: String,
    #[arg(long)]
    pub target: Option<String>,
    #[arg(long)]
    pub interface: Option<String>,
    #[arg(long)]
    pub joint_map: String,
    #[arg(long)]
    pub load_profile: String,
    #[arg(long)]
    pub out: PathBuf,
    #[arg(long, default_value_t = 50.0)]
    pub frequency_hz: f64,
    #[arg(long)]
    pub notes: Option<String>,
}

#[derive(Debug, Args, Clone)]
pub struct GravityReplaySampleArgs {
    #[arg(long)]
    pub role: String,
    #[arg(long)]
    pub target: Option<String>,
    #[arg(long)]
    pub interface: Option<String>,
    #[arg(long)]
    pub path: PathBuf,
    #[arg(long)]
    pub out: PathBuf,
    #[arg(long, default_value_t = 0.08)]
    pub max_velocity_rad_s: f64,
    #[arg(long, default_value_t = 0.02)]
    pub max_step_rad: f64,
    #[arg(long, default_value_t = 500)]
    pub settle_ms: u64,
    #[arg(long, default_value_t = 300)]
    pub sample_ms: u64,
    #[arg(long = "no-bidirectional", action = clap::ArgAction::SetFalse, default_value_t = true)]
    pub bidirectional: bool,
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, Args, Clone)]
pub struct GravityFitArgs {
    #[arg(long, required = true)]
    pub samples: Vec<PathBuf>,
    #[arg(long)]
    pub out: PathBuf,
    #[arg(long, default_value = crate::gravity::BASIS_TRIG_V1)]
    pub basis: Option<String>,
    #[arg(long, default_value_t = 1e-4)]
    pub ridge_lambda: f64,
    #[arg(long, default_value_t = 0.2)]
    pub holdout_ratio: f64,
}

#[derive(Debug, Args, Clone)]
pub struct GravityEvalArgs {
    #[arg(long)]
    pub model: PathBuf,
    #[arg(long, required = true)]
    pub samples: Vec<PathBuf>,
}

impl GravityCommand {
    pub async fn execute(self) -> Result<()> {
        match self.action {
            GravityAction::RecordPath(args) => crate::gravity::record_path::run(args).await,
            GravityAction::ReplaySample(args) => crate::gravity::replay_sample::run(args).await,
            GravityAction::Fit(args) => crate::gravity::fit::run(args),
            GravityAction::Eval(args) => crate::gravity::eval::run(args),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn gravity_fit_command_parses_sample_and_output_paths() {
        let cmd = GravityCommand::try_parse_from([
            "gravity",
            "fit",
            "--samples",
            "artifacts/gravity/slave.samples.jsonl",
            "--out",
            "artifacts/gravity/slave.model.toml",
        ])
        .expect("gravity fit command should parse");

        match cmd.action {
            GravityAction::Fit(args) => {
                assert_eq!(args.samples.len(), 1);
                assert_eq!(
                    args.samples[0],
                    PathBuf::from("artifacts/gravity/slave.samples.jsonl")
                );
                assert_eq!(
                    args.out,
                    PathBuf::from("artifacts/gravity/slave.model.toml")
                );
                assert_eq!(args.basis.as_deref(), Some("trig-v1"));
                assert_eq!(args.ridge_lambda, 1e-4);
            },
            _ => panic!("expected fit action"),
        }
    }

    #[test]
    fn gravity_record_path_parses_frequency_and_notes() {
        let cmd = GravityCommand::try_parse_from([
            "gravity",
            "record-path",
            "--role",
            "slave",
            "--target",
            "socketcan:can0",
            "--joint-map",
            "identity",
            "--load-profile",
            "normal-gripper-d405",
            "--out",
            "artifacts/gravity/slave.path.jsonl",
            "--frequency-hz",
            "25.0",
            "--notes",
            "operator note",
        ])
        .expect("gravity record-path command should parse");

        match cmd.action {
            GravityAction::RecordPath(args) => {
                assert_eq!(args.frequency_hz, 25.0);
                assert_eq!(args.notes.as_deref(), Some("operator note"));
            },
            _ => panic!("expected record-path action"),
        }
    }
}
