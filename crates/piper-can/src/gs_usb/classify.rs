use crate::gs_usb::frame::GsUsbFrame;
use crate::gs_usb::protocol::{
    CAN_EFF_FLAG, CAN_EFF_MASK, CAN_ERR_CRTL_RX_PASSIVE, CAN_ERR_CRTL_RX_WARNING,
    CAN_ERR_CRTL_TX_PASSIVE, CAN_ERR_CRTL_TX_WARNING, CAN_ERR_FLAG, CAN_RTR_FLAG, CAN_SFF_MASK,
    GS_CAN_FLAG_OVERFLOW, GS_USB_RX_ECHO_ID,
};
use crate::{
    CanData, CanDeviceError, CanDeviceErrorKind, CanError, ExtendedCanId, FrameError, PiperFrame,
    StandardCanId,
};

const CAN_ERR_BUSOFF: u32 = 0x0000_0040;
const CAN_ERR_CRTL: u32 = 0x0000_0004;
const CAN_ERR_CRTL_RX_OVERFLOW: u8 = 0x01;
const CAN_ERR_CRTL_TX_OVERFLOW: u8 = 0x02;
const CAN_ERR_CRTL_ACTIVE: u8 = 0x40;

const GS_CAN_FLAG_FD: u8 = 1 << 1;
const GS_CAN_FLAG_BRS: u8 = 1 << 2;
const GS_CAN_FLAG_ESI: u8 = 1 << 3;
const GS_CAN_FLAG_NON_CLASSIC_MASK: u8 = GS_CAN_FLAG_FD | GS_CAN_FLAG_BRS | GS_CAN_FLAG_ESI;

#[derive(Debug)]
pub enum GsUsbFrameClass {
    ValidData(PiperFrame),
    RecoverableNonData,
    FatalMalformedData(CanError),
    FatalDeviceStatus(CanError),
    FatalTransport(CanError),
}

fn invalid_frame_error(raw: &GsUsbFrame) -> CanError {
    CanError::Device(CanDeviceError::new(
        CanDeviceErrorKind::InvalidFrame,
        format!(
            "invalid GS-USB data frame: echo_id=0x{:08X}, can_id=0x{:08X}, dlc={}, flags=0x{:02X}, reserved=0x{:02X}",
            raw.echo_id, raw.can_id, raw.can_dlc, raw.flags, raw.reserved
        ),
    ))
}

fn fatal_frame_error(error: FrameError) -> GsUsbFrameClass {
    GsUsbFrameClass::FatalMalformedData(CanError::Frame(error))
}

fn classify_error_frame(raw: &GsUsbFrame) -> GsUsbFrameClass {
    let error_class = raw.can_id & CAN_EFF_MASK;
    if (error_class & CAN_ERR_BUSOFF) != 0 {
        return GsUsbFrameClass::FatalDeviceStatus(CanError::BusOff);
    }

    if (error_class & CAN_ERR_CRTL) != 0 {
        let controller_status = raw.data[1];
        if (controller_status & (CAN_ERR_CRTL_RX_OVERFLOW | CAN_ERR_CRTL_TX_OVERFLOW)) != 0 {
            return GsUsbFrameClass::FatalDeviceStatus(CanError::BufferOverflow);
        }

        if (controller_status
            & (CAN_ERR_CRTL_ACTIVE
                | CAN_ERR_CRTL_RX_WARNING
                | CAN_ERR_CRTL_TX_WARNING
                | CAN_ERR_CRTL_RX_PASSIVE
                | CAN_ERR_CRTL_TX_PASSIVE))
            != 0
        {
            return GsUsbFrameClass::RecoverableNonData;
        }
    }

    GsUsbFrameClass::RecoverableNonData
}

