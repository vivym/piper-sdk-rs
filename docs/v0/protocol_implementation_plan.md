# Piper 协议层实现方案报告

## 1. 概述

本文档描述 `src/protocol/` 模块的实现方案，该模块负责将 CAN 帧的原始字节数据解析为类型安全的 Rust 结构体，以及将 Rust 结构体编码为 CAN 帧数据。

### 1.1 设计目标

1. **类型安全**：使用 `bilge` 库进行位级解析，避免手动位操作错误
2. **零成本抽象**：编译期确定的数据布局，运行时无额外开销
3. **易于使用**：提供物理量转换方法（如 `position_deg()`），隐藏原始数据格式
4. **协议完整性**：覆盖 protocol.md 中定义的所有反馈帧、控制帧和配置帧

### 1.2 技术选型

- **`bilge`**：用于位级数据解析和打包
  - 使用 `#[bitsize(N)]` 定义位字段结构
  - 使用 `#[derive(FromBits)]` 或 `#[derive(TryFromBits)]` 支持类型转换
  - 支持嵌套结构、枚举、数组和元组
  - 编译期验证位布局（总位数必须匹配）
  - 零运行时开销（性能等同于手写位操作）
  - 类型安全，避免位操作错误
- **字节序处理**：协议使用 Motorola (MSB) 高位在前，需要手动处理字节序
  - 对于多字节整数（i32, i16, u32, u16），先转换为大端字节序的原始整数
  - 然后使用 bilge 进行位字段解析

---

## 2. 模块结构设计

```
src/protocol/
├── mod.rs          # 模块导出和错误定义
├── ids.rs          # CAN ID 常量定义和枚举
├── feedback.rs     # 反馈帧结构体（0x2A1~0x2A8, 0x251~0x256, 0x261~0x266）
├── control.rs      # 控制帧结构体（0x150~0x15F）
└── config.rs       # 配置帧结构体（0x470~0x47C）
```

---

## 3. 详细设计

### 3.1 `mod.rs` - 模块入口和错误定义

**职责**：
- 导出所有子模块
- 定义协议解析错误类型
- 提供通用的字节序转换工具函数

**错误类型**：
```rust
#[derive(Error, Debug)]
pub enum ProtocolError {
    #[error("Invalid frame length: expected {expected}, got {actual}")]
    InvalidLength { expected: usize, actual: usize },

    #[error("Invalid CAN ID: 0x{id:X}")]
    InvalidCanId { id: u32 },

    #[error("Parse error: {0}")]
    ParseError(String),

    #[error("Invalid value for field {field}: {value}")]
    InvalidValue { field: String, value: u8 },
}
```

**字节序工具**：
- `bytes_to_i32_be(bytes: [u8; 4]) -> i32`：大端字节序转 i32
- `bytes_to_i16_be(bytes: [u8; 2]) -> i16`：大端字节序转 i16
- `i32_to_bytes_be(value: i32) -> [u8; 4]`：i32 转大端字节序
- `i16_to_bytes_be(value: i16) -> [u8; 2]`：i16 转大端字节序

---

### 3.2 `ids.rs` - CAN ID 定义

**职责**：
- 定义所有 CAN ID 常量
- 提供 ID 分类枚举（反馈/控制/配置）
- 提供 ID 到帧类型的映射辅助函数

**设计要点**：
```rust
// 反馈帧 ID 范围
pub const FEEDBACK_BASE_ID: u32 = 0x2A1;
pub const FEEDBACK_END_ID: u32 = 0x2A8;

// 控制帧 ID 范围
pub const CONTROL_BASE_ID: u32 = 0x150;
pub const CONTROL_END_ID: u32 = 0x15F;

// 配置帧 ID 范围
pub const CONFIG_BASE_ID: u32 = 0x470;
pub const CONFIG_END_ID: u32 = 0x47E;  // 注意：包含 0x47D 和 0x47E

// 具体 ID 常量
pub const ID_ROBOT_STATUS: u32 = 0x2A1;
pub const ID_END_POSE_1: u32 = 0x2A2;
// ... 等等

// ID 分类枚举
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameType {
    Feedback,
    Control,
    Config,
    Unknown,
}

impl FrameType {
    pub fn from_id(id: u32) -> Self {
        match id {
            0x2A1..=0x2A8 | 0x251..=0x256 | 0x261..=0x266 | 0x481..=0x486 => FrameType::Feedback,
            0x150..=0x15F => FrameType::Control,
            0x470..=0x47E => FrameType::Config,  // 包含 0x47D 和 0x47E
            _ => FrameType::Unknown,
        }
    }
}
```

---

### 3.3 `feedback.rs` - 反馈帧定义

**职责**：
- 定义所有机械臂反馈帧的结构体
- 提供从 `PiperFrame` 解析的方法
- 提供物理量转换方法（原始值 → 物理单位）

#### 3.3.1 机械臂状态反馈 (0x2A1)

**使用 bilge 的正确方式**：

