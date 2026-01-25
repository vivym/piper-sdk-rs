//! 配置帧结构体定义
//!
//! 包含配置查询和设置指令的结构体，以及配置反馈帧。

use crate::can::PiperFrame;
use crate::{ProtocolError, bytes_to_i16_be, i16_to_bytes_be, ids::*};

// ============================================================================
// 随动主从模式设置指令
// ============================================================================

/// 联动设置指令
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkSetting {
    /// 无效
    Invalid = 0x00,
    /// 设置为示教输入臂
    TeachInputArm = 0xFA,
    /// 设置为运动输出臂
    MotionOutputArm = 0xFC,
}

impl TryFrom<u8> for LinkSetting {
    type Error = ProtocolError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0x00 => Ok(LinkSetting::Invalid),
            0xFA => Ok(LinkSetting::TeachInputArm),
            0xFC => Ok(LinkSetting::MotionOutputArm),
            _ => Err(ProtocolError::InvalidValue {
                field: "LinkSetting".to_string(),
                value,
            }),
        }
    }
}

/// 反馈指令偏移值
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeedbackIdOffset {
    /// 不偏移/恢复默认
    None = 0x00,
    /// 反馈指令基ID由2Ax偏移为2Bx
    Offset2Bx = 0x10,
    /// 反馈指令基ID由2Ax偏移为2Cx
    Offset2Cx = 0x20,
}

impl TryFrom<u8> for FeedbackIdOffset {
    type Error = ProtocolError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0x00 => Ok(FeedbackIdOffset::None),
            0x10 => Ok(FeedbackIdOffset::Offset2Bx),
            0x20 => Ok(FeedbackIdOffset::Offset2Cx),
            _ => Err(ProtocolError::InvalidValue {
                field: "FeedbackIdOffset".to_string(),
                value,
            }),
        }
    }
}

/// 控制指令偏移值
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlIdOffset {
    /// 不偏移/恢复默认
    None = 0x00,
    /// 控制指令基ID由15x偏移为16x
    Offset16x = 0x10,
    /// 控制指令基ID由15x偏移为17x
    Offset17x = 0x20,
}

impl TryFrom<u8> for ControlIdOffset {
    type Error = ProtocolError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0x00 => Ok(ControlIdOffset::None),
            0x10 => Ok(ControlIdOffset::Offset16x),
            0x20 => Ok(ControlIdOffset::Offset17x),
            _ => Err(ProtocolError::InvalidValue {
                field: "ControlIdOffset".to_string(),
                value,
            }),
        }
    }
}

/// 随动主从模式设置指令 (0x470)
///
/// 用于设置机械臂为示教输入臂或运动输出臂，并配置 ID 偏移值。
/// 协议长度：4 字节，但 CAN 帧为 8 字节（后 4 字节保留）。
#[derive(Debug, Clone, Copy)]
pub struct MasterSlaveModeCommand {
    pub link_setting: LinkSetting,            // Byte 0: 联动设置指令
    pub feedback_id_offset: FeedbackIdOffset, // Byte 1: 反馈指令偏移值
    pub control_id_offset: ControlIdOffset,   // Byte 2: 控制指令偏移值
    pub target_id_offset: ControlIdOffset,    // Byte 3: 联动模式控制目标地址偏移值
}

impl MasterSlaveModeCommand {
    /// 创建设置为示教输入臂的指令
    pub fn set_teach_input_arm(
        feedback_id_offset: FeedbackIdOffset,
        control_id_offset: ControlIdOffset,
        target_id_offset: ControlIdOffset,
    ) -> Self {
        Self {
            link_setting: LinkSetting::TeachInputArm,
            feedback_id_offset,
            control_id_offset,
            target_id_offset,
        }
    }

    /// 创建设置为运动输出臂的指令（恢复常规状态）
    pub fn set_motion_output_arm() -> Self {
        Self {
            link_setting: LinkSetting::MotionOutputArm,
            feedback_id_offset: FeedbackIdOffset::None,
            control_id_offset: ControlIdOffset::None,
            target_id_offset: ControlIdOffset::None,
        }
    }

    /// 转换为 CAN 帧
    pub fn to_frame(self) -> PiperFrame {
        let mut data = [0u8; 8];
        data[0] = self.link_setting as u8;
        data[1] = self.feedback_id_offset as u8;
        data[2] = self.control_id_offset as u8;
        data[3] = self.target_id_offset as u8;
        // Byte 4-7: 保留，已初始化为 0

        PiperFrame::new_standard(ID_MASTER_SLAVE_MODE as u16, &data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_link_setting_from_u8() {
        assert_eq!(LinkSetting::try_from(0x00).unwrap(), LinkSetting::Invalid);
        assert_eq!(
            LinkSetting::try_from(0xFA).unwrap(),
            LinkSetting::TeachInputArm
        );
        assert_eq!(
            LinkSetting::try_from(0xFC).unwrap(),
            LinkSetting::MotionOutputArm
        );
    }

    #[test]
    fn test_feedback_id_offset_from_u8() {
        assert_eq!(
            FeedbackIdOffset::try_from(0x00).unwrap(),
            FeedbackIdOffset::None
        );
        assert_eq!(
            FeedbackIdOffset::try_from(0x10).unwrap(),
            FeedbackIdOffset::Offset2Bx
        );
        assert_eq!(
            FeedbackIdOffset::try_from(0x20).unwrap(),
            FeedbackIdOffset::Offset2Cx
        );
    }

    #[test]
    fn test_control_id_offset_from_u8() {
        assert_eq!(
            ControlIdOffset::try_from(0x00).unwrap(),
            ControlIdOffset::None
        );
        assert_eq!(
            ControlIdOffset::try_from(0x10).unwrap(),
            ControlIdOffset::Offset16x
        );
        assert_eq!(
            ControlIdOffset::try_from(0x20).unwrap(),
            ControlIdOffset::Offset17x
        );
    }

    #[test]
    fn test_master_slave_mode_command_set_teach_input_arm() {
        let cmd = MasterSlaveModeCommand::set_teach_input_arm(
            FeedbackIdOffset::Offset2Bx,
            ControlIdOffset::Offset16x,
            ControlIdOffset::Offset16x,
        );
        let frame = cmd.to_frame();

        assert_eq!(frame.id, ID_MASTER_SLAVE_MODE);
        assert_eq!(frame.data[0], 0xFA); // TeachInputArm
        assert_eq!(frame.data[1], 0x10); // Offset2Bx
        assert_eq!(frame.data[2], 0x10); // Offset16x
        assert_eq!(frame.data[3], 0x10); // Offset16x
        assert_eq!(frame.data[4], 0x00); // 保留
    }

    #[test]
    fn test_master_slave_mode_command_set_motion_output_arm() {
        let cmd = MasterSlaveModeCommand::set_motion_output_arm();
        let frame = cmd.to_frame();

        assert_eq!(frame.id, ID_MASTER_SLAVE_MODE);
        assert_eq!(frame.data[0], 0xFC); // MotionOutputArm
        assert_eq!(frame.data[1], 0x00); // None
        assert_eq!(frame.data[2], 0x00); // None
        assert_eq!(frame.data[3], 0x00); // None
    }
}

// ============================================================================
// 查询电机限制指令和反馈
// ============================================================================

/// 查询内容类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryType {
    /// 查询电机角度/最大速度
    AngleAndMaxVelocity = 0x01,
    /// 查询电机最大加速度限制
    MaxAcceleration = 0x02,
}

impl TryFrom<u8> for QueryType {
    type Error = ProtocolError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0x01 => Ok(QueryType::AngleAndMaxVelocity),
            0x02 => Ok(QueryType::MaxAcceleration),
            _ => Err(ProtocolError::InvalidValue {
                field: "QueryType".to_string(),
                value,
            }),
        }
    }
}

/// 查询电机限制指令 (0x472)
#[derive(Debug, Clone, Copy)]
pub struct QueryMotorLimitCommand {
    pub joint_index: u8,       // Byte 0: 关节电机序号（1-6）
    pub query_type: QueryType, // Byte 1: 查询内容
}

impl QueryMotorLimitCommand {
    /// 创建查询电机角度/最大速度指令
    pub fn query_angle_and_max_velocity(joint_index: u8) -> Self {
        Self {
            joint_index,
            query_type: QueryType::AngleAndMaxVelocity,
        }
    }

    /// 创建查询电机最大加速度限制指令
    pub fn query_max_acceleration(joint_index: u8) -> Self {
        Self {
            joint_index,
            query_type: QueryType::MaxAcceleration,
        }
    }

    /// 转换为 CAN 帧
    pub fn to_frame(self) -> PiperFrame {
        let mut data = [0u8; 8];
        data[0] = self.joint_index;
        data[1] = self.query_type as u8;
        // Byte 2-7: 保留，已初始化为 0

        PiperFrame::new_standard(ID_QUERY_MOTOR_LIMIT as u16, &data)
    }
}

/// 反馈当前电机限制角度/最大速度 (0x473)
///
/// 单位：
/// - 角度限制：0.1°（原始值）
/// - 最大关节速度：0.01rad/s（原始值）
#[derive(Debug, Clone, Copy, Default)]
pub struct MotorLimitFeedback {
    pub joint_index: u8,    // Byte 0: 关节电机序号（1-6）
    pub max_angle_deg: i16, // Byte 1-2: 最大角度限制，单位 0.1°
    pub min_angle_deg: i16, // Byte 3-4: 最小角度限制，单位 0.1°
    pub max_velocity_rad_s: u16, // Byte 5-6: 最大关节速度，单位 0.01rad/s
                            // Byte 7: 保留
}

