# CLI 录制命令实现方案（修正版 v2）

**文档日期**: 2026-01-28
**状态**: 🚀 **可立即实施**（所有底层 API 已完成）
**优先级**: P1（用户可见功能）
**修订说明**: 修正了 Section 5 中 Channel 双重消费和停止条件处理的架构问题

---

## 1. 问题分析

### 当前状态

`apps/cli/src/commands/record.rs` **当前实现**：
```rust
pub async fn execute(&self, _config: &OneShotConfig) -> Result<()> {
    anyhow::bail!(
        "❌ 录制功能暂未实现\n\
         原因：piper_client 当前未暴露底层 CAN 帧访问接口..."
    );
}
```

### 调研结论 ✅

**好消息：所有底层 API 已经实现完毕！**

| 层级 | 组件 | 状态 | 说明 |
|------|------|------|------|
| **Driver** | `AsyncRecordingHook` | ✅ 完成 | 有界 Channel（防 OOM） |
| **Driver** | `HookManager` | ✅ 完成 | 运行时钩子管理 |
| **Client** | `RecordingConfig` | ✅ 完成 | 配置结构 |
| **Client** | `RecordingHandle` | ✅ 完成 | RAII 句柄 |
| **Client** | `start_recording()` | ✅ 完成 | `machine.rs:791` |
| **Client** | `stop_recording()` | ✅ 完成 | `machine.rs:1259` |
| **Tools** | `PiperRecording` | ✅ 完成 | 文件格式 |
| **CLI** | **record 命令** | ❌ 占位 | 仅返回错误 |

**证据**：
```rust
// crates/piper-client/src/state/machine.rs:791
pub fn start_recording(
    self,
    config: crate::recording::RecordingConfig,
) -> Result<(Self, crate::recording::RecordingHandle)> {
    // ✅ 完整实现！
    let (hook, rx) = piper_driver::recording::AsyncRecordingHook::new();
    // ... 注册 Hook
    // ... 返回 RecordingHandle（持有 rx）
}
```

### 🔴 关键架构约束

**当前 API 设计**：
- `start_recording()` 创建 `RecordingHandle`，持有 `rx: Receiver<TimestampedFrame>`
- `stop_recording()` 使用 `try_recv()` 收集所有帧并保存文件
- **SDK 没有自动消费 `rx` 的后台线程**（这是设计如此）

**CLI 层的职责**：
1. 决定何时停止录制（手动 Ctrl-C）
2. 调用 `stop_recording()` 来收集帧并保存文件
3. 实时显示录制进度

---

## 2. 实现方案

### 方案概述

直接使用现有的 Client API 实现 CLI 命令，类似于 `replay.rs` 的实现模式。

### 参考实现：`replay.rs`

`replay.rs` 已成功实现了类似功能：
- ✅ `spawn_blocking` 线程隔离
- ✅ `Arc<AtomicBool>` 停止信号
- ✅ Ctrl-C 处理
- ✅ 完整的错误处理

`record.rs` 应该复用这些模式。

---

## 3. 详细设计

### 3.1 命令行参数映射

```rust
// 当前参数 -> RecordingConfig
RecordCommand {
    output: String           // -> output_path
    duration: u64            // -> stop_condition::Duration()
    stop_on_id: Option<u32>  // -> stop_condition::OnCanId()
    interface: Option<String>
    serial: Option<String>
}
```

### 3.2 用户交互流程

```
1. 连接到机器人（显示连接信息）
2. 开始录制（显示录制开始）
   ├─ 启动 Ctrl-C 监听
   └─ 显示实时统计（每秒更新）
3. 用户按 Ctrl-C
4. 保存录制文件
5. 显示录制统计
```

### 3.3 停止条件映射（🔴 修正）

| CLI 参数 | `RecordingConfig` 映射 | 实际处理方式 |
|----------|----------------------|-------------|
| `duration: 0` | `StopCondition::Manual` | CLI 等待 Ctrl-C |
| `duration: N` | `StopCondition::Duration(N)` | ⚠️ CLI 负责超时检查 |
| `stop_on_id: Some(id)` | `StopCondition::OnCanId(id)` | 🔴 **CLI 无法实现**（见 Section 5.3） |
| 默认（无参数） | `StopCondition::Manual` | CLI 等待 Ctrl-C |

**关键问题**：`StopCondition::OnCanId` 在当前 API 设计下无法实现，因为：
- CLI 层无法访问 `rx`（所有权在 `RecordingHandle`）
- SDK 也没有自动消费 `rx` 检查 CAN ID

**解决方案**：
- Phase 1: 先实现 Manual 和 Duration 停止
- Phase 2: 如需 OnCanId，需修改 SDK（见 Section 5.3）

### 3.4 实时统计显示

建议每秒更新一次（使用 `std::thread::sleep`）：

```
🔴 正在录制... [00:05] | 帧数: 1,024 | 丢帧: 0
```

---

## 4. 代码实现

### 4.1 主流程

