//! 零点标定命令

use anyhow::Result;
use clap::Args;
use piper_control::set_joint_zero_blocking;
use piper_sdk::client::MotionConnectedPiper;

use crate::commands::config::CliConfig;
use crate::connection::{TargetArgs, client_builder};
use crate::parsing::parse_joint_indices_arg;
use crate::safety::confirm_zero_setting;

#[derive(Args, Debug, Clone)]
pub struct SetZeroCommand {
    /// 需要写入零点的关节编号，示例: 1,2,3；默认全部关节
    #[arg(long)]
    pub joints: Option<String>,

    /// 跳过确认提示
    #[arg(long)]
    pub force: bool,

    #[command(flatten)]
    pub target: TargetArgs,
}

impl SetZeroCommand {
    pub fn parse_joint_indices(&self) -> Result<Vec<usize>> {
        parse_joint_indices_arg(self.joints.as_deref())
    }

    pub async fn execute(&self, config: &CliConfig) -> Result<()> {
        let joints = self.parse_joint_indices()?;
        if !self.force && !confirm_zero_setting(&joints)? {
            println!("❌ 操作已取消");
            return Ok(());
        }

        let profile = config.control_profile(self.target.target.as_ref());
        let builder = client_builder(&profile.target);

        println!("🔌 连接到机器人...");
        let standby = builder.build()?;
        let standby = standby.require_motion()?;
        match &standby {
            MotionConnectedPiper::Strict(standby) => set_joint_zero_blocking(standby, &joints)?,
            MotionConnectedPiper::Soft(standby) => set_joint_zero_blocking(standby, &joints)?,
        }
        println!("✅ 零点标定命令已发送");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_joint_indices_defaults_to_all_joints() {
        let cmd = SetZeroCommand {
            joints: None,
            force: false,
            target: TargetArgs::default(),
        };

        assert_eq!(cmd.parse_joint_indices().unwrap(), vec![0, 1, 2, 3, 4, 5]);
    }

    #[test]
    fn parse_joint_indices_converts_to_zero_based() {
        let cmd = SetZeroCommand {
            joints: Some("1,3,6".to_string()),
            force: true,
            target: TargetArgs::default(),
        };

        assert_eq!(cmd.parse_joint_indices().unwrap(), vec![0, 2, 5]);
    }
}
