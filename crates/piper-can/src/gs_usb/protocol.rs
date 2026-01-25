//! GS-USB 协议定义
//!
//! 包含协议常量和结构体定义（只支持 CAN 2.0，不支持 CAN FD）

// ============================================================================
// Control Request Codes
// ============================================================================

/// 协议握手 + 字节序配置（历史接口）
///
/// 说明：
/// - 大多数现代固件默认按 little-endian 工作，且默认实现通常**不发送**该请求。
/// - 本 SDK 当前默认路径也**不依赖**该请求；如未来遇到特定固件兼容性问题，再考虑引入显式开关。
pub const GS_USB_BREQ_HOST_FORMAT: u8 = 0;
/// Set bit timing
pub const GS_USB_BREQ_BITTIMING: u8 = 1;
/// Set/start mode
pub const GS_USB_BREQ_MODE: u8 = 2;
/// Get bus errors
pub const GS_USB_BREQ_BERR: u8 = 3;
/// Get bit timing constants
pub const GS_USB_BREQ_BT_CONST: u8 = 4;
/// Get device configuration
pub const GS_USB_BREQ_DEVICE_CONFIG: u8 = 5;

// ============================================================================
// GS-USB Mode Flags (used in DeviceMode.flags)
// ============================================================================

/// Normal operation mode
pub const GS_CAN_MODE_NORMAL: u32 = 0;
/// Listen-only mode (no ACKs sent)
pub const GS_CAN_MODE_LISTEN_ONLY: u32 = 1 << 0;
/// Loopback mode (for testing)
pub const GS_CAN_MODE_LOOP_BACK: u32 = 1 << 1;
/// Triple sample mode
pub const GS_CAN_MODE_TRIPLE_SAMPLE: u32 = 1 << 2;
/// One-shot mode (no retransmission)
pub const GS_CAN_MODE_ONE_SHOT: u32 = 1 << 3;
/// Hardware timestamp mode
pub const GS_CAN_MODE_HW_TIMESTAMP: u32 = 1 << 4;

// ============================================================================
// GS-USB Mode Values
// ============================================================================

/// Reset/stop mode
pub const GS_CAN_MODE_RESET: u32 = 0;
/// Start mode
pub const GS_CAN_MODE_START: u32 = 1;

// ============================================================================
// CAN ID Flags (in CAN frame identifier)
// ============================================================================

/// Extended frame format flag (29-bit ID)
pub const CAN_EFF_FLAG: u32 = 0x8000_0000;
/// Remote transmission request flag
pub const CAN_RTR_FLAG: u32 = 0x4000_0000;
/// Error message frame flag
pub const CAN_ERR_FLAG: u32 = 0x2000_0000;

// ============================================================================
// CAN ID Masks
// ============================================================================

/// Standard frame format mask (11-bit ID)
pub const CAN_SFF_MASK: u32 = 0x0000_07FF;
/// Extended frame format mask (29-bit ID)
pub const CAN_EFF_MASK: u32 = 0x1FFF_FFFF;
/// Error mask (omit EFF, RTR, ERR flags)
pub const CAN_ERR_MASK: u32 = 0x1FFF_FFFF;

// ============================================================================
// Frame Constants
// ============================================================================

/// Echo ID for transmitted frames
pub const GS_USB_ECHO_ID: u32 = 0;
/// Echo ID value for received frames (from CAN bus)
pub const GS_USB_RX_ECHO_ID: u32 = 0xFFFF_FFFF;

/// Maximum data length for classic CAN
pub const CAN_MAX_DLEN: usize = 8;

/// Classic CAN frame size (without timestamp)
pub const GS_USB_FRAME_SIZE: usize = 20;
/// Classic CAN frame size (with hardware timestamp)
pub const GS_USB_FRAME_SIZE_HW_TIMESTAMP: usize = 24; // 20 + 4 (timestamp)

// ============================================================================
// GS-USB Frame Flags (in gs_host_frame.flags field)
// ============================================================================

/// RX overflow occurred
pub const GS_CAN_FLAG_OVERFLOW: u8 = 1 << 0;

// ============================================================================
// CAN Error Frame Constants (Linux can.h compatible)
// ============================================================================

