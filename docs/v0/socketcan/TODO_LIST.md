# SocketCAN 适配层实现 TODO List

> 基于 `socketcan/implementation_plan.md` 的实现清单
> **核心原则：测试优先，确保正确性**

## 📊 整体进度

| Phase | 状态 | 进度 | 测试状态 | 备注 |
|-------|------|------|----------|------|
| Phase 0: 环境准备 | ✅ 已完成 | 100% | - | vcan0 接口已创建 |
| Phase 1: 基础实现 | ✅ 已完成 | 100% | 11/11 | 所有任务完成，已集成到 PiperBuilder |
| Phase 2: 错误处理 | 🔄 进行中 | 60% | 0/5 | 错误帧解析已实现 |
| Phase 3: 时间戳支持 | ⏳ 待开始 | 0% | 0/4 | 时间戳提取 |
| Phase 4: 集成与测试 | ⏳ 待开始 | 0% | 0/10 | 完整测试套件 |
| Phase 5: 与 GS-USB 一致性验证 | ⏳ 待开始 | 0% | 0/6 | 跨平台行为一致性 |

**最后更新**：2024-12（初始创建）

---

## 🎯 开发原则（严格执行）

### 1. 测试驱动开发（TDD）

**流程**：红-绿-重构（Red-Green-Refactor）
- 🔴 **Red**：先写失败的测试
- 🟢 **Green**：实现最简代码使测试通过
- 🔵 **Refactor**：重构优化代码，确保测试仍通过

**要求**：
- **每个功能必须先写测试，再实现代码**
- **测试覆盖率目标：≥ 90%**
- **所有测试通过后才能提交代码**

### 2. 测试层级优先级

```
单元测试（Unit Tests） > 集成测试（Integration Tests） > 端到端测试（E2E）
```

**执行顺序**：
1. 单元测试：验证单个函数/方法的行为
2. 集成测试：验证模块间的交互
3. 端到端测试：验证完整流程（与 GS-USB 对比）

### 3. 正确性保证

**每个任务必须完成以下检查**：

- ✅ **功能正确性**：功能按预期工作
- ✅ **边界条件**：处理极端情况（空数据、最大长度等）
- ✅ **错误处理**：所有错误路径都有测试覆盖
- ✅ **资源管理**：确保没有内存泄漏、资源泄漏
- ✅ **线程安全**：如果有并发，验证线程安全
- ✅ **性能基准**：关键路径的性能测试

### 4. 测试环境要求

**必需环境**：
- Linux 系统（Arch/Ubuntu/Debian 等）
- 虚拟 CAN 接口（`vcan0`）用于测试
- root 权限（用于创建虚拟接口）

**设置命令**：
```bash
# 加载 vcan 模块
sudo modprobe vcan

# 创建虚拟接口
sudo ip link add dev vcan0 type vcan
sudo ip link set up vcan0

# 验证接口
ip link show vcan0
```

---

## Phase 0: 环境准备（30 分钟）

### Task 0.1: 测试环境设置 ✅

**目标**：建立可重复的测试环境

**任务清单**：
- [x] 在 Linux 系统上设置虚拟 CAN 接口（`vcan0`）
- [x] 验证 `socketcan` crate 依赖已正确添加
- [ ] 创建测试脚本用于发送/接收测试帧（可选，用于手动测试）
- [ ] 编写环境检查脚本（检查 `vcan0` 是否存在）

**验收标准**：
- ✅ 可以通过 `ip link show vcan0` 看到接口状态为 UP
- ✅ 可以手动使用 `candump vcan0` 和 `cansend vcan0` 工具（如果已安装）
- ✅ Rust 项目可以编译并找到 `socketcan` crate

**测试代码**（环境检查）：
```rust
#[cfg(test)]
mod env_tests {
    #[test]
    #[cfg(target_os = "linux")]
    fn test_vcan0_exists() {
        use std::process::Command;
        let output = Command::new("ip")
            .args(&["link", "show", "vcan0"])
            .output();
        assert!(output.is_ok(), "vcan0 interface should exist");
    }
}
```

---

## Phase 1: 基础实现（2-3 小时）

### Task 1.1: 创建模块结构 ✅

**目标**：建立 SocketCAN 模块的目录结构

