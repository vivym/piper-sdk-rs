use piper_driver::BackendCapability;

pub trait CapabilityMarker: Clone + Copy + Send + Sync + 'static {
    const BACKEND_CAPABILITY: BackendCapability;
}

pub trait MotionCapability: CapabilityMarker {}
pub trait StrictCapability: MotionCapability {}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UnspecifiedCapability;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StrictRealtime;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SoftRealtime;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MonitorOnly;

impl CapabilityMarker for UnspecifiedCapability {
    const BACKEND_CAPABILITY: BackendCapability = BackendCapability::MonitorOnly;
}

impl CapabilityMarker for StrictRealtime {
    const BACKEND_CAPABILITY: BackendCapability = BackendCapability::StrictRealtime;
}

impl CapabilityMarker for SoftRealtime {
    const BACKEND_CAPABILITY: BackendCapability = BackendCapability::SoftRealtime;
}

impl CapabilityMarker for MonitorOnly {
    const BACKEND_CAPABILITY: BackendCapability = BackendCapability::MonitorOnly;
}

impl MotionCapability for StrictRealtime {}
impl MotionCapability for SoftRealtime {}
impl StrictCapability for StrictRealtime {}