/// CAN Error Frame - Controller Error Status (`data[1]`)
/// These flags indicate the controller error state
pub const CAN_ERR_CRTL_RX_WARNING: u8 = 0x04; // RX Error Warning (REC > 96)
pub const CAN_ERR_CRTL_TX_WARNING: u8 = 0x08; // TX Error Warning (TEC > 96)
pub const CAN_ERR_CRTL_RX_PASSIVE: u8 = 0x10; // RX Error Passive (REC > 127)
pub const CAN_ERR_CRTL_TX_PASSIVE: u8 = 0x20; // TX Error Passive (TEC > 127)
pub const CAN_ERR_CRTL_TX_BUS_OFF: u8 = 0x40; // TX Bus Off (TEC > 255)
pub const CAN_ERR_CRTL_RX_BUS_OFF: u8 = 0x80; // RX Bus Off (rare, some controllers)

/// CAN Error Frame - Protocol Error Type (`data[2]`)
/// These flags indicate the type of protocol error
pub const CAN_ERR_PROT_BIT: u8 = 0x01; // Single bit error
pub const CAN_ERR_PROT_FORM: u8 = 0x02; // Format error (e.g., bitrate mismatch)
pub const CAN_ERR_PROT_STUFF: u8 = 0x04; // Stuff error
pub const CAN_ERR_PROT_BIT0: u8 = 0x08; // Unable to send dominant bit
pub const CAN_ERR_PROT_BIT1: u8 = 0x10; // Unable to send recessive bit
pub const CAN_ERR_PROT_OVERLOAD: u8 = 0x20; // Bus overload
pub const CAN_ERR_PROT_ACTIVE: u8 = 0x40; // Active error flag
pub const CAN_ERR_PROT_TX: u8 = 0x80; // Transmitted error flag

// ============================================================================
// USB Endpoints
// ============================================================================

/// Bulk OUT endpoint (host to device)
pub const GS_USB_ENDPOINT_OUT: u8 = 0x02;
/// Bulk IN endpoint (device to host)
pub const GS_USB_ENDPOINT_IN: u8 = 0x81;

// ============================================================================
// USB Request Types
// ============================================================================

/// USB Control Transfer: Host to Device | Vendor | Interface
pub const GS_USB_REQ_OUT: u8 = 0x41;
/// USB Control Transfer: Device to Host | Vendor | Interface
pub const GS_USB_REQ_IN: u8 = 0xC1;

// ============================================================================
// Protocol Structures
// ============================================================================

/// CAN 位定时配置
#[derive(Debug, Clone, Copy)]
pub struct DeviceBitTiming {
    pub prop_seg: u32,
    pub phase_seg1: u32,
    pub phase_seg2: u32,
    pub sjw: u32,
    pub brp: u32,
}

impl DeviceBitTiming {
    pub fn new(prop_seg: u32, phase_seg1: u32, phase_seg2: u32, sjw: u32, brp: u32) -> Self {
        Self {
            prop_seg,
            phase_seg1,
            phase_seg2,
            sjw,
            brp,
        }
    }

    /// Pack into bytes for USB transfer (20 bytes)
    pub fn pack(&self) -> [u8; 20] {
        let mut buf = [0u8; 20];
        buf[0..4].copy_from_slice(&self.prop_seg.to_le_bytes());
        buf[4..8].copy_from_slice(&self.phase_seg1.to_le_bytes());
        buf[8..12].copy_from_slice(&self.phase_seg2.to_le_bytes());
        buf[12..16].copy_from_slice(&self.sjw.to_le_bytes());
        buf[16..20].copy_from_slice(&self.brp.to_le_bytes());
        buf
    }
}

/// 设备模式配置
#[derive(Debug, Clone, Copy)]
pub struct DeviceMode {
    pub mode: u32,  // GS_CAN_MODE_START or GS_CAN_MODE_RESET
    pub flags: u32, // Mode flags
}

impl DeviceMode {
    pub fn new(mode: u32, flags: u32) -> Self {
        Self { mode, flags }
    }

    /// Pack into bytes for USB transfer (8 bytes)
    pub fn pack(&self) -> [u8; 8] {
        let mut buf = [0u8; 8];
        buf[0..4].copy_from_slice(&self.mode.to_le_bytes());
        buf[4..8].copy_from_slice(&self.flags.to_le_bytes());
        buf
    }
}

