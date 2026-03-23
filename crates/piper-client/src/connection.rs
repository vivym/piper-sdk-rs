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
    let start = Instant::now();
    debug!(
        "Waiting for robot feedback (timeout: {:?})",
        feedback_timeout
    );
    driver.wait_for_feedback(feedback_timeout)?;

    let firmware_version = driver.read_firmware_version(firmware_timeout)?;
    info!("Detected firmware version: {}", firmware_version);

    let quirks = resolve_device_quirks(&firmware_version)?;
    let initial_state = classify_initial_motion_state(&driver, start, feedback_timeout);

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
    start: Instant,
    feedback_timeout: Duration,
) -> InitialMotionState {
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
}
