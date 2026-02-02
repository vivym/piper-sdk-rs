# MuJoCo 统一构建架构分析报告（修订版）

**日期**: 2025-02-02
**版本**: v2.0（含改进建议）
**问题**: `auto-download-mujoco` 特性在 macOS 不可用，需要重新设计构建架构

---

## 📋 问题背景

### 当前架构的问题

1. **macOS 支持**: `auto-download-mujoco` 特性**不支持 macOS**（mujoco-rs 的限制）
2. **错误信息**: macOS 用户开启该特性后会看到混淆的错误
3. **职责不清**: build.rs、wrapper script、just 各有职责，但存在重复
4. **用户体验**: 需要处理多个不同的入口点

### 用户疑问

> "是不是应该去掉 `auto-download-mujoco`，但 build.rs 又无法在 mujoco-rs build.rs 之前执行？是不是也应该去掉 build.rs，写一个统一的脚本？"

---

## 🔍 当前架构分析

### 架构层次图

```
用户命令
  ├─ just build
  ├─ just test
  └─ ./build_with_mujoco.sh
        ↓
  调用 cargo
        ↓
  ┌─────────────────────────────────────┐
  │  1. mujoco-rs build.rs 执行         │  ← 第一个执行
  │     - 检查 MUJOCO_* 环境变量         │
  │     - auto-download? (仅 Linux/Win)  │
  │     - 否则 panic                     │
  └─────────────────────────────────────┘
        ↓
  ┌─────────────────────────────────────┐
  │  2. piper-physics build.rs 执行      │  ← 第二个执行
  │     - 读取 MUJOCO_* 环境变量         │
  │     - 检测系统 cache                 │
  │     - 嵌入 RPATH                    │
  │     - 友好错误信息                   │
  └─────────────────────────────────────┘
        ↓
  编译代码
```

### 各组件职责

| 组件 | 当前职责 | 问题 |
|------|---------|------|
| **piper-physics build.rs** | 检测 cache、嵌入 RPATH、友好错误 | ❌ 无法在 mujoco-rs 前执行 |
| **mujoco-rs build.rs** | 自动下载（Linux/Win） | ❌ macOS 不支持、错误不友好 |
| **build_with_mujoco.sh** | 设置环境变量、调用 cargo | ✅ 功能完整，但是独立脚本 |
| **justfile** | 提供友好的命令接口 | ✅ 用户体验好 |
| **just _mujojo_setup** | 输出 export 语句 | ✅ 环境变量设置 |

---

## 🎯 关键发现

### 发现 1: auto-download-mujoco 的局限性

**mujoco-rs 源码分析**:

```rust
// mujoco-rs/build.rs (简化版)
#[cfg(feature = "auto-download-mujoco")]
fn main() {
    let download_dir = env::var("MUJOCO_DOWNLOAD_DIR")
        .expect("MUJOCO_DOWNLOAD_DIR must be set");

    #[cfg(not(any(target_os = "linux", windows)))]
    compile_error!(
        "auto-download-mujoco is only supported on Linux and Windows. \
         On macOS, install MuJoCo via `brew install mujoco`"
    );
}
```

**结论**:
- ❌ **macOS 不支持 auto-download**（编译错误）
- ✅ Linux/Windows 支持自动下载
- ⚠️ 但用户可以在 macOS 开启该特性（条件编译在 build.rs 中）

### 发现 2: build.rs 的执行顺序不可改变

**Cargo 的依赖顺序**:
```
mujoco-rs (piper-physics 的依赖)
  └─> build.rs 执行
      └─> 如果失败，piper-physics build.rs 根本不会执行
```

**关键问题**:
- ❌ piper-physics build.rs 无法"前置"执行
- ❌ piper-physics build.rs 无法为 mujoco-rs 设置环境变量
- ❌ `cargo:rustc-env=` 不传播到依赖的 build.rs

### 发现 3: Wrapper Script 的必要性

