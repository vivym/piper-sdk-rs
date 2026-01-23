//! Heartbeat - 后台心跳机制
//!
//! 定期发送心跳信号，防止控制线程冻结导致硬件超时。
//!
//! # 设计目标
//!
//! - **超时保护**: 即使主线程冻结，心跳仍然发送
//! - **低开销**: 50Hz 发送频率，不影响控制性能
//! - **优雅关闭**: 可以安全停止心跳线程
//!
//! # 工作原理
//!
//! 硬件通常有看门狗定时器（Watchdog Timer），如果在规定时间内
//! 没有收到命令或心跳，会自动失能以保护安全。
//!
//! Heartbeat 在后台线程独立运行，确保即使主控制线程冻结
//! （如死锁、panic、长时间计算），硬件仍能收到信号。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use super::raw_commander::CanSender;
use crate::high_level::types::Result;

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
            enabled: true,
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
    /// # use piper_sdk::high_level::client::heartbeat::*;
    /// # use std::sync::Arc;
    /// # fn example(can_sender: Arc<dyn CanSender>) {
    /// let heartbeat = HeartbeatManager::start(
    ///     can_sender,
    ///     HeartbeatConfig::default(),
    /// );
    /// # }
    /// ```
    pub fn start(can_sender: Arc<dyn CanSender>, config: HeartbeatConfig) -> Self {
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
            Self::heartbeat_loop(can_sender, config, shutdown_clone);
        });

        HeartbeatManager {
            handle: Some(handle),
            shutdown,
        }
    }

    /// 心跳循环
    fn heartbeat_loop(
        can_sender: Arc<dyn CanSender>,
        config: HeartbeatConfig,
        shutdown: Arc<AtomicBool>,
    ) {
        let interval = Duration::from_millis(config.interval_ms);

        while !shutdown.load(Ordering::Relaxed) {
            // 发送心跳帧
            if let Err(_e) = Self::send_heartbeat(&can_sender) {
                // 心跳发送失败（通常是通信问题）
                // 记录警告但继续尝试
                tracing::warn!("Heartbeat failed: {}", _e);
            }

            thread::sleep(interval);
        }
    }

    /// 发送心跳帧
    fn send_heartbeat(can_sender: &Arc<dyn CanSender>) -> Result<()> {
        // 心跳帧 ID（通常是 0x00 或特定 ID）
        const HEARTBEAT_ID: u32 = 0x00;

        // 心跳帧数据（通常是空帧或特定数据）
        let data = vec![0xAA]; // 心跳标识

        can_sender.send_frame(HEARTBEAT_ID, &data)?;
        Ok(())
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
    use std::sync::Mutex;

    type RecordedFrames = Arc<Mutex<Vec<(u32, Vec<u8>)>>>;

    /// Mock CAN 发送器（记录发送的帧）
    struct MockCanSender {
        frames: RecordedFrames,
    }

    impl MockCanSender {
        fn new() -> Self {
            MockCanSender {
                frames: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn get_frame_count(&self) -> usize {
            self.frames.lock().unwrap().len()
        }
    }

    impl CanSender for MockCanSender {
        fn send_frame(&self, id: u32, data: &[u8]) -> Result<()> {
            self.frames.lock().unwrap().push((id, data.to_vec()));
            Ok(())
        }

        fn recv_frame(&self, _timeout_ms: u64) -> Result<(u32, Vec<u8>)> {
            Ok((0, vec![]))
        }
    }

    #[test]
    fn test_heartbeat_start_and_shutdown() {
        let sender = Arc::new(MockCanSender::new());
        let heartbeat = HeartbeatManager::start(sender.clone(), HeartbeatConfig::default());

        assert!(heartbeat.is_running());

        let start = std::time::Instant::now();
        heartbeat.shutdown();
        let elapsed = start.elapsed();

        // 应该在 100ms 内关闭
        assert!(elapsed.as_millis() < 100);
    }

    #[test]
    fn test_heartbeat_sends_frames() {
        let sender = Arc::new(MockCanSender::new());
        let heartbeat = HeartbeatManager::start(
            sender.clone(),
            HeartbeatConfig {
                interval_ms: 10, // 快速测试
                enabled: true,
            },
        );

        // 等待一些心跳
        thread::sleep(Duration::from_millis(100));

        let frame_count = sender.get_frame_count();

        // 应该至少发送了几个心跳（100ms / 10ms ≈ 10 个）
        assert!(
            frame_count >= 5,
            "Expected at least 5 frames, got {}",
            frame_count
        );

        heartbeat.shutdown();
    }

    #[test]
    fn test_heartbeat_drop() {
        let sender = Arc::new(MockCanSender::new());
        let heartbeat = HeartbeatManager::start(sender, HeartbeatConfig::default());

        assert!(heartbeat.is_running());
        drop(heartbeat);
        // Drop 应该自动关闭线程
    }

    #[test]
    fn test_heartbeat_disabled() {
        let sender = Arc::new(MockCanSender::new());
        let config = HeartbeatConfig {
            enabled: false,
            ..Default::default()
        };

        let heartbeat = HeartbeatManager::start(sender.clone(), config);

        assert!(!heartbeat.is_running());

        thread::sleep(Duration::from_millis(100));

        // 应该没有发送任何帧
        assert_eq!(sender.get_frame_count(), 0);
    }

    #[test]
    fn test_heartbeat_frequency() {
        let sender = Arc::new(MockCanSender::new());
        let config = HeartbeatConfig {
            interval_ms: 20, // 50Hz
            enabled: true,
        };

        let heartbeat = HeartbeatManager::start(sender.clone(), config);

        // 运行 1 秒
        thread::sleep(Duration::from_secs(1));

        let frame_count = sender.get_frame_count();

        // 50Hz = 50 帧/秒，允许 ±20% 误差
        assert!(
            (40..=60).contains(&frame_count),
            "Expected ~50 frames, got {}",
            frame_count
        );

        heartbeat.shutdown();
    }
}
