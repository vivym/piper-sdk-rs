# SocketCAN 适配器实现方案

## 1. 概述

本文档详细说明如何在 piper-sdk-rs 项目中实现 SocketCAN 适配器，作为 Linux 平台下 CAN 通讯的后端实现。

### 1.1 目标

- 实现 `CanAdapter` trait，提供与 GS-USB 适配器一致的接口
- 封装 `socketcan` crate，隐藏底层实现细节
- 支持硬件时间戳（SocketCAN 内核特性）
- 支持标准帧和扩展帧
- 提供与 GS-USB 适配器相同的错误处理语义

### 1.2 架构定位

```
src/can/
├── mod.rs          # CanAdapter trait 定义、PiperFrame、CanError
├── socketcan/      # [Linux] SocketCAN 实现
│   └── mod.rs      # SocketCanAdapter 实现
└── gs_usb/         # [非 Linux] GS-USB 实现
    └── mod.rs      # GsUsbCanAdapter 实现
```

## 2. 现有代码分析

### 2.1 核心 Trait 定义

`CanAdapter` trait 定义在 `src/can/mod.rs` 中：

```rust
pub trait CanAdapter {
    /// 发送一帧（Fire-and-Forget）
    fn send(&mut self, frame: PiperFrame) -> Result<(), CanError>;

    /// 接收一帧（阻塞直到收到有效数据帧或超时）
    fn receive(&mut self) -> Result<PiperFrame, CanError>;
}
```

### 2.2 数据结构

**PiperFrame** - SDK 统一的 CAN 帧格式：

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PiperFrame {
    pub id: u32,              // CAN ID（标准帧或扩展帧）
    pub data: [u8; 8],        // 固定 8 字节数据
    pub len: u8,              // 有效数据长度 (0-8)
    pub is_extended: bool,    // 是否为扩展帧（29-bit ID）
    pub timestamp_us: u32,    // 硬件时间戳（微秒）
}
```

**CanError** - 统一错误类型：

```rust
#[derive(Error, Debug)]
pub enum CanError {
    #[error("IO Error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Device Error: {0}")]
    Device(String),

    #[error("Read timeout")]
    Timeout,

    #[error("Buffer overflow")]
    BufferOverflow,

    #[error("Bus off")]
    BusOff,

    #[error("Device not started")]
    NotStarted,
}
```

### 2.3 GS-USB 实现参考

GS-USB 适配器的实现提供了很好的参考：

1. **状态管理**：使用 `started: bool` 跟踪设备状态
2. **模式管理**：使用 `mode: u32` 存储设备模式（用于判断是否需要过滤 Echo）
3. **接收队列**：使用 `rx_queue: VecDeque<PiperFrame>` 缓存批量接收的帧
4. **资源管理**：实现 `Drop` trait 自动清理资源
5. **错误处理**：统一的错误类型转换

## 3. socketcan-rs API 分析

### 3.1 核心类型

**CanSocket** - 主要的 SocketCAN 接口：

```rust
// 打开 CAN 接口
let mut sock = CanSocket::open("can0")?;

// 阻塞接收
let frame = sock.receive()?;

// 发送
sock.transmit(&frame)?;
```

**CanFrame** - CAN 2.0 帧类型：

```rust
// 从 raw ID 创建（自动判断标准/扩展帧）
let frame = CanFrame::new(StandardId::new(0x123)?, &[1, 2, 3, 4])?;

// 访问帧数据
let id = frame.raw_id();           // 原始 CAN ID（去除标志位）
let data = frame.data();           // 数据切片
let dlc = frame.dlc();             // 数据长度
let is_extended = frame.is_extended(); // 是否为扩展帧
```

### 3.2 重要特性

1. **阻塞/非阻塞模式**：
   - 默认阻塞模式，可通过 `set_nonblocking(true)` 切换
   - 非阻塞模式配合 `read_frame_timeout()` 实现超时

2. **超时控制**：
   ```rust
   // 设置读超时
   sock.set_read_timeout(Duration::from_millis(100))?;

   // 带超时的读取
   let frame = sock.read_frame_timeout(Duration::from_millis(100))?;
   ```

3. **错误帧处理**：
   - SocketCAN 会自动接收错误帧（CAN Error Frames）
   - 可通过 `is_error_frame()` 检测
   - 需要过滤错误帧，避免干扰正常数据流

4. **时间戳支持**：
   - SocketCAN 内核提供软件时间戳（通过 `SO_TIMESTAMP` socket option）
   - 硬件时间戳需要硬件支持（如 CAN-USB 适配器）
   - 时间戳通过 `CanFrame` 的扩展方法或直接读取 socket 消息获取

### 3.3 关键 API 方法

```rust
impl CanSocket {
    // 打开接口
    pub fn open(iface: &str) -> Result<Self, Error>;

