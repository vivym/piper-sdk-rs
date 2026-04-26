//! GS-USB CAN 适配器实现
//!
//! 支持 Linux/macOS/Windows 平台的 GS-USB 协议实现

pub mod classify;
pub mod device;
pub mod error;
pub mod frame;
pub mod protocol;
pub mod split;

use crate::gs_usb::classify::parse_gs_usb_batch;
use crate::gs_usb::device::{
    GS_USB_BATCH_FRAME_CAPACITY, GS_USB_READ_BUFFER_SIZE, GsUsbDevice, GsUsbDeviceSelector,
};
use crate::gs_usb::frame::GsUsbFrame;
use crate::gs_usb::protocol::*;
use crate::gs_usb::split::{GsUsbRxAdapter, GsUsbTxAdapter};
use crate::{
    BackendCapability, CanAdapter, CanDeviceError, CanDeviceErrorKind, CanError, CanId, PiperFrame,
    ReceivedFrame, SplittableAdapter, TimestampProvenance,
};
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;
use tracing::{trace, warn};

/// GS-USB CAN 适配器
///
/// 实现 `CanAdapter` trait，提供统一的 CAN 接口
pub struct GsUsbCanAdapter {
    device: GsUsbDevice, // 在 split 时包裹为 Arc
    started: bool,
    /// 当前设备模式（用于判断是否需要过滤 Echo）
    mode: u32,
    /// USB Bulk IN 接收超时（用于 `receive()` 内部批量读取）
    rx_timeout: Duration,
    /// 接收队列：用于缓存 USB 包中解包出的多余帧
    /// USB 硬件会将多个 CAN 帧打包在一个 USB Bulk 包中发送
    /// 我们需要缓存这些帧，以便逐帧返回给上层应用
    rx_queue: VecDeque<ReceivedFrame>,
    /// 复用 USB 读缓冲，避免热路径每包分配
    rx_usb_buf: [u8; GS_USB_READ_BUFFER_SIZE],
    /// 复用 GS-USB 帧容器，避免 steady-state 堆分配
    rx_batch_frames: Vec<GsUsbFrame>,
    /// 实时模式标志
    /// - `true`：写超时设为 5ms（快速失败）
    /// - `false`：写超时保持 1000ms（默认，更可靠）
    realtime_mode: bool,
    /// 连续写超时计数（用于检测设备故障）
    consecutive_write_timeouts: u32,
}

impl GsUsbCanAdapter {
    /// Maximum RX queue size to prevent unbounded memory growth
    /// When exceeded, oldest frames are dropped
    const MAX_QUEUE_SIZE: usize = 256;

    /// 创建新的适配器（扫描并打开设备）
    ///
    /// 如果没有指定序列号，自动选择第一个找到的设备。
    pub fn new() -> Result<Self, CanError> {
        Self::new_with_selector(GsUsbDeviceSelector::any())
    }

    /// 创建新的适配器（按序列号指定设备）
    ///
    /// # 参数
    /// - `serial_number`: 可选的设备序列号，如果提供，只打开匹配序列号的设备
    ///
    /// # 错误
    /// - `CanError::Device`: 如果没有找到匹配的设备，或者扫描失败
    pub fn new_with_serial(serial_number: Option<&str>) -> Result<Self, CanError> {
        let selector = match serial_number {
            Some(sn) => GsUsbDeviceSelector::by_serial(sn),
            None => GsUsbDeviceSelector::any(),
        };
        Self::new_with_selector(selector)
    }

