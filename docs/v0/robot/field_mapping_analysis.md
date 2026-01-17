# Driver 状态字段与 Protocol 反馈帧字段对比分析报告

## 1. 概述

本文档详细对比 `driver` 模块中定义的 `MotionState`、`DiagnosticState`、`ConfigState` 三个状态结构体的字段，
与 `protocol` 模块中定义的反馈帧字段，逐一检查是否有遗漏或不匹配。

## 2. MotionState 字段分析

### 2.1. 当前定义（implementation_plan.md）

```rust
pub struct MotionState {
    pub timestamp_us: u64,           // 时间戳（微秒）
    pub joint_pos: [f64; 6],         // 关节位置（弧度）
    pub joint_vel: [f64; 6],         // 关节速度（rad/s）
    pub joint_torque: [f64; 6],      // 关节电流/力矩（A 或 N·m）
    pub end_pose: [f64; 6],          // 末端位姿 [X, Y, Z, Rx, Ry, Rz]
    pub gripper_pos: f64,            // 夹爪位置（毫米，0-100% 开合度）
    pub control_mode: u8,            // 控制模式
    pub is_enabled: bool,            // 使能状态
    pub robot_status: u8,            // 机器人状态
}
```

### 2.2. Protocol 反馈帧字段映射

#### 2.2.1. 关节位置（✓ 已包含）

**来源：** `JointFeedback12` (0x2A5), `JointFeedback34` (0x2A6), `JointFeedback56` (0x2A7)

- `JointFeedback12`: `j1_rad()`, `j2_rad()` → `joint_pos[0]`, `joint_pos[1]` ✓
- `JointFeedback34`: `j3_rad()`, `j4_rad()` → `joint_pos[2]`, `joint_pos[3]` ✓
- `JointFeedback56`: `j5_rad()`, `j6_rad()` → `joint_pos[4]`, `joint_pos[5]` ✓

**状态：** ✅ 已完整映射

#### 2.2.2. 关节速度（⚠️ 部分遗漏）

**来源：** `JointDriverHighSpeedFeedback` (0x251~0x256)

- `JointDriverHighSpeedFeedback.speed()` → `joint_vel[joint_index - 1]` ✓

**状态：** ✅ 已映射，但需要注意：
- 需要从 6 个独立的 CAN 帧（0x251~0x256）分别更新每个关节的速度
- 这些帧是**高频反馈**，更新频率可能与关节角度反馈不同

#### 2.2.3. 关节电流/力矩（⚠️ 需要确认单位）

**来源：** `JointDriverHighSpeedFeedback` (0x251~0x256)

- `JointDriverHighSpeedFeedback.current()` → `joint_torque[joint_index - 1]` ⚠️

**问题：**
1. **单位不一致**：`current()` 返回的是**电流（A）**，不是力矩（N·m）
2. **缺少力矩数据**：协议中没有直接的力矩反馈，只有电流反馈

**建议：**
- 将字段名改为 `joint_current: [f64; 6]`（单位：A）
- 或者添加注释说明：电流可以作为力矩的近似值（需要乘以力矩常数）

**状态：** ⚠️ 字段名不准确，应改为 `joint_current`

#### 2.2.4. 末端位姿（✓ 已包含）

**来源：** `EndPoseFeedback1` (0x2A2), `EndPoseFeedback2` (0x2A3), `EndPoseFeedback3` (0x2A4)

- `EndPoseFeedback1.x()`, `.y()` → `end_pose[0]`, `end_pose[1]` ✓
- `EndPoseFeedback2.z()`, `.rx_rad()` → `end_pose[2]`, `end_pose[3]` ✓
- `EndPoseFeedback3.ry_rad()`, `.rz_rad()` → `end_pose[4]`, `end_pose[5]` ✓

**注意：**
- `EndPoseFeedback1.x()` 和 `.y()` 返回的是 **mm**，需要转换为 **米**（除以 1000.0）
- `EndPoseFeedback2.z()` 返回的是 **mm**，需要转换为 **米**
- `EndPoseFeedback3.ry_rad()`, `.rz_rad()` 返回的是 **弧度**，单位正确

**状态：** ⚠️ 单位转换需要注意（mm → m）

#### 2.2.5. 夹爪位置（⚠️ 单位不一致）

**来源：** `GripperFeedback` (0x2A8)

