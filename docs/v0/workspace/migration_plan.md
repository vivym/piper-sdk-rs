# Piper SDK Workspace 迁移计划

**日期**: 2026-01-25
**目标版本**: v0.1.0
**预计工期**: 7-9 天
**迁移分支**: `workspace-refactor`

---

## 迁移概览

本文档提供详细的、逐步的迁移指南，将 piper-sdk-rs 从单体库重构为 Cargo workspace。

### 迁移目标

- ✅ **零破坏**: 现有代码无需修改即可继续工作
- ✅ **测试覆盖**: 每个阶段都保持 100% 测试通过
- ✅ **渐进式**: 可以在任何阶段停止或回滚
- ✅ **可验证**: 每个阶段都有明确的验收标准

### 迁移策略

1. **新分支策略**: 在 `workspace-refactor` 分支上进行所有工作
2. **阶段化迁移**: 分 9 个阶段，每阶段独立可验证
3. **向后兼容**: 通过 `piper-sdk` 聚合库维护旧 API
4. **持续测试**: 每阶段结束后运行完整测试套件

---

## 阶段 0: 准备工作

### 0.1 创建迁移分支

```bash
# 从最新的 main 分支创建
git checkout main
git pull origin main
git checkout -b workspace-refactor

# 推送到远程
git push -u origin workspace-refactor
```

### 0.2 基线测试

```bash
# 记录当前编译时间
time cargo build --release

# 运行所有测试
cargo test --all-targets --all-features

# 记录测试结果
echo "561 tests passed" > migration_baseline.txt
```

### 0.3 创建目录结构

```bash
# 创建 crates 和 apps 目录
mkdir -p crates
mkdir -p apps
mkdir -p tools

# 创建占位符文件（让 git 追踪目录）
touch crates/.gitkeep
touch apps/.gitkeep
touch tools/.gitkeep

git add crates apps tools
git commit -m "feat: prepare workspace directory structure"
```

### 0.4 验收标准

- [ ] 分支创建成功
- [ ] 基线测试通过 (561/561)
- [ ] 目录结构创建完成

---

## 阶段 1: 设置 Workspace Root

### 1.1 修改根 Cargo.toml

**修改前** (`Cargo.toml`):
```toml
[package]
name = "piper-sdk"
version = "0.0.2"
edition = "2021"

[dependencies]
# ... 所有依赖
```

**修改后** (`Cargo.toml`):
```toml
[workspace]
members = [
    "crates/piper-protocol",
    "crates/piper-can",
    "crates/piper-driver",
    "crates/piper-client",
    "crates/piper-sdk",
    "apps/daemon",
]

[workspace.package]
version = "0.1.0"
edition = "2021"
rust-version = "1.75"
authors = ["Piper SDK Contributors"]
license = "MIT"
repository = "https://github.com/your-org/piper-sdk"

[workspace.dependencies]
# 协议层
bilge = "0.4"
num_enum = "0.5"
thiserror = "1.0"

# 并发和异步
crossbeam-channel = "0.5"
tokio = { version = "1.0", features = ["full"] }

# 序列化
serde = { version = "1.0", features = ["derive"] }

# 日志
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }

# CAN 硬件
rusb = "0.9"
socketcan = "2.0"

# 平台特定
[target.'cfg(target_os = "linux")'.dependencies]
socketcan = "2.0"

[target.'cfg(not(target_os = "linux"))'.dependencies]
rusb = "0.9"
```

### 1.2 验收标准

- [ ] `cargo check` 不报错
- [ ] `cargo test` 通过所有测试
- [ ] `cargo build --release` 成功

### 1.3 预期问题

**问题**: Cargo 可能警告 workspace 中没有成员的包

**解决**: 这是正常的，我们会在后续阶段添加成员

---

## 阶段 2: 拆分协议层 (piper-protocol)

### 2.1 创建 crate

```bash
mkdir -p crates/piper-protocol/src
touch crates/piper-protocol/src/lib.rs
```

### 2.2 创建 Cargo.toml

