# Piper SDK - CAN 帧录制架构限制深度分析报告

> **⚠️ 已废弃**
>
> **本文档（v1.0）包含已知的架构和性能问题，已被 v1.2 版本取代。**
>
> **主要问题**:
> - ❌ 方案 A 使用 `Arc<Mutex<PiperRecording>>` 会导致热路径阻塞
> - ❌ 方案 E 误导性地称为 "CAN 帧录制"，实际是"逻辑重放"
> - ❌ 缺少 TX 路径录制
> - ❌ GS-USB 平台兼容性描述不准确
> - ❌ **v1.1 遗留问题**: `unbounded()` 可能导致 OOM（v1.2 已修正）
> - ❌ **v1.1 遗留问题**: Hooks 在 `PipelineConfig` 破坏 POD 性质（v1.2 已修正）
>
> **请阅读最新版本**: [`can-recording-analysis-v1.2.md`](./can-recording-analysis-v1.2.md) ⭐
>
> **版本历史**:
> - **v1.2** (最新) - 🎯 工程就绪版（内存安全、架构优化、时间戳精度、TX 安全）
> - **v1.1** - 性能修正（Channel 模式、数据真实性、平台兼容性）
> - **v1.0** (本文档) - 初始版本（存在严重问题，已废弃）
>
> **v1.1 关键修正**:
> - ✅ 方案 A 改用 Channel 模式（非阻塞，<1μs）
> - ✅ 方案 E 重新定位为"逻辑重放"
> - ✅ 补充 TX 路径录制
> - ✅ 修正 GS-USB 平台兼容性
>
> **v1.2 工程安全修正**:
> - 🛡️ 使用 `bounded(10000)` 代替 `unbounded()` 防止 OOM
> - 🏗️ Hooks 从 `PipelineConfig` 移至 `PiperContext`
> - ⏱️ 强制使用硬件时间戳 `frame.timestamp_us()`
> - 🔒 仅在 `send()` 成功后记录 TX 帧
> - 🌐 明确方案 D 依赖 SocketCAN Loopback
>
> **执行摘要**: [`EXECUTIVE_SUMMARY.md`](./EXECUTIVE_SUMMARY.md)
>
> ---
>
> **以下为 v1.0 原文（仅供参考）**
>
> ---
>

**日期**: 2026-01-27
**版本**: v1.0（已废弃）
**状态**: ⚠️ 已被 v1.2 取代

---

## 执行摘要

本报告深入分析了 Piper SDK 中 CAN 帧录制功能面临的架构限制，识别了根本原因，提出了多种解决方案，并给出了推荐方案和实施路径。核心问题在于**分层架构导致无法直接访问原始 CAN 帧**。

**关键发现**:
- ✅ 问题可解决，但需要架构改进
- ✅ 最佳方案：在 driver 层添加录制钩子
- ✅ 实施复杂度：中等
- ✅ 预计工作量：2-3 天

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
        // 1. 接收 CAN 帧
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

#### 帧解析逻辑 (crates/piper-driver/src/pipeline.rs:715)
```rust
fn parse_and_update_state(
    frame: &PiperFrame,
    ctx: &Arc<PiperContext>,
    config: &PipelineConfig,
    state: &mut ParserState,
) {
    match frame.raw_id() {
        ID_JOINT_FEEDBACK_12 => { /* 处理关节反馈 */ },
        ID_JOINT_FEEDBACK_34 => { /* 处理关节反馈 */ },
        ID_JOINT_FEEDBACK_56 => { /* 处理关节反馈 */ },
        // ... 其他 CAN ID
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

错误依赖（被禁止）：
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

### 2.3 当前实现的局限性

#### CLI 层录制尝试（已失败）
```rust
// ❌ 方案 1: 包装 CanAdapter
// 问题: 无法在不破坏依赖规则的情况下在 piper-can 中使用 piper-tools

pub struct RecordingCanAdapter<A> {
    inner: A,
    recording: PiperRecording,  // 依赖 piper-tools
}
// piper-can 不能依赖 piper-tools！
```

```rust
// ❌ 方案 2: 在 CLI 层直接访问 driver
// 问题: CAN adapter 已经被移动到 IO 线程

let piper = PiperBuilder::new().build()?;
// piper 内部持有 adapter，但无法访问
```

---

## 3. 解决方案设计

### 方案 A: Driver 层录制钩子（推荐 ⭐）

#### 设计概述
在 `piper-driver` 层添加录制钩子，允许用户注册帧回调。

#### 架构设计

```rust
// 1. 定义帧回调 trait
pub trait FrameCallback: Send + Sync {
    fn on_frame_received(&self, frame: &PiperFrame);
}

// 2. 可共享的录制回调
pub struct RecordingCallback {
    recording: Arc<Mutex<PiperRecording>>,
}

