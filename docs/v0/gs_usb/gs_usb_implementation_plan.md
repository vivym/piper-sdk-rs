# GS-USB CAN 适配层实现方案报告

## 1. 需求与目标

### 1.1 核心目标
在 macOS 平台上实现基于 GS-USB 协议的 CAN 总线通信适配层，满足以下要求：

- ✅ 支持 macOS 平台的 USB 设备通信（使用 `rusb`）
- ✅ 实现 `embedded_can::blocking::Can` trait，提供标准化的 CAN 接口
- ✅ 支持高频数据收发（>1kHz），低延迟、高可靠性
- ✅ 提供完整的错误处理和状态监控
- ✅ 支持 CAN 总线配置（波特率、模式等）

### 1.2 技术约束
- **平台**: macOS (开发环境)
- **USB 库**: `rusb 0.9.4` (基于 libusb)
- **CAN 抽象**: `embedded-can 0.4.1`
- **协议解析**: `bilge 0.3.0` (用于帧结构定义)
- **日志**: `tracing 0.1.44`

---

## 2. GS-USB 协议背景

### 2.1 协议概述
GS-USB 是一个**用户空间 USB-to-CAN 转换器协议**，被广泛采用（如 GS-USB、CANable、candleLight 等硬件）。

### 2.2 关键特性
- **免驱动**: 通过用户态 USB 库直接操作，无需内核驱动
- **跨平台**: Windows/macOS/Linux 均可使用 libusb 实现
- **协议透明**: USB 层负责帧传输，上层直接操作 CAN 帧
- **灵活性**: 支持标准 CAN 和 CAN-FD（取决于硬件）

### 2.3 USB 端点配置

典型的 GS-USB 设备配置：

```
Configuration 1
  Interface 0 (CAN Interface)
    ├─ Endpoint 0x81 (Bulk IN)   - 接收 CAN 帧
    ├─ Endpoint 0x01 (Bulk OUT)  - 发送 CAN 帧
    └─ Endpoint 0x82 (Interrupt IN) - 状态/错误通知（可选）
```

### 2.4 USB 通信方式

| 传输类型 | 端点 | 用途 |
|---------|------|------|
| **Control Transfer** | Endpoint 0 | 设备配置：设置波特率、模式、启动/停止 |
| **Bulk Transfer IN** | 0x81 | 接收来自 CAN 总线的帧 |
| **Bulk Transfer OUT** | 0x01 | 发送 CAN 帧到总线 |

---

## 3. 架构设计

### 3.1 模块分层

```
┌─────────────────────────────────────────┐
│  应用层 API (embedded-can::Can)         │
│  - transmit(), receive()                │
│  - configure(), etc.                    │
└─────────────────────────────────────────┘
                   │
┌─────────────────────────────────────────┐
│  GS-USB 适配层 (GsUsbDriver)            │
│  - 帧编码/解码                           │
│  - 状态管理                              │
│  - 错误处理                              │
└─────────────────────────────────────────┘
                   │
┌─────────────────────────────────────────┐
│  GS-USB 协议层 (protocol.rs)            │
│  - Control Request 定义                 │
│  - 帧格式定义 (Host/Device)             │
│  - 波特率映射表                          │
└─────────────────────────────────────────┘
                   │
┌─────────────────────────────────────────┐
│  USB 设备层 (device.rs)                 │
│  - 设备枚举与打开                        │
│  - Interface Claim                       │
│  - Bulk/Control Transfer                │
└─────────────────────────────────────────┘
                   │
┌─────────────────────────────────────────┐
│  USB 库 (rusb)                          │
│  - libusb 绑定                           │
└─────────────────────────────────────────┘
```

### 3.2 目录结构

```
src/can/gs_usb/
├── mod.rs          # 主模块，实现 embedded-can::blocking::Can trait
├── protocol.rs     # GS-USB 协议定义（Control Request、帧格式）
├── device.rs       # USB 设备操作（枚举、打开、传输）
└── error.rs        # GS-USB 专用错误类型（DriverError）
```

