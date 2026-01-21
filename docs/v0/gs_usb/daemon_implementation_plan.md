# macOS GS-USB 守护进程实现方案

## 1. 问题背景

### 1.1 问题描述

在 macOS 系统下，GS-USB 适配器存在一个已知问题：

- **正常流程**：连接设备 → 读取/发送数据 → 断开连接 → 重新连接 → **可以正常连接，但无法正常发送/接收数据**
- **唯一解决方案**：物理重新插拔 USB 设备才能恢复正常

### 1.2 根本原因

macOS 的 USB 子系统在设备断开重连后，可能存在以下问题：

1. **USB 端点状态残留**：端点可能处于 Halt/Stall 状态，或 Data Toggle 不同步
2. **设备固件状态**：设备固件可能未完全复位，导致后续操作失败
3. **macOS USB 驱动限制**：macOS 的 USB 驱动可能对某些 USB 控制传输有特殊要求

虽然代码中已经实现了 `clear_usb_endpoints()` 和 `prepare_interface()` 等恢复机制，但在某些情况下仍然无法完全恢复设备状态。

### 1.3 解决方案

参考 Linux 的 SocketCAN 架构，实现一个**用户态守护进程**：

- **守护进程**：始终保持与 GS-USB 设备的连接，永不主动断开
- **网络接口**：通过 Unix Domain Socket (UDS) 或 UDP 端口向其他进程提供 CAN 总线访问
- **多客户端支持**：多个应用进程可以同时连接到守护进程
- **设备恢复**：实现状态机，自动处理 USB 热拔插和设备故障恢复
- **实时性保证**：针对机械臂力控（1kHz）场景优化，采用多线程阻塞架构，确保 < 200us 延迟

### 1.4 实时性要求

**关键约束**：本方案针对**机械臂力控（Force Control）**场景设计，需要满足：

- **控制频率**：1kHz (1ms 周期) 或更高
- **延迟要求**：往返延迟 < 200us（USB <-> Daemon <-> Client）
- **抖动要求**：延迟抖动 < 100us（P99）
- **实时性**：必须使用阻塞 IO，严禁轮询 + sleep

**架构原则**：
- ❌ **不使用 `tokio`**：异步运行时会增加调度器抖动，不适合实时场景
- ❌ **不使用 `sleep(1ms)`**：操作系统调度粒度不可控，会导致相位延迟
- ✅ **多线程阻塞**：每个 IO 操作使用独立线程，阻塞在系统调用上
- ✅ **macOS QoS**：必须设置线程优先级，避免被调度到 E-core（能效核）

这样，即使应用进程崩溃或重启，守护进程仍然保持设备连接，避免了断开重连的问题。

---

## 2. 架构设计

### 2.1 整体架构

```
┌─────────────────────────────────────────────────────────────┐
│                    macOS 系统                                │
├─────────────────────────────────────────────────────────────┤
│                                                               │
│  ┌──────────────┐         ┌──────────────┐                  │
│  │  应用进程 1  │         │  应用进程 2  │                  │
│  │ (robot_mon)  │         │  (其他工具)  │                  │
│  └──────┬───────┘         └──────┬───────┘                  │
│         │                        │                           │
│         │  UDS/UDP 协议          │                           │
│         │  (/tmp/gs_usb.sock)    │                           │
│         │  或 (127.0.0.1:XXXX)   │                           │
│         │                        │                           │
│         └────────┬───────────────┘                           │
│                  │                                           │
│         ┌────────▼──────────┐                               │
│         │  GS-USB 守护进程   │                               │
│         │  (gs_usb_daemon)   │                               │
│         │                    │                               │
│         │  ┌──────────────┐  │                               │
│         │  │ 线程 1: USB  │  │  (阻塞在 rusb.read_bulk)      │
│         │  │   -> IPC     │  │  收到数据 → 立即 send_to(UDS) │
│         │  └──────────────┘  │                               │
│         │                    │                               │
│         │  ┌──────────────┐  │                               │
│         │  │ 线程 2: IPC   │  │  (阻塞在 socket.recv_from)   │
│         │  │   -> USB     │  │  收到指令 → 立即 usb.write   │
│         │  └──────────────┘  │                               │
│         │                    │                               │
│         │  ┌──────────────┐  │                               │
│         │  │ 线程 3: 设备  │  │  (低优先级，可 sleep)        │
│         │  │   管理       │  │  监控状态，处理热拔插          │
│         │  └──────────────┘  │                               │
│         └────────┬───────────┘                               │
│                  │                                           │
│                  │  USB 协议                                 │
│                  │                                           │
│         ┌────────▼───────────┐                              │
│         │  GS-USB 适配器     │                              │
│         │  (硬件设备)        │                              │
│         └────────────────────┘                              │
│                                                               │
└─────────────────────────────────────────────────────────────┘
```

**关键设计**：
- **多线程阻塞架构**：每个 IO 操作使用独立线程，阻塞在系统调用上
- **零延迟唤醒**：数据到达时，内核立即唤醒线程（微秒级）
- **macOS QoS**：所有 IO 线程设置为高优先级，运行在 P-core（大核）上

### 2.2 核心组件

#### 2.2.1 守护进程 (`gs_usb_daemon`)

**职责**：
- 设备管理：扫描、连接、配置 GS-USB 设备，实现状态机处理热拔插
- 数据转发：在 UDS/UDP 客户端和 USB 设备之间转发 CAN 帧（支持过滤）
- 状态监控：监控设备健康状态，自动恢复（永不退出）
- 多客户端管理：管理多个客户端的连接，支持心跳和超时清理
- 单例保护：使用文件锁确保只有一个守护进程实例运行

**生命周期**：
- 启动时获取文件锁，防止重复启动
- 连接设备，保持连接直到进程退出
- 实现状态机：Connected → Disconnected → Reconnecting → Connected
- 无论 USB 发生什么错误，守护进程主循环都不应 panic 或退出
- 进程退出时（正常或异常）自动清理资源

#### 2.2.2 客户端库 (`gs_usb_udp_adapter`)

**职责**：
- 提供与 `CanAdapter` trait 兼容的接口
- 将 CAN 操作转换为 UDS/UDP 协议消息
- 处理网络错误和重连逻辑
- 实现心跳机制，防止纯监听模式被超时
- 支持 CAN ID 过滤，减少网络流量

**设计**：
- 实现 `CanAdapter` trait，与 `GsUsbCanAdapter` 接口一致
- 上层代码无需修改，只需切换适配器类型
- 后台线程定期发送心跳包，保持连接活跃

---

## 3. 协议设计

### 3.1 传输层选择

#### 3.1.1 Unix Domain Socket (UDS) vs UDP

**推荐方案**：优先使用 **Unix Domain Socket (`SOCK_DGRAM`)**，同时支持 UDP 作为备选。

**UDS 优势**：
- **性能**：不走网络协议栈，直接内核内存复制，延迟更低（微秒级差异）
- **权限**：可以通过文件权限 (`chmod`) 控制谁能访问 CAN 总线（比 UDP 端口更安全）
- **生命周期**：如果守护进程挂了，Socket 文件还在，客户端容易检测到"连接拒绝"而不是 UDP 的"发入黑洞"
- **本机通信**：对于 macOS 本机进程通信，UDS 是最佳选择

**UDP 优势**：
- **跨机器调试**：支持远程调试（如 iPad 控制 Mac 上的机器人）
- **网络工具**：可以使用标准网络工具（如 `tcpdump`）进行调试

**实现策略**：
- 默认使用 UDS（`/tmp/gs_usb_daemon.sock`）
- 通过配置选项支持 UDP（`127.0.0.1:8888`）
- 协议层统一，传输层可切换

### 3.2 协议格式

#### 3.2.1 消息类型

```rust
#[repr(u8)]
enum MessageType {
    // 客户端 → 守护进程
    Heartbeat = 0x00,      // 心跳包（防止超时）
    Connect = 0x01,        // 客户端连接请求
    Disconnect = 0x02,     // 客户端断开请求
    SendFrame = 0x03,      // 发送 CAN 帧
    GetStatus = 0x04,      // 查询守护进程状态
    SetFilter = 0x05,      // 设置 CAN ID 过滤规则

    // 守护进程 → 客户端
    ConnectAck = 0x81,    // 连接确认
    DisconnectAck = 0x82,  // 断开确认
    ReceiveFrame = 0x83,   // 接收到的 CAN 帧
    StatusResponse = 0x84, // 状态响应
    SendAck = 0x85,        // 发送确认（带 Sequence Number）
    Error = 0xFF,          // 错误消息
}
```

**新增消息说明**：
- **Heartbeat (0x00)**：客户端定期发送，防止纯监听模式被超时
- **SetFilter (0x05)**：客户端设置 CAN ID 过滤规则，减少网络流量
- **SendAck (0x85)**：守护进程确认发送成功/失败（带 Sequence Number）

#### 3.2.2 消息格式

**通用消息头**（所有消息的前 8 字节）：

```
+--------+--------+--------+--------+--------+--------+--------+--------+
| Type   | Flags  | Length | Reserved| Sequence Number (4 bytes)          |
+--------+--------+--------+--------+--------+--------+--------+--------+
| 1 byte | 1 byte | 2 bytes| 1 byte |       4 bytes (little-endian)    |
+--------+--------+--------+--------+--------+--------+--------+--------+
```

- **Type**: 消息类型（`MessageType`）
- **Flags**: 标志位（保留，用于未来扩展）
- **Length**: 消息总长度（包括头部，小端序）
- **Reserved**: 保留字段
- **Sequence Number**: 序列号（用于错误反馈和去重）

**Sequence Number 说明**：
- 客户端发送的每个 `SendFrame` 消息都带有一个递增的序列号
- 守护进程在 `SendAck` 或 `Error` 消息中返回相同的序列号
- 客户端可以根据序列号确认发送是否成功，实现错误反馈机制

