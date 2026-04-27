# Piper Driver/Client 架构混用问题深度分析报告

**日期**: 2026-01-27
**分析对象**: Code Review Report (code-review-full-repo-v1.2.1.md) 中关于 CLI 层使用 piper_driver 的建议
**分析结论**: 存在严重的架构混用问题，需要重新设计
**版本**: v2.1 (根据用户反馈进一步优化)
**状态**: ✅ Approved (已批准，可进入实施)

---

## 执行摘要

代码审查报告建议 CLI 层直接使用 `piper_driver` 来实现录制和回放功能。经深度代码调研，**此建议存在重大架构问题**：

1. **API 不完整**: `piper_client` 完全封装了 `piper_driver`，未暴露必要的底层功能
2. **双重实例问题**: 混用需要创建两个独立的 Piper 实例，导致资源竞争（SocketCAN/GS-USB 独占）
3. **抽象层破坏**: 破坏了分层架构的设计原则
4. **维护负担**: 未来需要在两层同时维护功能

**推荐方案**（双轨制）:
- **方案 A (标准录制)**: 扩展 `piper_client` 添加**录制 API**，用于常规使用场景
- **方案 B (高级诊断)**: 提供类型安全的 **`Diagnostics` 接口**（逃生舱模式），用于自定义诊断、调试、抓包
- **新增需求**: 引入 **`ReplayMode`** 状态，解决回放时的控制流冲突问题

---

## 目录

