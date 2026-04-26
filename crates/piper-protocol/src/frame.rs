use std::cmp::Ordering;

use thiserror::Error;

pub(crate) mod protocol_ids;

pub const STANDARD_CAN_ID_MAX: u32 = 0x7FF;
pub const EXTENDED_CAN_ID_MAX: u32 = 0x1FFF_FFFF;
pub const CAN_DATA_MAX_LEN: usize = 8;

#[derive(Error, Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameError {
    #[error("invalid standard CAN ID: 0x{id:X}")]
    InvalidStandardId { id: u32 },
    #[error("invalid extended CAN ID: 0x{id:X}")]
    InvalidExtendedId { id: u32 },
    #[error("payload too long: {len}, max {max}")]
    PayloadTooLong { len: usize, max: usize },
    #[error("invalid DLC: {dlc}")]
    InvalidDlc { dlc: u8 },
    #[error("invalid serialized frame format: {format}")]
    InvalidSerializedFrameFormat { format: u8 },
    #[error("noncanonical padding byte at {index}: 0x{value:02X}")]
    NonCanonicalPadding { index: usize, value: u8 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct StandardCanId(u16);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ExtendedCanId(u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CanId {
    Standard(StandardCanId),
    Extended(ExtendedCanId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CanData {
    bytes: [u8; 8],
    len: u8,
}

struct CanDataLen<const N: usize>;

trait CanDataLenAtMost8 {}

impl CanDataLenAtMost8 for CanDataLen<0> {}
impl CanDataLenAtMost8 for CanDataLen<1> {}
impl CanDataLenAtMost8 for CanDataLen<2> {}
impl CanDataLenAtMost8 for CanDataLen<3> {}
impl CanDataLenAtMost8 for CanDataLen<4> {}
impl CanDataLenAtMost8 for CanDataLen<5> {}
impl CanDataLenAtMost8 for CanDataLen<6> {}
impl CanDataLenAtMost8 for CanDataLen<7> {}
impl CanDataLenAtMost8 for CanDataLen<8> {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PiperFrame {
    id: CanId,
    data: CanData,
    timestamp_us: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct JointIndex(u8);

impl StandardCanId {
    pub const fn raw(self) -> u16 {
        self.0
    }

    pub fn new(raw: u32) -> Result<Self, FrameError> {
        if raw <= STANDARD_CAN_ID_MAX {
            Ok(Self(raw as u16))
        } else {
            Err(FrameError::InvalidStandardId { id: raw })
        }
    }

    #[allow(dead_code)]
    const fn new_const(raw: u16) -> Self {
        assert!((raw as u32) <= STANDARD_CAN_ID_MAX);
        Self(raw)
    }
}

impl ExtendedCanId {
    pub const fn raw(self) -> u32 {
        self.0
    }

    pub fn new(raw: u32) -> Result<Self, FrameError> {
        if raw <= EXTENDED_CAN_ID_MAX {
            Ok(Self(raw))
        } else {
            Err(FrameError::InvalidExtendedId { id: raw })
        }
    }

    #[allow(dead_code)]
    const fn new_const(raw: u32) -> Self {
        assert!(raw <= EXTENDED_CAN_ID_MAX);
        Self(raw)
    }
}

impl CanId {
    pub fn standard(raw: u32) -> Result<Self, FrameError> {
        Ok(Self::Standard(StandardCanId::new(raw)?))
    }

    pub fn extended(raw: u32) -> Result<Self, FrameError> {
        Ok(Self::Extended(ExtendedCanId::new(raw)?))
    }

    pub const fn raw(self) -> u32 {
        match self {
            Self::Standard(id) => id.raw() as u32,
            Self::Extended(id) => id.raw(),
        }
    }

    pub const fn is_standard(&self) -> bool {
        matches!(self, Self::Standard(_))
    }

    pub const fn is_extended(&self) -> bool {
        matches!(self, Self::Extended(_))
    }

    pub const fn as_standard(&self) -> Option<StandardCanId> {
        match self {
            Self::Standard(id) => Some(*id),
            Self::Extended(_) => None,
        }
    }

    pub const fn as_extended(&self) -> Option<ExtendedCanId> {
        match self {
            Self::Standard(_) => None,
            Self::Extended(id) => Some(*id),
        }
    }

    const fn format_discriminant(self) -> u8 {
        match self {
            Self::Standard(_) => 0,
            Self::Extended(_) => 1,
        }
    }
}

impl From<StandardCanId> for CanId {
    fn from(value: StandardCanId) -> Self {
        Self::Standard(value)
    }
}

impl From<ExtendedCanId> for CanId {
    fn from(value: ExtendedCanId) -> Self {
        Self::Extended(value)
    }
}

impl Ord for CanId {
    fn cmp(&self, other: &Self) -> Ordering {
        // Ordering is for maps/sets only; protocol range checks must use typed constructors.
        (self.format_discriminant(), self.raw()).cmp(&(other.format_discriminant(), other.raw()))
    }
}

impl PartialOrd for CanId {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl JointIndex {
    pub fn new(raw: u8) -> Result<Self, crate::ProtocolError> {
        if (1..=6).contains(&raw) {
            Ok(Self(raw))
        } else {
            Err(crate::ProtocolError::InvalidJointIndex { joint_index: raw })
        }
    }

    pub const fn get(self) -> u8 {
        self.0
    }

    pub const fn zero_based(self) -> u8 {
        self.0 - 1
    }
}

impl CanData {
    pub fn new(data: impl AsRef<[u8]>) -> Result<Self, FrameError> {
        let data = data.as_ref();
        if data.len() > CAN_DATA_MAX_LEN {
            return Err(FrameError::PayloadTooLong {
                len: data.len(),
                max: CAN_DATA_MAX_LEN,
            });
        }
        let mut bytes = [0u8; 8];
        bytes[..data.len()].copy_from_slice(data);
        Ok(Self {
            bytes,
            len: data.len() as u8,
        })
    }

    pub fn from_array(bytes: [u8; 8]) -> Self {
        Self { bytes, len: 8 }
    }

    #[allow(private_bounds)]
    pub fn from_exact<const N: usize>(bytes: [u8; N]) -> Self
    where
        CanDataLen<N>: CanDataLenAtMost8,
    {
        const {
            assert!(N <= 8);
        }
        let mut padded = [0u8; 8];
        padded[..N].copy_from_slice(&bytes);
        Self {
            bytes: padded,
            len: N as u8,
        }
    }

    pub fn from_padded(bytes: [u8; 8], len: u8) -> Result<Self, FrameError> {
        if len > 8 {
            return Err(FrameError::InvalidDlc { dlc: len });
        }
        let mut normalized = [0u8; 8];
        normalized[..len as usize].copy_from_slice(&bytes[..len as usize]);
        Ok(Self {
            bytes: normalized,
            len,
        })
    }

    pub fn validate_canonical_padding(bytes: [u8; 8], len: u8) -> Result<(), FrameError> {
        if len > 8 {
            return Err(FrameError::InvalidDlc { dlc: len });
        }
        for (index, value) in bytes.iter().enumerate().skip(len as usize) {
            if *value != 0 {
                return Err(FrameError::NonCanonicalPadding {
                    index,
                    value: *value,
                });
            }
        }
        Ok(())
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.bytes[..self.len as usize]
    }

    pub fn as_padded(&self) -> &[u8; 8] {
        &self.bytes
    }

    pub fn len(&self) -> u8 {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

impl PiperFrame {
    pub fn standard(id: StandardCanId, data: CanData) -> Self {
        Self {
            id: id.into(),
            data,
            timestamp_us: 0,
        }
    }

    pub fn extended(id: ExtendedCanId, data: CanData) -> Self {
        Self {
            id: id.into(),
            data,
            timestamp_us: 0,
        }
    }

    pub fn new_standard(id: u32, data: impl AsRef<[u8]>) -> Result<Self, FrameError> {
        Ok(Self::standard(StandardCanId::new(id)?, CanData::new(data)?))
    }

    pub fn new_extended(id: u32, data: impl AsRef<[u8]>) -> Result<Self, FrameError> {
        Ok(Self::extended(ExtendedCanId::new(id)?, CanData::new(data)?))
    }

    pub fn id(&self) -> CanId {
        self.id
    }

    pub fn raw_id(&self) -> u32 {
        self.id.raw()
    }

    pub fn is_standard(&self) -> bool {
        self.id.is_standard()
    }

    pub fn is_extended(&self) -> bool {
        self.id.is_extended()
    }

    pub fn dlc(&self) -> u8 {
        self.data.len()
    }

    pub fn data(&self) -> &[u8] {
        self.data.as_slice()
    }

    pub fn data_padded(&self) -> &[u8; 8] {
        self.data.as_padded()
    }

    pub fn timestamp_us(&self) -> u64 {
        self.timestamp_us
    }

    pub fn with_timestamp_us(mut self, timestamp_us: u64) -> Self {
        self.timestamp_us = timestamp_us;
        self
    }
}
