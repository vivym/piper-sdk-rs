# Piper SDK

[![Crates.io](https://img.shields.io/crates/v/piper-sdk)](https://crates.io/crates/piper-sdk)
[![Documentation](https://docs.rs/piper-sdk/badge.svg)](https://docs.rs/piper-sdk)
[![License](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

**高性能、跨平台（Linux/Windows/macOS）、零抽象开销**的 Rust SDK，专用于松灵 Piper 机械臂的高频力控（500Hz）。

[English README](README.md)

## ✨ 核心特性

- 🚀 **零抽象开销**：编译期多态，运行时无虚函数表（vtable）开销
- ⚡ **高性能读取**：基于 `ArcSwap` 的无锁状态读取，纳秒级响应
- 🔄 **无锁并发**：采用 RCU（Read-Copy-Update）机制，实现高效的状态共享
- 🎯 **类型安全**：使用 `bilge` 进行位级协议解析，编译期保证数据正确性
- 🌍 **跨平台支持（Linux/Windows/macOS）**：
  - **Linux**: 同时支持 SocketCAN（内核级性能）和 GS-USB（通过 libusb 用户态实现）
  - **Windows/macOS**: 基于 `rusb` 实现用户态 GS-USB 驱动（免驱动/通用）
- 📊 **高级健康监控**（gs_usb_daemon）：
  - **CAN Bus Off 检测**：检测 CAN Bus Off 事件（关键系统故障），带防抖机制
  - **Error Passive 监控**：监控 Error Passive 状态（Bus Off 前警告），用于早期检测
  - **USB STALL 跟踪**：跟踪 USB 端点 STALL 错误，监控 USB 通信健康状态
  - **性能基线**：使用 EWMA 进行动态 FPS 基线跟踪，用于异常检测
  - **健康评分**：基于多项指标的综合健康评分（0-100）

## 🛠️ 技术栈

| 模块 | Crates | 用途 |
|------|--------|------|
| CAN 接口 | 自定义 `CanAdapter` | 轻量级 CAN 适配器 Trait（无嵌入式负担） |
| Linux 后端 | `socketcan` | Linux 原生 CAN 支持（SocketCAN 接口） |
| USB 后端 | `rusb` | Windows/macOS 下操作 USB 设备，实现 GS-USB 协议 |
| 协议解析 | `bilge` | 位操作、非对齐数据处理，替代 serde |
| 并发模型 | `crossbeam-channel` | 高性能 MPSC 通道，用于发送控制指令 |
| 状态共享 | `arc-swap` | RCU 机制，实现无锁读取最新状态 |
| 错误处理 | `thiserror` | SDK 内部精确的错误枚举 |
| 日志 | `tracing` | 结构化日志记录 |

## 📦 安装

在 `Cargo.toml` 中添加依赖：

```toml
[dependencies]
piper-sdk = "0.0.1"
```

### 可选：实时线程优先级支持

对于高频控制场景（500Hz-1kHz），您可以启用 `realtime` 特性来将 RX 线程设置为最高优先级：

```toml
[dependencies]
piper-sdk = { version = "0.0.1", features = ["realtime"] }
```

**注意**：在 Linux 上，设置线程优先级需要适当的权限：
- 使用 `CAP_SYS_NICE` 能力运行：`sudo setcap cap_sys_nice+ep /path/to/your/binary`
- 或使用 `rtkit` (RealtimeKit) 进行用户空间优先级提升
- 或以 root 身份运行（不推荐用于生产环境）

如果权限不足，SDK 会记录警告但会继续正常运行。

详细设置说明请参阅 [实时配置指南](docs/v0/realtime_configuration.md)。

## 🚀 快速开始

### 基本使用（客户端 API - 推荐）

大多数用户应该使用高级客户端 API，提供类型安全、易于使用的控制接口：

```rust
use piper_sdk::prelude::*;

fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    // 使用 Builder API 连接（自动处理平台差异）
    let robot = PiperBuilder::new()
        .interface("can0")
        .baud_rate(1_000_000)
        .build()?;
    let robot = robot.enable_position_mode(PositionModeConfig::default())?;

    // 获取观察器用于读取状态
    let observer = robot.observer();

    // 读取状态（无锁，纳秒级返回）
    let joint_pos = observer.joint_positions();
    println!("关节位置: {:?}", joint_pos);

    // 使用类型安全的单位发送位置命令（方法直接在 robot 上调用）
    let target = JointArray::from([Rad(0.5), Rad(0.0), Rad(0.0), Rad(0.0), Rad(0.0), Rad(0.0)]);
    robot.send_position_command(&target)?;

    Ok(())
}
```

### 高级使用（驱动层 API）

需要直接控制 CAN 帧或追求最高性能时，使用驱动层 API：

```rust
use piper_sdk::driver::PiperBuilder;

fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    // 创建驱动实例
    let robot = PiperBuilder::new()
        .interface("can0")?  // Linux: SocketCAN 接口名（或 GS-USB 设备序列号）
        .baud_rate(1_000_000)?  // CAN 波特率
        .build()?;

    // 获取当前状态（无锁，纳秒级返回）
    let joint_pos = robot.get_joint_position();
    println!("关节位置: {:?}", joint_pos.joint_pos);

    // 发送控制帧
    let frame = piper_sdk::PiperFrame::new_standard(0x1A1, &[0x01, 0x02, 0x03]);
    robot.send_frame(frame)?;

    Ok(())
}
```

## 🏗️ 架构设计

### 热冷数据分离（Hot/Cold Splitting）

为了优化性能，状态数据分为两类：

- **高频数据（200Hz）**：
  - `JointPositionState`：关节位置（6 个关节）
  - `EndPoseState`：末端执行器位姿（位置和姿态）
  - `JointDynamicState`：关节动态状态（关节速度、电流）
  - `RobotControlState`：机器人控制状态（控制模式、机器人状态、故障码等）
  - `GripperState`：夹爪状态（行程、扭矩、状态码等）
  - 使用 `ArcSwap` 实现无锁读取，针对高频控制循环优化

- **低频数据（40Hz）**：
  - `JointDriverLowSpeedState`：关节驱动器诊断状态（温度、电压、电流、驱动器状态）
  - `CollisionProtectionState`：碰撞保护级别（按需）
  - `JointLimitConfigState`：关节角度和速度限制（按需）
  - `JointAccelConfigState`：关节加速度限制（按需）
  - `EndLimitConfigState`：末端执行器速度和加速度限制（按需）
  - 诊断数据使用 `ArcSwap`，配置数据使用 `RwLock`

### 架构层次

SDK 采用分层架构，从底层到高层：

- **CAN 层** (`can`): CAN 硬件抽象，支持 SocketCAN 和 GS-USB
- **协议层** (`protocol`): 类型安全的协议编码/解码
- **驱动层** (`driver`): IO 线程管理、状态同步、帧解析
- **客户端层** (`client`): 类型安全、易用的控制接口

### 核心组件

```
piper-rs/
├── src/
│   ├── lib.rs              # 库入口，Facade Pattern 导出
│   ├── prelude.rs          # 常用类型的便捷导入
│   ├── can/                # CAN 通讯适配层
│   │   ├── mod.rs          # CAN 适配器 Trait 和通用类型
│   │   └── gs_usb/         # [Win/Mac] GS-USB 协议实现
│   ├── protocol/           # 协议定义（业务无关，纯数据）
│   │   ├── ids.rs          # CAN ID 常量/枚举
│   │   ├── feedback.rs     # 机械臂反馈帧 (bilge)
│   │   ├── control.rs      # 控制指令帧 (bilge)
│   │   └── config.rs       # 配置帧 (bilge)
│   ├── driver/             # 驱动层（IO 管理、状态同步）
│   │   ├── mod.rs          # 驱动模块入口
│   │   ├── piper.rs        # 驱动层 Piper 对象 (API)
│   │   ├── pipeline.rs     # IO Loop、ArcSwap 更新逻辑
│   │   ├── state.rs        # 状态结构定义（热冷数据分离）
│   │   ├── builder.rs      # PiperBuilder（链式构造）
│   │   └── error.rs        # DriverError（错误类型）
│   └── client/             # 客户端层（类型安全、用户友好 API）
│       ├── mod.rs          # 客户端模块入口
│       ├── observer.rs      # Observer（只读状态访问）
│       ├── state/           # Type State Pattern 状态机
│       │   └── machine.rs   # Piper 状态机（命令方法）
│       ├── control/         # 控制器和轨迹规划
│       └── types/           # 类型系统（单位、关节、错误）
```

### 并发模型

采用**异步 IO 思想但用同步线程实现**（保证确定性延迟）：

1. **IO 线程**：负责 CAN 帧的收发和状态更新
2. **控制线程**：通过 `ArcSwap` 无锁读取最新状态，通过 `crossbeam-channel` 发送指令
3. **Frame Commit 机制**：确保控制线程读取的状态是一致的时间点快照

## 📚 示例

查看 `examples/` 目录了解更多示例：

> **注意**：示例代码正在开发中。更多示例请查看 [examples/](examples/) 目录。

可用示例：
- `state_api_demo.rs` - 简单的状态读取和打印
- `realtime_control_demo.rs` - 实时控制演示（双线程架构）
- `robot_monitor.rs` - 机器人状态监控
- `timestamp_verification.rs` - 时间戳同步验证

计划中的示例：
- `torque_control.rs` - 力控演示
- `configure_can.rs` - CAN 波特率配置工具

## 🤝 贡献

欢迎贡献！请查看 [CONTRIBUTING.md](CONTRIBUTING.md) 了解详细信息。

## 📄 许可证

本项目采用 MIT 许可证。详见 [LICENSE](LICENSE) 文件。

## 📖 文档

详细的设计文档请参阅：
- [架构设计文档](docs/v0/TDD.md)
- [协议文档](docs/v0/protocol.md)
- [实时配置指南](docs/v0/realtime_configuration.md)
- [实时优化指南](docs/v0/realtime_optimization.md)
- [迁移指南](docs/v0/MIGRATION_GUIDE.md) - 从 v0.1.x 迁移到 v0.2.0+ 的指南
- [位置控制与 MOVE 模式用户指南](docs/v0/position_control_user_guide.md) - 位置控制和运动类型完整指南

## 🔗 相关链接

- [松灵机器人](https://www.agilex.ai/)
- [bilge](https://docs.rs/bilge/)
- [rusb](https://docs.rs/rusb/)

---

**注意**：本项目正在积极开发中，API 可能会有变更。建议在生产环境使用前仔细测试。

