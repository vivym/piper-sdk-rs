# Piper SDK - CAN 帧录制架构限制深度分析报告（修正版）

> **⚠️ 已被 v1.2 取代**
>
> **本文档（v1.1）已被 v1.2 版本取代。**
>
> **v1.2 新增关键工程安全修正**:
> - 🛡️ **内存安全**: 使用 `bounded(10000)` 代替 `unbounded()` 防止 OOM
> - 🏗️ **架构优化**: Hooks 从 `PipelineConfig` 移至 `PiperContext`
> - ⏱️ **时间戳精度**: 强制使用硬件时间戳 `frame.timestamp_us`
> - 🔒 **TX 安全**: 仅在 `send()` 成功后记录 TX 帧
> - 🌐 **平台依赖**: 明确方案 D 依赖 SocketCAN Loopback
>
> **请阅读最新版本**: [`can-recording-analysis-v1.2.md`](./can-recording-analysis-v1.2.md)
> **执行摘要**: [`EXECUTIVE_SUMMARY.md`](./EXECUTIVE_SUMMARY.md) (已更新至 v1.2)
>
> ---
>
> **以下为 v1.1 原文（仅供参考）**
>
> ---

**日期**: 2026-01-27
**版本**: v1.1（已被 v1.2 取代）
**状态**: ⚠️ 已被 v1.2 取代

---

## 执行摘要

本报告深入分析了 Piper SDK 中 CAN 帧录制功能面临的架构限制，识别了根本原因，提出了多种解决方案，并给出了推荐方案和实施路径。

**关键修正**（v1.1）:
- ⚠️ **性能关键**: 修正方案 A 使用 Channel 代替 Mutex，避免热路径阻塞
- ⚠️ **数据真实性**: 明确方案 E 为"逻辑重放"而非"CAN 录制"
- ⚠️ **完整性**: 补充 TX 路径录制，保留完整上下文
- ⚠️ **平台兼容**: 修正 GS-USB 旁路监听的可行性

**关键发现**:
- ✅ 问题可解决，但需要架构改进
- ✅ 最佳方案：在 driver 层添加**异步录制钩子**（Channel 模式）
- ✅ 实施复杂度：中等
- ✅ 预计工作量：2-3 天

**⚠️ v1.1 遗留问题**（已在 v1.2 修正）:
- ❌ 使用 `unbounded()` 可能导致 OOM
- ❌ Hooks 在 `PipelineConfig` 破坏 POD 性质
- ❌ 时间戳精度说明不足
- ❌ TX 回调时序未明确

---

## 1. 当前架构分析

### 1.1 分层架构概览

```
┌─────────────────────────────────────────────────────────────┐
│                        CLI 层                               │
│  (apps/cli - One-shot 命令、REPL 模式、脚本系统)               │
├─────────────────────────────────────────────────────────────┤
│                      Client 层                               │
│  (crates/piper-client - Type State Pattern、Observer)          │
├─────────────────────────────────────────────────────────────┤
│                      Driver 层                               │
│  (crates/piper-driver - IO 线程、状态同步、Pipeline)          │
├─────────────────────────────────────────────────────────────┤
│                      Protocol 层                              │
│  (crates/piper-protocol - CAN 消息定义、编解码)               │
├─────────────────────────────────────────────────────────────┤
│                       CAN 层                                  │
│  (crates/piper-can - CanAdapter trait、硬件抽象)              │
└─────────────────────────────────────────────────────────────┘
```

### 1.2 CAN 帧流转路径

```
1. PiperBuilder::build()
   │
   ├─> 创建 CAN Adapter (SocketCanAdapter/GsUsbCanAdapter)
   │
   ├─> 调用 Piper::new_dual_thread(can, config)
   │
   └─> 在 new_dual_thread 中:
       │
       ├─> can.split() → (rx_adapter, tx_adapter)
       │
       ├─> 启动 RX 线程: rx_loop(rx_adapter, ...)
       │   │
       │   └─> 循环调用:
       │       ├─> rx.receive() → 接收 CAN 帧
       │       └─> parse_and_update_state() → 解析并更新状态
       │
       └─> 启动 TX 线程: tx_loop_mailbox(tx_adapter, ...)
           │
           └─> 从队列取命令并发送
```

### 1.3 关键代码路径

#### PiperBuilder 创建过程 (crates/piper-driver/src/builder.rs:270)
```rust
// 构建 SocketCAN 适配器
let mut can = SocketCanAdapter::new(interface)?;

// 使用双线程模式（默认）
Piper::new_dual_thread(can, self.pipeline_config.clone())
```

#### Piper 构造过程 (crates/piper-driver/src/piper.rs:173)
```rust
pub fn new_dual_thread<C>(can: C, config: Option<PipelineConfig>) -> Result<Self>
where
    C: SplittableAdapter + Send + 'static,
{
    // 分离适配器
    let (rx_adapter, tx_adapter) = can.split()?;

    // 启动 RX 线程
    let rx_thread = spawn(move || {
        rx_loop(rx_adapter, ctx_clone, config_clone, ...);
    });

    // 启动 TX 线程
    let tx_thread = spawn(move || {
        tx_loop_mailbox(tx_adapter, ...);
    });

    Ok(Self { ... })
}
```