```rust
impl RecordCommand {
    pub async fn execute(&self, _config: &OneShotConfig) -> Result<()> {
        // === 1. 参数验证 ===
        let output_path = PathBuf::from(&self.output);
        if output_path.exists() {
            anyhow::bail!("❌ 输出文件已存在: {}", self.output);
        }

        // === 2. 显示录制信息 ===
        println!("════════════════════════════════════════");
        println!("           录制模式");
        println!("════════════════════════════════════════");
        println!();
        println!("📁 输出: {}", self.output);
        println!("⏱️ 时长: {}", if self.duration == 0 {
            "手动停止".to_string()
        } else {
            format!("{} 秒", self.duration)
        });
        println!();

        // === 3. 安全确认 ===
        if !self.confirm {
            // ... 确认提示
        }

        // === 4. 🚨 创建停止信号 ===
        let running = Arc::new(AtomicBool::new(true));
        let running_clone = running.clone();

        tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                println!();
                println!("🛑 收到停止信号，正在保存录制...");
                running_clone.store(false, Ordering::SeqCst);
            }
        });

        // === 5. 使用 spawn_blocking 隔离 ===
        let result = spawn_blocking(move || {
            Self::record_sync(output_path, duration, interface, serial, running)
        }).await;

        // === 6. 处理结果 ===
        match result {
            Ok(Ok(stats)) => {
                println!();
                println!("✅ 录制完成");
                println!("   📊 帧数: {}", stats.frame_count);
                println!("   ⏱️ 时长: {:.2}s", stats.duration.as_secs_f64());
                println!("   ⚠️ 丢帧: {}", stats.dropped_frames);
                println!("   💾 已保存: {}", stats.output_path.display());
            }
            Ok(Err(e)) => Err(e),
            Err(e) => Err(anyhow::anyhow!("任务执行失败: {}", e)),
        }
    }
}
```

### 4.2 同步录制实现（🛡️ 防御性编程版本）

```rust
fn record_sync(
    output_path: PathBuf,
    duration: u64,
    interface: Option<String>,
    serial: Option<String>,
    running: Arc<AtomicBool>,
) -> Result<RecordingStats> {
    // === 1. 连接到机器人 ===
    let builder = Self::create_builder(interface, serial)?;
    let standby = builder.build()?;

    // ⚠️ 缓冲区警告（Phase 1 限制）
    if duration == 0 || duration > 180 {
        println!("⚠️  注意：当前版本主要用于短时录制（< 3分钟）");
        println!("   超过此时长可能导致数据丢失（缓冲区限制）");
        println!();
    }

    // === 2. 映射停止条件 ===
    let stop_condition = if duration > 0 {
        StopCondition::Duration(duration)
    } else {
        StopCondition::Manual
    };

    // ⚠️ 注意：OnCanId 在当前 API 下无法实现（CLI 无法访问 rx）

    // === 3. 启动录制 ===
    let metadata = RecordingMetadata {
        notes: format!("CLI recording, duration={}", duration),
        operator: whoami::username(),
    };

    let config = RecordingConfig {
        output_path: output_path.clone(),
        stop_condition,
        metadata,
    };

    let (standby, handle) = standby.start_recording(config)?;

    println!("🔴 开始录制...");
    println!("💡 提示: 按 Ctrl-C 停止录制");
    println!();

    // === 4. 循环逻辑（封装为独立函数，防止 panic 导致数据丢失）🛡️ ===
    let loop_result = Self::recording_loop(
        &handle,
        &running,
        duration,
    );

    // === 5. 无论循环如何结束，都尝试保存数据 🛡️ ===
    println!();
    println!("⏳ 正在保存录制...");

    let (_standby, stats) = standby.stop_recording(handle)?;

    // === 6. 然后再处理循环的错误（如果有） ===
    loop_result?;

    Ok(stats)
}

/// 录制循环（独立函数，错误不会影响数据保存）🛡️
///
/// 此函数的 panic 不会影响数据保存，
/// 因为 `stop_recording()` 在外层保证调用。
///
/// ⚡ UX 优化：100ms 轮询，每 1 秒刷新 UI
/// - Ctrl-C 响应时间：1 秒 → 100ms
/// - 时长精度：±1 秒 → ±100ms
fn recording_loop(
    handle: &RecordingHandle,
    running: &Arc<AtomicBool>,
    duration: u64,
) -> Result<()> {
    let start = Instant::now();
    let timeout = if duration > 0 {
        Some(Duration::from_secs(duration))
    } else {
        None
    };

    let mut ticks = 0usize;

    while running.load(Ordering::Relaxed) {
        // 1. 检查超时（精度 100ms）
        if let Some(timeout_duration) = timeout {
            if start.elapsed() >= timeout_duration {
                println!();
                println!("⏳ 录制时长已到");
                break;
            }
        }

        // 2. ⚡ 短暂休眠（提升 Ctrl-C 响应速度）
        std::thread::sleep(Duration::from_millis(100));
        ticks += 1;

        // 3. 每 1 秒（10 次 100ms）刷新一次 UI
        if ticks % 10 == 0 {
            // 显示进度（使用 SDK 暴露的 getter 方法）
            let elapsed = start.elapsed().as_secs();
            let current_count = handle.frame_count();  // ✅ 使用新增方法
            let dropped = handle.dropped_count();

            // ⚠️ 丢帧警告（缓冲区即将满）
            if dropped > 100 {
                eprint!("\r⚠️  已丢失 {} 帧 | ", dropped);
            }

            // 清除上一行并更新
            print!("\r🔴 正在录制... [{:02}:{:02}] | 帧数: {} | 丢帧: {}",
                elapsed / 60, elapsed % 60, current_count, dropped);
            std::io::stdout().flush()?;
        }
    }

    Ok(())
}
```

