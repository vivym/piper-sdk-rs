# Piper SDK MIT Mode Control 调研报告

**日期**: 2025-01-29
**版本**: v0.0.3
**调研范围**: piper-sdk 架构中的 MIT mode 支持情况

---

## 执行摘要

### 核心发现

** misconception**: "piper-sdk 没有 MIT mode control"

**实际情况**: **MIT mode 已完全实现**，但存在以下问题：

1. ✅ **协议层**: 100% 完整实现
2. ✅ **驱动层**: 100% 完整支持
3. ⚠️ **客户端层**: 已实现但**缺少文档和示例**
4. ❌ **用户体验**: API 设计改变导致使用门槛高

### 问题根源

不是功能缺失，而是：
- **API 设计哲学转变**: 从简单同步 API → 类型状态 + 异步架构
- **文档缺失**: 没有专门的 MIT mode 示例
- **可发现性差**: `MitController` 等高级 API 未导出到 prelude

---

## 1. 架构分层分析

### 1.1 协议层 (piper-protocol) ✅ 完全实现

**位置**: `crates/piper-protocol/src/control.rs`

#### 实现内容

```rust
// MIT 模式枚举 (line 55-63)
pub enum MitMode {
    PositionVelocity = 0x00,
    Mit = 0xAD,
}

// MIT 控制命令 (line 1398-1639)
pub struct MitControlCommand {
    pub joint_index: u8,    // 关节索引 [1, 6]
    pub pos_ref: f32,       // 位置参考: -12.5 ~ 12.5 弧度
    pub vel_ref: f32,       // 速度参考: -45.0 ~ 45.0 rad/s
    pub kp: f32,            // 比例增益: 0.0 ~ 500.0
    pub kd: f32,            // 微分增益: -5.0 ~ 5.0
    pub t_ref: f32,         // 力矩参考: -18.0 ~ 18.0 Nm
}

// 编码为 CAN 帧
impl MitControlCommand {
    pub fn to_frame(&self) -> PiperFrame {
        // 复杂的位域打包（12位跨字节边界）
        // CRC 计算（4位 XOR）
    }
}
```

#### 特性

- ✅ **完整的参数范围支持**
- ✅ **CRC 校验**（4-bit XOR）
- ✅ **跨字节位域打包**（12-bit pos/vel/torque）
- ✅ **单元测试覆盖**（lines 1642-1725）
- ✅ **CAN ID 定义**: 0x15A-0x15F (关节 1-6)

**状态**: **生产就绪** ✅

---

### 1.2 驱动层 (piper-driver) ✅ 完全支持

**位置**: `crates/piper-driver/src/`

#### 实现内容

```rust
// Piper impl
impl Piper {
    /// 发送实时控制命令包（零分配）
    pub fn send_realtime_package(
        &self,
        package: [PiperFrame; 6]
    ) -> Result<(), DriverError> {
        // 使用栈缓冲区，避免堆分配
        // 通过专用通道发送（可覆盖模式）
    }
}
```

#### 特性

- ✅ **实时优先级**（覆盖式队列）
- ✅ **批量发送**（6帧一组，确保原子性）
- ✅ **零拷贝**（栈缓冲区）
- ✅ **高频支持**（200Hz+）

**状态**: **生产就绪** ✅

---

### 1.3 客户端层 (piper-client) ⚠️ 已实现但文档缺失

**位置**: `crates/piper-client/src/`

#### 已实现的功能

##### 1. 类型状态模式 (Type State Pattern)

```rust
// state/machine.rs (line 35)
pub struct MitMode;  // 零尺寸类型标记

// 状态转换 (line 468-527)
impl Piper<Standby> {
    pub fn enable_mit_mode(
        self,
        config: MitModeConfig
    ) -> Result<Piper<Active<MitMode>>> {
        // 编译时保证：只能在 Standby 状态调用
        // 返回新的状态：Active<MitMode>
    }
}

// MIT 模式专用方法 (line 1458-1469)
impl Piper<Active<MitMode>> {
    /// 批量发送力矩命令（低级 API）
    pub fn command_torques(
        &self,
        pos_ref: &JointArray<Rad>,
        vel_ref: &JointArray<f64>,
        kp: &JointArray<f64>,
        kd: &JointArray<f64>,
        t_ref: &JointArray<NewtonMeter>,
    ) -> Result<()> {
        // 使用 RawCommander::send_mit_command_batch
    }
}
```

