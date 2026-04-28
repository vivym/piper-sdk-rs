//! # Piper CAN Adapter Layer
//!
//! CAN 硬件抽象层，提供统一的 CAN 接口抽象。

use std::time::{Duration, Instant};
use thiserror::Error;

// 重新导出 piper-protocol 中的 typed frame primitives.
pub use piper_protocol::{CanData, CanId, ExtendedCanId, FrameError, PiperFrame, StandardCanId};

pub mod raw_timestamp;
pub use raw_timestamp::{RawTimestampInfo, RawTimestampSample, monotonic_micros};

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

/// Backend capability level exposed to upper layers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendCapability {
    /// Backend can be used for strict host-side closed-loop realtime control.
    StrictRealtime,
    /// Backend can be used for bounded soft-realtime or task-style control, but not strict host-side closed-loop control.
    SoftRealtime,
    /// Backend can be used for monitoring / recording / open-loop commands only.
    MonitorOnly,
}

impl BackendCapability {
    #[inline]
    pub fn is_strict_realtime(self) -> bool {
        matches!(self, Self::StrictRealtime)
    }

    #[inline]
    pub fn is_soft_realtime(self) -> bool {
        matches!(self, Self::SoftRealtime)
    }

    #[inline]
    pub fn supports_motion_control(self) -> bool {
        matches!(self, Self::StrictRealtime | Self::SoftRealtime)
    }
}

/// CAN 适配层统一错误类型
#[derive(Error, Debug)]
pub enum CanError {
    #[error("IO Error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Device Error: {0}")]
    Device(#[from] CanDeviceError),
    #[error("Frame Error: {0}")]
    Frame(#[from] FrameError),
    #[error("Read timeout")]
    Timeout,
    #[error("Buffer overflow")]
    BufferOverflow,
    #[error("Bus off")]
    BusOff,
    #[error("Device not started")]
    NotStarted,
}

/// Source class for a received frame timestamp.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TimestampProvenance {
    Hardware,
    Kernel,
    Userspace,
    None,
}

/// CAN frame plus receive-side timestamp provenance metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ReceivedFrame {
    pub frame: PiperFrame,
    pub timestamp_provenance: TimestampProvenance,
    pub raw_timestamp: Option<RawTimestampInfo>,
}

impl ReceivedFrame {
    pub fn new(frame: PiperFrame, timestamp_provenance: TimestampProvenance) -> Self {
        Self {
            frame,
            timestamp_provenance,
            raw_timestamp: None,
        }
    }

    pub fn with_raw_timestamp(mut self, raw_timestamp: RawTimestampInfo) -> Self {
        self.raw_timestamp = Some(raw_timestamp);
        self
    }
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
    fn receive(&mut self) -> Result<ReceivedFrame, CanError>;
    fn set_receive_timeout(&mut self, _timeout: Duration) {}
    fn receive_timeout(&mut self, timeout: Duration) -> Result<ReceivedFrame, CanError> {
        self.set_receive_timeout(timeout);
        self.receive()
    }
    fn try_receive(&mut self) -> Result<Option<ReceivedFrame>, CanError> {
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
    fn receive(&mut self) -> Result<ReceivedFrame, CanError>;

    fn backend_capability(&self) -> BackendCapability {
        BackendCapability::StrictRealtime
    }

    /// Optional startup-time capability probe.
    ///
    /// Adapters that can classify realtime capability before worker threads start
    /// should override this and cache any consumed frames for later replay from
    /// `receive()`. The default implementation preserves the legacy behavior:
    /// construction defers capability validation to the driver runtime.
    fn startup_probe_until(
        &mut self,
        _deadline: std::time::Instant,
    ) -> Result<Option<BackendCapability>, CanError> {
        Ok(None)
    }
}

impl<T> RxAdapter for Box<T>
where
    T: RxAdapter + ?Sized,
{
    fn receive(&mut self) -> Result<ReceivedFrame, CanError> {
        (**self).receive()
    }

    fn backend_capability(&self) -> BackendCapability {
        (**self).backend_capability()
    }

    fn startup_probe_until(
        &mut self,
        deadline: std::time::Instant,
    ) -> Result<Option<BackendCapability>, CanError> {
        (**self).startup_probe_until(deadline)
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
    fn backend_capability(&self) -> BackendCapability {
        BackendCapability::StrictRealtime
    }
    fn split(self) -> Result<(Self::RxAdapter, Self::TxAdapter), CanError>;
}
