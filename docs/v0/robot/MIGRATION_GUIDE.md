# 迁移指南：从旧 API 迁移到新 API

> **版本**：v0.0.1
> **最后更新**：2024年

本文档帮助您将代码从旧的废弃 API 迁移到新的、更细粒度的状态 API。

---

## 📋 目录

- [概述](#概述)
- [迁移步骤](#迁移步骤)
- [详细迁移示例](#详细迁移示例)
- [常见问题](#常见问题)

---

## 概述

### 为什么需要迁移？

新的 API 设计基于以下原则：

1. **数据源分离**：每个状态结构只包含来自单一数据源的信息，避免语义混淆
2. **时间戳准确性**：每个状态都有独立的硬件时间戳和系统时间戳
3. **性能优化**：使用位掩码替代布尔数组，减少内存占用
4. **并发优化**：40Hz 诊断数据使用 `ArcSwap`（Wait-Free），提高并发性能
5. **数据完整性**：提供帧有效性检查和辅助方法

### 废弃时间表

- **v0.0.1**：旧 API 标记为 `#[deprecated]`，但仍可用
- **v0.1.0**：旧 API 将被移除（计划）

---

## 迁移步骤

### 步骤 1：识别使用的旧 API

检查您的代码中是否使用了以下废弃 API：

- `get_core_motion()` → 使用 `get_joint_position()` 和 `get_end_pose()`
- `get_control_status()` → 使用 `get_robot_control()` 和 `get_gripper()`
- `get_diagnostic_state()` → 使用 `get_joint_driver_low_speed()` 和 `get_collision_protection()`
- `get_config_state()` → 使用 `get_joint_limit_config()`、`get_joint_accel_config()` 和 `get_end_limit_config()`

### 步骤 2：替换 API 调用

根据下面的详细迁移示例，替换您的代码。

### 步骤 3：更新状态结构访问

新的状态结构字段名称可能不同，需要更新字段访问。

### 步骤 4：测试

运行测试，确保功能正常。

---

## 详细迁移示例

### 1. 从 `CoreMotionState` 迁移

#### 旧代码

```rust
let core = robot.get_core_motion();
let joint_pos = core.joint_pos;
let end_pose = core.end_pose;
let timestamp = core.timestamp_us;
```

#### 新代码（方案1：分别获取）

```rust
let joint_pos_state = robot.get_joint_position();
let end_pose_state = robot.get_end_pose();

let joint_pos = joint_pos_state.joint_pos;
let end_pose = end_pose_state.end_pose;

// 注意：时间戳是独立的
let joint_timestamp = joint_pos_state.hardware_timestamp_us;
let end_timestamp = end_pose_state.hardware_timestamp_us;
```

#### 新代码（方案2：使用快照）

```rust
// 如果需要逻辑原子性，使用快照
let snapshot = robot.capture_motion_snapshot();
let joint_pos = snapshot.joint_position.joint_pos;
let end_pose = snapshot.end_pose.end_pose;

// 注意：快照中的两个状态时间戳可能不同
let joint_timestamp = snapshot.joint_position.hardware_timestamp_us;
let end_timestamp = snapshot.end_pose.hardware_timestamp_us;
```

#### 关键差异

- **时间戳**：新 API 提供独立的硬件时间戳和系统时间戳
- **帧完整性**：新 API 提供 `is_fully_valid()` 和 `missing_frames()` 方法
- **原子性**：`JointPositionState` 和 `EndPoseState` 不是原子更新的，如需逻辑原子性，使用 `capture_motion_snapshot()`

---

### 2. 从 `ControlStatusState` 迁移

#### 旧代码

```rust
let status = robot.get_control_status();
let control_mode = status.control_mode;
let robot_status = status.robot_status;
let gripper_travel = status.gripper_travel;
let fault_angle_limit = status.fault_angle_limit;  // [bool; 6]
```

#### 新代码

```rust
let control = robot.get_robot_control();
let gripper = robot.get_gripper();

let control_mode = control.control_mode;
let robot_status = control.robot_status;
let gripper_travel = gripper.travel;

// 故障码使用位掩码和辅助方法
let fault_angle_limit_j1 = control.is_angle_limit(0);
let fault_angle_limit_j2 = control.is_angle_limit(1);
// ... 或循环检查
for i in 0..6 {
    if control.is_angle_limit(i) {
        println!("Joint {} angle limit reached", i + 1);
    }
}
```

#### 关键差异

- **分离**：控制状态和夹爪状态已分离
- **位掩码**：故障码使用位掩码（`u8`）替代布尔数组（`[bool; 6]`）
- **辅助方法**：使用 `is_angle_limit()` 和 `is_comm_error()` 访问位掩码
- **反馈计数器**：新增 `feedback_counter` 字段，用于检测链路是否卡死

---

### 3. 从 `DiagnosticState` 迁移

#### 旧代码

```rust
let diag = robot.get_diagnostic_state()?;
let motor_temps = diag.motor_temps;
let driver_voltage_low = diag.driver_voltage_low;  // [bool; 6]
```

#### 新代码

```rust
let driver_state = robot.get_joint_driver_low_speed();
let motor_temps = driver_state.motor_temps;

// 电压过低使用位掩码和辅助方法
let driver_voltage_low_j1 = driver_state.is_voltage_low(0);
// ... 或循环检查
for i in 0..6 {
    if driver_state.is_voltage_low(i) {
        println!("Joint {} voltage low", i + 1);
    }
}

// 碰撞保护状态单独获取
if let Ok(protection) = robot.get_collision_protection() {
    println!("Protection levels: {:?}", protection.protection_levels);
}
```

#### 关键差异

- **分离**：诊断状态和碰撞保护状态已分离
- **位掩码**：所有布尔状态字段使用位掩码（`u8`）替代布尔数组
- **辅助方法**：使用 `is_voltage_low()`、`is_motor_over_temp()` 等方法访问位掩码
- **无锁**：`get_joint_driver_low_speed()` 使用 `ArcSwap`（Wait-Free），性能更好
- **完整性检查**：提供 `is_fully_valid()` 和 `missing_joints()` 方法

---

### 4. 从 `ConfigState` 迁移

#### 旧代码

```rust
let config = robot.get_config_state()?;
let joint_limits_max = config.joint_limits_max;
let joint_max_velocity = config.joint_max_velocity;
let max_end_linear_velocity = config.max_end_linear_velocity;
```

#### 新代码

```rust
// 关节限制配置
if let Ok(limits) = robot.get_joint_limit_config() {
    let joint_limits_max = limits.joint_limits_max;
    let joint_max_velocity = limits.joint_max_velocity;

    // 检查完整性
    if limits.is_fully_valid() {
        println!("All joint limits received");
    } else {
        println!("Missing joints: {:?}", limits.missing_joints());
    }
}

// 关节加速度限制配置
if let Ok(accel_limits) = robot.get_joint_accel_config() {
    let max_acc_limits = accel_limits.max_acc_limits;
}

// 末端限制配置
if let Ok(end_limits) = robot.get_end_limit_config() {
    let max_end_linear_velocity = end_limits.max_end_linear_velocity;
    let max_end_angular_velocity = end_limits.max_end_angular_velocity;
    let max_end_linear_accel = end_limits.max_end_linear_accel;
    let max_end_angular_accel = end_limits.max_end_angular_accel;
}
```

#### 关键差异

- **分离**：配置状态已拆分为三个独立的状态
- **完整性检查**：提供 `is_fully_valid()` 和 `missing_joints()` 方法
- **时间戳**：每个状态都有独立的硬件时间戳和系统时间戳

---

## 常见问题

### Q1: 为什么 `JointPositionState` 和 `EndPoseState` 不是原子更新的？

**A**: 这两个状态来自不同的 CAN 帧组（0x2A5-0x2A7 和 0x2A2-0x2A4），它们在硬件上就是异步到达的。强行绑定在一起会掩盖物理事实，并可能导致时间戳混乱。

**解决方案**：如果需要逻辑原子性，使用 `capture_motion_snapshot()`。

### Q2: 如何使用位掩码访问故障码？

**A**: 使用辅助方法，例如：

```rust
let control = robot.get_robot_control();

// 检查单个关节
if control.is_angle_limit(0) {
    println!("Joint 1 angle limit reached");
}

// 检查所有关节
for i in 0..6 {
    if control.is_angle_limit(i) {
        println!("Joint {} angle limit reached", i + 1);
    }
}
```

### Q3: 如何检查数据完整性？

**A**: 使用 `is_fully_valid()` 和 `missing_frames()` / `missing_joints()` 方法：

```rust
let joint_pos = robot.get_joint_position();
if joint_pos.is_fully_valid() {
    println!("All frames received");
} else {
    println!("Missing frames: {:?}", joint_pos.missing_frames());
}

let driver_state = robot.get_joint_driver_low_speed();
if driver_state.is_fully_valid() {
    println!("All joints received");
} else {
    println!("Missing joints: {:?}", driver_state.missing_joints());
}
```

### Q4: 性能有影响吗？

**A**: 新 API 的性能通常更好：

- **无锁读取**：`ArcSwap` 的读取延迟 < 1000ns
- **内存优化**：位掩码替代布尔数组，减少内存占用
- **并发优化**：40Hz 诊断数据使用 `ArcSwap`（Wait-Free），多线程读取不会阻塞

### Q5: 旧 API 什么时候会被移除？

**A**: 计划在 v0.1.0 版本中移除。在此之前，旧 API 会一直可用，但会显示废弃警告。

---

## 完整迁移示例

### 示例：力控循环

#### 旧代码

```rust
loop {
    let core = robot.get_core_motion();
    let joint_dynamic = robot.get_joint_dynamic();

    let joint_pos = core.joint_pos;
    let joint_vel = joint_dynamic.joint_vel;
    let end_pose = core.end_pose;

    // 力控算法
    // ...
}
```

#### 新代码

```rust
loop {
    // 方案1：使用快照（推荐）
    let snapshot = robot.capture_motion_snapshot();
    let joint_pos = snapshot.joint_position.joint_pos;
    let end_pose = snapshot.end_pose.end_pose;

    let joint_dynamic = robot.get_joint_dynamic();
    let joint_vel = joint_dynamic.joint_vel;

    // 力控算法
    // ...
}
```

---

## 总结

迁移到新 API 的主要步骤：

1. ✅ 替换 `get_core_motion()` → `get_joint_position()` 和 `get_end_pose()`
2. ✅ 替换 `get_control_status()` → `get_robot_control()` 和 `get_gripper()`
3. ✅ 替换 `get_diagnostic_state()` → `get_joint_driver_low_speed()` 和 `get_collision_protection()`
4. ✅ 替换 `get_config_state()` → `get_joint_limit_config()`、`get_joint_accel_config()` 和 `get_end_limit_config()`
5. ✅ 更新字段访问（布尔数组 → 位掩码辅助方法）
6. ✅ 添加数据完整性检查（可选但推荐）

---

**最后更新**：2024年
**参考文档**：
- [API 参考文档](API_REFERENCE.md)
- [状态结构重构分析报告](state_structure_refactoring_analysis.md)

