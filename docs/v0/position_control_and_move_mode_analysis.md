# 位置控制与 MOVE 模式架构调研报告

## 1. 概述

本报告深入调研了当前 `piper-sdk-rs` 中关于位置控制命令和接口的实现，重点分析了 **MOVE 模式**（MoveMode）和 **MIT 模式**（MitMode）两个维度的区分方式，以及当前 Type State 架构的合理性。

### 1.1 调研背景

根据 Piper 机械臂的协议（0x151 控制模式指令），存在两个独立的控制维度：

1. **Byte 1: MOVE 模式（MoveMode）**
   - 0x00: MOVE P（点位/末端位姿模式）
   - 0x01: MOVE J（关节模式）
   - 0x02: MOVE L（直线运动模式）
   - 0x03: MOVE C（圆弧运动模式）
   - 0x04: MOVE M（MIT 模式）— 基于 V1.5-2 版本后
   - 0x05: MOVE CPV（连续位置速度模式）— 基于 V1.8-1 版本后

2. **Byte 3: MIT 模式（MitMode / ArmController）**
   - 0x00: 位置速度模式（默认）
   - 0xAD: MIT 模式（用于主从模式）

当前实现只区分了"位置模式"和"MIT 模式"两种 Type State，而没有暴露 MOVE 模式的配置选项。

---

## 2. 当前实现分析

### 2.1 协议层枚举定义

#### MoveMode（运动模式）

定义在 `src/protocol/feedback.rs`：

```rust
/// MOVE 模式
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MoveMode {
    /// MOVE P - 点位模式（末端位姿控制）
    #[default]
    MoveP = 0x00,
    /// MOVE J - 关节模式
    MoveJ = 0x01,
    /// MOVE L - 直线运动
    MoveL = 0x02,
    /// MOVE C - 圆弧运动
    MoveC = 0x03,
    /// MOVE M - MIT 模式（V1.5-2+）
    MoveM = 0x04,
    /// MOVE CPV - 连续位置速度模式（V1.8-1+）
    MoveCpv = 0x05,
}
```

**注意**：当前 Rust SDK 的 `MoveMode` 枚举可能缺少 `MoveCpv` (0x05)，需要补充。

#### MitMode（控制器类型）

定义在 `src/protocol/control.rs`：

```rust
/// MIT 模式
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MitMode {
    /// 位置速度模式（默认）
    #[default]
    PositionVelocity = 0x00,
    /// MIT模式（用于主从模式）
    Mit = 0xAD,
}
```

#### ControlModeCommandFrame（0x151 指令）

定义在 `src/protocol/control.rs`：

```rust
/// 控制模式指令 (0x151)
#[derive(Debug, Clone, Copy, Default)]
pub struct ControlModeCommandFrame {
    pub control_mode: ControlModeCommand, // Byte 0
    pub move_mode: MoveMode,              // Byte 1
    pub speed_percent: u8,                // Byte 2 (0-100)
    pub mit_mode: MitMode,                // Byte 3: 0x00 或 0xAD
    pub trajectory_stay_time: u8,         // Byte 4
    pub install_position: InstallPosition, // Byte 5
}
```

### 2.2 当前 Type State 实现

当前的 Type State 在 `src/client/state/machine.rs` 中定义：

```rust
/// MIT 模式
pub struct MitMode;

/// 位置模式
pub struct PositionMode;

/// 活动状态（带控制模式）
pub struct Active<Mode>(PhantomData<Mode>);
```

#### enable_mit_mode 实现

```rust
pub fn enable_mit_mode(self, config: MitModeConfig) -> Result<Piper<Active<MitMode>>> {
    // ...
    let control_cmd = ControlModeCommandFrame::new(
        ControlModeCommand::CanControl,
        MoveMode::MoveP,           // ❌ 错误：固定使用 MoveP
        0,
        MitMode::Mit,              // ✅ MIT 控制器
        0,
        InstallPosition::Invalid,
    );
    // ...
}
```

**问题分析**：

当前实现存在**严重错误**：`enable_mit_mode` 使用了 `MoveMode::MoveP`，这是不正确的。

根据协议规范和 Python SDK 的实现，启用真正的 MIT 混合控制需要：
- `MoveMode::MoveM` (0x04) + `MitMode::Mit` (0xAD)

