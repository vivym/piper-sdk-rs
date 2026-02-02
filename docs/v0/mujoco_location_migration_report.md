# MuJoCo 存储位置迁移完成报告

**日期**: 2025-02-02
**状态**: ✅ 完成

---

## 变更总结

### 从项目根目录迁移到系统缓存目录

```diff
- /home/viv/projs/piper-sdk-rs/.mujoco/mujoco-3.3.7/
+ ~/.cache/mujoco-rs/mujoco-3.3.7/
```

---

## 理由

### 1. 符合 Unix/Linux 规范
- 遵循 XDG Base Directory specification
- 与其他工具保持一致（cargo, npm, pip）
- 缓存文件应该放在 `~/.cache/` 而非项目目录

### 2. 跨项目共享
- MuJoCo 是预编译的二进制库（~100MB）
- 可以在多个项目间共享
- 避免重复下载，节省磁盘空间

### 3. Git 仓库清洁
- 不会被误提交到版本控制
- 保持项目目录整洁
- 符合开源项目最佳实践

---

## 实现细节

### 平台特定路径

| 平台 | 缓存目录 |
|------|----------|
| **Linux** | `~/.cache/mujoco-rs/` (或 `$XDG_CACHE_HOME/mujoco-rs/`) |
| **macOS** | `~/Library/Caches/mujoco-rs/` |
| **Windows** | `%LOCALAPPDATA%\mujoco-rs\` |

### 智能环境变量选择

```bash
if [ MuJoCo already cached ]; then
    # 使用 MUJOCO_DYNAMIC_LINK_DIR（直接使用已有库）
    export MUJOCO_DYNAMIC_LINK_DIR="$CACHE_DIR/mujoco-3.3.7/lib"
else
    # 使用 MUJOCO_DOWNLOAD_DIR（触发 mujoco-rs 下载）
    export MUJOCO_DOWNLOAD_DIR="$CACHE_DIR"
fi
```

**优势**：
- 避免重复下载
- 自动检测已缓存的库
- 支持首次自动下载

---

## 修改的文件

### 1. `build_with_mujoco.sh`
- ✅ 添加平台特定路径检测（Linux/macOS/Windows）
- ✅ 智能选择 `MUJOCO_DYNAMIC_LINK_DIR` 或 `MUJOCO_DOWNLOAD_DIR`
- ✅ 自动添加到 `LD_LIBRARY_PATH` / `DYLD_LIBRARY_PATH`

### 2. `crates/piper-physics/build.rs`
- ✅ 移除强制设置 `MUJOCO_DOWNLOAD_DIR` 的代码
- ✅ 支持外部环境变量（wrapper script 或用户手动设置）
- ✅ 添加 `MUJOCO_DOWNLOAD_DIR` 到 `rerun-if-env-changed`

### 3. `crates/piper-sdk/Cargo.toml`
- ✅ 添加缺失的 dev-dependencies：`nix`, `libc`, `socketcan`, `rusb`

### 4. `crates/piper-can/src/socketcan/mod.rs`
- ✅ 修复 doctest 导入路径（`piper_sdk` → `piper_can`）

---

## 测试验证

### 单元测试
```bash
$ ./build_with_mujoco.sh test -p piper-physics --lib

running 12 tests
test mujoco::tests::test_column_major_indexing_is_wrong ... ok
...
test result: ok. 12 passed; 0 failed; 0 ignored
```

### 缓存验证
```bash
$ ls -lh ~/.cache/mujoco-rs/
drwxr-xr-x 1 viv viv 102 Feb  2 14:16 mujoco-3.3.7

$ ./build_with_mujoco.sh build
=== MuJoCo Build Configuration ===
Cache directory: /home/viv/.cache/mujoco-rs
Using cached MuJoCo: /home/viv/.cache/mujoco-rs/mujoco-3.3.7/lib
==================================
```

---

## 用户体验

### 开发者（多项目）
```bash
# 项目 A
cd ~/projs/project-a
./build_with_mujoco.sh build
# 首次下载到 ~/.cache/mujoco-rs/mujoco-3.3.7/

# 项目 B（使用同一缓存）
cd ~/projs/project-b
./build_with_mujoco.sh build
# 直接使用缓存，无需重新下载 ✅
```

### CI/CD
```yaml
# GitHub Actions
- name: Cache MuJoCo
  uses: actions/cache@v3
  with:
    path: ~/.cache/mujoco-rs
    key: mujoco-3.3.7

- name: Build
  run: ./build_with_mujoco.sh build
```

### 清理缓存
```bash
# 清理 MuJoCo 缓存
rm -rf ~/.cache/mujoco-rs/

# 清理所有 XDG 缓存（谨慎使用）
rm -rf ~/.cache/*
```

---

## 向后兼容性

### 用户直接使用 cargo（不推荐）
```bash
# 仍然可以手动设置环境变量
export MUJOCO_DOWNLOAD_DIR="$HOME/.cache/mujoco-rs"
export LD_LIBRARY_PATH="$HOME/.cache/mujoco-rs/mujoco-3.3.7/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
cargo build
```

### 项目根目录 .mujoco（已废弃）
- `.gitignore` 中已保留 `.mujoco` 条目
- 支持旧版本兼容性
- 建议使用 wrapper script

---

## 优势总结

| 方面 | 项目目录 (`.mujoco`) | 系统缓存 (`~/.cache/`) |
|------|---------------------|------------------------|
| **空间效率** | ❌ 每个项目 100MB | ✅ 跨项目共享 |
| **Git 清洁** | ⚠️ 需要 .gitignore | ✅ 不会被提交 |
| **符合规范** | ❌ 不符合 XDG | ✅ 遵循 XDG 规范 |
| **用户体验** | ⚠️ 隐藏目录 | ✅ 明确的缓存位置 |
| **清理管理** | ⚠️ 需手动清理每个项目 | ✅ 统一清理 |
| **CI/CD** | ⚠️ 需配置每个项目 | ✅ 统一缓存配置 |

---

## 迁移指南

### 已有用户

如果您已经在项目根目录下载了 MuJoCo：

```bash
# 1. 移动到新位置
mv /path/to/project/.mujoco/mujoco-3.3.7 ~/.cache/mujoco-rs/

# 2. 清理旧目录
rm -rf /path/to/project/.mujoco/

# 3. 使用新的 wrapper script
./build_with_mujoco.sh build
```

---

## 文档

- `docs/v0/mujoco_location_analysis.md` - 详细分析报告
- `docs/v0/mujoco_implementation_final_report.md` - 实现总结
- `QUICKSTART.md` - 快速开始指南

---

**结论：迁移成功，所有测试通过，用户体验得到改善！** 🎉