- `GripperFeedback.travel()` → `gripper_pos` ⚠️

**问题：**
- `GripperFeedback.travel()` 返回的是 **mm**（行程，0.001mm 单位）
- 当前定义中 `gripper_pos` 的单位是 **毫米，0-100% 开合度**，语义不匹配

**建议：**
- 改为 `gripper_travel: f64`（单位：mm）或 `gripper_position_mm: f64`
- 或者添加 `gripper_open_percent: f64`（如果协议支持百分比）

**状态：** ⚠️ 字段语义不匹配

#### 2.2.6. 控制状态（✓ 已包含）

**来源：** `RobotStatusFeedback` (0x2A1)

- `RobotStatusFeedback.control_mode` → `control_mode` ✓
- `RobotStatusFeedback.robot_status` → `robot_status` ✓
- `robot_status == RobotStatus::Normal` → `is_enabled` ✓

**状态：** ✅ 已完整映射

#### 2.2.7. 遗漏的字段

##### ❌ MOVE 模式（MoveMode）

**来源：** `RobotStatusFeedback.move_mode` (0x2A1)

- `RobotStatusFeedback.move_mode` (MoveMode 枚举：MoveP, MoveJ, MoveL, MoveC, MoveM)

**建议：** 添加 `move_mode: u8` 或 `move_mode: MoveMode`

##### ❌ 示教状态（TeachStatus）

**来源：** `RobotStatusFeedback.teach_status` (0x2A1)

- `RobotStatusFeedback.teach_status` (TeachStatus 枚举：Closed, StartRecord, EndRecord, Execute, Pause, Continue, Terminate, MoveToStart)

**建议：** 添加 `teach_status: u8` 或 `teach_status: TeachStatus`

##### ❌ 运动状态（MotionStatus）

**来源：** `RobotStatusFeedback.motion_status` (0x2A1)

- `RobotStatusFeedback.motion_status` (MotionStatus 枚举：Arrived, NotArrived)

**建议：** 添加 `motion_status: u8` 或 `motion_status: MotionStatus`

##### ❌ 轨迹点索引

**来源：** `RobotStatusFeedback.trajectory_point_index` (0x2A1)

- `RobotStatusFeedback.trajectory_point_index` (u8)

**建议：** 添加 `trajectory_point_index: u8`

##### ❌ 故障码（位域）

**来源：** `RobotStatusFeedback` (0x2A1)

- `RobotStatusFeedback.fault_code_angle_limit` (FaultCodeAngleLimit 位域：6 个关节的角度超限位)
- `RobotStatusFeedback.fault_code_comm_error` (FaultCodeCommError 位域：6 个关节的通信异常)

**建议：** 添加 `fault_angle_limit: [bool; 6]` 和 `fault_comm_error: [bool; 6]`

##### ❌ 关节位置（JointDriverHighSpeedFeedback）

**来源：** `JointDriverHighSpeedFeedback.position()` (0x251~0x256)

- `JointDriverHighSpeedFeedback.position()` (rad 单位，但单位可能不准确，见 TODO)

**注意：** 这与 `JointFeedback12/34/56` 中的位置不同。`JointDriverHighSpeedFeedback` 是高频反馈，可能用于速度控制，
而 `JointFeedback12/34/56` 是角度反馈，用于位置控制。

**建议：** 保留 `joint_pos`（来自 `JointFeedback12/34/56`），但可能需要添加 `joint_pos_driver: [f64; 6]`（来自 `JointDriverHighSpeedFeedback`）用于高频控制

##### ❌ 末端速度/加速度

**来源：** `JointEndVelocityAccelFeedback` (0x481~0x486)

- `JointEndVelocityAccelFeedback.linear_velocity()` → 末端线速度（m/s，每个关节）
- `JointEndVelocityAccelFeedback.angular_velocity()` → 末端角速度（rad/s，每个关节）
- `JointEndVelocityAccelFeedback.linear_accel()` → 末端线加速度（m/s²，每个关节）
- `JointEndVelocityAccelFeedback.angular_accel()` → 末端角加速度（rad/s²，每个关节）

**注意：** 这是**每个关节的末端速度和加速度**，不是全局的末端速度和加速度。