---

## 4. 核心模块详细设计

### 4.1 协议层 (`protocol.rs`)

#### 4.1.1 Control Request 定义

GS-USB 使用 USB Control Transfer 进行设备配置。关键请求类型：

```rust
// Control Request Type
pub const GS_USB_REQ_OUT: u8 = 0x40;  // Host-to-Device
pub const GS_USB_REQ_IN: u8 = 0xC0;   // Device-to-Host

// Request Codes
pub const GS_USB_BREQ_SET_BITTIMING: u8 = 0x01;
pub const GS_USB_BREQ_SET_BITTIMING_FD: u8 = 0x08;
pub const GS_USB_BREQ_SET_MODE: u8 = 0x03;
pub const GS_USB_BREQ_SET_BASE: u8 = 0x09;
pub const GS_USB_BREQ_BT_CONST: u8 = 0x04;
pub const GS_USB_BREQ_DEVICE_CONFIG: u8 = 0x05;
pub const GS_USB_BREQ_TIMESTAMP: u8 = 0x06;
pub const GS_USB_BREQ_IDENTIFY: u8 = 0x07;
```

#### 4.1.2 CAN 帧格式（GS-USB Host-to-Device）

**标准帧格式** (8 bytes):
```
┌─────────────────────────────────────────┐
│ Byte 0: Echo ID (8 bits)                │
│ Byte 1: Flags (8 bits)                  │
│         bit 0: Extended Frame (EFF)     │
│         bit 1: Remote Transmission (RTR)│
│         bit 2: CAN-FD                   │
│         bit 3: Bit Rate Switch (BRS)    │
│         bit 4-7: Reserved               │
│ Byte 2-5: CAN ID (32 bits, big-endian) │
│ Byte 6: Data Length Code (DLC, 4 bits) │
│ Byte 7-14: Data (0-8 bytes, padded)    │
└─────────────────────────────────────────┘
```

#### 4.1.3 CAN 帧格式（GS-USB Device-to-Host）

与 Host-to-Device 相同，但可能包含额外的时间戳字段（如果设备支持）。

#### 4.1.4 模式设置

```rust
pub const GS_CAN_MODE_NORMAL: u32 = 0;
pub const GS_CAN_MODE_LISTEN_ONLY: u32 = (1 << 0);
pub const GS_CAN_MODE_LOOP_BACK: u32 = (1 << 1);
pub const GS_CAN_MODE_TRIPLE_SAMPLE: u32 = (1 << 2);
pub const GS_CAN_MODE_ONE_SHOT: u32 = (1 << 3);
pub const GS_CAN_MODE_HW_TIMESTAMP: u32 = (1 << 4);
pub const GS_CAN_MODE_IDENTIFY: u32 = (1 << 5);
pub const GS_CAN_MODE_USER_ID: u32 = (1 << 6);
pub const GS_CAN_MODE_PAD_PKTS_TO_MAX_PKT_SIZE: u32 = (1 << 7);
pub const GS_CAN_MODE_FD: u32 = (1 << 8);
pub const GS_CAN_MODE_BERR_REPORTING: u32 = (1 << 10);
```

#### 4.1.5 波特率配置

GS-USB 使用 **CAN Bit Timing** 结构配置波特率：

```rust
#[repr(C, packed)]
pub struct GsUsbBittiming {
    pub prop_seg: u32,   // Propagation segment
    pub phase_seg1: u32, // Phase segment 1
    pub phase_seg2: u32, // Phase segment 2
    pub sjw: u32,        // Synchronization Jump Width
    pub brp: u32,        // Bit Rate Prescaler
}

// 常用波特率的预定义配置
pub fn calculate_bittiming(baudrate: u32, clock_hz: u32) -> GsUsbBittiming {
    // 实现波特率到 bit timing 参数的转换
    // 公式：baudrate = clock_hz / (brp * (1 + prop_seg + phase_seg1 + phase_seg2))
}
```

