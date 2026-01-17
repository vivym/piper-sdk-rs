//! 控制帧结构体定义
//!
//! 包含所有控制指令帧的结构体，提供构建控制帧的方法
//! 和转换为 `PiperFrame` 的方法。

use crate::can::PiperFrame;
use crate::protocol::{ProtocolError, i16_to_bytes_be, i32_to_bytes_be, ids::*};
use bilge::prelude::*;

// ============================================================================
// 控制模式指令相关枚举
// ============================================================================

/// 控制模式（控制指令版本，0x151）
///
/// 注意：控制指令的 ControlMode 与反馈帧的 ControlMode 不同。
/// 控制指令只支持部分值（0x00, 0x01, 0x02, 0x03, 0x04, 0x07），
/// 不支持 0x05（Remote）和 0x06（LinkTeach）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ControlModeCommand {
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
    /// 离线轨迹模式
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
                value,
            }),
        }
    }
}

/// MIT 模式
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MitMode {
    /// 位置速度模式（默认）
    #[default]
    PositionVelocity = 0x00,
    /// MIT模式（用于主从模式）
    Mit = 0xAD,
}

impl TryFrom<u8> for MitMode {
    type Error = ProtocolError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0x00 => Ok(MitMode::PositionVelocity),
            0xAD => Ok(MitMode::Mit),
            _ => Err(ProtocolError::InvalidValue {
                field: "MitMode".to_string(),
                value,
            }),
        }
    }
}

/// 安装位置
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InstallPosition {
    /// 无效值
    #[default]
    Invalid = 0x00,
    /// 水平正装
    Horizontal = 0x01,
    /// 侧装左
    SideLeft = 0x02,
    /// 侧装右
    SideRight = 0x03,
}

impl TryFrom<u8> for InstallPosition {
    type Error = ProtocolError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0x00 => Ok(InstallPosition::Invalid),
            0x01 => Ok(InstallPosition::Horizontal),
            0x02 => Ok(InstallPosition::SideLeft),
            0x03 => Ok(InstallPosition::SideRight),
            _ => Err(ProtocolError::InvalidValue {
                field: "InstallPosition".to_string(),
                value,
            }),
        }
    }
}

// 从 feedback 模块导入 MoveMode（控制指令和反馈帧共用）
use crate::protocol::feedback::MoveMode;

// ============================================================================
// 控制模式指令结构体
// ============================================================================

/// 控制模式指令 (0x151)
///
/// 用于切换机械臂的控制模式、MOVE 模式、运动速度等参数。
#[derive(Debug, Clone, Copy, Default)]
pub struct ControlModeCommandFrame {
    pub control_mode: ControlModeCommand, // Byte 0
    pub move_mode: MoveMode,              // Byte 1
    pub speed_percent: u8,                // Byte 2 (0-100)
    pub mit_mode: MitMode,                // Byte 3: 0x00 或 0xAD
    pub trajectory_stay_time: u8,         // Byte 4: 0~254（单位s），255表示轨迹终止
    pub install_position: InstallPosition, // Byte 5: 安装位置
                                          // Byte 6-7: 保留
}

