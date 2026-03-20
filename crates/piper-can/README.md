# piper-can

CAN 硬件抽象层，为 Piper 机械臂 SDK 提供统一的 CAN 接口。

## 概述

`piper-can` 是 Piper SDK 的底层 CAN 抽象层，提供：

- **统一的 `CanAdapter` trait**：跨平台的 CAN 接口抽象
- **多后端支持**：SocketCAN（Linux）和 GS-USB（跨平台）
- **自动平台选择**：默认通过 `auto-backend` feature 自动选择合适的后端
- **灵活的 feature 控制**：支持显式选择后端和 mock 测试模式
- **类型安全**：基于 `PiperFrame` 的强类型 CAN 帧接口

## Features

`piper-can` 使用 Cargo features 来控制后端选择和功能：

| Feature | 说明 | 默认 |
|---------|------|------|
| `auto-backend` | 根据平台自动选择后端（Linux: SocketCAN + GS-USB，其他: GS-USB） | ✅ |
| `socketcan` | 强制启用 SocketCAN（仅 Linux） | - |
| `gs_usb` | 强制启用 GS-USB（所有平台） | - |
| `mock` | 禁用所有硬件依赖（用于 CI 测试） | - |
| `serde` | 启用 Serde 序列化支持 | - |

### Feature 组合示例

| Feature 组合 | 行为 | 使用场景 |
|------------|------|---------|
| `default` | Linux: SocketCAN + GS-USB<br>其他: GS-USB | 生产环境（推荐） |
| `gs_usb`, `default-features = false` | 仅 GS-USB | 交叉平台测试 |
| `socketcan`, `gs_usb` | Linux: 两者都启用<br>其他: 编译失败（仅 Linux） | 高级用例 |
| `mock`, `default-features = false` | 仅 Mock Adapter | CI 测试 |

### Feature 优先级

**Mock 优先级最高**：
- `mock` feature 会禁用所有硬件依赖（socketcan 和 gs_usb）
- 即使同时启用 `auto-backend` 和 `mock`，也只会编译 Mock Adapter
- 用于 CI 测试和无硬件的开发环境

**显式 Feature 优先于自动推导**：
- 用户显式指定的 features（如 `socketcan`）优先于 `auto-backend`
- 例如：`features = ["auto-backend", "socketcan"]` 等同于只启用 `socketcan`

**优先级顺序**：
```
mock > 显式 features (socketcan, gs_usb) > auto-backend
```

### ⚠️ 平台限制

**`socketcan` feature 仅在 Linux 上可用**：

```toml
# ❌ 错误：在 Windows/macOS 上启用 socketcan 会失败
piper-can = { features = ["socketcan"] }  # 编译失败（nix crate 不支持）

# ✅ 正确做法：依赖 auto-backend 的自动选择
piper-can = "0.0.3"  # 自动选择平台合适的后端
```

## 平台支持

### Linux

- **默认后端**：SocketCAN（通过 `cfg(target_os = "linux")` 自动启用）
- **可选后端**：GS-USB（可通过 `PiperBuilder` 在运行时切换）
- **特性**：
  - 内核级性能，支持硬件时间戳
  - 通过 `ip link` 工具配置接口和波特率
  - 支持标准帧和扩展帧

**使用示例**：
```rust
use piper_can::SocketCanAdapter;

// 打开 can0 接口
let mut adapter = SocketCanAdapter::new("can0")?;

// 发送帧
let frame = PiperFrame::new_standard(0x123, &[1, 2, 3, 4]);
adapter.send(frame)?;

// 接收帧
let rx_frame = adapter.receive()?;
```

### macOS / Windows

- **唯一后端**：GS-USB（通过 `cfg(not(target_os = "linux"))` 自动启用）
- **特性**：
  - 通过 USB 连接的 CAN 适配器（如 candleLight）
  - 跨平台支持（Linux/macOS/Windows）
  - 应用层配置波特率和模式

**使用示例**：
```rust
use piper_can::GsUsbCanAdapter;

// 自动打开第一个可用的 GS-USB 设备
let mut adapter = GsUsbCanAdapter::new()?;

// 或通过序列号指定设备
let mut adapter = GsUsbCanAdapter::new_with_serial("ABC123456")?;

// 配置波特率并启动
adapter.configure(1_000_000)?;

// 发送帧
let frame = PiperFrame::new_standard(0x123, &[1, 2, 3, 4]);
adapter.send(frame)?;

// 接收帧
let rx_frame = adapter.receive()?;
```

## 架构设计

### PiperFrame：通用 CAN 帧抽象

`PiperFrame` 是贯穿所有 SDK 层次的中央数据结构：