**注意**: 不同硬件可能使用不同的时钟频率，需要查询设备能力或使用默认值（通常 80MHz 或 48MHz）。

---

### 4.2 设备层 (`device.rs`)

#### 4.2.1 设备枚举

```rust
use rusb::{Context, Device, DeviceHandle, DeviceDescriptor};

pub struct GsUsbDevice {
    handle: DeviceHandle<Context>,
    endpoint_in: u8,
    endpoint_out: u8,
    max_packet_size: u16,
}

// GS-USB 常见 VID/PID
pub const GS_USB_VIDS: &[u16] = &[
    0x1d50, // OpenMoko
    0x1d51, // Holtzman
];

// 通过 PID 判断设备类型（示例）
pub fn find_gs_usb_device(vid: Option<u16>, pid: Option<u16>) -> Result<Device<Context>, DriverError> {
    let context = Context::new()?;
    for device in context.devices()?.iter() {
        let desc = device.device_descriptor()?;
        // 匹配 VID/PID
        // ...
    }
    Err(DriverError::NotFound(...))
}
```

#### 4.2.2 设备打开与配置

```rust
impl GsUsbDevice {
    pub fn open(device: Device<Context>) -> Result<Self, DriverError> {
        let mut handle = device.open()?;

        // 1. 查找并 claim interface
        let config_desc = device.config_descriptor(0)?;
        let interface = config_desc.interfaces().next()
            .and_then(|iface| iface.descriptors().next())
            .ok_or(DriverError::NotFound("interface"))?;

        handle.claim_interface(interface.number())?;

        // 2. 查找 Bulk IN/OUT 端点
        let (endpoint_in, endpoint_out, max_packet_size) =
            find_bulk_endpoints(&interface)?;

        Ok(Self {
            handle,
            endpoint_in,
            endpoint_out,
            max_packet_size,
        })
    }

    pub fn send_control_request(
        &self,
        request_type: u8,
        request: u8,
        value: u16,
        index: u16,
        data: &[u8],
        timeout: Duration,
    ) -> Result<usize, DriverError> {
        self.handle.write_control(
            request_type,
            request,
            value,
            index,
            data,
            timeout,
        ).map_err(DriverError::from)
    }

    pub fn bulk_write(&self, data: &[u8], timeout: Duration) -> Result<usize, DriverError> {
        self.handle.write_bulk(self.endpoint_out, data, timeout)
            .map_err(DriverError::from)
    }

    pub fn bulk_read(&self, buf: &mut [u8], timeout: Duration) -> Result<usize, DriverError> {
        self.handle.read_bulk(self.endpoint_in, buf, timeout)
            .map_err(DriverError::from)
    }
}
```

#### 4.2.3 macOS 特殊注意事项

1. **权限问题**: macOS 上通常不需要额外配置权限，libusb 会自动处理。但如果设备被系统驱动占用，需要先卸载系统驱动。

2. **Interface Claim**: 必须正确 claim interface，否则无法访问 Bulk 端点。

3. **Timeout 设置**: 建议使用合理的超时时间（如 100-1000ms），避免无限阻塞。

---

### 4.3 适配层 (`mod.rs`)

#### 4.3.1 核心结构体

```rust
use embedded_can::{blocking::Can, Frame, StandardId, ExtendedId, Id};
use crate::can::DriverError;

pub struct GsUsbDriver {
    device: GsUsbDevice,
    mode: CanMode,
    bitrate: u32,
    is_started: bool,
    // 可选的接收缓冲区（用于多线程场景）
    // rx_buffer: Arc<Mutex<VecDeque<Frame>>>,
}

pub enum CanMode {
    Normal,
    ListenOnly,
    LoopBack,
}
```

#### 4.3.2 实现 `embedded_can::blocking::Can` trait