**优点**:
- ✅ 编译时状态检查（不可能在错误状态调用 MIT 方法）
- ✅ 自动状态转换管理
- ✅ 零运行时开销（零尺寸类型）

**缺点**:
- ❌ API 冗长（需要显式状态转换）
- ❌ 学习曲线陡峭

##### 2. 批量命令发送器

```rust
// raw_commander.rs (line 39-111)
impl Piper<Active<MitMode>> {
    pub fn send_mit_command_batch(
        &self,
        commands: &[MitControlCommand; 6]
    ) -> Result<()> {
        // 发送所有 6 个关节的 MIT 命令
        // 使用驱动层的 send_realtime_package
    }
}
```

**设计决策**: **仅支持批量模式**
- 原因：CAN 邮箱的覆盖策略
- 单关节发送会导致竞争条件
- 批量模式确保所有 6 个关节原子更新

##### 3. 高级 MIT 控制器 ⭐

```rust
// control/mit_controller.rs
pub struct MitController {
    robot: Piper<Active<MitMode>>,
    config: MitControllerConfig,
}

impl MitController {
    /// 创建 MIT 控制器
    pub fn new(
        robot: Piper<Active<MitMode>>,
        config: MitControllerConfig
    ) -> Self { ... }

    /// 阻塞式位置控制（带循环锚定机制）
    pub fn move_to_position(
        &mut self,
        target: &JointArray<Rad>,
        threshold: &JointArray<Rad>,
        timeout: Duration,
    ) -> Result<()> {
        // 200Hz 控制循环
        // 精确定时（循环锚定）
        // 阻塞直到到达目标
    }

    /// 放松关节（发送零力矩）
    pub fn relax_joints(&mut self) -> Result<()> { ... }

    /// 停止并返回 Standby 状态
    pub fn park(self) -> Result<Piper<Standby>> { ... }
}
```

**特性**:
- ✅ **200Hz 控制循环**（精确计时）
- ✅ **循环锚定机制**（避免漂移）
- ✅ **阻塞 API**（易于使用）
- ✅ **自动清理**（drop 时自动休息）

**配置参数**:
```rust
pub struct MitControllerConfig {
    pub kp_gains: [f64; 6],      // 每关节比例增益
    pub kd_gains: [f64; 6],      // 每关节微分增益
    pub control_rate: f64,       // 控制频率 (Hz)
    pub timeout: Duration,       // 默认超时
}
```

---

#### 缺失的功能

##### 1. ❌ 单关节控制方法

**旧 SDK**:
```rust
pub fn send_joint_mit_control(
    &self,
    control: &JointMitControl
) -> Result<()>
```

**新 SDK**: 只有批量模式
```rust
// 必须发送所有 6 个关节
robot.command_torques(&pos, &vel, &kp, &kd, &torque)?;
```

**影响**:
- 无法单独测试某个关节
- 必须每次构造 6 个关节的数据

**解决方案** (可选，低优先级):
```rust
// impl Piper<Active<MitMode>>
pub fn command_joint_torque(
    &self,
    joint: Joint,
    pos_ref: Rad,
    vel_ref: f64,
    kp: f64,
    kd: f64,
    torque_ref: NewtonMeter,
) -> Result<()> {
    // 内部使用批量模式
    // 获取其他关节的当前位置
    let current = self.observer().joint_positions();
    // ... 构造批量命令
}
```

##### 2. ❌ 简化的 enable 方法

**旧 SDK**:
```rust
piper.enable_mit_mode(true)?;  // 简单
```

**新 SDK**:
```rust
let config = MitModeConfig::default();
let robot = robot.enable_mit_mode(config)?;
```

**影响**: 用户需要知道 `MitModeConfig::default()`

**解决方案** (可选):
```rust
// impl Piper<Standby>
pub fn enable_mit_mode_simple(self) -> Result<Piper<Active<MitMode>>> {
    self.enable_mit_mode(MitModeConfig::default())
}
```

##### 3. ❌ 文档和示例

**问题**:
- 没有专门的 `mit_mode_demo.rs`
- 没有说明何时使用 `command_torques()` vs `MitController`
- `MitController` 未导出到 prelude

