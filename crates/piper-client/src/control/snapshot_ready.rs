use crate::observer::ControlSnapshot;
use crate::types::RobotError;
use std::time::{Duration, Instant};

pub(crate) const CONTROL_SNAPSHOT_READY_TIMEOUT: Duration = Duration::from_millis(200);
pub(crate) const CONTROL_SNAPSHOT_POLL_INTERVAL: Duration = Duration::from_millis(5);

pub(crate) fn wait_for_control_snapshot_ready<Read>(
    timeout: Duration,
    poll_interval: Duration,
    mut read: Read,
) -> Result<ControlSnapshot, RobotError>
where
    Read: FnMut() -> Result<ControlSnapshot, RobotError>,
{
    let start = Instant::now();

    loop {
        match read() {
            Ok(snapshot) => return Ok(snapshot),
            Err(err @ RobotError::ControlStateIncomplete { .. })
            | Err(err @ RobotError::StateMisaligned { .. }) => {
                if start.elapsed() >= timeout {
                    return Err(err);
                }

                let remaining = timeout.saturating_sub(start.elapsed());
                let sleep_duration = poll_interval.min(remaining);
                if sleep_duration.is_zero() {
                    return Err(err);
                }

                std::thread::sleep(sleep_duration);
            },
            Err(err) => return Err(err),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{JointArray, NewtonMeter, Rad, RadPerSecond};
    use std::sync::{Arc, Mutex};

    fn test_snapshot() -> ControlSnapshot {
        ControlSnapshot {
            position: JointArray::splat(Rad(1.2)),
            velocity: JointArray::splat(RadPerSecond(-0.4)),
            torque: JointArray::splat(NewtonMeter(3.5)),
            position_timestamp_us: 10_000,
            dynamic_timestamp_us: 10_020,
            skew_us: 20,
        }
    }

    #[test]
    fn wait_for_control_snapshot_ready_retries_incomplete_and_misaligned_states() {
        let attempts = Arc::new(Mutex::new(0usize));
        let snapshot = test_snapshot();

        let waited =
            wait_for_control_snapshot_ready(Duration::from_millis(50), Duration::from_millis(1), {
                let attempts = Arc::clone(&attempts);
                move || {
                    let mut attempts = attempts.lock().unwrap();
                    *attempts += 1;
                    match *attempts {
                        1 => Err(RobotError::control_state_incomplete(0b001, 0b11_1111)),
                        2 => Err(RobotError::state_misaligned(6_000, 2_000)),
                        _ => Ok(snapshot),
                    }
                }
            })
            .expect("helper should wait until a complete aligned control snapshot becomes ready");

        assert_eq!(waited, snapshot);
        assert_eq!(*attempts.lock().unwrap(), 3);
    }

    #[test]
    fn wait_for_control_snapshot_ready_returns_stale_immediately() {
        let started_at = Instant::now();
        let error = wait_for_control_snapshot_ready(
            Duration::from_millis(50),
            Duration::from_millis(10),
            || {
                Err(RobotError::feedback_stale(
                    Duration::from_millis(20),
                    Duration::from_millis(15),
                ))
            },
        )
        .expect_err("stale control feedback should not be retried");

        assert!(started_at.elapsed() < Duration::from_millis(10));
        assert!(matches!(error, RobotError::FeedbackStale { .. }));
    }
}
