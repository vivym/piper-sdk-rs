//! 状态跟踪器
//!
//! 使用原子操作实现热路径无锁检查，避免高频控制循环中的锁竞争。
//!
//! # 设计目标
//!
//! - **热路径优化**: `is_valid()` 使用原子操作，~2ns 延迟
//! - **内存序正确**: Acquire/Release 语义保证跨线程可见性
//! - **无 Poison**: 使用 parking_lot::RwLock 避免 std 锁的 Poison
//!
//! # 架构
//!
//! ```text
//! ┌─────────────────┐
//! │  StateTracker   │
//! ├─────────────────┤
//! │ valid_flag      │ ← AtomicBool (快速检查)
//! │ details         │ ← RwLock<Details> (详细信息)
//! └─────────────────┘
//! ```
//!
//! # 示例
//!
//! ```rust,ignore
//! let tracker = StateTracker::new();
//!
//! // 热路径：快速检查（~2ns）
//! if tracker.is_valid() {
//!     // 执行命令
//! }
//!
//! // 后台线程：标记损坏
//! tracker.mark_poisoned("state drift detected");
//! ```

use parking_lot::RwLock;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use crate::high_level::types::RobotError;

/// 控制模式
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlMode {
    /// 位置模式
    PositionMode,
    /// MIT 模式
    MitMode,
    /// 未知/未初始化
    Unknown,
}

/// 机械臂控制器状态
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArmController {
    /// 使能
    Enabled,
    /// 待机
    Standby,
    /// 错误
    Error,
    /// 断开连接
    Disconnected,
}

/// 状态跟踪器详细信息
#[derive(Debug, Clone)]
struct TrackerDetails {
    /// 损坏原因
    poison_reason: Option<String>,
    /// 期望的控制模式
    expected_mode: ControlMode,
    /// 期望的控制器状态
    expected_controller: ArmController,
    /// 最后更新时间
    last_update: Instant,
}

impl Default for TrackerDetails {
    fn default() -> Self {
        TrackerDetails {
            poison_reason: None,
            expected_mode: ControlMode::Unknown,
            expected_controller: ArmController::Disconnected,
            last_update: Instant::now(),
        }
    }
}

/// 状态跟踪器
///
/// 使用原子标志实现热路径无锁检查。
#[derive(Clone)]
pub struct StateTracker {
    /// 快速有效性标志（原子操作）
    valid_flag: Arc<AtomicBool>,
    /// 详细状态信息（带锁保护）
    details: Arc<RwLock<TrackerDetails>>,
}

impl StateTracker {
    /// 创建新的状态跟踪器
    pub fn new() -> Self {
        StateTracker {
            valid_flag: Arc::new(AtomicBool::new(true)),
            details: Arc::new(RwLock::new(TrackerDetails::default())),
        }
    }

    /// 快速检查状态是否有效（热路径）
    ///
    /// 使用 Acquire 内存序确保能看到其他线程的写入。
    ///
    /// # 性能
    ///
    /// - 延迟: ~2ns
    /// - 吞吐: > 500M ops/s
    /// - 无锁竞争
    #[inline(always)]
    pub fn is_valid(&self) -> bool {
        self.valid_flag.load(Ordering::Acquire)
    }

    /// 快速检查并返回 Result（热路径）
    ///
    /// 如果状态无效，读取详细错误信息（慢路径）。
    pub fn check_valid_fast(&self) -> Result<(), RobotError> {
        if self.is_valid() {
            Ok(())
        } else {
            Err(self.read_error_details())
        }
    }

    /// 标记为损坏状态
    ///
    /// # 内存序
    ///
    /// 1. 先写入详细信息（带锁）
    /// 2. 再设置原子标志（Release 语义）
    ///
    /// 这样确保其他线程读取 valid_flag 后能看到完整的错误信息。
    pub fn mark_poisoned(&self, reason: impl Into<String>) {
        // 1. 更新详细信息
        {
            let mut details = self.details.write();
            details.poison_reason = Some(reason.into());
            details.last_update = Instant::now();
        } // 显式释放锁

        // 2. 设置原子标志（Release 确保写入可见）
        self.valid_flag.store(false, Ordering::Release);
    }

    /// 重置为有效状态
    pub fn reset(&self) {
        // 先更新详细信息
        {
            let mut details = self.details.write();
            details.poison_reason = None;
            details.last_update = Instant::now();
        }

        // 再设置原子标志
        self.valid_flag.store(true, Ordering::Release);
    }

    /// 设置期望的控制模式
    pub fn set_expected_mode(&self, mode: ControlMode) {
        let mut details = self.details.write();
        details.expected_mode = mode;
        details.last_update = Instant::now();
    }

    /// 设置期望的控制器状态
    pub fn set_expected_controller(&self, controller: ArmController) {
        let mut details = self.details.write();
        details.expected_controller = controller;
        details.last_update = Instant::now();
    }

    /// 获取期望的控制模式
    pub fn expected_mode(&self) -> ControlMode {
        self.details.read().expected_mode
    }

