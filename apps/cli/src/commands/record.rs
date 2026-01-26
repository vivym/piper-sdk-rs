//! 录制命令
//!
//! 录制 CAN 总线数据到文件

use anyhow::Result;
use clap::Args;
use piper_tools::{PiperRecording, RecordingMetadata, TimestampSource, TimestampedFrame};
use std::time::SystemTime;

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
}

impl RecordCommand {
    /// 执行录制
    pub async fn execute(&self, config: &crate::modes::oneshot::OneShotConfig) -> Result<()> {
        use piper_sdk::driver::PiperBuilder;
        use std::time::Duration;

        println!("⏳ 连接到机器人...");

        let interface_str =
            self.interface.as_deref().or(config.interface.as_deref()).unwrap_or("can0");

        // 创建录制
        let metadata = RecordingMetadata::new(interface_str.to_string(), 1_000_000);
        let mut recording = PiperRecording::new(metadata);

        // 模拟录制（实际应该从 CAN 总线读取）
        let start_time = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH)?.as_secs();

        let duration = self.duration;
        let max_frames = if duration > 0 {
            duration * 1000 // 假设 1000Hz
        } else {
            1000 // 默认录制 1000 帧
        };

        let mut frame_count = 0;

        // 连接到机器人读取状态
        let robot = PiperBuilder::new().interface(interface_str).build()?;

        println!("✅ 已连接，开始录制...");

        let start = std::time::Instant::now();
        let stop_id = self.stop_on_id;

        loop {
            // 检查时长限制
            if duration > 0 && start.elapsed() >= Duration::from_secs(duration) {
                println!("\n⏱️  达到时长限制");
                break;
            }

            // 读取状态（触发 CAN 接收）
            let _position = robot.get_joint_position();
            let _end_pose = robot.get_end_pose();

            // 模拟录制 CAN 帧
            // TODO: 实际实现需要访问 driver 层的 CAN 帧
            let can_id: u32 = (0x2A5 + (frame_count % 6)).try_into().unwrap();
            let frame = TimestampedFrame::new(
                start_time * 1_000_000 + frame_count * 1000,
                can_id,
                vec![frame_count as u8; 8],
                TimestampSource::Hardware,
            );

            recording.add_frame(frame);
            frame_count += 1;

            // 进度显示
            if frame_count % 100 == 0 {
                print!(
                    "\r录制中: {} 帧 (时长: {:.1}s)",
                    frame_count,
                    start.elapsed().as_secs_f64()
                );
                use std::io::Write;
                std::io::stdout().flush().ok();
            }

            // 检查帧数限制
            if frame_count >= max_frames {
                println!("\n✅ 达到帧数限制");
                break;
            }

            // 检查停止条件
            if matches!(stop_id, Some(id) if can_id == id) {
                println!("\n✅ 接收到停止 ID 0x{:03X}", stop_id.unwrap());
                break;
            }

            // 小延迟，避免 100% CPU
            tokio::time::sleep(Duration::from_millis(1)).await;
        }

        println!("\n✅ 录制完成: {} 帧", recording.frame_count());

        // 保存录制
        println!("💾 保存到: {}", self.output);
        recording.save(&self.output)?;
        println!("✅ 保存完成");

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
        };

        assert_eq!(cmd.output, "test.bin");
        assert_eq!(cmd.duration, 10);
        assert_eq!(cmd.stop_on_id, Some(0x2A5));
    }
}
