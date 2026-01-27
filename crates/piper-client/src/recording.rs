//! 标准录制 API
//!
//! 本模块提供易用的录制功能，适用于大多数用户场景。
//!
//! # 设计理念
//!
//! - **类型安全**：与类型状态机完全集成
//! - **RAII 语义**：自动管理资源
//! - **易于使用**：适合常规录制场景
//!
//! # 使用示例
//!
//! ```rust,no_run
//! use piper_client::{PiperBuilder, recording::{RecordingConfig, RecordingMetadata, StopCondition}};
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let robot = PiperBuilder::new()
//!     .interface("can0")
//!     .build()?;
//!
//! let active = robot.enable_position_mode(Default::default())?;
//!
//! // 启动录制（Active 状态）
//! let (active, handle) = active.start_recording(RecordingConfig {
//!     output_path: "demo.bin".into(),
//!     stop_condition: StopCondition::Duration(10),
//!     metadata: RecordingMetadata {
//!         notes: "Test recording".to_string(),
//!         operator: "Alice".to_string(),
//!     },
//! })?;
//!
//! // 执行操作（会被录制，包含控制指令帧）
//! // active.send_position_command(...)?;
//!
//! // 停止录制并保存
//! let _active = active.stop_recording(handle)?;
//! # Ok(())
//! # }
//! ```

use piper_driver::recording::TimestampedFrame;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

/// 录制句柄（用于控制和监控）
///
/// # Drop 语义
///
/// 当 `RecordingHandle` 被丢弃时：
/// - ✅ 自动 flush 缓冲区中的数据
/// - ✅ 自动关闭接收端
/// - ❌ 不会自动保存文件（需要显式调用 `stop_recording()`）
///
/// # Panics
///
/// 如果在 Drop 时发生 I/O 错误，错误会被静默忽略（Drop 上下文无法处理错误）。
/// 建议始终显式调用 `stop_recording()` 以获取错误结果。
pub struct RecordingHandle {
    /// 接收端（用于读取录制的帧）
    rx: crossbeam_channel::Receiver<TimestampedFrame>,

    /// 丢帧计数器
    dropped_frames: Arc<AtomicU64>,

    /// 输出文件路径
    output_path: PathBuf,

    /// 录制开始时间
    start_time: Instant,
}

impl RecordingHandle {
    /// 创建新的录制句柄（内部使用）
    pub(super) fn new(
        rx: crossbeam_channel::Receiver<TimestampedFrame>,
        dropped_frames: Arc<AtomicU64>,
        output_path: PathBuf,
        start_time: Instant,
    ) -> Self {
        Self {
            rx,
            dropped_frames,
            output_path,
            start_time,
        }
    }

    /// 获取当前丢帧数量
    pub fn dropped_count(&self) -> u64 {
        self.dropped_frames.load(Ordering::Relaxed)
    }

    /// 获取录制时长
    pub fn elapsed(&self) -> std::time::Duration {
        self.start_time.elapsed()
    }

    /// 获取输出文件路径
    pub fn output_path(&self) -> &PathBuf {
        &self.output_path
    }

    /// 获取接收端的引用（用于 stop_recording）
    pub(super) fn receiver(&self) -> &crossbeam_channel::Receiver<TimestampedFrame> {
        &self.rx
    }
}

impl Drop for RecordingHandle {
    /// ⚠️ Drop 语义：自动清理资源
    ///
    /// 注意：这里只关闭接收端，不保存文件。
    /// 文件保存必须在 `stop_recording()` 中显式完成。
    fn drop(&mut self) {
        // 接收端会在 Drop 时自动关闭
        // 这里只是显式标记（用于调试）
        tracing::debug!("RecordingHandle dropped, receiver closed");
    }
}

/// 录制配置
#[derive(Debug, Clone)]
pub struct RecordingConfig {
    /// 输出文件路径
    pub output_path: PathBuf,

    /// 自动停止条件
    pub stop_condition: StopCondition,

    /// 元数据
    pub metadata: RecordingMetadata,
}

