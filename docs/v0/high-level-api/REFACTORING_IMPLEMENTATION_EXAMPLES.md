# 重构实现示例

本文档提供具体的代码实现示例，展示如何将 `high_level` 模块重构为使用 `robot` 和 `protocol` 模块。

---

## 1. RawCommander 重构

### 1.1 结构体定义

**修改前：**
```rust
// src/high_level/client/raw_commander.rs
pub(crate) struct RawCommander {
    /// 状态跟踪器
    state_tracker: Arc<StateTracker>,
    /// CAN 发送接口
    can_sender: Arc<dyn CanSender>,
    /// 发送锁（保证帧序）
    send_lock: Mutex<()>,
}

impl RawCommander {
    pub(crate) fn new(
        state_tracker: Arc<StateTracker>,
        can_sender: Arc<dyn CanSender>,
    ) -> Self {
        RawCommander {
            state_tracker,
            can_sender,
            send_lock: Mutex::new(()),
        }
    }
}
```

**修改后：**
```rust
// src/high_level/client/raw_commander.rs
use crate::robot::Piper as RobotPiper;

pub(crate) struct RawCommander {
    /// 状态跟踪器
    state_tracker: Arc<StateTracker>,
    /// Robot 实例（使用 robot 模块）
    robot: Arc<RobotPiper>,
    /// 发送锁（保证帧序）
    send_lock: Mutex<()>,
}

impl RawCommander {
    pub(crate) fn new(
        state_tracker: Arc<StateTracker>,
        robot: Arc<RobotPiper>,
    ) -> Self {
        RawCommander {
            state_tracker,
            robot,
            send_lock: Mutex::new(()),
        }
    }
}
```

### 1.2 使能命令重构

**修改前（错误）：**
```rust
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

**修改后（正确）：**
```rust
use crate::protocol::control::MotorEnableCommand;
use crate::protocol::control::ArmController as ProtocolArmController;

pub(crate) fn enable_arm(&self) -> Result<()> {
    self.state_tracker.check_valid_fast()?;

    // ✅ 使用类型安全的协议接口
    let cmd = MotorEnableCommand::enable_all();
    let frame = cmd.to_frame();

    let _guard = self.send_lock.lock();
    self.robot.send_reliable(frame)?;

    self.state_tracker.set_expected_controller(ArmController::Enabled);
    Ok(())
}

/// 使能单个关节（新增方法）
pub(crate) fn enable_joint(&self, joint_index: u8) -> Result<()> {
    self.state_tracker.check_valid_fast()?;

    // ✅ 使用类型安全的协议接口
    let cmd = MotorEnableCommand::enable(joint_index);
    let frame = cmd.to_frame();

    let _guard = self.send_lock.lock();
    self.robot.send_reliable(frame)?;

    // ✅ 更新对应关节的期望状态
    self.state_tracker.set_joint_enabled(joint_index as usize, true);
    Ok(())
}

/// 使能多个关节（新增方法）
pub(crate) fn enable_joints(&self, joint_indices: &[u8]) -> Result<()> {
    self.state_tracker.check_valid_fast()?;

    let _guard = self.send_lock.lock();

    for &joint_index in joint_indices {
        let cmd = MotorEnableCommand::enable(joint_index);
        let frame = cmd.to_frame();
        self.robot.send_reliable(frame)?;

        self.state_tracker.set_joint_enabled(joint_index as usize, true);
    }

    Ok(())
}

/// 失能机械臂（重构后）
pub(crate) fn disable_arm(&self) -> Result<()> {
    // 失能不检查状态（安全操作）
    let cmd = MotorEnableCommand::disable_all();
    let frame = cmd.to_frame();

    let _guard = self.send_lock.lock();
    self.robot.send_reliable(frame)?;

    self.state_tracker.set_expected_controller(ArmController::Standby);
    Ok(())
}

/// 失能单个关节（新增方法）
pub(crate) fn disable_joint(&self, joint_index: u8) -> Result<()> {
    let cmd = MotorEnableCommand::disable(joint_index);
    let frame = cmd.to_frame();

    let _guard = self.send_lock.lock();
    self.robot.send_reliable(frame)?;

    self.state_tracker.set_joint_enabled(joint_index as usize, false);
    Ok(())
}
```

### 1.3 控制模式命令重构

**修改前（错误）：**
```rust
pub(crate) fn set_control_mode(&self, mode: ControlMode) -> Result<()> {
    self.state_tracker.check_valid_fast()?;

    let frame_id = 0x03;
    let data = match mode {
        ControlMode::PositionMode => vec![0x01],
        ControlMode::MitMode => vec![0x02],
        ControlMode::Unknown => return Err(RobotError::ConfigError("Invalid control mode".to_string())),
    };

    let _guard = self.send_lock.lock();
    self.can_sender.send_frame(frame_id, &data)?;

    self.state_tracker.set_expected_mode(mode);
    Ok(())
}
```

**修改后（正确）：**
```rust
use crate::protocol::control::ControlModeCommand;
use crate::protocol::control::ControlModeCommand as ProtocolControlMode;
use crate::protocol::control::MitMode as ProtocolMitMode;