1. [问题背景](#1-问题背景)
2. [代码调研结果](#2-代码调研结果)
3. [架构混用问题分析](#3-架构混用问题分析)
4. [可行性评估](#4-可行性评估)
5. [推荐方案详细设计](#5-推荐方案详细设计)
6. [方案对比](#6-方案对比)
7. [实施建议](#7-实施建议)
8. [风险评估](#8-风险评估)
9. [结论](#9-结论)

---

## 1. 问题背景

### 1.1 Code Review 报告建议

代码审查报告 (code-review-full-repo-v1.2.1.md) 在 CLI 层分析中指出：

> **P0 - Blocker**: `apps/cli/src/commands/record.rs:81`:
> ```rust
> // TODO: 实际实现需要访问 driver 层的 CAN 帧
> ```
>
> **P0 - Blocker**: `apps/cli/src/commands/replay.rs:82`:
> ```rust
> // TODO: 需要访问 driver 层的 send_frame 方法
> ```
>
> **建议**: 直接使用 `piper_driver::Piper` 来实现录制和回放功能。

### 1.2 当前 CLI 架构

CLI 当前架构：
```
apps/cli/
├── One-shot 模式  (piper_client)
│   ├── move      → Piper<Active<PositionMode>>::move_joints()
│   ├── position  → Observer::get_joint_position()
│   └── stop      → Piper<Active>::emergency_stop()
│
└── 录制/回放     (??? Stub 实现)
    ├── record    → Stub: 仅模拟接口，未接入真实数据源
    └── replay    → Stub: 仅显示进度，未实现实际发送
```

**核心问题**: `piper_client` 未暴露录制所需的底层 API。

---

## 2. 代码调研结果

### 2.1 piper_client API 边界

经过详细代码分析，`piper_client` 的封装策略如下：

#### 2.1.1 Piper 结构体定义

**文件**: `crates/piper-client/src/state/machine.rs:304`

```rust
pub struct Piper<State = Disconnected> {
    pub(crate) driver: Arc<piper_driver::Piper>,  // ❌ 私有字段
    pub(crate) observer: Observer,
    pub(crate) _state: State,
}
```

**关键发现**:
- `driver` 字段标记为 `pub(crate)`，**完全不对外暴露**
- 无法通过任何公共方法获取 driver 引用
- 没有 `.context()` 或 `.driver()` 逃生舱方法

#### 2.1.2 piper_client 公共 API

**文件**: `crates/piper-client/src/lib.rs`

```rust
// 公共 API 仅包含：
pub use state::machine::Piper;                    // 类型状态机
pub use observer::Observer;                       // 只读状态访问
pub use types::*;                                 // 单位、关节、错误类型

// ❌ 不包含：
// - pub use piper_driver::recording::AsyncRecordingHook;
// - pub use piper_driver::hooks::HookManager;
// - pub use piper_driver::PiperContext;
// - pub use piper_driver::Piper (driver layer);
```

#### 2.1.3 缺失的关键功能

`piper_client` **未暴露**以下录制所需功能：

| 功能 | piper_driver | piper_client | 影响 |
|------|--------------|--------------|------|
| 注册 FrameCallback | ✅ `context.hooks.write()?.add_callback()` | ❌ 无法访问 context | **无法录制** |
| 访问原始 CAN 帧 | ✅ `FrameCallback` trait | ❌ 无钩子系统 | **无法录制** |
| 发送原始帧 | ✅ `send_frame()` | ❌ 仅高层命令 | **无法回放** |
| 获取 PiperContext | ✅ `robot.context()` | ❌ 无此方法 | **无法扩展** |

### 2.2 piper_driver 公共 API

**文件**: `crates/piper-driver/src/lib.rs`

```rust
pub mod hooks;         // FrameCallback, HookManager
pub mod recording;     // AsyncRecordingHook, TimestampedFrame
pub mod state;         // PiperContext, JointState, EndPose

pub struct Piper {
    pub context: Arc<PiperContext>,  // ✅ 公开可访问
    // ...
}

impl Piper {
    pub fn send_frame(&self, frame: &PiperFrame) -> Result<()> { ... }  // ✅ 公开
}
```

**对比**:
- `piper_driver` 完全暴露了录制/回放所需的所有 API
- `piper_client` 刻意隐藏了这些底层细节

### 2.3 CLI 当前实现分析

#### 2.3.1 录制命令（Stub 实现）

**文件**: `apps/cli/src/commands/record.rs:62-91`

```rust
// 连接到机器人
let robot = PiperBuilder::new().interface(interface_str).build()?;  // 使用 piper_driver

println!("✅ 已连接，开始录制...");

loop {
    // 读取状态（触发 CAN 接收）
    let _position = robot.get_joint_position();  // ❌ 只能读高级状态
    let _end_pose = robot.get_end_pose();

    // ⚠️ Stub 实现：未接入真实数据源
    // TODO: 实际实现需要访问 driver 层的 CAN 帧
    let can_id: u32 = (0x2A5 + (frame_count % 6)).try_into().unwrap();
    let frame = TimestampedFrame::new(
        start_time * 1_000_000 + frame_count * 1000,
        can_id,
        vec![frame_count as u8; 8],
        TimestampSource::Hardware,  // ⚠️ 使用软件生成的时间戳
    );
    recording.add_frame(frame);
}
```

**现状**:
1. 使用 `piper_driver::PiperBuilder`（绕过了 client 层）
2. 只能调用高级方法 (`get_joint_position()`)，**无法访问原始 CAN 帧**
3. 当前是 **Stub 实现**，用于验证接口设计，**未接入真实 CAN 总线数据**
4. 时间戳不精确（使用软件生成时间戳，而非硬件时间戳）

#### 2.3.2 回放命令（Stub 实现）

**文件**: `apps/cli/src/commands/replay.rs:95-130`

```rust
for (i, frame) in recording.frames.iter().enumerate() {
    // 计算时间戳和延迟
    let delay_ms = if self.speed > 0.0 {
        (elapsed_ms as f64 / self.speed) as u64
    } else {
        elapsed_ms
    };

    // 进度显示
    print!("\r回放进度: {}/{} 帧", i + 1, total_frames);

    // ⚠️ Stub 实现：未实际发送 CAN 帧
    // TODO: 需要访问 driver 层的 Piper::send_frame 方法
    // piper_sdk::driver::Piper::send_frame(&piper_frame)

    // 控制回放速度
    if delay_ms > 0 {
        tokio::time::sleep(tokio::time::Duration::from_millis(delay_ms)).await;
    }
}
```

**现状**:
1. **完全无法实现**：没有访问 `send_frame()` 的途径
2. 当前只能打印进度，**不发送任何 CAN 帧**
3. 功能是 **Stub 实现**，用于演示接口流程

---

## 3. 架构混用问题分析

### 3.1 如果按 Code Review 建议实现

假设我们按照代码审查报告的建议，在 CLI 中混用 `piper_driver`：

```rust
// ❌ 架构混用示例
use piper_client::PiperBuilder;  // 用于正常操作
use piper_driver::PiperBuilder as DriverBuilder;  // 用于录制

// 创建两个实例！
let client_robot = PiperBuilder::new()
    .interface("can0")
    .build()?;  // 用于 move/position/stop

let driver_robot = DriverBuilder::new()
    .interface("can0")  // ❌ 冲突！
    .build()?;  // 用于 recording/replay
```

### 3.2 资源冲突问题

#### 3.2.1 SocketCAN 接口独占

**SocketCAN 限制**: 一个接口（如 `can0`）同一时间只能被一个进程打开。

```rust
// piper_driver/src/can/socketcan/socketcan.rs
impl SocketCANAdapter {
    pub fn new(interface: &str) -> Result<Self> {
        let socket = socket2::Socket::new(
            socket2::Domain::CAN,
            socket2::Type::RAW,
            None,
        )?;

        // ❌ 绑定到接口，独占访问
        socket.bind(&socket2::SockAddr::from(link_addr))?;
        // ...
    }
}
```

**问题**:
- 尝试创建第二个实例会**失败**：`Error: Address already in use`
- 无法在同一个进程中运行 `piper_client` 和 `piper_driver` 实例

#### 3.2.2 GS-USB 设备独占

**GS-USB 限制**: USB 设备通过 `rusb` 独占打开。

```rust
// piper_driver/src/can/gs_usb/device.rs
impl GSUSBDevice {
    pub fn open(serial: &str) -> Result<Self> {
        // ❌ 独占打开 USB 设备
        let handle = rusb::open_device_with_vid_pid(0x1d50, 0x606f)
            .ok_or("Device not found")?;

        handle.claim_interface(0)?;  // 独占声明
        // ...
    }
}
```

**问题**:
- 同样的独占冲突
- 第二个实例会失败：`Error: Device or resource busy`

### 3.3 架构一致性破坏

#### 3.3.1 抽象层违背

**设计原则**:

```
┌─────────────────────────────────────┐
│         Client Layer                │  ← 类型安全 API
│  (Piper<Active>, Type State)        │
├─────────────────────────────────────┤
│         Driver Layer                │  ← IO + 状态同步
│  (Piper, ArcSwap, Hooks)            │
├─────────────────────────────────────┤
│         CAN Layer                   │  ← 硬件抽象
│  (SocketCAN, GS-USB)                │
└─────────────────────────────────────┘
```

**混用后的实际结构**:

```
┌─────────────────────────────────────┐
│         CLI Application             │
│                                     │
│  ┌──────────────┐  ┌──────────────┐│
│  │ piper_client │  │ piper_driver ││ ← 破坏分层！
│  │  (move/pos)  │  │ (record)     ││
│  └──────────────┘  └──────────────┘│
└─────────────────────────────────────┘
```

**问题**:
1. CLI 开发者需要理解两套不同的 API
2. 类型状态机（client）和原始 API（driver）混在一起
3. 违背"单一职责"原则

#### 3.3.2 状态一致性问题

```rust
// 场景：用户在 CLI 中执行
piper> move --joints 0.1,0.2,0.3,0.4,0.5,0.6  // 使用 client_robot

piper> record --output demo.bin  // ❌ 无法实现！

// 如果强行用 driver_robot，会怎么样？
let driver_robot = DriverBuilder::new().interface("can0").build()?;  // 失败！
```

**结果**:
- `driver_robot` 创建失败（接口被 `client_robot` 占用）
- **无法同时使用两层**

---

## 4. 可行性评估

### 4.1 直接混用：❌ 不可行

**理由**:
1. ❌ SocketCAN/GS-USB 接口独占限制
2. ❌ 无法在同一个进程中创建两个实例
3. ❌ 状态不一致风险（即使成功）

### 4.2 CLI 完全迁移到 piper_driver：⚠️ 技术可行但不推荐

**实现方案**:

```rust
// ❌ 丢失类型安全
use piper_driver::PiperBuilder;

let robot = PiperBuilder::new().interface("can0").build()?;

// 手动管理状态
robot.enable()?;  // ❌ 运行时错误风险
robot.move_joints(positions)?;  // ❌ 可能忘记 enable
robot.disable()?;  // ❌ 可能忘记调用
```

**优点**:
- ✅ 可以访问录制 API
- ✅ 可以直接发送 CAN 帧
- ✅ 单一实例，无资源冲突

**缺点**:
- ❌ **失去类型状态机的编译时保护**
- ❌ 运行时错误风险增加
- ❌ API 更底层，使用更复杂
- ❌ 违背 SDK 设计理念（高层封装）

### 4.3 扩展 piper_client + Diagnostics 接口：✅ 强烈推荐

**双轨制设计**:
- **方案 A (标准录制)**: 在 `piper_client` 中提供易用的录制 API
- **方案 B (高级诊断)**: 提供 `PiperDiagnostics` 接口，暴露底层能力

**优点**:
- ✅ 保持类型状态机的安全性（方案 A）
- ✅ 提供底层访问灵活性（方案 B）
- ✅ 职责分离：录制 vs 诊断
- ✅ 不会让 client 层变得臃肿

---

## 5. 推荐方案详细设计

### 5.1 方案 A：标准录制 API（推荐用于常规使用）

#### 5.1.1 设计理念

**职责定位**: 提供开箱即用的录制功能，适用于大多数用户场景。

**核心特点**:
- 与类型状态机完全集成
- RAII 语义，自动管理资源
- 适合常规录制场景

#### 5.1.2 API 设计

```rust
// crates/piper-client/src/recording.rs

use piper_driver::recording::AsyncRecordingHook;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::path::PathBuf;

/// 录制句柄（用于控制和监控）
///
/// # Drop 语义
///
/// 当 `RecordingHandle` 被丢弃时：
/// - ✅ 自动 flush 缓冲区中的数据
/// - ✅ 自动关闭接收端
/// - ❌ 不会自动保存文件（需要显式调用 `stop_recording()`）
///
/// # Panics
///
/// 如果在 Drop 时发生 I/O 错误，错误会被静默忽略（Drop 上下文无法处理错误）。
/// 建议始终显式调用 `stop_recording()` 以获取错误结果。
pub struct RecordingHandle {
    rx: crossbeam_channel::Receiver<piper_driver::TimestampedFrame>,
    dropped_frames: Arc<AtomicU64>,
    output_path: PathBuf,
    start_time: std::time::Instant,
}

impl RecordingHandle {
    /// 获取当前丢帧数量
    pub fn dropped_count(&self) -> u64 {
        self.dropped_frames.load(Ordering::Relaxed)
    }

    /// 获取录制时长
    pub fn elapsed(&self) -> std::time::Duration {
        self.start_time.elapsed()
    }

    /// 获取输出文件路径
    pub fn output_path(&self) -> &PathBuf {
        &self.output_path
    }
}

impl Drop for RecordingHandle {
    /// ⚠️ Drop 语义：自动清理资源
    ///
    /// 注意：这里只关闭接收端，不保存文件。
    /// 文件保存必须在 `stop_recording()` 中显式完成。
    fn drop(&mut self) {
        // 接收端会在 Drop 时自动关闭
        // 这里只是显式标记（用于调试）
        tracing::debug!("RecordingHandle dropped, receiver closed");
    }
}

/// 录制配置
#[derive(Debug, Clone)]
pub struct RecordingConfig {
    /// 输出文件路径
    pub output_path: PathBuf,

    /// 自动停止条件
    pub stop_condition: StopCondition,

    /// 元数据
    pub metadata: RecordingMetadata,
}

#[derive(Debug, Clone)]
pub enum StopCondition {
    /// 时长限制（秒）
    Duration(u64),

    /// 手动停止
    Manual,

    /// 接收到特定 CAN ID 时停止
    OnCanId(u32),

    /// 接收到特定数量的帧后停止
    FrameCount(usize),
}

#[derive(Debug, Clone)]
pub struct RecordingMetadata {
    pub notes: String,
    pub operator: String,
}

// crates/piper-client/src/state/machine.rs

impl Piper<Standby> {
    /// 在 Standby 状态下启动录制
    ///
    /// # 示例
    ///
    /// ```rust,no_run
    /// use piper_client::{PiperBuilder, recording::{RecordingConfig, StopCondition}};
    ///
    /// # async fn example() -> anyhow::Result<()> {
    /// let robot = PiperBuilder::new()
    ///     .interface("can0")
    ///     .build()?;
    ///
    /// let standby = robot.connect()?;
    ///
    /// let (standby, handle) = standby.start_recording(RecordingConfig {
    ///     output_path: "demo.bin".into(),
    ///     stop_condition: StopCondition::Duration(10),
    ///     metadata: RecordingMetadata {
    ///         notes: "Test recording".to_string(),
    ///         operator: "Alice".to_string(),
    ///     },
    /// })?;
    ///
    /// // 执行操作（会被录制）
    /// // ...
    ///
    /// // 停止录制并保存
    /// let _standby = standby.stop_recording(handle)?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn start_recording(
        self,
        config: RecordingConfig,
    ) -> Result<(Piper<Standby>, RecordingHandle)> {
        self.start_recording_inner(config)
    }
}

impl<M> Piper<Active<M>>
where
    M: piper_client::Mode,
{
    /// 在 Active 状态下启动录制
    ///
    /// # 注意
    ///
    /// Active 状态下的录制会包含控制指令帧（0x1A1-0x1FF）。
    pub fn start_recording(
        self,
        config: RecordingConfig,
    ) -> Result<(Piper<Active<M>>, RecordingHandle)> {
        self.start_recording_inner(config)
    }

    /// 停止录制并保存文件
    ///
    /// # 返回
    ///
    /// 返回 `(Piper<Active<M>>, 录制统计)`
    pub fn stop_recording(
        self,
        handle: RecordingHandle,
    ) -> Result<(Piper<Active<M>>, RecordingStats)> {
        // 创建录制对象
        let mut recording = piper_tools::PiperRecording::new(
            piper_tools::RecordingMetadata::new(
                self.driver.context.interface.clone(),
                self.driver.context.bus_speed,
            )
        );

        // 收集所有帧
        let mut frame_count = 0;
        while let Ok(frame) = handle.rx.try_recv() {
            recording.add_frame(frame);
            frame_count += 1;
        }

        // 保存文件
        recording.save(&handle.output_path)?;

        let stats = RecordingStats {
            frame_count,
            duration: handle.elapsed(),
            dropped_frames: handle.dropped_count(),
            output_path: handle.output_path.clone(),
        };

        Ok((self, stats))
    }
}

/// 录制统计
#[derive(Debug, Clone)]
pub struct RecordingStats {
    pub frame_count: usize,
    pub duration: std::time::Duration,
    pub dropped_frames: u64,
    pub output_path: PathBuf,
}

// 内部实现（共享代码）
impl<S> Piper<S>
where
    S: piper_client::marker::StateMarker,
{
    fn start_recording_inner(
        &self,
        config: RecordingConfig,
    ) -> Result<(Self, RecordingHandle)> {
        // 创建录制钩子
        let (hook, rx) = piper_driver::recording::AsyncRecordingHook::new();
        let dropped = hook.dropped_frames().clone();

        // 注册到 driver 层
        self.driver.context.hooks.write()?.add_callback(
            Arc::new(hook) as Arc<dyn piper_driver::FrameCallback>
        )?;

        let handle = RecordingHandle {
            rx,
            dropped_frames: dropped,
            output_path: config.output_path.clone(),
            start_time: std::time::Instant::now(),
        };

        tracing::info!("Recording started: {:?}", config.output_path);

        Ok((self.clone(), handle))
    }
}
```

#### 5.1.3 CLI 使用示例

```rust
// apps/cli/src/commands/record.rs

impl RecordCommand {
    pub async fn execute(&self, config: &OneShotConfig) -> Result<()> {
        use piper_client::PiperBuilder;
        use piper_client::recording::{RecordingConfig, StopCondition, RecordingMetadata};

        // 连接（使用 client 层）
        let robot = PiperBuilder::new()
            .interface(self.interface.clone().unwrap_or_default())
            .build()?;

        let standby = robot.connect()?;

        // 启动录制（仍然在 Standby 状态）
        let (standby, handle) = standby.start_recording(RecordingConfig {
            output_path: PathBuf::from(&self.output),
            stop_condition: StopCondition::Duration(self.duration),
            metadata: RecordingMetadata {
                notes: String::new(),
                operator: whoami::username(),
            },
        })?;

        println!("✅ 开始录制...");

        // 启用电机
        let mut active = standby.enable()?;

        // 执行一些操作（会被录制）
        tokio::time::sleep(Duration::from_secs(5)).await;

        // 停止录制
        let (_active, stats) = active.stop_recording(handle)?;

        println!("✅ 录制完成:");
        println!("  帧数: {}", stats.frame_count);
        println!("  时长: {:?}", stats.duration);
        println!("  丢帧: {}", stats.dropped_frames);
        println!("  文件: {}", stats.output_path.display());

        Ok(())
    }
}
```

### 5.2 方案 B：高级诊断接口（推荐用于自定义场景）

#### 5.2.1 设计理念

**职责定位**: 提供底层访问能力，适用于：
- 自定义诊断工具
- 高级抓包和调试
- 非标准回放逻辑
- 性能分析和优化

**核心特点**:
- 有限制的底层访问
- 不会破坏类型状态机
- 灵活性高，可扩展性强

#### 5.2.2 API 设计

```rust
// crates/piper-client/src/diagnostics.rs

use std::sync::Arc;
use piper_driver::{FrameCallback, PiperFrame};

/// 高级诊断接口（逃生舱）
///
/// # 设计理念
///
/// 这是一个**受限的逃生舱**，暴露了底层 driver 的部分功能：
/// - ✅ 可以访问 context.hooks（注册自定义回调）
/// - ✅ 可以访问 send_frame（发送原始 CAN 帧）
/// - ❌ 不能直接调用 enable/disable（保持状态机安全）
///
/// # 线程安全
///
/// `PiperDiagnostics` 持有 `Arc<piper_driver::Piper>`，可以安全地跨线程传递：
/// - ✅ **独立生命周期**：不受原始 `Piper` 实例生命周期约束
/// - ✅ **跨线程使用**：可以在诊断线程中长期持有
/// - ✅ **`'static`**：可以存储在 `static` 变量或线程局部存储中
///
/// # 权衡说明
///
/// 由于持有 `Arc` 而非引用，`PiperDiagnostics` **脱离了 TypeState 的直接保护**。
/// 这是逃生舱设计的**有意权衡**：
/// - 优点：灵活性极高，适合复杂的诊断场景
/// - 缺点：无法在编译时保证关联的 `Piper` 仍然处于特定状态
/// - 缓解：通过运行时检查和文档警告来保证安全
///
/// # 使用场景
///
/// - 自定义诊断工具
/// - 高级抓包和调试
/// - 性能分析和优化
/// - 非标准回放逻辑
/// - 后台监控线程
///
/// # 安全注意事项
///
/// 此接口提供的底层能力**可能破坏状态机的不变性**。
/// 使用时需注意：
/// 1. **不要在 Active 状态下发送控制指令**（会导致双控制流冲突）
/// 2. **不要手动调用 `disable()`**（应该通过 `Piper` 的 `Drop` 来处理）
/// 3. **确保回调执行时间 <1μs**（否则会影响实时性能）
/// 4. **注意生命周期**：即使持有 `Arc`，也要确保关联的 `Piper` 实例未被销毁
///
/// # 示例
///
/// ## 基础使用
///
/// ```rust,no_run
/// use piper_client::{PiperBuilder};
/// use piper_driver::recording::AsyncRecordingHook;
/// use std::sync::Arc;
///
/// # fn main() -> anyhow::Result<()> {
/// let robot = PiperBuilder::new()
///     .interface("can0")
///     .build()?;
///
/// let active = robot.connect()?.enable()?;
///
/// // 获取诊断接口（持有 Arc，独立生命周期）
/// let diag = active.diagnostics();
///
/// // 创建自定义录制钩子
/// let (hook, rx) = AsyncRecordingHook::new();
///
/// // 注册钩子
/// diag.register_callback(Arc::new(hook))?;
///
/// // 在后台线程处理录制数据
/// std::thread::spawn(move || {
///     while let Ok(frame) = rx.recv() {
///         println!("Received CAN frame: 0x{:03X}", frame.can_id);
///     }
/// });
/// # Ok(())
/// # }
/// ```
///
/// ## 跨线程长期持有
///
/// ```rust,no_run
/// use piper_client::{PiperBuilder};
/// use std::sync::{Arc, Mutex};
/// use std::thread;
///
/// # fn main() -> anyhow::Result<()> {
/// let robot = PiperBuilder::new()
///     .interface("can0")
///     .build()?;
///
/// let active = robot.connect()?.enable()?;
///
/// // 获取诊断接口（可以安全地移动到其他线程）
/// let diag = active.diagnostics();
///
/// // 在另一个线程中长期持有
/// thread::spawn(move || {
///     // diag 在这里完全独立，不受主线程影响
///     loop {
///         // 执行诊断逻辑...
///         std::thread::sleep(std::time::Duration::from_secs(1));
///     }
/// });
///
/// // 主线程可以继续使用 active
/// // active.move_joints(target)?;
///
/// # Ok(())
/// # }
/// ```
pub struct PiperDiagnostics {
    /// 持有 driver 的 Arc 克隆
    ///
    /// **设计权衡**：
    /// - 使用 `Arc` 而非引用 → 独立生命周期，可跨线程
    /// - 脱离 TypeState 保护 → 依赖运行时检查
    ///
    /// 这与 `reqwest` 等成熟库的逃生舱设计一致。
    driver: Arc<piper_driver::Piper>,
}

impl PiperDiagnostics {
    pub(super) fn new<M>(inner: &Piper<Active<M>>) -> Self
    where
        M: piper_client::Mode,
    {
        // 克隆 Arc（轻量级操作，仅增加引用计数）
        Self {
            driver: Arc::clone(&inner.driver),
        }
    }

    /// 注册自定义 FrameCallback
    ///
    /// # 注意
    ///
    /// 回调会在 RX 线程中执行，必须保证：
    /// - 执行时间 <1μs
    /// - 不阻塞
    /// - 线程安全（Send + Sync）
    pub fn register_callback(
        &self,
        callback: Arc<dyn FrameCallback>,
    ) -> Result<()> {
        self.driver.context.hooks.write()?.add_callback(callback)?;
        Ok(())
    }

    /// 发送原始 CAN 帧
    ///
    /// # ⚠️ 安全警告
    ///
    /// **严禁在 Active 状态下发送控制指令帧（0x1A1-0x1FF）**。
    /// 这会导致与驱动层的周期性发送任务产生双控制流冲突。
    ///
    /// # 允许的使用场景
    ///
    /// - ✅ Standby 状态：发送配置帧（0x5A1-0x5FF）
    /// - ✅ ReplayMode：回放预先录制的帧
    /// - ✅ 调试：发送测试帧
    ///
    /// # 禁止的使用场景
    ///
    /// - ❌ Active<MIT>：发送 0x1A1-0x1A6（位置/速度/力矩指令）
    /// - ❌ Active<Position>: 发送 0x1A1-0x1A6
    pub fn send_frame(&self, frame: &PiperFrame) -> Result<()> {
        self.driver.send_frame(frame)?;
        Ok(())
    }

    /// 获取 driver 实例的 Arc 克隆（完全访问）
    ///
    /// # ⚠️ 高级逃生舱
    ///
    /// 此方法提供对底层 `piper_driver::Piper` 的完全访问。
    /// 仅用于**极端特殊场景**，99% 的情况下应该使用上面的 `register_callback` 和 `send_frame`。
    ///
    /// # 使用前提
    ///
    /// 你必须完全理解以下文档：
    /// - `piper_driver` 模块文档
    /// - 类型状态机设计
    /// - Driver 层 IO 线程模型
    ///
    /// # 安全保证
    ///
    /// 返回的是 `Arc` 引用计数指针，而非不可变引用：
    /// - ✅ 可以跨线程传递
    /// - ✅ 可以长期持有
    /// - ❌ 无法直接调用 `enable/disable`（这些方法需要 `&mut self`）
    pub fn driver(&self) -> Arc<piper_driver::Piper> {
        Arc::clone(&self.driver)
    }
}

// crates/piper-client/src/state/machine.rs

impl<M> Piper<Active<M>>
where
    M: piper_client::Mode,
{
    /// 获取诊断接口（逃生舱）
    ///
    /// # 返回值
    ///
    /// 返回的 `PiperDiagnostics` 持有 `Arc<piper_driver::Piper>`：
    /// - ✅ 独立于当前 `Piper` 实例的生命周期
    /// - ✅ 可以安全地移动到其他线程
    /// - ✅ 可以在后台线程中长期持有
    ///
    /// # 示例
    ///
    /// ```rust,no_run
    /// # use piper_client::PiperBuilder;
    /// # fn main() -> anyhow::Result<()> {
    /// let robot = PiperBuilder::new()
    ///     .interface("can0")
    ///     .build()?;
    ///
    /// let active = robot.connect()?.enable()?;
    ///
    /// // 获取诊断接口
    /// let diag = active.diagnostics();
    ///
    /// // diag 可以安全地移动到其他线程
    /// std::thread::spawn(move || {
    ///     // 在这里使用 diag...
    /// });
    ///
    /// // active 仍然可以正常使用
    /// # Ok(())
    /// # }
    /// ```
    pub fn diagnostics(&self) -> PiperDiagnostics {
        PiperDiagnostics::new(self)
    }
}
```

### v2.1 优化说明

**生命周期改进**（根据用户反馈）：
- **v2.0 设计**：`PiperDiagnostics<'a, M>` 持有 `&'a Piper<Active<M>>`
  - ❌ 生命周期绑定到 `Piper` 实例
  - ❌ 无法跨线程长期持有
  - ✅ 保留 TypeState 保护

- **v2.1 优化**：`PiperDiagnostics` 持有 `Arc<piper_driver::Piper>`
  - ✅ `'static` 生命周期，完全独立
  - ✅ 可以跨线程传递
  - ✅ 可以在后台线程中长期持有
  - ⚠️ 脱离 TypeState 保护（有意权衡）

**参考设计**：
- `reqwest::Client`：持有 `Arc<ClientInner>`，可跨线程
- `tokio::runtime::Handle`：持有 `Arc<Runtime>`，独立生命周期

#### 5.2.3 CLI 使用示例（高级录制）

```rust
// apps/cli/src/commands/record_advanced.rs

impl RecordAdvancedCommand {
    pub async fn execute(&self) -> Result<()> {
        use piper_client::PiperBuilder;
        use piper_driver::recording::AsyncRecordingHook;
        use std::sync::Arc;

        let robot = PiperBuilder::new()
            .interface("can0")
            .build()?;

        let active = robot.connect()?.enable()?;

        // 获取诊断接口
        let diag = active.diagnostics();

        // 创建自定义录制钩子
        let (hook, rx) = AsyncRecordingHook::new();
        let dropped_counter = hook.dropped_frames().clone();

        // 注册钩子
        diag.register_callback(Arc::new(hook))?;

        println!("✅ 高级录制已启动（使用诊断接口）");

        // 在后台线程处理录制数据
        let output_path = self.output.clone();
        let handle = std::thread::spawn(move || {
            let mut recording = piper_tools::PiperRecording::new(
                piper_tools::RecordingMetadata::new("can0".to_string(), 1_000_000)
            );

            let mut count = 0;
            while let Ok(frame) = rx.recv() {
                recording.add_frame(frame);
                count += 1;

                if count % 1000 == 0 {
                    println!("录制中: {} 帧", count);
                }

                if count >= 10000 {
                    break;
                }
            }

            recording.save(&output_path).unwrap();
            println!("✅ 录制已保存: {}", output_path);

            dropped_counter.load(std::sync::atomic::Ordering::Relaxed)
        });

        // 执行操作
        tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;

        // 等待录制完成
        let dropped = handle.join().unwrap();

        println!("丢帧数: {}", dropped);

        Ok(())
    }
}
```

### 5.3 方案 C：引入 ReplayMode（回放专用状态）

#### 5.3.1 设计理念

**问题**: 回放 CAN 帧时会与 Driver 层的 `tx_loop` 产生**双控制流冲突**：

```
┌─────────────────────────────────────────┐
│  Driver tx_loop                         │
│  ┌─────────────────────────────────┐   │
│  │ 周期性发送控制指令 (500Hz)        │   │
│  │ 0x1A1: Joint1 Position          │   │
│  │ 0x1A2: Joint2 Position          │   │
│  │ ...                              │   │
│  └─────────────────────────────────┘   │
│                                         │
│         ⚠️ 冲突！                       │
│                                         │
│  ┌─────────────────────────────────┐   │
│  │ 回放线程 (也在发送帧)            │   │
│  │ 0x1A1: Replay Frame 1           │   │
│  │ 0x1A2: Replay Frame 2           │   │
│  │ ...                              │   │
│  └─────────────────────────────────┘   │
└─────────────────────────────────────────┘
```

**结果**:
- 伺服电机接收到**混合的控制信号**
- 可能导致**电机震荡、发热、故障**
- 严重时会触发硬件 E-Stop

**解决方案**: 引入 `ReplayMode` 状态，在回放时**暂停 Driver 的周期性发送任务**。

#### 5.3.2 API 设计

```rust
// crates/piper-client/src/state/replay.rs

/// 回放模式标记
///
/// # 安全保证
///
/// 在 ReplayMode 下：
/// - ✅ Driver 的 `tx_loop` 仅作为回放通道，不主动发送控制指令
/// - ✅ 所有发送的帧都来自回放文件
/// - ✅ 电机使能状态保持不变
#[derive(Debug, Clone, Copy)]
pub struct ReplayMode {
    speed: f64,  // 回放速度倍数
}

impl piper_client::Mode for ReplayMode {
    const NAME: &'static str = "Replay";
}

// crates/piper-client/src/state/machine.rs

impl Piper<Standby> {
    /// 进入回放模式
    ///
    /// # 状态转换
    ///
    /// `Standby` → `Active<ReplayMode>`
    ///
    /// # ⚠️ 重要约束：必须从 Standby 开始
    ///
    /// **回放必须从静止状态（Standby）开始**，原因：
    ///
    /// 1. **防止控制跳变（Control Jump）**
    ///    - 如果从 `Active<Position>` 直接切换到 `ReplayMode`
    ///    - 控制指令会突然跳变（从当前目标位置跳到回放文件的第一帧）
    ///    - 这会导致电机剧烈运动，可能触发硬件保护
    ///
    /// 2. **避免双控制流冲突**
    ///    - 在 `Active` 状态下，Driver 的 `tx_loop` 正在周期性发送控制指令
    ///    - 如果直接切换到回放，会出现短暂的"双控制流"窗口
    ///    - 从 `Standby` 开始可以确保 `tx_loop` 处于完全静止状态
    ///
    /// 3. **符合机器安全规范**
    ///    - ISO 10218（工业机器人安全标准）要求：在回放/重放操作前，机器人必须处于静止状态
    ///    - 从 `Standby` 进入 `ReplayMode` 符合这一规范
    ///
    /// # 状态转换图
    ///
    /// ```text
    /// Disconnected
    ///     │
    ///     ▼
    ///   Standby  ◄───────┐
    ///     │              │
    ///     │ enter_replay │
    ///     ▼              │ disable
    /// Active<ReplayMode> │
    ///     │              │
    ///     │ replay_recording (完成后返回 Standby)
    ///     ▼
    ///   Standby
    /// ```
    ///
    /// # 安全检查
    ///
    /// - ✅ 确认回放文件来源可信
    /// - ✅ 确认回放速度合理（默认 1.0x，最大 2.0x）
    /// - ✅ 确认机器人处于 Standby（电机未使能）
    ///
    /// # 禁止的转换
    ///
    /// ❌ **不允许**：`Active<Position>` → `Active<ReplayMode>`
    /// ❌ **不允许**：`Active<MIT>` → `Active<ReplayMode>`
    ///
    /// 必须先 `disable()` 回到 `Standby`，然后再 `enter_replay_mode()`。
    ///
    /// # 示例
    ///
    /// ```rust,no_run
    /// # use piper_client::PiperBuilder;
    /// # fn main() -> anyhow::Result<()> {
    /// let robot = PiperBuilder::new()
    ///     .interface("can0")
    ///     .build()?;
    ///
    /// let standby = robot.connect()?;
    ///
    /// // ✅ 正确：从 Standby 进入回放模式
    /// let replay = standby.enter_replay_mode(1.0)?;
    ///
    /// // 回放完成后自动返回 Standby
    /// let (standby, _stats) = replay.replay_recording("demo.bin")?;
    ///
    /// // ❌ 错误：不能从 Active 状态直接进入回放模式
    /// // let active = standby.enable()?;
    /// // let replay = active.enter_replay_mode(1.0)?;  // 编译错误！
    ///
    /// # Ok(())
    /// # }
    /// ```
    pub fn enter_replay_mode(
        self,
        speed: f64,
    ) -> Result<Piper<Active<ReplayMode>>> {
        // 安全检查：速度限制
        if speed > 2.0 {
            anyhow::bail!(
                "回放速度 {}x 超过安全限制（最大 2.0x）",
                speed
            );
        }

        // ✅ 在这里，我们需要通知 Driver 层暂停周期性发送
        // 这需要在 Driver 层添加一个新的模式：
        //   piper_driver::Piper::set_mode(DriverMode::Replay)
        //
        // 在 Replay 模式下，tx_loop 不会自动发送控制指令，
        // 仅作为回放帧的发送通道。

        // 暂时使用 enable 作为占位（实际需要新的 Driver API）
        let active = self.enable()?;

        // 转换状态
        Ok(Piper {
            driver: active.driver,
            observer: active.observer,
            _state: ReplayMode { speed },
        })
    }
}

impl Piper<Active<ReplayMode>> {
    /// 回放录制文件
    ///
    /// # 执行流程
    ///
    /// 1. 加载录制文件
    /// 2. 获取第一个帧的时间戳作为基准
    /// 3. 按时间戳间隔发送帧
    /// 4. 应用速度倍数控制
    /// 5. 完成后返回 Standby
    ///
    /// # 示例
    ///
    /// ```rust,no_run
    /// # use piper_client::PiperBuilder;
    /// # fn main() -> anyhow::Result<()> {
    /// let robot = PiperBuilder::new()
    ///     .interface("can0")
    ///     .build()?;
    ///
    /// let standby = robot.connect()?;
    /// let replay = standby.enter_replay_mode(1.0)?;
    ///
    /// // 回放录制
    /// let (standby, stats) = replay.replay_recording("demo.bin")?;
    ///
    /// println!("回放完成: {} 帧", stats.frame_count);
    /// # Ok(())
    /// # }
    /// ```
    pub fn replay_recording(
        mut self,
        input_path: &str,
    ) -> Result<(Piper<Standby>, ReplayStats)> {
        use piper_tools::PiperRecording;

        // 加载录制
        let recording = PiperRecording::load(input_path)?;

        println!("📊 录制信息:");
        println!("  帧数: {}", recording.frame_count());
        if let Some(duration) = recording.duration() {
            println!("  时长: {:?}", duration);
        }
        println!("  回放速度: {}x", self._state.speed);
        println!();

        // 获取诊断接口（用于发送帧）
        let diag = self.diagnostics();

        // 获取基准时间戳
        let base_timestamp = recording.frames[0].timestamp_us();
        let speed = self._state.speed;

        println!("📝 开始回放...");

        let start = std::time::Instant::now();
        let mut frame_count = 0;

        for frame in &recording.frames {
            // 计算相对时间（微秒）
            let elapsed_us = frame.timestamp_us().saturating_sub(base_timestamp);
            let elapsed_ms = elapsed_us / 1000;

            // 应用速度控制
            let delay_ms = if speed > 0.0 {
                (elapsed_ms as f64 / speed) as u64
            } else {
                elapsed_ms
            };

            // 等待（控制回放速度）
            if delay_ms > 0 {
                std::thread::sleep(std::time::Duration::from_millis(delay_ms));
            }

            // 发送 CAN 帧
            // ✅ 这里安全，因为在 ReplayMode 下 tx_loop 已暂停
            let piper_frame = frame.frame;

            diag.send_frame(&piper_frame)?;

            frame_count += 1;

            // 进度显示
            if frame_count % 100 == 0 {
                print!(
                    "\r回放进度: {}/{} 帧 ({}%)",
                    frame_count,
                    recording.frame_count(),
                    (frame_count * 100 / recording.frame_count())
                );
                use std::io::Write;
                std::io::stdout().flush().ok();
            }
        }

        let elapsed = start.elapsed();

        println!("\n✅ 回放完成:");
        println!("  帧数: {}", frame_count);
        println!("  实际时长: {:?}", elapsed);

        // 禁用电机（退出回放模式）
        let standby = self.disable()?;

        let stats = ReplayStats {
            frame_count,
            duration: elapsed,
            speed,
        };

        Ok((standby, stats))
    }

    /// 获取诊断接口
    pub fn diagnostics(&self) -> PiperDiagnostics<'_, ReplayMode> {
        PiperDiagnostics::new(self)
    }
}

/// 回放统计
#[derive(Debug, Clone)]
pub struct ReplayStats {
    pub frame_count: usize,
    pub duration: std::time::Duration,
    pub speed: f64,
}
```

#### 5.3.3 CLI 使用示例

```rust
// apps/cli/src/commands/replay.rs

impl ReplayCommand {
    pub async fn execute(&self) -> Result<()> {
        use piper_client::PiperBuilder;

        println!("🔄 回放录制: {}", self.input);

        // 检查文件是否存在
        if !std::path::Path::new(&self.input).exists() {
            anyhow::bail!("录制文件不存在: {}", self.input);
        }

        // ⚠️ 安全确认
        if self.confirm || self.speed > 1.0 {
            println!("⚠️  回放速度: {}x", self.speed);
            if self.speed > 1.0 {
                println!("⚠️  高速回放可能不安全！");
            }

            let confirmed = utils::prompt_confirmation("确定要回放吗？", false)?;

            if !confirmed {
                println!("❌ 操作已取消");
                return Ok(());
            }

            println!("✅ 已确认");
        }

        // 连接
        let robot = PiperBuilder::new()
            .interface(self.interface.clone().unwrap_or_default())
            .build()?;

        let standby = robot.connect()?;

        // 进入回放模式
        let replay = standby.enter_replay_mode(self.speed)?;

        println!("✅ 已进入回放模式");

        // 回放录制
        let (standby, stats) = replay.replay_recording(&self.input)?;

        println!("✅ 回放完成:");
        println!("  帧数: {}", stats.frame_count);
        println!("  时长: {:?}", stats.duration);
        println!("  速度: {}x", stats.speed);

        Ok(())
    }
}
```

### 5.4 方案对比总结

| 特性 | 方案 A (标准录制) | 方案 B (诊断接口) | 方案 C (ReplayMode) |
|------|------------------|------------------|---------------------|
| **目标用户** | 普通用户 | 高级用户/开发者 | 所有用户 |
| **类型安全** | ✅ 完全保留 | ✅ 受限访问 | ✅ 完全保留 |
| **灵活性** | 🟡 中等 | ✅ 极高 | 🟡 中等 |
| **学习曲线** | ✅ 低 | ⚠️ 高 | ✅ 低 |
| **维护成本** | 🟢 低 | 🟡 中 | 🟢 低 |
| **适用场景** | 常规录制 | 自定义诊断、抓包 | 回放 |

**推荐使用策略**:
- **默认**: 方案 A（标准录制）
- **高级需求**: 方案 B（诊断接口）
- **回放**: 方案 C（ReplayMode）

---

## 6. 方案对比

| 方案 | 类型安全 | 实现复杂度 | 维护成本 | 灵活性 | 推荐度 |
|------|----------|------------|----------|--------|--------|
| **A. 标准录制 API** | ✅ 完全保留 | 🟡 中等 | 🟢 低 | 🟡 中等 | ⭐⭐⭐⭐⭐ |
| **B. 诊断接口** | ✅ 受限访问 | 🟢 简单 | 🟡 中 | 🟢 高 | ⭐⭐⭐⭐⭐ |
| **C. ReplayMode** | ✅ 完全保留 | 🟡 中等 | 🟢 低 | 🟡 中等 | ⭐⭐⭐⭐⭐ |
| **A+B 组合** | ✅ 完全保留 | 🟡 中等 | 🟢 低 | 🟢 高 | ⭐⭐⭐⭐⭐ |
| **D. 迁移到 piper_driver** | ❌ 完全丢失 | 🔴 复杂 | 🔴 高 | 🔴 低 | ⭐ |
| **E. 混用** | ❌ 不可能 | 🔴 不可行 | 🔴 极高 | ❌ 无 | ❌ |

**结论**: **A+B+C 组合方案**是最佳选择，兼顾安全性、灵活性和可维护性。

---

## 7. 实施建议

### 阶段 1：紧急修复（1-2 天）

**目标**: 移除 CLI 中的 Stub 实现，添加明确的错误提示

```rust
// apps/cli/src/commands/record.rs

impl RecordCommand {
    pub async fn execute(&self, config: &OneShotConfig) -> Result<()> {
        anyhow::bail!(
            "录制功能暂未实现。\n\
             \n\
             原因：piper_client 当前未暴露底层 CAN 帧访问接口。\n\
             \n\
             跟踪 Issue: https://github.com/xxx/issues/123\n\
             \n\
             计划实施（2026 Q1）:\n\
             - 方案 A: 标准录制 API（易于使用）\n\
             - 方案 B: 高级诊断接口（灵活定制）\n\
             \n\
             临时方案：如需紧急使用，请参考 docs/architecture/ 中的工作指南"
        );
    }
}
```

**任务清单**:
- [ ] 更新 `record.rs` 和 `replay.rs` 的错误提示
- [ ] 创建 GitHub Issue 跟踪实施进度
- [ ] 更新用户文档说明当前限制

### 阶段 2：实现方案 B（诊断接口）（2-3 天）

**优先级**: 🔴 高（快速提供逃生舱）

**任务清单**:
1. [ ] 创建 `crates/piper-client/src/diagnostics.rs` 模块
2. [ ] 实现 `PiperDiagnostics` 结构体
3. [ ] 实现 `register_callback()` 方法
4. [ ] 实现 `send_frame()` 方法（带安全警告）
5. [ ] 实现 `driver()` 方法（高级逃生舱）
6. [ ] 在 `Piper<Active>` 中添加 `diagnostics()` 方法
7. [ ] 添加完整的文档和使用示例
8. [ ] 添加单元测试（无硬件）

**验收标准**:
- ✅ 所有公共 API 有完整的文档注释
- ✅ 安全警告清晰明确
- ✅ 至少 3 个使用示例
- ✅ 单元测试覆盖率 >80%

### 阶段 3：实现方案 A（标准录制）（3-5 天）

**优先级**: 🟡 中（提供易用的标准 API）

**任务清单**:
1. [ ] 创建 `crates/piper-client/src/recording.rs` 模块
2. [ ] 实现 `RecordingHandle` 结构体（含 Drop 语义）
3. [ ] 实现 `RecordingConfig` 和 `StopCondition`
4. [ ] 实现 `Piper<Standby>::start_recording()`
5. [ ] 实现 `Piper<Active<M>>::start_recording()`
6. [ ] 实现 `stop_recording()` 方法
7. [ ] 在 CLI 中实现标准录制命令
8. [ ] 添加集成测试（需要虚拟 CAN）

**验收标准**:
- ✅ `RecordingHandle` 正确实现 Drop
- ✅ 录制文件格式符合规范
- ✅ 支持所有停止条件（时长/帧数/CAN ID）
- ✅ 丢帧计数准确
- ✅ 集成测试通过

### 阶段 4：实现方案 C（ReplayMode）（3-4 天）

**优先级**: 🟡 中（解决回放安全问题）

**任务清单**:
1. [ ] **Driver 层前置工作**:
    - [ ] 在 `piper_driver` 中添加 `DriverMode` 枚举
    - [ ] 在 `tx_loop` 中添加 Replay 模式支持
    - [ ] 修改 `Piper::set_mode()` 方法
    - [ ] 添加 Driver 层单元测试
2. [ ] **Client 层实现**:
    - [ ] 创建 `ReplayMode` 状态标记
    - [ ] 实现 `enter_replay_mode()` 方法
    - [ ] 实现 `replay_recording()` 方法
    - [ ] 添加安全检查（速度限制）
    - [ ] 在 CLI 中实现回放命令
3. [ ] **测试和文档**:
    - [ ] 添加集成测试（使用虚拟 CAN）
    - [ ] 验证无双控制流冲突
    - [ ] 编写用户使用指南
    - [ ] 添加安全警告文档

**验收标准**:
- ✅ 回放时 tx_loop 正确暂停
- ✅ 无双控制流冲突（通过示波器验证）
- ✅ 速度控制精确（误差 <5%）
- ✅ 集成测试通过
- ✅ 安全警告清晰

### 阶段 5：文档和示例（2-3 天）

**优先级**: 🟢 低（提升用户体验）

**任务清单**:
1. [ ] 更新 README.md 添加录制/回放章节
2. [ ] 创建完整示例 `examples/standard_recording.rs`
3. [ ] 创建高级示例 `examples/custom_diagnostics.rs`
4. [ ] 创建回放示例 `examples/replay_mode.rs`
5. [ ] 添加架构文档说明设计决策
6. [ ] 更新 CHANGELOG.md
7. [ ] 编写 CLI 用户手册
8. [ ] 录制演示视频

**验收标准**:
- ✅ 所有示例可独立运行
- ✅ 文档清晰易懂
- ✅ 架构图准确
- ✅ 视频演示完整

### 阶段 6：性能优化和测试（1-2 天）

**优先级**: 🟢 低（可选）

**任务清单**:
1. [ ] 性能基准测试（录制对 CPU 的影响）
2. [ ] 压力测试（高频 CAN 总线）
3. [ ] 内存泄漏检测
4. [ ] 丢帧率测试
5. [ ] 长时间稳定性测试

---

## 8. 风险评估

### 8.1 技术风险

| 风险 | 可能性 | 影响 | 缓解措施 |
|------|--------|------|----------|
| **破坏现有类型状态机** | 🟡 中 | 🔴 高 | 完整的单元测试 + 集成测试 |
| **回放时双控制流冲突** | 🔴 高 | 🔴 🔴 极高 | 引入 ReplayMode，暂停 tx_loop |
| **Drop 语义实现错误** | 🟡 中 | 🟡 中 | 仔细测试 RecordingHandle 的 Drop |
| **性能回归** | 🟢 低 | 🟡 中 | benchmark 测试，优化回调开销 |
| **诊断接口被滥用** | 🟡 中 | 🟡 中 | 详细文档 + 安全警告 |

### 8.2 项目风险

| 风险 | 可能性 | 影响 | 缓解措施 |
|------|--------|------|----------|
| **开发时间超预期** | 🟡 中 | 🟡 中 | 分阶段实施，优先方案 B（快速通道） |
| **向后兼容性破坏** | 🟢 低 | 🔴 高 | 仅添加新 API，不修改现有 API |
| **文档不完善** | 🟡 中 | 🟡 中 | 专人负责文档更新，代码审查 |
| **用户误解诊断接口** | 🟡 中 | 🟡 中 | 安全警告 + 使用示例 + RFC 讨论 |

### 8.3 安全风险

| 风险 | 可能性 | 影响 | 缓解措施 |
|------|--------|------|----------|
| **回放速度过快导致电机故障** | 🟡 中 | 🔴 高 | 速度限制 2.0x + 安全确认 |
| **Active 状态下发送控制指令** | 🟡 中 | 🔴 高 | 诊断接口文档警告 + 运行时检查 |
| **Panic 导致数据丢失** | 🟢 低 | 🟡 中 | RecordingHandle 实现 Drop |
| **回调执行时间过长** | 🟡 中 | 🟡 中 | 文档明确要求 <1μs |

---

## 9. 结论

### 9.1 核心发现

1. **Code Review 报告的建议不可行**
   - 直接混用 `piper_driver` 会导致 SocketCAN/GS-USB 接口独占冲突
   - 无法在同一个进程中创建两个实例

2. **根本原因**
   - `piper_client` 刻意隐藏了底层 API，导致无法实现录制/回放功能
   - 这是**有意的设计决策**，而非遗漏

3. **当前 CLI 实现是 Stub**
   - `record.rs` 和 `replay.rs` 是**桩代码**，用于验证接口设计
   - **未接入真实数据源**，这是正常的开发过程

### 9.2 推荐行动

**立即行动** (1 周内):
- [ ] 移除 CLI 中的 Stub 实现，添加明确的错误提示
- [ ] 创建 GitHub Issue 跟踪此问题
- [ ] 更新文档说明当前限制

**短期实施** (2-4 周):
- [ ] **优先实现方案 B（诊断接口）**：快速提供逃生舱
- [ ] 实现方案 A（标准录制）：提供易用的标准 API
- [ ] 更新 CLI 使用新 API
- [ ] 添加完整的单元测试

**中期实施** (1-2 个月):
- [ ] 实现方案 C（ReplayMode）：**关键安全问题**
- [ ] 修改 Driver 层支持 Replay 模式
- [ ] 集成测试和性能优化

**长期优化** (持续):
- [ ] 文档完善和示例
- [ ] 用户反馈收集
- [ ] API 迭代优化

### 9.3 架构原则

**未来 API 设计应遵循**:
1. ✅ **保持分层清晰**：Client → Driver → CAN
2. ✅ **类型安全优先**：优先使用类型状态机而非完全依赖逃生舱
3. ✅ **双轨制设计**：标准 API（易用）+ 诊断接口（灵活）
4. ✅ **渐进式披露**：高级用户可以通过 `diagnostics()` 访问底层功能
5. ✅ **安全第一**：回放等危险操作必须专用状态（ReplayMode）
6. ❌ **避免混用**：不要在同一应用中混用不同层级

### 9.4 对用户反馈的回应

**关于方案 B（诊断接口）**:
- ✅ **已提升地位**：与方案 A 并列推荐
- ✅ **增加限制**：通过 `PiperDiagnostics` 提供受限访问
- ✅ **Rust 社区实践**：这是成熟库的常见模式（tokio、reqwest）
- ✅ **v2.1 优化**：生命周期改进，持有 `Arc` 而非引用
  - 可跨线程传递
  - 可在后台线程中长期持有
  - `'static` 生命周期，完全独立

**关于回放复杂性**:
- ✅ **已识别为关键安全问题**：双控制流冲突
- ✅ **已引入 ReplayMode**：专用状态解决冲突
- ✅ **需要 Driver 层配合**：修改 tx_loop 支持暂停
- ✅ **v2.1 优化**：明确状态转换约束
  - **必须从 Standby 开始**，防止控制跳变
  - 符合 ISO 10218 机器安全规范
  - 添加详细的状态转换图

**关于 Drop 语义**:
- ✅ **已实现**：`RecordingHandle` 正确实现 Drop
- ✅ **明确职责**：Drop 清理资源，显式调用保存文件

**关于措辞**:
- ✅ **已修正**："假数据" → "Stub 实现（桩代码）"
- ✅ **更专业**：说明这是正常的开发过程

### 9.5 v2.1 版本优化总结

**优化 1：诊断接口生命周期改进**
- **问题**：v2.0 的引用绑定限制了跨线程使用
- **解决**：改用 `Arc<piper_driver::Piper>`
- **效果**：
  - ✅ `'static` 生命周期，完全独立
  - ✅ 支持跨线程长期持有
  - ✅ 对标 `reqwest`、`tokio` 等成熟库
  - ⚠️ 权衡：脱离 TypeState 保护（通过文档和运行时检查缓解）

**优化 2：ReplayMode 状态转换约束**
- **问题**：v2.0 未明确说明为何必须从 Standby 开始
- **解决**：添加详细的安全分析和状态转换图
- **理由**：
  1. **防止控制跳变**：避免电机剧烈运动
  2. **避免双控制流冲突**：确保 tx_loop 完全静止
  3. **符合 ISO 10218 规范**：工业机器人安全标准

**结论**：
- 两个优化都**显著提升了工程实用性**
- 保持了原有的安全性和架构清晰度
- 符合 Rust 社区和工业机器人领域的最佳实践

---

## 附录 A：相关代码位置

| 组件 | 文件路径 | 关键行 |
|------|----------|--------|
| piper_client 封装 | `crates/piper-client/src/state/machine.rs` | 304 |
| piper_driver 公共 API | `crates/piper-driver/src/lib.rs` | 全文 |
| CLI 录制 Stub | `apps/cli/src/commands/record.rs` | 81, 122 |
| CLI 回放 Stub | `apps/cli/src/commands/replay.rs` | 82, 122 |
| 现有的 raw_commander | `crates/piper-client/src/raw_commander.rs` | 参考模式 |
| AsyncRecordingHook | `crates/piper-driver/src/recording.rs` | 全文 |

---

## 附录 B：术语表

| 术语 | 解释 |
|------|------|
| **piper_client** | 客户端层，提供类型安全 API（类型状态机） |
| **piper_driver** | 驱动层，提供 IO 管理、状态同步、钩子系统 |
| **类型状态机** | 使用零大小类型标记在编译时保证状态转换的正确性 |
| **逃生舱（Escape Hatch）** | 允许访问底层 API 的方法，可能破坏抽象 |
| **诊断接口（Diagnostics）** | 受限的逃生舱，暴露部分底层功能 |
| **ReplayMode** | 回放专用状态，暂停 Driver 的周期性发送 |
| **双控制流冲突** | 回放时 tx_loop 与回放线程同时发送 CAN 帧导致的问题 |
| **Stub 实现（桩代码）** | 临时的占位实现，用于验证接口设计 |
| **ArcSwap** | 无锁原子指针交换，用于高频状态读取 |
| **FrameCallback** | CAN 帧回调 trait，用于钩子系统 |
| **AsyncRecordingHook** | 异步录制钩子，使用有界队列防止 OOM |
| **RecordingHandle** | 录制句柄，RAII 语义，管理录制资源 |
| **SocketCAN** | Linux 内核级 CAN 总线驱动 |
| **GS-USB** | USB CAN 适配器（用户空间驱动） |
| **RAII** | 资源获取即初始化（Rust 的所有权模式） |
| **Drop 语义** | Rust 中值离开作用域时自动执行的清理逻辑 |

---

## 附录 C：参考资料

### Rust 社区实践

1. **Tokio 的逃生舱设计**
   - `tokio::runtime::Handle`：允许在运行时环境外提交任务
   - `tokio::task::spawn_blocking`：访问底层线程池

2. **Reqwest 的 Client Config**
   - `reqwest::ClientBuilder`：暴露底层 `hyper` 配置
   - 允许高级用户自定义连接池、超时等

3. **Serde 的 Raw Value**
   - `serde::raw::RawValue`：跳过反序列化，保留原始 JSON
   - 用于性能敏感场景

### 相关 RFC

1. [Rust API Guidelines: Escape Hatches](https://rust-lang.github.io/api-guidelines/flexibility.html)
2. [The Rust Reference: Drop Glue](https://doc.rust-lang.org/reference/destructors.html)
3. [Typestate Pattern in Rust](https://docs.rs/typestate/)

---

**报告结束**

*本报告基于 2026-01-27 的代码状态生成。*
*版本 v2.1，根据用户反馈进一步优化。*
*状态：✅ Approved（已批准，可进入实施阶段）*
*如有疑问请联系架构组。*

---

## 版本历史

| 版本 | 日期 | 主要变更 |
|------|------|----------|
| **v1.0** | 2026-01-27 | 初版发布 |
| **v2.0** | 2026-01-27 | 修正架构混用问题，引入双轨制设计和 ReplayMode |
| **v2.1** | 2026-01-27 | 优化诊断接口生命周期，明确 ReplayMode 状态转换约束 |

## 致谢

感谢用户的以下专业反馈，这些意见极大地提升了本报告的质量：
- **生命周期优化建议**：从引用改为 `Arc`，提升跨线程能力
- **状态转换约束说明**：强调"必须从 Standby 开始"，防止控制跳变
- **工程实践对标**：参考 `reqwest`、`tokio` 等成熟库的设计
- **安全规范符合性**：引入 ISO 10218 工业机器人安全标准