**为什么必须用 wrapper**:
1. **环境变量传播**: shell export 会传播到所有子进程
2. **执行时机**: 在 cargo 之前执行，所有 build.rs 都能看到
3. **跨平台**: 可以处理不同平台的差异

**当前的三个 wrapper 方案**:
1. `build_with_mujoco.sh` - 独立的 bash 脚本
2. `just` + `_mujojo_setup` - just 的 shebang recipe
3. 手动 export - 高级用户

---

## 💡 解决方案对比

### 方案 A: 保留 auto-download-mujoco（当前方案）

**架构**:
```
just / build_with_mujoco.sh
  └─> 设置 MUJOCO_DOWNLOAD_DIR
      └─> mujoco-rs build.rs (自动下载)
```

**优势**:
- ✅ Linux/Windows: 零配置，自动下载
- ✅ 利用 mujoco-rs 的内置功能

**劣势**:
- ❌ macOS: 需要手动处理
- ❌ 错误信息不友好（panic）
- ❌ 职责分散（mujoco-rs + piper-physics build.rs）

**macOS 用户体验**:
```bash
$ cargo build
error: auto-download-mujoco is only supported on Linux and Windows
```
❌ **用户体验差**

---

### 方案 B: 去掉 auto-download-mujoco，统一脚本下载

**架构**:
```
just / build_with_mujoco.sh
  └─> _download_mujoco (统一脚本)
      ├─> Linux: 下载 tar.gz
      ├─> macOS: 检测 brew，失败则提示
      └─> Windows: 下载 zip
      └─> 设置 MUJOCO_DYNAMIC_LINK_DIR
      └─> cargo build
```

**实现**:

```just
# 下载 MuJoCo（跨平台）
_mujoco_download:
    #!/usr/bin/env bash
    # 检测平台
    case "$(uname -s)" in
        Linux*)
            cache_dir="${XDG_CACHE_HOME:-$HOME/.cache}/mujoco-rs"
            mujoco_version="3.3.7"
            download_url="https://github.com/google-deepmind/mujoco/releases/download/${mujoco_version}/mujoco-${mujoco_version}-linux-x86_64.tar.gz"
            ;;
        Darwin*)
            # macOS: 检测 brew
            if brew list mujoco &>/dev/null; then
                brew_lib=$(brew --prefix mujoco)/lib
                echo "export MUJOCO_DYNAMIC_LINK_DIR=\"$brew_lib\""
                exit 0
            else
                >&2 echo "❌ macOS: MuJoCo not found"
                >&2 echo ""
                >&2 echo "Please install MuJoCo via Homebrew:"
                >&2 echo "  brew install mujoco"
                exit 1
            fi
            ;;
        MINGW*|MSYS*|CYGWIN*|Windows_NT*)
            cache_dir="$LOCALAPPDATA/mujoco-rs"
            mujoco_version="3.3.7"
            download_url="https://github.com/google-deepmind/mujoco/releases/download/${mujoco_version}/mujoco-${mujoco_version}-windows-x86_64.zip"
            ;;
    esac

    # 检查 cache
    mujoco_lib="$cache_dir/mujoco-${mujoco_version}/lib"
    if [ -d "$mujoco_lib" ]; then
        echo "export MUJOCO_DYNAMIC_LINK_DIR=\"$mujoco_lib\""
        >&2 echo "✓ Using cached MuJoCo: $mujoco_lib"
        exit 0
    fi

    # 下载并解压
    >&2 echo "Downloading MuJoCo..."
    mkdir -p "$cache_dir"
    if [ "$(uname -s)" = "Linux" ]; then
        curl -L "$download_url" | tar xz -C "$cache_dir"
    else
        # Windows (需要 unzip)
        curl -L -o "$cache_dir/mujoco.zip" "$download_url"
        unzip -q "$cache_dir/mujoco.zip" -d "$cache_dir"
    fi

    echo "export MUJOCO_DYNAMIC_LINK_DIR=\"$mujoco_lib\""
    >&2 echo "✓ MuJoCo downloaded to: $cache_dir"
```