```rust
pub struct PiperFrame {
    pub id: u32,           // CAN ID（标准帧 11 位或扩展帧 29 位）
    pub data: [u8; 8],     // 数据载荷（最多 8 字节）
    pub len: u8,           // 实际数据长度
    pub is_extended: bool, // 是否为扩展帧
    pub timestamp_us: u64, // 硬件时间戳（微秒）
}
```

**特性**：
- **零成本抽象**：`Copy` trait，固定大小数组，无堆分配
- **类型安全**：编译时检查帧格式
- **时间戳支持**：保留硬件时间戳，用于力控和时间敏感应用

### CanAdapter Trait

所有 CAN 适配器实现的统一接口：

```rust
pub trait CanAdapter {
    fn send(&mut self, frame: PiperFrame) -> Result<(), CanError>;
    fn receive(&mut self) -> Result<PiperFrame, CanError>;
    fn set_receive_timeout(&mut self, timeout: Duration);
    // ... 更多方法
}
```

**支持的适配器**：
- `SocketCanAdapter`：Linux SocketCAN 接口
- `GsUsbCanAdapter`：GS-USB 设备（跨平台）
- `GsUsbUdpAdapter`：GS-USB 守护进程 bridge/debug 客户端（非实时）

`GsUsbUdpAdapter` 是 bridge/debug/replay 链路，不参与 realtime driver。
`set_receive_timeout()` 只影响 receive path；`send()` 始终使用固定的 `bridge_timeout`
作为 round-trip budget。若 send timeout、控制平面失同步，或 receive path 看见
`SendAck/Error(seq)`，session 会 fail-closed 并要求显式 `reconnect()`。
UDS 模式仅支持 pathname Unix datagram client；abstract namespace 或 non-UTF8
peer 会被 daemon 侧直接拒绝。

### 分离适配器（Splittable Adapter）

对于需要高并发的场景，支持将适配器分离为独立的 RX 和 TX 部分：

```rust
use piper_can::SplittableAdapter;

// 分离为 RX 和 TX 适配器
let (rx_adapter, tx_adapter) = adapter.split()?;

// 可以在不同线程中并发使用
std::thread::spawn(move || {
    loop {
        let frame = rx_adapter.receive()?;
        // 处理接收
    }
});

// TX 线程
for frame in frames {
    tx_adapter.send(frame)?;
}
```

## Features

### `serde`（可选）

启用 `PiperFrame` 的 Serde 序列化支持，用于帧录制和回放：

```toml
# Cargo.toml
piper-can = { version = "0.0.3", features = ["serde"] }
```

**使用示例**：
```rust
use serde::{Serialize, Deserialize};

#[derive(Serialize, Deserialize)]
struct CanFrameLog {
    timestamp: u64,
    frame: PiperFrame,  // 需要 serde feature
}
```

## 平台自动选择

`piper-can` 使用 **混合模式**（`target_cfg` + features）自动选择平台合适的后端：

| 目标平台 | 默认启用的后端 | 可用的后端 |
|---------|--------------|-----------|
| Linux | SocketCAN + GS-USB | SocketCAN, GS-USB, Mock |
| macOS | GS-USB | GS-USB, Mock |
| Windows | GS-USB | GS-USB, Mock |

### 使用默认配置

推荐使用默认配置（`auto-backend` feature），无需手动指定：

```toml
# ✅ 推荐：使用默认配置
[dependencies]
piper-can = "0.0.3"
```

### 显式选择后端

如果需要禁用某些后端或启用特定功能，可以手动配置 features：

```toml
# 只使用 GS-USB（移除 SocketCAN 依赖）
[dependencies]
piper-can = { version = "0.0.3", features = ["gs_usb"], default-features = false }

# CI 测试（无硬件依赖）
[dependencies]
piper-can = { version = "0.0.3", features = ["mock"], default-features = false }

# 启用 Serde 序列化
[dependencies]
piper-can = { version = "0.0.3", features = ["serde"] }
```

### 运行时后端选择

使用 `PiperBuilder` 在运行时选择后端：

```rust
use piper_driver::{PiperBuilder, DriverType};

// 自动选择（Linux 优先 SocketCAN）
let piper = PiperBuilder::new()
    .build()?;

// 强制使用 GS-USB（所有平台）
let piper = PiperBuilder::new()
    .with_driver_type(DriverType::GsUsb)
    .build()?;

// 强制使用 SocketCAN（仅 Linux）
let piper = PiperBuilder::new()
    .with_driver_type(DriverType::SocketCan)
    .build()?;
```

## Mock Adapter（测试模式）

`mock` feature 提供无硬件依赖的 `MockCanAdapter`，用于 CI 测试和单元测试。

