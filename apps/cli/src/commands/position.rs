//! 位置查询命令

use anyhow::Result;
use clap::Args;
use piper_sdk::client::ControlReadPolicy;

use crate::commands::config::CliConfig;
use crate::connection::{TargetArgs, client_builder};

#[derive(Args, Debug, Clone)]
pub struct PositionCommand {
    #[command(flatten)]
    pub target: TargetArgs,

    /// 输出格式
    #[arg(short, long, default_value = "table")]
    pub format: String,
}

impl PositionCommand {
    pub async fn execute(&self, config: &CliConfig) -> Result<()> {
        println!("⏳ 正在查询关节位置...");

        let profile = config.control_profile(self.target.target.as_ref());
        let builder = client_builder(&profile.target);

        println!("🔌 连接到机器人...");
        let robot = builder.build()?;
        let observer = robot.observer();
        let snapshot = observer.control_snapshot(ControlReadPolicy::default())?;

        println!("📊 关节位置:");
        for (index, pos) in snapshot.position.iter().enumerate() {
            println!(
                "  J{}: {:.3} rad ({:.1}°)",
                index + 1,
                pos.0,
                pos.to_deg().0
            );
        }

        let end_pose = observer.end_pose();
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
            format: "table".to_string(),
        };

        assert_eq!(cmd.format, "table");
    }

    #[test]
    fn position_command_accepts_target_override() {
        let cmd = PositionCommand {
            target: TargetArgs {
                target: Some(TargetSpec::DaemonUdp {
                    addr: "127.0.0.1:18888".to_string(),
                }),
            },
            format: "json".to_string(),
        };

        assert_eq!(
            cmd.target.target,
            Some(TargetSpec::DaemonUdp {
                addr: "127.0.0.1:18888".to_string()
            })
        );
    }
}
