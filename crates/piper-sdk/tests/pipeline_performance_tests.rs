//! Pipeline 结构性性能测试
//!
//! 这些测试保留在默认 `cargo test` 中，但只验证进度、队列语义和 metrics 准确性，
//! 不再依赖 wall-clock P95/P99 阈值。

use piper_sdk::can::{CanError, PiperFrame, RealtimeTxAdapter, RxAdapter};
use piper_sdk::driver::command::ReliableCommand;
use piper_sdk::driver::{
    BackendCapability, MaintenanceLeaseGate, MaintenanceStateSignal, NormalSendGate,
    PipelineConfig, PiperContext, PiperMetrics, ShutdownLane, rx_loop, test_support::spawn_tx_loop,
};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

fn wait_until(timeout: Duration, condition: impl Fn() -> bool) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if condition() {
            return;
        }
        thread::sleep(Duration::from_millis(1));
    }
    panic!("condition not met within {:?}", timeout);
}

struct QueueRxAdapter {
    frames: VecDeque<PiperFrame>,
}

impl QueueRxAdapter {
    fn new(frames: impl IntoIterator<Item = PiperFrame>) -> Self {
        Self {
            frames: frames.into_iter().collect(),
        }
    }
}

impl RxAdapter for QueueRxAdapter {
    fn receive(&mut self) -> Result<PiperFrame, CanError> {
        self.frames.pop_front().ok_or(CanError::Timeout)
    }
}

struct RecordingTxAdapter {
    sent_frames: Arc<Mutex<Vec<PiperFrame>>>,
    send_delay: Duration,
}

impl RecordingTxAdapter {
    fn new(send_delay: Duration) -> Self {
        Self {
            sent_frames: Arc::new(Mutex::new(Vec::new())),
            send_delay,
        }
    }
}

impl RealtimeTxAdapter for RecordingTxAdapter {
    fn send_control(&mut self, frame: PiperFrame, budget: Duration) -> Result<(), CanError> {
        let deadline = Instant::now() + budget;
        let now = Instant::now();
        let Some(remaining) = deadline.checked_duration_since(now) else {
            return Err(CanError::Timeout);
        };
        let sleep_for = self.send_delay.min(remaining);
        if !sleep_for.is_zero() {
            thread::sleep(sleep_for);
        }
        if self.send_delay > remaining {
            return Err(CanError::Timeout);
        }
        self.sent_frames.lock().unwrap().push(frame);
        Ok(())
    }

    fn send_shutdown_until(
        &mut self,
        frame: PiperFrame,
        deadline: Instant,
    ) -> Result<(), CanError> {
        let now = Instant::now();
        let Some(remaining) = deadline.checked_duration_since(now) else {
            return Err(CanError::Timeout);
        };
        let sleep_for = self.send_delay.min(remaining);
        if !sleep_for.is_zero() {
            thread::sleep(sleep_for);
        }
        if self.send_delay > remaining {
            return Err(CanError::Timeout);
        }
        self.sent_frames.lock().unwrap().push(frame);
        Ok(())
    }
}

struct BlockingFirstTxAdapter {
    sent_frames: Arc<Mutex<Vec<PiperFrame>>>,
    first_frame_tx: Option<mpsc::Sender<PiperFrame>>,
    release_first_rx: mpsc::Receiver<()>,
    first_send_blocked: bool,
}

impl BlockingFirstTxAdapter {
    fn new(first_frame_tx: mpsc::Sender<PiperFrame>, release_first_rx: mpsc::Receiver<()>) -> Self {
        Self {
            sent_frames: Arc::new(Mutex::new(Vec::new())),
            first_frame_tx: Some(first_frame_tx),
            release_first_rx,
            first_send_blocked: false,
        }
    }
}

impl RealtimeTxAdapter for BlockingFirstTxAdapter {
    fn send_control(&mut self, frame: PiperFrame, _budget: Duration) -> Result<(), CanError> {
        self.sent_frames.lock().unwrap().push(frame);

        if !self.first_send_blocked {
            self.first_send_blocked = true;
            if let Some(sender) = self.first_frame_tx.take() {
                sender.send(frame).unwrap();
            }
            self.release_first_rx.recv().unwrap();
        }

        Ok(())
    }

    fn send_shutdown_until(
        &mut self,
        frame: PiperFrame,
        _deadline: Instant,
    ) -> Result<(), CanError> {
        self.send_control(frame, Duration::from_millis(1))
    }
}