pub(crate) fn set_control_mode(&self, mode: ControlMode) -> Result<()> {
    self.state_tracker.check_valid_fast()?;

    // ✅ 使用类型安全的协议接口
    let protocol_mode = match mode {
        ControlMode::MitMode => ProtocolControlMode::CanControl,
        ControlMode::PositionMode => ProtocolControlMode::CanControl,
        ControlMode::Unknown => {
            return Err(RobotError::ConfigError("Invalid control mode".to_string()))
        }
    };

    let cmd = ControlModeCommand::mode_switch(protocol_mode);
    let frame = cmd.to_frame();

    let _guard = self.send_lock.lock();
    self.robot.send_reliable(frame)?;

    self.state_tracker.set_expected_mode(mode);
    Ok(())
}

/// 设置 MIT 模式（新增方法，更语义化）
pub(crate) fn set_mit_mode(&self) -> Result<()> {
    self.state_tracker.check_valid_fast()?;

    let cmd = ControlModeCommand::new(
        ProtocolControlMode::CanControl,
        crate::protocol::feedback::MoveMode::MoveP,
        0, // speed_percent
        ProtocolMitMode::Mit,
        0, // trajectory_stay_time
        crate::protocol::control::InstallPosition::Invalid,
    );
    let frame = cmd.to_frame();

    let _guard = self.send_lock.lock();
    self.robot.send_reliable(frame)?;

    self.state_tracker.set_expected_mode(ControlMode::MitMode);
    Ok(())
}

/// 设置位置模式（新增方法，更语义化）
pub(crate) fn set_position_mode(&self) -> Result<()> {
    self.state_tracker.check_valid_fast()?;

    let cmd = ControlModeCommand::new(
        ProtocolControlMode::CanControl,
        crate::protocol::feedback::MoveMode::MoveP,
        0, // speed_percent
        ProtocolMitMode::PositionVelocity,
        0, // trajectory_stay_time
        crate::protocol::control::InstallPosition::Invalid,
    );
    let frame = cmd.to_frame();

    let _guard = self.send_lock.lock();
    self.robot.send_reliable(frame)?;

    self.state_tracker.set_expected_mode(ControlMode::PositionMode);
    Ok(())
}
```

### 1.4 MIT 控制命令重构

**修改前（错误）：**
```rust
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

    let frame_id = 0x100 + joint.index() as u32;
    let data = self.build_mit_frame_data(position, velocity, kp, kd, torque);

    let _guard = self.send_lock.lock();
    self.can_sender.send_frame(frame_id, &data)?;

    Ok(())
}

/// 构建 MIT 模式帧数据（私有方法）
fn build_mit_frame_data(
    &self,
    position: Rad,
    velocity: f64,
    kp: f64,
    kd: f64,
    torque: NewtonMeter,
) -> Vec<u8> {
    let mut data = Vec::with_capacity(8);

    // 位置 (2 bytes, 缩放)
    let pos_scaled = (position.0 * 1000.0) as i16;
    data.extend_from_slice(&pos_scaled.to_le_bytes());

    // 速度 (2 bytes, 缩放)
    let vel_scaled = (velocity * 100.0) as i16;
    data.extend_from_slice(&vel_scaled.to_le_bytes());

    // kp (1 byte)
    data.push((kp * 10.0) as u8);

    // kd (1 byte)
    data.push((kd * 10.0) as u8);

    // 力矩 (2 bytes, 缩放)
    let torque_scaled = (torque.0 * 100.0) as i16;
    data.extend_from_slice(&torque_scaled.to_le_bytes());

    data
}
```

**修改后（正确）：**
```rust
use crate::protocol::control::MitControlCommand;

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

    // ✅ 使用类型安全的协议接口
    let joint_index = joint.index() as u8;
    let pos_ref = position.0 as f32;
    let vel_ref = velocity as f32;
    let kp_f32 = kp as f32;
    let kd_f32 = kd as f32;
    let t_ref = torque.0 as f32;

    // TODO: 实现正确的 CRC 计算
    let crc = 0x00;

    let cmd = MitControlCommand::new(joint_index, pos_ref, vel_ref, kp_f32, kd_f32, t_ref, crc);
    let frame = cmd.to_frame();

    // ✅ 使用 robot 模块的实时命令插槽（覆盖策略）
    let _guard = self.send_lock.lock();
    self.robot.send_realtime(frame)?;

    Ok(())
}

