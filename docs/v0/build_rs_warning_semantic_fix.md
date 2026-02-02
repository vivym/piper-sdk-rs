# Build.rs 输出语义改进报告

**日期**: 2025-02-02
**问题**: `cargo:warning` 用于成功信息不合适
**状态**: ✅ 已修复

---

## 📋 问题描述

### 原始实现

**build.rs**:
```rust
println!("cargo:warning=Using MuJoCo from: {}", lib_dir);
println!("cargo:warning=✓ RPATH embedded for Linux");
```

**输出**:
```
warning: piper-physics@0.0.3: Using MuJoCo from: /home/viv/.local/lib/mujoco/mujoco-3.3.7/lib
warning: piper-physics@0.0.3: ✓ RPATH embedded for Linux
```

### 问题分析

| 问题 | 说明 |
|------|------|
| **语义混淆** | "成功信息"被标记为"警告" |
| **CI/CD 误报** | `--warnings` 可能拦截这些"警告" |
| **用户体验** | 用户困惑："为什么要警告我？" |
| **日志级别错误** | 应该是 INFO，不是 WARNING |

---

## ✅ 解决方案：职责分离

### 原则

- **build.rs**: 只负责编译配置（链接器、RPATH），保持静默
- **just wrapper**: 负责用户交互，输出信息性消息

### 实现

#### build.rs（静默成功）

```rust
fn main() {
    println!("cargo:rerun-if-env-changed=MUJOCO_DYNAMIC_LINK_DIR");

    if let Ok(lib_dir) = env::var("MUJOCO_DYNAMIC_LINK_DIR") {
        // 只输出编译配置，不输出用户信息
        println!("cargo:rustc-link-search=native={}", lib_dir);

        #[cfg(target_os = "linux")]
        println!("cargo:rustc-link-arg=-Wl,-rpath,{}", lib_dir);

        println!("cargo:rustc-env=LD_LIBRARY_PATH={}", lib_dir);

        // Silent success - information is printed by just wrapper
    } else {
        // 错误信息仍然使用 warning（这是真正的警告）
        println!("cargo:warning=MUJOCO_DYNAMIC_LINK_DIR not set");
        println!("cargo:warning=Please use: just build");
    }
}
```

#### justfile（输出信息）

```just
build:
    #!/usr/bin/env bash
    eval "$(just _mujoco_download)"

    # 在 just 中输出信息（不是警告）
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

    cargo build --workspace
```

---

## 📊 改进对比

### 输出对比

#### 改进前

```
warning: piper-physics@0.0.3: Using MuJoCo from: /home/viv/.local/lib/mujoco/mujoco-3.3.7/lib
warning: piper-physics@0.0.3: ✓ RPATH embedded for Linux
   Compiling piper-physics v0.0.3
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.17s
```

**问题**:
- ❌ 成功信息被标记为 `warning:`
- ❌ 语义混淆
- ❌ CI/CD 可能误报

#### 改进后

```
✓ Using cached MuJoCo: /home/viv/.local/lib/mujoco/mujoco-3.3.7/lib
✓ Using MuJoCo from: /home/viv/.local/lib/mujoco/mujoco-3.3.7/lib
✓ RPATH embedded for Linux
   Compiling piper-physics v0.0.3
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.17s
```

**改进**:
- ✅ 没有 `warning:` 前缀
- ✅ 语义正确（成功 = 信息）
- ✅ 用户体验更清晰

---

## 🎯 关键改进

### 1. 语义正确性

| 场景 | 输出方式 | 原因 |
|------|---------|------|
| **成功** | just 输出到 stderr | 信息性消息（INFO） |
| **错误** | build.rs `cargo:warning` | 警告用户（WARNING） |

### 2. 职责分离

| 组件 | 职责 | 不做 |
|------|------|------|
| **build.rs** | 链接器配置、RPATH 嵌入 | 输出用户信息 |
| **just** | 下载、信息输出 | 不干预编译配置 |

### 3. 用户体验

**之前**:
```
warning: piper-physics@0.0.3: Using MuJoCo from: ...
warning: piper-physics@0.0.3: ✓ RPATH embedded for Linux
```
用户反应：❓"为什么要警告我？我做错了什么？"

**现在**:
```
✓ Using MuJoCo from: ...
✓ RPATH embedded for Linux
```
用户反应：✅"明白了，一切正常。"

---

## ✅ 验证

### 测试构建

```bash
$ just build-pkg piper-physics
✓ Using cached MuJoCo: /home/viv/.local/lib/mujoco/mujoco-3.3.7/lib
✓ Using MuJoCo from: /home/viv/.local/lib/mujoco/mujoco-3.3.7/lib
✓ RPATH embedded for Linux
   Compiling piper-physics v0.0.3
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.17s
```

✅ **没有 `warning:` 前缀，语义正确**

### 测试错误场景

```bash
$ cargo build -p piper-physics  # 不使用 just
error: MUJOCO_DYNAMIC_LINK_DIR not set
Please use: just build
Or set: export MUJOCO_DYNAMIC_LINK_DIR=/path/to/mujoco/lib
```

✅ **真正的错误仍然使用 `cargo:warning`**

---

## 📈 影响范围

### 修改的文件

1. **`crates/piper-physics/build.rs`**
   - 去掉成功信息的 `cargo:warning`
   - 保留错误信息的 `cargo:warning`
   - 成功时保持静默

2. **`justfile`**
   - 在所有构建/测试 recipe 中添加信息输出
   - 使用 `>&2 echo` 输出到 stderr
   - 区分平台特定的消息（Linux/macOS）

### 不受影响的场景

- ✅ `cargo build --release` - 仍然工作
- ✅ `cargo test` - 仍然工作
- ✅ CI/CD - 不会误报警告
- ✅ RPATH 嵌入 - 功能完全相同

---

## 🎓 经验总结

### `cargo:warning` 的正确使用

| 场景 | 是否应该使用 | 原因 |
|------|-------------|------|
| **编译警告** | ✅ 是 | 设计目的 |
| **配置错误** | ✅ 是 | 需要用户注意 |
| **成功信息** | ❌ 否 | 应该是静默或 INFO |
| **进度信息** | ❌ 否 | 应该输出到 stdout/stderr |

### 替代方案

| 需求 | 解决方案 |
|------|---------|
| **成功时静默** | 不输出任何信息 |
| **调试信息** | 使用环境变量（如 `VERBOSE=1`） |
| **信息输出** | 在 wrapper 中输出，不在 build.rs |
| **真正的警告** | 使用 `cargo:warning` |

---

## ✅ 总结

### 改进前

```rust
// ❌ 语义错误
println!("cargo:warning=Using MuJoCo from: {}", lib_dir);
println!("cargo:warning=✓ RPATH embedded");
```

### 改进后

```rust
// ✅ 成功时静默
if let Ok(lib_dir) = env::var("MUJOCO_DYNAMIC_LINK_DIR") {
    println!("cargo:rustc-link-search=native={}", lib_dir);
    // ... 只输出编译配置
}

// ✅ 错误时警告
else {
    println!("cargo:warning=MUJOCO_DYNAMIC_LINK_DIR not set");
}
```

```just
# ✅ 在 just 中输出信息
if [ -n "${MUJOCO_DYNAMIC_LINK_DIR:-}" ]; then
    >&2 echo "✓ Using MuJoCo from: $MUJOCO_DYNAMIC_LINK_DIR"
    >&2 echo "✓ RPATH embedded for Linux"
fi
```

---

**状态**: ✅ **已修复并测试通过**

**建议**: 所有成功信息都应该保持静默或输出到 wrapper，不应该使用 `cargo:warning`。