#### RX 线程主循环 (crates/piper-driver/src/pipeline.rs:341)
```rust
pub fn rx_loop(
    mut rx: impl RxAdapter,
    ctx: Arc<PiperContext>,
    config: PipelineConfig,
    is_running: Arc<AtomicBool>,
    metrics: Arc<PiperMetrics>,
) {
    loop {
        // 1. 接收 CAN 帧（热路径）
        let frame = match rx.receive() {
            Ok(frame) => frame,
            Err(CanError::Timeout) => continue,
            Err(e) => break,
        };

        // 2. 解析并更新状态
        parse_and_update_state(&frame, &ctx, &config, &mut state);
    }
}
```

#### TX 线程主循环 (crates/piper-driver/src/pipeline.rs:485)
```rust
pub fn tx_loop_mailbox(
    mut tx: impl TxAdapter,
    realtime_slot: Arc<std::sync::Mutex<Option<crate::command::RealtimeCommand>>>,
    reliable_rx: Receiver<PiperFrame>,
    is_running: Arc<AtomicBool>,
    metrics: Arc<PiperMetrics>,
) {
    loop {
        // 发送命令到 CAN 总线
        if let Some(command) = realtime_command {
            tx.send(frame)?;
        }
    }
}
```

---

## 2. 问题根本原因分析

### 2.1 核心问题

**无法在 CLI 层访问原始 CAN 帧**

原因链：
1. **CAN Adapter 被消费**:
   - `PiperBuilder::build()` 创建 CAN adapter
   - `Piper::new_dual_thread()` 消费 adapter（move 语义）
   - adapter 所有权转移到 IO 线程

2. **IO 线程隔离**:
   - RX 线程拥有 adapter
   - 用户代码运行在主线程
   - 无法跨线程访问 adapter

3. **无钩子机制**:
   - `rx_loop` 和 `parse_and_update_state` 不提供回调
   - 无法在帧处理流程中插入自定义逻辑

### 2.2 架构约束

#### 约束 1: 层级依赖规则
```
正确依赖方向：
piper-can (底层) ← piper-driver ← piper-client ← piper-cli (顶层)

错误依赖（被禁止）:
piper-can → piper-tools ❌
```

**原因**:
- `piper-can` 是硬件抽象层，应该保持最小依赖
- `piper-tools` 包含高层业务逻辑
- 循环依赖会导致编译失败和维护困难

#### 约束 2: 所有权转移
```rust
// PiperBuilder
let can = SocketCanAdapter::new(interface)?;
let piper = Piper::new_dual_thread(can, config)?;
//                                          ^^^^
//                                          can 被移动，无法再访问
```

#### 约束 3: 线程隔离
```rust
// RX 线程
spawn(move || {
    rx_loop(rx_adapter, ...);  // rx_adapter 所有权转移到线程
});

// 主线程
// 无法访问 rx_adapter
```

#### ⚠️ 约束 4: 实时性要求（关键修正）🔥

**问题**: CAN 总线频率 500Hz-1kHz+，`rx_loop` 是系统热路径（Hot Path）

```rust
❌ 错误设计: 在 rx_loop 中使用 Mutex 阻塞

pub fn rx_loop(...) {
    loop {
        let frame = rx.receive()?;

        // ⚠️ 危险: 如果回调中使用 Mutex.lock()
        for callback in callbacks.iter() {
            callback.on_frame_received(&frame);
            // ^^^^ 如果这里获取 Mutex 阻塞：
            //     1. rx_loop 停止接收
            //     2. CAN 帧堆积
            //     3. 控制延迟/jitter
            //     4. 机器人运动不平滑
        }
    }
}

✅ 正确设计: 使用 Channel 异步发送

pub fn rx_loop(...) {
    loop {
        let frame = rx.receive()?;

        // ✅ 安全: try_send 非阻塞
        for sender in senders.iter() {
            let _ = sender.try_send(frame.clone());
            //     ^^^^ 开销微秒级，不阻塞
        }

        parse_and_update_state(&frame, &ctx, &config, &mut state);
    }
}
```

**性能要求**（500Hz-1kHz CAN 总线）:
- ❌ 回调执行时间: 必须 <10μs (微秒)
- ❌ 阻塞时间: 必须 <1μs (微秒)
- ✅ 允许: Clone + Channel send (无界或大容量队列)
- ✅ 禁止: Mutex lock, I/O, 分配

---

## 3. 解决方案设计（修正版）

### 方案 A: Driver 层异步录制钩子（推荐 ⭐⭐⭐⭐⭐）

#### 设计概述
在 `piper-driver` 层添加**异步录制钩子**，使用 Channel（Actor 模式）避免热路径阻塞。

#### 核心架构设计

```rust
// 1. 定义帧回调 trait（快速，非阻塞）
pub trait FrameCallback: Send + Sync {
    /// 帧接收回调（必须在 <10μs 内返回）
    fn on_frame_received(&self, frame: &PiperFrame);
}

// 2. 异步录制通道
pub struct AsyncRecordingHook {
    sender: crossbeam::channel::Sender<TimestampedFrame>,
}

impl FrameCallback for AsyncRecordingHook {
    fn on_frame_received(&self, frame: &PiperFrame) {
        // 极速操作：仅 Clone 数据并发送（非阻塞）
        let _ = self.sender.try_send(TimestampedFrame::from(frame));
        //            ^^^^ 使用 try_send 避免阻塞
        // 如果队列满，丢弃此帧（优先保证实时性）
    }
}

// 3. 完整性：同时录制 RX 和 TX
pub enum FrameDirection {
    RX, // 接收帧
    TX, // 发送帧
}

pub trait FrameCallbackEx: FrameCallback {
    /// 带方向信息的回调
    fn on_frame_received_ex(&self, frame: &PiperFrame, direction: FrameDirection);
}
```

