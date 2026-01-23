# High Level 模块重构方案

## 执行摘要

本方案旨在重构 `high_level` 模块，使其充分利用成熟的 `protocol` 和 `robot` 模块，解决当前设计中的架构问题、状态管理问题和协议使用问题。

**核心目标：**
1. 让 `high_level` 基于 `robot::Piper` 构建，而不是直接通过 `CanSender`
2. 使用 `protocol` 模块的类型安全接口，消除硬编码
3. 改进状态表示，支持逐个电机的状态管理
4. 利用 `robot` 模块的 IO 线程管理、状态同步、帧解析等功能

---

## 1. 当前架构问题诊断

### 1.1 当前架构（错误）

```
┌─────────────────┐
│  high_level     │  ← Type State 状态机
└────────┬────────┘
         │ CanSender trait (抽象)
         ↓
┌─────────────────┐
│   can module    │  ← CAN 硬件抽象
└─────────────────┘
```

**问题：**
- ❌ 完全绕过了 `robot` 模块
- ❌ 绕过了 `protocol` 模块
- ❌ 需要自己实现状态同步、帧解析等功能
- ❌ 硬编码 CAN ID 和数据格式

### 1.2 建议架构（正确）

```
┌─────────────────┐
│  high_level     │  ← Type State 状态机（高层 API）
└────────┬────────┘
         │ 使用 robot::Piper
         ↓
┌─────────────────┐
│ robot::Piper    │  ← IO 线程管理、状态同步、帧解析
└────────┬────────┘
         │ 使用 protocol 模块
         ↓
┌─────────────────┐
│  protocol       │  ← 类型安全的协议接口
└────────┬────────┘
         │ 使用 can 模块
         ↓
┌─────────────────┐
│   can module    │  ← CAN 硬件抽象
└─────────────────┘
```

**好处：**
- ✅ 利用 `robot` 模块的 IO 线程管理
- ✅ 利用 `robot` 模块的状态同步机制（ArcSwap）
- ✅ 利用 `robot` 模块的帧解析与聚合
- ✅ 利用 `protocol` 模块的类型安全接口
- ✅ 消除硬编码，提高可维护性

---

## 2. 当前实现问题详细分析

### 2.1 RawCommander 的使能命令实现（错误）

```rust
// src/high_level/client/raw_commander.rs
pub(crate) fn enable_arm(&self) -> Result<()> {
    self.state_tracker.check_valid_fast()?;

    let frame_id = 0x01;  // ❌ 错误的 CAN ID（应该是 0x471）
    let data = vec![0x01]; // ❌ 错误的数据格式

    let _guard = self.send_lock.lock();
    self.can_sender.send_frame(frame_id, &data)?;

    self.state_tracker.set_expected_controller(ArmController::Enabled);
    Ok(())
}
```

**问题：**
1. CAN ID 错误（`0x01` 应该是 `ID_MOTOR_ENABLE = 0x471`）
2. 数据格式错误（应该是 `[joint_index, enable_flag]`）
3. 只能使能全部关节，无法逐个控制

### 2.2 Protocol 模块提供的类型安全接口（未使用）

```rust
// src/protocol/control.rs
/// 电机使能/失能设置指令 (0x471)
pub struct MotorEnableCommand {
    pub joint_index: u8, // Byte 0: 1-6 代表关节驱动器序号，7 代表全部关节电机
    pub enable: bool,    // Byte 1: true = 使能 (0x02), false = 失能 (0x01)
}

impl MotorEnableCommand {
    /// 使能全部关节电机
    pub fn enable_all() -> Self {
        Self {
            joint_index: 7,
            enable: true,
        }
    }

    /// 使能单个关节
    pub fn enable(joint_index: u8) -> Self {
        Self {
            joint_index,
            enable: true,
        }
    }

    /// 转换为 CAN 帧
    pub fn to_frame(self) -> PiperFrame { ... }
}
```

### 2.3 Robot 模块提供的功能（未使用）

```rust
// src/robot/robot_impl.rs
pub struct Piper {
    /// 命令发送通道（向 IO 线程发送控制帧）
    cmd_tx: ManuallyDrop<Sender<PiperFrame>>,
    /// 实时命令插槽（双线程模式，邮箱模式，Overwrite）
    realtime_slot: Option<Arc<std::sync::Mutex<Option<PiperFrame>>>>,
    /// 可靠命令队列发送端（双线程模式，容量 10，FIFO）
    reliable_tx: Option<Sender<PiperFrame>>,
    /// 共享状态上下文
    ctx: Arc<PiperContext>,
    // ...
}

impl Piper {
    /// 获取关节动态状态（无锁，纳秒级返回）
    pub fn get_joint_dynamic(&self) -> JointDynamicState { ... }

    /// 获取关节位置状态（无锁，纳秒级返回）
    pub fn get_joint_position(&self) -> JointPositionState { ... }

    /// 获取关节驱动器低速反馈状态（无锁）
    pub fn get_joint_driver_low_speed(&self) -> JointDriverLowSpeedState { ... }

    /// 发送控制帧（非阻塞）
    pub fn send_frame(&self, frame: PiperFrame) -> Result<(), RobotError> { ... }

    /// 发送实时控制命令（邮箱模式，覆盖策略）
    pub fn send_realtime(&self, frame: PiperFrame) -> Result<(), RobotError> { ... }

    /// 发送可靠命令（FIFO 策略）
    pub fn send_reliable(&self, frame: PiperFrame) -> Result<(), RobotError> { ... }
}
```