impl ControlModeCommandFrame {
    /// 创建模式切换指令（仅切换控制模式，其他字段填充 0x0）
    ///
    /// 用于快速切换控制模式，其他参数使用默认值。
    pub fn mode_switch(control_mode: ControlModeCommand) -> Self {
        Self {
            control_mode,
            move_mode: MoveMode::MoveP, // 默认值
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

    /// 转换为 CAN 帧
    pub fn to_frame(self) -> PiperFrame {
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

#[cfg(test)]
mod tests {
    use super::*;

    // ========================================================================
    // ControlModeCommand 枚举测试
    // ========================================================================

    #[test]
    fn test_control_mode_command_from_u8() {
        assert_eq!(
            ControlModeCommand::try_from(0x00).unwrap(),
            ControlModeCommand::Standby
        );
        assert_eq!(
            ControlModeCommand::try_from(0x01).unwrap(),
            ControlModeCommand::CanControl
        );
        assert_eq!(
            ControlModeCommand::try_from(0x02).unwrap(),
            ControlModeCommand::Teach
        );
        assert_eq!(
            ControlModeCommand::try_from(0x03).unwrap(),
            ControlModeCommand::Ethernet
        );
        assert_eq!(
            ControlModeCommand::try_from(0x04).unwrap(),
            ControlModeCommand::Wifi
        );
        assert_eq!(
            ControlModeCommand::try_from(0x07).unwrap(),
            ControlModeCommand::OfflineTrajectory
        );
    }

    #[test]
    fn test_control_mode_command_invalid_values() {
        // 测试无效值（0x05, 0x06 未定义）
        assert!(ControlModeCommand::try_from(0x05).is_err());
        assert!(ControlModeCommand::try_from(0x06).is_err());
        assert!(ControlModeCommand::try_from(0xFF).is_err());
    }

    // ========================================================================
    // MitMode 枚举测试
    // ========================================================================

    #[test]
    fn test_mit_mode_from_u8() {
        assert_eq!(MitMode::try_from(0x00).unwrap(), MitMode::PositionVelocity);
        assert_eq!(MitMode::try_from(0xAD).unwrap(), MitMode::Mit);
    }

    #[test]
    fn test_mit_mode_invalid_values() {
        assert!(MitMode::try_from(0x01).is_err());
        assert!(MitMode::try_from(0xFF).is_err());
    }

    // ========================================================================
    // InstallPosition 枚举测试
    // ========================================================================

    #[test]
    fn test_install_position_from_u8() {
        assert_eq!(
            InstallPosition::try_from(0x00).unwrap(),
            InstallPosition::Invalid
        );
        assert_eq!(
            InstallPosition::try_from(0x01).unwrap(),
            InstallPosition::Horizontal
        );
        assert_eq!(
            InstallPosition::try_from(0x02).unwrap(),
            InstallPosition::SideLeft
        );
        assert_eq!(
            InstallPosition::try_from(0x03).unwrap(),
            InstallPosition::SideRight
        );
    }

    #[test]
    fn test_install_position_invalid_values() {
        assert!(InstallPosition::try_from(0x04).is_err());
        assert!(InstallPosition::try_from(0xFF).is_err());
    }

    // ========================================================================
    // ControlModeCommandFrame 测试
    // ========================================================================

    #[test]
    fn test_control_mode_command_frame_mode_switch() {
        let cmd = ControlModeCommandFrame::mode_switch(ControlModeCommand::CanControl);
        let frame = cmd.to_frame();

        assert_eq!(frame.id, ID_CONTROL_MODE);
        assert_eq!(frame.data[0], 0x01); // CanControl
        assert_eq!(frame.data[1], 0x00); // MoveP (默认)
        assert_eq!(frame.data[2], 0x00); // speed_percent = 0
        assert_eq!(frame.data[3], 0x00); // PositionVelocity (默认)
        assert_eq!(frame.data[4], 0x00); // trajectory_stay_time = 0
        assert_eq!(frame.data[5], 0x00); // Invalid (默认)
        assert_eq!(frame.data[6], 0x00); // 保留
        assert_eq!(frame.data[7], 0x00); // 保留
    }

    #[test]
    fn test_control_mode_command_frame_new() {
        let cmd = ControlModeCommandFrame::new(
            ControlModeCommand::CanControl,
            MoveMode::MoveJ,
            50, // 50% 速度
            MitMode::Mit,
            10, // 停留 10 秒
            InstallPosition::Horizontal,
        );
        let frame = cmd.to_frame();

        assert_eq!(frame.id, ID_CONTROL_MODE);
        assert_eq!(frame.data[0], 0x01); // CanControl
        assert_eq!(frame.data[1], 0x01); // MoveJ
        assert_eq!(frame.data[2], 50); // speed_percent
        assert_eq!(frame.data[3], 0xAD); // Mit
        assert_eq!(frame.data[4], 10); // trajectory_stay_time
        assert_eq!(frame.data[5], 0x01); // Horizontal
    }

    #[test]
    fn test_control_mode_command_frame_trajectory_terminate() {
        // 测试轨迹终止（trajectory_stay_time = 255）
        let cmd = ControlModeCommandFrame::new(
            ControlModeCommand::OfflineTrajectory,
            MoveMode::MoveP,
            0,
            MitMode::PositionVelocity,
            255, // 轨迹终止
            InstallPosition::Invalid,
        );
        let frame = cmd.to_frame();

        assert_eq!(frame.data[4], 255); // 轨迹终止标志
    }
}

// ============================================================================
// 关节控制指令结构体
// ============================================================================

/// 机械臂臂部关节控制指令12 (0x155)
///
/// 用于控制 J1 和 J2 关节的目标角度。
/// 单位：0.001°（原始值），可通过 `new()` 方法从物理量（度）创建。
#[derive(Debug, Clone, Copy, Default)]
pub struct JointControl12 {
    pub j1_deg: i32, // Byte 0-3: J1角度，单位 0.001°
    pub j2_deg: i32, // Byte 4-7: J2角度，单位 0.001°
}

impl JointControl12 {
    /// 从物理量（度）创建关节控制指令
    pub fn new(j1: f64, j2: f64) -> Self {
        Self {
            j1_deg: (j1 * 1000.0) as i32,
            j2_deg: (j2 * 1000.0) as i32,
        }
    }

    /// 转换为 CAN 帧
    pub fn to_frame(self) -> PiperFrame {
        let mut data = [0u8; 8];
        let j1_bytes = i32_to_bytes_be(self.j1_deg);
        let j2_bytes = i32_to_bytes_be(self.j2_deg);
        data[0..4].copy_from_slice(&j1_bytes);
        data[4..8].copy_from_slice(&j2_bytes);

        PiperFrame::new_standard(ID_JOINT_CONTROL_12 as u16, &data)
    }
}

/// 机械臂腕部关节控制指令34 (0x156)
///
/// 用于控制 J3 和 J4 关节的目标角度。
#[derive(Debug, Clone, Copy, Default)]
pub struct JointControl34 {
    pub j3_deg: i32, // Byte 0-3: J3角度，单位 0.001°
    pub j4_deg: i32, // Byte 4-7: J4角度，单位 0.001°
}

impl JointControl34 {
    /// 从物理量（度）创建关节控制指令
    pub fn new(j3: f64, j4: f64) -> Self {
        Self {
            j3_deg: (j3 * 1000.0) as i32,
            j4_deg: (j4 * 1000.0) as i32,
        }
    }

    /// 转换为 CAN 帧
    pub fn to_frame(self) -> PiperFrame {
        let mut data = [0u8; 8];
        let j3_bytes = i32_to_bytes_be(self.j3_deg);
        let j4_bytes = i32_to_bytes_be(self.j4_deg);
        data[0..4].copy_from_slice(&j3_bytes);
        data[4..8].copy_from_slice(&j4_bytes);

        PiperFrame::new_standard(ID_JOINT_CONTROL_34 as u16, &data)
    }
}

/// 机械臂腕部关节控制指令56 (0x157)
///
/// 用于控制 J5 和 J6 关节的目标角度。
#[derive(Debug, Clone, Copy, Default)]
pub struct JointControl56 {
    pub j5_deg: i32, // Byte 0-3: J5角度，单位 0.001°
    pub j6_deg: i32, // Byte 4-7: J6角度，单位 0.001°
}

impl JointControl56 {
    /// 从物理量（度）创建关节控制指令
    pub fn new(j5: f64, j6: f64) -> Self {
        Self {
            j5_deg: (j5 * 1000.0) as i32,
            j6_deg: (j6 * 1000.0) as i32,
        }
    }

    /// 转换为 CAN 帧
    pub fn to_frame(self) -> PiperFrame {
        let mut data = [0u8; 8];
        let j5_bytes = i32_to_bytes_be(self.j5_deg);
        let j6_bytes = i32_to_bytes_be(self.j6_deg);
        data[0..4].copy_from_slice(&j5_bytes);
        data[4..8].copy_from_slice(&j6_bytes);

        PiperFrame::new_standard(ID_JOINT_CONTROL_56 as u16, &data)
    }
}

#[cfg(test)]
mod joint_control_tests {
    use super::*;

    #[test]
    fn test_joint_control12_new() {
        let cmd = JointControl12::new(90.0, -45.0);
        assert_eq!(cmd.j1_deg, 90000);
        assert_eq!(cmd.j2_deg, -45000);
    }

    #[test]
    fn test_joint_control12_to_frame() {
        let cmd = JointControl12::new(90.0, -45.0);
        let frame = cmd.to_frame();

        assert_eq!(frame.id, ID_JOINT_CONTROL_12);
        // 验证大端字节序编码
        let j1_decoded =
            i32::from_be_bytes([frame.data[0], frame.data[1], frame.data[2], frame.data[3]]);
        let j2_decoded =
            i32::from_be_bytes([frame.data[4], frame.data[5], frame.data[6], frame.data[7]]);
        assert_eq!(j1_decoded, 90000);
        assert_eq!(j2_decoded, -45000);
    }

    #[test]
    fn test_joint_control12_roundtrip() {
        // 测试编码-解码循环（直接验证编码后的字节值）
        let cmd = JointControl12::new(90.0, -45.0);
        let frame = cmd.to_frame();

        // 验证 CAN ID
        assert_eq!(frame.id, ID_JOINT_CONTROL_12);

        // 验证编码后的字节值（大端字节序）
        let j1_decoded =
            i32::from_be_bytes([frame.data[0], frame.data[1], frame.data[2], frame.data[3]]);
        let j2_decoded =
            i32::from_be_bytes([frame.data[4], frame.data[5], frame.data[6], frame.data[7]]);
        assert_eq!(j1_decoded, 90000);
        assert_eq!(j2_decoded, -45000);

        // 验证原始值
        assert_eq!(cmd.j1_deg, 90000);
        assert_eq!(cmd.j2_deg, -45000);
    }

    #[test]
    fn test_joint_control34_new() {
        let cmd = JointControl34::new(30.0, -60.0);
        assert_eq!(cmd.j3_deg, 30000);
        assert_eq!(cmd.j4_deg, -60000);
    }

    #[test]
    fn test_joint_control34_to_frame() {
        let cmd = JointControl34::new(30.0, -60.0);
        let frame = cmd.to_frame();

        assert_eq!(frame.id, ID_JOINT_CONTROL_34);
        let j3_decoded =
            i32::from_be_bytes([frame.data[0], frame.data[1], frame.data[2], frame.data[3]]);
        let j4_decoded =
            i32::from_be_bytes([frame.data[4], frame.data[5], frame.data[6], frame.data[7]]);
        assert_eq!(j3_decoded, 30000);
        assert_eq!(j4_decoded, -60000);
    }

    #[test]
    fn test_joint_control56_new() {
        let cmd = JointControl56::new(180.0, -90.0);
        assert_eq!(cmd.j5_deg, 180000);
        assert_eq!(cmd.j6_deg, -90000);
    }

    #[test]
    fn test_joint_control56_to_frame() {
        let cmd = JointControl56::new(180.0, -90.0);
        let frame = cmd.to_frame();

        assert_eq!(frame.id, ID_JOINT_CONTROL_56);
        let j5_decoded =
            i32::from_be_bytes([frame.data[0], frame.data[1], frame.data[2], frame.data[3]]);
        let j6_decoded =
            i32::from_be_bytes([frame.data[4], frame.data[5], frame.data[6], frame.data[7]]);
        assert_eq!(j5_decoded, 180000);
        assert_eq!(j6_decoded, -90000);
    }

    #[test]
    fn test_joint_control_precision() {
        // 测试精度：0.5° = 500 (0.001° 单位)
        let cmd = JointControl12::new(0.5, -0.5);
        assert_eq!(cmd.j1_deg, 500);
        assert_eq!(cmd.j2_deg, -500);
    }
}

// ============================================================================
// 快速急停/轨迹指令结构体
// ============================================================================

/// 快速急停动作
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EmergencyStopAction {
    /// 无效
    #[default]
    Invalid = 0x00,
    /// 快速急停
    EmergencyStop = 0x01,
    /// 恢复
    Resume = 0x02,
}

impl TryFrom<u8> for EmergencyStopAction {
    type Error = ProtocolError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0x00 => Ok(EmergencyStopAction::Invalid),
            0x01 => Ok(EmergencyStopAction::EmergencyStop),
            0x02 => Ok(EmergencyStopAction::Resume),
            _ => Err(ProtocolError::InvalidValue {
                field: "EmergencyStopAction".to_string(),
                value,
            }),
        }
    }
}

/// 轨迹指令
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TrajectoryCommand {
    /// 关闭
    #[default]
    Closed = 0x00,
    /// 暂停当前规划
    PausePlanning = 0x01,
    /// 开始/继续当前轨迹
    StartContinue = 0x02,
    /// 清除当前轨迹
    ClearCurrent = 0x03,
    /// 清除所有轨迹
    ClearAll = 0x04,
    /// 获取当前规划轨迹
    GetCurrentPlanning = 0x05,
    /// 终止执行
    Terminate = 0x06,
    /// 轨迹传输
    Transmit = 0x07,
    /// 轨迹传输结束
    TransmitEnd = 0x08,
}

impl TryFrom<u8> for TrajectoryCommand {
    type Error = ProtocolError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0x00 => Ok(TrajectoryCommand::Closed),
            0x01 => Ok(TrajectoryCommand::PausePlanning),
            0x02 => Ok(TrajectoryCommand::StartContinue),
            0x03 => Ok(TrajectoryCommand::ClearCurrent),
            0x04 => Ok(TrajectoryCommand::ClearAll),
            0x05 => Ok(TrajectoryCommand::GetCurrentPlanning),
            0x06 => Ok(TrajectoryCommand::Terminate),
            0x07 => Ok(TrajectoryCommand::Transmit),
            0x08 => Ok(TrajectoryCommand::TransmitEnd),
            _ => Err(ProtocolError::InvalidValue {
                field: "TrajectoryCommand".to_string(),
                value,
            }),
        }
    }
}