#### 实施步骤

##### 第一步：定义录制钩子
```rust
// crates/piper-driver/src/recording.rs (新建)

use crossbeam::channel::{Sender, unbounded};
use piper_tools::TimestampedFrame;

/// 异步录制钩子（Actor 模式）
pub struct AsyncRecordingHook {
    tx: Sender<TimestampedFrame>,
    _rx: Receiver<TimestampedFrame>,  // 保留用于未来扩展
}

impl AsyncRecordingHook {
    /// 创建新的录制钩子
    pub fn new() -> (Self, Receiver<TimestampedFrame>) {
        let (tx, rx) = unbounded();
        (
            Self { tx },
            rx
        )
    }

    /// 获取发送端（用于注册回调）
    pub fn sender(&self) -> Sender<TimestampedFrame> {
        self.tx.clone()
    }
}

impl FrameCallback for AsyncRecordingHook {
    #[inline]
    fn on_frame_received(&self, frame: &PiperFrame) {
        // 极速发送，不阻塞（如果队列满则丢弃帧）
        let _ = self.tx.try_send(TimestampedFrame::from(frame));
    }
}

// 同理，用于 TX 路径
pub struct TxRecordingHook {
    tx: Sender<TimestampedFrame>,
}

impl FrameCallback for TxRecordingHook {
    #[inline]
    fn on_frame_received(&self, frame: &PiperFrame) {
        let _ = self.tx.try_send(TimestampedFrame::from(frame));
    }
}
```

##### 第二步：在 PipelineConfig 中添加钩子配置
```rust
// crates/piper-driver/src/state.rs

use crate::recording::FrameCallback;
use std::sync::Arc;

pub struct PipelineConfig {
    pub receive_timeout_ms: u64,
    pub frame_group_timeout_ms: u64,
    pub velocity_buffer_timeout_us: u64,

    /// 🆕 新增：帧回调列表
    pub frame_callbacks: Vec<Arc<dyn FrameCallback>>,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            receive_timeout_ms: 2,
            frame_group_timeout_ms: 10,
            velocity_buffer_timeout_us: 10_000,
            frame_callbacks: Vec::new(),  // 新增
        }
    }
}
```

##### 第三步：在 rx_loop 中触发回调（非阻塞）
```rust
// crates/piper-driver/src/pipeline.rs

pub fn rx_loop(
    mut rx: impl RxAdapter,
    ctx: Arc<PiperContext>,
    config: PipelineConfig,
    ...
) {
    loop {
        // 检查运行标志
        if !is_running.load(Ordering::Acquire) {
            break;
        }

        // 1. 接收 CAN 帧
        let frame = match rx.receive() {
            Ok(frame) => frame,
            Err(CanError::Timeout) => {
                // ... 超时处理 ...
                continue;
            },
            Err(e) => {
                // ... 错误处理 ...
                break;
            },
        };

        // 2. 🆕 触发所有回调（非阻塞，Channel 模式）
        for callback in config.frame_callbacks.iter() {
            callback.on_frame_received(&frame);
            // ^^^^ 使用 try_send，<1μs 开销，不阻塞
        }

        // 3. 原有解析逻辑
        parse_and_update_state(&frame, &ctx, &config, &mut state);
    }
}
```

##### 第四步：在 tx_loop 中也触发回调（可选）
```rust
pub fn tx_loop_mailbox(
    mut tx: impl TxAdapter,
    ...
) {
    loop {
        // 发送命令
        if let Some(command) = realtime_command {
            // 发送前回调
            for callback in callbacks.iter() {
                callback.on_frame_ex(&frame, FrameDirection::TX);
            }

            tx.send(frame)?;

            // 发送后回调
            for callback in callbacks.iter() {
                callback.on_frame_ex(&frame, FrameDirection::TX);
            }
        }
    }
}
```

#### 优点
- ✅ **零阻塞**: Channel 模式，不阻塞热路径
- ✅ **高性能**: 帧复制 <1μs，try_send 非阻塞
- ✅ **架构清晰**: 分层设计，职责明确
- ✅ **可扩展**: 支持多个回调，易于添加功能
- ✅ **跨平台**: 所有平台统一方案
- ✅ **数据完整**: 同时录制 RX 和 TX

#### 缺点
- ⚠️ 需要修改 driver 层（~200 行）
- ⚠️ 队列满时会丢帧（但这是正确的行为，优先保证实时性）
- ⚠️ 需要后台线程处理录制数据

#### 性能分析
```rust
// 热路径开销分析（500Hz）

每帧处理时间：
- rx.receive()           ~100μs (硬件读取)
- parse_and_update()  ~10μs  (解析)
- 回调 (Channel)        ~1μs   (try_send + Clone)
────────────────────────────────
总计:                    ~111μs / 帧

CAN 总线频率：1000Hz
  -> 周期：1000μs
  -> 余量：889μs (80% 时间空闲)

✅ 性能完全满足要求
```

