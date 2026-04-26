use piper_protocol::{
    ControlModeCommandFrame, MotorLimitFeedback, PiperFrame, ProtocolError, RobotStatusFeedback,
    SettingResponse, ids,
};

fn assert_invalid_can_id<T: core::fmt::Debug>(
    result: Result<T, ProtocolError>,
    expected_raw_id: u32,
) {
    match result {
        Err(ProtocolError::InvalidCanId { id }) => assert_eq!(id, expected_raw_id),
        other => panic!("expected InvalidCanId({expected_raw_id:#X}), got {other:?}"),
    }
}

#[test]
fn feedback_parser_rejects_extended_frame_with_matching_raw_id() {
    let frame = PiperFrame::new_extended(ids::ID_ROBOT_STATUS.raw() as u32, [0u8; 8]).unwrap();

    assert_invalid_can_id(
        RobotStatusFeedback::try_from(frame),
        ids::ID_ROBOT_STATUS.raw() as u32,
    );
}

#[test]
fn config_parsers_reject_extended_frames_with_matching_raw_ids() {
    let motor_limit =
        PiperFrame::new_extended(ids::ID_MOTOR_LIMIT_FEEDBACK.raw() as u32, [0u8; 8]).unwrap();
    assert_invalid_can_id(
        MotorLimitFeedback::try_from(motor_limit),
        ids::ID_MOTOR_LIMIT_FEEDBACK.raw() as u32,
    );

    let setting_response =
        PiperFrame::new_extended(ids::ID_SETTING_RESPONSE.raw() as u32, [0u8; 8]).unwrap();
    assert_invalid_can_id(
        SettingResponse::try_from(setting_response),
        ids::ID_SETTING_RESPONSE.raw() as u32,
    );
}

#[test]
fn parsers_reject_unknown_legal_standard_id() {
    let frame = PiperFrame::new_standard(0x700, [0u8; 8]).unwrap();

    assert_invalid_can_id(RobotStatusFeedback::try_from(frame), 0x700);
}

#[test]
fn control_builder_returns_typed_standard_protocol_id() {
    let frame = ControlModeCommandFrame::default().to_frame();

    assert_eq!(frame.id().as_standard(), Some(ids::ID_CONTROL_MODE));
    assert!(!frame.is_extended());
}