**文件**: `crates/piper-protocol/Cargo.toml`
```toml
[package]
name = "piper-protocol"
version.workspace = true
edition.workspace = true

[dependencies]
bilge = { workspace = true }
num_enum = { workspace = true }
thiserror = { workspace = true }
```

### 2.3 移动代码

```bash
# 移动协议层代码
mv src/protocol crates/piper-protocol/src/

# 验证文件结构
ls crates/piper-protocol/src/protocol/
# 应该看到: mod.rs, ids.rs, feedback.rs, control.rs, config.rs
```

### 2.4 更新 lib.rs

**文件**: `crates/piper-protocol/src/lib.rs`
```rust
//! # Piper Protocol
//!
//! 机械臂 CAN 总线协议定义（无硬件依赖）
//!
//! ## 模块
//!
//! - `ids`: CAN ID 常量定义
//! - `feedback`: 反馈帧解析
//! - `control`: 控制帧构建
//! - `config`: 配置帧处理

pub mod ids;
pub mod feedback;
pub mod control;
pub mod config;

// 重新导出常用类型
pub use ids::*;
pub use feedback::*;
pub use control::*;
pub use config::*;
```

### 2.5 验收标准

- [ ] `cargo check -p piper-protocol` 成功
- [ ] `cargo test -p piper-protocol` 通过协议层测试
- [ ] `cargo build -p piper-protocol` 成功

### 2.6 预期测试结果

```
running 262 tests
test protocol::tests::... ... ok
test result: ok. 262 passed; 0 failed
```

---

## 阶段 3: 拆分 CAN 层 (piper-can)

### 3.1 创建 crate

```bash
mkdir -p crates/piper-can/src
touch crates/piper-can/src/lib.rs
```

### 3.2 创建 Cargo.toml

**文件**: `crates/piper-can/Cargo.toml`
```toml
[package]
name = "piper-can"
version.workspace = true
edition.workspace = true

[dependencies]
piper-protocol = { workspace = true }
thiserror = { workspace = true }
tracing = { workspace = true }

# 平台特定依赖
[target.'cfg(target_os = "linux")'.dependencies]
socketcan = { workspace = true }

[target.'cfg(not(target_os = "linux"))'.dependencies]
rusb = { workspace = true }
```

### 3.3 移动代码

```bash
# 移动 CAN 层代码
mv src/can crates/piper-can/src/

# 验证
ls crates/piper-can/src/can/
# 应该看到: mod.rs, adapter.rs, frame.rs, socketcan/, gs_usb/, gs_usb_udp/
```

### 3.4 更新导入

**文件**: `crates/piper-can/src/can/mod.rs`
```rust
// 修改前
use crate::protocol::ids::*;

// 修改后
use piper_protocol::ids::*;
```

### 3.5 验收标准

- [ ] `cargo check -p piper-can` 成功
- [ ] `cargo test -p piper-can` 通过 CAN 层测试
- [ ] `cargo build -p piper-can` 成功

---

## 阶段 4: 拆分驱动层 (piper-driver)

### 4.1 创建 crate

```bash
mkdir -p crates/piper-driver/src
touch crates/piper-driver/src/lib.rs
```

### 4.2 创建 Cargo.toml

**文件**: `crates/piper-driver/Cargo.toml`
```toml
[package]
name = "piper-driver"
version.workspace = true
edition.workspace = true

[dependencies]
piper-protocol = { workspace = true }
piper-can = { workspace = true }
crossbeam-channel = { workspace = true }
thiserror = { workspace = true }
tracing = { workspace = true }
tracing-subscriber = { workspace = true }
```

### 4.3 移动代码

```bash
# 移动驱动层代码
mv src/driver crates/piper-driver/src/

# 验证
ls crates/piper-driver/src/driver/
# 应该看到: mod.rs, piper.rs, pipeline.rs, state.rs, builder.rs,
#              command/, heartbeat.rs, metrics.rs
```

### 4.4 更新导入

**需要修改的关键文件**:
- `piper.rs`
- `pipeline.rs`
- `state.rs`
- `command/mod.rs`