```rust
use bilge::prelude::*;

// 定义枚举（使用 bilge 的位字段枚举）
#[bitsize(3)]
#[derive(FromBits, Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlMode {
    Standby = 0x00,
    CanControl = 0x01,
    Teach = 0x02,
    Ethernet = 0x03,
    Wifi = 0x04,
    Remote = 0x05,
    LinkTeach = 0x06,
    OfflineTrajectory = 0x07,
}

#[bitsize(4)]
#[derive(FromBits, Debug, Clone, Copy, PartialEq, Eq)]
pub enum RobotStatus {
    Normal = 0x00,
    EmergencyStop = 0x01,
    NoSolution = 0x02,
    Singularity = 0x03,
    AngleLimitExceeded = 0x04,
    JointCommError = 0x05,
    JointBrakeNotOpen = 0x06,
    Collision = 0x07,
    TeachOverspeed = 0x08,
    JointStatusError = 0x09,
    OtherError = 0x0A,
    TeachRecord = 0x0B,
    TeachExecute = 0x0C,
    TeachPause = 0x0D,
    MainControlOverTemp = 0x0E,
    ResistorOverTemp = 0x0F,
}

// 故障码位域（Byte 6: 角度超限位）
#[bitsize(8)]
#[derive(FromBits, DebugBits, Clone, Copy)]
pub struct FaultCodeAngleLimit {
    pub joint1_limit: bool,  // Bit 0
    pub joint2_limit: bool,  // Bit 1
    pub joint3_limit: bool,  // Bit 2
    pub joint4_limit: bool,  // Bit 3
    pub joint5_limit: bool,  // Bit 4
    pub joint6_limit: bool,  // Bit 5
    pub reserved: u2,        // Bit 6-7: 保留
}

// 故障码位域（Byte 7: 通信异常）
#[bitsize(8)]
#[derive(FromBits, DebugBits, Clone, Copy)]
pub struct FaultCodeCommError {
    pub joint1_comm_error: bool,  // Bit 0
    pub joint2_comm_error: bool,  // Bit 1
    pub joint3_comm_error: bool,  // Bit 2
    pub joint4_comm_error: bool,  // Bit 3
    pub joint5_comm_error: bool,  // Bit 4
    pub joint6_comm_error: bool,  // Bit 5
    pub reserved: u2,              // Bit 6-7: 保留
}

// 主结构体：8 字节 = 64 位
// 注意：由于协议中每个字段都是完整的字节（8位），我们可以直接使用 u8
// 但如果需要位级操作，可以使用 bilge
#[derive(Debug, Clone, Copy, Default)]
pub struct RobotStatusFeedback {
    pub control_mode: ControlMode,           // Byte 0 (3 bits, 但实际是完整 u8)
    pub robot_status: RobotStatus,           // Byte 1 (4 bits, 但实际是完整 u8)
    pub move_mode: MoveMode,                 // Byte 2
    pub teach_status: TeachStatus,          // Byte 3
    pub motion_status: MotionStatus,         // Byte 4
    pub trajectory_point_index: u8,          // Byte 5
    pub fault_code_angle_limit: FaultCodeAngleLimit,  // Byte 6 (位域)
    pub fault_code_comm_error: FaultCodeCommError,   // Byte 7 (位域)
}

// 注意：由于协议中大部分字段都是完整字节，我们可以选择：
// 1. 对于完整字节的枚举，直接使用普通枚举 + From<u8>
// 2. 对于位域（如故障码），使用 bilge 的位字段结构
// 3. 对于多字节整数，手动处理字节序后使用 bilge（如果需要位级操作）

// 简化版本（推荐用于完整字节字段）：
// 注意：反馈帧（0x2A1）和控制指令（0x151）的 ControlMode 枚举值不同

// 反馈帧的 ControlMode（完整定义，0x00-0x07）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlMode {
    Standby = 0x00,
    CanControl = 0x01,
    Teach = 0x02,
    Ethernet = 0x03,
    Wifi = 0x04,
    Remote = 0x05,        // 仅反馈帧有，控制指令不支持
    LinkTeach = 0x06,     // 仅反馈帧有，控制指令不支持
    OfflineTrajectory = 0x07,
}

impl From<u8> for ControlMode {
    fn from(value: u8) -> Self {
        match value {
            0x00 => ControlMode::Standby,
            0x01 => ControlMode::CanControl,
            0x02 => ControlMode::Teach,
            0x03 => ControlMode::Ethernet,
            0x04 => ControlMode::Wifi,
            0x05 => ControlMode::Remote,
            0x06 => ControlMode::LinkTeach,
            0x07 => ControlMode::OfflineTrajectory,
            _ => ControlMode::Standby, // 默认值，或使用 TryFrom 处理错误
        }
    }
}

// 控制指令的 ControlMode（部分值，0x151 指令只支持部分模式）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlModeCommand {
    Standby = 0x00,
    CanControl = 0x01,
    Teach = 0x02,
    Ethernet = 0x03,
    Wifi = 0x04,
    // 0x05, 0x06 未定义（控制指令不支持）
    OfflineTrajectory = 0x07,
}

impl TryFrom<u8> for ControlModeCommand {
    type Error = ProtocolError;
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0x00 => Ok(ControlModeCommand::Standby),
            0x01 => Ok(ControlModeCommand::CanControl),
            0x02 => Ok(ControlModeCommand::Teach),
            0x03 => Ok(ControlModeCommand::Ethernet),
            0x04 => Ok(ControlModeCommand::Wifi),
            0x07 => Ok(ControlModeCommand::OfflineTrajectory),
            _ => Err(ProtocolError::InvalidValue {
                field: "ControlModeCommand".to_string(),
                value
            }),
        }
    }
}
```

**最佳实践建议**：
- 对于**完整字节的枚举**（如 ControlMode），使用普通枚举 + `From<u8>` 更简单
- 对于**位域结构**（如故障码），使用 bilge 的 `#[bitsize]` 和位字段类型（`bool`, `u1`, `u2` 等）
- 对于**多字节整数**（i32, i16），先处理字节序，然后可以直接使用或封装在 bilge 结构中

#### 3.3.2 末端位姿反馈 (0x2A2, 0x2A3, 0x2A4)

```rust
// 0x2A2: X, Y 坐标
#[derive(Debug, Clone, Copy, Default)]
pub struct EndPoseFeedback1 {
    pub x_mm: i32,  // 单位：0.001mm，需要转换为 mm
    pub y_mm: i32,
}

impl EndPoseFeedback1 {
    pub fn x(&self) -> f64 {
        self.x_mm as f64 / 1000.0  // 转换为 mm
    }

    pub fn y(&self) -> f64 {
        self.y_mm as f64 / 1000.0
    }
}

// 0x2A3: Z, RX
#[derive(Debug, Clone, Copy, Default)]
pub struct EndPoseFeedback2 {
    pub z_mm: i32,
    pub rx_deg: i32,  // 单位：0.001°
}

// 0x2A4: RY, RZ
#[derive(Debug, Clone, Copy, Default)]
pub struct EndPoseFeedback3 {
    pub ry_deg: i32,
    pub rz_deg: i32,
}
```

#### 3.3.3 关节反馈 (0x2A5, 0x2A6, 0x2A7)

```rust
// 0x2A5: J1, J2
#[derive(Debug, Clone, Copy, Default)]
pub struct JointFeedback12 {
    pub j1_deg: i32,  // 单位：0.001°
    pub j2_deg: i32,
}

impl JointFeedback12 {
    pub fn j1(&self) -> f64 {
        self.j1_deg as f64 / 1000.0  // 转换为度
    }

    pub fn j2(&self) -> f64 {
        self.j2_deg as f64 / 1000.0
    }
}

// 类似地定义 JointFeedback34 和 JointFeedback56
```

#### 3.3.4 夹爪反馈 (0x2A8)

**使用 bilge 定义位域状态**：

```rust
use bilge::prelude::*;

// 夹爪状态位域（Byte 6: 8 位）
#[bitsize(8)]
#[derive(FromBits, DebugBits, Clone, Copy)]
pub struct GripperStatus {
    pub voltage_low: bool,          // Bit 0: 0正常 1过低
    pub motor_over_temp: bool,      // Bit 1: 0正常 1过温
    pub driver_over_current: bool, // Bit 2: 0正常 1过流
    pub driver_over_temp: bool,     // Bit 3: 0正常 1过温
    pub sensor_error: bool,         // Bit 4: 0正常 1异常
    pub driver_error: bool,         // Bit 5: 0正常 1错误
    pub enabled: bool,              // Bit 6: **1使能 0失能**（注意：反向逻辑，与通常相反）
    pub homed: bool,                // Bit 7: 0没有回零 1已经回零
}

// 主结构体（不使用 bilge，因为大部分是多字节整数）
#[derive(Debug, Clone, Copy, Default)]
pub struct GripperFeedback {
    pub travel_mm: i32,        // Byte 0-3: 单位 0.001mm
    pub torque_nm: i16,        // Byte 4-5: 单位 0.001N·m（牛·米）
    pub status: GripperStatus, // Byte 6: 位域
    // Byte 7: 保留
}

impl GripperFeedback {
    pub fn travel(&self) -> f64 {
        self.travel_mm as f64 / 1000.0  // 转换为 mm
    }

    pub fn torque(&self) -> f64 {
        self.torque_nm as f64 / 1000.0  // 转换为 N·m（牛·米）
    }
}

// 解析实现
impl TryFrom<PiperFrame> for GripperFeedback {
    type Error = ProtocolError;

    fn try_from(frame: PiperFrame) -> Result<Self, Self::Error> {
        if frame.id != ID_GRIPPER_FEEDBACK {
            return Err(ProtocolError::InvalidCanId { id: frame.id });
        }
        if frame.len < 7 {
            return Err(ProtocolError::InvalidLength { expected: 7, actual: frame.len as usize });
        }

        // 处理大端字节序
        let travel_bytes = [frame.data[0], frame.data[1], frame.data[2], frame.data[3]];
        let travel_mm = i32::from_be_bytes(travel_bytes);

        let torque_bytes = [frame.data[4], frame.data[5]];
        let torque_nm = i16::from_be_bytes(torque_bytes);

        // 使用 bilge 解析位域
        let status = GripperStatus::from(u8::new(frame.data[6]));

        Ok(Self {
            travel_mm,
            torque_nm,
            status,
        })
    }
}
```

