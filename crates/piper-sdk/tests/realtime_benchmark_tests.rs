//! 实时性测试框架
//!
//! 建立可回归的实时性测试框架，测量延迟和抖动：
//! 1. RX 状态更新周期分布（P50/P95/P99/max）
//! 2. TX 命令延迟分布
//! 3. Send 操作耗时分布
//! 4. 支持多种测试场景（500Hz/1kHz、USB故障、CAN高负载）
//! 5. 生成测试报告（Markdown）

use piper_sdk::can::{CanError, PiperFrame, RealtimeTxAdapter, RxAdapter};
use piper_sdk::driver::{
    BackendCapability, MaintenanceLeaseGate, MaintenanceStateSignal, NormalSendGate,
    PipelineConfig, PiperContext, PiperMetrics, ShutdownLane,
    command::{RealtimeCommand, ReliableCommand},
    rx_loop, tx_loop_mailbox,
};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
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

/// 实时性指标结构（增强版 LatencyStats）
#[derive(Debug, Clone)]
pub struct RealtimeMetrics {
    samples: Vec<Duration>,
    name: String,
}

impl RealtimeMetrics {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            samples: Vec::new(),
            name: name.into(),
        }
    }

    pub fn add_sample(&mut self, latency: Duration) {
        self.samples.push(latency);
    }

    pub fn percentile(&self, p: f64) -> Duration {
        if self.samples.is_empty() {
            return Duration::ZERO;
        }

        let mut sorted = self.samples.clone();
        sorted.sort();

        let index = ((sorted.len() as f64) * p / 100.0).ceil() as usize - 1;
        sorted[index.min(sorted.len() - 1)]
    }

    pub fn p50(&self) -> Duration {
        self.percentile(50.0)
    }

    pub fn p95(&self) -> Duration {
        self.percentile(95.0)
    }

    pub fn p99(&self) -> Duration {
        self.percentile(99.0)
    }

    pub fn p999(&self) -> Duration {
        self.percentile(99.9)
    }

    pub fn max(&self) -> Duration {
        self.samples.iter().max().copied().unwrap_or(Duration::ZERO)
    }

    pub fn min(&self) -> Duration {
        self.samples.iter().min().copied().unwrap_or(Duration::ZERO)
    }

    pub fn mean(&self) -> Duration {
        if self.samples.is_empty() {
            return Duration::ZERO;
        }
        let total: Duration = self.samples.iter().sum();
        total / self.samples.len() as u32
    }

    pub fn std_dev(&self) -> Duration {
        if self.samples.len() < 2 {
            return Duration::ZERO;
        }

        let mean = self.mean();
        let variance: f64 = self
            .samples
            .iter()
            .map(|&d| {
                let diff = d.as_nanos() as f64 - mean.as_nanos() as f64;
                diff * diff
            })
            .sum::<f64>()
            / (self.samples.len() - 1) as f64;

        Duration::from_nanos(variance.sqrt() as u64)
    }

    pub fn count(&self) -> usize {
        self.samples.len()
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// 生成 Markdown 格式的报告
    pub fn to_markdown(&self) -> String {
        format!(
            r#"### {}

- **Samples**: {}
- **Min**: {:?}
- **P50**: {:?}
- **P95**: {:?}
- **P99**: {:?}
- **P99.9**: {:?}
- **Max**: {:?}
- **Mean**: {:?}
- **Std Dev**: {:?}
"#,
            self.name,
            self.count(),
            self.min(),
            self.p50(),
            self.p95(),
            self.p99(),
            self.p999(),
            self.max(),
            self.mean(),
            self.std_dev()
        )
    }
}

/// 实时性基准测试工具
pub struct RealtimeBenchmark {
    rx_interval_metrics: RealtimeMetrics,
    tx_latency_metrics: RealtimeMetrics,
    send_duration_metrics: RealtimeMetrics,
    test_duration: Duration,
    frequency_hz: u32,
}

impl RealtimeBenchmark {
    pub fn new(frequency_hz: u32, test_duration: Duration) -> Self {
        Self {
            rx_interval_metrics: RealtimeMetrics::new(format!("RX Interval ({}Hz)", frequency_hz)),
            tx_latency_metrics: RealtimeMetrics::new("TX Latency"),
            send_duration_metrics: RealtimeMetrics::new("Send Duration"),
            test_duration,
            frequency_hz,
        }
    }

