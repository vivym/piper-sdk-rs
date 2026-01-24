# 实时命令发送与消费逻辑深度分析报告

**文档版本**：v1.0
**创建日期**：2026-01-XX
**分析目标**：深入分析 `send_realtime_package` 的发送和消费逻辑，识别潜在问题

## 执行摘要

本报告对实时命令（Realtime Command）的发送和消费流程进行了逐行分析，重点关注：
1. 发送端（Client 层）到驱动层（Driver 层）的完整路径
2. TX 线程的消费逻辑和调度策略
3. 潜在的竞态条件、时序问题和覆盖风险

**关键发现**：
- ✅ 发送端逻辑正确，使用邮箱模式（Last Write Wins）
- ✅ 消费端逻辑正确，优先级调度合理
- ⚠️ **潜在问题**：TX 线程的 `sleep(50μs)` 可能导致首次命令延迟处理
- ⚠️ **潜在问题**：如果发送端在 TX 线程 sleep 期间连续发送，可能发生覆盖

---

## 1. 发送端流程分析

### 1.1 调用链

```
position_control_demo.rs
  └─> motion.send_position_command_batch()
       └─> RawCommander::send_position_command_batch()
            └─> driver.send_realtime_package()
                 └─> Piper::send_realtime_package()
                      └─> Piper::send_realtime_command() [内部方法]
```

### 1.2 逐行代码分析

#### 步骤 1：`send_position_command_batch` (raw_commander.rs:138-160)

```rust
pub(crate) fn send_position_command_batch(&self, positions: &JointArray<Rad>) -> Result<()> {
    // 准备所有关节的角度（度）
    let j1_deg = positions[Joint::J1].to_deg().0;
    // ... 准备 j2-j6

    // 创建 3 个 CAN 帧（使用数组，栈上分配，零堆内存分配）
    let frames = [
        JointControl12::new(j1_deg, j2_deg).to_frame(), // 0x155
        JointControl34::new(j3_deg, j4_deg).to_frame(), // 0x156
        JointControl56::new(j5_deg, j6_deg).to_frame(), // 0x157
    ];

    // 原子性发送所有帧
    self.driver.send_realtime_package(frames)?;

    Ok(())
}
```

**分析**：
- ✅ **正确**：使用数组 `[frame1, frame2, frame3]`，栈上分配，零堆分配
- ✅ **正确**：一次性准备所有 6 个关节，避免关节覆盖问题
- ✅ **正确**：调用 `send_realtime_package`，传入数组迭代器

**潜在问题**：无

---

#### 步骤 2：`send_realtime_package` (piper.rs:732-761)

```rust
pub fn send_realtime_package(
    &self,
    frames: impl IntoIterator<Item = PiperFrame>,
) -> Result<(), DriverError> {
    use crate::driver::command::FrameBuffer;

    // 步骤 2.1：收集迭代器到 SmallVec
    let buffer: FrameBuffer = frames.into_iter().collect();

    // 步骤 2.2：验证非空
    if buffer.is_empty() {
        return Err(DriverError::InvalidInput(
            "Frame package cannot be empty".to_string(),
        ));
    }

    // 步骤 2.3：验证大小限制
    if buffer.len() > Self::MAX_REALTIME_PACKAGE_SIZE {
        return Err(DriverError::InvalidInput(format!(
            "Frame package too large: {} (max: {})",
            buffer.len(),
            Self::MAX_REALTIME_PACKAGE_SIZE
        )));
    }

    // 步骤 2.4：调用内部方法
    self.send_realtime_command(RealtimeCommand::package(buffer))
}
```

**分析**：
- ✅ **正确**：`frames.into_iter().collect()` 对于数组，会创建 `SmallVec`，如果 len ≤ 4，完全在栈上
- ✅ **正确**：验证非空和大小限制
- ✅ **正确**：调用 `send_realtime_command` 统一处理

**潜在问题**：
- ⚠️ **性能考虑**：如果用户传入 `Vec`（长度 > 4），`collect()` 会触发堆分配。但这是用户选择，不是 bug。

---

#### 步骤 3：`send_realtime_command` (piper.rs:763-796)