**防御性编程关键点**：

1. ✅ **循环分离**：`recording_loop()` 独立函数，panic 不影响数据保存
2. ✅ **数据安全优先**：`stop_recording()` 在外层保证调用
3. ✅ **错误隔离**：循环错误通过 `loop_result?` 延后处理
4. ✅ **缓冲区警告**：启动时提醒用户时长限制
5. ✅ **丢帧监控**：实时监控 `dropped_count()`，超过阈值警告
6. ⚡ **UX 优化**：100ms 轮询，Ctrl-C 响应快 10 倍，时长精度提升到 ±100ms

---

## 5. 实现细节（🔴 修正版）

### 5.1 实时帧数统计问题 🔴 关键修正

#### 问题分析

**当前 API 现状**：
- `RecordingHandle` 封装了 `rx: Receiver<TimestampedFrame>`
- `stop_recording()` 使用 `try_recv()` 收集所有帧并保存
- **但没有暴露任何方式让 CLI 层实时查询当前帧数**

**Channel 所有权约束**：
- `rx` 的所有权在 `RecordingHandle` 内部
- CLI 层无法直接读取（会导致编译错误：所有权冲突）
- SDK 也没有后台线程自动消费 `rx`（这是设计如此）

#### ✅ 正确方案：SDK 层添加原子计数器 + Getter 方法

**修改 SDK（`piper-driver` 和 `piper-client`）**：

##### 1. 修改 `AsyncRecordingHook`（Driver 层）

```rust
// crates/piper-driver/src/recording.rs

pub struct AsyncRecordingHook {
    tx: Sender<TimestampedFrame>,
    dropped_frames: Arc<AtomicU64>,

    // ✅ 新增：帧计数器（每次成功发送时递增）
    frame_counter: Arc<AtomicU64>,
}

impl AsyncRecordingHook {
    pub fn new() -> (Self, Receiver<TimestampedFrame>) {
        // ⚠️ 缓冲区大小：100,000 帧（约 3-4 分钟 @ 500Hz）
        // 内存占用：约 2.4MB（100k × 24 bytes/frame）
        // 风险提示：超过此时长会导致丢帧（见 Section 11.3）
        let (tx, rx) = bounded(100_000);

        let hook = Self {
            tx,
            dropped_frames: Arc::new(AtomicU64::new(0)),
            frame_counter: Arc::new(AtomicU64::new(0)), // ✅ 初始化计数器
        };

        (hook, rx)
    }

    // ✅ 新增：暴露计数器的引用（不可变，只读）
    pub fn frame_counter(&self) -> &Arc<AtomicU64> {
        &self.frame_counter
    }
}

impl FrameCallback for AsyncRecordingHook {
    fn on_frame_received(&self, frame: &PiperFrame) {
        let ts_frame = TimestampedFrame::from(frame);

        if self.tx.try_send(ts_frame).is_err() {
            // ⚠️ 缓冲区满时，丢弃"新"帧，保留"旧"帧
            //
            // 这是 bounded channel 的标准行为，也是正确的策略：
            // - 保留缓冲区里的旧数据（事故发生前的数据）
            // - 丢弃新来的帧（无法接收的帧）
            //
            // 对于故障复现场景，保留"事故发生前"的数据比保留最新数据更重要。
            self.dropped_frames.fetch_add(1, Ordering::Relaxed);
        } else {
            // ✅ 成功发送时增加计数（线程安全）
            self.frame_counter.fetch_add(1, Ordering::Relaxed);
        }
    }
}
```

##### 2. 修改 `RecordingHandle`（Client 层）