---

## 3. 重构方案

### 3.1 架构重构

#### 3.1.1 阶段 1：使用 Robot 模块作为底层

**目标：** 让 `high_level` 基于 `robot::Piper` 构建

**当前实现：**
```rust
// src/high_level/state/machine.rs
pub struct Piper<State = Disconnected> {
    pub(crate) raw_commander: Arc<RawCommander>,  // ❌ 使用 RawCommander
    pub(crate) observer: Observer,
    _state: PhantomData<State>,
}
```

**重构后：**
```rust
// src/high_level/state/machine.rs
pub struct Piper<State = Disconnected> {
    pub(crate) robot: Arc<robot::Piper>,  // ✅ 使用 robot::Piper
    pub(crate) observer: Observer,
    _state: PhantomData<State>,
}
```

#### 3.1.2 阶段 2：使用 Protocol 模块的类型安全接口

**目标：** 消除硬编码的 CAN ID 和数据格式

**当前实现：**
```rust
// src/high_level/client/raw_commander.rs
pub(crate) fn enable_arm(&self) -> Result<()> {
    self.state_tracker.check_valid_fast()?;

    let frame_id = 0x01;  // ❌ 硬编码
    let data = vec![0x01]; // ❌ 硬编码

    let _guard = self.send_lock.lock();
    self.can_sender.send_frame(frame_id, &data)?;

    self.state_tracker.set_expected_controller(ArmController::Enabled);
    Ok(())
}
```

**重构后：**
```rust
// src/high_level/client/raw_commander.rs
use crate::protocol::control::MotorEnableCommand;

pub(crate) fn enable_arm(&self) -> Result<()> {
    self.state_tracker.check_valid_fast()?;

    // ✅ 使用类型安全的协议接口
    let cmd = MotorEnableCommand::enable_all();
    let frame = cmd.to_frame();

    let _guard = self.send_lock.lock();
    self.robot.send_reliable(frame)?;  // ✅ 通过 robot 模块发送

    self.state_tracker.set_expected_controller(ArmController::Enabled);
    Ok(())
}

// ✅ 支持逐个使能
pub(crate) fn enable_joint(&self, joint_index: u8) -> Result<()> {
    self.state_tracker.check_valid_fast()?;

    let cmd = MotorEnableCommand::enable(joint_index);
    let frame = cmd.to_frame();

    let _guard = self.send_lock.lock();
    self.robot.send_reliable(frame)?;

    // ✅ 更新对应关节的期望状态
    self.state_tracker.set_joint_enabled(joint_index, true);
    Ok(())
}
```

### 3.2 状态管理改进

#### 3.2.1 当前状态表示（无法表示部分使能）

```rust
// src/high_level/client/state_tracker.rs
pub enum ArmController {
    Enabled,    // ❌ 只能表示"全部使能"
    Standby,    // ❌ 只能表示"全部失能"
    Error,
    Disconnected,
}

// src/high_level/client/observer.rs
pub struct RobotState {
    pub arm_enabled: bool,  // ❌ 单个布尔值，无法表示部分使能
    // ...
}
```

#### 3.2.2 改进方案：使用位掩码

**StateTracker 改进：**
```rust
// src/high_level/client/state_tracker.rs
/// 机械臂控制器状态（改进版）
pub struct ArmController {
    /// 每个关节的使能状态（位掩码，Bit 0-5 对应 J1-J6）
    enabled_mask: u8,
    /// 整体状态（用于快速检查）
    overall_state: OverallState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverallState {
    /// 全部失能
    AllDisabled,
    /// 部分使能
    PartiallyEnabled,
    /// 全部使能
    AllEnabled,
    /// 错误
    Error,
    /// 断开连接
    Disconnected,
}

impl ArmController {
    /// 检查指定关节是否使能
    pub fn is_joint_enabled(&self, joint_index: usize) -> bool {
        if joint_index >= 6 {
            return false;
        }
        (self.enabled_mask >> joint_index) & 1 == 1
    }

    /// 检查是否全部使能
    pub fn is_all_enabled(&self) -> bool {
        self.enabled_mask == 0b111111
    }

    /// 检查是否全部失能
    pub fn is_all_disabled(&self) -> bool {
        self.enabled_mask == 0
    }

    /// 检查是否部分使能
    pub fn is_partially_enabled(&self) -> bool {
        self.enabled_mask != 0 && self.enabled_mask != 0b111111
    }
}
```