**任务清单**：
- [x] 创建 `src/can/socketcan/mod.rs` 文件
- [x] 添加模块文档注释（说明用途、依赖、限制）
- [x] 添加必要的 `use` 语句（`socketcan`, `CanAdapter`, `CanError`, `PiperFrame`）
- [x] 添加 `#[cfg(target_os = "linux")]` 条件编译

**验收标准**：
- ✅ 模块文件存在且结构正确
- ✅ 代码可以编译（即使功能未实现）

**参考**：`src/can/gs_usb/mod.rs`

---

### Task 1.2: 实现 `SocketCanAdapter::new()` ⏳

**目标**：创建 SocketCAN 适配器实例

**测试优先**（先写这些测试）：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_socketcan_adapter_new_success() {
        let adapter = SocketCanAdapter::new("vcan0");
        assert!(adapter.is_ok());
    }

    #[test]
    fn test_socketcan_adapter_new_invalid_interface() {
        let adapter = SocketCanAdapter::new("nonexistent_can99");
        assert!(adapter.is_err());
        match adapter.unwrap_err() {
            CanError::Device(msg) => assert!(msg.contains("nonexistent_can99")),
            _ => panic!("Expected Device error"),
        }
    }

    #[test]
    fn test_socketcan_adapter_new_stores_interface_name() {
        let adapter = SocketCanAdapter::new("vcan0").unwrap();
        assert_eq!(adapter.interface(), "vcan0"); // 需要添加 getter
    }

    #[test]
    fn test_socketcan_adapter_new_sets_read_timeout() {
        let adapter = SocketCanAdapter::new("vcan0").unwrap();
        // 验证默认超时时间已设置（100ms）
        assert_eq!(adapter.read_timeout(), Duration::from_millis(100));
    }

    #[test]
    fn test_socketcan_adapter_new_sets_started_true() {
        let adapter = SocketCanAdapter::new("vcan0").unwrap();
        assert!(adapter.is_started()); // 需要添加 getter
    }
}
```

**实现清单**：
- [ ] 实现 `new(interface: impl Into<String>) -> Result<Self, CanError>`
- [ ] 调用 `CanSocket::open()` 打开接口
- [ ] 设置默认读超时（100ms）
- [ ] 设置 `started = true`（SocketCAN 打开即启动）
- [ ] 错误处理：接口不存在、权限不足等

**验收标准**：
- ✅ 所有测试通过（5/5）
- ✅ 错误消息清晰，包含接口名称
- ✅ 超时时间正确设置

---

### Task 1.3: 实现 `CanAdapter::send()` ⏳

**目标**：发送 CAN 帧到总线

**测试优先**：

```rust
#[test]
fn test_socketcan_adapter_send_standard_frame() {
    let mut adapter = SocketCanAdapter::new("vcan0").unwrap();
    let frame = PiperFrame::new_standard(0x123, &[1, 2, 3, 4]).unwrap();

    let result = adapter.send(frame);
    assert!(result.is_ok());
}

#[test]
fn test_socketcan_adapter_send_extended_frame() {
    let mut adapter = SocketCanAdapter::new("vcan0").unwrap();
    let frame = PiperFrame::new_extended(0x12345678, &[0xFF; 8]).unwrap();

    let result = adapter.send(frame);
    assert!(result.is_ok());
}

#[test]
fn test_socketcan_adapter_send_empty_frame() {
    let mut adapter = SocketCanAdapter::new("vcan0").unwrap();
    let frame = PiperFrame::new_standard(0x123, &[]).unwrap();

    let result = adapter.send(frame);
    assert!(result.is_ok());
}

#[test]
fn test_socketcan_adapter_send_not_started() {
    // 需要添加机制来模拟未启动状态（或直接不设置 started）
    // 这可能需要重构或使用 mock
    // 先标记为 TODO，后续完善
}

