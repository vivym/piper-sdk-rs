//! 钩子系统（Hook System）
//!
//! 本模块提供运行时钩子（Hook）管理功能，用于在 CAN 帧接收/发送时触发自定义回调。
//!
//! # 设计原则（v1.2.1）
//!
//! - **非阻塞**: 所有回调必须在 <1μs 内完成，使用 Channel 异步处理
//! - **职责分离**: HookManager 管理运行时回调，PipelineConfig 保持为 POD 数据
//! - **类型安全**: 使用 `dyn FrameCallback` trait object，支持多种回调类型
//!
//! # 使用示例
//!
//! ```rust
//! use piper_driver::hooks::{HookManager, FrameCallback};
//! use piper_driver::recording::AsyncRecordingHook;
//! use std::sync::Arc;
//!
//! // 创建钩子管理器
//! let mut hooks = HookManager::new();
//!
//! // 添加录制回调
//! let (hook, _rx) = AsyncRecordingHook::new();
//! let callback = Arc::new(hook) as Arc<dyn FrameCallback>;
//! hooks.add_callback(callback);
//!
//! // 触发所有回调（在 rx_loop 中）
//! let frame = piper_can::ReceivedFrame::new(
//!     piper_protocol::PiperFrame::new_standard(0x251, &[1, 2, 3, 4]).unwrap(),
//!     piper_can::TimestampProvenance::None,
//! );
//! hooks.trigger_all(frame);
//! ```

use crate::recording::{RecordedFrameDirection, RecordedFrameEvent, TimestampProvenance};
use piper_can::{PiperFrame, ReceivedFrame};
use std::sync::Arc;

/// 帧回调 Trait
///
/// 定义 CAN 帧回调接口，用于在接收或发送 CAN 帧时执行自定义逻辑。
///
/// # 性能要求
///
/// - **非阻塞**: 实现必须在 <1μs 内完成
/// - **无锁**: 禁止使用 Mutex、I/O、分配等阻塞操作
/// - **Channel 模式**: 推荐使用 `crossbeam::channel::Sender` 异步处理
///
/// # 示例
///
/// ```rust
/// use piper_driver::hooks::FrameCallback;
/// use piper_driver::recording::RecordedFrameEvent;
/// use crossbeam_channel::{Sender, bounded};
///
/// struct MyCallback {
///     sender: Sender<RecordedFrameEvent>,
/// }
///
/// impl FrameCallback for MyCallback {
///     fn on_frame(&self, event: RecordedFrameEvent) {
///         // ✅ 使用 try_send，非阻塞
///         let _ = self.sender.try_send(event);
///     }
/// }
/// ```
pub trait FrameCallback: Send + Sync {
    /// 当接收到或发送 CAN 帧时调用
    ///
    /// # 性能要求
    ///
    /// - 必须在 <1μs 内完成
    /// - 禁止阻塞操作（Mutex、I/O、分配）
    /// - 推荐使用 `try_send` 而非 `send`
    ///
    /// # 参数
    ///
    /// - `event`: 帧、方向和时间戳来源
    fn on_frame(&self, event: RecordedFrameEvent);
}

/// Runtime hook registration handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HookHandle(u64);

struct HookEntry {
    handle: HookHandle,
    callback: Arc<dyn FrameCallback>,
}

/// 钩子管理器
///
/// 专门管理运行时回调列表。
///
/// # 设计理由（v1.2.1）
///
/// - **Config vs Context 分离**:
///   - `PipelineConfig` 应该是 POD（Plain Old Data），用于序列化
///   - `PiperContext` 管理运行时状态和动态组件（如回调）
///
/// # 线程安全
///
/// 使用 `std::sync::Arc` 确保回调可以跨线程共享。
/// 回调列表本身不是线程安全的，需要外部同步（通常通过 `RwLock<HookManager>`）。
///
/// # 示例
///
/// ```rust
/// use piper_driver::hooks::{HookManager, FrameCallback};
/// use piper_driver::recording::AsyncRecordingHook;
/// use piper_can::{ReceivedFrame, TimestampProvenance};
/// use piper_protocol::PiperFrame;
/// use std::sync::Arc;
///
/// // 创建 hook manager 并添加回调
/// let mut hooks = HookManager::new();
/// let (hook, _rx) = AsyncRecordingHook::new();
/// let callback = Arc::new(hook) as Arc<dyn FrameCallback>;
/// hooks.add_callback(callback);
///
/// // 触发回调
/// let received = ReceivedFrame::new(
///     PiperFrame::new_standard(0x251, &[1, 2, 3, 4]).unwrap(),
///     TimestampProvenance::Kernel,
/// );
/// hooks.trigger_all(received);
/// ```
pub struct HookManager {
    next_handle: u64,
    /// 回调列表
    callbacks: Vec<HookEntry>,
}

