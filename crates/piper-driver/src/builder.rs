//! Builder 模式实现
//!
//! 提供链式构造 `Piper` 实例的便捷方式。

use crate::error::DriverError;
use crate::pipeline::PipelineConfig;
use crate::piper::Piper;
#[cfg(target_os = "linux")]
use piper_can::SocketCanAdapter;
use piper_can::gs_usb::GsUsbCanAdapter;
use piper_can::gs_usb_udp::GsUsbUdpAdapter;
use piper_can::{CanDeviceError, CanDeviceErrorKind, CanError};

/// 驱动类型选择
#[derive(Debug, Clone, Copy)]
pub enum DriverType {
    /// 自动探测（默认）
    /// - Linux: 如果 interface 是 "can0"/"can1" 等，使用 SocketCAN；否则尝试 GS-USB
    /// - 其他平台: 使用 GS-USB
    Auto,
    /// 强制使用 SocketCAN（仅 Linux）
    SocketCan,
    /// 强制使用 GS-USB（所有平台）
    GsUsb,
}

/// Piper Builder（链式构造）
///
/// 使用 Builder 模式创建 `Piper` 实例，支持链式调用。
///
/// # Example
///
/// ```no_run
/// use piper_driver::{PiperBuilder, PipelineConfig};
///
/// // 使用默认配置
/// let piper = PiperBuilder::new()
///     .build()
///     .unwrap();
///
/// // 自定义波特率和 Pipeline 配置
/// let config = PipelineConfig {
///     receive_timeout_ms: 5,
///     frame_group_timeout_ms: 20,
///     velocity_buffer_timeout_us: 20_000,
/// };
/// let piper = PiperBuilder::new()
///     .baud_rate(500_000)
///     .pipeline_config(config)
///     .build()
///     .unwrap();
/// ```
pub struct PiperBuilder {
    /// CAN 接口名称或设备序列号
    ///
    /// - Linux: "can0"/"can1" 等 SocketCAN 接口名，或设备序列号（使用 GS-USB）
    /// - macOS/Windows: GS-USB 设备序列号
    interface: Option<String>,
    /// CAN 波特率（1M, 500K, 250K 等）
    baud_rate: Option<u32>,
    /// Pipeline 配置
    pipeline_config: Option<PipelineConfig>,
    /// 守护进程地址（如果设置，使用守护进程模式）
    /// - UDS 路径（如 "/tmp/gs_usb_daemon.sock"）
    /// - UDP 地址（如 "127.0.0.1:8888"）
    daemon_addr: Option<String>,
    /// 驱动类型选择（新增）
    driver_type: DriverType,
}

impl PiperBuilder {
    /// 创建新的 Builder
    ///
    /// # Example
    ///
    /// ```no_run
    /// use piper_driver::PiperBuilder;
    ///
    /// let builder = PiperBuilder::new();
    /// ```
    pub fn new() -> Self {
        Self {
            interface: None,
            baud_rate: None,
            pipeline_config: None,
            daemon_addr: None,
            driver_type: DriverType::Auto,
        }
    }

    /// 显式指定驱动类型（可选，默认 Auto）
    ///
    /// # Example
    ///
    /// ```no_run
    /// use piper_driver::{PiperBuilder, DriverType};
    ///
    /// // 强制使用 GS-USB
    /// let piper = PiperBuilder::new()
    ///     .with_driver_type(DriverType::GsUsb)
    ///     .build()
    ///     .unwrap();
    /// ```
    pub fn with_driver_type(mut self, driver_type: DriverType) -> Self {
        self.driver_type = driver_type;
        self
    }

