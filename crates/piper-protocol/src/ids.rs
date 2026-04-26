//! CAN ID constants and classifiers.
//!
//! Public protocol IDs are typed `StandardCanId` values. Raw `u32` aliases are
//! kept crate-private only while older protocol builders/parsers are migrated.

pub use crate::frame::JointIndex;
pub use crate::frame::protocol_ids::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameType {
    Feedback,
    Control,
    Config,
    Unknown,
}

impl FrameType {
    pub fn from_id(id: crate::frame::CanId) -> Self {
        crate::frame::protocol_ids::frame_type_from_id(id)
    }
}

pub(crate) mod raw {
    use crate::frame::protocol_ids as typed;

    pub(crate) const ID_ROBOT_STATUS: u32 = typed::ID_ROBOT_STATUS.raw() as u32;
    pub(crate) const ID_END_POSE_1: u32 = typed::ID_END_POSE_1.raw() as u32;
    pub(crate) const ID_END_POSE_2: u32 = typed::ID_END_POSE_2.raw() as u32;
    pub(crate) const ID_END_POSE_3: u32 = typed::ID_END_POSE_3.raw() as u32;
    pub(crate) const ID_JOINT_FEEDBACK_12: u32 = typed::ID_JOINT_FEEDBACK_12.raw() as u32;
    pub(crate) const ID_JOINT_FEEDBACK_34: u32 = typed::ID_JOINT_FEEDBACK_34.raw() as u32;
    pub(crate) const ID_JOINT_FEEDBACK_56: u32 = typed::ID_JOINT_FEEDBACK_56.raw() as u32;
    pub(crate) const ID_GRIPPER_FEEDBACK: u32 = typed::ID_GRIPPER_FEEDBACK.raw() as u32;

    pub(crate) const ID_JOINT_DRIVER_HIGH_SPEED_BASE: u32 =
        typed::ID_JOINT_DRIVER_HIGH_SPEED_1.raw() as u32;
    pub(crate) const ID_JOINT_DRIVER_LOW_SPEED_BASE: u32 =
        typed::ID_JOINT_DRIVER_LOW_SPEED_1.raw() as u32;
    pub(crate) const ID_JOINT_END_VELOCITY_ACCEL_BASE: u32 =
        typed::ID_JOINT_END_VELOCITY_ACCEL_1.raw() as u32;

    pub(crate) const ID_FIRMWARE_READ: u32 = typed::ID_FIRMWARE_READ.raw() as u32;

    pub(crate) const ID_EMERGENCY_STOP: u32 = typed::ID_EMERGENCY_STOP.raw() as u32;
    pub(crate) const ID_CONTROL_MODE: u32 = typed::ID_CONTROL_MODE.raw() as u32;
    pub(crate) const ID_END_POSE_CONTROL_1: u32 = typed::ID_END_POSE_CONTROL_1.raw() as u32;
    pub(crate) const ID_END_POSE_CONTROL_2: u32 = typed::ID_END_POSE_CONTROL_2.raw() as u32;
    pub(crate) const ID_END_POSE_CONTROL_3: u32 = typed::ID_END_POSE_CONTROL_3.raw() as u32;
    pub(crate) const ID_JOINT_CONTROL_12: u32 = typed::ID_JOINT_CONTROL_12.raw() as u32;
    pub(crate) const ID_JOINT_CONTROL_34: u32 = typed::ID_JOINT_CONTROL_34.raw() as u32;
    pub(crate) const ID_JOINT_CONTROL_56: u32 = typed::ID_JOINT_CONTROL_56.raw() as u32;
    pub(crate) const ID_ARC_POINT: u32 = typed::ID_ARC_POINT.raw() as u32;
    pub(crate) const ID_GRIPPER_CONTROL: u32 = typed::ID_GRIPPER_CONTROL.raw() as u32;
    pub(crate) const ID_MIT_CONTROL_BASE: u32 = typed::ID_MIT_CONTROL_1.raw() as u32;
    pub(crate) const ID_LIGHT_CONTROL: u32 = typed::ID_LIGHT_CONTROL.raw() as u32;
    pub(crate) const ID_FIRMWARE_UPGRADE: u32 = typed::ID_FIRMWARE_UPGRADE.raw() as u32;

