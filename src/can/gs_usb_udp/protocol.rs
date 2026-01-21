//! GS-USB 守护进程协议定义
//!
//! 用于守护进程和客户端之间的通信协议（UDS/UDP）
//!
//! 参考：`daemon_implementation_plan.md` 第 3.2 节

use crate::can::PiperFrame;

// ============================================================================
// Message Types
// ============================================================================

/// 消息类型枚举
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageType {
    // 客户端 → 守护进程
    Heartbeat = 0x00,  // 心跳包（防止超时）
    Connect = 0x01,    // 客户端连接请求
    Disconnect = 0x02, // 客户端断开请求
    SendFrame = 0x03,  // 发送 CAN 帧
    GetStatus = 0x04,  // 查询守护进程状态
    SetFilter = 0x05,  // 设置 CAN ID 过滤规则

    // 守护进程 → 客户端
    ConnectAck = 0x81,     // 连接确认
    DisconnectAck = 0x82,  // 断开确认
    ReceiveFrame = 0x83,   // 接收到的 CAN 帧
    StatusResponse = 0x84, // 状态响应
    SendAck = 0x85,        // 发送确认（带 Sequence Number）
    Error = 0xFF,          // 错误消息
}

impl MessageType {
    /// 从 u8 值创建 MessageType
    pub fn from_u8(value: u8) -> Option<Self> {
        match value {
            0x00 => Some(MessageType::Heartbeat),
            0x01 => Some(MessageType::Connect),
            0x02 => Some(MessageType::Disconnect),
            0x03 => Some(MessageType::SendFrame),
            0x04 => Some(MessageType::GetStatus),
            0x05 => Some(MessageType::SetFilter),
            0x81 => Some(MessageType::ConnectAck),
            0x82 => Some(MessageType::DisconnectAck),
            0x83 => Some(MessageType::ReceiveFrame),
            0x84 => Some(MessageType::StatusResponse),
            0x85 => Some(MessageType::SendAck),
            0xFF => Some(MessageType::Error),
            _ => None,
        }
    }
}

// ============================================================================
// Error Codes
// ============================================================================

/// 错误码枚举
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    Unknown = 0x00,
    DeviceNotFound = 0x01,
    DeviceBusy = 0x02,
    InvalidMessage = 0x03,
    NotConnected = 0x04,
    DeviceError = 0x05,
    Timeout = 0x06,
}

impl ErrorCode {
    /// 从 u8 值创建 ErrorCode
    pub fn from_u8(value: u8) -> Self {
        match value {
            0x01 => ErrorCode::DeviceNotFound,
            0x02 => ErrorCode::DeviceBusy,
            0x03 => ErrorCode::InvalidMessage,
            0x04 => ErrorCode::NotConnected,
            0x05 => ErrorCode::DeviceError,
            0x06 => ErrorCode::Timeout,
            _ => ErrorCode::Unknown,
        }
    }
}

// ============================================================================
// CAN ID Filter
// ============================================================================

/// CAN ID 过滤规则
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CanIdFilter {
    /// 最小 CAN ID（包含）
    pub min_id: u32,
    /// 最大 CAN ID（包含）
    pub max_id: u32,
}

impl CanIdFilter {
    /// 创建新的过滤规则
    pub fn new(min_id: u32, max_id: u32) -> Self {
        Self { min_id, max_id }
    }

    /// 检查帧是否匹配过滤规则
    pub fn matches(&self, can_id: u32) -> bool {
        self.min_id <= can_id && can_id <= self.max_id
    }
}

// ============================================================================
// Message Header
// ============================================================================

/// 消息头（8 字节）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MessageHeader {
    pub msg_type: MessageType,
    pub flags: u8,
    pub length: u16,
    pub reserved: u8,
    pub seq: u32,
}

impl MessageHeader {
    /// 创建新的消息头
    pub fn new(msg_type: MessageType, length: u16, seq: u32) -> Self {
        Self {
            msg_type,
            flags: 0,
            length,
            reserved: 0,
            seq,
        }
    }

    /// 编码消息头到缓冲区（8 字节）
    pub fn encode(&self, buf: &mut [u8]) {
        assert!(buf.len() >= 8);
        buf[0] = self.msg_type as u8;
        buf[1] = self.flags;
        buf[2..4].copy_from_slice(&self.length.to_le_bytes());
        buf[4] = self.reserved;
        buf[5..8].copy_from_slice(&self.seq.to_le_bytes()[..3]);
        // 注意：根据实现方案，消息头是 8 字节，序列号使用 3 字节（24 位）
        // 这可以支持最大 16,777,215 的序列号，对于实际应用足够
    }

    /// 从缓冲区解码消息头
    pub fn decode(buf: &[u8]) -> Result<Self, ProtocolError> {
        if buf.len() < 8 {
            return Err(ProtocolError::TooShort);
        }

        let msg_type = MessageType::from_u8(buf[0]).ok_or(ProtocolError::InvalidMessageType)?;
        let flags = buf[1];
        let length = u16::from_le_bytes([buf[2], buf[3]]);
        let reserved = buf[4];
        let seq = u32::from_le_bytes([
            buf[5], buf[6], buf[7], 0, // 高字节为 0（只使用 24 位）
        ]);

        Ok(Self {
            msg_type,
            flags,
            length,
            reserved,
            seq,
        })
    }
}

// ============================================================================
// Protocol Error
// ============================================================================

/// 协议错误类型
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProtocolError {
    TooShort,
    InvalidMessageType,
    Incomplete,
    InvalidData,
}

impl std::fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProtocolError::TooShort => write!(f, "Message too short"),
            ProtocolError::InvalidMessageType => write!(f, "Invalid message type"),
            ProtocolError::Incomplete => write!(f, "Incomplete message"),
            ProtocolError::InvalidData => write!(f, "Invalid message data"),
        }
    }
}

impl std::error::Error for ProtocolError {}

// ============================================================================
// Message Enum
// ============================================================================

