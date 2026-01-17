//! GS-USB CAN 2.0 帧编码/解码
//!
//! 支持经典 CAN（8 字节数据）的帧格式编码和解码

use crate::can::gs_usb::error::GsUsbError;
use crate::can::gs_usb::protocol::*;
use bytes::{Buf, BufMut, Bytes, BytesMut};

/// GS-USB CAN 2.0 帧（不支持 CAN FD）
///
/// **注意**：此结构体不使用 `#[repr(packed)]`，因为我们完全使用 `bytes` 库手动打包/解包
/// （`pack_to` 和 `unpack_from_bytes`），不依赖结构体的内存布局。
///
/// 手动打包/解包的优势：
/// - 不依赖编译器对齐规则
/// - 避免 `packed` 带来的性能问题（未对齐访问）
/// - 代码更清晰，明确控制字节流格式
#[derive(Debug, Clone, Copy, Default)]
pub struct GsUsbFrame {
    pub echo_id: u32, // 0 = TX, 0xFFFFFFFF = RX
    pub can_id: u32,  // CAN ID（带 EFF/RTR 标志）
    pub can_dlc: u8,  // Data Length Code (0-8)
    pub channel: u8,  // CAN 通道号
    pub flags: u8,    // GS-USB 标志（OVERFLOW 等）
    pub reserved: u8,
    pub data: [u8; 8], // 固定 8 字节（CAN 2.0）
    /// Hardware timestamp in microseconds (0 if not available)
    pub timestamp_us: u32,
}

impl GsUsbFrame {
    /// 创建新的空帧
    pub fn new() -> Self {
        Self {
            echo_id: GS_USB_ECHO_ID,
            can_id: 0,
            can_dlc: 0,
            channel: 0,
            flags: 0,
            reserved: 0,
            data: [0u8; 8],
            timestamp_us: 0,
        }
    }

    /// Pack frame into BytesMut
    ///
    /// # Arguments
    /// * `buf` - Buffer to pack into
    /// * `hw_timestamp` - If true, include hardware timestamp field (frame size = 24 bytes, otherwise 20 bytes)
    pub fn pack_to(&self, buf: &mut BytesMut, hw_timestamp: bool) {
        let frame_size = if hw_timestamp {
            GS_USB_FRAME_SIZE_HW_TIMESTAMP
        } else {
            GS_USB_FRAME_SIZE
        };
        buf.reserve(frame_size);
        buf.put_u32_le(self.echo_id);
        buf.put_u32_le(self.can_id);
        buf.put_u8(self.can_dlc);
        buf.put_u8(self.channel);
        buf.put_u8(self.flags);
        buf.put_u8(self.reserved);
        buf.put_slice(&self.data);

        // Optional hardware timestamp
        if hw_timestamp {
            buf.put_u32_le(self.timestamp_us);
        }
    }

    /// Unpack from Bytes
    ///
    /// # Arguments
    /// * `data` - Bytes to unpack from
    /// * `hw_timestamp` - If true, expect hardware timestamp field (frame size = 24 bytes, otherwise 20 bytes)
    pub fn unpack_from_bytes(
        &mut self,
        mut data: Bytes,
        hw_timestamp: bool,
    ) -> Result<(), GsUsbError> {
        let min_size = if hw_timestamp {
            GS_USB_FRAME_SIZE_HW_TIMESTAMP
        } else {
            GS_USB_FRAME_SIZE
        };

        if data.len() < min_size {
            return Err(GsUsbError::InvalidFrame(format!(
                "Frame too short: {} bytes (expected at least {})",
                data.len(),
                min_size
            )));
        }

        self.echo_id = data.get_u32_le();
        self.can_id = data.get_u32_le();
        self.can_dlc = data.get_u8();
        self.channel = data.get_u8();
        self.flags = data.get_u8();
        self.reserved = data.get_u8();

        data.copy_to_slice(&mut self.data);

        // Optional hardware timestamp
        if hw_timestamp {
            // 前面已经检查过 data.len() >= GS_USB_FRAME_SIZE_HW_TIMESTAMP (24)
            // 所以这里 data.len() 一定 >= 4
            self.timestamp_us = data.get_u32_le();
        } else {
            self.timestamp_us = 0;
        }

        Ok(())
    }