    pub(crate) const ID_MASTER_SLAVE_MODE: u32 = typed::ID_MASTER_SLAVE_MODE.raw() as u32;
    pub(crate) const ID_MOTOR_ENABLE: u32 = typed::ID_MOTOR_ENABLE.raw() as u32;
    pub(crate) const ID_QUERY_MOTOR_LIMIT: u32 = typed::ID_QUERY_MOTOR_LIMIT.raw() as u32;
    pub(crate) const ID_MOTOR_LIMIT_FEEDBACK: u32 = typed::ID_MOTOR_LIMIT_FEEDBACK.raw() as u32;
    pub(crate) const ID_SET_MOTOR_LIMIT: u32 = typed::ID_SET_MOTOR_LIMIT.raw() as u32;
    pub(crate) const ID_JOINT_SETTING: u32 = typed::ID_JOINT_SETTING.raw() as u32;
    pub(crate) const ID_SETTING_RESPONSE: u32 = typed::ID_SETTING_RESPONSE.raw() as u32;
    pub(crate) const ID_PARAMETER_QUERY_SET: u32 = typed::ID_PARAMETER_QUERY_SET.raw() as u32;
    pub(crate) const ID_END_VELOCITY_ACCEL_FEEDBACK: u32 =
        typed::ID_END_VELOCITY_ACCEL_FEEDBACK.raw() as u32;
    pub(crate) const ID_SET_END_VELOCITY_ACCEL: u32 = typed::ID_SET_END_VELOCITY_ACCEL.raw() as u32;
    pub(crate) const ID_COLLISION_PROTECTION_LEVEL: u32 =
        typed::ID_COLLISION_PROTECTION_LEVEL.raw() as u32;
    pub(crate) const ID_COLLISION_PROTECTION_LEVEL_FEEDBACK: u32 =
        typed::ID_COLLISION_PROTECTION_LEVEL_FEEDBACK.raw() as u32;
    pub(crate) const ID_MOTOR_MAX_ACCEL_FEEDBACK: u32 =
        typed::ID_MOTOR_MAX_ACCEL_FEEDBACK.raw() as u32;
    pub(crate) const ID_GRIPPER_TEACH_PARAMS: u32 = typed::ID_GRIPPER_TEACH_PARAMS.raw() as u32;
    pub(crate) const ID_GRIPPER_TEACH_PARAMS_FEEDBACK: u32 =
        typed::ID_GRIPPER_TEACH_PARAMS_FEEDBACK.raw() as u32;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::CanId;

    #[test]
    fn frame_type_classifies_standard_protocol_ids() {
        assert_eq!(
            FrameType::from_id(CanId::standard(0x2A1).unwrap()),
            FrameType::Feedback
        );
        assert_eq!(
            FrameType::from_id(CanId::standard(0x256).unwrap()),
            FrameType::Feedback
        );
        assert_eq!(
            FrameType::from_id(CanId::standard(0x266).unwrap()),
            FrameType::Feedback
        );
        assert_eq!(
            FrameType::from_id(CanId::standard(0x486).unwrap()),
            FrameType::Feedback
        );
        assert_eq!(
            FrameType::from_id(CanId::standard(0x150).unwrap()),
            FrameType::Control
        );
        assert_eq!(
            FrameType::from_id(CanId::standard(0x15F).unwrap()),
            FrameType::Control
        );
        assert_eq!(
            FrameType::from_id(CanId::standard(0x470).unwrap()),
            FrameType::Config
        );
        assert_eq!(
            FrameType::from_id(CanId::standard(0x47E).unwrap()),
            FrameType::Config
        );
    }

    #[test]
    fn frame_type_rejects_extended_same_raw_id() {
        assert_eq!(
            FrameType::from_id(CanId::standard(0x251).unwrap()),
            FrameType::Feedback
        );
        assert_eq!(
            FrameType::from_id(CanId::extended(0x251).unwrap()),
            FrameType::Unknown
        );
    }

    #[test]
    fn frame_type_reports_unknown_for_auxiliary_or_shifted_ids() {
        assert_eq!(
            FrameType::from_id(CanId::standard(0x100).unwrap()),
            FrameType::Unknown
        );
        assert_eq!(
            FrameType::from_id(CanId::standard(0x121).unwrap()),
            FrameType::Unknown
        );
        assert_eq!(
            FrameType::from_id(CanId::standard(0x422).unwrap()),
            FrameType::Unknown
        );
        assert_eq!(
            FrameType::from_id(CanId::standard(0x16A).unwrap()),
            FrameType::Unknown
        );
        assert_eq!(
            FrameType::from_id(CanId::standard(0x2B1).unwrap()),
            FrameType::Unknown
        );
    }