/// 拖动示教指令
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TeachCommand {
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

impl TryFrom<u8> for TeachCommand {
    type Error = ProtocolError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0x00 => Ok(TeachCommand::Closed),
            0x01 => Ok(TeachCommand::StartRecord),
            0x02 => Ok(TeachCommand::EndRecord),
            0x03 => Ok(TeachCommand::Execute),
            0x04 => Ok(TeachCommand::Pause),
            0x05 => Ok(TeachCommand::Continue),
            0x06 => Ok(TeachCommand::Terminate),
            0x07 => Ok(TeachCommand::MoveToStart),
            _ => Err(ProtocolError::InvalidValue {
                field: "TeachCommand".to_string(),
                value,
            }),
        }
    }
}

/// 快速急停/轨迹指令 (0x150)
///
/// 用于快速急停、轨迹控制和拖动示教控制。
/// 注意：在离线轨迹模式下，Byte 3-7 用于轨迹传输（轨迹点索引、NameIndex、CRC16），
/// 其他模式下这些字段全部填充 0x0。
#[derive(Debug, Clone, Copy, Default)]
pub struct EmergencyStopCommand {
    pub emergency_stop: EmergencyStopAction,   // Byte 0
    pub trajectory_command: TrajectoryCommand, // Byte 1
    pub teach_command: TeachCommand,           // Byte 2
    pub trajectory_index: u8,                  // Byte 3: 轨迹点索引 (0~255)
    // 以下字段用于离线轨迹模式下的轨迹传输，其它模式下全部填充 0x0
    pub name_index: u16, // Byte 4-5: 轨迹包名称索引
    pub crc16: u16,      // Byte 6-7: CRC16 校验
}

impl EmergencyStopCommand {
    /// 创建快速急停指令
    pub fn emergency_stop() -> Self {
        Self {
            emergency_stop: EmergencyStopAction::EmergencyStop,
            trajectory_command: TrajectoryCommand::Closed,
            teach_command: TeachCommand::Closed,
            trajectory_index: 0,
            name_index: 0,
            crc16: 0,
        }
    }

    /// 创建恢复指令
    pub fn resume() -> Self {
        Self {
            emergency_stop: EmergencyStopAction::Resume,
            trajectory_command: TrajectoryCommand::Closed,
            teach_command: TeachCommand::Closed,
            trajectory_index: 0,
            name_index: 0,
            crc16: 0,
        }
    }

    /// 创建轨迹传输指令（用于离线轨迹模式）
    pub fn trajectory_transmit(trajectory_index: u8, name_index: u16, crc16: u16) -> Self {
        Self {
            emergency_stop: EmergencyStopAction::Invalid,
            trajectory_command: TrajectoryCommand::Transmit,
            teach_command: TeachCommand::Closed,
            trajectory_index,
            name_index,
            crc16,
        }
    }

