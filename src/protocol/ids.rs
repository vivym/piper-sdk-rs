//! CAN ID 常量定义和枚举
//!
//! 定义所有协议帧的 CAN ID 常量，并提供 ID 分类功能。

/// 反馈帧 ID 范围
pub const FEEDBACK_BASE_ID: u32 = 0x2A1;
pub const FEEDBACK_END_ID: u32 = 0x2A8;

/// 控制帧 ID 范围
pub const CONTROL_BASE_ID: u32 = 0x150;
pub const CONTROL_END_ID: u32 = 0x15F;

/// 配置帧 ID 范围
pub const CONFIG_BASE_ID: u32 = 0x470;
pub const CONFIG_END_ID: u32 = 0x47E; // 注意：包含 0x47D 和 0x47E

// ============================================================================
// 反馈帧 ID 常量
// ============================================================================

/// 机械臂状态反馈
pub const ID_ROBOT_STATUS: u32 = 0x2A1;

/// 机械臂末端位姿反馈
pub const ID_END_POSE_1: u32 = 0x2A2;
pub const ID_END_POSE_2: u32 = 0x2A3;
pub const ID_END_POSE_3: u32 = 0x2A4;

/// 机械臂关节反馈
pub const ID_JOINT_FEEDBACK_12: u32 = 0x2A5;
pub const ID_JOINT_FEEDBACK_34: u32 = 0x2A6;
pub const ID_JOINT_FEEDBACK_56: u32 = 0x2A7;

/// 夹爪反馈
pub const ID_GRIPPER_FEEDBACK: u32 = 0x2A8;

/// 关节驱动器高速反馈（0x251~0x256）
pub const ID_JOINT_DRIVER_HIGH_SPEED_BASE: u32 = 0x251;

/// 关节驱动器低速反馈（0x261~0x266）
pub const ID_JOINT_DRIVER_LOW_SPEED_BASE: u32 = 0x261;

/// 关节末端速度/加速度反馈（0x481~0x486）
pub const ID_JOINT_END_VELOCITY_ACCEL_BASE: u32 = 0x481;

// ============================================================================
// 控制帧 ID 常量
// ============================================================================

/// 快速急停/轨迹指令
pub const ID_EMERGENCY_STOP: u32 = 0x150;

/// 控制模式指令
pub const ID_CONTROL_MODE: u32 = 0x151;

/// 末端位姿控制指令
pub const ID_END_POSE_CONTROL_1: u32 = 0x152;
pub const ID_END_POSE_CONTROL_2: u32 = 0x153;
pub const ID_END_POSE_CONTROL_3: u32 = 0x154;

/// 关节控制指令
pub const ID_JOINT_CONTROL_12: u32 = 0x155;
pub const ID_JOINT_CONTROL_34: u32 = 0x156;
pub const ID_JOINT_CONTROL_56: u32 = 0x157;

/// 圆弧模式坐标序号更新指令
pub const ID_ARC_POINT: u32 = 0x158;

/// 夹爪控制指令
pub const ID_GRIPPER_CONTROL: u32 = 0x159;

/// MIT 控制指令（0x15A~0x15F）
pub const ID_MIT_CONTROL_BASE: u32 = 0x15A;

/// 灯光控制指令
pub const ID_LIGHT_CONTROL: u32 = 0x121;

/// 固件升级模式设定指令
pub const ID_FIRMWARE_UPGRADE: u32 = 0x422;

// ============================================================================
// 配置帧 ID 常量
// ============================================================================

/// 随动主从模式设置指令
pub const ID_MASTER_SLAVE_MODE: u32 = 0x470;

/// 电机使能/失能设置指令
pub const ID_MOTOR_ENABLE: u32 = 0x471;

/// 查询电机限制指令
pub const ID_QUERY_MOTOR_LIMIT: u32 = 0x472;

/// 反馈当前电机限制角度/最大速度
pub const ID_MOTOR_LIMIT_FEEDBACK: u32 = 0x473;

/// 设置电机限制指令
pub const ID_SET_MOTOR_LIMIT: u32 = 0x474;

/// 关节设置指令
pub const ID_JOINT_SETTING: u32 = 0x475;

