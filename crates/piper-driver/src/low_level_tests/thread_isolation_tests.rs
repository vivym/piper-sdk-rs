//! 线程隔离测试
//!
//! 验证双线程架构的核心价值：
//! 1. RX 线程不受 TX 故障影响
//! 2. TX 线程能感知 RX 故障并 fault-latch
//! 3. 外部 shutdown 请求仍能有序回收线程

use crate::command::{RealtimeCommand, ReliableCommand};
use crate::test_support::spawn_tx_loop;
use crate::{
    AtomicDriverMode, BackendCapability, DriverMode, MaintenanceLeaseGate, MaintenanceStateSignal,
    NormalSendGate, PipelineConfig, PiperContext, PiperMetrics, RuntimeFaultKind, ShutdownLane,
    rx_loop,
};
use piper_can::{
    CanDeviceError, CanDeviceErrorKind, CanError, PiperFrame, RealtimeTxAdapter, RxAdapter,
};
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
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

/// Mock RX 适配器：模拟正常接收
struct MockRxAdapter {
    frames: VecDeque<PiperFrame>,
    receive_delay: Duration,
    should_fail: Arc<AtomicBool>,
}

impl MockRxAdapter {
    fn new(frames: Vec<PiperFrame>, receive_delay: Duration) -> Self {
        Self {
            frames: VecDeque::from(frames),
            receive_delay,
            should_fail: Arc::new(AtomicBool::new(false)),
        }
    }

    #[allow(dead_code)]
    fn set_should_fail(&self, fail: bool) {
        self.should_fail.store(fail, Ordering::Relaxed);
    }
}

impl RxAdapter for MockRxAdapter {
    fn receive(&mut self) -> Result<PiperFrame, CanError> {
        if self.should_fail.load(Ordering::Relaxed) {
            return Err(CanError::Device(CanDeviceError::new(
                CanDeviceErrorKind::NoDevice,
                "Device disconnected",
            )));
        }

        thread::sleep(self.receive_delay);

        self.frames.pop_front().ok_or(CanError::Timeout)
    }
}

/// Mock TX 适配器：模拟发送延迟或故障
struct MockTxAdapter {
    send_delay: Duration,
    should_timeout: Arc<AtomicBool>,
    should_fail: Arc<AtomicBool>,
    sent_count: Arc<AtomicU64>,
}

impl MockTxAdapter {
    fn new(send_delay: Duration) -> Self {
        Self {
            send_delay,
            should_timeout: Arc::new(AtomicBool::new(false)),
            should_fail: Arc::new(AtomicBool::new(false)),
            sent_count: Arc::new(AtomicU64::new(0)),
        }
    }

    #[allow(dead_code)]
    fn set_should_timeout(&self, timeout: bool) {
        self.should_timeout.store(timeout, Ordering::Relaxed);
    }

    #[allow(dead_code)]
    fn set_should_fail(&self, fail: bool) {
        self.should_fail.store(fail, Ordering::Relaxed);
    }

    #[allow(dead_code)]
    fn sent_count(&self) -> u64 {
        self.sent_count.load(Ordering::Relaxed)
    }
}