**Heartbeat 消息**（客户端 → 守护进程）：

```
+--------+--------+--------+--------+
| Header (8 bytes)                   |
+--------+--------+--------+--------+
```

- 最简单的消息，只有头部，用于保持连接活跃

**Connect 消息**（客户端 → 守护进程）：

```
+--------+--------+--------+--------+
| Header (8 bytes)                   |
+--------+--------+--------+--------+
| Client ID (4 bytes, little-endian) |
+--------+--------+--------+--------+
| Filter Count (1 byte)               |
+--------+--------+--------+--------+
| Filter Rules (variable)             |
+--------+--------+--------+--------+
```

- **Client ID**: 客户端唯一标识
  - `0`：请求自动分配（**推荐**，守护进程自动生成唯一 ID）
  - `非零`：手动指定 ID（向后兼容，但 UDP 跨网络场景下可能冲突）
- **Filter Count**: CAN ID 过滤规则数量（0-255）
- **Filter Rules**: 过滤规则列表（每个规则 8 字节：ID 范围 + 掩码）

**注意**：
- **自动分配模式**（`client_id = 0`）是推荐方式，特别是 UDP 跨网络连接场景
- 守护进程保证自动分配的 ID 唯一性
- 手动指定 ID 可能与其他客户端冲突（特别是 UDP 跨网络场景）

**过滤规则格式**（每个规则 8 字节）：

```
+--------+--------+--------+--------+
| CAN ID Min (4 bytes, little-endian)|
+--------+--------+--------+--------+
| CAN ID Max (4 bytes, little-endian)|
+--------+--------+--------+--------+
```

- 只有 `CAN ID Min <= frame.id <= CAN ID Max` 的帧才会被转发给该客户端
- 如果 `Filter Count = 0`，表示接收所有帧（默认行为）

**ConnectAck 消息**（守护进程 → 客户端）：

```
+--------+--------+--------+--------+
| Header (8 bytes)                   |
+--------+--------+--------+--------+
| Client ID (4 bytes)                |
+--------+--------+--------+--------+
| Status (1 byte)                    |
+--------+--------+--------+--------+
```

- **Client ID**: 实际使用的客户端 ID
  - 如果客户端发送 `client_id = 0`（自动分配），此字段为守护进程分配的 ID
  - 如果客户端发送非零 ID（手动指定），此字段回显客户端发送的 ID
- **Status**: 0 = 成功，非 0 = 错误码

**注意**：
- 客户端必须从 `ConnectAck` 中获取实际使用的 `client_id`（特别是自动分配模式）
- 后续消息（如 `Heartbeat`、`SendFrame`）必须使用此 ID

**SendFrame 消息**（客户端 → 守护进程）：

```
+--------+--------+--------+--------+
| Header (8 bytes)                   |
+--------+--------+--------+--------+
| CAN ID (4 bytes, little-endian)   |
+--------+--------+--------+--------+
| Flags (1 byte)                     |
+--------+--------+--------+--------+
| DLC (1 byte)                       |
+--------+--------+--------+--------+
| Data (0-8 bytes)                   |
+--------+--------+--------+--------+
```

- **CAN ID**: CAN 帧 ID（标准帧或扩展帧）
- **Flags**: 标志位（bit 0 = 扩展帧标志，其他保留）
- **DLC**: 数据长度（0-8）
- **Data**: CAN 帧数据（最多 8 字节）
- **Sequence Number**: 在消息头中，用于错误反馈

**SendAck 消息**（守护进程 → 客户端）：

```
+--------+--------+--------+--------+
| Header (8 bytes)                   |
+--------+--------+--------+--------+
| Status (1 byte)                    |
+--------+--------+--------+--------+
```

- **Sequence Number**: 在消息头中，对应 `SendFrame` 的序列号
- **Status**: 0 = 成功，非 0 = 错误码（见错误码定义）

**SetFilter 消息**（客户端 → 守护进程）：

```
+--------+--------+--------+--------+
| Header (8 bytes)                   |
+--------+--------+--------+--------+
| Filter Count (1 byte)               |
+--------+--------+--------+--------+
| Filter Rules (variable)             |
+--------+--------+--------+--------+
```

- **Filter Count**: 过滤规则数量
- **Filter Rules**: 过滤规则列表（格式同 Connect 消息）

**ReceiveFrame 消息**（守护进程 → 客户端）：

```
+--------+--------+--------+--------+
| Header (8 bytes)                   |
+--------+--------+--------+--------+
| CAN ID (4 bytes, little-endian)    |
+--------+--------+--------+--------+
| Flags (1 byte)                     |
+--------+--------+--------+--------+
| DLC (1 byte)                       |
+--------+--------+--------+--------+
| Timestamp (8 bytes, little-endian) |
+--------+--------+--------+--------+
| Data (0-8 bytes)                   |
+--------+--------+--------+--------+
```

- **Timestamp**: 硬件时间戳（微秒，u64）
- 其他字段同 `SendFrame`
- **注意**：只有通过客户端过滤规则的帧才会被发送

**Error 消息**（守护进程 → 客户端）：

```
+--------+--------+--------+--------+
| Header (4 bytes)                   |
+--------+--------+--------+--------+
| Error Code (1 byte)                |
+--------+--------+--------+--------+
| Error Message (variable length)    |
+--------+--------+--------+--------+
```

- **Error Code**: 错误码（见错误码定义）
- **Error Message**: UTF-8 编码的错误消息（可选）

#### 3.1.3 错误码定义

```rust
#[repr(u8)]
enum ErrorCode {
    Unknown = 0x00,
    DeviceNotFound = 0x01,
    DeviceBusy = 0x02,
    InvalidMessage = 0x03,
    NotConnected = 0x04,
    DeviceError = 0x05,
    Timeout = 0x06,
}
```

### 3.2 通信流程

#### 3.2.1 客户端连接流程

**自动 ID 分配模式（推荐）**：

```
客户端                          守护进程
  |                                |
  |--- Connect (Client ID = 0) --->|
  |                                | 自动分配唯一 ID
  |                                | 验证设备状态
  |                                | 注册客户端
  |<-- ConnectAck (Assigned ID) ---|
  |                                | (保存分配的 ID)
  |                                |
```

**手动 ID 指定模式（向后兼容）**：

```
客户端                          守护进程
  |                                |
  |--- Connect (Client ID = X) --->|
  |                                | 验证 ID 是否冲突
  |                                | 验证设备状态
  |                                | 注册客户端
  |<-- ConnectAck (Status) --------|
  |                                |
```

**说明**：
- 自动分配模式（`client_id = 0`）：推荐方式，守护进程保证 ID 唯一性
- 手动指定模式（`client_id != 0`）：向后兼容，但可能在 UDP 跨网络场景下冲突
- `ConnectAck` 中的 `client_id` 字段包含实际使用的 ID（自动分配或手动指定）
- 客户端必须从 `ConnectAck` 获取并保存此 ID，用于后续通信

#### 3.2.2 发送 CAN 帧流程

```
客户端                          守护进程
  |                                |
  |--- SendFrame (CAN data) ------>|
  |                                | 转发到 USB 设备
  |                                | (Fire-and-Forget)
  |                                |
```

**注意**：守护进程不等待 USB Echo，直接返回（Fire-and-Forget 语义）。

#### 3.2.3 接收 CAN 帧流程

```
客户端                          守护进程
  |                                |
  |  (守护进程从 USB 接收数据)      |
  |                                |
  |<-- ReceiveFrame (CAN data) -----|
  |                                |
```

**注意**：守护进程会根据客户端的过滤规则，只向符合条件的客户端发送接收到的 CAN 帧。

#### 3.2.4 客户端心跳流程

```
客户端                          守护进程
  |                                |
  |--- Heartbeat ------------------>|
  |                                | 更新 last_active
  |                                |
```

**注意**：客户端定期发送心跳包（建议每 5 秒），防止纯监听模式被超时。

#### 3.2.5 客户端断开流程

```
客户端                          守护进程
  |                                |
  |--- Disconnect (Client ID) ----->|
  |                                | 注销客户端
  |<-- DisconnectAck -------------|
  |                                |
```

#### 3.2.6 发送确认流程（可选）

```
客户端                          守护进程
  |                                |
  |--- SendFrame (seq=123) -------->|
  |                                | 转发到 USB 设备
  |                                | 检查发送结果
  |<-- SendAck (seq=123, status) ---|
  |                                |
```

**注意**：如果 USB 发送失败（如缓冲区满），守护进程会通过 `SendAck` 或 `Error` 消息通知客户端。

---

## 4. 实现细节

### 4.1 守护进程实现

#### 4.1.1 模块结构

```
src/bin/gs_usb_daemon/
├── main.rs              # 主入口（单例锁、信号处理）
├── daemon.rs             # 守护进程核心逻辑（多线程阻塞架构）
├── client_manager.rs     # 客户端管理（过滤、心跳）
├── protocol.rs           # UDS/UDP 协议编解码（零拷贝优化）
├── device_manager.rs     # 设备管理（状态机、热拔插恢复）
├── singleton.rs          # 单例文件锁
└── macos_qos.rs          # macOS QoS 设置（线程优先级）
```

#### 4.1.2 核心数据结构