impl MotorLimitFeedback {
    /// 获取最大角度（度）
    pub fn max_angle(&self) -> f64 {
        self.max_angle_deg as f64 / 10.0
    }

    /// 获取最小角度（度）
    pub fn min_angle(&self) -> f64 {
        self.min_angle_deg as f64 / 10.0
    }

    /// 获取最大速度（rad/s）
    pub fn max_velocity(&self) -> f64 {
        self.max_velocity_rad_s as f64 / 100.0
    }
}

impl TryFrom<PiperFrame> for MotorLimitFeedback {
    type Error = ProtocolError;

    fn try_from(frame: PiperFrame) -> Result<Self, Self::Error> {
        // 验证 CAN ID
        if frame.id != ID_MOTOR_LIMIT_FEEDBACK {
            return Err(ProtocolError::InvalidCanId { id: frame.id });
        }

        // 验证数据长度
        if frame.len < 7 {
            return Err(ProtocolError::InvalidLength {
                expected: 7,
                actual: frame.len as usize,
            });
        }

        let joint_index = frame.data[0];

        // 大端字节序
        let max_angle_bytes = [frame.data[1], frame.data[2]];
        let max_angle_deg = bytes_to_i16_be(max_angle_bytes);

        let min_angle_bytes = [frame.data[3], frame.data[4]];
        let min_angle_deg = bytes_to_i16_be(min_angle_bytes);

        let max_velocity_bytes = [frame.data[5], frame.data[6]];
        let max_velocity_rad_s = u16::from_be_bytes(max_velocity_bytes);

        Ok(Self {
            joint_index,
            max_angle_deg,
            min_angle_deg,
            max_velocity_rad_s,
        })
    }
}

#[cfg(test)]
mod motor_limit_tests {
    use super::*;

    #[test]
    fn test_query_type_from_u8() {
        assert_eq!(
            QueryType::try_from(0x01).unwrap(),
            QueryType::AngleAndMaxVelocity
        );
        assert_eq!(
            QueryType::try_from(0x02).unwrap(),
            QueryType::MaxAcceleration
        );
    }

    #[test]
    fn test_query_motor_limit_command_query_angle_and_max_velocity() {
        let cmd = QueryMotorLimitCommand::query_angle_and_max_velocity(1);
        let frame = cmd.to_frame();

        assert_eq!(frame.id, ID_QUERY_MOTOR_LIMIT);
        assert_eq!(frame.data[0], 1);
        assert_eq!(frame.data[1], 0x01); // AngleAndMaxVelocity
    }

    #[test]
    fn test_query_motor_limit_command_query_max_acceleration() {
        let cmd = QueryMotorLimitCommand::query_max_acceleration(2);
        let frame = cmd.to_frame();

        assert_eq!(frame.id, ID_QUERY_MOTOR_LIMIT);
        assert_eq!(frame.data[0], 2);
        assert_eq!(frame.data[1], 0x02); // MaxAcceleration
    }

    #[test]
    fn test_motor_limit_feedback_parse() {
        // 测试数据：
        // - 最大角度: 180.0° = 1800 (0.1° 单位)
        // - 最小角度: -180.0° = -1800 (0.1° 单位)
        // - 最大速度: 5.0 rad/s = 500 (0.01rad/s 单位)
        let max_angle_val = 1800i16;
        let min_angle_val = -1800i16;
        let max_velocity_val = 500u16;

        let mut data = [0u8; 8];
        data[0] = 1; // 关节 1
        data[1..3].copy_from_slice(&max_angle_val.to_be_bytes());
        data[3..5].copy_from_slice(&min_angle_val.to_be_bytes());
        data[5..7].copy_from_slice(&max_velocity_val.to_be_bytes());

        let frame = PiperFrame::new_standard(ID_MOTOR_LIMIT_FEEDBACK as u16, &data);
        let feedback = MotorLimitFeedback::try_from(frame).unwrap();

        assert_eq!(feedback.joint_index, 1);
        assert_eq!(feedback.max_angle_deg, 1800);
        assert_eq!(feedback.min_angle_deg, -1800);
        assert_eq!(feedback.max_velocity_rad_s, 500);
        assert!((feedback.max_angle() - 180.0).abs() < 0.0001);
        assert!((feedback.min_angle() - (-180.0)).abs() < 0.0001);
        assert!((feedback.max_velocity() - 5.0).abs() < 0.0001);
    }

    #[test]
    fn test_motor_limit_feedback_invalid_id() {
        let frame = PiperFrame::new_standard(0x999, &[0; 8]);
        let result = MotorLimitFeedback::try_from(frame);
        assert!(result.is_err());
    }

    #[test]
    fn test_motor_limit_feedback_invalid_length() {
        let frame = PiperFrame::new_standard(ID_MOTOR_LIMIT_FEEDBACK as u16, &[0; 4]);
        let result = MotorLimitFeedback::try_from(frame);
        assert!(result.is_err());
    }
}

// ============================================================================
// 设置电机限制指令
// ============================================================================

/// 电机角度限制/最大速度设置指令 (0x474)
///
/// 用于设置电机的角度限制和最大速度。
/// 单位：
/// - 角度限制：0.1°（原始值），无效值：0x7FFF
/// - 最大关节速度：0.01rad/s（原始值），无效值：0x7FFF
#[derive(Debug, Clone, Copy)]
pub struct SetMotorLimitCommand {
    pub joint_index: u8,                 // Byte 0: 关节电机序号（1-6）
    pub max_angle_deg: Option<i16>,      // Byte 1-2: 最大角度限制，单位 0.1°，None 表示无效值
    pub min_angle_deg: Option<i16>,      // Byte 3-4: 最小角度限制，单位 0.1°，None 表示无效值
    pub max_velocity_rad_s: Option<u16>, // Byte 5-6: 最大关节速度，单位 0.01rad/s，None 表示无效值
}

impl SetMotorLimitCommand {
    /// 从物理量创建设置指令
    pub fn new(
        joint_index: u8,
        max_angle: Option<f64>,
        min_angle: Option<f64>,
        max_velocity: Option<f64>,
    ) -> Self {
        Self {
            joint_index,
            max_angle_deg: max_angle.map(|a| (a * 10.0) as i16),
            min_angle_deg: min_angle.map(|a| (a * 10.0) as i16),
            max_velocity_rad_s: max_velocity.map(|v| (v * 100.0) as u16),
        }
    }

    /// 转换为 CAN 帧
    pub fn to_frame(self) -> PiperFrame {
        let mut data = [0u8; 8];
        data[0] = self.joint_index;

        // 大端字节序
        let max_angle_bytes = if let Some(angle) = self.max_angle_deg {
            i16_to_bytes_be(angle)
        } else {
            [0x7F, 0xFF] // 无效值
        };
        data[1..3].copy_from_slice(&max_angle_bytes);

        let min_angle_bytes = if let Some(angle) = self.min_angle_deg {
            i16_to_bytes_be(angle)
        } else {
            [0x7F, 0xFF] // 无效值
        };
        data[3..5].copy_from_slice(&min_angle_bytes);

        let max_velocity_bytes = if let Some(velocity) = self.max_velocity_rad_s {
            velocity.to_be_bytes()
        } else {
            [0x7F, 0xFF] // 无效值
        };
        data[5..7].copy_from_slice(&max_velocity_bytes);

        PiperFrame::new_standard(ID_SET_MOTOR_LIMIT as u16, &data)
    }
}

#[cfg(test)]
mod set_motor_limit_tests {
    use super::*;

    #[test]
    fn test_set_motor_limit_command_new() {
        let cmd = SetMotorLimitCommand::new(
            1,
            Some(180.0),  // 最大角度 180°
            Some(-180.0), // 最小角度 -180°
            Some(5.0),    // 最大速度 5.0 rad/s
        );
        assert_eq!(cmd.joint_index, 1);
        assert_eq!(cmd.max_angle_deg, Some(1800));
        assert_eq!(cmd.min_angle_deg, Some(-1800));
        assert_eq!(cmd.max_velocity_rad_s, Some(500));
    }

    #[test]
    fn test_set_motor_limit_command_to_frame() {
        let cmd = SetMotorLimitCommand::new(1, Some(180.0), Some(-180.0), Some(5.0));
        let frame = cmd.to_frame();

        assert_eq!(frame.id, ID_SET_MOTOR_LIMIT);
        assert_eq!(frame.data[0], 1);

        // 验证大端字节序
        let max_angle = i16::from_be_bytes([frame.data[1], frame.data[2]]);
        let min_angle = i16::from_be_bytes([frame.data[3], frame.data[4]]);
        let max_velocity = u16::from_be_bytes([frame.data[5], frame.data[6]]);
        assert_eq!(max_angle, 1800);
        assert_eq!(min_angle, -1800);
        assert_eq!(max_velocity, 500);
    }

    #[test]
    fn test_set_motor_limit_command_invalid_values() {
        // 测试无效值（None）
        let cmd = SetMotorLimitCommand::new(1, None, None, None);
        let frame = cmd.to_frame();

        // 验证无效值编码为 0x7FFF
        assert_eq!(frame.data[1], 0x7F);
        assert_eq!(frame.data[2], 0xFF);
        assert_eq!(frame.data[3], 0x7F);
        assert_eq!(frame.data[4], 0xFF);
        assert_eq!(frame.data[5], 0x7F);
        assert_eq!(frame.data[6], 0xFF);
    }