    /// 创建新的适配器（按选择器指定设备）
    pub fn new_with_selector(selector: GsUsbDeviceSelector) -> Result<Self, CanError> {
        // 两段式：scan_info 用于决策/告警，open 才真正占用 handle
        let infos =
            GsUsbDevice::scan_info_with_filter(selector.serial_number.as_deref()).map_err(|e| {
                CanError::Device(CanDeviceError::new(
                    CanDeviceErrorKind::Backend,
                    format!("Failed to scan devices: {}", e),
                ))
            })?;

        if infos.is_empty() {
            let error_msg = if let Some(sn) = selector.serial_number.as_deref() {
                format!("No GS-USB device found with serial number: {}", sn)
            } else if let (Some(bus), Some(addr)) = (selector.bus_number, selector.address) {
                format!("No GS-USB device found at {}:{}", bus, addr)
            } else {
                "No GS-USB device found".to_string()
            };
            return Err(CanError::Device(error_msg.into()));
        }

        if infos.len() > 1 {
            let warning_msg = if let Some(sn) = selector.serial_number.as_deref() {
                format!(
                    "Multiple GS-USB devices found with serial number '{}', using the first one",
                    sn
                )
            } else if let (Some(bus), Some(addr)) = (selector.bus_number, selector.address) {
                format!(
                    "Multiple GS-USB devices found at {}:{}, using the first one",
                    bus, addr
                )
            } else {
                "Multiple GS-USB devices found, using the first one".to_string()
            };
            tracing::warn!("{}", warning_msg);
        }

        let device = GsUsbDevice::open(&selector).map_err(|e| {
            let (kind, message) = match e {
                crate::gs_usb::error::GsUsbError::DeviceNotFound => {
                    (CanDeviceErrorKind::NotFound, format!("Failed to open device: {}", e))
                },
                crate::gs_usb::error::GsUsbError::Usb(rusb::Error::Access) => {
                    let msg = format!(
                        "Permission denied accessing GS-USB device. \
                         Please install udev rules: sudo cp scripts/99-piper-gs-usb.rules /etc/udev/rules.d/ && \
                         sudo udevadm control --reload-rules && sudo udevadm trigger. \
                         Or run the installation script: ./scripts/install-udev-rules.sh. \
                         See docs/v0/gs_usb_linux_conditional_compilation_analysis.md for details. \
                         Original error: {}",
                        e
                    );
                    (CanDeviceErrorKind::AccessDenied, msg)
                },
                _ => {
                    (CanDeviceErrorKind::Backend, format!("Failed to open device: {}", e))
                },
            };
            CanError::Device(CanDeviceError::new(kind, message))
        })?;
        Ok(Self {
            device, // 保持为 GsUsbDevice，在 split 时包裹为 Arc
            started: false,
            mode: 0,
            // 统一默认超时为 2ms（与 PipelineConfig 默认值一致）
            // 对于力控/高频控制场景，2ms 超时能确保命令及时发送
            // 对于非实时场景，用户可通过 set_receive_timeout() 显式设置更大的值
            rx_timeout: Duration::from_millis(2),
            rx_queue: VecDeque::with_capacity(64), // 初始化队列，预分配容量
            rx_usb_buf: [0u8; GS_USB_READ_BUFFER_SIZE],
            rx_batch_frames: Vec::with_capacity(GS_USB_BATCH_FRAME_CAPACITY),
            realtime_mode: false, // 默认非实时模式
            consecutive_write_timeouts: 0,
        })
    }

    /// 设置 `receive()` 内部 USB Bulk IN 的超时
    ///
    /// - `Duration::ZERO`：阻塞等待（由底层 USB 库语义决定；不推荐在需要可取消的线程中使用）
    /// - 建议 daemon 场景使用较大值（例如 50~200ms），避免热循环
    pub fn set_receive_timeout(&mut self, timeout: Duration) {
        self.rx_timeout = timeout;
    }

    /// 设置实时模式
    ///
    /// 实时模式下，USB Bulk OUT 写超时从 1000ms 降到 5ms，实现快速失败。
    /// 这对于力控/高频控制场景很重要，可以避免长时间阻塞。
    ///
    /// # 参数
    /// - `enabled`: 是否启用实时模式
    ///
    /// # 使用场景
    /// - **实时模式（true）**：力控/高频控制，需要快速失败（< 10ms）
    /// - **默认模式（false）**：状态监控/调试，更可靠但可能阻塞（最多 1000ms）
    ///
    /// # 注意事项
    /// - 实时模式下，如果 USB 设备忙碌或总线拥塞，可能会频繁出现写超时
    /// - 连续超时超过阈值（10 次）时，建议检查设备状态或切换到默认模式
    pub fn set_realtime_mode(&mut self, enabled: bool) {
        self.realtime_mode = enabled;
        if enabled {
            self.device.set_write_timeout(Duration::from_millis(5));
            tracing::info!("GS-USB realtime mode enabled: write timeout set to 5ms");
        } else {
            self.device.set_write_timeout(Duration::from_millis(1000));
            tracing::info!("GS-USB realtime mode disabled: write timeout set to 1000ms");
        }
        // 重置连续超时计数
        self.consecutive_write_timeouts = 0;
    }