```rust
// src/bin/gs_usb_daemon/daemon.rs

use crate::can::gs_usb::GsUsbCanAdapter;
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::{Duration, Instant};

/// 设备状态机
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceState {
    /// 设备已连接，正常工作
    Connected,
    /// 设备断开（物理拔出或错误）
    Disconnected,
    /// 正在重连中
    Reconnecting,
}

/// 守护进程状态
pub struct Daemon {
    /// GS-USB 适配器（使用 RwLock 优化读取性能）
    adapter: Arc<RwLock<Option<GsUsbCanAdapter>>>,

    /// 设备状态
    device_state: Arc<RwLock<DeviceState>>,

    /// UDS Socket（Unix Domain Socket）
    socket_uds: Option<std::os::unix::net::UnixDatagram>,

    /// UDP Socket（可选，用于跨机器调试）
    socket_udp: Option<std::net::UdpSocket>,

    /// 客户端管理器（使用 RwLock 优化读取性能）
    clients: Arc<RwLock<ClientManager>>,

    /// 守护进程配置
    config: DaemonConfig,
}

/// 守护进程配置
pub struct DaemonConfig {
    /// UDS Socket 路径（默认 /tmp/gs_usb_daemon.sock）
    uds_path: Option<String>,

    /// UDP 监听地址（可选，如 "127.0.0.1:8888"）
    udp_addr: Option<String>,

    /// CAN 波特率（默认 1000000）
    bitrate: u32,

    /// 设备序列号（可选，用于多设备场景）
    serial_number: Option<String>,

    /// 重连间隔（秒，默认 1 秒）
    reconnect_interval: Duration,
}

/// 客户端信息
pub struct Client {
    /// 客户端 ID
    id: u32,

    /// 客户端地址（用于 UDS/UDP 回复）
    addr: ClientAddr,

    /// 最后活动时间
    last_active: Instant,

    /// CAN ID 过滤规则
    filters: Vec<CanIdFilter>,
}

/// 客户端地址（支持 UDS 和 UDP）
pub enum ClientAddr {
    Unix(std::os::unix::net::SocketAddr),
    Udp(std::net::SocketAddr),
}

/// CAN ID 过滤规则
pub struct CanIdFilter {
    /// 最小 CAN ID（包含）
    min_id: u32,
    /// 最大 CAN ID（包含）
    max_id: u32,
}

impl CanIdFilter {
    /// 检查帧是否匹配过滤规则
    pub fn matches(&self, can_id: u32) -> bool {
        self.min_id <= can_id && can_id <= self.max_id
    }
}
```

#### 4.1.3 主循环（带状态机和热拔插恢复）

```rust
impl Daemon {
    /// 启动守护进程
    pub fn run(&mut self) -> Result<(), DaemonError> {
        // 1. 初始化 Socket（UDS 优先，UDP 可选）
        self.init_sockets()?;

        // 2. 启动设备管理线程（状态机 + 热拔插恢复）
        let adapter_clone = Arc::clone(&self.adapter);
        let device_state_clone = Arc::clone(&self.device_state);
        let config_clone = self.config.clone();

        thread::spawn(move || {
            Self::device_manager_loop(
                adapter_clone,
                device_state_clone,
                config_clone,
            );
        });

        // 3. 启动 USB 接收线程（从 USB 设备读取 CAN 帧）
        let adapter_clone = Arc::clone(&self.adapter);
        let clients_clone = Arc::clone(&self.clients);
        let socket_uds_clone = self.socket_uds.as_ref().map(|s| s.try_clone().ok());
        let socket_udp_clone = self.socket_udp.as_ref().map(|s| s.try_clone().ok());

        thread::spawn(move || {
            Self::usb_receive_loop(
                adapter_clone,
                clients_clone,
                socket_uds_clone,
                socket_udp_clone,
            );
        });

        // 4. 启动客户端清理线程（定期清理超时客户端）
        let clients_clone = Arc::clone(&self.clients);
        thread::spawn(move || {
            Self::client_cleanup_loop(clients_clone);
        });

        // 5. 主循环：处理 UDS/UDP 消息
        self.message_loop()
    }

    /// 设备管理循环（状态机 + 热拔插恢复）
    ///
    /// **关键**：无论 USB 发生什么错误，守护进程都不应退出，而是进入重连模式。
    ///
    /// **去抖动机制**：在进入 `Reconnecting` 状态前，增加冷却时间，避免 macOS USB 枚举抖动。
    fn device_manager_loop(
        adapter: Arc<RwLock<Option<GsUsbCanAdapter>>>,
        device_state: Arc<RwLock<DeviceState>>,
        config: DaemonConfig,
    ) {
        // 去抖动：记录最后一次断开时间
        let mut last_disconnect_time: Option<Instant> = None;
        let debounce_interval = Duration::from_millis(500); // 500ms 冷却时间

        loop {
            let current_state = *device_state.read().unwrap();

            match current_state {
                DeviceState::Connected => {
                    // 检查设备是否仍然可用（可选：定期健康检查）
                    // 如果检测到错误，转入 Disconnected
                    // 注意：这里可以添加定期健康检查逻辑
                },
                DeviceState::Disconnected => {
                    // **去抖动**：检查是否在冷却期内
                    let now = Instant::now();
                    if let Some(last_time) = last_disconnect_time {
                        if now.duration_since(last_time) < debounce_interval {
                            // 仍在冷却期内，等待
                            thread::sleep(debounce_interval - now.duration_since(last_time));
                        }
                    }
                    last_disconnect_time = Some(now);

                    // 进入重连状态
                    *device_state.write().unwrap() = DeviceState::Reconnecting;
                },
                DeviceState::Reconnecting => {
                    // 尝试连接设备
                    match Self::try_connect_device(&config) {
                        Ok(new_adapter) => {
                            *adapter.write().unwrap() = Some(new_adapter);
                            *device_state.write().unwrap() = DeviceState::Connected;
                            last_disconnect_time = None; // 重置去抖动计时器
                            eprintln!("Device connected successfully");
                        },
                        Err(e) => {
                            eprintln!("Failed to connect device: {}. Retrying in {:?}...",
                                     e, config.reconnect_interval);
                            thread::sleep(config.reconnect_interval);
                            // 保持 Reconnecting 状态，继续重试
                        },
                    }
                },
            }

            // 短暂休眠，避免 CPU 空转
            thread::sleep(Duration::from_millis(10));
        }
    }

    /// 尝试连接设备
    fn try_connect_device(config: &DaemonConfig) -> Result<GsUsbCanAdapter, DaemonError> {
        // 1. 扫描设备
        let mut adapter = GsUsbCanAdapter::new_with_serial(config.serial_number.as_deref())
            .map_err(|e| DaemonError::DeviceInit(e))?;

        // 2. 配置设备
        adapter.configure(config.bitrate)
            .map_err(|e| DaemonError::DeviceConfig(e))?;

        Ok(adapter)
    }

    /// USB 接收循环（高优先级线程，阻塞 IO）
    ///
    /// **关键**：使用阻塞 IO，数据到达时内核立即唤醒线程（微秒级）
    /// **严禁**：不要使用 sleep 或轮询
    /// **优化**：使用 RwLock 读取锁，减少锁竞争
    fn usb_receive_loop(
        adapter: Arc<RwLock<Option<GsUsbCanAdapter>>>,
        clients: Arc<RwLock<ClientManager>>,
        socket_uds: Option<std::os::unix::net::UnixDatagram>,
        socket_udp: Option<std::net::UdpSocket>,
    ) {
        loop {
            // 1. 检查设备状态（快速检查，不要阻塞）
            let adapter_guard = adapter.read().unwrap();
            let adapter_ref = match adapter_guard.as_ref() {
                Some(a) => a,
                None => {
                    // 设备未连接，短暂等待后重试（设备管理线程会处理重连）
                    drop(adapter_guard);
                    thread::sleep(Duration::from_millis(100));
                    continue;
                },
            };

            // 2. 从 USB 设备读取 CAN 帧（阻塞 IO）
            // **关键**：receive() 内部使用阻塞的 rusb.read_bulk()，没有数据时线程挂起
            let frame = match adapter_ref.receive() {
                Ok(f) => f,
                Err(crate::can::CanError::Timeout) => {
                    // 超时是正常的（receive 内部有超时设置），继续循环
                    // 注意：这里的超时是 USB 层面的超时（如 2ms），不是 sleep
                    continue;
                },
                Err(e) => {
                    // 其他错误：可能是设备断开，通知设备管理线程
                    drop(adapter_guard);
                    // 设备管理线程会检测到并进入重连模式
                    // 短暂等待后重试，避免死循环
                    thread::sleep(Duration::from_millis(100));
                    continue;
                },
            };
            drop(adapter_guard);

            // 3. 向符合条件的客户端发送（使用读取锁，支持并发）
            let clients_guard = clients.read().unwrap();
            for client in clients_guard.iter() {
                // 检查过滤规则
                if !client.matches_filter(frame.id) {
                    continue;
                }

                // 零拷贝编码（使用栈上缓冲区）
                let mut buf = [0u8; 64];
                let msg = protocol::encode_receive_frame_zero_copy(&frame, &mut buf);

                // 根据客户端地址类型发送
                match &client.addr {
                    ClientAddr::Unix(addr) => {
                        if let Some(ref socket) = socket_uds {
                            if let Err(e) = socket.send_to(msg, addr) {
                                eprintln!("Failed to send frame to client {}: {}", client.id, e);
                            }
                        }
                    },
                    ClientAddr::Udp(addr) => {
                        if let Some(ref socket) = socket_udp {
                            if let Err(e) = socket.send_to(msg, addr) {
                                eprintln!("Failed to send frame to client {}: {}", client.id, e);
                            }
                        }
                    },
                }
            }
        }
    }

    /// 客户端清理循环（定期清理超时客户端）
    fn client_cleanup_loop(clients: Arc<RwLock<ClientManager>>) {
        loop {
            thread::sleep(Duration::from_secs(5));
            clients.write().unwrap().cleanup_timeout();
        }
    }

    /// 启动守护进程（多线程阻塞架构）
    ///
    /// **关键设计**：
    /// - **不使用 `tokio`**：异步运行时会增加调度器抖动，不适合实时场景
    /// - **不使用 `sleep`**：操作系统调度粒度不可控，会导致相位延迟
    /// - **多线程阻塞**：每个 IO 操作使用独立线程，阻塞在系统调用上
    /// - **macOS QoS**：所有 IO 线程设置为高优先级，运行在 P-core（大核）上
    pub fn run(&mut self) -> Result<(), DaemonError> {
        // 1. 初始化 Socket（UDS 优先，UDP 可选）
        self.init_sockets()?;

        // 2. 启动设备管理线程（低优先级，可以 sleep）
        let adapter_clone = Arc::clone(&self.adapter);
        let device_state_clone = Arc::clone(&self.device_state);
        let config_clone = self.config.clone();

        thread::Builder::new()
            .name("device_manager".into())
            .spawn(move || {
                Self::device_manager_loop(
                    adapter_clone,
                    device_state_clone,
                    config_clone,
                );
            })?;

        // 3. 启动 USB 接收线程（高优先级，阻塞 IO）
        let adapter_clone = Arc::clone(&self.adapter);
        let clients_clone = Arc::clone(&self.clients);
        let socket_uds_clone = self.socket_uds.as_ref().map(|s| s.try_clone().ok());
        let socket_udp_clone = self.socket_udp.as_ref().map(|s| s.try_clone().ok());

        thread::Builder::new()
            .name("usb_receive".into())
            .spawn(move || {
                // 设置 macOS QoS（高优先级）
                #[cfg(target_os = "macos")]
                macos_qos::set_high_priority();

                Self::usb_receive_loop(
                    adapter_clone,
                    clients_clone,
                    socket_uds_clone,
                    socket_udp_clone,
                );
            })?;

        // 4. 启动 UDS 接收线程（高优先级，阻塞 IO）
        if let Some(socket_uds) = self.socket_uds.take() {
            let adapter_clone = Arc::clone(&self.adapter);
            let device_state_clone = Arc::clone(&self.device_state);
            let clients_clone = Arc::clone(&self.clients);

            thread::Builder::new()
                .name("uds_receive".into())
                .spawn(move || {
                    // 设置 macOS QoS（高优先级）
                    #[cfg(target_os = "macos")]
                    macos_qos::set_high_priority();

                    Self::ipc_receive_loop(
                        socket_uds,
                        adapter_clone,
                        device_state_clone,
                        clients_clone,
                    );
                })?;
        }

        // 5. 启动 UDP 接收线程（高优先级，阻塞 IO，可选）
        if let Some(socket_udp) = self.socket_udp.take() {
            let adapter_clone = Arc::clone(&self.adapter);
            let device_state_clone = Arc::clone(&self.device_state);
            let clients_clone = Arc::clone(&self.clients);

            thread::Builder::new()
                .name("udp_receive".into())
                .spawn(move || {
                    // 设置 macOS QoS（高优先级）
                    #[cfg(target_os = "macos")]
                    macos_qos::set_high_priority();

                    Self::ipc_receive_loop(
                        socket_udp,
                        adapter_clone,
                        device_state_clone,
                        clients_clone,
                    );
                })?;
        }

        // 6. 启动客户端清理线程（低优先级，可以 sleep）
        let clients_clone = Arc::clone(&self.clients);
        thread::Builder::new()
            .name("client_cleanup".into())
            .spawn(move || {
                Self::client_cleanup_loop(clients_clone);
            })?;

        // 7. 主线程挂起（不再消耗 CPU）
        loop {
            thread::park();
        }
    }

    /// IPC 接收循环（阻塞 IO，零延迟）
    ///
    /// **关键**：使用阻塞 IO，数据到达时内核立即唤醒线程（微秒级）
    /// **严禁**：不要使用 sleep 或轮询
    fn ipc_receive_loop(
        socket: impl IpcSocket, // UDS 或 UDP 的统一接口
        adapter: Arc<RwLock<Option<GsUsbCanAdapter>>>,
        device_state: Arc<RwLock<DeviceState>>,
        clients: Arc<RwLock<ClientManager>>,
    ) {
        let mut buf = [0u8; 1024];

        loop {
            // 【关键】：阻塞接收！没有数据时线程挂起，不占 CPU。
            // 数据一来，内核立即唤醒（微秒级）。
            match socket.recv_from(&mut buf) {
                Ok((len, addr)) => {
                    // 立即处理，不要有任何 sleep
                    if let Ok(msg) = protocol::decode_message(&buf[..len]) {
                        Self::handle_ipc_message(
                            msg,
                            addr,
                            &adapter,
                            &device_state,
                            &clients,
                            &socket,
                        );
                    }
                },
                Err(e) => {
                    // 只有出错时才 sleep 一下防止死循环日志
                    eprintln!("IPC Recv Error: {}", e);
                    thread::sleep(Duration::from_millis(100));
                },
            }
        }
    }

    /// 处理消息（统一处理 UDS 和 UDP）
    fn handle_message(&mut self, msg: protocol::Message, addr: ClientAddr) -> Result<(), DaemonError> {
        match msg {
            protocol::Message::Heartbeat { client_id } => {
                self.handle_heartbeat(client_id)?;
            },
            protocol::Message::Connect { client_id, filters } => {
                self.handle_connect(client_id, addr, filters)?;
            },
            protocol::Message::Disconnect { client_id } => {
                self.handle_disconnect(client_id)?;
            },
            protocol::Message::SendFrame { frame, seq } => {
                self.handle_send_frame(frame, seq, addr)?;
            },
            protocol::Message::SetFilter { client_id, filters } => {
                self.handle_set_filter(client_id, filters)?;
            },
            protocol::Message::GetStatus => {
                self.handle_get_status(addr)?;
            },
        }
        Ok(())
    }

    /// 处理发送 CAN 帧请求（带 Sequence Number 和错误反馈）
    fn handle_send_frame(
        &self,
        frame: PiperFrame,
        seq: u32,
        addr: ClientAddr,
    ) -> Result<(), DaemonError> {
        // 检查设备状态
        let device_state = *self.device_state.read().unwrap();
        if device_state != DeviceState::Connected {
            // 设备未连接，发送错误消息
            self.send_error(addr, ErrorCode::DeviceNotFound, "Device not connected")?;
            return Ok(());
        }

        // 尝试发送
        let adapter_guard = self.adapter.read().unwrap();
        if let Some(ref adapter) = *adapter_guard {
            match adapter.send(frame) {
                Ok(()) => {
                    // 发送成功，发送确认（可选，为了性能可以省略）
                    self.send_ack(addr, seq, 0)?;
                },
                Err(e) => {
                    // 发送失败，发送错误消息
                    self.send_error(addr, ErrorCode::DeviceError, &format!("Send failed: {}", e))?;
                },
            }
        } else {
            self.send_error(addr, ErrorCode::DeviceNotFound, "Device not available")?;
        }

        Ok(())
    }
}
```