impl FrameCallback for RecordingCallback {
    fn on_frame_received(&self, frame: &PiperFrame) {
        let mut recording = self.recording.lock().unwrap();
        // 转换为 TimestampedFrame 并添加
        let timestamped = TimestampedFrame::new(
            frame.timestamp_us(),
            frame.raw_id(),
            frame.data().to_vec(),
            TimestampSource::Hardware,
        );
        recording.add_frame(timestamped);
    }
}

// 3. PiperContext 添加回调列表
pub struct PiperContext {
    // ... 现有字段

    /// 帧回调列表
    frame_callbacks: Vec<Arc<dyn FrameCallback>>,
}

impl PiperContext {
    pub fn add_frame_callback(&mut self, callback: Arc<dyn FrameCallback>) {
        self.frame_callbacks.push(callback);
    }
}

// 4. 在 rx_loop 中触发回调
pub fn rx_loop(...) {
    loop {
        let frame = rx.receive()?;

        // === 新增：触发所有回调 ===
        for callback in ctx.frame_callbacks.iter() {
            callback.on_frame_received(&frame);
        }

        // === 原有逻辑 ===
        parse_and_update_state(&frame, &ctx, &config, &mut state);
    }
}
```

#### 使用示例
```rust
// CLI 层使用
let recording = Arc::new(Mutex::new(
    PiperRecording::new(metadata)
));

let callback = Arc::new(RecordingCallback {
    recording: recording.clone(),
});

// 注册回调
piper.context().add_frame_callback(callback);

// 录制完成后保存
let recording = Arc::try_unwrap(recording).unwrap();
let recording = recording.into_inner();
recording.save("output.bin")?;
```

#### 优点
- ✅ 架构清晰，不破坏分层
- ✅ 可扩展，支持多个回调
- ✅ 性能影响小（回调在接收线程中）
- ✅ 类型安全

#### 缺点
- ⚠️ 需要修改 driver 层
- ⚠️ 需要暴露 PiperContext 访问接口
- ⚠️ 回调中不应执行耗时操作

#### 实施复杂度
- **代码量**: ~150 行
- **修改文件**:
  - `crates/piper-driver/src/pipeline.rs` (~50 行)
  - `crates/piper-driver/src/state.rs` (~30 行)
  - `crates/piper-driver/src/piper.rs` (~20 行)
  - `crates/piper-tools/src/lib.rs` (~50 行)
- **测试工作量**: 1-2 天
- **总工作量**: 2-3 天

---

### 方案 B: 可观测性模式（最优雅 ⭐⭐⭐）

#### 设计概述
引入"可观测性模式"概念，允许切换到录制模式，自动记录所有 CAN 帧。

#### 架构设计

```rust
// 1. 定义可观测性配置
pub enum ObservabilityMode {
    /// 正常模式（默认）
    Normal,
    /// 录制模式（记录所有 CAN 帧）
    Recording(Arc<Mutex<PiperRecording>>),
    /// 回放模式（从文件读取 CAN 帧）
    Replay(PiperRecording),
}

// 2. 添加到 PipelineConfig
pub struct PipelineConfig {
    pub receive_timeout_ms: u64,
    pub frame_group_timeout_ms: u64,
    pub velocity_buffer_timeout_us: u64,

    /// 新增：可观测性模式
    pub observability: ObservabilityMode,
}

// 3. 在 rx_loop 中处理
pub fn rx_loop(
    mut rx: impl RxAdapter,
    ctx: Arc<PiperContext>,
    config: PipelineConfig,
    ...
) {
    loop {
        let frame = rx.receive()?;

        // === 可观测性处理 ===
        match &config.observability {
            ObservabilityMode::Normal => {
                // 正常模式：不做额外处理
            },
            ObservabilityMode::Recording(recording) => {
                // 录制模式：记录帧
                if let Ok(mut recording) = recording.try_lock() {
                    let timestamped = TimestampedFrame::new(
                        frame.timestamp_us(),
                        frame.raw_id(),
                        frame.data().to_vec(),
                        TimestampSource::Hardware,
                    );
                    recording.add_frame(timestamped);
                }
            },
            ObservabilityMode::Replay(replay) => {
                // 回放模式：从文件读取（稍后实现）
            },
        }

        // === 原有逻辑 ===
        parse_and_update_state(&frame, &ctx, &config, &mut state);
    }
}
```

#### 使用示例
```rust
// CLI 层使用
let recording = Arc::new(Mutex::new(
    PiperRecording::new(metadata)
));

let config = PipelineConfig {
    observability: ObservabilityMode::Recording(recording),
    ..Default::default()
};

let piper = PiperBuilder::new()
    .pipeline_config(config)
    .build()?;

// 使用 piper
// ... 所有 CAN 帧自动录制

