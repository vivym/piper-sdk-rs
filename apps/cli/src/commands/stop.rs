//! 急停命令
//!
//! 软件急停功能，用于紧急情况下的快速停止

use anyhow::Result;
use clap::Args;
use piper_client::PiperBuilder;

/// 急停命令参数
#[derive(Args, Debug)]
pub struct StopCommand {
    /// CAN 接口（覆盖配置）
    #[arg(short, long)]
    pub interface: Option<String>,

    /// 设备序列号（GS-USB）
    #[arg(short, long)]
    pub serial: Option<String>,
}

impl StopCommand {
    /// 执行急停
    pub async fn execute(&self, config: &crate::modes::oneshot::OneShotConfig) -> Result<()> {
        // 确定接口（命令行参数优先）
        let interface = self.interface.as_ref().or(config.interface.as_ref()).map(|s| s.as_str());

        // 创建 Piper 实例
        let mut builder = PiperBuilder::new();
        if let Some(iface) = interface {
            builder = builder.interface(iface);
        }

        println!("🔌 连接到机器人...");
        let robot = builder.build()?;

        println!("🛑 发送急停命令（失能所有关节）...");

        // 使用 client 层的 disable_all 方法
        robot.disable_all()?;

        println!("✅ 急停完成");

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stop_command_creation() {
        let cmd = StopCommand {
            interface: Some("can0".to_string()),
            serial: Some("ABC123".to_string()),
        };

        assert_eq!(cmd.interface, Some("can0".to_string()));
        assert_eq!(cmd.serial, Some("ABC123".to_string()));
    }

    #[test]
    fn test_stop_command_defaults() {
        let cmd = StopCommand {
            interface: None,
            serial: None,
        };

        assert!(cmd.interface.is_none());
        assert!(cmd.serial.is_none());
    }
}
