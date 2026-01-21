//! Piper SDK 性能指标模块
//!
//! 提供零开销的原子计数器，用于监控 IO 链路的健康状态和性能。
//! 所有计数器都使用原子操作，可以在任何线程安全地读取，不会引入锁竞争。

use std::sync::atomic::{AtomicU64, Ordering};

/// Piper SDK 实时指标
///
/// 用于监控 IO 链路的健康状态和性能。所有计数器都使用原子操作，
/// 可以在任何线程安全地读取，不会引入锁竞争。
///
/// # 使用示例
///
/// ```rust
/// use piper_sdk::robot::PiperMetrics;
/// use std::sync::Arc;
/// use std::sync::atomic::Ordering;
///
/// let metrics = Arc::new(PiperMetrics::default());
///
/// // 在 IO 线程中更新指标
/// metrics.rx_frames_total.fetch_add(1, Ordering::Relaxed);
///
/// // 在主线程中读取快照
/// let snapshot = metrics.snapshot();
/// println!("Total RX frames: {}", snapshot.rx_frames_total);
/// ```
#[derive(Debug, Default)]
pub struct PiperMetrics {
    /// RX 接收的总帧数（包括被过滤的 Echo 帧）
    pub rx_frames_total: AtomicU64,

    /// RX 有效帧数（过滤 Echo 后的真实反馈帧）
    pub rx_frames_valid: AtomicU64,

    /// RX 过滤掉的 Echo 帧数（GS-USB 特有）
    pub rx_echo_filtered: AtomicU64,

    /// TX 发送的总帧数
    pub tx_frames_total: AtomicU64,

    /// TX 实时队列覆盖（Overwrite）次数
    ///
    /// 如果这个值快速增长，说明 TX 线程处理速度跟不上命令生成速度，
    /// 或者总线/设备存在瓶颈。
    pub tx_realtime_overwrites: AtomicU64,

    /// TX 可靠队列满（阻塞/失败）次数
    pub tx_reliable_drops: AtomicU64,

    /// USB/CAN 设备错误次数
    pub device_errors: AtomicU64,

    /// RX 超时次数（正常现象，无数据时会超时）
    pub rx_timeouts: AtomicU64,

    /// TX 超时次数（异常现象，说明设备响应慢）
    pub tx_timeouts: AtomicU64,
}

impl PiperMetrics {
    /// 创建新的指标实例（所有计数器初始化为 0）
    pub fn new() -> Self {
        Self::default()
    }

    /// 获取人类可读的指标快照
    ///
    /// 返回一个包含所有计数器当前值的快照结构。
    /// 快照是原子读取的，保证一致性（虽然不同计数器之间可能有微小的时间差）。
    ///
    /// # 性能
    ///
    /// 使用 `Ordering::Relaxed`，性能最优，适合监控场景。
    pub fn snapshot(&self) -> MetricsSnapshot {
        MetricsSnapshot {
            rx_frames_total: self.rx_frames_total.load(Ordering::Relaxed),
            rx_frames_valid: self.rx_frames_valid.load(Ordering::Relaxed),
            rx_echo_filtered: self.rx_echo_filtered.load(Ordering::Relaxed),
            tx_frames_total: self.tx_frames_total.load(Ordering::Relaxed),
            tx_realtime_overwrites: self.tx_realtime_overwrites.load(Ordering::Relaxed),
            tx_reliable_drops: self.tx_reliable_drops.load(Ordering::Relaxed),
            device_errors: self.device_errors.load(Ordering::Relaxed),
            rx_timeouts: self.rx_timeouts.load(Ordering::Relaxed),
            tx_timeouts: self.tx_timeouts.load(Ordering::Relaxed),
        }
    }

    /// 重置所有计数器（用于性能测试）
    ///
    /// 将所有计数器重置为 0。使用 `Ordering::Relaxed`，性能最优。
    pub fn reset(&self) {
        self.rx_frames_total.store(0, Ordering::Relaxed);
        self.rx_frames_valid.store(0, Ordering::Relaxed);
        self.rx_echo_filtered.store(0, Ordering::Relaxed);
        self.tx_frames_total.store(0, Ordering::Relaxed);
        self.tx_realtime_overwrites.store(0, Ordering::Relaxed);
        self.tx_reliable_drops.store(0, Ordering::Relaxed);
        self.device_errors.store(0, Ordering::Relaxed);
        self.rx_timeouts.store(0, Ordering::Relaxed);
        self.tx_timeouts.store(0, Ordering::Relaxed);
    }
}

/// 指标快照（不可变，用于读取）
///
/// 包含所有计数器的当前值，用于一次性读取所有指标，避免多次原子操作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MetricsSnapshot {
    /// RX 接收的总帧数
    pub rx_frames_total: u64,
    /// RX 有效帧数
    pub rx_frames_valid: u64,
    /// RX 过滤掉的 Echo 帧数
    pub rx_echo_filtered: u64,
    /// TX 发送的总帧数
    pub tx_frames_total: u64,
    /// TX 实时队列覆盖次数
    pub tx_realtime_overwrites: u64,
    /// TX 可靠队列满次数
    pub tx_reliable_drops: u64,
    /// 设备错误次数
    pub device_errors: u64,
    /// RX 超时次数
    pub rx_timeouts: u64,
    /// TX 超时次数
    pub tx_timeouts: u64,
}

