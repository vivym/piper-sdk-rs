# piper-can Features 配置调研报告

## 概述

本报告分析了 `piper-can` crate 的当前依赖配置问题，并提出了改进方案。主要问题在于：**features 定义存在但未真正启用**，且依赖关系未正确使用 feature flags 控制。

## 当前配置分析

### 1. crates/piper-can/Cargo.toml 当前配置

```toml
[features]
default = []
socketcan = []  # Linux: 由 target cfg 自动包含
gs_usb = []     # macOS/Windows: 由 target cfg 自动包含
mock = []       # 测试: 完全移除硬件依赖
serde = ["dep:serde", "piper-protocol/serde"]

[dependencies]
piper-protocol = { workspace = true }
thiserror = { workspace = true }
tracing = { workspace = true }
bytes = { workspace = true }
serde = { workspace = true, optional = true }

# 平台特定依赖（不标记为 optional，由 target cfg 自动包含）
[target.'cfg(target_os = "linux")'.dependencies]
socketcan = { workspace = true }
nix = { workspace = true }
libc = { workspace = true }

[target.'cfg(not(target_os = "linux"))'.dependencies]
rusb = { workspace = true, features = ["vendored"] }
libc = { workspace = true }
```

### 2. crates/piper-can/src/lib.rs 当前编译控制

```rust
#[cfg(target_os = "linux")]
pub mod socketcan;

#[cfg(target_os = "linux")]
pub use socketcan::SocketCanAdapter;

pub mod gs_usb;
pub use gs_usb::GsUsbCanAdapter;
```

### 3. 关键问题

#### 问题 1: Features 定义但未生效

- **`socketcan` feature**: 定义为空数组，但未实际控制依赖
- **`gs_usb` feature**: 定义为空数组，但未实际控制依赖
- **`mock` feature**: 定义为空数组，但没有对应的实现逻辑

当前使用 `target_cfg` 而非 feature flags：
- Linux: 通过 `cfg(target_os = "linux")` 自动包含 socketcan
- 非 Linux: 通过 `cfg(not(target_os = "linux"))` 自动包含 rusb

#### 问题 2: 所有平台依赖都会被编译

**在 Linux 上编译时：**
```bash
cargo build
# 实际编译的依赖：
# - socketcan (通过 target.'cfg(target_os = "linux")')
# - rusb (通过 target.'cfg(not(target_os = "linux"))') - 不会编译
# ✓ 正确
```

**在 Windows/macOS 上编译时：**
```bash
cargo build
# 实际编译的依赖：
# - socketcan - 不会编译
# - rusb (通过 target.'cfg(not(target_os = "linux"))')
# ✓ 正确
```

**结论：当前的平台特定依赖配置是正确的。**

#### 问题 3: Features 无法被下游 crates 控制

由于依赖不在 `[dependencies]` 主节中定义（仅在 `target_cfg` 中），下游 crates 无法通过 features 显式启用/禁用。

**影响：**
- 无法在 Linux 上构建只包含 gs_usb 的版本
- 无法在非 Linux 上构建"mock"版本进行测试
- 无法通过 `--no-default-features` 进行最小化构建

### 4. 使用 piper-can 的下游 crates

| Crate | 依赖声明 | Features 使用 | 分析 |
|-------|---------|--------------|------|
| piper-driver | `piper-can = { workspace = true }` | 无 | ✓ 正确（driver 自动选择后端） |
| piper-sdk | `piper-can = { workspace = true }` | `serde = ["piper-can/serde"]` | ✓ 正确 |
| gs_usb_daemon | 无直接依赖 | - | 通过 piper-driver 间接依赖 |
| piper-cli | 无直接依赖 | - | 通过 piper-sdk 间接依赖 |

### 5. 代码中的后端选择逻辑