**建议：** 如果需要，添加：
```rust
pub joint_end_linear_velocity: [f64; 6],  // 每个关节的末端线速度（m/s）
pub joint_end_angular_velocity: [f64; 6], // 每个关节的末端角速度（rad/s）
pub joint_end_linear_accel: [f64; 6],     // 每个关节的末端线加速度（m/s²）
pub joint_end_angular_accel: [f64; 6],    // 每个关节的末端角加速度（rad/s²）
```

##### ❌ 夹爪扭矩

**来源：** `GripperFeedback.torque()` (0x2A8)

- `GripperFeedback.torque()` (N·m 单位)

**建议：** 添加 `gripper_torque: f64`（单位：N·m）

### 2.3. MotionState 总结

| 字段 | 来源 | 状态 | 备注 |
|------|------|------|------|
| `timestamp_us` | 手动生成 | ✅ | 无问题 |
| `joint_pos` | JointFeedback12/34/56 | ✅ | 已完整映射 |
| `joint_vel` | JointDriverHighSpeedFeedback | ✅ | 已映射 |
| `joint_torque` | JointDriverHighSpeedFeedback | ⚠️ | **应改为 `joint_current`** |
| `end_pose` | EndPoseFeedback1/2/3 | ⚠️ | **需要单位转换（mm → m）** |
| `gripper_pos` | GripperFeedback | ⚠️ | **语义不匹配，应改为 `gripper_travel`** |
| `control_mode` | RobotStatusFeedback | ✅ | 已映射 |
| `is_enabled` | RobotStatusFeedback (推导) | ✅ | 已映射 |
| `robot_status` | RobotStatusFeedback | ✅ | 已映射 |
| `move_mode` | RobotStatusFeedback | ❌ | **遗漏** |
| `teach_status` | RobotStatusFeedback | ❌ | **遗漏** |
| `motion_status` | RobotStatusFeedback | ❌ | **遗漏** |
| `trajectory_point_index` | RobotStatusFeedback | ❌ | **遗漏** |
| `fault_angle_limit` | RobotStatusFeedback | ❌ | **遗漏** |
| `fault_comm_error` | RobotStatusFeedback | ❌ | **遗漏** |
| `gripper_torque` | GripperFeedback | ❌ | **遗漏** |
| `joint_end_linear_velocity` | JointEndVelocityAccelFeedback | ❌ | **可选（如果需要）** |
| `joint_end_angular_velocity` | JointEndVelocityAccelFeedback | ❌ | **可选（如果需要）** |
| `joint_end_linear_accel` | JointEndVelocityAccelFeedback | ❌ | **可选（如果需要）** |
| `joint_end_angular_accel` | JointEndVelocityAccelFeedback | ❌ | **可选（如果需要）** |

## 3. DiagnosticState 字段分析

### 3.1. 当前定义（implementation_plan.md）

```rust
pub struct DiagnosticState {
    pub motor_temps: [f32; 6],      // 电机温度（°C）
    pub driver_temps: [f32; 6],     // 驱动器温度（°C）
    pub bus_voltage: f32,           // 母线电压（V）
    pub bus_current: f32,           // 母线电流（A）
    pub error_code: u16,            // 错误码（16-bit）
    pub connection_status: bool,    // 连接状态
    pub protection_level: u8,       // 碰撞保护等级（0-10）
}
```

### 3.2. Protocol 反馈帧字段映射

#### 3.2.1. 电机温度（✓ 已包含）

**来源：** `JointDriverLowSpeedFeedback` (0x261~0x266)

- `JointDriverLowSpeedFeedback.motor_temp()` → `motor_temps[joint_index - 1]` ✓

**状态：** ✅ 已完整映射

#### 3.2.2. 驱动器温度（✓ 已包含）

**来源：** `JointDriverLowSpeedFeedback` (0x261~0x266)

- `JointDriverLowSpeedFeedback.driver_temp()` → `driver_temps[joint_index - 1]` ✓

**状态：** ✅ 已完整映射

#### 3.2.3. 母线电压（⚠️ 单位不一致）

**来源：** `JointDriverLowSpeedFeedback` (0x261~0x266)

- `JointDriverLowSpeedFeedback.voltage()` → `bus_voltage` ⚠️

**问题：**
- `JointDriverLowSpeedFeedback` 包含**每个关节的电压**（0x261~0x266，6 个帧），不是全局的母线电压
- 当前定义中 `bus_voltage` 是单个值，但协议中有 6 个关节的电压

