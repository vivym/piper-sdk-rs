use piper_protocol::frame::{
    CanData, CanId, ExtendedCanId, FrameError, JointIndex, PiperFrame, StandardCanId,
};
use piper_protocol::ids::{self, FrameType};
use std::collections::{BTreeSet, HashSet};

#[test]
fn id_constructors_reject_out_of_range_values() {
    assert_eq!(
        StandardCanId::new(0x800),
        Err(FrameError::InvalidStandardId { id: 0x800 })
    );
    assert_eq!(
        ExtendedCanId::new(0x2000_0000),
        Err(FrameError::InvalidExtendedId { id: 0x2000_0000 })
    );
}

#[test]
fn payload_constructors_reject_without_truncating() {
    let error = CanData::new([0u8; 9]).unwrap_err();
    assert_eq!(error, FrameError::PayloadTooLong { len: 9, max: 8 });
}

#[test]
fn frame_constructors_reject_oversized_payloads_without_truncating() {
    assert_eq!(
        PiperFrame::new_standard(0x123, [0u8; 9]),
        Err(FrameError::PayloadTooLong { len: 9, max: 8 })
    );
    assert_eq!(
        PiperFrame::new_extended(0x123, [0u8; 9]),
        Err(FrameError::PayloadTooLong { len: 9, max: 8 })
    );
}

#[test]
fn frame_constructors_reject_invalid_ids_at_public_boundary() {
    assert_eq!(
        PiperFrame::new_standard(0x800, [0u8; 0]),
        Err(FrameError::InvalidStandardId { id: 0x800 })
    );
    assert_eq!(
        PiperFrame::new_extended(0x2000_0000, [0u8; 0]),
        Err(FrameError::InvalidExtendedId { id: 0x2000_0000 })
    );
}

#[test]
fn padded_payload_normalizes_unused_bytes() {
    let data = CanData::from_padded([1, 2, 3, 0xAA, 0xBB, 0, 0, 0], 3).unwrap();
    assert_eq!(data.as_slice(), &[1, 2, 3]);
    assert_eq!(data.as_padded(), &[1, 2, 3, 0, 0, 0, 0, 0]);
}

#[test]
fn persisted_padding_validator_reports_noncanonical_byte() {
    let error = CanData::validate_canonical_padding([1, 2, 0xAA, 0, 0, 0, 0, 0], 2).unwrap_err();
    assert_eq!(
        error,
        FrameError::NonCanonicalPadding {
            index: 2,
            value: 0xAA
        }
    );
}

#[test]
fn frame_constructors_preserve_format_and_zero_timestamp() {
    let standard = PiperFrame::new_standard(0x123, [1, 2, 3]).unwrap();
    let extended = PiperFrame::new_extended(0x123, [1, 2, 3]).unwrap();

    assert_eq!(
        standard.id(),
        CanId::Standard(StandardCanId::new(0x123).unwrap())
    );
    assert_eq!(
        extended.id(),
        CanId::Extended(ExtendedCanId::new(0x123).unwrap())
    );
    assert_eq!(standard.timestamp_us(), 0);
    assert_eq!(extended.timestamp_us(), 0);
    assert_ne!(standard.id(), extended.id());
}

#[test]
fn with_timestamp_is_the_only_public_timestamp_setter() {
    let frame = PiperFrame::new_standard(0x123, [1]).unwrap();
    assert_eq!(frame.timestamp_us(), 0);
    assert_eq!(frame.with_timestamp_us(42).timestamp_us(), 42);
    assert_eq!(frame.timestamp_us(), 0);
}

#[test]
fn id_types_support_hash_and_ordering() {
    let mut ids = BTreeSet::new();
    ids.insert(CanId::standard(0x123).unwrap());
    ids.insert(CanId::extended(0x123).unwrap());
    assert_eq!(ids.len(), 2);

    let mut hashed_ids = HashSet::new();
    hashed_ids.insert(CanId::standard(0x123).unwrap());
    hashed_ids.insert(CanId::extended(0x123).unwrap());
    assert_eq!(hashed_ids.len(), 2);
    assert!(hashed_ids.contains(&CanId::standard(0x123).unwrap()));
    assert!(hashed_ids.contains(&CanId::extended(0x123).unwrap()));

    let mut joints = HashSet::new();
    joints.insert(JointIndex::new(1).unwrap());
    assert!(joints.contains(&JointIndex::new(1).unwrap()));
}

#[test]
fn joint_index_is_one_based_and_bounded() {
    assert!(JointIndex::new(0).is_err());
    assert_eq!(JointIndex::new(1).unwrap().get(), 1);
    assert_eq!(JointIndex::new(1).unwrap().zero_based(), 0);
    assert_eq!(JointIndex::new(6).unwrap().zero_based(), 5);
    assert!(JointIndex::new(7).is_err());
}

#[test]
fn protocol_classification_is_format_aware() {
    let standard = CanId::standard(0x251).unwrap();
    let extended = CanId::extended(0x251).unwrap();

    assert_eq!(FrameType::from_id(standard), FrameType::Feedback);
    assert_eq!(FrameType::from_id(extended), FrameType::Unknown);
    assert!(ids::is_robot_feedback_id(standard));
    assert!(!ids::is_robot_feedback_id(extended));
}

#[test]
fn dynamic_protocol_ids_require_valid_joint_index() {
    let joint = JointIndex::new(6).unwrap();
    assert_eq!(ids::mit_control_id(joint).raw(), 0x15F);
}
