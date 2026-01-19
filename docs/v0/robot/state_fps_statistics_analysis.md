# State FPS 统计功能分析与实现方案

## 1. 背景与需求

### 1.1 问题描述

在机器人控制系统中，状态更新频率（FPS，Frames Per Second）是一个重要的性能指标，用于：
- **性能监控**：实时了解各状态的更新频率，检测通信是否正常
- **调试诊断**：识别丢帧、延迟等问题
- **系统调优**：优化控制循环频率，确保实时性

### 1.2 当前状态架构

根据 `src/robot/state.rs`，系统包含 5 种状态，具有不同的更新频率和同步机制：

| 状态类型 | 更新频率 | 同步机制 | 更新方法 | 更新位置数量 |
|---------|---------|---------|---------|------------|
| `CoreMotionState` | 500Hz | ArcSwap | `store()` | 4 |
| `JointDynamicState` | 500Hz | ArcSwap | `store()` | 2 |
| `ControlStatusState` | 100Hz | ArcSwap | `rcu()` | 2 |
| `DiagnosticState` | 10Hz | RwLock | `try_write()` | 3 |
| `ConfigState` | 按需 | RwLock | `try_write()` | 3 |

### 1.3 关键约束

1. **性能要求**：不能影响高频更新（500Hz）的性能
2. **线程安全**：IO 线程更新，读取线程查询，需要无锁或低锁开销
3. **实时性**：统计结果需要及时反映当前状态
4. **精度要求**：需要准确反映更新频率，避免统计误差

---

## 2. 实现方案分析

### 2.1 方案一：更新侧计数器（Write-Side Counter）

#### 2.1.1 设计思路

在状态更新点（`pipeline.rs`）添加计数器，每次更新时原子递增计数器。

```rust
// 在 PiperContext 中添加统计字段
pub struct PiperContext {
    // ... 现有字段 ...

    // FPS 统计（使用原子计数器）
    pub fps_stats: Arc<FpsStatistics>,
}

pub struct FpsStatistics {
    // 使用 AtomicU64 计数器
    pub core_motion_updates: AtomicU64,
    pub joint_dynamic_updates: AtomicU64,
    pub control_status_updates: AtomicU64,
    pub diagnostics_updates: AtomicU64,
    pub config_updates: AtomicU64,

    // 时间窗口记录
    pub start_time: Instant,
}

// 在 pipeline.rs 中更新时递增
ctx.core_motion.store(Arc::new(new_state));
ctx.fps_stats.core_motion_updates.fetch_add(1, Ordering::Relaxed);
```

#### 2.1.2 优点

- ✅ **零开销读取**：读取统计时不需要额外锁
- ✅ **准确度高**：直接在更新点计数，不会遗漏
- ✅ **实现简单**：只需在更新点添加一行代码
- ✅ **线程安全**：使用原子操作，无锁

#### 2.1.3 缺点

- ❌ **需要额外存储**：每个状态需要一个计数器
- ❌ **时间窗口计算**：需要定期计算 FPS（需要后台任务或查询时计算）
- ❌ **修改更新代码**：需要在所有更新点添加计数器

#### 2.1.4 性能开销

- **写入开销**：每次更新增加 1 次原子操作（~10ns），对 500Hz 影响可忽略
- **读取开销**：需要读取计数器和计算时间差（~100ns）

---

### 2.2 方案二：读取侧时间戳比较（Read-Side Timestamp Tracking）

#### 2.2.1 设计思路

在读取侧记录上次时间戳，通过时间戳变化检测更新次数。

```rust
// 在 Piper 或用户代码中维护统计
pub struct FpsTracker {
    last_core_motion_ts: u64,
    last_joint_dynamic_ts: u64,
    // ...
    update_counters: [u64; 5],
    window_start: Instant,
}

impl FpsTracker {
    pub fn record_update(&mut self, state: &CoreMotionState) {
        if state.timestamp_us != self.last_core_motion_ts {
            self.update_counters[0] += 1;
            self.last_core_motion_ts = state.timestamp_us;
        }
    }
}
```

#### 2.2.2 优点

- ✅ **零更新开销**：不需要修改 `pipeline.rs`
- ✅ **可选择性启用**：用户可以选择是否启用统计

#### 2.2.3 缺点

- ❌ **准确性差**：依赖于读取频率，读取频率低时统计不准
- ❌ **不能检测未读取的更新**：如果某个周期没有读取，会漏统计
- ❌ **时间戳比较开销**：每次读取都需要比较

#### 2.2.4 性能开销

- **写入开销**：0（无修改）
- **读取开销**：时间戳比较 + 计数器更新（~50ns）

---