`MoveMode::MoveP` 是末端位姿控制模式，与 MIT 混合控制（位置+速度+力矩）是完全不同的概念。

**正确的实现应该是**：

```rust
let control_cmd = ControlModeCommandFrame::new(
    ControlModeCommand::CanControl,
    MoveMode::MoveM,           // ✅ 修正：使用 MoveM (MIT 模式，0x04)
    0,                         // MIT 模式下速度由指令内参数控制
    MitMode::Mit,              // ✅ MIT 控制器 (0xAD)
    0,
    InstallPosition::Invalid,
);
```

**版本要求**：`MoveMode::MoveM` 需要固件版本 >= V1.5-2。

#### enable_position_mode 实现

```rust
pub fn enable_position_mode(self, config: PositionModeConfig) -> Result<Piper<Active<PositionMode>>> {
    // ...
    let control_cmd = ControlModeCommandFrame::new(
        ControlModeCommand::CanControl,
        MoveMode::MoveJ,           // ❌ 固定使用 MoveJ
        config.speed_percent,
        MitMode::PositionVelocity, // ✅ 位置速度控制器
        0,
        config.install_position,
    );
    // ...
}
```

### 2.3 问题总结

| 问题 | 描述 | 影响 | 严重程度 |
|------|------|------|----------|
| **enable_mit_mode 实现错误** | `enable_mit_mode` 错误地使用 `MoveMode::MoveP`，应该使用 `MoveMode::MoveM` (0x04) | 无法启用真正的 MIT 混合控制，功能失效 | 🔴 **严重** |
| **MoveMode 被硬编码** | `enable_position_mode` 固定使用 `MoveJ`，无法配置其他运动模式 | 用户无法使用末端位姿控制（笛卡尔空间）、直线运动、圆弧运动等功能 | 🟡 **中等** |
| **末端位姿控制未暴露** | 虽然 `EndPoseControl1/2/3` 结构体已实现，但没有对应的高层接口 | 用户无法进行笛卡尔空间的点位运动 | 🟡 **中等** |
| **两个维度被混淆** | 将 MoveMode 和 MitMode 合并到单一的 Type State | 限制了 API 的灵活性和表达能力 | 🟡 **中等** |
| **缺少 MoveCpv** | `MoveMode` 枚举缺少 0x05 (MOVE CPV) 模式 | 不支持 V1.8-1 固件新增的连续位置速度模式 | 🟢 **轻微** |

### 2.4 协议层缺失：MoveCpv (0x05)

当前 `src/protocol/feedback.rs` 中的 `MoveMode` 枚举：

```rust
pub enum MoveMode {
    MoveP = 0x00,
    MoveJ = 0x01,
    MoveL = 0x02,
    MoveC = 0x03,
    MoveM = 0x04,
    // ❌ 缺少 MoveCpv = 0x05
}
```

需要补充：

```rust
/// MOVE CPV - 连续位置速度模式（V1.8-1+）
MoveCpv = 0x05,
```

同时需要更新 `From<u8>` 实现：

```rust
impl From<u8> for MoveMode {
    fn from(value: u8) -> Self {
        match value {
            0x00 => MoveMode::MoveP,
            0x01 => MoveMode::MoveJ,
            0x02 => MoveMode::MoveL,
            0x03 => MoveMode::MoveC,
            0x04 => MoveMode::MoveM,
            0x05 => MoveMode::MoveCpv,  // ✅ 新增
            _ => MoveMode::MoveP,
        }
    }
}
```

---

## 3. MOVE 模式与控制指令对应关系

### 3.1 指令对照表

| MoveMode | 协议值 | 对应控制指令 | 说明 | 版本要求 |
|----------|--------|-------------|------|----------|
| **MoveP** | 0x00 | 0x152-0x154 | 末端位姿控制（X,Y,Z,RX,RY,RZ），机械臂规划到目标点 | - |
| **MoveJ** | 0x01 | 0x155-0x157 | 关节角度控制（J1-J6），各关节独立运动到目标角度 | - |
| **MoveL** | 0x02 | 0x152-0x154 | 直线运动，末端沿直线轨迹到达目标位姿 | - |
| **MoveC** | 0x03 | 0x152-0x154 + 0x158 | 圆弧运动，需要指定起点/中点/终点序号 | - |
| **MoveM** | 0x04 | 0x15A-0x15F | MIT 混合控制（位置+速度+力矩） | V1.5-2+ |
| **MoveCpv** | 0x05 | 待确认（可能是 0x152-0x154 或特殊指令） | 连续位置速度模式（Continuous Position Velocity），用于高频轨迹跟踪 | V1.8-1+ |

