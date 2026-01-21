# 守护进程统计指标预留字段分析报告

## 概述

本报告分析 `src/bin/gs_usb_daemon/daemon.rs` 中 `DetailedStats` 结构体的预留字段，评估其使用价值和实现可行性。

## 预留字段列表

```rust
struct DetailedStats {
    // USB 传输错误
    usb_transfer_errors: AtomicU64,     // ✅ 已使用
    usb_timeout_count: AtomicU64,       // ✅ 已使用
    _usb_stall_count: AtomicU64,         // ⚠️ 预留
    usb_no_device_count: AtomicU64,     // ✅ 已使用

    // CAN 总线健康度
    can_error_frames: AtomicU64,        // ✅ 已使用
    _can_bus_off_count: AtomicU64,       // ⚠️ 预留
    _can_error_passive_count: AtomicU64, // ⚠️ 预留

    // 性能基线
    _baseline_rx_fps: f64,               // ⚠️ 预留
    _baseline_tx_fps: f64,               // ⚠️ 预留
}
```

---

## 1. `_usb_stall_count` - USB STALL 错误计数

### 当前状态
- **标记**：预留（字段名以 `_` 开头）
- **用途**：统计 USB 端点 STALL 发生的次数

### 技术分析

#### 何时发生 STALL
在 `src/can/gs_usb/device.rs:563-576` 中，当 USB 写操作超时时，代码会调用 `clear_halt()` 清除端点 STALL 状态：

```rust
match self.handle.write_bulk(self.endpoint_out, &buf, self.write_timeout) {
    Ok(_) => Ok(()),
    Err(rusb::Error::Timeout) => {
        // USB 批量传输超时后，endpoint 可能进入 STALL 状态
        // 必须清除 halt 才能恢复设备，否则后续操作会失败
        if let Err(clear_err) = self.handle.clear_halt(self.endpoint_out) {
            warn!("Failed to clear endpoint halt after timeout: {}", clear_err);
        } else {
            // ✅ 这里可以统计 STALL 清除成功次数
            std::thread::sleep(Duration::from_millis(50));
        }
        Err(GsUsbError::WriteTimeout)
    },
    Err(e) => Err(GsUsbError::Usb(e)),
}
```

#### STALL 的其他来源
除了超时导致的 STALL，还可能来自：
- **设备端错误**：设备固件主动发送 STALL
- **协议错误**：控制传输中的协议违规
- **端点配置错误**：端点被禁用或配置错误

### 实现可行性
- ✅ **完全可行**：可以在 `clear_halt()` 调用时统计
- ✅ **已有基础设施**：错误处理流程已存在，只需添加计数

### 价值评估
- **诊断价值**：⭐⭐⭐⭐⭐ 高
  - STALL 频繁发生通常表示设备端问题或 USB 连接不稳定
  - 有助于区分 "超时但设备正常" 和 "设备故障导致 STALL"
- **监控价值**：⭐⭐⭐⭐ 中高
  - 可以作为健康度评分的指标之一
  - 帮助预测设备故障

### 推荐方案
**强烈建议启用并实现**。

```rust
// 在 device.rs 的 send_raw 中
if let Err(clear_err) = self.handle.clear_halt(self.endpoint_out) {
    warn!("Failed to clear endpoint halt after timeout: {}", clear_err);
} else {
    // ✅ 统计 STALL 清除成功（表示发生了 STALL）
    // 需要在 DetailedStats 中添加回调或通过错误类型传递
    stats.usb_stall_count.fetch_add(1, Ordering::Relaxed);
    std::thread::sleep(Duration::from_millis(50));
}
```

**注意**：需要将 `DetailedStats` 的引用传递给设备层，或者通过错误类型间接传递统计信息。

---

## 2. `_can_bus_off_count` - CAN Bus Off 状态计数

### 当前状态
- **标记**：预留（字段名以 `_` 开头）
- **用途**：统计 CAN 总线进入 Bus Off 状态的次数

### 技术分析

#### CAN Bus Off 状态
Bus Off 是 CAN 总线的严重错误状态，表示：
- **错误计数器超过阈值**（通常 > 255）
- **节点被强制离线**，无法收发数据
- **需要软件或硬件复位才能恢复**

#### GS-USB 协议支持
在 `src/can/gs_usb/mod.rs:625` 中有注释：