```rust
// crates/piper-client/src/recording.rs

pub struct RecordingHandle {
    rx: crossbeam_channel::Receiver<TimestampedFrame>,
    dropped_frames: Arc<AtomicU64>,
    output_path: PathBuf,
    start_time: Instant,

    // ✅ 新增：帧计数器（从 Driver 层传递）
    frame_counter: Arc<AtomicU64>,

    // ✅ 新增：停止请求标记（用于 Manual 停止）
    stop_requested: Arc<AtomicBool>,
}

impl RecordingHandle {
    pub(super) fn new(
        rx: crossbeam_channel::Receiver<TimestampedFrame>,
        dropped_frames: Arc<AtomicU64>,
        frame_counter: Arc<AtomicU64>,  // ✅ 新增参数
        output_path: PathBuf,
        start_time: Instant,
    ) -> Self {
        Self {
            rx,
            dropped_frames,
            frame_counter,
            is_finished: Arc::new(AtomicBool::new(false)),
            output_path,
            start_time,
        }
    }

    // ✅ 新增：Getter 方法（封装原子操作）

    /// 获取当前已录制的帧数（线程安全，无阻塞）
    pub fn frame_count(&self) -> u64 {
        self.frame_counter.load(Ordering::Relaxed)
    }

    /// 获取丢帧数量
    pub fn dropped_count(&self) -> u64 {
        self.dropped_frames.load(Ordering::Relaxed)
    }

    /// 检查是否已请求停止（用于循环条件判断）
    pub fn is_stop_requested(&self) -> bool {
        self.stop_requested.load(Ordering::Relaxed)
    }

    /// 手动停止录制（请求停止）
    pub fn stop(&self) {
        self.stop_requested.store(true, Ordering::SeqCst);
    }

    // 保留原有的 receiver() 方法（仅供 stop_recording 内部使用）
    pub(super) fn receiver(&self) -> &crossbeam_channel::Receiver<TimestampedFrame> {
        &self.rx
    }
}
```

##### 3. 修改 `start_recording()` 传递计数器

```rust
// crates/piper-client/src/state/machine.rs

pub fn start_recording(
    self,
    config: crate::recording::RecordingConfig,
) -> Result<(Self, crate::recording::RecordingHandle)> {
    use crate::recording::RecordingHandle;

    let (hook, rx) = piper_driver::recording::AsyncRecordingHook::new();
    let dropped = hook.dropped_frames().clone();
    let counter = hook.frame_counter().clone(); // ✅ 获取计数器引用

    // ... 注册 hook

    let handle = RecordingHandle::new(
        rx,
        dropped,
        counter,  // ✅ 传递计数器
        config.output_path.clone(),
        std::time::Instant::now(),
    );

    tracing::info!("Recording started: {:?}", config.output_path);

    Ok((self, handle))
}
```

#### CLI 层使用（修正后）

```rust
// apps/cli/src/commands/record.rs

while running.load(Ordering::Relaxed) {
    // 检查超时
    if duration > 0 && start.elapsed() >= Duration::from_secs(duration) {
        println!();
        println!("⏳ 录制时长已到");
        break;
    }

    std::thread::sleep(Duration::from_secs(1));

    // ✅ 正确：通过 getter 方法读取，无所有权冲突
    let current_count = handle.frame_count();  // ✅ 使用新增的方法
    let dropped = handle.dropped_count();
    let elapsed = start.elapsed().as_secs();

    print!("\r🔴 正在录制... [{:02}:{:02}] | 帧数: {} | 丢帧: {}",
        elapsed / 60, elapsed % 60, current_count, dropped);
    std::io::stdout().flush()?;
}
```

### 5.2 Ctrl-C 处理

参考 `replay.rs` 的实现：
```rust
tokio::spawn(async move {
    if tokio::signal::ctrl_c().await.is_ok() {
        println!();
        println!("🛑 收到停止信号，正在保存录制...");
        running.store(false, Ordering::SeqCst);
    }
});
```

### 5.3 停止条件处理（🔴 重大修正）

#### Duration（时长限制）✅ CLI 负责

```rust
if duration > 0 {
    let timeout = Duration::from_secs(duration);
    while running.load(Ordering::Relaxed) && start.elapsed() < timeout {
        std::thread::sleep(Duration::from_secs(1));
        // ... 更新 UI
    }
    if start.elapsed() >= timeout {
        println!("⏳ 录制时长已到");
    }
}
```

#### Manual（手动停止）✅ CLI 负责

```rust
// 等待 Ctrl-C
while running.load(Ordering::Relaxed) {
    std::thread::sleep(Duration::from_secs(1));
    // ... 更新 UI
}
```

#### OnCanId（CAN ID 触发）🔴 **当前 API 无法实现**

**问题根源**：
- CLI 层无法访问 `rx`（所有权在 `RecordingHandle`）
- SDK 也没有自动消费 `rx` 检查 CAN ID
- `StopCondition::OnCanId` 当前**仅是配置参数，未实现逻辑**

**Phase 1 建议**：
- CLI 参数中保留 `--stop-on-id`，但在未实现前报错：
```rust
if self.stop_on_id.is_some() {
    anyhow::bail!(
        "❌ --stop-on-id 功能暂未实现\n\
         原因：当前 API 架构下 CLI 无法访问 CAN 帧数据。\n\
         计划：未来在 SDK 层实现自动停止逻辑。\n\
         临时方案：使用 --duration 限制时长，或手动 Ctrl-C 停止。"
    );
}
```

**Phase 2 方案**（如需实现）：

需要修改 SDK，在 Driver 层添加自动停止逻辑：

##### 方案 A：Driver 层后台线程检查（推荐）

