//! Controller-owned bridge host wrapper.
//!
//! This binary remains a non-realtime bridge/debug service, but it no longer
//! owns GS-USB RX/TX directly. Instead, it builds a driver instance and embeds
//! a bridge host around committed driver state and maintenance commands.

use piper_driver::{ConnectionTarget, PiperBuilder as DriverBuilder};
use piper_sdk::{BridgeHostConfig, BridgeTlsServerConfig, PiperBridgeHost};
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct DaemonConfig {
    pub uds_path: Option<String>,
    pub tcp_tls_addr: Option<String>,
    pub tls_server_cert: Option<String>,
    pub tls_server_key: Option<String>,
    pub tls_client_ca: Option<String>,
    pub tls_allowed_client_cert_sha256: Vec<String>,
    pub bitrate: u32,
    pub serial_number: Option<String>,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            #[cfg(unix)]
            uds_path: Some("/tmp/piper_bridge.sock".to_string()),
            #[cfg(not(unix))]
            uds_path: None,
            tcp_tls_addr: None,
            tls_server_cert: None,
            tls_server_key: None,
            tls_client_ca: None,
            tls_allowed_client_cert_sha256: Vec::new(),
            bitrate: 1_000_000,
            serial_number: None,
        }
    }
}

#[derive(Debug, Clone)]
pub enum DaemonError {
    Config(String),
    Driver(String),
    Host(String),
}

impl std::fmt::Display for DaemonError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Config(message) => write!(f, "{message}"),
            Self::Driver(message) => write!(f, "{message}"),
            Self::Host(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for DaemonError {}

pub struct Daemon {
    host: PiperBridgeHost,
}

impl Daemon {
    pub fn new(config: DaemonConfig) -> Result<Self, DaemonError> {
        if config.uds_path.is_none() && config.tcp_tls_addr.is_none() {
            return Err(DaemonError::Config(
                "at least one bridge endpoint must be enabled via --uds or --tcp-tls".to_string(),
            ));
        }

        let target = if let Some(serial) = &config.serial_number {
            ConnectionTarget::GsUsbSerial {
                serial: serial.clone(),
            }
        } else {
            ConnectionTarget::GsUsbAuto
        };

        let driver = DriverBuilder::new()
            .target(target)
            .baud_rate(config.bitrate)
            .build()
            .map(Arc::new)
            .map_err(|err| {
                DaemonError::Driver(format!(
                    "failed to initialize controller-owned driver: {err}"
                ))
            })?;

        let tcp_tls = match config.tcp_tls_addr {
            Some(addr) => Some(BridgeTlsServerConfig {
                listen_addr: addr.parse().map_err(|err| {
                    DaemonError::Config(format!("invalid TCP TLS bridge address: {err}"))
                })?,
                server_cert_pem: PathBuf::from(config.tls_server_cert.ok_or_else(|| {
                    DaemonError::Config("--tcp-tls requires --tls-server-cert".to_string())
                })?),
                server_key_pem: PathBuf::from(config.tls_server_key.ok_or_else(|| {
                    DaemonError::Config("--tcp-tls requires --tls-server-key".to_string())
                })?),
                client_ca_cert_pem: PathBuf::from(config.tls_client_ca.ok_or_else(|| {
                    DaemonError::Config("--tcp-tls requires --tls-client-ca".to_string())
                })?),
                allowed_client_cert_sha256: config.tls_allowed_client_cert_sha256,
                handshake_timeout: std::time::Duration::from_secs(5),
            }),
            None => None,
        };

        let host_config = BridgeHostConfig {
            uds_path: config.uds_path.map(PathBuf::from),
            tcp_tls,
            maintenance_mode: false,
            enable_raw_frame_tap: true,
        };

        let host = PiperBridgeHost::from_driver(driver, host_config);
        Ok(Self { host })
    }

    pub fn run(self) -> Result<(), DaemonError> {
        self.host
            .run()
            .map_err(|err| DaemonError::Host(format!("bridge host failed: {err}")))
    }
}
