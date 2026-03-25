//! 异步录制钩子（Async Recording Hook）
//!
//! 本模块提供基于 Channel 的异步录制钩子，用于高性能 CAN 帧录制。
//!
//! # 设计原则
//!
//! - **Bounded Queue**: 使用 `bounded(100_000)` 防止 OOM
//! - **非阻塞**: 使用 `try_send`，队列满时丢帧而非阻塞
//! - **丢帧监控**: 提供 `dropped_frames` 计数器
//! - **时间戳精度**: 直接使用 `frame.timestamp_us`（硬件时间戳）
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
//! use piper_protocol::PiperFrame;
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
//!     while let Ok(frame) = rx.recv() {
//!         // 处理帧...
//!     }
//! });
//!
//! // 监控丢帧
//! println!("丢了 {} 帧", dropped_counter.load(std::sync::atomic::Ordering::Relaxed));
//! ```

use crate::hooks::FrameCallback;
use crossbeam_channel::{Receiver, Sender, bounded};
use piper_protocol::PiperFrame;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// 带时间戳的帧
///
/// 保存 CAN 帧及其硬件时间戳，用于录制和回放。
#[derive(Debug, Clone)]
pub struct TimestampedFrame {
    /// 硬件时间戳（微秒）
    ///
    /// ⏱️ **时间戳精度**: 必须直接使用 `frame.timestamp_us`（硬件时间戳）
    /// 禁止在回调中调用 `SystemTime::now()`，因为回调执行时间已晚于帧到达时间。
    pub timestamp_us: u64,

    /// CAN ID
    pub id: u32,

    /// 帧数据（最多 8 bytes）
    pub data: Vec<u8>,
}

