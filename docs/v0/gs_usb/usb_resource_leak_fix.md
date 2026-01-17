# USB 资源泄漏问题分析与修复报告

## 问题描述

在运行 `loopback_sanity_check.rs` 测试时，第一次运行成功，第二次及之后的运行都失败。

## 根本原因分析

通过查阅 rusb（Rust 的 libusb 绑定）文档和深入分析代码，找到了以下三个关键问题：

### 1. **USB 接口未释放（最严重）**

**问题代码（修复前）：**

```rust
// device.rs - start() 方法
pub fn start(&mut self, flags: u32) -> Result<()> {
    // ...

    // Claim the interface
    self.handle
        .claim_interface(0)
        .map_err(GsUsbError::ClaimInterface)?;

    // ... 启动设备逻辑
}

// device.rs - stop() 方法
pub fn stop(&mut self) -> Result<()> {
    let mode = DeviceMode::new(GS_CAN_MODE_RESET, 0);
    let _ = self.control_out(GS_USB_BREQ_MODE, 0, &mode.pack());
    self.started = false;
    Ok(())
    // ❌ 没有调用 release_interface(0)！
}
```

**后果：**
- 第一次运行时 `claim_interface(0)` 成功，但结束时没有释放
- 第二次运行时再次 `claim_interface(0)` 会失败，返回 `LIBUSB_ERROR_BUSY`
- 设备接口处于 "已占用" 状态，无法被重新使用

**修复：**
```rust
pub fn stop(&mut self) -> Result<()> {
    if !self.started {
        return Ok(());
    }

    let mode = DeviceMode::new(GS_CAN_MODE_RESET, 0);
    let _ = self.control_out(GS_USB_BREQ_MODE, 0, &mode.pack());

    // ✅ 释放接口（关键修复）
    let _ = self.handle.release_interface(0);

    // ✅ 重新附加内核驱动
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        let _ = self.handle.attach_kernel_driver(0);
    }

    self.started = false;
    Ok(())
}
```

### 2. **reset() 调用时机不当**

**问题代码（修复前）：**

```rust
pub fn start(&mut self, flags: u32) -> Result<()> {
    // Reset to support restart multiple times
    self.handle.reset()?;  // ❌ reset 在 claim_interface 之前

    // Detach kernel driver
    // ...

    // Claim the interface
    self.handle.claim_interface(0)?;
    // ...
}
```

**问题分析：**
- `reset()` 会断开所有 USB 连接和状态
- 在 reset 之后立即 claim interface，设备可能还未完全恢复
- 这可能导致不稳定的行为或连接失败

**修复：**
```rust
pub fn start(&mut self, flags: u32) -> Result<()> {
    // ✅ 如果已启动，先正确停止（释放资源）
    if self.started {
        self.stop()?;
    }

    // Detach kernel driver
    // ...

    // Claim the interface
    self.handle.claim_interface(0)?;
    // ...
}
```

**改进说明：**
- 移除了 `reset()` 调用
- 改为检查 `self.started` 状态，如果已启动则先调用 `stop()` 正确清理资源
- 这样更符合 libusb 的最佳实践

### 3. **内核驱动管理不完整**

**问题：**
- 在 Linux/macOS 上，`start()` 中会 `detach_kernel_driver(0)`
- 但 `stop()` 中没有对应的 `attach_kernel_driver(0)`
- 这会导致设备在测试结束后处于"无驱动"状态

**修复：**
```rust
pub fn stop(&mut self) -> Result<()> {
    // ...

    // ✅ 重新附加内核驱动（重要）
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        let _ = self.handle.attach_kernel_driver(0);
    }

    // ...
}
```

### 4. **Drop 实现注释更新**

虽然原来的 Drop 实现调用了 `stop()`，但由于 `stop()` 本身不完整，Drop 也无法正确清理资源。现在 `stop()` 已修复，Drop 能够正确工作。

```rust
impl Drop for GsUsb {
    fn drop(&mut self) {
        // ✅ 注释更新：stop() 现在会正确释放接口和重新附加内核驱动
        let _ = self.stop();
    }
}
```

## rusb/libusb 资源管理规则总结

根据 rusb 文档和 libusb 最佳实践：

1. **接口管理规则：**
   - `claim_interface(N)` 和 `release_interface(N)` 必须成对出现
   - 未释放的接口会阻止后续的 claim 操作
   - 即使在错误路径中，也必须确保释放

2. **内核驱动管理规则（Linux/macOS）：**
   - `detach_kernel_driver(N)` 和 `attach_kernel_driver(N)` 应该成对
   - detach 后不 attach 会让设备处于"无驱动"状态
   - 某些情况下系统无法自动恢复驱动

3. **DeviceHandle 生命周期：**
   - `DeviceHandle` 在 Drop 时会自动关闭底层 USB 句柄
   - 但 Drop 不会自动释放接口或重新附加驱动
   - 必须显式管理这些资源

4. **reset() 使用注意事项：**
   - `reset()` 会断开所有连接和状态
   - 应该在必要时谨慎使用（如设备挂起时）
   - 不应该作为常规的"重启"机制

## 测试验证

创建了 `test_repeated_runs.rs` 测试文件，包含两个测试：

1. **`test_repeated_runs_resource_cleanup`**
   - 连续 5 次完整的：扫描设备 → 启动 → 发送接收 → 停止 → drop 设备
   - 验证每次都能成功，确保资源正确释放

2. **`test_repeated_start_stop_same_handle`**
   - 在同一个设备句柄上多次 start/stop
   - 验证 start/stop 的正确性和可重复性

## 运行测试

```bash
# 运行原始的 sanity check 测试（应该能连续运行多次）
cd tmp/gs_usb_rs
cargo test --test loopback_sanity_check -- --ignored --nocapture

# 运行新的重复运行测试
cargo test --test test_repeated_runs -- --ignored --nocapture
```

## 预期结果

修复后，应该能够：
- ✅ 连续多次运行同一个测试而不失败
- ✅ 在同一个程序中多次 start/stop 设备
- ✅ 测试结束后设备状态正常，不需要物理重插拔
- ✅ 系统资源正确释放，没有泄漏

## 参考文档

- [rusb DeviceHandle 文档](https://docs.rs/rusb/latest/rusb/struct.DeviceHandle.html)
- [rusb UsbContext 文档](https://docs.rs/rusb/latest/rusb/trait.UsbContext.html)
- [libusb API 文档](https://libusb.sourceforge.io/api-1.0/)
- libusb 最佳实践：接口管理、内核驱动处理、资源清理

## 总结

这是一个典型的 **USB 资源泄漏问题**。关键在于：
1. USB 接口的 claim/release 必须严格成对
2. 内核驱动的 detach/attach 应该成对管理
3. 资源清理必须在所有代码路径（包括错误路径）中执行
4. 不要滥用 reset()，应该通过正确的 start/stop 来管理设备状态

修复后的代码遵循了 rusb/libusb 的最佳实践，确保资源正确释放，支持连续多次运行。

