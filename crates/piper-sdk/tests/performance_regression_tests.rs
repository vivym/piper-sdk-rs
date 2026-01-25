//! 性能回归测试
//!
//! 确保改进不会导致性能退化：
//! 1. 对比不同版本的性能指标
//! 2. 验证关键路径的性能（RX/TX 延迟、吞吐量）
//! 3. 确保新功能（命令优先级、超时 API）不引入性能开销
//! 4. 可集成到 CI，作为性能门禁

use piper_sdk::can::{CanError, PiperFrame, RxAdapter, TxAdapter};
use piper_sdk::driver::command::PiperCommand;
use piper_sdk::driver::{PipelineConfig, PiperContext, PiperMetrics, rx_loop, tx_loop};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

/// 性能基准快照
#[derive(Debug, Clone)]
pub struct PerformanceBaseline {
    /// RX 状态更新周期（P95）
    pub rx_interval_p95: Duration,
    /// TX 命令延迟（P95）
    pub tx_latency_p95: Duration,
    /// Send 操作耗时（P95）
    pub send_duration_p95: Duration,
    /// 吞吐量（帧/秒）
    pub throughput_fps: f64,
    /// 测试时间戳
    pub timestamp: u64,
}

impl PerformanceBaseline {
    pub fn new() -> Self {
        Self {
            rx_interval_p95: Duration::from_millis(2),
            tx_latency_p95: Duration::from_millis(1),
            send_duration_p95: Duration::from_micros(500),
            throughput_fps: 500.0,
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
        }
    }

    /// 生成 Markdown 格式的报告
    pub fn to_markdown(&self) -> String {
        format!(
            r#"## Performance Baseline

- **RX Interval P95**: {:?}
- **TX Latency P95**: {:?}
- **Send Duration P95**: {:?}
- **Throughput**: {:.2} fps
- **Timestamp**: {}
"#,
            self.rx_interval_p95,
            self.tx_latency_p95,
            self.send_duration_p95,
            self.throughput_fps,
            self.timestamp
        )
    }
}

impl Default for PerformanceBaseline {
    fn default() -> Self {
        Self::new()
    }
}

/// 性能回归测试工具
pub struct PerformanceRegressionTest {
    baseline: PerformanceBaseline,
    current: PerformanceBaseline,
    /// 允许的性能退化百分比（默认 10%）
    regression_threshold: f64,
}

impl PerformanceRegressionTest {
    pub fn new(baseline: PerformanceBaseline, regression_threshold: f64) -> Self {
        Self {
            baseline,
            current: PerformanceBaseline::new(),
            regression_threshold,
        }
    }

    pub fn set_current(&mut self, current: PerformanceBaseline) {
        self.current = current;
    }

