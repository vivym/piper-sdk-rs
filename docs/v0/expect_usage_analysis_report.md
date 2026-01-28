# 专项报告 3: expect() 使用矛盾深度审查

**审查日期**: 2026-01-27
**问题等级**: 🟡 P1 - 高风险（设计问题）
**审查范围**: 所有 expect() 调用及其设计合理性
**审查方法**: 设计模式分析和类型系统审查

---

## 执行摘要

**原报告自相矛盾**:
- 3.1 节声称: "未发现生产代码中不当的 panic! 使用"
- 3.2 节列出了 5 个 `expect()` 调用

**技术事实**: `expect()` = `panic!()`（带自定义消息）

**关键发现**:
- 🟡 **3 个 expect() 在 MitController 中**
- 🟡 **存在设计矛盾：Option + expect 的反模式**
- 🟡 **可能导致 panic 的场景被忽略**

---

## 1. 发现的 expect() 调用

### 1.1 完整列表

| 序号 | 位置 | 代码 | 场景 | 风险 |
|------|------|------|------|------|
| 1 | `mit_controller.rs:228` | `self.piper.as_ref().expect("Piper should exist")` | 控制循环开始 | 🟡 中 |
| 2 | `mit_controller.rs:322` | `self.piper.as_ref().expect("Piper should exist")` | PID 控制循环 | 🟡 中 |
| 3 | `mit_controller.rs:401` | `self.piper.take().expect("Piper should exist")` | park() 方法 | 🟡 中 |

---

## 2. 设计问题分析

### 2.1 MitController 结构定义

**位置**: `crates/piper-client/src/control/mit_controller.rs`

```rust
/// MIT 控制器
pub struct MitController {
    /// ⚠️ Option 包装，允许 park() 时安全提取
    piper: Option<Piper<Active<MitMode>>>,

    /// 状态观察器
    observer: Observer,

    // ... 其他字段
}
```

**设计意图**（从注释推断）:
- 使用 `Option<Piper<Active<MitMode>>>` 允许 `park()` 时**提取**内部值
- `park()` 后 Controller 变为"空壳"，不能再使用

---

### 2.2 使用 expect() 的位置

#### A. move_to_position() 方法

**位置**: `mit_controller.rs:228`

```rust
pub fn move_to_position(
    &self,
    target: [Rad; 6],
    threshold: Rad,
    timeout: Duration,
) -> Result<bool, ControlError> {
    // ...

    // 🔴 问题：假设 piper 一定存在
    let _piper = self.piper.as_ref().expect("Piper should exist");

    // 控制循环
    while start.elapsed() < timeout {
        // 使用 _piper 发送命令
        // ...
    }
}
```

**风险场景**:
```rust
let controller = MitController::new(piper, config);

// 场景 1: 先调用 park()
let standby = controller.park(config)?;

// 场景 2: 后续继续使用 controller（错误！）
controller.move_to_position(target, threshold, timeout)?;
// ❌ PANIC: "Piper should exist"
```

---

#### B. run_pid_control_loop() 方法

**位置**: `mit_controller.rs:322`

```rust
pub fn run_pid_control_loop<F>(
    &self,
    target_generator: F,
    timeout: Duration,
) -> Result<(), ControlError>
where
    F: Fn() -> [Rad; 6],
{
    // ...

    // 🔴 同样的问题
    let piper = self.piper.as_ref().expect("Piper should exist");

    // 控制循环
    while start.elapsed() < timeout {
        // ...
    }
}
```

**同样的风险**: 如果先调用了 `park()`，这里会 panic

---

#### C. park() 方法

**位置**: `mit_controller.rs:401`

```rust
pub fn park(mut self, config: DisableConfig) -> Result<Piper<Standby>> {
    // 🔴 这是唯一合理的 expect()：消费 self
    let piper = self.piper.take().expect("Piper should exist");

    // 失能并返回到 Standby 状态
    piper.disable(config)
}
```