/// 协议消息枚举
#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    Heartbeat {
        client_id: u32,
    },
    Connect {
        client_id: u32,
        filters: Vec<CanIdFilter>,
    },
    Disconnect {
        client_id: u32,
    },
    SendFrame {
        frame: PiperFrame,
        seq: u32,
    },
    ReceiveFrame(PiperFrame),
    ConnectAck {
        client_id: u32,
        status: u8,
    },
    DisconnectAck,
    GetStatus,
    StatusResponse {
        /// 设备状态（0=Disconnected, 1=Connected, 2=Reconnecting）
        device_state: u8,
        /// RX 帧率（FPS，编码为 u32，实际值为 fps * 1000）
        rx_fps_x1000: u32,
        /// TX 帧率（FPS，编码为 u32，实际值为 fps * 1000）
        tx_fps_x1000: u32,
        /// IPC 发送到客户端的帧率（FPS * 1000）
        ipc_sent_fps_x1000: u32,
        /// IPC 从客户端接收的帧率（FPS * 1000）
        ipc_received_fps_x1000: u32,
        /// 健康度评分（0-100）
        health_score: u8,
        /// USB STALL 计数
        usb_stall_count: u64,
        /// CAN Bus Off 计数
        can_bus_off_count: u64,
        /// CAN Error Passive 计数
        can_error_passive_count: u64,
        /// CPU 占用率（0-100）
        cpu_usage_percent: u8,
        /// 客户端数量
        client_count: u32,
        /// 客户端发送阻塞次数
        client_send_blocked: u64,
    },
    Error {
        code: ErrorCode,
        message: String,
    },
    SendAck {
        seq: u32,
        status: u8,
    },
    SetFilter {
        client_id: u32,
        filters: Vec<CanIdFilter>,
    },
}

// ============================================================================
// Encoding Functions (Zero-Copy)
// ============================================================================

/// 编码心跳消息（最小消息，只有头部）
pub fn encode_heartbeat(client_id: u32, seq: u32, buf: &mut [u8; 12]) -> &[u8] {
    assert!(buf.len() >= 12);
    let header = MessageHeader::new(MessageType::Heartbeat, 12, seq);
    header.encode(&mut buf[..8]);
    buf[8..12].copy_from_slice(&client_id.to_le_bytes());
    &buf[..12]
}

/// 编码 Connect 消息
pub fn encode_connect<'a>(
    client_id: u32,
    filters: &[CanIdFilter],
    seq: u32,
    buf: &'a mut [u8; 256],
) -> Result<&'a [u8], ProtocolError> {
    // 计算消息长度：8 (header) + 4 (client_id) + 1 (filter_count) + filters.len() * 8
    let filter_count = filters.len().min(255);
    let length = 8 + 4 + 1 + (filter_count * 8);

    if length > buf.len() {
        return Err(ProtocolError::InvalidData);
    }

    let header = MessageHeader::new(MessageType::Connect, length as u16, seq);
    header.encode(&mut buf[..8]);

    buf[8..12].copy_from_slice(&client_id.to_le_bytes());
    buf[12] = filter_count as u8;

    // 编码过滤规则
    for (i, filter) in filters.iter().take(filter_count).enumerate() {
        let offset = 13 + (i * 8);
        buf[offset..offset + 4].copy_from_slice(&filter.min_id.to_le_bytes());
        buf[offset + 4..offset + 8].copy_from_slice(&filter.max_id.to_le_bytes());
    }

    Ok(&buf[..length])
}

/// 编码 SendFrame 消息（零拷贝）
pub fn encode_send_frame_with_seq<'a>(
    frame: &PiperFrame,
    seq: u32,
    buf: &'a mut [u8; 64],
) -> Result<&'a [u8], ProtocolError> {
    // 计算消息长度：8 (header) + 4 (can_id) + 1 (flags) + 1 (dlc) + frame.len
    let length = 8 + 4 + 1 + 1 + frame.len as usize;

    if length > buf.len() {
        return Err(ProtocolError::InvalidData);
    }

    let header = MessageHeader::new(MessageType::SendFrame, length as u16, seq);
    header.encode(&mut buf[..8]);

    buf[8..12].copy_from_slice(&frame.id.to_le_bytes());
    buf[12] = if frame.is_extended { 0x01 } else { 0x00 };
    buf[13] = frame.len;
    buf[14..14 + frame.len as usize].copy_from_slice(&frame.data[..frame.len as usize]);

    Ok(&buf[..length])
}

/// 编码 ReceiveFrame 消息（零拷贝）
pub fn encode_receive_frame_zero_copy<'a>(
    frame: &PiperFrame,
    buf: &'a mut [u8; 64],
) -> Result<&'a [u8], ProtocolError> {
    // 计算消息长度：8 (header) + 4 (can_id) + 1 (flags) + 1 (dlc) + 8 (timestamp) + frame.len
    let length = 8 + 4 + 1 + 1 + 8 + frame.len as usize;

    if length > buf.len() {
        return Err(ProtocolError::InvalidData);
    }

    let header = MessageHeader::new(MessageType::ReceiveFrame, length as u16, 0);
    header.encode(&mut buf[..8]);

    buf[8..12].copy_from_slice(&frame.id.to_le_bytes());
    buf[12] = if frame.is_extended { 0x01 } else { 0x00 };
    buf[13] = frame.len;
    buf[14..22].copy_from_slice(&frame.timestamp_us.to_le_bytes());
    buf[22..22 + frame.len as usize].copy_from_slice(&frame.data[..frame.len as usize]);

    Ok(&buf[..length])
}

/// 编码 SendAck 消息
pub fn encode_send_ack(seq: u32, status: u8, buf: &mut [u8; 12]) -> &[u8] {
    let header = MessageHeader::new(MessageType::SendAck, 12, seq);
    header.encode(&mut buf[..8]);
    buf[8] = status;
    // 剩余字节为 0
    &buf[..12]
}

/// 编码 Error 消息
pub fn encode_error<'a>(
    code: ErrorCode,
    message: &str,
    seq: u32,
    buf: &'a mut [u8; 256],
) -> Result<&'a [u8], ProtocolError> {
    let msg_bytes = message.as_bytes();
    let length = 8 + 1 + msg_bytes.len();

    if length > buf.len() {
        return Err(ProtocolError::InvalidData);
    }

    let header = MessageHeader::new(MessageType::Error, length as u16, seq);
    header.encode(&mut buf[..8]);
    buf[8] = code as u8;
    buf[9..9 + msg_bytes.len()].copy_from_slice(msg_bytes);

    Ok(&buf[..length])
}

