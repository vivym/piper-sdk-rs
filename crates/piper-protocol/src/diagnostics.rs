#[cfg(test)]
mod tests {
    use crate::can::PiperFrame;
    use crate::config::{decode_collision_protection_feedback, decode_motor_limit_feedback};
    use crate::diagnostics::{DecodeResult, ProtocolDiagnostic};

    #[test]
    fn decode_collision_protection_out_of_range_returns_diagnostic() {
        let frame = PiperFrame::new_standard(0x47B, &[255, 0, 0, 0, 0, 0, 0, 0]);
        match decode_collision_protection_feedback(frame) {
            DecodeResult::Diagnostic(ProtocolDiagnostic::OutOfRange { field, .. }) => {
                assert_eq!(field, "collision_protection_level");
            }
            other => panic!("expected out-of-range diagnostic, got {other:?}"),
        }
    }

    #[test]
    fn decode_motor_limit_valid_frame_returns_data() {
        let frame = PiperFrame::new_standard(0x473, &[1, 0x07, 0x08, 0xF8, 0xF8, 0x01, 0x2C, 0x00]);
        assert!(matches!(
            decode_motor_limit_feedback(frame),
            DecodeResult::Data(_)
        ));
    }
}
