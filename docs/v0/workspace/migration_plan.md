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
2. **阶段化迁移**: 分 10 个阶段，每阶段独立可验证
3. **向后兼容**: 通过 `piper-sdk` 聚合库维护旧 API
4. **持续测试**: 每阶段结束后运行完整测试套件
5. **⚠️ Git 历史保护**: **必须使用 `git mv` 而不是 `mv`**，否则会丢失 `git blame` 历史记录

### 🚨 关键原则

#### 原则 1: 永远使用 `git mv`
**为什么**: `mv` 命令会让 Git 认为文件是"删除+新建"，导致历史记录断层
**正确做法**:
```bash
# ❌ 错误 - 会丢失历史
mv src/protocol crates/piper-protocol/src/

# ✅ 正确 - 保留历史
git mv src/protocol crates/piper-protocol/src/
```

#### 原则 2: 分离文件移动和内容修改
**为什么**: 让 Git 最好地识别重命名
**正确做法**:
```bash
# 第一步：只移动文件（不修改内容）
git mv src/protocol crates/piper-protocol/src/
git commit -m "refactor(protocol): move to workspace crate"

# 第二步：修改内容（更新导入路径等）
# ... 编辑文件 ...
git commit -m "refactor(protocol): update import paths"
```

#### 原则 3: 避免循环开发依赖
**风险**: 如果底层 crate 的测试依赖高层 crate，会导致编译失败
**检查**: 迁移前检查 `tests/` 是否有共享测试工具，必要时创建 `piper-test-utils`

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

### 0.5 检查公共类型和测试工具

在开始拆分之前，必须检查是否有被多个层使用的共享代码。

#### 0.5.1 检查公共类型

```bash
# 检查是否有 utils 或 common 模块
find src -name "utils.rs" -o -name "common.rs"

# 检查是否有被多个模块导入的类型
grep -r "use crate::common" src/
grep -r "use crate::utils" src/
```

**如果发现公共类型**:
```bash
# 创建 piper-common crate
mkdir -p crates/piper-common/src
touch crates/piper-common/src/lib.rs

# 将公共代码移动过去
git mv src/utils.rs crates/piper-common/src/utils.rs
git mv src/common.rs crates/piper-common/src/common.rs

git commit -m "refactor: extract common types to piper-common crate"
```

#### 0.5.2 检查测试工具

```bash
# 检查 tests/ 目录结构
ls -la tests/

# 查找共享的测试辅助代码
find tests -name "helpers.rs" -o -name "common.rs" -o -name "mod.rs"
```

**如果发现共享测试工具**:
- **选项 A**: 创建独立的 `piper-test-utils` crate（仅 `[dev-dependencies]`）
- **选项 B**: 将测试辅助代码保留在 `tests/common/`，但确保各 crate 的测试不依赖它

#### 0.5.3 检查循环依赖风险

```bash
# 检查 tests/ 是否引用了 src/ 的代码
grep -r "use piper_sdk" tests/

# 如果有，标记为需要在迁移后修复
echo "⚠️  Found tests that import piper_sdk" > cyclic_deps_warning.txt
```

#### 0.5.4 验收标准

- [ ] 公共类型已识别并处理
- [ ] 测试工具已识别并处理
- [ ] 循环依赖风险已评估

### 0.6 检查 .gitignore

确保 `.gitignore` 配置正确，避免提交不必要的文件。

```bash
# 检查现有 .gitignore
cat .gitignore

# 应该包含以下内容（如果没有则添加）
cat >> .gitignore << 'EOF'
# Rust build artifacts
/target/
**/target/

# Backup files
**/*.rs.bk
*.rs.bk

# Cargo lock file (workspace 只有一个 Cargo.lock)
/Cargo.lock

# IDE
.vscode/
.idea/
*.swp
*.swo
*~

# OS
.DS_Store
Thumbs.db
EOF

git add .gitignore
git commit -m "chore: ensure .gitignore is properly configured"
```

#### 0.6.1 验收标准

- [ ] `.gitignore` 包含 `target/` 和 `**/*.rs.bk`
- [ ] `.gitignore` 包含 `/Cargo.lock`（workspace 只有一个根 Cargo.lock）

### 0.7 检查非 Cargo 构建配置

