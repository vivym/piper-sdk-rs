# GS-USB CAN 适配层实现方案报告 v2

> 基于参考实现 `tmp/gs_usb_rs` 的深入分析和改进建议

## 1. 参考实现核心发现

### 1.1 架构优势

参考实现采用了清晰的模块化设计：

```
constants.rs  → 所有协议常量（控制请求码、模式标志、帧标志等）
structures.rs → 协议结构体（DeviceBitTiming, DeviceMode, DeviceInfo, DeviceCapability）
frame.rs      → CAN 帧编码/解码（pack/unpack）
device.rs     → USB 设备操作（扫描、配置、发送/接收）
error.rs      → 错误类型定义（使用 thiserror）
```

**可借鉴点**：
- ✅ **分离协议定义与实现**：协议相关的常量、结构体单独模块，便于维护
- ✅ **帧格式的 pack/unpack**：使用固定大小的字节数组，避免动态分配
- ✅ **设备能力缓存**：获取一次 `DeviceCapability` 后缓存，避免重复查询

### 1.2 关键设计亮点

#### 1.2.1 波特率预定义表

参考实现针对**80MHz**和**40MHz**两种常见时钟提供了完整的波特率映射表：

```rust
// device.rs:174-217
match clock {
    80_000_000 => match bitrate {
        10_000 => Some((87, 87, 25, 12, 40)),
        20_000 => Some((87, 87, 25, 12, 20)),
        // ... 125k, 250k, 500k, 1M
    },
    40_000_000 => match bitrate {
        // 对应的 40MHz 映射
    },
}
```

**优势**：
- ✅ 直接查询，无需运行时计算
- ✅ 已验证的位定时参数，保证兼容性
- ✅ 通过 `device_capability()` 获取实际时钟频率

#### 1.2.2 帧格式设计

参考实现使用**固定大小的结构体**表示 GS-USB 帧：

```rust
// frame.rs:44-62
pub struct GsUsbFrame {
    pub echo_id: u32,      // TX: 0, RX: 0xFFFFFFFF
    pub can_id: u32,       // CAN ID (带 EFF/RTR 标志)
    pub can_dlc: u8,       // Data Length Code
    pub channel: u8,       // CAN 通道号
    pub flags: u8,         // GS-USB 标志（FD, BRS等）
    pub reserved: u8,
    pub data: [u8; 64],    // 支持 CAN FD（最大64字节）
    pub timestamp_us: u32, // 硬件时间戳（可选）
}
```

**帧大小**：
- Classic CAN: **20 字节**（无时间戳）或 **24 字节**（有时间戳）
- CAN FD: **76 字节**（无时间戳）或 **80 字节**（有时间戳）

**可借鉴点**：
- ✅ 清晰的字段定义
- ✅ `pack()`/`unpack_from()` 方法封装字节转换
- ✅ 支持硬件时间戳（虽然 Piper 可能不需要）

#### 1.2.3 设备初始化流程

```rust
// device.rs:106-145
pub fn start(&mut self, flags: u32) -> Result<()> {
    // 1. Reset 设备
    self.handle.reset()?;

    // 2. Detach kernel driver (Linux/macOS)
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        if self.handle.kernel_driver_active(0).unwrap_or(false) {
            self.handle.detach_kernel_driver(0)?;
        }
    }

    // 3. Claim interface
    self.handle.claim_interface(0)?;

    // 4. 获取设备能力（检查功能支持）
    let capability = self.device_capability()?;
    let flags = flags & capability.feature; // 只保留设备支持的功能

    // 5. 设置模式并启动
    let mode = DeviceMode::new(GS_CAN_MODE_START, flags);
    self.control_out(GS_USB_BREQ_MODE, 0, &mode.pack())?;

    self.started = true;
    Ok(())
}
```

**可借鉴点**：
- ✅ **Reset 设备**：确保干净状态
- ✅ **Kernel driver detach**：macOS 上虽然通常不需要，但保留兼容性
- ✅ **Feature mask**：只启用设备支持的功能

#### 1.2.4 控制传输封装

