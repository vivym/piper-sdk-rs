# Piper 协议层实现 TODO 列表

本文档基于 `protocol_implementation_plan.md` 的实现方案，采用 **TDD（测试驱动开发）** 范式，确保每个功能在实现前都有测试用例，保证实现的正确性。

## TDD 开发流程

对于每个功能模块，遵循以下流程：

1. **编写测试用例**（Red）：先编写测试，此时测试应该失败
2. **实现最小功能**（Green）：实现最少的代码使测试通过
3. **重构优化**（Refactor）：优化代码结构，保持测试通过
4. **文档更新**：更新相关文档和注释

---

## Phase 1: 核心反馈帧（高频使用）

### 1.1 模块基础结构

#### TODO-1.1.1: 创建协议模块目录结构
- [x] 创建 `src/protocol/` 目录
- [x] 创建 `src/protocol/mod.rs`（模块导出和错误定义）
- [x] 创建 `src/protocol/ids.rs`（CAN ID 常量）
- [x] 创建 `src/protocol/feedback.rs`（反馈帧）
- [x] 创建 `src/protocol/control.rs`（控制帧）
- [x] 创建 `src/protocol/config.rs`（配置帧）
- [x] 在 `src/lib.rs` 中导出 `protocol` 模块

**参考**：`protocol_implementation_plan.md` 第 2 节

**状态**：✅ 已完成

---

#### TODO-1.1.2: 实现错误类型和工具函数
- [x] 实现 `ProtocolError` 枚举（参考 `protocol_implementation_plan.md` 3.1 节）
  - [x] `InvalidLength`
  - [x] `InvalidCanId`
  - [x] `ParseError`
  - [x] `InvalidValue`
- [x] 实现字节序转换工具函数
  - [x] `bytes_to_i32_be`
  - [x] `bytes_to_i16_be`
  - [x] `i32_to_bytes_be`
  - [x] `i16_to_bytes_be`
- [x] **测试**：编写单元测试验证字节序转换正确性
  - [x] 测试 i32 大端字节序转换（正数、负数、roundtrip）
  - [x] 测试 i16 大端字节序转换（正数、负数、roundtrip）

**参考**：`protocol_implementation_plan.md` 3.1 节

**状态**：✅ 已完成（所有测试通过）

---

#### TODO-1.1.3: 实现 CAN ID 定义
- [x] 定义所有 CAN ID 常量（参考 `protocol_implementation_plan.md` 3.2 节）
- [x] 实现 `FrameType` 枚举和 `from_id()` 方法
- [x] **测试**：验证 ID 分类正确性
  - [x] 测试反馈帧 ID 识别（0x2A1~0x2A8, 0x251~0x256, 0x261~0x266, 0x481~0x486）
  - [x] 测试控制帧 ID 识别（0x150~0x15F）
  - [x] 测试配置帧 ID 识别（0x470~0x47E）
  - [x] 测试未知 ID 处理

**参考**：`protocol_implementation_plan.md` 3.2 节

**状态**：✅ 已完成（所有测试通过）

---

### 1.2 机械臂状态反馈 (0x2A1)

#### TODO-1.2.1: 定义枚举类型
- [x] 实现 `ControlMode` 枚举（反馈帧版本，0x00-0x07）
- [x] 实现 `RobotStatus` 枚举（0x00-0x0F）
- [x] 实现 `MoveMode` 枚举（0x00-0x04）
- [x] 实现 `TeachStatus` 枚举（0x00-0x07）
- [x] 实现 `MotionStatus` 枚举（0x00-0x01）
- [x] 实现 `From<u8>` trait 用于所有枚举
- [x] **测试**：验证所有枚举值的转换正确性
  - [x] 测试所有枚举值的 From<u8> 转换
  - [x] 测试无效值的默认处理
  - [x] 测试枚举值与协议文档的一致性

**参考**：`protocol_implementation_plan.md` 3.3.1 节

**状态**：✅ 已完成（所有测试通过）

---

#### TODO-1.2.2: 实现位域结构（使用 bilge）
- [x] 实现 `FaultCodeAngleLimit` 位域结构（Byte 6）
  - [x] 使用 `#[bitsize(8)]` 和 `#[derive(FromBits, DebugBits)]`
  - [x] 定义 6 个 bool 字段 + 2 位保留