**⚠️ 重要**: 如果项目使用 Docker、Makefile 或其他构建脚本，需要更新路径引用。

#### 0.7.1 检查 Dockerfile

```bash
# 检查是否存在 Dockerfile
if [ -f Dockerfile ]; then
    echo "发现 Dockerfile，需要检查以下内容:"
    echo "1. COPY src/ ./src/  → 需要更新为 COPY crates/ ./crates/ 和 COPY apps/ ./apps/"
    echo "2. COPY tests/ ./tests/  → 需要更新（如果集成测试已移动）"
    echo ""
    echo "建议更新命令:"
    echo "  COPY Cargo.toml Cargo.lock ./"
    echo "  COPY crates/ ./crates/"
    echo "  COPY apps/ ./apps/"
fi
```

**如果发现 Dockerfile，记录待更新**:
```bash
echo "⚠️  发现 Dockerfile，需要在阶段 9.2 更新" > dockerfile_update_warning.txt
```

#### 0.7.2 检查 Makefile

```bash
# 检查是否存在 Makefile
if [ -f Makefile ]; then
    echo "发现 Makefile，需要检查以下内容:"
    echo "1. 路径引用（如 SRC_DIR=src/）"
    echo "2. 测试命令（如 cargo test --test integration）"
    echo "3. 构建命令（如 cargo build --bin gs_usb_daemon）"
fi
```

**如果发现 Makefile，记录待更新**:
```bash
echo "⚠️  发现 Makefile，需要在阶段 9.2 更新" > makefile_update_warning.txt
```

#### 0.7.3 检查 CI/CD 配置

```bash
# 检查常见的 CI 配置文件
for ci_file in .github/workflows/*.yml .gitlab-ci.yml Jenkinsfile; do
    if [ -f "$ci_file" ]; then
        echo "发现 CI 配置: $ci_file"
        echo "需要检查:"
        echo "1. 路径引用（如 examples/, tests/）"
        echo "2. cargo test 命令（需要使用 -p 指定 crate）"
        echo "3. cargo build 命令（需要使用 --bin 指定二进制）"
    fi
done
```

#### 0.7.4 验收标准

- [ ] 已检查 Dockerfile（如果存在）
- [ ] 已检查 Makefile（如果存在）
- [ ] 已检查 CI/CD 配置（如果存在）
- [ ] 所有发现的配置文件已记录待更新

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

# CAN 硬件（所有平台都声明，具体 crate 按需选择）
rusb = "0.9"
socketcan = "2.0"
```

**⚠️ 重要**: `[workspace.dependencies]` 的作用是**声明版本号变量**，不支持条件语法。平台特定依赖的选择应该在各个 crate 的 `Cargo.toml` 中通过 `target.'cfg...'` 引用。

### 1.2 清理旧 Cargo.lock

**⚠️ 重要**: 在转换为 workspace 之前，清理旧的 `Cargo.lock`，避免依赖冲突。

```bash
# 备份旧的 Cargo.lock（以防需要回滚）
cp Cargo.lock Cargo.lock.bak

# 删除旧的 Cargo.lock
rm Cargo.lock

# 让 Cargo 重新生成 workspace 的 Cargo.lock
cargo generate-lockfile

# 验证新的 Cargo.lock
head -n 20 Cargo.lock
# 应该看到: # This file is automatically @generated by Cargo.
# 以及 workspace 版本信息

# 如果一切正常，删除备份
rm Cargo.lock.bak

git add Cargo.lock
git commit -m "chore: regenerate Cargo.lock for workspace"
```

### 1.3 验收标准

- [ ] `cargo check` 不报错
- [ ] `cargo test` 通过所有测试
- [ ] `cargo build --release` 成功

### 1.4 预期问题

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
# ⚠️ 重要：使用 git mv 保留历史记录
# 方案 A: 移动整个文件夹后调整层级（推荐，更清晰）
git mv src/protocol crates/piper-protocol/src/

# 现在结构是: crates/piper-protocol/src/protocol/mod.rs（嵌套了）
# 我们需要将文件提出来到 src/ 下
git mv crates/piper-protocol/src/protocol/* crates/piper-protocol/src/
rmdir crates/piper-protocol/src/protocol

# 验证文件结构
ls crates/piper-protocol/src/
# 应该看到: ids.rs, feedback.rs, control.rs, config.rs, mod.rs
# 注意: mod.rs 的内容需要手动合并到 lib.rs（见下阶段）

# 立即提交（分离文件移动和内容修改）
git commit -m "refactor(protocol): move to workspace crate"
```

