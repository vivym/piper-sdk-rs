use clap::Args;
use piper_client::PiperBuilder as ClientPiperBuilder;
use piper_control::{TargetSpec, client_builder_for_target, driver_builder_for_target};
use piper_sdk::client::types::{Result as ClientResult, RobotError};
use piper_sdk::driver::{ConnectionTarget, PiperBuilder as DriverPiperBuilder};
use std::time::{Duration, Instant};

use crate::commands::config::CliConfig;

pub const INITIAL_MONITOR_SNAPSHOT_TIMEOUT: Duration = Duration::from_millis(200);
pub const INITIAL_MONITOR_SNAPSHOT_POLL_INTERVAL: Duration = Duration::from_millis(5);

#[derive(Args, Debug, Clone, Default)]
pub struct TargetArgs {
    /// 连接目标，示例: auto-strict / socketcan:can0 / gs-usb-serial:ABC123 / gs-usb-bus-address:1:8
    #[arg(long, value_name = "SPEC")]
    pub target: Option<TargetSpec>,
}

pub fn resolved_target_spec(
    config: &CliConfig,
    override_target: Option<&TargetSpec>,
) -> TargetSpec {
    config.resolved_target_spec(override_target)
}

pub fn resolved_target(
    config: &CliConfig,
    override_target: Option<&TargetSpec>,
) -> ConnectionTarget {
    resolved_target_spec(config, override_target).into_connection_target()
}

pub fn client_builder(target: &ConnectionTarget) -> ClientPiperBuilder {
    client_builder_for_target(target)
}

pub fn driver_builder(target: &ConnectionTarget) -> DriverPiperBuilder {
    driver_builder_for_target(target)
}

pub fn wait_for_initial_monitor_snapshot<T, Read>(read: Read) -> ClientResult<T>
where
    Read: FnMut() -> ClientResult<T>,
{
    wait_for_monitor_snapshot(
        INITIAL_MONITOR_SNAPSHOT_TIMEOUT,
        INITIAL_MONITOR_SNAPSHOT_POLL_INTERVAL,
        read,
    )
}

fn wait_for_monitor_snapshot<T, Read>(
    timeout: Duration,
    poll_interval: Duration,
    mut read: Read,
) -> ClientResult<T>
where
    Read: FnMut() -> ClientResult<T>,
{
    let start = Instant::now();

    loop {
        match read() {
            Ok(value) => return Ok(value),
            Err(
                RobotError::MonitorStateIncomplete { .. } | RobotError::MonitorStateStale { .. },
            ) => {},
            Err(other) => return Err(other),
        }

        if start.elapsed() >= timeout {
            return Err(RobotError::Timeout {
                timeout_ms: timeout.as_millis() as u64,
            });
        }

        let remaining = timeout.saturating_sub(start.elapsed());
        let sleep_duration = poll_interval.min(remaining);
        if sleep_duration.is_zero() {
            return Err(RobotError::Timeout {
                timeout_ms: timeout.as_millis() as u64,
            });
        }

        std::thread::sleep(sleep_duration);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use piper_sdk::client::types::MonitorStateSource;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn wait_for_monitor_snapshot_retries_incomplete_state_until_success() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let read = {
            let attempts = Arc::clone(&attempts);
            move || {
                let current = attempts.fetch_add(1, Ordering::SeqCst);
                if current < 2 {
                    Err(RobotError::monitor_state_incomplete(
                        MonitorStateSource::JointPosition,
                        0b001,
                        0b111,
                    ))
                } else {
                    Ok(42_u8)
                }
            }
        };

        let value =
            wait_for_monitor_snapshot(Duration::from_millis(50), Duration::from_millis(1), read)
                .expect("helper should retry until snapshot becomes ready");

        assert_eq!(value, 42);
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn wait_for_monitor_snapshot_returns_non_snapshot_error_immediately() {
        let started_at = Instant::now();
        let error =
            wait_for_monitor_snapshot(Duration::from_millis(50), Duration::from_millis(10), || {
                Err::<u8, _>(RobotError::ConfigError("boom".to_string()))
            })
            .expect_err("non-snapshot errors must not be retried");

        assert!(started_at.elapsed() < Duration::from_millis(10));
        assert!(matches!(error, RobotError::ConfigError(_)));
    }

    #[test]
    fn wait_for_monitor_snapshot_does_not_oversleep_timeout_budget() {
        let started_at = Instant::now();
        let error = wait_for_monitor_snapshot(
            Duration::from_millis(20),
            Duration::from_millis(100),
            || {
                Err::<u8, _>(RobotError::monitor_state_stale(
                    MonitorStateSource::EndPose,
                    Duration::from_millis(30),
                    Duration::from_millis(15),
                ))
            },
        )
        .expect_err("persistent warmup errors should time out");

        let elapsed = started_at.elapsed();
        assert!(
            elapsed < Duration::from_millis(80),
            "snapshot wait overslept too far past its 20ms budget: {elapsed:?}",
        );
        assert!(matches!(error, RobotError::Timeout { timeout_ms: 20 }));
    }
}