**评价**: ✅ **这个 expect() 是合理的**，因为：
1. `park()` 消费 `self`（take ownership）
2. 调用 `park()` 后，Controller 不能再使用
3. 如果 `piper` 已经是 None，说明有严重 bug（重复调用 park）

---

## 3. 根本问题：设计矛盾

### 3.1 类型状态模式的矛盾

**类型状态模式的目标**:
```rust
// 编译时保证：Active 状态才能调用控制方法
impl Piper<Active<MitMode>> {
    pub fn send_command(&self, ...) -> Result<()> { ... }
}

// Standby 状态不能调用
impl Piper<Standby> {
    pub fn send_command(&self, ...) -> Result<()> { ... }
    // ❌ 编译错误：此方法不存在
}
```

**当前 MitController 的设计**:
```rust
pub struct MitController {
    piper: Option<Piper<Active<MitMode>>>,  // ← 运行时检查
}
```

**矛盾点**:
1. **编译时**: 类型状态模式保证 `Piper<Active<MitMode>>` 存在
2. **运行时**: `Option` 又引入了 `None` 的可能性
3. **结果**: 类型系统的保证被 `Option` 抵消了

---

### 3.2 为什么使用 Option？

**推测原因**: 为了实现 `park()` 的"消费"模式

```rust
// park() 需要"提取"内部的 Piper
pub fn park(mut self) -> Piper<Standby> {
    self.piper.take()  // ← 需要 Option 才能 take
}
```

**但是**: 这个设计引入了运行时 panic 的风险

---

## 4. 修复方案（第5轮修正：工程可行性评估）

**🚨 修正说明**（第5轮专家反馈）:
- 原报告对方案 C（类型状态模式）的评估**过于乐观**
- **方案 A 的生命周期传染性**被忽略
- **方案 C 的所有权黑洞**导致用户代码复杂化
- **新增方案 D（算子模式）**：最务实的长期方案

---

### 方案 A: 移除 Option，使用引用（次优）⚠️

**原报告评估**: ✅ 推荐
**修正后评估**: ⚠️ 次优（有工程缺陷）

**实施**:
```rust
pub struct MitController<'a> {
    // ✅ 直接存储引用（生命周期绑定）
    piper: &'a Piper<Active<MitMode>>,
    observer: Observer,
    config: MitControllerConfig,
}

impl<'a> MitController<'a> {
    pub fn new(piper: &'a Piper<Active<MitMode>>, config: MitControllerConfig) -> Self {
        Self {
            piper,
            observer: piper.observer(),
            config,
        }
    }

    pub fn move_to_position(&self, ...) -> Result<bool> {
        self.piper.send_command(...)?;
    }
}
```

#### 🚨 致命缺陷：生命周期传染（Lifetime Poisoning）

**问题**: 引入生命周期 `'a` 后，会传染到所有持有 `MitController` 的结构体

```rust
// 用户想在自己的结构体中持有 controller
struct MyRobot<'a> {
    controller: MitController<'a>,  // ❌ MyRobot 也需要 'a
    // 其他字段...
}

// 更糟糕的情况
struct AppState<'a> {
    robot: MyRobot<'a>,  // ❌ AppState 也需要 'a
    // 更多嵌套结构都需要 'a
}
```

**后果**:
- 🔴 **生命周期爆炸**: 整个应用结构体树都需要生命周期参数
- 🔴 **编译错误地狱**: 初级/中级用户难以理解编译错误
- 🔴 **API 不友好**: 强迫用户学习高级生命周期概念

**结论**: 方案 A 只适合**临时使用**的场景，不适合存储在结构体中

---

### 方案 B: 保留 Option，返回 Result（P1 推荐）✅

**原报告评估**: ⚠️ 次优
**修正后评估**: ✅ **P1 短期最佳方案**

**优点**:
- ✅ 不改变 API 签名
- ✅ 最小代码改动
- ✅ 避免panic
- ✅ 无生命周期问题
- ✅ 用户友好