### 2.3 方案三：代理包装器（Proxy Wrapper）

#### 2.3.1 设计思路

包装 `ArcSwap` 和 `RwLock`，在包装器内部添加统计逻辑。

```rust
pub struct InstrumentedArcSwap<T> {
    inner: ArcSwap<T>,
    update_counter: AtomicU64,
}

impl<T> InstrumentedArcSwap<T> {
    pub fn store(&self, new: Arc<T>) {
        self.inner.store(new);
        self.update_counter.fetch_add(1, Ordering::Relaxed);
    }

    pub fn load(&self) -> Guard<T> {
        self.inner.load()
    }
}

// 在 PiperContext 中使用
pub struct PiperContext {
    pub core_motion: Arc<InstrumentedArcSwap<CoreMotionState>>,
    // ...
}
```

#### 2.3.2 优点

- ✅ **封装性好**：统计逻辑封装在包装器中
- ✅ **透明使用**：对用户代码几乎无影响
- ✅ **集中管理**：所有统计逻辑在一个地方

#### 2.3.3 缺点

- ❌ **需要重构**：需要修改 `PiperContext` 结构，影响较大
- ❌ **维护成本**：需要维护包装器的所有方法
- ❌ **类型转换**：可能需要修改大量调用代码

#### 2.3.4 性能开销

- **写入开销**：同方案一（~10ns）
- **读取开销**：几乎无额外开销（代理转发）

---

### 2.4 方案四：环形缓冲区时间戳记录（Ring Buffer Timestamp）

#### 2.4.1 设计思路

使用环形缓冲区记录最近 N 次更新的时间戳，计算 FPS。

```rust
use lockfree::queue::Queue;

pub struct FpsStatistics {
    // 使用无锁队列记录时间戳
    update_timestamps: Queue<Instant>,
    max_samples: usize,
}

impl FpsStatistics {
    pub fn record_update(&self) {
        let now = Instant::now();
        self.update_timestamps.push(now);
        // 限制队列大小
        while self.update_timestamps.len() > self.max_samples {
            self.update_timestamps.pop();
        }
    }

    pub fn calculate_fps(&self) -> f64 {
        // 计算时间窗口内的更新次数
        // ...
    }
}
```

#### 2.4.2 优点

- ✅ **更精确的统计**：可以计算瞬时 FPS 和平均 FPS
- ✅ **支持滑动窗口**：可以计算不同时间窗口的 FPS

#### 2.4.3 缺点

- ❌ **内存开销大**：需要存储时间戳数组
- ❌ **实现复杂**：需要管理环形缓冲区和时间窗口
- ❌ **性能开销**：内存分配和队列操作（即使是无锁队列）

#### 2.4.4 性能开销

- **写入开销**：队列操作（~50ns）+ 时间戳记录
- **读取开销**：遍历队列计算（~500ns-1μs）

---

## 3. 方案对比总结

| 方案 | 准确性 | 写入开销 | 读取开销 | 实现复杂度 | 内存开销 | 推荐度 |
|-----|--------|---------|---------|-----------|---------|--------|
| 方案一：更新侧计数器 | ⭐⭐⭐⭐⭐ | ~10ns | ~100ns | ⭐⭐ | 低（5个原子变量） | ⭐⭐⭐⭐⭐ |
| 方案二：读取侧时间戳 | ⭐⭐ | 0 | ~50ns | ⭐ | 低 | ⭐⭐ |
| 方案三：代理包装器 | ⭐⭐⭐⭐⭐ | ~10ns | ~0 | ⭐⭐⭐⭐ | 低 | ⭐⭐⭐ |
| 方案四：环形缓冲区 | ⭐⭐⭐⭐⭐ | ~50ns | ~1μs | ⭐⭐⭐⭐⭐ | 高 | ⭐⭐⭐ |

---

## 4. 推荐方案：方案一（更新侧计数器）+ 简化时间窗口

### 4.1 方案选择理由

1. **性能最优**：写入开销最小（仅原子递增），适合 500Hz 高频更新
2. **准确性高**：直接在更新点计数，不会遗漏
3. **实现简单**：只需添加计数器和计算逻辑
4. **内存开销低**：只需要几个原子变量

### 4.2 详细设计

#### 4.2.1 数据结构

