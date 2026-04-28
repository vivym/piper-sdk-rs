use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SocketCanTarget {
    pub iface: String,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum TargetError {
    #[error("target must be exactly socketcan:<iface> before connect: {target}")]
    NonSocketCan { target: String },
    #[error("SocketCAN target is not supported on this platform: {target}")]
    SocketCanUnsupported { target: String },
    #[error("SocketCAN interface name must be non-empty and contain no whitespace: {target}")]
    InvalidInterface { target: String },
    #[error("master and slave targets must use different SocketCAN interfaces: {iface}")]
    DuplicateInterface { iface: String },
}

pub fn validate_targets(
    master_target: &str,
    slave_target: &str,
) -> Result<(SocketCanTarget, SocketCanTarget), TargetError> {
    let master = parse_socketcan_target(master_target)?;
    let slave = parse_socketcan_target(slave_target)?;

    if master.iface == slave.iface {
        return Err(TargetError::DuplicateInterface {
            iface: master.iface,
        });
    }

    Ok((master, slave))
}

fn parse_socketcan_target(target: &str) -> Result<SocketCanTarget, TargetError> {
    let Some(iface) = target.strip_prefix("socketcan:") else {
        return Err(TargetError::NonSocketCan {
            target: target.to_string(),
        });
    };

    #[cfg(not(target_os = "linux"))]
    {
        let _ = iface;
        return Err(TargetError::SocketCanUnsupported {
            target: target.to_string(),
        });
    }

    #[cfg(target_os = "linux")]
    {
        if iface.is_empty() || iface.chars().any(char::is_whitespace) {
            return Err(TargetError::InvalidInterface {
                target: target.to_string(),
            });
        }

        Ok(SocketCanTarget {
            iface: iface.to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_non_socketcan_targets_before_connect() {
        assert!(validate_targets("gs-usb:ABC", "socketcan:can1").is_err());
        assert!(validate_targets("auto", "socketcan:can1").is_err());
    }

    #[test]
    fn rejects_duplicate_socketcan_interfaces() {
        assert!(validate_targets("socketcan:can0", "socketcan:can0").is_err());
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn rejects_socketcan_on_non_linux() {
        assert!(validate_targets("socketcan:can0", "socketcan:can1").is_err());
    }
}