impl Default for HookManager {
    fn default() -> Self {
        Self::new()
    }
}

impl HookManager {
    /// 创建新的钩子管理器
    #[must_use]
    pub const fn new() -> Self {
        Self {
            next_handle: 1,
            callbacks: Vec::new(),
        }
    }

    /// 添加回调
    ///
    /// # 线程安全
    ///
    /// 此方法不是线程安全的，需要外部同步（通常通过 `RwLock`）。
    ///
    /// # 参数
    ///
    /// - `callback`: 要添加的回调（必须实现 `FrameCallback`）
    ///
    /// # 示例
    ///
    /// ```rust
    /// use piper_driver::hooks::{HookManager, FrameCallback};
    /// use piper_driver::recording::AsyncRecordingHook;
    /// use std::sync::Arc;
    ///
    /// let mut hooks = HookManager::new();
    /// let (hook, _rx) = AsyncRecordingHook::new();
    /// let callback = Arc::new(hook) as Arc<dyn FrameCallback>;
    /// hooks.add_callback(callback);
    /// ```
    pub fn add_callback(&mut self, callback: Arc<dyn FrameCallback>) -> HookHandle {
        let handle = HookHandle(self.next_handle);
        self.next_handle = self.next_handle.wrapping_add(1).max(1);
        self.callbacks.push(HookEntry { handle, callback });
        handle
    }

    /// 移除指定回调。
    pub fn remove_callback(&mut self, handle: HookHandle) -> bool {
        if let Some(index) = self.callbacks.iter().position(|entry| entry.handle == handle) {
            self.callbacks.swap_remove(index);
            true
        } else {
            false
        }
    }

    /// 移除所有回调
    ///
    /// # 用途
    ///
    /// 主要用于测试或清理场景。
    pub fn clear(&mut self) {
        self.callbacks.clear();
    }

    /// 触发所有回调（在 rx_loop 中调用）
    ///
    /// # 性能要求
    ///
    /// - 总耗时 <1μs（假设每个回调 <100ns）
    /// - 非阻塞：所有回调必须使用 `try_send` 而非 `send`
    ///
    /// # 参数
    ///
    /// - `received`: 接收到的 CAN 帧及其时间戳来源
    ///
    /// # 示例
    ///
    /// ```rust
    /// use piper_driver::hooks::HookManager;
    /// use piper_driver::recording::AsyncRecordingHook;
    /// use piper_protocol::PiperFrame;
    /// use std::sync::Arc;
    ///
    /// let mut hooks = HookManager::new();
    /// let (hook, _rx) = AsyncRecordingHook::new();
    /// hooks.add_callback(Arc::new(hook));
    ///
    /// // 在 rx_loop 中触发
    /// let frame = piper_can::ReceivedFrame::new(
    ///     PiperFrame::new_standard(0x251, &[1, 2, 3, 4]).unwrap(),
    ///     piper_can::TimestampProvenance::None,
    /// );
    /// hooks.trigger_all(frame);
    /// ```
    pub fn trigger_all(&self, received: ReceivedFrame) {
        let event = RecordedFrameEvent {
            frame: received.frame,
            direction: RecordedFrameDirection::Rx,
            timestamp_provenance: received.timestamp_provenance,
        };
        for entry in self.callbacks.iter() {
            entry.callback.on_frame(event);
            // ^^^^ 使用 try_send，<1μs，非阻塞
        }
    }

    /// 触发所有 TX 回调（在 tx_loop 发送成功后调用）
    ///
    /// # 时机
    ///
    /// 仅在 `tx.send()` 成功后调用，确保录制的是实际发送的帧。
    ///
    /// # 参数
    ///
    /// - `frame`: 成功发送的 CAN 帧
    ///
    /// # 示例
    ///
    /// ```rust
    /// use piper_driver::hooks::HookManager;
    /// use piper_protocol::PiperFrame;
    ///
    /// let hooks = HookManager::new();
    ///
    /// // 在 tx_loop 中，发送成功后触发回调
    /// let frame = PiperFrame::new_standard(0x123, &[1, 2, 3, 4]).unwrap();
    /// // 假设 tx.send(&frame) 返回 Ok(())
    /// hooks.trigger_all_sent(&frame);
    /// ```
    pub fn trigger_all_sent(&self, frame: &PiperFrame) {
        let event = RecordedFrameEvent {
            frame: *frame,
            direction: RecordedFrameDirection::Tx,
            timestamp_provenance: TimestampProvenance::Userspace,
        };
        for entry in self.callbacks.iter() {
            entry.callback.on_frame(event);
        }
    }