**⚠️ 为什么使用 `git mv`**:
- `mv` 会导致 Git 丢失文件历史（`git blame` 会断层）
- `git mv` 让 Git 识别这是重命名操作，保留完整历史
- 这是**不可逆**的操作，必须正确执行

**如果不幸使用了 `mv`**:
```bash
# 恢复方法（如果尚未推送）
git reset --hard HEAD~1
git mv src/protocol crates/piper-protocol/src/
git commit -m "refactor(protocol): move to workspace crate (with git mv)"
```

### 2.4 更新 lib.rs

**文件**: `crates/piper-protocol/src/lib.rs`

首先，检查 `src/protocol/mod.rs` 的内容，将其合并到 `lib.rs`：

```bash
# 查看原 mod.rs 的内容
cat crates/piper-protocol/src/mod.rs

# 如果 mod.rs 有 pub use 或 pub mod 声明，需要合并到 lib.rs
# 通常 mod.rs 的内容应该类似:
#   pub mod ids;
#   pub mod feedback;
#   pub mod control;
#   pub mod config;
```

然后创建/更新 `lib.rs`：

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

**⚠️ 重要**: 合并完 `mod.rs` 的内容后，删除 `mod.rs`：

```bash
rm crates/piper-protocol/src/mod.rs

# 提交 lib.rs 的修改
git add crates/piper-protocol/src/lib.rs
git commit -m "refactor(protocol): update lib.rs with module declarations"
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

[features]
default = []  # 不启用任何 feature，由平台特定配置决定

# CAN 后端选择（互斥，通常通过目标平台自动选择）
socketcan = []  # Linux 平台自动启用
gs_usb = []     # 非 Linux 平台自动启用
mock = []       # 用于测试的 mock 实现

[dependencies]
piper-protocol = { workspace = true }
thiserror = { workspace = true }
tracing = { workspace = true }

# 平台特定依赖（标记为 optional 以便在 features 中引用）
[target.'cfg(target_os = "linux")'.dependencies]
socketcan = { workspace = true, optional = true }

[target.'cfg(not(target_os = "linux"))'.dependencies]
rusb = { workspace = true, optional = true, features = ["usb"] }
```

**⚠️ 重要说明**:
1. `optional = true` 是必需的，以便依赖可以被 features 引用
2. `socketcan` 和 `gs_usb` features 主要用于明确标识和测试目的
3. 实际使用时，平台自动决定启用哪个后端：
   - Linux → `socketcan` feature 自动启用
   - macOS/Windows → `gs_usb` feature 自动启用
4. `mock` feature 完全移除所有硬件依赖，用于单元测试

### 3.3 移动代码

```bash
# ⚠️ 重要：使用 git mv 保留历史记录
# 移动整个文件夹后调整层级
git mv src/can crates/piper-can/src/

# 现在结构是: crates/piper-can/src/can/mod.rs（嵌套了）
# 将文件提出来到 src/ 下
git mv crates/piper-can/src/can/* crates/piper-can/src/
rmdir crates/piper-can/src/can

# 验证文件结构
ls crates/piper-can/src/
# 应该看到: mod.rs, adapter.rs, frame.rs, socketcan/, gs_usb/, gs_usb_udp/

# 立即提交
git commit -m "refactor(can): move to workspace crate"

# ⚠️ 注意: mod.rs 的内容需要手动合并到 lib.rs（见阶段 3.4）
```

### 3.4 更新 lib.rs

首先，检查并合并 `mod.rs` 的内容：

```bash
# 查看原 mod.rs 的内容
cat crates/piper-can/src/mod.rs
```

然后更新 `lib.rs`，将 `mod.rs` 的模块声明合并进去：

**文件**: `crates/piper-can/src/lib.rs`
```rust
// 修改前
use crate::protocol::ids::*;

// 修改后
use piper_protocol::ids::*;
```

**⚠️ 重要**: 合并完 `mod.rs` 的内容后，删除 `mod.rs` 并更新内部导入：

