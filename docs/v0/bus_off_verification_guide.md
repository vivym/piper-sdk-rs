# Bus Off 检测设备支持验证指南

> 本指南用于验证 GS-USB 设备是否支持 Bus Off 状态检测，以及设备如何报告 Bus Off 状态。

## 验证步骤

### 步骤 1：在守护进程中启用详细 Flags 日志

我已经在代码中添加了临时的 flags 日志功能。当守护进程运行时，它会打印所有非零 flags 的帧信息。

**启动守护进程并观察日志**：

```bash
# 编译并运行守护进程
cargo build --release --bin gs_usb_daemon
./target/release/gs_usb_daemon

# 观察输出，查找类似以下格式的日志：
# [Bus Off Verification] Frame flags: 0xXX, CAN ID: 0xXXXXXXXX
```

### 步骤 2：触发 Bus Off 状态（需要配合）

为了验证设备支持，需要实际触发 Bus Off 状态。以下是几种触发方法：

#### 方法 A：波特率不匹配（推荐，最简单）

1. **准备两个 CAN 设备**：
   - 设备 A：配置为 125000 bps
   - 设备 B：配置为 500000 bps

2. **连接两个设备到同一个 CAN 总线**

3. **让设备 A 发送大量数据**，由于波特率不匹配，设备 B 会收到大量错误帧，最终进入 Bus Off 状态

#### 方法 B：物理层短路（需要谨慎）

1. **将 CAN_H 和 CAN_L 短接**（会触发大量错误）
2. **观察设备是否会进入 Bus Off 状态**

#### 方法 C：CAN 错误帧注入（如果设备支持）

某些高级设备支持错误帧注入测试。

### 步骤 3：观察 Flags 变化

在触发 Bus Off 状态后，观察守护进程的日志输出：

**期望看到的情况（State Flags 支持）**：
```
[Bus Off Verification] Frame flags: 0x02, CAN ID: 0xXXXXXXXX  # Bus Off 标志位
[Bus Off Verification] Frame flags: 0x03, CAN ID: 0xXXXXXXXX  # Bus Off + Overflow
```

**如果看到（错误帧支持）**：
```
[Bus Off Verification] Error frame detected: CAN ID: 0x20000000 (CAN_ERR_FLAG set)
[Bus Off Verification] Error type: 0x40 (CAN_ERR_CRTL_TX_BUS_OFF)
```

**如果看不到任何 flags 变化（可能需要控制传输查询）**：
```
[Bus Off Verification] No flags detected, may need control transfer query
```

## 配合检查清单

- [ ] 确保 GS-USB 设备已连接
- [ ] 启动守护进程并观察日志输出
- [ ] 准备触发 Bus Off 的方法（波特率不匹配或物理层短路）
- [ ] 触发 Bus Off 状态
- [ ] 记录观察到的 flags 值
- [ ] 记录是否有错误帧出现
- [ ] 记录设备恢复过程

## 验证结果记录

请将以下信息记录并反馈：

1. **设备型号/固件版本**：
   ```
   VID:PID=XXXX:XXXX
   固件版本：XXXX
   ```

2. **正常运行时 flags 值**：
   ```
   通常为 0x00（无标志）
   或偶尔出现 0x01（OVERFLOW）
   ```

3. **Bus Off 状态时的 flags 值**：
   ```
   观察到的 flags：0xXX
   ```

4. **是否出现错误帧**：
   ```
   [ ] 是，CAN ID 包含 CAN_ERR_FLAG (0x20000000)
   [ ] 否，没有观察到错误帧
   ```

5. **错误帧详细信息（如果有）**：
   ```
   错误类型字段值：0xXX
   是否包含 CAN_ERR_CRTL_TX_BUS_OFF (0x40) 或 CAN_ERR_CRTL_RX_BUS_OFF (0x80)？
   ```

## 下一步

根据验证结果：
- ✅ **如果观察到 State Flags（flags 字段非零）** → 实施方案 A（State Flags 检测）
- ✅ **如果观察到错误帧** → 实施方案 B（错误帧解析）
- ⚠️ **如果没有观察到任何标志** → 实施方案 C（控制传输查询）或方案 D（设备不支持警告）