- [x] 实现 `FaultCodeCommError` 位域结构（Byte 7）
  - [x] 使用 `#[bitsize(8)]` 和 `#[derive(FromBits, DebugBits)]`
  - [x] 定义 6 个 bool 字段 + 2 位保留
- [x] **测试**：验证位域解析和编码
  - [x] 测试从 u8 解析到位域结构
  - [x] 测试从位域结构编码为 u8
  - [x] 测试所有位的设置和读取
  - [x] 测试 roundtrip（编码-解码循环）
  - [x] 测试保留位处理

**参考**：`protocol_implementation_plan.md` 3.3.1 节

**状态**：✅ 已完成（所有测试通过）

---

#### TODO-1.2.3: 实现 RobotStatusFeedback 结构体
- [x] 定义 `RobotStatusFeedback` 结构体
- [x] 实现 `TryFrom<PiperFrame>` trait
  - [x] 验证 CAN ID（必须是 0x2A1）
  - [x] 验证数据长度（必须是 8）
  - [x] 解析所有字段（包括位域）
- [x] **测试**：编写完整的解析测试
  - [x] 测试正常数据解析
  - [x] 测试错误 CAN ID
  - [x] 测试数据长度不足
  - [x] 测试所有枚举值的解析
  - [x] 测试位域解析（各种组合）
  - [x] 测试所有字段的各种值组合

**参考**：`protocol_implementation_plan.md` 3.3.1 节

**状态**：✅ 已完成（所有测试通过）

---

### 1.3 关节反馈 (0x2A5, 0x2A6, 0x2A7)

#### TODO-1.3.1: 实现 JointFeedback12
- [x] 定义 `JointFeedback12` 结构体
- [x] 实现物理量转换方法
  - [x] `j1()` / `j2()`：转换为度
  - [x] `j1_rad()` / `j2_rad()`：转换为弧度
  - [x] `j1_raw()` / `j2_raw()`：原始值
- [x] 实现 `TryFrom<PiperFrame>` trait
  - [x] 处理大端字节序（i32）
  - [x] 验证 CAN ID（0x2A5）
- [x] **测试**：
  - [x] 测试大端字节序解析
  - [x] 测试物理量转换精度
  - [x] 测试边界值（最大/最小角度）
  - [x] 测试错误处理（无效 ID、长度不足）
  - [x] 测试 roundtrip（编码-解码循环）

**参考**：`protocol_implementation_plan.md` 3.3.3 节

**状态**：✅ 已完成（所有测试通过）

---

#### TODO-1.3.2: 实现 JointFeedback34
- [x] 定义 `JointFeedback34` 结构体
- [x] 实现物理量转换方法
- [x] 实现 `TryFrom<PiperFrame>` trait（CAN ID: 0x2A6）
- [x] **测试**：同 JointFeedback12

**参考**：`protocol_implementation_plan.md` 3.3.3 节

**状态**：✅ 已完成（所有测试通过）

---

#### TODO-1.3.3: 实现 JointFeedback56
- [x] 定义 `JointFeedback56` 结构体
- [x] 实现物理量转换方法
- [x] 实现 `TryFrom<PiperFrame>` trait（CAN ID: 0x2A7）
- [x] **测试**：同 JointFeedback12

**参考**：`protocol_implementation_plan.md` 3.3.3 节

**状态**：✅ 已完成（所有测试通过）

---

### 1.4 末端位姿反馈 (0x2A2, 0x2A3, 0x2A4)

#### TODO-1.4.1: 实现 EndPoseFeedback1 (0x2A2)
- [x] 定义 `EndPoseFeedback1` 结构体（X, Y 坐标）
- [x] 实现物理量转换方法（转换为 mm）
- [x] 实现 `TryFrom<PiperFrame>` trait
- [x] **测试**：
  - [x] 测试坐标解析（大端字节序）
  - [x] 测试单位转换（0.001mm -> mm）
  - [x] 测试错误处理

**参考**：`protocol_implementation_plan.md` 3.3.2 节

**状态**：✅ 已完成（所有测试通过）

---

