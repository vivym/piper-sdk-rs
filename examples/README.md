# 示例代码

本目录包含 Piper SDK 的使用示例。

## 📋 可用示例

### 核心示例

- **`state_api_demo`** - 状态 API 使用演示
  - 一次性读取所有机器人状态
  - 展示所有 API 功能（包括配置状态）
  - 适合学习 API 用法和作为参考文档
  - 运行：`cargo run --example state_api_demo`

- **`robot_monitor`** - 机器人实时监控工具
  - 持续循环读取状态（1Hz 刷新频率）
  - 显示关节位置、速度、电流等实时数据
  - 包含中文状态转换函数，便于理解
  - 支持 Ctrl+C 优雅退出
  - 运行：`cargo run --example robot_monitor`

### 工具示例

- **`timestamp_verification`** - SocketCAN 硬件时间戳验证程序
  - 验证 Linux SocketCAN 是否支持硬件时间戳（SO_TIMESTAMPING）
  - 运行：`cargo run --example timestamp_verification`

### 计划中的示例

- `torque_control` - 力控演示
- `configure_can` - CAN 波特率配置工具

## 🚀 运行示例

```bash
# 运行状态 API 演示
cargo run --example state_api_demo

# 运行实时监控工具
cargo run --example robot_monitor

# 运行时间戳验证程序
cargo run --example timestamp_verification
```

## 📝 示例说明

### `state_api_demo` vs `robot_monitor`

- **`state_api_demo`**：适合学习 API 用法，一次性展示所有功能
- **`robot_monitor`**：适合实际监控场景，持续显示实时数据

两者功能有重叠，但定位不同：
- `state_api_demo` 更注重 API 的完整性和教学性
- `robot_monitor` 更注重实用性和实时性

## ⚠️ 注意事项

- 部分示例需要连接硬件设备
- 请确保具有适当的系统权限（USB 设备访问权限等）
- 详细说明请参考各示例文件中的注释

---

**注意**：示例代码正在积极开发中。如有问题或建议，欢迎提交 Issue 或 PR。