```rust
// src/robot/fps_stats.rs

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

/// FPS 统计数据
#[derive(Debug)]
pub struct FpsStatistics {
    // 更新计数器（原子操作，无锁）
    pub core_motion_updates: AtomicU64,
    pub joint_dynamic_updates: AtomicU64,
    pub control_status_updates: AtomicU64,
    pub diagnostics_updates: AtomicU64,
    pub config_updates: AtomicU64,

    // 统计窗口开始时间
    pub window_start: Instant,
}

impl FpsStatistics {
    pub fn new() -> Self {
        Self {
            core_motion_updates: AtomicU64::new(0),
            joint_dynamic_updates: AtomicU64::new(0),
            control_status_updates: AtomicU64::new(0),
            diagnostics_updates: AtomicU64::new(0),
            config_updates: AtomicU64::new(0),
            window_start: Instant::now(),
        }
    }

    /// 重置统计窗口
    pub fn reset(&mut self) {
        self.core_motion_updates.store(0, Ordering::Relaxed);
        self.joint_dynamic_updates.store(0, Ordering::Relaxed);
        self.control_status_updates.store(0, Ordering::Relaxed);
        self.diagnostics_updates.store(0, Ordering::Relaxed);
        self.config_updates.store(0, Ordering::Relaxed);
        self.window_start = Instant::now();
    }

    /// 计算 FPS（基于当前计数器和时间窗口）
    pub fn calculate_fps(&self) -> FpsResult {
        let elapsed_secs = self.window_start.elapsed().as_secs_f64();

        // 避免除零
        let elapsed_secs = elapsed_secs.max(0.001); // 至少 1ms

        FpsResult {
            core_motion: self.core_motion_updates.load(Ordering::Relaxed) as f64 / elapsed_secs,
            joint_dynamic: self.joint_dynamic_updates.load(Ordering::Relaxed) as f64 / elapsed_secs,
            control_status: self.control_status_updates.load(Ordering::Relaxed) as f64 / elapsed_secs,
            diagnostics: self.diagnostics_updates.load(Ordering::Relaxed) as f64 / elapsed_secs,
            config: self.config_updates.load(Ordering::Relaxed) as f64 / elapsed_secs,
        }
    }

    /// 获取原始计数器值（用于精确计算）
    pub fn get_counts(&self) -> FpsCounts {
        FpsCounts {
            core_motion: self.core_motion_updates.load(Ordering::Relaxed),
            joint_dynamic: self.joint_dynamic_updates.load(Ordering::Relaxed),
            control_status: self.control_status_updates.load(Ordering::Relaxed),
            diagnostics: self.diagnostics_updates.load(Ordering::Relaxed),
            config: self.config_updates.load(Ordering::Relaxed),
        }
    }
}

/// FPS 计算结果
#[derive(Debug, Clone, Copy)]
pub struct FpsResult {
    pub core_motion: f64,
    pub joint_dynamic: f64,
    pub control_status: f64,
    pub diagnostics: f64,
    pub config: f64,
}

/// FPS 计数器值
#[derive(Debug, Clone, Copy)]
pub struct FpsCounts {
    pub core_motion: u64,
    pub joint_dynamic: u64,
    pub control_status: u64,
    pub diagnostics: u64,
    pub config: u64,
}
```

#### 4.2.2 集成到 PiperContext

```rust
// src/robot/state.rs

use crate::robot::fps_stats::FpsStatistics;

pub struct PiperContext {
    // ... 现有字段 ...

    // FPS 统计（可选，默认启用）
    pub fps_stats: Arc<FpsStatistics>,
}

impl PiperContext {
    pub fn new() -> Self {
        Self {
            // ... 现有初始化 ...
            fps_stats: Arc::new(FpsStatistics::new()),
        }
    }
}
```

#### 4.2.3 在 pipeline.rs 中更新统计

```rust
// src/robot/pipeline.rs

// 在 CoreMotionState 更新时
ctx.core_motion.store(Arc::new(new_state));
ctx.fps_stats.core_motion_updates.fetch_add(1, Ordering::Relaxed);

// 在 JointDynamicState 更新时
ctx.joint_dynamic.store(Arc::new(pending_joint_dynamic.clone()));
ctx.fps_stats.joint_dynamic_updates.fetch_add(1, Ordering::Relaxed);

// 在 ControlStatusState 更新时
ctx.control_status.rcu(|old| { /* ... */ });
ctx.fps_stats.control_status_updates.fetch_add(1, Ordering::Relaxed);

// 在 DiagnosticState 更新时
if let Ok(mut diag) = ctx.diagnostics.try_write() {
    // ... 更新 ...
    ctx.fps_stats.diagnostics_updates.fetch_add(1, Ordering::Relaxed);
}

// 在 ConfigState 更新时
if let Ok(mut config) = ctx.config.try_write() {
    // ... 更新 ...
    ctx.fps_stats.config_updates.fetch_add(1, Ordering::Relaxed);
}
```

#### 4.2.4 添加 API 方法

