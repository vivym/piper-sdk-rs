//! 反馈帧结构体定义
//!
//! 包含所有机械臂反馈帧的结构体，提供从 `PiperFrame` 解析的方法
//! 和物理量转换方法。

use crate::can::PiperFrame;
use crate::control::{ControlModeCommand, InstallPosition, MitMode};
use crate::{
    ProtocolError, bytes_to_i16_be, bytes_to_i32_be,
    ids::{
        ID_CONTROL_MODE, ID_END_POSE_1, ID_END_POSE_2, ID_END_POSE_3, ID_FIRMWARE_READ,
        ID_GRIPPER_CONTROL, ID_GRIPPER_FEEDBACK, ID_JOINT_CONTROL_12, ID_JOINT_CONTROL_34,
        ID_JOINT_CONTROL_56, ID_JOINT_DRIVER_HIGH_SPEED_BASE, ID_JOINT_DRIVER_LOW_SPEED_BASE,
        ID_JOINT_END_VELOCITY_ACCEL_BASE, ID_JOINT_FEEDBACK_12, ID_JOINT_FEEDBACK_34,
        ID_JOINT_FEEDBACK_56, ID_ROBOT_STATUS,
    },
};
use bilge::prelude::*;

// ============================================================================
// 枚举类型定义
// ============================================================================

/// 控制模式（反馈帧版本，0x2A1）
///
/// 注意：反馈帧和控制指令的 ControlMode 枚举值不同。
/// 反馈帧包含完整定义（0x00-0x07），控制指令只支持部分值。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, num_enum::FromPrimitive)]
#[repr(u8)]
pub enum ControlMode {
    /// 待机模式
    #[default]
    Standby = 0x00,
    /// CAN指令控制模式
    CanControl = 0x01,
    /// 示教模式
    Teach = 0x02,
    /// 以太网控制模式
    Ethernet = 0x03,
    /// wifi控制模式
    Wifi = 0x04,
    /// 遥控器控制模式（仅反馈帧有，控制指令不支持）
    Remote = 0x05,
    /// 联动示教输入模式（仅反馈帧有，控制指令不支持）
    LinkTeach = 0x06,
    /// 离线轨迹模式
    OfflineTrajectory = 0x07,
}

/// 机械臂状态
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, num_enum::FromPrimitive)]
#[repr(u8)]
pub enum RobotStatus {
    /// 正常
    #[default]
    Normal = 0x00,
    /// 急停
    EmergencyStop = 0x01,
    /// 无解
    NoSolution = 0x02,
    /// 奇异点
    Singularity = 0x03,
    /// 目标角度超过限
    AngleLimitExceeded = 0x04,
    /// 关节通信异常
    JointCommError = 0x05,
    /// 关节抱闸未打开
    JointBrakeNotOpen = 0x06,
    /// 机械臂发生碰撞
    Collision = 0x07,
    /// 拖动示教时超速
    TeachOverspeed = 0x08,
    /// 关节状态异常
    JointStatusError = 0x09,
    /// 其它异常
    OtherError = 0x0A,
    /// 示教记录
    TeachRecord = 0x0B,
    /// 示教执行
    TeachExecute = 0x0C,
    /// 示教暂停
    TeachPause = 0x0D,
    /// 主控NTC过温
    MainControlOverTemp = 0x0E,
    /// 释放电阻NTC过温
    ResistorOverTemp = 0x0F,
}

/// MOVE 模式
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, num_enum::FromPrimitive)]
#[repr(u8)]
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

/// 示教状态
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, num_enum::FromPrimitive)]
#[repr(u8)]
pub enum TeachStatus {
    /// 关闭
    #[default]
    Closed = 0x00,
    /// 开始示教记录（进入拖动示教模式）
    StartRecord = 0x01,
    /// 结束示教记录（退出拖动示教模式）
    EndRecord = 0x02,
    /// 执行示教轨迹（拖动示教轨迹复现）
    Execute = 0x03,
    /// 暂停执行
    Pause = 0x04,
    /// 继续执行（轨迹复现继续）
    Continue = 0x05,
    /// 终止执行
    Terminate = 0x06,
    /// 运动到轨迹起点
    MoveToStart = 0x07,
}

/// 运动状态
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, num_enum::FromPrimitive)]
#[repr(u8)]
pub enum MotionStatus {
    /// 到达指定点位
    #[default]
    Arrived = 0x00,
    /// 未到达指定点位
    NotArrived = 0x01,
}

// ============================================================================
// 位域结构定义（使用 bilge）
// ============================================================================

/// 故障码位域（Byte 6: 角度超限位）
///
/// 协议定义（Motorola MSB 高位在前）：
/// - Bit 0: 1号关节角度超限位（0：正常 1：异常）
/// - Bit 1: 2号关节角度超限位
/// - Bit 2: 3号关节角度超限位
/// - Bit 3: 4号关节角度超限位
/// - Bit 4: 5号关节角度超限位
/// - Bit 5: 6号关节角度超限位
/// - Bit 6-7: 保留
///
/// 注意：协议使用 Motorola (MSB) 高位在前，这是指**字节序**（多字节整数）。
/// 对于**单个字节内的位域**，协议明确 Bit 0 对应 1号关节，这是 LSB first（小端位序）。
/// bilge 默认使用 LSB first 位序，与协议要求一致。
#[bitsize(8)]
#[derive(FromBits, DebugBits, Clone, Copy, Default)]
pub struct FaultCodeAngleLimit {
    pub joint1_limit: bool, // Bit 0: 1号关节角度超限位
    pub joint2_limit: bool, // Bit 1: 2号关节角度超限位
    pub joint3_limit: bool, // Bit 2: 3号关节角度超限位
    pub joint4_limit: bool, // Bit 3: 4号关节角度超限位
    pub joint5_limit: bool, // Bit 4: 5号关节角度超限位
    pub joint6_limit: bool, // Bit 5: 6号关节角度超限位
    pub reserved: u2,       // Bit 6-7: 保留
}

/// 故障码位域（Byte 7: 通信异常）
///
/// 协议定义（Motorola MSB 高位在前）：
/// - Bit 0: 1号关节通信异常（0：正常 1：异常）
/// - Bit 1: 2号关节通信异常
/// - Bit 2: 3号关节通信异常
/// - Bit 3: 4号关节通信异常
/// - Bit 4: 5号关节通信异常
/// - Bit 5: 6号关节通信异常
/// - Bit 6-7: 保留
#[bitsize(8)]
#[derive(FromBits, DebugBits, Clone, Copy, Default)]
pub struct FaultCodeCommError {
    pub joint1_comm_error: bool, // Bit 0: 1号关节通信异常
    pub joint2_comm_error: bool, // Bit 1: 2号关节通信异常
    pub joint3_comm_error: bool, // Bit 2: 3号关节通信异常
    pub joint4_comm_error: bool, // Bit 3: 4号关节通信异常
    pub joint5_comm_error: bool, // Bit 4: 5号关节通信异常
    pub joint6_comm_error: bool, // Bit 5: 6号关节通信异常
    pub reserved: u2,            // Bit 6-7: 保留
}

// ============================================================================
// 机械臂状态反馈结构体
// ============================================================================

/// 机械臂状态反馈 (0x2A1)
///
/// 包含控制模式、机械臂状态、MOVE 模式、示教状态、运动状态、
/// 轨迹点索引以及故障码位域。
#[derive(Debug, Clone, Copy, Default)]
pub struct RobotStatusFeedback {
    pub control_mode: ControlMode,                   // Byte 0
    pub robot_status: RobotStatus,                   // Byte 1
    pub move_mode: MoveMode,                         // Byte 2
    pub teach_status: TeachStatus,                   // Byte 3
    pub motion_status: MotionStatus,                 // Byte 4
    pub trajectory_point_index: u8,                  // Byte 5
    pub fault_code_angle_limit: FaultCodeAngleLimit, // Byte 6 (位域)
    pub fault_code_comm_error: FaultCodeCommError,   // Byte 7 (位域)
}

impl TryFrom<PiperFrame> for RobotStatusFeedback {
    type Error = ProtocolError;

    fn try_from(frame: PiperFrame) -> Result<Self, Self::Error> {
        // 验证 CAN ID
        if frame.id != ID_ROBOT_STATUS {
            return Err(ProtocolError::InvalidCanId { id: frame.id });
        }

        // 验证数据长度
        if frame.len < 8 {
            return Err(ProtocolError::InvalidLength {
                expected: 8,
                actual: frame.len as usize,
            });
        }

        // 解析所有字段
        Ok(Self {
            control_mode: ControlMode::from(frame.data[0]),
            robot_status: RobotStatus::from(frame.data[1]),
            move_mode: MoveMode::from(frame.data[2]),
            teach_status: TeachStatus::from(frame.data[3]),
            motion_status: MotionStatus::from(frame.data[4]),
            trajectory_point_index: frame.data[5],
            fault_code_angle_limit: FaultCodeAngleLimit::from(u8::new(frame.data[6])),
            fault_code_comm_error: FaultCodeCommError::from(u8::new(frame.data[7])),
        })
    }
}

// ============================================================================
// 关节反馈结构体
// ============================================================================

/// 机械臂臂部关节反馈12 (0x2A5)
///
/// 包含 J1 和 J2 关节的角度反馈。
/// 单位：0.001°（原始值），可通过方法转换为度或弧度。
#[derive(Debug, Clone, Copy, Default)]
pub struct JointFeedback12 {
    pub j1_deg: i32, // Byte 0-3: J1角度，单位 0.001°
    pub j2_deg: i32, // Byte 4-7: J2角度，单位 0.001°
}

impl JointFeedback12 {
    /// 获取 J1 原始值（0.001° 单位）
    pub fn j1_raw(&self) -> i32 {
        self.j1_deg
    }

    /// 获取 J2 原始值（0.001° 单位）
    pub fn j2_raw(&self) -> i32 {
        self.j2_deg
    }

    /// 获取 J1 角度（度）
    pub fn j1(&self) -> f64 {
        self.j1_deg as f64 / 1000.0
    }

    /// 获取 J2 角度（度）
    pub fn j2(&self) -> f64 {
        self.j2_deg as f64 / 1000.0
    }

    /// 获取 J1 角度（弧度）
    pub fn j1_rad(&self) -> f64 {
        self.j1() * std::f64::consts::PI / 180.0
    }

    /// 获取 J2 角度（弧度）
    pub fn j2_rad(&self) -> f64 {
        self.j2() * std::f64::consts::PI / 180.0
    }
}

impl TryFrom<PiperFrame> for JointFeedback12 {
    type Error = ProtocolError;

    fn try_from(frame: PiperFrame) -> Result<Self, Self::Error> {
        // 验证 CAN ID
        if frame.id != ID_JOINT_FEEDBACK_12 {
            return Err(ProtocolError::InvalidCanId { id: frame.id });
        }

        // 验证数据长度
        if frame.len < 8 {
            return Err(ProtocolError::InvalidLength {
                expected: 8,
                actual: frame.len as usize,
            });
        }

        // 解析 J1 角度（Byte 0-3，大端字节序）
        let j1_bytes = [frame.data[0], frame.data[1], frame.data[2], frame.data[3]];
        let j1_deg = bytes_to_i32_be(j1_bytes);

        // 解析 J2 角度（Byte 4-7，大端字节序）
        let j2_bytes = [frame.data[4], frame.data[5], frame.data[6], frame.data[7]];
        let j2_deg = bytes_to_i32_be(j2_bytes);

        Ok(Self { j1_deg, j2_deg })
    }
}

/// 机械臂腕部关节反馈34 (0x2A6)
///
/// 包含 J3 和 J4 关节的角度反馈。
/// 单位：0.001°（原始值），可通过方法转换为度或弧度。
#[derive(Debug, Clone, Copy, Default)]
pub struct JointFeedback34 {
    pub j3_deg: i32, // Byte 0-3: J3角度，单位 0.001°
    pub j4_deg: i32, // Byte 4-7: J4角度，单位 0.001°
}

impl JointFeedback34 {
    /// 获取 J3 原始值（0.001° 单位）
    pub fn j3_raw(&self) -> i32 {
        self.j3_deg
    }

    /// 获取 J4 原始值（0.001° 单位）
    pub fn j4_raw(&self) -> i32 {
        self.j4_deg
    }

    /// 获取 J3 角度（度）
    pub fn j3(&self) -> f64 {
        self.j3_deg as f64 / 1000.0
    }

    /// 获取 J4 角度（度）
    pub fn j4(&self) -> f64 {
        self.j4_deg as f64 / 1000.0
    }

    /// 获取 J3 角度（弧度）
    pub fn j3_rad(&self) -> f64 {
        self.j3() * std::f64::consts::PI / 180.0
    }

    /// 获取 J4 角度（弧度）
    pub fn j4_rad(&self) -> f64 {
        self.j4() * std::f64::consts::PI / 180.0
    }
}

impl TryFrom<PiperFrame> for JointFeedback34 {
    type Error = ProtocolError;

    fn try_from(frame: PiperFrame) -> Result<Self, Self::Error> {
        // 验证 CAN ID
        if frame.id != ID_JOINT_FEEDBACK_34 {
            return Err(ProtocolError::InvalidCanId { id: frame.id });
        }

        // 验证数据长度
        if frame.len < 8 {
            return Err(ProtocolError::InvalidLength {
                expected: 8,
                actual: frame.len as usize,
            });
        }

        // 解析 J3 角度（Byte 0-3，大端字节序）
        let j3_bytes = [frame.data[0], frame.data[1], frame.data[2], frame.data[3]];
        let j3_deg = bytes_to_i32_be(j3_bytes);

        // 解析 J4 角度（Byte 4-7，大端字节序）
        let j4_bytes = [frame.data[4], frame.data[5], frame.data[6], frame.data[7]];
        let j4_deg = bytes_to_i32_be(j4_bytes);

        Ok(Self { j3_deg, j4_deg })
    }
}

