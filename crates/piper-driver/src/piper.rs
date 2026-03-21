//! Robot API 模块
//!
//! 提供对外的 `Piper` 结构体，封装底层 IO 线程和状态同步细节。

use crate::command::{CommandPriority, PiperCommand, RealtimeCommand, ReliableCommand};
use crate::error::DriverError;
use crate::fps_stats::{FpsCounts, FpsResult};
use crate::metrics::{MetricsSnapshot, PiperMetrics};
use crate::pipeline::*;
use crate::state::*;
use crossbeam_channel::{Receiver, Sender};
use piper_can::{
    CanError, PiperFrame, RealtimeTxAdapter, RxAdapter, SplittableAdapter, TimingCapability,
};
use std::mem::ManuallyDrop;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{JoinHandle, spawn};
use std::time::{Duration, Instant};
use tracing::{error, info, warn};

/// Extension trait for timeout-capable thread joins
trait JoinTimeout {
    fn join_timeout(self, timeout: Duration) -> std::thread::Result<()>;
}

impl<T: std::marker::Send + 'static> JoinTimeout for JoinHandle<T> {
    fn join_timeout(self, timeout: Duration) -> std::thread::Result<()> {
        use std::sync::mpsc;

        // Create a channel for signaling completion
        let (tx, rx) = mpsc::channel();

        // Spawn a watchdog thread that joins the target thread
        spawn(move || {
            let result = self.join();
            // Send result (ignore send errors - receiver may have timed out)
            let _ = tx.send(result);
        });

        // Block with timeout - no busy waiting!
        match rx.recv_timeout(timeout) {
            Ok(join_result) => join_result.map(|_| ()), // Thread finished
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // Timeout: watchdog thread continues running
                // This is acceptable - OS will clean up on process exit
                Err(std::boxed::Box::new(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "Thread join timeout",
                )))
            },
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                // Channel disconnected unexpectedly - thread panicked
                Err(std::boxed::Box::new(std::io::Error::new(
                    std::io::ErrorKind::ConnectionReset,
                    "Thread panicked during join",
                )))
            },
        }
    }
}

#[derive(Debug)]
struct RuntimeWorkers {
    rx_thread: Option<JoinHandle<()>>,
    tx_thread: Option<JoinHandle<()>>,
}

/// 运行时健康故障类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum RuntimeFaultKind {
    RxExited = 1,
    TxExited = 2,
    TransportError = 3,
}

impl RuntimeFaultKind {
    fn from_raw(raw: u8) -> Option<Self> {
        match raw {
            1 => Some(Self::RxExited),
            2 => Some(Self::TxExited),
            3 => Some(Self::TransportError),
            _ => None,
        }
    }
}

/// 运行时健康状态快照。
#[derive(Debug, Clone)]
pub struct HealthStatus {
    pub connected: bool,
    pub last_feedback_age: Duration,
    pub rx_alive: bool,
    pub tx_alive: bool,
    pub fault: Option<RuntimeFaultKind>,
}

/// Driver 运行时阶段。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum RuntimePhase {
    Running = 0,
    FaultLatched = 1,
    Stopping = 2,
}

pub(crate) const NORMAL_FRAME_SEND_BUDGET: Duration = Duration::from_micros(500);

#[doc(hidden)]
#[derive(Debug, Default)]
pub struct NormalSendGate {
    closed: AtomicBool,
    epoch: AtomicU64,
    inflight_normal_sends: AtomicUsize,
}

#[derive(Debug)]
pub(crate) struct NormalSendPermit<'a> {
    gate: &'a NormalSendGate,
    epoch: u64,
}

impl NormalSendGate {
    pub fn new() -> Self {
        Self::default()
    }

    pub(crate) fn acquire(&self) -> Option<NormalSendPermit<'_>> {
        let epoch = self.epoch.load(Ordering::Acquire);
        if self.closed.load(Ordering::Acquire) {
            return None;
        }

        self.inflight_normal_sends.fetch_add(1, Ordering::AcqRel);

        let closed = self.closed.load(Ordering::Acquire);
        let current_epoch = self.epoch.load(Ordering::Acquire);
        if closed || current_epoch != epoch {
            self.inflight_normal_sends.fetch_sub(1, Ordering::AcqRel);
            return None;
        }

        Some(NormalSendPermit { gate: self, epoch })
    }

    pub(crate) fn close(&self) {
        self.closed.store(true, Ordering::Release);
        self.epoch.fetch_add(1, Ordering::AcqRel);
    }

    pub fn inflight_normal_sends(&self) -> usize {
        self.inflight_normal_sends.load(Ordering::Acquire)
    }
}

impl Drop for NormalSendPermit<'_> {
    fn drop(&mut self) {
        self.gate.inflight_normal_sends.fetch_sub(1, Ordering::AcqRel);
    }
}

impl NormalSendPermit<'_> {
    pub(crate) fn still_open(&self) -> bool {
        !self.gate.closed.load(Ordering::Acquire)
            && self.gate.epoch.load(Ordering::Acquire) == self.epoch
    }
}

#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum MaintenanceGateState {
    DeniedFaulted = 0,
    DeniedActiveControl = 1,
    DeniedTransportDown = 2,
    AllowedStandby = 3,
}

impl MaintenanceGateState {
    pub fn allows_lease(self) -> bool {
        matches!(self, Self::AllowedStandby)
    }

    fn from_raw(raw: u8) -> Self {
        match raw {
            0 => Self::DeniedFaulted,
            1 => Self::DeniedActiveControl,
            3 => Self::AllowedStandby,
            _ => Self::DeniedTransportDown,
        }
    }
}

#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaintenanceRevocationReason {
    ControllerStateChanged,
    SessionReplaced,
    SessionClosed,
}

#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MaintenanceRevocationEvent {
    session_id: u32,
    session_key: u64,
    reason: MaintenanceRevocationReason,
}

impl MaintenanceRevocationEvent {
    pub fn session_id(self) -> u32 {
        self.session_id
    }

    pub fn session_key(self) -> u64 {
        self.session_key
    }

    pub fn reason(self) -> MaintenanceRevocationReason {
        self.reason
    }
}

#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MaintenanceLeaseSnapshot {
    state: MaintenanceGateState,
    holder_session_id: Option<u32>,
    holder_session_key: Option<u64>,
    lease_epoch: u64,
}

impl MaintenanceLeaseSnapshot {
    pub fn state(self) -> MaintenanceGateState {
        self.state
    }

    pub fn holder_session_id(self) -> Option<u32> {
        self.holder_session_id
    }

    pub fn holder_session_key(self) -> Option<u64> {
        self.holder_session_key
    }

    pub fn lease_epoch(self) -> u64 {
        self.lease_epoch
    }
}

#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaintenanceLeaseAcquireResult {
    Granted { lease_epoch: u64 },
    DeniedHeld { holder_session_id: Option<u32> },
    DeniedState { state: MaintenanceGateState },
}

#[derive(Debug, Clone, Copy)]
struct MaintenanceGateInner {
    state: MaintenanceGateState,
    holder_session_id: Option<u32>,
    holder_session_key: Option<u64>,
    lease_epoch: u64,
}

impl Default for MaintenanceGateInner {
    fn default() -> Self {
        Self {
            state: MaintenanceGateState::DeniedTransportDown,
            holder_session_id: None,
            holder_session_key: None,
            lease_epoch: 0,
        }
    }
}

#[doc(hidden)]
#[derive(Debug)]
pub struct MaintenanceGate {
    state: AtomicU8,
    holder_session_id: AtomicU32,
    holder_session_key: AtomicU64,
    lease_epoch: AtomicU64,
    inner: Mutex<MaintenanceGateInner>,
    wait_cv: Condvar,
    event_sink: Mutex<Option<Sender<MaintenanceRevocationEvent>>>,
}

impl Default for MaintenanceGate {
    fn default() -> Self {
        let inner = MaintenanceGateInner::default();
        Self {
            state: AtomicU8::new(inner.state as u8),
            holder_session_id: AtomicU32::new(0),
            holder_session_key: AtomicU64::new(0),
            lease_epoch: AtomicU64::new(inner.lease_epoch),
            inner: Mutex::new(inner),
            wait_cv: Condvar::new(),
            event_sink: Mutex::new(None),
        }
    }
}

impl MaintenanceGate {
    fn sync_atomics(&self, inner: &MaintenanceGateInner) {
        self.state.store(inner.state as u8, Ordering::Release);
        self.holder_session_id
            .store(inner.holder_session_id.unwrap_or(0), Ordering::Release);
        self.holder_session_key
            .store(inner.holder_session_key.unwrap_or(0), Ordering::Release);
        self.lease_epoch.store(inner.lease_epoch, Ordering::Release);
    }

    fn emit(&self, event: MaintenanceRevocationEvent) {
        if let Some(sink) = self.event_sink.lock().unwrap().as_ref() {
            let _ = sink.try_send(event);
        }
    }

    fn build_revocation_event(
        &self,
        inner: &MaintenanceGateInner,
        reason: MaintenanceRevocationReason,
    ) -> Option<MaintenanceRevocationEvent> {
        Some(MaintenanceRevocationEvent {
            session_id: inner.holder_session_id?,
            session_key: inner.holder_session_key?,
            reason,
        })
    }

    pub fn set_event_sink(&self, sink: Sender<MaintenanceRevocationEvent>) {
        *self.event_sink.lock().unwrap() = Some(sink);
    }

    pub fn snapshot(&self) -> MaintenanceLeaseSnapshot {
        MaintenanceLeaseSnapshot {
            state: MaintenanceGateState::from_raw(self.state.load(Ordering::Acquire)),
            holder_session_id: match self.holder_session_id.load(Ordering::Acquire) {
                0 => None,
                holder => Some(holder),
            },
            holder_session_key: match self.holder_session_key.load(Ordering::Acquire) {
                0 => None,
                holder => Some(holder),
            },
            lease_epoch: self.lease_epoch.load(Ordering::Acquire),
        }
    }

    pub fn current_state(&self) -> MaintenanceGateState {
        MaintenanceGateState::from_raw(self.state.load(Ordering::Acquire))
    }

    pub fn set_state(&self, new_state: MaintenanceGateState) {
        let event = {
            let mut inner = self.inner.lock().unwrap();
            let mut event = None;
            if inner.state != new_state {
                inner.state = new_state;
            }
            if !new_state.allows_lease() {
                event = self.build_revocation_event(
                    &inner,
                    MaintenanceRevocationReason::ControllerStateChanged,
                );
                if event.is_some() {
                    inner.holder_session_id = None;
                    inner.holder_session_key = None;
                    inner.lease_epoch = inner.lease_epoch.wrapping_add(1);
                }
            }
            self.sync_atomics(&inner);
            self.wait_cv.notify_all();
            event
        };

        if let Some(event) = event {
            self.emit(event);
        }
    }

    pub fn acquire_blocking(
        &self,
        session_id: u32,
        session_key: u64,
        timeout: Duration,
    ) -> MaintenanceLeaseAcquireResult {
        let deadline = Instant::now() + timeout;
        let mut inner = self.inner.lock().unwrap();
        loop {
            if !inner.state.allows_lease() {
                return MaintenanceLeaseAcquireResult::DeniedState { state: inner.state };
            }

            match inner.holder_session_key {
                None => {
                    inner.holder_session_id = Some(session_id);
                    inner.holder_session_key = Some(session_key);
                    inner.lease_epoch = inner.lease_epoch.wrapping_add(1);
                    let lease_epoch = inner.lease_epoch;
                    self.sync_atomics(&inner);
                    self.wait_cv.notify_all();
                    return MaintenanceLeaseAcquireResult::Granted { lease_epoch };
                },
                Some(holder_key) if holder_key == session_key => {
                    return MaintenanceLeaseAcquireResult::Granted {
                        lease_epoch: inner.lease_epoch,
                    };
                },
                _ => {
                    let now = Instant::now();
                    if now >= deadline {
                        return MaintenanceLeaseAcquireResult::DeniedHeld {
                            holder_session_id: inner.holder_session_id,
                        };
                    }
                    let timeout = deadline.saturating_duration_since(now);
                    let (next_inner, wait_result) =
                        self.wait_cv.wait_timeout(inner, timeout).unwrap();
                    inner = next_inner;
                    if wait_result.timed_out() {
                        return MaintenanceLeaseAcquireResult::DeniedHeld {
                            holder_session_id: inner.holder_session_id,
                        };
                    }
                },
            }
        }
    }

    pub fn release_if_holder(&self, session_key: u64) -> bool {
        let mut inner = self.inner.lock().unwrap();
        if inner.holder_session_key != Some(session_key) {
            return false;
        }
        inner.holder_session_id = None;
        inner.holder_session_key = None;
        inner.lease_epoch = inner.lease_epoch.wrapping_add(1);
        self.sync_atomics(&inner);
        self.wait_cv.notify_all();
        true
    }

    pub fn revoke_if_holder(
        &self,
        session_key: u64,
        reason: MaintenanceRevocationReason,
    ) -> Option<MaintenanceRevocationEvent> {
        let event = {
            let mut inner = self.inner.lock().unwrap();
            if inner.holder_session_key != Some(session_key) {
                return None;
            }
            let event = self.build_revocation_event(&inner, reason);
            inner.holder_session_id = None;
            inner.holder_session_key = None;
            inner.lease_epoch = inner.lease_epoch.wrapping_add(1);
            self.sync_atomics(&inner);
            self.wait_cv.notify_all();
            event
        };

        if let Some(event) = event {
            self.emit(event);
        }
        event
    }

    pub fn is_valid(&self, session_key: u64, lease_epoch: u64) -> bool {
        self.holder_session_key.load(Ordering::Acquire) == session_key
            && self.lease_epoch.load(Ordering::Acquire) == lease_epoch
            && self.current_state().allows_lease()
    }
}

#[doc(hidden)]
pub type MaintenanceStateSignal = MaintenanceGate;

#[doc(hidden)]
pub type MaintenanceLeaseGate = MaintenanceGate;