fn start_rx_loop(
    rx_adapter: impl RxAdapter + Send + 'static,
    ctx: Arc<PiperContext>,
    metrics: Arc<PiperMetrics>,
    is_running: Arc<AtomicBool>,
    runtime_phase: Arc<AtomicU8>,
    fault: Arc<AtomicU8>,
) -> thread::JoinHandle<()> {
    let maintenance_state_signal = Arc::new(MaintenanceStateSignal::default());
    let normal_send_gate = Arc::new(NormalSendGate::new());
    let driver_mode = Arc::new(piper_sdk::driver::AtomicDriverMode::new(
        piper_sdk::driver::DriverMode::Normal,
    ));
    thread::spawn(move || {
        rx_loop(
            rx_adapter,
            BackendCapability::StrictRealtime,
            ctx,
            PipelineConfig::default(),
            is_running,
            runtime_phase,
            normal_send_gate,
            driver_mode,
            metrics,
            fault,
            maintenance_state_signal,
        );
    })
}

#[allow(clippy::too_many_arguments)]
fn start_tx_loop(
    tx_adapter: impl RealtimeTxAdapter + Send + 'static,
    ctx: Arc<PiperContext>,
    metrics: Arc<PiperMetrics>,
    is_running: Arc<AtomicBool>,
    runtime_phase: Arc<AtomicU8>,
    fault: Arc<AtomicU8>,
    realtime_slot: Arc<std::sync::Mutex<Option<piper_sdk::driver::command::RealtimeCommand>>>,
    shutdown_lane: Arc<ShutdownLane>,
    reliable_rx: crossbeam_channel::Receiver<ReliableCommand>,
) -> thread::JoinHandle<()> {
    let normal_send_gate = Arc::new(NormalSendGate::new());
    let _maintenance_state_signal = Arc::new(MaintenanceStateSignal::default());
    let maintenance_lease_gate = Arc::new(MaintenanceLeaseGate::default());
    spawn_tx_loop(
        tx_adapter,
        BackendCapability::StrictRealtime,
        PipelineConfig::default(),
        realtime_slot,
        shutdown_lane,
        reliable_rx,
        is_running,
        runtime_phase,
        normal_send_gate,
        metrics,
        ctx,
        fault,
        maintenance_lease_gate,
        Arc::new(piper_sdk::driver::AtomicDriverMode::new(
            piper_sdk::driver::DriverMode::Normal,
        )),
    )
}

#[test]
fn test_rx_loop_processes_burst_without_transport_fault() {
    let frames = (0..12).map(|i| PiperFrame::new_standard((0x251 + (i % 6)) as u16, &[i as u8; 8]));
    let ctx = Arc::new(PiperContext::new());
    let metrics = Arc::new(PiperMetrics::new());
    let is_running = Arc::new(AtomicBool::new(true));
    let runtime_phase = Arc::new(AtomicU8::new(0));
    let fault = Arc::new(AtomicU8::new(0));

    let handle = start_rx_loop(
        QueueRxAdapter::new(frames),
        ctx,
        metrics.clone(),
        is_running.clone(),
        runtime_phase,
        fault.clone(),
    );

    wait_until(Duration::from_secs(1), || {
        metrics.rx_frames_valid.load(Ordering::Relaxed) >= 12
    });

    is_running.store(false, Ordering::Relaxed);
    handle.join().unwrap();

    let snapshot = metrics.snapshot();
    assert_eq!(snapshot.rx_frames_valid, 12);
    assert_eq!(snapshot.device_errors, 0);
    assert_eq!(fault.load(Ordering::Relaxed), 0);
}