**piper-driver/src/builder.rs:**
```rust
#[cfg(target_os = "linux")]
use piper_can::SocketCanAdapter;
use piper_can::gs_usb::GsUsbCanAdapter;

pub enum DriverType {
    Auto,        // 自动选择
    SocketCan,   // 强制 SocketCAN（仅 Linux）
    GsUsb,       // 强制 GS-USB（所有平台）
}
```

**piper-client/src/builder.rs:**
```rust
// 使用平台默认值
#[cfg(target_os = "linux")]
{
    Some("can0".to_string())
}
#[cfg(not(target_os = "linux"))]
{
    None // macOS/Windows: 自动选择第一个 GS-USB 设备
}
```

**结论：后端选择逻辑完全基于 `cfg(target_os)`，不依赖 feature flags。**

## 改进方案

### 方案 A: 完全使用 target_cfg（当前方案，推荐）

**优点：**
- ✅ 简单：无需修改现有代码
- ✅ 自动：平台自动选择合适的后端
- ✅ 零配置：用户无需关心 feature flags

**缺点：**
- ❌ 无法禁用特定后端（例如 Linux 上禁用 SocketCAN）
- ❌ 无法构建"mock"版本进行 CI 测试

**建议：保持现状，仅做文档更新。**

### 方案 B: 混合模式（target_cfg + optional features）

#### 核心设计原则

1. **显式 Feature 优先**：用户显式指定的 features 优先于自动推导
2. **增量式设计**：允许 features 共存，由运行时逻辑决定优先级
3. **正交性**：每个 feature 独立启用，避免隐式互斥

#### Cargo.toml 配置

```toml
[features]
default = ["auto-backend"]  # 默认启用自动后端选择

# 自动后端选择（通过 target_cfg）
auto-backend = []

# 显式后端选择（可选，增量式设计，允许共存）
socketcan = ["dep:socketcan", "dep:nix"]
gs_usb = ["dep:rusb"]

# Mock 模式（无硬件依赖，优先级最高）
mock = []

# Serde 序列化支持
serde = ["dep:serde", "piper-protocol/serde"]

[dependencies]
piper-protocol = { workspace = true }
thiserror = { workspace = true }
tracing = { workspace = true }
bytes = { workspace = true }
serde = { workspace = true, optional = true }

# ⚠️ 重要：平台特定依赖必须标记为 optional = true
# 这样才能被 features 正确控制
# 同时也保留 target_cfg 的平台过滤作用
[target.'cfg(target_os = "linux")'.dependencies]
socketcan = { workspace = true, optional = true }
nix = { workspace = true, optional = true }
libc = { workspace = true }

[target.'cfg(not(target_os = "linux"))'.dependencies]
rusb = { workspace = true, optional = true, features = ["vendored"] }
libc = { workspace = true }

# docs.rs 配置：生成所有平台的文档
[package.metadata.docs.rs]
all-features = true
rustdoc-args = ["--cfg", "docsrs"]
```

#### ⚠️ Weak Dependencies (`dep:` 语法) 说明

使用 `dep:` 语法是 Rust 的最佳实践，但需要配套条件：

```toml
# ✅ 正确：依赖声明时 optional = true
[dependencies]
socketcan = { workspace = true, optional = true }

[features]
socketcan = ["dep:socketcan", "dep:nix"]  # 可选依赖使用 dep:
```

**关键规则：**
1. 使用 `dep:` 时，该依赖**必须**在 `[dependencies]` 中标记为 `optional = true`
2. 对于 `target.'cfg(...)'.dependencies`，在非目标平台启用对应的 feature **可能导致编译失败**
3. 示例：在 Windows 上启用 `socketcan` feature 会导致失败，因为 `nix` crate 不支持 Windows

#### ⚠️ 跨平台依赖的优雅降级

**问题：** 如果下游 crate 在 Windows 上不小心启用了 `socketcan` feature，会遇到编译错误（`nix` crate 不支持 Windows）。

**解决方案：** `target_cfg` + `optional` 的组合提供了双重保护：