#### 4.1.4 macOS QoS 设置（关键：线程优先级）

**重要性**：在 Apple Silicon (M1/M2/M3) 上，如果不显式设置 QoS，后台运行的 Daemon 很容易被调度到 **E-core (能效核)**，导致延迟从微秒级飙升到毫秒级。

```rust
// src/bin/gs_usb_daemon/macos_qos.rs

#[cfg(target_os = "macos")]
mod macos_qos {
    use std::os::raw::{c_int, c_void};

    #[allow(non_camel_case_types)]
    type pthread_t = *mut c_void;
    #[allow(non_camel_case_types)]
    type qos_class_t = c_int;

    // macOS QoS 级别定义
    const QOS_CLASS_USER_INTERACTIVE: qos_class_t = 0x21; // 最高实时性优先级
    const QOS_CLASS_USER_INITIATED: qos_class_t = 0x19;
    const QOS_CLASS_DEFAULT: qos_class_t = 0x15;
    const QOS_CLASS_UTILITY: qos_class_t = 0x11;
    const QOS_CLASS_BACKGROUND: qos_class_t = 0x09;

    extern "C" {
        fn pthread_self() -> pthread_t;
        fn pthread_set_qos_class_np(
            thread: pthread_t,
            qos_class: qos_class_t,
            relative_priority: c_int,
        ) -> c_int;
    }

    /// 设置当前线程为高优先级（User Interactive）
    ///
    /// **作用**：
    /// - 告诉 macOS 调度器："这个线程在处理实时硬件通信，必须放在 P-core (大核)"
    /// - 避免被调度到 E-core (能效核)，导致延迟波动
    ///
    /// **调用时机**：在每个 IO 线程（USB Rx, IPC Rx）的开头调用
    pub fn set_high_priority() {
        unsafe {
            let result = pthread_set_qos_class_np(
                pthread_self(),
                QOS_CLASS_USER_INTERACTIVE,
                0, // relative_priority: 0 = 默认相对优先级
            );

            if result != 0 {
                eprintln!("Warning: Failed to set thread QoS (error: {})", result);
            }
        }
    }

    /// 设置当前线程为低优先级（Utility）
    ///
    /// **用途**：设备管理线程等非实时任务
    pub fn set_low_priority() {
        unsafe {
            let _ = pthread_set_qos_class_np(
                pthread_self(),
                QOS_CLASS_UTILITY,
                0,
            );
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod macos_qos {
    /// 非 macOS 平台，QoS 设置为空操作
    pub fn set_high_priority() {}
    pub fn set_low_priority() {}
}

pub use macos_qos::{set_high_priority, set_low_priority};
```

**使用方式**：

```rust
// 在每个 IO 线程的开头调用
thread::Builder::new()
    .name("usb_receive".into())
    .spawn(move || {
        // 设置 macOS QoS（高优先级）
        #[cfg(target_os = "macos")]
        macos_qos::set_high_priority();

        // 继续执行线程逻辑...
    })?;
```

