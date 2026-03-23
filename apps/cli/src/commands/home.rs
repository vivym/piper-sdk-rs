//! 回零命令

use anyhow::Result;
use clap::Args;
use piper_control::home_zero_blocking;
use piper_sdk::client::{MotionConnectedPiper, MotionConnectedState};

use crate::commands::config::CliConfig;
use crate::connection::{TargetArgs, client_builder};

#[derive(Args, Debug, Clone)]
pub struct HomeCommand {
    #[command(flatten)]
    pub target: TargetArgs,
}

impl HomeCommand {
    pub async fn execute(&self, config: &CliConfig) -> Result<()> {
        let profile = config.control_profile(self.target.target.as_ref());
        let builder = client_builder(&profile.target);

        println!("🔌 连接到机器人...");
        let standby = builder.build()?;
        let standby = standby.require_motion()?;
        println!("⏳ 发送零关节目标...");
        match standby {
            MotionConnectedPiper::Strict(MotionConnectedState::Standby(standby)) => {
                let _standby = home_zero_blocking(standby, &profile)?;
            },
            MotionConnectedPiper::Soft(MotionConnectedState::Standby(standby)) => {
                let _standby = home_zero_blocking(standby, &profile)?;
            },
            MotionConnectedPiper::Strict(MotionConnectedState::Maintenance(_))
            | MotionConnectedPiper::Soft(MotionConnectedState::Maintenance(_)) => {
                anyhow::bail!("机械臂当前不在确认全失能的 Standby，请先执行 stop")
            },
        }
        println!("✅ 回零完成");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_command_defaults_to_config_target() {
        let cmd = HomeCommand {
            target: TargetArgs::default(),
        };

        assert!(cmd.target.target.is_none());
    }
}
