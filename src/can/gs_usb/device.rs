//! GS-USB 设备操作
//!
//! 提供 USB 设备扫描、配置、数据传输等功能

use rusb::{DeviceHandle, GlobalContext};
use std::time::Duration;
use tracing::trace;

use crate::can::gs_usb::error::GsUsbError;
use crate::can::gs_usb::frame::GsUsbFrame;
use crate::can::gs_usb::protocol::*;

/// GS-USB 设备句柄
pub struct GsUsbDevice {
    handle: DeviceHandle<GlobalContext>,
    interface_number: u8,
    endpoint_in: u8,
    endpoint_out: u8,
    capability: Option<DeviceCapability>,
    last_timing: Option<DeviceBitTiming>,
    started: bool,
    /// 记录是否已经 claim 了接口（用于正确的资源清理）
    interface_claimed: bool,
    /// 是否启用硬件时间戳模式
    hw_timestamp: bool,
    /// 设备序列号（用于设备识别）
    serial_number: Option<String>,
}

impl GsUsbDevice {
    /// 检查是否为 GS-USB 设备
    fn is_gs_usb_device(vendor_id: u16, product_id: u16) -> bool {
        matches!(
            (vendor_id, product_id),
            (0x1D50, 0x606F)   // GS-USB
                | (0x1209, 0x2323)  // Candlelight
                | (0x1CD2, 0x606F)  // CES CANext FD
                | (0x16D0, 0x10B8) // ABE CANdebugger FD
        )
    }

    /// 扫描所有 GS-USB 设备
    pub fn scan() -> Result<Vec<GsUsbDevice>, GsUsbError> {
        Self::scan_with_filter(None)
    }

    /// 扫描所有 GS-USB 设备，可选地按序列号过滤
    ///
    /// # 参数
    /// - `serial_number_filter`: 可选的序列号过滤器，如果提供，只返回匹配序列号的设备
    ///
    /// # 注意
    /// - 如果设备没有序列号（序列号索引为 0 或读取失败），序列号字段将为 `None`
    /// - 如果提供了 `serial_number_filter`，只有序列号匹配的设备会被返回
    /// - 序列号匹配是大小写敏感的
    pub fn scan_with_filter(
        serial_number_filter: Option<&str>,
    ) -> Result<Vec<GsUsbDevice>, GsUsbError> {
        let mut devices = Vec::new();

        for device in rusb::devices()?.iter() {
            let desc = match device.device_descriptor() {
                Ok(desc) => desc,
                Err(_) => continue,
            };

            if Self::is_gs_usb_device(desc.vendor_id(), desc.product_id()) {
                let handle = match device.open() {
                    Ok(handle) => handle,
                    Err(_) => continue,
                };

                // 尝试读取序列号
                let serial_number = match desc.serial_number_string_index() {
                    Some(idx) if idx != 0 => {
                        match handle.read_string_descriptor_ascii(idx) {
                            Ok(serial) => {
                                // 如果提供了过滤器，检查是否匹配
                                if let Some(filter) = serial_number_filter
                                    && serial != filter
                                {
                                    continue; // 序列号不匹配，跳过此设备
                                }
                                Some(serial)
                            },
                            Err(_) => {
                                // 读取序列号失败，但如果提供了过滤器，必须匹配，所以跳过
                                if serial_number_filter.is_some() {
                                    continue;
                                }
                                None
                            },
                        }
                    },
                    _ => {
                        // 没有序列号，但如果提供了过滤器，必须匹配，所以跳过
                        if serial_number_filter.is_some() {
                            continue;
                        }
                        None
                    },
                };

                // 查找接口和端点
                let config_desc = match device.config_descriptor(0) {
                    Ok(config) => config,
                    Err(_) => continue,
                };

                let interface = match config_desc
                    .interfaces()
                    .next()
                    .and_then(|iface| iface.descriptors().next())
                {
                    Some(iface) => iface,
                    None => continue,
                };

                // GS-USB 设备通常只有一个接口，接口号为 0
                let interface_number = 0u8;

                // 查找 Bulk IN/OUT 端点
                let (endpoint_in, endpoint_out) = match Self::find_bulk_endpoints(&interface) {
                    Some((in_ep, out_ep)) => (in_ep, out_ep),
                    None => continue,
                };

                devices.push(GsUsbDevice {
                    handle,
                    interface_number,
                    endpoint_in,
                    endpoint_out,
                    capability: None,
                    last_timing: None,
                    started: false,
                    interface_claimed: false,
                    hw_timestamp: false,
                    serial_number,
                });
            }
        }

        Ok(devices)
    }

