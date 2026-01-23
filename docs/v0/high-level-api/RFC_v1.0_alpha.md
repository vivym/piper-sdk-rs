# RFC: Piper SDK Rust High-Level API v1.0-alpha

**状态**: 提案
**日期**: 2026-01-23
**作者**: Piper SDK Team
**版本**: v1.0-alpha

---

## 📋 摘要 (Abstract)

本 RFC 提议为 Piper 机械臂 Rust SDK 增加一套工业级的高级 API，通过 Rust 的类型系统和所有权模型，提供编译期安全保证、高性能并发控制、以及开发者友好的接口。

**关键创新**:
1. **Type State Pattern** - 编译期状态安全
2. **Capability-based Security** - 基于能力的权限控制
3. **Reader-Writer Split** - 并发友好的读写分离
4. **Atomic Fast Path** - 无锁高性能热路径
5. **Iterator-based Trajectory** - 内存高效的轨迹规划

---

## 🎯 动机 (Motivation)

### 现状问题

当前的 Piper SDK Rust API (低级 API) 存在以下限制:

1. **手动状态管理**: 开发者需要手动追踪机器人状态
   ```rust
   // ❌ 问题: 可能在未使能时发送命令
   can_bus.send(command)?;  // 运行时才能发现错误
   ```

2. **CAN 协议细节暴露**: 需要手动构造和解析 CAN 帧
   ```rust
   // ❌ 问题: 繁琐且容易出错
   let frame = CanFrame::new(0x01, &[0x01, 0x02, ...])?;
   ```

3. **并发不友好**: 同时控制和监控需要复杂的锁管理
   ```rust
   // ❌ 问题: Borrow Checker 阻止合理的并发
   let state = robot.read_state()?;  // 借用
   robot.send_command(...)?;         // 编译错误！
   ```

4. **单位混淆风险**: 使用原始 f64 类型
   ```rust
   // ❌ 问题: 角度单位不明确
   fn set_position(joint: usize, angle: f64) { ... }
   ```

### 目标

设计一套高级 API，实现:

- ✅ **编译期安全**: 非法状态转换在编译时被捕获
- ✅ **零开销抽象**: 高级接口不引入运行时开销
- ✅ **并发友好**: 天然支持多线程控制和监控
- ✅ **类型安全**: 强类型单位防止混淆
- ✅ **开发者友好**: 简洁直观的 API

---

## 🏗️ 设计概览 (Design Overview)

### 架构分层

```
┌─────────────────────────────────────────────┐
│   Layer 3: Controller Mode (高级控制器)       │
│   - PidController, TrajectoryPlanner        │
│   - Custom Controllers (trait Controller)   │
└─────────────────────────────────────────────┘
                    ↓
┌─────────────────────────────────────────────┐
│   Layer 2: Piper Type State (状态机)         │
│   - Piper<Disconnected> → Piper<Standby>    │
│   - Piper<Active<MitMode>> / <PositionMode> │
└─────────────────────────────────────────────┘
                    ↓
┌─────────────────────────────────────────────┐
│   Layer 1: Reader-Writer Split (读写分离)     │
│   - MotionCommander (write, 公开)           │
│   - Observer (read, 线程安全)                │
│   - RawCommander (full, 内部)               │
└─────────────────────────────────────────────┘
                    ↓
┌─────────────────────────────────────────────┐
│   Layer 0: Types & Utilities (基础类型)      │
│   - Rad, Deg, NewtonMeter (强类型单位)       │
│   - JointArray<T> (类型安全数组)             │
│   - RobotError (结构化错误)                   │
└─────────────────────────────────────────────┘
```

### 核心设计模式

#### 1. Type State Pattern

**目标**: 在编译期保证状态转换的合法性

