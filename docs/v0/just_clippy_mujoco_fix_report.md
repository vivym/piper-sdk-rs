# Just Clippy MuJoCo 修复报告

**日期**: 2025-02-02
**状态**: ✅ 已完成

---

## 📋 问题分析

### 原始问题

用户发现 `just clippy` 命令缺少 MuJoCo 环境设置，导致编译失败：

```bash
$ just clippy
error: MUJOCO_DYNAMIC_LINK_DIR not set
```

### 根本原因

1. **`just clippy` 没有 MuJoCo 设置**: clippy 触发编译 piper-physics，需要 MuJoCo 环境
2. **`--all-features` 导致 mock 冲突**: 启用 mock feature 后禁用 socketcan/gs_usb
3. **pre-commit hook 也没有 MuJoCo 设置**: 同样会失败
4. **clone_on_copy 警告**: `PiperFrame` 实现 Copy trait，不需要 clone()
5. **mock feature 未定义**: piper-driver 没有定义 mock feature

---

## ✅ 解决方案

### 1. 更新 `just clippy` 添加 MuJoCo 设置

**文件**: `justfile`

**修改前**:
```just
clippy:
    cargo clippy --all-targets --all-features -- -D warnings
```

**修改后**:
```just
clippy:
    #!/usr/bin/env bash
    eval "$(just _mujoco_download)"
    if [ -n "${MUJOCO_DYNAMIC_LINK_DIR:-}" ]; then
        >&2 echo "✓ Using MuJoCo from: $MUJOCO_DYNAMIC_LINK_DIR"
        case "$(uname -s)" in
            Linux*)
                >&2 echo "✓ RPATH embedded for Linux"
                ;;
            Darwin*)
                >&2 echo "✓ Framework linked for macOS"
                ;;
        esac
    fi
    cargo clippy --workspace --all-targets -- -D warnings
```

**关键改动**:
- ✅ 添加 shebang (`#!/usr/bin/env bash`)
- ✅ 调用 `just _mujoco_download` 设置环境变量
- ✅ 添加信息输出（与 build/test 一致）
- ✅ **移除 `--all-features`**（避免 mock 冲突）
- ✅ 改用 `--workspace`（更明确）

---

### 2. 更新 `just check` 添加 MuJoCo 设置

**文件**: `justfile`

**修改前**:
```just
check:
    cargo check --all-targets
```

**修改后**:
```just
check:
    #!/usr/bin/env bash
    eval "$(just _mujoco_download)"
    if [ -n "${MUJOCO_DYNAMIC_LINK_DIR:-}" ]; then
        >&2 echo "✓ Using MuJoCo from: $MUJOCO_DYNAMIC_LINK_DIR"
        case "$(uname -s)" in
            Linux*)
                >&2 echo "✓ RPATH embedded for Linux"
                ;;
            Darwin*)
                >&2 echo "✓ Framework linked for macOS"
                ;;
        esac
    fi
    cargo check --all-targets
```

**理由**: `cargo check` 也触发编译，同样需要 MuJoCo 环境

---

### 3. 修复 clone_on_copy 警告

**文件**: `crates/piper-can/src/mock.rs`

**修改**:
```rust
// ❌ 修改前
adapter.send(frame.clone()).unwrap();

// ✅ 修改后
adapter.send(frame).unwrap();
```

**位置**: Lines 260, 271, 382

**理由**: `PiperFrame` 实现 `Copy` trait，不需要显式 clone()

---

### 4. 添加 piper-driver mock feature

**文件**: `crates/piper-driver/Cargo.toml`

**添加**:
```toml
[features]
default = []
# Real-time thread priority support (Linux/macOS/Windows)
realtime = ["dep:thread-priority"]
# Mock mode for testing without hardware
mock = ["piper-can/mock"]
```

**理由**: 需要定义 mock feature 才能使用 `#[cfg(feature = "mock")]`

---

### 5. Guard 硬件后端相关代码

**文件**: `crates/piper-driver/src/builder.rs`