#### 4.1.5 客户端管理（支持过滤和心跳）

```rust
// src/bin/gs_usb_daemon/client_manager.rs

use std::collections::HashMap;
use std::time::{Duration, Instant};

pub struct ClientManager {
    clients: HashMap<u32, Client>,
    /// 客户端超时时间（默认 30 秒）
    timeout: Duration,
}

impl Client {
    /// 检查帧是否匹配客户端的过滤规则
    pub fn matches_filter(&self, can_id: u32) -> bool {
        // 如果没有过滤规则，接收所有帧
        if self.filters.is_empty() {
            return true;
        }

        // 检查是否匹配任一过滤规则
        self.filters.iter().any(|filter| filter.matches(can_id))
    }
}

impl ClientManager {
    pub fn new() -> Self {
        Self {
            clients: HashMap::new(),
            timeout: Duration::from_secs(30),
        }
    }

    /// 注册客户端
    pub fn register(&mut self, id: u32, addr: SocketAddr) -> Result<(), ClientError> {
        if self.clients.contains_key(&id) {
            return Err(ClientError::AlreadyExists);
        }

        self.clients.insert(id, Client {
            id,
            addr,
            last_active: Instant::now(),
        });

        Ok(())
    }

    /// 注销客户端
    pub fn unregister(&mut self, id: u32) {
        self.clients.remove(&id);
    }

    /// 更新客户端活动时间（用于心跳）
    pub fn update_activity(&mut self, id: u32) {
        if let Some(client) = self.clients.get_mut(&id) {
            client.last_active = Instant::now();
        }
    }

    /// 设置客户端过滤规则
    pub fn set_filters(&mut self, id: u32, filters: Vec<CanIdFilter>) {
        if let Some(client) = self.clients.get_mut(&id) {
            client.filters = filters;
        }
    }

    /// 清理超时客户端
    pub fn cleanup_timeout(&mut self) {
        let now = Instant::now();
        self.clients.retain(|_, client| {
            now.duration_since(client.last_active) < self.timeout
        });
    }

    /// 获取所有客户端（用于广播）
    pub fn iter(&self) -> impl Iterator<Item = &Client> {
        self.clients.values()
    }
}
```

#### 4.1.5 单例文件锁

```rust
// src/bin/gs_usb_daemon/singleton.rs

use std::fs::File;
use std::io::{self, Write};
use std::os::unix::io::{AsRawFd, RawFd};
use nix::fcntl::{flock, FlockArg};

/// 单例文件锁
///
/// 使用文件锁确保只有一个守护进程实例运行。
/// 比 `pgrep` 更可靠，因为即使进程崩溃，锁也会自动释放。
pub struct SingletonLock {
    file: File,
    _path: std::path::PathBuf,
}

impl SingletonLock {
    /// 尝试获取单例锁
    ///
    /// # 参数
    /// - `lock_path`: 锁文件路径（如 `/var/run/gs_usb_daemon.lock`）
    ///
    /// # 返回
    /// - `Ok(Self)`: 成功获取锁
    /// - `Err`: 锁已被其他进程持有，或文件操作失败
    pub fn try_lock(lock_path: impl AsRef<std::path::Path>) -> Result<Self, io::Error> {
        let path = lock_path.as_ref();

        // 创建锁文件（如果不存在）
        let file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(path)?;

        // 尝试获取排他锁（非阻塞）
        flock(file.as_raw_fd(), FlockArg::LockExclusiveNonblock)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("Failed to acquire lock: {}", e)))?;

        // 写入当前进程 PID（用于调试）
        let pid = std::process::id();
        writeln!(&file, "{}", pid)?;
        file.sync_all()?;

        Ok(Self {
            file,
            _path: path.to_path_buf(),
        })
    }
}

impl Drop for SingletonLock {
    fn drop(&mut self) {
        // 释放锁（文件关闭时自动释放）
        let _ = flock(self.file.as_raw_fd(), FlockArg::Unlock);
    }
}
```

**使用方式**（在 `main.rs` 中）：

```rust
// src/bin/gs_usb_daemon/main.rs

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 尝试获取单例锁
    let _lock = SingletonLock::try_lock("/var/run/gs_usb_daemon.lock")
        .map_err(|e| {
            eprintln!("Another daemon instance is already running");
            e
        })?;

    // 继续启动守护进程...
    let mut daemon = Daemon::new(config)?;
    daemon.run()?;

    Ok(())
}
```

### 4.2 客户端库实现

#### 4.2.1 模块结构

```
src/can/gs_usb_udp/
├── mod.rs              # GsUsbUdpAdapter 实现（支持 UDS 和 UDP）
├── protocol.rs         # 协议编解码（与守护进程共享，零拷贝优化）
└── client.rs           # 客户端逻辑（心跳线程）
```

#### 4.2.2 UDS/UDP 适配器实现（带心跳机制）

```rust
// src/can/gs_usb_udp/mod.rs

use crate::can::{CanAdapter, CanError, PiperFrame};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

/// GS-USB UDS/UDP 适配器（通过守护进程访问设备）
pub struct GsUsbUdpAdapter {
    /// Socket（UDS 或 UDP）
    socket: SocketWrapper,

    /// 守护进程地址
    daemon_addr: DaemonAddr,

    /// 客户端 ID
    client_id: u32,

    /// 接收缓冲区
    rx_buffer: Arc<Mutex<Vec<PiperFrame>>>,

    /// 序列号（用于错误反馈）
    sequence_number: Arc<Mutex<u32>>,

    /// 心跳线程句柄（保持线程存活）
    _heartbeat_handle: thread::JoinHandle<()>,
}

/// Socket 包装器（支持 UDS 和 UDP）
enum SocketWrapper {
    Unix(std::os::unix::net::UnixDatagram),
    Udp(std::net::UdpSocket),
}

/// 守护进程地址（支持 UDS 和 UDP）
enum DaemonAddr {
    Unix(String),  // UDS 路径
    Udp(String),   // UDP 地址
}

impl GsUsbUdpAdapter {
    /// 创建新的适配器（自动检测 UDS 或 UDP）
    ///
    /// # 参数
    /// - `daemon_addr`: 守护进程地址
    ///   - UDS: `/tmp/gs_usb_daemon.sock` 或 `unix:/tmp/gs_usb_daemon.sock`
    ///   - UDP: `127.0.0.1:8888` 或 `udp:127.0.0.1:8888`
    pub fn new(daemon_addr: &str) -> Result<Self, CanError> {
        // 解析地址类型
        let (addr_type, addr_str) = if daemon_addr.starts_with("unix:") {
            (DaemonAddr::Unix(daemon_addr[5..].to_string()), "unix")
        } else if daemon_addr.starts_with("udp:") {
            (DaemonAddr::Udp(daemon_addr[4..].to_string()), "udp")
        } else if daemon_addr.starts_with('/') {
            // 默认 UDS 路径
            (DaemonAddr::Unix(daemon_addr.to_string()), "unix")
        } else {
            // 默认 UDP 地址
            (DaemonAddr::Udp(daemon_addr.to_string()), "udp")
        };

        // 创建 Socket
        let socket = match &addr_type {
            DaemonAddr::Unix(path) => {
                let socket = std::os::unix::net::UnixDatagram::unbound()
                    .map_err(|e| CanError::Device(format!("Failed to create UDS socket: {}", e)))?;
                socket.set_read_timeout(Some(Duration::from_millis(100)))
                    .map_err(|e| CanError::Device(format!("Failed to set timeout: {}", e)))?;
                SocketWrapper::Unix(socket)
            },
            DaemonAddr::Udp(addr) => {
                let socket = std::net::UdpSocket::bind("0.0.0.0:0")
                    .map_err(|e| CanError::Device(format!("Failed to bind UDP socket: {}", e)))?;
                socket.set_read_timeout(Some(Duration::from_millis(100)))
                    .map_err(|e| CanError::Device(format!("Failed to set timeout: {}", e)))?;
                SocketWrapper::Udp(socket)
            },
        };

        // 生成客户端 ID（使用进程 ID）
        let client_id = std::process::id();

        let rx_buffer = Arc::new(Mutex::new(Vec::new()));
        let sequence_number = Arc::new(Mutex::new(0u32));

        let mut adapter = Self {
            socket,
            daemon_addr: addr_type,
            client_id,
            rx_buffer: rx_buffer.clone(),
            sequence_number: sequence_number.clone(),
            _heartbeat_handle: thread::spawn(|| {}), // 临时值，稍后替换
        };

        // 连接到守护进程
        adapter.connect()?;

        // 启动心跳线程
        let socket_clone = adapter.socket.try_clone()?;
        let daemon_addr_clone = adapter.daemon_addr.clone();
        let client_id = adapter.client_id;
        let heartbeat_handle = thread::spawn(move || {
            Self::heartbeat_loop(socket_clone, daemon_addr_clone, client_id);
        });

        adapter._heartbeat_handle = heartbeat_handle;

        Ok(adapter)
    }

    /// 心跳循环（后台线程）
    ///
    /// **关键**：定期发送心跳包，防止纯监听模式被超时
    fn heartbeat_loop(socket: SocketWrapper, daemon_addr: DaemonAddr, client_id: u32) {
        loop {
            thread::sleep(Duration::from_secs(5)); // 每 5 秒发送一次心跳

            let msg = protocol::encode_heartbeat(client_id);
            match (&socket, &daemon_addr) {
                (SocketWrapper::Unix(ref s), DaemonAddr::Unix(ref path)) => {
                    if let Ok(addr) = std::os::unix::net::SocketAddr::from_pathname(path) {
                        let _ = s.send_to(&msg, &addr);
                    }
                },
                (SocketWrapper::Udp(ref s), DaemonAddr::Udp(ref addr)) => {
                    if let Ok(addr) = addr.parse::<std::net::SocketAddr>() {
                        let _ = s.send_to(&msg, &addr);
                    }
                },
                _ => {},
            }
        }
    }

    /// 连接到守护进程
    fn connect(&mut self) -> Result<(), CanError> {
        let msg = protocol::encode_connect(self.client_id);
        self.socket.send_to(&msg, self.daemon_addr)
            .map_err(|e| CanError::Device(format!("Failed to send connect: {}", e)))?;

        // 等待连接确认
        let mut buf = [0u8; 1024];
        let (len, _) = self.socket.recv_from(&mut buf)
            .map_err(|e| CanError::Device(format!("Failed to receive connect ack: {}", e)))?;

        let ack = protocol::decode_message(&buf[..len])
            .map_err(|e| CanError::Device(format!("Invalid connect ack: {}", e)))?;

        match ack {
            protocol::Message::ConnectAck { status } if status == 0 => {
                Ok(())
            },
            protocol::Message::ConnectAck { status } => {
                Err(CanError::Device(format!("Connect failed with status: {}", status)))
            },
            protocol::Message::Error { code, message } => {
                Err(CanError::Device(format!("Connect error {}: {}", code, message)))
            },
            _ => {
                Err(CanError::Device("Unexpected message type".to_string()))
            },
        }
    }
}

impl CanAdapter for GsUsbUdpAdapter {
    /// 发送 CAN 帧
    fn send(&mut self, frame: PiperFrame) -> Result<(), CanError> {
        let msg = protocol::encode_send_frame(&frame);
        self.socket.send_to(&msg, self.daemon_addr)
            .map_err(|e| CanError::Device(format!("Failed to send frame: {}", e)))?;
        Ok(())
    }

    /// 接收 CAN 帧
    fn receive(&mut self) -> Result<PiperFrame, CanError> {
        // 1. 先检查缓冲区
        if let Some(frame) = self.rx_buffer.pop() {
            return Ok(frame);
        }

        // 2. 从 UDP 接收
        let mut buf = [0u8; 1024];
        loop {
            let (len, _) = match self.socket.recv_from(&mut buf) {
                Ok((len, addr)) => (len, addr),
                Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {
                    return Err(CanError::Timeout);
                },
                Err(e) => {
                    return Err(CanError::Io(e));
                },
            };

            let msg = protocol::decode_message(&buf[..len])
                .map_err(|e| CanError::Device(format!("Invalid message: {}", e)))?;

            match msg {
                protocol::Message::ReceiveFrame(frame) => {
                    return Ok(frame);
                },
                protocol::Message::Error { code, message } => {
                    return Err(CanError::Device(format!("Error {}: {}", code, message)));
                },
                _ => {
                    // 忽略其他消息类型
                    continue;
                },
            }
        }
    }
}

impl Drop for GsUsbUdpAdapter {
    fn drop(&mut self) {
        // 发送断开连接消息
        let msg = protocol::encode_disconnect(self.client_id);
        let _ = self.socket.send_to(&msg, self.daemon_addr);
    }
}
```