// 删除 build_mit_frame_data 方法，不再需要
```

### 1.5 位置控制命令重构

**修改前（错误）：**
```rust
pub(crate) fn send_position_command(
    &self,
    joint: Joint,
    position: Rad,
    velocity: f64,
) -> Result<()> {
    self.state_tracker.check_valid_fast()?;

    let frame_id = 0x200 + joint.index() as u32;
    let data = self.build_position_frame_data(position, velocity);

    let _guard = self.send_lock.lock();
    self.can_sender.send_frame(frame_id, &data)?;

    Ok(())
}

/// 构建位置模式帧数据（私有方法）
fn build_position_frame_data(&self, position: Rad, velocity: f64) -> Vec<u8> {
    let mut data = Vec::with_capacity(8);

    let pos_scaled = (position.0 * 1000.0) as i32;
    data.extend_from_slice(&pos_scaled.to_le_bytes());

    let vel_scaled = (velocity * 100.0) as i16;
    data.extend_from_slice(&vel_scaled.to_le_bytes());

    data
}
```

**修改后（正确）：**
```rust
use crate::protocol::control::{JointControl12, JointControl34, JointControl56};

pub(crate) fn send_position_command(
    &self,
    joint: Joint,
    position: Rad,
    velocity: f64,
) -> Result<()> {
    self.state_tracker.check_valid_fast()?;

    // ✅ 使用类型安全的协议接口
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

// 删除 build_position_frame_data 方法，不再需要
```

### 1.6 夹爪控制命令重构

**修改前（错误）：**
```rust
pub(crate) fn send_gripper_command(&self, position: f64, effort: f64) -> Result<()> {
    self.state_tracker.check_valid_fast()?;

    let frame_id = 0x300;
    let data = self.build_gripper_frame_data(position, effort);

    let _guard = self.send_lock.lock();
    self.can_sender.send_frame(frame_id, &data)?;

    Ok(())
}

/// 构建夹爪帧数据（私有方法）
fn build_gripper_frame_data(&self, position: f64, effort: f64) -> Vec<u8> {
    let mut data = Vec::with_capacity(8);

    let pos_scaled = (position * 1000.0) as u16;
    data.extend_from_slice(&pos_scaled.to_le_bytes());

    let effort_scaled = (effort * 100.0) as u16;
    data.extend_from_slice(&effort_scaled.to_le_bytes());

    data
}
```

**修改后（正确）：**
```rust
use crate::protocol::control::GripperControlCommand;

pub(crate) fn send_gripper_command(&self, position: f64, effort: f64) -> Result<()> {
    self.state_tracker.check_valid_fast()?;

    // ✅ 使用类型安全的协议接口
    // position: 0.0-1.0 归一化值，转换为 mm
    // effort: 0.0-1.0 归一化值，转换为 N·m
    let position_mm = position * 100.0;  // 假设夹爪行程 100mm
    let torque_nm = effort * 10.0;       // 假设最大扭矩 10N·m
    let enable = true;

    let cmd = GripperControlCommand::new(position_mm, torque_nm, enable);
    let frame = cmd.to_frame();

    let _guard = self.send_lock.lock();
    self.robot.send_reliable(frame)?;

    Ok(())
}

/// 夹爪使能命令（新增方法）
pub(crate) fn enable_gripper(&self) -> Result<()> {
    self.state_tracker.check_valid_fast()?;

    let cmd = GripperControlCommand::new(0.0, 0.0, true);
    let frame = cmd.to_frame();

    let _guard = self.send_lock.lock();
    self.robot.send_reliable(frame)?;

    Ok(())
}

/// 夹爪失能命令（新增方法）
pub(crate) fn disable_gripper(&self) -> Result<()> {
    let cmd = GripperControlCommand::new(0.0, 0.0, false);
    let frame = cmd.to_frame();

    let _guard = self.send_lock.lock();
    self.robot.send_reliable(frame)?;

    Ok(())
}

/// 夹爪设置零点（新增方法）
pub(crate) fn set_gripper_zero(&self) -> Result<()> {
    let cmd = GripperControlCommand::new(0.0, 0.0, false).set_zero_point();
    let frame = cmd.to_frame();

    let _guard = self.send_lock.lock();
    self.robot.send_reliable(frame)?;

    Ok(())
}

// 删除 build_gripper_frame_data 方法，不再需要
```

---

## 2. StateTracker 重构

### 2.1 添加位掩码支持

**修改前：**
```rust
// src/high_level/client/state_tracker.rs
/// 机械臂控制器状态
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArmController {
    /// 使能
    Enabled,
    /// 待机
    Standby,
    /// 错误
    Error,
    /// 断开连接
    Disconnected,
}

impl ArmController {
    // ... 只能表示整体状态，无法表示部分使能
}
```

**修改后：**
```rust
// src/high_level/client/state_tracker.rs
/// 机械臂控制器状态（改进版）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArmController {
    /// 每个关节的使能状态（位掩码，Bit 0-5 对应 J1-J6）
    enabled_mask: u8,
    /// 整体状态（用于快速检查）
    overall_state: OverallState,
}