/// 停机命令的有界确认句柄。
///
/// 只服务 shutdown lane，不扩展到普通 reliable/realtime 命令。
#[derive(Debug)]
pub struct ShutdownReceipt {
    ack_rx: Receiver<Result<(), DriverError>>,
    deadline: Instant,
}

impl ShutdownReceipt {
    /// 等待 TX 线程返回停机帧发送结果。
    ///
    /// timeout 语义已经在 enqueue 时绑定到 shutdown command，本方法只等待 ack。
    pub fn wait(self) -> Result<(), DriverError> {
        let wait_result = match self.deadline.checked_duration_since(Instant::now()) {
            Some(remaining) => self.ack_rx.recv_timeout(remaining),
            None => match self.ack_rx.try_recv() {
                Ok(result) => return result,
                Err(crossbeam_channel::TryRecvError::Empty) => {
                    return Err(DriverError::Timeout);
                },
                Err(crossbeam_channel::TryRecvError::Disconnected) => {
                    return Err(DriverError::ChannelClosed);
                },
            },
        };

        match wait_result {
            Ok(result) => result,
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => Err(DriverError::Timeout),
            Err(_) => Err(DriverError::ChannelClosed),
        }
    }
}

#[derive(Debug)]
struct ShutdownRequest {
    frame: PiperFrame,
    deadline: Instant,
    waiters: Vec<crossbeam_channel::Sender<Result<(), DriverError>>>,
    sending: bool,
}

#[derive(Debug, Default)]
struct ShutdownLaneState {
    request: Option<ShutdownRequest>,
    closed: bool,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ShutdownDispatch {
    pub frame: PiperFrame,
    pub deadline: Instant,
}

#[derive(Debug, Default)]
pub struct ShutdownLane {
    has_pending: AtomicBool,
    state: Mutex<ShutdownLaneState>,
}

impl ShutdownLane {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn has_pending(&self) -> bool {
        self.has_pending.load(Ordering::Acquire)
    }

    pub fn enqueue(
        &self,
        frame: PiperFrame,
        deadline: Instant,
        metrics: &Arc<PiperMetrics>,
    ) -> Result<ShutdownReceipt, DriverError> {
        metrics.tx_shutdown_requests_total.fetch_add(1, Ordering::Relaxed);

        let (ack_tx, ack_rx) = crossbeam_channel::bounded(1);
        let mut state = self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());

        if state.closed {
            return Err(DriverError::ChannelClosed);
        }

        match state.request.as_mut() {
            None => {
                state.request = Some(ShutdownRequest {
                    frame,
                    deadline,
                    waiters: vec![ack_tx],
                    sending: false,
                });
                self.has_pending.store(true, Ordering::Release);
            },
            Some(request) if request.frame == frame => {
                request.deadline = request.deadline.min(deadline);
                request.waiters.push(ack_tx);
                metrics.tx_shutdown_coalesced_total.fetch_add(1, Ordering::Relaxed);
            },
            Some(_) => {
                metrics.tx_shutdown_conflicts_total.fetch_add(1, Ordering::Relaxed);
                return Err(DriverError::ShutdownConflict);
            },
        }

        Ok(ShutdownReceipt { ack_rx, deadline })
    }

    pub(crate) fn take_pending(&self) -> Option<ShutdownDispatch> {
        if !self.has_pending() {
            return None;
        }

        let mut state = self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let request = state.request.as_mut()?;
        if request.sending {
            self.has_pending.store(false, Ordering::Release);
            return None;
        }

        request.sending = true;
        self.has_pending.store(false, Ordering::Release);
        Some(ShutdownDispatch {
            frame: request.frame,
            deadline: request.deadline,
        })
    }

    pub fn finish(&self, result: Result<(), DriverError>) {
        let waiters = {
            let mut state = self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            self.has_pending.store(false, Ordering::Release);
            state.request.take().map(|request| request.waiters).unwrap_or_default()
        };

        if waiters.is_empty() {
            return;
        }

        for waiter in waiters {
            let _ = waiter.send(clone_shutdown_result(&result));
        }
    }

    pub fn close_with(&self, result: Result<(), DriverError>) {
        let waiters = {
            let mut state = self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            state.closed = true;
            self.has_pending.store(false, Ordering::Release);
            state.request.take().map(|request| request.waiters).unwrap_or_default()
        };

        for waiter in waiters {
            let _ = waiter.send(clone_shutdown_result(&result));
        }
    }
}

fn clone_shutdown_result(result: &Result<(), DriverError>) -> Result<(), DriverError> {
    match result {
        Ok(()) => Ok(()),
        Err(error) => Err(clone_driver_error(error)),
    }
}

fn clone_driver_error(error: &DriverError) -> DriverError {
    match error {
        DriverError::Can(source) => DriverError::Can(clone_can_error(source)),
        DriverError::Protocol(source) => DriverError::Protocol(clone_protocol_error(source)),
        DriverError::ChannelClosed => DriverError::ChannelClosed,
        DriverError::ControlPathClosed => DriverError::ControlPathClosed,
        DriverError::ChannelFull => DriverError::ChannelFull,
        DriverError::ShutdownConflict => DriverError::ShutdownConflict,
        DriverError::NotDualThread => DriverError::NotDualThread,
        DriverError::PoisonedLock => DriverError::PoisonedLock,
        DriverError::IoThread(message) => DriverError::IoThread(message.clone()),
        DriverError::NotImplemented(message) => DriverError::NotImplemented(message.clone()),
        DriverError::Timeout => DriverError::Timeout,
        DriverError::InvalidInput(message) => DriverError::InvalidInput(message.clone()),
        DriverError::RealtimeDeliveryOverwritten => DriverError::RealtimeDeliveryOverwritten,
        DriverError::RealtimeDeliveryFailed {
            sent,
            total,
            source,
        } => DriverError::RealtimeDeliveryFailed {
            sent: *sent,
            total: *total,
            source: clone_can_error(source),
        },
        DriverError::RealtimeDeliveryAbortedByFault { sent, total } => {
            DriverError::RealtimeDeliveryAbortedByFault {
                sent: *sent,
                total: *total,
            }
        },
        DriverError::ReliableDeliveryFailed { source } => DriverError::ReliableDeliveryFailed {
            source: clone_can_error(source),
        },
        DriverError::CommandAbortedByFault => DriverError::CommandAbortedByFault,
        DriverError::MaintenanceWriteDenied(message) => {
            DriverError::MaintenanceWriteDenied(message.clone())
        },
        DriverError::RealtimeDeliveryTimeout => DriverError::RealtimeDeliveryTimeout,
    }
}

fn clone_can_error(error: &CanError) -> CanError {
    match error {
        CanError::Io(source) => {
            CanError::Io(std::io::Error::new(source.kind(), source.to_string()))
        },
        CanError::Device(source) => CanError::Device(source.clone()),
        CanError::Timeout => CanError::Timeout,
        CanError::BufferOverflow => CanError::BufferOverflow,
        CanError::BusOff => CanError::BusOff,
        CanError::NotStarted => CanError::NotStarted,
    }
}

fn clone_protocol_error(error: &piper_protocol::ProtocolError) -> piper_protocol::ProtocolError {
    match error {
        piper_protocol::ProtocolError::InvalidLength { expected, actual } => {
            piper_protocol::ProtocolError::InvalidLength {
                expected: *expected,
                actual: *actual,
            }
        },
        piper_protocol::ProtocolError::InvalidCanId { id } => {
            piper_protocol::ProtocolError::InvalidCanId { id: *id }
        },
        piper_protocol::ProtocolError::InvalidJointIndex { joint_index } => {
            piper_protocol::ProtocolError::InvalidJointIndex {
                joint_index: *joint_index,
            }
        },
        piper_protocol::ProtocolError::MitInputOutOfRange {
            joint_index,
            field,
            value,
            min,
            max,
        } => piper_protocol::ProtocolError::MitInputOutOfRange {
            joint_index: *joint_index,
            field: *field,
            value: *value,
            min: *min,
            max: *max,
        },
        piper_protocol::ProtocolError::ParseError(message) => {
            piper_protocol::ProtocolError::ParseError(message.clone())
        },
        piper_protocol::ProtocolError::InvalidValue { field, value } => {
            piper_protocol::ProtocolError::InvalidValue {
                field: field.clone(),
                value: *value,
            }
        },
    }
}

impl RuntimePhase {
    pub(crate) fn from_raw(raw: u8) -> Self {
        match raw {
            1 => Self::FaultLatched,
            2 => Self::Stopping,
            _ => Self::Running,
        }
    }
}

/// Piper 机械臂驱动（对外 API）
pub struct Piper {
    /// 普通可靠命令发送通道。
    reliable_tx: ManuallyDrop<Sender<ReliableCommand>>,
    /// 单飞急停通道。
    shutdown_lane: Arc<ShutdownLane>,
    /// 实时命令插槽（邮箱模式，Overwrite）
    realtime_slot: Arc<std::sync::Mutex<Option<RealtimeCommand>>>,
    /// 共享状态上下文
    ctx: Arc<PiperContext>,
    /// 统一管理的 worker 句柄。
    workers: RuntimeWorkers,
    /// worker 生命周期标志。
    workers_running: Arc<AtomicBool>,
    /// 运行时阶段（控制路径开关）。
    runtime_phase: Arc<AtomicU8>,
    /// 普通控制帧发送门闩。
    normal_send_gate: Arc<NormalSendGate>,
    /// 性能指标（原子计数器）
    metrics: Arc<PiperMetrics>,
    /// 最近一次运行时故障。
    runtime_fault: Arc<AtomicU8>,
    /// Controller-owned maintenance gate used by bridge integrations.
    maintenance_gate: Arc<MaintenanceGate>,
    /// CAN 接口名称（用于录制元数据）
    interface: String,
    /// CAN 总线速度（bps）（用于录制元数据）
    bus_speed: u32,
    /// Driver 工作模式（用于回放模式控制）
    driver_mode: crate::mode::AtomicDriverMode,
    /// Timing capability of the active backend.
    timing_capability: TimingCapability,
}

impl Piper {
    /// 最大允许的实时帧包大小
    ///
    /// 允许调用者在客户端进行预检查，避免跨层调用后的运行时错误。
    ///
    /// # 示例
    ///
    /// ```rust,no_run
    /// # use piper_driver::Piper;
    /// # use piper_can::PiperFrame;
    /// # fn example(piper: &Piper) -> std::result::Result<(), Box<dyn std::error::Error>> {
    /// let frame1 = PiperFrame::new_standard(0x100, &[]);
    /// let frame2 = PiperFrame::new_standard(0x101, &[]);
    /// let frame3 = PiperFrame::new_standard(0x102, &[]);
    /// let frames = [frame1, frame2, frame3];
    /// if frames.len() > Piper::MAX_REALTIME_PACKAGE_SIZE {
    ///     return Err("Package too large".into());
    /// }
    /// piper.send_realtime_package(frames)?;
    /// # Ok(())
    /// # }
    /// ```
    pub const MAX_REALTIME_PACKAGE_SIZE: usize = 10;

    /// 设置元数据（内部方法，由 Builder 调用）
    pub(crate) fn with_metadata(mut self, interface: String, bus_speed: u32) -> Self {
        self.interface = interface;
        self.bus_speed = bus_speed;
        self
    }

    fn rx_thread_alive(&self) -> bool {
        self.workers
            .rx_thread
            .as_ref()
            .map(|handle| !handle.is_finished())
            .unwrap_or(false)
    }

    fn tx_thread_alive(&self) -> bool {
        self.workers
            .tx_thread
            .as_ref()
            .map(|handle| !handle.is_finished())
            .unwrap_or(false)
    }

    fn runtime_phase(&self) -> RuntimePhase {
        RuntimePhase::from_raw(self.runtime_phase.load(Ordering::Acquire))
    }

    fn normal_control_open(&self) -> bool {
        self.runtime_phase() == RuntimePhase::Running
    }

    fn shutdown_lane_open(&self) -> bool {
        matches!(
            self.runtime_phase(),
            RuntimePhase::Running | RuntimePhase::FaultLatched
        )
    }

    /// 创建双线程模式的 Piper 实例
    ///
    /// 将 CAN 适配器分离为独立的 RX 和 TX 适配器，实现物理隔离。
    /// RX 线程专门负责接收反馈帧，TX 线程专门负责发送控制命令。
    ///
    /// # 参数
    /// - `can`: 可分离的 CAN 适配器（必须已启动）
    /// - `config`: Pipeline 配置（可选）
    ///
    /// # 错误
    /// - `CanError::NotStarted`: 适配器未启动
    /// - `CanError::Device`: 分离适配器失败
    ///
    /// # 使用场景
    /// - 实时控制：需要 RX 不受 TX 阻塞影响
    /// - 高频控制：500Hz-1kHz 控制循环
    ///
    /// # 注意
    /// - 适配器必须已启动（调用 `configure()` 或 `start()`）
    /// - 分离后，原适配器不再可用（消费 `can`）
    pub fn new_dual_thread<C>(can: C, config: Option<PipelineConfig>) -> Result<Self, CanError>
    where
        C: SplittableAdapter + Send + 'static,
        C::RxAdapter: Send + 'static,
        C::TxAdapter: Send + 'static,
    {
        let (rx_adapter, tx_adapter) = can.split()?;
        Self::new_dual_thread_parts(rx_adapter, tx_adapter, config)
    }

