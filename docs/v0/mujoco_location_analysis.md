# MuJoCo 存储位置分析报告

**日期**: 2025-02-02
**问题**: `.mujoco` 应该放在项目根目录还是 home 目录？

---

## 结论：推荐使用 `~/.cache/mujoco-rs/`

### 理由

1. **跨项目共享**
   - MuJoCo 是预编译的二进制库（~100MB）
   - 可以在多个项目间共享
   - 避免重复下载，节省磁盘空间

2. **符合 Unix 规范**
   - 遵循 XDG Base Directory 规范
   - 与其他工具（cargo, npm, pip）保持一致
   - 缓存文件应该放在 `~/.cache/` 而非项目目录

3. **版本管理**
   - 支持版本隔离：`~/.cache/mujoco-rs/mujoco-3.3.7/`
   - 未来可支持多版本共存
   - 便于清理旧版本

4. **Git 仓库清洁**
   - 不会被误提交到版本控制
   - 不需要维护 `.gitignore` 规则
   - CI 环境自动处理

---

## 实现方案

### 修改 `build_with_mujoco.sh`

```bash
#!/bin/bash
set -e

PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# 使用 XDG cache 目录或 home 目录
if [[ "$OSTYPE" == "linux-gnu"* ]]; then
    # Linux: 遵循 XDG 规范
    CACHE_DIR="${XDG_CACHE_HOME:-$HOME/.cache}"
    MUJOCO_DIR="$CACHE_DIR/mujoco-rs"
elif [[ "$OSTYPE" == "darwin"* ]]; then
    # macOS: 使用 ~/Library/Caches
    MUJOCO_DIR="$HOME/Library/Caches/mujoco-rs"
else
    # Windows: 使用 %LOCALAPPDATA%
    MUJOCO_DIR="$LOCALAPPDATA/mujoco-rs"
fi

mkdir -p "$MUJOCO_DIR"

export MUJOCO_DOWNLOAD_DIR="$MUJOCO_DIR"

# 设置 LD_LIBRARY_PATH
if [ -d "$MUJOCO_DIR/mujoco-3.3.7/lib" ]; then
    export LD_LIBRARY_PATH="$MUJOCO_DIR/mujoco-3.3.7/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
    echo "=== MuJoCo Build Configuration ==="
    echo "Cache directory: $MUJOCO_DIR"
    echo "Library path: $MUJOCO_DIR/mujoco-3.3.7/lib"
    echo "=================================="
else
    echo "=== MuJoCo Build Configuration ==="
    echo "Cache directory: $MUJOCO_DIR"
    echo "Note: MuJoCo will be downloaded on first build"
    echo "=================================="
fi
echo ""

cargo "$@"
```

---

## 对比表

| 方案 | 优点 | 缺点 | 适用场景 |
|------|------|------|----------|
| **项目根目录** `.mujoco/` | • 简单<br>• 项目自包含 | • 重复下载<br>• 浪费空间<br>• 需 .gitignore | ❌ 不推荐 |
| **Home 目录** `~/.mujoco/` | • 跨项目共享<br>• 简单 | • 不符合规范<br>• 隐藏目录 | ⚠️ 可用 |
| **XDG Cache** `~/.cache/mujoco-rs/` | • 符合规范<br>• 语义明确<br>• 易于清理 | • 稍复杂 | ✅ **推荐** |
| **用户自定义** `MUJOCO_CACHE_DIR` | • 最灵活 | • 用户需配置 | 高级用户 |

---

## 迁移步骤

如果已经下载到项目目录，可以迁移：

```bash
# 1. 移动已下载的 MuJoCo
mv /path/to/project/.mujoco/mujoco-3.3.7 ~/.cache/mujoco-rs/

# 2. 清理项目目录
rm -rf /path/to/project/.mujoco/

# 3. 添加到 .gitignore（如果还没有）
echo ".mujoco/" >> /path/to/project/.gitignore
```

---

## 其他考虑

### CI/CD 环境

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

### 多项目开发

```
~/.cache/mujoco-rs/
├── mujoco-3.3.7/          # 版本 3.3.7
│   ├── include/
│   ├── lib/
│   └── bin/
└── mujoco-3.3.8/          # 未来版本（可能）
    └── ...
```

### 清理策略

```bash
# 清理 MuJoCo 缓存
rm -rf ~/.cache/mujoco-rs/

# 清理所有 XDG 缓存
rm -rf ~/.cache/*
```

---

## 最终建议

**使用 `~/.cache/mujoco-rs/`（或平台等价目录）**

理由：
1. 符合 Unix/Linux 规范
2. 与其他工具（cargo, npm, pip）保持一致
3. 节省磁盘空间（跨项目共享）
4. 更好的可维护性
5. 明确的"缓存"语义
