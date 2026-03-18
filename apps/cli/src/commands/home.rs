//! 回零命令

use anyhow::Result;
use clap::Args;
use piper_control::home_zero_blocking;

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
        println!("⏳ 发送零关节目标...");
        let _standby = home_zero_blocking(standby, &profile)?;
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
