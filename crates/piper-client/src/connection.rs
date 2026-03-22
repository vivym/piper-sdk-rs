use crate::types::{DeviceQuirks, Result, RobotError};
use piper_driver::Piper as DriverPiper;
use semver::Version;
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, info};

pub(crate) fn initialize_connected_driver(
    driver: Arc<DriverPiper>,
    feedback_timeout: Duration,
    firmware_timeout: Duration,
) -> Result<DeviceQuirks> {
    debug!(
        "Waiting for robot feedback (timeout: {:?})",
        feedback_timeout
    );
    driver.wait_for_feedback(feedback_timeout)?;

    let firmware_version = driver.read_firmware_version(firmware_timeout)?;
    info!("Detected firmware version: {}", firmware_version);

    let quirks = resolve_device_quirks(&firmware_version)?;
    Ok(quirks)
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
}