/// 整体状态枚举
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

    /// 获取使能掩码
    pub fn enabled_mask(&self) -> u8 {
        self.enabled_mask
    }

    /// 更新整体状态（私有方法）
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

impl From<ArmControllerState> for ArmController {
    fn from(state: ArmControllerState) -> Self {
        match state {
            ArmControllerState::Enabled => ArmController {
                enabled_mask: 0b111111,
                overall_state: OverallState::AllEnabled,
            },
            ArmControllerState::Standby => ArmController {
                enabled_mask: 0,
                overall_state: OverallState::AllDisabled,
            },
            ArmControllerState::Error => ArmController {
                enabled_mask: 0,
                overall_state: OverallState::Error,
            },
            ArmControllerState::Disconnected => ArmController {
                enabled_mask: 0,
                overall_state: OverallState::Disconnected,
            },
        }
    }
}

impl From<ArmController> for ArmControllerState {
    fn from(controller: ArmController) -> Self {
        match controller.overall_state() {
            OverallState::AllEnabled => ArmControllerState::Enabled,
            OverallState::AllDisabled => ArmControllerState::Standby,
            OverallState::Error => ArmControllerState::Error,
            OverallState::Disconnected => ArmControllerState::Disconnected,
            OverallState::PartiallyEnabled => ArmControllerState::Standby, // 部分使能映射到 Standby
        }
    }
}
```

### 2.2 添加逐个关节状态管理方法

```rust
impl StateTracker {
    // ... 现有方法 ...

    /// 设置指定关节的期望使能状态
    pub fn set_joint_enabled(&self, joint_index: usize, enabled: bool) {
        let mut details = self.details.write();
        details.expected_controller.set_joint_enabled(joint_index, enabled);
        details.last_update = Instant::now();
    }

    /// 获取指定关节的期望使能状态
    pub fn is_joint_expected_enabled(&self, joint_index: usize) -> bool {
        self.details.read().expected_controller.is_joint_enabled(joint_index)
    }

    /// 设置全部关节的期望使能状态
    pub fn set_all_joints_enabled(&self, enabled: bool) {
        let mut details = self.details.write();
        if enabled {
            details.expected_controller.set_all_enabled();
        } else {
            details.expected_controller.set_all_disabled();
        }
        details.last_update = Instant::now();
    }

    /// 获取使能掩码
    pub fn enabled_mask(&self) -> u8 {
        self.details.read().expected_controller.enabled_mask()
    }

    /// 检查是否部分使能
    pub fn is_partially_enabled(&self) -> bool {
        self.details.read().expected_controller.is_partially_enabled()
    }

    /// 从 robot 模块同步使能状态
    pub fn sync_enable_state_from_robot(&self, robot: &RobotPiper) {
        let driver_state = robot.get_joint_driver_low_speed();
        let enabled_mask = driver_state.driver_enabled_mask;

        let mut details = self.details.write();
        details.expected_controller = ArmController {
            enabled_mask,
            overall_state: if enabled_mask == 0b111111 {
                OverallState::AllEnabled
            } else if enabled_mask == 0 {
                OverallState::AllDisabled
            } else {
                OverallState::PartiallyEnabled
            },
        };
        details.last_update = Instant::now();
    }
}
```

---

## 3. Observer 重构

### 3.1 添加位掩码支持

**修改前：**
```rust
// src/high_level/client/observer.rs
pub struct RobotState {
    pub joint_positions: JointArray<Rad>,
    pub joint_velocities: JointArray<f64>,
    pub joint_torques: JointArray<NewtonMeter>,
    pub gripper_state: GripperState,
    pub arm_enabled: bool,  // ❌ 单个布尔值，无法表示部分使能
    pub last_update: Instant,
}
```

**修改后：**
```rust
// src/high_level/client/observer.rs
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
```

### 3.2 添加状态查询方法

```rust
impl Observer {
    // ... 现有方法 ...

