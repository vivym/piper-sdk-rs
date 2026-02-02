# Piper SDK - MuJoCo 快速开始

## ⚡ 快速开始（3 步）

```bash
# 1. 安装 just 命令运行器
cargo install just

# 2. 构建项目（首次会自动下载 MuJoCo）
just build

# 3. 运行测试
just test
```

## 构建

本项目使用 MuJoCo 物理引擎进行重力补偿计算。首次构建时会自动下载 MuJoCo 库到系统缓存目录。

### 推荐方式（使用 `just` 命令运行器）

```bash
# 安装 just（如果还没有）
cargo install just

# 查看所有可用命令
just

# 构建整个项目
just build

# 运行所有测试
just test

# 运行特定包的测试
just test-pkg piper-physics

# 发布构建
just release

# 查看 MuJoCo 安装信息
just mujoco-info

# 清理 MuJoCo 安装
just mujoco-clean
```

### 高级用法（手动设置环境变量）

如果您想使用已安装的 MuJoCo，可以手动设置环境变量：

```bash
# Linux
export MUJOCO_DYNAMIC_LINK_DIR="$HOME/.local/lib/mujoco/mujoco-3.3.7/lib"
cargo build

# macOS (手动安装的 MuJoCo)
export MUJOCO_DYNAMIC_LINK_DIR="$HOME/Library/Frameworks/mujoco.framework/Versions/A"
cargo build

# Windows
set MUJOCO_DYNAMIC_LINK_DIR=%LOCALAPPDATA%\mujoco\mujoco-3.3.7\lib
cargo build
```

**注意**：推荐使用 `just`，它会自动处理 MuJoCo 的下载和配置。

## MuJoCo 自动下载

- ✅ **Linux**: 自动下载 tar.gz 并解压到 `~/.local/lib/mujoco/`
- ✅ **macOS**: 自动下载 DMG 并安装到 `~/Library/Frameworks/`
- ✅ **Windows**: 自动下载 zip 并解压到 `%LOCALAPPDATA%\mujoco\`

**所有平台零配置，首次构建时自动下载。**

## MuJoCo 安装位置

### Linux
```
~/.local/lib/mujoco/
└── mujoco-3.3.7/
    ├── include/        # C 头文件
    ├── lib/            # 预编译库
    └── bin/            # 工具和插件
```

### macOS
```
~/Library/Frameworks/
└── mujoco.framework/
    └── Versions/
        └── A/          # 当前版本
            ├── include/
            └── lib/
```

### Windows
```
%LOCALAPPDATA%\mujoco\
└── mujoco-3.3.7\
    ├── include/
    ├── lib/
    └── bin/
```

## 详细文档

### MuJoCo 集成
- `docs/v0/mujoco_unified_build_architecture_analysis.md` - 统一架构分析
- `docs/v0/mujoco_v2.1_manual_download_report.md` - v2.1 手动下载实施报告
- `docs/v0/mujoco_implementation_final_report.md` - MuJoCo 集成技术细节
- `docs/v0/build_rs_vs_wrapper_script_analysis.md` - build.rs 架构分析
- `docs/v0/build_rs_warning_semantic_fix.md` - build.rs 警告语义修复

### 代码质量与 CI/CD
- `docs/v0/final_clippy_solution_summary.md` - Clippy 分层检查方案总结
- `docs/v0/all_features_enhanced_solution.md` - Clippy 完整技术文档（v2.0）
- `docs/v0/husky_and_github_actions_final_summary.md` - Husky 和 GitHub Actions 配置
- `docs/v0/pre-commit_hook_just_refactor.md` - Pre-commit Hook 优化

### 测试与调试
- `docs/v0/test_env_var_race_condition_fix.md` - 测试环境变量竞态条件修复

### 开发指南
- `CLAUDE.md` - Claude Code 开发指南（项目架构、概念、命令）

## 开发命令速览

使用 `just` 命令运行器（推荐）：

```bash
just                    # 列出所有命令

# === 构建与测试 ===
just build              # 构建整个项目
just test               # 运行所有测试
just test-pkg piper-physics  # 运行特定包的测试
just check              # 快速检查（编译但不运行）
just release            # 发布构建
just clean              # 清理构建产物

# === 代码质量检查 ===
just fmt                # 格式化代码
just fmt-check          # 验证格式（不修改文件）
just clippy             # 日常开发检查（default + realtime）
just clippy-all         # 完整功能检查（+serde +statistics）
just clippy-mock        # Mock 模式检查（无硬件环境）

# === MuJoCo 管理 ===
just mujoco-info        # MuJoCo 缓存信息
just mujoco-clean       # 清理 MuJoCo 缓存
just mujoco-shell       # 进入带 MuJoCo 环境的 shell
```

## Pre-commit Hook

项目使用 cargo-husky 管理 pre-commit hook，每次提交时自动运行：

```bash
# 1. 格式检查（just fmt-check）
# 2. Clippy 检查（just clippy-all，包含所有 features）
```

**手动运行检查**（与 pre-commit 一致）：

```bash
just fmt-check && just clippy-all
```

## Clippy 检查策略

项目采用**分层检查策略**，平衡开发速度和代码质量：

| 命令 | Features | 执行时间 | 适用场景 |
|------|----------|---------|----------|
| `just clippy` | default + realtime | ~0.2s | 日常开发 |
| `just clippy-all` | default + realtime + serde + statistics | ~0.2s | **Pre-commit、PR 检查** |
| `just clippy-mock` | mock（排他） | ~0.07s | Mock 模式开发 |

**推荐工作流**：
1. 日常开发：`just clippy`（快速反馈）
2. 提交前：pre-commit 自动运行 `just clippy-all`
3. PR 合并：CI 运行所有 clippy 检查

## 常见问题

### Q: 为什么需要 MuJoCo？

**A**: MuJoCo 是一个高性能物理引擎，用于机器人的重力补偿计算。本项目使用 MuJoCo 3.3.7 版本。

### Q: MuJoCo 会占用多少空间？

**A**:
- Linux/macOS: ~100 MB
- Windows: ~100 MB

### Q: 可以使用已安装的 MuJoCo 吗？

**A**: 可以！设置环境变量 `MUJOCO_DYNAMIC_LINK_DIR` 指向 MuJoCo 的 lib 目录。

### Q: 如何在没有硬件的环境下开发？

**A**: 使用 `just clippy-mock` 进行代码检查，避免依赖硬件后端。

### Q: Pre-commit hook 太慢怎么办？

**A**: Pre-commit 使用 `just clippy-all`（~0.2s），已经很快。如果仍觉得慢，可以修改 `.cargo-husky/hooks/pre-commit` 使用 `just clippy`。

### Q: 如何更新 MuJoCo 版本？

**A**: 修改 `Cargo.lock` 中的 mujoco-rs 版本，`just _mujoco_parse_version` 会自动解析新版本。

### Q: 测试失败了怎么办？

**A**:
1. 确保运行 `just test`（包含 MuJoCo 设置）
2. 单独运行失败的测试：`cargo test test_name`
3. 查看测试文档：`docs/v0/test_env_var_race_condition_fix.md`

## 获取帮助

```bash
# 查看所有 just 命令
just

# 查看 just 命令的帮助
just --help

# 查看项目文档
cat CLAUDE.md

# 查看 MuJoCo 安装信息
just mujoco-info
```

## 相关资源

- **GitHub**: https://github.com/vivym/piper-sdk-rs
- **文档**: `docs/v0/` 目录下的详细技术文档
- **开发指南**: `CLAUDE.md` - Claude Code AI 助手开发指南
