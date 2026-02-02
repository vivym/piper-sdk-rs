# MuJoCo 依赖配置分析与解决方案

**日期**: 2025-02-02
**问题**: `cargo test` 编译失败，MuJoCo 库未找到
**状态**: 🔍 分析完成，待执行修复

---

## 1. 问题根源分析

### 1.1 错误信息

```bash
error: failed to run custom build command for `mujoco-rs v2.3.0+mj-3.3.7`

The system library `mujoco` required by crate `mujoco-rs` was not found.
Unable to locate MuJoCo via pkg-config and neither MUJOCO_DYNAMIC_LINK_DIR
nor MUJOCO_DYNAMIC_LINK_DIR is set and the 'auto-download-mujoco' Cargo feature
is disabled.

Consider enabling automatic download of MuJoCo:
'cargo add mujoco-rs --features "auto-download-mujoco"'.
```

### 1.2 依赖链分析

```
piper-physics
    ↓ depends on
mujoco-rs (2.3.0)
    ↓ requires native library
libmujoco (system dependency)
```

**当前配置** (`crates/piper-physics/Cargo.toml:14`):
```toml
mujoco-rs = "2.3"  # ❌ 没有启用任何 feature
```

### 1.3 架构冲突

**关键发现**: piper-physics 存在 **双重自动化机制**，但两者未协调工作：

#### 机制 A: piper-physics 的自定义 build.rs
- **文件**: `crates/piper-physics/build.rs` (597 行)
- **功能**:
  - ✅ 自动下载 MuJoCo (Linux/macOS/Windows)
  - ✅ 自动配置 linker paths
  - ✅ 自动嵌入 RPATH
  - ✅ 支持手动路径 (`MUJOCO_DYNAMIC_LINK_DIR`)
  - ✅ 生成环境变量脚本