```rust
// device.rs:475-513
fn control_out(&self, request: u8, value: u16, data: &[u8]) -> Result<()> {
    self.handle.write_control(
        0x41, // bmRequestType: vendor, host-to-device
        request,
        value,
        0, // wIndex
        data,
        Duration::from_millis(1000),
    )?;
    Ok(())
}

fn control_in(&self, request: u8, value: u16, length: usize) -> Result<Vec<u8>> {
    let mut buf = vec![0u8; length];
    let len = self.handle.read_control(
        0xC1, // bmRequestType: vendor, device-to-host
        request,
        value,
        0,
        &mut buf,
        Duration::from_millis(1000),
    )?;
    // 验证长度...
    Ok(buf)
}
```

**关键点**：
- ✅ **请求类型固定**：`0x41` (OUT) / `0xC1` (IN)
- ✅ **超时时间**：1000ms（合理值）
- ✅ **长度验证**：IN 传输后验证返回数据长度

---

## 2. 我们方案的改进点

### 2.1 简化：只支持 CAN 2.0

**参考实现支持 CAN FD**，但我们只需要 Classic CAN：

```rust
// ❌ 不需要：CAN FD 相关代码
pub const GS_CAN_MODE_FD: u32 = 1 << 8;
pub const GS_CAN_FLAG_FD: u8 = 1 << 1;
pub const CANFD_MAX_DLEN: usize = 64;
pub const GS_USB_FRAME_SIZE_FD: usize = 76;

// ✅ 只需要：Classic CAN
pub const CAN_MAX_DLEN: usize = 8;
pub const GS_USB_FRAME_SIZE: usize = 20;  // 无时间戳
pub const GS_USB_FRAME_SIZE_HW_TIMESTAMP: usize = 24;  // 有时间戳
```

**改进方案**：
- 移除所有 CAN FD 相关常量、标志和逻辑
- 帧的 `data` 字段从 `[u8; 64]` 改为 `[u8; 8]`
- 简化 `pack()`/`unpack()` 方法，不需要 `fd_mode` 参数

### 2.2 实现 `embedded_can::blocking::Can` trait

**参考实现的限制**：它是一个独立的库，不实现 `embedded_can` trait。

**我们的目标**：需要实现：

```rust
use embedded_can::blocking::Can;

impl Can for GsUsbDriver {
    type Frame = embedded_can::Frame;  // 使用 embedded-can 的 Frame 类型
    type Error = DriverError;

    fn transmit(&mut self, frame: &Self::Frame) -> Result<(), Self::Error> {
        // 1. 转换为 GS-USB 帧格式
        // 2. 发送 Bulk OUT
        // 3. （可选）等待 echo 确认
    }

    fn receive(&mut self) -> Result<Self::Frame, Self::Error> {
        // 1. 从 Bulk IN 读取
        // 2. 解析为 GS-USB 帧
        // 3. 转换为 embedded_can::Frame
    }
}
```

**关键挑战**：
- `embedded_can::Frame` 的具体 API 需要查阅文档
- 需要实现 `Frame` 与 `GsUsbFrame` 之间的转换

### 2.3 使用 `bytes` crate 优化

**参考实现**：直接使用 `Vec<u8>` 和 `[u8; N]`。

**改进方案**：使用 `bytes::Bytes` 和 `bytes::BytesMut` 实现零拷贝：

```rust
use bytes::{Bytes, BytesMut, BufMut};

impl GsUsbFrame {
    /// Pack frame into BytesMut (零拷贝准备)
    pub fn pack_to(&self, buf: &mut BytesMut, hw_timestamp: bool) {
        buf.reserve(GS_USB_FRAME_SIZE + if hw_timestamp { 4 } else { 0 });
        buf.put_u32_le(self.echo_id);
        buf.put_u32_le(self.can_id);
        buf.put_u8(self.can_dlc);
        buf.put_u8(self.channel);
        buf.put_u8(self.flags);
        buf.put_u8(self.reserved);
        buf.put_slice(&self.data[..8]); // 只取8字节（CAN 2.0）
        if hw_timestamp {
            buf.put_u32_le(self.timestamp_us);
        }
    }

    /// Unpack from Bytes
    pub fn unpack_from_bytes(&mut self, data: Bytes, hw_timestamp: bool) -> Result<()> {
        // 使用 bytes::Buf trait 读取
        // ...
    }
}
```

**优势**：
- ✅ **零拷贝潜力**：如果 USB 库支持，可以直接使用底层缓冲区
- ✅ **统一的字节处理**：所有字节操作使用 `bytes` API

