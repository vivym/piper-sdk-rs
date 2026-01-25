//! # Piper CAN Adapter Layer
//!
//! CAN 硬件抽象层，提供统一的 CAN 接口抽象。

use std::time::Duration;
use thiserror::Error;

// 重新导出 piper-protocol 中的 PiperFrame
pub use piper_protocol::PiperFrame;

#[cfg(target_os = "linux")]
pub mod socketcan;

#[cfg(target_os = "linux")]
pub use socketcan::SocketCanAdapter;

#[cfg(target_os = "linux")]
pub use socketcan::split::{SocketCanRxAdapter, SocketCanTxAdapter};

pub mod gs_usb;

// Re-export gs_usb 类型
pub use gs_usb::GsUsbCanAdapter;

// GS-UDP 守护进程客户端库（UDS/UDP）
pub mod gs_usb_udp;

// 导出 split 相关的类型（如果可用）
pub use gs_usb::split::{GsUsbRxAdapter, GsUsbTxAdapter};

/// CAN 适配层统一错误类型
#[derive(Error, Debug)]
pub enum CanError {
    #[error("IO Error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Device Error: {0}")]
    Device(#[from] CanDeviceError),
    #[error("Read timeout")]
    Timeout,
    #[error("Buffer overflow")]
    BufferOverflow,
    #[error("Bus off")]
    BusOff,
    #[error("Device not started")]
    NotStarted,
}

/// 设备/后端错误的结构化分类
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CanDeviceErrorKind {
    Unknown,
    NotFound,
    NoDevice,
    AccessDenied,
    Busy,
    UnsupportedConfig,
    InvalidResponse,
    InvalidFrame,
    Backend,
}

/// 结构化设备错误
#[derive(Error, Debug, Clone)]
#[error("{kind:?}: {message}")]
pub struct CanDeviceError {
    pub kind: CanDeviceErrorKind,
    pub message: String,
}

impl CanDeviceError {
    pub fn new(kind: CanDeviceErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    pub fn is_fatal(&self) -> bool {
        matches!(
            self.kind,
            CanDeviceErrorKind::NoDevice
                | CanDeviceErrorKind::AccessDenied
                | CanDeviceErrorKind::NotFound
        )
    }
}

impl From<String> for CanDeviceError {
    fn from(message: String) -> Self {
        Self::new(CanDeviceErrorKind::Unknown, message)
    }
}

impl From<&str> for CanDeviceError {
    fn from(message: &str) -> Self {
        Self::new(CanDeviceErrorKind::Unknown, message)
    }
}

pub trait CanAdapter {
    fn send(&mut self, frame: PiperFrame) -> Result<(), CanError>;
    fn receive(&mut self) -> Result<PiperFrame, CanError>;
    fn set_receive_timeout(&mut self, _timeout: Duration) {}
    fn receive_timeout(&mut self, timeout: Duration) -> Result<PiperFrame, CanError> {
        self.set_receive_timeout(timeout);
        self.receive()
    }
    fn try_receive(&mut self) -> Result<Option<PiperFrame>, CanError> {
        match self.receive_timeout(Duration::ZERO) {
            Ok(frame) => Ok(Some(frame)),
            Err(CanError::Timeout) => Ok(None),
            Err(e) => Err(e),
        }
    }
    fn send_timeout(&mut self, frame: PiperFrame, _timeout: Duration) -> Result<(), CanError> {
        self.send(frame)
    }
}

pub trait RxAdapter {
    fn receive(&mut self) -> Result<PiperFrame, CanError>;
}

pub trait TxAdapter {
    fn send(&mut self, frame: PiperFrame) -> Result<(), CanError>;
}

pub trait SplittableAdapter: CanAdapter {
    type RxAdapter: RxAdapter;
    type TxAdapter: TxAdapter;
    fn split(self) -> Result<(Self::RxAdapter, Self::TxAdapter), CanError>;
}
