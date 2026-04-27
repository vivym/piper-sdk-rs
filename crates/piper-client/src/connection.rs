use crate::types::{DeviceQuirks, Result, RobotError};
use piper_driver::Piper as DriverPiper;
use semver::Version;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{debug, info};

const INITIAL_STATE_POLL_INTERVAL: Duration = Duration::from_millis(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InitialMotionState {
    Standby,
    Maintenance { confirmed_mask: Option<u8> },
}

impl InitialMotionState {
    pub(crate) fn confirmed_mask(self) -> Option<u8> {
        match self {
            Self::Standby => Some(0),
            Self::Maintenance { confirmed_mask } => confirmed_mask,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct InitializedConnection {
    pub(crate) quirks: DeviceQuirks,
    pub(crate) initial_state: InitialMotionState,
}

pub(crate) fn initialize_connected_driver(
    driver: Arc<DriverPiper>,
    feedback_timeout: Duration,
    firmware_timeout: Duration,
) -> Result<InitializedConnection> {
    debug!(
        "Waiting for robot feedback (timeout: {:?})",
        feedback_timeout
    );
    driver.wait_for_feedback(feedback_timeout)?;

    let firmware_version = driver.read_firmware_version(firmware_timeout)?;
    info!("Detected firmware version: {}", firmware_version);

    let quirks = resolve_device_quirks(&firmware_version)?;
    let initial_state = classify_initial_motion_state(&driver, feedback_timeout);

    Ok(InitializedConnection {
        quirks,
        initial_state,
    })
}

pub(crate) fn parse_firmware_version(version_str: &str) -> Option<Version> {
    let version_str = version_str.trim();
    let version_part = version_str.strip_prefix("S-V")?;
    let normalized = version_part.replace('-', ".");
    Version::parse(&normalized).ok()
}

fn resolve_device_quirks(firmware_version: &str) -> Result<DeviceQuirks> {
    let version = parse_firmware_version(firmware_version).ok_or_else(|| {
        RobotError::ConfigError(format!(
            "Unsupported firmware version string: {firmware_version}"
        ))
    })?;

    Ok(DeviceQuirks::from_firmware_version(version))
}

fn classify_initial_motion_state(
    driver: &DriverPiper,
    feedback_timeout: Duration,
) -> InitialMotionState {
    let start = Instant::now();
    loop {
        let confirmed_mask = driver.get_robot_control().confirmed_driver_enabled_mask;
        match confirmed_mask {
            Some(0) => return InitialMotionState::Standby,
            Some(mask) => {
                return InitialMotionState::Maintenance {
                    confirmed_mask: Some(mask),
                };
            },
            None => {},
        }

        let Some(remaining) = feedback_timeout.checked_sub(start.elapsed()) else {
            return InitialMotionState::Maintenance {
                confirmed_mask: None,
            };
        };
        if remaining.is_zero() {
            return InitialMotionState::Maintenance {
                confirmed_mask: None,
            };
        }

        std::thread::sleep(INITIAL_STATE_POLL_INTERVAL.min(remaining));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use piper_can::{CanError, PiperFrame, RealtimeTxAdapter, RxAdapter};
    use piper_driver::Piper;
    use std::collections::VecDeque;
    use std::time::Instant;

    #[test]
    fn test_parse_firmware_version() {
        let version = parse_firmware_version("S-V1.8-1").expect("version should parse");
        assert_eq!(version, Version::new(1, 8, 1));
    }

    #[test]
    fn test_parse_firmware_version_rejects_invalid_string() {
        assert!(parse_firmware_version("not-a-version").is_none());
        assert!(parse_firmware_version("1.8.1").is_none());
    }

    #[test]
    fn test_resolve_device_quirks_requires_supported_format() {
        let error = resolve_device_quirks("invalid-format").unwrap_err();
        assert!(matches!(error, RobotError::ConfigError(_)));
    }

    #[test]
    fn test_initial_motion_state_confirmed_mask_accessor() {
        assert_eq!(InitialMotionState::Standby.confirmed_mask(), Some(0));
        assert_eq!(
            InitialMotionState::Maintenance {
                confirmed_mask: Some(0b000111),
            }
            .confirmed_mask(),
            Some(0b000111)
        );
        assert_eq!(
            InitialMotionState::Maintenance {
                confirmed_mask: None,
            }
            .confirmed_mask(),
            None
        );
    }

    #[derive(Debug)]
    struct TimedFrame {
        delay: Duration,
        frame: PiperFrame,
    }

    struct PacedRxAdapter {
        bootstrap: bool,
        frames: VecDeque<TimedFrame>,
    }

    impl PacedRxAdapter {
        fn new(frames: Vec<TimedFrame>) -> Self {
            Self {
                bootstrap: false,
                frames: frames.into(),
            }
        }
    }

    impl RxAdapter for PacedRxAdapter {
        fn receive(&mut self) -> std::result::Result<piper_can::ReceivedFrame, CanError> {
            if !self.bootstrap {
                self.bootstrap = true;
                return Ok(piper_can::ReceivedFrame::new(
                    bootstrap_timestamp_frame(),
                    piper_can::TimestampProvenance::None,
                ));
            }

            match self.frames.pop_front() {
                Some(timed) => {
                    if !timed.delay.is_zero() {
                        std::thread::sleep(timed.delay);
                    }
                    Ok(piper_can::ReceivedFrame::new(
                        timed.frame,
                        piper_can::TimestampProvenance::None,
                    ))
                },
                None => Err(CanError::Timeout),
            }
        }
    }

    struct NoopTxAdapter;

    impl RealtimeTxAdapter for NoopTxAdapter {
        fn send_control(
            &mut self,
            _frame: PiperFrame,
            budget: Duration,
        ) -> std::result::Result<(), CanError> {
            if budget.is_zero() {
                Err(CanError::Timeout)
            } else {
                Ok(())
            }
        }

        fn send_shutdown_until(
            &mut self,
            _frame: PiperFrame,
            deadline: Instant,
        ) -> std::result::Result<(), CanError> {
            if deadline <= Instant::now() {
                Err(CanError::Timeout)
            } else {
                Ok(())
            }
        }
    }

    fn bootstrap_timestamp_frame() -> PiperFrame {
        PiperFrame::new_standard(
            piper_protocol::ids::ID_JOINT_FEEDBACK_12.raw().into(),
            [0; 8],
        )
        .unwrap()
        .with_timestamp_us(1)
    }

    fn joint_driver_disabled_frame(joint_index: u8, timestamp_us: u64) -> PiperFrame {
        let id = u32::from(piper_protocol::ids::ID_JOINT_DRIVER_LOW_SPEED_1.raw())
            + u32::from(joint_index)
            - 1;
        let mut data = [0u8; 8];
        data[0..2].copy_from_slice(&240u16.to_be_bytes());
        data[2..4].copy_from_slice(&45i16.to_be_bytes());
        data[4] = 50;
        data[5] = 0x00;
        data[6..8].copy_from_slice(&5000u16.to_be_bytes());
        PiperFrame::new_standard(id, data).unwrap().with_timestamp_us(timestamp_us)
    }

    #[test]
    fn classify_initial_motion_state_waits_full_budget_after_feedback_path() {
        let driver = Piper::new_dual_thread_parts(
            PacedRxAdapter::new(vec![
                TimedFrame {
                    delay: Duration::from_millis(5),
                    frame: joint_driver_disabled_frame(1, 1_000),
                },
                TimedFrame {
                    delay: Duration::from_millis(5),
                    frame: joint_driver_disabled_frame(2, 1_001),
                },
                TimedFrame {
                    delay: Duration::from_millis(5),
                    frame: joint_driver_disabled_frame(3, 1_002),
                },
                TimedFrame {
                    delay: Duration::from_millis(5),
                    frame: joint_driver_disabled_frame(4, 1_003),
                },
                TimedFrame {
                    delay: Duration::from_millis(5),
                    frame: joint_driver_disabled_frame(5, 1_004),
                },
                TimedFrame {
                    delay: Duration::from_millis(5),
                    frame: joint_driver_disabled_frame(6, 1_005),
                },
            ]),
            NoopTxAdapter,
            None,
        )
        .expect("driver should start");

        std::thread::sleep(Duration::from_millis(40));
        let initial_state = classify_initial_motion_state(&driver, Duration::from_millis(50));

        assert_eq!(initial_state, InitialMotionState::Standby);
    }
}
