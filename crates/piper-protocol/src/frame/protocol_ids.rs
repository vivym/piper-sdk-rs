use super::{CanId, JointIndex, StandardCanId};

const fn standard(raw: u16) -> StandardCanId {
    StandardCanId::new_const(raw)
}

pub const FEEDBACK_BASE_ID: StandardCanId = ID_ROBOT_STATUS;
pub const FEEDBACK_END_ID: StandardCanId = ID_GRIPPER_FEEDBACK;

pub const CONTROL_BASE_ID: StandardCanId = ID_EMERGENCY_STOP;
pub const CONTROL_END_ID: StandardCanId = ID_MIT_CONTROL_6;

pub const CONFIG_BASE_ID: StandardCanId = ID_MASTER_SLAVE_MODE;
pub const CONFIG_END_ID: StandardCanId = ID_GRIPPER_TEACH_PARAMS_FEEDBACK;

pub const ID_ROBOT_STATUS: StandardCanId = standard(0x2A1);
pub const ID_END_POSE_1: StandardCanId = standard(0x2A2);
pub const ID_END_POSE_2: StandardCanId = standard(0x2A3);
pub const ID_END_POSE_3: StandardCanId = standard(0x2A4);
pub const ID_JOINT_FEEDBACK_12: StandardCanId = standard(0x2A5);
pub const ID_JOINT_FEEDBACK_34: StandardCanId = standard(0x2A6);
pub const ID_JOINT_FEEDBACK_56: StandardCanId = standard(0x2A7);
pub const ID_GRIPPER_FEEDBACK: StandardCanId = standard(0x2A8);

pub const ID_JOINT_DRIVER_HIGH_SPEED_1: StandardCanId = standard(0x251);
pub const ID_JOINT_DRIVER_HIGH_SPEED_2: StandardCanId = standard(0x252);
pub const ID_JOINT_DRIVER_HIGH_SPEED_3: StandardCanId = standard(0x253);
pub const ID_JOINT_DRIVER_HIGH_SPEED_4: StandardCanId = standard(0x254);
pub const ID_JOINT_DRIVER_HIGH_SPEED_5: StandardCanId = standard(0x255);
pub const ID_JOINT_DRIVER_HIGH_SPEED_6: StandardCanId = standard(0x256);

pub const ID_JOINT_DRIVER_LOW_SPEED_1: StandardCanId = standard(0x261);
pub const ID_JOINT_DRIVER_LOW_SPEED_2: StandardCanId = standard(0x262);
pub const ID_JOINT_DRIVER_LOW_SPEED_3: StandardCanId = standard(0x263);
pub const ID_JOINT_DRIVER_LOW_SPEED_4: StandardCanId = standard(0x264);
pub const ID_JOINT_DRIVER_LOW_SPEED_5: StandardCanId = standard(0x265);
pub const ID_JOINT_DRIVER_LOW_SPEED_6: StandardCanId = standard(0x266);

pub const ID_JOINT_END_VELOCITY_ACCEL_1: StandardCanId = standard(0x481);
pub const ID_JOINT_END_VELOCITY_ACCEL_2: StandardCanId = standard(0x482);
pub const ID_JOINT_END_VELOCITY_ACCEL_3: StandardCanId = standard(0x483);
pub const ID_JOINT_END_VELOCITY_ACCEL_4: StandardCanId = standard(0x484);
pub const ID_JOINT_END_VELOCITY_ACCEL_5: StandardCanId = standard(0x485);
pub const ID_JOINT_END_VELOCITY_ACCEL_6: StandardCanId = standard(0x486);

pub const ID_FIRMWARE_READ: StandardCanId = standard(0x4AF);

pub const ID_EMERGENCY_STOP: StandardCanId = standard(0x150);
pub const ID_CONTROL_MODE: StandardCanId = standard(0x151);
pub const ID_END_POSE_CONTROL_1: StandardCanId = standard(0x152);
pub const ID_END_POSE_CONTROL_2: StandardCanId = standard(0x153);
pub const ID_END_POSE_CONTROL_3: StandardCanId = standard(0x154);
pub const ID_JOINT_CONTROL_12: StandardCanId = standard(0x155);
pub const ID_JOINT_CONTROL_34: StandardCanId = standard(0x156);
pub const ID_JOINT_CONTROL_56: StandardCanId = standard(0x157);
pub const ID_ARC_POINT: StandardCanId = standard(0x158);
pub const ID_GRIPPER_CONTROL: StandardCanId = standard(0x159);

pub const ID_MIT_CONTROL_1: StandardCanId = standard(0x15A);
pub const ID_MIT_CONTROL_2: StandardCanId = standard(0x15B);
pub const ID_MIT_CONTROL_3: StandardCanId = standard(0x15C);
pub const ID_MIT_CONTROL_4: StandardCanId = standard(0x15D);
pub const ID_MIT_CONTROL_5: StandardCanId = standard(0x15E);
pub const ID_MIT_CONTROL_6: StandardCanId = standard(0x15F);

pub const ID_LIGHT_CONTROL: StandardCanId = standard(0x121);
pub const ID_FIRMWARE_UPGRADE: StandardCanId = standard(0x422);