**缺点**:
- ⚠️ 每次调用都需要检查（但这是合理的）
- ⚠️ 运行时检查（但无法避免）

**实施**:
```rust
impl MitController {
    pub fn move_to_position(&self, ...) -> Result<bool, ControlError> {
        // ✅ 返回错误而非 panic
        let piper = self.piper.as_ref()
            .ok_or(ControlError::AlreadyParked)?;

        // ...
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ControlError {
    #[error("Controller was already parked")]
    AlreadyParked,
    // ... 其他错误
}
```

**用户代码**:
```rust
struct RobotApp {
    controller: Option<MitController>,  // ✅ 无生命周期
}

impl RobotApp {
    fn run(&mut self) {
        if let Some(ctrl) = &self.controller {
            match ctrl.move_to_position(...) {
                Ok(success) => { /* ... */ }
                Err(ControlError::AlreadyParked) => {
                    // 正确处理错误
                }
            }
        }
    }
}
```

**结论**: **当前最佳务实方案**，适合立即实施

---

### 方案 C: 使用类型状态模式（原报告：最佳 ✅✅ → 修正后：不推荐 ❌）

**原报告评估**: ✅✅ 最佳
**修正后评估**: ❌ **不推荐**（工程复杂度过高）

#### 🚨 致命缺陷 1: 所有权黑洞

**问题**: 用户为了存储 `ActiveController`，**依然需要使用 Option**

```rust
struct RobotApp {
    // 🔴 依然需要 Option！
    controller: Option<ActiveController>,
}

impl RobotApp {
    fn shutdown(&mut self) {
        // 🔴 依然需要 take()！
        if let Some(ctrl) = self.controller.take() {
            let standby = ctrl.park(config).unwrap();
            // 🔴 现在又多了一个 Piper<Standby> 需要处理
            // 存到哪里？
        }
    }
}
```

**结论**: **方案 C 并没有消除 Option，只是把 Option 从 SDK 内部推给了用户**

#### 🚨 致命缺陷 2: API 易用性极差

**问题**: `move_to_position` 需要 `&self`，但 `park` 需要 `self`（消费所有权）

```rust
let controller = ActiveController::new(piper, config);

// ✅ 可以调用
controller.move_to_position(...)?;

// ✅ 可以调用
controller.run_pid_control_loop(...)?;

// ❌ 但如果想存储 controller，需要 Option
let mut app = RobotApp {
    controller: Some(controller),
};

// ❌ 调用 move_to_position 需要复杂的 Option 操作
app.controller.as_ref().unwrap().move_to_position(...)?;

// ❌ 调用 park 需要 take()
let standby = app.controller.take().unwrap().park(config)?;
```

**结论**: **方案 C 增加了用户的负担，而不是减轻**

#### 为什么原报告评估错误？

**错误原因**:
1. **只考虑了"理论正确性"**：编译时保证，无 panic
2. **忽略了"工程可用性"**：用户如何在实际项目中使用？
3. **忽略了"所有权管理复杂性"**：`park()` 消费 `self` 后，返回的 `Piper<Standby>` 如何处理？

**教训**:
> **类型安全的完美性 ≠ API 易用性**
> 设计 API 时，必须考虑**用户的使用场景**，而不仅仅是理论上的优雅

---

### 方案 D: 算子模式 / Operator Pattern（P2 推荐）✅✅

**核心思想**: Controller **不持有** Piper 的所有权，Piper 作为**参数传入**

**关键原则**:
- Controller 是**纯逻辑算子**（Algorithm）
- Controller 仅持有**算法状态**（如 PID 积分项）
- Controller **不持有**硬件状态（Observer、Piper）
- 所有硬件状态通过参数传入