    #[test]
    fn test_set_motor_limit_command_partial_values() {
        // 测试部分有效值
        let cmd = SetMotorLimitCommand::new(
            2,
            Some(90.0), // 只设置最大角度
            None,       // 最小角度无效
            Some(3.0),  // 设置最大速度
        );
        let frame = cmd.to_frame();

        let max_angle = i16::from_be_bytes([frame.data[1], frame.data[2]]);
        let min_angle = i16::from_be_bytes([frame.data[3], frame.data[4]]);
        let max_velocity = u16::from_be_bytes([frame.data[5], frame.data[6]]);

        assert_eq!(max_angle, 900);
        assert_eq!(min_angle, 0x7FFF); // 无效值
        assert_eq!(max_velocity, 300);
    }
}

// ============================================================================
// 关节设置指令
// ============================================================================

/// 关节设置指令 (0x475)
///
/// 用于设置关节的零点、加速度参数和清除错误代码。
/// 单位：
/// - 最大关节加速度：0.01rad/s²（原始值），无效值：0x7FFF
///   特殊值：
/// - 0xAE: 表示设置生效/清除错误
#[derive(Debug, Clone, Copy)]
pub struct JointSettingCommand {
    pub joint_index: u8,               // Byte 0: 关节电机序号（1-7，7代表全部）
    pub set_zero_point: bool,          // Byte 1: 设置当前位置为零点（0xAE 表示设置）
    pub accel_param_enable: bool,      // Byte 2: 加速度参数设置是否生效（0xAE 表示生效）
    pub max_accel_rad_s2: Option<u16>, // Byte 3-4: 最大关节加速度，单位 0.01rad/s²，None 表示无效值
    pub clear_error: bool,             // Byte 5: 清除关节错误代码（0xAE 表示清除）
}

impl JointSettingCommand {
    /// 创建设置零点指令
    pub fn set_zero_point(joint_index: u8) -> Self {
        Self {
            joint_index,
            set_zero_point: true,
            accel_param_enable: false,
            max_accel_rad_s2: None,
            clear_error: false,
        }
    }

    /// 创建设置加速度参数指令
    pub fn set_acceleration(joint_index: u8, max_accel: f64) -> Self {
        Self {
            joint_index,
            set_zero_point: false,
            accel_param_enable: true,
            max_accel_rad_s2: Some((max_accel * 100.0) as u16),
            clear_error: false,
        }
    }

    /// 创建清除错误指令
    pub fn clear_error(joint_index: u8) -> Self {
        Self {
            joint_index,
            set_zero_point: false,
            accel_param_enable: false,
            max_accel_rad_s2: None,
            clear_error: true,
        }
    }

    /// 转换为 CAN 帧
    pub fn to_frame(self) -> PiperFrame {
        let mut data = [0u8; 8];
        data[0] = self.joint_index;
        data[1] = if self.set_zero_point { 0xAE } else { 0x00 };
        data[2] = if self.accel_param_enable { 0xAE } else { 0x00 };

        // 大端字节序
        let accel_bytes = if let Some(accel) = self.max_accel_rad_s2 {
            accel.to_be_bytes()
        } else {
            [0x7F, 0xFF] // 无效值
        };
        data[3..5].copy_from_slice(&accel_bytes);

        data[5] = if self.clear_error { 0xAE } else { 0x00 };
        // Byte 6-7: 保留，已初始化为 0

        PiperFrame::new_standard(ID_JOINT_SETTING as u16, &data)
    }
}

#[cfg(test)]
mod joint_setting_tests {
    use super::*;

    #[test]
    fn test_joint_setting_command_set_zero_point() {
        let cmd = JointSettingCommand::set_zero_point(1);
        let frame = cmd.to_frame();

        assert_eq!(frame.id, ID_JOINT_SETTING);
        assert_eq!(frame.data[0], 1);
        assert_eq!(frame.data[1], 0xAE); // 设置零点
        assert_eq!(frame.data[2], 0x00);
        assert_eq!(frame.data[5], 0x00);
    }

    #[test]
    fn test_joint_setting_command_set_acceleration() {
        let cmd = JointSettingCommand::set_acceleration(2, 10.0); // 10.0 rad/s²
        let frame = cmd.to_frame();

        assert_eq!(frame.id, ID_JOINT_SETTING);
        assert_eq!(frame.data[0], 2);
        assert_eq!(frame.data[1], 0x00);
        assert_eq!(frame.data[2], 0xAE); // 加速度参数生效

        let max_accel = u16::from_be_bytes([frame.data[3], frame.data[4]]);
        assert_eq!(max_accel, 1000); // 10.0 * 100 = 1000
    }

    #[test]
    fn test_joint_setting_command_clear_error() {
        let cmd = JointSettingCommand::clear_error(3);
        let frame = cmd.to_frame();

        assert_eq!(frame.id, ID_JOINT_SETTING);
        assert_eq!(frame.data[0], 3);
        assert_eq!(frame.data[1], 0x00);
        assert_eq!(frame.data[2], 0x00);
        assert_eq!(frame.data[5], 0xAE); // 清除错误
    }

    #[test]
    fn test_joint_setting_command_all_joints() {
        // 测试全部关节（joint_index = 7）
        let cmd = JointSettingCommand::set_zero_point(7);
        let frame = cmd.to_frame();
        assert_eq!(frame.data[0], 7);
    }

    #[test]
    fn test_joint_setting_command_invalid_accel() {
        // 测试无效加速度值（None）
        let mut cmd = JointSettingCommand::set_acceleration(1, 10.0);
        cmd.max_accel_rad_s2 = None;
        let frame = cmd.to_frame();

        // 验证无效值编码为 0x7FFF
        assert_eq!(frame.data[3], 0x7F);
        assert_eq!(frame.data[4], 0xFF);
    }
}

// ============================================================================
// 设置指令应答
// ============================================================================

/// 轨迹包传输完成应答状态（Byte 3）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrajectoryPackCompleteStatus {
    /// 传输完成且校验成功
    Success,
    /// 校验失败，需要整包重传
    ChecksumFailed,
    /// 其他状态值（未定义）
    Other(u8),
}

impl From<u8> for TrajectoryPackCompleteStatus {
    fn from(value: u8) -> Self {
        match value {
            0xAE => TrajectoryPackCompleteStatus::Success,
            0xEE => TrajectoryPackCompleteStatus::ChecksumFailed,
            _ => TrajectoryPackCompleteStatus::Other(value),
        }
    }
}

impl From<TrajectoryPackCompleteStatus> for u8 {
    fn from(status: TrajectoryPackCompleteStatus) -> Self {
        match status {
            TrajectoryPackCompleteStatus::Success => 0xAE,
            TrajectoryPackCompleteStatus::ChecksumFailed => 0xEE,
            TrajectoryPackCompleteStatus::Other(val) => val,
        }
    }
}

/// 设置指令应答 (0x476)
///
/// 用于应答设置指令的执行结果。
/// 注意：此帧有两种用途：
/// 1. 设置指令应答（Byte 0 = 设置指令 ID 的最后一个字节，如 0x471 -> 0x71）
/// 2. 轨迹传输应答（Byte 0 = 0x50，Byte 2 = 轨迹点索引，Byte 3 = 轨迹包传输完成应答）
#[derive(Debug, Clone, Copy)]
pub struct SettingResponse {
    pub response_index: u8,       // Byte 0: 应答指令索引
    pub zero_point_success: bool, // Byte 1: 零点是否设置成功（0x01: 成功，0x00: 失败/未设置）
    pub trajectory_index: u8,     // Byte 2: 轨迹点索引（仅用于轨迹传输应答）
    pub pack_complete_status: Option<TrajectoryPackCompleteStatus>, // Byte 3: 轨迹包传输完成应答（0xAE: 成功, 0xEE: 失败）
    pub name_index: u16, // Byte 4-5: 轨迹包名称索引（仅用于轨迹传输应答）
    pub crc16: u16,      // Byte 6-7: CRC16 校验（仅用于轨迹传输应答）
}

impl SettingResponse {
    /// 判断是否为轨迹传输应答
    pub fn is_trajectory_response(&self) -> bool {
        self.response_index == 0x50
    }

    /// 判断是否为设置指令应答
    pub fn is_setting_response(&self) -> bool {
        !self.is_trajectory_response()
    }

    /// 获取轨迹包传输完成状态（如果是轨迹传输应答）
    pub fn trajectory_pack_complete_status(&self) -> Option<TrajectoryPackCompleteStatus> {
        self.pack_complete_status
    }

    /// 获取轨迹点索引（如果是轨迹传输应答）
    pub fn trajectory_point_index(&self) -> Option<u8> {
        if self.is_trajectory_response() {
            Some(self.trajectory_index)
        } else {
            None
        }
    }

    /// 获取轨迹包名称索引（如果是轨迹传输应答）
    pub fn trajectory_name_index(&self) -> Option<u16> {
        if self.is_trajectory_response() {
            Some(self.name_index)
        } else {
            None
        }
    }

    /// 获取轨迹包 CRC16（如果是轨迹传输应答）
    pub fn trajectory_crc16(&self) -> Option<u16> {
        if self.is_trajectory_response() {
            Some(self.crc16)
        } else {
            None
        }
    }
}

impl TryFrom<PiperFrame> for SettingResponse {
    type Error = ProtocolError;