**影响**: 用户无法发现高级 API

---

## 2. 新旧 SDK 对比

### 2.1 API 对比表

| 特性 | 旧 SDK (piper_sdk_rs) | 新 SDK (piper-sdk) |
|------|----------------------|-------------------|
| **状态安全** | 运行时检查 | 编译时 (Type State) |
| **单关节控制** | ✅ `send_joint_mit_control()` | ❌ 仅批量模式 |
| **启用/禁用** | ✅ `enable_mit_mode(bool)` | ⚠️ `enable_mit_mode(config)` |
| **高级控制器** | ❌ 无 | ✅ `MitController` |
| **文档示例** | ✅ `mit_control.rs` | ❌ 无专用示例 |
| **类型安全** | ⚠️ 运行时错误 | ✅ 编译时保证 |
| **控制循环** | 手动实现 | ✅ 内置 (MitController) |
| **循环锚定** | ❌ 无 | ✅ 有 (200Hz 精确) |

### 2.2 代码对比

#### 旧 SDK API

```rust
// 简单直观
use piper_sdk_rs::{PiperInterface, JointMitControl};

let piper = PiperInterface::new("can0")?;

// 启用 MIT 模式
piper.enable_mit_mode(true)?;

// 发送单个关节命令
let mit_ctrl = JointMitControl::new(
    1,      // 关节 1
    0.5,    // pos_ref
    0.0,    // vel_ref
    10.0,   // kp
    0.8,    // kd
    0.0     // t_ref
);
piper.send_joint_mit_control(&mit_ctrl)?;

// 禁用
piper.enable_mit_mode(false)?;
```

#### 新 SDK API (低级)

```rust
// 类型安全但冗长
use piper_sdk::PiperBuilder;
use piper_sdk::client::state::*;
use piper_sdk::client::types::*;

let robot = PiperBuilder::new()
    .interface("can0")
    .build()?;

// 启用 MIT 模式（需要显式配置）
let config = MitModeConfig::default();
let mut robot = robot.enable_mit_mode(config)?;

// 批量发送命令（必须提供所有 6 个关节）
let positions = JointArray::from([
    Rad(0.5), Rad(0.0), Rad(0.0),
    Rad(0.0), Rad(0.0), Rad(0.0)
]);
let velocities = JointArray::from([0.0; 6]);
let kp = JointArray::from([10.0; 6]);
let kd = JointArray::from([0.8; 6]);
let torques = JointArray::from([NewtonMeter(0.0); 6]);

robot.command_torques(&positions, &velocities, &kp, &kd, &torques)?;

// 禁用（自动状态转换）
drop(robot);  // 或 robot.disable()
```

#### 新 SDK API (高级) ⭐

```rust
use piper_sdk::PiperBuilder;
use piper_sdk::client::state::*;
use piper_sdk::client::control::{MitController, MitControllerConfig};
use piper_sdk::client::types::*;

let robot = PiperBuilder::new()
    .interface("can0")
    .build()?;

// 启用 MIT 模式
let config = MitModeConfig::default();
let robot = robot.enable_mit_mode(config)?;

// 使用高级控制器
let mut controller = MitController::new(
    robot,
    MitControllerConfig {
        kp_gains: [5.0; 6],
        kd_gains: [0.8; 6],
        control_rate: 200.0,
        ..Default::default()
    }
);

// 移动到目标位置（阻塞）
let target = JointArray::from([
    Rad(0.5), Rad(0.0), Rad(0.0),
    Rad(0.0), Rad(0.0), Rad(0.0)
]);
let threshold = JointArray::from([
    Rad(0.01), Rad(0.01), Rad(0.01),
    Rad(0.01), Rad(0.01), Rad(0.01)
]);
controller.move_to_position(&target, &threshold, Duration::from_secs(5))?;

// 返回 Standby
let robot = controller.park()?;
```

---

## 3. 设计哲学分析

### 3.1 从旧 SDK 到新 SDK 的转变

#### 核心设计决策

1. **类型状态模式** (Type State Pattern)
   - **目标**: 编译时状态安全
   - **实现**: 零尺寸类型标记 (`Standby`, `Active<MitMode>`)
   - **权衡**: API 冗长 vs 运行时安全