**建议：**
- 改为 `joint_voltage: [f32; 6]`（每个关节的电压）
- 或者从第一个关节的电压推导全局母线电压（假设所有关节电压相同）

**状态：** ⚠️ 语义不匹配（应该按关节存储）

#### 3.2.4. 母线电流（✓ 已包含）

**来源：** `JointDriverLowSpeedFeedback` (0x261~0x266)

- `JointDriverLowSpeedFeedback.bus_current()` → `bus_current` ⚠️

**问题：**
- 同样的问题：`JointDriverLowSpeedFeedback` 包含**每个关节的母线电流**（6 个帧），不是全局的母线电流

**建议：**
- 改为 `joint_bus_current: [f32; 6]`（每个关节的母线电流）
- 或者求和得到全局母线电流

**状态：** ⚠️ 语义不匹配（应该按关节存储或求和）

#### 3.2.5. 错误码（❌ 遗漏）

**来源：** `RobotStatusFeedback` (0x2A1)

- `RobotStatusFeedback.robot_status` 包含错误状态（如 `RobotStatus::EmergencyStop`, `RobotStatus::Collision` 等）

**建议：** `error_code` 可以从 `robot_status` 推导，但也可以直接使用 `robot_status` 作为错误码

**状态：** ✅ 可以通过 `robot_status` 获取（已在 MotionState 中）

#### 3.2.6. 连接状态（✓ 逻辑字段）

**来源：** 非协议字段，需要根据是否收到数据判断

**状态：** ✅ 逻辑字段，无问题

#### 3.2.7. 碰撞保护等级（✓ 已包含）

**来源：** `CollisionProtectionLevelFeedback` (0x47B)

- `CollisionProtectionLevelFeedback.levels` → `protection_level` ⚠️

**问题：**
- `CollisionProtectionLevelFeedback` 包含**6 个关节的保护等级**（数组），不是单个值

**建议：** 改为 `protection_levels: [u8; 6]`（每个关节的保护等级）

**状态：** ⚠️ 应改为数组

#### 3.2.8. 遗漏的字段

##### ❌ 驱动器状态（DriverStatus）

**来源：** `JointDriverLowSpeedFeedback.status` (0x261~0x266)

- `DriverStatus` 位域包含：
  - `voltage_low`: 电源电压过低
  - `motor_over_temp`: 电机过温
  - `driver_over_current`: 驱动器过流
  - `driver_over_temp`: 驱动器过温
  - `collision_protection`: 碰撞保护触发
  - `driver_error`: 驱动器错误
  - `enabled`: 驱动器使能状态
  - `stall_protection`: 堵转保护触发

**建议：** 添加 `driver_status: [[bool; 8]; 6]` 或分别存储每个状态：
```rust
pub driver_voltage_low: [bool; 6],
pub driver_motor_over_temp: [bool; 6],
pub driver_over_current: [bool; 6],
pub driver_over_temp: [bool; 6],
pub driver_collision_protection: [bool; 6],
pub driver_error: [bool; 6],
pub driver_enabled: [bool; 6],
pub driver_stall_protection: [bool; 6],
```

##### ❌ 夹爪状态（GripperStatus）

**来源：** `GripperFeedback.status` (0x2A8)

- `GripperStatus` 位域包含：
  - `voltage_low`: 电源电压过低
  - `motor_over_temp`: 电机过温
  - `driver_over_current`: 驱动器过流
  - `driver_over_temp`: 驱动器过温
  - `sensor_error`: 传感器异常
  - `driver_error`: 驱动器错误
  - `enabled`: 驱动器使能状态（反向逻辑）
  - `homed`: 回零状态

**建议：** 添加 `gripper_status: GripperStatus` 或分别存储

### 3.3. DiagnosticState 总结