    /// Check if this is an RX frame (from CAN bus)
    pub fn is_rx_frame(&self) -> bool {
        self.echo_id == GS_USB_RX_ECHO_ID
    }

    /// Check if this is a TX echo (confirmation)
    pub fn is_tx_echo(&self) -> bool {
        self.echo_id != GS_USB_RX_ECHO_ID
    }

    /// Check for buffer overflow
    pub fn has_overflow(&self) -> bool {
        (self.flags & GS_CAN_FLAG_OVERFLOW) != 0
    }
}

#[cfg(test)]
mod tests {
    use super::GsUsbFrame;
    use crate::can::gs_usb::protocol::*;
    use bytes::BytesMut;

    #[test]
    fn test_frame_pack_to() {
        let frame = GsUsbFrame {
            echo_id: GS_USB_ECHO_ID,
            can_id: 0x123,
            can_dlc: 4,
            channel: 0,
            flags: 0,
            reserved: 0,
            data: [0x01, 0x02, 0x03, 0x04, 0, 0, 0, 0],
            timestamp_us: 0,
        };

        let mut buf = BytesMut::new();
        frame.pack_to(&mut buf, false);

        assert_eq!(buf.len(), GS_USB_FRAME_SIZE);

        // 验证 Header
        assert_eq!(buf[0..4], [0, 0, 0, 0]); // echo_id
        assert_eq!(buf[4..8], [0x23, 0x01, 0, 0]); // can_id (little-endian)
        assert_eq!(buf[8], 4); // can_dlc
        assert_eq!(buf[9], 0); // channel
        assert_eq!(buf[10], 0); // flags
        assert_eq!(buf[11], 0); // reserved

        // 验证 Data
        assert_eq!(buf[12..16], [0x01, 0x02, 0x03, 0x04]);
    }