    /// 获取设备序列号
    pub fn serial_number(&self) -> Option<&str> {
        self.serial_number.as_deref()
    }

    /// 查找 Bulk IN/OUT 端点
    fn find_bulk_endpoints(interface: &rusb::InterfaceDescriptor) -> Option<(u8, u8)> {
        let mut endpoint_in = None;
        let mut endpoint_out = None;

        for endpoint in interface.endpoint_descriptors() {
            if endpoint.transfer_type() == rusb::TransferType::Bulk {
                let address = endpoint.address();
                if endpoint.direction() == rusb::Direction::In {
                    endpoint_in = Some(address);
                } else {
                    endpoint_out = Some(address);
                }
            }
        }

        match (endpoint_in, endpoint_out) {
            (Some(in_ep), Some(out_ep)) => Some((in_ep, out_ep)),
            _ => None,
        }
    }

    /// 发送 Host Format (0xBEEF) 进行握手
    ///
    /// **重要性**：虽然设备和主机都是 Little Endian，但这个请求充当：
    /// 1. 协议握手信号 - 某些固件在收到此命令前可能处于未初始化状态
    /// 2. 字节序配置 - 告知设备主机的字节序（虽然现代设备通常默认 LE）
    ///
    /// **策略**：Fire-and-Forget for Handshake
    /// - 尝试发送，但忽略错误（设备可能不支持或已默认 LE）
    /// - 即使失败也不阻断后续流程
    pub fn send_host_format(&self) -> Result<(), GsUsbError> {
        let val: u32 = 0x0000_BEEF;
        let data = val.to_le_bytes();

        // 短超时（100ms），忽略错误
        let _ = self.handle.write_control(
            GS_USB_REQ_OUT,
            GS_USB_BREQ_HOST_FORMAT,
            0, // Value（参考实现使用 0）
            0, // wIndex（大多数控制请求使用 0）
            &data,
            Duration::from_millis(100),
        );

        Ok(()) // 始终返回成功，不阻断流程
    }

    /// 准备接口（detach driver 和 claim interface）
    ///
    /// 提取为独立方法，以便在需要时提前调用（例如在 set_bitrate 之前）
    pub fn prepare_interface(&mut self) -> Result<(), GsUsbError> {
        // 如果接口已经 claim 了，跳过（避免重复 claim）
        if self.interface_claimed {
            return Ok(());
        }

        // 1. Detach kernel driver on Linux/macOS（在 claim 之前）
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            if self.handle.kernel_driver_active(self.interface_number).unwrap_or(false) {
                self.handle
                    .detach_kernel_driver(self.interface_number)
                    .map_err(GsUsbError::Usb)?;
            }
        }

        // 2. Claim interface（必须在 reset 之前，否则可能导致段错误）
        self.handle.claim_interface(self.interface_number).map_err(GsUsbError::Usb)?;

        self.interface_claimed = true;

        // 3. Reset 设备（参考实现在最前面，但我们先 claim interface 避免段错误）
        // 注意：某些设备需要在 reset 后才能正确响应控制传输
        // 但在 macOS 上，reset 必须在 claim interface 之后
        if let Err(e) = self.handle.reset() {
            trace!("Device reset failed (may be normal): {}", e);
            // 不立即返回错误，继续尝试后续步骤
        }

        // 4. 短暂延迟，让设备稳定（特别是 reset 后）
        std::thread::sleep(Duration::from_millis(100));