**注意**：`MoveCpv` 的具体控制指令格式需要进一步调研官方文档或固件实现。根据其名称（连续位置速度），可能使用末端位姿指令（0x152-0x154）但以更高频率发送，或使用特殊的连续控制指令。

### 3.2 当前已实现的控制指令

```text
✅ 0x155-0x157: JointControl12/34/56（关节角度控制）
✅ 0x152-0x154: EndPoseControl1/2/3（末端位姿控制）
✅ 0x15A-0x15F: MitControlCommand（MIT 混合控制）
✅ 0x159: GripperControlCommand（夹爪控制）
❌ 0x158: ArcPointIndexCommand（圆弧模式序号）- 未暴露高层接口
```

---

## 4. Python SDK 参考分析

### 4.1 Python SDK 的模式定义

来自 `piper_control/src/piper_control/piper_interface.py`：

```python
class MoveMode(enum.IntEnum):
    POSITION = 0x00   # 末端位姿控制（MoveP）
    JOINT = 0x01      # 关节模式（MoveJ）
    LINEAR = 0x02     # 直线运动（MoveL）
    CIRCULAR = 0x03   # 圆弧运动（MoveC）
    MIT = 0x04        # MIT 模式（MoveM）— V1.5-2+
    CPV = 0x05        # 连续位置速度模式（MoveCpv）— V1.8-1+

class ArmController(enum.IntEnum):
    POSITION_VELOCITY = 0x00  # 位置速度控制器
    MIT = 0xAD                # MIT 控制器
    INVALID = 0xFF
```

### 4.2 Python SDK 的 API 设计

```python
def standby(
    self,
    move_mode: MoveMode = MoveMode.JOINT,
    arm_controller: ArmController = ArmController.POSITION_VELOCITY,
) -> None:
    """Puts the robot into standby mode."""
    self.piper.MotionCtrl_2(
        ControlMode.STANDBY,
        move_mode,
        0,
        arm_controller,
    )

def set_arm_mode(
    self,
    speed: int = 100,
    move_mode: MoveMode = MoveMode.JOINT,
    ctrl_mode: ControlMode = ControlMode.CAN_COMMAND,
    arm_controller: ArmController = ArmController.POSITION_VELOCITY,
) -> None:
    """Changes the arm motion control mode."""
    self.piper.MotionCtrl_2(
        ctrl_mode,
        move_mode,
        speed,
        arm_controller,
    )
```

### 4.3 Python SDK 使用示例

```python
# 关节模式 + 位置速度控制器
arm.set_arm_mode(
    move_mode=MoveMode.JOINT,
    arm_controller=ArmController.POSITION_VELOCITY,
)
arm.command_joint_positions([0.0, 0.5, -0.5, 0.0, 0.0, 0.0])

# MIT 模式 + MIT 控制器
arm.set_arm_mode(
    move_mode=MoveMode.MIT,
    arm_controller=ArmController.MIT,
)
arm.command_joint_position_mit(motor_idx=1, position=0.5, kp=10, kd=2, torque_ff=0)

# 末端位姿模式（POSITION）- 用于笛卡尔空间控制
arm.set_arm_mode(
    move_mode=MoveMode.POSITION,
    arm_controller=ArmController.POSITION_VELOCITY,
)
# 然后使用 0x152-0x154 指令发送末端位姿
```

---

## 5. 架构改进方案分析

### 5.1 方案一：扩展 PositionModeConfig（最小改动）

**思路**：在现有 `PositionModeConfig` 中增加 `move_mode` 字段，让用户可以配置。

```rust
/// 位置模式配置
#[derive(Debug, Clone)]
pub struct PositionModeConfig {
    pub timeout: Duration,
    pub debounce_threshold: usize,
    pub poll_interval: Duration,
    pub speed_percent: u8,
    pub install_position: InstallPosition,
    /// 新增：运动模式
    pub move_mode: MoveMode,  // 默认 MoveJ
}
```

