//! GS-USB CAN 适配器实现
//!
//! 支持 macOS/Windows 平台的 GS-USB 协议实现

pub mod device;
pub mod error;
pub mod frame;
pub mod protocol;

use crate::can::gs_usb::device::GsUsbDevice;
use crate::can::gs_usb::frame::GsUsbFrame;
use crate::can::gs_usb::protocol::*;
use crate::can::{CanAdapter, CanError, PiperFrame};
use std::collections::VecDeque;
use std::time::Duration;
use tracing::{error, trace};

/// GS-USB CAN 适配器
///
/// 实现 `CanAdapter` trait，提供统一的 CAN 接口
pub struct GsUsbCanAdapter {
    device: GsUsbDevice,
    started: bool,
    /// 当前设备模式（用于判断是否需要过滤 Echo）
    mode: u32,
    /// 接收队列：用于缓存 USB 包中解包出的多余帧
    /// USB 硬件会将多个 CAN 帧打包在一个 USB Bulk 包中发送
    /// 我们需要缓存这些帧，以便逐帧返回给上层应用
    rx_queue: VecDeque<PiperFrame>,
}

impl GsUsbCanAdapter {
    /// 创建新的适配器（扫描并打开设备）
    pub fn new() -> Result<Self, CanError> {
        let mut devices = GsUsbDevice::scan()
            .map_err(|e| CanError::Device(format!("Failed to scan devices: {}", e)))?;

        if devices.is_empty() {
            return Err(CanError::Device("No GS-USB device found".to_string()));
        }

        let device = devices.remove(0);
        Ok(Self {
            device,
            started: false,
            mode: 0,
            rx_queue: VecDeque::with_capacity(64), // 初始化队列，预分配容量
        })
    }

    /// 内部方法：统一配置逻辑
    ///
    /// 所有配置方法的核心逻辑都集中在这里，消除重复代码。
    fn configure_with_mode(&mut self, bitrate: u32, mode: u32) -> Result<(), CanError> {
        // 1. 发送 HOST_FORMAT（协议握手 + 字节序配置）
        //
        // **关键**：这个请求不仅仅是字节序配置，更是协议握手信号。
        // 某些固件在收到此命令前可能处于未初始化状态，拒绝后续配置命令。
        //
        // **策略**：Fire-and-Forget for Handshake
        // - 必须尝试发送，以兼容需要握手的固件
        // - 忽略错误，因为：
        //   * 现代设备可能不支持此命令（默认 LE）
        //   * 设备可能已处于正确状态
        //   * 不应因握手失败阻断整个初始化流程
        let _ = self.device.send_host_format();

        // 2. 设置波特率
        self.device
            .set_bitrate(bitrate)
            .map_err(|e| CanError::Device(format!("Failed to set bitrate: {}", e)))?;

        // 3. 启动设备
        self.device
            .start(mode)
            .map_err(|e| CanError::Device(format!("Failed to start device: {}", e)))?;

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
            "GS-USB device started in {} mode at {} bps",
            mode_name, bitrate
        );
        Ok(())
    }

    /// 配置并启动设备（Normal 模式）
    pub fn configure(&mut self, bitrate: u32) -> Result<(), CanError> {
        self.configure_with_mode(bitrate, GS_CAN_MODE_NORMAL)
    }

    /// 配置并启动设备（Loopback 模式，安全测试）
    ///
    /// Loopback 模式下，发送的帧会在设备内部回环，不会向 CAN 总线发送。
    /// 这允许在安全的环境中测试完整的发送/接收路径。
    pub fn configure_loopback(&mut self, bitrate: u32) -> Result<(), CanError> {
        self.configure_with_mode(bitrate, GS_CAN_MODE_LOOP_BACK)
    }

    /// 配置并启动设备（Listen-Only 模式，只接收不发送）
    ///
    /// Listen-Only 模式下，设备不会发送任何帧，也不会发送 ACK。
    /// 适用于安全地监听 CAN 总线上的数据。
    pub fn configure_listen_only(&mut self, bitrate: u32) -> Result<(), CanError> {
        self.configure_with_mode(bitrate, GS_CAN_MODE_LISTEN_ONLY)
    }
}

impl Drop for GsUsbCanAdapter {
    /// 自动清理：当适配器离开作用域时，自动停止设备并释放资源
    ///
    /// 这是 Rust 管理硬件资源的黄金法则：
    /// - 无论测试成功还是失败（panic），设备都会被正确复位
    /// - 释放 USB 接口，交还给操作系统
    /// - 防止设备状态残留，导致下次测试失败
    /// - 类似 C++ 的 RAII (Resource Acquisition Is Initialization) 模式
    fn drop(&mut self) {
        // 当 Adapter 离开作用域时（测试结束、函数返回、或 Panic 时）自动调用

        // 1. 停止设备固件逻辑（发送 RESET 命令）
        if self.started {
            // 发送 RESET 命令停止设备
            // 忽略错误，因为我们都要退出了，且 device 可能已经断开了
            let _ = self.device.start(GS_CAN_MODE_RESET);
            trace!("[Auto-Drop] Device reset command sent");
        }

        // 2. 释放 USB 接口（交还给操作系统）
        // **关键**：这解决了"状态残留"问题，特别是 macOS 的独占访问控制。
        // 如果不释放接口，操作系统可能认为接口仍被占用，导致下次启动时
        // 无法 claim 接口（Access denied）或保持错误的 Data Toggle 状态。
        self.device.release_interface();
        trace!("[Auto-Drop] USB Interface released");
    }
}