#### 实施复杂度
- **代码量**: ~250 行
- **修改文件**:
  - `crates/piper-driver/src/recording.rs` (新建, ~80 行)
  - `crates/piper-driver/src/pipeline.rs` (~30 行)
  - `crates/piper-driver/src/state.rs` (~20 行)
  - `crates/piper-driver/src/piper.rs` (~30 行)
  - `apps/cli/src/commands/record.rs` (~80 行)
- **测试工作量**: 1-2 天
- **总工作量**: 2-3 天

---

### 方案 B: 可观测性模式（最优雅 ⭐⭐⭐）

#### 设计概述
引入"可观测性模式"概念，提供录制、回放、监控等多种模式。

#### 架构设计

```rust
// 1. 定义可观测性模式
pub enum ObservabilityMode {
    /// 正常模式（默认）
    Normal,

    /// 录制模式（异步 Channel 模式）
    Recording {
        /// 数据发送端（Channel 发送者）
        sender: Sender<TimestampedFrame>,
        /// 录制元数据
        metadata: RecordingMetadata,
    },

    /// 回放模式
    Replay {
        recording: PiperRecording,
        speed: f64,
    },

    /// 监控模式（统计和分析）
    Monitor {
        stats: Arc<Mutex<Statistics>>,
    },
}

// 2. 添加到 PipelineConfig
pub struct PipelineConfig {
    pub receive_timeout_ms: u64,
    pub frame_group_timeout_ms: u64,
    pub velocity_buffer_timeout_us: u64,

    /// 🆕 可观测性模式
    pub observability: ObservabilityMode,
}

// 3. 在 rx_loop 中处理
pub fn rx_loop(...) {
    loop {
        let frame = rx.receive()?;

        // 可观测性处理（快速，<1μs）
        match &config.observability {
            ObservabilityMode::Normal => {
                // 正常模式：不做额外处理
            },
            ObservabilityMode::Recording { sender, .. } => {
                // 录制模式：异步发送帧
                let _ = sender.try_send(TimestampedFrame::from(frame));
            },
            ObservabilityMode::Replay { .. } => {
                // 回放模式：从文件读取（稍后实现）
            },
            ObservabilityMode::Monitor { stats, .. } => {
                // 监控模式：更新统计
                if let Ok(mut stats) = stats.try_lock() {
                    stats.frame_count += 1;
                }
            },
        }

        // 原有逻辑
        parse_and_update_state(&frame, &ctx, &config, &mut state);
    }
}
```

#### 使用示例
```rust
// 创建录制钩子
let (tx, _rx) = unbounded();
let metadata = RecordingMetadata::new("can0".to_string(), 1_000_000);

let config = PipelineConfig {
    observability: ObservabilityMode::Recording { tx, metadata },
    ..Default::default()
};

let piper = PiperBuilder::new()
    .pipeline_config(config)
    .build()?;

// 后台线程处理录制
spawn(move || {
    while let Ok(frame) = rx.recv() {
        recording.add_frame(frame);
        // 定期保存到磁盘...
    }
});
```

#### 优点
- ✅ **最优雅的设计**
- ✅ **配置驱动**，声明式使用
- ✅ **可扩展**：支持多种可观测性功能
- ✅ **零阻塞**：Channel 模式
- ✅ **跨平台**

#### 缺点
- ⚠️ 需要更多架构改动
- ⚠️ 实施时间较长

#### 实施复杂度
- **代码量**: ~400 行
- **修改文件**: 6-8 个
- **测试工作量**: 2-3 天
- **总工作量**: 3-4 天

---

### 方案 C: 自定义 CAN Adapter（灵活性 ⭐⭐⭐）

#### 设计概述
提供自定义 CAN adapter 的构建接口，允许用户包装 adapter。

#### 复杂性分析（修正）

```rust
pub struct RecordingAdapter<A> {
    inner: A,
    sender: Sender<TimestampedFrame>,
}

// ⚠️ 问题：需要实现 SplittableAdapter
impl<A: SplittableAdapter> SplittableAdapter for RecordingAdapter<A>
where
    A: RxAdapter + TxAdapter + Send + 'static,
{
    type RxAdapter = RecordingRxAdapter<A>;
    type TxAdapter = RecordingTxAdapter<A>;

    fn split(self) -> Result<(Self::RxAdapter, Self::TxAdapter), CanError> {
        let (rx_inner, tx_inner) = self.inner.split()?;

        Ok((
            RecordingRxAdapter {
                inner: rx_inner,
                sender: self.sender.clone(),
            },
            RecordingTxAdapter {
                inner: tx_inner,
                sender: self.sender.clone(),
            },
        ))
    }
}

// 需要额外的包装类型
pub struct RecordingRxAdapter<A> {
    inner: A::RxAdapter,
    sender: Sender<TimestampedFrame>,
}

impl<A: RxAdapter> RxAdapter for RecordingRxAdapter<A> {
    fn receive(&mut self) -> Result<PiperFrame, CanError> {
        let frame = self.inner.receive()?;
        let _ = self.sender.try_send(TimestampedFrame::from(frame));
        Ok(frame)
    }
}

// ... TxAdapter 类似
```