    // 阻塞接收（可能收到错误帧）
    pub fn receive(&self) -> IoResult<CanFrame>;

    // 发送（Fire-and-Forget）
    pub fn transmit<F>(&self, frame: &F) -> IoResult<()>
    where F: Into<CanFrame> + AsPtr;

    // 设置读超时
    fn set_read_timeout<D>(&self, duration: D) -> IoResult<()>
    where D: Into<Option<Duration>>;

    // 带超时的读取
    fn read_frame_timeout(&self, timeout: Duration) -> IoResult<CanFrame>;
}
```

## 4. 实现设计

### 4.1 模块结构

```rust
// src/can/socketcan/mod.rs

use socketcan::{CanSocket, CanFrame, Frame};
use crate::can::{CanAdapter, CanError, PiperFrame};
use std::time::Duration;
use tracing::{error, trace, warn};

/// SocketCAN 适配器
///
/// 实现 `CanAdapter` trait，提供 Linux 平台下的 SocketCAN 支持
pub struct SocketCanAdapter {
    /// SocketCAN socket
    socket: CanSocket,
    /// 接口名称（如 "can0"）
    interface: String,
    /// 是否已启动（SocketCAN 无需显式启动，但需要接口 UP）
    started: bool,
    /// 读超时时间（用于 receive 方法）
    read_timeout: Duration,
}

impl SocketCanAdapter {
    /// 创建新的 SocketCAN 适配器
    ///
    /// # 参数
    /// - `interface`: CAN 接口名称（如 "can0"）
    ///
    /// # 错误
    /// - `CanError::Device`: 接口不存在或无法打开
    pub fn new(interface: impl Into<String>) -> Result<Self, CanError> {
        let interface = interface.into();

        // 打开 SocketCAN 接口
        let socket = CanSocket::open(&interface)
            .map_err(|e| CanError::Device(format!("Failed to open CAN interface '{}': {}", interface, e)))?;

        // 设置读超时（默认 100ms，避免无限阻塞）
        let read_timeout = Duration::from_millis(100);
        socket.set_read_timeout(read_timeout)
            .map_err(|e| CanError::Io(e))?;

        Ok(Self {
            socket,
            interface: interface.clone(),
            started: true,  // SocketCAN 打开即启动，无需额外配置
            read_timeout,
        })
    }

    /// 配置接口波特率（可选，通常由系统工具配置）
    ///
    /// 注意：SocketCAN 的波特率通常由 `ip link set can0 type can bitrate 500000` 配置
    /// 这个方法主要用于验证接口配置，不修改配置
    pub fn configure(&mut self, _bitrate: u32) -> Result<(), CanError> {
        // SocketCAN 的波特率由系统工具（ip link）配置，不在应用层设置
        // 这里只验证接口是否可用
        // 实际配置应该由系统管理员或初始化脚本完成
        trace!("SocketCAN interface '{}' configured (bitrate set externally)", self.interface);
        Ok(())
    }

    /// 设置读超时
    pub fn set_read_timeout(&mut self, timeout: Duration) -> Result<(), CanError> {
        self.socket.set_read_timeout(timeout)
            .map_err(|e| CanError::Io(e))?;
        self.read_timeout = timeout;
        Ok(())
    }
}