    /// 检查指定关节是否使能
    pub fn is_joint_enabled(&self, joint_index: usize) -> bool {
        if joint_index >= 6 {
            return false;
        }
        (self.state.read().joint_enabled_mask >> joint_index) & 1 == 1
    }

    /// 检查是否全部使能
    pub fn is_all_enabled(&self) -> bool {
        self.state.read().joint_enabled_mask == 0b111111
    }

    /// 检查是否全部失能
    pub fn is_all_disabled(&self) -> bool {
        self.state.read().joint_enabled_mask == 0
    }

    /// 检查是否部分使能
    pub fn is_partially_enabled(&self) -> bool {
        let mask = self.state.read().joint_enabled_mask;
        mask != 0 && mask != 0b111111
    }

    /// 获取使能掩码
    pub fn joint_enabled_mask(&self) -> u8 {
        self.state.read().joint_enabled_mask
    }

    /// 从 robot 模块同步状态
    #[doc(hidden)]
    pub fn sync_from_robot(&self, robot: &RobotPiper) {
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
        state.gripper_state.position = (gripper.travel / 100.0).clamp(0.0, 1.0); // 归一化到 0.0-1.0
        state.gripper_state.effort = (gripper.torque / 10.0).clamp(0.0, 1.0);    // 归一化到 0.0-1.0
        state.gripper_state.enabled = gripper.is_enabled();
        state.last_update = Instant::now();
    }

    /// 批量更新使能状态（内部使用）
    #[doc(hidden)]
    pub fn update_joint_enabled_mask(&self, enabled_mask: u8) {
        let mut state = self.state.write();
        state.joint_enabled_mask = enabled_mask;
        state.arm_enabled = enabled_mask == 0b111111;
        state.last_update = Instant::now();
    }
}
```

---

## 4. Type State Machine 重构

### 4.1 结构体定义

**修改前：**
```rust
// src/high_level/state/machine.rs
pub struct Piper<State = Disconnected> {
    pub(crate) raw_commander: Arc<RawCommander>,
    pub(crate) observer: Observer,
    _state: PhantomData<State>,
}
```

**修改后：**
```rust
// src/high_level/state/machine.rs
use crate::robot::Piper as RobotPiper;

pub struct Piper<State = Disconnected> {
    pub(crate) robot: Arc<RobotPiper>,  // ✅ 使用 robot::Piper
    pub(crate) observer: Observer,
    pub(crate) state_monitor: Option<StateMonitor>,
    _state: PhantomData<State>,
}
```

### 4.2 Disconnected 状态

```rust
impl Piper<Disconnected> {
    /// 连接到机械臂
    pub fn connect<C>(can_adapter: C, config: ConnectionConfig) -> Result<Piper<Standby>>
    where
        C: robot::SplittableAdapter + Send + 'static,
        C::RxAdapter: Send + 'static,
        C::TxAdapter: Send + 'static,
    {
        // ✅ 使用 robot 模块创建双线程模式的 Piper
        let robot = Arc::new(robot::Piper::new_dual_thread(can_adapter, None)?);

        // 等待接收到第一个有效反馈
        robot.wait_for_feedback(config.timeout)?;

        // 创建 Observer 并同步初始状态
        let observer = Observer::new(Arc::new(RwLock::new(RobotState::default())));
        observer.sync_from_robot(&robot);

        // 创建状态监控线程
        let monitor_config = StateMonitorConfig {
            sync_interval: Duration::from_millis(10),
        };
        let state_monitor = Some(StateMonitor::new(
            robot.clone(),
            observer.clone(),
            monitor_config,
        ));

        Ok(Piper {
            robot,
            observer,
            state_monitor,
            _state: PhantomData,
        })
    }
}
```

### 4.3 Standby 状态

```rust
impl Piper<Standby> {
    /// 使能全部关节
    pub fn enable_all(self) -> Result<Piper<Active<MitMode>>> {
        // 使用 protocol 模块的类型安全接口
        use crate::protocol::control::MotorEnableCommand;
        let cmd = MotorEnableCommand::enable_all();
        let frame = cmd.to_frame();

        self.robot.send_reliable(frame)?;

        // 等待使能完成
        self.wait_for_all_enabled(Duration::from_secs(2))?;

        // 设置 MIT 模式
        self.set_mit_mode_internal()?;

        // 类型转换
        self.transition_to_active_mit_mode()
    }