```toml
# 双重保护机制：
# 1. target_cfg 过滤：Windows 平台不会链接 socketcan/nix
# 2. optional 标记：允许 feature 显式控制（仅在目标平台生效）

[target.'cfg(target_os = "linux")'.dependencies]
socketcan = { workspace = true, optional = true }
nix = { workspace = true, optional = true }
```

**行为分析：**

| 平台 | Feature 组合 | 行为 | 原因 |
|-----|-------------|------|------|
| Linux | `socketcan` | ✅ 编译成功 | `nix` 可用 |
| Linux | `default` | ✅ 编译成功 | `auto-backend` + `target_os = "linux"` 启用 |
| Windows | `socketcan` | ❌ 编译失败 | `nix` crate 不支持 Windows |
| Windows | `default` | ✅ 编译成功 | `auto-backend` + `target_os != "linux"` 启用 gs_usb |

**文档标记技巧（可选）：**

如果希望更明确的跨平台警告，可以在 `lib.rs` 中添加编译时检查：

```rust
// 在 lib.rs 顶部添加（可选）
#[cfg(all(
    feature = "socketcan",
    not(target_os = "linux")
))]
compile_error!(
    "The 'socketcan' feature is only supported on Linux. \
     Please use the default features or 'gs_usb' feature on this platform."
);
```

#### lib.rs 编译逻辑（改进版）

```rust
// SocketCAN (Linux only)
// 优先级：mock 优先级最高，然后是显式 feature，最后是 auto-backend
#[cfg(all(
    not(feature = "mock"),                          // ⚠️ 确保 mock 模式下彻底禁用硬件
    any(
        feature = "socketcan",                      // 显式启用
        all(feature = "auto-backend", target_os = "linux")  // 自动推导
    )
))]
pub mod socketcan;

#[cfg(all(
    not(feature = "mock"),
    any(
        feature = "socketcan",
        all(feature = "auto-backend", target_os = "linux")
    )
))]
pub use socketcan::SocketCanAdapter;

// GS-USB (所有平台)
// 优先级：mock 优先级最高，然后是显式 feature，最后是 auto-backend
#[cfg(all(
    not(feature = "mock"),                          // mock 模式下禁用
    any(
        feature = "gs_usb",                         // 显式启用
        feature = "auto-backend"                    // 自动推导
    )
))]
pub mod gs_usb;

#[cfg(all(
    not(feature = "mock"),
    any(
        feature = "gs_usb",
        feature = "auto-backend"
    )
))]
pub use gs_usb::GsUsbCanAdapter;

// Mock Adapter (用于测试)
#[cfg(feature = "mock")]
pub mod mock;

#[cfg(feature = "mock")]
pub use mock::MockCanAdapter;
```

#### mock 模块实现结构

```rust
// crates/piper-can/src/mock.rs
//! Mock CAN 适配器（用于测试）

use crate::{CanAdapter, CanError, PiperFrame};
use std::time::Duration;

/// Mock CAN 适配器（无硬件依赖）
pub struct MockCanAdapter {
    frames: Vec<PiperFrame>,
}

impl MockCanAdapter {
    pub fn new() -> Self {
        Self { frames: Vec::new() }
    }

    /// 注入测试帧
    pub fn inject(&mut self, frame: PiperFrame) {
        self.frames.push(frame);
    }
}

impl Default for MockCanAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl CanAdapter for MockCanAdapter {
    fn send(&mut self, frame: PiperFrame) -> Result<(), CanError> {
        // 模拟发送：将帧放入接收队列（回环）
        self.frames.push(frame);
        Ok(())
    }

    fn receive(&mut self) -> Result<PiperFrame, CanError> {
        self.frames.pop()
            .ok_or(CanError::Timeout)
    }

    fn set_receive_timeout(&mut self, _timeout: Duration) {
        // Mock 实现：无操作
    }
}
```

#### 架构设计注意事项

**1. `CanAdapter` Trait 必须公开**

为了支持方案 B 的扩展性（包括 Mock Adapter 和用户自定义 Adapter），`CanAdapter` trait 必须声明为 `pub`：

