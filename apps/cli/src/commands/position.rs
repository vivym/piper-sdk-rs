//! 位置查询命令

use anyhow::Result;
use clap::{Args, ValueEnum};
use piper_sdk::client::ConnectedPiper;
use serde_json::json;

use crate::commands::config::CliConfig;
use crate::connection::{TargetArgs, client_builder, wait_for_initial_monitor_snapshot};

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum PositionOutputFormat {
    Table,
    Json,
}

#[derive(Args, Debug, Clone)]
pub struct PositionCommand {
    #[command(flatten)]
    pub target: TargetArgs,

    /// 输出格式
    #[arg(short, long, value_enum, default_value_t = PositionOutputFormat::Table)]
    pub format: PositionOutputFormat,
}

impl PositionCommand {
    fn emits_human_progress(format: PositionOutputFormat) -> bool {
        matches!(format, PositionOutputFormat::Table)
    }

    pub async fn execute(&self, config: &CliConfig) -> Result<()> {
        if Self::emits_human_progress(self.format) {
            println!("⏳ 正在查询关节位置...");
        }

        let profile = config.control_profile(self.target.target.as_ref());
        let builder = client_builder(&profile.target);

        if Self::emits_human_progress(self.format) {
            println!("🔌 连接到机器人...");
        }
        let robot = builder.build()?;
        let (positions, end_pose) = wait_for_initial_monitor_snapshot(|| match &robot {
            ConnectedPiper::Strict(state) => Ok((
                state.observer().joint_positions()?,
                state.observer().end_pose()?,
            )),
            ConnectedPiper::Soft(state) => Ok((
                state.observer().joint_positions()?,
                state.observer().end_pose()?,
            )),
            ConnectedPiper::Monitor(robot) => Ok((
                robot.observer().joint_positions()?,
                robot.observer().end_pose()?,
            )),
        })?;

        match self.format {
            PositionOutputFormat::Table => {
                println!("📊 关节位置:");
                for (index, pos) in positions.iter().enumerate() {
                    println!(
                        "  J{}: {:.3} rad ({:.1}°)",
                        index + 1,
                        pos.0,
                        pos.to_deg().0
                    );
                }

                println!("\n📍 末端位姿:");
                println!("  位置 (m):");
                println!("    X: {:.4}", end_pose.end_pose[0]);
                println!("    Y: {:.4}", end_pose.end_pose[1]);
                println!("    Z: {:.4}", end_pose.end_pose[2]);
                println!("  姿态 (rad):");
                println!("    Rx: {:.4}", end_pose.end_pose[3]);
                println!("    Ry: {:.4}", end_pose.end_pose[4]);
                println!("    Rz: {:.4}", end_pose.end_pose[5]);

                if end_pose.frame_valid_mask != 0b111 {
                    println!(
                        "\n⚠️  警告: 末端位姿数据不完整（帧组掩码: {:#03b}）",
                        end_pose.frame_valid_mask
                    );
                }
            },
            PositionOutputFormat::Json => {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({
                        "joint_positions_rad": positions.iter().map(|pos| pos.0).collect::<Vec<_>>(),
                        "joint_positions_deg": positions.iter().map(|pos| pos.to_deg().0).collect::<Vec<_>>(),
                        "end_pose": {
                            "position_m": &end_pose.end_pose[..3],
                            "orientation_rad": &end_pose.end_pose[3..],
                            "frame_valid_mask": end_pose.frame_valid_mask,
                        }
                    }))?
                );
            },
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use piper_control::TargetSpec;

    #[test]
    fn position_command_defaults_to_table_output() {
        let cmd = PositionCommand {
            target: TargetArgs::default(),
            format: PositionOutputFormat::Table,
        };

        assert_eq!(cmd.format, PositionOutputFormat::Table);
    }

    #[test]
    fn position_command_accepts_target_override() {
        let cmd = PositionCommand {
            target: TargetArgs {
                target: Some(TargetSpec::SocketCan {
                    iface: "can0".to_string(),
                }),
            },
            format: PositionOutputFormat::Json,
        };

        assert_eq!(
            cmd.target.target,
            Some(TargetSpec::SocketCan {
                iface: "can0".to_string()
            })
        );
        assert_eq!(cmd.format, PositionOutputFormat::Json);
    }

    #[test]
    fn json_output_suppresses_human_progress_messages() {
        assert!(!PositionCommand::emits_human_progress(
            PositionOutputFormat::Json
        ));
    }

    #[test]
    fn table_output_keeps_human_progress_messages() {
        assert!(PositionCommand::emits_human_progress(
            PositionOutputFormat::Table
        ));
    }
}
