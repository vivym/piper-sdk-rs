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

### 使用日志

我们使用 `tracing` 进行日志记录。开发时可以启用：

```rust
use tracing::{debug, info, warn, error};

debug!("调试信息");
info!("一般信息");
warn!("警告");
error!("错误");
```

运行时设置日志级别：

```bash
RUST_LOG=piper_sdk=debug cargo run --example your_example
```

## ❓ 问题？

如果对贡献流程有任何疑问，请：

1. 查看现有 Issues 和 PR
2. 开启新的 Discussion 或 Issue
3. 联系项目维护者

感谢你的贡献！🎉