**Observer 改进：**
```rust
// src/high_level/client/observer.rs
pub struct RobotState {
    // ...
    /// 每个关节的使能状态（位掩码，Bit 0-5 对应 J1-J6）
    pub joint_enabled_mask: u8,
    /// 整体使能状态（用于向后兼容）
    pub arm_enabled: bool,  // = (joint_enabled_mask == 0b111111)
    // ...
}

impl RobotState {
    /// 检查指定关节是否使能
    pub fn is_joint_enabled(&self, joint_index: usize) -> bool {
        if joint_index >= 6 {
            return false;
        }
        (self.joint_enabled_mask >> joint_index) & 1 == 1
    }

    /// 检查是否全部使能
    pub fn is_all_enabled(&self) -> bool {
        self.joint_enabled_mask == 0b111111
    }

    /// 检查是否部分使能
    pub fn is_partially_enabled(&self) -> bool {
        self.joint_enabled_mask != 0 && self.joint_enabled_mask != 0b111111
    }
}
```

### 3.3 Observer 状态同步改进

#### 3.3.1 当前实现（手动更新状态）

```rust
// src/high_level/client/observer.rs
impl Observer {
    /// 更新机械臂使能状态（仅内部可见）
    #[doc(hidden)]
    pub fn update_arm_enabled(&self, enabled: bool) {
        let mut state = self.state.write();
        state.arm_enabled = enabled;
        state.last_update = Instant::now();
    }
}
```

**问题：** 需要手动更新状态，容易与实际硬件状态不一致

#### 3.3.2 改进方案：利用 Robot 模块的状态同步

```rust
// src/high_level/client/observer.rs
impl Observer {
    /// 从 robot 模块同步使能状态（内部使用）
    #[doc(hidden)]
    pub fn sync_enable_state_from_robot(&self, robot: &robot::Piper) {
        let driver_state = robot.get_joint_driver_low_speed();
        let enabled_mask = driver_state.driver_enabled_mask;

        let mut state = self.state.write();
        state.joint_enabled_mask = enabled_mask;
        state.arm_enabled = enabled_mask == 0b111111;
        state.last_update = Instant::now();
    }
}
```

---

## 4. 具体重构步骤

### 步骤 1：修改 RawCommander，使用 robot::Piper

**文件：** `src/high_level/client/raw_commander.rs`

**修改前：**
```rust
pub(crate) struct RawCommander {
    /// 状态跟踪器
    state_tracker: Arc<StateTracker>,
    /// CAN 发送接口
    can_sender: Arc<dyn CanSender>,
    /// 发送锁（保证帧序）
    send_lock: Mutex<()>,
}
```

**修改后：**
```rust
pub(crate) struct RawCommander {
    /// 状态跟踪器
    state_tracker: Arc<StateTracker>,
    /// Robot 实例（使用 robot 模块）
    robot: Arc<robot::Piper>,
    /// 发送锁（保证帧序）
    send_lock: Mutex<()>,
}
```

