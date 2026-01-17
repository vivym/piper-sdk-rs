# GS-USB 测试故障排除指南

## 问题 1: "Access denied (insufficient permissions)"

**症状**：
```
Failed to start device: USB error: Access denied (insufficient permissions)
```

**原因**：macOS 需要明确授予应用程序访问 USB 设备的权限。

**解决方案**：

### 方法 1: 系统设置授予权限（推荐）
1. 打开 **系统设置** (System Settings)
2. 进入 **隐私与安全性** (Privacy & Security)
3. 选择 **USB 设备** (USB)
4. 确保 **终端** (Terminal) 或你使用的 IDE 被添加到允许列表
5. 如果没有看到终端，可能需要：
   - 先运行一次程序，触发权限请求
   - 在系统设置中手动添加应用

### 方法 2: 使用 sudo（不推荐，仅用于测试）
```bash
sudo cargo test --test gs_usb_stage1_loopback_tests -- --ignored
```
⚠️ **注意**：不推荐在生产环境中使用 sudo。

### 方法 3: 将用户添加到 `_usb` 组（Linux）
```bash
sudo usermod -aG dialout $USER  # 某些发行版
sudo usermod -aG plugdev $USER  # 其他发行版
```
需要重新登录才能生效。

---

## 问题 2: "No GS-USB device found"

**症状**：
```
Failed to create adapter: Device("No GS-USB device found")
```

**可能原因**：
1. 设备未连接
2. 设备 VID/PID 不在支持列表中
3. 设备被其他程序占用

**诊断步骤**：

### 步骤 1: 运行调试扫描工具
```bash
cargo test --test gs_usb_debug_scan -- --ignored --nocapture
```

这会列出所有 USB 设备，并标记出 GS-USB 兼容设备。

### 步骤 2: 检查设备 VID/PID
如果设备未被识别，检查其 VID/PID：
```bash
# macOS
system_profiler SPUSBDataType | grep -A 10 -i "can\|usb"

# Linux
lsusb
```

### 步骤 3: 添加自定义 VID/PID（如果设备不在支持列表中）
编辑 `src/can/gs_usb/device.rs` 中的 `is_gs_usb_device()` 函数。

---

## 问题 3: "Entity not found"

**症状**：
```
Failed to start device: USB error: Entity not found
```

**可能原因**：
1. USB 端点配置错误
2. 接口未正确声明
3. 设备固件问题

**解决方案**：
1. 检查设备是否正确连接
2. 尝试重新插拔 USB 设备
3. 运行调试扫描工具确认端点配置

---

## 问题 4: Loopback 模式下接收超时

**症状**：
```
✓ Frame sent: ID=0x123
  Attempt 1: Timeout (retrying...)
  ...
Failed to receive echo frame in loopback mode
```

**可能原因**：
1. Loopback 模式下，设备固件可能不返回 Echo（某些固件行为）
2. Echo 被过滤逻辑过滤掉了（`echo_id == GS_USB_ECHO_ID`）
3. 需要更长的等待时间

**诊断**：

### 检查 Echo 过滤逻辑
在 Loopback 模式下，发送的帧的 `echo_id` 通常是 `GS_USB_ECHO_ID`（0x00000000）。
如果设备返回的 Echo 也带有这个 ID，我们的过滤逻辑会将其过滤掉。

**临时解决方案**（用于调试）：
1. 检查 `receive()` 方法中的 Echo 过滤逻辑
2. 在 Loopback 模式下，可能需要调整过滤策略
3. 或者使用不同的模式标志组合

**长期解决方案**：
- 根据设备的实际行为调整过滤逻辑
- 或者为 Loopback 模式实现特殊的处理

---

## 问题 5: 设备被其他程序占用

**症状**：
```
Failed to claim interface: USB error: Resource busy
```

**解决方案**：
1. 关闭可能占用设备的其他程序（如其他 CAN 工具、串口终端等）
2. 检查是否有内核驱动占用设备：
   ```bash
   # Linux
   lsmod | grep can
   sudo modprobe -r gs_usb  # 如果已加载
   ```

---

## 通用诊断命令

### 1. 运行设备扫描诊断
```bash
cargo test --test gs_usb_debug_scan -- --ignored --nocapture
```

### 2. 检查 USB 设备列表
```bash
# macOS
system_profiler SPUSBDataType

# Linux
lsusb -v | grep -A 20 -i "gs-usb\|candlelight"
```

### 3. 使用 RUST_LOG 查看详细日志
```bash
RUST_LOG=trace cargo test --test gs_usb_stage1_loopback_tests -- --ignored --nocapture
```

### 4. 检查系统日志
```bash
# macOS
log show --predicate 'eventMessage contains "USB"' --last 5m

# Linux
dmesg | tail -50
journalctl -k -f  # 实时查看内核日志
```

---

## 测试建议

### 分阶段测试
1. **第一阶段**：运行设备扫描诊断，确认设备被识别
2. **第二阶段**：解决权限问题（如果需要）
3. **第三阶段**：运行单个简单测试，确认基本功能
4. **第四阶段**：运行完整测试套件

### 推荐的测试顺序
```bash
# 1. 诊断设备
cargo test --test gs_usb_debug_scan -- --ignored --nocapture

# 2. 运行单个简单测试
cargo test --test gs_usb_stage1_loopback_tests -- --ignored test_loopback_device_state --nocapture

# 3. 如果成功，运行端到端测试
cargo test --test gs_usb_stage1_loopback_tests -- --ignored test_loopback_end_to_end --nocapture

# 4. 运行所有测试
cargo test --test gs_usb_stage1_loopback_tests -- --ignored
```

---

## 参考资源

- GS-USB 实现方案：`docs/v0/gs_usb_implementation_plan_v3.md`
- 安全分析报告：`docs/v0/gs_usb_safety_analysis.md`
- Linux GS-USB 内核驱动：`drivers/net/can/usb/gs_usb.c`