    /// 设置 CAN 接口（可选，默认自动检测）
    ///
    /// # 注意
    /// - Linux: 此参数可以是 SocketCAN 接口名称（如 "can0" 或 "vcan0"）或 GS-USB 设备序列号
    ///   - 如果接口名是 "can0"/"can1" 等，优先使用 SocketCAN（通过 Smart Default）
    ///   - 如果接口名是设备序列号或未提供，使用 GS-USB
    /// - macOS/Windows (GS-USB): 此参数用作设备序列号，用于区分多个 GS-USB 设备
    ///   - 如果提供序列号，只打开匹配序列号的设备
    ///   - 如果不提供，自动选择第一个找到的设备
    ///
    /// # Example
    ///
    /// ```no_run
    /// use piper_driver::PiperBuilder;
    ///
    /// // 通过序列号指定设备
    /// let piper = PiperBuilder::new()
    ///     .interface("ABC123456")
    ///     .build()
    ///     .unwrap();
    /// ```
    pub fn interface(mut self, interface: impl Into<String>) -> Self {
        self.interface = Some(interface.into());
        self
    }

    /// 设置 CAN 波特率（可选，默认 1M）
    pub fn baud_rate(mut self, baud_rate: u32) -> Self {
        self.baud_rate = Some(baud_rate);
        self
    }

    /// 设置 Pipeline 配置（可选）
    pub fn pipeline_config(mut self, config: PipelineConfig) -> Self {
        self.pipeline_config = Some(config);
        self
    }

    /// 使用守护进程模式（可选）
    ///
    /// 当指定守护进程地址时，使用 `GsUsbUdpAdapter` 通过守护进程访问 GS-USB 设备。
    /// 这解决了 macOS 下 GS-USB 设备重连后无法正常工作的限制。
    ///
    /// # 参数
    /// - `daemon_addr`: 守护进程地址
    ///   - UDS 路径（如 "/tmp/gs_usb_daemon.sock"）
    ///   - UDP 地址（如 "127.0.0.1:8888"）
    ///
    /// # Example
    ///
    /// ```no_run
    /// use piper_driver::PiperBuilder;
    ///
    /// // 使用 UDS 连接守护进程
    /// let piper = PiperBuilder::new()
    ///     .with_daemon("/tmp/gs_usb_daemon.sock")
    ///     .build()
    ///     .unwrap();
    ///
    /// // 使用 UDP 连接守护进程（用于跨机器调试）
    /// let piper = PiperBuilder::new()
    ///     .with_daemon("127.0.0.1:8888")
    ///     .build()
    ///     .unwrap();
    /// ```
    pub fn with_daemon(mut self, daemon_addr: impl Into<String>) -> Self {
        self.daemon_addr = Some(daemon_addr.into());
        self
    }

    /// 构建 Piper 实例
    ///
    /// 创建并启动 `Piper` 实例，启动后台 IO 线程。
    ///
    /// # Errors
    /// - `DriverError::Can`: CAN 设备初始化失败
    ///
    /// # Example
    ///
    /// ```no_run
    /// use piper_driver::PiperBuilder;
    ///
    /// match PiperBuilder::new().build() {
    ///     Ok(piper) => {
    ///         // 使用 piper 读取状态
    ///         let state = piper.get_joint_position();
    ///     }
    ///     Err(e) => {
    ///         eprintln!("Failed to create Piper: {}", e);
    ///     }
    /// }
    /// ```
    pub fn build(self) -> Result<Piper, DriverError> {
        // 1. 守护进程模式（所有平台，优先级最高）
        if let Some(ref daemon_addr) = self.daemon_addr {
            return self.build_gs_usb_daemon(daemon_addr.clone());
        }

        // 2. 根据 driver_type 和 interface 自动选择后端
        match self.driver_type {
            DriverType::Auto => {
                // Linux: Smart Default 逻辑
                #[cfg(target_os = "linux")]
                {
                    if let Some(ref interface) = self.interface {
                        // 如果接口名是 "can0", "can1" 等，尝试 SocketCAN
                        if interface.starts_with("can") && interface.len() <= 5 {
                            // 尝试 SocketCAN（可能失败，例如接口不存在）
                            if let Ok(piper) = self.build_socketcan(interface.as_str()) {
                                return Ok(piper);
                            }
                            // 如果 SocketCAN 失败，fallback 到 GS-USB
                            tracing::info!(
                                "SocketCAN interface '{}' not available, falling back to GS-USB",
                                interface
                            );
                        }
                    }
                    // 其他情况（未指定接口、USB 总线号等）：使用 GS-USB
                    self.build_gs_usb_direct()
                }

                // 其他平台：默认使用 GS-USB
                #[cfg(not(target_os = "linux"))]
                {
                    self.build_gs_usb_direct()
                }
            },
            DriverType::SocketCan => {
                #[cfg(target_os = "linux")]
                {
                    let interface = self.interface.as_deref().unwrap_or("can0");
                    self.build_socketcan(interface)
                }
                #[cfg(not(target_os = "linux"))]
                {
                    Err(DriverError::Can(CanError::Device(CanDeviceError::new(
                        CanDeviceErrorKind::UnsupportedConfig,
                        "SocketCAN is only available on Linux",
                    ))))
                }
            },
            DriverType::GsUsb => self.build_gs_usb_direct(),
        }
    }

