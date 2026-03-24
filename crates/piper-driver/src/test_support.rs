use crate::command::{RealtimeCommand, ReliableCommand, SoftRealtimeMailbox};
use crate::mode::AtomicDriverMode;
use crate::pipeline::{PipelineConfig, tx_loop_mailbox};
use crate::{
    BackendCapability, MaintenanceLeaseGate, NormalSendGate, PiperContext, PiperMetrics,
    ShutdownLane,
};
use crossbeam_channel::Receiver;
use piper_can::RealtimeTxAdapter;
use std::sync::atomic::{AtomicBool, AtomicU8};
use std::sync::{Arc, Mutex};
use std::thread;

/// Workspace 内部测试辅助：启动 TX loop，但不暴露 mailbox 等内部调度原语。
#[allow(clippy::too_many_arguments)]
pub fn spawn_tx_loop<T>(
    tx: T,
    backend_capability: BackendCapability,
    config: PipelineConfig,
    realtime_slot: Arc<Mutex<Option<RealtimeCommand>>>,
    shutdown_lane: Arc<ShutdownLane>,
    reliable_rx: Receiver<ReliableCommand>,
    workers_running: Arc<AtomicBool>,
    runtime_phase: Arc<AtomicU8>,
    normal_send_gate: Arc<NormalSendGate>,
    metrics: Arc<PiperMetrics>,
    ctx: Arc<PiperContext>,
    last_fault: Arc<AtomicU8>,
    maintenance_gate: Arc<MaintenanceLeaseGate>,
    driver_mode: Arc<AtomicDriverMode>,
) -> thread::JoinHandle<()>
where
    T: RealtimeTxAdapter + Send + 'static,
{
    let soft_realtime_rx = Arc::new(SoftRealtimeMailbox::new());
    let (maintenance_ctrl_tx, maintenance_ctrl_rx) = crossbeam_channel::unbounded();
    maintenance_gate.set_control_sink(maintenance_ctrl_tx);

    thread::spawn(move || {
        tx_loop_mailbox(
            tx,
            backend_capability,
            config,
            realtime_slot,
            soft_realtime_rx,
            shutdown_lane,
            reliable_rx,
            workers_running,
            runtime_phase,
            normal_send_gate,
            metrics,
            ctx,
            last_fault,
            maintenance_ctrl_rx,
            maintenance_gate,
            driver_mode,
        );
    })
}
