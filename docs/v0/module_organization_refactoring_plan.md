# 模块组织重构执行方案

## 文档信息

- **文档版本**: 1.0
- **创建日期**: 2024-12-19
- **基于**: `module_organization_analysis.md` 推荐方案 B+
- **目标版本**: v0.2.0（或 v1.0.0）

---

## 执行摘要

本执行方案基于模块组织分析报告的推荐方案 B+（功能导向命名 + Facade Pattern），将现有模块结构重构为更符合 Rust 社区习惯的组织方式。

**核心变更**：
- `robot` → `driver`（功能导向命名）
- `high_level` → `client`（功能导向命名）
- 添加 `prelude` 模块
- 实现 Facade Pattern 在 `lib.rs`
- 重命名 `RobotError` → `DriverError`

**向后兼容性**：通过 `#[deprecated]` 标记保留旧模块，在下一个主版本中移除。

---

## 1. 执行阶段规划

### 阶段 1: 准备阶段（1-2 天）
- 创建新分支
- 备份当前代码
- 准备测试环境

### 阶段 2: 模块迁移（2-3 天）
- 创建新模块结构
- 移动和重命名文件
- 更新模块引用

### 阶段 3: 代码更新（2-3 天）
- 更新 `lib.rs` 和 `prelude.rs`
- 更新所有内部引用
- 重命名 `RobotError` → `DriverError`

### 阶段 4: 向后兼容层（1 天）
- 创建兼容层模块
- 添加 `#[deprecated]` 标记

### 阶段 5: 测试和验证（2-3 天）
- 运行所有测试
- 更新示例代码
- 验证向后兼容性

### 阶段 6: 文档更新（1-2 天）
- 更新 README
- 更新 API 文档
- 编写迁移指南

**总预计时间**: 9-14 天

---

## 2. 详细执行步骤

### 2.1 阶段 1: 准备阶段

#### 步骤 1.1: 创建重构分支
```bash
git checkout -b refactor/module-organization
git push -u origin refactor/module-organization
```

#### 步骤 1.2: 备份当前状态
```bash
# 创建备份标签
git tag backup-before-module-refactor
git push origin backup-before-module-refactor
```

#### 步骤 1.3: 运行当前测试套件
```bash
cargo test --all-features
cargo test --examples
# 确保所有测试通过，作为基准
```

#### 步骤 1.4: 检查 Cargo.toml
```bash
# 检查是否有硬编码的路径需要更新
cat Cargo.toml | grep -E "path|example|bin"

# 检查是否有涉及模块路径的自定义配置
cat Cargo.toml | grep -E "metadata|workspace"
```

**需要检查的内容**：
- `[[example]]` 部分是否有显式的 `path` 配置
- `[[bin]]` 部分是否有显式的 `path` 配置
- `[package.metadata]` 等自定义配置中是否有涉及模块路径的内容

**注意**：通常 Rust 会自动发现示例和二进制文件，但如果 `Cargo.toml` 中有显式配置，可能需要更新路径。

---

### 2.2 阶段 2: 模块迁移

#### 步骤 2.1: 检查 Cargo.toml

在开始移动文件之前，先检查 `Cargo.toml` 是否有需要更新的配置：

```bash
# 检查是否有硬编码的路径需要更新
cat Cargo.toml | grep -E "path|example|bin"

# 检查是否有涉及模块路径的自定义配置
cat Cargo.toml | grep -E "metadata|workspace"
```

**需要检查的内容**：
- `[[example]]` 部分是否有显式的 `path` 配置（通常 Rust 会自动发现，但如果有显式配置可能需要更新）
- `[[bin]]` 部分是否有显式的 `path` 配置
- `[package.metadata]` 等自定义配置中是否有涉及模块路径的内容

**注意**：如果 `Cargo.toml` 中有显式路径配置，在文件移动后可能需要更新。

#### 步骤 2.2: 创建新模块目录结构

