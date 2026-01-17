# GS-USB 协议测试安全性分析报告

> **问题**：USB Adapter 已插入，但 CAN 口上的 Piper 机械臂未启动。能否在不启动 Piper 的情况下测试 GS-USB 协议的正确性？

**分析日期**：2024-12-XX
**分析目的**：确保测试安全性，避免意外启动机械臂

---

## 1. 执行摘要

✅ **结论：可以在不启动 Piper 机械臂的情况下，安全地测试 GS-USB 协议的大部分功能。**

**关键发现**：
- GS-USB 协议测试主要发生在 **USB 通信层**，不直接依赖 CAN 总线上的设备
- 使用 **Loopback 模式**可以实现端到端测试，且不会向 CAN 总线发送帧
- 部分测试（如接收）需要 Loopback 模式或 CAN 总线连接，但不影响协议正确性验证

---

## 2. 技术分析

### 2.1 GS-USB 协议分层架构

GS-USB 协议可以分为以下层次：

```
┌─────────────────────────────────────────────────┐
│ 应用层：Piper SDK (send/receive)                │
├─────────────────────────────────────────────────┤
│ 适配层：GsUsbCanAdapter (PiperFrame <-> GsUsbFrame) │
├─────────────────────────────────────────────────┤
│ USB 通信层：GsUsbDevice (控制传输 + Bulk 传输)   │
├─────────────────────────────────────────────────┤
│ USB 硬件层：rusb 库                             │
├─────────────────────────────────────────────────┤
│ CAN 控制器层：GS-USB Adapter 固件               │
├─────────────────────────────────────────────────┤
│ CAN 物理层：CAN 总线（可选，Loopback 时绕过）   │
└─────────────────────────────────────────────────┘
```

**关键点**：
- **USB 通信层**以上的测试不需要实际 CAN 总线连接
- **CAN 物理层**在 Loopback 模式下被绕过，不会向外发送帧

### 2.2 GS-USB 设备模式分析

GS-USB 协议支持多种模式，每种模式对 CAN 总线的影响不同：

| 模式 | 标志位 | CAN 总线行为 | 是否安全测试 |
|------|--------|-------------|-------------|
| **NORMAL** | `0` | 正常收发，会向总线发送帧 | ⚠️ **不安全**（会发送帧） |
| **LISTEN_ONLY** | `1 << 0` | 只接收，不发送 ACK，不发送帧 | ✅ **安全**（不会发送） |
| **LOOP_BACK** | `1 << 1` | 内部回环，不向总线发送 | ✅ **安全**（不会发送） |
| **TRIPLE_SAMPLE** | `1 << 2` | 三重采样（通常与 NORMAL 组合） | ⚠️ 取决于组合模式 |

**推荐测试模式**：
- ✅ **`GS_CAN_MODE_LOOP_BACK`**：最安全，可以完整测试发送/接收路径
- ✅ **`GS_CAN_MODE_LISTEN_ONLY`**：安全，但无法测试发送路径

### 2.3 测试功能分类

#### ✅ 可以在不连接 CAN 设备时测试的功能

**1. USB 设备枚举和配置**
- ✅ 设备扫描（`GsUsbDevice::scan()`）
- ✅ USB 接口声明（`claim_interface()`）
- ✅ 内核驱动分离（`detach_kernel_driver()`）
- ✅ 设备能力查询（`device_capability()`）
- ✅ HOST_FORMAT 握手（`send_host_format()`）
- ✅ 波特率配置（`set_bitrate()`）
- ✅ 模式设置（`start()` with `GS_CAN_MODE_LOOP_BACK`）

**2. USB 控制传输**
- ✅ 所有控制请求（HOST_FORMAT、BITTIMING、MODE、BT_CONST 等）
- ✅ 控制传输参数验证（`wValue`、`wIndex`、数据长度）

**3. 帧编码/解码（已覆盖）**
- ✅ `PiperFrame` ↔ `GsUsbFrame` 转换（单元测试已覆盖）
- ✅ CAN ID 标志处理（标准帧/扩展帧）
- ✅ 数据长度验证（DLC）

**4. USB Bulk 传输（发送路径）**
- ✅ `send_raw()` USB Bulk OUT 传输
- ✅ 帧打包（`pack_to()`）
- ✅ Fire-and-Forget 语义验证

**5. 错误处理**
- ✅ 设备未启动时的错误（`NotStarted`）
- ✅ USB 通信错误（`Timeout`、`Entity not found` 等）
- ✅ 帧格式错误

#### ⚠️ 需要 Loopback 模式或 CAN 总线连接的功能

**1. 接收路径（Loopback 模式可测试）**
- ⚠️ `receive_raw()` USB Bulk IN 传输
- ⚠️ TX Echo 过滤（三层过滤漏斗）
- ⚠️ 帧解包（`unpack_from_bytes()`）

