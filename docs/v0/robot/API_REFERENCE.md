# Piper SDK API 参考文档

> **版本**：v0.0.1
> **最后更新**：2024年

本文档描述了 Piper SDK 的完整 API，包括所有公共方法和状态结构。

---

## 📋 目录

- [核心 API](#核心-api)
- [状态结构](#状态结构)
- [废弃 API](#废弃-api)
- [迁移指南](#迁移指南)

---

## 核心 API

### `Piper` 结构体

`Piper` 是 SDK 的主要接口，提供对机器人状态的访问和控制命令的发送。

#### 创建实例

```rust
use piper_sdk::robot::PiperBuilder;

let robot = PiperBuilder::new()
    .interface("can0")?  // Linux: SocketCAN 接口名
    .baud_rate(1_000_000)?  // CAN 波特率
    .build()?;
```

---

### 运动状态 API（500Hz，无锁）

#### `get_joint_position() -> JointPositionState`

获取关节位置状态（无锁，纳秒级返回）。

**更新频率**：500Hz
**性能**：无锁读取（ArcSwap::load），适合高频控制循环

**示例**：
```rust
let joint_pos = robot.get_joint_position();
println!("Joint positions: {:?}", joint_pos.joint_pos);
println!("Hardware timestamp: {} us", joint_pos.hardware_timestamp_us);
println!("System timestamp: {} us", joint_pos.system_timestamp_us);

// 检查帧完整性
if joint_pos.is_fully_valid() {
    println!("All frames received");
} else {
    println!("Missing frames: {:?}", joint_pos.missing_frames());
}
```

#### `get_end_pose() -> EndPoseState`

获取末端位姿状态（无锁，纳秒级返回）。

**更新频率**：500Hz
**性能**：无锁读取（ArcSwap::load），适合高频控制循环

**示例**：
```rust
let end_pose = robot.get_end_pose();
println!("End pose: {:?}", end_pose.end_pose);
println!("Frame valid mask: 0b{:08b}", end_pose.frame_valid_mask);

// 检查帧完整性
if end_pose.is_fully_valid() {
    println!("All frames received");
}
```

#### `capture_motion_snapshot() -> MotionSnapshot`

原子性地获取关节位置和末端位姿的最新快照。

**性能**：无锁读取（两次 ArcSwap::load），适合需要同时使用关节位置和末端位姿的场景

**示例**：
```rust
let snapshot = robot.capture_motion_snapshot();
println!("Joint positions: {:?}", snapshot.joint_position.joint_pos);
println!("End pose: {:?}", snapshot.end_pose.end_pose);

// 计算末端执行器相对于关节的位置
// ...
```

---

### 控制状态 API（100Hz，无锁）

#### `get_robot_control() -> RobotControlState`

获取机器人控制状态（无锁）。

**更新频率**：100Hz
**性能**：无锁读取（ArcSwap::load）

**示例**：
```rust
let control = robot.get_robot_control();
println!("Control mode: {}", control.control_mode);
println!("Robot status: {}", control.robot_status);
println!("Is enabled: {}", control.is_enabled);

// 检查故障码（位掩码）
if control.is_angle_limit(0) {
    println!("Joint 1 angle limit reached!");
}

if control.is_comm_error(2) {
    println!("Joint 3 communication error!");
}

// 检查反馈计数器（用于检测链路是否卡死）
println!("Feedback counter: {}", control.feedback_counter);
```

#### `get_gripper() -> GripperState`

获取夹爪状态（无锁）。

**更新频率**：100Hz
**性能**：无锁读取（ArcSwap::load）

**示例**：
```rust
let gripper = robot.get_gripper();
println!("Gripper travel: {:.2} mm", gripper.travel);
println!("Gripper torque: {:.2} N·m", gripper.torque);

// 检查状态
if gripper.is_voltage_low() {
    println!("Gripper voltage low!");
}

if gripper.is_moving() {
    println!("Gripper is moving");
}

// 检查状态码
println!("Status code: 0x{:02X}", gripper.status_code);
```

---

### 诊断状态 API（40Hz，无锁）

#### `get_joint_driver_low_speed() -> JointDriverLowSpeedState`

获取关节驱动器低速反馈状态（无锁，Wait-Free）。

**更新频率**：40Hz
**性能**：无锁读取（ArcSwap::load，Wait-Free），多线程读取不会阻塞

**示例**：
```rust
let driver_state = robot.get_joint_driver_low_speed();

// 读取温度
for i in 0..6 {
    println!("Joint {} motor temp: {:.1}°C", i + 1, driver_state.motor_temps[i]);
    println!("Joint {} driver temp: {:.1}°C", i + 1, driver_state.driver_temps[i]);
}

// 读取电压和电流
for i in 0..6 {
    println!("Joint {} voltage: {:.2}V", i + 1, driver_state.joint_voltage[i]);
    println!("Joint {} current: {:.2}A", i + 1, driver_state.joint_bus_current[i]);
}

// 检查状态（位掩码）
for i in 0..6 {
    if driver_state.is_voltage_low(i) {
        println!("Joint {} voltage low!", i + 1);
    }
    if driver_state.is_motor_over_temp(i) {
        println!("Joint {} motor over temperature!", i + 1);
    }
    if driver_state.is_enabled(i) {
        println!("Joint {} driver enabled", i + 1);
    }
}

// 检查完整性
if driver_state.is_fully_valid() {
    println!("All joint driver states received");
} else {
    println!("Missing joints: {:?}", driver_state.missing_joints());
}
```

---

### 配置状态 API（按需查询，读锁）

#### `get_collision_protection() -> Result<CollisionProtectionState, RobotError>`

获取碰撞保护状态（读锁）。

**更新频率**：按需查询
**性能**：读锁（RwLock::read）

**示例**：
```rust
let protection = robot.get_collision_protection()?;
println!("Protection levels: {:?}", protection.protection_levels);
println!("Hardware timestamp: {} us", protection.hardware_timestamp_us);
```

#### `get_joint_limit_config() -> Result<JointLimitConfigState, RobotError>`

获取关节限制配置状态（读锁）。

**更新频率**：按需查询
**性能**：读锁（RwLock::read）

**示例**：
```rust
let limits = robot.get_joint_limit_config()?;

// 读取关节限制
for i in 0..6 {
    println!("Joint {} max: {:.2} rad", i + 1, limits.joint_limits_max[i]);
    println!("Joint {} min: {:.2} rad", i + 1, limits.joint_limits_min[i]);
    println!("Joint {} max velocity: {:.2} rad/s", i + 1, limits.joint_max_velocity[i]);
}

// 检查完整性
if limits.is_fully_valid() {
    println!("All joint limits received");
} else {
    println!("Missing joints: {:?}", limits.missing_joints());
}
```

#### `get_joint_accel_config() -> Result<JointAccelConfigState, RobotError>`

获取关节加速度限制配置状态（读锁）。

**更新频率**：按需查询
**性能**：读锁（RwLock::read）

**示例**：
```rust
let accel_limits = robot.get_joint_accel_config()?;

for i in 0..6 {
    println!("Joint {} max accel: {:.2} rad/s²", i + 1, accel_limits.max_acc_limits[i]);
}

if accel_limits.is_fully_valid() {
    println!("All acceleration limits received");
}
```

#### `get_end_limit_config() -> Result<EndLimitConfigState, RobotError>`

获取末端限制配置状态（读锁）。

**更新频率**：按需查询
**性能**：读锁（RwLock::read）

**示例**：
```rust
let end_limits = robot.get_end_limit_config()?;
println!("Max linear velocity: {:.2} m/s", end_limits.max_end_linear_velocity);
println!("Max angular velocity: {:.2} rad/s", end_limits.max_end_angular_velocity);
println!("Max linear accel: {:.2} m/s²", end_limits.max_end_linear_accel);
println!("Max angular accel: {:.2} rad/s²", end_limits.max_end_angular_accel);

if end_limits.is_valid {
    println!("End limits are valid");
}
```

---

### 其他 API

#### `get_joint_dynamic() -> JointDynamicState`

获取关节动态状态（速度、电流）。

**更新频率**：500Hz
**性能**：无锁读取（ArcSwap::load）

#### `wait_for_feedback(timeout: Duration) -> Result<(), RobotError>`

等待接收到第一个有效反馈（用于初始化）。

**示例**：
```rust
robot.wait_for_feedback(Duration::from_secs(5))?;
println!("Robot feedback received!");
```

#### `get_fps() -> FpsResult`

获取 FPS 统计结果。

**示例**：
```rust
let fps = robot.get_fps();
println!("Joint position FPS: {:.2}", fps.joint_position);
println!("End pose FPS: {:.2}", fps.end_pose);
println!("Robot control FPS: {:.2}", fps.robot_control);
println!("Gripper FPS: {:.2}", fps.gripper);
```

#### `send_frame(frame: PiperFrame) -> Result<(), RobotError>`

发送控制帧（非阻塞）。

#### `send_frame_blocking(frame: PiperFrame, timeout: Duration) -> Result<(), RobotError>`

发送控制帧（阻塞，带超时）。

---

## 状态结构

### 运动状态

#### `JointPositionState`

关节位置状态。

**字段**：
- `hardware_timestamp_us: u64` - 硬件时间戳（微秒）
- `system_timestamp_us: u64` - 系统时间戳（微秒）
- `joint_pos: [f64; 6]` - 6个关节的位置（弧度）
- `frame_valid_mask: u8` - 帧有效性掩码（bit 0-2 对应 0x2A5-0x2A7）

**方法**：
- `is_fully_valid() -> bool` - 检查是否接收到完整的帧组
- `missing_frames() -> Vec<u8>` - 返回缺失的帧 ID 列表

#### `EndPoseState`

末端位姿状态。

**字段**：
- `hardware_timestamp_us: u64` - 硬件时间戳（微秒）
- `system_timestamp_us: u64` - 系统时间戳（微秒）
- `end_pose: [f64; 6]` - 末端位姿 [x, y, z, roll, pitch, yaw]
- `frame_valid_mask: u8` - 帧有效性掩码（bit 0-2 对应 0x2A2-0x2A4）

**方法**：
- `is_fully_valid() -> bool` - 检查是否接收到完整的帧组
- `missing_frames() -> Vec<u8>` - 返回缺失的帧 ID 列表

#### `MotionSnapshot`

运动快照（组合状态）。

**字段**：
- `joint_position: JointPositionState` - 关节位置状态
- `end_pose: EndPoseState` - 末端位姿状态

---

### 控制状态

#### `RobotControlState`

机器人控制状态。

**字段**：
- `hardware_timestamp_us: u64` - 硬件时间戳（微秒）
- `system_timestamp_us: u64` - 系统时间戳（微秒）
- `control_mode: u8` - 控制模式
- `robot_status: u8` - 机器人状态
- `move_mode: u8` - 运动模式
- `teach_status: u8` - 示教状态
- `motion_status: u8` - 运动状态
- `trajectory_point_index: u8` - 轨迹点索引
- `fault_angle_limit_mask: u8` - 角度限制故障掩码（bit 0-5 对应 J1-J6）
- `fault_comm_error_mask: u8` - 通信错误故障掩码（bit 0-5 对应 J1-J6）
- `is_enabled: bool` - 是否启用
- `feedback_counter: u8` - 反馈计数器（用于检测链路是否卡死）

**方法**：
- `is_angle_limit(joint_index: usize) -> bool` - 检查指定关节是否达到角度限制
- `is_comm_error(joint_index: usize) -> bool` - 检查指定关节是否有通信错误

#### `GripperState`

夹爪状态。

**字段**：
- `hardware_timestamp_us: u64` - 硬件时间戳（微秒）
- `system_timestamp_us: u64` - 系统时间戳（微秒）
- `travel: f64` - 夹爪行程（毫米）
- `torque: f64` - 夹爪扭矩（N·m）
- `status_code: u8` - 状态码（原始字节）
- `last_travel: f64` - 上次行程（用于判断是否在运动）

**方法**：
- `is_voltage_low() -> bool` - 检查电压是否过低
- `is_motor_over_temp() -> bool` - 检查电机是否过温
- `is_moving() -> bool` - 检查是否在运动（基于 travel 变化率）

---

### 诊断状态

#### `JointDriverLowSpeedState`

关节驱动器低速反馈状态。

**字段**：
- `hardware_timestamp_us: u64` - 最后更新的硬件时间戳（微秒）
- `system_timestamp_us: u64` - 最后更新的系统时间戳（微秒）
- `motor_temps: [f32; 6]` - 电机温度（°C）
- `driver_temps: [f32; 6]` - 驱动器温度（°C）
- `joint_voltage: [f32; 6]` - 关节电压（V）
- `joint_bus_current: [f32; 6]` - 关节总线电流（A）
- `hardware_timestamps: [u64; 6]` - 每个关节的硬件时间戳（微秒）
- `system_timestamps: [u64; 6]` - 每个关节的系统时间戳（微秒）
- `valid_mask: u8` - 有效性掩码（bit 0-5 对应 J1-J6）

**位掩码字段**（`u8`，bit 0-5 对应 J1-J6）：
- `driver_voltage_low_mask: u8` - 电压过低掩码
- `driver_motor_over_temp_mask: u8` - 电机过温掩码
- `driver_over_current_mask: u8` - 过流掩码
- `driver_over_temp_mask: u8` - 驱动器过温掩码
- `driver_collision_protection_mask: u8` - 碰撞保护掩码
- `driver_error_mask: u8` - 驱动器错误掩码
- `driver_enabled_mask: u8` - 驱动器启用掩码
- `driver_stall_protection_mask: u8` - 堵转保护掩码

**方法**：
- `is_voltage_low(joint_index: usize) -> bool` - 检查指定关节电压是否过低
- `is_motor_over_temp(joint_index: usize) -> bool` - 检查指定关节电机是否过温
- `is_over_current(joint_index: usize) -> bool` - 检查指定关节是否过流
- `is_over_temp(joint_index: usize) -> bool` - 检查指定关节驱动器是否过温
- `is_collision_protection(joint_index: usize) -> bool` - 检查指定关节是否触发碰撞保护
- `is_error(joint_index: usize) -> bool` - 检查指定关节是否有错误
- `is_enabled(joint_index: usize) -> bool` - 检查指定关节驱动器是否启用
- `is_stall_protection(joint_index: usize) -> bool` - 检查指定关节是否触发堵转保护
- `is_fully_valid() -> bool` - 检查是否接收到所有关节的数据
- `missing_joints() -> Vec<usize>` - 返回缺失的关节索引列表

---

### 配置状态

#### `CollisionProtectionState`

碰撞保护状态。

**字段**：
- `hardware_timestamp_us: u64` - 硬件时间戳（微秒）
- `system_timestamp_us: u64` - 系统时间戳（微秒）
- `protection_levels: [u8; 6]` - 各关节的碰撞保护等级（0-8）

#### `JointLimitConfigState`

关节限制配置状态。

**字段**：
- `last_update_hardware_timestamp_us: u64` - 最后更新的硬件时间戳（微秒）
- `last_update_system_timestamp_us: u64` - 最后更新的系统时间戳（微秒）
- `joint_update_hardware_timestamps: [u64; 6]` - 每个关节的硬件时间戳（微秒）
- `joint_update_system_timestamps: [u64; 6]` - 每个关节的系统时间戳（微秒）
- `joint_limits_max: [f64; 6]` - 关节最大角度限制（弧度）
- `joint_limits_min: [f64; 6]` - 关节最小角度限制（弧度）
- `joint_max_velocity: [f64; 6]` - 关节最大速度限制（弧度/秒）
- `valid_mask: u8` - 有效性掩码（bit 0-5 对应 J1-J6）

**方法**：
- `is_fully_valid() -> bool` - 检查是否接收到所有关节的配置
- `missing_joints() -> Vec<usize>` - 返回缺失的关节索引列表

#### `JointAccelConfigState`

关节加速度限制配置状态。

**字段**：
- `last_update_hardware_timestamp_us: u64` - 最后更新的硬件时间戳（微秒）
- `last_update_system_timestamp_us: u64` - 最后更新的系统时间戳（微秒）
- `joint_update_hardware_timestamps: [u64; 6]` - 每个关节的硬件时间戳（微秒）
- `joint_update_system_timestamps: [u64; 6]` - 每个关节的系统时间戳（微秒）
- `max_acc_limits: [f64; 6]` - 关节最大加速度限制（弧度/秒²）
- `valid_mask: u8` - 有效性掩码（bit 0-5 对应 J1-J6）

**方法**：
- `is_fully_valid() -> bool` - 检查是否接收到所有关节的配置
- `missing_joints() -> Vec<usize>` - 返回缺失的关节索引列表

#### `EndLimitConfigState`

末端限制配置状态。

**字段**：
- `last_update_hardware_timestamp_us: u64` - 最后更新的硬件时间戳（微秒）
- `last_update_system_timestamp_us: u64` - 最后更新的系统时间戳（微秒）
- `max_end_linear_velocity: f64` - 末端最大线速度（m/s）
- `max_end_angular_velocity: f64` - 末端最大角速度（rad/s）
- `max_end_linear_accel: f64` - 末端最大线加速度（m/s²）
- `max_end_angular_accel: f64` - 末端最大角加速度（rad/s²）
- `is_valid: bool` - 是否有效

---

## 废弃 API

以下 API 已废弃，将在未来版本中移除。请使用新的 API 替代。

### `get_core_motion() -> CoreMotionState` ⚠️ 已废弃

**替代方案**：使用 `get_joint_position()` 和 `get_end_pose()` 或 `capture_motion_snapshot()`

```rust
// 旧代码
let core = robot.get_core_motion();
println!("Joint positions: {:?}", core.joint_pos);
println!("End pose: {:?}", core.end_pose);

// 新代码
let joint_pos = robot.get_joint_position();
let end_pose = robot.get_end_pose();
println!("Joint positions: {:?}", joint_pos.joint_pos);
println!("End pose: {:?}", end_pose.end_pose);

// 或者使用快照
let snapshot = robot.capture_motion_snapshot();
println!("Joint positions: {:?}", snapshot.joint_position.joint_pos);
println!("End pose: {:?}", snapshot.end_pose.end_pose);
```

### `get_control_status() -> ControlStatusState` ⚠️ 已废弃

**替代方案**：使用 `get_robot_control()` 和 `get_gripper()`

```rust
// 旧代码
let status = robot.get_control_status();
println!("Control mode: {}", status.control_mode);
println!("Gripper travel: {}", status.gripper_travel);

// 新代码
let control = robot.get_robot_control();
let gripper = robot.get_gripper();
println!("Control mode: {}", control.control_mode);
println!("Gripper travel: {}", gripper.travel);
```

### `get_diagnostic_state() -> Result<DiagnosticState, RobotError>` ⚠️ 已废弃

**替代方案**：使用 `get_joint_driver_low_speed()` 和 `get_collision_protection()`

```rust
// 旧代码
let diag = robot.get_diagnostic_state()?;
println!("Motor temps: {:?}", diag.motor_temps);

// 新代码
let driver_state = robot.get_joint_driver_low_speed();
println!("Motor temps: {:?}", driver_state.motor_temps);
```

### `get_config_state() -> Result<ConfigState, RobotError>` ⚠️ 已废弃

**替代方案**：使用 `get_joint_limit_config()`、`get_joint_accel_config()` 和 `get_end_limit_config()`

```rust
// 旧代码
let config = robot.get_config_state()?;
println!("Joint limits: {:?}", config.joint_limits_max);

// 新代码
let limits = robot.get_joint_limit_config()?;
println!("Joint limits: {:?}", limits.joint_limits_max);
```

---

## 迁移指南

### 从 `CoreMotionState` 迁移

**旧代码**：
```rust
let core = robot.get_core_motion();
let joint_pos = core.joint_pos;
let end_pose = core.end_pose;
let timestamp = core.timestamp_us;
```

**新代码**：
```rust
// 方案1：分别获取（推荐用于需要独立时间戳的场景）
let joint_pos_state = robot.get_joint_position();
let end_pose_state = robot.get_end_pose();
let joint_pos = joint_pos_state.joint_pos;
let end_pose = end_pose_state.end_pose;
let joint_timestamp = joint_pos_state.hardware_timestamp_us;
let end_timestamp = end_pose_state.hardware_timestamp_us;

// 方案2：使用快照（推荐用于需要逻辑原子性的场景）
let snapshot = robot.capture_motion_snapshot();
let joint_pos = snapshot.joint_position.joint_pos;
let end_pose = snapshot.end_pose.end_pose;
```

### 从 `ControlStatusState` 迁移

**旧代码**：
```rust
let status = robot.get_control_status();
let control_mode = status.control_mode;
let gripper_travel = status.gripper_travel;
let fault_angle_limit = status.fault_angle_limit;
```

**新代码**：
```rust
let control = robot.get_robot_control();
let gripper = robot.get_gripper();
let control_mode = control.control_mode;
let gripper_travel = gripper.travel;

// 位掩码访问
let fault_angle_limit_j1 = control.is_angle_limit(0);
```

### 从 `DiagnosticState` 迁移

**旧代码**：
```rust
let diag = robot.get_diagnostic_state()?;
let motor_temps = diag.motor_temps;
let driver_voltage_low = diag.driver_voltage_low;
```

**新代码**：
```rust
let driver_state = robot.get_joint_driver_low_speed();
let motor_temps = driver_state.motor_temps;

// 位掩码访问
let driver_voltage_low_j1 = driver_state.is_voltage_low(0);
```

---

## 性能说明

### 无锁读取（ArcSwap）

以下 API 使用 `ArcSwap` 实现无锁读取，适合高频控制循环（500Hz）：

- `get_joint_position()` - < 1000ns
- `get_end_pose()` - < 1000ns
- `capture_motion_snapshot()` - < 2000ns
- `get_robot_control()` - < 1000ns
- `get_gripper()` - < 1000ns
- `get_joint_driver_low_speed()` - < 1000ns（Wait-Free）

### 读锁（RwLock）

以下 API 使用 `RwLock` 实现读锁，适合低频查询：

- `get_collision_protection()` - < 2000ns
- `get_joint_limit_config()` - < 2000ns
- `get_joint_accel_config()` - < 2000ns
- `get_end_limit_config()` - < 2000ns

---

## 注意事项

1. **时间戳差异**：`JointPositionState` 和 `EndPoseState` 不是原子更新的，它们的时间戳可能不同。如需逻辑原子性，请使用 `capture_motion_snapshot()`。

2. **帧完整性**：使用 `is_fully_valid()` 和 `missing_frames()` / `missing_joints()` 检查数据完整性，特别是在 CAN 总线可能出现丢包的情况下。

3. **位掩码访问**：使用辅助方法（如 `is_angle_limit()`、`is_voltage_low()`）访问位掩码字段，而不是直接操作位掩码。

4. **并发安全**：所有 API 都是线程安全的，可以在多线程环境中并发调用。

---

**最后更新**：2024年
**参考文档**：
- [状态结构重构分析报告](state_structure_refactoring_analysis.md)
- [执行计划](state_refactoring_todo.md)