```rust
// 3.3 检查致命错误：Bus Off（需要通过 DeviceCapability 查询）
// 这里假设通过 flags 或其他机制检测
// 如果设备支持 GET_STATE，可以查询状态
```

#### 错误帧检测
在 `src/can/gs_usb/protocol.rs` 中定义了错误帧标志：
```rust
/// Error message frame flag
pub const CAN_ERR_FLAG: u32 = 0x2000_0000;
```

但当前代码只检测了 `OVERFLOW` 标志，没有处理 CAN 错误帧。

#### 检测机制选项

**选项 1：State Flags（推荐，优先实现）**
GS-USB 协议通常在每一帧的 `flags` 字段中携带当前控制器状态。需要检查 `GsUsbFrame::flags` 是否包含指示 Bus Off 的状态位：

```rust
// 在 src/can/gs_usb/mod.rs 的帧处理循环中
if gs_frame.flags & GS_CAN_FLAG_BUS_OFF != 0 {
    // 检测到 Bus Off 状态
    detailed.can_bus_off_count.fetch_add(1, Ordering::Relaxed);
    error!("CAN Bus Off detected!");
    // 可能需要停止设备或触发恢复流程
}
```

**选项 2：错误帧解析（如果设备支持）**
如果设备通过错误帧报告 Bus Off，需要解析 CAN 错误帧：

```rust
// 检查是否为错误帧
if (gs_frame.can_id & CAN_ERR_FLAG) != 0 {
    // 解析错误帧，提取错误类型
    let error_type = gs_frame.data[1]; // CAN 错误帧的错误类型字段
    if error_type & CAN_ERR_CRTL_TX_BUS_OFF != 0
       || error_type & CAN_ERR_CRTL_RX_BUS_OFF != 0 {
        detailed.can_bus_off_count.fetch_add(1, Ordering::Relaxed);
    }
}
```

**选项 3：控制传输查询（兜底机制）**
对于不支持主动上报错误帧的设备，可以通过 Control Transfer 定期轮询设备状态：

```rust
// 每 1-5 秒轮询一次设备状态
fn poll_device_state(&self) -> Result<DeviceState, GsUsbError> {
    // 发送 GET_STATE 控制传输命令
    // 解析返回的状态，检查 Bus Off 标志
}
```

**实施建议**：
1. **首先检查** `GsUsbFrame::flags` 是否包含 Bus Off 指示位（最简单、最直接）
2. **其次尝试** 错误帧解析（如果设备发送错误帧）
3. **最后考虑** 控制传输查询（作为兜底机制）

#### 边界情况：Bus Off 计数防抖（Debounce）

**问题**：如果设备在每一帧的 `flags` 中携带 Bus Off 状态，当设备处于 Bus Off 状态时，每秒可能报告 1000 次 Bus Off（1kHz 帧率），导致 `can_bus_off_count` 瞬间增加 1000，这在统计上不合理。

**解决方案**：引入防抖机制，统计 **"Bus Off 事件发生的次数"**，而不是 **"处于 Bus Off 状态的帧数"**。

**实现策略**：
```rust
struct DetailedStats {
    can_bus_off_count: AtomicU64,
    // Bus Off 状态标志（用于防抖）
    is_bus_off: AtomicBool,  // 当前是否处于 Bus Off 状态
}

impl DetailedStats {
    /// 检查并更新 Bus Off 状态（带防抖）
    fn update_bus_off_status(&self, is_bus_off_now: bool) {
        let was_bus_off = self.is_bus_off.load(Ordering::Relaxed);

        // 只有从 false -> true 的转换才计数（上升沿检测）
        if !was_bus_off && is_bus_off_now {
            // 新进入 Bus Off 状态，计数加 1
            self.can_bus_off_count.fetch_add(1, Ordering::Relaxed);
            self.is_bus_off.store(true, Ordering::Relaxed);

            error!("CAN Bus Off detected! Total occurrences: {}",
                   self.can_bus_off_count.load(Ordering::Relaxed));
        } else if was_bus_off && !is_bus_off_now {
            // 从 Bus Off 状态恢复，重置标志
            self.is_bus_off.store(false, Ordering::Relaxed);
            info!("CAN Bus Off recovered. Ready for next detection.");
        }
        // 如果状态未变化（一直是 true 或一直是 false），不做任何操作
    }
}

// 在帧处理循环中使用
if gs_frame.flags & GS_CAN_FLAG_BUS_OFF != 0 {
    detailed.update_bus_off_status(true);
} else {
    detailed.update_bus_off_status(false);
}
```