    /// 使能指定关节
    pub fn enable_joints(self, joints: &[Joint]) -> Result<Piper<Standby>> {
        // 使用 protocol 模块的类型安全接口
        for &joint in joints {
            let cmd = MotorEnableCommand::enable(joint.index() as u8);
            let frame = cmd.to_frame();
            self.robot.send_reliable(frame)?;
        }

        // 不转换状态，仍保持 Standby（部分使能）
        Ok(self)
    }

    /// 使能单个关节
    pub fn enable_joint(self, joint: Joint) -> Result<Piper<Standby>> {
        let cmd = MotorEnableCommand::enable(joint.index() as u8);
        let frame = cmd.to_frame();
        self.robot.send_reliable(frame)?;

        Ok(self)
    }

    /// 失能全部关节
    pub fn disable_all(self) -> Result<()> {
        use crate::protocol::control::MotorEnableCommand;
        let cmd = MotorEnableCommand::disable_all();
        let frame = cmd.to_frame();
        self.robot.send_reliable(frame)?;

        Ok(())
    }

    /// 失能指定关节
    pub fn disable_joints(self, joints: &[Joint]) -> Result<()> {
        for &joint in joints {
            let cmd = MotorEnableCommand::disable(joint.index() as u8);
            let frame = cmd.to_frame();
            self.robot.send_reliable(frame)?;
        }

        Ok(())
    }

    /// 等待全部使能完成（内部方法）
    fn wait_for_all_enabled(&self, timeout: Duration) -> Result<()> {
        let start = Instant::now();
        let poll_interval = Duration::from_millis(10);

        loop {
            if start.elapsed() > timeout {
                return Err(RobotError::Timeout {
                    timeout_ms: timeout.as_millis() as u64,
                });
            }

            // 从 robot 模块读取实际使能状态
            let driver_state = self.robot.get_joint_driver_low_speed();
            let enabled_mask = driver_state.driver_enabled_mask;

            if enabled_mask == 0b111111 {
                return Ok(());
            }

            std::thread::sleep(poll_interval);
        }
    }

    /// 设置 MIT 模式（内部方法）
    fn set_mit_mode_internal(&self) -> Result<()> {
        use crate::protocol::control::ControlModeCommand;
        use crate::protocol::control::ControlModeCommand as ProtocolControlMode;
        use crate::protocol::control::MitMode as ProtocolMitMode;

        let cmd = ControlModeCommand::new(
            ProtocolControlMode::CanControl,
            crate::protocol::feedback::MoveMode::MoveP,
            0, // speed_percent
            ProtocolMitMode::Mit,
            0, // trajectory_stay_time
            crate::protocol::control::InstallPosition::Invalid,
        );
        let frame = cmd.to_frame();

        self.robot.send_reliable(frame)?;

        Ok(())
    }

    /// 转换到 Active<MitMode> 状态（内部方法）
    fn transition_to_active_mit_mode(self) -> Result<Piper<Active<MitMode>>> {
        let new_piper = Piper {
            robot: self.robot.clone(),
            observer: self.observer.clone(),
            state_monitor: self.state_monitor.clone(),
            _state: PhantomData,
        };

        std::mem::forget(self);
        Ok(new_piper)
    }

    /// 获取 Observer（只读）
    pub fn observer(&self) -> &Observer {
        &self.observer
    }

    /// 同步状态
    pub fn sync_state(&self) {
        self.observer.sync_from_robot(&self.robot);
    }
}
```

### 4.4 Active<MitMode> 状态

```rust
impl Piper<Active<MitMode>> {
    /// 发送 MIT 模式力矩指令
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

        // TODO: 实现正确的 CRC 计算
        let crc = 0x00;

        let cmd = MitControlCommand::new(joint_index, pos_ref, vel_ref, kp_f32, kd_f32, t_ref, crc);
        let frame = cmd.to_frame();

        // ✅ 使用 robot 模块的实时命令插槽（覆盖策略）
        self.robot.send_realtime(frame)?;