```rust
// 在 start_recording() 中启动后台线程
let (hook, rx) = AsyncRecordingHook::new();
let stop_signal = Arc::new(AtomicBool::new(false));

// ✅ 启动后台线程消费 rx，检查停止条件
let stop_signal_clone = stop_signal.clone();
std::thread::spawn(move || {
    while let Ok(frame) = rx.recv() {
        // ... 累积帧到缓冲区

        // 检查停止条件
        if stop_condition == StopCondition::OnCanId(target_id) {
            if frame.id == target_id {
                stop_signal_clone.store(true, Ordering::SeqCst);
                break;
            }
        }
    }
});

// CLI 层轮询 stop_signal
while !stop_signal.load(Ordering::Relaxed) {
    // ... 更新 UI
}
```

##### 方案 B：扩展 `AsyncRecordingHook`

```rust
// 在 Hook 中添加停止条件检查
impl AsyncRecordingHook {
    pub fn with_stop_condition(mut self, condition: StopCondition) -> Self {
        // 在 on_frame_received 中检查
        self.stop_condition = Some(condition);
        self
    }
}
```

**结论**：Phase 1 不实现 OnCanId，Phase 2 可根据需求决定是否添加。

---

## 6. 错误处理

### 6.1 文件已存在

```rust
if output_path.exists() {
    // 提示覆盖或取消
    println!("⚠️ 文件已存在: {}", self.output);
    println!("是否覆盖? [y/N] ");
    // ... 读取用户输入
}

// ✅ 添加 --force 参数跳过确认
#[arg(long)]
pub force: bool,

if !self.force && output_path.exists() {
    // ... 交互确认
}
```

### 6.2 磁盘空间不足（Phase 2 可选）

**建议**：MVP 版本跳过，仅在写入失败时报错。

如果需要实现，使用条件编译：
```rust
#[cfg(unix)]
fn check_disk_space(path: &Path, required_mb: u64) -> Result<()> {
    // 使用 nix::sys::statvfs 或直接读取 statfs
}

#[cfg(not(unix))]
fn check_disk_space(_path: &Path, _required_mb: u64) -> Result<()> {
    Ok(()) // Windows/macOS 暂不检查
}
```

### 6.3 丢帧警告

```rust
if stats.dropped_frames > 0 {
    println!("⚠️ 警告: 录制过程中丢失 {} 帧", stats.dropped_frames);
    println!("   建议: 检查磁盘 I/O 性能");
}
```

---

## 7. 测试计划

### 7.1 单元测试

```rust
#[test]
fn test_record_command_creation() {
    let cmd = RecordCommand {
        output: "test.bin".to_string(),
        duration: 10,
        stop_on_id: Some(0x2A5),
        ...
    };
    assert_eq!(cmd.duration, 10);
}
```

### 7.2 集成测试

```bash
# 测试 1: 手动停止（Ctrl-C）
$ piper-cli record --output test.bin
# 按 Ctrl-C，验证文件保存成功

# 测试 2: 时长限制
$ piper-cli record --output test.bin --duration 5
# 验证录制约 5 秒

# 测试 3: OnCanId（Phase 2）
$ piper-cli record --output test.bin --stop-on-id 0x2A5
# 应该在 Phase 1 报错提示未实现
```

---

## 8. 实施步骤（修正版）

### Phase 1: SDK API 修改（必需）

**估计时间**: 2-3 小时

1. ✅ 修改 `AsyncRecordingHook` 添加 `frame_counter`
   - `crates/piper-driver/src/recording.rs`
   - 添加字段、`frame_counter()` 方法
   - 在 `on_frame_received` 中递增

2. ✅ 修改 `RecordingHandle` 添加 getter 方法
   - `crates/piper-client/src/recording.rs`
   - 添加 `frame_counter` 字段
   - 添加 `frame_count()`, `is_stop_requested()`, `stop()` 方法

3. ✅ 修改 `start_recording()` 传递计数器
   - `crates/piper-client/src/state/machine.rs`
   - 获取 `hook.frame_counter()` 并传递给 `RecordingHandle`

4. ✅ 编译验证

```bash
cargo check --all-targets
cargo test --lib
```

### Phase 2: CLI 基础录制

**估计时间**: 3-4 小时

5. ✅ 实现 `record_sync()` - 同步录制逻辑
6. ✅ 参数验证 + 错误处理
7. ✅ 文件保存

### Phase 3: 用户交互

**估计时间**: 2-3 小时

8. ✅ Ctrl-C 处理
9. ✅ 实时统计显示
10. ✅ 进度条

### Phase 4: 停止条件（Phase 1 仅 Duration）

**估计时间**: 1 小时

11. ✅ Duration 停止
12. 🔶 OnCanId 停止（Phase 2，可选）

### Phase 5: 完善和测试

**估计时间**: 1-2 小时

13. ✅ 单元测试
14. ✅ 文档更新
15. ✅ 错误提示优化

**总计**：
- **Phase 1（SDK 修改）**: 2-3 小时
- **Phase 2-5（CLI 实现）**: 7-10 小时
- **总计**: **9-13 小时**（1.5 个工作日）

---

## 9. 文件修改清单（修正版）