**优势**:
- ✅ **统一的下载逻辑**：一个脚本处理所有平台
- ✅ **macOS 友好**：检测 brew，给出清晰的提示
- ✅ **更好的错误处理**：可以提供平台特定的建议
- ✅ **职责清晰**：piper-physics 不依赖 mujoco-rs 的自动下载
- ✅ **可以去掉 piper-physics build.rs**：环境设置都在 wrapper 中

**劣势**:
- ⚠️ 需要维护下载逻辑（但很简单）
- ⚠️ 去掉了 mujoco-rs 的便利性

**macOS 用户体验**:
```bash
$ just build
❌ macOS: MuJoCo not found

Please install MuJoCo via Homebrew:
  brew install mujoco
```
✅ **用户体验好**

---

### 方案 C: 混合模式（条件化 auto-download）

**架构**:
```
piper-physics/Cargo.toml:
  [target.'cfg(not(target_os = "macos"))'.dependencies]
  mujoco-rs = { features = ["auto-download-mujoco"] }

  [target.'cfg(target_os = "macos")'.dependencies]
  mujoco-rs = { version = "2.3" }  # 无 auto-download
```

**优势**:
- ✅ Linux/Windows: 保留自动下载
- ✅ macOS: 避免 auto-download 错误
- ✅ 最小改动

**劣势**:
- ❌ macOS 用户仍需手动设置环境变量
- ❌ piper-physics build.rs 仍需要（用于 RPATH）
- ❌ 职责仍然分散

**macOS 用户体验**:
```bash
$ cargo build
error: MUJOCO_DYNAMIC_LINK_DIR must be set
```
⚠️ **仍然不够友好**

---

## 🎯 推荐方案：方案 B（统一脚本下载）

### 为什么选择方案 B？

1. **最佳用户体验**
   - 所有平台都有清晰的错误提示
   - macOS 用户得到明确的 brew 安装指令
   - Linux/Windows 用户自动下载

2. **职责清晰**
   - **Wrapper script/just**: 负责 MuJoCo 获取
   - **mujoco-rs**: 只负责绑定，不负责下载
   - **piper-physics**: 只负责使用 MuJoCo

3. **可以简化架构**
   - ✅ 去掉 `auto-download-mujoco` 特性
   - ✅ 简化或去掉 `piper-physics/build.rs`
   - ✅ `build_with_mujoco.sh` 可以被 just 替代

4. **更好的错误处理**
   - 可以在下载前检测网络
   - 可以提供平台特定的建议
   - 可以处理代理、镜像等特殊情况

---

## 🔧 关键改进建议（基于专业反馈）

在实施方案 B 之前，以下 **4 点关键改进** 是确保方案"能用、好维护、真跨平台"的必要条件。

### 改进 1: ⚠️ 版本耦合问题 (Version Coupling)

**问题**: 原方案在脚本中硬编码 `mujoco_version="3.3.7"`，如果未来升级 `Cargo.toml` 中的 `mujoco-rs` 但忘记修改脚本，会导致版本不匹配，运行时崩溃（Segfault）。

**解决方案**: 从 `Cargo.lock` 自动解析 MuJoCo 版本，作为唯一的真理来源（Single Source of Truth）。

**Cargo.lock 格式**:
```toml
[[package]]
name = "mujoco-rs"
version = "2.3.0+mj-3.3.7"  # 格式: <crate-version>+mj-<mujoco-version>
```

**实现**:
```just
# 自动从 Cargo.lock 解析 MuJoCo 版本
_mujoco_parse_version:
    #!/usr/bin/env bash
    grep -A 1 '^name = "mujoco-rs"' "${PWD}/Cargo.lock" | \
      grep '^version' | \
      sed -E 's/.*\+mj-([0-9.]+).*/\1/'
```

---

### 改进 2: 🐛 Shell 兼容性陷阱 (Process Substitution)