```rust
// crates/piper-can/src/lib.rs
/// CAN 适配器统一接口
///
/// # 实现自定义 Adapter
///
/// 如果需要实现自定义 Adapter（例如虚拟总线、录制回放等）：
///
/// ```rust
/// use piper_can::{CanAdapter, PiperFrame, CanError};
///
/// pub struct MyCustomAdapter;
///
/// impl CanAdapter for MyCustomAdapter {
///     fn send(&mut self, frame: PiperFrame) -> Result<(), CanError> {
///         // 自定义实现
///         Ok(())
///     }
///
///     fn receive(&mut self) -> Result<PiperFrame, CanError> {
///         // 自定义实现
///         Ok(PiperFrame::new_standard(0x123, &[0]))
///     }
/// }
/// ```
pub trait CanAdapter {
    fn send(&mut self, frame: PiperFrame) -> Result<(), CanError>;
    fn receive(&mut self) -> Result<PiperFrame, CanError>;
    // ... 其他方法
}
```

**当前实现检查：**

```bash
# 验证 CanAdapter 是否为 pub
grep -n "pub trait CanAdapter" crates/piper-can/src/lib.rs

# 预期输出：
# 100:pub trait CanAdapter {
```

如果 `CanAdapter` 不是 `pub`，则在实施方案 B 之前需要修改可见性。

**2. 依赖注入兼容性**

当前的 `piper-driver` 和 `piper-client` 已经通过泛型或 trait object 支持任意 `CanAdapter` 实现，无需修改即可与 Mock Adapter 或自定义 Adapter 配合使用：

```rust
// piper-driver 已经支持任意 CanAdapter 实现
let piper = Piper::new_dual_thread(mock_adapter, pipeline_config)?;
```

**优点：**
- ✅ 保持自动平台选择（通过 `auto-backend` feature）
- ✅ 允许显式控制（通过显式 features）
- ✅ 支持构建"mock"版本（用于 CI 测试）
- ✅ **增量式设计**：允许多个 backend 同时编译，由运行时逻辑选择
- ✅ **显式优先**：用户显式指定的 features 优先于自动推导

**缺点：**
- ❌ 增加配置复杂度
- ❌ 需要修改代码（添加 `#[cfg(feature = "...")]`）
- ⚠️ **跨平台风险**：在非 Linux 平台启用 `socketcan` feature 会导致编译失败

#### 功能正交性说明

**设计理念：Features 应该是增量的（Additive），而非互斥的**

当前设计允许 `socketcan` 和 `gs_usb` 同时启用，由 `DriverType` 枚举在运行时决定优先级：

```rust
// piper-driver/src/builder.rs
pub enum DriverType {
    Auto,       // 自动探测（Linux: 优先 SocketCAN，fallback 到 GS-USB）
    SocketCan,  // 强制 SocketCAN（仅 Linux）
    GsUsb,      // 强制 GS-USB（所有平台）
}
```

**为什么允许共存？**
1. **灵活性**：Linux 用户可能需要同时使用两种后端（例如：SocketCAN 用于生产，GS-USB 用于调试）
2. **统一构建**：下游 crates（如 `piper-driver`）无需根据平台切换 features
3. **运行时切换**：`PiperBuilder::with_driver_type()` 可以在运行时选择后端，无需重新编译

**Feature 组合示例：**

| Feature 组合 | 行为 | 使用场景 |
|------------|------|---------|
| `default` | Linux: SocketCAN + GS-USB<br>其他: GS-USB | 生产环境（推荐） |
| `gs_usb`, `default-features = false` | 仅 GS-USB | 交叉平台测试 |
| `socketcan`, `gs_usb` | Linux: 两者都启用<br>其他: 编译失败（仅 Linux） | 高级用例 |
| `mock`, `default-features = false` | 仅 Mock Adapter | CI 测试 |

### 方案 C: 完全使用 features（不推荐）