2. **批量优先** (Batch-Only Mode)
   - **目标**: 避免竞争条件
   - **实现**: 仅支持 6 关节批量发送
   - **权衡**: 灵活性 vs 原子性

3. **高级抽象** (High-Level Abstraction)
   - **目标**: 简化常见用例
   - **实现**: `MitController` 包装器
   - **权衡**: API 表面面积 vs 易用性

4. **安全优先** (Safety Over Convenience)
   - **目标**: 强制显式配置
   - **实现**: `MitModeConfig` 而非 `bool`
   - **权衡**: 冗长 vs 明确

### 3.2 架构优势

#### 新 SDK 的优势

1. **编译时安全**
```rust
// 旧 SDK: 运行时错误
piper.enable_mit_mode(true)?;
piper.send_joint_mit_control(&ctrl)?;  // 如果未启用会怎样？运行时 panic

// 新 SDK: 编译时错误
let robot = PiperBuilder::new().build()?;
robot.command_torques(...)?;  // ❌ 编译错误: 必须先 enable_mit_mode
```

2. **状态管理自动化**
```rust
// 旧 SDK: 手动管理
piper.enable_mit_mode(true)?;
// ... 使用 MIT 模式
piper.enable_mit_mode(false)?;  // 必须记得禁用

// 新 SDK: 自动清理
{
    let robot = robot.enable_mit_mode(config)?;
    // ... 使用 MIT 模式
}  // 自动 drop → 自动禁用
```

3. **精确控制循环**
```rust
// 旧 SDK: 手动实现控制循环
let start = Instant::now();
while start.elapsed() < duration {
    let t1 = Instant::now();
    // ... 计算并发送命令
    let t2 = Instant::now();
    std::thread::sleep(loop_rate - (t2 - t1));  // 漂移累积
}

// 新 SDK: 内置循环锚定
controller.move_to_position(target, threshold, timeout)?;
// 200Hz 精确定时，无漂移
```

4. **批量原子性**
```rust
// 旧 SDK: 逐个发送（竞争条件）
for joint in 1..=6 {
    piper.send_joint_mit_control(&commands[joint])?;
}
// 问题: 关节 1 的命令可能在关节 6 之前到达

// 新 SDK: 批量原子发送
robot.command_torques(&pos, &vel, &kp, &kd, &torque)?;
// 所有 6 个关节同时到达
```

---

## 4. 问题分析

### 4.1 为什么用户认为"没有 MIT mode"？

#### 原因分析

1. **文档缺失** (主要因素)
   - 没有专门的 `mit_mode_demo.rs`
   - `multi_threaded_demo.rs` 没有突出 MIT mode 功能
   - `MitController` 未在文档中展示

2. **API 发现困难**
   - `MitController` 在 `control` 子模块中
   - 需要显式导入: `use piper_client::control::MitController`
   - 用户可能不知道高级 API 的存在

3. **API 理解门槛**
   - Type State Pattern 对新手不友好
   - 需要理解状态转换和生命周期
   - 示例代码缺少注释解释

4. **与旧 SDK 不兼容**
   - 旧用户找不到 `enable_mit_mode(bool)`
   - 旧用户找不到 `send_joint_mit_control()`
   - 迁移成本高

### 4.2 真实情况

**功能完整性**: ✅ 100% 实现
- 协议层: 完整
- 驱动层: 完整
- 客户端层: 完整（包括高级抽象）

**用户体验**: ⚠️ 需要改进
- 缺少示例和文档
- API 发现困难
- 学习曲线陡峭

---

## 5. 改进建议

### 5.1 优先级 P0: 文档和示例（立即实施）

#### 1. 创建专门的 MIT mode 示例

**文件**: `crates/piper-sdk/examples/mit_mode_demo.rs`