| 字段 | 来源 | 状态 | 备注 |
|------|------|------|------|
| `motor_temps` | JointDriverLowSpeedFeedback | ✅ | 已完整映射 |
| `driver_temps` | JointDriverLowSpeedFeedback | ✅ | 已完整映射 |
| `bus_voltage` | JointDriverLowSpeedFeedback | ⚠️ | **应改为 `joint_voltage: [f32; 6]`** |
| `bus_current` | JointDriverLowSpeedFeedback | ⚠️ | **应改为 `joint_bus_current: [f32; 6]`** |
| `error_code` | RobotStatusFeedback (间接) | ✅ | 可通过 `robot_status` 获取 |
| `connection_status` | 逻辑字段 | ✅ | 无问题 |
| `protection_level` | CollisionProtectionLevelFeedback | ⚠️ | **应改为 `protection_levels: [u8; 6]`** |
| `driver_status` | JointDriverLowSpeedFeedback | ❌ | **遗漏（8 个状态位，6 个关节）** |
| `gripper_status` | GripperFeedback | ❌ | **遗漏（8 个状态位）** |

## 4. ConfigState 字段分析

### 4.1. 当前定义（implementation_plan.md）

```rust
pub struct ConfigState {
    pub firmware_version: String,        // 固件版本号
    pub joint_limits_max: [f64; 6],      // 关节角度上限（弧度）
    pub joint_limits_min: [f64; 6],      // 关节角度下限（弧度）
    pub max_acc_limit: f64,              // 最大加速度限制（rad/s²）
}
```

### 4.2. Protocol 配置反馈帧字段映射

#### 4.2.1. 固件版本（❌ 遗漏）

**来源：** 协议中**没有**固件版本反馈帧

**状态：** ⚠️ 无法从协议获取，需要通过其他方式（如设备描述符或专用查询命令）

#### 4.2.2. 关节角度限制（✓ 已包含）

**来源：** `MotorLimitFeedback` (0x473)

- `MotorLimitFeedback.max_angle()` → `joint_limits_max[joint_index - 1]` ⚠️
- `MotorLimitFeedback.min_angle()` → `joint_limits_min[joint_index - 1]` ⚠️

**问题：**
- `MotorLimitFeedback` 是**单关节反馈**（需要查询 6 次，每个关节一次），不是一次返回所有关节的限制
- 单位：`MotorLimitFeedback.max_angle()` 返回**度**，需要转换为**弧度**

**建议：**
- 需要累积 6 次查询结果
- 添加单位转换（度 → 弧度）

**状态：** ⚠️ 需要累积查询和单位转换

#### 4.2.3. 最大加速度限制（⚠️ 单位不一致）

**来源：** `MotorMaxAccelFeedback` (0x47C)

- `MotorMaxAccelFeedback.max_accel()` → `max_acc_limit` ⚠️

**问题：**
- `MotorMaxAccelFeedback` 是**单关节反馈**（需要查询 6 次，每个关节一次），不是单个值
- 单位一致（rad/s²），但需要累积 6 个关节的值

**建议：** 改为 `max_acc_limits: [f64; 6]`（每个关节的最大加速度限制）

**状态：** ⚠️ 应改为数组

#### 4.2.4. 遗漏的字段

##### ❌ 关节最大速度限制

**来源：** `MotorLimitFeedback.max_velocity()` (0x473)

- `MotorLimitFeedback.max_velocity()` (rad/s 单位)

**建议：** 添加 `joint_max_velocity: [f64; 6]`（每个关节的最大速度限制）

##### ❌ 末端速度/加速度参数

**来源：** `EndVelocityAccelFeedback` (0x478)

- `EndVelocityAccelFeedback.max_linear_velocity()` (m/s)
- `EndVelocityAccelFeedback.max_angular_velocity()` (rad/s)
- `EndVelocityAccelFeedback.max_linear_accel()` (m/s²)
- `EndVelocityAccelFeedback.max_angular_accel()` (rad/s²)

**建议：** 添加：
```rust
pub max_end_linear_velocity: f64,    // 末端最大线速度（m/s）
pub max_end_angular_velocity: f64,   // 末端最大角速度（rad/s）
pub max_end_linear_accel: f64,       // 末端最大线加速度（m/s²）
pub max_end_angular_accel: f64,      // 末端最大角加速度（rad/s²）
```

##### ❌ 夹爪/示教器参数

**来源：** `GripperTeachParamsFeedback` (0x47E)

- `GripperTeachParamsFeedback.teach_travel_coeff` (100~200，单位%)
- `GripperTeachParamsFeedback.max_travel_limit` (mm)
- `GripperTeachParamsFeedback.friction_coeff` (1-10)

**建议：** 添加（如果需要）：
```rust
pub teach_travel_coeff: u8,          // 示教器行程系数（100~200%）
pub gripper_max_travel_limit: u8,    // 夹爪最大行程限制（mm）
pub teach_friction_coeff: u8,        // 示教器摩擦系数（1-10）
```