```bash
# 重要：不要预先创建 src/driver 目录！
# 如果 src/driver 目录已存在，后续 git mv src/robot src/driver 会将 robot 文件夹
# 移动到 driver 内部，变成 src/driver/robot/，而不是重命名为 src/driver/
# 当目标目录不存在时，git mv 表现为重命名，这正是我们需要的

# 创建 client 模块目录
# 注意：只创建父目录 src/client，不要预先创建 state/control/types 子目录
# 否则后续 git mv src/high_level/state src/client/ 会因为目标目录已存在而报错或导致嵌套
mkdir -p src/client
```

#### 步骤 2.3: 迁移 driver 模块（原 robot）

**文件映射表**：

| 源文件 | 目标文件 | 操作 |
|--------|---------|------|
| `src/robot/mod.rs` | `src/driver/mod.rs` | 移动并更新内容 |
| `src/robot/builder.rs` | `src/driver/builder.rs` | 移动 |
| `src/robot/command.rs` | `src/driver/command.rs` | 移动 |
| `src/robot/error.rs` | `src/driver/error.rs` | 移动并重命名类型 |
| `src/robot/fps_stats.rs` | `src/driver/fps_stats.rs` | 移动 |
| `src/robot/metrics.rs` | `src/driver/metrics.rs` | 移动 |
| `src/robot/pipeline.rs` | `src/driver/pipeline.rs` | 移动 |
| `src/robot/robot_impl.rs` | `src/driver/piper.rs` | 移动并重命名 |
| `src/robot/state.rs` | `src/driver/state.rs` | 移动 |

**执行命令**（使用 `git mv` 保留 Git 历史）：
```bash
# 重要：使用 git mv 而不是 mv，以保留文件的 Git 历史记录
# 这样可以保留文件的修改历史和 Blame 信息，对后续维护至关重要

# 重要：确保 src/driver 目录不存在！
# 如果 src/driver 已存在，git mv 会将 robot 移动到 driver 内部（src/driver/robot/）
# 当目标目录不存在时，git mv 表现为重命名（src/robot → src/driver），这正是我们需要的

# 移动整个 robot 目录到 driver（重命名）
git mv src/robot src/driver

# 重命名 robot_impl.rs → piper.rs
git mv src/driver/robot_impl.rs src/driver/piper.rs
```

**注意**：使用 `git mv` 而不是普通的 `mv` 命令，这样可以：
- ✅ 保留文件的 Git 历史记录
- ✅ 保留文件的 Blame 信息
- ✅ 便于后续追溯 Bug 和代码变更
- ✅ 避免 Git 认为删除了旧文件并创建了新文件

#### 步骤 2.4: 迁移 client 模块（原 high_level）

**重要**：需要移除原有的 `client` 子目录，实现扁平化。

**文件映射表**：

| 源文件 | 目标文件 | 操作 |
|--------|---------|------|
| `src/high_level/mod.rs` | `src/client/mod.rs` | 移动并更新内容 |
| `src/high_level/client/motion_commander.rs` | `src/client/motion.rs` | 移动并重命名 |
| `src/high_level/client/observer.rs` | `src/client/observer.rs` | 移动 |
| `src/high_level/client/raw_commander.rs` | `src/client/raw_commander.rs` | 移动 |
| `src/high_level/client/heartbeat.rs` | `src/client/heartbeat.rs` | 移动 |
| `src/high_level/client/mod.rs` | （删除，内容合并到 `client/mod.rs`） | 删除 |
| `src/high_level/state/` | `src/client/state/` | 移动整个目录 |
| `src/high_level/control/` | `src/client/control/` | 移动整个目录 |
| `src/high_level/types/` | `src/client/types/` | 移动整个目录 |

