//! # Piper CAN Adapter Layer
//!
//! CAN 硬件抽象层，提供统一的 CAN 接口抽象。

use std::time::{Duration, Instant};
use thiserror::Error;

// 重新导出 piper-protocol 中的 PiperFrame
pub use piper_protocol::PiperFrame;

// SocketCAN (Linux only)
// 优先级：mock 优先级最高，然后是显式 feature，最后是 auto-backend
#[cfg(all(
    target_os = "linux",
    any(feature = "socketcan", feature = "auto-backend")
))]
pub mod socketcan;

#[cfg(all(
    target_os = "linux",
    any(feature = "socketcan", feature = "auto-backend")
))]
pub use socketcan::SocketCanAdapter;

#[cfg(all(
    target_os = "linux",
    any(feature = "socketcan", feature = "auto-backend")
))]
pub use socketcan::split::{SocketCanRxAdapter, SocketCanTxAdapter};

// GS-USB (所有平台)
// 优先级：mock 优先级最高，然后是显式 feature，最后是 auto-backend
#[cfg(any(
    feature = "gs_usb",      // 显式启用
    feature = "auto-backend" // 自动推导
))]
pub mod gs_usb;

#[cfg(any(feature = "gs_usb", feature = "auto-backend"))]
pub use gs_usb::GsUsbCanAdapter;

// Controller-owned bridge client (UnixStream/TCP)
// Non-realtime debug / record / replay path only.
mod gs_usb_bridge;
pub mod bridge {
    pub use super::gs_usb_bridge::protocol;
    pub use super::gs_usb_bridge::{
        BridgeClientOptions, BridgeEndpoint, BridgeError, BridgeResult, BridgeTlsClientConfig,
        GsUsbBridgeClient as BridgeClient, WriterLease as MaintenanceLease,
    };
}
pub use bridge::protocol::{
    BridgeDeviceState, BridgeEvent, BridgeRole, BridgeStatus, CanIdFilter, ErrorCode, SessionToken,
};
pub use bridge::{
    BridgeClient, BridgeClientOptions, BridgeEndpoint, BridgeError, BridgeResult,
    BridgeTlsClientConfig, MaintenanceLease,
};

// 导出 split 相关的类型（如果可用）
#[cfg(any(feature = "gs_usb", feature = "auto-backend"))]
pub use gs_usb::split::{GsUsbBridgeTxAdapter, GsUsbRxAdapter, GsUsbTxAdapter};

// Mock Adapter (用于测试)
#[cfg(feature = "mock")]
pub mod mock;

#[cfg(feature = "mock")]
pub use mock::MockCanAdapter;

/// Backend timing capability for host-side realtime control.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimingCapability {
    /// Backend provides a monotonic device/transport time base suitable for host-side closed-loop control.
    RealtimeCapable,
    /// Backend can be used for monitoring / recording / open-loop commands only.
    MonitorOnly,
}

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

    fn timing_capability(&self) -> TimingCapability {
        TimingCapability::RealtimeCapable
    }
}

impl<T> RxAdapter for Box<T>
where
    T: RxAdapter + ?Sized,
{
    fn receive(&mut self) -> Result<PiperFrame, CanError> {
        (**self).receive()
    }

    fn timing_capability(&self) -> TimingCapability {
        (**self).timing_capability()
    }
}

/// 实时控制专用 TX 适配器。
///
/// 普通控制帧和故障停机帧走两条不同语义的发送路径：
/// - `send_control` 只用于 steady-state realtime / reliable 控制帧，调用方提供固定 budget。
/// - `send_shutdown_until` 只用于 fault shutdown lane，调用方提供绝对 deadline。
pub trait RealtimeTxAdapter {
    fn send_control(&mut self, frame: PiperFrame, budget: Duration) -> Result<(), CanError>;
    fn send_shutdown_until(&mut self, frame: PiperFrame, deadline: Instant)
    -> Result<(), CanError>;
}

impl<T> RealtimeTxAdapter for Box<T>
where
    T: RealtimeTxAdapter + ?Sized,
{
    fn send_control(&mut self, frame: PiperFrame, budget: Duration) -> Result<(), CanError> {
        (**self).send_control(frame, budget)
    }

    fn send_shutdown_until(
        &mut self,
        frame: PiperFrame,
        deadline: Instant,
    ) -> Result<(), CanError> {
        (**self).send_shutdown_until(frame, deadline)
    }
}

/// bridge / daemon / debug 用 TX 适配器。
///
/// 这条路径明确是 best-effort 非实时语义：
/// - 不参与 realtime dual-thread driver
/// - 不承诺 bounded shutdown
/// - bridge timeout 在 adapter/session 创建时绑定，不按调用动态指定
pub trait BridgeTxAdapter {
    fn send_bridge(&mut self, frame: PiperFrame) -> Result<(), CanError>;
}

impl<T> BridgeTxAdapter for Box<T>
where
    T: BridgeTxAdapter + ?Sized,
{
    fn send_bridge(&mut self, frame: PiperFrame) -> Result<(), CanError> {
        (**self).send_bridge(frame)
    }
}

pub trait SplittableAdapter: CanAdapter {
    type RxAdapter: RxAdapter;
    type TxAdapter: RealtimeTxAdapter;
    fn split(self) -> Result<(Self::RxAdapter, Self::TxAdapter), CanError>;
}