**优点**：
- 改动最小，向后兼容
- 不改变 Type State 结构

**缺点**：
- `PositionMode` 这个名字变得不准确（实际可能是末端位姿模式）
- 没有编译期保证 MoveMode 与控制指令的匹配
- 用户可能错误配置（如设置 `MoveP` 但发送关节角度指令）

### 5.2 方案二：细化 Type State（类型安全）

**思路**：将 MoveMode 编码到类型系统中，提供编译期保证。

```rust
// 运动模式类型（零大小类型）
pub struct JointMode;       // 关节空间
pub struct CartesianMode;   // 笛卡尔空间（末端位姿）
pub struct LinearMode;      // 直线运动
pub struct CircularMode;    // 圆弧运动

// 控制器类型（与 MitMode 对应）
pub struct PositionVelocityController;  // 位置速度控制
pub struct MitController;               // MIT 混合控制

// 组合状态
pub struct Active<Controller, Motion>(PhantomData<(Controller, Motion)>);

// 示例类型
type JointPositionMode = Piper<Active<PositionVelocityController, JointMode>>;
type CartesianPositionMode = Piper<Active<PositionVelocityController, CartesianMode>>;
type MitJointMode = Piper<Active<MitController, JointMode>>;
```

**优点**：
- 完全类型安全，编译期防止错误配置
- API 语义清晰
- 可以为不同模式提供特定的方法

**缺点**：
- 类型爆炸（Controller × Motion = 2 × 4 = 8 种组合）
- 实现复杂度高
- 向后不兼容

### 5.3 方案三：运行时配置 + 类型辅助（推荐）

**思路**：保持现有的两种 Type State（PositionMode, MitMode），但增加运动模式的运行时配置和辅助方法。

```rust
/// 运动模式配置
#[derive(Debug, Clone, Copy, Default)]
pub enum MotionType {
    /// 关节空间运动（发送 0x155-0x157）
    #[default]
    Joint,
    /// 笛卡尔空间运动（发送 0x152-0x154）
    Cartesian,
    /// 直线运动（发送 0x152-0x154，但轨迹为直线）
    Linear,
    /// 圆弧运动（发送 0x152-0x154 + 0x158）
    Circular,
}

/// 位置模式配置（扩展）
#[derive(Debug, Clone)]
pub struct PositionModeConfig {
    // ... 现有字段 ...

    /// 运动类型
    ///
    /// - `Joint`: 关节空间运动，使用关节角度指令（0x155-0x157）
    /// - `Cartesian`: 笛卡尔空间运动，使用末端位姿指令（0x152-0x154）
    /// - `Linear`: 直线运动，末端沿直线轨迹
    /// - `Circular`: 圆弧运动，需要指定起点/中点/终点
    pub motion_type: MotionType,
}

// Active<PositionMode> 实现
impl Piper<Active<PositionMode>> {
    /// 发送关节角度命令（仅在 MotionType::Joint 时有效）
    pub fn command_joint_positions(&self, positions: &JointArray<Rad>) -> Result<()>;

    /// 发送末端位姿命令（仅在 MotionType::Cartesian/Linear 时有效）
    pub fn command_cartesian_pose(&self, pose: &CartesianPose) -> Result<()>;

    /// 发送直线运动命令
    pub fn move_linear(&self, target: &CartesianPose) -> Result<()>;

    /// 发送圆弧运动命令
    pub fn move_circular(&self, via: &CartesianPose, target: &CartesianPose) -> Result<()>;
}
```

**优点**：
- 向后兼容（默认行为不变）
- 提供了更丰富的控制能力
- 类型系统仍然保证了 MIT/Position 模式的正确性
- 运动类型的错误使用可以通过文档和运行时检查提示

**缺点**：
- 需要运行时检查 MotionType 与调用方法的匹配
- 不如方案二的编译期保证强

---

## 6. 推荐方案

### 6.1 推荐：方案三（运行时配置 + 类型辅助）

**理由**：
1. **平衡了灵活性和类型安全**：保持了 MIT/Position 模式的类型安全区分，同时提供了运动类型的配置能力
2. **向后兼容**：现有代码无需修改
3. **符合实际使用场景**：大多数用户只需要关节控制，高级用户可以配置其他模式
4. **参考 Python SDK 设计**：与官方 Python SDK 的 API 风格一致