**修改方法：**
```rust
impl RawCommander {
    pub(crate) fn new(
        state_tracker: Arc<StateTracker>,
        robot: Arc<robot::Piper>,
    ) -> Self {
        RawCommander {
            state_tracker,
            robot,
            send_lock: Mutex::new(()),
        }
    }

    /// 使能机械臂（重构后）
    pub(crate) fn enable_arm(&self) -> Result<()> {
        self.state_tracker.check_valid_fast()?;

        // ✅ 使用 protocol 模块的类型安全接口
        use crate::protocol::control::MotorEnableCommand;
        let cmd = MotorEnableCommand::enable_all();
        let frame = cmd.to_frame();

        let _guard = self.send_lock.lock();
        self.robot.send_reliable(frame)?;

        self.state_tracker.set_expected_controller(ArmController::Enabled);
        Ok(())
    }

    /// 使能单个关节（新增）
    pub(crate) fn enable_joint(&self, joint_index: u8) -> Result<()> {
        self.state_tracker.check_valid_fast()?;

        use crate::protocol::control::MotorEnableCommand;
        let cmd = MotorEnableCommand::enable(joint_index);
        let frame = cmd.to_frame();

        let _guard = self.send_lock.lock();
        self.robot.send_reliable(frame)?;

        // 更新对应关节的期望状态
        self.state_tracker.set_joint_enabled(joint_index, true);
        Ok(())
    }

    /// 失能机械臂（重构后）
    pub(crate) fn disable_arm(&self) -> Result<()> {
        // 失能不检查状态（安全操作）
        use crate::protocol::control::MotorEnableCommand;
        let cmd = MotorEnableCommand::disable_all();
        let frame = cmd.to_frame();

        let _guard = self.send_lock.lock();
        self.robot.send_reliable(frame)?;

        self.state_tracker.set_expected_controller(ArmController::Standby);
        Ok(())
    }

    /// 设置控制模式（重构后）
    pub(crate) fn set_control_mode(&self, mode: ControlMode) -> Result<()> {
        self.state_tracker.check_valid_fast()?;

        use crate::protocol::control::ControlModeCommand;
        use crate::protocol::control::ControlModeCommand as ProtocolControlMode;

        let protocol_mode = match mode {
            ControlMode::MitMode => ProtocolControlMode::CanControl,
            ControlMode::PositionMode => ProtocolControlMode::CanControl,
            ControlMode::Unknown => return Err(RobotError::ConfigError("Invalid control mode".to_string())),
        };

        let cmd = ControlModeCommand::mode_switch(protocol_mode);
        let frame = cmd.to_frame();

        let _guard = self.send_lock.lock();
        self.robot.send_reliable(frame)?;

        self.state_tracker.set_expected_mode(mode);
        Ok(())
    }

    /// 发送 MIT 模式指令（重构后）
    pub(crate) fn send_mit_command(
        &self,
        joint: Joint,
        position: Rad,
        velocity: f64,
        kp: f64,
        kd: f64,
        torque: NewtonMeter,
    ) -> Result<()> {
        self.state_tracker.check_valid_fast()?;

        use crate::protocol::control::MitControlCommand;

        let joint_index = joint.index() as u8;
        let pos_ref = position.0 as f32;
        let vel_ref = velocity as f32;
        let kp_f32 = kp as f32;
        let kd_f32 = kd as f32;
        let t_ref = torque.0 as f32;
        let crc = 0x00; // TODO: 计算 CRC

        let cmd = MitControlCommand::new(joint_index, pos_ref, vel_ref, kp_f32, kd_f32, t_ref, crc);
        let frame = cmd.to_frame();

        let _guard = self.send_lock.lock();
        self.robot.send_realtime(frame)?;

        Ok(())
    }

    /// 发送位置模式指令（重构后）
    pub(crate) fn send_position_command(
        &self,
        joint: Joint,
        position: Rad,
        velocity: f64,
    ) -> Result<()> {
        self.state_tracker.check_valid_fast()?;

        use crate::protocol::control::{JointControl12, JointControl34, JointControl56};

        let pos_deg = (position.0 * 180.0 / std::f64::consts::PI) as f64;

        let frame = match joint {
            Joint::J1 => {
                let cmd = JointControl12::new(pos_deg, 0.0);
                cmd.to_frame()
            },
            Joint::J2 => {
                let cmd = JointControl12::new(0.0, pos_deg);
                cmd.to_frame()
            },
            Joint::J3 => {
                let cmd = JointControl34::new(pos_deg, 0.0);
                cmd.to_frame()
            },
            Joint::J4 => {
                let cmd = JointControl34::new(0.0, pos_deg);
                cmd.to_frame()
            },
            Joint::J5 => {
                let cmd = JointControl56::new(pos_deg, 0.0);
                cmd.to_frame()
            },
            Joint::J6 => {
                let cmd = JointControl56::new(0.0, pos_deg);
                cmd.to_frame()
            },
        };

        let _guard = self.send_lock.lock();
        self.robot.send_reliable(frame)?;

        Ok(())
    }

    /// 控制夹爪（重构后）
    pub(crate) fn send_gripper_command(&self, position: f64, effort: f64) -> Result<()> {
        self.state_tracker.check_valid_fast()?;

        use crate::protocol::control::GripperControlCommand;

        let position_mm = position * 1000.0;
        let torque_nm = effort;
        let enable = true;

        let cmd = GripperControlCommand::new(position_mm, torque_nm, enable);
        let frame = cmd.to_frame();

        let _guard = self.send_lock.lock();
        self.robot.send_reliable(frame)?;

        Ok(())
    }
}
```

### 步骤 2：修改 StateTracker，支持位掩码

**文件：** `src/high_level/client/state_tracker.rs`

**修改前：**
```rust
pub enum ArmController {
    Enabled,
    Standby,
    Error,
    Disconnected,
}
```