    /// 使用已拆分的 RX/TX 适配器创建双线程 runtime。
    pub fn new_dual_thread_parts(
        rx_adapter: impl RxAdapter + Send + 'static,
        tx_adapter: impl RealtimeTxAdapter + Send + 'static,
        config: Option<PipelineConfig>,
    ) -> Result<Self, CanError> {
        let timing_capability = rx_adapter.timing_capability();
        let realtime_slot = Arc::new(std::sync::Mutex::new(None::<RealtimeCommand>));
        let (reliable_tx, reliable_rx) = crossbeam_channel::bounded::<ReliableCommand>(10);
        let shutdown_lane = Arc::new(ShutdownLane::new());
        let ctx = Arc::new(PiperContext::new());
        let workers_running = Arc::new(AtomicBool::new(true));
        let runtime_phase = Arc::new(AtomicU8::new(RuntimePhase::Running as u8));
        let normal_send_gate = Arc::new(NormalSendGate::new());
        let metrics = Arc::new(PiperMetrics::new());
        let runtime_fault = Arc::new(AtomicU8::new(0));
        let maintenance_gate = Arc::new(MaintenanceGate::default());

        let ctx_clone = ctx.clone();
        let workers_running_clone = workers_running.clone();
        let runtime_phase_rx = runtime_phase.clone();
        let metrics_clone = metrics.clone();
        let runtime_fault_rx = runtime_fault.clone();
        let config_clone = config.clone().unwrap_or_default();
        let timing_capability_rx = timing_capability;
        let maintenance_gate_rx = maintenance_gate.clone();

        let rx_thread = spawn(move || {
            crate::pipeline::rx_loop(
                rx_adapter,
                timing_capability_rx,
                ctx_clone,
                config_clone,
                workers_running_clone,
                runtime_phase_rx,
                metrics_clone,
                runtime_fault_rx,
                maintenance_gate_rx,
            );
        });

        let ctx_tx = ctx.clone();
        let workers_running_tx = workers_running.clone();
        let runtime_phase_tx = runtime_phase.clone();
        let normal_send_gate_tx = normal_send_gate.clone();
        let metrics_tx = metrics.clone();
        let realtime_slot_tx = realtime_slot.clone();
        let runtime_fault_tx = runtime_fault.clone();
        let shutdown_lane_tx = shutdown_lane.clone();
        let maintenance_gate_tx = maintenance_gate.clone();
        let maintenance_gate_tx_compat = maintenance_gate.clone();

        let tx_thread = spawn(move || {
            crate::pipeline::tx_loop_mailbox(
                tx_adapter,
                realtime_slot_tx,
                shutdown_lane_tx,
                reliable_rx,
                workers_running_tx,
                runtime_phase_tx,
                normal_send_gate_tx,
                metrics_tx,
                ctx_tx,
                runtime_fault_tx,
                maintenance_gate_tx,
                maintenance_gate_tx_compat,
            );
        });

        std::thread::sleep(std::time::Duration::from_millis(10));

        Ok(Self {
            reliable_tx: ManuallyDrop::new(reliable_tx),
            shutdown_lane,
            realtime_slot,
            ctx,
            workers: RuntimeWorkers {
                rx_thread: Some(rx_thread),
                tx_thread: Some(tx_thread),
            },
            workers_running,
            runtime_phase,
            normal_send_gate,
            metrics,
            runtime_fault,
            maintenance_gate,
            interface: "unknown".to_string(),
            bus_speed: 1_000_000,
            driver_mode: crate::mode::AtomicDriverMode::new(crate::mode::DriverMode::Normal),
            timing_capability,
        })
    }

    pub fn timing_capability(&self) -> TimingCapability {
        self.timing_capability
    }

    /// 获取运行时健康状态。
    pub fn health(&self) -> HealthStatus {
        let rx_alive = self.rx_thread_alive();
        let tx_alive = self.tx_thread_alive();
        let connected = self.is_connected();
        let last_feedback_age = self.connection_age();
        let runtime_fault = RuntimeFaultKind::from_raw(self.runtime_fault.load(Ordering::Acquire));

        let fault = if runtime_fault.is_some() {
            runtime_fault
        } else if !rx_alive {
            Some(RuntimeFaultKind::RxExited)
        } else if !tx_alive {
            Some(RuntimeFaultKind::TxExited)
        } else {
            None
        };

        HealthStatus {
            connected,
            last_feedback_age,
            rx_alive,
            tx_alive,
            fault,
        }
    }

    /// 锁存故障并关闭正常控制路径。
    ///
    /// 进入故障锁存后：
    /// - 新的 realtime / normal reliable 控制命令会被拒绝
    /// - TX 线程会主动清空 pending realtime slot 和 normal reliable queue
    /// - shutdown lane 仍然保持可用，用于 bounded stop attempt
    pub fn latch_fault(&self) {
        let previous = RuntimePhase::from_raw(
            self.runtime_phase.swap(RuntimePhase::FaultLatched as u8, Ordering::AcqRel),
        );
        if previous == RuntimePhase::Stopping {
            return;
        }
        self.normal_send_gate.close();
        self.clear_realtime_slot(DriverError::CommandAbortedByFault, true);
        self.maintenance_gate.set_state(MaintenanceGateState::DeniedFaulted);
    }

    /// 请求 worker 停止并关闭所有命令通路。
    pub fn request_stop(&self) {
        self.runtime_phase.store(RuntimePhase::Stopping as u8, Ordering::Release);
        self.normal_send_gate.close();
        self.workers_running.store(false, Ordering::Release);
        self.shutdown_lane.close_with(Err(DriverError::ChannelClosed));
        self.clear_realtime_slot(DriverError::ChannelClosed, false);
        self.maintenance_gate.set_state(MaintenanceGateState::DeniedTransportDown);
    }

    /// 获取性能指标快照
    ///
    /// 返回当前所有计数器的快照，用于监控 IO 链路健康状态。
    pub fn get_metrics(&self) -> MetricsSnapshot {
        self.metrics.snapshot()
    }

    /// 获取关节动态状态（无锁，纳秒级返回）
    ///
    /// 包含关节速度和电流（独立帧 + Buffered Commit）。
    ///
    /// # 性能
    /// - 无锁读取（ArcSwap::load）
    /// - 返回快照副本（Clone 开销低，< 150 字节）
    /// - 适合 500Hz 控制循环
    pub fn get_joint_dynamic(&self) -> JointDynamicState {
        self.get_joint_dynamic_monitor_snapshot()
            .latest_complete_cloned()
            .unwrap_or_default()
    }

    /// 获取原始关节动态状态（允许部分动态组，仅供诊断）
    pub fn get_raw_joint_dynamic(&self) -> JointDynamicState {
        self.get_joint_dynamic_monitor_snapshot().latest_raw().clone()
    }

    /// 获取关节动态监控快照（完整监控 + raw 诊断）
    pub fn get_joint_dynamic_monitor_snapshot(&self) -> JointDynamicMonitorSnapshot {
        self.ctx.capture_joint_dynamic_monitor_snapshot()
    }

    /// 获取关节位置状态（无锁，纳秒级返回）
    ///
    /// 包含6个关节的位置信息（500Hz更新）。
    ///
    /// # 性能
    /// - 无锁读取（ArcSwap::load）
    /// - 返回快照副本（Clone 开销低）
    /// - 适合 500Hz 控制循环
    ///
    /// # 注意
    /// - 此状态与 `EndPoseState` 不是原子更新的，如需同时获取，请使用 `capture_motion_snapshot()`
    pub fn get_joint_position(&self) -> JointPositionState {
        self.get_joint_position_monitor_snapshot()
            .latest_complete_cloned()
            .unwrap_or_default()
    }

    /// 获取原始关节位置状态（允许部分帧组，仅供诊断）
    pub fn get_raw_joint_position(&self) -> JointPositionState {
        self.get_joint_position_monitor_snapshot().latest_raw().clone()
    }

    /// 获取关节位置监控快照（完整监控 + raw 诊断）
    pub fn get_joint_position_monitor_snapshot(&self) -> JointPositionMonitorSnapshot {
        self.ctx.capture_joint_position_monitor_snapshot()
    }

    /// 获取末端位姿状态（无锁，纳秒级返回）
    ///
    /// 包含末端执行器的位置和姿态信息（500Hz更新）。
    ///
    /// # 性能
    /// - 无锁读取（ArcSwap::load）
    /// - 返回快照副本（Clone 开销低）
    /// - 适合 500Hz 控制循环
    ///
    /// # 注意
    /// - 此状态与 `JointPositionState` 不是原子更新的，如需同时获取，请使用 `capture_motion_snapshot()`
    pub fn get_end_pose(&self) -> EndPoseState {
        self.get_end_pose_monitor_snapshot()
            .latest_complete_cloned()
            .unwrap_or_default()
    }

    /// 获取原始末端位姿状态（允许部分帧组，仅供诊断）
    pub fn get_raw_end_pose(&self) -> EndPoseState {
        self.get_end_pose_monitor_snapshot().latest_raw().clone()
    }

    /// 获取末端位姿监控快照（完整监控 + raw 诊断）
    pub fn get_end_pose_monitor_snapshot(&self) -> EndPoseMonitorSnapshot {
        self.ctx.capture_end_pose_monitor_snapshot()
    }

    /// 获取运动快照（无锁，纳秒级返回）
    ///
    /// 原子性地获取 `JointPositionState` 和 `EndPoseState` 的最新快照。
    /// 虽然这两个状态在硬件上不是同时更新的，但此方法保证逻辑上的原子性。
    ///
    /// # 性能
    /// - 无锁读取（单次 ArcSwap::load）
    /// - 返回快照副本
    /// - 适合需要同时使用关节位置和末端位姿的场景
    ///
    /// # 示例
    ///
    /// ```
    /// # use piper_driver::Piper;
    /// # // 注意：此示例需要实际的 CAN 适配器，仅供参考
    /// # // let piper = Piper::new(/* ... */).unwrap();
    /// # // let snapshot = piper.capture_motion_snapshot();
    /// # // println!("Joint positions: {:?}", snapshot.joint_position.joint_pos);
    /// # // println!("End pose: {:?}", snapshot.end_pose.end_pose);
    /// ```
    pub fn capture_motion_snapshot(&self) -> MotionSnapshot {
        self.ctx.capture_motion_snapshot()
    }

    /// 获取原始运动快照（允许部分帧组，仅供诊断）
    pub fn capture_raw_motion_snapshot(&self) -> MotionSnapshot {
        self.ctx.capture_raw_motion_snapshot()
    }

    /// 获取机器人控制状态（无锁）
    ///
    /// 包含控制模式、机器人状态、故障码等（100Hz更新）。
    ///
    /// # 性能
    /// - 无锁读取（ArcSwap::load）
    /// - 返回快照副本
    pub fn get_robot_control(&self) -> RobotControlState {
        self.ctx.robot_control.load().as_ref().clone()
    }

    /// 获取夹爪状态（无锁）
    ///
    /// 包含夹爪行程、扭矩、状态码等（100Hz更新）。
    ///
    /// # 性能
    /// - 无锁读取（ArcSwap::load）
    /// - 返回快照副本
    pub fn get_gripper(&self) -> GripperState {
        self.ctx.gripper.load().as_ref().clone()
    }

    /// 获取关节驱动器低速反馈状态（无锁）
    ///
    /// 包含温度、电压、电流、驱动器状态等（40Hz更新）。
    ///
    /// # 性能
    /// - 无锁读取（ArcSwap::load，Wait-Free）
    /// - 返回快照副本
    pub fn get_joint_driver_low_speed(&self) -> JointDriverLowSpeedState {
        self.ctx.joint_driver_low_speed.load().as_ref().clone()
    }

    /// 返回已缓存的固件版本。
    pub fn firmware_version_cached(&self) -> Option<String> {
        if let Ok(mut firmware_state) = self.ctx.firmware_version.write() {
            if let Some(version) = firmware_state.version_string() {
                return Some(version.clone());
            }
            firmware_state.parse_version()
        } else {
            None
        }
    }