**添加 cfg guards**:
```rust
// Imports
#[cfg(all(not(feature = "mock"), target_os = "linux"))]
use piper_can::SocketCanAdapter;

#[cfg(not(feature = "mock"))]
use piper_can::gs_usb::GsUsbCanAdapter;

#[cfg(not(feature = "mock"))]
use piper_can::gs_usb_udp::GsUsbUdpAdapter;

#[cfg(not(feature = "mock"))]
use piper_can::{CanDeviceError, CanDeviceErrorKind, CanError};

// Methods
#[cfg(all(not(feature = "mock"), target_os = "linux"))]
fn build_socketcan(&self, interface: &str) -> Result<Piper, DriverError>

#[cfg(not(feature = "mock"))]
fn build_gs_usb_direct(&self) -> Result<Piper, DriverError>

#[cfg(not(feature = "mock"))]
fn build_gs_usb_daemon(&self, daemon_addr: String) -> Result<Piper, DriverError>
```

**添加 mock 模式实现**:
```rust
// Mock 模式：使用 MockCanAdapter
#[cfg(feature = "mock")]
{
    use piper_can::MockCanAdapter;
    let can = MockCanAdapter::new();

    let interface = self.interface.unwrap_or_else(|| "mock".to_string());
    let bus_speed = self.baud_rate.unwrap_or(1_000_000);
    Piper::new(can, self.pipeline_config)
        .map(|p| p.with_metadata(interface, bus_speed))
        .map_err(DriverError::Can)
}
```

**理由**: 当 mock feature 启用时，硬件后端不可用，需要提供 mock 实现

---

### 6. 更新 pre-commit hook

**文件**: `.cargo-husky/hooks/pre-commit`

**添加 MuJoCo 设置**:
```bash
# 设置 MuJoCo 环境（复用 justfile 逻辑）
eval "$(just _mujoco_download 2>/dev/null || true)"
if [ -n "${MUJOCO_DYNAMIC_LINK_DIR:-}" ]; then
  >&2 echo "✓ Using MuJoCo from: $MUJOCO_DYNAMIC_LINK_DIR"
fi

# 运行 Clippy (与 just clippy 保持一致)
echo "Running cargo clippy..."
cargo clippy --workspace --all-targets -- -D warnings
```

**关键改动**:
- ✅ 添加 `eval "$(just _mujoco_download 2>/dev/null || true)"`（容错处理）
- ✅ 改用 `--workspace` 替代 `--all-features`（避免 mock 冲突）

**理由**: pre-commit hook 也需要 MuJoCo 环境才能编译 piper-physics

---

## 🔍 技术细节

### 为什么要移除 `--all-features`？

**问题**: `cargo clippy --all-features` 会启用所有 features，包括：
- `piper-can/mock`
- `piper-driver/mock`（新添加）
- 硬件后端 features（socketcan, gs_usb）

**冲突**: mock feature 的设计是**排他性**的：
```rust
#[cfg(all(
    not(feature = "mock"),  // ⚠️ mock 优先级最高
    any(feature = "socketcan", feature = "auto-backend")
))]
pub mod socketcan;
```

当 mock 启用时，socketcan 和 gs_usb 模块被**完全禁用**，导致：
- `piper-driver/src/builder.rs` 中的 `use piper_can::SocketCanAdapter` 失败
- Tests 和 examples 中的硬件后端导入失败

**解决方案**: 使用 `--workspace` 替代 `--all-features`，只启用 default features

---

### Mock Feature 架构

```
┌─────────────────────────────────────────────────────────┐
│                    piper-can                            │
│  ┌───────────────────────────────────────────────────┐  │
│  │              [feature = "mock"]                    │  │
│  │  ┌─────────────────────────────────────────────┐  │  │
│  │  │  MockCanAdapter (only)                      │  │  │
│  │  │  - No hardware dependencies                  │  │  │
│  │  │  - Implements CanAdapter (not Splittable)   │  │  │
│  │  └─────────────────────────────────────────────┘  │  │
│  └───────────────────────────────────────────────────┘  │
│                                                         │
│  ┌───────────────────────────────────────────────────┐  │
│  │         [not(feature = "mock")]                   │  │
│  │  ┌──────────────┐  ┌──────────────┐              │  │
│  │  │ SocketCAN    │  │  GS-USB      │              │  │
│  │  │ (Linux only) │  │ (Cross-plat) │              │  │
│  │  └──────────────┘  └──────────────┘              │  │
│  └───────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────┘
```