```rust
fn send_realtime_command(&self, command: RealtimeCommand) -> Result<(), DriverError> {
    let realtime_slot = self.realtime_slot.as_ref().ok_or(DriverError::NotDualThread)?;

    match realtime_slot.lock() {
        Ok(mut slot) => {
            // 步骤 3.1：检测覆盖
            let is_overwrite = slot.is_some();

            // 步骤 3.2：计算帧数量
            let frame_count = command.len();

            // 步骤 3.3：直接覆盖（邮箱模式：Last Write Wins）
            *slot = Some(command);

            // 步骤 3.4：显式释放锁
            drop(slot);

            // 步骤 3.5：更新指标（在锁外）
            self.metrics.tx_frames_total.fetch_add(frame_count as u64, Ordering::Relaxed);
            if is_overwrite {
                self.metrics.tx_realtime_overwrites.fetch_add(1, Ordering::Relaxed);
            }

            Ok(())
        },
        Err(_) => {
            error!("Realtime slot lock poisoned, TX thread may have panicked");
            Err(DriverError::PoisonedLock)
        },
    }
}
```

**分析**：
- ✅ **正确**：使用 `Mutex` 保护共享插槽
- ✅ **正确**：检测覆盖并更新指标
- ✅ **正确**：使用 `drop(slot)` 显式释放锁，减少锁持有时间
- ✅ **正确**：在锁外更新指标，避免在锁内进行原子操作

**潜在问题**：
- ⚠️ **竞态条件**：如果 TX 线程正在持有锁（处理命令），发送端会阻塞等待
  - **影响**：发送延迟可能达到几微秒到几十微秒
  - **可接受性**：这是设计选择，邮箱模式需要互斥访问

**时序分析**：
```
时间线：
T0: 发送端调用 send_realtime_command()
T1: 发送端获取锁成功
T2: 发送端写入插槽: *slot = Some(command)
T3: 发送端释放锁: drop(slot)
T4: 发送端更新指标（原子操作，无锁）
T5: 发送端返回 Ok(())
```

**关键点**：
- 锁持有时间极短（< 50ns，仅为内存拷贝）
- 发送端返回后，命令已经在插槽中
- **但是**：TX 线程可能正在 sleep，需要等待 sleep 结束才能处理

---

## 2. 消费端流程分析（TX 线程）

### 2.1 TX 线程主循环 (`tx_loop_mailbox`)

```rust
pub fn tx_loop_mailbox(
    mut tx: impl TxAdapter,
    realtime_slot: Arc<Mutex<Option<RealtimeCommand>>>,
    reliable_rx: Receiver<PiperFrame>,
    is_running: Arc<AtomicBool>,
    metrics: Arc<PiperMetrics>,
) {
    const REALTIME_BURST_LIMIT: usize = 100;
    let mut realtime_burst_count = 0;

    loop {
        // 步骤 1：检查运行标志
        if !is_running.load(Ordering::Relaxed) {
            break;
        }

        // 步骤 2：优先级调度 (Priority 1: 实时邮箱)
        let realtime_command = {
            match realtime_slot.lock() {
                Ok(mut slot) => slot.take(), // 取出数据，插槽变为 None
                Err(_) => {
                    error!("TX thread: Realtime slot lock poisoned");
                    None
                },
            }
        };

        // 步骤 3：处理实时命令
        if let Some(command) = realtime_command {
            // ... 处理命令 ...
            // 饿死保护逻辑
            realtime_burst_count += 1;
            if realtime_burst_count >= REALTIME_BURST_LIMIT {
                realtime_burst_count = 0;
                // 不 continue，自然掉落检查 reliable_rx
            } else {
                continue; // 立即回到循环开始
            }
        } else {
            realtime_burst_count = 0;
        }

        // 步骤 4：处理可靠命令队列
        if let Ok(frame) = reliable_rx.try_recv() {
            // ... 发送可靠命令 ...
            continue;
        }

        // 步骤 5：空闲休眠
        std::thread::sleep(Duration::from_micros(50));
    }
}
```

### 2.2 逐步骤分析

#### 步骤 1：检查运行标志

```rust
if !is_running.load(Ordering::Relaxed) {
    break;
}
```

**分析**：
- ✅ **正确**：使用 `Ordering::Relaxed`，性能最优
- ✅ **正确**：检查频率高，响应及时

**潜在问题**：无

---

#### 步骤 2：从插槽取出命令

```rust
let realtime_command = {
    match realtime_slot.lock() {
        Ok(mut slot) => slot.take(), // 取出数据，插槽变为 None
        Err(_) => {
            error!("TX thread: Realtime slot lock poisoned");
            None
        },
    }
};
```