**执行命令**（使用 `git mv` 保留 Git 历史）：
```bash
# 重要：使用 git mv 而不是 mv，以保留文件的 Git 历史记录
# 这样可以保留文件的修改历史和 Blame 信息，对后续维护至关重要

# 因为 src/client 已在步骤 2.2 创建且为空（不包含冲突的子目录），直接移动即可

# 1. 迁移子模块目录 (state, control, types)
# 注意：使用 if 检查作为防御性编程，确保源目录存在
if [ -d "src/high_level/state" ]; then
    git mv src/high_level/state src/client/
fi
if [ -d "src/high_level/control" ]; then
    git mv src/high_level/control src/client/
fi
if [ -d "src/high_level/types" ]; then
    git mv src/high_level/types src/client/
fi

# 2. 迁移 mod.rs
if [ -f "src/high_level/mod.rs" ]; then
    git mv src/high_level/mod.rs src/client/mod.rs
fi

# 3. 迁移 client 子目录中的文件（扁平化操作）
# 将 src/high_level/client/* 移动到 src/client/*
# 注意：git mv 不支持通配符批量重命名到新目录，需逐个移动
if [ -f "src/high_level/client/motion_commander.rs" ]; then
    git mv src/high_level/client/motion_commander.rs src/client/motion.rs
fi
if [ -f "src/high_level/client/observer.rs" ]; then
    git mv src/high_level/client/observer.rs src/client/
fi
if [ -f "src/high_level/client/raw_commander.rs" ]; then
    git mv src/high_level/client/raw_commander.rs src/client/
fi
if [ -f "src/high_level/client/heartbeat.rs" ]; then
    git mv src/high_level/client/heartbeat.rs src/client/
fi

# 4. 清理旧目录
# 删除空的 client 子目录
if [ -d "src/high_level/client" ]; then
    rmdir src/high_level/client
fi

# 删除空的 high_level 目录
if [ -d "src/high_level" ]; then
    rmdir src/high_level
fi
```

**注意**：
- 使用 `git mv` 而不是普通的 `mv` 命令，以保留 Git 历史
- 步骤 2.2 只创建了 `src/client` 父目录，不包含子目录，因此不会有目录冲突
- 所有 `git mv` 操作都使用 `if` 检查作为防御性编程，确保源文件/目录存在

---

### 2.3 阶段 3: 代码更新

#### 步骤 3.1: 更新 driver 模块的 mod.rs

**文件**: `src/driver/mod.rs`

**变更内容**：
```rust
//! 驱动层模块
//!
//! 本模块提供 Piper 机械臂的设备驱动功能，包括：
//! - IO 线程管理（单线程/双线程模式）
//! - 状态同步（ArcSwap 无锁读取）
//! - 帧解析与聚合
//! - 命令优先级管理
//!
//! # 使用场景
//!
//! 适用于需要直接控制 CAN 帧、需要高性能状态读取的场景。
//! 大多数用户应该使用 [`client`](crate::client) 模块提供的更高级接口。

mod builder;
pub mod command;
mod error;
mod fps_stats;
pub mod metrics;
pub mod pipeline;
mod piper;  // 原 robot_impl.rs
pub mod state;

pub use builder::{DriverType, PiperBuilder};
pub use command::{CommandPriority, PiperCommand};
pub use error::DriverError;  // 原 RobotError
pub use fps_stats::{FpsCounts, FpsResult};
pub use metrics::{MetricsSnapshot, PiperMetrics};
pub use pipeline::{PipelineConfig, io_loop, rx_loop, tx_loop, tx_loop_mailbox};
pub use piper::Piper;
pub use state::*;
```

#### 步骤 3.2: 重命名 RobotError → DriverError

**文件**: `src/driver/error.rs`

**变更内容**：
```rust
// 重命名错误类型
#[derive(Error, Debug)]
pub enum DriverError {  // 原 RobotError
    // ... 保持所有变体不变
}

// 更新所有相关类型别名和引用
pub type Result<T> = std::result::Result<T, DriverError>;
```