**问题：**
- 需要修改所有下游 crates
- 用户需要手动选择平台特定的 features
- 容易配置错误

## 推荐方案

### 短期（保持现状）

**1. 更新文档说明平台自动选择**

在 `crates/piper-can/README.md` 中添加：

```markdown
## 平台支持

### Linux
- **默认后端**: SocketCAN (通过 `cfg(target_os = "linux")` 自动启用)
- **可选后端**: GS-USB (可通过 `PiperBuilder::with_driver_type(DriverType::GsUsb)` 切换)

### macOS / Windows
- **唯一后端**: GS-USB (通过 `cfg(not(target_os = "linux"))` 自动启用)

### 无需手动配置 features
平台特定的依赖会根据 `target_os` 自动选择，无需在 `Cargo.toml` 中手动指定 features。
```

**2. 移除无用的 features 定义**

```toml
[features]
default = []
# 移除以下未使用的 features
# socketcan = []  # 由 target_cfg 自动控制
# gs_usb = []     # 由 target_cfg 自动控制
# mock = []       # 未实现

serde = ["dep:serde", "piper-protocol/serde"]
```

### 长期（方案 B：混合模式）

如果未来需要以下功能：
1. 在 Linux 上构建"纯 gs_usb"版本（不含 SocketCAN）
2. 在 CI 中构建"mock"版本进行测试
3. 在 docs.rs 上生成完整的跨平台文档

则实施**方案 B**（详见"方案 B"章节）：

#### 实施步骤

**步骤 1: 修改 `crates/piper-can/Cargo.toml`**

使用方案 B 中的完整配置（参见上文），特别注意：
- 所有平台特定依赖必须标记 `optional = true`
- 添加 `[package.metadata.docs.rs]` 配置

**步骤 2: 修改 `crates/piper-can/src/lib.rs`**

使用方案 B 中的改进版编译逻辑（参见上文），确保：
- `mock` feature 优先级最高（**排除所有硬件依赖**，包括 socketcan 和 gs_usb）
- 显式 features 优先于 `auto-backend`
- 条件逻辑清晰，避免冲突

**前置检查：验证 `CanAdapter` 可见性**

```bash
# 检查 CanAdapter 是否为 pub
grep -A 5 "trait CanAdapter" crates/piper-can/src/lib.rs

# 预期输出：
# pub trait CanAdapter {
#     fn send(&mut self, frame: PiperFrame) -> Result<(), CanError>;
#     fn receive(&mut self) -> Result<PiperFrame, CanError>;
#     ...
# }

# 如果缺少 pub 关键字，需要修改：
# pub trait CanAdapter {  // 添加 pub
#     ...
# }
```

如果 `CanAdapter` 不是 `pub`，则需要先修改可见性（参见"架构设计注意事项"）。

**步骤 3: 创建 `crates/piper-can/src/mock.rs`**

实现 Mock Adapter（参见上文方案 B 的代码示例），确保：
- 实现 `CanAdapter` trait
- 提供 `inject()` 方法用于测试
- 无硬件依赖，可在任何平台运行

**步骤 4: 更新文档**

创建 `crates/piper-can/README.md`：

```markdown
# piper-can

CAN 硬件抽象层，提供统一的 CAN 接口。

## Features

- `auto-backend` (default): 根据平台自动选择后端
  - Linux: SocketCAN + GS-USB
  - macOS/Windows: GS-USB

- `socketcan`: 强制启用 SocketCAN（仅 Linux）
  - ⚠️ 在非 Linux 平台启用此 feature 会导致编译失败

- `gs_usb`: 强制启用 GS-USB（所有平台）

- `mock`: 禁用所有硬件依赖（用于 CI 测试）
  - 优先级最高，会禁用所有硬件后端

- `serde`: 启用 Serde 序列化支持

## Feature 组合示例

| Feature 组合 | 行为 | 使用场景 |
|------------|------|---------|
| `default` | Linux: SocketCAN + GS-USB<br>其他: GS-USB | 生产环境（推荐） |
| `gs_usb`, `default-features = false` | 仅 GS-USB | 交叉平台测试 |
| `mock`, `default-features = false` | 仅 Mock Adapter | CI 测试 |

## 使用示例

# 默认构建（自动后端选择）
piper-can = "0.0.3"

# Linux 上只使用 GS-USB（移除 SocketCAN 依赖）
piper-can = { version = "0.0.3", features = ["gs_usb"], default-features = false }

# CI 测试（无硬件依赖）
piper-can = { version = "0.0.3", features = ["mock"], default-features = false }

# 启用 Serde 序列化
piper-can = { version = "0.0.3", features = ["serde"] }
```