    fn try_from(frame: PiperFrame) -> Result<Self, Self::Error> {
        // 验证 CAN ID
        if frame.id != ID_SETTING_RESPONSE {
            return Err(ProtocolError::InvalidCanId { id: frame.id });
        }

        // 验证数据长度（至少需要 8 字节）
        if frame.len < 8 {
            return Err(ProtocolError::InvalidLength {
                expected: 8,
                actual: frame.len as usize,
            });
        }

        let response_index = frame.data[0];
        let zero_point_success = frame.data[1] == 0x01;
        let trajectory_index = frame.data[2];

        // Byte 3: 轨迹包传输完成应答（仅在轨迹传输应答时有效）
        let pack_complete_status = if response_index == 0x50 {
            Some(TrajectoryPackCompleteStatus::from(frame.data[3]))
        } else {
            None
        };

        // Byte 4-5: NameIndex（大端字节序）
        let name_index = u16::from_be_bytes([frame.data[4], frame.data[5]]);

        // Byte 6-7: CRC16（大端字节序）
        let crc16 = u16::from_be_bytes([frame.data[6], frame.data[7]]);

        Ok(Self {
            response_index,
            zero_point_success,
            trajectory_index,
            pack_complete_status,
            name_index,
            crc16,
        })
    }
}

#[cfg(test)]
mod setting_response_tests {
    use super::*;

    #[test]
    fn test_setting_response_setting_command() {
        // 测试设置指令应答（应答 0x471 电机使能指令）
        let mut data = [0u8; 8];
        data[0] = 0x71; // 0x471 的最后一个字节
        data[1] = 0x00; // 不是零点设置

        let frame = PiperFrame::new_standard(ID_SETTING_RESPONSE as u16, &data);
        let response = SettingResponse::try_from(frame).unwrap();

        assert_eq!(response.response_index, 0x71);
        assert!(!response.zero_point_success);
        assert!(response.is_setting_response());
        assert!(!response.is_trajectory_response());
    }

    #[test]
    fn test_setting_response_zero_point_success() {
        // 测试零点设置成功应答
        let mut data = [0u8; 8];
        data[0] = 0x75; // 0x475 关节设置指令的最后一个字节
        data[1] = 0x01; // 零点设置成功

        let frame = PiperFrame::new_standard(ID_SETTING_RESPONSE as u16, &data);
        let response = SettingResponse::try_from(frame).unwrap();

        assert_eq!(response.response_index, 0x75);
        assert!(response.zero_point_success);
    }

    #[test]
    fn test_setting_response_trajectory_transmit() {
        // 测试轨迹传输应答
        let mut data = [0u8; 8];
        data[0] = 0x50; // 轨迹传输应答标识
        data[1] = 0x00; // 不相关
        data[2] = 5; // 轨迹点索引
        data[3] = 0xAE; // 轨迹包传输完成且校验成功
        data[4] = 0x12; // NameIndex_H
        data[5] = 0x34; // NameIndex_L
        data[6] = 0x56; // CRC16_H
        data[7] = 0x78; // CRC16_L

        let frame = PiperFrame::new_standard(ID_SETTING_RESPONSE as u16, &data);
        let response = SettingResponse::try_from(frame).unwrap();

        assert_eq!(response.response_index, 0x50);
        assert_eq!(response.trajectory_index, 5);
        assert!(response.is_trajectory_response());
        assert!(!response.is_setting_response());
        assert_eq!(
            response.trajectory_pack_complete_status(),
            Some(TrajectoryPackCompleteStatus::Success)
        );
        assert_eq!(response.trajectory_name_index(), Some(0x1234));
        assert_eq!(response.trajectory_crc16(), Some(0x5678));
    }

    #[test]
    fn test_setting_response_trajectory_checksum_failed() {
        // 测试轨迹包校验失败
        let mut data = [0u8; 8];
        data[0] = 0x50; // 轨迹传输应答标识
        data[1] = 0x00;
        data[2] = 10; // 轨迹点索引
        data[3] = 0xEE; // 校验失败

        let frame = PiperFrame::new_standard(ID_SETTING_RESPONSE as u16, &data);
        let response = SettingResponse::try_from(frame).unwrap();

        assert_eq!(
            response.trajectory_pack_complete_status(),
            Some(TrajectoryPackCompleteStatus::ChecksumFailed)
        );
    }

    #[test]
    fn test_setting_response_invalid_id() {
        let frame = PiperFrame::new_standard(0x999, &[0; 8]);
        let result = SettingResponse::try_from(frame);
        assert!(result.is_err());
    }

    #[test]
    fn test_setting_response_invalid_length() {
        let frame = PiperFrame::new_standard(ID_SETTING_RESPONSE as u16, &[0; 2]);
        let result = SettingResponse::try_from(frame);
        assert!(result.is_err());
    }
}

// ============================================================================
// 参数查询与设置指令
// ============================================================================

/// 参数查询类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParameterQueryType {
    /// 查询末端V/acc参数
    EndVelocityAccel = 0x01,
    /// 查询碰撞防护等级
    CollisionProtectionLevel = 0x02,
    /// 查询当前轨迹索引
    CurrentTrajectoryIndex = 0x03,
    /// 查询夹爪/示教器参数索引
    GripperTeachParamsIndex = 0x04,
}

impl TryFrom<u8> for ParameterQueryType {
    type Error = ProtocolError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0x01 => Ok(ParameterQueryType::EndVelocityAccel),
            0x02 => Ok(ParameterQueryType::CollisionProtectionLevel),
            0x03 => Ok(ParameterQueryType::CurrentTrajectoryIndex),
            0x04 => Ok(ParameterQueryType::GripperTeachParamsIndex),
            _ => Err(ProtocolError::InvalidValue {
                field: "ParameterQueryType".to_string(),
                value,
            }),
        }
    }
}

/// 参数设置类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParameterSetType {
    /// 设置末端V/acc参数为初始值
    EndVelocityAccelToDefault = 0x01,
    /// 设置全部关节限位、关节最大速度、关节加速度为默认值
    AllJointLimitsToDefault = 0x02,
}

/// 0x48X报文反馈设置
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Feedback48XSetting {
    /// 无效
    #[default]
    Invalid = 0x00,
    /// 开启周期反馈（开启后周期上报1~6号关节当前末端速度/加速度）
    Enable = 0x01,
    /// 关闭周期反馈
    Disable = 0x02,
}

impl TryFrom<u8> for Feedback48XSetting {
    type Error = ProtocolError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0x00 => Ok(Feedback48XSetting::Invalid),
            0x01 => Ok(Feedback48XSetting::Enable),
            0x02 => Ok(Feedback48XSetting::Disable),
            _ => Err(ProtocolError::InvalidValue {
                field: "Feedback48XSetting".to_string(),
                value,
            }),
        }
    }
}

/// 末端负载设置
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[allow(clippy::enum_variant_names)]
pub enum EndLoadSetting {
    /// 空载
    #[default]
    NoLoad = 0x00,
    /// 半载
    HalfLoad = 0x01,
    /// 满载
    FullLoad = 0x02,
}

impl TryFrom<u8> for EndLoadSetting {
    type Error = ProtocolError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0x00 => Ok(EndLoadSetting::NoLoad),
            0x01 => Ok(EndLoadSetting::HalfLoad),
            0x02 => Ok(EndLoadSetting::FullLoad),
            _ => Err(ProtocolError::InvalidValue {
                field: "EndLoadSetting".to_string(),
                value,
            }),
        }
    }
}

impl TryFrom<u8> for ParameterSetType {
    type Error = ProtocolError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0x01 => Ok(ParameterSetType::EndVelocityAccelToDefault),
            0x02 => Ok(ParameterSetType::AllJointLimitsToDefault),
            _ => Err(ProtocolError::InvalidValue {
                field: "ParameterSetType".to_string(),
                value,
            }),
        }
    }
}

/// 机械臂参数查询与设置指令 (0x477)
///
/// 注意：查询和设置是互斥的，不能同时设置。
#[derive(Debug, Clone, Copy)]
pub struct ParameterQuerySetCommand {
    pub query_type: Option<ParameterQueryType>, // Byte 0: 参数查询（0x00 表示不查询）
    pub set_type: Option<ParameterSetType>,     // Byte 1: 参数设置（0x00 表示不设置）
    pub feedback_48x_setting: Feedback48XSetting, // Byte 2: 0x48X报文反馈设置
    pub load_param_enable: bool,                // Byte 3: 末端负载参数设置是否生效（0xAE=有效值）
    pub end_load: EndLoadSetting,               // Byte 4: 设置末端负载
                                                // Byte 5-7: 保留
}

impl ParameterQuerySetCommand {
    /// 创建查询指令
    pub fn query(query_type: ParameterQueryType) -> Self {
        Self {
            query_type: Some(query_type),
            set_type: None,
            feedback_48x_setting: Feedback48XSetting::Invalid,
            load_param_enable: false,
            end_load: EndLoadSetting::NoLoad,
        }
    }

    /// 创建设置指令
    pub fn set(set_type: ParameterSetType) -> Self {
        Self {
            query_type: None,
            set_type: Some(set_type),
            feedback_48x_setting: Feedback48XSetting::Invalid,
            load_param_enable: false,
            end_load: EndLoadSetting::NoLoad,
        }
    }

    /// 设置0x48X报文反馈
    pub fn with_feedback_48x(mut self, setting: Feedback48XSetting) -> Self {
        self.feedback_48x_setting = setting;
        self
    }

    /// 设置末端负载
    pub fn with_end_load(mut self, load: EndLoadSetting) -> Self {
        self.load_param_enable = true; // 设置负载时自动启用
        self.end_load = load;
        self
    }