    /// 转换为 CAN 帧
    pub fn to_frame(self) -> PiperFrame {
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

#[cfg(test)]
mod emergency_stop_tests {
    use super::*;

    #[test]
    fn test_emergency_stop_action_from_u8() {
        assert_eq!(
            EmergencyStopAction::try_from(0x00).unwrap(),
            EmergencyStopAction::Invalid
        );
        assert_eq!(
            EmergencyStopAction::try_from(0x01).unwrap(),
            EmergencyStopAction::EmergencyStop
        );
        assert_eq!(
            EmergencyStopAction::try_from(0x02).unwrap(),
            EmergencyStopAction::Resume
        );
    }

    #[test]
    fn test_trajectory_command_from_u8() {
        assert_eq!(
            TrajectoryCommand::try_from(0x00).unwrap(),
            TrajectoryCommand::Closed
        );
        assert_eq!(
            TrajectoryCommand::try_from(0x07).unwrap(),
            TrajectoryCommand::Transmit
        );
        assert_eq!(
            TrajectoryCommand::try_from(0x08).unwrap(),
            TrajectoryCommand::TransmitEnd
        );
    }

    #[test]
    fn test_teach_command_from_u8() {
        assert_eq!(TeachCommand::try_from(0x00).unwrap(), TeachCommand::Closed);
        assert_eq!(
            TeachCommand::try_from(0x01).unwrap(),
            TeachCommand::StartRecord
        );
        assert_eq!(
            TeachCommand::try_from(0x07).unwrap(),
            TeachCommand::MoveToStart
        );
    }

    #[test]
    fn test_emergency_stop_command_emergency_stop() {
        let cmd = EmergencyStopCommand::emergency_stop();
        let frame = cmd.to_frame();

        assert_eq!(frame.id, ID_EMERGENCY_STOP);
        assert_eq!(frame.data[0], 0x01); // EmergencyStop
        assert_eq!(frame.data[1], 0x00); // Closed
        assert_eq!(frame.data[2], 0x00); // Closed
        assert_eq!(frame.data[3], 0x00); // trajectory_index = 0
    }

    #[test]
    fn test_emergency_stop_command_resume() {
        let cmd = EmergencyStopCommand::resume();
        let frame = cmd.to_frame();

        assert_eq!(frame.id, ID_EMERGENCY_STOP);
        assert_eq!(frame.data[0], 0x02); // Resume
    }

    #[test]
    fn test_emergency_stop_command_trajectory_transmit() {
        let cmd = EmergencyStopCommand::trajectory_transmit(5, 0x1234, 0x5678);
        let frame = cmd.to_frame();

        assert_eq!(frame.id, ID_EMERGENCY_STOP);
        assert_eq!(frame.data[0], 0x00); // Invalid
        assert_eq!(frame.data[1], 0x07); // Transmit
        assert_eq!(frame.data[3], 5); // trajectory_index

        // 验证大端字节序
        let name_index = u16::from_be_bytes([frame.data[4], frame.data[5]]);
        let crc16 = u16::from_be_bytes([frame.data[6], frame.data[7]]);
        assert_eq!(name_index, 0x1234);
        assert_eq!(crc16, 0x5678);
    }
}

// ============================================================================
// 电机使能指令结构体
// ============================================================================

/// 电机使能/失能设置指令 (0x471)
///
/// 用于使能或失能指定的关节电机。
#[derive(Debug, Clone, Copy)]
pub struct MotorEnableCommand {
    pub joint_index: u8, // Byte 0: 1-6 代表关节驱动器序号，7 代表全部关节电机
    pub enable: bool,    // Byte 1: true = 使能 (0x02), false = 失能 (0x01)
}

impl MotorEnableCommand {
    /// 创建使能指令
    pub fn enable(joint_index: u8) -> Self {
        Self {
            joint_index,
            enable: true,
        }
    }

    /// 创建失能指令
    pub fn disable(joint_index: u8) -> Self {
        Self {
            joint_index,
            enable: false,
        }
    }

    /// 使能全部关节电机
    pub fn enable_all() -> Self {
        Self {
            joint_index: 7,
            enable: true,
        }
    }

    /// 失能全部关节电机
    pub fn disable_all() -> Self {
        Self {
            joint_index: 7,
            enable: false,
        }
    }

    /// 转换为 CAN 帧
    pub fn to_frame(self) -> PiperFrame {
        let mut data = [0u8; 8];
        data[0] = self.joint_index;
        data[1] = if self.enable { 0x02 } else { 0x01 };
        // Byte 2-7: 保留，已初始化为 0

        PiperFrame::new_standard(ID_MOTOR_ENABLE as u16, &data)
    }
}

#[cfg(test)]
mod motor_enable_tests {
    use super::*;

    #[test]
    fn test_motor_enable_command_enable() {
        let cmd = MotorEnableCommand::enable(1);
        let frame = cmd.to_frame();

        assert_eq!(frame.id, ID_MOTOR_ENABLE);
        assert_eq!(frame.data[0], 1);
        assert_eq!(frame.data[1], 0x02); // 使能
    }

    #[test]
    fn test_motor_enable_command_disable() {
        let cmd = MotorEnableCommand::disable(2);
        let frame = cmd.to_frame();

        assert_eq!(frame.id, ID_MOTOR_ENABLE);
        assert_eq!(frame.data[0], 2);
        assert_eq!(frame.data[1], 0x01); // 失能
    }

    #[test]
    fn test_motor_enable_command_enable_all() {
        let cmd = MotorEnableCommand::enable_all();
        let frame = cmd.to_frame();

        assert_eq!(frame.id, ID_MOTOR_ENABLE);
        assert_eq!(frame.data[0], 7); // 全部关节
        assert_eq!(frame.data[1], 0x02); // 使能
    }

    #[test]
    fn test_motor_enable_command_all_joints() {
        // 测试所有关节序号（1-6）
        for i in 1..=6 {
            let cmd = MotorEnableCommand::enable(i);
            let frame = cmd.to_frame();
            assert_eq!(frame.data[0], i);
            assert_eq!(frame.data[1], 0x02);
        }
    }
}

// ============================================================================
// 夹爪控制指令结构体
// ============================================================================

/// 夹爪控制标志位域（Byte 6: 8 位）
///
/// 协议定义：
/// - Bit 0: 置1使能，0失能
/// - Bit 1: 置1清除错误
/// - Bit 2-7: 保留
#[bitsize(8)]
#[derive(FromBits, DebugBits, Clone, Copy, Default)]
pub struct GripperControlFlags {
    pub enable: bool,      // Bit 0: 置1使能，0失能
    pub clear_error: bool, // Bit 1: 置1清除错误
    pub reserved: u6,      // Bit 2-7: 保留
}

/// 夹爪控制指令 (0x159)
///
/// 用于控制夹爪的行程、扭矩、使能状态和零点设置。
/// - 行程单位：0.001mm（原始值），0值表示完全闭合
/// - 扭矩单位：0.001N·m（原始值）
#[derive(Debug, Clone, Copy)]
pub struct GripperControlCommand {
    pub travel_mm: i32, // Byte 0-3: 夹爪行程，单位 0.001mm（0值表示完全闭合）
    pub torque_nm: i16, // Byte 4-5: 夹爪扭矩，单位 0.001N·m
    pub control_flags: GripperControlFlags, // Byte 6: 控制标志位域
    pub zero_setting: u8, // Byte 7: 零点设置（0x00: 无效，0xAE: 设置当前为零点）
}

impl GripperControlCommand {
    /// 从物理量创建夹爪控制指令
    pub fn new(travel_mm: f64, torque_nm: f64, enable: bool) -> Self {
        Self {
            travel_mm: (travel_mm * 1000.0) as i32,
            torque_nm: (torque_nm * 1000.0) as i16,
            control_flags: {
                let mut flags = GripperControlFlags::from(u8::new(0));
                flags.set_enable(enable);
                flags
            },
            zero_setting: 0x00,
        }
    }

    /// 设置零点（设置当前为零点）
    pub fn set_zero_point(mut self) -> Self {
        self.zero_setting = 0xAE;
        // 设置零点时，Byte 6 应该填充 0x0（失能）
        let mut flags = GripperControlFlags::from(u8::new(0));
        flags.set_enable(false);
        self.control_flags = flags;
        self
    }

    /// 清除错误
    pub fn clear_error(mut self) -> Self {
        let mut flags = self.control_flags;
        flags.set_clear_error(true);
        self.control_flags = flags;
        self
    }

    /// 转换为 CAN 帧
    pub fn to_frame(self) -> PiperFrame {
        let mut data = [0u8; 8];

        // 大端字节序
        let travel_bytes = i32_to_bytes_be(self.travel_mm);
        data[0..4].copy_from_slice(&travel_bytes);

        let torque_bytes = i16_to_bytes_be(self.torque_nm);
        data[4..6].copy_from_slice(&torque_bytes);

        data[6] = u8::from(self.control_flags).value();
        data[7] = self.zero_setting;

        PiperFrame::new_standard(ID_GRIPPER_CONTROL as u16, &data)
    }
}

#[cfg(test)]
mod gripper_control_tests {
    use super::*;

    #[test]
    fn test_gripper_control_flags_parse() {
        // 测试：Bit 0 = 1（使能），Bit 1 = 1（清除错误）
        let byte = 0b0000_0011;
        let flags = GripperControlFlags::from(u8::new(byte));

        assert!(flags.enable());
        assert!(flags.clear_error());
    }

    #[test]
    fn test_gripper_control_flags_encode() {
        let mut flags = GripperControlFlags::from(u8::new(0));
        flags.set_enable(true);
        flags.set_clear_error(true);

        let encoded = u8::from(flags).value();
        assert_eq!(encoded, 0b0000_0011);
    }

    #[test]
    fn test_gripper_control_command_new() {
        let cmd = GripperControlCommand::new(50.0, 2.5, true);
        assert_eq!(cmd.travel_mm, 50000);
        assert_eq!(cmd.torque_nm, 2500);
        assert!(cmd.control_flags.enable());
        assert_eq!(cmd.zero_setting, 0x00);
    }

    #[test]
    fn test_gripper_control_command_to_frame() {
        let cmd = GripperControlCommand::new(50.0, 2.5, true);
        let frame = cmd.to_frame();

        assert_eq!(frame.id, ID_GRIPPER_CONTROL);

        // 验证大端字节序
        let travel_decoded =
            i32::from_be_bytes([frame.data[0], frame.data[1], frame.data[2], frame.data[3]]);
        let torque_decoded = i16::from_be_bytes([frame.data[4], frame.data[5]]);
        assert_eq!(travel_decoded, 50000);
        assert_eq!(torque_decoded, 2500);
        assert_eq!(frame.data[6] & 0x01, 0x01); // Bit 0 = 1（使能）
        assert_eq!(frame.data[7], 0x00); // 零点设置无效
    }

    #[test]
    fn test_gripper_control_command_set_zero_point() {
        let cmd = GripperControlCommand::new(0.0, 0.0, false).set_zero_point();
        let frame = cmd.to_frame();

        assert_eq!(frame.data[6], 0x00); // 失能（设置零点时）
        assert_eq!(frame.data[7], 0xAE); // 设置零点标志
    }

    #[test]
    fn test_gripper_control_command_clear_error() {
        let cmd = GripperControlCommand::new(50.0, 2.5, true).clear_error();
        let frame = cmd.to_frame();

        assert_eq!(frame.data[6] & 0x03, 0x03); // Bit 0 和 Bit 1 都是 1
    }

    #[test]
    fn test_gripper_control_command_fully_closed() {
        // 测试完全闭合（travel = 0）
        let cmd = GripperControlCommand::new(0.0, 1.0, true);
        assert_eq!(cmd.travel_mm, 0);
    }
}

// ============================================================================
// 末端位姿控制指令结构体
// ============================================================================

/// 机械臂运动控制直角坐标指令1 (0x152)
///
/// 用于控制末端执行器的 X 和 Y 坐标。
/// 单位：0.001mm（原始值）
#[derive(Debug, Clone, Copy, Default)]
pub struct EndPoseControl1 {
    pub x_mm: i32, // Byte 0-3: X坐标，单位 0.001mm
    pub y_mm: i32, // Byte 4-7: Y坐标，单位 0.001mm
}

impl EndPoseControl1 {
    /// 从物理量（mm）创建末端位姿控制指令
    pub fn new(x: f64, y: f64) -> Self {
        Self {
            x_mm: (x * 1000.0) as i32,
            y_mm: (y * 1000.0) as i32,
        }
    }

    /// 转换为 CAN 帧
    pub fn to_frame(self) -> PiperFrame {
        let mut data = [0u8; 8];
        let x_bytes = i32_to_bytes_be(self.x_mm);
        let y_bytes = i32_to_bytes_be(self.y_mm);
        data[0..4].copy_from_slice(&x_bytes);
        data[4..8].copy_from_slice(&y_bytes);

        PiperFrame::new_standard(ID_END_POSE_CONTROL_1 as u16, &data)
    }
}

/// 机械臂运动控制旋转坐标指令2 (0x153)
///
/// 用于控制末端执行器的 Z 坐标和 RX 角度。
/// - Z 单位：0.001mm（原始值）
/// - RX 单位：0.001°（原始值）
#[derive(Debug, Clone, Copy, Default)]
pub struct EndPoseControl2 {
    pub z_mm: i32,   // Byte 0-3: Z坐标，单位 0.001mm
    pub rx_deg: i32, // Byte 4-7: RX角度，单位 0.001°
}

impl EndPoseControl2 {
    /// 从物理量创建末端位姿控制指令
    pub fn new(z: f64, rx: f64) -> Self {
        Self {
            z_mm: (z * 1000.0) as i32,
            rx_deg: (rx * 1000.0) as i32,
        }
    }

    /// 转换为 CAN 帧
    pub fn to_frame(self) -> PiperFrame {
        let mut data = [0u8; 8];
        let z_bytes = i32_to_bytes_be(self.z_mm);
        let rx_bytes = i32_to_bytes_be(self.rx_deg);
        data[0..4].copy_from_slice(&z_bytes);
        data[4..8].copy_from_slice(&rx_bytes);

        PiperFrame::new_standard(ID_END_POSE_CONTROL_2 as u16, &data)
    }
}

/// 机械臂运动控制旋转坐标指令3 (0x154)
///
/// 用于控制末端执行器的 RY 和 RZ 角度。
/// 单位：0.001°（原始值）
#[derive(Debug, Clone, Copy, Default)]
pub struct EndPoseControl3 {
    pub ry_deg: i32, // Byte 0-3: RY角度，单位 0.001°
    pub rz_deg: i32, // Byte 4-7: RZ角度，单位 0.001°
}

impl EndPoseControl3 {
    /// 从物理量（度）创建末端位姿控制指令
    pub fn new(ry: f64, rz: f64) -> Self {
        Self {
            ry_deg: (ry * 1000.0) as i32,
            rz_deg: (rz * 1000.0) as i32,
        }
    }

    /// 转换为 CAN 帧
    pub fn to_frame(self) -> PiperFrame {
        let mut data = [0u8; 8];
        let ry_bytes = i32_to_bytes_be(self.ry_deg);
        let rz_bytes = i32_to_bytes_be(self.rz_deg);
        data[0..4].copy_from_slice(&ry_bytes);
        data[4..8].copy_from_slice(&rz_bytes);

        PiperFrame::new_standard(ID_END_POSE_CONTROL_3 as u16, &data)
    }
}

#[cfg(test)]
mod end_pose_control_tests {
    use super::*;

    #[test]
    fn test_end_pose_control1_new() {
        let cmd = EndPoseControl1::new(100.0, -50.0);
        assert_eq!(cmd.x_mm, 100000);
        assert_eq!(cmd.y_mm, -50000);
    }

    #[test]
    fn test_end_pose_control1_to_frame() {
        let cmd = EndPoseControl1::new(100.0, -50.0);
        let frame = cmd.to_frame();

        assert_eq!(frame.id, ID_END_POSE_CONTROL_1);
        let x_decoded =
            i32::from_be_bytes([frame.data[0], frame.data[1], frame.data[2], frame.data[3]]);
        let y_decoded =
            i32::from_be_bytes([frame.data[4], frame.data[5], frame.data[6], frame.data[7]]);
        assert_eq!(x_decoded, 100000);
        assert_eq!(y_decoded, -50000);
    }

    #[test]
    fn test_end_pose_control2_new() {
        let cmd = EndPoseControl2::new(200.0, 90.0);
        assert_eq!(cmd.z_mm, 200000);
        assert_eq!(cmd.rx_deg, 90000);
    }

    #[test]
    fn test_end_pose_control2_to_frame() {
        let cmd = EndPoseControl2::new(200.0, 90.0);
        let frame = cmd.to_frame();

        assert_eq!(frame.id, ID_END_POSE_CONTROL_2);
        let z_decoded =
            i32::from_be_bytes([frame.data[0], frame.data[1], frame.data[2], frame.data[3]]);
        let rx_decoded =
            i32::from_be_bytes([frame.data[4], frame.data[5], frame.data[6], frame.data[7]]);
        assert_eq!(z_decoded, 200000);
        assert_eq!(rx_decoded, 90000);
    }

    #[test]
    fn test_end_pose_control3_new() {
        let cmd = EndPoseControl3::new(-45.0, 180.0);
        assert_eq!(cmd.ry_deg, -45000);
        assert_eq!(cmd.rz_deg, 180000);
    }

    #[test]
    fn test_end_pose_control3_to_frame() {
        let cmd = EndPoseControl3::new(-45.0, 180.0);
        let frame = cmd.to_frame();

        assert_eq!(frame.id, ID_END_POSE_CONTROL_3);
        let ry_decoded =
            i32::from_be_bytes([frame.data[0], frame.data[1], frame.data[2], frame.data[3]]);
        let rz_decoded =
            i32::from_be_bytes([frame.data[4], frame.data[5], frame.data[6], frame.data[7]]);
        assert_eq!(ry_decoded, -45000);
        assert_eq!(rz_decoded, 180000);
    }

    #[test]
    fn test_end_pose_control_precision() {
        // 测试精度：0.5mm = 500 (0.001mm 单位)
        let cmd = EndPoseControl1::new(0.5, -0.5);
        assert_eq!(cmd.x_mm, 500);
        assert_eq!(cmd.y_mm, -500);
    }
}

// ============================================================================
// 圆弧模式坐标序号更新指令结构体
// ============================================================================

/// 圆弧模式坐标序号
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArcPointIndex {
    /// 无效
    Invalid = 0x00,
    /// 起点
    Start = 0x01,
    /// 中间点
    Middle = 0x02,
    /// 终点
    End = 0x03,
}

impl TryFrom<u8> for ArcPointIndex {
    type Error = ProtocolError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0x00 => Ok(ArcPointIndex::Invalid),
            0x01 => Ok(ArcPointIndex::Start),
            0x02 => Ok(ArcPointIndex::Middle),
            0x03 => Ok(ArcPointIndex::End),
            _ => Err(ProtocolError::InvalidValue {
                field: "ArcPointIndex".to_string(),
                value,
            }),
        }
    }
}