**关键点**：
- ✅ **上升沿检测**：只有状态从 `false` 变为 `true` 时才计数
- ✅ **状态保持**：持续处于 Bus Off 状态时不再计数
- ✅ **恢复检测**：状态恢复正常时重置标志，准备检测下次事件
- ✅ **运维友好**：统计的是"事件次数"而非"帧数"，更有实际意义

**边界情况处理**：

除了通过 flags 检测状态变化外，还需要在以下情况下**强制重置** `is_bus_off` 标志：

```rust
impl DetailedStats {
    /// 强制重置 Bus Off 状态标志（设备重连或手动复位后调用）
    fn reset_bus_off_status(&self) {
        let was_bus_off = self.is_bus_off.swap(false, Ordering::Relaxed);
        if was_bus_off {
            info!("Bus Off status flag reset (device reconnected or manually reset)");
        }
    }
}

// 在设备重连成功后调用
fn on_device_reconnected() {
    stats.detailed.read().unwrap().reset_bus_off_status();
}

// 在手动设备复位后调用
fn on_device_reset() {
    stats.detailed.read().unwrap().reset_bus_off_status();
}
```

**复位时机**：
- ✅ **设备重连**：热拔插恢复后，新会话开始时
- ✅ **手动复位**：软件主动调用设备复位命令后
- ✅ **守护进程重启**：启动时自然重置为 `false`（字段初始化）

**重要性**：如果设备不支持自动恢复（Automatic Bus Off Recovery），Bus Off 状态可能需要软件干预才能清除。强制重置确保在新会话开始时状态是干净的，避免状态残留导致计数异常。

**注意**：Error Passive 状态检测应该使用相同的防抖机制和复位策略。

### 实现可行性
- ⚠️ **部分可行**：取决于 GS-USB 设备的具体实现
- ✅ **State Flags 检测**：如果设备在 flags 中包含状态信息，实现简单
- ⚠️ **错误帧解析**：需要设备支持错误帧报告
- ⚠️ **控制传输查询**：需要设备支持 GET_STATE 命令

### 价值评估
- **诊断价值**：⭐⭐⭐⭐⭐ **极其严重**（最高优先级）
  - Bus Off 表示 CAN 总线严重问题（通常意味着物理层短路、波特率错误或总线冲突）
  - 一旦发生 Bus Off，节点被强制离线，**系统彻底瘫痪**，无法收发任何数据
  - 对于实时控制系统（如机械臂），这是**最严重的故障**，可能导致安全事故
- **监控价值**：⭐⭐⭐⭐⭐ 极高
  - 应该在健康度评分中**严重扣分**（直接设为 0 或触发紧急告警）
  - 应该立即触发告警或自动恢复流程
  - **必须作为 P0 级别任务处理**

### 推荐方案
**强烈建议实现（优先级：P0，最高级）**。

**实施策略**（按优先级排序）：
1. **立即验证**设备是否在 `flags` 字段中携带 Bus Off 状态
2. **如果支持 State Flags**：实现 flags 检测（最简单、最直接）
3. **如果不支持**：尝试错误帧解析
4. **如果仍不支持**：实现控制传输查询（兜底机制）
5. **如果设备完全不支持**：在启动日志中明确警告："⚠️ WARNING: This device does not support bus health monitoring. Bus Off state cannot be detected."

**注意**：Bus Off 检测应该作为**最高优先级**功能实现。如果设备不支持，应该在文档和日志中明确说明，因为缺少这个监控能力在工业环境中是不可接受的。

---

## 3. `_can_error_passive_count` - CAN Error Passive 状态计数

### 当前状态
- **标记**：预留（字段名以 `_` 开头）
- **用途**：统计 CAN 总线进入 Error Passive 状态的次数

### 技术分析

#### CAN Error Passive 状态
Error Passive 是 Bus Off 的前置状态：
- **错误计数器在 128-255 之间**
- **节点仍可通信**，但发送时会有额外延迟
- **如果不处理**，可能升级为 Bus Off

#### 检测方式
与 Bus Off 类似，需要：
- GS-USB 设备支持错误帧报告
- 解析错误帧中的错误类型标志

### 实现可行性
- ⚠️ **与 Bus Off 相同**：取决于设备支持