#[test]
fn test_tx_loop_drains_reliable_queue_with_slow_sender() {
    let ctx = Arc::new(PiperContext::new());
    let metrics = Arc::new(PiperMetrics::new());
    let is_running = Arc::new(AtomicBool::new(true));
    let runtime_phase = Arc::new(AtomicU8::new(0));
    let fault = Arc::new(AtomicU8::new(0));
    let tx_adapter = RecordingTxAdapter::new(Duration::from_micros(200));
    let sent_frames = tx_adapter.sent_frames.clone();
    let shutdown_lane = Arc::new(ShutdownLane::new());
    let (reliable_tx, reliable_rx) = crossbeam_channel::bounded::<ReliableCommand>(10);
    let realtime_slot = Arc::new(std::sync::Mutex::new(None));

    let handle = start_tx_loop(
        tx_adapter,
        ctx,
        metrics.clone(),
        is_running.clone(),
        runtime_phase,
        fault.clone(),
        realtime_slot,
        shutdown_lane,
        reliable_rx,
    );

    let expected: Vec<PiperFrame> = (0..10)
        .map(|i| PiperFrame::new_standard((0x180 + i) as u16, &[i as u8; 8]))
        .collect();
    for frame in &expected {
        reliable_tx.send(ReliableCommand::single(*frame)).unwrap();
    }

    wait_until(Duration::from_secs(2), || {
        sent_frames.lock().unwrap().len() == expected.len()
    });

    is_running.store(false, Ordering::Relaxed);
    handle.join().unwrap();

    let sent = sent_frames.lock().unwrap().clone();
    assert_eq!(sent, expected);

    let snapshot = metrics.snapshot();
    assert_eq!(snapshot.tx_frames_sent_total, expected.len() as u64);
    assert_eq!(snapshot.tx_timeouts, 0);
    assert_eq!(snapshot.device_errors, 0);
    assert_eq!(fault.load(Ordering::Relaxed), 0);
}

#[test]
fn test_tx_loop_realtime_bursts_do_not_starve_reliable_queue() {
    let ctx = Arc::new(PiperContext::new());
    let metrics = Arc::new(PiperMetrics::new());
    let is_running = Arc::new(AtomicBool::new(true));
    let runtime_phase = Arc::new(AtomicU8::new(0));
    let fault = Arc::new(AtomicU8::new(0));
    let tx_adapter = RecordingTxAdapter::new(Duration::from_micros(300));
    let sent_frames = tx_adapter.sent_frames.clone();
    let shutdown_lane = Arc::new(ShutdownLane::new());
    let (reliable_tx, reliable_rx) = crossbeam_channel::bounded::<ReliableCommand>(10);
    let realtime_slot = Arc::new(std::sync::Mutex::new(None));
    let realtime_slot_writer = realtime_slot.clone();

    let handle = start_tx_loop(
        tx_adapter,
        ctx,
        metrics.clone(),
        is_running.clone(),
        runtime_phase,
        fault.clone(),
        realtime_slot,
        shutdown_lane,
        reliable_rx,
    );

    let reliable_frames: Vec<PiperFrame> = (0..3)
        .map(|i| PiperFrame::new_standard((0x300 + i) as u16, &[0xAA, i as u8]))
        .collect();
    for frame in &reliable_frames {
        reliable_tx.send(ReliableCommand::single(*frame)).unwrap();
    }

    let realtime_writer = thread::spawn(move || {
        for i in 0..30 {
            let frame = PiperFrame::new_standard((0x400 + i) as u16, &[i as u8; 8]);
            *realtime_slot_writer.lock().unwrap() =
                Some(piper_sdk::driver::command::RealtimeCommand::single(frame));
            thread::sleep(Duration::from_micros(50));
        }
    });

    wait_until(Duration::from_secs(2), || {
        let sent = sent_frames.lock().unwrap();
        reliable_frames
            .iter()
            .all(|frame| sent.iter().any(|sent_frame| sent_frame.id == frame.id))
    });

    realtime_writer.join().unwrap();
    is_running.store(false, Ordering::Relaxed);
    handle.join().unwrap();

    let sent = sent_frames.lock().unwrap();
    for frame in &reliable_frames {
        assert!(
            sent.iter().any(|sent_frame| sent_frame.id == frame.id),
            "reliable frame 0x{:X} should not starve",
            frame.id
        );
    }
    assert_eq!(fault.load(Ordering::Relaxed), 0);
}

