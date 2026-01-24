# piper_control BuiltinJointPositionController 实现分析报告

## 问题背景

在使用 Rust SDK 的位置控制示例时，发现部分关节（J2, J3, J5）的位置显示为 0.0，与实际目标位置差距很大。而使用 `piper_control` 库的 Python 脚本可以正常工作。本报告深入分析了 `piper_control` 库中 `BuiltinJointPositionController` 的实现，并与 Rust SDK 的实现进行对比。

## 1. piper_control 库架构分析

### 1.1 库结构

`piper_control` 是一个高级控制库，位于底层 `piper_sdk` 之上，提供更友好的接口：

```
piper_control (高级接口)
    ↓
piper_interface (中间层，封装 piper_sdk)
    ↓
piper_sdk (底层 SDK，直接操作 CAN 协议)
```

### 1.2 BuiltinJointPositionController 实现

**位置**：`tmp/piper_control/src/piper_control/piper_control.py` 第 178-212 行

```python
class BuiltinJointPositionController(JointPositionController):
  """Joint position controller that uses the inbuilt position commands."""

  def __init__(
      self,
      piper: pi.PiperInterface,
      rest_position: (
          Sequence[float] | None
      ) = ArmOrientations.upright.rest_position,
  ):
    super().__init__(piper)
    self._rest_position = rest_position

  def start(self) -> None:
    self.piper.set_arm_mode(
        arm_controller=pi.ArmController.POSITION_VELOCITY,
        move_mode=pi.MoveMode.JOINT,
    )

  def stop(self) -> None:
    if self._rest_position:
      self.move_to_position(self._rest_position, timeout=3.0)

  def command_joints(self, target: Sequence[float]) -> None:
    self.piper.command_joint_positions(target)
```

**关键点**：
1. `start()` 方法设置模式：`POSITION_VELOCITY` 控制器 + `JOINT` 移动模式
2. `command_joints()` 直接调用 `piper.command_joint_positions(target)`

## 2. piper_interface 层实现分析

### 2.1 set_arm_mode 方法

**位置**：`tmp/piper_control/src/piper_control/piper_interface.py` 第 691-719 行

```python
def set_arm_mode(
    self,
    speed: int = 100,
    move_mode: MoveMode = MoveMode.JOINT,
    ctrl_mode: ControlMode = ControlMode.CAN_COMMAND,
    arm_controller: ArmController = ArmController.POSITION_VELOCITY,
) -> None:
    self.piper.MotionCtrl_2(
        ctrl_mode,  # type: ignore
        move_mode,
        speed,
        arm_controller,
    )
```

**关键参数**：
- `ctrl_mode`: `ControlMode.CAN_COMMAND` (0x01)
- `move_mode`: `MoveMode.JOINT` (0x01) ✅ **关键：使用 JOINT 模式**
- `speed`: `100` (100% 速度) ✅ **关键：默认 100% 速度**
- `arm_controller`: `ArmController.POSITION_VELOCITY` (0x00)

### 2.2 command_joint_positions 方法

**位置**：`tmp/piper_control/src/piper_control/piper_interface.py` 第 721-745 行

```python
def command_joint_positions(self, positions: Sequence[float]) -> None:
    """
    Sets the joint positions using JointCtrl.

    Note: The robot should be using POSITION_VELOCITY controller and JOINT move
    mode for this to work. Use the set_arm_mode() function for this.

    Args:
      positions (Sequence[float]): A list of joint angles in radians.
    """

    joint_angles = []
    joint_limits = get_joint_limits(self._piper_arm_type)

    for i, pos in enumerate(positions):
      min_rad, max_rad = (
          joint_limits["min"][i],
          joint_limits["max"][i],
      )
      clipped_pos = min(max(pos, min_rad), max_rad)
      pos_deg = clipped_pos * RAD_TO_DEG
      joint_angle = round(pos_deg * 1e3)  # Convert to millidegrees
      joint_angles.append(joint_angle)

    self.piper.JointCtrl(*joint_angles)  # pylint: disable=no-value-for-parameter
```

**关键处理**：
1. **限位裁剪**：每个关节位置都会被裁剪到限位范围内
2. **单位转换**：弧度 → 度 → 毫度（0.001°）
3. **一次性发送**：调用 `JointCtrl(*joint_angles)` 一次性发送所有 6 个关节

## 3. 底层 piper_sdk JointCtrl 实现

### 3.1 JointCtrl 方法签名

**位置**：`tmp/piper_sdk/piper_sdk/interface/piper_interface_v2.py` 第 2716-2777 行