### 2.4 错误处理改进

**参考实现的错误类型**已经很好，但我们可以针对 `embedded_can` 做适配：

```rust
use thiserror::Error;

#[derive(Error, Debug)]
pub enum DriverError {
    #[error("USB error: {0}")]
    Usb(#[from] rusb::Error),

    #[error("Device not found")]
    DeviceNotFound,

    #[error("Device not started")]
    NotStarted,

    #[error("Unsupported bitrate {bitrate} for clock {clock_hz} Hz")]
    UnsupportedBitrate { bitrate: u32, clock_hz: u32 },

    #[error("Read timeout")]
    ReadTimeout,

    #[error("Invalid frame format: {0}")]
    InvalidFrame(String),

    // 新增：embedded_can 转换错误
    #[error("Frame conversion error: {0}")]
    FrameConversion(String),
}
```

---

## 3. 具体实现方案（基于参考实现优化）

### 3.1 模块结构

```
src/can/gs_usb/
├── mod.rs          # 实现 embedded_can::blocking::Can trait
├── protocol.rs     # 协议常量、控制请求码、标志位
├── structures.rs   # DeviceBitTiming, DeviceMode, DeviceCapability
├── frame.rs        # GS-USB 帧编码/解码（只支持 CAN 2.0）
└── device.rs       # USB 设备操作（扫描、配置、传输）
```

### 3.2 protocol.rs - 协议常量

```rust
// Control Request Codes
pub const GS_USB_BREQ_HOST_FORMAT: u8 = 0;
pub const GS_USB_BREQ_BITTIMING: u8 = 1;
pub const GS_USB_BREQ_MODE: u8 = 2;
pub const GS_USB_BREQ_BERR: u8 = 3;
pub const GS_USB_BREQ_BT_CONST: u8 = 4;
pub const GS_USB_BREQ_DEVICE_CONFIG: u8 = 5;

// Mode Flags (只保留 CAN 2.0 相关的)
pub const GS_CAN_MODE_NORMAL: u32 = 0;
pub const GS_CAN_MODE_LISTEN_ONLY: u32 = 1 << 0;
pub const GS_CAN_MODE_LOOP_BACK: u32 = 1 << 1;
pub const GS_CAN_MODE_TRIPLE_SAMPLE: u32 = 1 << 2;
pub const GS_CAN_MODE_ONE_SHOT: u32 = 1 << 3;
pub const GS_CAN_MODE_HW_TIMESTAMP: u32 = 1 << 4;  // 可选，Piper可能不需要

// Mode Values
pub const GS_CAN_MODE_RESET: u32 = 0;
pub const GS_CAN_MODE_START: u32 = 1;

// CAN ID Flags
pub const CAN_EFF_FLAG: u32 = 0x8000_0000;
pub const CAN_RTR_FLAG: u32 = 0x4000_0000;
pub const CAN_EFF_MASK: u32 = 0x1FFF_FFFF;
pub const CAN_SFF_MASK: u32 = 0x0000_07FF;

// Frame Constants
pub const GS_USB_ECHO_ID: u32 = 0;              // TX 帧 echo_id
pub const GS_USB_RX_ECHO_ID: u32 = 0xFFFF_FFFF; // RX 帧 echo_id
pub const CAN_MAX_DLEN: usize = 8;
pub const GS_USB_FRAME_SIZE: usize = 20;        // 无时间戳
pub const GS_USB_FRAME_SIZE_HW_TIMESTAMP: usize = 24; // 有时间戳

// USB Endpoints
pub const GS_USB_ENDPOINT_OUT: u8 = 0x02;
pub const GS_USB_ENDPOINT_IN: u8 = 0x81;

// USB Request Types
pub const GS_USB_REQ_OUT: u8 = 0x41; // vendor, host-to-device
pub const GS_USB_REQ_IN: u8 = 0xC1;  // vendor, device-to-host
```

### 3.3 structures.rs - 协议结构体

**直接复用参考实现**，但移除 CAN FD 相关字段：