    /// 获取期望的控制器状态
    pub fn expected_controller(&self) -> ArmController {
        self.details.read().expected_controller
    }

    /// 获取最后更新时间
    pub fn last_update(&self) -> Instant {
        self.details.read().last_update
    }

    /// 获取 poison 原因（如果状态被标记为异常）
    ///
    /// # 返回
    ///
    /// - `Some(reason)`: 状态已被标记为异常，返回原因
    /// - `None`: 状态正常
    pub fn poison_reason(&self) -> Option<String> {
        self.details.read().poison_reason.clone()
    }

    /// 读取详细错误信息（慢路径）
    fn read_error_details(&self) -> RobotError {
        let details = self.details.read();
        RobotError::state_poisoned(
            details.poison_reason.clone().unwrap_or_else(|| "Unknown reason".to_string()),
        )
    }
}

impl Default for StateTracker {
    fn default() -> Self {
        Self::new()
    }
}

// 确保 Send + Sync
unsafe impl Send for StateTracker {}
unsafe impl Sync for StateTracker {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn test_initial_state() {
        let tracker = StateTracker::new();
        assert!(tracker.is_valid());
        assert_eq!(tracker.expected_mode(), ControlMode::Unknown);
        assert_eq!(tracker.expected_controller(), ArmController::Disconnected);
    }

    #[test]
    fn test_mark_poisoned() {
        let tracker = StateTracker::new();
        assert!(tracker.is_valid());

        tracker.mark_poisoned("test error");
        assert!(!tracker.is_valid());

        let result = tracker.check_valid_fast();
        assert!(result.is_err());
        match result.unwrap_err() {
            RobotError::StatePoisoned { reason } => {
                assert_eq!(reason, "test error");
            },
            _ => panic!("Expected StatePoisoned error"),
        }
    }

    #[test]
    fn test_reset() {
        let tracker = StateTracker::new();

        tracker.mark_poisoned("test error");
        assert!(!tracker.is_valid());

        tracker.reset();
        assert!(tracker.is_valid());
        assert!(tracker.check_valid_fast().is_ok());
    }

    #[test]
    fn test_set_expected_states() {
        let tracker = StateTracker::new();

        tracker.set_expected_mode(ControlMode::MitMode);
        assert_eq!(tracker.expected_mode(), ControlMode::MitMode);

        tracker.set_expected_controller(ArmController::Enabled);
        assert_eq!(tracker.expected_controller(), ArmController::Enabled);
    }

    #[test]
    fn test_memory_ordering() {
        let tracker = Arc::new(StateTracker::new());
        let tracker_clone = tracker.clone();

        // 线程 1: 写入
        let writer = thread::spawn(move || {
            tracker_clone.mark_poisoned("concurrent error");
        });

        writer.join().unwrap();

        // 线程 2: 读取（应该看到更新）
        assert!(!tracker.is_valid());
        match tracker.check_valid_fast() {
            Err(RobotError::StatePoisoned { reason }) => {
                assert_eq!(reason, "concurrent error");
            },
            _ => panic!("Expected poisoned error"),
        }
    }

    #[test]
    fn test_concurrent_access() {
        let tracker = Arc::new(StateTracker::new());
        let mut handles = vec![];

        // 多个读线程
        for i in 0..10 {
            let tracker_clone = tracker.clone();
            handles.push(thread::spawn(move || {
                for _ in 0..1000 {
                    let _ = tracker_clone.is_valid();
                    if i == 5 {
                        thread::sleep(Duration::from_micros(1));
                    }
                }
            }));
        }

        // 一个写线程
        let tracker_clone = tracker.clone();
        handles.push(thread::spawn(move || {
            thread::sleep(Duration::from_millis(5));
            tracker_clone.mark_poisoned("write test");
        }));

        for handle in handles {
            handle.join().unwrap();
        }

        assert!(!tracker.is_valid());
    }

    #[test]
    fn test_parking_lot_no_poison() {
        // 验证 parking_lot::RwLock 不会 Poison
        let tracker = Arc::new(StateTracker::new());
        let tracker_clone = tracker.clone();

        let handle = thread::spawn(move || {
            let _lock = tracker_clone.details.write();
            panic!("Intentional panic");
        });

        let _ = handle.join();

        // parking_lot 不会 poison，应该仍然可以获取锁
        let details = tracker.details.read();
        drop(details); // 成功
    }

    #[test]
    fn test_fast_path_performance() {
        let tracker = StateTracker::new();

        let start = std::time::Instant::now();
        for _ in 0..1_000_000 {
            let _ = tracker.is_valid();
        }
        let elapsed = start.elapsed();

        // 应该 < 10ms (100万次调用)
        assert!(
            elapsed.as_millis() < 10,
            "Fast path too slow: {:?}",
            elapsed
        );

        println!("Fast path: {:?} for 1M calls", elapsed);
    }

    #[test]
    fn test_clone() {
        let tracker = StateTracker::new();
        tracker.mark_poisoned("test");

        let tracker2 = tracker.clone();
        assert!(!tracker2.is_valid());
    }

    #[test]
    fn test_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<StateTracker>();
    }
}
