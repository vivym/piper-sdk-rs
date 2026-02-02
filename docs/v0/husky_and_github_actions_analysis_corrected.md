# Husky 和 GitHub Actions 更新分析报告（修正版）

**日期**: 2025-02-02
**问题**: 是否应该更新 husky 和 GitHub Actions 配置？
**状态**: ⚠️ **需要立即更新**
**修正**: 修复了原分析中的 3 个严重技术隐患

---

## 📋 修正说明

原分析报告存在以下**严重技术问题**，已在本版本中修正：

1. ❌ **违反 DRY 原则**：在 CI 中重复实现下载逻辑
2. ❌ **环境变量不共享**：GitHub Actions step 之间环境变量丢失
3. ❌ **缓存路径不一致**：CI 缓存路径与 justfile 实际路径不匹配

---

## 🔴 原分析的问题

### 问题 1: 违反 DRY 原则

**原方案**:
```yaml
# ❌ 在 CI 中重新实现下载逻辑
- name: Setup MuJoCo (macOS)
  run: |
    curl -L -o mujoco.dmg ...
    hdiutil attach ...
    cp -R ...
```

**问题**:
- 需要同时维护 `justfile` 和 `ci.yml` 两份逻辑
- 本地开发用 just，CI 用 shell → **不一致风险**
- 升级 MuJoCo 版本需要改两处

**正确做法**: 复用 `just _mujoco_download`

---

### 问题 2: 环境变量不共享

**GitHub Actions 的机制**:
- 每个 `run` step 是独立的 shell 进程
- Step A 中的 `export VAR=VAL` **不会**传递到 Step B

**原方案**:
```yaml
# ❌ 环境变量不会传递
- name: Setup MuJoCo
  run: |
    export MUJOCO_DYNAMIC_LINK_DIR=/path
    just _mujoco_download  # 输出 export 语句

- name: Run tests
  run: just test  # ❌ 看不到环境变量
```

**正确做法**: 写入 `$GITHUB_ENV`

---

### 问题 3: 缓存路径不一致

**justfile 实际路径**:
```bash
Linux:   ~/.local/lib/mujoco/
macOS:   ~/Library/Frameworks/mujoco.framework/
Windows: %LOCALAPPDATA%/mujoco/
```

**原 CI 配置**:
```yaml
# ❌ 路径不匹配
path: ~/.cache/mujoco-rs  # 错误！
```

**后果**: CI 以为缓存了，但脚本去另一个目录找 → **缓存失效，每次重新下载**

---

## ✅ 正确的解决方案

### 1. 更新 pre-commit hook

**`.git/hooks/pre-commit`**:
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
# pre-commit 应该快速（< 5 秒）
# 测试应该在 pre-push 或 CI 中运行

echo "✅ Pre-commit checks passed!"
```

---

### 2. 正确的 GitHub Actions 配置

**关键原则**:
1. ✅ **复用 justfile 逻辑**（DRY）
2. ✅ **使用 $GITHUB_ENV**（环境变量共享）
3. ✅ **路径严格一致**（缓存有效）

**完整的 `test` job**:

```yaml
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

      # ✅ 安装 just
      - name: Install just
        uses: taiki-e/install-action@v2
        with:
          tool: just

      # ✅ 缓存 MuJoCo（路径与 justfile 严格一致）
      - name: Cache MuJoCo
        uses: actions/cache@v3
        with:
          path: |
            ~/.local/lib/mujoco                    # Linux
            ~/Library/Frameworks/mujoco.framework   # macOS
            ~\AppData\Local\mujoco                 # Windows
          key: mujoco-${{ runner.os }}-3.3.7

      # ✅ 设置 MuJoCo 环境（复用 justfile 逻辑）
      - name: Setup MuJoCo Environment
        shell: bash
        run: |
          # 执行 just 的下载逻辑，并将输出写入 $GITHUB_ENV
          # _mujoco_download 会输出: export MUJOCO_DYNAMIC_LINK_DIR=...
          just _mujoco_download >> $GITHUB_ENV

      - name: Setup vcan0 (Linux only)
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

      # ✅ 运行测试（环境变量已通过 $GITHUB_ENV 传递）
      - name: Run unit tests
        run: just test
```

---

## 🔍 关键技术细节

### 1. $GITHUB_ENV 工作原理

```yaml
- name: Setup
  shell: bash
  run: |
    echo "VAR=value" >> $GITHUB_ENV  # 写入环境文件

- name: Test
  run: |
    echo $VAR  # ✅ 可以读取到
```

**关键点**:
- `$GITHUB_ENV` 是一个文件，不是环境变量
- 每行 `VAR=value` 会被后续 step 读取为环境变量
- 适用于所有 shell（bash、pwsh、powershell）

---

### 2. 路径一致性（关键）

**justfile 中的实际路径**:
```bash
case "$(uname -s)" in
    Linux*)
        install_dir="$HOME/.local/lib/mujoco"        # ← CI 缓存这个
        version_dir="$install_dir/mujoco-${version}"
        ;;
    Darwin*)
        install_dir="$HOME/Library/Frameworks"       # ← CI 缓存这个
        framework_path="$install_dir/mujoco.framework"
        ;;
    MINGW*|MSYS*|CYGWIN*|Windows_NT*)
        install_dir="$LOCALAPPDATA/mujoco"           # ← CI 缓存这个
        ;;
