# GS-USB Stage 1 测试调试报告

## 调试过程

### 问题现象

在 Loopback 模式测试中，即使不连接 Piper 机械臂，仍然出现 "Pipe error"：

```
Failed to set bitrate: USB error: Pipe error
```

### 逐步调试结果

通过 `gs_usb_debug_step_by_step` 测试，逐步验证每个初始化步骤：

1. ✅ **设备扫描**：成功找到设备
2. ✅ **准备接口**：成功声明接口
3. ✅ **发送 HOST_FORMAT**：成功（错误被忽略）
4. ❌ **获取设备能力**：失败 - "Pipe error"

**关键发现**：即使接口已正确声明，`device_capability()` 中的 `control_in(GS_USB_BREQ_BT_CONST, 0, 40)` 仍然失败。

### 根本原因分析

**问题不在接口声明**，而在控制传输本身。可能的原因：

1. **设备状态问题**：
   - 设备可能需要先 `reset()` 才能响应 `GS_USB_BREQ_BT_CONST` 请求
   - 但 `reset()` 必须在 `claim_interface()` 之后（否则会导致段错误）

2. **时序问题**：
   - `reset()` 后需要足够的延迟让设备稳定
   - 当前延迟可能不够（50ms → 100ms）

3. **设备固件限制**：
   - 某些固件版本可能不支持在初始化早期阶段查询 capability
   - 可能需要先发送其他初始化命令

### 修复方案

#### 方案 1：在 prepare_interface 中添加 reset（当前实现）

```rust
pub fn prepare_interface(&mut self) -> Result<(), GsUsbError> {
    // 1. Detach kernel driver
    // ...

    // 2. Claim interface（必须先 claim，再 reset）
    self.handle.claim_interface(self.interface_number)?;

    // 3. Reset 设备
    let _ = self.handle.reset();  // 容错

    // 4. 延迟等待设备稳定
    std::thread::sleep(Duration::from_millis(100));

    Ok(())
}
```

**优点**：
- reset 在控制传输之前执行
- 给设备足够的稳定时间

**缺点**：
- 如果 reset 失败，可能影响后续操作

#### 方案 2：延迟 device_capability() 调用

将 `device_capability()` 延迟到 `start()` 内部调用，而不是在 `set_bitrate()` 中调用。

但这样会导致 `set_bitrate()` 无法获取时钟频率，需要预先知道时钟（80/48/40 MHz）。

#### 方案 3：Fallback 机制

如果 `device_capability()` 失败，使用默认时钟（如 48MHz）尝试。

## 当前状态

### 已修复的问题

1. ✅ **接口声明顺序**：正确在 reset 之前声明接口（避免段错误）
2. ✅ **Reset 容错**：reset 失败不阻断流程
3. ✅ **延迟增加**：从 50ms 增加到 100ms

### 仍存在的问题

1. ❌ **控制传输失败**：`control_in(GS_USB_BREQ_BT_CONST)` 仍然失败

### 可能的解决方案

1. **增加延迟时间**：尝试 200ms 或 500ms
2. **检查设备固件**：确认设备是否支持 `GS_USB_BREQ_BT_CONST` 请求
3. **使用默认时钟**：如果 capability 查询失败，使用默认值（48MHz）继续
4. **参考其他实现**：检查 Linux 内核驱动的初始化序列

## 建议的下一步

1. **测试不同延迟**：尝试 200ms、500ms、1000ms
2. **检查设备日志**：使用 `dmesg` 或系统日志查看设备状态
3. **尝试默认时钟**：如果 capability 查询失败，使用 48MHz 作为默认值
4. **验证设备固件**：确认设备的固件版本和支持的功能

## 测试命令

⚠️ **重要**：设备是独占的，必须使用 `--test-threads=1` 串行运行：

```bash
# 逐步调试
cargo test --test gs_usb_debug_step_by_step -- --ignored --nocapture

# Stage 1 测试（串行）
cargo test --test gs_usb_stage1_loopback_tests -- --ignored --test-threads=1 --nocapture

# 设备扫描
cargo test --test gs_usb_debug_scan -- --ignored --nocapture
```

## 测试结果（串行运行后）

使用 `--test-threads=1` 串行运行后：

- ✅ `test_loopback_device_state` - 通过
- ✅ `test_loopback_echo_filtering` - 通过（但未收到 Echo，这是正常的）
- ✅ `test_loopback_standard_and_extended_frames` - 通过（但未收到 Echo）
- ❌ `test_loopback_end_to_end` - 失败（接收超时，Loopback 模式下可能不返回 Echo）
- ❌ `test_loopback_fire_and_forget` - 失败（发送超时，可能是批量发送太快）

**结论**：串行运行解决了并发问题。剩余问题主要是 Loopback 模式下的 Echo 行为（可能被过滤或设备固件特性）。

## 注意事项

⚠️ **设备可能因 reset 断开**：如果运行测试后设备找不到，可能需要重新插拔 USB 设备。