    /// 构建 SocketCAN 适配器（Linux only）
    #[cfg(target_os = "linux")]
    fn build_socketcan(&self, interface: &str) -> Result<Piper, DriverError> {
        let mut can = SocketCanAdapter::new(interface).map_err(DriverError::Can)?;

        // SocketCAN 的波特率由系统配置，但可以调用 configure 验证接口状态
        if let Some(bitrate) = self.baud_rate {
            can.configure(bitrate).map_err(DriverError::Can)?;
        }

        let config = self.pipeline_config.clone().unwrap_or_default();
        can.set_read_timeout(std::time::Duration::from_millis(config.receive_timeout_ms))
            .map_err(DriverError::Can)?;

        // 使用双线程模式（默认）
        let interface = interface.to_string();
        let bus_speed = self.baud_rate.unwrap_or(1_000_000);
        Piper::new_dual_thread(can, self.pipeline_config.clone())
            .map(|p| p.with_metadata(interface, bus_speed))
            .map_err(DriverError::Can)
    }

    /// 构建 GS-USB 直连适配器
    fn build_gs_usb_direct(&self) -> Result<Piper, DriverError> {
        // interface 可能是：
        // - 设备序列号（如 "ABC123456"）
        // - USB 总线号（如 "1:12"，表示 bus 1, address 12）
        // - None（自动选择第一个设备）

        let mut can = match &self.interface {
            Some(serial) if serial.contains(':') => {
                // USB 总线号格式：bus:address
                let parts: Vec<&str> = serial.split(':').collect();
                if parts.len() == 2 {
                    if let (Ok(bus), Ok(addr)) = (parts[0].parse::<u8>(), parts[1].parse::<u8>()) {
                        use piper_can::gs_usb::device::{GsUsbDevice, GsUsbDeviceSelector};
                        let selector = GsUsbDeviceSelector::by_bus_address(bus, addr);
                        let _device = GsUsbDevice::open(&selector).map_err(|e| {
                            DriverError::Can(CanError::Device(CanDeviceError::new(
                                CanDeviceErrorKind::Backend,
                                format!("Failed to open GS-USB device at {}:{}: {}", bus, addr, e),
                            )))
                        })?;
                        // 注意：从 device 创建 adapter 的完整实现需要访问 GsUsbCanAdapter 的内部
                        // 暂时 fallback 到序列号方式
                        GsUsbCanAdapter::new_with_serial(Some(serial.as_str()))
                            .map_err(DriverError::Can)?
                    } else {
                        GsUsbCanAdapter::new_with_serial(Some(serial.as_str()))
                            .map_err(DriverError::Can)?
                    }
                } else {
                    GsUsbCanAdapter::new_with_serial(Some(serial.as_str()))
                        .map_err(DriverError::Can)?
                }
            },
            Some(serial) => {
                GsUsbCanAdapter::new_with_serial(Some(serial.as_str())).map_err(DriverError::Can)?
            },
            None => GsUsbCanAdapter::new().map_err(DriverError::Can)?,
        };

        let bitrate = self.baud_rate.unwrap_or(1_000_000);
        can.configure(bitrate).map_err(DriverError::Can)?;

        let config = self.pipeline_config.clone().unwrap_or_default();
        can.set_receive_timeout(std::time::Duration::from_millis(config.receive_timeout_ms));

        // 使用双线程模式（默认）
        let interface = self.interface.clone().unwrap_or_else(|| {
            // Try to get the actual device serial
            "unknown".to_string()
        });
        let bus_speed = bitrate;
        Piper::new_dual_thread(can, self.pipeline_config.clone())
            .map(|p| p.with_metadata(interface, bus_speed))
            .map_err(DriverError::Can)
    }