/// 圆弧模式坐标序号更新指令 (0x158)
///
/// 用于在圆弧模式（MOVE C）下更新坐标序号。
/// 只有 Byte 0 有效，其他字节填充 0x0。
#[derive(Debug, Clone, Copy)]
pub struct ArcPointCommand {
    pub point_index: ArcPointIndex,
}

impl ArcPointCommand {
    /// 创建起点指令
    pub fn start() -> Self {
        Self {
            point_index: ArcPointIndex::Start,
        }
    }

    /// 创建中间点指令
    pub fn middle() -> Self {
        Self {
            point_index: ArcPointIndex::Middle,
        }
    }

    /// 创建终点指令
    pub fn end() -> Self {
        Self {
            point_index: ArcPointIndex::End,
        }
    }

    /// 从枚举值创建
    pub fn new(point_index: ArcPointIndex) -> Self {
        Self { point_index }
    }

    /// 转换为 CAN 帧
    pub fn to_frame(self) -> PiperFrame {
        let mut data = [0u8; 8];
        data[0] = self.point_index as u8;
        // Byte 1-7: 保留，已初始化为 0

        PiperFrame::new_standard(ID_ARC_POINT as u16, &data)
    }
}

#[cfg(test)]
mod arc_point_tests {
    use super::*;