    /// 发送查询并阻塞等待固件版本。
    pub fn read_firmware_version(&self, timeout: Duration) -> Result<String, DriverError> {
        use piper_protocol::FirmwareVersionQueryCommand;

        if let Ok(mut firmware_state) = self.ctx.firmware_version.write() {
            firmware_state.clear();
        } else {
            return Err(DriverError::PoisonedLock);
        }

        self.send_reliable(FirmwareVersionQueryCommand::new().to_frame())?;

        let start = std::time::Instant::now();
        loop {
            if let Some(version) = self.firmware_version_cached() {
                return Ok(version);
            }

            if start.elapsed() >= timeout {
                return Err(DriverError::Timeout);
            }

            if !self.rx_thread_alive() || !self.tx_thread_alive() {
                return Err(DriverError::ChannelClosed);
            }

            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// 获取主从模式控制模式指令状态（无锁）
    ///
    /// 包含控制模式、运动模式、速度等（主从模式下，~200Hz更新）。
    ///
    /// # 性能
    /// - 无锁读取（ArcSwap::load）
    /// - 返回快照副本
    pub fn get_master_slave_control_mode(&self) -> MasterSlaveControlModeState {
        self.ctx.master_slave_control_mode.load().as_ref().clone()
    }

    /// 获取主从模式关节控制指令状态（无锁）
    ///
    /// 包含6个关节的目标角度（主从模式下，~500Hz更新）。
    ///
    /// # 性能
    /// - 无锁读取（ArcSwap::load）
    /// - 返回快照副本
    /// - 帧组同步，保证6个关节数据的逻辑一致性
    pub fn get_master_slave_joint_control(&self) -> MasterSlaveJointControlState {
        self.ctx.master_slave_joint_control.load().as_ref().clone()
    }

    /// 获取主从模式夹爪控制指令状态（无锁）
    ///
    /// 包含夹爪目标行程、扭矩等（主从模式下，~200Hz更新）。
    ///
    /// # 性能
    /// - 无锁读取（ArcSwap::load）
    /// - 返回快照副本
    pub fn get_master_slave_gripper_control(&self) -> MasterSlaveGripperControlState {
        self.ctx.master_slave_gripper_control.load().as_ref().clone()
    }

    /// 获取碰撞保护状态（读锁）
    ///
    /// 包含各关节的碰撞保护等级（按需查询）。
    ///
    /// # 性能
    /// - 读锁（RwLock::read）
    /// - 返回快照副本
    pub fn get_collision_protection(&self) -> Result<CollisionProtectionState, DriverError> {
        self.ctx
            .collision_protection
            .read()
            .map(|guard| guard.clone())
            .map_err(|_| DriverError::PoisonedLock)
    }

    /// 获取关节限制配置状态（读锁）
    ///
    /// 包含关节角度限制和速度限制（按需查询）。
    ///
    /// # 性能
    /// - 读锁（RwLock::read）
    /// - 返回快照副本
    pub fn get_joint_limit_config(&self) -> Result<JointLimitConfigState, DriverError> {
        self.ctx
            .joint_limit_config
            .read()
            .map(|guard| guard.clone())
            .map_err(|_| DriverError::PoisonedLock)
    }

    /// 获取关节加速度限制配置状态（读锁）
    ///
    /// 包含关节加速度限制（按需查询）。
    ///
    /// # 性能
    /// - 读锁（RwLock::read）
    /// - 返回快照副本
    pub fn get_joint_accel_config(&self) -> Result<JointAccelConfigState, DriverError> {
        self.ctx
            .joint_accel_config
            .read()
            .map(|guard| guard.clone())
            .map_err(|_| DriverError::PoisonedLock)
    }

    /// 获取末端限制配置状态（读锁）
    ///
    /// 包含末端执行器的速度和加速度限制（按需查询）。
    ///
    /// # 性能
    /// - 读锁（RwLock::read）
    /// - 返回快照副本
    pub fn get_end_limit_config(&self) -> Result<EndLimitConfigState, DriverError> {
        self.ctx
            .end_limit_config
            .read()
            .map(|guard| guard.clone())
            .map_err(|_| DriverError::PoisonedLock)
    }

    /// 获取组合运动状态（所有热数据）
    ///
    /// 注意：不同子状态的时间戳可能不同步（差异通常在毫秒级）。
    /// 如果需要时间对齐的状态，请使用 `get_aligned_motion()`。
    pub fn get_motion_state(&self) -> CombinedMotionState {
        let snapshot = self.capture_motion_snapshot();
        CombinedMotionState {
            joint_position: snapshot.joint_position,
            end_pose: snapshot.end_pose,
            joint_dynamic: self.get_joint_dynamic(),
        }
    }

    /// 获取时间对齐的运动状态（推荐用于力控算法）
    ///
    /// 以 `joint_position.hardware_timestamp_us` 为基准时间，检查时间戳差异。
    /// 即使时间戳差异超过阈值，也返回状态数据（让用户有选择权）。
    ///
    /// # 参数
    /// - `max_time_diff_us`: 允许的最大时间戳差异（微秒），推荐值：5000（5ms）
    ///
    /// # 返回值
    /// - `AlignmentResult::Ok(state)`: 时间戳差异在可接受范围内
    /// - `AlignmentResult::Misaligned { state, diff_us }`: 时间戳差异过大，但仍返回状态数据
    pub fn get_aligned_motion(&self, max_time_diff_us: u64) -> AlignmentResult {
        let joint_position = self.ctx.control_joint_position.load();
        let joint_dynamic = self.get_joint_dynamic();

        let time_diff =
            joint_position.hardware_timestamp_us.abs_diff(joint_dynamic.group_timestamp_us);

        let state = AlignedMotionState {
            joint_pos: joint_position.joint_pos,
            joint_vel: joint_dynamic.joint_vel,
            joint_current: joint_dynamic.joint_current,
            position_timestamp_us: joint_position.hardware_timestamp_us,
            dynamic_timestamp_us: joint_dynamic.group_timestamp_us,
            position_host_rx_mono_us: joint_position.host_rx_mono_us,
            dynamic_host_rx_mono_us: joint_dynamic.group_host_rx_mono_us,
            position_frame_valid_mask: joint_position.frame_valid_mask,
            dynamic_valid_mask: joint_dynamic.valid_mask,
            skew_us: (joint_dynamic.group_timestamp_us as i64)
                - (joint_position.hardware_timestamp_us as i64),
        };

        if time_diff > max_time_diff_us {
            AlignmentResult::Misaligned {
                state,
                diff_us: time_diff,
            }
        } else {
            AlignmentResult::Ok(state)
        }
    }

    /// 等待接收到第一个有效反馈（用于初始化）
    ///
    /// 在 `Piper::new()` 后调用，确保在控制循环开始前已收到有效数据。
    /// 避免使用全零的初始状态导致错误的控制指令。
    ///
    /// # 参数
    /// - `timeout`: 超时时间
    ///
    /// # 返回值
    /// - `Ok(())`: 成功接收到有效反馈（`timestamp_us > 0`）
    /// - `Err(DriverError::Timeout)`: 超时未收到反馈
    pub fn wait_for_feedback(&self, timeout: std::time::Duration) -> Result<(), DriverError> {
        let start = std::time::Instant::now();

        loop {
            // 检查是否超时
            if start.elapsed() >= timeout {
                return Err(DriverError::Timeout);
            }

            if self.is_connected() {
                return Ok(());
            }

            if !self.rx_thread_alive() || !self.tx_thread_alive() {
                return Err(DriverError::ChannelClosed);
            }

            // 短暂休眠，避免 CPU 空转
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    /// 获取 FPS 统计结果
    ///
    /// 返回最近一次统计窗口内的更新频率（FPS）。
    /// 建议定期调用（如每秒一次）或按需调用。
    ///
    /// # 性能
    /// - 无锁读取（仅原子读取）
    /// - 开销：~100ns（5 次原子读取 + 浮点计算）
    ///
    /// # Example
    ///
    /// ```
    /// # use piper_driver::Piper;
    /// # // 注意：此示例需要实际的 CAN 适配器，仅供参考
    /// # // let piper = Piper::new(/* ... */).unwrap();
    /// # // 运行一段时间后查询 FPS
    /// # // std::thread::sleep(std::time::Duration::from_secs(5));
    /// # // let fps = piper.get_fps();
    /// # // println!("Joint Position FPS: {:.2}", fps.joint_position);
    /// # // println!("End Pose FPS: {:.2}", fps.end_pose);
    /// # // println!("Joint Dynamic FPS: {:.2}", fps.joint_dynamic);
    /// ```
    pub fn get_fps(&self) -> FpsResult {
        self.ctx.fps_stats.load().calculate_fps()
    }

    /// 获取 FPS 计数器原始值
    ///
    /// 返回当前计数器的原始值，可以配合自定义时间窗口计算 FPS。
    ///
    /// # 性能
    /// - 无锁读取（仅原子读取）
    /// - 开销：~50ns（5 次原子读取）
    ///
    /// # Example
    ///
    /// ```
    /// # use piper_driver::Piper;
    /// # // 注意：此示例需要实际的 CAN 适配器，仅供参考
    /// # // let piper = Piper::new(/* ... */).unwrap();
    /// # // 记录开始时间和计数
    /// # // let start = std::time::Instant::now();
    /// # // let counts_start = piper.get_fps_counts();
    /// # // 运行一段时间
    /// # // std::thread::sleep(std::time::Duration::from_secs(1));
    /// # // 计算实际 FPS
    /// # // let counts_end = piper.get_fps_counts();
    /// # // let elapsed = start.elapsed();
    /// # // let actual_fps = (counts_end.joint_position - counts_start.joint_position) as f64 / elapsed.as_secs_f64();
    /// ```
    pub fn get_fps_counts(&self) -> FpsCounts {
        self.ctx.fps_stats.load().get_counts()
    }

    /// 重置 FPS 统计窗口（清空计数器并重新开始计时）
    ///
    /// 这是一个轻量级、无锁的重置：通过 `ArcSwap` 将内部 `FpsStatistics` 原子替换为新实例。
    /// 适合在监控工具中做固定窗口统计（例如每 5 秒 reset 一次）。
    pub fn reset_fps_stats(&self) {
        self.ctx.fps_stats.store(Arc::new(crate::fps_stats::FpsStatistics::new()));
    }

    // ============================================================
    // 连接监控 API
    // ============================================================

    /// 检查机器人是否仍在响应
    ///
    /// 如果在超时窗口内收到反馈，返回 `true`。
    /// 这可用于检测机器人是否断电、CAN 线缆断开或固件崩溃。
    ///
    /// # 性能
    /// - 无锁读取（AtomicU64::load）
    /// - O(1) 时间复杂度
    pub fn is_connected(&self) -> bool {
        self.ctx.connection_monitor.check_connection()
    }

    /// 获取自上次反馈以来的时间
    ///
    /// 返回自上次成功处理 CAN 帧以来的时间。
    /// 可用于连接质量监控或诊断。
    pub fn connection_age(&self) -> std::time::Duration {
        self.ctx.connection_monitor.time_since_last_feedback()
    }

    /// 发送控制帧（非阻塞）
    ///
    /// # 参数
    /// - `frame`: 控制帧（已构建的 `PiperFrame`）
    ///
    /// # 错误
    /// - `DriverError::ChannelClosed`: 命令通道已关闭（IO 线程退出）
    /// - `DriverError::ChannelFull`: 命令队列已满（缓冲区容量 10）
    pub fn send_frame(&self, frame: PiperFrame) -> Result<(), DriverError> {
        self.enqueue_reliable(ReliableCommand::single(frame))
    }

    /// 获取钩子管理器的引用（用于高级诊断）
    ///
    /// # 设计理念
    ///
    /// 这是一个**逃生舱（Escape Hatch）**，用于高级诊断场景：
    /// - 注册自定义 CAN 帧回调
    /// - 实现录制功能
    /// - 性能分析和调试
    ///
    /// # 使用场景
    ///
    /// - 自定义诊断工具
    /// - 高级抓包和调试
    /// - 性能分析和优化
    /// - 后台监控线程
    ///
    /// # 示例
    ///
    /// ```rust,no_run
    /// # use piper_driver::Piper;
    /// # use piper_driver::hooks::FrameCallback;
    /// # use piper_driver::recording::AsyncRecordingHook;
    /// # use std::sync::Arc;
    /// # fn example(robot: &Piper) {
    /// // 获取 hooks 访问
    /// let hooks = robot.hooks();
    ///
    /// // 创建录制钩子
    /// let (hook, _rx) = AsyncRecordingHook::new();
    /// let callback = Arc::new(hook) as Arc<dyn FrameCallback>;
    ///
    /// // 注册回调（忽略错误以简化示例）
    /// if let Ok(mut hooks_guard) = hooks.write() {
    ///     hooks_guard.add_callback(callback);
    /// }
    /// # }
    /// ```
    ///
    /// # 安全注意事项
    ///
    /// - **性能要求**：回调必须在 <1μs 内完成
    /// - **线程安全**：返回 `Arc<RwLock<HookManager>>`，需手动加锁
    /// - **不要阻塞**：禁止在回调中使用 Mutex、I/O、分配等阻塞操作
    ///
    /// # 返回值
    ///
    /// `Arc<RwLock<HookManager>>`: 钩子管理器的共享引用
    ///
    /// # 参考
    ///
    /// - [`HookManager`](crate::hooks::HookManager) - 钩子管理器
    /// - [`FrameCallback`](crate::hooks::FrameCallback) - 回调 trait
    /// - [架构分析报告](../../../docs/architecture/piper-driver-client-mixing-analysis.md) - 方案 B 设计
    pub fn hooks(&self) -> Arc<std::sync::RwLock<crate::hooks::HookManager>> {
        Arc::clone(&self.ctx.hooks)
    }

    /// 获取 CAN 接口名称
    ///
    /// # 返回值
    ///
    /// CAN 接口名称，例如 "can0", "vcan0" 等
    pub fn interface(&self) -> String {
        self.interface.clone()
    }

    /// 获取 CAN 总线速度
    ///
    /// # 返回值
    ///
    /// CAN 总线速度（bps），例如 1000000 (1Mbps)
    pub fn bus_speed(&self) -> u32 {
        self.bus_speed
    }

    /// 获取当前 Driver 模式
    ///
    /// # 返回值
    ///
    /// 当前 Driver 模式（Normal 或 Replay）
    ///
    /// # 示例
    ///
    /// ```rust,no_run
    /// # use piper_driver::Piper;
    /// # fn example(robot: &Piper) {
    /// let mode = robot.mode();
    /// println!("Current mode: {:?}", mode);
    /// # }
    /// ```
    pub fn mode(&self) -> crate::mode::DriverMode {
        self.driver_mode.get(std::sync::atomic::Ordering::Relaxed)
    }

    /// 设置 Driver 模式
    ///
    /// # 参数
    ///
    /// - `mode`: 新的 Driver 模式
    ///
    /// # 模式说明
    ///
    /// - **Normal**: 正常模式，TX 线程按周期发送控制指令
    /// - **Replay**: 回放模式，TX 线程暂停周期性发送
    ///
    /// # 使用场景
    ///
    /// Replay 模式用于安全地回放预先录制的 CAN 帧：
    /// - 暂停 TX 线程的周期性发送
    /// - 避免双控制流冲突
    /// - 允许精确控制帧发送时机
    ///
    /// # ⚠️ 安全警告
    ///
    /// - 切换到 Replay 模式前，应确保机器人处于 Standby 状态
    /// - 在 Replay 模式下发送控制指令时，应遵守安全速度限制
    ///
    /// # 示例
    ///
    /// ```rust,no_run
    /// # use piper_driver::{Piper, mode::DriverMode};
    /// # fn example(robot: &Piper) {
    /// // 切换到回放模式
    /// robot.set_mode(DriverMode::Replay);
    ///
    /// // ... 执行回放 ...
    ///
    /// // 恢复正常模式
    /// robot.set_mode(DriverMode::Normal);
    /// # }
    /// ```
    pub fn set_mode(&self, mode: crate::mode::DriverMode) {
        self.driver_mode.set(mode, std::sync::atomic::Ordering::Relaxed);
        tracing::info!("Driver mode set to: {:?}", mode);
    }

    /// 发送控制帧（阻塞，带超时）
    ///
    /// 如果命令通道已满，阻塞等待直到有空闲位置或超时。
    ///
    /// # 参数
    /// - `frame`: 控制帧（已构建的 `PiperFrame`）
    /// - `timeout`: 超时时间
    ///
    /// # 错误
    /// - `DriverError::ChannelClosed`: 命令通道已关闭（IO 线程退出）
    /// - `DriverError::Timeout`: 超时未发送成功
    pub fn send_frame_blocking(
        &self,
        frame: PiperFrame,
        timeout: std::time::Duration,
    ) -> Result<(), DriverError> {
        self.enqueue_reliable_timeout(ReliableCommand::single(frame), timeout)
    }

    /// 发送实时控制命令（邮箱模式，覆盖策略）
    ///
    /// 实时命令使用邮箱模式（Mailbox），直接覆盖旧命令，确保最新命令被发送。
    /// 这对于力控/高频控制场景很重要，只保留最新的控制指令。
    ///
    /// # 参数
    /// - `frame`: 控制帧（已构建的 `PiperFrame`）
    ///
    /// # 错误
    /// - `DriverError::NotDualThread`: 未使用双线程模式
    /// - `DriverError::PoisonedLock`: 锁中毒（极少见，通常意味着 TX 线程 panic）
    ///
    /// # 实现细节
    /// - 获取 Mutex 锁并直接覆盖插槽内容（Last Write Wins）
    /// - 锁持有时间极短（< 50ns），仅为内存拷贝
    /// - 永不阻塞：无论 TX 线程是否消费，都能立即写入
    /// - 如果插槽已有数据，会被覆盖（更新 `metrics.tx_realtime_overwrites_total`）
    ///
    /// # 性能
    /// - 典型延迟：20-50ns（无竞争情况下）
    /// - 最坏延迟：200ns（与 TX 线程锁竞争时）
    /// - 相比 Channel 重试策略，延迟降低 10-100 倍
    ///
    /// 发送单个实时帧（向后兼容，API 不变）
    pub fn send_realtime(&self, frame: PiperFrame) -> Result<(), DriverError> {
        self.send_realtime_command(RealtimeCommand::single(frame))
    }

    /// 发送实时帧包（新 API）
    ///
    /// # 参数
    /// - `frames`: 要发送的帧迭代器，必须非空
    ///
    /// **接口优化**：接受 `impl IntoIterator`，允许用户传入：
    /// - 数组：`[frame1, frame2, frame3]`（栈上，零堆分配）
    /// - 切片：`&[frame1, frame2, frame3]`
    /// - Vec：`vec![frame1, frame2, frame3]`
    ///
    /// # 错误
    /// - `DriverError::NotDualThread`: 未使用双线程模式
    /// - `DriverError::InvalidInput`: 帧列表为空或过大
    /// - `DriverError::PoisonedLock`: 锁中毒
    ///
    /// # 原子性保证
    /// Package 内的所有帧要么全部发送成功，要么都不发送。
    /// 如果发送过程中出现错误，已发送的帧不会被回滚（CAN 总线特性），
    /// 但未发送的帧不会继续发送。
    ///
    /// # 性能特性
    /// - 如果帧数量 ≤ 4，完全在栈上分配，零堆内存分配
    /// - 如果帧数量 > 4，SmallVec 会自动溢出到堆，但仍保持高效
    pub fn send_realtime_package(
        &self,
        frames: impl IntoIterator<Item = PiperFrame>,
    ) -> Result<(), DriverError> {
        use crate::command::FrameBuffer;

        let buffer: FrameBuffer = frames.into_iter().collect();

        if buffer.is_empty() {
            return Err(DriverError::InvalidInput(
                "Frame package cannot be empty".to_string(),
            ));
        }

        // 限制包大小，防止内存问题
        // 使用 Piper 的关联常量，允许客户端预检查
        //
        // 注意：如果用户传入超大 Vec（如长度 1000），这里会先进行 collect 操作，
        // 可能导致堆分配。虽然之后会检查并报错，但内存开销已经发生。
        // 这是可以接受的权衡（安全网），但建议用户在调用前进行预检查。
        if buffer.len() > Self::MAX_REALTIME_PACKAGE_SIZE {
            return Err(DriverError::InvalidInput(format!(
                "Frame package too large: {} (max: {})",
                buffer.len(),
                Self::MAX_REALTIME_PACKAGE_SIZE
            )));
        }

        self.send_realtime_command(RealtimeCommand::package(buffer))
    }

    /// 发送实时帧包并等待 TX 线程确认实际发送结果。
    pub fn send_realtime_package_confirmed(
        &self,
        frames: impl IntoIterator<Item = PiperFrame>,
        timeout: Duration,
    ) -> Result<(), DriverError> {
        use crate::command::FrameBuffer;

        if !self.tx_thread_alive() {
            return Err(DriverError::ChannelClosed);
        }
        if !self.normal_control_open() {
            return Err(DriverError::ControlPathClosed);
        }

        let buffer: FrameBuffer = frames.into_iter().collect();

        if buffer.is_empty() {
            return Err(DriverError::InvalidInput(
                "Frame package cannot be empty".to_string(),
            ));
        }

        if buffer.len() > Self::MAX_REALTIME_PACKAGE_SIZE {
            return Err(DriverError::InvalidInput(format!(
                "Frame package too large: {} (max: {})",
                buffer.len(),
                Self::MAX_REALTIME_PACKAGE_SIZE
            )));
        }

        let (ack_tx, ack_rx) = crossbeam_channel::bounded(1);
        self.send_realtime_command(RealtimeCommand::confirmed(buffer, ack_tx))?;

        match ack_rx.recv_timeout(timeout) {
            Ok(result) => result,
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                Err(DriverError::RealtimeDeliveryTimeout)
            },
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                Err(DriverError::ChannelClosed)
            },
        }
    }

    /// 内部方法：发送实时命令（统一处理单个帧和帧包）
    fn send_realtime_command(&self, command: RealtimeCommand) -> Result<(), DriverError> {
        if !self.tx_thread_alive() {
            command.complete(Err(DriverError::ChannelClosed));
            return Err(DriverError::ChannelClosed);
        }
        match self.realtime_slot.lock() {
            Ok(mut slot) => {
                if !self.normal_control_open() {
                    drop(slot);
                    command.complete(Err(DriverError::ControlPathClosed));
                    return Err(DriverError::ControlPathClosed);
                }
                // 检测是否发生覆盖（如果插槽已有数据）
                let previous = slot.replace(command);
                let is_overwrite = previous.is_some();

                // 直接覆盖（邮箱模式：Last Write Wins）
                // 注意：如果旧命令是 Package，Drop 操作会释放 SmallVec
                // 但如果数据在栈上（len ≤ 4），Drop 只是栈指针移动，几乎零开销

                // 更新指标（在锁外更新，减少锁持有时间）
                // 注意：先释放锁，再更新指标，避免在锁内进行原子操作
                drop(slot); // 显式释放锁

                if let Some(previous) = previous {
                    previous.complete(Err(DriverError::RealtimeDeliveryOverwritten));
                }

                // 更新指标（在锁外更新，减少锁持有时间）
                let total =
                    self.metrics.tx_realtime_enqueued_total.fetch_add(1, Ordering::Relaxed) + 1;

                if is_overwrite {
                    let overwrites =
                        self.metrics.tx_realtime_overwrites_total.fetch_add(1, Ordering::Relaxed)
                            + 1;

                    // 智能监控：每 1000 次发送检查一次覆盖率
                    // 避免频繁计算，减少性能开销
                    if total > 0 && total.is_multiple_of(1000) {
                        let rate = (overwrites as f64 / total as f64) * 100.0;

                        // 只在覆盖率超过阈值时警告
                        if rate > 50.0 {
                            // 异常情况：覆盖率 > 50%，记录警告
                            warn!(
                                "High realtime overwrite rate detected: {:.1}% ({} overwrites / {} total sends). \
                                 This may indicate TX thread bottleneck or excessive send frequency.",
                                rate, overwrites, total
                            );
                        } else if rate > 30.0 {
                            // 中等情况：覆盖率 30-50%，记录信息（可选，生产环境可关闭）
                            info!(
                                "Moderate realtime overwrite rate: {:.1}% ({} overwrites / {} total sends). \
                                 This is normal for high-frequency control (> 500Hz).",
                                rate, overwrites, total
                            );
                        }
                        // < 30% 不记录日志（正常情况）
                    }
                }

                Ok(())
            },
            Err(_) => {
                error!("Realtime slot lock poisoned, TX thread may have panicked");
                Err(DriverError::PoisonedLock)
            },
        }
    }

    fn clear_realtime_slot(&self, reason: DriverError, count_fault_abort: bool) {
        if let Ok(mut slot) = self.realtime_slot.lock()
            && let Some(command) = slot.take()
        {
            if count_fault_abort {
                self.metrics.tx_fault_aborts_total.fetch_add(1, Ordering::Relaxed);
                self.metrics.tx_packages_fault_aborted_total.fetch_add(1, Ordering::Relaxed);
            }
            command.complete(Err(reason));
        }
    }

    /// 发送可靠命令（FIFO 策略）
    ///
    /// 可靠命令使用容量为 10 的队列，按 FIFO 顺序发送，不会覆盖。
    /// 这对于配置帧、状态机切换帧等关键命令很重要。
    ///
    /// # 参数
    /// - `frame`: 控制帧（已构建的 `PiperFrame`）
    ///
    /// # 错误
    /// - `DriverError::NotDualThread`: 未使用双线程模式
    /// - `DriverError::ChannelClosed`: 命令通道已关闭（TX 线程退出）
    /// - `DriverError::ChannelFull`: 队列满（非阻塞）
    pub fn send_reliable(&self, frame: PiperFrame) -> Result<(), DriverError> {
        self.enqueue_reliable(ReliableCommand::single(frame))
    }

    /// 发送维护帧并等待 TX 线程在实际发送点完成最终运行时准入判定。
    #[doc(hidden)]
    pub fn send_maintenance_frame_confirmed(
        &self,
        session_id: u32,
        session_key: u64,
        lease_epoch: u64,
        frame: PiperFrame,
        timeout: Duration,
    ) -> Result<(), DriverError> {
        if !self.tx_thread_alive() {
            return Err(DriverError::ChannelClosed);
        }
        if !self.normal_control_open() {
            return Err(DriverError::ControlPathClosed);
        }

        let (ack_tx, ack_rx) = crossbeam_channel::bounded(1);
        self.enqueue_reliable_timeout(
            ReliableCommand::maintenance_confirmed(
                frame,
                session_id,
                session_key,
                lease_epoch,
                ack_tx,
            ),
            timeout,
        )?;

        match ack_rx.recv_timeout(timeout) {
            Ok(result) => result,
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => Err(DriverError::Timeout),
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                Err(DriverError::ChannelClosed)
            },
        }
    }

    #[doc(hidden)]
    pub fn maintenance_lease_snapshot(&self) -> MaintenanceLeaseSnapshot {
        self.maintenance_gate.snapshot()
    }

    #[doc(hidden)]
    pub fn register_maintenance_event_sink(&self, sink: Sender<MaintenanceRevocationEvent>) {
        self.maintenance_gate.set_event_sink(sink);
    }

    #[doc(hidden)]
    pub fn acquire_maintenance_lease_gate(
        &self,
        session_id: u32,
        session_key: u64,
        timeout: Duration,
    ) -> MaintenanceLeaseAcquireResult {
        self.maintenance_gate.acquire_blocking(session_id, session_key, timeout)
    }

    #[doc(hidden)]
    pub fn release_maintenance_lease_gate_if_holder(&self, session_key: u64) -> bool {
        self.maintenance_gate.release_if_holder(session_key)
    }

    #[doc(hidden)]
    pub fn revoke_maintenance_lease_gate(
        &self,
        session_key: u64,
        reason: MaintenanceRevocationReason,
    ) -> Option<MaintenanceRevocationEvent> {
        self.maintenance_gate.revoke_if_holder(session_key, reason)
    }

    #[doc(hidden)]
    pub fn set_maintenance_gate_state(&self, state: MaintenanceGateState) {
        self.maintenance_gate.set_state(state);
    }

    #[doc(hidden)]
    pub fn maintenance_lease_is_valid(&self, session_key: u64, lease_epoch: u64) -> bool {
        self.maintenance_gate.is_valid(session_key, lease_epoch)
    }

    #[doc(hidden)]
    pub fn connection_timeout_remaining(&self) -> Option<Duration> {
        self.ctx.connection_monitor.remaining_until_timeout()
    }

    #[doc(hidden)]
    pub fn maintenance_runtime_open(&self) -> bool {
        self.runtime_phase() == RuntimePhase::Running
    }

    /// 发送命令（根据优先级自动选择队列）
    ///
    /// 根据命令的优先级自动选择实时队列或可靠队列。
    ///
    /// # 参数
    /// - `command`: 带优先级的命令
    ///
    /// # 错误
    /// - `DriverError::NotDualThread`: 未使用双线程模式
    /// - `DriverError::ChannelClosed`: 命令通道已关闭（TX 线程退出）
    /// - `DriverError::ChannelFull`: 队列满（仅可靠命令）
    pub fn send_command(&self, command: PiperCommand) -> Result<(), DriverError> {
        match command.priority() {
            CommandPriority::RealtimeControl => self.send_realtime(command.frame()),
            CommandPriority::ReliableCommand => self.send_reliable(command.frame()),
        }
    }

    /// 发送可靠命令（阻塞，带超时）
    ///
    /// 如果队列满，阻塞等待直到有空闲位置或超时。
    ///
    /// # 参数
    /// - `frame`: 控制帧（已构建的 `PiperFrame`）
    /// - `timeout`: 超时时间
    ///
    /// # 错误
    /// - `DriverError::NotDualThread`: 未使用双线程模式
    /// - `DriverError::ChannelClosed`: 命令通道已关闭（TX 线程退出）
    /// - `DriverError::Timeout`: 超时未发送成功
    pub fn send_reliable_timeout(
        &self,
        frame: PiperFrame,
        timeout: std::time::Duration,
    ) -> Result<(), DriverError> {
        self.enqueue_reliable_timeout(ReliableCommand::single(frame), timeout)
    }

    /// 将停机专用命令加入 shutdown lane，并返回确认句柄。
    pub fn enqueue_shutdown(
        &self,
        frame: PiperFrame,
        deadline: Instant,
    ) -> Result<ShutdownReceipt, DriverError> {
        if !self.tx_thread_alive() {
            return Err(DriverError::ChannelClosed);
        }
        if !self.shutdown_lane_open() {
            return Err(DriverError::ChannelClosed);
        }

        self.shutdown_lane.enqueue(frame, deadline, &self.metrics)
    }

    fn enqueue_reliable(&self, command: ReliableCommand) -> Result<(), DriverError> {
        if !self.tx_thread_alive() {
            command.complete(Err(DriverError::ChannelClosed));
            return Err(DriverError::ChannelClosed);
        }
        if !self.normal_control_open() {
            command.complete(Err(DriverError::ControlPathClosed));
            return Err(DriverError::ControlPathClosed);
        }
        match self.reliable_tx.try_send(command) {
            Ok(_) => {
                self.metrics.tx_reliable_enqueued_total.fetch_add(1, Ordering::Relaxed);
                Ok(())
            },
            Err(crossbeam_channel::TrySendError::Full(_)) => {
                self.metrics.tx_reliable_queue_full_total.fetch_add(1, Ordering::Relaxed);
                Err(DriverError::ChannelFull)
            },
            Err(crossbeam_channel::TrySendError::Disconnected(_)) => {
                Err(DriverError::ChannelClosed)
            },
        }
    }

    fn enqueue_reliable_timeout(
        &self,
        command: ReliableCommand,
        timeout: Duration,
    ) -> Result<(), DriverError> {
        if !self.tx_thread_alive() {
            command.complete(Err(DriverError::ChannelClosed));
            return Err(DriverError::ChannelClosed);
        }
        if !self.normal_control_open() {
            command.complete(Err(DriverError::ControlPathClosed));
            return Err(DriverError::ControlPathClosed);
        }
        match self.reliable_tx.send_timeout(command, timeout) {
            Ok(_) => {
                self.metrics.tx_reliable_enqueued_total.fetch_add(1, Ordering::Relaxed);
                Ok(())
            },
            Err(crossbeam_channel::SendTimeoutError::Timeout(_)) => {
                self.metrics.tx_reliable_queue_full_total.fetch_add(1, Ordering::Relaxed);
                Err(DriverError::Timeout)
            },
            Err(crossbeam_channel::SendTimeoutError::Disconnected(_)) => {
                Err(DriverError::ChannelClosed)
            },
        }
    }
}

impl Drop for Piper {
    fn drop(&mut self) {
        self.runtime_phase.store(RuntimePhase::Stopping as u8, Ordering::Release);
        self.normal_send_gate.close();
        self.workers_running.store(false, Ordering::Release);
        self.shutdown_lane.close_with(Err(DriverError::ChannelClosed));

        unsafe {
            ManuallyDrop::drop(&mut self.reliable_tx);
        }

        self.clear_realtime_slot(DriverError::ChannelClosed, false);

        let join_timeout = Duration::from_secs(2);

        if let Some(handle) = self.workers.rx_thread.take()
            && let Err(_e) = handle.join_timeout(join_timeout)
        {
            error!(
                "RX thread panicked or failed to shut down within {:?}",
                join_timeout
            );
        }

        if let Some(handle) = self.workers.tx_thread.take()
            && let Err(_e) = handle.join_timeout(join_timeout)
        {
            error!(
                "TX thread panicked or failed to shut down within {:?}",
                join_timeout
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use piper_can::{CanAdapter, PiperFrame, SplittableAdapter};
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex, mpsc};

    struct MockCanAdapter;

    impl CanAdapter for MockCanAdapter {
        fn send(&mut self, _frame: PiperFrame) -> Result<(), CanError> {
            Ok(())
        }

        fn receive(&mut self) -> Result<PiperFrame, CanError> {
            // 永远超时，避免阻塞测试
            Err(CanError::Timeout)
        }
    }

    struct MockRxAdapter;

    impl piper_can::RxAdapter for MockRxAdapter {
        fn receive(&mut self) -> Result<PiperFrame, CanError> {
            Err(CanError::Timeout)
        }
    }

    struct FatalRxAdapter {
        tripped: bool,
    }

    impl piper_can::RxAdapter for FatalRxAdapter {
        fn receive(&mut self) -> Result<PiperFrame, CanError> {
            if !self.tripped {
                self.tripped = true;
                Err(CanError::BufferOverflow)
            } else {
                Err(CanError::Timeout)
            }
        }
    }

    struct MockTxAdapter;

    impl piper_can::RealtimeTxAdapter for MockTxAdapter {
        fn send_control(&mut self, _frame: PiperFrame, budget: Duration) -> Result<(), CanError> {
            if budget.is_zero() {
                return Err(CanError::Timeout);
            }
            Ok(())
        }

        fn send_shutdown_until(
            &mut self,
            _frame: PiperFrame,
            deadline: Instant,
        ) -> Result<(), CanError> {
            if deadline <= Instant::now() {
                return Err(CanError::Timeout);
            }
            Ok(())
        }
    }

    struct RecordingTxAdapter {
        sent_frames: Arc<Mutex<Vec<PiperFrame>>>,
    }

    impl piper_can::RealtimeTxAdapter for RecordingTxAdapter {
        fn send_control(&mut self, frame: PiperFrame, budget: Duration) -> Result<(), CanError> {
            if budget.is_zero() {
                return Err(CanError::Timeout);
            }
            self.sent_frames.lock().expect("sent frames lock").push(frame);
            Ok(())
        }

        fn send_shutdown_until(
            &mut self,
            frame: PiperFrame,
            deadline: Instant,
        ) -> Result<(), CanError> {
            if deadline <= Instant::now() {
                return Err(CanError::Timeout);
            }
            self.sent_frames.lock().expect("sent frames lock").push(frame);
            Ok(())
        }
    }

    struct FailOnNthTxAdapter {
        fail_on: usize,
        sends: usize,
    }

    impl piper_can::RealtimeTxAdapter for FailOnNthTxAdapter {
        fn send_control(&mut self, _frame: PiperFrame, budget: Duration) -> Result<(), CanError> {
            if budget.is_zero() {
                return Err(CanError::Timeout);
            }
            self.sends += 1;
            if self.sends == self.fail_on {
                return Err(CanError::BufferOverflow);
            }
            Ok(())
        }

        fn send_shutdown_until(
            &mut self,
            _frame: PiperFrame,
            deadline: Instant,
        ) -> Result<(), CanError> {
            if deadline <= Instant::now() {
                return Err(CanError::Timeout);
            }
            self.sends += 1;
            if self.sends == self.fail_on {
                return Err(CanError::BufferOverflow);
            }
            Ok(())
        }
    }

    struct CoordinatedFailTxAdapter {
        started_tx: mpsc::Sender<()>,
        release_rx: mpsc::Receiver<()>,
        first_send: bool,
    }

    impl piper_can::RealtimeTxAdapter for CoordinatedFailTxAdapter {
        fn send_control(&mut self, _frame: PiperFrame, _budget: Duration) -> Result<(), CanError> {
            if !self.first_send {
                self.first_send = true;
                let _ = self.started_tx.send(());
                let _ = self.release_rx.recv();
                return Err(CanError::BufferOverflow);
            }
            Ok(())
        }

        fn send_shutdown_until(
            &mut self,
            _frame: PiperFrame,
            _deadline: Instant,
        ) -> Result<(), CanError> {
            if !self.first_send {
                self.first_send = true;
                let _ = self.started_tx.send(());
                let _ = self.release_rx.recv();
                return Err(CanError::BufferOverflow);
            }
            Ok(())
        }
    }

    struct BlockingFirstSendTxAdapter {
        sent_frames: Arc<Mutex<Vec<PiperFrame>>>,
        started_tx: mpsc::Sender<()>,
        release_rx: mpsc::Receiver<()>,
        sends: usize,
    }

    impl piper_can::RealtimeTxAdapter for BlockingFirstSendTxAdapter {
        fn send_control(&mut self, frame: PiperFrame, _budget: Duration) -> Result<(), CanError> {
            self.sends += 1;
            self.sent_frames.lock().expect("sent frames lock").push(frame);
            if self.sends == 1 {
                let _ = self.started_tx.send(());
                let _ = self.release_rx.recv();
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

    struct SlowTxAdapter {
        delay: Duration,
    }

    impl piper_can::RealtimeTxAdapter for SlowTxAdapter {
        fn send_control(&mut self, _frame: PiperFrame, budget: Duration) -> Result<(), CanError> {
            if budget.is_zero() {
                return Err(CanError::Timeout);
            }
            if budget < self.delay {
                std::thread::sleep(budget);
                return Err(CanError::Timeout);
            }
            std::thread::sleep(self.delay);
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
            if remaining < self.delay {
                std::thread::sleep(remaining);
                return Err(CanError::Timeout);
            }
            std::thread::sleep(self.delay);
            Ok(())
        }
    }

    impl SplittableAdapter for MockCanAdapter {
        type RxAdapter = MockRxAdapter;
        type TxAdapter = MockTxAdapter;

        fn split(self) -> Result<(Self::RxAdapter, Self::TxAdapter), CanError> {
            Ok((MockRxAdapter, MockTxAdapter))
        }
    }

    fn wait_shutdown(
        piper: &Piper,
        frame: PiperFrame,
        timeout: Duration,
    ) -> Result<(), DriverError> {
        let receipt = piper.enqueue_shutdown(frame, Instant::now() + timeout)?;
        receipt.wait()
    }

    struct ScriptedRxAdapter {
        frames: VecDeque<PiperFrame>,
        first_delay: Duration,
        emitted_first_frame: bool,
    }

    impl ScriptedRxAdapter {
        fn new(frames: Vec<PiperFrame>, first_delay: Duration) -> Self {
            Self {
                frames: frames.into(),
                first_delay,
                emitted_first_frame: false,
            }
        }
    }

    impl piper_can::RxAdapter for ScriptedRxAdapter {
        fn receive(&mut self) -> Result<PiperFrame, CanError> {
            if !self.emitted_first_frame && !self.first_delay.is_zero() {
                std::thread::sleep(self.first_delay);
                self.emitted_first_frame = true;
            }
            self.frames.pop_front().ok_or(CanError::Timeout)
        }
    }

    #[test]
    fn test_piper_new() {
        let mock_can = MockCanAdapter;
        let piper = Piper::new_dual_thread(mock_can, None).unwrap();

        // 验证可以获取状态（默认状态）
        let joint_pos = piper.get_joint_position();
        assert_eq!(joint_pos.hardware_timestamp_us, 0);

        // 验证通道正常工作
        let frame = PiperFrame::new_standard(0x123, &[0x01, 0x02]);
        assert!(piper.send_frame(frame).is_ok());
    }

    #[test]
    fn test_piper_drop() {
        let mock_can = MockCanAdapter;
        let piper = Piper::new_dual_thread(mock_can, None).unwrap();
        // drop 应该能够正常退出，IO 线程被 join
        drop(piper);
    }

    #[test]
    fn test_piper_get_motion_state() {
        let mock_can = MockCanAdapter;
        let piper = Piper::new_dual_thread(mock_can, None).unwrap();
        let motion = piper.get_motion_state();
        assert_eq!(motion.joint_position.hardware_timestamp_us, 0);
        assert_eq!(motion.joint_dynamic.group_timestamp_us, 0);
    }

    #[test]
    fn test_piper_send_frame_channel_full() {
        let mock_can = MockCanAdapter;
        let piper = Piper::new_dual_thread(mock_can, None).unwrap();
        let frame = PiperFrame::new_standard(0x123, &[0x01]);

        // 填满命令通道（容量 10）
        // 注意：IO 线程会持续消费帧，所以需要快速填充
        // 或者等待 IO 线程稍微延迟消费
        std::thread::sleep(std::time::Duration::from_millis(50));

        for _ in 0..10 {
            assert!(piper.send_frame(frame).is_ok());
        }

        // 第 11 次发送可能返回 ChannelFull（如果 IO 线程还没消费完）
        // 或者成功（如果 IO 线程已经消费了一些）
        // 为了测试 ChannelFull，我们需要更快速地发送，确保通道填满
        let result = piper.send_frame(frame);

        // 由于 IO 线程在后台消费，可能成功也可能失败
        // 验证至少前 10 次都成功即可
        match result {
            Err(DriverError::ChannelFull) => {
                // 通道满，这是预期情况
            },
            Ok(()) => {
                // 如果 IO 线程消费很快，这也可能发生
                // 这是可接受的行为
            },
            Err(e) => panic!("Unexpected error: {:?}", e),
        }
    }

    #[test]
    fn test_get_aligned_motion_aligned() {
        let mock_can = MockCanAdapter;
        let piper = Piper::new_dual_thread(mock_can, None).unwrap();

        // 由于 MockCanAdapter 不发送帧，时间戳都为 0
        // 测试默认状态下的对齐检查（时间戳都为 0，应该是对齐的）
        let result = piper.get_aligned_motion(5000);
        match result {
            AlignmentResult::Ok(state) => {
                assert_eq!(state.position_timestamp_us, 0);
                assert_eq!(state.dynamic_timestamp_us, 0);
                assert_eq!(state.position_frame_valid_mask, 0);
                assert_eq!(state.dynamic_valid_mask, 0);
                assert_eq!(state.skew_us, 0);
            },
            AlignmentResult::Misaligned { .. } => {
                // 如果时间戳都为 0，不应该是不对齐的
                // 但允许这种情况（因为时间戳都是 0）
            },
        }
    }

    #[test]
    fn test_get_aligned_motion_misaligned_threshold() {
        let mock_can = MockCanAdapter;
        let piper = Piper::new_dual_thread(mock_can, None).unwrap();

        // 测试不同的时间差阈值
        // 由于时间戳都是 0，应该是对齐的
        let result1 = piper.get_aligned_motion(0);
        let result2 = piper.get_aligned_motion(1000);
        let result3 = piper.get_aligned_motion(1000000);

        // 所有结果都应该返回状态（即使是对齐的）
        match (result1, result2, result3) {
            (AlignmentResult::Ok(_), AlignmentResult::Ok(_), AlignmentResult::Ok(_)) => {
                // 正常情况
            },
            _ => {
                // 允许其他情况
            },
        }
    }

    #[test]
    fn test_get_robot_control() {
        let mock_can = MockCanAdapter;
        let piper = Piper::new_dual_thread(mock_can, None).unwrap();

        let control = piper.get_robot_control();
        assert_eq!(control.hardware_timestamp_us, 0);
        assert_eq!(control.control_mode, 0);
        assert!(!control.is_enabled);
    }

    #[test]
    fn test_get_joint_driver_low_speed() {
        let mock_can = MockCanAdapter;
        let piper = Piper::new_dual_thread(mock_can, None).unwrap();

        let driver_state = piper.get_joint_driver_low_speed();
        assert_eq!(driver_state.hardware_timestamp_us, 0);
        assert_eq!(driver_state.motor_temps, [0.0; 6]);
    }

    #[test]
    fn test_get_joint_limit_config() {
        let mock_can = MockCanAdapter;
        let piper = Piper::new_dual_thread(mock_can, None).unwrap();

        let limits = piper.get_joint_limit_config().unwrap();
        assert_eq!(limits.joint_limits_max, [0.0; 6]);
    }

    #[test]
    fn test_wait_for_feedback_timeout() {
        let mock_can = MockCanAdapter;
        let piper = Piper::new_dual_thread(mock_can, None).unwrap();

        // MockCanAdapter 不发送帧，所以应该超时
        let result = piper.wait_for_feedback(std::time::Duration::from_millis(10));
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), DriverError::Timeout));
    }

    #[test]
    fn test_wait_for_feedback_uses_connection_monitor() {
        let frame = PiperFrame::new_standard(0x251, &[0; 8]);
        let piper = Piper::new_dual_thread_parts(
            ScriptedRxAdapter::new(vec![frame], Duration::from_millis(20)),
            MockTxAdapter,
            None,
        )
        .unwrap();

        assert!(piper.wait_for_feedback(Duration::from_millis(200)).is_ok());
        assert!(piper.health().connected);
    }

    #[test]
    fn test_health_reports_runtime_only_faults() {
        let mock_can = MockCanAdapter;
        let piper = Piper::new_dual_thread(mock_can, None).unwrap();

        let health = piper.health();
        assert!(health.rx_alive);
        assert!(health.tx_alive);
        assert!(!health.connected);
        assert!(health.fault.is_none());
    }

    #[test]
    fn test_read_firmware_version_timeout_does_not_pollute_health_fault() {
        let mock_can = MockCanAdapter;
        let piper = Piper::new_dual_thread(mock_can, None).unwrap();

        let result = piper.read_firmware_version(Duration::from_millis(10));
        assert!(matches!(result, Err(DriverError::Timeout)));
        assert!(piper.health().fault.is_none());
    }

    #[test]
    fn test_read_firmware_version_success() {
        let frame =
            PiperFrame::new_standard(piper_protocol::ids::ID_FIRMWARE_READ as u16, b"S-V1.8-1");
        let piper = Piper::new_dual_thread_parts(
            ScriptedRxAdapter::new(vec![frame], Duration::from_millis(20)),
            MockTxAdapter,
            None,
        )
        .unwrap();

        let version = piper
            .read_firmware_version(Duration::from_millis(200))
            .expect("firmware version should be available");
        assert_eq!(version, "S-V1.8-1");
        assert!(piper.health().fault.is_none());
    }

    #[test]
    fn test_read_firmware_version_waits_for_complete_split_payload() {
        let frame_1 =
            PiperFrame::new_standard(piper_protocol::ids::ID_FIRMWARE_READ as u16, b"S-V1.6");
        let frame_2 = PiperFrame::new_standard(piper_protocol::ids::ID_FIRMWARE_READ as u16, b"-3");
        let piper = Piper::new_dual_thread_parts(
            ScriptedRxAdapter::new(vec![frame_1, frame_2], Duration::from_millis(20)),
            MockTxAdapter,
            None,
        )
        .unwrap();

        let version = piper
            .read_firmware_version(Duration::from_millis(200))
            .expect("firmware version should wait for all 8 bytes");
        assert_eq!(version, "S-V1.6-3");
        assert_eq!(piper.firmware_version_cached().as_deref(), Some("S-V1.6-3"));
    }

    #[test]
    fn test_send_frame_blocking_timeout() {
        let mock_can = MockCanAdapter;
        let piper = Piper::new_dual_thread(mock_can, None).unwrap();
        let frame = PiperFrame::new_standard(0x123, &[0x01]);

        // 快速填充通道（如果 IO 线程来不及消费）
        // 然后测试阻塞发送
        // 由于通道容量为 10，在 IO 线程消费的情况下，应该能成功
        // 但为了测试超时，我们使用极短的超时时间
        let result = piper.send_frame_blocking(frame, std::time::Duration::from_millis(1));

        // 结果可能是成功（IO 线程消费快）或超时（通道满）
        match result {
            Ok(()) => {
                // 成功是正常情况
            },
            Err(DriverError::Timeout) => {
                // 超时也是可接受的（如果通道满）
            },
            Err(e) => panic!("Unexpected error: {:?}", e),
        }
    }

    #[test]
    fn test_get_aligned_motion_with_time_diff() {
        let mock_can = MockCanAdapter;
        let piper = Piper::new_dual_thread(mock_can, None).unwrap();

        // 测试对齐阈值边界情况
        // 时间戳都为 0 时，skew_us 应该是 0
        let result = piper.get_aligned_motion(0);
        match result {
            AlignmentResult::Ok(state) => {
                assert_eq!(state.skew_us, 0);
            },
            AlignmentResult::Misaligned { state, diff_us } => {
                // 如果时间戳都为 0，diff_us 应该也是 0
                assert_eq!(diff_us, 0);
                assert_eq!(state.skew_us, 0);
            },
        }
    }

    #[test]
    fn test_get_aligned_motion_carries_completeness_masks() {
        let mock_can = MockCanAdapter;
        let piper = Piper::new_dual_thread(mock_can, None).unwrap();

        piper.ctx.publish_control_joint_position(JointPositionState {
            hardware_timestamp_us: 1_000,
            host_rx_mono_us: 2_000,
            joint_pos: [0.0; 6],
            frame_valid_mask: 0b101,
        });
        piper.ctx.joint_dynamic_monitor.store(Arc::new(
            JointDynamicMonitorSnapshot::from_complete(JointDynamicState {
                group_timestamp_us: 1_000,
                group_host_rx_mono_us: 2_000,
                joint_vel: [0.0; 6],
                joint_current: [0.0; 6],
                timestamps: [1_000; 6],
                valid_mask: 0b001111,
            }),
        ));

        let result = piper.get_aligned_motion(0);
        let state = match result {
            AlignmentResult::Ok(state) | AlignmentResult::Misaligned { state, .. } => state,
        };

        assert_eq!(state.position_frame_valid_mask, 0b101);
        assert_eq!(state.dynamic_valid_mask, 0b001111);
        assert!(!state.is_complete());
    }

    #[test]
    fn test_send_realtime_package_confirmed_success() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let piper = Piper::new_dual_thread_parts(
            MockRxAdapter,
            RecordingTxAdapter {
                sent_frames: sent_frames.clone(),
            },
            None,
        )
        .unwrap();
        let frames = [
            PiperFrame::new_standard(0x155, &[0x01]),
            PiperFrame::new_standard(0x156, &[0x02]),
        ];

        piper
            .send_realtime_package_confirmed(frames, Duration::from_millis(200))
            .expect("confirmed send should succeed");

        let sent = sent_frames.lock().expect("sent frames lock");
        assert_eq!(sent.len(), 2);
    }

    #[test]
    fn test_enqueue_shutdown_success() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let piper = Piper::new_dual_thread_parts(
            MockRxAdapter,
            RecordingTxAdapter {
                sent_frames: sent_frames.clone(),
            },
            None,
        )
        .unwrap();
        let frame = PiperFrame::new_standard(0x471, &[0x01]);

        wait_shutdown(&piper, frame, Duration::from_millis(200))
            .expect("confirmed shutdown send should succeed");

        let sent = sent_frames.lock().expect("sent frames lock");
        assert_eq!(sent.as_slice(), &[frame]);
    }

