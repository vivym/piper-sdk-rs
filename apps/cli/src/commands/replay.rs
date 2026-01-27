//! replay 命令
//!
//! 回放录制的数据

use anyhow::Result;
use clap::Args;
use piper_sdk::PiperBuilder;

/// 回放命令参数
#[derive(Args, Debug)]
pub struct ReplayCommand {
    /// 录制文件路径
    #[arg(short, long)]
    pub input: String,

    /// 回放速度倍数（1.0 = 正常速度）
    ///
    /// # 安全说明
    ///
    /// - 1.0x: 原始速度（推荐）
    /// - 0.1x ~ 2.0x: 安全范围
    /// - > 2.0x: 需要特别小心
    /// - 最大值: 5.0x
    #[arg(short, long, default_value_t = 1.0)]
    pub speed: f64,

    /// CAN 接口（覆盖配置）
    #[arg(short, long)]
    pub interface: Option<String>,

    /// 设备序列号（GS-USB）
    #[arg(short, long)]
    pub serial: Option<String>,

    /// 回放前确认
    #[arg(long)]
    pub confirm: bool,
}

impl ReplayCommand {
    /// 执行回放
    pub async fn execute(&self) -> Result<()> {
        // === 1. 文件检查 ===

        let path = std::path::Path::new(&self.input);
        if !path.exists() {
            anyhow::bail!("❌ 录制文件不存在: {}", self.input);
        }

        // === 2. 速度验证 ===

        const MAX_SPEED_FACTOR: f64 = 5.0;
        const RECOMMENDED_SPEED_FACTOR: f64 = 2.0;

        if self.speed <= 0.0 {
            anyhow::bail!("❌ 速度倍数必须为正数，当前: {:.2}", self.speed);
        }

        if self.speed > MAX_SPEED_FACTOR {
            anyhow::bail!(
                "❌ 速度倍数超出最大值: {:.2} > {}\n   最大速度倍数限制为安全考虑",
                self.speed,
                MAX_SPEED_FACTOR
            );
        }

        // === 3. 显示回放信息 ===

        println!("════════════════════════════════════════");
        println!("           回放模式");
        println!("════════════════════════════════════════");
        println!();
        println!("📁 文件: {}", self.input);
        println!("⚡ 速度: {:.2}x", self.speed);

        if self.speed > RECOMMENDED_SPEED_FACTOR {
            println!(
                "⚠️  警告: 速度超过推荐值 ({:.1}x)",
                RECOMMENDED_SPEED_FACTOR
            );
            println!("   请确保:");
            println!("   • 回放环境安全，无人员/障碍物");
            println!("   • 有急停准备");
            println!("   • 机器人状态正常");
        }

        println!();

        // === 4. 安全确认 ===

        if !self.confirm {
            let prompt = "即将开始回放，确定要继续吗？[y/N] ";

            print!("{}", prompt);
            use std::io::Write;
            std::io::stdout().flush()?;

            let mut input = String::new();
            std::io::stdin().read_line(&mut input)?;

            if !input.trim().to_lowercase().starts_with('y') {
                println!("❌ 操作已取消");
                return Ok(());
            }

            println!("✅ 已确认");
            println!();
        }

        // === 5. 连接到机器人 ===

        println!("⏳ 连接到机器人...");

        let builder = if let Some(interface) = &self.interface {
            #[cfg(target_os = "linux")]
            {
                println!("   使用 CAN 接口: {} (SocketCAN)", interface);
            }
            #[cfg(not(target_os = "linux"))]
            {
                println!("   使用设备序列号: {}", interface);
            }
            PiperBuilder::new().interface(interface)
        } else if let Some(serial) = &self.serial {
            println!("   使用设备序列号: {}", serial);
            PiperBuilder::new().interface(serial)
        } else {
            #[cfg(target_os = "linux")]
            {
                println!("   使用默认 CAN 接口: can0");
                PiperBuilder::new().interface("can0")
            }
            #[cfg(target_os = "macos")]
            {
                let default_daemon = "127.0.0.1:18888";
                println!("   使用默认守护进程: {}", default_daemon);
                PiperBuilder::new().with_daemon(default_daemon)
            }
            #[cfg(not(any(target_os = "linux", target_os = "macos")))]
            {
                println!("   自动扫描 GS-USB 设备...");
                PiperBuilder::new()
            }
        };

        let standby = builder.build()?;
        println!("✅ 已连接");

        // === 6. 进入回放模式 ===

        println!("⏳ 进入回放模式...");
        let replay = standby.enter_replay_mode()?;
        println!("✅ 已进入回放模式（Driver tx_loop 已暂停）");

        // === 7. 回放录制 ===

        println!("🔄 开始回放...");
        println!();
        println!("   进度: [回放中...]");
        println!();

        let _standby = replay.replay_recording(&self.input, self.speed)?;

        // === 8. 完成 ===

        println!();
        println!("✅ 回放完成");
        println!("   已退出回放模式（Driver tx_loop 已恢复）");
        println!();

        // 任何连接都会在这里自动 Drop 并断开
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_replay_command_creation() {
        let cmd = ReplayCommand {
            input: "recording.bin".to_string(),
            speed: 2.0,
            interface: Some("can0".to_string()),
            serial: None,
            confirm: true,
        };

        assert_eq!(cmd.input, "recording.bin");
        assert_eq!(cmd.speed, 2.0);
        assert!(cmd.confirm);
    }

    #[test]
    fn test_replay_command_defaults() {
        let cmd = ReplayCommand {
            input: "recording.bin".to_string(),
            speed: 1.0,
            interface: None,
            serial: None,
            confirm: false,
        };

        assert_eq!(cmd.speed, 1.0);
        assert!(!cmd.confirm);
    }

    #[test]
    fn test_replay_command_with_serial() {
        let cmd = ReplayCommand {
            input: "test.bin".to_string(),
            speed: 1.5,
            interface: None,
            serial: Some("ABC123".to_string()),
            confirm: false,
        };

        assert_eq!(cmd.input, "test.bin");
        assert_eq!(cmd.speed, 1.5);
        assert_eq!(cmd.serial, Some("ABC123".to_string()));
        assert!(cmd.interface.is_none());
    }

    #[test]
    fn test_replay_command_interface_takes_precedence() {
        let cmd = ReplayCommand {
            input: "test.bin".to_string(),
            speed: 1.0,
            interface: Some("vcan0".to_string()),
            serial: Some("ABC123".to_string()),
            confirm: true,
        };

        // Both can be set, but interface should take precedence in execute()
        assert_eq!(cmd.interface, Some("vcan0".to_string()));
        assert_eq!(cmd.serial, Some("ABC123".to_string()));
    }

    #[test]
    fn test_replay_command_max_speed() {
        let max_speed = 5.0;
        let cmd = ReplayCommand {
            input: "test.bin".to_string(),
            speed: max_speed,
            interface: None,
            serial: None,
            confirm: true,
        };

        assert_eq!(cmd.speed, max_speed);
    }

    #[test]
    fn test_replay_command_slow_speed() {
        let min_speed = 0.1;
        let cmd = ReplayCommand {
            input: "test.bin".to_string(),
            speed: min_speed,
            interface: None,
            serial: None,
            confirm: false,
        };

        assert_eq!(cmd.speed, min_speed);
    }

    #[test]
    fn test_replay_command_recommended_speed() {
        let recommended_speed = 2.0;
        let cmd = ReplayCommand {
            input: "test.bin".to_string(),
            speed: recommended_speed,
            interface: None,
            serial: None,
            confirm: false,
        };

        assert_eq!(cmd.speed, recommended_speed);
    }
}