**修改后：**
```rust
/// 机械臂控制器状态（改进版）
pub struct ArmController {
    /// 每个关节的使能状态（位掩码，Bit 0-5 对应 J1-J6）
    enabled_mask: u8,
    /// 整体状态（用于快速检查）
    overall_state: OverallState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverallState {
    /// 全部失能
    AllDisabled,
    /// 部分使能
    PartiallyEnabled,
    /// 全部使能
    AllEnabled,
    /// 错误
    Error,
    /// 断开连接
    Disconnected,
}

impl ArmController {
    /// 创建新的控制器状态
    pub fn new() -> Self {
        Self {
            enabled_mask: 0,
            overall_state: OverallState::AllDisabled,
        }
    }

    /// 检查指定关节是否使能
    pub fn is_joint_enabled(&self, joint_index: usize) -> bool {
        if joint_index >= 6 {
            return false;
        }
        (self.enabled_mask >> joint_index) & 1 == 1
    }

    /// 检查是否全部使能
    pub fn is_all_enabled(&self) -> bool {
        self.enabled_mask == 0b111111
    }

    /// 检查是否全部失能
    pub fn is_all_disabled(&self) -> bool {
        self.enabled_mask == 0
    }

    /// 检查是否部分使能
    pub fn is_partially_enabled(&self) -> bool {
        self.enabled_mask != 0 && self.enabled_mask != 0b111111
    }

    /// 设置关节使能状态
    pub fn set_joint_enabled(&mut self, joint_index: usize, enabled: bool) {
        if joint_index >= 6 {
            return;
        }
        if enabled {
            self.enabled_mask |= 1 << joint_index;
        } else {
            self.enabled_mask &= !(1 << joint_index);
        }
        self.update_overall_state();
    }

    /// 设置全部使能
    pub fn set_all_enabled(&mut self) {
        self.enabled_mask = 0b111111;
        self.overall_state = OverallState::AllEnabled;
    }

    /// 设置全部失能
    pub fn set_all_disabled(&mut self) {
        self.enabled_mask = 0;
        self.overall_state = OverallState::AllDisabled;
    }

    /// 获取整体状态
    pub fn overall_state(&self) -> OverallState {
        self.overall_state
    }

    /// 更新整体状态
    fn update_overall_state(&mut self) {
        self.overall_state = match self.enabled_mask {
            0 => OverallState::AllDisabled,
            0b111111 => OverallState::AllEnabled,
            _ => OverallState::PartiallyEnabled,
        };
    }
}

impl Default for ArmController {
    fn default() -> Self {
        Self::new()
    }
}

// 为了向后兼容，保留旧的枚举（标记为 deprecated）
#[deprecated(note = "Use ArmController struct with bit mask instead")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArmControllerState {
    Enabled,
    Standby,
    Error,
    Disconnected,
}

// 在 StateTracker 中添加位掩码支持
impl StateTracker {
    // ... 现有方法 ...

    /// 设置指定关节的使能状态
    pub fn set_joint_enabled(&self, joint_index: usize, enabled: bool) {
        let mut details = self.details.write();
        details.expected_controller.set_joint_enabled(joint_index, enabled);
        details.last_update = Instant::now();
    }

    /// 获取指定关节的期望使能状态
    pub fn is_joint_expected_enabled(&self, joint_index: usize) -> bool {
        self.details.read().expected_controller.is_joint_enabled(joint_index)
    }
}
```

### 步骤 3：修改 Observer，同步 robot 状态

**文件：** `src/high_level/client/observer.rs`

**修改前：**
```rust
pub struct RobotState {
    pub joint_positions: JointArray<Rad>,
    pub joint_velocities: JointArray<f64>,
    pub joint_torques: JointArray<NewtonMeter>,
    pub gripper_state: GripperState,
    pub arm_enabled: bool,  // ❌ 单个布尔值
    pub last_update: Instant,
}
```

**修改后：**
```rust
pub struct RobotState {
    pub joint_positions: JointArray<Rad>,
    pub joint_velocities: JointArray<f64>,
    pub joint_torques: JointArray<NewtonMeter>,
    pub gripper_state: GripperState,
    /// 每个关节的使能状态（位掩码，Bit 0-5 对应 J1-J6）
    pub joint_enabled_mask: u8,
    /// 整体使能状态（用于向后兼容）
    pub arm_enabled: bool,  // = (joint_enabled_mask == 0b111111)
    pub last_update: Instant,
}

impl Default for RobotState {
    fn default() -> Self {
        RobotState {
            joint_positions: JointArray::splat(Rad(0.0)),
            joint_velocities: JointArray::splat(0.0),
            joint_torques: JointArray::splat(NewtonMeter(0.0)),
            gripper_state: GripperState::default(),
            joint_enabled_mask: 0,
            arm_enabled: false,
            last_update: Instant::now(),
        }
    }
}

impl Observer {
    /// 从 robot 模块同步状态（新增）
    #[doc(hidden)]
    pub fn sync_from_robot(&self, robot: &robot::Piper) {
        // 同步关节位置
        let joint_pos = robot.get_joint_position();
        let positions = JointArray::new(joint_pos.joint_pos.map(|rad| Rad(rad)));

        // 同步关节动态（速度和力矩）
        let joint_dyn = robot.get_joint_dynamic();
        let velocities = JointArray::new(joint_dyn.joint_vel);
        let torques = JointArray::new(joint_dyn.get_all_torques().map(|torque| NewtonMeter(torque)));

        // 同步使能状态
        let driver_state = robot.get_joint_driver_low_speed();
        let enabled_mask = driver_state.driver_enabled_mask;

        // 同步夹爪状态
        let gripper = robot.get_gripper();

        let mut state = self.state.write();
        state.joint_positions = positions;
        state.joint_velocities = velocities;
        state.joint_torques = torques;
        state.joint_enabled_mask = enabled_mask;
        state.arm_enabled = enabled_mask == 0b111111;
        state.gripper_state.position = gripper.travel / 100.0; // 归一化到 0.0-1.0
        state.gripper_state.effort = gripper.torque / 10.0; // 归一化到 0.0-1.0
        state.gripper_state.enabled = gripper.is_enabled();
        state.last_update = Instant::now();
    }

    /// 检查指定关节是否使能（新增）
    pub fn is_joint_enabled(&self, joint_index: usize) -> bool {
        if joint_index >= 6 {
            return false;
        }
        (self.state.read().joint_enabled_mask >> joint_index) & 1 == 1
    }

    /// 检查是否全部使能（新增）
    pub fn is_all_enabled(&self) -> bool {
        self.state.read().joint_enabled_mask == 0b111111
    }

    /// 检查是否部分使能（新增）
    pub fn is_partially_enabled(&self) -> bool {
        let mask = self.state.read().joint_enabled_mask;
        mask != 0 && mask != 0b111111
    }
}
```

