# docs/v0 归档说明

`docs/v0/` 保存的是 **历史设计文档、分析报告、实施计划和阶段性总结**。

这些文档仍然有参考价值，但它们**不是当前实现的规范来源**。随着项目在 2026 年 3 月之后继续演进，目录中的不少内容已经和现状存在偏差，例如：

- `PiperBuilder::build()` 之后直接得到可控制的 `Standby`
- 通过 `context.hooks` 访问回调系统
- 从 workspace 根目录直接运行 `cargo run --example ...`
- recording/replay 的早期生命周期语义
- 部分旧的队列容量、CLI target grammar、实现状态说明

如果你需要当前可执行、可构建、可验证的入口，请优先参考：

- 顶层 [README.md](../../README.md)
- [crates/piper-sdk/examples/README.md](../../crates/piper-sdk/examples/README.md)
- 当前源码和测试

HIL 相关文档是本目录中的少数例外。下面这些文件目前仍然是当前手工硬件验收流程的有效入口：

- [piper_hil_handbook.md](./piper_hil_handbook.md)
- [piper_hil_execution_checklist.md](./piper_hil_execution_checklist.md)
- [piper_hil_results_template.md](./piper_hil_results_template.md)
- [piper_hil_operator_runbook.md](./piper_hil_operator_runbook.md)

使用顺序建议是：

1. 先看 [piper_hil_handbook.md](./piper_hil_handbook.md) 了解规范性判据
2. 执行时配合 [piper_hil_execution_checklist.md](./piper_hil_execution_checklist.md) 勾选
3. 把证据写入 [piper_hil_results_template.md](./piper_hil_results_template.md)
4. 现场逐步执行时参考 [piper_hil_operator_runbook.md](./piper_hil_operator_runbook.md)

阅读 `docs/v0/` 时，建议把它视为：

1. 历史决策记录
2. 设计演进背景
3. 旧实现阶段的分析材料

而不要直接把其中的代码片段、状态说明或运行命令当成当前 API 契约。
