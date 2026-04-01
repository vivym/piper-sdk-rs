use crate::query_coordinator::QueryKind;
use crossbeam_channel::{Receiver, Sender, TrySendError, bounded};
use piper_protocol::ProtocolDiagnostic;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueryDiagnostic {
    Busy,
    UnexpectedFrameForActiveQuery { query: QueryKind, can_id: u32 },
    DiagnosticsOnlyTimeout { query: QueryKind },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiagnosticEvent {
    Protocol(ProtocolDiagnostic),
    Query(QueryDiagnostic),
}

#[derive(Debug, Clone)]
pub struct DiagnosticBuffer {
    inner: Arc<Mutex<DiagnosticBufferInner>>,
}

#[derive(Debug)]
struct DiagnosticBufferInner {
    capacity: usize,
    retained: VecDeque<DiagnosticEvent>,
    subscribers: Vec<Sender<DiagnosticEvent>>,
}

impl DiagnosticBuffer {
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(DiagnosticBufferInner {
                capacity,
                retained: VecDeque::new(),
                subscribers: Vec::new(),
            })),
        }
    }

    pub fn push(&self, event: DiagnosticEvent) {
        let mut inner = self.inner.lock().unwrap_or_else(|poison| poison.into_inner());

        if inner.capacity == 0 {
            inner.retained.clear();
        } else {
            while inner.retained.len() >= inner.capacity {
                inner.retained.pop_front();
            }
            inner.retained.push_back(event.clone());
        }

        inner.subscribers.retain(|subscriber| match subscriber.try_send(event.clone()) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) => true,
            Err(TrySendError::Disconnected(_)) => false,
        });
    }

    pub fn snapshot(&self) -> Vec<DiagnosticEvent> {
        self.inner
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .retained
            .iter()
            .cloned()
            .collect()
    }

    pub fn subscribe(&self) -> Receiver<DiagnosticEvent> {
        let capacity =
            self.inner.lock().unwrap_or_else(|poison| poison.into_inner()).capacity.max(1);
        let (tx, rx) = bounded(capacity);
        self.inner
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .subscribers
            .push(tx);
        rx
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn diagnostics_buffer_retains_recent_events() {
        let buffer = DiagnosticBuffer::new(2);
        let first = DiagnosticEvent::Query(QueryDiagnostic::Busy);
        let second = DiagnosticEvent::Query(QueryDiagnostic::DiagnosticsOnlyTimeout {
            query: QueryKind::JointLimit,
        });
        let third = DiagnosticEvent::Query(QueryDiagnostic::UnexpectedFrameForActiveQuery {
            query: QueryKind::CollisionProtection,
            can_id: 0x47B,
        });

        buffer.push(first);
        buffer.push(second.clone());
        buffer.push(third.clone());

        assert_eq!(buffer.snapshot(), vec![second, third]);
    }

    #[test]
    fn diagnostics_are_fanned_out_to_all_subscribers() {
        let buffer = DiagnosticBuffer::new(4);
        let rx_a = buffer.subscribe();
        let rx_b = buffer.subscribe();
        let event = DiagnosticEvent::Query(QueryDiagnostic::Busy);

        buffer.push(event.clone());

        assert_eq!(rx_a.recv_timeout(Duration::from_millis(10)).unwrap(), event);
        assert_eq!(rx_b.recv_timeout(Duration::from_millis(10)).unwrap(), event);
    }

    #[test]
    fn diagnostics_snapshot_is_retained_without_subscription_backfill() {
        let buffer = DiagnosticBuffer::new(2);
        let retained = DiagnosticEvent::Query(QueryDiagnostic::Busy);
        let live = DiagnosticEvent::Query(QueryDiagnostic::DiagnosticsOnlyTimeout {
            query: QueryKind::EndLimit,
        });

        buffer.push(retained.clone());
        let rx = buffer.subscribe();
        buffer.push(live.clone());

        assert_eq!(buffer.snapshot(), vec![retained, live.clone()]);
        assert_eq!(rx.recv_timeout(Duration::from_millis(10)).unwrap(), live);
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn diagnostics_buffer_accepts_protocol_layer_diagnostic() {
        let buffer = DiagnosticBuffer::new(1);
        buffer.push(DiagnosticEvent::Protocol(
            ProtocolDiagnostic::UnsupportedValue {
                field: "collision_protection_level",
                raw: 7,
            },
        ));

        assert!(matches!(
            buffer.snapshot().as_slice(),
            [DiagnosticEvent::Protocol(
                ProtocolDiagnostic::UnsupportedValue {
                    field: "collision_protection_level",
                    raw: 7,
                }
            )]
        ));
    }

    #[test]
    fn diagnostics_push_drops_events_for_full_slow_subscriber_without_blocking() {
        use std::sync::mpsc;
        use std::thread;

        let buffer = DiagnosticBuffer::new(2);
        let rx = buffer.subscribe();

        let first = DiagnosticEvent::Query(QueryDiagnostic::Busy);
        let second = DiagnosticEvent::Query(QueryDiagnostic::DiagnosticsOnlyTimeout {
            query: QueryKind::JointLimit,
        });
        let third = DiagnosticEvent::Query(QueryDiagnostic::UnexpectedFrameForActiveQuery {
            query: QueryKind::CollisionProtection,
            can_id: 0x47B,
        });

        buffer.push(first.clone());
        buffer.push(second.clone());

        let (done_tx, done_rx) = mpsc::channel();
        let cloned = buffer.clone();
        let third_for_thread = third.clone();
        thread::spawn(move || {
            cloned.push(third_for_thread);
            done_tx.send(()).unwrap();
        });

        done_rx
            .recv_timeout(Duration::from_millis(100))
            .expect("push should not block on a full subscriber queue");

        assert_eq!(buffer.snapshot(), vec![second.clone(), third]);
        assert_eq!(rx.recv_timeout(Duration::from_millis(10)).unwrap(), first);
        assert_eq!(rx.recv_timeout(Duration::from_millis(10)).unwrap(), second);
        assert!(rx.recv_timeout(Duration::from_millis(10)).is_err());
    }

    #[test]
    fn diagnostics_cleanup_discards_disconnected_subscribers() {
        let buffer = DiagnosticBuffer::new(1);
        let rx = buffer.subscribe();
        drop(rx);

        buffer.push(DiagnosticEvent::Query(QueryDiagnostic::Busy));
        buffer.push(DiagnosticEvent::Query(
            QueryDiagnostic::DiagnosticsOnlyTimeout {
                query: QueryKind::EndLimit,
            },
        ));

        assert_eq!(
            buffer.snapshot(),
            vec![DiagnosticEvent::Query(
                QueryDiagnostic::DiagnosticsOnlyTimeout {
                    query: QueryKind::EndLimit,
                }
            )]
        );
    }
}
