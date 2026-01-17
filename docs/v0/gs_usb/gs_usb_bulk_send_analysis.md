# GS-USB 批量发送问题深入分析

## 问题描述

1. **`test_loopback_fire_and_forget` 测试失败**：
   - 批量发送 100 帧时，在第 28 次左右出现超时
   - 错误：`USB error: Operation timed out`

2. **设备需要重新插拔**：
   - 测试后设备无法继续使用
   - 后续操作失败，需要物理重新插拔 USB 设备

## 根本原因分析

### 1. USB Bulk Transfer 超时机制

当 USB Bulk 传输超时发生时：

```rust
// src/can/gs_usb/device.rs
pub fn send_raw(&self, frame: &GsUsbFrame) -> Result<(), GsUsbError> {
    self.handle
        .write_bulk(self.endpoint_out, &buf, Duration::from_millis(1000))
        .map_err(GsUsbError::Usb)?;
    Ok(())
}
```

**问题**：
- `write_bulk()` 超时后，USB endpoint 可能进入 **STALL 状态**
- STALL 是 USB 协议中的错误状态，表示设备无法处理请求
- 一旦 endpoint stall，后续的传输都会失败，直到清除 stall 状态

### 2. 为什么会导致设备需要重新插拔？

**USB Endpoint Stall 状态**：
- 当批量传输超时时，设备固件可能将 endpoint 设置为 STALL
- STALL 状态会阻止所有后续的传输
- **清除 STALL 的唯一方法**：
  1. 调用 `clear_halt()` 清除 endpoint halt（推荐）
  2. 重置整个设备（`reset()`）
  3. 物理重新插拔（最极端的方法）

**当前代码问题**：
- `send_raw()` 在超时后直接返回错误
- **没有清除 endpoint halt**
- 导致设备处于 STALL 状态，后续操作失败
- 最终导致需要物理重新插拔

### 3. USB 缓冲区限制

**缓冲区满的原因**：
- USB 设备的 OUT endpoint 有有限的缓冲区
- 如果发送速度超过设备处理速度，缓冲区会满
- 当缓冲区满时，`write_bulk()` 会阻塞，直到：
  - 缓冲区有空间（设备处理完数据）
  - 或者超时（1 秒）

**为什么添加延迟有帮助**：
- 延迟给设备时间处理缓冲区的数据
- 避免缓冲区满，从而避免超时

## 解决方案

### 方案 1: 超时后清除 Endpoint Halt（推荐）✅

在 `send_raw()` 中检测超时，并清除 endpoint halt：

```rust
pub fn send_raw(&self, frame: &GsUsbFrame) -> Result<(), GsUsbError> {
    let mut buf = bytes::BytesMut::new();
    frame.pack_to(&mut buf);

    match self.handle.write_bulk(self.endpoint_out, &buf, Duration::from_millis(1000)) {
        Ok(_) => Ok(()),
        Err(rusb::Error::Timeout) => {
            // 超时后清除 endpoint halt，恢复设备状态
            if let Err(clear_err) = self.handle.clear_halt(self.endpoint_out) {
                error!("Failed to clear halt after timeout: {}", clear_err);
            }
            Err(GsUsbError::WriteTimeout)
        }
        Err(e) => Err(GsUsbError::Usb(e)),
    }
}
```

**优点**：
- 自动恢复设备状态，无需重新插拔
- 符合 USB 协议标准做法
- 用户友好的错误处理

**缺点**：
- 如果设备状态严重错误，可能仍然需要重置

### 方案 2: 发送前检查并清除 Halt

在每次发送前检查并清除 halt（更激进的方法）：

```rust
pub fn send_raw(&self, frame: &GsUsbFrame) -> Result<(), GsUsbError> {
    // 可选：清除 halt 状态（某些设备可能需要）
    let _ = self.handle.clear_halt(self.endpoint_out);

    // ... 发送逻辑
}
```

**优点**：
- 预防性处理，避免 STALL 状态累积

**缺点**：
- 可能影响性能（每次发送都清除）
- 对于正常工作的设备是多余的

### 方案 3: 结合延迟策略和 Halt 清除

**最佳实践**：
1. 添加适当的发送延迟（避免缓冲区满）
2. 超时后自动清除 halt（恢复设备状态）
3. 如果清除 halt 后仍然失败，尝试重置设备

```rust
pub fn send_raw(&self, frame: &GsUsbFrame) -> Result<(), GsUsbError> {
    let mut buf = bytes::BytesMut::new();
    frame.pack_to(&mut buf);

    match self.handle.write_bulk(self.endpoint_out, &buf, Duration::from_millis(1000)) {
        Ok(_) => Ok(()),
        Err(rusb::Error::Timeout) => {
            // 1. 清除 endpoint halt
            if let Err(clear_err) = self.handle.clear_halt(self.endpoint_out) {
                warn!("Failed to clear halt after timeout: {}", clear_err);
            }

            // 2. 可选：短暂延迟，让设备恢复
            std::thread::sleep(Duration::from_millis(10));

            Err(GsUsbError::WriteTimeout)
        }
        Err(e) => Err(GsUsbError::Usb(e)),
    }
}
```

## 测试建议

### 1. 验证清除 Halt 的效果

在 `test_loopback_fire_and_forget` 中：
- 如果发送超时，应该能继续发送（因为自动清除了 halt）
- 设备不应该需要重新插拔

### 2. 测试不同的发送策略

- 无延迟 + 清除 halt
- 小延迟 + 清除 halt
- 大延迟 + 清除 halt

找到最佳平衡点。

## 结论

**核心问题**：USB 批量传输超时后，endpoint 进入 STALL 状态，没有清除，导致设备无法继续使用。

**解决方案**：在超时后自动清除 endpoint halt，恢复设备状态。

**实现优先级**：
1. ✅ 在 `send_raw()` 中添加清除 halt 逻辑（**必须**）
2. ✅ 在测试中调整发送延迟（优化性能）
3. ✅ 添加错误日志，便于调试

这将解决设备需要重新插拔的问题。