/// 机械臂腕部关节反馈56 (0x2A7)
///
/// 包含 J5 和 J6 关节的角度反馈。
/// 单位：0.001°（原始值），可通过方法转换为度或弧度。
#[derive(Debug, Clone, Copy, Default)]
pub struct JointFeedback56 {
    pub j5_deg: i32, // Byte 0-3: J5角度，单位 0.001°
    pub j6_deg: i32, // Byte 4-7: J6角度，单位 0.001°
}

impl JointFeedback56 {
    /// 获取 J5 原始值（0.001° 单位）
    pub fn j5_raw(&self) -> i32 {
        self.j5_deg
    }

    /// 获取 J6 原始值（0.001° 单位）
    pub fn j6_raw(&self) -> i32 {
        self.j6_deg
    }

    /// 获取 J5 角度（度）
    pub fn j5(&self) -> f64 {
        self.j5_deg as f64 / 1000.0
    }

    /// 获取 J6 角度（度）
    pub fn j6(&self) -> f64 {
        self.j6_deg as f64 / 1000.0
    }

    /// 获取 J5 角度（弧度）
    pub fn j5_rad(&self) -> f64 {
        self.j5() * std::f64::consts::PI / 180.0
    }

    /// 获取 J6 角度（弧度）
    pub fn j6_rad(&self) -> f64 {
        self.j6() * std::f64::consts::PI / 180.0
    }
}

impl TryFrom<PiperFrame> for JointFeedback56 {
    type Error = ProtocolError;

    fn try_from(frame: PiperFrame) -> Result<Self, Self::Error> {
        // 验证 CAN ID
        if frame.id != ID_JOINT_FEEDBACK_56 {
            return Err(ProtocolError::InvalidCanId { id: frame.id });
        }

        // 验证数据长度
        if frame.len < 8 {
            return Err(ProtocolError::InvalidLength {
                expected: 8,
                actual: frame.len as usize,
            });
        }

        // 解析 J5 角度（Byte 0-3，大端字节序）
        let j5_bytes = [frame.data[0], frame.data[1], frame.data[2], frame.data[3]];
        let j5_deg = bytes_to_i32_be(j5_bytes);

        // 解析 J6 角度（Byte 4-7，大端字节序）
        let j6_bytes = [frame.data[4], frame.data[5], frame.data[6], frame.data[7]];
        let j6_deg = bytes_to_i32_be(j6_bytes);

        Ok(Self { j5_deg, j6_deg })
    }
}

// ============================================================================
// 末端位姿反馈结构体
// ============================================================================

/// 机械臂末端位姿反馈1 (0x2A2)
///
/// 包含 X 和 Y 坐标反馈。
/// 单位：0.001mm（原始值），可通过方法转换为 mm。
#[derive(Debug, Clone, Copy, Default)]
pub struct EndPoseFeedback1 {
    pub x_mm: i32, // Byte 0-3: X坐标，单位 0.001mm
    pub y_mm: i32, // Byte 4-7: Y坐标，单位 0.001mm
}

impl EndPoseFeedback1 {
    /// 获取 X 原始值（0.001mm 单位）
    pub fn x_raw(&self) -> i32 {
        self.x_mm
    }

    /// 获取 Y 原始值（0.001mm 单位）
    pub fn y_raw(&self) -> i32 {
        self.y_mm
    }

    /// 获取 X 坐标（mm）
    pub fn x(&self) -> f64 {
        self.x_mm as f64 / 1000.0
    }

    /// 获取 Y 坐标（mm）
    pub fn y(&self) -> f64 {
        self.y_mm as f64 / 1000.0
    }
}

impl TryFrom<PiperFrame> for EndPoseFeedback1 {
    type Error = ProtocolError;

    fn try_from(frame: PiperFrame) -> Result<Self, Self::Error> {
        // 验证 CAN ID
        if frame.id != ID_END_POSE_1 {
            return Err(ProtocolError::InvalidCanId { id: frame.id });
        }

        // 验证数据长度
        if frame.len < 8 {
            return Err(ProtocolError::InvalidLength {
                expected: 8,
                actual: frame.len as usize,
            });
        }

        // 解析 X 坐标（Byte 0-3，大端字节序）
        let x_bytes = [frame.data[0], frame.data[1], frame.data[2], frame.data[3]];
        let x_mm = bytes_to_i32_be(x_bytes);

        // 解析 Y 坐标（Byte 4-7，大端字节序）
        let y_bytes = [frame.data[4], frame.data[5], frame.data[6], frame.data[7]];
        let y_mm = bytes_to_i32_be(y_bytes);

        Ok(Self { x_mm, y_mm })
    }
}

/// 机械臂末端位姿反馈2 (0x2A3)
///
/// 包含 Z 坐标和 RX 角度反馈。
/// 单位：Z 为 0.001mm（原始值），RX 为 0.001°（原始值）。
#[derive(Debug, Clone, Copy, Default)]
pub struct EndPoseFeedback2 {
    pub z_mm: i32,   // Byte 0-3: Z坐标，单位 0.001mm
    pub rx_deg: i32, // Byte 4-7: RX角度，单位 0.001°
}

impl EndPoseFeedback2 {
    /// 获取 Z 原始值（0.001mm 单位）
    pub fn z_raw(&self) -> i32 {
        self.z_mm
    }

    /// 获取 RX 原始值（0.001° 单位）
    pub fn rx_raw(&self) -> i32 {
        self.rx_deg
    }

    /// 获取 Z 坐标（mm）
    pub fn z(&self) -> f64 {
        self.z_mm as f64 / 1000.0
    }

    /// 获取 RX 角度（度）
    pub fn rx(&self) -> f64 {
        self.rx_deg as f64 / 1000.0
    }

    /// 获取 RX 角度（弧度）
    pub fn rx_rad(&self) -> f64 {
        self.rx() * std::f64::consts::PI / 180.0
    }
}

impl TryFrom<PiperFrame> for EndPoseFeedback2 {
    type Error = ProtocolError;

    fn try_from(frame: PiperFrame) -> Result<Self, Self::Error> {
        // 验证 CAN ID
        if frame.id != ID_END_POSE_2 {
            return Err(ProtocolError::InvalidCanId { id: frame.id });
        }

        // 验证数据长度
        if frame.len < 8 {
            return Err(ProtocolError::InvalidLength {
                expected: 8,
                actual: frame.len as usize,
            });
        }

        // 解析 Z 坐标（Byte 0-3，大端字节序）
        let z_bytes = [frame.data[0], frame.data[1], frame.data[2], frame.data[3]];
        let z_mm = bytes_to_i32_be(z_bytes);

        // 解析 RX 角度（Byte 4-7，大端字节序）
        let rx_bytes = [frame.data[4], frame.data[5], frame.data[6], frame.data[7]];
        let rx_deg = bytes_to_i32_be(rx_bytes);

        Ok(Self { z_mm, rx_deg })
    }
}

/// 机械臂末端位姿反馈3 (0x2A4)
///
/// 包含 RY 和 RZ 角度反馈。
/// 单位：0.001°（原始值），可通过方法转换为度或弧度。
#[derive(Debug, Clone, Copy, Default)]
pub struct EndPoseFeedback3 {
    pub ry_deg: i32, // Byte 0-3: RY角度，单位 0.001°
    pub rz_deg: i32, // Byte 4-7: RZ角度，单位 0.001°
}

impl EndPoseFeedback3 {
    /// 获取 RY 原始值（0.001° 单位）
    pub fn ry_raw(&self) -> i32 {
        self.ry_deg
    }

    /// 获取 RZ 原始值（0.001° 单位）
    pub fn rz_raw(&self) -> i32 {
        self.rz_deg
    }

    /// 获取 RY 角度（度）
    pub fn ry(&self) -> f64 {
        self.ry_deg as f64 / 1000.0
    }

    /// 获取 RZ 角度（度）
    pub fn rz(&self) -> f64 {
        self.rz_deg as f64 / 1000.0
    }

    /// 获取 RY 角度（弧度）
    pub fn ry_rad(&self) -> f64 {
        self.ry() * std::f64::consts::PI / 180.0
    }

    /// 获取 RZ 角度（弧度）
    pub fn rz_rad(&self) -> f64 {
        self.rz() * std::f64::consts::PI / 180.0
    }
}

impl TryFrom<PiperFrame> for EndPoseFeedback3 {
    type Error = ProtocolError;

    fn try_from(frame: PiperFrame) -> Result<Self, Self::Error> {
        // 验证 CAN ID
        if frame.id != ID_END_POSE_3 {
            return Err(ProtocolError::InvalidCanId { id: frame.id });
        }

        // 验证数据长度
        if frame.len < 8 {
            return Err(ProtocolError::InvalidLength {
                expected: 8,
                actual: frame.len as usize,
            });
        }

        // 解析 RY 角度（Byte 0-3，大端字节序）
        let ry_bytes = [frame.data[0], frame.data[1], frame.data[2], frame.data[3]];
        let ry_deg = bytes_to_i32_be(ry_bytes);

        // 解析 RZ 角度（Byte 4-7，大端字节序）
        let rz_bytes = [frame.data[4], frame.data[5], frame.data[6], frame.data[7]];
        let rz_deg = bytes_to_i32_be(rz_bytes);

        Ok(Self { ry_deg, rz_deg })
    }
}

// ============================================================================
// 关节驱动器高速反馈结构体
// ============================================================================

/// 关节驱动器高速反馈 (0x251~0x256)
///
/// 包含关节速度、电流和位置反馈。
/// - 速度单位：0.001rad/s（原始值）
/// - 电流单位：0.001A（原始值）
/// - 位置单位：rad（原始值）
///
/// 注意：关节索引从 CAN ID 推导（0x251 -> 1, 0x252 -> 2, ..., 0x256 -> 6）
#[derive(Debug, Clone, Copy, Default)]
pub struct JointDriverHighSpeedFeedback {
    pub joint_index: u8,  // 从 ID 推导：0x251 -> 1, 0x252 -> 2, ...
    pub speed_rad_s: i16, // Byte 0-1: 速度，单位 0.001rad/s
    pub current_a: i16,   // Byte 2-3: 电流，单位 0.001A（有符号 i16，支持负值表示反向电流）
    pub position_rad: i32, // Byte 4-7: 位置（未使用）
                          //
                          // ⚠️ 此字段单位未确认（可能是 rad、mrad、0.01° 或编码器 ticks）
                          // ⚠️ 生产代码不使用此字段（实际位置来自 0x2A5-0x2A7 帧的 millidegrees）
                          // 💡 如需位置数据，请使用 JointFeedback12/34/56 的 j1_rad() 等方法
                          // 📖 详见：docs/v0/position_unit_analysis_report.md
}

impl JointDriverHighSpeedFeedback {
    /// 关节 1-3 的力矩系数（CAN ID: 0x251~0x253）
    ///
    /// 根据官方参考实现，关节 1、2、3 使用此系数计算力矩。
    /// 公式：torque = current * COEFFICIENT_1_3
    pub const COEFFICIENT_1_3: f64 = 1.18125;

    /// 关节 4-6 的力矩系数（CAN ID: 0x254~0x256）
    ///
    /// 根据官方参考实现，关节 4、5、6 使用此系数计算力矩。
    /// 公式：torque = current * COEFFICIENT_4_6
    pub const COEFFICIENT_4_6: f64 = 0.95844;

    /// 获取速度原始值（0.001rad/s 单位）
    pub fn speed_raw(&self) -> i16 {
        self.speed_rad_s
    }

    /// 获取电流原始值（0.001A 单位）
    pub fn current_raw(&self) -> i16 {
        self.current_a
    }

    /// 获取位置原始值（rad 单位）
    pub fn position_raw(&self) -> i32 {
        self.position_rad
    }

    /// 获取速度（rad/s）
    pub fn speed(&self) -> f64 {
        self.speed_rad_s as f64 / 1000.0
    }

    /// 获取电流（A）
    ///
    /// 注意：电流可以为负值（反向电流）
    pub fn current(&self) -> f64 {
        self.current_a as f64 / 1000.0
    }