#### TODO-1.4.2: 实现 EndPoseFeedback2 (0x2A3)
- [x] 定义 `EndPoseFeedback2` 结构体（Z, RX）
- [x] 实现物理量转换方法
- [x] 实现 `TryFrom<PiperFrame>` trait
- [x] **测试**：同 EndPoseFeedback1

**参考**：`protocol_implementation_plan.md` 3.3.2 节

**状态**：✅ 已完成（所有测试通过）

---

#### TODO-1.4.3: 实现 EndPoseFeedback3 (0x2A4)
- [x] 定义 `EndPoseFeedback3` 结构体（RY, RZ）
- [x] 实现物理量转换方法
- [x] 实现 `TryFrom<PiperFrame>` trait
- [x] **测试**：同 EndPoseFeedback1

**参考**：`protocol_implementation_plan.md` 3.3.2 节

**状态**：✅ 已完成（所有测试通过）

---

### 1.5 关节驱动器高速反馈 (0x251~0x256)

#### TODO-1.5.1: 实现 JointDriverHighSpeedFeedback
- [x] 定义 `JointDriverHighSpeedFeedback` 结构体
- [x] 实现物理量转换方法
  - [x] `speed()`：转换为 rad/s
  - [x] `current()`：转换为 A
  - [x] `position()`：转换为 rad
  - [x] `position_deg()`：转换为度
- [x] 实现 `TryFrom<PiperFrame>` trait
  - [x] 从 CAN ID 推导关节索引（0x251 -> 1, 0x252 -> 2, ...）
  - [x] 处理 i16（速度）和 u16（电流）和 i32（位置）的大端字节序
- [x] **测试**：
  - [x] 测试所有 6 个关节的 ID 识别
  - [x] 测试多字节整数解析（i16, u16, i32）
  - [x] 测试物理量转换
  - [x] 测试边界值
  - [x] 测试负速度（反向旋转）
  - [x] 测试错误处理（无效 ID、长度不足）

**参考**：`protocol_implementation_plan.md` 3.3.5 节

**状态**：✅ 已完成（所有测试通过）

---

## Phase 2: 核心控制帧

### 2.1 控制模式指令 (0x151)

#### TODO-2.1.1: 实现 ControlModeCommand 枚举
- [x] 定义 `ControlModeCommand` 枚举（控制指令版本，不包含 0x05, 0x06）
- [x] 实现 `TryFrom<u8>` trait（处理无效值）
- [x] **测试**：
  - [x] 测试有效值转换
  - [x] 测试无效值（0x05, 0x06）返回错误

**参考**：`protocol_implementation_plan.md` 3.3.1 节（控制指令版本）

**状态**：✅ 已完成（所有测试通过）

---

#### TODO-2.1.2: 实现 MitMode 和 InstallPosition 枚举
- [x] 定义 `MitMode` 枚举（0x00, 0xAD）
- [x] 定义 `InstallPosition` 枚举（0x00-0x03）
- [x] 实现 `TryFrom<u8>` trait
- [x] **测试**：验证所有枚举值转换
  - [x] 测试所有有效值
  - [x] 测试无效值错误处理

**参考**：`protocol_implementation_plan.md` 3.4.2 节

**状态**：✅ 已完成（所有测试通过）

---

#### TODO-2.1.3: 实现 ControlModeCommand 结构体
- [x] 定义 `ControlModeCommandFrame` 结构体
- [x] 实现 `mode_switch()` 方法（模式切换，其他字段为 0）
- [x] 实现 `new()` 方法（完整参数）
- [x] 实现 `to_frame()` 方法
- [x] **测试**：
  - [x] 测试模式切换指令编码
  - [x] 测试完整控制指令编码
  - [x] 测试轨迹终止标志（255）

**参考**：`protocol_implementation_plan.md` 3.4.2 节

**状态**：✅ 已完成（所有测试通过）

---

### 2.2 关节控制指令 (0x155, 0x156, 0x157)

#### TODO-2.2.1: 实现 JointControl12
- [x] 定义 `JointControl12` 结构体
- [x] 实现 `new()` 方法（从物理量创建）
- [x] 实现 `to_frame()` 方法
  - [x] 处理大端字节序编码
  - [x] 验证 CAN ID（0x155）
