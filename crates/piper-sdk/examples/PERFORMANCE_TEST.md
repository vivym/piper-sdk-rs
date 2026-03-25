# Embedded Bridge Host 性能测试指南

## 概述

本指南介绍如何测试 daemon 性能指标。

## 测试前准备

### 1. 启动 Embedded Bridge Host

```bash
# Linux / Unix: 默认使用 UDS listener
cargo run -p piper-sdk --example embedded_bridge_host

# 非 Unix: 必须显式使用 TCP/TLS listener
cargo run -p piper-sdk --example embedded_bridge_host -- \
  --tcp-tls 127.0.0.1:18888 \
  --tls-server-cert server.pem \
  --tls-server-key server.key \
  --tls-client-ca ca.pem
```

确保 embedded bridge host 成功启动并连接到设备。

### 2. 运行性能测试

```bash
# Unix UDS
cargo run -p piper-sdk --example bridge_latency_bench

# TCP/TLS
cargo run -p piper-sdk --example bridge_latency_bench -- \
  --endpoint 127.0.0.1:18888 \
  --tls-ca ca.pem \
  --tls-client-cert client.pem \
  --tls-client-key client.key \
  --tls-server-name bridge.local
```

## 测试场景

### 场景 1: 发送延迟测试

测试从客户端发送帧到 embedded bridge host 的延迟。

**预期结果**:
- P50 延迟: < 100μs
- P99 延迟: < 200μs
- P999 延迟: < 500μs

### 场景 2: 接收延迟测试

测试从 embedded bridge host 接收帧的延迟（需要外部数据源）。

**注意**: 如果没有外部 CAN 数据源，此测试会超时。

### 场景 3: 吞吐量测试

测试 daemon 的最大吞吐量（fps）。

**预期结果**:
- 吞吐量: > 1000 fps

### 场景 4: 客户端阻塞处理

测试故障客户端是否会被正确断开。

**手动测试步骤**:
1. 启动 embedded bridge host
2. 运行性能测试
3. 在另一个终端运行一个"卡死"客户端（连接但不读取数据）
4. 观察 bridge host 日志，验证：
   - 日志限频生效（不会洪水）
   - 1 秒后客户端被断开
   - 其他客户端不受影响

## 性能基准参考值

| 指标 | 目标值 | 说明 |
|-----|-------|------|
| P50 延迟 | < 100μs | 中位数延迟 |
| P99 延迟 | < 200μs | 99% 分位延迟（关键指标）|
| P999 延迟 | < 500μs | 99.9% 分位延迟 |
| 吞吐量 | > 1000 fps | 每秒帧数 |
| CPU 占用 | < 30% | 单核占用率 |

## 故障排查

### 问题 1: 无法连接到 daemon

**错误**: `无法连接到 bridge host: ...`

**解决方案**:
1. 确认 embedded bridge host 已启动: `ps aux | grep embedded_bridge_host`
2. Unix UDS 路径下检查 socket 文件: `ls -l /tmp/piper_bridge.sock`
3. TCP/TLS 路径下确认 `--tcp-tls` 地址和证书参数匹配
4. 确认 daemon 日志显示设备已连接

### 问题 2: 接收测试超时

**错误**: `超时：只收到 X 帧`

**解决方案**:
- 这是正常的，如果没有外部 CAN 数据源
- 可以跳过接收测试，只关注发送延迟和吞吐量

### 问题 3: 延迟超标

**现象**: P99 延迟 > 200μs

**可能原因**:
1. 系统负载过高
2. USB 设备问题
3. macOS 调度问题

**解决方案**:
1. 关闭其他应用
2. 检查 USB 连接
3. 使用 `sudo` 运行（macOS 可能需要）

## 高级测试

### Round-trip 延迟测试（需要 Loopback 模式）

如果需要测试完整的 round-trip 延迟，需要：
1. 配置设备为 Loopback 模式
2. 发送帧并接收回显
3. 测量完整延迟

### 多客户端并发测试

可以运行多个测试实例来测试并发性能：

```bash
# 终端 2
cargo run -p piper-sdk --example bridge_latency_bench

# 终端 3
cargo run -p piper-sdk --example bridge_latency_bench

# 终端 4
cargo run -p piper-sdk --example bridge_latency_bench
```

非 Unix 平台请为每个实例显式传 `--endpoint` 和完整 TLS 参数。

观察 daemon 日志，验证：
- 所有客户端正常工作
- 没有性能退化
- 故障客户端被正确隔离

## 结果分析

### 成功标准

✅ **所有测试通过**:
- P99 延迟 < 200μs
- 吞吐量 > 1000 fps
- 客户端阻塞被正确处理

### 性能改进对比

| 指标 | 改进前 | 改进后 | 提升 |
|-----|-------|--------|------|
| 最坏延迟 | 200ms | 2ms | 100x |
| P99 延迟 | 250-800μs | 50-200μs | 5x |
| 客户端清理 | 5s | < 1ms | 5000x |
| RX/TX 竞争 | 存在 | 零竞争 | ∞ |

## 参考资料

- 架构分析报告: `docs/v0/gs_usb_daemon_architecture_analysis.md`
- 实施计划: `docs/v0/gs_usb_daemon_implementation_plan.md`
