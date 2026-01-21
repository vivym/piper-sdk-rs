//! GS-USB 适配器分离实现
//!
//! 提供独立的 RX 和 TX 适配器，支持双线程并发访问。
//! 基于 `Arc<GsUsbDevice>` 实现，利用 `rusb::DeviceHandle` 的 `Sync` 特性。

use crate::can::gs_usb::device::GsUsbDevice;
use crate::can::gs_usb::frame::GsUsbFrame;
use crate::can::gs_usb::protocol::{
    CAN_EFF_FLAG, CAN_EFF_MASK, CAN_ERR_CRTL_RX_BUS_OFF, CAN_ERR_CRTL_RX_PASSIVE,
    CAN_ERR_CRTL_TX_BUS_OFF, CAN_ERR_CRTL_TX_PASSIVE, CAN_ERR_FLAG, CAN_ERR_PROT_FORM,
    GS_CAN_MODE_LOOP_BACK, GS_USB_ECHO_ID,
};
use crate::can::{CanDeviceError, CanDeviceErrorKind, CanError, PiperFrame, RxAdapter, TxAdapter};
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, error, trace, warn};

/// 只读适配器（用于 RX 线程）
///
/// 独立的 RX 适配器，持有 `Arc<GsUsbDevice>`，
/// 可以在不同线程中与 `GsUsbTxAdapter` 并发使用。
pub struct GsUsbRxAdapter {
    device: Arc<GsUsbDevice>,
    rx_timeout: Duration,
    mode: u32,
    /// 接收队列：缓存从 USB 包中解包的多余帧
    ///
    /// **性能优化**：预分配容量以避免动态扩容的内存分配抖动（Allocator Jitter）。
    /// 在实时系统中，即使是微秒级的内存分配延迟也会积累成可观测的抖动。
    ///
    /// USB Bulk 包通常包含 1-8 个 CAN 帧（512 字节包 / 20 字节每帧），
    /// 因此预分配 64 个槽位足够应对突发流量，且避免频繁分配/释放。
    rx_queue: VecDeque<PiperFrame>,
    /// Bus Off 状态更新回调（可选）
    bus_off_callback: Option<Arc<dyn Fn(bool) + Send + Sync>>,
    /// Error Passive 状态更新回调（可选）
    error_passive_callback: Option<Arc<dyn Fn(bool) + Send + Sync>>,
}