**实施**:
```rust
pub struct MitController {
    // ✅ 不持有 Observer（从 Piper 获取）
    // ✅ 不持有 Piper（作为参数传入）
    config: MitControllerConfig,

    // ✅ 仅保留算法相关状态（如 PID 积分误差）
    // 如果算法无状态，可以完全省略
    integral_error: Option<[f64; 6]>,
    last_error: Option<[f64; 6]>,
}

impl MitController {
    pub fn new(config: MitControllerConfig) -> Self {
        Self {
            config,
            integral_error: None,  // 懒初始化
            last_error: None,
        }
    }

    // ✅ Piper 作为参数传入
    // ✅ 使用 &mut self（如果需要更新 PID 状态）
    pub fn move_to_position(
        &mut self,  // ← 改为 &mut self（需要更新算法状态）
        piper: &mut Piper<Active<MitMode>>,  // ← 参数
        target: [Rad; 6],
        threshold: Rad,
        timeout: Duration,
    ) -> Result<bool> {
        let start = Instant::now();

        loop {
            // ✅ 通过参数访问硬件状态（而非内部字段）
            let current = piper.observer().get_joint_positions()?;
            let errors = target.iter()
                .zip(current.iter())
                .map(|(&t, &c)| (t - c).abs())
                .collect::<Vec<_>>();

            // 检查是否到达目标
            if errors.iter().all(|&e| e < threshold) {
                return Ok(true);
            }

            if start.elapsed() > timeout {
                return Ok(false);
            }

            // ✅ 使用算法状态（如果有）
            if let Some(ref integral) = self.integral_error {
                // PID 计算使用积分项...
            }

            // ✅ 发送命令
            piper.send_command(...)?;

            sleep(Duration::from_millis(10));
        }
    }

    // ✅ 同样，piper 作为参数
    pub fn run_pid_control_loop<F>(
        &mut self,  // ← 更新 PID 状态
        piper: &mut Piper<Active<MitMode>>,  // ← 参数
        target_generator: F,
        timeout: Duration,
    ) -> Result<()>
    where
        F: Fn() -> [Rad; 6],
    {
        let start = Instant::now();
        let mut dt_accumulator = Duration::ZERO;

        // 初始化 PID 状态
        let mut last_error = [0.0f64; 6];
        let mut integral = [0.0f64; 6];

        while start.elapsed() < timeout {
            let target = target_generator();
            let current = piper.observer().get_joint_positions()?;

            // ✅ PID 算法（使用局部状态）
            for i in 0..6 {
                let error = target[i].0 - current[i].0;
                integral[i] += error * dt_accumulator.as_secs_f64();
                let derivative = (error - last_error[i]) / dt_accumulator.as_secs_f64();

                let output = self.config.kp[i] * error
                    + self.config.ki[i] * integral[i]
                    + self.config.kd[i] * derivative;

                piper.send_torque(i, output)?;
                last_error[i] = error;
            }

            dt_accumulator = Duration::from_millis(10);
            spin_sleep::sleep(Duration::from_millis(10));
        }

        // ✅ 可选：保存算法状态（用于下次调用）
        self.integral_error = Some(integral);
        self.last_error = Some(last_error);

        Ok(())
    }

    // ❌ 移除 park() 方法
    // 原因：park() 是 Piper 的职责，不属于控制算法
}
```

**用户代码**:
```rust
struct RobotApp {
    // ✅ 无生命周期，无 Option
    controller: MitController,
    piper: Option<Piper<Active<MitMode>>>,
}

impl RobotApp {
    fn new(mut piper: Piper<Active<MitMode>>) -> Self {
        let controller = MitController::new(config);
        Self {
            controller,
            piper: Some(piper),
        }
    }

    fn run(&mut self) {
        if let Some(piper) = &mut self.piper {
            // ✅ 简洁、清晰
            self.controller.move_to_position(
                piper,
                target,
                threshold,
                timeout,
            )?;
        }
    }

    fn shutdown(&mut self) {
        // ✅ 简单、直接
        if let Some(piper) = self.piper.take() {
            // ✅ park() 是 Piper 的方法，不是 Controller 的
            let standby = piper.disable(config)?;
            // standby 可以被存储或丢弃
            self.standby = Some(standby);
        }
    }
}
```

#### 优点对比

