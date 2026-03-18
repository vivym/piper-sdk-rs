//! 急停命令

use anyhow::Result;
use clap::Args;

use crate::commands::config::CliConfig;
use crate::connection::{TargetArgs, client_builder};

#[derive(Args, Debug, Clone)]
pub struct StopCommand {
    #[command(flatten)]
    pub target: TargetArgs,
}

impl StopCommand {
    pub async fn execute(&self, config: &CliConfig) -> Result<()> {
        let profile = config.control_profile(self.target.target.as_ref());
        let builder = client_builder(&profile.target);

        println!("🔌 连接到机器人...");
        let robot = builder.build()?;
        println!("🛑 发送急停命令（失能所有关节）...");
        robot.disable_all()?;
        println!("✅ 急停完成");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use piper_control::TargetSpec;

    #[test]
    fn stop_command_can_override_target() {
        let cmd = StopCommand {
            target: TargetArgs {
                target: Some(TargetSpec::SocketCan {
                    iface: "can0".to_string(),
                }),
            },
        };

        assert_eq!(
            cmd.target.target,
            Some(TargetSpec::SocketCan {
                iface: "can0".to_string()
            })
        );
    }

    #[test]
    fn stop_command_defaults_to_config_target() {
        let cmd = StopCommand {
            target: TargetArgs::default(),
        };

        assert!(cmd.target.target.is_none());
    }
}