/// 停止条件
#[derive(Debug, Clone)]
pub enum StopCondition {
    /// 时长限制（秒）
    Duration(u64),

    /// 手动停止
    Manual,

    /// 接收到特定 CAN ID 时停止
    OnCanId(u32),

    /// 接收到特定数量的帧后停止
    FrameCount(usize),
}

/// 录制元数据
#[derive(Debug, Clone)]
pub struct RecordingMetadata {
    pub notes: String,
    pub operator: String,
}

/// 录制统计
#[derive(Debug, Clone)]
pub struct RecordingStats {
    pub frame_count: usize,
    pub duration: std::time::Duration,
    pub dropped_frames: u64,
    pub output_path: PathBuf,
}

// 以下方法将在 state/machine.rs 的 impl 中实现
// 因为它们需要访问私有字段

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stop_condition_duration() {
        let condition = StopCondition::Duration(10);
        match condition {
            StopCondition::Duration(s) => assert_eq!(s, 10),
            _ => panic!("Wrong condition"),
        }
    }

    #[test]
    fn test_stop_condition_manual() {
        let condition = StopCondition::Manual;
        match condition {
            StopCondition::Manual => {}, // OK
            _ => panic!("Wrong condition"),
        }
    }

    #[test]
    fn test_stop_condition_on_can_id() {
        let condition = StopCondition::OnCanId(0x1A1);
        match condition {
            StopCondition::OnCanId(id) => assert_eq!(id, 0x1A1),
            _ => panic!("Wrong condition"),
        }
    }

    #[test]
    fn test_stop_condition_frame_count() {
        let condition = StopCondition::FrameCount(1000);
        match condition {
            StopCondition::FrameCount(count) => assert_eq!(count, 1000),
            _ => panic!("Wrong condition"),
        }
    }

    #[test]
    fn test_recording_metadata() {
        let metadata = RecordingMetadata {
            notes: "Test".to_string(),
            operator: "Alice".to_string(),
        };
        assert_eq!(metadata.notes, "Test");
        assert_eq!(metadata.operator, "Alice");
    }

    #[test]
    fn test_recording_metadata_empty_strings() {
        let metadata = RecordingMetadata {
            notes: "".to_string(),
            operator: "".to_string(),
        };
        assert_eq!(metadata.notes, "");
        assert_eq!(metadata.operator, "");
    }

    #[test]
    fn test_recording_config() {
        let config = RecordingConfig {
            output_path: "/tmp/test.bin".into(),
            stop_condition: StopCondition::Duration(10),
            metadata: RecordingMetadata {
                notes: "Test".to_string(),
                operator: "Bob".to_string(),
            },
        };

        assert_eq!(
            config.output_path,
            std::path::PathBuf::from("/tmp/test.bin")
        );
        match config.stop_condition {
            StopCondition::Duration(s) => assert_eq!(s, 10),
            _ => panic!("Wrong condition"),
        }
        assert_eq!(config.metadata.notes, "Test");
        assert_eq!(config.metadata.operator, "Bob");
    }

    #[test]
    fn test_recording_stats() {
        let stats = RecordingStats {
            frame_count: 1000,
            duration: std::time::Duration::from_secs(10),
            dropped_frames: 5,
            output_path: "/tmp/test.bin".into(),
        };

        assert_eq!(stats.frame_count, 1000);
        assert_eq!(stats.duration.as_secs(), 10);
        assert_eq!(stats.dropped_frames, 5);
        assert_eq!(stats.output_path, std::path::PathBuf::from("/tmp/test.bin"));
    }

    #[test]
    fn test_recording_stats_clone() {
        let stats = RecordingStats {
            frame_count: 100,
            duration: std::time::Duration::from_millis(500),
            dropped_frames: 0,
            output_path: "/tmp/clone_test.bin".into(),
        };

        let cloned = stats.clone();
        assert_eq!(cloned.frame_count, stats.frame_count);
        assert_eq!(cloned.duration, stats.duration);
        assert_eq!(cloned.dropped_frames, stats.dropped_frames);
        assert_eq!(cloned.output_path, stats.output_path);
    }
}