```rust
impl Can for GsUsbDriver {
    type Frame = embedded_can::Frame;
    type Error = DriverError;

    /// 发送 CAN 帧
    fn transmit(&mut self, frame: &Self::Frame) -> Result<(), Self::Error> {
        if !self.is_started {
            return Err(DriverError::NotStarted);
        }

        // 1. 将 embedded_can::Frame 转换为 GS-USB 帧格式
        let gs_frame = encode_frame(frame)?;

        // 2. 通过 Bulk OUT 发送
        self.device.bulk_write(&gs_frame, Duration::from_millis(100))?;

        Ok(())
    }

    /// 接收 CAN 帧（阻塞，带超时）
    fn receive(&mut self) -> Result<Self::Frame, Self::Error> {
        if !self.is_started {
            return Err(DriverError::NotStarted);
        }

        // 1. 从 Bulk IN 读取（带超时）
        let mut buf = [0u8; 16]; // GS-USB 帧最大 16 bytes
        let len = self.device.bulk_read(&mut buf, Duration::from_millis(100))?;

        // 2. 解析为 embedded_can::Frame
        let frame = decode_frame(&buf[..len])?;

        Ok(frame)
    }
}
```

#### 4.3.3 配置与启动/停止

```rust
impl GsUsbDriver {
    /// 创建并初始化驱动
    pub fn new(vid: Option<u16>, pid: Option<u16>, bitrate: u32) -> Result<Self, DriverError> {
        // 1. 查找设备
        let device = find_gs_usb_device(vid, pid)?;

        // 2. 打开设备
        let gs_device = GsUsbDevice::open(device)?;

        let mut driver = Self {
            device: gs_device,
            mode: CanMode::Normal,
            bitrate,
            is_started: false,
        };

        // 3. 配置波特率
        driver.configure_bitrate(bitrate)?;

        Ok(driver)
    }

    /// 配置 CAN 波特率
    pub fn configure_bitrate(&mut self, bitrate: u32) -> Result<(), DriverError> {
        // 1. 计算 bit timing
        let bittiming = calculate_bittiming(bitrate, 80000000); // 假设 80MHz 时钟

        // 2. 发送控制请求
        let data = bittiming_to_bytes(&bittiming);
        self.device.send_control_request(
            GS_USB_REQ_OUT,
            GS_USB_BREQ_SET_BITTIMING,
            0,
            0,
            &data,
            Duration::from_millis(1000),
        )?;

        self.bitrate = bitrate;
        Ok(())
    }

    /// 启动 CAN 通道
    pub fn start(&mut self) -> Result<(), DriverError> {
        if self.is_started {
            return Ok(());
        }

        // 1. 设置模式
        let mode_flags = mode_to_flags(self.mode);
        let mode_bytes = mode_flags.to_le_bytes();
        self.device.send_control_request(
            GS_USB_REQ_OUT,
            GS_USB_BREQ_SET_MODE,
            0,
            0,
            &mode_bytes,
            Duration::from_millis(1000),
        )?;

        // 2. 发送启动命令（通常通过 SET_BASE 或特定请求）
        // 根据具体硬件协议实现

        self.is_started = true;
        tracing::info!("GS-USB CAN channel started at {} bps", self.bitrate);
        Ok(())
    }

    /// 停止 CAN 通道
    pub fn stop(&mut self) -> Result<(), DriverError> {
        if !self.is_started {
            return Ok(());
        }

        // 发送停止命令
        // ...

        self.is_started = false;
        tracing::info!("GS-USB CAN channel stopped");
        Ok(())
    }
}
```

#### 4.3.4 帧编码/解码