- [x] **测试**：
  - [x] 测试从物理量创建
  - [x] 测试编码为大端字节序
  - [x] 测试编码-解码循环（验证字节值）
  - [x] 测试精度转换

**参考**：`protocol_implementation_plan.md` 3.4.3 节

**状态**：✅ 已完成（所有测试通过）

---

#### TODO-2.2.2: 实现 JointControl34
- [x] 定义 `JointControl34` 结构体
- [x] 实现 `new()` 和 `to_frame()` 方法（CAN ID: 0x156）
- [x] **测试**：同 JointControl12

**参考**：`protocol_implementation_plan.md` 3.4.3 节

**状态**：✅ 已完成（所有测试通过）

---

#### TODO-2.2.3: 实现 JointControl56
- [x] 定义 `JointControl56` 结构体
- [x] 实现 `new()` 和 `to_frame()` 方法（CAN ID: 0x157）
- [x] **测试**：同 JointControl12

**参考**：`protocol_implementation_plan.md` 3.4.3 节

**状态**：✅ 已完成（所有测试通过）

---

### 2.3 快速急停/轨迹指令 (0x150)

#### TODO-2.3.1: 实现枚举类型
- [x] 定义 `EmergencyStopAction` 枚举（0x00-0x02）
- [x] 定义 `TrajectoryCommand` 枚举（0x00-0x08）
- [x] 定义 `TeachCommand` 枚举（0x00-0x07）
- [x] **测试**：验证所有枚举值

**参考**：`protocol_implementation_plan.md` 3.4.1 节

**状态**：✅ 已完成（所有测试通过）

---

#### TODO-2.3.2: 实现 EmergencyStopCommand
- [x] 定义 `EmergencyStopCommand` 结构体
- [x] 实现便捷构造方法（`emergency_stop()`, `resume()`, `trajectory_transmit()`）
- [x] 实现 `to_frame()` 方法
  - [x] 处理 Byte 0-3（基本字段）
  - [x] 处理 Byte 4-7（轨迹传输字段，大端字节序）
- [x] **测试**：
  - [x] 测试基本急停指令编码
  - [x] 测试恢复指令编码
  - [x] 测试轨迹传输字段编码
  - [x] 测试大端字节序编码

**参考**：`protocol_implementation_plan.md` 3.4.1 节

**状态**：✅ 已完成（所有测试通过）

---

### 2.4 电机使能指令 (0x471)

#### TODO-2.4.1: 实现 MotorEnableCommand
- [x] 定义 `MotorEnableCommand` 结构体
- [x] 实现便捷构造方法（`enable()`, `disable()`, `enable_all()`, `disable_all()`）
- [x] 实现 `to_frame()` 方法
  - [x] Byte 0: 关节序号（1-6 或 7）
  - [x] Byte 1: 使能/失能（0x02/0x01）
- [x] **测试**：
  - [x] 测试使能编码（0x02）
  - [x] 测试失能编码（0x01）
  - [x] 测试所有关节序号（1-7）
  - [x] 测试全部关节使能/失能

**参考**：`protocol_implementation_plan.md` 3.5.1 节

**状态**：✅ 已完成（所有测试通过）

---

## Phase 3: 完整协议覆盖

### 3.1 夹爪相关

#### TODO-3.1.1: 实现 GripperStatus 位域（反馈）
- [x] 使用 bilge 定义 `GripperStatus` 位域结构
- [x] 注意 Bit 6 的反向逻辑（1使能 0失能）
- [x] **测试**：
  - [x] 测试所有位的解析
  - [x] 测试 Bit 6 的反向逻辑
  - [x] 测试位域编码

**参考**：`protocol_implementation_plan.md` 3.3.4 节

**状态**：✅ 已完成（所有测试通过）

---

#### TODO-3.1.2: 实现 GripperFeedback (0x2A8)
- [x] 定义 `GripperFeedback` 结构体
- [x] 实现物理量转换方法
- [x] 实现 `TryFrom<PiperFrame>` trait
- [x] **测试**：完整的解析测试
  - [x] 测试解析和物理量转换
  - [x] 测试错误处理
  - [x] 测试所有状态位

**参考**：`protocol_implementation_plan.md` 3.3.4 节

