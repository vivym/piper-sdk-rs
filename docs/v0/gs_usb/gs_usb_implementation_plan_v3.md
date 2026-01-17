# GS-USB CAN 适配层实现方案报告 v3（最终版）

> 基于专家评审的优化方案：轻量级适配层 + Fire-and-Forget 语义

## 1. 核心设计理念

### 1.1 架构决策

**放弃 `embedded_can`，采用自定义轻量级 Trait**

理由：
- ✅ `embedded_can` 为嵌入式设计，包含 `nb` 非阻塞等不适合桌面环境的概念
- ✅ Piper SDK 是专用控制 SDK，不需要通用 CAN 分析工具的功能
- ✅ 自定义 Trait 可以精确控制语义（Fire-and-Forget 发送、过滤 Echo）
- ✅ 零依赖负担，核心层完全独立

### 1.2 关键语义定义

| 操作 | 语义 | 实现方式 |
|------|------|---------|
| **`send()`** | Fire-and-Forget | USB Bulk OUT 成功即返回，**不等待 Echo** |
| **`receive()`** | 阻塞直到有效帧 | 内部循环过滤 Echo 和错误帧，只返回有效数据 |

### 1.3 错误处理策略（三层过滤漏斗）

1. **TX Echo 帧** → 静默丢弃（更新内部统计）
2. **瞬态错误** → 日志记录 + 自动重试（不返回错误）
3. **致命错误** → 向上传递（Bus Off、Buffer Overflow）

---

## 2. 架构设计

### 2.1 模块结构

```
src/can/
├── mod.rs              # 核心：PiperFrame + CanAdapter trait + CanError
├── traits.rs           # CanAdapter trait 定义（可选，也可放在 mod.rs）
├── socketcan_impl.rs   # [Linux] SocketCAN 后端实现
└── gs_usb/            # [macOS/Windows] GS-USB 后端实现
    ├── mod.rs          # GsUsbCanAdapter（实现 CanAdapter）
    ├── protocol.rs     # 协议常量、控制请求码
    ├── structures.rs   # DeviceBitTiming, DeviceMode, DeviceCapability
    ├── frame.rs        # GS-USB 帧编码/解码（CAN 2.0 only）
    ├── device.rs       # USB 设备操作（扫描、配置、传输）
    └── error.rs        # GS-USB 专用错误类型
```

### 2.2 核心数据结构

#### `PiperFrame` - SDK 统一帧格式

```rust
// src/can/mod.rs

/// SDK 通用的 CAN 帧定义（只针对 CAN 2.0）
///
/// 设计要点：
/// - Copy trait：零成本复制，适合高频场景
/// - 固定 8 字节数据：避免堆分配
/// - 无生命周期：简化 API
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PiperFrame {
    /// CAN ID（标准帧或扩展帧）
    pub id: u32,

    /// 帧数据（固定 8 字节，未使用部分为 0）
    pub data: [u8; 8],

    /// 有效数据长度 (0-8)
    pub len: u8,

    /// 是否为扩展帧（29-bit ID）
    pub is_extended: bool,
}

impl PiperFrame {
    /// 创建标准帧
    pub fn new_standard(id: u16, data: &[u8]) -> Self {
        Self::new(id as u32, data, false)
    }

    /// 创建扩展帧
    pub fn new_extended(id: u32, data: &[u8]) -> Self {
        Self::new(id, data, true)
    }

    /// 通用构造器
    fn new(id: u32, data: &[u8], is_extended: bool) -> Self {
        let mut fixed_data = [0u8; 8];
        let len = data.len().min(8);
        fixed_data[..len].copy_from_slice(&data[..len]);

        Self {
            id,
            data: fixed_data,
            len: len as u8,
            is_extended,
        }
    }

    /// 获取数据切片（只包含有效数据）
    pub fn data_slice(&self) -> &[u8] {
        &self.data[..self.len as usize]
    }
}
```

#### `CanError` - 统一错误类型

