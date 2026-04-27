//! 异步录制钩子（Async Recording Hook）
//!
//! 本模块提供基于 Channel 的异步录制钩子，用于高性能 CAN 帧录制。
//!
//! # 设计原则
//!
//! - **Bounded Queue**: 使用 `bounded(100_000)` 防止 OOM
//! - **非阻塞**: 使用 `try_send`，队列满时丢帧而非阻塞
//! - **丢帧监控**: 提供 `dropped_frames` 计数器
//! - **时间戳精度**: 保留来源元数据，并在录制边界归一化为会话内 elapsed timestamp
//!
//! # 性能分析
//!
//! - 队列容量: 100,000 帧（约 1.6 分钟 @ 1kHz，约 3.3 分钟 @ 500Hz）
//! - 回调开销: <1μs (0.1%)
//! - 内存占用: 每帧约 32 bytes → 队列总约 3.2 MB
//!
//! # 使用示例
//!
//! ```rust
//! use piper_driver::recording::AsyncRecordingHook;
//! use piper_driver::hooks::FrameCallback;
//! use std::sync::Arc;
//!
//! // 创建录制钩子
//! let (hook, rx) = AsyncRecordingHook::new();
//! let dropped_counter = hook.dropped_frames().clone();  // 📊 直接持有引用
//!
//! // 注册为回调
//! let callback = Arc::new(hook) as Arc<dyn FrameCallback>;
//!
//! // 在后台线程处理录制数据
//! std::thread::spawn(move || {
//!     while let Ok(recorded) = rx.recv() {
//!         // recorded.timestamp_us() 是会话内 elapsed 微秒
//!         let _elapsed_us = recorded.timestamp_us();
//!     }
//! });
//!
//! // 监控丢帧
//! println!("丢了 {} 帧", dropped_counter.load(std::sync::atomic::Ordering::Relaxed));
//! ```

use crate::hooks::FrameCallback;
use crossbeam_channel::{Receiver, Sender, bounded};
pub use piper_can::TimestampProvenance;
use piper_protocol::{CanId, PiperFrame};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

/// 录制帧方向。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordedFrameDirection {
    Rx,
    Tx,
}

/// 带时间戳的帧。
#[derive(Debug, Clone)]
pub struct TimestampedFrame {
    /// CAN 帧，`timestamp_us()` 已归一化为录制会话开始后的微秒数。
    pub frame: PiperFrame,
    /// RX/TX 方向。
    pub direction: RecordedFrameDirection,
    /// 归一化后时间戳来源。
    pub timestamp_provenance: TimestampProvenance,
}

impl TimestampedFrame {
    /// Returns the typed CAN ID for the recorded frame.
    pub fn id(&self) -> CanId {
        self.frame.id()
    }

    /// Returns the raw CAN ID value without losing standard/extended typing on the frame.
    pub fn raw_id(&self) -> u32 {
        self.frame.raw_id()
    }

    /// Returns the recorded frame payload without canonical padding bytes.
    pub fn data(&self) -> &[u8] {
        self.frame.data()
    }

    /// Returns the fixed 8-byte padded CAN payload.
    pub fn data_padded(&self) -> &[u8; 8] {
        self.frame.data_padded()
    }

    /// Returns the CAN data length code.
    pub fn dlc(&self) -> u8 {
        self.frame.dlc()
    }

    /// Returns the normalized recording timestamp in microseconds.
    pub fn timestamp_us(&self) -> u64 {
        self.frame.timestamp_us()
    }

    /// Returns whether the frame was received or transmitted.
    pub fn direction(&self) -> RecordedFrameDirection {
        self.direction
    }

    /// Returns the provenance for the normalized timestamp.
    pub fn timestamp_provenance(&self) -> TimestampProvenance {
        self.timestamp_provenance
    }
}

#[derive(Debug, Clone, Copy)]
pub struct RecordedFrameEvent {
    pub frame: PiperFrame,
    pub direction: RecordedFrameDirection,
    pub timestamp_provenance: TimestampProvenance,
}

