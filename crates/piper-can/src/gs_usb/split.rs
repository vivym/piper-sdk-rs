//! GS-USB 适配器分离实现
//!
//! 提供独立的 RX 和 TX 适配器，支持双线程并发访问。
//! 基于 `Arc<GsUsbDevice>` 实现，利用 `rusb::DeviceHandle` 的 `Sync` 特性。

use crate::gs_usb::classify::parse_gs_usb_batch;
use crate::gs_usb::device::{GS_USB_BATCH_FRAME_CAPACITY, GS_USB_READ_BUFFER_SIZE, GsUsbDevice};
use crate::gs_usb::frame::GsUsbFrame;
use crate::gs_usb::protocol::{CAN_EFF_FLAG, GS_USB_ECHO_ID};
use crate::{
    BackendCapability, BridgeTxAdapter, CanDeviceError, CanDeviceErrorKind, CanError, CanId,
    PiperFrame, RealtimeTxAdapter, ReceivedFrame, RxAdapter, TimestampProvenance,
};
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::warn;

/// 只读适配器（用于 RX 线程）
///
/// 独立的 RX 适配器，持有 `Arc<GsUsbDevice>`，
/// 可以在不同线程中与 `GsUsbTxAdapter` 并发使用。
pub struct GsUsbRxAdapter {
    device: Arc<GsUsbDevice>,
    rx_timeout: Duration,
    hw_timestamp_enabled: bool,
    timestamp_wraps: u64,
    last_timestamp_low: Option<u32>,
    /// 接收队列：缓存从 USB 包中解包的多余帧
    ///
    /// **性能优化**：预分配容量以避免动态扩容的内存分配抖动（Allocator Jitter）。
    /// 在实时系统中，即使是微秒级的内存分配延迟也会积累成可观测的抖动。
    ///
    /// USB Bulk 包通常包含 1-8 个 CAN 帧（512 字节包 / 20 字节每帧），
    /// 因此预分配 64 个槽位足够应对突发流量，且避免频繁分配/释放。
    rx_queue: VecDeque<ReceivedFrame>,
    /// 复用 USB 读缓冲，避免热路径每包分配
    rx_usb_buf: [u8; GS_USB_READ_BUFFER_SIZE],
    /// 复用 GS-USB 帧容器，避免 steady-state 堆分配
    rx_batch_frames: Vec<GsUsbFrame>,
    /// Bus Off 状态更新回调（可选）
    bus_off_callback: Option<Arc<dyn Fn(bool) + Send + Sync>>,
    /// Error Passive 状态更新回调（可选）
    error_passive_callback: Option<Arc<dyn Fn(bool) + Send + Sync>>,
}

impl GsUsbRxAdapter {
    /// Maximum RX queue size to prevent unbounded memory growth
    /// When exceeded, oldest frames are dropped
    const MAX_QUEUE_SIZE: usize = 256;

    /// 创建新的 RX 适配器
    pub fn new(
        device: Arc<GsUsbDevice>,
        rx_timeout: Duration,
        _mode: u32,
        hw_timestamp_enabled: bool,
    ) -> Self {
        Self {
            device,
            rx_timeout,
            hw_timestamp_enabled,
            timestamp_wraps: 0,
            last_timestamp_low: None,
            // 关键：预分配容量，避免运行时扩容
            // 64 是经验值：足够应对突发，但不会浪费过多内存
            rx_queue: VecDeque::with_capacity(64),
            rx_usb_buf: [0u8; GS_USB_READ_BUFFER_SIZE],
            rx_batch_frames: Vec::with_capacity(GS_USB_BATCH_FRAME_CAPACITY),
            bus_off_callback: None,
            error_passive_callback: None,
        }
    }

    /// 设置 Bus Off 状态更新回调
    pub fn set_bus_off_callback<F>(&mut self, callback: F)
    where
        F: Fn(bool) + Send + Sync + 'static,
    {
        self.bus_off_callback = Some(Arc::new(callback));
    }

