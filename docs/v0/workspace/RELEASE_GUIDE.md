# Piper SDK 发布指南

**版本**: v0.0.3
**最后更新**: 2026-01-26
**状态**: ✅ cargo-release 已配置

---

## 📋 前置准备

### 1. 安装 cargo-release

```bash
cargo install cargo-release
```

验证安装:
```bash
cargo release --version
```

### 2. 配置 Git 远程仓库

确保你已经配置了正确的远程仓库:
```bash
git remote -v
# 应该看到:
# origin    https://github.com/vivym/piper-sdk-rs (fetch)
# origin    https://github.com/vivym/piper-sdk-rs (push)
```

### 3. 配置 crates.io Token

首次发布需要在 `~/.cargo/config.toml` 中添加 API token:
```bash
mkdir -p ~/.cargo
cat >> ~/.cargo/config.toml << 'EOF'
[registry]
default = "crates-io"

[crates-io]
token = "your_api_token_here"  # 从 https://crates.io/me 获取
EOF
```

---

## 🚀 发布流程

### 方式 1: 自动发布（推荐）⭐

使用 cargo-release 工具一键发布整个 workspace:

```bash
# 1. 确保在 main 分支
git checkout main
git pull origin main

# 2. 创建发布分支（推荐）
git checkout -b release-v0.0.3

# 3. 更新版本号（如果需要）
# 编辑 Cargo.toml 中的 [workspace.package].version

# 4. 执行发布（dry-run 模式，不实际发布）
cargo release --workspace --no-dev --dry-run

# 5. 如果 dry-run 成功，执行实际发布
cargo release --workspace --no-dev
```

**这个命令会自动**:
- ✅ 运行所有测试（`pre-release-hook`）
- ✅ 按依赖顺序发布所有 crates（protocol → can → driver → client → sdk）
- ✅ 等待 crates.io 索引更新
- ✅ 创建统一的 `v0.0.3` tag
- ✅ 推送 tag 到远程
- ✅ 合并所有提交和推送操作

---

### 方式 2: 手动发布（备选）

如果 cargo-release 工具有问题，可以手动按顺序发布:

```bash
# 1. 发布 piper-protocol
cd crates/piper-protocol
cargo publish
cd ../..

# 等待 1-2 分钟让 crates.io 索引更新
sleep 90

# 2. 发布 piper-can
cd crates/piper-can
cargo publish
cd ../..

# 等待 1-2 分钟
sleep 90

# 3. 发布 piper-driver
cd crates/piper-driver
cargo publish
cd ../..

# 等待 1-2 分钟
sleep 90

# 4. 发布 piper-client
cd crates/piper-client
cargo publish
cd ../..

# 等待 1-2 分钟
sleep 90

# 5. 最后发布 piper-sdk
cd crates/piper-sdk
cargo publish
cd ../..

# 6. 创建并推送 tag
git tag v0.0.3
git push origin v0.0.3
```

---

## 🔍 发布前检查清单

### 代码质量

- [ ] `cargo fmt --all` 无格式差异
  ```bash
  cargo fmt --all
  git diff
  ```

- [ ] `cargo clippy --workspace` 无警告
  ```bash
  cargo clippy --workspace --all-targets -- -D warnings
  ```

- [ ] `cargo test --workspace` 所有测试通过
  ```bash
  cargo test --workspace
  # 预期: 543/543 单元测试通过
  ```

- [ ] `cargo test --workspace --doc` 所有 doctest 通过
  ```bash
  cargo test --workspace --doc
  # 预期: 56/56 doctest 通过
  ```

### 文档检查

- [ ] `cargo doc --workspace` 无 broken links
  ```bash
  cargo doc --workspace --no-deps 2>&1 | grep broken
  ```

- [ ] CHANGELOG.md 已更新
  ```bash
  # 添加 v0.0.3 的变更记录
  ```

### 版本检查

- [ ] 所有 crate 的版本号一致
  ```bash
  grep -r "version.workspace = true" crates/*/Cargo.toml
  # 所有 crate 都应该使用 workspace 版本
  ```

- [ ] `[workspace.package].version` 已更新
  ```toml
  [workspace.package]
  version = "0.0.3"  # ← 检查这个
  ```

---

## 📊 Workspace 发布配置

当前配置（在根目录 `Cargo.toml` 中）:

```toml
[workspace.metadata.release]
# 统一的 tag 命名格式
tag-name = "v{{version}}"

# 原子操作
consolidate-commits = true
consolidate-pushes = true

# 发布前测试
pre-release-hook = ["cargo", "test", "--workspace"]

# 自动推送
push = true

# 发布到 crates.io
publish = true

# 共享版本号
shared-version = true
```

**配置说明**:
- `tag-name = "v{{version}}"`: 创建 `v0.0.3` 而不是 `piper-protocol-v0.0.3`
- `consolidate-commits = true`: 所有 crate 的变更合并为一个提交
- `consolidate-pushes = true`: 所有推送合并为一次
- `pre-release-hook`: 发布前运行完整测试套件
- `shared-version = true`: 所有 crate 共享同一个版本号

---

## 🎯 发布 Crate 顺序

由于依赖关系，**必须按以下顺序发布**:

```
1. piper-protocol    (无内部依赖)
   ↓
2. piper-can         (依赖 piper-protocol)
   ↓
3. piper-driver      (依赖 piper-can, piper-protocol)
   ↓
4. piper-client      (依赖 piper-driver, piper-can, piper-protocol)
   ↓
5. piper-sdk         (依赖所有上述 crates)
```

**注意**: `apps/daemon` 是二进制程序，不需要发布到 crates.io。

---

## ⚠️ 常见问题

### Q1: 发布时提示 "crate already exists"

**原因**: 旧版本已存在，需要增加版本号

**解决**:
```bash
# 更新 Cargo.toml 中的版本号
version = "0.0.4"  # ← 增加版本号

# 重新发布
cargo release --workspace --no-dev
```

### Q2: 发布时提示 "waiting for crate to be indexed"

**原因**: crates.io 需要时间索引新发布的 crate

**解决**: 等待 1-2 分钟后重试

### Q3: 发布失败，但 tag 已创建

**原因**: 发布过程中断，需要清理

**解决**:
```bash
# 删除本地 tag
git tag -d v0.0.3

# 删除远程 tag
git push origin :refs/tags/v0.0.3

# 重新发布
cargo release --workspace --no-dev
```

### Q4: cargo-release 工具报错

**原因**: 可能是配置问题或工具版本过旧

**解决**:
```bash
# 更新工具
cargo install cargo-release --force

# 检查配置
cat Cargo.toml | grep -A 20 "\[workspace.metadata.release\]"

# 如果仍有问题，使用手动发布方式（方式 2）
```

---

## 🔐 安全检查

### Token 权限

确认你的 crates.io token 有发布权限:
```bash
cat ~/.cargo/config.toml | grep token
```

### Git 权限

确认你有推送到远程仓库的权限:
```bash
git push origin --dry-run
```

---

## 📝 发布后验证

### 1. 验证 crates.io

访问以下链接验证发布成功:
- https://crates.io/crates/piper-protocol
- https://crates.io/crates/piper-can
- https://crates.io/crates/piper-driver
- https://crates.io/crates/piper-client
- https://crates.io/crates/piper-sdk

### 2. 验证 Git Tag

```bash
git tag | grep v0.0.3
git show v0.0.3
```

### 3. 验证远程 Tag

```bash
git ls-remote --tags origin | grep v0.0.3
```

### 4. 测试安装

在新项目中测试安装:
```bash
cargo new test_piper && cd test_piper
cargo add piper-sdk
cargo build
```

---

## 🎉 发布成功后

### 1. 更新 GitHub Release

在 GitHub 上创建 Release:
1. 访问: https://github.com/vivym/piper-sdk-rs/releases/new
2. 选择 tag: `v0.0.3`
3. 标题: `v0.0.3`
4. 内容: 复制 CHANGELOG.md 中的相关部分
5. 点击 "Publish release"

### 2. 通知用户

在合适的渠道通知用户:
- GitHub Release
- 项目 README 更新
- 社交媒体/论坛（如果适用）

### 3. 合并到主分支

如果在发布分支上工作:
```bash
git checkout main
git merge release-v0.0.3
git push origin main
```

---

## 📚 相关资源

- [cargo-release 文档](https://github.com/crate-ci/cargo-release)
- [crates.io 发布指南](https://doc.rust-lang.org/cargo/reference/publishing.html)
- [Workspace 发布最佳实践](https://doc.rust-lang.org/cargo/reference/workspaces.html#publishing-workspaces)

---

**最后更新**: 2026-01-26
**维护者**: Piper SDK Team
**配置状态**: ✅ cargo-release 已配置并可用
