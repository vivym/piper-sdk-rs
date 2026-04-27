use crate::{CanDeviceError, CanDeviceErrorKind, CanError, PiperFrame};
use piper_protocol::{CanData, ExtendedCanId, StandardCanId};

use super::CLASSIC_CAN_MTU;

#[derive(Debug)]
pub enum ParsedSocketCanFrame {
    Data(PiperFrame),
    RecoverableNonData,
    Fatal(CanError),
}

fn invalid_frame(message: impl Into<String>) -> CanError {
    CanError::Device(CanDeviceError::new(
        CanDeviceErrorKind::InvalidFrame,
        message,
    ))
}

fn fatal_frame_error(error: piper_protocol::FrameError) -> ParsedSocketCanFrame {
    ParsedSocketCanFrame::Fatal(CanError::Frame(error))
}

fn fatal_invalid_frame(message: impl Into<String>) -> ParsedSocketCanFrame {
    ParsedSocketCanFrame::Fatal(invalid_frame(message))
}

fn read_u32_ne(bytes: &[u8], start: usize) -> Option<u32> {
    let raw = bytes.get(start..start + 4)?;
    Some(u32::from_ne_bytes([raw[0], raw[1], raw[2], raw[3]]))
}

fn parse_error_frame(can_id: u32, data: [u8; 8]) -> ParsedSocketCanFrame {
    if (can_id & libc::CAN_ERR_BUSOFF) != 0 {
        return ParsedSocketCanFrame::Fatal(CanError::BusOff);
    }

    if (can_id & libc::CAN_ERR_CRTL) != 0 {
        let controller_status = data[1] as u32;
        let overflow_bits =
            (libc::CAN_ERR_CRTL_RX_OVERFLOW | libc::CAN_ERR_CRTL_TX_OVERFLOW) as u32;
        if (controller_status & overflow_bits) != 0 {
            return ParsedSocketCanFrame::Fatal(CanError::BufferOverflow);
        }
    }

    ParsedSocketCanFrame::RecoverableNonData
}

pub fn parse_libc_can_frame_bytes(
    bytes: &[u8],
    msg_len: usize,
    msg_flags: i32,
) -> ParsedSocketCanFrame {
    if (msg_flags & libc::MSG_TRUNC) != 0 {
        return fatal_invalid_frame("truncated SocketCAN frame");
    }

    if msg_len != CLASSIC_CAN_MTU {
        return fatal_invalid_frame("non-classic CAN MTU");
    }

    if bytes.len() < CLASSIC_CAN_MTU {
        return fatal_invalid_frame(format!(
            "short SocketCAN frame buffer: {} bytes",
            bytes.len()
        ));
    }

    let Some(can_id) = read_u32_ne(bytes, 0) else {
        return fatal_invalid_frame("missing SocketCAN can_id");
    };
    let dlc = bytes[4];

    if dlc > 8 {
        return fatal_frame_error(piper_protocol::FrameError::InvalidDlc { dlc });
    }

    let is_extended = (can_id & libc::CAN_EFF_FLAG) != 0;
    let is_rtr = (can_id & libc::CAN_RTR_FLAG) != 0;
    let is_error = (can_id & libc::CAN_ERR_FLAG) != 0;

    let mut data = [0u8; 8];
    data.copy_from_slice(&bytes[8..16]);

    if is_extended && is_error {
        return fatal_frame_error(piper_protocol::FrameError::InvalidExtendedId {
            id: can_id & !libc::CAN_EFF_FLAG,
        });
    }

    if is_error {
        return parse_error_frame(can_id, data);
    }

    if is_rtr {
        return ParsedSocketCanFrame::RecoverableNonData;
    }

    if is_extended {
        let id = can_id & libc::CAN_EFF_MASK;
        let id = match ExtendedCanId::new(id) {
            Ok(id) => id,
            Err(error) => return fatal_frame_error(error),
        };
        let data = match CanData::from_padded(data, dlc) {
            Ok(data) => data,
            Err(error) => return fatal_frame_error(error),
        };
        ParsedSocketCanFrame::Data(PiperFrame::extended(id, data))
    } else {
        let invalid_standard_bits =
            can_id & !(libc::CAN_SFF_MASK | libc::CAN_RTR_FLAG | libc::CAN_ERR_FLAG);
        if invalid_standard_bits != 0 {
            return fatal_frame_error(piper_protocol::FrameError::InvalidStandardId {
                id: can_id & !libc::CAN_RTR_FLAG,
            });
        }

        let id = can_id & libc::CAN_SFF_MASK;
        let id = match StandardCanId::new(id) {
            Ok(id) => id,
            Err(error) => return fatal_frame_error(error),
        };
        let data = match CanData::from_padded(data, dlc) {
            Ok(data) => data,
            Err(error) => return fatal_frame_error(error),
        };
        ParsedSocketCanFrame::Data(PiperFrame::standard(id, data))
    }
}

