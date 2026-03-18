//! Client 层 Piper Builder
//!
//! 提供链式 API 创建 `Piper<Standby>` 实例，自动处理启动握手与固件 quirks 初始化。

use crate::connection::initialize_connected_driver;
use crate::state::*;
use crate::types::Result;
use piper_driver::{ConnectionTarget, PiperBuilder as DriverBuilder};
use std::sync::Arc;
use std::time::Duration;
use tracing::debug;

/// Client 层 Piper Builder
pub struct PiperBuilder {
    target: ConnectionTarget,
    baud_rate: u32,
    feedback_timeout: Duration,
    firmware_timeout: Duration,
}

impl PiperBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn target(mut self, target: ConnectionTarget) -> Self {
        self.target = target;
        self
    }

    pub fn socketcan(mut self, iface: impl Into<String>) -> Self {
        self.target = ConnectionTarget::SocketCan {
            iface: iface.into(),
        };
        self
    }

    pub fn gs_usb_auto(mut self) -> Self {
        self.target = ConnectionTarget::GsUsbAuto;
        self
    }

    pub fn gs_usb_serial(mut self, serial: impl Into<String>) -> Self {
        self.target = ConnectionTarget::GsUsbSerial {
            serial: serial.into(),
        };
        self
    }

    pub fn gs_usb_bus_address(mut self, bus: u8, address: u8) -> Self {
        self.target = ConnectionTarget::GsUsbBusAddress { bus, address };
        self
    }

    pub fn daemon_udp(mut self, addr: impl Into<String>) -> Self {
        self.target = ConnectionTarget::DaemonUdp { addr: addr.into() };
        self
    }

    pub fn daemon_uds(mut self, path: impl Into<std::path::PathBuf>) -> Self {
        self.target = ConnectionTarget::DaemonUds { path: path.into() };
        self
    }

    pub fn baud_rate(mut self, baud_rate: u32) -> Self {
        self.baud_rate = baud_rate;
        self
    }

    pub fn feedback_timeout(mut self, timeout: Duration) -> Self {
        self.feedback_timeout = timeout;
        self
    }

    pub fn firmware_timeout(mut self, timeout: Duration) -> Self {
        self.firmware_timeout = timeout;
        self
    }
    pub fn build(self) -> Result<Piper<Standby>> {
        debug!("Building Piper client connection");

        let driver = Arc::new(
            DriverBuilder::new()
                .target(self.target.clone())
                .baud_rate(self.baud_rate)
                .build()?,
        );

        let connected = initialize_connected_driver(
            driver.clone(),
            self.feedback_timeout,
            self.firmware_timeout,
        )?;

        Ok(Piper {
            driver,
            observer: connected.observer,
            quirks: connected.quirks,
            _state: machine::Standby,
        })
    }
}

impl Default for PiperBuilder {
    fn default() -> Self {
        Self {
            target: ConnectionTarget::Auto,
            baud_rate: 1_000_000,
            feedback_timeout: Duration::from_secs(5),
            firmware_timeout: Duration::from_millis(100),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_piper_builder_defaults() {
        let builder = PiperBuilder::new();
        assert_eq!(builder.target, ConnectionTarget::Auto);
        assert_eq!(builder.baud_rate, 1_000_000);
        assert_eq!(builder.feedback_timeout, Duration::from_secs(5));
        assert_eq!(builder.firmware_timeout, Duration::from_millis(100));
    }

    #[test]
    fn test_piper_builder_chain() {
        let builder = PiperBuilder::new()
            .gs_usb_bus_address(1, 8)
            .baud_rate(500_000)
            .feedback_timeout(Duration::from_secs(2))
            .firmware_timeout(Duration::from_millis(50));

        assert_eq!(
            builder.target,
            ConnectionTarget::GsUsbBusAddress { bus: 1, address: 8 }
        );
        assert_eq!(builder.baud_rate, 500_000);
        assert_eq!(builder.feedback_timeout, Duration::from_secs(2));
        assert_eq!(builder.firmware_timeout, Duration::from_millis(50));
    }
}