impl From<RecordedFrameEvent> for TimestampedFrame {
    fn from(event: RecordedFrameEvent) -> Self {
        Self {
            frame: event.frame,
            direction: event.direction,
            timestamp_provenance: event.timestamp_provenance,
        }
    }
}

/// 异步录制钩子（Actor 模式 + Bounded Queue）
///
/// # 内存安全
///
/// 使用 **有界通道**（Bounded Channel）防止 OOM：
/// - 容量: 100,000 帧（约 3.3 分钟 @ 500Hz）
/// - 队列满时丢帧，而不是无限增长导致 OOM
/// - 可通过 `dropped_frames` 和 `frame_counter` 计数器监控
///
/// # 设计理由
///
/// - `unbounded()` 在慢消费场景下可能导致 OOM
/// - `bounded(100_000)` 为短时磁盘抖动和后台分析线程提供足够缓冲
/// - 队列满时通过丢帧计数器显式暴露背压，而不是把压力转成无限内存增长
///
/// # 示例
///
/// ```rust
/// use piper_driver::recording::AsyncRecordingHook;
/// use piper_driver::hooks::FrameCallback;
/// use std::sync::Arc;
///
/// // 创建录制钩子
/// let (hook, rx) = AsyncRecordingHook::new();
///
/// // 直接持有计数器的 Arc 引用
/// let dropped_counter = hook.dropped_frames().clone();
/// let frame_counter = hook.frame_counter().clone();
///
/// // 注册为回调
/// let callback = Arc::new(hook) as Arc<dyn FrameCallback>;
///
/// // 监控丢帧和帧数
/// let dropped = dropped_counter.load(std::sync::atomic::Ordering::Relaxed);
/// let frames = frame_counter.load(std::sync::atomic::Ordering::Relaxed);
/// println!("已录制 {} 帧，丢了 {} 帧", frames, dropped);
/// ```
pub struct AsyncRecordingHook {
    /// 发送端（用于 Channel）
    tx: Sender<TimestampedFrame>,

    /// 丢帧计数器（用于监控）
    dropped_frames: Arc<AtomicU64>,

    /// 帧计数器（每次成功发送时递增）
    frame_counter: Arc<AtomicU64>,

    /// 停止条件：当收到此 typed CAN ID 时停止录制（None 表示不启用）
    stop_on_id: Option<CanId>,

    /// 停止条件：达到指定录制时长后自动停止。
    stop_deadline: Option<Instant>,

    /// 停止条件：成功录制指定数量的帧后自动停止。
    stop_after_frame_count: Option<u64>,

    /// 停止请求标志（原子操作，用于跨线程通信）
    stop_requested: Arc<AtomicBool>,

    /// 录制会话开始时间，用于将 TX/Userspace 时间戳归一化为 elapsed 微秒。
    session_start: Instant,

    /// Raw source timestamp and session elapsed timestamp observed for the first mappable frame.
    hardware_origin: OnceLock<(u64, u64)>,
    /// Raw source timestamp and session elapsed timestamp observed for the first mappable frame.
    kernel_origin: OnceLock<(u64, u64)>,
}

impl AsyncRecordingHook {
    /// 创建新的录制钩子
    ///
    /// # 队列容量
    ///
    /// - 容量: 100,000 帧（约 3.3 分钟 @ 500Hz）
    /// - 500Hz CAN 总线: 约 3.3 分钟缓存
    /// - 1kHz CAN 总线: 约 1.6 分钟缓存
    /// - 内存占用: 约 2.4MB（100k × 24 bytes/frame）
    ///
    /// **设计理由**:
    /// - 足够吸收短暂的磁盘 I/O 延迟，同时防止 OOM
    /// - 支持中等时长的录制（3 分钟左右）
    /// - 超过此时长会导致丢帧（Channel 满）
    ///
    /// # 返回
    ///
    /// - `(hook, rx)`: 钩子实例和接收端
    ///
    /// # 示例
    ///
    /// ```rust
    /// use piper_driver::recording::AsyncRecordingHook;
    ///
    /// let (hook, rx) = AsyncRecordingHook::new();
    /// ```
    #[must_use]
    pub fn new() -> (Self, Receiver<TimestampedFrame>) {
        Self::with_auto_stop(None, None, None)
    }

