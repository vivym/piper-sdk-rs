# Piper SDK Workspace 重构文档

本目录包含将 piper-sdk-rs 重构为 Cargo workspace 的完整分析和规划文档。

## 📚 文档目录

### 1. [分析报告](./analysis_report.md) ⭐ 从这里开始
**详细的技术分析和收益评估**

- 当前项目结构分析（35K+ 行代码）
- 为什么需要 workspace
- 拟议的 workspace 结构
- 扩展项目规划（上位机、CLI 工具等）
- 代码统计和依赖分析
- 预期收益（编译时间 -40~-60%）

**适合**: 需要了解全局视角的技术决策者、架构师

---

### 2. [迁移计划](./migration_plan.md) 🛠️ 实施指南
**逐步的、可操作的迁移指南**

- 9 个详细阶段（每个 0.5-1 天）
- 每个阶段的代码示例和验收标准
- 回滚计划和风险缓解
- 时间估算（总计 7-9 天）
- 常见问题解答

**适合**: 执行迁移的开发工程师、项目经理

---

### 3. [架构决策记录](./architecture_decision_record.md) 📋 决策文档
**标准化的问题分析和决策记录**

- 背景和问题陈述
- 替代方案对比
- 权衡分析（优势/劣势）
- 成功指标
- 决策历史

**适合**: 需要理解"为什么"的干系人、审查者

---

## 🚀 快速开始

### 我想了解全局情况
1. 阅读 [分析报告](./analysis_report.md) 的"执行摘要"和"建议的 Workspace 结构"
2. 查看"预期收益"了解改善数据

### 我想执行迁移
1. 阅读 [迁移计划](./migration_plan.md)
2. 从"阶段 0: 准备工作"开始
3. 按顺序完成所有阶段

### 我想审查决策
1. 阅读 [架构决策记录](./architecture_decision_record.md)
2. 查看"替代方案"和"权衡分析"

---

## 📊 关键数据

### 当前项目规模
- **代码行数**: 35,000+ 行
- **文件数**: 106 个 Rust 文件
- **测试数**: 561 个测试
- **模块数**: 4 层架构

### 预期改善
| 指标 | 改善幅度 |
|------|----------|
| 编译时间（修改客户端） | **-60%** |
| 编译时间（修改协议） | **-50%** |
| 编译时间（修改守护进程） | **-88%** |
| 依赖体积（嵌入式用户） | **-87%** |

### 建议的 Crate 结构
```
piper-protocol    (6.2K LOC)  ← 无硬件依赖
    ↓
piper-can         (4.5K LOC)  ← CAN 抽象
    ↓
piper-driver      (5.8K LOC)  ← IO 管理
    ↓
piper-client      (8.2K LOC)  ← 高级 API
    ↓
piper-sdk         (聚合库)    ← 向后兼容
```

---

## 🔮 扩展项目规划

### 短期（迁移完成后）
- ✅ `apps/cli` - 命令行工具
- ✅ `tools/can-sniffer` - CAN 总线监控

### 中期（3-6 个月）
- ✅ `apps/gui` - 上位机 GUI (Tauri)
- ✅ `tools/protocol-analyzer` - 协议分析器

### 长期（6-12 个月）
- 🔮 `bindings/python` - Python 绑定
- 🔮 `clients/ros2` - ROS 2 节点
- 🔮 `wasm/piper-protocol` - WebAssembly 版本

---

## ❓ 常见问题

### Q: 这会影响现有用户吗？
**A**: 不会。通过 `piper-sdk` 聚合库，现有代码无需修改：

```rust
// 旧代码（仍然有效）
use piper_sdk::prelude::*;
let piper = PiperBuilder::new().build()?;
```

### Q: 迁移需要多久？
**A**: 预计 7-9 天：
- 准备：1 天
- 拆分 crates：5 天
- 更新外部代码：1 天
- 文档和发布：1-2 天

### Q: 可以回滚吗？
**A**: 可以。分阶段迁移，每阶段独立可验证，随时可以回滚。

### Q: 编译时间真的会改善吗？
**A**: 是的。基于类似项目的经验：
- 协议层修改：-50% (42s → 21s)
- 客户端修改：-60% (42s → 17s)
- 守护进程修改：-88% (42s → 5s)

---

## 📖 相关资源

### 官方文档
- [Cargo Workspaces](https://doc.rust-lang.org/book/ch14-03-cargo-workspaces.html)
- [Workspace 发布](https://doc.rust-lang.org/cargo/reference/publishing.html#workspaces)

### 最佳实践
- [Large Rust Projects](https://users.rust-lang.org/t/tips-for-large-rust-projects-with-a-workflow/2734)
- [Bevy Workspace](https://github.com/bevyengine/bevy) (参考实现)

### 本项目文档
- [Pipeline 设计](../pipeline_design.md)
- [Position Control 用户指南](../position_control_user_guide.md)

---

## 🤝 贡献

如果你对 workspace 重构有建议或发现问题：

1. 查看 [分析报告](./analysis_report.md) 了解全局情况
2. 在项目中提出 Issue 或 PR
3. 联系维护团队讨论

---

## 📅 更新日志

| 日期 | 文档 | 更新内容 |
|------|------|----------|
| 2026-01-25 | 全部 | 初始版本 |
| - | analysis_report.md | 完成技术分析 |
| - | migration_plan.md | 完成迁移计划 |
| - | architecture_decision_record.md | 完成决策记录 |

---

**最后更新**: 2026-01-25
**维护者**: Piper SDK 团队
**状态**: ✅ 分析完成，待实施
