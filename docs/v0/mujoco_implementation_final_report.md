# MuJoCo 自动下载实现完成报告

**日期**: 2025-02-02
**状态**: ✅ 实现完成并测试通过

---

## 执行总结

成功实现了 piper-physics crate 的 MuJoCo 自动下载功能，所有测试通过。

### 关键成果

- ✅ **编译成功**: piper-physics 成功编译并链接 MuJoCo
- ✅ **测试通过**: 所有 12 个单元测试通过
- ✅ **自动化**: MuJoCo 在首次构建时自动下载
- ✅ **跨平台**: 支持 Linux、Windows、macOS (macOS 需手动安装)

---

## 实现方案

### 方案 A (已实施): 启用 mujoco-rs 的 auto-download-mujoco feature

**架构设计**:

```
┌─────────────────────────────────────────────────────────┐
│           build_with_mujoco.sh (wrapper script)        │
│  ┌─────────────────────────────────────────────────┐   │
│  │ 1. 设置 MUJOCO_DOWNLOAD_DIR                    │   │
│  │ 2. 设置 LD_LIBRARY_PATH (runtime linking)      │   │
│  │ 3. 调用 cargo                                  │   │
│  └─────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────┘
                         ↓
┌─────────────────────────────────────────────────────────┐
│              piper-physics build.rs                     │
│  ┌─────────────────────────────────────────────────┐   │
│  │ 1. 设置 MUJOCO_DOWNLOAD_DIR (for mujoco-rs)    │   │
│  │ 2. 检测 MUJOCO_DOWNLOAD_DIR 是否已设置          │   │
│  │ 3. 如果已设置，跳过自定义下载逻辑                │   │
│  └─────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────┘
                         ↓
┌─────────────────────────────────────────────────────────┐
│                mujoco-rs build.rs                       │
│  ┌─────────────────────────────────────────────────┐   │
│  │ 1. 读取 MUJOCO_DOWNLOAD_DIR                     │   │
│  │ 2. 下载 MuJoCo 到指定目录                       │   │
│  │ 3. 验证 SHA256                                 │   │
│  │ 4. 解压并设置 linker paths                     │   │
│  └─────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────┘
```

---

## 修改文件列表

### 1. `crates/piper-physics/Cargo.toml`

**修改**:
```toml
# Before
mujoco-rs = "2.3"

# After
mujoco-rs = { version = "2.3", features = ["auto-download-mujoco"] }
```

**原因**: 启用 mujoco-rs 的自动下载功能

---

### 2. `crates/piper-physics/build.rs`

**关键修改**:

```rust
fn main() {
    // 1. 设置 MUJOCO_DOWNLOAD_DIR 给 mujoco-rs
    let workspace_root = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap())
        .parent().unwrap().parent().unwrap().to_path_buf();
    let mujoco_download_dir = workspace_root.join(".mujoco");
    let mujoco_download_dir_abs = mujoco_download_dir.canonicalize()
        .unwrap_or_else(|_| {
            fs::create_dir_all(&mujoco_download_dir).unwrap();
            mujoco_download_dir.canonicalize().unwrap()
        });

    println!("cargo:rustc-env=MUJOCO_DOWNLOAD_DIR={}",
        mujoco_download_dir_abs.display());

    // 2. 如果 MUJOCO_DOWNLOAD_DIR 已设置（外部调用），
    //    mujoco-rs 会处理下载，跳过我们的自定义逻辑
    if env::var("MUJOCO_DOWNLOAD_DIR").is_ok() {
        println!("cargo:warning=Using mujoco-rs auto-download");
        return;
    }

    // 3. 否则，使用自定义下载逻辑（用于直接 cargo 调用）
    // ... (原有代码保留)
}
```

**优化**:
- 移除了未使用的 `Path` import
- 添加了早期返回逻辑，避免重复下载

---

### 3. `build_with_mujoco.sh` (新建)