**示例修改** (`piper.rs`):
```rust
// 修改前
use crate::can::{CanAdapter, PiperFrame};
use crate::protocol::feedback::*;
use crate::driver::state::*;

// 修改后
use piper_can::{CanAdapter, PiperFrame};
use piper_protocol::feedback::*;
use piper_driver::state::*;
```

### 4.5 验收标准

- [ ] `cargo check -p piper-driver` 成功
- [ ] `cargo test -p piper-driver` 通过驱动层测试
- [ ] 集成测试通过

---

## 阶段 5: 拆分客户端层 (piper-client)

### 5.1 创建 crate

```bash
mkdir -p crates/piper-client/src
touch crates/piper-client/src/lib.rs
```

### 5.2 创建 Cargo.toml

**文件**: `crates/piper-client/Cargo.toml`
```toml
[package]
name = "piper-client"
version.workspace = true
edition.workspace = true

[dependencies]
piper-protocol = { workspace = true }
piper-driver = { workspace = true }
thiserror = { workspace = true }
tracing = { workspace = true }
```

### 5.3 移动代码

```bash
# 移动客户端层代码
mv src/client crates/piper-client/src/

# 验证
ls crates/piper-client/src/client/
# 应该看到: mod.rs, builder.rs, motion.rs, observer.rs,
#              state/, control/, types/, heartbeat.rs
```

### 5.4 更新导入

**关键修改点**:
- `builder.rs`
- `motion.rs`
- `observer.rs`

**示例** (`builder.rs`):
```rust
// 修改前
use crate::driver::{Piper, PiperBuilder as DriverBuilder};
use crate::protocol::*;

// 修改后
use piper_driver::{Piper, PiperBuilder as DriverBuilder};
use piper_protocol::*;
```

### 5.5 验收标准

- [ ] `cargo check -p piper-client` 成功
- [ ] `cargo test -p piper-client` 通过客户端层测试
- [ ] 高级集成测试通过

---

## 阶段 6: 创建兼容层 (piper-sdk)

### 6.1 创建 crate

```bash
mkdir -p crates/piper-sdk/src
touch crates/piper-sdk/src/lib.rs
```

### 6.2 创建 Cargo.toml

**文件**: `crates/piper-sdk/Cargo.toml`
```toml
[package]
name = "piper-sdk"
version.workspace = true
edition.workspace = true

[dependencies]
# 重新导出所有其他 crates
piper-protocol = { workspace = true }
piper-can = { workspace = true }
piper-driver = { workspace = true }
piper-client = { workspace = true }

# 为了完整性，包含所有外部依赖
bilge = { workspace = true }
num_enum = { workspace = true }
thiserror = { workspace = true }
crossbeam-channel = { workspace = true }
tokio = { workspace = true }
tracing = { workspace = true }
```

### 6.3 创建 lib.rs (重新导出)

**文件**: `crates/piper-sdk/src/lib.rs`
```rust
//! # Piper SDK - 机械臂控制 Rust SDK
//!
//! 这是 Piper SDK 的主入口点，重新导出了所有子模块的公共 API。
//!
//! ## 快速开始
//!
//! ```rust,no_run
//! use piper_sdk::prelude::*;
//!
//! #[tokio::main]
//! async fn main() -> Result<()> {
//!     let piper = PiperBuilder::new().build()?;
//!     // ...
//! }
//! ```
//!
//! ## 模块结构
//!
//! - [`client`] - 高级类型安全 API（推荐大多数用户使用）
//! - [`driver`] - 驱动层 API（需要低级控制的用户）
//! - [`can`] - CAN 硬件抽象层
//! - [`protocol`] - CAN 总线协议定义
//!
//! ## 模块化使用
//!
//! 如果你只需要特定功能，可以直接依赖子 crate：
//!
//! - `piper-protocol` - 仅协议定义（最小依赖）
//! - `piper-can` - 协议 + CAN 抽象
//! - `piper-driver` - 协议 + CAN + 驱动层
//! - `piper-client` - 完整高级 API
//! - `piper-sdk` - 全部（便利包）