**需要更新的文件**：
- `src/driver/error.rs` - 定义处
- `src/driver/mod.rs` - 导出处
- `src/driver/builder.rs` - 所有使用处
- `src/driver/piper.rs` - 所有使用处
- `src/driver/pipeline.rs` - 所有使用处
- 所有其他使用 `RobotError` 的文件

**查找和替换命令**：
```bash
# 在 driver 模块中查找所有 RobotError
grep -r "RobotError" src/driver/

# 批量替换（谨慎使用，建议逐个文件检查）
# 注意：以下 sed 命令适用于 Linux (GNU sed)
# 如果在 macOS 上执行，请使用：sed -i '' 's/RobotError/DriverError/g' {} \;
find src/driver -type f -name "*.rs" -exec sed -i 's/RobotError/DriverError/g' {} \;
```

**macOS 兼容性说明**：
- **Linux (GNU sed)**: `sed -i 's/old/new/g' file`
- **macOS (BSD sed)**: `sed -i '' 's/old/new/g' file`
- 如果需要在 macOS 上执行，请使用 `sed -i ''` 而不是 `sed -i`

#### 步骤 3.3: 更新 client 模块的 mod.rs

**文件**: `src/client/mod.rs`

**变更内容**：
```rust
//! 客户端接口模块
//!
//! 本模块提供 Piper 机械臂的用户友好接口，包括：
//! - Type State Pattern（编译期状态安全）
//! - Commander/Observer 模式（读写分离）
//! - 强类型单位（Rad、Deg、NewtonMeter）
//! - 轨迹规划和控制
//!
//! # 使用场景
//!
//! 这是大多数用户应该使用的模块，提供了类型安全、易于使用的 API。
//! 如果需要直接控制 CAN 帧或需要更高性能，可以使用 [`driver`](crate::driver) 模块。

pub mod motion;  // 原 motion_commander.rs
pub mod observer;
pub(crate) mod raw_commander;
pub mod heartbeat;
pub mod state;
pub mod control;
pub mod types;

// 重新导出常用类型
pub use motion::MotionCommander;
pub use observer::Observer;
pub use state::Piper;  // Type State Pattern 的状态机
pub use types::*;
```

#### 步骤 3.4: 更新 client 模块内部引用

**需要更新的文件**：

1. **`src/client/motion.rs`**（原 `motion_commander.rs`）
   - 更新模块引用：`use crate::high_level::` → `use crate::client::`
   - 更新 `robot::Piper` → `driver::Piper`

2. **`src/client/observer.rs`**
   - 更新模块引用
   - 更新 `robot::Piper` → `driver::Piper`

3. **`src/client/raw_commander.rs`**
   - 更新模块引用
   - 更新 `robot::Piper` → `driver::Piper`

4. **`src/client/state/machine.rs`**
   - 更新模块引用
   - 更新 `robot::Piper` → `driver::Piper`

**查找和替换**：
```bash
# 查找所有 high_level 引用
grep -r "crate::high_level" src/client/
grep -r "use.*high_level" src/client/

# 查找所有 robot 引用
grep -r "crate::robot" src/client/
grep -r "robot::Piper" src/client/

# 批量替换（建议逐个文件检查）
# 注意：以下 sed 命令适用于 Linux (GNU sed)
# 如果在 macOS 上执行，请使用：sed -i '' 's/old/new/g' {} \;
find src/client -type f -name "*.rs" -exec sed -i 's/crate::high_level/crate::client/g' {} \;
find src/client -type f -name "*.rs" -exec sed -i 's/crate::robot/crate::driver/g' {} \;
find src/client -type f -name "*.rs" -exec sed -i 's/robot::Piper/driver::Piper/g' {} \;
```

#### 步骤 3.5: 更新 driver 模块内部引用

**需要更新的文件**：
- `src/driver/builder.rs` - 更新 `use crate::robot::` → `use crate::driver::`
- `src/driver/piper.rs` - 更新所有内部引用
- `src/driver/pipeline.rs` - 更新所有内部引用