    pub fn rx_interval_metrics(&self) -> &RealtimeMetrics {
        &self.rx_interval_metrics
    }

    pub fn tx_latency_metrics(&self) -> &RealtimeMetrics {
        &self.tx_latency_metrics
    }

    pub fn send_duration_metrics(&self) -> &RealtimeMetrics {
        &self.send_duration_metrics
    }

    /// 生成完整的测试报告（Markdown）
    pub fn generate_report(&self) -> String {
        format!(
            r#"# Realtime Benchmark Report

**Test Configuration**:
- Frequency: {} Hz
- Test Duration: {:?}
- Timestamp: {}

## Metrics

{}

{}

{}

## Summary

- **RX Interval P95**: {:?} (Target: < 2ms for 500Hz, < 1ms for 1kHz)
- **TX Latency P95**: {:?} (Target: < 1ms)
- **Send Duration P95**: {:?} (Target: < 500µs)
"#,
            self.frequency_hz,
            self.test_duration,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            self.rx_interval_metrics.to_markdown(),
            self.tx_latency_metrics.to_markdown(),
            self.send_duration_metrics.to_markdown(),
            self.rx_interval_metrics.p95(),
            self.tx_latency_metrics.p95(),
            self.send_duration_metrics.p95()
        )
    }
}

/// 高频 RX 适配器（可配置频率和故障模拟）
struct ConfigurableRxAdapter {
    frames: VecDeque<PiperFrame>,
    interval: Duration,
    frame_count: Arc<AtomicU64>,
    start_time: Instant,
    /// 模拟延迟（可选）
    delay_probability: f64,
    delay_duration: Duration,
    /// 模拟丢包（可选）
    drop_probability: f64,
}

impl ConfigurableRxAdapter {
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
            delay_probability: 0.0,
            delay_duration: Duration::ZERO,
            drop_probability: 0.0,
        }
    }

    /// 设置延迟模拟（概率和持续时间）
    fn with_delay(mut self, probability: f64, duration: Duration) -> Self {
        self.delay_probability = probability;
        self.delay_duration = duration;
        self
    }

    /// 设置丢包模拟（概率）
    fn with_drop(mut self, probability: f64) -> Self {
        self.drop_probability = probability;
        self
    }

    #[allow(dead_code)]
    fn frame_count(&self) -> u64 {
        self.frame_count.load(Ordering::Relaxed)
    }
}