**状态**：✅ 已完成（所有测试通过）

---

#### TODO-3.1.3: 实现 GripperControlFlags 位域（控制）
- [x] 使用 bilge 定义 `GripperControlFlags` 位域结构
- [x] **测试**：位域解析和编码

**参考**：`protocol_implementation_plan.md` 3.4.6 节

**状态**：✅ 已完成（所有测试通过）

---

#### TODO-3.1.4: 实现 GripperControlCommand (0x159)
- [x] 定义 `GripperControlCommand` 结构体
- [x] 实现 `new()` 和 `to_frame()` 方法
- [x] 实现便捷方法（`set_zero_point()`, `clear_error()`）
- [x] **测试**：
  - [x] 测试编码
  - [x] 测试零点设置（0xAE）
  - [x] 测试清除错误
  - [x] 测试完全闭合（travel = 0）

**参考**：`protocol_implementation_plan.md` 3.4.6 节

**状态**：✅ 已完成（所有测试通过）

---

### 3.2 末端位姿控制指令 (0x152, 0x153, 0x154)

#### TODO-3.2.1: 实现 EndPoseControl1/2/3
- [x] 定义三个结构体
- [x] 实现 `new()` 和 `to_frame()` 方法
- [x] **测试**：编码和 roundtrip 测试
  - [x] 测试所有三个结构体的编码
  - [x] 测试大端字节序
  - [x] 测试精度转换

**参考**：`protocol_implementation_plan.md` 3.4.4 节

**状态**：✅ 已完成（所有测试通过）

---

### 3.3 圆弧模式坐标序号更新指令 (0x158)

#### TODO-3.3.1: 实现 ArcPointCommand
- [x] 定义 `ArcPointIndex` 枚举（0x00: 无效, 0x01: 起点, 0x02: 中点, 0x03: 终点）
- [x] 定义 `ArcPointCommand` 结构体
- [x] 实现便捷构造方法（`start()`, `middle()`, `end()`）
- [x] 实现 `to_frame()` 方法（只有 Byte 0 有效）
- [x] **测试**：
  - [x] 测试所有点序号编码
  - [x] 测试无效值错误处理
  - [x] 测试其他字节为 0

**参考**：`protocol_implementation_plan.md` 3.4.5 节

**状态**：✅ 已完成（所有测试通过）

---

### 3.4 关节驱动器低速反馈 (0x261~0x266)

#### TODO-3.4.1: 实现 DriverStatus 位域
- [x] 使用 bilge 定义 `DriverStatus` 位域结构
- [x] **测试**：位域解析和编码

**参考**：`protocol_implementation_plan.md` 3.3.6 节

**状态**：✅ 已完成（所有测试通过）

---

#### TODO-3.4.2: 实现 JointDriverLowSpeedFeedback
- [x] 定义 `JointDriverLowSpeedFeedback` 结构体
- [x] 实现物理量转换方法
- [x] 实现 `TryFrom<PiperFrame>` trait
- [x] **测试**：完整的解析测试
  - [x] 测试所有 6 个关节
  - [x] 测试物理量转换
  - [x] 测试错误处理

**参考**：`protocol_implementation_plan.md` 3.3.6 节

**状态**：✅ 已完成（所有测试通过）

---

### 3.5 关节末端速度/加速度反馈 (0x481~0x486)

#### TODO-3.5.1: 实现 JointEndVelocityAccelFeedback
- [x] 定义 `JointEndVelocityAccelFeedback` 结构体
- [x] 实现物理量转换方法
- [x] 实现 `TryFrom<PiperFrame>` trait
- [x] **测试**：完整的解析测试
  - [x] 测试所有 6 个关节
  - [x] 测试物理量转换
  - [x] 测试负值处理
  - [x] 测试错误处理

**参考**：`protocol_implementation_plan.md` 3.3.7 节

**状态**：✅ 已完成（所有测试通过）

---

### 3.6 配置指令

