# Build.rs vs Wrapper Script：技术分析报告

**日期**: 2025-02-02
**问题**: 能否只用 build.rs 而不用 wrapper script？

---

## 🎯 结论

**答案：❌ 不能只用 build.rs**

**原因**：mujoco-rs 的 build.rs 会先执行（依赖顺序），如果环境变量未设置会立即 panic，piper-physics 的 build.rs 根本没有机会执行。

---

## 📊 执行顺序分析

### Cargo 构建顺序

```
1. mujoco-rs build.rs 执行 (piper-physics 的依赖)
   └─> 检查 MUJOCO_DOWNLOAD_DIR
   └─> 未设置 → panic! ❌ (构建失败)
   └─> 已设置 → 继续 ✅

2. piper-physics build.rs 执行 (如果步骤1成功)
   └─> 可以读取环境变量 ✅
   └─> 可以显示友好信息 ✅

3. 编译代码
```

**关键点**：
- ✅ Wrapper script 设置的环境变量会传播到所有 build.rs
- ❌ `cargo:rustc-env=` 不会传播到依赖的 build.rs
- ⚠️ piper-physics build.rs 只有在 mujoco-rs 成功后才会执行

---

## 🔍 实验验证

### 实验 1：直接使用 cargo（无环境变量）

```bash
$ cargo build -p piper-physics

error: mujoco-rs build.rs panicked:
  "MUJOCO_DOWNLOAD_DIR must be set"

Result: ❌ 失败（piper-physics build.rs 未执行）
```

**结论**： mujoco-rs 先执行并失败，piper-physics build.rs 没有机会显示友好错误信息。

### 实验 2：使用 wrapper script

```bash
$ ./build_with_mujoco.sh build

=== MuJoCo Build Configuration ===
Cache directory: /home/viv/.cache/mujoco-rs
Using cached MuJoCo: /home/viv/.cache/mujoco-rs/mujoco-3.3.7/lib
==================================

Result: ✅ 成功
```

**结论**：wrapper script 必需。

---

## 💡 为什么 `cargo:rustc-env=` 不能解决问题？

### 误解澄清

**常见误解**：
> "既然 build.rs 能用 `cargo:rustc-env=` 覆盖环境变量，那直接在 build.rs 中设置 `~/.cache/` 不就行了吗？"

**实际情况**：

| 特性 | `cargo:rustc-env=` | Wrapper script `export` |
|------|-------------------|------------------------|
| **作用范围** | 只在该 crate 编译时有效 | 传播到所有子进程 |
| **传播到依赖** | ❌ 否 | ✅ 是 |
| **能被 mujoco-rs 读取** | ❌ 否 | ✅ 是 |
| **能被 piper-physics 读取** | ✅ 是 | ✅ 是 |

### 关键发现

```rust
// piper-physics/build.rs
println!("cargo:rustc-env=MUJOCO_DOWNLOAD_DIR=/some/path");
```

**效果**：
- ✅ 编译 piper-physics 时，环境变量可用
- ❌ mujoco-rs build.rs 看不到这个变量（因为它不是编译过程的一部分）

---

## 📋 解决方案对比

### 方案 A：只用 build.rs（❌ 不可行）

```rust
// crates/piper-physics/build.rs
fn main() {
    // 尝试设置环境变量
    println!("cargo:rustc-env=MUJOCO_DOWNLOAD_DIR=~/.cache/mujoco-rs");

    // 尝试友好错误
    if env::var("MUJOCO_DOWNLOAD_DIR").is_err() {
        print_friendly_error();  // 不会执行！
    }
}
```

**问题**：
1. mujoco-rs build.rs 先执行，立即 panic
2. piper-physics build.rs 根本不执行
3. 用户看到的是 mujoco-rs 的错误，不是友好的信息

**结果**: ❌ **不可行**

---

### 方案 B：Wrapper script + 智能 build.rs（✅ 推荐）

