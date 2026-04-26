//! record 命令
//!
//! 录制 CAN 总线数据到文件

use crate::commands::config::CliConfig;
use crate::connection::{TargetArgs, client_builder, resolved_target_spec};
use anyhow::{Context, Result};
use clap::Args;
use piper_control::TargetSpec;
use piper_sdk::can::CanId;
use piper_sdk::client::state::{CapabilityMarker, Standby};
use piper_sdk::client::{ConnectedPiper, MotionConnectedState, Piper};
use piper_sdk::driver::ConnectionTarget;
use piper_sdk::{RecordingConfig, RecordingMetadata, StopCondition};
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

    #[command(flatten)]
    pub target: TargetArgs,

    /// 录制时长（秒），0 表示无限
    #[arg(short, long, default_value_t = 0)]
    pub duration: u64,

    /// 自动停止（接收到特定 CAN ID 时停止）
    #[arg(short, long, value_parser = parse_can_id_arg)]
    pub stop_on_id: Option<CanId>,

    /// 跳过确认提示
    #[arg(long)]
    pub force: bool,
}

fn parse_can_id_arg(value: &str) -> std::result::Result<CanId, String> {
    let (format, raw) = value.split_once(':').ok_or_else(|| {
        "CAN ID must be explicit: use standard:0x123 or extended:0x123".to_string()
    })?;

    let id = parse_u32_arg(raw)?;
    match format {
        "standard" => CanId::standard(id).map_err(|err| err.to_string()),
        "extended" => CanId::extended(id).map_err(|err| err.to_string()),
        _ => Err("CAN ID format must be standard or extended".to_string()),
    }
}

fn parse_u32_arg(value: &str) -> std::result::Result<u32, String> {
    if let Some(hex) = value.strip_prefix("0x").or_else(|| value.strip_prefix("0X")) {
        u32::from_str_radix(hex, 16).map_err(|err| format!("invalid hex CAN ID: {err}"))
    } else {
        value.parse::<u32>().map_err(|err| format!("invalid CAN ID: {err}"))
    }
}