#### 3.3.5 关节驱动器高速反馈 (0x251~0x256)

```rust
#[derive(Debug, Clone, Copy, Default)]
pub struct JointDriverHighSpeedFeedback {
    pub joint_index: u8,        // 从 ID 推导：0x251 -> 1, 0x252 -> 2, ...
    pub speed_rad_s: i16,       // 单位：0.001rad/s
    pub current_a: u16,         // 单位：0.001A
    pub position_rad: i32,      // 单位：rad
}

impl JointDriverHighSpeedFeedback {
    pub fn speed(&self) -> f64 {
        self.speed_rad_s as f64 / 1000.0
    }

    pub fn current(&self) -> f64 {
        self.current_a as f64 / 1000.0
    }

    pub fn position(&self) -> f64 {
        self.position_rad as f64
    }
}
```

#### 3.3.6 关节驱动器低速反馈 (0x261~0x266)

```rust
use bilge::prelude::*;

// 驱动器状态位域（Byte 5: 8 位）
#[bitsize(8)]
#[derive(FromBits, DebugBits, Clone, Copy)]
pub struct DriverStatus {
    pub voltage_low: bool,          // Bit 0: 电源电压是否过低
    pub motor_over_temp: bool,      // Bit 1: 电机是否过温
    pub driver_over_current: bool,  // Bit 2: 驱动器是否过流
    pub driver_over_temp: bool,     // Bit 3: 驱动器是否过温
    pub collision_protection: bool,  // Bit 4: 碰撞保护状态
    pub driver_error: bool,         // Bit 5: 驱动器错误状态
    pub enabled: bool,              // Bit 6: 驱动器使能状态
    pub stall_protection: bool,     // Bit 7: 堵转保护状态
}

#[derive(Debug, Clone, Copy, Default)]
pub struct JointDriverLowSpeedFeedback {
    pub joint_index: u8,
    pub voltage_v: u16,         // Byte 0-1: 单位 0.1V
    pub driver_temp_c: i16,     // Byte 2-3: 单位 1℃
    pub motor_temp_c: i8,       // Byte 4: 单位 1℃
    pub driver_status: DriverStatus, // Byte 5: 位域
    pub bus_current_a: u16,     // Byte 6-7: 单位 0.001A
}
```

#### 3.3.7 关节末端速度/加速度反馈 (0x481~0x486)

```rust
#[derive(Debug, Clone, Copy, Default)]
pub struct JointEndVelocityAccelFeedback {
    pub joint_index: u8,        // 从 ID 推导：0x481 -> 1, 0x482 -> 2, ...
    pub linear_velocity: u16,   // Byte 0-1: 末端线速度，单位 0.001m/s
    pub angular_velocity: u16,  // Byte 2-3: 末端角速度，单位 0.001rad/s
    pub linear_accel: u16,      // Byte 4-5: 末端线加速度，单位 0.001m/s²
    pub angular_accel: u16,     // Byte 6-7: 末端角加速度，单位 0.001rad/s²
}

impl JointEndVelocityAccelFeedback {
    pub fn linear_velocity(&self) -> f64 {
        self.linear_velocity as f64 / 1000.0  // 转换为 m/s
    }

    pub fn angular_velocity(&self) -> f64 {
        self.angular_velocity as f64 / 1000.0  // 转换为 rad/s
    }

    pub fn linear_accel(&self) -> f64 {
        self.linear_accel as f64 / 1000.0  // 转换为 m/s²
    }

    pub fn angular_accel(&self) -> f64 {
        self.angular_accel as f64 / 1000.0  // 转换为 rad/s²
    }
}
```

#### 3.3.8 解析方法

所有反馈帧结构体实现 `From<PiperFrame>` trait：

```rust
impl TryFrom<PiperFrame> for RobotStatusFeedback {
    type Error = ProtocolError;

    fn try_from(frame: PiperFrame) -> Result<Self, Self::Error> {
        if frame.id != ID_ROBOT_STATUS {
            return Err(ProtocolError::InvalidCanId { id: frame.id });
        }
        if frame.len < 8 {
            return Err(ProtocolError::InvalidLength { expected: 8, actual: frame.len as usize });
        }

        Ok(Self {
            control_mode: ControlMode::from(frame.data[0]),
            robot_status: RobotStatus::from(frame.data[1]),
            // ... 解析其他字段
        })
    }
}
```

---

### 3.4 `control.rs` - 控制帧定义

**职责**：
- 定义所有控制指令帧的结构体
- 提供构建控制帧的方法
- 提供转换为 `PiperFrame` 的方法

#### 3.4.1 快速急停/轨迹指令 (0x150)

```rust
#[derive(Debug, Clone, Copy, Default)]
pub struct EmergencyStopCommand {
    pub emergency_stop: EmergencyStopAction,  // Byte 0
    pub trajectory_command: TrajectoryCommand, // Byte 1
    pub teach_command: TeachCommand,          // Byte 2
    pub trajectory_index: u8,                // Byte 3: 轨迹点索引 (0~255)
    // 以下字段用于离线轨迹模式下的轨迹传输，其它模式下全部填充 0x0
    pub name_index: u16,                      // Byte 4-5: 轨迹包名称索引
    pub crc16: u16,                          // Byte 6-7: CRC16 校验
}

impl EmergencyStopCommand {
    pub fn to_frame(&self) -> PiperFrame {
        let mut data = [0u8; 8];
        data[0] = self.emergency_stop as u8;
        data[1] = self.trajectory_command as u8;
        data[2] = self.teach_command as u8;
        data[3] = self.trajectory_index;

        // 大端字节序
        let name_index_bytes = self.name_index.to_be_bytes();
        data[4] = name_index_bytes[0];
        data[5] = name_index_bytes[1];

        let crc_bytes = self.crc16.to_be_bytes();
        data[6] = crc_bytes[0];
        data[7] = crc_bytes[1];

        PiperFrame::new_standard(ID_EMERGENCY_STOP as u16, &data)
    }
}

impl EmergencyStopCommand {
    /// 注意：主控收到轨迹传输后会应答 0x476
    /// - Byte 0 = 0x50（特殊值，表示轨迹传输应答）
    /// - Byte 2 = 轨迹点索引 N（0~255）
    /// - Byte 3 = 轨迹包状态（0xAE: 成功，0xEE: 失败）
    /// - Byte 4-7 = NameIndex 和 CRC16
    /// 未收到应答需要重传
}
```

#### 3.4.2 控制模式指令 (0x151)

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MitMode {
    PositionVelocity = 0x00,  // 位置速度模式（默认）
    Mit = 0xAD,               // MIT模式（用于主从模式）
}