**问题**: 原方案使用 `source <(just _mujoco_download)`。
- `<(...)` 是 **Bash 特有**的进程替换语法
- 标准 `/bin/sh`（Debian/Ubuntu 的 dash）**不支持**
- 非 bash 环境会报错 `syntax error near unexpected token '('`

**解决方案**: 使用 `eval` 替代 `source`，在所有 POSIX shell 中通用。

**修改前**:
```bash
source <(just _mujoco_download)
```

**修改后**:
```bash
eval "$(just _mujoco_download)"
```

---

### 改进 3: 🖥️ Windows 环境支持（策略 B）

**问题**: 原方案假设 Windows 用户使用 Git Bash。如果用户直接使用 PowerShell 或 CMD，脚本会失败。

**解决方案**: 实现策略 B - 完整的跨平台支持。

**just 原生支持 OS 检测**:
```just
# Linux/macOS
_mujoco_download:
    #!/usr/bin/env bash
    # ... bash 逻辑

_mujoco_download_windows:
    #!/usr/bin/env powershell
    # ... PowerShell 逻辑
```

**但更好的方案**: 统一脚本，通过 shebang 检测 OS。
```just
_mujoco_download:
    #!/usr/bin/env bash
    # PowerShell 可以通过 bash 调用（Git Bash/WSL）
    # 或在脚本中检测并调用 PowerShell
```

**实际策略**: 由于 `just` 本身是跨平台的，且 Windows 用户通常有 Git Bash（用于 Rust 开发），我们在脚本中添加检测和友好提示。

---

### 改进 4: 🔧 `piper-physics/build.rs` 必须保留（极简版）

**问题**: 如果完全去掉 build.rs，编译出的二进制文件没有 RPATH，用户每次运行时必须手动设置 `LD_LIBRARY_PATH`。

**解决方案**: 保留**极简版** build.rs，仅用于：
1. 设置 RPATH（运行时可直接执行）
2. 设置 `cargo:rustc-env=LD_LIBRARY_PATH`（测试时不需要手动 export）

**极简版 build.rs**:
```rust
fn main() {
    println!("cargo:rerun-if-env-changed=MUJOCO_DYNAMIC_LINK_DIR");

    if let Ok(lib_dir) = std::env::var("MUJOCO_DYNAMIC_LINK_DIR") {
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

---

## 🎯 改进后的最终架构

```
justfile
  │
  ├─> _mujoco_parse_version (自动从 Cargo.lock 解析)
  │
  └─> just build
       │
       ├─> eval "$(just _mujoco_download)"
       │     │
       │     ├─> 调用 _mujoco_parse_version
       │     ├─> 检测平台 (Linux/macOS/Windows)
       │     ├─> Linux/Windows: 下载对应版本
       │     ├─> macOS: 检测 brew
       │     └─> print "export MUJOCO_DYNAMIC_LINK_DIR=..."
       │
       └─> cargo build
             │
             ├─> mujoco-rs (被动接收环境变量)
             │
             └─> piper-physics/build.rs (极简版)
                   ├─> 设置 cargo:rustc-link-search (编译通过)
                   ├─> 设置 RPATH (运行时直接运行)
                   └─> 设置 cargo:rustc-env (测试无需 export)
```

---

## 📦 实施计划（更新版）

### 步骤 1: 修改 piper-physics/Cargo.toml

```toml
[dependencies]
mujoco-rs = "2.3"  # 去掉 features = ["auto-download-mujoco"]

[build-dependencies]
# 保留（如果需要）
```

### 步骤 2: 简化 piper-physics/build.rs

使用极简版 build.rs（见"改进 4"）。

### 步骤 3: 实现新版 justfile

**关键 recipe**:

```just
# 自动从 Cargo.lock 解析 MuJoCo 版本
_mujoco_parse_version:
    #!/usr/bin/env bash
    grep -A 1 '^name = "mujoco-rs"' "${PWD}/Cargo.lock" | \
      grep '^version' | \
      sed -E 's/.*\+mj-([0-9.]+).*/\1/'

