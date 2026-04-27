//! Builder 模式实现
//!
//! 提供链式构造 `Piper` 实例的便捷方式。

use crate::error::DriverError;
use crate::pipeline::PipelineConfig;
use crate::piper::{Piper, StartupValidationDeadline};
#[cfg(all(
    target_os = "linux",
    any(feature = "socketcan", feature = "auto-backend")
))]
use piper_can::SocketCanAdapter;
#[cfg(any(feature = "gs_usb", feature = "auto-backend"))]
use piper_can::gs_usb::GsUsbCanAdapter;
#[cfg(any(feature = "gs_usb", feature = "auto-backend"))]
use piper_can::gs_usb::device::GsUsbDeviceSelector;
use piper_can::{
    CanAdapter, CanDeviceError, CanDeviceErrorKind, CanError, RealtimeTxAdapter, RxAdapter,
    SplittableAdapter,
};
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SocketCanRequirement {
    StrictOnly,
    MotionCapable,
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
    fn open_socketcan(
        &self,
        iface: &str,
        baud_rate: u32,
        receive_timeout: Duration,
    ) -> Result<BuiltBackend, DriverError> {
        #[cfg(all(
            target_os = "linux",
            any(feature = "socketcan", feature = "auto-backend")
        ))]
        {
            let mut can = SocketCanAdapter::new(iface).map_err(DriverError::Can)?;
            can.configure(baud_rate).map_err(DriverError::Can)?;
            can.set_receive_timeout(receive_timeout);
            let (rx, tx) = can.split().map_err(DriverError::Can)?;
            Ok(BuiltBackend::new(rx, tx, iface, baud_rate))
        }
        #[cfg(not(all(
            target_os = "linux",
            any(feature = "socketcan", feature = "auto-backend")
        )))]
        {
            let _ = (iface, baud_rate, receive_timeout);
            Err(DriverError::Can(CanError::Device(CanDeviceError::new(
                CanDeviceErrorKind::UnsupportedConfig,
                "SocketCAN backend is not enabled",
            ))))
        }
    }

    fn open_gs_usb(
        &self,
        selector: GsUsbSelectorSpec,
        baud_rate: u32,
        receive_timeout: Duration,
    ) -> Result<BuiltBackend, DriverError> {
        #[cfg(any(feature = "gs_usb", feature = "auto-backend"))]
        {
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
        #[cfg(not(any(feature = "gs_usb", feature = "auto-backend")))]
        {
            let _ = (selector, baud_rate, receive_timeout);
            Err(DriverError::Can(CanError::Device(CanDeviceError::new(
                CanDeviceErrorKind::UnsupportedConfig,
                "GS-USB backend is not enabled",
            ))))
        }
    }
}

/// Piper Builder（链式构造）
pub struct PiperBuilder {
    target: ConnectionTarget,
    baud_rate: u32,
    pipeline_config: PipelineConfig,
    startup_validation_timeout: Duration,
}