**功能**:
```bash
#!/bin/bash
# 设置 MuJoCo 环境变量并调用 cargo

PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MUJOCO_DIR="${PROJECT_ROOT}/.mujoco"

# 关键环境变量
export MUJOCO_DOWNLOAD_DIR="${MUJOCO_DIR}"
export LD_LIBRARY_PATH="${MUJOCO_DIR}/mujoco-3.3.7/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"

# 调用 cargo
cargo "$@"
```

**用途**:
- 简化用户操作
- 确保 MUJOCO_DOWNLOAD_DIR 正确设置
- 设置 LD_LIBRARY_PATH 用于运行时链接

---

## 技术决策

### 为什么选择 wrapper script 方案？

**问题**: `cargo:rustc-env=` 不会传递给依赖的 build.rs

**方案对比**:

| 方案 | 优点 | 缺点 | 选择 |
|------|------|------|------|
| **Wrapper Script** | 简单、可靠、无需修改 Cargo | 用户需要使用脚本 | ✅ **采用** |
| 修改 .cargo/config.toml | 对用户透明 | Cargo 不支持此功能 | ❌ 不可行 |
| dotenv crate | 对用户透明 | 需要大量代码修改 | ❌ 工作量大 |
| 环境变量文档 | 最简单 | 用户体验差 | ⚠️ 备选方案 |

---

## 使用方式

### 标准使用（推荐）

```bash
# 构建
./build_with_mujoco.sh build

# 测试
./build_with_mujoco.sh test

# 发布构建
./build_with_mujoco.sh build --release
```

### 高级使用（直接使用 cargo）

```bash
# 1. 设置环境变量
export MUJOCO_DOWNLOAD_DIR="$(pwd)/.mujoco"
export LD_LIBRARY_PATH="$(pwd)/.mujoco/mujoco-3.3.7/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"

# 2. 正常使用 cargo
cargo build
cargo test
```

---

## 测试结果

### 单元测试

```bash
$ ./build_with_mujoco.sh test -p piper-physics --lib

running 12 tests
test mujoco::tests::test_column_major_indexing_is_wrong ... ok
test mujoco::tests::test_com_offset_calculation ... ok
test mujoco::tests::test_ffi_pointer_creation ... ok
test mujoco::tests::test_row_major_matrix_conversion ... ok
test tests::test_column_major_indexing_is_wrong ... ok
test tests::test_com_offset_calculation ... ok
test tests::test_ffi_pointer_creation ... ok
test tests::test_row_major_matrix_conversion ... ok
test mujoco::tests::test_default_initialization ... ok
test mujoco::tests::test_gravity_compensation_matches_partial_at_zero_velocity ... ok
test mujoco::tests::test_partial_inverse_dynamics_includes_coriolis ... ok
test mujoco::tests::test_full_inverse_dynamics_includes_inertia ... ok

test result: ok. 12 passed; 0 failed; 0 ignored; 0 measured
```

### 文件结构

```
piper-sdk-rs/
├── .mujoco/                          # MuJoCo 下载目录
│   └── mujoco-3.3.7/
│       ├── include/                  # 头文件
│       ├── lib/                      # 库文件
│       │   ├── libmujoco.so
│       │   └── libmujoco.so.3.3.7
│       └── bin/                      # 工具和插件
├── build_with_mujoco.sh              # 构建脚本
├── crates/
│   └── piper-physics/
│       ├── Cargo.toml                # 已修改
│       └── build.rs                  # 已修改
└── docs/v0/
    └── mujoco_*.md                   # 文档报告
```

---

## 平台支持

### Linux ✅
- 自动下载: 支持
- 库格式: `libmujoco.so`
- 环境变量: `MUJOCO_DOWNLOAD_DIR` + `LD_LIBRARY_PATH`

### Windows ✅
- 自动下载: 支持
- 库格式: `mujoco.dll`
- 环境变量: `MUJOCO_DOWNLOAD_DIR` (DLL 自动复制到 target/)

### macOS ⚠️
- 自动下载: **不支持**
- 解决方案: 用户手动安装或使用 Homebrew
  ```bash
  brew install mujoco pkgconf
  export MUJOCO_DYNAMIC_LINK_DIR="/path/to/mujoco"
  ```