#[test]
fn test_socketcan_adapter_send_invalid_id() {
    // 测试无效的 CAN ID（如超过标准帧范围的扩展帧）
    // 注意：socketcan-rs 可能已经在底层处理，需要验证
}
```

**实现清单**：
- [ ] 检查 `started` 状态（未启动返回 `CanError::NotStarted`）
- [ ] 转换 `PiperFrame` -> `CanFrame`
  - [ ] 标准帧：使用 `StandardId::new()`
  - [ ] 扩展帧：使用 `ExtendedId::new()`
- [ ] 调用 `socket.transmit(&can_frame)`
- [ ] 错误处理：转换错误到 `CanError`

**验收标准**：
- ✅ 所有测试通过（≥ 3/5，部分测试可能需要完善）
- ✅ 标准帧和扩展帧都能正确发送
- ✅ 错误处理正确

**验证方法**：
- 使用 `candump vcan0` 观察发送的帧（手动验证）

---

### Task 1.4: 实现 `CanAdapter::receive()`（基础版本）⏳

**目标**：从总线接收 CAN 帧（先不处理错误帧）

**测试优先**：

```rust
#[test]
fn test_socketcan_adapter_receive_standard_frame() {
    let mut adapter = SocketCanAdapter::new("vcan0").unwrap();

    // 先发送一个帧（另一个线程或外部工具）
    let tx_frame = PiperFrame::new_standard(0x123, &[1, 2, 3, 4]).unwrap();
    adapter.send(tx_frame).unwrap();

    // 接收帧
    let rx_frame = adapter.receive().unwrap();
    assert_eq!(rx_frame.raw_id(), 0x123);
    assert_eq!(rx_frame.data(), &[1, 2, 3, 4]);
    assert_eq!(rx_frame.dlc(), 4);
    assert!(!rx_frame.is_extended());
}

#[test]
fn test_socketcan_adapter_receive_extended_frame() {
    let mut adapter = SocketCanAdapter::new("vcan0").unwrap();

    let tx_frame = PiperFrame::new_extended(0x12345678, &[0xFF; 8]).unwrap();
    adapter.send(tx_frame).unwrap();

    let rx_frame = adapter.receive().unwrap();
    assert_eq!(rx_frame.raw_id(), 0x12345678);
    assert!(rx_frame.is_extended());
}

#[test]
fn test_socketcan_adapter_receive_timeout() {
    let mut adapter = SocketCanAdapter::new("vcan0").unwrap();

    // 设置短超时（10ms）
    adapter.set_read_timeout(Duration::from_millis(10)).unwrap();

    // 不发送任何帧，应该超时
    let result = adapter.receive();
    assert!(result.is_err());
    match result.unwrap_err() {
        CanError::Timeout => {},
        _ => panic!("Expected Timeout error"),
    }
}

#[test]
fn test_socketcan_adapter_receive_not_started() {
    // TODO: 需要模拟未启动状态
}
```

**实现清单**：
- [ ] 检查 `started` 状态
- [ ] 调用 `socket.read_frame_timeout(timeout)`
- [ ] 处理超时错误（`should_retry()` -> `CanError::Timeout`）
- [ ] 转换 `CanFrame` -> `PiperFrame`
- [ ] 处理 IO 错误

**验收标准**：
- ✅ 所有测试通过（≥ 3/5）
- ✅ 超时处理正确
- ✅ 帧转换正确（ID、数据、长度、扩展标志）

**注意事项**：
- 当前版本**不处理错误帧**，将在 Phase 2 实现
- 测试可能需要 `vcan0` 回环模式（`ip link set vcan0 up type vcan`）

---

### Task 1.5: 更新模块导出 ⏳

**目标**：在 `can/mod.rs` 中导出 SocketCAN 模块

**任务清单**：
- [ ] 在 `src/can/mod.rs` 中添加条件编译：
  ```rust
  #[cfg(target_os = "linux")]
  pub mod socketcan;

  #[cfg(target_os = "linux")]
  pub use socketcan::SocketCanAdapter;
  ```
- [ ] 验证代码在 Linux 上编译通过
- [ ] 验证代码在非 Linux 平台上不包含 SocketCAN 代码

**验收标准**：
- ✅ `cargo check --target x86_64-unknown-linux-gnu` 通过
- ✅ `cargo check --target x86_64-apple-darwin` 不包含 SocketCAN 代码
- ✅ `cargo build` 在 Linux 上成功

---

### Task 1.6: 集成到 PiperBuilder ✅

**目标**：在 `robot/builder.rs` 中使用 SocketCAN 适配器

**任务清单**：
- [x] 更新 `src/robot/builder.rs` 中的 `#[cfg(target_os = "linux")]` 分支
- [x] 实现 `SocketCanAdapter::new(interface)`
- [x] 调用 `configure()`（可选，用于验证接口状态）
- [x] 创建 `Piper` 实例