### 价值评估
- **诊断价值**：⭐⭐⭐⭐ 高
  - Error Passive 是 Bus Off 的预警，有助于提前发现问题
- **监控价值**：⭐⭐⭐⭐ 高
  - 可以作为健康度预警指标

### 推荐方案
**建议实现（如果设备支持）**。

与 Bus Off 一起实现，使用相同的错误帧解析机制。

---

## 4. `_baseline_rx_fps` 和 `_baseline_tx_fps` - 性能基线

### 当前状态
- **标记**：预留（字段名以 `_` 开头）
- **用途**：记录正常的 RX/TX 帧率基线，用于异常检测

### 技术分析

#### 当前实现
守护进程已有实时 FPS 统计：
- `get_rx_fps()`: 计算平均接收帧率
- `get_tx_fps()`: 计算平均发送帧率

但没有基线跟踪机制。

#### 基线计算策略
常见策略：
1. **启动后稳定期计算**：运行一段时间后，计算平均 FPS 作为基线
2. **滑动窗口平均**：使用最近 N 秒的平均值作为动态基线
3. **固定阈值**：基于预期负载设置固定基线

### 实现可行性
- ✅ **完全可行**：已有 FPS 计算，只需添加基线跟踪
- ✅ **实现简单**：不需要底层协议支持

### 价值评估
- **诊断价值**：⭐⭐⭐⭐ 高
  - 可以帮助检测性能下降（如设备故障、总线负载过高等）
  - 异常检测：如果当前 FPS 显著低于基线，可能有问题
- **监控价值**：⭐⭐⭐⭐⭐ 极高
  - 可以用于自动化运维（告警、降级等）

### 推荐方案
**强烈建议实现**。

**实现策略**：

#### 类型选择（避免锁）
在 `DetailedStats` 这种高频访问的结构体中，**必须避免锁**。

**推荐方案：使用位模式转换（Bit Casting）**

**理由**：
- Rust 的 `f64::to_bits()` 和 `f64::from_bits()` 是零开销、标准化的转换
- 逻辑上更纯粹：只是借用 `AtomicU64` 作为存储容器，不改变数值语义
- **定点数方案的缺点**：
  - 需要进行 `int <-> float` 转换，在 EWMA 计算中会反复转换
  - 容易引入精度丢失或溢出（虽然概率极低）
  - 需要手动管理精度（例如乘以 1000），增加代码复杂性

**实现**：
```rust
struct DetailedStats {
    // 使用 AtomicU64 存储 f64 的位模式（零开销转换）
    baseline_rx_fps: AtomicU64,  // f64 位模式
    baseline_tx_fps: AtomicU64,  // f64 位模式
    is_warmed_up: AtomicBool,     // 预热期标志
    warmup_start_time: Instant,   // 预热开始时间
}

impl DetailedStats {
    /// 获取 RX 基线 FPS
    fn baseline_rx_fps(&self) -> f64 {
        f64::from_bits(self.baseline_rx_fps.load(Ordering::Relaxed))
    }

    /// 设置 RX 基线 FPS
    fn set_baseline_rx_fps(&self, fps: f64) {
        self.baseline_rx_fps.store(fps.to_bits(), Ordering::Relaxed);
    }

    /// 获取 TX 基线 FPS
    fn baseline_tx_fps(&self) -> f64 {
        f64::from_bits(self.baseline_tx_fps.load(Ordering::Relaxed))
    }

    /// 设置 TX 基线 FPS
    fn set_baseline_tx_fps(&self, fps: f64) {
        self.baseline_tx_fps.store(fps.to_bits(), Ordering::Relaxed);
    }
}
```

**注意**：位模式转换是 Rust 标准库提供的安全转换，不会改变数值语义，只是改变了存储表示。

