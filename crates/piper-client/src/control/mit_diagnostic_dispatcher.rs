use std::io;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
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
    Enabled(Sender<MitDiagnosticEvent>),
    Disabled,
}

static GLOBAL_DISPATCHER: OnceLock<GlobalDispatcherState> = OnceLock::new();
static GLOBAL_INIT_ERROR: OnceLock<String> = OnceLock::new();
static GLOBAL_INIT_WARNING_EMITTED: AtomicBool = AtomicBool::new(false);

pub(crate) fn global_dispatcher()
-> core::result::Result<MitDiagnosticDispatcher, GlobalDispatcherInitError> {
    let state = GLOBAL_DISPATCHER.get_or_init(|| match MitDiagnosticDispatcher::build_global() {
        Ok(dispatcher) => GlobalDispatcherState::Enabled(
            dispatcher.sender.expect("enabled dispatcher must retain a sender"),
        ),
        Err(error) => {
            let _ = GLOBAL_INIT_ERROR.set(error.to_string());
            GlobalDispatcherState::Disabled
        },
    });

    match state {
        GlobalDispatcherState::Enabled(sender) => {
            Ok(MitDiagnosticDispatcher::from_sender(sender.clone()))
        },
        GlobalDispatcherState::Disabled => Err(GlobalDispatcherInitError {
            should_warn: !GLOBAL_INIT_WARNING_EMITTED.swap(true, Ordering::AcqRel),
            message: GLOBAL_INIT_ERROR
                .get()
                .cloned()
                .unwrap_or_else(|| "failed to initialize MIT diagnostic dispatcher".to_string()),
        }),
    }
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
            TrySendError::Disconnected(_) => MitDiagnosticDispatchError::Disconnected,
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
            SendTimeoutError::Disconnected(_) => MitDiagnosticDispatchError::Disconnected,
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
        spawn(Box::new(move || Self::logging_loop(receiver)))
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
    use super::{MitDiagnosticDispatcher, MitDiagnosticEvent};
    use crate::control::hot_path_diagnostics::RecoverySummary;
    use crossbeam_channel::bounded;
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
}