    /// 验证互斥性（查询和设置不能同时进行）
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.query_type.is_some() && self.set_type.is_some() {
            return Err(ProtocolError::ParseError(
                "查询和设置不能同时进行".to_string(),
            ));
        }
        if self.query_type.is_none() && self.set_type.is_none() {
            return Err(ProtocolError::ParseError(
                "必须指定查询或设置之一".to_string(),
            ));
        }
        Ok(())
    }

    /// 转换为 CAN 帧
    pub fn to_frame(self) -> Result<PiperFrame, ProtocolError> {
        // 验证互斥性
        self.validate()?;

        let mut data = [0u8; 8];
        data[0] = self.query_type.map(|q| q as u8).unwrap_or(0x00);
        data[1] = self.set_type.map(|s| s as u8).unwrap_or(0x00);
        data[2] = self.feedback_48x_setting as u8;
        data[3] = if self.load_param_enable { 0xAE } else { 0x00 };
        data[4] = self.end_load as u8;
        // Byte 5-7: 保留，已初始化为 0

        Ok(PiperFrame::new_standard(
            ID_PARAMETER_QUERY_SET as u16,
            &data,
        ))
    }
}

#[cfg(test)]
mod parameter_query_set_tests {
    use super::*;

    #[test]
    fn test_parameter_query_type_from_u8() {
        assert_eq!(
            ParameterQueryType::try_from(0x01).unwrap(),
            ParameterQueryType::EndVelocityAccel
        );
        assert_eq!(
            ParameterQueryType::try_from(0x02).unwrap(),
            ParameterQueryType::CollisionProtectionLevel
        );
        assert_eq!(
            ParameterQueryType::try_from(0x03).unwrap(),
            ParameterQueryType::CurrentTrajectoryIndex
        );
        assert_eq!(
            ParameterQueryType::try_from(0x04).unwrap(),
            ParameterQueryType::GripperTeachParamsIndex
        );
    }

    #[test]
    fn test_parameter_set_type_from_u8() {
        assert_eq!(
            ParameterSetType::try_from(0x01).unwrap(),
            ParameterSetType::EndVelocityAccelToDefault
        );
        assert_eq!(
            ParameterSetType::try_from(0x02).unwrap(),
            ParameterSetType::AllJointLimitsToDefault
        );
    }

    #[test]
    fn test_parameter_query_set_command_query() {
        let cmd = ParameterQuerySetCommand::query(ParameterQueryType::EndVelocityAccel);
        let frame = cmd.to_frame().unwrap();

        assert_eq!(frame.id, ID_PARAMETER_QUERY_SET);
        assert_eq!(frame.data[0], 0x01); // EndVelocityAccel
        assert_eq!(frame.data[1], 0x00); // 不设置
    }

    #[test]
    fn test_parameter_query_set_command_set() {
        let cmd = ParameterQuerySetCommand::set(ParameterSetType::AllJointLimitsToDefault);
        let frame = cmd.to_frame().unwrap();

        assert_eq!(frame.id, ID_PARAMETER_QUERY_SET);
        assert_eq!(frame.data[0], 0x00); // 不查询
        assert_eq!(frame.data[1], 0x02); // AllJointLimitsToDefault
    }

    #[test]
    fn test_parameter_query_set_command_validate_mutually_exclusive() {
        // 测试互斥性：同时设置查询和设置应该失败
        let mut cmd = ParameterQuerySetCommand::query(ParameterQueryType::EndVelocityAccel);
        cmd.set_type = Some(ParameterSetType::AllJointLimitsToDefault);

        assert!(cmd.validate().is_err());
        assert!(cmd.to_frame().is_err());
    }

    #[test]
    fn test_parameter_query_set_command_validate_neither() {
        // 测试：既不查询也不设置应该失败
        let cmd = ParameterQuerySetCommand {
            query_type: None,
            set_type: None,
            feedback_48x_setting: Feedback48XSetting::Invalid,
            load_param_enable: false,
            end_load: EndLoadSetting::NoLoad,
        };

        assert!(cmd.validate().is_err());
        assert!(cmd.to_frame().is_err());
    }

    #[test]
    fn test_feedback_48x_setting_from_u8() {
        assert_eq!(
            Feedback48XSetting::try_from(0x00).unwrap(),
            Feedback48XSetting::Invalid
        );
        assert_eq!(
            Feedback48XSetting::try_from(0x01).unwrap(),
            Feedback48XSetting::Enable
        );
        assert_eq!(
            Feedback48XSetting::try_from(0x02).unwrap(),
            Feedback48XSetting::Disable
        );
    }

    #[test]
    fn test_end_load_setting_from_u8() {
        assert_eq!(
            EndLoadSetting::try_from(0x00).unwrap(),
            EndLoadSetting::NoLoad
        );
        assert_eq!(
            EndLoadSetting::try_from(0x01).unwrap(),
            EndLoadSetting::HalfLoad
        );
        assert_eq!(
            EndLoadSetting::try_from(0x02).unwrap(),
            EndLoadSetting::FullLoad
        );
    }

    #[test]
    fn test_parameter_query_set_command_with_feedback_48x() {
        let cmd = ParameterQuerySetCommand::query(ParameterQueryType::EndVelocityAccel)
            .with_feedback_48x(Feedback48XSetting::Enable);
        let frame = cmd.to_frame().unwrap();

        assert_eq!(frame.id, ID_PARAMETER_QUERY_SET);
        assert_eq!(frame.data[0], 0x01); // EndVelocityAccel
        assert_eq!(frame.data[1], 0x00); // 不设置
        assert_eq!(frame.data[2], 0x01); // Enable
    }

    #[test]
    fn test_parameter_query_set_command_with_end_load() {
        let cmd = ParameterQuerySetCommand::set(ParameterSetType::EndVelocityAccelToDefault)
            .with_end_load(EndLoadSetting::FullLoad);
        let frame = cmd.to_frame().unwrap();

        assert_eq!(frame.id, ID_PARAMETER_QUERY_SET);
        assert_eq!(frame.data[0], 0x00); // 不查询
        assert_eq!(frame.data[1], 0x01); // EndVelocityAccelToDefault
        assert_eq!(frame.data[3], 0xAE); // 负载参数生效
        assert_eq!(frame.data[4], 0x02); // FullLoad
    }
}

// ============================================================================
// 反馈末端速度/加速度参数
// ============================================================================

/// 反馈当前末端速度/加速度参数 (0x478)
///
/// 单位：
/// - 线速度：0.001m/s（原始值）
/// - 角速度：0.001rad/s（原始值）
/// - 线加速度：0.001m/s²（原始值）
/// - 角加速度：0.001rad/s²（原始值）
#[derive(Debug, Clone, Copy, Default)]
pub struct EndVelocityAccelFeedback {
    pub max_linear_velocity: u16,  // Byte 0-1: 末端最大线速度，单位 0.001m/s
    pub max_angular_velocity: u16, // Byte 2-3: 末端最大角速度，单位 0.001rad/s
    pub max_linear_accel: u16,     // Byte 4-5: 末端最大线加速度，单位 0.001m/s²
    pub max_angular_accel: u16,    // Byte 6-7: 末端最大角加速度，单位 0.001rad/s²
}

impl EndVelocityAccelFeedback {
    /// 获取最大线速度（m/s）
    pub fn max_linear_velocity(&self) -> f64 {
        self.max_linear_velocity as f64 / 1000.0
    }

    /// 获取最大角速度（rad/s）
    pub fn max_angular_velocity(&self) -> f64 {
        self.max_angular_velocity as f64 / 1000.0
    }

    /// 获取最大线加速度（m/s²）
    pub fn max_linear_accel(&self) -> f64 {
        self.max_linear_accel as f64 / 1000.0
    }

    /// 获取最大角加速度（rad/s²）
    pub fn max_angular_accel(&self) -> f64 {
        self.max_angular_accel as f64 / 1000.0
    }
}

impl TryFrom<PiperFrame> for EndVelocityAccelFeedback {
    type Error = ProtocolError;

    fn try_from(frame: PiperFrame) -> Result<Self, Self::Error> {
        // 验证 CAN ID
        if frame.id != ID_END_VELOCITY_ACCEL_FEEDBACK {
            return Err(ProtocolError::InvalidCanId { id: frame.id });
        }

        // 验证数据长度
        if frame.len < 8 {
            return Err(ProtocolError::InvalidLength {
                expected: 8,
                actual: frame.len as usize,
            });
        }

        // 大端字节序
        let max_linear_velocity = u16::from_be_bytes([frame.data[0], frame.data[1]]);
        let max_angular_velocity = u16::from_be_bytes([frame.data[2], frame.data[3]]);
        let max_linear_accel = u16::from_be_bytes([frame.data[4], frame.data[5]]);
        let max_angular_accel = u16::from_be_bytes([frame.data[6], frame.data[7]]);

        Ok(Self {
            max_linear_velocity,
            max_angular_velocity,
            max_linear_accel,
            max_angular_accel,
        })
    }
}

#[cfg(test)]
mod end_velocity_accel_feedback_tests {
    use super::*;