```bash
# 1. 删除 mod.rs
rm crates/piper-can/src/mod.rs

# 2. 更新所有内部导入（从 crate::can::xxx 改为直接使用）
# 例如在 adapter.rs 中:
#   use crate::can::frame::PiperFrame;  →  use crate::frame::PiperFrame;

# 3. 提交修改
git add crates/piper-can/src/
git commit -m "refactor(can): update lib.rs and internal imports"
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
# ⚠️ 重要：使用 git mv 保留历史记录
# 移动整个文件夹后调整层级
git mv src/driver crates/piper-driver/src/

# 现在结构是: crates/piper-driver/src/driver/mod.rs（嵌套了）
# 将文件提出来到 src/ 下
git mv crates/piper-driver/src/driver/* crates/piper-driver/src/
rmdir crates/piper-driver/src/driver

# 验证文件结构
ls crates/piper-driver/src/
# 应该看到: mod.rs, piper.rs, pipeline.rs, state.rs, builder.rs,
#              command/, heartbeat.rs, metrics.rs

# 立即提交
git commit -m "refactor(driver): move to workspace crate"

# ⚠️ 注意: mod.rs 的内容需要手动合并到 lib.rs（见阶段 4.4）
```

### 4.4 更新 lib.rs 和导入

首先，检查并合并 `mod.rs` 的内容：

```bash
# 查看原 mod.rs 的内容
cat crates/piper-driver/src/mod.rs
```

然后更新 `lib.rs`，将 `mod.rs` 的模块声明合并进去。

**需要修改的关键文件**（现在直接位于 `src/` 下）:
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

**⚠️ 重要**: 合并完 `mod.rs` 的内容后，删除 `mod.rs` 并更新内部导入：

```bash
# 1. 删除 mod.rs
rm crates/piper-driver/src/mod.rs

# 2. 更新所有内部导入（从 crate::driver::xxx 改为直接使用）
# 例如在 piper.rs 中:
#   use crate::driver::state::RobotState;  →  use crate::state::RobotState;

# 3. 提交修改
git add crates/piper-driver/src/
git commit -m "refactor(driver): update lib.rs and internal imports"
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
# ⚠️ 重要：使用 git mv 保留历史记录
# 移动整个文件夹后调整层级
git mv src/client crates/piper-client/src/

# 现在结构是: crates/piper-client/src/client/mod.rs（嵌套了）
# 将文件提出来到 src/ 下
git mv crates/piper-client/src/client/* crates/piper-client/src/
rmdir crates/piper-client/src/client

# 验证文件结构
ls crates/piper-client/src/
# 应该看到: mod.rs, builder.rs, motion.rs, observer.rs,
#              state/, control/, types/, heartbeat.rs

# 立即提交
git commit -m "refactor(client): move to workspace crate"

# ⚠️ 注意: mod.rs 的内容需要手动合并到 lib.rs（见阶段 5.4）
```

### 5.4 更新 lib.rs 和导入

首先，检查并合并 `mod.rs` 的内容：

```bash
# 查看原 mod.rs 的内容
cat crates/piper-client/src/mod.rs
```

然后更新 `lib.rs`，将 `mod.rs` 的模块声明合并进去。

**关键修改点**（现在直接位于 `src/` 下）:
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

**⚠️ 重要**: 合并完 `mod.rs` 的内容后，删除 `mod.rs` 并更新内部导入：