**2. 端到端功能（Loopback 模式可测试）**
- ⚠️ `send()` → `receive()` 完整流程
- ⚠️ Echo 回显验证
- ⚠️ 错误帧过滤

**3. 无法在不启动设备时测试的功能**
- ❌ 实际 CAN 总线通信（需要真实的 CAN 设备）
- ❌ CAN 错误处理（Bus Off、错误计数等）
- ❌ 波特率实际匹配验证（需要 CAN 总线上的设备使用相同波特率）

---

## 3. 安全测试方案

### 3.1 方案 A：Loopback 模式测试（推荐 ⭐⭐⭐）

**优点**：
- ✅ 完全安全，不会向 CAN 总线发送帧
- ✅ 可以测试完整的发送/接收路径
- ✅ 可以验证 Echo 过滤逻辑
- ✅ 无需外部 CAN 设备

**实现方式**：
```rust
// 使用 Loopback 模式启动设备
adapter.device.start(GS_CAN_MODE_LOOP_BACK)?;

// 发送的帧会在设备内部回环，可以通过 receive() 接收
adapter.send(frame)?;
let received = adapter.receive()?; // 会收到 Echo
```

**测试覆盖**：
- ✅ USB 通信层（100%）
- ✅ 帧编码/解码（100%）
- ✅ 发送路径（100%）
- ✅ 接收路径（Echo 过滤，100%）
- ✅ 端到端流程（100%）

### 3.2 方案 B：Listen-Only 模式测试

**优点**：
- ✅ 完全安全，不发送帧，不发送 ACK
- ✅ 可以测试设备配置和接收路径

**缺点**：
- ❌ 无法测试发送路径
- ❌ 需要外部 CAN 设备发送帧才能测试接收

**适用场景**：
- 测试设备配置和接收路径（需要外部 CAN 信号源）

### 3.3 方案 C：只测试 USB 层（最保守）

**优点**：
- ✅ 最安全，完全不涉及 CAN 层

**缺点**：
- ❌ 无法测试接收路径
- ❌ 无法验证 Echo 过滤

**实现方式**：
- 测试设备扫描、配置、控制传输
- 测试发送路径（USB Bulk OUT 会成功，但没有实际 CAN 帧）
- 跳过接收测试

---

## 4. 推荐测试策略

### 4.1 分阶段测试

**阶段 1：USB 层测试（最安全）** ⭐⭐⭐
- 测试设备扫描、配置、控制传输
- 测试发送路径（USB 层）
- **状态**：不启动 CAN 控制器（或不设置模式）

**阶段 2：Loopback 模式测试（推荐）** ⭐⭐⭐
- 使用 `GS_CAN_MODE_LOOP_BACK` 启动设备
- 测试完整的发送/接收路径
- 验证 Echo 过滤逻辑
- **状态**：安全，不会向 CAN 总线发送帧

**阶段 3：实际 CAN 总线测试（需要 Piper）** ⚠️
- 在确认协议正确后，连接 Piper 进行实际测试
- 建议使用较低波特率（125kbps 或 250kbps）
- 建议先测试读取（LISTEN_ONLY 模式）

### 4.2 具体测试用例建议

#### 测试用例 1：USB 设备层测试（不启动 CAN）
```rust
#[test]
#[ignore]
fn test_usb_device_layer_only() {
    // 1. 扫描设备
    let devices = GsUsbDevice::scan().unwrap();
    assert!(!devices.is_empty());

    // 2. 打开设备
    let mut device = devices.remove(0);

    // 3. 发送 HOST_FORMAT
    device.send_host_format().unwrap();

    // 4. 查询设备能力
    let cap = device.device_capability().unwrap();
    println!("Clock: {} Hz", cap.fclk_can);

    // 5. 设置波特率（但不启动）
    device.set_bitrate(250_000).unwrap();

    // ✅ 不调用 start()，因此不会启动 CAN 控制器
    // ✅ 不会向 CAN 总线发送任何帧
}
```

#### 测试用例 2：Loopback 模式端到端测试（推荐）
```rust
#[test]
#[ignore]
fn test_loopback_mode_safe() {
    let mut adapter = GsUsbCanAdapter::new().unwrap();

    // ⚠️ 修改 configure() 使用 LOOP_BACK 模式
    // 或者添加一个新方法：
    // adapter.configure_loopback(250_000)?;

    // 发送帧
    let tx_frame = PiperFrame::new_standard(0x123, &[0x01, 0x02]);
    adapter.send(tx_frame).unwrap();

    // 接收 Echo（Loopback 模式下会收到）
    let rx_frame = adapter.receive().unwrap();
    assert_eq!(rx_frame.id, 0x123);

    // ✅ 不会向 CAN 总线发送帧
}
```

