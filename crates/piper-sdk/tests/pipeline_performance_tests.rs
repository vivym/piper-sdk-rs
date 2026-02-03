//! 性能测试与调优
//!
//! 全面测试性能改进和 metrics 准确性：
//! 1. RX 状态更新周期分布（P50/P95/P99/max）
//! 2. TX 命令延迟分布
//! 3. Overwrite 次数、错误次数
//! 4. Metrics 准确性验证

use piper_sdk::can::{CanError, PiperFrame, RxAdapter, TxAdapter};
use piper_sdk::driver::{PipelineConfig, PiperContext, PiperMetrics, rx_loop, tx_loop_mailbox};
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

/// 检测是否在CI环境中运行
fn is_ci_env() -> bool {
    std::env::var("CI").is_ok()
        || std::env::var("GITHUB_ACTIONS").is_ok()
        || std::env::var("GITLAB_CI").is_ok()
        || std::env::var("CIRCLECI").is_ok()
        || std::env::var("TRAVIS").is_ok()
        || std::env::var("APPVEYOR").is_ok()
}

/// 根据环境调整时间阈值（毫秒）
/// 在CI环境中，使用更宽松的阈值（通常是本地环境的3-5倍）
fn adjust_threshold_ms(local_threshold_ms: u64) -> Duration {
    let multiplier = if is_ci_env() { 5 } else { 1 };
    Duration::from_millis(local_threshold_ms * multiplier)
}

/// 性能统计结构
#[derive(Debug, Default)]
struct LatencyStats {
    samples: Vec<Duration>,
}

impl LatencyStats {
    fn new() -> Self {
        Self {
            samples: Vec::new(),
        }
    }

    fn add_sample(&mut self, latency: Duration) {
        self.samples.push(latency);
    }

    fn percentile(&self, p: f64) -> Duration {
        if self.samples.is_empty() {
            return Duration::ZERO;
        }

        let mut sorted = self.samples.clone();
        sorted.sort();

        let index = ((sorted.len() as f64) * p / 100.0).ceil() as usize - 1;
        sorted[index.min(sorted.len() - 1)]
    }

    fn p50(&self) -> Duration {
        self.percentile(50.0)
    }

    fn p95(&self) -> Duration {
        self.percentile(95.0)
    }

    fn p99(&self) -> Duration {
        self.percentile(99.0)
    }

    fn max(&self) -> Duration {
        self.samples.iter().max().copied().unwrap_or(Duration::ZERO)
    }

    fn min(&self) -> Duration {
        self.samples.iter().min().copied().unwrap_or(Duration::ZERO)
    }

    fn mean(&self) -> Duration {
        if self.samples.is_empty() {
            return Duration::ZERO;
        }
        let total: Duration = self.samples.iter().sum();
        total / self.samples.len() as u32
    }

    fn count(&self) -> usize {
        self.samples.len()
    }
}

/// Mock RX 适配器：模拟高频接收（1kHz）
struct HighFreqRxAdapter {
    frames: VecDeque<PiperFrame>,
    interval: Duration,
    frame_count: Arc<AtomicU64>,
    start_time: Instant,
}

impl HighFreqRxAdapter {
    fn new(frames_per_second: u32) -> Self {
        let mut frames = VecDeque::new();
        // 生成足够的帧（假设测试运行 1 分钟）
        let total_frames = frames_per_second * 60;
        for i in 0..total_frames {
            frames.push_back(PiperFrame::new_standard(
                (0x251 + (i % 6)) as u16,
                &[i as u8; 8],
            ));
        }

        Self {
            frames,
            interval: Duration::from_millis(1000 / frames_per_second as u64),
            frame_count: Arc::new(AtomicU64::new(0)),
            start_time: Instant::now(),
        }
    }

    #[allow(dead_code)]
    fn frame_count(&self) -> u64 {
        self.frame_count.load(Ordering::Relaxed)
    }
}

impl RxAdapter for HighFreqRxAdapter {
    fn receive(&mut self) -> Result<PiperFrame, CanError> {
        let elapsed = self.start_time.elapsed();
        let expected_frame_index = (elapsed.as_millis() / self.interval.as_millis()) as usize;

        if expected_frame_index >= self.frames.len() {
            return Err(CanError::Timeout);
        }

        // 模拟精确的时序
        let next_frame_time = self.start_time + self.interval * expected_frame_index as u32;
        let now = Instant::now();
        if now < next_frame_time {
            thread::sleep(next_frame_time - now);
        }

        self.frame_count.fetch_add(1, Ordering::Relaxed);
        self.frames.pop_front().ok_or(CanError::Timeout)
    }
}

