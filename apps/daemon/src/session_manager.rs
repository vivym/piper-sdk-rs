//! Bridge v2 session manager.

use crossbeam_channel::{Receiver, Sender, bounded};
use piper_can::PiperFrame;
use piper_can::gs_usb_bridge::protocol::{BridgeEvent, BridgeRole, CanIdFilter, SessionToken};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::time::Duration;

pub const OUTBOUND_QUEUE_CAPACITY: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeaseAcquireResult {
    Granted,
    Denied { holder_session_id: Option<u32> },
}

#[derive(Debug)]
pub enum ConnectionOutput {
    Event(BridgeEvent),
    CloseAfterEvent(BridgeEvent),
    Shutdown,
}

pub trait SessionControl: Send + Sync {
    fn shutdown(&self);
}

pub struct BridgeSession {
    session_id: u32,
    session_token: SessionToken,
    filters: RwLock<Vec<CanIdFilter>>,
    outbound_tx: Sender<ConnectionOutput>,
    pending_gap: AtomicU32,
    control: Arc<dyn SessionControl>,
}

impl BridgeSession {
    fn new(
        session_id: u32,
        session_token: SessionToken,
        filters: Vec<CanIdFilter>,
        outbound_tx: Sender<ConnectionOutput>,
        control: Arc<dyn SessionControl>,
    ) -> Self {
        Self {
            session_id,
            session_token,
            filters: RwLock::new(filters),
            outbound_tx,
            pending_gap: AtomicU32::new(0),
            control,
        }
    }

    pub fn session_id(&self) -> u32 {
        self.session_id
    }

    pub fn session_token(&self) -> SessionToken {
        self.session_token
    }

    pub fn set_filters(&self, filters: Vec<CanIdFilter>) {
        *self.filters.write().unwrap() = filters;
    }

    pub fn matches_filter(&self, can_id: u32) -> bool {
        let filters = self.filters.read().unwrap();
        if filters.is_empty() {
            return true;
        }
        filters.iter().any(|filter| filter.matches(can_id))
    }

    pub fn enqueue_frame(&self, frame: PiperFrame) -> bool {
        let dropped = self.pending_gap.swap(0, Ordering::AcqRel);
        if dropped > 0
            && self
                .outbound_tx
                .try_send(ConnectionOutput::Event(BridgeEvent::Gap { dropped }))
                .is_err()
        {
            self.pending_gap.fetch_add(dropped + 1, Ordering::AcqRel);
            return false;
        }

        match self
            .outbound_tx
            .try_send(ConnectionOutput::Event(BridgeEvent::ReceiveFrame(frame)))
        {
            Ok(()) => true,
            Err(_) => {
                self.pending_gap.fetch_add(1, Ordering::AcqRel);
                false
            },
        }
    }

    pub fn replace_and_close(&self) {
        if self
            .outbound_tx
            .send_timeout(
                ConnectionOutput::CloseAfterEvent(BridgeEvent::SessionReplaced),
                Duration::from_millis(50),
            )
            .is_err()
        {
            self.control.shutdown();
        }
    }

    pub fn shutdown(&self) {
        let _ = self
            .outbound_tx
            .send_timeout(ConnectionOutput::Shutdown, Duration::from_millis(50));
        self.control.shutdown();
    }
}

pub struct RegisterResult {
    pub session_id: u32,
    pub replaced: Option<Arc<BridgeSession>>,
}

pub struct PreparedSession {
    session_id: u32,
    session_token: SessionToken,
    role_granted: BridgeRole,
    filters: Vec<CanIdFilter>,
    outbound_tx: Sender<ConnectionOutput>,
    control: Arc<dyn SessionControl>,
}

impl PreparedSession {
    pub fn session_id(&self) -> u32 {
        self.session_id
    }

    pub fn role_granted(&self) -> BridgeRole {
        self.role_granted
    }
}

struct Registry {
    sessions: HashMap<u32, Arc<BridgeSession>>,
    token_to_session: HashMap<SessionToken, u32>,
}

pub struct SessionManager {
    registry: RwLock<Registry>,
    next_session_id: AtomicU32,
    writer_lease: Mutex<Option<u32>>,
    writer_lease_cv: Condvar,
}