#### 预热期处理
```rust
impl DetailedStats {
    // ============================================================
    // 基线计算配置常量（集中管理，便于调优）
    // ============================================================

    /// 预热期时长（秒）
    ///
    /// **调优建议**：
    /// - 较短（5-10秒）：快速建立基线，适合稳定负载场景
    /// - 较长（15-30秒）：更准确的基线，适合动态负载或需要更保守的异常检测
    /// - 默认 10 秒是一个平衡点
    const WARMUP_PERIOD_SECS: u64 = 10;

    /// EWMA 平滑因子（0.0 - 1.0）
    ///
    /// **调优建议**：
    /// - **较小值（0.001-0.01）**：基线变化缓慢，适合极稳定的工业场景
    ///   - 优点：对短期波动不敏感，基线稳定
    ///   - 缺点：适应长期负载变化慢
    /// - **中等值（0.01-0.05）**：平衡稳定性和适应性（推荐）
    ///   - 适合大多数场景，默认 0.01
    /// - **较大值（0.05-0.1）**：基线更新快，适合高动态负载场景
    ///   - 优点：快速适应负载变化
    ///   - 缺点：可能对短期波动过度敏感
    ///
    /// **计算**：α = 0.01 表示新数据权重 1%，基线权重 99%
    /// - 新基线 = 旧基线 × 99% + 当前值 × 1%
    /// - 半衰期 ≈ ln(0.5) / ln(1 - α) ≈ 69 次更新（约 69 秒 @ 1Hz）
    const EWMA_ALPHA: f64 = 0.01;

    /// 检查是否已预热完成
    fn is_warmed_up(&self) -> bool {
        self.is_warmed_up.load(Ordering::Relaxed)
    }

    /// 更新基线（启动后稳定期计算 + 动态更新）
    fn update_baseline(&mut self, rx_fps: f64, tx_fps: f64, elapsed: Duration) {
        let elapsed_secs = elapsed.as_secs();

        // 预热期：累积计算平均值
        if elapsed_secs < Self::WARMUP_PERIOD_SECS {
            let samples = elapsed_secs.max(1); // 避免除零
            let current_rx = self.baseline_rx_fps();
            let current_tx = self.baseline_tx_fps();

            // 累积平均
            let new_rx = (current_rx * (samples - 1) as f64 + rx_fps) / samples as f64;
            let new_tx = (current_tx * (samples - 1) as f64 + tx_fps) / samples as f64;

            self.set_baseline_rx_fps(new_rx);
            self.set_baseline_tx_fps(new_tx);
        } else {
            // 预热期结束，标记为已预热
            if !self.is_warmed_up() {
                self.is_warmed_up.store(true, Ordering::Relaxed);
                info!("Performance baseline established: RX={:.1} fps, TX={:.1} fps",
                      self.baseline_rx_fps(), self.baseline_tx_fps());
            }

            // 动态基线更新：使用指数加权移动平均 (EWMA)
            // 使用配置的 ALPHA 值，缓慢适应长期负载变化
            let current_rx = self.baseline_rx_fps();
            let current_tx = self.baseline_tx_fps();

            let new_rx = current_rx * (1.0 - Self::EWMA_ALPHA) + rx_fps * Self::EWMA_ALPHA;
            let new_tx = current_tx * (1.0 - Self::EWMA_ALPHA) + tx_fps * Self::EWMA_ALPHA;

            // 使用位模式转换存储（零开销）
            self.set_baseline_rx_fps(new_rx);
            self.set_baseline_tx_fps(new_tx);
        }
    }

    /// 检测性能异常（仅在预热期后检测）
    fn is_performance_degraded(&self, current_rx_fps: f64, current_tx_fps: f64) -> bool {
        // 预热期内不进行异常检测（避免误报）
        if !self.is_warmed_up() {
            return false;
        }

        let baseline_rx = self.baseline_rx_fps();
        let baseline_tx = self.baseline_tx_fps();

        if baseline_rx == 0.0 || baseline_tx == 0.0 {
            return false; // 基线未建立
        }

        // 如果当前 FPS 低于基线的 50%，认为性能下降
        current_rx_fps < baseline_rx * 0.5 || current_tx_fps < baseline_tx * 0.5
    }
}
```

**关键点**：
- ✅ **避免锁**：使用 `AtomicU64` 存储 f64 位模式（`f64::to_bits()` / `f64::from_bits()`），完全无锁
- ✅ **预热期**：前 `WARMUP_PERIOD_SECS` 秒（默认 10 秒）不进行异常检测，避免误报
- ✅ **动态更新**：使用 EWMA（`EWMA_ALPHA`，默认 0.01）缓慢适应长期负载变化
- ✅ **配置化**：`WARMUP_PERIOD_SECS` 和 `EWMA_ALPHA` 定义为常量并集中管理，便于调优
- ✅ **线程安全**：所有操作都是原子操作，支持多线程并发访问

---

## 总结与优先级