    /// 设置 Error Passive 状态更新回调
    pub fn set_error_passive_callback<F>(&mut self, callback: F)
    where
        F: Fn(bool) + Send + Sync + 'static,
    {
        self.error_passive_callback = Some(Arc::new(callback));
    }

    /// Push frame to RX queue with bounded size check
    ///
    /// If the queue is full, drops the oldest frame to make room.
    /// This prevents unbounded memory growth if consumer stops consuming.
    fn push_to_rx_queue(&mut self, frame: ReceivedFrame) {
        if self.rx_queue.len() >= Self::MAX_QUEUE_SIZE {
            warn!(
                "RX queue full ({} frames), dropping oldest frame",
                Self::MAX_QUEUE_SIZE
            );
            self.rx_queue.pop_front();
        }
        self.rx_queue.push_back(frame);
    }

    fn timestamp_provenance(&self) -> TimestampProvenance {
        if self.hw_timestamp_enabled {
            TimestampProvenance::Hardware
        } else {
            TimestampProvenance::None
        }
    }

    fn handle_fatal_receive_error(&self, error: &CanError) {
        if matches!(error, CanError::BusOff)
            && let Some(ref callback) = self.bus_off_callback
        {
            callback(true);
        }
    }

    /// 接收 CAN 帧（带 Echo 帧过滤）
    ///
    /// 在双线程模式下，RX 线程会读到 TX 线程发送的回显帧。
    /// 这些 Echo 帧需要被正确过滤，否则会干扰状态解算。
    pub fn receive(&mut self) -> Result<ReceivedFrame, CanError> {
        // 1. 优先从缓存队列返回
        if let Some(frame) = self.rx_queue.pop_front() {
            return Ok(frame);
        }

        // 2. 从 USB Endpoint IN 批量读取
        loop {
            match self.device.receive_batch_into(
                self.rx_timeout,
                &mut self.rx_usb_buf,
                &mut self.rx_batch_frames,
            ) {
                Ok(()) => {},
                Err(crate::gs_usb::error::GsUsbError::ReadTimeout) => {
                    return Err(CanError::Timeout);
                },
                Err(e) => {
                    let kind = match e {
                        crate::gs_usb::error::GsUsbError::Usb(rusb::Error::NoDevice) => {
                            CanDeviceErrorKind::NoDevice
                        },
                        crate::gs_usb::error::GsUsbError::Usb(rusb::Error::Access) => {
                            CanDeviceErrorKind::AccessDenied
                        },
                        crate::gs_usb::error::GsUsbError::Usb(rusb::Error::NotFound) => {
                            CanDeviceErrorKind::NotFound
                        },
                        crate::gs_usb::error::GsUsbError::InvalidFrame(_) => {
                            CanDeviceErrorKind::InvalidFrame
                        },
                        crate::gs_usb::error::GsUsbError::InvalidResponse { .. } => {
                            CanDeviceErrorKind::InvalidResponse
                        },
                        _ => CanDeviceErrorKind::Backend,
                    };
                    return Err(CanError::Device(CanDeviceError::new(
                        kind,
                        format!("USB receive failed: {}", e),
                    )));
                },
            }

            // 如果读取成功但没有帧（可能是空包），继续读下一个包
            if self.rx_batch_frames.is_empty() {
                continue;
            }

            let parsed = parse_gs_usb_batch(&self.rx_batch_frames);
            self.rx_batch_frames.clear();
            let parsed = match parsed {
                Ok(parsed) => parsed,
                Err(error) => {
                    self.handle_fatal_receive_error(&error);
                    return Err(error);
                },
            };

            let provenance = self.timestamp_provenance();
            for frame in parsed {
                let frame = if self.hw_timestamp_enabled {
                    self.extend_frame_timestamp(frame)
                } else {
                    frame.with_timestamp_us(0)
                };
                self.push_to_rx_queue(ReceivedFrame::new(frame, provenance));
            }

            // 4. 返回第一帧（如果有）
            if let Some(frame) = self.rx_queue.pop_front() {
                return Ok(frame);
            }

            // 5. 如果全是 Echo 帧，继续读取
        }
    }

