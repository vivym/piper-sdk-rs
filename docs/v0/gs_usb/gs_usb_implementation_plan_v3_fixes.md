# GS-USB 实现方案 v3 对比检查与修正

## 发现的问题

### 1. ❌ 缺少 `GS_CAN_MODE_TRIPLE_SAMPLE` 模式标志

**参考实现**：
```rust
pub const GS_CAN_MODE_TRIPLE_SAMPLE: u32 = 1 << 2;
```

**我们的方案**：缺少此常量（虽然 Piper 可能不需要，但为完整性应保留）

**修正**：在 `protocol.rs` 中添加：
```rust
pub const GS_CAN_MODE_TRIPLE_SAMPLE: u32 = 1 << 2;
```

### 2. ❌ `send_host_format()` 的 `wValue` 参数

**参考实现**：
```rust
pub fn send_host_format(&mut self) -> Result<()> {
    let host_format: [u8; 4] = 0x0000_BEEFu32.to_le_bytes();
    // Ignore errors - this may fail on some devices that don't support it
    let _ = self.control_out(GS_USB_BREQ_HOST_FORMAT, 0, &host_format);
    Ok(())
}
```
注意：`value` 参数是 **0**，不是 1！

**我们的方案错误**：使用了 `value = 1`

**修正**：`send_host_format()` 应该使用 `value = 0`：
```rust
fn send_host_format(&self) -> Result<(), GsUsbError> {
    let val: u32 = 0x0000_BEEF;
    let data = val.to_le_bytes();

    let _ = self.handle.write_control(
        0x41,
        GS_USB_BREQ_HOST_FORMAT,
        0,    // ✅ 修正：应该是 0，不是 1
        self.interface_number as u16,
        &data,
        Duration::from_millis(100),
    );

    Ok(())
}
```

### 3. ⚠️ `start()` 方法的初始化流程细节

**参考实现的 `start()` 流程**：
```rust
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

    // 5. 过滤 flags（只保留设备支持的功能）
    let mut flags = flags & capability.feature;

    // 6. 设置模式并启动
    let mode = DeviceMode::new(GS_CAN_MODE_START, flags);
    self.control_out(GS_USB_BREQ_MODE, 0, &mode.pack())?;

    self.started = true;
    Ok(())
}
```

**关键点**：
- `start()` 中会调用 `reset()`、`detach_kernel_driver()`、`claim_interface()`
- 这意味着设备在 `scan()` 时只是打开，接口的 claim 在 `start()` 中进行

**我们的方案问题**：文档中没有明确说明 `start()` 的完整流程

**修正建议**：在 `device.rs` 的实现说明中，明确 `start()` 的完整步骤

### 4. ✅ 帧格式 - Header 结构确认正确

**参考实现**：
```rust
// Header: echo_id (4) + can_id (4) + can_dlc (1) + channel (1) + flags (1) + reserved (1)
// 总计：12 字节 header + 8 字节 data = 20 字节（无时间戳）
```

**我们的方案**：正确 ✅

### 5. ✅ 端点地址确认正确

**参考实现**：
```rust
pub const GS_USB_ENDPOINT_OUT: u8 = 0x02;
pub const GS_USB_ENDPOINT_IN: u8 = 0x81;
```

**我们的方案**：正确 ✅

### 6. ✅ 控制传输请求类型确认正确

**参考实现**：
```rust
// OUT: 0x41 (Host to Device | Vendor | Interface)
// IN:  0xC1 (Device to Host | Vendor | Interface)
```

**我们的方案**：正确 ✅

### 7. ⚠️ `scan()` 方法的设备匹配

**参考实现**中的设备匹配逻辑：
```rust
fn is_gs_usb_device(vendor_id: u16, product_id: u16) -> bool {
    matches!(
        (vendor_id, product_id),
        (GS_USB_ID_VENDOR, GS_USB_ID_PRODUCT)              // 0x1D50, 0x606F
            | (GS_USB_CANDLELIGHT_VENDOR_ID, GS_USB_CANDLELIGHT_PRODUCT_ID)  // 0x1209, 0x2323
            | (GS_USB_CES_CANEXT_FD_VENDOR_ID, GS_USB_CES_CANEXT_FD_PRODUCT_ID)
            | (GS_USB_ABE_CANDEBUGGER_FD_VENDOR_ID, GS_USB_ABE_CANDEBUGGER_FD_PRODUCT_ID)
    )
}
```