| 字段 | 优先级 | 可行性 | 实现难度 | 推荐 | 备注 |
|------|--------|--------|----------|------|------|
| `can_bus_off_count` | **P0** ⭐⭐⭐⭐⭐ | ⚠️ 中等 | 中 | **最高优先级** | **极其严重故障**，系统瘫痪级别 |
| `usb_stall_count` | P1 ⭐⭐⭐⭐⭐ | ✅ 高 | 低 | **强烈推荐** | 高诊断价值 |
| `baseline_rx_fps` | P1 ⭐⭐⭐⭐⭐ | ✅ 高 | 低 | **强烈推荐** | 性能监控核心 |
| `baseline_tx_fps` | P1 ⭐⭐⭐⭐⭐ | ✅ 高 | 低 | **强烈推荐** | 性能监控核心 |
| `can_error_passive_count` | P2 ⭐⭐⭐⭐ | ⚠️ 中等 | 中 | **推荐（需验证设备支持）** | Bus Off 预警 |

### 实施建议

#### 阶段 1：P0 任务（最高优先级，立即实施）
**`can_bus_off_count` - Bus Off 检测（系统瘫痪级别故障）**
- **立即验证**设备是否支持 Bus Off 状态检测
  1. 检查 `GsUsbFrame::flags` 是否包含 Bus Off 指示位（优先方案）
  2. 如果支持 State Flags：实现 flags 检测（最简单、最直接）
  3. 如果不支持：尝试错误帧解析
  4. 如果仍不支持：实现控制传输查询（兜底机制）
  5. **如果设备完全不支持**：在启动日志中明确警告："⚠️ WARNING: This device does not support bus health monitoring. Bus Off state cannot be detected."

- **防抖机制**（必须实现）：
  - 引入 `is_bus_off: AtomicBool` 状态标志
  - 使用上升沿检测：只有状态从 `false` → `true` 时才计数
  - 状态恢复正常时重置标志
  - **强制复位**：设备重连或手动复位后强制重置标志，确保新会话状态干净
  - 避免高频状态报告导致计数爆炸（例如每秒 1000 次）

- **重要性**：Bus Off 是系统瘫痪级别故障，**必须在第一时间验证并实现**

#### 阶段 2：P1 任务（高优先级、高可行性，立即实施）
1. **`usb_stall_count`**：在设备层统计 STALL 清除次数
   - 在 `src/can/gs_usb/device.rs:569` 的 `clear_halt()` 调用后添加计数
   - 需要将 `DetailedStats` 引用传递给设备层，或通过回调机制传递

2. **`baseline_rx_fps` 和 `baseline_tx_fps`**：实现基线跟踪和异常检测
   - 使用 `AtomicU64` 存储 f64 位模式（`f64::to_bits()` / `f64::from_bits()`），避免锁
   - **推荐位模式转换方案**：零开销、标准化，逻辑纯粹
   - 实现预热期（`WARMUP_PERIOD_SECS`，默认 10 秒），避免误报
   - 实现 EWMA 动态基线更新（`EWMA_ALPHA`，默认 0.01），适应长期负载变化
   - **配置化常量**：将 `WARMUP_PERIOD_SECS` 和 `EWMA_ALPHA` 定义为常量并集中管理，便于调优
   - 在健康度评分中集成性能异常检测

#### 阶段 3：P2 任务（中优先级、需要验证）
**`can_error_passive_count`**：Bus Off 预警指标
- 与 Bus Off 检测一起实现，使用相同的错误帧解析机制
- **使用相同的防抖机制**：引入 `is_error_passive: AtomicBool`，上升沿检测
- 如果设备不支持，与 Bus Off 一起标记为"不可用"

#### 实施后清理
- **移除 `_` 前缀**：一旦实现，移除字段名的 `_` 前缀
- **更新健康度评分**：在 `health_score()` 中集成新指标
- **更新文档**：在 README 和代码注释中说明新的监控能力

### 相关代码位置

- **错误处理**: `src/can/gs_usb/device.rs:563-576` (STALL 清除)
- **帧处理**: `src/can/gs_usb/mod.rs:574-652` (错误帧检测)
- **统计更新**: `src/bin/gs_usb_daemon/daemon.rs:580-600` (错误统计)
- **健康度评分**: `src/bin/gs_usb_daemon/daemon.rs:157-197` (health_score)

