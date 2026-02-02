# MuJoCo 依赖配置解决方案 v2.0

**日期**: 2025-02-02
**状态**: ✅ 源码分析完成

---

## 关键发现 (从 mujoco-rs/build.rs 源码)

### 1. mujoco-rs 的自动下载机制

**环境变量要求**:
```rust
// line 293-304
let download_dir = PathBuf::from(std::env::var(MUJOCO_DOWNLOAD_PATH_VAR).unwrap_or_else(|_| {
    panic!(
        "when Cargo feature 'auto-download-mujoco' is enabled, {MUJOCO_DOWNLOAD_PATH_VAR} must be set to \
        an absolute path, where MuJoCo will be extracted --- \
        {os_example}",
    );
}));
```

**关键点**:
- ✅ **Linux**: 支持 `auto-download-mujoco` feature
- ✅ **Windows**: 支持 `auto-download-mujoco` feature
- ❌ **macOS**: **不支持自动下载** (line 228-235)
- ⚠️ **必须设置**: `MUJOCO_DOWNLOAD_DIR` 环境变量（绝对路径）

### 2. 为什么之前的方案失败

**Build.rs 执行顺序**:
```
1. mujoco-rs build.rs 执行 (依赖)
   └─> 检查 MUJOCO_DOWNLOAD_DIR
   └─> 未设置 → panic (line 293-304)

2. piper-physics build.rs 执行 (依赖者)
   └─> 设置 cargo:rustc-env=MUJOCO_DOWNLOAD_DIR
   └─> 但为时已晚，mujoco-rs 已经失败
```

**根本问题**: `cargo:rustc-env=` 只影响编译时的环境变量，**不会传递给依赖 crate 的 build.rs**。

---

## 正确的解决方案

### 方案 A: 启用 mujoco-rs 的 auto-download-mujoco feature

**修改 `crates/piper-physics/Cargo.toml`**:
```toml
[dependencies]
mujoco-rs = { version = "2.3", features = ["auto-download-mujoco"] }
```

**修改 `crates/piper-physics/build.rs`**，在 `main()` 开头添加：
```rust
fn main() {
    // 设置 MUJOCO_DOWNLOAD_DIR，让 mujoco-rs 知道在哪里解压
    // 使用项目根目录下的 .mujoco 目录
    let mujoco_download_dir = std::env::current_dir()
        .unwrap()
        .join(".mujoco")
        .canonicalize()
        .unwrap();

    println!("cargo:rustc-env=MUJOCO_DOWNLOAD_DIR={}", mujoco_download_dir.display());
    println!("cargo:warning=MuJoCo will be downloaded to: {}", mujoco_download_dir.display());

    // ... 原有的代码
}
```

**使用方式**:
```bash
cargo test --workspace
# mujoco-rs 会自动下载 MuJoCo 到项目根目录的 .mujoco/
```

**优点**:
- ✅ **简单直接**（最小代码改动）
- ✅ **利用上游维护的功能**
- ✅ **SHA256 验证**（安全）

**缺点**:
- ❌ **macOS 不支持**（用户仍需手动安装）
- ⚠️ 依赖 mujoco-rs 的内部实现

---

### 方案 B: 移除 mujoco-rs，直接链接 MuJoCo

**修改 `crates/piper-physics/Cargo.toml`**:
```toml
[dependencies]
# 移除 mujoco-rs，改为使用 cc crate 直接编译
# mujoco-rs = "2.3"  # ❌ 删除

cc = "1.0"  # 用于编译 C++ 库
```

**修改 `crates/piper-physics/build.rs`**:
- 保留现有的自动下载逻辑（Linux/macOS/Windows）
- 使用 `cc::Build` 编译 MuJoCo C++ 库

**优点**:
- ✅ **完全控制**
- ✅ **macOS 支持完美**
- ✅ **不依赖外部 crate 的 build 脚本**

**缺点**:
- ❌ **工程量大**（需要重构）
- ❌ **维护成本高**
- ❌ 需要手动生成 FFI bindings

---

### 方案 C: 混合模式（推荐）

**核心思路**: 根据平台选择不同的策略

**修改 `crates/piper-physics/Cargo.toml`**:
```toml
[dependencies]
mujoco-rs = { version = "2.3", optional = true, features = ["auto-download-mujoco"] }
cc = "1.0"

[features]
default = ["mujoco-auto"]
mujoco-auto = ["dep:mujoco-rs"]  # Linux/Windows 使用 mujoco-rs 自动下载
mujoco-manual = []  # macOS 使用手动编译
```

**修改 `crates/piper-physics/build.rs`**:
```rust
fn main() {
    #[cfg(feature = "mujoco-auto")]
    {
        // Linux/Windows: 设置 MUJOCO_DOWNLOAD_DIR
        let mujoco_download_dir = std::env::current_dir()
            .unwrap()
            .join(".mujoco")
            .canonicalize()
            .unwrap();
        println!("cargo:rustc-env=MUJOCO_DOWNLOAD_DIR={}", mujoco_download_dir.display());
    }

    #[cfg(not(feature = "mujoco-auto"))]
    {
        // macOS: 使用现有的自定义下载逻辑
        // ... 保留现有的 configure_macos() 等代码
    }
}
```

**使用方式**:
```bash
# Linux/Windows
cargo test --workspace --features mujoco-auto

# macOS (用户需手动安装 MuJoCo)
cargo test --workspace --features mujoco-manual
```

**优点**:
- ✅ **灵活性高**
- ✅ **平台优化**
- ✅ **渐进式迁移**

**缺点**:
- ⚠️ 用户需要了解 feature 区别
- ⚠️ macOS 体验不如 Linux/Windows

---

## 最终推荐

### 短期方案（立即可执行）

**方案 A - 启用 mujoco-rs 自动下载**:
1. 修改 `Cargo.toml`: 添加 `features = ["auto-download-mujoco"]`
2. 修改 `build.rs`: 设置 `MUJOCO_DOWNLOAD_DIR`
3. macOS 用户需手动安装 MuJoCo

### 长期方案（考虑实现）

**方案 B - 直接链接 MuJoCo**:
- 移除对 mujoco-rs 的依赖
- 使用 cc crate 编译 MuJoCo
- 完全控制三个平台的下载和编译逻辑

---

## 立即执行（方案 A）

让我知道是否要执行方案 A。