```python
def JointCtrl(self,
              joint_1: int,
              joint_2: int,
              joint_3: int,
              joint_4: int,
              joint_5: int,
              joint_6: int):
    """
    机械臂关节控制, 发送前需要切换机械臂模式为关节控制模式

    CAN ID:
        0x155,0x156,0x157
    """
    joint_1 = self.__CalJointSDKLimit(joint_1, "j1")
    joint_2 = self.__CalJointSDKLimit(joint_2, "j2")
    joint_3 = self.__CalJointSDKLimit(joint_3, "j3")
    joint_4 = self.__CalJointSDKLimit(joint_4, "j4")
    joint_5 = self.__CalJointSDKLimit(joint_5, "j5")
    joint_6 = self.__CalJointSDKLimit(joint_6, "j6")
    self.__JointCtrl_12(joint_1, joint_2)
    self.__JointCtrl_34(joint_3, joint_4)
    self.__JointCtrl_56(joint_5, joint_6)
```

**关键实现**：
1. **一次性接收所有 6 个关节角度**
2. **依次发送 3 个 CAN 帧**：
   - `__JointCtrl_12(joint_1, joint_2)` → CAN ID 0x155
   - `__JointCtrl_34(joint_3, joint_4)` → CAN ID 0x156
   - `__JointCtrl_56(joint_5, joint_6)` → CAN ID 0x157

### 3.2 单个 CAN 帧发送实现

**位置**：`tmp/piper_sdk/piper_sdk/interface/piper_interface_v2.py` 第 2779-2804 行

```python
def __JointCtrl_12(self, joint_1: int, joint_2: int):
    tx_can = Message()
    joint_ctrl = ArmMsgJointCtrl(joint_1=joint_1, joint_2=joint_2)
    msg = PiperMessage(type_=ArmMsgType.PiperMsgJointCtrl_12, arm_joint_ctrl=joint_ctrl)
    self.__parser.EncodeMessage(msg, tx_can)
    feedback = self.__arm_can.SendCanMessage(tx_can.arbitration_id, tx_can.data)
    if feedback is not self.__arm_can.CAN_STATUS.SEND_MESSAGE_SUCCESS:
        self.logger.error("JointCtrl_J12 send failed: SendCanMessage(%s)", feedback)
```

**关键点**：
- 每个 CAN 帧包含**两个关节**的角度
- 三个 CAN 帧必须**依次发送**，确保所有 6 个关节的位置都被正确设置

## 4. Rust SDK 实现对比分析

### 4.1 当前 Rust SDK 实现

**位置**：`src/client/raw_commander.rs` 第 138-156 行

```rust
pub(crate) fn send_position_command(&self, joint: Joint, position: Rad) -> Result<()> {
    let pos_deg = position.to_deg().0;

    let frame = match joint {
        Joint::J1 => JointControl12::new(pos_deg, 0.0).to_frame(),
        Joint::J2 => JointControl12::new(0.0, pos_deg).to_frame(),
        Joint::J3 => JointControl34::new(pos_deg, 0.0).to_frame(),
        Joint::J4 => JointControl34::new(0.0, pos_deg).to_frame(),
        Joint::J5 => JointControl56::new(pos_deg, 0.0).to_frame(),
        Joint::J6 => JointControl56::new(0.0, pos_deg).to_frame(),
    };

    self.driver.send_realtime(frame)?;
    Ok(())
}
```

**位置**：`src/client/motion.rs` 第 175-191 行

```rust
pub fn send_position_command_batch(&self, positions: &JointArray<Rad>) -> Result<()> {
    let raw = RawCommander::new(&self.driver);

    for joint in [
        Joint::J1,
        Joint::J2,
        Joint::J3,
        Joint::J4,
        Joint::J5,
        Joint::J6,
    ] {
        raw.send_position_command(joint, positions[joint])?;
    }
    Ok(())
}
```

### 4.2 问题分析

#### 问题 1：关节覆盖问题 ⚠️ **严重**

**根本原因**：
- 每个 CAN 帧（0x155, 0x156, 0x157）包含**两个关节**的角度
- 当前实现循环发送每个关节，但每次只设置一个关节，另一个关节被设置为 0.0
- 当发送 J1 时：`JointControl12::new(pos_deg, 0.0)` → J2 被设置为 0.0
- 当发送 J2 时：`JointControl12::new(0.0, pos_deg)` → J1 被设置为 0.0

**时序问题**：
```
时间线：
T1: 发送 0x155 (J1=target, J2=0.0)     → 机械臂收到：J1=target, J2=0.0
T2: 发送 0x155 (J1=0.0, J2=target)      → 机械臂收到：J1=0.0, J2=target  ❌ J1 被覆盖！
T3: 发送 0x156 (J3=target, J4=0.0)     → 机械臂收到：J3=target, J4=0.0
T4: 发送 0x156 (J3=0.0, J4=target)     → 机械臂收到：J3=0.0, J4=target  ❌ J3 被覆盖！
T5: 发送 0x157 (J5=target, J6=0.0)     → 机械臂收到：J5=target, J6=0.0
T6: 发送 0x157 (J5=0.0, J6=target)     → 机械臂收到：J5=0.0, J6=target  ❌ J5 被覆盖！
```

**结果**：
- J1, J3, J5 的位置被后续发送的帧覆盖为 0.0
- 只有最后发送的关节（J2, J4, J6）保持正确位置
- 这解释了为什么 J2, J3, J5 显示为 0.0