| 特性 | 方案 A (引用) | 方案 B (Option) | 方案 C (类型状态) | **方案 D (算子)** |
|------|------------|---------------|-----------------|-----------------|
| **零 Option** | ✅ | ❌ | ✅ (但用户需要) | ✅ |
| **零 Panic** | ✅ | ✅ | ✅ | ✅ |
| **无生命周期** | ❌ (传染性) | ✅ | ✅ | ✅ |
| **用户友好** | 🟡 中 | ✅ 高 | ❌ 低 | ✅✅ **极高** |
| **灵活性** | 🟡 中 | ✅ 高 | ❌ 低 | ✅✅ **极高** |
| **可组合性** | 🟡 中 | ✅ 高 | ❌ 低 | ✅✅ **极高** |
| **无状态冗余** | ❌ | ❌ | ❌ | ✅✅ **纯逻辑** |

---

#### 🔑 关键架构决策：为什么移除 `Observer`？

**第5轮专家反馈修正**:

原方案 D 中，`MitController` 持有 `Observer` 字段：

```rust
// ❌ 原设计（有冗余）
pub struct MitController {
    observer: Observer,  // ❓ 这个 Observer 从哪里获取数据？
    config: MitControllerConfig,
}
```

**问题分析**:

1. **数据来源不明确**: `Observer` 的数据来自底层 CAN 驱动
2. **所有权混乱**: `Piper` 拥有驱动，因此 `Piper` 理应拥有 `Observer`
3. **状态冗余**: 如果 `MitController` 持有自己的 `Observer`，它与 `Piper.observer()` 不同步
4. **违背算子模式**: 算子应该是纯逻辑，不应持有硬件状态

**修正后的设计**:

```rust
// ✅ 修正设计（纯逻辑算子）
pub struct MitController {
    // ✅ 不持有 Observer（通过 piper.observer() 访问）
    // ✅ 不持有 Piper（作为参数传入）
    config: MitControllerConfig,

    // ✅ 仅持有算法状态（如 PID 积分误差）
    integral_error: Option<[f64; 6]>,
    last_error: Option<[f64; 6]>,
}
```

**收益对比**:

| 特性 | 原设计（持有 Observer） | **修正设计（纯逻辑）** |
|------|---------------------|---------------------|
| **状态一致性** | ❌ 需要同步两个 Observer | ✅ 单一数据源 |
| **职责清晰** | ❌ Controller 混杂硬件状态 | ✅ Controller 纯逻辑 |
| **可测试性** | 🟡 需要 Mock Observer | ✅✅ 纯算法，易测试 |
| **可组合性** | 🟡 受 Observer 绑定 | ✅✅ 完全解耦 |
| **线程安全** | ❌ Observer 需要同步 | ✅✅ 无共享状态 |

**示例：测试纯逻辑算子**:

```rust
#[test]
fn test_mit_controller_logic() {
    // ✅ 无需硬件，直接测试控制逻辑
    let controller = MitController::new(config);

    // ✅ 可以使用 Mock Piper
    let mut mock_piper = MockPiper::new();
    mock_piper.set_joint_positions(&[0.0, 0.0, 0.0, 0.0, 0.0, 0.0]);

    // ✅ 测试控制算法
    let result = controller.move_to_position(
        &mut mock_piper,
        [Rad(0.1), Rad(0.1), Rad(0.1), Rad(0.1), Rad(0.1), Rad(0.1)],
        Rad(0.01),
        Duration::from_secs(1),
    );

    assert!(result.is_ok());
}
```

**架构清晰度**:

```
┌─────────────────────────────────────────┐
│  应用层 (User Code)                     │
│  ┌──────────────┐      ┌──────────────┐ │
│  │ MitController│─────│   Piper      │ │
│  │ (纯逻辑算子) │      │ (硬件抽象)   │ │
│  └──────────────┘      └──────────────┘ │
│         │                     │          │
│         │ Algorithm          │ Hardware  │
│         │ State              │ State    │
└─────────┼─────────────────────┼──────────┘
          │                     │
          │                     │
      积分误差、配置        CAN 驱动、Observer
```