```rust
// src/robot/robot_impl.rs

impl Piper {
    /// 获取 FPS 统计结果
    ///
    /// 返回最近一次统计窗口内的更新频率（FPS）。
    /// 建议定期调用（如每秒一次）或按需调用。
    pub fn get_fps(&self) -> FpsResult {
        self.ctx.fps_stats.calculate_fps()
    }

    /// 重置 FPS 统计窗口
    ///
    /// 清除当前计数器并开始新的统计窗口。
    /// 建议在需要精确测量时调用。
    pub fn reset_fps_stats(&self) {
        // 注意：需要获取可变引用，但 Arc 不支持
        // 解决方案：使用内部可变性（RefCell）或提供新方法
        // 或者：提供返回新 FpsStatistics 的方法
    }

    /// 获取 FPS 计数器原始值
    ///
    /// 返回当前计数器的原始值，可以配合自定义时间窗口计算 FPS。
    pub fn get_fps_counts(&self) -> FpsCounts {
        self.ctx.fps_stats.get_counts()
    }
}
```

---

## 5. 实现细节与优化

### 5.1 时间窗口管理

#### 方案 A：固定时间窗口（推荐）

统计从启动或上次重置开始到现在的 FPS。

**优点**：
- 实现简单
- 内存开销最小
- 适合长期监控

**缺点**：
- 需要定期重置才能获得瞬时 FPS

#### 方案 B：滑动时间窗口

维护一个时间戳队列，只计算最近 N 秒内的 FPS。

**优点**：
- 自动反映瞬时 FPS
- 不需要手动重置

**缺点**：
- 内存开销大
- 实现复杂

**推荐**：使用方案 A，提供 `reset()` 方法供用户按需重置。

### 5.2 原子操作顺序

使用 `Ordering::Relaxed` 即可，因为：
- 计数器独立，不需要与其他变量同步
- 只关心最终计数，不关心中间状态
- `Relaxed` 性能最好

### 5.3 避免计数溢出

`AtomicU64` 最大值为 2^64 - 1。在 500Hz 更新频率下：
- 每秒 500 次 = 500
- 每小时 500 * 3600 = 1,800,000
- 每天 500 * 86400 = 43,200,000
- 溢出时间 ≈ 2^64 / 43,200,000 ≈ 4,278,000,000 天 ≈ 11,700,000 年

**结论**：溢出风险可忽略，无需特殊处理。

### 5.4 性能优化建议

1. **批量更新**：如果某个状态在同一函数中多次更新，可以考虑批量计数（但通常不必要）
2. **条件编译**：可以使用 `#[cfg(feature = "fps-stats")]` 让统计功能可选
3. **零成本抽象**：如果不需要统计，可以通过编译时优化完全消除开销

---

## 6. 使用示例

### 6.1 基本使用

```rust
use piper_sdk::robot::Piper;

let piper = Piper::new(can_adapter, None)?;

// 运行一段时间后查询 FPS
std::thread::sleep(Duration::from_secs(5));

let fps = piper.get_fps();
println!("Core Motion FPS: {:.2}", fps.core_motion);
println!("Joint Dynamic FPS: {:.2}", fps.joint_dynamic);
println!("Control Status FPS: {:.2}", fps.control_status);

// 重置统计窗口，开始新的测量
piper.reset_fps_stats();
std::thread::sleep(Duration::from_secs(1));
let fps_after_reset = piper.get_fps();
```

### 6.2 定期监控

```rust
// 在后台线程定期打印 FPS
std::thread::spawn(move || {
    loop {
        std::thread::sleep(Duration::from_secs(1));
        let fps = piper.get_fps();

        // 检查 FPS 是否正常
        if fps.core_motion < 400.0 {
            eprintln!("警告：Core Motion FPS 异常低: {:.2}", fps.core_motion);
        }

        println!("FPS - Core: {:.1}, Joint: {:.1}, Status: {:.1}",
                 fps.core_motion, fps.joint_dynamic, fps.control_status);
    }
});
```

### 6.3 精确测量

```rust
// 重置统计
piper.reset_fps_stats();

// 运行控制循环
for _ in 0..1000 {
    let motion = piper.get_core_motion();
    // ... 控制逻辑 ...
}

// 测量实际 FPS
let counts = piper.get_fps_counts();
let elapsed = piper.get_elapsed_time(); // 需要添加此方法
let actual_fps = counts.core_motion as f64 / elapsed.as_secs_f64();
```

---

## 7. 测试策略

