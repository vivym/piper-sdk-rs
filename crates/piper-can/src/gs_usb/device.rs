//! GS-USB 设备操作
//!
//! 提供 USB 设备扫描、配置、数据传输等功能

use rusb::{DeviceHandle, GlobalContext};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::trace;

use crate::gs_usb::error::GsUsbError;
use crate::gs_usb::frame::GsUsbFrame;
use crate::gs_usb::protocol::*;

#[cfg(test)]
use std::collections::VecDeque;
#[cfg(test)]
use std::sync::Mutex;
#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};

pub(crate) const GS_USB_READ_BUFFER_SIZE: usize = 4096;
pub(crate) const GS_USB_BATCH_FRAME_CAPACITY: usize = GS_USB_READ_BUFFER_SIZE / GS_USB_FRAME_SIZE;

/// 轻量的设备枚举信息（不持有 USB 句柄）
#[derive(Debug, Clone)]
pub struct GsUsbDeviceInfo {
    pub vendor_id: u16,
    pub product_id: u16,
    pub bus_number: u8,
    pub address: u8,
    pub serial_number: Option<String>,
}

/// 打开设备时的选择器（避免枚举阶段持有 handle）
#[derive(Debug, Clone, Default)]
pub struct GsUsbDeviceSelector {
    pub serial_number: Option<String>,
    pub bus_number: Option<u8>,
    pub address: Option<u8>,
}

impl GsUsbDeviceSelector {
    pub fn any() -> Self {
        Self::default()
    }

    pub fn by_serial(serial: impl Into<String>) -> Self {
        Self {
            serial_number: Some(serial.into()),
            bus_number: None,
            address: None,
        }
    }

    pub fn by_bus_address(bus_number: u8, address: u8) -> Self {
        Self {
            serial_number: None,
            bus_number: Some(bus_number),
            address: Some(address),
        }
    }
}

/// `start()` 的协商结果（用于让上层可见“最终生效配置”）
#[derive(Debug, Clone, Copy)]
pub struct StartResult {
    /// 设备能力（来自 BT_CONST）
    pub capability: DeviceCapability,
    /// 传入 flags 在 capability/驱动支持过滤后的最终生效值
    pub effective_flags: u32,
    /// 是否启用硬件时间戳（由 effective_flags 决定）
    pub hw_timestamp: bool,
}

#[cfg(test)]
#[derive(Clone, Debug, Default)]
pub(crate) struct TestUsbHandleHarness {
    stop_requests: Arc<AtomicUsize>,
    interface_releases: Arc<AtomicUsize>,
    read_packets: Arc<Mutex<VecDeque<Vec<u8>>>>,
}

#[cfg(test)]
impl TestUsbHandleHarness {
    pub(crate) fn stop_requests(&self) -> usize {
        self.stop_requests.load(Ordering::Relaxed)
    }

    pub(crate) fn interface_releases(&self) -> usize {
        self.interface_releases.load(Ordering::Relaxed)
    }

    pub(crate) fn enqueue_read_packet(&self, packet: Vec<u8>) {
        self.read_packets.lock().expect("poisoned test queue").push_back(packet);
    }
}

#[cfg(test)]
#[derive(Clone, Debug)]
struct TestUsbHandle {
    harness: TestUsbHandleHarness,
}

#[cfg(test)]
impl TestUsbHandle {
    fn new(harness: TestUsbHandleHarness) -> Self {
        Self { harness }
    }
}

enum UsbHandle {
    Real(DeviceHandle<GlobalContext>),
    #[cfg(test)]
    Test(TestUsbHandle),
}

#[allow(clippy::too_many_arguments)]
impl UsbHandle {
    fn kernel_driver_active(&self, interface_number: u8) -> Result<bool, rusb::Error> {
        match self {
            Self::Real(handle) => handle.kernel_driver_active(interface_number),
            #[cfg(test)]
            Self::Test(_) => Ok(false),
        }
    }

    fn detach_kernel_driver(&self, interface_number: u8) -> Result<(), rusb::Error> {
        match self {
            Self::Real(handle) => handle.detach_kernel_driver(interface_number),
            #[cfg(test)]
            Self::Test(_) => Ok(()),
        }
    }

