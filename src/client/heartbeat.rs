//! Heartbeat - 后台心跳机制
//!
//! **⚠️ 注意：此功能已禁用**
//!
//! 经过硬件验证，PiPER 机械臂**没有看门狗机制**，不需要定期发送心跳信号。
//! 此模块保留仅用于未来可能的扩展需求。
//!
//! # 历史设计目标（已废弃）
//!
//! - **超时保护**: 即使主线程冻结，心跳仍然发送
//! - **低开销**: 50Hz 发送频率，不影响控制性能
//! - **优雅关闭**: 可以安全停止心跳线程
//!
//! # 工作原理（已废弃）
//!
//! ~~硬件通常有看门狗定时器（Watchdog Timer），如果在规定时间内
//! 没有收到命令或心跳，会自动失能以保护安全。~~
//!
//! **实际情况**：PiPER 机械臂没有看门狗机制，不需要定期信号。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use crate::client::types::{Result, RobotError};
use crate::driver::Piper as RobotPiper;

/// Heartbeat 配置
#[derive(Debug, Clone)]
pub struct HeartbeatConfig {
    /// 心跳间隔（毫秒）
    pub interval_ms: u64,
    /// 是否启用心跳（默认启用）
    pub enabled: bool,
}

impl Default for HeartbeatConfig {
    fn default() -> Self {
        HeartbeatConfig {
            interval_ms: 20, // 50Hz
            enabled: false,  // 已禁用：机械臂没有看门狗机制，不需要心跳包
        }
    }
}

/// 心跳管理器
///
/// 在后台线程中定期发送心跳帧。
pub struct HeartbeatManager {
    handle: Option<thread::JoinHandle<()>>,
    shutdown: Arc<AtomicBool>,
}

impl HeartbeatManager {
    /// 启动心跳线程
    ///
    /// # 参数
    ///
    /// - `can_sender`: CAN 发送接口
    /// - `config`: 心跳配置
    ///
    /// # 示例
    ///
    /// ```rust,ignore
    /// # use piper_sdk::client::client::heartbeat::*;
    /// # use std::sync::Arc;
    /// # fn example(robot: Arc<RobotPiper>) {
    /// let heartbeat = HeartbeatManager::start(
    ///     robot,
    ///     HeartbeatConfig::default(),
    /// );
    /// # }
    /// ```
    pub fn start(robot: Arc<RobotPiper>, config: HeartbeatConfig) -> Self {
        if !config.enabled {
            // 心跳被禁用
            return HeartbeatManager {
                handle: None,
                shutdown: Arc::new(AtomicBool::new(true)),
            };
        }

        let shutdown = Arc::new(AtomicBool::new(false));
        let shutdown_clone = shutdown.clone();

        let handle = thread::spawn(move || {
            Self::heartbeat_loop(robot, config, shutdown_clone);
        });

        HeartbeatManager {
            handle: Some(handle),
            shutdown,
        }
    }

    /// 心跳循环
    fn heartbeat_loop(robot: Arc<RobotPiper>, config: HeartbeatConfig, shutdown: Arc<AtomicBool>) {
        let interval = Duration::from_millis(config.interval_ms);

        while !shutdown.load(Ordering::Relaxed) {
            // 发送心跳帧
            if let Err(_e) = Self::send_heartbeat(&robot) {
                // 心跳发送失败（通常是通信问题）
                // 记录警告但继续尝试
                tracing::warn!("Heartbeat failed: {}", _e);
            }

            thread::sleep(interval);
        }
    }

    /// 发送心跳帧
    ///
    /// **⚠️ 已废弃**：机械臂没有看门狗机制，不需要心跳包。
    /// 此方法保留仅用于未来可能的扩展需求。
    ///
    /// # 错误
    ///
    /// 目前总是返回错误，因为没有定义有效的心跳帧协议。
    /// 如果未来需要实现心跳，应该：
    /// 1. 与硬件厂商协商定义专用的心跳帧 ID
    /// 2. 或者使用协议中定义的合法指令（如定期发送 0x151）
    fn send_heartbeat(_robot: &Arc<RobotPiper>) -> Result<()> {
        // ❌ 不实现：机械臂没有看门狗机制
        Err(RobotError::ConfigError(
            "Heartbeat is not supported: robot does not have a watchdog mechanism".to_string(),
        ))
    }

    /// 优雅关闭心跳线程
    ///
    /// 设置关闭标志并等待线程结束。
    pub fn shutdown(mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }

    /// 检查心跳线程是否在运行
    pub fn is_running(&self) -> bool {
        !self.shutdown.load(Ordering::Relaxed)
    }
}

impl Drop for HeartbeatManager {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // 注意：heartbeat 测试需要真实的 robot 实例，应该在集成测试中完成
    // 这里只测试基本逻辑

    #[test]
    fn test_heartbeat_config_default() {
        let config = HeartbeatConfig::default();
        // ✅ 根据 HEARTBEAT_ANALYSIS_REPORT.md，默认应该禁用（机械臂没有看门狗机制）
        assert!(!config.enabled);
        assert_eq!(config.interval_ms, 20);
    }

    #[test]
    fn test_heartbeat_config_disabled() {
        let config = HeartbeatConfig {
            enabled: false,
            interval_ms: 10,
        };
        assert!(!config.enabled);
    }
}