#### 问题 2：速度百分比设置

**Python SDK**：
- `set_arm_mode(speed=100)` → 默认 100% 速度

**Rust SDK**：
- `PositionModeConfig::default().speed_percent = 50` → 默认 50% 速度
- ✅ 这个差异不是主要问题，但可能影响运动速度

#### 问题 3：限位裁剪

**Python SDK**：
- `command_joint_positions` 中会裁剪每个关节到限位范围

**Rust SDK**：
- 当前实现**没有限位裁剪**
- ⚠️ 如果目标位置超出限位，可能导致机械臂异常

## 5. 正确实现方式

### 5.1 Python SDK 的正确流程

```
command_joint_positions([j1, j2, j3, j4, j5, j6])
    ↓
JointCtrl(j1_millideg, j2_millideg, j3_millideg, j4_millideg, j5_millideg, j6_millideg)
    ↓
__JointCtrl_12(j1_millideg, j2_millideg)      → 0x155
__JointCtrl_34(j3_millideg, j4_millideg)      → 0x156
__JointCtrl_56(j5_millideg, j6_millideg)      → 0x157
```

**关键**：一次性发送所有 6 个关节，每个 CAN 帧包含两个关节的正确值。

### 5.2 Rust SDK 应该的实现

```rust
pub fn send_position_command_batch(&self, positions: &JointArray<Rad>) -> Result<()> {
    let raw = RawCommander::new(&self.driver);

    // ✅ 一次性准备所有关节的角度（度）
    let j1_deg = positions[Joint::J1].to_deg().0;
    let j2_deg = positions[Joint::J2].to_deg().0;
    let j3_deg = positions[Joint::J3].to_deg().0;
    let j4_deg = positions[Joint::J4].to_deg().0;
    let j5_deg = positions[Joint::J5].to_deg().0;
    let j6_deg = positions[Joint::J6].to_deg().0;

    // ✅ 依次发送 3 个 CAN 帧，每个帧包含两个关节
    raw.send_joint_control_12(j1_deg, j2_deg)?;
    raw.send_joint_control_34(j3_deg, j4_deg)?;
    raw.send_joint_control_56(j5_deg, j6_deg)?;

    Ok(())
}
```

## 6. 修复建议

### 6.1 立即修复：修改 send_position_command_batch

**文件**：`src/client/motion.rs`

**修改**：
1. 移除循环发送单个关节的逻辑
2. 改为一次性准备所有关节角度
3. 依次发送 3 个 CAN 帧（0x155, 0x156, 0x157）

### 6.2 增强功能：添加限位裁剪

**文件**：`src/client/motion.rs` 或 `src/client/raw_commander.rs`

**建议**：
- 在发送位置命令前，检查并裁剪关节角度到限位范围
- 可以参考 `piper_interface.py` 中的实现

### 6.3 可选优化：调整默认速度

**文件**：`src/client/state/machine.rs`

**建议**：
- 考虑将默认速度从 50% 提高到 100%，与 Python SDK 保持一致
- 或者提供更清晰的文档说明速度设置的影响

## 7. 验证步骤

修复后，验证以下内容：

1. **位置准确性**：所有 6 个关节都能到达目标位置
2. **无覆盖问题**：发送位置命令后，所有关节都保持正确位置
3. **限位保护**：超出限位的位置会被自动裁剪
4. **运动速度**：调整速度百分比，验证运动速度变化

## 8. 总结

### 根本原因

**关节覆盖问题**：当前实现循环发送单个关节，导致每个 CAN 帧中的另一个关节被设置为 0.0，后发送的帧会覆盖前面发送的关节位置。

### 关键差异对比

| 方面 | Python SDK (piper_control) | Rust SDK (当前) | 影响 |
|------|---------------------------|----------------|------|
| 发送方式 | 一次性发送所有 6 个关节 | 循环发送单个关节 | ⚠️ **严重：关节覆盖** |
| CAN 帧组织 | 3 个帧，每帧 2 个关节 | 6 个帧，每帧 1 个关节 | ⚠️ **严重：数据错误** |
| 限位裁剪 | ✅ 有 | ❌ 无 | ⚠️ 中等：安全性 |
| 默认速度 | 100% | 50% | ✅ 轻微：可配置 |

### 修复优先级

1. **P0（立即修复）**：修改 `send_position_command_batch`，一次性发送所有关节
2. **P1（重要）**：添加限位裁剪功能
3. **P2（可选）**：调整默认速度，与 Python SDK 保持一致

## 相关文件

- `tmp/piper_control/src/piper_control/piper_control.py` - BuiltinJointPositionController 实现
- `tmp/piper_control/src/piper_control/piper_interface.py` - command_joint_positions 实现
- `tmp/piper_sdk/piper_sdk/interface/piper_interface_v2.py` - JointCtrl 底层实现
- `src/client/motion.rs` - Rust SDK 批量发送实现（需要修复）
- `src/client/raw_commander.rs` - Rust SDK 单个关节发送实现（需要修改）