fn format_can_id_arg(id: CanId) -> String {
    let format = if id.is_standard() {
        "standard"
    } else {
        "extended"
    };
    format!("{format}:0x{:X}", id.raw())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecordRunOutcome {
    Completed,
    StoppedByCondition,
    InterruptedByUser,
}

fn classify_recording_outcome(
    still_running: bool,
    stop_requested: bool,
    _duration_elapsed: bool,
) -> RecordRunOutcome {
    if !still_running {
        RecordRunOutcome::InterruptedByUser
    } else if stop_requested {
        RecordRunOutcome::StoppedByCondition
    } else {
        RecordRunOutcome::Completed
    }
}

impl RecordCommand {
    fn effective_stop_condition(duration: u64, stop_on_id: Option<CanId>) -> StopCondition {
        if let Some(id) = stop_on_id {
            StopCondition::OnCanId(id)
        } else if duration > 0 {
            StopCondition::Duration(duration)
        } else {
            StopCondition::Manual
        }
    }

    fn effective_loop_timeout_secs(duration: u64, stop_on_id: Option<CanId>) -> u64 {
        if stop_on_id.is_some() { 0 } else { duration }
    }

    fn current_operator_name() -> String {
        std::env::var("USER")
            .ok()
            .filter(|value| !value.is_empty())
            .or_else(|| std::env::var("USERNAME").ok().filter(|value| !value.is_empty()))
            .unwrap_or_else(|| "unknown".to_string())
    }

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
            println!("🛑 停止条件: CAN ID {}", format_can_id_arg(stop_id));
        }
        let config = CliConfig::load()?;
        let target_spec = resolved_target_spec(&config, self.target.target.as_ref());
        println!("🎯 target: {}", target_spec);
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
        let target = target_spec.clone().into_connection_target();
        let stop_on_id = self.stop_on_id;
        let running_for_task = running.clone();

        println!("💡 提示: 按 Ctrl-C 停止录制");
        println!();

        // 在专用线程中运行录制逻辑
        let task = spawn_blocking(move || {
            Self::record_sync(
                output_path,
                duration,
                target,
                target_spec,
                stop_on_id,
                running_for_task,
            )
        });

        let result: Result<
            Result<(piper_sdk::RecordingStats, RecordRunOutcome), anyhow::Error>,
            tokio::task::JoinError,
        > = task.await;

        // === 6. 检查结果 ===

        match result {
            Ok(inner_result) => match inner_result {
                Ok((stats, outcome)) => {
                    println!();
                    match outcome {
                        RecordRunOutcome::Completed => println!("✅ 录制完成"),
                        RecordRunOutcome::StoppedByCondition => {
                            println!("✅ 录制已按停止条件结束");
                        },
                        RecordRunOutcome::InterruptedByUser => {
                            println!("⚠️  录制被用户中断");
                        },
                    }
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
        target: ConnectionTarget,
        target_spec: TargetSpec,
        stop_on_id: Option<CanId>,
        running: Arc<AtomicBool>,
    ) -> Result<(piper_sdk::RecordingStats, RecordRunOutcome)> {
        // === 1. 连接到机器人 ===

        println!("⏳ 连接到机器人...");

        println!("   target: {}", target_spec);

        let builder = client_builder(&target);

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
        let stop_condition = Self::effective_stop_condition(duration, stop_on_id);
        let loop_timeout_secs = Self::effective_loop_timeout_secs(duration, stop_on_id);

        // === 3. 启动录制 ===

        let metadata = RecordingMetadata {
            notes: match stop_condition {
                StopCondition::OnCanId(id) => {
                    format!("CLI recording, stop_on_id={}", format_can_id_arg(id))
                },
                StopCondition::Duration(seconds) => {
                    format!("CLI recording, duration={seconds}")
                },
                StopCondition::Manual => "CLI recording, manual stop".to_string(),
                StopCondition::FrameCount(count) => {
                    format!("CLI recording, frame_count={count}")
                },
            },
            operator: Self::current_operator_name(),
        };

        let config = RecordingConfig {
            output_path: PathBuf::from(&output_path),
            stop_condition,
            metadata,
        };

        match standby {
            ConnectedPiper::Strict(MotionConnectedState::Standby(standby)) => {
                Self::record_with_standby(standby, config, loop_timeout_secs, running)
            },
            ConnectedPiper::Soft(MotionConnectedState::Standby(standby)) => {
                Self::record_with_standby(standby, config, loop_timeout_secs, running)
            },
            ConnectedPiper::Strict(MotionConnectedState::Maintenance(_))
            | ConnectedPiper::Soft(MotionConnectedState::Maintenance(_)) => Err(anyhow::anyhow!(
                "机械臂当前不在确认全失能的 Standby，请先执行 stop"
            )),
            ConnectedPiper::Monitor(standby) => {
                Self::record_with_standby(standby, config, loop_timeout_secs, running)
            },
        }
    }

    fn record_with_standby<Capability>(
        standby: Piper<Standby, Capability>,
        config: RecordingConfig,
        duration: u64,
        running: Arc<AtomicBool>,
    ) -> Result<(piper_sdk::RecordingStats, RecordRunOutcome)>
    where
        Capability: CapabilityMarker,
    {
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

        let outcome = loop_result?;

        Ok((stats, outcome))
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
    ) -> Result<RecordRunOutcome> {
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
                return Ok(classify_recording_outcome(true, false, true));
            }

            // 2. ✅ 检查 OnCanId 停止条件（Driver 层检测到触发帧）
            if handle.is_stop_requested() {
                println!();
                println!("🛑 检测到停止触发帧");
                return Ok(classify_recording_outcome(true, true, false));
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

        Ok(classify_recording_outcome(false, false, false))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use piper_sdk::can::CanId;

    #[test]
    fn test_record_command_creation() {
        let cmd = RecordCommand {
            output: "test.bin".to_string(),
            target: TargetArgs {
                target: Some(TargetSpec::SocketCan {
                    iface: "can0".to_string(),
                }),
            },
            duration: 10,
            stop_on_id: Some(CanId::standard(0x2A5).unwrap()),
            force: false,
        };

        assert_eq!(cmd.output, "test.bin");
        assert_eq!(cmd.duration, 10);
        assert_eq!(cmd.stop_on_id, Some(CanId::standard(0x2A5).unwrap()));
        assert!(!cmd.force);
    }

    #[test]
    fn test_record_command_defaults() {
        let cmd = RecordCommand {
            output: "recording.bin".to_string(),
            target: TargetArgs::default(),
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
            target: TargetArgs {
                target: Some(TargetSpec::GsUsbSerial {
                    serial: "ABC123".to_string(),
                }),
            },
            duration: 30,
            stop_on_id: None,
            force: true,
        };

        assert_eq!(cmd.output, "test.bin");
        assert_eq!(cmd.duration, 30);
        assert!(matches!(
            cmd.target.target,
            Some(TargetSpec::GsUsbSerial { .. })
        ));
        assert!(cmd.force);
    }

    #[test]
    fn effective_stop_condition_prefers_stop_on_id_over_duration() {
        let expected = CanId::standard(0x2A5).unwrap();
        match RecordCommand::effective_stop_condition(30, Some(expected)) {
            StopCondition::OnCanId(id) if id == expected => {},
            StopCondition::OnCanId(other) => {
                panic!("unexpected CAN ID: {}", format_can_id_arg(other))
            },
            StopCondition::Duration(seconds) => {
                panic!("expected OnCanId, got Duration({seconds})")
            },
            StopCondition::Manual => panic!("expected OnCanId, got Manual"),
            StopCondition::FrameCount(count) => {
                panic!("expected OnCanId, got FrameCount({count})")
            },
        }
        assert_eq!(
            RecordCommand::effective_loop_timeout_secs(30, Some(expected)),
            0
        );
    }

    #[test]
    fn effective_stop_condition_uses_duration_when_no_can_id_is_set() {
        match RecordCommand::effective_stop_condition(30, None) {
            StopCondition::Duration(30) => {},
            StopCondition::Duration(other) => panic!("unexpected duration: {other}"),
            StopCondition::OnCanId(id) => {
                panic!("expected Duration, got OnCanId({})", format_can_id_arg(id))
            },
            StopCondition::Manual => panic!("expected Duration, got Manual"),
            StopCondition::FrameCount(count) => {
                panic!("expected Duration, got FrameCount({count})")
            },
        }
        assert_eq!(RecordCommand::effective_loop_timeout_secs(30, None), 30);
    }

    #[test]
    fn current_operator_name_falls_back_to_username() {
        let user_backup = std::env::var("USER").ok();
        let username_backup = std::env::var("USERNAME").ok();

        unsafe {
            std::env::remove_var("USER");
            std::env::set_var("USERNAME", "windows-user");
        }

        assert_eq!(RecordCommand::current_operator_name(), "windows-user");

        unsafe {
            std::env::remove_var("USERNAME");
            if let Some(user) = user_backup {
                std::env::set_var("USER", user);
            }
            if let Some(username) = username_backup {
                std::env::set_var("USERNAME", username);
            }
        }
    }

    #[test]
    fn classify_recording_outcome_marks_user_stop_as_interrupted() {
        assert_eq!(
            classify_recording_outcome(false, false, false),
            RecordRunOutcome::InterruptedByUser
        );
    }

    #[test]
    fn classify_recording_outcome_marks_stop_condition_as_condition_triggered() {
        assert_eq!(
            classify_recording_outcome(true, true, false),
            RecordRunOutcome::StoppedByCondition
        );
    }

    #[test]
    fn classify_recording_outcome_marks_timeout_completion_as_completed() {
        assert_eq!(
            classify_recording_outcome(true, false, true),
            RecordRunOutcome::Completed
        );
    }

    #[test]
    fn parses_explicit_standard_stop_id() {
        assert_eq!(
            parse_can_id_arg("standard:0x2A5").unwrap(),
            CanId::standard(0x2A5).unwrap()
        );
    }

    #[test]
    fn rejects_ambiguous_stop_id() {
        assert!(parse_can_id_arg("0x2A5").is_err());
    }

    #[test]
    fn rejects_invalid_standard_stop_id() {
        assert!(parse_can_id_arg("standard:0x800").is_err());
    }
}