### 4.3 协议编解码（零拷贝优化）

**优化目标**：减少堆分配和内存拷贝，提高性能。

**策略**：
- 使用栈上数组（`[u8; 64]`）作为编码缓冲区，避免 `Vec` 分配
- 使用 `Cursor` 进行序列化，减少中间拷贝
- 解码时直接操作输入缓冲区，避免不必要的拷贝

```rust
// src/can/gs_usb_udp/protocol.rs

use crate::can::PiperFrame;
use std::io::{Cursor, Write};

pub enum Message {
    Heartbeat { client_id: u32 },
    Connect { client_id: u32, filters: Vec<CanIdFilter> },
    Disconnect { client_id: u32 },
    SendFrame { frame: PiperFrame, seq: u32 },
    ReceiveFrame(PiperFrame),
    ConnectAck { status: u8 },
    DisconnectAck,
    GetStatus,
    StatusResponse { /* ... */ },
    Error { code: u8, message: String },
    SendAck { seq: u32, status: u8 },
}

/// 零拷贝编码：使用栈上缓冲区
///
/// **优势**：
/// - 避免堆分配（`Vec::new()`）
/// - 减少内存拷贝
/// - 适合高频场景（1kHz+）
pub fn encode_receive_frame_zero_copy(
    frame: &PiperFrame,
    buf: &mut [u8; 64],
) -> &[u8] {
    let mut cursor = Cursor::new(buf.as_mut());

    // 消息头（8 字节）
    cursor.write_all(&[0x83, 0x00]).unwrap(); // Type, Flags
    let length = 8 + 4 + 1 + 1 + 8 + 8; // Header + ID + Flags + DLC + Timestamp + Data
    cursor.write_all(&(length as u16).to_le_bytes()).unwrap(); // Length
    cursor.write_all(&[0x00, 0x00, 0x00, 0x00]).unwrap(); // Reserved + Seq (4 bytes)

    // CAN 帧数据
    cursor.write_all(&frame.id.to_le_bytes()).unwrap();
    cursor.write_all(&[if frame.is_extended { 0x01 } else { 0x00 }]).unwrap();
    cursor.write_all(&[frame.len]).unwrap();
    cursor.write_all(&frame.timestamp_us.to_le_bytes()).unwrap();
    cursor.write_all(&frame.data).unwrap();

    &buf[..length]
}

/// 编码发送帧（带序列号）
pub fn encode_send_frame_with_seq(
    frame: &PiperFrame,
    seq: u32,
    buf: &mut [u8; 64],
) -> &[u8] {
    let mut cursor = Cursor::new(buf.as_mut());

    // 消息头（8 字节）
    cursor.write_all(&[0x03, 0x00]).unwrap(); // Type, Flags
    let length = 8 + 4 + 1 + 1 + frame.len as usize; // Header + ID + Flags + DLC + Data
    cursor.write_all(&(length as u16).to_le_bytes()).unwrap(); // Length
    cursor.write_all(&[0x00]).unwrap(); // Reserved
    cursor.write_all(&seq.to_le_bytes()).unwrap(); // Sequence Number

    // CAN 帧数据
    cursor.write_all(&frame.id.to_le_bytes()).unwrap();
    cursor.write_all(&[if frame.is_extended { 0x01 } else { 0x00 }]).unwrap();
    cursor.write_all(&[frame.len]).unwrap();
    cursor.write_all(&frame.data[..frame.len as usize]).unwrap();

    &buf[..length]
}

/// 编码心跳包（最小消息）
pub fn encode_heartbeat(client_id: u32) -> [u8; 12] {
    let mut buf = [0u8; 12];
    buf[0] = 0x00; // Type: Heartbeat
    buf[1] = 0x00; // Flags
    buf[2..4].copy_from_slice(&12u16.to_le_bytes()); // Length
    buf[4..8].copy_from_slice(&client_id.to_le_bytes()); // Client ID
    buf
}

pub fn decode_message(data: &[u8]) -> Result<Message, ProtocolError> {
    if data.len() < 4 {
        return Err(ProtocolError::TooShort);
    }

    let msg_type = data[0];
    let _flags = data[1];
    let length = u16::from_le_bytes([data[2], data[3]]) as usize;

    if data.len() < length {
        return Err(ProtocolError::Incomplete);
    }

    match msg_type {
        0x01 => {
            // Connect
            let client_id = u32::from_le_bytes([
                data[4], data[5], data[6], data[7]
            ]);
            Ok(Message::Connect { client_id })
        },
        0x03 => {
            // SendFrame
            let id = u32::from_le_bytes([
                data[4], data[5], data[6], data[7]
            ]);
            let is_extended = (data[8] & 0x01) != 0;
            let len = data[9].min(8);
            let mut frame_data = [0u8; 8];
            frame_data[..len as usize].copy_from_slice(&data[10..10 + len as usize]);

            Ok(Message::SendFrame(PiperFrame {
                id,
                data: frame_data,
                len,
                is_extended,
                timestamp_us: 0,
            }))
        },
        0x83 => {
            // ReceiveFrame
            let id = u32::from_le_bytes([
                data[4], data[5], data[6], data[7]
            ]);
            let is_extended = (data[8] & 0x01) != 0;
            let len = data[9].min(8);
            let timestamp = u64::from_le_bytes([
                data[10], data[11], data[12], data[13],
                data[14], data[15], data[16], data[17],
            ]);
            let mut frame_data = [0u8; 8];
            frame_data[..len as usize].copy_from_slice(&data[18..18 + len as usize]);

            Ok(Message::ReceiveFrame(PiperFrame {
                id,
                data: frame_data,
                len,
                is_extended,
                timestamp_us: timestamp,
            }))
        },
        _ => Err(ProtocolError::UnknownType),
    }
}
```

---

## 5. 集成方案

### 5.1 修改 `CanAdapter` 选择逻辑

在 `src/robot/builder.rs` 中，添加对 UDP 适配器的支持：

```rust
// src/robot/builder.rs

#[cfg(target_os = "macos")]
{
    // 检查是否使用守护进程模式
    if let Some(daemon_addr) = self.daemon_addr {
        // 使用 UDP 适配器
        use crate::can::gs_usb_udp::GsUsbUdpAdapter;
        let mut can = GsUsbUdpAdapter::new(&daemon_addr)
            .map_err(RobotError::Can)?;
        // 注意：UDP 适配器不需要 configure，守护进程已配置
        Piper::new(can, self.pipeline_config).map_err(RobotError::Can)
    } else {
        // 直接使用 GS-USB 适配器（传统模式）
        use crate::can::GsUsbCanAdapter;
        let mut can = GsUsbCanAdapter::new_with_serial(self.serial_number.as_deref())
            .map_err(RobotError::Can)?;
        can.configure(self.bitrate.unwrap_or(1_000_000))
            .map_err(RobotError::Can)?;
        Piper::new(can, self.pipeline_config).map_err(RobotError::Can)
    }
}
```