```rust
// src/can/mod.rs

use thiserror::Error;

/// CAN 适配层统一错误类型
#[derive(Error, Debug)]
pub enum CanError {
    /// USB/IO 底层错误
    #[error("IO Error: {0}")]
    Io(#[from] std::io::Error),

    /// 设备相关错误（设备未找到、未启动、配置失败等）
    #[error("Device Error: {0}")]
    Device(String),

    /// 读取超时（非致命，可以重试）
    #[error("Read timeout")]
    Timeout,

    /// 缓冲区溢出（致命错误）
    #[error("Buffer overflow")]
    BufferOverflow,

    /// 总线关闭（致命错误，需要重启）
    #[error("Bus off")]
    BusOff,

    /// 设备未启动
    #[error("Device not started")]
    NotStarted,
}
```

#### `CanAdapter` - 核心 Trait

```rust
// src/can/mod.rs (或 traits.rs)

/// CAN 适配器 Trait
///
/// 语义：
/// - `send()`: Fire-and-Forget，USB 写入成功即返回
/// - `receive()`: 阻塞直到收到有效数据帧或超时
pub trait CanAdapter {
    /// 发送一帧
    ///
    /// # 语义
    /// - **Fire-and-Forget**：将帧放入发送缓冲区即返回
    /// - **不等待 Echo**：不阻塞等待 USB echo 确认
    /// - **返回条件**：USB Bulk OUT 写入成功
    ///
    /// # 错误处理
    /// - 设备未启动 → `CanError::NotStarted`
    /// - USB 写入失败 → `CanError::Io` 或 `CanError::Device`
    fn send(&mut self, frame: PiperFrame) -> Result<(), CanError>;

    /// 接收一帧
    ///
    /// # 语义
    /// - **阻塞读取**：直到收到有效数据帧或超时
    /// - **自动过滤**：内部过滤 Echo 帧和瞬态错误
    /// - **只返回有效数据**：过滤后的 CAN 总线数据
    ///
    /// # 错误处理
    /// - 超时 → `CanError::Timeout`（可重试）
    /// - 缓冲区溢出 → `CanError::BufferOverflow`（致命）
    /// - 总线关闭 → `CanError::BusOff`（致命）
    /// - 设备未启动 → `CanError::NotStarted`
    fn receive(&mut self) -> Result<PiperFrame, CanError>;
}
```

---

## 3. GS-USB 实现详解

### 3.1 模块划分（复用参考实现）

```
src/can/gs_usb/
├── mod.rs          # GsUsbCanAdapter（实现 CanAdapter）
├── protocol.rs     # 协议常量 + 协议结构体（DeviceBitTiming 等）
├── frame.rs        # GS-USB 帧编码/解码（CAN 2.0 only）
├── device.rs       # USB 设备操作（扫描、配置、传输）
└── error.rs        # GS-USB 专用错误（映射到 CanError）
```

### 3.2 `protocol.rs` - 协议常量 + 结构体

包含两部分：
1. **协议常量**：控制请求码、模式标志、端点地址等
2. **协议结构体**：`DeviceBitTiming`、`DeviceMode`、`DeviceCapability` 等

#### 协议常量（简化版）