#### 修正的实施复杂度
- **代码量**: ~200 行（比原估算多）
- **样板代码**: SplittableAdapter 包装
- **测试工作量**: 1-2 天
- **总工作量**: 2-3 天

#### 优点
- ✅ 完全灵活
- ✅ 不修改 driver 层核心
- ✅ RX 和 TX 都能录制

#### 缺点
- ⚠️ 需要实现 SplittableAdapter（样板代码多）
- ⚠️ API 不太友好
- ⚠️ 用户需要处理更多细节

---

### 方案 D: 旁路监听模式（无侵入 ⭐⭐⭐⭐）

#### 设计概述
利用 SocketCAN 的多读特性，创建额外的监听 adapter。

#### 平台支持（修正）

**Linux SocketCAN**:
```rust
let mut bypass = SocketCanAdapter::new("can0")?;
// ✅ 支持，多读并发
```

**GS-USB (修正)**:
```rust
// 情况 1: 使用 socketcan-rs（Linux 内核模块）
//     → 设备显示为 can0/can1
//     → ✅ 支持方案 D

// 情况 2: 使用 libusb 用户态驱动
//     → SDK 直接打开 USB 设备（独占访问）
//     → ❌ 不支持方案 D
```

#### 架构设计
```rust
pub async fn record_with_bypass(interface: &str) -> Result<()> {
    // 主 adapter（用于控制）
    let piper = PiperBuilder::new()
        .interface(interface)
        .build()?;

    // 旁路 adapter（用于监听）
    let mut bypass = SocketCanAdapter::new(interface)?;
    let recording = Arc::new(Mutex::new(
        PiperRecording::new(metadata)
    ));

    // 后台线程录制
    let stop_signal = Arc::new(AtomicBool::new(false));
    spawn(move || {
        while !stop_signal.load(Ordering::Relaxed) {
            if let Ok(frame) = bypass.receive_timeout(Duration::from_millis(100)) {
                if let Ok(mut rec) = recording.try_lock() {
                    rec.add_frame(TimestampedFrame::from(frame));
                }
            }
        }
    });

    // 主线程执行控制
    tokio::time::sleep(Duration::from_secs(10)).await;

    // 停止录制
    stop_signal.store(true, Ordering::Release);

    // 保存录制
    let recording = Arc::try_unwrap(recording).unwrap();
    recording.into_inner().save("output.bin")?;
}
```

#### 平台兼容性表（修正）

| 平台 | Driver 实现 | 方案 D 可用性 |
|------|------------|-------------|
| Linux + SocketCAN | socketcan-rs | ✅ 完全支持 |
| Linux + GS-USB | socketcan-rs | ✅ 完全支持 |
| Linux + GS-USB | libusb 用户态 | ❌ 不支持 |
| macOS | libusb 用户态 | ❌ 不支持 |
| Windows | libusb 用户态 | ❌ 不支持 |

#### 优点
- ✅ **零侵入**：不需要修改任何现有代码
- ✅ **真实帧**：录制真正的原始 CAN 帧
- ✅ **高性能**：不影响主控制回路
- ✅ **简单直接**

#### 缺点
- ❌ **平台限制**：仅 Linux SocketCAN
- ⚠️ 需要管理额外的线程和 adapter

#### 实施复杂度
- **代码量**: ~150 行
- **修改文件**:
  - `apps/cli/src/commands/record.rs` (~150 行)
- **测试工作量**: 1 天
- **总工作量**: 1-2 天

---

### 方案 E: 智能重建（逻辑重放）⚠️

#### 设计概述（重新定位）

**⚠️ 重要修正**: 这是**逻辑重放**，不是真正的 CAN 帧录制

```rust
pub async fn record_logic_replay(duration: Duration) -> Result<PiperRecording> {
    let piper = PiperBuilder::new().build()?;
    let mut recording = PiperRecording::new(metadata);

    while start.elapsed() < duration {
        // 1. 读取状态（触发 CAN 通信）
        let position = piper.get_joint_position();
        let end_pose = piper.get_end_pose();

        // 2. 重建 CAN 帧（模拟数据）
        for i in 0..6 {
            let frame = JointFeedbackFrame::new()
                .with_joint(i, position.joint_pos[i])
                .with_timestamp(position.hardware_timestamp_us);

            recording.add_frame(TimestampedFrame::from(frame));
        }

        // 3. 控制采样率
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    Ok(recording)
}
```

#### ⚠️ 风险评估（新增）

**数据丢失**:
- ❌ **时间戳不精确**：软件重建，非硬件时间戳
- ❌ **错误帧丢失**：无法记录总线错误帧
- ❌ **仲裁顺序丢失**：无法记录 CAN 仲裁
- ❌ **上下文缺失**：只有 RX，没有 TX

**适用场景**:
- ✅ 逻辑重放（重现应用层操作）
- ✅ 轨件测试（验证控制逻辑）
- ❌ **底层调试**（如信号干扰、总线负载）

**用户体验建议**:
```bash
$ piper-cli record --output test.bin --duration 10

⚠️  警告：当前模式为逻辑重放
⚠️  - 时间戳由软件生成
⚠️  - 不包含总线错误帧
⚠️  - 仅适用于应用层测试

如需录制原始 CAN 总线数据：
  Linux: piper-cli record --mode can-bypass --output test.bin
  其他平台: 逻辑重放是唯一选项
```