    #[test]
    fn test_arc_point_index_from_u8() {
        assert_eq!(
            ArcPointIndex::try_from(0x00).unwrap(),
            ArcPointIndex::Invalid
        );
        assert_eq!(ArcPointIndex::try_from(0x01).unwrap(), ArcPointIndex::Start);
        assert_eq!(
            ArcPointIndex::try_from(0x02).unwrap(),
            ArcPointIndex::Middle
        );
        assert_eq!(ArcPointIndex::try_from(0x03).unwrap(), ArcPointIndex::End);
    }

    #[test]
    fn test_arc_point_index_invalid() {
        assert!(ArcPointIndex::try_from(0x04).is_err());
        assert!(ArcPointIndex::try_from(0xFF).is_err());
    }

    #[test]
    fn test_arc_point_command_start() {
        let cmd = ArcPointCommand::start();
        let frame = cmd.to_frame();

        assert_eq!(frame.id, ID_ARC_POINT);
        assert_eq!(frame.data[0], 0x01);
        assert_eq!(frame.data[1], 0x00); // 保留字段
    }

    #[test]
    fn test_arc_point_command_middle() {
        let cmd = ArcPointCommand::middle();
        let frame = cmd.to_frame();

        assert_eq!(frame.id, ID_ARC_POINT);
        assert_eq!(frame.data[0], 0x02);
    }

    #[test]
    fn test_arc_point_command_end() {
        let cmd = ArcPointCommand::end();
        let frame = cmd.to_frame();

        assert_eq!(frame.id, ID_ARC_POINT);
        assert_eq!(frame.data[0], 0x03);
    }
}

// ============================================================================
// MIT 控制指令结构体
// ============================================================================

/// MIT 控制指令 (0x15A~0x15F)
///
/// 用于控制机械臂关节的 MIT 模式（主从模式）。
/// 包含位置参考、速度参考、比例增益、微分增益、力矩参考和 CRC 校验。
///
/// 注意：此指令使用复杂的跨字节位域打包，需要仔细处理。
#[derive(Debug, Clone, Copy)]
pub struct MitControlCommand {
    pub joint_index: u8, // 从 ID 推导：0x15A -> 1, 0x15B -> 2, ...
    pub pos_ref: f32,    // 位置参考值
    pub vel_ref: f32,    // 速度参考值
    pub kp: f32,         // 比例增益（参考值：10）
    pub kd: f32,         // 微分增益（参考值：0.8）
    pub t_ref: f32,      // 力矩参考值
    pub crc: u8,         // CRC 校验（4位，但存储为 u8）
}

impl MitControlCommand {
    /// 辅助函数：将浮点数转换为无符号整数（根据协议公式）
    ///
    /// 公式：`(x - x_min) * ((1 << bits) - 1) / (x_max - x_min)`
    fn float_to_uint(x: f32, x_min: f32, x_max: f32, bits: u32) -> u32 {
        let span = x_max - x_min;
        let offset = x_min;
        if span <= 0.0 {
            return 0;
        }
        let result = ((x - offset) * ((1u32 << bits) - 1) as f32 / span) as u32;
        result.min((1u32 << bits) - 1)
    }

    /// 辅助函数：将无符号整数转换为浮点数（根据协议公式）
    ///
    /// 公式：`x_int * (x_max - x_min) / ((1 << bits) - 1) + x_min`
    ///
    /// 注意：此函数目前仅用于测试，保留作为公共 API 以便将来可能需要解析 MIT 控制反馈。
    #[allow(dead_code)]
    pub fn uint_to_float(x_int: u32, x_min: f32, x_max: f32, bits: u32) -> f32 {
        let span = x_max - x_min;
        let offset = x_min;
        (x_int as f32) * span / ((1u32 << bits) - 1) as f32 + offset
    }