```rust
// 只保留 CAN 2.0 相关的常量，移除所有 CAN FD

// Control Request Codes
pub const GS_USB_BREQ_HOST_FORMAT: u8 = 0;      // 协议握手 + 字节序配置（必须发送，但可忽略错误）
pub const GS_USB_BREQ_BITTIMING: u8 = 1;
pub const GS_USB_BREQ_MODE: u8 = 2;
pub const GS_USB_BREQ_BT_CONST: u8 = 4;
pub const GS_USB_BREQ_DEVICE_CONFIG: u8 = 5;

// Mode Flags
pub const GS_CAN_MODE_NORMAL: u32 = 0;
pub const GS_CAN_MODE_LISTEN_ONLY: u32 = 1 << 0;
pub const GS_CAN_MODE_LOOP_BACK: u32 = 1 << 1;
pub const GS_CAN_MODE_TRIPLE_SAMPLE: u32 = 1 << 2;  // 三重采样模式
pub const GS_CAN_MODE_ONE_SHOT: u32 = 1 << 3;

// Mode Values
pub const GS_CAN_MODE_RESET: u32 = 0;
pub const GS_CAN_MODE_START: u32 = 1;

// CAN ID Flags
pub const CAN_EFF_FLAG: u32 = 0x8000_0000;
pub const CAN_RTR_FLAG: u32 = 0x4000_0000;
pub const CAN_EFF_MASK: u32 = 0x1FFF_FFFF;
pub const CAN_SFF_MASK: u32 = 0x0000_07FF;

// Frame Constants
pub const GS_USB_ECHO_ID: u32 = 0;              // TX 帧
pub const GS_USB_RX_ECHO_ID: u32 = 0xFFFF_FFFF; // RX 帧
pub const CAN_MAX_DLEN: usize = 8;
pub const GS_USB_FRAME_SIZE: usize = 20;        // 无时间戳

// Frame Flags
pub const GS_CAN_FLAG_OVERFLOW: u8 = 1 << 0;

// USB Endpoints
pub const GS_USB_ENDPOINT_OUT: u8 = 0x02;
pub const GS_USB_ENDPOINT_IN: u8 = 0x81;

// USB Request Types
pub const GS_USB_REQ_OUT: u8 = 0x41;
pub const GS_USB_REQ_IN: u8 = 0xC1;
```

#### 协议结构体

```rust
/// CAN 位定时配置
#[derive(Debug, Clone, Copy)]
pub struct DeviceBitTiming {
    pub prop_seg: u32,
    pub phase_seg1: u32,
    pub phase_seg2: u32,
    pub sjw: u32,
    pub brp: u32,
}

impl DeviceBitTiming {
    pub fn new(prop_seg: u32, phase_seg1: u32, phase_seg2: u32, sjw: u32, brp: u32) -> Self {
        Self { prop_seg, phase_seg1, phase_seg2, sjw, brp }
    }

    /// Pack into bytes for USB transfer (20 bytes)
    pub fn pack(&self) -> [u8; 20] {
        let mut buf = [0u8; 20];
        buf[0..4].copy_from_slice(&self.prop_seg.to_le_bytes());
        buf[4..8].copy_from_slice(&self.phase_seg1.to_le_bytes());
        buf[8..12].copy_from_slice(&self.phase_seg2.to_le_bytes());
        buf[12..16].copy_from_slice(&self.sjw.to_le_bytes());
        buf[16..20].copy_from_slice(&self.brp.to_le_bytes());
        buf
    }
}

/// 设备模式配置
#[derive(Debug, Clone, Copy)]
pub struct DeviceMode {
    pub mode: u32,  // GS_CAN_MODE_START or GS_CAN_MODE_RESET
    pub flags: u32, // Mode flags
}

impl DeviceMode {
    pub fn new(mode: u32, flags: u32) -> Self {
        Self { mode, flags }
    }

    /// Pack into bytes for USB transfer (8 bytes)
    pub fn pack(&self) -> [u8; 8] {
        let mut buf = [0u8; 8];
        buf[0..4].copy_from_slice(&self.mode.to_le_bytes());
        buf[4..8].copy_from_slice(&self.flags.to_le_bytes());
        buf
    }
}

/// 设备能力（位定时约束和功能标志）
#[derive(Debug, Clone, Copy)]
pub struct DeviceCapability {
    pub feature: u32,     // 功能标志位
    pub fclk_can: u32,    // CAN 时钟频率（Hz）
    pub tseg1_min: u32,
    pub tseg1_max: u32,
    pub tseg2_min: u32,
    pub tseg2_max: u32,
    pub sjw_max: u32,
    pub brp_min: u32,
    pub brp_max: u32,
    pub brp_inc: u32,
}

impl DeviceCapability {
    /// Unpack from BT_CONST response (40 bytes)
    pub fn unpack(data: &[u8]) -> Self {
        Self {
            feature: u32::from_le_bytes([data[0], data[1], data[2], data[3]]),
            fclk_can: u32::from_le_bytes([data[4], data[5], data[6], data[7]]),
            tseg1_min: u32::from_le_bytes([data[8], data[9], data[10], data[11]]),
            tseg1_max: u32::from_le_bytes([data[12], data[13], data[14], data[15]]),
            tseg2_min: u32::from_le_bytes([data[16], data[17], data[18], data[19]]),
            tseg2_max: u32::from_le_bytes([data[20], data[21], data[22], data[23]]),
            sjw_max: u32::from_le_bytes([data[24], data[25], data[26], data[27]]),
            brp_min: u32::from_le_bytes([data[28], data[29], data[30], data[31]]),
            brp_max: u32::from_le_bytes([data[32], data[33], data[34], data[35]]),
            brp_inc: u32::from_le_bytes([data[36], data[37], data[38], data[39]]),
        }
    }
}

/// 设备信息（固件版本、通道数等）
#[derive(Debug, Clone, Copy)]
pub struct DeviceInfo {
    pub icount: u8,        // 通道数 - 1
    pub fw_version: u32,   // 固件版本（实际版本 = fw_version / 10）
    pub hw_version: u32,   // 硬件版本（实际版本 = hw_version / 10）
}

impl DeviceInfo {
    pub fn unpack(data: &[u8]) -> Self {
        Self {
            icount: data[3],
            fw_version: u32::from_le_bytes([data[4], data[5], data[6], data[7]]),
            hw_version: u32::from_le_bytes([data[8], data[9], data[10], data[11]]),
        }
    }

    pub fn channel_count(&self) -> u8 {
        self.icount + 1
    }
}
```