#### 优点
- ✅ **跨平台**：所有平台统一方案
- ✅ **简单**：1 天实现
- ✅ **零侵入**

#### 缺点
- ⚠️ **不是真正的 CAN 录制**
- ⚠️ **无法用于底层调试**
- ⚠️ **数据不完整**

---

## 4. 方案对比（修正版）

### 4.1 功能对比表

| 方案 | 真实帧 | 时间戳精度 | 错误帧 | RX+TX | 跨平台 | 非阻塞 | 实施时间 |
|------|--------|----------|--------|-------|--------|--------|----------|
| **A: Driver 钩子** | ✅ | ✅ 硬件 | ✅ | ✅ | ✅ | ✅ | 2-3天 |
| **B: 可观测性** | ✅ | ✅ 硬件 | ✅ | ✅ | ✅ | ✅ | 3-4天 |
| **C: 自定义 Adapter** | ✅ | ✅ 硬件 | ✅ | ✅ | ✅ | ✅ | 2-3天 |
| **D: 旁路监听** | ✅ | ✅ 硬件 | ✅ | ❌ | ✅ | ✅ | 1-2天 |
| **E: 逻辑重放** | ❌ | ⚠️ 软件 | ❌ | ❌ | ✅ | ✅ | 1天 |

### 4.2 性能对比表

| 方案 | 热路径开销 | 内存开销 | CPU 开销 | 抖动风险 |
|------|-----------|----------|---------|----------|
| **A: Driver 钩子** | <1μs | 中等 | 低（后台线程） | ✅ 无 |
| **B: 可观测性** | <1μs | 中等 | 低（后台线程） | ✅ 无 |
| **C: 自定义 Adapter** | <1μs | 低 | 低 | ✅ 无 |
| **D: 旁路监听** | 0μs | 低 | 低（独立线程） | ✅ 无 |
| **E: 逻辑重放** | ~10μs | 低 | 中 | ⚠️ 轻微抖动 |

### 4.3 实施时间对比表

| 方案 | 设计 | 编码 | 测试 | 总计 |
|------|------|------|------|------|
| **A: Driver 钩子** | 0.5d | 1d | 1d | 2.5d |
| **B: 可观测性** | 1d | 1.5d | 1.5d | 4d |
| **C: 自定义 Adapter** | 0.5d | 1d | 1d | 2.5d |
| **D: 旁路监听** | 0.5d | 0.5d | 0.5d | 1.5d |
| **E: 逻辑重放** | 0.5d | 0.5d | 0.5d | 1.5d |

---

## 5. 推荐方案（修正版）

### 5.1 短期方案（1-2 天）

**方案 D: 旁路监听（Linux） + 方案 E: 逻辑重放（跨平台）**

**Linux SocketCAN 环境**:
```rust
// ✅ 真实 CAN 帧录制
use socketcan::SocketCanAdapter;

let mut bypass = SocketCanAdapter::new("can0")?;
spawn(move || {
    while !stop_signal {
        if let Ok(frame) = bypass.receive() {
            recording.add_frame(frame);  // 真实 CAN 帧
        }
    }
});
```

**其他平台（macOS/Windows）**:
```rust
// ⚠️ 逻辑重放（非真实 CAN 帧）
let piper = PiperBuilder::new().build()?;
while elapsed < duration {
    let state = piper.get_joint_position();
    // 重建 CAN 帧...
}
```

**用户提示**:
```bash
$ piper-cli record --output test.bin --duration 10

⚠️  注意：当前模式为逻辑重放
⚠️  - 时间戳由软件生成
⚠️  - 不包含总线错误帧
⚠️  - 仅适用于应用层测试

如需录制原始 CAN 总线数据：
  Linux: 自动使用 CAN 旁路监听（真实帧）
  其他平台: 逻辑重放（模拟帧）
```

---

### 5.2 中期方案（1 周）⭐⭐⭐⭐⭐

**方案 A: Driver 层异步录制钩子（Channel 模式）**

**关键修正**: 使用 `crossbeam::channel::Sender` 代替 `Arc<Mutex>`

```rust
// ❌ 错误设计（会阻塞热路径）
pub struct RecordingCallback {
    recording: Arc<Mutex<PiperRecording>>,
}

impl FrameCallback for RecordingCallback {
    fn on_frame_received(&self, frame: &PiperFrame) {
        let mut rec = self.recording.lock().unwrap();  // ❌ 可能阻塞
        rec.add_frame(...);
    }
}

// ✅ 正确设计（Channel 模式，不阻塞）
pub struct AsyncRecordingHook {
    sender: Sender<TimestampedFrame>,
}

impl FrameCallback for AsyncRecordingHook {
    fn on_frame_received(&self, frame: &PiperFrame) {
        let _ = self.sender.try_send(TimestampedFrame::from(frame));
        //   ^^^^^^ 非阻塞，<1μs
    }
}
```

**完整实施**:
1. 定义 `FrameCallback` trait
2. 实现异步录制钩子
3. 在 `rx_loop` 中注册
4. 在 `tx_loop` 中注册（可选，录制 TX）
5. 后台线程处理录制数据

---