#### TODO-3.6.1: 实现随动主从模式设置 (0x470)
- [x] 定义 `LinkSetting` 枚举
- [x] 定义 `FeedbackIdOffset` 和 `ControlIdOffset` 枚举
- [x] 定义 `MasterSlaveModeCommand` 结构体
- [x] 实现便捷构造方法（`set_teach_input_arm()`, `set_motion_output_arm()`）
- [x] 实现 `to_frame()` 方法（Len: 4，但 CAN 帧 8 字节）
- [x] **测试**：编码测试

**参考**：`protocol_implementation_plan.md` 3.5.2 节

**状态**：✅ 已完成（所有测试通过）

---

#### TODO-3.6.2: 实现查询电机限制 (0x472) 和反馈 (0x473)
- [x] 定义 `QueryType` 枚举
- [x] 定义 `QueryMotorLimitCommand` 结构体
- [x] 定义 `MotorLimitFeedback` 结构体
- [x] 实现便捷构造方法（`query_angle_and_max_velocity()`, `query_max_acceleration()`）
- [x] 实现物理量转换方法
- [x] 实现 `TryFrom<PiperFrame>` trait（反馈）
- [x] **测试**：编码和解析测试
  - [x] 测试查询指令编码
  - [x] 测试反馈解析和物理量转换
  - [x] 测试错误处理

**参考**：`protocol_implementation_plan.md` 3.5.3 节

**状态**：✅ 已完成（所有测试通过）

---

#### TODO-3.6.3: 实现设置电机限制 (0x474)
- [x] 定义 `SetMotorLimitCommand` 结构体
- [x] 实现 `new()` 方法（从物理量创建，支持 Option 表示无效值）
- [x] 实现 `to_frame()` 方法（处理无效值 0x7FFF）
- [x] **测试**：编码测试
  - [x] 测试完整参数编码
  - [x] 测试无效值编码
  - [x] 测试部分有效值

**参考**：`protocol_implementation_plan.md` 3.5.4 节

**状态**：✅ 已完成（所有测试通过）

---

#### TODO-3.6.4: 实现关节设置指令 (0x475)
- [x] 定义 `JointSettingCommand` 结构体
- [x] 实现便捷构造方法（`set_zero_point()`, `set_acceleration()`, `clear_error()`）
- [x] 实现 `to_frame()` 方法
- [x] 处理特殊值（0xAE）和无效值（0x7FFF）
- [x] **测试**：编码测试
  - [x] 测试设置零点
  - [x] 测试设置加速度参数
  - [x] 测试清除错误
  - [x] 测试全部关节（joint_index = 7）

**参考**：`protocol_implementation_plan.md` 3.5.5 节

**状态**：✅ 已完成（所有测试通过）

---

#### TODO-3.6.5: 实现设置指令应答 (0x476)
- [x] 定义 `TrajectoryPackStatus` 枚举
- [x] 定义 `SettingResponse` 结构体
- [x] 实现 `TryFrom<PiperFrame>` trait
- [x] 实现 `is_trajectory_response()` 和 `is_setting_response()` 方法
- [x] 实现 `trajectory_status()` 方法
- [x] **测试**：
  - [x] 测试设置指令应答解析
  - [x] 测试轨迹传输应答解析
  - [x] 测试应答类型判断
  - [x] 测试零点设置成功应答
  - [x] 测试错误处理

**参考**：`protocol_implementation_plan.md` 3.5.6 节

**状态**：✅ 已完成（所有测试通过）

---

#### TODO-3.6.6: 实现参数查询与设置 (0x477)
- [x] 定义所有相关枚举（`ParameterQueryType`, `ParameterSetType`）
- [x] 定义 `ParameterQuerySetCommand` 结构体
- [x] 实现 `query()` 和 `set()` 构造方法
- [x] 实现 `validate()` 方法（验证互斥性）
- [x] 实现 `to_frame()` 方法（返回 Result，包含验证）
- [x] **测试**：
  - [x] 测试查询指令编码
  - [x] 测试设置指令编码
  - [x] 测试互斥性验证（同时设置查询和设置应该失败）
  - [x] 测试既不查询也不设置的情况

**参考**：`protocol_implementation_plan.md` 3.5.7 节

**状态**：✅ 已完成（所有测试通过）

---