### 6.2 实现路线图

#### 阶段零：协议层补全（前置）

**必须先完成**，否则后续功能无法正确工作：

1. 在 `src/protocol/feedback.rs` 中为 `MoveMode` 枚举添加 `MoveCpv = 0x05`
2. 更新 `From<u8> for MoveMode` 实现
3. 添加相关测试
4. **关键修正**：修正 `enable_mit_mode` 中的 `MoveMode::MoveP` → `MoveMode::MoveM`

#### 版本依赖说明

**重要**：新架构对固件版本有硬性要求：

| MoveMode | 最低固件版本 | 说明 |
|----------|-------------|------|
| `MoveP`, `MoveJ`, `MoveL`, `MoveC` | 无要求 | 基础模式，所有版本支持 |
| `MoveM` | V1.5-2+ | MIT 混合控制模式 |
| `MoveCpv` | V1.8-1+ | 连续位置速度模式 |

**建议**：
- 在 `enable_mit_mode` 和 `enable_position_mode` 的文档注释中明确标注版本要求
- 可选：在运行时检测固件版本并给出警告（但一般建议用户直接升级固件）

#### 阶段一：扩展配置（基础）

1. 在 `PositionModeConfig` 中增加 `motion_type: MotionType` 字段
2. 修改 `enable_position_mode()` 使用配置的 `motion_type` 转换为 `MoveMode`
3. 保持默认值为 `Joint`，确保向后兼容

#### 阶段二：增加末端位姿控制接口

1. 在 `Piper` 中增加 `send_cartesian_pose_batch()` 方法
2. 在 `RawCommander` 中实现底层 `send_end_pose_command()` 方法
3. 在 `Piper<Active<PositionMode>>` 中增加 `command_cartesian_pose()` 便捷方法

#### 阶段三：增加高级运动模式（可选）

1. 实现直线运动接口 `move_linear()`
2. 实现圆弧运动接口 `move_circular()`（**高难度**）
   - 需要增加 0x158 指令支持（圆弧序号指令）
   - `MoveC` 模式需要发送两个点（中间点 + 终点）
   - 可能涉及状态机的子状态管理：
     - 等待中间点录入 → 等待终点录入
   - 建议先实现基础版本，后续根据实际需求完善
3. 添加轨迹规划辅助工具

---

## 7. 代码实现示例

### 7.1 MotionType 枚举

```rust
/// 运动类型
///
/// 决定机械臂如何规划运动轨迹。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MotionType {
    /// 关节空间运动
    ///
    /// 各关节独立运动到目标角度，末端轨迹不可预测。
    /// 对应 MoveMode::MoveJ (0x01)，使用指令 0x155-0x157。
    #[default]
    Joint,

    /// 笛卡尔空间运动（点位模式）
    ///
    /// 末端从当前位置运动到目标位姿，轨迹由机械臂内部规划。
    /// 对应 MoveMode::MoveP (0x00)，使用指令 0x152-0x154。
    Cartesian,

    /// 直线运动
    ///
    /// 末端沿直线轨迹运动到目标位姿。
    /// 对应 MoveMode::MoveL (0x02)，使用指令 0x152-0x154。
    Linear,

    /// 圆弧运动
    ///
    /// 末端沿圆弧轨迹运动，需要指定起点、中点、终点。
    /// 对应 MoveMode::MoveC (0x03)，使用指令 0x152-0x154 + 0x158。
    Circular,

    /// 连续位置速度模式（V1.8-1+）
    ///
    /// 连续的位置和速度控制，适用于轨迹跟踪等场景。
    /// 对应 MoveMode::MoveCpv (0x05)。
    ///
    /// **注意**：此模式也属于 `Active<PositionMode>` 状态，不需要单独开辟新的 Type State。
    /// 它本质上是一种特殊的"位置控制"模式，用于高频轨迹跟踪。
    ContinuousPositionVelocity,
}

impl From<MotionType> for MoveMode {
    fn from(motion_type: MotionType) -> Self {
        match motion_type {
            MotionType::Joint => MoveMode::MoveJ,
            MotionType::Cartesian => MoveMode::MoveP,
            MotionType::Linear => MoveMode::MoveL,
            MotionType::Circular => MoveMode::MoveC,
            MotionType::ContinuousPositionVelocity => MoveMode::MoveCpv,
        }
    }
}
```