// 保存录制
let recording = config.get_recording().unwrap();
recording.save("output.bin")?;
```

#### 优点
- ✅ 最优雅的设计
- ✅ 不需要额外的 API
- ✅ 支持扩展（其他可观测性功能）
- ✅ 配置驱动，易于使用

#### 缺点
- ⚠️ 需要修改 PipelineConfig
- ⚠️ 回放模式实现复杂
- ⚠️ 锁竞争（录制时需要 Mutex）

#### 实施复杂度
- **代码量**: ~200 行
- **修改文件**:
  - `crates/piper-driver/src/pipeline.rs` (~80 行)
  - `crates/piper-driver/src/state.rs` (~40 行)
  - `crates/piper-driver/src/piper.rs` (~30 行)
  - `crates/piper-tools/src/lib.rs` (~50 行)
- **测试工作量**: 2-3 天
- **总工作量**: 3-4 天

---

### 方案 C: 自定义 CAN Adapter（最灵活 ⭐⭐）

#### 设计概述
提供自定义 CAN adapter 的构建接口，允许用户在 adapter 层面录制。

#### 架构设计

```rust
// 1. 创建 RecordingAdapter 包装器
pub struct RecordingAdapter<A> {
    inner: A,
    recording: Arc<Mutex<PiperRecording>>,
}

impl<A: CanAdapter> CanAdapter for RecordingAdapter<A> {
    fn send(&mut self, frame: PiperFrame) -> Result<(), CanError> {
        self.inner.send(frame)
    }

    fn receive(&mut self) -> Result<PiperFrame, CanError> {
        let frame = self.inner.receive()?;

        // 录制接收的帧
        if let Ok(mut recording) = self.recording.try_lock() {
            let timestamped = TimestampedFrame::new(
                frame.timestamp_us(),
                frame.raw_id(),
                frame.data().to_vec(),
                TimestampSource::Hardware,
            );
            recording.add_frame(timestamped);
        }

        Ok(frame)
    }
}

// 2. 提供自定义 adapter 构建接口
impl PiperBuilder {
    /// 使用自定义 CAN adapter 构建
    pub fn build_with_adapter<A>(self, adapter: A) -> Result<Piper, DriverError>
    where
        A: CanAdapter + Send + 'static,
    {
        let config = self.pipeline_config.unwrap_or_default();
        Piper::new_dual_thread(adapter, Some(config))
            .map_err(DriverError::Can)
    }
}
```

#### 使用示例
```rust
// CLI 层使用
let recording = Arc::new(Mutex::new(
    PiperRecording::new(metadata)
));

// 创建基础 adapter
let base_adapter = SocketCanAdapter::new("can0")?;

// 包装为录制 adapter
let recording_adapter = RecordingAdapter {
    inner: base_adapter,
    recording,
};

// 使用自定义 adapter 构建
let piper = PiperBuilder::new()
    .build_with_adapter(recording_adapter)?;