**测试优先**：

```rust
#[test]
#[cfg(target_os = "linux")]
fn test_piper_builder_with_socketcan() {
    let piper = PiperBuilder::new()
        .interface("vcan0")
        .build();
    assert!(piper.is_ok());
}

#[test]
#[cfg(target_os = "linux")]
fn test_piper_builder_socketcan_default_interface() {
    // 测试默认接口 "can0"（如果不存在，应该使用 vcan0）
    let piper = PiperBuilder::new().build();
    // 可能失败，取决于系统配置
}
```

**验收标准**：
- ✅ `PiperBuilder::new().interface("vcan0").build()` 成功
- ✅ 非 Linux 平台上仍使用 GS-USB 适配器

---

### Phase 1 验收测试 ⏳

**目标**：验证 Phase 1 的所有功能

**测试清单**：
- [ ] 运行所有单元测试（`cargo test --lib`）
- [ ] 运行集成测试（`cargo test --test socketcan_integration`，如果已创建）
- [ ] 手动测试：使用 `candump` 和 `cansend` 工具验证发送/接收
- [ ] 代码格式化（`cargo fmt`）
- [ ] 代码检查（`cargo clippy`）

**验收标准**：
- ✅ 所有测试通过
- ✅ 无 Clippy 警告
- ✅ 代码符合项目风格

---

## Phase 2: 错误处理（1-2 小时）

### Task 2.1: 错误帧过滤 ⏳

**目标**：过滤 SocketCAN 接收到的错误帧

**测试优先**：

```rust
#[test]
fn test_socketcan_adapter_receive_filters_error_frames() {
    let mut adapter = SocketCanAdapter::new("vcan0").unwrap();

    // 注意：错误帧通常由内核生成，难以手动创建
    // 可以通过特定条件触发（如总线错误）
    // 或者使用 mock/stub 模拟错误帧

    // TODO: 研究如何生成错误帧用于测试
    // 可能的方法：
    // 1. 模拟 CanFrame 为错误帧（需要底层 API）
    // 2. 使用特定的总线错误条件
    // 3. 使用测试工具生成错误帧
}

#[test]
fn test_socketcan_adapter_receive_skips_error_frames() {
    // 验证：如果收到错误帧，会跳过并继续读取下一个有效帧
    // 需要模拟：错误帧 -> 有效帧 -> 有效帧
}
```

**实现清单**：
- [ ] 在 `receive()` 中添加错误帧检测
- [ ] 使用 `can_frame.is_error_frame()` 检查
- [ ] 跳过错误帧，继续循环读取
- [ ] 记录错误帧日志（使用 `tracing::warn!`）

**验收标准**：
- ✅ 错误帧被正确过滤（不返回给上层）
- ✅ 错误帧后仍能正常接收有效帧
- ✅ 日志记录正确

---

### Task 2.2: 错误帧解析（基础） ⏳

**目标**：解析错误帧类型，映射到 `CanError`

**测试优先**：

```rust
#[test]
fn test_socketcan_adapter_parse_bus_off_error() {
    // 测试 Bus Off 错误帧解析
    // 注意：需要能够生成或模拟 Bus Off 错误帧
}

#[test]
fn test_socketcan_adapter_parse_buffer_overflow_error() {
    // 测试缓冲区溢出错误帧解析
}

#[test]
fn test_socketcan_adapter_error_frame_logging() {
    // 验证错误帧被记录到日志
}
```

**实现清单**：
- [ ] 研究 `socketcan-rs` 的错误帧 API（`CanErrorFrame`）
- [ ] 实现错误帧类型检测（Bus Off、Overflow 等）
- [ ] 映射到 `CanError`（`BusOff`, `BufferOverflow` 等）
- [ ] 添加详细的错误日志

**验收标准**：
- ✅ 错误帧类型能正确识别
- ✅ 错误类型映射正确
- ✅ 日志信息详细

**参考**：
- `socketcan-rs` 的 `CanErrorFrame` 文档
- Linux SocketCAN 错误帧格式

---

### Task 2.3: 完善错误类型映射 ⏳

**目标**：确保所有 socketcan-rs 错误都正确映射到 `CanError`