impl From<&PiperFrame> for TimestampedFrame {
    fn from(frame: &PiperFrame) -> Self {
        Self {
            // ⏱️ 直接透传硬件时间戳
            timestamp_us: frame.timestamp_us,
            id: frame.id,
            data: frame.data.to_vec(),
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

    /// 停止条件：当收到此 CAN ID 时停止录制（None 表示不启用）
    stop_on_id: Option<u32>,

    /// 停止条件：达到指定录制时长后自动停止。
    stop_deadline: Option<Instant>,

    /// 停止条件：成功录制指定数量的帧后自动停止。
    stop_after_frame_count: Option<u64>,

    /// 停止请求标志（原子操作，用于跨线程通信）
    stop_requested: Arc<AtomicBool>,
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
        stop_on_id: Option<u32>,
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
    ///
    /// // 当收到 0x2A4 时停止录制（末端位姿帧）
    /// let (hook, rx) = AsyncRecordingHook::with_stop_condition(Some(0x2A4));
    /// ```
    #[must_use]
    pub fn with_stop_condition(stop_on_id: Option<u32>) -> (Self, Receiver<TimestampedFrame>) {
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
}

impl FrameCallback for AsyncRecordingHook {
    /// 当接收到 CAN 帧时调用
    ///
    /// # 性能要求
    ///
    /// - <1μs 开销（非阻塞）
    /// - 队列满时丢帧，而非阻塞或无限增长
    ///
    /// # 时间戳精度（v1.2.1）
    ///
    /// ⏱️ **必须使用硬件时间戳**:
    ///
    /// ```rust
    /// use piper_driver::recording::TimestampedFrame;
    /// use piper_protocol::PiperFrame;
    ///
    /// let frame = PiperFrame::new_standard(0x251, &[1, 2, 3, 4]);
    /// let ts_frame = TimestampedFrame::from(&frame);
    /// assert_eq!(ts_frame.timestamp_us, frame.timestamp_us);  // ✅ 硬件时间戳
    /// ```
    ///
    /// ❌ **禁止软件生成时间戳**:
    ///
    /// // ❌ 错误：回调执行时间已晚于帧到达时间（仅说明概念）
    /// // let ts = SystemTime::now().duration_since(UNIX_EPOCH)?.as_micros() as u64;
    ///
    #[inline]
    #[allow(clippy::collapsible_if)] // 嵌套 if 结构更清晰：先检查 Option，再比较 ID
    fn on_frame_received(&self, frame: &PiperFrame) {
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
        let ts_frame = TimestampedFrame::from(frame);
        if self.tx.try_send(ts_frame).is_err() {
            // ⚠️ 缓冲区满时，丢弃"新"帧，保留"旧"帧
            self.dropped_frames.fetch_add(1, Ordering::Relaxed);
        } else {
            let new_count = self.frame_counter.fetch_add(1, Ordering::Relaxed) + 1;
            if let Some(limit) = self.stop_after_frame_count
                && new_count >= limit
            {
                self.stop_requested.store(true, Ordering::Release);
            }
        }

        // 2. 再检查停止条件（原子操作，极快）
        if let Some(stop_id) = self.stop_on_id {
            if frame.id() == stop_id {
                // ✅ 原子存储，不会阻塞
                self.stop_requested.store(true, Ordering::SeqCst);
                // ✅ 注意：不使用 return，因为已经记录了触发帧
            }
        }
    }

    /// 当发送 CAN 帧成功后调用（可选）
    ///
    /// # 时机
    ///
    /// 仅在 `tx.send()` 成功后调用，确保录制的是实际发送的帧。
    #[inline]
    fn on_frame_sent(&self, frame: &PiperFrame) {
        if self.stop_requested.load(Ordering::Acquire) {
            return;
        }
        if let Some(deadline) = self.stop_deadline
            && Instant::now() >= deadline
        {
            self.stop_requested.store(true, Ordering::Release);
            return;
        }

        // ⏱️ 直接透传硬件时间戳
        let ts_frame = TimestampedFrame::from(frame);

        // 🛡️ 非阻塞发送
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn test_async_recording_hook_basic() {
        let (hook, rx) = AsyncRecordingHook::new();
        let callback = Arc::new(hook) as Arc<dyn FrameCallback>;

        // 创建测试帧
        let frame = PiperFrame {
            id: 0x2A5,
            data: [0, 1, 2, 3, 4, 5, 6, 7],
            len: 8,
            is_extended: false,
            timestamp_us: 12345,
        };

        // 触发回调
        callback.on_frame_received(&frame);

        // 验证接收到帧
        let received = rx.recv_timeout(Duration::from_millis(100)).unwrap();
        assert_eq!(received.timestamp_us, 12345);
        assert_eq!(received.id, 0x2A5);
        assert_eq!(received.data, vec![0, 1, 2, 3, 4, 5, 6, 7]);
    }

    #[test]
    fn test_async_recording_hook_dropped_frames() {
        let (hook, rx) = AsyncRecordingHook::new();
        let dropped_counter = hook.dropped_frames().clone();
        let callback = Arc::new(hook) as Arc<dyn FrameCallback>;

        // 创建测试帧
        let frame = PiperFrame {
            id: 0x2A5,
            data: [0, 1, 2, 3, 4, 5, 6, 7],
            len: 8,
            is_extended: false,
            timestamp_us: 12345,
        };

        // 正常情况：无丢帧
        callback.on_frame_received(&frame);
        assert_eq!(dropped_counter.load(Ordering::Relaxed), 0);

        // 清空接收端，模拟队列满的情况
        drop(rx);

        // 现在发送会失败（队列已关闭）
        for _ in 0..10 {
            callback.on_frame_received(&frame);
        }

        // 应该记录了 10 个丢帧
        assert_eq!(dropped_counter.load(Ordering::Relaxed), 10);
    }

    #[test]
    fn test_async_recording_hook_tx_callback() {
        let (hook, rx) = AsyncRecordingHook::new();
        let callback = Arc::new(hook) as Arc<dyn FrameCallback>;

        // 创建测试帧
        let frame = PiperFrame {
            id: 0x1A1,
            data: [1, 2, 3, 4, 5, 6, 7, 8],
            len: 8,
            is_extended: false,
            timestamp_us: 54321,
        };

        // 触发 TX 回调
        callback.on_frame_sent(&frame);

        // 验证接收到帧
        let received = rx.recv_timeout(Duration::from_millis(100)).unwrap();
        assert_eq!(received.timestamp_us, 54321);
        assert_eq!(received.id, 0x1A1);
    }

    #[test]
    fn test_async_recording_hook_frame_count_auto_stop_stops_after_limit() {
        let (hook, rx) = AsyncRecordingHook::with_auto_stop(None, None, Some(1));
        let callback = Arc::new(hook) as Arc<dyn FrameCallback>;

        let frame = PiperFrame {
            id: 0x2A5,
            data: [0; 8],
            len: 8,
            is_extended: false,
            timestamp_us: 100,
        };

        callback.on_frame_received(&frame);
        callback.on_frame_received(&PiperFrame {
            timestamp_us: 200,
            ..frame
        });

        let first = rx.recv_timeout(Duration::from_millis(100)).unwrap();
        assert_eq!(first.timestamp_us, 100);
        assert!(
            rx.try_recv().is_err(),
            "second frame must be ignored after auto-stop"
        );
    }

    #[test]
    fn test_async_recording_hook_duration_auto_stop_stops_new_frames_after_deadline() {
        let (hook, rx) = AsyncRecordingHook::with_auto_stop(None, Some(Duration::ZERO), None);
        let callback = Arc::new(hook) as Arc<dyn FrameCallback>;

        let frame = PiperFrame {
            id: 0x2A5,
            data: [0; 8],
            len: 8,
            is_extended: false,
            timestamp_us: 100,
        };

        callback.on_frame_received(&frame);
        assert!(
            rx.try_recv().is_err(),
            "expired duration stop must reject new frames"
        );
    }

    #[test]
    fn test_timestamped_frame_from_piper_frame() {
        let frame = PiperFrame {
            id: 0x2A5,
            data: [0, 1, 2, 3, 4, 5, 6, 7],
            len: 8,
            is_extended: false,
            timestamp_us: 99999,
        };

        let ts_frame = TimestampedFrame::from(&frame);

        assert_eq!(ts_frame.timestamp_us, 99999);
        assert_eq!(ts_frame.id, 0x2A5);
        assert_eq!(ts_frame.data, vec![0, 1, 2, 3, 4, 5, 6, 7]);
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
                    let frame = PiperFrame {
                        id: 0x2A5,
                        data: [i as u8; 8],
                        len: 8,
                        is_extended: false,
                        timestamp_us: i as u64,
                    };
                    cb.on_frame_received(&frame);
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