**关键原则**:

> **算子模式的核心：算法与硬件完全解耦**
>
> - **Controller**: 纯逻辑（PID、轨迹规划等）
> - **Piper**: 纯硬件抽象（CAN、驱动、状态）
> - **交互点**: 通过方法参数（而非字段）

---

#### 为什么方案 D 是最优？

1. **所有权清晰**: 用户完全控制 Piper 的生命周期
2. **零生命周期传染**: Controller 不带 `'a`
3. **零 Option 灾难**: 用户可以按需使用 Option
4. **符合 Rust 惯用法**: 类似 `Iterator` 的设计
   - `Iterator` 不持有数据，只是操作数据的"算子"
   - `sort_by()` 等方法接收闭包作为"算子"
5. **算法与硬件完全解耦**: Controller 是纯逻辑，Piper 是纯硬件抽象
6. **可组合性强**: 可以轻松切换不同的 Controller
   ```rust
   struct RobotApp {
       controllers: Vec<Box<dyn Controller>>,  // 多态
       piper: Option<Piper<Active<MitMode>>>,
   }
   ```
7. **无状态冗余**: 单一数据源，避免状态同步问题
8. **极易测试**: 无需 Mock 硬件，直接测试算法逻辑

#### 缺点

- ⚠️ API 签名改动较大（每个方法都需要 Piper 参数）
- ⚠️ 调用时需要多传一个参数

**但是**，这些缺点是**可以接受的**，因为：
1. 清晰度 > 简洁性：明确表达依赖关系
2. Rust 惯用法：类似 `std::sort_slice(slice, cmp)` 的设计
3. 长期收益：更好的可维护性和可测试性

---

### 方案对比总结（第5轮修正）

| 方案 | 原评级 | 修正后评级 | 适用场景 | 主要问题 |
|------|--------|----------|----------|----------|
| **A (引用)** | ✅ 推荐 | ⚠️ 次优 | 临时使用 | 生命周期传染 |
| **B (Option+Result)** | ⚠️ 次优 | ✅ **P1 推荐** | **短期修复** | 运行时检查 |
| **C (类型状态)** | ✅✅ 最佳 | ❌ **不推荐** | ❌ 无 | 所有权黑洞 |
| **D (算子)** | ❌ 未提及 | ✅✅ **P2 推荐** | **长期重构** | API 改动大 |

---

## 5. 修正后的行动计划（第5轮）

### P1 - 短期修复（0.1.0 前，1 天）✅

**任务: 使用方案 B（Option + Result）修复 expect()**

**理由**:
- ✅ 最小代码改动
- ✅ 避免panic
- ✅ 无生命周期问题
- ✅ 用户友好

**实施步骤**:

```rust
// 1. 添加错误类型
#[derive(Debug, thiserror::Error)]
pub enum ControlError {
    #[error("Controller was already parked, cannot execute commands")]
    AlreadyParked,

    // ... 现有错误
}

// 2. 修改所有 expect() 为 ok_or()
impl MitController {
    pub fn move_to_position(&self, ...) -> Result<bool, ControlError> {
        let piper = self.piper.as_ref()
            .ok_or(ControlError::AlreadyParked)?;

        // ...
    }

    pub fn run_pid_control_loop(&self, ...) -> Result<(), ControlError> {
        let piper = self.piper.as_ref()
            .ok_or(ControlError::AlreadyParked)?;

        // ...
    }

    pub fn park(mut self, config: DisableConfig) -> Result<Piper<Standby>, ControlError> {
        let piper = self.piper.take()
            .ok_or(ControlError::AlreadyParked)?;

        piper.disable(config).map_err(ControlError::DisableFailed)
    }
}
```

**工作量估计**: 2-3 小时

---

### P2 - 长期重构（0.2.0，2-3 天）✅✅

**任务: 重构为方案 D（算子模式 / Operator Pattern）**