---

## 依赖版本

### 关键依赖

```toml
[dependencies]
mujoco-rs = { version = "2.3", features = ["auto-download-mujoco"] }

[build-dependencies]
flate2 = "1.0"      # 版本范围，与 mujoco-rs 兼容
ureq = "2.9"
tar = "0.4"
zip = "0.6"
dirs = "5.0"
```

**注意**: flate2 使用版本范围 `"1.0"` 而非精确版本，以避免与 mujoco-rs 的依赖冲突。

---

## CI/CD 集成

### GitHub Actions 示例

```yaml
name: Test

on: [push, pull_request]

jobs:
  test:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v3
      - uses: actions-rs/toolchain@v1
        with:
          toolchain: stable

      # 使用构建脚本（推荐）
      - name: Run tests
        run: ./build_with_mujoco.sh test --workspace

      # 或者直接设置环境变量
      # - name: Set MuJoCo environment
      #   run: |
      #     echo "MUJOCO_DOWNLOAD_DIR=$PWD/.mujoco" >> $GITHUB_ENV
      #     echo "LD_LIBRARY_PATH=$PWD/.mujoco/mujoco-3.3.7/lib" >> $GITHUB_ENV
      # - name: Run tests
      #   run: cargo test --workspace
```

---

## 故障排除

### 问题 1: Timeout during download

**错误**:
```
thread 'main' panicked at mujoco-rs/build.rs:320:64:
failed to download MuJoCo: Timeout(Global)
```

**解决方案**:
- 检查网络连接
- 手动下载 MuJoCo 到 `.mujoco/` 目录
- 使用代理设置 `HTTP_PROXY` 和 `HTTPS_PROXY`

---

### 问题 2: libmujoco.so.3.3.7: cannot open shared object file

**错误**:
```
error while loading shared libraries: libmujoco.so.3.3.7: cannot open shared object file
```

**解决方案**:
```bash
# 使用构建脚本（推荐）
./build_with_mujoco.sh test

# 或手动设置 LD_LIBRARY_PATH
export LD_LIBRARY_PATH="$(pwd)/.mujoco/mujoco-3.3.7/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
cargo test
```

---

### 问题 3: macOS 编译失败

**错误**:
```
Unable to locate MuJoCo via pkg-config and neither MUJOCO_STATIC_LINK_DIR
nor MUJOCO_DYNAMIC_LINK_DIR is set.
```

**解决方案**:
```bash
# macOS 不支持自动下载，需手动安装
brew install mujoco pkgconf

# 设置 MUJOCO_DYNAMIC_LINK_DIR
export MUJOCO_DYNAMIC_LINK_DIR="$(brew --prefix mujoco)/lib"
cargo test
```

---

## 文档

### 创建的文档

1. **mujoco_dependency_analysis_report.md** - 问题分析报告
2. **mujoco_solution_v2.md** - 源码分析报告
3. **mujoco_implementation_final_report.md** (本文) - 实现总结报告

---

## 下一步

### 短期 (已完成)
- ✅ 修复 MuJoCo 自动下载
- ✅ 创建构建脚本
- ✅ 测试验证
- ✅ 清理代码

### 长期 (可选)
- [ ] 添加 macOS 自动下载支持（需 fork mujoco-rs）
- [ ] 集成到 CI/CD
- [ ] 添加 Makefile 简化操作
- [ ] 考虑使用 vendored MuJoCo（离线构建）

---

## 参考资料

- [mujoco-rs GitHub](https://github.com/Computational-Robotics/mujoco-rs)
- [MuJoCo 官方文档](https://github.com/google-deepmind/mujoco)
- [Cargo Build Scripts](https://doc.rust-lang.org/cargo/reference/build-scripts.html)
- [RFC 3013: Weak Dependency Features](https://rust-lang.github.io/rfcs/3013-weak-dependency-features.html)

---

**作者**: Claude (Anthropic)
**日期**: 2025-02-02
**版本**: 1.0