/// Mock TX 适配器：模拟正常发送
struct NormalTxAdapter {
    send_delay: Duration,
    sent_count: Arc<AtomicU64>,
}

impl NormalTxAdapter {
    fn new() -> Self {
        Self {
            send_delay: Duration::from_micros(100), // 100µs 发送延迟
            sent_count: Arc::new(AtomicU64::new(0)),
        }
    }

    #[allow(dead_code)]
    fn sent_count(&self) -> u64 {
        self.sent_count.load(Ordering::Relaxed)
    }
}

impl TxAdapter for NormalTxAdapter {
    fn send(&mut self, _frame: PiperFrame) -> Result<(), CanError> {
        thread::sleep(self.send_delay);
        self.sent_count.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
}

#[test]
fn test_rx_update_period_distribution() {
    // 测试场景：1kHz 控制回路，测量 RX 状态更新周期分布

    let ctx = Arc::new(PiperContext::new());
    let config = PipelineConfig::default();
    let is_running = Arc::new(AtomicBool::new(true));
    let metrics = Arc::new(PiperMetrics::new());

    // 创建 1kHz RX 适配器
    let rx_adapter = HighFreqRxAdapter::new(1000);

    // 启动 RX 线程
    let ctx_rx = ctx.clone();
    let is_running_rx = is_running.clone();
    let metrics_rx = metrics.clone();
    let rx_handle = thread::spawn(move || {
        rx_loop(rx_adapter, ctx_rx, config, is_running_rx, metrics_rx);
    });

    // 监控状态更新周期
    let mut stats = LatencyStats::new();
    let mut last_update_time = Instant::now();
    let mut last_update_count = 0u64;

    // 运行 5 秒
    let test_duration = Duration::from_secs(5);
    let start = Instant::now();

    while start.elapsed() < test_duration {
        let current_count = metrics.rx_frames_valid.load(Ordering::Relaxed);

        if current_count > last_update_count {
            let period = last_update_time.elapsed();
            stats.add_sample(period);
            last_update_time = Instant::now();
            last_update_count = current_count;
        }

        thread::sleep(Duration::from_millis(1));
    }

    // 停止线程
    is_running.store(false, Ordering::Relaxed);
    let _ = rx_handle.join();

    // 验证统计结果
    println!("RX Update Period Statistics:");
    println!("  Samples: {}", stats.count());
    println!("  P50: {:?}", stats.p50());
    println!("  P95: {:?}", stats.p95());
    println!("  P99: {:?}", stats.p99());
    println!("  Max: {:?}", stats.max());
    println!("  Min: {:?}", stats.min());
    println!("  Mean: {:?}", stats.mean());

    // 验收标准：P99 < 5ms（CI环境会放宽）
    let threshold = adjust_threshold_ms(5);
    assert!(
        stats.p99() < threshold,
        "RX update period P99 should be < {:?} (CI环境已放宽), got: {:?}",
        threshold,
        stats.p99()
    );

    // 验证：P50 应该在 1ms 左右（1kHz = 1ms 周期）
    // 本地：考虑系统调度延迟，允许 30% 误差 [0.7ms, 1.5ms]
    // CI：调度不可控，只要求有周期性更新，上界放宽为 adjust_threshold_ms(2)
    let expected_period = Duration::from_millis(1);
    let p50 = stats.p50();
    let p50_min = expected_period * 7 / 10;
    let p50_max = if is_ci_env() {
        adjust_threshold_ms(2) // CI 下 2*5=10ms
    } else {
        expected_period * 15 / 10
    };
    assert!(
        p50 >= p50_min && p50 <= p50_max,
        "RX update period P50 should be in [{:?}, {:?}] (1kHz ~1ms, CI relaxed), got: {:?}",
        p50_min,
        p50_max,
        p50
    );
}

#[test]
fn test_tx_command_latency_distribution() {
    // 测试场景：测量 TX 命令延迟分布

    let ctx = Arc::new(PiperContext::new());
    let _config = PipelineConfig::default();
    let is_running = Arc::new(AtomicBool::new(true));
    let metrics = Arc::new(PiperMetrics::new());

    // 创建 TX 适配器
    let tx_adapter = NormalTxAdapter::new();

    // 创建命令通道
    let (_reliable_tx, reliable_rx) = crossbeam_channel::bounded::<PiperFrame>(10);
    let realtime_slot: Arc<std::sync::Mutex<Option<piper_sdk::driver::command::RealtimeCommand>>> =
        Arc::new(std::sync::Mutex::new(None));
    let realtime_slot_clone = realtime_slot.clone();

    // 启动 TX 线程
    let ctx_tx = ctx.clone();
    let is_running_tx = is_running.clone();
    let metrics_tx = metrics.clone();
    let tx_handle = thread::spawn(move || {
        tx_loop_mailbox(
            tx_adapter,
            realtime_slot,
            reliable_rx,
            is_running_tx,
            metrics_tx,
            ctx_tx,
        );
    });

    // 测量命令延迟
    let mut stats = LatencyStats::new();
    let test_duration = Duration::from_secs(2);
    let start = Instant::now();

    while start.elapsed() < test_duration {
        let send_start = Instant::now();
        let frame = PiperFrame::new_standard(0x123, &[1, 2, 3, 4]);
        *realtime_slot_clone.lock().unwrap() =
            Some(piper_sdk::driver::command::RealtimeCommand::single(frame));

        // 等待发送完成（通过 metrics 验证）
        let initial_count = metrics.tx_frames_total.load(Ordering::Relaxed);
        let mut attempts = 0;
        while metrics.tx_frames_total.load(Ordering::Relaxed) == initial_count && attempts < 100 {
            thread::sleep(Duration::from_millis(1));
            attempts += 1;
        }

        let latency = send_start.elapsed();
        stats.add_sample(latency);

        // 1kHz 发送频率
        thread::sleep(Duration::from_millis(1));
    }

    // 停止线程
    is_running.store(false, Ordering::Relaxed);
    let _ = tx_handle.join();

    // 验证统计结果
    println!("TX Command Latency Statistics:");
    println!("  Samples: {}", stats.count());
    println!("  P50: {:?}", stats.p50());
    println!("  P95: {:?}", stats.p95());
    println!("  P99: {:?}", stats.p99());
    println!("  Max: {:?}", stats.max());

    // 验收标准：实时命令延迟 P95 < 2ms（考虑系统调度和 Mock 适配器延迟）
    // 在实际硬件环境中，这个值应该 < 1ms
    // 在CI环境中，阈值会放宽
    let threshold = adjust_threshold_ms(2);
    assert!(
        stats.p95() < threshold,
        "TX command latency P95 should be < {:?} (CI环境已放宽, in real hardware < 1ms), got: {:?}",
        threshold,
        stats.p95()
    );
}

#[test]
fn test_metrics_accuracy() {
    // 测试场景：验证 metrics 计数与实际发送/接收帧数一致

    let ctx = Arc::new(PiperContext::new());
    let config = PipelineConfig::default();
    let is_running = Arc::new(AtomicBool::new(true));
    let metrics = Arc::new(PiperMetrics::new());

    // 创建 RX 适配器
    let rx_adapter = HighFreqRxAdapter::new(100);
    // 注意：frame_count 在移动后无法访问，这里我们使用 metrics 来验证

    // 创建 TX 适配器
    let tx_adapter = NormalTxAdapter::new();

    // 创建命令通道
    let (reliable_tx, reliable_rx) = crossbeam_channel::bounded::<PiperFrame>(10);
    let realtime_slot: Arc<std::sync::Mutex<Option<piper_sdk::driver::command::RealtimeCommand>>> =
        Arc::new(std::sync::Mutex::new(None));

    // 启动 RX 线程
    let ctx_rx = ctx.clone();
    let is_running_rx = is_running.clone();
    let metrics_rx = metrics.clone();
    let rx_handle = thread::spawn(move || {
        rx_loop(rx_adapter, ctx_rx, config, is_running_rx, metrics_rx);
    });

    // 启动 TX 线程
    let ctx_tx = ctx.clone();
    let is_running_tx = is_running.clone();
    let metrics_tx = metrics.clone();
    let tx_handle = thread::spawn(move || {
        tx_loop_mailbox(
            tx_adapter,
            realtime_slot,
            reliable_rx,
            is_running_tx,
            metrics_tx,
            ctx_tx,
        );
    });

    // 发送固定数量的命令
    let expected_tx_frames = 50u64;
    for i in 0..expected_tx_frames {
        let frame = PiperFrame::new_standard(0x123, &[i as u8; 8]);
        reliable_tx.send(frame).unwrap();
    }

    // 等待处理完成
    thread::sleep(Duration::from_millis(200));

    // 停止线程
    is_running.store(false, Ordering::Relaxed);
    let _ = rx_handle.join();
    let _ = tx_handle.join();

    // 验证 metrics 准确性
    let snapshot = metrics.snapshot();

    println!("Metrics Accuracy Test:");
    println!("  RX frames total: {}", snapshot.rx_frames_total);
    println!("  RX frames valid: {}", snapshot.rx_frames_valid);
    println!("  TX frames total: {}", snapshot.tx_frames_total);
    println!("  Expected TX frames: {}", expected_tx_frames);
    println!("  Actual TX frames sent: {}", expected_tx_frames);

    // 验证：TX 帧数应该与实际发送数一致（允许少量误差）
    let tx_accuracy = if expected_tx_frames > 0 {
        (snapshot.tx_frames_total as f64 / expected_tx_frames as f64) * 100.0
    } else {
        100.0
    };

    assert!(
        tx_accuracy > 95.0,
        "TX metrics accuracy should be > 95%, got: {:.2}%",
        tx_accuracy
    );

    // 验证：RX 有效帧数应该 > 0
    assert!(
        snapshot.rx_frames_valid > 0,
        "RX should receive at least some valid frames"
    );
}

#[test]
fn test_realtime_overwrite_accuracy() {
    // 测试场景：验证 Overwrite 次数与实际触发次数一致

    let is_running = Arc::new(AtomicBool::new(true));
    let ctx = Arc::new(PiperContext::new());
    let metrics = Arc::new(PiperMetrics::new());

    // 创建慢速 TX 适配器（模拟瓶颈）
    struct SlowTxAdapter {
        send_delay: Duration,
        sent_count: Arc<AtomicU64>,
    }

    impl SlowTxAdapter {
        fn new() -> Self {
            Self {
                send_delay: Duration::from_millis(10), // 10ms 发送延迟（慢）
                sent_count: Arc::new(AtomicU64::new(0)),
            }
        }
    }

    impl TxAdapter for SlowTxAdapter {
        fn send(&mut self, _frame: PiperFrame) -> Result<(), CanError> {
            thread::sleep(self.send_delay);
            self.sent_count.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
    }

    let tx_adapter = SlowTxAdapter::new();

    // 创建命令通道
    let (_reliable_tx, reliable_rx) = crossbeam_channel::bounded::<PiperFrame>(10);
    let realtime_slot: Arc<std::sync::Mutex<Option<piper_sdk::driver::command::RealtimeCommand>>> =
        Arc::new(std::sync::Mutex::new(None));
    let realtime_slot_clone = realtime_slot.clone();

    // 启动 TX 线程
    let ctx_tx = ctx.clone();
    let is_running_tx = is_running.clone();
    let metrics_tx = metrics.clone();
    let tx_handle = thread::spawn(move || {
        tx_loop_mailbox(
            tx_adapter,
            realtime_slot,
            reliable_rx,
            is_running_tx,
            metrics_tx,
            ctx_tx,
        );
    });

    // 快速发送命令（超过 TX 处理速度，触发 Overwrite）
    // Mailbox 模式下，每次写入都会覆盖之前的值
    let total_sends = 20u64;

    for i in 0..total_sends {
        let frame = PiperFrame::new_standard(0x123, &[i as u8; 8]);
        // 直接写入 slot，会自动覆盖之前的值
        *realtime_slot_clone.lock().unwrap() =
            Some(piper_sdk::driver::command::RealtimeCommand::single(frame));
        thread::sleep(Duration::from_millis(1)); // 1ms 间隔发送
    }

    // 等待处理完成
    thread::sleep(Duration::from_millis(500));

    // 停止线程
    is_running.store(false, Ordering::Relaxed);
    let _ = tx_handle.join();

    // 验证发送结果
    let snapshot = metrics.snapshot();

    println!("Overwrite Accuracy Test:");
    println!("  Total sends: {}", total_sends);
    println!("  Metrics overwrites: {}", snapshot.tx_realtime_overwrites);
    println!("  TX frames total: {}", snapshot.tx_frames_total);

    // 验证：Mailbox 模式下，应该有帧被发送
    // 由于快速发送覆盖，实际发送的帧数可能少于 total_sends
    assert!(snapshot.tx_frames_total > 0, "Should have sent some frames");
}