    /// 创建 MIT 控制指令
    ///
    /// 参数范围（根据官方 SDK，固定值，不要更改）：
    /// - pos_ref: -12.5 ~ 12.5
    /// - vel_ref: -45.0 ~ 45.0 rad/s
    /// - kp: 0.0 ~ 500.0
    /// - kd: -5.0 ~ 5.0
    /// - t_ref: -18.0 ~ 18.0 N·m
    ///
    /// # Arguments
    ///
    /// * `joint_index` - 关节序号 [1, 6]
    /// * `pos_ref` - 设定期望的目标位置
    /// * `vel_ref` - 设定电机运动的速度
    /// * `kp` - 比例增益，控制位置误差对输出力矩的影响
    /// * `kd` - 微分增益，控制速度误差对输出力矩的影响
    /// * `t_ref` - 目标力矩参考值，用于控制电机施加的力矩或扭矩
    /// * `crc` - CRC 校验值（4位）
    pub fn new(
        joint_index: u8,
        pos_ref: f32,
        vel_ref: f32,
        kp: f32,
        kd: f32,
        t_ref: f32,
        crc: u8,
    ) -> Self {
        Self {
            joint_index,
            pos_ref,
            vel_ref,
            kp,
            kd,
            t_ref,
            crc: crc & 0x0F, // 只保留低4位
        }
    }

    /// 转换为 CAN 帧
    ///
    /// 协议位域布局：
    /// - Byte 0-1: Pos_ref (16位)
    /// - Byte 2: Vel_ref [bit11~bit4] (8位)
    /// - Byte 3: Vel_ref [bit3~bit0] | Kp [bit11~bit8] (跨字节打包)
    /// - Byte 4: Kp [bit7~bit0] (8位)
    /// - Byte 5: Kd [bit11~bit4] (8位)
    /// - Byte 6: Kd [bit3~bit0] | T_ref [bit7~bit4] (跨字节打包)
    /// - Byte 7: T_ref [bit3~bit0] | CRC [bit3~bit0] (跨字节打包)
    ///
    /// 参数范围（根据官方 SDK）：
    /// - Pos_ref: -12.5 ~ 12.5 (16位)
    /// - Vel_ref: -45.0 ~ 45.0 rad/s (12位)
    /// - Kp: 0.0 ~ 500.0 (12位)
    /// - Kd: -5.0 ~ 5.0 (12位)
    /// - T_ref: -18.0 ~ 18.0 N·m (8位)
    pub fn to_frame(self) -> PiperFrame {
        let mut data = [0u8; 8];

        // Byte 0-1: Pos_ref (16位)
        // 范围：-12.5 ~ 12.5（根据官方 SDK）
        let pos_ref_uint = Self::float_to_uint(self.pos_ref, -12.5, 12.5, 16);
        data[0] = ((pos_ref_uint >> 8) & 0xFF) as u8;
        data[1] = (pos_ref_uint & 0xFF) as u8;

        // Byte 2-3: Vel_ref (12位) 和 Kp (12位) 的跨字节打包
        // Vel_ref 范围：-45.0 ~ 45.0 rad/s（根据官方 SDK）
        let vel_ref_uint = Self::float_to_uint(self.vel_ref, -45.0, 45.0, 12);
        data[2] = ((vel_ref_uint >> 4) & 0xFF) as u8; // Vel_ref [bit11~bit4]

        // Byte 3: Vel_ref [bit3~bit0] | Kp [bit11~bit8]
        // Kp 范围：0.0 ~ 500.0（根据官方 SDK）
        let kp_uint = Self::float_to_uint(self.kp, 0.0, 500.0, 12);
        let vel_ref_low = (vel_ref_uint & 0x0F) as u8;
        let kp_high = ((kp_uint >> 8) & 0x0F) as u8;
        data[3] = (vel_ref_low << 4) | kp_high;

        // Byte 4: Kp [bit7~bit0]
        data[4] = (kp_uint & 0xFF) as u8;

        // Byte 5-6: Kd (12位) 和 T_ref (8位) 的跨字节打包
        // Kd 范围：-5.0 ~ 5.0（根据官方 SDK）
        let kd_uint = Self::float_to_uint(self.kd, -5.0, 5.0, 12);
        data[5] = ((kd_uint >> 4) & 0xFF) as u8; // Kd [bit11~bit4]

        // Byte 6: Kd [bit3~bit0] | T_ref [bit7~bit4]
        // T_ref 范围：-18.0 ~ 18.0 N·m（根据官方 SDK）
        let t_ref_uint = Self::float_to_uint(self.t_ref, -18.0, 18.0, 8);
        let kd_low = (kd_uint & 0x0F) as u8;
        let t_ref_high = ((t_ref_uint >> 4) & 0x0F) as u8;
        data[6] = (kd_low << 4) | t_ref_high;

        // Byte 7: T_ref [bit3~bit0] | CRC [bit3~bit0]
        let t_ref_low = (t_ref_uint & 0x0F) as u8;
        let crc_low = self.crc & 0x0F;
        data[7] = (t_ref_low << 4) | crc_low;

        let can_id = ID_MIT_CONTROL_BASE + (self.joint_index - 1) as u32;
        PiperFrame::new_standard(can_id as u16, &data)
    }
}

#[cfg(test)]
mod mit_control_tests {
    use super::*;

    #[test]
    fn test_float_to_uint() {
        // 测试转换公式：范围 0.0 ~ 10.0，12位
        let result = MitControlCommand::float_to_uint(5.0, 0.0, 10.0, 12);
        // 期望：5.0 / 10.0 * 4095 = 2047.5 ≈ 2047
        assert_eq!(result, 2047);
    }

    #[test]
    fn test_float_to_uint_boundary() {
        // 测试边界值
        let min = MitControlCommand::float_to_uint(0.0, 0.0, 10.0, 12);
        let max = MitControlCommand::float_to_uint(10.0, 0.0, 10.0, 12);
        assert_eq!(min, 0);
        assert_eq!(max, 4095);
    }

    #[test]
    fn test_uint_to_float() {
        // 测试转换公式：范围 0.0 ~ 10.0，12位
        let result = MitControlCommand::uint_to_float(2047, 0.0, 10.0, 12);
        // 期望：2047 / 4095 * 10.0 ≈ 5.0
        assert!((result - 5.0).abs() < 0.01);
    }

    #[test]
    fn test_uint_to_float_boundary() {
        // 测试边界值
        let min = MitControlCommand::uint_to_float(0, 0.0, 10.0, 12);
        let max = MitControlCommand::uint_to_float(4095, 0.0, 10.0, 12);
        assert!((min - 0.0).abs() < 0.001);
        assert!((max - 10.0).abs() < 0.001);
    }

    #[test]
    fn test_mit_control_command_new() {
        let cmd = MitControlCommand::new(1, 1.0, 2.0, 10.0, 0.8, 5.0, 0x0A);
        assert_eq!(cmd.joint_index, 1);
        assert_eq!(cmd.pos_ref, 1.0);
        assert_eq!(cmd.vel_ref, 2.0);
        assert_eq!(cmd.kp, 10.0);
        assert_eq!(cmd.kd, 0.8);
        assert_eq!(cmd.t_ref, 5.0);
        assert_eq!(cmd.crc, 0x0A);
    }