### 3.3 `frame.rs` - GS-USB 帧（简化版）

```rust
use bytes::{Bytes, BytesMut, Buf, BufMut};
use crate::can::gs_usb::protocol::*;

/// GS-USB CAN 2.0 帧（不支持 CAN FD）
#[derive(Debug, Clone)]
pub struct GsUsbFrame {
    pub echo_id: u32,        // 0 = TX, 0xFFFFFFFF = RX
    pub can_id: u32,         // CAN ID（带 EFF/RTR 标志）
    pub can_dlc: u8,         // Data Length Code (0-8)
    pub channel: u8,         // CAN 通道号
    pub flags: u8,           // GS-USB 标志（OVERFLOW 等）
    pub reserved: u8,
    pub data: [u8; 8],       // 固定 8 字节（CAN 2.0）
}

impl GsUsbFrame {
    /// Pack frame into BytesMut
    pub fn pack_to(&self, buf: &mut BytesMut) {
        buf.reserve(GS_USB_FRAME_SIZE);
        buf.put_u32_le(self.echo_id);
        buf.put_u32_le(self.can_id);
        buf.put_u8(self.can_dlc);
        buf.put_u8(self.channel);
        buf.put_u8(self.flags);
        buf.put_u8(self.reserved);
        buf.put_slice(&self.data);
    }

    /// Unpack from Bytes
    pub fn unpack_from_bytes(&mut self, mut data: Bytes) -> Result<(), GsUsbError> {
        if data.len() < GS_USB_FRAME_SIZE {
            return Err(GsUsbError::InvalidFrame("Frame too short".to_string()));
        }

        self.echo_id = data.get_u32_le();
        self.can_id = data.get_u32_le();
        self.can_dlc = data.get_u8();
        self.channel = data.get_u8();
        self.flags = data.get_u8();
        self.reserved = data.get_u8();

        data.copy_to_slice(&mut self.data);

        Ok(())
    }

    /// Check if this is an RX frame (from CAN bus)
    pub fn is_rx_frame(&self) -> bool {
        self.echo_id == GS_USB_RX_ECHO_ID
    }

    /// Check if this is a TX echo (confirmation)
    pub fn is_tx_echo(&self) -> bool {
        self.echo_id != GS_USB_RX_ECHO_ID
    }

    /// Check for buffer overflow
    pub fn has_overflow(&self) -> bool {
        (self.flags & GS_CAN_FLAG_OVERFLOW) != 0
    }
}
```

### 3.4 `device.rs` - USB 设备操作（复用参考实现核心逻辑）

**关键改进**：
- 使用 `bytes::BytesMut` 作为发送缓冲区
- 移除所有 CAN FD 相关逻辑
- 添加 `tracing` 日志