#### 测试用例 3：Listen-Only 模式测试（安全但功能受限）
```rust
#[test]
#[ignore]
fn test_listen_only_mode() {
    let mut adapter = GsUsbCanAdapter::new().unwrap();

    // 使用 LISTEN_ONLY 模式启动（不发送帧，不发送 ACK）
    adapter.device.start(GS_CAN_MODE_LISTEN_ONLY)?;

    // ✅ 安全，但无法测试发送路径
    // ⚠️ 需要外部 CAN 信号源才能测试接收
}
```

---

## 5. 代码修改建议

### 5.1 添加 Loopback 模式配置方法

在 `GsUsbCanAdapter` 中添加：

```rust
impl GsUsbCanAdapter {
    /// 配置并启动设备（Loopback 模式，安全测试）
    pub fn configure_loopback(&mut self, bitrate: u32) -> Result<(), CanError> {
        let _ = self.device.send_host_format();
        self.device
            .set_bitrate(bitrate)
            .map_err(|e| CanError::Device(format!("Failed to set bitrate: {}", e)))?;

        // 使用 LOOP_BACK 模式，不会向 CAN 总线发送帧
        self.device
            .start(GS_CAN_MODE_LOOP_BACK)
            .map_err(|e| CanError::Device(format!("Failed to start device: {}", e)))?;

        self.started = true;
        trace!("GS-USB device started in LOOP_BACK mode at {} bps", bitrate);
        Ok(())
    }
}
```

### 5.2 修改现有 `configure()` 方法（可选）

如果希望默认使用安全模式，可以：
- 添加参数：`configure(&mut self, bitrate: u32, mode: u32)`
- 或者添加配置选项：`configure_safe(&mut self, bitrate: u32)` → 使用 `LOOP_BACK`

---

## 6. 风险评估

### 6.1 使用 NORMAL 模式的风险

| 风险项 | 风险等级 | 说明 |
|--------|---------|------|
| **向 CAN 总线发送帧** | 🔴 **高** | 可能触发 Piper 机械臂意外动作 |
| **发送错误格式帧** | 🟡 **中** | 可能导致 Piper 解析错误，但不会执行动作 |
| **波特率不匹配** | 🟡 **中** | 可能导致 Piper 接收不到帧，但不会有副作用 |

### 6.2 Loopback 模式的风险

| 风险项 | 风险等级 | 说明 |
|--------|---------|------|
| **向 CAN 总线发送帧** | ✅ **无** | Loopback 模式不向总线发送 |
| **设备固件错误** | 🟢 **低** | 理论上可能，但概率极低 |

### 6.3 推荐安全措施

1. ✅ **始终使用 Loopback 模式进行初始测试**
2. ✅ **在测试代码中明确标注使用的模式**
3. ✅ **在连接到 Piper 之前，先在 Loopback 模式验证所有功能**
4. ✅ **实际连接到 Piper 时，使用 LISTEN_ONLY 模式先测试接收**

---

## 7. 结论与建议

### 7.1 最终结论

✅ **可以在不启动 Piper 机械臂的情况下，安全地测试 GS-USB 协议。**

**推荐方案**：
1. **第一阶段**：使用 **Loopback 模式**进行端到端测试
   - 可以测试完整的发送/接收路径
   - 不会向 CAN 总线发送帧
   - 可以验证 Echo 过滤逻辑

2. **第二阶段**：使用 **Listen-Only 模式**连接 Piper
   - 先测试接收路径（Piper 发送的数据）
   - 不会向 Piper 发送帧

3. **第三阶段**：在确认协议正确后，使用 **Normal 模式**
   - 进行实际的 CAN 通信测试
   - 建议从低波特率和简单命令开始

### 7.2 实施建议

1. ✅ **立即实施**：添加 `configure_loopback()` 方法
2. ✅ **立即实施**：创建 Loopback 模式的集成测试
3. ⏳ **后续实施**：在实际连接 Piper 前，先在 Loopback 模式验证所有功能
4. ⏳ **后续实施**：建立 Piper 连接时的安全检查清单

### 7.3 测试优先级

| 优先级 | 测试内容 | 模式 | 状态 |
|--------|---------|------|------|
| **P0** | USB 设备层测试（不启动 CAN） | 无 | ✅ 已实现 |
| **P0** | Loopback 模式端到端测试 | `LOOP_BACK` | ⏳ 待实现 |
| **P1** | Listen-Only 模式接收测试 | `LISTEN_ONLY` | ⏳ 待实现（需要 Piper） |
| **P2** | Normal 模式实际通信 | `NORMAL` | ⏳ 待实现（需要 Piper） |

---

## 8. 参考资料

- GS-USB 协议文档：`docs/v0/gs_usb_implementation_plan_v3.md`
- 实现方案：`docs/v0/gs_usb_todo_list.md`
- 当前代码：`src/can/gs_usb/`

---

**报告版本**：v1.0
**最后更新**：2024-12-XX