```bash
# 1. 删除 mod.rs
rm crates/piper-client/src/mod.rs

# 2. 更新所有内部导入（从 crate::client::xxx 改为直接使用）
# 例如在 builder.rs 中:
#   use crate::client::types::Error;  →  use crate::types::Error;

# 3. 提交修改
git add crates/piper-client/src/
git commit -m "refactor(client): update lib.rs and internal imports"
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
# ⚠️ 重要：使用 git mv 保留历史记录
git mv src/bin/gs_usb_daemon apps/daemon

# 立即提交
git commit -m "refactor(daemon): move to apps/ directory"

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

### 8.1 移动集成测试到 piper-sdk crate

**⚠️ 关键步骤**: 解决 Virtual Workspace 的 `tests/` 忽略问题

#### 8.1.5 理解问题

**问题根源**:
- 在阶段 1 中，我们将根 `Cargo.toml` 转换为 `[workspace]`（Virtual Workspace）
- **在 Virtual Workspace（根目录没有 `package` 定义）中，Cargo 会自动忽略根目录下的 `tests/` 文件夹**
- 这意味着根目录的集成测试**不会运行**，但不会报错，给你虚假的安全感

**症状**:
```bash
# 这些测试会悄无声息地不执行
cargo test --test high_level_integration_v2
cargo test --test robot_integration_tests
# "No such test target" 但不会失败
```

**解决方案**: 将根目录的 `tests/` 移动到 `crates/piper-sdk/tests/`，因为 piper-sdk 是测试 SDK 最终接口的合适位置。

#### 8.1.6 移动集成测试

```bash
# ⚠️ 重要：使用 git mv 保留历史记录
# 创建 piper-sdk tests 目录
mkdir -p crates/piper-sdk/tests