impl SessionManager {
    pub fn new() -> Self {
        Self {
            registry: RwLock::new(Registry {
                sessions: HashMap::new(),
                token_to_session: HashMap::new(),
            }),
            next_session_id: AtomicU32::new(1),
            writer_lease: Mutex::new(None),
            writer_lease_cv: Condvar::new(),
        }
    }

    pub fn new_connection_queue() -> (Sender<ConnectionOutput>, Receiver<ConnectionOutput>) {
        bounded(OUTBOUND_QUEUE_CAPACITY)
    }

    fn next_session_id(&self) -> u32 {
        self.next_session_id
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                let next = current.wrapping_add(1).max(1);
                Some(next)
            })
            .expect("session id fetch_update should not fail")
    }

    pub fn prepare_session(
        &self,
        session_token: SessionToken,
        role_request: BridgeRole,
        filters: Vec<CanIdFilter>,
        outbound_tx: Sender<ConnectionOutput>,
        control: Arc<dyn SessionControl>,
    ) -> PreparedSession {
        PreparedSession {
            session_id: self.next_session_id(),
            session_token,
            role_granted: role_request,
            filters,
            outbound_tx,
            control,
        }
    }

    pub fn commit_prepared(&self, prepared: PreparedSession) -> RegisterResult {
        let mut registry = self.registry.write().unwrap();

        let replaced = registry
            .token_to_session
            .remove(&prepared.session_token)
            .and_then(|old_id| registry.sessions.remove(&old_id));

        let session = Arc::new(BridgeSession::new(
            prepared.session_id,
            prepared.session_token,
            prepared.filters,
            prepared.outbound_tx,
            prepared.control,
        ));
        registry.token_to_session.insert(prepared.session_token, prepared.session_id);
        registry.sessions.insert(prepared.session_id, Arc::clone(&session));
        drop(registry);

        if let Some(ref old_session) = replaced {
            let mut lease = self.writer_lease.lock().unwrap();
            if lease.as_ref().copied() == Some(old_session.session_id()) {
                *lease = Some(prepared.session_id);
                self.writer_lease_cv.notify_all();
            }
        }

        RegisterResult {
            session_id: prepared.session_id,
            replaced,
        }
    }

    pub fn unregister_session(&self, session_id: u32) -> Option<Arc<BridgeSession>> {
        let removed = {
            let mut registry = self.registry.write().unwrap();
            let removed = registry.sessions.remove(&session_id)?;
            if registry.token_to_session.get(&removed.session_token()).copied() == Some(session_id)
            {
                registry.token_to_session.remove(&removed.session_token());
            }
            Some(removed)
        };

        let mut lease = self.writer_lease.lock().unwrap();
        if lease.as_ref().copied() == Some(session_id) {
            *lease = None;
            self.writer_lease_cv.notify_all();
        }

        removed
    }

    pub fn session(&self, session_id: u32) -> Option<Arc<BridgeSession>> {
        self.registry.read().unwrap().sessions.get(&session_id).map(Arc::clone)
    }

    pub fn count(&self) -> u32 {
        self.registry.read().unwrap().sessions.len() as u32
    }

    pub fn broadcast_frame(&self, frame: PiperFrame) -> u64 {
        let registry = self.registry.read().unwrap();
        let mut dropped = 0u64;
        for session in registry.sessions.values() {
            if session.matches_filter(frame.id) && !session.enqueue_frame(frame) {
                dropped += 1;
            }
        }
        dropped
    }

    pub fn set_filters(&self, session_id: u32, filters: Vec<CanIdFilter>) -> bool {
        if let Some(session) = self.session(session_id) {
            session.set_filters(filters);
            true
        } else {
            false
        }
    }

    pub fn has_writer_lease(&self, session_id: u32) -> bool {
        self.writer_lease.lock().unwrap().as_ref().copied() == Some(session_id)
    }

    pub fn acquire_writer_lease(&self, session_id: u32, timeout: Duration) -> LeaseAcquireResult {
        let deadline = std::time::Instant::now() + timeout;
        let mut guard = self.writer_lease.lock().unwrap();
        loop {
            match *guard {
                None => {
                    *guard = Some(session_id);
                    return LeaseAcquireResult::Granted;
                },
                Some(owner) if owner == session_id => return LeaseAcquireResult::Granted,
                Some(owner) => {
                    let now = std::time::Instant::now();
                    if now >= deadline {
                        return LeaseAcquireResult::Denied {
                            holder_session_id: Some(owner),
                        };
                    }
                    let remaining = deadline.saturating_duration_since(now);
                    let (next_guard, wait_result) =
                        self.writer_lease_cv.wait_timeout(guard, remaining).unwrap();
                    guard = next_guard;
                    if wait_result.timed_out() {
                        return LeaseAcquireResult::Denied {
                            holder_session_id: guard.as_ref().copied(),
                        };
                    }
                },
            }
        }
    }

    pub fn release_writer_lease(&self, session_id: u32) -> bool {
        let mut guard = self.writer_lease.lock().unwrap();
        if guard.as_ref().copied() == Some(session_id) {
            *guard = None;
            self.writer_lease_cv.notify_all();
            return true;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use piper_can::gs_usb_bridge::protocol::SessionToken;

    struct NoopControl;

    impl SessionControl for NoopControl {
        fn shutdown(&self) {}
    }

    fn token(byte: u8) -> SessionToken {
        SessionToken::new([byte; 16])
    }

    #[test]
    fn same_token_replaces_old_session_and_transfers_lease() {
        let manager = SessionManager::new();
        let (tx_a, _rx_a) = SessionManager::new_connection_queue();
        let first = manager.commit_prepared(manager.prepare_session(
            token(1),
            BridgeRole::WriterCandidate,
            vec![],
            tx_a,
            Arc::new(NoopControl),
        ));
        assert_eq!(
            manager.acquire_writer_lease(first.session_id, Duration::from_millis(1)),
            LeaseAcquireResult::Granted
        );

        let (tx_b, _rx_b) = SessionManager::new_connection_queue();
        let second = manager.commit_prepared(manager.prepare_session(
            token(1),
            BridgeRole::WriterCandidate,
            vec![],
            tx_b,
            Arc::new(NoopControl),
        ));

        assert_eq!(
            second.replaced.as_ref().map(|s| s.session_id()),
            Some(first.session_id)
        );
        assert!(!manager.has_writer_lease(first.session_id));
        assert!(manager.has_writer_lease(second.session_id));
    }

    #[test]
    fn broadcast_respects_filters() {
        let manager = SessionManager::new();
        let (tx, rx) = SessionManager::new_connection_queue();
        let registered = manager.commit_prepared(manager.prepare_session(
            token(2),
            BridgeRole::Observer,
            vec![CanIdFilter::new(0x100, 0x1FF)],
            tx,
            Arc::new(NoopControl),
        ));
        let dropped = manager.broadcast_frame(PiperFrame::new_standard(0x123, &[1]));
        assert_eq!(dropped, 0);
        let event = rx.try_recv().unwrap();
        assert!(matches!(
            event,
            ConnectionOutput::Event(BridgeEvent::ReceiveFrame(_))
        ));

        let dropped = manager.broadcast_frame(PiperFrame::new_standard(0x300, &[1]));
        assert_eq!(dropped, 0);
        assert!(rx.try_recv().is_err());
        manager.unregister_session(registered.session_id);
    }

    #[test]
    fn acquire_writer_lease_denies_busy_holder() {
        let manager = SessionManager::new();
        let (tx_a, _rx_a) = SessionManager::new_connection_queue();
        let first = manager.commit_prepared(manager.prepare_session(
            token(3),
            BridgeRole::WriterCandidate,
            vec![],
            tx_a,
            Arc::new(NoopControl),
        ));
        let (tx_b, _rx_b) = SessionManager::new_connection_queue();
        let second = manager.commit_prepared(manager.prepare_session(
            token(4),
            BridgeRole::WriterCandidate,
            vec![],
            tx_b,
            Arc::new(NoopControl),
        ));

        assert_eq!(
            manager.acquire_writer_lease(first.session_id, Duration::from_millis(1)),
            LeaseAcquireResult::Granted
        );
        assert_eq!(
            manager.acquire_writer_lease(second.session_id, Duration::from_millis(5)),
            LeaseAcquireResult::Denied {
                holder_session_id: Some(first.session_id)
            }
        );
    }
}