        Ok(())
    }

    /// 失能机械臂（返回 Standby 状态）
    pub fn disable(self, timeout: Duration) -> Result<Piper<Standby>> {
        use crate::protocol::control::MotorEnableCommand;
        let cmd = MotorEnableCommand::disable_all();
        let frame = cmd.to_frame();

        self.robot.send_reliable(frame)?;

        // 等待失能完成
        self.wait_for_all_disabled(timeout)?;

        // 类型转换
        let new_piper = Piper {
            robot: self.robot.clone(),
            observer: self.observer.clone(),
            state_monitor: self.state_monitor.clone(),
            _state: PhantomData,
        };

        std::mem::forget(self);
        Ok(new_piper)
    }

    /// 等待全部失能完成（内部方法）
    fn wait_for_all_disabled(&self, timeout: Duration) -> Result<()> {
        let start = Instant::now();
        let poll_interval = Duration::from_millis(10);

        loop {
            if start.elapsed() > timeout {
                return Err(RobotError::Timeout {
                    timeout_ms: timeout.as_millis() as u64,
                });
            }

            let driver_state = self.robot.get_joint_driver_low_speed();
            let enabled_mask = driver_state.driver_enabled_mask;

            if enabled_mask == 0 {
                return Ok(());
            }

            std::thread::sleep(poll_interval);
        }
    }

    /// 获取 Observer（只读）
    pub fn observer(&self) -> &Observer {
        &self.observer
    }

    /// 同步状态
    pub fn sync_state(&self) {
        self.observer.sync_from_robot(&self.robot);
    }
}
```

### 4.5 Drop 实现

```rust
impl<State> Drop for Piper<State> {
    fn drop(&mut self) {
        // 停止状态监控线程
        if let Some(mut monitor) = self.state_monitor.take() {
            monitor.stop();
        }

        // 失能全部关节（忽略错误，因为可能已经失能）
        let _ = self.disable_all();
    }
}
```

---

## 5. StateMonitor 实现

```rust
// src/high_level/client/state_monitor.rs
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

## 6. API 使用示例

### 6.1 连接和使能

```rust
use piper_sdk::high_level::state::*;
use piper_sdk::high_level::types::*;
use piper_sdk::can::SocketCanAdapter;
use std::time::Duration;

// 创建 CAN 适配器
let can_adapter = SocketCanAdapter::new("can0").unwrap();

// 连接到机械臂
let config = ConnectionConfig {
    interface: "can0".to_string(),
    timeout: Duration::from_secs(5),
};
let mut robot = Piper::connect(can_adapter, config)?;

// 使能全部关节
let robot = robot.enable_all()?;

// 或者只使能部分关节
// let robot = robot.enable_joints(&[Joint::J1, Joint::J2, Joint::J3])?;
```

### 6.2 MIT 模式控制

```rust
// MIT 模式控制
robot.command_torques(
    Joint::J1,
    Rad(1.0),
    0.5,
    10.0,
    2.0,
    NewtonMeter(5.0),
)?;

// 检查状态
let observer = robot.observer();
println!("J1 enabled: {}", observer.is_joint_enabled(0)); // J1
println!("All enabled: {}", observer.is_all_enabled());
println!("Partially enabled: {}", observer.is_partially_enabled());
```

### 6.3 状态查询

```rust
// 获取关节位置
let positions = observer.joint_positions();
for joint in [Joint::J1, Joint::J2, Joint::J3, Joint::J4, Joint::J5, Joint::J6] {
    println!("{:?}: {:.3} rad", joint, positions[joint].to_deg());
}

// 获取关节速度
let velocities = observer.joint_velocities();

// 获取关节力矩
let torques = observer.joint_torques();

// 获取使能掩码
let enabled_mask = observer.joint_enabled_mask();
println!("Enabled mask: 0b{:06b}", enabled_mask);

// 检查指定关节是否使能
if observer.is_joint_enabled(0) {
    println!("J1 is enabled");
}
```

### 6.4 失能

```rust
// 失能全部关节
let robot = robot.disable(Duration::from_secs(2))?;

// 或者只失能部分关节
robot.disable_joints(&[Joint::J4, Joint::J5, Joint::J6])?;
```

---

## 7. 测试示例