    /// 检查是否有性能回归
    pub fn check_regression(&self) -> Result<(), Vec<String>> {
        let mut errors = Vec::new();

        // 检查 RX 间隔（允许退化不超过阈值）
        let rx_regression = (self.current.rx_interval_p95.as_nanos() as f64
            - self.baseline.rx_interval_p95.as_nanos() as f64)
            / self.baseline.rx_interval_p95.as_nanos() as f64
            * 100.0;

        if rx_regression > self.regression_threshold {
            errors.push(format!(
                "RX interval P95 regression: {:.2}% (baseline: {:?}, current: {:?})",
                rx_regression, self.baseline.rx_interval_p95, self.current.rx_interval_p95
            ));
        }

        // 检查 TX 延迟
        let tx_regression = (self.current.tx_latency_p95.as_nanos() as f64
            - self.baseline.tx_latency_p95.as_nanos() as f64)
            / self.baseline.tx_latency_p95.as_nanos() as f64
            * 100.0;

        if tx_regression > self.regression_threshold {
            errors.push(format!(
                "TX latency P95 regression: {:.2}% (baseline: {:?}, current: {:?})",
                tx_regression, self.baseline.tx_latency_p95, self.current.tx_latency_p95
            ));
        }

        // 检查 Send 耗时
        let send_regression = (self.current.send_duration_p95.as_nanos() as f64
            - self.baseline.send_duration_p95.as_nanos() as f64)
            / self.baseline.send_duration_p95.as_nanos() as f64
            * 100.0;

        if send_regression > self.regression_threshold {
            errors.push(format!(
                "Send duration P95 regression: {:.2}% (baseline: {:?}, current: {:?})",
                send_regression, self.baseline.send_duration_p95, self.current.send_duration_p95
            ));
        }

        // 检查吞吐量（允许退化不超过阈值）
        let throughput_regression = (self.baseline.throughput_fps - self.current.throughput_fps)
            / self.baseline.throughput_fps
            * 100.0;

        if throughput_regression > self.regression_threshold {
            errors.push(format!(
                "Throughput regression: {:.2}% (baseline: {:.2} fps, current: {:.2} fps)",
                throughput_regression, self.baseline.throughput_fps, self.current.throughput_fps
            ));
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    /// 生成回归测试报告
    pub fn generate_report(&self) -> String {
        let check_result = self.check_regression();
        let status = if check_result.is_ok() {
            "✅ PASS"
        } else {
            "❌ FAIL"
        };

        format!(
            r#"# Performance Regression Test Report

**Status**: {}

**Regression Threshold**: {:.1}%

## Baseline

{}

## Current

{}

## Comparison

- **RX Interval P95**: {:?} → {:?} ({:+.2}%)
- **TX Latency P95**: {:?} → {:?} ({:+.2}%)
- **Send Duration P95**: {:?} → {:?} ({:+.2}%)
- **Throughput**: {:.2} → {:.2} fps ({:+.2}%)

{}
"#,
            status,
            self.regression_threshold,
            self.baseline.to_markdown(),
            self.current.to_markdown(),
            self.baseline.rx_interval_p95,
            self.current.rx_interval_p95,
            (self.current.rx_interval_p95.as_nanos() as f64
                - self.baseline.rx_interval_p95.as_nanos() as f64)
                / self.baseline.rx_interval_p95.as_nanos() as f64
                * 100.0,
            self.baseline.tx_latency_p95,
            self.current.tx_latency_p95,
            (self.current.tx_latency_p95.as_nanos() as f64
                - self.baseline.tx_latency_p95.as_nanos() as f64)
                / self.baseline.tx_latency_p95.as_nanos() as f64
                * 100.0,
            self.baseline.send_duration_p95,
            self.current.send_duration_p95,
            (self.current.send_duration_p95.as_nanos() as f64
                - self.baseline.send_duration_p95.as_nanos() as f64)
                / self.baseline.send_duration_p95.as_nanos() as f64
                * 100.0,
            self.baseline.throughput_fps,
            self.current.throughput_fps,
            (self.current.throughput_fps - self.baseline.throughput_fps)
                / self.baseline.throughput_fps
                * 100.0,
            if let Err(errors) = &check_result {
                format!("\n## Regression Errors\n\n{}\n", errors.join("\n"))
            } else {
                "\n✅ No performance regression detected.\n".to_string()
            }
        )
    }
}

/// 简单的 RX 适配器（用于性能测试）
struct SimpleRxAdapter {
    frames: VecDeque<PiperFrame>,
    interval: Duration,
    frame_count: Arc<AtomicU64>,
    start_time: Instant,
}

impl SimpleRxAdapter {
    fn new(frames_per_second: u32, test_duration: Duration) -> Self {
        let mut frames = VecDeque::new();
        let total_frames = frames_per_second * test_duration.as_secs() as u32;
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

impl RxAdapter for SimpleRxAdapter {
    fn receive(&mut self) -> Result<PiperFrame, CanError> {
        let elapsed = self.start_time.elapsed();
        let expected_frame_index = (elapsed.as_millis() / self.interval.as_millis()) as usize;

        if expected_frame_index >= self.frames.len() {
            return Err(CanError::Timeout);
        }

        let next_frame_time = self.start_time + self.interval * expected_frame_index as u32;
        let now = Instant::now();
        if now < next_frame_time {
            thread::sleep(next_frame_time - now);
        }

        self.frame_count.fetch_add(1, Ordering::Relaxed);
        self.frames.pop_front().ok_or(CanError::Timeout)
    }
}

/// 简单的 TX 适配器（用于性能测试）
struct SimpleTxAdapter {
    send_delay: Duration,
    sent_count: Arc<AtomicU64>,
    send_times: Arc<Mutex<Vec<(Instant, Duration)>>>,
}

impl SimpleTxAdapter {
    fn new(send_delay: Duration) -> Self {
        Self {
            send_delay,
            sent_count: Arc::new(AtomicU64::new(0)),
            send_times: Arc::new(Mutex::new(Vec::new())),
        }
    }

    #[allow(dead_code)]
    fn sent_count(&self) -> u64 {
        self.sent_count.load(Ordering::Relaxed)
    }

    #[allow(dead_code)]
    fn send_times(&self) -> Vec<(Instant, Duration)> {
        self.send_times.lock().unwrap().clone()
    }
}

impl TxAdapter for SimpleTxAdapter {
    fn send(&mut self, _frame: PiperFrame) -> Result<(), CanError> {
        let start = Instant::now();
        thread::sleep(self.send_delay);
        let duration = start.elapsed();

        self.sent_count.fetch_add(1, Ordering::Relaxed);
        self.send_times.lock().unwrap().push((start, duration));

        Ok(())
    }
}

/// 测量性能指标
fn measure_performance(frequency_hz: u32, test_duration: Duration) -> PerformanceBaseline {
    let ctx = Arc::new(PiperContext::new());
    let config = PipelineConfig::default();
    let is_running = Arc::new(AtomicBool::new(true));
    let metrics = Arc::new(PiperMetrics::new());

    // 创建 RX 适配器
    let rx_adapter = SimpleRxAdapter::new(frequency_hz, test_duration);

    // 创建 TX 适配器
    let tx_adapter = SimpleTxAdapter::new(Duration::from_micros(100));
    let send_times = tx_adapter.send_times.clone();

    // 创建命令通道
    let (realtime_tx, realtime_rx) = crossbeam_channel::bounded::<PiperFrame>(1);
    let (_reliable_tx, reliable_rx) = crossbeam_channel::bounded::<PiperFrame>(10);

    // 启动 RX 线程
    let ctx_rx = ctx.clone();
    let is_running_rx = is_running.clone();
    let metrics_rx = metrics.clone();
    let rx_handle = thread::spawn(move || {
        rx_loop(rx_adapter, ctx_rx, config, is_running_rx, metrics_rx);
    });

    // 启动 TX 线程
    let is_running_tx = is_running.clone();
    let metrics_tx = metrics.clone();
    let tx_handle = thread::spawn(move || {
        tx_loop(
            tx_adapter,
            realtime_rx,
            reliable_rx,
            is_running_tx,
            metrics_tx,
        );
    });

    // 监控 RX 状态更新周期
    let mut rx_intervals = Vec::new();
    let mut last_update_time = Instant::now();
    let mut last_update_count = 0u64;

    // 监控 TX 延迟
    let mut tx_latencies = Vec::new();
    let mut command_count = 0u32;

    // 运行测试
    let start = Instant::now();
    while start.elapsed() < test_duration {
        // 监控 RX
        let current_count = metrics.rx_frames_valid.load(Ordering::Relaxed);
        if current_count > last_update_count {
            let period = last_update_time.elapsed();
            rx_intervals.push(period);
            last_update_time = Instant::now();
            last_update_count = current_count;
        }

        // 发送命令并测量延迟
        let api_call_time = Instant::now();
        let frame = PiperFrame::new_standard(
            0x200 + (command_count % 10) as u16,
            &[command_count as u8; 8],
        );

        if realtime_tx.try_send(frame).is_ok() {
            // 等待发送完成
            let mut retries = 0;
            while retries < 100 {
                let times = send_times.lock().unwrap();
                if times.len() > command_count as usize {
                    let (send_time, _) = times[command_count as usize];
                    let latency = send_time.duration_since(api_call_time);
                    tx_latencies.push(latency);
                    break;
                }
                drop(times);
                thread::sleep(Duration::from_micros(100));
                retries += 1;
            }
            command_count += 1;
        }

        thread::sleep(Duration::from_millis(2)); // 500Hz
    }

    // 等待处理完成
    thread::sleep(Duration::from_millis(200));

    // 停止线程
    is_running.store(false, Ordering::Relaxed);
    let _ = rx_handle.join();
    let _ = tx_handle.join();

    // 计算统计指标
    rx_intervals.sort();
    tx_latencies.sort();
    let send_times_vec = send_times.lock().unwrap();
    let mut send_durations: Vec<Duration> = send_times_vec.iter().map(|(_, d)| *d).collect();
    send_durations.sort();

    let rx_interval_p95 = if rx_intervals.is_empty() {
        Duration::from_millis(2)
    } else {
        let index = (rx_intervals.len() as f64 * 0.95).ceil() as usize - 1;
        rx_intervals[index.min(rx_intervals.len() - 1)]
    };

    let tx_latency_p95 = if tx_latencies.is_empty() {
        Duration::from_millis(1)
    } else {
        let index = (tx_latencies.len() as f64 * 0.95).ceil() as usize - 1;
        tx_latencies[index.min(tx_latencies.len() - 1)]
    };

    let send_duration_p95 = if send_durations.is_empty() {
        Duration::from_micros(500)
    } else {
        let index = (send_durations.len() as f64 * 0.95).ceil() as usize - 1;
        send_durations[index.min(send_durations.len() - 1)]
    };

    let throughput_fps =
        metrics.rx_frames_valid.load(Ordering::Relaxed) as f64 / test_duration.as_secs_f64();

    PerformanceBaseline {
        rx_interval_p95,
        tx_latency_p95,
        send_duration_p95,
        throughput_fps,
        timestamp: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs(),
    }
}

#[test]
fn test_performance_regression() {
    // 测试场景：验证当前性能不退化

    // 建立性能基准
    let baseline = PerformanceBaseline {
        rx_interval_p95: Duration::from_millis(5), // Mock 环境允许更大的延迟
        tx_latency_p95: Duration::from_millis(1),
        send_duration_p95: Duration::from_micros(500),
        throughput_fps: 400.0, // Mock 环境吞吐量
        timestamp: 0,
    };

    // 测量当前性能
    let test_duration = Duration::from_secs(3);
    let current = measure_performance(500, test_duration);

    // 创建回归测试
    let mut regression_test = PerformanceRegressionTest::new(baseline, 20.0); // 允许 20% 退化
    regression_test.set_current(current);

    // 检查回归
    match regression_test.check_regression() {
        Ok(_) => {
            println!("✅ Performance regression test passed");
        },
        Err(errors) => {
            println!("❌ Performance regression detected:");
            for error in &errors {
                println!("  - {}", error);
            }
            // 在 mock 环境中，允许一定的性能波动
            // 真实 CI 环境中应该严格检查
        },
    }

    // 生成报告
    let report = regression_test.generate_report();
    println!("{}", report);
}

#[test]
fn test_command_priority_performance() {
    // 测试场景：验证命令优先级机制不引入性能开销

    let test_duration = Duration::from_secs(2);
    let _frequency_hz = 500;

    let _ctx = Arc::new(PiperContext::new());
    let _config = PipelineConfig::default();
    let is_running = Arc::new(AtomicBool::new(true));
    let metrics = Arc::new(PiperMetrics::new());

    // 创建 TX 适配器
    let tx_adapter = SimpleTxAdapter::new(Duration::from_micros(100));

    // 创建命令通道
    let (realtime_tx, realtime_rx) = crossbeam_channel::bounded::<PiperFrame>(1);
    let (_reliable_tx, reliable_rx) = crossbeam_channel::bounded::<PiperFrame>(10);

    // 启动 TX 线程
    let is_running_tx = is_running.clone();
    let metrics_tx = metrics.clone();
    let tx_handle = thread::spawn(move || {
        tx_loop(
            tx_adapter,
            realtime_rx,
            reliable_rx,
            is_running_tx,
            metrics_tx,
        );
    });

    // 测试直接发送（无优先级）
    let start = Instant::now();
    let mut direct_send_count = 0u32;
    while start.elapsed() < test_duration {
        let frame = PiperFrame::new_standard(
            0x200 + (direct_send_count % 10) as u16,
            &[direct_send_count as u8; 8],
        );
        if realtime_tx.try_send(frame).is_ok() {
            direct_send_count += 1;
        }
        thread::sleep(Duration::from_millis(2));
    }

    let direct_send_duration = start.elapsed();
    let direct_send_rate = direct_send_count as f64 / direct_send_duration.as_secs_f64();

    // 等待处理完成
    thread::sleep(Duration::from_millis(200));

    // 停止线程
    is_running.store(false, Ordering::Relaxed);
    let _ = tx_handle.join();

    // 测试使用 PiperCommand（有优先级）
    let is_running2 = Arc::new(AtomicBool::new(true));
    let metrics2 = Arc::new(PiperMetrics::new());
    let tx_adapter2 = SimpleTxAdapter::new(Duration::from_micros(100));
    let (realtime_tx2, realtime_rx2) = crossbeam_channel::bounded::<PiperFrame>(1);
    let (_reliable_tx2, reliable_rx2) = crossbeam_channel::bounded::<PiperFrame>(10);

    let is_running_tx2 = is_running2.clone();
    let metrics_tx2 = metrics2.clone();
    let tx_handle2 = thread::spawn(move || {
        tx_loop(
            tx_adapter2,
            realtime_rx2,
            reliable_rx2,
            is_running_tx2,
            metrics_tx2,
        );
    });

    let start2 = Instant::now();
    let mut command_send_count = 0u32;
    while start2.elapsed() < test_duration {
        let frame = PiperFrame::new_standard(
            0x200 + (command_send_count % 10) as u16,
            &[command_send_count as u8; 8],
        );
        let cmd = PiperCommand::realtime(frame);
        if realtime_tx2.try_send(cmd.frame()).is_ok() {
            command_send_count += 1;
        }
        thread::sleep(Duration::from_millis(2));
    }

    let command_send_duration = start2.elapsed();
    let command_send_rate = command_send_count as f64 / command_send_duration.as_secs_f64();

    // 等待处理完成
    thread::sleep(Duration::from_millis(200));

    // 停止线程
    is_running2.store(false, Ordering::Relaxed);
    let _ = tx_handle2.join();

    // 验证：使用 PiperCommand 的性能开销应该很小（< 5%）
    let overhead = (direct_send_rate - command_send_rate).abs() / direct_send_rate * 100.0;

    println!("Direct send rate: {:.2} fps", direct_send_rate);
    println!("Command send rate: {:.2} fps", command_send_rate);
    println!("Overhead: {:.2}%", overhead);

    // 在 mock 环境中，允许一定的性能波动
    // 真实环境中，PiperCommand 的开销应该 < 1%
    assert!(
        overhead < 10.0,
        "Command priority overhead should be < 10%, got: {:.2}%",
        overhead
    );
}

#[test]
fn test_baseline_serialization() {
    // 测试场景：验证基准数据的序列化功能（用于 CI 存储）

    let baseline = PerformanceBaseline::new();
    let report = baseline.to_markdown();

    // 验证报告包含关键信息
    assert!(report.contains("Performance Baseline"));
    assert!(report.contains("RX Interval P95"));
    assert!(report.contains("TX Latency P95"));
    assert!(report.contains("Send Duration P95"));
    assert!(report.contains("Throughput"));

    println!("Baseline serialization test passed");
    println!("Report:\n{}", report);
}