```rust
use bilge::prelude::*;

/// GS-USB Host-to-Device 帧结构（使用 bilge）
#[bitsize(128)] // 16 bytes
#[derive(FromBits, DebugBits, Clone, Copy)]
pub struct GsUsbFrame {
    echo_id: u8,
    flags: u8,        // Extended, RTR, CAN-FD, BRS, etc.
    can_id: u32,      // CAN ID (标准或扩展)
    dlc: u8,          // Data Length Code
    data: [u8; 8],    // CAN 数据（最多 8 字节）
}

fn encode_frame(frame: &embedded_can::Frame) -> Result<[u8; 16], DriverError> {
    let id = match frame.id() {
        Id::Standard(id) => id.as_raw() as u32,
        Id::Extended(id) => id.as_raw(),
    };

    let flags = (frame.is_extended() as u8) << 0
        | (frame.is_rtr() as u8) << 1;

    let gs_frame = GsUsbFrame::new()
        .with_echo_id(0)
        .with_flags(flags)
        .with_can_id(id.to_be_bytes())
        .with_dlc(frame.dlc())
        .with_data(frame.data());

    // 转换为字节数组
    Ok(gs_frame.to_bytes())
}

fn decode_frame(data: &[u8]) -> Result<embedded_can::Frame, DriverError> {
    if data.len() < 8 {
        return Err(DriverError::Protocol("Frame too short"));
    }

    let flags = data[1];
    let is_extended = (flags & 0x01) != 0;
    let is_rtr = (flags & 0x02) != 0;

    let id_bytes = &data[2..6];
    let id_value = u32::from_be_bytes([id_bytes[0], id_bytes[1], id_bytes[2], id_bytes[3]]);

    let dlc = data[6] & 0x0F;
    let can_data = &data[7..7 + dlc as usize.min(8)];

    let id = if is_extended {
        Id::Extended(ExtendedId::new(id_value)?)
    } else {
        Id::Standard(StandardId::new(id_value as u16)?)
    };

    // 构造 embedded_can::Frame
    // 注意：embedded_can::Frame 可能没有直接的构造函数，需要查看具体 API
    // 这里使用伪代码
    let frame = Frame::new(id, can_data)?;
    Ok(frame)
}
```

---

### 4.4 错误处理 (`error.rs`)

```rust
use thiserror::Error;

#[derive(Error, Debug)]
pub enum DriverError {
    #[error("Device not found: {0}")]
    NotFound(String),

    #[error("USB error: {0}")]
    Usb(#[from] rusb::Error),

    #[error("Protocol error: {0}")]
    Protocol(&'static str),

    #[error("CAN channel not started")]
    NotStarted,

    #[error("Invalid frame format")]
    InvalidFrame,

    #[error("Timeout")]
    Timeout,

    #[error("Invalid CAN ID")]
    InvalidId,
}
```

---

## 5. 实现步骤与里程碑

### Phase 1: 基础设备通信（1-2 天）
- [ ] 实现 `device.rs`: 设备枚举、打开、claim interface
- [ ] 实现基本的 Control Transfer 发送/接收
- [ ] 测试：能成功打开设备并发送控制请求

### Phase 2: 协议实现（2-3 天）
- [ ] 实现 `protocol.rs`: Control Request 定义、帧格式、波特率计算
- [ ] 实现帧编码/解码（embedded_can ↔ GS-USB）
- [ ] 测试：能发送/接收简单 CAN 帧

### Phase 3: CAN 接口实现（1-2 天）
- [ ] 实现 `mod.rs`: 完整的 `embedded_can::blocking::Can` trait
- [ ] 实现 `start()` / `stop()` / `configure_bitrate()`
- [ ] 测试：通过 `embedded_can` API 发送/接收帧

### Phase 4: 错误处理与稳定性（1 天）
- [ ] 完善错误处理，覆盖所有边界情况
- [ ] 添加超时处理、设备断开检测
- [ ] 添加 `tracing` 日志

### Phase 5: 测试与文档（1 天）
- [ ] 编写单元测试（Mock USB 设备）
- [ ] 编写集成测试（需要实际硬件）
- [ ] 编写使用文档和示例代码

---

## 6. 关键技术难点

### 6.1 波特率计算
**问题**: GS-USB 需要将波特率转换为 CAN Bit Timing 参数，公式复杂且受硬件时钟限制。

**解决方案**:
- 使用预定义的常用波特率映射表（125k, 250k, 500k, 1M）
- 对于非常用波特率，实现基于公式的计算函数
- 在设备初始化时查询设备支持的时钟频率（如果协议支持）