    /// 获取实时模式状态
    pub fn is_realtime_mode(&self) -> bool {
        self.realtime_mode
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
        if self.device.hw_timestamp_enabled() {
            TimestampProvenance::Hardware
        } else {
            TimestampProvenance::None
        }
    }

    /// 分离为独立的 RX 和 TX 适配器
    ///
    /// 返回的适配器可以在不同线程中并发使用，实现物理隔离。
    ///
    /// **注意**：此方法会消费 `self`，分离后不能再使用 `GsUsbCanAdapter`。
    ///
    /// # 前置条件
    /// - 设备必须已启动（`started == true`）
    ///
    /// # 返回
    /// - `Ok((rx_adapter, tx_adapter))`：成功分离
    /// - `Err(CanError::NotStarted)`：设备未启动
    pub fn split(self) -> Result<(GsUsbRxAdapter, GsUsbTxAdapter), CanError> {
        if !self.started {
            return Err(CanError::NotStarted);
        }

        let GsUsbCanAdapter {
            device,
            rx_timeout,
            mode,
            ..
        } = self;
        let device_arc = Arc::new(device);

        Ok((
            GsUsbRxAdapter::new(
                device_arc.clone(),
                rx_timeout,
                mode,
                device_arc.hw_timestamp_enabled(),
            ),
            GsUsbTxAdapter::new(device_arc),
        ))
    }

    /// 批量接收：一次从 USB 读取一个包，解析并返回其中所有有效 CAN 帧及时间戳来源
    ///
    /// - 会应用与 `receive()` 相同的 Echo 过滤与 overflow 检测逻辑
    /// - 返回的 Vec 可能为空（例如读到的都是 Echo 且被过滤，或读到空包）
    pub fn receive_batch_frames(&mut self) -> Result<Vec<ReceivedFrame>, CanError> {
        if !self.started {
            return Err(CanError::NotStarted);
        }

        // 先把队列里剩余的帧吐出来（保持语义一致）
        if !self.rx_queue.is_empty() {
            let mut out = Vec::with_capacity(self.rx_queue.len());
            while let Some(received) = self.rx_queue.pop_front() {
                out.push(received);
            }
            return Ok(out);
        }

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

        if self.rx_batch_frames.is_empty() {
            return Ok(Vec::new());
        }

        let out = parse_gs_usb_batch(&self.rx_batch_frames);
        self.rx_batch_frames.clear();
        let provenance = self.timestamp_provenance();
        out.map(|frames| {
            frames.into_iter().map(|frame| ReceivedFrame::new(frame, provenance)).collect()
        })
    }

    /// 获取当前打开设备的基础信息（用于日志/诊断）
    pub fn device_info(&self) -> (u16, u16, u8, u8, Option<&str>) {
        (
            self.device.vendor_id(),
            self.device.product_id(),
            self.device.bus_number(),
            self.device.address(),
            self.device.serial_number(),
        )
    }

    /// 设置 USB STALL 计数回调
    ///
    /// 当设备发生 USB STALL 并被成功清除时，会调用此回调。
    /// 必须在 `split()` 之前调用，因为 `split()` 会移动设备。
    ///
    /// # 参数
    /// - `callback`: 回调函数，在 STALL 清除成功时调用
    pub fn set_stall_count_callback<F>(&mut self, callback: F)
    where
        F: Fn() + Send + Sync + 'static,
    {
        self.device.set_stall_count_callback(callback);
    }