### 7.1 单元测试

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fps_statistics_basic() {
        let stats = FpsStatistics::new();

        // 初始 FPS 应该为 0
        let fps = stats.calculate_fps();
        assert_eq!(fps.core_motion, 0.0);

        // 模拟更新
        stats.core_motion_updates.fetch_add(500, Ordering::Relaxed);

        // 等待 1 秒
        std::thread::sleep(Duration::from_secs(1));

        // FPS 应该接近 500
        let fps = stats.calculate_fps();
        assert!((fps.core_motion - 500.0).abs() < 10.0);
    }

    #[test]
    fn test_fps_statistics_reset() {
        let mut stats = FpsStatistics::new();

        stats.core_motion_updates.fetch_add(100, Ordering::Relaxed);
        stats.reset();

        // 重置后计数器应该为 0
        let counts = stats.get_counts();
        assert_eq!(counts.core_motion, 0);
    }
}
```

### 7.2 集成测试

```rust
#[test]
fn test_pipeline_fps_tracking() {
    let ctx = Arc::new(PiperContext::new());
    let mut mock_can = MockCanAdapter::new();

    // 模拟高频更新
    for _ in 0..500 {
        let frame = create_joint_feedback_frame();
        mock_can.queue_frame(frame);
    }

    // 运行 pipeline
    run_pipeline_for_duration(mock_can, ctx.clone(), Duration::from_secs(1));

    // 验证 FPS 统计
    let fps = ctx.fps_stats.calculate_fps();
    assert!(fps.core_motion > 400.0); // 允许一定误差
}
```

---

## 8. 扩展功能（可选）

### 8.1 历史统计

维护最近 N 秒的 FPS 历史记录：

```rust
pub struct FpsHistory {
    samples: VecDeque<FpsResult>,
    max_samples: usize,
}

impl FpsHistory {
    pub fn record(&mut self, fps: FpsResult) {
        self.samples.push_back(fps);
        if self.samples.len() > self.max_samples {
            self.samples.pop_front();
        }
    }

    pub fn average_fps(&self) -> FpsResult {
        // 计算平均值
    }

    pub fn min_fps(&self) -> FpsResult {
        // 计算最小值
    }

    pub fn max_fps(&self) -> FpsResult {
        // 计算最大值
    }
}
```

### 8.2 告警功能

当 FPS 低于阈值时触发告警：

```rust
pub struct FpsMonitor {
    thresholds: FpsResult,
    callback: Box<dyn Fn(&str, f64)>,
}

impl FpsMonitor {
    pub fn check(&self, fps: &FpsResult) {
        if fps.core_motion < self.thresholds.core_motion {
            (self.callback)("core_motion", fps.core_motion);
        }
        // ...
    }
}
```

### 8.3 导出统计信息

支持以 JSON 或 CSV 格式导出统计信息，用于离线分析。

---

## 9. 总结

### 9.1 推荐实现

**方案一（更新侧计数器）** 是最优选择，因为：
- ✅ 性能开销最小（~10ns 写入开销）
- ✅ 准确性最高（直接计数，不遗漏）
- ✅ 实现简单（只需添加原子计数器）
- ✅ 内存开销低（仅 5 个原子变量）

### 9.2 实现步骤

1. **创建 FPS 统计模块**（`src/robot/fps_stats.rs`）
   - 定义 `FpsStatistics` 结构
   - 实现 `calculate_fps()` 方法

2. **集成到 PiperContext**（`src/robot/state.rs`）
   - 添加 `fps_stats: Arc<FpsStatistics>` 字段
   - 在 `new()` 中初始化

3. **在 pipeline 中更新计数**（`src/robot/pipeline.rs`）
   - 在所有状态更新点添加 `fetch_add(1, Ordering::Relaxed)`

4. **添加 API 方法**（`src/robot/robot_impl.rs`）
   - 实现 `get_fps()` 方法
   - 实现 `reset_fps_stats()` 方法（可选）

5. **编写测试**（`src/robot/fps_stats.rs`）
   - 单元测试
   - 集成测试

### 9.3 性能影响评估

| 操作 | 当前开销 | 增加开销 | 影响 |
|-----|---------|---------|------|
| CoreMotionState 更新 | ~100ns | +10ns | 可忽略（10%） |
| JointDynamicState 更新 | ~100ns | +10ns | 可忽略（10%） |
| ControlStatusState 更新 | ~200ns | +10ns | 可忽略（5%） |
| FPS 查询 | 0 | ~100ns | 可接受 |

**结论**：性能影响可以忽略不计，适合生产环境使用。

---

## 10. 参考资料

- `src/robot/state.rs`：状态结构定义
- `src/robot/pipeline.rs`：状态更新逻辑
- `src/robot/robot_impl.rs`：对外 API
- Rust 原子操作文档：`std::sync::atomic`