### 5.2 守护进程启动脚本

创建 `scripts/gs_usb_daemon.sh`：

```bash
#!/bin/bash

# GS-USB 守护进程启动脚本

DAEMON_BIN="target/release/gs_usb_daemon"
LOCK_FILE="/var/run/gs_usb_daemon.lock"
UDS_PATH="/tmp/gs_usb_daemon.sock"
UDP_ADDR="127.0.0.1:8888"  # 可选，用于跨机器调试
DAEMON_BITRATE="1000000"

# 启动守护进程（守护进程内部会使用文件锁，这里不需要 pgrep）
echo "Starting GS-USB daemon..."
echo "  UDS: $UDS_PATH"
echo "  UDP: $UDP_ADDR (optional)"
echo "  Bitrate: $DAEMON_BITRATE"

# 启动守护进程（如果已运行，守护进程会检测到文件锁并退出）
$DAEMON_BIN \
    --uds "$UDS_PATH" \
    --udp "$UDP_ADDR" \
    --bitrate "$DAEMON_BITRATE" \
    --lock-file "$LOCK_FILE" \
    > /var/log/gs_usb_daemon.log 2>&1 &

DAEMON_PID=$!

# 等待守护进程启动
sleep 1

# 检查守护进程是否成功启动（检查进程是否还在运行）
if kill -0 $DAEMON_PID 2>/dev/null; then
    echo "GS-USB daemon started successfully (PID: $DAEMON_PID)"
    echo "Logs: /var/log/gs_usb_daemon.log"
else
    echo "Failed to start GS-USB daemon"
    echo "Check logs: /var/log/gs_usb_daemon.log"
    exit 1
fi
```

**注意**：
- 守护进程内部使用文件锁（`SingletonLock`），比 `pgrep` 更可靠
- 如果守护进程已运行，新实例会检测到锁并自动退出
- 即使进程崩溃，文件锁也会自动释放（文件描述符关闭）

### 5.3 使用示例

```rust
// examples/robot_monitor_daemon.rs

use piper_sdk::robot::PiperBuilder;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 使用守护进程模式
    let mut robot = PiperBuilder::new()
        .with_daemon("127.0.0.1:8888")  // 连接到守护进程
        .build()?;

    // 正常使用，无需修改
    loop {
        let state = robot.state();
        println!("Joint positions: {:?}", state.joint_positions());
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}
```

---

## 6. 部署与运维

### 6.1 守护进程配置

**配置文件**（`~/.config/gs_usb_daemon/config.toml`）：

```toml
[daemon]
# UDP 监听地址
listen_addr = "127.0.0.1:8888"

# CAN 波特率
bitrate = 1000000

# 设备序列号（可选，用于多设备场景）
# serial_number = "ABC123"

[client]
# 客户端超时时间（秒）
timeout = 30

[logging]
# 日志级别：trace, debug, info, warn, error
level = "info"

# 日志文件路径（可选）
# file = "/var/log/gs_usb_daemon.log"
```

### 6.2 系统服务（可选）

创建 macOS LaunchDaemon（`/Library/LaunchDaemons/com.piper.gs_usb_daemon.plist`）：

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.piper.gs_usb_daemon</string>
    <key>ProgramArguments</key>
    <array>
        <string>/usr/local/bin/gs_usb_daemon</string>
        <string>--config</string>
        <string>/etc/gs_usb_daemon/config.toml</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardOutPath</key>
    <string>/var/log/gs_usb_daemon.log</string>
    <key>StandardErrorPath</key>
    <string>/var/log/gs_usb_daemon.error.log</string>
</dict>
</plist>
```

**安装服务**：

```bash
sudo launchctl load /Library/LaunchDaemons/com.piper.gs_usb_daemon.plist
```

### 6.3 监控与诊断

**健康检查工具**（`tools/daemon_health_check.rs`）：

```rust
// 检查守护进程是否运行
// 发送 GetStatus 消息，验证响应
```

**日志分析**：

- 守护进程日志：记录所有客户端连接/断开、设备错误等
- 客户端日志：记录网络错误、重连尝试等

---

## 7. 性能考虑（实时性关键）

### 7.1 延迟分析（针对力控场景）

**目标**：满足机械臂力控（1kHz）的实时性要求

**延迟指标**：
- **往返延迟**：< 200us（USB <-> Daemon <-> Client）
- **延迟抖动**：< 100us（P99）
- **控制频率**：1kHz (1ms 周期) 或更高

**传输层延迟**：
- **UDS (Unix Domain Socket)**：< 50us（内核内存复制，最优）
- **UDP (本地回环)**：< 200us（网络协议栈，可接受）
- **USB Bulk 传输**：< 100us（硬件延迟）

**架构选择**：
- ✅ **多线程阻塞 IO**：零延迟唤醒（内核立即唤醒线程）
- ❌ **tokio 异步运行时**：会增加调度器抖动（微秒到毫秒级）
- ❌ **轮询 + sleep**：会导致相位延迟和不可控的调度延迟

### 7.2 吞吐量分析

**CAN 帧大小**：
- 协议开销：~20 字节（UDS/UDP 头部）
- 数据：8 字节（CAN 2.0）
- 总大小：~28 字节

**带宽需求**：
- 1kHz 发送 + 1kHz 接收 = 2k 包/秒
- 总带宽：~56 KB/s（远低于网络接口能力）

### 7.3 优化建议

1. **使用 UDS**：Unix Domain Socket 延迟最低（< 50us）
2. **多线程阻塞**：每个 IO 操作使用独立线程，阻塞在系统调用上
3. **macOS QoS**：设置线程优先级，避免被调度到 E-core
4. **零拷贝编码**：使用栈上缓冲区，避免堆分配
5. **帧过滤**：客户端可以订阅特定 CAN ID，减少网络流量
6. **严禁 sleep**：在热路径上不要使用 sleep，会导致不可控延迟

---

## 8. 测试方案

### 8.1 单元测试

- 协议编解码测试
- 客户端管理器测试
- 错误处理测试

### 8.2 集成测试

- 守护进程 + 单个客户端
- 守护进程 + 多个客户端
- 客户端断开重连测试
- 守护进程重启测试（客户端自动重连）

### 8.3 压力测试

- 1kHz 发送/接收持续运行
- 多客户端并发访问
- 网络丢包场景测试

---

## 9. 实施计划

### Phase 1: 核心协议（2-3 天）

- [ ] 实现 UDS/UDP 协议编解码（零拷贝优化）
- [ ] 实现基础消息类型（Heartbeat, Connect, SendFrame, ReceiveFrame, SetFilter）
- [ ] 实现 Sequence Number 和错误反馈机制
- [ ] 单元测试

### Phase 2: 守护进程核心（3-4 天）

- [ ] 实现单例文件锁
- [ ] 实现 macOS QoS 设置（线程优先级）
- [ ] 实现多线程阻塞架构（USB Rx、IPC Rx 独立线程）
- [ ] 实现守护进程主循环（UDS + UDP 支持）
- [ ] 实现客户端管理（过滤、心跳、超时清理）
- [ ] 实现 USB 设备状态机（Connected → Disconnected → Reconnecting，带去抖动）
- [ ] 实现热拔插恢复逻辑
- [ ] **关键**：确保所有热路径不使用 sleep
- [ ] 集成测试

### Phase 3: 客户端库（2-3 天）

- [ ] 实现 `GsUsbUdpAdapter`（支持 UDS 和 UDP）
- [ ] 实现 `CanAdapter` trait
- [ ] 实现心跳线程（防止超时）
- [ ] 实现错误处理和重连逻辑
- [ ] 实现 Sequence Number 跟踪
- [ ] 单元测试

### Phase 4: 集成与测试（2-3 天）

- [ ] 修改 `PiperBuilder` 支持守护进程模式
- [ ] 端到端测试（单客户端、多客户端）
- [ ] 性能测试（1kHz 发送/接收）
- [ ] 压力测试（多客户端并发）
- [ ] 热拔插测试
- [ ] 文档编写

### Phase 5: 部署工具（1-2 天）

- [ ] 启动脚本（使用文件锁）
- [ ] 配置文件支持（UDS/UDP 选择）
- [ ] 健康检查工具
- [ ] 系统服务配置（macOS LaunchDaemon，可选）

**总计**：10-15 个工作日

**关键里程碑**：
- ✅ Phase 1 完成：协议层就绪
- ✅ Phase 2 完成：守护进程可以运行，支持基本功能
- ✅ Phase 3 完成：客户端可以连接并使用
- ✅ Phase 4 完成：可以替代直接 GS-USB 连接
- ✅ Phase 5 完成：生产就绪

---

## 9.5 实施最佳实践

### 9.5.1 日志分级策略

**问题**：在高频收发（1kHz）时，如果打印 `Info` 级别的日志（如 "Sent ID: xxx"），I/O 会成为瓶颈。

**解决方案**：使用 `tracing` crate 进行结构化日志，合理设置日志级别。

```rust
// 使用 tracing 进行日志分级

use tracing::{trace, debug, info, warn, error};