    #[test]
    fn test_end_velocity_accel_feedback_parse() {
        // 测试数据：
        // - 最大线速度: 1.0 m/s = 1000 (0.001m/s 单位)
        // - 最大角速度: 2.0 rad/s = 2000 (0.001rad/s 单位)
        // - 最大线加速度: 0.5 m/s² = 500 (0.001m/s² 单位)
        // - 最大角加速度: 1.5 rad/s² = 1500 (0.001rad/s² 单位)
        let mut data = [0u8; 8];
        data[0..2].copy_from_slice(&1000u16.to_be_bytes());
        data[2..4].copy_from_slice(&2000u16.to_be_bytes());
        data[4..6].copy_from_slice(&500u16.to_be_bytes());
        data[6..8].copy_from_slice(&1500u16.to_be_bytes());

        let frame = PiperFrame::new_standard(ID_END_VELOCITY_ACCEL_FEEDBACK as u16, &data);
        let feedback = EndVelocityAccelFeedback::try_from(frame).unwrap();

        assert_eq!(feedback.max_linear_velocity, 1000);
        assert_eq!(feedback.max_angular_velocity, 2000);
        assert_eq!(feedback.max_linear_accel, 500);
        assert_eq!(feedback.max_angular_accel, 1500);
        assert!((feedback.max_linear_velocity() - 1.0).abs() < 0.0001);
        assert!((feedback.max_angular_velocity() - 2.0).abs() < 0.0001);
        assert!((feedback.max_linear_accel() - 0.5).abs() < 0.0001);
        assert!((feedback.max_angular_accel() - 1.5).abs() < 0.0001);
    }

    #[test]
    fn test_end_velocity_accel_feedback_invalid_id() {
        let frame = PiperFrame::new_standard(0x999, &[0; 8]);
        let result = EndVelocityAccelFeedback::try_from(frame);
        assert!(result.is_err());
    }

    #[test]
    fn test_end_velocity_accel_feedback_invalid_length() {
        let frame = PiperFrame::new_standard(ID_END_VELOCITY_ACCEL_FEEDBACK as u16, &[0; 4]);
        let result = EndVelocityAccelFeedback::try_from(frame);
        assert!(result.is_err());
    }
}

// ============================================================================
// 设置末端速度/加速度参数指令
// ============================================================================

/// 末端速度/加速度参数设置指令 (0x479)
///
/// 用于设置末端的最大线速度、角速度、线加速度和角加速度。
/// 单位：
/// - 线速度：0.001m/s（原始值），无效值：0x7FFF
/// - 角速度：0.001rad/s（原始值），无效值：0x7FFF
/// - 线加速度：0.001m/s²（原始值），无效值：0x7FFF
/// - 角加速度：0.001rad/s²（原始值），无效值：0x7FFF
#[derive(Debug, Clone, Copy)]
pub struct SetEndVelocityAccelCommand {
    pub max_linear_velocity: Option<u16>, // Byte 0-1: 末端最大线速度，单位 0.001m/s，None 表示无效值
    pub max_angular_velocity: Option<u16>, // Byte 2-3: 末端最大角速度，单位 0.001rad/s，None 表示无效值
    pub max_linear_accel: Option<u16>, // Byte 4-5: 末端最大线加速度，单位 0.001m/s²，None 表示无效值
    pub max_angular_accel: Option<u16>, // Byte 6-7: 末端最大角加速度，单位 0.001rad/s²，None 表示无效值
}

impl SetEndVelocityAccelCommand {
    /// 从物理量创建设置指令
    pub fn new(
        max_linear_velocity: Option<f64>,
        max_angular_velocity: Option<f64>,
        max_linear_accel: Option<f64>,
        max_angular_accel: Option<f64>,
    ) -> Self {
        Self {
            max_linear_velocity: max_linear_velocity.map(|v| (v * 1000.0) as u16),
            max_angular_velocity: max_angular_velocity.map(|v| (v * 1000.0) as u16),
            max_linear_accel: max_linear_accel.map(|a| (a * 1000.0) as u16),
            max_angular_accel: max_angular_accel.map(|a| (a * 1000.0) as u16),
        }
    }

    /// 转换为 CAN 帧
    pub fn to_frame(self) -> PiperFrame {
        let mut data = [0u8; 8];

        // 大端字节序
        let linear_vel_bytes = if let Some(vel) = self.max_linear_velocity {
            vel.to_be_bytes()
        } else {
            [0x7F, 0xFF] // 无效值
        };
        data[0..2].copy_from_slice(&linear_vel_bytes);

        let angular_vel_bytes = if let Some(vel) = self.max_angular_velocity {
            vel.to_be_bytes()
        } else {
            [0x7F, 0xFF] // 无效值
        };
        data[2..4].copy_from_slice(&angular_vel_bytes);

        let linear_accel_bytes = if let Some(accel) = self.max_linear_accel {
            accel.to_be_bytes()
        } else {
            [0x7F, 0xFF] // 无效值
        };
        data[4..6].copy_from_slice(&linear_accel_bytes);

        let angular_accel_bytes = if let Some(accel) = self.max_angular_accel {
            accel.to_be_bytes()
        } else {
            [0x7F, 0xFF] // 无效值
        };
        data[6..8].copy_from_slice(&angular_accel_bytes);

        PiperFrame::new_standard(ID_SET_END_VELOCITY_ACCEL as u16, &data)
    }
}

#[cfg(test)]
mod set_end_velocity_accel_tests {
    use super::*;

    #[test]
    fn test_set_end_velocity_accel_command_new() {
        let cmd = SetEndVelocityAccelCommand::new(
            Some(1.0), // 最大线速度 1.0 m/s
            Some(2.0), // 最大角速度 2.0 rad/s
            Some(0.5), // 最大线加速度 0.5 m/s²
            Some(1.5), // 最大角加速度 1.5 rad/s²
        );
        assert_eq!(cmd.max_linear_velocity, Some(1000));
        assert_eq!(cmd.max_angular_velocity, Some(2000));
        assert_eq!(cmd.max_linear_accel, Some(500));
        assert_eq!(cmd.max_angular_accel, Some(1500));
    }

    #[test]
    fn test_set_end_velocity_accel_command_to_frame() {
        let cmd = SetEndVelocityAccelCommand::new(Some(1.0), Some(2.0), Some(0.5), Some(1.5));
        let frame = cmd.to_frame();

        assert_eq!(frame.id, ID_SET_END_VELOCITY_ACCEL);

        // 验证大端字节序
        let linear_vel = u16::from_be_bytes([frame.data[0], frame.data[1]]);
        let angular_vel = u16::from_be_bytes([frame.data[2], frame.data[3]]);
        let linear_accel = u16::from_be_bytes([frame.data[4], frame.data[5]]);
        let angular_accel = u16::from_be_bytes([frame.data[6], frame.data[7]]);

        assert_eq!(linear_vel, 1000);
        assert_eq!(angular_vel, 2000);
        assert_eq!(linear_accel, 500);
        assert_eq!(angular_accel, 1500);
    }

    #[test]
    fn test_set_end_velocity_accel_command_invalid_values() {
        // 测试无效值（None）
        let cmd = SetEndVelocityAccelCommand::new(None, None, None, None);
        let frame = cmd.to_frame();

        // 验证无效值编码为 0x7FFF
        assert_eq!(frame.data[0], 0x7F);
        assert_eq!(frame.data[1], 0xFF);
        assert_eq!(frame.data[2], 0x7F);
        assert_eq!(frame.data[3], 0xFF);
        assert_eq!(frame.data[4], 0x7F);
        assert_eq!(frame.data[5], 0xFF);
        assert_eq!(frame.data[6], 0x7F);
        assert_eq!(frame.data[7], 0xFF);
    }

    #[test]
    fn test_set_end_velocity_accel_command_partial_values() {
        // 测试部分有效值
        let cmd = SetEndVelocityAccelCommand::new(
            Some(1.0), // 只设置线速度
            None,      // 角速度无效
            Some(0.5), // 设置线加速度
            None,      // 角加速度无效
        );
        let frame = cmd.to_frame();

        let linear_vel = u16::from_be_bytes([frame.data[0], frame.data[1]]);
        let angular_vel = u16::from_be_bytes([frame.data[2], frame.data[3]]);
        let linear_accel = u16::from_be_bytes([frame.data[4], frame.data[5]]);
        let angular_accel = u16::from_be_bytes([frame.data[6], frame.data[7]]);

        assert_eq!(linear_vel, 1000);
        assert_eq!(angular_vel, 0x7FFF); // 无效值
        assert_eq!(linear_accel, 500);
        assert_eq!(angular_accel, 0x7FFF); // 无效值
    }
}

// ============================================================================
// 碰撞防护等级设置和反馈
// ============================================================================

/// 碰撞防护等级设置指令 (0x47A)
///
/// 用于设置6个关节的碰撞防护等级（0~8，等级0代表不检测碰撞）。
#[derive(Debug, Clone, Copy)]
pub struct CollisionProtectionLevelCommand {
    pub levels: [u8; 6], // Byte 0-5: 1~6号关节碰撞防护等级（0~8）
}

impl CollisionProtectionLevelCommand {
    /// 创建设置指令
    pub fn new(levels: [u8; 6]) -> Self {
        // 验证等级范围（0~8）
        for &level in &levels {
            if level > 8 {
                panic!("碰撞防护等级必须在0~8之间");
            }
        }
        Self { levels }
    }

    /// 设置所有关节为相同等级
    pub fn all_joints(level: u8) -> Self {
        if level > 8 {
            panic!("碰撞防护等级必须在0~8之间");
        }
        Self { levels: [level; 6] }
    }

    /// 转换为 CAN 帧
    pub fn to_frame(self) -> PiperFrame {
        let mut data = [0u8; 8];
        data[0..6].copy_from_slice(&self.levels);
        // Byte 6-7: 保留，已初始化为 0

        PiperFrame::new_standard(ID_COLLISION_PROTECTION_LEVEL as u16, &data)
    }
}