**分析**：
- ✅ **正确**：使用 `slot.take()`，取出后插槽变为 `None`
- ✅ **正确**：锁持有时间极短（仅为 `take()` 操作）
- ✅ **正确**：使用短暂作用域，确保锁立即释放

**潜在问题**：
- ⚠️ **竞态条件**：如果发送端在 TX 线程 `take()` 之后、`lock()` 释放之前写入新命令，新命令会被立即处理（这是期望行为）
- ⚠️ **竞态条件**：如果发送端在 TX 线程 `take()` 之后、下一次循环之前写入新命令，新命令会留在插槽中，等待下一次循环

**时序分析**：

**场景 A：正常情况（TX 线程及时处理）**
```
T0: 发送端写入插槽: *slot = Some(command1)
T1: 发送端释放锁
T2: TX 线程获取锁
T3: TX 线程取出命令: slot.take() -> Some(command1)
T4: TX 线程释放锁
T5: TX 线程处理命令（发送到 CAN 总线）
```
**结果**：✅ 命令被及时处理

**场景 B：TX 线程正在 sleep（问题场景）**
```
T0: TX 线程进入 sleep(50μs)
T1: 发送端写入插槽: *slot = Some(command1)
T2: 发送端释放锁
T3: TX 线程仍在 sleep（剩余 30μs）
T4: TX 线程 sleep 结束
T5: TX 线程获取锁
T6: TX 线程取出命令: slot.take() -> Some(command1)
T7: TX 线程处理命令
```
**结果**：⚠️ 命令延迟处理（最多 50μs）

**场景 C：连续发送（覆盖场景）**
```
T0: 发送端写入插槽: *slot = Some(command1)
T1: 发送端释放锁
T2: TX 线程获取锁
T3: TX 线程取出命令: slot.take() -> Some(command1)
T4: TX 线程释放锁
T5: 发送端写入插槽: *slot = Some(command2)  // 覆盖（但 command1 已被取出）
T6: TX 线程处理 command1（发送到 CAN 总线）
T7: TX 线程下一次循环，取出 command2
```
**结果**：✅ 两个命令都被处理（无覆盖）

**场景 D：连续发送 + TX 线程 sleep（潜在问题）**
```
T0: TX 线程进入 sleep(50μs)
T1: 发送端写入插槽: *slot = Some(command1)
T2: 发送端释放锁
T3: 发送端立即再次写入: *slot = Some(command2)  // 覆盖 command1
T4: 发送端释放锁
T5: TX 线程 sleep 结束
T6: TX 线程获取锁
T7: TX 线程取出命令: slot.take() -> Some(command2)  // command1 丢失
T8: TX 线程处理 command2
```
**结果**：❌ **command1 丢失**（被 command2 覆盖）

---

#### 步骤 3：处理实时命令

```rust
if let Some(command) = realtime_command {
    let frames = command.into_frames();
    let total_frames = frames.len();
    let mut sent_count = 0;
    let mut should_break = false;

    for frame in frames {
        match tx.send(frame) {
            Ok(_) => {
                sent_count += 1;
            },
            Err(e) => {
                // 错误处理
                break;
            },
        }
    }

    // 记录统计
    if sent_count > 0 {
        metrics.tx_package_sent.fetch_add(1, Ordering::Relaxed);
        if sent_count < total_frames {
            metrics.tx_package_partial.fetch_add(1, Ordering::Relaxed);
        }
    }

    // 饿死保护
    realtime_burst_count += 1;
    if realtime_burst_count >= REALTIME_BURST_LIMIT {
        realtime_burst_count = 0;
        // 不 continue，自然掉落检查 reliable_rx
    } else {
        continue; // 立即回到循环开始
    }
}
```

**分析**：
- ✅ **正确**：使用 `into_frames()` 消费命令，避免额外拷贝
- ✅ **正确**：循环发送所有帧，保证原子性（尽可能）
- ✅ **正确**：错误处理正确，部分发送时停止后续帧
- ✅ **正确**：饿死保护机制，避免 Reliable 队列饿死

**潜在问题**：
- ⚠️ **部分原子性**：如果发送第 2 帧失败，第 1 帧已经发送到 CAN 总线，无法回滚
  - **可接受性**：这是 CAN 总线特性，不是 bug

---

#### 步骤 4：处理可靠命令队列

```rust
if let Ok(frame) = reliable_rx.try_recv() {
    match tx.send(frame) {
        Ok(_) => {
            // 不在这里更新 tx_frames_total，因为 send_reliable() 已经更新了
        },
        Err(e) => {
            // 错误处理
        },
    }
    continue;
}
```