**步骤 5: 添加 docs.rs 配置**

在 `crates/piper-can/Cargo.toml` 中添加（已在步骤 1 完成）：

```toml
[package.metadata.docs.rs]
all-features = true
rustdoc-args = ["--cfg", "docsrs"]
```

这将确保 docs.rs 生成的文档包含所有平台的 API，而不仅仅是构建平台的文档。

**步骤 6: 更新 CI 配置**

在 `.github/workflows/test.yml` 中添加 Mock 测试：

```yaml
jobs:
  test-mock:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v3
      - uses: dtolnay/rust-toolchain@stable
      - name: Test mock backend
        run: |
          cargo test --package piper-can --features mock,default-features=false
```

## 测试建议

### 1. 验证当前配置（方案 A）

```bash
# Linux: 验证只编译 socketcan
cargo build --package piper-can 2>&1 | grep -E "Compiling socketcan|Compiling rusb"

# macOS/Windows: 验证只编译 rusb
cargo build --package piper-can 2>&1 | grep -E "Compiling socketcan|Compiling rusb"
```

### 2. 验证混合模式（方案 B）

```bash
# 测试默认功能（auto-backend）
cargo build --package piper-can --features auto-backend

# Linux: 测试只启用 socketcan（移除 gs_usb）
cargo build --package piper-can --features "socketcan" --no-default-features

# 测试只启用 gs_usb（移除 socketcan）
cargo build --package piper-can --features "gs_usb" --no-default-features

# 测试 mock 模式（无硬件依赖）
cargo build --package piper-can --features "mock" --no-default-features

# 测试同时启用 socketcan 和 gs_usb（增量式设计）
cargo build --package piper-can --features "socketcan,gs_usb"

# 测试 docs.rs 配置（生成完整文档）
RUSTDOCFLAGS="--cfg docsrs" cargo doc --package piper-can --all-features --no-deps --open
```

### 3. 跨平台兼容性测试（方案 B）

```bash
# ⚠️ 这个命令在 Windows/macOS 上会失败（预期行为）
# cargo build --package piper-can --features "socketcan" --no-default-features

# 错误示例：
# error: target platform not found for crate `nix`
# --> 因为 nix crate 只支持 Linux
```

### 4. Feature 优先级测试（方案 B）

```bash
# 测试 mock 优先级最高（即使启用 auto-backend，mock 也会禁用硬件）
cargo build --package piper-can --features "auto-backend,mock"

# 预期结果：只有 MockCanAdapter 可用，没有 SocketCanAdapter 和 GsUsbCanAdapter

# 验证编译结果：
cargo doc --package piper-can --features "auto-backend,mock" --no-deps --open
# 检查文档中不应出现 SocketCanAdapter 和 GsUsbCanAdapter
```

### 5. 编译时错误检查（方案 B 可选）

如果在 `lib.rs` 中添加了 `compile_error!` 宏（参见"跨平台依赖的优雅降级"），可以验证其工作：