```rust
#[derive(Debug, Clone, Copy)]
pub struct DeviceBitTiming {
    pub prop_seg: u32,
    pub phase_seg1: u32,
    pub phase_seg2: u32,
    pub sjw: u32,
    pub brp: u32,
}

impl DeviceBitTiming {
    pub fn pack(&self) -> [u8; 20] {
        // 复用参考实现的 pack() 方法
    }
}

#[derive(Debug, Clone, Copy)]
pub struct DeviceMode {
    pub mode: u32,  // GS_CAN_MODE_START or GS_CAN_MODE_RESET
    pub flags: u32, // Mode flags
}

impl DeviceMode {
    pub fn pack(&self) -> [u8; 8] {
        // 复用参考实现的 pack() 方法
    }
}

#[derive(Debug, Clone, Copy)]
pub struct DeviceCapability {
    pub feature: u32,
    pub fclk_can: u32,
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
    pub fn unpack(data: &[u8]) -> Self {
        // 复用参考实现的 unpack() 方法
    }
}
```

### 3.4 frame.rs - CAN 2.0 帧（简化版）

```rust
use bytes::{Bytes, BytesMut, Buf, BufMut};
use crate::can::gs_usb::protocol::*;

/// GS-USB CAN 2.0 帧（不支持 CAN FD）
#[derive(Debug, Clone)]
pub struct GsUsbFrame {
    pub echo_id: u32,
    pub can_id: u32,
    pub can_dlc: u8,
    pub channel: u8,
    pub flags: u8,
    pub reserved: u8,
    pub data: [u8; 8], // 固定 8 字节（CAN 2.0）
    pub timestamp_us: u32, // 硬件时间戳（可选）
}

impl GsUsbFrame {
    /// Pack frame into BytesMut
    pub fn pack_to(&self, buf: &mut BytesMut, hw_timestamp: bool) {
        let size = if hw_timestamp {
            GS_USB_FRAME_SIZE_HW_TIMESTAMP
        } else {
            GS_USB_FRAME_SIZE
        };
        buf.reserve(size);

        buf.put_u32_le(self.echo_id);
        buf.put_u32_le(self.can_id);
        buf.put_u8(self.can_dlc);
        buf.put_u8(self.channel);
        buf.put_u8(self.flags);
        buf.put_u8(self.reserved);
        buf.put_slice(&self.data);

        if hw_timestamp {
            buf.put_u32_le(self.timestamp_us);
        }
    }

    /// Unpack from Bytes
    pub fn unpack_from_bytes(
        &mut self,
        mut data: Bytes,
        hw_timestamp: bool,
    ) -> Result<(), DriverError> {
        if data.len() < GS_USB_FRAME_SIZE {
            return Err(DriverError::InvalidFrame("Frame too short".to_string()));
        }

        self.echo_id = data.get_u32_le();
        self.can_id = data.get_u32_le();
        self.can_dlc = data.get_u8();
        self.channel = data.get_u8();
        self.flags = data.get_u8();
        self.reserved = data.get_u8();

        let data_slice = &data[..8];
        self.data.copy_from_slice(data_slice);
        data.advance(8);

        if hw_timestamp && data.len() >= 4 {
            self.timestamp_us = data.get_u32_le();
        } else {
            self.timestamp_us = 0;
        }

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
}
```

### 3.5 device.rs - USB 设备操作（复用参考实现）

**核心方法**直接复用参考实现：

- `scan()` - 扫描设备
- `start()` / `stop()` - 启动/停止
- `set_bitrate()` - 设置波特率（使用预定义表）
- `send()` / `read()` - 发送/接收帧
- `control_out()` / `control_in()` - 控制传输

**改进点**：
- 使用 `bytes::Bytes` 作为帧缓冲区
- 添加 `tracing` 日志

### 3.6 mod.rs - 实现 embedded_can trait

