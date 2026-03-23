//! 移动命令

use crate::commands::config::CliConfig;
use crate::connection::{TargetArgs, client_builder};
use crate::safety::confirm_prepared_move;
use anyhow::{Context, Result};
use clap::Args;
use piper_control::{move_to_joint_target_blocking, prepare_move};
use piper_sdk::client::{MotionConnectedPiper, MotionConnectedState};

#[derive(Args, Debug, Clone)]
pub struct MoveCommand {
    /// 目标关节位置（弧度），逗号分隔；1~6 个值会依次映射到 J1..Jn，剩余关节保持当前位置
    #[arg(short, long)]
    pub joints: Option<String>,

    /// 跳过大幅移动确认
    #[arg(long)]
    pub force: bool,

    #[command(flatten)]
    pub target: TargetArgs,
}

impl MoveCommand {
    pub fn parse_joints(&self) -> Result<Vec<f64>> {
        let joints_str = self
            .joints
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("未指定关节位置，请使用 --joints 参数"))?;

        let positions: Vec<f64> = joints_str
            .split(',')
            .map(|value| value.trim().parse::<f64>())
            .collect::<Result<Vec<_>, _>>()
            .context("解析关节位置失败")?;

        if positions.is_empty() {
            anyhow::bail!("关节位置不能为空");
        }
        if positions.len() > 6 {
            anyhow::bail!("最多支持 6 个关节");
        }

        Ok(positions)
    }

    pub async fn execute(&self, config: &CliConfig) -> Result<()> {
        let requested_positions = self.parse_joints()?;
        let profile = config.control_profile(self.target.target.as_ref());
        let builder = client_builder(&profile.target);

        println!("🔌 连接到机器人...");
        let standby = builder.build()?;
        let standby = standby.require_motion()?;
        let current = current_positions(&standby)?;
        let prepared = prepare_move(current, &requested_positions, &profile.safety, self.force)?;

        if prepared.requires_confirmation && !confirm_prepared_move(&prepared)? {
            println!("❌ 操作已取消");
            return Ok(());
        }

        println!("⏳ 正在移动到目标位置...");
        for (index, value) in prepared.effective_target.iter().enumerate() {
            let source = if index < requested_positions.len() {
                "用户指定"
            } else {
                "保持当前"
            };
            println!(
                "  J{}: {:.3} rad ({:.1}°) [{}]",
                index + 1,
                value,
                value.to_degrees(),
                source
            );
        }

        match standby {
            MotionConnectedPiper::Strict(MotionConnectedState::Standby(standby)) => {
                let _standby =
                    move_to_joint_target_blocking(standby, &profile, prepared.effective_target)?;
            },
            MotionConnectedPiper::Soft(MotionConnectedState::Standby(standby)) => {
                let _standby =
                    move_to_joint_target_blocking(standby, &profile, prepared.effective_target)?;
            },
            MotionConnectedPiper::Strict(MotionConnectedState::Maintenance(_))
            | MotionConnectedPiper::Soft(MotionConnectedState::Maintenance(_)) => {
                anyhow::bail!("机械臂当前不在确认全失能的 Standby，请先执行 stop")
            },
        }
        println!("✅ 移动完成");
        Ok(())
    }
}

fn current_positions(standby: &MotionConnectedPiper) -> Result<[f64; 6]> {
    let positions = match standby {
        MotionConnectedPiper::Strict(state) => state.observer().joint_positions()?,
        MotionConnectedPiper::Soft(state) => state.observer().joint_positions()?,
    };
    Ok(std::array::from_fn(|index| positions[index].0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use piper_control::{TargetSpec, prepare_move};
    use piper_tools::SafetyConfig;

    #[test]
    fn parse_joints_allows_partial_targets() {
        let cmd = MoveCommand {
            joints: Some("0.1,0.2,0.3".to_string()),
            force: false,
            target: TargetArgs::default(),
        };

        assert_eq!(cmd.parse_joints().unwrap(), vec![0.1, 0.2, 0.3]);
    }

    #[test]
    fn parse_joints_rejects_invalid_numbers() {
        let cmd = MoveCommand {
            joints: Some("0.1,invalid,0.3".to_string()),
            force: false,
            target: TargetArgs::default(),
        };

        assert!(cmd.parse_joints().is_err());
    }

    #[test]
    fn prepare_move_requires_confirmation_for_large_rollback() {
        let prepared = prepare_move(
            [2.5, 0.0, 0.0, 0.0, 0.0, 0.0],
            &[0.1],
            &SafetyConfig::default_config(),
            false,
        )
        .unwrap();

        assert!(prepared.requires_confirmation);
        assert!(prepared.max_delta_deg > 100.0);
    }

    #[test]
    fn target_override_is_carried_by_args() {
        let cmd = MoveCommand {
            joints: Some("0.1".to_string()),
            force: true,
            target: TargetArgs {
                target: Some(TargetSpec::SocketCan {
                    iface: "vcan0".to_string(),
                }),
            },
        };

        assert_eq!(
            cmd.target.target,
            Some(TargetSpec::SocketCan {
                iface: "vcan0".to_string()
            })
        );
    }
}