**依赖 `protocol.rs`**：
```rust
use crate::can::gs_usb::protocol::{DeviceBitTiming, DeviceMode, DeviceCapability, ...};
```

**核心方法**（复用参考实现的逻辑）：
- `scan()` - 扫描设备（复用参考实现）
- `send_host_format()` - **协议握手**（必须发送，但可忽略错误）
  ```rust
  /// 发送 Host Format (0xBEEF) 进行握手
  ///
  /// **重要性**：虽然设备和主机都是 Little Endian，但这个请求充当：
  /// 1. 协议握手信号 - 某些固件在收到此命令前可能处于未初始化状态
  /// 2. 字节序配置 - 告知设备主机的字节序（虽然现代设备通常默认 LE）
  ///
  /// **策略**：Fire-and-Forget for Handshake
  /// - 尝试发送，但忽略错误（设备可能不支持或已默认 LE）
  /// - 即使失败也不阻断后续流程
  fn send_host_format(&self) -> Result<(), GsUsbError> {
      let val: u32 = 0x0000_BEEF;
      let data = val.to_le_bytes();

      // 短超时（100ms），忽略错误
      let _ = self.handle.write_control(
          0x41, // Host to Device | Vendor | Interface
          GS_USB_BREQ_HOST_FORMAT,
          0,    // Value（参考实现使用 0）
          0,    // wIndex（大多数控制请求使用 0）
          &data,
          Duration::from_millis(100),
      );

      Ok(()) // 始终返回成功，不阻断流程
  }
  ```
- `start()` / `stop()` - 启动/停止（复用参考实现）
- `set_bitrate()` - 设置波特率（复用预定义表）
- `send_raw()` - 发送原始 GS-USB 帧（Fire-and-Forget）
- `receive_raw()` - 接收原始 GS-USB 帧（带超时）

**设备初始化流程**（参考 Linux 内核驱动）：
1. `scan()` - 扫描并打开设备
2. **`send_host_format()` - 协议握手**（必须发送，忽略错误）
   - 发送 `0x0000_BEEF`（little-endian）
   - 作用：协议复位 + 字节序配置
   - 策略：Fire-and-Forget（尽力发送，失败不阻断）
3. `set_bitrate()` - 配置波特率
4. `start()` - 启动 CAN 通道

### 3.5 `mod.rs` - `GsUsbCanAdapter` 实现

