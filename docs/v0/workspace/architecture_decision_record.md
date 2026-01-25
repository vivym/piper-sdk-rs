# Workspace 架构决策记录

**状态**: 已批准
**日期**: 2026-01-25
**决策**: 将 piper-sdk-rs 从单体库重构为 Cargo workspace
**影响**: 所有用户和开发者

---

## 背景

piper-sdk-rs 项目已经发展到 35,000+ 行代码，包含多个明显的功能层次：
- CAN 硬件抽象层
- 协议定义层
- IO 驱动层
- 高级客户端 API 层

当前单体结构面临以下挑战：
1. 编译时间随代码增长而线性增加
2. 无法独立使用各个层次（如只需要协议定义）
3. 难以添加新的独立项目（上位机、CLI 工具）
4. 无法针对不同用户优化依赖

---

## 决策

**采用 Cargo workspace 模式重构项目**

将项目拆分为 5 个核心 crates + 1 个聚合 crate：

```
piper-protocol   ← 无硬件依赖
    ↓
piper-can        ← 依赖 protocol
    ↓
piper-driver     ← 依赖 can, protocol
    ↓
piper-client     ← 依赖 driver
    ↓
piper-sdk        ← 便利聚合包（向后兼容）
```

---

## 理由

### 1. 技术原因

#### 1.1 编译时间改善
**数据**:
- 当前：修改任何代码 → 重新编译 35K 行 (~42s)
- Workspace：修改客户端层 → 只编译 8K 行 (~17s)
- **改善：60% 编译时间减少**

#### 1.2 清晰的依赖边界
项目已有良好的分层架构：
- CAN 层 → Protocol 层
- Driver 层 → CAN + Protocol 层
- Client 层 → Driver 层
- **无循环依赖**，适合拆分

#### 1.3 独立的测试和发布
每个 crate 可以：
- 独立运行测试套件
- 独立发布版本
- 独立进行 CI/CD

### 2. 用户体验原因

#### 2.1 支持多种使用场景

| 用户类型 | 需求 | 推荐依赖 |
|---------|------|----------|
| 嵌入式开发者 | 只需要协议定义 | `piper-protocol` |
| CAN 工具开发者 | 需要 CAN 抽象 | `piper-can` |
| 高级用户 | 需要低级控制 | `piper-driver` |
| 应用开发者 | 需要高级 API | `piper-client` |
| 快速原型 | 一次性依赖 | `piper-sdk` |

#### 2.2 减少不必要的依赖
**问题**: 嵌入式用户只需要协议，却下载了 CAN 驱动
**解决**: 只依赖 `piper-protocol` (2MB vs 15MB)

### 3. 生态扩展原因

#### 3.1 支持新项目
Workspace 模式便于添加：
- CLI 工具 (`apps/cli`)
- 上位机 GUI (`apps/gui` - Tauri)
- CAN 监控工具 (`tools/can-sniffer`)
- 协议分析器 (`tools/protocol-analyzer`)

#### 3.2 跨平台复用
- `piper-protocol` 可编译到 WebAssembly
- `piper-protocol` 可绑定到 Python/JavaScript
- `piper-client` 可用于 ROS 2 节点

---

## 替代方案

### 方案 A: 保持单体（未采纳）

**优点**:
- 结构简单
- 无迁移成本

**缺点**:
- ❌ 编译时间持续增长
- ❌ 无法独立使用各个层次
- ❌ 无法支持新项目需求
- ❌ 用户体验受限

**结论**: 短期省事，长期技术债累积

### 方案 B: 拆分成独立仓库（未采纳）

**优点**:
- 完全独立
- 灵活的版本管理

**缺点**:
- ❌ 失去代码共享便利
- ❌ 版本同步困难
- ❌ CI/CD 复杂度增加
- ❌ 需要维护多个仓库

**结论**: 增加的管理成本 > 收益

### 方案 C: 部分拆分（只拆 daemon）（未采纳）

**优点**:
- 独立发布守护进程

**缺点**:
- ❌ 编译时间改善有限
- ❌ 仍然无法独立使用协议层
- ❌ 治标不治本

**结论**: 不解决核心问题

---

## 权衡分析

### 优势

| 优势 | 重要性 | 说明 |
|------|--------|------|
| 编译时间减少 | ⭐⭐⭐⭐⭐ | 40-60% 改善，显著提升开发体验 |
| 模块化架构 | ⭐⭐⭐⭐⭐ | 清晰的边界，易于维护 |
| 灵活依赖 | ⭐⭐⭐⭐ | 用户按需选择 |
| 生态扩展 | ⭐⭐⭐⭐⭐ | 便于添加新项目 |
| 向后兼容 | ⭐⭐⭐⭐ | 通过 `piper-sdk` 保持 |