    /// 构建 GS-USB 守护进程适配器
    ///
    /// 注意：GsUsbUdpAdapter 不支持 SplittableAdapter，因此使用单线程模式。
    fn build_gs_usb_daemon(&self, daemon_addr: String) -> Result<Piper, DriverError> {
        let mut can = if daemon_addr.starts_with('/') || daemon_addr.starts_with("unix:") {
            // UDS 模式
            #[cfg(unix)]
            {
                let path = daemon_addr.strip_prefix("unix:").unwrap_or(&daemon_addr);
                GsUsbUdpAdapter::new_uds(path).map_err(DriverError::Can)?
            }
            #[cfg(not(unix))]
            {
                return Err(DriverError::Can(CanError::Device(
                    "Unix Domain Sockets are not supported on this platform. Please use UDP address format (e.g., 127.0.0.1:8888)".into(),
                )));
            }
        } else {
            // UDP 模式
            GsUsbUdpAdapter::new_udp(&daemon_addr).map_err(DriverError::Can)?
        };

        // 连接到守护进程（使用空的过滤规则，接收所有帧）
        can.connect(vec![]).map_err(DriverError::Can)?;

        // 注意：GsUsbUdpAdapter 不支持 SplittableAdapter，因此使用单线程模式
        // TODO: 实现双线程模式
        let interface = daemon_addr.clone();
        let bus_speed = self.baud_rate.unwrap_or(1_000_000);
        Piper::new(can, self.pipeline_config.clone())
            .map(|p| p.with_metadata(interface, bus_speed))
            .map_err(DriverError::Can)
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

    #[test]
    fn test_piper_builder_new() {
        let builder = PiperBuilder::new();
        assert_eq!(builder.interface, None);
        assert_eq!(builder.baud_rate, None);
        // 注意：不直接比较 pipeline_config，因为它没有实现 PartialEq
        // 但可以通过 is_none() 检查
        assert!(builder.pipeline_config.is_none());
        assert_eq!(builder.daemon_addr, None);
    }

    #[test]
    fn test_piper_builder_chain() {
        let builder = PiperBuilder::new().interface("can0").baud_rate(500_000);

        assert_eq!(builder.interface, Some("can0".to_string()));
        assert_eq!(builder.baud_rate, Some(500_000));
    }

    #[test]
    fn test_piper_builder_default() {
        let builder = PiperBuilder::default();
        assert_eq!(builder.interface, None);
        assert_eq!(builder.baud_rate, None);
    }

    #[test]
    fn test_piper_builder_pipeline_config() {
        let config = PipelineConfig {
            receive_timeout_ms: 5,
            frame_group_timeout_ms: 20,
            velocity_buffer_timeout_us: 10_000,
        };
        let builder = PiperBuilder::new().pipeline_config(config.clone());

        // 验证 pipeline_config 已设置
        assert!(builder.pipeline_config.is_some());
        let stored_config = builder.pipeline_config.as_ref().unwrap();
        assert_eq!(stored_config.receive_timeout_ms, 5);
        assert_eq!(stored_config.frame_group_timeout_ms, 20);
    }

    #[test]
    fn test_piper_builder_all_options() {
        let config = PipelineConfig {
            receive_timeout_ms: 3,
            frame_group_timeout_ms: 15,
            velocity_buffer_timeout_us: 10_000,
        };
        let builder =
            PiperBuilder::new().interface("can1").baud_rate(250_000).pipeline_config(config);

        assert_eq!(builder.interface, Some("can1".to_string()));
        assert_eq!(builder.baud_rate, Some(250_000));
        assert!(builder.pipeline_config.is_some());
    }

    #[test]
    fn test_piper_builder_interface_chaining() {
        let builder1 = PiperBuilder::new().interface("can0");
        let builder2 = builder1.interface("can1");

        // 验证最后一次设置生效
        assert_eq!(builder2.interface, Some("can1".to_string()));
    }

    #[test]
    fn test_piper_builder_receive_timeout_config() {
        // 测试：PipelineConfig.receive_timeout_ms 应该被应用到 adapter
        // 注意：这是一个编译时测试，实际运行时测试需要硬件设备
        let config = PipelineConfig {
            receive_timeout_ms: 5,
            frame_group_timeout_ms: 20,
            velocity_buffer_timeout_us: 10_000,
        };
        let builder = PiperBuilder::new().pipeline_config(config.clone());

        // 验证配置被正确存储
        assert!(builder.pipeline_config.is_some());
        let stored_config = builder.pipeline_config.as_ref().unwrap();
        assert_eq!(stored_config.receive_timeout_ms, 5);
        assert_eq!(stored_config.frame_group_timeout_ms, 20);

        // 验证默认配置
        let default_config = PipelineConfig::default();
        assert_eq!(default_config.receive_timeout_ms, 2);
        assert_eq!(default_config.frame_group_timeout_ms, 10);
    }

    #[test]
    fn test_piper_builder_baud_rate_chaining() {
        let builder1 = PiperBuilder::new().baud_rate(1_000_000);
        let builder2 = builder1.baud_rate(500_000);

        // 验证最后一次设置生效
        assert_eq!(builder2.baud_rate, Some(500_000));
    }

    #[test]
    fn test_piper_builder_with_daemon_uds() {
        let builder = PiperBuilder::new().with_daemon("/tmp/gs_usb_daemon.sock");
        assert_eq!(
            builder.daemon_addr,
            Some("/tmp/gs_usb_daemon.sock".to_string())
        );
    }

    #[test]
    fn test_piper_builder_with_daemon_udp() {
        let builder = PiperBuilder::new().with_daemon("127.0.0.1:8888");
        assert_eq!(builder.daemon_addr, Some("127.0.0.1:8888".to_string()));
    }

    #[test]
    fn test_piper_builder_with_daemon_chaining() {
        let builder1 = PiperBuilder::new().with_daemon("/tmp/test1.sock");
        let builder2 = builder1.with_daemon("127.0.0.1:8888");

        // 验证最后一次设置生效
        assert_eq!(builder2.daemon_addr, Some("127.0.0.1:8888".to_string()));
    }

    #[test]
    fn test_piper_builder_daemon_and_interface() {
        // 守护进程模式和 interface 可以同时设置（虽然 interface 在守护进程模式下会被忽略）
        let builder =
            PiperBuilder::new().with_daemon("/tmp/gs_usb_daemon.sock").interface("ABC123");

        assert_eq!(
            builder.daemon_addr,
            Some("/tmp/gs_usb_daemon.sock".to_string())
        );
        assert_eq!(builder.interface, Some("ABC123".to_string()));
    }

    #[test]
    fn test_piper_builder_driver_type() {
        let builder1 = PiperBuilder::new();
        assert!(matches!(builder1.driver_type, DriverType::Auto));

        let builder2 = PiperBuilder::new().with_driver_type(DriverType::GsUsb);
        assert!(matches!(builder2.driver_type, DriverType::GsUsb));

        let builder3 = PiperBuilder::new().with_driver_type(DriverType::SocketCan);
        assert!(matches!(builder3.driver_type, DriverType::SocketCan));
    }
}
