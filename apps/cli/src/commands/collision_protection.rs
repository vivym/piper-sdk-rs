//! 碰撞保护命令

use anyhow::Result;
use clap::{Args, Subcommand};
use piper_control::{query_collision_protection_blocking, set_collision_protection_verified};

use crate::commands::config::CliConfig;
use crate::connection::{TargetArgs, client_builder};
use crate::parsing::parse_collision_levels;

#[derive(Args, Debug, Clone)]
pub struct CollisionProtectionCommand {
    #[command(subcommand)]
    pub action: CollisionProtectionAction,
}

#[derive(Subcommand, Debug, Clone)]
pub enum CollisionProtectionAction {
    Get {
        #[command(flatten)]
        target: TargetArgs,
    },
    Set {
        /// 对全部关节使用统一等级（0~8）
        #[arg(long)]
        level: Option<u8>,

        /// 分别指定 6 个关节的等级，格式: j1,j2,j3,j4,j5,j6
        #[arg(long)]
        levels: Option<String>,

        #[command(flatten)]
        target: TargetArgs,
    },
}

impl CollisionProtectionCommand {
    pub async fn execute(&self, config: &CliConfig) -> Result<()> {
        match &self.action {
            CollisionProtectionAction::Get { target } => {
                let profile = config.control_profile(target.target.as_ref());
                let builder = client_builder(&profile.target);
                let standby = builder.build()?;
                let levels = query_collision_protection_blocking(&standby, &profile.wait)?;
                println!("collision protection levels: {:?}", levels);
                Ok(())
            },
            CollisionProtectionAction::Set {
                level,
                levels,
                target,
            } => {
                let desired = parse_collision_levels(*level, levels.as_deref())?;
                let profile = config.control_profile(target.target.as_ref());
                let builder = client_builder(&profile.target);
                let standby = builder.build()?;
                set_collision_protection_verified(&standby, desired, &profile.wait)?;
                println!("✅ 碰撞保护等级已写入并校验: {:?}", desired);
                Ok(())
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_levels_accepts_single_level() {
        assert_eq!(parse_collision_levels(Some(3), None).unwrap(), [3; 6]);
    }

    #[test]
    fn parse_levels_accepts_per_joint_levels() {
        assert_eq!(
            parse_collision_levels(None, Some("1,2,3,4,5,6")).unwrap(),
            [1, 2, 3, 4, 5, 6]
        );
    }

    #[test]
    fn parse_levels_rejects_invalid_mixes() {
        assert!(parse_collision_levels(Some(3), Some("1,2,3,4,5,6")).is_err());
        assert!(parse_collision_levels(None, None).is_err());
    }
}