    #[test]
    fn test_mit_control_command_crc_mask() {
        // 测试 CRC 只保留低4位
        let cmd = MitControlCommand::new(1, 0.0, 0.0, 0.0, 0.0, 0.0, 0xFF);
        assert_eq!(cmd.crc, 0x0F);
    }

    #[test]
    fn test_mit_control_command_to_frame() {
        // 使用官方 SDK 的参考值：kp=10, kd=0.8
        let cmd = MitControlCommand::new(1, 0.0, 0.0, 10.0, 0.8, 0.0, 0x05);
        let frame = cmd.to_frame();

        assert_eq!(frame.id, ID_MIT_CONTROL_BASE);
        // 验证 CRC 在 Byte 7 的低4位
        assert_eq!(frame.data[7] & 0x0F, 0x05);
    }

    #[test]
    fn test_mit_control_command_with_official_ranges() {
        // 测试使用官方 SDK 的参数范围
        // pos_ref: -12.5 ~ 12.5
        // vel_ref: -45.0 ~ 45.0
        // kp: 0.0 ~ 500.0
        // kd: -5.0 ~ 5.0
        // t_ref: -18.0 ~ 18.0

        // 测试边界值
        let cmd_min = MitControlCommand::new(1, -12.5, -45.0, 0.0, -5.0, -18.0, 0x00);
        let frame_min = cmd_min.to_frame();
        assert_eq!(frame_min.id, ID_MIT_CONTROL_BASE);

        let cmd_max = MitControlCommand::new(1, 12.5, 45.0, 500.0, 5.0, 18.0, 0x0F);
        let frame_max = cmd_max.to_frame();
        assert_eq!(frame_max.id, ID_MIT_CONTROL_BASE);

        // 测试参考值（根据协议文档）
        let cmd_ref = MitControlCommand::new(1, 0.0, 0.0, 10.0, 0.8, 0.0, 0x00);
        let frame_ref = cmd_ref.to_frame();
        assert_eq!(frame_ref.id, ID_MIT_CONTROL_BASE);
    }

    #[test]
    fn test_mit_control_command_all_joints() {
        // 测试所有 6 个关节
        for i in 1..=6 {
            let cmd = MitControlCommand::new(i, 0.0, 0.0, 10.0, 0.8, 0.0, 0x00);
            let frame = cmd.to_frame();
            let expected_id = ID_MIT_CONTROL_BASE + (i - 1) as u32;
            assert_eq!(frame.id, expected_id);
        }
    }

    #[test]
    fn test_mit_control_command_roundtrip() {
        // 测试转换函数的往返转换
        let original = 5.0f32;
        let x_min = 0.0f32;
        let x_max = 10.0f32;
        let bits = 12u32;

        let uint_val = MitControlCommand::float_to_uint(original, x_min, x_max, bits);
        let float_val = MitControlCommand::uint_to_float(uint_val, x_min, x_max, bits);

        // 由于精度损失，允许一定误差
        assert!((float_val - original).abs() < 0.01);
    }
}

// ============================================================================
// 灯光控制指令
// ============================================================================

/// 灯光控制使能标志
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LightControlEnable {
    /// 控制指令无效
    #[default]
    Disabled = 0x00,
    /// 灯光控制使能
    Enabled = 0x01,
}

impl From<u8> for LightControlEnable {
    fn from(value: u8) -> Self {
        match value {
            0x00 => Self::Disabled,
            0x01 => Self::Enabled,
            _ => Self::Disabled, // 默认无效
        }
    }
}

/// 灯光控制指令 (0x121)
///
/// 用于控制关节上的 LED 灯光。
#[derive(Debug, Clone, Copy)]
pub struct LightControlCommand {
    pub enable: LightControlEnable, // Byte 0: 灯光控制使能标志
    pub joint_index: u8,            // Byte 1: 关节序号 (1~6)
    pub led_index: u8,              // Byte 2: 灯珠序号 (0-254, 0xFF表示同时操作全部)
    pub r: u8,                      // Byte 3: R通道灰度值 (0~255)
    pub g: u8,                      // Byte 4: G通道灰度值 (0~255)
    pub b: u8,                      // Byte 5: B通道灰度值 (0~255)
    pub counter: u8,                // Byte 7: 计数校验 (0-255循环计数)
                                    // Byte 6: 保留
}

impl LightControlCommand {
    /// 创建灯光控制指令
    pub fn new(
        enable: LightControlEnable,
        joint_index: u8,
        led_index: u8,
        r: u8,
        g: u8,
        b: u8,
        counter: u8,
    ) -> Self {
        Self {
            enable,
            joint_index,
            led_index,
            r,
            g,
            b,
            counter,
        }
    }

    /// 转换为 CAN 帧
    pub fn to_frame(self) -> PiperFrame {
        let mut data = [0u8; 8];
        data[0] = self.enable as u8;
        data[1] = self.joint_index;
        data[2] = self.led_index;
        data[3] = self.r;
        data[4] = self.g;
        data[5] = self.b;
        // Byte 6: 保留，已初始化为 0
        data[7] = self.counter;

        PiperFrame::new_standard(ID_LIGHT_CONTROL as u16, &data)
    }
}

#[cfg(test)]
mod light_control_tests {
    use super::*;

    #[test]
    fn test_light_control_enable_from() {
        assert_eq!(LightControlEnable::from(0x00), LightControlEnable::Disabled);
        assert_eq!(LightControlEnable::from(0x01), LightControlEnable::Enabled);
        assert_eq!(LightControlEnable::from(0xFF), LightControlEnable::Disabled); // 默认无效
    }

    #[test]
    fn test_light_control_command_new() {
        let cmd = LightControlCommand::new(LightControlEnable::Enabled, 1, 0xFF, 255, 128, 0, 10);
        assert_eq!(cmd.enable, LightControlEnable::Enabled);
        assert_eq!(cmd.joint_index, 1);
        assert_eq!(cmd.led_index, 0xFF);
        assert_eq!(cmd.r, 255);
        assert_eq!(cmd.g, 128);
        assert_eq!(cmd.b, 0);
        assert_eq!(cmd.counter, 10);
    }

    #[test]
    fn test_light_control_command_to_frame() {
        let cmd = LightControlCommand::new(LightControlEnable::Enabled, 2, 5, 100, 200, 50, 42);
        let frame = cmd.to_frame();

        assert_eq!(frame.id, ID_LIGHT_CONTROL);
        assert_eq!(frame.data[0], 0x01); // Enabled
        assert_eq!(frame.data[1], 2); // joint_index
        assert_eq!(frame.data[2], 5); // led_index
        assert_eq!(frame.data[3], 100); // R
        assert_eq!(frame.data[4], 200); // G
        assert_eq!(frame.data[5], 50); // B
        assert_eq!(frame.data[6], 0x00); // 保留
        assert_eq!(frame.data[7], 42); // counter
    }

    #[test]
    fn test_light_control_command_all_leds() {
        // 测试 0xFF 表示同时操作全部灯珠
        let cmd = LightControlCommand::new(LightControlEnable::Enabled, 3, 0xFF, 255, 255, 255, 0);
        let frame = cmd.to_frame();
        assert_eq!(frame.data[2], 0xFF);
    }

    #[test]
    fn test_light_control_command_disabled() {
        let cmd = LightControlCommand::new(LightControlEnable::Disabled, 1, 0, 0, 0, 0, 0);
        let frame = cmd.to_frame();
        assert_eq!(frame.data[0], 0x00);
    }
}