/// 设置指令应答
pub const ID_SETTING_RESPONSE: u32 = 0x476;

/// 参数查询与设置指令
pub const ID_PARAMETER_QUERY_SET: u32 = 0x477;

/// 反馈当前末端速度/加速度参数
pub const ID_END_VELOCITY_ACCEL_FEEDBACK: u32 = 0x478;

/// 设置末端速度/加速度参数
pub const ID_SET_END_VELOCITY_ACCEL: u32 = 0x479;

/// 碰撞防护等级设置指令
pub const ID_COLLISION_PROTECTION_LEVEL: u32 = 0x47A;

/// 碰撞防护等级设置反馈
pub const ID_COLLISION_PROTECTION_LEVEL_FEEDBACK: u32 = 0x47B;

/// 反馈当前电机最大加速度限制
pub const ID_MOTOR_MAX_ACCEL_FEEDBACK: u32 = 0x47C;

/// 夹爪/示教器参数设置指令
pub const ID_GRIPPER_TEACH_PARAMS: u32 = 0x47D;

/// 夹爪/示教器参数反馈
pub const ID_GRIPPER_TEACH_PARAMS_FEEDBACK: u32 = 0x47E;

// ============================================================================
// ID 分类枚举
// ============================================================================

/// CAN 帧类型分类
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameType {
    /// 反馈帧
    Feedback,
    /// 控制帧
    Control,
    /// 配置帧
    Config,
    /// 未知类型
    Unknown,
}

impl FrameType {
    /// 根据 CAN ID 判断帧类型
    pub fn from_id(id: u32) -> Self {
        match id {
            // 反馈帧范围
            0x2A1..=0x2A8 | 0x251..=0x256 | 0x261..=0x266 | 0x481..=0x486 => FrameType::Feedback,
            // 控制帧范围
            0x150..=0x15F => FrameType::Control,
            // 配置帧范围
            0x470..=0x47E => FrameType::Config,
            // 未知类型
            _ => FrameType::Unknown,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_frame_type_feedback() {
        assert_eq!(FrameType::from_id(0x2A1), FrameType::Feedback);
        assert_eq!(FrameType::from_id(0x2A8), FrameType::Feedback);
        assert_eq!(FrameType::from_id(0x251), FrameType::Feedback);
        assert_eq!(FrameType::from_id(0x256), FrameType::Feedback);
        assert_eq!(FrameType::from_id(0x261), FrameType::Feedback);
        assert_eq!(FrameType::from_id(0x266), FrameType::Feedback);
        assert_eq!(FrameType::from_id(0x481), FrameType::Feedback);
        assert_eq!(FrameType::from_id(0x486), FrameType::Feedback);
    }

    #[test]
    fn test_frame_type_control() {
        assert_eq!(FrameType::from_id(0x150), FrameType::Control);
        assert_eq!(FrameType::from_id(0x15F), FrameType::Control);
        assert_eq!(FrameType::from_id(0x151), FrameType::Control);
        assert_eq!(FrameType::from_id(0x155), FrameType::Control);
    }

    #[test]
    fn test_frame_type_config() {
        assert_eq!(FrameType::from_id(0x470), FrameType::Config);
        assert_eq!(FrameType::from_id(0x47E), FrameType::Config);
        assert_eq!(FrameType::from_id(0x471), FrameType::Config);
        assert_eq!(FrameType::from_id(0x47D), FrameType::Config);
    }

    #[test]
    fn test_frame_type_unknown() {
        assert_eq!(FrameType::from_id(0x100), FrameType::Unknown);
        assert_eq!(FrameType::from_id(0x999), FrameType::Unknown);
        assert_eq!(FrameType::from_id(0x121), FrameType::Unknown); // 灯光控制，辅助功能
        assert_eq!(FrameType::from_id(0x422), FrameType::Unknown); // 固件升级，辅助功能
    }

    #[test]
    fn test_id_constants() {
        // 验证一些关键 ID 常量
        assert_eq!(ID_ROBOT_STATUS, 0x2A1);
        assert_eq!(ID_EMERGENCY_STOP, 0x150);
        assert_eq!(ID_MOTOR_ENABLE, 0x471);
    }
}
