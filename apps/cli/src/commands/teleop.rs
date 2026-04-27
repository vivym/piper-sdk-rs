use anyhow::Result;
use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(name = "teleop")]
pub struct TeleopCommand {
    #[command(subcommand)]
    pub action: TeleopAction,
}

#[derive(Debug, Subcommand)]
pub enum TeleopAction {
    DualArm(TeleopDualArmArgs),
}

#[derive(Debug, Args, Clone)]
#[command(
    about = "Run dual-arm isomorphic teleoperation",
    long_about = "Run dual-arm isomorphic teleoperation.\n\nv1 runtime support is StrictRealtime SocketCAN on Linux. GS-USB concrete target syntax is parsed for configuration compatibility, but command execution requires future SDK SoftRealtime dual-arm support and is rejected before hardware connect."
)]
pub struct TeleopDualArmArgs {
    #[arg(long)]
    pub config: Option<PathBuf>,
    #[arg(long)]
    pub master_target: Option<String>,
    #[arg(long)]
    pub slave_target: Option<String>,
    #[arg(long)]
    pub master_interface: Option<String>,
    #[arg(long)]
    pub slave_interface: Option<String>,
    #[arg(long)]
    pub master_serial: Option<String>,
    #[arg(long)]
    pub slave_serial: Option<String>,
    #[arg(long)]
    pub master_gs_usb_bus_address: Option<String>,
    #[arg(long)]
    pub slave_gs_usb_bus_address: Option<String>,
    #[arg(long, default_value_t = 1_000_000)]
    pub baud_rate: u32,
    #[arg(long, value_enum)]
    pub mode: Option<crate::teleop::config::TeleopMode>,
    #[arg(long, value_enum)]
    pub profile: Option<crate::teleop::config::TeleopProfile>,
    #[arg(long)]
    pub frequency_hz: Option<f64>,
    #[arg(long)]
    pub track_kp: Option<f64>,
    #[arg(long)]
    pub track_kd: Option<f64>,
    #[arg(long)]
    pub master_damping: Option<f64>,
    #[arg(long)]
    pub reflection_gain: Option<f64>,
    #[arg(long)]
    pub disable_gripper_mirror: bool,
    #[arg(long)]
    pub calibration_file: Option<PathBuf>,
    #[arg(long)]
    pub calibration_max_error_rad: Option<f64>,
    #[arg(long)]
    pub save_calibration: Option<PathBuf>,
    #[arg(long)]
    pub report_json: Option<PathBuf>,
    #[arg(long)]
    pub yes: bool,
    #[arg(long)]
    pub max_iterations: Option<usize>,
    #[arg(long, value_enum)]
    pub timing_mode: Option<crate::teleop::config::TeleopTimingMode>,
}

impl TeleopCommand {
    pub async fn execute(self) -> Result<()> {
        match self.action {
            TeleopAction::DualArm(args) => crate::teleop::workflow::run_dual_arm(args).await,
        }
    }
}

#[cfg(test)]
impl TeleopDualArmArgs {
    pub fn default_for_tests() -> Self {
        Self {
            config: None,
            master_target: None,
            slave_target: None,
            master_interface: None,
            slave_interface: None,
            master_serial: None,
            slave_serial: None,
            master_gs_usb_bus_address: None,
            slave_gs_usb_bus_address: None,
            baud_rate: 1_000_000,
            mode: None,
            profile: None,
            frequency_hz: None,
            track_kp: None,
            track_kd: None,
            master_damping: None,
            reflection_gain: None,
            disable_gripper_mirror: false,
            calibration_file: None,
            calibration_max_error_rad: None,
            save_calibration: None,
            report_json: None,
            yes: false,
            max_iterations: None,
            timing_mode: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn dual_arm_command_parses_socketcan_targets() {
        let cmd = TeleopCommand::try_parse_from([
            "teleop",
            "dual-arm",
            "--master-interface",
            "can0",
            "--slave-interface",
            "can1",
            "--mode",
            "master-follower",
        ])
        .expect("teleop dual-arm command should parse");

        match cmd.action {
            TeleopAction::DualArm(args) => {
                assert_eq!(args.master_interface.as_deref(), Some("can0"));
                assert_eq!(args.slave_interface.as_deref(), Some("can1"));
                assert_eq!(
                    args.mode,
                    Some(crate::teleop::config::TeleopMode::MasterFollower)
                );
            },
        }
    }

    #[test]
    fn dual_arm_command_parses_canonical_targets() {
        let cmd = TeleopCommand::try_parse_from([
            "teleop",
            "dual-arm",
            "--master-target",
            "socketcan:can0",
            "--slave-target",
            "socketcan:can1",
        ])
        .expect("canonical targets should parse");

        match cmd.action {
            TeleopAction::DualArm(args) => {
                assert_eq!(args.master_target.as_deref(), Some("socketcan:can0"));
                assert_eq!(args.slave_target.as_deref(), Some("socketcan:can1"));
            },
        }
    }

    #[test]
    fn dual_arm_help_mentions_realtime_runtime_and_key_options() {
        let err = TeleopCommand::try_parse_from(["teleop", "dual-arm", "--help"])
            .expect_err("--help should return clap help");
        let help = err.to_string();

        assert!(help.contains("StrictRealtime"));
        assert!(help.contains("GS-USB"));
        assert!(help.contains("--master-target"));
        assert!(help.contains("--slave-target"));
        assert!(help.contains("--mode"));
        assert!(help.contains("--profile"));
        assert!(help.contains("--calibration-file"));
        assert!(help.contains("--report-json"));
    }
}