### 4.3. ConfigState 总结

| 字段 | 来源 | 状态 | 备注 |
|------|------|------|------|
| `firmware_version` | 无协议支持 | ⚠️ | **无法从协议获取** |
| `joint_limits_max` | MotorLimitFeedback | ⚠️ | **需要累积查询和单位转换（度→弧度）** |
| `joint_limits_min` | MotorLimitFeedback | ⚠️ | **需要累积查询和单位转换（度→弧度）** |
| `max_acc_limit` | MotorMaxAccelFeedback | ⚠️ | **应改为 `max_acc_limits: [f64; 6]`** |
| `joint_max_velocity` | MotorLimitFeedback | ❌ | **遗漏** |
| `max_end_linear_velocity` | EndVelocityAccelFeedback | ❌ | **遗漏** |
| `max_end_angular_velocity` | EndVelocityAccelFeedback | ❌ | **遗漏** |
| `max_end_linear_accel` | EndVelocityAccelFeedback | ❌ | **遗漏** |
| `max_end_angular_accel` | EndVelocityAccelFeedback | ❌ | **遗漏** |
| `teach_travel_coeff` | GripperTeachParamsFeedback | ❌ | **可选（如果需要）** |
| `gripper_max_travel_limit` | GripperTeachParamsFeedback | ❌ | **可选（如果需要）** |
| `teach_friction_coeff` | GripperTeachParamsFeedback | ❌ | **可选（如果需要）** |

## 5. 修复建议

### 5.1. MotionState 修复

```rust
pub struct MotionState {
    pub timestamp_us: u64,

    // === 关节数据 ===
    pub joint_pos: [f64; 6],              // 关节位置（弧度）
    pub joint_vel: [f64; 6],              // 关节速度（rad/s）
    pub joint_current: [f64; 6],          // 关节电流（A）【修复：改名】

    // === 末端位姿 ===
    pub end_pose: [f64; 6],               // 末端位姿 [X(m), Y(m), Z(m), Rx(rad), Ry(rad), Rz(rad)]

    // === 夹爪 ===
    pub gripper_travel: f64,              // 夹爪行程（mm）【修复：改名】
    pub gripper_torque: f64,              // 夹爪扭矩（N·m）【新增】

    // === 控制状态 ===
    pub control_mode: u8,
    pub move_mode: u8,                    // 【新增】
    pub robot_status: u8,
    pub teach_status: u8,                 // 【新增】
    pub motion_status: u8,                // 【新增】
    pub trajectory_point_index: u8,       // 【新增】
    pub is_enabled: bool,

    // === 故障码 ===
    pub fault_angle_limit: [bool; 6],     // 【新增】各关节角度超限位
    pub fault_comm_error: [bool; 6],      // 【新增】各关节通信异常

    // === 可选：末端速度/加速度（每个关节） ===
    // 如果需要，可以添加：
    // pub joint_end_linear_velocity: [f64; 6],
    // pub joint_end_angular_velocity: [f64; 6],
    // pub joint_end_linear_accel: [f64; 6],
    // pub joint_end_angular_accel: [f64; 6],
}
```

### 5.2. DiagnosticState 修复

```rust
pub struct DiagnosticState {
    // === 温度 ===
    pub motor_temps: [f32; 6],            // 电机温度（°C）
    pub driver_temps: [f32; 6],           // 驱动器温度（°C）

    // === 电压/电流 ===
    pub joint_voltage: [f32; 6],          // 【修复】各关节电压（V）
    pub joint_bus_current: [f32; 6],      // 【修复】各关节母线电流（A）

    // === 保护等级 ===
    pub protection_levels: [u8; 6],       // 【修复】各关节碰撞保护等级（0-8）

    // === 驱动器状态（每个关节） ===
    pub driver_voltage_low: [bool; 6],            // 【新增】
    pub driver_motor_over_temp: [bool; 6],       // 【新增】
    pub driver_over_current: [bool; 6],          // 【新增】
    pub driver_over_temp: [bool; 6],             // 【新增】
    pub driver_collision_protection: [bool; 6],  // 【新增】
    pub driver_error: [bool; 6],                 // 【新增】
    pub driver_enabled: [bool; 6],               // 【新增】
    pub driver_stall_protection: [bool; 6],      // 【新增】

    // === 夹爪状态 ===
    pub gripper_voltage_low: bool,        // 【新增】
    pub gripper_motor_over_temp: bool,    // 【新增】
    pub gripper_over_current: bool,       // 【新增】
    pub gripper_over_temp: bool,          // 【新增】
    pub gripper_sensor_error: bool,       // 【新增】
    pub gripper_driver_error: bool,       // 【新增】
    pub gripper_enabled: bool,            // 【新增】（注意：反向逻辑）
    pub gripper_homed: bool,              // 【新增】

    // === 连接状态 ===
    pub connection_status: bool,          // 连接状态（是否收到数据）
}
```

