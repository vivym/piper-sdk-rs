#[cfg(test)]
use std::cell::RefCell;
use std::io;
use std::panic::{self, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::Duration;

use crossbeam_channel::{Receiver, SendTimeoutError, Sender, TrySendError, bounded};
use tracing::{info, warn};

use super::hot_path_diagnostics::RecoverySummary;

const DIAGNOSTIC_QUEUE_CAPACITY: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MitDiagnosticEvent {
    SendFailureRecovery(RecoverySummary),
    OverrunRecovery(RecoverySummary),
    DroppedRecoveryEvents { count: u32 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MitDiagnosticDispatchError {
    Unavailable,
    Full,
    Timeout,
    Disconnected,
}

#[derive(Debug, Clone)]
pub(crate) struct MitDiagnosticDispatcher {
    sender: Option<Sender<MitDiagnosticEvent>>,
}

#[derive(Debug)]
pub(crate) struct GlobalDispatcherInitError {
    should_warn: bool,
    message: String,
}

impl GlobalDispatcherInitError {
    pub(crate) fn should_warn(&self) -> bool {
        self.should_warn
    }

    pub(crate) fn message(&self) -> &str {
        &self.message
    }
}

enum GlobalDispatcherState {
    Uninitialized,
    Enabled { sender: Sender<MitDiagnosticEvent> },
    Disabled { generation: u64, reason: String },
    Rebuilding { generation: u64 },
}

static GLOBAL_DISPATCHER: OnceLock<Mutex<GlobalDispatcherState>> = OnceLock::new();
static GLOBAL_INIT_WARNING_EMITTED: AtomicBool = AtomicBool::new(false);
static GLOBAL_RUNTIME_WARNING_GENERATION: AtomicU64 = AtomicU64::new(0);
static GLOBAL_DISPATCHER_GENERATION: AtomicU64 = AtomicU64::new(1);

#[cfg(test)]
type TestBuilder = Box<dyn Fn() -> io::Result<MitDiagnosticDispatcher> + Send + Sync>;

#[cfg(test)]
thread_local! {
    static TEST_GLOBAL_BUILDER: RefCell<Option<TestBuilder>> = RefCell::new(None);
    static TEST_DIRECT_WARNING_SINK: RefCell<Option<Sender<String>>> = const { RefCell::new(None) };
}

#[cfg(test)]
static TEST_GLOBAL_GUARD: OnceLock<Mutex<()>> = OnceLock::new();

fn global_dispatcher_state() -> &'static Mutex<GlobalDispatcherState> {
    GLOBAL_DISPATCHER.get_or_init(|| Mutex::new(GlobalDispatcherState::Uninitialized))
}

pub(crate) fn global_dispatcher()
-> core::result::Result<MitDiagnosticDispatcher, GlobalDispatcherInitError> {
    #[cfg(test)]
    if let Some(dispatcher) = current_thread_test_dispatcher() {
        return dispatcher;
    }

    if let Some(dispatcher) = current_enabled_dispatcher() {
        return Ok(dispatcher);
    }

    let previous_generation = {
        let mut state =
            global_dispatcher_state().lock().expect("MIT diagnostic dispatcher state lock");
        match &*state {
            GlobalDispatcherState::Enabled { sender, .. } => {
                return Ok(MitDiagnosticDispatcher::from_sender(sender.clone()));
            },
            GlobalDispatcherState::Uninitialized => {
                *state = GlobalDispatcherState::Rebuilding { generation: 0 };
                0
            },
            GlobalDispatcherState::Disabled { generation, reason } => {
                let generation = *generation;
                let _ = reason.as_str();
                *state = GlobalDispatcherState::Rebuilding { generation };
                generation
            },
            GlobalDispatcherState::Rebuilding { generation } => {
                let generation = *generation;
                *state = GlobalDispatcherState::Rebuilding { generation };
                generation
            },
        }
    };

    match build_global_dispatcher_instance() {
        Ok(dispatcher) => {
            let sender = dispatcher
                .sender
                .as_ref()
                .expect("enabled dispatcher must retain a sender")
                .clone();
            let _ = next_dispatcher_generation();
            let mut state =
                global_dispatcher_state().lock().expect("MIT diagnostic dispatcher state lock");
            *state = GlobalDispatcherState::Enabled { sender };
            Ok(dispatcher)
        },
        Err(error) => {
            let generation = if previous_generation == 0 {
                next_dispatcher_generation()
            } else {
                previous_generation
            };
            let message = error.to_string();
            let mut state =
                global_dispatcher_state().lock().expect("MIT diagnostic dispatcher state lock");
            *state = GlobalDispatcherState::Disabled {
                generation,
                reason: message.clone(),
            };
            Err(GlobalDispatcherInitError {
                should_warn: !GLOBAL_INIT_WARNING_EMITTED.swap(true, Ordering::AcqRel),
                message,
            })
        },
    }
}

fn current_enabled_dispatcher() -> Option<MitDiagnosticDispatcher> {
    let state = global_dispatcher_state().lock().expect("MIT diagnostic dispatcher state lock");
    match &*state {
        GlobalDispatcherState::Enabled { sender, .. } => {
            Some(MitDiagnosticDispatcher::from_sender(sender.clone()))
        },
        _ => None,
    }
}

fn next_dispatcher_generation() -> u64 {
    GLOBAL_DISPATCHER_GENERATION.fetch_add(1, Ordering::AcqRel)
}

fn build_global_dispatcher_instance() -> io::Result<MitDiagnosticDispatcher> {
    #[cfg(test)]
    {
        if let Some(result) = current_thread_test_builder_result() {
            return result;
        }
    }

    MitDiagnosticDispatcher::build_global()
}

#[cfg(test)]
fn current_thread_test_builder_result() -> Option<io::Result<MitDiagnosticDispatcher>> {
    TEST_GLOBAL_BUILDER.with(|slot| slot.borrow().as_ref().map(|builder| builder()))
}

#[cfg(test)]
fn has_current_thread_test_builder() -> bool {
    TEST_GLOBAL_BUILDER.with(|slot| slot.borrow().is_some())
}

#[cfg(test)]
fn current_thread_test_dispatcher()
-> Option<core::result::Result<MitDiagnosticDispatcher, GlobalDispatcherInitError>> {
    current_thread_test_builder_result().map(|result| {
        result.map_err(|error| GlobalDispatcherInitError {
            should_warn: !GLOBAL_INIT_WARNING_EMITTED.swap(true, Ordering::AcqRel),
            message: error.to_string(),
        })
    })
}

fn mark_global_dispatcher_disconnected(reason: impl Into<String>) {
    let reason = reason.into();

    #[cfg(test)]
    if has_current_thread_test_builder() {
        let generation = next_dispatcher_generation();
        *global_dispatcher_state().lock().expect("MIT diagnostic dispatcher state lock") =
            GlobalDispatcherState::Disabled {
                generation,
                reason: reason.clone(),
            };
        emit_runtime_disconnect_warning_once(generation, &reason);
        return;
    }

    let generation = {
        let mut state =
            global_dispatcher_state().lock().expect("MIT diagnostic dispatcher state lock");
        match &*state {
            GlobalDispatcherState::Disabled { generation, .. }
            | GlobalDispatcherState::Rebuilding { generation } => *generation,
            GlobalDispatcherState::Enabled { .. } | GlobalDispatcherState::Uninitialized => {
                let generation = next_dispatcher_generation();
                *state = GlobalDispatcherState::Disabled {
                    generation,
                    reason: reason.clone(),
                };
                generation
            },
        }
    };

    emit_runtime_disconnect_warning_once(generation, &reason);
}

fn emit_runtime_disconnect_warning_once(generation: u64, reason: &str) {
    let previous = GLOBAL_RUNTIME_WARNING_GENERATION.load(Ordering::Acquire);
    if previous == generation {
        return;
    }
    if GLOBAL_RUNTIME_WARNING_GENERATION
        .compare_exchange(previous, generation, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
    {
        emit_direct_warning(&format!(
            "MIT async diagnostic logger disconnected or crashed; recovery summaries may be dropped until the dispatcher rebuilds: {reason}"
        ));
    }
}

fn emit_direct_warning(message: &str) {
    #[cfg(test)]
    {
        if TEST_DIRECT_WARNING_SINK.with(|slot| {
            if let Some(sender) = slot.borrow().as_ref() {
                let _ = sender.send(message.to_string());
                true
            } else {
                false
            }
        }) {
            return;
        }
    }

    warn!("{message}");
}

#[cfg(test)]
pub(crate) fn reset_global_dispatcher_for_test() {
    *global_dispatcher_state().lock().expect("MIT diagnostic dispatcher state lock") =
        GlobalDispatcherState::Uninitialized;
    GLOBAL_INIT_WARNING_EMITTED.store(false, Ordering::Release);
    GLOBAL_RUNTIME_WARNING_GENERATION.store(0, Ordering::Release);
    GLOBAL_DISPATCHER_GENERATION.store(1, Ordering::Release);
    TEST_GLOBAL_BUILDER.with(|slot| *slot.borrow_mut() = None);
    TEST_DIRECT_WARNING_SINK.with(|slot| *slot.borrow_mut() = None);
}

#[cfg(test)]
pub(crate) fn set_global_builder_for_test<F>(builder: F)
where
    F: Fn() -> io::Result<MitDiagnosticDispatcher> + Send + Sync + 'static,
{
    TEST_GLOBAL_BUILDER.with(|slot| *slot.borrow_mut() = Some(Box::new(builder)));
}

#[cfg(test)]
pub(crate) fn set_direct_warning_sink_for_test(sender: Option<Sender<String>>) {
    TEST_DIRECT_WARNING_SINK.with(|slot| *slot.borrow_mut() = sender);
}

#[cfg(test)]
pub(crate) fn global_test_guard() -> std::sync::MutexGuard<'static, ()> {
    TEST_GLOBAL_GUARD
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl MitDiagnosticDispatcher {
    pub(crate) fn disabled() -> Self {
        Self { sender: None }
    }

    pub(crate) fn submit_runtime_event(
        &self,
        event: MitDiagnosticEvent,
    ) -> core::result::Result<(), MitDiagnosticDispatchError> {
        let Some(sender) = &self.sender else {
            return Err(MitDiagnosticDispatchError::Unavailable);
        };

        sender.try_send(event).map_err(|error| match error {
            TrySendError::Full(_) => MitDiagnosticDispatchError::Full,
            TrySendError::Disconnected(_) => {
                mark_global_dispatcher_disconnected(
                    "runtime event path lost the MIT diagnostic receiver",
                );
                MitDiagnosticDispatchError::Disconnected
            },
        })
    }

    pub(crate) fn submit_cold_path_event(
        &self,
        event: MitDiagnosticEvent,
        timeout: Duration,
    ) -> core::result::Result<(), MitDiagnosticDispatchError> {
        let Some(sender) = &self.sender else {
            return Err(MitDiagnosticDispatchError::Unavailable);
        };

        sender.send_timeout(event, timeout).map_err(|error| match error {
            SendTimeoutError::Timeout(_) => MitDiagnosticDispatchError::Timeout,
            SendTimeoutError::Disconnected(_) => {
                mark_global_dispatcher_disconnected(
                    "cold-path flush lost the MIT diagnostic receiver",
                );
                MitDiagnosticDispatchError::Disconnected
            },
        })
    }

    #[cfg(test)]
    pub(crate) fn for_test(sender: Sender<MitDiagnosticEvent>) -> Self {
        Self::from_sender(sender)
    }

    #[cfg(test)]
    pub(crate) fn disabled_for_test() -> Self {
        Self::disabled()
    }

    #[cfg(test)]
    pub(crate) fn build_for_test_with_spawn<F>(spawn: F) -> io::Result<Self>
    where
        F: FnOnce(Box<dyn FnOnce() + Send>) -> io::Result<()>,
    {
        Self::build_with_spawn(spawn)
    }

    fn from_sender(sender: Sender<MitDiagnosticEvent>) -> Self {
        Self {
            sender: Some(sender),
        }
    }

    fn build_global() -> io::Result<Self> {
        Self::build_with_spawn(|runner| {
            thread::Builder::new().name("mit_diag_logger".into()).spawn(runner).map(|_| ())
        })
    }

    fn build_with_spawn<F>(spawn: F) -> io::Result<Self>
    where
        F: FnOnce(Box<dyn FnOnce() + Send>) -> io::Result<()>,
    {
        let (sender, receiver) = bounded(DIAGNOSTIC_QUEUE_CAPACITY);
        Self::spawn_logger_thread(receiver, spawn)?;
        Ok(Self::from_sender(sender))
    }

    fn spawn_logger_thread<F>(receiver: Receiver<MitDiagnosticEvent>, spawn: F) -> io::Result<()>
    where
        F: FnOnce(Box<dyn FnOnce() + Send>) -> io::Result<()>,
    {
        spawn(Box::new(move || {
            let result = panic::catch_unwind(AssertUnwindSafe(|| Self::logging_loop(receiver)));
            if result.is_err() {
                mark_global_dispatcher_disconnected("MIT async diagnostic logger thread panicked");
            }
        }))
    }

    fn logging_loop(receiver: Receiver<MitDiagnosticEvent>) {
        while let Ok(event) = receiver.recv() {
            match event {
                MitDiagnosticEvent::SendFailureRecovery(summary) => info!(
                    "MIT control send path recovered {} time(s). Warning limiter suppressed {} repeated transient-failure warning(s) since the last summary.",
                    summary.recovery_count, summary.suppressed_fault_warnings,
                ),
                MitDiagnosticEvent::OverrunRecovery(summary) => info!(
                    "MIT control loop returned within budget {} time(s). Warning limiter suppressed {} repeated overrun warning(s) since the last summary.",
                    summary.recovery_count, summary.suppressed_fault_warnings,
                ),
                MitDiagnosticEvent::DroppedRecoveryEvents { count } => warn!(
                    "Dropped {} MIT recovery diagnostic event(s) before the async logger could accept them.",
                    count,
                ),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{MitDiagnosticDispatcher, MitDiagnosticEvent, global_dispatcher};
    use crate::control::hot_path_diagnostics::RecoverySummary;
    use crossbeam_channel::bounded;
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    #[test]
    fn runtime_submission_enqueues_structured_events_without_blocking() {
        let (tx, rx) = bounded(1);
        let dispatcher = MitDiagnosticDispatcher::for_test(tx);

        dispatcher
            .submit_runtime_event(MitDiagnosticEvent::SendFailureRecovery(RecoverySummary {
                recovery_count: 1,
                suppressed_fault_warnings: 2,
            }))
            .expect("runtime submit should succeed");

        assert_eq!(
            rx.recv().expect("receiver should observe the structured event"),
            MitDiagnosticEvent::SendFailureRecovery(RecoverySummary {
                recovery_count: 1,
                suppressed_fault_warnings: 2,
            })
        );
    }

    #[test]
    fn runtime_submission_reports_queue_full_without_blocking() {
        let (tx, _rx) = bounded(1);
        let dispatcher = MitDiagnosticDispatcher::for_test(tx.clone());
        tx.try_send(MitDiagnosticEvent::DroppedRecoveryEvents { count: 9 })
            .expect("queue should be pre-filled");

        assert!(
            dispatcher
                .submit_runtime_event(MitDiagnosticEvent::OverrunRecovery(RecoverySummary {
                    recovery_count: 1,
                    suppressed_fault_warnings: 0,
                }))
                .is_err(),
            "runtime submit must fail fast when the bounded queue is full",
        );
    }

    #[test]
    fn cold_path_submission_uses_bounded_wait_instead_of_blocking_forever() {
        let (tx, _rx) = bounded::<MitDiagnosticEvent>(0);
        let dispatcher = MitDiagnosticDispatcher::for_test(tx);

        assert!(
            dispatcher
                .submit_cold_path_event(
                    MitDiagnosticEvent::DroppedRecoveryEvents { count: 1 },
                    Duration::from_millis(1),
                )
                .is_err(),
            "forced flush must time out instead of blocking indefinitely",
        );
    }

    #[test]
    fn dispatcher_construction_failure_is_reported_as_disabled() {
        let error = MitDiagnosticDispatcher::build_for_test_with_spawn(|_| {
            Err(std::io::Error::other("boom"))
        })
        .expect_err("failing spawn factory must disable async logging");
        assert!(error.to_string().contains("boom"));
    }

    #[test]
    fn global_dispatcher_rebuilds_after_runtime_disconnect() {
        let _guard = super::global_test_guard();
        super::reset_global_dispatcher_for_test();

        let (first_tx, first_rx) = bounded(4);
        let (second_tx, second_rx) = bounded(4);
        let attempts = Arc::new(Mutex::new(VecDeque::from([
            Ok(MitDiagnosticDispatcher::for_test(first_tx)),
            Ok(MitDiagnosticDispatcher::for_test(second_tx)),
        ])));
        super::set_global_builder_for_test({
            let attempts = Arc::clone(&attempts);
            move || {
                attempts
                    .lock()
                    .expect("test builder attempts")
                    .pop_front()
                    .expect("expected another builder attempt")
            }
        });

        let dispatcher = global_dispatcher().expect("initial dispatcher should build");
        drop(first_rx);
        assert_eq!(
            dispatcher.submit_runtime_event(MitDiagnosticEvent::DroppedRecoveryEvents { count: 1 }),
            Err(super::MitDiagnosticDispatchError::Disconnected),
            "disconnected sender must surface an explicit runtime disconnect",
        );

        let rebuilt = global_dispatcher().expect("next acquisition should lazily rebuild");
        rebuilt
            .submit_runtime_event(MitDiagnosticEvent::OverrunRecovery(RecoverySummary {
                recovery_count: 1,
                suppressed_fault_warnings: 0,
            }))
            .expect("rebuilt dispatcher should accept runtime events");
        assert_eq!(
            second_rx
                .recv_timeout(Duration::from_millis(50))
                .expect("rebuilt sink should receive runtime events"),
            MitDiagnosticEvent::OverrunRecovery(RecoverySummary {
                recovery_count: 1,
                suppressed_fault_warnings: 0,
            })
        );
    }

    #[test]
    fn runtime_disconnect_warning_is_emitted_once_per_failure_epoch() {
        let _guard = super::global_test_guard();
        super::reset_global_dispatcher_for_test();

        let (warning_tx, warning_rx) = bounded::<String>(8);
        super::set_direct_warning_sink_for_test(Some(warning_tx));

        let (first_tx, first_rx) = bounded(1);
        let (second_tx, second_rx) = bounded(1);
        let attempts = Arc::new(Mutex::new(VecDeque::from([
            Ok(MitDiagnosticDispatcher::for_test(first_tx)),
            Ok(MitDiagnosticDispatcher::for_test(second_tx)),
        ])));
        super::set_global_builder_for_test({
            let attempts = Arc::clone(&attempts);
            move || {
                attempts
                    .lock()
                    .expect("test builder attempts")
                    .pop_front()
                    .expect("expected another builder attempt")
            }
        });

        let first = global_dispatcher().expect("initial dispatcher should build");
        drop(first_rx);
        let _ = first.submit_runtime_event(MitDiagnosticEvent::DroppedRecoveryEvents { count: 1 });
        let first_warning = warning_rx
            .recv_timeout(Duration::from_millis(50))
            .expect("first disconnect must emit a one-shot direct warning");
        assert!(first_warning.contains("MIT async diagnostic"));
        assert!(
            warning_rx.try_recv().is_err(),
            "repeated submits in the same failure epoch must not spam warnings",
        );

        let rebuilt = global_dispatcher().expect("dispatcher should rebuild after disconnect");
        rebuilt
            .submit_runtime_event(MitDiagnosticEvent::DroppedRecoveryEvents { count: 2 })
            .expect("rebuilt dispatcher should accept events");
        assert_eq!(
            second_rx
                .recv_timeout(Duration::from_millis(50))
                .expect("rebuilt sink should receive events"),
            MitDiagnosticEvent::DroppedRecoveryEvents { count: 2 }
        );

        drop(second_rx);
        let _ =
            rebuilt.submit_runtime_event(MitDiagnosticEvent::DroppedRecoveryEvents { count: 3 });
        let second_warning = warning_rx
            .recv_timeout(Duration::from_millis(50))
            .expect("a later disconnect after successful rebuild should emit a new epoch warning");
        assert!(second_warning.contains("MIT async diagnostic"));

        super::set_direct_warning_sink_for_test(None);
    }
}