**理由**:
- ✅ 所有权最清晰
- ✅ 零生命周期传染
- ✅ 最符合 Rust 惯用法
- ✅ 可组合性最强

**实施步骤**:

#### 步骤 1: 修改 MitController 结构体

```rust
// 旧设计
pub struct MitController {
    piper: Option<Piper<Active<MitMode>>>,  // ❌ 删除
    observer: Observer,
    config: MitControllerConfig,
}

// 新设计
pub struct MitController {
    // ✅ 不持有 Piper
    observer: Observer,
    config: MitControllerConfig,
}
```

#### 步骤 2: 修改所有方法签名

```rust
// 旧签名
pub fn move_to_position(&self, ...) -> Result<bool>;

// 新签名
pub fn move_to_position(
    &self,
    piper: &mut Piper<Active<MitMode>>,  // ← 新增参数
    ...
) -> Result<bool>;
```

#### 步骤 3: 更新用户代码

```rust
// 旧用法
controller.move_to_position(target, threshold, timeout)?;

// 新用法
controller.move_to_position(&mut piper, target, threshold, timeout)?;
```

#### 步骤 4: 废弃 park() 方法

**原因**: `park()` 应该由 `Piper` 自己提供，不需要 Controller

```rust
// 旧设计（有问题的）
controller.park(config)?;

// 新设计（清晰的）
let standby = piper.disable(config)?;
```

**工作量估计**: 2-3 天（包含文档更新和用户迁移）

---

### 优先级总结

| 优先级 | 方案 | 时间 | 理由 |
|--------|------|------|------|
| **P1** | B (Option+Result) | 2-3 小时 | 立即修复 panic，最小改动 |
| **P2** | D (算子模式) | 2-3 天 | 长期最优设计，需要 API 变更 |
| ❌ 不推荐 | C (类型状态) | - | 工程复杂度过高 |
| ⚠️ 次优 | A (引用) | - | 生命周期传染 |

---

## 6. 测试计划（更新）

### P1 测试（方案 B）

```rust
#[test]
fn test_controller_after_park_should_error() {
    let mut controller = MitController::new(piper, config);

    // 正常使用
    assert!(controller.move_to_position(...).is_ok());

    // Park
    let standby = controller.park(config).unwrap();

    // 后续使用应该返回错误（而非 panic）
    let result = controller.move_to_position(...);
    assert!(matches!(result, Err(ControlError::AlreadyParked)));
}

#[test]
fn test_controller_double_park() {
    let mut controller = MitController::new(piper, config);

    let _standby = controller.park(config).unwrap();

    // 重复 park 应该返回错误（而非 panic）
    let result = controller.park(config);
    assert!(matches!(result, Err(ControlError::AlreadyParked)));
}
```

### P2 测试（方案 D）

```rust
#[test]
fn test_operator_pattern() {
    let controller = MitController::new(config);
    let mut piper = Piper::enable_mit_mode(...)?;

    // ✅ 正常工作
    assert!(controller.move_to_position(&mut piper, ...).is_ok());

    // ✅ 可以多次调用
    assert!(controller.move_to_position(&mut piper, ...).is_ok());

    // ✅ park 是独立的操作
    let standby = piper.disable(config)?;
}

#[test]
fn test_operator_composition() {
    let controller = MitController::new(config);
    let mut piper = Piper::enable_mit_mode(...)?;

    // ✅ 可以组合多个操作
    controller.move_to_position(&mut piper, [0.1, 0.2, ...], ...)?;
    controller.run_pid_control_loop(&mut piper, || {...}, ...)?;
    controller.move_to_position(&mut piper, [0.0, 0.0, ...], ...)?;
}
```

---

## 7. 其他语言的类似问题（保持不变）

这个问题在 Rust 社区有广泛讨论：

