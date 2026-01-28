//! record 命令
//!
//! 录制 CAN 总线数据到文件

use anyhow::{Context, Result};
use clap::Args;
use piper_sdk::{PiperBuilder, RecordingConfig, RecordingMetadata, StopCondition};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tokio::task::spawn_blocking;

use crate::validation::PathValidator;

/// 录制命令参数
#[derive(Args, Debug)]
pub struct RecordCommand {
    /// 输出文件路径
    #[arg(short, long)]
    pub output: String,

    /// CAN 接口（覆盖配置）
    #[arg(short, long)]
    pub interface: Option<String>,

    /// 设备序列号（GS-USB）
    #[arg(short, long)]
    pub serial: Option<String>,

    /// 录制时长（秒），0 表示无限
    #[arg(short, long, default_value_t = 0)]
    pub duration: u64,

    /// 自动停止（接收到特定 CAN ID 时停止）
    #[arg(short, long)]
    pub stop_on_id: Option<u32>,

    /// 跳过确认提示
    #[arg(long)]
    pub force: bool,
}

impl RecordCommand {
    /// 执行录制
    pub async fn execute(&self) -> Result<()> {
        // === 1. 参数验证 ===

        let output_path = PathBuf::from(&self.output);

        // 🔴 P0 安全修复：验证输出路径
        let validator = PathValidator::new();
        validator
            .validate_output_path(&self.output)
            .context("输出路径验证失败，请确保父目录存在")?;

        // 检查文件是否已存在
        if output_path.exists() && !self.force {
            println!("⚠️  文件已存在: {}", self.output);
            print!("是否覆盖? [y/N] ");
            use std::io::Write;
            std::io::stdout().flush()?;

            let mut input = String::new();
            std::io::stdin().read_line(&mut input)?;

            if !input.trim().to_lowercase().starts_with('y') {
                println!("❌ 操作已取消");
                return Ok(());
            }
        }

        // ✅ --stop-on-id 已在 SDK 层实现（v1.4）

        // === 2. 显示录制信息 ===

        println!("════════════════════════════════════════");
        println!("           录制模式");
        println!("════════════════════════════════════════");
        println!();
        println!("📁 输出: {}", self.output);
        println!(
            "⏱️  时长: {}",
            if self.duration == 0 {
                "手动停止".to_string()
            } else {
                format!("{} 秒", self.duration)
            }
        );
        if let Some(stop_id) = self.stop_on_id {
            println!("🛑 停止条件: CAN ID 0x{:X}", stop_id);
        }
        if let Some(interface) = &self.interface {
            println!("💾 接口: {}", interface);
        } else if let Some(serial) = &self.serial {
            println!("🔧 序列号: {}", serial);
        } else {
            #[cfg(target_os = "linux")]
            println!("💾 接口: can0 (默认)");
            #[cfg(target_os = "macos")]
            println!("🔧 守护进程: 127.0.0.1:18888 (默认)");
        }
        println!();

        // === 3. 安全确认 ===

        if !self.force {
            let prompt = "即将开始录制，确定要继续吗？[y/N] ";

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

        // === 4. 🚨 创建停止信号 ===

        let running = Arc::new(AtomicBool::new(true));
        let running_clone = running.clone();

        // 注册 Ctrl-C 处理器
        tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                println!();
                println!("🛑 收到停止信号，正在保存录制...");
                running_clone.store(false, Ordering::SeqCst);
            }
        });

        // === 5. 使用 spawn_blocking 隔离 ===

        // 在专用线程中运行录制逻辑
        let output_path = self.output.clone();
        let duration = self.duration;
        let interface = self.interface.clone();
        let serial = self.serial.clone();
        let stop_on_id = self.stop_on_id;
        let running_for_task = running.clone();

        println!("💡 提示: 按 Ctrl-C 停止录制");
        println!();

        // 在专用线程中运行录制逻辑
        let task = spawn_blocking(move || {
            Self::record_sync(
                output_path,
                duration,
                interface,
                serial,
                stop_on_id,
                running_for_task,
            )
        });

        let result: Result<
            Result<piper_sdk::RecordingStats, anyhow::Error>,
            tokio::task::JoinError,
        > = task.await;

        // === 6. 检查结果 ===

        match result {
            Ok(inner_result) => match inner_result {
                Ok(stats) => {
                    println!();
                    println!("✅ 录制完成");
                    println!("   📊 帧数: {}", stats.frame_count);
                    println!("   ⏱️  时长: {:.2}s", stats.duration.as_secs_f64());
                    println!("   ⚠️ 丢帧: {}", stats.dropped_frames);
                    println!("   💾 已保存: {}", stats.output_path.display());
                    Ok(())
                },
                Err(e) => Err(e.context("录制失败")),
            },
            Err(e) => {
                if e.is_cancelled() {
                    println!("⚠️  录制被取消");
                    Ok(())
                } else {
                    Err(anyhow::anyhow!("任务执行失败: {}", e))
                }
            },
        }
    }

    /// 同步录制实现（在专用线程中运行）
    ///
    /// 此方法在 spawn_blocking 的 OS 线程中执行，包含：
    /// 1. 连接到机器人（阻塞）
    /// 2. 启动录制（阻塞）
    /// 3. 录制循环（阻塞 + 可取消）
    /// 4. 停止录制并保存（安全退出）
    fn record_sync(
        output_path: String,
        duration: u64,
        interface: Option<String>,
        serial: Option<String>,
        stop_on_id: Option<u32>,
        running: Arc<AtomicBool>,
    ) -> Result<piper_sdk::RecordingStats> {
        // === 1. 连接到机器人 ===

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

        // ⚠️ 缓冲区警告（Phase 1 限制）
        if duration == 0 || duration > 180 {
            println!();
            println!("⚠️  注意：当前版本主要用于短时录制（< 3分钟）");
            println!("   超过此时长可能导致数据丢失（缓冲区限制）");
            println!();
        }

        // === 2. 映射停止条件 ===

        // ✅ 优先级：stop_on_id > duration > manual
        let stop_condition = if let Some(id) = stop_on_id {
            StopCondition::OnCanId(id)
        } else if duration > 0 {
            StopCondition::Duration(duration)
        } else {
            StopCondition::Manual
        };

        // === 3. 启动录制 ===

        let metadata = RecordingMetadata {
            notes: format!("CLI recording, duration={}", duration),
            operator: std::env::var("USER").unwrap_or_else(|_| "unknown".to_string()),
        };

        let config = RecordingConfig {
            output_path: PathBuf::from(&output_path),
            stop_condition,
            metadata,
        };

        let (standby, handle) = standby.start_recording(config)?;

        println!("🔴 开始录制...");
        println!();

        // === 4. 循环逻辑（封装为独立函数，防止 panic 导致数据丢失）🛡️ ===

        let loop_result = Self::recording_loop(&handle, &running, duration);

        // === 5. 无论循环如何结束，都尝试保存数据 🛡️ ===

        println!();
        println!("⏳ 正在保存录制...");

        let (_standby, stats) = standby.stop_recording(handle)?;

        // === 6. 然后再处理循环的错误（如果有） ===

        loop_result?;

        Ok(stats)
    }

    /// 录制循环（独立函数，错误不会影响数据保存）🛡️
    ///
    /// 此函数的 panic 不会影响数据保存，
    /// 因为 `stop_recording()` 在外层保证调用。
    ///
    /// ⚡ UX 优化：100ms 轮询，每 1 秒刷新 UI
    /// - Ctrl-C 响应时间：1 秒 → 100ms
    /// - 时长精度：±1 秒 → ±100ms
    fn recording_loop(
        handle: &piper_sdk::RecordingHandle,
        running: &Arc<AtomicBool>,
        duration: u64,
    ) -> Result<()> {
        let start = Instant::now();
        let timeout = if duration > 0 {
            Some(Duration::from_secs(duration))
        } else {
            None
        };

        let mut ticks = 0usize;

        while running.load(Ordering::Relaxed) {
            // 1. 检查超时（精度 100ms）
            if matches!(timeout, Some(duration) if start.elapsed() >= duration) {
                println!();
                println!("⏳ 录制时长已到");
                break;
            }

            // 2. ✅ 检查 OnCanId 停止条件（Driver 层检测到触发帧）
            if handle.is_stop_requested() {
                println!();
                println!("🛑 检测到停止触发帧");
                break;
            }

            // 3. ⚡ 短暂休眠（提升 Ctrl-C 响应速度）
            std::thread::sleep(Duration::from_millis(100));
            ticks += 1;

            // 3. 每 1 秒（10 次 100ms）刷新一次 UI
            if ticks.is_multiple_of(10) {
                // 显示进度（使用 SDK 暴露的 getter 方法）
                let elapsed = start.elapsed().as_secs();
                let current_count = handle.frame_count(); // ✅ 使用新增方法
                let dropped = handle.dropped_count();

                // ⚠️ 丢帧警告（缓冲区即将满）
                if dropped > 100 {
                    eprint!("\r⚠️  已丢失 {} 帧 | ", dropped);
                }

                // 清除上一行并更新
                print!(
                    "\r🔴 正在录制... [{:02}:{:02}] | 帧数: {} | 丢帧: {}",
                    elapsed / 60,
                    elapsed % 60,
                    current_count,
                    dropped
                );
                std::io::stdout().flush()?;
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_record_command_creation() {
        let cmd = RecordCommand {
            output: "test.bin".to_string(),
            interface: Some("can0".to_string()),
            serial: None,
            duration: 10,
            stop_on_id: Some(0x2A5),
            force: false,
        };

        assert_eq!(cmd.output, "test.bin");
        assert_eq!(cmd.duration, 10);
        assert_eq!(cmd.stop_on_id, Some(0x2A5));
        assert!(!cmd.force);
    }

    #[test]
    fn test_record_command_defaults() {
        let cmd = RecordCommand {
            output: "recording.bin".to_string(),
            interface: None,
            serial: None,
            duration: 0,
            stop_on_id: None,
            force: false,
        };

        assert_eq!(cmd.output, "recording.bin");
        assert_eq!(cmd.duration, 0);
        assert!(!cmd.force);
    }

    #[test]
    fn test_record_command_with_serial() {
        let cmd = RecordCommand {
            output: "test.bin".to_string(),
            interface: None,
            serial: Some("ABC123".to_string()),
            duration: 30,
            stop_on_id: None,
            force: true,
        };

        assert_eq!(cmd.output, "test.bin");
        assert_eq!(cmd.duration, 30);
        assert_eq!(cmd.serial, Some("ABC123".to_string()));
        assert!(cmd.interface.is_none());
        assert!(cmd.force);
    }
}