**内容**:
```rust
//! MIT Mode Control Demo
//!
//! This example demonstrates how to use MIT mode for:
//! - Direct torque control
//! - Impedance control
//! - Gravity compensation

use piper_sdk::PiperBuilder;
use piper_sdk::client::state::*;
use piper_sdk::client::control::{MitController, MitControllerConfig};
use piper_sdk::client::types::*;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // ============================================================
    // Method 1: Low-Level API (Direct Torque Control)
    // ============================================================
    println!("Method 1: Low-Level API\n");

    let robot = PiperBuilder::new().interface("can0").build()?;
    let config = MitModeConfig::default();
    let mut robot = robot.enable_mit_mode(config)?;

    // Pure torque control (gravity compensation)
    loop {
        let observer = robot.observe()?;

        // Read current positions
        let current_pos = observer.joint_positions();

        // Compute gravity torques (using piper-physics)
        let torques = compute_gravity(&current_pos);

        // Send torque commands (zero impedance)
        let kp = JointArray::from([0.0; 6]);  // No position stiffness
        let kd = JointArray::from([0.0; 6]);  // No damping
        robot.command_torques(&current_pos, &current_pos.velocities, &kp, &kd, &torques)?;

        std::thread::sleep(Duration::from_millis(5));  // 200Hz
    }

    // ============================================================
    // Method 2: High-Level API (MitController)
    // ============================================================
    println!("Method 2: High-Level API\n");

    let robot = PiperBuilder::new().interface("can0").build()?;
    let config = MitModeConfig::default();
    let robot = robot.enable_mit_mode(config)?;

    let controller_config = MitControllerConfig {
        kp_gains: [10.0; 6],
        kd_gains: [1.0; 6],
        control_rate: 200.0,
        ..Default::default()
    };
    let mut controller = MitController::new(robot, controller_config);

    // Move to target position (blocking)
    let target = JointArray::from([
        Rad(0.5), Rad(0.0), Rad(0.0),
        Rad(0.0), Rad(0.0), Rad(0.0)
    ]);
    let threshold = JointArray::from([
        Rad(0.01), Rad(0.01), Rad(0.01),
        Rad(0.01), Rad(0.01), Rad(0.01)
    ]);

    controller.move_to_position(&target, &threshold, Duration::from_secs(5))?;

    // Relax joints
    controller.relax_joints()?;

    // Park (return to Standby)
    let robot = controller.park()?;

    Ok(())
}
```

#### 2. 添加 API 文档注释

```rust
// crates/piper-client/src/state/machine.rs

impl Piper<Standby> {
    /// Enable MIT ( impedance) mode for torque control
    ///
    /// MIT mode allows direct torque control with optional impedance:
    /// - Pure torque control: Set kp=0, kd=0
    /// - Impedance control: Set kp>0, kd>0
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use piper_sdk::PiperBuilder;
    /// use piper_sdk::client::state::*;
    ///
    /// let robot = PiperBuilder::new().interface("can0").build()?;
    ///
    /// // Enable MIT mode with default configuration
    /// let config = MitModeConfig::default();
    /// let mut robot = robot.enable_mit_mode(config)?;
    ///
    /// // Now you can use MIT mode methods
    /// let observer = robot.observe()?;
    /// ```
    pub fn enable_mit_mode(
        self,
        config: MitModeConfig
    ) -> Result<Piper<Active<MitMode>>> { ... }
}
```

### 5.2 优先级 P1: 导出便捷（短期实施）

#### 1. 将 `MitController` 添加到 prelude

**文件**: `crates/piper-client/src/lib.rs`

```rust
// Re-exports for convenience
pub use control::{MitController, MitControllerConfig};
```

**效果**: 用户可以直接使用
```rust
use piper_sdk::{MitController, MitControllerConfig};
// 而不是
use piper_client::control::MitController;
```

### 5.3 优先级 P2: API 便捷方法（可选）

#### 1. 简化的 enable 方法

```rust
// impl Piper<Standby>
pub fn enable_mit_mode_simple(self) -> Result<Piper<Active<MitMode>>> {
    self.enable_mit_mode(MitModeConfig::default())
}
```

#### 2. 单关节控制方法

```rust
// impl Piper<Active<MitMode>>
pub fn command_joint_torque(
    &self,
    joint: Joint,
    pos_ref: Rad,
    vel_ref: f64,
    kp: f64,
    kd: f64,
    torque_ref: NewtonMeter,
) -> Result<()> {
    // Get current state
    let observer = self.observe()?;
    let current_pos = observer.joint_positions();
    let current_vel = observer.joint_velocities();

    // Build batch command
    let mut pos = *current_pos;
    pos[joint] = pos_ref;

    let mut vel = *current_vel;
    vel[joint] = vel_ref;

    let mut kp_array = JointArray::from([0.0; 6]);
    kp_array[joint] = kp;

    let mut kd_array = JointArray::from([0.0; 6]);
    kd_array[joint] = kd;

    let mut torques = JointArray::from([NewtonMeter(0.0); 6]);
    torques[joint] = torque_ref;

    self.command_torques(&pos, &vel, &kp_array, &kd_array, &torques)
}
```

**注意**: 这是**低优先级**，因为：
- 大多数用例需要所有 6 个关节
- 增加代码维护成本
- 用户可以自己实现（使用批量 API）

---

## 6. 迁移指南

### 6.1 从旧 SDK 迁移到新 SDK

#### 场景 1: 纯力矩控制（重力补偿）

**旧 SDK**:
```rust
piper.enable_mit_mode(true)?;