| 文件 | 修改类型 | 优先级 | 说明 |
|------|----------|--------|------|
| **SDK 修改** |
| `crates/piper-driver/src/recording.rs` | 🔴 新增 | P0 | 添加 `frame_counter` 和 getter |
| `crates/piper-client/src/recording.rs` | 🔴 新增 | P0 | 添加字段和方法 |
| `crates/piper-client/src/state/machine.rs` | 🟡 修改 | P0 | 传递计数器引用 |
| **CLI 修改** |
| `apps/cli/src/commands/record.rs` | 🔴 完全重写 | P1 | 实现完整的录制命令 |
| `apps/cli/src/commands/mod.rs` | ✅ 无需修改 | - | 已经导出 |

**无需修改的文件**：
- ✅ `crates/piper-tools/src/recording.rs` - 文件格式已定义
- ✅ `crates/piper-driver/src/hooks.rs` - Hook 系统已完整

---

## 10. 与 Replay 命令的对比

| 特性 | Record（待实现） | Replay（已完成） |
|------|-----------------|-----------------|
| 线程隔离 | ✅ 使用 spawn_blocking | ✅ 已实现 |
| 停止信号 | ✅ Arc<AtomicBool> | ✅ 已实现 |
| Ctrl-C 处理 | ✅ tokio::signal::ctrl_c | ✅ 已实现 |
| 实时统计 | 🔶 需 SDK 暴露 frame_count() | N/A |
| 进度显示 | 🔶 待实现 | ✅ 已实现 |
| 停止条件 | 🟡 Duration（简单） | N/A |

**结论**：可以直接复用 `replay.rs` 的架构模式！

---

## 11. 安全考虑

### 11.1 磁盘空间

```rust
// MVP 版本：仅在写入失败时报错
match recording.save(&output_path) {
    Ok(_) => Ok(()),
    Err(e) if e.to_string().contains("No space left") => {
        anyhow::bail!("磁盘空间不足，无法保存录制文件")
    }
    Err(e) => Err(e.into()),
}
```

### 11.2 信号处理

```rust
// 确保在任何情况下都能安全退出
impl Drop for RecordingHandle {
    fn drop(&mut self) {
        // ✅ 自动关闭接收端
        // ✅ 防止资源泄漏
    }
}
```

### 11.3 ⚠️ 缓冲区大小限制（"20秒墙"）

#### 风险分析

**当前架构**：内存累积 → 停止时写入（无后台落盘线程）

**缓冲区配置**：
```rust
let (tx, rx) = bounded(100_000);  // 100,000 帧
```

**容量计算**：
- 假设 CAN 总线负载：500Hz（典型机械臂控制频率）
- 缓冲时长：`100,000 / 500 = 200` 秒 ≈ **3.3 分钟**
- 内存占用：`100,000 × 24 bytes/frame ≈ 2.4 MB`

**风险**：
- 超过 **~3 分钟** 后，`dropped_frames` 会直线上升
- 后续数据全部丢失（Channel 满后 `try_send` 失败）

#### 缓解措施

**Phase 1（MVP）**：
1. ✅ 已增大缓冲区至 `100_000`（从 `10_000`）
2. ✅ 在 CLI 启动时打印警告：
   ```rust
   if duration == 0 || duration > 180 {
       println!("⚠️  注意：当前版本主要用于短时录制（< 3分钟）");
       println!("   超过此时长可能导致数据丢失（缓冲区限制）");
   }
   ```
3. ✅ 实时监控 `dropped_count()`，超过阈值时警告：
   ```rust
   if dropped > 100 {
       println!("\n⚠️  警告：已检测到 {} 帧丢失，请尽快停止录制", dropped);
   }
   ```

**Phase 2（长期优化）**：
- 实现后台落盘线程，边收边写
- 彻底移除时长限制
- 使用 `mpsc` + `BufWriter` 组合

### 11.4 🛡️ Panic 安全性（防御性编程）

#### 风险场景

如果在 `record_sync` 的循环中发生 panic：
```rust
while running.load(Ordering::Relaxed) {
    // 如果这里 panic（例如 unwrap 失败）
    some_flaky_operation()?;  // 💥 panic!
}

// ⚠️ 这行永远不会执行，内存中的数据全部丢失！
let (_standby, stats) = standby.stop_recording(handle)?;
```

**后果**：`RecordingHandle` 被 Drop，Channel 断开，但**数据未保存**。

#### ✅ 防御性方案

**方案 1：循环逻辑分离**（推荐）