### 7.2 扩展后的 PositionModeConfig

**术语说明**：

虽然我们保留了 `PositionMode` 这个名字以保持向后兼容性，但在新架构下，它的语义已经扩展：

- **旧定义**：只支持 `MoveJ`（关节位置控制）
- **新定义**：**标准规划模式**（Standard Planning Mode），涵盖所有由机械臂底层规划轨迹的模式：
  - `MoveJ`：关节空间运动
  - `MoveP`：末端位姿控制（点位模式）
  - `MoveL`：直线运动
  - `MoveC`：圆弧运动
  - `MoveCpv`：连续位置速度模式

与 `MitMode`（由上位机高频控制力矩的混合控制模式）形成对立。

```rust
/// 位置模式配置（带 Debounce 参数）
///
/// **术语说明**：虽然名为 "PositionMode"，但实际支持多种运动规划模式
/// （关节空间、笛卡尔空间、直线、圆弧等），与 MIT 混合控制模式相对。
#[derive(Debug, Clone)]
pub struct PositionModeConfig {
    /// 使能超时
    pub timeout: Duration,
    /// Debounce 阈值
    pub debounce_threshold: usize,
    /// 轮询间隔
    pub poll_interval: Duration,
    /// 运动速度百分比（0-100）
    pub speed_percent: u8,
    /// 安装位置
    pub install_position: InstallPosition,
    /// 运动类型（新增）
    ///
    /// 默认为 `Joint`（关节空间运动）。
    ///
    /// **重要**：必须根据 `motion_type` 使用对应的控制方法：
    /// - `Joint`: 使用 `command_joint_positions()` 或 `Piper.send_position_command_batch()`
    /// - `Cartesian`/`Linear`: 使用 `command_cartesian_pose()` 或 `Piper.send_cartesian_pose_batch()`
    /// - `Circular`: 使用 `move_circular()` 方法
    pub motion_type: MotionType,
}

impl Default for PositionModeConfig {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(2),
            debounce_threshold: 3,
            poll_interval: Duration::from_millis(10),
            speed_percent: 50,
            install_position: InstallPosition::Invalid,
            motion_type: MotionType::Joint, // ✅ 默认关节模式，向后兼容
        }
    }
}
```

### 7.3 修改后的 enable_position_mode

```rust
pub fn enable_position_mode(
    self,
    config: PositionModeConfig,
) -> Result<Piper<Active<PositionMode>>> {
    use crate::protocol::control::*;

    // 1. 发送使能指令
    let enable_cmd = MotorEnableCommand::enable_all();
    self.driver.send_reliable(enable_cmd.to_frame())?;

    // 2. 等待使能完成（带 Debounce）
    self.wait_for_enabled(
        config.timeout,
        config.debounce_threshold,
        config.poll_interval,
    )?;

    // 3. 设置位置模式
    // ✅ 修改：使用配置的 motion_type
    let move_mode: MoveMode = config.motion_type.into();

    let control_cmd = ControlModeCommandFrame::new(
        ControlModeCommand::CanControl,
        move_mode,                 // ✅ 使用配置的运动类型
        config.speed_percent,
        MitMode::PositionVelocity,
        0,
        config.install_position,
    );
    self.driver.send_reliable(control_cmd.to_frame())?;

    // 4. 状态转移
    // ...
}
```

### 7.4 修正后的 enable_mit_mode

**关键修正**：必须将 `MoveMode` 从 `MoveP` 改为 `MoveM` (0x04)。