pub fn classify_gs_usb_frame(raw: &GsUsbFrame) -> GsUsbFrameClass {
    if (raw.flags & GS_CAN_FLAG_OVERFLOW) != 0 {
        return GsUsbFrameClass::FatalDeviceStatus(CanError::BufferOverflow);
    }

    if (raw.can_id & CAN_ERR_FLAG) != 0 {
        return classify_error_frame(raw);
    }

    if raw.echo_id != GS_USB_RX_ECHO_ID {
        return GsUsbFrameClass::RecoverableNonData;
    }

    if (raw.can_id & CAN_RTR_FLAG) != 0 {
        return GsUsbFrameClass::RecoverableNonData;
    }

    if (raw.flags & GS_CAN_FLAG_NON_CLASSIC_MASK) != 0 {
        return GsUsbFrameClass::RecoverableNonData;
    }

    if raw.reserved != 0 || raw.flags != 0 {
        return GsUsbFrameClass::FatalMalformedData(invalid_frame_error(raw));
    }

    if raw.can_dlc > 8 {
        return fatal_frame_error(FrameError::InvalidDlc { dlc: raw.can_dlc });
    }

    let data = match CanData::from_padded(raw.data, raw.can_dlc) {
        Ok(data) => data,
        Err(error) => return fatal_frame_error(error),
    };

    let frame = if (raw.can_id & CAN_EFF_FLAG) != 0 {
        let id = match ExtendedCanId::new(raw.can_id & CAN_EFF_MASK) {
            Ok(id) => id,
            Err(error) => return fatal_frame_error(error),
        };
        PiperFrame::extended(id, data)
    } else {
        let invalid_standard_bits = raw.can_id & !CAN_SFF_MASK;
        if invalid_standard_bits != 0 {
            return fatal_frame_error(FrameError::InvalidStandardId { id: raw.can_id });
        }

        let id = match StandardCanId::new(raw.can_id & CAN_SFF_MASK) {
            Ok(id) => id,
            Err(error) => return fatal_frame_error(error),
        };
        PiperFrame::standard(id, data)
    };

    GsUsbFrameClass::ValidData(frame.with_timestamp_us(raw.timestamp_us as u64))
}

