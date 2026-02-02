//! Client 层 Piper Builder
//!
//! 提供链式 API 创建 `Piper<Standby>` 实例，自动处理平台差异和适配器选择。

use crate::observer::Observer;
use crate::state::*;
use crate::types::{DeviceQuirks, Result};
use piper_driver::PiperBuilder as DriverBuilder;
use semver::Version;
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, info, warn};

/// Client 层 Piper Builder
///
/// 提供链式 API 创建 `Piper<Standby>` 实例，自动处理平台差异和适配器选择。
///
/// # 示例
///
/// ```rust,no_run
/// use piper_client::PiperBuilder;
/// use std::time::Duration;
///
/// # fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
/// // 使用默认配置（推荐）
/// let robot = PiperBuilder::new().build()?;
///
/// // 指定接口
/// let robot = PiperBuilder::new()
///     .interface("can0")
///     .build()?;
///
/// // 完整配置
/// let robot = PiperBuilder::new()
///     .interface("can0")
///     .baud_rate(1_000_000)
///     .timeout(Duration::from_secs(5))
///     .build()?;
///
/// // 使用守护进程
/// let robot = PiperBuilder::new()
///     .with_daemon("/tmp/gs_usb_daemon.sock")
///     .build()?;
/// # Ok(())
/// # }
/// ```
pub struct PiperBuilder {
    interface: Option<String>,
    baud_rate: Option<u32>,
    timeout: Option<Duration>,
    daemon_addr: Option<String>,
}

/// 解析固件版本字符串
///
/// 将 "S-V1.6-3" 格式的字符串解析为 semver::Version
///
/// # 参数
///
/// - `version_str`: 固件版本字符串（例如 "S-V1.6-3"）
///
/// # 返回
///
/// 解析后的 semver::Version（例如 "1.6.3"）
fn parse_firmware_version(version_str: &str) -> Option<Version> {
    // 格式: "S-V1.6-3" -> "1.6.3"
    let version_str = version_str.trim();

    // 移除 "S-V" 前缀
    let version_part = version_str.strip_prefix("S-V")?;

    // 替换连字符为点号: "1.6-3" -> "1.6.3"
    let normalized = version_part.replace('-', ".");

    // 解析为 semver::Version
    Version::parse(&normalized).ok()
}

impl PiperBuilder {
    /// 创建新的 Builder
    pub fn new() -> Self {
        Self::default()
    }

    /// 设置 CAN 接口名称或设备序列号
    ///
    /// - Linux: "can0"/"can1" 等 SocketCAN 接口名，或设备序列号（使用 GS-USB）
    /// - macOS/Windows: GS-USB 设备序列号
    /// - 如果为 `None`，使用平台默认值（Linux: "can0", 其他: 自动选择）
    pub fn interface(mut self, interface: impl Into<String>) -> Self {
        self.interface = Some(interface.into());
        self
    }

    /// 设置 CAN 波特率（默认: 1_000_000）
    pub fn baud_rate(mut self, baud_rate: u32) -> Self {
        self.baud_rate = Some(baud_rate);
        self
    }

    /// 设置连接超时（默认: 5 秒）
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// 使用守护进程模式
    ///
    /// 当启用守护进程模式时，`interface` 参数会被忽略（守护进程模式优先级最高）。
    ///
    /// # 参数
    ///
    /// - `daemon_addr`: 守护进程地址
    ///   - UDS 路径（如 "/tmp/gs_usb_daemon.sock"）
    ///   - UDP 地址（如 "127.0.0.1:8888"）
    ///
    /// # 注意
    ///
    /// 守护进程模式会忽略 `interface` 参数，因为设备选择由守护进程管理。
    pub fn with_daemon(mut self, daemon_addr: impl Into<String>) -> Self {
        self.daemon_addr = Some(daemon_addr.into());
        self
    }