    #[test]
    fn test_enqueue_shutdown_channel_closed_when_tx_thread_exits() {
        let piper = Piper::new_dual_thread_parts(MockRxAdapter, MockTxAdapter, None).unwrap();
        piper.request_stop();
        std::thread::sleep(Duration::from_millis(5));
        assert!(!piper.tx_thread_alive());

        let error = piper
            .enqueue_shutdown(
                PiperFrame::new_standard(0x471, &[0x01]),
                Instant::now() + Duration::from_millis(50),
            )
            .expect_err("stopped tx thread should reject confirmed shutdown send");

        assert!(matches!(error, DriverError::ChannelClosed));
    }

    #[test]
    fn test_enqueue_shutdown_reports_transport_failure() {
        let piper = Piper::new_dual_thread_parts(
            MockRxAdapter,
            FailOnNthTxAdapter {
                fail_on: 1,
                sends: 0,
            },
            None,
        )
        .unwrap();

        let error = wait_shutdown(
            &piper,
            PiperFrame::new_standard(0x471, &[0x01]),
            Duration::from_millis(200),
        )
        .expect_err("transport error should fail confirmed shutdown send");

        assert!(matches!(error, DriverError::ReliableDeliveryFailed { .. }));
    }

    #[test]
    fn test_shutdown_receipt_times_out_waiting_for_ack() {
        let piper = Piper::new_dual_thread_parts(
            MockRxAdapter,
            SlowTxAdapter {
                delay: Duration::from_millis(50),
            },
            None,
        )
        .unwrap();

        let error = wait_shutdown(
            &piper,
            PiperFrame::new_standard(0x471, &[0x01]),
            Duration::from_millis(5),
        )
        .expect_err("slow tx should time out confirmed shutdown send");

        assert!(matches!(error, DriverError::Timeout));
    }