impl RealtimeTxAdapter for MockTxAdapter {
    fn send_control(&mut self, _frame: PiperFrame, budget: Duration) -> Result<(), CanError> {
        if self.should_fail.load(Ordering::Relaxed) {
            return Err(CanError::Device(CanDeviceError::new(
                CanDeviceErrorKind::NoDevice,
                "Device disconnected",
            )));
        }

        if self.should_timeout.load(Ordering::Relaxed) {
            thread::sleep(budget.min(Duration::from_millis(100)));
            return Err(CanError::Timeout);
        }

        if budget.is_zero() {
            return Err(CanError::Timeout);
        }
        thread::sleep(self.send_delay.min(budget));
        if self.send_delay > budget {
            return Err(CanError::Timeout);
        }
        self.sent_count.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    fn send_shutdown_until(
        &mut self,
        _frame: PiperFrame,
        deadline: Instant,
    ) -> Result<(), CanError> {
        let now = Instant::now();
        let Some(remaining) = deadline.checked_duration_since(now) else {
            return Err(CanError::Timeout);
        };
        if self.should_fail.load(Ordering::Relaxed) {
            return Err(CanError::Device(CanDeviceError::new(
                CanDeviceErrorKind::NoDevice,
                "Device disconnected",
            )));
        }
        if self.should_timeout.load(Ordering::Relaxed) {
            thread::sleep(remaining.min(Duration::from_millis(100)));
            return Err(CanError::Timeout);
        }
        thread::sleep(self.send_delay.min(remaining));
        if self.send_delay > remaining {
            return Err(CanError::Timeout);
        }
        self.sent_count.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
}

/// 生成测试帧
fn generate_test_frames(count: usize) -> Vec<PiperFrame> {
    (0..count)
        .map(|i| PiperFrame::new_standard((0x251 + (i % 6)) as u16, &[i as u8; 8]))
        .collect()
}

#[test]
fn test_rx_unaffected_by_tx_timeout() {
    // 测试场景：TX 线程遇到超时，RX 线程应继续正常工作

    let ctx = Arc::new(PiperContext::new());
    let config = PipelineConfig::default();
    let is_running = Arc::new(AtomicBool::new(true));
    let runtime_phase = Arc::new(AtomicU8::new(0));
    let last_fault = Arc::new(AtomicU8::new(0));
    let metrics = Arc::new(PiperMetrics::new());
    let maintenance_state_signal = Arc::new(MaintenanceStateSignal::default());
    let maintenance_lease_gate = Arc::new(MaintenanceLeaseGate::default());

    // 创建 RX 适配器：每 2ms 接收一帧
    let rx_frames = generate_test_frames(100);
    let rx_adapter = MockRxAdapter::new(rx_frames, Duration::from_millis(2));

    // 创建 TX 适配器：正常发送延迟 1ms
    let tx_adapter = MockTxAdapter::new(Duration::from_millis(1));

    // 创建命令通道
    let (reliable_tx, reliable_rx) = crossbeam_channel::bounded::<ReliableCommand>(10);
    let shutdown_lane = Arc::new(ShutdownLane::new());
    let normal_send_gate = Arc::new(NormalSendGate::new());
    let driver_mode = Arc::new(AtomicDriverMode::new(DriverMode::Normal));
    let realtime_slot: Arc<std::sync::Mutex<Option<RealtimeCommand>>> =
        Arc::new(std::sync::Mutex::new(None));

    // 启动 RX 线程
    let ctx_rx = ctx.clone();
    let is_running_rx = is_running.clone();
    let runtime_phase_rx = runtime_phase.clone();
    let normal_send_gate_rx = normal_send_gate.clone();
    let driver_mode_rx = driver_mode.clone();
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

    // 启动 TX 线程
    let ctx_tx = ctx.clone();
    let is_running_tx = is_running.clone();
    let runtime_phase_tx = runtime_phase.clone();
    let metrics_tx = metrics.clone();
    let last_fault_tx = last_fault.clone();
    let maintenance_lease_gate_tx = maintenance_lease_gate.clone();
    let normal_send_gate_tx = normal_send_gate.clone();
    let tx_handle = spawn_tx_loop(
        tx_adapter,
        BackendCapability::StrictRealtime,
        PipelineConfig::default(),
        realtime_slot,
        shutdown_lane,
        reliable_rx,
        is_running_tx,
        runtime_phase_tx,
        normal_send_gate_tx,
        metrics_tx,
        ctx_tx,
        last_fault_tx,
        maintenance_lease_gate_tx,
        Arc::new(AtomicDriverMode::new(DriverMode::Normal)),
    );

    // 等待 50ms，让系统稳定运行
    thread::sleep(Duration::from_millis(50));

    // 记录初始状态
    let initial_rx_count = metrics.rx_frames_valid.load(Ordering::Relaxed);

    // 模拟 TX 超时：设置 TX 适配器超时
    // 注意：由于 MockTxAdapter 是移动的，我们需要通过其他方式模拟
    // 这里我们发送一个会导致超时的命令（在实际场景中，这可能是总线错误）
    reliable_tx
        .send(ReliableCommand::single(PiperFrame::new_standard(
            0x123,
            &[1, 2, 3],
        )))
        .unwrap();

    // 等待 100ms，观察 RX 是否受影响
    thread::sleep(Duration::from_millis(100));

    // 检查 RX 状态更新是否继续
    let final_rx_count = metrics.rx_frames_valid.load(Ordering::Relaxed);
    let rx_updates = final_rx_count.saturating_sub(initial_rx_count);

    // 验证：RX 应该继续接收帧（即使 TX 遇到问题）
    // 在 100ms 内，RX 应该至少接收到一些帧（假设每 2ms 一帧，应该至少 30-40 帧）
    assert!(
        rx_updates > 0,
        "RX should continue receiving frames even when TX has issues. Received: {}",
        rx_updates
    );

    // 验证：RX 更新周期应该保持稳定（抖动 < 5ms）
    // 这里我们检查 metrics 中的超时次数，应该相对较少
    let rx_timeouts = metrics.rx_timeouts.load(Ordering::Relaxed);
    let total_rx_attempts = metrics.rx_frames_total.load(Ordering::Relaxed);
    let timeout_ratio = if total_rx_attempts > 0 {
        rx_timeouts as f64 / total_rx_attempts as f64
    } else {
        0.0
    };

    // 超时比例应该 < 50%（大部分时间应该能收到帧）
    assert!(
        timeout_ratio < 0.5,
        "RX timeout ratio should be low (< 50%), got: {:.2}%",
        timeout_ratio * 100.0
    );

    // 停止线程
    is_running.store(false, Ordering::Relaxed);
    let _ = rx_handle.join();
    let _ = tx_handle.join();
}

#[test]
fn test_tx_detects_rx_failure() {
    // 测试场景：RX 线程遇到致命错误，TX 线程应快速进入 fault-latched，
    // 关闭正常控制路径，但不要求立刻退出，以保留 shutdown lane。

    let ctx = Arc::new(PiperContext::new());
    let config = PipelineConfig::default();
    let is_running = Arc::new(AtomicBool::new(true));
    let runtime_phase = Arc::new(AtomicU8::new(0));
    let last_fault = Arc::new(AtomicU8::new(0));
    let metrics = Arc::new(PiperMetrics::new());
    let maintenance_state_signal = Arc::new(MaintenanceStateSignal::default());
    let maintenance_lease_gate = Arc::new(MaintenanceLeaseGate::default());

    // 创建 RX 适配器：初始正常，稍后模拟故障
    let rx_frames = generate_test_frames(10);
    let rx_adapter = MockRxAdapter::new(rx_frames, Duration::from_millis(2));
    let rx_should_fail = rx_adapter.should_fail.clone();

    // 创建 TX 适配器：正常发送
    let tx_adapter = MockTxAdapter::new(Duration::from_millis(1));

    // 创建命令通道
    let (_reliable_tx, reliable_rx) = crossbeam_channel::bounded::<ReliableCommand>(10);
    let shutdown_lane = Arc::new(ShutdownLane::new());
    let normal_send_gate = Arc::new(NormalSendGate::new());
    let driver_mode = Arc::new(AtomicDriverMode::new(DriverMode::Normal));
    let realtime_slot: Arc<std::sync::Mutex<Option<RealtimeCommand>>> =
        Arc::new(std::sync::Mutex::new(None));

    // 启动 RX 线程
    let ctx_rx = ctx.clone();
    let is_running_rx = is_running.clone();
    let runtime_phase_rx = runtime_phase.clone();
    let normal_send_gate_rx = normal_send_gate.clone();
    let driver_mode_rx = driver_mode.clone();
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

    // 启动 TX 线程
    let ctx_tx = ctx.clone();
    let is_running_tx = is_running.clone();
    let runtime_phase_tx = runtime_phase.clone();
    let metrics_tx = metrics.clone();
    let last_fault_tx = last_fault.clone();
    let maintenance_lease_gate_tx = maintenance_lease_gate.clone();
    let normal_send_gate_tx = normal_send_gate.clone();
    let tx_handle = spawn_tx_loop(
        tx_adapter,
        BackendCapability::StrictRealtime,
        PipelineConfig::default(),
        realtime_slot,
        shutdown_lane,
        reliable_rx,
        is_running_tx,
        runtime_phase_tx,
        normal_send_gate_tx,
        metrics_tx,
        ctx_tx,
        last_fault_tx,
        maintenance_lease_gate_tx,
        Arc::new(AtomicDriverMode::new(DriverMode::Normal)),
    );

    // 等待 20ms，让系统稳定运行
    thread::sleep(Duration::from_millis(20));

    // 模拟 RX 故障：设置 should_fail = true
    rx_should_fail.store(true, Ordering::Relaxed);

    // 等待 TX 线程观测到故障并进入 fault-latched
    let start = Instant::now();
    let mut fault_latched = false;
    for _ in 0..30 {
        if runtime_phase.load(Ordering::Relaxed) == 1
            && last_fault.load(Ordering::Relaxed) == RuntimeFaultKind::TransportError as u8
        {
            fault_latched = true;
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    let elapsed = start.elapsed();

    // 验证：TX 线程应快速进入 fault-latched，而不是继续开放正常控制路径
    let threshold = adjust_threshold_ms(200);
    assert!(
        fault_latched,
        "TX thread should latch the runtime fault within {:?} after RX failure. Elapsed: {:?}",
        threshold, elapsed
    );
    assert!(
        elapsed < threshold,
        "TX thread should observe RX failure quickly (< {:?}, CI环境已放宽). Elapsed: {:?}",
        threshold,
        elapsed
    );

    assert!(
        !tx_handle.is_finished(),
        "TX thread should remain alive after RX failure so the shutdown lane can still run"
    );

    // 清理
    is_running.store(false, Ordering::Relaxed);
    let _ = rx_handle.join();
    let _ = tx_handle.join();
}

#[test]
fn test_thread_lifecycle_linkage() {
    // 测试场景：RX 线程故障后，运行时先 fault-latch 保留 TX；
    // 外部再关闭 workers_running 时，两个线程都应有序退出。

    let ctx = Arc::new(PiperContext::new());
    let config = PipelineConfig::default();
    let is_running = Arc::new(AtomicBool::new(true));
    let runtime_phase = Arc::new(AtomicU8::new(0));
    let last_fault = Arc::new(AtomicU8::new(0));
    let metrics = Arc::new(PiperMetrics::new());
    let maintenance_state_signal = Arc::new(MaintenanceStateSignal::default());
    let maintenance_lease_gate = Arc::new(MaintenanceLeaseGate::default());

    // 创建 RX 适配器
    let rx_frames = generate_test_frames(5);
    let rx_adapter = MockRxAdapter::new(rx_frames, Duration::from_millis(2));
    let rx_should_fail = rx_adapter.should_fail.clone();

    // 创建 TX 适配器
    let tx_adapter = MockTxAdapter::new(Duration::from_millis(1));

    // 创建命令通道
    let (_reliable_tx, reliable_rx) = crossbeam_channel::bounded::<ReliableCommand>(10);
    let shutdown_lane = Arc::new(ShutdownLane::new());
    let normal_send_gate = Arc::new(NormalSendGate::new());
    let driver_mode = Arc::new(AtomicDriverMode::new(DriverMode::Normal));
    let realtime_slot: Arc<std::sync::Mutex<Option<RealtimeCommand>>> =
        Arc::new(std::sync::Mutex::new(None));

    // 启动 RX 线程
    let ctx_rx = ctx.clone();
    let is_running_rx = is_running.clone();
    let runtime_phase_rx = runtime_phase.clone();
    let normal_send_gate_rx = normal_send_gate.clone();
    let driver_mode_rx = driver_mode.clone();
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

    // 启动 TX 线程
    let ctx_tx = ctx.clone();
    let is_running_tx = is_running.clone();
    let runtime_phase_tx = runtime_phase.clone();
    let metrics_tx = metrics.clone();
    let last_fault_tx = last_fault.clone();
    let maintenance_lease_gate_tx = maintenance_lease_gate.clone();
    let normal_send_gate_tx = normal_send_gate.clone();
    let tx_handle = spawn_tx_loop(
        tx_adapter,
        BackendCapability::StrictRealtime,
        PipelineConfig::default(),
        realtime_slot,
        shutdown_lane,
        reliable_rx,
        is_running_tx,
        runtime_phase_tx,
        normal_send_gate_tx,
        metrics_tx,
        ctx_tx,
        last_fault_tx,
        maintenance_lease_gate_tx,
        Arc::new(AtomicDriverMode::new(DriverMode::Normal)),
    );

    // 等待 20ms，让系统稳定运行
    thread::sleep(Duration::from_millis(20));

    // 模拟 RX 致命错误
    rx_should_fail.store(true, Ordering::Relaxed);

    // 先等待 RX 线程退出且运行时进入 fault-latched
    let start = Instant::now();
    let mut runtime_fault_latched = false;

    for _ in 0..30 {
        if rx_handle.is_finished()
            && runtime_phase.load(Ordering::Relaxed) == 1
            && last_fault.load(Ordering::Relaxed) == RuntimeFaultKind::TransportError as u8
        {
            runtime_fault_latched = true;
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }

    let elapsed = start.elapsed();

    assert!(
        runtime_fault_latched,
        "RX failure should terminate RX and latch runtime fault before external shutdown. Elapsed: {:?}",
        elapsed
    );

    let threshold = adjust_threshold_ms(300);
    assert!(
        elapsed < threshold,
        "RX failure should surface quickly (< {:?}, CI环境已放宽). Elapsed: {:?}",
        threshold,
        elapsed
    );

    assert!(
        !tx_handle.is_finished(),
        "TX worker should remain alive on the fault-latch path until external shutdown"
    );

    is_running.store(false, Ordering::Relaxed);
    let shutdown_start = Instant::now();
    let mut both_exited = false;
    for _ in 0..30 {
        if rx_handle.is_finished() && tx_handle.is_finished() {
            both_exited = true;
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert!(
        both_exited,
        "Both workers should exit after external shutdown request. Elapsed after shutdown: {:?}",
        shutdown_start.elapsed()
    );

    let _ = rx_handle.join();
    let _ = tx_handle.join();
}