```

#### 优点
- ✅ 完全灵活，用户可自定义
- ✅ 不修改 driver 层核心逻辑
- ✅ 可用于其他用途（过滤、修改帧）

#### 缺点
- ❌ **破坏性**：不适用于 `SplittableAdapter`
- ⚠️ 需要手动处理 adapter 配置
- ⚠️ API 不太友好

#### 实施复杂度
- **代码量**: ~100 行
- **修改文件**:
  - `crates/piper-driver/src/builder.rs` (~20 行)
  - `apps/cli/src/commands/record.rs` (~80 行)
- **测试工作量**: 1-2 天
- **总工作量**: 2-3 天

---

### 方案 D: 旁路监听模式（无侵入 ⭐⭐⭐⭐）

#### 设计概述
利用 SocketCAN 的多读特性，在主 adapter 之外创建额外的监听 adapter。

#### 架构设计

```rust
// CLI 层实现
pub async fn record_with_bypass(interface: &str) -> Result<PiperRecording> {
    // 1. 主 adapter 用于控制
    let piper = PiperBuilder::new()
        .interface(interface)
        .build()?;

    // 2. 旁路 adapter 用于监听（仅 SocketCAN 支持）
    #[cfg(target_os = "linux")]
    {
        let mut bypass = SocketCanAdapter::new(interface)?;
        let recording = Arc::new(Mutex::new(
            PiperRecording::new(metadata)
        ));

        // 3. 在后台线程中录制
        let recording_clone = recording.clone();
        let stop_signal = Arc::new(AtomicBool::new(false));

        spawn(move || {
            while !stop_signal.load(Ordering::Relaxed) {
                match bypass.receive_timeout(Duration::from_millis(100)) {
                    Ok(frame) => {
                        if let Ok(mut rec) = recording_clone.try_lock() {
                            rec.add_frame(/* ... */);
                        }
                    },
                    Err(CanError::Timeout) => continue,
                    Err(_) => break,
                }
            }
        });

        // 4. 录制期间使用 piper
        // ... 执行操作 ...

        // 5. 停止录制
        stop_signal.store(true, Ordering::Release);

        // 6. 保存录制
        let recording = Arc::try_unwrap(recording).unwrap();
        Ok(recording.into_inner())
    }

    #[cfg(not(target_os = "linux"))]
    {
        anyhow::bail!("旁路监听模式仅支持 Linux SocketCAN");
    }
}
```

#### 优点
- ✅ **零侵入**：不需要修改任何现有代码
- ✅ 简单直接，易于理解
- ✅ 不影响主性能

#### 缺点
- ❌ **仅限 Linux SocketCAN**
- ❌ GS-USB 不支持（硬件限制）
- ⚠️ 需要管理额外的线程

#### 实施复杂度
- **代码量**: ~150 行
- **修改文件**:
  - `apps/cli/src/commands/record.rs` (~150 行)
- **测试工作量**: 1 天
- **总工作量**: 1-2 天

---

### 方案 E: 混合录制（最实用 ⭐⭐⭐⭐⭐）

#### 设计概述
结合状态查询和高层命令，智能重建 CAN 帧序列。

#### 架构设计

```rust
pub async fn smart_record(duration: Duration) -> Result<PiperRecording> {
    let piper = PiperBuilder::new().build()?;
    let mut recording = PiperRecording::new(metadata);
    let start = SystemTime::now();

    while start.elapsed()? < duration {
        // 1. 读取状态（触发 CAN 通信）
        let position = piper.get_joint_position();

        // 2. 智能重建 CAN 帧
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

#### 优点
- ✅ 跨平台
- ✅ 简单易用
- ✅ 无需修改现有架构

#### 缺点
- ⚠️ 不是真正的原始 CAN 帧
- ⚠️ 时间戳可能不精确
- ⚠️ 可能丢失某些帧

#### 实施复杂度
- **代码量**: ~100 行
- **修改文件**:
  - `apps/cli/src/commands/record.rs` (~100 行)
- **测试工作量**: 1 天
- **总工作量**: 1 天

---

## 4. 方案对比

### 4.1 功能对比

| 方案 | 原始帧 | 跨平台 | 无侵入 | 实时性 | 复杂度 |
|------|--------|--------|--------|--------|--------|
| A: Driver 钩子 | ✅ | ✅ | ❌ | ⭐⭐⭐⭐ | 中 |
| B: 可观测性 | ✅ | ✅ | ❌ | ⭐⭐⭐⭐ | 中高 |
| C: 自定义 Adapter | ✅ | ✅ | ❌ | ⭐⭐⭐⭐ | 中 |
| D: 旁路监听 | ✅ | ❌ | ✅ | ⭐⭐⭐ | 低 |
| E: 智能重建 | ❌ | ✅ | ✅ | ⭐⭐⭐⭐ | 低 |

### 4.2 实施时间对比

| 方案 | 设计 | 编码 | 测试 | 总计 |
|------|------|------|------|------|
| A: Driver 钩子 | 0.5d | 1d | 1d | 2.5d |
| B: 可观测性 | 1d | 1.5d | 1.5d | 4d |
| C: 自定义 Adapter | 0.5d | 1d | 1d | 2.5d |
| D: 旁路监听 | 0.5d | 0.5d | 0.5d | 1.5d |
| E: 智能重建 | 0.5d | 0.5d | 0.5d | 1.5d |

### 4.3 维护成本对比

| 方案 | 代码维护 | 测试维护 | 文档 | 总体 |
|------|----------|----------|------|------|
| A: Driver 钩子 | 中 | 中 | 中 | 中 |
| B: 可观测性 | 中 | 中 | 低 | 中 |
| C: 自定义 Adapter | 低 | 低 | 低 | 低 |
| D: 旁路监听 | 低 | 低 | 低 | 低 |
| E: 智能重建 | 低 | 低 | 低 | 低 |

---

## 5. 推荐方案

### 5.1 短期方案（1-2 天）⭐⭐⭐⭐⭐

**方案 D + E 混合：旁路监听 + 智能重建**

**理由**:
1. ✅ **零侵入**：不需要修改 piper-sdk 核心
2. ✅ **快速实现**：1-2 天即可完成
3. ✅ **立即可用**：满足当前录制需求

**实施步骤**:
```rust
// 1. Linux SocketCAN 环境：使用旁路监听
#[cfg(target_os = "linux")]
pub async fn record_with_bypass(interface: &str, duration: Duration) -> Result<()> {
    // 创建旁路 adapter 用于监听
    let mut bypass = SocketCanAdapter::new(interface)?;
    let recording = Arc::new(Mutex::new(PiperRecording::new(...)));

    // 后台线程录制
    spawn(move || {
        while !stop_signal.load(Ordering::Relaxed) {
            if let Ok(frame) = bypass.receive_timeout(Duration::from_millis(100)) {
                recording.lock().unwrap().add_frame(frame);
            }
        }
    });

    // 主线程执行控制
    // ...
}

// 2. 其他平台（macOS/Windows）：使用智能重建
#[cfg(not(target_os = "linux"))]
pub async fn record_smart(interface: &str, duration: Duration) -> Result<()> {
    let piper = PiperBuilder::new().interface(interface).build()?;

    while elapsed < duration {
        let state = piper.get_joint_position();
        // 重建 CAN 帧...
    }
}
```

**优点**:
- 快速解决当前问题
- 不阻塞架构改进
- 立即可用

**缺点**:
- 旁路监听仅限 Linux
- 智能重建不是真正的原始帧

---

### 5.2 长期方案（1-2 周）⭐⭐⭐⭐

**方案 A: Driver 层录制钩子**

**理由**:
1. ✅ **架构清晰**：符合分层设计原则
2. ✅ **可扩展**：支持多种可观测性功能
3. ✅ **跨平台**：所有平台统一方案
4. ✅ **真实帧**：录制真正的原始 CAN 帧

**实施路径**:
```
阶段 1：基础架构（1 天）
├─ 定义 FrameCallback trait
├─ 在 PiperContext 添加回调列表
└─ 添加注册接口

阶段 2：实现钩子（0.5 天）
├─ 在 rx_loop 中调用回调
└─ 在 tx_loop 中调用回调（可选）

阶段 3：工具函数（1 天）
├─ 实现录制回调
├─ 实现回放功能
└─ 添加 CLI 集成

阶段 4：测试验证（0.5 天）
├─ 单元测试
├─ 集成测试
└─ 性能测试
```

**代码示例**:
```rust
// 1. 定义回调
pub struct RecordingCallback {
    recording: Arc<Mutex<PiperRecording>>,
}

impl FrameCallback for RecordingCallback {
    fn on_frame_received(&self, frame: &PiperFrame) {
        let mut rec = self.recording.lock().unwrap();
        rec.add_frame(TimestampedFrame::from(frame));
    }
}

// 2. 注册回调
let piper = PiperBuilder::new().build()?;
let callback = Arc::new(RecordingCallback { ... });
piper.context().add_frame_callback(callback);

// 3. 正常使用
// ... 所有 CAN 帧自动录制 ...
```

**优点**:
- 长期可维护
- 架构优雅
- 功能完整

**缺点**:
- 需要修改核心代码
- 实施时间较长

---

### 5.3 终极方案（2-4 周）⭐⭐⭐⭐⭐

**方案 B: 可观测性模式**

**理由**:
1. ✅ **最优雅**：配置驱动，声明式
2. ✅ **最灵活**：支持多种可观测性
3. ✅ **最完整**：录制、回放、调试、监控

**可观测性功能**:
```rust
pub enum ObservabilityMode {
    /// 正常模式
    Normal,

    /// 录制模式
    Recording(RecordingConfig),

    /// 回放模式
    Replay(ReplayConfig),

    /// 监控模式（统计、分析）
    Monitor(MonitorConfig),

    /// 调试模式（详细日志）
    Debug(DebugConfig),
}
```

**未来扩展**:
```rust
// 性能分析
piper.set_observability(ObservabilityMode::Profile);

// 数据包分析
piper.set_observability(ObservabilityMode::PacketCapture);

// 实时可视化
piper.set_observability(ObservabilityMode::Visualization);
```

---

## 6. 实施建议

### 6.1 阶段性实施计划

#### 第一阶段（1-2 天，立即开始）
**目标**: 快速解决当前问题

**实施**: 方案 D + E 混合
- Linux: 旁路监听模式
- macOS/Windows: 智能重建模式

**产出**:
- ✅ 立即可用的录制功能
- ✅ 跨平台支持
- ✅ 零架构修改

**代码位置**:
```
apps/cli/src/commands/
├── record_bypass.rs    # Linux 旁路监听
└── record_smart.rs     # 其他平台智能重建
```

#### 第二阶段（1 周，短期目标）
**目标**: 实现核心录制钩子

**实施**: 方案 A - Driver 层录制钩子

**产出**:
- ✅ FrameCallback trait
- ✅ 录制回调实现
- ✅ CLI 集成

**代码位置**:
```
crates/piper-driver/src/
├── callback.rs         # FrameCallback trait 定义
├── piper.rs            # 添加回调注册接口
└── pipeline.rs         # 在 rx_loop 中触发回调

crates/piper-tools/src/
└── recording_callback.rs  # 录制回调实现
```

#### 第三阶段（1-2 周，长期目标）
**目标**: 完整的可观测性框架

**实施**: 方案 B - 可观测性模式

**产出**:
- ✅ ObservabilityMode 枚举
- ✅ 录制/回放/监控模式
- ✅ 配置化使用
- ✅ 完整文档

**代码位置**:
```
crates/piper-driver/src/
├── observability.rs     # 可观测性模式定义
├── config.rs            # 扩展 PipelineConfig
└── pipeline.rs          # 处理不同模式

crates/piper-tools/src/
├── recording.rs         # 录制工具
├── replay.rs            # 回放工具
└── monitor.rs           # 监控工具
```

### 6.2 风险评估

#### 技术风险

| 风险 | 影响 | 概率 | 缓解措施 |
|------|------|------|----------|
| 回调性能下降 | 中 | 低 | 优化回调逻辑，异步处理 |
| 锁竞争 | 中 | 中 | 使用无锁队列或消息通道 |
| 线程安全 | 高 | 低 | 充分测试，使用 Arc/Mutex |
| 跨平台兼容 | 中 | 低 | 提供平台特定实现 |

#### 项目风险

| 风险 | 影响 | 概率 | 缓解措施 |
|------|------|------|----------|
| 时间超期 | 低 | 中 | 分阶段实施，优先实现核心功能 |
| 接口不稳定 | 中 | 低 | 充分测试，版本化 API |
| 维护成本 | 中 | 低 | 清晰文档，代码注释 |

---

## 7. 详细实施指南（方案 A）

### 7.1 第一步：定义 FrameCallback trait

**文件**: `crates/piper-driver/src/callback.rs` (新建)

```rust
//! CAN 帧回调 trait
//!
//! 提供用户自定义 CAN 帧处理的能力

use crate::pipeline::PiperFrame;
use std::sync::Arc;

/// CAN 帧回调 trait
///
/// 允许用户在接收到 CAN 帧时执行自定义逻辑。
///
/// # Thread Safety
/// 回调方法会在 RX 线程中被调用，因此：
/// - 必须是 `Send + Sync`
/// - 必须快速返回（避免阻塞 RX 线程）
/// - 不应执行耗时操作（如 I/O、大量计算）
///
/// # Example
///
/// ```no_run
/// use piper_driver::callback::FrameCallback;
/// use piper_driver::PiperFrame;
///
/// struct MyCallback;
///
/// impl FrameCallback for MyCallback {
///     fn on_frame_received(&self, frame: &PiperFrame) {
///         println!("Received frame: ID=0x{:03X}", frame.raw_id());
///     }
/// }
/// ```
pub trait FrameCallback: Send + Sync {
    /// 帧接收回调
    ///
    /// # 参数
    /// - `frame`: 接收到的 CAN 帧（只读引用）
    ///
    /// # 注意
    /// - 此方法在 RX 线程中调用
    /// - 必须快速返回，避免阻塞接收循环
    /// - 不应在回调中执行耗时操作
    fn on_frame_received(&self, frame: &PiperFrame);
}

/// 录制回调实现
///
/// 将接收到的 CAN 帧录制到 PiperRecording。
pub struct RecordingCallback {
    /// 录制数据（共享）
    pub recording: Arc<std::sync::Mutex<piper_tools::PiperRecording>>,
}

impl FrameCallback for RecordingCallback {
    fn on_frame_received(&self, frame: &PiperFrame) {
        // 快速获取锁
        if let Ok(mut recording) = self.recording.try_lock() {
            // 转换为 TimestampedFrame
            let timestamped = piper_tools::TimestampedFrame::new(
                frame.timestamp_us(),
                frame.raw_id(),
                frame.data().to_vec(),
                piper_tools::TimestampSource::Hardware,
            );

            // 添加到录制
            recording.add_frame(timestamped);
        }
        // 如果获取锁失败，跳过此帧（避免阻塞）
    }
}
```

### 7.2 第二步：修改 PiperContext

**文件**: `crates/piper-driver/src/state.rs`

```rust
// 在 PiperContext 中添加

use crate::callback::FrameCallback;
use std::sync::Arc;

pub struct PiperContext {
    // ... 现有字段 ...

    /// 帧回调列表
    frame_callbacks: Vec<Arc<dyn FrameCallback>>,
}

impl PiperContext {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            // ... 现有字段初始化 ...
            frame_callbacks: Vec::new(),
        })
    }

    /// 添加帧回调
    ///
    /// # Example
    ///
    /// ```no_run
    /// use piper_driver::Piper;
    ///
    /// let piper = PiperBuilder::new().build()?;
    /// let callback = Arc::new(MyCallback);
    /// piper.context().add_frame_callback(callback);
    /// ```
    pub fn add_frame_callback(&self, callback: Arc<dyn FrameCallback>) {
        // 注意：需要内部可变性
        // 这里使用 unsafe 或重新设计结构
        // 建议使用 Arc<Mutex<Vec<...>> 或类似机制
    }
}
```

**改进建议**:
```rust
// 更好的设计：使用 Mutex 保护回调列表