impl MetricsSnapshot {
    /// 计算 Echo 帧过滤率（百分比）
    ///
    /// 返回 0.0 到 100.0 之间的值。如果 `rx_frames_total` 为 0，返回 0.0。
    pub fn echo_filter_rate(&self) -> f64 {
        if self.rx_frames_total == 0 {
            return 0.0;
        }
        (self.rx_echo_filtered as f64 / self.rx_frames_total as f64) * 100.0
    }

    /// 计算有效帧率（百分比）
    ///
    /// 返回 0.0 到 100.0 之间的值。如果 `rx_frames_total` 为 0，返回 0.0。
    pub fn valid_frame_rate(&self) -> f64 {
        if self.rx_frames_total == 0 {
            return 0.0;
        }
        (self.rx_frames_valid as f64 / self.rx_frames_total as f64) * 100.0
    }

    /// 计算实时队列覆盖率（百分比）
    ///
    /// 返回 0.0 到 100.0 之间的值。如果 `tx_frames_total` 为 0，返回 0.0。
    pub fn overwrite_rate(&self) -> f64 {
        if self.tx_frames_total == 0 {
            return 0.0;
        }
        (self.tx_realtime_overwrites as f64 / self.tx_frames_total as f64) * 100.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn test_metrics_default() {
        let metrics = PiperMetrics::new();
        let snapshot = metrics.snapshot();

        assert_eq!(snapshot.rx_frames_total, 0);
        assert_eq!(snapshot.rx_frames_valid, 0);
        assert_eq!(snapshot.tx_frames_total, 0);
    }

    #[test]
    fn test_metrics_increment() {
        let metrics = Arc::new(PiperMetrics::new());

        metrics.rx_frames_total.fetch_add(10, Ordering::Relaxed);
        metrics.rx_frames_valid.fetch_add(8, Ordering::Relaxed);
        metrics.rx_echo_filtered.fetch_add(2, Ordering::Relaxed);

        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.rx_frames_total, 10);
        assert_eq!(snapshot.rx_frames_valid, 8);
        assert_eq!(snapshot.rx_echo_filtered, 2);
    }

    #[test]
    fn test_metrics_reset() {
        let metrics = Arc::new(PiperMetrics::new());

        metrics.rx_frames_total.fetch_add(100, Ordering::Relaxed);
        metrics.tx_frames_total.fetch_add(50, Ordering::Relaxed);

        let snapshot_before = metrics.snapshot();
        assert_eq!(snapshot_before.rx_frames_total, 100);
        assert_eq!(snapshot_before.tx_frames_total, 50);

        metrics.reset();

        let snapshot_after = metrics.snapshot();
        assert_eq!(snapshot_after.rx_frames_total, 0);
        assert_eq!(snapshot_after.tx_frames_total, 0);
    }

    #[test]
    fn test_metrics_concurrent_updates() {
        let metrics = Arc::new(PiperMetrics::new());
        let mut handles = vec![];

        // 启动 10 个线程，每个线程增加 100 次
        for _ in 0..10 {
            let m = metrics.clone();
            let handle = thread::spawn(move || {
                for _ in 0..100 {
                    m.rx_frames_total.fetch_add(1, Ordering::Relaxed);
                }
            });
            handles.push(handle);
        }

        // 等待所有线程完成
        for handle in handles {
            handle.join().unwrap();
        }

        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.rx_frames_total, 1000);
    }

    #[test]
    fn test_metrics_snapshot_rates() {
        let snapshot = MetricsSnapshot {
            rx_frames_total: 100,
            rx_frames_valid: 80,
            rx_echo_filtered: 20,
            tx_frames_total: 50,
            tx_realtime_overwrites: 5,
            tx_reliable_drops: 0,
            device_errors: 0,
            rx_timeouts: 10,
            tx_timeouts: 0,
        };

        assert_eq!(snapshot.echo_filter_rate(), 20.0);
        assert_eq!(snapshot.valid_frame_rate(), 80.0);
        assert_eq!(snapshot.overwrite_rate(), 10.0);
    }

    #[test]
    fn test_metrics_snapshot_rates_zero_total() {
        let snapshot = MetricsSnapshot {
            rx_frames_total: 0,
            rx_frames_valid: 0,
            rx_echo_filtered: 0,
            tx_frames_total: 0,
            tx_realtime_overwrites: 0,
            tx_reliable_drops: 0,
            device_errors: 0,
            rx_timeouts: 0,
            tx_timeouts: 0,
        };

        assert_eq!(snapshot.echo_filter_rate(), 0.0);
        assert_eq!(snapshot.valid_frame_rate(), 0.0);
        assert_eq!(snapshot.overwrite_rate(), 0.0);
    }
}
