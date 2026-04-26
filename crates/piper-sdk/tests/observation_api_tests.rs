use piper_sdk::can::{CanAdapter, CanError, PiperFrame, RealtimeTxAdapter, SplittableAdapter};
use piper_sdk::driver::observation::{Freshness, Observation, ObservationPayload};
use piper_sdk::driver::{DiagnosticEvent, Piper, ProtocolDiagnostic, QueryDiagnostic, QueryError};
use piper_sdk::protocol::ids::{
    ID_COLLISION_PROTECTION_LEVEL_FEEDBACK, ID_JOINT_FEEDBACK_12, ID_MOTOR_LIMIT_FEEDBACK,
};
use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

fn received(frame: PiperFrame) -> piper_sdk::can::ReceivedFrame {
    piper_sdk::can::ReceivedFrame::new(frame, piper_sdk::can::TimestampProvenance::None)
}

struct MockCanAdapter {
    receive_queue: Arc<Mutex<VecDeque<PiperFrame>>>,
    sent_frames: Arc<(Mutex<Vec<PiperFrame>>, Condvar)>,
}

impl MockCanAdapter {
    fn new() -> Self {
        Self {
            receive_queue: Arc::new(Mutex::new(VecDeque::new())),
            sent_frames: Arc::new((Mutex::new(Vec::new()), Condvar::new())),
        }
    }

    fn queue_frame(&self, frame: PiperFrame) {
        self.receive_queue.lock().unwrap().push_back(frame);
    }

    fn wait_for_sent_frame_count(&self, expected: usize, timeout: Duration) -> bool {
        let (lock, cvar) = &*self.sent_frames;
        let frames = lock.lock().unwrap();
        let result = cvar
            .wait_timeout_while(frames, timeout, |frames| frames.len() < expected)
            .unwrap();
        result.0.len() >= expected
    }
}

impl CanAdapter for MockCanAdapter {
    fn send(&mut self, frame: PiperFrame) -> Result<(), CanError> {
        let (lock, cvar) = &*self.sent_frames;
        lock.lock().unwrap().push(frame);
        cvar.notify_all();
        Ok(())
    }

    fn receive(&mut self) -> Result<piper_sdk::can::ReceivedFrame, CanError> {
        self.receive_queue
            .lock()
            .unwrap()
            .pop_front()
            .map(received)
            .ok_or(CanError::Timeout)
    }
}

struct MockRxAdapter {
    receive_queue: Arc<Mutex<VecDeque<PiperFrame>>>,
}

impl piper_sdk::can::RxAdapter for MockRxAdapter {
    fn receive(&mut self) -> Result<piper_sdk::can::ReceivedFrame, CanError> {
        self.receive_queue
            .lock()
            .unwrap()
            .pop_front()
            .map(received)
            .ok_or(CanError::Timeout)
    }
}

struct MockTxAdapter {
    sent_frames: Arc<(Mutex<Vec<PiperFrame>>, Condvar)>,
}

impl RealtimeTxAdapter for MockTxAdapter {
    fn send_control(&mut self, frame: PiperFrame, budget: Duration) -> Result<(), CanError> {
        if budget.is_zero() {
            return Err(CanError::Timeout);
        }

        let (lock, cvar) = &*self.sent_frames;
        lock.lock().unwrap().push(frame);
        cvar.notify_all();
        Ok(())
    }

    fn send_shutdown_until(
        &mut self,
        frame: PiperFrame,
        deadline: std::time::Instant,
    ) -> Result<(), CanError> {
        if deadline <= std::time::Instant::now() {
            return Err(CanError::Timeout);
        }

        let (lock, cvar) = &*self.sent_frames;
        lock.lock().unwrap().push(frame);
        cvar.notify_all();
        Ok(())
    }
}

impl SplittableAdapter for MockCanAdapter {
    type RxAdapter = MockRxAdapter;
    type TxAdapter = MockTxAdapter;

    fn split(self) -> Result<(Self::RxAdapter, Self::TxAdapter), CanError> {
        Ok((
            MockRxAdapter {
                receive_queue: Arc::clone(&self.receive_queue),
            },
            MockTxAdapter {
                sent_frames: Arc::clone(&self.sent_frames),
            },
        ))
    }
}

fn bootstrap_timestamp_frame() -> PiperFrame {
    let mut frame = PiperFrame::new_standard(ID_JOINT_FEEDBACK_12 as u16, &[0; 8]);
    frame.timestamp_us = 1;
    frame
}

fn build_test_piper(mock_can: &Arc<MockCanAdapter>) -> Piper {
    mock_can.queue_frame(bootstrap_timestamp_frame());
    let can_adapter = MockCanAdapter {
        receive_queue: Arc::clone(&mock_can.receive_queue),
        sent_frames: Arc::clone(&mock_can.sent_frames),
    };
    Piper::new_dual_thread(can_adapter, None).unwrap()
}

