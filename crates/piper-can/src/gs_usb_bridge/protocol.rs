//! Stream-oriented GS-USB bridge protocol.
//!
//! The bridge uses length-prefixed binary frames over UnixStream/TCP. Requests
//! and responses are correlated by full-width `u32` request ids. Events are
//! asynchronous and do not carry request ids. Filter-bearing client requests use
//! the v3 tags and schema: `Hello` is tag `0x09`, `SetFilters` is tag `0x0A`,
//! and each filter carries an explicit CAN ID format plus typed bounds.

use crate::{CanData, CanId, ExtendedCanId, FrameError, PiperFrame, StandardCanId};
use rand::random;
use std::io::{self, Read, Write};

pub const SESSION_TOKEN_LEN: usize = 16;
pub const MAX_PAYLOAD_LEN: usize = 8 * 1024;

const TAG_GET_STATUS: u8 = 0x02;
const TAG_SET_RAW_FRAME_TAP: u8 = 0x04;
const TAG_ACQUIRE_WRITER_LEASE: u8 = 0x05;
const TAG_RELEASE_WRITER_LEASE: u8 = 0x06;
const TAG_SEND_FRAME: u8 = 0x07;
const TAG_PING: u8 = 0x08;
const TAG_HELLO_V3: u8 = 0x09;
const TAG_SET_FILTERS_V3: u8 = 0x0A;

const TAG_HELLO_ACK: u8 = 0x81;
const TAG_OK: u8 = 0x82;
const TAG_ERROR: u8 = 0x83;
const TAG_STATUS_RESPONSE: u8 = 0x84;
const TAG_LEASE_GRANTED: u8 = 0x85;
const TAG_LEASE_DENIED: u8 = 0x86;

const TAG_EVENT_RECEIVE_FRAME: u8 = 0xC1;
const TAG_EVENT_GAP: u8 = 0xC2;
const TAG_EVENT_SESSION_REPLACED: u8 = 0xC3;
const TAG_EVENT_LEASE_REVOKED: u8 = 0xC4;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SessionToken([u8; SESSION_TOKEN_LEN]);

impl SessionToken {
    pub const fn new(bytes: [u8; SESSION_TOKEN_LEN]) -> Self {
        Self(bytes)
    }

    pub fn random() -> Self {
        Self(random())
    }