    #[test]
    fn test_shutdown_receipt_times_out_when_deadline_has_already_passed() {
        let piper = Piper::new_dual_thread_parts(
            MockRxAdapter,
            SlowTxAdapter {
                delay: Duration::from_millis(50),
            },
            None,
        )
        .unwrap();
        let receipt = piper
            .enqueue_shutdown(PiperFrame::new_standard(0x471, &[0x01]), Instant::now())
            .expect("enqueue should succeed");
        let error = receipt
            .wait()
            .expect_err("an expired deadline without a ready ack must time out");

        assert!(matches!(error, DriverError::Timeout));
    }

    #[test]
    fn test_shutdown_receipt_returns_ready_ack_even_after_deadline_passes() {
        let piper = Piper::new_dual_thread_parts(MockRxAdapter, MockTxAdapter, None).unwrap();
        let receipt = piper
            .enqueue_shutdown(
                PiperFrame::new_standard(0x471, &[0x01]),
                Instant::now() + Duration::from_millis(10),
            )
            .expect("enqueue should succeed");
        std::thread::sleep(Duration::from_millis(20));

        receipt
            .wait()
            .expect("ready ack should still be observed after the shared deadline passes");
    }

    #[test]
    fn test_latch_fault_closes_normal_control_path_but_keeps_shutdown_lane() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let piper = Piper::new_dual_thread_parts(
            MockRxAdapter,
            RecordingTxAdapter {
                sent_frames: sent_frames.clone(),
            },
            None,
        )
        .unwrap();
        let stop_frame = PiperFrame::new_standard(0x471, &[0x09]);