```bash
# 测试在非 Linux 平台启用 socketcan feature 的错误提示
# （需要在 Windows/macOS 上运行，或使用 cross-compilation）

# 方法 1: 使用 cargo check（不需要完整编译）
cargo check --package piper-can --features "socketcan" --target x86_64-pc-windows-msvc

# 预期输出：
# error: The 'socketcan' feature is only supported on Linux.
#        Please use the default features or 'gs_usb' feature on this platform.

# 方法 2: 在 Windows 本地测试（如果有 Windows 环境）
# cargo build --package piper-can --features "socketcan"
```

### 6. CanAdapter 可见性验证（方案 B）

```bash
# 验证 CanAdapter trait 是否为 pub（支持自定义 Adapter）
grep -n "^pub trait CanAdapter" crates/piper-can/src/lib.rs

# 预期输出：
# 100:pub trait CanAdapter {

# 如果输出为空，或者缺少 pub 关键字，需要修改：
# 将 "trait CanAdapter" 改为 "pub trait CanAdapter"
```

## 总结与建议

### 方案对比

| 方案 | 优点 | 缺点 | 推荐度 | 适用场景 |
|-----|------|------|--------|---------|
| A (现状) | 简单、零配置、自动平台选择 | 灵活性低、无法 mock 测试 | ⭐⭐⭐⭐⭐ | 短期/生产环境 |
| B (混合) | 自动 + 灵活、支持 mock、增量式设计 | 需修改代码、跨平台风险 | ⭐⭐⭐⭐ | 长期/CI 测试 |
| C (纯 features) | 完全可控 | 复杂、易错、用户负担重 | ⭐ | 不推荐 |

### 核心设计原则

#### 1. 显式 Feature 优先

用户显式指定的 features 优先于自动推导（`auto-backend`）：

```rust
// 优先级：feature = "socketcan" > feature = "auto-backend" + target_os = "linux"
#[cfg(any(
    feature = "socketcan",                          // 显式优先
    all(feature = "auto-backend", target_os = "linux")
))]
pub mod socketcan;
```

#### 2. 增量式设计（Additive Features）

允许 features 共存，由运行时逻辑决定优先级，而非编译时互斥：

```rust
// ✅ 允许同时启用 socketcan 和 gs_usb
piper-can = { version = "0.0.3", features = ["socketcan", "gs_usb"] }

// 运行时通过 DriverType 选择
let piper = PiperBuilder::new()
    .with_driver_type(DriverType::SocketCan)  // 或 GsUsb, Auto
    .build()?;
```

#### 3. Mock 优先级最高

Mock feature 应该完全禁用硬件依赖，用于 CI 测试：

```rust
#[cfg(all(
    not(feature = "mock"),  // mock 模式下禁用所有硬件后端
    any(feature = "gs_usb", feature = "auto-backend")
))]
pub mod gs_usb;
```

#### 4. Weak Dependencies 规范

使用 `dep:` 语法时，必须配套 `optional = true`：

```toml
# ✅ 正确
[dependencies]
socketcan = { version = "3.5", optional = true }

[features]
socketcan = ["dep:socketcan"]

# ❌ 错误（会编译失败）
[dependencies]
socketcan = "3.5"  # 缺少 optional = true

[features]
socketcan = ["dep:socketcan"]  # 错误：socketcan 不是 optional
```

### 当前建议

#### 短期（立即执行）

1. **移除无用的 features 定义**：

```toml
# crates/piper-can/Cargo.toml
[features]
default = []
# 移除以下未使用的 features：
# socketcan = []  # 由 target_cfg 自动控制
# gs_usb = []     # 由 target_cfg 自动控制
# mock = []       # 未实现

serde = ["dep:serde", "piper-protocol/serde"]
```

2. **更新文档**：

在 `crates/piper-can/README.md` 中说明平台自动选择逻辑（参见"方案 A"章节）。

#### 长期（按需执行）

如果需要以下功能，则实施方案 B：
1. ✅ CI 中需要"mock"模式进行无硬件测试
2. ✅ 需要在 Linux 上构建"纯 gs_usb"版本（减少依赖）
3. ✅ 需要在 docs.rs 上生成完整的跨平台文档