impl GsUsbRxAdapter {
    /// 创建新的 RX 适配器
    pub fn new(device: Arc<GsUsbDevice>, rx_timeout: Duration, mode: u32) -> Self {
        Self {
            device,
            rx_timeout,
            mode,
            // 关键：预分配容量，避免运行时扩容
            // 64 是经验值：足够应对突发，但不会浪费过多内存
            rx_queue: VecDeque::with_capacity(64),
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
            let gs_frames = match self.device.receive_batch(self.rx_timeout) {
                Ok(frames) => frames,
                Err(crate::can::gs_usb::error::GsUsbError::ReadTimeout) => {
                    return Err(CanError::Timeout);
                },
                Err(e) => {
                    let kind = match e {
                        crate::can::gs_usb::error::GsUsbError::Usb(rusb::Error::NoDevice) => {
                            CanDeviceErrorKind::NoDevice
                        },
                        crate::can::gs_usb::error::GsUsbError::Usb(rusb::Error::Access) => {
                            CanDeviceErrorKind::AccessDenied
                        },
                        crate::can::gs_usb::error::GsUsbError::Usb(rusb::Error::NotFound) => {
                            CanDeviceErrorKind::NotFound
                        },
                        crate::can::gs_usb::error::GsUsbError::InvalidFrame(_) => {
                            CanDeviceErrorKind::InvalidFrame
                        },
                        crate::can::gs_usb::error::GsUsbError::InvalidResponse { .. } => {
                            CanDeviceErrorKind::InvalidResponse
                        },
                        _ => CanDeviceErrorKind::Backend,
                    };
                    return Err(CanError::Device(CanDeviceError::new(
                        kind,
                        format!("USB receive failed: {}", e),
                    )));
                },
            };

            // 如果读取成功但没有帧（可能是空包），继续读下一个包
            if gs_frames.is_empty() {
                continue;
            }

            // ============================================================
            // Bus Off 检测 - 检测错误帧并触发回调
            // ============================================================
            for gs_frame in &gs_frames {
                // 1. 调试日志：非零 Flags（仅在 trace 级别）
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

                // 2. 检测错误帧（CAN ID 包含 CAN_ERR_FLAG 0x20000000）
                // 根据 Linux CAN 错误帧格式：
                // data[0] = CAN_ERR_FLAG (0x01) - 错误帧标志（某些设备可能不设置）
                // data[1] = Controller Error Status (CAN_ERR_CRTL_*)
                // data[2] = Protocol Error Type (CAN_ERR_PROT_*)
                // data[3] = Error Location (CAN_ERR_LOC_*)
                // data[4..7] = Extended error information
                let can_id_raw = gs_frame.can_id;
                if (can_id_raw & CAN_ERR_FLAG) != 0 {
                    // 首次检测到错误帧，确认设备支持 Bus Off 检测
                    // 通过回调通知 daemon 设备支持错误帧上报
                    // 注意：这里不能直接访问 DetailedStats，需要通过回调机制
                    // Error Passive 和 Bus Off 回调会在检测到时被调用，那里会标记设备支持
                    if gs_frame.can_dlc >= 2 {
                        // data[1] = Controller Error Status
                        let ctrl_err = gs_frame.data[1];

                        // 检测 Bus Off（最高优先级）
                        // Bus Off 在 data[1] 中检测，标准标志：
                        // CAN_ERR_CRTL_TX_BUS_OFF = 0x40 (bit 6) - TX Bus Off (TEC > 255)
                        // CAN_ERR_CRTL_RX_BUS_OFF = 0x80 (bit 7) - RX Bus Off (rare)
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

                            // 通过回调更新 Bus Off 状态
                            if let Some(ref callback) = self.bus_off_callback {
                                callback(true); // Bus Off 状态为 true
                            }
                        } else {
                            // 检测 Error Passive（Bus Off 之前的警告状态）
                            // Error Passive 在 data[1] 中检测：
                            // CAN_ERR_CRTL_RX_PASSIVE = 0x10 (bit 4) - RX Error Passive (REC > 127)
                            // CAN_ERR_CRTL_TX_PASSIVE = 0x20 (bit 5) - TX Error Passive (TEC > 127)
                            let is_rx_passive = (ctrl_err & CAN_ERR_CRTL_RX_PASSIVE) != 0;
                            let is_tx_passive = (ctrl_err & CAN_ERR_CRTL_TX_PASSIVE) != 0;

                            if is_rx_passive || is_tx_passive {
                                warn!(
                                    "CAN Error Passive detected (pre-Bus Off warning): RX: {}, TX: {}, Controller Error: 0x{:02x}, CAN ID: 0x{:08x}",
                                    is_rx_passive, is_tx_passive, ctrl_err, can_id_raw
                                );

                                // 通过回调更新 Error Passive 状态
                                if let Some(ref callback) = self.error_passive_callback {
                                    callback(true); // Error Passive 状态为 true
                                }
                            } else {
                                // 如果之前处于 Error Passive 状态，现在已恢复，触发回调
                                // 注意：这里无法直接判断"之前的状态"，回调应该处理状态变化
                                // 如果需要，可以在回调中实现状态机
                                // 当前实现：只在检测到 Error Passive 时调用 callback(true)
                                // 恢复检测需要额外逻辑或通过定期轮询实现
                            }

                            // 解析协议错误类型（data[2]），用于调试
                            if gs_frame.can_dlc >= 3 {
                                let prot_err = gs_frame.data[2];

                                // 记录格式错误（通常是波特率不匹配）
                                if (prot_err & CAN_ERR_PROT_FORM) != 0 {
                                    warn!(
                                        "CAN Format Error detected (possible bitrate mismatch): Protocol Error: 0x{:02x}, CAN ID: 0x{:08x}",
                                        prot_err, can_id_raw
                                    );
                                }

                                // 其他协议错误类型，记录但不处理（debug 级别）
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
                        }
                    } else {
                        // DLC 不足，无法解析错误状态（debug 级别）
                        debug!(
                            "CAN Error frame (DLC too short to parse): CAN ID: 0x{:08x}, DLC: {}, Data: {:?}",
                            can_id_raw,
                            gs_frame.can_dlc,
                            &gs_frame.data[..gs_frame.can_dlc.min(8) as usize]
                        );
                    }
                }
            }

            // 3. 过滤 Echo 帧（关键：双线程模式下会收到 TX 回显）
            let is_loopback = (self.mode & GS_CAN_MODE_LOOP_BACK) != 0;

            // 注意：这里需要移动 gs_frames，所以验证日志必须在前面完成
            for gs_frame in gs_frames {
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
                self.rx_queue.push_back(piper_frame);
            }

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
    fn convert_to_piper_frame(&self, gs_frame: &GsUsbFrame) -> Result<PiperFrame, CanError> {
        Ok(PiperFrame {
            id: gs_frame.can_id & CAN_EFF_MASK,
            is_extended: (gs_frame.can_id & CAN_EFF_FLAG) != 0,
            len: gs_frame.can_dlc,
            data: gs_frame.data,
            timestamp_us: gs_frame.timestamp_us as u64, // 保留硬件时间戳（u32 -> u64）
        })
    }
}

impl RxAdapter for GsUsbRxAdapter {
    fn receive(&mut self) -> Result<PiperFrame, CanError> {
        self.receive()
    }
}

/// 只写适配器（用于 TX 线程）
///
/// 独立的 TX 适配器，持有 `Arc<GsUsbDevice>`，
/// 可以在不同线程中与 `GsUsbRxAdapter` 并发使用。
pub struct GsUsbTxAdapter {
    device: Arc<GsUsbDevice>,
}

impl GsUsbTxAdapter {
    /// 创建新的 TX 适配器
    pub fn new(device: Arc<GsUsbDevice>) -> Self {
        Self { device }
    }

    /// 发送 CAN 帧
    pub fn send(&mut self, frame: PiperFrame) -> Result<(), CanError> {
        // 转换 PiperFrame -> GsUsbFrame
        let gs_frame = GsUsbFrame {
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
        };

        // 发送到 USB Endpoint OUT
        self.device.send_raw(&gs_frame).map_err(|e| {
            let kind = match e {
                crate::can::gs_usb::error::GsUsbError::WriteTimeout => CanDeviceErrorKind::Busy,
                crate::can::gs_usb::error::GsUsbError::Usb(rusb::Error::NoDevice) => {
                    CanDeviceErrorKind::NoDevice
                },
                crate::can::gs_usb::error::GsUsbError::Usb(rusb::Error::Access) => {
                    CanDeviceErrorKind::AccessDenied
                },
                crate::can::gs_usb::error::GsUsbError::Usb(rusb::Error::NotFound) => {
                    CanDeviceErrorKind::NotFound
                },
                _ => CanDeviceErrorKind::Backend,
            };
            CanError::Device(CanDeviceError::new(kind, format!("USB send failed: {}", e)))
        })
    }
}

impl TxAdapter for GsUsbTxAdapter {
    fn send(&mut self, frame: PiperFrame) -> Result<(), CanError> {
        self.send(frame)
    }
}