fn queue_complete_joint_limit_query_response(mock_can: &Arc<MockCanAdapter>) {
    for joint_index in 1..=6 {
        let mut data = [0u8; 8];
        data[0] = joint_index;
        data[1..3].copy_from_slice(&(1800i16 + i16::from(joint_index)).to_be_bytes());
        data[3..5].copy_from_slice(&(-1800i16 - i16::from(joint_index)).to_be_bytes());
        data[5..7].copy_from_slice(&(500u16 + u16::from(joint_index)).to_be_bytes());
        mock_can.queue_frame(PiperFrame::new_standard(
            ID_MOTOR_LIMIT_FEEDBACK as u16,
            &data,
        ));
    }
}

#[test]
fn busy_query_returns_query_error_busy() {
    let mock_can = Arc::new(MockCanAdapter::new());
    let piper = build_test_piper(&mock_can);

    std::thread::scope(|scope| {
        let query_handle =
            scope.spawn(|| piper.query_joint_limit_config(Duration::from_millis(150)));

        assert!(
            mock_can.wait_for_sent_frame_count(6, Duration::from_millis(100)),
            "joint-limit query should send all six request frames before timeout"
        );

        let err = piper.query_collision_protection(Duration::from_millis(20)).unwrap_err();
        assert!(matches!(err, QueryError::Busy));

        let first_query = query_handle.join().unwrap();
        assert!(matches!(first_query, Err(QueryError::Timeout)));
    });
}

#[test]
fn diagnostics_snapshot_and_subscription_expose_protocol_events() {
    let mock_can = Arc::new(MockCanAdapter::new());
    let piper = build_test_piper(&mock_can);
    let diagnostics_rx = piper.subscribe_diagnostics();

    mock_can.queue_frame(PiperFrame::new_standard(
        ID_COLLISION_PROTECTION_LEVEL_FEEDBACK as u16,
        &[255, 0, 0, 0, 0, 0, 0, 0],
    ));

    let event = diagnostics_rx
        .recv_timeout(Duration::from_millis(250))
        .expect("expected protocol diagnostic from subscription");
    assert!(matches!(
        event,
        DiagnosticEvent::Protocol(ProtocolDiagnostic::OutOfRange {
            field: "collision_protection_level",
            ..
        })
    ));

    std::thread::sleep(Duration::from_millis(50));

    assert!(piper.snapshot_diagnostics().iter().any(|event| matches!(
        event,
        DiagnosticEvent::Protocol(ProtocolDiagnostic::OutOfRange {
            field: "collision_protection_level",
            ..
        })
    )));
}

#[test]
fn busy_query_is_retained_in_diagnostics_snapshot() {
    let mock_can = Arc::new(MockCanAdapter::new());
    let piper = build_test_piper(&mock_can);

    std::thread::scope(|scope| {
        let query_handle =
            scope.spawn(|| piper.query_joint_limit_config(Duration::from_millis(150)));

        assert!(
            mock_can.wait_for_sent_frame_count(6, Duration::from_millis(100)),
            "joint-limit query should send all six request frames before timeout"
        );

        let err = piper.query_collision_protection(Duration::from_millis(20)).unwrap_err();
        assert!(matches!(err, QueryError::Busy));

        assert!(
            piper
                .snapshot_diagnostics()
                .iter()
                .any(|event| matches!(event, DiagnosticEvent::Query(QueryDiagnostic::Busy)))
        );

        let first_query = query_handle.join().unwrap();
        assert!(matches!(first_query, Err(QueryError::Timeout)));
    });
}

#[test]
fn query_timeout_does_not_invalidate_prior_complete_joint_limit_config() {
    let mock_can = Arc::new(MockCanAdapter::new());
    let piper = build_test_piper(&mock_can);

    std::thread::scope(|scope| {
        let query_handle =
            scope.spawn(|| piper.query_joint_limit_config(Duration::from_millis(250)));

        assert!(
            mock_can.wait_for_sent_frame_count(6, Duration::from_millis(100)),
            "joint-limit query should send all six request frames before receiving the response"
        );
        queue_complete_joint_limit_query_response(&mock_can);

        query_handle
            .join()
            .expect("query thread should not panic")
            .expect("first query should complete from the injected response");
    });

    let err = piper
        .query_joint_limit_config(Duration::from_millis(20))
        .expect_err("second query should time out without a new response");
    assert!(matches!(err, QueryError::Timeout));

    let observation = piper.get_joint_limit_config();
    assert!(matches!(
        observation,
        Observation::Available(available)
            if matches!(available.payload, ObservationPayload::Complete(_))
                && matches!(available.freshness, Freshness::Fresh)
    ));
}
