# 示例代码

本目录包含 Piper SDK 的使用示例。

## 📋 可用示例

示例代码正在开发中。计划包含以下示例：

- `read_state.rs` - 简单的状态读取和打印
- `torque_control.rs` - 力控演示
- `configure_can.rs` - CAN 波特率配置工具

## 🚀 运行示例

一旦示例可用，你可以使用以下命令运行：

```bash
# 运行特定示例
cargo run --example read_state

# 运行力控示例
cargo run --example torque_control

# 运行 CAN 配置工具
cargo run --example configure_can
```

## ⚠️ 注意事项

- 部分示例需要连接硬件设备
- 请确保具有适当的系统权限（USB 设备访问权限等）
- 详细说明请参考各示例文件中的注释

---

**注意**：示例代码正在积极开发中。如有问题或建议，欢迎提交 Issue 或 PR。