**查找和替换**：
```bash
# 查找所有 robot 模块内部引用
grep -r "crate::robot" src/driver/

# 批量替换
# 注意：以下 sed 命令适用于 Linux (GNU sed)
# 如果在 macOS 上执行，请使用：sed -i '' 's/crate::robot/crate::driver/g' {} \;
find src/driver -type f -name "*.rs" -exec sed -i 's/crate::robot/crate::driver/g' {} \;
```

#### 步骤 3.6: 更新 protocol 和 can 模块的引用

**检查是否有直接引用 robot 或 high_level 的地方**：
```bash
# 在整个代码库中查找
grep -r "crate::robot" src/
grep -r "crate::high_level" src/
grep -r "use.*robot" src/
grep -r "use.*high_level" src/
```

#### 步骤 3.7: 创建 prelude 模块

**文件**: `src/prelude.rs`

**内容**：
```rust
//! Prelude - 常用类型的便捷导入
//!
//! 大多数用户应该使用这个模块来导入常用类型：
//!
//! ```rust
//! use piper_sdk::prelude::*;
//! ```

// 客户端层（推荐使用）
pub use crate::client::Piper;
pub use crate::client::{MotionCommander, Observer};
pub use crate::client::{JointArray, Rad, Deg, NewtonMeter};

// CAN 层（常用 Trait）
pub use crate::can::CanAdapter;

// 驱动层（高级用户使用）
pub use crate::driver::{Piper as Driver, PiperBuilder};

// 错误类型
pub use crate::can::CanError;
pub use crate::protocol::ProtocolError;
pub use crate::driver::DriverError;
```

#### 步骤 3.8: 更新 lib.rs

**文件**: `src/lib.rs`

**完整内容**：
```rust
//! Piper SDK - 松灵机械臂 Rust SDK
//!
//! 高性能、跨平台、零抽象开销的 Rust SDK，专用于松灵 Piper 机械臂的高频力控（>1kHz）。
//!
//! # 架构设计
//!
//! 本 SDK 采用分层架构，从底层到高层：
//!
//! - **CAN 层** (`can`): CAN 硬件抽象，支持 SocketCAN 和 GS-USB
//! - **协议层** (`protocol`): 类型安全的协议编码/解码
//! - **驱动层** (`driver`): IO 线程管理、状态同步、帧解析
//! - **客户端层** (`client`): 类型安全、易用的控制接口
//!
//! # 快速开始
//!
//! 大多数用户应该使用高层 API（客户端接口）：
//!
//! ```rust
//! use piper_sdk::prelude::*;
//! // 或
//! use piper_sdk::{Piper, MotionCommander, Observer};
//! ```
//!
//! 需要直接控制 CAN 帧或需要更高性能的用户可以使用驱动层：
//!
//! ```rust
//! use piper_sdk::driver::{Piper as Driver, PiperBuilder};
//! ```

// 内部模块结构（按功能划分 - 方案 B）
pub mod can;
pub mod protocol;
pub mod driver;
pub mod client;

// Prelude 模块
pub mod prelude;

// --- 用户以此为界 ---
// 以下是通过 Facade Pattern 提供的公共 API

// CAN 层常用类型
pub use can::{CanAdapter, CanError, PiperFrame};

// 协议层错误
pub use protocol::ProtocolError;

// 驱动层（高级用户使用）- 通过模块路径访问，避免命名冲突
// 注意：不直接导出 driver::Piper，因为与 client::Piper 冲突
// 用户可以通过 driver::Piper 或类型别名访问
// 注意：RobotError 已重命名为 DriverError，以保持与模块命名一致
pub use driver::{PiperBuilder, DriverError};

// 客户端层（普通用户使用）- 这是推荐的入口点
// 导出 client::Piper 为 Piper（这是大多数用户应该使用的）
pub use client::Piper;  // Type State Pattern 的状态机
pub use client::{
    MotionCommander, Observer,
    JointArray, Rad, Deg, NewtonMeter,
    // ... 其他常用类型（根据实际需要添加）
};