impl CanAdapter for GsUsbCanAdapter {
    /// 发送帧（Fire-and-Forget）
    fn send(&mut self, frame: PiperFrame) -> Result<(), CanError> {
        if !self.started {
            return Err(CanError::NotStarted);
        }

        // 1. 转换 PiperFrame -> GsUsbFrame
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
            timestamp_us: 0, // 发送时时间戳值（如果启用了硬件时间戳模式，pack_to 会包含该字段）
        };

        // 2. 发送 USB Bulk OUT（不等待 Echo）
        self.device
            .send_raw(&gs_frame)
            .map_err(|e| CanError::Device(format!("USB send failed: {}", e)))?;

        trace!("Sent CAN frame: ID=0x{:X}, len={}", frame.id, frame.len);
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
    fn receive(&mut self) -> Result<PiperFrame, CanError> {
        if !self.started {
            return Err(CanError::NotStarted);
        }

        // 1. 优先从队列中取（如果有上次读剩下的）
        if let Some(frame) = self.rx_queue.pop_front() {
            // 调试信息：验证 rx_queue 正在工作
            trace!(
                "Returning frame from queue (queue size: {})",
                self.rx_queue.len()
            );
            return Ok(frame);
        }

        // 2. 队列为空，从 USB 读取一批数据
        // USB 硬件可能将一个或多个 CAN 帧打包在一个 USB Bulk 包中
        // 我们需要一次性解析所有帧，并将它们放入队列
        loop {
            // 从 USB 读取一批帧
            let gs_frames = match self.device.receive_batch(Duration::from_millis(2)) {
                Ok(frames) => frames,
                Err(crate::can::gs_usb::error::GsUsbError::ReadTimeout) => {
                    return Err(CanError::Timeout);
                },
                Err(e) => {
                    return Err(CanError::Device(format!("USB receive failed: {}", e)));
                },
            };

            // 如果读取成功但没有帧（可能是空包），继续读下一个包
            // 注意：空包是正常情况，USB 硬件可能发送空的批量传输包
            // 超时时间短（2ms），影响不大，继续读取即可
            if gs_frames.is_empty() {
                continue;
            }

            // 3. 处理这批帧：过滤 Echo 和错误帧，将有效帧放入队列
            let is_loopback = (self.mode & GS_CAN_MODE_LOOP_BACK) != 0;

            // 优化：如果只有一个帧，且是有效帧，直接返回（避免队列操作）
            if gs_frames.len() == 1 {
                let gs_frame = &gs_frames[0];

                // 检查是否为有效帧
                let is_valid = if !is_loopback && gs_frame.is_tx_echo() {
                    // 是 Echo 且非 Loopback 模式，跳过
                    false
                } else if gs_frame.has_overflow() {
                    // 缓冲区溢出，立即返回错误
                    error!("CAN Buffer Overflow!");
                    return Err(CanError::BufferOverflow);
                } else {
                    // 有效帧
                    true
                };

                if is_valid {
                    // 直接返回，不需要队列操作
                    let frame = PiperFrame {
                        id: gs_frame.can_id & CAN_EFF_MASK,
                        data: gs_frame.data,
                        len: gs_frame.can_dlc.min(8),
                        is_extended: (gs_frame.can_id & CAN_EFF_FLAG) != 0,
                        timestamp_us: gs_frame.timestamp_us,
                    };
                    trace!("Received CAN frame: ID=0x{:X}, len={}", frame.id, frame.len);
                    return Ok(frame);
                }
                // 如果是 Echo 且非 Loopback，继续循环读取下一个包
                continue;
            }

            // 多个帧：批量处理并放入队列
            for gs_frame in gs_frames {
                // 3.1 过滤 TX Echo（静默丢弃）
                // 注意：在 Loopback 模式下，Echo 是测试的一部分，不应该被过滤
                if !is_loopback && gs_frame.is_tx_echo() {
                    trace!("Received TX echo (ignored)");
                    continue;
                }

                // 3.2 检查致命错误：缓冲区溢出
                if gs_frame.has_overflow() {
                    error!("CAN Buffer Overflow!");
                    return Err(CanError::BufferOverflow);
                }

                // 3.3 检查致命错误：Bus Off（需要通过 DeviceCapability 查询）
                // 这里假设通过 flags 或其他机制检测
                // 如果设备支持 GET_STATE，可以查询状态

                // 3.4 转换格式并放入队列（保留硬件时间戳）
                let frame = PiperFrame {
                    id: gs_frame.can_id & CAN_EFF_MASK, // 移除标志位
                    data: gs_frame.data,
                    len: gs_frame.can_dlc.min(8),
                    is_extended: (gs_frame.can_id & CAN_EFF_FLAG) != 0,
                    timestamp_us: gs_frame.timestamp_us, // 保留硬件时间戳
                };

                self.rx_queue.push_back(frame);
            }

            // 4. 如果队列里有东西了，返回第一个；否则继续循环读 USB
            // 注意：如果这批数据都被过滤掉了（例如全是 Echo 且非 Loopback），循环继续
            if let Some(frame) = self.rx_queue.pop_front() {
                trace!(
                    "Received CAN frame: ID=0x{:X}, len={} (queue size: {})",
                    frame.id,
                    frame.len,
                    self.rx_queue.len()
                );
                return Ok(frame);
            }
            // 如果这批数据都被过滤掉了，继续读下一个 USB 包
        }
    }
}