// 重新导出协议层
pub use piper_protocol::*;

// 重新导出 CAN 层
pub use piper_can::*;

// 重新导出驱动层
pub use piper_driver::*;

// 重新导出客户端层
pub use piper_client::*;

// 重新导出 prelude
pub use piper_client::prelude;
```

### 6.4 移动原 lib.rs 内容

```bash
# 将原来的 lib.rs 内容移动到 prelude.rs
cp src/lib.rs crates/piper-sdk/src/prelude.rs

# 更新 prelude.rs 的导入
# 需要将所有 crate::xxx 替换为 piper_xxx
```

### 6.5 验收标准

- [ ] `cargo check -p piper-sdk` 成功
- [ ] 现有示例无需修改即可编译
- [ ] 所有测试通过

---

## 阶段 7: 迁移二进制

### 7.1 移动守护进程

```bash
# 移动到 apps 目录
mv src/bin/gs_usb_daemon apps/daemon

# 创建新的 Cargo.toml
touch apps/daemon/Cargo.toml
```

### 7.2 更新守护进程的 Cargo.toml

**文件**: `apps/daemon/Cargo.toml`
```toml
[package]
name = "gs_usb_daemon"
version.workspace = true
edition.workspace = true

[[bin]]
name = "gs_usb_daemon"
path = "src/main.rs"

[dependencies]
piper-driver = { workspace = true }
piper-protocol = { workspace = true }
piper-can = { workspace = true }
tracing = { workspace = true }
tracing-subscriber = { workspace = true }
```

### 7.3 更新 workspace members

**修改** 根目录的 `Cargo.toml`:
```toml
[workspace]
members = [
    "crates/piper-protocol",
    "crates/piper-can",
    "crates/piper-driver",
    "crates/piper-client",
    "crates/piper-sdk",
    "apps/daemon",  # ← 新增
]
```

### 7.4 验收标准

- [ ] `cargo build --bin gs_usb_daemon` 成功
- [ ] `cargo run --bin gs_usb_daemon -- --help` 正常工作
- [ ] 守护进程测试通过

---

## 阶段 8: 更新示例和测试

### 8.1 更新所有示例的导入

虽然 `piper-sdk` 提供了向后兼容，但我们应该更新示例使用新的 crate 结构。

**脚本化批量更新**:
```bash
# 查找所有需要更新的示例
find examples -name "*.rs" -exec grep -l "use piper_sdk" {} \;

# 可选：更新为使用 piper-client
# sed -i '' 's/use piper_sdk::/use piper_client::/g' examples/*.rs
```

**注意**: 为了向后兼容，示例可以保持使用 `piper-sdk`

### 8.2 更新集成测试

**检查文件**:
- `tests/high_level_integration_v2.rs`
- `tests/robot_integration_tests.rs`
- `tests/high_level_phase1_integration.rs`

**验证**:
```bash
cargo test --test high_level_integration_v2
cargo test --test robot_integration_tests
cargo test --test high_level_phase1_integration
```

### 8.3 验收标准

- [ ] 所有示例编译通过
- [ ] 所有集成测试通过
- [ ] `cargo test --all-targets` 全部通过

---

## 阶段 9: 文档和发布

### 9.1 更新 README.md

**添加 Workspace 部分**:
```markdown
## Workspace 结构

本项目使用 Cargo workspace 管理，包含以下 crates:

- **piper-protocol**: CAN 总线协议定义（无硬件依赖）
- **piper-can**: CAN 硬件抽象层
- **piper-driver**: IO 线程和状态同步
- **piper-client**: 高级类型安全 API
- **piper-sdk**: 便利聚合包（向后兼容）

### 依赖方式

#### 方式 1: 使用聚合包（推荐新手）
```toml
[dependencies]
piper-sdk = "0.1"
```

#### 方式 2: 使用特定 crate（推荐高级用户）
```toml
[dependencies]
piper-client = "0.1"
```

