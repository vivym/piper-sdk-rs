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

use piper_can::CanId;
use piper_driver::recording::{RecordedFrameEvent, TimestampProvenance, TimestampedFrame};
use piper_driver::{FrameCallback, HookHandle, HookManager};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::sync::{Mutex, RwLock};
use std::time::{Duration, Instant};

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

    /// Shared accept/close gate used by the callback and stop path.
    gate: Arc<Mutex<RecordingGate>>,

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
}

pub(super) struct RecordingHandleParts {
    pub rx: crossbeam_channel::Receiver<TimestampedFrame>,
    pub dropped_frames: Arc<AtomicU64>,
    pub frame_counter: Arc<AtomicU64>,
    pub stop_requested: Arc<AtomicBool>,
    pub gate: Arc<Mutex<RecordingGate>>,
    pub output_path: PathBuf,
    pub metadata: RecordingMetadata,
    pub start_time_unix_secs: u64,
    pub start_time: Instant,
    pub hook_manager: Arc<RwLock<HookManager>>,
    pub hook_handle: HookHandle,
}

#[derive(Debug, Clone, Copy)]
pub(super) enum RecordingStopCondition {
    Manual,
    OnCanId(CanId),
    Duration(Duration),
    FrameCount(u64),
}

#[derive(Debug)]
pub(super) struct RecordingGate {
    accepting: bool,
    accepted_count: u64,
    deadline_us: Option<u64>,
    stop_on_id: Option<CanId>,
    frame_count_limit: Option<u64>,
}

impl RecordingGate {
    fn new(condition: RecordingStopCondition) -> Self {
        let (deadline_us, stop_on_id, frame_count_limit) = match condition {
            RecordingStopCondition::Manual => (None, None, None),
            RecordingStopCondition::Duration(duration) => (
                Some(duration.as_micros().min(u128::from(u64::MAX)) as u64),
                None,
                None,
            ),
            RecordingStopCondition::OnCanId(id) => (None, Some(id), None),
            RecordingStopCondition::FrameCount(limit) => (None, None, Some(limit)),
        };

        Self {
            accepting: true,
            accepted_count: 0,
            deadline_us,
            stop_on_id,
            frame_count_limit,
        }
    }

    fn close(&mut self) {
        self.accepting = false;
    }

    fn accept(&mut self, frame: &piper_can::PiperFrame) -> bool {
        if !self.accepting {
            return false;
        }

        self.accepted_count = self.accepted_count.saturating_add(1);

        let reached_deadline =
            self.deadline_us.is_some_and(|deadline_us| frame.timestamp_us() >= deadline_us);
        let reached_frame_count =
            self.frame_count_limit.is_some_and(|limit| self.accepted_count >= limit);
        let reached_can_id = self.stop_on_id.is_some_and(|id| frame.id() == id);

        if reached_deadline || reached_frame_count || reached_can_id {
            self.accepting = false;
        }

        true
    }
}

pub(super) struct ClientRecordingHook {
    tx: crossbeam_channel::Sender<TimestampedFrame>,
    dropped_frames: Arc<AtomicU64>,
    frame_counter: Arc<AtomicU64>,
    stop_requested: Arc<AtomicBool>,
    gate: Arc<Mutex<RecordingGate>>,
    session_start: Instant,
    hardware_origin: OnceLock<(u64, u64)>,
    kernel_origin: OnceLock<(u64, u64)>,
}

impl ClientRecordingHook {
    pub(super) fn new(
        condition: RecordingStopCondition,
    ) -> (Self, crossbeam_channel::Receiver<TimestampedFrame>) {
        let (tx, rx) = crossbeam_channel::bounded(100_000);
        let hook = Self {
            tx,
            dropped_frames: Arc::new(AtomicU64::new(0)),
            frame_counter: Arc::new(AtomicU64::new(0)),
            stop_requested: Arc::new(AtomicBool::new(false)),
            gate: Arc::new(Mutex::new(RecordingGate::new(condition))),
            session_start: Instant::now(),
            hardware_origin: OnceLock::new(),
            kernel_origin: OnceLock::new(),
        };

        (hook, rx)
    }

    pub(super) fn dropped_frames(&self) -> &Arc<AtomicU64> {
        &self.dropped_frames
    }

    pub(super) fn frame_counter(&self) -> &Arc<AtomicU64> {
        &self.frame_counter
    }

    pub(super) fn stop_requested(&self) -> &Arc<AtomicBool> {
        &self.stop_requested
    }

    pub(super) fn gate(&self) -> &Arc<Mutex<RecordingGate>> {
        &self.gate
    }

