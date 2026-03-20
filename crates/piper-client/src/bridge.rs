//! Generic non-realtime bridge client API.
//!
//! This wraps the current stream bridge transport behind generic naming so the
//! controller-owned bridge host can evolve without leaking transport/backend
//! specifics into user-facing APIs.

use piper_can::PiperFrame;

pub use piper_can::bridge::protocol::{
    BridgeDeviceState, BridgeRole, BridgeStatus, CanIdFilter, ErrorCode, SessionToken,
};
pub use piper_can::{
    BridgeClientOptions, BridgeEndpoint, BridgeError, BridgeResult, BridgeTlsClientConfig,
};

#[derive(Debug, Clone, PartialEq)]
pub enum BridgeEvent {
    ReceiveFrame(PiperFrame),
    Gap { dropped: u32 },
    SessionReplaced,
    MaintenanceLeaseRevoked,
}

impl From<piper_can::bridge::protocol::BridgeEvent> for BridgeEvent {
    fn from(value: piper_can::bridge::protocol::BridgeEvent) -> Self {
        match value {
            piper_can::bridge::protocol::BridgeEvent::ReceiveFrame(frame) => {
                Self::ReceiveFrame(frame)
            },
            piper_can::bridge::protocol::BridgeEvent::Gap { dropped } => Self::Gap { dropped },
            piper_can::bridge::protocol::BridgeEvent::SessionReplaced => Self::SessionReplaced,
            piper_can::bridge::protocol::BridgeEvent::LeaseRevoked => Self::MaintenanceLeaseRevoked,
        }
    }
}

pub struct PiperBridgeClient {
    inner: piper_can::BridgeClient,
}

impl PiperBridgeClient {
    pub fn connect(endpoint: BridgeEndpoint, options: BridgeClientOptions) -> BridgeResult<Self> {
        Ok(Self {
            inner: piper_can::BridgeClient::connect(endpoint, options)?,
        })
    }

    pub fn endpoint(&self) -> &BridgeEndpoint {
        self.inner.endpoint()
    }

    pub fn session_token(&self) -> SessionToken {
        self.inner.session_token()
    }

    pub fn session_id(&self) -> u32 {
        self.inner.session_id()
    }

    pub fn role_granted(&self) -> BridgeRole {
        self.inner.role_granted()
    }

    pub fn recv_event(&mut self, timeout: std::time::Duration) -> BridgeResult<BridgeEvent> {
        self.inner.recv_event(timeout).map(BridgeEvent::from)
    }

    pub fn get_status(&mut self) -> BridgeResult<BridgeStatus> {
        self.inner.get_status()
    }

    pub fn set_filters(&mut self, filters: Vec<CanIdFilter>) -> BridgeResult<()> {
        self.inner.set_filters(filters)
    }

    pub fn set_raw_frame_tap(&mut self, enabled: bool) -> BridgeResult<()> {
        self.inner.set_raw_frame_tap(enabled)
    }

    pub fn ping(&mut self) -> BridgeResult<()> {
        self.inner.ping()
    }

    pub fn acquire_maintenance_lease(
        &mut self,
        timeout: std::time::Duration,
    ) -> BridgeResult<MaintenanceLease<'_>> {
        Ok(MaintenanceLease {
            inner: self.inner.acquire_writer_lease(timeout)?,
        })
    }

    pub fn disconnect(&mut self) {
        self.inner.disconnect();
    }
}

pub struct MaintenanceLease<'a> {
    inner: piper_can::MaintenanceLease<'a>,
}

impl MaintenanceLease<'_> {
    pub fn send_frame(&mut self, frame: PiperFrame) -> BridgeResult<()> {
        self.inner.send_frame(frame)
    }

    pub fn release(self) -> BridgeResult<()> {
        self.inner.release()
    }
}
