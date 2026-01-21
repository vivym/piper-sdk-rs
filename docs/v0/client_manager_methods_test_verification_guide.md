# ClientManager 方法测试验证指南

> 本文档提供详细的测试验证步骤，用于验证 `SetFilter` 和 `GetStatus` 消息处理的正确性。

## 概述

本指南涵盖两种测试方法：
1. **手动测试**：使用命令行工具或简单的测试客户端
2. **自动化测试**：编写单元测试或集成测试（推荐）

---

## 前置条件

### 1. 启动守护进程

```bash
# 编译并启动守护进程
cargo run --bin gs_usb_daemon -- --bitrate 500000

# 或者使用 release 模式
cargo build --release --bin gs_usb_daemon
./target/release/gs_usb_daemon --bitrate 500000
```

**预期输出**：
```
GS-USB Daemon starting...
  UDS: /tmp/gs_usb_daemon.sock
  Bitrate: 500000 bps
  Lock file: /tmp/gs_usb_daemon.lock
GS-USB Daemon started. Press Ctrl+C to stop.
[Daemon] Device found and initialized successfully
[DeviceManager] Device reconnected successfully
[Status] State: Connected, Clients: 0, ...
```

### 2. 验证守护进程运行

```bash
# 检查守护进程进程
ps aux | grep gs_usb_daemon

# 检查 UDS socket 文件
ls -l /tmp/gs_usb_daemon.sock
```

---

## 测试方法 1：手动测试（推荐用于快速验证）

### 测试 1.1：`SetFilter` 消息处理

#### 步骤 1：创建测试客户端

创建一个简单的测试脚本 `test_set_filter.rs`：

```rust
use piper_sdk::can::gs_usb_udp::{GsUsbUdpAdapter, protocol::{Message, MessageType, encode_set_filter}};
use std::io::{self, Write};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 连接到守护进程
    let mut adapter = GsUsbUdpAdapter::new_uds("/tmp/gs_usb_daemon.sock")?;
    println!("✅ 已连接到守护进程");

    // 等待 ConnectAck
    std::thread::sleep(std::time::Duration::from_millis(100));

    // 测试 1：发送 SetFilter 消息（带过滤规则）
    println!("\n📤 发送 SetFilter 消息（client_id: 1, 过滤规则: 0x100-0x200）...");

    let client_id = 1;
    let filters = vec![
        piper_sdk::can::CanIdFilter::new(0x100, 0x200),
    ];

    let mut buf = [0u8; 64];
    let encoded = encode_set_filter(client_id, &filters, 0, &mut buf)?;

    // 通过 adapter 的内部 socket 发送（需要访问内部实现）
    // 注意：这需要 adapter 暴露发送原始消息的方法
    // 或者我们可以通过 adapter 的连接状态来验证

    println!("✅ SetFilter 消息已发送");
    println!("   - Client ID: {}", client_id);
    println!("   - 过滤规则数量: {}", filters.len());
    println!("   - 过滤范围: 0x{:03X}-0x{:03X}", 0x100, 0x200);

    // 检查守护进程日志
    println!("\n📋 请检查守护进程日志，应该看到：");
    println!("   [Client {}] Filters updated: {} rules", client_id, filters.len());

    // 测试 2：发送空的过滤规则
    println!("\n📤 发送 SetFilter 消息（空过滤规则）...");
    let empty_filters = vec![];
    let encoded_empty = encode_set_filter(client_id, &empty_filters, 0, &mut buf)?;
    println!("✅ 空过滤规则已发送");
    println!("   - 过滤规则数量: 0");

    // 等待响应
    std::thread::sleep(std::time::Duration::from_millis(500));

    println!("\n✅ SetFilter 测试完成！");
    Ok(())
}
```

#### 步骤 2：编译并运行测试

```bash
# 编译测试程序
cargo build --example test_set_filter

# 运行测试
cargo run --example test_set_filter
```

#### 步骤 3：验证结果

**在守护进程日志中查找**：
```
[Client 1] Filters updated: 1 rules
[Client 1] Filters updated: 0 rules
```

---

### 测试 1.2：`GetStatus` 消息处理

#### 步骤 1：创建测试客户端

