//! Builder 模式实现
//!
//! 提供链式构造 `Piper` 实例的便捷方式。

use crate::error::DriverError;
use crate::pipeline::PipelineConfig;
use crate::piper::Piper;
#[cfg(target_os = "linux")]
use piper_can::SocketCanAdapter;
use piper_can::gs_usb::GsUsbCanAdapter;
use piper_can::gs_usb::device::GsUsbDeviceSelector;
use piper_can::{CanDeviceError, CanDeviceErrorKind, CanError, RealtimeTxAdapter, RxAdapter};
use std::time::Duration;

/// 类型化的连接目标。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ConnectionTarget {
    #[default]
    AutoStrict,
    AutoAny,
    SocketCan {
        iface: String,
    },
    GsUsbAuto,
    GsUsbSerial {
        serial: String,
    },
    GsUsbBusAddress {
        bus: u8,
        address: u8,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum GsUsbSelectorSpec {
    Auto,
    Serial(String),
    BusAddress { bus: u8, address: u8 },
}

struct BuiltBackend {
    rx: Box<dyn RxAdapter + Send>,
    tx: Box<dyn RealtimeTxAdapter + Send>,
    interface: String,
    bus_speed: u32,
}

impl BuiltBackend {
    fn new(
        rx: impl RxAdapter + Send + 'static,
        tx: impl RealtimeTxAdapter + Send + 'static,
        interface: impl Into<String>,
        bus_speed: u32,
    ) -> Self {
        Self {
            rx: Box::new(rx),
            tx: Box::new(tx),
            interface: interface.into(),
            bus_speed,
        }
    }
}

trait BackendFactory {
    #[cfg(target_os = "linux")]
    fn socketcan_available(&self, iface: &str) -> bool;

    fn open_socketcan(
        &self,
        iface: &str,
        baud_rate: u32,
        receive_timeout: Duration,
    ) -> Result<BuiltBackend, DriverError>;

    fn open_gs_usb(
        &self,
        selector: GsUsbSelectorSpec,
        baud_rate: u32,
        receive_timeout: Duration,
    ) -> Result<BuiltBackend, DriverError>;
}

struct RealBackendFactory;

impl BackendFactory for RealBackendFactory {
    #[cfg(target_os = "linux")]
    fn socketcan_available(&self, iface: &str) -> bool {
        SocketCanAdapter::new(iface).is_ok()
    }

    fn open_socketcan(
        &self,
        iface: &str,
        baud_rate: u32,
        receive_timeout: Duration,
    ) -> Result<BuiltBackend, DriverError> {
        #[cfg(target_os = "linux")]
        {
            let mut can = SocketCanAdapter::new(iface).map_err(DriverError::Can)?;
            can.configure(baud_rate).map_err(DriverError::Can)?;
            can.set_receive_timeout(receive_timeout);
            let (rx, tx) = can.split().map_err(DriverError::Can)?;
            Ok(BuiltBackend::new(rx, tx, iface, baud_rate))
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (iface, baud_rate, receive_timeout);
            Err(DriverError::Can(CanError::Device(CanDeviceError::new(
                CanDeviceErrorKind::UnsupportedConfig,
                "SocketCAN is only available on Linux",
            ))))
        }
    }

    fn open_gs_usb(
        &self,
        selector: GsUsbSelectorSpec,
        baud_rate: u32,
        receive_timeout: Duration,
    ) -> Result<BuiltBackend, DriverError> {
        let device_selector = match &selector {
            GsUsbSelectorSpec::Auto => GsUsbDeviceSelector::any(),
            GsUsbSelectorSpec::Serial(serial) => GsUsbDeviceSelector::by_serial(serial),
            GsUsbSelectorSpec::BusAddress { bus, address } => {
                GsUsbDeviceSelector::by_bus_address(*bus, *address)
            },
        };

        let mut can =
            GsUsbCanAdapter::new_with_selector(device_selector).map_err(DriverError::Can)?;
        can.configure(baud_rate).map_err(DriverError::Can)?;
        can.set_receive_timeout(receive_timeout);
        let (rx, tx) = can.split().map_err(DriverError::Can)?;

        let interface = match selector {
            GsUsbSelectorSpec::Auto => "gs-usb:auto".to_string(),
            GsUsbSelectorSpec::Serial(serial) => format!("gs-usb:serial:{serial}"),
            GsUsbSelectorSpec::BusAddress { bus, address } => {
                format!("gs-usb:bus-address:{bus}:{address}")
            },
        };

        Ok(BuiltBackend::new(rx, tx, interface, baud_rate))
    }
}

/// Piper Builder（链式构造）
pub struct PiperBuilder {
    target: ConnectionTarget,
    baud_rate: u32,
    pipeline_config: PipelineConfig,
}

impl PiperBuilder {
    /// 创建新的 Builder。
    pub fn new() -> Self {
        Self {
            target: ConnectionTarget::AutoStrict,
            baud_rate: 1_000_000,
            pipeline_config: PipelineConfig::default(),
        }
    }

    /// 显式指定连接目标。
    pub fn target(mut self, target: ConnectionTarget) -> Self {
        self.target = target;
        self
    }

    /// 使用 SocketCAN。
    pub fn socketcan(mut self, iface: impl Into<String>) -> Self {
        self.target = ConnectionTarget::SocketCan {
            iface: iface.into(),
        };
        self
    }

    /// 自动选择 GS-USB 设备。
    pub fn gs_usb_auto(mut self) -> Self {
        self.target = ConnectionTarget::GsUsbAuto;
        self
    }

    /// 按序列号选择 GS-USB 设备。
    pub fn gs_usb_serial(mut self, serial: impl Into<String>) -> Self {
        self.target = ConnectionTarget::GsUsbSerial {
            serial: serial.into(),
        };
        self
    }

    /// 按 USB bus/address 选择 GS-USB 设备。
    pub fn gs_usb_bus_address(mut self, bus: u8, address: u8) -> Self {
        self.target = ConnectionTarget::GsUsbBusAddress { bus, address };
        self
    }

    /// 设置 CAN 波特率。
    pub fn baud_rate(mut self, baud_rate: u32) -> Self {
        self.baud_rate = baud_rate;
        self
    }

    /// 设置 Pipeline 配置。
    pub fn pipeline_config(mut self, config: PipelineConfig) -> Self {
        self.pipeline_config = config;
        self
    }

    /// 构建 Piper 实例。
    pub fn build(self) -> Result<Piper, DriverError> {
        self.build_with_factory(&RealBackendFactory)
    }

    fn build_with_factory(self, factory: &impl BackendFactory) -> Result<Piper, DriverError> {
        let receive_timeout = Duration::from_millis(self.pipeline_config.receive_timeout_ms);
        let backend = match &self.target {
            ConnectionTarget::AutoStrict => self.build_auto_strict(factory, receive_timeout)?,
            ConnectionTarget::AutoAny => self.build_auto_any(factory, receive_timeout)?,
            ConnectionTarget::SocketCan { iface } => {
                factory.open_socketcan(iface, self.baud_rate, receive_timeout)?
            },
            ConnectionTarget::GsUsbAuto => {
                factory.open_gs_usb(GsUsbSelectorSpec::Auto, self.baud_rate, receive_timeout)?
            },
            ConnectionTarget::GsUsbSerial { serial } => factory.open_gs_usb(
                GsUsbSelectorSpec::Serial(serial.clone()),
                self.baud_rate,
                receive_timeout,
            )?,
            ConnectionTarget::GsUsbBusAddress { bus, address } => factory.open_gs_usb(
                GsUsbSelectorSpec::BusAddress {
                    bus: *bus,
                    address: *address,
                },
                self.baud_rate,
                receive_timeout,
            )?,
        };

        let piper =
            Piper::new_dual_thread_parts(backend.rx, backend.tx, Some(self.pipeline_config))
                .map(|piper| piper.with_metadata(backend.interface, backend.bus_speed))
                .map_err(DriverError::Can)?;

        if piper.backend_capability().is_strict_realtime()
            && let Err(error) = piper
                .wait_for_timestamped_feedback(crate::piper::STRICT_TIMESTAMP_VALIDATION_TIMEOUT)
        {
            piper.request_stop();
            return Err(error);
        }

        Ok(piper)
    }

    fn build_auto_strict(
        &self,
        _factory: &impl BackendFactory,
        _receive_timeout: Duration,
    ) -> Result<BuiltBackend, DriverError> {
        #[cfg(target_os = "linux")]
        {
            for iface in ["can0", "vcan0"] {
                if factory.socketcan_available(iface) {
                    return factory.open_socketcan(iface, self.baud_rate, receive_timeout);
                }
            }
        }

        Err(DriverError::Can(CanError::Device(CanDeviceError::new(
            CanDeviceErrorKind::NotFound,
            "AutoStrict could not find a SocketCAN interface; userspace GS-USB is intentionally excluded from strict realtime auto-selection",
        ))))
    }

    fn build_auto_any(
        &self,
        factory: &impl BackendFactory,
        receive_timeout: Duration,
    ) -> Result<BuiltBackend, DriverError> {
        #[cfg(target_os = "linux")]
        {
            for iface in ["can0", "vcan0"] {
                if factory.socketcan_available(iface) {
                    return factory.open_socketcan(iface, self.baud_rate, receive_timeout);
                }
            }
        }

        factory.open_gs_usb(GsUsbSelectorSpec::Auto, self.baud_rate, receive_timeout)
    }
}

impl Default for PiperBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::time::Instant;

    struct TestRxAdapter;

    impl RxAdapter for TestRxAdapter {
        fn receive(&mut self) -> Result<piper_can::PiperFrame, CanError> {
            Err(CanError::Timeout)
        }

        fn backend_capability(&self) -> piper_can::BackendCapability {
            piper_can::BackendCapability::SoftRealtime
        }
    }

    struct TestTxAdapter;

    impl RealtimeTxAdapter for TestTxAdapter {
        fn send_control(
            &mut self,
            _frame: piper_can::PiperFrame,
            budget: Duration,
        ) -> Result<(), CanError> {
            if budget.is_zero() {
                return Err(CanError::Timeout);
            }
            Ok(())
        }

        fn send_shutdown_until(
            &mut self,
            _frame: piper_can::PiperFrame,
            deadline: Instant,
        ) -> Result<(), CanError> {
            if deadline <= Instant::now() {
                return Err(CanError::Timeout);
            }
            Ok(())
        }
    }

    #[derive(Default)]
    struct FakeFactory {
        #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
        socketcan_available: Vec<String>,
        calls: Mutex<Vec<String>>,
    }

    impl FakeFactory {
        fn backend(&self, label: impl Into<String>, bus_speed: u32) -> BuiltBackend {
            let label = label.into();
            self.calls.lock().unwrap().push(label.clone());
            BuiltBackend::new(TestRxAdapter, TestTxAdapter, label, bus_speed)
        }
    }

    impl BackendFactory for FakeFactory {
        #[cfg(target_os = "linux")]
        fn socketcan_available(&self, iface: &str) -> bool {
            self.socketcan_available.iter().any(|item| item == iface)
        }

        fn open_socketcan(
            &self,
            iface: &str,
            baud_rate: u32,
            _receive_timeout: Duration,
        ) -> Result<BuiltBackend, DriverError> {
            Ok(self.backend(format!("socketcan:{iface}"), baud_rate))
        }

        fn open_gs_usb(
            &self,
            selector: GsUsbSelectorSpec,
            baud_rate: u32,
            _receive_timeout: Duration,
        ) -> Result<BuiltBackend, DriverError> {
            let label = match selector {
                GsUsbSelectorSpec::Auto => "gs-usb:auto".to_string(),
                GsUsbSelectorSpec::Serial(serial) => format!("gs-usb:serial:{serial}"),
                GsUsbSelectorSpec::BusAddress { bus, address } => {
                    format!("gs-usb:bus-address:{bus}:{address}")
                },
            };
            Ok(self.backend(label, baud_rate))
        }
    }

    #[test]
    fn test_builder_defaults() {
        let builder = PiperBuilder::new();
        assert_eq!(builder.target, ConnectionTarget::AutoStrict);
        assert_eq!(builder.baud_rate, 1_000_000);
        assert_eq!(builder.pipeline_config, PipelineConfig::default());
    }

    #[test]
    fn test_builder_chain() {
        let config = PipelineConfig {
            receive_timeout_ms: 5,
            frame_group_timeout_ms: 20,
            velocity_buffer_timeout_us: 15_000,
        };
        let builder = PiperBuilder::new()
            .gs_usb_bus_address(1, 12)
            .baud_rate(500_000)
            .pipeline_config(config.clone());

        assert_eq!(
            builder.target,
            ConnectionTarget::GsUsbBusAddress {
                bus: 1,
                address: 12
            }
        );
        assert_eq!(builder.baud_rate, 500_000);
        assert_eq!(builder.pipeline_config, config);
    }

    #[test]
    fn test_auto_strict_prefers_can0_then_vcan0_and_never_falls_back_to_gs_usb() {
        let factory = FakeFactory {
            socketcan_available: vec!["vcan0".to_string()],
            calls: Mutex::new(Vec::new()),
        };

        let result = PiperBuilder::new().build_with_factory(&factory);
        #[cfg(target_os = "linux")]
        {
            result.unwrap();
            assert_eq!(
                factory.calls.lock().unwrap().as_slice(),
                &["socketcan:vcan0".to_string()]
            );
        }
        #[cfg(not(target_os = "linux"))]
        {
            assert!(result.is_err());
            assert!(factory.calls.lock().unwrap().is_empty());
        }
    }

    #[test]
    fn test_explicit_targets_do_not_fallback() {
        let factory = FakeFactory::default();

        let _ = PiperBuilder::new()
            .gs_usb_serial("ABC123")
            .build_with_factory(&factory)
            .unwrap();

        assert_eq!(
            factory.calls.lock().unwrap().as_slice(),
            &["gs-usb:serial:ABC123".to_string()]
        );
    }

    #[test]
    fn test_bus_address_uses_selector_path() {
        let factory = FakeFactory::default();
        let _ = PiperBuilder::new()
            .gs_usb_bus_address(2, 9)
            .build_with_factory(&factory)
            .unwrap();

        assert_eq!(
            factory.calls.lock().unwrap().as_slice(),
            &["gs-usb:bus-address:2:9".to_string()]
        );
    }
}