#### TODO-3.6.7: 实现反馈末端速度/加速度参数 (0x478)
- [x] 定义 `EndVelocityAccelFeedback` 结构体
- [x] 实现物理量转换方法
- [x] 实现 `TryFrom<PiperFrame>` trait
- [x] **测试**：解析测试
  - [x] 测试解析和物理量转换
  - [x] 测试错误处理

**参考**：`protocol_implementation_plan.md` 3.5.8 节

**状态**：✅ 已完成（所有测试通过）

---

#### TODO-3.6.8: 实现设置末端速度/加速度参数 (0x479)
- [x] 定义 `SetEndVelocityAccelCommand` 结构体
- [x] 实现 `new()` 方法（从物理量创建，支持 Option 表示无效值）
- [x] 实现 `to_frame()` 方法（处理无效值 0x7FFF）
- [x] **测试**：编码测试
  - [x] 测试完整参数编码
  - [x] 测试无效值编码
  - [x] 测试部分有效值

**参考**：`protocol_implementation_plan.md` 3.5.9 节

**状态**：✅ 已完成（所有测试通过）

---

#### TODO-3.6.9: 实现碰撞防护等级设置和反馈 (0x47A, 0x47B)
- [x] 定义 `CollisionProtectionLevelCommand` 结构体
- [x] 实现便捷构造方法（`new()`, `all_joints()`）
- [x] 实现 `to_frame()` 方法（0x47A）
- [x] 定义 `CollisionProtectionLevelFeedback` 结构体
- [x] 实现 `TryFrom<PiperFrame>` trait（0x47B）
- [x] **测试**：编码和解析测试
  - [x] 测试设置指令编码
  - [x] 测试反馈解析
  - [x] 测试等级0和最大等级8
  - [x] 测试错误处理

**参考**：`protocol_implementation_plan.md` 3.5.10 节

**状态**：✅ 已完成（所有测试通过）

---

#### TODO-3.6.10: 实现反馈电机最大加速度限制 (0x47C)
- [x] 定义 `MotorMaxAccelFeedback` 结构体
- [x] 实现物理量转换方法
- [x] 实现 `TryFrom<PiperFrame>` trait
- [x] **测试**：解析测试
  - [x] 测试所有 6 个关节
  - [x] 测试物理量转换
  - [x] 测试错误处理

**参考**：`protocol_implementation_plan.md` 3.5.11 节

**状态**：✅ 已完成（所有测试通过）

---

#### TODO-3.6.11: 实现夹爪/示教器参数设置和反馈 (0x47D, 0x47E)
- [x] 定义 `GripperTeachParamsCommand` 结构体
- [x] 实现便捷构造方法（`set_teach_travel_coeff()`, `set_gripper_travel_coeff()`, `set_gripper_torque_coeff()`）
- [x] 实现 `to_frame()` 方法（0x47D）
- [x] 定义 `GripperTeachParamsFeedback` 结构体
- [x] 实现 `TryFrom<PiperFrame>` trait（0x47E）
- [x] **测试**：编码和解析测试
  - [x] 测试所有三种设置指令编码
  - [x] 测试反馈解析
  - [x] 测试错误处理

**参考**：`protocol_implementation_plan.md` 3.5.12 节

**状态**：✅ 已完成（所有测试通过）

---

### 3.7 MIT 控制指令 (0x15A~0x15F)

#### TODO-3.7.1: 实现 MitControlCommand（基础版本）
- [x] 定义 `MitControlCommand` 结构体
- [x] 实现 `float_to_uint()` 和 `uint_to_float()` 辅助函数
- [x] 实现 `new()` 方法
- [x] **测试**：
  - [x] 测试转换公式正确性
  - [x] 测试边界值转换
  - [x] 测试往返转换（roundtrip）
  - [x] 测试所有 6 个关节

**参考**：`protocol_implementation_plan.md` 3.4.7 节

**状态**：✅ 已完成（所有测试通过）

---

#### TODO-3.7.2: 实现 MitControlCommand（完整位域打包）
- [x] 实现 `to_frame()` 方法
  - [x] 处理跨字节位域打包（手动位操作）
  - [x] 实现 Pos_ref (16位)、Vel_ref (12位)、Kp (12位)、Kd (12位)、T_ref (8位)、CRC (4位) 的打包