    /// 内部方法：统一配置逻辑
    ///
    /// 所有配置方法的核心逻辑都集中在这里，消除重复代码。
    ///
    /// 推荐启动流程（参考实现一致）：
    /// open() -> set_bitrate() -> start()
    /// start() 内部: reset() -> detach_kernel_driver() -> 获取 capability -> 发送 MODE
    ///
    /// 注意：
    /// - 默认实现不发送 HOST_FORMAT（兼容性策略见文档）
    /// - set_bitrate() 在 start() 之前调用，但 start() 内部会 reset
    /// - reset 不会清除 bitrate 设置，因为 bitrate 是通过控制请求设置的持久化配置
    fn configure_with_mode(&mut self, bitrate: u32, mode: u32) -> Result<(), CanError> {
        // **对齐参考实现的推荐流程**：
        // 1) set_bitrate() 在 start() 之前调用（set_bitrate 内部会确保 interface 已 claim）
        // 2) start() 内部负责 reset/detach/claim/capability/MODE
        //
        // 说明：我们不再对外暴露“仅 claim interface”的历史接口，避免多套启动语义。

        // 1. 设置波特率（在 start() 之前）
        // 注意：start() 内部会 reset，但 reset 不会清除 bitrate 设置
        // 因为 bitrate 是通过控制请求设置的，是持久化配置
        self.device.set_bitrate(bitrate).map_err(|e| {
            let kind = match e {
                crate::gs_usb::error::GsUsbError::UnsupportedBitrate { .. } => {
                    CanDeviceErrorKind::UnsupportedConfig
                },
                _ => CanDeviceErrorKind::Backend,
            };
            CanError::Device(CanDeviceError::new(
                kind,
                format!("Failed to set bitrate: {}", e),
            ))
        })?;

        // 2. 启动设备（start 内部会 reset, detach, 获取 capability, 发送 MODE）
        // start() 内部会 reset，但不会清除之前设置的 bitrate
        let start_result = self.device.start(mode).map_err(|e| {
            CanError::Device(CanDeviceError::new(
                CanDeviceErrorKind::Backend,
                format!("Failed to start device: {}", e),
            ))
        })?;

        self.started = true;
        self.mode = mode;
        self.rx_queue.clear(); // 启动时清空队列

        // 构建模式名称（支持组合模式，如 LOOP_BACK|HW_TIMESTAMP）
        let mode_name = {
            let mut parts = Vec::new();
            if (mode & GS_CAN_MODE_LOOP_BACK) != 0 {
                parts.push("LOOP_BACK");
            }
            if (mode & GS_CAN_MODE_LISTEN_ONLY) != 0 {
                parts.push("LISTEN_ONLY");
            }
            if (mode & GS_CAN_MODE_HW_TIMESTAMP) != 0 {
                parts.push("HW_TIMESTAMP");
            }
            if parts.is_empty() {
                "NORMAL".to_string()
            } else {
                parts.join("|")
            }
        };
        trace!(
            "GS-USB device started in {} mode at {} bps (effective_flags=0x{:08x}, fclk_can={}Hz, hw_timestamp={})",
            mode_name,
            bitrate,
            start_result.effective_flags,
            start_result.capability.fclk_can,
            start_result.hw_timestamp
        );
        Ok(())
    }

    /// 配置并启动设备（Normal 模式，默认启用硬件时间戳）
    ///
    /// 对于机械臂场景，硬件时间戳对于精确的时间测量和力控算法至关重要。
    pub fn configure(&mut self, bitrate: u32) -> Result<(), CanError> {
        self.configure_with_mode(bitrate, GS_CAN_MODE_NORMAL | GS_CAN_MODE_HW_TIMESTAMP)
    }

    /// 配置并启动设备（Loopback 模式，安全测试，默认启用硬件时间戳）
    ///
    /// Loopback 模式下，发送的帧会在设备内部回环，不会向 CAN 总线发送。
    /// 这允许在安全的环境中测试完整的发送/接收路径。
    ///
    /// 对于机械臂场景，硬件时间戳对于精确的时间测量和力控算法至关重要。
    pub fn configure_loopback(&mut self, bitrate: u32) -> Result<(), CanError> {
        self.configure_with_mode(bitrate, GS_CAN_MODE_LOOP_BACK | GS_CAN_MODE_HW_TIMESTAMP)
    }