impl CanAdapter for SocketCanAdapter {
    /// 发送帧（Fire-and-Forget）
    fn send(&mut self, frame: PiperFrame) -> Result<(), CanError> {
        if !self.started {
            return Err(CanError::NotStarted);
        }

        // 1. 转换 PiperFrame -> CanFrame
        let can_frame = if frame.is_extended {
            // 扩展帧
            CanFrame::new(ExtendedId::new(frame.id)?, &frame.data[..frame.len as usize])
                .ok_or_else(|| CanError::Device("Failed to create extended frame".to_string()))?
        } else {
            // 标准帧
            CanFrame::new(StandardId::new(frame.id as u16)?, &frame.data[..frame.len as usize])
                .ok_or_else(|| CanError::Device("Failed to create standard frame".to_string()))?
        };

        // 2. 发送（Fire-and-Forget）
        self.socket.transmit(&can_frame)
            .map_err(|e| CanError::Io(e))?;

        trace!("Sent CAN frame: ID=0x{:X}, len={}", frame.id, frame.len);
        Ok(())
    }

    /// 接收帧（阻塞直到收到有效数据帧或超时）
    ///
    /// **关键**：需要过滤错误帧，只返回有效数据帧
    fn receive(&mut self) -> Result<PiperFrame, CanError> {
        if !self.started {
            return Err(CanError::NotStarted);
        }

        // 循环读取，直到收到有效数据帧（跳过错误帧）
        loop {
            // 使用带超时的读取，避免无限阻塞
            let can_frame = match self.socket.read_frame_timeout(self.read_timeout) {
                Ok(frame) => frame,
                Err(e) if e.should_retry() => {
                    // 超时
                    return Err(CanError::Timeout);
                },
                Err(e) => {
                    // 其他 IO 错误
                    return Err(CanError::Io(e));
                },
            };

            // 1. 过滤错误帧
            if can_frame.is_error_frame() {
                // 解析错误帧，转换为适当的 CanError
                // TODO: 解析错误帧类型（Bus Off, Buffer Overflow 等）
                warn!("Received CAN error frame, ignoring");
                continue;
            }

            // 2. 转换 CanFrame -> PiperFrame
            let piper_frame = PiperFrame {
                id: can_frame.raw_id(),
                data: {
                    let mut data = [0u8; 8];
                    let frame_data = can_frame.data();
                    let len = frame_data.len().min(8);
                    data[..len].copy_from_slice(&frame_data[..len]);
                    data
                },
                len: can_frame.dlc() as u8,
                is_extended: can_frame.is_extended(),
                timestamp_us: 0,  // TODO: 提取时间戳
            };

            trace!("Received CAN frame: ID=0x{:X}, len={}", piper_frame.id, piper_frame.len);
            return Ok(piper_frame);
        }
    }
}

// Drop 实现：SocketCAN 会自动清理，无需额外操作
// 但可以记录日志
impl Drop for SocketCanAdapter {
    fn drop(&mut self) {
        trace!("[Auto-Drop] SocketCAN interface '{}' closed", self.interface);
    }
}
```

### 4.2 关键设计决策

#### 4.2.1 错误帧处理

**问题**：SocketCAN 会自动接收错误帧，需要过滤以避免干扰正常数据流。

**方案**：在 `receive()` 方法中循环读取，跳过错误帧，只返回有效数据帧。

**实现**：
```rust
if can_frame.is_error_frame() {
    warn!("Received CAN error frame, ignoring");
    continue;  // 跳过错误帧，继续读取下一个帧
}
```

#### 4.2.2 时间戳提取

**问题**：SocketCAN 支持软件时间戳和硬件时间戳，需要提取并映射到 `PiperFrame.timestamp_us`。

**方案**：
1. SocketCAN 内核通过 `SO_TIMESTAMP` socket option 提供时间戳
2. 时间戳在 `recvmsg()` 返回的 `msghdr` 结构中的 `cmsg` 中
3. `socketcan-rs` crate 可能不直接暴露时间戳，需要手动提取

**实现**（初步）：
```rust
// TODO: 通过 recvmsg 提取时间戳
// 当前先设置为 0，后续完善
timestamp_us: 0,
```

**后续改进**：
- 研究 `socketcan-rs` 是否提供时间戳 API
- 如果没有，考虑使用 `libc::recvmsg()` 直接读取
- 或者提交 PR 到 `socketcan-rs` 添加时间戳支持

#### 4.2.3 超时处理

**问题**：`CanAdapter::receive()` 需要阻塞直到收到帧或超时。

**方案**：使用 `read_frame_timeout()` 方法，设置合理的超时时间（默认 100ms）。

**实现**：
```rust
let can_frame = match self.socket.read_frame_timeout(self.read_timeout) {
    Ok(frame) => frame,
    Err(e) if e.should_retry() => {
        return Err(CanError::Timeout);
    },
    // ...
};
```

#### 4.2.4 状态管理

**问题**：SocketCAN 打开即启动，不像 GS-USB 需要显式配置。

**方案**：
- `started` 字段在 `new()` 时即设为 `true`
- `configure()` 方法留空或仅用于验证接口状态
- 保持与 GS-USB 接口一致，便于上层代码统一处理

### 4.3 与 GS-USB 的差异

| 特性 | GS-USB | SocketCAN |
|------|--------|-----------|
| 初始化 | 需要扫描设备、配置波特率、启动设备 | 打开 socket 即就绪 |
| 波特率配置 | 应用层通过 USB Control Transfer 配置 | 系统层通过 `ip link` 配置 |
| 模式管理 | 支持 LOOP_BACK、LISTEN_ONLY 等模式 | 由系统配置决定 |
| Echo 过滤 | 需要过滤 USB Echo 帧 | 不需要（内核层已处理） |
| 批量接收 | USB Bulk 可能打包多个帧，需要队列 | 每次读一个帧 |
| 时间戳 | 硬件时间戳（设备提供） | 软件/硬件时间戳（内核提供） |
| 错误帧 | 通过错误标志位检测 | 通过 `is_error_frame()` 检测 |

## 5. 模块导出与集成

### 5.1 更新 `src/can/mod.rs`

```rust
#[cfg(target_os = "linux")]
pub mod socketcan;

