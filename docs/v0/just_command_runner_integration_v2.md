# Just Command Runner Integration（v2.0 统一架构）

**日期**: 2025-02-02
**版本**: v2.0
**主题**: 使用 `just` 实现统一的 MuJoCo 下载/检测/构建接口

---

## 📋 架构概述

### v2.0 关键改进

基于 `docs/v0/mujoco_unified_build_architecture_analysis.md` 的专业反馈，v2.0 实现了以下关键改进：

1. ✅ **版本自动解析**: 从 `Cargo.lock` 自动解析 MuJoCo 版本（避免硬编码）
2. ✅ **Shell 兼容性**: 使用 `eval` 替代 `source <(...)`（兼容所有 POSIX shell）
3. ✅ **完整跨平台**: Linux/macOS/Windows 统一处理
4. ✅ **极简 build.rs**: 只处理 RPATH 嵌入，下载逻辑移至 wrapper
5. ✅ **去掉 auto-download-mujoco**: mujoco-rs 不再负责下载

---

## 🏗️ 架构设计

```
justfile (统一入口)
  │
  ├─> _mujoco_parse_version (自动从 Cargo.lock 解析)
  │
  └─> just build / just test
       │
       ├─> eval "$(just _mujoco_download)"
       │     │
       │     ├─> 调用 _mujoco_parse_version
       │     ├─> Linux: 下载 tar.gz 到 ~/.cache/mujoco-rs/
       │     ├─> macOS: 检测 brew，失败则友好提示
       │     ├─> Windows: 下载 zip 到 %LOCALAPPDATA%/mujoco-rs/
       │     └─> print "export MUJOCO_DYNAMIC_LINK_DIR=..."
       │
       └─> cargo build/test
             │
             ├─> mujoco-rs (被动接收环境变量，不再自作主张)
             │
             └─> piper-physics/build.rs (极简版)
                   ├─> cargo:rustc-link-search (编译时)
                   ├─> cargo:rustc-link-arg=-Wl,-rpath (运行时 RPATH)
                   └─> cargo:rustc-env=LD_LIBRARY_PATH (测试时)
```

---

## 🔧 技术实现

### 1. 版本自动解析（Single Source of Truth）

**问题**: 硬编码版本号会导致升级时的版本不匹配风险。

**解决方案**: 从 `Cargo.lock` 自动解析。

```just
_mujoco_parse_version:
    #!/usr/bin/env bash
    grep -A 1 '^name = "mujoco-rs"' "${PWD}/Cargo.lock" | \
      grep '^version' | \
      sed -E 's/.*\+mj-([0-9.]+).*/\1/'
```

**Cargo.lock 格式**:
```toml
[[package]]
name = "mujoco-rs"
version = "2.3.0+mj-3.3.7"  # +mj-3.3.7 表示 MuJoCo 版本
```

**输出**: `3.3.7`

---

### 2. Shell 兼容性（eval vs source）

**问题**: `source <(...)` 是 Bash 特有的进程替换，标准 `/bin/sh` 不支持。

**解决方案**: 使用 `eval` 在所有 POSIX shell 中通用。

```just
build:
    #!/usr/bin/env bash
    eval "$(just _mujoco_download)"  # ✅ POSIX 兼容
    cargo build --workspace
```

---

### 3. 跨平台下载/检测

**Linux**:
```bash
cache_dir="${XDG_CACHE_HOME:-$HOME/.cache}/mujoco-rs"
download_url=".../mujoco-${version}-linux-x86_64.tar.gz"
curl -L "$download_url" | tar xz -C "$cache_dir"
```

**macOS**:
```bash
# 检测 brew
if brew list mujoco &>/dev/null; then
    echo "export MUJOCO_DYNAMIC_LINK_DIR=$(brew --prefix mujoco)/lib"
else
    >&2 echo "❌ Please install: brew install mujoco"
    exit 1
fi
```

**Windows (Git Bash)**:
```bash
cache_dir="$LOCALAPPDATA/mujoco-rs"
download_url=".../mujoco-${version}-windows-x86_64.zip"
curl -L -o "$cache_dir/mujoco.zip" "$download_url"
unzip -q "$cache_dir/mujoco.zip" -d "$cache_dir"
```

---

### 4. 极简版 build.rs

**职责**: 只处理编译时的链接器和运行时的 RPATH。

```rust
fn main() {
    println!("cargo:rerun-if-env-changed=MUJOCO_DYNAMIC_LINK_DIR");

    if let Ok(lib_dir) = env::var("MUJOCO_DYNAMIC_LINK_DIR") {
        // 编译时：告诉链接器库的位置
        println!("cargo:rustc-link-search=native={}", lib_dir);

        // 运行时：嵌入 RPATH（Linux Only）
        #[cfg(target_os = "linux")]
        println!("cargo:rustc-link-arg=-Wl,-rpath,{}", lib_dir);

        // 测试时：设置环境变量
        println!("cargo:rustc-env=LD_LIBRARY_PATH={}", lib_dir);
    }
}
```

**去掉的功能** (移至 just wrapper):
- ❌ 版本检测
- ❌ 平台检测
- ❌ 下载逻辑
- ❌ 友好错误信息

---

## 📦 提供的命令

### 构建相关

| 命令 | 说明 |
|------|------|
| `just build` | 构建整个 workspace（自动下载 MuJoCo） |
| `just build-pkg <package>` | 构建特定包 |
| `just release` | Release 构建 |
| `just check` | 快速检查编译 |
| `just clean` | 清理构建产物 |