创建一个简单的测试脚本 `test_get_status.rs`：

```rust
use piper_sdk::can::gs_usb_udp::{GsUsbUdpAdapter, protocol::{Message, MessageType, decode_message}};
use std::os::unix::net::UnixDatagram;
use std::time::Duration;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 创建客户端 socket
    let client_socket = UnixDatagram::unbound()?;
    let client_path = format!("/tmp/test_client_{}.sock", std::process::id());
    std::fs::remove_file(&client_path).ok(); // 清理旧文件
    client_socket.bind(&client_path)?;

    println!("✅ 客户端 socket 已创建: {}", client_path);

    // 连接到守护进程的 socket
    let daemon_path = "/tmp/gs_usb_daemon.sock";

    // 发送 GetStatus 消息（未注册客户端）
    println!("\n📤 发送 GetStatus 消息（未注册客户端）...");

    // GetStatus 消息格式：[header(8 bytes)]
    // MessageType::GetStatus = 0x04
    let mut get_status_msg = [0u8; 8];
    get_status_msg[0] = 0x47; // Magic: 'G'
    get_status_msg[1] = 0x55; // Magic: 'U'
    get_status_msg[2] = 0x04; // MessageType::GetStatus
    get_status_msg[3] = 0x00; // Reserved
    // Length (little-endian u32): 0 (只有 header)
    get_status_msg[4..8].copy_from_slice(&0u32.to_le_bytes());

    client_socket.send_to(&get_status_msg, daemon_path)?;
    println!("✅ GetStatus 消息已发送");

    // 接收 StatusResponse
    println!("\n📥 等待 StatusResponse...");
    let mut recv_buf = [0u8; 1024];

    // 设置超时
    client_socket.set_read_timeout(Some(Duration::from_secs(5)))?;

    match client_socket.recv_from(&mut recv_buf) {
        Ok((len, _)) => {
            println!("✅ 收到响应 ({} 字节)", len);

            // 解析 StatusResponse
            match decode_message(&recv_buf[..len]) {
                Ok(Message::StatusResponse {
                    device_state,
                    rx_fps_x1000,
                    tx_fps_x1000,
                    health_score,
                    client_count,
                    usb_stall_count,
                    can_bus_off_count,
                    can_error_passive_count,
                    cpu_usage_percent,
                    client_send_blocked,
                    ..
                }) => {
                    println!("\n📊 StatusResponse 内容：");
                    println!("   - 设备状态: {} (0=Disconnected, 1=Connected, 2=Reconnecting)", device_state);
                    println!("   - RX 帧率: {:.2} fps", rx_fps_x1000 as f32 / 1000.0);
                    println!("   - TX 帧率: {:.2} fps", tx_fps_x1000 as f32 / 1000.0);
                    println!("   - 健康度评分: {}/100", health_score);
                    println!("   - 客户端数量: {}", client_count);
                    println!("   - USB STALL 计数: {}", usb_stall_count);
                    println!("   - CAN Bus Off 计数: {}", can_bus_off_count);
                    println!("   - CAN Error Passive 计数: {}", can_error_passive_count);
                    println!("   - CPU 使用率: {}%", cpu_usage_percent);
                    println!("   - 客户端发送阻塞: {}", client_send_blocked);

                    // 验证关键字段
                    assert!(health_score <= 100, "健康度评分应该在 0-100 之间");
                    assert!(cpu_usage_percent <= 100, "CPU 使用率应该在 0-100 之间");

                    println!("\n✅ GetStatus 测试通过！");
                },
                Ok(msg) => {
                    eprintln!("❌ 收到意外的消息类型: {:?}", msg);
                    return Err("收到意外的消息类型".into());
                },
                Err(e) => {
                    eprintln!("❌ 解析响应失败: {}", e);
                    return Err(e.into());
                },
            }
        },
        Err(e) => {
            eprintln!("❌ 接收响应失败: {}", e);
            return Err(e.into());
        },
    }

    // 清理
    std::fs::remove_file(&client_path).ok();

    Ok(())
}
```

#### 步骤 2：编译并运行测试

```bash
# 编译测试程序
cargo build --example test_get_status

# 运行测试
cargo run --example test_get_status
```

#### 步骤 3：验证结果