    fn claim_interface(&self, interface_number: u8) -> Result<(), rusb::Error> {
        match self {
            Self::Real(handle) => handle.claim_interface(interface_number),
            #[cfg(test)]
            Self::Test(_) => Ok(()),
        }
    }

    fn release_interface(&self, interface_number: u8) -> Result<(), rusb::Error> {
        match self {
            Self::Real(handle) => handle.release_interface(interface_number),
            #[cfg(test)]
            Self::Test(handle) => {
                let _ = interface_number;
                handle.harness.interface_releases.fetch_add(1, Ordering::Relaxed);
                Ok(())
            },
        }
    }

    fn reset(&self) -> Result<(), rusb::Error> {
        match self {
            Self::Real(handle) => handle.reset(),
            #[cfg(test)]
            Self::Test(_) => Ok(()),
        }
    }

    fn write_bulk(
        &self,
        endpoint: u8,
        data: &[u8],
        timeout: Duration,
    ) -> Result<usize, rusb::Error> {
        match self {
            Self::Real(handle) => handle.write_bulk(endpoint, data, timeout),
            #[cfg(test)]
            Self::Test(_) => {
                let _ = (endpoint, timeout);
                Ok(data.len())
            },
        }
    }

    fn read_bulk(
        &self,
        endpoint: u8,
        buf: &mut [u8],
        timeout: Duration,
    ) -> Result<usize, rusb::Error> {
        match self {
            Self::Real(handle) => handle.read_bulk(endpoint, buf, timeout),
            #[cfg(test)]
            Self::Test(handle) => {
                let _ = (endpoint, timeout);
                let mut packets = handle.harness.read_packets.lock().expect("poisoned test queue");
                let Some(packet) = packets.pop_front() else {
                    return Err(rusb::Error::Timeout);
                };
                let len = packet.len().min(buf.len());
                buf[..len].copy_from_slice(&packet[..len]);
                Ok(len)
            },
        }
    }

    fn write_control(
        &self,
        request_type: u8,
        request: u8,
        value: u16,
        index: u16,
        data: &[u8],
        timeout: Duration,
    ) -> Result<usize, rusb::Error> {
        match self {
            Self::Real(handle) => {
                handle.write_control(request_type, request, value, index, data, timeout)
            },
            #[cfg(test)]
            Self::Test(handle) => {
                let _ = (request_type, value, index, timeout);
                let reset_mode = DeviceMode::new(GS_CAN_MODE_RESET, 0).pack();
                if request == GS_USB_BREQ_MODE && data == reset_mode.as_slice() {
                    handle.harness.stop_requests.fetch_add(1, Ordering::Relaxed);
                }
                Ok(data.len())
            },
        }
    }

    fn read_control(
        &self,
        request_type: u8,
        request: u8,
        value: u16,
        index: u16,
        buf: &mut [u8],
        timeout: Duration,
    ) -> Result<usize, rusb::Error> {
        match self {
            Self::Real(handle) => {
                handle.read_control(request_type, request, value, index, buf, timeout)
            },
            #[cfg(test)]
            Self::Test(_) => {
                let _ = (request_type, request, value, index, buf, timeout);
                Err(rusb::Error::NotSupported)
            },
        }
    }
}