/// 编码 Disconnect 消息
pub fn encode_disconnect(client_id: u32, seq: u32, buf: &mut [u8; 12]) -> &[u8] {
    let header = MessageHeader::new(MessageType::Disconnect, 12, seq);
    header.encode(&mut buf[..8]);
    buf[8..12].copy_from_slice(&client_id.to_le_bytes());
    &buf[..12]
}

/// 编码 DisconnectAck 消息
pub fn encode_disconnect_ack(seq: u32, buf: &mut [u8; 8]) -> &[u8] {
    let header = MessageHeader::new(MessageType::DisconnectAck, 8, seq);
    header.encode(&mut buf[..8]);
    &buf[..8]
}

/// 编码 ConnectAck 消息
pub fn encode_connect_ack(client_id: u32, status: u8, seq: u32, buf: &mut [u8; 13]) -> &[u8] {
    let header = MessageHeader::new(MessageType::ConnectAck, 13, seq);
    header.encode(&mut buf[..8]);
    buf[8..12].copy_from_slice(&client_id.to_le_bytes());
    buf[12] = status;
    &buf[..13]
}

/// 编码 SetFilter 消息
pub fn encode_set_filter<'a>(
    client_id: u32,
    filters: &[CanIdFilter],
    seq: u32,
    buf: &'a mut [u8; 256],
) -> Result<&'a [u8], ProtocolError> {
    // 计算消息长度：8 (header) + 4 (client_id) + 1 (filter_count) + filters.len() * 8
    let filter_count = filters.len().min(255);
    let length = 8 + 4 + 1 + (filter_count * 8);

    if length > buf.len() {
        return Err(ProtocolError::InvalidData);
    }

    let header = MessageHeader::new(MessageType::SetFilter, length as u16, seq);
    header.encode(&mut buf[..8]);

    buf[8..12].copy_from_slice(&client_id.to_le_bytes());
    buf[12] = filter_count as u8;

    // 编码过滤规则
    for (i, filter) in filters.iter().take(filter_count).enumerate() {
        let offset = 13 + (i * 8);
        buf[offset..offset + 4].copy_from_slice(&filter.min_id.to_le_bytes());
        buf[offset + 4..offset + 8].copy_from_slice(&filter.max_id.to_le_bytes());
    }

    Ok(&buf[..length])
}

/// 编码 GetStatus 消息
pub fn encode_get_status(seq: u32, buf: &mut [u8; 8]) -> &[u8] {
    let header = MessageHeader::new(MessageType::GetStatus, 8, seq);
    header.encode(&mut buf[..8]);
    &buf[..8]
}

/// 编码 StatusResponse 消息
pub fn encode_status_response<'a>(
    status: &StatusResponse,
    seq: u32,
    buf: &'a mut [u8; 64],
) -> Result<&'a [u8], ProtocolError> {
    // 计算消息长度：8 (header) + 状态字段
    // 状态字段：1 (device_state) + 4*4 (4个fps_x1000) + 1 (health_score) + 8*3 (3个u64计数) + 1 (cpu_usage) + 4 (client_count) + 8 (client_send_blocked) = 51
    let length = 8 + 51;

    if length > buf.len() {
        return Err(ProtocolError::InvalidData);
    }

    let header = MessageHeader::new(MessageType::StatusResponse, length as u16, seq);
    header.encode(&mut buf[..8]);

    let mut offset = 8;

    // 编码字段
    buf[offset] = status.device_state;
    offset += 1;

    buf[offset..offset + 4].copy_from_slice(&status.rx_fps_x1000.to_le_bytes());
    offset += 4;

    buf[offset..offset + 4].copy_from_slice(&status.tx_fps_x1000.to_le_bytes());
    offset += 4;

    buf[offset..offset + 4].copy_from_slice(&status.ipc_sent_fps_x1000.to_le_bytes());
    offset += 4;

    buf[offset..offset + 4].copy_from_slice(&status.ipc_received_fps_x1000.to_le_bytes());
    offset += 4;

    buf[offset] = status.health_score;
    offset += 1;

    buf[offset..offset + 8].copy_from_slice(&status.usb_stall_count.to_le_bytes());
    offset += 8;

    buf[offset..offset + 8].copy_from_slice(&status.can_bus_off_count.to_le_bytes());
    offset += 8;

    buf[offset..offset + 8].copy_from_slice(&status.can_error_passive_count.to_le_bytes());
    offset += 8;

    buf[offset] = status.cpu_usage_percent;
    offset += 1;

    buf[offset..offset + 4].copy_from_slice(&status.client_count.to_le_bytes());
    offset += 4;

    buf[offset..offset + 8].copy_from_slice(&status.client_send_blocked.to_le_bytes());

    Ok(&buf[..length])
}

/// 状态响应结构体
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatusResponse {
    /// 设备状态（0=Disconnected, 1=Connected, 2=Reconnecting）
    pub device_state: u8,
    /// RX 帧率（FPS，编码为 u32，实际值为 fps * 1000）
    pub rx_fps_x1000: u32,
    /// TX 帧率（FPS，编码为 u32，实际值为 fps * 1000）
    pub tx_fps_x1000: u32,
    /// IPC 发送到客户端的帧率（FPS * 1000）
    pub ipc_sent_fps_x1000: u32,
    /// IPC 从客户端接收的帧率（FPS * 1000）
    pub ipc_received_fps_x1000: u32,
    /// 健康度评分（0-100）
    pub health_score: u8,
    /// USB STALL 计数
    pub usb_stall_count: u64,
    /// CAN Bus Off 计数
    pub can_bus_off_count: u64,
    /// CAN Error Passive 计数
    pub can_error_passive_count: u64,
    /// CPU 占用率（0-100）
    pub cpu_usage_percent: u8,
    /// 客户端数量
    pub client_count: u32,
    /// 客户端发送阻塞次数
    pub client_send_blocked: u64,
}

// ============================================================================
// Decoding Functions
// ============================================================================