### 步骤 4：修改 Type State Machine，使用 robot::Piper

**文件：** `src/high_level/state/machine.rs`

**修改前：**
```rust
pub struct Piper<State = Disconnected> {
    pub(crate) raw_commander: Arc<RawCommander>,  // ❌
    pub(crate) observer: Observer,
    _state: PhantomData<State>,
}
```

**修改后：**
```rust
pub struct Piper<State = Disconnected> {
    pub(crate) robot: Arc<robot::Piper>,  // ✅
    pub(crate) observer: Observer,
    _state: PhantomData<State>,
}

impl Piper<Disconnected> {
    /// 连接到机械臂（重构后）
    pub fn connect<C>(can_adapter: C, config: ConnectionConfig) -> Result<Piper<Standby>>
    where
        C: robot::SplittableAdapter + Send + 'static,
        C::RxAdapter: Send + 'static,
        C::TxAdapter: Send + 'static,
    {
        // 使用 robot 模块创建双线程模式的 Piper
        let robot = Arc::new(robot::Piper::new_dual_thread(can_adapter, None)?);

        // 等待接收到第一个有效反馈
        robot.wait_for_feedback(config.timeout)?;

        // 创建 Observer 并同步初始状态
        let observer = Observer::new(Arc::new(RwLock::new(RobotState::default())));
        observer.sync_from_robot(&robot);

        Ok(Piper {
            robot,
            observer,
            _state: PhantomData,
        })
    }
}

impl Piper<Standby> {
    /// 使能 MIT 模式（重构后）
    pub fn enable_mit_mode(
        self,
        config: MitModeConfig,
    ) -> Result<Piper<Active<MitMode>>> {
        // 1. 使用 protocol 模块的类型安全接口使能
        use crate::protocol::control::MotorEnableCommand;
        let cmd = MotorEnableCommand::enable_all();
        let frame = cmd.to_frame();

        self.robot.send_reliable(frame)?;

        // 2. 等待使能完成
        self.wait_for_enabled(config.timeout)?;

        // 3. 设置 MIT 模式
        use crate::protocol::control::{ControlModeCommand, ControlModeCommand as ProtocolControlMode};
        let cmd = ControlModeCommand::mode_switch(ProtocolControlMode::CanControl);
        let frame = cmd.to_frame();

        self.robot.send_reliable(frame)?;

        // 4. 类型转换
        let new_piper = Piper {
            robot: self.robot.clone(),
            observer: self.observer.clone(),
            _state: PhantomData,
        };

        std::mem::forget(self);
        Ok(new_piper)
    }

    /// 等待机械臂使能完成（重构后）
    fn wait_for_enabled(&self, timeout: Duration) -> Result<()> {
        let start = Instant::now();
        let poll_interval = Duration::from_millis(10);

        loop {
            if start.elapsed() > timeout {
                return Err(RobotError::Timeout {
                    timeout_ms: timeout.as_millis() as u64,
                });
            }

            // ✅ 从 robot 模块读取实际使能状态
            let driver_state = self.robot.get_joint_driver_low_speed();
            let enabled_mask = driver_state.driver_enabled_mask;

            if enabled_mask == 0b111111 {
                return Ok(());
            }

            std::thread::sleep(poll_interval);
        }
    }
}

impl Piper<Active<MitMode>> {
    /// 发送 MIT 模式力矩指令（重构后）
    pub fn command_torques(
        &self,
        joint: Joint,
        position: Rad,
        velocity: f64,
        kp: f64,
        kd: f64,
        torque: NewtonMeter,
    ) -> Result<()> {
        use crate::protocol::control::MitControlCommand;

        let joint_index = joint.index() as u8;
        let pos_ref = position.0 as f32;
        let vel_ref = velocity as f32;
        let kp_f32 = kp as f32;
        let kd_f32 = kd as f32;
        let t_ref = torque.0 as f32;
        let crc = 0x00;

        let cmd = MitControlCommand::new(joint_index, pos_ref, vel_ref, kp_f32, kd_f32, t_ref, crc);
        let frame = cmd.to_frame();

        // ✅ 使用 robot 模块的实时命令插槽
        self.robot.send_realtime(frame)?;

        Ok(())
    }

    /// 同步状态（新增）
    pub fn sync_state(&self) {
        self.observer.sync_from_robot(&self.robot);
    }
}
```