    /// 获取回调数量
    ///
    /// # 用途
    ///
    /// 主要用于调试和监控。
    #[must_use]
    pub fn len(&self) -> usize {
        self.callbacks.len()
    }

    /// 检查是否为空
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.callbacks.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossbeam_channel::{Sender, bounded};
    use std::sync::atomic::{AtomicU64, Ordering};

    #[derive(Debug)]
    struct TestCallback {
        tx: Sender<RecordedFrameEvent>,
        count: Arc<AtomicU64>,
    }

    impl FrameCallback for TestCallback {
        fn on_frame(&self, event: RecordedFrameEvent) {
            let _ = self.tx.try_send(event);
            self.count.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[test]
    fn test_hook_manager_add_callback() {
        let mut hooks = HookManager::new();
        assert!(hooks.is_empty());

        let (tx, _rx) = bounded(10);
        let count = Arc::new(AtomicU64::new(0));
        let callback = Arc::new(TestCallback { tx, count });

        let _handle = hooks.add_callback(callback);
        assert_eq!(hooks.len(), 1);
    }

    #[test]
    fn test_hook_manager_trigger_all() {
        let mut hooks = HookManager::new();

        let (tx, rx) = bounded::<RecordedFrameEvent>(10);
        let count = Arc::new(AtomicU64::new(0));
        let callback = Arc::new(TestCallback {
            tx,
            count: count.clone(),
        });

        hooks.add_callback(callback);

        // 创建测试帧
        let frame = PiperFrame::new_standard(0x2A5, [0, 1, 2, 3, 4, 5, 6, 7])
            .unwrap()
            .with_timestamp_us(12345);

        // 触发回调
        hooks.trigger_all(ReceivedFrame::new(frame, TimestampProvenance::Kernel));

        // 验证
        assert_eq!(count.load(Ordering::Relaxed), 1);
        let event = rx.try_recv().unwrap();
        assert_eq!(event.direction, RecordedFrameDirection::Rx);
        assert_eq!(event.timestamp_provenance, TimestampProvenance::Kernel);
    }

    #[test]
    fn test_hook_manager_trigger_sent() {
        let mut hooks = HookManager::new();

        let (tx, rx) = bounded::<RecordedFrameEvent>(10);
        let count = Arc::new(AtomicU64::new(0));
        let callback = Arc::new(TestCallback {
            tx,
            count: count.clone(),
        });

        hooks.add_callback(callback);

        // 创建测试帧
        let frame = PiperFrame::new_standard(0x1A1, [0, 1, 2, 3, 4, 5, 6, 7])
            .unwrap()
            .with_timestamp_us(12345);

        // 触发 TX 回调
        hooks.trigger_all_sent(&frame);

        // 验证
        assert_eq!(count.load(Ordering::Relaxed), 1);
        let event = rx.try_recv().unwrap();
        assert_eq!(event.direction, RecordedFrameDirection::Tx);
        assert_eq!(event.timestamp_provenance, TimestampProvenance::Userspace);
    }

    #[test]
    fn test_hook_manager_clear() {
        let mut hooks = HookManager::new();

        let (tx, _rx) = bounded(10);
        let count = Arc::new(AtomicU64::new(0));
        let callback = Arc::new(TestCallback { tx, count });

        hooks.add_callback(callback);
        assert_eq!(hooks.len(), 1);

        hooks.clear();
        assert!(hooks.is_empty());
    }

    #[test]
    fn test_hook_manager_remove_callback() {
        let mut hooks = HookManager::new();
        let (tx, _rx) = bounded(10);
        let count = Arc::new(AtomicU64::new(0));
        let callback = Arc::new(TestCallback { tx, count });

        let handle = hooks.add_callback(callback);
        assert_eq!(hooks.len(), 1);
        assert!(hooks.remove_callback(handle));
        assert!(hooks.is_empty());
        assert!(!hooks.remove_callback(handle));
    }
}