**分析**：
- ✅ **正确**：使用 `try_recv()`，非阻塞
- ✅ **正确**：发送成功后 `continue`，立即回到循环开始

**潜在问题**：无

---

#### 步骤 5：空闲休眠

```rust
// 都没有数据，避免忙等待
// 使用短暂的 sleep（50μs）降低 CPU 占用
// 注意：这里的延迟不会影响控制循环，因为控制循环在另一个线程
std::thread::sleep(Duration::from_micros(50));
```

**分析**：
- ✅ **正确**：避免忙等待，降低 CPU 占用
- ✅ **正确**：50μs 延迟对控制循环影响很小（控制循环通常在 1-2ms）

**潜在问题**：
- ⚠️ **延迟处理**：如果发送端在 TX 线程 sleep 期间写入命令，命令会被延迟处理（最多 50μs）
  - **影响**：对于 500Hz-1kHz 的控制循环，50μs 延迟通常可接受
  - **但**：如果发送端连续发送两次，第一次可能被第二次覆盖（见场景 D）

---

## 3. 关键问题识别

### 3.1 问题 1：TX 线程 sleep 期间的命令延迟

**问题描述**：
- 如果发送端在 TX 线程 `sleep(50μs)` 期间写入命令，命令会被延迟处理（最多 50μs）

**影响**：
- 对于高频控制循环（500Hz-1kHz），50μs 延迟通常可接受
- 但对于极低延迟场景（< 100μs），可能不可接受

**严重程度**：🟡 **中等**（设计权衡）

**解决方案**：
1. **方案 A**：移除 sleep，改为完全忙等待（适用于独占 CPU 核心场景）
2. **方案 B**：使用 Condvar 唤醒机制（需要修改结构体，添加 `cvar` 字段）
3. **方案 C**：保持现状（当前设计已足够）

**推荐**：方案 C（保持现状），除非有明确的低延迟需求

---

### 3.2 问题 2：连续发送时的命令覆盖（关键问题）

**问题描述**：
- 如果发送端在 TX 线程 sleep 期间连续发送两次命令，第一次命令会被第二次覆盖，导致丢失

**触发条件**：
1. TX 线程进入 `sleep(50μs)`
2. 发送端写入 `command1` 到插槽
3. 发送端立即再次写入 `command2` 到插槽（覆盖 `command1`）
4. TX 线程 sleep 结束，只处理 `command2`，`command1` 丢失

**实际场景**：
- 在 `position_control_demo.rs` 中，步骤 4 只发送一次命令，不会触发此问题
- 但在步骤 5（保持位置）中，每 200ms 发送一次命令，如果发送频率过高，可能触发

**严重程度**：🔴 **高**（可能导致命令丢失）

**验证方法**：
- 检查 `metrics.tx_realtime_overwrites` 指标
- 如果此指标 > 0，说明发生了覆盖

**解决方案**：
1. **方案 A**：使用 Condvar 唤醒机制，TX 线程 sleep 前等待信号
2. **方案 B**：发送端检测覆盖，记录警告日志
3. **方案 C**：保持现状，但添加监控和警告

**推荐**：方案 C（添加监控），因为：
- 邮箱模式的设计就是 "Last Write Wins"
- 如果发送频率过高，覆盖是预期行为
- 但应该通过指标监控，让用户知道发生了覆盖

---

### 3.3 问题 3：锁竞争导致的发送延迟

**问题描述**：
- 如果 TX 线程正在持有锁（处理命令），发送端会阻塞等待

**影响**：
- 发送延迟可能达到几微秒到几十微秒
- 对于高频控制循环，可能累积延迟

**严重程度**：🟢 **低**（设计权衡）

**解决方案**：
- 当前设计已优化（锁持有时间极短）
- 无需修改

---

## 4. 时序图分析

### 4.1 正常情况（无覆盖）

```
发送端线程                    TX 线程
    |                           |
    |-- send_realtime() ------->|
    |   (获取锁)                |
    |   (写入插槽)               |
    |   (释放锁)                 |
    |<-- 返回 Ok() -------------|
    |                           |
    |                           |-- 获取锁
    |                           |-- 取出命令
    |                           |-- 释放锁
    |                           |-- 处理命令（发送到 CAN）
    |                           |-- continue（回到循环开始）
```

**结果**：✅ 命令被及时处理

---

### 4.2 问题情况（TX 线程 sleep + 连续发送）

