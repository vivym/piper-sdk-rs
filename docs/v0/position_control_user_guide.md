# 位置控制与 MOVE 模式用户指南

**版本**: v1.0
**创建日期**: 2024
**最后更新**: 2024

---

## 目录

1. [概述](#概述)
2. [快速开始](#快速开始)
3. [控制模式说明](#控制模式说明)
4. [运动类型配置](#运动类型配置)
5. [使用示例](#使用示例)
6. [版本要求](#版本要求)
7. [常见问题](#常见问题)
8. [迁移指南](#迁移指南)

---

## 概述

本指南介绍如何使用 `piper-sdk-rs` 的位置控制和 MOVE 模式功能。这些功能允许您：

- ✅ 使用不同的运动规划方式（关节空间、笛卡尔空间、直线、圆弧）
- ✅ 控制机械臂的末端位姿（位置 + 姿态）
- ✅ 执行直线运动和圆弧运动
- ✅ 配置运动速度和安装位置

### 关键概念

- **控制模式（Control Mode）**：决定机械臂如何响应控制指令
  - `PositionMode`：位置控制模式（支持多种运动规划方式）
  - `MitMode`：MIT 混合控制模式（位置、速度、力矩混合控制）

- **运动类型（Motion Type）**：决定机械臂如何规划运动轨迹
  - `Joint`：关节空间运动
  - `Cartesian`：笛卡尔空间运动（点位模式）
  - `Linear`：直线运动
  - `Circular`：圆弧运动
  - `ContinuousPositionVelocity`：连续位置速度模式

---

## 快速开始

### 1. 基本连接和使能

```rust
use piper_sdk::client::state::*;
use piper_sdk::client::types::*;

// 连接机械臂
let robot = Piper::connect("can0", ConnectionConfig::default())?;

// 使能位置模式（默认使用关节空间运动）
let robot = robot.enable_position_mode(PositionModeConfig::default())?;
```

### 2. 关节位置控制（默认）

```rust
// 使用默认配置（Joint 模式）
let robot = robot.enable_position_mode(PositionModeConfig::default())?;

// 更新单个关节
robot.command_position(Joint::J1, Rad(1.57))?;

// 或批量更新所有关节
let positions = JointArray::from([
    Rad(0.0),   // J1
    Rad(0.5),   // J2
    Rad(1.0),   // J3
    Rad(0.0),   // J4
    Rad(0.0),   // J5
    Rad(0.0),   // J6
]);
robot.Piper.send_position_command_batch(&positions)?;
```

### 3. 末端位姿控制

```rust
use piper_sdk::client::types::{Position3D, EulerAngles};

// 配置为笛卡尔空间模式
let config = PositionModeConfig {
    motion_type: MotionType::Cartesian,
    speed_percent: 50,
    ..Default::default()
};
let robot = robot.enable_position_mode(config)?;

// 发送末端位姿命令
robot.command_cartesian_pose(
    Position3D::new(0.3, 0.0, 0.2),           // x, y, z (米)
    EulerAngles::new(0.0, 180.0, 0.0),        // roll, pitch, yaw (度)
)?;
```

---

## 控制模式说明

### PositionMode（位置控制模式）

位置控制模式支持多种运动规划方式，通过 `motion_type` 配置：

| 运动类型 | MoveMode | 说明 | 使用场景 |
|---------|----------|------|----------|
| `Joint` | MoveJ (0x01) | 关节空间运动，各关节独立运动 | 默认模式，适用于大多数场景 |
| `Cartesian` | MoveP (0x00) | 笛卡尔空间点位运动 | 需要精确控制末端位姿 |
| `Linear` | MoveL (0x02) | 直线运动 | 需要末端沿直线轨迹运动 |
| `Circular` | MoveC (0x03) | 圆弧运动 | 需要末端沿圆弧轨迹运动 |
| `ContinuousPositionVelocity` | MoveCpv (0x05) | 连续位置速度模式 | 轨迹跟踪等高频控制场景 |

### MitMode（MIT 混合控制模式）

MIT 模式支持位置、速度、力矩的混合控制，适用于主从控制等场景。

```rust
// 使能 MIT 模式
let config = MitModeConfig {
    speed_percent: 100,  // 速度百分比（默认 100）
    ..Default::default()
};
let robot = robot.enable_mit_mode(config)?;

// 发送 MIT 控制命令
robot.command_torques(
    Joint::J1,
    Rad(1.0),           // 位置参考
    0.5,                // 速度参考
    10.0,               // 位置增益 (kp)
    2.0,                // 速度增益 (kd)
    NewtonMeter(5.0),   // 力矩参考
)?;
```

---

## 运动类型配置

### 配置运动类型

在使能位置模式时，通过 `PositionModeConfig` 配置运动类型：

```rust
let config = PositionModeConfig {
    motion_type: MotionType::Cartesian,  // 选择运动类型
    speed_percent: 50,                    // 运动速度（0-100）
    install_position: InstallPosition::Horizontal,  // 安装位置
    ..Default::default()
};
let robot = robot.enable_position_mode(config)?;
```

### 运动类型与使用方法的对应关系

| 运动类型 | 应使用的控制方法 |
|---------|----------------|
| `Joint` | `command_position()` 或 `Piper.send_position_command_batch()` |
| `Cartesian` | `command_cartesian_pose()` 或 `Piper.send_cartesian_pose()` |
| `Linear` | `move_linear()` |
| `Circular` | `move_circular()` |
| `ContinuousPositionVelocity` | 待实现 |

**重要**：必须根据配置的 `motion_type` 使用对应的控制方法，否则可能导致运动异常。

---

## 使用示例

### 示例 1：关节位置控制（默认）

```rust
use piper_sdk::client::state::*;
use piper_sdk::client::types::*;

// 连接
let robot = Piper::connect("can0", ConnectionConfig::default())?;

// 使能位置模式（默认 Joint 模式）
let robot = robot.enable_position_mode(PositionModeConfig::default())?;

// 控制关节
robot.command_position(Joint::J1, Rad(1.57))?;
robot.command_position(Joint::J2, Rad(0.5))?;
```

### 示例 2：末端位姿控制（笛卡尔空间）

```rust
use piper_sdk::client::state::*;
use piper_sdk::client::types::*;

// 连接
let robot = Piper::connect("can0", ConnectionConfig::default())?;

// 配置为笛卡尔空间模式
let config = PositionModeConfig {
    motion_type: MotionType::Cartesian,
    speed_percent: 50,
    ..Default::default()
};
let robot = robot.enable_position_mode(config)?;

// 发送末端位姿命令
robot.command_cartesian_pose(
    Position3D::new(0.3, 0.0, 0.2),           // 位置（米）
    EulerAngles::new(0.0, 180.0, 0.0),        // 姿态（度）
)?;
```

### 示例 3：直线运动

```rust
use piper_sdk::client::state::*;
use piper_sdk::client::types::*;

// 连接和配置
let robot = Piper::connect("can0", ConnectionConfig::default())?;
let config = PositionModeConfig {
    motion_type: MotionType::Linear,
    speed_percent: 50,
    ..Default::default()
};
let robot = robot.enable_position_mode(config)?;

// 发送直线运动命令
robot.move_linear(
    Position3D::new(0.3, 0.0, 0.2),           // 目标位置（米）
    EulerAngles::new(0.0, 180.0, 0.0),        // 目标姿态（度）
)?;
```

### 示例 4：圆弧运动

```rust
use piper_sdk::client::state::*;
use piper_sdk::client::types::*;

// 连接和配置
let robot = Piper::connect("can0", ConnectionConfig::default())?;
let config = PositionModeConfig {
    motion_type: MotionType::Circular,
    speed_percent: 50,
    ..Default::default()
};
let robot = robot.enable_position_mode(config)?;

// 发送圆弧运动命令
// 起点：当前末端位姿（自动获取）
// 中间点：via
// 终点：target
robot.move_circular(
    Position3D::new(0.2, 0.1, 0.2),          // via: 中间点位置（米）
    EulerAngles::new(0.0, 90.0, 0.0),        // via: 中间点姿态（度）
    Position3D::new(0.3, 0.0, 0.2),          // target: 终点位置（米）
    EulerAngles::new(0.0, 180.0, 0.0),        // target: 终点姿态（度）
)?;
```

### 示例 5：MIT 混合控制

```rust
use piper_sdk::client::state::*;
use piper_sdk::client::types::*;

// 连接
let robot = Piper::connect("can0", ConnectionConfig::default())?;

// 使能 MIT 模式
let config = MitModeConfig {
    speed_percent: 100,  // 速度百分比
    ..Default::default()
};
let robot = robot.enable_mit_mode(config)?;

// 发送 MIT 控制命令
robot.command_torques(
    Joint::J1,
    Rad(1.0),           // 位置参考
    0.5,                // 速度参考（rad/s）
    10.0,               // 位置增益 (kp)
    2.0,                // 速度增益 (kd)
    NewtonMeter(5.0),   // 力矩参考
)?;
```

### 示例 6：批量发送末端位姿（轨迹跟踪）

```rust
use piper_sdk::client::state::*;
use piper_sdk::client::types::*;

// 连接和配置
let robot = Piper::connect("can0", ConnectionConfig::default())?;
let config = PositionModeConfig {
    motion_type: MotionType::Cartesian,
    speed_percent: 50,
    ..Default::default()
};
let robot = robot.enable_position_mode(config)?;

// 准备轨迹点
let trajectory = vec![
    (Position3D::new(0.3, 0.0, 0.2), EulerAngles::new(0.0, 180.0, 0.0)),
    (Position3D::new(0.3, 0.1, 0.2), EulerAngles::new(0.0, 180.0, 0.0)),
    (Position3D::new(0.3, 0.2, 0.2), EulerAngles::new(0.0, 180.0, 0.0)),
];

// 批量发送
robot.Piper.send_cartesian_pose_batch(&trajectory)?;
```

---

## 版本要求

### 固件版本要求

| 功能 | 最低固件版本 | 说明 |
|------|------------|------|
| 基本位置控制 | V1.0+ | 所有版本支持 |
| MIT 模式（MoveM） | V1.5-2+ | 需要 `MoveMode::MoveM` (0x04) |
| 连续位置速度模式（MoveCpv） | V1.8-1+ | 需要 `MoveMode::MoveCpv` (0x05) |
| 安装位置配置 | V1.5-2+ | 支持水平正装、侧装等 |

### 检查固件版本

```rust
use piper_sdk::client::observer::Observer;

let observer = robot.observer();
let firmware_version = observer.firmware_version();
println!("Firmware version: {:?}", firmware_version);
```

---

## 常见问题

### Q1: 如何选择运动类型？

**A**: 根据您的应用场景选择：

- **关节空间运动（Joint）**：适用于大多数场景，各关节独立运动，末端轨迹不可预测
- **笛卡尔空间运动（Cartesian）**：需要精确控制末端位姿，轨迹由机械臂内部规划
- **直线运动（Linear）**：需要末端沿直线轨迹运动
- **圆弧运动（Circular）**：需要末端沿圆弧轨迹运动

### Q2: 为什么我的末端位姿控制不工作？

**A**: 请检查以下几点：

1. **运动类型配置**：确保使用 `MotionType::Cartesian` 或 `MotionType::Linear`
2. **使用方法匹配**：必须使用 `command_cartesian_pose()` 或 `move_linear()`，而不是 `command_position()`
3. **单位正确**：位置单位是**米（m）**，角度单位是**度（degree）**

### Q3: 圆弧运动需要发送起点吗？

**A**: 不需要。起点由机械臂内部自动记录（当前末端位姿），您只需要发送中间点和终点。

### Q4: MIT 模式的速度参数如何设置？

**A**: 在 `MitModeConfig` 中设置 `speed_percent`（0-100），默认值为 100。**重要**：不应设为 0，否则某些固件版本可能会锁死关节。

### Q5: 如何确保指令顺序正确？

**A**: SDK 内部使用 Frame Package 机制，利用 CAN 总线优先级保证指令顺序。对于圆弧运动，所有相关帧会打包发送，确保顺序正确。

---

## 迁移指南

### 从旧版本迁移

#### 向后兼容性

✅ **好消息**：现有代码无需修改即可继续工作！

默认行为保持不变：
- `PositionModeConfig::default()` 使用 `MotionType::Joint`
- 所有现有的 `command_position()` 调用继续有效

#### 使用新功能

如果您想使用新的运动类型，只需在配置时指定：

```rust
// 旧代码（仍然有效）
let robot = robot.enable_position_mode(PositionModeConfig::default())?;
robot.command_position(Joint::J1, Rad(1.57))?;

// 新功能：使用笛卡尔空间控制
let config = PositionModeConfig {
    motion_type: MotionType::Cartesian,  // 新增配置
    ..Default::default()
};
let robot = robot.enable_position_mode(config)?;
robot.command_cartesian_pose(
    Position3D::new(0.3, 0.0, 0.2),
    EulerAngles::new(0.0, 180.0, 0.0),
)?;
```

#### MIT 模式修正

**重要修正**：`enable_mit_mode` 现在正确使用 `MoveMode::MoveM` (0x04)，而不是之前的 `MoveMode::MoveP`。

如果您之前遇到 MIT 模式无法正常工作的问题，请更新到最新版本。

---

## 相关文档

- [位置控制与 MOVE 模式架构调研报告](position_control_and_move_mode_analysis.md)
- [位置控制与 MOVE 模式执行方案](position_control_move_mode_implementation_plan.md)
- [协议文档](protocol.md)

---

**文档版本**: v1.0
**最后更新**: 2024