# 下载/检测 MuJoCo（跨平台）
_mujoco_download:
    #!/usr/bin/env bash
    set -euo pipefail

    # 获取 MuJoCo 版本
    mujoco_version=$(just _mujoco_parse_version)

    # 检测平台并设置缓存目录
    case "$(uname -s)" in
        Linux*)
            cache_dir="${XDG_CACHE_HOME:-$HOME/.cache}/mujoco-rs"
            download_url="https://github.com/google-deepmind/mujoco/releases/download/${mujoco_version}/mujoco-${mujoco_version}-linux-x86_64.tar.gz"
            ;;
        Darwin*)
            # macOS: 检测 brew
            if command -v brew &>/dev/null && brew list mujoco &>/dev/null; then
                brew_lib=$(brew --prefix mujoco)/lib
                echo "export MUJOCO_DYNAMIC_LINK_DIR=\"$brew_lib\""
                >&2 echo "✓ Using MuJoCo from Homebrew: $brew_lib"
                exit 0
            else
                >&2 echo "❌ macOS: MuJoCo not found"
                >&2 echo ""
                >&2 echo "Please install MuJoCo via Homebrew:"
                >&2 echo "  brew install mujoco"
                exit 1
            fi
            ;;
        MINGW*|MSYS*|CYGWIN*|Windows_NT*)
            cache_dir="$LOCALAPPDATA/mujoco-rs"
            download_url="https://github.com/google-deepmind/mujoco/releases/download/${mujoco_version}/mujoco-${mujoco_version}-windows-x86_64.zip"
            ;;
        *)
            >&2 echo "❌ Unsupported platform: $(uname -s)"
            exit 1
            ;;
    esac

    # 检查 cache
    mujoco_lib="$cache_dir/mujoco-${mujoco_version}/lib"
    if [ -d "$mujoco_lib" ]; then
        echo "export MUJOCO_DYNAMIC_LINK_DIR=\"$mujoco_lib\""
        # 输出信息到 stderr（不影响 eval）
        >&2 echo "✓ Using cached MuJoCo: $mujoco_lib"
        exit 0
    fi

    # 下载并解压
    mkdir -p "$cache_dir"
    >&2 echo "Downloading MuJoCo ${mujoco_version}..."

    case "$(uname -s)" in
        Linux*)
            curl -L "$download_url" | tar xz -C "$cache_dir"
            ;;
        MINGW*|MSYS*|CYGWIN*|Windows_NT*)
            # Windows (需要 unzip)
            curl -L -o "$cache_dir/mujoco.zip" "$download_url"
            unzip -q "$cache_dir/mujoco.zip" -d "$cache_dir"
            rm "$cache_dir/mujoco.zip"
            ;;
    esac

    # 验证下载成功
    if [ ! -d "$mujoco_lib" ]; then
        >&2 echo "❌ Failed to download MuJoCo"
        exit 1
    fi

    echo "export MUJOCO_DYNAMIC_LINK_DIR=\"$mujoco_lib\""
    >&2 echo "✓ MuJoCo downloaded to: $cache_dir"
```

### 步骤 4: 更新所有 just recipes

```just
build:
    #!/usr/bin/env bash
    eval "$(just _mujoco_download)"
    cargo build --workspace

test:
    #!/usr/bin/env bash
    eval "$(just _mujoco_download)"
    cargo test --workspace