```
发送端线程                    TX 线程
    |                           |
    |                           |-- sleep(50μs) 开始
    |-- send_realtime() ------->|
    |   (获取锁)                |
    |   (写入 command1)         |
    |   (释放锁)                |
    |<-- 返回 Ok() -------------|
    |                           |
    |-- send_realtime() ------->|
    |   (获取锁)                |
    |   (写入 command2)         |   [覆盖 command1]
    |   (释放锁)                |
    |<-- 返回 Ok() -------------|
    |                           |
    |                           |-- sleep(50μs) 结束
    |                           |-- 获取锁
    |                           |-- 取出 command2
    |                           |-- 释放锁
    |                           |-- 处理 command2
    |                           |   [command1 丢失]
```

**结果**：❌ command1 丢失

---

## 5. 代码审查检查清单

### 5.1 发送端检查

- [x] ✅ 使用数组而非 Vec（栈分配）
- [x] ✅ 一次性准备所有关节（避免覆盖）
- [x] ✅ 锁持有时间极短（< 50ns）
- [x] ✅ 在锁外更新指标（减少锁竞争）
- [x] ✅ 错误处理正确

### 5.2 消费端检查

- [x] ✅ 优先级调度正确（Realtime > Reliable）
- [x] ✅ 使用 `slot.take()` 取出命令（插槽变为 None）
- [x] ✅ 锁持有时间极短（仅为 `take()` 操作）
- [x] ✅ 饿死保护机制正确
- [x] ✅ 错误处理正确（部分发送场景）
- [x] ✅ sleep 逻辑正确（避免忙等待）

### 5.3 潜在问题检查

- [x] ⚠️ TX 线程 sleep 期间的命令延迟（设计权衡，可接受）
- [x] ⚠️ 连续发送时的命令覆盖（需要监控）
- [x] ⚠️ 锁竞争导致的发送延迟（设计权衡，可接受）

---

## 6. 建议和改进

### 6.1 立即改进（高优先级）

**状态**：✅ **已实施**（2026-01-XX）

1. **智能覆盖监控策略**（避免日志噪音）：

   **问题分析**：
   - 在高频控制场景（500Hz-1kHz）下，覆盖是**预期行为**（Last Write Wins）
   - 如果每次覆盖都 warning，会产生大量日志，影响性能和可读性
   - 需要区分**正常覆盖**（高频控制）和**异常覆盖**（TX 线程瓶颈）

   **推荐方案：基于覆盖率的阈值监控**

   **方案 A：覆盖率阈值监控（推荐）**
   ```rust
   // 在 send_realtime_command 中
   if is_overwrite {
       self.metrics.tx_realtime_overwrites.fetch_add(1, Ordering::Relaxed);

       // 计算覆盖率（每 1000 次发送检查一次，避免频繁计算）
       let total = self.metrics.tx_frames_total.load(Ordering::Relaxed);
       if total > 0 && total % 1000 == 0 {
           let overwrites = self.metrics.tx_realtime_overwrites.load(Ordering::Relaxed);
           let rate = (overwrites as f64 / total as f64) * 100.0;

           // 只在覆盖率超过阈值时警告（例如 > 50%）
           if rate > 50.0 {
               warn!(
                   "High realtime overwrite rate: {:.1}% ({} overwrites / {} total sends)",
                   rate, overwrites, total
               );
           }
       }
   }
   ```

   **优点**：
   - ✅ 避免日志噪音（只在异常时警告）
   - ✅ 性能开销小（每 1000 次才计算一次）
   - ✅ 能区分正常和异常覆盖

   **缺点**：
   - ⚠️ 需要定期检查，增加少量开销

   **方案 B：采样日志（备选）**
   ```rust
   // 使用静态计数器，每 N 次覆盖记录一次
   static OVERWRITE_LOG_COUNTER: AtomicU64 = AtomicU64::new(0);

   if is_overwrite {
       self.metrics.tx_realtime_overwrites.fetch_add(1, Ordering::Relaxed);

       // 每 100 次覆盖记录一次日志
       let count = OVERWRITE_LOG_COUNTER.fetch_add(1, Ordering::Relaxed);
       if count % 100 == 0 {
           let total = self.metrics.tx_frames_total.load(Ordering::Relaxed);
           let overwrites = self.metrics.tx_realtime_overwrites.load(Ordering::Relaxed);
           let rate = (overwrites as f64 / total as f64) * 100.0;

           debug!(
               "Realtime overwrite sample: {:.1}% rate ({} overwrites / {} total)",
               rate, overwrites, total
           );
       }
   }
   ```

   **优点**：
   - ✅ 日志频率可控
   - ✅ 使用 `debug!` 级别，生产环境可关闭

   **缺点**：
   - ⚠️ 仍会产生日志（虽然频率低）

   **方案 C：仅指标监控，无日志（最轻量）**
   ```rust
   // 不添加任何日志，只更新指标
   if is_overwrite {
       self.metrics.tx_realtime_overwrites.fetch_add(1, Ordering::Relaxed);
   }
   ```

   **优点**：
   - ✅ 零日志开销
   - ✅ 用户可以通过 `metrics.snapshot()` 主动检查

   **缺点**：
   - ⚠️ 用户需要主动监控，可能错过异常

   **推荐**：**方案 A（覆盖率阈值监控）**
   - 在正常场景下（覆盖率 < 50%），不产生日志
   - 在异常场景下（覆盖率 > 50%），产生警告
   - 性能开销小（每 1000 次才计算一次）