详细文档请参阅 [docs/v0/workspace/](docs/v0/workspace/)
```

### 9.2 创建迁移指南

**文件**: `docs/v0/workspace/migration_guide.md`

内容应包括：
- 从旧版本迁移的步骤
- 常见问题和解决方案
- 性能对比数据

### 9.3 发布 v0.1.0

```bash
# 更新版本号
# (在 workspace.package 中设置 version = "0.1.0")

# 发布所有 crate
cargo release -p piper-protocol --no-dev
cargo release -p piper-can --no-dev
cargo release -p piper-driver --no-dev
cargo release -p piper-client --no-dev
cargo release -p piper-sdk --no-dev

# 或者使用 cargo-publish-workspace 工具
cargo publish-workspace
```

### 9.4 合并到主分支

```bash
# 确保所有检查通过
cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features

# 合并到 main
git checkout main
git merge workspace-refactor
git push origin main
```

---

## 验收清单

### 代码质量

- [ ] `cargo fmt --all` 无格式差异
- [ ] `cargo clippy --all-targets --all-features -- -D warnings` 无警告
- [ ] `cargo test --all-targets --all-features` 561/561 测试通过

### 性能基准

- [ ] 冷启动编译时间 < 50s
- [ ] 增量编译（修改协议层）< 25s
- [ ] 增量编译（修改客户端）< 20s

### 文档完整性

- [ ] README.md 更新完成
- [ ] 迁移指南文档完成
- [ ] 所有 public API 有 rustdoc 注释
- [ ] `cargo doc --no-deps` 无警告

### 兼容性

- [ ] 旧代码（使用 `piper-sdk`）无需修改即可编译
- [ ] 所有示例继续工作
- [ ] 集成测试全部通过

---

## 回滚计划

如果迁移过程中遇到无法解决的问题，可以回滚：

```bash
# 保存当前工作
git stash

# 回到 main 分支
git checkout main

# 删除 workspace 分支
git branch -D workspace-refactor
git push origin --delete workspace-refactor
```

---

## 时间估算

| 阶段 | 任务 | 预计时间 | 实际时间 | 状态 |
|------|------|----------|----------|------|
| 0 | 准备工作 | 1h | | 待开始 |
| 1 | Workspace Root | 1h | | 待开始 |
| 2 | 协议层 | 3h | | 待开始 |
| 3 | CAN 层 | 3h | | 待开始 |
| 4 | 驱动层 | 4h | | 待开始 |
| 5 | 客户端层 | 4h | | 待开始 |
| 6 | 兼容层 | 2h | | 待开始 |
| 7 | 二进制 | 1h | | 待开始 |
| 8 | 示例和测试 | 2h | | 待开始 |
| 9 | 文档和发布 | 4h | | 待开始 |
| **总计** | | **25h (3天)** | | |

---

## 附录 A: 常见问题

### Q1: 编译时出现 "cannot find crate X"

**A**: 确保 `Cargo.toml` 中的 `[workspace]` members 包含该 crate。

### Q2: 测试失败，提示 "undefined symbol"

**A**: 检查导入路径是否从 `crate::xxx` 更新为 `piper_xxx`。

### Q3: 如何在本地测试 workspace？

**A**:
```bash
# 检查所有 crate
cargo check --workspace

# 测试所有 crate
cargo test --workspace

# 构建 release 版本
cargo build --release --workspace
```

### Q4: CI/CD 需要修改吗？

**A**: 是的，需要更新 CI 配置以支持 workspace：
```yaml
# .github/workflows/test.yml
- name: Run tests
  run: cargo test --workspace --all-targets

- name: Run clippy
  run: cargo clippy --workspace --all-targets -- -D warnings
```

---

## 附录 B: 有用的 Git 命令

```bash
# 查看 workspace 中所有 crate
cargo tree -i piper-sdk --workspace

# 检查某个 crate 的依赖
cargo tree -p piper-protocol

# 验证版本一致性
cargo workspaces --version

# 清理所有构建产物
cargo clean --workspace

# 发布所有 crate
cargo publish --workspace
```