    fn extend_frame_timestamp(&mut self, frame: PiperFrame) -> PiperFrame {
        frame.with_timestamp_us(self.extend_hw_timestamp(frame.timestamp_us() as u32))
    }

    fn extend_hw_timestamp(&mut self, timestamp_low: u32) -> u64 {
        if timestamp_low == 0 {
            return 0;
        }

        if let Some(previous) = self.last_timestamp_low
            && timestamp_low < previous
        {
            self.timestamp_wraps = self.timestamp_wraps.saturating_add(1);
        }

        self.last_timestamp_low = Some(timestamp_low);
        (self.timestamp_wraps << 32) | (timestamp_low as u64)
    }
}

impl RxAdapter for GsUsbRxAdapter {
    fn receive(&mut self) -> Result<ReceivedFrame, CanError> {
        self.receive()
    }

    fn backend_capability(&self) -> BackendCapability {
        if self.hw_timestamp_enabled {
            BackendCapability::SoftRealtime
        } else {
            BackendCapability::MonitorOnly
        }
    }
}

/// 只写适配器（用于 TX 线程）
///
/// 独立的 TX 适配器，持有 `Arc<GsUsbDevice>`，
/// 可以在不同线程中与 `GsUsbRxAdapter` 并发使用。
pub struct GsUsbTxAdapter {
    device: Arc<GsUsbDevice>,
}

#[doc(hidden)]
pub struct GsUsbBridgeTxAdapter {
    device: Arc<GsUsbDevice>,
    bridge_timeout: Duration,
}

fn encode_tx_frame(frame: PiperFrame) -> GsUsbFrame {
    let can_id = match frame.id() {
        CanId::Standard(id) => id.raw() as u32,
        CanId::Extended(id) => id.raw() | CAN_EFF_FLAG,
    };

    GsUsbFrame {
        echo_id: GS_USB_ECHO_ID,
        can_id,
        can_dlc: frame.dlc(),
        channel: 0,
        flags: 0,
        reserved: 0,
        data: *frame.data_padded(),
        timestamp_us: 0,
    }
}

fn map_usb_send_error(error: crate::gs_usb::error::GsUsbError) -> CanError {
    let kind = match error {
        crate::gs_usb::error::GsUsbError::WriteTimeout => {
            return CanError::Timeout;
        },
        crate::gs_usb::error::GsUsbError::Usb(rusb::Error::NoDevice) => {
            CanDeviceErrorKind::NoDevice
        },
        crate::gs_usb::error::GsUsbError::Usb(rusb::Error::Access) => {
            CanDeviceErrorKind::AccessDenied
        },
        crate::gs_usb::error::GsUsbError::Usb(rusb::Error::NotFound) => {
            CanDeviceErrorKind::NotFound
        },
        crate::gs_usb::error::GsUsbError::PartialWrite { .. } => CanDeviceErrorKind::Backend,
        _ => CanDeviceErrorKind::Backend,
    };
    CanError::Device(CanDeviceError::new(
        kind,
        format!("USB send failed: {}", error),
    ))
}

fn send_frame_until(
    device: &GsUsbDevice,
    frame: PiperFrame,
    deadline: Instant,
) -> Result<(), CanError> {
    let gs_frame = encode_tx_frame(frame);
    device.send_raw_until(&gs_frame, deadline).map_err(map_usb_send_error)
}

impl GsUsbTxAdapter {
    /// 创建新的 TX 适配器
    pub fn new(device: Arc<GsUsbDevice>) -> Self {
        Self { device }
    }