### 步骤 5：添加状态监控线程

**文件：** `src/high_level/client/state_monitor.rs`

```rust
//! 状态监控线程
//!
//! 后台线程定期从 robot 模块同步状态到 Observer。

use std::sync::Arc;
use std::thread;
use std::time::Duration;

use crate::robot::Piper as RobotPiper;
use crate::high_level::client::observer::Observer;

/// 状态监控配置
#[derive(Debug, Clone)]
pub struct StateMonitorConfig {
    /// 同步间隔（默认 10ms = 100Hz）
    pub sync_interval: Duration,
}

impl Default for StateMonitorConfig {
    fn default() -> Self {
        Self {
            sync_interval: Duration::from_millis(10),
        }
    }
}

/// 状态监控线程
pub struct StateMonitor {
    /// 运行标志
    is_running: Arc<std::sync::atomic::AtomicBool>,
    /// 线程句柄
    thread_handle: Option<thread::JoinHandle<()>>,
}

impl StateMonitor {
    /// 创建状态监控线程
    pub fn new(
        robot: Arc<RobotPiper>,
        observer: Observer,
        config: StateMonitorConfig,
    ) -> Self {
        let is_running = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let is_running_clone = is_running.clone();

        let thread_handle = thread::spawn(move || {
            while is_running_clone.load(std::sync::atomic::Ordering::Relaxed) {
                observer.sync_from_robot(&robot);
                thread::sleep(config.sync_interval);
            }
        });

        Self {
            is_running,
            thread_handle: Some(thread_handle),
        }
    }

    /// 停止监控线程
    pub fn stop(&mut self) {
        self.is_running.store(false, std::sync::atomic::Ordering::Relaxed);
        if let Some(handle) = self.thread_handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for StateMonitor {
    fn drop(&mut self) {
        self.stop();
    }
}
```

---

## 5. API 改进示例

### 5.1 当前 API（错误）

```rust
// 当前实现
let robot = Piper::connect(can_adapter, config)?;
let robot = robot.enable_mit_mode(MitModeConfig::default())?;

robot.command_torques(Joint::J1, Rad(1.0), 0.5, 10.0, 2.0, NewtonMeter(5.0))?;

// ❌ 无法逐个使能关节
// ❌ 无法查询部分使能状态
// ❌ 硬编码的 CAN ID
```

### 5.2 改进后 API

```rust
// 改进后
let robot = Piper::connect(can_adapter, config)?;

// ✅ 支持逐个使能关节
let robot = robot.enable_joints(&[Joint::J1, Joint::J2])?;
let robot = robot.enable_all()?; // 使能全部

// ✅ 检查部分使能状态
let observer = robot.observer();
if observer.is_joint_enabled(Joint::J1) {
    println!("J1 已使能");
}
if observer.is_partially_enabled() {
    println!("部分关节已使能");
}

// ✅ 使能 MIT 模式
let robot = robot.enable_mit_mode(MitModeConfig::default())?;

robot.command_torques(Joint::J1, Rad(1.0), 0.5, 10.0, 2.0, NewtonMeter(5.0))?;
```

---

## 6. 重构优先级

### 阶段 1：核心重构（高优先级）

1. ✅ 修改 `RawCommander` 使用 `robot::Piper` 而不是 `CanSender`
2. ✅ 修改 `RawCommander` 使用 `protocol` 模块的类型安全接口
3. ✅ 修改 `Type State Machine` 使用 `robot::Piper`

**预计工作量：** 2-3 天

### 阶段 2：状态管理改进（中优先级）

1. ✅ 修改 `StateTracker` 使用位掩码支持逐个电机状态
2. ✅ 修改 `Observer` 同步 `robot` 模块的状态
3. ✅ 添加状态监控线程

**预计工作量：** 2-3 天

### 阶段 3：API 改进（低优先级）

1. ✅ 添加逐个关节控制的 API
2. ✅ 添加状态查询 API
3. ✅ 向后兼容性处理

**预计工作量：** 1-2 天

---

## 7. 测试策略

### 7.1 单元测试