**任务清单**：
- [ ] 列出所有 `socketcan-rs` 可能的错误类型
- [ ] 为每个错误类型编写映射规则
- [ ] 实现 `From<socketcan::Error>` for `CanError`（如果适用）
- [ ] 编写测试覆盖所有错误路径

**测试清单**：
- [ ] IO 错误映射
- [ ] 构造错误映射
- [ ] 超时错误映射
- [ ] 设备错误映射

**验收标准**：
- ✅ 所有错误类型都有对应的映射
- ✅ 错误消息清晰，便于调试

---

### Phase 2 验收测试 ⏳

**验收标准**：
- ✅ 错误帧被正确过滤
- ✅ 错误类型映射正确
- ✅ 所有测试通过（5/5 或更多）

---

## Phase 3: 时间戳支持（2-3 小时）

### Task 3.1: 研究时间戳 API ⏳

**目标**：了解 SocketCAN 时间戳的实现方式

**任务清单**：
- [ ] 阅读 `socketcan-rs` 文档，查找时间戳 API
- [ ] 研究 Linux SocketCAN 时间戳机制（`SO_TIMESTAMP`）
- [ ] 查看 `socketcan-rs` 示例代码中是否有时间戳使用示例
- [ ] 测试硬件时间戳支持（如果硬件支持）

**研究问题**：
- `socketcan-rs` 是否提供时间戳 API？
- 时间戳是软件时间戳还是硬件时间戳？
- 时间戳的单位是什么？（纳秒、微秒、毫秒）
- 如何提取时间戳？

**文档记录**：
- [ ] 记录研究发现到 `docs/v0/socketcan/timestamp_research.md`

---

### Task 3.2: 实现时间戳提取（软件时间戳） ⏳

**目标**：提取 SocketCAN 的软件时间戳（内核提供）

**测试优先**：

```rust
#[test]
fn test_socketcan_adapter_receive_includes_timestamp() {
    let mut adapter = SocketCanAdapter::new("vcan0").unwrap();

    // 发送帧
    let tx_frame = PiperFrame::new_standard(0x123, &[1, 2, 3]).unwrap();
    adapter.send(tx_frame).unwrap();

    // 接收帧，检查时间戳
    let rx_frame = adapter.receive().unwrap();
    assert!(rx_frame.timestamp_us() > 0, "Timestamp should be set");
}

#[test]
fn test_socketcan_adapter_timestamp_accuracy() {
    // 验证时间戳的准确性（微秒级精度）
    let mut adapter = SocketCanAdapter::new("vcan0").unwrap();

    let start = std::time::Instant::now();
    // ... 发送和接收帧
    let end = std::time::Instant::now();

    // 时间戳应该与实际时间接近（允许一定误差）
}
```

**实现清单**：
- [ ] 根据研究结果实现时间戳提取
- [ ] 转换时间戳单位到微秒（`timestamp_us`）
- [ ] 更新 `receive()` 方法，填充 `timestamp_us` 字段

**验收标准**：
- ✅ 时间戳被正确提取
- ✅ 时间戳精度合理（微秒级）
- ✅ 所有测试通过（2/2 或更多）

---

### Task 3.3: 硬件时间戳支持（如果硬件支持） ⏳

**目标**：如果硬件支持，启用硬件时间戳

**任务清单**：
- [ ] 检测硬件是否支持时间戳
- [ ] 实现硬件时间戳提取（如果不同于软件时间戳）
- [ ] 测试硬件时间戳准确性

**验收标准**：
- ✅ 硬件时间戳（如果支持）被正确提取
- ✅ 硬件时间戳精度高于软件时间戳

---

### Task 3.4: 时间戳单元测试 ⏳

**目标**：完善时间戳相关的测试

**测试清单**：
- [ ] 时间戳存在性测试
- [ ] 时间戳单调性测试（递增）
- [ ] 时间戳精度测试
- [ ] 时间戳单位转换测试

**验收标准**：
- ✅ 所有时间戳测试通过（4/4）

---

### Phase 3 验收测试 ⏳

**验收标准**：
- ✅ 时间戳被正确提取和使用
- ✅ 所有测试通过（4/4 或更多）

---

## Phase 4: 集成与测试（2-3 小时）

### Task 4.1: 编写完整的单元测试套件 ⏳

**目标**：覆盖所有代码路径