```rust
use embedded_can::blocking::Can;
use embedded_can::{Frame, StandardId, ExtendedId, Id};
use crate::can::DriverError;

pub struct GsUsbDriver {
    device: GsUsbDevice,
    device_flags: u32,
    hw_timestamp: bool,
    started: bool,
}

impl Can for GsUsbDriver {
    type Frame = embedded_can::Frame; // 需要查看具体类型
    type Error = DriverError;

    fn transmit(&mut self, frame: &Self::Frame) -> Result<(), Self::Error> {
        if !self.started {
            return Err(DriverError::NotStarted);
        }

        // 转换 embedded_can::Frame -> GsUsbFrame
        let gs_frame = frame_to_gs_usb(frame)?;

        // 发送（内部会等待 echo 确认）
        self.device.send(&gs_frame)?;

        Ok(())
    }

    fn receive(&mut self) -> Result<Self::Frame, Self::Error> {
        if !self.started {
            return Err(DriverError::NotStarted);
        }

        // 接收 GS-USB 帧
        let gs_frame = self.device.read(Duration::from_millis(1000))?;

        // 只处理 RX 帧（忽略 TX echo）
        if !gs_frame.is_rx_frame() {
            // 递归调用或返回错误
            return self.receive(); // 或者返回特定错误让上层处理
        }

        // 转换 GsUsbFrame -> embedded_can::Frame
        gs_usb_to_frame(gs_frame)
    }
}

// 转换函数（需要根据 embedded_can::Frame 的实际 API 实现）
fn frame_to_gs_usb(frame: &embedded_can::Frame) -> Result<GsUsbFrame, DriverError> {
    // 实现转换逻辑
}

fn gs_usb_to_frame(gs_frame: GsUsbFrame) -> Result<embedded_can::Frame, DriverError> {
    // 实现转换逻辑
}
```

---

## 4. 关键改进点总结

| 改进项 | 参考实现 | 我们的方案 |
|--------|---------|-----------|
| **CAN FD 支持** | ✅ 支持 | ❌ 不支持（Piper 不需要） |
| **波特率计算** | ✅ 预定义表（80/40MHz） | ✅ 复用预定义表 |
| **帧格式** | ✅ 完整实现 | ✅ 简化（只 8 字节数据） |
| **embedded_can** | ❌ 不实现 | ✅ 实现 trait |
| **bytes crate** | ❌ 使用 Vec<u8> | ✅ 使用 Bytes/BytesMut |
| **错误处理** | ✅ 完整 | ✅ 复用并扩展 |
| **设备能力缓存** | ✅ 是 | ✅ 复用 |
| **tracing 日志** | ❌ 无 | ✅ 添加 |

---

## 5. 实现优先级

### Phase 1: 核心协议（1-2 天）
1. ✅ 复制并简化 `constants.rs`（移除 CAN FD）
2. ✅ 复制 `structures.rs`（DeviceBitTiming, DeviceMode, DeviceCapability）
3. ✅ 实现 `frame.rs`（简化为 CAN 2.0，使用 bytes）

### Phase 2: USB 设备层（2-3 天）
1. ✅ 复制并修改 `device.rs`（移除 CAN FD 逻辑）
2. ✅ 测试设备扫描、打开、配置
3. ✅ 测试波特率设置

### Phase 3: embedded_can 适配（1-2 天）
1. ⚠️ 查阅 `embedded_can::Frame` API
2. ✅ 实现 `frame_to_gs_usb()` / `gs_usb_to_frame()`
3. ✅ 实现 `Can` trait

### Phase 4: 测试与优化（1 天）
1. ✅ 单元测试（帧编码/解码）
2. ✅ 集成测试（实际硬件）
3. ✅ 性能测试（1kHz 收发）

---

## 6. 待确认事项

1. **embedded_can::Frame 的 API**：
   - `Frame` 是 trait 还是具体类型？
   - 如何构造标准/扩展 ID 的帧？
   - 如何获取/设置数据？

2. **TX Echo 处理**：
   - `transmit()` 是否需要等待 echo 确认？
   - 还是异步发送，由上层处理确认？

3. **硬件时间戳**：
   - Piper 是否需要时间戳？
   - 如果不需要，可以简化帧格式

4. **错误帧处理**：
   - 是否需要处理 CAN 错误帧？
   - 如何报告给上层？

---

## 7. 参考资源

- 参考实现：`tmp/gs_usb_rs/src/`
- embedded-can 文档：需要查阅 `embedded-can 0.4.1` 的 API
- Linux gs_usb 驱动：`drivers/net/can/usb/gs_usb.c`

---

## 总结

参考实现为我们提供了**完整的、经过验证的协议实现**。我们的改进方案：

1. ✅ **简化**：移除 CAN FD 支持，只保留 CAN 2.0
2. ✅ **复用**：直接复用波特率表、结构体定义、设备操作逻辑
3. ✅ **增强**：添加 `embedded_can` trait 实现、`bytes` 支持、`tracing` 日志
4. ✅ **优化**：针对高频场景（1kHz+）的性能优化

预计实现时间：**5-7 个工作日**（基于参考实现）。