/// 碰撞防护等级设置反馈 (0x47B)
///
/// 反馈6个关节的碰撞防护等级（0~8，等级0代表不检测碰撞）。
#[derive(Debug, Clone, Copy, Default)]
pub struct CollisionProtectionLevelFeedback {
    pub levels: [u8; 6], // Byte 0-5: 1~6号关节碰撞防护等级（0~8）
}

impl TryFrom<PiperFrame> for CollisionProtectionLevelFeedback {
    type Error = ProtocolError;

    fn try_from(frame: PiperFrame) -> Result<Self, Self::Error> {
        // 验证 CAN ID
        if frame.id != ID_COLLISION_PROTECTION_LEVEL_FEEDBACK {
            return Err(ProtocolError::InvalidCanId { id: frame.id });
        }

        // 验证数据长度
        if frame.len < 6 {
            return Err(ProtocolError::InvalidLength {
                expected: 6,
                actual: frame.len as usize,
            });
        }

        let mut levels = [0u8; 6];
        levels.copy_from_slice(&frame.data[0..6]);

        Ok(Self { levels })
    }
}

#[cfg(test)]
mod collision_protection_tests {
    use super::*;

    #[test]
    fn test_collision_protection_level_command_new() {
        let cmd = CollisionProtectionLevelCommand::new([1, 2, 3, 4, 5, 6]);
        assert_eq!(cmd.levels, [1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn test_collision_protection_level_command_all_joints() {
        let cmd = CollisionProtectionLevelCommand::all_joints(5);
        assert_eq!(cmd.levels, [5; 6]);
    }

    #[test]
    fn test_collision_protection_level_command_to_frame() {
        let cmd = CollisionProtectionLevelCommand::new([1, 2, 3, 4, 5, 6]);
        let frame = cmd.to_frame();

        assert_eq!(frame.id, ID_COLLISION_PROTECTION_LEVEL);
        assert_eq!(frame.data[0..6], [1, 2, 3, 4, 5, 6]);
        assert_eq!(frame.data[6], 0x00); // 保留
        assert_eq!(frame.data[7], 0x00); // 保留
    }

    #[test]
    fn test_collision_protection_level_feedback_parse() {
        let mut data = [0u8; 8];
        data[0..6].copy_from_slice(&[1, 2, 3, 4, 5, 6]);

        let frame = PiperFrame::new_standard(ID_COLLISION_PROTECTION_LEVEL_FEEDBACK as u16, &data);
        let feedback = CollisionProtectionLevelFeedback::try_from(frame).unwrap();

        assert_eq!(feedback.levels, [1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn test_collision_protection_level_feedback_invalid_id() {
        let frame = PiperFrame::new_standard(0x999, &[0; 8]);
        let result = CollisionProtectionLevelFeedback::try_from(frame);
        assert!(result.is_err());
    }

    #[test]
    fn test_collision_protection_level_feedback_invalid_length() {
        let frame =
            PiperFrame::new_standard(ID_COLLISION_PROTECTION_LEVEL_FEEDBACK as u16, &[0; 4]);
        let result = CollisionProtectionLevelFeedback::try_from(frame);
        assert!(result.is_err());
    }

    #[test]
    fn test_collision_protection_level_zero() {
        // 测试等级0（不检测碰撞）
        let cmd = CollisionProtectionLevelCommand::all_joints(0);
        let frame = cmd.to_frame();
        assert_eq!(frame.data[0..6], [0; 6]);
    }

    #[test]
    fn test_collision_protection_level_max() {
        // 测试最大等级8
        let cmd = CollisionProtectionLevelCommand::all_joints(8);
        let frame = cmd.to_frame();
        assert_eq!(frame.data[0..6], [8; 6]);
    }
}

// ============================================================================
// 反馈电机最大加速度限制
// ============================================================================

/// 反馈当前电机最大加速度限制 (0x47C)
///
/// 单位：0.001rad/s²（原始值）
#[derive(Debug, Clone, Copy, Default)]
pub struct MotorMaxAccelFeedback {
    pub joint_index: u8,       // Byte 0: 关节电机序号（1-6）
    pub max_accel_rad_s2: u16, // Byte 1-2: 最大关节加速度，单位 0.001rad/s²
}

impl MotorMaxAccelFeedback {
    /// 获取最大加速度（rad/s²）
    pub fn max_accel(&self) -> f64 {
        self.max_accel_rad_s2 as f64 / 1000.0
    }
}

impl TryFrom<PiperFrame> for MotorMaxAccelFeedback {
    type Error = ProtocolError;

    fn try_from(frame: PiperFrame) -> Result<Self, Self::Error> {
        // 验证 CAN ID
        if frame.id != ID_MOTOR_MAX_ACCEL_FEEDBACK {
            return Err(ProtocolError::InvalidCanId { id: frame.id });
        }

        // 验证数据长度
        if frame.len < 3 {
            return Err(ProtocolError::InvalidLength {
                expected: 3,
                actual: frame.len as usize,
            });
        }

        let joint_index = frame.data[0];
        let max_accel_bytes = [frame.data[1], frame.data[2]];
        let max_accel_rad_s2 = u16::from_be_bytes(max_accel_bytes);

        Ok(Self {
            joint_index,
            max_accel_rad_s2,
        })
    }
}

#[cfg(test)]
mod motor_max_accel_feedback_tests {
    use super::*;

    #[test]
    fn test_motor_max_accel_feedback_parse() {
        // 测试数据：
        // - 关节序号: 1
        // - 最大加速度: 10.0 rad/s² = 10000 (0.001rad/s² 单位)
        let mut data = [0u8; 8];
        data[0] = 1;
        data[1..3].copy_from_slice(&10000u16.to_be_bytes());

        let frame = PiperFrame::new_standard(ID_MOTOR_MAX_ACCEL_FEEDBACK as u16, &data);
        let feedback = MotorMaxAccelFeedback::try_from(frame).unwrap();

        assert_eq!(feedback.joint_index, 1);
        assert_eq!(feedback.max_accel_rad_s2, 10000);
        assert!((feedback.max_accel() - 10.0).abs() < 0.0001);
    }

    #[test]
    fn test_motor_max_accel_feedback_all_joints() {
        // 测试所有 6 个关节
        for i in 1..=6 {
            let mut data = [0u8; 8];
            data[0] = i;
            data[1..3].copy_from_slice(&5000u16.to_be_bytes());

            let frame = PiperFrame::new_standard(ID_MOTOR_MAX_ACCEL_FEEDBACK as u16, &data);
            let feedback = MotorMaxAccelFeedback::try_from(frame).unwrap();
            assert_eq!(feedback.joint_index, i);
        }
    }

    #[test]
    fn test_motor_max_accel_feedback_invalid_id() {
        let frame = PiperFrame::new_standard(0x999, &[0; 8]);
        let result = MotorMaxAccelFeedback::try_from(frame);
        assert!(result.is_err());
    }

    #[test]
    fn test_motor_max_accel_feedback_invalid_length() {
        let frame = PiperFrame::new_standard(ID_MOTOR_MAX_ACCEL_FEEDBACK as u16, &[0; 2]);
        let result = MotorMaxAccelFeedback::try_from(frame);
        assert!(result.is_err());
    }
}

// ============================================================================
// 夹爪/示教器参数设置和反馈
// ============================================================================

/// 夹爪/示教器参数设置指令 (0x47D)
///
/// 用于设置示教器行程系数、夹爪行程系数和夹爪扭矩系数。
/// 注意：根据协议文档，实际字段与实现计划可能有所不同。
/// 协议文档显示：
/// - Byte 0: 示教器行程系数设置（100~200，单位%）
/// - Byte 1: 夹爪/示教器最大控制行程限制值设置（单位mm）
/// - Byte 2: 示教器摩擦系数设置（1-10）
#[derive(Debug, Clone, Copy)]
pub struct GripperTeachParamsCommand {
    pub teach_travel_coeff: u8, // Byte 0: 示教器行程系数设置（100~200，单位%）
    pub max_travel_limit: u8,   // Byte 1: 夹爪/示教器最大控制行程限制值（单位mm）
    pub friction_coeff: u8,     // Byte 2: 示教器摩擦系数设置（1-10）
                                // Byte 3-7: 保留
}

impl GripperTeachParamsCommand {
    /// 创建设置指令
    pub fn new(teach_travel_coeff: u8, max_travel_limit: u8, friction_coeff: u8) -> Self {
        Self {
            teach_travel_coeff,
            max_travel_limit,
            friction_coeff,
        }
    }

    /// 转换为 CAN 帧
    pub fn to_frame(self) -> PiperFrame {
        let mut data = [0u8; 8];
        data[0] = self.teach_travel_coeff;
        data[1] = self.max_travel_limit;
        data[2] = self.friction_coeff;
        // Byte 3-7: 保留，已初始化为 0

        PiperFrame::new_standard(ID_GRIPPER_TEACH_PARAMS as u16, &data)
    }
}

/// 夹爪/示教器参数反馈 (0x47E)
///
/// 反馈示教器行程系数、夹爪行程系数和夹爪扭矩系数。
#[derive(Debug, Clone, Copy, Default)]
pub struct GripperTeachParamsFeedback {
    pub teach_travel_coeff: u8, // Byte 0: 示教器行程系数反馈（100~200，单位%）
    pub max_travel_limit: u8,   // Byte 1: 夹爪/示教器最大控制行程限制值反馈（单位mm）
    pub friction_coeff: u8,     // Byte 2: 示教器摩擦系数反馈（1-10）
                                // Byte 3-7: 保留
}

impl TryFrom<PiperFrame> for GripperTeachParamsFeedback {
    type Error = ProtocolError;

    fn try_from(frame: PiperFrame) -> Result<Self, Self::Error> {
        // 验证 CAN ID
        if frame.id != ID_GRIPPER_TEACH_PARAMS_FEEDBACK {
            return Err(ProtocolError::InvalidCanId { id: frame.id });
        }

        // 验证数据长度
        if frame.len < 3 {
            return Err(ProtocolError::InvalidLength {
                expected: 3,
                actual: frame.len as usize,
            });
        }

        Ok(Self {
            teach_travel_coeff: frame.data[0],
            max_travel_limit: frame.data[1],
            friction_coeff: frame.data[2],
        })
    }
}

#[cfg(test)]
mod gripper_teach_params_tests {
    use super::*;

    #[test]
    fn test_gripper_teach_params_command_new() {
        let cmd = GripperTeachParamsCommand::new(150, 70, 5);
        assert_eq!(cmd.teach_travel_coeff, 150);
        assert_eq!(cmd.max_travel_limit, 70);
        assert_eq!(cmd.friction_coeff, 5);
    }

    #[test]
    fn test_gripper_teach_params_command_to_frame() {
        let cmd = GripperTeachParamsCommand::new(150, 70, 5);
        let frame = cmd.to_frame();

        assert_eq!(frame.id, ID_GRIPPER_TEACH_PARAMS);
        assert_eq!(frame.data[0], 150);
        assert_eq!(frame.data[1], 70);
        assert_eq!(frame.data[2], 5);
        assert_eq!(frame.data[3], 0x00); // 保留
    }

    #[test]
    fn test_gripper_teach_params_feedback_parse() {
        let mut data = [0u8; 8];
        data[0] = 150; // 示教器行程系数
        data[1] = 70; // 最大控制行程限制值
        data[2] = 5; // 摩擦系数

        let frame = PiperFrame::new_standard(ID_GRIPPER_TEACH_PARAMS_FEEDBACK as u16, &data);
        let feedback = GripperTeachParamsFeedback::try_from(frame).unwrap();

        assert_eq!(feedback.teach_travel_coeff, 150);
        assert_eq!(feedback.max_travel_limit, 70);
        assert_eq!(feedback.friction_coeff, 5);
    }

    #[test]
    fn test_gripper_teach_params_feedback_invalid_id() {
        let frame = PiperFrame::new_standard(0x999, &[0; 8]);
        let result = GripperTeachParamsFeedback::try_from(frame);
        assert!(result.is_err());
    }

    #[test]
    fn test_gripper_teach_params_feedback_invalid_length() {
        let frame = PiperFrame::new_standard(ID_GRIPPER_TEACH_PARAMS_FEEDBACK as u16, &[0; 2]);
        let result = GripperTeachParamsFeedback::try_from(frame);
        assert!(result.is_err());
    }
}

// ============================================================================
// 固件升级模式设定指令
// ============================================================================

/// 固件升级模式
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, num_enum::FromPrimitive)]
#[repr(u8)]
pub enum FirmwareUpgradeMode {
    /// 退出固件升级模式（默认状态）
    #[default]
    Exit = 0x00,
    /// 进入 CAN 升级外部总线静默模式（用于升级主控）
    CanUpgradeSilent = 0x01,
    /// 进入内外网组合升级模式（用于升级主控以及内网设备）
    /// 内外网总线静默，主控进入内外网 CAN 透传模式，退出后恢复数据反馈
    CombinedUpgrade = 0x02,
}

/// 固件升级模式设定指令 (0x422)
///
/// 用于进入/退出固件升级模式。
#[derive(Debug, Clone, Copy)]
pub struct FirmwareUpgradeCommand {
    pub mode: FirmwareUpgradeMode,
}

impl FirmwareUpgradeCommand {
    /// 创建固件升级模式设定指令
    pub fn new(mode: FirmwareUpgradeMode) -> Self {
        Self { mode }
    }

    /// 创建退出固件升级模式指令
    pub fn exit() -> Self {
        Self {
            mode: FirmwareUpgradeMode::Exit,
        }
    }

    /// 创建进入 CAN 升级外部总线静默模式指令
    pub fn can_upgrade_silent() -> Self {
        Self {
            mode: FirmwareUpgradeMode::CanUpgradeSilent,
        }
    }

    /// 创建进入内外网组合升级模式指令
    pub fn combined_upgrade() -> Self {
        Self {
            mode: FirmwareUpgradeMode::CombinedUpgrade,
        }
    }

    /// 转换为 CAN 帧
    pub fn to_frame(self) -> PiperFrame {
        let data = [self.mode as u8];
        PiperFrame::new_standard(ID_FIRMWARE_UPGRADE as u16, &data)
    }
}

#[cfg(test)]
mod firmware_upgrade_tests {
    use super::*;

    #[test]
    fn test_firmware_upgrade_mode_from() {
        assert_eq!(FirmwareUpgradeMode::from(0x00), FirmwareUpgradeMode::Exit);
        assert_eq!(
            FirmwareUpgradeMode::from(0x01),
            FirmwareUpgradeMode::CanUpgradeSilent
        );
        assert_eq!(
            FirmwareUpgradeMode::from(0x02),
            FirmwareUpgradeMode::CombinedUpgrade
        );
        assert_eq!(FirmwareUpgradeMode::from(0xFF), FirmwareUpgradeMode::Exit); // 默认退出
    }

    #[test]
    fn test_firmware_upgrade_command_new() {
        let cmd = FirmwareUpgradeCommand::new(FirmwareUpgradeMode::CanUpgradeSilent);
        assert_eq!(cmd.mode, FirmwareUpgradeMode::CanUpgradeSilent);
    }

    #[test]
    fn test_firmware_upgrade_command_exit() {
        let cmd = FirmwareUpgradeCommand::exit();
        assert_eq!(cmd.mode, FirmwareUpgradeMode::Exit);
    }

    #[test]
    fn test_firmware_upgrade_command_can_upgrade_silent() {
        let cmd = FirmwareUpgradeCommand::can_upgrade_silent();
        assert_eq!(cmd.mode, FirmwareUpgradeMode::CanUpgradeSilent);
    }

    #[test]
    fn test_firmware_upgrade_command_combined_upgrade() {
        let cmd = FirmwareUpgradeCommand::combined_upgrade();
        assert_eq!(cmd.mode, FirmwareUpgradeMode::CombinedUpgrade);
    }

    #[test]
    fn test_firmware_upgrade_command_to_frame() {
        let cmd = FirmwareUpgradeCommand::new(FirmwareUpgradeMode::CanUpgradeSilent);
        let frame = cmd.to_frame();

        assert_eq!(frame.id, ID_FIRMWARE_UPGRADE);
        assert_eq!(frame.len, 1);
        assert_eq!(frame.data[0], 0x01);
    }

    #[test]
    fn test_firmware_upgrade_command_all_modes() {
        // 测试所有模式
        let modes = [
            FirmwareUpgradeMode::Exit,
            FirmwareUpgradeMode::CanUpgradeSilent,
            FirmwareUpgradeMode::CombinedUpgrade,
        ];

        for mode in modes.iter() {
            let cmd = FirmwareUpgradeCommand::new(*mode);
            let frame = cmd.to_frame();
            assert_eq!(frame.id, ID_FIRMWARE_UPGRADE);
            assert_eq!(frame.data[0], *mode as u8);
        }
    }
}

// ============================================================================
// 固件版本查询指令
// ============================================================================

/// 固件版本查询指令 (0x4AF)
///
/// 用于查询机械臂固件版本信息。
/// 查询和反馈使用相同的 CAN ID (0x4AF)。
/// 查询命令的数据负载为固定值：`[0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]`
///
/// **注意**：发送查询命令前应清空固件数据缓存，以便接收新的反馈数据。
#[derive(Debug, Clone, Copy)]
pub struct FirmwareVersionQueryCommand;

impl FirmwareVersionQueryCommand {
    /// 创建固件版本查询指令
    ///
    /// 返回一个查询命令，用于向机械臂查询固件版本信息。
    pub fn new() -> Self {
        Self
    }

    /// 转换为 CAN 帧
    ///
    /// 根据 Python SDK 的实现，查询命令的数据为：
    /// `[0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]`
    pub fn to_frame(self) -> PiperFrame {
        // 与 Python SDK 对齐：data = [0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]
        let data = [0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
        PiperFrame::new_standard(ID_FIRMWARE_READ as u16, &data)
    }
}

impl Default for FirmwareVersionQueryCommand {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod firmware_version_query_tests {
    use super::*;

    #[test]
    fn test_firmware_version_query_command_new() {
        let cmd = FirmwareVersionQueryCommand::new();
        let frame = cmd.to_frame();

        assert_eq!(frame.id, ID_FIRMWARE_READ);
        assert_eq!(frame.len, 8);
        assert_eq!(frame.data[0], 0x01);
        assert_eq!(frame.data[1..8], [0x00; 7]);
    }

    #[test]
    fn test_firmware_version_query_command_default() {
        let cmd = FirmwareVersionQueryCommand;
        let frame = cmd.to_frame();

        assert_eq!(frame.id, ID_FIRMWARE_READ);
        assert_eq!(frame.data, [0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
    }

    #[test]
    fn test_firmware_version_query_command_data_format() {
        // 验证数据格式与 Python SDK 一致
        let cmd = FirmwareVersionQueryCommand::new();
        let frame = cmd.to_frame();

        // Python SDK: [0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]
        let expected_data = [0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
        assert_eq!(frame.data, expected_data);
    }
}