    /// 获取位置（rad）
    ///
    /// # ⚠️ 弃用警告 (Deprecated)
    ///
    /// **此方法的返回值单位未确认**，可能导致不正确的位置值。
    ///
    /// **已知问题**:
    /// - 字段标注为 `rad`，但测试数据存在矛盾（3141592 对应 π？）
    /// - 可能的单位：rad、mrad（0.001rad）、0.01°、编码器 ticks
    /// - 当前**没有生产代码使用此方法**
    ///
    /// **替代方案**:
    /// - 高层 API: `piper.observer().get_joint_position(joint)` - 推荐，单位已确认为弧度
    /// - 协议层: `JointFeedback12::j1_rad()`, `j2_rad()` 等 - 单位明确 (从 0.001° 转换)
    /// - 原始值: `self.position_raw()` - 获取未转换的 i32 原始值
    ///
    /// **背景**: 详见 `docs/v0/position_unit_analysis_report.md`
    #[deprecated(
        since = "0.1.0",
        note = "Field unit unverified (rad vs mrad). Prefer `Observer::get_joint_position()` for verified position data, or use `position_raw()` for raw access."
    )]
    pub fn position(&self) -> f64 {
        self.position_rad as f64
    }

    /// 计算力矩（N·m）
    ///
    /// 根据关节索引和电流值计算力矩。
    /// - 关节 1-3 (CAN ID: 0x251~0x253) 使用系数 `COEFFICIENT_1_3 = 1.18125`
    /// - 关节 4-6 (CAN ID: 0x254~0x256) 使用系数 `COEFFICIENT_4_6 = 0.95844`
    ///
    /// 公式：`torque = current * coefficient`
    ///
    /// # 参数
    /// - `current_opt`: 可选的电流值（A）。如果为 `None`，则使用当前反馈的电流值。
    ///
    /// # 返回值
    /// 计算得到的力矩值（N·m）
    ///
    /// # 示例
    /// ```rust
    /// # use piper_protocol::feedback::JointDriverHighSpeedFeedback;
    /// # use piper_protocol::can::PiperFrame;
    /// # let frame = PiperFrame::new_standard(0x251, &[0; 8]);
    /// # let feedback = JointDriverHighSpeedFeedback::try_from(frame).unwrap();
    /// // 使用反馈中的电流值计算力矩
    /// let torque = feedback.torque(None);
    ///
    /// // 使用指定的电流值计算力矩
    /// let torque = feedback.torque(Some(2.5)); // 2.5A
    /// ```
    pub fn torque(&self, current_opt: Option<f64>) -> f64 {
        let current = current_opt.unwrap_or_else(|| self.current());
        let coefficient = if self.joint_index <= 3 {
            Self::COEFFICIENT_1_3
        } else {
            Self::COEFFICIENT_4_6
        };
        current * coefficient
    }

    /// 获取力矩原始值（0.001N·m 单位）
    ///
    /// 返回以 0.001N·m 为单位的力矩原始值（整数形式）。
    /// 这对应于官方参考实现中 `effort` 字段的单位。
    ///
    /// # 返回值
    /// 力矩原始值（0.001N·m 单位，即毫牛·米）
    ///
    /// # 示例
    /// ```rust
    /// # use piper_protocol::feedback::JointDriverHighSpeedFeedback;
    /// # use piper_protocol::can::PiperFrame;
    /// # let frame = PiperFrame::new_standard(0x251, &[0; 8]);
    /// # let feedback = JointDriverHighSpeedFeedback::try_from(frame).unwrap();
    /// let torque_raw = feedback.torque_raw(); // 例如：1181 (表示 1.181 N·m)
    /// ```
    pub fn torque_raw(&self) -> i32 {
        (self.torque(None) * 1000.0).round() as i32
    }
}

impl TryFrom<PiperFrame> for JointDriverHighSpeedFeedback {
    type Error = ProtocolError;

    fn try_from(frame: PiperFrame) -> Result<Self, Self::Error> {
        // 验证 CAN ID 范围（0x251~0x256）
        if frame.id < ID_JOINT_DRIVER_HIGH_SPEED_BASE
            || frame.id > ID_JOINT_DRIVER_HIGH_SPEED_BASE + 5
        {
            return Err(ProtocolError::InvalidCanId { id: frame.id });
        }

        // 从 CAN ID 推导关节索引（0x251 -> 1, 0x252 -> 2, ..., 0x256 -> 6）
        let joint_index = (frame.id - ID_JOINT_DRIVER_HIGH_SPEED_BASE + 1) as u8;

        // 验证数据长度
        if frame.len < 8 {
            return Err(ProtocolError::InvalidLength {
                expected: 8,
                actual: frame.len as usize,
            });
        }

        // 解析速度（Byte 0-1，大端字节序，i16）
        let speed_bytes = [frame.data[0], frame.data[1]];
        let speed_rad_s = bytes_to_i16_be(speed_bytes);

        // 解析电流（Byte 2-3，大端字节序，i16，支持负值表示反向电流）
        let current_bytes = [frame.data[2], frame.data[3]];
        let current_a = bytes_to_i16_be(current_bytes);

        // 解析位置（Byte 4-7，大端字节序，i32）
        let position_bytes = [frame.data[4], frame.data[5], frame.data[6], frame.data[7]];
        let position_rad = bytes_to_i32_be(position_bytes);

        Ok(Self {
            joint_index,
            speed_rad_s,
            current_a,
            position_rad,
        })
    }
}

// ============================================================================
// 关节驱动器低速反馈结构体
// ============================================================================

/// 驱动器状态位域（Byte 5: 8 位）
///
/// 协议定义（Motorola MSB 高位在前）：
/// - Bit 0: 电源电压是否过低（0：正常 1：过低）
/// - Bit 1: 电机是否过温（0：正常 1：过温）
/// - Bit 2: 驱动器是否过流（0：正常 1：过流）
/// - Bit 3: 驱动器是否过温（0：正常 1：过温）
/// - Bit 4: 碰撞保护状态（0：正常 1：触发保护）
/// - Bit 5: 驱动器错误状态（0：正常 1：错误）
/// - Bit 6: 驱动器使能状态（0：失能 1：使能）
/// - Bit 7: 堵转保护状态（0：正常 1：触发保护）
#[bitsize(8)]
#[derive(FromBits, DebugBits, Clone, Copy, Default)]
pub struct DriverStatus {
    pub voltage_low: bool,          // Bit 0: 0正常 1过低
    pub motor_over_temp: bool,      // Bit 1: 0正常 1过温
    pub driver_over_current: bool,  // Bit 2: 0正常 1过流
    pub driver_over_temp: bool,     // Bit 3: 0正常 1过温
    pub collision_protection: bool, // Bit 4: 0正常 1触发保护
    pub driver_error: bool,         // Bit 5: 0正常 1错误
    pub enabled: bool,              // Bit 6: 0失能 1使能
    pub stall_protection: bool,     // Bit 7: 0正常 1触发保护
}

/// 关节驱动器低速反馈 (0x261~0x266)
///
/// 包含电压、温度、驱动器状态和母线电流反馈。
/// - 电压单位：0.1V（原始值）
/// - 温度单位：1℃（原始值）
/// - 电流单位：0.001A（原始值）
#[derive(Debug, Clone, Copy, Default)]
pub struct JointDriverLowSpeedFeedback {
    pub joint_index: u8,      // 从 ID 推导：0x261 -> 1, 0x262 -> 2, ...
    pub voltage: u16,         // Byte 0-1: 电压，单位 0.1V
    pub driver_temp: i16,     // Byte 2-3: 驱动器温度，单位 1℃
    pub motor_temp: i8,       // Byte 4: 电机温度，单位 1℃
    pub status: DriverStatus, // Byte 5: 驱动器状态位域
    pub bus_current: u16,     // Byte 6-7: 母线电流，单位 0.001A
}

impl JointDriverLowSpeedFeedback {
    /// 获取电压原始值（0.1V 单位）
    pub fn voltage_raw(&self) -> u16 {
        self.voltage
    }

    /// 获取驱动器温度原始值（1℃ 单位）
    pub fn driver_temp_raw(&self) -> i16 {
        self.driver_temp
    }

    /// 获取电机温度原始值（1℃ 单位）
    pub fn motor_temp_raw(&self) -> i8 {
        self.motor_temp
    }

    /// 获取母线电流原始值（0.001A 单位）
    pub fn bus_current_raw(&self) -> u16 {
        self.bus_current
    }

    /// 获取电压（V）
    pub fn voltage(&self) -> f64 {
        self.voltage as f64 / 10.0
    }

    /// 获取驱动器温度（℃）
    pub fn driver_temp(&self) -> f64 {
        self.driver_temp as f64
    }

    /// 获取电机温度（℃）
    pub fn motor_temp(&self) -> f64 {
        self.motor_temp as f64
    }

    /// 获取母线电流（A）
    pub fn bus_current(&self) -> f64 {
        self.bus_current as f64 / 1000.0
    }
}

impl TryFrom<PiperFrame> for JointDriverLowSpeedFeedback {
    type Error = ProtocolError;

    fn try_from(frame: PiperFrame) -> Result<Self, Self::Error> {
        // 从 CAN ID 推导关节序号
        let joint_index = (frame.id - ID_JOINT_DRIVER_LOW_SPEED_BASE + 1) as u8;
        if !(1..=6).contains(&joint_index) {
            return Err(ProtocolError::InvalidCanId { id: frame.id });
        }

        // 验证数据长度
        if frame.len < 8 {
            return Err(ProtocolError::InvalidLength {
                expected: 8,
                actual: frame.len as usize,
            });
        }

        // 处理大端字节序
        let voltage_bytes = [frame.data[0], frame.data[1]];
        let voltage = u16::from_be_bytes(voltage_bytes);

        let driver_temp_bytes = [frame.data[2], frame.data[3]];
        let driver_temp = bytes_to_i16_be(driver_temp_bytes);

        let motor_temp = frame.data[4] as i8;

        // 使用 bilge 解析位域（Byte 5）
        let status = DriverStatus::from(u8::new(frame.data[5]));

        let bus_current_bytes = [frame.data[6], frame.data[7]];
        let bus_current = u16::from_be_bytes(bus_current_bytes);

        Ok(Self {
            joint_index,
            voltage,
            driver_temp,
            motor_temp,
            status,
            bus_current,
        })
    }
}

// ============================================================================
// 关节末端速度/加速度反馈结构体
// ============================================================================

/// 关节末端速度/加速度反馈 (0x481~0x486)
///
/// 包含关节末端线速度、角速度、线加速度和角加速度反馈。
/// - 线速度单位：0.001m/s（原始值）
/// - 角速度单位：0.001rad/s（原始值）
/// - 线加速度单位：0.001m/s²（原始值）
/// - 角加速度单位：0.001rad/s²（原始值）
///
/// 注意：这是"末端"速度和加速度，不是关节本身的速度和加速度。
#[derive(Debug, Clone, Copy, Default)]
pub struct JointEndVelocityAccelFeedback {
    pub joint_index: u8,                 // 从 ID 推导：0x481 -> 1, 0x482 -> 2, ...
    pub linear_velocity_m_s_raw: u16,    // Byte 0-1: 末端线速度，单位 0.001m/s
    pub angular_velocity_rad_s_raw: u16, // Byte 2-3: 末端角速度，单位 0.001rad/s
    pub linear_accel_m_s2_raw: u16,      // Byte 4-5: 末端线加速度，单位 0.001m/s²
    pub angular_accel_rad_s2_raw: u16,   // Byte 6-7: 末端角加速度，单位 0.001rad/s²
}

impl JointEndVelocityAccelFeedback {
    /// 获取末端线速度原始值（0.001m/s 单位）
    pub fn linear_velocity_raw(&self) -> u16 {
        self.linear_velocity_m_s_raw
    }

    /// 获取末端角速度原始值（0.001rad/s 单位）
    pub fn angular_velocity_raw(&self) -> u16 {
        self.angular_velocity_rad_s_raw
    }

    /// 获取末端线加速度原始值（0.001m/s² 单位）
    pub fn linear_accel_raw(&self) -> u16 {
        self.linear_accel_m_s2_raw
    }

    /// 获取末端角加速度原始值（0.001rad/s² 单位）
    pub fn angular_accel_raw(&self) -> u16 {
        self.angular_accel_rad_s2_raw
    }

    /// 获取末端线速度（m/s）
    pub fn linear_velocity(&self) -> f64 {
        self.linear_velocity_m_s_raw as f64 / 1000.0
    }

    /// 获取末端角速度（rad/s）
    pub fn angular_velocity(&self) -> f64 {
        self.angular_velocity_rad_s_raw as f64 / 1000.0
    }

    /// 获取末端线加速度（m/s²）
    pub fn linear_accel(&self) -> f64 {
        self.linear_accel_m_s2_raw as f64 / 1000.0
    }

    /// 获取末端角加速度（rad/s²）
    pub fn angular_accel(&self) -> f64 {
        self.angular_accel_rad_s2_raw as f64 / 1000.0
    }
}

impl TryFrom<PiperFrame> for JointEndVelocityAccelFeedback {
    type Error = ProtocolError;

    fn try_from(frame: PiperFrame) -> Result<Self, Self::Error> {
        // 验证 CAN ID 范围（0x481~0x486）
        if frame.id < ID_JOINT_END_VELOCITY_ACCEL_BASE
            || frame.id > ID_JOINT_END_VELOCITY_ACCEL_BASE + 5
        {
            return Err(ProtocolError::InvalidCanId { id: frame.id });
        }

        // 从 CAN ID 推导关节序号（0x481 -> 1, 0x482 -> 2, ..., 0x486 -> 6）
        let joint_index = (frame.id - ID_JOINT_END_VELOCITY_ACCEL_BASE + 1) as u8;

        // 验证数据长度（需要 8 字节：线速度 2 + 角速度 2 + 线加速度 2 + 角加速度 2）
        if frame.len < 8 {
            return Err(ProtocolError::InvalidLength {
                expected: 8,
                actual: frame.len as usize,
            });
        }

        // 处理大端字节序（所有字段都是 uint16）
        let linear_velocity_bytes = [frame.data[0], frame.data[1]];
        let linear_velocity_m_s_raw = u16::from_be_bytes(linear_velocity_bytes);

        let angular_velocity_bytes = [frame.data[2], frame.data[3]];
        let angular_velocity_rad_s_raw = u16::from_be_bytes(angular_velocity_bytes);

        let linear_accel_bytes = [frame.data[4], frame.data[5]];
        let linear_accel_m_s2_raw = u16::from_be_bytes(linear_accel_bytes);

        let angular_accel_bytes = [frame.data[6], frame.data[7]];
        let angular_accel_rad_s2_raw = u16::from_be_bytes(angular_accel_bytes);

        Ok(Self {
            joint_index,
            linear_velocity_m_s_raw,
            angular_velocity_rad_s_raw,
            linear_accel_m_s2_raw,
            angular_accel_rad_s2_raw,
        })
    }
}

// ============================================================================
// 夹爪反馈结构体
// ============================================================================

