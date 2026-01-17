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
  - **Linux**: 基于 SocketCAN（内核级性能）
  - **Windows/macOS**: 基于 `rusb` 实现用户态 GS-USB 驱动（免驱动/通用）

## 🛠️ 技术栈

| 模块 | Crates | 用途 |
|------|--------|------|
| CAN 接口 | 自定义 `CanAdapter` | 轻量级 CAN 适配器 Trait（无嵌入式负担） |
| Linux 后端 | `socketcan` | Linux 原生 CAN 支持（计划中） |
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

## 🚀 快速开始

### 基本使用

```rust
use piper_sdk::PiperBuilder;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 创建 Piper 实例
    let robot = PiperBuilder::new()
        .interface("can0")?  // Linux: SocketCAN 接口名
        .baud_rate(1_000_000)?  // CAN 波特率
        .build()?;

    // 获取当前状态（无锁，纳秒级返回）
    let core_motion = robot.get_core_motion();
    println!("关节位置: {:?}", core_motion.joint_pos);

    let joint_dynamic = robot.get_joint_dynamic();
    println!("关节速度: {:?}", joint_dynamic.joint_vel);

    // 发送控制帧
    let frame = piper_sdk::PiperFrame::new_standard(0x1A1, &[0x01, 0x02, 0x03]);
    robot.send_frame(frame)?;

    Ok(())
}
```

## 🏗️ 架构设计

### 热冷数据分离（Hot/Cold Splitting）

为了优化性能，状态数据分为三类：

- **热数据（Hot，500Hz）**：
  - `CoreMotionState`：核心运动状态（关节位置、末端位姿）
  - `JointDynamicState`：关节动态状态（关节速度、电流）
  - 使用 `ArcSwap` 实现无锁读取，Frame Commit 机制保证原子性

- **温数据（Warm，100Hz）**：
  - `ControlStatusState`：控制状态（控制模式、机器人状态、故障码等）
  - 使用 `ArcSwap` 进行读写，更新频率中等

- **冷数据（Cold，10Hz 或按需）**：
  - `DiagnosticState`：诊断信息（电机温度、总线电压、错误码等）
  - `ConfigState`：配置信息（固件版本、关节限位、PID 参数等）
  - 使用 `RwLock` 进行读写，更新频率低

### 核心组件

```
piper-rs/
├── src/
│   ├── lib.rs              # 库入口，模块导出
│   ├── can/                # CAN 通讯适配层
│   │   ├── mod.rs          # CAN 适配器 Trait 和通用类型
│   │   └── gs_usb/         # [Win/Mac] GS-USB 协议实现
│   │       ├── mod.rs      # GS-USB CAN 适配器
│   │       ├── device.rs   # USB 设备操作
│   │       ├── protocol.rs # GS-USB 协议定义
│   │       └── frame.rs    # GS-USB 帧结构
│   ├── protocol/           # 协议定义（业务无关，纯数据）
│   │   ├── ids.rs          # CAN ID 常量/枚举
│   │   ├── feedback.rs     # 机械臂反馈帧 (bilge)
│   │   ├── control.rs      # 控制指令帧 (bilge)
│   │   └── config.rs       # 配置帧 (bilge)
│   └── robot/              # 核心业务逻辑
│       ├── mod.rs          # Robot 模块入口
│       ├── robot_impl.rs   # 对外的高级 Piper 对象 (API)
│       ├── pipeline.rs     # IO Loop、ArcSwap 更新逻辑
│       ├── state.rs        # 状态结构定义（热冷数据分离）
│       ├── builder.rs      # PiperBuilder（链式构造）
│       └── error.rs        # RobotError（错误类型）
```

### 并发模型

采用**异步 IO 思想但用同步线程实现**（保证确定性延迟）：

1. **IO 线程**：负责 CAN 帧的收发和状态更新
2. **控制线程**：通过 `ArcSwap` 无锁读取最新状态，通过 `crossbeam-channel` 发送指令
3. **Frame Commit 机制**：确保控制线程读取的状态是一致的时间点快照

## 📚 示例

查看 `examples/` 目录了解更多示例：

> **注意**：示例代码正在开发中。更多示例请查看 [examples/](examples/) 目录。

计划包含的示例：
- `read_state.rs` - 简单的状态读取和打印
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

## 🔗 相关链接

- [松灵机器人](https://www.agilex.ai/)
- [bilge](https://docs.rs/bilge/)
- [rusb](https://docs.rs/rusb/)

---

**注意**：本项目正在积极开发中，API 可能会有变更。建议在生产环境使用前仔细测试。