    pub fn into_bridge(self, bridge_timeout: Duration) -> GsUsbBridgeTxAdapter {
        GsUsbBridgeTxAdapter {
            device: self.device,
            bridge_timeout,
        }
    }

    /// 在固定 budget 内发送普通控制帧。
    pub fn send_frame_with_budget(
        &mut self,
        frame: PiperFrame,
        budget: Duration,
    ) -> Result<(), CanError> {
        if budget.is_zero() {
            return Err(CanError::Timeout);
        }
        self.send_frame_until(frame, Instant::now() + budget)
    }

    /// 在绝对 deadline 内发送停机帧。
    pub fn send_frame_until(
        &mut self,
        frame: PiperFrame,
        deadline: Instant,
    ) -> Result<(), CanError> {
        send_frame_until(self.device.as_ref(), frame, deadline)
    }
}

impl GsUsbBridgeTxAdapter {
    pub fn new(device: Arc<GsUsbDevice>, bridge_timeout: Duration) -> Self {
        Self {
            device,
            bridge_timeout,
        }
    }

    pub fn bridge_timeout(&self) -> Duration {
        self.bridge_timeout
    }
}

impl RealtimeTxAdapter for GsUsbTxAdapter {
    fn send_control(&mut self, frame: PiperFrame, budget: Duration) -> Result<(), CanError> {
        self.send_frame_with_budget(frame, budget)
    }

    fn send_shutdown_until(
        &mut self,
        frame: PiperFrame,
        deadline: Instant,
    ) -> Result<(), CanError> {
        self.send_frame_until(frame, deadline)
    }
}

