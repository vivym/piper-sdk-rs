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
//! use piper_client::{MotionConnectedPiper, MotionConnectedState, PiperBuilder, recording::{RecordingConfig, RecordingMetadata, StopCondition}};
//! use piper_client::state::{MotionCapability, Piper, Standby};
//!
//! # fn run_example<C: MotionCapability>(
//! #     standby: Piper<Standby, C>,
//! # ) -> Result<(), Box<dyn std::error::Error>> {
//! let active = standby.enable_position_mode(Default::default())?;
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
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let robot = PiperBuilder::new()
//!     .socketcan("can0")
//!     .build()?;
//!
//! match robot.require_motion()? {
//!     MotionConnectedPiper::Strict(MotionConnectedState::Standby(standby)) => run_example(standby)?,
//!     MotionConnectedPiper::Soft(MotionConnectedState::Standby(standby)) => run_example(standby)?,
//!     MotionConnectedPiper::Strict(MotionConnectedState::Maintenance(_))
//!     | MotionConnectedPiper::Soft(MotionConnectedState::Maintenance(_)) => {
//!         return Err("robot is not in confirmed Standby".into());
//!     }
//! }
//! # Ok(())
//! # }
//! ```

use piper_driver::recording::TimestampedFrame;
use piper_driver::{HookHandle, HookManager};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, RwLock};
use std::time::Instant;

/// 录制句柄（用于控制和监控）
///
/// # Drop 语义
///
/// 当 `RecordingHandle` 被丢弃时：
/// - ✅ 自动请求停止录制
/// - ✅ 自动解绑 Driver 侧 recording hook
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

    /// 帧计数器（从 Driver 层传递）
    frame_counter: Arc<AtomicU64>,

    /// 停止请求标记（用于 Manual 停止）
    stop_requested: Arc<AtomicBool>,

    /// 输出文件路径
    output_path: PathBuf,

    /// 用户提供的录制元数据。
    metadata: RecordingMetadata,

    /// 录制开始时间（Unix 时间戳，秒）。
    start_time_unix_secs: u64,

    /// 录制开始时间
    start_time: Instant,

    /// Driver hook 注册信息，用于在 stop_recording/Drop 时解绑 callback。
    hook_registration: Mutex<Option<(Arc<RwLock<HookManager>>, HookHandle)>>,

    /// 自动停止条件。
    stop_condition: RecordingStopCondition,
}

pub(super) struct RecordingHandleParts {
    pub rx: crossbeam_channel::Receiver<TimestampedFrame>,
    pub dropped_frames: Arc<AtomicU64>,
    pub frame_counter: Arc<AtomicU64>,
    pub stop_requested: Arc<AtomicBool>,
    pub output_path: PathBuf,
    pub metadata: RecordingMetadata,
    pub start_time_unix_secs: u64,
    pub start_time: Instant,
    pub hook_manager: Arc<RwLock<HookManager>>,
    pub hook_handle: HookHandle,
    pub stop_condition: RecordingStopCondition,
}

#[derive(Debug, Clone, Copy)]
pub(super) enum RecordingStopCondition {
    Manual,
    OnCanId,
    Duration(std::time::Duration),
    FrameCount(u64),
}

impl RecordingHandle {
    /// 创建新的录制句柄（内部使用）
    ///
    /// # 参数
    ///
    /// - `stop_requested`: 可选的外部停止标志（用于 Driver 层的 `OnCanId` 停止条件）
    ///   - `None`: 创建新的内部停止标志（用于 `Manual` 停止条件）
    ///   - `Some(external)`: 使用 Driver 层提供的停止标志（用于 `OnCanId` 停止条件）
    pub(super) fn new(parts: RecordingHandleParts) -> Self {
        Self {
            rx: parts.rx,
            dropped_frames: parts.dropped_frames,
            frame_counter: parts.frame_counter,
            stop_requested: parts.stop_requested,
            output_path: parts.output_path,
            metadata: parts.metadata,
            start_time_unix_secs: parts.start_time_unix_secs,
            start_time: parts.start_time,
            hook_registration: Mutex::new(Some((parts.hook_manager, parts.hook_handle))),
            stop_condition: parts.stop_condition,
        }
    }

    fn refresh_stop_condition(&self) {
        if self.stop_requested.load(Ordering::Acquire) {
            return;
        }

        let should_stop = match self.stop_condition {
            RecordingStopCondition::Manual | RecordingStopCondition::OnCanId => false,
            RecordingStopCondition::Duration(limit) => self.start_time.elapsed() >= limit,
            RecordingStopCondition::FrameCount(limit) => {
                self.frame_counter.load(Ordering::Relaxed) >= limit
            },
        };

        if should_stop {
            self.stop_requested.store(true, Ordering::Release);
        }
    }

    /// 获取当前已录制的帧数（线程安全，无阻塞）
    ///
    /// # 返回
    ///
    /// 当前已成功录制的帧数
    pub fn frame_count(&self) -> u64 {
        self.refresh_stop_condition();
        self.frame_counter.load(Ordering::Relaxed)
    }

    /// 获取当前丢帧数量
    pub fn dropped_count(&self) -> u64 {
        self.refresh_stop_condition();
        self.dropped_frames.load(Ordering::Relaxed)
    }

    /// 检查是否已请求停止（用于循环条件判断）
    pub fn is_stop_requested(&self) -> bool {
        self.refresh_stop_condition();
        self.stop_requested.load(Ordering::Relaxed)
    }

    /// 手动停止录制（请求停止）
    pub fn stop(&self) {
        self.stop_requested.store(true, Ordering::SeqCst);
    }

    /// 获取录制时长
    pub fn elapsed(&self) -> std::time::Duration {
        let elapsed = self.start_time.elapsed();
        self.refresh_stop_condition();
        elapsed
    }

    /// 获取输出文件路径
    pub fn output_path(&self) -> &PathBuf {
        &self.output_path
    }

    pub(super) fn metadata(&self) -> &RecordingMetadata {
        &self.metadata
    }

    pub(super) fn start_time_unix_secs(&self) -> u64 {
        self.start_time_unix_secs
    }

    /// 获取接收端的引用（用于 stop_recording）
    pub(super) fn receiver(&self) -> &crossbeam_channel::Receiver<TimestampedFrame> {
        &self.rx
    }

    /// 解绑当前录制 hook。重复调用是幂等的。
    pub(super) fn detach_hook(&self) {
        let registration = match self.hook_registration.lock() {
            Ok(mut guard) => guard.take(),
            Err(poisoned) => poisoned.into_inner().take(),
        };

        if let Some((hook_manager, hook_handle)) = registration
            && let Ok(mut hooks) = hook_manager.write()
        {
            hooks.remove_callback(hook_handle);
        }
    }
}

impl Drop for RecordingHandle {
    /// ⚠️ Drop 语义：自动清理资源
    ///
    /// 注意：这里只请求停止录制并解绑 hook，不保存文件。
    /// 文件保存必须在 `stop_recording()` 中显式完成。
    fn drop(&mut self) {
        self.stop_requested.store(true, Ordering::SeqCst);
        self.detach_hook();
        tracing::debug!("RecordingHandle dropped, callback removed");
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