pub const ID_MASTER_SLAVE_MODE: StandardCanId = standard(0x470);
pub const ID_MOTOR_ENABLE: StandardCanId = standard(0x471);
pub const ID_QUERY_MOTOR_LIMIT: StandardCanId = standard(0x472);
pub const ID_MOTOR_LIMIT_FEEDBACK: StandardCanId = standard(0x473);
pub const ID_SET_MOTOR_LIMIT: StandardCanId = standard(0x474);
pub const ID_JOINT_SETTING: StandardCanId = standard(0x475);
pub const ID_SETTING_RESPONSE: StandardCanId = standard(0x476);
pub const ID_PARAMETER_QUERY_SET: StandardCanId = standard(0x477);
pub const ID_END_VELOCITY_ACCEL_FEEDBACK: StandardCanId = standard(0x478);
pub const ID_SET_END_VELOCITY_ACCEL: StandardCanId = standard(0x479);
pub const ID_COLLISION_PROTECTION_LEVEL: StandardCanId = standard(0x47A);
pub const ID_COLLISION_PROTECTION_LEVEL_FEEDBACK: StandardCanId = standard(0x47B);
pub const ID_MOTOR_MAX_ACCEL_FEEDBACK: StandardCanId = standard(0x47C);
pub const ID_GRIPPER_TEACH_PARAMS: StandardCanId = standard(0x47D);
pub const ID_GRIPPER_TEACH_PARAMS_FEEDBACK: StandardCanId = standard(0x47E);

const JOINT_DRIVER_HIGH_SPEED_IDS: [StandardCanId; 6] = [
    ID_JOINT_DRIVER_HIGH_SPEED_1,
    ID_JOINT_DRIVER_HIGH_SPEED_2,
    ID_JOINT_DRIVER_HIGH_SPEED_3,
    ID_JOINT_DRIVER_HIGH_SPEED_4,
    ID_JOINT_DRIVER_HIGH_SPEED_5,
    ID_JOINT_DRIVER_HIGH_SPEED_6,
];

const JOINT_DRIVER_LOW_SPEED_IDS: [StandardCanId; 6] = [
    ID_JOINT_DRIVER_LOW_SPEED_1,
    ID_JOINT_DRIVER_LOW_SPEED_2,
    ID_JOINT_DRIVER_LOW_SPEED_3,
    ID_JOINT_DRIVER_LOW_SPEED_4,
    ID_JOINT_DRIVER_LOW_SPEED_5,
    ID_JOINT_DRIVER_LOW_SPEED_6,
];

const JOINT_END_VELOCITY_ACCEL_IDS: [StandardCanId; 6] = [
    ID_JOINT_END_VELOCITY_ACCEL_1,
    ID_JOINT_END_VELOCITY_ACCEL_2,
    ID_JOINT_END_VELOCITY_ACCEL_3,
    ID_JOINT_END_VELOCITY_ACCEL_4,
    ID_JOINT_END_VELOCITY_ACCEL_5,
    ID_JOINT_END_VELOCITY_ACCEL_6,
];

const MIT_CONTROL_IDS: [StandardCanId; 6] = [
    ID_MIT_CONTROL_1,
    ID_MIT_CONTROL_2,
    ID_MIT_CONTROL_3,
    ID_MIT_CONTROL_4,
    ID_MIT_CONTROL_5,
    ID_MIT_CONTROL_6,
];

pub const DRIVER_RX_ROBOT_FEEDBACK_IDS: [StandardCanId; 30] = [
    ID_ROBOT_STATUS,
    ID_END_POSE_1,
    ID_END_POSE_2,
    ID_END_POSE_3,
    ID_JOINT_FEEDBACK_12,
    ID_JOINT_FEEDBACK_34,
    ID_JOINT_FEEDBACK_56,
    ID_GRIPPER_FEEDBACK,
    ID_JOINT_DRIVER_HIGH_SPEED_1,
    ID_JOINT_DRIVER_HIGH_SPEED_2,
    ID_JOINT_DRIVER_HIGH_SPEED_3,
    ID_JOINT_DRIVER_HIGH_SPEED_4,
    ID_JOINT_DRIVER_HIGH_SPEED_5,
    ID_JOINT_DRIVER_HIGH_SPEED_6,
    ID_JOINT_DRIVER_LOW_SPEED_1,
    ID_JOINT_DRIVER_LOW_SPEED_2,
    ID_JOINT_DRIVER_LOW_SPEED_3,
    ID_JOINT_DRIVER_LOW_SPEED_4,
    ID_JOINT_DRIVER_LOW_SPEED_5,
    ID_JOINT_DRIVER_LOW_SPEED_6,
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

pub fn joint_driver_high_speed_id(joint: JointIndex) -> StandardCanId {
    JOINT_DRIVER_HIGH_SPEED_IDS[joint.zero_based() as usize]
}

pub fn joint_driver_low_speed_id(joint: JointIndex) -> StandardCanId {
    JOINT_DRIVER_LOW_SPEED_IDS[joint.zero_based() as usize]
}

pub fn joint_end_velocity_accel_id(joint: JointIndex) -> StandardCanId {
    JOINT_END_VELOCITY_ACCEL_IDS[joint.zero_based() as usize]
}

pub fn mit_control_id(joint: JointIndex) -> StandardCanId {
    MIT_CONTROL_IDS[joint.zero_based() as usize]
}

pub const fn driver_rx_robot_feedback_ids() -> &'static [StandardCanId] {
    &DRIVER_RX_ROBOT_FEEDBACK_IDS
}

pub fn is_robot_feedback_id(id: CanId) -> bool {
    let Some(standard_id) = id.as_standard() else {
        return false;
    };

    DRIVER_RX_ROBOT_FEEDBACK_IDS.contains(&standard_id)
}

pub fn frame_type_from_id(id: CanId) -> crate::ids::FrameType {
    let Some(standard_id) = id.as_standard() else {
        return crate::ids::FrameType::Unknown;
    };

    match standard_id.raw() {
        0x2A1..=0x2A8 | 0x251..=0x256 | 0x261..=0x266 | 0x481..=0x486 | 0x4AF => {
            crate::ids::FrameType::Feedback
        },
        0x150..=0x15F => crate::ids::FrameType::Control,
        0x470..=0x47E => crate::ids::FrameType::Config,
        _ => crate::ids::FrameType::Unknown,
    }
}