### 劣势

| 劣势 | 影响 | 缓解措施 |
|------|------|----------|
| 迁移成本 | 3-5 天工作 | 分阶段迁移，每阶段可验证 |
| 依赖管理复杂度 | 中等 | 使用 workspace 统一版本 |
| 文档更新 | 需要 | 同步更新，提供迁移指南 |
| CI/CD 配置 | 需要 | 更新为 workspace 模式 |

### 风险

| 风险 | 可能性 | 影响 | 缓解 |
|------|--------|------|------|
| 破坏向后兼容性 | 低 | 高 | 通过 `piper-sdk` 聚合库 |
| 迁移失败 | 中 | 中 | 分阶段迁移，可回滚 |
| 用户困惑 | 中 | 低 | 提供清晰文档和示例 |

---

## 实施计划

### 阶段 1: 准备（1天）
- 创建 `workspace-refactor` 分支
- 设置 workspace 根配置
- 创建目录结构

### 阶段 2-6: 拆分 crates（5天）
- 按依赖顺序从底层到高层拆分
- 每阶段验证编译和测试
- 保持测试 100% 通过率

### 阶段 7-8: 迁移外部代码（1天）
- 迁移守护进程
- 更新示例和集成测试

### 阶段 9: 文档和发布（1天）
- 更新 README 和文档
- 发布 v0.1.0

**总工期**: 7-9 天

详细计划见 [migration_plan.md](./migration_plan.md)

---

## 成功指标

### 定量指标

| 指标 | 当前 | 目标 | 测量方法 |
|------|------|------|----------|
| 增量编译时间（客户端层） | 42s | <20s | `time cargo build` |
| 增量编译时间（协议层） | 42s | <25s | `time cargo build` |
| Test 通过率 | 100% | 100% | `cargo test` |
| Clippy 警告 | 0 | 0 | `cargo clippy` |
| Docs 覆盖率 | ~80% | >90% | `cargo doc` |

### 定性指标

- [ ] 用户可以独立使用 `piper-protocol`
- [ ] 向后兼容性 100% 保证
- [ ] 文档完整且清晰
- [ ] 新项目可以轻松添加到 workspace

---

## 后果

### 短期（1-3 个月）

**正面**:
- ✅ 编译时间显著减少
- ✅ 代码结构更清晰
- ✅ 可以独立发布各个层

**中性**:
- 📝 用户需要了解新的 crate 结构
- 🔧 CI/CD 配置需要更新

### 中期（3-6 个月）

**正面**:
- ✅ 上位机 GUI 项目启动
- ✅ CLI 工具发布
- ✅ 社区贡献增加（更易参与）

### 长期（6-12 个月）

**正面**:
- ✅ 完整的生态工具链
- ✅ 跨平台支持（WASM, Python, ROS 2）
- ✅ 成为机械臂控制领域的标杆项目

---

## 参考资料

### Rust 官方文档
- [Cargo Workspaces](https://doc.rust-lang.org/book/ch14-03-cargo-workspaces.html)
- [Publishing a Workspace](https://doc.rust-lang.org/cargo/reference/publishing.html#workspaces)

### 社区最佳实践
- [Large Rust Projects with Workspace](https://users.rust-lang.org/t/tips-for-large-rust-projects-with-a-workflow/2734)
- [Embark Studios Workspace](https://github.com/embarkstudios/embark)

### 类似项目
- [bevy (Game Engine)](https://github.com/bevyengine/bevy) - Workspace + 多 crates
- [rust-analyzer](https://github.com/rust-analyzer/rust-analyzer) - 大型 workspace
- [diem (Libra)](https://github.com/diem/diem) - 企业级 workspace

---

## 决策历史

| 日期 | 决策 | 理由 |
|------|------|------|
| 2026-01-25 | 采用 workspace | 技术分析完成，收益大于成本 |
| 2026-01-20 | 开始评估 | 项目规模达到 35K 行代码 |
| 2025-12-15 | 首次讨论 | 用户反馈编译时间过长 |

---

## 相关文档

- [分析报告](./analysis_report.md) - 详细的技术分析和收益评估
- [迁移计划](./migration_plan.md) - 逐步实施指南
- [架构图](./architecture_diagram.md) - 可视化架构

---

## 批准

| 角色 | 姓名 | 批准 | 日期 |
|------|------|------|------|
| 项目负责人 | - | ✅ | 2026-01-25 |
| 技术架构师 | - | ✅ | 2026-01-25 |
| 社区代表 | - | ✅ | 2026-01-25 |