// 在守护进程中
impl Daemon {
    fn handle_send_frame(&self, frame: PiperFrame, seq: u32) -> Result<(), DaemonError> {
        // ❌ 错误：高频日志使用 Info 级别
        // info!("Sent CAN frame: ID=0x{:X}, len={}", frame.id, frame.len);

        // ✅ 正确：高频日志使用 Trace 或 Debug 级别
        trace!("Sent CAN frame: ID=0x{:X}, len={}, seq={}", frame.id, frame.len, seq);

        // ✅ 正确：重要事件使用 Info 级别
        info!("Device connected successfully");

        // ✅ 正确：错误使用 Warn 或 Error 级别
        warn!("USB receive error: {}", e);
        error!("Device connection failed: {}", e);
    }
}
```

**日志级别建议**：

| 日志类型 | 级别 | 说明 | 示例 |
|---------|------|------|------|
| **高频数据帧** | `Trace` | 仅在深度调试时开启 | "Sent/Received CAN frame" |
| **一般调试信息** | `Debug` | 开发调试时开启 | "Client connected", "Filter updated" |
| **重要事件** | `Info` | 生产环境默认开启 | "Daemon started", "Device connected" |
| **警告** | `Warn` | 需要关注但不致命 | "Client timeout", "USB error (retrying)" |
| **错误** | `Error` | 需要立即处理 | "Device connection failed", "Protocol error" |

**配置示例**（使用 `tracing-subscriber`）：

```rust
// 在 main.rs 中配置日志级别

use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

fn init_logging() {
    // 从环境变量读取日志级别，默认 Info
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info"));

    tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer())
        .init();
}

// 使用方式：
// RUST_LOG=trace ./gs_usb_daemon  # 开启所有日志（包括 Trace）
// RUST_LOG=debug ./gs_usb_daemon  # 开启 Debug 及以上
// RUST_LOG=info ./gs_usb_daemon   # 默认：Info 及以上
```

### 9.5.2 异步 IO 选择（实时性关键）

**⚠️ 重要**：针对机械臂力控（1kHz）场景，**严禁使用 `tokio`**。

**为什么不用 `tokio`**：

1. **调度器抖动**：Tokio 是协作式多任务运行时，设计目标是高吞吐量（10,000+ 并发），而不是低延迟。调度器的"唤醒 -> 排队 -> 执行"过程会引入微秒甚至毫秒级的抖动。

2. **批处理机制**：为了吞吐量，异步运行时通常会进行系统调用批处理，可能导致 CAN 帧被迫"等一等"其他事件，破坏实时性。

3. **不可抢占**：如果同一个 Worker 线程上的另一个任务执行时间稍长，CAN 处理任务就会被阻塞。

**推荐方案：多线程阻塞 IO**

**架构原则**：
- ✅ **One Thread Per Task**：一个线程只做一件事（收 USB，或者收 IPC）
- ✅ **Blocking IO**：永远不要 sleep。如果没有数据，线程就阻塞在 `read()` 上。一旦数据到达，内核会立刻（微秒级）唤醒线程。
- ✅ **macOS QoS**：必须将线程优先级设置为 `UserInteractive`，否则 macOS 可能会把线程调度到 E-core，导致巨大的延迟波动。

**实现方式**：

```rust
// 每个 Socket 一个接收线程（阻塞 IO）

// 线程 1: USB 接收
thread::Builder::new()
    .name("usb_receive".into())
    .spawn(move || {
        macos_qos::set_high_priority();
        loop {
            // 阻塞在 USB read，数据到达立即唤醒
            let frame = adapter.receive().unwrap();
            // 立即发送到 IPC
            socket.send_to(&encode_frame(&frame), &client_addr).unwrap();
        }
    })?;

// 线程 2: IPC 接收
thread::Builder::new()
    .name("ipc_receive".into())
    .spawn(move || {
        macos_qos::set_high_priority();
        loop {
            // 阻塞在 socket recv，数据到达立即唤醒
            let (len, addr) = socket.recv_from(&mut buf).unwrap();
            // 立即发送到 USB
            adapter.send(decode_frame(&buf[..len])).unwrap();
        }
    })?;
```

**性能对比**：

| 方案 | 延迟 | 抖动 | 适用场景 |
|------|------|------|----------|
| **多线程阻塞 IO** | < 200us | < 100us | ✅ 实时控制（力控） |
| tokio 异步运行时 | 200us - 2ms | 100us - 1ms | ❌ 不适合实时场景 |
| 轮询 + sleep | 1ms - 5ms | 不可控 | ❌ 不适合实时场景 |

### 9.5.3 状态机去抖动

**问题**：macOS 在 USB 设备枚举时，可能会在短时间内多次出现"连接-断开-连接"的抖动。

**解决方案**：在进入 `Reconnecting` 状态前，增加冷却时间（Debounce）。

**实现**：已在 `device_manager_loop` 中实现，冷却时间 500ms。

**可配置参数**：

```rust
pub struct DaemonConfig {
    // ... 其他配置 ...

    /// 重连冷却时间（防止 USB 枚举抖动）
    /// 默认 500ms
    pub reconnect_debounce: Duration,
}
```

---

## 10. 风险与限制

### 10.1 已知限制

1. **单点故障**：如果守护进程崩溃，所有客户端都会断开
   - **缓解**：实现守护进程自动重启（通过系统服务）
   - **缓解**：客户端实现自动重连
   - **改进**：守护进程实现状态机，永不退出，自动处理设备故障

2. **网络延迟**：UDS/UDP 网络会增加少量延迟
   - **UDS**：微秒级延迟（内核内存复制），对 1kHz 控制回路完全可接受
   - **UDP**：< 1ms 延迟（本地回环），对 1kHz 控制回路可接受
   - **优化**：默认使用 UDS，性能最优

3. **多设备支持**：当前设计假设只有一个 GS-USB 设备
   - **扩展**：可以通过设备序列号支持多设备，每个设备运行一个守护进程
   - **扩展**：未来可以支持一个守护进程管理多个设备（需要修改协议）

4. **USB 热拔插**：设备物理断开后需要重连
   - **改进**：实现状态机自动处理热拔插，无需人工干预
   - **限制**：重连期间客户端会收到错误，需要实现重试逻辑

### 10.2 潜在问题

1. **UDS/UDP 丢包**：UDP 不保证可靠传输（UDS 更可靠）
   - **缓解**：本地回环通常不会丢包
   - **缓解**：如果丢包，客户端会收到超时错误，可以重试
   - **优化**：默认使用 UDS，更可靠

2. **客户端超时**：如果客户端崩溃，守护进程需要清理
   - **改进**：实现客户端心跳机制，定期更新活动时间
   - **改进**：后台线程定期清理超时客户端

3. **设备恢复**：如果设备物理断开，守护进程需要检测并恢复
   - **改进**：实现状态机（Connected → Disconnected → Reconnecting → Connected）
   - **改进**：守护进程永不退出，自动重连设备

---

## 11. 总结

本方案通过实现一个用户态守护进程，解决了 macOS 下 GS-USB 适配器断开重连后无法正常工作的问题。守护进程始终保持设备连接，通过 Unix Domain Socket (UDS) 或 UDP 端口向多个客户端提供 CAN 总线访问。

### 11.1 核心优势

- ✅ **避免断开重连问题**：守护进程始终保持设备连接，应用进程可以随时连接/断开
- ✅ **支持多客户端并发访问**：多个应用进程可以同时连接到守护进程
- ✅ **与现有代码兼容**：实现 `CanAdapter` trait，上层代码无需修改
- ✅ **易于部署和维护**：单例文件锁、系统服务支持
- ✅ **高性能**：UDS 零拷贝优化，微秒级延迟
- ✅ **健壮性**：状态机自动处理 USB 热拔插，守护进程永不退出
- ✅ **可扩展性**：支持 CAN ID 过滤，减少网络流量

### 11.2 关键优化

1. **实时性优化**（针对力控场景）：
   - **多线程阻塞架构**：每个 IO 操作使用独立线程，阻塞在系统调用上
   - **零延迟唤醒**：数据到达时，内核立即唤醒线程（微秒级）
   - **macOS QoS**：设置线程优先级，避免被调度到 E-core（能效核）
   - ❌ **不使用 tokio**：异步运行时会增加调度器抖动，不适合实时场景
   - ❌ **不使用 sleep**：在热路径上严禁 sleep，会导致不可控延迟

2. **传输层优化**：
   - 默认使用 Unix Domain Socket (UDS)，延迟最低（< 50us）
   - 可选 UDP 支持，用于跨机器调试（< 200us）

3. **协议优化**：
   - Sequence Number 支持错误反馈
   - CAN ID 过滤减少网络流量
   - 零拷贝编码（栈上缓冲区）

4. **架构优化**：
   - RwLock 优化读取性能（减少锁竞争）
   - 状态机自动处理设备故障（带去抖动）
   - 客户端心跳机制防止超时

5. **可靠性优化**：
   - 单例文件锁（比 pgrep 更可靠）
   - 守护进程永不退出，自动重连
   - 客户端自动重连机制

6. **实施优化**：
   - 日志分级策略（tracing，避免高频日志成为瓶颈）
   - 状态机去抖动（防止 USB 枚举抖动）

### 11.3 下一步

1. 按照实施计划逐步实现
2. 进行充分测试（单元测试、集成测试、压力测试）
3. 收集用户反馈，持续优化
4. 考虑未来扩展（多设备支持、性能监控等）

---

**文档版本**：v2.2（实时性优化：多线程阻塞架构）
**创建日期**：2024-12
**最后更新**：2024-12
**作者**：基于 macOS GS-USB 重连问题分析和专业架构评估

**更新日志**：
- v2.2：实时性优化（多线程阻塞架构、macOS QoS、严禁 tokio 和 sleep）
- v2.1：添加实施最佳实践（日志分级、异步 IO 选择、状态机去抖动）
- v2.0：基于专业评估优化（UDS、状态机、心跳、文件锁、零拷贝等）
- v1.0：初始版本

**性能目标**：
- 往返延迟：< 200us（USB <-> Daemon <-> Client）
- 延迟抖动：< 100us（P99）
- 控制频率：1kHz (1ms 周期) 或更高