    #[test]
    fn protocol_id_constants_are_typed_standard_ids() {
        assert_eq!(ID_ROBOT_STATUS.raw(), 0x2A1);
        assert_eq!(ID_EMERGENCY_STOP.raw(), 0x150);
        assert_eq!(ID_MOTOR_ENABLE.raw(), 0x471);
    }

    #[test]
    fn robot_feedback_classifier_uses_typed_standard_ids() {
        assert!(is_robot_feedback_id(ID_ROBOT_STATUS.into()));
        assert!(is_robot_feedback_id(ID_JOINT_FEEDBACK_12.into()));
        assert!(is_robot_feedback_id(ID_JOINT_DRIVER_HIGH_SPEED_6.into()));
        assert!(is_robot_feedback_id(ID_JOINT_DRIVER_LOW_SPEED_6.into()));
        assert!(is_robot_feedback_id(ID_MOTOR_LIMIT_FEEDBACK.into()));
        assert!(is_robot_feedback_id(ID_CONTROL_MODE.into()));
        assert!(is_robot_feedback_id(ID_JOINT_CONTROL_56.into()));
        assert!(is_robot_feedback_id(ID_GRIPPER_CONTROL.into()));

        assert!(!is_robot_feedback_id(ID_EMERGENCY_STOP.into()));
        assert!(!is_robot_feedback_id(ID_MOTOR_ENABLE.into()));
        assert!(!is_robot_feedback_id(ID_SET_END_VELOCITY_ACCEL.into()));
        assert!(!is_robot_feedback_id(
            ID_GRIPPER_TEACH_PARAMS_FEEDBACK.into()
        ));
        assert!(!is_robot_feedback_id(
            CanId::extended(ID_ROBOT_STATUS.raw() as u32).unwrap()
        ));
    }

    #[test]
    fn driver_feedback_ids_are_typed_standard_ids() {
        assert!(driver_rx_robot_feedback_ids().contains(&ID_JOINT_DRIVER_HIGH_SPEED_6));
        assert!(driver_rx_robot_feedback_ids().contains(&ID_JOINT_DRIVER_LOW_SPEED_6));

        for id in driver_rx_robot_feedback_ids() {
            assert!(
                is_robot_feedback_id((*id).into()),
                "shared driver RX ID surface must stay aligned with classifier for 0x{:X}",
                id.raw()
            );
        }

        assert!(!driver_rx_robot_feedback_ids().contains(&ID_EMERGENCY_STOP));
        assert!(!driver_rx_robot_feedback_ids().contains(&ID_MOTOR_ENABLE));
        assert!(!driver_rx_robot_feedback_ids().contains(&ID_SET_END_VELOCITY_ACCEL));
        assert!(!driver_rx_robot_feedback_ids().contains(&ID_GRIPPER_TEACH_PARAMS_FEEDBACK));
    }

    #[test]
    fn dynamic_id_accessors_match_protocol_values() {
        assert_eq!(
            joint_driver_high_speed_id(JointIndex::new(1).unwrap()).raw(),
            0x251
        );
        assert_eq!(
            joint_driver_high_speed_id(JointIndex::new(6).unwrap()).raw(),
            0x256
        );
        assert_eq!(
            joint_driver_low_speed_id(JointIndex::new(1).unwrap()).raw(),
            0x261
        );
        assert_eq!(
            joint_driver_low_speed_id(JointIndex::new(6).unwrap()).raw(),
            0x266
        );
        assert_eq!(
            joint_end_velocity_accel_id(JointIndex::new(1).unwrap()).raw(),
            0x481
        );
        assert_eq!(
            joint_end_velocity_accel_id(JointIndex::new(6).unwrap()).raw(),
            0x486
        );
        assert_eq!(mit_control_id(JointIndex::new(1).unwrap()).raw(), 0x15A);
        assert_eq!(mit_control_id(JointIndex::new(6).unwrap()).raw(), 0x15F);
    }
}
