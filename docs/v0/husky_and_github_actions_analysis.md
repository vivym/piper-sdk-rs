# Husky 和 GitHub Actions 更新分析报告

**日期**: 2025-02-02
**问题**: 是否应该更新 husky 和 GitHub Actions 配置？
**状态**: ⚠️ **需要立即更新**

---

## 📋 现状分析

### 1. Git Hooks (cargo-husky)

**当前配置**: `.git/hooks/pre-commit`

```bash
#!/bin/sh
# This hook was set by cargo-husky v1.5.0

# 1. cargo fmt --all -- --check
# 2. cargo clippy --all-targets --all-features -- -D warnings
# 3. cargo test
```

**问题**:
- ❌ `cargo test` 会失败（piper-physics 需要 MuJoCo）
- ❌ 本地开发时，每次 commit 都要下载/配置 MuJoCo
- ❌ 测试太慢，影响 commit 流程

---

### 2. GitHub Actions CI

**当前配置**: `.github/workflows/ci.yml`

```yaml
jobs:
  test:
    - name: Run unit tests
      run: cargo test --lib  # ❌ 会失败
```

**问题**:
- ❌ `cargo test --lib` 会失败（piper-physics 需要 MuJoCo）
- ❌ macOS 和 Windows 测试会失败（没有 MuJoCo 配置）
- ❌ 没有使用 just（无法自动下载 MuJoCo）
- ❌ 没有 MuJoCo 缓存（每次都要重新下载）

---

## 🔍 关键问题

### 问题 1: piper-physics 需要 MuJoCo

**现状**:
```bash
$ cargo test --lib
error: MUJOCO_DYNAMIC_LINK_DIR not set
Please use: just build
```

**CI 输出**:
```
❌ cargo test failed
```

---

### 问题 2: 没有安装 just

**CI 配置**:
```yaml
- name: Install Rust toolchain
  uses: dtolnay/rust-toolchain@stable
# ❌ 没有安装 just
```

**问题**: CI 无法运行 `just test`

---

### 问题 3: 没有 MuJoCo 缓存

**当前缓存**:
```yaml
- name: Cache cargo registry
  uses: actions/cache@v3
  with:
    path: |
      ~/.cargo/bin/
      ~/.cargo/registry/index/
      ~/.cargo/registry/cache/
      ~/.cargo/git/db/
      target/  # ❌ 只缓存编译产物
```

**问题**: MuJoCo 安装目录没有被缓存，每次都要重新下载（~13MB）

---

## ✅ 解决方案

### 方案 A: 使用 just（推荐）

#### 更新 Git Hooks

**pre-commit**:
```bash
#!/bin/sh
# This hook was set by cargo-husky v1.5.0

echo "Running cargo fmt..."
cargo fmt --all -- --check
if [ $? -ne 0 ]; then
  echo "❌ Cargo fmt failed."
  exit 1
fi

echo "Running cargo clippy..."
cargo clippy --all-targets --all-features -- -D warnings
if [ $? -ne 0 ]; then
  echo "❌ Cargo clippy failed."
  exit 1
fi

# ❌ 移除 cargo test（太慢，且需要 MuJoCo）
# echo "Running cargo test..."
# cargo test

echo "✅ Pre-commit checks passed!"
```

**理由**:
- ✅ pre-commit 应该快速（< 5 秒）
- ✅ 测试太慢（~30 秒），影响开发体验
- ✅ 测试需要 MuJoCo，每次 commit 都要检查环境

---

#### 更新 GitHub Actions

**完整的 CI 配置**:

```yaml
name: CI

on:
  push:
    branches: [ main, master, develop ]
  pull_request:
    branches: [ main, master, develop ]

env:
  CARGO_TERM_COLOR: always

jobs:
  check:
    name: Check
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4

      - name: Install Rust toolchain
        uses: dtolnay/rust-toolchain@stable

      - name: Check code
        run: cargo check --all-targets

  fmt:
    name: Format
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4

      - name: Install Rust toolchain
        uses: dtolnay/rust-toolchain@stable
        with:
          components: rustfmt

      - name: Check formatting
        run: cargo fmt --all -- --check

  clippy:
    name: Clippy
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4

      - name: Install Rust toolchain
        uses: dtolnay/rust-toolchain@stable
        with:
          components: clippy

      - name: Clippy check
        run: cargo clippy --all-targets --all-features -- -D warnings

  test:
    name: Test (Unit)
    runs-on: ${{ matrix.os }}
    strategy:
      matrix:
        os: [ubuntu-latest, macos-latest, windows-latest]
        rust: [stable]
    steps:
      - uses: actions/checkout@v4

      - name: Install Rust toolchain
        uses: dtolnay/rust-toolchain@stable
        with:
          toolchain: ${{ matrix.rust }}

      # ✅ 新增：安装 just
      - name: Install just
        uses: taiki-e/install-action@v2
        with:
          tool: just

      # ✅ 新增：缓存 MuJoCo 安装
      - name: Cache MuJoCo
        uses: actions/cache@v3
        with:
          path: |
            ~/.local/lib/mujoco  # Linux
            ~/Library/Frameworks/mujoco.framework  # macOS
            %USERPROFILE%\AppData\Local\mujoco  # Windows
          key: mujoco-${{ runner.os }}-3.3.7

      # ✅ 新增：设置 MuJoCo 环境（macOS/Windows）
      - name: Setup MuJoCo (macOS)
        if: matrix.os == 'macos-latest'
        run: |
          # 下载并安装 MuJoCo framework
          mujoco_version="3.3.7"
          framework_dir="$HOME/Library/Frameworks"
          mkdir -p "$framework_dir"

          if [ ! -d "$framework_dir/mujoco.framework/Versions/A" ]; then
            echo "Downloading MuJoCo..."
            curl -L -o "$framework_dir/mujoco-${mujoco_version}.dmg" \
              "https://github.com/google-deepmind/mujoco/releases/download/${mujoco_version}/mujoco-${mujoco_version}-macos-universal.dmg"

            mount_point="/tmp/mujoco_mount_ci"
            hdiutil attach "$framework_dir/mujoco-${mujoco_version}.dmg" \
              -nobrowse -readonly -mountpoint "$mount_point"
            cp -R "$mount_point/mujoco.framework" "$framework_dir/"
            hdiutil detach "$mount_point"
            rm -f "$framework_dir/mujoco-${mujoco_version}.dmg"

            # Remove quarantine
            xattr -r -d com.apple.quarantine "$framework_dir/mujoco.framework" 2>/dev/null || true
          fi

      - name: Setup MuJoCo (Windows)
        if: matrix.os == 'windows-latest'
        run: |
          # 下载并解压 MuJoCo
          $mujocoVersion = "3.3.7"
          $installDir = "$env:LOCALAPPDATA\mujoco"
          $versionDir = "$installDir\mujoco-$mujocoVersion"

          if (-not (Test-Path "$versionDir\lib")) {
            echo "Downloading MuJoCo..."
            New-Item -ItemType Directory -Force -Path "$installDir" | Out-Null
            Invoke-WebRequest -Uri "https://github.com/google-deepmind/mujoco/releases/download/$mujocoVersion/mujoco-$mujocoVersion-windows-x86_64.zip" -OutFile "$installDir\mujoco.zip"
            Expand-Archive -Path "$installDir\mujoco.zip" -DestinationPath "$installDir" -Force
            Remove-Item "$installDir\mujoco.zip"
          }

      - name: Setup vcan0 for Linux
        if: matrix.os == 'ubuntu-latest'
        run: |
          sudo modprobe vcan || echo "Warning: vcan module not available"
          sudo ip link add dev vcan0 type vcan || echo "Warning: Failed to create vcan0"
          sudo ip link set up vcan0 || echo "Warning: Failed to bring up vcan0"

      - name: Cache cargo registry
        uses: actions/cache@v3
        with:
          path: |
            ~/.cargo/bin/
            ~/.cargo/registry/index/
            ~/.cargo/registry/cache/
            ~/.cargo/git/db/
            target/
          key: ${{ runner.os }}-cargo-${{ hashFiles('**/Cargo.lock') }}
          restore-keys: |
            ${{ runner.os }}-cargo-

      # ✅ 使用 just 运行测试
      - name: Run unit tests
        run: just test

  docs:
    name: Documentation
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4

      - name: Install Rust toolchain
        uses: dtolnay/rust-toolchain@stable
        with:
          components: rust-docs

      - name: Cache cargo registry
        uses: actions/cache@v3
        with:
          path: |
            ~/.cargo/bin/
            ~/.cargo/registry/index/
            ~/.cargo/registry/cache/
            ~/.cargo/git/db/
            target/
          key: ${{ runner.os }}-cargo-${{ hashFiles('**/Cargo.lock') }}
          restore-keys: |
            ${{ runner.os }}-cargo-

      - name: Build documentation
        run: cargo doc --no-deps --document-private-items

      - name: Check documentation links
        run: cargo doc --no-deps --document-private-items 2>&1 | grep -i "warning\|error" && exit 1 || exit 0
```