loop {
    let state = piper.get_joint_high_speed_states()?;
    let torques = compute_gravity(&state.q);

    for i in 1..=6 {
        let ctrl = JointMitControl::new(i, 0.0, 0.0, 0.0, 0.0, torques[i-1]);
        piper.send_joint_mit_control(&ctrl)?;
    }

    std::thread::sleep(Duration::from_millis(5));
}
```

**新 SDK** (低级 API):
```rust
let config = MitModeConfig::default();
let mut robot = robot.enable_mit_mode(config)?;

loop {
    let observer = robot.observe()?;
    let pos = observer.joint_positions();
    let torques = compute_gravity(&pos);

    let kp = JointArray::from([0.0; 6]);  // 纯力矩控制
    let kd = JointArray::from([0.0; 6]);
    let t_ref = torques;  // JointArray -> JointArray conversion

    robot.command_torques(&pos, &pos.velocities, &kp, &kd, &t_ref)?;

    std::thread::sleep(Duration::from_millis(5));
}
```

#### 场景 2: 位置控制（阻抗模式）

**旧 SDK**:
```rust
piper.enable_mit_mode(true)?;

for i in 1..=6 {
    let ctrl = JointMitControl::new(i, target[i-1], 0.0, 10.0, 0.8, 0.0);
    piper.send_joint_mit_control(&ctrl)?;
}

// 手动控制循环等待到达
loop {
    let state = piper.get_joint_high_speed_states()?;
    if reached_target(&state.q, &target) {
        break;
    }
    // 持续发送命令...
    std::thread::sleep(Duration::from_millis(5));
}
```

**新 SDK** (高级 API):
```rust
let config = MitModeConfig::default();
let robot = robot.enable_mit_mode(config)?;

let controller_config = MitControllerConfig {
    kp_gains: [10.0; 6],
    kd_gains: [0.8; 6],
    control_rate: 200.0,
    ..Default::default()
};
let mut controller = MitController::new(robot, controller_config);

let target = JointArray::from([
    Rad(0.5), Rad(0.0), Rad(0.0),
    Rad(0.0), Rad(0.0), Rad(0.0)
]);
let threshold = JointArray::from([
    Rad(0.01), Rad(0.01), Rad(0.01),
    Rad(0.01), Rad(0.01), Rad(0.01)
]);