#[cfg(test)]
mod tests {
    use super::{CLASSIC_CAN_MTU, ParsedSocketCanFrame, parse_libc_can_frame_bytes};
    use crate::socketcan::CANFD_MTU;
    use crate::{CanDeviceErrorKind, CanError};
    use piper_protocol::FrameError;

    fn raw_frame_bytes(can_id: u32, dlc: u8, data: [u8; 8]) -> [u8; CLASSIC_CAN_MTU] {
        let mut bytes = [0u8; CLASSIC_CAN_MTU];
        bytes[..4].copy_from_slice(&can_id.to_ne_bytes());
        bytes[4] = dlc;
        bytes[8..16].copy_from_slice(&data);
        bytes
    }

    fn parse(bytes: &[u8], msg_len: usize, msg_flags: i32) -> ParsedSocketCanFrame {
        parse_libc_can_frame_bytes(bytes, msg_len, msg_flags)
    }

    fn fatal_message(parsed: ParsedSocketCanFrame) -> String {
        match parsed {
            ParsedSocketCanFrame::Fatal(error) => error.to_string(),
            other => panic!("expected fatal parse result, got {other:?}"),
        }
    }

    #[test]
    fn parses_classic_standard_data_frame() {
        let bytes = raw_frame_bytes(0x123, 4, [1, 2, 3, 4, 0xAA, 0xBB, 0xCC, 0xDD]);

        let ParsedSocketCanFrame::Data(frame) = parse(&bytes, CLASSIC_CAN_MTU, 0) else {
            panic!("expected data frame");
        };

        assert_eq!(frame.raw_id(), 0x123);
        assert!(frame.is_standard());
        assert_eq!(frame.dlc(), 4);
        assert_eq!(frame.data(), &[1, 2, 3, 4]);
        assert_eq!(frame.data_padded(), &[1, 2, 3, 4, 0, 0, 0, 0]);
        assert_eq!(frame.timestamp_us(), 0);
    }

    #[test]
    fn parses_classic_extended_data_frame() {
        let bytes = raw_frame_bytes(
            libc::CAN_EFF_FLAG | 0x01AB_CDEF,
            8,
            [0x10, 0x20, 0x30, 0x40, 0x50, 0x60, 0x70, 0x80],
        );

        let ParsedSocketCanFrame::Data(frame) = parse(&bytes, CLASSIC_CAN_MTU, 0) else {
            panic!("expected data frame");
        };

        assert_eq!(frame.raw_id(), 0x01AB_CDEF);
        assert!(frame.is_extended());
        assert_eq!(frame.dlc(), 8);
        assert_eq!(
            frame.data(),
            &[0x10, 0x20, 0x30, 0x40, 0x50, 0x60, 0x70, 0x80]
        );
    }

    #[test]
    fn rejects_canfd_mtu() {
        let bytes = [0u8; CANFD_MTU];

        let message = fatal_message(parse(&bytes, CANFD_MTU, 0));

        assert!(message.contains("non-classic CAN MTU"));
    }

    #[test]
    fn rejects_other_non_classic_mtu() {
        let bytes = raw_frame_bytes(0x123, 1, [0; 8]);

        let message = fatal_message(parse(&bytes, 12, 0));

        assert!(message.contains("non-classic CAN MTU"));
    }

    #[test]
    fn rejects_truncated_message_flag() {
        let bytes = raw_frame_bytes(0x123, 1, [0; 8]);

        let message = fatal_message(parse(&bytes, CLASSIC_CAN_MTU, libc::MSG_TRUNC));

        assert!(message.contains("truncated SocketCAN frame"));
    }