实施步骤参见"方案 B → 实施步骤"章节。

### 风险提示

#### 1. 跨平台 Feature 限制

```toml
# ⚠️ 警告：在非 Linux 平台启用 socketcan feature 会失败
# Windows/macOS 用户不要使用：
piper-can = { features = ["socketcan"] }  # ❌ 编译失败（nix crate 不支持）

# ✅ 正确做法：依赖 auto-backend 的自动选择
piper-can = { version = "0.0.3" }  # 自动选择平台合适的后端
```

#### 2. Feature 组合验证

建议在 CI 中添加 feature 组合测试：

```yaml
# .github/workflows/test.yml
test-features:
  runs-on: ${{ matrix.os }}
  strategy:
    matrix:
      os: [ubuntu-latest, macos-latest, windows-latest]
      features: ["default", "gs_usb", "mock"]
  steps:
    - uses: actions/checkout@v3
    - uses: dtolnay/rust-toolchain@stable
    - name: Test features
      run: cargo test --package piper-can --features "${{ matrix.features }}"
```

### 参考资源

- [RFC 3013: Weak Dependency Features](https://rust-lang.github.io/rfcs/3013-weak-dependency-features.html)
- [Cargo Features: The Definitive Guide](https://doc.rust-lang.org/cargo/reference/features.html)
- [docs.rs: Building Documentation](https://docs.rs/about/builds)
- [The Cargo Book: Conditional Compilation](https://doc.rust-lang.org/cargo/reference/specifying-dependencies.html#platform-specific-dependencies)

---

## 最终检查清单

在实施方案 B 之前，请确认以下所有检查项：

### ✅ 配置检查

- [ ] `socketcan`, `nix`, `rusb` 依赖已标记为 `optional = true`
- [ ] `auto-backend` 添加到 `default` features
- [ ] `[package.metadata.docs.rs]` 配置已添加
- [ ] Weak Dependencies 使用 `dep:` 语法

### ✅ 代码检查

- [ ] **socketcan 和 gs_usb 模块都添加了 `not(feature = "mock")`**（关键！）
- [ ] `CanAdapter` trait 已声明为 `pub`
- [ ] Mock Adapter 实现了完整的 `CanAdapter` trait
- [ ] （可选）添加了跨平台 `compile_error!` 检查

### ✅ 文档检查

- [ ] README.md 包含 feature 组合示例表
- [ ] 文档说明了 `socketcan` feature 的平台限制
- [ ] 文档说明了 mock 优先级最高的行为

### ✅ 测试检查

- [ ] Mock 模式编译测试通过
- [ ] Feature 优先级测试通过（`auto-backend + mock`）
- [ ] （可选）跨平台错误检查测试通过

### ✅ 关键陷阱提醒

```toml
# ❌ 错误：忘记给 socketcan 添加 not(feature = "mock")
#[cfg(any(
    feature = "socketcan",
    all(feature = "auto-backend", target_os = "linux")
))]
pub mod socketcan;  // 在 mock 模式下仍会被编译！

# ✅ 正确：彻底排除硬件依赖
#[cfg(all(
    not(feature = "mock"),  // 必须添加！
    any(
        feature = "socketcan",
        all(feature = "auto-backend", target_os = "linux")
    )
))]
pub mod socketcan;
```

```toml
# ❌ 错误：CanAdapter 不是 pub
trait CanAdapter {  // 缺少 pub，下游无法实现自定义 Adapter
    ...
}

# ✅ 正确：公开 trait
pub trait CanAdapter {
    ...
}
```

```toml
# ❌ 错误：optional 依赖忘记使用 dep: 语法
[features]
socketcan = ["socketcan"]  // 错误：未使用 dep:

# ✅ 正确：Weak Dependencies 规范
[features]
socketcan = ["dep:socketcan", "dep:nix"]
```

---

**报告版本**: v2.0（最终修订版）
**最后更新**: 2026-02-02
**状态**: ✅ 已达到生产就绪状态