```rust
// 测试 protocol 模块的类型安全接口
#[cfg(test)]
mod protocol_tests {
    use crate::protocol::control::MotorEnableCommand;

    #[test]
    fn test_motor_enable_command() {
        let cmd = MotorEnableCommand::enable_all();
        let frame = cmd.to_frame();

        assert_eq!(frame.id, 0x471); // ✅ 正确的 CAN ID
        assert_eq!(frame.data[0], 7); // 全部关节
        assert_eq!(frame.data[1], 0x02); // 使能
    }
}

// 测试状态管理
#[cfg(test)]
mod state_tests {
    use crate::high_level::client::state_tracker::ArmController;

    #[test]
    fn test_joint_enable_mask() {
        let mut controller = ArmController::new();

        controller.set_joint_enabled(0, true); // J1
        controller.set_joint_enabled(2, true); // J3

        assert!(controller.is_joint_enabled(0));
        assert!(!controller.is_joint_enabled(1));
        assert!(controller.is_joint_enabled(2));
        assert!(!controller.is_joint_enabled(3));
        assert!(!controller.is_joint_enabled(4));
        assert!(!controller.is_joint_enabled(5));

        assert!(controller.is_partially_enabled());
    }
}
```

### 7.2 集成测试

```rust
// 测试 high_level 与 robot、protocol 模块的集成
#[cfg(test)]
mod integration_tests {
    use super::*;

    #[test]
    fn test_high_level_with_robot_and_protocol() {
        // 创建 CAN 适配器
        let can_adapter = /* ... */;

        // 使用 robot 模块创建 Piper
        let robot = robot::Piper::new_dual_thread(can_adapter, None).unwrap();

        // 创建 high_level 的 RawCommander
        let raw_commander = RawCommander::new(
            Arc::new(StateTracker::new()),
            Arc::new(robot),
        );

        // 测试使能命令（使用 protocol 模块的类型安全接口）
        assert!(raw_commander.enable_arm().is_ok());

        // 等待使能完成
        std::thread::sleep(Duration::from_millis(100));

        // 验证状态已更新
        let driver_state = raw_commander.robot.get_joint_driver_low_speed();
        assert_eq!(driver_state.driver_enabled_mask, 0b111111);
    }
}
```

---

## 8. 向后兼容性

### 8.1 保留旧的 API（标记为 deprecated）

```rust
#[deprecated(note = "Use enable_joint() instead")]
pub fn enable_arm(&self) -> Result<()> {
    self.enable_all()
}

#[deprecated(note = "Use ArmController struct with bit mask instead")]
pub enum ArmControllerState {
    Enabled,
    Standby,
    Error,
    Disconnected,
}
```

### 8.2 提供迁移指南

```rust
/// 旧 API 迁移到新 API
///
/// 旧 API:
/// ```rust,ignore
/// robot.enable_arm()?;
/// ```
///
/// 新 API:
/// ```rust,ignore
/// robot.enable_all()?;
/// // 或者
/// robot.enable_joints(&[Joint::J1, Joint::J2])?;
/// ```
```

---

## 9. 文档更新

### 9.1 更新架构图

```
┌─────────────────┐
│  high_level     │  ← Type State 状态机（高层 API）
└────────┬────────┘
         │ 使用 robot::Piper
         ↓
┌─────────────────┐
│ robot::Piper    │  ← IO 线程管理、状态同步、帧解析
└────────┬────────┘
         │ 使用 protocol 模块
         ↓
┌─────────────────┐
│  protocol       │  ← 类型安全的协议接口
└────────┬────────┘
         │ 使用 can 模块
         ↓
┌─────────────────┐
│   can module    │  ← CAN 硬件抽象
└─────────────────┘
```

### 9.2 更新 API 文档

```rust
/// 使能机械臂（重构后）
///
/// 使用 protocol 模块的类型安全接口，发送电机使能命令。
///
/// # 示例
///
/// ```rust,ignore
/// let robot = robot.enable_arm()?;
/// ```
///
/// # 注意
///
/// 此方法会使能全部 6 个关节。如需逐个使能，使用 `enable_joint()`。
pub fn enable_arm(&self) -> Result<Piper<Active<MitMode>>> { ... }
```

---

## 10. 总结

### 10.1 重构收益

1. ✅ **消除硬编码**：使用 protocol 模块的类型安全接口
2. ✅ **利用成熟功能**：使用 robot 模块的 IO 线程管理、状态同步、帧解析
3. ✅ **改进状态管理**：支持逐个电机的状态表示
4. ✅ **提高可维护性**：协议变更时自动更新
5. ✅ **增强类型安全**：编译期检查协议参数

### 10.2 预计工作量

- 阶段 1：2-3 天
- 阶段 2：2-3 天
- 阶段 3：1-2 天
- 测试和文档：1-2 天

**总计：** 6-10 天

### 10.3 风险评估

- **低风险**：阶段 1 和阶段 2 的重构主要是替换底层实现，不影响高层 API
- **中风险**：阶段 3 的 API 改进可能需要用户迁移代码
- **缓解措施**：提供向后兼容的 deprecated API 和详细的迁移指南

---

**文档版本：** v1.0
**创建时间：** 2025-01-23
**最后更新：** 2025-01-23