### 5.3 长期方案（2-4 周）⭐⭐⭐⭐⭐

**方案 B: 可观测性模式**

**扩展能力**:
- 性能分析
- 数据包捕获
- 实时可视化
- 分布式追踪

---

## 6. 详细实施指南（方案 A 修正版）

### 6.1 第一步：定义回调 trait

**文件**: `crates/piper-driver/src/callback.rs` (新建)

```rust
//! CAN 帧回调 trait
//!
//! 提供用户自定义 CAN 帧处理的能力

use crate::pipeline::PiperFrame;

/// CAN 帧回调 trait
///
/// ⚠️ 性能关键: 此方法在 RX 线程热路径中被调用
///
/// # Thread Safety
/// 回调方法会在 RX 线程中被调用，因此：
/// - 必须是 `Send + Sync`
/// - **必须快速返回**（< 10μs）
/// - **不能阻塞**（包括获取锁、I/O 操作）
/// - 不应执行耗时计算
///
/// # Performance Requirements
///
/// 对于 500Hz-1kHz 的 CAN 总线：
/// - 每帧可用时间: 1000μs (1kHz) 到 2000μs (500Hz)
/// - 帧处理预算: <10μs
/// - 回调开销: <1μs
///
/// # Example
///
/// ```no_run
/// use piper_driver::callback::FrameCallback;
///
/// struct MyCallback;
///
/// impl FrameCallback for MyCallback {
///     fn on_frame_received(&self, frame: &PiperFrame) {
///         // 快速操作：<1μs
///         println!("Frame: 0x{:03X}", frame.id);
///     }
/// }
/// ```
pub trait FrameCallback: Send + Sync {
    /// 帧接收回调（< 10μs）
    ///
    /// # 注意
    /// - 此方法在 RX 线程中调用
    /// - **绝对禁止**使用 Mutex、lock、I/O、阻塞操作
    /// - 仅执行快速操作（日志、计数、Channel send）
    fn on_frame_received(&self, frame: &PiperFrame);
}

/// 方向扩展（可选，用于完整录制）
pub trait FrameCallbackEx: FrameCallback {
    /// 带方向信息的回调
    ///
    /// # Direction
    /// - RX: 接收帧（来自总线）
    /// - TX: 发送帧（发送到总线）
    fn on_frame_ex(&self, frame: &PiperFrame, direction: FrameDirection);
}

pub enum FrameDirection {
    RX,
    TX,
}
```

### 6.2 第二步：实现异步录制钩子

**文件**: `crates/piper-driver/src/recording.rs` (新建)

```rust
//! 异步录制钩子
//!
//! 使用 Channel (Actor 模式) 实现 CAN 帧录制

use crossbeam::channel::{unbounded, Sender, Receiver};
use piper_protocol::PiperFrame;
use piper_tools::{TimestampedFrame, TimestampSource};
use crate::callback::FrameCallback;

/// 异步录制钩子
pub struct AsyncRecordingHook {
    sender: Sender<TimestampedFrame>,
}

impl AsyncRecordingHook {
    /// 创建新的录制钩子
    pub fn new() -> (Self, Receiver<TimestampedFrame>) {
        let (tx, rx) = unbounded();
        (Self { tx }, rx)
    }

    /// 获取发送端（用于注册回调）
    pub fn sender(&self) -> Sender<TimestampedFrame> {
        self.tx.clone()
    }
}

impl FrameCallback for AsyncRecordingHook {
    #[inline]
    fn on_frame_received(&self, frame: &PiperFrame) {
        // 极速发送：try_send 非阻塞，<1μs
        let _ = self.sender.try_send(TimestampedFrame::from(frame));
        // 队列满时丢帧（保证实时性）
    }
}

/// TX 路径录制钩子
pub struct AsyncTxRecordingHook {
    sender: Sender<TimestampedFrame>,
}

impl FrameCallback for AsyncTxRecordingHook {
    #[inline]
    fn on_frame_received(&self, frame: &PiperFrame) {
        let _ = self.sender.try_send(TimestampedFrame::from(frame));
    }
}
```

### 6.3 第三步：修改 PipelineConfig

**文件**: `crates/piper-driver/src/state.rs`

```rust
use crate::callback::FrameCallback;
use std::sync::Arc;

pub struct PipelineConfig {
    pub receive_timeout_ms: u64,
    pub frame_group_timeout_ms: u64,
    pub velocity_buffer_timeout_us: u64,

    /// 帧回调列表（🆕 新增）
    pub frame_callbacks: Vec<Arc<dyn FrameCallback>>,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            receive_timeout_ms: 2,
            frame_group_timeout_ms: 10,
            velocity_buffer_timeout_us: 10_000,
            frame_callbacks: Vec::new(),
        }
    }
}
```

### 6.4 第四步：在 rx_loop 中触发回调

**文件**: `crates/piper-driver/src/pipeline.rs`

```rust
pub fn rx_loop(
    mut rx: impl RxAdapter,
    ctx: Arc<PiperContext>,
    config: PipelineConfig,
    is_running: Arc<AtomicBool>,
    metrics: Arc<PiperMetrics>,
) {
    loop {
        // ... 运行检查 ...

        // 1. 接收 CAN 帧
        let frame = match rx.receive() {
            Ok(frame) => frame,
            Err(CanError::Timeout) => {
                // ... 超时处理 ...
                continue;
            },
            Err(e) => { /* ... 错误处理 ... */ break; },
        };

        // 2. 🆕 触发所有回调（非阻塞，<1μs）
        for callback in config.frame_callbacks.iter() {
            callback.on_frame_received(&frame);
        }

        // 3. 原有解析逻辑
        parse_and_update_state(&frame, &ctx, &config, &mut state);
    }
}
```

### 6.5 第五步：CLI 集成

**文件**: `apps/cli/src/commands/record.rs`

```rust
use piper_driver::callback::{FrameCallback, AsyncRecordingHook};
use piper_driver::PipelineConfig;
use crossbeam_channel::unbounded;