    #[test]
    fn test_frame_pack_extended_id() {
        let frame = GsUsbFrame {
            echo_id: GS_USB_ECHO_ID,
            can_id: 0x12345678 | CAN_EFF_FLAG, // 扩展帧
            can_dlc: 8,
            channel: 0,
            flags: 0,
            reserved: 0,
            data: [0xFF; 8],
            timestamp_us: 0,
        };

        let mut buf = BytesMut::new();
        frame.pack_to(&mut buf, false);

        // 验证扩展 ID（包含 EFF_FLAG）
        let can_id = u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]);
        assert_eq!(can_id & CAN_EFF_FLAG, CAN_EFF_FLAG);
        assert_eq!(can_id & CAN_EFF_MASK, 0x12345678);
    }

    #[test]
    fn test_frame_unpack_from_bytes() {
        use bytes::Bytes;

        // 构造测试数据
        let mut data = vec![0u8; GS_USB_FRAME_SIZE];

        // echo_id = 0xFFFF_FFFF (RX 帧)
        data[0..4].copy_from_slice(&GS_USB_RX_ECHO_ID.to_le_bytes());
        // can_id = 0x123
        data[4..8].copy_from_slice(&0x123u32.to_le_bytes());
        data[8] = 4; // can_dlc
        data[9] = 0; // channel
        data[10] = 0; // flags
        data[11] = 0; // reserved
        data[12..16].copy_from_slice(&[0x01, 0x02, 0x03, 0x04]);

        let mut frame = GsUsbFrame::default();
        frame.unpack_from_bytes(Bytes::from(data), false).unwrap();

        assert_eq!(frame.echo_id, GS_USB_RX_ECHO_ID);
        assert_eq!(frame.can_id, 0x123);
        assert_eq!(frame.can_dlc, 4);
        assert_eq!(frame.data[..4], [0x01, 0x02, 0x03, 0x04]);
        assert!(frame.is_rx_frame());
    }

    #[test]
    fn test_frame_unpack_too_short() {
        use bytes::Bytes;

        let mut frame = GsUsbFrame::default();
        let data = Bytes::from(vec![0u8; 10]); // 太短

        let result = frame.unpack_from_bytes(data, false);
        assert!(result.is_err());
    }

    #[test]
    fn test_frame_is_tx_echo() {
        let frame = GsUsbFrame {
            echo_id: 0x1234, // 非 RX_ECHO_ID
            ..Default::default()
        };

        assert!(frame.is_tx_echo());
        assert!(!frame.is_rx_frame());
    }

    #[test]
    fn test_frame_has_overflow() {
        let frame = GsUsbFrame {
            flags: GS_CAN_FLAG_OVERFLOW,
            ..Default::default()
        };

        assert!(frame.has_overflow());
    }

    #[test]
    fn test_frame_roundtrip() {
        let original = GsUsbFrame {
            echo_id: GS_USB_RX_ECHO_ID,
            can_id: 0x12345678 | CAN_EFF_FLAG,
            can_dlc: 6,
            channel: 1,
            flags: 0,
            reserved: 0,
            data: [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF, 0, 0],
            timestamp_us: 0,
        };

        let mut buf = BytesMut::new();
        original.pack_to(&mut buf, false);

        let mut unpacked = GsUsbFrame::default();
        unpacked.unpack_from_bytes(buf.freeze(), false).unwrap();

        assert_eq!(original.echo_id, unpacked.echo_id);
        assert_eq!(original.can_id, unpacked.can_id);
        assert_eq!(original.can_dlc, unpacked.can_dlc);
        assert_eq!(original.data, unpacked.data);
    }

    #[test]
    fn test_frame_pack_with_hw_timestamp() {
        let frame = GsUsbFrame {
            echo_id: GS_USB_RX_ECHO_ID,
            can_id: 0x123,
            can_dlc: 4,
            channel: 0,
            flags: 0,
            reserved: 0,
            data: [0x01, 0x02, 0x03, 0x04, 0, 0, 0, 0],
            timestamp_us: 12345678,
        };

        let mut buf = BytesMut::new();
        frame.pack_to(&mut buf, true);

        assert_eq!(buf.len(), GS_USB_FRAME_SIZE_HW_TIMESTAMP);

        // 验证时间戳字段（最后 4 字节）
        let timestamp = u32::from_le_bytes([buf[20], buf[21], buf[22], buf[23]]);
        assert_eq!(timestamp, 12345678);
    }

    #[test]
    fn test_frame_unpack_with_hw_timestamp() {
        use bytes::Bytes;

        let mut data = vec![0u8; GS_USB_FRAME_SIZE_HW_TIMESTAMP];

        // echo_id = 0xFFFF_FFFF (RX 帧)
        data[0..4].copy_from_slice(&GS_USB_RX_ECHO_ID.to_le_bytes());
        // can_id = 0x123
        data[4..8].copy_from_slice(&0x123u32.to_le_bytes());
        data[8] = 4; // can_dlc
        data[9] = 0; // channel
        data[10] = 0; // flags
        data[11] = 0; // reserved
        data[12..16].copy_from_slice(&[0x01, 0x02, 0x03, 0x04]);
        // timestamp = 12345678
        data[20..24].copy_from_slice(&12345678u32.to_le_bytes());

        let mut frame = GsUsbFrame::default();
        frame.unpack_from_bytes(Bytes::from(data), true).unwrap();

        assert_eq!(frame.echo_id, GS_USB_RX_ECHO_ID);
        assert_eq!(frame.can_id, 0x123);
        assert_eq!(frame.can_dlc, 4);
        assert_eq!(frame.data[..4], [0x01, 0x02, 0x03, 0x04]);
        assert_eq!(frame.timestamp_us, 12345678);
    }

    #[test]
    fn test_frame_unpack_hw_timestamp_too_short() {
        use bytes::Bytes;

        let mut frame = GsUsbFrame::default();
        // 只有 20 字节（无时间戳），但要求 24 字节（有时间戳）
        let data = Bytes::from(vec![0u8; GS_USB_FRAME_SIZE]);

        let result = frame.unpack_from_bytes(data, true);
        assert!(result.is_err());
    }

    #[test]
    fn test_frame_pack_roundtrip_with_timestamp() {
        let original = GsUsbFrame {
            echo_id: GS_USB_RX_ECHO_ID,
            can_id: 0x12345678 | CAN_EFF_FLAG,
            can_dlc: 6,
            channel: 1,
            flags: GS_CAN_FLAG_OVERFLOW,
            reserved: 0xAB,
            data: [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF, 0, 0],
            timestamp_us: 98765432,
        };

        // 打包（带时间戳）
        let mut buf = BytesMut::new();
        original.pack_to(&mut buf, true);
        assert_eq!(buf.len(), GS_USB_FRAME_SIZE_HW_TIMESTAMP);

        // 解包（带时间戳）
        let mut unpacked = GsUsbFrame::default();
        unpacked.unpack_from_bytes(buf.freeze(), true).unwrap();

        assert_eq!(original.echo_id, unpacked.echo_id);
        assert_eq!(original.can_id, unpacked.can_id);
        assert_eq!(original.can_dlc, unpacked.can_dlc);
        assert_eq!(original.channel, unpacked.channel);
        assert_eq!(original.flags, unpacked.flags);
        assert_eq!(original.reserved, unpacked.reserved);
        assert_eq!(original.data, unpacked.data);
        assert_eq!(original.timestamp_us, unpacked.timestamp_us); // 时间戳也匹配
    }

    #[test]
    fn test_frame_pack_without_timestamp() {
        let frame = GsUsbFrame {
            timestamp_us: 99999999, // 即使有时间戳值
            ..Default::default()
        };

        let mut buf = BytesMut::new();
        frame.pack_to(&mut buf, false); // 不包含时间戳

        assert_eq!(buf.len(), GS_USB_FRAME_SIZE); // 只有 20 字节
    }

    #[test]
    fn test_frame_unpack_without_timestamp_clears_timestamp() {
        use bytes::Bytes;

        let data = vec![0u8; GS_USB_FRAME_SIZE];
        // 只包含 20 字节的标准帧

        let mut frame = GsUsbFrame {
            timestamp_us: 12345, // 预设值
            ..Default::default()
        };

        frame.unpack_from_bytes(Bytes::from(data), false).unwrap();
        assert_eq!(frame.timestamp_us, 0); // 应该被清除
    }

    #[test]
    fn test_frame_is_rx_frame() {
        let rx_frame = GsUsbFrame {
            echo_id: GS_USB_RX_ECHO_ID,
            ..Default::default()
        };
        assert!(rx_frame.is_rx_frame());
        assert!(!rx_frame.is_tx_echo());

        let tx_frame = GsUsbFrame {
            echo_id: GS_USB_ECHO_ID,
            ..Default::default()
        };
        assert!(!tx_frame.is_rx_frame());
        assert!(tx_frame.is_tx_echo());
    }

    #[test]
    fn test_frame_overflow_flag() {
        let frame_with_overflow = GsUsbFrame {
            flags: GS_CAN_FLAG_OVERFLOW,
            ..Default::default()
        };
        assert!(frame_with_overflow.has_overflow());

        let frame_without_overflow = GsUsbFrame {
            flags: 0,
            ..Default::default()
        };
        assert!(!frame_without_overflow.has_overflow());

        let frame_with_other_flags = GsUsbFrame {
            flags: 0x02, // 其他标志位
            ..Default::default()
        };
        assert!(!frame_with_other_flags.has_overflow());
    }

    #[test]
    fn test_frame_default() {
        let frame = GsUsbFrame::default();
        assert_eq!(frame.echo_id, GS_USB_ECHO_ID);
        assert_eq!(frame.can_id, 0);
        assert_eq!(frame.can_dlc, 0);
        assert_eq!(frame.channel, 0);
        assert_eq!(frame.flags, 0);
        assert_eq!(frame.reserved, 0);
        assert_eq!(frame.data, [0u8; 8]);
        assert_eq!(frame.timestamp_us, 0);
    }

    #[test]
    fn test_frame_new() {
        let frame = GsUsbFrame::new();
        assert_eq!(frame.echo_id, GS_USB_ECHO_ID);
        assert_eq!(frame.can_id, 0);
        assert_eq!(frame.can_dlc, 0);
        assert_eq!(frame.timestamp_us, 0);
    }
}