#### 机制 B: mujoco-rs 的内置功能
- **Feature**: `auto-download-mujoco`
- **功能**:
  - ✅ 自动下载 MuJoCo
  - ⚠️ **仅支持 Linux 和 Windows** ([文档](https://mujoco-rs.readthedocs.io/en/latest/installation.html))
  - ❌ **不支持 macOS**

**问题**: 机制 A 下载了 MuJoCo，但机制 B (mujoco-rs) 不知道，导致找不到库。

---

## 2. 为什么 `auto-download-mujoco` 未启用？

### 2.1 历史遗留

查看 `crates/piper-physics/build.rs` 的注释：

```rust
//! MuJoCo automatic configuration build script
//!
//! This build script automatically downloads and configures MuJoCo for all platforms:
//! - Linux: Downloads tar.gz, extracts to ~/.local/lib/mujoco/
//! - macOS: Downloads DMG, mounts, copies framework, removes quarantine
//! - Windows: Downloads ZIP, extracts to %LOCALAPPDATA%\mujoco\
//!
//! If MUJOCO_DYNAMIC_LINK_DIR is already set, automatic configuration is skipped.
```

**推断**:
- piper-physics 开发时，mujoco-rs 的 `auto-download-mujoco` feature 还不存在
- 或者当时该功能不支持 macOS，所以自己实现了完整的 build.rs

### 2.2 依赖配置缺失

**问题代码** (`crates/piper-physics/Cargo.toml:14`):
```toml
mujoco-rs = "2.3"  # ❌ 缺少 features 声明
```

**正确配置应该是**:
```toml
mujoco-rs = { version = "2.3", features = ["auto-download-mujoco"] }  # ✅
```

但是，这样会与 piper-physics 的 build.rs 产生冲突或重复工作。

---

## 3. 解决方案对比

### 方案 A: 移除自定义 build.rs，使用 mujoco-rs 的 feature

**修改**:
```toml
# crates/piper-physics/Cargo.toml
mujoco-rs = { version = "2.3", features = ["auto-download-mujoco"] }
```

**优点**:
- ✅ 简化维护 (移除 597 行 build.rs)
- ✅ 利用上游维护的功能
- ✅ 减少依赖 (ureq, flate2, tar, zip, dirs)

**缺点**:
- ❌ **失去 macOS 支持** (mujoco-rs 的 auto-download 不支持 macOS)
- ❌ 失去自定义 RPATH 嵌入逻辑
- ❌ 失去环境变量脚本生成
- ❌ macOS 用户必须手动安装 MuJoCo

**结论**: ❌ **不推荐** (会破坏 macOS 用户体验)

---

### 方案 B: 启用 mujoco-rs feature + 环境变量协调 (推荐)

**核心思路**: 让 piper-physics 的 build.rs 下载 MuJoCo，然后设置环境变量告诉 mujoco-rs。

**步骤**:

1. **修改 piper-physics/Cargo.toml**:
```toml
mujoco-rs = { version = "2.3", features = ["auto-download-mujoco"] }
```

2. **修改 piper-physics/build.rs**，在下载完成后设置环境变量:
```rust
// 在 configure_linux() / configure_macos() / configure_windows() 中
// 下载完成后，告诉 cargo 将路径传递给 mujoco-rs 的 build.rs
println!("cargo:rustc-env=MUJOCO_DYNAMIC_LINK_DIR={}", lib_path);
```

**优点**:
- ✅ **保留 macOS 支持** (piper-physics 的 build.rs 下载)
- ✅ **兼容 Linux/Windows** (两者都能处理，优先用 piper-physics 的)
- ✅ **防止重复下载** (mujoco-rs 检测到 MUJOCO_DYNAMIC_LINK_DIR 会跳过下载)
- ✅ **保持自定义功能** (RPATH 嵌入，脚本生成)

**缺点**:
- ⚠️ 需要测试确保两个 build.rs 协调工作
- ⚠️ Linux/Windows 会有两个下载机制并存 (冗余但无害)

**结论**: ✅ **推荐** (平衡兼容性和维护成本)

---

### 方案 C: 禁用 mujoco-rs 的 feature，完全依赖自定义 build.rs

**修改**:
```toml
# crates/piper-physics/Cargo.toml
# 保持原样，不添加 features
mujoco-rs = "2.3"
```

**修改 build.rs**，确保环境变量传递给 mujoco-rs:
```rust
// 在 configure_linux() / configure_macos() / configure_windows() 中
// 关键：必须通过 cargo:rustc-env 传递给依赖 crate 的 build.rs
println!("cargo:rustc-env=MUJOCO_DYNAMIC_LINK_DIR={}", lib_path);
```

**优点**:
- ✅ **完全控制** (不依赖 mujoco-rs 的内部实现)
- ✅ **macOS 支持完美** (已有实现)
- ✅ **代码不变** (只需添加环境变量输出)

**缺点**:
- ⚠️ 需要维护自定义 build.rs (597 行)
- ⚠️ 如果 mujoco-rs 更新依赖，需要跟进测试

**结论**: ✅ **次优选择** (最小改动，风险最低)

---

## 4. 推荐执行方案

### 短期方案 (立即可执行)

**方案 C - 最小改动**:

1. **修改 `crates/piper-physics/build.rs`**:
   - 在 `configure_linux()` 的最后添加:
     ```rust
     println!("cargo:rustc-env=MUJOCO_DYNAMIC_LINK_DIR={}", lib_path);
     ```
   - 在 `configure_macos()` 的 `setup_macos_linking()` 中添加:
     ```rust
     println!("cargo:rustc-env=MUJOCO_DYNAMIC_LINK_DIR={}", lib_path);
     ```
   - 在 `configure_windows()` 的最后添加:
     ```rust
     println!("cargo:rustc-env=MUJOCO_DYNAMIC_LINK_DIR={}", lib_path);
     ```

2. **保持 `Cargo.toml` 不变**:
   ```toml
   mujoco-rs = "2.3"  # 不添加 features
   ```

**原理**:
- piper-physics 的 build.rs 先运行，下载 MuJoCo 并设置 `MUJOCO_DYNAMIC_LINK_DIR`
- 通过 `cargo:rustc-env=` 将环境变量传递给依赖 crate (mujoco-rs) 的 build.rs
- mujoco-rs 的 build.rs 检测到 `MUJOCO_DYNAMIC_LINK_DIR`，跳过下载，直接使用

**测试验证**:
```bash
cargo clean
cargo test --workspace  # 应该成功，不再报错
```

---

### 长期方案 (考虑未来优化)

**条件**: 当 mujoco-rs 的 `auto-download-mujoco` 支持 macOS 时，迁移到方案 B。

**迁移步骤**:
1. 添加 `features = ["auto-download-mujoco"]`
2. 简化 build.rs (移除下载逻辑，保留 RPATH 嵌入)
3. 大幅减少依赖 (ureq, flate2, tar, zip, dirs)

**收益**:
- 减少约 400 行代码
- 减少 5 个依赖
- 简化维护

---

## 5. 技术细节补充

### 5.1 cargo:rustc-env 的作用

```rust
// piper-physics/build.rs
println!("cargo:rustc-env=MUJOCO_DYNAMIC_LINK_DIR=/home/user/.local/lib/mujoco/current/lib");
```

**效果**:
- 编译 piper-physics 时，该环境变量对 piper-physics 可见
- 编译依赖 mujoco-rs 时，该环境变量也会传递给 mujoco-rs 的 build.rs
- mujoco-rs 的 build.rs 会检查这个变量，如果已设置则跳过下载

**参考**: [Cargo Build Script Output - rustc-env](https://doc.rust-lang.org/cargo/reference/build-scripts.html#rustc-env)

### 5.2 build.rs 执行顺序

```
编译顺序:
1. piper-physics build.rs (下载 MuJoCo)
   └─> 设置 cargo:rustc-env=MUJOCO_DYNAMIC_LINK_DIR
2. mujoco-rs build.rs (检查环境变量)
   └─> 检测到 MUJOCO_DYNAMIC_LINK_DIR
   └─> 跳过下载，直接使用
3. 编译 piper-physics/src/lib.rs (链接 MuJoCo)
```

### 5.3 macOS 为何特殊

**mujoco-rs 限制** ([文档](https://mujoco-rs.readthedocs.io/en/latest/installation.html)):
> Automatic download is only available on Linux and Windows.
> macOS users must download MuJoCo manually.

**原因**:
- macOS 使用 Framework Bundle (.framework) 而非简单的 .dylib
- 需要挂载 DMG、处理 quarantine 属性、复制 Framework
- 这些操作复杂度高，mujoco-rs 选择不实现

**piper-physics 的实现** (`build.rs:355-458`):
- ✅ 完整支持 macOS DMG 下载
- ✅ 自动挂载/卸载
- ✅ 移除 quarantine 属性
- ✅ 复制 Framework 到 `~/Library/Frameworks/`
- ✅ 设置正确的 RPATH

---

## 6. 风险评估

### 短期方案 (方案 C)

| 风险 | 概率 | 影响 | 缓解措施 |
|------|------|------|----------|
| `cargo:rustc-env=` 不传递给依赖 | 低 | 高 | 测试验证；备选方案 B |
| macOS DMG 挂载失败 | 低 | 中 | 已有完善错误处理 |
| Linux 系统缺少 tar/gzip | 低 | 低 | 所有 Linux 发行版都预装 |
| Windows 权限问题 | 中 | 中 | 已使用 LOCALAPPDATA (用户目录) |

### 总结

**推荐执行**: ✅ **方案 C (最小改动)**

**理由**:
1. 改动最小 (只修改 build.rs，不修改 Cargo.toml)
2. 风险最低 (不改变依赖配置)
3. 向后兼容 (不破坏现有功能)
4. macOS 支持完整 (保持优势)

**下一步**:
1. 修改 `crates/piper-physics/build.rs`
2. 运行 `cargo clean && cargo test --workspace` 验证
3. 提交 PR 并进行 CI 测试

---

## 7. 参考资料

- [mujoco-rs Installation Guide](https://mujoco-rs.readthedocs.io/en/latest/installation.html)
- [mujoco-rs on crates.io](https://crates.io/crates/mujoco-rs)
- [Cargo Build Script Output](https://doc.rust-lang.org/cargo/reference/build-scripts.html#rustc-env)
- [MuJoCo Official Build Instructions](https://github.com/google-deepmind/mujoco/blob/main/BUILD.md)