pub fn parse_gs_usb_batch(raw: &[GsUsbFrame]) -> Result<Vec<PiperFrame>, CanError> {
    let mut parsed = Vec::with_capacity(raw.len());
    for frame in raw {
        match classify_gs_usb_frame(frame) {
            GsUsbFrameClass::ValidData(frame) => parsed.push(frame),
            GsUsbFrameClass::RecoverableNonData => {},
            GsUsbFrameClass::FatalMalformedData(error)
            | GsUsbFrameClass::FatalDeviceStatus(error)
            | GsUsbFrameClass::FatalTransport(error) => return Err(error),
        }
    }

    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::{GsUsbFrameClass, classify_gs_usb_frame, parse_gs_usb_batch};
    use crate::CanError;
    use crate::FrameError;
    use crate::gs_usb::frame::GsUsbFrame;
    use crate::gs_usb::protocol::{
        CAN_EFF_FLAG, CAN_ERR_CRTL_RX_PASSIVE, CAN_ERR_CRTL_RX_WARNING, CAN_ERR_CRTL_TX_PASSIVE,
        CAN_ERR_CRTL_TX_WARNING, CAN_ERR_FLAG, CAN_RTR_FLAG, GS_CAN_FLAG_OVERFLOW, GS_USB_ECHO_ID,
        GS_USB_RX_ECHO_ID,
    };

    const CAN_ERR_BUSOFF: u32 = 0x0000_0040;
    const CAN_ERR_CRTL: u32 = 0x0000_0004;
    const CAN_ERR_CRTL_ACTIVE: u8 = 0x40;
    const CAN_ERR_CRTL_RX_OVERFLOW: u8 = 0x01;
    const GS_CAN_FLAG_FD: u8 = 1 << 1;

    fn valid_frame(can_id: u32) -> GsUsbFrame {
        GsUsbFrame {
            echo_id: GS_USB_RX_ECHO_ID,
            can_id,
            can_dlc: 8,
            channel: 0,
            flags: 0,
            reserved: 0,
            data: [0xA5; 8],
            timestamp_us: 42,
        }
    }

    fn recoverable_echo_frame() -> GsUsbFrame {
        GsUsbFrame {
            echo_id: GS_USB_ECHO_ID,
            ..valid_frame(0x123)
        }
    }

    fn malformed_dlc_frame(dlc: u8) -> GsUsbFrame {
        GsUsbFrame {
            can_dlc: dlc,
            ..valid_frame(0x123)
        }
    }

    fn error_frame(error_class: u32, controller_status: u8) -> GsUsbFrame {
        let mut frame = valid_frame(CAN_ERR_FLAG | error_class);
        frame.data[1] = controller_status;
        frame
    }

    fn assert_recoverable(frame: GsUsbFrame) {
        assert!(matches!(
            classify_gs_usb_frame(&frame),
            GsUsbFrameClass::RecoverableNonData
        ));
    }

    #[test]
    fn batch_recoverable_frame_is_skipped_between_valid_frames() {
        let batch = vec![
            valid_frame(0x100),
            recoverable_echo_frame(),
            valid_frame(0x101),
        ];
        let parsed = parse_gs_usb_batch(&batch).unwrap();
        assert_eq!(
            parsed.iter().map(|f| f.raw_id()).collect::<Vec<_>>(),
            vec![0x100, 0x101]
        );
    }

    #[test]
    fn batch_malformed_frame_discards_whole_batch() {
        let batch = vec![
            valid_frame(0x100),
            malformed_dlc_frame(9),
            valid_frame(0x101),
        ];
        assert!(parse_gs_usb_batch(&batch).is_err());
    }

    #[test]
    fn reserved_byte_on_data_frame_is_fatal_malformed() {
        let frame = GsUsbFrame {
            reserved: 1,
            ..valid_frame(0x123)
        };

        assert!(matches!(
            classify_gs_usb_frame(&frame),
            GsUsbFrameClass::FatalMalformedData(_)
        ));
    }

    #[test]
    fn unsupported_nonzero_flags_on_data_frame_are_fatal_malformed() {
        let frame = GsUsbFrame {
            flags: 0x80,
            ..valid_frame(0x123)
        };

        assert!(matches!(
            classify_gs_usb_frame(&frame),
            GsUsbFrameClass::FatalMalformedData(_)
        ));
    }

    #[test]
    fn explicit_control_status_and_fd_frames_are_recoverable_non_data() {
        assert_recoverable(recoverable_echo_frame());
        assert_recoverable(GsUsbFrame {
            can_id: CAN_RTR_FLAG | 0x123,
            ..valid_frame(0x123)
        });
        assert_recoverable(GsUsbFrame {
            flags: GS_CAN_FLAG_FD,
            ..valid_frame(0x123)
        });
    }

    #[test]
    fn fatal_device_status_wins_over_non_data_classification() {
        assert!(matches!(
            classify_gs_usb_frame(&GsUsbFrame {
                echo_id: GS_USB_ECHO_ID,
                flags: GS_CAN_FLAG_OVERFLOW,
                ..valid_frame(0x123)
            }),
            GsUsbFrameClass::FatalDeviceStatus(CanError::BufferOverflow)
        ));
    }

    #[test]
    fn linux_can_err_bus_off_is_fatal_bus_off() {
        assert!(matches!(
            classify_gs_usb_frame(&error_frame(CAN_ERR_BUSOFF, 0)),
            GsUsbFrameClass::FatalDeviceStatus(CanError::BusOff)
        ));
    }

    #[test]
    fn linux_can_err_controller_overflow_is_fatal_buffer_overflow() {
        assert!(matches!(
            classify_gs_usb_frame(&error_frame(CAN_ERR_CRTL, CAN_ERR_CRTL_RX_OVERFLOW)),
            GsUsbFrameClass::FatalDeviceStatus(CanError::BufferOverflow)
        ));
    }

    #[test]
    fn linux_can_err_controller_active_warning_and_passive_are_recoverable() {
        for status in [
            CAN_ERR_CRTL_ACTIVE,
            CAN_ERR_CRTL_RX_WARNING,
            CAN_ERR_CRTL_TX_WARNING,
            CAN_ERR_CRTL_RX_PASSIVE,
            CAN_ERR_CRTL_TX_PASSIVE,
        ] {
            assert_recoverable(error_frame(CAN_ERR_CRTL, status));
        }
    }

    #[test]
    fn invalid_dlc_is_fatal_frame_error() {
        assert!(matches!(
            classify_gs_usb_frame(&malformed_dlc_frame(9)),
            GsUsbFrameClass::FatalMalformedData(CanError::Frame(FrameError::InvalidDlc { dlc: 9 }))
        ));
    }

    #[test]
    fn valid_extended_frame_preserves_id_data_and_timestamp() {
        let frame = GsUsbFrame {
            can_id: CAN_EFF_FLAG | 0x01AB_CDEF,
            can_dlc: 4,
            data: [1, 2, 3, 4, 0xAA, 0xBB, 0xCC, 0xDD],
            timestamp_us: 123,
            ..valid_frame(0)
        };

        let GsUsbFrameClass::ValidData(parsed) = classify_gs_usb_frame(&frame) else {
            panic!("expected valid data frame");
        };

        assert_eq!(parsed.raw_id(), 0x01AB_CDEF);
        assert!(parsed.is_extended());
        assert_eq!(parsed.dlc(), 4);
        assert_eq!(parsed.data(), &[1, 2, 3, 4]);
        assert_eq!(parsed.data_padded(), &[1, 2, 3, 4, 0, 0, 0, 0]);
        assert_eq!(parsed.timestamp_us(), 123);
    }

    #[test]
    fn fatal_transport_variant_is_available_for_receive_surfaces() {
        assert!(matches!(
            GsUsbFrameClass::FatalTransport(CanError::Timeout),
            GsUsbFrameClass::FatalTransport(CanError::Timeout)
        ));
    }
}
