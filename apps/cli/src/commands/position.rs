//! 位置查询命令

use anyhow::Result;
use clap::Args;
use piper_client::PiperBuilder;

/// 位置查询命令参数
#[derive(Args, Debug)]
pub struct PositionCommand {
    /// CAN 接口（覆盖配置）
    #[arg(short, long)]
    pub interface: Option<String>,

    /// 设备序列号（GS-USB）
    #[arg(short, long)]
    pub serial: Option<String>,

    /// 输出格式
    #[arg(short, long, default_value = "table")]
    pub format: String,
}

impl PositionCommand {
    /// 执行位置查询
    pub async fn execute(&self, config: &crate::modes::oneshot::OneShotConfig) -> Result<()> {
        println!("⏳ 正在查询关节位置...");

        // 确定接口（命令行参数优先）
        let interface = self.interface.as_ref().or(config.interface.as_ref()).map(|s| s.as_str());

        // 创建 Piper 实例
        let mut builder = PiperBuilder::new();
        if let Some(iface) = interface {
            builder = builder.interface(iface);
        }

        println!("🔌 连接到机器人...");
        let robot = builder.build()?;

        // 获取 Observer
        let observer = robot.observer();

        // 读取关节位置
        println!("📊 关节位置:");
        let snapshot = observer.snapshot();

        for (i, pos) in snapshot.position.iter().enumerate() {
            let deg = pos.to_deg();
            println!("  J{}: {:.3} rad ({:.1}°)", i + 1, pos.0, deg.0);
        }

        // TODO: 末端位姿需要使用 driver 层 API
        // 目前简化实现，只显示关节位置
        println!("\n💡 提示: 末端位姿查看请使用 monitor 命令");

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_position_command_creation() {
        let cmd = PositionCommand {
            interface: Some("can0".to_string()),
            serial: None,
            format: "json".to_string(),
        };

        assert_eq!(cmd.interface, Some("can0".to_string()));
        assert_eq!(cmd.format, "json");
    }

    #[test]
    fn test_position_command_default_format() {
        let cmd = PositionCommand {
            interface: None,
            serial: None,
            format: "table".to_string(),
        };

        assert_eq!(cmd.format, "table");
    }
}