// 类型别名：为驱动层提供清晰的别名
pub type Driver = driver::Piper;  // 高级用户可以使用这个别名

// ==================== 向后兼容层 ====================

/// 向后兼容：robot 模块（已废弃）
///
/// 使用 `driver` 模块替代。`driver` 模块提供设备驱动功能（IO 线程管理、状态同步等）。
#[deprecated(note = "使用 driver 模块替代。driver 模块提供设备驱动功能（IO 线程管理、状态同步等）")]
pub mod robot {
    pub use crate::driver::*;

    // 类型别名，保持向后兼容
    #[deprecated(note = "使用 DriverError 替代")]
    pub use crate::driver::DriverError as RobotError;
}

/// 向后兼容：high_level 模块（已废弃）
///
/// 使用 `client` 模块替代。`client` 模块提供用户友好的客户端接口。
#[deprecated(note = "使用 client 模块替代。client 模块提供用户友好的客户端接口")]
pub mod high_level {
    pub use crate::client::*;
}
```

---

### 2.4 阶段 4: 向后兼容层

向后兼容层已在步骤 3.8 中添加到 `lib.rs`，采用 **Inline Module** 方式。

**设计说明**：
- 兼容层直接在 `lib.rs` 中内联定义，而不是创建独立的 `src/robot.rs` 和 `src/high_level.rs` 文件
- 这样可以保持文件树清爽，避免在 `src/` 下留下几乎空的兼容层文件
- 对于纯粹为了重导出（Re-export）和标记废弃的模块，Inline 方式是最干净的实现

**验证步骤**：

1. 确保所有旧模块路径仍然可用
2. `#[deprecated]` 标记正确
3. 迁移说明清晰

**验证命令**：
```bash
# 编译检查，确保没有破坏性变更
cargo build --all-features

# 检查 deprecation 警告
cargo build 2>&1 | grep -i "deprecated"

# 验证旧模块路径仍然可用（应该能编译通过）
cargo check --example backward_compatibility_test 2>&1 | head -20
```

---

### 2.5 阶段 5: 测试和验证

#### 步骤 5.1: 运行所有测试

```bash
# 运行单元测试
cargo test --lib

# 运行集成测试
cargo test --test '*'

# 运行所有测试（包括示例）
cargo test --all-features

# 运行示例代码
cargo run --example high_level_simple_move
cargo run --example high_level_gripper_control
```

#### 步骤 5.2: 更新示例代码

**需要更新的示例文件**：

1. **`examples/high_level_simple_move.rs`**
   ```rust
   // 更新前
   use piper_sdk::high_level::{
       control::TrajectoryPlanner,
       types::{JointArray, Rad},
   };

   // 更新后
   use piper_sdk::prelude::*;
   use piper_sdk::client::control::TrajectoryPlanner;
   ```

2. **`examples/high_level_gripper_control.rs`**
   - 类似更新

3. **其他示例文件**
   - 检查并更新所有使用旧模块路径的示例

#### 步骤 5.3: 验证向后兼容性

创建测试文件验证向后兼容性：

**文件**: `tests/backward_compatibility.rs`

```rust
#[test]
fn test_robot_module_still_works() {
    // 验证旧的 robot 模块仍然可用（但会显示 deprecation 警告）
    use piper_sdk::robot::PiperBuilder;
    // 应该能编译通过
}

#[test]
fn test_high_level_module_still_works() {
    // 验证旧的 high_level 模块仍然可用（但会显示 deprecation 警告）
    use piper_sdk::high_level::MotionCommander;
    // 应该能编译通过
}

#[test]
fn test_new_modules_work() {
    // 验证新模块可用
    use piper_sdk::driver::PiperBuilder;
    use piper_sdk::client::MotionCommander;
    // 应该能编译通过
}
```