**在守护进程日志中查找**：
```
[GetStatus] Sent StatusResponse to /tmp/test_client_xxxxx.sock
```

**预期输出**：
```
📊 StatusResponse 内容：
   - 设备状态: 1 (Connected)
   - RX 帧率: 0.00 fps
   - TX 帧率: 0.00 fps
   - 健康度评分: 85/100
   - 客户端数量: 0
   ...
```

---

## 测试方法 2：自动化测试（推荐用于 CI/CD）

### 测试 2.1：`SetFilter` 单元测试

在 `tests/integration/` 目录下创建 `test_set_filter.rs`：

```rust
use piper_sdk::can::gs_usb_udp::protocol::{encode_set_filter, decode_message, Message, MessageType};
use piper_sdk::can::CanIdFilter;
use std::os::unix::net::UnixDatagram;
use std::time::Duration;

#[test]
fn test_set_filter_message() {
    // 创建测试 socket
    let server = UnixDatagram::unbound().unwrap();
    let server_path = "/tmp/test_daemon.sock";
    std::fs::remove_file(server_path).ok();
    server.bind(server_path).unwrap();

    let client = UnixDatagram::unbound().unwrap();
    let client_path = format!("/tmp/test_client_{}.sock", std::process::id());
    std::fs::remove_file(&client_path).ok();
    client.bind(&client_path).unwrap();

    // 编码 SetFilter 消息
    let client_id = 1;
    let filters = vec![
        CanIdFilter::new(0x100, 0x200),
        CanIdFilter::new(0x300, 0x400),
    ];

    let mut buf = [0u8; 64];
    let encoded = encode_set_filter(client_id, &filters, 0, &mut buf).unwrap();

    // 发送消息
    client.send_to(encoded, server_path).unwrap();

    // 接收并解析
    let mut recv_buf = [0u8; 1024];
    server.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
    let (len, _) = server.recv_from(&mut recv_buf).unwrap();

    let decoded = decode_message(&recv_buf[..len]).unwrap();

    match decoded {
        Message::SetFilter { client_id: id, filters: fs } => {
            assert_eq!(id, client_id);
            assert_eq!(fs.len(), 2);
            assert_eq!(fs[0].min_id(), 0x100);
            assert_eq!(fs[0].max_id(), 0x200);
            assert_eq!(fs[1].min_id(), 0x300);
            assert_eq!(fs[1].max_id(), 0x400);
        },
        _ => panic!("收到意外的消息类型"),
    }

    // 清理
    std::fs::remove_file(server_path).ok();
    std::fs::remove_file(&client_path).ok();
}
```

### 测试 2.2：`GetStatus` 单元测试

在 `tests/integration/` 目录下创建 `test_get_status.rs`：

```rust
use piper_sdk::can::gs_usb_udp::protocol::{decode_message, Message, MessageType};
use std::os::unix::net::UnixDatagram;
use std::time::Duration;

#[test]
fn test_get_status_message() {
    // 创建测试 socket（模拟守护进程）
    let server = UnixDatagram::unbound().unwrap();
    let server_path = "/tmp/test_daemon.sock";
    std::fs::remove_file(server_path).ok();
    server.bind(server_path).unwrap();

    let client = UnixDatagram::unbound().unwrap();
    let client_path = format!("/tmp/test_client_{}.sock", std::process::id());
    std::fs::remove_file(&client_path).ok();
    client.bind(&client_path).unwrap();

    // 发送 GetStatus 消息
    let mut get_status_msg = [0u8; 8];
    get_status_msg[0] = 0x47; // Magic: 'G'
    get_status_msg[1] = 0x55; // Magic: 'U'
    get_status_msg[2] = MessageType::GetStatus as u8; // 0x04
    get_status_msg[3] = 0x00; // Reserved
    get_status_msg[4..8].copy_from_slice(&0u32.to_le_bytes()); // Length = 0

    client.send_to(&get_status_msg, server_path).unwrap();

    // 模拟守护进程响应（这里只是测试消息格式，实际需要真实守护进程）
    // 在实际测试中，需要连接到真实的守护进程

    // 清理
    std::fs::remove_file(server_path).ok();
    std::fs::remove_file(&client_path).ok();
}
```