# 移动所有集成测试
git mv tests/*.rs crates/piper-sdk/tests/

# 立即提交
git commit -m "refactor(tests): move integration tests to piper-sdk crate

This resolves the Virtual Workspace tests/ ignore issue.
Integration tests now live in piper-sdk where they test the final SDK API."

# 验证移动成功
ls crates/piper-sdk/tests/
# 应该看到: high_level_integration_v2.rs, robot_integration_tests.rs,
#            high_level_phase1_integration.rs 等

# 删除空的 tests 目录
rmdir tests 2>/dev/null || true

git add tests
git commit -m "chore: remove empty tests directory"
```

#### 8.1.7 验证测试仍然可运行

```bash
# 验证测试现在从 piper-sdk 运行
cargo test -p piper-sdk --test high_level_integration_v2
cargo test -p piper-sdk --test robot_integration_tests
cargo test -p piper-sdk --test high_level_phase1_integration

# 验证所有测试通过
cargo test -p piper-sdk
```

#### 8.1.8 更新 CI/CD 配置（如果有）

如果项目的 CI 配置直接引用了根目录的测试，需要更新：

```yaml
# .github/workflows/test.yml (修改前)
- name: Run integration tests
  run: cargo test --test high_level_integration_v2

# .github/workflows/test.yml (修改后)
- name: Run integration tests
  run: cargo test -p piper-sdk --test high_level_integration_v2
```

### 8.2 更新所有示例的导入

虽然 `piper-sdk` 提供了向后兼容，但我们应该更新示例使用新的 crate 结构。

**脚本化批量更新**:
```bash
# 查找所有需要更新的示例
find examples -name "*.rs" -exec grep -l "use piper_sdk" {} \;

# 可选：更新为使用 piper-client
# sed -i '' 's/use piper_sdk::/use piper_client::/g' examples/*.rs
```

**注意**: 为了向后兼容，示例可以保持使用 `piper-sdk`

### 8.3 更新集成测试路径

**检查文件**（注意：这些文件现在在 `crates/piper-sdk/tests/`）:
- `crates/piper-sdk/tests/high_level_integration_v2.rs`
- `crates/piper-sdk/tests/robot_integration_tests.rs`
- `crates/piper-sdk/tests/high_level_phase1_integration.rs`

**验证**:
```bash
# 测试现在从 piper-sdk 运行（已经在 8.1.7 中验证）
cargo test -p piper-sdk --test high_level_integration_v2
cargo test -p piper-sdk --test robot_integration_tests
cargo test -p piper-sdk --test high_level_phase1_integration
```

### 8.4 验收标准

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

### 9.25 配置 Feature Flags

由于 workspace 中有多个 crate，需要确保 features 正确传递。

#### 9.25.1 在 `piper-can` 中定义 features

**文件**: `crates/piper-can/Cargo.toml`

**⚠️ 重要**: features 定义已在阶段 3.2 中完成，这里只需验证和确认。

```toml
[features]
default = []  # 空默认值，由平台自动选择

# CAN 后端 features（标识符，不使用 dep: 语法）
socketcan = []  # Linux: 由 target cfg 自动启用
gs_usb = []     # macOS/Windows: 由 target cfg 自动启用
mock = []       # 测试: 完全移除硬件依赖
```

**为什么不用 `dep:` 语法**:
- `socketcan` 和 `gs_usb` 依赖通过 `target.'cfg...'` 自动包含
- features 只是标识符，用于明确启用哪个后端（主要用于测试）
- 不需要 `dep:socketcan` 因为依赖已经通过平台配置包含

**平台自动选择逻辑**:
- Linux 编译: `socketcan` 依赖自动包含（由 `target.'cfg(target_os = "linux")'.dependencies` 控制）
- macOS/Windows 编译: `gs_usb` 依赖自动包含（由 `target.'cfg(not(target_os = "linux"))'.dependencies` 控制）
- 测试编译: 启用 `mock` feature，移除所有硬件依赖

#### 9.25.2 在 `piper-sdk` 中重新暴露 features

**文件**: `crates/piper-sdk/Cargo.toml`
```toml
[features]
default = []  # 不启用默认 features，由平台自动选择

# 重新暴露 CAN 后端 features（用于明确指定）
socketcan = ["piper-can/socketcan"]
gs_usb = ["piper-can/gs_usb"]
mock = ["piper-can/mock"]  # 用于测试

# 用户 API features
client = ["piper-client"]
```

**⚠️ 重要**: `piper-sdk` 的 features 只是标识符传递，不使用 `dep:` 语法。

#### 9.25.3 验证 Feature Flags

```bash
# 测试默认 feature（平台自动选择后端）
cargo build -p piper-sdk
# Linux: 自动使用 socketcan
# macOS/Windows: 自动使用 gs_usb

# 测试 mock feature（用于测试）
cargo test -p piper-sdk --features mock

# 验证 feature 标识符传递
cargo build -p piper-sdk --features socketcan  # 强制使用 socketcan
cargo build -p piper-sdk --features gs_usb      # 强制使用 gs_usb
```

### 9.26 检查文档内链接

拆分 crate 后，文档中的链接可能会失效。需要检查所有 intra-doc links。

```bash
# 构建文档并检查链接
cargo doc --no-deps --document-private-items 2>&1 | grep "broken"

# 如果有 broken link 警告，记录下来待修复
echo "⚠️  Intra-doc link check" > doc_link_check.txt
```

**修复 broken links**:
- 底层 crate 不应引用高层 crate 的链接
- 将无法解析的链接改为纯文本或完整 URL
- 例如: `[`PiperClient`]` → `PiperClient`（纯文本）

### 9.3 发布 v0.1.0

⚠️ **重要**: Workspace 发布比单体库复杂，必须遵循特定顺序。

#### 9.3.1 配置 cargo-release（推荐）

在发布前，在根目录 `Cargo.toml` 中添加 `cargo-release` 的 workspace 配置：

**文件**: `Cargo.toml`（根目录）

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

# ... [workspace.package] 和 [workspace.dependencies] ...

[workspace.metadata.release]
# 统一的 tag 命名格式（避免 piper-protocol-v0.1.0 这样的冲突）
tag-name = "v{{version}}"

# 将所有 crate 的提交合并为一个（原子操作）
consolidate-commits = true

# 将所有 crate 的推送合并为一个
consolidate-pushes = true

# 发布前先运行所有测试
pre-release-hook = ["cargo", "test", "--workspace"]

# 推送 tag 到远程
push = true

# 发布到 crates.io
publish = true

# 不为每个 crate 创建单独的 tag（只创建一个 workspace 级别的 tag）
shared-version = true
```

**⚠️ 重要**: 这个配置确保：
- 所有 crate 共享同一个版本号（`shared-version = true`）
- 只创建一个 `v0.1.0` tag，而不是 `piper-protocol-v0.1.0`, `piper-can-v0.1.0` 等
- 所有发布操作在一个原子操作中完成

#### 9.3.2 安装发布工具

```bash
# 安装 cargo-release
cargo install cargo-release

# 验证安装
cargo release --version
```

#### 9.3.3 手动发布顺序（备选方案）

**⚠️ 重要**: 这是手动发布的备选方案。如果你配置了 `[workspace.metadata.release]`（见 9.3.1），**强烈推荐使用阶段 9.3.4 的自动发布**。

**必须按依赖顺序从底层到高层发布**:

1. **发布 `piper-protocol`**
   ```bash
   cd crates/piper-protocol
   cargo publish
   ```

2. **等待 crates.io 索引更新** ⏱️ **等待 1-2 分钟**
   ```bash
   echo "⏳  等待 crates.io 索引 piper-protocol v0.1.0..."
   sleep 90
   ```

3. **发布 `piper-can`**
   ```bash
   cd ../piper-can
   cargo publish
   ```

4. **等待 crates.io 索引更新** ⏱️ **等待 1-2 分钟**

5. **发布 `piper-driver`**
   ```bash
   cd ../piper-driver
   cargo publish
   ```

6. **等待 crates.io 索引更新** ⏱️ **等待 1-2 分钟**

7. **发布 `piper-client`**
   ```bash
   cd ../piper-client
   cargo publish
   ```

8. **等待 crates.io 索引更新** ⏱️ **等待 1-2 分钟**

9. **最后发布 `piper-sdk`**
   ```bash
   cd ../piper-sdk
   cargo publish
   ```

**⚠️ 注意**: 使用 `cargo publish`（Rust 原生命令）而不是 `cargo release`（工具命令），避免与 workspace 配置冲突。

#### 9.3.4 使用 cargo-release 自动发布（最推荐）

如果配置了 `[workspace.metadata.release]`（见 9.3.1），可以一键发布整个 workspace：

```bash
# 方式 1: 自动发布所有 crates（按拓扑顺序）
cargo release --workspace --no-dev

# 这个命令会自动：
# 1. 按依赖顺序发布所有 crates（protocol → can → driver → client → sdk）
# 2. 等待 crates.io 索引更新
# 3. 创建一个统一的 v0.1.0 tag
# 4. 推送 tag 到远程
# 5. 合并所有提交和推送操作

# 方式 2: 手动指定发布某个 crate（不推荐，除非只发布单个 crate）
cargo release -p piper-protocol --no-dev
# 注意：如果使用 shared-version，手动发布单个 crate 可能导致版本不一致
```

**⚠️ 重要**: 使用 `cargo release --workspace` 时，确保：
- 所有 crate 的 `[package]` 部分都有 `version.workspace = true`
- 所有内部依赖使用 `workspace = true` 或包含 `version`
- 已配置 `[workspace.metadata.release]`（见阶段 9.3.1）

#### 9.3.5 发布检查清单

**发布前**:
- [ ] 所有 crate 的 `version` 已更新（使用 `workspace.package.version`）
- [ ] 所有内部依赖使用 `workspace = true` 或包含 `version` 字段
- [ ] `cargo test --workspace` 全部通过
- [ ] `cargo clippy --workspace` 无警告
- [ ] `cargo doc --workspace` 无 broken links
- [ ] 所有 CHANGELOG 已更新

#### 验证内部依赖配置

在发布前，确保所有 workspace 内部依赖配置正确：

```bash
# 检查所有 Cargo.toml 文件中的内部依赖
grep -r "piper-" crates/*/Cargo.toml