#[cfg(target_os = "linux")]
pub use socketcan::SocketCanAdapter;

#[cfg(not(target_os = "linux"))]
pub mod gs_usb;

#[cfg(not(target_os = "linux"))]
pub use gs_usb::GsUsbCanAdapter;
```

### 5.2 更新 `src/robot/builder.rs`

```rust
#[cfg(target_os = "linux")]
{
    use crate::can::SocketCanAdapter;

    // 打开 SocketCAN 接口
    let interface = self.interface.as_deref().unwrap_or("can0");
    let mut can = SocketCanAdapter::new(interface)
        .map_err(RobotError::Can)?;

    // SocketCAN 的波特率由系统配置，这里只验证接口状态
    // 注意：如果需要在应用层配置，可以使用 netlink API（需要 root 权限）
    if let Some(bitrate) = self.baud_rate {
        can.configure(bitrate).map_err(RobotError::Can)?;
    }

    // 创建 Piper 实例
    Piper::new(can, self.pipeline_config).map_err(RobotError::Can)
}
```

### 5.3 目录结构

```
src/can/
├── mod.rs              # 核心定义（已有）
├── socketcan/          # [新增] SocketCAN 实现
│   └── mod.rs          # SocketCanAdapter 实现
└── gs_usb/             # [已有] GS-USB 实现
    ├── mod.rs
    ├── device.rs
    ├── error.rs
    ├── frame.rs
    └── protocol.rs
```

## 6. 错误处理映射

### 6.1 socketcan-rs Error -> CanError

| socketcan-rs Error | CanError | 说明 |
|-------------------|----------|------|
| `IoError` (超时) | `CanError::Timeout` | 通过 `should_retry()` 检测 |
| `IoError` (其他) | `CanError::Io` | 直接转换 |
| `ConstructionError` | `CanError::Device` | 包装到 Device 错误 |
| 错误帧 (Bus Off) | `CanError::BusOff` | 需要解析错误帧类型 |
| 错误帧 (Overflow) | `CanError::BufferOverflow` | 需要解析错误帧类型 |

### 6.2 错误帧解析

SocketCAN 错误帧包含详细的错误信息，可以通过解析 `CanErrorFrame` 获取：

```rust
use socketcan::CanErrorFrame;

if can_frame.is_error_frame() {
    if let Ok(error_frame) = CanErrorFrame::try_from(can_frame) {
        // 检查错误类型
        if error_frame.has_overflow() {
            return Err(CanError::BufferOverflow);
        }
        if error_frame.is_bus_off() {
            return Err(CanError::BusOff);
        }
        // 其他错误类型...
    }
}
```

## 7. 测试策略

### 7.1 单元测试

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_socketcan_adapter_new() {
        // 需要 vcan0 接口
        let adapter = SocketCanAdapter::new("vcan0");
        assert!(adapter.is_ok());
    }

    #[test]
    fn test_socketcan_adapter_send_receive() {
        let mut adapter = SocketCanAdapter::new("vcan0").unwrap();

        // 发送测试帧
        let tx_frame = PiperFrame::new_standard(0x123, &[1, 2, 3, 4]);
        adapter.send(tx_frame).unwrap();

        // 接收（可能需要另一个线程或工具发送）
        // 注意：vcan0 是虚拟接口，需要回环模式或外部发送
    }
}
```