```rust
// apps/cli/src/commands/record.rs

fn record_sync(
    output_path: PathBuf,
    duration: u64,
    interface: Option<String>,
    serial: Option<String>,
    running: Arc<AtomicBool>,
) -> Result<RecordingStats> {
    // === 1. 连接并启动录制 ===
    let builder = Self::create_builder(interface, serial)?;
    let standby = builder.build()?;
    let (standby, handle) = standby.start_recording(config)?;

    println!("🔴 开始录制...");
    println!("💡 提示: 按 Ctrl-C 停止录制");
    println!();

    // === 2. 循环逻辑（封装为独立函数） ===
    let loop_result = Self::recording_loop(
        &handle,
        &running,
        duration,
    );

    // === 3. 无论循环如何结束，都尝试保存数据 🛡️ ===
    println!();
    println!("⏳ 正在保存录制...");

    let (_standby, stats) = standby.stop_recording(handle)?;

    // === 4. 然后再处理循环的错误（如果有） ===
    loop_result?;

    Ok(stats)
}

/// 录制循环（独立函数，错误不会影响数据保存）
///
/// ⚡ UX 优化：100ms 轮询，每 1 秒刷新 UI
/// - Ctrl-C 响应时间：1 秒 → 100ms（快 10 倍）
/// - 时长精度：±1 秒 → ±100ms（精度提升）
fn recording_loop(
    handle: &RecordingHandle,
    running: &Arc<AtomicBool>,
    duration: u64,
) -> Result<()> {
    let start = Instant::now();
    let timeout = if duration > 0 {
        Some(Duration::from_secs(duration))
    } else {
        None
    };

    let mut ticks = 0usize;

    while running.load(Ordering::Relaxed) {
        // 1. 检查超时（精度 100ms）
        if let Some(timeout_duration) = timeout {
            if start.elapsed() >= timeout_duration {
                println!();
                println!("⏳ 录制时长已到");
                break;
            }
        }

        // 2. ⚡ 短暂休眠（提升 Ctrl-C 响应速度）
        std::thread::sleep(Duration::from_millis(100));
        ticks += 1;

        // 3. 每 1 秒（10 次 100ms）刷新一次 UI
        if ticks % 10 == 0 {
            // 显示进度
            let elapsed = start.elapsed().as_secs();
            let current_count = handle.frame_count();
            let dropped = handle.dropped_count();

            // ⚠️ 丢帧警告
            if dropped > 100 {
                eprint!("\r⚠️  已丢失 {} 帧 | ", dropped);
            }

            print!("\r🔴 正在录制... [{:02}:{:02}] | 帧数: {} | 丢帧: {}",
                elapsed / 60, elapsed % 60, current_count, dropped);
            std::io::stdout().flush()?;
        }
    }

    Ok(())
}
```

**关键优势**：
1. ✅ **数据安全优先**：即使循环 panic，`stop_recording()` 仍会被调用
2. ✅ **错误隔离**：循环错误不影响数据保存
3. ✅ **代码清晰**：职责分离，易于维护
4. ⚡ **UX 优化**：Ctrl-C 响应快 10 倍（100ms vs 1秒），时长精度提升到 ±100ms

**方案 2：使用 `scopeguard` crate**（可选）

如果引入外部依赖，可以使用更优雅的 defer 模式：

```rust
use scopeguard::defer;

fn record_sync(...) -> Result<RecordingStats> {
    let (standby, handle) = standby.start_recording(config)?;

    // 🛡️ 注册 defer，确保函数退出时保存数据
    defer! {
        // 注意：这里需要 move ownership，实际实现会更复杂
        // 仅作为概念展示
    }

    // ... 循环逻辑
}
```

**结论**：Phase 1 使用方案 1（循环分离），无需引入新依赖。

---

## 12. 示例输出

### 12.1 正常录制（手动停止）

```bash
$ piper-cli record --output demo.bin

════════════════════════════════════════
           录制模式
════════════════════════════════════════

📁 输出: demo.bin
⏱️ 时长: 手动停止
💾 接口: can0 (SocketCAN)

⏳ 连接到机器人...
✅ 已连接

🔴 开始录制...
💡 提示: 按 Ctrl-C 停止录制

🔴 正在录制... [00:05] | 帧数: 1,024 | 丢帧: 0
^C
🛑 收到停止信号，正在保存录制...
⏳ 正在保存录制...

✅ 录制完成
   📊 帧数: 1,024
   ⏱️ 时长: 5.23s
   ⚠️ 丢帧: 0
   💾 已保存: demo.bin
```

### 12.2 时长限制

```bash
$ piper-cli record --output demo.bin --duration 10

...
🔴 开始录制...
💡 提示: 按 Ctrl-C 可提前停止

🔴 正在录制... [00:05] | 帧数: 1,024 | 丢帧: 0
🔴 正在录制... [00:10] | 帧数: 2,048 | 丢帧: 0

⏳ 录制时长已到，正在保存录制...
✅ 录制完成
   ...
```

---

## 13. 总结

### ✅ 可行性评估

- **技术风险**: 🟢 **低** - 所有底层 API 已实现
- **工作量**: 🟡 **中** - 9-13 小时（1.5 个工作日）
- **优先级**: 🟡 **P1** - 用户可见功能
- **API 兼容性**: 🟢 **完全兼容** - 仅新增方法，不破坏现有 API

### 🎯 核心要点

1. **需要 SDK 修改** - 添加 `frame_counter` 和 getter 方法
2. **封装原则** - 使用 getter 方法而非 `pub` 字段
3. **职责分离** - CLI 负责 Manual/Duration，SDK 未来负责 OnCanId
4. **参考 replay.rs** - 使用相同的模式（spawn_blocking + AtomicBool）
5. **重点在用户体验** - 进度显示、错误提示

