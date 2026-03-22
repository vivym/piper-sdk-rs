//! replay 命令
//!
//! 回放录制的数据

use crate::commands::config::CliConfig;
use crate::connection::{TargetArgs, client_builder, resolved_target_spec};
use anyhow::Result;
use clap::Args;
use piper_control::TargetSpec;
use piper_sdk::client::state::{MotionCapability, Standby};
use piper_sdk::client::{MotionConnectedPiper, Piper};
use piper_sdk::driver::ConnectionTarget;
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

    #[command(flatten)]
    pub target: TargetArgs,

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

        let config = CliConfig::load()?;
        let target_spec = resolved_target_spec(&config, self.target.target.as_ref());
        let input = self.input.clone();
        let speed = self.speed;
        let target = target_spec.clone().into_connection_target();
        let running_for_task = running.clone();

        println!("💡 提示: 按 Ctrl-C 可随时停止回放");
        println!("🎯 target: {}", target_spec);
        println!();

        let result = spawn_blocking(move || {
            // ✅ 在专用 OS 线程中运行，不阻塞 Tokio Worker
            Self::replay_sync(input, speed, target, target_spec, running_for_task)
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
        target: ConnectionTarget,
        target_spec: TargetSpec,
        running: Arc<AtomicBool>,
    ) -> Result<()> {
        // === 连接到机器人 ===

        println!("⏳ 连接到机器人...");
        println!("   target: {}", target_spec);

        let builder = client_builder(&target);

        let standby = builder.build()?.require_motion()?;
        println!("✅ 已连接");

        // === 进入回放模式 ===

        match standby {
            MotionConnectedPiper::Strict(standby) => {
                Self::replay_with_standby(standby, &input, speed, &running)
            },
            MotionConnectedPiper::Soft(standby) => {
                Self::replay_with_standby(standby, &input, speed, &running)
            },
        }
    }

    fn replay_with_standby<Capability>(
        standby: Piper<Standby, Capability>,
        input: &str,
        speed: f64,
        running: &Arc<AtomicBool>,
    ) -> Result<()>
    where
        Capability: MotionCapability,
    {
        println!("⏳ 进入回放模式...");
        let replay = standby.enter_replay_mode()?;
        println!("✅ 已进入回放模式（Driver tx_loop 已暂停）");

        println!("🔄 开始回放...");
        println!();

        match replay.replay_recording_with_cancel(input, speed, running) {
            Ok(_) => Ok(()),
            Err(e) if e.to_string().contains("cancelled") => {
                println!("⚠️ 正在发送安全停止指令...");
                println!("✅ 已进入 Standby");
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
            target: TargetArgs {
                target: Some(TargetSpec::SocketCan {
                    iface: "can0".to_string(),
                }),
            },
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
            target: TargetArgs::default(),
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
            target: TargetArgs {
                target: Some(TargetSpec::GsUsbSerial {
                    serial: "ABC123".to_string(),
                }),
            },
            confirm: false,
        };

        assert_eq!(cmd.input, "test.bin");
        assert_eq!(cmd.speed, 1.5);
        assert!(matches!(
            cmd.target.target,
            Some(TargetSpec::GsUsbSerial { .. })
        ));
    }

    #[test]
    fn test_replay_command_accepts_target_override() {
        let cmd = ReplayCommand {
            input: "test.bin".to_string(),
            speed: 1.0,
            target: TargetArgs {
                target: Some(TargetSpec::SocketCan {
                    iface: "vcan0".to_string(),
                }),
            },
            confirm: true,
        };

        assert!(matches!(
            cmd.target.target,
            Some(TargetSpec::SocketCan { .. })
        ));
    }

    #[test]
    fn test_replay_command_max_speed() {
        let max_speed = 5.0;
        let cmd = ReplayCommand {
            input: "test.bin".to_string(),
            speed: max_speed,
            target: TargetArgs::default(),
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
            target: TargetArgs::default(),
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
            target: TargetArgs::default(),
            confirm: false,
        };

        assert_eq!(cmd.speed, recommended_speed);
    }
}
