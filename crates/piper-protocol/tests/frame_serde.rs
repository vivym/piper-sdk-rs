#![cfg(feature = "serde")]

use bincode::Options;
use piper_protocol::frame::{CanId, PiperFrame};

fn fixed_bincode_frame_bytes(
    id: u32,
    format: u8,
    data_len: u8,
    data: [u8; 8],
    timestamp_us: u64,
) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&id.to_le_bytes());
    bytes.push(format);
    bytes.push(data_len);
    bytes.extend_from_slice(&data);
    bytes.extend_from_slice(&timestamp_us.to_le_bytes());
    bytes
}

#[test]
fn json_uses_explicit_human_readable_shape() {
    let frame = PiperFrame::new_standard(0x123, [1, 2, 3]).unwrap().with_timestamp_us(99);
    let json = serde_json::to_value(frame).unwrap();
    assert_eq!(
        json,
        serde_json::json!({
            "id": 0x123,
            "format": "standard",
            "data": [1, 2, 3],
            "timestamp_us": 99
        })
    );
}

#[test]
fn json_rejects_old_shape_and_invalid_format() {
    let old = serde_json::json!({
        "id": 0x123,
        "data": [1, 2, 3, 0, 0, 0, 0, 0],
        "len": 3,
        "is_extended": false,
        "timestamp_us": 0
    });
    assert!(serde_json::from_value::<PiperFrame>(old).is_err());

    let bad_format = serde_json::json!({
        "id": 0x123,
        "format": "both",
        "data": [],
        "timestamp_us": 0
    });
    assert!(serde_json::from_value::<PiperFrame>(bad_format).is_err());
}

#[test]
fn json_rejects_missing_duplicate_and_unknown_fields() {
    assert!(
        serde_json::from_str::<PiperFrame>(r#"{"id":1,"format":"standard","data":[]}"#).is_err()
    );
    assert!(
        serde_json::from_str::<PiperFrame>(
            r#"{"id":1,"format":"standard","data":[],"timestamp_us":0,"extra":true}"#
        )
        .is_err()
    );
    assert!(
        serde_json::from_str::<PiperFrame>(
            r#"{"id":1,"id":2,"format":"standard","data":[],"timestamp_us":0}"#
        )
        .is_err()
    );
}

#[test]
fn json_rejects_invalid_ids_and_oversized_data() {
    let invalid_standard_id = serde_json::json!({
        "id": 0x800,
        "format": "standard",
        "data": [],
        "timestamp_us": 0
    });
    assert!(serde_json::from_value::<PiperFrame>(invalid_standard_id).is_err());

    let invalid_extended_id = serde_json::json!({
        "id": 0x2000_0000u32,
        "format": "extended",
        "data": [],
        "timestamp_us": 0
    });
    assert!(serde_json::from_value::<PiperFrame>(invalid_extended_id).is_err());

    let oversized_data = serde_json::json!({
        "id": 0x123,
        "format": "standard",
        "data": [0, 1, 2, 3, 4, 5, 6, 7, 8],
        "timestamp_us": 0
    });
    assert!(serde_json::from_value::<PiperFrame>(oversized_data).is_err());
}

#[test]
fn bincode_uses_canonical_frame_helper_and_rejects_bad_values() {
    let frame = PiperFrame::new_extended(0x123, [1, 2]).unwrap().with_timestamp_us(7);
    let encoded = bincode::DefaultOptions::new()
        .with_little_endian()
        .with_fixint_encoding()
        .serialize(&frame)
        .unwrap();
    assert_eq!(
        encoded,
        fixed_bincode_frame_bytes(0x123, 1, 2, [1, 2, 0, 0, 0, 0, 0, 0], 7)
    );

    let decoded: PiperFrame = bincode::DefaultOptions::new()
        .with_little_endian()
        .with_fixint_encoding()
        .reject_trailing_bytes()
        .deserialize(&encoded)
        .unwrap();
    assert_eq!(decoded.id(), CanId::extended(0x123).unwrap());
    assert_eq!(decoded.data(), &[1, 2]);
    assert_eq!(decoded.timestamp_us(), 7);
}

#[test]
fn bincode_rejects_invalid_format_dlc_and_noncanonical_padding() {
    let invalid_format = fixed_bincode_frame_bytes(0x123, 9, 0, [0; 8], 0);
    let invalid_dlc = fixed_bincode_frame_bytes(0x123, 0, 9, [0; 8], 0);
    let noncanonical_padding =
        fixed_bincode_frame_bytes(0x123, 0, 1, [1, 0xAA, 0, 0, 0, 0, 0, 0], 0);

    for bytes in [invalid_format, invalid_dlc, noncanonical_padding] {
        let result: Result<PiperFrame, _> = bincode::DefaultOptions::new()
            .with_little_endian()
            .with_fixint_encoding()
            .reject_trailing_bytes()
            .deserialize(&bytes);
        assert!(result.is_err());
    }
}

#[test]
fn bincode_rejects_invalid_ids_for_declared_format() {
    let invalid_standard_id = fixed_bincode_frame_bytes(0x800, 0, 0, [0; 8], 0);
    let invalid_extended_id = fixed_bincode_frame_bytes(0x2000_0000, 1, 0, [0; 8], 0);

    for bytes in [invalid_standard_id, invalid_extended_id] {
        let result: Result<PiperFrame, _> = bincode::DefaultOptions::new()
            .with_little_endian()
            .with_fixint_encoding()
            .reject_trailing_bytes()
            .deserialize(&bytes);
        assert!(result.is_err());
    }
}
