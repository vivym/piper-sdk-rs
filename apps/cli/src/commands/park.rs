//! 停靠命令

use anyhow::Result;
use clap::Args;
use piper_control::park_blocking;
use piper_sdk::client::MotionConnectedPiper;

use crate::commands::config::CliConfig;
use crate::connection::{TargetArgs, client_builder};

#[derive(Args, Debug, Clone)]
pub struct ParkCommand {
    #[command(flatten)]
    pub target: TargetArgs,
}

impl ParkCommand {
    pub async fn execute(&self, config: &CliConfig) -> Result<()> {
        let profile = config.control_profile(self.target.target.as_ref());
        let builder = client_builder(&profile.target);

        println!("🔌 连接到机器人...");
        let standby = builder.build()?;
        let standby = standby.require_motion()?;
        println!(
            "⏳ 前往停靠位姿（orientation = {}）...",
            profile.orientation
        );
        match standby {
            MotionConnectedPiper::Strict(standby) => {
                let _standby = park_blocking(standby, &profile)?;
            },
            MotionConnectedPiper::Soft(standby) => {
                let _standby = park_blocking(standby, &profile)?;
            },
        }
        println!("✅ 停靠完成");
        Ok(())
    }
}