/// 夹爪状态位域（Byte 6: 8 位）
///
/// 协议定义（Motorola MSB 高位在前）：
/// - Bit 0: 电源电压是否过低（0：正常 1：过低）
/// - Bit 1: 电机是否过温（0：正常 1：过温）
/// - Bit 2: 驱动器是否过流（0：正常 1：过流）
/// - Bit 3: 驱动器是否过温（0：正常 1：过温）
/// - Bit 4: 传感器状态（0：正常 1：异常）
/// - Bit 5: 驱动器错误状态（0：正常 1：错误）
/// - Bit 6: 驱动器使能状态（**1：使能 0：失能**，注意：反向逻辑）
/// - Bit 7: 回零状态（0：没有回零 1：已经回零）
#[bitsize(8)]
#[derive(FromBits, DebugBits, Clone, Copy, Default)]
pub struct GripperStatus {
    pub voltage_low: bool,         // Bit 0: 0正常 1过低
    pub motor_over_temp: bool,     // Bit 1: 0正常 1过温
    pub driver_over_current: bool, // Bit 2: 0正常 1过流
    pub driver_over_temp: bool,    // Bit 3: 0正常 1过温
    pub sensor_error: bool,        // Bit 4: 0正常 1异常
    pub driver_error: bool,        // Bit 5: 0正常 1错误
    pub enabled: bool,             // Bit 6: **1使能 0失能**（注意：反向逻辑，与通常相反）
    pub homed: bool,               // Bit 7: 0没有回零 1已经回零
}

/// 夹爪反馈指令 (0x2A8)
///
/// 包含夹爪行程、扭矩和状态反馈。
/// - 行程单位：0.001mm（原始值）
/// - 扭矩单位：0.001N·m（原始值）
#[derive(Debug, Clone, Copy, Default)]
pub struct GripperFeedback {
    pub travel_mm: i32, // Byte 0-3: 单位 0.001mm
    pub torque_nm: i16, // Byte 4-5: 单位 0.001N·m（牛·米）
    pub status: GripperStatus, // Byte 6: 位域
                        // Byte 7: 保留
}

impl GripperFeedback {
    /// 获取行程原始值（0.001mm 单位）
    pub fn travel_raw(&self) -> i32 {
        self.travel_mm
    }

    /// 获取扭矩原始值（0.001N·m 单位）
    pub fn torque_raw(&self) -> i16 {
        self.torque_nm
    }

    /// 获取行程（mm）
    pub fn travel(&self) -> f64 {
        self.travel_mm as f64 / 1000.0
    }

    /// 获取扭矩（N·m）
    pub fn torque(&self) -> f64 {
        self.torque_nm as f64 / 1000.0
    }
}

impl TryFrom<PiperFrame> for GripperFeedback {
    type Error = ProtocolError;

