#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProtocolDiagnostic {
    InvalidLength {
        can_id: u32,
        expected: usize,
        actual: usize,
    },
    InvalidEnum {
        field: &'static str,
        raw: u8,
    },
    OutOfRange {
        field: &'static str,
        raw: u32,
        min: u32,
        max: u32,
    },
    UnsupportedValue {
        field: &'static str,
        raw: u32,
    },
    MalformedGroupMember {
        can_id: u32,
        member: &'static str,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum DecodeResult<T> {
    Data(TypedFrame<T>),
    Diagnostic(ProtocolDiagnostic),
    Ignore,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TypedFrame<T> {
    pub can_id: u32,
    pub payload: T,
    pub hardware_timestamp_us: Option<u64>,
}