# 正确的配置（两种方式都正确）:
# 方式 1: 使用 workspace = true（推荐）
[dependencies]
piper-protocol = { workspace = true }

# 方式 2: 显式指定 version（兼容性更好）
[dependencies]
piper-protocol = { version = "0.1.0", path = "../piper-protocol" }

# 错误的配置（缺少 version）:
[dependencies]
piper-protocol = { path = "../piper-protocol" }  # ❌ 缺少 version
```

**为什么需要 `version`**:
- `workspace = true` 在 workspace 内部有效，但发布到 crates.io 后需要 `version`
- 如果使用 `path` 依赖，必须同时指定 `version`，否则 crates.io 会拒绝
- **最佳实践**: 使用 `workspace = true`，让 `[workspace.dependencies]` 统一管理版本

**发布中**:
- [ ] 按依赖顺序发布（protocol → can → driver → client → sdk）
- [ ] 每次发布后等待 1-2 分钟让 crates.io 索引更新
- [ ] 验证每个 crate 在 crates.io 上可访问

**发布后**:
- [ ] 创建 Git tag: `git tag v0.1.0`
- [ ] 推送 tag: `git push origin v0.1.0`
- [ ] 验证用户可以从 crates.io 安装:
  ```bash
  cargo search piper-sdk
  cargo add piper-sdk --vers "0.1.0"
  ```

### 9.5 合并到主分支

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
