# src/ 目录分析报告

**Date**: 2025-01-29
**Status**: ⚠️ 应删除（已废弃）

## 问题

项目根目录的 `src/` 目录已经**完全过时**，但由于 workspace 架构重构，这些代码**不会被编译或使用**。

## 当前架构

### Workspace 结构

```toml
[workspace]
members = [
    "crates/piper-protocol",
    "crates/piper-can",
    "crates/piper-driver",
    "crates/piper-client",
    "crates/piper-sdk",      # ✅ 这是实际的入口点
    "crates/piper-tools",
    "crates/piper-physics",
    "apps/daemon",
    "apps/cli",
]
```

**注意**: workspace members 中**没有包含根目录的 package**，只有 workspace 定义。

### 根目录 Cargo.toml

```toml
# 只有 [workspace] 定义，没有 [package] section
[workspace]
resolver = "2"
members = [...]
```

这意味着：
- 根目录的 `src/` **不会被编译**
- 根目录的 `src/` **不会被发布**
- 根目录的 `src/` **是死代码**

## 文件对比

### `src/lib.rs` vs `crates/piper-sdk/src/lib.rs`

#### 旧代码 (`src/lib.rs`) - 已废弃
```rust
// ❌ 这种写法假设模块在 src/ 目录下
pub mod can;
pub mod client;
pub mod driver;
pub mod protocol;
```

#### 新代码 (`crates/piper-sdk/src/lib.rs`) - 正在使用
```rust
// ✅ 这种写法从各个 crate re-export
pub mod can {
    pub use piper_can::*;
}

pub mod protocol {
    pub use piper_protocol::*;
}

pub mod driver {
    pub use piper_driver::*;
}

pub mod client {
    pub use piper_client::*;
}
```

**关键区别**：
- 旧代码：**单体架构**，所有模块在同一个包内
- 新代码：**模块化架构**，每个层是独立的 crate

### `src/prelude.rs` vs `crates/piper-sdk/src/prelude.rs`

两个文件**内容完全相同**（27行代码）。

## 危险

保留 `src/` 目录会导致：

1. **混淆**：开发者可能修改 `src/` 而不是 `crates/piper-sdk/src/`，导致修改不生效
2. **不一致**：两个版本的代码可能逐渐分离，造成维护困难
3. **误导**：新贡献者不知道应该修改哪个文件

## 建议

### ✅ 立即删除 `src/` 目录

```bash
# 安全删除（先确认）
git rm -r src/

# 或直接删除
rm -rf src/
```

### 验证删除后项目正常工作

```bash
# 编译 workspace
cargo build --workspace

# 运行测试
cargo test --workspace

# 运行 clippy
cargo clippy --workspace --all-targets -- -D warnings
```

## 正确的使用方式

### 用户使用 SDK

```toml
[dependencies]
piper-sdk = "0.0.3"  # 从 crates/piper-sdk 发布
```

### 开发者修改代码

- 修改 CAN 层 → `crates/piper-can/src/`
- 修改协议层 → `crates/piper-protocol/src/`
- 修改驱动层 → `crates/piper-driver/src/`
- 修改客户端层 → `crates/piper-client/src/`
- 修改 SDK 入口 → `crates/piper-sdk/src/` ✅ **这里是新的入口点**

## 迁移历史

根据文件日期，迁移发生在：

- `src/lib.rs` 最后修改：2025-01-25 16:56
- `crates/piper-sdk` 创建：更早

这说明：
1. 项目从单体架构重构为 workspace 架构
2. `src/` 目录是旧架构的残留
3. 迁移完成后忘记删除旧代码

## 结论

**`src/` 目录应该立即删除**，因为：

1. ✅ 代码已完全迁移到 `crates/piper-sdk`
2. ✅ Workspace 架构不使用根 `src/`
3. ✅ 保留会造成混淆和维护问题
4. ✅ 没有任何代码依赖这个目录

删除这个目录不会影响任何功能，只会让项目结构更清晰。