**我们的方案**：文档中没有明确说明设备匹配逻辑

**修正建议**：在 `device.rs` 实现中添加设备 VID/PID 匹配

### 8. ⚠️ `DeviceInfo` 结构体的字段顺序

**参考实现**：
```rust
pub struct DeviceInfo {
    pub reserved1: u8,     // byte 0
    pub reserved2: u8,     // byte 1
    pub reserved3: u8,     // byte 2
    pub icount: u8,        // byte 3
    pub fw_version: u32,   // byte 4-7
    pub hw_version: u32,   // byte 8-11
}
```

**我们的方案**：文档中有定义，但需要确认 `unpack()` 的字节顺序正确

### 9. ⚠️ 控制传输的 `wIndex` 参数

**参考实现**：
```rust
fn control_out(&self, request: u8, value: u16, data: &[u8]) -> Result<()> {
    self.handle.write_control(
        0x41,
        request,
        value,
        0, // wIndex - 注意：大多数请求使用 0
        data,
        Duration::from_millis(1000),
    )?;
}
```

**我们的方案**：文档中使用了 `self.interface_number`，需要确认是否正确

**修正**：对于大多数控制请求，`wIndex` 应该是 `0`。只有特定请求（如 `GET_STATE`）可能使用 channel number 作为 `wIndex`。

---

## 修正后的关键代码片段

### `protocol.rs` - 添加缺失的常量

```rust
// Mode Flags
pub const GS_CAN_MODE_NORMAL: u32 = 0;
pub const GS_CAN_MODE_LISTEN_ONLY: u32 = 1 << 0;
pub const GS_CAN_MODE_LOOP_BACK: u32 = 1 << 1;
pub const GS_CAN_MODE_TRIPLE_SAMPLE: u32 = 1 << 2;  // ✅ 添加
pub const GS_CAN_MODE_ONE_SHOT: u32 = 1 << 3;
```

### `device.rs` - 修正 `send_host_format()`

```rust
fn send_host_format(&self) -> Result<(), GsUsbError> {
    let val: u32 = 0x0000_BEEF;
    let data = val.to_le_bytes();

    let _ = self.handle.write_control(
        0x41,
        GS_USB_BREQ_HOST_FORMAT,
        0,    // ✅ 修正：value 应该是 0，不是 1
        0,    // wIndex - 大多数请求使用 0
        &data,
        Duration::from_millis(100),
    );

    Ok(())
}
```

### `device.rs` - 设备扫描和匹配

```rust
fn is_gs_usb_device(vendor_id: u16, product_id: u16) -> bool {
    matches!(
        (vendor_id, product_id),
        (0x1D50, 0x606F)   // GS-USB
            | (0x1209, 0x2323)  // Candlelight
            | (0x1CD2, 0x606F)  // CES CANext FD
            | (0x16D0, 0x10B8)  // ABE CANdebugger FD
    )
}

pub fn scan() -> Result<Vec<GsUsbDevice>, GsUsbError> {
    let mut devices = Vec::new();

    for device in rusb::devices()?.iter() {
        let desc = device.device_descriptor()?;
        if Self::is_gs_usb_device(desc.vendor_id(), desc.product_id()) {
            // 打开设备...
        }
    }

    Ok(devices)
}
```

---

## 总结

### 必须修正的问题：
1. ✅ `send_host_format()` 的 `wValue` 应该是 `0`，不是 `1`
2. ✅ 添加 `GS_CAN_MODE_TRIPLE_SAMPLE` 常量（为完整性）

### 需要确认的细节：
1. ⚠️ `start()` 方法的完整流程（reset、detach、claim）
2. ⚠️ 设备 VID/PID 匹配逻辑
3. ⚠️ 控制传输的 `wIndex` 参数（大多数为 0）

### 已确认正确的部分：
1. ✅ 端点地址（0x02 OUT, 0x81 IN）
2. ✅ 控制传输请求类型（0x41 OUT, 0xC1 IN）
3. ✅ 帧格式 Header 结构（12 字节）
4. ✅ 帧大小常量（20 字节无时间戳）

