# Piper SDK

[![Crates.io](https://img.shields.io/crates/v/piper-sdk)](https://crates.io/crates/piper-sdk)
[![Documentation](https://docs.rs/piper-sdk/badge.svg)](https://docs.rs/piper-sdk)
[![License](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

**高性能、跨平台（Linux/Windows/macOS）、零抽象开销**的 Rust SDK，专用于松灵 Piper 机械臂的高频力控（500Hz）和异步 CAN 帧录制。

[English README](README.md)

> **⚠️ 重要提示**
> **本项目正在积极开发中。API 可能会发生变化。请在生产环境中使用前仔细测试。**
>
> **版本状态**：当前版本为 **0.1.0 之前**（alpha 质量阶段）。SDK **尚未在真实机械臂上进行全面测试**，可能无法正确或安全地工作。
>
> **⚠️ 安全警告**：未经全面测试，请勿在生产环境或真实机械臂上使用此 SDK。软件可能发送错误的指令，导致机械臂损坏或造成安全危险。

## ✨ 核心特性

- 🚀 **零抽象开销**：编译期多态，运行时无虚函数表（vtable）开销
- ⚡ **高性能读取**：基于 `ArcSwap` 的无锁状态读取，纳秒级响应
- 🔄 **无锁并发**：采用 RCU（Read-Copy-Update）机制，实现高效的状态共享
- 🎯 **类型安全**：使用 `bilge` 进行位级协议解析，编译期保证数据正确性
- 🌍 **跨平台支持（Linux/Windows/macOS）**：
  - **Linux**: 同时支持 SocketCAN（内核级性能）和 GS-USB（通过 libusb 用户态实现）
  - **Windows/macOS**: 基于 `rusb` 实现用户态 GS-USB 驱动（免驱动/通用）
- 🎬 **异步 CAN 帧录制**：
  - **非阻塞钩子**：使用 `try_send` 实现 <1μs 帧开销
  - **有界队列**：10,000 帧容量，防止 1kHz 时 OOM
  - **硬件时间戳**：直接使用内核/驱动中断时间戳
  - **TX 安全**：仅在成功 `send()` 后录制帧
  - **丢帧监控**：内置 `dropped_frames` 计数器
- 📊 **高级健康监控**（piper_bridge_host，控制进程持有的非实时 bridge/debug 路径）：
  - **CAN Bus Off 检测**：检测 CAN Bus Off 事件（关键系统故障），带防抖机制
  - **Error Passive 监控**：监控 Error Passive 状态（Bus Off 前警告），用于早期检测
  - **USB STALL 跟踪**：跟踪 USB 端点 STALL 错误，监控 USB 通信健康状态
  - **性能基线**：使用 EWMA 进行动态 FPS 基线跟踪，用于异常检测
  - **健康评分**：基于多项指标的综合健康评分（0-100）

## 🏗️ 架构

Piper SDK 使用模块化工作空间架构，职责清晰分离：

```
piper-sdk-rs/
├── crates/
│   ├── piper-protocol/    # 协议层（位级 CAN 协议）
│   ├── piper-can/         # CAN 抽象（SocketCAN/GS-USB）
│   ├── piper-driver/      # 驱动层（I/O 线程、状态同步、钩子）
│   ├── piper-client/      # 客户端层（类型安全用户 API）
│   ├── piper-tools/       # 录制和分析工具
│   └── piper-sdk/         # 兼容层（重新导出所有）
└── apps/
    ├── cli/               # 命令行接口
    └── daemon/            # 控制进程持有的 bridge host 二进制
```

### 层次概览

| 层 | Crate | 用途 | 测试覆盖 |
|------|-------|---------|---------|
| 协议 | `piper-protocol` | 类型安全的 CAN 协议编码/解码 | 214 测试 ✅ |
| CAN | `piper-can` | CAN 适配器硬件抽象 | 97 测试 ✅ |
| 驱动 | `piper-driver` | I/O 管理、状态同步、钩子 | 149 测试 ✅ |
| 客户端 | `piper-client` | 高级类型安全 API | 105 测试 ✅ |
| 工具 | `piper-tools` | 录制、统计、安全 | 23 测试 ✅ |
| SDK | `piper-sdk` | 兼容层（重新导出） | 588 测试 ✅ |

**优势**：
- ✅ **编译更快**：仅重新编译修改的层（快达 88%）
- ✅ **依赖灵活**：可依赖特定层以减少依赖
- ✅ **边界清晰**：每层职责明确
- ✅ **100% 向后兼容**：现有代码无需任何更改

详见[工作空间迁移指南](docs/v0/workspace/USER_MIGRATION_GUIDE.md)。

## 🛠️ 技术栈

| 模块 | Crates | 用途 |
|------|--------|------|
| CAN 接口 | 自定义 `CanAdapter` | 轻量级 CAN 适配器 Trait（无嵌入式负担） |
| Linux 后端 | `socketcan` | Linux 原生 CAN 支持（SocketCAN 接口） |
| USB 后端 | `rusb` | Windows/macOS 下操作 USB 设备，实现 GS-USB 协议 |
| 协议解析 | `bilge` | 位操作、非对齐数据处理，替代 serde |
| 并发模型 | `crossbeam-channel` | 高性能 MPSC 通道，用于发送控制指令 |
| 状态共享 | `arc-swap` | RCU 机制，实现无锁读取最新状态 |
| 帧钩子 | `hooks` + `recording` | 非阻塞异步录制，有界队列 |
| 错误处理 | `thiserror` | SDK 内部精确的错误枚举 |
| 日志 | `tracing` | 结构化日志记录 |

## 📦 安装

在 `Cargo.toml` 中添加依赖：

```toml
[dependencies]
piper-sdk = "0.1"
```

### 可选特性

#### Serde 序列化支持

启用数据类型的序列化/反序列化：

```toml
[dependencies]
piper-sdk = { version = "0.1", features = ["serde"] }
```

这将添加 `Serialize` 和 `Deserialize` 实现到：
- 类型单位（`Rad`、`Deg`、`NewtonMeter` 等）
- 关节数组和关节索引
- 笛卡尔位姿和四元数类型
- **CAN 帧（`PiperFrame`、`GsUsbFrame`）** - 用于帧转储/回放

使用示例：

```rust
use piper_sdk::prelude::*;
use serde_json;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 序列化关节位置
    let positions = JointArray::from([
        Rad(0.0), Rad(0.5), Rad(0.0),
        Rad(0.0), Rad(0.0), Rad(0.0)
    ]);

    let json = serde_json::to_string(&positions)?;
    println!("序列化: {}", json);

    // 反序列化回来
    let deserialized: JointArray<Rad> = serde_json::from_str(&json)?;

    Ok(())
}
```

#### 帧转储示例

用于 CAN 帧录制和回放：

```bash
# 运行帧转储示例
cargo run -p piper-sdk --example frame_dump --features serde
```

这演示了：
- 将 CAN 帧录制到 JSON
- 保存/加载帧数据
- 调试 CAN 总线通信

详见 [examples/frame_dump.rs](../crates/piper-sdk/examples/frame_dump.rs)。

### 平台特定特性

特性会根据目标平台自动选择：
- **Linux**: `socketcan`（SocketCAN 支持）
- **Linux/macOS/Windows**: `gs_usb`（GS-USB USB 适配器）

无需手动配置平台选择特性！

### 高级用法：依赖特定层

为减少依赖，可直接依赖特定层：

```toml
# 仅使用客户端层（最常见）
[dependencies]
piper-client = "0.1"

# 仅使用驱动层（高级用户）
[dependencies]
piper-driver = "0.1"

# 仅使用工具（录制/分析）
[dependencies]
piper-tools = "0.1"
```

**注意**：使用特定层时，需要更新导入：
- `piper_sdk::Piper` → `piper_client::Piper`
- `piper_sdk::Driver` → `piper_driver::Piper`

详见[工作空间迁移指南](docs/v0/workspace/USER_MIGRATION_GUIDE.md)了解迁移详情。

## 🚀 快速开始

### 基本使用（客户端 API - 推荐）

大多数用户应该使用高级客户端 API，提供类型安全、易于使用的控制接口：

```rust
use piper_sdk::prelude::*;

fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    // 在 Linux 上显式使用 SocketCAN 连接。
    // macOS/Windows 请改用 `.gs_usb_auto()` 或 `.gs_usb_serial(...)`。
    let robot = PiperBuilder::new()
        .socketcan("can0")
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

### CAN 帧录制

使用非阻塞钩子异步录制 CAN 帧：

```rust
use piper_driver::recording::AsyncRecordingHook;
use piper_driver::hooks::FrameCallback;
use piper_sdk::prelude::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::Duration;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 创建录制钩子
    let (hook, rx) = AsyncRecordingHook::new();
    let dropped_counter = hook.dropped_frames().clone();

    // 注册为回调
    let callback = Arc::new(hook) as Arc<dyn FrameCallback>;

    // 连接机器人
    let robot = PiperBuilder::new()
        .socketcan("can0")
        .build()?;

    // 在驱动层注册钩子
    // （注意：这是高级用法 - 参见驱动 API 文档）
    robot.context.hooks.write()?.add_callback(callback);

    // 启动录制线程
    let handle = thread::spawn(move || {
        let mut file = std::fs::File::create("recording.bin")?;
        while let Ok(frame) = rx.recv() {
            // 处理帧：写入文件、分析等
            println!("接收帧: ID=0x{:03X}, timestamp={}us",
                     frame.id, frame.timestamp_us);
        }
        Ok::<_, Box<dyn std::error::Error>>(())
    });

    // 运行 5 秒
    thread::sleep(Duration::from_secs(5));

    // 检查丢帧数
    let dropped = dropped_counter.load(Ordering::Relaxed);
    println!("丢帧数: {}", dropped);

    handle.join().ok();
    Ok(())
}
```

**核心特性**：
- ✅ **非阻塞**：每帧开销 `<1μs`
- ✅ **OOM 安全**：有界队列（1kHz 时 10,000 帧 = 10s 缓冲）
- ✅ **硬件时间戳**：来自内核/驱动的微秒级精度
- ✅ **TX 安全**：仅录制成功发送的帧
- ✅ **丢失跟踪**：内置 `dropped_frames` 计数器

## 🎬 录制与回放

Piper SDK 提供三个互补的 API 用于 CAN 帧录制和回放：

| API | 使用场景 | 复杂度 | 安全性 |
|-----|----------|------------|--------|
| **标准录制** | 简单的录制保存工作流 | ⭐ 低 | ✅ 类型安全 |
| **自定义诊断** | 实时帧分析和自定义处理 | ⭐⭐ 中 | ✅ 线程安全 |
| **回放模式** | 安全回放预先录制的会话 | ⭐⭐ 中 | ✅ 类型安全 + 驱动层保护 |

### 1. 标准录制 API

将 CAN 帧录制到文件的最简单方式：

```rust
use piper_client::{PiperBuilder, recording::{RecordingConfig, RecordingMetadata, StopCondition}};
use std::time::Duration;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 连接到机器人
    let robot = PiperBuilder::new()
        .socketcan("can0")
        .build()?;

    // 启动录制（带元数据）
    let (robot, handle) = robot.start_recording(RecordingConfig {
        output_path: "demo_recording.bin".into(),
        stop_condition: StopCondition::Duration(10), // 录制 10 秒
        metadata: RecordingMetadata {
            notes: "标准录制示例".to_string(),
            operator: "DemoUser".to_string(),
        },
    })?;

    // 执行操作（所有 CAN 帧都会被录制）
    tokio::time::sleep(Duration::from_secs(10)).await;

    // 停止录制并获取统计信息
    let (robot, stats) = robot.stop_recording(handle)?;

    println!("录制了 {} 帧，耗时 {:.2} 秒", stats.frame_count, stats.duration.as_secs_f64());
    println!("丢帧数: {}", stats.dropped_frames);

    Ok(())
}
```

**核心特性**：
- ✅ **自动停止条件**：时长、帧数或手动停止
- ✅ **丰富的元数据**：记录操作员、备注、时间戳
- ✅ **统计信息**：帧数、时长、丢帧数
- ✅ **类型安全**：录制句柄防止误用

完整示例参见 [examples/standard_recording.rs](examples/standard_recording.rs)

### 2. 自定义诊断 API

高级用户可以注册自定义帧回调进行实时分析：

```rust
use piper_client::PiperBuilder;
use piper_driver::recording::AsyncRecordingHook;
use std::sync::Arc;
use std::thread;

fn main() -> anyhow::Result<()> {
    // 连接并使能机器人
    let robot = PiperBuilder::new()
        .socketcan("can0")
        .build()?;
    let active = robot.enable_position_mode(Default::default())?;

    // 获取诊断接口
    let diag = active.diagnostics();

    // 创建自定义录制钩子
    let (hook, rx) = AsyncRecordingHook::new();
    let dropped_counter = hook.dropped_frames().clone();

    // 注册钩子
    let callback = Arc::new(hook) as Arc<dyn piper_driver::FrameCallback>;
    diag.register_callback(callback)?;

    // 在后台线程处理帧
    thread::spawn(move || {
        let mut frame_count = 0;
        while let Ok(frame) = rx.recv() {
            frame_count += 1;

            // 自定义分析：例如 CAN ID 分布、时序分析
            if frame_count % 1000 == 0 {
                println!("收到帧: ID=0x{:03X}", frame.id);
            }
        }

        println!("总帧数: {}", frame_count);
        println!("丢帧: {}", dropped_counter.load(std::sync::atomic::Ordering::Relaxed));
    });

    // 执行操作...
    thread::sleep(std::time::Duration::from_secs(5));

    // 关闭
    let _standby = active.shutdown()?;

    Ok(())
}
```

**核心特性**：
- ✅ **实时处理**：帧到达时即时分析
- ✅ **自定义逻辑**：实现任何分析算法
- ✅ **后台线程**：主线程不阻塞
- ✅ **丢失跟踪**：监控丢帧数

完整示例参见 [examples/custom_diagnostics.rs](examples/custom_diagnostics.rs)

### 3. 回放模式 API

使用驱动层保护安全地回放预先录制的会话：

```rust
use piper_client::PiperBuilder;

fn main() -> anyhow::Result<()> {
    // 连接到机器人
    let robot = PiperBuilder::new()
        .socketcan("can0")
        .build()?;

    // 进入回放模式（驱动 TX 线程自动暂停）
    let replay = robot.enter_replay_mode()?;

    // 以 2.0x 速度回放录制
    let robot = replay.replay_recording("demo_recording.bin", 2.0)?;

    // 自动退出回放模式（TX 线程恢复）
    println!("回放完成！");

    Ok(())
}
```

**安全特性**：
- ✅ **驱动层保护**：回放期间 TX 线程暂停（无双控制流）
- ✅ **速度限制**：最大 5.0x，推荐 ≤ 2.0x 并有警告
- ✅ **类型安全转换**：在回放模式下无法调用使能/失能
- ✅ **自动清理**：总是返回到待机状态

**速度指南**：
- **1.0x**：原始速度（推荐大多数使用场景）
- **0.1x ~ 2.0x**：测试/调试的安全范围
- **> 2.0x**：谨慎使用 - 确保安全环境
- **最大值**：5.0x（安全硬限制）

完整示例参见 [examples/replay_mode.rs](examples/replay_mode.rs)

### CLI 使用

`piper-cli` 工具提供了录制和回放的便捷命令：

```bash
# 录制 CAN 帧
piper-cli record -o demo.bin --duration 10

# 回放录制（正常速度）
piper-cli replay -i demo.bin

# 以 2.0x 速度回放
piper-cli replay -i demo.bin --speed 2.0

# 回放时跳过确认提示
piper-cli replay -i demo.bin --confirm
```

### 完整工作流示例

```bash
# 步骤 1: 录制会话
cargo run --example standard_recording

# 步骤 2: 分析录制
cargo run --example custom_diagnostics

# 步骤 3: 安全回放录制
cargo run --example replay_mode
```

### 架构亮点

#### 为什么是三个 API？

每个 API 服务于不同的目的：

1. **标准录制**：适合想要"直接录制"的用户，无需复杂配置
2. **自定义诊断**：适合研究人员开发自定义分析工具
3. **回放模式**：适合测试工程师重现 bug 或测试序列

#### 通过类型状态实现类型安全

ReplayMode API 使用 Rust 类型系统实现编译期安全：

```rust
// ✅ 编译期错误：在回放模式下无法使能
let replay = robot.enter_replay_mode()?;
let active = replay.enable_position_mode(...);  // 错误！

// ✅ 必须先退出回放模式
let robot = replay.replay_recording(...)?;
let active = robot.enable_position_mode(...);  // OK!
```

#### 驱动层保护

ReplayMode 将驱动切换到 `DriverMode::Replay`，从而：

- **暂停周期性 TX**：驱动停止发送自动控制命令
- **允许显式帧**：只有回放帧被发送到 CAN 总线
- **防止冲突**：无双控制流（驱动 vs 回放）

此设计记录在[架构分析](docs/architecture/piper-driver-client-mixing-analysis.md)中。

### 高级使用（驱动层 API）

需要直接控制 CAN 帧或追求最高性能时，使用驱动层 API：

```rust
use piper_sdk::driver::PiperBuilder;

fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    // 创建驱动实例
    let robot = PiperBuilder::new()
        .socketcan("can0")  // Linux: 显式指定 SocketCAN 目标
        .baud_rate(1_000_000)  // CAN 波特率
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

为优化性能，状态数据分为两类：

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

- **CAN 层** (`can`)：CAN 硬件抽象，支持 SocketCAN 和 GS-USB
- **协议层** (`protocol`)：类型安全的协议编码/解码
- **驱动层** (`driver`)：IO 线程管理、状态同步、帧解析
  - **钩子系统**：用于帧录制的运行时回调注册
  - **录制模块**：带有界队列的异步非阻塞录制
- **客户端层** (`client`)：类型安全、易用的控制接口
- **工具层** (`tools`)：录制格式、统计、安全验证

### 核心组件

```
piper-sdk-rs/
├── crates/
│   ├── piper-protocol/
│   │   └── src/
│   │       ├── lib.rs          # 协议模块入口
│   │       ├── ids.rs          # CAN ID 常量/枚举
│   │       ├── feedback.rs     # 机械臂反馈帧 (bilge)
│   │       ├── control.rs      # 控制指令帧 (bilge)
│   │       └── config.rs       # 配置帧 (bilge)
│   ├── piper-can/
│   │   └── src/
│   │       ├── lib.rs          # CAN 模块入口
│   │       ├── socketcan/      # [Linux] SocketCAN 实现
│   │       └── gs_usb/         # [Win/Mac/Linux] GS-USB 协议
│   ├── piper-driver/
│   │   └── src/
│   │       ├── mod.rs          # 驱动模块入口
│   │       ├── piper.rs        # 驱动层 Piper 对象 (API)
│   │       ├── pipeline.rs     # IO Loop、ArcSwap 更新逻辑
│   │       ├── state.rs        # 状态结构定义
│   │       ├── hooks.rs        # 帧回调钩子系统
│   │       ├── recording.rs    # 带有界队列的异步录制
│   │       ├── builder.rs      # PiperBuilder（链式构造）
│   │       └── metrics.rs      # 性能指标
│   ├── piper-client/
│   │   └── src/
│   │       ├── mod.rs          # 客户端模块入口
│   │       ├── observer.rs      # Observer（只读状态访问）
│   │       ├── state/           # Type State Pattern 状态机
│   │       ├── motion.rs       # Piper 命令接口
│   │       └── types/           # 类型系统（单位、关节、错误）
│   └── piper-tools/
│       └── src/
│           ├── recording.rs    # 录制格式和工具
│           ├── statistics.rs    # CAN 统计分析
│           └── safety.rs        # 安全验证
└── apps/
    └── cli/
        └── src/
            ├── commands/       # CLI 命令
            └── modes/          # CLI 模式（repl、oneshot）
```

### 并发模型

采用**异步 IO 思想但用同步线程实现**（保证确定性延迟）：

1. **IO 线程**：负责 CAN 帧的收发和状态更新
2. **控制线程**：通过 `ArcSwap` 无锁读取最新状态，通过 `crossbeam-channel` 发送指令
3. **Frame Commit 机制**：确保控制线程读取的状态是一致的时间点快照
4. **钩子系统**：在 RX/TX 帧上触发的非阻塞回调用于录制

## 📚 示例

查看 `examples/` 目录了解更多示例：

> **注意**：示例代码正在开发中。更多示例请查看 [examples/](examples/) 目录。

可用示例：
- `state_api_demo.rs` - 简单的状态读取和打印
- `realtime_control_demo.rs` - 实时控制演示（双线程架构）
- `robot_monitor.rs` - 机器人状态监控
- `timestamp_verification.rs` - 时间戳同步验证
- `standard_recording.rs` - 📼 标准录制 API 使用（录制 CAN 帧到文件）
- `custom_diagnostics.rs` - 🔧 自定义诊断接口（实时帧分析）
- `replay_mode.rs` - 🔄 回放模式 API（安全 CAN 帧回放）

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
- **[钩子系统代码审查](docs/architecture/code-review-v1.2.1-hooks-system.md)** - 录制系统设计深度剖析
- **[全仓库代码审查](docs/architecture/code-review-full-repo-v1.2.1.md)** - 代码库综合分析

## 🔗 相关链接

- [松灵机器人](https://www.agilex.ai/)
- [bilge](https://docs.rs/bilge/)
- [rusb](https://docs.rs/rusb/)