    #[test]
    fn treats_rtr_frames_as_recoverable_non_data() {
        let bytes = raw_frame_bytes(libc::CAN_RTR_FLAG | 0x123, 0, [0; 8]);

        assert!(matches!(
            parse(&bytes, CLASSIC_CAN_MTU, 0),
            ParsedSocketCanFrame::RecoverableNonData
        ));
    }

    #[test]
    fn treats_bus_off_error_frame_as_fatal() {
        let bytes = raw_frame_bytes(libc::CAN_ERR_FLAG | libc::CAN_ERR_BUSOFF, 8, [0; 8]);

        assert!(matches!(
            parse(&bytes, CLASSIC_CAN_MTU, 0),
            ParsedSocketCanFrame::Fatal(CanError::BusOff)
        ));
    }

    #[test]
    fn treats_controller_overflow_error_frame_as_fatal() {
        let mut data = [0u8; 8];
        data[1] = libc::CAN_ERR_CRTL_RX_OVERFLOW as u8;
        let bytes = raw_frame_bytes(libc::CAN_ERR_FLAG | libc::CAN_ERR_CRTL, 8, data);

        assert!(matches!(
            parse(&bytes, CLASSIC_CAN_MTU, 0),
            ParsedSocketCanFrame::Fatal(CanError::BufferOverflow)
        ));
    }

    #[test]
    fn treats_controller_tx_overflow_error_frame_as_fatal() {
        let mut data = [0u8; 8];
        data[1] = libc::CAN_ERR_CRTL_TX_OVERFLOW as u8;
        let bytes = raw_frame_bytes(libc::CAN_ERR_FLAG | libc::CAN_ERR_CRTL, 8, data);

        assert!(matches!(
            parse(&bytes, CLASSIC_CAN_MTU, 0),
            ParsedSocketCanFrame::Fatal(CanError::BufferOverflow)
        ));
    }

    #[test]
    fn treats_controller_active_error_frame_as_recoverable() {
        let mut data = [0u8; 8];
        data[1] = libc::CAN_ERR_CRTL_ACTIVE as u8;
        let bytes = raw_frame_bytes(libc::CAN_ERR_FLAG | libc::CAN_ERR_CRTL, 8, data);

        assert!(matches!(
            parse(&bytes, CLASSIC_CAN_MTU, 0),
            ParsedSocketCanFrame::RecoverableNonData
        ));
    }

    #[test]
    fn rejects_invalid_dlc() {
        let bytes = raw_frame_bytes(0x123, 9, [0; 8]);

        assert!(matches!(
            parse(&bytes, CLASSIC_CAN_MTU, 0),
            ParsedSocketCanFrame::Fatal(CanError::Frame(FrameError::InvalidDlc { dlc: 9 }))
        ));
    }

    #[test]
    fn rejects_invalid_standard_id() {
        let bytes = raw_frame_bytes(0x0800, 0, [0; 8]);

        assert!(matches!(
            parse(&bytes, CLASSIC_CAN_MTU, 0),
            ParsedSocketCanFrame::Fatal(CanError::Frame(FrameError::InvalidStandardId {
                id: 0x0800
            }))
        ));
    }

    #[test]
    fn rejects_invalid_extended_id() {
        let bytes = raw_frame_bytes(libc::CAN_EFF_FLAG | 0x2000_0000, 0, [0; 8]);

        assert!(matches!(
            parse(&bytes, CLASSIC_CAN_MTU, 0),
            ParsedSocketCanFrame::Fatal(CanError::Frame(FrameError::InvalidExtendedId {
                id: 0x2000_0000
            }))
        ));
    }

    #[test]
    fn classifies_unrecognized_error_frames_as_recoverable() {
        let bytes = raw_frame_bytes(libc::CAN_ERR_FLAG | libc::CAN_ERR_PROT, 8, [0; 8]);

        assert!(matches!(
            parse(&bytes, CLASSIC_CAN_MTU, 0),
            ParsedSocketCanFrame::RecoverableNonData
        ));
    }

    #[test]
    fn reports_invalid_frame_error_kind_for_short_classic_payload() {
        let bytes = [0u8; 8];

        let ParsedSocketCanFrame::Fatal(CanError::Device(error)) =
            parse(&bytes, CLASSIC_CAN_MTU, 0)
        else {
            panic!("expected invalid frame fatal error");
        };

        assert_eq!(error.kind, CanDeviceErrorKind::InvalidFrame);
    }
}
