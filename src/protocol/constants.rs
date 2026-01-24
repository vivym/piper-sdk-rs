//! 硬件相关常量定义
//!
//! 集中定义所有硬件相关的常量，避免在代码中散落"魔法数"。

/// Gripper 位置归一化比例尺
///
/// 将硬件值（mm）转换为归一化值（0.0-1.0）
pub const GRIPPER_POSITION_SCALE: f64 = 100.0;

/// Gripper 力度归一化比例尺
///
/// 将硬件值（N·m）转换为归一化值（0.0-1.0）
pub const GRIPPER_FORCE_SCALE: f64 = 10.0;

// 重新导出 CAN ID 常量（从 ids.rs）
pub use crate::protocol::ids::{
    ID_CONTROL_MODE, ID_EMERGENCY_STOP, ID_GRIPPER_CONTROL, ID_JOINT_CONTROL_12,
    ID_JOINT_CONTROL_34, ID_JOINT_CONTROL_56, ID_MIT_CONTROL_BASE, ID_MOTOR_ENABLE,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_gripper_normalization() {
        // 验证归一化常量的正确性
        assert_eq!(GRIPPER_POSITION_SCALE, 100.0);
        assert_eq!(GRIPPER_FORCE_SCALE, 10.0);

        // 测试归一化
        let travel_mm = 50.0;
        let normalized = travel_mm / GRIPPER_POSITION_SCALE;
        assert_eq!(normalized, 0.5);

        let torque_nm = 5.0;
        let normalized = torque_nm / GRIPPER_FORCE_SCALE;
        assert_eq!(normalized, 0.5);
    }

    #[test]
    fn test_can_id_constants() {
        // 验证 CAN ID 常量的正确性
        assert_eq!(ID_MOTOR_ENABLE, 0x471);
        assert_eq!(ID_MIT_CONTROL_BASE, 0x15A);
        assert_eq!(ID_JOINT_CONTROL_12, 0x155);
        assert_eq!(ID_JOINT_CONTROL_34, 0x156);
        assert_eq!(ID_JOINT_CONTROL_56, 0x157);
        assert_eq!(ID_CONTROL_MODE, 0x151);
        assert_eq!(ID_EMERGENCY_STOP, 0x150);
        assert_eq!(ID_GRIPPER_CONTROL, 0x159);
    }
}