    /// 构建 `Piper<Standby>` 实例
    ///
    /// # 注意
    ///
    /// - 当启用 Daemon 模式时，`interface` 参数会被忽略（Daemon 模式优先级最高）
    /// - Interface 为 `None` 时，Linux 平台默认使用 "can0"，其他平台自动选择第一个 GS-USB 设备
    pub fn build(self) -> Result<Piper<Standby>> {
        debug!("Building Piper client connection");

        // 构造 Driver Builder
        let mut driver_builder = DriverBuilder::new();

        // 处理 interface：保持 Option<String> 语义
        if let Some(ref interface) = self.interface {
            debug!("Configuring interface: {}", interface);
            driver_builder = driver_builder.interface(interface);
        } else {
            // 使用平台默认值
            #[cfg(target_os = "linux")]
            {
                debug!("Using default Linux interface: can0");
                driver_builder = driver_builder.interface("can0");
            }
            #[cfg(not(target_os = "linux"))]
            {
                debug!("Auto-selecting first available GS-USB device");
            }
        }

        // 设置波特率（如果有）
        if let Some(baud) = self.baud_rate {
            debug!("Configuring baud rate: {} bps", baud);
            driver_builder = driver_builder.baud_rate(baud);
        }

        // 设置守护进程（如果有，优先级最高）
        if let Some(ref daemon) = self.daemon_addr {
            info!("Using daemon mode: {}", daemon);
            driver_builder = driver_builder.with_daemon(daemon);
        }

        // 构建 Driver 实例
        // 注意：DriverError 通过 #[from] 自动转换为 RobotError::Infrastructure
        let driver = Arc::new(driver_builder.build()?);
        debug!("Driver connection established");

        // 等待反馈
        let timeout = self.timeout.unwrap_or(Duration::from_secs(5));
        debug!("Waiting for robot feedback (timeout: {:?})", timeout);
        // 注意：DriverError 通过 #[from] 自动转换为 RobotError::Infrastructure
        driver.wait_for_feedback(timeout)?;
        info!("Robot ready - Standby mode");

        // 查询固件版本（用于 DeviceQuirks）
        info!("Querying firmware version for DeviceQuirks initialization");
        if let Err(e) = driver.query_firmware_version() {
            warn!(
                "Failed to query firmware version: {:?}. Using default quirks (latest firmware behavior).",
                e
            );
        }

        // 获取固件版本并创建 DeviceQuirks
        let quirks = if let Some(version_str) = driver.get_firmware_version() {
            debug!("Raw firmware version string: {}", version_str);
            match parse_firmware_version(&version_str) {
                Some(version) => {
                    info!("Parsed firmware version: {}", version);
                    DeviceQuirks::from_firmware_version(version)
                },
                None => {
                    warn!(
                        "Failed to parse firmware version '{}'. Using default quirks (latest firmware behavior).",
                        version_str
                    );
                    DeviceQuirks::from_firmware_version(Version::new(1, 9, 0)) // 默认使用最新版本行为
                },
            }
        } else {
            // 如果查询失败，使用最新版本行为（所有 quirks 都为 false/1.0）
            info!(
                "No firmware version available. Using default quirks (latest firmware behavior)."
            );
            DeviceQuirks::from_firmware_version(Version::new(1, 9, 0))
        };

        // 创建 Observer
        let observer = Observer::new(driver.clone());

        Ok(Piper {
            driver,
            observer,
            quirks,
            _state: machine::Standby,
        })
    }
}

impl Default for PiperBuilder {
    fn default() -> Self {
        Self {
            interface: {
                #[cfg(target_os = "linux")]
                {
                    Some("can0".to_string())
                }
                #[cfg(not(target_os = "linux"))]
                {
                    None // macOS/Windows: 自动选择第一个 GS-USB 设备
                }
            },
            baud_rate: Some(1_000_000),
            timeout: Some(Duration::from_secs(5)),
            daemon_addr: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_piper_builder_new() {
        let builder = PiperBuilder::new();
        assert!(builder.interface.is_some() || builder.interface.is_none());
        assert_eq!(builder.baud_rate, Some(1_000_000));
        assert_eq!(builder.timeout, Some(Duration::from_secs(5)));
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
        assert_eq!(builder.baud_rate, Some(1_000_000));
        assert_eq!(builder.timeout, Some(Duration::from_secs(5)));
    }

    #[test]
    fn test_piper_builder_with_daemon() {
        let builder = PiperBuilder::new().with_daemon("/tmp/gs_usb_daemon.sock");
        assert_eq!(
            builder.daemon_addr,
            Some("/tmp/gs_usb_daemon.sock".to_string())
        );
    }
}