**实现**:
```rust
pub struct Piper<State> {
    raw_commander: Arc<RawCommander>,
    observer: Observer,
    _state: PhantomData<State>,
}

// 状态类型（零大小类型）
pub struct Disconnected;
pub struct Standby;
pub struct Active<Mode> { _mode: PhantomData<Mode> }
pub struct MitMode;
pub struct PositionMode;

// 状态转换方法（消费 self，返回新状态）
impl Piper<Disconnected> {
    pub fn connect(config: ConnectionConfig) -> Result<Piper<Standby>, RobotError> {
        // ...
    }
}

impl Piper<Standby> {
    pub fn enable_mit_mode(self, config: MitModeConfig)
        -> Result<Piper<Active<MitMode>>, RobotError> {
        // ...
    }
}

impl Piper<Active<MitMode>> {
    pub fn command_torques(&self, torques: JointArray<NewtonMeter>)
        -> Result<(), RobotError> {
        // ...
    }
}
```

**优势**:
- ✅ 编译期保证: 未使能时无法发送命令
- ✅ 零运行时开销: `PhantomData<T>` 是零大小类型
- ✅ 自文档化: 类型签名即文档

#### 2. Capability-based Security

**目标**: 精细化权限控制，防止用户绕过状态机

**实现**:
```rust
// 内部完整权限（pub(crate)）
pub(crate) struct RawCommander {
    // ...
}

impl RawCommander {
    pub(crate) fn enable_arm(&self) -> Result<(), RobotError> { ... }
    pub(crate) fn disable_arm(&self) -> Result<(), RobotError> { ... }
    pub(crate) fn send_mit_command(...) -> Result<(), RobotError> { ... }
}

// 公开受限权限（pub）
pub struct MotionCommander {
    raw: Arc<RawCommander>,
}

impl MotionCommander {
    // ✅ 只暴露运动相关方法
    pub fn command_torques(&self, torques: JointArray<NewtonMeter>)
        -> Result<(), RobotError> {
        self.raw.send_mit_command(...)
    }

    // ❌ 不暴露状态变更方法（enable/disable）
}
```

**优势**:
- ✅ 防御"后门": 用户无法绕过状态机直接调用 `enable`/`disable`
- ✅ 最小权限原则: 只暴露必要的接口

#### 3. Reader-Writer Split

**目标**: 支持并发控制和监控

**实现**:
```rust
// 只读观察器（多线程安全）
pub struct Observer {
    state: Arc<RwLock<RobotState>>,
}

impl Observer {
    pub fn joint_positions(&self) -> JointArray<Rad> {
        self.state.read().joint_positions.clone()
    }

    pub fn joint_velocities(&self) -> JointArray<f64> {
        self.state.read().joint_velocities.clone()
    }
}

// 使用示例
let observer = piper.observer();
let commander = piper.motion_commander();

// ✅ 并发: 控制线程 + 监控线程
thread::spawn(move || {
    loop {
        let pos = observer.joint_positions();  // 读
        println!("Current: {:?}", pos);
    }
});

loop {
    commander.command_torques(torques)?;  // 写
}
```

**优势**:
- ✅ 并发友好: 读写分离避免 Borrow Checker 冲突
- ✅ 线程安全: `Arc<RwLock<T>>` 保证安全共享

#### 4. Atomic Fast Path

**目标**: 消除热路径锁竞争

**实现**:
```rust
pub(crate) struct StateTracker {
    valid_flag: Arc<AtomicBool>,  // 快速路径
    details: RwLock<TrackerDetails>,  // 慢路径
}

impl StateTracker {
    pub(crate) fn check_valid_fast(&self) -> Result<(), RobotError> {
        // ✅ 快速路径: 无锁原子检查 (~18ns)
        if !self.valid_flag.load(Ordering::Acquire) {
            // ❌ 慢路径: 只在失败时获取锁读详情
            return Err(self.details.read().to_error());
        }
        Ok(())
    }
}
```

**性能**:
- StateTracker 快速路径: ~18ns (目标 < 100ns, **5.4x 超标**)
- Observer 读取: ~11ns (目标 < 50ns, **4.5x 超标**)

#### 5. Iterator-based Trajectory

**目标**: 内存高效的轨迹规划