/// GS-USB 设备句柄
///
/// `GsUsbDevice` 本身会在 split 后被 `Arc` 共享；底层 USB handle 通过内部封装统一访问，
/// 既保留生产路径的真实 `rusb::DeviceHandle`，也允许在单测里注入轻量测试替身。
pub struct GsUsbDevice {
    handle: UsbHandle,
    vendor_id: u16,
    product_id: u16,
    bus_number: u8,
    address: u8,
    interface_number: u8,
    endpoint_in: u8,
    endpoint_out: u8,
    capability: Option<DeviceCapability>,
    /// 记录是否已经 claim 了接口（用于正确的资源清理）
    interface_claimed: bool,
    /// 设备是否已经进入 START 状态
    started: bool,
    /// 是否启用硬件时间戳模式
    hw_timestamp: bool,
    /// 设备序列号（用于设备识别）
    serial_number: Option<String>,
    /// USB Bulk OUT 发送超时（支持实时模式）
    write_timeout: Duration,
    /// USB STALL 计数回调（可选）
    stall_count_callback: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl GsUsbDevice {
    pub fn hw_timestamp_enabled(&self) -> bool {
        self.hw_timestamp
    }

    #[cfg(test)]
    pub(crate) fn new_test_device(
        started: bool,
        interface_claimed: bool,
    ) -> (GsUsbDevice, TestUsbHandleHarness) {
        let harness = TestUsbHandleHarness::default();
        let device = GsUsbDevice {
            handle: UsbHandle::Test(TestUsbHandle::new(harness.clone())),
            vendor_id: 0x1D50,
            product_id: 0x606F,
            bus_number: 1,
            address: 1,
            interface_number: 0,
            endpoint_in: 0x81,
            endpoint_out: 0x01,
            capability: None,
            interface_claimed,
            started,
            hw_timestamp: false,
            serial_number: Some("test-gs-usb".to_string()),
            write_timeout: Duration::from_millis(1),
            stall_count_callback: None,
        };
        (device, harness)
    }

    fn usb_timeout_from_deadline(deadline: Instant, now: Instant) -> Result<Duration, GsUsbError> {
        let Some(remaining) = deadline.checked_duration_since(now) else {
            return Err(GsUsbError::WriteTimeout);
        };

        if remaining.as_millis() == 0 {
            return Err(GsUsbError::WriteTimeout);
        }

        Ok(Duration::from_millis(remaining.as_millis() as u64))
    }

    fn validate_bulk_write(transferred: usize, expected: usize) -> Result<(), GsUsbError> {
        if transferred == expected {
            return Ok(());
        }

        if transferred == 0 {
            return Err(GsUsbError::WriteTimeout);
        }

        Err(GsUsbError::PartialWrite {
            transferred,
            expected,
        })
    }

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

    /// 打开一个 GS-USB 设备（按选择器匹配）
    ///
    /// - 若 `selector.serial_number` 存在，则仅打开 serial 匹配的设备（大小写敏感，与 scan 逻辑一致）
    /// - 若 `selector.bus_number/address` 存在，则仅打开匹配 bus/address 的设备
    /// - 若都未指定，则打开找到的第一个 GS-USB 设备
    pub fn open(selector: &GsUsbDeviceSelector) -> Result<GsUsbDevice, GsUsbError> {
        for device in rusb::devices()?.iter() {
            let desc = match device.device_descriptor() {
                Ok(desc) => desc,
                Err(_) => continue,
            };

            let vendor_id = desc.vendor_id();
            let product_id = desc.product_id();
            if !Self::is_gs_usb_device(vendor_id, product_id) {
                continue;
            }

            // bus/address 过滤（如果指定）
            if let Some(bus) = selector.bus_number
                && let Some(addr) = selector.address
                && (device.bus_number() != bus || device.address() != addr)
            {
                continue;
            }

            let bus_number = device.bus_number();
            let address = device.address();

            // 打开 handle（后续还需要读取 serial / 查端点）
            let handle = match device.open() {
                Ok(handle) => handle,
                Err(_) => continue,
            };

            // 读取 serial（如需要）
            let serial_number = match desc.serial_number_string_index() {
                Some(idx) if idx != 0 => handle.read_string_descriptor_ascii(idx).ok(),
                _ => None,
            };

            if let Some(filter) = selector.serial_number.as_deref()
                && serial_number.as_deref() != Some(filter)
            {
                continue;
            }

            // 查找接口和端点（沿用 scan_with_filter 的逻辑）
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

            let interface_number = 0u8;
            let (endpoint_in, endpoint_out) = match Self::find_bulk_endpoints(&interface) {
                Some((in_ep, out_ep)) => (in_ep, out_ep),
                None => continue,
            };

            return Ok(GsUsbDevice {
                handle: UsbHandle::Real(handle),
                vendor_id,
                product_id,
                bus_number,
                address,
                interface_number,
                endpoint_in,
                endpoint_out,
                capability: None,
                interface_claimed: false,
                started: false,
                hw_timestamp: false,
                serial_number,
                write_timeout: Duration::from_millis(1000), // 默认 1000ms（向后兼容）
                stall_count_callback: None,                 // USB STALL 计数回调（可选）
            });
        }

        Err(GsUsbError::DeviceNotFound)
    }

    /// 扫描设备信息（不持有 USB 句柄），可选按序列号过滤
    ///
    /// 说明：
    /// - 为了读取序列号，仍需要短暂 open handle 读取 descriptor；读取完成后立即释放，不返回持有 handle 的对象。
    /// - 适用于 daemon/CLI 的“列出设备”与选择逻辑，避免枚举阶段占用设备资源。
    pub fn scan_info_with_filter(
        serial_number_filter: Option<&str>,
    ) -> Result<Vec<GsUsbDeviceInfo>, GsUsbError> {
        let mut infos = Vec::new();

        for device in rusb::devices()?.iter() {
            let desc = match device.device_descriptor() {
                Ok(desc) => desc,
                Err(_) => continue,
            };

            let vendor_id = desc.vendor_id();
            let product_id = desc.product_id();
            if !Self::is_gs_usb_device(vendor_id, product_id) {
                continue;
            }

            // 尝试读取序列号（需要短暂 open）
            let serial_number = match device.open() {
                Ok(handle) => match desc.serial_number_string_index() {
                    Some(idx) if idx != 0 => handle.read_string_descriptor_ascii(idx).ok(),
                    _ => None,
                },
                Err(_) => None,
            };

            // 过滤（大小写敏感，与 scan_with_filter 保持一致）
            if let Some(filter) = serial_number_filter
                && serial_number.as_deref() != Some(filter)
            {
                continue;
            }

            infos.push(GsUsbDeviceInfo {
                vendor_id,
                product_id,
                bus_number: device.bus_number(),
                address: device.address(),
                serial_number,
            });
        }

        Ok(infos)
    }

    /// 扫描设备信息（不持有 USB 句柄）
    pub fn scan_info() -> Result<Vec<GsUsbDeviceInfo>, GsUsbError> {
        Self::scan_info_with_filter(None)
    }

    /// 获取设备序列号
    pub fn serial_number(&self) -> Option<&str> {
        self.serial_number.as_deref()
    }

    /// 设备 VID
    pub fn vendor_id(&self) -> u16 {
        self.vendor_id
    }

    /// 设备 PID
    pub fn product_id(&self) -> u16 {
        self.product_id
    }

    /// USB bus number
    pub fn bus_number(&self) -> u8 {
        self.bus_number
    }

    /// USB address
    pub fn address(&self) -> u8 {
        self.address
    }

    /// 设置 USB Bulk OUT 发送超时（支持实时模式）
    ///
    /// # 参数
    /// - `timeout`: 发送超时时间
    ///   - 默认模式：1000ms（适合高负载/loopback 场景，提高可靠性）
    ///   - 实时模式：5ms（快速失败，适合力控场景）
    ///
    /// # 使用场景
    /// - **实时模式（5ms）**：力控/高频控制，需要快速失败而非长阻塞
    /// - **默认模式（1000ms）**：状态监控/调试，更可靠但可能阻塞
    pub fn set_write_timeout(&mut self, timeout: Duration) {
        self.write_timeout = timeout;
    }

    /// 获取当前写超时设置
    pub fn write_timeout(&self) -> Duration {
        self.write_timeout
    }

    /// 设置 USB STALL 计数回调
    ///
    /// 当设备发生 USB STALL 并被成功清除时，会调用此回调。
    /// 用于统计 STALL 事件，帮助诊断 USB 通信问题。
    ///
    /// # 参数
    /// - `callback`: 回调函数，在 STALL 清除成功时调用
    pub fn set_stall_count_callback<F>(&mut self, callback: F)
    where
        F: Fn() + Send + Sync + 'static,
    {
        self.stall_count_callback = Some(Arc::new(callback));
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

    /// 确保接口已 detach/claim，供控制传输使用（内部辅助）
    fn ensure_interface_claimed(&mut self) -> Result<(), GsUsbError> {
        // 如果接口已经 claim 了，跳过
        if self.interface_claimed {
            return Ok(());
        }

        // 如果 kernel driver 是 active 的，先 detach（与推荐启动流程一致）
        // 注意：detach_kernel_driver() 在 reset() 之后执行
        // 但为了确保 set_bitrate() 能成功，我们在这里也处理
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            if self.handle.kernel_driver_active(self.interface_number).unwrap_or(false) {
                self.handle
                    .detach_kernel_driver(self.interface_number)
                    .map_err(GsUsbError::Usb)?;
            }
        }

        // 然后 claim interface
        self.handle.claim_interface(self.interface_number).map_err(GsUsbError::Usb)?;
        self.interface_claimed = true;
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
    ///
    /// 推荐的 start() 行为（与参考实现一致）：
    /// 1. reset() - 重置设备
    /// 2. detach_kernel_driver() - 在 Linux/Unix 上 detach kernel driver（在 reset 之后）
    /// 3. 获取 device_capability - 检查设备支持的功能
    /// 4. 过滤 flags - 只保留设备支持的功能
    /// 5. 发送 MODE 命令 - 启动设备
    ///
    /// 注意：start() 内部会 reset，但 reset 不会清除之前设置的 bitrate
    /// 因为 bitrate 是通过控制请求设置的，是持久化配置
    pub fn start(&mut self, flags: u32) -> Result<StartResult, GsUsbError> {
        // 推荐流程：reset() -> detach_kernel_driver() -> 获取 capability -> 过滤 flags -> 发送 MODE

        // 1. Reset 设备（start() 内部 reset，最前面）
        // 注意：reset 不会清除之前设置的 bitrate，因为 bitrate 是持久化配置
        // 但 reset 可能会清除接口 claim 状态，所以需要在 reset 后重新处理接口
        if let Err(e) = self.handle.reset() {
            trace!("Device reset failed (may be normal): {}", e);
            // 不立即返回错误，继续尝试后续步骤
        }

        // 2. Detach kernel driver on Linux/macOS（在 reset 之后）
        // 注意：detach_kernel_driver() 在 reset() 之后执行
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            // reset 后，接口状态可能被清除，需要检查并重新处理
            // 检查 kernel driver 是否 active，如果是，说明接口状态被 reset 清除了
            let kernel_driver_active =
                self.handle.kernel_driver_active(self.interface_number).unwrap_or(false);

            if kernel_driver_active {
                // Kernel driver 是 active 的，说明 reset 清除了接口状态
                // 需要 detach 和 claim（与推荐流程一致）
                tracing::info!(
                    "Detaching kernel driver for GS-USB device to enable userspace mode. \
                     Note: CAN network interface (can0) will temporarily disappear."
                );
                self.interface_claimed = false;
                self.handle
                    .detach_kernel_driver(self.interface_number)
                    .map_err(GsUsbError::Usb)?;
            }

            // 如果接口未 claim，需要 claim（可能在 reset 前已 claim，但 reset 后状态被清除）
            if !self.interface_claimed {
                self.handle.claim_interface(self.interface_number).map_err(GsUsbError::Usb)?;
                self.interface_claimed = true;
            }
        }

        // 3. 短暂延迟，让设备稳定（特别是 reset 后）
        std::thread::sleep(Duration::from_millis(100));

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

        trace!(
            "GS-USB device started with flags: 0x{:08x}, hw_timestamp={}",
            flags, self.hw_timestamp
        );
        self.started = true;
        Ok(StartResult {
            capability,
            effective_flags: flags,
            hw_timestamp: self.hw_timestamp,
        })
    }

    /// 停止设备
    pub fn stop(&mut self) -> Result<(), GsUsbError> {
        if !self.started {
            return Ok(());
        }
        let mode = DeviceMode::new(GS_CAN_MODE_RESET, 0);
        // 忽略错误（设备可能已经停止）
        let _ = self.control_out(GS_USB_BREQ_MODE, 0, &mode.pack());
        self.started = false;
        trace!("GS-USB device stopped");
        Ok(())
    }

    /// 设置 CAN 波特率
    ///
    /// 使用预定义的波特率映射表（推荐表，sample point 87.5%）
    ///
    /// 关键点：
    /// - sample point = 87.5%
    /// - 并依据 `device_capability().fclk_can`（常见 48MHz / 80MHz）选择参数
    /// - 如果位定时参数不匹配，典型现象是：设备能 start，但总线错误/无 ACK，导致“收不到帧”
    ///
    /// 如果无法查询设备能力，将使用默认时钟（48MHz）作为 fallback
    pub fn set_bitrate(&mut self, bitrate: u32) -> Result<(), GsUsbError> {
        // 确保接口已 claim，避免控制请求失败
        self.ensure_interface_claimed()?;

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

        // 位定时表：推荐配置（sample point 87.5%）
        // 返回顺序：(prop_seg, phase_seg1, phase_seg2, sjw, brp)
        let timing = match clock {
            // 48 MHz clock (Candlelight / STM32-based devices)
            48_000_000 => match bitrate {
                10_000 => Some((1, 12, 2, 1, 300)),
                20_000 => Some((1, 12, 2, 1, 150)),
                50_000 => Some((1, 12, 2, 1, 60)),
                83_333 => Some((1, 12, 2, 1, 36)),
                100_000 => Some((1, 12, 2, 1, 30)),
                125_000 => Some((1, 12, 2, 1, 24)),
                250_000 => Some((1, 12, 2, 1, 12)),
                500_000 => Some((1, 12, 2, 1, 6)),
                800_000 => Some((1, 11, 2, 1, 4)),
                1_000_000 => Some((1, 12, 2, 1, 3)),
                _ => None,
            },
            // 80 MHz clock
            80_000_000 => match bitrate {
                10_000 => Some((1, 12, 2, 1, 500)),
                20_000 => Some((1, 12, 2, 1, 250)),
                50_000 => Some((1, 12, 2, 1, 100)),
                83_333 => Some((1, 12, 2, 1, 60)),
                100_000 => Some((1, 12, 2, 1, 50)),
                125_000 => Some((1, 12, 2, 1, 40)),
                250_000 => Some((1, 12, 2, 1, 20)),
                500_000 => Some((1, 12, 2, 1, 10)),
                800_000 => Some((1, 7, 1, 1, 10)),
                1_000_000 => Some((1, 12, 2, 1, 5)),
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
    /// 这条路径遵循 exact-write + fail-fast 语义：
    /// - 只有整帧写满才算成功
    /// - timeout / partial write 会立即返回错误
    /// - 不在发送热路径内做 endpoint 恢复
    pub fn send_raw(&self, frame: &GsUsbFrame) -> Result<(), GsUsbError> {
        self.send_raw_until(frame, Instant::now() + self.write_timeout)
    }

    /// 在绝对 deadline 内发送原始 GS-USB 帧。
    ///
    /// deadline 超过前仍未完成 USB Bulk OUT，则直接返回 `WriteTimeout`。
    /// 实时控制路径不在热路径内做 endpoint 恢复，后续恢复应由显式 reopen/recover 流程处理。
    pub fn send_raw_until(&self, frame: &GsUsbFrame, deadline: Instant) -> Result<(), GsUsbError> {
        let mut buf = [0u8; GS_USB_FRAME_SIZE_HW_TIMESTAMP];
        let packed = frame.pack_into_array(&mut buf, self.hw_timestamp);

        let timeout = Self::usb_timeout_from_deadline(deadline, Instant::now())?;

        match self.handle.write_bulk(self.endpoint_out, packed, timeout) {
            Ok(transferred) => Self::validate_bulk_write(transferred, packed.len()),
            Err(rusb::Error::Timeout) => Err(GsUsbError::WriteTimeout),
            Err(e) => Err(GsUsbError::Usb(e)),
        }
    }

    /// 批量接收：读取一个 USB Bulk 包，并将解析结果写入调用方复用的缓冲区和帧容器。
    pub fn receive_batch_into(
        &self,
        timeout: Duration,
        buf: &mut [u8],
        frames: &mut Vec<GsUsbFrame>,
    ) -> Result<(), GsUsbError> {
        frames.clear();

        let len = match self.handle.read_bulk(self.endpoint_in, buf, timeout) {
            Ok(len) => len,
            Err(rusb::Error::Timeout) => return Err(GsUsbError::ReadTimeout),
            Err(e) => return Err(GsUsbError::Usb(e)),
        };

        if len == 0 {
            return Ok(());
        }

        let frame_size = if self.hw_timestamp {
            GS_USB_FRAME_SIZE_HW_TIMESTAMP
        } else {
            GS_USB_FRAME_SIZE
        };

        if len % frame_size != 0 {
            return Err(GsUsbError::InvalidFrame(format!(
                "USB packet contains incomplete frame: {} bytes (expected multiple of {})",
                len, frame_size
            )));
        }

        let complete_frames = len / frame_size;
        if frames.capacity() < complete_frames {
            frames.reserve(complete_frames - frames.capacity());
        }

        let mut offset = 0;
        while offset + frame_size <= len {
            let frame_bytes = &buf[offset..offset + frame_size];
            let mut frame = GsUsbFrame::default();
            frame.unpack_from_bytes(frame_bytes, self.hw_timestamp)?;
            frames.push(frame);
            offset += frame_size;
        }

        Ok(())
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

impl Drop for GsUsbDevice {
    fn drop(&mut self) {
        let _ = self.stop();
        self.release_interface();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn test_is_gs_usb_device() {
        // 测试已知的 VID/PID
        assert!(GsUsbDevice::is_gs_usb_device(0x1D50, 0x606F)); // GS-USB
        assert!(GsUsbDevice::is_gs_usb_device(0x1209, 0x2323)); // Candlelight
        assert!(!GsUsbDevice::is_gs_usb_device(0x1234, 0x5678)); // 未知设备
    }

    #[test]
    fn test_host_format_bytes_le() {
        // 历史上存在 HOST_FORMAT 相关实现，这里仅保留字节序格式验证（不再要求实际发送该请求）
        let val: u32 = 0x0000_BEEF;
        let data = val.to_le_bytes();

        // 在 little-endian 系统上应该是 [0xEF, 0xBE, 0x00, 0x00]
        assert_eq!(data[0], 0xEF);
        assert_eq!(data[1], 0xBE);
        assert_eq!(data[2], 0x00);
        assert_eq!(data[3], 0x00);
    }

    #[test]
    fn test_usb_timeout_from_deadline_rejects_elapsed_deadline() {
        let now = Instant::now();
        let err = GsUsbDevice::usb_timeout_from_deadline(now, now).unwrap_err();
        assert!(matches!(err, GsUsbError::WriteTimeout));
    }

    #[test]
    fn test_usb_timeout_from_deadline_rejects_sub_millisecond_budget() {
        let now = Instant::now();
        let err = GsUsbDevice::usb_timeout_from_deadline(now + Duration::from_micros(999), now)
            .unwrap_err();
        assert!(matches!(err, GsUsbError::WriteTimeout));
    }

    #[test]
    fn test_usb_timeout_from_deadline_uses_millisecond_granularity_without_round_up() {
        let now = Instant::now();
        let timeout =
            GsUsbDevice::usb_timeout_from_deadline(now + Duration::from_micros(2500), now).unwrap();
        assert_eq!(timeout, Duration::from_millis(2));
    }

    #[test]
    fn test_validate_bulk_write_accepts_exact_write() {
        assert!(GsUsbDevice::validate_bulk_write(20, 20).is_ok());
    }

    #[test]
    fn test_validate_bulk_write_zero_bytes_is_timeout() {
        let err = GsUsbDevice::validate_bulk_write(0, 20).unwrap_err();
        assert!(matches!(err, GsUsbError::WriteTimeout));
    }

    #[test]
    fn test_validate_bulk_write_partial_write_is_error() {
        let err = GsUsbDevice::validate_bulk_write(8, 20).unwrap_err();
        assert!(matches!(
            err,
            GsUsbError::PartialWrite {
                transferred: 8,
                expected: 20
            }
        ));
    }

    #[test]
    fn test_drop_cleanup_runs_once_for_direct_device_owner() {
        let (device, harness) = GsUsbDevice::new_test_device(true, true);

        drop(device);

        assert_eq!(harness.stop_requests(), 1);
        assert_eq!(harness.interface_releases(), 1);
    }

    #[test]
    fn test_drop_cleanup_skips_stop_and_release_when_already_inactive() {
        let (device, harness) = GsUsbDevice::new_test_device(false, false);

        drop(device);

        assert_eq!(harness.stop_requests(), 0);
        assert_eq!(harness.interface_releases(), 0);
    }

    #[test]
    fn test_receive_batch_into_parses_standard_frames_without_heap_backing_per_frame() {
        let (device, harness) = GsUsbDevice::new_test_device(false, false);
        let frames_in = [
            GsUsbFrame {
                echo_id: GS_USB_RX_ECHO_ID,
                can_id: 0x251,
                can_dlc: 8,
                channel: 0,
                flags: 0,
                reserved: 0,
                data: [1, 2, 3, 4, 5, 6, 7, 8],
                timestamp_us: 0,
            },
            GsUsbFrame {
                echo_id: GS_USB_RX_ECHO_ID,
                can_id: 0x252,
                can_dlc: 4,
                channel: 0,
                flags: 0,
                reserved: 0,
                data: [9, 10, 11, 12, 0, 0, 0, 0],
                timestamp_us: 0,
            },
        ];
        harness.enqueue_read_packet(pack_packet(&frames_in, false));

        let mut buf = [0u8; GS_USB_READ_BUFFER_SIZE];
        let mut frames_out = Vec::with_capacity(1);
        device
            .receive_batch_into(Duration::from_millis(1), &mut buf, &mut frames_out)
            .unwrap();

        assert_eq!(frames_out.len(), 2);
        assert_eq!(frames_out[0].can_id, 0x251);
        assert_eq!(frames_out[0].data, [1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(frames_out[1].can_id, 0x252);
        assert_eq!(frames_out[1].can_dlc, 4);
        assert_eq!(frames_out[1].data[..4], [9, 10, 11, 12]);
    }

    #[test]
    fn test_receive_batch_into_parses_hw_timestamp_frames() {
        let (mut device, harness) = GsUsbDevice::new_test_device(false, false);
        device.hw_timestamp = true;
        let frames_in = [GsUsbFrame {
            echo_id: GS_USB_RX_ECHO_ID,
            can_id: 0x2A1,
            can_dlc: 8,
            channel: 0,
            flags: 0,
            reserved: 0,
            data: [0xAA, 0xBB, 0xCC, 0xDD, 1, 2, 3, 4],
            timestamp_us: 0x1122_3344,
        }];
        harness.enqueue_read_packet(pack_packet(&frames_in, true));

        let mut buf = [0u8; GS_USB_READ_BUFFER_SIZE];
        let mut frames_out = Vec::new();
        device
            .receive_batch_into(Duration::from_millis(1), &mut buf, &mut frames_out)
            .unwrap();

        assert_eq!(frames_out.len(), 1);
        assert_eq!(frames_out[0].can_id, 0x2A1);
        assert_eq!(frames_out[0].timestamp_us, 0x1122_3344);
    }

    #[test]
    fn test_receive_batch_into_rejects_trailing_incomplete_frame_bytes() {
        let (device, harness) = GsUsbDevice::new_test_device(false, false);
        let frame = GsUsbFrame {
            echo_id: GS_USB_RX_ECHO_ID,
            can_id: 0x2A5,
            can_dlc: 8,
            channel: 0,
            flags: 0,
            reserved: 0,
            data: [8, 7, 6, 5, 4, 3, 2, 1],
            timestamp_us: 0,
        };
        let mut packet = pack_packet(&[frame], false);
        packet.extend_from_slice(&[0xDE, 0xAD, 0xBE]);
        harness.enqueue_read_packet(packet);

        let mut buf = [0u8; GS_USB_READ_BUFFER_SIZE];
        let mut frames_out = Vec::new();
        let err = device
            .receive_batch_into(Duration::from_millis(1), &mut buf, &mut frames_out)
            .expect_err("trailing transport bytes must reject the whole packet");

        assert!(matches!(err, GsUsbError::InvalidFrame(_)));
        assert!(frames_out.is_empty());
    }

    // 注意：scan() 和实际 USB 操作的测试需要硬件，放在集成测试中
}
