# 位置控制指令无效问题分析报告

## 问题描述

在执行 `position_control_demo` 示例时，虽然成功使能了机械臂并发送了位置控制指令，但机械臂没有运动。

## 问题分析

### 1. 协议要求回顾

根据协议文档 `docs/v0/protocol.md` 第 55 行：

> **关节模式运动**：发送 `0x155`、`0x156`、`0x157` 指令更新目标关节角度，发送 `0x151` 进入 **MOVE J** 模式并设置运动速度百分比即可。

关键点：
- 必须发送 `0x151` 指令进入 **MOVE J** 模式（关节模式）
- 必须设置**非零**的运动速度百分比（0-100）

### 2. 当前实现问题

#### 2.1 MOVE 模式设置错误

**位置**：`src/client/state/machine.rs` 第 279-287 行

```279:287:src/client/state/machine.rs
        // 3. 设置位置模式
        let control_cmd = ControlModeCommandFrame::new(
            ControlModeCommand::CanControl,
            MoveMode::MoveP,  // ❌ 错误：应该是 MoveJ
            0,                 // ❌ 错误：速度百分比为 0
            MitMode::PositionVelocity, // 位置模式
            0,
            InstallPosition::Invalid,
        );
```

**问题**：
- 当前代码使用 `MoveMode::MoveP`（点位模式，0x00）
- 根据协议，位置控制应该使用 `MoveMode::MoveJ`（关节模式，0x01）

**协议定义**（`docs/v0/protocol.md` 第 271-276 行）：
```
* **Byte 1: MOVE模式 (uint8)**
* 0x00: MOVE P
* 0x01: MOVE J
* 0x02: MOVE L
* 0x03: MOVE C
* 0x04: MOVE M
```

#### 2.2 速度百分比为 0

**问题**：
- 当前代码设置 `speed_percent = 0`
- 速度百分比为 0 时，机械臂不会运动

**协议定义**（`docs/v0/protocol.md` 第 279-280 行）：
```
* **Byte 2: 运动速度百分比 (uint8_t)**
* 0~100
```

### 3. Python SDK 参考实现

查看官方 Python SDK 示例 `tmp/piper_sdk/piper_sdk/demo/V2/piper_ctrl_moveJ.py` 第 37 行：

```python
piper.MotionCtrl_2(0x01, 0x01, 100, 0x00)
```

参数说明：
- `0x01`: 控制模式 = CAN指令控制模式 ✅
- `0x01`: MOVE模式 = **MOVE J**（关节模式）✅
- `100`: 运动速度百分比 = **100%** ✅
- `0x00`: MIT模式 = 位置速度模式 ✅

### 4. 指令 ID 和数据格式验证

#### 4.1 指令 ID 正确性

**位置控制指令 ID**：
- `0x155`: J1, J2 关节控制 ✅
- `0x156`: J3, J4 关节控制 ✅
- `0x157`: J5, J6 关节控制 ✅
- `0x151`: 控制模式指令 ✅

**验证**：`src/protocol/ids.rs` 和 `src/protocol/control.rs` 中的 ID 定义正确。

#### 4.2 数据格式和单位转换

**位置控制指令数据格式**（`src/protocol/control.rs` 第 343-350 行）：

```343:350:src/protocol/control.rs
impl JointControl12 {
    /// 从物理量（度）创建关节控制指令
    pub fn new(j1: f64, j2: f64) -> Self {
        Self {
            j1_deg: (j1 * 1000.0) as i32,
            j2_deg: (j2 * 1000.0) as i32,
        }
    }
```

**单位转换流程**（`src/client/raw_commander.rs` 第 138-149 行）：

```138:149:src/client/raw_commander.rs
    pub(crate) fn send_position_command(&self, joint: Joint, position: Rad) -> Result<()> {
        // ✅ 修正：使用 Rad 类型的 to_deg() 方法，提高可读性
        let pos_deg = position.to_deg().0;

        let frame = match joint {
            Joint::J1 => JointControl12::new(pos_deg, 0.0).to_frame(),
            Joint::J2 => JointControl12::new(0.0, pos_deg).to_frame(),
            Joint::J3 => JointControl34::new(pos_deg, 0.0).to_frame(),
            Joint::J4 => JointControl34::new(0.0, pos_deg).to_frame(),
            Joint::J5 => JointControl56::new(pos_deg, 0.0).to_frame(),
            Joint::J6 => JointControl56::new(0.0, pos_deg).to_frame(),
        };
```