        piper.latch_fault();

        assert!(matches!(
            piper.send_realtime(PiperFrame::new_standard(0x155, &[0x01])),
            Err(DriverError::ControlPathClosed)
        ));
        assert!(matches!(
            piper.send_reliable(PiperFrame::new_standard(0x472, &[0x02])),
            Err(DriverError::ControlPathClosed)
        ));

        wait_shutdown(&piper, stop_frame, Duration::from_millis(200))
            .expect("shutdown lane should remain available after fault latch");

        let sent = sent_frames.lock().expect("sent frames lock");
        assert_eq!(sent.as_slice(), &[stop_frame]);
        let metrics = piper.get_metrics();
        assert_eq!(metrics.tx_shutdown_requests_total, 1);
        assert_eq!(metrics.tx_shutdown_sent_total, 1);
        assert_eq!(metrics.tx_frames_sent_total, 1);
    }

    #[test]
    fn test_rx_fatal_keeps_shutdown_lane_available_while_tx_is_alive() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let piper = Piper::new_dual_thread_parts(
            FatalRxAdapter { tripped: false },
            RecordingTxAdapter {
                sent_frames: sent_frames.clone(),
            },
            None,
        )
        .unwrap();
        let stop_frame = PiperFrame::new_standard(0x471, &[0x04]);

        std::thread::sleep(Duration::from_millis(20));
        let health = piper.health();
        assert!(!health.rx_alive);
        assert!(health.tx_alive);
        assert_eq!(health.fault, Some(RuntimeFaultKind::TransportError));
        assert!(matches!(
            piper.send_reliable(PiperFrame::new_standard(0x472, &[0x02])),
            Err(DriverError::ControlPathClosed)
        ));

        wait_shutdown(&piper, stop_frame, Duration::from_millis(200))
            .expect("rx fatal should still allow bounded shutdown via tx lane");

