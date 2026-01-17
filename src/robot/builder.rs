//! Builder 模式实现
//!
//! 提供链式构造 `Piper` 实例的便捷方式。

#[cfg(not(target_os = "linux"))]
use crate::can::gs_usb::GsUsbCanAdapter;
use crate::robot::error::RobotError;
use crate::robot::pipeline::PipelineConfig;
use crate::robot::robot_impl::Piper;

/// Piper Builder（链式构造）
///
/// 使用 Builder 模式创建 `Piper` 实例，支持链式调用。
///
/// # Example
///
/// ```no_run
/// use piper_sdk::robot::{PiperBuilder, PipelineConfig};
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
/// };
/// let piper = PiperBuilder::new()
///     .baud_rate(500_000)
///     .pipeline_config(config)
///     .build()
///     .unwrap();
/// ```
pub struct PiperBuilder {
    /// CAN 接口名称（Linux: "can0", macOS/Windows: 暂不使用，自动检测）
    interface: Option<String>,
    /// CAN 波特率（1M, 500K, 250K 等）
    baud_rate: Option<u32>,
    /// Pipeline 配置
    pipeline_config: Option<PipelineConfig>,
}

impl PiperBuilder {
    /// 创建新的 Builder
    ///
    /// # Example
    ///
    /// ```no_run
    /// use piper_sdk::robot::PiperBuilder;
    ///
    /// let builder = PiperBuilder::new();
    /// ```
    pub fn new() -> Self {
        Self {
            interface: None,
            baud_rate: None,
            pipeline_config: None,
        }
    }

    /// 设置 CAN 接口（可选，默认自动检测）
    ///
    /// # 注意
    /// - macOS/Windows (GS-USB): 此参数暂不使用，自动检测第一个设备
    /// - Linux (SocketCAN): 未来实现时将使用此参数（如 "can0"）
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

    /// 构建 Piper 实例
    ///
    /// 创建并启动 `Piper` 实例，启动后台 IO 线程。
    ///
    /// # Errors
    /// - `RobotError::Can`: CAN 设备初始化失败
    /// - `RobotError::NotImplemented`: Linux 平台尚未实现 SocketCAN
    ///
    /// # Example
    ///
    /// ```no_run
    /// use piper_sdk::robot::PiperBuilder;
    ///
    /// match PiperBuilder::new().build() {
    ///     Ok(piper) => {
    ///         // 使用 piper 读取状态
    ///         let state = piper.get_core_motion();
    ///     }
    ///     Err(e) => {
    ///         eprintln!("Failed to create Piper: {}", e);
    ///     }
    /// }
    /// ```
    pub fn build(self) -> Result<Piper, RobotError> {
        // 创建 CAN 适配器
        #[cfg(not(target_os = "linux"))]
        {
            // macOS/Windows: 使用 GS-USB 适配器
            let mut can = GsUsbCanAdapter::new().map_err(RobotError::Can)?;

            // 配置波特率（如果指定）
            let bitrate = self.baud_rate.unwrap_or(1_000_000);
            can.configure(bitrate).map_err(RobotError::Can)?;

            // 创建 Piper 实例
            Piper::new(can, self.pipeline_config).map_err(RobotError::Can)
        }

        #[cfg(target_os = "linux")]
        {
            // Linux: SocketCAN 适配器（待实现）
            let _ = self.interface; // 暂时忽略，避免未使用警告
            let _ = self.baud_rate;
            Err(RobotError::NotImplemented(
                "SocketCAN not implemented yet".to_string(),
            ))
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

    #[test]
    fn test_piper_builder_new() {
        let builder = PiperBuilder::new();
        assert_eq!(builder.interface, None);
        assert_eq!(builder.baud_rate, None);
        // 注意：不直接比较 pipeline_config，因为它没有实现 PartialEq
        // 但可以通过 is_none() 检查
        assert!(builder.pipeline_config.is_none());
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
    fn test_piper_builder_baud_rate_chaining() {
        let builder1 = PiperBuilder::new().baud_rate(1_000_000);
        let builder2 = builder1.baud_rate(500_000);

        // 验证最后一次设置生效
        assert_eq!(builder2.baud_rate, Some(500_000));
    }
}