    /// 创建新的录制钩子（带自动停止条件）
    #[must_use]
    pub fn with_auto_stop(
        stop_on_id: Option<CanId>,
        stop_duration: Option<Duration>,
        stop_after_frame_count: Option<u64>,
    ) -> (Self, Receiver<TimestampedFrame>) {
        // ⚠️ 缓冲区大小：100,000 帧（约 3-4 分钟 @ 500Hz）
        // 内存占用：约 2.4MB（100k × 24 bytes/frame）
        // 风险提示：超过此时长会导致丢帧
        let (tx, rx) = bounded(100_000);

        let hook = Self {
            tx,
            dropped_frames: Arc::new(AtomicU64::new(0)),
            frame_counter: Arc::new(AtomicU64::new(0)),
            stop_on_id,
            stop_deadline: stop_duration.map(|duration| Instant::now() + duration),
            stop_after_frame_count,
            stop_requested: Arc::new(AtomicBool::new(false)),
            session_start: Instant::now(),
            hardware_origin: OnceLock::new(),
            kernel_origin: OnceLock::new(),
        };

        (hook, rx)
    }

    /// 创建新的录制钩子（带停止条件）
    ///
    /// # 参数
    ///
    /// - `stop_on_id`: 当收到此 CAN ID 时停止录制（None 表示不启用）
    ///
    /// # 返回
    ///
    /// - `(hook, rx)`: 钩子实例和接收端
    ///
    /// # 示例
    ///
    /// ```rust
    /// use piper_driver::recording::AsyncRecordingHook;
    /// use piper_protocol::CanId;
    ///
    /// // 当收到 0x2A4 时停止录制（末端位姿帧）
    /// let stop_id = CanId::standard(0x2A4).unwrap();
    /// let (hook, rx) = AsyncRecordingHook::with_stop_condition(Some(stop_id));
    /// ```
    #[must_use]
    pub fn with_stop_condition(stop_on_id: Option<CanId>) -> (Self, Receiver<TimestampedFrame>) {
        Self::with_auto_stop(stop_on_id, None, None)
    }

    /// 获取停止请求标志（新增：v1.4）
    ///
    /// 用于检查是否应该停止录制
    pub fn is_stop_requested(&self) -> bool {
        self.stop_requested.load(Ordering::Relaxed)
    }

    /// 获取停止请求标志的 Arc 引用（新增：v1.4）
    ///
    /// 用于跨线程共享停止标志
    pub fn stop_requested(&self) -> &Arc<AtomicBool> {
        &self.stop_requested
    }

    /// 获取发送端（用于自定义场景）
    ///
    /// # 注意
    ///
    /// 大多数情况下不需要直接使用此方法，只需将 `AsyncRecordingHook` 注册为 `FrameCallback` 即可。
    #[must_use]
    pub fn sender(&self) -> Sender<TimestampedFrame> {
        self.tx.clone()
    }

    /// 获取丢帧计数器
    ///
    /// # 使用建议（v1.2.1）
    ///
    /// ✅ **推荐**: 在创建钩子时直接持有 `Arc` 引用
    ///
    /// ```rust
    /// use piper_driver::recording::AsyncRecordingHook;
    /// use std::sync::atomic::Ordering;
    ///
    /// let (hook, _rx) = AsyncRecordingHook::new();
    /// let dropped_counter = hook.dropped_frames().clone();  // 在此持有
    ///
    /// // 直接读取，无需从 Context downcast
    /// let count = dropped_counter.load(Ordering::Relaxed);
    /// ```
    ///
    /// ❌ **不推荐**: 试图从 `Context` 中 `downcast`（需要 Trait 继承 `Any`）
    ///
    /// # 返回
    ///
    /// `Arc<AtomicU64>`: 丢帧计数器的引用
    #[must_use]
    pub fn dropped_frames(&self) -> &Arc<AtomicU64> {
        &self.dropped_frames
    }

