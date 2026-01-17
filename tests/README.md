# GS-USB 测试说明

## 重要提示

⚠️ **GS-USB 设备是独占的**：同一时间只能有一个进程/线程访问设备。因此所有硬件测试**必须串行运行**。

## 运行硬件测试

### 方法 1：使用 `--test-threads=1`（推荐）

```bash
# Stage 1: Loopback 模式测试
cargo test --test gs_usb_stage1_loopback_tests -- --ignored --test-threads=1

# 集成测试
cargo test --test gs_usb_integration_tests -- --ignored --test-threads=1

# 性能测试
cargo test --test gs_usb_performance_tests -- --ignored --test-threads=1
```

### 方法 2：运行单个测试（自动串行）

```bash
# 运行单个测试（不会并发）
cargo test --test gs_usb_stage1_loopback_tests -- --ignored test_loopback_end_to_end
```

## 测试文件说明

| 测试文件 | 描述 | 是否需要硬件 |
|---------|------|-------------|
| `gs_usb_stage1_loopback_tests.rs` | Stage 1: Loopback 模式端到端测试 | ✅ 是 |
| `gs_usb_integration_tests.rs` | 集成测试（基本功能验证） | ✅ 是 |
| `gs_usb_performance_tests.rs` | 性能测试（1kHz 等） | ✅ 是 |
| `gs_usb_debug_scan.rs` | 设备扫描诊断工具 | ✅ 是 |
| `gs_usb_debug_step_by_step.rs` | 逐步初始化调试工具 | ✅ 是 |

## 单元测试

单元测试（不需要硬件）可以并发运行：

```bash
# 运行所有单元测试（并发，默认行为）
cargo test --lib
```

## 故障排除

如果测试失败并出现 "Access denied" 或 "Resource busy" 错误：

1. 确保使用 `--test-threads=1` 串行运行
2. 检查是否有其他程序占用设备
3. 尝试重新插拔 USB 设备
4. 查看详细错误：`cargo test --test <test_name> -- --ignored --nocapture --test-threads=1`

## 调试工具

### 设备扫描诊断
```bash
cargo test --test gs_usb_debug_scan -- --ignored --nocapture
```

### 逐步初始化调试
```bash
cargo test --test gs_usb_debug_step_by_step -- --ignored --nocapture
```

这些工具可以帮助诊断设备连接、权限和初始化问题。