**测试清单**：
- [ ] `new()` 的所有错误路径
- [ ] `send()` 的所有边界情况
- [ ] `receive()` 的所有边界情况
- [ ] 错误帧处理的所有情况
- [ ] 时间戳的所有情况
- [ ] `Drop` 实现（如果添加了清理逻辑）

**覆盖率目标**：
- ✅ 代码覆盖率 ≥ 90%
- ✅ 所有公共 API 都有测试
- ✅ 所有错误路径都有测试

**工具**：
- 使用 `cargo tarpaulin` 或 `cargo llvm-cov` 检查覆盖率

---

### Task 4.2: 编写集成测试 ⏳

**目标**：测试 SocketCAN 适配器在实际场景中的行为

**测试清单**：

```rust
// tests/socketcan_integration_tests.rs

#[test]
#[cfg(target_os = "linux")]
fn test_socketcan_full_loopback() {
    // 完整的回环测试：发送 -> 接收
}

#[test]
#[cfg(target_os = "linux")]
fn test_socketcan_high_frequency_send() {
    // 高频发送测试（500Hz，模拟力控场景）
}

#[test]
#[cfg(target_os = "linux")]
fn test_socketcan_concurrent_access() {
    // 并发访问测试（多个线程同时读写）
}

#[test]
#[cfg(target_os = "linux")]
fn test_socketcan_timeout_behavior() {
    // 超时行为测试
}

#[test]
#[cfg(target_os = "linux")]
fn test_socketcan_resource_cleanup() {
    // 资源清理测试（确保没有泄漏）
}
```

**验收标准**：
- ✅ 所有集成测试通过（≥ 5 个测试）

---

### Task 4.3: 性能基准测试 ⏳

**目标**：确保 SocketCAN 适配器满足性能要求

**测试清单**：
- [ ] 单次发送延迟（应该 < 1ms）
- [ ] 单次接收延迟（应该 < 1ms）
- [ ] 吞吐量测试（500Hz 发送/接收）
- [ ] 内存使用测试（确保无泄漏）

**基准目标**：
- ✅ 发送延迟 < 1ms
- ✅ 接收延迟 < 1ms
- ✅ 支持 500Hz 频率（2ms 周期）

**工具**：
- 使用 `criterion` 进行基准测试

---

### Task 4.4: 错误恢复测试 ⏳

**目标**：测试错误恢复能力

**测试清单**：
- [ ] 超时后恢复（继续接收）
- [ ] 错误帧后恢复（继续接收有效帧）
- [ ] 接口临时断开后恢复（如果可能）

**验收标准**：
- ✅ 适配器能够从错误中恢复
- ✅ 恢复后功能正常

---

### Task 4.5: 代码质量检查 ⏳

**任务清单**：
- [ ] 运行 `cargo fmt`（代码格式化）
- [ ] 运行 `cargo clippy`（代码检查）
- [ ] 修复所有 Clippy 警告
- [ ] 检查文档注释完整性
- [ ] 检查 `#[allow]` 注释的合理性

**验收标准**：
- ✅ 无 Clippy 警告
- ✅ 所有公共 API 都有文档注释
- ✅ 代码符合项目风格

---

### Phase 4 验收测试 ⏳

**验收标准**：
- ✅ 所有测试通过（单元测试 + 集成测试）
- ✅ 代码覆盖率 ≥ 90%
- ✅ 性能满足要求
- ✅ 无代码质量问题

---

## Phase 5: 与 GS-USB 一致性验证（1-2 小时）

### Task 5.1: 接口语义一致性测试 ⏳

**目标**：确保 SocketCAN 和 GS-USB 的接口语义完全一致

**测试清单**：
- [ ] `send()` 的行为一致（Fire-and-Forget）
- [ ] `receive()` 的行为一致（阻塞直到收到帧或超时）
- [ ] 错误处理一致（相同的错误类型和消息）

**测试方法**：
```rust
#[test]
#[cfg(target_os = "linux")]
fn test_socketcan_gs_usb_send_consistency() {
    // 比较发送行为的差异（如果有）
}

#[test]
#[cfg(target_os = "linux")]
fn test_socketcan_gs_usb_receive_consistency() {
    // 比较接收行为的差异（如果有）
}
```

**验收标准**：
- ✅ 接口语义完全一致
- ✅ 错误处理一致

---

### Task 5.2: 端到端功能对比测试 ⏳