```rust
use crate::can::{CanAdapter, PiperFrame, CanError};
use crate::can::gs_usb::device::GsUsbDevice;
use crate::can::gs_usb::frame::GsUsbFrame;
use crate::can::gs_usb::protocol::*;
use tracing::{trace, warn, error};
use std::time::Duration;

pub struct GsUsbCanAdapter {
    device: GsUsbDevice,
    started: bool,
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
        })
    }

    /// 配置并启动设备
    pub fn configure(&mut self, bitrate: u32) -> Result<(), CanError> {
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
        self.device.set_bitrate(bitrate)
            .map_err(|e| CanError::Device(format!("Failed to set bitrate: {}", e)))?;

        // 3. 启动设备
        self.device.start(GS_CAN_MODE_NORMAL)
            .map_err(|e| CanError::Device(format!("Failed to start device: {}", e)))?;

        self.started = true;
        trace!("GS-USB device started at {} bps", bitrate);
        Ok(())
    }
}

impl CanAdapter for GsUsbCanAdapter {
    /// 发送帧（Fire-and-Forget）
    fn send(&mut self, frame: PiperFrame) -> Result<(), CanError> {
        if !self.started {
            return Err(CanError::NotStarted);
        }

        // 1. 转换 PiperFrame -> GsUsbFrame
        let gs_frame = {
            let mut gs = GsUsbFrame {
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
            };
            gs
        };

        // 2. 发送 USB Bulk OUT（不等待 Echo）
        self.device.send_raw(&gs_frame)
            .map_err(|e| CanError::Device(format!("USB send failed: {}", e)))?;

        trace!("Sent CAN frame: ID=0x{:X}, len={}", frame.id, frame.len);
        Ok(())
    }

    /// 接收帧（三层过滤漏斗）
    fn receive(&mut self) -> Result<PiperFrame, CanError> {
        if !self.started {
            return Err(CanError::NotStarted);
        }

        // 内部循环：过滤 Echo 和错误帧
        loop {
            // 1. 尝试从 USB 读取（短暂超时，避免死锁）
            let gs_frame = match self.device.receive_raw(Duration::from_millis(2)) {
                Ok(f) => f,
                Err(crate::can::gs_usb::error::GsUsbError::ReadTimeout) => {
                    return Err(CanError::Timeout);
                }
                Err(e) => {
                    return Err(CanError::Device(format!("USB receive failed: {}", e)));
                }
            };

            // 2. 过滤 TX Echo（静默丢弃）
            if gs_frame.is_tx_echo() {
                // 可选：更新内部统计 self.stats.tx_confirmed += 1;
                trace!("Received TX echo (ignored)");
                continue;
            }

            // 3. 检查致命错误：缓冲区溢出
            if gs_frame.has_overflow() {
                error!("CAN Buffer Overflow!");
                return Err(CanError::BufferOverflow);
            }

            // 4. 检查致命错误：Bus Off（需要通过 DeviceCapability 查询）
            // 这里假设通过 flags 或其他机制检测
            // 如果设备支持 GET_STATE，可以查询状态

            // 5. 返回有效数据帧
            let frame = PiperFrame {
                id: gs_frame.can_id & CAN_EFF_MASK, // 移除标志位
                data: gs_frame.data,
                len: gs_frame.can_dlc.min(8),
                is_extended: (gs_frame.can_id & CAN_EFF_FLAG) != 0,
            };

            trace!("Received CAN frame: ID=0x{:X}, len={}", frame.id, frame.len);
            return Ok(frame);
        }
    }
}
```

---

## 4. SocketCAN 实现（Linux 后端）

```rust
// src/can/socketcan_impl.rs

#[cfg(target_os = "linux")]
use socketcan::{CanSocket, Socket, CANFrame};
#[cfg(target_os = "linux")]
use crate::can::{CanAdapter, PiperFrame, CanError};

#[cfg(target_os = "linux")]
pub struct SocketCanAdapter {
    socket: CanSocket,
}

#[cfg(target_os = "linux")]
impl SocketCanAdapter {
    pub fn new(interface: &str) -> Result<Self, CanError> {
        let socket = CanSocket::open(interface)
            .map_err(|e| CanError::Io(std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("Failed to open CAN socket: {}", e)
            )))?;

        Ok(Self { socket })
    }
}

#[cfg(target_os = "linux")]
impl CanAdapter for SocketCanAdapter {
    fn send(&mut self, frame: PiperFrame) -> Result<(), CanError> {
        let can_frame = CANFrame::new(
            frame.id,
            frame.data_slice(),
            frame.is_extended,
            false, // RTR
        ).map_err(|_| CanError::Device("Invalid frame".to_string()))?;

        self.socket.write_frame(&can_frame)
            .map_err(|e| CanError::Io(std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("Socket write failed: {}", e)
            )))?;

        Ok(())
    }

    fn receive(&mut self) -> Result<PiperFrame, CanError> {
        let can_frame = self.socket.read_frame()
            .map_err(|e| CanError::Io(std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("Socket read failed: {}", e)
            )))?;

        let mut data = [0u8; 8];
        let frame_data = can_frame.data();
        data[..frame_data.len()].copy_from_slice(frame_data);

        Ok(PiperFrame {
            id: can_frame.id(),
            data,
            len: frame_data.len() as u8,
            is_extended: can_frame.is_extended(),
        })
    }
}
```

---

## 5. 统一入口（条件编译）