impl TryFrom<u8> for MitMode {
    type Error = ProtocolError;
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0x00 => Ok(MitMode::PositionVelocity),
            0xAD => Ok(MitMode::Mit),
            _ => Err(ProtocolError::InvalidValue {
                field: "MitMode".to_string(),
                value
            }),
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ControlModeCommand {
    pub control_mode: ControlModeCommand,  // Byte 0: 注意使用 ControlModeCommand 而不是 ControlMode
    pub move_mode: MoveMode,               // Byte 1
    pub speed_percent: u8,                 // Byte 2 (0-100)
    pub mit_mode: MitMode,                 // Byte 3: 0x00 或 0xAD
    pub trajectory_stay_time: u8,         // Byte 4: 0~254（单位s），255表示轨迹终止
    pub install_position: InstallPosition, // Byte 5: 安装位置
    // Byte 6-7: 保留
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallPosition {
    Invalid = 0x00,
    Horizontal = 0x01,  // 水平正装
    SideLeft = 0x02,    // 侧装左
    SideRight = 0x03,   // 侧装右
}

impl ControlModeCommand {
    /// 创建模式切换指令（仅切换控制模式，其他字段填充 0x0）
    pub fn mode_switch(control_mode: ControlModeCommand) -> Self {
        Self {
            control_mode,
            move_mode: MoveMode::MoveP,  // 默认值
            speed_percent: 0,
            mit_mode: MitMode::PositionVelocity,
            trajectory_stay_time: 0,
            install_position: InstallPosition::Invalid,
        }
    }

    /// 创建完整的控制指令（包含所有参数）
    pub fn new(
        control_mode: ControlModeCommand,
        move_mode: MoveMode,
        speed_percent: u8,
        mit_mode: MitMode,
        trajectory_stay_time: u8,
        install_position: InstallPosition,
    ) -> Self {
        Self {
            control_mode,
            move_mode,
            speed_percent,
            mit_mode,
            trajectory_stay_time,
            install_position,
        }
    }

    pub fn to_frame(&self) -> PiperFrame {
        let mut data = [0u8; 8];
        data[0] = self.control_mode as u8;
        data[1] = self.move_mode as u8;
        data[2] = self.speed_percent;
        data[3] = self.mit_mode as u8;
        data[4] = self.trajectory_stay_time;
        data[5] = self.install_position as u8;
        // Byte 6-7: 保留，已初始化为 0

        PiperFrame::new_standard(ID_CONTROL_MODE as u16, &data)
    }
}
```

#### 3.4.3 关节控制指令 (0x155, 0x156, 0x157)

```rust
#[derive(Debug, Clone, Copy, Default)]
pub struct JointControl12 {
    pub j1_deg: i32,  // 单位：0.001°
    pub j2_deg: i32,
}

impl JointControl12 {
    pub fn new(j1: f64, j2: f64) -> Self {
        Self {
            j1_deg: (j1 * 1000.0) as i32,
            j2_deg: (j2 * 1000.0) as i32,
        }
    }

    pub fn to_frame(&self) -> PiperFrame {
        let mut data = [0u8; 8];
        let j1_bytes = i32_to_bytes_be(self.j1_deg);
        let j2_bytes = i32_to_bytes_be(self.j2_deg);
        data[0..4].copy_from_slice(&j1_bytes);
        data[4..8].copy_from_slice(&j2_bytes);

        PiperFrame::new_standard(ID_JOINT_CONTROL_12 as u16, &data)
    }
}
```

#### 3.4.4 末端位姿控制指令 (0x152, 0x153, 0x154)

类似关节控制指令，但使用末端坐标（X, Y, Z, RX, RY, RZ）。

```rust
// 0x152: X, Y 坐标
#[derive(Debug, Clone, Copy, Default)]
pub struct EndPoseControl1 {
    pub x_mm: i32,  // Byte 0-3: 单位 0.001mm
    pub y_mm: i32,  // Byte 4-7: 单位 0.001mm
}

// 0x153: Z, RX
#[derive(Debug, Clone, Copy, Default)]
pub struct EndPoseControl2 {
    pub z_mm: i32,   // Byte 0-3: 单位 0.001mm
    pub rx_deg: i32, // Byte 4-7: 单位 0.001°
}

// 0x154: RY, RZ
#[derive(Debug, Clone, Copy, Default)]
pub struct EndPoseControl3 {
    pub ry_deg: i32, // Byte 0-3: 单位 0.001°
    pub rz_deg: i32, // Byte 4-7: 单位 0.001°
}
```

#### 3.4.5 圆弧模式坐标序号更新指令 (0x158)

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArcPointIndex {
    Invalid = 0x00,
    Start = 0x01,    // 起点
    Middle = 0x02,  // 中点
    End = 0x03,      // 终点
}

#[derive(Debug, Clone, Copy)]
pub struct ArcPointCommand {
    pub point_index: ArcPointIndex,
}

impl ArcPointCommand {
    pub fn to_frame(&self) -> PiperFrame {
        let data = [self.point_index as u8, 0, 0, 0, 0, 0, 0, 0];
        PiperFrame::new_standard(ID_ARC_POINT as u16, &data)
    }
}
```

#### 3.4.6 夹爪控制指令 (0x159)

```rust
use bilge::prelude::*;

// 夹爪控制标志位域（Byte 6: 8 位）
#[bitsize(8)]
#[derive(FromBits, DebugBits, Clone, Copy)]
pub struct GripperControlFlags {
    pub enable: bool,        // Bit 0: 置1使能，0失能
    pub clear_error: bool,   // Bit 1: 置1清除错误
    pub reserved: u6,        // Bit 2-7: 保留
}

#[derive(Debug, Clone, Copy, Default)]
pub struct GripperControlCommand {
    pub travel_mm: i32,              // Byte 0-3: 夹爪行程，单位 0.001mm（0值表示完全闭合）
    pub torque_nm: i16,               // Byte 4-5: 夹爪扭矩，单位 0.001N·m
    pub control_flags: GripperControlFlags, // Byte 6: 控制标志位域
    pub zero_setting: u8,             // Byte 7: 零点设置（0x00: 无效，0xAE: 设置当前为零点）
}

impl GripperControlCommand {
    pub fn new(travel_mm: f64, torque_nm: f64, enable: bool) -> Self {
        Self {
            travel_mm: (travel_mm * 1000.0) as i32,
            torque_nm: (torque_nm * 1000.0) as i16,
            control_flags: GripperControlFlags::from(u8::new(
                if enable { 0x01 } else { 0x00 }
            )),
            zero_setting: 0x00,
        }
    }

    pub fn to_frame(&self) -> PiperFrame {
        let mut data = [0u8; 8];

        // 大端字节序
        let travel_bytes = self.travel_mm.to_be_bytes();
        data[0..4].copy_from_slice(&travel_bytes);

        let torque_bytes = self.torque_nm.to_be_bytes();
        data[4..6].copy_from_slice(&torque_bytes);

        data[6] = u8::from(self.control_flags).value();
        data[7] = self.zero_setting;

        PiperFrame::new_standard(ID_GRIPPER_CONTROL as u16, &data)
    }
}
```

#### 3.4.7 MIT 控制指令 (0x15A~0x15F)

#### 3.4.7 MIT 控制指令 (0x15A~0x15F)

**注意**：MIT 控制指令使用了复杂的跨字节位域打包，实现时需要仔细处理。

```rust
#[derive(Debug, Clone, Copy, Default)]
pub struct MitControlCommand {
    pub joint_index: u8,  // 从 ID 推导：0x15A -> 1, 0x15B -> 2, ...
    pub pos_ref: f32,     // 位置参考值
    pub vel_ref: f32,     // 速度参考值
    pub kp: f32,          // 比例增益（参考值：10）
    pub kd: f32,          // 微分增益（参考值：0.8）
    pub t_ref: f32,       // 力矩参考值
    pub crc: u4,          // CRC 校验（4位）
}

impl MitControlCommand {
    /// 根据协议文档的转换公式编码
    ///
    /// 协议中的位域布局：
    /// - Byte 0-1: Pos_ref (16位)
    /// - Byte 2-3: Vel_ref [bit11~bit4] | Kp [bit11~bit8] (跨字节打包)
    /// - Byte 4: Kp [bit7~bit0]
    /// - Byte 5-6: Kd [bit11~bit4] | T_ref [bit7~bit4] (跨字节打包)
    /// - Byte 7: T_ref [bit3~bit0] | CRC [bit3~bit0]
    pub fn to_frame(&self) -> PiperFrame {
        // 使用协议文档中的 float_to_uint 函数进行编码
        // 注意：由于跨字节位域打包复杂，建议使用手动位操作
        // 或使用 bilge 的嵌套结构（需要仔细设计位布局）

        // TODO: 实现完整的位域打包逻辑
        // 这里需要根据协议文档的转换公式和位域布局进行实现
        let mut data = [0u8; 8];
        // ... 位域打包代码 ...

        PiperFrame::new_standard(ID_MIT_CONTROL_BASE + self.joint_index as u16, &data)
    }

    /// 辅助函数：将浮点数转换为无符号整数（根据协议公式）
    fn float_to_uint(x: f32, x_min: f32, x_max: f32, bits: u32) -> u32 {
        let span = x_max - x_min;
        let offset = x_min;
        ((x - offset) * ((1u32 << bits) - 1) as f32 / span) as u32
    }

    /// 辅助函数：将无符号整数转换为浮点数（根据协议公式）
    fn uint_to_float(x_int: u32, x_min: f32, x_max: f32, bits: u32) -> f32 {
        let span = x_max - x_min;
        let offset = x_min;
        (x_int as f32) * span / ((1u32 << bits) - 1) as f32 + offset
    }
}
```

**实现建议**：
- MIT 控制指令的位域打包非常复杂，建议先实现基本功能，位域打包可以后续优化
- 可以使用 bilge 的嵌套结构，但需要仔细设计位布局
- 或者使用手动的位操作，虽然代码较长但更直观

---

### 3.5 `config.rs` - 配置帧定义

**职责**：
- 定义配置查询和设置指令
- 定义配置反馈帧

#### 3.5.1 电机使能指令 (0x471)

```rust
#[derive(Debug, Clone, Copy)]
pub struct MotorEnableCommand {
    pub joint_index: u8,  // 1-6 或 7（全部）
    pub enable: bool,     // true = 使能, false = 失能
}

impl MotorEnableCommand {
    pub fn to_frame(&self) -> PiperFrame {
        let mut data = [0u8; 8];
        data[0] = self.joint_index;
        data[1] = if self.enable { 0x02 } else { 0x01 };
        PiperFrame::new_standard(ID_MOTOR_ENABLE as u16, &data)
    }
}
```

#### 3.5.2 随动主从模式设置指令 (0x470)

```rust
#[derive(Debug, Clone, Copy)]
pub struct MasterSlaveModeCommand {
    pub link_setting: LinkSetting,  // Byte 0: 联动设置指令
    pub feedback_id_offset: u8,      // Byte 1: 反馈指令偏移值
    pub control_id_offset: u8,      // Byte 2: 控制指令偏移值
    pub target_id_offset: u8,       // Byte 3: 联动模式控制目标地址偏移值
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkSetting {
    Invalid = 0x00,
    TeachInputArm = 0xFA,   // 设置为示教输入臂
    MotionOutputArm = 0xFC, // 设置为运动输出臂
}

impl MasterSlaveModeCommand {
    pub fn to_frame(&self) -> PiperFrame {
        let data = [
            self.link_setting as u8,
            self.feedback_id_offset,
            self.control_id_offset,
            self.target_id_offset,
            0, 0, 0, 0,  // Byte 4-7: 保留（协议中 Len: 4，但 CAN 帧固定 8 字节）
        ];
        PiperFrame::new_standard(ID_MASTER_SLAVE_MODE as u16, &data)
    }
}
```

#### 3.5.3 查询电机限制指令 (0x472) 和反馈 (0x473)

```rust
#[derive(Debug, Clone, Copy)]
pub struct QueryMotorLimitCommand {
    pub joint_index: u8,  // Byte 0: 1-6
    pub query_type: QueryType,  // Byte 1: 查询内容
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryType {
    AngleAndSpeed = 0x01,  // 查询电机角度/最大速度
    Acceleration = 0x02,    // 查询电机最大加速度限制
}

#[derive(Debug, Clone, Copy, Default)]
pub struct MotorLimitFeedback {
    pub joint_index: u8,
    pub max_angle_limit: i16,    // Byte 1-2: 最大角度限制，单位 0.1°
    pub min_angle_limit: i16,    // Byte 3-4: 最小角度限制，单位 0.1°
    pub max_joint_speed: u16,    // Byte 5-6: 最大关节速度，单位 0.01rad/s
}
```

#### 3.5.4 设置电机限制指令 (0x474)

```rust
#[derive(Debug, Clone, Copy)]
pub struct SetMotorLimitCommand {
    pub joint_index: u8,
    pub max_angle_limit: i16,    // Byte 1-2: 最大角度限制，单位 0.1°（无效值：0x7FFF）
    pub min_angle_limit: i16,    // Byte 3-4: 最小角度限制，单位 0.1°（无效值：0x7FFF）
    pub max_joint_speed: u16,    // Byte 5-6: 最大关节速度，单位 0.01rad/s（无效值：0x7FFF）
}
```

#### 3.5.5 关节设置指令 (0x475)

```rust
#[derive(Debug, Clone, Copy)]
pub struct JointSettingCommand {
    pub joint_index: u8,              // Byte 0: 1-6 或 7（全部）
    pub set_zero_point: bool,         // Byte 1: 有效值 0xAE 表示设置零点
    pub accel_setting_enabled: bool,  // Byte 2: 有效值 0xAE 表示加速度参数设置生效
    pub max_joint_accel: u16,         // Byte 3-4: 最大关节加速度，单位 0.01rad/s²（无效值：0x7FFF）
    pub clear_error: bool,            // Byte 5: 有效值 0xAE 表示清除关节错误代码
}
```

#### 3.5.6 设置指令应答 (0x476)

**注意**：0x476 应答帧的字段有效性取决于被应答的指令类型：
- **设置指令应答**（0x471, 0x474, 0x475 等）：Byte 0 = 指令 ID 最后一个字节（如 0x71, 0x74, 0x75），Byte 1 可能有效（零点设置）
- **轨迹传输应答**（0x150）：Byte 0 = 0x50（特殊值），Byte 2-7 有效（轨迹点索引、包状态、NameIndex、CRC）

```rust
#[derive(Debug, Clone, Copy, Default)]
pub struct SettingResponse {
    pub response_index: u8,        // Byte 0: 应答指令索引
                                    // - 设置指令：取设置指令 ID 最后一个字节（如 0x471 -> 0x71）
                                    // - 轨迹传输：固定值 0x50
    pub zero_set_success: bool,    // Byte 1: 零点是否设置成功（0x01: 成功，0x00: 失败）
                                    // 仅在关节设置指令（0x475）成功设置零点时有效
    pub trajectory_point_index: u8, // Byte 2: 轨迹点传输成功应答（索引 N=0~255）
                                    // 仅在轨迹传输（0x150）时有效
    pub trajectory_pack_status: TrajectoryPackStatus, // Byte 3: 轨迹包传输完成应答
                                                        // 仅在轨迹传输（0x150）时有效
    pub name_index: u16,            // Byte 4-5: 当前轨迹包名称索引
                                    // 仅在轨迹传输（0x150）时有效
    pub crc16: u16,                 // Byte 6-7: CRC16
                                    // 仅在轨迹传输（0x150）时有效
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrajectoryPackStatus {
    NotCompleted = 0x00,
    Success = 0xAE,  // 传输完成且校验成功
    Failed = 0xEE,   // 校验失败，需要整包重传
}

impl SettingResponse {
    /// 判断是否为轨迹传输应答
    pub fn is_trajectory_response(&self) -> bool {
        self.response_index == 0x50
    }

    /// 判断是否为设置指令应答
    pub fn is_setting_response(&self) -> bool {
        self.response_index != 0x50 &&
        (self.response_index == 0x71 || self.response_index == 0x74 ||
         self.response_index == 0x75 || self.response_index == 0x7A)
    }
}
```

#### 3.5.7 参数查询与设置指令 (0x477)

**注意**：Byte 0（查询）和 Byte 1（设置）是互斥的，不能同时设置。

```rust
#[derive(Debug, Clone, Copy, Default)]
pub struct ParameterQuerySetCommand {
    pub query_type: Option<ParameterQuery>,  // Byte 0: 参数查询（与 set_type 互斥）
    pub set_type: Option<ParameterSet>,      // Byte 1: 参数设置（与 query_type 互斥）
    pub feedback_setting: FeedbackSetting,   // Byte 2: 0x48X 报文反馈设置
    pub load_setting_enabled: bool,          // Byte 3: 末端负载参数设置是否生效（有效值：0xAE）
    pub end_effector_load: EndEffectorLoad, // Byte 4: 设置末端负载
    // Byte 5-7: 保留
}

impl ParameterQuerySetCommand {
    /// 创建查询指令
    pub fn query(query_type: ParameterQuery) -> Self {
        Self {
            query_type: Some(query_type),
            set_type: None,
            feedback_setting: FeedbackSetting::Invalid,
            load_setting_enabled: false,
            end_effector_load: EndEffectorLoad::NoLoad,
        }
    }

    /// 创建设置指令
    pub fn set(set_type: ParameterSet) -> Self {
        Self {
            query_type: None,
            set_type: Some(set_type),
            feedback_setting: FeedbackSetting::Invalid,
            load_setting_enabled: false,
            end_effector_load: EndEffectorLoad::NoLoad,
        }
    }

    /// 验证字段有效性
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.query_type.is_some() && self.set_type.is_some() {
            return Err(ProtocolError::ParseError(
                "ParameterQuerySetCommand: query_type and set_type cannot be set simultaneously".to_string()
            ));
        }
        Ok(())
    }

    pub fn to_frame(&self) -> Result<PiperFrame, ProtocolError> {
        self.validate()?;

        let mut data = [0u8; 8];
        data[0] = self.query_type.map(|q| q as u8).unwrap_or(0);
        data[1] = self.set_type.map(|s| s as u8).unwrap_or(0);
        data[2] = self.feedback_setting as u8;
        data[3] = if self.load_setting_enabled { 0xAE } else { 0x00 };
        data[4] = self.end_effector_load as u8;
        // Byte 5-7: 保留，已初始化为 0

        Ok(PiperFrame::new_standard(ID_PARAMETER_QUERY_SET as u16, &data))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParameterQuery {
    EndVelocityAccel = 0x01,      // 查询末端V/acc参数
    CollisionProtection = 0x02,   // 查询碰撞防护等级
    TrajectoryIndex = 0x03,       // 查询当前轨迹索引
    GripperTeachParams = 0x04,   // 查询夹爪/示教器参数索引
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParameterSet {
    ResetEndVelocityAccel = 0x01,  // 设置末端V/acc参数为初始值
    ResetJointLimits = 0x02,       // 设置全部关节限位、关节最大速度、关节加速度为默认值
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeedbackSetting {
    Invalid = 0x00,
    Enable = 0x01,   // 开启周期反馈
    Disable = 0x02,  // 关闭周期反馈
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndEffectorLoad {
    NoLoad = 0x00,   // 空载
    HalfLoad = 0x01, // 半载
    FullLoad = 0x02, // 满载
}
```

#### 3.5.8 反馈当前末端速度/加速度参数 (0x478)

```rust
#[derive(Debug, Clone, Copy, Default)]
pub struct EndVelocityAccelFeedback {
    pub max_linear_velocity: u16,    // Byte 0-1: 末端最大线速度，单位 0.001m/s
    pub max_angular_velocity: u16,   // Byte 2-3: 末端最大角速度，单位 0.001rad/s
    pub max_linear_accel: u16,       // Byte 4-5: 末端最大线加速度，单位 0.001m/s²
    pub max_angular_accel: u16,      // Byte 6-7: 末端最大角加速度，单位 0.001rad/s²
}
```

#### 3.5.9 末端速度/加速度参数设置指令 (0x479)

```rust
#[derive(Debug, Clone, Copy)]
pub struct SetEndVelocityAccelCommand {
    pub max_linear_velocity: u16,    // Byte 0-1: 末端最大线速度，单位 0.001m/s（无效值：0x7FFF）
    pub max_angular_velocity: u16,   // Byte 2-3: 末端最大角速度，单位 0.001rad/s（无效值：0x7FFF）
    pub max_linear_accel: u16,       // Byte 4-5: 末端最大线加速度，单位 0.001m/s²（无效值：0x7FFF）
    pub max_angular_accel: u16,      // Byte 6-7: 末端最大角加速度，单位 0.001rad/s²（无效值：0x7FFF）
}
```

#### 3.5.10 碰撞防护等级设置指令 (0x47A) 和反馈 (0x47B)

```rust
#[derive(Debug, Clone, Copy, Default)]
pub struct CollisionProtectionLevelCommand {
    pub joint1_level: u8,  // Byte 0: 0~8，等级0代表不检测碰撞
    pub joint2_level: u8,  // Byte 1
    pub joint3_level: u8,  // Byte 2
    pub joint4_level: u8,  // Byte 3
    pub joint5_level: u8,  // Byte 4
    pub joint6_level: u8,  // Byte 5
    // Byte 6-7: 保留
}

// 0x47B 反馈结构与 0x47A 相同
pub type CollisionProtectionLevelFeedback = CollisionProtectionLevelCommand;
```

#### 3.5.11 反馈当前电机最大加速度限制 (0x47C)

```rust
#[derive(Debug, Clone, Copy, Default)]
pub struct MotorMaxAccelFeedback {
    pub joint_index: u8,
    pub max_joint_accel: u16,  // Byte 1-2: 最大关节加速度，单位 0.001rad/s²
    // Byte 3-7: 保留
}
```

#### 3.5.12 夹爪/示教器参数设置指令 (0x47D) 和反馈 (0x47E)

```rust
#[derive(Debug, Clone, Copy, Default)]
pub struct GripperTeachParamsCommand {
    pub travel_coefficient: u8,      // Byte 0: 示教器行程系数，100~200，单位 %（默认100%）
    pub max_travel_limit: u8,        // Byte 1: 夹爪/示教器最大控制行程限制值，单位 mm（小夹爪70mm，大夹爪100mm）
    pub friction_coefficient: u8,    // Byte 2: 示教器摩擦系数，1~10
    // Byte 3-7: 保留
}

// 0x47E 反馈结构与 0x47D 相同
pub type GripperTeachParamsFeedback = GripperTeachParamsCommand;
```

#### 3.5.13 其他配置指令

**0x121: 灯光控制指令**（可选实现，Phase 3）
- 控制关节上的 LED 灯光
- 节点ID: 0x1, 帧ID: 0x121

**0x422: 固件升级模式设定指令**（可选实现，Phase 3）
- 进入/退出固件升级模式
- 数据长度: 0x01

---

## 4. 实现策略

### 4.1 字节序处理

协议使用 **Motorola (MSB) 高位在前**，但 Rust 默认是小端序。需要手动处理：

```rust
// 大端字节序转换工具函数
pub fn bytes_to_i32_be(bytes: [u8; 4]) -> i32 {
    i32::from_be_bytes(bytes)
}

pub fn i32_to_bytes_be(value: i32) -> [u8; 4] {
    value.to_be_bytes()
}
```

### 4.2 位域处理（使用 bilge）

对于包含位域的字节（如故障码、状态码），**使用 bilge 的位字段结构**：

```rust
use bilge::prelude::*;

// 示例：夹爪状态位域
#[bitsize(8)]
#[derive(FromBits, DebugBits, Clone, Copy)]
pub struct GripperStatus {
    pub voltage_low: bool,          // Bit 0
    pub motor_over_temp: bool,     // Bit 1
    pub driver_over_current: bool, // Bit 2
    pub driver_over_temp: bool,    // Bit 3
    pub sensor_error: bool,        // Bit 4
    pub driver_error: bool,        // Bit 5
    pub enabled: bool,             // Bit 6
    pub homed: bool,               // Bit 7
}

// 使用方式：
let status_byte = frame.data[6];
let status = GripperStatus::from(u8::new(status_byte));

// 访问字段
if status.voltage_low() {
    // 处理电压过低
}

// 设置字段
let mut new_status = GripperStatus::from(u8::new(0));
new_status.set_enabled(true);
new_status.set_homed(true);
let status_byte = u8::from(new_status).value();
```

**关键点**：
- `#[bitsize(8)]` 必须等于所有字段的位宽之和（8 个 bool = 8 位）
- 使用 `FromBits` 进行 infallible 转换（所有位组合都有效）
- 使用 `TryFromBits` 如果某些位组合无效（需要错误处理）
- `bool` 类型占 1 位
- 使用 `u8::new(value)` 创建 bilge 的位宽类型
- 使用 `u8::from(struct).value()` 获取原始值

### 4.3 物理量转换

所有原始值字段都提供物理量转换方法：

```rust
impl JointFeedback12 {
    // 原始值（0.001° 单位）
    pub fn j1_raw(&self) -> i32 {
        self.j1_deg
    }

    // 物理量（度）
    pub fn j1(&self) -> f64 {
        self.j1_deg as f64 / 1000.0
    }

    // 物理量（弧度）
    pub fn j1_rad(&self) -> f64 {
        self.j1() * std::f64::consts::PI / 180.0
    }
}
```

### 4.4 bilge 最佳实践

根据 bilge README 和我们的协议特点，以下是推荐的最佳实践：

#### 4.4.1 何时使用 bilge

**推荐使用 bilge 的场景**：
1. **位域结构**：一个字节或多个字节中包含多个独立的位字段（如状态码、故障码）
2. **非字节对齐字段**：字段不是完整的字节（如 3 位、5 位等）
3. **嵌套位域**：位域中包含子结构或枚举

**不推荐使用 bilge 的场景**：
1. **完整字节的枚举**：如 `ControlMode`（0x00-0x07），使用普通枚举 + `From<u8>` 更简单
2. **多字节整数**：如 i32、i16，直接使用 `from_be_bytes()` 处理字节序即可
3. **简单字节数组**：如 8 字节数据，直接使用数组或切片

#### 4.4.2 bilge 语法要点

```rust
use bilge::prelude::*;

// 1. 基本结构体定义
#[bitsize(8)]  // 总位数必须等于所有字段位宽之和
#[derive(FromBits, DebugBits, Clone, Copy)]
pub struct StatusByte {
    pub flag1: bool,    // 1 位
    pub flag2: bool,    // 1 位
    pub value: u4,      // 4 位（bilge 提供 u1-u128 类型）
    pub reserved: u2,   // 2 位（保留字段）
}

// 2. 枚举定义（用于位域中的枚举值）
#[bitsize(3)]
#[derive(FromBits, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Standby = 0,
    Active = 1,
    Error = 2,
    // 如果未定义所有值，使用 TryFromBits 或 #[fallback]
}

// 3. 使用 #[fallback] 处理未定义值
#[bitsize(2)]
#[derive(FromBits, Debug, PartialEq)]
pub enum Subclass {
    Mouse,
    Keyboard,
    Speakers,
    #[fallback]
    Reserved,  // 所有未定义的值都会映射到这里
}

// 或者保留原始值：
#[bitsize(2)]
#[derive(FromBits, Debug, PartialEq)]
pub enum Subclass2 {
    Mouse,
    Keyboard,
    Speakers,
    #[fallback]
    Reserved(u2),  // 保留原始位值
}

// 4. 嵌套结构
#[bitsize(8)]
#[derive(FromBits, DebugBits)]
pub struct Header {
    pub version: u2,
    pub flags: Flags,  // 嵌套的位域结构
    pub reserved: u2,
}

#[bitsize(4)]
#[derive(FromBits, DebugBits)]
pub struct Flags {
    pub enabled: bool,
    pub error: bool,
    pub reserved: u2,
}

// 5. 数组支持
#[bitsize(32)]
#[derive(FromBits, DebugBits)]
pub struct InterruptSetEnables([bool; 32]);

// 使用：
let mut ise = InterruptSetEnables::from(u32::new(0b0000_0000_0000_0000_0000_0000_0001_0000));
let ise5 = ise.val_0_at(4);  // 访问数组元素
ise.set_val_0_at(2, ise5);   // 设置数组元素
```

#### 4.4.3 转换和访问

```rust
// 从原始值创建
let status = StatusByte::from(u8::new(0b1010_1100));

// 访问字段（使用 getter）
let flag1 = status.flag1();
status.set_flag2(true);

// 转换回原始值
let raw_value = u8::from(status).value();

// TryFrom 用于可能失败的情况
#[bitsize(2)]
#[derive(TryFromBits, Debug, PartialEq)]
pub enum Class {
    Mobile = 0,
    Semimobile = 1,
    // 2 未定义
    Stationary = 3,
}

let class = Class::try_from(u2::new(2));  // 返回 Err
let class = Class::try_from(u2::new(3));   // 返回 Ok(Class::Stationary)
```

#### 4.4.4 与协议解析的集成

```rust
impl TryFrom<PiperFrame> for GripperFeedback {
    type Error = ProtocolError;

    fn try_from(frame: PiperFrame) -> Result<Self, Self::Error> {
        // 1. 验证 CAN ID 和长度
        if frame.id != ID_GRIPPER_FEEDBACK {
            return Err(ProtocolError::InvalidCanId { id: frame.id });
        }
        if frame.len < 7 {
            return Err(ProtocolError::InvalidLength { expected: 7, actual: frame.len as usize });
        }

        // 2. 处理多字节整数（大端序）
        let travel_mm = i32::from_be_bytes([
            frame.data[0], frame.data[1], frame.data[2], frame.data[3]
        ]);

        let torque_nm = i16::from_be_bytes([frame.data[4], frame.data[5]]);

        // 3. 使用 bilge 解析位域
        let status = GripperStatus::from(u8::new(frame.data[6]));

        Ok(Self {
            travel_mm,
            torque_nm,
            status,
        })
    }
}
```

#### 4.4.5 常见错误和注意事项

1. **位宽不匹配**：`#[bitsize(N)]` 中的 N 必须等于所有字段位宽之和，否则编译错误
2. **字节序处理**：bilge 处理的是位序，字节序需要在传入 bilge 前处理
3. **枚举值完整性**：如果枚举未定义所有可能值，使用 `TryFromBits` 或 `#[fallback]`
4. **Debug 输出**：使用 `DebugBits` 而不是 `Debug` 来打印位域结构
5. **性能**：bilge 生成的代码性能等同于手写位操作，无需担心性能问题

### 4.4 错误处理

- 使用 `TryFrom<PiperFrame>` 进行解析，失败时返回 `ProtocolError`
- 使用 `Into<PiperFrame>` 或 `to_frame()` 方法进行编码

---

## 5. 测试策略

### 5.1 单元测试

为每个结构体编写测试：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_robot_status_feedback_parse() {
        let frame = PiperFrame::new_standard(
            ID_ROBOT_STATUS as u16,
            &[0x01, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00]
        );

        let status = RobotStatusFeedback::try_from(frame).unwrap();
        assert_eq!(status.control_mode, ControlMode::CanControl);
        assert_eq!(status.robot_status, RobotStatus::Normal);
    }

    #[test]
    fn test_joint_control_encode() {
        let cmd = JointControl12::new(90.0, -45.0);
        let frame = cmd.to_frame();

        assert_eq!(frame.id, ID_JOINT_CONTROL_12);
        // 验证编码后的字节
    }
}
```

### 5.2 集成测试

测试完整的编码-解码循环：

```rust
#[test]
fn test_encode_decode_roundtrip() {
    let original = JointControl12::new(90.0, -45.0);
    let frame = original.to_frame();
    let decoded = JointControl12::try_from(frame).unwrap();

    assert_eq!(original.j1_deg, decoded.j1_deg);
    assert_eq!(original.j2_deg, decoded.j2_deg);
}
```

---

## 6. 实现优先级

### Phase 1: 核心反馈帧（高频使用）
1. `RobotStatusFeedback` (0x2A1)
2. `JointFeedback12/34/56` (0x2A5~0x2A7)
3. `EndPoseFeedback1/2/3` (0x2A2~0x2A4)
4. `JointDriverHighSpeedFeedback` (0x251~0x256)

### Phase 2: 核心控制帧
1. `ControlModeCommand` (0x151)
2. `JointControl12/34/56` (0x155~0x157)
3. `EmergencyStopCommand` (0x150)
4. `MotorEnableCommand` (0x471)

### Phase 3: 完整协议覆盖
1. 夹爪相关（0x2A8, 0x159）
2. 配置指令（0x470~0x47E）
   - 0x470: 随动主从模式设置
   - 0x471: 电机使能/失能
   - 0x472~0x475: 电机限制查询和设置
   - 0x476: 设置指令应答
   - 0x477: 参数查询与设置
   - 0x478: 反馈末端速度/加速度参数
   - 0x479: 设置末端速度/加速度参数
   - 0x47A~0x47B: 碰撞防护等级
   - 0x47C: 反馈电机最大加速度限制
   - 0x47D~0x47E: 夹爪/示教器参数
3. MIT 控制（0x15A~0x15F）
4. 关节末端速度/加速度反馈（0x481~0x486）
5. 其他辅助功能（0x121 灯光控制, 0x422 固件升级等）

---

## 7. 注意事项

1. **字节序**：协议使用大端序（Motorola），所有多字节数据都需要转换
2. **单位转换**：协议中的单位（0.001mm, 0.001°）需要在物理量方法中转换
3. **位域处理**：故障码、状态码等使用位域，需要仔细处理每一位
4. **可选字段**：某些帧的某些字节在不同模式下含义不同，需要文档说明
5. **ID 偏移**：协议支持 ID 偏移（0x470 指令），需要在 `ids.rs` 中考虑

---

## 8. bilge 使用总结

### 8.1 我们的协议中 bilge 的应用场景

根据协议文档分析，以下场景适合使用 bilge：

1. **故障码位域**（0x2A1 Byte 6-7）：
   - `FaultCodeAngleLimit`：6 个 bool + 2 位保留
   - `FaultCodeCommError`：6 个 bool + 2 位保留

2. **状态码位域**（0x2A8 Byte 6）：
   - `GripperStatus`：8 个 bool 标志位

3. **驱动器状态位域**（0x261~0x266 Byte 5）：
   - `DriverStatus`：8 个 bool 标志位

4. **夹爪控制标志位域**（0x159 Byte 6）：
   - `GripperControlFlags`：2 个 bool + 6 位保留

5. **MIT 控制指令**（0x15A~0x15F）：
   - 如果协议中的位域打包复杂，可以使用 bilge 简化（但跨字节打包可能需要手动处理）

### 8.2 不需要使用 bilge 的场景

1. **完整字节枚举**：如 `ControlMode`、`RobotStatus` 等，使用普通枚举 + `From<u8>` 更简单
2. **多字节整数**：如关节角度（i32）、坐标（i32）等，直接使用 `from_be_bytes()` 处理字节序即可
3. **简单字节字段**：如速度百分比（u8）、索引（u8）等

### 8.3 实现建议

- **Phase 1**：先实现不使用 bilge 的简单结构（完整字节字段）
- **Phase 2**：为位域结构添加 bilge 支持（故障码、状态码）
- **Phase 3**：优化复杂位域（如 MIT 控制指令）

### 8.4 bilge 关键语法速查

```rust
use bilge::prelude::*;

// 1. 基本用法
#[bitsize(8)]
#[derive(FromBits, DebugBits, Clone, Copy)]
pub struct Status {
    pub flag1: bool,    // 1 位
    pub value: u4,      // 4 位
    pub reserved: u3,   // 3 位
}

// 2. 创建和转换
let status = Status::from(u8::new(0b1010_1100));
let flag = status.flag1();           // 访问
status.set_flag1(true);              // 设置
let raw = u8::from(status).value();  // 转回原始值

// 3. 枚举（带 fallback）
#[bitsize(2)]
#[derive(FromBits, Debug, PartialEq)]
pub enum Mode {
    Standby = 0,
    Active = 1,
    #[fallback]
    Reserved,
}
```

---

## 9. 后续优化

1. **代码生成**：如果协议稳定，可以考虑从协议文档自动生成代码
2. **序列化支持**：如果需要持久化，可以添加 serde 支持（但注意性能）
3. **性能基准测试**：对比使用 bilge 和手写位操作的性能差异（预期应该相同）

---

## 10. 总结

本实现方案遵循 TDD 文档的设计原则：
- **使用 `bilge` 进行位域解析**：对于包含位字段的结构（故障码、状态码），使用 bilge 提供类型安全和可读性
- **普通枚举处理完整字节**：对于完整字节的枚举值，使用普通枚举 + `From<u8>` 更简单直接
- **手动处理多字节整数**：对于 i32、i16 等多字节整数，先处理字节序，然后直接使用
- **提供类型安全的 API**：所有结构体都提供物理量转换方法
- **零成本抽象**：bilge 生成的代码性能等同于手写位操作
- **完整的协议覆盖**：分阶段实现，优先核心功能

实现将分阶段进行，优先实现高频使用的反馈帧和控制帧，确保核心功能可用后再完善其他协议。对于位域结构，使用 bilge 可以显著提高代码的可读性和类型安全性。