### 启用 Mock 模式

```toml
[dependencies]
piper-can = { version = "0.0.3", features = ["mock"], default-features = false }
```

### 使用示例

```rust
use piper_can::{MockCanAdapter, CanAdapter, PiperFrame};

let mut adapter = MockCanAdapter::new();

// 注入测试帧
let frame = PiperFrame::new_standard(0x123, &[1, 2, 3, 4]);
adapter.inject(frame.clone())?;

// 接收帧
let rx_frame = adapter.receive()?;
assert_eq!(rx_frame.id, 0x123);

// 回环模式：发送的帧自动进入接收队列
adapter.send(frame)?;
let rx_frame2 = adapter.receive()?;
assert_eq!(rx_frame2.id, 0x123);
```

### Mock 特性

- **回环模式**：发送的帧自动进入接收队列
- **零延迟**：所有操作立即完成
- **FIFO 队列**：模拟 CAN 总线的帧顺序
- **超时模拟**：支持测试超时逻辑

### CI 测试示例

```yaml
# .github/workflows/test.yml
test-mock:
  runs-on: ubuntu-latest
  steps:
    - uses: actions/checkout@v3
    - uses: dtolnay/rust-toolchain@stable
    - name: Test mock backend
      run: cargo test --package piper-can --features mock,default-features=false
```

## 架构设计

### PiperFrame：通用 CAN 帧抽象

`PiperFrame` 是贯穿所有 SDK 层次的中央数据结构：

```rust
pub struct PiperFrame {
    pub id: u32,           // CAN ID（标准帧 11 位或扩展帧 29 位）
    pub data: [u8; 8],     // 数据载荷（最多 8 字节）
    pub len: u8,           // 实际数据长度
    pub is_extended: bool, // 是否为扩展帧
    pub timestamp_us: u64, // 硬件时间戳（微秒）
}
```

**特性**：
- **零成本抽象**：`Copy` trait，固定大小数组，无堆分配
- **类型安全**：编译时检查帧格式
- **时间戳支持**：保留硬件时间戳，用于力控和时间敏感应用

### CanAdapter Trait

所有 CAN 适配器实现的统一接口，包括 `SocketCanAdapter`、`GsUsbCanAdapter` 和 `MockCanAdapter`：

```rust
pub trait CanAdapter {
    fn send(&mut self, frame: PiperFrame) -> Result<(), CanError>;
    fn receive(&mut self) -> Result<PiperFrame, CanError>;
    fn set_receive_timeout(&mut self, timeout: Duration);
    // ... 更多方法
}
```

## 错误处理

`piper-can` 提供结构化的错误类型：

```rust
use piper_can::{CanError, CanDeviceError, CanDeviceErrorKind};

match adapter.receive() {
    Ok(frame) => /* 处理帧 */,
    Err(CanError::Timeout) => /* 超时，可重试 */,
    Err(CanError::Device(err)) => {
        match err.kind {
            CanDeviceErrorKind::NotFound => /* 设备未找到 */,
            CanDeviceErrorKind::AccessDenied => /* 权限不足 */,
            CanDeviceErrorKind::Busy => /* 设备忙碌 */,
            _ => /* 其他错误 */,
        }
    },
    Err(e) => /* 其他错误 */,
}
```

## 权限要求

### Linux

- **SocketCAN**：通常需要 `dialout` 组权限或 `sudo`
- **GS-USB**：需要 udev 规则或 `sudo`

**安装 udev 规则**（推荐）：
```bash
sudo cp scripts/99-piper-gs-usb.rules /etc/udev/rules.d/
sudo udevadm control --reload-rules
sudo udevadm trigger
```

### macOS / Windows

- GS-USB：可能需要管理员权限（首次连接时安装驱动）

## 性能特性

### SocketCAN（Linux）

- **零拷贝**：内核级 CAN 处理
- **硬件时间戳**：支持硬件时间戳（微秒级精度）
- **高效轮询**：使用 `poll` + `recvmsg` 实现非阻塞接收

### GS-USB（跨平台）

- **批量读取**：一次 USB 传输可包含多个 CAN 帧
- **接收队列**：缓存 USB 包中的多帧，避免丢包
- **实时模式**：可配置写超时，实现快速失败（力控场景）

## 相关文档

- [架构概述](../../docs/v0/README.md)（CLAUDE.md）
- [GS-USB 实现对比](../../docs/v0/gs_usb/gs_usb_implementation_comparison.md)
- [硬件时间戳实现](../../docs/v0/gs_usb/hardware_timestamp_implementation_plan.md)
- [Position Control 用户指南](../../docs/v0/position_control_user_guide.md)

## License

MIT OR Apache-2.0