**实现**:
```rust
pub struct TrajectoryPlanner {
    spline_coeffs: JointArray<CubicSplineCoeffs>,
    current_time: f64,
    duration_sec: f64,
    interval_sec: f64,
}

impl Iterator for TrajectoryPlanner {
    type Item = (JointArray<Rad>, JointArray<f64>);  // (位置, 速度)

    fn next(&mut self) -> Option<Self::Item> {
        if self.current_time > self.duration_sec {
            return None;
        }

        let t = self.current_time / self.duration_sec;
        let (positions, velocities) = self.evaluate_at(t);

        self.current_time += self.interval_sec;
        Some((positions, velocities))
    }
}

// ✅ 使用: O(1) 内存，按需生成
for (position, velocity) in trajectory_planner {
    piper.motion_commander().command_positions(position)?;
}
```

**优势**:
- ✅ 内存高效: O(1) 内存，无需预分配
- ✅ 惰性计算: 按需生成，节省计算
- ✅ 符合 Rust 习惯: 标准 Iterator trait

---

## 📊 性能评估 (Performance Evaluation)

### 基准测试结果

| 组件 | 性能 | 目标 | 倍数 | 状态 |
|------|------|------|------|------|
| StateTracker (快速路径) | ~18ns | < 100ns | 5.4x | ⚡ 超标 |
| Observer (读取) | ~11ns | < 50ns | 4.5x | ⚡ 超标 |
| TrajectoryPlanner (每步) | ~279ns | < 1µs | 3.6x | ⚡ 超标 |
| PidController (tick) | ~100ns | < 1µs | 10x | ⚡ 优秀 |

### 与 Python SDK 对比

| 指标 | Python SDK | Rust SDK (本提案) | 改进 |
|------|-----------|------------------|------|
| 状态检查 | ~1-5µs (解释器) | ~18ns (原子操作) | **50-250x** |
| 状态读取 | ~10-50µs | ~11ns | **1000-5000x** |
| 轨迹计算 | ~5-10µs | ~279ns | **18-36x** |
| 内存占用 | O(n) | O(1) | **n倍** |

---

## 🧪 测试策略 (Testing Strategy)

### 测试覆盖

- **单元测试**: 593 个
- **集成测试**: Phase 0-4 完整覆盖
- **属性测试**: proptest (单位转换、数值稳定性)
- **性能基准**: Criterion (6 个场景)
- **CI/CD**: GitHub Actions (Ubuntu + macOS, stable + nightly)

### 测试方法

1. **Mock 硬件框架**: `MockCanBus`, `MockHardwareState`
2. **状态机测试**: 编译期 + 运行时
3. **并发测试**: 多线程读写
4. **性能回归**: 基准测试自动化

---

## 📚 示例代码 (Examples)

### 示例 1: 简单点对点移动

```rust
use piper_sdk::high_level::*;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. 连接并使能
    let piper = Piper::connect(ConnectionConfig::default())?
        .enable_position_mode(PositionModeConfig::default())?;

    // 2. 创建轨迹规划器
    let start = piper.observer().joint_positions();
    let end = JointArray::from([Rad(0.5), Rad(1.0), Rad(0.3),
                                 Rad(-0.5), Rad(0.0), Rad(0.2)]);

    let planner = TrajectoryPlanner::new(
        start, end,
        Duration::from_secs(5),
        100.0,  // 100Hz
    );

    // 3. 执行轨迹
    for (position, _velocity) in planner {
        piper.motion_commander().command_positions(position)?;
        thread::sleep(Duration::from_millis(10));
    }

    Ok(())
}
```

### 示例 2: PID 控制

```rust
use piper_sdk::high_level::*;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let piper = Piper::connect(ConnectionConfig::default())?
        .enable_mit_mode(MitModeConfig::default())?;

    let mut pid = PidController::new(target_position)
        .with_gains(10.0, 0.5, 0.1)
        .with_integral_limit(5.0)
        .with_output_limit(50.0);

    let config = LoopConfig {
        frequency_hz: 500.0,
        max_dt: Duration::from_millis(20),
        shutdown_flag: Arc::new(AtomicBool::new(false)),
    };

    run_controller(
        piper.observer(),
        piper.motion_commander(),
        pid,
        config,
    )?;

    Ok(())
}
```

---

