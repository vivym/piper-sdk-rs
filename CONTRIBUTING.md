# 贡献指南

感谢你对 Piper SDK 项目的关注！我们欢迎所有形式的贡献。

## 📋 目录

- [行为准则](#行为准则)
- [如何贡献](#如何贡献)
- [开发环境设置](#开发环境设置)
- [代码风格](#代码风格)
- [提交规范](#提交规范)
- [测试要求](#测试要求)
- [拉取请求流程](#拉取请求流程)

## 🤝 行为准则

参与本项目时，请遵循以下行为准则：

- 保持友好和尊重
- 接受建设性的批评
- 关注对社区最有利的事情
- 对其他社区成员表示同理心

## 💡 如何贡献

### 报告 Bug

如果你发现了一个 bug，请通过 GitHub Issues 报告。请在报告中包含：

- 清晰的 bug 描述
- 重现步骤
- 预期的行为
- 实际的行为
- 环境信息（操作系统、Rust 版本等）
- 如果可能，提供最小可重现示例

### 提出功能建议

我们欢迎新功能的建议！请通过 GitHub Issues 提出，并包含：

- 功能的详细描述
- 使用场景和动机
- 可能的实现方式（如果已有想法）
- 对现有 API 的影响（如果有）

### 提交代码

1. Fork 本仓库
2. 创建功能分支 (`git checkout -b feature/amazing-feature`)
3. 提交更改 (`git commit -m 'Add some amazing feature'`)
4. 推送到分支 (`git push origin feature/amazing-feature`)
5. 开启 Pull Request

## 🔧 开发环境设置

### 前置要求

- Rust 1.70.0 或更高版本
- Git
- （可选）硬件设备用于集成测试
- （Linux 可选）虚拟 CAN 接口 `vcan0` 用于 SocketCAN 测试

### 克隆和构建

```bash
# 克隆仓库
git clone https://github.com/vivym/piper-sdk-rs.git
cd piper-sdk-rs

# 构建项目
cargo build

# 运行测试
cargo test

# 运行所有测试（包括需要硬件的测试）
cargo test -- --test-threads=1
```

### Linux 环境设置（SocketCAN 测试）

如果要在 Linux 上运行 SocketCAN 相关测试，需要设置虚拟 CAN 接口：

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

**注意**：如果 `vcan0` 接口不存在，SocketCAN 测试会自动跳过，不会导致测试失败。这确保了测试可以在没有配置虚拟 CAN 接口的环境中正常运行。

### 文档生成

```bash
# 生成文档
cargo doc --open
```

## 📝 代码风格

### 格式化

我们使用 `rustfmt` 进行代码格式化。提交前请确保运行：

```bash
cargo fmt --all
```

### Linting

我们使用 `clippy` 进行代码检查。请确保代码通过：

```bash
cargo clippy --all-targets --all-features -- -D warnings
```

### 命名约定

- 函数和变量：`snake_case`
- 类型和模块：`PascalCase`
- 常量：`SCREAMING_SNAKE_CASE`
- 私有模块：`snake_case`

## 📋 提交规范

我们遵循 [Conventional Commits](https://www.conventionalcommits.org/) 规范：

- `feat`: 新功能
- `fix`: Bug 修复
- `docs`: 文档更改
- `style`: 代码格式（不影响代码逻辑）
- `refactor`: 代码重构
- `perf`: 性能优化
- `test`: 添加或修改测试
- `chore`: 构建过程或辅助工具的变动

示例：

```
feat(can): 添加 GS-USB 设备扫描功能
fix(pipeline): 修复状态更新中的竞态条件
docs(readme): 更新快速开始示例
```

## ✅ 测试要求

### 单元测试

所有新功能都应包含单元测试：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_feature() {
        // 测试代码
    }
}
```

### 集成测试

如果需要硬件测试，请在 `tests/` 目录下添加集成测试，并使用 `#[ignore]` 标记：

```rust
#[test]
#[ignore] // 需要硬件设备
fn test_hardware_feature() {
    // 硬件测试代码
}
```

运行硬件测试：

```bash
cargo test --test <test_name> -- --ignored --test-threads=1
```

### 测试覆盖率

尽量保持高测试覆盖率。新的公共 API 应该有相应的测试。

## 🔄 拉取请求流程

### 提交前检查清单

- [ ] 代码已经格式化 (`cargo fmt`)
- [ ] 代码通过 Clippy 检查 (`cargo clippy`)
- [ ] 所有测试通过 (`cargo test`)
- [ ] 添加了必要的文档注释
- [ ] 更新了 CHANGELOG.md（如果有重大变更）
- [ ] 提交信息遵循 Conventional Commits 规范

### PR 审查

- PR 将被审查，可能需要修改
- 请及时响应审查意见
- 保持 PR 的原子性（一次只做一件事）
- 如果 PR 较大，请考虑拆分成多个小 PR

### 合并标准

PR 将被合并当：

- 至少有一位维护者批准
- 所有 CI 检查通过
- 没有冲突
- 符合代码风格和测试要求

## 📚 文档

### 代码文档

所有公共 API 应包含文档注释：

```rust
/// 获取当前时刻的最新状态
///
/// # 返回值
///
/// 返回 `RobotState` 的原子快照，读取操作无锁。
///
/// # 示例
///
/// ```no_run
/// let robot = PiperBuilder::new().build()?;
/// let state = robot.get_state();
/// println!("关节位置: {:?}", state.joint_pos);
/// ```
pub fn get_state(&self) -> RobotState {
    // ...
}
```

### 示例代码

示例代码应放在 `examples/` 目录下，并确保可以编译运行。

## 🐛 调试

### 日志使用指南

我们使用 `tracing` 生态系统进行结构化日志记录。

#### 初始化日志

**示例和二进制程序**：必须在 `main()` 函数开头初始化日志：

```rust
use piper_sdk::prelude::*;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // ✅ 第一步：初始化日志
    init_logger!();

    // 然后才能看到日志输出
    let driver = PiperBuilder::new().interface("can0").build()?;

    Ok(())
}
```

#### 日志级别策略

| 级别 | 用途 | 性能开销 | 示例 |
|------|------|----------|------|
| `error` | 错误，影响功能 | 极低 | CAN 连接失败、设备无响应 |
| `warn` | 警告，不影响功能 | 极低 | 重连成功、使用默认值 |
| `info` | 重要状态变化 | 低 | 连接成功、模式切换、配置加载 |
| `debug` | 调试信息（单次） | 低 | 解析成功、命令发送、函数入口 |
| `trace` | 高频循环（200Hz） | **高** | 单帧接收、单帧发送 |

**关键原则**：
- ❌ **禁止**在 200Hz 循环中使用 `trace!`（性能杀手）
- ✅ 使用 `debug!` 记录单次事件（如函数入口）
- ✅ 使用 `info!` 记录重要状态变化

#### 性能敏感场景

**驱动层 200Hz RX/TX 线程**：默认级别应为 `warn`，避免日志开销：

```rust
// ❌ 错误：高频循环中的 trace
fn rx_thread(&self) {
    loop {
        let frame = self.rx.recv()?;
        tracing::trace!("Received frame: {:?}", frame); // 每秒 200 次日志！
    }
}

// ✅ 正确：仅在警告级别记录异常情况
fn rx_thread(&self) {
    loop {
        match self.rx.recv_timeout(Duration::from_millis(100)) {
            Ok(frame) => {
                // 不记录日志（性能敏感）
                self.process_frame(frame);
            },
            Err(Timeout) => {
                // 超时是正常情况，仅在调试时启用
                tracing::debug!("RX timeout (normal)");
            },
            Err(e) => {
                // 真正的错误才记录
                tracing::warn!("RX error: {}", e);
            },
        }
    }
}
```

#### 结构化日志（字段）

优先使用结构化字段而非字符串拼接：

```rust
// ❌ 避免：字符串拼接
tracing::info!("Connected to can0 with bitrate 1000000");

// ✅ 推荐：结构化字段
tracing::info!(
    interface = "can0",
    bitrate = 1_000_000,
    "Connected to CAN interface"
);
```

#### 使用 `#[instrument]`

**适用场景**：
- ✅ 短生命周期函数（如单次命令）
- ✅ 异步函数
- ✅ 需要追踪参数和返回值的函数

**⚠️ 危险陷阱**：不要在长生命周期循环上使用 `#[instrument]`：

```rust
// ❌ 危险：长生命周期循环
#[instrument(skip(self, rx))]
fn run_rx_thread(&self, rx: Receiver<Frame>) {
    loop { /* 永远运行 */ }
}

// ✅ 正确：在短生命周期函数上使用
fn run_rx_thread(&self, rx: Receiver<Frame>) {
    loop {
        let frame = rx.recv()?;
        self.process_single_frame(frame);  // ← 在这里用 #[instrument]
    }
}

#[instrument(skip(self), fields(frame_id = frame.id))]
fn process_single_frame(&self, frame: Frame) {
    // 短生命周期函数，适合用 #[instrument]
}
```

#### 运行时日志级别

```bash
# 默认级别
cargo run --example your_example

# 启用调试日志
RUST_LOG=debug cargo run --example your_example

# 启用特定模块的 trace 日志（谨慎使用！）
RUST_LOG=piper_sdk=trace cargo run --example your_example

# 多模块组合
RUST_LOG=piper_sdk=debug,piper_driver=warn cargo run --example your_example
```

#### 兼容旧 `log` crate

`init_logger!()` 宏会自动初始化 `tracing-log::LogTracer`，因此旧的 `log::info!` 等宏会自动桥接到 `tracing`：

```rust
// ✅ 旧代码仍然工作
use log::info;  // 仍然支持

fn main() {
    init_logger!();  // 自动启用 LogTracer
    info!("Hello from old log crate");  // 会正常输出
}
```

#### 高级用法（自定义配置）

如需文件输出、日志轮转等高级功能，请参考 `apps/daemon/src/main.rs` 的 `init_logging()` 函数实现。

## ❓ 问题？

如果对贡献流程有任何疑问，请：

1. 查看现有 Issues 和 PR
2. 开启新的 Discussion 或 Issue
3. 联系项目维护者

感谢你的贡献！🎉