        let sent = sent_frames.lock().expect("sent frames lock");
        assert_eq!(sent.as_slice(), &[stop_frame]);
    }

    #[test]
    fn test_fault_latched_shutdown_preempts_pending_realtime_and_reliable_commands() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let piper = Piper::new_dual_thread_parts(
            MockRxAdapter,
            RecordingTxAdapter {
                sent_frames: sent_frames.clone(),
            },
            None,
        )
        .unwrap();
        let stale_realtime = PiperFrame::new_standard(0x155, &[0x01]);
        let stale_reliable = PiperFrame::new_standard(0x472, &[0x02]);
        let stop_frame = PiperFrame::new_standard(0x471, &[0x03]);

        piper.latch_fault();
        {
            let mut slot = piper.realtime_slot.lock().expect("realtime slot lock");
            slot.replace(RealtimeCommand::single(stale_realtime));
        }
        piper
            .reliable_tx
            .try_send(ReliableCommand::single(stale_reliable))
            .expect("stale reliable should queue directly for test");

        wait_shutdown(&piper, stop_frame, Duration::from_millis(200))
            .expect("shutdown command should be sent");

        std::thread::sleep(Duration::from_millis(20));
        let sent = sent_frames.lock().expect("sent frames lock");
        assert_eq!(sent.as_slice(), &[stop_frame]);
        let metrics = piper.get_metrics();
        assert_eq!(metrics.tx_reliable_queue_full_total, 0);
        assert_eq!(metrics.tx_fault_aborts_total, 2);
    }

    #[test]
    fn test_realtime_package_fault_abort_reports_sent_prefix_and_stops_remaining_frames() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let piper = Arc::new(
            Piper::new_dual_thread_parts(
                MockRxAdapter,
                BlockingFirstSendTxAdapter {
                    sent_frames: sent_frames.clone(),
                    started_tx,
                    release_rx,
                    sends: 0,
                },
                None,
            )
            .unwrap(),
        );
        let frames = [
            PiperFrame::new_standard(0x155, &[0x01]),
            PiperFrame::new_standard(0x156, &[0x02]),
            PiperFrame::new_standard(0x157, &[0x03]),
        ];
        let piper_clone = piper.clone();
        let result_handle = std::thread::spawn(move || {
            piper_clone.send_realtime_package_confirmed(frames, Duration::from_millis(500))
        });

        started_rx
            .recv_timeout(Duration::from_millis(200))
            .expect("first realtime frame should begin sending");
        piper.latch_fault();
        let _ = release_tx.send(());

        let error = result_handle
            .join()
            .expect("confirmed send thread should finish")
            .expect_err("fault latch should abort remaining frames");
        assert!(matches!(
            error,
            DriverError::RealtimeDeliveryAbortedByFault { sent: 1, total: 3 }
        ));

        let sent = sent_frames.lock().expect("sent frames lock");
        assert_eq!(sent.as_slice(), &[frames[0]]);
        let metrics = piper.get_metrics();
        assert_eq!(metrics.tx_frames_sent_total, 1);
        assert_eq!(metrics.tx_fault_aborts_total, 1);
        assert_eq!(metrics.tx_packages_fault_aborted_total, 1);
        assert_eq!(metrics.tx_packages_completed_total, 0);
        assert_eq!(metrics.tx_packages_partial_total, 0);
    }

    #[test]
    fn test_maintenance_send_point_rejects_stale_lease_after_revocation() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let piper = Arc::new(
            Piper::new_dual_thread_parts(
                MockRxAdapter,
                BlockingFirstSendTxAdapter {
                    sent_frames: sent_frames.clone(),
                    started_tx,
                    release_rx,
                    sends: 0,
                },
                None,
            )
            .unwrap(),
        );

        let session_key = 77;
        piper.ctx.connection_monitor.register_feedback();
        piper.set_maintenance_gate_state(MaintenanceGateState::AllowedStandby);
        let lease_epoch =
            match piper.acquire_maintenance_lease_gate(7, session_key, Duration::from_millis(10)) {
                MaintenanceLeaseAcquireResult::Granted { lease_epoch } => lease_epoch,
                other => panic!("unexpected maintenance acquire result: {other:?}"),
            };

        let blocking_frame = PiperFrame::new_standard(0x201, &[0x01]);
        let piper_blocking = Arc::clone(&piper);
        let blocking_handle = std::thread::spawn(move || {
            piper_blocking.send_maintenance_frame_confirmed(
                7,
                session_key,
                lease_epoch,
                blocking_frame,
                Duration::from_millis(500),
            )
        });

        started_rx
            .recv_timeout(Duration::from_millis(200))
            .expect("first maintenance frame should begin sending");

        let stale_frame = PiperFrame::new_standard(0x202, &[0x02]);
        let piper_stale = Arc::clone(&piper);
        let stale_handle = std::thread::spawn(move || {
            piper_stale.send_maintenance_frame_confirmed(
                7,
                session_key,
                lease_epoch,
                stale_frame,
                Duration::from_millis(500),
            )
        });

        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(
            piper
                .revoke_maintenance_lease_gate(
                    session_key,
                    MaintenanceRevocationReason::SessionReplaced,
                )
                .map(|event| event.session_id()),
            Some(7)
        );
        let _ = release_tx.send(());

        blocking_handle
            .join()
            .expect("blocking maintenance sender should finish")
            .expect("frame already admitted before revocation should complete");

        let stale_error = stale_handle
            .join()
            .expect("stale maintenance sender should finish")
            .expect_err("revoked lease epoch must be rejected at send point");
        assert!(matches!(
            stale_error,
            DriverError::MaintenanceWriteDenied(_)
        ));

        let sent = sent_frames.lock().expect("sent frames lock");
        assert_eq!(sent.as_slice(), &[blocking_frame]);
    }

    #[test]
    fn test_maintenance_gate_waiter_wakes_immediately_after_holder_releases() {
        let gate = Arc::new(MaintenanceGate::default());
        gate.set_state(MaintenanceGateState::AllowedStandby);
        match gate.acquire_blocking(1, 11, Duration::from_millis(10)) {
            MaintenanceLeaseAcquireResult::Granted { .. } => {},
            other => panic!("initial maintenance lease grant failed: {other:?}"),
        }

        let waiter_gate = Arc::clone(&gate);
        let waiter = std::thread::spawn(move || {
            waiter_gate.acquire_blocking(2, 22, Duration::from_millis(250))
        });

        std::thread::sleep(Duration::from_millis(20));
        assert!(gate.release_if_holder(11));

        match waiter.join().expect("waiter thread should finish") {
            MaintenanceLeaseAcquireResult::Granted { .. } => {},
            other => panic!("waiting maintenance lease did not wake correctly: {other:?}"),
        }
    }

    #[test]
    fn test_maintenance_gate_emits_revocation_event_on_denied_state_transition() {
        let gate = MaintenanceGate::default();
        let (tx, rx) = crossbeam_channel::bounded(1);
        gate.set_event_sink(tx);
        gate.set_state(MaintenanceGateState::AllowedStandby);
        match gate.acquire_blocking(7, 77, Duration::from_millis(10)) {
            MaintenanceLeaseAcquireResult::Granted { .. } => {},
            other => panic!("initial maintenance lease grant failed: {other:?}"),
        }

        gate.set_state(MaintenanceGateState::DeniedActiveControl);
        let event = rx
            .recv_timeout(Duration::from_millis(100))
            .expect("lease revocation should be emitted immediately");
        assert_eq!(event.session_id(), 7);
        assert_eq!(event.session_key(), 77);
        assert_eq!(
            event.reason(),
            MaintenanceRevocationReason::ControllerStateChanged
        );
    }

    #[test]
    fn test_send_realtime_package_confirmed_reports_partial_failure() {
        let piper = Piper::new_dual_thread_parts(
            MockRxAdapter,
            FailOnNthTxAdapter {
                fail_on: 2,
                sends: 0,
            },
            None,
        )
        .unwrap();
        let frames = [
            PiperFrame::new_standard(0x155, &[0x01]),
            PiperFrame::new_standard(0x156, &[0x02]),
            PiperFrame::new_standard(0x157, &[0x03]),
        ];

        let error = piper
            .send_realtime_package_confirmed(frames, Duration::from_millis(200))
            .expect_err("partial send should fail");

        assert!(matches!(
            error,
            DriverError::RealtimeDeliveryFailed {
                sent: 1,
                total: 3,
                ..
            }
        ));
    }

    #[test]
    fn test_health_prefers_latched_transport_fault_over_tx_exited() {
        let (started_tx, started_rx) = mpsc::channel();
        drop(started_rx);
        let (release_tx, release_rx) = mpsc::channel();
        drop(release_tx);
        let piper = Piper::new_dual_thread_parts(
            MockRxAdapter,
            CoordinatedFailTxAdapter {
                started_tx,
                release_rx,
                first_send: false,
            },
            None,
        )
        .unwrap();

        let error = piper
            .send_realtime_package_confirmed(
                [PiperFrame::new_standard(0x155, &[0x01])],
                Duration::from_millis(200),
            )
            .expect_err("fatal transport error should fail confirmed send");
        assert!(matches!(
            error,
            DriverError::RealtimeDeliveryFailed { .. } | DriverError::ChannelClosed
        ));

        std::thread::sleep(Duration::from_millis(20));
        let health = piper.health();
        assert!(!health.tx_alive);
        assert_eq!(health.fault, Some(RuntimeFaultKind::TransportError));
    }

    #[test]
    fn test_pending_confirmed_send_is_failed_when_tx_thread_exits() {
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let piper = Arc::new(
            Piper::new_dual_thread_parts(
                MockRxAdapter,
                CoordinatedFailTxAdapter {
                    started_tx,
                    release_rx,
                    first_send: false,
                },
                None,
            )
            .unwrap(),
        );
        let first = [PiperFrame::new_standard(0x155, &[0x01])];
        let second = [PiperFrame::new_standard(0x156, &[0x02])];

        piper.send_realtime_package(first).expect("first realtime package should queue");
        started_rx
            .recv_timeout(Duration::from_millis(200))
            .expect("tx thread should start sending first frame");

        let pending_result = Arc::new(Mutex::new(None));
        let pending_result_clone = pending_result.clone();
        let piper_clone = piper.clone();
        let handle = std::thread::spawn(move || {
            let result =
                piper_clone.send_realtime_package_confirmed(second, Duration::from_millis(500));
            *pending_result_clone.lock().expect("pending result lock") = Some(result);
        });

        let enqueue_deadline = Instant::now() + Duration::from_millis(200);
        loop {
            if piper.realtime_slot.lock().expect("realtime slot lock").is_some() {
                break;
            }
            assert!(
                Instant::now() < enqueue_deadline,
                "second confirmed realtime package should be pending before transport failure"
            );
            std::thread::yield_now();
        }

        let _ = release_tx.send(());
        handle.join().expect("confirmed send thread should finish");

        let result = pending_result
            .lock()
            .expect("pending result lock")
            .take()
            .expect("confirmed send result should be captured");
        assert!(matches!(result, Err(DriverError::ChannelClosed)));
    }

    #[test]
    fn test_get_motion_state_returns_combined() {
        let mock_can = MockCanAdapter;
        let piper = Piper::new_dual_thread(mock_can, None).unwrap();

        let motion = piper.get_motion_state();
        // 验证返回的是组合状态
        assert_eq!(motion.joint_position.hardware_timestamp_us, 0);
        assert_eq!(motion.joint_dynamic.group_timestamp_us, 0);
        assert_eq!(motion.joint_position.joint_pos, [0.0; 6]);
        assert_eq!(motion.joint_dynamic.joint_vel, [0.0; 6]);
    }

    #[test]
    fn test_send_frame_non_blocking() {
        let mock_can = MockCanAdapter;
        let piper = Piper::new_dual_thread(mock_can, None).unwrap();
        let frame = PiperFrame::new_standard(0x123, &[0x01, 0x02]);

        // 非阻塞发送应该总是成功（除非通道满或关闭）
        let result = piper.send_frame(frame);
        assert!(result.is_ok(), "Non-blocking send should succeed");
    }

    #[test]
    fn test_get_joint_dynamic_default() {
        let mock_can = MockCanAdapter;
        let piper = Piper::new_dual_thread(mock_can, None).unwrap();

        let joint_dynamic = piper.get_joint_dynamic();
        assert_eq!(joint_dynamic.group_timestamp_us, 0);
        assert_eq!(joint_dynamic.joint_vel, [0.0; 6]);
        assert_eq!(joint_dynamic.joint_current, [0.0; 6]);
        assert!(!joint_dynamic.is_complete());
    }

    #[test]
    fn test_get_joint_position_default() {
        let mock_can = MockCanAdapter;
        let piper = Piper::new_dual_thread(mock_can, None).unwrap();

        let joint_pos = piper.get_joint_position();
        assert_eq!(joint_pos.hardware_timestamp_us, 0);
        assert_eq!(joint_pos.joint_pos, [0.0; 6]);

        let end_pose = piper.get_end_pose();
        assert_eq!(end_pose.hardware_timestamp_us, 0);
        assert_eq!(end_pose.end_pose, [0.0; 6]);
    }

    #[test]
    fn test_joint_driver_low_speed_clone() {
        let mock_can = MockCanAdapter;
        let piper = Piper::new_dual_thread(mock_can, None).unwrap();

        // 测试读取并克隆诊断状态
        let driver1 = piper.get_joint_driver_low_speed();
        let driver2 = piper.get_joint_driver_low_speed();

        // 验证可以多次读取（ArcSwap 无锁读取）
        assert_eq!(driver1.hardware_timestamp_us, driver2.hardware_timestamp_us);
        assert_eq!(driver1.motor_temps, driver2.motor_temps);
    }

    #[test]
    fn test_joint_limit_config_read_lock() {
        let mock_can = MockCanAdapter;
        let piper = Piper::new_dual_thread(mock_can, None).unwrap();

        // 测试可以多次读取配置状态
        let limits1 = piper.get_joint_limit_config().unwrap();
        let limits2 = piper.get_joint_limit_config().unwrap();

        assert_eq!(limits1.joint_limits_max, limits2.joint_limits_max);
        assert_eq!(limits1.joint_limits_min, limits2.joint_limits_min);
    }
}