impl BridgeTxAdapter for GsUsbBridgeTxAdapter {
    fn send_bridge(&mut self, frame: PiperFrame) -> Result<(), CanError> {
        if self.bridge_timeout.is_zero() {
            return Err(CanError::Timeout);
        }
        send_frame_until(
            self.device.as_ref(),
            frame,
            Instant::now() + self.bridge_timeout,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gs_usb::protocol::{
        GS_CAN_FLAG_OVERFLOW, GS_USB_FRAME_SIZE, GS_USB_FRAME_SIZE_HW_TIMESTAMP, GS_USB_RX_ECHO_ID,
    };

    fn pack_packet(frames: &[GsUsbFrame], hw_timestamp: bool) -> Vec<u8> {
        let frame_size = if hw_timestamp {
            GS_USB_FRAME_SIZE_HW_TIMESTAMP
        } else {
            GS_USB_FRAME_SIZE
        };
        let mut packet = Vec::with_capacity(frames.len() * frame_size);
        for frame in frames {
            let mut raw = [0u8; GS_USB_FRAME_SIZE_HW_TIMESTAMP];
            packet.extend_from_slice(frame.pack_into_array(&mut raw, hw_timestamp));
        }
        packet
    }

    fn packet_with_trailing_incomplete_bytes(frames: &[GsUsbFrame]) -> Vec<u8> {
        let mut packet = pack_packet(frames, false);
        packet.extend_from_slice(&[0xDE, 0xAD, 0xBE]);
        packet
    }

    fn rx_frame(can_id: u32, flags: u8, data0: u8) -> GsUsbFrame {
        GsUsbFrame {
            echo_id: GS_USB_RX_ECHO_ID,
            can_id,
            can_dlc: 8,
            channel: 0,
            flags,
            reserved: 0,
            data: [data0, 0, 0, 0, 0, 0, 0, 0],
            timestamp_us: 0,
        }
    }

    fn recoverable_echo_frame() -> GsUsbFrame {
        GsUsbFrame {
            echo_id: GS_USB_ECHO_ID,
            ..rx_frame(0x120, 0, 0xEE)
        }
    }

    fn malformed_dlc_frame(dlc: u8) -> GsUsbFrame {
        GsUsbFrame {
            can_dlc: dlc,
            ..rx_frame(0x120, 0, 0xEE)
        }
    }

    #[test]
    fn split_receive_discards_batch_on_overflow_status() {
        let (device, harness) = GsUsbDevice::new_test_device(false, false);
        harness.enqueue_read_packet(pack_packet(
            &[
                rx_frame(0x251, GS_CAN_FLAG_OVERFLOW, 0x11),
                rx_frame(0x252, 0, 0x22),
            ],
            false,
        ));
        let mut adapter = GsUsbRxAdapter::new(Arc::new(device), Duration::from_millis(2), 0, false);

        let overflow =
            adapter.receive().expect_err("overflow should reject the whole device batch");
        let later =
            adapter.receive().expect_err("valid frames from fatal batch must not be queued");

        assert!(matches!(overflow, CanError::BufferOverflow));
        assert!(matches!(later, CanError::Timeout));
    }

    #[test]
    fn split_receive_skips_recoverable_between_valid_frames() {
        let (device, harness) = GsUsbDevice::new_test_device(false, false);
        harness.enqueue_read_packet(pack_packet(
            &[
                rx_frame(0x100, 0, 0x10),
                recoverable_echo_frame(),
                rx_frame(0x101, 0, 0x11),
            ],
            false,
        ));
        let mut adapter = GsUsbRxAdapter::new(Arc::new(device), Duration::from_millis(2), 0, false);

        assert_eq!(adapter.receive().unwrap().frame.raw_id(), 0x100);
        assert_eq!(adapter.receive().unwrap().frame.raw_id(), 0x101);
        assert!(matches!(adapter.receive(), Err(CanError::Timeout)));
    }

    #[test]
    fn split_receive_malformed_discards_whole_batch() {
        let (device, harness) = GsUsbDevice::new_test_device(false, false);
        harness.enqueue_read_packet(pack_packet(
            &[
                rx_frame(0x100, 0, 0x10),
                malformed_dlc_frame(9),
                rx_frame(0x101, 0, 0x11),
            ],
            false,
        ));
        let mut adapter = GsUsbRxAdapter::new(Arc::new(device), Duration::from_millis(2), 0, false);

        assert!(adapter.receive().is_err());
        assert!(matches!(adapter.receive(), Err(CanError::Timeout)));
    }

    #[test]
    fn split_receive_fatal_status_discards_whole_batch() {
        let (device, harness) = GsUsbDevice::new_test_device(false, false);
        harness.enqueue_read_packet(pack_packet(
            &[
                rx_frame(0x100, 0, 0x10),
                rx_frame(0x120, GS_CAN_FLAG_OVERFLOW, 0xEE),
                rx_frame(0x101, 0, 0x11),
            ],
            false,
        ));
        let mut adapter = GsUsbRxAdapter::new(Arc::new(device), Duration::from_millis(2), 0, false);

        assert!(matches!(adapter.receive(), Err(CanError::BufferOverflow)));
        assert!(matches!(adapter.receive(), Err(CanError::Timeout)));
    }

    #[test]
    fn split_receive_fatal_transport_discards_whole_batch() {
        let (device, harness) = GsUsbDevice::new_test_device(false, false);
        harness.enqueue_read_packet(packet_with_trailing_incomplete_bytes(&[
            rx_frame(0x100, 0, 0x10),
            rx_frame(0x101, 0, 0x11),
        ]));
        let mut adapter = GsUsbRxAdapter::new(Arc::new(device), Duration::from_millis(2), 0, false);

        assert!(adapter.receive().is_err());
        assert!(matches!(adapter.receive(), Err(CanError::Timeout)));
    }

    #[test]
    fn split_receive_transport_timeout_queues_no_frame() {
        let (device, _harness) = GsUsbDevice::new_test_device(false, false);
        let mut adapter = GsUsbRxAdapter::new(Arc::new(device), Duration::from_millis(2), 0, false);

        assert!(matches!(adapter.receive(), Err(CanError::Timeout)));
        assert!(matches!(adapter.receive(), Err(CanError::Timeout)));
    }
}