    fn try_from(frame: PiperFrame) -> Result<Self, Self::Error> {
        // 验证 CAN ID
        if frame.id != ID_GRIPPER_FEEDBACK {
            return Err(ProtocolError::InvalidCanId { id: frame.id });
        }

        // 验证数据长度
        if frame.len < 7 {
            return Err(ProtocolError::InvalidLength {
                expected: 7,
                actual: frame.len as usize,
            });
        }

        // 处理大端字节序
        let travel_bytes = [frame.data[0], frame.data[1], frame.data[2], frame.data[3]];
        let travel_mm = bytes_to_i32_be(travel_bytes);

        let torque_bytes = [frame.data[4], frame.data[5]];
        let torque_nm = bytes_to_i16_be(torque_bytes);

        // 使用 bilge 解析位域
        let status = GripperStatus::from(u8::new(frame.data[6]));

        Ok(Self {
            travel_mm,
            torque_nm,
            status,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_control_mode_from_u8() {
        assert_eq!(ControlMode::from(0x00), ControlMode::Standby);
        assert_eq!(ControlMode::from(0x01), ControlMode::CanControl);
        assert_eq!(ControlMode::from(0x02), ControlMode::Teach);
        assert_eq!(ControlMode::from(0x03), ControlMode::Ethernet);
        assert_eq!(ControlMode::from(0x04), ControlMode::Wifi);
        assert_eq!(ControlMode::from(0x05), ControlMode::Remote);
        assert_eq!(ControlMode::from(0x06), ControlMode::LinkTeach);
        assert_eq!(ControlMode::from(0x07), ControlMode::OfflineTrajectory);
        // 无效值应该返回默认值
        assert_eq!(ControlMode::from(0xFF), ControlMode::Standby);
    }

    #[test]
    fn test_robot_status_from_u8() {
        assert_eq!(RobotStatus::from(0x00), RobotStatus::Normal);
        assert_eq!(RobotStatus::from(0x01), RobotStatus::EmergencyStop);
        assert_eq!(RobotStatus::from(0x0F), RobotStatus::ResistorOverTemp);
        // 测试所有值
        for i in 0x00..=0x0F {
            let status = RobotStatus::from(i);
            assert_eq!(status as u8, i);
        }
    }

    #[test]
    fn test_move_mode_from_u8() {
        assert_eq!(MoveMode::from(0x00), MoveMode::MoveP);
        assert_eq!(MoveMode::from(0x01), MoveMode::MoveJ);
        assert_eq!(MoveMode::from(0x02), MoveMode::MoveL);
        assert_eq!(MoveMode::from(0x03), MoveMode::MoveC);
        assert_eq!(MoveMode::from(0x04), MoveMode::MoveM);
        assert_eq!(MoveMode::from(0x05), MoveMode::MoveCpv);
        // 无效值应该返回默认值
        assert_eq!(MoveMode::from(0xFF), MoveMode::MoveP);
    }

    #[test]
    fn test_move_mode_cpv() {
        assert_eq!(MoveMode::from(0x05), MoveMode::MoveCpv);
        assert_eq!(MoveMode::MoveCpv as u8, 0x05);
    }

    #[test]
    fn test_move_mode_all_values() {
        // 验证所有枚举值
        for (value, expected) in [
            (0x00, MoveMode::MoveP),
            (0x01, MoveMode::MoveJ),
            (0x02, MoveMode::MoveL),
            (0x03, MoveMode::MoveC),
            (0x04, MoveMode::MoveM),
            (0x05, MoveMode::MoveCpv),
        ] {
            assert_eq!(MoveMode::from(value), expected);
            assert_eq!(expected as u8, value);
        }
    }

    #[test]
    fn test_teach_status_from_u8() {
        assert_eq!(TeachStatus::from(0x00), TeachStatus::Closed);
        assert_eq!(TeachStatus::from(0x01), TeachStatus::StartRecord);
        assert_eq!(TeachStatus::from(0x07), TeachStatus::MoveToStart);
        // 测试所有值
        for i in 0x00..=0x07 {
            let status = TeachStatus::from(i);
            assert_eq!(status as u8, i);
        }
    }

    #[test]
    fn test_motion_status_from_u8() {
        assert_eq!(MotionStatus::from(0x00), MotionStatus::Arrived);
        assert_eq!(MotionStatus::from(0x01), MotionStatus::NotArrived);
        // 无效值应该返回默认值（Arrived 被标记为 #[default]）
        assert_eq!(MotionStatus::from(0xFF), MotionStatus::Arrived);
    }

    #[test]
    fn test_enum_values_match_protocol() {
        // 验证枚举值与协议文档一致
        assert_eq!(ControlMode::Standby as u8, 0x00);
        assert_eq!(ControlMode::CanControl as u8, 0x01);
        assert_eq!(ControlMode::Remote as u8, 0x05);
        assert_eq!(ControlMode::LinkTeach as u8, 0x06);

        assert_eq!(RobotStatus::Normal as u8, 0x00);
        assert_eq!(RobotStatus::EmergencyStop as u8, 0x01);
        assert_eq!(RobotStatus::ResistorOverTemp as u8, 0x0F);

        assert_eq!(MoveMode::MoveP as u8, 0x00);
        assert_eq!(MoveMode::MoveM as u8, 0x04);
        assert_eq!(MoveMode::MoveCpv as u8, 0x05);
    }

    // ========================================================================
    // 位域结构测试 - 验证 bilge 位序是否符合协议要求
    // ========================================================================

    /// 验证 bilge 的位序是否符合协议要求
    ///
    /// 协议要求：
    /// - Bit 0: 1号关节
    /// - Bit 1: 2号关节
    /// - Bit 2: 3号关节
    /// - Bit 3: 4号关节
    /// - Bit 4: 5号关节
    /// - Bit 5: 6号关节
    ///
    /// 如果只有 1号关节超限位，字节值应该是 0b0000_0001 = 0x01
    /// 如果只有 2号关节超限位，字节值应该是 0b0000_0010 = 0x02
    /// 如果 1号和2号关节都超限位，字节值应该是 0b0000_0011 = 0x03
    #[test]
    fn test_fault_code_angle_limit_bit_order() {
        // 测试：只有 1号关节超限位
        // 协议：Bit 0 = 1，其他位 = 0
        // 期望字节值：0b0000_0001 = 0x01
        let byte = 0x01;
        let fault = FaultCodeAngleLimit::from(u8::new(byte));
        assert!(fault.joint1_limit(), "1号关节应该超限位");
        assert!(!fault.joint2_limit(), "2号关节应该正常");
        assert!(!fault.joint3_limit(), "3号关节应该正常");
        assert!(!fault.joint4_limit(), "4号关节应该正常");
        assert!(!fault.joint5_limit(), "5号关节应该正常");
        assert!(!fault.joint6_limit(), "6号关节应该正常");

        // 测试：只有 2号关节超限位
        // 协议：Bit 1 = 1，其他位 = 0
        // 期望字节值：0b0000_0010 = 0x02
        let byte = 0x02;
        let fault = FaultCodeAngleLimit::from(u8::new(byte));
        assert!(!fault.joint1_limit(), "1号关节应该正常");
        assert!(fault.joint2_limit(), "2号关节应该超限位");
        assert!(!fault.joint3_limit(), "3号关节应该正常");

        // 测试：1号和2号关节都超限位
        // 协议：Bit 0 = 1, Bit 1 = 1，其他位 = 0
        // 期望字节值：0b0000_0011 = 0x03
        let byte = 0x03;
        let fault = FaultCodeAngleLimit::from(u8::new(byte));
        assert!(fault.joint1_limit(), "1号关节应该超限位");
        assert!(fault.joint2_limit(), "2号关节应该超限位");
        assert!(!fault.joint3_limit(), "3号关节应该正常");

        // 测试：所有关节都超限位（Bit 0-5 = 1）
        // 期望字节值：0b0011_1111 = 0x3F
        let byte = 0x3F;
        let fault = FaultCodeAngleLimit::from(u8::new(byte));
        assert!(fault.joint1_limit(), "1号关节应该超限位");
        assert!(fault.joint2_limit(), "2号关节应该超限位");
        assert!(fault.joint3_limit(), "3号关节应该超限位");
        assert!(fault.joint4_limit(), "4号关节应该超限位");
        assert!(fault.joint5_limit(), "5号关节应该超限位");
        assert!(fault.joint6_limit(), "6号关节应该超限位");
    }

    #[test]
    fn test_fault_code_angle_limit_encode() {
        // 创建位域结构：设置 1号、3号、5号关节超限位
        // 协议：Bit 0 = 1, Bit 2 = 1, Bit 4 = 1
        // 期望字节值：0b0001_0101 = 0x15
        let mut fault = FaultCodeAngleLimit::from(u8::new(0));
        fault.set_joint1_limit(true);
        fault.set_joint3_limit(true);
        fault.set_joint5_limit(true);

        // 编码回 u8
        let encoded = u8::from(fault).value();
        assert_eq!(encoded, 0x15, "编码值应该是 0x15 (Bit 0, 2, 4 = 1)");

        // 验证各个位
        assert!(fault.joint1_limit());
        assert!(!fault.joint2_limit());
        assert!(fault.joint3_limit());
        assert!(!fault.joint4_limit());
        assert!(fault.joint5_limit());
        assert!(!fault.joint6_limit());
    }

    #[test]
    fn test_fault_code_angle_limit_roundtrip() {
        // 测试编码-解码循环
        // 设置关节3,4,5,6超限位（Bit 2,3,4,5 = 1）
        // 期望字节值：0b0011_1100 = 0x3C
        let original_byte = 0x3C;
        let fault = FaultCodeAngleLimit::from(u8::new(original_byte));
        let encoded = u8::from(fault).value();

        // 验证解析
        assert!(!fault.joint1_limit());
        assert!(!fault.joint2_limit());
        assert!(fault.joint3_limit());
        assert!(fault.joint4_limit());
        assert!(fault.joint5_limit());
        assert!(fault.joint6_limit());

        // 验证编码（保留位可能被清零，所以只比较有效位）
        assert_eq!(encoded & 0b0011_1111, original_byte & 0b0011_1111);
    }

    #[test]
    fn test_fault_code_comm_error_bit_order() {
        // 测试：只有 1号关节通信异常
        // 协议：Bit 0 = 1，其他位 = 0
        // 期望字节值：0b0000_0001 = 0x01
        let byte = 0x01;
        let fault = FaultCodeCommError::from(u8::new(byte));
        assert!(fault.joint1_comm_error(), "1号关节应该通信异常");
        assert!(!fault.joint2_comm_error(), "2号关节应该正常");

        // 测试：只有 2号关节通信异常
        // 协议：Bit 1 = 1，其他位 = 0
        // 期望字节值：0b0000_0010 = 0x02
        let byte = 0x02;
        let fault = FaultCodeCommError::from(u8::new(byte));
        assert!(!fault.joint1_comm_error(), "1号关节应该正常");
        assert!(fault.joint2_comm_error(), "2号关节应该通信异常");
    }

    #[test]
    fn test_fault_code_comm_error_encode() {
        // 设置 2号和6号关节通信异常
        // 协议：Bit 1 = 1, Bit 5 = 1
        // 期望字节值：0b0010_0010 = 0x22
        let mut fault = FaultCodeCommError::from(u8::new(0));
        fault.set_joint2_comm_error(true);
        fault.set_joint6_comm_error(true);

        let encoded = u8::from(fault).value();
        assert_eq!(encoded, 0x22, "编码值应该是 0x22 (Bit 1, 5 = 1)");
    }

    #[test]
    fn test_fault_code_comm_error_all_joints() {
        // 测试所有关节都通信异常
        // 协议：Bit 0-5 = 1
        // 期望字节值：0b0011_1111 = 0x3F
        let mut fault = FaultCodeCommError::from(u8::new(0));
        fault.set_joint1_comm_error(true);
        fault.set_joint2_comm_error(true);
        fault.set_joint3_comm_error(true);
        fault.set_joint4_comm_error(true);
        fault.set_joint5_comm_error(true);
        fault.set_joint6_comm_error(true);

        let encoded = u8::from(fault).value();
        assert_eq!(encoded & 0b0011_1111, 0x3F, "前6位应该都是1");
    }

    // ========================================================================
    // RobotStatusFeedback 测试
    // ========================================================================

    #[test]
    fn test_robot_status_feedback_parse() {
        let frame = PiperFrame::new_standard(
            ID_ROBOT_STATUS as u16,
            &[
                0x01,        // Byte 0: CAN指令控制模式
                0x00,        // Byte 1: 正常
                0x01,        // Byte 2: MOVE J
                0x00,        // Byte 3: 示教关闭
                0x00,        // Byte 4: 到达指定点位
                0x05,        // Byte 5: 轨迹点索引 5
                0b0011_1111, // Byte 6: 所有关节角度超限位（Bit 0-5 = 1）
                0b0000_0000, // Byte 7: 无通信异常
            ],
        );

        let status = RobotStatusFeedback::try_from(frame).unwrap();

        assert_eq!(status.control_mode, ControlMode::CanControl);
        assert_eq!(status.robot_status, RobotStatus::Normal);
        assert_eq!(status.move_mode, MoveMode::MoveJ);
        assert_eq!(status.teach_status, TeachStatus::Closed);
        assert_eq!(status.motion_status, MotionStatus::Arrived);
        assert_eq!(status.trajectory_point_index, 5);
        assert!(status.fault_code_angle_limit.joint1_limit());
        assert!(status.fault_code_angle_limit.joint6_limit());
        assert!(!status.fault_code_comm_error.joint1_comm_error());
    }

    #[test]
    fn test_robot_status_feedback_invalid_id() {
        let frame = PiperFrame::new_standard(0x999, &[0; 8]);
        let result = RobotStatusFeedback::try_from(frame);
        assert!(result.is_err());
        match result.unwrap_err() {
            ProtocolError::InvalidCanId { id } => assert_eq!(id, 0x999),
            _ => panic!("Expected InvalidCanId error"),
        }
    }

    #[test]
    fn test_robot_status_feedback_invalid_length() {
        let frame = PiperFrame::new_standard(ID_ROBOT_STATUS as u16, &[0; 4]);
        let result = RobotStatusFeedback::try_from(frame);
        assert!(result.is_err());
        match result.unwrap_err() {
            ProtocolError::InvalidLength { expected, actual } => {
                assert_eq!(expected, 8);
                assert_eq!(actual, 4);
            },
            _ => panic!("Expected InvalidLength error"),
        }
    }

    #[test]
    fn test_robot_status_feedback_all_fields() {
        // 测试所有字段的各种值
        let frame = PiperFrame::new_standard(
            ID_ROBOT_STATUS as u16,
            &[
                0x07,        // Byte 0: 离线轨迹模式
                0x0F,        // Byte 1: 释放电阻NTC过温
                0x04,        // Byte 2: MOVE M
                0x07,        // Byte 3: 运动到轨迹起点
                0x01,        // Byte 4: 未到达指定点位
                0xFF,        // Byte 5: 轨迹点索引 255
                0b0011_1111, // Byte 6: 所有关节超限位（Bit 0-5 = 1）
                0b0011_1111, // Byte 7: 所有关节通信异常（Bit 0-5 = 1）
            ],
        );

        let status = RobotStatusFeedback::try_from(frame).unwrap();

        assert_eq!(status.control_mode, ControlMode::OfflineTrajectory);
        assert_eq!(status.robot_status, RobotStatus::ResistorOverTemp);
        assert_eq!(status.move_mode, MoveMode::MoveM);
        assert_eq!(status.teach_status, TeachStatus::MoveToStart);
        assert_eq!(status.motion_status, MotionStatus::NotArrived);
        assert_eq!(status.trajectory_point_index, 0xFF);

        // 验证所有关节故障位
        assert!(status.fault_code_angle_limit.joint1_limit());
        assert!(status.fault_code_angle_limit.joint2_limit());
        assert!(status.fault_code_angle_limit.joint3_limit());
        assert!(status.fault_code_angle_limit.joint4_limit());
        assert!(status.fault_code_angle_limit.joint5_limit());
        assert!(status.fault_code_angle_limit.joint6_limit());

        // 验证所有关节通信异常位
        assert!(status.fault_code_comm_error.joint1_comm_error());
        assert!(status.fault_code_comm_error.joint2_comm_error());
        assert!(status.fault_code_comm_error.joint3_comm_error());
        assert!(status.fault_code_comm_error.joint4_comm_error());
        assert!(status.fault_code_comm_error.joint5_comm_error());
        assert!(status.fault_code_comm_error.joint6_comm_error());
    }

    // ========================================================================
    // 关节反馈测试
    // ========================================================================

    #[test]
    fn test_joint_feedback12_parse() {
        // 测试数据：J1 = 90.0° = 90000 (0.001° 单位)，J2 = -45.0° = -45000
        let j1_val = 90000i32;
        let j2_val = -45000i32;
        let mut data = [0u8; 8];
        data[0..4].copy_from_slice(&j1_val.to_be_bytes());
        data[4..8].copy_from_slice(&j2_val.to_be_bytes());
        let frame = PiperFrame::new_standard(ID_JOINT_FEEDBACK_12 as u16, &data);

        let feedback = JointFeedback12::try_from(frame).unwrap();
        assert_eq!(feedback.j1_raw(), 90000);
        assert_eq!(feedback.j2_raw(), -45000);
        assert!((feedback.j1() - 90.0).abs() < 0.0001);
        assert!((feedback.j2() - (-45.0)).abs() < 0.0001);
    }

    #[test]
    fn test_joint_feedback12_physical_conversion() {
        // 测试物理量转换精度
        let frame = PiperFrame::new_standard(
            ID_JOINT_FEEDBACK_12 as u16,
            &[
                0x00, 0x00, 0x00, 0x00, // J1: 0°
                0x00, 0x00, 0x01, 0xF4, // J2: 500 (0.001° 单位) = 0.5°
            ],
        );

        let feedback = JointFeedback12::try_from(frame).unwrap();
        assert_eq!(feedback.j1(), 0.0);
        assert!((feedback.j2() - 0.5).abs() < 0.0001);

        // 测试弧度转换
        assert!((feedback.j1_rad() - 0.0).abs() < 0.0001);
        assert!((feedback.j2_rad() - (0.5 * std::f64::consts::PI / 180.0)).abs() < 0.0001);
    }

    #[test]
    fn test_joint_feedback12_boundary_values() {
        // 测试最大正值：i32::MAX / 1000 ≈ 2147483.647°
        let max_positive = i32::MAX;
        let mut data = [0u8; 8];
        data[0..4].copy_from_slice(&max_positive.to_be_bytes());
        data[4..8].copy_from_slice(&max_positive.to_be_bytes());
        let frame = PiperFrame::new_standard(ID_JOINT_FEEDBACK_12 as u16, &data);

        let feedback = JointFeedback12::try_from(frame).unwrap();
        assert_eq!(feedback.j1_raw(), max_positive);
        assert_eq!(feedback.j2_raw(), max_positive);

        // 测试最大负值：i32::MIN / 1000 ≈ -2147483.648°
        let min_negative = i32::MIN;
        let mut data = [0u8; 8];
        data[0..4].copy_from_slice(&min_negative.to_be_bytes());
        data[4..8].copy_from_slice(&min_negative.to_be_bytes());
        let frame = PiperFrame::new_standard(ID_JOINT_FEEDBACK_12 as u16, &data);

        let feedback = JointFeedback12::try_from(frame).unwrap();
        assert_eq!(feedback.j1_raw(), min_negative);
        assert_eq!(feedback.j2_raw(), min_negative);
    }

    #[test]
    fn test_joint_feedback12_invalid_id() {
        let frame = PiperFrame::new_standard(0x999, &[0; 8]);
        let result = JointFeedback12::try_from(frame);
        assert!(result.is_err());
        match result.unwrap_err() {
            ProtocolError::InvalidCanId { id } => assert_eq!(id, 0x999),
            _ => panic!("Expected InvalidCanId error"),
        }
    }

    #[test]
    fn test_joint_feedback12_invalid_length() {
        let frame = PiperFrame::new_standard(ID_JOINT_FEEDBACK_12 as u16, &[0; 4]);
        let result = JointFeedback12::try_from(frame);
        assert!(result.is_err());
        match result.unwrap_err() {
            ProtocolError::InvalidLength { expected, actual } => {
                assert_eq!(expected, 8);
                assert_eq!(actual, 4);
            },
            _ => panic!("Expected InvalidLength error"),
        }
    }

    #[test]
    fn test_joint_feedback34_parse() {
        // 测试数据：J3 = 30.0° = 30000, J4 = -60.0° = -60000
        let j3_val = 30000i32;
        let j4_val = -60000i32;
        let mut data = [0u8; 8];
        data[0..4].copy_from_slice(&j3_val.to_be_bytes());
        data[4..8].copy_from_slice(&j4_val.to_be_bytes());
        let frame = PiperFrame::new_standard(ID_JOINT_FEEDBACK_34 as u16, &data);

        let feedback = JointFeedback34::try_from(frame).unwrap();
        assert_eq!(feedback.j3_raw(), 30000);
        assert_eq!(feedback.j4_raw(), -60000);
        assert!((feedback.j3() - 30.0).abs() < 0.0001);
        assert!((feedback.j4() - (-60.0)).abs() < 0.0001);
    }

    #[test]
    fn test_joint_feedback56_parse() {
        // 测试数据：J5 = 180.0° = 180000, J6 = -90.0° = -90000
        let j5_val = 180000i32;
        let j6_val = -90000i32;
        let mut data = [0u8; 8];
        data[0..4].copy_from_slice(&j5_val.to_be_bytes());
        data[4..8].copy_from_slice(&j6_val.to_be_bytes());
        let frame = PiperFrame::new_standard(ID_JOINT_FEEDBACK_56 as u16, &data);

        let feedback = JointFeedback56::try_from(frame).unwrap();
        assert_eq!(feedback.j5_raw(), 180000);
        assert_eq!(feedback.j6_raw(), -90000);
        assert!((feedback.j5() - 180.0).abs() < 0.0001);
        assert!((feedback.j6() - (-90.0)).abs() < 0.0001);
    }

    #[test]
    fn test_joint_feedback_roundtrip() {
        // 测试编码-解码循环（通过原始值验证）
        let test_cases = vec![
            (0i32, 0i32),
            (90000i32, -45000i32),
            (i32::MAX, i32::MIN),
            (180000i32, -90000i32),
        ];

        for (j1_val, j2_val) in test_cases {
            let frame = PiperFrame::new_standard(
                ID_JOINT_FEEDBACK_12 as u16,
                &[
                    j1_val.to_be_bytes()[0],
                    j1_val.to_be_bytes()[1],
                    j1_val.to_be_bytes()[2],
                    j1_val.to_be_bytes()[3],
                    j2_val.to_be_bytes()[0],
                    j2_val.to_be_bytes()[1],
                    j2_val.to_be_bytes()[2],
                    j2_val.to_be_bytes()[3],
                ],
            );

            let feedback = JointFeedback12::try_from(frame).unwrap();
            assert_eq!(feedback.j1_raw(), j1_val);
            assert_eq!(feedback.j2_raw(), j2_val);
        }
    }

    // ========================================================================
    // 末端位姿反馈测试
    // ========================================================================

    #[test]
    fn test_end_pose_feedback1_parse() {
        // 测试数据：X = 100.0mm = 100000 (0.001mm 单位)，Y = -50.0mm = -50000
        let x_val = 100000i32;
        let y_val = -50000i32;
        let mut data = [0u8; 8];
        data[0..4].copy_from_slice(&x_val.to_be_bytes());
        data[4..8].copy_from_slice(&y_val.to_be_bytes());
        let frame = PiperFrame::new_standard(ID_END_POSE_1 as u16, &data);

        let feedback = EndPoseFeedback1::try_from(frame).unwrap();
        assert_eq!(feedback.x_raw(), 100000);
        assert_eq!(feedback.y_raw(), -50000);
        assert!((feedback.x() - 100.0).abs() < 0.0001);
        assert!((feedback.y() - (-50.0)).abs() < 0.0001);
    }

    #[test]
    fn test_end_pose_feedback1_unit_conversion() {
        // 测试单位转换（0.001mm -> mm）
        let x_val = 1234i32; // 1.234mm
        let y_val = -5678i32; // -5.678mm
        let mut data = [0u8; 8];
        data[0..4].copy_from_slice(&x_val.to_be_bytes());
        data[4..8].copy_from_slice(&y_val.to_be_bytes());
        let frame = PiperFrame::new_standard(ID_END_POSE_1 as u16, &data);

        let feedback = EndPoseFeedback1::try_from(frame).unwrap();
        assert!((feedback.x() - 1.234).abs() < 0.0001);
        assert!((feedback.y() - (-5.678)).abs() < 0.0001);
    }

    #[test]
    fn test_end_pose_feedback1_invalid_id() {
        let frame = PiperFrame::new_standard(0x999, &[0; 8]);
        let result = EndPoseFeedback1::try_from(frame);
        assert!(result.is_err());
    }

    #[test]
    fn test_end_pose_feedback2_parse() {
        // 测试数据：Z = 200.0mm = 200000, RX = 45.0° = 45000
        let z_val = 200000i32;
        let rx_val = 45000i32;
        let mut data = [0u8; 8];
        data[0..4].copy_from_slice(&z_val.to_be_bytes());
        data[4..8].copy_from_slice(&rx_val.to_be_bytes());
        let frame = PiperFrame::new_standard(ID_END_POSE_2 as u16, &data);

        let feedback = EndPoseFeedback2::try_from(frame).unwrap();
        assert_eq!(feedback.z_raw(), 200000);
        assert_eq!(feedback.rx_raw(), 45000);
        assert!((feedback.z() - 200.0).abs() < 0.0001);
        assert!((feedback.rx() - 45.0).abs() < 0.0001);
        assert!((feedback.rx_rad() - (45.0 * std::f64::consts::PI / 180.0)).abs() < 0.0001);
    }

    #[test]
    fn test_end_pose_feedback3_parse() {
        // 测试数据：RY = 90.0° = 90000, RZ = -30.0° = -30000
        let ry_val = 90000i32;
        let rz_val = -30000i32;
        let mut data = [0u8; 8];
        data[0..4].copy_from_slice(&ry_val.to_be_bytes());
        data[4..8].copy_from_slice(&rz_val.to_be_bytes());
        let frame = PiperFrame::new_standard(ID_END_POSE_3 as u16, &data);

        let feedback = EndPoseFeedback3::try_from(frame).unwrap();
        assert_eq!(feedback.ry_raw(), 90000);
        assert_eq!(feedback.rz_raw(), -30000);
        assert!((feedback.ry() - 90.0).abs() < 0.0001);
        assert!((feedback.rz() - (-30.0)).abs() < 0.0001);
    }

    // ========================================================================
    // 关节驱动器高速反馈测试
    // ========================================================================

    #[test]
    fn test_joint_driver_high_speed_feedback_parse() {
        // 测试数据：
        // - 关节1 (ID: 0x251)
        // - 速度: 1.5 rad/s = 1500 (0.001rad/s 单位)
        // - 电流: 2.5 A = 2500 (0.001A 单位)
        // - 位置: 根据协议单位是 rad（signed int32），直接返回 i32 转 f64
        let speed_val = 1500i16;
        let current_val = 2500u16;
        let position_val = 1000000i32; // 测试值，实际单位需要根据硬件确认

        let mut data = [0u8; 8];
        data[0..2].copy_from_slice(&speed_val.to_be_bytes());
        data[2..4].copy_from_slice(&current_val.to_be_bytes());
        data[4..8].copy_from_slice(&position_val.to_be_bytes());

        let frame = PiperFrame::new_standard(ID_JOINT_DRIVER_HIGH_SPEED_BASE as u16, &data);
        let feedback = JointDriverHighSpeedFeedback::try_from(frame).unwrap();

        assert_eq!(feedback.joint_index, 1);
        assert_eq!(feedback.speed_raw(), 1500);
        assert_eq!(feedback.current_raw(), 2500i16); // 电流现在是有符号 i16
        assert_eq!(feedback.position_raw(), 1000000);
        assert!((feedback.speed() - 1.5).abs() < 0.0001);
        assert!((feedback.current() - 2.5).abs() < 0.0001);
        // 位置单位已弃用：position() 方法单位未确认（rad vs mrad）
        // 应该使用 Observer::get_joint_position() 或 JointFeedback12::j1_rad() 获取已验证的位置
    }

    #[test]
    fn test_joint_driver_high_speed_feedback_all_joints() {
        // 测试所有 6 个关节的 ID 识别
        for joint_id in 1..=6 {
            let can_id = ID_JOINT_DRIVER_HIGH_SPEED_BASE + (joint_id - 1);
            let frame = PiperFrame::new_standard(can_id as u16, &[0; 8]);
            let feedback = JointDriverHighSpeedFeedback::try_from(frame).unwrap();
            assert_eq!(feedback.joint_index, joint_id as u8);
        }
    }

    #[test]
    fn test_joint_driver_high_speed_feedback_physical_conversion() {
        // 测试物理量转换
        let speed_val = 3141i16; // 3.141 rad/s
        let current_val = 5000u16; // 5.0 A
        let position_val = 3141592i32; // 约 π rad（如果按 0.001rad 单位）

        let mut data = [0u8; 8];
        data[0..2].copy_from_slice(&speed_val.to_be_bytes());
        data[2..4].copy_from_slice(&current_val.to_be_bytes());
        data[4..8].copy_from_slice(&position_val.to_be_bytes());

        let frame = PiperFrame::new_standard(ID_JOINT_DRIVER_HIGH_SPEED_BASE as u16, &data);
        let feedback = JointDriverHighSpeedFeedback::try_from(frame).unwrap();

        assert!((feedback.speed() - std::f64::consts::PI).abs() < 0.001);
        assert!((feedback.current() - 5.0).abs() < 0.001);
        // 位置单位已弃用：position() 方法单位未确认（rad vs mrad）
        // 原始值可以正确获取
        assert_eq!(feedback.position_raw(), position_val);
    }

    #[test]
    fn test_joint_driver_high_speed_feedback_boundary_values() {
        // 测试边界值
        // 最大速度：i16::MAX = 32767 = 32.767 rad/s
        let speed_val = i16::MAX;
        let current_val = i16::MAX; // 32767 = 32.767 A（最大正电流）
        let position_val = i32::MAX;

        let mut data = [0u8; 8];
        data[0..2].copy_from_slice(&speed_val.to_be_bytes());
        data[2..4].copy_from_slice(&current_val.to_be_bytes());
        data[4..8].copy_from_slice(&position_val.to_be_bytes());

        let frame = PiperFrame::new_standard(ID_JOINT_DRIVER_HIGH_SPEED_BASE as u16, &data);
        let feedback = JointDriverHighSpeedFeedback::try_from(frame).unwrap();

        assert_eq!(feedback.speed_raw(), i16::MAX);
        assert_eq!(feedback.current_raw(), i16::MAX);
        assert_eq!(feedback.position_raw(), i32::MAX);
    }

    #[test]
    fn test_joint_driver_high_speed_feedback_invalid_id() {
        // 测试无效的 CAN ID
        let frame = PiperFrame::new_standard(0x250, &[0; 8]); // 小于 0x251
        let result = JointDriverHighSpeedFeedback::try_from(frame);
        assert!(result.is_err());

        let frame = PiperFrame::new_standard(0x257, &[0; 8]); // 大于 0x256
        let result = JointDriverHighSpeedFeedback::try_from(frame);
        assert!(result.is_err());
    }

    #[test]
    fn test_joint_driver_high_speed_feedback_invalid_length() {
        let frame = PiperFrame::new_standard(ID_JOINT_DRIVER_HIGH_SPEED_BASE as u16, &[0; 4]);
        let result = JointDriverHighSpeedFeedback::try_from(frame);
        assert!(result.is_err());
        match result.unwrap_err() {
            ProtocolError::InvalidLength { expected, actual } => {
                assert_eq!(expected, 8);
                assert_eq!(actual, 4);
            },
            _ => panic!("Expected InvalidLength error"),
        }
    }

    #[test]
    fn test_joint_driver_high_speed_feedback_negative_speed() {
        // 测试负速度（反向旋转）
        let speed_val = -1000i16; // -1.0 rad/s
        let current_val = 1000u16; // 1.0 A
        let position_val = 0i32;

        let mut data = [0u8; 8];
        data[0..2].copy_from_slice(&speed_val.to_be_bytes());
        data[2..4].copy_from_slice(&current_val.to_be_bytes());
        data[4..8].copy_from_slice(&position_val.to_be_bytes());

        let frame = PiperFrame::new_standard(ID_JOINT_DRIVER_HIGH_SPEED_BASE as u16, &data);
        let feedback = JointDriverHighSpeedFeedback::try_from(frame).unwrap();

        assert_eq!(feedback.speed_raw(), -1000);
        assert!((feedback.speed() - (-1.0)).abs() < 0.0001);
    }

    #[test]
    fn test_joint_driver_high_speed_feedback_torque_joints_1_3() {
        // 测试关节 1-3 的力矩计算（使用系数 1.18125）
        // 关节 1 (CAN ID: 0x251)
        let current_val = 1000u16; // 1.0 A
        let mut data = [0u8; 8];
        data[2..4].copy_from_slice(&current_val.to_be_bytes());

        let frame = PiperFrame::new_standard(ID_JOINT_DRIVER_HIGH_SPEED_BASE as u16, &data);
        let feedback = JointDriverHighSpeedFeedback::try_from(frame).unwrap();

        assert_eq!(feedback.joint_index, 1);
        let expected_torque = 1.0 * JointDriverHighSpeedFeedback::COEFFICIENT_1_3; // 1.18125 N·m
        assert!((feedback.torque(None) - expected_torque).abs() < 0.0001);
        assert_eq!(
            feedback.torque_raw(),
            (expected_torque * 1000.0).round() as i32
        );

        // 关节 2 (CAN ID: 0x252)
        let frame = PiperFrame::new_standard(ID_JOINT_DRIVER_HIGH_SPEED_BASE as u16 + 1, &data);
        let feedback = JointDriverHighSpeedFeedback::try_from(frame).unwrap();
        assert_eq!(feedback.joint_index, 2);
        assert!((feedback.torque(None) - expected_torque).abs() < 0.0001);

        // 关节 3 (CAN ID: 0x253)
        let frame = PiperFrame::new_standard(ID_JOINT_DRIVER_HIGH_SPEED_BASE as u16 + 2, &data);
        let feedback = JointDriverHighSpeedFeedback::try_from(frame).unwrap();
        assert_eq!(feedback.joint_index, 3);
        assert!((feedback.torque(None) - expected_torque).abs() < 0.0001);
    }

    #[test]
    fn test_joint_driver_high_speed_feedback_torque_joints_4_6() {
        // 测试关节 4-6 的力矩计算（使用系数 0.95844）
        // 关节 4 (CAN ID: 0x254)
        let current_val = 1000u16; // 1.0 A
        let mut data = [0u8; 8];
        data[2..4].copy_from_slice(&current_val.to_be_bytes());

        let frame = PiperFrame::new_standard(ID_JOINT_DRIVER_HIGH_SPEED_BASE as u16 + 3, &data);
        let feedback = JointDriverHighSpeedFeedback::try_from(frame).unwrap();

        assert_eq!(feedback.joint_index, 4);
        let expected_torque = 1.0 * JointDriverHighSpeedFeedback::COEFFICIENT_4_6; // 0.95844 N·m
        assert!((feedback.torque(None) - expected_torque).abs() < 0.0001);
        assert_eq!(
            feedback.torque_raw(),
            (expected_torque * 1000.0).round() as i32
        );

        // 关节 5 (CAN ID: 0x255)
        let frame = PiperFrame::new_standard(ID_JOINT_DRIVER_HIGH_SPEED_BASE as u16 + 4, &data);
        let feedback = JointDriverHighSpeedFeedback::try_from(frame).unwrap();
        assert_eq!(feedback.joint_index, 5);
        assert!((feedback.torque(None) - expected_torque).abs() < 0.0001);

        // 关节 6 (CAN ID: 0x256)
        let frame = PiperFrame::new_standard(ID_JOINT_DRIVER_HIGH_SPEED_BASE as u16 + 5, &data);
        let feedback = JointDriverHighSpeedFeedback::try_from(frame).unwrap();
        assert_eq!(feedback.joint_index, 6);
        assert!((feedback.torque(None) - expected_torque).abs() < 0.0001);
    }

    #[test]
    fn test_joint_driver_high_speed_feedback_torque_with_custom_current() {
        // 测试使用自定义电流值计算力矩
        let current_val = 2000u16; // 2.0 A（反馈中的电流）
        let custom_current = 2.5; // 自定义电流值（A）

        let mut data = [0u8; 8];
        data[2..4].copy_from_slice(&current_val.to_be_bytes());

        // 关节 1：使用反馈中的电流值
        let frame = PiperFrame::new_standard(ID_JOINT_DRIVER_HIGH_SPEED_BASE as u16, &data);
        let feedback = JointDriverHighSpeedFeedback::try_from(frame).unwrap();
        let torque_from_feedback = feedback.torque(None);
        let expected_from_feedback = 2.0 * JointDriverHighSpeedFeedback::COEFFICIENT_1_3;
        assert!((torque_from_feedback - expected_from_feedback).abs() < 0.0001);

        // 关节 1：使用自定义电流值
        let torque_from_custom = feedback.torque(Some(custom_current));
        let expected_from_custom = custom_current * JointDriverHighSpeedFeedback::COEFFICIENT_1_3;
        assert!((torque_from_custom - expected_from_custom).abs() < 0.0001);

        // 关节 4：使用自定义电流值
        let frame = PiperFrame::new_standard(ID_JOINT_DRIVER_HIGH_SPEED_BASE as u16 + 3, &data);
        let feedback = JointDriverHighSpeedFeedback::try_from(frame).unwrap();
        let torque_from_custom = feedback.torque(Some(custom_current));
        let expected_from_custom = custom_current * JointDriverHighSpeedFeedback::COEFFICIENT_4_6;
        assert!((torque_from_custom - expected_from_custom).abs() < 0.0001);
    }

    #[test]
    fn test_joint_driver_high_speed_feedback_torque_coefficients() {
        // 验证系数值与官方参考实现一致
        assert_eq!(JointDriverHighSpeedFeedback::COEFFICIENT_1_3, 1.18125);
        assert_eq!(JointDriverHighSpeedFeedback::COEFFICIENT_4_6, 0.95844);

        // 测试具体计算值（与官方参考实现的 cal_effort 方法一致）
        // 示例：current = 1000 (0.001A 单位) = 1.0 A
        // 示例：effort = 1000 * 1.18125 = 1181.25 (0.001N·m 单位) = 1.18125 N·m
        let current_val = 1000u16; // 1.0 A
        let mut data = [0u8; 8];
        data[2..4].copy_from_slice(&current_val.to_be_bytes());

        let frame = PiperFrame::new_standard(ID_JOINT_DRIVER_HIGH_SPEED_BASE as u16, &data);
        let feedback = JointDriverHighSpeedFeedback::try_from(frame).unwrap();
        let torque = feedback.torque(None);
        assert!((torque - 1.18125).abs() < 0.0001);
        assert_eq!(feedback.torque_raw(), 1181); // 四舍五入到整数
    }

    // ========================================================================
    // 夹爪反馈测试
    // ========================================================================

    #[test]
    fn test_gripper_status_bit_order() {
        // 测试：只有 Bit 0 和 Bit 6 被设置
        // Bit 0 = 1: 电压过低
        // Bit 6 = 1: 使能（注意：反向逻辑，1表示使能）
        // 期望字节值：0b0100_0001 = 0x41
        let byte = 0x41;
        let status = GripperStatus::from(u8::new(byte));

        assert!(status.voltage_low(), "电压应该过低");
        assert!(!status.motor_over_temp(), "电机应该正常");
        assert!(status.enabled(), "应该使能（Bit 6 = 1）");
        assert!(!status.homed(), "应该没有回零");
    }

    #[test]
    fn test_gripper_status_encode() {
        let mut status = GripperStatus::from(u8::new(0));
        status.set_voltage_low(true);
        status.set_enabled(true);
        status.set_homed(true);

        let encoded = u8::from(status).value();
        // Bit 0, 6, 7 = 1: 0b1100_0001 = 0xC1
        assert_eq!(encoded, 0xC1);
    }

    #[test]
    fn test_gripper_feedback_parse() {
        // 测试数据：
        // - 行程: 50.0mm = 50000 (0.001mm 单位)
        // - 扭矩: 2.5N·m = 2500 (0.001N·m 单位)
        // - 状态: 0b0100_0001 (电压过低，使能)
        let travel_val = 50000i32;
        let torque_val = 2500i16;
        let status_byte = 0x41u8;

        let mut data = [0u8; 8];
        data[0..4].copy_from_slice(&travel_val.to_be_bytes());
        data[4..6].copy_from_slice(&torque_val.to_be_bytes());
        data[6] = status_byte;

        let frame = PiperFrame::new_standard(ID_GRIPPER_FEEDBACK as u16, &data);
        let feedback = GripperFeedback::try_from(frame).unwrap();

        assert_eq!(feedback.travel_raw(), 50000);
        assert_eq!(feedback.torque_raw(), 2500);
        assert!((feedback.travel() - 50.0).abs() < 0.0001);
        assert!((feedback.torque() - 2.5).abs() < 0.0001);
        assert!(feedback.status.voltage_low());
        assert!(feedback.status.enabled());
    }

    #[test]
    fn test_gripper_feedback_invalid_id() {
        let frame = PiperFrame::new_standard(0x999, &[0; 8]);
        let result = GripperFeedback::try_from(frame);
        assert!(result.is_err());
    }

    #[test]
    fn test_gripper_feedback_invalid_length() {
        let frame = PiperFrame::new_standard(ID_GRIPPER_FEEDBACK as u16, &[0; 4]);
        let result = GripperFeedback::try_from(frame);
        assert!(result.is_err());
    }

    #[test]
    fn test_gripper_status_all_flags() {
        // 测试所有状态位
        let mut status = GripperStatus::from(u8::new(0));
        status.set_voltage_low(true);
        status.set_motor_over_temp(true);
        status.set_driver_over_current(true);
        status.set_driver_over_temp(true);
        status.set_sensor_error(true);
        status.set_driver_error(true);
        status.set_enabled(true);
        status.set_homed(true);

        let encoded = u8::from(status).value();
        assert_eq!(encoded, 0xFF); // 所有位都是1
    }

    // ========================================================================
    // 关节驱动器低速反馈测试
    // ========================================================================

    #[test]
    fn test_driver_status_bit_order() {
        // 测试：Bit 0, 2, 4, 6 被设置
        // Bit 0 = 1: 电源电压过低
        // Bit 2 = 1: 驱动器过流
        // Bit 4 = 1: 碰撞保护触发
        // Bit 6 = 1: 驱动器使能
        // 期望字节值：0b0101_0101 = 0x55
        let byte = 0x55;
        let status = DriverStatus::from(u8::new(byte));

        assert!(status.voltage_low(), "电源电压应该过低");
        assert!(!status.motor_over_temp(), "电机应该正常温度");
        assert!(status.driver_over_current(), "驱动器应该过流");
        assert!(status.collision_protection(), "碰撞保护应该触发");
        assert!(status.enabled(), "驱动器应该使能");
    }

    #[test]
    fn test_driver_status_encode() {
        let mut status = DriverStatus::from(u8::new(0));
        status.set_voltage_low(true);
        status.set_driver_over_current(true);
        status.set_collision_protection(true);
        status.set_enabled(true);

        let encoded = u8::from(status).value();
        // Bit 0, 2, 4, 6 = 1: 0b0101_0101 = 0x55
        assert_eq!(encoded, 0x55);
    }

    #[test]
    fn test_joint_driver_low_speed_feedback_parse() {
        // 测试关节 1 (0x261)
        // 电压: 24.0V = 240 (0.1V 单位)
        // 驱动器温度: 45℃ = 45 (1℃ 单位)
        // 电机温度: 50℃ = 50 (1℃ 单位)
        // 状态: 0x44 (Bit 2=过流, Bit 6=使能)
        // 母线电流: 5.0A = 5000 (0.001A 单位)
        let voltage_val = 240u16;
        let driver_temp_val = 45i16;
        let motor_temp_val = 50i8;
        let status_byte = 0x44u8; // Bit 2=过流, Bit 6=使能
        let bus_current_val = 5000u16;

        let mut data = [0u8; 8];
        data[0..2].copy_from_slice(&voltage_val.to_be_bytes());
        data[2..4].copy_from_slice(&driver_temp_val.to_be_bytes());
        data[4] = motor_temp_val as u8;
        data[5] = status_byte;
        data[6..8].copy_from_slice(&bus_current_val.to_be_bytes());

        let frame = PiperFrame::new_standard(ID_JOINT_DRIVER_LOW_SPEED_BASE as u16, &data);
        let feedback = JointDriverLowSpeedFeedback::try_from(frame).unwrap();

        assert_eq!(feedback.joint_index, 1);
        assert_eq!(feedback.voltage, 240);
        assert_eq!(feedback.driver_temp, 45);
        assert_eq!(feedback.motor_temp, 50);
        assert_eq!(feedback.bus_current, 5000);
        assert!(feedback.status.driver_over_current());
        assert!(feedback.status.enabled());
    }

    #[test]
    fn test_joint_driver_low_speed_feedback_all_joints() {
        // 测试所有 6 个关节
        for i in 1..=6 {
            let id = ID_JOINT_DRIVER_LOW_SPEED_BASE + (i - 1) as u32;
            let mut data = [0u8; 8];
            data[0..2].copy_from_slice(&240u16.to_be_bytes()); // 24.0V
            data[2..4].copy_from_slice(&45i16.to_be_bytes()); // 45℃
            data[4] = 50; // 50℃
            data[5] = 0x40; // Bit 6=使能

            let frame = PiperFrame::new_standard(id as u16, &data);
            let feedback = JointDriverLowSpeedFeedback::try_from(frame).unwrap();
            assert_eq!(feedback.joint_index, i);
        }
    }

    #[test]
    fn test_joint_driver_low_speed_feedback_conversions() {
        let mut data = [0u8; 8];
        data[0..2].copy_from_slice(&240u16.to_be_bytes()); // 24.0V
        data[2..4].copy_from_slice(&45i16.to_be_bytes()); // 45℃
        data[4] = 50; // 50℃
        data[5] = 0x40; // Bit 6=使能
        data[6..8].copy_from_slice(&5000u16.to_be_bytes()); // 5.0A

        let frame = PiperFrame::new_standard(ID_JOINT_DRIVER_LOW_SPEED_BASE as u16, &data);
        let feedback = JointDriverLowSpeedFeedback::try_from(frame).unwrap();

        assert!((feedback.voltage() - 24.0).abs() < 0.01);
        assert!((feedback.driver_temp() - 45.0).abs() < 0.01);
        assert!((feedback.motor_temp() - 50.0).abs() < 0.01);
        assert!((feedback.bus_current() - 5.0).abs() < 0.001);
    }

    #[test]
    fn test_joint_driver_low_speed_feedback_invalid_id() {
        let frame = PiperFrame::new_standard(0x999, &[0; 8]);
        let result = JointDriverLowSpeedFeedback::try_from(frame);
        assert!(result.is_err());
    }

    #[test]
    fn test_joint_driver_low_speed_feedback_invalid_length() {
        let frame = PiperFrame::new_standard(ID_JOINT_DRIVER_LOW_SPEED_BASE as u16, &[0; 4]);
        let result = JointDriverLowSpeedFeedback::try_from(frame);
        assert!(result.is_err());
    }

    // ========================================================================
    // 关节末端速度/加速度反馈测试
    // ========================================================================

    #[test]
    fn test_joint_end_velocity_accel_feedback_parse() {
        // 测试关节 1 (0x481)
        let linear_vel = 1000u16; // 单位：0.001m/s = 1.0 m/s
        let angular_vel = 5000u16; // 单位：0.001rad/s = 5.0 rad/s
        let linear_accel = 2000u16; // 单位：0.001m/s² = 2.0 m/s²
        let angular_accel = 3000u16; // 单位：0.001rad/s² = 3.0 rad/s²

        let mut data = [0u8; 8];
        data[0..2].copy_from_slice(&linear_vel.to_be_bytes());
        data[2..4].copy_from_slice(&angular_vel.to_be_bytes());
        data[4..6].copy_from_slice(&linear_accel.to_be_bytes());
        data[6..8].copy_from_slice(&angular_accel.to_be_bytes());

        let frame = PiperFrame::new_standard(ID_JOINT_END_VELOCITY_ACCEL_BASE as u16, &data);
        let feedback = JointEndVelocityAccelFeedback::try_from(frame).unwrap();

        assert_eq!(feedback.joint_index, 1);
        assert_eq!(feedback.linear_velocity_m_s_raw, 1000);
        assert_eq!(feedback.angular_velocity_rad_s_raw, 5000);
        assert_eq!(feedback.linear_accel_m_s2_raw, 2000);
        assert_eq!(feedback.angular_accel_rad_s2_raw, 3000);
    }

    #[test]
    fn test_joint_end_velocity_accel_feedback_all_joints() {
        // 测试所有 6 个关节
        for i in 1..=6 {
            let id = ID_JOINT_END_VELOCITY_ACCEL_BASE + (i - 1) as u32;
            let mut data = [0u8; 8];
            data[0..2].copy_from_slice(&1000u16.to_be_bytes());
            data[2..4].copy_from_slice(&2000u16.to_be_bytes());
            data[4..6].copy_from_slice(&3000u16.to_be_bytes());
            data[6..8].copy_from_slice(&4000u16.to_be_bytes());

            let frame = PiperFrame::new_standard(id as u16, &data);
            let feedback = JointEndVelocityAccelFeedback::try_from(frame).unwrap();
            assert_eq!(feedback.joint_index, i);
        }
    }

    #[test]
    fn test_joint_end_velocity_accel_feedback_conversions() {
        let mut data = [0u8; 8];
        data[0..2].copy_from_slice(&1000u16.to_be_bytes()); // 1.0 m/s
        data[2..4].copy_from_slice(&2000u16.to_be_bytes()); // 2.0 rad/s
        data[4..6].copy_from_slice(&3000u16.to_be_bytes()); // 3.0 m/s²
        data[6..8].copy_from_slice(&4000u16.to_be_bytes()); // 4.0 rad/s²

        let frame = PiperFrame::new_standard(ID_JOINT_END_VELOCITY_ACCEL_BASE as u16, &data);
        let feedback = JointEndVelocityAccelFeedback::try_from(frame).unwrap();

        assert!((feedback.linear_velocity() - 1.0).abs() < 0.0001);
        assert!((feedback.angular_velocity() - 2.0).abs() < 0.0001);
        assert!((feedback.linear_accel() - 3.0).abs() < 0.0001);
        assert!((feedback.angular_accel() - 4.0).abs() < 0.0001);
    }

    #[test]
    fn test_joint_end_velocity_accel_feedback_zero() {
        // 测试零值
        let data = [0u8; 8];
        let frame = PiperFrame::new_standard(ID_JOINT_END_VELOCITY_ACCEL_BASE as u16, &data);
        let feedback = JointEndVelocityAccelFeedback::try_from(frame).unwrap();

        assert_eq!(feedback.linear_velocity_m_s_raw, 0);
        assert_eq!(feedback.angular_velocity_rad_s_raw, 0);
        assert_eq!(feedback.linear_accel_m_s2_raw, 0);
        assert_eq!(feedback.angular_accel_rad_s2_raw, 0);
        assert!((feedback.linear_velocity() - 0.0).abs() < 0.0001);
        assert!((feedback.angular_velocity() - 0.0).abs() < 0.0001);
    }

    #[test]
    fn test_joint_end_velocity_accel_feedback_invalid_id() {
        let frame = PiperFrame::new_standard(0x999, &[0; 8]);
        let result = JointEndVelocityAccelFeedback::try_from(frame);
        assert!(result.is_err());
    }

    #[test]
    fn test_joint_end_velocity_accel_feedback_invalid_length() {
        // 测试长度不足（需要 8 字节）
        let frame = PiperFrame::new_standard(ID_JOINT_END_VELOCITY_ACCEL_BASE as u16, &[0; 7]);
        let result = JointEndVelocityAccelFeedback::try_from(frame);
        assert!(result.is_err());
    }
}

// ============================================================================
// 固件版本读取反馈结构体
// ============================================================================

/// 固件版本读取反馈 (0x4AF)
///
/// 用于接收机械臂固件版本信息。
/// 注意：固件版本数据可能是分多个 CAN 帧传输的，需要累积接收。
#[derive(Debug, Clone, Default)]
pub struct FirmwareReadFeedback {
    pub firmware_data: [u8; 8],
}

impl FirmwareReadFeedback {
    /// 获取固件数据原始字节
    pub fn firmware_data(&self) -> &[u8; 8] {
        &self.firmware_data
    }

    /// 尝试从累积的固件数据中解析版本字符串
    ///
    /// 固件版本字符串通常以 "S-V" 开头，后面跟版本号。
    /// 此方法会在累积数据中查找版本字符串。
    ///
    /// **注意**：与 Python SDK 对齐，固定提取 8 字节长度（从 S-V 开始，包括 S-V）。
    ///
    /// # 参数
    /// - `accumulated_data`: 累积的固件数据（可能包含多个 CAN 帧的数据）
    ///
    /// # 返回值
    /// 如果找到版本字符串，返回 `Some(String)`，否则返回 `None`
    pub fn parse_version_string(accumulated_data: &[u8]) -> Option<String> {
        // 查找 "S-V" 标记
        if let Some(version_start) = accumulated_data.windows(3).position(|w| w == b"S-V") {
            // 固定长度为 8 字节（从 S-V 开始，包括 S-V），与 Python SDK 对齐
            let version_length = 8;
            // 确保不会超出数组长度
            let version_end = (version_start + version_length).min(accumulated_data.len());

            // 提取版本信息，截取固定长度的字节数据
            let version_bytes = &accumulated_data[version_start..version_end];

            // 使用 UTF-8 解码，忽略错误（与 Python SDK 的 errors='ignore' 对应）
            // 使用 from_utf8_lossy 而不是 from_utf8，以处理无效 UTF-8 字符
            Some(String::from_utf8_lossy(version_bytes).trim().to_string())
        } else {
            None
        }
    }
}

impl TryFrom<PiperFrame> for FirmwareReadFeedback {
    type Error = ProtocolError;

    fn try_from(frame: PiperFrame) -> Result<Self, Self::Error> {
        // 验证 CAN ID
        if frame.id != ID_FIRMWARE_READ {
            return Err(ProtocolError::InvalidCanId { id: frame.id });
        }

        // 验证数据长度（至少需要 1 字节，最多 8 字节）
        if frame.len == 0 {
            return Err(ProtocolError::InvalidLength {
                expected: 1,
                actual: 0,
            });
        }

        let mut firmware_data = [0u8; 8];
        let copy_len = (frame.len as usize).min(8);
        firmware_data[..copy_len].copy_from_slice(&frame.data[..copy_len]);

        Ok(Self { firmware_data })
    }
}

#[cfg(test)]
mod firmware_read_tests {
    use super::*;

    #[test]
    fn test_firmware_read_feedback_parse() {
        // 测试数据：包含 "S-V1.6-3" 版本字符串
        let data = b"S-V1.6-3";
        let frame = PiperFrame::new_standard(ID_FIRMWARE_READ as u16, data);
        let feedback = FirmwareReadFeedback::try_from(frame).unwrap();

        assert_eq!(&feedback.firmware_data[..8], data);
    }

    #[test]
    fn test_firmware_read_feedback_parse_version_string() {
        // 测试解析版本字符串（固定 8 字节长度，包括 S-V）
        let accumulated_data = b"Some prefix S-V1.6-3\nOther data";
        let version = FirmwareReadFeedback::parse_version_string(accumulated_data);
        // 应该返回 "S-V1.6-3"（8 字节，包括 S-V），与 Python SDK 对齐
        assert_eq!(version, Some("S-V1.6-3".to_string()));
    }

    #[test]
    fn test_firmware_read_feedback_parse_version_string_fixed_length() {
        // 测试固定 8 字节长度解析（包括 S-V）
        let accumulated_data = b"Some prefix S-V1.6-3\nOther data";
        let version = FirmwareReadFeedback::parse_version_string(accumulated_data);
        // 应该返回 "S-V1.6-3"（8 字节，包括 S-V）
        assert_eq!(version, Some("S-V1.6-3".to_string()));
    }

    #[test]
    fn test_firmware_read_feedback_parse_version_string_short() {
        // 测试数据不足 8 字节的情况
        let accumulated_data = b"S-V1.6";
        let version = FirmwareReadFeedback::parse_version_string(accumulated_data);
        // 应该返回 "S-V1.6"（实际长度，不超过 8 字节）
        assert_eq!(version, Some("S-V1.6".to_string()));
    }

    #[test]
    fn test_firmware_read_feedback_parse_version_string_invalid_utf8() {
        // 测试包含无效 UTF-8 字符的情况
        let data = vec![b'S', b'-', b'V', 0xFF, 0xFE, b'1', b'.', b'6'];
        let version = FirmwareReadFeedback::parse_version_string(&data);
        // 应该使用 lossy 解码，不会 panic，返回包含替换字符的字符串
        assert!(version.is_some());
        let version_str = version.unwrap();
        // 验证包含替换字符（通常显示为）
        assert!(version_str.contains("S-V"));
    }

    #[test]
    fn test_firmware_read_feedback_parse_version_string_not_found() {
        // 测试未找到版本字符串
        let accumulated_data = b"Some data without version";
        let version = FirmwareReadFeedback::parse_version_string(accumulated_data);
        assert_eq!(version, None);
    }

    #[test]
    fn test_firmware_read_feedback_invalid_id() {
        let frame = PiperFrame::new_standard(0x999, &[0; 8]);
        let result = FirmwareReadFeedback::try_from(frame);
        assert!(result.is_err());
    }

    #[test]
    fn test_firmware_read_feedback_empty_data() {
        let frame = PiperFrame::new_standard(ID_FIRMWARE_READ as u16, &[]);
        let result = FirmwareReadFeedback::try_from(frame);
        assert!(result.is_err());
    }
}

// ============================================================================
// 主从模式控制指令反馈（用于接收主臂发送的控制指令）
// ============================================================================

/// 主从模式控制模式指令反馈 (0x151)
///
/// 在主从模式下，示教输入臂会发送控制指令给运动输出臂。
/// 此反馈用于解析从示教输入臂接收到的控制模式指令。
///
/// 注意：此结构与 `ControlModeCommandFrame` 相同，但用于接收而非发送。
#[derive(Debug, Clone, Copy, Default)]
pub struct ControlModeCommandFeedback {
    pub control_mode: ControlModeCommand,
    pub move_mode: MoveMode,
    pub speed_percent: u8,
    pub mit_mode: MitMode,
    pub trajectory_stay_time: u8,
    pub install_position: InstallPosition,
}

impl TryFrom<PiperFrame> for ControlModeCommandFeedback {
    type Error = ProtocolError;

    fn try_from(frame: PiperFrame) -> Result<Self, Self::Error> {
        // 验证 CAN ID
        if frame.id != ID_CONTROL_MODE {
            return Err(ProtocolError::InvalidCanId { id: frame.id });
        }

        // 验证数据长度
        if frame.len < 6 {
            return Err(ProtocolError::InvalidLength {
                expected: 6,
                actual: frame.len as usize,
            });
        }

        Ok(Self {
            control_mode: ControlModeCommand::try_from(frame.data[0])?,
            move_mode: MoveMode::from(frame.data[1]),
            speed_percent: frame.data[2],
            mit_mode: MitMode::try_from(frame.data[3])?,
            trajectory_stay_time: frame.data[4],
            install_position: InstallPosition::try_from(frame.data[5])?,
        })
    }
}

/// 主从模式关节控制指令反馈 (0x155-0x157)
///
/// 在主从模式下，示教输入臂会发送关节控制指令给运动输出臂。
/// 此反馈用于解析从示教输入臂接收到的关节控制指令。
#[derive(Debug, Clone, Copy, Default)]
pub struct JointControlFeedback {
    pub j1_deg: i32,
    pub j2_deg: i32,
    pub j3_deg: i32,
    pub j4_deg: i32,
    pub j5_deg: i32,
    pub j6_deg: i32,
}

impl JointControlFeedback {
    /// 从 0x155 (J1-J2) 帧更新
    pub fn update_from_12(&mut self, feedback: JointControl12Feedback) {
        self.j1_deg = feedback.j1_deg;
        self.j2_deg = feedback.j2_deg;
    }

    /// 从 0x156 (J3-J4) 帧更新
    pub fn update_from_34(&mut self, feedback: JointControl34Feedback) {
        self.j3_deg = feedback.j3_deg;
        self.j4_deg = feedback.j4_deg;
    }

    /// 从 0x157 (J5-J6) 帧更新
    pub fn update_from_56(&mut self, feedback: JointControl56Feedback) {
        self.j5_deg = feedback.j5_deg;
        self.j6_deg = feedback.j6_deg;
    }
}

/// 主从模式关节控制指令反馈12 (0x155)
#[derive(Debug, Clone, Copy, Default)]
pub struct JointControl12Feedback {
    pub j1_deg: i32,
    pub j2_deg: i32,
}

impl TryFrom<PiperFrame> for JointControl12Feedback {
    type Error = ProtocolError;

    fn try_from(frame: PiperFrame) -> Result<Self, Self::Error> {
        if frame.id != ID_JOINT_CONTROL_12 {
            return Err(ProtocolError::InvalidCanId { id: frame.id });
        }

        if frame.len < 8 {
            return Err(ProtocolError::InvalidLength {
                expected: 8,
                actual: frame.len as usize,
            });
        }

        let j1_bytes = [frame.data[0], frame.data[1], frame.data[2], frame.data[3]];
        let j1_deg = bytes_to_i32_be(j1_bytes);

        let j2_bytes = [frame.data[4], frame.data[5], frame.data[6], frame.data[7]];
        let j2_deg = bytes_to_i32_be(j2_bytes);

        Ok(Self { j1_deg, j2_deg })
    }
}

/// 主从模式关节控制指令反馈34 (0x156)
#[derive(Debug, Clone, Copy, Default)]
pub struct JointControl34Feedback {
    pub j3_deg: i32,
    pub j4_deg: i32,
}

impl TryFrom<PiperFrame> for JointControl34Feedback {
    type Error = ProtocolError;

    fn try_from(frame: PiperFrame) -> Result<Self, Self::Error> {
        if frame.id != ID_JOINT_CONTROL_34 {
            return Err(ProtocolError::InvalidCanId { id: frame.id });
        }

        if frame.len < 8 {
            return Err(ProtocolError::InvalidLength {
                expected: 8,
                actual: frame.len as usize,
            });
        }

        let j3_bytes = [frame.data[0], frame.data[1], frame.data[2], frame.data[3]];
        let j3_deg = bytes_to_i32_be(j3_bytes);

        let j4_bytes = [frame.data[4], frame.data[5], frame.data[6], frame.data[7]];
        let j4_deg = bytes_to_i32_be(j4_bytes);

        Ok(Self { j3_deg, j4_deg })
    }
}

/// 主从模式关节控制指令反馈56 (0x157)
#[derive(Debug, Clone, Copy, Default)]
pub struct JointControl56Feedback {
    pub j5_deg: i32,
    pub j6_deg: i32,
}

impl TryFrom<PiperFrame> for JointControl56Feedback {
    type Error = ProtocolError;

    fn try_from(frame: PiperFrame) -> Result<Self, Self::Error> {
        if frame.id != ID_JOINT_CONTROL_56 {
            return Err(ProtocolError::InvalidCanId { id: frame.id });
        }

        if frame.len < 8 {
            return Err(ProtocolError::InvalidLength {
                expected: 8,
                actual: frame.len as usize,
            });
        }

        let j5_bytes = [frame.data[0], frame.data[1], frame.data[2], frame.data[3]];
        let j5_deg = bytes_to_i32_be(j5_bytes);

        let j6_bytes = [frame.data[4], frame.data[5], frame.data[6], frame.data[7]];
        let j6_deg = bytes_to_i32_be(j6_bytes);

        Ok(Self { j5_deg, j6_deg })
    }
}

/// 主从模式夹爪控制指令反馈 (0x159)
///
/// 在主从模式下，示教输入臂会发送夹爪控制指令给运动输出臂。
#[derive(Debug, Clone, Copy, Default)]
pub struct GripperControlFeedback {
    pub travel_mm: i32,
    pub torque_nm: i16,
    pub status_code: u8,
    pub set_zero: u8,
}

impl TryFrom<PiperFrame> for GripperControlFeedback {
    type Error = ProtocolError;

    fn try_from(frame: PiperFrame) -> Result<Self, Self::Error> {
        if frame.id != ID_GRIPPER_CONTROL {
            return Err(ProtocolError::InvalidCanId { id: frame.id });
        }

        if frame.len < 8 {
            return Err(ProtocolError::InvalidLength {
                expected: 8,
                actual: frame.len as usize,
            });
        }

        let travel_bytes = [frame.data[0], frame.data[1], frame.data[2], frame.data[3]];
        let travel_mm = bytes_to_i32_be(travel_bytes);

        let torque_bytes = [frame.data[4], frame.data[5]];
        let torque_nm = bytes_to_i16_be(torque_bytes);

        Ok(Self {
            travel_mm,
            torque_nm,
            status_code: frame.data[6],
            set_zero: frame.data[7],
        })
    }
}