    /// 配置并启动设备（Listen-Only 模式，只接收不发送，默认启用硬件时间戳）
    ///
    /// Listen-Only 模式下，设备不会发送任何帧，也不会发送 ACK。
    /// 适用于安全地监听 CAN 总线上的数据。
    ///
    /// 对于机械臂场景，硬件时间戳对于精确的时间测量和力控算法至关重要。
    pub fn configure_listen_only(&mut self, bitrate: u32) -> Result<(), CanError> {
        self.configure_with_mode(bitrate, GS_CAN_MODE_LISTEN_ONLY | GS_CAN_MODE_HW_TIMESTAMP)
    }

    pub fn backend_capability(&self) -> BackendCapability {
        if self.device.hw_timestamp_enabled() {
            BackendCapability::SoftRealtime
        } else {
            BackendCapability::MonitorOnly
        }
    }
}

impl SplittableAdapter for GsUsbCanAdapter {
    type RxAdapter = GsUsbRxAdapter;
    type TxAdapter = GsUsbTxAdapter;

    fn backend_capability(&self) -> BackendCapability {
        self.backend_capability()
    }

    fn split(self) -> Result<(Self::RxAdapter, Self::TxAdapter), CanError> {
        GsUsbCanAdapter::split(self)
    }
}

impl CanAdapter for GsUsbCanAdapter {
    /// 发送帧（Fire-and-Forget）
    fn send(&mut self, frame: PiperFrame) -> Result<(), CanError> {
        if !self.started {
            return Err(CanError::NotStarted);
        }

        let can_id = match frame.id() {
            CanId::Standard(id) => id.raw() as u32,
            CanId::Extended(id) => id.raw() | CAN_EFF_FLAG,
        };

        // 1. 转换 PiperFrame -> GsUsbFrame
        let gs_frame = GsUsbFrame {
            echo_id: GS_USB_ECHO_ID,
            can_id,
            can_dlc: frame.dlc(),
            channel: 0,
            flags: 0,
            reserved: 0,
            data: *frame.data_padded(),
            timestamp_us: 0, // 发送时时间戳值（如果启用了硬件时间戳模式，pack_to 会包含该字段）
        };

        // 2. 发送 USB Bulk OUT（不等待 Echo）
        match self.device.send_raw(&gs_frame) {
            Ok(_) => {
                // 发送成功，重置连续超时计数
                self.consecutive_write_timeouts = 0;
            },
            Err(crate::gs_usb::error::GsUsbError::WriteTimeout) => {
                // 写超时，增加计数
                self.consecutive_write_timeouts += 1;

                // 如果连续超时超过阈值（10 次），记录警告
                if self.consecutive_write_timeouts >= 10 {
                    tracing::warn!(
                        "GS-USB consecutive write timeouts: {} (threshold: 10). \
                        Device may be busy or USB connection unstable. \
                        Consider checking device status or disabling realtime mode.",
                        self.consecutive_write_timeouts
                    );
                }

                return Err(CanError::Device(CanDeviceError::new(
                    CanDeviceErrorKind::Busy,
                    format!(
                        "USB send timeout (consecutive: {})",
                        self.consecutive_write_timeouts
                    ),
                )));
            },
            Err(e) => {
                // 其他错误，重置计数
                self.consecutive_write_timeouts = 0;
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
                    crate::gs_usb::error::GsUsbError::PartialWrite { .. } => {
                        CanDeviceErrorKind::Backend
                    },
                    _ => CanDeviceErrorKind::Backend,
                };
                return Err(CanError::Device(CanDeviceError::new(
                    kind,
                    format!("USB send failed: {}", e),
                )));
            },
        }