```rust
pub fn enable_mit_mode(self, config: MitModeConfig) -> Result<Piper<Active<MitMode>>> {
    use crate::protocol::control::*;
    use crate::protocol::feedback::MoveMode;

    // 1. 发送使能指令
    let enable_cmd = MotorEnableCommand::enable_all();
    self.driver.send_reliable(enable_cmd.to_frame())?;

    // 2. 等待使能完成（带 Debounce）
    self.wait_for_enabled(
        config.timeout,
        config.debounce_threshold,
        config.poll_interval,
    )?;

    // 3. 设置 MIT 模式
    // ✅ 关键修正：MoveMode 必须设为 MoveM (0x04)
    // 注意：这意味着用户固件必须 >= V1.5-2
    let control_cmd = ControlModeCommandFrame::new(
        ControlModeCommand::CanControl,
        MoveMode::MoveM,           // ✅ 修正：使用 MoveM (MIT 模式，0x04)
        0,                         // MIT 模式下速度通常由指令内参数控制，此处为 0
        MitMode::Mit,              // ✅ MIT 控制器 (0xAD)
        0,
        InstallPosition::Invalid,
    );
    self.driver.send_reliable(control_cmd.to_frame())?;

    // 4. 状态转移
    let driver = self.driver.clone();
    let observer = self.observer.clone();
    std::mem::forget(self);

    Ok(Piper {
        driver,
        observer,
        _state: PhantomData,
    })
}
```

**版本依赖说明**：

- `MoveMode::MoveM` (0x04) 需要固件版本 >= V1.5-2
- 如果用户固件版本过旧，此模式可能不被支持
- 建议在文档中明确标注版本要求，或在运行时检测固件版本

### 7.5 新增末端位姿控制方法

```rust
impl Piper<Active<PositionMode>> {
    /// 发送末端位姿命令（笛卡尔空间控制）
    ///
    /// **前提条件**：必须使用 `MotionType::Cartesian` 或 `MotionType::Linear` 配置。
    ///
    /// # 参数
    ///
    /// - `position`: 末端位置（毫米）
    /// - `orientation`: 末端姿态（欧拉角，度）
    ///
    /// # 示例
    ///
    /// ```rust,ignore
    /// let config = PositionModeConfig {
    ///     motion_type: MotionType::Cartesian,
    ///     ..Default::default()
    /// };
    /// let robot = robot.enable_position_mode(config)?;
    ///
    /// // 发送末端位姿
    /// robot.command_cartesian_pose(
    ///     Position3D::new(300.0, 0.0, 200.0),  // x, y, z (mm)
    ///     EulerAngles::new(0.0, 180.0, 0.0),   // rx, ry, rz (deg)
    /// )?;
    /// ```
    pub fn command_cartesian_pose(
        &self,
        position: Position3D,   // 位置 (mm)
        orientation: EulerAngles, // 姿态 (deg)
    ) -> Result<()> {
        let motion = self.Piper;
        motion.send_cartesian_pose(position, orientation)
    }
}
```

---

## 8. 迁移指南

### 8.1 向后兼容性

现有代码无需修改：

```rust
// 旧代码（仍然有效）
let robot = robot.enable_position_mode(PositionModeConfig::default())?;
robot.command_position(Joint::J1, Rad(1.0))?;
```

### 8.2 使用新功能

```rust
// 使用末端位姿控制
let config = PositionModeConfig {
    motion_type: MotionType::Cartesian,
    speed_percent: 30,  // 30% 速度
    ..Default::default()
};
let robot = robot.enable_position_mode(config)?;