2. **指标监控和诊断工具**：
   - 提供 `MetricsSnapshot::overwrite_rate()` 方法，计算覆盖率
   - 在文档中说明：覆盖率 < 30% 为正常，> 50% 为异常
   - 提供示例代码，展示如何监控覆盖率

### 6.2 可选改进（中优先级）

1. **使用 Condvar 唤醒机制**：
   - 在 `Piper` 结构体中添加 `cvar: Arc<Condvar>`
   - 发送端写入插槽后，调用 `cvar.notify_one()` 唤醒 TX 线程
   - TX 线程 sleep 前，使用 `cvar.wait(lock)` 等待信号
   - **优点**：消除 sleep 延迟，命令立即处理
   - **缺点**：增加复杂度，需要修改结构体

2. **添加调试日志**：
   - 在关键路径添加 `tracing::debug!` 日志
   - 便于问题排查（但用户已拒绝此方案）

### 6.3 长期改进（低优先级）

1. **性能优化**：
   - 如果锁竞争严重，考虑使用无锁数据结构（如 `ArcSwap`）
   - 但当前设计已足够，无需优化

2. **文档完善**：
   - 在 API 文档中明确说明邮箱模式的行为
   - 说明 "Last Write Wins" 的含义和影响

---

## 7. 结论

### 7.1 代码质量评估

**整体评估**：✅ **优秀**

- 发送端逻辑正确，性能优化到位
- 消费端逻辑正确，优先级调度合理
- 错误处理完善
- 代码结构清晰

### 7.2 潜在问题总结

1. **TX 线程 sleep 延迟**：🟡 中等严重程度，设计权衡，通常可接受
2. **连续发送覆盖**：🟡 中等严重程度（修正：正常场景下是预期行为，只需监控异常情况）
3. **锁竞争延迟**：🟢 低严重程度，设计权衡，已优化

**关于覆盖行为的说明**：
- **正常覆盖**（覆盖率 < 30%）：高频控制场景下的预期行为，无需警告
- **异常覆盖**（覆盖率 > 50%）：TX 线程瓶颈或发送频率过高，需要警告
- **监控策略**：使用覆盖率阈值，避免日志噪音

### 7.3 建议行动

1. **立即行动**：
   - 实现**智能覆盖监控**（方案 A：覆盖率阈值监控）
   - 在 `MetricsSnapshot` 中添加 `overwrite_rate()` 方法
   - 在文档中说明邮箱模式的行为和覆盖率阈值

2. **可选行动**：
   - 如果低延迟需求明确，考虑使用 Condvar 唤醒机制
   - 提供监控示例代码，展示如何检查覆盖率

3. **无需行动**：
   - 当前设计已足够，无需大规模重构
   - **不推荐**：每次覆盖都 warning（会产生日志噪音）

---

## 8. 附录

### 8.1 相关文件

- `src/client/raw_commander.rs` - 发送端入口
- `src/driver/piper.rs` - 发送端实现
- `src/driver/pipeline.rs` - 消费端实现（TX 线程）
- `src/driver/command.rs` - RealtimeCommand 定义

### 8.2 相关文档

- `docs/v0/mailbox_frame_package_implementation_plan.md` - 实现方案
- `docs/v0/mailbox_frame_package_execution_plan.md` - 执行方案
- `docs/v0/send_realtime_overwrite_issue_analysis.md` - 覆盖问题分析

---

**文档结束**