impl PiperBuilder {
    /// 创建新的 Builder。
    pub fn new() -> Self {
        Self {
            target: ConnectionTarget::AutoStrict,
            baud_rate: 1_000_000,
            pipeline_config: PipelineConfig::default(),
            startup_validation_timeout: crate::piper::STRICT_TIMESTAMP_VALIDATION_TIMEOUT,
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

    /// 设置整个启动验收流程的总超时预算。
    ///
    /// 该预算覆盖：
    /// - backend 打开阶段
    /// - startup probe / validation
    /// - Auto 模式下的候选切换与 fallback
    pub fn startup_validation_timeout(mut self, timeout: Duration) -> Self {
        self.startup_validation_timeout = timeout;
        self
    }

    /// 构建 Piper 实例。
    pub fn build(self) -> Result<Piper, DriverError> {
        self.build_with_factory(&RealBackendFactory)
    }

    fn build_with_factory(self, factory: &impl BackendFactory) -> Result<Piper, DriverError> {
        let receive_timeout = Duration::from_millis(self.pipeline_config.receive_timeout_ms);
        let startup_deadline = StartupValidationDeadline::after(self.startup_validation_timeout);
        match &self.target {
            ConnectionTarget::AutoStrict => {
                self.build_auto_strict(factory, receive_timeout, startup_deadline)
            },
            ConnectionTarget::AutoAny => {
                self.build_auto_any(factory, receive_timeout, startup_deadline)
            },
            ConnectionTarget::SocketCan { iface } => {
                self.build_socketcan_backend(factory, iface, receive_timeout, startup_deadline)
            },
            ConnectionTarget::GsUsbAuto => self.build_gs_usb_backend(
                factory,
                GsUsbSelectorSpec::Auto,
                receive_timeout,
                startup_deadline,
            ),
            ConnectionTarget::GsUsbSerial { serial } => self.build_gs_usb_backend(
                factory,
                GsUsbSelectorSpec::Serial(serial.clone()),
                receive_timeout,
                startup_deadline,
            ),
            ConnectionTarget::GsUsbBusAddress { bus, address } => self.build_gs_usb_backend(
                factory,
                GsUsbSelectorSpec::BusAddress {
                    bus: *bus,
                    address: *address,
                },
                receive_timeout,
                startup_deadline,
            ),
        }
    }

    fn startup_deadline_expired_error(&self, context: impl Into<String>) -> DriverError {
        crate::piper::strict_realtime_timestamp_error(format!(
            "startup validation deadline elapsed {}",
            context.into()
        ))
    }

    fn build_socketcan_backend(
        &self,
        factory: &impl BackendFactory,
        iface: &str,
        receive_timeout: Duration,
        startup_deadline: StartupValidationDeadline,
    ) -> Result<Piper, DriverError> {
        if startup_deadline.is_expired_now() {
            return Err(self.startup_deadline_expired_error(format!(
                "before opening SocketCAN target {iface}"
            )));
        }

        let backend = factory.open_socketcan(iface, self.baud_rate, receive_timeout)?;
        self.build_backend_until_deadline(backend, startup_deadline)
    }

    fn build_gs_usb_backend(
        &self,
        factory: &impl BackendFactory,
        selector: GsUsbSelectorSpec,
        receive_timeout: Duration,
        startup_deadline: StartupValidationDeadline,
    ) -> Result<Piper, DriverError> {
        if startup_deadline.is_expired_now() {
            return Err(self.startup_deadline_expired_error("before opening GS-USB target"));
        }

        let backend = factory.open_gs_usb(selector, self.baud_rate, receive_timeout)?;
        self.build_backend_until_deadline(backend, startup_deadline)
    }

    fn build_backend_until_deadline(
        &self,
        backend: BuiltBackend,
        startup_deadline: StartupValidationDeadline,
    ) -> Result<Piper, DriverError> {
        if startup_deadline.is_expired_now() {
            return Err(self.startup_deadline_expired_error(format!(
                "before completing strict runtime startup for {}",
                backend.interface
            )));
        }

        let interface = backend.interface;
        let bus_speed = backend.bus_speed;
        Piper::new_dual_thread_parts_with_startup_deadline(
            backend.rx,
            backend.tx,
            Some(self.pipeline_config.clone()),
            startup_deadline,
        )
        .map(|piper| piper.with_metadata(interface, bus_speed))
    }

    fn strict_candidate_summary(errors: &[(String, DriverError)]) -> String {
        errors
            .iter()
            .map(|(iface, error)| format!("{iface}: {error}"))
            .collect::<Vec<_>>()
            .join("; ")
    }

    fn attach_auto_context(error: DriverError, context: impl Into<String>) -> DriverError {
        let context = context.into();
        match error {
            DriverError::Can(CanError::Device(mut device_error)) => {
                device_error.message = format!("{}; {}", device_error.message, context);
                DriverError::Can(CanError::Device(device_error))
            },
            other => DriverError::Can(CanError::Device(CanDeviceError::new(
                CanDeviceErrorKind::Backend,
                format!("{other}; {context}"),
            ))),
        }
    }

    fn try_socketcan_candidates(
        &self,
        factory: &impl BackendFactory,
        receive_timeout: Duration,
        startup_deadline: StartupValidationDeadline,
        requirement: SocketCanRequirement,
    ) -> Result<Piper, Vec<(String, DriverError)>> {
        #[cfg(target_os = "linux")]
        let mut errors = Vec::new();
        #[cfg(not(target_os = "linux"))]
        let errors = Vec::new();

        #[cfg(target_os = "linux")]
        {
            for iface in ["can0", "vcan0"] {
                if startup_deadline.is_expired_now() {
                    errors.push((
                        iface.to_string(),
                        self.startup_deadline_expired_error(format!(
                            "before trying SocketCAN candidate {iface}"
                        )),
                    ));
                    break;
                }

                let result =
                    self.build_socketcan_backend(factory, iface, receive_timeout, startup_deadline);
                match result {
                    Ok(piper) => {
                        let capability = piper.backend_capability();
                        let accepted = match requirement {
                            SocketCanRequirement::StrictOnly => capability.is_strict_realtime(),
                            SocketCanRequirement::MotionCapable => {
                                capability.supports_motion_control()
                            },
                        };
                        if accepted {
                            return Ok(piper);
                        }
                        let requirement_error = match requirement {
                            SocketCanRequirement::StrictOnly => {
                                crate::piper::strict_realtime_timestamp_error(format!(
                                    "SocketCAN candidate {iface} only exposed {capability:?} during startup probing"
                                ))
                            },
                            SocketCanRequirement::MotionCapable => {
                                DriverError::Can(CanError::Device(CanDeviceError::new(
                                    CanDeviceErrorKind::UnsupportedConfig,
                                    format!(
                                        "SocketCAN candidate {iface} resolved to {capability:?}, which does not support motion control"
                                    ),
                                )))
                            },
                        };
                        let deadline_expired = startup_deadline.is_expired_now();
                        errors.push((iface.to_string(), requirement_error));
                        if deadline_expired {
                            break;
                        }
                    },
                    Err(error) => {
                        let deadline_expired = startup_deadline.is_expired_now();
                        errors.push((iface.to_string(), error));
                        if deadline_expired {
                            break;
                        }
                    },
                }
            }
        }
        #[cfg(not(target_os = "linux"))]
        let _ = (factory, receive_timeout, startup_deadline, requirement);

        Err(errors)
    }

    fn build_auto_strict(
        &self,
        factory: &impl BackendFactory,
        receive_timeout: Duration,
        startup_deadline: StartupValidationDeadline,
    ) -> Result<Piper, DriverError> {
        match self.try_socketcan_candidates(
            factory,
            receive_timeout,
            startup_deadline,
            SocketCanRequirement::StrictOnly,
        ) {
            Ok(piper) => Ok(piper),
            Err(errors) if errors.is_empty() => {
                Err(DriverError::Can(CanError::Device(CanDeviceError::new(
                    CanDeviceErrorKind::NotFound,
                    "AutoStrict could not find a SocketCAN interface; userspace GS-USB is intentionally excluded from strict realtime auto-selection",
                ))))
            },
            Err(errors) => {
                let summary = Self::strict_candidate_summary(&errors);
                let (_, last_error) =
                    errors.into_iter().last().expect("non-empty strict candidate errors");
                Err(Self::attach_auto_context(
                    last_error,
                    format!("tried SocketCAN candidates: {summary}"),
                ))
            },
        }
    }

    fn build_auto_any(
        &self,
        factory: &impl BackendFactory,
        receive_timeout: Duration,
        startup_deadline: StartupValidationDeadline,
    ) -> Result<Piper, DriverError> {
        let strict_errors = match self.try_socketcan_candidates(
            factory,
            receive_timeout,
            startup_deadline,
            SocketCanRequirement::MotionCapable,
        ) {
            Ok(piper) => return Ok(piper),
            Err(errors) => errors,
        };

        match self.build_gs_usb_backend(
            factory,
            GsUsbSelectorSpec::Auto,
            receive_timeout,
            startup_deadline,
        ) {
            Ok(piper) => Ok(piper),
            Err(error) if strict_errors.is_empty() => Err(error),
            Err(error) => Err(Self::attach_auto_context(
                error,
                format!(
                    "strict candidates failed before GS-USB fallback: {}",
                    Self::strict_candidate_summary(&strict_errors)
                ),
            )),
        }
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

    fn received(frame: piper_can::PiperFrame) -> piper_can::ReceivedFrame {
        piper_can::ReceivedFrame::new(frame, piper_can::TimestampProvenance::None)
    }

    struct TestRxAdapter;

    impl RxAdapter for TestRxAdapter {
        fn receive(&mut self) -> Result<piper_can::ReceivedFrame, CanError> {
            Err(CanError::Timeout)
        }

        fn backend_capability(&self) -> piper_can::BackendCapability {
            piper_can::BackendCapability::SoftRealtime
        }
    }

    struct StrictNoTimestampRxAdapter;

    impl RxAdapter for StrictNoTimestampRxAdapter {
        fn receive(&mut self) -> Result<piper_can::ReceivedFrame, CanError> {
            Err(CanError::Timeout)
        }
    }

    struct StrictBootstrapRxAdapter {
        bootstrap: Option<piper_can::PiperFrame>,
    }

    impl StrictBootstrapRxAdapter {
        fn new() -> Self {
            let frame =
                piper_can::PiperFrame::new_standard(0x251, [0; 8]).unwrap().with_timestamp_us(1);
            Self {
                bootstrap: Some(frame),
            }
        }
    }

    impl RxAdapter for StrictBootstrapRxAdapter {
        fn receive(&mut self) -> Result<piper_can::ReceivedFrame, CanError> {
            if let Some(frame) = self.bootstrap.take() {
                return Ok(received(frame));
            }
            Err(CanError::Timeout)
        }

        fn startup_probe_until(
            &mut self,
            deadline: Instant,
        ) -> Result<Option<piper_can::BackendCapability>, CanError> {
            if Instant::now() > deadline {
                return Err(CanError::Device(CanDeviceError::new(
                    CanDeviceErrorKind::UnsupportedConfig,
                    "startup validation deadline elapsed before strict probe completed",
                )));
            }
            Ok(Some(piper_can::BackendCapability::StrictRealtime))
        }
    }

    struct SoftBootstrapRxAdapter {
        bootstrap: Option<piper_can::PiperFrame>,
    }

    impl SoftBootstrapRxAdapter {
        fn new() -> Self {
            let frame =
                piper_can::PiperFrame::new_standard(0x251, [0; 8]).unwrap().with_timestamp_us(1);
            Self {
                bootstrap: Some(frame),
            }
        }
    }

    impl RxAdapter for SoftBootstrapRxAdapter {
        fn receive(&mut self) -> Result<piper_can::ReceivedFrame, CanError> {
            if let Some(frame) = self.bootstrap.take() {
                return Ok(received(frame));
            }
            Err(CanError::Timeout)
        }

        fn backend_capability(&self) -> piper_can::BackendCapability {
            piper_can::BackendCapability::SoftRealtime
        }

        fn startup_probe_until(
            &mut self,
            deadline: Instant,
        ) -> Result<Option<piper_can::BackendCapability>, CanError> {
            if Instant::now() > deadline {
                return Err(CanError::Device(CanDeviceError::new(
                    CanDeviceErrorKind::UnsupportedConfig,
                    "startup validation deadline elapsed before soft probe completed",
                )));
            }
            Ok(Some(piper_can::BackendCapability::SoftRealtime))
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
        openable_socketcan: Vec<String>,
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
        fn open_socketcan(
            &self,
            iface: &str,
            baud_rate: u32,
            _receive_timeout: Duration,
        ) -> Result<BuiltBackend, DriverError> {
            self.calls.lock().unwrap().push(format!("socketcan:{iface}"));
            if self.openable_socketcan.iter().any(|item| item == iface) {
                Ok(BuiltBackend::new(
                    StrictBootstrapRxAdapter::new(),
                    TestTxAdapter,
                    format!("socketcan:{iface}"),
                    baud_rate,
                ))
            } else {
                Err(DriverError::Can(CanError::Device(CanDeviceError::new(
                    CanDeviceErrorKind::NotFound,
                    format!("unexpected SocketCAN candidate: {iface}"),
                ))))
            }
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

    #[derive(Default)]
    struct FallbackFactory {
        openable_socketcan: Vec<String>,
        calls: Mutex<Vec<String>>,
    }

    impl BackendFactory for FallbackFactory {
        fn open_socketcan(
            &self,
            iface: &str,
            baud_rate: u32,
            _receive_timeout: Duration,
        ) -> Result<BuiltBackend, DriverError> {
            self.calls.lock().unwrap().push(format!("socketcan:{iface}"));
            if !self.openable_socketcan.iter().any(|item| item == iface) {
                return Err(DriverError::Can(CanError::Device(CanDeviceError::new(
                    CanDeviceErrorKind::NotFound,
                    format!("unexpected SocketCAN candidate: {iface}"),
                ))));
            }
            match iface {
                "can0" => Ok(BuiltBackend::new(
                    StrictNoTimestampRxAdapter,
                    TestTxAdapter,
                    format!("socketcan:{iface}"),
                    baud_rate,
                )),
                "vcan0" => Ok(BuiltBackend::new(
                    StrictBootstrapRxAdapter::new(),
                    TestTxAdapter,
                    format!("socketcan:{iface}"),
                    baud_rate,
                )),
                other => Err(DriverError::Can(CanError::Device(CanDeviceError::new(
                    CanDeviceErrorKind::NotFound,
                    format!("unexpected SocketCAN candidate: {other}"),
                )))),
            }
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
            self.calls.lock().unwrap().push(label.clone());
            Ok(BuiltBackend::new(
                TestRxAdapter,
                TestTxAdapter,
                label,
                baud_rate,
            ))
        }
    }

    #[derive(Default)]
    struct ProbedSocketCanFactory {
        openable_socketcan: Vec<String>,
        calls: Mutex<Vec<String>>,
    }

    impl BackendFactory for ProbedSocketCanFactory {
        fn open_socketcan(
            &self,
            iface: &str,
            baud_rate: u32,
            _receive_timeout: Duration,
        ) -> Result<BuiltBackend, DriverError> {
            self.calls.lock().unwrap().push(format!("socketcan:{iface}"));
            if !self.openable_socketcan.iter().any(|item| item == iface) {
                return Err(DriverError::Can(CanError::Device(CanDeviceError::new(
                    CanDeviceErrorKind::NotFound,
                    format!("unexpected SocketCAN candidate: {iface}"),
                ))));
            }
            match iface {
                "can0" => Ok(BuiltBackend::new(
                    SoftBootstrapRxAdapter::new(),
                    TestTxAdapter,
                    format!("socketcan:{iface}"),
                    baud_rate,
                )),
                "vcan0" => Ok(BuiltBackend::new(
                    StrictBootstrapRxAdapter::new(),
                    TestTxAdapter,
                    format!("socketcan:{iface}"),
                    baud_rate,
                )),
                other => Err(DriverError::Can(CanError::Device(CanDeviceError::new(
                    CanDeviceErrorKind::NotFound,
                    format!("unexpected SocketCAN candidate: {other}"),
                )))),
            }
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
            self.calls.lock().unwrap().push(label.clone());
            Ok(BuiltBackend::new(
                TestRxAdapter,
                TestTxAdapter,
                label,
                baud_rate,
            ))
        }
    }

    struct SlowOpenFactory {
        openable_socketcan: Vec<String>,
        calls: Mutex<Vec<String>>,
        socketcan_delay: Duration,
    }

    impl BackendFactory for SlowOpenFactory {
        fn open_socketcan(
            &self,
            iface: &str,
            baud_rate: u32,
            _receive_timeout: Duration,
        ) -> Result<BuiltBackend, DriverError> {
            self.calls.lock().unwrap().push(format!("socketcan:{iface}"));
            if !self.openable_socketcan.iter().any(|item| item == iface) {
                return Err(DriverError::Can(CanError::Device(CanDeviceError::new(
                    CanDeviceErrorKind::NotFound,
                    format!("unexpected SocketCAN candidate: {iface}"),
                ))));
            }
            if !self.socketcan_delay.is_zero() {
                std::thread::sleep(self.socketcan_delay);
            }
            Ok(BuiltBackend::new(
                StrictBootstrapRxAdapter::new(),
                TestTxAdapter,
                format!("socketcan:{iface}"),
                baud_rate,
            ))
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
            self.calls.lock().unwrap().push(label.clone());
            Ok(BuiltBackend::new(
                TestRxAdapter,
                TestTxAdapter,
                label,
                baud_rate,
            ))
        }
    }

    #[test]
    fn test_builder_defaults() {
        let builder = PiperBuilder::new();
        assert_eq!(builder.target, ConnectionTarget::AutoStrict);
        assert_eq!(builder.baud_rate, 1_000_000);
        assert_eq!(builder.pipeline_config, PipelineConfig::default());
        assert_eq!(
            builder.startup_validation_timeout,
            crate::piper::STRICT_TIMESTAMP_VALIDATION_TIMEOUT
        );
    }

    #[test]
    fn test_builder_chain() {
        let config = PipelineConfig {
            receive_timeout_ms: 5,
            frame_group_timeout_ms: 20,
            velocity_buffer_timeout_us: 15_000,
            low_speed_drive_state_freshness_ms: 150,
        };
        let builder = PiperBuilder::new()
            .gs_usb_bus_address(1, 12)
            .baud_rate(500_000)
            .pipeline_config(config.clone())
            .startup_validation_timeout(Duration::from_millis(25));

        assert_eq!(
            builder.target,
            ConnectionTarget::GsUsbBusAddress {
                bus: 1,
                address: 12
            }
        );
        assert_eq!(builder.baud_rate, 500_000);
        assert_eq!(builder.pipeline_config, config);
        assert_eq!(
            builder.startup_validation_timeout,
            Duration::from_millis(25)
        );
    }

    #[test]
    fn test_auto_strict_prefers_can0_then_vcan0_and_never_falls_back_to_gs_usb() {
        let factory = FakeFactory {
            openable_socketcan: vec!["vcan0".to_string()],
            calls: Mutex::new(Vec::new()),
        };

        let result = PiperBuilder::new().build_with_factory(&factory);
        #[cfg(target_os = "linux")]
        {
            result.unwrap();
            assert_eq!(
                factory.calls.lock().unwrap().as_slice(),
                &["socketcan:can0".to_string(), "socketcan:vcan0".to_string()]
            );
        }
        #[cfg(not(target_os = "linux"))]
        {
            assert!(result.is_err());
            assert!(factory.calls.lock().unwrap().is_empty());
        }
    }

    #[test]
    fn test_auto_strict_stops_after_validation_failure_exhausts_shared_deadline() {
        let factory = FallbackFactory {
            openable_socketcan: vec!["can0".to_string(), "vcan0".to_string()],
            calls: Mutex::new(Vec::new()),
        };

        let result = PiperBuilder::new()
            .target(ConnectionTarget::AutoStrict)
            .startup_validation_timeout(Duration::from_millis(20))
            .build_with_factory(&factory);

        #[cfg(target_os = "linux")]
        {
            assert!(
                result.is_err(),
                "AutoStrict should fail once the first strict validation exhausts the shared deadline"
            );
            assert_eq!(
                factory.calls.lock().unwrap().as_slice(),
                &["socketcan:can0".to_string()]
            );
        }
        #[cfg(not(target_os = "linux"))]
        {
            assert!(result.is_err());
        }
    }

    #[test]
    fn test_auto_strict_skips_soft_socketcan_candidate_and_continues_to_strict_one() {
        let factory = ProbedSocketCanFactory {
            openable_socketcan: vec!["can0".to_string(), "vcan0".to_string()],
            calls: Mutex::new(Vec::new()),
        };

        let result = PiperBuilder::new()
            .target(ConnectionTarget::AutoStrict)
            .startup_validation_timeout(Duration::from_millis(20))
            .build_with_factory(&factory);

        #[cfg(target_os = "linux")]
        {
            let piper =
                result.expect("AutoStrict should skip soft candidate and accept strict one");
            assert_eq!(
                piper.backend_capability(),
                piper_can::BackendCapability::StrictRealtime
            );
            assert_eq!(
                factory.calls.lock().unwrap().as_slice(),
                &["socketcan:can0".to_string(), "socketcan:vcan0".to_string()]
            );
        }
        #[cfg(not(target_os = "linux"))]
        {
            assert!(result.is_err());
        }
    }

    #[test]
    fn test_auto_any_prefers_soft_socketcan_candidate_without_gs_usb_fallback() {
        let factory = ProbedSocketCanFactory {
            openable_socketcan: vec!["can0".to_string(), "vcan0".to_string()],
            calls: Mutex::new(Vec::new()),
        };

        let result = PiperBuilder::new()
            .target(ConnectionTarget::AutoAny)
            .startup_validation_timeout(Duration::from_millis(20))
            .build_with_factory(&factory);

        #[cfg(target_os = "linux")]
        {
            let piper =
                result.expect("AutoAny should accept the first soft-realtime SocketCAN candidate");
            assert_eq!(
                piper.backend_capability(),
                piper_can::BackendCapability::SoftRealtime
            );
            assert_eq!(
                factory.calls.lock().unwrap().as_slice(),
                &["socketcan:can0".to_string()]
            );
        }
        #[cfg(not(target_os = "linux"))]
        {
            result.expect("non-Linux AutoAny should go straight to GS-USB");
        }
    }

    #[test]
    fn test_auto_any_falls_back_to_gs_usb_after_fast_socketcan_failure() {
        let factory = FakeFactory {
            openable_socketcan: Vec::new(),
            calls: Mutex::new(Vec::new()),
        };

        let result = PiperBuilder::new()
            .target(ConnectionTarget::AutoAny)
            .startup_validation_timeout(Duration::from_millis(20))
            .build_with_factory(&factory);

        #[cfg(target_os = "linux")]
        {
            let piper = result.expect("AutoAny should fall back to GS-USB");
            assert_eq!(
                piper.backend_capability(),
                piper_can::BackendCapability::SoftRealtime
            );
            assert_eq!(
                factory.calls.lock().unwrap().as_slice(),
                &[
                    "socketcan:can0".to_string(),
                    "socketcan:vcan0".to_string(),
                    "gs-usb:auto".to_string(),
                ]
            );
        }
        #[cfg(not(target_os = "linux"))]
        {
            result.expect("non-Linux AutoAny should go straight to GS-USB");
        }
    }

    #[test]
    fn test_auto_any_uses_shared_startup_deadline_across_strict_candidates() {
        let factory = FallbackFactory {
            openable_socketcan: vec!["can0".to_string(), "vcan0".to_string()],
            calls: Mutex::new(Vec::new()),
        };

        let result = PiperBuilder::new()
            .target(ConnectionTarget::AutoAny)
            .startup_validation_timeout(Duration::from_millis(5))
            .build_with_factory(&factory);

        #[cfg(target_os = "linux")]
        {
            assert!(
                result.is_err(),
                "AutoAny should stop once the shared startup deadline is exhausted before fallback"
            );
            assert_eq!(
                factory.calls.lock().unwrap().as_slice(),
                &["socketcan:can0".to_string()]
            );
        }
        #[cfg(not(target_os = "linux"))]
        {
            result.expect("non-Linux AutoAny should go straight to GS-USB");
        }
    }

    #[test]
    fn test_auto_strict_stops_after_shared_startup_deadline_exhausts() {
        let factory = FallbackFactory {
            openable_socketcan: vec!["can0".to_string(), "vcan0".to_string()],
            calls: Mutex::new(Vec::new()),
        };

        let result = PiperBuilder::new()
            .target(ConnectionTarget::AutoStrict)
            .startup_validation_timeout(Duration::from_millis(5))
            .build_with_factory(&factory);

        #[cfg(target_os = "linux")]
        {
            assert!(
                result.is_err(),
                "strict auto build should fail after budget exhaustion"
            );
            assert_eq!(
                factory.calls.lock().unwrap().as_slice(),
                &["socketcan:can0".to_string()]
            );
        }
        #[cfg(not(target_os = "linux"))]
        {
            assert!(result.is_err());
        }
    }

    #[test]
    fn test_explicit_socketcan_counts_open_time_against_startup_deadline() {
        let factory = SlowOpenFactory {
            openable_socketcan: vec!["can0".to_string()],
            calls: Mutex::new(Vec::new()),
            socketcan_delay: Duration::from_millis(20),
        };

        let result = PiperBuilder::new()
            .socketcan("can0")
            .startup_validation_timeout(Duration::from_millis(5))
            .build_with_factory(&factory);

        #[cfg(target_os = "linux")]
        {
            let error = match result {
                Err(error) => error,
                Ok(_) => panic!("slow open should exhaust explicit strict deadline"),
            };
            match error {
                DriverError::Can(CanError::Device(device_error)) => {
                    assert_eq!(device_error.kind, CanDeviceErrorKind::UnsupportedConfig);
                    assert!(
                        device_error.message.contains("validation deadline"),
                        "unexpected error message: {device_error}"
                    );
                },
                other => panic!("unexpected error: {other:?}"),
            }
            assert_eq!(
                factory.calls.lock().unwrap().as_slice(),
                &["socketcan:can0".to_string()]
            );
        }
        #[cfg(not(target_os = "linux"))]
        {
            assert!(result.is_err());
        }
    }

    #[test]
    fn test_auto_any_stops_after_slow_open_exhausts_total_startup_budget() {
        let factory = SlowOpenFactory {
            openable_socketcan: vec!["can0".to_string(), "vcan0".to_string()],
            calls: Mutex::new(Vec::new()),
            socketcan_delay: Duration::from_millis(20),
        };

        let result = PiperBuilder::new()
            .target(ConnectionTarget::AutoAny)
            .startup_validation_timeout(Duration::from_millis(5))
            .build_with_factory(&factory);

        #[cfg(target_os = "linux")]
        {
            assert!(
                result.is_err(),
                "AutoAny should fail once the first slow probe exhausts the total startup budget"
            );
            assert_eq!(
                factory.calls.lock().unwrap().as_slice(),
                &["socketcan:can0".to_string()]
            );
        }
        #[cfg(not(target_os = "linux"))]
        {
            result.expect("non-Linux AutoAny should go straight to GS-USB");
        }
    }

    #[test]
    fn test_auto_strict_stops_after_slow_open_exhausts_shared_deadline() {
        let factory = SlowOpenFactory {
            openable_socketcan: vec!["can0".to_string(), "vcan0".to_string()],
            calls: Mutex::new(Vec::new()),
            socketcan_delay: Duration::from_millis(20),
        };

        let result = PiperBuilder::new()
            .target(ConnectionTarget::AutoStrict)
            .startup_validation_timeout(Duration::from_millis(5))
            .build_with_factory(&factory);

        #[cfg(target_os = "linux")]
        {
            let error = match result {
                Err(error) => error,
                Ok(_) => {
                    panic!("AutoStrict should fail once slow open exhausts the shared deadline")
                },
            };
            match error {
                DriverError::Can(CanError::Device(device_error)) => {
                    assert_eq!(device_error.kind, CanDeviceErrorKind::UnsupportedConfig);
                    assert!(
                        device_error.message.contains("validation deadline"),
                        "unexpected error message: {device_error}"
                    );
                },
                other => panic!("unexpected error: {other:?}"),
            }
            assert_eq!(
                factory.calls.lock().unwrap().as_slice(),
                &["socketcan:can0".to_string()]
            );
        }
        #[cfg(not(target_os = "linux"))]
        {
            assert!(result.is_err());
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