**关键特性**:
- ✅ **排他性**: mock 禁用所有硬件后端
- ✅ **自动选择**: `auto-backend` feature 自动选择平台后端
- ✅ **测试友好**: mock 模式无硬件依赖

---

## 📊 改进效果

### Before (修复前)

| 命令 | 状态 | 问题 |
|------|------|------|
| `just clippy` | ❌ 失败 | 缺少 MuJoCo 环境 |
| `just check` | ❌ 失败 | 缺少 MuJoCo 环境 |
| `git commit` | ❌ 失败 | pre-commit hook 缺少 MuJoCo |
| clippy 警告 | ⚠️ 3个 | clone_on_copy |
| mock feature | ❌ 未定义 | piper-driver 没有 |

### After (修复后)

| 命令 | 状态 | 结果 |
|------|------|------|
| `just clippy` | ✅ 通过 | 正确设置 MuJoCo |
| `just check` | ✅ 通过 | 正确设置 MuJoCo |
| `git commit` | ✅ 通过 | pre-commit hook 正常工作 |
| clippy 警告 | ✅ 0个 | 所有警告已修复 |
| mock feature | ✅ 已实现 | piper-driver 支持 mock |

---

## ✅ 验证清单

- [x] ✅ `just clippy` 通过（无错误，无警告）
- [x] ✅ `just check` 通过
- [x] ✅ `just build` 通过
- [x] ✅ `just test` 通过
- [x] ✅ pre-commit hook 工作正常
- [x] ✅ clone_on_copy 警告已修复
- [x] ✅ mock feature 已添加到 piper-driver
- [x] ✅ 硬件后端代码已正确 guard
- [x] ✅ mock 模式实现已添加
- [x] ✅ 移除 `--all-features` 避免 mock 冲突

---

## 📝 文件变更总结

### 修改的文件

1. **`justfile`**
   - `clippy`: 添加 MuJoCo 设置，移除 `--all-features`
   - `check`: 添加 MuJoCo 设置

2. **`crates/piper-can/src/mock.rs`**
   - 修复 3 处 `clone_on_copy` 警告（lines 260, 271, 382）

3. **`crates/piper-driver/Cargo.toml`**
   - 添加 `mock = ["piper-can/mock"]` feature

4. **`crates/piper-driver/src/builder.rs`**
   - 添加 mock feature guards
   - 添加 mock 模式实现
   - Guard 硬件后端相关代码

5. **`.cargo-husky/hooks/pre-commit`**
   - 添加 MuJoCo 环境设置
   - 改用 `--workspace` 替代 `--all-features`

### 未修改的文件

- `crates/piper-physics/build.rs` - 无需修改
- `crates/piper-physics/Cargo.toml` - 无需修改
- 其他 crate 的配置 - 无需修改

---

## 🎯 最佳实践总结

### ✅ 正确做法

1. **所有编译命令都需要 MuJoCo 设置**
   - build, test, check, clippy 都需要
   - 统一使用 `eval "$(just _mujoco_download)"`

2. **避免使用 `--all-features`**
   - mock feature 是排他性的
   - 使用 `--workspace` 代替，只启用 default features

3. **正确使用 cfg guards**
   - 定义 mock feature 后才能使用 `#[cfg(feature = "mock")]`
   - 所有硬件后端代码都需要 `#[cfg(not(feature = "mock"))]`

4. **Pre-commit hook 需要容错**
   - 使用 `2>/dev/null || true` 避免失败
   - 复用 justfile 逻辑（DRY）

### ❌ 错误做法

1. **使用 `--all-features`**
   - 导致 mock 和硬件后端冲突
   - 编译失败

2. **忘记添加 MuJoCo 设置**
   - clippy/check 命令失败
   - pre-commit hook 失败

3. **未定义 mock feature**
   - `#[cfg(feature = "mock")]` 被视为意外配置
   - clippy 拒绝编译

---

**状态**: ✅ **已完成并验证**

所有更改已完成，`just clippy` 和 `just check` 现在正常工作，pre-commit hook 也已更新。