// 发送末端位姿（笛卡尔空间）
robot.command_cartesian_pose(
    Position3D::new(300.0, 0.0, 200.0),
    EulerAngles::new(0.0, 180.0, 0.0),
)?;
```

---

## 9. 结论

### 9.0 关键修正总结

**必须立即修正的问题**（优先级最高）：

1. **`enable_mit_mode` 实现错误**：
   - **当前**：使用 `MoveMode::MoveP` (0x00)
   - **应该**：使用 `MoveMode::MoveM` (0x04)
   - **影响**：无法启用真正的 MIT 混合控制，功能完全失效
   - **修正位置**：`src/client/state/machine.rs` 的 `enable_mit_mode` 方法

2. **协议层缺失 `MoveCpv`**：
   - **当前**：`MoveMode` 枚举缺少 `MoveCpv = 0x05`
   - **影响**：不支持 V1.8-1+ 固件的连续位置速度模式
   - **修正位置**：`src/protocol/feedback.rs` 的 `MoveMode` 枚举

**架构改进**（优先级次之）：

3. 在 `PositionModeConfig` 中增加 `motion_type` 字段，支持配置不同的运动模式
4. 增加末端位姿控制等高层 API

### 9.1 当前实现的问题

1. **🔴 enable_mit_mode 实现错误**：错误地使用 `MoveMode::MoveP`，应该使用 `MoveMode::MoveM` (0x04)，导致无法启用真正的 MIT 混合控制
2. **MoveMode 被硬编码**：用户无法选择末端位姿控制、直线运动等模式
3. **末端位姿控制接口缺失**：虽然协议层已实现，但高层 API 未暴露
4. **Type State 名称不准确**：`PositionMode` 实际只支持关节位置控制（但新架构下会扩展语义）

### 9.2 推荐改进

采用**方案三（运行时配置 + 类型辅助）**：

**优先级 1（必须立即修正）**：
1. **修正 `enable_mit_mode`**：将 `MoveMode::MoveP` 改为 `MoveMode::MoveM` (0x04)
2. **补充 `MoveCpv` 枚举**：在协议层添加 `MoveMode::MoveCpv = 0x05`

**优先级 2（架构改进）**：
3. 在 `PositionModeConfig` 中增加 `motion_type` 字段
4. 修改 `enable_position_mode()` 使用配置的运动类型
5. 增加 `command_cartesian_pose()` 等末端位姿控制方法
6. 保持默认值向后兼容

**优先级 3（可选增强）**：
7. 实现直线运动接口 `move_linear()`
8. 实现圆弧运动接口 `move_circular()`（高难度，需要状态机支持）

### 9.3 是否需要增加 MoveMode 维度到 Type State？

**不建议**。原因：

1. 会导致类型爆炸（8 种组合）
2. 增加实现复杂度
3. 实际使用中，运行时配置 + 文档说明已经足够
4. 与 Python SDK 的设计风格保持一致

---

## 附录 A：协议参考

### 0x151 控制模式指令

| Byte | 字段 | 类型 | 说明 |
|------|------|------|------|
| 0 | control_mode | uint8 | 0x00=待机, 0x01=CAN控制, 0x02=示教, 0x07=离线轨迹 |
| 1 | move_mode | uint8 | 0x00=MoveP, 0x01=MoveJ, 0x02=MoveL, 0x03=MoveC, 0x04=MoveM, 0x05=MoveCpv |
| 2 | speed_percent | uint8 | 0-100 |
| 3 | mit_mode | uint8 | 0x00=位置速度, 0xAD=MIT |
| 4 | trajectory_stay_time | uint8 | 0-254 秒, 255=终止 |
| 5 | install_position | uint8 | 0x00=无效, 0x01=水平, 0x02=侧左, 0x03=侧右 |
| 6-7 | reserved | - | 保留 |

### 相关控制指令

| ID | 名称 | 适用 MoveMode |
|----|----|--------------|
| 0x152-0x154 | 末端位姿控制 | MoveP, MoveL, MoveC, MoveCpv? |
| 0x155-0x157 | 关节角度控制 | MoveJ |
| 0x158 | 圆弧序号指令 | MoveC |
| 0x15A-0x15F | MIT 控制 | MoveM (mit_mode=0xAD) |

**注意**：
- `MoveCpv` 使用的控制指令格式待确认（可能需要进一步调研）
- `MoveM` 必须配合 `mit_mode=0xAD` 使用，否则无效

---

## 附录 B：Python SDK 参考

### MoveMode 枚举对照

| Python SDK | Rust SDK | 协议值 | 版本要求 |
|------------|----------|--------|----------|
| `MoveMode.POSITION` | `MoveMode::MoveP` | 0x00 | - |
| `MoveMode.JOINT` | `MoveMode::MoveJ` | 0x01 | - |
| `MoveMode.LINEAR` | `MoveMode::MoveL` | 0x02 | - |
| `MoveMode.CIRCULAR` | `MoveMode::MoveC` | 0x03 | - |
| `MoveMode.MIT` | `MoveMode::MoveM` | 0x04 | V1.5-2+ |
| `MoveMode.CPV` | `MoveMode::MoveCpv` ⚠️ | 0x05 | V1.8-1+ |

⚠️ **注意**：当前 Rust SDK 的 `MoveMode` 枚举缺少 `MoveCpv` (0x05)，需要补充。

### ArmController 枚举对照

| Python SDK | Rust SDK | 协议值 |
|------------|----------|--------|
| `ArmController.POSITION_VELOCITY` | `MitMode::PositionVelocity` | 0x00 |
| `ArmController.MIT` | `MitMode::Mit` | 0xAD |