- [x] **测试**：
  - [x] 测试编码功能
  - [x] 测试所有字段的位域打包正确性
  - [x] 测试 CRC 编码
  - [x] 测试所有 6 个关节

**参考**：`protocol_implementation_plan.md` 3.4.7 节

**注意**：由于协议文档中未明确给出各参数的具体范围，当前实现使用了合理的假设范围。
实际使用时可能需要根据硬件规格调整参数范围。

**状态**：✅ 已完成（所有测试通过）

---

## 测试要求

### 单元测试要求

每个结构体和枚举都应该有对应的单元测试：

1. **枚举测试**：
   - 测试所有有效值的转换
   - 测试无效值的错误处理（如果使用 `TryFrom`）

2. **结构体解析测试**：
   - 测试正常数据解析
   - 测试错误 CAN ID
   - 测试数据长度不足
   - 测试边界值

3. **结构体编码测试**：
   - 测试编码为大端字节序
   - 测试所有字段的编码正确性

4. **Roundtrip 测试**：
   - 编码 -> 解码 -> 比较原始值
   - 确保数据完整性

### 集成测试要求

1. **协议完整性测试**：
   - 测试所有协议帧的编码和解析
   - 验证与协议文档的一致性

2. **错误处理测试**：
   - 测试各种错误情况的处理
   - 确保错误信息清晰

3. **性能测试**（可选）：
   - 测试解析性能（1kHz 频率下）
   - 对比 bilge 和手写位操作的性能

---

## 开发检查清单

在提交代码前，确保：

- [x] 所有测试通过（`cargo test`）- ✅ 215 个测试全部通过
- [x] 没有编译警告（`cargo build --all-targets`）- ✅ 已清理未使用的导入
- [ ] 代码格式正确（`cargo fmt --check`）
- [ ] 通过 clippy 检查（`cargo clippy --all-targets -- -D warnings`）
- [x] 文档注释完整（所有公共 API 都有文档）- ✅ 已完成
- [x] 错误处理完善（所有可能的错误都有处理）- ✅ 已完成
- [x] 单元测试覆盖率达到要求（至少 80%）- ✅ 已完成

---

## 进度跟踪

### Phase 1 进度
- [x] 模块基础结构（TODO-1.1.1 ~ 1.1.3）
- [x] 机械臂状态反馈（TODO-1.2.1 ~ 1.2.3）
- [x] 关节反馈（TODO-1.3.1 ~ 1.3.3）
- [x] 末端位姿反馈（TODO-1.4.1 ~ 1.4.3）
- [x] 关节驱动器高速反馈（TODO-1.5.1）

### Phase 2 进度
- [x] 控制模式指令（TODO-2.1.1 ~ 2.1.3）
- [x] 关节控制指令（TODO-2.2.1 ~ 2.2.3）
- [x] 快速急停指令（TODO-2.3.1 ~ 2.3.2）
- [x] 电机使能指令（TODO-2.4.1）

### Phase 3 进度
- [x] 夹爪相关（TODO-3.1.1 ~ 3.1.4）
- [x] 末端位姿控制（TODO-3.2.1）
- [x] 圆弧模式指令（TODO-3.3.1）
- [x] 关节驱动器低速反馈（TODO-3.4.1 ~ 3.4.2）
- [x] 关节末端速度/加速度反馈（TODO-3.5.1）
- [x] 配置指令（TODO-3.6.1 ~ 3.6.11）
- [x] MIT 控制指令（TODO-3.7.1 ~ 3.7.2）
- [x] 辅助功能（可选）
  - [x] 灯光控制指令（0x121）
  - [x] 固件升级模式设定指令（0x422）

---

## 注意事项

1. **严格遵循 TDD**：先写测试，再实现功能
2. **充分测试**：每个功能都要有完整的测试覆盖
3. **错误处理**：所有可能的错误情况都要处理
4. **文档完整**：所有公共 API 都要有文档注释
5. **代码质量**：通过所有 lint 检查，保持代码整洁
6. **参考文档**：实现时随时参考 `protocol_implementation_plan.md` 中的设计细节

---

## 相关文档

- **实现方案**：`docs/v0/protocol_implementation_plan.md`
- **协议文档**：`docs/v0/protocol.md`
- **架构设计**：`docs/v0/TDD.md`

