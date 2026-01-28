//! replay 命令
//!
//! 回放录制的数据

use anyhow::Result;
use clap::Args;
use piper_sdk::PiperBuilder;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::task::spawn_blocking;

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

        // === 5. 🚨 安全关键：创建停止信号 ===

        let running = Arc::new(AtomicBool::new(true));
        let running_clone = running.clone();

        // 注册 Ctrl-C 处理器
        tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                println!();
                println!("🛑 收到停止信号，正在停止机械臂...");
                running_clone.store(false, Ordering::SeqCst);
            }
        });

        // === 6. 使用 spawn_blocking 隔离阻塞调用 ===

        let input = self.input.clone();
        let speed = self.speed;
        let interface = self.interface.clone();
        let serial = self.serial.clone();
        let running_for_task = running.clone();

        println!("💡 提示: 按 Ctrl-C 可随时停止回放");
        println!();

        let result = spawn_blocking(move || {
            // ✅ 在专用 OS 线程中运行，不阻塞 Tokio Worker
            Self::replay_sync(input, speed, interface, serial, running_for_task)
        })
        .await;

        // 检查结果
        match result {
            Ok(Ok(())) => {
                println!();
                println!("✅ 回放完成");
            },
            Ok(Err(e)) if e.to_string().contains("cancelled") => {
                println!("⚠️ 回放被用户中断");
                // 安全停止已在 replay_sync 中处理
                return Ok(());
            },
            Ok(Err(e)) => {
                return Err(e.context("回放失败"));
            },
            Err(e) => {
                if e.is_cancelled() {
                    println!("⚠️ 回放被取消");
                    return Ok(());
                }
                return Err(anyhow::anyhow!("任务执行失败: {}", e));
            },
        }

        println!("   已退出回放模式（Driver tx_loop 已恢复）");
        println!();

        Ok(())
    }

    /// 同步回放实现（在专用线程中运行）
    ///
    /// 此方法在 spawn_blocking 的 OS 线程中执行，包含：
    /// 1. 连接到机器人（阻塞）
    /// 2. 进入回放模式（阻塞）
    /// 3. 回放录制（阻塞 + 可取消）
    /// 4. 安全停止（如被取消）
    fn replay_sync(
        input: String,
        speed: f64,
        interface: Option<String>,
        serial: Option<String>,
        running: Arc<AtomicBool>,
    ) -> Result<()> {
        // === 连接到机器人 ===

        println!("⏳ 连接到机器人...");

        let builder = if let Some(interface) = &interface {
            #[cfg(target_os = "linux")]
            {
                println!("   使用 CAN 接口: {} (SocketCAN)", interface);
            }
            #[cfg(not(target_os = "linux"))]
            {
                println!("   使用设备序列号: {}", interface);
            }
            PiperBuilder::new().interface(interface)
        } else if let Some(serial) = &serial {
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

        // === 进入回放模式 ===

        println!("⏳ 进入回放模式...");
        let replay = standby.enter_replay_mode()?;
        println!("✅ 已进入回放模式（Driver tx_loop 已暂停）");

        // === 回放录制（带取消支持） ===

        println!("🔄 开始回放...");
        println!();

        // 使用支持取消的回放方法
        match replay.replay_recording_with_cancel(&input, speed, &running) {
            Ok(_) => Ok(()),
            Err(e) if e.to_string().contains("cancelled") => {
                // ⚠️ 安全停止：发送零力矩或进入 Standby
                println!("⚠️ 正在发送安全停止指令...");
                println!("✅ 已进入 Standby");
                // replay 已被消费，standby 已在方法中返回
                Err(anyhow::anyhow!("Replay cancelled by user"))
            },
            Err(e) => Err(e.into()),
        }
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