#[test]
fn test_metrics_snapshot_matches_processed_frames() {
    let ctx = Arc::new(PiperContext::new());
    let metrics = Arc::new(PiperMetrics::new());
    let is_running = Arc::new(AtomicBool::new(true));
    let runtime_phase = Arc::new(AtomicU8::new(0));
    let fault = Arc::new(AtomicU8::new(0));

    let rx_frames: Vec<PiperFrame> = (0..6)
        .map(|i| PiperFrame::new_standard((0x251 + (i % 6)) as u16, &[i as u8; 8]))
        .collect();
    let rx_handle = start_rx_loop(
        QueueRxAdapter::new(rx_frames.clone()),
        ctx.clone(),
        metrics.clone(),
        is_running.clone(),
        runtime_phase.clone(),
        fault.clone(),
    );

    let tx_adapter = RecordingTxAdapter::new(Duration::ZERO);
    let shutdown_lane = Arc::new(ShutdownLane::new());
    let (reliable_tx, reliable_rx) = crossbeam_channel::bounded::<ReliableCommand>(10);
    let realtime_slot = Arc::new(std::sync::Mutex::new(None));
    let tx_handle = start_tx_loop(
        tx_adapter,
        ctx,
        metrics.clone(),
        is_running.clone(),
        runtime_phase,
        fault.clone(),
        realtime_slot,
        shutdown_lane,
        reliable_rx,
    );

    let tx_frames: Vec<PiperFrame> = (0..4)
        .map(|i| PiperFrame::new_standard((0x500 + i) as u16, &[i as u8; 8]))
        .collect();
    for frame in &tx_frames {
        reliable_tx.send(ReliableCommand::single(*frame)).unwrap();
    }

    wait_until(Duration::from_secs(1), || {
        let snapshot = metrics.snapshot();
        snapshot.rx_frames_valid == rx_frames.len() as u64
            && snapshot.tx_frames_sent_total == tx_frames.len() as u64
    });

    is_running.store(false, Ordering::Relaxed);
    rx_handle.join().unwrap();
    tx_handle.join().unwrap();

    let snapshot = metrics.snapshot();
    assert_eq!(snapshot.rx_frames_valid, rx_frames.len() as u64);
    assert_eq!(snapshot.tx_frames_sent_total, tx_frames.len() as u64);
    assert_eq!(snapshot.device_errors, 0);
    assert_eq!(snapshot.tx_timeouts, 0);
}

#[test]
fn test_realtime_overwrite_keeps_latest_pending_command() {
    let ctx = Arc::new(PiperContext::new());
    let metrics = Arc::new(PiperMetrics::new());
    let is_running = Arc::new(AtomicBool::new(true));
    let runtime_phase = Arc::new(AtomicU8::new(0));
    let fault = Arc::new(AtomicU8::new(0));
    let (first_frame_tx, first_frame_rx) = mpsc::channel();
    let (release_first_tx, release_first_rx) = mpsc::channel();
    let tx_adapter = BlockingFirstTxAdapter::new(first_frame_tx, release_first_rx);
    let sent_frames = tx_adapter.sent_frames.clone();
    let shutdown_lane = Arc::new(ShutdownLane::new());
    let (_reliable_tx, reliable_rx) = crossbeam_channel::bounded::<ReliableCommand>(10);
    let realtime_slot = Arc::new(std::sync::Mutex::new(None));
    let realtime_slot_writer = realtime_slot.clone();

    let handle = start_tx_loop(
        tx_adapter,
        ctx,
        metrics.clone(),
        is_running.clone(),
        runtime_phase,
        fault.clone(),
        realtime_slot,
        shutdown_lane,
        reliable_rx,
    );

    let first = PiperFrame::new_standard(0x610, &[1; 8]);
    *realtime_slot_writer.lock().unwrap() =
        Some(piper_sdk::driver::command::RealtimeCommand::single(first));

    let blocked_frame = first_frame_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    assert_eq!(blocked_frame.id, first.id);

    let latest = PiperFrame::new_standard(0x614, &[4; 8]);
    for frame in [
        PiperFrame::new_standard(0x611, &[2; 8]),
        PiperFrame::new_standard(0x612, &[3; 8]),
        PiperFrame::new_standard(0x613, &[4; 8]),
        latest,
    ] {
        *realtime_slot_writer.lock().unwrap() =
            Some(piper_sdk::driver::command::RealtimeCommand::single(frame));
    }

    release_first_tx.send(()).unwrap();

    wait_until(Duration::from_secs(1), || {
        sent_frames.lock().unwrap().len() >= 2
    });

    is_running.store(false, Ordering::Relaxed);
    handle.join().unwrap();

    let sent = sent_frames.lock().unwrap().clone();
    assert_eq!(sent[0].id, first.id);
    assert_eq!(sent[1].id, latest.id);
    assert_eq!(metrics.snapshot().tx_frames_sent_total, 2);
    assert_eq!(fault.load(Ordering::Relaxed), 0);
}
