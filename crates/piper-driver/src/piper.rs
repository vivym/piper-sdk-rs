//! Robot API 模块
//!
//! 提供对外的 `Piper` 结构体，封装底层 IO 线程和状态同步细节。

use crate::ProtocolDiagnostic;
use crate::WaitError;
use crate::command::{
    CommandPriority, DeliveryPhase, MaintenanceCommandMeta, PiperCommand, RealtimeCommand,
    ReliableCommand, ReliableCommandKind, SoftRealtimeCommand, SoftRealtimeMailbox,
    SoftRealtimeTryReserveError, SoftRealtimeTrySendError,
};
use crate::diagnostics::{DiagnosticEvent, QueryDiagnostic};
use crate::error::DriverError;
use crate::fps_stats::{FpsCounts, FpsResult};
use crate::metrics::{MetricsSnapshot, PiperMetrics};
use crate::observation::{Complete, Observation, ObservationPayload};
use crate::pipeline::*;
use crate::query_coordinator::{QueryError, QueryGuard, QueryKind};
use crate::state::*;
use crossbeam_channel::{Receiver, Sender};
use piper_can::{
    BackendCapability, CanError, PiperFrame, RealtimeTxAdapter, RxAdapter, SplittableAdapter,
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
    ManualFault = 4,
}