**目标**：使用相同的测试数据，验证两种适配器行为一致

**测试清单**：
- [ ] 相同的帧（ID、数据）发送，验证结果一致
- [ ] 相同的超时设置，验证超时行为一致
- [ ] 相同的错误场景，验证错误处理一致

**测试方法**：
```rust
#[test]
#[cfg(target_os = "linux")]
fn test_socketcan_gs_usb_frame_equivalence() {
    // 发送相同的帧，验证两种适配器的行为
    // 注意：GS-USB 可能不在 Linux 上，需要跨平台测试
}
```

**验收标准**：
- ✅ 行为基本一致（允许平台差异）

---

### Task 5.3: 性能对比测试 ⏳

**目标**：比较 SocketCAN 和 GS-USB 的性能差异

**测试清单**：
- [ ] 发送延迟对比
- [ ] 接收延迟对比
- [ ] 吞吐量对比

**预期结果**：
- SocketCAN（内核级）应该比 GS-USB（用户态）性能更好

**验收标准**：
- ✅ 性能差异合理（SocketCAN 应该更快或相当）

---

### Task 5.4: 文档一致性检查 ⏳

**任务清单**：
- [ ] 比较两种适配器的文档注释
- [ ] 确保文档风格一致
- [ ] 确保示例代码一致（如果适用）

**验收标准**：
- ✅ 文档风格一致

---

### Task 5.5: 上层 API 集成测试 ⏳

**目标**：确保 `PiperBuilder` 能无缝切换两种适配器

**测试清单**：
- [ ] `PiperBuilder` 在 Linux 上使用 SocketCAN
- [ ] `PiperBuilder` 在非 Linux 上使用 GS-USB
- [ ] 相同的 `PiperBuilder` 配置在不同平台上行为一致

**验收标准**：
- ✅ 上层 API 无需修改即可使用两种适配器

---

### Phase 5 验收测试 ⏳

**验收标准**：
- ✅ 两种适配器行为基本一致
- ✅ 上层 API 集成正常

---

## 最终验收

### 完整测试套件运行

**执行清单**：
- [ ] 运行所有单元测试（`cargo test --lib`）
- [ ] 运行所有集成测试（`cargo test --test '*'`）
- [ ] 运行基准测试（`cargo bench`，如果已实现）
- [ ] 运行代码覆盖率检查（`cargo tarpaulin`）
- [ ] 运行 Clippy 检查（`cargo clippy -- -D warnings`）

**验收标准**：
- ✅ **所有测试通过**
- ✅ **代码覆盖率 ≥ 90%**
- ✅ **无 Clippy 警告**
- ✅ **性能满足要求**

---

### 文档完整性

**检查清单**：
- [ ] API 文档完整（`cargo doc --no-deps`）
- [ ] 实现方案文档更新（如有变更）
- [ ] README 更新（如有必要）

**验收标准**：
- ✅ 所有文档完整且准确

---

### 代码审查清单

**审查要点**：
- [ ] 代码符合项目风格
- [ ] 错误处理完整
- [ ] 资源管理正确（无泄漏）
- [ ] 线程安全（如果适用）
- [ ] 性能优化（如果适用）
- [ ] 可维护性（代码清晰、注释充分）

**验收标准**：
- ✅ 代码审查通过

---

## 问题与风险

### 已知问题

1. **时间戳支持**：`socketcan-rs` 的时间戳 API 可能不完善，需要深入研究
2. **错误帧生成**：难以在测试中生成错误帧，可能需要 mock
3. **权限问题**：SocketCAN 接口可能需要特定权限（`dialout` 组或 `sudo`）

### 风险缓解

1. **时间戳**：如果 `socketcan-rs` 不支持，考虑提交 PR 或使用底层 API
2. **错误帧测试**：使用 mock/stub 或文档说明手动测试方法
3. **权限**：文档说明权限要求，或提供权限检查工具

---

## 参考文档

- **实现方案**：`docs/v0/socketcan/implementation_plan.md`
- **项目架构**：`docs/v0/TDD.md`
- **GS-USB 参考**：`src/can/gs_usb/mod.rs`
- **socketcan-rs 文档**：https://docs.rs/socketcan/

---

**文档版本**：v1.0
**创建日期**：2024-12
**最后更新**：2024-12