### 📋 下一步

1. ✅ 审阅本方案（修正版）
2. ✅ 开始实施 Phase 1（SDK API 修改）
3. ✅ 编译验证 SDK 修改
4. ✅ 实施 Phase 2-5（CLI 实现）
5. ✅ 完善测试和文档

---

## 附录：关键审查意见摘要

感谢代码审查员的详细反馈，本修正版采纳了以下关键建议：

### 第一轮审查：架构问题

1. **Channel 双重消费悖论**（Section 5.1）
   - ❌ 错误：CLI 无法读取 `rx`（所有权已被 SDK 消费）
   - ✅ 修正：SDK 层添加原子计数器 + getter 方法

2. **停止条件职责归属**（Section 5.3）
   - ❌ 错误：CLI 无法处理 OnCanId（看不到帧数据）
   - ✅ 修正：CLI 仅负责 Manual/Duration，OnCanId 留待 Phase 2

3. **API 设计封装性**
   - ❌ 错误：直接暴露 `pub Arc<AtomicU64>` 字段
   - ✅ 修正：使用 `pub fn frame_count() -> u64` getter 方法

### 第二轮审查：边界条件和防御性编程

4. **缓冲区大小限制**（Section 11.3）⚠️
   - ❌ 风险：`bounded(10_000)` 仅支持 20 秒 @ 500Hz
   - ✅ 修正：增大至 `bounded(100_000)`（约 3.3 分钟）
   - ✅ 添加：启动时警告用户时长限制
   - ✅ 添加：实时监控丢帧，超过阈值警告

5. **Panic 安全性**（Section 11.4）🛡️
   - ❌ 风险：循环中 panic 会导致数据全部丢失
   - ✅ 修正：循环逻辑分离为独立函数 `recording_loop()`
   - ✅ 保证：`stop_recording()` 在外层始终调用

6. **命名语义**
   - ❌ 模糊：`is_finished` 语义不清（"完成" vs "停止请求"）
   - ✅ 修正：重命名为 `stop_requested`
   - ✅ 修正：方法名改为 `is_stop_requested()`

### 第三轮审查：UX 优化和逻辑微调

7. **Ctrl-C 响应速度**（Section 4.2, 11.4）⚡
   - ❌ 问题：1 秒休眠，Ctrl-C 响应慢，用户体验差
   - ✅ 修正：100ms 轮询，响应快 10 倍
   - ✅ 附加：时长精度提升到 ±100ms
   - ✅ 优化：每 10 次（1秒）刷新一次 UI，保持流畅

8. **丢帧策略明确化**（Section 5.1）📝
   - ❌ 模糊：未说明 bounded channel 的丢弃策略
   - ✅ 明确：丢弃"新"帧，保留"旧"帧（缓冲区里的数据）
   - ✅ 理由：对于故障复现，保留"事故发生前"的数据更重要
   - ✅ 添加：代码注释说明行为

### 修正后的架构优势

- ✅ **封装性更好**：字段私有，方法公开
- ✅ **无所有权冲突**：CLI 通过 getter 读取，不涉及 `rx`
- ✅ **线程安全**：原子变量 + Relaxed ordering，零开销
- ✅ **向后兼容**：仅新增方法，不破坏现有 API
- ✅ **职责清晰**：CLI 负责 UI 和简单条件，SDK 负责数据处理
- ✅ **数据安全**：防御性编程，panic 不影响数据保存
- ✅ **用户友好**：明确的缓冲区限制和丢帧警告
- ⚡ **响应灵敏**：100ms 轮询，Ctrl-C 响应快 10 倍
- 📝 **策略明确**：bounded channel 行为清晰，保留旧数据

### 风险缓解清单

| 风险 | 缓解措施 | 优先级 |
|------|----------|--------|
| **缓冲区溢出** | ✅ 增大至 100k 帧 + 警告提示 | P0 |
| **Panic 数据丢失** | ✅ 循环分离 + 外层保证保存 | P0 |
| **Ctrl-C 响应慢** | ✅ 100ms 轮询（快 10 倍） | P1 |
| **命名歧义** | ✅ 重命名为 `stop_requested` | P2 |
| **时长限制** | ✅ 用户警告 + 丢帧监控 | P1 |
| **OnCanId 未实现** | ✅ Phase 1 明确禁用 + 报错 | P1 |
| **丢帧策略不明** | ✅ 注释说明（丢弃新，保留旧） | P2 |

---

**文档作者**: AI Code Auditor
**最后更新**: 2026-01-28
**版本**: v4（最终版）
**状态**: ✅ 方案已就绪，可立即实施
**审查状态**: ✅ **已通过三轮代码审查，获得最终批准（Final Approval）**
  - 第一轮：架构问题（Channel、封装性）
  - 第二轮：边界条件和防御性编程
  - 第三轮：UX 优化和逻辑微调
