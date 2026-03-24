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

/// 固件版本读取反馈
pub const ID_FIRMWARE_READ: u32 = 0x4AF;

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

/// Driver RX 线程允许进入实时状态管线的机器人反馈/回显反馈 ID 单一真值源。
///
/// SocketCAN kernel filter、startup probe bootstrap 缓存，以及 `is_robot_feedback_id()`
/// 都必须复用这份列表，避免定义漂移导致实时路径误收或漏收帧。
pub const DRIVER_RX_ROBOT_FEEDBACK_IDS: [u32; 30] = [
    ID_ROBOT_STATUS,
    ID_END_POSE_1,
    ID_END_POSE_2,
    ID_END_POSE_3,
    ID_JOINT_FEEDBACK_12,
    ID_JOINT_FEEDBACK_34,
    ID_JOINT_FEEDBACK_56,
    ID_GRIPPER_FEEDBACK,
    ID_JOINT_DRIVER_HIGH_SPEED_BASE,
    ID_JOINT_DRIVER_HIGH_SPEED_BASE + 1,
    ID_JOINT_DRIVER_HIGH_SPEED_BASE + 2,
    ID_JOINT_DRIVER_HIGH_SPEED_BASE + 3,
    ID_JOINT_DRIVER_HIGH_SPEED_BASE + 4,
    ID_JOINT_DRIVER_HIGH_SPEED_BASE + 5,
    ID_JOINT_DRIVER_LOW_SPEED_BASE,
    ID_JOINT_DRIVER_LOW_SPEED_BASE + 1,
    ID_JOINT_DRIVER_LOW_SPEED_BASE + 2,
    ID_JOINT_DRIVER_LOW_SPEED_BASE + 3,
    ID_JOINT_DRIVER_LOW_SPEED_BASE + 4,
    ID_JOINT_DRIVER_LOW_SPEED_BASE + 5,
    ID_FIRMWARE_READ,
    ID_COLLISION_PROTECTION_LEVEL_FEEDBACK,
    ID_MOTOR_LIMIT_FEEDBACK,
    ID_MOTOR_MAX_ACCEL_FEEDBACK,
    ID_END_VELOCITY_ACCEL_FEEDBACK,
    ID_CONTROL_MODE,
    ID_JOINT_CONTROL_12,
    ID_JOINT_CONTROL_34,
    ID_JOINT_CONTROL_56,
    ID_GRIPPER_CONTROL,
];

/// Returns the canonical driver RX ID allow-list used by real-time feedback parsing.
pub const fn driver_rx_robot_feedback_ids() -> &'static [u32] {
    &DRIVER_RX_ROBOT_FEEDBACK_IDS
}

/// Returns whether a CAN ID represents robot-originated feedback/state traffic.
///
/// This intentionally includes feedback-style control/config echoes that the driver
/// treats as robot feedback for connection monitoring and startup validation.
pub const fn is_robot_feedback_id(id: u32) -> bool {
    let ids = &DRIVER_RX_ROBOT_FEEDBACK_IDS;
    let mut index = 0;
    while index < ids.len() {
        if ids[index] == id {
            return true;
        }
        index += 1;
    }
    false
}

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
            0x2A1..=0x2A8 | 0x251..=0x256 | 0x261..=0x266 | 0x481..=0x486 | 0x4AF => {
                FrameType::Feedback
            },
            // 控制帧范围（注意：在主从模式下，0x151, 0x155-0x157, 0x159 也可能作为反馈解析）
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
        assert_eq!(FrameType::from_id(0x16A), FrameType::Unknown); // shifted control IDs 未实现
        assert_eq!(FrameType::from_id(0x2B1), FrameType::Unknown); // shifted feedback IDs 未实现
    }

    #[test]
    fn test_id_constants() {
        // 验证一些关键 ID 常量
        assert_eq!(ID_ROBOT_STATUS, 0x2A1);
        assert_eq!(ID_EMERGENCY_STOP, 0x150);
        assert_eq!(ID_MOTOR_ENABLE, 0x471);
    }

    #[test]
    fn test_is_robot_feedback_id_matches_driver_feedback_surface() {
        assert!(is_robot_feedback_id(ID_ROBOT_STATUS));
        assert!(is_robot_feedback_id(ID_JOINT_FEEDBACK_12));
        assert!(is_robot_feedback_id(ID_JOINT_DRIVER_HIGH_SPEED_BASE + 5));
        assert!(is_robot_feedback_id(ID_JOINT_DRIVER_LOW_SPEED_BASE + 5));
        assert!(is_robot_feedback_id(ID_MOTOR_LIMIT_FEEDBACK));
        assert!(is_robot_feedback_id(ID_CONTROL_MODE));
        assert!(is_robot_feedback_id(ID_JOINT_CONTROL_56));
        assert!(is_robot_feedback_id(ID_GRIPPER_CONTROL));

        assert!(!is_robot_feedback_id(ID_EMERGENCY_STOP));
        assert!(!is_robot_feedback_id(ID_MOTOR_ENABLE));
        assert!(!is_robot_feedback_id(ID_SET_END_VELOCITY_ACCEL));
        assert!(!is_robot_feedback_id(ID_GRIPPER_TEACH_PARAMS_FEEDBACK));
    }

    #[test]
    fn test_driver_rx_robot_feedback_ids_are_the_single_truth_source() {
        assert!(driver_rx_robot_feedback_ids().contains(&ID_ROBOT_STATUS));
        assert!(driver_rx_robot_feedback_ids().contains(&ID_GRIPPER_FEEDBACK));
        assert!(driver_rx_robot_feedback_ids().contains(&ID_FIRMWARE_READ));
        assert!(driver_rx_robot_feedback_ids().contains(&ID_COLLISION_PROTECTION_LEVEL_FEEDBACK));
        assert!(driver_rx_robot_feedback_ids().contains(&ID_MOTOR_LIMIT_FEEDBACK));
        assert!(driver_rx_robot_feedback_ids().contains(&ID_MOTOR_MAX_ACCEL_FEEDBACK));
        assert!(driver_rx_robot_feedback_ids().contains(&ID_END_VELOCITY_ACCEL_FEEDBACK));
        assert!(driver_rx_robot_feedback_ids().contains(&ID_CONTROL_MODE));
        assert!(driver_rx_robot_feedback_ids().contains(&ID_JOINT_CONTROL_12));
        assert!(driver_rx_robot_feedback_ids().contains(&ID_JOINT_CONTROL_34));
        assert!(driver_rx_robot_feedback_ids().contains(&ID_JOINT_CONTROL_56));
        assert!(driver_rx_robot_feedback_ids().contains(&ID_GRIPPER_CONTROL));

        for id in driver_rx_robot_feedback_ids() {
            assert!(
                is_robot_feedback_id(*id),
                "shared driver RX ID surface must stay aligned with classifier for 0x{id:X}"
            );
        }

        assert!(!driver_rx_robot_feedback_ids().contains(&ID_EMERGENCY_STOP));
        assert!(!driver_rx_robot_feedback_ids().contains(&ID_MOTOR_ENABLE));
        assert!(!driver_rx_robot_feedback_ids().contains(&ID_SET_END_VELOCITY_ACCEL));
        assert!(!driver_rx_robot_feedback_ids().contains(&ID_GRIPPER_TEACH_PARAMS_FEEDBACK));
    }
}