---

### 2.6 阶段 6: 文档更新

#### 步骤 6.1: 更新 README.md

**需要更新的部分**：
1. 快速开始示例
2. 架构说明
3. 模块组织说明

**示例更新**：
```markdown
## 快速开始

```rust
use piper_sdk::prelude::*;

// 使用 Type State API（推荐）
let robot = Piper::connect(...)?;
let robot = robot.enable_mit_mode()?;

// 或使用驱动层 API（高级用户）
use piper_sdk::Driver;
let driver = Driver::new(...)?;
```
```

#### 步骤 6.2: 更新 API 文档

运行文档生成并检查：
```bash
cargo doc --all-features --open
```

检查：
- 所有模块文档正确
- 向后兼容说明清晰
- 迁移指南完整

#### 步骤 6.3: 编写迁移指南

**文件**: `docs/v0/MIGRATION_GUIDE.md`

**内容应包括**：
1. 模块重命名映射表
2. 代码迁移示例
3. 常见问题解答
4. 向后兼容性说明

---

## 3. 代码变更清单

### 3.1 文件移动清单

| 操作 | 源路径 | 目标路径 | 说明 |
|------|--------|---------|------|
| 移动目录 | `src/robot/` | `src/driver/` | 使用 `git mv` |
| 重命名文件 | `src/driver/robot_impl.rs` | `src/driver/piper.rs` | 使用 `git mv` |
| 移动目录（扁平化） | `src/high_level/` | `src/client/` | 使用 `git mv`，注意扁平化 |
| 重命名文件 | `src/client/motion_commander.rs` | `src/client/motion.rs` | 使用 `git mv` |
| 新建文件 | - | `src/prelude.rs` | 新建文件 |
| 内联模块 | - | `src/lib.rs`（兼容层） | 在 `lib.rs` 中内联定义 `robot` 和 `high_level` 模块 |

**重要说明**：
- 所有文件移动都使用 `git mv` 命令，以保留 Git 历史记录
- 兼容层（`robot` 和 `high_level` 模块）采用 **Inline Module** 方式，直接在 `lib.rs` 中定义，而不是创建独立的文件
- 这样可以保持文件树清爽，避免在 `src/` 下留下几乎空的兼容层文件

### 3.2 类型重命名清单

| 旧名称 | 新名称 | 位置 |
|--------|--------|------|
| `RobotError` | `DriverError` | `src/driver/error.rs` |
| `robot::Piper` | `driver::Piper` | 所有引用处 |
| `high_level::Piper` | `client::Piper` | 所有引用处 |

### 3.3 模块引用更新清单

| 旧引用 | 新引用 | 影响文件 |
|--------|--------|---------|
| `use crate::robot::` | `use crate::driver::` | 所有文件 |
| `use crate::high_level::` | `use crate::client::` | 所有文件 |
| `robot::Piper` | `driver::Piper` | client 模块所有文件 |

---

## 4. 测试计划

### 4.1 单元测试

- [ ] 运行所有现有单元测试
- [ ] 确保所有测试通过
- [ ] 添加新模块的单元测试（如有需要）

### 4.2 集成测试

- [ ] 运行所有集成测试
- [ ] 测试向后兼容性
- [ ] 测试新 API 的使用

### 4.3 示例代码测试

- [ ] 更新所有示例代码
- [ ] 确保所有示例可以编译
- [ ] 确保所有示例可以运行（如有硬件）

### 4.4 文档测试

- [ ] 运行 `cargo doc` 确保文档生成成功
- [ ] 检查所有文档链接
- [ ] 验证代码示例在文档中正确

---

## 5. 风险评估和缓解措施

### 5.1 风险清单