impl RuntimeFaultKind {
    pub(crate) fn from_raw(raw: u8) -> Option<Self> {
        match raw {
            1 => Some(Self::RxExited),
            2 => Some(Self::TxExited),
            3 => Some(Self::TransportError),
            4 => Some(Self::ManualFault),
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
pub(crate) const SOFT_CONTROL_SEND_BUDGET: Duration = Duration::from_millis(5);
pub(crate) const SOFT_DEADLINE_MISS_FAULT_THRESHOLD: u32 = 3;
pub(crate) const STRICT_TIMESTAMP_VALIDATION_TIMEOUT: Duration = Duration::from_secs(1);
const DEFAULT_MODE_SWITCH_TIMEOUT: Duration = Duration::from_millis(100);
const CONFIG_QUERY_POLL_INTERVAL: Duration = Duration::from_millis(10);
const END_POSE_FRESHNESS_WINDOW_US: u64 = 6_000;
const REBUILT_LOW_SPEED_OBSERVATION_FRESHNESS_WINDOW_US: u64 = 75_000;

#[cfg(test)]
#[derive(Debug)]
struct ModeSwitchBarrier {
    reached_tx: std::sync::mpsc::Sender<()>,
    release_rx: std::sync::mpsc::Receiver<()>,
}

#[cfg(test)]
#[derive(Debug)]
struct FaultLatchBarrier {
    reached_tx: std::sync::mpsc::Sender<()>,
    release_rx: std::sync::mpsc::Receiver<()>,
}

#[cfg(test)]
#[derive(Debug)]
struct StateTransitionBarrier {
    reached_tx: std::sync::mpsc::Sender<()>,
    release_rx: std::sync::mpsc::Receiver<()>,
}

#[cfg(test)]
#[derive(Debug)]
struct SoftRealtimeAdmissionBarrier {
    reached_tx: std::sync::mpsc::Sender<()>,
    release_rx: std::sync::mpsc::Receiver<()>,
}

#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManualFaultRecoveryResult {
    Standby,
    Maintenance { confirmed_mask: u8 },
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct StartupValidationDeadline {
    instant_deadline: Instant,
    host_rx_deadline_mono_us: u64,
}

impl StartupValidationDeadline {
    pub(crate) fn after(timeout: Duration) -> Self {
        let timeout_us = timeout.as_micros().min(u64::MAX as u128) as u64;
        Self {
            instant_deadline: Instant::now() + timeout,
            host_rx_deadline_mono_us: crate::heartbeat::monotonic_micros()
                .saturating_add(timeout_us),
        }
    }

    pub(crate) fn is_expired_now(&self) -> bool {
        Instant::now() >= self.instant_deadline
    }

    pub(crate) fn instant_deadline(&self) -> Instant {
        self.instant_deadline
    }

    fn host_rx_deadline_mono_us(&self) -> u64 {
        self.host_rx_deadline_mono_us
    }
}

pub(crate) fn strict_realtime_timestamp_error(reason: impl Into<String>) -> DriverError {
    DriverError::Can(CanError::Device(piper_can::CanDeviceError::new(
        piper_can::CanDeviceErrorKind::UnsupportedConfig,
        format!(
            "StrictRealtime requires trusted CAN timestamps; refusing strict connection: {}",
            reason.into()
        ),
    )))
}

fn recv_until_deadline<T, F>(
    rx: &Receiver<T>,
    deadline: Instant,
    timeout_error: F,
) -> Result<T, DriverError>
where
    F: Fn() -> DriverError,
{
    match rx.try_recv() {
        Ok(value) => return Ok(value),
        Err(crossbeam_channel::TryRecvError::Disconnected) => {
            return Err(DriverError::ChannelClosed);
        },
        Err(crossbeam_channel::TryRecvError::Empty) => {},
    }

    let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
        return match rx.try_recv() {
            Ok(value) => Ok(value),
            Err(crossbeam_channel::TryRecvError::Empty) => Err(timeout_error()),
            Err(crossbeam_channel::TryRecvError::Disconnected) => Err(DriverError::ChannelClosed),
        };
    };

    match rx.recv_timeout(remaining) {
        Ok(value) => Ok(value),
        Err(crossbeam_channel::RecvTimeoutError::Timeout) => match rx.try_recv() {
            Ok(value) => Ok(value),
            Err(crossbeam_channel::TryRecvError::Empty) => Err(timeout_error()),
            Err(crossbeam_channel::TryRecvError::Disconnected) => Err(DriverError::ChannelClosed),
        },
        Err(crossbeam_channel::RecvTimeoutError::Disconnected) => Err(DriverError::ChannelClosed),
    }
}

fn wait_for_maintenance_send_result(
    ack_rx: Receiver<MaintenanceSendPhase>,
    deadline: Instant,
) -> Result<(), DriverError> {
    match recv_until_deadline(&ack_rx, deadline, || DriverError::Timeout)? {
        DeliveryPhase::Finished(result) => result,
        DeliveryPhase::Committed { .. } => loop {
            match ack_rx.recv() {
                Ok(DeliveryPhase::Finished(result)) => return result,
                Ok(DeliveryPhase::Committed { .. }) => continue,
                Err(_) => return Err(DriverError::ChannelClosed),
            }
        },
    }
}

fn wait_for_delivery_result_with_commit<F>(
    ack_rx: Receiver<DeliveryPhase>,
    deadline: Instant,
    timeout_error: F,
) -> Result<Option<u64>, DriverError>
where
    F: Fn() -> DriverError,
{
    match recv_until_deadline(&ack_rx, deadline, timeout_error)? {
        DeliveryPhase::Finished(result) => {
            result?;
            Ok(None)
        },
        DeliveryPhase::Committed {
            host_commit_mono_us,
        } => loop {
            match ack_rx.recv() {
                Ok(DeliveryPhase::Finished(result)) => {
                    result?;
                    return Ok(Some(host_commit_mono_us));
                },
                Ok(DeliveryPhase::Committed { .. }) => continue,
                Err(_) => return Err(DriverError::ChannelClosed),
            }
        },
    }
}

fn wait_for_delivery_result<F>(
    ack_rx: Receiver<DeliveryPhase>,
    deadline: Instant,
    timeout_error: F,
) -> Result<(), DriverError>
where
    F: Fn() -> DriverError,
{
    wait_for_delivery_result_with_commit(ack_rx, deadline, timeout_error).map(|_| ())
}

#[doc(hidden)]
#[derive(Debug)]
pub struct NormalSendGate {
    state: AtomicU8,
    epoch: AtomicU64,
    inflight_normal_sends: AtomicUsize,
    disable_confirmation_pending: AtomicBool,
    #[cfg(test)]
    fault_latch_barrier: Mutex<Option<FaultLatchBarrier>>,
    #[cfg(test)]
    state_transition_barrier: Mutex<Option<StateTransitionBarrier>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum NormalSendGateState {
    Open = 0,
    ReplaySwitchPending = 1,
    ReplayPaused = 2,
    FaultClosed = 3,
    StoppingClosed = 4,
    StateTransitionClosed = 5,
}

impl NormalSendGateState {
    fn from_raw(raw: u8) -> Self {
        match raw {
            0 => Self::Open,
            1 => Self::ReplaySwitchPending,
            2 => Self::ReplayPaused,
            3 => Self::FaultClosed,
            4 => Self::StoppingClosed,
            _ => Self::StateTransitionClosed,
        }
    }

    fn deny_reason(self) -> Option<NormalSendGateDenyReason> {
        match self {
            Self::Open => None,
            Self::ReplaySwitchPending | Self::ReplayPaused => {
                Some(NormalSendGateDenyReason::ReplayPaused)
            },
            Self::FaultClosed => Some(NormalSendGateDenyReason::FaultClosed),
            Self::StoppingClosed => Some(NormalSendGateDenyReason::StoppingClosed),
            Self::StateTransitionClosed => Some(NormalSendGateDenyReason::StateTransitionClosed),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NormalSendGateDenyReason {
    ReplayPaused,
    FaultClosed,
    StoppingClosed,
    StateTransitionClosed,
}

#[derive(Debug)]
pub(crate) struct NormalSendPermit<'a> {
    gate: &'a NormalSendGate,
    epoch: u64,
}

impl NormalSendGate {
    pub fn new() -> Self {
        Self {
            state: AtomicU8::new(NormalSendGateState::Open as u8),
            epoch: AtomicU64::new(0),
            inflight_normal_sends: AtomicUsize::new(0),
            disable_confirmation_pending: AtomicBool::new(false),
            #[cfg(test)]
            fault_latch_barrier: Mutex::new(None),
            #[cfg(test)]
            state_transition_barrier: Mutex::new(None),
        }
    }

    pub(crate) fn state(&self) -> NormalSendGateState {
        NormalSendGateState::from_raw(self.state.load(Ordering::Acquire))
    }

    fn set_state(&self, new_state: NormalSendGateState) {
        let previous = self.state.swap(new_state as u8, Ordering::AcqRel);
        if previous != new_state as u8 {
            self.epoch.fetch_add(1, Ordering::AcqRel);
        }
    }

    pub(crate) fn is_replay_paused(&self) -> bool {
        matches!(
            self.state(),
            NormalSendGateState::ReplaySwitchPending | NormalSendGateState::ReplayPaused
        )
    }

    pub(crate) fn accepts_front_door_submissions(&self) -> bool {
        self.state() == NormalSendGateState::Open
    }

    pub(crate) fn acquire_normal(&self) -> Result<NormalSendPermit<'_>, NormalSendGateDenyReason> {
        loop {
            let epoch = self.epoch.load(Ordering::Acquire);
            let state = self.state();
            if let Some(reason) = state.deny_reason() {
                return Err(reason);
            }

            self.inflight_normal_sends.fetch_add(1, Ordering::AcqRel);

            let current_epoch = self.epoch.load(Ordering::Acquire);
            let current_state = self.state();
            if current_state == NormalSendGateState::Open && current_epoch == epoch {
                return Ok(NormalSendPermit { gate: self, epoch });
            }

            self.inflight_normal_sends.fetch_sub(1, Ordering::AcqRel);

            if let Some(reason) = current_state.deny_reason() {
                return Err(reason);
            }
        }
    }

    pub(crate) fn begin_replay_switch(&self) -> Result<(), NormalSendGateDenyReason> {
        loop {
            let current = self.state();
            match current {
                NormalSendGateState::Open => {
                    if self
                        .state
                        .compare_exchange(
                            current as u8,
                            NormalSendGateState::ReplaySwitchPending as u8,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        )
                        .is_ok()
                    {
                        self.epoch.fetch_add(1, Ordering::AcqRel);
                        return Ok(());
                    }
                },
                NormalSendGateState::ReplaySwitchPending | NormalSendGateState::ReplayPaused => {
                    return Err(NormalSendGateDenyReason::ReplayPaused);
                },
                NormalSendGateState::FaultClosed => {
                    return Err(NormalSendGateDenyReason::FaultClosed);
                },
                NormalSendGateState::StoppingClosed => {
                    return Err(NormalSendGateDenyReason::StoppingClosed);
                },
                NormalSendGateState::StateTransitionClosed => {
                    return Err(NormalSendGateDenyReason::StateTransitionClosed);
                },
            }
        }
    }

    pub(crate) fn commit_replay_switch(&self) -> Result<(), NormalSendGateDenyReason> {
        loop {
            let current = self.state();
            match current {
                NormalSendGateState::ReplaySwitchPending => {
                    if self
                        .state
                        .compare_exchange(
                            current as u8,
                            NormalSendGateState::ReplayPaused as u8,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        )
                        .is_ok()
                    {
                        self.epoch.fetch_add(1, Ordering::AcqRel);
                        return Ok(());
                    }
                },
                NormalSendGateState::ReplayPaused => {
                    return Err(NormalSendGateDenyReason::ReplayPaused);
                },
                NormalSendGateState::FaultClosed => {
                    return Err(NormalSendGateDenyReason::FaultClosed);
                },
                NormalSendGateState::StoppingClosed => {
                    return Err(NormalSendGateDenyReason::StoppingClosed);
                },
                NormalSendGateState::StateTransitionClosed | NormalSendGateState::Open => {
                    return Err(NormalSendGateDenyReason::StateTransitionClosed);
                },
            }
        }
    }

    pub(crate) fn resume_from_replay(&self) {
        if matches!(
            self.state(),
            NormalSendGateState::ReplaySwitchPending | NormalSendGateState::ReplayPaused
        ) {
            self.set_state(NormalSendGateState::Open);
        }
    }

    fn replay_restore_state(
        runtime_phase: RuntimePhase,
        runtime_fault: Option<RuntimeFaultKind>,
        driver_mode: crate::mode::DriverMode,
    ) -> NormalSendGateState {
        match runtime_phase {
            RuntimePhase::Stopping => NormalSendGateState::StoppingClosed,
            RuntimePhase::FaultLatched => NormalSendGateState::FaultClosed,
            RuntimePhase::Running => {
                if runtime_fault.is_some() {
                    NormalSendGateState::FaultClosed
                } else if driver_mode.is_replay() {
                    NormalSendGateState::ReplayPaused
                } else {
                    NormalSendGateState::Open
                }
            },
        }
    }

    pub(crate) fn abort_replay_switch(
        &self,
        runtime_phase: RuntimePhase,
        runtime_fault: Option<RuntimeFaultKind>,
        driver_mode: crate::mode::DriverMode,
    ) {
        match self.state() {
            NormalSendGateState::ReplaySwitchPending | NormalSendGateState::ReplayPaused => {
                self.set_state(Self::replay_restore_state(
                    runtime_phase,
                    runtime_fault,
                    driver_mode,
                ));
            },
            NormalSendGateState::StateTransitionClosed => {},
            NormalSendGateState::Open
            | NormalSendGateState::FaultClosed
            | NormalSendGateState::StoppingClosed => {},
        }
    }

    pub(crate) fn close_for_fault(&self) {
        self.disable_confirmation_pending.store(false, Ordering::Release);
        if self.state() != NormalSendGateState::StoppingClosed {
            self.set_state(NormalSendGateState::FaultClosed);
        }
    }

    pub(crate) fn reopen_after_fault(&self) {
        if self.state() == NormalSendGateState::FaultClosed {
            self.set_state(NormalSendGateState::Open);
        }
    }

    #[cfg(test)]
    fn install_fault_latch_barrier(
        &self,
        reached_tx: std::sync::mpsc::Sender<()>,
        release_rx: std::sync::mpsc::Receiver<()>,
    ) {
        let mut guard =
            self.fault_latch_barrier.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        *guard = Some(FaultLatchBarrier {
            reached_tx,
            release_rx,
        });
    }

    #[cfg(test)]
    pub(crate) fn maybe_wait_test_fault_latch_barrier(&self) {
        let barrier = self
            .fault_latch_barrier
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        if let Some(barrier) = barrier {
            let _ = barrier.reached_tx.send(());
            let _ = barrier.release_rx.recv();
        }
    }

    #[cfg(not(test))]
    pub(crate) fn maybe_wait_test_fault_latch_barrier(&self) {}

    #[cfg(test)]
    fn install_state_transition_barrier(
        &self,
        reached_tx: std::sync::mpsc::Sender<()>,
        release_rx: std::sync::mpsc::Receiver<()>,
    ) {
        let mut guard = self
            .state_transition_barrier
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *guard = Some(StateTransitionBarrier {
            reached_tx,
            release_rx,
        });
    }

    #[cfg(test)]
    pub(crate) fn maybe_wait_test_state_transition_barrier(&self) {
        let barrier = self
            .state_transition_barrier
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        if let Some(barrier) = barrier {
            let _ = barrier.reached_tx.send(());
            let _ = barrier.release_rx.recv();
        }
    }

    #[cfg(not(test))]
    pub(crate) fn maybe_wait_test_state_transition_barrier(&self) {}

    pub(crate) fn close_for_stop(&self) {
        self.disable_confirmation_pending.store(false, Ordering::Release);
        self.set_state(NormalSendGateState::StoppingClosed);
    }

    pub(crate) fn close_for_state_transition(&self) -> Result<(), NormalSendGateDenyReason> {
        loop {
            let current = self.state();
            match current {
                NormalSendGateState::Open | NormalSendGateState::ReplaySwitchPending => {
                    if self
                        .state
                        .compare_exchange(
                            current as u8,
                            NormalSendGateState::StateTransitionClosed as u8,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        )
                        .is_ok()
                    {
                        self.epoch.fetch_add(1, Ordering::AcqRel);
                        return Ok(());
                    }
                },
                NormalSendGateState::ReplayPaused => {
                    return Err(NormalSendGateDenyReason::ReplayPaused);
                },
                NormalSendGateState::FaultClosed => {
                    return Err(NormalSendGateDenyReason::FaultClosed);
                },
                NormalSendGateState::StoppingClosed => {
                    return Err(NormalSendGateDenyReason::StoppingClosed);
                },
                NormalSendGateState::StateTransitionClosed => {
                    return Err(NormalSendGateDenyReason::StateTransitionClosed);
                },
            }
        }
    }

    pub(crate) fn arm_disable_confirmation_pending(&self) {
        self.disable_confirmation_pending.store(true, Ordering::Release);
    }

    pub(crate) fn disable_confirmation_pending(&self) -> bool {
        self.disable_confirmation_pending.load(Ordering::Acquire)
    }

    pub(crate) fn finalize_disable_confirmation(
        &self,
        runtime_phase: RuntimePhase,
        runtime_fault: Option<RuntimeFaultKind>,
        driver_mode: crate::mode::DriverMode,
    ) {
        if !self.disable_confirmation_pending.swap(false, Ordering::AcqRel) {
            return;
        }
        self.restore_after_state_transition(runtime_phase, runtime_fault, driver_mode);
    }

    pub(crate) fn restore_after_state_transition(
        &self,
        runtime_phase: RuntimePhase,
        runtime_fault: Option<RuntimeFaultKind>,
        driver_mode: crate::mode::DriverMode,
    ) {
        if self.state() != NormalSendGateState::StateTransitionClosed {
            return;
        }

        self.set_state(Self::replay_restore_state(
            runtime_phase,
            runtime_fault,
            driver_mode,
        ));
    }

    pub fn inflight_normal_sends(&self) -> usize {
        self.inflight_normal_sends.load(Ordering::Acquire)
    }
}

impl Default for NormalSendGate {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for NormalSendPermit<'_> {
    fn drop(&mut self) {
        self.gate.inflight_normal_sends.fetch_sub(1, Ordering::AcqRel);
    }
}

impl NormalSendPermit<'_> {
    pub(crate) fn send_allowed(&self) -> Result<(), NormalSendGateDenyReason> {
        let current_state = self.gate.state();
        let current_epoch = self.gate.epoch.load(Ordering::Acquire);
        if current_epoch == self.epoch {
            return current_state.deny_reason().map_or(Ok(()), Err);
        }

        match current_state {
            NormalSendGateState::Open
            | NormalSendGateState::ReplaySwitchPending
            | NormalSendGateState::ReplayPaused => Ok(()),
            NormalSendGateState::FaultClosed => Err(NormalSendGateDenyReason::FaultClosed),
            NormalSendGateState::StoppingClosed => Err(NormalSendGateDenyReason::StoppingClosed),
            NormalSendGateState::StateTransitionClosed => {
                Err(NormalSendGateDenyReason::StateTransitionClosed)
            },
        }
    }
}

struct ReplaySwitchTxn<'a> {
    normal_send_gate: &'a NormalSendGate,
    driver_mode: &'a crate::mode::AtomicDriverMode,
    runtime_phase: &'a AtomicU8,
    runtime_fault: &'a AtomicU8,
    published_replay: bool,
}

impl<'a> ReplaySwitchTxn<'a> {
    fn begin(piper: &'a Piper) -> Result<Self, DriverError> {
        match piper.normal_send_gate.begin_replay_switch() {
            Ok(()) => Ok(Self {
                normal_send_gate: piper.normal_send_gate.as_ref(),
                driver_mode: piper.driver_mode.as_ref(),
                runtime_phase: piper.runtime_phase.as_ref(),
                runtime_fault: piper.runtime_fault.as_ref(),
                published_replay: false,
            }),
            Err(NormalSendGateDenyReason::StoppingClosed) => Err(DriverError::ChannelClosed),
            Err(
                NormalSendGateDenyReason::ReplayPaused
                | NormalSendGateDenyReason::FaultClosed
                | NormalSendGateDenyReason::StateTransitionClosed,
            ) => Err(DriverError::ControlPathClosed),
        }
    }

    fn commit_paused(&self) -> Result<(), DriverError> {
        match self.normal_send_gate.commit_replay_switch() {
            Ok(()) => Ok(()),
            Err(NormalSendGateDenyReason::StoppingClosed) => Err(DriverError::ChannelClosed),
            Err(
                NormalSendGateDenyReason::ReplayPaused
                | NormalSendGateDenyReason::FaultClosed
                | NormalSendGateDenyReason::StateTransitionClosed,
            ) => Err(DriverError::ControlPathClosed),
        }
    }

    fn publish_replay(mut self) {
        self.driver_mode.set(crate::mode::DriverMode::Replay, Ordering::Release);
        self.published_replay = true;
    }

    fn runtime_phase(&self) -> RuntimePhase {
        RuntimePhase::from_raw(self.runtime_phase.load(Ordering::Acquire))
    }

    fn runtime_fault(&self) -> Option<RuntimeFaultKind> {
        RuntimeFaultKind::from_raw(self.runtime_fault.load(Ordering::Acquire))
    }
}

impl Drop for ReplaySwitchTxn<'_> {
    fn drop(&mut self) {
        if self.published_replay {
            return;
        }

        self.normal_send_gate.abort_replay_switch(
            self.runtime_phase(),
            self.runtime_fault(),
            self.driver_mode.get(Ordering::Acquire),
        );
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
    DeniedDriveStateUnknown = 4,
}

impl MaintenanceGateState {
    pub fn allows_lease(self) -> bool {
        matches!(self, Self::AllowedStandby)
    }

    pub fn denial_message(self) -> &'static str {
        match self {
            Self::DeniedFaulted => {
                "maintenance writes are disabled while a runtime fault is latched"
            },
            Self::DeniedActiveControl => {
                "maintenance writes are disabled while active control is enabled"
            },
            Self::DeniedTransportDown => {
                "maintenance writes are disabled while transport is disconnected"
            },
            Self::AllowedStandby => "maintenance allowed",
            Self::DeniedDriveStateUnknown => {
                "maintenance writes are disabled until joint driver enable state is confirmed"
            },
        }
    }

    fn from_raw(raw: u8) -> Self {
        match raw {
            0 => Self::DeniedFaulted,
            1 => Self::DeniedActiveControl,
            4 => Self::DeniedDriveStateUnknown,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaintenanceControlOp {
    Grant {
        session_id: u32,
        session_key: u64,
        lease_epoch: u64,
    },
    Release {
        session_key: u64,
        lease_epoch: u64,
    },
    Revoke {
        session_key: u64,
        lease_epoch: u64,
        reason: MaintenanceRevocationReason,
    },
    SetState {
        state: MaintenanceGateState,
        holder_session_id: Option<u32>,
        holder_session_key: Option<u64>,
        lease_epoch: u64,
    },
}

#[doc(hidden)]
pub type MaintenanceSendPhase = DeliveryPhase;

#[doc(hidden)]
#[derive(Debug)]
pub enum MaintenanceLaneCommand {
    Control {
        op: MaintenanceControlOp,
        ack: Option<Sender<()>>,
    },
    AbortPendingNormalControl {
        ack: Sender<()>,
    },
    Send {
        frame: PiperFrame,
        meta: MaintenanceCommandMeta,
        deadline: Instant,
        ack: Sender<MaintenanceSendPhase>,
    },
    LocalSend {
        frame: PiperFrame,
        deadline: Instant,
        ack: Sender<MaintenanceSendPhase>,
    },
    StateTransitionLocalSend {
        frame: PiperFrame,
        deadline: Instant,
        ack: Sender<MaintenanceSendPhase>,
        completion: StateTransitionCompletion,
    },
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateTransitionCompletion {
    RestoreAfterDispatch,
    HoldUntilDisableConfirmed,
}

impl MaintenanceLaneCommand {
    fn control(op: MaintenanceControlOp) -> Self {
        Self::Control { op, ack: None }
    }

    fn blocking_control(op: MaintenanceControlOp) -> (Self, Receiver<()>) {
        let (ack_tx, ack_rx) = crossbeam_channel::bounded(1);
        (
            Self::Control {
                op,
                ack: Some(ack_tx),
            },
            ack_rx,
        )
    }

    fn send(
        frame: PiperFrame,
        meta: MaintenanceCommandMeta,
        deadline: Instant,
        ack: Sender<MaintenanceSendPhase>,
    ) -> Self {
        Self::Send {
            frame,
            meta,
            deadline,
            ack,
        }
    }

    fn local_send(frame: PiperFrame, deadline: Instant, ack: Sender<MaintenanceSendPhase>) -> Self {
        Self::LocalSend {
            frame,
            deadline,
            ack,
        }
    }

    fn state_transition_local_send(
        frame: PiperFrame,
        deadline: Instant,
        ack: Sender<MaintenanceSendPhase>,
        completion: StateTransitionCompletion,
    ) -> Self {
        Self::StateTransitionLocalSend {
            frame,
            deadline,
            ack,
            completion,
        }
    }

    fn blocking_abort_pending_normal_control() -> (Self, Receiver<()>) {
        let (ack_tx, ack_rx) = crossbeam_channel::bounded(1);
        (Self::AbortPendingNormalControl { ack: ack_tx }, ack_rx)
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
    applied_lease_epoch: u64,
}

impl Default for MaintenanceGateInner {
    fn default() -> Self {
        Self {
            state: MaintenanceGateState::DeniedTransportDown,
            holder_session_id: None,
            holder_session_key: None,
            lease_epoch: 0,
            applied_lease_epoch: 0,
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
    lane_sink: Mutex<Option<Sender<MaintenanceLaneCommand>>>,
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
            lane_sink: Mutex::new(None),
        }
    }
}

impl MaintenanceGate {
    fn sync_atomics(&self, inner: &MaintenanceGateInner) {
        let published_holder_visible =
            inner.holder_session_key.is_some() && inner.applied_lease_epoch == inner.lease_epoch;
        let published_lease_epoch =
            if inner.holder_session_key.is_some() && !published_holder_visible {
                inner.applied_lease_epoch
            } else {
                inner.lease_epoch
            };
        self.state.store(inner.state as u8, Ordering::Release);
        self.holder_session_id.store(
            if published_holder_visible {
                inner.holder_session_id.unwrap_or(0)
            } else {
                0
            },
            Ordering::Release,
        );
        self.holder_session_key.store(
            if published_holder_visible {
                inner.holder_session_key.unwrap_or(0)
            } else {
                0
            },
            Ordering::Release,
        );
        self.lease_epoch.store(published_lease_epoch, Ordering::Release);
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

    #[doc(hidden)]
    pub fn set_lane_sink(&self, sink: Sender<MaintenanceLaneCommand>) {
        *self.lane_sink.lock().unwrap() = Some(sink);
    }

    pub fn set_control_sink(&self, sink: Sender<MaintenanceLaneCommand>) {
        self.set_lane_sink(sink);
    }

    fn send_control_command_locked(
        &self,
        op: MaintenanceControlOp,
        wait_for_ack: bool,
    ) -> Result<Option<Receiver<()>>, DriverError> {
        let sink = self.lane_sink.lock().unwrap().clone().ok_or(DriverError::ChannelClosed)?;
        if wait_for_ack {
            let (command, ack_rx) = MaintenanceLaneCommand::blocking_control(op);
            sink.send(command).map_err(|_| DriverError::ChannelClosed)?;
            Ok(Some(ack_rx))
        } else {
            sink.send(MaintenanceLaneCommand::control(op))
                .map_err(|_| DriverError::ChannelClosed)?;
            Ok(None)
        }
    }

    fn wait_control_ack_until(ack_rx: Receiver<()>, deadline: Instant) -> Result<(), DriverError> {
        recv_until_deadline(&ack_rx, deadline, || DriverError::Timeout)
    }

    fn rollback_tentative_grant(
        &self,
        session_key: u64,
        lease_epoch: u64,
    ) -> Result<(), DriverError> {
        let mut inner = self.inner.lock().unwrap();
        if inner.holder_session_key != Some(session_key)
            || inner.lease_epoch != lease_epoch
            || inner.applied_lease_epoch >= lease_epoch
        {
            return Ok(());
        }

        inner.holder_session_id = None;
        inner.holder_session_key = None;
        inner.lease_epoch = inner.lease_epoch.wrapping_add(1);
        let rollback_epoch = inner.lease_epoch;
        self.sync_atomics(&inner);
        self.wait_cv.notify_all();
        self.send_control_command_locked(
            MaintenanceControlOp::Release {
                session_key,
                lease_epoch: rollback_epoch,
            },
            false,
        )?;
        Ok(())
    }

    fn mark_grant_applied(&self, session_key: u64, lease_epoch: u64) {
        let mut inner = self.inner.lock().unwrap();
        if inner.holder_session_key == Some(session_key)
            && inner.lease_epoch == lease_epoch
            && inner.applied_lease_epoch < lease_epoch
        {
            inner.applied_lease_epoch = lease_epoch;
            self.sync_atomics(&inner);
            self.wait_cv.notify_all();
        }
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

    pub fn set_state_synced(&self, new_state: MaintenanceGateState) -> Result<(), DriverError> {
        let event = {
            let mut inner = self.inner.lock().unwrap();
            let mut event = None;
            let mut changed = false;
            if inner.state != new_state {
                inner.state = new_state;
                changed = true;
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
                    changed = true;
                }
            }
            self.sync_atomics(&inner);
            self.wait_cv.notify_all();

            if changed {
                self.send_control_command_locked(
                    MaintenanceControlOp::SetState {
                        state: inner.state,
                        holder_session_id: inner.holder_session_id,
                        holder_session_key: inner.holder_session_key,
                        lease_epoch: inner.lease_epoch,
                    },
                    false,
                )?;
            }
            event
        };

        if let Some(event) = event {
            self.emit(event);
        }
        Ok(())
    }

    pub fn acquire_blocking(
        &self,
        session_id: u32,
        session_key: u64,
        timeout: Duration,
    ) -> Result<MaintenanceLeaseAcquireResult, DriverError> {
        let deadline = Instant::now() + timeout;
        let mut inner = self.inner.lock().unwrap();
        loop {
            if !inner.state.allows_lease() {
                return Ok(MaintenanceLeaseAcquireResult::DeniedState { state: inner.state });
            }

            match inner.holder_session_key {
                None => {
                    inner.holder_session_id = Some(session_id);
                    inner.holder_session_key = Some(session_key);
                    inner.lease_epoch = inner.lease_epoch.wrapping_add(1);
                    let lease_epoch = inner.lease_epoch;
                    self.sync_atomics(&inner);
                    self.wait_cv.notify_all();
                    let ack_rx = match self.send_control_command_locked(
                        MaintenanceControlOp::Grant {
                            session_id,
                            session_key,
                            lease_epoch,
                        },
                        true,
                    ) {
                        Ok(Some(ack_rx)) => ack_rx,
                        Ok(None) => unreachable!("blocking grant must return ack"),
                        Err(err) => {
                            inner.holder_session_id = None;
                            inner.holder_session_key = None;
                            inner.lease_epoch = inner.lease_epoch.wrapping_add(1);
                            self.sync_atomics(&inner);
                            self.wait_cv.notify_all();
                            return Err(err);
                        },
                    };
                    drop(inner);
                    if let Err(err) = Self::wait_control_ack_until(ack_rx, deadline) {
                        let _ = self.rollback_tentative_grant(session_key, lease_epoch);
                        return Err(err);
                    }
                    self.mark_grant_applied(session_key, lease_epoch);
                    return Ok(MaintenanceLeaseAcquireResult::Granted { lease_epoch });
                },
                Some(holder_key) if holder_key == session_key => {
                    if inner.applied_lease_epoch >= inner.lease_epoch {
                        return Ok(MaintenanceLeaseAcquireResult::Granted {
                            lease_epoch: inner.lease_epoch,
                        });
                    }

                    let now = Instant::now();
                    if now >= deadline {
                        return Err(DriverError::Timeout);
                    }

                    let timeout = deadline.saturating_duration_since(now);
                    let (next_inner, wait_result) =
                        self.wait_cv.wait_timeout(inner, timeout).unwrap();
                    inner = next_inner;

                    if wait_result.timed_out()
                        && inner.holder_session_key == Some(session_key)
                        && inner.applied_lease_epoch < inner.lease_epoch
                    {
                        return Err(DriverError::Timeout);
                    }
                },
                _ => {
                    let now = Instant::now();
                    if now >= deadline {
                        return Ok(MaintenanceLeaseAcquireResult::DeniedHeld {
                            holder_session_id: inner.holder_session_id,
                        });
                    }
                    let timeout = deadline.saturating_duration_since(now);
                    let (next_inner, wait_result) =
                        self.wait_cv.wait_timeout(inner, timeout).unwrap();
                    inner = next_inner;
                    if wait_result.timed_out() {
                        return Ok(MaintenanceLeaseAcquireResult::DeniedHeld {
                            holder_session_id: inner.holder_session_id,
                        });
                    }
                },
            }
        }
    }

    pub fn release_if_holder(&self, session_key: u64) -> Result<bool, DriverError> {
        {
            let mut inner = self.inner.lock().unwrap();
            if inner.holder_session_key != Some(session_key) {
                return Ok(false);
            }
            inner.holder_session_id = None;
            inner.holder_session_key = None;
            inner.lease_epoch = inner.lease_epoch.wrapping_add(1);
            let lease_epoch = inner.lease_epoch;
            self.sync_atomics(&inner);
            self.wait_cv.notify_all();
            match self.send_control_command_locked(
                MaintenanceControlOp::Release {
                    session_key,
                    lease_epoch,
                },
                false,
            ) {
                Ok(None) => {},
                Ok(Some(_)) => unreachable!("non-blocking release must not return ack"),
                Err(err) => return Err(err),
            }
        }
        Ok(true)
    }

    pub fn revoke_if_holder(
        &self,
        session_key: u64,
        reason: MaintenanceRevocationReason,
    ) -> Result<Option<MaintenanceRevocationEvent>, DriverError> {
        let event = {
            let mut inner = self.inner.lock().unwrap();
            if inner.holder_session_key != Some(session_key) {
                return Ok(None);
            }
            let event = self.build_revocation_event(&inner, reason);
            inner.holder_session_id = None;
            inner.holder_session_key = None;
            inner.lease_epoch = inner.lease_epoch.wrapping_add(1);
            let lease_epoch = inner.lease_epoch;
            self.sync_atomics(&inner);
            self.wait_cv.notify_all();
            match self.send_control_command_locked(
                MaintenanceControlOp::Revoke {
                    session_key,
                    lease_epoch,
                    reason,
                },
                false,
            ) {
                Ok(None) => {},
                Ok(Some(_)) => unreachable!("non-blocking revoke must not return ack"),
                Err(err) => return Err(err),
            }
            event
        };
        if let Some(event) = event {
            self.emit(event);
            Ok(Some(event))
        } else {
            Ok(None)
        }
    }

    pub fn revoke_current_holder(
        &self,
        reason: MaintenanceRevocationReason,
    ) -> Result<Option<MaintenanceRevocationEvent>, DriverError> {
        let event = {
            let mut inner = self.inner.lock().unwrap();
            let Some(session_key) = inner.holder_session_key else {
                return Ok(None);
            };
            let event = self.build_revocation_event(&inner, reason);
            inner.holder_session_id = None;
            inner.holder_session_key = None;
            inner.lease_epoch = inner.lease_epoch.wrapping_add(1);
            let lease_epoch = inner.lease_epoch;
            self.sync_atomics(&inner);
            self.wait_cv.notify_all();
            match self.send_control_command_locked(
                MaintenanceControlOp::Revoke {
                    session_key,
                    lease_epoch,
                    reason,
                },
                false,
            ) {
                Ok(None) => {},
                Ok(Some(_)) => unreachable!("non-blocking revoke must not return ack"),
                Err(err) => return Err(err),
            }
            event
        };
        if let Some(event) = event {
            self.emit(event);
            Ok(Some(event))
        } else {
            Ok(None)
        }
    }

    pub fn is_valid(&self, session_key: u64, lease_epoch: u64) -> bool {
        self.holder_session_key.load(Ordering::Acquire) == session_key
            && self.lease_epoch.load(Ordering::Acquire) == lease_epoch
            && self.current_state().allows_lease()
    }

    pub fn local_set_state(&self, new_state: MaintenanceGateState) {
        self.set_state(new_state);
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
        recv_until_deadline(&self.ack_rx, self.deadline, || DriverError::Timeout)?
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
        DriverError::ReplayModeActive => DriverError::ReplayModeActive,
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
        DriverError::ReliablePackageDeliveryFailed {
            sent,
            total,
            source,
        } => DriverError::ReliablePackageDeliveryFailed {
            sent: *sent,
            total: *total,
            source: clone_can_error(source),
        },
        DriverError::ReliablePackageTimeout { sent, total } => {
            DriverError::ReliablePackageTimeout {
                sent: *sent,
                total: *total,
            }
        },
        DriverError::CommandAbortedByFault => DriverError::CommandAbortedByFault,
        DriverError::CommandAbortedByStateTransition => {
            DriverError::CommandAbortedByStateTransition
        },
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
    /// 维护写入/授权线性化通道。
    maintenance_lane_tx: ManuallyDrop<Sender<MaintenanceLaneCommand>>,
    /// SoftRealtime 原始批命令邮箱。
    soft_realtime_tx: Arc<SoftRealtimeMailbox>,
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
    /// Pipeline 配置（用于按当前时刻重算 safety-grade 状态）。
    pipeline_config: PipelineConfig,
    /// Controller-owned maintenance gate used by bridge integrations.
    maintenance_gate: Arc<MaintenanceGate>,
    /// CAN 接口名称（用于录制元数据）
    interface: String,
    /// CAN 总线速度（bps）（用于录制元数据）
    bus_speed: u32,
    /// Driver 工作模式（用于回放模式控制）
    driver_mode: Arc<crate::mode::AtomicDriverMode>,
    /// 线性化 Driver 模式切换，避免 gate/mode 交错留下混合状态。
    mode_switch_lock: Mutex<()>,
    /// Test-only barrier that lets one Piper instance pause Replay mode publication.
    #[cfg(test)]
    mode_switch_replay_barrier: Mutex<Option<ModeSwitchBarrier>>,
    /// Test-only barrier that pauses SoftRealtime admission before enqueue.
    #[cfg(test)]
    soft_realtime_admission_barrier: Mutex<Option<SoftRealtimeAdmissionBarrier>>,
    /// Test-only barrier that pauses SoftRealtime admission after the final deadline check.
    #[cfg(test)]
    soft_realtime_post_check_barrier: Mutex<Option<SoftRealtimeAdmissionBarrier>>,
    /// Capability of the active backend.
    backend_capability: BackendCapability,
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
    pub const MAX_RELIABLE_PACKAGE_SIZE: usize = 10;

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

    fn runtime_fault_kind(&self) -> Option<RuntimeFaultKind> {
        RuntimeFaultKind::from_raw(self.runtime_fault.load(Ordering::Acquire))
    }

    fn normal_control_open(&self) -> bool {
        self.runtime_phase() == RuntimePhase::Running
            && self.runtime_fault.load(Ordering::Acquire) == 0
            && self.rx_thread_alive()
            && self.normal_send_gate.accepts_front_door_submissions()
    }

    fn reliable_front_door_open(&self, kind: ReliableCommandKind) -> bool {
        if self.runtime_phase() != RuntimePhase::Running
            || self.runtime_fault.load(Ordering::Acquire) != 0
            || !self.rx_thread_alive()
        {
            return false;
        }

        match kind {
            ReliableCommandKind::Replay => true,
            ReliableCommandKind::Standard | ReliableCommandKind::Maintenance => {
                self.normal_send_gate.accepts_front_door_submissions()
            },
        }
    }

    fn low_speed_drive_state_freshness_window_us(&self) -> u64 {
        self.pipeline_config.low_speed_drive_state_freshness_ms.saturating_mul(1_000)
    }

    fn rebuilt_low_speed_observation_freshness_window_us(&self) -> u64 {
        REBUILT_LOW_SPEED_OBSERVATION_FRESHNESS_WINDOW_US
    }

    fn end_pose_freshness_window_us(&self) -> u64 {
        END_POSE_FRESHNESS_WINDOW_US
    }

    fn replay_mode_active(&self) -> bool {
        self.mode().is_replay()
    }

    fn replay_barrier_active(&self) -> bool {
        self.normal_send_gate.is_replay_paused()
    }

    #[cfg(test)]
    fn install_mode_switch_replay_barrier(
        &self,
        reached_tx: std::sync::mpsc::Sender<()>,
        release_rx: std::sync::mpsc::Receiver<()>,
    ) {
        let mut guard = self
            .mode_switch_replay_barrier
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *guard = Some(ModeSwitchBarrier {
            reached_tx,
            release_rx,
        });
    }

    #[cfg(test)]
    fn maybe_wait_test_mode_switch_replay_barrier(&self) {
        let barrier = self
            .mode_switch_replay_barrier
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        if let Some(barrier) = barrier {
            let _ = barrier.reached_tx.send(());
            let _ = barrier.release_rx.recv();
        }
    }

    #[cfg(not(test))]
    fn maybe_wait_test_mode_switch_replay_barrier(&self) {}

    #[cfg(test)]
    fn install_soft_realtime_admission_barrier(
        &self,
        reached_tx: std::sync::mpsc::Sender<()>,
        release_rx: std::sync::mpsc::Receiver<()>,
    ) {
        let mut guard = self
            .soft_realtime_admission_barrier
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *guard = Some(SoftRealtimeAdmissionBarrier {
            reached_tx,
            release_rx,
        });
    }

    #[cfg(test)]
    fn maybe_wait_test_soft_realtime_admission_barrier(&self) {
        let barrier = self
            .soft_realtime_admission_barrier
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        if let Some(barrier) = barrier {
            let _ = barrier.reached_tx.send(());
            let _ = barrier.release_rx.recv();
        }
    }

    #[cfg(not(test))]
    fn maybe_wait_test_soft_realtime_admission_barrier(&self) {}

    #[cfg(test)]
    fn install_soft_realtime_post_check_barrier(
        &self,
        reached_tx: std::sync::mpsc::Sender<()>,
        release_rx: std::sync::mpsc::Receiver<()>,
    ) {
        let mut guard = self
            .soft_realtime_post_check_barrier
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *guard = Some(SoftRealtimeAdmissionBarrier {
            reached_tx,
            release_rx,
        });
    }

    #[cfg(test)]
    fn maybe_wait_test_soft_realtime_post_check_barrier(&self) {
        let barrier = self
            .soft_realtime_post_check_barrier
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        if let Some(barrier) = barrier {
            let _ = barrier.reached_tx.send(());
            let _ = barrier.release_rx.recv();
        }
    }

    #[cfg(not(test))]
    fn maybe_wait_test_soft_realtime_post_check_barrier(&self) {}

    fn ensure_mode_allows_reliable_kind(
        &self,
        kind: ReliableCommandKind,
    ) -> Result<(), DriverError> {
        if self.replay_mode_active() {
            return match kind {
                ReliableCommandKind::Replay => Ok(()),
                ReliableCommandKind::Standard | ReliableCommandKind::Maintenance => {
                    Err(DriverError::ReplayModeActive)
                },
            };
        }

        if self.replay_barrier_active() {
            return match kind {
                ReliableCommandKind::Replay => Err(DriverError::InvalidInput(
                    "replay frames may only be sent while DriverMode::Replay is active".to_string(),
                )),
                ReliableCommandKind::Standard | ReliableCommandKind::Maintenance => {
                    Err(DriverError::ReplayModeActive)
                },
            };
        }

        match kind {
            ReliableCommandKind::Replay => Err(DriverError::InvalidInput(
                "replay frames may only be sent while DriverMode::Replay is active".to_string(),
            )),
            ReliableCommandKind::Standard | ReliableCommandKind::Maintenance => Ok(()),
        }
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
    pub fn new_dual_thread<C>(can: C, config: Option<PipelineConfig>) -> Result<Self, DriverError>
    where
        C: SplittableAdapter + Send + 'static,
        C::RxAdapter: Send + 'static,
        C::TxAdapter: Send + 'static,
    {
        Self::new_dual_thread_with_startup_timeout(can, config, STRICT_TIMESTAMP_VALIDATION_TIMEOUT)
    }

    /// 使用显式的 strict 启动验收超时创建双线程 runtime。
    pub fn new_dual_thread_with_startup_timeout<C>(
        can: C,
        config: Option<PipelineConfig>,
        startup_timeout: Duration,
    ) -> Result<Self, DriverError>
    where
        C: SplittableAdapter + Send + 'static,
        C::RxAdapter: Send + 'static,
        C::TxAdapter: Send + 'static,
    {
        let startup_deadline = StartupValidationDeadline::after(startup_timeout);
        let (rx_adapter, tx_adapter) = can.split().map_err(DriverError::Can)?;
        Self::new_dual_thread_parts_with_startup_deadline(
            rx_adapter,
            tx_adapter,
            config,
            startup_deadline,
        )
    }

    /// 使用已拆分的 RX/TX 适配器创建双线程 runtime。
    pub fn new_dual_thread_parts(
        rx_adapter: impl RxAdapter + Send + 'static,
        tx_adapter: impl RealtimeTxAdapter + Send + 'static,
        config: Option<PipelineConfig>,
    ) -> Result<Self, DriverError> {
        Self::new_dual_thread_parts_with_startup_timeout(
            rx_adapter,
            tx_adapter,
            config,
            STRICT_TIMESTAMP_VALIDATION_TIMEOUT,
        )
    }

    /// 使用显式的 strict 启动验收超时创建双线程 runtime。
    pub fn new_dual_thread_parts_with_startup_timeout(
        rx_adapter: impl RxAdapter + Send + 'static,
        tx_adapter: impl RealtimeTxAdapter + Send + 'static,
        config: Option<PipelineConfig>,
        startup_timeout: Duration,
    ) -> Result<Self, DriverError> {
        let startup_deadline = StartupValidationDeadline::after(startup_timeout);
        Self::new_dual_thread_parts_with_startup_deadline(
            rx_adapter,
            tx_adapter,
            config,
            startup_deadline,
        )
    }

    pub(crate) fn new_dual_thread_parts_with_startup_deadline(
        rx_adapter: impl RxAdapter + Send + 'static,
        tx_adapter: impl RealtimeTxAdapter + Send + 'static,
        config: Option<PipelineConfig>,
        startup_deadline: StartupValidationDeadline,
    ) -> Result<Self, DriverError> {
        let mut rx_adapter = rx_adapter;
        let probed_capability = rx_adapter
            .startup_probe_until(startup_deadline.instant_deadline())
            .map_err(DriverError::Can)?;
        let backend_capability =
            probed_capability.unwrap_or_else(|| rx_adapter.backend_capability());
        let startup_validated = probed_capability.is_some();

        let piper = Self::new_dual_thread_parts_internal(
            rx_adapter,
            tx_adapter,
            config,
            backend_capability,
        )
        .map_err(DriverError::Can)?;
        if startup_validated {
            Ok(piper)
        } else {
            piper.validate_startup_until(startup_deadline)
        }
    }

    fn new_dual_thread_parts_internal(
        rx_adapter: impl RxAdapter + Send + 'static,
        tx_adapter: impl RealtimeTxAdapter + Send + 'static,
        config: Option<PipelineConfig>,
        backend_capability: BackendCapability,
    ) -> Result<Self, CanError> {
        let pipeline_config = config.unwrap_or_default();
        let realtime_slot = Arc::new(std::sync::Mutex::new(None::<RealtimeCommand>));
        let (reliable_tx, reliable_rx) = crossbeam_channel::bounded::<ReliableCommand>(10);
        let soft_realtime_tx = Arc::new(SoftRealtimeMailbox::new());
        let soft_realtime_rx = soft_realtime_tx.clone();
        let shutdown_lane = Arc::new(ShutdownLane::new());
        let metrics = Arc::new(PiperMetrics::new());
        let ctx = Arc::new(PiperContext::with_metrics(metrics.clone()));
        let workers_running = Arc::new(AtomicBool::new(true));
        let runtime_phase = Arc::new(AtomicU8::new(RuntimePhase::Running as u8));
        let normal_send_gate = Arc::new(NormalSendGate::new());
        let runtime_fault = Arc::new(AtomicU8::new(0));
        let maintenance_gate = Arc::new(MaintenanceGate::default());
        let driver_mode = Arc::new(crate::mode::AtomicDriverMode::new(
            crate::mode::DriverMode::Normal,
        ));
        let (maintenance_lane_tx, maintenance_lane_rx) =
            crossbeam_channel::unbounded::<MaintenanceLaneCommand>();
        maintenance_gate.set_lane_sink(maintenance_lane_tx.clone());

        let ctx_clone = ctx.clone();
        let workers_running_clone = workers_running.clone();
        let runtime_phase_rx = runtime_phase.clone();
        let metrics_clone = metrics.clone();
        let runtime_fault_rx = runtime_fault.clone();
        let config_clone = pipeline_config.clone();
        let backend_capability_rx = backend_capability;
        let maintenance_gate_rx = maintenance_gate.clone();
        let normal_send_gate_rx = normal_send_gate.clone();
        let driver_mode_rx = driver_mode.clone();

        let rx_thread = spawn(move || {
            crate::pipeline::rx_loop(
                rx_adapter,
                backend_capability_rx,
                ctx_clone,
                config_clone,
                workers_running_clone,
                runtime_phase_rx,
                normal_send_gate_rx,
                driver_mode_rx,
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
        let backend_capability_tx = backend_capability;
        let driver_mode_tx = driver_mode.clone();
        let config_tx = pipeline_config.clone();

        let tx_thread = spawn(move || {
            crate::pipeline::tx_loop_mailbox(
                tx_adapter,
                backend_capability_tx,
                config_tx,
                realtime_slot_tx,
                soft_realtime_rx,
                shutdown_lane_tx,
                reliable_rx,
                workers_running_tx,
                runtime_phase_tx,
                normal_send_gate_tx,
                metrics_tx,
                ctx_tx,
                runtime_fault_tx,
                maintenance_lane_rx,
                maintenance_gate_tx,
                driver_mode_tx,
            );
        });

        Ok(Self {
            reliable_tx: ManuallyDrop::new(reliable_tx),
            maintenance_lane_tx: ManuallyDrop::new(maintenance_lane_tx),
            soft_realtime_tx,
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
            pipeline_config,
            maintenance_gate,
            interface: "unknown".to_string(),
            bus_speed: 1_000_000,
            driver_mode,
            mode_switch_lock: Mutex::new(()),
            #[cfg(test)]
            mode_switch_replay_barrier: Mutex::new(None),
            #[cfg(test)]
            soft_realtime_admission_barrier: Mutex::new(None),
            #[cfg(test)]
            soft_realtime_post_check_barrier: Mutex::new(None),
            backend_capability,
        })
    }

    #[cfg(test)]
    pub(crate) fn new_dual_thread_parts_unvalidated(
        rx_adapter: impl RxAdapter + Send + 'static,
        tx_adapter: impl RealtimeTxAdapter + Send + 'static,
        config: Option<PipelineConfig>,
    ) -> Result<Self, CanError> {
        let backend_capability = rx_adapter.backend_capability();
        Self::new_dual_thread_parts_internal(rx_adapter, tx_adapter, config, backend_capability)
    }

    fn validate_startup_until(
        self,
        startup_deadline: StartupValidationDeadline,
    ) -> Result<Self, DriverError> {
        if !self.backend_capability.is_strict_realtime() {
            return Ok(self);
        }

        if let Err(error) = self.wait_for_timestamped_feedback_until(startup_deadline) {
            self.request_stop();
            return Err(error);
        }

        Ok(self)
    }

    pub fn backend_capability(&self) -> BackendCapability {
        self.backend_capability
    }

    /// 获取运行时健康状态。
    pub fn health(&self) -> HealthStatus {
        let rx_alive = self.rx_thread_alive();
        let tx_alive = self.tx_thread_alive();
        let connected = self.is_connected();
        let last_feedback_age = self.connection_age();
        let runtime_fault = self.runtime_fault_kind();

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

    /// 锁存手动故障并关闭正常控制路径。
    ///
    /// 进入故障锁存后：
    /// - 新的 realtime / normal reliable 控制命令会被拒绝
    /// - TX 线程会主动清空 pending realtime slot 和 normal reliable queue
    /// - shutdown lane 仍然保持可用，用于 bounded stop attempt
    /// - `health().fault` 会稳定返回 `Some(RuntimeFaultKind::ManualFault)`
    pub fn latch_fault(&self) {
        let previous = latch_runtime_fault_state(
            &self.runtime_phase,
            &self.normal_send_gate,
            &self.runtime_fault,
            RuntimeFaultKind::ManualFault,
        );
        if previous == RuntimePhase::Stopping {
            return;
        }
        self.clear_realtime_slot(DriverError::CommandAbortedByFault, true);
        let _ = self.maintenance_gate.set_state_synced(MaintenanceGateState::DeniedFaulted);
    }

    fn latch_fault_with_kind(&self, fault: RuntimeFaultKind) {
        let previous = latch_runtime_fault_state(
            &self.runtime_phase,
            &self.normal_send_gate,
            &self.runtime_fault,
            fault,
        );
        if previous == RuntimePhase::Stopping {
            return;
        }
        self.clear_realtime_slot(DriverError::CommandAbortedByFault, true);
        let _ = self.maintenance_gate.set_state_synced(MaintenanceGateState::DeniedFaulted);
    }

    fn latch_state_transition_timeout_fault_fast(&self) {
        if self
            .runtime_phase
            .compare_exchange(
                RuntimePhase::Running as u8,
                RuntimePhase::FaultLatched as u8,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
        {
            let _ = self.runtime_fault.compare_exchange(
                0,
                RuntimeFaultKind::TransportError as u8,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
            self.normal_send_gate.close_for_fault();
            self.normal_send_gate.maybe_wait_test_fault_latch_barrier();
            self.clear_realtime_slot(DriverError::CommandAbortedByFault, true);
            let _ = self.maintenance_gate.set_state_synced(MaintenanceGateState::DeniedFaulted);
        }
    }

    /// 请求 worker 停止并关闭所有命令通路。
    pub fn request_stop(&self) {
        self.runtime_phase.store(RuntimePhase::Stopping as u8, Ordering::Release);
        self.normal_send_gate.close_for_stop();
        self.workers_running.store(false, Ordering::Release);
        self.shutdown_lane.close_with(Err(DriverError::ChannelClosed));
        self.clear_realtime_slot(DriverError::ChannelClosed, false);
        let _ = self
            .maintenance_gate
            .set_state_synced(MaintenanceGateState::DeniedTransportDown);
    }

    fn validate_manual_fault_recovery_preconditions(&self) -> Result<(), DriverError> {
        if !self.rx_thread_alive() || !self.tx_thread_alive() {
            return Err(DriverError::ChannelClosed);
        }
        if self.replay_mode_active() || self.replay_barrier_active() {
            return Err(DriverError::ReplayModeActive);
        }
        if self.runtime_phase() != RuntimePhase::FaultLatched {
            return Err(DriverError::ControlPathClosed);
        }
        if self.runtime_fault_kind() != Some(RuntimeFaultKind::ManualFault) {
            return Err(DriverError::InvalidInput(
                "manual fault recovery requires a latched ManualFault".to_string(),
            ));
        }
        Ok(())
    }

    fn manual_fault_recovery_result_from_confirmed_mask(
        confirmed_mask: u8,
    ) -> (ManualFaultRecoveryResult, MaintenanceGateState) {
        if confirmed_mask == 0 {
            (
                ManualFaultRecoveryResult::Standby,
                MaintenanceGateState::AllowedStandby,
            )
        } else {
            (
                ManualFaultRecoveryResult::Maintenance { confirmed_mask },
                MaintenanceGateState::DeniedActiveControl,
            )
        }
    }

    fn finalize_manual_fault_recovery(
        &self,
        confirmed_mask: u8,
    ) -> Result<ManualFaultRecoveryResult, DriverError> {
        let _mode_switch_guard =
            self.mode_switch_lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());

        self.validate_manual_fault_recovery_preconditions()?;

        let (result, maintenance_state) =
            Self::manual_fault_recovery_result_from_confirmed_mask(confirmed_mask);
        self.maintenance_gate.set_state_synced(maintenance_state)?;
        self.runtime_fault.store(0, Ordering::Release);

        match self.runtime_phase.compare_exchange(
            RuntimePhase::FaultLatched as u8,
            RuntimePhase::Running as u8,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => {
                self.normal_send_gate.reopen_after_fault();
                // Publish the controller-visible maintenance state after reopening the
                // runtime so observers do not depend on the next RX refresh tick.
                self.maintenance_gate.local_set_state(maintenance_state);
                Ok(result)
            },
            Err(observed) => match RuntimePhase::from_raw(observed) {
                RuntimePhase::Stopping => {
                    self.runtime_fault
                        .store(RuntimeFaultKind::ManualFault as u8, Ordering::Release);
                    Err(DriverError::ChannelClosed)
                },
                RuntimePhase::Running | RuntimePhase::FaultLatched => {
                    self.runtime_fault
                        .store(RuntimeFaultKind::ManualFault as u8, Ordering::Release);
                    Err(DriverError::ControlPathClosed)
                },
            },
        }
    }

    #[doc(hidden)]
    pub fn complete_manual_fault_recovery_after_resume_until(
        &self,
        deadline: Instant,
        poll_interval: Duration,
    ) -> Result<ManualFaultRecoveryResult, DriverError> {
        self.validate_manual_fault_recovery_preconditions()?;

        let baseline_driver_state = self.ctx.joint_driver_low_speed.load().as_ref().clone();
        let post_resume_baseline = baseline_driver_state.post_resume_feedback_baseline();
        let baseline_host_mono_us = crate::heartbeat::monotonic_micros();

        loop {
            self.validate_manual_fault_recovery_preconditions()?;

            let now = Instant::now();
            if now >= deadline {
                return Err(DriverError::Timeout);
            }

            let driver_state = self.ctx.joint_driver_low_speed.load().as_ref().clone();
            let now_host_mono_us = crate::heartbeat::monotonic_micros();
            if let Some(confirmed_mask) = driver_state
                .confirmed_driver_enabled_mask_after_post_resume_feedback(
                    post_resume_baseline,
                    baseline_host_mono_us,
                    now_host_mono_us,
                    self.low_speed_drive_state_freshness_window_us(),
                )
            {
                return self.finalize_manual_fault_recovery(confirmed_mask);
            }

            std::thread::sleep(poll_interval.min(deadline.saturating_duration_since(now)));
        }
    }

    #[doc(hidden)]
    pub fn best_effort_disable_or_shutdown_on_drop(&self, shutdown_timeout: Duration) {
        use piper_protocol::control::MotorEnableCommand;

        match self.runtime_phase() {
            RuntimePhase::Running => {
                if !self.tx_thread_alive() {
                    warn!("Drop auto-disable skipped because TX worker is no longer alive");
                    return;
                }

                let disable_frame = MotorEnableCommand::disable_all().to_frame();
                let disable_start = Instant::now();
                match self
                    .send_local_state_transition_frame_confirmed(disable_frame, shutdown_timeout)
                {
                    Ok(()) => {},
                    Err(error) if self.runtime_phase() == RuntimePhase::FaultLatched => {
                        let remaining = shutdown_timeout.saturating_sub(disable_start.elapsed());
                        if remaining.is_zero() {
                            warn!(
                                "Drop auto-disable raced with a fault latch and exhausted the {:?} budget before fallback shutdown could run",
                                shutdown_timeout
                            );
                            return;
                        }

                        warn!(
                            "Drop auto-disable raced with a fault latch ({error}); falling back to bounded shutdown-lane emergency stop"
                        );
                        self.best_effort_fault_shutdown_on_drop(remaining);
                    },
                    Err(DriverError::Timeout) => {
                        warn!(
                            "Drop auto-disable timed out after {:?} while aborting pending normal control or waiting for maintenance confirmation",
                            shutdown_timeout
                        );
                    },
                    Err(error) => {
                        warn!("Drop auto-disable failed: {error}");
                    },
                }
            },
            RuntimePhase::FaultLatched => {
                self.best_effort_fault_shutdown_on_drop(shutdown_timeout);
            },
            RuntimePhase::Stopping => {
                self.metrics.tx_drop_shutdown_skipped_total.fetch_add(1, Ordering::Relaxed);
                warn!("Drop auto-disable skipped because runtime is already stopping");
            },
        }
    }

    fn best_effort_fault_shutdown_on_drop(&self, shutdown_timeout: Duration) {
        use piper_protocol::control::EmergencyStopCommand;

        if !self.tx_thread_alive() {
            self.metrics.tx_drop_shutdown_skipped_total.fetch_add(1, Ordering::Relaxed);
            warn!(
                "Drop fault shutdown skipped because runtime is fault-latched but TX worker is no longer alive"
            );
            return;
        }

        self.metrics.tx_drop_shutdown_attempt_total.fetch_add(1, Ordering::Relaxed);

        let frame = EmergencyStopCommand::emergency_stop().to_frame();
        match self
            .enqueue_shutdown(frame, Instant::now() + shutdown_timeout)
            .and_then(|receipt| receipt.wait())
        {
            Ok(()) => {
                self.metrics.tx_drop_shutdown_success_total.fetch_add(1, Ordering::Relaxed);
                info!("Drop fault shutdown sent bounded emergency stop through shutdown lane");
            },
            Err(DriverError::Timeout) => {
                self.metrics.tx_drop_shutdown_timeout_total.fetch_add(1, Ordering::Relaxed);
                warn!(
                    "Drop fault shutdown timed out after {:?} while waiting for shutdown-lane confirmation",
                    shutdown_timeout
                );
            },
            Err(error) => {
                self.metrics.tx_drop_shutdown_skipped_total.fetch_add(1, Ordering::Relaxed);
                warn!("Drop fault shutdown failed before completion: {error}");
            },
        }
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
    /// - 无锁读取（固定槽位快照单元）
    /// - 返回快照副本（按值复制，< 150 字节）
    /// - 适合 500Hz 控制循环
    pub fn get_joint_dynamic(&self) -> JointDynamicState {
        self.get_joint_dynamic_monitor_snapshot()
            .latest_complete_cloned()
            .unwrap_or_default()
    }

    /// 获取控制级关节动态状态。
    ///
    /// 返回最近一份 coherent control pair 中的 dynamic 状态，
    /// 不会暴露仅单边推进的控制候选值；如果 coherent pair 尚未建立或反馈已过期则返回 `None`。
    pub fn get_control_joint_dynamic(
        &self,
        max_feedback_age: Duration,
    ) -> Option<JointDynamicState> {
        self.ctx.capture_control_joint_dynamic(max_feedback_age)
    }

    /// 获取原始关节动态状态（允许部分动态组，仅供诊断）
    pub fn get_raw_joint_dynamic(&self) -> JointDynamicState {
        *self.get_joint_dynamic_monitor_snapshot().latest_raw()
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
    /// - 无锁读取（固定槽位快照单元）
    /// - 返回快照副本（按值复制）
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
        *self.get_joint_position_monitor_snapshot().latest_raw()
    }

    /// 获取关节位置监控快照（完整监控 + raw 诊断）
    pub fn get_joint_position_monitor_snapshot(&self) -> JointPositionMonitorSnapshot {
        self.ctx.capture_joint_position_monitor_snapshot()
    }

    fn observe_end_pose_at(&self, now_host_mono_us: u64) -> Observation<EndPose, PartialEndPose> {
        let Ok(store) = self.ctx.end_pose_observation.read() else {
            return Observation::Unavailable;
        };

        store.observe(
            now_host_mono_us,
            self.end_pose_freshness_window_us(),
            EndPose::from_slots,
        )
    }

    fn observe_joint_driver_low_speed_at(
        &self,
        now_host_mono_us: u64,
    ) -> Observation<JointDriverLowSpeed, PartialJointDriverLowSpeed> {
        let Ok(store) = self.ctx.joint_driver_low_speed_observation.read() else {
            return Observation::Unavailable;
        };

        store.observe(
            now_host_mono_us,
            self.rebuilt_low_speed_observation_freshness_window_us(),
            JointDriverLowSpeed::from_slots,
        )
    }

    fn complete_from_observation<T, TPartial>(
        observation: Observation<T, TPartial>,
    ) -> Option<Complete<T>> {
        match observation {
            Observation::Available(available) => match available.payload {
                ObservationPayload::Complete(value) => Some(Complete {
                    value,
                    meta: available.meta,
                }),
                ObservationPayload::Partial { .. } => None,
            },
            Observation::Unavailable => None,
        }
    }

    #[cfg(test)]
    fn get_joint_driver_low_speed_at(
        &self,
        now_host_mono_us: u64,
    ) -> Observation<JointDriverLowSpeed, PartialJointDriverLowSpeed> {
        self.observe_joint_driver_low_speed_at(now_host_mono_us)
    }

    /// 获取末端位姿观测（无锁，纳秒级返回）
    ///
    /// 末端执行器的位置和姿态信息通过 `Observation` 返回，完整性和新鲜度正交表达。
    pub fn get_end_pose(&self) -> Observation<EndPose, PartialEndPose> {
        self.observe_end_pose_at(crate::heartbeat::monotonic_micros().max(1))
    }

    /// 获取原始末端位姿状态（允许部分帧组，仅供诊断）
    pub fn get_raw_end_pose(&self) -> EndPoseState {
        *self.get_end_pose_monitor_snapshot().latest_raw()
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
    /// - 无锁读取（单次固定槽位快照读取）
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
        let mut control = self.ctx.robot_control.load().as_ref().clone();
        let driver_state = self.ctx.joint_driver_low_speed.load();
        control.confirmed_driver_enabled_mask = driver_state.confirmed_driver_enabled_mask(
            crate::heartbeat::monotonic_micros(),
            self.low_speed_drive_state_freshness_window_us(),
        );
        control
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

    /// 获取关节驱动器低速反馈观测（无锁）
    ///
    /// 返回 `Observation`，将完整性和新鲜度分离表达。
    pub fn get_joint_driver_low_speed(
        &self,
    ) -> Observation<JointDriverLowSpeed, PartialJointDriverLowSpeed> {
        self.observe_joint_driver_low_speed_at(crate::heartbeat::monotonic_micros().max(1))
    }

    #[doc(hidden)]
    pub fn confirmed_driver_enabled_mask_after_host_mono(
        &self,
        min_host_rx_mono_us: u64,
    ) -> Option<u8> {
        let driver_state = self.ctx.joint_driver_low_speed.load();
        driver_state.confirmed_driver_enabled_mask_after_host_mono(
            min_host_rx_mono_us,
            crate::heartbeat::monotonic_micros().max(1),
            self.low_speed_drive_state_freshness_window_us(),
        )
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

    fn wait_for_cached_update<T, F>(
        &self,
        deadline: Instant,
        poll_interval: Duration,
        mut try_read: F,
    ) -> Result<T, DriverError>
    where
        F: FnMut(&Self) -> Result<Option<T>, DriverError>,
    {
        loop {
            if let Some(value) = try_read(self)? {
                return Ok(value);
            }

            if Instant::now() >= deadline {
                return Err(DriverError::Timeout);
            }

            if !self.rx_thread_alive() || !self.tx_thread_alive() {
                return Err(DriverError::ChannelClosed);
            }

            let sleep_duration =
                poll_interval.min(deadline.saturating_duration_since(Instant::now()));
            if sleep_duration.is_zero() {
                std::thread::yield_now();
            } else {
                std::thread::sleep(sleep_duration);
            }
        }
    }

    fn try_begin_query(&self, kind: QueryKind) -> Result<QueryGuard<'_>, QueryError> {
        self.ctx.query_coordinator.try_begin(kind).map_err(|err| {
            if matches!(err, QueryError::Busy) {
                self.ctx.diagnostics.push(DiagnosticEvent::Query(QueryDiagnostic::Busy));
            }
            err
        })
    }

    fn classify_query_timeout(
        &self,
        kind: QueryKind,
        diagnostics_rx: &Receiver<DiagnosticEvent>,
    ) -> QueryError {
        if diagnostics_rx
            .try_iter()
            .any(|event| Self::is_query_relevant_diagnostic(kind, &event))
        {
            self.ctx.diagnostics.push(DiagnosticEvent::Query(
                QueryDiagnostic::DiagnosticsOnlyTimeout { query: kind },
            ));
            QueryError::DiagnosticsOnlyTimeout
        } else {
            QueryError::Timeout
        }
    }

    fn is_query_relevant_diagnostic(kind: QueryKind, event: &DiagnosticEvent) -> bool {
        match event {
            DiagnosticEvent::Query(QueryDiagnostic::UnexpectedFrameForActiveQuery {
                query,
                ..
            }) => *query == kind,
            DiagnosticEvent::Query(QueryDiagnostic::DiagnosticsOnlyTimeout { query }) => {
                *query == kind
            },
            DiagnosticEvent::Query(QueryDiagnostic::Busy) => false,
            DiagnosticEvent::Protocol(diagnostic) => match kind {
                QueryKind::CollisionProtection => match diagnostic {
                    ProtocolDiagnostic::InvalidLength { can_id, .. } => {
                        *can_id == piper_protocol::ids::ID_COLLISION_PROTECTION_LEVEL_FEEDBACK
                    },
                    ProtocolDiagnostic::OutOfRange { field, .. }
                    | ProtocolDiagnostic::UnsupportedValue { field, .. } => {
                        *field == "collision_protection_level"
                    },
                    _ => false,
                },
                QueryKind::JointLimit => match diagnostic {
                    ProtocolDiagnostic::InvalidLength { can_id, .. } => {
                        *can_id == piper_protocol::ids::ID_MOTOR_LIMIT_FEEDBACK
                    },
                    ProtocolDiagnostic::OutOfRange { field, .. } => *field == "joint_index",
                    ProtocolDiagnostic::UnsupportedValue { field, .. } => {
                        *field == "motor_limit_feedback"
                    },
                    _ => false,
                },
                QueryKind::JointAccel => match diagnostic {
                    ProtocolDiagnostic::InvalidLength { can_id, .. } => {
                        *can_id == piper_protocol::ids::ID_MOTOR_MAX_ACCEL_FEEDBACK
                    },
                    ProtocolDiagnostic::OutOfRange { field, .. } => *field == "joint_index",
                    ProtocolDiagnostic::UnsupportedValue { field, .. } => {
                        *field == "motor_max_accel_feedback"
                    },
                    _ => false,
                },
                QueryKind::EndLimit => match diagnostic {
                    ProtocolDiagnostic::InvalidLength { can_id, .. } => {
                        *can_id == piper_protocol::ids::ID_END_VELOCITY_ACCEL_FEEDBACK
                    },
                    ProtocolDiagnostic::UnsupportedValue { field, .. } => {
                        *field == "end_velocity_accel_feedback"
                    },
                    _ => false,
                },
            },
        }
    }

    /// 主动查询碰撞保护等级并等待新反馈。
    pub fn query_collision_protection(
        &self,
        timeout: Duration,
    ) -> Result<Complete<CollisionProtection>, QueryError> {
        use piper_protocol::config::{ParameterQuerySetCommand, ParameterQueryType};

        let query = self.try_begin_query(QueryKind::CollisionProtection)?;
        let token = query.token();
        let diagnostics_rx = self.ctx.diagnostics.subscribe();
        self.ctx
            .collision_protection_observation
            .write()
            .map(|mut store| store.begin_query(token, u64::MAX))
            .map_err(|_| QueryError::from(DriverError::PoisonedLock))?;

        let result = (|| {
            let deadline = Instant::now() + timeout;
            let frame =
                ParameterQuerySetCommand::query(ParameterQueryType::CollisionProtectionLevel)
                    .to_frame()
                    .map_err(DriverError::from)
                    .map_err(QueryError::from)?;
            let commit_host_mono_us = self
                .send_reliable_frame_confirmed_commit_marker(frame, timeout)
                .map_err(QueryError::from)?;
            self.ctx
                .collision_protection_observation
                .write()
                .map(|mut store| {
                    store.advance_query_min_host_rx_mono_us(token, commit_host_mono_us)
                })
                .map_err(|_| QueryError::from(DriverError::PoisonedLock))?;

            self.wait_for_cached_update(deadline, CONFIG_QUERY_POLL_INTERVAL, |this| {
                this.ctx
                    .collision_protection_observation
                    .read()
                    .map_err(|_| DriverError::PoisonedLock)
                    .map(|store| store.current_complete_for_token(token))
            })
            .map_err(|error| match error {
                DriverError::Timeout => {
                    self.classify_query_timeout(QueryKind::CollisionProtection, &diagnostics_rx)
                },
                other => QueryError::from(other),
            })
        })();

        if let Ok(mut store) = self.ctx.collision_protection_observation.write() {
            store.finish_query(token);
        }

        result
    }

    /// 主动查询全部关节角度/速度限制并等待完整反馈。
    pub fn query_joint_limit_config(
        &self,
        timeout: Duration,
    ) -> Result<Complete<JointLimitConfig>, QueryError> {
        use piper_protocol::config::QueryMotorLimitCommand;

        let query = self.try_begin_query(QueryKind::JointLimit)?;
        let token = query.token();
        let diagnostics_rx = self.ctx.diagnostics.subscribe();
        self.ctx
            .joint_limit_observation
            .write()
            .map(|mut store| store.begin_query(token, u64::MAX))
            .map_err(|_| QueryError::from(DriverError::PoisonedLock))?;

        let result = (|| {
            let deadline = Instant::now() + timeout;
            let frames = (1..=6u8).map(|joint_index| {
                QueryMotorLimitCommand::query_angle_and_max_velocity(joint_index).to_frame()
            });
            let commit_host_mono_us = self
                .send_reliable_package_confirmed_commit_marker(frames, timeout)
                .map_err(QueryError::from)?;
            self.ctx
                .joint_limit_observation
                .write()
                .map(|mut store| {
                    store.advance_query_min_host_rx_mono_us(
                        token,
                        commit_host_mono_us,
                        JointLimitConfig::from_slots,
                    )
                })
                .map_err(|_| QueryError::from(DriverError::PoisonedLock))?;

            self.wait_for_cached_update(deadline, CONFIG_QUERY_POLL_INTERVAL, |this| {
                this.ctx
                    .joint_limit_observation
                    .read()
                    .map_err(|_| DriverError::PoisonedLock)
                    .map(|store| store.current_complete_for_token(token))
            })
            .map_err(|error| match error {
                DriverError::Timeout => {
                    self.classify_query_timeout(QueryKind::JointLimit, &diagnostics_rx)
                },
                other => QueryError::from(other),
            })
        })();

        if let Ok(mut store) = self.ctx.joint_limit_observation.write() {
            store.finish_query(token);
        }

        result
    }

    /// 主动查询全部关节加速度限制并等待完整反馈。
    pub fn query_joint_accel_config(
        &self,
        timeout: Duration,
    ) -> Result<Complete<JointAccelConfig>, QueryError> {
        use piper_protocol::config::QueryMotorLimitCommand;

        let query = self.try_begin_query(QueryKind::JointAccel)?;
        let token = query.token();
        let diagnostics_rx = self.ctx.diagnostics.subscribe();
        self.ctx
            .joint_accel_observation
            .write()
            .map(|mut store| store.begin_query(token, u64::MAX))
            .map_err(|_| QueryError::from(DriverError::PoisonedLock))?;

        let result = (|| {
            let deadline = Instant::now() + timeout;
            let frames = (1..=6u8).map(|joint_index| {
                QueryMotorLimitCommand::query_max_acceleration(joint_index).to_frame()
            });
            let commit_host_mono_us = self
                .send_reliable_package_confirmed_commit_marker(frames, timeout)
                .map_err(QueryError::from)?;
            self.ctx
                .joint_accel_observation
                .write()
                .map(|mut store| {
                    store.advance_query_min_host_rx_mono_us(
                        token,
                        commit_host_mono_us,
                        JointAccelConfig::from_slots,
                    )
                })
                .map_err(|_| QueryError::from(DriverError::PoisonedLock))?;

            self.wait_for_cached_update(deadline, CONFIG_QUERY_POLL_INTERVAL, |this| {
                this.ctx
                    .joint_accel_observation
                    .read()
                    .map_err(|_| DriverError::PoisonedLock)
                    .map(|store| store.current_complete_for_token(token))
            })
            .map_err(|error| match error {
                DriverError::Timeout => {
                    self.classify_query_timeout(QueryKind::JointAccel, &diagnostics_rx)
                },
                other => QueryError::from(other),
            })
        })();

        if let Ok(mut store) = self.ctx.joint_accel_observation.write() {
            store.finish_query(token);
        }

        result
    }

    /// 主动查询末端速度/加速度限制并等待新反馈。
    pub fn query_end_limit_config(
        &self,
        timeout: Duration,
    ) -> Result<Complete<EndLimitConfig>, QueryError> {
        use piper_protocol::config::{ParameterQuerySetCommand, ParameterQueryType};

        let query = self.try_begin_query(QueryKind::EndLimit)?;
        let token = query.token();
        let diagnostics_rx = self.ctx.diagnostics.subscribe();
        self.ctx
            .end_limit_observation
            .write()
            .map(|mut store| store.begin_query(token, u64::MAX))
            .map_err(|_| QueryError::from(DriverError::PoisonedLock))?;

        let result = (|| {
            let deadline = Instant::now() + timeout;
            let frame = ParameterQuerySetCommand::query(ParameterQueryType::EndVelocityAccel)
                .to_frame()
                .map_err(DriverError::from)
                .map_err(QueryError::from)?;
            let commit_host_mono_us = self
                .send_reliable_frame_confirmed_commit_marker(frame, timeout)
                .map_err(QueryError::from)?;
            self.ctx
                .end_limit_observation
                .write()
                .map(|mut store| {
                    store.advance_query_min_host_rx_mono_us(token, commit_host_mono_us)
                })
                .map_err(|_| QueryError::from(DriverError::PoisonedLock))?;

            self.wait_for_cached_update(deadline, CONFIG_QUERY_POLL_INTERVAL, |this| {
                this.ctx
                    .end_limit_observation
                    .read()
                    .map_err(|_| DriverError::PoisonedLock)
                    .map(|store| store.current_complete_for_token(token))
            })
            .map_err(|error| match error {
                DriverError::Timeout => {
                    self.classify_query_timeout(QueryKind::EndLimit, &diagnostics_rx)
                },
                other => QueryError::from(other),
            })
        })();

        if let Ok(mut store) = self.ctx.end_limit_observation.write() {
            store.finish_query(token);
        }

        result
    }

    /// 获取 0x151 控制模式回显状态（无锁）。
    ///
    /// 包含控制模式、运动模式、速度、MIT 模式和安装位置等字段。
    ///
    /// # 性能
    /// - 无锁读取（ArcSwap::load）
    /// - 返回快照副本
    pub fn get_control_mode_echo(&self) -> MasterSlaveControlModeState {
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
    pub fn get_collision_protection(&self) -> Observation<CollisionProtection> {
        let Ok(store) = self.ctx.collision_protection_observation.read() else {
            return Observation::Unavailable;
        };

        store.observe()
    }

    /// 获取最近一次设置指令应答（读锁）
    ///
    /// 包含设置指令索引及零点设置结果（按需查询）。
    pub fn get_setting_response(&self) -> Result<SettingResponseState, DriverError> {
        self.ctx
            .setting_response
            .read()
            .map(|guard| guard.clone())
            .map_err(|_| DriverError::PoisonedLock)
    }

    /// 清空最近一次设置指令应答缓存。
    ///
    /// 用于请求-应答型配置操作在发送新命令前显式失效旧缓存，
    /// 避免调用方把历史 0x476 应答误判为本次请求的确认。
    pub fn clear_setting_response(&self) -> Result<(), DriverError> {
        self.ctx
            .setting_response
            .write()
            .map(|mut guard| {
                *guard = SettingResponseState::default();
            })
            .map_err(|_| DriverError::PoisonedLock)
    }

    /// 获取关节限制配置状态（读锁）
    ///
    /// 包含关节角度限制和速度限制（按需查询）。
    ///
    /// # 性能
    /// - 读锁（RwLock::read）
    /// - 返回快照副本
    pub fn get_joint_limit_config(&self) -> Observation<JointLimitConfig, PartialJointLimitConfig> {
        let Ok(store) = self.ctx.joint_limit_observation.read() else {
            return Observation::Unavailable;
        };

        store.observe()
    }

    /// 获取关节加速度限制配置状态（读锁）
    ///
    /// 包含关节加速度限制（按需查询）。
    ///
    /// # 性能
    /// - 读锁（RwLock::read）
    /// - 返回快照副本
    pub fn get_joint_accel_config(&self) -> Observation<JointAccelConfig, PartialJointAccelConfig> {
        let Ok(store) = self.ctx.joint_accel_observation.read() else {
            return Observation::Unavailable;
        };

        store.observe()
    }

    /// 获取末端限制配置状态（读锁）
    ///
    /// 包含末端执行器的速度和加速度限制（按需查询）。
    ///
    /// # 性能
    /// - 读锁（RwLock::read）
    /// - 返回快照副本
    pub fn get_end_limit_config(&self) -> Observation<EndLimitConfig> {
        let Ok(store) = self.ctx.end_limit_observation.read() else {
            return Observation::Unavailable;
        };

        store.observe()
    }

    pub fn wait_for_complete_low_speed_state(
        &self,
        timeout: Duration,
    ) -> Result<Complete<JointDriverLowSpeed>, WaitError> {
        let deadline = Instant::now() + timeout;

        self.wait_for_cached_update(deadline, Duration::from_millis(1), |this| {
            Ok(Self::complete_from_observation(
                this.observe_joint_driver_low_speed_at(crate::heartbeat::monotonic_micros().max(1)),
            ))
        })
        .map_err(WaitError::from)
    }

    pub fn wait_for_complete_end_pose(
        &self,
        timeout: Duration,
    ) -> Result<Complete<EndPose>, WaitError> {
        let deadline = Instant::now() + timeout;

        self.wait_for_cached_update(deadline, Duration::from_millis(1), |this| {
            Ok(Self::complete_from_observation(this.observe_end_pose_at(
                crate::heartbeat::monotonic_micros().max(1),
            )))
        })
        .map_err(WaitError::from)
    }

    pub fn snapshot_diagnostics(&self) -> Vec<DiagnosticEvent> {
        self.ctx.diagnostics.snapshot()
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

    /// 获取时间对齐且仍然新鲜的运动状态（推荐用于力控算法）
    ///
    /// 以 `joint_position.hardware_timestamp_us` 为基准时间，检查时间戳差异。
    /// 仅当 coherent control pair 仍在 freshness 窗口内时才返回有效状态。
    ///
    /// # 参数
    /// - `max_time_diff_us`: 允许的最大时间戳差异（微秒），推荐值：5000（5ms）
    /// - `max_feedback_age`: 允许的最大反馈年龄；位置和动态两侧都必须满足
    ///
    /// # 返回值
    /// - `AlignmentResult::Incomplete { .. }`: coherent control pair 尚未准备好，或状态不完整
    /// - `AlignmentResult::Stale { state, age }`: coherent control pair 完整但反馈已过期
    /// - `AlignmentResult::Ok(state)`: 时间戳差异在可接受范围内，且状态完整且新鲜
    /// - `AlignmentResult::Misaligned { state, diff_us }`: 时间戳差异过大，但状态仍完整且新鲜
    pub fn get_aligned_motion(
        &self,
        max_time_diff_us: u64,
        max_feedback_age: Duration,
    ) -> AlignmentResult {
        let view = self.ctx.capture_control_read_view();
        let pair = view.pair;
        let joint_position = pair.joint_position;
        let joint_dynamic = pair.joint_dynamic;

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
            dynamic_group_span_us: joint_dynamic.group_span_us(),
            skew_us: (joint_dynamic.group_timestamp_us as i64)
                - (joint_position.hardware_timestamp_us as i64),
        };

        let max_feedback_age_us = max_feedback_age.as_micros().min(u128::from(u64::MAX)) as u64;
        let feedback_age_us = state.feedback_age().as_micros().min(u128::from(u64::MAX)) as u64;

        if pair.position_sequence == 0 || pair.dynamic_sequence == 0 || !state.is_complete() {
            return AlignmentResult::Incomplete {
                position_candidate_mask: view.position_candidate_mask,
                dynamic_candidate_mask: view.dynamic_candidate_mask,
            };
        }

        let feedback_age = state.feedback_age();
        if feedback_age_us > max_feedback_age_us {
            return AlignmentResult::Stale {
                state,
                age: feedback_age,
            };
        }

        let time_diff =
            joint_position.hardware_timestamp_us.abs_diff(joint_dynamic.group_timestamp_us);

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

    /// 等待至少一帧带可信设备时间戳的反馈。
    ///
    /// StrictRealtime 后端必须在启动期证明设备时间基线可用，否则拒绝暴露 strict 控制语义。
    pub fn wait_for_timestamped_feedback(&self, timeout: Duration) -> Result<(), DriverError> {
        if !self.backend_capability.is_strict_realtime() {
            return Err(DriverError::InvalidInput(
                "timestamped feedback validation is only available on StrictRealtime backends"
                    .to_string(),
            ));
        }

        self.wait_for_timestamped_feedback_until(StartupValidationDeadline::after(timeout))
    }

    fn wait_for_timestamped_feedback_until(
        &self,
        deadline: StartupValidationDeadline,
    ) -> Result<(), DriverError> {
        loop {
            let first_feedback_host_rx_mono_us =
                self.ctx.first_timestamped_feedback_host_rx_mono_us();
            if first_feedback_host_rx_mono_us != 0 {
                if first_feedback_host_rx_mono_us <= deadline.host_rx_deadline_mono_us() {
                    return Ok(());
                }
                return Err(strict_realtime_timestamp_error(
                    "no timestamped feedback arrived before validation deadline",
                ));
            }

            if deadline.is_expired_now() {
                return Err(strict_realtime_timestamp_error(
                    "no timestamped feedback arrived before validation deadline",
                ));
            }

            if !self.rx_thread_alive() || !self.tx_thread_alive() {
                return Err(DriverError::ChannelClosed);
            }

            std::thread::sleep(Duration::from_millis(1));
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

    #[doc(hidden)]
    pub fn send_replay_frame(&self, frame: PiperFrame) -> Result<(), DriverError> {
        self.enqueue_reliable(ReliableCommand::replay(frame))
    }

    #[doc(hidden)]
    pub fn send_replay_frame_confirmed(
        &self,
        frame: PiperFrame,
        timeout: Duration,
    ) -> Result<(), DriverError> {
        if !self.tx_thread_alive() {
            return Err(DriverError::ChannelClosed);
        }

        let deadline = Instant::now() + timeout;
        let (ack_tx, ack_rx) = crossbeam_channel::bounded(2);
        self.enqueue_reliable_timeout_until(
            ReliableCommand::replay_confirmed(frame, deadline, ack_tx),
            deadline,
        )?;

        wait_for_delivery_result(ack_rx, deadline, || DriverError::Timeout)
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
    /// - [架构分析报告](../../../docs/v0/architecture/piper-driver-client-mixing-analysis.md) - 方案 B 设计
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
    ///
    /// 此值会观察到已经成功发布的模式切换结果。
    pub fn mode(&self) -> crate::mode::DriverMode {
        self.driver_mode.get(std::sync::atomic::Ordering::Acquire)
    }

    /// 尝试在有界时间内设置 Driver 模式。
    ///
    /// 此操作在 driver 内部串行化。成功返回后，`driver_mode` 与
    /// `normal_send_gate` 一定处于一致的模式语义；其他线程的前门模式判决
    /// 也必须与返回值一致。
    pub fn try_set_mode(
        &self,
        mode: crate::mode::DriverMode,
        timeout: Duration,
    ) -> Result<(), DriverError> {
        let _mode_switch_guard =
            self.mode_switch_lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());

        match mode {
            crate::mode::DriverMode::Replay => {
                if self.driver_mode.get(std::sync::atomic::Ordering::Acquire).is_replay() {
                    return Ok(());
                }

                match self.runtime_phase() {
                    RuntimePhase::Running => {},
                    RuntimePhase::FaultLatched => return Err(DriverError::ControlPathClosed),
                    RuntimePhase::Stopping => return Err(DriverError::ChannelClosed),
                }

                if self.normal_send_gate.state() == NormalSendGateState::StateTransitionClosed {
                    return Err(DriverError::ControlPathClosed);
                }

                let deadline = Instant::now() + timeout;
                let replay_switch = ReplaySwitchTxn::begin(self)?;

                while self.normal_send_gate.inflight_normal_sends() > 0 {
                    if self.normal_send_gate.state() == NormalSendGateState::StateTransitionClosed {
                        return Err(DriverError::ControlPathClosed);
                    }
                    if self.runtime_phase() != RuntimePhase::Running
                        || self.runtime_fault_kind().is_some()
                    {
                        return Err(DriverError::ControlPathClosed);
                    }
                    if !self.tx_thread_alive() {
                        return Err(DriverError::ChannelClosed);
                    }
                    if Instant::now() >= deadline {
                        self.latch_fault_with_kind(RuntimeFaultKind::TransportError);
                        return Err(DriverError::Timeout);
                    }
                    spin_sleep::sleep(Duration::from_micros(50));
                }

                if !self.tx_thread_alive() {
                    return Err(DriverError::ChannelClosed);
                }

                self.maybe_wait_test_mode_switch_replay_barrier();
                if Instant::now() >= deadline {
                    self.latch_fault_with_kind(RuntimeFaultKind::TransportError);
                    return Err(DriverError::Timeout);
                }
                if !self.tx_thread_alive() {
                    return Err(DriverError::ChannelClosed);
                }
                match (self.runtime_phase(), self.runtime_fault_kind()) {
                    (RuntimePhase::Running, None) => {},
                    (RuntimePhase::Stopping, _) => return Err(DriverError::ChannelClosed),
                    (RuntimePhase::Running, Some(_)) | (RuntimePhase::FaultLatched, _) => {
                        return Err(DriverError::ControlPathClosed);
                    },
                }

                replay_switch.commit_paused()?;
                if Instant::now() >= deadline {
                    self.latch_fault_with_kind(RuntimeFaultKind::TransportError);
                    return Err(DriverError::Timeout);
                }
                if !self.tx_thread_alive() {
                    return Err(DriverError::ChannelClosed);
                }
                self.maintenance_gate
                    .revoke_current_holder(MaintenanceRevocationReason::ControllerStateChanged)?;

                match (self.runtime_phase(), self.runtime_fault_kind()) {
                    (RuntimePhase::Running, None) => {
                        replay_switch.publish_replay();
                        Ok(())
                    },
                    (RuntimePhase::Stopping, _) => Err(DriverError::ChannelClosed),
                    (RuntimePhase::Running, Some(_)) | (RuntimePhase::FaultLatched, _) => {
                        Err(DriverError::ControlPathClosed)
                    },
                }
            },
            crate::mode::DriverMode::Normal => {
                self.driver_mode.set(mode, std::sync::atomic::Ordering::Release);
                self.normal_send_gate.resume_from_replay();
                Ok(())
            },
        }
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
    ///
    /// 这是一个串行化且有界等待的兼容包装：
    /// - `Replay` 切换成功返回后，普通控制路径一定已被隔离
    /// - `Normal` 切换不会与正在进行的 `Replay` 切换交错出混合状态
    /// - 成功返回后，其他线程观察到的前门模式判决必须与返回值一致
    pub fn set_mode(&self, mode: crate::mode::DriverMode) {
        match self.try_set_mode(mode, DEFAULT_MODE_SWITCH_TIMEOUT) {
            Ok(()) => tracing::info!("Driver mode set to: {:?}", mode),
            Err(error) => tracing::error!("Failed to set driver mode to {:?}: {}", mode, error),
        }
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
        if !self.backend_capability.is_strict_realtime() {
            return Err(DriverError::InvalidInput(
                "strict realtime delivery is only available on StrictRealtime backends".to_string(),
            ));
        }
        if self.replay_mode_active() || self.replay_barrier_active() {
            return Err(DriverError::ReplayModeActive);
        }
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
    /// # 发送语义
    /// 一旦 TX 线程开始发送首帧，后续会返回真实发送结果。
    /// 如中途失败，已发送的帧不会回滚，但剩余帧不会继续发送。
    ///
    /// # 性能特性
    /// - 如果帧数量 ≤ 4，完全在栈上分配，零堆内存分配
    /// - 如果帧数量 > 4，SmallVec 会自动溢出到堆，但仍保持高效
    pub fn send_realtime_package(
        &self,
        frames: impl IntoIterator<Item = PiperFrame>,
    ) -> Result<(), DriverError> {
        use crate::command::FrameBuffer;

        if !self.backend_capability.is_strict_realtime() {
            return Err(DriverError::InvalidInput(
                "strict realtime package delivery is only available on StrictRealtime backends"
                    .to_string(),
            ));
        }
        if self.replay_mode_active() || self.replay_barrier_active() {
            return Err(DriverError::ReplayModeActive);
        }

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
    ///
    /// `timeout` 只约束此包能否在 deadline 前进入 TX commit point。
    /// 一旦 TX 线程在 deadline 前进入 `tx.send_control(...)`，调用方将继续等待真实发送结果，
    /// 即使该结果返回时间晚于最初的 `timeout`。
    pub fn send_realtime_package_confirmed(
        &self,
        frames: impl IntoIterator<Item = PiperFrame>,
        timeout: Duration,
    ) -> Result<(), DriverError> {
        use crate::command::FrameBuffer;

        if !self.backend_capability.is_strict_realtime() {
            return Err(DriverError::InvalidInput(
                "strict realtime package delivery is only available on StrictRealtime backends"
                    .to_string(),
            ));
        }
        if !self.tx_thread_alive() {
            return Err(DriverError::ChannelClosed);
        }
        if self.replay_mode_active() || self.replay_barrier_active() {
            return Err(DriverError::ReplayModeActive);
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

        let deadline = Instant::now() + timeout;
        let (ack_tx, ack_rx) = crossbeam_channel::bounded(2);
        self.send_realtime_command(RealtimeCommand::confirmed(buffer, deadline, ack_tx))?;

        wait_for_delivery_result(ack_rx, deadline, || DriverError::RealtimeDeliveryTimeout)
    }

    /// 发送 SoftRealtime 原始批命令并等待实际发送结果。
    pub fn send_soft_realtime_package_confirmed(
        &self,
        frames: impl IntoIterator<Item = PiperFrame>,
        timeout: Duration,
    ) -> Result<(), DriverError> {
        use crate::command::FrameBuffer;

        if !self.backend_capability.is_soft_realtime() {
            return Err(DriverError::InvalidInput(
                "soft realtime batch delivery is only available on SoftRealtime backends"
                    .to_string(),
            ));
        }
        if !self.tx_thread_alive() {
            return Err(DriverError::ChannelClosed);
        }
        if self.replay_mode_active() || self.replay_barrier_active() {
            return Err(DriverError::ReplayModeActive);
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

        if timeout.is_zero() {
            return Err(DriverError::Timeout);
        }

        let deadline = Instant::now() + timeout;
        if deadline <= Instant::now() {
            return Err(DriverError::Timeout);
        }

        self.maybe_wait_test_soft_realtime_admission_barrier();
        if Instant::now() >= deadline {
            self.metrics.tx_soft_admission_timeout_total.fetch_add(1, Ordering::Relaxed);
            return Err(DriverError::Timeout);
        }

        let reservation = match self.soft_realtime_tx.try_reserve() {
            Ok(reservation) => reservation,
            Err(SoftRealtimeTryReserveError::Full) => return Err(DriverError::ChannelFull),
            Err(SoftRealtimeTryReserveError::Disconnected) => {
                return Err(DriverError::ChannelClosed);
            },
        };

        self.maybe_wait_test_soft_realtime_post_check_barrier();
        if Instant::now() >= deadline {
            self.metrics.tx_soft_admission_timeout_total.fetch_add(1, Ordering::Relaxed);
            return Err(DriverError::Timeout);
        }

        let (ack_tx, ack_rx) = crossbeam_channel::bounded(1);
        let command = SoftRealtimeCommand::confirmed(buffer, deadline, ack_tx);

        match reservation.publish(command) {
            Ok(_) => {},
            Err(SoftRealtimeTrySendError::Full(command)) => {
                let command = *command;
                command.complete(Err(DriverError::ChannelFull));
                return Err(DriverError::ChannelFull);
            },
            Err(SoftRealtimeTrySendError::Disconnected(command)) => {
                let command = *command;
                command.complete(Err(DriverError::ChannelClosed));
                return Err(DriverError::ChannelClosed);
            },
        }

        recv_until_deadline(&ack_rx, deadline, || DriverError::Timeout)?
    }

    /// 内部方法：发送实时命令（统一处理单个帧和帧包）
    fn send_realtime_command(&self, command: RealtimeCommand) -> Result<(), DriverError> {
        if !self.tx_thread_alive() {
            command.complete(Err(DriverError::ChannelClosed));
            return Err(DriverError::ChannelClosed);
        }
        if self.replay_mode_active() || self.replay_barrier_active() {
            command.complete(Err(DriverError::ReplayModeActive));
            return Err(DriverError::ReplayModeActive);
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

    /// 发送可靠帧包（FIFO，整包作为单个队列元素）。
    pub fn send_reliable_package(
        &self,
        frames: impl IntoIterator<Item = PiperFrame>,
    ) -> Result<(), DriverError> {
        use crate::command::FrameBuffer;

        let buffer: FrameBuffer = frames.into_iter().collect();
        if buffer.is_empty() {
            return Err(DriverError::InvalidInput(
                "Reliable frame package cannot be empty".to_string(),
            ));
        }
        if buffer.len() > Self::MAX_RELIABLE_PACKAGE_SIZE {
            return Err(DriverError::InvalidInput(format!(
                "Reliable frame package too large: {} (max: {})",
                buffer.len(),
                Self::MAX_RELIABLE_PACKAGE_SIZE
            )));
        }

        self.enqueue_reliable(ReliableCommand::package(buffer))
    }

    /// 发送可靠帧包并等待 TX 线程确认整包实际发送结果。
    ///
    /// `timeout` 只约束此包能否在 deadline 前进入 TX commit point。
    /// 一旦 TX 线程在 deadline 前进入 `tx.send_control(...)`，调用方将继续等待真实发送结果，
    /// 即使该结果返回时间晚于最初的 `timeout`。
    pub fn send_reliable_package_confirmed(
        &self,
        frames: impl IntoIterator<Item = PiperFrame>,
        timeout: Duration,
    ) -> Result<(), DriverError> {
        use crate::command::FrameBuffer;

        let buffer: FrameBuffer = frames.into_iter().collect();
        if buffer.is_empty() {
            return Err(DriverError::InvalidInput(
                "Reliable frame package cannot be empty".to_string(),
            ));
        }
        if buffer.len() > Self::MAX_RELIABLE_PACKAGE_SIZE {
            return Err(DriverError::InvalidInput(format!(
                "Reliable frame package too large: {} (max: {})",
                buffer.len(),
                Self::MAX_RELIABLE_PACKAGE_SIZE
            )));
        }

        let deadline = Instant::now() + timeout;
        let (ack_tx, ack_rx) = crossbeam_channel::bounded(2);
        self.enqueue_reliable_timeout_until(
            ReliableCommand::package_confirmed(buffer, deadline, ack_tx),
            deadline,
        )?;

        wait_for_delivery_result(ack_rx, deadline, || DriverError::Timeout)
    }

    #[doc(hidden)]
    pub fn send_reliable_package_confirmed_commit_marker(
        &self,
        frames: impl IntoIterator<Item = PiperFrame>,
        timeout: Duration,
    ) -> Result<u64, DriverError> {
        use crate::command::FrameBuffer;

        let buffer: FrameBuffer = frames.into_iter().collect();
        if buffer.is_empty() {
            return Err(DriverError::InvalidInput(
                "Reliable frame package cannot be empty".to_string(),
            ));
        }
        if buffer.len() > Self::MAX_RELIABLE_PACKAGE_SIZE {
            return Err(DriverError::InvalidInput(format!(
                "Reliable frame package too large: {} (max: {})",
                buffer.len(),
                Self::MAX_RELIABLE_PACKAGE_SIZE
            )));
        }

        let deadline = Instant::now() + timeout;
        let (ack_tx, ack_rx) = crossbeam_channel::bounded(2);
        self.enqueue_reliable_timeout_until(
            ReliableCommand::package_confirmed_with_post_send_commit(buffer, deadline, ack_tx),
            deadline,
        )?;

        wait_for_delivery_result_with_commit(ack_rx, deadline, || DriverError::Timeout)?
            .ok_or(DriverError::ChannelClosed)
    }

    #[doc(hidden)]
    pub fn send_reliable_frame_confirmed_commit_marker(
        &self,
        frame: PiperFrame,
        timeout: Duration,
    ) -> Result<u64, DriverError> {
        let deadline = Instant::now() + timeout;
        let (ack_tx, ack_rx) = crossbeam_channel::bounded(2);
        self.enqueue_reliable_timeout_until(
            ReliableCommand::package_confirmed([frame], deadline, ack_tx),
            deadline,
        )?;

        wait_for_delivery_result_with_commit(ack_rx, deadline, || DriverError::Timeout)?
            .ok_or(DriverError::ChannelClosed)
    }

    /// 发送维护帧并等待 TX 线程在实际发送点完成最终运行时准入判定。
    ///
    /// `timeout` 只约束此帧能否在 deadline 前进入 TX commit point。
    /// 一旦 TX 线程在 deadline 前进入 `tx.send_control(...)`，调用方将继续等待真实发送结果，
    /// 即使该结果返回时间晚于最初的 `timeout`。
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
        if self.replay_mode_active() || self.replay_barrier_active() {
            return Err(DriverError::ReplayModeActive);
        }
        if !self.normal_control_open() {
            return Err(DriverError::ControlPathClosed);
        }

        let deadline = Instant::now() + timeout;
        let (ack_tx, ack_rx) = crossbeam_channel::bounded(2);
        self.maintenance_lane_tx
            .send(MaintenanceLaneCommand::send(
                frame,
                MaintenanceCommandMeta::new(session_id, session_key, lease_epoch),
                deadline,
                ack_tx,
            ))
            .map_err(|_| DriverError::ChannelClosed)?;
        self.metrics.tx_reliable_enqueued_total.fetch_add(1, Ordering::Relaxed);

        wait_for_maintenance_send_result(ack_rx, deadline)
    }

    #[doc(hidden)]
    pub fn abort_pending_normal_control_for_state_transition(
        &self,
        timeout: Duration,
    ) -> Result<(), DriverError> {
        if !self.tx_thread_alive() {
            return Err(DriverError::ChannelClosed);
        }

        let deadline = Instant::now() + timeout;
        let (command, ack_rx) = MaintenanceLaneCommand::blocking_abort_pending_normal_control();
        self.maintenance_lane_tx.send(command).map_err(|_| DriverError::ChannelClosed)?;
        MaintenanceGate::wait_control_ack_until(ack_rx, deadline)
    }

    #[doc(hidden)]
    pub fn send_local_maintenance_frame_confirmed(
        &self,
        frame: PiperFrame,
        timeout: Duration,
    ) -> Result<(), DriverError> {
        if !self.tx_thread_alive() {
            return Err(DriverError::ChannelClosed);
        }
        if self.replay_mode_active() || self.replay_barrier_active() {
            return Err(DriverError::ReplayModeActive);
        }
        if !self.normal_control_open() {
            return Err(DriverError::ControlPathClosed);
        }

        let deadline = Instant::now() + timeout;
        let (ack_tx, ack_rx) = crossbeam_channel::bounded(2);
        self.maintenance_lane_tx
            .send(MaintenanceLaneCommand::local_send(frame, deadline, ack_tx))
            .map_err(|_| DriverError::ChannelClosed)?;
        self.metrics.tx_reliable_enqueued_total.fetch_add(1, Ordering::Relaxed);

        wait_for_maintenance_send_result(ack_rx, deadline)
    }

    #[doc(hidden)]
    pub fn send_local_state_transition_frame_confirmed(
        &self,
        frame: PiperFrame,
        timeout: Duration,
    ) -> Result<(), DriverError> {
        self.send_local_state_transition_frame_confirmed_commit_marker(frame, timeout)
            .map(|_| ())
    }

    #[doc(hidden)]
    pub fn send_local_state_transition_frame_confirmed_commit_marker(
        &self,
        frame: PiperFrame,
        timeout: Duration,
    ) -> Result<u64, DriverError> {
        if !self.tx_thread_alive() {
            return Err(DriverError::ChannelClosed);
        }
        if self.replay_mode_active() {
            return Err(DriverError::ReplayModeActive);
        }
        if self.runtime_phase() != RuntimePhase::Running
            || self.runtime_fault_kind().is_some()
            || !self.rx_thread_alive()
        {
            return Err(DriverError::ControlPathClosed);
        }

        let deadline = Instant::now() + timeout;
        let (ack_tx, ack_rx) = crossbeam_channel::bounded(2);
        match self.normal_send_gate.close_for_state_transition() {
            Ok(()) => {},
            Err(NormalSendGateDenyReason::ReplayPaused) => {
                return Err(DriverError::ReplayModeActive);
            },
            Err(_) => return Err(DriverError::ControlPathClosed),
        }

        {
            let _mode_switch_guard =
                self.mode_switch_lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());

            if !self.tx_thread_alive() {
                self.normal_send_gate.restore_after_state_transition(
                    self.runtime_phase(),
                    self.runtime_fault_kind(),
                    self.mode(),
                );
                return Err(DriverError::ChannelClosed);
            }
            if self.replay_mode_active() {
                self.normal_send_gate.restore_after_state_transition(
                    self.runtime_phase(),
                    self.runtime_fault_kind(),
                    self.mode(),
                );
                return Err(DriverError::ReplayModeActive);
            }
            if self.normal_send_gate.state() == NormalSendGateState::ReplayPaused {
                self.normal_send_gate.restore_after_state_transition(
                    self.runtime_phase(),
                    self.runtime_fault_kind(),
                    self.mode(),
                );
                return Err(DriverError::ReplayModeActive);
            }
            if self.runtime_phase() != RuntimePhase::Running
                || self.runtime_fault_kind().is_some()
                || !self.rx_thread_alive()
                || self.normal_send_gate.state() != NormalSendGateState::StateTransitionClosed
            {
                self.normal_send_gate.restore_after_state_transition(
                    self.runtime_phase(),
                    self.runtime_fault_kind(),
                    self.mode(),
                );
                return Err(DriverError::ControlPathClosed);
            }

            if self
                .maintenance_lane_tx
                .send(MaintenanceLaneCommand::state_transition_local_send(
                    frame,
                    deadline,
                    ack_tx,
                    StateTransitionCompletion::HoldUntilDisableConfirmed,
                ))
                .is_err()
            {
                self.normal_send_gate.restore_after_state_transition(
                    self.runtime_phase(),
                    self.runtime_fault_kind(),
                    self.mode(),
                );
                return Err(DriverError::ChannelClosed);
            }
        }

        self.metrics.tx_reliable_enqueued_total.fetch_add(1, Ordering::Relaxed);
        let result =
            wait_for_delivery_result_with_commit(ack_rx, deadline, || DriverError::Timeout)
                .and_then(|commit| commit.ok_or(DriverError::ChannelClosed));
        if matches!(result, Err(DriverError::Timeout)) {
            self.latch_state_transition_timeout_fault_fast();
        }
        result
    }

    #[doc(hidden)]
    /// Returns a runtime-level maintenance denial override for external observers.
    ///
    /// RX death, TX death, fault latch, and Replay isolation all make the maintenance path
    /// unavailable even if the underlying lease gate has not been refreshed yet.
    fn maintenance_runtime_override_state(&self) -> Option<MaintenanceGateState> {
        (!self.maintenance_runtime_open()).then_some(MaintenanceGateState::DeniedFaulted)
    }

    #[doc(hidden)]
    /// Returns the maintenance lease snapshot with runtime-level availability applied.
    ///
    /// When the runtime is not maintenance-open (fault latched, RX dead, TX dead,
    /// Replay active, or Replay barrier engaged), the exposed state is forced to
    /// `DeniedFaulted` while preserving the underlying holder identifiers and lease
    /// epoch for diagnostics.
    pub fn maintenance_lease_snapshot(&self) -> MaintenanceLeaseSnapshot {
        let snapshot = self.maintenance_gate.snapshot();
        let Some(state) = self.maintenance_runtime_override_state() else {
            return snapshot;
        };

        MaintenanceLeaseSnapshot {
            state,
            holder_session_id: snapshot.holder_session_id,
            holder_session_key: snapshot.holder_session_key,
            lease_epoch: snapshot.lease_epoch,
        }
    }

    #[doc(hidden)]
    pub fn register_maintenance_event_sink(&self, sink: Sender<MaintenanceRevocationEvent>) {
        self.maintenance_gate.set_event_sink(sink);
    }

    #[doc(hidden)]
    /// Acquires the maintenance lease only when the runtime is maintenance-open.
    ///
    /// RX death, TX death, fault latch, and Replay isolation all deny acquisition as
    /// `DeniedFaulted` even if the underlying lease gate still reflects a stale
    /// standby state.
    pub fn acquire_maintenance_lease_gate(
        &self,
        session_id: u32,
        session_key: u64,
        timeout: Duration,
    ) -> Result<MaintenanceLeaseAcquireResult, DriverError> {
        if let Some(state) = self.maintenance_runtime_override_state() {
            return Ok(MaintenanceLeaseAcquireResult::DeniedState { state });
        }

        let result = self.maintenance_gate.acquire_blocking(session_id, session_key, timeout)?;

        if matches!(result, MaintenanceLeaseAcquireResult::Granted { .. })
            && let Some(state) = self.maintenance_runtime_override_state()
        {
            let _ = self.release_maintenance_lease_gate_if_holder(session_key);
            return Ok(MaintenanceLeaseAcquireResult::DeniedState { state });
        }

        Ok(result)
    }

    #[doc(hidden)]
    pub fn release_maintenance_lease_gate_if_holder(
        &self,
        session_key: u64,
    ) -> Result<bool, DriverError> {
        self.maintenance_gate.release_if_holder(session_key)
    }

    #[doc(hidden)]
    pub fn revoke_maintenance_lease_gate(
        &self,
        session_key: u64,
        reason: MaintenanceRevocationReason,
    ) -> Result<Option<MaintenanceRevocationEvent>, DriverError> {
        self.maintenance_gate.revoke_if_holder(session_key, reason)
    }

    #[doc(hidden)]
    pub fn set_maintenance_gate_state(&self, state: MaintenanceGateState) {
        let _ = self.maintenance_gate.set_state_synced(state);
    }

    #[doc(hidden)]
    /// Returns whether the current maintenance lease is still valid under runtime-open semantics.
    ///
    /// This check applies the same runtime fail-closed override as
    /// `maintenance_lease_snapshot()`: Replay, fault latch, RX death, and TX death all
    /// invalidate an otherwise matching cached lease immediately.
    pub fn maintenance_lease_is_valid(&self, session_key: u64, lease_epoch: u64) -> bool {
        let snapshot = self.maintenance_lease_snapshot();
        snapshot.state() == MaintenanceGateState::AllowedStandby
            && snapshot.holder_session_key() == Some(session_key)
            && snapshot.lease_epoch() == lease_epoch
    }

    #[doc(hidden)]
    pub fn connection_timeout_remaining(&self) -> Option<Duration> {
        self.ctx.connection_monitor.remaining_until_timeout()
    }

    #[doc(hidden)]
    pub fn maintenance_runtime_open(&self) -> bool {
        self.runtime_phase() == RuntimePhase::Running
            && self.runtime_fault.load(Ordering::Acquire) == 0
            && self.rx_thread_alive()
            && self.tx_thread_alive()
            && !self.replay_mode_active()
            && !self.replay_barrier_active()
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
        let kind = command.kind();
        if !self.tx_thread_alive() {
            command.complete(Err(DriverError::ChannelClosed));
            return Err(DriverError::ChannelClosed);
        }
        if let Err(error) = self.ensure_mode_allows_reliable_kind(kind) {
            command.complete(Err(clone_driver_error(&error)));
            return Err(error);
        }
        if !self.reliable_front_door_open(kind) {
            command.complete(Err(DriverError::ControlPathClosed));
            return Err(DriverError::ControlPathClosed);
        }
        match self.reliable_tx.try_send(command) {
            Ok(_) => {
                self.metrics.tx_reliable_enqueued_total.fetch_add(1, Ordering::Relaxed);
                Ok(())
            },
            Err(crossbeam_channel::TrySendError::Full(command)) => {
                self.metrics.tx_reliable_queue_full_total.fetch_add(1, Ordering::Relaxed);
                command.complete(Err(DriverError::ChannelFull));
                Err(DriverError::ChannelFull)
            },
            Err(crossbeam_channel::TrySendError::Disconnected(command)) => {
                command.complete(Err(DriverError::ChannelClosed));
                Err(DriverError::ChannelClosed)
            },
        }
    }

    fn enqueue_reliable_timeout(
        &self,
        command: ReliableCommand,
        timeout: Duration,
    ) -> Result<(), DriverError> {
        self.enqueue_reliable_timeout_until(command, Instant::now() + timeout)
    }

    fn enqueue_reliable_timeout_until(
        &self,
        command: ReliableCommand,
        deadline: Instant,
    ) -> Result<(), DriverError> {
        let kind = command.kind();
        if !self.tx_thread_alive() {
            command.complete(Err(DriverError::ChannelClosed));
            return Err(DriverError::ChannelClosed);
        }
        if let Err(error) = self.ensure_mode_allows_reliable_kind(kind) {
            command.complete(Err(clone_driver_error(&error)));
            return Err(error);
        }
        if !self.reliable_front_door_open(kind) {
            command.complete(Err(DriverError::ControlPathClosed));
            return Err(DriverError::ControlPathClosed);
        }
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            command.complete(Err(DriverError::Timeout));
            return Err(DriverError::Timeout);
        };
        match self.reliable_tx.send_timeout(command, remaining) {
            Ok(_) => {
                self.metrics.tx_reliable_enqueued_total.fetch_add(1, Ordering::Relaxed);
                Ok(())
            },
            Err(crossbeam_channel::SendTimeoutError::Timeout(command)) => {
                self.metrics.tx_reliable_queue_full_total.fetch_add(1, Ordering::Relaxed);
                command.complete(Err(DriverError::Timeout));
                Err(DriverError::Timeout)
            },
            Err(crossbeam_channel::SendTimeoutError::Disconnected(command)) => {
                command.complete(Err(DriverError::ChannelClosed));
                Err(DriverError::ChannelClosed)
            },
        }
    }
}

impl Drop for Piper {
    fn drop(&mut self) {
        self.runtime_phase.store(RuntimePhase::Stopping as u8, Ordering::Release);
        self.normal_send_gate.close_for_stop();
        self.workers_running.store(false, Ordering::Release);
        self.shutdown_lane.close_with(Err(DriverError::ChannelClosed));
        self.soft_realtime_tx.close();

        unsafe {
            ManuallyDrop::drop(&mut self.reliable_tx);
            ManuallyDrop::drop(&mut self.maintenance_lane_tx);
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
    use crate::DriverMode;
    use crate::observation::{Available, Complete, Freshness, Observation, ObservationPayload};
    use crate::{DiagnosticEvent, ProtocolDiagnostic, QueryError, WaitError};
    use piper_can::{CanAdapter, PiperFrame, SplittableAdapter};
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex, mpsc};
    use std::thread;

    const TEST_MAINTENANCE_FRESHNESS_MS: u64 = 5_000;

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

    fn install_tx_loop_barrier(piper: &Piper) -> (mpsc::Receiver<()>, mpsc::Sender<()>) {
        let (reached_tx, reached_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        piper.ctx.install_tx_loop_dispatch_barrier(reached_tx, release_rx);
        (reached_rx, release_tx)
    }

    fn maintenance_ready_config() -> PipelineConfig {
        PipelineConfig {
            low_speed_drive_state_freshness_ms: TEST_MAINTENANCE_FRESHNESS_MS,
            ..PipelineConfig::default()
        }
    }

    fn mark_maintenance_standby_confirmed(piper: &Piper) {
        publish_confirmed_driver_mask(piper, 0);
        piper.set_maintenance_gate_state(MaintenanceGateState::AllowedStandby);
    }

    fn publish_confirmed_driver_mask(piper: &Piper, driver_enabled_mask: u8) {
        let now = crate::heartbeat::monotonic_micros().max(1);
        piper.ctx.connection_monitor.register_feedback();
        piper.ctx.joint_driver_low_speed.store(Arc::new(JointDriverLowSpeedState {
            host_rx_mono_us: now,
            host_rx_mono_timestamps: [now; 6],
            driver_enabled_mask,
            valid_mask: 0b11_1111,
            ..JointDriverLowSpeedState::default()
        }));
    }

    fn install_fault_latch_barrier(piper: &Piper) -> (mpsc::Receiver<()>, mpsc::Sender<()>) {
        let (reached_tx, reached_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        piper.normal_send_gate.install_fault_latch_barrier(reached_tx, release_rx);
        (reached_rx, release_tx)
    }

    fn install_state_transition_barrier(piper: &Piper) -> (mpsc::Receiver<()>, mpsc::Sender<()>) {
        let (reached_tx, reached_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        piper.normal_send_gate.install_state_transition_barrier(reached_tx, release_rx);
        (reached_rx, release_tx)
    }

    fn install_mode_switch_barrier(piper: &Piper) -> (mpsc::Receiver<()>, mpsc::Sender<()>) {
        let (reached_tx, reached_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        piper.install_mode_switch_replay_barrier(reached_tx, release_rx);
        (reached_rx, release_tx)
    }

    fn install_soft_realtime_admission_barrier(
        piper: &Piper,
    ) -> (mpsc::Receiver<()>, mpsc::Sender<()>) {
        let (reached_tx, reached_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        piper.install_soft_realtime_admission_barrier(reached_tx, release_rx);
        (reached_rx, release_tx)
    }

    fn install_soft_realtime_post_check_barrier(
        piper: &Piper,
    ) -> (mpsc::Receiver<()>, mpsc::Sender<()>) {
        let (reached_tx, reached_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        piper.install_soft_realtime_post_check_barrier(reached_tx, release_rx);
        (reached_rx, release_tx)
    }

    fn wait_until(timeout: Duration, mut predicate: impl FnMut() -> bool, message: &str) {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if predicate() {
                return;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        panic!("{message}");
    }

    struct BootstrappedMockRxAdapter {
        bootstrap: Option<PiperFrame>,
    }

    impl BootstrappedMockRxAdapter {
        fn new() -> Self {
            let mut frame = PiperFrame::new_standard(0x251, &[0; 8]);
            frame.timestamp_us = 1;
            Self {
                bootstrap: Some(frame),
            }
        }
    }

    impl piper_can::RxAdapter for BootstrappedMockRxAdapter {
        fn receive(&mut self) -> Result<PiperFrame, CanError> {
            if let Some(frame) = self.bootstrap.take() {
                return Ok(frame);
            }
            Err(CanError::Timeout)
        }
    }

    struct ProbedBootstrapRxAdapter {
        capability: BackendCapability,
        bootstrap: VecDeque<PiperFrame>,
    }

    impl ProbedBootstrapRxAdapter {
        fn new(capability: BackendCapability, frame: PiperFrame) -> Self {
            let mut bootstrap = VecDeque::new();
            bootstrap.push_back(frame);
            Self {
                capability,
                bootstrap,
            }
        }
    }

    impl piper_can::RxAdapter for ProbedBootstrapRxAdapter {
        fn receive(&mut self) -> Result<PiperFrame, CanError> {
            self.bootstrap.pop_front().ok_or(CanError::Timeout)
        }

        fn backend_capability(&self) -> BackendCapability {
            self.capability
        }

        fn startup_probe_until(
            &mut self,
            deadline: Instant,
        ) -> Result<Option<BackendCapability>, CanError> {
            if Instant::now() > deadline {
                return Err(CanError::Device(piper_can::CanDeviceError::new(
                    piper_can::CanDeviceErrorKind::UnsupportedConfig,
                    "startup validation deadline elapsed before startup probe completed",
                )));
            }
            Ok(Some(self.capability))
        }
    }

    struct SoftRxAdapter;

    impl piper_can::RxAdapter for SoftRxAdapter {
        fn receive(&mut self) -> Result<PiperFrame, CanError> {
            Err(CanError::Timeout)
        }

        fn backend_capability(&self) -> BackendCapability {
            BackendCapability::SoftRealtime
        }
    }

    struct TimestampedJunkRxAdapter {
        emitted: bool,
    }

    impl TimestampedJunkRxAdapter {
        fn new() -> Self {
            Self { emitted: false }
        }
    }

    impl piper_can::RxAdapter for TimestampedJunkRxAdapter {
        fn receive(&mut self) -> Result<PiperFrame, CanError> {
            if !self.emitted {
                self.emitted = true;
                let mut frame = PiperFrame::new_standard(0x7FF, &[0]);
                frame.timestamp_us = 123;
                return Ok(frame);
            }
            Err(CanError::Timeout)
        }
    }

    struct DelayedTimestampedFeedbackRxAdapter {
        delay: Duration,
        emitted: bool,
    }

    impl DelayedTimestampedFeedbackRxAdapter {
        fn new(delay: Duration) -> Self {
            Self {
                delay,
                emitted: false,
            }
        }
    }

    impl piper_can::RxAdapter for DelayedTimestampedFeedbackRxAdapter {
        fn receive(&mut self) -> Result<PiperFrame, CanError> {
            if !self.emitted {
                self.emitted = true;
                if !self.delay.is_zero() {
                    std::thread::sleep(self.delay);
                }
                let mut frame = PiperFrame::new_standard(0x251, &[0; 8]);
                frame.timestamp_us = 1;
                return Ok(frame);
            }
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

    struct TriggeredFatalRxAdapter {
        trigger: Arc<std::sync::atomic::AtomicBool>,
        tripped: bool,
    }

    impl piper_can::RxAdapter for TriggeredFatalRxAdapter {
        fn receive(&mut self) -> Result<PiperFrame, CanError> {
            if !self.tripped && self.trigger.load(std::sync::atomic::Ordering::Acquire) {
                self.tripped = true;
                return Err(CanError::BufferOverflow);
            }
            Err(CanError::Timeout)
        }
    }

    struct PanickingRxAdapter;

    impl piper_can::RxAdapter for PanickingRxAdapter {
        fn receive(&mut self) -> Result<PiperFrame, CanError> {
            panic!("rx panic injected by test");
        }
    }

    struct PanickingTxAdapter;

    impl piper_can::RealtimeTxAdapter for PanickingTxAdapter {
        fn send_control(&mut self, _frame: PiperFrame, _budget: Duration) -> Result<(), CanError> {
            panic!("tx panic injected by test");
        }

        fn send_shutdown_until(
            &mut self,
            _frame: PiperFrame,
            _deadline: Instant,
        ) -> Result<(), CanError> {
            panic!("tx panic injected by test");
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

    struct PartialTimeoutTxAdapter {
        sends: usize,
    }

    struct AlwaysTimeoutTxAdapter;

    struct DelayedSubMillisecondBudgetTxAdapter {
        sends: usize,
    }

    impl piper_can::RealtimeTxAdapter for AlwaysTimeoutTxAdapter {
        fn send_control(&mut self, _frame: PiperFrame, _budget: Duration) -> Result<(), CanError> {
            Err(CanError::Timeout)
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

    impl piper_can::RealtimeTxAdapter for DelayedSubMillisecondBudgetTxAdapter {
        fn send_control(&mut self, _frame: PiperFrame, budget: Duration) -> Result<(), CanError> {
            self.sends += 1;
            if self.sends == 1 {
                let remaining_tail = Duration::from_micros(700);
                let delay = budget.saturating_sub(remaining_tail);
                let delay_until = Instant::now() + delay;
                while Instant::now() < delay_until {
                    std::hint::spin_loop();
                }
                return Ok(());
            }

            if budget < Duration::from_millis(1) {
                return Err(CanError::Timeout);
            }

            Err(CanError::BufferOverflow)
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

    impl piper_can::RealtimeTxAdapter for PartialTimeoutTxAdapter {
        fn send_control(&mut self, _frame: PiperFrame, budget: Duration) -> Result<(), CanError> {
            if budget.is_zero() {
                return Err(CanError::Timeout);
            }

            self.sends += 1;
            if self.sends % 2 == 1 {
                Ok(())
            } else {
                Err(CanError::Timeout)
            }
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

    fn attach_test_maintenance_control_sink(gate: &MaintenanceGate) {
        let (tx, rx) = crossbeam_channel::unbounded::<MaintenanceLaneCommand>();
        gate.set_control_sink(tx);
        std::thread::spawn(move || {
            while let Ok(command) = rx.recv() {
                match command {
                    MaintenanceLaneCommand::Control { ack, .. } => {
                        if let Some(ack) = ack {
                            let _ = ack.send(());
                        }
                    },
                    MaintenanceLaneCommand::AbortPendingNormalControl { ack } => {
                        let _ = ack.send(());
                    },
                    MaintenanceLaneCommand::Send { ack, .. }
                    | MaintenanceLaneCommand::LocalSend { ack, .. }
                    | MaintenanceLaneCommand::StateTransitionLocalSend { ack, .. } => {
                        let _ = ack.send(MaintenanceSendPhase::Finished(Err(
                            DriverError::ChannelClosed,
                        )));
                    },
                }
            }
        });
    }

    fn attach_recording_maintenance_control_sink(
        gate: &MaintenanceGate,
        ops: Arc<Mutex<Vec<MaintenanceControlOp>>>,
    ) {
        let (tx, rx) = crossbeam_channel::unbounded::<MaintenanceLaneCommand>();
        gate.set_control_sink(tx);
        std::thread::spawn(move || {
            while let Ok(command) = rx.recv() {
                match command {
                    MaintenanceLaneCommand::Control { op, ack } => {
                        ops.lock().expect("maintenance ops lock").push(op);
                        if let Some(ack) = ack {
                            let _ = ack.send(());
                        }
                    },
                    MaintenanceLaneCommand::AbortPendingNormalControl { ack } => {
                        let _ = ack.send(());
                    },
                    MaintenanceLaneCommand::Send { ack, .. }
                    | MaintenanceLaneCommand::LocalSend { ack, .. }
                    | MaintenanceLaneCommand::StateTransitionLocalSend { ack, .. } => {
                        let _ = ack.send(MaintenanceSendPhase::Finished(Err(
                            DriverError::ChannelClosed,
                        )));
                    },
                }
            }
        });
    }

    fn attach_delayed_first_grant_control_sink(
        gate: &MaintenanceGate,
        ops: Arc<Mutex<Vec<MaintenanceControlOp>>>,
        first_grant_started: mpsc::Sender<()>,
        first_grant_release: mpsc::Receiver<()>,
    ) {
        let (tx, rx) = crossbeam_channel::unbounded::<MaintenanceLaneCommand>();
        gate.set_control_sink(tx);
        std::thread::spawn(move || {
            let mut delayed_first_grant = true;
            while let Ok(command) = rx.recv() {
                match command {
                    MaintenanceLaneCommand::Control { op, ack } => {
                        ops.lock().expect("maintenance ops lock").push(op);
                        if matches!(op, MaintenanceControlOp::Grant { .. }) && delayed_first_grant {
                            delayed_first_grant = false;
                            let _ = first_grant_started.send(());
                            let _ = first_grant_release.recv();
                        }
                        if let Some(ack) = ack {
                            let _ = ack.send(());
                        }
                    },
                    MaintenanceLaneCommand::AbortPendingNormalControl { ack } => {
                        let _ = ack.send(());
                    },
                    MaintenanceLaneCommand::Send { ack, .. }
                    | MaintenanceLaneCommand::LocalSend { ack, .. }
                    | MaintenanceLaneCommand::StateTransitionLocalSend { ack, .. } => {
                        let _ = ack.send(MaintenanceSendPhase::Finished(Err(
                            DriverError::ChannelClosed,
                        )));
                    },
                }
            }
        });
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

    struct BlockingTxAdapter {
        entered_tx: mpsc::Sender<()>,
        release_rx: mpsc::Receiver<()>,
    }

    impl piper_can::RealtimeTxAdapter for BlockingTxAdapter {
        fn send_control(&mut self, _frame: PiperFrame, budget: Duration) -> Result<(), CanError> {
            if budget.is_zero() {
                return Err(CanError::Timeout);
            }
            let _ = self.entered_tx.send(());
            self.release_rx
                .recv_timeout(Duration::from_millis(100))
                .map_err(|_| CanError::Timeout)?;
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

    struct TimeoutOnceShutdownTxAdapter {
        shutdown_sends: usize,
        sent_frames: Arc<Mutex<Vec<PiperFrame>>>,
    }

    impl piper_can::RealtimeTxAdapter for TimeoutOnceShutdownTxAdapter {
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
            self.shutdown_sends += 1;
            if self.shutdown_sends == 1 {
                return Err(CanError::Timeout);
            }
            self.sent_frames.lock().expect("sent frames lock").push(frame);
            Ok(())
        }
    }

    impl SplittableAdapter for MockCanAdapter {
        type RxAdapter = BootstrappedMockRxAdapter;
        type TxAdapter = MockTxAdapter;

        fn split(self) -> Result<(Self::RxAdapter, Self::TxAdapter), CanError> {
            Ok((BootstrappedMockRxAdapter::new(), MockTxAdapter))
        }
    }

    struct SlowSplitCanAdapter {
        delay: Duration,
    }

    impl CanAdapter for SlowSplitCanAdapter {
        fn send(&mut self, _frame: PiperFrame) -> Result<(), CanError> {
            Ok(())
        }

        fn receive(&mut self) -> Result<PiperFrame, CanError> {
            Err(CanError::Timeout)
        }
    }

    impl SplittableAdapter for SlowSplitCanAdapter {
        type RxAdapter = BootstrappedMockRxAdapter;
        type TxAdapter = MockTxAdapter;

        fn split(self) -> Result<(Self::RxAdapter, Self::TxAdapter), CanError> {
            if !self.delay.is_zero() {
                std::thread::sleep(self.delay);
            }
            Ok((BootstrappedMockRxAdapter::new(), MockTxAdapter))
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
        bootstrap: Option<PiperFrame>,
        frames: VecDeque<PiperFrame>,
        first_delay: Duration,
        emitted_first_frame: bool,
    }

    impl ScriptedRxAdapter {
        fn new(frames: Vec<PiperFrame>, first_delay: Duration) -> Self {
            Self {
                bootstrap: Some({
                    let mut frame = PiperFrame::new_standard(0x251, &[0; 8]);
                    frame.timestamp_us = 1;
                    frame
                }),
                frames: frames.into(),
                first_delay,
                emitted_first_frame: false,
            }
        }
    }

    impl piper_can::RxAdapter for ScriptedRxAdapter {
        fn receive(&mut self) -> Result<PiperFrame, CanError> {
            if let Some(frame) = self.bootstrap.take() {
                return Ok(frame);
            }
            if !self.emitted_first_frame && !self.first_delay.is_zero() {
                std::thread::sleep(self.first_delay);
                self.emitted_first_frame = true;
            }
            self.frames.pop_front().ok_or(CanError::Timeout)
        }
    }

    struct ChannelRxAdapter {
        bootstrap: Option<PiperFrame>,
        frames_rx: mpsc::Receiver<PiperFrame>,
    }

    impl ChannelRxAdapter {
        fn new(frames_rx: mpsc::Receiver<PiperFrame>) -> Self {
            Self {
                bootstrap: Some({
                    let mut frame = PiperFrame::new_standard(0x251, &[0; 8]);
                    frame.timestamp_us = 1;
                    frame
                }),
                frames_rx,
            }
        }
    }

    impl piper_can::RxAdapter for ChannelRxAdapter {
        fn receive(&mut self) -> Result<PiperFrame, CanError> {
            if let Some(frame) = self.bootstrap.take() {
                return Ok(frame);
            }

            self.frames_rx
                .recv_timeout(Duration::from_millis(2))
                .map_err(|_| CanError::Timeout)
        }
    }

    fn collision_protection_feedback_frame(levels: [u8; 6], timestamp_us: u64) -> PiperFrame {
        let mut data = [0u8; 8];
        data[..6].copy_from_slice(&levels);
        let mut frame = PiperFrame::new_standard(
            piper_protocol::ids::ID_COLLISION_PROTECTION_LEVEL_FEEDBACK as u16,
            &data,
        );
        frame.timestamp_us = timestamp_us;
        frame
    }

    fn end_limit_feedback_frame(
        linear_velocity_mm_s: u16,
        angular_velocity_millirad_s: u16,
        linear_accel_mm_s2: u16,
        angular_accel_millirad_s2: u16,
        timestamp_us: u64,
    ) -> PiperFrame {
        let mut data = [0u8; 8];
        data[0..2].copy_from_slice(&linear_velocity_mm_s.to_be_bytes());
        data[2..4].copy_from_slice(&angular_velocity_millirad_s.to_be_bytes());
        data[4..6].copy_from_slice(&linear_accel_mm_s2.to_be_bytes());
        data[6..8].copy_from_slice(&angular_accel_millirad_s2.to_be_bytes());
        let mut frame = PiperFrame::new_standard(
            piper_protocol::ids::ID_END_VELOCITY_ACCEL_FEEDBACK as u16,
            &data,
        );
        frame.timestamp_us = timestamp_us;
        frame
    }

    fn joint_limit_feedback_frame(
        joint_index: u8,
        max_angle_deci_deg: i16,
        min_angle_deci_deg: i16,
        max_velocity_millirad_s: u16,
        timestamp_us: u64,
    ) -> PiperFrame {
        let mut data = [0u8; 8];
        data[0] = joint_index;
        data[1..3].copy_from_slice(&max_angle_deci_deg.to_be_bytes());
        data[3..5].copy_from_slice(&min_angle_deci_deg.to_be_bytes());
        data[5..7].copy_from_slice(&max_velocity_millirad_s.to_be_bytes());
        let mut frame =
            PiperFrame::new_standard(piper_protocol::ids::ID_MOTOR_LIMIT_FEEDBACK as u16, &data);
        frame.timestamp_us = timestamp_us;
        frame
    }

    fn joint_accel_feedback_frame(
        joint_index: u8,
        max_accel_millirad_s2: u16,
        timestamp_us: u64,
    ) -> PiperFrame {
        let mut data = [0u8; 8];
        data[0] = joint_index;
        data[1..3].copy_from_slice(&max_accel_millirad_s2.to_be_bytes());
        let mut frame = PiperFrame::new_standard(
            piper_protocol::ids::ID_MOTOR_MAX_ACCEL_FEEDBACK as u16,
            &data,
        );
        frame.timestamp_us = timestamp_us;
        frame
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
    fn test_replay_mode_rejects_normal_control_paths_but_allows_replay_frames() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let piper = Piper::new_dual_thread_parts_unvalidated(
            MockRxAdapter,
            RecordingTxAdapter {
                sent_frames: sent_frames.clone(),
            },
            None,
        )
        .unwrap();
        let replay_frame = PiperFrame::new_standard(0x155, &[0xAA]);

        piper.set_mode(crate::mode::DriverMode::Replay);

        assert!(matches!(
            piper.send_frame(PiperFrame::new_standard(0x123, &[0x01])),
            Err(DriverError::ReplayModeActive)
        ));
        assert!(matches!(
            piper.send_realtime(PiperFrame::new_standard(0x155, &[0x01])),
            Err(DriverError::ReplayModeActive)
        ));
        assert!(matches!(
            piper.send_maintenance_frame_confirmed(
                7,
                77,
                1,
                replay_frame,
                Duration::from_millis(50)
            ),
            Err(DriverError::ReplayModeActive)
        ));

        piper
            .send_replay_frame_confirmed(replay_frame, Duration::from_millis(200))
            .expect("replay frame should still be sendable in Replay mode");

        let sent = sent_frames.lock().expect("sent frames lock");
        assert_eq!(sent.as_slice(), &[replay_frame]);
    }

    #[test]
    fn test_soft_realtime_batch_is_rejected_in_replay_mode() {
        let piper = Piper::new_dual_thread_parts(SoftRxAdapter, MockTxAdapter, None).unwrap();
        piper.set_mode(crate::mode::DriverMode::Replay);

        let error = piper
            .send_soft_realtime_package_confirmed(
                [PiperFrame::new_standard(0x155, &[0x01])],
                Duration::from_millis(50),
            )
            .expect_err("soft realtime batch should be blocked while Replay mode is active");

        assert!(matches!(error, DriverError::ReplayModeActive));
    }

    #[test]
    fn test_replay_reliable_uses_current_mode_after_try_set_mode_replay_returns() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let piper = Piper::new_dual_thread_parts_unvalidated(
            MockRxAdapter,
            RecordingTxAdapter {
                sent_frames: sent_frames.clone(),
            },
            None,
        )
        .unwrap();
        let replay_frame = PiperFrame::new_standard(0x155, &[0xA5]);
        let (reached_rx, release_tx) = install_tx_loop_barrier(&piper);

        reached_rx
            .recv_timeout(Duration::from_millis(200))
            .expect("TX loop should hit dispatch barrier");

        piper
            .try_set_mode(crate::mode::DriverMode::Replay, Duration::from_millis(100))
            .expect("Replay barrier should succeed");
        piper
            .send_replay_frame(replay_frame)
            .expect("replay frame should enqueue while Replay mode is active");

        let _ = release_tx.send(());
        wait_until(
            Duration::from_millis(200),
            || sent_frames.lock().expect("sent frames lock").len() == 1,
            "replay frame should be sent after Replay mode switch",
        );

        let sent = sent_frames.lock().expect("sent frames lock");
        assert_eq!(sent.as_slice(), &[replay_frame]);
    }

    #[test]
    fn test_mode_and_replay_front_door_observe_replay_after_cross_thread_switch() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let piper = Arc::new(
            Piper::new_dual_thread_parts_unvalidated(
                MockRxAdapter,
                RecordingTxAdapter {
                    sent_frames: sent_frames.clone(),
                },
                None,
            )
            .unwrap(),
        );
        let replay_frame = PiperFrame::new_standard(0x159, &[0xE1]);
        let (ready_tx, ready_rx) = mpsc::channel();

        let piper_switch = Arc::clone(&piper);
        let switch_handle = std::thread::spawn(move || {
            piper_switch
                .try_set_mode(crate::mode::DriverMode::Replay, Duration::from_millis(100))
                .expect("Replay switch should succeed");
            ready_tx.send(()).expect("mode switch signal should be delivered");
        });

        ready_rx
            .recv_timeout(Duration::from_millis(200))
            .expect("cross-thread replay switch should signal completion");
        assert_eq!(piper.mode(), crate::mode::DriverMode::Replay);
        piper
            .send_replay_frame_confirmed(replay_frame, Duration::from_millis(200))
            .expect("replay front door should observe Replay mode immediately");

        switch_handle.join().expect("Replay mode switch thread should finish cleanly");

        wait_until(
            Duration::from_millis(200),
            || sent_frames.lock().expect("sent frames lock").len() == 1,
            "replay frame should be sent after the cross-thread mode switch",
        );

        let sent = sent_frames.lock().expect("sent frames lock");
        assert_eq!(sent.as_slice(), &[replay_frame]);
    }

    #[test]
    fn test_realtime_uses_current_mode_after_try_set_mode_normal_returns() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let piper = Piper::new_dual_thread_parts_unvalidated(
            MockRxAdapter,
            RecordingTxAdapter {
                sent_frames: sent_frames.clone(),
            },
            None,
        )
        .unwrap();
        let realtime_frame = PiperFrame::new_standard(0x156, &[0xB6]);
        let (reached_rx, release_tx) = install_tx_loop_barrier(&piper);

        piper
            .try_set_mode(crate::mode::DriverMode::Replay, Duration::from_millis(100))
            .expect("entering Replay should succeed");
        reached_rx
            .recv_timeout(Duration::from_millis(200))
            .expect("TX loop should hit dispatch barrier");

        piper
            .try_set_mode(crate::mode::DriverMode::Normal, Duration::from_millis(100))
            .expect("returning to Normal should succeed");
        piper
            .send_realtime(realtime_frame)
            .expect("realtime frame should enqueue after returning to Normal");

        let _ = release_tx.send(());
        wait_until(
            Duration::from_millis(200),
            || sent_frames.lock().expect("sent frames lock").len() == 1,
            "realtime frame should be sent after returning to Normal mode",
        );

        let sent = sent_frames.lock().expect("sent frames lock");
        assert_eq!(sent.as_slice(), &[realtime_frame]);
    }

    #[test]
    fn test_standard_reliable_uses_current_mode_after_try_set_mode_normal_returns() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let piper = Piper::new_dual_thread_parts_unvalidated(
            MockRxAdapter,
            RecordingTxAdapter {
                sent_frames: sent_frames.clone(),
            },
            None,
        )
        .unwrap();
        let reliable_frame = PiperFrame::new_standard(0x157, &[0xC7]);
        let (reached_rx, release_tx) = install_tx_loop_barrier(&piper);

        piper
            .try_set_mode(crate::mode::DriverMode::Replay, Duration::from_millis(100))
            .expect("entering Replay should succeed");
        reached_rx
            .recv_timeout(Duration::from_millis(200))
            .expect("TX loop should hit dispatch barrier");

        piper
            .try_set_mode(crate::mode::DriverMode::Normal, Duration::from_millis(100))
            .expect("returning to Normal should succeed");
        piper
            .send_frame(reliable_frame)
            .expect("standard reliable frame should enqueue after returning to Normal");

        let _ = release_tx.send(());
        wait_until(
            Duration::from_millis(200),
            || sent_frames.lock().expect("sent frames lock").len() == 1,
            "standard reliable frame should be sent after returning to Normal mode",
        );

        let sent = sent_frames.lock().expect("sent frames lock");
        assert_eq!(sent.as_slice(), &[reliable_frame]);
    }

    #[test]
    fn test_mode_and_normal_front_door_observe_normal_after_cross_thread_restore() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let piper = Arc::new(
            Piper::new_dual_thread_parts_unvalidated(
                MockRxAdapter,
                RecordingTxAdapter {
                    sent_frames: sent_frames.clone(),
                },
                None,
            )
            .unwrap(),
        );
        let reliable_frame = PiperFrame::new_standard(0x15A, &[0xE2]);
        let realtime_frame = PiperFrame::new_standard(0x15B, &[0xE3]);
        let (ready_tx, ready_rx) = mpsc::channel();

        piper
            .try_set_mode(crate::mode::DriverMode::Replay, Duration::from_millis(100))
            .expect("entering Replay should succeed");

        let piper_restore = Arc::clone(&piper);
        let restore_handle = std::thread::spawn(move || {
            piper_restore
                .try_set_mode(crate::mode::DriverMode::Normal, Duration::from_millis(100))
                .expect("returning to Normal should succeed");
            ready_tx.send(()).expect("mode restore signal should be delivered");
        });

        ready_rx
            .recv_timeout(Duration::from_millis(200))
            .expect("cross-thread Normal restore should signal completion");
        assert_eq!(piper.mode(), crate::mode::DriverMode::Normal);
        piper
            .send_frame(reliable_frame)
            .expect("standard reliable front door should observe Normal mode immediately");
        piper
            .send_realtime(realtime_frame)
            .expect("realtime front door should observe Normal mode immediately");

        restore_handle.join().expect("Normal restore thread should finish cleanly");

        wait_until(
            Duration::from_millis(200),
            || sent_frames.lock().expect("sent frames lock").len() == 2,
            "normal sends should be accepted immediately after the cross-thread restore",
        );

        let sent = sent_frames.lock().expect("sent frames lock");
        assert_eq!(sent.len(), 2);
        assert!(sent.contains(&reliable_frame));
        assert!(sent.contains(&realtime_frame));
    }

    #[test]
    fn test_replay_mode_revokes_maintenance_lease_before_returning_to_normal() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let piper = Arc::new(
            Piper::new_dual_thread_parts_unvalidated(
                MockRxAdapter,
                RecordingTxAdapter {
                    sent_frames: sent_frames.clone(),
                },
                Some(maintenance_ready_config()),
            )
            .unwrap(),
        );
        let session_key = 99;
        let maintenance_frame = PiperFrame::new_standard(0x158, &[0xD8]);

        mark_maintenance_standby_confirmed(&piper);
        let lease_epoch = match piper
            .acquire_maintenance_lease_gate(9, session_key, Duration::from_millis(10))
            .expect("maintenance acquire should succeed")
        {
            MaintenanceLeaseAcquireResult::Granted { lease_epoch } => lease_epoch,
            other => panic!("unexpected maintenance acquire result: {other:?}"),
        };

        piper
            .try_set_mode(crate::mode::DriverMode::Replay, Duration::from_millis(100))
            .expect("entering Replay should succeed");
        let snapshot = piper.maintenance_lease_snapshot();
        assert_eq!(snapshot.holder_session_id(), None);
        assert_eq!(snapshot.holder_session_key(), None);
        let (reached_rx, release_tx) = install_tx_loop_barrier(&piper);
        reached_rx
            .recv_timeout(Duration::from_millis(200))
            .expect("TX loop should hit dispatch barrier");

        piper
            .try_set_mode(crate::mode::DriverMode::Normal, Duration::from_millis(100))
            .expect("returning to Normal should succeed");

        let piper_send = Arc::clone(&piper);
        let send_handle = std::thread::spawn(move || {
            piper_send.send_maintenance_frame_confirmed(
                9,
                session_key,
                lease_epoch,
                maintenance_frame,
                Duration::from_millis(200),
            )
        });

        let _ = release_tx.send(());
        let error = send_handle
            .join()
            .expect("maintenance send thread should finish")
            .expect_err("Replay must revoke the pre-existing maintenance lease");
        assert!(matches!(error, DriverError::MaintenanceWriteDenied(_)));

        let sent = sent_frames.lock().expect("sent frames lock");
        assert!(sent.is_empty());
        drop(sent);

        let new_lease_epoch = match piper
            .acquire_maintenance_lease_gate(9, session_key, Duration::from_millis(10))
            .expect("maintenance reacquire should succeed after Replay returns to Normal")
        {
            MaintenanceLeaseAcquireResult::Granted { lease_epoch } => lease_epoch,
            other => panic!("unexpected maintenance reacquire result: {other:?}"),
        };
        assert!(
            new_lease_epoch > lease_epoch,
            "Replay revocation should bump the maintenance lease epoch"
        );

        piper
            .send_maintenance_frame_confirmed(
                9,
                session_key,
                new_lease_epoch,
                maintenance_frame,
                Duration::from_millis(200),
            )
            .expect("maintenance writes should succeed after reacquiring a fresh lease");

        let sent = sent_frames.lock().expect("sent frames lock");
        assert_eq!(sent.as_slice(), &[maintenance_frame]);
    }

    #[test]
    fn test_reliable_command_enqueued_before_replay_mode_is_rejected_at_send_point() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let piper = Arc::new(
            Piper::new_dual_thread_parts_unvalidated(
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

        let first = PiperFrame::new_standard(0x101, &[0x01]);
        let second = [PiperFrame::new_standard(0x102, &[0x02])];
        piper.send_frame(first).expect("first reliable frame should queue");
        started_rx
            .recv_timeout(Duration::from_millis(200))
            .expect("first reliable frame should start sending");

        let piper_clone = Arc::clone(&piper);
        let handle = std::thread::spawn(move || {
            piper_clone.send_reliable_package_confirmed(second, Duration::from_millis(500))
        });

        std::thread::sleep(Duration::from_millis(20));
        let piper_mode = Arc::clone(&piper);
        let mode_handle =
            std::thread::spawn(move || piper_mode.set_mode(crate::mode::DriverMode::Replay));
        wait_until(
            Duration::from_millis(200),
            || piper.replay_barrier_active(),
            "Replay switch should pause the normal gate before the in-flight frame is released",
        );
        let _ = release_tx.send(());
        mode_handle
            .join()
            .expect("Replay barrier thread should finish once current frame drains");

        let error = handle.join().expect("reliable sender thread should finish").expect_err(
            "queued standard reliable command must be rejected once Replay mode is active",
        );
        assert!(matches!(error, DriverError::ReplayModeActive));

        let sent = sent_frames.lock().expect("sent frames lock");
        assert_eq!(sent.as_slice(), &[first]);
    }

    #[test]
    fn test_realtime_command_enqueued_before_replay_mode_is_rejected_at_send_point() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let piper = Arc::new(
            Piper::new_dual_thread_parts_unvalidated(
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

        let first = [PiperFrame::new_standard(0x155, &[0x01])];
        let second = [PiperFrame::new_standard(0x156, &[0x02])];
        piper.send_realtime_package(first).expect("first realtime frame should queue");
        started_rx
            .recv_timeout(Duration::from_millis(200))
            .expect("first realtime frame should start sending");

        let piper_clone = Arc::clone(&piper);
        let handle = std::thread::spawn(move || {
            piper_clone.send_realtime_package_confirmed(second, Duration::from_millis(500))
        });

        let enqueue_deadline = Instant::now() + Duration::from_millis(200);
        loop {
            if piper.realtime_slot.lock().expect("realtime slot lock").is_some() {
                break;
            }
            assert!(
                Instant::now() < enqueue_deadline,
                "confirmed realtime command should remain pending while the first send blocks"
            );
            std::thread::yield_now();
        }

        let piper_mode = Arc::clone(&piper);
        let mode_handle =
            std::thread::spawn(move || piper_mode.set_mode(crate::mode::DriverMode::Replay));
        wait_until(
            Duration::from_millis(200),
            || piper.replay_barrier_active(),
            "Replay switch should pause the normal gate before the in-flight frame is released",
        );
        let _ = release_tx.send(());
        mode_handle
            .join()
            .expect("Replay barrier thread should finish once current frame drains");

        let error = handle
            .join()
            .expect("realtime sender thread should finish")
            .expect_err("queued realtime command must be rejected once Replay mode is active");
        assert!(matches!(error, DriverError::ReplayModeActive));

        let sent = sent_frames.lock().expect("sent frames lock");
        assert_eq!(sent.as_slice(), &first);
    }

    #[test]
    fn test_reliable_package_is_truncated_after_current_frame_when_replay_barrier_engages() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let piper = Arc::new(
            Piper::new_dual_thread_parts_unvalidated(
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
            PiperFrame::new_standard(0x201, &[0x01]),
            PiperFrame::new_standard(0x202, &[0x02]),
        ];

        let piper_send = Arc::clone(&piper);
        let send_handle = std::thread::spawn(move || {
            piper_send.send_reliable_package_confirmed(frames, Duration::from_millis(500))
        });

        started_rx
            .recv_timeout(Duration::from_millis(200))
            .expect("first package frame should start sending");

        let piper_mode = Arc::clone(&piper);
        let mode_handle =
            std::thread::spawn(move || piper_mode.set_mode(crate::mode::DriverMode::Replay));
        std::thread::sleep(Duration::from_millis(20));
        let _ = release_tx.send(());
        mode_handle
            .join()
            .expect("Replay barrier thread should finish once current frame drains");

        let error = send_handle
            .join()
            .expect("reliable package sender should finish")
            .expect_err("Replay barrier should truncate the remaining package frames");
        assert!(matches!(error, DriverError::ReplayModeActive));

        let sent = sent_frames.lock().expect("sent frames lock");
        assert_eq!(
            sent.as_slice(),
            &[frames[0]],
            "only the in-flight frame may complete before Replay barrier closes the gate"
        );
        let metrics = piper.get_metrics();
        assert_eq!(metrics.tx_packages_partial_total, 1);
    }

    #[test]
    fn test_try_set_mode_replay_times_out_and_latches_transport_fault() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let piper = Piper::new_dual_thread_parts_unvalidated(
            MockRxAdapter,
            BlockingFirstSendTxAdapter {
                sent_frames: sent_frames.clone(),
                started_tx,
                release_rx,
                sends: 0,
            },
            None,
        )
        .unwrap();
        let first = PiperFrame::new_standard(0x301, &[0x01]);

        piper.send_frame(first).expect("first reliable frame should queue");
        started_rx
            .recv_timeout(Duration::from_millis(200))
            .expect("first reliable frame should begin sending");

        let start = Instant::now();

        let error = piper
            .try_set_mode(crate::mode::DriverMode::Replay, Duration::from_millis(50))
            .expect_err("Replay barrier should time out while a normal send is stuck");
        let elapsed = start.elapsed();

        assert!(matches!(error, DriverError::Timeout));
        assert!(elapsed >= Duration::from_millis(45));
        assert!(elapsed < Duration::from_millis(250));
        assert_eq!(piper.mode(), crate::mode::DriverMode::Normal);
        assert_eq!(piper.health().fault, Some(RuntimeFaultKind::TransportError));
        assert!(matches!(
            piper.send_frame(PiperFrame::new_standard(0x302, &[0x02])),
            Err(DriverError::ControlPathClosed)
        ));

        let _ = release_tx.send(());
        wait_until(
            Duration::from_millis(200),
            || sent_frames.lock().expect("sent frames lock").len() == 1,
            "in-flight frame should finish after releasing the blocked adapter",
        );
    }

    #[test]
    fn test_try_set_mode_replay_times_out_in_late_publish_window_and_latches_fault() {
        let piper = Arc::new(
            Piper::new_dual_thread_parts_unvalidated(
                MockRxAdapter,
                RecordingTxAdapter {
                    sent_frames: Arc::new(Mutex::new(Vec::new())),
                },
                None,
            )
            .unwrap(),
        );

        let (reached_rx, release_tx) = install_mode_switch_barrier(piper.as_ref());
        let piper_for_mode = Arc::clone(&piper);
        let mode_handle = std::thread::spawn(move || {
            piper_for_mode.try_set_mode(crate::mode::DriverMode::Replay, Duration::from_millis(20))
        });

        reached_rx
            .recv_timeout(Duration::from_millis(200))
            .expect("Replay switch should reach the late-publish barrier");
        std::thread::sleep(Duration::from_millis(30));
        release_tx.send(()).expect("mode-switch barrier should release cleanly");

        let error = mode_handle
            .join()
            .expect("Replay switch thread should finish")
            .expect_err("Replay switch must honor the caller deadline in the late-publish window");
        assert!(matches!(error, DriverError::Timeout));
        assert_eq!(piper.mode(), crate::mode::DriverMode::Normal);
        assert_eq!(piper.health().fault, Some(RuntimeFaultKind::TransportError));
        assert_eq!(
            piper.normal_send_gate.state(),
            NormalSendGateState::FaultClosed
        );
    }

    #[test]
    fn test_set_mode_replay_wrapper_is_bounded_and_latches_fault_on_timeout() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let piper = Arc::new(
            Piper::new_dual_thread_parts_unvalidated(
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
        let first = PiperFrame::new_standard(0x311, &[0x01]);

        piper.send_frame(first).expect("first reliable frame should queue");
        started_rx
            .recv_timeout(Duration::from_millis(200))
            .expect("first reliable frame should begin sending");

        let piper_mode = Arc::clone(&piper);
        let handle = std::thread::spawn(move || {
            let started_at = Instant::now();
            piper_mode.set_mode(crate::mode::DriverMode::Replay);
            started_at.elapsed()
        });

        wait_until(
            Duration::from_millis(250),
            || handle.is_finished(),
            "set_mode wrapper should not block indefinitely while waiting for Replay barrier",
        );
        let elapsed = handle.join().expect("mode switch thread should finish");

        assert!(elapsed >= Duration::from_millis(90));
        assert!(elapsed < Duration::from_millis(250));
        assert_eq!(piper.mode(), crate::mode::DriverMode::Normal);
        assert_eq!(piper.health().fault, Some(RuntimeFaultKind::TransportError));
        assert!(matches!(
            piper.send_frame(PiperFrame::new_standard(0x312, &[0x02])),
            Err(DriverError::ControlPathClosed)
        ));

        let _ = release_tx.send(());
        wait_until(
            Duration::from_millis(200),
            || sent_frames.lock().expect("sent frames lock").len() == 1,
            "in-flight frame should finish after releasing the blocked adapter",
        );
        assert!(
            !(piper.mode().is_replay()
                && piper.normal_send_gate.state() == NormalSendGateState::Open),
            "timeout path must not leave Replay mode published with an open normal-send gate"
        );
    }

    #[test]
    fn test_try_set_mode_serializes_concurrent_replay_and_normal_switches() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let piper = Arc::new(
            Piper::new_dual_thread_parts_unvalidated(
                MockRxAdapter,
                RecordingTxAdapter {
                    sent_frames: sent_frames.clone(),
                },
                None,
            )
            .unwrap(),
        );
        let (reached_rx, release_tx) = install_mode_switch_barrier(piper.as_ref());

        let piper_replay = Arc::clone(&piper);
        let replay_handle = std::thread::spawn(move || {
            piper_replay.try_set_mode(crate::mode::DriverMode::Replay, Duration::from_millis(100))
        });

        reached_rx
            .recv_timeout(Duration::from_millis(200))
            .expect("Replay switch should block at the mode-switch barrier");

        let piper_normal = Arc::clone(&piper);
        let normal_handle = std::thread::spawn(move || {
            piper_normal.try_set_mode(crate::mode::DriverMode::Normal, Duration::from_millis(100))
        });

        std::thread::sleep(Duration::from_millis(20));
        assert!(
            !normal_handle.is_finished(),
            "Normal mode switch must wait until the in-flight Replay switch releases the lock"
        );

        let _ = release_tx.send(());
        replay_handle
            .join()
            .expect("Replay switch thread should finish")
            .expect("Replay switch should complete before the queued Normal restore");
        normal_handle
            .join()
            .expect("Normal switch thread should finish")
            .expect("Normal restore should succeed after Replay switch completes");

        assert_eq!(piper.mode(), crate::mode::DriverMode::Normal);
        assert_ne!(
            piper.normal_send_gate.state(),
            NormalSendGateState::ReplayPaused,
            "serialized mode switches must not leave the gate stuck in ReplayPaused"
        );
        assert!(
            !piper.replay_barrier_active(),
            "serialized mode switches must not leave the replay barrier engaged"
        );

        let reliable_frame = PiperFrame::new_standard(0x320, &[0x01]);
        let realtime_frame = PiperFrame::new_standard(0x321, &[0x02]);
        piper
            .send_frame(reliable_frame)
            .expect("normal reliable send should work after returning to Normal");
        piper
            .send_realtime(realtime_frame)
            .expect("normal realtime send should work after returning to Normal");
        assert!(matches!(
            piper.send_replay_frame(PiperFrame::new_standard(0x322, &[0x03])),
            Err(DriverError::InvalidInput(_))
        ));

        wait_until(
            Duration::from_millis(200),
            || sent_frames.lock().expect("sent frames lock").len() == 2,
            "normal sends should reach the TX thread after serialized mode switches",
        );
    }

    #[test]
    fn test_set_mode_normal_wrapper_does_not_interleave_with_replay_switch() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let piper = Arc::new(
            Piper::new_dual_thread_parts_unvalidated(
                MockRxAdapter,
                RecordingTxAdapter {
                    sent_frames: sent_frames.clone(),
                },
                None,
            )
            .unwrap(),
        );
        let (reached_rx, release_tx) = install_mode_switch_barrier(piper.as_ref());

        let piper_replay = Arc::clone(&piper);
        let replay_handle = std::thread::spawn(move || {
            piper_replay.try_set_mode(crate::mode::DriverMode::Replay, Duration::from_millis(100))
        });

        reached_rx
            .recv_timeout(Duration::from_millis(200))
            .expect("Replay switch should block at the mode-switch barrier");

        let piper_normal = Arc::clone(&piper);
        let normal_handle = std::thread::spawn(move || {
            piper_normal.set_mode(crate::mode::DriverMode::Normal);
        });

        std::thread::sleep(Duration::from_millis(20));
        assert!(
            !normal_handle.is_finished(),
            "set_mode(Normal) wrapper must also serialize behind the Replay switch"
        );

        let _ = release_tx.send(());
        replay_handle
            .join()
            .expect("Replay switch thread should finish")
            .expect("Replay switch should complete before the wrapper restore");
        normal_handle.join().expect("Normal wrapper thread should finish");

        assert_eq!(piper.mode(), crate::mode::DriverMode::Normal);
        assert_ne!(
            piper.normal_send_gate.state(),
            NormalSendGateState::ReplayPaused,
            "wrapper restore must not leave the gate stuck in ReplayPaused"
        );
        assert!(
            !piper.replay_barrier_active(),
            "wrapper restore must not leave the replay barrier engaged"
        );

        let reliable_frame = PiperFrame::new_standard(0x330, &[0x01]);
        let realtime_frame = PiperFrame::new_standard(0x331, &[0x02]);
        piper
            .send_frame(reliable_frame)
            .expect("normal reliable send should work after wrapper restore");
        piper
            .send_realtime(realtime_frame)
            .expect("normal realtime send should work after wrapper restore");
        assert!(matches!(
            piper.send_replay_frame(PiperFrame::new_standard(0x332, &[0x03])),
            Err(DriverError::InvalidInput(_))
        ));

        wait_until(
            Duration::from_millis(200),
            || sent_frames.lock().expect("sent frames lock").len() == 2,
            "normal sends should reach the TX thread after wrapper restore",
        );
    }

    #[test]
    fn test_try_set_mode_channel_closed_does_not_leave_replay_mode_with_open_gate() {
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let piper = Arc::new(
            Piper::new_dual_thread_parts_unvalidated(
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

        piper
            .send_frame(PiperFrame::new_standard(0x340, &[0x01]))
            .expect("first reliable frame should queue");
        started_rx
            .recv_timeout(Duration::from_millis(200))
            .expect("first reliable frame should begin sending");

        let piper_mode = Arc::clone(&piper);
        let mode_handle = std::thread::spawn(move || {
            piper_mode.try_set_mode(crate::mode::DriverMode::Replay, Duration::from_millis(500))
        });

        std::thread::sleep(Duration::from_millis(20));
        let _ = release_tx.send(());

        let error = mode_handle
            .join()
            .expect("mode switch thread should finish")
            .expect_err("Replay switch should fail once the TX thread exits");
        assert!(
            matches!(
                error,
                DriverError::ChannelClosed | DriverError::ControlPathClosed
            ),
            "mode switch should report channel/runtme closure after the TX worker exits, got {error:?}"
        );
        assert!(
            !(piper.mode().is_replay()
                && piper.normal_send_gate.state() == NormalSendGateState::Open),
            "ChannelClosed path must not leave Replay mode published with an open normal-send gate"
        );
    }

    #[test]
    fn test_get_aligned_motion_aligned() {
        let mock_can = MockCanAdapter;
        let piper = Piper::new_dual_thread(mock_can, None).unwrap();

        piper.ctx.publish_control_joint_position(JointPositionState {
            hardware_timestamp_us: 1_000,
            host_rx_mono_us: 2_000,
            joint_pos: [1.0; 6],
            frame_valid_mask: 0b111,
        });
        piper.ctx.publish_control_joint_dynamic(JointDynamicState {
            group_timestamp_us: 1_000,
            group_host_rx_mono_us: 2_000,
            joint_vel: [2.0; 6],
            joint_current: [3.0; 6],
            timestamps: [1_000; 6],
            valid_mask: 0b11_1111,
        });

        let result = piper.get_aligned_motion(5000, Duration::from_secs(3600));
        match result {
            AlignmentResult::Ok(state) => {
                assert_eq!(state.position_timestamp_us, 1_000);
                assert_eq!(state.dynamic_timestamp_us, 1_000);
                assert_eq!(state.position_frame_valid_mask, 0b111);
                assert_eq!(state.dynamic_valid_mask, 0b11_1111);
                assert_eq!(state.skew_us, 0);
            },
            AlignmentResult::Misaligned { .. }
            | AlignmentResult::Incomplete { .. }
            | AlignmentResult::Stale { .. } => {
                panic!("complete coherent pair should be reported as aligned");
            },
        }
    }

    #[test]
    fn test_get_aligned_motion_misaligned_threshold() {
        let mock_can = MockCanAdapter;
        let piper = Piper::new_dual_thread(mock_can, None).unwrap();

        piper.ctx.publish_control_joint_position(JointPositionState {
            hardware_timestamp_us: 1_000,
            host_rx_mono_us: 2_000,
            joint_pos: [0.0; 6],
            frame_valid_mask: 0b111,
        });
        piper.ctx.publish_control_joint_dynamic(JointDynamicState {
            group_timestamp_us: 2_000,
            group_host_rx_mono_us: 3_000,
            joint_vel: [0.0; 6],
            joint_current: [0.0; 6],
            timestamps: [2_000; 6],
            valid_mask: 0b11_1111,
        });

        let result1 = piper.get_aligned_motion(0, Duration::from_secs(3600));
        let result2 = piper.get_aligned_motion(1000, Duration::from_secs(3600));
        let result3 = piper.get_aligned_motion(1000000, Duration::from_secs(3600));

        match (result1, result2, result3) {
            (
                AlignmentResult::Misaligned { diff_us, .. },
                AlignmentResult::Ok(_),
                AlignmentResult::Ok(_),
            ) => assert_eq!(diff_us, 1_000),
            other => panic!("unexpected threshold behavior: {other:?}"),
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
        assert_eq!(control.confirmed_driver_enabled_mask, None);
    }

    #[test]
    fn test_get_robot_control_recomputes_confirmed_drive_state_from_current_time() {
        let piper = Piper::new_dual_thread_parts_unvalidated(
            MockRxAdapter,
            MockTxAdapter,
            Some(PipelineConfig {
                low_speed_drive_state_freshness_ms: 10,
                ..PipelineConfig::default()
            }),
        )
        .expect("driver should start");

        let now = crate::heartbeat::monotonic_micros();
        piper.ctx.robot_control.store(Arc::new(RobotControlState {
            driver_enabled_mask: 0b11_1111,
            any_drive_enabled: true,
            is_enabled: true,
            confirmed_driver_enabled_mask: Some(0b11_1111),
            ..RobotControlState::default()
        }));
        piper.ctx.joint_driver_low_speed.store(Arc::new(JointDriverLowSpeedState {
            driver_enabled_mask: 0b11_1111,
            host_rx_mono_us: now,
            host_rx_mono_timestamps: [now; 6],
            valid_mask: 0b11_1111,
            ..JointDriverLowSpeedState::default()
        }));

        assert_eq!(
            piper.get_robot_control().confirmed_driver_enabled_mask,
            Some(0b11_1111)
        );

        std::thread::sleep(Duration::from_millis(25));

        assert_eq!(
            piper.get_robot_control().confirmed_driver_enabled_mask,
            None
        );
    }

    #[test]
    fn test_get_joint_driver_low_speed() {
        let mock_can = MockCanAdapter;
        let piper = Piper::new_dual_thread(mock_can, None).unwrap();

        assert!(matches!(
            piper.get_joint_driver_low_speed(),
            Observation::Unavailable
        ));
    }

    #[test]
    fn test_get_joint_limit_config() {
        let mock_can = MockCanAdapter;
        let piper = Piper::new_dual_thread(mock_can, None).unwrap();

        assert!(matches!(
            piper.get_joint_limit_config(),
            Observation::Unavailable
        ));
    }

    fn build_test_piper() -> Piper {
        Piper::new_dual_thread_parts_unvalidated(
            MockRxAdapter,
            MockTxAdapter,
            Some(PipelineConfig {
                low_speed_drive_state_freshness_ms: 75,
                ..PipelineConfig::default()
            }),
        )
        .expect("driver should start")
    }

    fn build_default_test_piper() -> Piper {
        Piper::new_dual_thread_parts_unvalidated(MockRxAdapter, MockTxAdapter, None)
            .expect("driver should start")
    }

    fn inject_low_speed_joint(
        piper: &Piper,
        joint_index: usize,
        host_rx_mono_us: u64,
        hardware_timestamp_us: Option<u64>,
    ) {
        let joint = JointDriverLowSpeedJoint {
            hardware_timestamp_us,
            host_rx_mono_us,
            motor_temp_c: 41.0 + joint_index as f32,
            driver_temp_c: 51.0 + joint_index as f32,
            joint_voltage_v: 24.0 + joint_index as f32,
            joint_bus_current_a: 3.0 + joint_index as f32,
            voltage_low: false,
            motor_over_temp: false,
            over_current: false,
            driver_over_temp: false,
            collision_protection: false,
            driver_error: false,
            enabled: joint_index.is_multiple_of(2),
            stall_protection: false,
        };

        piper
            .ctx
            .joint_driver_low_speed_observation
            .write()
            .expect("low-speed observation store lock")
            .record_slot(joint_index, joint, host_rx_mono_us, hardware_timestamp_us);
    }

    fn inject_all_low_speed_joints(
        piper: &Piper,
        first_host_rx_mono_us: u64,
        hardware_timestamp_us: Option<u64>,
    ) {
        for joint_index in 0..6 {
            inject_low_speed_joint(
                piper,
                joint_index,
                first_host_rx_mono_us + joint_index as u64,
                hardware_timestamp_us,
            );
        }
    }

    fn inject_end_pose_group(
        piper: &Piper,
        first_host_rx_mono_us: u64,
        hardware_timestamp_us: Option<u64>,
    ) {
        let members = [
            EndPoseMembers {
                first: 0.1,
                second: 0.2,
            },
            EndPoseMembers {
                first: 0.3,
                second: 0.4,
            },
            EndPoseMembers {
                first: 0.5,
                second: 0.6,
            },
        ];

        let mut store =
            piper.ctx.end_pose_observation.write().expect("end-pose observation store lock");
        for (slot, member) in members.into_iter().enumerate() {
            store.record_slot(
                slot,
                member,
                first_host_rx_mono_us + slot as u64,
                hardware_timestamp_us,
            );
        }
    }

    #[test]
    fn low_speed_group_can_be_partial_and_stale_at_once() {
        let piper = build_test_piper();
        inject_low_speed_joint(&piper, 0, 1_000_000, Some(100));

        let observation = piper.get_joint_driver_low_speed_at(1_100_000);

        match observation {
            Observation::Available(available) => {
                assert!(matches!(
                    available.payload,
                    ObservationPayload::Partial { .. }
                ));
                assert!(matches!(available.freshness, Freshness::Stale { .. }));
            },
            other => panic!("expected available observation, got {other:?}"),
        }
    }

    #[test]
    fn rebuilt_low_speed_observation_uses_fixed_75ms_stale_policy_by_default() {
        let piper = build_default_test_piper();
        inject_low_speed_joint(&piper, 0, 1_000_000, Some(100));

        let observation = piper.get_joint_driver_low_speed_at(1_076_000);

        match observation {
            Observation::Available(available) => {
                assert!(matches!(
                    available.payload,
                    ObservationPayload::Partial { .. }
                ));
                assert!(matches!(available.freshness, Freshness::Stale { .. }));
            },
            other => panic!("expected stale rebuilt low-speed observation, got {other:?}"),
        }
    }

    #[test]
    fn low_speed_group_can_be_partial_and_fresh() {
        let piper = build_test_piper();
        inject_low_speed_joint(&piper, 0, 1_000_000, Some(100));

        let observation = piper.get_joint_driver_low_speed_at(1_010_000);

        match observation {
            Observation::Available(available) => {
                assert!(matches!(
                    available.payload,
                    ObservationPayload::Partial { .. }
                ));
                assert!(matches!(available.freshness, Freshness::Fresh));
            },
            other => panic!("expected fresh partial observation, got {other:?}"),
        }
    }

    #[test]
    fn low_speed_group_can_be_complete_and_stale() {
        let piper = build_test_piper();
        inject_all_low_speed_joints(&piper, 1_000_000, Some(100));

        let observation = piper.get_joint_driver_low_speed_at(1_100_000);

        match observation {
            Observation::Available(available) => {
                assert!(matches!(available.payload, ObservationPayload::Complete(_)));
                assert!(matches!(available.freshness, Freshness::Stale { .. }));
            },
            other => panic!("expected complete stale observation, got {other:?}"),
        }
    }

    #[test]
    fn wait_for_complete_low_speed_state_returns_complete_payload() {
        let piper = build_test_piper();
        let now = crate::heartbeat::monotonic_micros().max(1);
        inject_all_low_speed_joints(&piper, now, Some(321));

        let result = piper
            .wait_for_complete_low_speed_state(Duration::from_millis(10))
            .expect("complete low-speed observation should become available");

        assert!(result.meta.host_rx_mono_us.is_some());
    }

    #[test]
    fn wait_for_complete_low_speed_state_times_out_with_wait_error() {
        let piper = build_test_piper();

        let err = piper
            .wait_for_complete_low_speed_state(Duration::from_millis(5))
            .expect_err("missing low-speed observation should time out");

        assert!(matches!(err, WaitError::Timeout));
    }

    #[test]
    fn wait_for_complete_low_speed_state_rejects_stale_complete_observation() {
        let piper = build_test_piper();
        let stale_host_mono_us =
            crate::heartbeat::monotonic_micros().max(1).saturating_sub(100_000);
        inject_all_low_speed_joints(&piper, stale_host_mono_us, Some(321));

        let err = piper
            .wait_for_complete_low_speed_state(Duration::from_millis(5))
            .expect_err("stale complete low-speed observation must not satisfy wait helper");

        assert!(matches!(err, WaitError::Timeout));
    }

    #[test]
    fn complete_end_pose_wait_returns_complete_payload() {
        let piper = build_test_piper();
        let now = crate::heartbeat::monotonic_micros().max(1);
        inject_end_pose_group(&piper, now, Some(654));

        let result = piper
            .wait_for_complete_end_pose(Duration::from_millis(10))
            .expect("complete end-pose observation should become available");

        assert!(result.meta.host_rx_mono_us.is_some());
    }

    #[test]
    fn wait_for_complete_end_pose_times_out_with_wait_error() {
        let piper = build_test_piper();

        let err = piper
            .wait_for_complete_end_pose(Duration::from_millis(5))
            .expect_err("missing end-pose observation should time out");

        assert!(matches!(err, WaitError::Timeout));
    }

    #[test]
    fn wait_for_complete_end_pose_rejects_stale_complete_observation() {
        let piper = build_test_piper();
        let stale_host_mono_us = crate::heartbeat::monotonic_micros().max(1).saturating_sub(10_000);
        inject_end_pose_group(&piper, stale_host_mono_us, Some(654));

        let err = piper
            .wait_for_complete_end_pose(Duration::from_millis(5))
            .expect_err("stale complete end-pose observation must not satisfy wait helper");

        assert!(matches!(err, WaitError::Timeout));
    }

    #[test]
    fn test_get_setting_response() {
        let mock_can = MockCanAdapter;
        let piper = Piper::new_dual_thread(mock_can, None).unwrap();

        let response = piper.get_setting_response().unwrap();
        assert_eq!(response.response_index, 0);
        assert!(!response.zero_point_success);
        assert!(!response.is_valid);
    }

    #[test]
    fn test_wait_for_feedback_timeout() {
        let piper =
            Piper::new_dual_thread_parts_unvalidated(MockRxAdapter, MockTxAdapter, None).unwrap();

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
    fn test_wait_for_feedback_ignores_unrecognized_bus_traffic() {
        let piper = Piper::new_dual_thread_parts_unvalidated(
            TimestampedJunkRxAdapter::new(),
            MockTxAdapter,
            None,
        )
        .unwrap();

        let error = piper
            .wait_for_feedback(Duration::from_millis(30))
            .expect_err("unrecognized traffic must not satisfy feedback wait");
        assert!(matches!(error, DriverError::Timeout));
        assert!(!piper.health().connected);
    }

    #[test]
    fn test_public_new_dual_thread_parts_rejects_strict_backend_without_timestamped_feedback() {
        let error = match Piper::new_dual_thread_parts(MockRxAdapter, MockTxAdapter, None) {
            Ok(_) => panic!("public constructor must validate strict startup"),
            Err(error) => error,
        };

        match error {
            DriverError::Can(CanError::Device(device_error)) => {
                assert_eq!(
                    device_error.kind,
                    piper_can::CanDeviceErrorKind::UnsupportedConfig
                );
                assert!(
                    device_error.message.contains("refusing strict connection"),
                    "unexpected error message: {device_error}"
                );
            },
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn test_public_new_dual_thread_parts_accepts_soft_probed_backend_and_replays_bootstrap_feedback()
     {
        let mut frame = PiperFrame::new_standard(0x251, &[0; 8]);
        frame.timestamp_us = 123;

        let piper = Piper::new_dual_thread_parts(
            ProbedBootstrapRxAdapter::new(BackendCapability::SoftRealtime, frame),
            MockTxAdapter,
            None,
        )
        .expect("soft startup probe should admit the runtime");

        assert_eq!(piper.backend_capability(), BackendCapability::SoftRealtime);
        piper
            .wait_for_feedback(Duration::from_millis(200))
            .expect("bootstrap replayed feedback should satisfy wait_for_feedback");
        assert!(piper.health().connected);
    }

    #[test]
    fn test_wait_for_timestamped_feedback_times_out_without_timestamped_frames() {
        let piper =
            Piper::new_dual_thread_parts_unvalidated(MockRxAdapter, MockTxAdapter, None).unwrap();

        let error = piper
            .wait_for_timestamped_feedback(Duration::from_millis(20))
            .expect_err("strict validation should reject missing trusted timestamps");

        match error {
            DriverError::Can(CanError::Device(device_error)) => {
                assert_eq!(
                    device_error.kind,
                    piper_can::CanDeviceErrorKind::UnsupportedConfig
                );
                assert!(
                    device_error
                        .message
                        .to_ascii_lowercase()
                        .contains("strictrealtime requires trusted can timestamps"),
                    "unexpected error message: {device_error}"
                );
            },
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn test_wait_for_timestamped_feedback_succeeds_after_timestamped_frame() {
        let mut frame = PiperFrame::new_standard(0x251, &[0; 8]);
        frame.timestamp_us = 123;
        let piper = Piper::new_dual_thread_parts(
            ScriptedRxAdapter::new(vec![frame], Duration::from_millis(10)),
            MockTxAdapter,
            None,
        )
        .unwrap();

        piper
            .wait_for_timestamped_feedback(Duration::from_millis(200))
            .expect("timestamped feedback should satisfy strict validation");
    }

    #[test]
    fn test_wait_for_timestamped_feedback_ignores_unrecognized_bus_traffic() {
        let piper = Piper::new_dual_thread_parts_unvalidated(
            TimestampedJunkRxAdapter::new(),
            MockTxAdapter,
            None,
        )
        .unwrap();

        let error = piper
            .wait_for_timestamped_feedback(Duration::from_millis(30))
            .expect_err("junk traffic must not satisfy strict timestamp validation");

        match error {
            DriverError::Can(CanError::Device(device_error)) => {
                assert_eq!(
                    device_error.kind,
                    piper_can::CanDeviceErrorKind::UnsupportedConfig
                );
            },
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn test_public_constructor_honors_custom_startup_timeout() {
        let error = match Piper::new_dual_thread_parts_with_startup_timeout(
            DelayedTimestampedFeedbackRxAdapter::new(Duration::from_millis(30)),
            MockTxAdapter,
            None,
            Duration::from_millis(10),
        ) {
            Ok(_) => panic!("late feedback must not satisfy a shorter startup timeout"),
            Err(error) => error,
        };

        match error {
            DriverError::Can(CanError::Device(device_error)) => {
                assert_eq!(
                    device_error.kind,
                    piper_can::CanDeviceErrorKind::UnsupportedConfig
                );
            },
            other => panic!("unexpected error: {other:?}"),
        }

        Piper::new_dual_thread_parts_with_startup_timeout(
            DelayedTimestampedFeedbackRxAdapter::new(Duration::from_millis(30)),
            MockTxAdapter,
            None,
            Duration::from_millis(100),
        )
        .expect("longer startup timeout should admit the same delayed feedback");
    }

    #[test]
    fn test_public_constructor_uses_absolute_startup_deadline_from_entry() {
        let error = match Piper::new_dual_thread_parts_with_startup_timeout(
            DelayedTimestampedFeedbackRxAdapter::new(Duration::from_millis(7)),
            MockTxAdapter,
            None,
            Duration::from_millis(5),
        ) {
            Ok(_) => panic!("feedback arriving after deadline must be rejected"),
            Err(error) => error,
        };

        match error {
            DriverError::Can(CanError::Device(device_error)) => {
                assert_eq!(
                    device_error.kind,
                    piper_can::CanDeviceErrorKind::UnsupportedConfig
                );
                assert!(
                    device_error.message.contains("validation deadline"),
                    "unexpected error message: {device_error}"
                );
            },
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn test_public_constructor_counts_split_time_against_startup_deadline() {
        let error = match Piper::new_dual_thread_with_startup_timeout(
            SlowSplitCanAdapter {
                delay: Duration::from_millis(20),
            },
            None,
            Duration::from_millis(5),
        ) {
            Ok(_) => panic!("slow split must exhaust the startup deadline"),
            Err(error) => error,
        };

        match error {
            DriverError::Can(CanError::Device(device_error)) => {
                assert_eq!(
                    device_error.kind,
                    piper_can::CanDeviceErrorKind::UnsupportedConfig
                );
                assert!(
                    device_error.message.contains("validation deadline"),
                    "unexpected error message: {device_error}"
                );
            },
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn test_wait_for_timestamped_feedback_until_rejects_late_first_feedback() {
        let piper = Piper::new_dual_thread_parts_unvalidated(MockRxAdapter, MockTxAdapter, None)
            .expect("unvalidated driver should build");
        let deadline = StartupValidationDeadline {
            instant_deadline: Instant::now() + Duration::from_millis(50),
            host_rx_deadline_mono_us: crate::heartbeat::monotonic_micros(),
        };

        piper
            .ctx
            .register_timestamped_robot_feedback(deadline.host_rx_deadline_mono_us + 1);

        let error = piper
            .wait_for_timestamped_feedback_until(deadline)
            .expect_err("late first feedback must not satisfy strict startup validation");

        match error {
            DriverError::Can(CanError::Device(device_error)) => {
                assert_eq!(
                    device_error.kind,
                    piper_can::CanDeviceErrorKind::UnsupportedConfig
                );
                assert!(
                    device_error.message.contains("validation deadline"),
                    "unexpected error message: {device_error}"
                );
            },
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn test_wait_for_timestamped_feedback_until_keeps_first_feedback_timestamp() {
        let piper = Piper::new_dual_thread_parts_unvalidated(MockRxAdapter, MockTxAdapter, None)
            .expect("unvalidated driver should build");
        let deadline = StartupValidationDeadline {
            instant_deadline: Instant::now() + Duration::from_millis(50),
            host_rx_deadline_mono_us: crate::heartbeat::monotonic_micros() + 10_000,
        };
        let first_feedback_host_rx_mono_us = deadline.host_rx_deadline_mono_us - 1;

        piper.ctx.register_timestamped_robot_feedback(first_feedback_host_rx_mono_us);
        piper
            .ctx
            .register_timestamped_robot_feedback(deadline.host_rx_deadline_mono_us + 1_000);

        assert_eq!(
            piper.ctx.first_timestamped_feedback_host_rx_mono_us(),
            first_feedback_host_rx_mono_us,
            "later timestamped feedback must not overwrite the first strict startup witness"
        );

        piper
            .wait_for_timestamped_feedback_until(deadline)
            .expect("first in-deadline feedback should satisfy strict startup validation");
    }

    #[test]
    fn test_recv_until_deadline_prefers_ready_value_over_expired_deadline() {
        let (tx, rx) = crossbeam_channel::bounded(1);
        tx.send(7u8).expect("send should succeed");

        let value = recv_until_deadline(&rx, Instant::now(), || DriverError::Timeout)
            .expect("ready ack should win over timeout");

        assert_eq!(value, 7);
    }

    #[test]
    fn test_soft_realtime_rejects_strict_mailbox_apis() {
        let piper = Piper::new_dual_thread_parts(SoftRxAdapter, MockTxAdapter, None).unwrap();

        let single = piper.send_realtime(PiperFrame::new_standard(0x155, &[0x01]));
        assert!(matches!(single, Err(DriverError::InvalidInput(_))));

        let package = piper.send_realtime_package([PiperFrame::new_standard(0x155, &[0x01])]);
        assert!(matches!(package, Err(DriverError::InvalidInput(_))));
    }

    #[test]
    fn test_soft_realtime_deadline_miss_streak_counts_per_command_package() {
        let piper =
            Piper::new_dual_thread_parts(SoftRxAdapter, AlwaysTimeoutTxAdapter, None).unwrap();
        let frames = [
            PiperFrame::new_standard(0x155, &[0x01]),
            PiperFrame::new_standard(0x156, &[0x02]),
        ];

        for _ in 0..3 {
            let error = piper
                .send_soft_realtime_package_confirmed(frames, Duration::from_millis(200))
                .expect_err("0-frame soft realtime misses should time out");
            assert!(matches!(error, DriverError::Timeout));
        }

        std::thread::sleep(Duration::from_millis(20));

        let metrics = piper.get_metrics();
        assert_eq!(metrics.tx_soft_deadline_miss_total, 3);
        assert_eq!(metrics.tx_soft_consecutive_deadline_miss_total, 2);
        assert_eq!(metrics.tx_frames_sent_total, 0);
        assert_eq!(piper.health().fault, Some(RuntimeFaultKind::TransportError));
        assert_eq!(
            piper.normal_send_gate.state(),
            NormalSendGateState::FaultClosed
        );
        assert_eq!(
            piper.maintenance_gate.current_state(),
            MaintenanceGateState::DeniedFaulted
        );
        assert!(matches!(
            piper.send_soft_realtime_package_confirmed(frames, Duration::from_millis(50)),
            Err(DriverError::ControlPathClosed)
        ));
    }

    #[test]
    fn test_soft_realtime_partial_timeout_latches_fault_immediately() {
        let piper =
            Piper::new_dual_thread_parts(SoftRxAdapter, PartialTimeoutTxAdapter { sends: 0 }, None)
                .unwrap();
        let frames = [
            PiperFrame::new_standard(0x155, &[0x01]),
            PiperFrame::new_standard(0x156, &[0x02]),
        ];

        let error = piper
            .send_soft_realtime_package_confirmed(frames, Duration::from_millis(200))
            .expect_err("partial soft realtime package must fail closed");
        assert!(matches!(
            error,
            DriverError::RealtimeDeliveryFailed {
                sent: 1,
                total: 2,
                source: CanError::Timeout,
            }
        ));

        std::thread::sleep(Duration::from_millis(20));

        let metrics = piper.get_metrics();
        assert_eq!(metrics.tx_soft_deadline_miss_total, 0);
        assert_eq!(metrics.tx_soft_consecutive_deadline_miss_total, 0);
        assert_eq!(metrics.tx_frames_sent_total, 1);
        assert_eq!(piper.health().fault, Some(RuntimeFaultKind::TransportError));
        assert_eq!(
            piper.normal_send_gate.state(),
            NormalSendGateState::FaultClosed
        );
        assert_eq!(
            piper.maintenance_gate.current_state(),
            MaintenanceGateState::DeniedFaulted
        );
        assert!(matches!(
            piper.send_soft_realtime_package_confirmed(frames, Duration::from_millis(50)),
            Err(DriverError::ControlPathClosed)
        ));
    }

    #[test]
    fn test_soft_realtime_zero_budget_never_late_sends_expired_command() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let piper = Piper::new_dual_thread_parts(
            SoftRxAdapter,
            RecordingTxAdapter {
                sent_frames: sent_frames.clone(),
            },
            None,
        )
        .unwrap();
        let frames = [PiperFrame::new_standard(0x155, &[0x01])];

        let error = piper
            .send_soft_realtime_package_confirmed(frames, Duration::ZERO)
            .expect_err("zero-budget soft realtime command must time out before commit");
        assert!(matches!(error, DriverError::Timeout));

        std::thread::sleep(Duration::from_millis(20));

        let sent = sent_frames.lock().expect("sent frames lock");
        assert!(
            sent.is_empty(),
            "expired soft realtime command must not commit to the bus later",
        );
        assert_eq!(piper.get_metrics().tx_frames_sent_total, 0);
    }

    #[test]
    fn test_soft_realtime_zero_budget_front_door_rejection_does_not_fill_queue() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let piper = Arc::new(
            Piper::new_dual_thread_parts(
                SoftRxAdapter,
                RecordingTxAdapter {
                    sent_frames: sent_frames.clone(),
                },
                None,
            )
            .unwrap(),
        );
        let (reached_rx, release_tx) = install_tx_loop_barrier(piper.as_ref());
        reached_rx
            .recv_timeout(Duration::from_millis(200))
            .expect("TX loop should hit dispatch barrier");

        let expired_frame = PiperFrame::new_standard(0x155, &[0x01]);
        for _ in 0..4 {
            let error = piper
                .send_soft_realtime_package_confirmed([expired_frame], Duration::ZERO)
                .expect_err("zero-budget soft realtime command must fail at the front door");
            assert!(matches!(error, DriverError::Timeout));
        }

        let piper_for_send = Arc::clone(&piper);
        let valid_frame = PiperFrame::new_standard(0x156, &[0x02]);
        let send_handle = std::thread::spawn(move || {
            piper_for_send
                .send_soft_realtime_package_confirmed([valid_frame], Duration::from_millis(200))
        });

        std::thread::sleep(Duration::from_millis(10));
        let _ = release_tx.send(());

        let result = send_handle.join().expect("soft realtime sender thread should join");
        assert!(
            result.is_ok(),
            "front-door zero-budget rejections must not consume soft realtime queue capacity: {result:?}",
        );

        wait_until(
            Duration::from_millis(200),
            || sent_frames.lock().expect("sent frames lock").len() == 1,
            "valid soft realtime command should still be delivered after zero-budget rejections",
        );
    }

    #[test]
    fn test_soft_realtime_rechecks_deadline_before_enqueue_and_preserves_queue_capacity() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let piper = Arc::new(
            Piper::new_dual_thread_parts(
                SoftRxAdapter,
                RecordingTxAdapter {
                    sent_frames: sent_frames.clone(),
                },
                None,
            )
            .unwrap(),
        );
        let (tx_reached_rx, tx_release_tx) = install_tx_loop_barrier(piper.as_ref());
        tx_reached_rx
            .recv_timeout(Duration::from_millis(200))
            .expect("TX loop should hit dispatch barrier");

        let mut expired_handles = Vec::new();
        for index in 0..4u8 {
            let (admission_reached_rx, admission_release_tx) =
                install_soft_realtime_admission_barrier(piper.as_ref());
            let piper_for_send = Arc::clone(&piper);
            expired_handles.push(thread::spawn(move || {
                piper_for_send.send_soft_realtime_package_confirmed(
                    [PiperFrame::new_standard(0x155 + u16::from(index), &[index])],
                    Duration::from_millis(20),
                )
            }));
            admission_reached_rx
                .recv_timeout(Duration::from_millis(200))
                .expect("soft realtime sender should block at admission barrier");
            thread::sleep(Duration::from_millis(30));
            admission_release_tx.send(()).expect("admission barrier should release cleanly");
        }

        let piper_for_valid_send = Arc::clone(&piper);
        let valid_handle = thread::spawn(move || {
            piper_for_valid_send.send_soft_realtime_package_confirmed(
                [PiperFrame::new_standard(0x159, &[0xFF])],
                Duration::from_millis(200),
            )
        });

        thread::sleep(Duration::from_millis(10));
        tx_release_tx.send(()).expect("TX loop barrier should release cleanly");

        for handle in expired_handles {
            let result = handle.join().expect("expired sender thread should join");
            assert!(
                matches!(result, Err(DriverError::Timeout)),
                "admission-expired command must fail before enqueue: {result:?}",
            );
        }

        let valid_result = valid_handle.join().expect("valid sender thread should join");
        assert!(
            valid_result.is_ok(),
            "admission-expired commands must not consume soft queue capacity: {valid_result:?}",
        );

        wait_until(
            Duration::from_millis(200),
            || sent_frames.lock().expect("sent frames lock").len() == 1,
            "only the valid soft realtime command should reach the bus",
        );
    }

    #[test]
    fn test_soft_realtime_expiry_after_final_admission_check_does_not_fill_queue() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let piper = Arc::new(
            Piper::new_dual_thread_parts(
                SoftRxAdapter,
                RecordingTxAdapter {
                    sent_frames: sent_frames.clone(),
                },
                None,
            )
            .unwrap(),
        );
        let (tx_reached_rx, tx_release_tx) = install_tx_loop_barrier(piper.as_ref());
        tx_reached_rx
            .recv_timeout(Duration::from_millis(200))
            .expect("TX loop should hit dispatch barrier");

        let mut expired_handles = Vec::new();
        for index in 0..4u8 {
            let (post_check_reached_rx, post_check_release_tx) =
                install_soft_realtime_post_check_barrier(piper.as_ref());
            let piper_for_send = Arc::clone(&piper);
            expired_handles.push(thread::spawn(move || {
                piper_for_send.send_soft_realtime_package_confirmed(
                    [PiperFrame::new_standard(0x160 + u16::from(index), &[index])],
                    Duration::from_millis(20),
                )
            }));
            post_check_reached_rx
                .recv_timeout(Duration::from_millis(200))
                .expect("soft realtime sender should block after the final admission check");
            thread::sleep(Duration::from_millis(30));
            post_check_release_tx
                .send(())
                .expect("post-check barrier should release cleanly");
        }

        let piper_for_valid_send = Arc::clone(&piper);
        let valid_handle = thread::spawn(move || {
            piper_for_valid_send.send_soft_realtime_package_confirmed(
                [PiperFrame::new_standard(0x169, &[0xFF])],
                Duration::from_millis(200),
            )
        });

        thread::sleep(Duration::from_millis(10));
        tx_release_tx.send(()).expect("TX loop barrier should release cleanly");

        for handle in expired_handles {
            let result = handle.join().expect("expired sender thread should join");
            assert!(
                matches!(result, Err(DriverError::Timeout)),
                "admission-expired command must fail before becoming TX-visible: {result:?}",
            );
        }

        let valid_result = valid_handle.join().expect("valid sender thread should join");
        assert!(
            valid_result.is_ok(),
            "post-check-expired commands must not consume soft queue capacity: {valid_result:?}",
        );
        assert_eq!(piper.get_metrics().tx_soft_admission_timeout_total, 4);

        wait_until(
            Duration::from_millis(200),
            || sent_frames.lock().expect("sent frames lock").len() == 1,
            "only the valid soft realtime command should reach the bus",
        );
    }

    #[test]
    fn test_soft_realtime_sub_millisecond_remaining_budget_is_not_rounded_up() {
        let piper = Piper::new_dual_thread_parts(
            SoftRxAdapter,
            DelayedSubMillisecondBudgetTxAdapter { sends: 0 },
            None,
        )
        .unwrap();
        let frames = [
            PiperFrame::new_standard(0x155, &[0x01]),
            PiperFrame::new_standard(0x156, &[0x02]),
        ];

        let error = piper
            .send_soft_realtime_package_confirmed(frames, Duration::from_millis(20))
            .expect_err("sub-millisecond remaining budget must not be rounded up");
        assert!(matches!(
            error,
            DriverError::RealtimeDeliveryFailed {
                sent: 1,
                total: 2,
                source: CanError::Timeout,
            }
        ));
    }

    #[test]
    fn test_health_reports_runtime_only_faults() {
        let piper =
            Piper::new_dual_thread_parts_unvalidated(MockRxAdapter, MockTxAdapter, None).unwrap();

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
    fn test_query_collision_protection_returns_fresh_feedback_and_sends_query_frame() {
        let mut data = [0u8; 8];
        data[0..6].copy_from_slice(&[1, 2, 3, 4, 5, 6]);
        let mut feedback = PiperFrame::new_standard(
            piper_protocol::ids::ID_COLLISION_PROTECTION_LEVEL_FEEDBACK as u16,
            &data,
        );
        feedback.timestamp_us = 321;

        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let piper = Piper::new_dual_thread_parts(
            ScriptedRxAdapter::new(vec![feedback], Duration::from_millis(20)),
            RecordingTxAdapter {
                sent_frames: sent_frames.clone(),
            },
            None,
        )
        .unwrap();

        let state = piper
            .query_collision_protection(Duration::from_millis(200))
            .expect("collision protection query should succeed");

        assert_eq!(
            state,
            Complete {
                value: CollisionProtection::try_from_raw_levels([1, 2, 3, 4, 5, 6]).unwrap(),
                meta: crate::observation::ObservationMeta {
                    hardware_timestamp_us: Some(321),
                    host_rx_mono_us: Some(state.meta.host_rx_mono_us.unwrap()),
                    source: crate::observation::ObservationSource::Query,
                },
            }
        );

        let sent = sent_frames.lock().expect("sent frames lock");
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].id, piper_protocol::ids::ID_PARAMETER_QUERY_SET);
        assert_eq!(
            sent[0].data[0],
            piper_protocol::config::ParameterQueryType::CollisionProtectionLevel as u8
        );
        assert_eq!(sent[0].data[1], 0);
    }

    #[test]
    fn query_collision_protection_ignores_pre_commit_feedback() {
        let (frames_tx, frames_rx) = mpsc::channel();
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let piper = Piper::new_dual_thread_parts(
            ChannelRxAdapter::new(frames_rx),
            BlockingTxAdapter {
                entered_tx,
                release_rx,
            },
            None,
        )
        .unwrap();

        let result = thread::scope(|scope| {
            let query = scope.spawn(|| piper.query_collision_protection(Duration::from_millis(80)));
            entered_rx
                .recv_timeout(Duration::from_millis(40))
                .expect("query should reach tx commit point");
            frames_tx
                .send(collision_protection_feedback_frame([1, 2, 3, 4, 5, 6], 42))
                .expect("stale frame should be sent before commit");
            wait_until(
                Duration::from_millis(40),
                || {
                    piper
                        .ctx
                        .collision_protection
                        .read()
                        .map(|state| state.host_rx_mono_us != 0)
                        .unwrap_or(false)
                },
                "stale collision-protection frame should be consumed before tx release",
            );
            release_tx.send(()).expect("tx barrier release should be delivered");
            query.join().expect("query thread should not panic")
        });

        assert!(
            matches!(result, Err(QueryError::Timeout)),
            "expected timeout, got {result:?}"
        );
        assert!(matches!(
            piper.get_collision_protection(),
            Observation::Unavailable
        ));
    }

    #[test]
    fn test_query_end_limit_config_returns_fresh_feedback_and_sends_query_frame() {
        let mut data = [0u8; 8];
        data[0..2].copy_from_slice(&1000u16.to_be_bytes());
        data[2..4].copy_from_slice(&2000u16.to_be_bytes());
        data[4..6].copy_from_slice(&3000u16.to_be_bytes());
        data[6..8].copy_from_slice(&4000u16.to_be_bytes());
        let mut feedback = PiperFrame::new_standard(
            piper_protocol::ids::ID_END_VELOCITY_ACCEL_FEEDBACK as u16,
            &data,
        );
        feedback.timestamp_us = 654;

        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let piper = Piper::new_dual_thread_parts(
            ScriptedRxAdapter::new(vec![feedback], Duration::from_millis(20)),
            RecordingTxAdapter {
                sent_frames: sent_frames.clone(),
            },
            None,
        )
        .unwrap();

        let state = piper
            .query_end_limit_config(Duration::from_millis(200))
            .expect("end limit query should succeed");

        assert_eq!(
            state,
            Complete {
                value: EndLimitConfig {
                    max_linear_velocity_m_s: 1.0,
                    max_angular_velocity_rad_s: 2.0,
                    max_linear_accel_m_s2: 3.0,
                    max_angular_accel_rad_s2: 4.0,
                },
                meta: crate::observation::ObservationMeta {
                    hardware_timestamp_us: Some(654),
                    host_rx_mono_us: Some(state.meta.host_rx_mono_us.unwrap()),
                    source: crate::observation::ObservationSource::Query,
                },
            }
        );

        let sent = sent_frames.lock().expect("sent frames lock");
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].id, piper_protocol::ids::ID_PARAMETER_QUERY_SET);
        assert_eq!(
            sent[0].data[0],
            piper_protocol::config::ParameterQueryType::EndVelocityAccel as u8
        );
    }

    #[test]
    fn query_end_limit_config_ignores_pre_commit_feedback() {
        let (frames_tx, frames_rx) = mpsc::channel();
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let piper = Piper::new_dual_thread_parts(
            ChannelRxAdapter::new(frames_rx),
            BlockingTxAdapter {
                entered_tx,
                release_rx,
            },
            None,
        )
        .unwrap();

        let result = thread::scope(|scope| {
            let query = scope.spawn(|| piper.query_end_limit_config(Duration::from_millis(80)));
            entered_rx
                .recv_timeout(Duration::from_millis(40))
                .expect("query should reach tx commit point");
            frames_tx
                .send(end_limit_feedback_frame(1000, 2000, 3000, 4000, 84))
                .expect("stale frame should be sent before commit");
            wait_until(
                Duration::from_millis(40),
                || {
                    piper
                        .ctx
                        .end_limit_config
                        .read()
                        .map(|state| state.last_update_host_rx_mono_us != 0)
                        .unwrap_or(false)
                },
                "stale end-limit frame should be consumed before tx release",
            );
            release_tx.send(()).expect("tx barrier release should be delivered");
            query.join().expect("query thread should not panic")
        });

        assert!(
            matches!(result, Err(QueryError::Timeout)),
            "expected timeout, got {result:?}"
        );
        assert!(matches!(
            piper.get_end_limit_config(),
            Observation::Unavailable
        ));
    }

    #[test]
    fn test_query_joint_limit_config_queries_all_joints_and_waits_for_full_state() {
        let frames: Vec<PiperFrame> = (1..=6)
            .map(|joint_index| {
                let mut data = [0u8; 8];
                data[0] = joint_index;
                data[1..3].copy_from_slice(&(1800i16 + i16::from(joint_index)).to_be_bytes());
                data[3..5].copy_from_slice(&(-1800i16 - i16::from(joint_index)).to_be_bytes());
                data[5..7].copy_from_slice(&(500u16 + u16::from(joint_index)).to_be_bytes());
                let mut frame = PiperFrame::new_standard(
                    piper_protocol::ids::ID_MOTOR_LIMIT_FEEDBACK as u16,
                    &data,
                );
                frame.timestamp_us = 1000 + u64::from(joint_index);
                frame
            })
            .collect();

        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let piper = Piper::new_dual_thread_parts(
            ScriptedRxAdapter::new(frames, Duration::from_millis(20)),
            RecordingTxAdapter {
                sent_frames: sent_frames.clone(),
            },
            None,
        )
        .unwrap();

        let state = piper
            .query_joint_limit_config(Duration::from_millis(400))
            .expect("joint limit query should succeed");

        assert!((state.value.joints[0].max_angle_rad - 180.1_f64.to_radians()).abs() < 1e-6);
        assert!((state.value.joints[5].min_angle_rad - (-180.6_f64).to_radians()).abs() < 1e-6);
        assert!((state.value.joints[4].max_velocity_rad_s - 0.505).abs() < 1e-6);
        assert_eq!(
            state.meta.source,
            crate::observation::ObservationSource::Query
        );

        let sent = sent_frames.lock().expect("sent frames lock");
        assert_eq!(sent.len(), 6);
        for (idx, frame) in sent.iter().enumerate() {
            assert_eq!(frame.id, piper_protocol::ids::ID_QUERY_MOTOR_LIMIT);
            assert_eq!(frame.data[0], (idx + 1) as u8);
            assert_eq!(
                frame.data[1],
                piper_protocol::config::QueryType::AngleAndMaxVelocity as u8
            );
        }
    }

    #[test]
    fn query_joint_limit_config_ignores_pre_commit_group_feedback() {
        let (frames_tx, frames_rx) = mpsc::channel();
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let piper = Piper::new_dual_thread_parts(
            ChannelRxAdapter::new(frames_rx),
            BlockingTxAdapter {
                entered_tx,
                release_rx,
            },
            None,
        )
        .unwrap();

        let result = thread::scope(|scope| {
            let query = scope.spawn(|| piper.query_joint_limit_config(Duration::from_millis(120)));
            entered_rx
                .recv_timeout(Duration::from_millis(40))
                .expect("query should block before the reliable package fully commits");

            for joint_index in 1..=6 {
                frames_tx
                    .send(joint_limit_feedback_frame(
                        joint_index,
                        1800 + i16::from(joint_index),
                        -1800 - i16::from(joint_index),
                        500 + u16::from(joint_index),
                        100 + u64::from(joint_index),
                    ))
                    .expect("stale frame should be injected before package commit");
            }

            let stale_visible = {
                let deadline = Instant::now() + Duration::from_millis(40);
                let mut visible = false;
                while Instant::now() < deadline {
                    if matches!(
                        piper.get_joint_limit_config(),
                        Observation::Available(Available {
                            payload: ObservationPayload::Complete(_),
                            ..
                        })
                    ) {
                        visible = true;
                        break;
                    }
                    thread::sleep(Duration::from_millis(1));
                }
                visible
            };

            for _ in 0..6 {
                release_tx.send(()).expect("package send release should be delivered");
            }

            (
                stale_visible,
                query.join().expect("query thread should not panic"),
            )
        });

        assert!(
            !result.0,
            "pre-commit group feedback must stay hidden from the query observation store"
        );
        assert!(
            matches!(result.1, Err(QueryError::Timeout)),
            "expected timeout, got {:?}",
            result.1
        );
        assert!(matches!(
            piper.get_joint_limit_config(),
            Observation::Unavailable
        ));
    }

    #[test]
    fn test_query_joint_accel_config_queries_all_joints_and_waits_for_full_state() {
        let frames: Vec<PiperFrame> = (1..=6)
            .map(|joint_index| {
                let mut data = [0u8; 8];
                data[0] = joint_index;
                data[1..3].copy_from_slice(&(1000u16 + 10 * u16::from(joint_index)).to_be_bytes());
                let mut frame = PiperFrame::new_standard(
                    piper_protocol::ids::ID_MOTOR_MAX_ACCEL_FEEDBACK as u16,
                    &data,
                );
                frame.timestamp_us = 2000 + u64::from(joint_index);
                frame
            })
            .collect();

        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let piper = Piper::new_dual_thread_parts(
            ScriptedRxAdapter::new(frames, Duration::from_millis(20)),
            RecordingTxAdapter {
                sent_frames: sent_frames.clone(),
            },
            None,
        )
        .unwrap();

        let state = piper
            .query_joint_accel_config(Duration::from_millis(400))
            .expect("joint accel query should succeed");

        assert!((state.value.max_accel_rad_s2[0] - 1.01).abs() < 1e-6);
        assert!((state.value.max_accel_rad_s2[5] - 1.06).abs() < 1e-6);
        assert_eq!(
            state.meta.source,
            crate::observation::ObservationSource::Query
        );

        let sent = sent_frames.lock().expect("sent frames lock");
        assert_eq!(sent.len(), 6);
        for (idx, frame) in sent.iter().enumerate() {
            assert_eq!(frame.id, piper_protocol::ids::ID_QUERY_MOTOR_LIMIT);
            assert_eq!(frame.data[0], (idx + 1) as u8);
            assert_eq!(
                frame.data[1],
                piper_protocol::config::QueryType::MaxAcceleration as u8
            );
        }
    }

    #[test]
    fn query_joint_accel_config_ignores_pre_commit_group_feedback() {
        let (frames_tx, frames_rx) = mpsc::channel();
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let piper = Piper::new_dual_thread_parts(
            ChannelRxAdapter::new(frames_rx),
            BlockingTxAdapter {
                entered_tx,
                release_rx,
            },
            None,
        )
        .unwrap();

        let result = thread::scope(|scope| {
            let query = scope.spawn(|| piper.query_joint_accel_config(Duration::from_millis(120)));
            entered_rx
                .recv_timeout(Duration::from_millis(40))
                .expect("query should block before the reliable package fully commits");

            for joint_index in 1..=6 {
                frames_tx
                    .send(joint_accel_feedback_frame(
                        joint_index,
                        1000 + 10 * u16::from(joint_index),
                        200 + u64::from(joint_index),
                    ))
                    .expect("stale frame should be injected before package commit");
            }

            let stale_visible = {
                let deadline = Instant::now() + Duration::from_millis(40);
                let mut visible = false;
                while Instant::now() < deadline {
                    if matches!(
                        piper.get_joint_accel_config(),
                        Observation::Available(Available {
                            payload: ObservationPayload::Complete(_),
                            ..
                        })
                    ) {
                        visible = true;
                        break;
                    }
                    thread::sleep(Duration::from_millis(1));
                }
                visible
            };

            for _ in 0..6 {
                release_tx.send(()).expect("package send release should be delivered");
            }

            (
                stale_visible,
                query.join().expect("query thread should not panic"),
            )
        });

        assert!(
            !result.0,
            "pre-commit accel feedback must stay hidden from the query observation store"
        );
        assert!(
            matches!(result.1, Err(QueryError::Timeout)),
            "expected timeout, got {:?}",
            result.1
        );
        assert!(matches!(
            piper.get_joint_accel_config(),
            Observation::Unavailable
        ));
    }

    #[test]
    fn collision_protection_invalid_frame_goes_to_diagnostics_not_state() {
        let frame = PiperFrame::new_standard(
            piper_protocol::ids::ID_COLLISION_PROTECTION_LEVEL_FEEDBACK as u16,
            &[255, 0, 0, 0, 0, 0, 0, 0],
        );
        let piper = Piper::new_dual_thread_parts(
            ScriptedRxAdapter::new(vec![frame], Duration::from_millis(20)),
            MockTxAdapter,
            None,
        )
        .unwrap();

        std::thread::sleep(Duration::from_millis(80));

        assert!(matches!(
            piper.get_collision_protection(),
            Observation::Unavailable
        ));
        assert!(piper.snapshot_diagnostics().iter().any(|event| matches!(
            event,
            DiagnosticEvent::Protocol(ProtocolDiagnostic::OutOfRange { .. })
        )));
    }

    #[test]
    fn unqueried_joint_limit_config_is_unavailable() {
        let piper = Piper::new_dual_thread(MockCanAdapter, None).unwrap();

        assert!(matches!(
            piper.get_joint_limit_config(),
            Observation::Unavailable
        ));
    }

    #[test]
    fn query_timeout_does_not_invalidate_prior_complete_joint_limit_config() {
        let frames: Vec<PiperFrame> = (1..=6)
            .map(|joint_index| {
                let mut data = [0u8; 8];
                data[0] = joint_index;
                data[1..3].copy_from_slice(&(1800i16 + i16::from(joint_index)).to_be_bytes());
                data[3..5].copy_from_slice(&(-1800i16 - i16::from(joint_index)).to_be_bytes());
                data[5..7].copy_from_slice(&(500u16 + u16::from(joint_index)).to_be_bytes());
                PiperFrame::new_standard(piper_protocol::ids::ID_MOTOR_LIMIT_FEEDBACK as u16, &data)
            })
            .collect();
        let piper = Piper::new_dual_thread_parts(
            ScriptedRxAdapter::new(frames, Duration::from_millis(20)),
            MockTxAdapter,
            None,
        )
        .unwrap();

        let _ = piper.query_joint_limit_config(Duration::from_millis(400)).unwrap();

        let error = piper.query_joint_limit_config(Duration::from_millis(10)).unwrap_err();
        assert!(matches!(error, QueryError::Timeout));

        assert!(matches!(
            piper.get_joint_limit_config(),
            Observation::Available(Available {
                payload: ObservationPayload::Complete(_),
                freshness: Freshness::Fresh,
                ..
            })
        ));
    }

    #[test]
    fn query_collision_protection_invalid_feedback_returns_diagnostics_only_timeout() {
        let frame = PiperFrame::new_standard(
            piper_protocol::ids::ID_COLLISION_PROTECTION_LEVEL_FEEDBACK as u16,
            &[255, 0, 0, 0, 0, 0, 0, 0],
        );
        let piper = Piper::new_dual_thread_parts(
            ScriptedRxAdapter::new(vec![frame], Duration::from_millis(20)),
            MockTxAdapter,
            None,
        )
        .unwrap();

        let error = piper.query_collision_protection(Duration::from_millis(60)).unwrap_err();

        assert!(matches!(error, QueryError::DiagnosticsOnlyTimeout));
    }

    #[test]
    fn query_joint_accel_invalid_feedback_returns_diagnostics_only_timeout() {
        let frame = joint_accel_feedback_frame(7, 1000, 20);
        let piper = Piper::new_dual_thread_parts(
            ScriptedRxAdapter::new(vec![frame], Duration::from_millis(20)),
            MockTxAdapter,
            None,
        )
        .unwrap();

        let error = piper.query_joint_accel_config(Duration::from_millis(60)).unwrap_err();

        assert!(matches!(error, QueryError::DiagnosticsOnlyTimeout));
    }

    #[test]
    fn query_collision_protection_timeout_ignores_unrelated_diagnostics() {
        let piper =
            Piper::new_dual_thread_parts(BootstrappedMockRxAdapter::new(), MockTxAdapter, None)
                .unwrap();
        let diagnostics = piper.ctx.diagnostics.clone();
        let push_thread = thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(10));
            diagnostics.push(DiagnosticEvent::Protocol(
                ProtocolDiagnostic::InvalidLength {
                    can_id: piper_protocol::ids::ID_MOTOR_LIMIT_FEEDBACK,
                    expected: 8,
                    actual: 3,
                },
            ));
        });

        let error = piper.query_collision_protection(Duration::from_millis(60)).unwrap_err();
        push_thread.join().expect("diagnostic thread should not panic");

        assert!(matches!(error, QueryError::Timeout));
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

        let result = piper.get_aligned_motion(0, Duration::from_secs(3600));
        match result {
            AlignmentResult::Incomplete {
                position_candidate_mask,
                dynamic_candidate_mask,
            } => {
                assert_eq!(position_candidate_mask, 0);
                assert_eq!(dynamic_candidate_mask, 0);
            },
            AlignmentResult::Ok(_)
            | AlignmentResult::Misaligned { .. }
            | AlignmentResult::Stale { .. } => {
                panic!("empty control path must report incomplete");
            },
        }
    }

    #[test]
    fn test_get_aligned_motion_incomplete_pair_reports_incomplete_result() {
        let mock_can = MockCanAdapter;
        let piper = Piper::new_dual_thread(mock_can, None).unwrap();

        piper.ctx.publish_control_joint_position(JointPositionState {
            hardware_timestamp_us: 1_000,
            host_rx_mono_us: 2_000,
            joint_pos: [0.0; 6],
            frame_valid_mask: 0b101,
        });
        piper.ctx.publish_control_joint_dynamic(JointDynamicState {
            group_timestamp_us: 1_000,
            group_host_rx_mono_us: 2_000,
            joint_vel: [0.0; 6],
            joint_current: [0.0; 6],
            timestamps: [1_000; 6],
            valid_mask: 0b001111,
        });

        let result = piper.get_aligned_motion(0, Duration::from_secs(3600));
        match result {
            AlignmentResult::Incomplete {
                position_candidate_mask,
                dynamic_candidate_mask,
            } => {
                assert_eq!(position_candidate_mask, 0b101);
                assert_eq!(dynamic_candidate_mask, 0b001111);
            },
            AlignmentResult::Ok(_)
            | AlignmentResult::Misaligned { .. }
            | AlignmentResult::Stale { .. } => {
                panic!("incomplete control pair must not be reported as Ok/Misaligned");
            },
        }
    }

    #[test]
    fn test_get_aligned_motion_without_coherent_pair_reports_incomplete_result() {
        let mock_can = MockCanAdapter;
        let piper = Piper::new_dual_thread(mock_can, None).unwrap();

        assert!(piper.get_control_joint_dynamic(Duration::from_millis(1)).is_none());

        piper.ctx.publish_control_joint_position(JointPositionState {
            hardware_timestamp_us: 1_000,
            host_rx_mono_us: 2_000,
            joint_pos: [1.0; 6],
            frame_valid_mask: 0b111,
        });

        assert!(piper.get_control_joint_dynamic(Duration::from_millis(1)).is_none());

        let result = piper.get_aligned_motion(5_000, Duration::from_secs(3600));
        match result {
            AlignmentResult::Incomplete {
                position_candidate_mask,
                dynamic_candidate_mask,
            } => {
                assert_eq!(position_candidate_mask, 0b111);
                assert_eq!(dynamic_candidate_mask, 0);
            },
            AlignmentResult::Ok(_)
            | AlignmentResult::Misaligned { .. }
            | AlignmentResult::Stale { .. } => {
                panic!("control read must not expose an uninitialized pair through Ok/Misaligned");
            },
        }
    }

    #[test]
    fn test_get_aligned_motion_holds_last_coherent_control_pair_until_both_sides_advance() {
        let mock_can = MockCanAdapter;
        let piper = Piper::new_dual_thread(mock_can, None).unwrap();

        piper.ctx.publish_control_joint_position(JointPositionState {
            hardware_timestamp_us: 1_000,
            host_rx_mono_us: 2_000,
            joint_pos: [1.0; 6],
            frame_valid_mask: 0b111,
        });
        piper.ctx.publish_control_joint_dynamic(JointDynamicState {
            group_timestamp_us: 1_000,
            group_host_rx_mono_us: 2_000,
            joint_vel: [10.0; 6],
            joint_current: [20.0; 6],
            timestamps: [1_000; 6],
            valid_mask: 0b11_1111,
        });

        let initial = match piper.get_aligned_motion(5_000, Duration::from_secs(3600)) {
            AlignmentResult::Ok(state) | AlignmentResult::Misaligned { state, .. } => state,
            AlignmentResult::Incomplete { .. } | AlignmentResult::Stale { .. } => {
                panic!("published coherent pair must stay readable");
            },
        };
        assert_eq!(initial.position_timestamp_us, 1_000);
        assert_eq!(initial.dynamic_timestamp_us, 1_000);

        piper.ctx.publish_control_joint_dynamic(JointDynamicState {
            group_timestamp_us: 3_000,
            group_host_rx_mono_us: 4_000,
            joint_vel: [30.0; 6],
            joint_current: [40.0; 6],
            timestamps: [3_000; 6],
            valid_mask: 0b11_1111,
        });

        let after_dynamic_only = match piper.get_aligned_motion(5_000, Duration::from_secs(3600)) {
            AlignmentResult::Ok(state) | AlignmentResult::Misaligned { state, .. } => state,
            AlignmentResult::Incomplete { .. } | AlignmentResult::Stale { .. } => {
                panic!("last coherent pair must remain readable while waiting for peer");
            },
        };
        assert_eq!(after_dynamic_only.position_timestamp_us, 1_000);
        assert_eq!(after_dynamic_only.dynamic_timestamp_us, 1_000);
        assert_eq!(
            piper
                .get_control_joint_dynamic(Duration::from_secs(3600))
                .expect("published coherent pair must expose dynamic state")
                .group_timestamp_us,
            1_000
        );

        piper.ctx.publish_control_joint_position(JointPositionState {
            hardware_timestamp_us: 3_000,
            host_rx_mono_us: 4_000,
            joint_pos: [3.0; 6],
            frame_valid_mask: 0b111,
        });

        let after_both_advanced = match piper.get_aligned_motion(5_000, Duration::from_secs(3600)) {
            AlignmentResult::Ok(state) | AlignmentResult::Misaligned { state, .. } => state,
            AlignmentResult::Incomplete { .. } | AlignmentResult::Stale { .. } => {
                panic!("coherent pair should become readable after both sides advance");
            },
        };
        assert_eq!(after_both_advanced.position_timestamp_us, 3_000);
        assert_eq!(after_both_advanced.dynamic_timestamp_us, 3_000);
        assert_eq!(
            piper
                .get_control_joint_dynamic(Duration::from_secs(3600))
                .expect("coherent pair should become readable after both sides advance")
                .group_timestamp_us,
            3_000
        );
    }

    #[test]
    fn test_get_control_joint_dynamic_returns_none_when_published_pair_is_stale() {
        let mock_can = MockCanAdapter;
        let piper = Piper::new_dual_thread(mock_can, None).unwrap();
        let now = crate::heartbeat::monotonic_micros().max(1);

        piper.ctx.publish_control_joint_position(JointPositionState {
            hardware_timestamp_us: 1_000,
            host_rx_mono_us: now,
            joint_pos: [1.0; 6],
            frame_valid_mask: 0b111,
        });
        piper.ctx.publish_control_joint_dynamic(JointDynamicState {
            group_timestamp_us: 1_000,
            group_host_rx_mono_us: now,
            joint_vel: [10.0; 6],
            joint_current: [20.0; 6],
            timestamps: [1_000; 6],
            valid_mask: 0b11_1111,
        });

        assert!(piper.get_control_joint_dynamic(Duration::from_millis(10)).is_some());
        assert!(piper.get_control_joint_dynamic(Duration::from_nanos(0)).is_none());
    }

    #[test]
    fn test_get_control_joint_dynamic_returns_none_when_only_position_side_is_fresh() {
        let mock_can = MockCanAdapter;
        let piper = Piper::new_dual_thread(mock_can, None).unwrap();
        std::thread::sleep(Duration::from_millis(25));
        let now = crate::heartbeat::monotonic_micros();

        piper.ctx.publish_control_joint_position(JointPositionState {
            hardware_timestamp_us: 2_000,
            host_rx_mono_us: now,
            joint_pos: [1.0; 6],
            frame_valid_mask: 0b111,
        });
        piper.ctx.publish_control_joint_dynamic(JointDynamicState {
            group_timestamp_us: 2_000,
            group_host_rx_mono_us: now.saturating_sub(20_000),
            joint_vel: [10.0; 6],
            joint_current: [20.0; 6],
            timestamps: [2_000; 6],
            valid_mask: 0b11_1111,
        });

        assert!(piper.get_control_joint_dynamic(Duration::from_millis(10)).is_none());
    }

    #[test]
    fn test_get_control_joint_dynamic_returns_none_when_only_dynamic_side_is_fresh() {
        let mock_can = MockCanAdapter;
        let piper = Piper::new_dual_thread(mock_can, None).unwrap();
        std::thread::sleep(Duration::from_millis(25));
        let now = crate::heartbeat::monotonic_micros();

        piper.ctx.publish_control_joint_position(JointPositionState {
            hardware_timestamp_us: 3_000,
            host_rx_mono_us: now.saturating_sub(20_000),
            joint_pos: [1.0; 6],
            frame_valid_mask: 0b111,
        });
        piper.ctx.publish_control_joint_dynamic(JointDynamicState {
            group_timestamp_us: 3_000,
            group_host_rx_mono_us: now,
            joint_vel: [10.0; 6],
            joint_current: [20.0; 6],
            timestamps: [3_000; 6],
            valid_mask: 0b11_1111,
        });

        assert!(piper.get_control_joint_dynamic(Duration::from_millis(10)).is_none());
    }

    #[test]
    fn test_get_aligned_motion_returns_stale_when_published_pair_is_stale() {
        let mock_can = MockCanAdapter;
        let piper = Piper::new_dual_thread(mock_can, None).unwrap();
        let now = crate::heartbeat::monotonic_micros().max(1);

        piper.ctx.publish_control_joint_position(JointPositionState {
            hardware_timestamp_us: 4_000,
            host_rx_mono_us: now,
            joint_pos: [1.0; 6],
            frame_valid_mask: 0b111,
        });
        piper.ctx.publish_control_joint_dynamic(JointDynamicState {
            group_timestamp_us: 4_000,
            group_host_rx_mono_us: now,
            joint_vel: [10.0; 6],
            joint_current: [20.0; 6],
            timestamps: [4_000; 6],
            valid_mask: 0b11_1111,
        });

        match piper.get_aligned_motion(5_000, Duration::from_nanos(0)) {
            AlignmentResult::Stale { state, age } => {
                assert_eq!(state.position_frame_valid_mask, 0b111);
                assert_eq!(state.dynamic_valid_mask, 0b11_1111);
                assert!(age > Duration::from_nanos(0));
            },
            AlignmentResult::Ok(_)
            | AlignmentResult::Misaligned { .. }
            | AlignmentResult::Incomplete { .. } => {
                panic!("stale coherent pair must be classified explicitly as stale");
            },
        }
    }

    #[test]
    fn test_send_realtime_package_confirmed_success() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let piper = Piper::new_dual_thread_parts_unvalidated(
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
    fn test_send_reliable_package_confirmed_success() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let piper = Piper::new_dual_thread_parts_unvalidated(
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
            PiperFrame::new_standard(0x157, &[0x03]),
        ];

        piper
            .send_reliable_package_confirmed(frames, Duration::from_millis(200))
            .expect("confirmed reliable package send should succeed");

        let sent = sent_frames.lock().expect("sent frames lock");
        assert_eq!(sent.as_slice(), &frames);
        wait_until(
            Duration::from_millis(200),
            || piper.get_metrics().tx_packages_completed_total == 1,
            "confirmed reliable package should publish its completed-package metric",
        );
        assert_eq!(piper.get_metrics().tx_packages_completed_total, 1);
    }

    #[test]
    fn test_send_reliable_package_confirmed_reports_partial_transport_failure() {
        let piper = Piper::new_dual_thread_parts_unvalidated(
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
            .send_reliable_package_confirmed(frames, Duration::from_millis(200))
            .expect_err("transport failure should surface partial package delivery");

        assert!(matches!(
            error,
            DriverError::ReliablePackageDeliveryFailed {
                sent: 1,
                total: 3,
                ..
            }
        ));
        assert_eq!(piper.health().fault, Some(RuntimeFaultKind::TransportError));
    }

    #[test]
    fn test_soft_reliable_partial_timeout_fails_closed_immediately() {
        let piper =
            Piper::new_dual_thread_parts(SoftRxAdapter, PartialTimeoutTxAdapter { sends: 0 }, None)
                .unwrap();
        let frames = [
            PiperFrame::new_standard(0x155, &[0x01]),
            PiperFrame::new_standard(0x156, &[0x02]),
        ];

        let error = piper
            .send_reliable_package_confirmed(frames, Duration::from_millis(200))
            .expect_err("partial reliable package must fail closed");
        assert!(matches!(
            error,
            DriverError::ReliablePackageDeliveryFailed {
                sent: 1,
                total: 2,
                source: CanError::Timeout,
            }
        ));

        std::thread::sleep(Duration::from_millis(20));
        let metrics = piper.get_metrics();
        assert_eq!(metrics.tx_soft_deadline_miss_total, 0);
        assert_eq!(metrics.tx_soft_consecutive_deadline_miss_total, 0);
        assert_eq!(metrics.tx_frames_sent_total, 1);
        assert_eq!(metrics.tx_packages_partial_total, 1);
        assert_eq!(piper.health().fault, Some(RuntimeFaultKind::TransportError));
        assert_eq!(
            piper.normal_send_gate.state(),
            NormalSendGateState::FaultClosed
        );
        assert_eq!(
            piper.maintenance_gate.current_state(),
            MaintenanceGateState::DeniedFaulted
        );
        assert!(matches!(
            piper.send_reliable_package_confirmed(frames, Duration::from_millis(50)),
            Err(DriverError::ControlPathClosed)
        ));
    }

    #[test]
    fn test_enqueue_shutdown_success() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let piper = Piper::new_dual_thread_parts_unvalidated(
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
        let piper =
            Piper::new_dual_thread_parts_unvalidated(MockRxAdapter, MockTxAdapter, None).unwrap();
        piper.request_stop();
        let deadline = Instant::now() + Duration::from_millis(100);
        while piper.tx_thread_alive() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
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
        let piper = Piper::new_dual_thread_parts_unvalidated(
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
        let piper = Piper::new_dual_thread_parts_unvalidated(
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
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(piper.health().fault, Some(RuntimeFaultKind::TransportError));
        assert!(matches!(
            piper.send_frame(PiperFrame::new_standard(0x123, &[0x01])),
            Err(DriverError::ControlPathClosed)
        ));
    }

    #[test]
    fn test_shutdown_timeout_latches_fault_but_still_allows_retry_via_shutdown_lane() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let piper = Piper::new_dual_thread_parts_unvalidated(
            MockRxAdapter,
            TimeoutOnceShutdownTxAdapter {
                shutdown_sends: 0,
                sent_frames: sent_frames.clone(),
            },
            None,
        )
        .unwrap();
        let retry_frame = PiperFrame::new_standard(0x472, &[0x02]);

        let error = wait_shutdown(
            &piper,
            PiperFrame::new_standard(0x471, &[0x01]),
            Duration::from_millis(50),
        )
        .expect_err("first shutdown attempt should time out");
        assert!(matches!(error, DriverError::Timeout));

        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(piper.health().fault, Some(RuntimeFaultKind::TransportError));
        assert!(matches!(
            piper.send_reliable(PiperFrame::new_standard(0x123, &[0x01])),
            Err(DriverError::ControlPathClosed)
        ));

        wait_shutdown(&piper, retry_frame, Duration::from_millis(200))
            .expect("shutdown lane should remain available for retry after timeout");

        let sent = sent_frames.lock().expect("sent frames lock");
        assert_eq!(sent.as_slice(), &[retry_frame]);
    }

    #[test]
    fn test_shutdown_receipt_times_out_when_deadline_has_already_passed() {
        let piper = Piper::new_dual_thread_parts_unvalidated(
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
        let piper =
            Piper::new_dual_thread_parts_unvalidated(MockRxAdapter, MockTxAdapter, None).unwrap();
        let timeout = Duration::from_millis(200);
        let receipt = piper
            .enqueue_shutdown(
                PiperFrame::new_standard(0x471, &[0x01]),
                Instant::now() + timeout,
            )
            .expect("enqueue should succeed");
        wait_until(
            timeout,
            || piper.get_metrics().tx_shutdown_sent_total == 1,
            "shutdown send should complete before receipt wait crosses the deadline",
        );
        std::thread::sleep(timeout + Duration::from_millis(20));

        receipt
            .wait()
            .expect("ready ack should still be observed after the shared deadline passes");
    }

    #[test]
    fn test_latch_fault_closes_normal_control_path_but_keeps_shutdown_lane() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let piper = Piper::new_dual_thread_parts_unvalidated(
            MockRxAdapter,
            RecordingTxAdapter {
                sent_frames: sent_frames.clone(),
            },
            None,
        )
        .unwrap();
        let stop_frame = PiperFrame::new_standard(0x471, &[0x09]);

        piper.latch_fault();
        let health = piper.health();

        assert_eq!(health.fault, Some(RuntimeFaultKind::ManualFault));
        assert_eq!(
            piper.normal_send_gate.state(),
            NormalSendGateState::FaultClosed
        );
        assert_eq!(
            piper.maintenance_gate.current_state(),
            MaintenanceGateState::DeniedFaulted
        );
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
    fn test_complete_manual_fault_recovery_after_resume_reopens_normal_control_and_restores_maintenance()
     {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let piper = Arc::new(
            Piper::new_dual_thread_parts_unvalidated(
                MockRxAdapter,
                RecordingTxAdapter {
                    sent_frames: sent_frames.clone(),
                },
                Some(maintenance_ready_config()),
            )
            .unwrap(),
        );
        let reliable_frame = PiperFrame::new_standard(0x472, &[0x0C]);

        mark_maintenance_standby_confirmed(&piper);
        piper.latch_fault();

        let piper_for_feedback = Arc::clone(&piper);
        let feedback_thread = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(5));
            publish_confirmed_driver_mask(&piper_for_feedback, 0);
        });

        let recovery = piper
            .complete_manual_fault_recovery_after_resume_until(
                Instant::now() + Duration::from_millis(200),
                Duration::from_millis(1),
            )
            .expect("manual fault recovery should reopen the runtime");
        feedback_thread.join().expect("feedback thread should join");

        assert_eq!(recovery, ManualFaultRecoveryResult::Standby);
        assert!(piper.health().fault.is_none());
        assert_eq!(piper.runtime_phase(), RuntimePhase::Running);
        assert_eq!(piper.normal_send_gate.state(), NormalSendGateState::Open);
        assert_eq!(
            piper.maintenance_lease_snapshot().state(),
            MaintenanceGateState::AllowedStandby
        );

        piper
            .send_reliable(reliable_frame)
            .expect("normal reliable control must reopen after manual fault recovery");
        wait_until(
            Duration::from_millis(200),
            || sent_frames.lock().expect("sent frames lock").contains(&reliable_frame),
            "recovered runtime should send normal reliable frames again",
        );
    }

    #[test]
    fn test_manual_fault_recovery_times_out_without_fresh_post_resume_feedback() {
        let piper = Piper::new_dual_thread_parts_unvalidated(
            MockRxAdapter,
            MockTxAdapter,
            Some(maintenance_ready_config()),
        )
        .unwrap();

        mark_maintenance_standby_confirmed(&piper);
        piper.latch_fault();

        let error = piper
            .complete_manual_fault_recovery_after_resume_until(
                Instant::now() + Duration::from_millis(20),
                Duration::from_millis(1),
            )
            .expect_err("manual fault recovery must fail closed without fresh feedback");
        assert!(matches!(error, DriverError::Timeout));
        assert_eq!(piper.health().fault, Some(RuntimeFaultKind::ManualFault));
        assert_eq!(piper.runtime_phase(), RuntimePhase::FaultLatched);
        assert_eq!(
            piper.normal_send_gate.state(),
            NormalSendGateState::FaultClosed
        );
        assert_eq!(
            piper.maintenance_lease_snapshot().state(),
            MaintenanceGateState::DeniedFaulted
        );
    }

    #[test]
    fn test_manual_fault_recovery_times_out_without_pre_resume_baseline_even_if_full_batch_arrives()
    {
        let piper = Arc::new(
            Piper::new_dual_thread_parts_unvalidated(
                MockRxAdapter,
                MockTxAdapter,
                Some(maintenance_ready_config()),
            )
            .unwrap(),
        );

        mark_maintenance_standby_confirmed(&piper);
        piper.latch_fault();
        piper
            .ctx
            .joint_driver_low_speed
            .store(Arc::new(JointDriverLowSpeedState::default()));

        let piper_for_feedback = Arc::clone(&piper);
        let feedback_thread = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(5));
            let now = crate::heartbeat::monotonic_micros().max(1);
            piper_for_feedback.ctx.connection_monitor.register_feedback();
            piper_for_feedback.ctx.joint_driver_low_speed.store(Arc::new(
                JointDriverLowSpeedState {
                    host_rx_mono_us: now + 5,
                    host_rx_mono_timestamps: [now, now + 1, now + 2, now + 3, now + 4, now + 5],
                    hardware_timestamp_us: 105,
                    hardware_timestamps: [100, 101, 102, 103, 104, 105],
                    driver_enabled_mask: 0,
                    valid_mask: 0b11_1111,
                    ..JointDriverLowSpeedState::default()
                },
            ));
        });

        let error = piper
            .complete_manual_fault_recovery_after_resume_until(
                Instant::now() + Duration::from_millis(25),
                Duration::from_millis(1),
            )
            .expect_err(
                "recovery must fail closed when no complete pre-resume low-speed baseline exists",
            );
        feedback_thread.join().expect("feedback thread should join");

        assert!(matches!(error, DriverError::Timeout));
        assert_eq!(piper.health().fault, Some(RuntimeFaultKind::ManualFault));
        assert_eq!(piper.runtime_phase(), RuntimePhase::FaultLatched);
        assert_eq!(
            piper.normal_send_gate.state(),
            NormalSendGateState::FaultClosed
        );
    }

    #[test]
    fn test_manual_fault_recovery_expired_deadline_wins_over_ready_feedback() {
        let piper = Piper::new_dual_thread_parts_unvalidated(
            MockRxAdapter,
            MockTxAdapter,
            Some(maintenance_ready_config()),
        )
        .unwrap();

        mark_maintenance_standby_confirmed(&piper);
        piper.latch_fault();
        publish_confirmed_driver_mask(&piper, 0);

        let error = piper
            .complete_manual_fault_recovery_after_resume_until(
                Instant::now(),
                Duration::from_millis(1),
            )
            .expect_err("expired deadline must fail even if ready feedback is already visible");
        assert!(matches!(error, DriverError::Timeout));
        assert_eq!(piper.health().fault, Some(RuntimeFaultKind::ManualFault));
        assert_eq!(piper.runtime_phase(), RuntimePhase::FaultLatched);
        assert_eq!(
            piper.normal_send_gate.state(),
            NormalSendGateState::FaultClosed
        );
    }

    #[test]
    fn test_manual_fault_recovery_times_out_with_partial_pre_resume_baseline() {
        let piper = Arc::new(
            Piper::new_dual_thread_parts_unvalidated(
                MockRxAdapter,
                MockTxAdapter,
                Some(maintenance_ready_config()),
            )
            .unwrap(),
        );

        mark_maintenance_standby_confirmed(&piper);
        piper.latch_fault();

        let baseline_now = crate::heartbeat::monotonic_micros().max(1);
        piper.ctx.connection_monitor.register_feedback();
        piper.ctx.joint_driver_low_speed.store(Arc::new(JointDriverLowSpeedState {
            hardware_timestamps: [100, 101, 102, 0, 0, 0],
            host_rx_mono_timestamps: [baseline_now, baseline_now + 1, baseline_now + 2, 0, 0, 0],
            valid_mask: 0b000111,
            ..JointDriverLowSpeedState::default()
        }));

        let piper_for_feedback = Arc::clone(&piper);
        let feedback_thread = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(5));
            let now = crate::heartbeat::monotonic_micros().max(1);
            piper_for_feedback.ctx.connection_monitor.register_feedback();
            piper_for_feedback.ctx.joint_driver_low_speed.store(Arc::new(
                JointDriverLowSpeedState {
                    host_rx_mono_us: now + 5,
                    host_rx_mono_timestamps: [now, now + 1, now + 2, now + 3, now + 4, now + 5],
                    hardware_timestamp_us: 205,
                    hardware_timestamps: [200, 201, 202, 203, 204, 205],
                    driver_enabled_mask: 0,
                    valid_mask: 0b11_1111,
                    ..JointDriverLowSpeedState::default()
                },
            ));
        });

        let error = piper
            .complete_manual_fault_recovery_after_resume_until(
                Instant::now() + Duration::from_millis(25),
                Duration::from_millis(1),
            )
            .expect_err(
                "recovery must fail closed when pre-resume low-speed baseline was incomplete",
            );
        feedback_thread.join().expect("feedback thread should join");

        assert!(matches!(error, DriverError::Timeout));
        assert_eq!(piper.health().fault, Some(RuntimeFaultKind::ManualFault));
        assert_eq!(piper.runtime_phase(), RuntimePhase::FaultLatched);
        assert_eq!(
            piper.normal_send_gate.state(),
            NormalSendGateState::FaultClosed
        );
    }

    #[test]
    fn test_fault_latch_barrier_prevents_new_normal_sends_after_gate_closes() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let piper = Arc::new(
            Piper::new_dual_thread_parts_unvalidated(
                MockRxAdapter,
                RecordingTxAdapter {
                    sent_frames: sent_frames.clone(),
                },
                None,
            )
            .unwrap(),
        );
        let reliable_frame = PiperFrame::new_standard(0x472, &[0x0A]);
        let realtime_frame = PiperFrame::new_standard(0x155, &[0x0B]);
        let (reached_rx, release_tx) = install_fault_latch_barrier(piper.as_ref());

        let piper_fault = Arc::clone(&piper);
        let fault_handle = std::thread::spawn(move || piper_fault.latch_fault());
        reached_rx
            .recv_timeout(Duration::from_millis(200))
            .expect("fault latch should close the normal gate before publishing the phase");

        assert!(matches!(
            piper.normal_send_gate.acquire_normal(),
            Err(NormalSendGateDenyReason::FaultClosed)
        ));

        let piper_reliable = Arc::clone(&piper);
        let reliable_handle = std::thread::spawn(move || {
            piper_reliable
                .send_reliable_package_confirmed([reliable_frame], Duration::from_millis(200))
        });
        let piper_realtime = Arc::clone(&piper);
        let realtime_handle = std::thread::spawn(move || {
            piper_realtime
                .send_realtime_package_confirmed([realtime_frame], Duration::from_millis(200))
        });

        let _ = release_tx.send(());
        fault_handle.join().expect("fault latch thread should finish cleanly");

        let reliable_error = reliable_handle
            .join()
            .expect("reliable sender thread should finish")
            .expect_err("reliable command must not be sent once fault latch closes the gate");
        assert!(matches!(
            reliable_error,
            DriverError::CommandAbortedByFault | DriverError::ControlPathClosed
        ));

        let realtime_error = realtime_handle
            .join()
            .expect("realtime sender thread should finish")
            .expect_err("realtime command must not be sent once fault latch closes the gate");
        assert!(matches!(
            realtime_error,
            DriverError::CommandAbortedByFault
                | DriverError::ControlPathClosed
                | DriverError::RealtimeDeliveryAbortedByFault { sent: 0, total: 1 }
        ));

        std::thread::sleep(Duration::from_millis(20));
        let sent = sent_frames.lock().expect("sent frames lock");
        assert!(
            sent.is_empty(),
            "no new normal frame may enter tx.send_control once the fault latch closes the gate",
        );
        assert_eq!(piper.health().fault, Some(RuntimeFaultKind::ManualFault));
        assert_eq!(
            piper.normal_send_gate.state(),
            NormalSendGateState::FaultClosed
        );
    }

    #[test]
    fn test_rx_fatal_keeps_shutdown_lane_available_while_tx_is_alive() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let piper = Piper::new_dual_thread_parts_unvalidated(
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
        assert_eq!(
            piper.normal_send_gate.state(),
            NormalSendGateState::FaultClosed
        );
        assert_eq!(
            piper.maintenance_gate.current_state(),
            MaintenanceGateState::DeniedFaulted
        );
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
    fn test_rx_panic_closes_normal_control_front_door() {
        let piper =
            Piper::new_dual_thread_parts_unvalidated(PanickingRxAdapter, MockTxAdapter, None)
                .unwrap();

        wait_until(
            Duration::from_millis(200),
            || !piper.health().rx_alive,
            "RX panic should make the RX worker appear dead",
        );

        let health = piper.health();
        assert!(!health.rx_alive);
        assert_eq!(health.fault, Some(RuntimeFaultKind::RxExited));
        piper.set_maintenance_gate_state(MaintenanceGateState::AllowedStandby);
        assert!(matches!(
            piper.send_realtime(PiperFrame::new_standard(0x155, &[0x01])),
            Err(DriverError::ControlPathClosed)
        ));
        assert!(matches!(
            piper.send_reliable(PiperFrame::new_standard(0x472, &[0x02])),
            Err(DriverError::ControlPathClosed)
        ));
        assert!(!piper.maintenance_runtime_open());
        assert_eq!(
            piper.maintenance_lease_snapshot().state(),
            MaintenanceGateState::DeniedFaulted
        );
        assert_eq!(
            piper
                .acquire_maintenance_lease_gate(11, 22, Duration::from_millis(10))
                .expect("maintenance acquire should expose runtime fault to observers"),
            MaintenanceLeaseAcquireResult::DeniedState {
                state: MaintenanceGateState::DeniedFaulted,
            }
        );
    }

    #[test]
    fn test_tx_panic_closes_maintenance_runtime_observers() {
        let piper =
            Piper::new_dual_thread_parts_unvalidated(MockRxAdapter, PanickingTxAdapter, None)
                .unwrap();
        piper.set_maintenance_gate_state(MaintenanceGateState::AllowedStandby);
        piper
            .send_reliable(PiperFrame::new_standard(0x472, &[0x09]))
            .expect("reliable send should enqueue before the tx worker panics");

        wait_until(
            Duration::from_millis(200),
            || !piper.tx_thread_alive(),
            "TX panic should make the TX worker appear dead",
        );

        let health = piper.health();
        assert!(health.rx_alive);
        assert!(!health.tx_alive);
        assert_eq!(health.fault, Some(RuntimeFaultKind::TxExited));
        assert!(!piper.maintenance_runtime_open());
        assert_eq!(
            piper.maintenance_lease_snapshot().state(),
            MaintenanceGateState::DeniedFaulted
        );
        assert_eq!(
            piper
                .acquire_maintenance_lease_gate(12, 34, Duration::from_millis(10))
                .expect("maintenance acquire should expose tx-dead runtime to observers"),
            MaintenanceLeaseAcquireResult::DeniedState {
                state: MaintenanceGateState::DeniedFaulted,
            }
        );
    }

    #[test]
    fn test_maintenance_lease_is_valid_returns_true_only_while_runtime_is_open() {
        let piper = Piper::new_dual_thread_parts_unvalidated(
            MockRxAdapter,
            MockTxAdapter,
            Some(maintenance_ready_config()),
        )
        .unwrap();

        let session_key = 55;
        mark_maintenance_standby_confirmed(&piper);
        wait_until(
            Duration::from_millis(200),
            || piper.maintenance_lease_snapshot().state() == MaintenanceGateState::AllowedStandby,
            "maintenance runtime should expose standby before lease acquisition",
        );
        let lease_epoch = match piper
            .acquire_maintenance_lease_gate(5, session_key, Duration::from_millis(10))
            .expect("maintenance acquire should succeed while runtime is open")
        {
            MaintenanceLeaseAcquireResult::Granted { lease_epoch } => lease_epoch,
            other => panic!("unexpected maintenance acquire result: {other:?}"),
        };

        assert!(
            piper.maintenance_lease_is_valid(session_key, lease_epoch),
            "matching holder and epoch should be valid while runtime remains open"
        );

        piper
            .try_set_mode(DriverMode::Replay, Duration::from_millis(50))
            .expect("replay mode switch should succeed");

        assert!(
            !piper.maintenance_lease_is_valid(session_key, lease_epoch),
            "replay isolation must invalidate a previously granted maintenance lease"
        );
    }

    #[test]
    fn test_maintenance_lease_is_valid_returns_false_after_manual_fault_latch() {
        let piper = Piper::new_dual_thread_parts_unvalidated(
            MockRxAdapter,
            MockTxAdapter,
            Some(maintenance_ready_config()),
        )
        .unwrap();

        let session_key = 66;
        mark_maintenance_standby_confirmed(&piper);
        wait_until(
            Duration::from_millis(200),
            || piper.maintenance_lease_snapshot().state() == MaintenanceGateState::AllowedStandby,
            "maintenance runtime should expose standby before lease acquisition",
        );
        let lease_epoch = match piper
            .acquire_maintenance_lease_gate(6, session_key, Duration::from_millis(10))
            .expect("maintenance acquire should succeed before manual fault latch")
        {
            MaintenanceLeaseAcquireResult::Granted { lease_epoch } => lease_epoch,
            other => panic!("unexpected maintenance acquire result: {other:?}"),
        };

        assert!(piper.maintenance_lease_is_valid(session_key, lease_epoch));

        piper.latch_fault();

        assert!(
            !piper.maintenance_lease_is_valid(session_key, lease_epoch),
            "manual fault latch must invalidate the cached maintenance lease immediately"
        );
    }

    #[test]
    fn test_fault_latched_shutdown_preempts_pending_realtime_and_reliable_commands() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let piper = Piper::new_dual_thread_parts_unvalidated(
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
    fn test_state_transition_local_send_preempts_pending_normal_control_before_disable() {
        use crate::command::DeliveryPhase;
        use piper_protocol::control::MotorEnableCommand;

        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let piper = Arc::new(
            Piper::new_dual_thread_parts_unvalidated(
                MockRxAdapter,
                RecordingTxAdapter {
                    sent_frames: sent_frames.clone(),
                },
                None,
            )
            .unwrap(),
        );

        let (reached_rx, release_tx) = install_tx_loop_barrier(&piper);
        reached_rx
            .recv_timeout(Duration::from_millis(200))
            .expect("tx loop should reach the installed dispatch barrier");

        let stale_realtime = PiperFrame::new_standard(0x155, &[0x01]);
        let stale_soft = PiperFrame::new_standard(0x156, &[0x02]);
        let stale_reliable = PiperFrame::new_standard(0x151, &[0x03]);
        let disable_frame = MotorEnableCommand::disable_all().to_frame();
        let deadline = Instant::now() + Duration::from_secs(1);
        let (state_reached_rx, state_release_tx) = install_state_transition_barrier(piper.as_ref());

        let (realtime_ack_tx, realtime_ack_rx) = crossbeam_channel::bounded(2);
        {
            let mut slot = piper.realtime_slot.lock().expect("realtime slot lock");
            slot.replace(RealtimeCommand::confirmed(
                [stale_realtime],
                deadline,
                realtime_ack_tx,
            ));
        }

        let (soft_ack_tx, soft_ack_rx) = crossbeam_channel::bounded(1);
        piper
            .soft_realtime_tx
            .try_send(SoftRealtimeCommand::confirmed(
                [stale_soft],
                deadline,
                soft_ack_tx,
            ))
            .expect("stale soft realtime command should queue");

        let (reliable_ack_tx, reliable_ack_rx) = crossbeam_channel::bounded(2);
        piper
            .reliable_tx
            .try_send(ReliableCommand::confirmed(
                stale_reliable,
                deadline,
                reliable_ack_tx,
            ))
            .expect("stale reliable command should queue");

        let piper_for_disable = Arc::clone(&piper);
        let disable_handle = std::thread::spawn(move || {
            piper_for_disable.send_local_state_transition_frame_confirmed(
                disable_frame,
                Duration::from_millis(200),
            )
        });

        wait_until(
            Duration::from_millis(200),
            || piper.normal_send_gate.state() == NormalSendGateState::StateTransitionClosed,
            "state-transition disable should close the normal-control gate before TX resumes",
        );
        release_tx.send(()).expect("tx loop barrier should release cleanly");
        state_reached_rx
            .recv_timeout(Duration::from_millis(200))
            .expect("state-transition dispatch should abort pending normal control before send");
        state_release_tx
            .send(())
            .expect("state-transition barrier should release cleanly");
        disable_handle
            .join()
            .expect("state-transition sender thread should finish cleanly")
            .expect("state-transition disable should preempt pending normal control");

        wait_until(
            Duration::from_millis(200),
            || !sent_frames.lock().expect("sent frames lock").is_empty(),
            "disable frame should be the only transmitted frame",
        );

        let realtime_error = match realtime_ack_rx
            .recv_timeout(Duration::from_millis(200))
            .expect("realtime waiter should receive an abort result")
        {
            DeliveryPhase::Finished(result) => result.expect_err("realtime command must abort"),
            DeliveryPhase::Committed { .. } => panic!("stale realtime command must not commit"),
        };
        assert!(matches!(
            realtime_error,
            DriverError::CommandAbortedByStateTransition
        ));

        let soft_error = soft_ack_rx
            .recv_timeout(Duration::from_millis(200))
            .expect("soft realtime waiter should receive an abort result")
            .expect_err("soft realtime command must abort");
        assert!(matches!(
            soft_error,
            DriverError::CommandAbortedByStateTransition
        ));

        let reliable_error = match reliable_ack_rx
            .recv_timeout(Duration::from_millis(200))
            .expect("reliable waiter should receive an abort result")
        {
            DeliveryPhase::Finished(result) => result.expect_err("reliable command must abort"),
            DeliveryPhase::Committed { .. } => panic!("stale reliable command must not commit"),
        };
        assert!(matches!(
            reliable_error,
            DriverError::CommandAbortedByStateTransition
        ));

        let sent = sent_frames.lock().expect("sent frames lock");
        assert_eq!(
            sent.as_slice(),
            &[disable_frame],
            "state transition abort must prevent stale normal-control frames from being transmitted",
        );
    }

    #[test]
    fn test_state_transition_closes_front_door_before_disable_dispatch() {
        use piper_protocol::control::MotorEnableCommand;

        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let piper = Arc::new(
            Piper::new_dual_thread_parts_unvalidated(
                MockRxAdapter,
                RecordingTxAdapter {
                    sent_frames: sent_frames.clone(),
                },
                None,
            )
            .unwrap(),
        );

        let disable_frame = MotorEnableCommand::disable_all().to_frame();
        let reliable_frame = PiperFrame::new_standard(0x151, &[0x05]);
        let realtime_frame = PiperFrame::new_standard(0x155, &[0x06]);
        let (reached_rx, release_tx) = install_state_transition_barrier(piper.as_ref());

        let piper_for_disable = Arc::clone(&piper);
        let disable_handle = std::thread::spawn(move || {
            piper_for_disable.send_local_state_transition_frame_confirmed(
                disable_frame,
                Duration::from_millis(200),
            )
        });

        reached_rx
            .recv_timeout(Duration::from_millis(200))
            .expect("state-transition send should close the front door before dispatch");

        let reliable_error = piper
            .send_reliable_package_confirmed([reliable_frame], Duration::from_millis(200))
            .expect_err("new reliable submissions must be rejected during state transition");
        assert!(matches!(reliable_error, DriverError::ControlPathClosed));

        let realtime_error = piper
            .send_realtime_package_confirmed([realtime_frame], Duration::from_millis(200))
            .expect_err("new realtime submissions must be rejected during state transition");
        assert!(matches!(realtime_error, DriverError::ControlPathClosed));

        release_tx.send(()).expect("state-transition barrier should release cleanly");
        disable_handle
            .join()
            .expect("state-transition sender thread should finish")
            .expect("state-transition disable should complete");

        assert_eq!(
            piper.normal_send_gate.state(),
            NormalSendGateState::StateTransitionClosed,
            "disable dispatch must keep the front door closed until drives are confirmed disabled",
        );

        let sent = sent_frames.lock().expect("sent frames lock");
        assert_eq!(
            sent.as_slice(),
            &[disable_frame],
            "state-transition barrier must not allow new commands to slip ahead of disable",
        );
    }

    #[test]
    fn test_try_set_mode_replay_is_rejected_while_state_transition_disable_is_pending() {
        use piper_protocol::control::MotorEnableCommand;

        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let piper = Arc::new(
            Piper::new_dual_thread_parts_unvalidated(
                MockRxAdapter,
                RecordingTxAdapter {
                    sent_frames: sent_frames.clone(),
                },
                None,
            )
            .unwrap(),
        );

        let disable_frame = MotorEnableCommand::disable_all().to_frame();
        let (reached_rx, release_tx) = install_state_transition_barrier(piper.as_ref());
        let piper_for_disable = Arc::clone(&piper);
        let disable_handle = std::thread::spawn(move || {
            piper_for_disable.send_local_state_transition_frame_confirmed(
                disable_frame,
                Duration::from_millis(200),
            )
        });

        reached_rx
            .recv_timeout(Duration::from_millis(200))
            .expect("state-transition disable should reach the pre-dispatch barrier");

        let replay_error = piper
            .try_set_mode(crate::mode::DriverMode::Replay, Duration::from_millis(50))
            .expect_err("Replay mode must not overtake a pending safety disable");
        assert!(matches!(replay_error, DriverError::ControlPathClosed));
        assert_eq!(
            piper.mode(),
            crate::mode::DriverMode::Normal,
            "failed Replay switch must not publish Replay mode while disable is pending",
        );

        release_tx.send(()).expect("state-transition barrier should release cleanly");
        disable_handle
            .join()
            .expect("state-transition sender thread should finish")
            .expect("pending disable must still complete after Replay is rejected");

        let sent = sent_frames.lock().expect("sent frames lock");
        assert_eq!(
            sent.as_slice(),
            &[disable_frame],
            "Replay rejection must not cancel the pending disable frame",
        );
    }

    #[test]
    fn test_state_transition_disable_preempts_inflight_replay_switch_before_lock_release() {
        use piper_protocol::control::MotorEnableCommand;

        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let piper = Arc::new(
            Piper::new_dual_thread_parts_unvalidated(
                MockRxAdapter,
                BlockingFirstSendTxAdapter {
                    sent_frames: sent_frames.clone(),
                    started_tx,
                    release_rx,
                    sends: 0,
                },
                None,
            )
            .expect("driver should start"),
        );

        let inflight_frame = PiperFrame::new_standard(0x303, &[0x01]);
        piper
            .send_frame(inflight_frame)
            .expect("reliable frame should enter the blocked in-flight send");
        started_rx
            .recv_timeout(Duration::from_millis(200))
            .expect("reliable frame should begin sending");

        let piper_replay = Arc::clone(&piper);
        let replay_handle = std::thread::spawn(move || {
            piper_replay.try_set_mode(crate::mode::DriverMode::Replay, Duration::from_millis(200))
        });

        wait_until(
            Duration::from_millis(100),
            || piper.replay_barrier_active(),
            "replay switch should pause the front door while it waits for inflight sends",
        );

        let disable_frame = MotorEnableCommand::disable_all().to_frame();
        let piper_disable = Arc::clone(&piper);
        let disable_handle = std::thread::spawn(move || {
            let started_at = Instant::now();
            let result = piper_disable.send_local_state_transition_frame_confirmed(
                disable_frame,
                Duration::from_millis(20),
            );
            (started_at.elapsed(), result)
        });

        wait_until(
            Duration::from_millis(100),
            || piper.normal_send_gate.state() == NormalSendGateState::StateTransitionClosed,
            "safety disable should close the front door before the replay switch releases the lock",
        );

        wait_until(
            Duration::from_millis(100),
            || replay_handle.is_finished(),
            "replay switch should fail quickly once safety disable wins the front door",
        );

        let replay_error = replay_handle
            .join()
            .expect("replay switch thread should finish")
            .expect_err("replay switch must not block a pending safety disable");
        assert!(matches!(replay_error, DriverError::ControlPathClosed));
        assert_eq!(piper.mode(), crate::mode::DriverMode::Normal);

        let (disable_elapsed, disable_result) =
            disable_handle.join().expect("state-transition sender thread should finish");
        let disable_error =
            disable_result.expect_err("blocked state-transition disable must still time out");

        assert!(matches!(disable_error, DriverError::Timeout));
        assert!(
            disable_elapsed < Duration::from_millis(120),
            "disable timeout should not be dragged by the replay timeout, got {disable_elapsed:?}"
        );
        assert_eq!(piper.health().fault, Some(RuntimeFaultKind::TransportError));
        assert_eq!(
            piper.normal_send_gate.state(),
            NormalSendGateState::FaultClosed
        );

        release_tx.send(()).expect("blocked adapter should release cleanly");

        let sent = sent_frames.lock().expect("sent frames lock");
        assert_eq!(
            sent.as_slice(),
            &[inflight_frame],
            "replay-preempted state-transition disable must not commit after timing out",
        );
    }

    #[test]
    fn test_state_transition_disable_preempts_replay_in_late_publish_window() {
        use piper_protocol::control::MotorEnableCommand;

        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let piper = Arc::new(
            Piper::new_dual_thread_parts_unvalidated(
                MockRxAdapter,
                RecordingTxAdapter {
                    sent_frames: sent_frames.clone(),
                },
                None,
            )
            .expect("driver should start"),
        );

        let (reached_rx, release_tx) = install_mode_switch_barrier(piper.as_ref());
        let piper_replay = Arc::clone(&piper);
        let replay_handle = std::thread::spawn(move || {
            piper_replay.try_set_mode(crate::mode::DriverMode::Replay, Duration::from_millis(100))
        });

        reached_rx
            .recv_timeout(Duration::from_millis(200))
            .expect("Replay switch should reach the late-publish barrier");

        let disable_frame = MotorEnableCommand::disable_all().to_frame();
        let piper_disable = Arc::clone(&piper);
        let disable_handle = std::thread::spawn(move || {
            piper_disable.send_local_state_transition_frame_confirmed(
                disable_frame,
                Duration::from_millis(100),
            )
        });

        wait_until(
            Duration::from_millis(100),
            || piper.normal_send_gate.state() == NormalSendGateState::StateTransitionClosed,
            "safety disable should close the front door while replay is blocked at late publish",
        );

        release_tx.send(()).expect("mode-switch barrier should release cleanly");

        let replay_result =
            replay_handle.join().expect("replay switch thread should finish cleanly");
        let disable_result = disable_handle
            .join()
            .expect("state-transition sender thread should finish cleanly");

        assert!(
            replay_result.is_err(),
            "late-publish replay switch must not succeed once safety disable has closed the gate",
        );
        assert!(
            !matches!(disable_result, Err(DriverError::ReplayModeActive)),
            "safety disable must not lose the race to a replay publish that was still in flight",
        );
        assert_eq!(
            piper.mode(),
            crate::mode::DriverMode::Normal,
            "late-publish race must not leave Replay mode published",
        );
        let sent = sent_frames.lock().expect("sent frames lock");
        assert!(
            sent.as_slice() == [disable_frame] || sent.is_empty(),
            "late-publish race must not emit replay traffic",
        );
    }

    #[test]
    fn test_state_transition_timeout_before_commit_latches_transport_fault_and_never_reopens() {
        use piper_protocol::control::MotorEnableCommand;

        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let piper = Arc::new(
            Piper::new_dual_thread_parts_unvalidated(
                MockRxAdapter,
                RecordingTxAdapter {
                    sent_frames: sent_frames.clone(),
                },
                None,
            )
            .unwrap(),
        );

        let disable_frame = MotorEnableCommand::disable_all().to_frame();
        let reliable_frame = PiperFrame::new_standard(0x151, &[0x07]);
        let (reached_rx, release_tx) = install_state_transition_barrier(piper.as_ref());

        let piper_for_disable = Arc::clone(&piper);
        let disable_handle = std::thread::spawn(move || {
            piper_for_disable.send_local_state_transition_frame_confirmed(
                disable_frame,
                Duration::from_millis(20),
            )
        });

        reached_rx
            .recv_timeout(Duration::from_millis(200))
            .expect("state-transition send should reach the pre-commit barrier");
        thread::sleep(Duration::from_millis(30));
        release_tx.send(()).expect("state-transition barrier should release cleanly");

        let error = disable_handle
            .join()
            .expect("state-transition sender thread should finish cleanly")
            .expect_err("deadline-expired state-transition disable must time out");
        assert!(matches!(error, DriverError::Timeout));
        assert_eq!(piper.health().fault, Some(RuntimeFaultKind::TransportError));
        assert_eq!(
            piper.normal_send_gate.state(),
            NormalSendGateState::FaultClosed
        );
        assert_eq!(
            piper.maintenance_gate.current_state(),
            MaintenanceGateState::DeniedFaulted
        );
        assert!(matches!(
            piper.send_reliable_package_confirmed([reliable_frame], Duration::from_millis(50)),
            Err(DriverError::ControlPathClosed)
        ));

        let sent = sent_frames.lock().expect("sent frames lock");
        assert!(
            sent.is_empty(),
            "expired state-transition disable must not commit to the bus later",
        );
    }

    #[test]
    fn test_state_transition_send_control_timeout_latches_transport_fault_immediately() {
        use piper_protocol::control::MotorEnableCommand;

        let piper = Piper::new_dual_thread_parts(SoftRxAdapter, AlwaysTimeoutTxAdapter, None)
            .expect("driver should start");
        let disable_frame = MotorEnableCommand::disable_all().to_frame();

        let error = piper
            .send_local_state_transition_frame_confirmed(disable_frame, Duration::from_millis(50))
            .expect_err("state-transition disable must fail closed on a single send timeout");

        assert!(matches!(error, DriverError::Timeout));
        assert_eq!(piper.health().fault, Some(RuntimeFaultKind::TransportError));
        assert_eq!(
            piper.normal_send_gate.state(),
            NormalSendGateState::FaultClosed
        );
        assert_eq!(
            piper.maintenance_gate.current_state(),
            MaintenanceGateState::DeniedFaulted
        );
        assert!(matches!(
            piper.send_reliable(PiperFrame::new_standard(0x151, &[0x01])),
            Err(DriverError::ControlPathClosed)
        ));
    }

    #[test]
    fn test_state_transition_timeout_returns_within_own_budget_even_if_mode_switch_holds_lock() {
        use piper_protocol::control::MotorEnableCommand;

        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let piper = Arc::new(
            Piper::new_dual_thread_parts_unvalidated(
                MockRxAdapter,
                BlockingFirstSendTxAdapter {
                    sent_frames: sent_frames.clone(),
                    started_tx,
                    release_rx,
                    sends: 0,
                },
                None,
            )
            .expect("driver should start"),
        );

        let inflight_frame = PiperFrame::new_standard(0x301, &[0x01]);
        piper
            .send_frame(inflight_frame)
            .expect("reliable frame should enter the blocked in-flight send");
        started_rx
            .recv_timeout(Duration::from_millis(200))
            .expect("reliable frame should begin sending");

        let disable_frame = MotorEnableCommand::disable_all().to_frame();
        let piper_disable = Arc::clone(&piper);
        let disable_handle = std::thread::spawn(move || {
            let started_at = Instant::now();
            let result = piper_disable.send_local_state_transition_frame_confirmed(
                disable_frame,
                Duration::from_millis(20),
            );
            (started_at.elapsed(), result)
        });

        wait_until(
            Duration::from_millis(100),
            || piper.normal_send_gate.state() == NormalSendGateState::StateTransitionClosed,
            "state-transition disable should close the front door before timeout",
        );

        let piper_mode = Arc::clone(&piper);
        let replay_handle = std::thread::spawn(move || {
            piper_mode.try_set_mode(crate::mode::DriverMode::Replay, Duration::from_millis(200))
        });

        let (disable_elapsed, disable_result) =
            disable_handle.join().expect("state-transition sender thread should finish");
        let disable_error =
            disable_result.expect_err("state-transition disable must still time out");

        assert!(matches!(disable_error, DriverError::Timeout));
        assert!(
            disable_elapsed < Duration::from_millis(120),
            "disable timeout should return close to its own budget, got {disable_elapsed:?}"
        );
        assert_eq!(piper.health().fault, Some(RuntimeFaultKind::TransportError));
        assert_eq!(
            piper.normal_send_gate.state(),
            NormalSendGateState::FaultClosed
        );
        assert_eq!(
            piper.maintenance_gate.current_state(),
            MaintenanceGateState::DeniedFaulted
        );
        assert!(matches!(
            piper.send_frame(PiperFrame::new_standard(0x302, &[0x02])),
            Err(DriverError::ControlPathClosed)
        ));

        release_tx.send(()).expect("blocked adapter should release cleanly");
        let replay_error = replay_handle
            .join()
            .expect("replay mode switch thread should finish")
            .expect_err("fault-latched mode switch must not publish Replay");
        assert!(matches!(
            replay_error,
            DriverError::ControlPathClosed | DriverError::Timeout
        ));
        assert_eq!(piper.mode(), crate::mode::DriverMode::Normal);

        let sent = sent_frames.lock().expect("sent frames lock");
        assert_eq!(
            sent.as_slice(),
            &[inflight_frame],
            "timed-out state-transition disable must not commit after returning timeout",
        );
    }

    #[test]
    fn test_state_transition_send_control_timeout_returns_timeout_on_strict_backend() {
        use piper_protocol::control::MotorEnableCommand;

        let piper =
            Piper::new_dual_thread_parts_unvalidated(MockRxAdapter, AlwaysTimeoutTxAdapter, None)
                .expect("driver should start");
        let disable_frame = MotorEnableCommand::disable_all().to_frame();

        let error = piper
            .send_local_state_transition_frame_confirmed(disable_frame, Duration::from_millis(50))
            .expect_err("strict-backend state-transition timeout must still surface as Timeout");

        assert!(matches!(error, DriverError::Timeout));
        assert_eq!(piper.health().fault, Some(RuntimeFaultKind::TransportError));
        assert_eq!(
            piper.normal_send_gate.state(),
            NormalSendGateState::FaultClosed
        );
        assert_eq!(
            piper.maintenance_gate.current_state(),
            MaintenanceGateState::DeniedFaulted
        );
    }

    #[test]
    fn test_drop_running_disable_preempts_pending_normal_control_before_disable() {
        use crate::command::DeliveryPhase;
        use piper_protocol::control::MotorEnableCommand;

        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let piper = Arc::new(
            Piper::new_dual_thread_parts_unvalidated(
                MockRxAdapter,
                RecordingTxAdapter {
                    sent_frames: sent_frames.clone(),
                },
                None,
            )
            .unwrap(),
        );

        let (reached_rx, release_tx) = install_tx_loop_barrier(&piper);
        reached_rx
            .recv_timeout(Duration::from_millis(200))
            .expect("tx loop should reach the installed dispatch barrier");

        let stale_realtime = PiperFrame::new_standard(0x155, &[0x01]);
        let stale_soft = PiperFrame::new_standard(0x156, &[0x02]);
        let stale_reliable = PiperFrame::new_standard(0x151, &[0x03]);
        let disable_frame = MotorEnableCommand::disable_all().to_frame();
        let deadline = Instant::now() + Duration::from_secs(1);
        let (state_reached_rx, state_release_tx) = install_state_transition_barrier(piper.as_ref());

        let (realtime_ack_tx, realtime_ack_rx) = crossbeam_channel::bounded(2);
        {
            let mut slot = piper.realtime_slot.lock().expect("realtime slot lock");
            slot.replace(RealtimeCommand::confirmed(
                [stale_realtime],
                deadline,
                realtime_ack_tx,
            ));
        }

        let (soft_ack_tx, soft_ack_rx) = crossbeam_channel::bounded(1);
        piper
            .soft_realtime_tx
            .try_send(SoftRealtimeCommand::confirmed(
                [stale_soft],
                deadline,
                soft_ack_tx,
            ))
            .expect("stale soft realtime command should queue");

        let (reliable_ack_tx, reliable_ack_rx) = crossbeam_channel::bounded(2);
        piper
            .reliable_tx
            .try_send(ReliableCommand::confirmed(
                stale_reliable,
                deadline,
                reliable_ack_tx,
            ))
            .expect("stale reliable command should queue");

        let piper_for_drop = Arc::clone(&piper);
        let drop_handle = thread::spawn(move || {
            piper_for_drop.best_effort_disable_or_shutdown_on_drop(Duration::from_millis(200));
        });

        wait_until(
            Duration::from_millis(200),
            || piper.normal_send_gate.state() == NormalSendGateState::StateTransitionClosed,
            "drop-time disable should close the normal-control gate before TX resumes",
        );
        release_tx.send(()).expect("tx loop barrier should release cleanly");
        state_reached_rx
            .recv_timeout(Duration::from_millis(200))
            .expect("drop-time disable should abort pending normal control before send");
        state_release_tx
            .send(())
            .expect("state-transition barrier should release cleanly");
        drop_handle.join().expect("drop-time disable thread should join cleanly");

        assert_eq!(
            piper.normal_send_gate.state(),
            NormalSendGateState::StateTransitionClosed,
            "drop-time disable must also keep the front door closed until disabled feedback arrives",
        );

        wait_until(
            Duration::from_millis(200),
            || !sent_frames.lock().expect("sent frames lock").is_empty(),
            "drop-time disable should be transmitted after preempting stale normal control",
        );

        let realtime_error = match realtime_ack_rx
            .recv_timeout(Duration::from_millis(200))
            .expect("realtime waiter should receive an abort result")
        {
            DeliveryPhase::Finished(result) => result.expect_err("realtime command must abort"),
            DeliveryPhase::Committed { .. } => panic!("stale realtime command must not commit"),
        };
        assert!(matches!(
            realtime_error,
            DriverError::CommandAbortedByStateTransition
        ));

        let soft_error = soft_ack_rx
            .recv_timeout(Duration::from_millis(200))
            .expect("soft realtime waiter should receive an abort result")
            .expect_err("soft realtime command must abort");
        assert!(matches!(
            soft_error,
            DriverError::CommandAbortedByStateTransition
        ));

        let reliable_error = match reliable_ack_rx
            .recv_timeout(Duration::from_millis(200))
            .expect("reliable waiter should receive an abort result")
        {
            DeliveryPhase::Finished(result) => result.expect_err("reliable command must abort"),
            DeliveryPhase::Committed { .. } => panic!("stale reliable command must not commit"),
        };
        assert!(matches!(
            reliable_error,
            DriverError::CommandAbortedByStateTransition
        ));

        let sent = sent_frames.lock().expect("sent frames lock");
        assert_eq!(
            sent.as_slice(),
            &[disable_frame],
            "drop-time disable must preempt stale normal-control traffic and reach the bus first",
        );
    }

    #[test]
    fn test_realtime_package_fault_abort_reports_sent_prefix_and_stops_remaining_frames() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let piper = Arc::new(
            Piper::new_dual_thread_parts_unvalidated(
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
    fn test_rx_fatal_aborts_realtime_package_after_inflight_frame_and_closes_gate() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let fatal_trigger = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let piper = Arc::new(
            Piper::new_dual_thread_parts_unvalidated(
                TriggeredFatalRxAdapter {
                    trigger: fatal_trigger.clone(),
                    tripped: false,
                },
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
            PiperFrame::new_standard(0x165, &[0x01]),
            PiperFrame::new_standard(0x166, &[0x02]),
            PiperFrame::new_standard(0x167, &[0x03]),
        ];
        let piper_clone = Arc::clone(&piper);
        let result_handle = std::thread::spawn(move || {
            piper_clone.send_realtime_package_confirmed(frames, Duration::from_millis(500))
        });

        started_rx
            .recv_timeout(Duration::from_millis(200))
            .expect("first realtime frame should begin sending");
        fatal_trigger.store(true, std::sync::atomic::Ordering::Release);

        wait_until(
            Duration::from_millis(200),
            || {
                piper.health().fault == Some(RuntimeFaultKind::TransportError)
                    && piper.normal_send_gate.state() == NormalSendGateState::FaultClosed
            },
            "RX fatal path should latch a transport fault and close the normal send gate",
        );
        let _ = release_tx.send(());

        let error = result_handle
            .join()
            .expect("confirmed send thread should finish")
            .expect_err("RX fatal should abort the remaining realtime package frames");
        assert!(matches!(
            error,
            DriverError::RealtimeDeliveryAbortedByFault { sent: 1, total: 3 }
        ));

        let sent = sent_frames.lock().expect("sent frames lock");
        assert_eq!(
            sent.as_slice(),
            &[frames[0]],
            "only the in-flight frame may complete after the RX fatal closes the gate",
        );
        assert_eq!(
            piper.maintenance_gate.current_state(),
            MaintenanceGateState::DeniedFaulted
        );
    }

    #[test]
    fn test_maintenance_send_point_rejects_stale_lease_after_revocation() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let piper = Arc::new(
            Piper::new_dual_thread_parts_unvalidated(
                MockRxAdapter,
                BlockingFirstSendTxAdapter {
                    sent_frames: sent_frames.clone(),
                    started_tx,
                    release_rx,
                    sends: 0,
                },
                Some(maintenance_ready_config()),
            )
            .unwrap(),
        );

        let session_key = 77;
        mark_maintenance_standby_confirmed(&piper);
        let lease_epoch = match piper
            .acquire_maintenance_lease_gate(7, session_key, Duration::from_millis(10))
            .expect("maintenance acquire should reach TX control lane")
        {
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
                .expect("maintenance revoke should sync to TX control lane")
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
    fn test_maintenance_send_timeout_never_later_sends_expired_frame() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let piper = Arc::new(
            Piper::new_dual_thread_parts_unvalidated(
                MockRxAdapter,
                BlockingFirstSendTxAdapter {
                    sent_frames: sent_frames.clone(),
                    started_tx,
                    release_rx,
                    sends: 0,
                },
                Some(maintenance_ready_config()),
            )
            .unwrap(),
        );

        let session_key = 88;
        mark_maintenance_standby_confirmed(&piper);
        let lease_epoch = match piper
            .acquire_maintenance_lease_gate(8, session_key, Duration::from_millis(10))
            .expect("maintenance acquire should reach TX control lane")
        {
            MaintenanceLeaseAcquireResult::Granted { lease_epoch } => lease_epoch,
            other => panic!("unexpected maintenance acquire result: {other:?}"),
        };

        let blocking_frame = PiperFrame::new_standard(0x211, &[0x01]);
        let piper_blocking = Arc::clone(&piper);
        let blocking_handle = std::thread::spawn(move || {
            piper_blocking.send_maintenance_frame_confirmed(
                8,
                session_key,
                lease_epoch,
                blocking_frame,
                Duration::from_millis(500),
            )
        });

        started_rx
            .recv_timeout(Duration::from_millis(200))
            .expect("first maintenance frame should begin sending");

        let expired_frame = PiperFrame::new_standard(0x212, &[0x02]);
        let piper_expired = Arc::clone(&piper);
        let expired_handle = std::thread::spawn(move || {
            piper_expired.send_maintenance_frame_confirmed(
                8,
                session_key,
                lease_epoch,
                expired_frame,
                Duration::from_millis(10),
            )
        });

        std::thread::sleep(Duration::from_millis(30));
        let _ = release_tx.send(());

        blocking_handle
            .join()
            .expect("blocking maintenance sender should finish")
            .expect("first frame should complete after it entered commit point");

        let expired_error = expired_handle
            .join()
            .expect("expired maintenance sender should finish")
            .expect_err("deadline-expired maintenance send must not commit later");
        assert!(matches!(expired_error, DriverError::Timeout));

        let sent = sent_frames.lock().expect("sent frames lock");
        assert_eq!(sent.as_slice(), &[blocking_frame]);
    }

    #[test]
    fn test_maintenance_send_returns_real_result_after_commit_point_crosses_deadline() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let piper = Arc::new(
            Piper::new_dual_thread_parts_unvalidated(
                MockRxAdapter,
                BlockingFirstSendTxAdapter {
                    sent_frames: sent_frames.clone(),
                    started_tx,
                    release_rx,
                    sends: 0,
                },
                Some(maintenance_ready_config()),
            )
            .unwrap(),
        );

        let session_key = 98;
        mark_maintenance_standby_confirmed(&piper);
        let lease_epoch = match piper
            .acquire_maintenance_lease_gate(9, session_key, Duration::from_millis(10))
            .expect("maintenance acquire should reach TX control lane")
        {
            MaintenanceLeaseAcquireResult::Granted { lease_epoch } => lease_epoch,
            other => panic!("unexpected maintenance acquire result: {other:?}"),
        };

        let frame = PiperFrame::new_standard(0x213, &[0x03]);
        let piper_send = Arc::clone(&piper);
        let send_handle = std::thread::spawn(move || {
            piper_send.send_maintenance_frame_confirmed(
                9,
                session_key,
                lease_epoch,
                frame,
                Duration::from_millis(20),
            )
        });

        started_rx
            .recv_timeout(Duration::from_millis(200))
            .expect("maintenance frame should enter tx.send_control before deadline");

        std::thread::sleep(Duration::from_millis(40));
        let _ = release_tx.send(());

        send_handle
            .join()
            .expect("maintenance sender should finish after delayed adapter release")
            .expect("send that crossed commit point before deadline must return real result");

        let sent = sent_frames.lock().expect("sent frames lock");
        assert_eq!(sent.as_slice(), &[frame]);
    }

    #[test]
    fn test_maintenance_acquire_timeout_rolls_back_tentative_grant() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let piper = Arc::new(
            Piper::new_dual_thread_parts_unvalidated(
                MockRxAdapter,
                BlockingFirstSendTxAdapter {
                    sent_frames,
                    started_tx,
                    release_rx,
                    sends: 0,
                },
                Some(maintenance_ready_config()),
            )
            .unwrap(),
        );

        let holder_session_key = 77;
        mark_maintenance_standby_confirmed(&piper);
        let holder_lease_epoch = match piper
            .acquire_maintenance_lease_gate(7, holder_session_key, Duration::from_millis(10))
            .expect("initial maintenance acquire should reach TX control lane")
        {
            MaintenanceLeaseAcquireResult::Granted { lease_epoch } => lease_epoch,
            other => panic!("unexpected initial acquire result: {other:?}"),
        };

        let blocking_frame = PiperFrame::new_standard(0x221, &[0x01]);
        let piper_blocking = Arc::clone(&piper);
        let blocking_handle = std::thread::spawn(move || {
            piper_blocking.send_maintenance_frame_confirmed(
                7,
                holder_session_key,
                holder_lease_epoch,
                blocking_frame,
                Duration::from_millis(500),
            )
        });

        started_rx
            .recv_timeout(Duration::from_millis(200))
            .expect("first maintenance frame should begin sending");

        assert!(
            piper
                .release_maintenance_lease_gate_if_holder(holder_session_key)
                .expect("holder release should enqueue tx-lane update"),
            "current lease holder should release successfully"
        );

        let piper_waiter = Arc::clone(&piper);
        let acquire_started = Instant::now();
        let waiter = std::thread::spawn(move || {
            piper_waiter.acquire_maintenance_lease_gate(8, 88, Duration::from_millis(20))
        });

        let acquire_error = waiter
            .join()
            .expect("waiting maintenance acquire should finish")
            .expect_err("grant ack that misses deadline must time out");
        assert!(matches!(acquire_error, DriverError::Timeout));
        assert!(
            acquire_started.elapsed() < Duration::from_millis(150),
            "maintenance acquire should remain bounded by caller timeout"
        );

        let snapshot = piper.maintenance_lease_snapshot();
        assert_eq!(snapshot.holder_session_id(), None);
        assert_eq!(snapshot.holder_session_key(), None);

        let _ = release_tx.send(());
        blocking_handle
            .join()
            .expect("blocking maintenance sender should finish")
            .expect("in-flight frame should complete after release");

        let reacquired_epoch = match piper
            .acquire_maintenance_lease_gate(9, 99, Duration::from_millis(100))
            .expect("fresh maintenance acquire should reach TX control lane")
        {
            MaintenanceLeaseAcquireResult::Granted { lease_epoch } => lease_epoch,
            other => panic!("unexpected reacquire result after timeout rollback: {other:?}"),
        };
        assert!(
            reacquired_epoch > holder_lease_epoch,
            "rollback should bump lease epoch before the next successful grant"
        );
    }

    #[test]
    fn test_maintenance_same_holder_reacquire_is_idempotent_after_grant_applied() {
        let gate = MaintenanceGate::default();
        let ops = Arc::new(Mutex::new(Vec::new()));
        attach_recording_maintenance_control_sink(&gate, ops.clone());
        gate.set_state(MaintenanceGateState::AllowedStandby);

        let first_epoch = match gate
            .acquire_blocking(3, 33, Duration::from_millis(20))
            .expect("initial same-holder acquire should sync to control lane")
        {
            MaintenanceLeaseAcquireResult::Granted { lease_epoch } => lease_epoch,
            other => panic!("unexpected first same-holder acquire result: {other:?}"),
        };

        let second_epoch = match gate
            .acquire_blocking(3, 33, Duration::from_millis(20))
            .expect("duplicate same-holder acquire should succeed idempotently")
        {
            MaintenanceLeaseAcquireResult::Granted { lease_epoch } => lease_epoch,
            other => panic!("unexpected duplicate same-holder acquire result: {other:?}"),
        };

        assert_eq!(
            second_epoch, first_epoch,
            "duplicate same-holder acquire should reuse the existing lease epoch"
        );

        let snapshot = gate.snapshot();
        assert_eq!(snapshot.holder_session_id(), Some(3));
        assert_eq!(snapshot.holder_session_key(), Some(33));
        assert_eq!(snapshot.lease_epoch(), first_epoch);

        let ops = ops.lock().expect("maintenance ops lock");
        assert_eq!(
            ops.iter().filter(|op| matches!(op, MaintenanceControlOp::Grant { .. })).count(),
            1,
            "duplicate same-holder acquire must not enqueue a second grant"
        );
        assert!(
            ops.iter().all(|op| !matches!(op, MaintenanceControlOp::Release { .. })),
            "idempotent same-holder reacquire must not revoke the held lease"
        );
    }

    #[test]
    fn test_maintenance_same_holder_timeout_does_not_revoke_inflight_grant() {
        let gate = Arc::new(MaintenanceGate::default());
        let ops = Arc::new(Mutex::new(Vec::new()));
        let (first_grant_started_tx, first_grant_started_rx) = mpsc::channel();
        let (first_grant_release_tx, first_grant_release_rx) = mpsc::channel();
        attach_delayed_first_grant_control_sink(
            &gate,
            ops.clone(),
            first_grant_started_tx,
            first_grant_release_rx,
        );
        gate.set_state(MaintenanceGateState::AllowedStandby);

        let first_gate = Arc::clone(&gate);
        let first_acquire = std::thread::spawn(move || {
            first_gate.acquire_blocking(4, 44, Duration::from_millis(500))
        });

        first_grant_started_rx
            .recv_timeout(Duration::from_millis(200))
            .expect("first grant should reach control lane before duplicate acquire");

        let second_gate = Arc::clone(&gate);
        let duplicate_started = Instant::now();
        let duplicate_acquire = std::thread::spawn(move || {
            second_gate.acquire_blocking(4, 44, Duration::from_millis(20))
        });

        let duplicate_error = duplicate_acquire
            .join()
            .expect("duplicate same-holder acquire should finish")
            .expect_err("in-flight same-holder reacquire should time out without revoking");
        assert!(matches!(duplicate_error, DriverError::Timeout));
        assert!(
            duplicate_started.elapsed() < Duration::from_millis(150),
            "duplicate same-holder reacquire should remain bounded by caller timeout"
        );

        let snapshot = gate.snapshot();
        assert_eq!(snapshot.holder_session_id(), None);
        assert_eq!(snapshot.holder_session_key(), None);
        assert_eq!(
            snapshot.lease_epoch(),
            0,
            "tentative grant must not publish a lease epoch before control-lane ack"
        );

        {
            let ops = ops.lock().expect("maintenance ops lock");
            assert_eq!(
                ops.iter().filter(|op| matches!(op, MaintenanceControlOp::Grant { .. })).count(),
                1,
                "duplicate same-holder timeout must not enqueue a second grant"
            );
            assert!(
                ops.iter().all(|op| !matches!(op, MaintenanceControlOp::Release { .. })),
                "duplicate same-holder timeout must not revoke the in-flight lease"
            );
        }

        let _ = first_grant_release_tx.send(());
        let first_epoch = match first_acquire
            .join()
            .expect("first same-holder acquire should finish")
            .expect("initial grant should still complete after delayed ack")
        {
            MaintenanceLeaseAcquireResult::Granted { lease_epoch } => lease_epoch,
            other => panic!("unexpected delayed first acquire result: {other:?}"),
        };

        let reacquired_epoch = match gate
            .acquire_blocking(4, 44, Duration::from_millis(20))
            .expect("same-holder acquire should succeed once the original grant is applied")
        {
            MaintenanceLeaseAcquireResult::Granted { lease_epoch } => lease_epoch,
            other => panic!("unexpected post-ack same-holder acquire result: {other:?}"),
        };
        assert_eq!(
            reacquired_epoch, first_epoch,
            "same-holder reacquire after delayed ack should observe the original lease epoch"
        );
    }

    #[test]
    fn test_maintenance_gate_waiter_wakes_immediately_after_holder_releases() {
        let gate = Arc::new(MaintenanceGate::default());
        attach_test_maintenance_control_sink(&gate);
        gate.set_state(MaintenanceGateState::AllowedStandby);
        match gate
            .acquire_blocking(1, 11, Duration::from_millis(10))
            .expect("initial acquire should sync to control lane")
        {
            MaintenanceLeaseAcquireResult::Granted { .. } => {},
            other => panic!("initial maintenance lease grant failed: {other:?}"),
        }

        let waiter_gate = Arc::clone(&gate);
        let waiter = std::thread::spawn(move || {
            waiter_gate.acquire_blocking(2, 22, Duration::from_millis(250))
        });

        std::thread::sleep(Duration::from_millis(20));
        assert!(gate.release_if_holder(11).expect("release should sync to control lane"));

        match waiter
            .join()
            .expect("waiter thread should finish")
            .expect("waiter acquire should sync to control lane")
        {
            MaintenanceLeaseAcquireResult::Granted { .. } => {},
            other => panic!("waiting maintenance lease did not wake correctly: {other:?}"),
        }
    }

    #[test]
    fn test_maintenance_gate_emits_revocation_event_on_denied_state_transition() {
        let gate = MaintenanceGate::default();
        let (tx, rx) = crossbeam_channel::bounded(1);
        gate.set_event_sink(tx);
        attach_test_maintenance_control_sink(&gate);
        gate.set_state(MaintenanceGateState::AllowedStandby);
        match gate
            .acquire_blocking(7, 77, Duration::from_millis(10))
            .expect("initial acquire should sync to control lane")
        {
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
        let piper = Piper::new_dual_thread_parts_unvalidated(
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
        let piper = Piper::new_dual_thread_parts_unvalidated(
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
            Piper::new_dual_thread_parts_unvalidated(
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
    fn test_realtime_confirmed_timeout_never_late_sends_expired_command() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let piper = Arc::new(
            Piper::new_dual_thread_parts_unvalidated(
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
        let first = [PiperFrame::new_standard(0x155, &[0x01])];
        let expired = [PiperFrame::new_standard(0x156, &[0x02])];

        piper.send_realtime_package(first).expect("first realtime package should queue");
        started_rx
            .recv_timeout(Duration::from_millis(200))
            .expect("first realtime frame should start sending");

        let piper_clone = Arc::clone(&piper);
        let handle = std::thread::spawn(move || {
            piper_clone.send_realtime_package_confirmed(expired, Duration::from_millis(20))
        });

        let enqueue_deadline = Instant::now() + Duration::from_millis(200);
        loop {
            if piper.realtime_slot.lock().expect("realtime slot lock").is_some() {
                break;
            }
            assert!(
                Instant::now() < enqueue_deadline,
                "confirmed realtime command should remain pending while TX is blocked"
            );
            std::thread::yield_now();
        }

        let error = handle
            .join()
            .expect("confirmed realtime sender should finish")
            .expect_err("expired confirmed realtime command must time out before commit");
        assert!(matches!(error, DriverError::RealtimeDeliveryTimeout));

        let _ = release_tx.send(());
        std::thread::sleep(Duration::from_millis(20));

        let sent = sent_frames.lock().expect("sent frames lock");
        assert_eq!(sent.as_slice(), &first);
    }

    #[test]
    fn test_reliable_confirmed_timeout_never_late_sends_expired_command() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let piper = Arc::new(
            Piper::new_dual_thread_parts_unvalidated(
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
        let first = PiperFrame::new_standard(0x201, &[0x01]);
        let expired = [PiperFrame::new_standard(0x202, &[0x02])];

        piper.send_frame(first).expect("first reliable frame should queue");
        started_rx
            .recv_timeout(Duration::from_millis(200))
            .expect("first reliable frame should start sending");

        let piper_clone = Arc::clone(&piper);
        let handle = std::thread::spawn(move || {
            piper_clone.send_reliable_package_confirmed(expired, Duration::from_millis(20))
        });

        let error = handle
            .join()
            .expect("confirmed reliable sender should finish")
            .expect_err("expired confirmed reliable command must time out before commit");
        assert!(matches!(error, DriverError::Timeout));

        let _ = release_tx.send(());
        std::thread::sleep(Duration::from_millis(20));

        let sent = sent_frames.lock().expect("sent frames lock");
        assert_eq!(sent.as_slice(), &[first]);
    }

    #[test]
    fn test_realtime_confirmed_waits_past_timeout_after_commit() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let piper = Arc::new(
            Piper::new_dual_thread_parts_unvalidated(
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
        let frames = [PiperFrame::new_standard(0x155, &[0x01])];
        let result = Arc::new(Mutex::new(None));
        let result_clone = Arc::clone(&result);
        let piper_clone = Arc::clone(&piper);

        let handle = std::thread::spawn(move || {
            let send_result =
                piper_clone.send_realtime_package_confirmed(frames, Duration::from_millis(20));
            *result_clone.lock().expect("result lock") = Some(send_result);
        });

        started_rx
            .recv_timeout(Duration::from_millis(200))
            .expect("confirmed realtime send should reach tx.send_control");
        std::thread::sleep(Duration::from_millis(40));
        assert!(
            result.lock().expect("result lock").is_none(),
            "confirmed realtime send should keep waiting after commit even if timeout elapsed"
        );

        let _ = release_tx.send(());
        handle.join().expect("confirmed realtime sender should finish");

        let send_result = result
            .lock()
            .expect("result lock")
            .take()
            .expect("confirmed realtime result should be captured");
        send_result.expect("confirmed realtime send should succeed after delayed completion");

        let sent = sent_frames.lock().expect("sent frames lock");
        assert_eq!(sent.as_slice(), &frames);
    }

    #[test]
    fn test_reliable_confirmed_waits_past_timeout_after_commit() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let piper = Arc::new(
            Piper::new_dual_thread_parts_unvalidated(
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
        let frames = [PiperFrame::new_standard(0x201, &[0x01])];
        let result = Arc::new(Mutex::new(None));
        let result_clone = Arc::clone(&result);
        let piper_clone = Arc::clone(&piper);

        let handle = std::thread::spawn(move || {
            let send_result =
                piper_clone.send_reliable_package_confirmed(frames, Duration::from_millis(20));
            *result_clone.lock().expect("result lock") = Some(send_result);
        });

        started_rx
            .recv_timeout(Duration::from_millis(200))
            .expect("confirmed reliable send should reach tx.send_control");
        std::thread::sleep(Duration::from_millis(40));
        assert!(
            result.lock().expect("result lock").is_none(),
            "confirmed reliable send should keep waiting after commit even if timeout elapsed"
        );

        let _ = release_tx.send(());
        handle.join().expect("confirmed reliable sender should finish");

        let send_result = result
            .lock()
            .expect("result lock")
            .take()
            .expect("confirmed reliable result should be captured");
        send_result.expect("confirmed reliable send should succeed after delayed completion");

        let sent = sent_frames.lock().expect("sent frames lock");
        assert_eq!(sent.as_slice(), &frames);
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

        assert!(matches!(piper.get_end_pose(), Observation::Unavailable));
    }

    #[test]
    fn test_joint_driver_low_speed_clone() {
        let mock_can = MockCanAdapter;
        let piper = Piper::new_dual_thread(mock_can, None).unwrap();

        // 测试可以多次读取 observation 状态
        let driver1 = piper.get_joint_driver_low_speed();
        let driver2 = piper.get_joint_driver_low_speed();

        assert!(matches!(driver1, Observation::Unavailable));
        assert!(matches!(driver2, Observation::Unavailable));
    }

    #[test]
    fn test_joint_limit_config_read_lock() {
        let mock_can = MockCanAdapter;
        let piper = Piper::new_dual_thread(mock_can, None).unwrap();

        // 测试可以多次读取配置状态
        assert!(matches!(
            piper.get_joint_limit_config(),
            Observation::Unavailable
        ));
        assert!(matches!(
            piper.get_joint_limit_config(),
            Observation::Unavailable
        ));
    }
}
