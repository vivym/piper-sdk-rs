//! GS-USB 适配器分离实现
//!
//! 提供独立的 RX 和 TX 适配器，支持双线程并发访问。
//! 基于 `Arc<GsUsbDevice>` 实现，利用 `rusb::DeviceHandle` 的 `Sync` 特性。

use crate::gs_usb::device::{GS_USB_BATCH_FRAME_CAPACITY, GS_USB_READ_BUFFER_SIZE, GsUsbDevice};
use crate::gs_usb::frame::GsUsbFrame;
use crate::gs_usb::protocol::{
    CAN_EFF_FLAG, CAN_EFF_MASK, CAN_ERR_CRTL_RX_BUS_OFF, CAN_ERR_CRTL_RX_PASSIVE,
    CAN_ERR_CRTL_TX_BUS_OFF, CAN_ERR_CRTL_TX_PASSIVE, CAN_ERR_FLAG, CAN_ERR_PROT_FORM,
    GS_CAN_MODE_LOOP_BACK, GS_USB_ECHO_ID,
};
use crate::{
    BackendCapability, BridgeTxAdapter, CanDeviceError, CanDeviceErrorKind, CanError, PiperFrame,
    RealtimeTxAdapter, RxAdapter,
};
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{debug, error, trace, warn};

/// 只读适配器（用于 RX 线程）
///
/// 独立的 RX 适配器，持有 `Arc<GsUsbDevice>`，
/// 可以在不同线程中与 `GsUsbTxAdapter` 并发使用。
pub struct GsUsbRxAdapter {
    device: Arc<GsUsbDevice>,
    rx_timeout: Duration,
    mode: u32,
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
    rx_queue: VecDeque<PiperFrame>,
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
        mode: u32,
        hw_timestamp_enabled: bool,
    ) -> Self {
        Self {
            device,
            rx_timeout,
            mode,
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
    fn push_to_rx_queue(&mut self, frame: PiperFrame) {
        if self.rx_queue.len() >= Self::MAX_QUEUE_SIZE {
            warn!(
                "RX queue full ({} frames), dropping oldest frame",
                Self::MAX_QUEUE_SIZE
            );
            self.rx_queue.pop_front();
        }
        self.rx_queue.push_back(frame);
    }

    fn handle_error_frame(&mut self, gs_frame: &GsUsbFrame) -> Result<bool, CanError> {
        let can_id_raw = gs_frame.can_id;
        if (can_id_raw & CAN_ERR_FLAG) == 0 {
            return Ok(false);
        }

        if gs_frame.can_dlc < 2 {
            debug!(
                "CAN Error frame (DLC too short to parse): CAN ID: 0x{:08x}, DLC: {}, Data: {:?}",
                can_id_raw,
                gs_frame.can_dlc,
                &gs_frame.data[..gs_frame.can_dlc.min(8) as usize]
            );
            return Ok(true);
        }

        let ctrl_err = gs_frame.data[1];
        let is_tx_bus_off = (ctrl_err & CAN_ERR_CRTL_TX_BUS_OFF) != 0;
        let is_rx_bus_off = (ctrl_err & CAN_ERR_CRTL_RX_BUS_OFF) != 0;

        if is_tx_bus_off || is_rx_bus_off {
            error!(
                "CAN Bus Off detected! TX: {}, RX: {}, Controller Error: 0x{:02x}, CAN ID: 0x{:08x}, Data: {:?}",
                is_tx_bus_off,
                is_rx_bus_off,
                ctrl_err,
                can_id_raw,
                &gs_frame.data[..gs_frame.can_dlc.min(8) as usize]
            );

            if let Some(ref callback) = self.bus_off_callback {
                callback(true);
            }
            return Err(CanError::BusOff);
        }

        let is_rx_passive = (ctrl_err & CAN_ERR_CRTL_RX_PASSIVE) != 0;
        let is_tx_passive = (ctrl_err & CAN_ERR_CRTL_TX_PASSIVE) != 0;

        if is_rx_passive || is_tx_passive {
            warn!(
                "CAN Error Passive detected (pre-Bus Off warning): RX: {}, TX: {}, Controller Error: 0x{:02x}, CAN ID: 0x{:08x}",
                is_rx_passive, is_tx_passive, ctrl_err, can_id_raw
            );

            if let Some(ref callback) = self.error_passive_callback {
                callback(true);
            }
        }

        if gs_frame.can_dlc >= 3 {
            let prot_err = gs_frame.data[2];
            if (prot_err & CAN_ERR_PROT_FORM) != 0 {
                warn!(
                    "CAN Format Error detected (possible bitrate mismatch): Protocol Error: 0x{:02x}, CAN ID: 0x{:08x}",
                    prot_err, can_id_raw
                );
            }

            if prot_err != 0 && (prot_err & CAN_ERR_PROT_FORM) == 0 {
                debug!(
                    "CAN Protocol Error: 0x{:02x}, Controller Error: 0x{:02x}, CAN ID: 0x{:08x}, Data: {:?}",
                    prot_err,
                    ctrl_err,
                    can_id_raw,
                    &gs_frame.data[..gs_frame.can_dlc.min(8) as usize]
                );
            }
        }

        Ok(true)
    }

    /// 接收 CAN 帧（带 Echo 帧过滤）
    ///
    /// 在双线程模式下，RX 线程会读到 TX 线程发送的回显帧。
    /// 这些 Echo 帧需要被正确过滤，否则会干扰状态解算。
    pub fn receive(&mut self) -> Result<PiperFrame, CanError> {
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

            // 3. 过滤 Echo 帧（关键：双线程模式下会收到 TX 回显）
            let is_loopback = (self.mode & GS_CAN_MODE_LOOP_BACK) != 0;

            for idx in 0..self.rx_batch_frames.len() {
                let gs_frame = self.rx_batch_frames[idx];
                if gs_frame.flags != 0 {
                    trace!(
                        "Non-zero flags detected: 0x{:02x}, CAN ID: 0x{:08x}, Channel: {}, Echo ID: 0x{:08x}, DLC: {}",
                        gs_frame.flags,
                        gs_frame.can_id,
                        gs_frame.channel,
                        gs_frame.echo_id,
                        gs_frame.can_dlc
                    );
                }

                if self.handle_error_frame(&gs_frame)? {
                    continue;
                }

                // 检查是否为 Echo 帧
                if self.is_echo_frame(&gs_frame, is_loopback) {
                    trace!(
                        "Filtered echo frame: ID=0x{:X}, echo_id={}",
                        gs_frame.can_id, gs_frame.echo_id
                    );
                    continue; // 丢弃 Echo 帧
                }

                // 检查是否为 Overflow
                if gs_frame.has_overflow() {
                    error!("CAN Buffer Overflow!");
                    return Err(CanError::BufferOverflow);
                }

                // 转换为 PiperFrame
                let piper_frame = self.convert_to_piper_frame(&gs_frame)?;
                self.push_to_rx_queue(piper_frame);
            }
            self.rx_batch_frames.clear();

            // 4. 返回第一帧（如果有）
            if let Some(frame) = self.rx_queue.pop_front() {
                return Ok(frame);
            }

            // 5. 如果全是 Echo 帧，继续读取
        }
    }

    /// 判断是否为 Echo 帧
    ///
    /// Echo 帧的特征：
    /// - `echo_id != GS_USB_RX_ECHO_ID` (0xFFFFFFFF)
    /// - 或者 flag 中包含特定标记（取决于设备模式）
    fn is_echo_frame(&self, frame: &GsUsbFrame, is_loopback: bool) -> bool {
        // 方法 1：根据 echo_id 判断
        // RX 帧的 echo_id 为 0xFFFFFFFF，Echo 帧的 echo_id 为发送时分配的值
        if !frame.is_rx_frame() {
            // 非 Loopback 模式下，所有非 RX 帧都是 Echo
            if !is_loopback {
                return true;
            }
            // Loopback 模式下，可能需要额外的过滤逻辑
            // 这里暂时也过滤掉（因为 Loopback 模式下我们通常不需要 Echo）
        }

        false
    }

    /// 转换 GsUsbFrame 到 PiperFrame
    fn convert_to_piper_frame(&mut self, gs_frame: &GsUsbFrame) -> Result<PiperFrame, CanError> {
        let timestamp_us = if self.hw_timestamp_enabled {
            self.extend_hw_timestamp(gs_frame.timestamp_us)
        } else {
            0
        };

        Ok(PiperFrame {
            id: gs_frame.can_id & CAN_EFF_MASK,
            is_extended: (gs_frame.can_id & CAN_EFF_FLAG) != 0,
            len: gs_frame.can_dlc,
            data: gs_frame.data,
            timestamp_us,
        })
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
    fn receive(&mut self) -> Result<PiperFrame, CanError> {
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
    GsUsbFrame {
        echo_id: GS_USB_ECHO_ID,
        can_id: if frame.is_extended {
            frame.id | CAN_EFF_FLAG
        } else {
            frame.id
        },
        can_dlc: frame.len,
        channel: 0,
        flags: 0,
        reserved: 0,
        data: frame.data,
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