/// 设备能力（位定时约束和功能标志）
#[derive(Debug, Clone, Copy)]
pub struct DeviceCapability {
    pub feature: u32,  // 功能标志位
    pub fclk_can: u32, // CAN 时钟频率（Hz）
    pub tseg1_min: u32,
    pub tseg1_max: u32,
    pub tseg2_min: u32,
    pub tseg2_max: u32,
    pub sjw_max: u32,
    pub brp_min: u32,
    pub brp_max: u32,
    pub brp_inc: u32,
}

impl DeviceCapability {
    /// Unpack from BT_CONST response (40 bytes)
    pub fn unpack(data: &[u8]) -> Self {
        Self {
            feature: u32::from_le_bytes([data[0], data[1], data[2], data[3]]),
            fclk_can: u32::from_le_bytes([data[4], data[5], data[6], data[7]]),
            tseg1_min: u32::from_le_bytes([data[8], data[9], data[10], data[11]]),
            tseg1_max: u32::from_le_bytes([data[12], data[13], data[14], data[15]]),
            tseg2_min: u32::from_le_bytes([data[16], data[17], data[18], data[19]]),
            tseg2_max: u32::from_le_bytes([data[20], data[21], data[22], data[23]]),
            sjw_max: u32::from_le_bytes([data[24], data[25], data[26], data[27]]),
            brp_min: u32::from_le_bytes([data[28], data[29], data[30], data[31]]),
            brp_max: u32::from_le_bytes([data[32], data[33], data[34], data[35]]),
            brp_inc: u32::from_le_bytes([data[36], data[37], data[38], data[39]]),
        }
    }
}

/// 设备信息（固件版本、通道数等）
#[derive(Debug, Clone, Copy)]
pub struct DeviceInfo {
    pub icount: u8,      // 通道数 - 1
    pub fw_version: u32, // 固件版本（实际版本 = fw_version / 10）
    pub hw_version: u32, // 硬件版本（实际版本 = hw_version / 10）
}

impl DeviceInfo {
    pub fn unpack(data: &[u8]) -> Self {
        Self {
            icount: data[3],
            fw_version: u32::from_le_bytes([data[4], data[5], data[6], data[7]]),
            hw_version: u32::from_le_bytes([data[8], data[9], data[10], data[11]]),
        }
    }

    pub fn channel_count(&self) -> u8 {
        self.icount + 1
    }

    /// Get firmware version as a float
    pub fn firmware_version(&self) -> f32 {
        self.fw_version as f32 / 10.0
    }