// 自动控制循环（阻塞）
controller.move_to_position(&target, &threshold, Duration::from_secs(5))?;
```

---

## 7. 重力补偿实现方案

### 7.1 基于调研结果的实现方案

#### 使用新 SDK 的低级 API

```rust
use piper_sdk::PiperBuilder;
use piper_physics::{MujocoGravityCompensation, GravityCompensation, JointState};
use piper_sdk::client::state::*;
use piper_sdk::client::types::*;
use std::time::Duration;
use std::thread;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. 初始化物理引擎
    let mut gravity_calc = MujocoGravityCompensation::from_embedded()?;

    // 2. 连接机械臂
    let driver = PiperBuilder::new().interface("can0").build()?;

    // 3. 启用 MIT 模式
    let config = MitModeConfig::default();
    let mut robot = driver.enable_mit_mode(config)?;

    // 4. 控制循环 (200Hz)
    let loop_rate = Duration::from_millis(5);

    loop {
        let loop_start = std::time::Instant::now();

        // 读取当前状态
        let observer = robot.observe()?;
        let positions = observer.joint_positions();

        // 转换为 piper-physics 类型
        let q: JointState = positions.into();

        // 计算重力补偿力矩
        let torques = gravity_calc.compute_gravity_compensation(&q)?;

        // 转换为 piper-sdk 类型
        let t_ref = JointArray::from(
            torques.as_slice().map(|t| NewtonMeter(t))
        );

        // 发送 MIT 命令（纯力矩，零阻抗）
        let kp = JointArray::from([0.0; 6]);  // 无位置刚度
        let kd = JointArray::from([0.0; 6]);  // 无阻尼
        robot.command_torques(&positions, &positions.velocities, &kp, &kd, &t_ref)?;

        // 维持控制频率
        let elapsed = loop_start.elapsed();
        if elapsed < loop_rate {
            thread::sleep(loop_rate - elapsed);
        }
    }
}
```

### 7.2 关键要点

1. **类型转换**:
   - `piper-physics` 的 `JointState` ←→ `piper-sdk` 的 `JointArray<Rad>`
   - `piper-physics` 的 `[f64; 6]` ←→ `piper-sdk` 的 `JointArray<NewtonMeter>`

2. **状态读取**:
   - 使用 `robot.observe()` 获取 `Observer`
   - 通过 `observer.joint_positions()` 获取位置
   - 通过 `observer.joint_velocities()` 获取速度

3. **命令发送**:
   - 低级 API: `robot.command_torques(&pos, &vel, &kp, &kd, &torque)`
   - 必须提供所有 6 个关节的数据

4. **控制频率**:
   - 使用 `Instant::now()` 精确计时
   - 维持 200Hz (5ms) 控制循环

---

## 8. 总结

### 8.1 核心结论

1. **功能完整性**: ✅ MIT mode 已在所有层完全实现
   - 协议层: 100%
   - 驱动层: 100%
   - 客户端层: 100% (包括高级抽象)

2. **主要问题**: ⚠️ 用户体验问题，非功能缺失
   - 缺少文档和示例
   - API 发现困难
   - 学习曲线陡峭

3. **根本原因**: 架构设计转变
   - 从简单同步 API → 类型状态 + 异步架构
   - 牺牲易用性换取安全性和性能

### 8.2 行动建议

#### 立即实施（本周）
1. ✅ 创建 `examples/mit_mode_demo.rs`
2. ✅ 添加 API 文档注释
3. ✅ 更新 README 说明 MIT mode 使用

#### 短期实施（本月）
4. ✅ 将 `MitController` 导出到 prelude
5. ✅ 添加"MIT Mode Quick Start"文档
6. ✅ 修复 `gravity_compensation_robot.rs` 示例

#### 可选实施（下季度）
7. ⚠️ 考虑添加 `enable_mit_mode_simple()` 便捷方法
8. ⚠️ 考虑添加 `command_joint_torque()` 单关节方法（如果有用户需求）

### 8.3 预期效果

实施以上改进后：
- ✅ 用户能够轻松发现 MIT mode API
- ✅ 有完整的示例代码参考
- ✅ 理解新旧 SDK 的区别
- ✅ 能够快速实现重力补偿等功能

---

## 附录

### A. 相关文件清单

#### 协议层
- `crates/piper-protocol/src/control.rs` (lines 55-78, 1386-1725)
- `crates/piper-protocol/src/ids.rs` (CAN ID 定义)

#### 客户端层
- `crates/piper-client/src/state/machine.rs` (Type State 实现)
- `crates/piper-client/src/raw_commander.rs` (批量命令发送)
- `crates/piper-client/src/control/mit_controller.rs` (高级控制器)

#### 示例
- `crates/piper-sdk/examples/multi_threaded_demo.rs` (使用 MIT mode)
- `tmp/piper_sdk_other_rs/piper_sdk_rs/examples/mit_control.rs` (旧 SDK 参考)
- `tmp/piper_sdk_other_rs/piper_sdk_rs/examples/gravity_compensation.rs` (完整实现)

### B. 参考资料

- [Type State Pattern in Rust](https://docs.rs/typestate/latest/typestate/)
- [MIT Impedance Control](https://en.wikipedia.org/wiki/Impedance_control)
- MuJoCo Physics Engine: https://github.com/google-deepmind/mujoco

---

**报告结束**