---

## 📊 改进对比

### Pre-commit Hook

| 场景 | 当前 | 改进后 |
|------|------|--------|
| **执行时间** | ~30 秒（含测试） | ~2 秒（不含测试） |
| **失败率** | 高（MuJoCo 问题） | 低（只检查格式） |
| **用户体验** | 每次都要等 | 快速反馈 |

**理由**:
- pre-commit 应该快速（< 5 秒）
- 测试应该在 pre-push 或 CI 中运行
- 用户可以快速迭代，不被测试阻塞

---

### GitHub Actions

| 场景 | 当前 | 改进后 |
|------|------|--------|
| **Linux 测试** | ❌ 失败（无 MuJoCo） | ✅ 成功（just 下载） |
| **macOS 测试** | ❌ 失败（无 MuJoCo） | ✅ 成功（DMG 安装） |
| **Windows 测试** | ❌ 失败（无 MuJoCo） | ✅ 成功（ZIP 安装） |
| **MuJoCo 缓存** | ❌ 无 | ✅ 缓存 |
| **构建时间** | 慢（每次下载） | 快（缓存命中） |

---

## 🎯 推荐实施步骤

### 步骤 1: 更新 pre-commit hook（立即）

```bash
# 编辑 .git/hooks/pre-commit
# 移除 cargo test 部分
```

**效果**: commit 更快，体验更好

---

### 步骤 2: 更新 GitHub Actions（立即）

**关键改动**:
1. ✅ 安装 just
2. ✅ 添加 MuJoCo 缓存
3. ✅ 添加 MuJoCo 安装步骤（macOS/Windows）
4. ✅ 使用 `just test` 替代 `cargo test`

**效果**: 所有平台测试通过

---

### 步骤 3: 添加 pre-push hook（可选）

```bash
#!/bin/sh
# 在 push 时运行完整测试
echo "Running full test suite..."
just test
if [ $? -ne 0 ]; then
  echo "❌ Tests failed. Aborting push."
  exit 1
fi
echo "✅ All tests passed!"
```

**理由**:
- pre-commit 快速（格式 + clippy）
- pre-push 完整（测试）
- 平衡体验和安全性

---

## ✅ 总结

### 问题总结

| 组件 | 问题 | 优先级 |
|------|------|--------|
| **pre-commit** | `cargo test` 太慢且会失败 | 🔴 高 |
| **GitHub Actions** | `cargo test` 失败（无 MuJoCo） | 🔴 高 |
| **GitHub Actions** | 没有安装 just | 🔴 高 |
| **GitHub Actions** | 没有 MuJoCo 缓存 | 🟡 中 |

### 推荐行动

1. ✅ **立即**: 更新 pre-commit（移除 `cargo test`）
2. ✅ **立即**: 更新 GitHub Actions（安装 just，使用 MuJoCo）
3. ⚠️ **可选**: 添加 pre-push hook（运行测试）

### 预期效果

- ✅ **本地开发**: commit 更快，体验更好
- ✅ **CI/CD**: 所有平台测试通过
- ✅ **构建速度**: MuJoCo 缓存加速
- ✅ **开发体验**: 清晰的职责分离

---

**下一步**: 是否立即执行这些更新？