## 🔍 安全性分析 (Safety Analysis)

### 编译期保证

1. **状态安全**: Type State 防止非法状态转换
2. **类型安全**: NewType 防止单位混淆
3. **所有权安全**: Rust 所有权模型防止数据竞争

### 运行时保护

1. **状态漂移检测**: `StateMonitor` 20Hz 同步物理状态
2. **心跳保护**: `HeartbeatManager` 50Hz 防止主线程冻结
3. **Poisoned 机制**: Fatal Error 时标记实例为不可用
4. **积分饱和保护**: PID 控制器防止 Integral Windup
5. **输出钳位**: 力矩输出限制保护硬件

---

## 🚀 实施计划 (Implementation Plan)

### 已完成 (Phase 0-4)

- ✅ Phase 0: 项目准备 (~1,000 行, 28 测试)
- ✅ Phase 1: 基础类型系统 (~2,500 行, 90 测试)
- ✅ Phase 2: 读写分离 (~1,440 行, 47 测试)
- ✅ Phase 3: Type State 核心 (~1,000 行, 12 测试)
- ✅ Phase 4: 控制器框架 (~1,500 行, 26 测试)

### 进行中 (Phase 5)

- ⏳ Phase 5: 完善和文档 (预计 5 天)
  - ✅ 示例程序 (3 个)
  - ✅ CHANGELOG
  - ⏳ RFC 文档 (本文档)
  - ⏳ API 文档完善

### 未来路线图 (Phase 6+)

- ⏳ Phase 6: 生产化准备
  - Cartesian 控制完整集成
  - Via Points 支持 (轨迹规划)
  - 更多控制器 (Admittance, Impedance)
  - 文档网站
  - crates.io 发布

---

## 💭 未解决问题 (Unresolved Questions)

1. **Cartesian 控制集成**: 类型已定义，但未完全集成到控制循环
2. **Via Points**: TrajectoryPlanner 需要支持途径点（非零中间速度）
3. **错误恢复策略**: 某些 Recoverable 错误的最佳恢复路径
4. **性能极限**: 是否可以进一步优化到 < 10ns？

---

## 🤝 替代方案 (Alternatives Considered)

### 方案 A: 不使用 Type State

**优点**: 实现更简单
**缺点**: 失去编译期安全保证
**结论**: ❌ 拒绝，编译期安全是核心价值

### 方案 B: 不使用读写分离

**优点**: 架构更简单
**缺点**: 并发不友好，Borrow Checker 限制大
**结论**: ❌ 拒绝，并发是实际需求

### 方案 C: 使用 async/await

**优点**: 现代异步模式
**缺点**: 实时控制不适合异步，增加复杂度
**结论**: ❌ 拒绝，同步模式更适合实时控制

---

## 📖 参考资料 (References)

1. **设计文档系列**:
   - `rust_high_level_api_design_v2.0.md`
   - `rust_high_level_api_design_v3.0.md`
   - `rust_high_level_api_design_v3.1_defensive.md`
   - `rust_high_level_api_design_v3.2_final.md`

2. **实施文档**:
   - `IMPLEMENTATION_TODO_LIST.md` (v1.2)
   - `PHASE0-4_COMPLETION_REPORT.md`

3. **外部参考**:
   - Type State Pattern in Rust
   - Zero-Cost Abstractions in Rust
   - ROS2_control Architecture
   - Python `piper_control` SDK

---

## ✅ 决议 (Decision)

**建议**: **批准** 本 RFC，继续 Phase 5 完成工作，准备 v1.0-alpha 发布。

**理由**:
1. ✅ 核心功能完整 (Phase 0-4)
2. ✅ 性能超标 3-5x
3. ✅ 测试覆盖优秀 (593 个测试)
4. ✅ 文档完善 (26 个文档)
5. ✅ 示例程序可用 (3 个)

**下一步**:
1. 完成 Phase 5 剩余工作
2. 社区反馈收集
3. v1.0-alpha 发布
4. 规划 Phase 6 (生产化)

---

**RFC 状态**: 提案中
**预计发布**: 2026-01-24
**版本**: v1.0-alpha