### 7.2 集成测试

需要真实的 CAN 总线或虚拟 CAN 接口（`vcan0`）：

```bash
# 设置虚拟 CAN 接口
sudo modprobe vcan
sudo ip link add dev vcan0 type vcan
sudo ip link set up vcan0
```

### 7.3 与 GS-USB 的一致性测试

确保 SocketCAN 和 GS-USB 的行为一致：

- 发送/接收相同 ID 和数据，验证结果一致
- 超时行为一致
- 错误处理一致

## 8. 待实现功能

### 8.1 时间戳支持（优先级：高）

- [ ] 研究 `socketcan-rs` 的时间戳 API
- [ ] 实现时间戳提取逻辑
- [ ] 测试硬件时间戳支持

### 8.2 错误帧详细解析（优先级：中）

- [ ] 实现 `CanErrorFrame` 解析
- [ ] 映射错误类型到 `CanError`
- [ ] 添加错误帧日志

### 8.3 接口状态检查（优先级：低）

- [ ] 实现接口 UP/DOWN 状态检查
- [ ] 实现波特率查询（如果系统支持）
- [ ] 添加接口配置验证

### 8.4 性能优化（优先级：低）

- [ ] 非阻塞模式 + 轮询（如果需要）
- [ ] 批量接收优化（SocketCAN 可能不支持，需要验证）

## 9. 开发步骤

### Phase 1: 基础实现（2-3 小时）

1. 创建 `src/can/socketcan/mod.rs`
2. 实现 `SocketCanAdapter::new()`
3. 实现 `CanAdapter::send()`
4. 实现 `CanAdapter::receive()`（基础版本，先不处理错误帧）
5. 更新 `src/can/mod.rs` 导出
6. 更新 `src/robot/builder.rs` 集成

### Phase 2: 错误处理（1-2 小时）

1. 添加错误帧过滤
2. 实现错误帧解析（基础）
3. 完善错误类型映射

### Phase 3: 时间戳支持（2-3 小时）

1. 研究时间戳 API
2. 实现时间戳提取
3. 测试时间戳准确性

### Phase 4: 测试与文档（2-3 小时）

1. 编写单元测试
2. 编写集成测试
3. 更新文档

## 10. 参考资料

### 10.1 socketcan-rs 文档

- [socketcan-rs GitHub](https://github.com/socketcan-rs/socketcan-rs)
- [socketcan-rs Docs](https://docs.rs/socketcan/)
- [Examples](https://github.com/socketcan-rs/socketcan-rs/tree/main/examples)

### 10.2 Linux SocketCAN 文档

- [SocketCAN 官方文档](https://www.kernel.org/doc/Documentation/networking/can.txt)
- [CAN 总线协议](https://en.wikipedia.org/wiki/CAN_bus)

### 10.3 项目内参考

- `src/can/gs_usb/mod.rs` - GS-USB 适配器实现参考
- `src/can/mod.rs` - CanAdapter trait 定义
- `docs/v0/TDD.md` - 项目架构设计

## 11. 注意事项

1. **权限要求**：SocketCAN 接口可能需要特定权限，建议用户添加到 `dialout` 组或使用 `sudo`

2. **接口配置**：SocketCAN 接口的波特率、模式等由系统工具配置，不在应用层设置

3. **错误帧处理**：必须过滤错误帧，否则会影响正常数据流

4. **时间戳实现**：需要深入研究 `socketcan-rs` 的时间戳支持，可能需要使用底层 API

5. **与 GS-USB 一致性**：保持接口语义一致，便于上层代码统一处理

6. **测试环境**：需要 Linux 环境或虚拟 CAN 接口（`vcan0`）进行测试

---

**文档版本**：v1.0
**创建日期**：2024-12
**作者**：基于现有代码分析和 socketcan-rs 研究