        Ok(())
    }

    /// 清除 USB 端点的 Halt 状态和 Data Toggle
    ///
    /// **重要性**：在 macOS 上，当程序非正常退出或超时后，USB 端点可能处于 Halt/Stall 状态，
    /// 或者 Host 和 Device 的 Data Toggle 不同步（Host 认为应该是 DATA0，Device 认为是 DATA1）。
    ///
    /// **现象**：主机发送数据成功（USB 物理层 ACK），但设备硬件检查 Data Toggle 位发现不对，
    /// 直接丢弃数据包，固件层收不到任何数据，自然不会返回 Echo。
    ///
    /// **解决方案**：`clear_halt()` 会强制将 Host 和 Device 的 Data Toggle 都重置为 DATA0，
    /// 并清除端点的 Halt 状态，让双方重新握手。
    ///
    /// **调用时机**：在 `prepare_interface()` 之后，执行任何 USB 传输之前调用。
    pub fn clear_usb_endpoints(&mut self) -> Result<(), GsUsbError> {
        // 清除 IN 端点（接收端）
        if let Err(e) = self.handle.clear_halt(self.endpoint_in) {
            trace!("Failed to clear halt on IN endpoint: {}", e);
            // 不立即返回错误，尝试继续清除 OUT 端点
        }

        // 清除 OUT 端点（发送端）
        if let Err(e) = self.handle.clear_halt(self.endpoint_out) {
            trace!("Failed to clear halt on OUT endpoint: {}", e);
            // 即使失败也继续，因为这可能是端点尚未初始化的正常情况
        }

        // 短暂延迟，让端点状态稳定
        std::thread::sleep(Duration::from_millis(10));

        Ok(())
    }

    /// 释放 USB 接口（交还给操作系统）
    ///
    /// **重要性**：在 Drop 时释放接口是 Rust 管理硬件资源的关键。
    /// 如果不释放接口，操作系统（特别是 macOS/Linux）可能会认为该接口仍被占用，
    /// 导致下次启动时无法 claim 接口（Access denied）。
    ///
    /// 这会强制复位 host 端的 USB 状态机（Data Toggle 等），防止状态残留。
    pub fn release_interface(&mut self) {
        if self.interface_claimed {
            // 忽略错误，因为我们是在销毁过程中
            // 即使失败（例如设备已断开），也不应该 panic
            let _ = self.handle.release_interface(self.interface_number);
            self.interface_claimed = false;
            trace!("[Release] USB Interface released");
        }
    }

    /// 启动设备
    pub fn start(&mut self, flags: u32) -> Result<(), GsUsbError> {
        // 1. 准备接口（如果还未声明）
        // 注意：prepare_interface() 内部会处理 detach_kernel_driver, claim_interface, reset
        // 如果接口已声明，则跳过（避免重复操作）
        let _ = self.prepare_interface();

        // 2. 获取设备能力（检查功能支持）
        let capability = self.device_capability()?;

        // 3. 过滤 flags：只保留设备支持的功能
        let mut flags = flags & capability.feature;

        // 4. 过滤 flags：只保留驱动支持的功能
        // 我们的驱动支持 CAN 2.0 和硬件时间戳，不支持 CAN FD 等高级功能
        flags &= GS_CAN_MODE_LISTEN_ONLY
            | GS_CAN_MODE_LOOP_BACK
            | GS_CAN_MODE_NORMAL
            | GS_CAN_MODE_HW_TIMESTAMP;
        // 注意：不包含 GS_CAN_MODE_FD, GS_CAN_MODE_ONE_SHOT 等
        // 因为我们只支持经典 CAN 2.0

        // 5. 记录是否启用硬件时间戳
        self.hw_timestamp = (flags & GS_CAN_MODE_HW_TIMESTAMP) != 0;

        // 6. 设置模式并启动
        let mode = DeviceMode::new(GS_CAN_MODE_START, flags);
        self.control_out(GS_USB_BREQ_MODE, 0, &mode.pack())?;

        self.started = true;
        trace!("GS-USB device started with flags: 0x{:08x}", flags);
        Ok(())
    }

    /// 停止设备
    pub fn stop(&mut self) -> Result<(), GsUsbError> {
        let mode = DeviceMode::new(GS_CAN_MODE_RESET, 0);
        // 忽略错误（设备可能已经停止）
        let _ = self.control_out(GS_USB_BREQ_MODE, 0, &mode.pack());
        self.started = false;
        trace!("GS-USB device stopped");
        Ok(())
    }

    /// 设置 CAN 波特率
    ///
    /// 使用预定义的波特率映射表（80MHz、48MHz 和 40MHz 时钟）
    ///
    /// 如果无法查询设备能力，将使用默认时钟（48MHz）作为 fallback
    pub fn set_bitrate(&mut self, bitrate: u32) -> Result<(), GsUsbError> {
        // 尝试获取设备能力，如果失败则使用默认时钟（48MHz）
        let clock = match self.device_capability() {
            Ok(cap) => cap.fclk_can,
            Err(e) => {
                trace!(
                    "Failed to get device capability, using default clock (48MHz): {}",
                    e
                );
                // 使用默认时钟：48MHz（Candlelight / STM32-based devices 最常见）
                48_000_000
            },
        };

        // 获取位定时参数（基于时钟频率）
        // 公式：bitrate = clock_hz / (brp * (1 + prop_seg + phase_seg1 + phase_seg2))
        let timing = match clock {
            // 80 MHz clock (PEAK Systems)
            80_000_000 => match bitrate {
                10_000 => Some((87, 87, 25, 12, 40)),
                20_000 => Some((87, 87, 25, 12, 20)),
                50_000 => Some((87, 87, 25, 12, 8)),
                100_000 => Some((87, 87, 25, 12, 4)),
                125_000 => Some((69, 70, 20, 10, 4)),
                250_000 => Some((69, 70, 20, 10, 2)),
                500_000 => Some((69, 70, 20, 10, 1)),
                1_000_000 => Some((29, 30, 20, 10, 1)),
                _ => None,
            },
            // 48 MHz clock (Candlelight / STM32-based devices)
            48_000_000 => match bitrate {
                10_000 => Some((87, 87, 25, 12, 24)),
                20_000 => Some((87, 87, 25, 12, 12)),
                50_000 => Some((87, 87, 25, 12, 5)),
                100_000 => Some((87, 87, 25, 12, 2)),
                125_000 => Some((69, 70, 20, 10, 2)),
                250_000 => Some((69, 70, 20, 10, 1)),
                500_000 => Some((34, 35, 10, 5, 1)),
                1_000_000 => Some((14, 15, 10, 5, 1)),
                _ => None,
            },
            // 40 MHz clock (CF3 / candleLight)
            40_000_000 => match bitrate {
                10_000 => Some((87, 87, 25, 12, 20)),
                20_000 => Some((87, 87, 25, 12, 10)),
                50_000 => Some((87, 87, 25, 12, 4)),
                100_000 => Some((87, 87, 25, 12, 2)),
                125_000 => Some((69, 70, 20, 10, 2)),
                250_000 => Some((69, 70, 20, 10, 1)),
                500_000 => Some((34, 35, 10, 5, 1)),
                1_000_000 => Some((14, 15, 10, 5, 1)),
                _ => None,
            },
            _ => None,
        };

        match timing {
            Some((prop_seg, phase_seg1, phase_seg2, sjw, brp)) => {
                self.set_timing(prop_seg, phase_seg1, phase_seg2, sjw, brp)
            },
            None => Err(GsUsbError::UnsupportedBitrate {
                bitrate,
                clock_hz: clock,
            }),
        }
    }

    /// 设置原始 CAN 位定时参数
    pub fn set_timing(
        &mut self,
        prop_seg: u32,
        phase_seg1: u32,
        phase_seg2: u32,
        sjw: u32,
        brp: u32,
    ) -> Result<(), GsUsbError> {
        let timing = DeviceBitTiming::new(prop_seg, phase_seg1, phase_seg2, sjw, brp);
        self.control_out(GS_USB_BREQ_BITTIMING, 0, &timing.pack())?;
        self.last_timing = Some(timing);
        trace!(
            "Set bit timing: prop_seg={}, phase_seg1={}, phase_seg2={}, sjw={}, brp={}",
            prop_seg, phase_seg1, phase_seg2, sjw, brp
        );
        Ok(())
    }

    /// 获取设备能力
    pub fn device_capability(&mut self) -> Result<DeviceCapability, GsUsbError> {
        if let Some(ref cap) = self.capability {
            return Ok(*cap);
        }

        let data = self.control_in(GS_USB_BREQ_BT_CONST, 0, 40)?;
        let cap = DeviceCapability::unpack(&data);
        self.capability = Some(cap);
        Ok(cap)
    }

    /// 发送原始 GS-USB 帧（Fire-and-Forget）
    ///
    /// **错误恢复**：如果 USB 批量传输超时，endpoint 可能进入 STALL 状态。
    /// 超时后会自动清除 endpoint halt，恢复设备状态，避免需要重新插拔设备。
    pub fn send_raw(&self, frame: &GsUsbFrame) -> Result<(), GsUsbError> {
        let mut buf = bytes::BytesMut::new();
        frame.pack_to(&mut buf, self.hw_timestamp);

        // **关键修复**：增加发送超时时间
        // 在 Loopback 模式下，设备 CPU 负载很高（收->拷->发），USB 控制器可能返回 NAK
        // 如果超时设置太短，会导致偶发的 Write timeout 错误
        // 注意：这里的超时是最大允许等待时间，正常情况下传输是微秒级的
        // 只有在设备忙碌时才会等待，所以增加超时不会影响正常吞吐量
        // **超时设置**：1000ms (1秒)
        // 在 Loopback 模式下，设备 CPU 负载很高，需要足够的超时时间
        // 这不会影响正常吞吐量（正常传输是微秒级），只是给设备忙碌时的时间
        match self.handle.write_bulk(self.endpoint_out, &buf, Duration::from_millis(1000)) {
            Ok(_) => Ok(()),
            Err(rusb::Error::Timeout) => {
                // USB 批量传输超时后，endpoint 可能进入 STALL 状态
                // 必须清除 halt 才能恢复设备，否则后续操作会失败
                use tracing::warn;
                if let Err(clear_err) = self.handle.clear_halt(self.endpoint_out) {
                    warn!("Failed to clear endpoint halt after timeout: {}", clear_err);
                } else {
                    // 清除成功后，延迟让设备恢复
                    // 注意：某些设备可能需要更长的恢复时间，特别是连续超时后
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(GsUsbError::WriteTimeout)
            },
            Err(e) => Err(GsUsbError::Usb(e)),
        }
    }

    /// 接收原始 GS-USB 帧（带超时）
    ///
    /// **注意**：此方法只读取 USB 包中的第一个帧。如果 USB 包包含多个帧，后续帧会被丢弃。
    /// 对于高吞吐量场景，请使用 `receive_batch()` 方法。
    pub fn receive_raw(&self, timeout: Duration) -> Result<GsUsbFrame, GsUsbError> {
        let frame_size = if self.hw_timestamp {
            GS_USB_FRAME_SIZE_HW_TIMESTAMP
        } else {
            GS_USB_FRAME_SIZE
        };
        let mut buf = vec![0u8; frame_size];

        let len = match self.handle.read_bulk(self.endpoint_in, &mut buf, timeout) {
            Ok(len) => len,
            Err(rusb::Error::Timeout) => return Err(GsUsbError::ReadTimeout),
            Err(e) => return Err(GsUsbError::Usb(e)),
        };

        if len < frame_size {
            return Err(GsUsbError::InvalidFrame(format!(
                "Frame too short: {} bytes (expected at least {})",
                len, frame_size
            )));
        }

        let mut frame = GsUsbFrame::default();
        frame.unpack_from_bytes(bytes::Bytes::from(buf), self.hw_timestamp)?;

        Ok(frame)
    }

    /// 批量接收：读取一个 USB Bulk 包，并解析其中所有帧
    ///
    /// **关键修复**：USB 硬件会将多个 CAN 帧打包在一个 USB Bulk 包中发送（例如 64 或 512 字节）。
    /// 此方法一次性读取整个 USB 包，并解析出其中所有的 GS-USB 帧。
    ///
    /// **返回**：包含所有解析出的帧的 Vec。如果 USB 包为空或只包含部分帧，返回相应数量的帧。
    pub fn receive_batch(&self, timeout: Duration) -> Result<Vec<GsUsbFrame>, GsUsbError> {
        // 准备一个足够大的 Buffer（USB Bulk 包通常是 64 或 512 字节）
        // 假设每个 GS-USB 帧是 20-24 字节，4096 字节可以容纳 200+ 帧（足够安全）
        let mut buf = vec![0u8; 4096];

        // 读取 USB Bulk IN
        let len = match self.handle.read_bulk(self.endpoint_in, &mut buf, timeout) {
            Ok(len) => len,
            Err(rusb::Error::Timeout) => return Err(GsUsbError::ReadTimeout),
            Err(e) => return Err(GsUsbError::Usb(e)),
        };

        // 如果没有数据，返回空 Vec
        if len == 0 {
            return Ok(Vec::new());
        }

        // 根据是否启用硬件时间戳确定帧大小
        let frame_size = if self.hw_timestamp {
            GS_USB_FRAME_SIZE_HW_TIMESTAMP
        } else {
            GS_USB_FRAME_SIZE
        };

        // 如果数据长度不是帧大小的整数倍，可能是最后一个不完整的帧
        // 我们只解析完整的帧
        let mut frames = Vec::new();
        let mut offset = 0;

        // GS-USB 协议：帧是连续紧凑排列的，每个帧固定大小（20 或 24 字节）
        while offset + frame_size <= len {
            // 从缓冲区中提取一个帧的字节
            let frame_bytes = &buf[offset..offset + frame_size];

            // 解析帧
            let mut frame = GsUsbFrame::default();
            frame.unpack_from_bytes(
                bytes::Bytes::copy_from_slice(frame_bytes),
                self.hw_timestamp,
            )?;

            frames.push(frame);
            offset += frame_size;
        }

        // 如果还有剩余数据（不完整的帧），记录警告但不报错
        // 这可能表明设备固件或 USB 传输有问题
        if offset < len {
            use tracing::warn;
            warn!(
                "USB packet contains incomplete frame: {} bytes (expected multiple of {})",
                len, frame_size
            );
        }

        Ok(frames)
    }

    /// 执行控制 OUT 传输
    fn control_out(&self, request: u8, value: u16, data: &[u8]) -> Result<(), GsUsbError> {
        self.handle
            .write_control(
                GS_USB_REQ_OUT,
                request,
                value,
                0, // wIndex
                data,
                Duration::from_millis(1000),
            )
            .map_err(GsUsbError::Usb)?;
        Ok(())
    }

    /// 执行控制 IN 传输
    fn control_in(&self, request: u8, value: u16, length: usize) -> Result<Vec<u8>, GsUsbError> {
        let mut buf = vec![0u8; length];
        let len = self
            .handle
            .read_control(
                GS_USB_REQ_IN,
                request,
                value,
                0, // wIndex
                &mut buf,
                Duration::from_millis(1000),
            )
            .map_err(GsUsbError::Usb)?;

        if len < length {
            return Err(GsUsbError::InvalidResponse {
                expected: length,
                actual: len,
            });
        }

        Ok(buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_gs_usb_device() {
        // 测试已知的 VID/PID
        assert!(GsUsbDevice::is_gs_usb_device(0x1D50, 0x606F)); // GS-USB
        assert!(GsUsbDevice::is_gs_usb_device(0x1209, 0x2323)); // Candlelight
        assert!(!GsUsbDevice::is_gs_usb_device(0x1234, 0x5678)); // 未知设备
    }

    #[test]
    fn test_send_host_format_format() {
        // 验证 HOST_FORMAT 的数据格式
        let val: u32 = 0x0000_BEEF;
        let data = val.to_le_bytes();

        // 在 little-endian 系统上应该是 [0xEF, 0xBE, 0x00, 0x00]
        assert_eq!(data[0], 0xEF);
        assert_eq!(data[1], 0xBE);
        assert_eq!(data[2], 0x00);
        assert_eq!(data[3], 0x00);
    }

    // 注意：scan() 和实际 USB 操作的测试需要硬件，放在集成测试中
}