pub async fn execute(&self, _config: &OneShotConfig) -> Result<()> {
    println!("⏳ 连接到机器人...");

    // 1. 创建录制钩子
    let (hook, rx) = AsyncRecordingHook::new();
    let callback = std::sync::Arc::new(hook) as Arc<dyn FrameCallback>;

    // 2. 配置回调
    let config = PipelineConfig {
        frame_callbacks: vec![callback],
        ..Default::default()
    };

    // 3. 连接机器人（带回调）
    let piper = PiperBuilder::new()
        .interface(interface_str)
        .pipeline_config(config)
        .build()?;

    println!("✅ 已连接，开始录制...");

    // 4. 执行操作（自动录制）
    let start = std::time::Instant::now();

    while start.elapsed() < Duration::from_secs(self.duration) {
        // 触发 CAN 通信
        let _ = piper.get_joint_position();

        // 进度显示
        print!("\r录制中: {:.1}s / {}s",
            start.elapsed().as_secs_f64(),
            self.duration
        );
        std::io::stdout().flush().ok();

        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    println!("\n✅ 录制完成: {} 帧", rx.recv().count());

    // 5. 保存录制
    let recording = PiperRecording::new(metadata);
    for frame in rx {
        recording.add_frame(frame);
    }
    recording.save(&self.output)?;

    println!("✅ 保存完成");

    Ok(())
}
```

---

## 7. 风险评估（修正版）

### 7.1 技术风险

| 风险 | 影响 | 概率 | 缓解措施 |
|------|------|------|----------|
| 回调性能下降 | 中 | 低 | Channel 模式，<1μs 开销 |
| 队列满导致丢帧 | 低 | 低 | 可接受（优先保证实时性） |
| 线程安全 | 高 | 低 | 充分测试，使用 Arc/Channel |
| 跨平台兼容 | 中 | 低 | 平台特定实现 |

### 7.2 数据完整性

### 数据类型对比

| 方案 | 原始帧 | 硬件时间戳 | 错误帧 | TX 帧 | 仲裁顺序 |
|------|--------|------------|--------|-------|----------|
| **A: Driver 钩子** | ✅ | ✅ | ✅ | ✅ | ✅ |
| **B: 可观测性** | ✅ | ✅ | ✅ | ✅ | ✅ |
| **C: 自定义 Adapter** | ✅ | ✅ | ✅ | ✅ | ✅ |
| **D: 旁路监听** | ✅ | ✅ | ✅ | ❌ | ✅ |
| **E: 逻辑重放** | ❌ | ❌ | ❌ | ❌ | ❌ |

### 7.3 应用场景适配

| 应用场景 | 推荐方案 | 理由 |
|---------|----------|------|
| **底层调试** | A/B/D | 需要完整 CAN 信息 |
| **性能分析** | A/B | 需要高精度数据 |
| **逻辑重放** | E | 仅应用层测试 |
| **CI/自动化** | E | 简单快捷 |
| **长期维护** | B | 架构优雅 |

---

## 8. 总结

### 8.1 问题回顾

**核心问题**: 无法在 CLI 层访问原始 CAN 帧

**根本原因**:
1. 分层架构导致的依赖限制
2. CAN adapter 被消费并移动到 IO 线程
3. 缺少钩子机制

### 8.2 推荐解决方案（3 阶段）

| 时间 | 方案 | 目标 | 平台 |
|------|------|------|------|
| **短期（1-2天）** | D + E 混合 | 快速解决 | Linux: D<br/>其他: E |
| **中期（1周）** | **A: Driver 钩子** | 核心功能 | 所有平台 |
| **长期（2-4周）** | **B: 可观测性** | 完整框架 | 所有平台 |

### 8.3 关键收益

实施后将获得：
- ✅ 真实的 CAN 帧录制
- ✅ 完整的回放功能
- ✅ 性能分析工具
- ✅ 可扩展架构
- ✅ **零阻塞**: 使用 Channel 模式，不影响实时性能

### 8.4 核心修正总结

**关键改进**（基于专家反馈）:
1. ✅ **性能优先**: 使用 Channel 代替 Mutex，避免热路径阻塞
2. ✅ **数据真实性**: 明确方案 E 为逻辑重放，不是 CAN 录制
3. ✅ **完整性**: 补充 TX 路径录制
4. ✅ **平台兼容**: 修正 GS-USB 旁路监听的可行性
5. ✅ **架构清晰**: Actor 模式，职责分离

---

**报告作者**: Claude Code
**日期**: 2026-01-27
**版本**: v1.1（已修正）
**许可证**: MIT OR Apache-2.0