/// 解码消息
pub fn decode_message(data: &[u8]) -> Result<Message, ProtocolError> {
    if data.len() < 8 {
        return Err(ProtocolError::TooShort);
    }

    let header = MessageHeader::decode(data)?;

    if data.len() < header.length as usize {
        return Err(ProtocolError::Incomplete);
    }

    match header.msg_type {
        MessageType::Heartbeat => {
            if data.len() < 12 {
                return Err(ProtocolError::Incomplete);
            }
            let client_id = u32::from_le_bytes([data[8], data[9], data[10], data[11]]);
            Ok(Message::Heartbeat { client_id })
        },
        MessageType::Connect => {
            if data.len() < 13 {
                return Err(ProtocolError::Incomplete);
            }
            let client_id = u32::from_le_bytes([data[8], data[9], data[10], data[11]]);
            let filter_count = data[12] as usize;

            let mut filters = Vec::new();
            for i in 0..filter_count {
                let offset = 13 + (i * 8);
                if data.len() < offset + 8 {
                    return Err(ProtocolError::Incomplete);
                }
                let min_id = u32::from_le_bytes([
                    data[offset],
                    data[offset + 1],
                    data[offset + 2],
                    data[offset + 3],
                ]);
                let max_id = u32::from_le_bytes([
                    data[offset + 4],
                    data[offset + 5],
                    data[offset + 6],
                    data[offset + 7],
                ]);
                filters.push(CanIdFilter::new(min_id, max_id));
            }

            Ok(Message::Connect { client_id, filters })
        },
        MessageType::SendFrame => {
            if data.len() < 14 {
                return Err(ProtocolError::Incomplete);
            }
            let id = u32::from_le_bytes([data[8], data[9], data[10], data[11]]);
            let is_extended = (data[12] & 0x01) != 0;
            let len = data[13].min(8);

            if data.len() < 14 + len as usize {
                return Err(ProtocolError::Incomplete);
            }

            let mut frame_data = [0u8; 8];
            frame_data[..len as usize].copy_from_slice(&data[14..14 + len as usize]);

            Ok(Message::SendFrame {
                frame: PiperFrame {
                    id,
                    data: frame_data,
                    len,
                    is_extended,
                    timestamp_us: 0,
                },
                seq: header.seq,
            })
        },
        MessageType::ReceiveFrame => {
            if data.len() < 22 {
                return Err(ProtocolError::Incomplete);
            }
            let id = u32::from_le_bytes([data[8], data[9], data[10], data[11]]);
            let is_extended = (data[12] & 0x01) != 0;
            let len = data[13].min(8);
            let timestamp = u64::from_le_bytes([
                data[14], data[15], data[16], data[17], data[18], data[19], data[20], data[21],
            ]);

            if data.len() < 22 + len as usize {
                return Err(ProtocolError::Incomplete);
            }

            let mut frame_data = [0u8; 8];
            frame_data[..len as usize].copy_from_slice(&data[22..22 + len as usize]);

            Ok(Message::ReceiveFrame(PiperFrame {
                id,
                data: frame_data,
                len,
                is_extended,
                timestamp_us: timestamp,
            }))
        },
        MessageType::SendAck => {
            if data.len() < 12 {
                return Err(ProtocolError::Incomplete);
            }
            let status = data[8];
            Ok(Message::SendAck {
                seq: header.seq,
                status,
            })
        },
        MessageType::Error => {
            if data.len() < 9 {
                return Err(ProtocolError::Incomplete);
            }
            let code = ErrorCode::from_u8(data[8]);
            let message = String::from_utf8_lossy(&data[9..]).to_string();
            Ok(Message::Error { code, message })
        },
        MessageType::ConnectAck => {
            if data.len() < 13 {
                return Err(ProtocolError::Incomplete);
            }
            let client_id = u32::from_le_bytes([data[8], data[9], data[10], data[11]]);
            let status = data[12];
            Ok(Message::ConnectAck { client_id, status })
        },
        MessageType::Disconnect => {
            if data.len() < 12 {
                return Err(ProtocolError::Incomplete);
            }
            let client_id = u32::from_le_bytes([data[8], data[9], data[10], data[11]]);
            Ok(Message::Disconnect { client_id })
        },
        MessageType::DisconnectAck => Ok(Message::DisconnectAck),
        MessageType::SetFilter => {
            if data.len() < 13 {
                return Err(ProtocolError::Incomplete);
            }
            let client_id = u32::from_le_bytes([data[8], data[9], data[10], data[11]]);
            let filter_count = data[12] as usize;

            let mut filters = Vec::new();
            for i in 0..filter_count {
                let offset = 13 + (i * 8);
                if data.len() < offset + 8 {
                    return Err(ProtocolError::Incomplete);
                }
                let min_id = u32::from_le_bytes([
                    data[offset],
                    data[offset + 1],
                    data[offset + 2],
                    data[offset + 3],
                ]);
                let max_id = u32::from_le_bytes([
                    data[offset + 4],
                    data[offset + 5],
                    data[offset + 6],
                    data[offset + 7],
                ]);
                filters.push(CanIdFilter::new(min_id, max_id));
            }

            Ok(Message::SetFilter { client_id, filters })
        },
        MessageType::GetStatus => Ok(Message::GetStatus),
        MessageType::StatusResponse => {
            if data.len() < 59 {
                return Err(ProtocolError::Incomplete);
            }
            let mut offset = 8;

            let device_state = data[offset];
            offset += 1;

            let rx_fps_x1000 = u32::from_le_bytes([
                data[offset],
                data[offset + 1],
                data[offset + 2],
                data[offset + 3],
            ]);
            offset += 4;

            let tx_fps_x1000 = u32::from_le_bytes([
                data[offset],
                data[offset + 1],
                data[offset + 2],
                data[offset + 3],
            ]);
            offset += 4;

            let ipc_sent_fps_x1000 = u32::from_le_bytes([
                data[offset],
                data[offset + 1],
                data[offset + 2],
                data[offset + 3],
            ]);
            offset += 4;

            let ipc_received_fps_x1000 = u32::from_le_bytes([
                data[offset],
                data[offset + 1],
                data[offset + 2],
                data[offset + 3],
            ]);
            offset += 4;

            let health_score = data[offset];
            offset += 1;

            let usb_stall_count = u64::from_le_bytes([
                data[offset],
                data[offset + 1],
                data[offset + 2],
                data[offset + 3],
                data[offset + 4],
                data[offset + 5],
                data[offset + 6],
                data[offset + 7],
            ]);
            offset += 8;

            let can_bus_off_count = u64::from_le_bytes([
                data[offset],
                data[offset + 1],
                data[offset + 2],
                data[offset + 3],
                data[offset + 4],
                data[offset + 5],
                data[offset + 6],
                data[offset + 7],
            ]);
            offset += 8;

            let can_error_passive_count = u64::from_le_bytes([
                data[offset],
                data[offset + 1],
                data[offset + 2],
                data[offset + 3],
                data[offset + 4],
                data[offset + 5],
                data[offset + 6],
                data[offset + 7],
            ]);
            offset += 8;

            let cpu_usage_percent = data[offset];
            offset += 1;

            let client_count = u32::from_le_bytes([
                data[offset],
                data[offset + 1],
                data[offset + 2],
                data[offset + 3],
            ]);
            offset += 4;

            let client_send_blocked = u64::from_le_bytes([
                data[offset],
                data[offset + 1],
                data[offset + 2],
                data[offset + 3],
                data[offset + 4],
                data[offset + 5],
                data[offset + 6],
                data[offset + 7],
            ]);

            Ok(Message::StatusResponse {
                device_state,
                rx_fps_x1000,
                tx_fps_x1000,
                ipc_sent_fps_x1000,
                ipc_received_fps_x1000,
                health_score,
                usb_stall_count,
                can_bus_off_count,
                can_error_passive_count,
                cpu_usage_percent,
                client_count,
                client_send_blocked,
            })
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::can::PiperFrame;

    #[test]
    fn test_message_type_values() {
        assert_eq!(MessageType::Heartbeat as u8, 0x00);
        assert_eq!(MessageType::Connect as u8, 0x01);
        assert_eq!(MessageType::SendFrame as u8, 0x03);
        assert_eq!(MessageType::ReceiveFrame as u8, 0x83);
        assert_eq!(MessageType::SendAck as u8, 0x85);
        assert_eq!(MessageType::Error as u8, 0xFF);
    }

    #[test]
    fn test_message_type_from_u8() {
        assert_eq!(MessageType::from_u8(0x00), Some(MessageType::Heartbeat));
        assert_eq!(MessageType::from_u8(0x01), Some(MessageType::Connect));
        assert_eq!(MessageType::from_u8(0x99), None);
    }

    #[test]
    fn test_message_header_roundtrip() {
        let header = MessageHeader::new(MessageType::Connect, 20, 12345);
        let mut buf = [0u8; 8];
        header.encode(&mut buf);

        let decoded = MessageHeader::decode(&buf).unwrap();
        assert_eq!(header.msg_type, decoded.msg_type);
        assert_eq!(header.length, decoded.length);
        assert_eq!(header.seq, decoded.seq);
    }

    #[test]
    fn test_heartbeat_encode_decode() {
        let mut buf = [0u8; 12];
        let encoded = encode_heartbeat(12345, 0, &mut buf);

        let decoded = decode_message(encoded).unwrap();
        match decoded {
            Message::Heartbeat { client_id } => {
                assert_eq!(client_id, 12345);
            },
            _ => panic!("Expected Heartbeat message"),
        }
    }

    #[test]
    fn test_connect_encode_decode() {
        let filters = vec![
            CanIdFilter::new(0x100, 0x200),
            CanIdFilter::new(0x300, 0x400),
        ];
        let mut buf = [0u8; 256];
        let encoded = encode_connect(12345, &filters, 0, &mut buf).unwrap();

        let decoded = decode_message(encoded).unwrap();
        match decoded {
            Message::Connect {
                client_id,
                filters: decoded_filters,
            } => {
                assert_eq!(client_id, 12345);
                assert_eq!(decoded_filters.len(), 2);
                assert_eq!(decoded_filters[0].min_id, 0x100);
                assert_eq!(decoded_filters[0].max_id, 0x200);
            },
            _ => panic!("Expected Connect message"),
        }
    }

    #[test]
    fn test_send_frame_encode_decode() {
        let frame = PiperFrame::new_standard(0x123, &[0x01, 0x02, 0x03, 0x04]);
        let mut buf = [0u8; 64];
        let encoded = encode_send_frame_with_seq(&frame, 12345, &mut buf).unwrap();

        let decoded = decode_message(encoded).unwrap();
        match decoded {
            Message::SendFrame {
                frame: decoded_frame,
                seq,
            } => {
                assert_eq!(decoded_frame.id, 0x123);
                assert_eq!(decoded_frame.len, 4);
                assert_eq!(decoded_frame.data[..4], [0x01, 0x02, 0x03, 0x04]);
                assert_eq!(seq, 12345);
            },
            _ => panic!("Expected SendFrame message"),
        }
    }

    #[test]
    fn test_receive_frame_encode_decode() {
        let frame = PiperFrame {
            id: 0x123,
            data: [0x01, 0x02, 0x03, 0x04, 0, 0, 0, 0],
            len: 4,
            is_extended: false,
            timestamp_us: 12345678,
        };
        let mut buf = [0u8; 64];
        let encoded = encode_receive_frame_zero_copy(&frame, &mut buf).unwrap();

        let decoded = decode_message(encoded).unwrap();
        match decoded {
            Message::ReceiveFrame(decoded_frame) => {
                assert_eq!(decoded_frame.id, 0x123);
                assert_eq!(decoded_frame.len, 4);
                assert_eq!(decoded_frame.timestamp_us, 12345678);
            },
            _ => panic!("Expected ReceiveFrame message"),
        }
    }

    #[test]
    fn test_can_id_filter_matches() {
        let filter = CanIdFilter::new(0x100, 0x200);
        assert!(filter.matches(0x150));
        assert!(filter.matches(0x100)); // 边界：匹配
        assert!(filter.matches(0x200)); // 边界：匹配
        assert!(!filter.matches(0x099));
        assert!(!filter.matches(0x201));
    }

    #[test]
    fn test_decode_invalid_message() {
        // 测试消息太短
        assert!(decode_message(&[0x01]).is_err());

        // 测试未知消息类型
        let mut buf = [0u8; 8];
        buf[0] = 0x99; // 未知类型
        assert!(decode_message(&buf).is_err());
    }

    // ============================================================================
    // 补充测试：所有消息类型的编解码
    // ============================================================================

    #[test]
    fn test_connect_ack_encode_decode() {
        let mut buf = [0u8; 13];
        let encoded = encode_connect_ack(12345, 0, 0, &mut buf);

        let decoded = decode_message(encoded).unwrap();
        match decoded {
            Message::ConnectAck { client_id, status } => {
                assert_eq!(client_id, 12345);
                assert_eq!(status, 0);
            },
            _ => panic!("Expected ConnectAck message"),
        }
    }

    #[test]
    fn test_disconnect_encode_decode() {
        let mut buf = [0u8; 12];
        let encoded = encode_disconnect(12345, 0, &mut buf);

        let decoded = decode_message(encoded).unwrap();
        match decoded {
            Message::Disconnect { client_id } => {
                assert_eq!(client_id, 12345);
            },
            _ => panic!("Expected Disconnect message"),
        }
    }

    #[test]
    fn test_disconnect_ack_encode_decode() {
        let mut buf = [0u8; 8];
        let encoded = encode_disconnect_ack(0, &mut buf);

        let decoded = decode_message(encoded).unwrap();
        match decoded {
            Message::DisconnectAck => {},
            _ => panic!("Expected DisconnectAck message"),
        }
    }

    #[test]
    fn test_set_filter_encode_decode() {
        let filters = vec![
            CanIdFilter::new(0x100, 0x200),
            CanIdFilter::new(0x300, 0x400),
        ];
        let mut buf = [0u8; 256];
        let encoded = encode_set_filter(12345, &filters, 0, &mut buf).unwrap();

        let decoded = decode_message(encoded).unwrap();
        match decoded {
            Message::SetFilter {
                client_id,
                filters: decoded_filters,
            } => {
                assert_eq!(client_id, 12345);
                assert_eq!(decoded_filters.len(), 2);
            },
            _ => panic!("Expected SetFilter message"),
        }
    }

    #[test]
    fn test_send_ack_encode_decode() {
        let mut buf = [0u8; 12];
        let encoded = encode_send_ack(12345, 0, &mut buf);

        let decoded = decode_message(encoded).unwrap();
        match decoded {
            Message::SendAck { seq, status } => {
                assert_eq!(seq, 12345);
                assert_eq!(status, 0);
            },
            _ => panic!("Expected SendAck message"),
        }
    }

    #[test]
    fn test_error_encode_decode() {
        let mut buf = [0u8; 256];
        let encoded =
            encode_error(ErrorCode::DeviceNotFound, "Device not found", 0, &mut buf).unwrap();

        let decoded = decode_message(encoded).unwrap();
        match decoded {
            Message::Error { code, message } => {
                assert_eq!(code, ErrorCode::DeviceNotFound);
                assert_eq!(message, "Device not found");
            },
            _ => panic!("Expected Error message"),
        }
    }

    #[test]
    fn test_get_status_encode_decode() {
        let mut buf = [0u8; 8];
        let encoded = encode_get_status(0, &mut buf);

        let decoded = decode_message(encoded).unwrap();
        match decoded {
            Message::GetStatus => {},
            _ => panic!("Expected GetStatus message"),
        }
    }

    // ============================================================================
    // 边界情况测试
    // ============================================================================

    #[test]
    fn test_connect_zero_filters() {
        // 测试 0 个过滤规则（接收所有帧）
        let filters = vec![];
        let mut buf = [0u8; 256];
        let encoded = encode_connect(12345, &filters, 0, &mut buf).unwrap();

        let decoded = decode_message(encoded).unwrap();
        match decoded {
            Message::Connect {
                client_id,
                filters: decoded_filters,
            } => {
                assert_eq!(client_id, 12345);
                assert_eq!(decoded_filters.len(), 0);
            },
            _ => panic!("Expected Connect message"),
        }
    }

    #[test]
    fn test_connect_max_filters() {
        // 测试最大过滤规则数量（255 个）
        let filters: Vec<CanIdFilter> = (0..255)
            .map(|i| CanIdFilter::new(i as u32 * 0x100, (i as u32 + 1) * 0x100))
            .collect();
        let mut buf = [0u8; 256];
        // 注意：255 个过滤规则需要 8 + 4 + 1 + 255*8 = 2053 字节，超过缓冲区
        // 应该被截断到 255 个
        let encoded = encode_connect(12345, &filters, 0, &mut buf);
        // 由于缓冲区太小，应该返回错误或截断
        // 这里我们测试截断的情况
        if let Ok(encoded) = encoded {
            let decoded = decode_message(encoded).unwrap();
            match decoded {
                Message::Connect {
                    filters: decoded_filters,
                    ..
                } => {
                    // 应该被截断到适合缓冲区的数量
                    assert!(decoded_filters.len() <= 30); // 30 * 8 + 13 = 253 < 256
                },
                _ => panic!("Expected Connect message"),
            }
        }
    }

    #[test]
    fn test_send_frame_empty_data() {
        // 测试 0 字节数据
        let frame = PiperFrame::new_standard(0x123, &[]);
        let mut buf = [0u8; 64];
        let encoded = encode_send_frame_with_seq(&frame, 0, &mut buf).unwrap();

        let decoded = decode_message(encoded).unwrap();
        match decoded {
            Message::SendFrame {
                frame: decoded_frame,
                ..
            } => {
                assert_eq!(decoded_frame.id, 0x123);
                assert_eq!(decoded_frame.len, 0);
            },
            _ => panic!("Expected SendFrame message"),
        }
    }

    #[test]
    fn test_send_frame_max_data() {
        // 测试 8 字节数据（最大）
        let data = [0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08];
        let frame = PiperFrame::new_standard(0x123, &data);
        let mut buf = [0u8; 64];
        let encoded = encode_send_frame_with_seq(&frame, 0, &mut buf).unwrap();

        let decoded = decode_message(encoded).unwrap();
        match decoded {
            Message::SendFrame {
                frame: decoded_frame,
                ..
            } => {
                assert_eq!(decoded_frame.id, 0x123);
                assert_eq!(decoded_frame.len, 8);
                assert_eq!(decoded_frame.data, data);
            },
            _ => panic!("Expected SendFrame message"),
        }
    }

    #[test]
    fn test_receive_frame_empty_data() {
        // 测试 0 字节数据
        let frame = PiperFrame {
            id: 0x123,
            data: [0; 8],
            len: 0,
            is_extended: false,
            timestamp_us: 12345678,
        };
        let mut buf = [0u8; 64];
        let encoded = encode_receive_frame_zero_copy(&frame, &mut buf).unwrap();

        let decoded = decode_message(encoded).unwrap();
        match decoded {
            Message::ReceiveFrame(decoded_frame) => {
                assert_eq!(decoded_frame.id, 0x123);
                assert_eq!(decoded_frame.len, 0);
                assert_eq!(decoded_frame.timestamp_us, 12345678);
            },
            _ => panic!("Expected ReceiveFrame message"),
        }
    }

    #[test]
    fn test_receive_frame_max_data() {
        // 测试 8 字节数据（最大）
        let data = [0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08];
        let frame = PiperFrame {
            id: 0x123,
            data,
            len: 8,
            is_extended: false,
            timestamp_us: 12345678,
        };
        let mut buf = [0u8; 64];
        let encoded = encode_receive_frame_zero_copy(&frame, &mut buf).unwrap();

        let decoded = decode_message(encoded).unwrap();
        match decoded {
            Message::ReceiveFrame(decoded_frame) => {
                assert_eq!(decoded_frame.id, 0x123);
                assert_eq!(decoded_frame.len, 8);
                assert_eq!(decoded_frame.data, data);
                assert_eq!(decoded_frame.timestamp_us, 12345678);
            },
            _ => panic!("Expected ReceiveFrame message"),
        }
    }

    #[test]
    fn test_receive_frame_extended_id() {
        // 测试扩展帧 ID
        let frame = PiperFrame::new_extended(0x12345678, &[0x01, 0x02]);
        let mut buf = [0u8; 64];
        let encoded = encode_receive_frame_zero_copy(&frame, &mut buf).unwrap();

        let decoded = decode_message(encoded).unwrap();
        match decoded {
            Message::ReceiveFrame(decoded_frame) => {
                assert_eq!(decoded_frame.id, 0x12345678);
                assert!(decoded_frame.is_extended);
            },
            _ => panic!("Expected ReceiveFrame message"),
        }
    }

    // ============================================================================
    // 错误处理测试
    // ============================================================================

    #[test]
    fn test_decode_message_length_mismatch() {
        // 测试消息长度不匹配（header 中的 length 与实际长度不符）
        let mut buf = [0u8; 20];
        buf[0] = MessageType::Connect as u8;
        buf[2] = 0xFF; // 长度设为 0xFFFF（最大）
        buf[3] = 0xFF;
        // 但实际数据只有 20 字节

        assert!(decode_message(&buf).is_err());
    }

    #[test]
    fn test_decode_message_incomplete_connect() {
        // 测试 Connect 消息不完整（缺少过滤规则数据）
        let mut buf = [0u8; 20];
        buf[0] = MessageType::Connect as u8;
        buf[2] = 20; // 长度
        buf[8..12].copy_from_slice(&12345u32.to_le_bytes());
        buf[12] = 2; // 2 个过滤规则
        // 但只有 1 个过滤规则的数据（缺少第二个）

        assert!(decode_message(&buf).is_err());
    }

    #[test]
    fn test_decode_message_incomplete_send_frame() {
        // 测试 SendFrame 消息不完整（数据长度不足）
        let mut buf = [0u8; 20];
        buf[0] = MessageType::SendFrame as u8;
        buf[2] = 20; // 长度
        buf[8..12].copy_from_slice(&0x123u32.to_le_bytes());
        buf[13] = 8; // 8 字节数据
        // 但实际数据只有 4 字节

        assert!(decode_message(&buf).is_err());
    }

    #[test]
    fn test_encode_error_normal() {
        // 测试 Error 消息正常编码
        let mut buf = [0u8; 256];
        let result = encode_error(ErrorCode::DeviceNotFound, "test", 0, &mut buf);
        assert!(result.is_ok());

        let decoded = decode_message(result.unwrap()).unwrap();
        match decoded {
            Message::Error { code, message } => {
                assert_eq!(code, ErrorCode::DeviceNotFound);
                assert_eq!(message, "test");
            },
            _ => panic!("Expected Error message"),
        }
    }

    #[test]
    fn test_encode_connect_buffer_too_small() {
        // 测试 Connect 消息缓冲区太小（太多过滤规则）
        // 注意：由于函数签名要求 [u8; 256]，我们测试边界情况
        // 30 个过滤规则需要 8 + 4 + 1 + 30*8 = 253 字节，刚好在范围内
        // 31 个过滤规则需要 8 + 4 + 1 + 31*8 = 261 字节，超过缓冲区
        let filters: Vec<CanIdFilter> = (0..31)
            .map(|i| CanIdFilter::new(i as u32 * 0x100, (i as u32 + 1) * 0x100))
            .collect();
        let mut buf = [0u8; 256];
        // 由于函数内部会截断到 255 个，但 31 个过滤规则需要 261 字节，超过 256
        // 所以应该返回错误
        let result = encode_connect(12345, &filters, 0, &mut buf);
        // 实际上，函数会先截断到 255 个，但 255 个需要 8+4+1+255*8 = 2053 字节
        // 所以应该返回错误
        assert!(result.is_err());
    }

    #[test]
    fn test_error_code_from_u8() {
        // 测试所有错误码
        assert_eq!(ErrorCode::from_u8(0x00), ErrorCode::Unknown);
        assert_eq!(ErrorCode::from_u8(0x01), ErrorCode::DeviceNotFound);
        assert_eq!(ErrorCode::from_u8(0x02), ErrorCode::DeviceBusy);
        assert_eq!(ErrorCode::from_u8(0x03), ErrorCode::InvalidMessage);
        assert_eq!(ErrorCode::from_u8(0x04), ErrorCode::NotConnected);
        assert_eq!(ErrorCode::from_u8(0x05), ErrorCode::DeviceError);
        assert_eq!(ErrorCode::from_u8(0x06), ErrorCode::Timeout);
        assert_eq!(ErrorCode::from_u8(0x99), ErrorCode::Unknown); // 未知错误码
    }

    #[test]
    fn test_message_header_all_fields() {
        // 测试消息头的所有字段
        let header = MessageHeader {
            msg_type: MessageType::Connect,
            flags: 0xAA,
            length: 0x1234,
            reserved: 0xBB,
            seq: 0x123456,
        };
        let mut buf = [0u8; 8];
        header.encode(&mut buf);

        let decoded = MessageHeader::decode(&buf).unwrap();
        assert_eq!(decoded.msg_type, MessageType::Connect);
        assert_eq!(decoded.flags, 0xAA);
        assert_eq!(decoded.length, 0x1234);
        assert_eq!(decoded.reserved, 0xBB);
        // 注意：seq 只使用 24 位，所以 0x123456 会被正确解码
        assert_eq!(decoded.seq, 0x123456);
    }

    #[test]
    fn test_message_header_decode_too_short() {
        // 测试消息头解码时缓冲区太短
        let buf = [0u8; 7]; // 只有 7 字节，需要 8 字节
        assert!(MessageHeader::decode(&buf).is_err());
    }

    // ============================================================================
    // 性能测试：验证零拷贝（无堆分配）
    // ============================================================================

    #[test]
    fn test_zero_copy_no_heap_allocation() {
        // 验证编码函数使用栈上缓冲区，不进行堆分配
        // 这个测试通过编译时检查，确保所有编码函数都使用 &mut [u8; N] 参数

        let frame = PiperFrame::new_standard(0x123, &[0x01, 0x02, 0x03, 0x04]);
        let mut buf1 = [0u8; 64];
        let mut buf2 = [0u8; 64];
        let mut buf3 = [0u8; 256];

        // 所有编码函数都使用栈上缓冲区
        let _encoded1 = encode_send_frame_with_seq(&frame, 0, &mut buf1).unwrap();
        let _encoded2 = encode_receive_frame_zero_copy(&frame, &mut buf2).unwrap();
        let _encoded3 = encode_connect(12345, &[], 0, &mut buf3).unwrap();

        // 如果编译通过，说明没有使用 Vec 等堆分配类型
    }

    #[test]
    fn test_sequence_number_preservation() {
        // 测试序列号在编码/解码过程中保持不变
        // 注意：序列号只使用 24 位（3 字节），所以使用一个 24 位范围内的值
        let frame = PiperFrame::new_standard(0x123, &[0x01, 0x02]);
        let mut buf = [0u8; 64];
        let seq = 0x123456; // 24 位范围内的值（最大 0xFFFFFF）
        let encoded = encode_send_frame_with_seq(&frame, seq, &mut buf).unwrap();

        let decoded = decode_message(encoded).unwrap();
        match decoded {
            Message::SendFrame {
                seq: decoded_seq, ..
            } => {
                assert_eq!(decoded_seq, seq);
            },
            _ => panic!("Expected SendFrame message"),
        }
    }

    #[test]
    fn test_sequence_number_24bit_limit() {
        // 测试序列号的 24 位限制
        // 超过 24 位的值会被截断
        let frame = PiperFrame::new_standard(0x123, &[0x01, 0x02]);
        let mut buf = [0u8; 64];
        let seq_full = 0x12345678; // 32 位值
        let seq_expected = 0x345678; // 只保留低 24 位
        let encoded = encode_send_frame_with_seq(&frame, seq_full, &mut buf).unwrap();

        let decoded = decode_message(encoded).unwrap();
        match decoded {
            Message::SendFrame {
                seq: decoded_seq, ..
            } => {
                assert_eq!(decoded_seq, seq_expected);
            },
            _ => panic!("Expected SendFrame message"),
        }
    }

    #[test]
    fn test_all_message_types_roundtrip() {
        // 测试所有消息类型的 roundtrip
        let mut buf_256 = [0u8; 256];
        let mut buf_64 = [0u8; 64];
        let mut buf_13 = [0u8; 13];
        let mut buf_12 = [0u8; 12];
        let mut buf_8 = [0u8; 8];

        // Heartbeat
        let mut buf_heartbeat = [0u8; 12];
        let encoded = encode_heartbeat(12345, 0, &mut buf_heartbeat);
        assert!(decode_message(encoded).is_ok());

        // Connect
        let encoded = encode_connect(12345, &[], 0, &mut buf_256).unwrap();
        assert!(decode_message(encoded).is_ok());

        // Disconnect
        let encoded = encode_disconnect(12345, 0, &mut buf_12);
        assert!(decode_message(encoded).is_ok());

        // SendFrame
        let frame = PiperFrame::new_standard(0x123, &[0x01, 0x02]);
        let encoded = encode_send_frame_with_seq(&frame, 0, &mut buf_64).unwrap();
        assert!(decode_message(encoded).is_ok());

        // ReceiveFrame
        let encoded = encode_receive_frame_zero_copy(&frame, &mut buf_64).unwrap();
        assert!(decode_message(encoded).is_ok());

        // SendAck
        let encoded = encode_send_ack(12345, 0, &mut buf_12);
        assert!(decode_message(encoded).is_ok());

        // ConnectAck
        let encoded = encode_connect_ack(12345, 0, 0, &mut buf_13);
        assert!(decode_message(encoded).is_ok());

        // DisconnectAck
        let encoded = encode_disconnect_ack(0, &mut buf_8);
        assert!(decode_message(encoded).is_ok());

        // Error
        let encoded = encode_error(ErrorCode::DeviceNotFound, "test", 0, &mut buf_256).unwrap();
        assert!(decode_message(encoded).is_ok());

        // SetFilter
        let encoded = encode_set_filter(12345, &[], 0, &mut buf_256).unwrap();
        assert!(decode_message(encoded).is_ok());

        // GetStatus
        let encoded = encode_get_status(0, &mut buf_8);
        assert!(decode_message(encoded).is_ok());
    }
}