```

### 步骤 5: 删除 build_with_mujoco.sh

- 被 just 完全替代
- 保留为备用（可选）

### 步骤 6: 更新文档

- QUICKSTART.md
- just 集成文档

---

## 🔍 实施验证清单

- [ ] ✅ 版本自动解析（从 Cargo.lock）
- [ ] ✅ Shell 兼容性（使用 `eval`）
- [ ] ✅ 跨平台支持（Linux/macOS/Windows）
- [ ] ✅ 极简版 build.rs（RPATH 嵌入）
- [ ] ✅ Linux 下载测试
- [ ] ✅ macOS brew 检测测试
- [ ] ✅ Windows 下载测试（Git Bash）
- [ ] ✅ 文档更新

---

## 🔄 迁移对比表

| 特性 | 当前方案 (方案 A) | 新方案 (方案 B) |
|------|------------------|----------------|
| **Linux 下载** | mujoco-rs 自动 | wrapper 脚本 |
| **Windows 下载** | mujoco-rs 自动 | wrapper 脚本 |
| **macOS 处理** | ❌ 混乱 | ✅ 检测 brew |
| **错误信息** | ❌ panic | ✅ 友好提示 |
| **piper-physics build.rs** | 需要（RPATH） | 可选/简化 |
| **build_with_mujoco.sh** | 需要 | 可删除 |
| **职责清晰度** | ⚠️ 分散 | ✅ 清晰 |
| **维护成本** | ⚠️ 依赖 mujoco-rs | ✅ 自主控制 |

---

## 📊 最终建议

### 推荐架构

```
用户命令
  └─ just build / just test
      └─ source <(just _mujoco_download)
          ├─> 检测平台
          ├─> 检查 cache / 下载 / 检测 brew
          └─> 输出 export MUJOCO_DYNAMIC_LINK_DIR
              └─> cargo build/test
                  └─> mujoco-rs build.rs (检查环境变量)
                      └─> piper-physics (可选的 RPATH 嵌入)
```

### 具体改动

1. **去掉 `auto-download-mujoco` 特性**
2. **实现 `_mujoco_download` recipe**（跨平台下载/检测）
3. **简化 `piper-physics/build.rs`**（只保留 RPATH 嵌入）
4. **删除 `build_with_mujoco.sh`**（被 just 替代）

### 用户体验

#### Linux/Windows
```bash
$ just build
✓ Using cached MuJoCo: ~/.cache/mujoco-rs/mujoco-3.3.7/lib
   Compiling piper-physics v0.0.3
    Finished `dev` profile
```

#### macOS (有 brew)
```bash
$ just build
✓ Using MuJoCo from Homebrew: /opt/homebrew/opt/mujoco/lib
   Compiling piper-physics v0.0.3
    Finished `dev` profile
```

#### macOS (无 brew)
```bash
$ just build
❌ macOS: MuJoCo not found

Please install MuJoCo via Homebrew:
  brew install mujoco

Or manually install and set:
  export MUJOCO_DYNAMIC_LINK_DIR=/path/to/mujoco/lib
```

---

## ✅ 总结

### 回答用户的问题

> "开启 auto-download-mujoco 是不是在 macOS 下就不行了？"

**答**: 是的，mujoco-rs 的 `auto-download-mujoco` 特性**不支持 macOS**。开启后会导致编译错误。

> "是不是应该去掉 auto-download-mujoco？"

**答**: **是的**。应该由我们自己控制下载逻辑，这样可以：
- 统一处理所有平台
- 提供更好的错误信息
- 职责更清晰

> "build.rs 又无法实现在 mujoco-rs build.rs 之前执行，是不是也应该去掉 build.rs？"

**答**: **可以简化，但不一定要完全去掉**。可以：
- 去掉环境检测逻辑（移到 wrapper）
- 保留 RPATH 嵌入（Linux 动态链接需要）
- 或者完全去掉（如果不需要 RPATH）

> "是不是应该写一个脚本实现 build.rs 的功能？"

**答**: **是的**。这个脚本应该：
- 在 `_mujoco_download` recipe 中实现
- 处理所有平台的下载/检测
- 输出环境变量供 source

> "build_with_mujoco.sh 是不是也不需要了？"

**答**: **是的**。just 的 `_mujoco_download` recipe 可以完全替代它。

---

**下一步**: 开始实施方案 B，创建统一的 MuJoCo 下载逻辑。