### 测试相关

| 命令 | 说明 |
|------|------|
| `just test` | 运行所有测试 |
| `just test-pkg <package> [args]` | 运行特定包的测试，可传递额外参数 |

### 代码质量

| 命令 | 说明 |
|------|------|
| `just clippy` | 运行 linter |
| `just fmt` | 格式化代码 |
| `just fmt-check` | 检查格式化 |

### MuJoCo 相关

| 命令 | 说明 |
|------|------|
| `just mujoco-info` | 显示 MuJoCo 缓存位置和状态 |
| `just mujoco-clean` | 清理 MuJoCo 缓存 |
| `just mujoco-shell` | 进入带 MuJoCo 环境的 shell |
| `just _mujoco_parse_version` | 解析 MuJoCo 版本（调试用） |
| `just _mujoco_download` | 下载/检测 MuJoCo（调试用） |

---

## 🚀 使用示例

### 基本使用

```bash
# 查看所有命令
just

# 构建项目（首次会自动下载 MuJoCo）
just build

# 运行测试
just test

# 格式化代码
just fmt
```

### 高级使用

```bash
# 构建特定包
just build-pkg piper-physics

# 运行特定包的测试（带额外参数）
just test-pkg piper-physics --lib

# 发布构建
just release

# 查看 MuJoCo 状态
just mujoco-info

# 清理缓存并重新下载
just mujoco-clean
just build
```

---

## 📊 各平台用户体验

### Linux

```bash
$ just build
Downloading MuJoCo 3.3.7...
✓ MuJoCo downloaded to: /home/user/.cache/mujoco-rs
   Compiling piper-physics v0.0.3
warning: Using MuJoCo from: /home/user/.cache/mujoco-rs/mujoco-3.3.7/lib
warning: ✓ RPATH embedded for Linux
    Finished `dev` profile [unoptimized + debuginfo] target(s)
```

✅ **零配置，自动下载**

### macOS (有 brew)

```bash
$ just build
✓ Using MuJoCo from Homebrew: /opt/homebrew/opt/mujoco/lib
   Compiling piper-physics v0.0.3
    Finished `dev` profile [unoptimized + debuginfo] target(s)
```

✅ **自动检测 brew，无需配置**

### macOS (无 brew)

```bash
$ just build
❌ macOS: MuJoCo not found

Please install MuJoCo via Homebrew:
  brew install mujoco
```

✅ **清晰的错误提示**

### Windows (Git Bash)

```bash
$ just build
Downloading MuJoCo 3.3.7...
✓ MuJoCo downloaded to: C:\Users\user\AppData\Local\mujoco-rs
   Compiling piper-physics v0.0.3
    Finished `dev` profile [unoptimized + debuginfo] target(s)
```

✅ **自动下载（需要 Git Bash）**

---

## 🔄 从 v1.0 迁移

### v1.0 → v2.0 变更

| 特性 | v1.0 | v2.0 |
|------|------|------|
| **版本管理** | 硬编码 `3.3.7` | 自动从 Cargo.lock 解析 |
| **Shell 兼容** | `source <(...)` (Bash only) | `eval "$(...)"` (POSIX) |
| **macOS 支持** | ❌ 混乱 | ✅ 检测 brew |
| **build.rs** | 复杂（下载+RPATH） | 极简（只 RPATH） |
| **mujoco-rs** | `auto-download-mujoco` | 去掉该 feature |
| **错误信息** | panic | 友好提示 |

### 代码变更

**piper-physics/Cargo.toml**:
```toml
# v1.0
mujoco-rs = { version = "2.3", features = ["auto-download-mujoco"] }

# v2.0
mujoco-rs = { version = "2.3" }
```

**piper-physics/build.rs**:
```rust
// v1.0: ~300 行复杂逻辑（下载、检测、RPATH）

// v2.0: ~30 行极简逻辑（只 RPATH）
```

**justfile**:
```just
# v1.0
_mujoco_setup:
    echo "export MUJOCO_DOWNLOAD_DIR=..."

build:
    source <(just _mujoco_setup)  # Bash only

# v2.0
_mujoco_parse_version:
    grep ... Cargo.lock

_mujoco_download:
    # 完整的跨平台下载逻辑

build:
    eval "$(just _mujoco_download)"  # POSIX compatible
```

---

## ✅ 优势总结

1. **版本安全**: 自动从 Cargo.lock 解析，避免版本不匹配
2. **Shell 兼容**: `eval` 在所有 POSIX shell 中工作
3. **macOS 友好**: 检测 brew，提供清晰的安装指令
4. **职责清晰**: build.rs 只处理 RPATH，下载在 wrapper 中
5. **更好的错误**: 友好的错误提示，不是 panic
6. **跨平台**: 统一的接口，Linux/macOS/Windows 都支持

---

## 📚 相关文档

- [Just 官方文档](https://just.systems/)
- `docs/v0/mujoco_unified_build_architecture_analysis.md` - 架构设计详解
- `docs/v0/mujoco_implementation_final_report.md` - MuJoCo 集成历史
- `docs/v0/build_rs_vs_wrapper_script_analysis.md` - build.rs 架构分析
- `QUICKSTART.md` - 快速开始指南

---

**总结**: v2.0 实现了统一、安全、跨平台的 MuJoCo 构建架构，推荐所有用户使用 `just` 作为主要构建接口。`build_with_mujoco.sh` 已被完全替代，可以删除。
