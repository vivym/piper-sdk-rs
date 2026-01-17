//! GS-USB 错误类型
//!
//! 定义 GS-USB 设备操作中的错误类型

use thiserror::Error;

/// GS-USB 错误类型
#[derive(Error, Debug)]
pub enum GsUsbError {
    /// USB 错误（来自 rusb）
    #[error("USB error: {0}")]
    Usb(#[from] rusb::Error),

    /// 设备未找到
    #[error("Device not found")]
    DeviceNotFound,

    /// 设备未打开
    #[error("Device not open")]
    DeviceNotOpen,

    /// 控制传输失败
    #[error("Control transfer failed: {0}")]
    ControlTransfer(rusb::Error),

    /// 批量传输失败
    #[error("Bulk transfer failed: {0}")]
    BulkTransfer(rusb::Error),

    /// 读取超时
    #[error("Read timeout")]
    ReadTimeout,

    /// 写入超时
    #[error("Write timeout")]
    WriteTimeout,

    /// 无效响应
    #[error("Invalid response from device: expected {expected} bytes, got {actual}")]
    InvalidResponse { expected: usize, actual: usize },

    /// 无效帧格式
    #[error("Invalid frame format: {0}")]
    InvalidFrame(String),

    /// 不支持的波特率
    #[error("Unsupported bitrate {bitrate} for clock {clock_hz} Hz")]
    UnsupportedBitrate { bitrate: u32, clock_hz: u32 },
}

impl GsUsbError {
    /// 检查是否为超时错误
    pub fn is_timeout(&self) -> bool {
        matches!(
            self,
            GsUsbError::ReadTimeout
                | GsUsbError::WriteTimeout
                | GsUsbError::Usb(rusb::Error::Timeout)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::GsUsbError;

    #[test]
    fn test_gs_usb_error_from_rusb_error() {
        let rusb_err = rusb::Error::NotFound;
        let gs_err: GsUsbError = rusb_err.into();

        match gs_err {
            GsUsbError::Usb(_) => {}
            _ => panic!("Expected Usb variant"),
        }
    }

    #[test]
    fn test_gs_usb_error_is_timeout() {
        let err1 = GsUsbError::ReadTimeout;
        let err2 = GsUsbError::WriteTimeout;
        let err3 = GsUsbError::Usb(rusb::Error::Timeout);

        assert!(err1.is_timeout());
        assert!(err2.is_timeout());
        assert!(err3.is_timeout());
    }

    #[test]
    fn test_gs_usb_error_is_not_timeout() {
        let err1 = GsUsbError::DeviceNotFound;
        let err2 = GsUsbError::DeviceNotOpen;
        let err3 = GsUsbError::Usb(rusb::Error::NoDevice);
        let err4 = GsUsbError::InvalidFrame("test".to_string());

        assert!(!err1.is_timeout());
        assert!(!err2.is_timeout());
        assert!(!err3.is_timeout());
        assert!(!err4.is_timeout());
    }

    #[test]
    fn test_gs_usb_error_display() {
        // 测试所有错误变体的 Display 实现
        let err1 = GsUsbError::DeviceNotFound;
        assert!(err1.to_string().contains("Device not found"));

        let err2 = GsUsbError::DeviceNotOpen;
        assert!(err2.to_string().contains("Device not open"));

        let err3 = GsUsbError::ReadTimeout;
        assert!(err3.to_string().contains("Read timeout"));

        let err4 = GsUsbError::WriteTimeout;
        assert!(err4.to_string().contains("Write timeout"));

        let err5 = GsUsbError::InvalidFrame("test message".to_string());
        assert!(err5.to_string().contains("Invalid frame format"));
        assert!(err5.to_string().contains("test message"));

        let err6 = GsUsbError::InvalidResponse {
            expected: 40,
            actual: 20,
        };
        assert!(err6.to_string().contains("Invalid response"));
        assert!(err6.to_string().contains("40"));
        assert!(err6.to_string().contains("20"));

        let err7 = GsUsbError::UnsupportedBitrate {
            bitrate: 1000000,
            clock_hz: 80000000,
        };
        assert!(err7.to_string().contains("Unsupported bitrate"));
        assert!(err7.to_string().contains("1000000"));
        assert!(err7.to_string().contains("80000000"));
    }

    #[test]
    fn test_gs_usb_error_control_transfer() {
        let rusb_err = rusb::Error::NoDevice;
        let err = GsUsbError::ControlTransfer(rusb_err);

        match err {
            GsUsbError::ControlTransfer(_) => {}
            _ => panic!("Expected ControlTransfer variant"),
        }
        assert!(!err.is_timeout());
    }

    #[test]
    fn test_gs_usb_error_bulk_transfer() {
        let rusb_err = rusb::Error::NoDevice;
        let err = GsUsbError::BulkTransfer(rusb_err);

        match err {
            GsUsbError::BulkTransfer(_) => {}
            _ => panic!("Expected BulkTransfer variant"),
        }
        assert!(!err.is_timeout());
    }

    #[test]
    fn test_gs_usb_error_from_usb_timeout() {
        // 测试通过 From trait 转换的 USB 超时错误
        let rusb_err = rusb::Error::Timeout;
        let gs_err: GsUsbError = rusb_err.into();

        match gs_err {
            GsUsbError::Usb(rusb::Error::Timeout) => {}
            _ => panic!("Expected Usb(Timeout) variant"),
        }
        assert!(gs_err.is_timeout());
    }
}