### 5.3. ConfigState 修复

```rust
pub struct ConfigState {
    // === 固件版本 ===
    pub firmware_version: Option<String>, // 【修改】可选，无法从协议获取

    // === 关节限制 ===
    pub joint_limits_max: [f64; 6],      // 关节角度上限（弧度）
    pub joint_limits_min: [f64; 6],      // 关节角度下限（弧度）
    pub joint_max_velocity: [f64; 6],    // 【新增】各关节最大速度（rad/s）
    pub max_acc_limits: [f64; 6],        // 【修复】各关节最大加速度（rad/s²）

    // === 末端限制 ===
    pub max_end_linear_velocity: f64,    // 【新增】末端最大线速度（m/s）
    pub max_end_angular_velocity: f64,   // 【新增】末端最大角速度（rad/s）
    pub max_end_linear_accel: f64,       // 【新增】末端最大线加速度（m/s²）
    pub max_end_angular_accel: f64,      // 【新增】末端最大角加速度（rad/s²）

    // === 夹爪/示教器参数（可选） ===
    pub teach_travel_coeff: Option<u8>,            // 【可选】
    pub gripper_max_travel_limit: Option<u8>,      // 【可选】
    pub teach_friction_coeff: Option<u8>,          // 【可选】
}
```

## 6. 总结

### 6.1. 主要问题

1. **MotionState**：
   - ❌ 缺少 `move_mode`, `teach_status`, `motion_status`, `trajectory_point_index` 等控制状态字段
   - ❌ 缺少故障码字段（`fault_angle_limit`, `fault_comm_error`）
   - ⚠️ `joint_torque` 应改为 `joint_current`（单位不匹配）
   - ⚠️ `gripper_pos` 应改为 `gripper_travel`（语义不匹配）
   - ⚠️ `end_pose` 需要单位转换（mm → m）

2. **DiagnosticState**：
   - ❌ 缺少驱动器状态字段（8 个状态位，6 个关节）
   - ❌ 缺少夹爪状态字段（8 个状态位）
   - ⚠️ `bus_voltage` 和 `bus_current` 应改为数组（按关节存储）
   - ⚠️ `protection_level` 应改为 `protection_levels`（数组）

3. **ConfigState**：
   - ❌ 缺少关节最大速度限制
   - ❌ 缺少末端速度/加速度参数
   - ⚠️ `max_acc_limit` 应改为 `max_acc_limits`（数组）
   - ⚠️ `firmware_version` 无法从协议获取

### 6.2. 修复优先级

**高优先级（影响功能）**：
1. MotionState：添加 `move_mode`, `teach_status`, `motion_status`
2. MotionState：修复 `joint_torque` → `joint_current`
3. MotionState：修复 `gripper_pos` → `gripper_travel`
4. MotionState：修复 `end_pose` 单位转换（mm → m）
5. DiagnosticState：修复 `bus_voltage` → `joint_voltage: [f32; 6]`
6. DiagnosticState：修复 `bus_current` → `joint_bus_current: [f32; 6]`
7. DiagnosticState：修复 `protection_level` → `protection_levels: [u8; 6]`

**中优先级（增强功能）**：
1. MotionState：添加故障码字段
2. DiagnosticState：添加驱动器状态字段
3. ConfigState：添加关节最大速度限制
4. ConfigState：添加末端速度/加速度参数

**低优先级（可选功能）**：
1. MotionState：添加末端速度/加速度（每个关节）
2. ConfigState：添加夹爪/示教器参数

---

**文档版本**: v1.0
**最后更新**: 2024-12
**分析人**: Driver 模块设计团队

