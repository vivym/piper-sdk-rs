# 测试说明

本文档说明如何运行项目中的各种测试。

## GS-USB 测试

### 重要提示

⚠️ **GS-USB 设备是独占的**：同一时间只能有一个进程/线程访问设备。因此所有硬件测试**必须串行运行**。

## 运行硬件测试

### 方法 1：使用 `--test-threads=1`（推荐）

```bash
# Loopback 模式测试
cargo test --test gs_usb_stage1_loopback_tests -- --ignored --test-threads=1

# 集成测试
cargo test --test gs_usb_integration_tests -- --ignored --test-threads=1

# 性能测试
cargo test --test gs_usb_performance_tests -- --ignored --test-threads=1
```

### 一键运行实时验收

仓库提供了统一脚本：

```bash
# SocketCAN StrictRealtime 验收（默认 can0）
./scripts/run_realtime_acceptance.sh socketcan-strict

# 指定 SocketCAN 接口
PIPER_TEST_SOCKETCAN_IFACE=vcan0 ./scripts/run_realtime_acceptance.sh socketcan-strict

# GS-USB SoftRealtime 验收
./scripts/run_realtime_acceptance.sh gs-usb-soft

# 顺序跑完整套
./scripts/run_realtime_acceptance.sh all
```

脚本覆盖两类验收：
- `socketcan-strict`: SocketCAN 超时配置 + StrictRealtime benchmark
- `gs-usb-soft`: GS-USB 超时配置 + GS-USB 性能测试

脚本默认会把每一步的日志、环境快照和汇总报告写到：

```bash
artifacts/realtime_acceptance/<timestamp>/
```

其中包括：
- `summary.md`: 验收步骤汇总
- `environment.txt`: 运行环境快照
- `git-status.txt`: 工作区状态
- `git-diff-stat.txt`: 当前代码变更摘要
- `logs/*.log`: 每一步的完整日志

常用环境变量：

```bash
# 自定义产物目录
PIPER_ACCEPTANCE_OUT_DIR=/tmp/piper-acceptance ./scripts/run_realtime_acceptance.sh all

# 失败后继续跑完剩余步骤，并把所有失败都记录下来
PIPER_ACCEPTANCE_CONTINUE_ON_FAILURE=1 ./scripts/run_realtime_acceptance.sh all
```

失败时脚本会自动追加诊断：
- `socketcan-strict`: 记录 `ip -details link show <iface>`
- `gs-usb-soft`: 运行 `gs_usb_debug_scan`

### 方法 2：运行单个测试（自动串行）

```bash
# 运行单个测试（不会并发）
cargo test --test gs_usb_stage1_loopback_tests -- --ignored test_loopback_end_to_end
```

## 测试文件说明

| 测试文件 | 描述 | 是否需要硬件 |
|---------|------|-------------|
| `gs_usb_stage1_loopback_tests.rs` | Loopback 模式端到端测试 | ✅ 是 |
| `gs_usb_integration_tests.rs` | 集成测试（基本功能验证） | ✅ 是 |
| `gs_usb_performance_tests.rs` | 性能测试（1kHz 等） | ✅ 是 |
| `gs_usb_debug_scan.rs` | 设备扫描诊断工具 | ✅ 是 |

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
当前仓库没有单独的 `gs_usb_debug_step_by_step` 测试文件。
如需现场诊断，优先使用：

```bash
cargo test --test gs_usb_debug_scan -- --ignored --nocapture
```

这些工具可以帮助诊断设备连接、权限和初始化问题。

---

## SocketCAN 测试

### 重要提示

✅ **SocketCAN 测试会自动处理接口缺失**：如果 `vcan0` 接口不存在，测试会自动跳过，不会导致测试失败。

### 设置虚拟 CAN 接口（可选）

如果要在 Linux 上运行 SocketCAN 相关测试，可以设置虚拟 CAN 接口：

```bash
# 加载 vcan 内核模块
sudo modprobe vcan

# 创建虚拟 CAN 接口
sudo ip link add dev vcan0 type vcan

# 启动接口
sudo ip link set up vcan0

# 验证接口
ip link show vcan0
```

### 运行 SocketCAN 测试

SocketCAN 测试是单元测试的一部分，会自动运行：

```bash
# 运行所有单元测试（包括 SocketCAN 测试）
cargo test --lib

# 如果 vcan0 不存在，相关测试会自动跳过
# 如果 vcan0 存在，所有测试都会正常运行
```

### CI/CD 环境

在 GitHub Actions 中，`vcan0` 接口会自动设置，确保所有测试都能正常运行。

### 测试文件说明

| 测试位置 | 描述 | 是否需要 vcan0 |
|---------|------|---------------|
| `src/can/socketcan/mod.rs` | SocketCAN 适配器单元测试 | ⚠️ 可选（会自动跳过） |

**注意**：即使没有 `vcan0` 接口，测试也会通过（相关测试会被跳过）。这确保了测试可以在任何环境中运行。

---