**参考**:
- [Rust API Guidelines: Use types to prevent invalid states](https://rust-lang.github.io/api-guidelines/type-safety.html)
- [Making Invalid States Unrepresentable](https://www.youtube.com/watch?v=Ib_pTb1CqQs)
- [The Option Pattern in Rust](https://doc.rust-lang.org/book/ch06/01-iflet.html)

**核心原则**:
> **Make invalid states unrepresentable**
> （让无效状态在编译时就无法表示）

**第5轮修正**:
> **But also consider engineering usability**
> （让 API 既安全又易用）

---

## 8. 总结（第5轮修正）

### 8.1 问题总结（不变）

| 项目 | 当前状态 | 问题 |
|------|----------|------|
| expect() 数量 | 3 个 | 都在 MitController 中 |
| 设计模式 | Option + expect | 反模式，存在运行时 panic 风险 |
| 类型系统 | 未充分利用 | Option 抵消了类型状态的优势 |

### 8.2 风险评估（不变）

| 风险 | 等级 | 触发条件 |
|------|------|----------|
| 先 park 后使用 | 🟡 中 | 用户误用 API |
| 重复调用 park | 🟢 低 | 需要明显的错误 |

### 8.3 修正后的方案评估（第5轮）

| 方案 | 优先级 | 理由 |
|------|--------|------|
| **A (引用)** | ⚠️ 次优 | 生命周期传染，不适合存储 |
| **B (Option+Result)** | ✅ **P1** | 短期最佳，最小改动 |
| **C (类型状态)** | ❌ 不推荐 | **所有权黑洞，用户负担重** |
| **D (算子)** | ✅✅ **P2** | 长期最优，零生命周期 |

### 8.4 关键教训（第5轮新增）

1. **理论完美 ≠ 工程可用**
   - 方案 C 在理论上完美（编译时保证）
   - 但在工程中极难使用（所有权黑洞）

2. **必须考虑用户场景**
   - 用户通常需要将 Controller 存储在结构体中
   - 不能只考虑"临时使用"的场景

3. **生命周期是 Rust API 设计的第一性原理**
   - 引入生命周期参数会传染整个类型树
   - 必须极其谨慎

4. **清晰度 > 简洁性**
   - 方案 D 需要多传一个参数
   - 但所有权关系更清晰，长期收益更大

5. **算子模式的核心：算法与硬件完全解耦**（第5轮修正新增）
   - Controller 应该是**纯逻辑算子**（如 PID 算法）
   - Controller **不应持有硬件状态**（如 Observer）
   - 所有硬件状态通过参数传入（`piper.observer()`）
   - 收益：单一数据源、职责清晰、易测试、可组合

6. **状态冗余是架构设计的隐形杀手**（第5轮修正新增）
   - 如果 Controller 持有 `Observer`，它与 `Piper.observer()` 不同步
   - 状态冗余导致数据一致性、线程安全、测试复杂性问题
   - **单一数据源原则**: `Piper` 是硬件状态的唯一来源

---

**报告生成**: 2026-01-27 (v5.1 - 第5轮架构纯净性修正)
**审查人员**: AI Code Auditor
**专家反馈**: 5轮深度审查，修正了理论完美但工程灾难的问题，并优化了算子模式的架构纯净性

**关键修正历程**:
- 第1-4轮: 发现 expect() 问题，提出方案 A/B/C
- **第5轮第一阶段**: 修正方案 C 的过度乐观，提出方案 D（算子模式）
- **第5轮第二阶段**: 移除方案 D 中的 `Observer` 字段，实现真正的纯逻辑算子

---

**下一步行动**（按优先级）:
1. **P1 (0.1.0 前)**: 实施方案 B（Option + Result），修复 panic
2. **P2 (0.2.0)**: 评估并实施方案 D（算子模式 - 纯逻辑版本），长期最优设计

---

**特别致谢**（第5轮第二阶段）:
感谢专家对方案 D 的精准架构审查，指出了 `Observer` 字段的冗余问题。这一修正让方案 D 从"算子模式雏形"提升到了"**真正的纯逻辑算子**"，实现了算法与硬件的完全解耦。
