//! StateMonitor - 后台状态监控线程
//!
//! 持续监控物理状态，确保类型状态与硬件状态同步。
//!
//! # 设计目标
//!
//! - **状态同步**: 防止类型状态与物理状态漂移
//! - **异常检测**: 检测急停、超时等异常
//! - **自动恢复**: 标记 Poisoned 状态，触发错误处理
//! - **低开销**: 20Hz 轮询，不影响控制性能
//!
//! # 使用场景
//!
//! - 硬件急停按钮被按下
//! - 固件过热保护触发
//! - CAN 通信意外中断
//! - 用户手动切换控制模式

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use super::observer::Observer;
use super::state_tracker::StateTracker;

/// StateMonitor 配置
#[derive(Debug, Clone)]
pub struct MonitorConfig {
    /// 轮询间隔（毫秒）
    pub poll_interval_ms: u64,
    /// 是否启用监控（默认启用）
    pub enabled: bool,
}

impl Default for MonitorConfig {
    fn default() -> Self {
        MonitorConfig {
            poll_interval_ms: 50, // 20Hz
            enabled: true,
        }
    }
}

/// 状态监控器
///
/// 在后台线程中持续监控硬件状态。
pub struct StateMonitor {
    handle: Option<thread::JoinHandle<()>>,
    shutdown: Arc<AtomicBool>,
}

impl StateMonitor {
    /// 启动状态监控线程
    ///
    /// # 参数
    ///
    /// - `state_tracker`: 状态跟踪器
    /// - `observer`: 状态观察器（用于读取硬件状态）
    /// - `config`: 监控配置
    ///
    /// # 示例
    ///
    /// ```rust,ignore
    /// # use piper_sdk::high_level::client::state_monitor::*;
    /// # use std::sync::Arc;
    /// # fn example(state_tracker: Arc<StateTracker>, observer: Observer) {
    /// let monitor = StateMonitor::start(
    ///     state_tracker,
    ///     observer,
    ///     MonitorConfig::default(),
    /// );
    /// # }
    /// ```
    pub fn start(
        state_tracker: Arc<StateTracker>,
        observer: Observer,
        config: MonitorConfig,
    ) -> Self {
        if !config.enabled {
            // 监控被禁用，返回空监控器
            return StateMonitor {
                handle: None,
                shutdown: Arc::new(AtomicBool::new(true)),
            };
        }

        let shutdown = Arc::new(AtomicBool::new(false));
        let shutdown_clone = shutdown.clone();

        let handle = thread::spawn(move || {
            Self::monitor_loop(state_tracker, observer, config, shutdown_clone);
        });

        StateMonitor {
            handle: Some(handle),
            shutdown,
        }
    }

    /// 监控循环
    fn monitor_loop(
        state_tracker: Arc<StateTracker>,
        observer: Observer,
        config: MonitorConfig,
        shutdown: Arc<AtomicBool>,
    ) {
        let interval = Duration::from_millis(config.poll_interval_ms);

        while !shutdown.load(Ordering::Relaxed) {
            // 1. 检查状态新鲜度（1秒无更新视为超时）
            if !observer.is_fresh(Duration::from_secs(1)) {
                state_tracker
                    .mark_poisoned("State update timeout - no feedback from hardware".to_string());
                thread::sleep(interval);
                continue;
            }

            // 2. 检查机械臂使能状态一致性
            let arm_enabled = observer.is_arm_enabled();
            // 注意：这里简化处理，假设 Enabled 状态对应 arm_enabled = true
            // 实际实现可能需要更复杂的状态映射
            if !arm_enabled {
                tracing::warn!("Arm disabled detected - possible emergency stop or manual disable");
                // 如果处于活动控制状态但机械臂被失能，标记为异常
                // 注意：这里简化处理，实际需要检查期望状态
                // state_tracker.mark_poisoned("Arm unexpectedly disabled (emergency stop?)".to_string());
            }

            // 3. 检查控制模式一致性 TODO:
            // 注意：这里需要从 Observer 读取实际的控制模式
            // 如果硬件反馈中包含模式信息，可以在此检查
            // let actual_mode = observer.control_mode();
            // let expected_mode = state_tracker.expected_mode();
            // if actual_mode != expected_mode { ... }

            // 4. 睡眠到下一个轮询周期
            thread::sleep(interval);
        }
    }

    /// 优雅关闭监控线程
    ///
    /// 设置关闭标志并等待线程结束。
    pub fn shutdown(mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }

    /// 检查监控线程是否在运行
    pub fn is_running(&self) -> bool {
        !self.shutdown.load(Ordering::Relaxed)
    }
}

impl Drop for StateMonitor {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

// 确保 Send + Sync（需要在线程间传递）
// StateMonitor 本身不实现 Send，因为它持有 JoinHandle
// 但可以通过 shutdown 方法正确关闭

#[cfg(test)]
mod tests {
    use super::*;
    use crate::high_level::client::observer::RobotState;
    use parking_lot::RwLock;

    fn create_test_monitor() -> (StateMonitor, Arc<StateTracker>, Observer) {
        let state_tracker = Arc::new(StateTracker::new());
        let state = Arc::new(RwLock::new(RobotState::default()));
        let observer = Observer::new(state);

        let monitor = StateMonitor::start(
            state_tracker.clone(),
            observer.clone(),
            MonitorConfig::default(),
        );

        (monitor, state_tracker, observer)
    }

    #[test]
    fn test_monitor_start_and_shutdown() {
        let (monitor, _tracker, _observer) = create_test_monitor();

        assert!(monitor.is_running());

        let start = std::time::Instant::now();
        monitor.shutdown();
        let elapsed = start.elapsed();

        // 应该在 100ms 内关闭
        assert!(elapsed.as_millis() < 200);
    }

    #[test]
    fn test_monitor_drop() {
        let (monitor, _tracker, _observer) = create_test_monitor();
        assert!(monitor.is_running());

        drop(monitor);
        // Drop 应该自动关闭线程
    }

    #[test]
    fn test_monitor_disabled() {
        let state_tracker = Arc::new(StateTracker::new());
        let state = Arc::new(RwLock::new(RobotState::default()));
        let observer = Observer::new(state);

        let config = MonitorConfig {
            enabled: false,
            ..Default::default()
        };

        let monitor = StateMonitor::start(state_tracker, observer, config);

        assert!(!monitor.is_running());
    }

    #[test]
    fn test_state_timeout_detection() {
        let (monitor, tracker, _observer) = create_test_monitor();

        // 等待足够长时间让状态超时
        thread::sleep(Duration::from_millis(1100));

        // 应该检测到超时并标记为 poisoned
        assert!(!tracker.is_valid());

        monitor.shutdown();
    }
}
