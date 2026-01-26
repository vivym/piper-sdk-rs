//! replay 命令
//!
//! 回放录制的数据

use anyhow::Result;
use clap::Args;
use piper_tools::PiperRecording;

use crate::utils;

/// 回放命令参数
#[derive(Args, Debug)]
pub struct ReplayCommand {
    /// 录制文件路径
    #[arg(short, long)]
    pub input: String,

    /// 回放速度倍数（1.0 = 正常速度）
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
        println!("🔄 回放录制: {}", self.input);

        // 检查文件是否存在
        if !std::path::Path::new(&self.input).exists() {
            anyhow::bail!("录制文件不存在: {}", self.input);
        }

        // ⚠️ 安全确认
        if self.confirm || self.speed > 1.0 {
            println!("⚠️  回放速度: {}x", self.speed);
            if self.speed > 1.0 {
                println!("⚠️  高速回放可能不安全！");
            }

            let confirmed = utils::prompt_confirmation("确定要回放吗？", false)?;

            if !confirmed {
                println!("❌ 操作已取消");
                return Ok(());
            }

            println!("✅ 已确认");
        }

        println!("⏳ 加载录制文件...");

        // 加载录制
        let recording = PiperRecording::load(&self.input)?;

        println!("📊 录制信息:");
        println!("  文件: {}", self.input);
        println!("  版本: {}", recording.version);
        println!("  帧数: {}", recording.frame_count());
        if let Some(duration) = recording.duration() {
            println!("  时长: {:?}", duration);
        }
        println!("  接口: {}", recording.metadata.interface);
        println!("  速度: {}x", self.speed);
        println!();

        println!("⏳ 回放中...");

        // 注意：实际回放需要发送 CAN 帧
        // 由于架构限制，这里只能显示进度
        // TODO: 需要访问 driver 层的 send_frame 方法

        let total_frames = recording.frame_count();

        if recording.frames.is_empty() {
            println!("⚠️  录制文件为空");
            return Ok(());
        }

        // 获取第一个帧的时间戳作为基准
        let base_timestamp = recording.frames[0].timestamp_us;

        println!("📝 开始回放 {} 帧...", total_frames);
        println!("💡 注意：当前仅显示进度，实际 CAN 帧发送需要底层访问");
        println!();

        for (i, frame) in recording.frames.iter().enumerate() {
            // 计算相对时间（微秒）
            let elapsed_us = frame.timestamp_us.saturating_sub(base_timestamp);
            let elapsed_ms = elapsed_us / 1000;

            // 应用速度控制
            let delay_ms = if self.speed > 0.0 {
                (elapsed_ms as f64 / self.speed) as u64
            } else {
                elapsed_ms
            };

            // 进度显示
            if i % 100 == 0 || i == total_frames - 1 {
                print!(
                    "\r回放进度: {}/{} 帧 ({}%)",
                    i + 1,
                    total_frames,
                    ((i + 1) * 100 / total_frames)
                );
                use std::io::Write;
                std::io::stdout().flush().ok();
            }

            // TODO: 实际发送 CAN 帧
            // 需要访问 driver 层的 Piper::send_frame 方法
            // piper_sdk::driver::Piper::send_frame(&piper_frame)

            // 控制回放速度
            if delay_ms > 0 {
                tokio::time::sleep(tokio::time::Duration::from_millis(delay_ms)).await;
            }
        }

        println!("\r✅ 回放完成: {} 帧", total_frames);

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
}