| 风险 | 影响 | 概率 | 缓解措施 |
|------|------|------|---------|
| 编译错误 | 高 | 中 | 分阶段实施，每阶段验证编译 |
| 测试失败 | 高 | 中 | 保持测试套件完整，及时修复 |
| 向后兼容性破坏 | 高 | 低 | 使用兼容层，充分测试 |
| 文档不完整 | 中 | 中 | 文档更新作为独立阶段 |
| 用户代码需要大量修改 | 中 | 低 | 提供清晰的迁移指南 |

### 5.2 回滚计划

如果重构过程中遇到严重问题：

1. **立即回滚**：
   ```bash
   git checkout backup-before-module-refactor
   git branch -D refactor/module-organization
   ```

2. **部分回滚**：
   - 保留已完成的工作
   - 修复问题后继续

3. **渐进式发布**：
   - 先发布带兼容层的版本
   - 在下一个版本中移除兼容层

---

## 6. 验收标准

### 6.1 功能验收

- [ ] 所有现有功能正常工作
- [ ] 所有测试通过
- [ ] 所有示例代码可以编译和运行

### 6.2 代码质量验收

- [ ] 代码编译无警告（deprecation 警告除外）
- [ ] 所有模块文档完整
- [ ] 代码符合 Rust 风格指南

### 6.3 文档验收

- [ ] README 更新完整
- [ ] API 文档生成正确
- [ ] 迁移指南完整清晰

### 6.4 向后兼容性验收

- [ ] 旧模块路径仍然可用（带 deprecation 警告）
- [ ] 迁移指南清晰
- [ ] 用户代码可以平滑迁移

---

## 7. 发布计划

### 7.1 版本规划

- **v0.1.x**：当前版本（使用旧模块结构）
- **v0.2.0**：重构版本（新模块结构 + 向后兼容层）
- **v0.3.0 或 v1.0.0**：移除向后兼容层

### 7.2 发布检查清单

- [ ] 所有测试通过
- [ ] 文档完整
- [ ] CHANGELOG 更新
- [ ] 版本号更新
- [ ] 向后兼容性验证
- [ ] 迁移指南发布

---

## 8. 后续工作

### 8.1 短期（v0.2.0 发布后）

- 收集用户反馈
- 修复迁移过程中的问题
- 完善迁移指南

### 8.2 中期（v0.3.0 或 v1.0.0）

- 移除向后兼容层
- 清理废弃代码
- 最终文档更新

### 8.3 长期

- 持续监控用户使用情况
- 根据反馈优化 API
- 保持文档更新

---

## 附录 A: 快速参考

### A.1 模块映射表

| 旧模块 | 新模块 | 说明 |
|--------|--------|------|
| `robot` | `driver` | 设备驱动层 |
| `high_level` | `client` | 客户端接口层 |
| - | `prelude` | 便捷导入模块 |

### A.2 类型映射表

| 旧类型 | 新类型 | 说明 |
|--------|--------|------|
| `robot::Piper` | `driver::Piper` | 驱动层 API |
| `high_level::Piper` | `client::Piper` | 客户端 API |
| `RobotError` | `DriverError` | 错误类型 |

### A.3 常用导入模式

```rust
// 推荐：使用 prelude
use piper_sdk::prelude::*;

// 普通用户：直接导入
use piper_sdk::{Piper, MotionCommander, Observer};

// 高级用户：驱动层
use piper_sdk::driver::{Piper as Driver, PiperBuilder};
// 或使用别名
use piper_sdk::Driver;
```

---

## 附录 B: 常见问题

### B.1 为什么需要重构？

- 提高代码可维护性
- 符合 Rust 社区习惯
- 改善 API 清晰度

### B.2 重构会影响现有代码吗？

- 短期：不会（通过兼容层）
- 长期：需要迁移（但提供清晰的迁移指南）

### B.3 如何迁移我的代码？

参考 `docs/v0/MIGRATION_GUIDE.md` 获取详细指南。

### B.4 什么时候移除兼容层？

计划在 v0.3.0 或 v1.0.0 中移除，具体时间取决于用户反馈。

---

**文档结束**