### 6.2 帧对齐与缓冲
**问题**: USB Bulk Transfer 可能一次传输多个帧，或帧被分割。

**解决方案**:
- GS-USB 协议通常每帧固定 16 bytes（或设备指定大小），便于对齐
- 使用固定大小的接收缓冲区
- 在接收循环中按固定大小解析帧

### 6.3 macOS 平台兼容性
**问题**: libusb 在 macOS 上的行为可能与 Linux 不同。

**解决方案**:
- 充分测试 USB 操作的超时处理
- 处理设备断开和重连场景
- 使用 `tracing` 记录详细的 USB 操作日志

### 6.4 embedded_can::Frame 兼容性
**问题**: `embedded_can::Frame` 的具体 API 可能需要适配。

**解决方案**:
- 查看 `embedded-can 0.4.1` 的文档和源码
- 如需要，创建转换层或 wrapper

---

## 7. 测试策略

### 7.1 单元测试
- **Mock USB 设备**: 使用 `rusb` 的测试工具或创建模拟设备
- **协议解析测试**: 测试帧编码/解码的正确性
- **错误处理测试**: 测试各种错误场景

### 7.2 集成测试
- **真实硬件测试**: 使用实际的 GS-USB 设备（如 CANable）
- **Loopback 测试**: 使用设备的 Loopback 模式自测
- **压力测试**: 高频发送/接收，验证稳定性

### 7.3 示例代码
```rust
// examples/test_gs_usb.rs
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut driver = GsUsbDriver::new(None, None, 500_000)?;
    driver.start()?;

    // 发送测试帧
    let frame = Frame::new(...)?;
    driver.transmit(&frame)?;

    // 接收帧
    let received = driver.receive()?;
    println!("Received: {:?}", received);

    Ok(())
}
```

---

## 8. 后续优化方向

### 8.1 性能优化
- **零拷贝**: 使用 `bytes` crate 避免不必要的内存拷贝
- **批量传输**: 如果设备支持，批量发送多个帧
- **异步支持**: 未来可考虑实现 `embedded_can::nb::Can` trait

### 8.2 功能扩展
- **CAN-FD 支持**: 如果硬件支持，实现 CAN-FD 协议
- **时间戳支持**: 利用设备的硬件时间戳功能
- **错误帧处理**: 实现错误帧的检测和报告

### 8.3 跨平台支持
- **Windows 后端**: 验证并优化 Windows 平台支持
- **Linux SocketCAN 后端**: 实现 `socket.rs` 模块

---

## 9. 参考资源

- [GS-USB Linux 驱动源码](https://github.com/hartkopp/can-usb-8dev) - 了解协议细节
- [CANable 固件](https://github.com/normaldotcom/canable-fw) - 参考实现
- [embedded-can 文档](https://docs.rs/embedded-can/)
- [rusb 文档](https://docs.rs/rusb/)
- [libusb 文档](https://libusb.info/) - USB 底层细节

---

## 10. 风险评估

| 风险 | 影响 | 缓解措施 |
|------|------|---------|
| GS-USB 协议变体不兼容 | 高 | 支持多种常见的 VID/PID，提供配置选项 |
| macOS USB 权限问题 | 中 | 提供清晰的错误提示和解决方案文档 |
| 波特率计算错误 | 中 | 使用预定义映射表，充分测试常用波特率 |
| embedded_can API 变化 | 低 | 锁定依赖版本，持续关注上游更新 |

---

## 总结

本实现方案提供了从 USB 设备操作到 CAN 协议适配的完整技术路径。关键成功因素：

1. **清晰的模块划分**: 协议层、设备层、适配层各司其职
2. **完整的错误处理**: 覆盖 USB、协议、CAN 各层错误
3. **标准化接口**: 实现 `embedded_can::blocking::Can`，保证兼容性
4. **充分的测试**: 单元测试 + 集成测试 + 硬件验证

预计总开发时间：**5-8 个工作日**（单开发者）。