```rust
// src/can/mod.rs

pub mod traits;

// 导出核心类型
pub use traits::CanAdapter;
pub use crate::can::frame::PiperFrame;
pub use crate::can::error::CanError;

// 条件编译：选择后端
#[cfg(target_os = "linux")]
pub mod socketcan_impl;
#[cfg(target_os = "linux")]
pub use socketcan_impl::SocketCanAdapter as BackendCanAdapter;

#[cfg(not(target_os = "linux"))]
pub mod gs_usb;
#[cfg(not(target_os = "linux"))]
pub use gs_usb::GsUsbCanAdapter as BackendCanAdapter;

// 工厂函数
pub fn create_can_adapter(interface: Option<&str>) -> Result<Box<dyn CanAdapter>, CanError> {
    #[cfg(target_os = "linux")]
    {
        let iface = interface.unwrap_or("can0");
        Ok(Box::new(SocketCanAdapter::new(iface)?))
    }

    #[cfg(not(target_os = "linux"))]
    {
        Ok(Box::new(GsUsbCanAdapter::new()?))
    }
}
```

---

## 6. 实现优先级与里程碑

### Phase 1: 核心框架（1 天）
- [ ] 定义 `PiperFrame` 和 `CanError`
- [ ] 定义 `CanAdapter` trait
- [ ] 编写单元测试

### Phase 2: GS-USB 协议层（2-3 天）
- [ ] 实现 `protocol.rs`（常量 + 结构体，简化版）
- [ ] 实现 `frame.rs`（CAN 2.0 only，使用 bytes）

### Phase 3: GS-USB 设备层（2-3 天）
- [ ] 实现 `device.rs`（扫描、配置、传输）
- [ ] 集成波特率预定义表
- [ ] 测试设备初始化流程

### Phase 4: GS-USB 适配层（1-2 天）
- [ ] 实现 `GsUsbCanAdapter`（`CanAdapter` trait）
- [ ] 实现 Fire-and-Forget `send()`
- [ ] 实现三层过滤漏斗 `receive()`

### Phase 5: SocketCAN 适配层（1 天，可选）
- [ ] 实现 `SocketCanAdapter`
- [ ] 测试 Linux 后端

### Phase 6: 集成测试（1 天）
- [ ] 单元测试（帧编码/解码）
- [ ] 集成测试（实际硬件）
- [ ] 性能测试（1kHz 收发）

---

## 7. 关键设计决策总结

| 决策项 | 方案 | 理由 |
|--------|------|------|
| **Trait 选择** | 自定义 `CanAdapter` | 避免 `embedded_can` 的嵌入式负担 |
| **发送语义** | Fire-and-Forget | 保证 1kHz 控制回路的低延迟 |
| **接收语义** | 阻塞 + 内部过滤 | 自动处理 Echo，简化上层逻辑 |
| **错误处理** | 三层过滤漏斗 | 只传递致命错误，瞬态错误自动恢复 |
| **帧格式** | `PiperFrame`（固定 8 字节） | 零成本复制，无堆分配 |
| **CAN FD** | ❌ 不支持 | Piper 不需要，简化实现 |

---

## 8. 与参考实现的对比

| 特性 | 参考实现 | 我们的方案 |
|------|---------|-----------|
| **接口** | 独立库（`GsUsb`） | `CanAdapter` trait（统一接口） |
| **Frame 类型** | `GsUsbFrame` | `PiperFrame`（跨后端） |
| **发送语义** | 不等待 Echo ✅ | 不等待 Echo ✅ |
| **接收语义** | 直接返回所有帧 | 自动过滤 Echo ✅ |
| **错误处理** | 返回所有错误 | 三层过滤漏斗 ✅ |
| **CAN FD** | 支持 | ❌ 不支持（简化） |

---

## 总结

这个方案的核心优势：

1. ✅ **轻量级**：核心层无外部依赖，只有具体实现才依赖 `rusb` 或 `socketcan`
2. ✅ **高性能**：Fire-and-Forget 发送，自动过滤 Echo，零拷贝设计
3. ✅ **易用性**：统一的 `CanAdapter` 接口，上层业务代码与后端解耦
4. ✅ **可测试**：可以轻松实现 `MockCanAdapter` 进行单元测试
5. ✅ **可扩展**：未来可以轻松添加其他后端（如虚拟设备、模拟器）

**预计实现时间**：7-10 个工作日（基于参考实现的代码复用）。