pub struct PiperContext {
    callbacks: Arc<Mutex<Vec<Arc<dyn FrameCallback>>>>,
    // ... 其他字段 ...
}

impl PiperContext {
    pub fn add_callback(&self, callback: Arc<dyn FrameCallback>) {
        let mut callbacks = self.callbacks.lock().unwrap();
        callbacks.push(callback);
    }

    pub fn get_callbacks(&self) -> Vec<Arc<dyn FrameCallback>> {
        self.callbacks.lock().unwrap().clone()
    }
}
```

### 7.3 第三步：在 rx_loop 中触发回调

**文件**: `crates/piper-driver/src/pipeline.rs`

```rust
pub fn rx_loop(
    mut rx: impl RxAdapter,
    ctx: Arc<PiperContext>,
    config: PipelineConfig,
    is_running: Arc<AtomicBool>,
    metrics: Arc<PiperMetrics>,
) {
    // ... 现有初始化 ...

    loop {
        // ... 运行检查 ...

        // 1. 接收 CAN 帧
        let frame = match rx.receive() {
            Ok(frame) => frame,
            Err(CanError::Timeout) => { /* ... */; continue; },
            Err(e) => { /* ... */; break; },
        };

        // 2. === 新增：触发所有帧回调 ===
        {
            let callbacks = ctx.get_callbacks();
            for callback in callbacks.iter() {
                callback.on_frame_received(&frame);
            }
            // 注意：这里不使用 ? 或 unwrap，避免单个回调失败影响整体
        }

        // 3. 原有的解析逻辑
        parse_and_update_state(&frame, &ctx, &config, &mut state);
    }
}
```

### 7.4 第四步：CLI 集成

**文件**: `apps/cli/src/commands/record.rs`

```rust
pub async fn execute(&self, _config: &OneShotConfig) -> Result<()> {
    use piper_driver::PiperBuilder;
    use piper_driver::callback::{FrameCallback, RecordingCallback};
    use piper_tools::{PiperRecording, RecordingMetadata};
    use std::sync::Arc;

    println!("⏳ 连接到机器人...");

    // 1. 创建录制
    let interface_str = self.interface.as_deref().unwrap_or("can0");
    let metadata = RecordingMetadata::new(interface_str.to_string(), 1_000_000);
    let recording = Arc::new(std::sync::Mutex::new(
        PiperRecording::new(metadata)
    ));

    // 2. 创建录制回调
    let callback = Arc::new(RecordingCallback {
        recording: recording.clone(),
    }) as Arc<dyn FrameCallback>;

    // 3. 创建并配置 Piper
    let piper = PiperBuilder::new()
        .interface(interface_str)
        .build()?;

    // 4. 注册回调
    piper.context().add_callback(callback);

    println!("✅ 已连接，开始录制...");

    // 5. 执行操作（自动录制）
    let start = std::time::Instant::now();
    let duration = Duration::from_secs(self.duration);

    while start.elapsed() < duration {
        // 触发 CAN 通信
        let _position = piper.get_joint_position();

        // 进度显示
        print!("\r录制中: {:.1}s / {}s",
            start.elapsed().as_secs_f64(),
            duration.as_secs_f64()
        );
        std::io::stdout().flush().ok();

        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    println!("\n✅ 录制完成");

    // 6. 保存录制
    let recording = Arc::try_unwrap(recording).unwrap();
    let recording = recording.into_inner();
    recording.save(&self.output)?;

    println!("✅ 保存完成");

    Ok(())
}
```

---

## 8. 测试策略

### 8.1 单元测试

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_callback_registration() {
        let ctx = PiperContext::new();
        let callback = Arc::new(TestCallback);

        ctx.add_callback(callback.clone());

        let callbacks = ctx.get_callbacks();
        assert_eq!(callbacks.len(), 1);
    }

    #[test]
    fn test_recording_callback() {
        let recording = Arc::new(Mutex::new(
            PiperRecording::new(metadata)
        ));
        let callback = RecordingCallback { recording };

        // 模拟接收帧
        let frame = create_test_frame();
        callback.on_frame_received(&frame);

        // 验证已录制
        let rec = recording.lock().unwrap();
        assert_eq!(rec.frame_count(), 1);
    }
}
```

### 8.2 集成测试

```rust
#[tokio::test]
async fn test_record_with_callback() {
    // 1. 创建 Piper 并注册回调
    let recording = Arc::new(Mutex::new(PiperRecording::new(...)));
    let callback = RecordingCallback { recording };

    let piper = PiperBuilder::new().build()?;
    piper.context().add_callback(callback);

    // 2. 执行操作
    let _ = piper.get_joint_position();

    // 3. 验证录制
    let rec = recording.lock().unwrap();
    assert!(rec.frame_count() > 0);
}
```

### 8.3 性能测试

```rust
#[tokio::test]
async fn benchmark_callback_overhead() {
    // 测试回调对性能的影响

    // 无回调模式
    let start = Instant::now();
    let piper1 = PiperBuilder::new().build().unwrap();
    for _ in 0..1000 {
        let _ = piper1.get_joint_position();
    }
    let duration_no_callback = start.elapsed();

    // 有回调模式
    let callback = Arc::new(NullCallback); // 空回调
    let piper2 = PiperBuilder::new().build().unwrap();
    piper2.context().add_callback(callback);

    let start = Instant::now();
    for _ in 0..1000 {
        let _ = piper2.get_joint_position();
    }
    let duration_with_callback = start.elapsed();

    // 性能影响应 <5%
    assert!(
        duration_with_callback < duration_no_callback * 1.05,
        "Callback overhead too high"
    );
}
```

---

## 9. 文档计划

### 9.1 用户文档

**录制功能指南** (`docs/recording.md`):
```markdown
# CAN 帧录制指南

## 概述
Piper SDK 支持录制所有 CAN 总线通信，用于离线分析和回放。

## 使用方法

### 1. 基本录制
\`\`\`bash
piper-cli record --output test.bin --duration 10
\`\`\`

### 2. 编程接口
\`\`\`rust
use piper_driver::callback::RecordingCallback;

let callback = RecordingCallback::new(...);
piper.context().add_callback(callback);
\`\`\`

## 注意事项
- 回调必须在 RX 线程中快速返回
- 录制大量数据时注意内存使用
- ...
```

### 9.2 API 文档

**FrameCallback trait**:
```rust
/// CAN 帧回调 trait
///
/// # Examples
///
/// ## 录制 CAN 帧
///
/// \`\`\`no_run
/// use piper_driver::callback::FrameCallback;
///
/// struct Recorder {
///     file: std::fs::File,
/// }
///
/// impl FrameCallback for Recorder {
///     fn on_frame_received(&self, frame: &PiperFrame) {
///         writeln!(self.file, "{:?}", frame).ok();
///     }
/// }
/// \`\`\`
pub trait FrameCallback: Send + Sync {
    /// 帧接收回调
    fn on_frame_received(&self, frame: &PiperFrame);
}
```

### 9.3 架构文档

**可观测性架构** (`docs/architecture/observability.md`):
```markdown
# 可观测性架构设计

## 概述
Piper SDK 提供了完整的可观测性框架，支持录制、回放、监控等功能。

## 架构
\`\`\`
┌─────────────────────────────────────┐
│         用户代码（CLI/应用）           │
├─────────────────────────────────────┤
│   FrameCallback (用户自定义逻辑)      │
│   └─> on_frame_received()            │
├─────────────────────────────────────┤
│      PiperContext (回调管理)          │
│   └─> callbacks: Vec<Arc<Callback>>  │
├─────────────────────────────────────┤
│         rx_loop (触发回调)            │
│   └─> for callback in callbacks      │
│       callback.on_frame_received()   │
└─────────────────────────────────────┘
\`\`\`
```

---

## 10. 总结

### 10.1 问题回顾

**核心问题**: 无法在 CLI 层访问原始 CAN 帧

**根本原因**:
1. 分层架构导致的依赖限制
2. CAN adapter 被消费并移动到 IO 线程
3. 缺少钩子机制

### 10.2 推荐方案

| 时间框架 | 方案 | 目标 |
|---------|------|------|
| **短期（1-2天）** | 方案 D+E: 旁路监听+智能重建 | 快速解决当前问题 |
| **中期（1周）** | 方案 A: Driver 层录制钩子 | 实现核心录制功能 |
| **长期（2-4周）** | 方案 B: 可观测性模式 | 完整的可观测性框架 |

### 10.3 关键收益

实施后的能力：
- ✅ 真实的 CAN 帧录制
- ✅ 完整的回放功能
- ✅ 性能分析工具
- ✅ 调试和监控能力
- ✅ 可扩展的架构

### 10.4 下一步行动

**立即行动**（本周）:
1. 实现方案 D+E 混合
2. 更新 CLI 录制命令
3. 编写用户文档
4. 进行集成测试

**短期目标**（下周）:
1. 设计方案 A 的详细接口
2. 实现 FrameCallback trait
3. 修改 driver 层
4. CLI 集成测试

**长期规划**（下月）:
1. 实现可观测性模式
2. 添加监控和分析工具
3. 完善文档和示例
4. 性能优化和测试

---

**报告作者**: Claude Code
**日期**: 2026-01-27
**版本**: v1.0
**许可证**: MIT OR Apache-2.0