        // Hot path: removed trace! call (TX can be high frequency)
        Ok(())
    }

    /// 接收帧（带缓冲的批量处理）
    ///
    /// **关键修复**：USB 硬件会将多个 CAN 帧打包在一个 USB Bulk 包中发送。
    /// 如果只解析第一个帧并丢弃后续帧，会导致高吞吐量测试中的丢包。
    ///
    /// **解决方案**：
    /// 1. 使用内部队列 (`rx_queue`) 缓存从 USB 包中解析出的所有帧
    /// 2. 优先从队列中返回帧（如果队列非空）
    /// 3. 队列为空时，从 USB 读取一个包，解析出所有帧并放入队列
    fn receive(&mut self) -> Result<ReceivedFrame, CanError> {
        if !self.started {
            return Err(CanError::NotStarted);
        }

        // 1. 优先从队列中取（如果有上次读剩下的）
        if let Some(frame) = self.rx_queue.pop_front() {
            // Hot path: removed trace! call (queue return is 200Hz+)
            return Ok(frame);
        }
        // 2. 队列为空，从 USB 读取一批数据
        // USB 硬件可能将一个或多个 CAN 帧打包在一个 USB Bulk 包中
        // 我们需要一次性解析所有帧，并将它们放入队列
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
            // 注意：空包是正常情况，USB 硬件可能发送空的批量传输包
            // 超时时间短（2ms），影响不大，继续读取即可
            if self.rx_batch_frames.is_empty() {
                continue;
            }

            let parsed = parse_gs_usb_batch(&self.rx_batch_frames);
            self.rx_batch_frames.clear();
            let parsed = parsed?;
            let provenance = self.timestamp_provenance();
            for frame in parsed {
                self.push_to_rx_queue(ReceivedFrame::new(frame, provenance));
            }

            // 4. 如果队列里有东西了，返回第一个；否则继续循环读 USB
            // 注意：如果这批数据都被过滤掉了（例如全是 Echo），循环继续
            if let Some(frame) = self.rx_queue.pop_front() {
                // Hot path: removed trace! call (200Hz+)
                return Ok(frame);
            }
            // 如果这批数据都被过滤掉了，继续读下一个 USB 包
        }
    }

    /// 设置接收超时
    fn set_receive_timeout(&mut self, timeout: Duration) {
        // 直接设置 rx_timeout 字段
        // 注意：GsUsbDevice 的接收超时是在 read_bulk 时使用的，这里只更新适配器的超时
        self.rx_timeout = timeout;
    }

    /// 带超时的接收
    fn receive_timeout(&mut self, timeout: Duration) -> Result<ReceivedFrame, CanError> {
        // 保存原超时
        let old_timeout = self.rx_timeout;

        // 设置新超时
        self.set_receive_timeout(timeout);

        // 接收
        let result = self.receive();

        // 恢复原超时
        self.set_receive_timeout(old_timeout);

        result
    }

    /// 非阻塞接收
    fn try_receive(&mut self) -> Result<Option<ReceivedFrame>, CanError> {
        // 使用零超时模拟非阻塞
        match self.receive_timeout(Duration::ZERO) {
            Ok(frame) => Ok(Some(frame)),
            Err(CanError::Timeout) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// 带超时的发送
    fn send_timeout(&mut self, frame: PiperFrame, timeout: Duration) -> Result<(), CanError> {
        // 保存原写超时
        let old_timeout = self.device.write_timeout();

        // 设置新写超时
        self.device.set_write_timeout(timeout);

        // 发送
        let result = self.send(frame);

        // 恢复原写超时
        self.device.set_write_timeout(old_timeout);

        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

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

    fn started_adapter(device: GsUsbDevice) -> GsUsbCanAdapter {
        GsUsbCanAdapter {
            device,
            started: true,
            mode: 0,
            rx_timeout: Duration::from_millis(2),
            rx_queue: VecDeque::with_capacity(64),
            rx_usb_buf: [0u8; GS_USB_READ_BUFFER_SIZE],
            rx_batch_frames: Vec::with_capacity(GS_USB_BATCH_FRAME_CAPACITY),
            realtime_mode: false,
            consecutive_write_timeouts: 0,
        }
    }

    #[test]
    fn test_unsplit_adapter_drop_cleans_up_once() {
        let (device, harness) = GsUsbDevice::new_test_device(true, true);
        let adapter = GsUsbCanAdapter {
            device,
            started: true,
            mode: 0,
            rx_timeout: Duration::from_millis(2),
            rx_queue: VecDeque::with_capacity(64),
            rx_usb_buf: [0u8; GS_USB_READ_BUFFER_SIZE],
            rx_batch_frames: Vec::with_capacity(GS_USB_BATCH_FRAME_CAPACITY),
            realtime_mode: false,
            consecutive_write_timeouts: 0,
        };

        drop(adapter);

        assert_eq!(harness.stop_requests(), 1);
        assert_eq!(harness.interface_releases(), 1);
    }

    #[test]
    fn test_split_adapters_cleanup_runs_once_after_last_owner_drops() {
        let (device, harness) = GsUsbDevice::new_test_device(true, true);
        let adapter = GsUsbCanAdapter {
            device,
            started: true,
            mode: 0,
            rx_timeout: Duration::from_millis(2),
            rx_queue: VecDeque::with_capacity(64),
            rx_usb_buf: [0u8; GS_USB_READ_BUFFER_SIZE],
            rx_batch_frames: Vec::with_capacity(GS_USB_BATCH_FRAME_CAPACITY),
            realtime_mode: false,
            consecutive_write_timeouts: 0,
        };

        let (rx, tx) = adapter.split().expect("test device should split");

        drop(rx);
        assert_eq!(harness.stop_requests(), 0);
        assert_eq!(harness.interface_releases(), 0);

        drop(tx);
        assert_eq!(harness.stop_requests(), 1);
        assert_eq!(harness.interface_releases(), 1);
    }

    #[test]
    fn test_unsplit_receive_discards_batch_on_overflow_status() {
        let (device, harness) = GsUsbDevice::new_test_device(false, false);
        harness.enqueue_read_packet(pack_packet(
            &[
                rx_frame(0x251, GS_CAN_FLAG_OVERFLOW, 0x11),
                rx_frame(0x252, 0, 0x22),
            ],
            false,
        ));
        let mut adapter = GsUsbCanAdapter {
            device,
            started: true,
            mode: 0,
            rx_timeout: Duration::from_millis(2),
            rx_queue: VecDeque::with_capacity(64),
            rx_usb_buf: [0u8; GS_USB_READ_BUFFER_SIZE],
            rx_batch_frames: Vec::with_capacity(GS_USB_BATCH_FRAME_CAPACITY),
            realtime_mode: false,
            consecutive_write_timeouts: 0,
        };

        let overflow =
            adapter.receive().expect_err("overflow should reject the whole device batch");
        let later =
            adapter.receive().expect_err("valid frames from fatal batch must not be queued");

        assert!(matches!(overflow, CanError::BufferOverflow));
        assert!(matches!(later, CanError::Timeout));
    }

    #[test]
    fn unsplit_batch_skips_recoverable_between_valid_frames() {
        let (device, harness) = GsUsbDevice::new_test_device(false, false);
        harness.enqueue_read_packet(pack_packet(
            &[
                rx_frame(0x100, 0, 0x10),
                recoverable_echo_frame(),
                rx_frame(0x101, 0, 0x11),
            ],
            false,
        ));
        let mut adapter = started_adapter(device);

        let frames = adapter.receive_batch_frames().unwrap();

        assert_eq!(
            frames.iter().map(|received| received.frame.raw_id()).collect::<Vec<_>>(),
            vec![0x100, 0x101]
        );
        assert!(
            frames
                .iter()
                .all(|received| received.timestamp_provenance == TimestampProvenance::None)
        );
    }

    #[test]
    fn unsplit_batch_malformed_discards_whole_batch() {
        let (device, harness) = GsUsbDevice::new_test_device(false, false);
        harness.enqueue_read_packet(pack_packet(
            &[
                rx_frame(0x100, 0, 0x10),
                malformed_dlc_frame(9),
                rx_frame(0x101, 0, 0x11),
            ],
            false,
        ));
        let mut adapter = started_adapter(device);

        assert!(adapter.receive_batch_frames().is_err());
        assert!(matches!(adapter.receive(), Err(CanError::Timeout)));
    }

    #[test]
    fn unsplit_batch_fatal_status_discards_whole_batch() {
        let (device, harness) = GsUsbDevice::new_test_device(false, false);
        harness.enqueue_read_packet(pack_packet(
            &[
                rx_frame(0x100, 0, 0x10),
                rx_frame(0x120, GS_CAN_FLAG_OVERFLOW, 0xEE),
                rx_frame(0x101, 0, 0x11),
            ],
            false,
        ));
        let mut adapter = started_adapter(device);

        assert!(matches!(
            adapter.receive_batch_frames(),
            Err(CanError::BufferOverflow)
        ));
        assert!(matches!(adapter.receive(), Err(CanError::Timeout)));
    }

    #[test]
    fn unsplit_batch_fatal_transport_discards_whole_batch() {
        let (device, harness) = GsUsbDevice::new_test_device(false, false);
        harness.enqueue_read_packet(packet_with_trailing_incomplete_bytes(&[
            rx_frame(0x100, 0, 0x10),
            rx_frame(0x101, 0, 0x11),
        ]));
        let mut adapter = started_adapter(device);

        assert!(adapter.receive_batch_frames().is_err());
        assert!(matches!(adapter.receive(), Err(CanError::Timeout)));
    }

    #[test]
    fn unsplit_batch_preserves_queued_received_frame_provenance() {
        let (device, harness) = GsUsbDevice::new_test_device(false, false);
        harness.enqueue_read_packet(pack_packet(
            &[rx_frame(0x100, 0, 0x10), rx_frame(0x101, 0, 0x11)],
            false,
        ));
        let mut adapter = started_adapter(device);

        assert_eq!(adapter.receive().unwrap().frame.raw_id(), 0x100);
        let queued = adapter.receive_batch_frames().unwrap();

        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].frame.raw_id(), 0x101);
        assert_eq!(queued[0].timestamp_provenance, TimestampProvenance::None);
    }

    #[test]
    fn unsplit_receive_skips_recoverable_between_valid_frames() {
        let (device, harness) = GsUsbDevice::new_test_device(false, false);
        harness.enqueue_read_packet(pack_packet(
            &[
                rx_frame(0x100, 0, 0x10),
                recoverable_echo_frame(),
                rx_frame(0x101, 0, 0x11),
            ],
            false,
        ));
        let mut adapter = started_adapter(device);

        assert_eq!(adapter.receive().unwrap().frame.raw_id(), 0x100);
        assert_eq!(adapter.receive().unwrap().frame.raw_id(), 0x101);
        assert!(matches!(adapter.receive(), Err(CanError::Timeout)));
    }

    #[test]
    fn unsplit_receive_malformed_discards_whole_batch() {
        let (device, harness) = GsUsbDevice::new_test_device(false, false);
        harness.enqueue_read_packet(pack_packet(
            &[
                rx_frame(0x100, 0, 0x10),
                malformed_dlc_frame(9),
                rx_frame(0x101, 0, 0x11),
            ],
            false,
        ));
        let mut adapter = started_adapter(device);

        assert!(adapter.receive().is_err());
        assert!(matches!(adapter.receive(), Err(CanError::Timeout)));
    }

    #[test]
    fn unsplit_receive_fatal_status_discards_whole_batch() {
        let (device, harness) = GsUsbDevice::new_test_device(false, false);
        harness.enqueue_read_packet(pack_packet(
            &[
                rx_frame(0x100, 0, 0x10),
                rx_frame(0x120, GS_CAN_FLAG_OVERFLOW, 0xEE),
                rx_frame(0x101, 0, 0x11),
            ],
            false,
        ));
        let mut adapter = started_adapter(device);

        assert!(matches!(adapter.receive(), Err(CanError::BufferOverflow)));
        assert!(matches!(adapter.receive(), Err(CanError::Timeout)));
    }

    #[test]
    fn unsplit_receive_fatal_transport_discards_whole_batch() {
        let (device, harness) = GsUsbDevice::new_test_device(false, false);
        harness.enqueue_read_packet(packet_with_trailing_incomplete_bytes(&[
            rx_frame(0x100, 0, 0x10),
            rx_frame(0x101, 0, 0x11),
        ]));
        let mut adapter = started_adapter(device);

        assert!(adapter.receive().is_err());
        assert!(matches!(adapter.receive(), Err(CanError::Timeout)));
    }

    #[test]
    fn unsplit_receive_transport_timeout_queues_no_frame() {
        let (device, _harness) = GsUsbDevice::new_test_device(false, false);
        let mut adapter = started_adapter(device);

        assert!(matches!(adapter.receive(), Err(CanError::Timeout)));
        assert!(matches!(adapter.receive(), Err(CanError::Timeout)));
    }
}
