# 🚀 快速发布参考

## ✅ cargo-release 已配置

**配置位置**: `Cargo.toml` (根目录)
**配置状态**: ✅ 已添加
**最后更新**: 2026-01-26

---

## 📝 快速发布命令

### 完整发布流程

```bash
# 1. 切换到发布分支
git checkout main
git pull origin main
git checkout -b release-v0.0.3

# 2. Dry-run 模式（推荐先测试）
cargo release --workspace --no-dev --dry-run

# 3. 实际发布
cargo release --workspace --no-dev
```

### 单步发布命令

```bash
# 仅执行发布前检查
cargo release --workspace --no-dev --no-publish --no-tag

# 仅创建 tag，不发布
cargo release --workspace --no-dev --no-publish

# 仅推送，不发布
cargo release --workspace --no-dev --no-publish --no-tag
```

---

## 🎯 Workspace 发布配置

```toml
[workspace.metadata.release]
tag-name = "v{{version}}"
consolidate-commits = true
consolidate-pushes = true
pre-release-hook = ["cargo", "test", "--workspace"]
push = true
publish = true
shared-version = true
```

**配置说明**:
- ✅ 统一 tag 命名: `v0.0.3`
- ✅ 原子提交: 合并所有变更
- ✅ 原子推送: 一次推送完成
- ✅ 自动测试: 发布前运行 `cargo test --workspace`
- ✅ 共享版本: 所有 crate 使用同一版本号

---

## 📊 发布顺序（自动）

```
piper-protocol (v0.0.3)
    ↓ 等待 crates.io 索引
piper-can (v0.0.3)
    ↓ 等待 crates.io 索引
piper-driver (v0.0.3)
    ↓ 等待 crates.io 索引
piper-client (v0.0.3)
    ↓ 等待 crates.io 索引
piper-sdk (v0.0.3)
    ↓
Git Tag: v0.0.3
    ↓
推送到远程
```

**总耗时**: 约 5-10 分钟（包含 crates.io 索引等待时间）

---

## ⚠️ 发布前必查

```bash
# 1. 格式检查
cargo fmt --all

# 2. Lint 检查
cargo clippy --workspace --all-targets -- -D warnings

# 3. 单元测试
cargo test --workspace
# 预期: 543 passed

# 4. Doctest
cargo test --workspace --doc
# 预期: 56 passed

# 5. 文档检查
cargo doc --workspace --no-deps 2>&1 | grep broken
```

---

## 🔧 故障排除

### 问题: 发布工具未安装

```bash
cargo install cargo-release
```

### 问题: 配置无效

```bash
# 检查配置
cat Cargo.toml | grep -A 20 "\[workspace.metadata.release\]"
```

### 问题: Token 未配置

```bash
# 编辑配置
nano ~/.cargo/config.toml

# 添加 token
[crates-io]
token = "your_token_here"
```

---

## 📚 完整文档

详细发布指南: `docs/v0/workspace/RELEASE_GUIDE.md`

---

**配置完成时间**: 2026-01-26
**配置版本**: v0.0.3
**状态**: ✅ 生产就绪