    pub const fn as_bytes(&self) -> &[u8; SESSION_TOKEN_LEN] {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum BridgeRole {
    Observer = 0,
    WriterCandidate = 1,
}

impl BridgeRole {
    fn from_u8(value: u8) -> Result<Self, ProtocolError> {
        match value {
            0 => Ok(Self::Observer),
            1 => Ok(Self::WriterCandidate),
            _ => Err(ProtocolError::InvalidData("invalid bridge role")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum BridgeDeviceState {
    Disconnected = 0,
    Connected = 1,
    Reconnecting = 2,
}

impl BridgeDeviceState {
    fn from_u8(value: u8) -> Result<Self, ProtocolError> {
        match value {
            0 => Ok(Self::Disconnected),
            1 => Ok(Self::Connected),
            2 => Ok(Self::Reconnecting),
            _ => Err(ProtocolError::InvalidData("invalid device state")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ErrorCode {
    Unknown = 0,
    NotConnected = 1,
    InvalidMessage = 2,
    PermissionDenied = 3,
    Busy = 4,
    Timeout = 5,
    DeviceNotFound = 6,
    DeviceBusy = 7,
    DeviceError = 8,
    ProtocolError = 9,
}

impl ErrorCode {
    fn from_u8(value: u8) -> Result<Self, ProtocolError> {
        match value {
            0 => Ok(Self::Unknown),
            1 => Ok(Self::NotConnected),
            2 => Ok(Self::InvalidMessage),
            3 => Ok(Self::PermissionDenied),
            4 => Ok(Self::Busy),
            5 => Ok(Self::Timeout),
            6 => Ok(Self::DeviceNotFound),
            7 => Ok(Self::DeviceBusy),
            8 => Ok(Self::DeviceError),
            9 => Ok(Self::ProtocolError),
            _ => Err(ProtocolError::InvalidData("invalid error code")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CanIdFilter {
    kind: CanIdFilterKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CanIdFilterKind {
    Standard {
        min: StandardCanId,
        max: StandardCanId,
    },
    Extended {
        min: ExtendedCanId,
        max: ExtendedCanId,
    },
}

impl CanIdFilter {
    pub fn standard(min: StandardCanId, max: StandardCanId) -> Result<Self, ProtocolError> {
        if min > max {
            return Err(ProtocolError::InvalidData(
                "invalid local bridge filter range",
            ));
        }
        Ok(Self {
            kind: CanIdFilterKind::Standard { min, max },
        })
    }

    pub fn extended(min: ExtendedCanId, max: ExtendedCanId) -> Result<Self, ProtocolError> {
        if min > max {
            return Err(ProtocolError::InvalidData(
                "invalid local bridge filter range",
            ));
        }
        Ok(Self {
            kind: CanIdFilterKind::Extended { min, max },
        })
    }

    pub fn matches(&self, id: CanId) -> bool {
        match (self.kind, id) {
            (CanIdFilterKind::Standard { min, max }, CanId::Standard(id)) => min <= id && id <= max,
            (CanIdFilterKind::Extended { min, max }, CanId::Extended(id)) => min <= id && id <= max,
            _ => false,
        }
    }

    pub fn bounds(&self) -> (CanId, CanId) {
        match self.kind {
            CanIdFilterKind::Standard { min, max } => (CanId::from(min), CanId::from(max)),
            CanIdFilterKind::Extended { min, max } => (CanId::from(min), CanId::from(max)),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeStatus {
    pub device_state: BridgeDeviceState,
    pub rx_fps_x1000: u32,
    pub tx_fps_x1000: u32,
    pub ipc_out_fps_x1000: u32,
    pub ipc_in_fps_x1000: u32,
    pub health_score: u8,
    pub usb_stall_count: u64,
    pub can_bus_off_count: u64,
    pub can_error_passive_count: u64,
    pub cpu_usage_percent: u8,
    pub session_count: u32,
    pub queue_drop_count: u64,
    pub inactive_enqueue_count: u64,
    pub session_replacement_discard_count: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ClientRequest {
    Hello {
        request_id: u32,
        session_token: SessionToken,
        filters: Vec<CanIdFilter>,
    },
    GetStatus {
        request_id: u32,
    },
    SetFilters {
        request_id: u32,
        filters: Vec<CanIdFilter>,
    },
    SetRawFrameTap {
        request_id: u32,
        enabled: bool,
    },
    AcquireWriterLease {
        request_id: u32,
        timeout_ms: u32,
    },
    ReleaseWriterLease {
        request_id: u32,
    },
    SendFrame {
        request_id: u32,
        frame: PiperFrame,
    },
    Ping {
        request_id: u32,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum ServerResponse {
    HelloAck {
        request_id: u32,
        session_id: u32,
        role_granted: BridgeRole,
    },
    Ok {
        request_id: u32,
    },
    Error {
        request_id: u32,
        code: ErrorCode,
        message: String,
    },
    StatusResponse {
        request_id: u32,
        status: BridgeStatus,
    },
    LeaseGranted {
        request_id: u32,
        session_id: u32,
    },
    LeaseDenied {
        request_id: u32,
        holder_session_id: Option<u32>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum BridgeEvent {
    ReceiveFrame(PiperFrame),
    Gap { dropped: u32 },
    SessionReplaced,
    LeaseRevoked,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ServerMessage {
    Response(ServerResponse),
    Event(BridgeEvent),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProtocolError {
    Io {
        kind: io::ErrorKind,
        message: String,
    },
    TooShort,
    FrameTooLarge(usize),
    InvalidTag(u8),
    InvalidData(&'static str),
    Utf8,
}

impl std::fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io { message, .. } => write!(f, "io error: {message}"),
            Self::TooShort => write!(f, "message too short"),
            Self::FrameTooLarge(size) => write!(f, "frame too large: {size}"),
            Self::InvalidTag(tag) => write!(f, "invalid tag: {tag:#x}"),
            Self::InvalidData(message) => write!(f, "invalid data: {message}"),
            Self::Utf8 => write!(f, "invalid utf-8"),
        }
    }
}

impl std::error::Error for ProtocolError {}

impl From<io::Error> for ProtocolError {
    fn from(value: io::Error) -> Self {
        Self::Io {
            kind: value.kind(),
            message: value.to_string(),
        }
    }
}

fn put_u8(buf: &mut Vec<u8>, value: u8) {
    buf.push(value);
}

fn put_u16(buf: &mut Vec<u8>, value: u16) {
    buf.extend_from_slice(&value.to_le_bytes());
}

fn put_u32(buf: &mut Vec<u8>, value: u32) {
    buf.extend_from_slice(&value.to_le_bytes());
}

fn put_u64(buf: &mut Vec<u8>, value: u64) {
    buf.extend_from_slice(&value.to_le_bytes());
}

fn decode_bool(value: u8, field: &'static str) -> Result<bool, ProtocolError> {
    match value {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(ProtocolError::InvalidData(field)),
    }
}

fn put_token(buf: &mut Vec<u8>, token: SessionToken) {
    buf.extend_from_slice(token.as_bytes());
}

fn put_filters(buf: &mut Vec<u8>, filters: &[CanIdFilter]) -> Result<(), ProtocolError> {
    let count =
        u16::try_from(filters.len()).map_err(|_| ProtocolError::InvalidData("too many filters"))?;
    put_u16(buf, count);
    for filter in filters {
        match filter.kind {
            CanIdFilterKind::Standard { min, max } => {
                put_u8(buf, 0);
                put_u32(buf, min.raw() as u32);
                put_u32(buf, max.raw() as u32);
            },
            CanIdFilterKind::Extended { min, max } => {
                put_u8(buf, 1);
                put_u32(buf, min.raw());
                put_u32(buf, max.raw());
            },
        }
    }
    Ok(())
}

fn put_string(buf: &mut Vec<u8>, value: &str) -> Result<(), ProtocolError> {
    let len =
        u16::try_from(value.len()).map_err(|_| ProtocolError::InvalidData("string too long"))?;
    put_u16(buf, len);
    buf.extend_from_slice(value.as_bytes());
    Ok(())
}

fn put_frame_request(buf: &mut Vec<u8>, frame: &PiperFrame) {
    put_u32(buf, frame.raw_id());
    put_u8(buf, u8::from(frame.is_extended()));
    put_u8(buf, frame.dlc());
    buf.extend_from_slice(frame.data_padded());
}

fn put_frame_event(buf: &mut Vec<u8>, frame: &PiperFrame) {
    put_frame_request(buf, frame);
    put_u64(buf, frame.timestamp_us());
}

fn map_frame_error(context: &'static str, _error: FrameError) -> ProtocolError {
    ProtocolError::InvalidData(context)
}

struct Cursor<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    fn remaining(&self) -> usize {
        self.buf.len().saturating_sub(self.pos)
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8], ProtocolError> {
        if self.remaining() < len {
            return Err(ProtocolError::TooShort);
        }
        let start = self.pos;
        self.pos += len;
        Ok(&self.buf[start..start + len])
    }

    fn u8(&mut self) -> Result<u8, ProtocolError> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, ProtocolError> {
        let bytes = self.take(2)?;
        Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
    }

    fn u32(&mut self) -> Result<u32, ProtocolError> {
        let bytes = self.take(4)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn u64(&mut self) -> Result<u64, ProtocolError> {
        let bytes = self.take(8)?;
        Ok(u64::from_le_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]))
    }

    fn token(&mut self) -> Result<SessionToken, ProtocolError> {
        let bytes = self.take(SESSION_TOKEN_LEN)?;
        let mut token = [0u8; SESSION_TOKEN_LEN];
        token.copy_from_slice(bytes);
        Ok(SessionToken::new(token))
    }

    fn filters(&mut self) -> Result<Vec<CanIdFilter>, ProtocolError> {
        let count = self.u16()? as usize;
        let mut filters = Vec::with_capacity(count);
        for _ in 0..count {
            let format = self.u8()?;
            let min = self.u32()?;
            let max = self.u32()?;
            let filter = match format {
                0 => Self::wire_filter(CanIdFilter::standard(
                    StandardCanId::new(min)
                        .map_err(|err| map_frame_error("invalid bridge standard filter id", err))?,
                    StandardCanId::new(max)
                        .map_err(|err| map_frame_error("invalid bridge standard filter id", err))?,
                ))?,
                1 => Self::wire_filter(CanIdFilter::extended(
                    ExtendedCanId::new(min)
                        .map_err(|err| map_frame_error("invalid bridge extended filter id", err))?,
                    ExtendedCanId::new(max)
                        .map_err(|err| map_frame_error("invalid bridge extended filter id", err))?,
                ))?,
                _ => return Err(ProtocolError::InvalidData("invalid bridge filter format")),
            };
            filters.push(filter);
        }
        Ok(filters)
    }

    fn wire_filter(
        result: Result<CanIdFilter, ProtocolError>,
    ) -> Result<CanIdFilter, ProtocolError> {
        result.map_err(|error| match error {
            ProtocolError::InvalidData("invalid local bridge filter range") => {
                ProtocolError::InvalidData("invalid bridge wire filter range")
            },
            other => other,
        })
    }

    fn string(&mut self) -> Result<String, ProtocolError> {
        let len = self.u16()? as usize;
        let bytes = self.take(len)?;
        String::from_utf8(bytes.to_vec()).map_err(|_| ProtocolError::Utf8)
    }

    fn frame_request(&mut self) -> Result<PiperFrame, ProtocolError> {
        let id = self.u32()?;
        let is_extended = decode_bool(self.u8()?, "invalid bridge frame format")?;
        let len = self.u8()?;
        let mut data = [0u8; 8];
        data.copy_from_slice(self.take(8)?);
        CanData::validate_canonical_padding(data, len)
            .map_err(|err| map_frame_error("invalid bridge frame data padding", err))?;
        let data = CanData::from_padded(data, len)
            .map_err(|err| map_frame_error("invalid bridge frame data", err))?;
        if is_extended {
            let id = ExtendedCanId::new(id)
                .map_err(|err| map_frame_error("invalid bridge extended can id", err))?;
            Ok(PiperFrame::extended(id, data))
        } else {
            let id = StandardCanId::new(id)
                .map_err(|err| map_frame_error("invalid bridge standard can id", err))?;
            Ok(PiperFrame::standard(id, data))
        }
    }

    fn frame_event(&mut self) -> Result<PiperFrame, ProtocolError> {
        let frame = self.frame_request()?;
        Ok(frame.with_timestamp_us(self.u64()?))
    }
}

fn encode_payload_from_request(message: &ClientRequest) -> Result<Vec<u8>, ProtocolError> {
    let mut buf = Vec::with_capacity(128);
    match message {
        ClientRequest::Hello {
            request_id,
            session_token,
            filters,
        } => {
            put_u8(&mut buf, TAG_HELLO_V3);
            put_u32(&mut buf, *request_id);
            put_token(&mut buf, *session_token);
            put_filters(&mut buf, filters)?;
        },
        ClientRequest::GetStatus { request_id } => {
            put_u8(&mut buf, TAG_GET_STATUS);
            put_u32(&mut buf, *request_id);
        },
        ClientRequest::SetFilters {
            request_id,
            filters,
        } => {
            put_u8(&mut buf, TAG_SET_FILTERS_V3);
            put_u32(&mut buf, *request_id);
            put_filters(&mut buf, filters)?;
        },
        ClientRequest::SetRawFrameTap {
            request_id,
            enabled,
        } => {
            put_u8(&mut buf, TAG_SET_RAW_FRAME_TAP);
            put_u32(&mut buf, *request_id);
            put_u8(&mut buf, u8::from(*enabled));
        },
        ClientRequest::AcquireWriterLease {
            request_id,
            timeout_ms,
        } => {
            put_u8(&mut buf, TAG_ACQUIRE_WRITER_LEASE);
            put_u32(&mut buf, *request_id);
            put_u32(&mut buf, *timeout_ms);
        },
        ClientRequest::ReleaseWriterLease { request_id } => {
            put_u8(&mut buf, TAG_RELEASE_WRITER_LEASE);
            put_u32(&mut buf, *request_id);
        },
        ClientRequest::SendFrame { request_id, frame } => {
            put_u8(&mut buf, TAG_SEND_FRAME);
            put_u32(&mut buf, *request_id);
            put_frame_request(&mut buf, frame);
        },
        ClientRequest::Ping { request_id } => {
            put_u8(&mut buf, TAG_PING);
            put_u32(&mut buf, *request_id);
        },
    }
    Ok(buf)
}

fn encode_payload_from_server(message: &ServerMessage) -> Result<Vec<u8>, ProtocolError> {
    let mut buf = Vec::with_capacity(128);
    match message {
        ServerMessage::Response(ServerResponse::HelloAck {
            request_id,
            session_id,
            role_granted,
        }) => {
            put_u8(&mut buf, TAG_HELLO_ACK);
            put_u32(&mut buf, *request_id);
            put_u32(&mut buf, *session_id);
            put_u8(&mut buf, *role_granted as u8);
        },
        ServerMessage::Response(ServerResponse::Ok { request_id }) => {
            put_u8(&mut buf, TAG_OK);
            put_u32(&mut buf, *request_id);
        },
        ServerMessage::Response(ServerResponse::Error {
            request_id,
            code,
            message,
        }) => {
            put_u8(&mut buf, TAG_ERROR);
            put_u32(&mut buf, *request_id);
            put_u8(&mut buf, *code as u8);
            put_string(&mut buf, message)?;
        },
        ServerMessage::Response(ServerResponse::StatusResponse { request_id, status }) => {
            put_u8(&mut buf, TAG_STATUS_RESPONSE);
            put_u32(&mut buf, *request_id);
            put_u8(&mut buf, status.device_state as u8);
            put_u32(&mut buf, status.rx_fps_x1000);
            put_u32(&mut buf, status.tx_fps_x1000);
            put_u32(&mut buf, status.ipc_out_fps_x1000);
            put_u32(&mut buf, status.ipc_in_fps_x1000);
            put_u8(&mut buf, status.health_score);
            put_u64(&mut buf, status.usb_stall_count);
            put_u64(&mut buf, status.can_bus_off_count);
            put_u64(&mut buf, status.can_error_passive_count);
            put_u8(&mut buf, status.cpu_usage_percent);
            put_u32(&mut buf, status.session_count);
            put_u64(&mut buf, status.queue_drop_count);
            put_u64(&mut buf, status.inactive_enqueue_count);
            put_u64(&mut buf, status.session_replacement_discard_count);
        },
        ServerMessage::Response(ServerResponse::LeaseGranted {
            request_id,
            session_id,
        }) => {
            put_u8(&mut buf, TAG_LEASE_GRANTED);
            put_u32(&mut buf, *request_id);
            put_u32(&mut buf, *session_id);
        },
        ServerMessage::Response(ServerResponse::LeaseDenied {
            request_id,
            holder_session_id,
        }) => {
            put_u8(&mut buf, TAG_LEASE_DENIED);
            put_u32(&mut buf, *request_id);
            put_u8(&mut buf, u8::from(holder_session_id.is_some()));
            put_u32(&mut buf, holder_session_id.unwrap_or_default());
        },
        ServerMessage::Event(BridgeEvent::ReceiveFrame(frame)) => {
            put_u8(&mut buf, TAG_EVENT_RECEIVE_FRAME);
            put_frame_event(&mut buf, frame);
        },
        ServerMessage::Event(BridgeEvent::Gap { dropped }) => {
            put_u8(&mut buf, TAG_EVENT_GAP);
            put_u32(&mut buf, *dropped);
        },
        ServerMessage::Event(BridgeEvent::SessionReplaced) => {
            put_u8(&mut buf, TAG_EVENT_SESSION_REPLACED);
        },
        ServerMessage::Event(BridgeEvent::LeaseRevoked) => {
            put_u8(&mut buf, TAG_EVENT_LEASE_REVOKED);
        },
    }
    Ok(buf)
}

pub fn encode_client_request(message: &ClientRequest) -> Result<Vec<u8>, ProtocolError> {
    let payload = encode_payload_from_request(message)?;
    encode_framed_payload(&payload)
}

pub fn encode_server_message(message: &ServerMessage) -> Result<Vec<u8>, ProtocolError> {
    let payload = encode_payload_from_server(message)?;
    encode_framed_payload(&payload)
}

fn encode_framed_payload(payload: &[u8]) -> Result<Vec<u8>, ProtocolError> {
    let len = u32::try_from(payload.len())
        .map_err(|_| ProtocolError::InvalidData("payload too large"))?;
    let mut framed = Vec::with_capacity(payload.len() + 4);
    framed.extend_from_slice(&len.to_le_bytes());
    framed.extend_from_slice(payload);
    Ok(framed)
}

pub fn decode_client_request(payload: &[u8]) -> Result<ClientRequest, ProtocolError> {
    let mut cursor = Cursor::new(payload);
    let tag = cursor.u8()?;
    let request_id = cursor.u32()?;
    let message = match tag {
        TAG_HELLO_V3 => ClientRequest::Hello {
            request_id,
            session_token: cursor.token()?,
            filters: cursor.filters()?,
        },
        TAG_GET_STATUS => ClientRequest::GetStatus { request_id },
        TAG_SET_FILTERS_V3 => ClientRequest::SetFilters {
            request_id,
            filters: cursor.filters()?,
        },
        TAG_SET_RAW_FRAME_TAP => ClientRequest::SetRawFrameTap {
            request_id,
            enabled: decode_bool(cursor.u8()?, "set raw frame tap enabled")?,
        },
        TAG_ACQUIRE_WRITER_LEASE => ClientRequest::AcquireWriterLease {
            request_id,
            timeout_ms: cursor.u32()?,
        },
        TAG_RELEASE_WRITER_LEASE => ClientRequest::ReleaseWriterLease { request_id },
        TAG_SEND_FRAME => ClientRequest::SendFrame {
            request_id,
            frame: cursor.frame_request()?,
        },
        TAG_PING => ClientRequest::Ping { request_id },
        other => return Err(ProtocolError::InvalidTag(other)),
    };
    if cursor.remaining() != 0 {
        return Err(ProtocolError::InvalidData("trailing bytes"));
    }
    Ok(message)
}

pub fn decode_server_message(payload: &[u8]) -> Result<ServerMessage, ProtocolError> {
    let mut cursor = Cursor::new(payload);
    let tag = cursor.u8()?;
    let message = match tag {
        TAG_HELLO_ACK => {
            let request_id = cursor.u32()?;
            ServerMessage::Response(ServerResponse::HelloAck {
                request_id,
                session_id: cursor.u32()?,
                role_granted: BridgeRole::from_u8(cursor.u8()?)?,
            })
        },
        TAG_OK => {
            let request_id = cursor.u32()?;
            ServerMessage::Response(ServerResponse::Ok { request_id })
        },
        TAG_ERROR => {
            let request_id = cursor.u32()?;
            ServerMessage::Response(ServerResponse::Error {
                request_id,
                code: ErrorCode::from_u8(cursor.u8()?)?,
                message: cursor.string()?,
            })
        },
        TAG_STATUS_RESPONSE => {
            let request_id = cursor.u32()?;
            ServerMessage::Response(ServerResponse::StatusResponse {
                request_id,
                status: BridgeStatus {
                    device_state: BridgeDeviceState::from_u8(cursor.u8()?)?,
                    rx_fps_x1000: cursor.u32()?,
                    tx_fps_x1000: cursor.u32()?,
                    ipc_out_fps_x1000: cursor.u32()?,
                    ipc_in_fps_x1000: cursor.u32()?,
                    health_score: cursor.u8()?,
                    usb_stall_count: cursor.u64()?,
                    can_bus_off_count: cursor.u64()?,
                    can_error_passive_count: cursor.u64()?,
                    cpu_usage_percent: cursor.u8()?,
                    session_count: cursor.u32()?,
                    queue_drop_count: cursor.u64()?,
                    inactive_enqueue_count: cursor.u64()?,
                    session_replacement_discard_count: cursor.u64()?,
                },
            })
        },
        TAG_LEASE_GRANTED => {
            let request_id = cursor.u32()?;
            ServerMessage::Response(ServerResponse::LeaseGranted {
                request_id,
                session_id: cursor.u32()?,
            })
        },
        TAG_LEASE_DENIED => {
            let request_id = cursor.u32()?;
            let has_holder = decode_bool(cursor.u8()?, "lease denied has holder")?;
            let holder = cursor.u32()?;
            ServerMessage::Response(ServerResponse::LeaseDenied {
                request_id,
                holder_session_id: if has_holder { Some(holder) } else { None },
            })
        },
        TAG_EVENT_RECEIVE_FRAME => {
            ServerMessage::Event(BridgeEvent::ReceiveFrame(cursor.frame_event()?))
        },
        TAG_EVENT_GAP => ServerMessage::Event(BridgeEvent::Gap {
            dropped: cursor.u32()?,
        }),
        TAG_EVENT_SESSION_REPLACED => ServerMessage::Event(BridgeEvent::SessionReplaced),
        TAG_EVENT_LEASE_REVOKED => ServerMessage::Event(BridgeEvent::LeaseRevoked),
        other => return Err(ProtocolError::InvalidTag(other)),
    };
    if cursor.remaining() != 0 {
        return Err(ProtocolError::InvalidData("trailing bytes"));
    }
    Ok(message)
}

pub fn write_framed<W: Write>(writer: &mut W, bytes: &[u8]) -> Result<(), ProtocolError> {
    writer.write_all(bytes)?;
    writer.flush()?;
    Ok(())
}

pub fn read_framed<R: Read>(reader: &mut R) -> Result<Vec<u8>, ProtocolError> {
    let mut len_buf = [0u8; 4];
    reader.read_exact(&mut len_buf)?;
    let len = u32::from_le_bytes(len_buf) as usize;
    if len > MAX_PAYLOAD_LEN {
        return Err(ProtocolError::FrameTooLarge(len));
    }
    let mut payload = vec![0u8; len];
    reader.read_exact(&mut payload)?;
    Ok(payload)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CanId, ExtendedCanId, StandardCanId};

    const TEST_TOKEN: SessionToken = SessionToken::new([0xAB; SESSION_TOKEN_LEN]);
    const HELLO_V3_STANDARD_FILTER_BYTES: &[u8] = &[
        0x09, 0x07, 0x00, 0x00, 0x00, 0xAB, 0xAB, 0xAB, 0xAB, 0xAB, 0xAB, 0xAB, 0xAB, 0xAB, 0xAB,
        0xAB, 0xAB, 0xAB, 0xAB, 0xAB, 0xAB, 0x01, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0xFF, 0x01,
        0x00, 0x00,
    ];
    const SET_FILTERS_V3_EXTENDED_FILTER_BYTES: &[u8] = &[
        0x0A, 0x08, 0x00, 0x00, 0x00, 0x01, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0xFF, 0x01, 0x00,
        0x00,
    ];

    fn sample_status() -> BridgeStatus {
        BridgeStatus {
            device_state: BridgeDeviceState::Connected,
            rx_fps_x1000: 123_000,
            tx_fps_x1000: 98_000,
            ipc_out_fps_x1000: 77_000,
            ipc_in_fps_x1000: 66_000,
            health_score: 91,
            usb_stall_count: 1,
            can_bus_off_count: 2,
            can_error_passive_count: 3,
            cpu_usage_percent: 12,
            session_count: 4,
            queue_drop_count: 5,
            inactive_enqueue_count: 6,
            session_replacement_discard_count: 7,
        }
    }

    #[test]
    fn test_client_request_roundtrip() {
        let request = ClientRequest::Hello {
            request_id: 7,
            session_token: TEST_TOKEN,
            filters: vec![
                CanIdFilter::standard(
                    StandardCanId::new(0x100).unwrap(),
                    StandardCanId::new(0x1FF).unwrap(),
                )
                .unwrap(),
            ],
        };
        let encoded = encode_client_request(&request).unwrap();
        let decoded = decode_client_request(&encoded[4..]).unwrap();
        assert_eq!(decoded, request);
    }

    #[test]
    fn rejects_noncanonical_boolean_values() {
        let payload = [TAG_SET_RAW_FRAME_TAP, 11, 0, 0, 0, 2];

        let error = decode_client_request(&payload).unwrap_err();

        assert!(matches!(
            error,
            ProtocolError::InvalidData("set raw frame tap enabled")
        ));
    }

    #[test]
    fn hello_v3_filter_bytes_are_locked() {
        let request = ClientRequest::Hello {
            request_id: 7,
            session_token: TEST_TOKEN,
            filters: vec![
                CanIdFilter::standard(
                    StandardCanId::new(0x100).unwrap(),
                    StandardCanId::new(0x1FF).unwrap(),
                )
                .unwrap(),
            ],
        };

        let encoded = encode_client_request(&request).unwrap();

        assert_eq!(&encoded[4..], HELLO_V3_STANDARD_FILTER_BYTES);
        assert_eq!(
            decode_client_request(HELLO_V3_STANDARD_FILTER_BYTES).unwrap(),
            request
        );
    }

    #[test]
    fn set_filters_v3_filter_bytes_are_locked() {
        let request = ClientRequest::SetFilters {
            request_id: 8,
            filters: vec![
                CanIdFilter::extended(
                    ExtendedCanId::new(0x100).unwrap(),
                    ExtendedCanId::new(0x1FF).unwrap(),
                )
                .unwrap(),
            ],
        };

        let encoded = encode_client_request(&request).unwrap();

        assert_eq!(&encoded[4..], SET_FILTERS_V3_EXTENDED_FILTER_BYTES);
        assert_eq!(
            decode_client_request(SET_FILTERS_V3_EXTENDED_FILTER_BYTES).unwrap(),
            request
        );
    }

    #[test]
    fn old_filter_tags_are_rejected() {
        let mut old_hello = Vec::new();
        put_u8(&mut old_hello, 0x01);
        put_u32(&mut old_hello, 7);
        put_token(&mut old_hello, TEST_TOKEN);
        put_u16(&mut old_hello, 0);

        let error = decode_client_request(&old_hello).unwrap_err();

        assert_eq!(error, ProtocolError::InvalidTag(0x01));
    }

    #[test]
    fn set_filters_old_tag_is_rejected() {
        let mut old_set_filters = Vec::new();
        put_u8(&mut old_set_filters, 0x03);
        put_u32(&mut old_set_filters, 8);
        put_u16(&mut old_set_filters, 0);

        let error = decode_client_request(&old_set_filters).unwrap_err();

        assert_eq!(error, ProtocolError::InvalidTag(0x03));
    }

    #[test]
    fn test_set_raw_frame_tap_roundtrip() {
        let request = ClientRequest::SetRawFrameTap {
            request_id: 11,
            enabled: true,
        };
        let encoded = encode_client_request(&request).unwrap();
        let decoded = decode_client_request(&encoded[4..]).unwrap();
        assert_eq!(decoded, request);
    }

    #[test]
    fn test_server_message_roundtrip() {
        let message = ServerMessage::Response(ServerResponse::StatusResponse {
            request_id: 9,
            status: sample_status(),
        });
        let encoded = encode_server_message(&message).unwrap();
        let decoded = decode_server_message(&encoded[4..]).unwrap();
        assert_eq!(decoded, message);
    }

    #[test]
    fn lease_denied_rejects_noncanonical_has_holder() {
        let mut payload = Vec::new();
        put_u8(&mut payload, TAG_LEASE_DENIED);
        put_u32(&mut payload, 9);
        put_u8(&mut payload, 2);
        put_u32(&mut payload, 123);

        let error = decode_server_message(&payload).unwrap_err();

        assert!(matches!(
            error,
            ProtocolError::InvalidData("lease denied has holder")
        ));
    }

    #[test]
    fn test_event_roundtrip() {
        let message = ServerMessage::Event(BridgeEvent::ReceiveFrame(
            PiperFrame::new_standard(0x123, [1, 2, 3, 4]).unwrap(),
        ));
        let encoded = encode_server_message(&message).unwrap();
        let decoded = decode_server_message(&encoded[4..]).unwrap();
        assert_eq!(decoded, message);
    }

    #[test]
    fn receive_frame_event_decode_preserves_timestamp() {
        let frame = PiperFrame::new_standard(0x123, [1, 2, 3, 4])
            .unwrap()
            .with_timestamp_us(0x0102_0304_0506_0708);
        let message = ServerMessage::Event(BridgeEvent::ReceiveFrame(frame));

        let encoded = encode_server_message(&message).unwrap();
        let decoded = decode_server_message(&encoded[4..]).unwrap();

        assert_eq!(decoded, message);
    }

    #[test]
    fn decode_send_frame_rejects_noncanonical_padding() {
        let mut payload = Vec::new();
        put_u8(&mut payload, TAG_SEND_FRAME);
        put_u32(&mut payload, 42);
        put_u32(&mut payload, 0x123);
        put_u8(&mut payload, 0);
        put_u8(&mut payload, 1);
        payload.extend_from_slice(&[0xAA, 0xBB, 0, 0, 0, 0, 0, 0]);

        let error = decode_client_request(&payload).unwrap_err();

        assert!(matches!(
            error,
            ProtocolError::InvalidData("invalid bridge frame data padding")
        ));
    }

    #[test]
    fn decode_send_frame_rejects_noncanonical_is_extended() {
        let mut payload = Vec::new();
        put_u8(&mut payload, TAG_SEND_FRAME);
        put_u32(&mut payload, 43);
        put_u32(&mut payload, 0x123);
        put_u8(&mut payload, 2);
        put_u8(&mut payload, 1);
        payload.extend_from_slice(&[0xAA, 0, 0, 0, 0, 0, 0, 0]);

        let error = decode_client_request(&payload).unwrap_err();

        assert!(matches!(
            error,
            ProtocolError::InvalidData("invalid bridge frame format")
        ));
    }

    #[test]
    fn decode_send_frame_rejects_invalid_standard_id() {
        let mut payload = Vec::new();
        put_u8(&mut payload, TAG_SEND_FRAME);
        put_u32(&mut payload, 44);
        put_u32(&mut payload, 0x800);
        put_u8(&mut payload, 0);
        put_u8(&mut payload, 1);
        payload.extend_from_slice(&[0xAA, 0, 0, 0, 0, 0, 0, 0]);

        let error = decode_client_request(&payload).unwrap_err();

        assert!(matches!(
            error,
            ProtocolError::InvalidData("invalid bridge standard can id")
        ));
    }

    #[test]
    fn decode_send_frame_rejects_invalid_extended_id() {
        let mut payload = Vec::new();
        put_u8(&mut payload, TAG_SEND_FRAME);
        put_u32(&mut payload, 45);
        put_u32(&mut payload, 0x2000_0000);
        put_u8(&mut payload, 1);
        put_u8(&mut payload, 1);
        payload.extend_from_slice(&[0xAA, 0, 0, 0, 0, 0, 0, 0]);

        let error = decode_client_request(&payload).unwrap_err();

        assert!(matches!(
            error,
            ProtocolError::InvalidData("invalid bridge extended can id")
        ));
    }

    #[test]
    fn decode_send_frame_rejects_invalid_dlc() {
        let mut payload = Vec::new();
        put_u8(&mut payload, TAG_SEND_FRAME);
        put_u32(&mut payload, 46);
        put_u32(&mut payload, 0x123);
        put_u8(&mut payload, 0);
        put_u8(&mut payload, 9);
        payload.extend_from_slice(&[0xAA, 0, 0, 0, 0, 0, 0, 0]);

        let error = decode_client_request(&payload).unwrap_err();

        assert!(matches!(
            error,
            ProtocolError::InvalidData("invalid bridge frame data padding")
        ));
    }

    #[test]
    fn send_frame_request_decode_discards_frame_timestamp() {
        let request = ClientRequest::SendFrame {
            request_id: 47,
            frame: PiperFrame::new_standard(0x123, [1, 2, 3]).unwrap().with_timestamp_us(999),
        };

        let encoded = encode_client_request(&request).unwrap();
        let ClientRequest::SendFrame { frame, .. } = decode_client_request(&encoded[4..]).unwrap()
        else {
            panic!("expected send frame request");
        };

        assert_eq!(frame.timestamp_us(), 0);
    }

    #[test]
    fn standard_and_extended_same_raw_id_match_different_filters() {
        let standard = CanIdFilter::standard(
            StandardCanId::new(0x123).unwrap(),
            StandardCanId::new(0x123).unwrap(),
        )
        .unwrap();
        let extended = CanIdFilter::extended(
            ExtendedCanId::new(0x123).unwrap(),
            ExtendedCanId::new(0x123).unwrap(),
        )
        .unwrap();

        assert!(standard.matches(CanId::standard(0x123).unwrap()));
        assert!(!standard.matches(CanId::extended(0x123).unwrap()));
        assert!(extended.matches(CanId::extended(0x123).unwrap()));
        assert!(!extended.matches(CanId::standard(0x123).unwrap()));
    }

    #[test]
    fn decode_filters_reject_invalid_format_values() {
        let mut payload = Vec::new();
        put_u8(&mut payload, 0x0A);
        put_u32(&mut payload, 8);
        put_u16(&mut payload, 1);
        put_u8(&mut payload, 2);
        put_u32(&mut payload, 0x100);
        put_u32(&mut payload, 0x1FF);

        let error = decode_client_request(&payload).unwrap_err();

        assert!(matches!(
            error,
            ProtocolError::InvalidData("invalid bridge filter format")
        ));
    }

    #[test]
    fn decode_filters_reject_invalid_ranges() {
        let mut payload = Vec::new();
        put_u8(&mut payload, 0x0A);
        put_u32(&mut payload, 8);
        put_u16(&mut payload, 1);
        put_u8(&mut payload, 0);
        put_u32(&mut payload, 0x200);
        put_u32(&mut payload, 0x100);

        let error = decode_client_request(&payload).unwrap_err();

        assert!(matches!(
            error,
            ProtocolError::InvalidData("invalid bridge wire filter range")
        ));
    }

    #[test]
    fn decode_extended_filters_reject_invalid_ranges() {
        let mut payload = Vec::new();
        put_u8(&mut payload, 0x0A);
        put_u32(&mut payload, 8);
        put_u16(&mut payload, 1);
        put_u8(&mut payload, 1);
        put_u32(&mut payload, 0x200);
        put_u32(&mut payload, 0x100);

        let error = decode_client_request(&payload).unwrap_err();

        assert!(matches!(
            error,
            ProtocolError::InvalidData("invalid bridge wire filter range")
        ));
    }

    #[test]
    fn local_filter_construction_rejects_invalid_ranges() {
        let error = CanIdFilter::standard(
            StandardCanId::new(0x200).unwrap(),
            StandardCanId::new(0x100).unwrap(),
        )
        .unwrap_err();

        assert_eq!(
            error,
            ProtocolError::InvalidData("invalid local bridge filter range")
        );
    }

    #[test]
    fn local_extended_filter_construction_rejects_invalid_ranges() {
        let error = CanIdFilter::extended(
            ExtendedCanId::new(0x200).unwrap(),
            ExtendedCanId::new(0x100).unwrap(),
        )
        .unwrap_err();

        assert_eq!(
            error,
            ProtocolError::InvalidData("invalid local bridge filter range")
        );
    }

    #[test]
    fn valid_filters_expose_typed_bounds_for_encoding() {
        let filter = CanIdFilter::extended(
            ExtendedCanId::new(0x100).unwrap(),
            ExtendedCanId::new(0x1FF).unwrap(),
        )
        .unwrap();

        assert_eq!(
            filter.bounds(),
            (
                CanId::extended(0x100).unwrap(),
                CanId::extended(0x1FF).unwrap()
            )
        );
    }

    #[test]
    fn test_read_write_framed_roundtrip() {
        let request = ClientRequest::Ping { request_id: 3 };
        let encoded = encode_client_request(&request).unwrap();
        let mut cursor = std::io::Cursor::new(encoded.clone());
        let framed = read_framed(&mut cursor).unwrap();
        assert_eq!(&encoded[4..], framed.as_slice());
    }

    #[test]
    fn test_frame_too_large_rejected() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&(MAX_PAYLOAD_LEN as u32 + 1).to_le_bytes());
        let error = read_framed(&mut std::io::Cursor::new(bytes)).unwrap_err();
        assert!(matches!(error, ProtocolError::FrameTooLarge(_)));
    }
}