    #[cfg(test)]
    pub(super) fn request_stop(&self) {
        self.stop_requested.store(true, Ordering::Release);
        match self.gate.lock() {
            Ok(mut gate) => gate.close(),
            Err(poisoned) => poisoned.into_inner().close(),
        }
    }

    #[cfg(test)]
    pub(super) fn is_stop_requested(&self) -> bool {
        self.stop_requested.load(Ordering::Acquire)
    }

    fn elapsed_us_since_start(&self) -> u64 {
        self.session_start.elapsed().as_micros().min(u128::from(u64::MAX)) as u64
    }

    fn normalize_source_timestamp(
        &self,
        raw_timestamp_us: u64,
        provenance: TimestampProvenance,
    ) -> Option<u64> {
        if raw_timestamp_us == 0 {
            return None;
        }

        let origin = match provenance {
            TimestampProvenance::Hardware => &self.hardware_origin,
            TimestampProvenance::Kernel => &self.kernel_origin,
            _ => return None,
        };

        let elapsed_now = self.elapsed_us_since_start().max(1);
        let &(raw_origin, elapsed_origin) = origin.get_or_init(|| (raw_timestamp_us, elapsed_now));

        raw_timestamp_us
            .checked_sub(raw_origin)
            .map(|delta| elapsed_origin.saturating_add(delta))
    }

    fn normalize_event(&self, event: RecordedFrameEvent) -> RecordedFrameEvent {
        let raw_timestamp_us = event.frame.timestamp_us();
        let mapped_source_timestamp =
            self.normalize_source_timestamp(raw_timestamp_us, event.timestamp_provenance);

        match (event.timestamp_provenance, mapped_source_timestamp) {
            (TimestampProvenance::Hardware | TimestampProvenance::Kernel, Some(timestamp_us)) => {
                RecordedFrameEvent {
                    frame: event.frame.with_timestamp_us(timestamp_us),
                    ..event
                }
            },
            _ => RecordedFrameEvent {
                frame: event.frame.with_timestamp_us(self.elapsed_us_since_start().max(1)),
                timestamp_provenance: TimestampProvenance::Userspace,
                ..event
            },
        }
    }
}

impl FrameCallback for ClientRecordingHook {
    fn on_frame(&self, event: RecordedFrameEvent) {
        if self.stop_requested.load(Ordering::Acquire) {
            return;
        }

        let event = self.normalize_event(event);
        let accepted = match self.gate.lock() {
            Ok(mut gate) => gate.accept(&event.frame),
            Err(poisoned) => poisoned.into_inner().accept(&event.frame),
        };

        if !accepted {
            self.stop_requested.store(true, Ordering::Release);
            return;
        }

        let frame = TimestampedFrame::from(event);
        if self.tx.try_send(frame).is_err() {
            self.dropped_frames.fetch_add(1, Ordering::Relaxed);
        } else {
            self.frame_counter.fetch_add(1, Ordering::Relaxed);
        }

        let accepting = match self.gate.lock() {
            Ok(gate) => gate.accepting,
            Err(poisoned) => poisoned.into_inner().accepting,
        };
        if !accepting {
            self.stop_requested.store(true, Ordering::Release);
        }
    }
}

pub(super) fn map_source(source: TimestampProvenance) -> Option<piper_tools::TimestampSource> {
    match source {
        TimestampProvenance::Hardware => Some(piper_tools::TimestampSource::Hardware),
        TimestampProvenance::Kernel => Some(piper_tools::TimestampSource::Kernel),
        TimestampProvenance::Userspace => Some(piper_tools::TimestampSource::Userspace),
        TimestampProvenance::None => None,
    }
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
            gate: parts.gate,
            output_path: parts.output_path,
            metadata: parts.metadata,
            start_time_unix_secs: parts.start_time_unix_secs,
            start_time: parts.start_time,
            hook_registration: Mutex::new(Some((parts.hook_manager, parts.hook_handle))),
        }
    }

    fn refresh_stop_condition(&self) {
        let _ = self.stop_requested.load(Ordering::Acquire);
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
        match self.gate.lock() {
            Ok(mut gate) => gate.close(),
            Err(poisoned) => poisoned.into_inner().close(),
        }
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
        match self.gate.lock() {
            Ok(mut gate) => gate.close(),
            Err(poisoned) => poisoned.into_inner().close(),
        }
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
    OnCanId(CanId),

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
        let condition = StopCondition::OnCanId(CanId::standard(0x1A1).unwrap());
        match condition {
            StopCondition::OnCanId(id) => assert_eq!(id, CanId::standard(0x1A1).unwrap()),
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