    /// 获取当前丢帧数量
    ///
    /// # 返回
    ///
    /// 当前丢失的帧数
    #[must_use]
    pub fn dropped_count(&self) -> u64 {
        self.dropped_frames.load(Ordering::Relaxed)
    }

    /// 获取帧计数器（新增：v1.3.0）
    ///
    /// # 使用建议
    ///
    /// ✅ **推荐**: 在创建钩子时直接持有 `Arc` 引用
    ///
    /// ```rust
    /// use piper_driver::recording::AsyncRecordingHook;
    /// use std::sync::atomic::Ordering;
    ///
    /// let (hook, _rx) = AsyncRecordingHook::new();
    /// let frame_counter = hook.frame_counter().clone();  // 在此持有
    ///
    /// // 直接读取，无需从 Context downcast
    /// let count = frame_counter.load(Ordering::Relaxed);
    /// ```
    ///
    /// # 返回
    ///
    /// `Arc<AtomicU64>`: 帧计数器的引用（不可变，只读）
    #[must_use]
    pub fn frame_counter(&self) -> &Arc<AtomicU64> {
        &self.frame_counter
    }

    /// 获取当前已录制的帧数（新增：v1.3.0）
    ///
    /// # 返回
    ///
    /// 当前已成功录制的帧数
    #[must_use]
    pub fn frame_count(&self) -> u64 {
        self.frame_counter.load(Ordering::Relaxed)
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

        if raw_timestamp_us < raw_origin {
            None
        } else {
            Some(elapsed_origin.saturating_add(raw_timestamp_us - raw_origin))
        }
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

    fn try_record_event(&self, event: RecordedFrameEvent) {
        let ts_frame = TimestampedFrame::from(self.normalize_event(event));

        if self.tx.try_send(ts_frame).is_err() {
            self.dropped_frames.fetch_add(1, Ordering::Relaxed);
        } else {
            let new_count = self.frame_counter.fetch_add(1, Ordering::Relaxed) + 1;
            if let Some(limit) = self.stop_after_frame_count
                && new_count >= limit
            {
                self.stop_requested.store(true, Ordering::Release);
            }
        }
    }
}

impl FrameCallback for AsyncRecordingHook {
    /// 当接收到 CAN 帧时调用
    ///
    /// # 性能要求
    ///
    /// - <1μs 开销（非阻塞）
    /// - 队列满时丢帧，而非阻塞或无限增长
    ///
    /// # 时间戳精度
    ///
    /// 回调事件携带帧方向和来源，录制钩子会把源时间戳归一化为会话内 elapsed 微秒。
    ///
    /// ```rust
    /// use piper_driver::hooks::FrameCallback;
    /// use piper_driver::recording::{AsyncRecordingHook, RecordedFrameDirection, RecordedFrameEvent};
    /// use piper_can::TimestampProvenance;
    /// use piper_protocol::PiperFrame;
    /// use std::sync::Arc;
    ///
    /// let (hook, rx) = AsyncRecordingHook::new();
    /// let callback = Arc::new(hook) as Arc<dyn FrameCallback>;
    /// let frame = PiperFrame::new_standard(0x251, [1, 2, 3, 4])
    ///     .unwrap()
    ///     .with_timestamp_us(99_000);
    ///
    /// callback.on_frame(RecordedFrameEvent {
    ///     frame,
    ///     direction: RecordedFrameDirection::Rx,
    ///     timestamp_provenance: TimestampProvenance::Kernel,
    /// });
    ///
    /// let recorded = rx.recv().unwrap();
    /// assert!(recorded.timestamp_us() < 99_000);
    /// ```
    #[inline]
    fn on_frame(&self, event: RecordedFrameEvent) {
        if self.stop_requested.load(Ordering::Acquire) {
            return;
        }
        if let Some(deadline) = self.stop_deadline
            && Instant::now() >= deadline
        {
            self.stop_requested.store(true, Ordering::Release);
            return;
        }

        // ⚠️ 关键：这里运行在 CAN 接收线程中，必须极快
        // ✅ 性能优化：先记录所有帧（包括触发帧），再检查停止条件（v1.4 修正）

        // 1. 先记录帧（无论是否为触发帧）
        self.try_record_event(event);

        // 2. 再检查停止条件（原子操作，极快）
        if event.direction == RecordedFrameDirection::Rx
            && let Some(stop_id) = self.stop_on_id
            && event.frame.id() == stop_id
        {
            // ✅ 原子存储，不会阻塞
            self.stop_requested.store(true, Ordering::SeqCst);
            // ✅ 注意：不使用 return，因为已经记录了触发帧
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hooks::HookManager;
    use piper_can::{ReceivedFrame, TimestampProvenance};
    use std::thread;
    use std::time::Duration;

    fn frame_with_timestamp(id: u32, data: &[u8], timestamp_us: u64) -> PiperFrame {
        PiperFrame::new_standard(id, data).unwrap().with_timestamp_us(timestamp_us)
    }

    fn extended_frame_with_timestamp(id: u32, data: &[u8], timestamp_us: u64) -> PiperFrame {
        PiperFrame::new_extended(id, data).unwrap().with_timestamp_us(timestamp_us)
    }

    fn event(
        frame: PiperFrame,
        direction: RecordedFrameDirection,
        timestamp_provenance: TimestampProvenance,
    ) -> RecordedFrameEvent {
        RecordedFrameEvent {
            frame,
            direction,
            timestamp_provenance,
        }
    }

    #[test]
    fn timestamped_frame_stores_piper_frame_directly() {
        let frame = frame_with_timestamp(0x2A5, &[0, 1, 2, 3, 4, 5, 6, 7], 12345);

        let timestamped = TimestampedFrame {
            frame,
            direction: RecordedFrameDirection::Rx,
            timestamp_provenance: TimestampProvenance::Hardware,
        };

        assert_eq!(timestamped.frame, frame);
        assert_eq!(timestamped.frame.raw_id(), 0x2A5);
        assert_eq!(timestamped.frame.data(), &[0, 1, 2, 3, 4, 5, 6, 7]);
        assert_eq!(timestamped.direction, RecordedFrameDirection::Rx);
        assert_eq!(
            timestamped.timestamp_provenance,
            TimestampProvenance::Hardware
        );
    }

    #[test]
    fn timestamped_frame_exposes_read_only_accessors() {
        let frame = frame_with_timestamp(0x2A5, &[0, 1, 2, 3], 12345);

        let timestamped = TimestampedFrame {
            frame,
            direction: RecordedFrameDirection::Rx,
            timestamp_provenance: TimestampProvenance::Hardware,
        };

        assert_eq!(timestamped.id(), frame.id());
        assert_eq!(timestamped.raw_id(), 0x2A5);
        assert_eq!(timestamped.dlc(), 4);
        assert_eq!(timestamped.data(), &[0, 1, 2, 3]);
        assert_eq!(timestamped.data_padded(), frame.data_padded());
        assert_eq!(timestamped.timestamp_us(), 12345);
        assert_eq!(timestamped.direction(), RecordedFrameDirection::Rx);
        assert_eq!(
            timestamped.timestamp_provenance(),
            TimestampProvenance::Hardware
        );
    }

    #[test]
    fn rx_recording_preserves_received_frame_provenance() {
        let (hook, rx) = AsyncRecordingHook::new();
        let mut hooks = HookManager::new();
        hooks.add_callback(Arc::new(hook));

        let received_frame = ReceivedFrame::new(
            frame_with_timestamp(0x2A5, &[1, 2, 3, 4], 99_000),
            TimestampProvenance::Kernel,
        );
        hooks.trigger_all(received_frame);

        let recorded = rx.recv_timeout(Duration::from_millis(100)).unwrap();
        assert_eq!(recorded.raw_id(), 0x2A5);
        assert_eq!(recorded.direction, RecordedFrameDirection::Rx);
        assert_eq!(recorded.timestamp_provenance, TimestampProvenance::Kernel);
    }

    #[test]
    fn tx_recording_event_uses_tx_direction() {
        let (hook, rx) = AsyncRecordingHook::new();
        let callback = Arc::new(hook) as Arc<dyn FrameCallback>;

        callback.on_frame(RecordedFrameEvent {
            frame: frame_with_timestamp(0x1A1, &[1, 2, 3, 4], 0),
            direction: RecordedFrameDirection::Tx,
            timestamp_provenance: TimestampProvenance::Userspace,
        });

        let recorded = rx.recv_timeout(Duration::from_millis(100)).unwrap();
        assert_eq!(recorded.direction, RecordedFrameDirection::Tx);
    }

    #[test]
    fn tx_recording_stamps_replayable_userspace_copy_after_send() {
        let (hook, rx) = AsyncRecordingHook::new();
        std::thread::sleep(Duration::from_millis(1));

        let mut hooks = HookManager::new();
        hooks.add_callback(Arc::new(hook));
        let backend_observed = frame_with_timestamp(0x1A1, &[1, 2, 3, 4], 0);

        hooks.trigger_all_sent(&backend_observed);

        let recorded = rx.recv_timeout(Duration::from_millis(100)).unwrap();
        assert_eq!(recorded.direction, RecordedFrameDirection::Tx);
        assert_eq!(
            recorded.timestamp_provenance,
            TimestampProvenance::Userspace
        );
        assert!(
            recorded.timestamp_us() > 0,
            "recording copy must be replayable with elapsed userspace timestamp"
        );
        assert_eq!(
            backend_observed.timestamp_us(),
            0,
            "backend-observed frame must remain unstamped"
        );
    }

    fn assert_source_timestamps_are_normalized(timestamp_provenance: TimestampProvenance) {
        let (hook, rx) = AsyncRecordingHook::new();
        let callback = Arc::new(hook) as Arc<dyn FrameCallback>;

        callback.on_frame(event(
            frame_with_timestamp(0x2A5, &[1, 2, 3, 4], 99_000),
            RecordedFrameDirection::Rx,
            timestamp_provenance,
        ));
        callback.on_frame(event(
            frame_with_timestamp(0x2A5, &[1, 2, 3, 4], 100_250),
            RecordedFrameDirection::Rx,
            timestamp_provenance,
        ));

        let first = rx.recv_timeout(Duration::from_millis(100)).unwrap();
        let second = rx.recv_timeout(Duration::from_millis(100)).unwrap();

        assert_eq!(first.timestamp_provenance, timestamp_provenance);
        assert_eq!(second.timestamp_provenance, timestamp_provenance);
        assert_ne!(
            first.frame.timestamp_us(),
            99_000,
            "recording timestamp must be session-relative, not raw absolute source time"
        );
        assert_eq!(
            second.frame.timestamp_us() - first.frame.timestamp_us(),
            1_250
        );
    }

    #[test]
    fn kernel_timestamps_are_normalized_to_recording_elapsed_time() {
        assert_source_timestamps_are_normalized(TimestampProvenance::Kernel);
    }

    #[test]
    fn hardware_timestamps_are_normalized_to_recording_elapsed_time() {
        assert_source_timestamps_are_normalized(TimestampProvenance::Hardware);
    }

    #[test]
    fn hardware_timestamp_without_source_time_falls_back_to_userspace() {
        let (hook, rx) = AsyncRecordingHook::new();
        let callback = Arc::new(hook) as Arc<dyn FrameCallback>;

        callback.on_frame(event(
            frame_with_timestamp(0x2A5, &[1, 2, 3, 4], 0),
            RecordedFrameDirection::Rx,
            TimestampProvenance::Hardware,
        ));

        let recorded = rx.recv_timeout(Duration::from_millis(100)).unwrap();
        assert_eq!(
            recorded.timestamp_provenance,
            TimestampProvenance::Userspace
        );
        assert!(recorded.timestamp_us() > 0);
    }

    #[test]
    fn test_async_recording_hook_basic() {
        let (hook, rx) = AsyncRecordingHook::new();
        let callback = Arc::new(hook) as Arc<dyn FrameCallback>;

        let frame = frame_with_timestamp(0x2A5, &[0, 1, 2, 3, 4, 5, 6, 7], 12345);

        // 触发回调
        callback.on_frame(event(
            frame,
            RecordedFrameDirection::Rx,
            TimestampProvenance::Hardware,
        ));

        // 验证接收到帧
        let received = rx.recv_timeout(Duration::from_millis(100)).unwrap();
        assert_eq!(received.frame.raw_id(), 0x2A5);
        assert_eq!(received.frame.data(), &[0, 1, 2, 3, 4, 5, 6, 7]);
        assert_eq!(received.direction, RecordedFrameDirection::Rx);
        assert_eq!(received.timestamp_provenance, TimestampProvenance::Hardware);
    }

    #[test]
    fn test_async_recording_hook_dropped_frames() {
        let (hook, rx) = AsyncRecordingHook::new();
        let dropped_counter = hook.dropped_frames().clone();
        let callback = Arc::new(hook) as Arc<dyn FrameCallback>;

        let frame = frame_with_timestamp(0x2A5, &[0, 1, 2, 3, 4, 5, 6, 7], 12345);

        // 正常情况：无丢帧
        callback.on_frame(event(
            frame,
            RecordedFrameDirection::Rx,
            TimestampProvenance::Hardware,
        ));
        assert_eq!(dropped_counter.load(Ordering::Relaxed), 0);

        // 清空接收端，模拟队列满的情况
        drop(rx);

        // 现在发送会失败（队列已关闭）
        for _ in 0..10 {
            callback.on_frame(event(
                frame,
                RecordedFrameDirection::Rx,
                TimestampProvenance::Hardware,
            ));
        }

        // 应该记录了 10 个丢帧
        assert_eq!(dropped_counter.load(Ordering::Relaxed), 10);
    }

    #[test]
    fn test_async_recording_hook_tx_callback() {
        let (hook, rx) = AsyncRecordingHook::new();
        let callback = Arc::new(hook) as Arc<dyn FrameCallback>;

        let frame = frame_with_timestamp(0x1A1, &[1, 2, 3, 4, 5, 6, 7, 8], 0);

        // 触发 TX 回调
        callback.on_frame(event(
            frame,
            RecordedFrameDirection::Tx,
            TimestampProvenance::Userspace,
        ));

        // 验证接收到帧
        let received = rx.recv_timeout(Duration::from_millis(100)).unwrap();
        assert_eq!(received.frame.raw_id(), 0x1A1);
        assert_eq!(received.direction, RecordedFrameDirection::Tx);
        assert_eq!(
            received.timestamp_provenance,
            TimestampProvenance::Userspace
        );
    }

    #[test]
    fn test_async_recording_hook_frame_count_auto_stop_stops_after_limit() {
        let (hook, rx) = AsyncRecordingHook::with_auto_stop(None, None, Some(1));
        let callback = Arc::new(hook) as Arc<dyn FrameCallback>;

        let frame = frame_with_timestamp(0x2A5, &[0; 8], 100);

        callback.on_frame(event(
            frame,
            RecordedFrameDirection::Rx,
            TimestampProvenance::Hardware,
        ));
        callback.on_frame(event(
            frame.with_timestamp_us(200),
            RecordedFrameDirection::Rx,
            TimestampProvenance::Hardware,
        ));

        let first = rx.recv_timeout(Duration::from_millis(100)).unwrap();
        assert_eq!(first.frame.raw_id(), 0x2A5);
        assert!(
            rx.try_recv().is_err(),
            "second frame must be ignored after auto-stop"
        );
    }

    #[test]
    fn test_async_recording_hook_duration_auto_stop_stops_new_frames_after_deadline() {
        let (hook, rx) = AsyncRecordingHook::with_auto_stop(None, Some(Duration::ZERO), None);
        let callback = Arc::new(hook) as Arc<dyn FrameCallback>;

        let frame = frame_with_timestamp(0x2A5, &[0; 8], 100);

        callback.on_frame(event(
            frame,
            RecordedFrameDirection::Rx,
            TimestampProvenance::Hardware,
        ));
        assert!(
            rx.try_recv().is_err(),
            "expired duration stop must reject new frames"
        );
    }

    #[test]
    fn standard_stop_condition_does_not_match_extended_frame_with_same_raw_id() {
        let stop_id = CanId::standard(0x123).unwrap();
        let (hook, rx) = AsyncRecordingHook::with_stop_condition(Some(stop_id));
        let stop_requested = hook.stop_requested().clone();
        let callback = Arc::new(hook) as Arc<dyn FrameCallback>;

        callback.on_frame(event(
            extended_frame_with_timestamp(0x123, &[1], 100),
            RecordedFrameDirection::Rx,
            TimestampProvenance::Hardware,
        ));
        callback.on_frame(event(
            frame_with_timestamp(0x124, &[2], 200),
            RecordedFrameDirection::Rx,
            TimestampProvenance::Hardware,
        ));

        let first = rx.recv_timeout(Duration::from_millis(100)).unwrap();
        let second = rx.recv_timeout(Duration::from_millis(100)).unwrap();

        assert_eq!(first.id(), CanId::extended(0x123).unwrap());
        assert_eq!(second.id(), CanId::standard(0x124).unwrap());
        assert!(
            !stop_requested.load(Ordering::Acquire),
            "extended frame with the same raw value must not satisfy a standard stop condition"
        );
    }

    #[test]
    fn matching_can_id_stop_condition_records_triggering_frame_before_stopping() {
        let stop_id = CanId::extended(0x123).unwrap();
        let (hook, rx) = AsyncRecordingHook::with_stop_condition(Some(stop_id));
        let callback = Arc::new(hook) as Arc<dyn FrameCallback>;

        callback.on_frame(event(
            extended_frame_with_timestamp(0x123, &[1], 100),
            RecordedFrameDirection::Rx,
            TimestampProvenance::Hardware,
        ));
        callback.on_frame(event(
            frame_with_timestamp(0x124, &[2], 200),
            RecordedFrameDirection::Rx,
            TimestampProvenance::Hardware,
        ));

        let trigger = rx.recv_timeout(Duration::from_millis(100)).unwrap();
        assert_eq!(trigger.id(), stop_id);
        assert_eq!(trigger.data(), &[1]);
        assert!(
            rx.try_recv().is_err(),
            "frames after the matching stop condition must be ignored"
        );
    }

    #[test]
    fn test_timestamped_frame_from_recorded_event() {
        let frame = frame_with_timestamp(0x2A5, &[0, 1, 2, 3, 4, 5, 6, 7], 99999);

        let ts_frame = TimestampedFrame::from(event(
            frame,
            RecordedFrameDirection::Rx,
            TimestampProvenance::Hardware,
        ));

        assert_eq!(ts_frame.timestamp_us(), 99999);
        assert_eq!(ts_frame.raw_id(), 0x2A5);
        assert_eq!(ts_frame.data(), &[0, 1, 2, 3, 4, 5, 6, 7]);
        assert_eq!(ts_frame.direction, RecordedFrameDirection::Rx);
    }

    #[test]
    fn test_async_recording_hook_concurrent() {
        let (hook, rx) = AsyncRecordingHook::new();
        let callback = Arc::new(hook) as Arc<dyn FrameCallback>;

        // 创建多个线程并发触发回调
        let handles: Vec<_> = (0..10)
            .map(|i| {
                let cb = callback.clone();
                thread::spawn(move || {
                    let frame = frame_with_timestamp(0x2A5, &[i as u8; 8], i as u64 + 1);
                    cb.on_frame(event(
                        frame,
                        RecordedFrameDirection::Rx,
                        TimestampProvenance::Hardware,
                    ));
                })
            })
            .collect();

        // 等待所有线程完成
        for handle in handles {
            handle.join().unwrap();
        }

        // 验证接收到所有帧（顺序可能不同）
        let mut count = 0;
        while rx.try_recv().is_ok() {
            count += 1;
        }
        assert_eq!(count, 10);
    }
}