### 7.1 单元测试

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_arm_controller_bit_mask() {
        let mut controller = ArmController::new();

        // 使能 J1, J3, J5
        controller.set_joint_enabled(0, true);
        controller.set_joint_enabled(2, true);
        controller.set_joint_enabled(4, true);

        // 检查状态
        assert!(controller.is_joint_enabled(0));
        assert!(!controller.is_joint_enabled(1));
        assert!(controller.is_joint_enabled(2));
        assert!(!controller.is_joint_enabled(3));
        assert!(controller.is_joint_enabled(4));
        assert!(!controller.is_joint_enabled(5));

        assert!(controller.is_partially_enabled());
        assert!(!controller.is_all_enabled());
        assert!(!controller.is_all_disabled());

        // 使能全部
        controller.set_all_enabled();
        assert!(controller.is_all_enabled());
        assert!(!controller.is_partially_enabled());

        // 失能全部
        controller.set_all_disabled();
        assert!(controller.is_all_disabled());
    }

    #[test]
    fn test_observer_bit_mask() {
        use parking_lot::RwLock;

        let observer = Observer::new(Arc::new(RwLock::new(RobotState::default())));

        // 设置部分使能（J1, J2, J3）
        observer.update_joint_enabled_mask(0b000111);

        assert!(observer.is_joint_enabled(0));
        assert!(observer.is_joint_enabled(1));
        assert!(observer.is_joint_enabled(2));
        assert!(!observer.is_joint_enabled(3));
        assert!(!observer.is_joint_enabled(4));
        assert!(!observer.is_joint_enabled(5));

        assert!(observer.is_partially_enabled());
        assert!(!observer.is_all_enabled());
        assert!(!observer.is_all_disabled());

        // 设置全部使能
        observer.update_joint_enabled_mask(0b111111);
        assert!(observer.is_all_enabled());
        assert!(!observer.is_partially_enabled());
    }
}
```

### 7.2 集成测试

```rust
#[cfg(test)]
mod integration_tests {
    use super::*;
    use crate::can::MockCanAdapter;

    #[test]
    fn test_high_level_with_robot_and_protocol() {
        // 创建 Mock CAN 适配器
        let can_adapter = MockCanAdapter::new();

        // 使用 robot 模块创建 Piper
        let robot = Arc::new(robot::Piper::new_dual_thread(can_adapter, None).unwrap());

        // 创建 Observer
        let observer = Observer::new(Arc::new(RwLock::new(RobotState::default())));

        // 创建 StateMonitor
        let monitor_config = StateMonitorConfig {
            sync_interval: Duration::from_millis(10),
        };
        let _monitor = StateMonitor::new(
            robot.clone(),
            observer.clone(),
            monitor_config,
        );

        // 测试使能命令（使用 protocol 模块的类型安全接口）
        let raw_commander = RawCommander::new(
            Arc::new(StateTracker::new()),
            robot,
        );

        // 使能全部关节
        assert!(raw_commander.enable_arm().is_ok());

        // 等待状态同步
        std::thread::sleep(Duration::from_millis(100));

        // 验证状态已更新
        let enabled_mask = observer.joint_enabled_mask();
        assert_eq!(enabled_mask, 0b111111);
        assert!(observer.is_all_enabled());
    }
}
```

---

## 8. 迁移指南

### 8.1 从旧 API 迁移到新 API

**旧 API：**
```rust
// 使能机械臂
robot.enable_arm()?;

// 检查是否使能
if observer.is_arm_enabled() {
    println!("Arm is enabled");
}
```

**新 API：**
```rust
// 使能全部关节（等效于旧 API）
robot.enable_all()?;

// 或者使能部分关节（新功能）
robot.enable_joints(&[Joint::J1, Joint::J2, Joint::J3])?;

// 检查是否使能
if observer.is_all_enabled() {
    println!("All joints are enabled");
}

// 或者检查部分使能（新功能）
if observer.is_partially_enabled() {
    println!("Some joints are enabled");
}
```

### 8.2 类型转换

**旧 API（deprecated）：**
```rust
pub enum ArmController {
    Enabled,
    Standby,
    Error,
    Disconnected,
}
```

**新 API：**
```rust
pub struct ArmController {
    enabled_mask: u8,
    overall_state: OverallState,
}

// 提供转换方法
impl From<ArmController> for ArmControllerState {
    fn from(controller: ArmController) -> Self {
        match controller.overall_state() {
            OverallState::AllEnabled => ArmControllerState::Enabled,
            OverallState::AllDisabled => ArmControllerState::Standby,
            OverallState::Error => ArmControllerState::Error,
            OverallState::Disconnected => ArmControllerState::Disconnected,
            OverallState::PartiallyEnabled => ArmControllerState::Standby,
        }
    }
}
```

---

**文档版本：** v1.0
**创建时间：** 2025-01-23
**最后更新：** 2025-01-23