**Wrapper script**：
```bash
#!/bin/bash
# 设置环境变量（会传播到所有 build.rs）
export MUJOCO_DYNAMIC_LINK_DIR="$HOME/.cache/mujoco-rs/mujoco-3.3.7/lib"
cargo build
```

**Build.rs**：
```rust
fn main() {
    // 优先级 1: 检查环境变量（wrapper script 设置的）
    if env::var("MUJOCO_DYNAMIC_LINK_DIR").is_ok() {
        return;  // 使用已配置的路径
    }

    // 优先级 2: 检查系统 cache
    let cache_dir = get_default_cache_lib_dir();
    if cache_dir.exists() {
        // 自动使用 cache
        return;
    }

    // 优先级 3: 提供友好错误
    print_friendly_error();
}
```

**优势**：
1. ✅ mujoco-rs 能看到环境变量
2. ✅ piper-physics build.rs 能看到环境变量
3. ✅ 自动检测系统 cache
4. ✅ 友好的错误信息
5. ✅ 用户可以手动设置环境变量（绕过 wrapper）

**结果**: ✅ **完美**

---

## 🎓 关键教训

### 1. Cargo 构建顺序很重要

依赖的 build.rs 先于依赖者执行。这是 Cargo 的设计，不能改变。

### 2. `cargo:rustc-env=` 的局限性

- **设计目的**：设置编译该 crate 时的环境变量
- **不是用来**：传递信息给依赖的 build.rs
- **正确用途**：设置编译时的宏、条件编译等

### 3. Wrapper script 的价值

- ✅ 确保环境变量传播到所有 build.rs
- ✅ 集中管理配置（cache 目录、LD_LIBRARY_PATH）
- ✅ 跨平台兼容性（处理 Linux/macOS/Windows 差异）
- ✅ 用户体验（一次配置，到处使用）

---

## 🚀 最佳实践

### 推荐架构

```
Wrapper Script (build_with_mujoco.sh)
    ↓ 设置环境变量
piper-physics build.rs
    ↓ 检查环境变量 / cache
mujoco-rs build.rs
    ↓ 使用环境变量
下载/使用 MuJoCo
```

### 代码实现

**优先级顺序**：

1. **用户手动设置**（最高优先级）
   ```bash
   export MUJOCO_DYNAMIC_LINK_DIR=/custom/path
   ```

2. **Wrapper script**（推荐）
   ```bash
   ./build_with_mujoco.sh build
   ```

3. **系统 cache 自动检测**（智能 fallback）
   ```rust
   if ~/.cache/mujoco-rs/mujoco-3.3.7/lib exists {
       use it
   }
   ```

4. **友好错误信息**（最后手段）
   ```rust
   print_setup_instructions_and_panic()
   ```

---

## 📊 最终对比表

| 特性 | 只用 build.rs | Wrapper script + build.rs |
|------|--------------|---------------------------|
| **mujoco-rs 能读取环境变量** | ❌ | ✅ |
| **自动使用系统 cache** | ⚠️ 有但无用 | ✅ |
| **友好错误信息** | ⚠️ 有但无用 | ✅ |
| **用户手动配置** | ❌ | ✅ |
| **CI/CD 友好** | ❌ | ✅ |
| **跨平台** | ❌ | ✅ |

---

## ✅ 总结

**回答用户的问题**：

> "既然 build.rs 能覆盖 wrapper script 的设置，是不是意味着只用 build.rs 也是可以的？"

**答案**：❌ **不是的**

**原因**：
1. **执行顺序**：mujoco-rs build.rs 先执行，会立即 panic
2. **`cargo:rustc-env=` 局限性**：不传播到依赖的 build.rs
3. **环境变量传播**：只有 wrapper script 能确保所有 build.rs 都能看到

**最佳方案**：
- ✅ **必须使用 wrapper script**（或手动设置环境变量）
- ✅ **build.rs 做智能检测**（cache、友好错误）
- ✅ **提供多种配置方式**（wrapper、手动、系统安装）

---

**当前实现已经是最佳实践！** 🎉