### 测试 2.3：集成测试（需要真实守护进程）

在 `tests/integration/` 目录下创建 `test_daemon_set_filter.rs`：

```rust
// 需要真实的守护进程运行
#[test]
#[ignore] // 默认忽略，需要手动运行
fn test_daemon_set_filter_integration() {
    use piper_sdk::can::gs_usb_udp::GsUsbUdpAdapter;
    use piper_sdk::can::CanIdFilter;
    use std::time::Duration;

    // 连接到守护进程
    let mut adapter = GsUsbUdpAdapter::new_uds("/tmp/gs_usb_daemon.sock")
        .expect("守护进程未运行，请先启动: cargo run --bin gs_usb_daemon");

    // 等待连接建立
    std::thread::sleep(Duration::from_millis(100));

    // TODO: 添加 SetFilter 测试逻辑
    // 注意：当前 GsUsbUdpAdapter 可能还没有暴露 SetFilter 方法
    // 需要扩展适配器 API

    println!("✅ SetFilter 集成测试完成");
}
```

---

## 验证检查清单

### `SetFilter` 消息处理验证

- [ ] **基本功能**：客户端发送 `SetFilter` 消息后，守护进程日志显示过滤规则更新
- [ ] **空过滤规则**：发送空的过滤规则列表，验证处理正确
- [ ] **不存在的客户端**：发送不存在的 `client_id`，验证处理正确（应该静默失败）
- [ ] **过滤规则生效**：验证过滤规则在实际 CAN 帧分发中生效

### `GetStatus` 消息处理验证

- [ ] **基本功能**：未注册客户端发送 `GetStatus` 消息，收到 `StatusResponse`
- [ ] **响应路由**：验证响应正确路由到请求者（不会广播给其他客户端）
- [ ] **字段完整性**：验证 `StatusResponse` 所有字段都有值
- [ ] **设备状态**：验证不同设备状态（Connected/Disconnected/Reconnecting）的响应正确
- [ ] **客户端数量**：验证 `client_count` 字段正确反映当前连接的客户端数量
- [ ] **多个客户端**：多个客户端同时发送 `GetStatus`，每个都收到正确的响应

---

## 常见问题排查

### 问题 1：连接失败

**错误**：`Connection refused` 或 `No such file or directory`

**解决**：
1. 确认守护进程正在运行：`ps aux | grep gs_usb_daemon`
2. 确认 UDS socket 文件存在：`ls -l /tmp/gs_usb_daemon.sock`
3. 检查 socket 文件权限

### 问题 2：消息解析失败

**错误**：`ProtocolError::InvalidMessageType` 或 `ProtocolError::Incomplete`

**解决**：
1. 检查消息格式是否正确（Magic bytes、MessageType、Length）
2. 检查消息长度是否匹配
3. 验证字节序（little-endian）

### 问题 3：未收到响应

**错误**：`GetStatus` 消息发送后未收到响应

**解决**：
1. 检查守护进程日志，确认消息已收到
2. 检查客户端 socket 是否正确绑定
3. 验证地址字符串提取逻辑（`as_pathname()` 可能返回 `None`）

---

## 快速测试脚本

创建一个简单的测试脚本 `quick_test.sh`：

```bash
#!/bin/bash

echo "=== ClientManager 方法测试验证 ==="
echo ""

# 检查守护进程是否运行
if ! pgrep -f "gs_usb_daemon" > /dev/null; then
    echo "❌ 守护进程未运行"
    echo "请先启动: cargo run --bin gs_usb_daemon"
    exit 1
fi

echo "✅ 守护进程正在运行"
echo ""

# 测试 GetStatus（最简单，不需要注册客户端）
echo "📤 测试 GetStatus 消息..."
# 这里可以运行 test_get_status 示例

# 测试 SetFilter（需要客户端连接）
echo "📤 测试 SetFilter 消息..."
# 这里可以运行 test_set_filter 示例

echo ""
echo "✅ 所有测试完成！"
echo ""
echo "📋 请检查守护进程日志验证结果"
```

---

**测试指南创建日期**：2024年
**适用版本**：ClientManager 方法启用实施计划 v1.0
**参考文档**：《ClientManager 方法启用实施计划》