**单位转换验证**：
1. 输入：`Rad`（弧度）✅
2. 转换：`to_deg()` → 度 ✅
3. 编码：`度 * 1000.0` → 0.001° 单位 ✅
4. 协议要求：`int32, 单位 0.001°` ✅

**结论**：单位转换正确。

#### 4.3 字节序验证

**协议要求**（`docs/v0/protocol.md` 第 7 行）：
- 数据格式：Motorola (MSB) 高位在前（大端字节序）

**实现验证**（`src/protocol/control.rs` 第 352-361 行）：

```352:361:src/protocol/control.rs
    /// 转换为 CAN 帧
    pub fn to_frame(self) -> PiperFrame {
        let mut data = [0u8; 8];
        let j1_bytes = i32_to_bytes_be(self.j1_deg);
        let j2_bytes = i32_to_bytes_be(self.j2_deg);
        data[0..4].copy_from_slice(&j1_bytes);
        data[4..8].copy_from_slice(&j2_bytes);

        PiperFrame::new_standard(ID_JOINT_CONTROL_12 as u16, &data)
    }
```

**结论**：使用 `i32_to_bytes_be`（大端字节序）✅ 正确。

## 根本原因总结

### 问题 1：MOVE 模式设置错误

**根本原因**：`enable_position_mode` 函数中使用了错误的 MOVE 模式。

- **当前实现**：`MoveMode::MoveP`（点位模式，0x00）
- **正确实现**：`MoveMode::MoveJ`（关节模式，0x01）

**影响**：
- 机械臂主控收到位置控制指令（0x155-0x157），但处于 MOVE P 模式
- MOVE P 模式期望接收末端位姿指令（0x152-0x154），而不是关节角度指令
- 因此位置控制指令被忽略，机械臂不运动

### 问题 2：速度百分比为 0

**根本原因**：`enable_position_mode` 函数中设置的速度百分比为 0。

- **当前实现**：`speed_percent = 0`
- **正确实现**：`speed_percent = 50` 或更高（建议默认 50）

**影响**：
- 即使 MOVE 模式正确，速度百分比为 0 也会导致机械臂不运动
- 根据协议，速度百分比范围是 0-100，0 表示不运动

## 修复方案

### 修复代码位置

**文件**：`src/client/state/machine.rs`

**函数**：`enable_position_mode`

**修复内容**：

1. 将 `MoveMode::MoveP` 改为 `MoveMode::MoveJ`
2. 将 `speed_percent: 0` 改为 `speed_percent: 50`（或从 `PositionModeConfig` 读取）

### 修复后的代码

```rust
// 3. 设置位置模式
let control_cmd = ControlModeCommandFrame::new(
    ControlModeCommand::CanControl,
    MoveMode::MoveJ,              // ✅ 修复：使用关节模式
    config.speed_percent.unwrap_or(50), // ✅ 修复：设置默认速度百分比
    MitMode::PositionVelocity,
    0,
    InstallPosition::Invalid,
);
```

### 可选：增强 PositionModeConfig

可以在 `PositionModeConfig` 中添加 `speed_percent` 字段，允许用户自定义速度：

```rust
pub struct PositionModeConfig {
    pub timeout: Duration,
    pub debounce_threshold: usize,
    pub poll_interval: Duration,
    pub speed_percent: Option<u8>, // 新增：运动速度百分比（0-100）
}
```

## 验证步骤

修复后，验证以下内容：

1. **使能后状态**：确认机械臂进入 CAN 控制模式
2. **MOVE 模式**：确认反馈帧（0x2A1）的 Byte 2 为 0x01（MOVE J）
3. **位置控制**：发送位置指令后，机械臂应该开始运动
4. **速度控制**：调整 `speed_percent` 值，验证运动速度变化

## 相关文件

- `src/client/state/machine.rs` - 需要修复的主要文件
- `src/client/types.rs` - 可能需要添加 `speed_percent` 配置
- `src/protocol/control.rs` - 控制指令定义（无需修改）
- `src/protocol/feedback.rs` - MoveMode 枚举定义（无需修改）
- `docs/v0/protocol.md` - 协议文档参考

## 总结

位置控制指令无效的根本原因是：

1. **MOVE 模式错误**：使用了 `MoveP` 而不是 `MoveJ`
2. **速度百分比为 0**：导致即使模式正确也不会运动

这两个问题都出现在 `enable_position_mode` 函数中，需要同时修复才能让位置控制正常工作。