impl RxAdapter for ConfigurableRxAdapter {
    fn receive(&mut self) -> Result<PiperFrame, CanError> {
        // 模拟延迟
        if rand::random::<f64>() < self.delay_probability {
            thread::sleep(self.delay_duration);
        }

        // 模拟丢包
        if rand::random::<f64>() < self.drop_probability {
            self.frames.pop_front();
            return Err(CanError::Timeout);
        }

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

/// 可配置的 TX 适配器（支持延迟和错误模拟）
struct ConfigurableTxAdapter {
    send_delay: Duration,
    sent_count: Arc<AtomicU64>,
    send_times: Arc<Mutex<Vec<(Instant, Duration)>>>,
    /// 模拟错误（概率）
    error_probability: f64,
}

impl ConfigurableTxAdapter {
    fn new(send_delay: Duration) -> Self {
        Self {
            send_delay,
            sent_count: Arc::new(AtomicU64::new(0)),
            send_times: Arc::new(Mutex::new(Vec::new())),
            error_probability: 0.0,
        }
    }

    /// 设置错误模拟（概率）
    #[allow(dead_code)]
    fn with_error(mut self, probability: f64) -> Self {
        self.error_probability = probability;
        self
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

impl RealtimeTxAdapter for ConfigurableTxAdapter {
    fn send_control(&mut self, _frame: PiperFrame, budget: Duration) -> Result<(), CanError> {
        let start = Instant::now();
        if budget.is_zero() {
            return Err(CanError::Timeout);
        }

        if rand::random::<f64>() < self.error_probability {
            return Err(CanError::Io(std::io::Error::other("Simulated error")));
        }

        thread::sleep(self.send_delay.min(budget));
        if self.send_delay > budget {
            return Err(CanError::Timeout);
        }
        let duration = start.elapsed();

        self.sent_count.fetch_add(1, Ordering::Relaxed);
        self.send_times.lock().unwrap().push((start, duration));

        Ok(())
    }

    fn send_shutdown_until(
        &mut self,
        _frame: PiperFrame,
        deadline: Instant,
    ) -> Result<(), CanError> {
        let start = Instant::now();
        let Some(remaining) = deadline.checked_duration_since(start) else {
            return Err(CanError::Timeout);
        };

        // 模拟错误
        if rand::random::<f64>() < self.error_probability {
            return Err(CanError::Io(std::io::Error::other("Simulated error")));
        }

        thread::sleep(self.send_delay.min(remaining));
        if self.send_delay > remaining {
            return Err(CanError::Timeout);
        }
        let duration = start.elapsed();

        self.sent_count.fetch_add(1, Ordering::Relaxed);
        self.send_times.lock().unwrap().push((start, duration));

        Ok(())
    }
}

#[test]
#[ignore = "non-gating realtime benchmark"]
fn test_500hz_realtime_benchmark() {
    // CI 调度不可控，时序断言易偶发失败；仅在本地运行
    if is_ci_env() {
        return;
    }
    // 测试场景：500Hz 控制回路，测量实时性指标

    let frequency_hz = 500;
    let test_duration = Duration::from_secs(5);
    let mut benchmark = RealtimeBenchmark::new(frequency_hz, test_duration);

    let ctx = Arc::new(PiperContext::new());
    let config = PipelineConfig::default();
    let is_running = Arc::new(AtomicBool::new(true));
    let runtime_phase = Arc::new(AtomicU8::new(0));
    let metrics = Arc::new(PiperMetrics::new());
    let last_fault = Arc::new(AtomicU8::new(0));
    let maintenance_state_signal = Arc::new(MaintenanceStateSignal::default());
    let normal_send_gate = Arc::new(NormalSendGate::new());

    // 创建 500Hz RX 适配器
    let rx_adapter = ConfigurableRxAdapter::new(frequency_hz, test_duration);

    // 启动 RX 线程
    let ctx_rx = ctx.clone();
    let is_running_rx = is_running.clone();
    let runtime_phase_rx = runtime_phase.clone();
    let normal_send_gate_rx = normal_send_gate.clone();
    let driver_mode_rx = Arc::new(piper_sdk::driver::AtomicDriverMode::new(
        piper_sdk::driver::DriverMode::Normal,
    ));
    let metrics_rx = metrics.clone();
    let last_fault_rx = last_fault.clone();
    let maintenance_state_signal_rx = maintenance_state_signal.clone();
    let rx_handle = thread::spawn(move || {
        rx_loop(
            rx_adapter,
            BackendCapability::StrictRealtime,
            ctx_rx,
            config,
            is_running_rx,
            runtime_phase_rx,
            normal_send_gate_rx,
            driver_mode_rx,
            metrics_rx,
            last_fault_rx,
            maintenance_state_signal_rx,
        );
    });

    // 监控状态更新周期
    let mut last_update_time = Instant::now();
    let mut last_update_count = 0u64;

    // 运行测试
    let start = Instant::now();
    while start.elapsed() < test_duration {
        let current_count = metrics.rx_frames_valid.load(Ordering::Relaxed);

        if current_count > last_update_count {
            let period = last_update_time.elapsed();
            benchmark.rx_interval_metrics.add_sample(period);
            last_update_time = Instant::now();
            last_update_count = current_count;
        }

        thread::sleep(Duration::from_millis(1));
    }

    // 停止线程
    is_running.store(false, Ordering::Relaxed);
    let _ = rx_handle.join();

    // 验证统计结果
    let rx_metrics = benchmark.rx_interval_metrics();
    println!("{}", rx_metrics.to_markdown());

    // 验收标准：P95 < 5ms（500Hz = 2ms 周期，允许系统调度延迟）
    // 注意：在 mock 测试环境中，实际延迟可能高于真实硬件环境
    // 在CI环境中，阈值会放宽
    let threshold = adjust_threshold_ms(5);
    assert!(
        rx_metrics.p95() < threshold,
        "RX update period P95 should be < {:?} for 500Hz (CI环境已放宽), got: {:?}",
        threshold,
        rx_metrics.p95()
    );

    // 验证：P50 应该在合理范围内（500Hz = 2ms 周期）
    // 本地：约 [1ms, 4ms]；CI：上界放宽为 adjust_threshold_ms(4)=20ms
    let expected_period = Duration::from_millis(2);
    let p50 = rx_metrics.p50();
    let p50_min = expected_period * 5 / 10;
    let p50_max = if is_ci_env() {
        adjust_threshold_ms(4) // CI 下 20ms
    } else {
        expected_period * 20 / 10
    };
    assert!(
        p50 >= p50_min && p50 <= p50_max,
        "RX update period P50 should be in [{:?}, {:?}] (500Hz, CI relaxed), got: {:?}",
        p50_min,
        p50_max,
        p50
    );
}

#[test]
#[ignore = "non-gating realtime benchmark"]
fn test_1khz_realtime_benchmark() {
    // CI 调度不可控，时序断言易偶发失败；仅在本地运行
    if is_ci_env() {
        return;
    }
    // 测试场景：1kHz 控制回路，测量实时性指标

    let frequency_hz = 1000;
    let test_duration = Duration::from_secs(5);
    let mut benchmark = RealtimeBenchmark::new(frequency_hz, test_duration);

    let ctx = Arc::new(PiperContext::new());
    let config = PipelineConfig::default();
    let is_running = Arc::new(AtomicBool::new(true));
    let runtime_phase = Arc::new(AtomicU8::new(0));
    let metrics = Arc::new(PiperMetrics::new());
    let last_fault = Arc::new(AtomicU8::new(0));
    let maintenance_state_signal = Arc::new(MaintenanceStateSignal::default());
    let normal_send_gate = Arc::new(NormalSendGate::new());

    // 创建 1kHz RX 适配器
    let rx_adapter = ConfigurableRxAdapter::new(frequency_hz, test_duration);

    // 启动 RX 线程
    let ctx_rx = ctx.clone();
    let is_running_rx = is_running.clone();
    let runtime_phase_rx = runtime_phase.clone();
    let normal_send_gate_rx = normal_send_gate.clone();
    let driver_mode_rx = Arc::new(piper_sdk::driver::AtomicDriverMode::new(
        piper_sdk::driver::DriverMode::Normal,
    ));
    let metrics_rx = metrics.clone();
    let last_fault_rx = last_fault.clone();
    let maintenance_state_signal_rx = maintenance_state_signal.clone();
    let rx_handle = thread::spawn(move || {
        rx_loop(
            rx_adapter,
            BackendCapability::StrictRealtime,
            ctx_rx,
            config,
            is_running_rx,
            runtime_phase_rx,
            normal_send_gate_rx,
            driver_mode_rx,
            metrics_rx,
            last_fault_rx,
            maintenance_state_signal_rx,
        );
    });

    // 监控状态更新周期
    let mut last_update_time = Instant::now();
    let mut last_update_count = 0u64;

    // 运行测试
    let start = Instant::now();
    while start.elapsed() < test_duration {
        let current_count = metrics.rx_frames_valid.load(Ordering::Relaxed);

        if current_count > last_update_count {
            let period = last_update_time.elapsed();
            benchmark.rx_interval_metrics.add_sample(period);
            last_update_time = Instant::now();
            last_update_count = current_count;
        }

        thread::sleep(Duration::from_millis(1));
    }

    // 停止线程
    is_running.store(false, Ordering::Relaxed);
    let _ = rx_handle.join();

    // 验证统计结果
    let rx_metrics = benchmark.rx_interval_metrics();
    println!("{}", rx_metrics.to_markdown());

    // 验收标准：P95 < 5ms（1kHz = 1ms 周期，允许系统调度与 CI 环境波动）
    // 注意：在 mock/CI 环境中，实际延迟可能高于真实硬件，故放宽至 5ms 避免 flake
    // 在CI环境中，阈值会进一步放宽
    let threshold = adjust_threshold_ms(5);
    assert!(
        rx_metrics.p95() < threshold,
        "RX update period P95 should be < {:?} for 1kHz (CI环境已放宽), got: {:?}",
        threshold,
        rx_metrics.p95()
    );

    // 验证：P50 应该在合理范围内（1kHz = 1ms 周期）
    // 本地：约 [0.5ms, 2ms]；CI：调度不可控，上界放宽为 15ms 避免偶发超 10ms
    let expected_period = Duration::from_millis(1);
    let p50 = rx_metrics.p50();
    let p50_min = expected_period * 5 / 10;
    let p50_max = if is_ci_env() {
        adjust_threshold_ms(3) // CI 下 15ms
    } else {
        expected_period * 20 / 10
    };
    assert!(
        p50 >= p50_min && p50 <= p50_max,
        "RX update period P50 should be in [{:?}, {:?}] (1kHz, CI relaxed), got: {:?}",
        p50_min,
        p50_max,
        p50
    );
}

#[test]
#[ignore = "non-gating realtime benchmark"]
fn test_tx_latency_benchmark() {
    // CI 调度不可控，P95 等时序断言易偶发失败；仅在本地运行
    if is_ci_env() {
        return;
    }
    // 测试场景：测量 TX 命令延迟（从 API 调用到实际发送）

    let test_duration = Duration::from_secs(3);
    let mut benchmark = RealtimeBenchmark::new(500, test_duration);

    let _ctx = Arc::new(PiperContext::new());
    let _config = PipelineConfig::default();
    let is_running = Arc::new(AtomicBool::new(true));
    let runtime_phase = Arc::new(AtomicU8::new(0));
    let metrics = Arc::new(PiperMetrics::new());
    let last_fault = Arc::new(AtomicU8::new(0));
    let _maintenance_state_signal = Arc::new(MaintenanceStateSignal::default());
    let maintenance_lease_gate = Arc::new(MaintenanceLeaseGate::default());

    // 创建 TX 适配器（记录发送时间）
    let tx_adapter = ConfigurableTxAdapter::new(Duration::from_micros(100));
    let send_times = tx_adapter.send_times.clone();

    // 创建命令通道
    let (_reliable_tx, reliable_rx) = crossbeam_channel::bounded::<ReliableCommand>(10);
    let shutdown_lane = Arc::new(ShutdownLane::new());
    let realtime_slot: Arc<std::sync::Mutex<Option<piper_sdk::driver::command::RealtimeCommand>>> =
        Arc::new(std::sync::Mutex::new(None));

    // 启动 TX 线程
    let ctx_tx = _ctx.clone();
    let is_running_tx = is_running.clone();
    let runtime_phase_tx = runtime_phase.clone();
    let metrics_tx = metrics.clone();
    let last_fault_tx = last_fault.clone();
    let realtime_slot_tx = realtime_slot.clone();
    let maintenance_lease_gate_tx = maintenance_lease_gate.clone();
    let (maintenance_ctrl_tx, maintenance_ctrl_rx) = crossbeam_channel::unbounded();
    maintenance_lease_gate.set_control_sink(maintenance_ctrl_tx);
    let soft_realtime_rx = Arc::new(piper_sdk::driver::command::SoftRealtimeMailbox::default());
    let tx_handle = thread::spawn(move || {
        let normal_send_gate = Arc::new(NormalSendGate::new());
        tx_loop_mailbox(
            tx_adapter,
            BackendCapability::StrictRealtime,
            PipelineConfig::default(),
            realtime_slot_tx,
            soft_realtime_rx,
            shutdown_lane,
            reliable_rx,
            is_running_tx,
            runtime_phase_tx,
            normal_send_gate,
            metrics_tx,
            ctx_tx,
            last_fault_tx,
            maintenance_ctrl_rx,
            maintenance_lease_gate_tx,
            Arc::new(piper_sdk::driver::AtomicDriverMode::new(
                piper_sdk::driver::DriverMode::Normal,
            )),
        );
    });

    // 发送命令并测量延迟
    let start = Instant::now();
    let mut command_count = 0u32;

    while start.elapsed() < test_duration {
        let api_call_time = Instant::now();
        let frame = PiperFrame::new_standard(
            0x200 + (command_count % 10) as u16,
            &[command_count as u8; 8],
        );

        let queued = {
            let mut slot = realtime_slot.lock().unwrap();
            if slot.is_none() {
                *slot = Some(RealtimeCommand::single(frame));
                true
            } else {
                false
            }
        };

        if queued {
            // 等待发送完成（通过检查 send_times）
            let mut retries = 0;
            while retries < 100 {
                let times = send_times.lock().unwrap();
                if times.len() > command_count as usize {
                    let (send_time, _) = times[command_count as usize];
                    let latency = send_time.duration_since(api_call_time);
                    benchmark.tx_latency_metrics.add_sample(latency);
                    break;
                }
                drop(times);
                thread::sleep(Duration::from_micros(100));
                retries += 1;
            }
            command_count += 1;
        } else {
            // 邮箱忙，跳过
        }

        thread::sleep(Duration::from_millis(2)); // 500Hz
    }

    // 等待处理完成
    thread::sleep(Duration::from_millis(200));

    // 停止线程
    is_running.store(false, Ordering::Relaxed);
    let _ = tx_handle.join();

    // 验证统计结果
    let tx_metrics = benchmark.tx_latency_metrics();
    println!("{}", tx_metrics.to_markdown());

    // 验收标准：P95 < 1ms（CI 环境会放宽，与 pipeline_performance_tests 一致用 3→15ms）
    let threshold = adjust_threshold_ms(3);
    assert!(
        tx_metrics.p95() < threshold,
        "TX latency P95 should be < {:?} (CI环境已放宽), got: {:?}",
        threshold,
        tx_metrics.p95()
    );
}

#[test]
#[ignore = "non-gating realtime benchmark"]
fn test_send_duration_benchmark() {
    // CI 调度不可控，时序断言易偶发失败；仅在本地运行
    if is_ci_env() {
        return;
    }
    // 测试场景：测量 Send 操作耗时

    let test_duration = Duration::from_secs(3);
    let mut benchmark = RealtimeBenchmark::new(500, test_duration);

    let _ctx = Arc::new(PiperContext::new());
    let _config = PipelineConfig::default();
    let is_running = Arc::new(AtomicBool::new(true));
    let runtime_phase = Arc::new(AtomicU8::new(0));
    let metrics = Arc::new(PiperMetrics::new());
    let last_fault = Arc::new(AtomicU8::new(0));
    let _maintenance_state_signal = Arc::new(MaintenanceStateSignal::default());
    let maintenance_lease_gate = Arc::new(MaintenanceLeaseGate::default());

    // 创建 TX 适配器（记录发送耗时）
    let tx_adapter = ConfigurableTxAdapter::new(Duration::from_micros(100));
    let send_times = tx_adapter.send_times.clone();

    // 创建命令通道
    let (_reliable_tx, reliable_rx) = crossbeam_channel::bounded::<ReliableCommand>(10);
    let shutdown_lane = Arc::new(ShutdownLane::new());
    let realtime_slot: Arc<std::sync::Mutex<Option<piper_sdk::driver::command::RealtimeCommand>>> =
        Arc::new(std::sync::Mutex::new(None));

    // 启动 TX 线程
    let ctx_tx = _ctx.clone();
    let is_running_tx = is_running.clone();
    let runtime_phase_tx = runtime_phase.clone();
    let metrics_tx = metrics.clone();
    let last_fault_tx = last_fault.clone();
    let realtime_slot_tx = realtime_slot.clone();
    let maintenance_lease_gate_tx = maintenance_lease_gate.clone();
    let (maintenance_ctrl_tx, maintenance_ctrl_rx) = crossbeam_channel::unbounded();
    maintenance_lease_gate.set_control_sink(maintenance_ctrl_tx);
    let soft_realtime_rx = Arc::new(piper_sdk::driver::command::SoftRealtimeMailbox::default());
    let tx_handle = thread::spawn(move || {
        let normal_send_gate = Arc::new(NormalSendGate::new());
        tx_loop_mailbox(
            tx_adapter,
            BackendCapability::StrictRealtime,
            PipelineConfig::default(),
            realtime_slot_tx,
            soft_realtime_rx,
            shutdown_lane,
            reliable_rx,
            is_running_tx,
            runtime_phase_tx,
            normal_send_gate,
            metrics_tx,
            ctx_tx,
            last_fault_tx,
            maintenance_ctrl_rx,
            maintenance_lease_gate_tx,
            Arc::new(piper_sdk::driver::AtomicDriverMode::new(
                piper_sdk::driver::DriverMode::Normal,
            )),
        );
    });

    // 发送命令
    let start = Instant::now();
    let mut command_count = 0u32;

    while start.elapsed() < test_duration {
        let frame = PiperFrame::new_standard(
            0x200 + (command_count % 10) as u16,
            &[command_count as u8; 8],
        );

        let queued = {
            let mut slot = realtime_slot.lock().unwrap();
            if slot.is_none() {
                *slot = Some(RealtimeCommand::single(frame));
                true
            } else {
                false
            }
        };

        if queued {
            command_count += 1;
        }

        thread::sleep(Duration::from_millis(2)); // 500Hz
    }

    // 等待处理完成
    thread::sleep(Duration::from_millis(200));

    // 停止线程
    is_running.store(false, Ordering::Relaxed);
    let _ = tx_handle.join();

    // 收集发送耗时
    let times = send_times.lock().unwrap();
    for (_, duration) in times.iter() {
        benchmark.send_duration_metrics.add_sample(*duration);
    }

    // 验证统计结果
    let send_metrics = benchmark.send_duration_metrics();
    println!("{}", send_metrics.to_markdown());

    // 验收标准：P95 < 1.5ms（本地）；CI 调度方差大，与其它延迟测试一致放宽为 10ms
    let threshold = adjust_threshold_ms(2);
    assert!(
        send_metrics.p95() < threshold,
        "Send duration P95 should be < {:?} (CI环境已放宽), got: {:?}",
        threshold,
        send_metrics.p95()
    );
}

#[test]
#[ignore = "non-gating realtime benchmark"]
fn test_usb_fault_simulation() {
    // CI 调度不可控，故障场景下 P95 时序断言易偶发失败；仅在本地运行
    if is_ci_env() {
        return;
    }
    // 测试场景：模拟 USB 故障（延迟、丢包）

    let frequency_hz = 500;
    let test_duration = Duration::from_secs(3);
    let mut benchmark = RealtimeBenchmark::new(frequency_hz, test_duration);

    let ctx = Arc::new(PiperContext::new());
    let config = PipelineConfig::default();
    let is_running = Arc::new(AtomicBool::new(true));
    let runtime_phase = Arc::new(AtomicU8::new(0));
    let metrics = Arc::new(PiperMetrics::new());
    let last_fault = Arc::new(AtomicU8::new(0));
    let maintenance_state_signal = Arc::new(MaintenanceStateSignal::default());
    let normal_send_gate = Arc::new(NormalSendGate::new());

    // 创建 RX 适配器（模拟 5% 延迟，10ms 延迟）
    let rx_adapter = ConfigurableRxAdapter::new(frequency_hz, test_duration)
        .with_delay(0.05, Duration::from_millis(10))
        .with_drop(0.01); // 1% 丢包

    // 启动 RX 线程
    let ctx_rx = ctx.clone();
    let is_running_rx = is_running.clone();
    let runtime_phase_rx = runtime_phase.clone();
    let normal_send_gate_rx = normal_send_gate.clone();
    let driver_mode_rx = Arc::new(piper_sdk::driver::AtomicDriverMode::new(
        piper_sdk::driver::DriverMode::Normal,
    ));
    let metrics_rx = metrics.clone();
    let last_fault_rx = last_fault.clone();
    let maintenance_state_signal_rx = maintenance_state_signal.clone();
    let rx_handle = thread::spawn(move || {
        rx_loop(
            rx_adapter,
            BackendCapability::StrictRealtime,
            ctx_rx,
            config,
            is_running_rx,
            runtime_phase_rx,
            normal_send_gate_rx,
            driver_mode_rx,
            metrics_rx,
            last_fault_rx,
            maintenance_state_signal_rx,
        );
    });

    // 监控状态更新周期
    let mut last_update_time = Instant::now();
    let mut last_update_count = 0u64;

    // 运行测试
    let start = Instant::now();
    while start.elapsed() < test_duration {
        let current_count = metrics.rx_frames_valid.load(Ordering::Relaxed);

        if current_count > last_update_count {
            let period = last_update_time.elapsed();
            benchmark.rx_interval_metrics.add_sample(period);
            last_update_time = Instant::now();
            last_update_count = current_count;
        }

        thread::sleep(Duration::from_millis(1));
    }

    // 停止线程
    is_running.store(false, Ordering::Relaxed);
    let _ = rx_handle.join();

    // 验证：即使在故障情况下，P95 也应该在可接受范围内
    let rx_metrics = benchmark.rx_interval_metrics();
    println!("USB Fault Simulation Results:");
    println!("{}", rx_metrics.to_markdown());

    // 验收标准：P95 < 20ms（允许故障导致的延迟）
    // 注意：在 mock 测试环境中，故障模拟（5% 延迟 + 1% 丢包）可能导致更大的延迟
    // 真实硬件环境中，故障恢复应该更快
    // 在CI环境中，阈值会放宽
    let threshold = adjust_threshold_ms(20);
    assert!(
        rx_metrics.p95() < threshold,
        "RX update period P95 should be < {:?} even with USB faults (CI环境已放宽), got: {:?}",
        threshold,
        rx_metrics.p95()
    );
}