esac
```

**CI 缓存配置必须匹配**:
```yaml
path: |
  ~/.local/lib/mujoco                    # ✅ Linux
  ~/Library/Frameworks/mujoco.framework   # ✅ macOS
  ~\AppData\Local\mujoco                 # ✅ Windows
```

---

### 3. Windows 路径注意事项

**justfile**:
```bash
install_dir="$LOCALAPPDATA/mujoco"
```

**GitHub Actions (Windows)**:
```yaml
path: ~\AppData\Local\mujoco  # ✅ 正确
```

**注意**: Windows 上的 `~` 会被 GitHub Actions 展开为 `%USERPROFILE%`

---

## 📊 修正前后对比

### 违反 DRY 原则

| 场景 | 原方案 | 修正方案 |
|------|--------|---------|
| **维护成本** | 两份逻辑（justfile + CI） | 一份逻辑（justfile） |
| **一致性** | 本地和 CI 可能不同 | 完全一致 |
| **升级成本** | 需要改两处 | 只需改 justfile |

### 环境变量传递

| 场景 | 原方案 | 修正方案 |
|------|--------|---------|
| **step 1** | `export VAR=...` | `echo VAR=... >> $GITHUB_ENV` |
| **step 2** | ❌ 读取不到 | ✅ 可以读取 |

### 缓存路径

| 平台 | 原方案（错误） | 修正方案（正确） |
|------|--------------|----------------|
| **Linux** | `~/.cache/mujoco-rs/` | `~/.local/lib/mujoco/` |
| **macOS** | `~/Library/Caches/mujoco-rs/` | `~/Library/Frameworks/mujoco.framework/` |
| **Windows** | `%LOCALAPPDATA%/mujoco-rs/` | `%LOCALAPPDATA%/mujoco/` |

---

## 🎯 Husky 配置建议

### 问题

**当前做法**:
```bash
# 直接编辑 .git/hooks/pre-commit
# ⚠️ 可能被 cargo-husky 覆盖
```

**风险**:
- `cargo build` 可能会检查并覆盖 hooks
- 不在版本控制中，团队成员无法共享

### 改进方案

**方案 A: 纳入版本控制（推荐）**

1. 创建 `scripts/pre-commit`:
```bash
#!/bin/sh
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
```

2. 在 `justfile` 中添加 setup recipe:
```just
# 设置开发环境
setup:
    #!/usr/bin/env bash
    # 安装 pre-commit hook
    cp scripts/pre-commit .git/hooks/pre-commit
    chmod +x .git/hooks/pre-commit
    echo "✓ Git hooks installed"
```

3. 团队成员运行:
```bash
just setup
```

**方案 B: 使用 git-hooks crate**

```toml
# Cargo.toml
[dependencies]
git-hooks = "0.1"

[package.metadata.hooks]
pre-commit = ["cargo fmt --all -- --check", "cargo clippy --all-targets --all-features -- -D warnings"]
```

---

## ✅ 实施步骤

### 步骤 1: 更新 pre-commit hook（立即）

**手动更新**:
```bash
# 编辑 .git/hooks/pre-commit
# 移除 cargo test 部分
```

**或纳入版本控制**:
```bash
# 创建 scripts/pre-commit
git add scripts/pre-commit
git add justfile  # 包含 setup recipe
```

---

### 步骤 2: 更新 GitHub Actions（立即）

**关键改动**:
1. ✅ 安装 just
2. ✅ 添加 MuJoCo 缓存（正确路径）
3. ✅ 使用 `just _mujoco_download >> $GITHUB_ENV`
4. ✅ 运行 `just test`

---

### 步骤 3: 验证 CI（推送后）

检查:
- ✅ Linux 测试通过
- ✅ macOS 测试通过
- ✅ Windows 测试通过
- ✅ MuJoCo 缓存命中（第二次运行更快）

---

## 📚 总结

### 修正的三个严重问题

| 问题 | 原方案 | 修正方案 |
|------|--------|---------|
| **DRY 原则** | ❌ 重复实现下载逻辑 | ✅ 复用 justfile |
| **环境变量** | ❌ 不共享 | ✅ $GITHUB_ENV |
| **缓存路径** | ❌ 不一致 | ✅ 严格匹配 |

### 预期效果

- ✅ **维护成本**: 单一逻辑源
- ✅ **一致性**: 本地和 CI 完全相同
- ✅ **缓存有效**: 路径匹配，缓存命中
- ✅ **CI 可靠**: 环境变量正确传递

---

**下一步**: 是否立即执行修正后的更新？