    /// Get hardware version as a float
    pub fn hardware_version(&self) -> f32 {
        self.hw_version as f32 / 10.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_control_request_codes() {
        assert_eq!(GS_USB_BREQ_HOST_FORMAT, 0);
        assert_eq!(GS_USB_BREQ_BITTIMING, 1);
        assert_eq!(GS_USB_BREQ_MODE, 2);
        assert_eq!(GS_USB_BREQ_BT_CONST, 4);
        assert_eq!(GS_USB_BREQ_DEVICE_CONFIG, 5);
    }

    #[test]
    fn test_mode_flags() {
        assert_eq!(GS_CAN_MODE_NORMAL, 0);
        assert_eq!(GS_CAN_MODE_LISTEN_ONLY, 1 << 0);
        assert_eq!(GS_CAN_MODE_LOOP_BACK, 1 << 1);
        assert_eq!(GS_CAN_MODE_TRIPLE_SAMPLE, 1 << 2);
        assert_eq!(GS_CAN_MODE_ONE_SHOT, 1 << 3);
    }

    #[test]
    fn test_frame_constants() {
        assert_eq!(GS_USB_ECHO_ID, 0);
        assert_eq!(GS_USB_RX_ECHO_ID, 0xFFFF_FFFF);
        assert_eq!(GS_USB_FRAME_SIZE, 20);
    }

    #[test]
    fn test_endpoints() {
        assert_eq!(GS_USB_ENDPOINT_OUT, 0x02);
        assert_eq!(GS_USB_ENDPOINT_IN, 0x81);
    }

    #[test]
    fn test_can_id_flags_and_masks() {
        assert_eq!(CAN_EFF_FLAG, 0x8000_0000);
        assert_eq!(CAN_RTR_FLAG, 0x4000_0000);
        assert_eq!(CAN_EFF_MASK, 0x1FFF_FFFF);
        assert_eq!(CAN_SFF_MASK, 0x0000_07FF);
    }

    #[test]
    fn test_device_bit_timing_pack() {
        let timing = DeviceBitTiming::new(1, 12, 2, 1, 6);
        let packed = timing.pack();

        // 验证 little-endian 编码
        assert_eq!(packed[0..4], [1, 0, 0, 0]); // prop_seg
        assert_eq!(packed[4..8], [12, 0, 0, 0]); // phase_seg1
        assert_eq!(packed[8..12], [2, 0, 0, 0]); // phase_seg2
        assert_eq!(packed[12..16], [1, 0, 0, 0]); // sjw
        assert_eq!(packed[16..20], [6, 0, 0, 0]); // brp

        assert_eq!(packed.len(), 20);
    }

    #[test]
    fn test_device_mode_pack() {
        let mode = DeviceMode::new(GS_CAN_MODE_START, GS_CAN_MODE_NORMAL);
        let packed = mode.pack();

        // 验证 little-endian 编码
        assert_eq!(packed[0..4], [1, 0, 0, 0]); // mode = START
        assert_eq!(packed[4..8], [0, 0, 0, 0]); // flags = NORMAL
        assert_eq!(packed.len(), 8);
    }

    #[test]
    fn test_device_capability_unpack() {
        // 构造测试数据（40 字节，10 x u32）
        let mut data = vec![0u8; 40];

        // feature = 0x00000001
        data[0..4].copy_from_slice(&1u32.to_le_bytes());
        // fclk_can = 80_000_000
        data[4..8].copy_from_slice(&80_000_000u32.to_le_bytes());
        // tseg1_min = 1
        data[8..12].copy_from_slice(&1u32.to_le_bytes());

        let cap = DeviceCapability::unpack(&data);

        assert_eq!(cap.feature, 1);
        assert_eq!(cap.fclk_can, 80_000_000);
        assert_eq!(cap.tseg1_min, 1);
    }

    #[test]
    fn test_device_info_unpack() {
        let mut data = vec![0u8; 12];
        data[3] = 1; // icount = 1 (2 channels)
        data[4..8].copy_from_slice(&20u32.to_le_bytes()); // fw_version = 20 (2.0)
        data[8..12].copy_from_slice(&10u32.to_le_bytes()); // hw_version = 10 (1.0)

        let info = DeviceInfo::unpack(&data);

        assert_eq!(info.channel_count(), 2);
        assert_eq!(info.firmware_version(), 2.0);
        assert_eq!(info.hardware_version(), 1.0);
    }

    #[test]
    fn test_device_bit_timing_new() {
        let timing = DeviceBitTiming::new(2, 13, 2, 1, 6);
        assert_eq!(timing.prop_seg, 2);
        assert_eq!(timing.phase_seg1, 13);
        assert_eq!(timing.phase_seg2, 2);
        assert_eq!(timing.sjw, 1);
        assert_eq!(timing.brp, 6);
    }

    #[test]
    fn test_device_bit_timing_pack_large_values() {
        let timing =
            DeviceBitTiming::new(0x12345678, 0x87654321, 0xABCDEF00, 0x00FEDCBA, 0x11223344);
        let packed = timing.pack();

        // 验证 little-endian 编码
        assert_eq!(packed[0..4], [0x78, 0x56, 0x34, 0x12]); // prop_seg
        assert_eq!(packed[4..8], [0x21, 0x43, 0x65, 0x87]); // phase_seg1
        assert_eq!(packed[8..12], [0x00, 0xEF, 0xCD, 0xAB]); // phase_seg2
        assert_eq!(packed[12..16], [0xBA, 0xDC, 0xFE, 0x00]); // sjw
        assert_eq!(packed[16..20], [0x44, 0x33, 0x22, 0x11]); // brp
    }

    #[test]
    fn test_device_bit_timing_pack_zero() {
        let timing = DeviceBitTiming::new(0, 0, 0, 0, 0);
        let packed = timing.pack();

        assert_eq!(packed, [0u8; 20]);
    }

    #[test]
    fn test_device_mode_new() {
        let mode = DeviceMode::new(GS_CAN_MODE_START, GS_CAN_MODE_NORMAL);
        assert_eq!(mode.mode, GS_CAN_MODE_START);
        assert_eq!(mode.flags, GS_CAN_MODE_NORMAL);
    }

    #[test]
    fn test_device_mode_pack_reset() {
        let mode = DeviceMode::new(GS_CAN_MODE_RESET, 0);
        let packed = mode.pack();

        assert_eq!(packed[0..4], [0, 0, 0, 0]); // mode = RESET
        assert_eq!(packed[4..8], [0, 0, 0, 0]); // flags = 0
    }

    #[test]
    fn test_device_mode_pack_combined_flags() {
        let flags = GS_CAN_MODE_LOOP_BACK | GS_CAN_MODE_HW_TIMESTAMP;
        let mode = DeviceMode::new(GS_CAN_MODE_START, flags);
        let packed = mode.pack();

        assert_eq!(packed[0..4], [1, 0, 0, 0]); // mode = START
        // flags = LOOP_BACK | HW_TIMESTAMP = (1 << 1) | (1 << 4) = 0x12
        assert_eq!(packed[4..8], [0x12, 0, 0, 0]);
    }

    #[test]
    fn test_device_mode_pack_all_flags() {
        let flags = GS_CAN_MODE_LISTEN_ONLY
            | GS_CAN_MODE_LOOP_BACK
            | GS_CAN_MODE_TRIPLE_SAMPLE
            | GS_CAN_MODE_ONE_SHOT
            | GS_CAN_MODE_HW_TIMESTAMP;
        let mode = DeviceMode::new(GS_CAN_MODE_START, flags);
        let packed = mode.pack();

        assert_eq!(packed[0..4], [1, 0, 0, 0]); // mode = START
        // flags = 0x1F (所有标志位)
        assert_eq!(packed[4..8], [0x1F, 0, 0, 0]);
    }

    #[test]
    fn test_device_capability_unpack_full() {
        let mut data = vec![0u8; 40];

        // 设置所有字段
        data[0..4].copy_from_slice(&0x12345678u32.to_le_bytes()); // feature
        data[4..8].copy_from_slice(&80_000_000u32.to_le_bytes()); // fclk_can
        data[8..12].copy_from_slice(&1u32.to_le_bytes()); // tseg1_min
        data[12..16].copy_from_slice(&16u32.to_le_bytes()); // tseg1_max
        data[16..20].copy_from_slice(&1u32.to_le_bytes()); // tseg2_min
        data[20..24].copy_from_slice(&8u32.to_le_bytes()); // tseg2_max
        data[24..28].copy_from_slice(&4u32.to_le_bytes()); // sjw_max
        data[28..32].copy_from_slice(&1u32.to_le_bytes()); // brp_min
        data[32..36].copy_from_slice(&1024u32.to_le_bytes()); // brp_max
        data[36..40].copy_from_slice(&1u32.to_le_bytes()); // brp_inc

        let cap = DeviceCapability::unpack(&data);

        assert_eq!(cap.feature, 0x12345678);
        assert_eq!(cap.fclk_can, 80_000_000);
        assert_eq!(cap.tseg1_min, 1);
        assert_eq!(cap.tseg1_max, 16);
        assert_eq!(cap.tseg2_min, 1);
        assert_eq!(cap.tseg2_max, 8);
        assert_eq!(cap.sjw_max, 4);
        assert_eq!(cap.brp_min, 1);
        assert_eq!(cap.brp_max, 1024);
        assert_eq!(cap.brp_inc, 1);
    }

    #[test]
    fn test_device_capability_unpack_zero() {
        let data = vec![0u8; 40];
        let cap = DeviceCapability::unpack(&data);

        assert_eq!(cap.feature, 0);
        assert_eq!(cap.fclk_can, 0);
        assert_eq!(cap.tseg1_min, 0);
        assert_eq!(cap.tseg1_max, 0);
        assert_eq!(cap.tseg2_min, 0);
        assert_eq!(cap.tseg2_max, 0);
        assert_eq!(cap.sjw_max, 0);
        assert_eq!(cap.brp_min, 0);
        assert_eq!(cap.brp_max, 0);
        assert_eq!(cap.brp_inc, 0);
    }

    #[test]
    fn test_device_info_channel_count() {
        // 测试不同的通道数
        let mut data = vec![0u8; 12];

        data[3] = 0; // icount = 0 (1 channel)
        let info1 = DeviceInfo::unpack(&data);
        assert_eq!(info1.channel_count(), 1);

        data[3] = 3; // icount = 3 (4 channels)
        let info2 = DeviceInfo::unpack(&data);
        assert_eq!(info2.channel_count(), 4);
    }

    #[test]
    fn test_device_info_version_conversion() {
        let mut data = vec![0u8; 12];
        data[3] = 0;

        // 测试版本号转换
        data[4..8].copy_from_slice(&150u32.to_le_bytes()); // fw_version = 150 (15.0)
        data[8..12].copy_from_slice(&200u32.to_le_bytes()); // hw_version = 200 (20.0)

        let info = DeviceInfo::unpack(&data);
        assert_eq!(info.firmware_version(), 15.0);
        assert_eq!(info.hardware_version(), 20.0);
    }

    #[test]
    fn test_hw_timestamp_constant() {
        assert_eq!(GS_CAN_MODE_HW_TIMESTAMP, 1 << 4);
        assert_eq!(GS_CAN_MODE_HW_TIMESTAMP, 16);
    }

    #[test]
    fn test_frame_size_constants() {
        assert_eq!(GS_USB_FRAME_SIZE, 20);
        assert_eq!(GS_USB_FRAME_SIZE_HW_TIMESTAMP, 24);
        assert_eq!(GS_USB_FRAME_SIZE_HW_TIMESTAMP - GS_USB_FRAME_SIZE, 4); // 时间戳占 4 字节
    }

    #[test]
    fn test_can_flags_combinations() {
        // 测试标志位的组合
        let extended_rtr = CAN_EFF_FLAG | CAN_RTR_FLAG;
        assert_eq!(extended_rtr & CAN_EFF_FLAG, CAN_EFF_FLAG);
        assert_eq!(extended_rtr & CAN_RTR_FLAG, CAN_RTR_FLAG);

        // 测试掩码提取 ID
        let can_id_with_flags = 0x12345678 | CAN_EFF_FLAG | CAN_RTR_FLAG;
        let id_only = can_id_with_flags & CAN_EFF_MASK;
        assert_eq!(id_only, 0x12345678);
    }

    #[test]
    fn test_mode_flag_combinations() {
        // 测试模式标志的组合
        let combined = GS_CAN_MODE_LOOP_BACK | GS_CAN_MODE_HW_TIMESTAMP;
        assert_eq!(combined & GS_CAN_MODE_LOOP_BACK, GS_CAN_MODE_LOOP_BACK);
        assert_eq!(
            combined & GS_CAN_MODE_HW_TIMESTAMP,
            GS_CAN_MODE_HW_TIMESTAMP
        );

        // 测试普通模式（无标志位）
        assert_eq!(GS_CAN_MODE_NORMAL, 0);
        assert_eq!(GS_CAN_MODE_NORMAL & GS_CAN_MODE_LOOP_BACK, 0);
    }

    #[test]
    fn test_usb_request_types() {
        assert_eq!(GS_USB_REQ_OUT, 0x41);
        assert_eq!(GS_USB_REQ_IN, 0xC1);
    }

    #[test]
    fn test_overflow_flag() {
        assert_eq!(GS_CAN_FLAG_OVERFLOW, 1 << 0);
        assert_eq!(GS_CAN_FLAG_OVERFLOW, 1);
    }

    #[test]
    fn test_echo_ids() {
        assert_eq!(GS_USB_ECHO_ID, 0);
        assert_eq!(GS_USB_RX_ECHO_ID, 0xFFFF_FFFF);
        assert_ne!(GS_USB_ECHO_ID, GS_USB_RX_ECHO_ID);
    }
}
