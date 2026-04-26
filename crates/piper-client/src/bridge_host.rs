//! Controller-owned non-realtime bridge host.
//!
//! The bridge host is intentionally separate from the realtime control path:
//! it reads committed driver state and optional raw frame taps, and all
//! maintenance writes are mediated through driver-level runtime checks.

use crossbeam_channel::{Receiver, Sender, bounded};
use hex::FromHex;
use mio::net::TcpStream as MioTcpStream;
#[cfg(unix)]
use mio::net::UnixStream as MioUnixStream;
use mio::{Events, Interest, Poll, Token, Waker};
use piper_can::bridge::protocol::{
    self, BridgeDeviceState, BridgeEvent, BridgeRole, BridgeStatus, CanIdFilter, ClientRequest,
    ErrorCode, MAX_PAYLOAD_LEN, ServerMessage, ServerResponse, SessionToken,
};
use piper_can::{CanId, PiperFrame};
use piper_driver::hooks::FrameCallback;
use piper_driver::recording::RecordedFrameEvent;
use piper_driver::{
    DriverError, HealthStatus, HookHandle, MaintenanceGateState, MaintenanceLeaseAcquireResult,
    MaintenanceRevocationEvent, Piper as RobotPiper,
};
use rustls::pki_types::PrivateKeyDer;
use rustls::server::{ServerConfig, ServerConnection, WebPkiClientVerifier};
use rustls::{RootCertStore, StreamOwned};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::io::Cursor;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
#[cfg(unix)]
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock, Weak};
use std::thread;
use std::time::{Duration, Instant};
use tracing::warn;

const OUTBOUND_QUEUE_CAPACITY: usize = 256;
const RAW_FRAME_TAP_QUEUE_CAPACITY: usize = 1024;
const AUTH_LOG_WINDOW: Duration = Duration::from_secs(1);
const TLS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_EVENT_BURST: usize = 128;
const SOCKET_TOKEN: Token = Token(0);
const WAKE_TOKEN: Token = Token(1);

#[derive(Debug, Clone)]
pub struct BridgeTlsClientPolicy {
    pub fingerprint_sha256: String,
    pub granted_role: BridgeRole,
}

#[derive(Debug, Clone)]
pub struct BridgeTlsServerConfig {
    pub listen_addr: SocketAddr,
    pub server_cert_pem: PathBuf,
    pub server_key_pem: PathBuf,
    pub client_ca_cert_pem: PathBuf,
    pub client_policies: Vec<BridgeTlsClientPolicy>,
    pub handshake_timeout: Duration,
}

impl BridgeTlsServerConfig {
    pub fn with_addr(listen_addr: SocketAddr) -> Self {
        Self {
            listen_addr,
            server_cert_pem: PathBuf::new(),
            server_key_pem: PathBuf::new(),
            client_ca_cert_pem: PathBuf::new(),
            client_policies: Vec::new(),
            handshake_timeout: TLS_HANDSHAKE_TIMEOUT,
        }
    }
}

#[derive(Debug, Clone)]
pub struct BridgeUdsListenerConfig {
    pub path: PathBuf,
    pub granted_role: BridgeRole,
}

#[derive(Debug, Clone)]
pub struct BridgeHostConfig {
    pub uds: Option<BridgeUdsListenerConfig>,
    pub tcp_tls: Option<BridgeTlsServerConfig>,
    pub allow_raw_frame_tap: bool,
}

impl Default for BridgeHostConfig {
    fn default() -> Self {
        Self {
            #[cfg(unix)]
            uds: Some(BridgeUdsListenerConfig {
                path: PathBuf::from("/tmp/piper_bridge.sock"),
                granted_role: BridgeRole::Observer,
            }),
            #[cfg(not(unix))]
            uds: None,
            tcp_tls: None,
            allow_raw_frame_tap: false,
        }
    }
}

#[derive(Debug)]
pub enum BridgeHostError {
    Listener(String),
    Io(String),
    InvalidConfig(String),
    Driver(String),
    Backend(String),
    AlreadyRunning,
}

impl std::fmt::Display for BridgeHostError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Listener(message) => write!(f, "{message}"),
            Self::Io(message) => write!(f, "{message}"),
            Self::InvalidConfig(message) => write!(f, "{message}"),
            Self::Driver(message) => write!(f, "{message}"),
            Self::Backend(message) => write!(f, "{message}"),
            Self::AlreadyRunning => write!(f, "bridge host is already running"),
        }
    }
}

impl std::error::Error for BridgeHostError {}

#[derive(Debug, Clone)]
pub(crate) struct BridgeStatusInput {
    pub health: HealthStatus,
    pub usb_stall_count: u64,
    pub can_bus_off_count: u64,
    pub can_error_passive_count: u64,
    pub cpu_usage_percent: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BridgeMaintenanceState {
    DeniedFaulted,
    DeniedActiveControl,
    DeniedTransportDown,
    AllowedStandby,
    DeniedDriveStateUnknown,
}

impl BridgeMaintenanceState {
    fn denial_message(self) -> &'static str {
        match self {
            Self::DeniedFaulted => MaintenanceGateState::DeniedFaulted.denial_message(),
            Self::DeniedActiveControl => MaintenanceGateState::DeniedActiveControl.denial_message(),
            Self::DeniedTransportDown => MaintenanceGateState::DeniedTransportDown.denial_message(),
            Self::AllowedStandby => MaintenanceGateState::AllowedStandby.denial_message(),
            Self::DeniedDriveStateUnknown => {
                MaintenanceGateState::DeniedDriveStateUnknown.denial_message()
            },
        }
    }
}

impl From<MaintenanceGateState> for BridgeMaintenanceState {
    fn from(value: MaintenanceGateState) -> Self {
        match value {
            MaintenanceGateState::DeniedFaulted => Self::DeniedFaulted,
            MaintenanceGateState::DeniedActiveControl => Self::DeniedActiveControl,
            MaintenanceGateState::DeniedTransportDown => Self::DeniedTransportDown,
            MaintenanceGateState::AllowedStandby => Self::AllowedStandby,
            MaintenanceGateState::DeniedDriveStateUnknown => Self::DeniedDriveStateUnknown,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct BridgeBackendError {
    code: ErrorCode,
    message: String,
}

impl BridgeBackendError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    fn code(&self) -> ErrorCode {
        self.code
    }

    fn message(&self) -> &str {
        &self.message
    }
}

impl std::fmt::Display for BridgeBackendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for BridgeBackendError {}

trait RawTapCleanup: Send + Sync {
    fn uninstall(&self);
}

pub(crate) struct RawTapSubscription {
    cleanup: Option<Box<dyn RawTapCleanup>>,
}

impl RawTapSubscription {
    fn new(cleanup: impl RawTapCleanup + 'static) -> Self {
        Self {
            cleanup: Some(Box::new(cleanup)),
        }
    }
}

impl Drop for RawTapSubscription {
    fn drop(&mut self) {
        if let Some(cleanup) = self.cleanup.take() {
            cleanup.uninstall();
        }
    }
}

trait BridgeControllerBackend: Send + Sync {
    fn status_snapshot(&self) -> BridgeStatusInput;
    fn register_maintenance_event_sink(
        &self,
        sink: Sender<MaintenanceRevocationEvent>,
    ) -> Result<(), BridgeBackendError>;
    fn acquire_maintenance_lease(
        &self,
        authority: &SessionAuthority,
        timeout: Duration,
    ) -> Result<LeaseAcquireResult, BridgeBackendError>;
    fn release_maintenance_lease(
        &self,
        authority: &SessionAuthority,
    ) -> Result<bool, BridgeBackendError>;
    fn send_maintenance_frame(
        &self,
        authority: &SessionAuthority,
        frame: PiperFrame,
    ) -> Result<(), BridgeBackendError>;
    fn install_raw_frame_tap(
        &self,
        tx: Sender<PiperFrame>,
    ) -> Result<RawTapSubscription, BridgeBackendError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LeaseAcquireResult {
    Granted,
    Denied { holder_session_id: Option<u32> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum SessionLifecycle {
    Active = 0,
    Replaced = 1,
    Closing = 2,
    Closed = 3,
}

impl SessionLifecycle {
    fn from_u8(value: u8) -> Self {
        match value {
            x if x == Self::Replaced as u8 => Self::Replaced,
            x if x == Self::Closing as u8 => Self::Closing,
            x if x == Self::Closed as u8 => Self::Closed,
            _ => Self::Active,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct SessionKey(u64);

impl SessionKey {
    fn raw(self) -> u64 {
        self.0
    }
}

#[derive(Clone)]
struct SessionAuthority {
    session: Arc<BridgeSession>,
    sessions: Weak<SessionManager>,
    authority_epoch_snapshot: u64,
}

impl SessionAuthority {
    fn new(session: Arc<BridgeSession>, sessions: &Arc<SessionManager>) -> Self {
        let authority_epoch_snapshot = session.authority_epoch();
        Self {
            session,
            sessions: Arc::downgrade(sessions),
            authority_epoch_snapshot,
        }
    }

    fn session_id(&self) -> u32 {
        self.session.session_id()
    }

    fn session_key(&self) -> SessionKey {
        self.session.session_key()
    }

    fn role_granted(&self) -> BridgeRole {
        self.session.role_granted()
    }

    fn is_current(&self) -> bool {
        if self.session.authority_epoch() != self.authority_epoch_snapshot {
            return false;
        }
        let Some(sessions) = self.sessions.upgrade() else {
            return false;
        };
        sessions.active_session_by_key(self.session_key()).is_some_and(|current| {
            Arc::ptr_eq(&current, &self.session)
                && current.authority_epoch() == self.authority_epoch_snapshot
        })
    }
}

#[derive(Debug)]
enum ConnectionOutput {
    Event(BridgeEvent),
    CloseAfterEvent(BridgeEvent),
    Shutdown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EnqueueFrameResult {
    Delivered,
    QueueFull,
    Inactive,
}

trait ConnectionWake: Send + Sync {
    fn wake(&self);
}

#[derive(Debug)]
struct BridgeHostStats {
    started_at: Instant,
    frame_rx_total: AtomicU64,
    maintenance_tx_total: AtomicU64,
    ipc_in_total: AtomicU64,
    ipc_out_total: AtomicU64,
    queue_drop_total: AtomicU64,
    inactive_enqueue_total: AtomicU64,
    session_replacement_discard_total: AtomicU64,
}

impl BridgeHostStats {
    fn new() -> Self {
        Self {
            started_at: Instant::now(),
            frame_rx_total: AtomicU64::new(0),
            maintenance_tx_total: AtomicU64::new(0),
            ipc_in_total: AtomicU64::new(0),
            ipc_out_total: AtomicU64::new(0),
            queue_drop_total: AtomicU64::new(0),
            inactive_enqueue_total: AtomicU64::new(0),
            session_replacement_discard_total: AtomicU64::new(0),
        }
    }

    fn elapsed_secs(&self) -> f64 {
        self.started_at.elapsed().as_secs_f64().max(0.001)
    }

    fn fps(counter: &AtomicU64, elapsed: f64) -> f64 {
        counter.load(Ordering::Relaxed) as f64 / elapsed
    }

    fn frame_rx_fps(&self) -> f64 {
        Self::fps(&self.frame_rx_total, self.elapsed_secs())
    }

    fn maintenance_tx_fps(&self) -> f64 {
        Self::fps(&self.maintenance_tx_total, self.elapsed_secs())
    }

    fn ipc_in_fps(&self) -> f64 {
        Self::fps(&self.ipc_in_total, self.elapsed_secs())
    }

    fn ipc_out_fps(&self) -> f64 {
        Self::fps(&self.ipc_out_total, self.elapsed_secs())
    }

    fn apply_queue_drop_penalty(&self, base_score: u8) -> u8 {
        let mut score = base_score as i32;
        let dropped = self.queue_drop_total.load(Ordering::Relaxed);
        if dropped > 0 {
            score -= (dropped.min(1000) / 20) as i32;
        }
        score.clamp(0, 100) as u8
    }
}

#[derive(Debug)]
struct WarnLogState {
    last_logged: Instant,
    suppressed: u64,
}

#[derive(Debug)]
struct WarnRateLimiter {
    window: Duration,
    states: Mutex<HashMap<&'static str, WarnLogState>>,
}

impl WarnRateLimiter {
    fn new(window: Duration) -> Self {
        Self {
            window,
            states: Mutex::new(HashMap::new()),
        }
    }

    fn warn<F>(&self, key: &'static str, message: F)
    where
        F: FnOnce() -> String,
    {
        let mut states = self.states.lock().unwrap();
        let now = Instant::now();
        let entry = states.entry(key).or_insert(WarnLogState {
            last_logged: now.checked_sub(self.window).unwrap_or(now),
            suppressed: 0,
        });

        if now.duration_since(entry.last_logged) >= self.window {
            if entry.suppressed > 0 {
                warn!(
                    "{} (suppressed {} similar warnings)",
                    message(),
                    entry.suppressed
                );
                entry.suppressed = 0;
            } else {
                warn!("{}", message());
            }
            entry.last_logged = now;
        } else {
            entry.suppressed += 1;
        }
    }
}

#[derive(Debug)]
struct PendingWrite {
    bytes: Vec<u8>,
    offset: usize,
    close_after: bool,
}

impl PendingWrite {
    fn from_message(message: ServerMessage, close_after: bool) -> Result<Self, BridgeHostError> {
        let bytes = protocol::encode_server_message(&message).map_err(|err| {
            BridgeHostError::Io(format!("failed to encode bridge message: {err}"))
        })?;
        Ok(Self {
            bytes,
            offset: 0,
            close_after,
        })
    }

    fn remaining(&self) -> &[u8] {
        &self.bytes[self.offset..]
    }

    fn advance(&mut self, written: usize) {
        self.offset = self.offset.saturating_add(written).min(self.bytes.len());
    }

    fn is_complete(&self) -> bool {
        self.offset >= self.bytes.len()
    }
}

#[derive(Debug)]
struct ActorWake {
    waker: Arc<Waker>,
}

impl ConnectionWake for ActorWake {
    fn wake(&self) {
        let _ = self.waker.wake();
    }
}

struct PlainTransport<S> {
    socket: S,
    peer_label: String,
    read_buf: Vec<u8>,
    current_write: Option<PendingWrite>,
    current_post_flush: Option<PostFlushAction>,
    close_after_flush: bool,
}

impl<S> PlainTransport<S>
where
    S: mio::event::Source + Read + Write,
{
    fn new(socket: S, peer_label: String) -> Self {
        Self {
            socket,
            peer_label,
            read_buf: Vec::with_capacity(1024),
            current_write: None,
            current_post_flush: None,
            close_after_flush: false,
        }
    }

    fn register(
        &mut self,
        registry: &mio::Registry,
        token: Token,
        interest: Interest,
    ) -> Result<(), BridgeHostError> {
        registry
            .register(&mut self.socket, token, interest)
            .map_err(|err| BridgeHostError::Io(format!("failed to register bridge stream: {err}")))
    }

    fn reregister(
        &mut self,
        registry: &mio::Registry,
        token: Token,
        interest: Interest,
    ) -> Result<(), BridgeHostError> {
        registry.reregister(&mut self.socket, token, interest).map_err(|err| {
            BridgeHostError::Io(format!("failed to update bridge stream interest: {err}"))
        })
    }
}

struct TlsTransport {
    socket: MioTcpStream,
    conn: ServerConnection,
    peer_label: String,
    read_buf: Vec<u8>,
    current_write: Option<PendingWrite>,
    current_post_flush: Option<PostFlushAction>,
    close_after_flush: bool,
}

#[derive(Default)]
struct FlushResult {
    wrote_any: bool,
    completed: Option<(bool, Option<PostFlushAction>)>,
}

impl TlsTransport {
    fn register(
        &mut self,
        registry: &mio::Registry,
        token: Token,
        interest: Interest,
    ) -> Result<(), BridgeHostError> {
        registry
            .register(&mut self.socket, token, interest)
            .map_err(|err| BridgeHostError::Io(format!("failed to register bridge stream: {err}")))
    }

    fn reregister(
        &mut self,
        registry: &mio::Registry,
        token: Token,
        interest: Interest,
    ) -> Result<(), BridgeHostError> {
        registry.reregister(&mut self.socket, token, interest).map_err(|err| {
            BridgeHostError::Io(format!("failed to update bridge stream interest: {err}"))
        })
    }
}

enum ServerStream {
    #[cfg(unix)]
    Unix(Box<PlainTransport<MioUnixStream>>),
    TcpTls(Box<TlsTransport>),
}

impl ServerStream {
    fn peer_label(&self) -> &str {
        match self {
            #[cfg(unix)]
            Self::Unix(stream) => &stream.peer_label,
            Self::TcpTls(stream) => &stream.peer_label,
        }
    }

    fn register(
        &mut self,
        registry: &mio::Registry,
        wants_write: bool,
    ) -> Result<(), BridgeHostError> {
        let interest = self.interest(wants_write);
        match self {
            #[cfg(unix)]
            Self::Unix(stream) => stream.register(registry, SOCKET_TOKEN, interest),
            Self::TcpTls(stream) => stream.register(registry, SOCKET_TOKEN, interest),
        }
    }

    fn update_interest(
        &mut self,
        registry: &mio::Registry,
        wants_write: bool,
    ) -> Result<(), BridgeHostError> {
        let interest = self.interest(wants_write);
        match self {
            #[cfg(unix)]
            Self::Unix(stream) => stream.reregister(registry, SOCKET_TOKEN, interest),
            Self::TcpTls(stream) => stream.reregister(registry, SOCKET_TOKEN, interest),
        }
    }

    fn interest(&self, wants_write: bool) -> Interest {
        if wants_write || self.has_pending_write() {
            Interest::READABLE | Interest::WRITABLE
        } else {
            Interest::READABLE
        }
    }

    fn has_pending_write(&self) -> bool {
        match self {
            #[cfg(unix)]
            Self::Unix(stream) => stream.current_write.is_some(),
            Self::TcpTls(stream) => stream.current_write.is_some() || stream.conn.wants_write(),
        }
    }

    fn start_message(&mut self, queued: QueuedMessage) -> Result<(), BridgeHostError> {
        match self {
            #[cfg(unix)]
            Self::Unix(stream) => {
                debug_assert!(stream.current_write.is_none());
                stream.current_write = Some(PendingWrite::from_message(
                    queued.message,
                    queued.close_after,
                )?);
                stream.current_post_flush = queued.post_flush;
            },
            Self::TcpTls(stream) => {
                debug_assert!(stream.current_write.is_none());
                stream.current_write = Some(PendingWrite::from_message(
                    queued.message,
                    queued.close_after,
                )?);
                stream.current_post_flush = queued.post_flush;
            },
        }
        Ok(())
    }

    fn shutdown(&mut self) {
        match self {
            #[cfg(unix)]
            Self::Unix(stream) => {
                let _ = stream.socket.shutdown(std::net::Shutdown::Both);
            },
            Self::TcpTls(stream) => {
                let _ = stream.socket.shutdown(std::net::Shutdown::Both);
            },
        }
    }

    fn should_close_after_flush(&self) -> bool {
        match self {
            #[cfg(unix)]
            Self::Unix(stream) => stream.close_after_flush && stream.current_write.is_none(),
            Self::TcpTls(stream) => {
                stream.close_after_flush
                    && stream.current_write.is_none()
                    && !stream.conn.wants_write()
            },
        }
    }

    fn clear_close_after_flush(&mut self) {
        match self {
            #[cfg(unix)]
            Self::Unix(stream) => stream.close_after_flush = false,
            Self::TcpTls(stream) => stream.close_after_flush = false,
        }
    }
}

fn is_would_block(err: &std::io::Error) -> bool {
    matches!(
        err.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
    )
}

fn drain_framed_payloads(
    buf: &mut Vec<u8>,
    payloads: &mut Vec<Vec<u8>>,
) -> Result<(), BridgeHostError> {
    loop {
        if buf.len() < 4 {
            return Ok(());
        }
        let len = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
        if len > MAX_PAYLOAD_LEN {
            return Err(BridgeHostError::Io(format!(
                "bridge frame too large: {len} bytes"
            )));
        }
        if buf.len() < 4 + len {
            return Ok(());
        }
        payloads.push(buf[4..4 + len].to_vec());
        buf.drain(..4 + len);
    }
}

impl ServerStream {
    fn read_available(&mut self) -> Result<(Vec<Vec<u8>>, bool), BridgeHostError> {
        let mut payloads = Vec::new();
        let mut saw_eof = false;

        match self {
            #[cfg(unix)]
            Self::Unix(stream) => {
                let mut scratch = [0u8; 4096];
                loop {
                    match stream.socket.read(&mut scratch) {
                        Ok(0) => {
                            saw_eof = true;
                            break;
                        },
                        Ok(n) => stream.read_buf.extend_from_slice(&scratch[..n]),
                        Err(err) if is_would_block(&err) => break,
                        Err(err) => {
                            return Err(BridgeHostError::Io(format!(
                                "bridge read failed for {}: {err}",
                                stream.peer_label
                            )));
                        },
                    }
                }
                drain_framed_payloads(&mut stream.read_buf, &mut payloads)?;
            },
            Self::TcpTls(stream) => {
                loop {
                    match stream.conn.read_tls(&mut stream.socket) {
                        Ok(0) => {
                            saw_eof = true;
                            break;
                        },
                        Ok(_) => {},
                        Err(err) if is_would_block(&err) => break,
                        Err(err) => {
                            return Err(BridgeHostError::Io(format!(
                                "bridge tls read failed for {}: {err}",
                                stream.peer_label
                            )));
                        },
                    }
                }

                match stream.conn.process_new_packets() {
                    Ok(_) => {},
                    Err(err) => {
                        return Err(BridgeHostError::Io(format!(
                            "bridge tls packet processing failed for {}: {err}",
                            stream.peer_label
                        )));
                    },
                }

                let mut scratch = [0u8; 4096];
                loop {
                    match stream.conn.reader().read(&mut scratch) {
                        Ok(0) => break,
                        Ok(n) => stream.read_buf.extend_from_slice(&scratch[..n]),
                        Err(err) if is_would_block(&err) => break,
                        Err(err) => {
                            return Err(BridgeHostError::Io(format!(
                                "bridge tls plaintext read failed for {}: {err}",
                                stream.peer_label
                            )));
                        },
                    }
                }

                drain_framed_payloads(&mut stream.read_buf, &mut payloads)?;
            },
        }

        Ok((payloads, saw_eof))
    }

    fn flush_pending_write(
        &mut self,
        stats: &BridgeHostStats,
    ) -> Result<FlushResult, BridgeHostError> {
        let mut result = FlushResult::default();
        match self {
            #[cfg(unix)]
            Self::Unix(stream) => loop {
                let mut completed = None;
                let Some(pending) = stream.current_write.as_mut() else {
                    break;
                };
                match stream.socket.write(pending.remaining()) {
                    Ok(0) => {
                        return Err(BridgeHostError::Io(format!(
                            "bridge write returned EOF for {}",
                            stream.peer_label
                        )));
                    },
                    Ok(written) => {
                        result.wrote_any = true;
                        pending.advance(written);
                        if pending.is_complete() {
                            completed = Some(pending.close_after);
                        }
                    },
                    Err(err) if is_would_block(&err) => break,
                    Err(err) => {
                        return Err(BridgeHostError::Io(format!(
                            "bridge write failed for {}: {err}",
                            stream.peer_label
                        )));
                    },
                }
                if let Some(close_after) = completed {
                    stats.ipc_out_total.fetch_add(1, Ordering::Relaxed);
                    let post_flush = stream.current_post_flush.take();
                    stream.current_write = None;
                    if close_after {
                        stream.close_after_flush = true;
                    }
                    result.completed = Some((close_after, post_flush));
                }
            },
            Self::TcpTls(stream) => {
                loop {
                    let mut finished_current = false;
                    if let Some(pending) = stream.current_write.as_mut() {
                        match stream.conn.writer().write(pending.remaining()) {
                            Ok(0) => break,
                            Ok(written) => {
                                result.wrote_any = true;
                                pending.advance(written);
                                if pending.is_complete() {
                                    finished_current = true;
                                }
                            },
                            Err(err) => {
                                return Err(BridgeHostError::Io(format!(
                                    "bridge tls plaintext write failed for {}: {err}",
                                    stream.peer_label
                                )));
                            },
                        }
                    }
                    if finished_current {
                        stats.ipc_out_total.fetch_add(1, Ordering::Relaxed);
                        let close_after = stream
                            .current_write
                            .as_ref()
                            .map(|pending| pending.close_after)
                            .unwrap_or(false);
                        let post_flush = stream.current_post_flush.take();
                        stream.current_write = None;
                        if close_after {
                            stream.close_after_flush = true;
                        }
                        result.completed = Some((close_after, post_flush));
                    }
                    if stream.current_write.is_none() {
                        break;
                    }
                }

                while stream.conn.wants_write() {
                    match stream.conn.write_tls(&mut stream.socket) {
                        Ok(0) => break,
                        Ok(_) => {
                            result.wrote_any = true;
                        },
                        Err(err) if is_would_block(&err) => break,
                        Err(err) => {
                            return Err(BridgeHostError::Io(format!(
                                "bridge tls write failed for {}: {err}",
                                stream.peer_label
                            )));
                        },
                    }
                }
            },
        }

        Ok(result)
    }
}

struct TlsListenerConfig {
    server_config: Arc<ServerConfig>,
    client_roles: Arc<HashMap<[u8; 32], BridgeRole>>,
    handshake_timeout: Duration,
}

struct RawFrameTap {
    tx: Sender<PiperFrame>,
}

impl FrameCallback for RawFrameTap {
    fn on_frame(&self, event: RecordedFrameEvent) {
        if event.direction != piper_driver::recording::RecordedFrameDirection::Rx {
            return;
        }
        let _ = self.tx.try_send(event.frame);
    }
}

struct DriverRawTapCleanup {
    hooks: Arc<std::sync::RwLock<piper_driver::HookManager>>,
    handle: HookHandle,
}

impl RawTapCleanup for DriverRawTapCleanup {
    fn uninstall(&self) {
        if let Ok(mut hooks) = self.hooks.write() {
            hooks.remove_callback(self.handle);
        }
    }
}

struct RawTapManager {
    allow: bool,
    tx: Sender<PiperFrame>,
    subscription: Option<RawTapSubscription>,
}

enum RawTapUpdateError {
    NotConnected,
    Backend(BridgeHostError),
}

impl RawTapManager {
    fn new(allow: bool, tx: Sender<PiperFrame>) -> Self {
        Self {
            allow,
            tx,
            subscription: None,
        }
    }

    fn allowed(&self) -> bool {
        self.allow
    }

    fn reconcile(
        &mut self,
        sessions: &SessionManager,
        backend: &Arc<dyn BridgeControllerBackend>,
    ) -> Result<(), BridgeHostError> {
        let desired = self.allow && sessions.raw_tap_subscriber_count() > 0;
        match (desired, self.subscription.is_some()) {
            (true, false) => {
                self.subscription = Some(
                    backend
                        .install_raw_frame_tap(self.tx.clone())
                        .map_err(|err| BridgeHostError::Backend(err.to_string()))?,
                );
            },
            (false, true) => {
                self.subscription = None;
            },
            _ => {},
        }
        Ok(())
    }

    fn set_session_subscription(
        &mut self,
        sessions: &SessionManager,
        backend: &Arc<dyn BridgeControllerBackend>,
        session_key: SessionKey,
        enabled: bool,
    ) -> Result<(), RawTapUpdateError> {
        if sessions.active_session_by_key(session_key).is_none() {
            return Err(RawTapUpdateError::NotConnected);
        }
        let already_subscribed = sessions.is_raw_tap_subscribed(session_key);
        if already_subscribed == enabled {
            return self.reconcile(sessions, backend).map_err(RawTapUpdateError::Backend);
        }

        let subscriber_count_before = sessions.raw_tap_subscriber_count();
        let needs_install_first = enabled && subscriber_count_before == 0;
        if needs_install_first {
            self.subscription = Some(backend.install_raw_frame_tap(self.tx.clone()).map_err(
                |err| RawTapUpdateError::Backend(BridgeHostError::Backend(err.to_string())),
            )?);
        }

        if !sessions.set_raw_tap_subscription(session_key, enabled) {
            if needs_install_first {
                self.subscription = None;
            }
            return Err(RawTapUpdateError::NotConnected);
        }

        if let Err(err) = self.reconcile(sessions, backend) {
            let _ = sessions.set_raw_tap_subscription(session_key, already_subscribed);
            if needs_install_first && subscriber_count_before == 0 {
                self.subscription = None;
            }
            return Err(RawTapUpdateError::Backend(err));
        }

        Ok(())
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct BroadcastFrameStats {
    dropped: u64,
    inactive: u64,
}

enum PostFlushAction {
    CommitPrepared(PreparedSession),
}

struct QueuedMessage {
    message: ServerMessage,
    close_after: bool,
    post_flush: Option<PostFlushAction>,
}

impl QueuedMessage {
    fn response(response: ServerResponse) -> Self {
        Self {
            message: ServerMessage::Response(response),
            close_after: false,
            post_flush: None,
        }
    }

    fn response_with_post_flush(response: ServerResponse, post_flush: PostFlushAction) -> Self {
        Self {
            message: ServerMessage::Response(response),
            close_after: false,
            post_flush: Some(post_flush),
        }
    }

    fn event(event: BridgeEvent) -> Self {
        Self {
            message: ServerMessage::Event(event),
            close_after: false,
            post_flush: None,
        }
    }

    fn close_after_event(event: BridgeEvent) -> Self {
        Self {
            message: ServerMessage::Event(event),
            close_after: true,
            post_flush: None,
        }
    }
}

struct BridgeSession {
    session_id: u32,
    session_key: SessionKey,
    session_token: SessionToken,
    role_granted: BridgeRole,
    filters: RwLock<Vec<CanIdFilter>>,
    event_tx: Sender<ConnectionOutput>,
    control_tx: Sender<ConnectionOutput>,
    pending_gap: AtomicU32,
    lifecycle: AtomicU8,
    authority_epoch: AtomicU64,
    replacement_close_queued: AtomicBool,
    wake: Arc<dyn ConnectionWake>,
}

impl BridgeSession {
    #[allow(clippy::too_many_arguments)]
    fn new(
        session_id: u32,
        session_key: SessionKey,
        session_token: SessionToken,
        role_granted: BridgeRole,
        filters: Vec<CanIdFilter>,
        event_tx: Sender<ConnectionOutput>,
        control_tx: Sender<ConnectionOutput>,
        wake: Arc<dyn ConnectionWake>,
    ) -> Self {
        Self {
            session_id,
            session_key,
            session_token,
            role_granted,
            filters: RwLock::new(filters),
            event_tx,
            control_tx,
            pending_gap: AtomicU32::new(0),
            lifecycle: AtomicU8::new(SessionLifecycle::Active as u8),
            authority_epoch: AtomicU64::new(1),
            replacement_close_queued: AtomicBool::new(false),
            wake,
        }
    }

    fn session_id(&self) -> u32 {
        self.session_id
    }

    fn session_key(&self) -> SessionKey {
        self.session_key
    }

    fn session_token(&self) -> SessionToken {
        self.session_token
    }

    fn role_granted(&self) -> BridgeRole {
        self.role_granted
    }

    fn lifecycle(&self) -> SessionLifecycle {
        SessionLifecycle::from_u8(self.lifecycle.load(Ordering::Acquire))
    }

    fn authority_epoch(&self) -> u64 {
        self.authority_epoch.load(Ordering::Acquire)
    }

    fn bump_authority_epoch(&self) {
        self.authority_epoch.fetch_add(1, Ordering::AcqRel);
    }

    fn is_active(&self) -> bool {
        self.lifecycle() == SessionLifecycle::Active
    }

    fn mark_replaced(&self) -> bool {
        let replaced = self
            .lifecycle
            .compare_exchange(
                SessionLifecycle::Active as u8,
                SessionLifecycle::Replaced as u8,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok();
        if replaced {
            self.bump_authority_epoch();
        }
        replaced
    }

    fn mark_closing(&self) {
        self.lifecycle.store(SessionLifecycle::Closing as u8, Ordering::Release);
        self.bump_authority_epoch();
    }

    fn mark_closed(&self) {
        self.lifecycle.store(SessionLifecycle::Closed as u8, Ordering::Release);
        self.bump_authority_epoch();
    }

    fn set_filters(&self, filters: Vec<CanIdFilter>) {
        *self.filters.write().unwrap() = filters;
    }

    fn matches_filter(&self, can_id: CanId) -> bool {
        let filters = self.filters.read().unwrap();
        if filters.is_empty() {
            return true;
        }
        filters.iter().any(|filter| filter.matches(can_id))
    }

    fn enqueue_frame(&self, frame: PiperFrame) -> EnqueueFrameResult {
        if !self.is_active() {
            return EnqueueFrameResult::Inactive;
        }

        let dropped = self.pending_gap.swap(0, Ordering::AcqRel);
        if dropped > 0
            && self
                .event_tx
                .try_send(ConnectionOutput::Event(BridgeEvent::Gap { dropped }))
                .is_err()
        {
            self.pending_gap.fetch_add(dropped + 1, Ordering::AcqRel);
            return EnqueueFrameResult::QueueFull;
        }

        match self
            .event_tx
            .try_send(ConnectionOutput::Event(BridgeEvent::ReceiveFrame(frame)))
        {
            Ok(()) => {
                self.wake.wake();
                EnqueueFrameResult::Delivered
            },
            Err(_) => {
                self.pending_gap.fetch_add(1, Ordering::AcqRel);
                EnqueueFrameResult::QueueFull
            },
        }
    }

    fn send_priority_event(&self, event: BridgeEvent) {
        let _ = self.control_tx.try_send(ConnectionOutput::Event(event));
        self.wake.wake();
    }

    fn replace_and_close(&self) {
        match self.lifecycle() {
            SessionLifecycle::Active => {
                self.mark_replaced();
            },
            SessionLifecycle::Replaced => {},
            SessionLifecycle::Closing | SessionLifecycle::Closed => return,
        }
        if self.replacement_close_queued.swap(true, Ordering::AcqRel) {
            return;
        }
        if self
            .control_tx
            .try_send(ConnectionOutput::CloseAfterEvent(
                BridgeEvent::SessionReplaced,
            ))
            .is_err()
        {
            self.wake.wake();
        }
        self.wake.wake();
    }

    fn revoke_maintenance_lease(&self) {
        if !self.is_active() {
            return;
        }
        self.send_priority_event(BridgeEvent::LeaseRevoked);
    }

    fn shutdown(&self) {
        self.mark_closing();
        let _ = self.control_tx.try_send(ConnectionOutput::Shutdown);
        self.wake.wake();
    }
}

struct PreparedSession {
    session_id: u32,
    session_key: SessionKey,
    session_token: SessionToken,
    role_granted: BridgeRole,
    filters: Vec<CanIdFilter>,
    event_tx: Sender<ConnectionOutput>,
    event_rx: Receiver<ConnectionOutput>,
    control_tx: Sender<ConnectionOutput>,
    control_rx: Receiver<ConnectionOutput>,
    wake: Arc<dyn ConnectionWake>,
}

impl PreparedSession {
    fn session_id(&self) -> u32 {
        self.session_id
    }

    fn role_granted(&self) -> BridgeRole {
        self.role_granted
    }
}

struct RegisterResult {
    session: Arc<BridgeSession>,
    replaced: Option<Arc<BridgeSession>>,
}

struct Registry {
    sessions: HashMap<u32, Arc<BridgeSession>>,
    token_to_session: HashMap<SessionToken, u32>,
    key_to_session: HashMap<SessionKey, u32>,
    raw_tap_sessions: HashSet<SessionKey>,
}

struct SessionManager {
    registry: RwLock<Registry>,
    next_session_id: AtomicU32,
    next_session_key: AtomicU64,
}

impl SessionManager {
    fn new() -> Self {
        Self {
            registry: RwLock::new(Registry {
                sessions: HashMap::new(),
                token_to_session: HashMap::new(),
                key_to_session: HashMap::new(),
                raw_tap_sessions: HashSet::new(),
            }),
            next_session_id: AtomicU32::new(1),
            next_session_key: AtomicU64::new(1),
        }
    }

    fn new_connection_queue() -> (Sender<ConnectionOutput>, Receiver<ConnectionOutput>) {
        bounded(OUTBOUND_QUEUE_CAPACITY)
    }

    fn next_session_id(&self) -> u32 {
        self.next_session_id
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                let next = current.wrapping_add(1).max(1);
                Some(next)
            })
            .expect("session id fetch_update should not fail")
    }

    fn next_session_key(&self) -> SessionKey {
        SessionKey(
            self.next_session_key
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                    let next = current.wrapping_add(1).max(1);
                    Some(next)
                })
                .expect("session key fetch_update should not fail"),
        )
    }

    fn prepare_session(
        &self,
        session_token: SessionToken,
        role_granted: BridgeRole,
        filters: Vec<CanIdFilter>,
        wake: Arc<dyn ConnectionWake>,
    ) -> PreparedSession {
        let (event_tx, event_rx) = Self::new_connection_queue();
        let (control_tx, control_rx) = crossbeam_channel::unbounded();
        PreparedSession {
            session_id: self.next_session_id(),
            session_key: self.next_session_key(),
            session_token,
            role_granted,
            filters,
            event_tx,
            event_rx,
            control_tx,
            control_rx,
            wake,
        }
    }

    fn commit_prepared(&self, prepared: PreparedSession) -> RegisterResult {
        let mut registry = self.registry.write().unwrap();

        let replaced =
            registry.token_to_session.remove(&prepared.session_token).and_then(|old_id| {
                let removed = registry.sessions.remove(&old_id)?;
                registry.raw_tap_sessions.remove(&removed.session_key());
                if registry.key_to_session.get(&removed.session_key()).copied() == Some(old_id) {
                    registry.key_to_session.remove(&removed.session_key());
                }
                Some(removed)
            });

        if let Some(ref old_session) = replaced {
            old_session.mark_replaced();
        }

        let session = Arc::new(BridgeSession::new(
            prepared.session_id,
            prepared.session_key,
            prepared.session_token,
            prepared.role_granted,
            prepared.filters,
            prepared.event_tx,
            prepared.control_tx,
            prepared.wake,
        ));
        registry.token_to_session.insert(prepared.session_token, prepared.session_id);
        registry.key_to_session.insert(prepared.session_key, prepared.session_id);
        registry.sessions.insert(prepared.session_id, Arc::clone(&session));
        drop(registry);

        RegisterResult { session, replaced }
    }

    fn unregister_session(&self, session_id: u32) -> Option<Arc<BridgeSession>> {
        let removed = {
            let mut registry = self.registry.write().unwrap();
            let removed = registry.sessions.remove(&session_id)?;
            registry.raw_tap_sessions.remove(&removed.session_key());
            if registry.token_to_session.get(&removed.session_token()).copied() == Some(session_id)
            {
                registry.token_to_session.remove(&removed.session_token());
            }
            if registry.key_to_session.get(&removed.session_key()).copied() == Some(session_id) {
                registry.key_to_session.remove(&removed.session_key());
            }
            Some(removed)
        };

        if let Some(ref session) = removed {
            session.mark_closed();
        }
        removed
    }

    #[cfg(test)]
    fn session(&self, session_id: u32) -> Option<Arc<BridgeSession>> {
        self.registry.read().unwrap().sessions.get(&session_id).map(Arc::clone)
    }

    #[cfg(test)]
    fn active_session(&self, session_id: u32) -> Option<Arc<BridgeSession>> {
        self.session(session_id).filter(|session| session.is_active())
    }

    fn active_session_by_key(&self, session_key: SessionKey) -> Option<Arc<BridgeSession>> {
        let registry = self.registry.read().unwrap();
        let session_id = registry.key_to_session.get(&session_key).copied()?;
        let session = registry.sessions.get(&session_id)?.clone();
        drop(registry);
        session.is_active().then_some(session)
    }

    fn count(&self) -> u32 {
        self.registry.read().unwrap().sessions.len() as u32
    }

    fn is_raw_tap_subscribed(&self, session_key: SessionKey) -> bool {
        self.registry.read().unwrap().raw_tap_sessions.contains(&session_key)
    }

    fn set_raw_tap_subscription(&self, session_key: SessionKey, enabled: bool) -> bool {
        let mut registry = self.registry.write().unwrap();
        let Some(session_id) = registry.key_to_session.get(&session_key).copied() else {
            return false;
        };
        let is_active =
            registry.sessions.get(&session_id).is_some_and(|session| session.is_active());
        if !is_active {
            return false;
        }
        if enabled {
            registry.raw_tap_sessions.insert(session_key);
        } else {
            registry.raw_tap_sessions.remove(&session_key);
        }
        true
    }

    fn raw_tap_subscriber_count(&self) -> usize {
        self.registry.read().unwrap().raw_tap_sessions.len()
    }

    fn broadcast_frame(&self, frame: PiperFrame) -> BroadcastFrameStats {
        let sessions = {
            let registry = self.registry.read().unwrap();
            registry
                .raw_tap_sessions
                .iter()
                .filter_map(|session_key| {
                    let session_id = registry.key_to_session.get(session_key).copied()?;
                    registry.sessions.get(&session_id).cloned()
                })
                .collect::<Vec<_>>()
        };
        let mut stats = BroadcastFrameStats::default();
        for session in sessions {
            if !session.matches_filter(frame.id()) {
                continue;
            }
            match session.enqueue_frame(frame) {
                EnqueueFrameResult::Delivered => {},
                EnqueueFrameResult::QueueFull => stats.dropped += 1,
                EnqueueFrameResult::Inactive => stats.inactive += 1,
            }
        }
        stats
    }

    #[cfg(test)]
    fn set_filters(&self, session_id: u32, filters: Vec<CanIdFilter>) -> bool {
        if let Some(session) = self.active_session(session_id) {
            session.set_filters(filters);
            true
        } else {
            false
        }
    }
}

struct ConnectionContext<'a> {
    backend: &'a Arc<dyn BridgeControllerBackend>,
    sessions: &'a Arc<SessionManager>,
    raw_tap: &'a Arc<Mutex<RawTapManager>>,
    stats: &'a Arc<BridgeHostStats>,
    warn_limiter: &'a Arc<WarnRateLimiter>,
}

pub struct PiperBridgeHost {
    backend: Arc<dyn BridgeControllerBackend>,
    config: BridgeHostConfig,
    sessions: Arc<SessionManager>,
    stats: Arc<BridgeHostStats>,
    warn_limiter: Arc<WarnRateLimiter>,
    started: AtomicBool,
}

fn supported_bridge_endpoint_count(config: &BridgeHostConfig, uds_supported: bool) -> usize {
    usize::from(uds_supported && config.uds.is_some()) + usize::from(config.tcp_tls.is_some())
}

impl PiperBridgeHost {
    fn attach_backend(backend: Arc<dyn BridgeControllerBackend>, config: BridgeHostConfig) -> Self {
        Self {
            backend,
            config,
            sessions: Arc::new(SessionManager::new()),
            stats: Arc::new(BridgeHostStats::new()),
            warn_limiter: Arc::new(WarnRateLimiter::new(AUTH_LOG_WINDOW)),
            started: AtomicBool::new(false),
        }
    }

    pub(crate) fn attach_to_driver(driver: Arc<RobotPiper>, config: BridgeHostConfig) -> Self {
        let backend = PiperBridgeBackend::from_driver(Arc::clone(&driver));
        Self::attach_backend(backend, config)
    }

    pub fn run(self) -> Result<(), BridgeHostError> {
        if self.started.swap(true, Ordering::AcqRel) {
            return Err(BridgeHostError::AlreadyRunning);
        }
        if supported_bridge_endpoint_count(&self.config, cfg!(unix)) == 0 {
            return Err(BridgeHostError::InvalidConfig(
                "at least one supported bridge endpoint must be enabled on this platform"
                    .to_string(),
            ));
        }

        let raw_tap = if self.config.allow_raw_frame_tap {
            let (tx, rx) = bounded(RAW_FRAME_TAP_QUEUE_CAPACITY);
            let sessions = Arc::clone(&self.sessions);
            let stats = Arc::clone(&self.stats);
            thread::Builder::new()
                .name("bridge_frame_fanout".into())
                .spawn(move || Self::frame_fanout_loop(rx, sessions, stats))
                .map_err(|err| {
                    BridgeHostError::Io(format!("failed to spawn frame fanout loop: {err}"))
                })?;
            Arc::new(Mutex::new(RawTapManager::new(true, tx)))
        } else {
            Arc::new(Mutex::new(RawTapManager::new(
                false,
                bounded::<PiperFrame>(1).0,
            )))
        };

        {
            let (lease_event_tx, lease_event_rx) = bounded(64);
            self.backend
                .register_maintenance_event_sink(lease_event_tx)
                .map_err(|err| BridgeHostError::Backend(err.to_string()))?;
            let backend = Arc::clone(&self.backend);
            let sessions = Arc::clone(&self.sessions);
            thread::Builder::new()
                .name("bridge_lease_events".into())
                .spawn(move || Self::lease_event_loop(backend, sessions, lease_event_rx))
                .map_err(|err| {
                    BridgeHostError::Io(format!("failed to spawn lease event loop: {err}"))
                })?;
        }

        let mut handles = Vec::new();

        #[cfg(unix)]
        if let Some(uds) = &self.config.uds {
            let path = &uds.path;
            let granted_role = uds.granted_role;
            if path.exists() {
                std::fs::remove_file(path).map_err(|err| {
                    BridgeHostError::Listener(format!(
                        "failed to remove existing uds socket {}: {err}",
                        path.display()
                    ))
                })?;
            }
            let listener = UnixListener::bind(path).map_err(|err| {
                BridgeHostError::Listener(format!(
                    "failed to bind uds listener {}: {err}",
                    path.display()
                ))
            })?;
            let backend = Arc::clone(&self.backend);
            let sessions = Arc::clone(&self.sessions);
            let raw_tap_manager = Arc::clone(&raw_tap);
            let stats = Arc::clone(&self.stats);
            let warn_limiter = Arc::clone(&self.warn_limiter);
            handles.push(
                thread::Builder::new()
                    .name("bridge_accept_uds".into())
                    .spawn(move || {
                        for incoming in listener.incoming() {
                            match incoming {
                                Ok(stream) => {
                                    let peer_label = match stream.peer_addr() {
                                        Ok(addr) => addr
                                            .as_pathname()
                                            .map(|path| format!("unix://{}", path.display()))
                                            .unwrap_or_else(|| "unix://peer".to_string()),
                                        Err(_) => "unix://peer".to_string(),
                                    };
                                    if let Err(err) = stream.set_nonblocking(true) {
                                        warn!("failed to set uds bridge stream nonblocking: {err}");
                                        continue;
                                    }
                                    let backend = Arc::clone(&backend);
                                    let sessions = Arc::clone(&sessions);
                                    let raw_tap_manager = Arc::clone(&raw_tap_manager);
                                    let stats = Arc::clone(&stats);
                                    let warn_limiter = Arc::clone(&warn_limiter);
                                    thread::spawn(move || {
                                        let stream =
                                            ServerStream::Unix(Box::new(PlainTransport::new(
                                                MioUnixStream::from_std(stream),
                                                peer_label,
                                            )));
                                        Self::run_connection_actor(
                                            stream,
                                            granted_role,
                                            ConnectionContext {
                                                backend: &backend,
                                                sessions: &sessions,
                                                raw_tap: &raw_tap_manager,
                                                stats: &stats,
                                                warn_limiter: &warn_limiter,
                                            },
                                        );
                                    });
                                },
                                Err(err) => warn!("bridge uds accept error: {err}"),
                            }
                        }
                    })
                    .map_err(|err| {
                        BridgeHostError::Io(format!("failed to spawn uds accept loop: {err}"))
                    })?,
            );
        }

        if let Some(tcp_tls) = &self.config.tcp_tls {
            let tls_listener = Arc::new(Self::build_tls_listener_config(tcp_tls)?);
            let addr = tcp_tls.listen_addr;
            let listener = TcpListener::bind(addr).map_err(|err| {
                BridgeHostError::Listener(format!("failed to bind tcp tls listener {addr}: {err}"))
            })?;
            let backend = Arc::clone(&self.backend);
            let sessions = Arc::clone(&self.sessions);
            let raw_tap_manager = Arc::clone(&raw_tap);
            let stats = Arc::clone(&self.stats);
            let warn_limiter = Arc::clone(&self.warn_limiter);
            let tls_listener = Arc::clone(&tls_listener);
            handles.push(
                thread::Builder::new()
                    .name("bridge_accept_tcp_tls".into())
                    .spawn(move || {
                        for incoming in listener.incoming() {
                            match incoming {
                                Ok(stream) => {
                                    if let Err(err) = stream.set_nodelay(true) {
                                        warn!("failed to set tcp nodelay: {err}");
                                    }
                                    let backend = Arc::clone(&backend);
                                    let sessions = Arc::clone(&sessions);
                                    let raw_tap_manager = Arc::clone(&raw_tap_manager);
                                    let stats = Arc::clone(&stats);
                                    let warn_limiter = Arc::clone(&warn_limiter);
                                    let tls_listener = Arc::clone(&tls_listener);
                                    thread::spawn(move || {
                                        match Self::accept_tls_stream(stream, &tls_listener) {
                                            Ok((stream, granted_role)) => {
                                                Self::run_connection_actor(
                                                    stream,
                                                    granted_role,
                                                    ConnectionContext {
                                                        backend: &backend,
                                                        sessions: &sessions,
                                                        raw_tap: &raw_tap_manager,
                                                        stats: &stats,
                                                        warn_limiter: &warn_limiter,
                                                    },
                                                )
                                            },
                                            Err(err) => {
                                                warn!("bridge tcp tls connection rejected: {err}")
                                            },
                                        }
                                    });
                                },
                                Err(err) => warn!("bridge tcp tls accept error: {err}"),
                            }
                        }
                    })
                    .map_err(|err| {
                        BridgeHostError::Io(format!("failed to spawn tcp tls accept loop: {err}"))
                    })?,
            );
        }

        for handle in handles {
            handle
                .join()
                .map_err(|_| BridgeHostError::Io("bridge accept loop panicked".to_string()))?;
        }

        Ok(())
    }

    fn frame_fanout_loop(
        rx: Receiver<PiperFrame>,
        sessions: Arc<SessionManager>,
        stats: Arc<BridgeHostStats>,
    ) {
        while let Ok(frame) = rx.recv() {
            stats.frame_rx_total.fetch_add(1, Ordering::Relaxed);
            let fanout = sessions.broadcast_frame(frame);
            if fanout.dropped > 0 {
                stats.queue_drop_total.fetch_add(fanout.dropped, Ordering::Relaxed);
            }
            if fanout.inactive > 0 {
                stats.inactive_enqueue_total.fetch_add(fanout.inactive, Ordering::Relaxed);
            }
        }
    }

    fn lease_event_loop(
        _backend: Arc<dyn BridgeControllerBackend>,
        sessions: Arc<SessionManager>,
        lease_events: Receiver<MaintenanceRevocationEvent>,
    ) {
        while let Ok(event) = lease_events.recv() {
            if let Some(holder) = sessions.active_session_by_key(SessionKey(event.session_key())) {
                holder.revoke_maintenance_lease();
            }
        }
    }

    fn build_status(
        backend: &dyn BridgeControllerBackend,
        sessions: &SessionManager,
        stats: &BridgeHostStats,
    ) -> BridgeStatus {
        let status_input = backend.status_snapshot();

        BridgeStatus {
            device_state: if status_input.health.connected {
                BridgeDeviceState::Connected
            } else if status_input.health.rx_alive || status_input.health.tx_alive {
                BridgeDeviceState::Reconnecting
            } else {
                BridgeDeviceState::Disconnected
            },
            rx_fps_x1000: (stats.frame_rx_fps() * 1000.0) as u32,
            tx_fps_x1000: (stats.maintenance_tx_fps() * 1000.0) as u32,
            ipc_out_fps_x1000: (stats.ipc_out_fps() * 1000.0) as u32,
            ipc_in_fps_x1000: (stats.ipc_in_fps() * 1000.0) as u32,
            health_score: stats
                .apply_queue_drop_penalty(backend_health_score(&status_input.health)),
            usb_stall_count: status_input.usb_stall_count,
            can_bus_off_count: status_input.can_bus_off_count,
            can_error_passive_count: status_input.can_error_passive_count,
            cpu_usage_percent: status_input.cpu_usage_percent,
            session_count: sessions.count(),
            queue_drop_count: stats.queue_drop_total.load(Ordering::Relaxed),
            inactive_enqueue_count: stats.inactive_enqueue_total.load(Ordering::Relaxed),
            session_replacement_discard_count: stats
                .session_replacement_discard_total
                .load(Ordering::Relaxed),
        }
    }

    fn reconcile_raw_tap(
        raw_tap: &Arc<Mutex<RawTapManager>>,
        sessions: &Arc<SessionManager>,
        backend: &Arc<dyn BridgeControllerBackend>,
    ) -> Result<(), BridgeHostError> {
        raw_tap.lock().unwrap().reconcile(sessions, backend)
    }

    fn build_tls_listener_config(
        config: &BridgeTlsServerConfig,
    ) -> Result<TlsListenerConfig, BridgeHostError> {
        if config.client_policies.is_empty() {
            return Err(BridgeHostError::InvalidConfig(
                "tcp tls requires at least one allowed client certificate fingerprint".to_string(),
            ));
        }

        let mut roots = RootCertStore::empty();
        let ca_certs = load_cert_chain(&config.client_ca_cert_pem)?;
        let (added, _) = roots.add_parsable_certificates(ca_certs);
        if added == 0 {
            return Err(BridgeHostError::InvalidConfig(
                "tcp tls client CA bundle did not contain any valid certificates".to_string(),
            ));
        }

        let verifier = WebPkiClientVerifier::builder(Arc::new(roots)).build().map_err(|err| {
            BridgeHostError::Listener(format!("failed to build tcp tls client verifier: {err}"))
        })?;
        let server_certs = load_cert_chain(&config.server_cert_pem)?;
        let server_key = load_private_key(&config.server_key_pem)?;
        let server_config = ServerConfig::builder()
            .with_client_cert_verifier(verifier)
            .with_single_cert(server_certs, server_key)
            .map_err(|err| {
                BridgeHostError::Listener(format!(
                    "failed to configure tcp tls server certs: {err}"
                ))
            })?;

        let mut client_roles = HashMap::with_capacity(config.client_policies.len());
        for policy in &config.client_policies {
            let fingerprint =
                parse_cert_fingerprint(&policy.fingerprint_sha256).map_err(|err| {
                    BridgeHostError::InvalidConfig(format!(
                        "invalid tls client fingerprint {}: {err}",
                        policy.fingerprint_sha256
                    ))
                })?;
            client_roles.insert(fingerprint, policy.granted_role);
        }

        Ok(TlsListenerConfig {
            server_config: Arc::new(server_config),
            client_roles: Arc::new(client_roles),
            handshake_timeout: config.handshake_timeout,
        })
    }

    fn accept_tls_stream(
        tcp_stream: TcpStream,
        tls_listener: &TlsListenerConfig,
    ) -> Result<(ServerStream, BridgeRole), BridgeHostError> {
        tcp_stream
            .set_read_timeout(Some(tls_listener.handshake_timeout))
            .map_err(|err| {
                BridgeHostError::Io(format!("failed to set tcp tls read timeout: {err}"))
            })?;
        tcp_stream
            .set_write_timeout(Some(tls_listener.handshake_timeout))
            .map_err(|err| {
                BridgeHostError::Io(format!("failed to set tcp tls write timeout: {err}"))
            })?;

        let connection =
            ServerConnection::new(Arc::clone(&tls_listener.server_config)).map_err(|err| {
                BridgeHostError::Listener(format!("failed to create tcp tls session: {err}"))
            })?;
        let mut stream = StreamOwned::new(connection, tcp_stream);
        while stream.conn.is_handshaking() {
            stream
                .conn
                .complete_io(&mut stream.sock)
                .map_err(|err| BridgeHostError::Io(format!("tcp tls handshake failed: {err}")))?;
        }

        let peer_certs = stream.conn.peer_certificates().ok_or_else(|| {
            BridgeHostError::Listener(
                "tcp tls peer did not present a client certificate".to_string(),
            )
        })?;
        let peer_cert = peer_certs.first().ok_or_else(|| {
            BridgeHostError::Listener("tcp tls peer certificate chain was empty".to_string())
        })?;
        let fingerprint = Sha256::digest(peer_cert.as_ref());
        let mut fingerprint_bytes = [0u8; 32];
        fingerprint_bytes.copy_from_slice(&fingerprint);
        let Some(granted_role) = tls_listener.client_roles.get(&fingerprint_bytes).copied() else {
            return Err(BridgeHostError::Listener(
                "tcp tls client certificate is not in the allowlist".to_string(),
            ));
        };

        let peer_label = stream
            .sock
            .peer_addr()
            .map(|addr| format!("tls://{addr}"))
            .unwrap_or_else(|_| "tls://peer".to_string());

        stream.sock.set_read_timeout(None).map_err(|err| {
            BridgeHostError::Io(format!("failed to clear tcp tls read timeout: {err}"))
        })?;
        stream.sock.set_write_timeout(None).map_err(|err| {
            BridgeHostError::Io(format!("failed to clear tcp tls write timeout: {err}"))
        })?;
        stream.sock.set_nonblocking(true).map_err(|err| {
            BridgeHostError::Io(format!("failed to set tcp tls nonblocking mode: {err}"))
        })?;

        let StreamOwned { conn, sock } = stream;
        Ok((
            ServerStream::TcpTls(Box::new(TlsTransport {
                socket: MioTcpStream::from_std(sock),
                conn,
                peer_label,
                read_buf: Vec::with_capacity(1024),
                current_write: None,
                current_post_flush: None,
                close_after_flush: false,
            })),
            granted_role,
        ))
    }

    fn queue_error_response(
        response_queue: &mut VecDeque<QueuedMessage>,
        request_id: u32,
        code: ErrorCode,
        message: impl Into<String>,
    ) {
        response_queue.push_back(QueuedMessage::response(ServerResponse::Error {
            request_id,
            code,
            message: message.into(),
        }));
    }

    fn has_local_outbound(
        control_queue: &VecDeque<QueuedMessage>,
        response_queue: &VecDeque<QueuedMessage>,
        event_queue: &VecDeque<QueuedMessage>,
        control_rx: Option<&Receiver<ConnectionOutput>>,
        event_rx: Option<&Receiver<ConnectionOutput>>,
    ) -> bool {
        !control_queue.is_empty()
            || !response_queue.is_empty()
            || !event_queue.is_empty()
            || control_rx.is_some_and(|rx| !rx.is_empty())
            || event_rx.is_some_and(|rx| !rx.is_empty())
    }

    fn drain_control_mailbox(
        control_rx: Option<&Receiver<ConnectionOutput>>,
        event_rx: Option<&Receiver<ConnectionOutput>>,
        control_queue: &mut VecDeque<QueuedMessage>,
        response_queue: &mut VecDeque<QueuedMessage>,
        event_queue: &mut VecDeque<QueuedMessage>,
        stats: &BridgeHostStats,
    ) -> bool {
        let Some(control_rx) = control_rx else {
            return false;
        };

        loop {
            match control_rx.try_recv() {
                Ok(ConnectionOutput::Event(event)) => {
                    control_queue.push_back(QueuedMessage::event(event));
                },
                Ok(ConnectionOutput::CloseAfterEvent(event)) => {
                    response_queue.clear();
                    control_queue.clear();
                    let mut discarded = event_queue.len() as u64;
                    event_queue.clear();
                    if let Some(event_rx) = event_rx {
                        loop {
                            match event_rx.try_recv() {
                                Ok(ConnectionOutput::Event(_)) => discarded += 1,
                                Ok(ConnectionOutput::CloseAfterEvent(_))
                                | Ok(ConnectionOutput::Shutdown) => {},
                                Err(crossbeam_channel::TryRecvError::Empty) => break,
                                Err(crossbeam_channel::TryRecvError::Disconnected) => break,
                            }
                        }
                    }
                    if discarded > 0 {
                        stats
                            .session_replacement_discard_total
                            .fetch_add(discarded, Ordering::Relaxed);
                    }
                    control_queue.push_front(QueuedMessage::close_after_event(event));
                },
                Ok(ConnectionOutput::Shutdown) => return true,
                Err(crossbeam_channel::TryRecvError::Empty) => return false,
                Err(crossbeam_channel::TryRecvError::Disconnected) => return true,
            }
        }
    }

    fn fill_event_queue(
        event_rx: Option<&Receiver<ConnectionOutput>>,
        event_queue: &mut VecDeque<QueuedMessage>,
        max_events: usize,
    ) {
        let Some(event_rx) = event_rx else {
            return;
        };
        while event_queue.len() < max_events {
            match event_rx.try_recv() {
                Ok(ConnectionOutput::Event(event)) => {
                    event_queue.push_back(QueuedMessage::event(event));
                },
                Ok(ConnectionOutput::CloseAfterEvent(_)) | Ok(ConnectionOutput::Shutdown) => {},
                Err(crossbeam_channel::TryRecvError::Empty)
                | Err(crossbeam_channel::TryRecvError::Disconnected) => break,
            }
        }
    }

    fn apply_post_flush_action(
        action: PostFlushAction,
        session: &mut Option<Arc<BridgeSession>>,
        control_rx: &mut Option<Receiver<ConnectionOutput>>,
        event_rx: &mut Option<Receiver<ConnectionOutput>>,
        ctx: &ConnectionContext<'_>,
    ) -> Result<(), BridgeHostError> {
        match action {
            PostFlushAction::CommitPrepared(prepared) => {
                let prepared_control_rx = prepared.control_rx.clone();
                let prepared_event_rx = prepared.event_rx.clone();
                let register = ctx.sessions.commit_prepared(prepared);
                *control_rx = Some(prepared_control_rx);
                *event_rx = Some(prepared_event_rx);
                *session = Some(Arc::clone(&register.session));
                if let Some(replaced) = register.replaced {
                    let replaced_authority =
                        SessionAuthority::new(Arc::clone(&replaced), ctx.sessions);
                    let _ = ctx.backend.release_maintenance_lease(&replaced_authority);
                    replaced.replace_and_close();
                }
                Self::reconcile_raw_tap(ctx.raw_tap, ctx.sessions, ctx.backend)?;
            },
        }
        Ok(())
    }

    fn run_connection_actor(
        mut stream: ServerStream,
        granted_role: BridgeRole,
        ctx: ConnectionContext<'_>,
    ) {
        let peer_label = stream.peer_label().to_string();
        let mut poll = match Poll::new() {
            Ok(poll) => poll,
            Err(err) => {
                warn!("failed to create bridge poll actor for {peer_label}: {err}");
                return;
            },
        };
        let waker = match Waker::new(poll.registry(), WAKE_TOKEN) {
            Ok(waker) => Arc::new(waker),
            Err(err) => {
                warn!("failed to create bridge actor waker for {peer_label}: {err}");
                return;
            },
        };
        let wake: Arc<dyn ConnectionWake> = Arc::new(ActorWake { waker });
        if let Err(err) = stream.register(poll.registry(), false) {
            warn!("failed to register bridge actor socket for {peer_label}: {err}");
            return;
        }
        let mut events = Events::with_capacity(8);

        let mut session: Option<Arc<BridgeSession>> = None;
        let mut control_rx: Option<Receiver<ConnectionOutput>> = None;
        let mut event_rx: Option<Receiver<ConnectionOutput>> = None;
        let mut control_queue: VecDeque<QueuedMessage> = VecDeque::new();
        let mut response_queue: VecDeque<QueuedMessage> = VecDeque::new();
        let mut event_queue: VecDeque<QueuedMessage> = VecDeque::new();
        let mut pending_post_flush: Option<PostFlushAction> = None;

        'actor: loop {
            let mut force_poll = false;
            let mut made_progress = false;
            let mut event_budget = MAX_EVENT_BURST;

            loop {
                if Self::drain_control_mailbox(
                    control_rx.as_ref(),
                    event_rx.as_ref(),
                    &mut control_queue,
                    &mut response_queue,
                    &mut event_queue,
                    ctx.stats,
                ) {
                    break 'actor;
                }

                if !stream.has_pending_write()
                    && let Some(post_flush) = pending_post_flush.take()
                {
                    if let Err(err) = Self::apply_post_flush_action(
                        post_flush,
                        &mut session,
                        &mut control_rx,
                        &mut event_rx,
                        &ctx,
                    ) {
                        warn!("failed to apply bridge post-flush action for {peer_label}: {err}");
                        break 'actor;
                    }
                    made_progress = true;
                    continue;
                }

                if !stream.has_pending_write() {
                    if let Some(queued) = control_queue.pop_front() {
                        if let Err(err) = stream.start_message(queued) {
                            warn!("failed to queue bridge control output for {peer_label}: {err}");
                            break 'actor;
                        }
                        made_progress = true;
                        continue;
                    }
                    if let Some(queued) = response_queue.pop_front() {
                        if let Err(err) = stream.start_message(queued) {
                            warn!("failed to queue bridge response for {peer_label}: {err}");
                            break 'actor;
                        }
                        made_progress = true;
                        continue;
                    }

                    if event_budget > 0 {
                        Self::fill_event_queue(event_rx.as_ref(), &mut event_queue, event_budget);
                    }
                    if let Some(queued) = event_queue.pop_front() {
                        if let Err(err) = stream.start_message(queued) {
                            warn!("failed to queue bridge event for {peer_label}: {err}");
                            break 'actor;
                        }
                        event_budget = event_budget.saturating_sub(1);
                        made_progress = true;
                        continue;
                    }
                }

                if stream.has_pending_write() {
                    match stream.flush_pending_write(ctx.stats) {
                        Ok(flush) => {
                            if let Some((_close_after, Some(post_flush))) = flush.completed {
                                pending_post_flush = Some(post_flush);
                            }
                            if stream.should_close_after_flush() {
                                stream.clear_close_after_flush();
                                break 'actor;
                            }
                            if flush.wrote_any {
                                made_progress = true;
                                continue;
                            }
                        },
                        Err(err) => {
                            warn!("bridge actor write failed for {peer_label}: {err}");
                            break 'actor;
                        },
                    }
                }

                if event_budget == 0
                    && Self::has_local_outbound(
                        &control_queue,
                        &response_queue,
                        &event_queue,
                        control_rx.as_ref(),
                        event_rx.as_ref(),
                    )
                {
                    force_poll = true;
                }
                break;
            }

            let wants_write = Self::has_local_outbound(
                &control_queue,
                &response_queue,
                &event_queue,
                control_rx.as_ref(),
                event_rx.as_ref(),
            ) || stream.has_pending_write();

            if !force_poll && made_progress && !stream.should_close_after_flush() && wants_write {
                continue;
            }

            if let Err(err) = stream.update_interest(poll.registry(), wants_write) {
                warn!("failed to update bridge actor interest for {peer_label}: {err}");
                break;
            }

            if let Err(err) = poll.poll(&mut events, None) {
                warn!("bridge actor poll failed for {peer_label}: {err}");
                break;
            }

            let mut socket_readable = false;
            let mut socket_writable = false;
            for event in &events {
                match event.token() {
                    SOCKET_TOKEN => {
                        socket_readable |= event.is_readable() || event.is_read_closed();
                        socket_writable |= event.is_writable() || event.is_write_closed();
                    },
                    WAKE_TOKEN => {},
                    _ => {},
                }
            }

            if socket_readable {
                let (payloads, saw_eof) = match stream.read_available() {
                    Ok(result) => result,
                    Err(err) => {
                        warn!("bridge actor read failed for {peer_label}: {err}");
                        break;
                    },
                };
                if saw_eof {
                    break;
                }

                for payload in payloads {
                    let request = match protocol::decode_client_request(&payload) {
                        Ok(request) => request,
                        Err(err) => {
                            ctx.warn_limiter.warn("protocol-decode-error", || {
                                format!("bridge protocol decode error from {peer_label}: {err}")
                            });
                            break 'actor;
                        },
                    };
                    ctx.stats.ipc_in_total.fetch_add(1, Ordering::Relaxed);

                    match request {
                        ClientRequest::Hello {
                            request_id,
                            session_token,
                            filters,
                        } => {
                            if session.is_some() {
                                Self::queue_error_response(
                                    &mut response_queue,
                                    request_id,
                                    ErrorCode::InvalidMessage,
                                    "hello already completed for this connection",
                                );
                                continue;
                            }

                            let prepared = ctx.sessions.prepare_session(
                                session_token,
                                granted_role,
                                filters,
                                Arc::clone(&wake),
                            );
                            response_queue.push_back(QueuedMessage::response_with_post_flush(
                                ServerResponse::HelloAck {
                                    request_id,
                                    session_id: prepared.session_id(),
                                    role_granted: prepared.role_granted(),
                                },
                                PostFlushAction::CommitPrepared(prepared),
                            ));
                        },
                        other => {
                            let Some(active_session) = session.as_ref().cloned() else {
                                let request_id = request_id_of(&other);
                                ctx.warn_limiter.warn("request-before-hello", || {
                                    format!(
                                        "rejecting {} from unauthenticated bridge connection {}",
                                        message_kind(&other),
                                        peer_label
                                    )
                                });
                                Self::queue_error_response(
                                    &mut response_queue,
                                    request_id,
                                    ErrorCode::NotConnected,
                                    "hello handshake required before requests",
                                );
                                continue;
                            };

                            let authority =
                                SessionAuthority::new(Arc::clone(&active_session), ctx.sessions);
                            if !authority.is_current() {
                                Self::queue_error_response(
                                    &mut response_queue,
                                    request_id_of(&other),
                                    ErrorCode::NotConnected,
                                    "bridge session was replaced or closed",
                                );
                                continue;
                            }

                            match other {
                                ClientRequest::GetStatus { request_id } => {
                                    let status = Self::build_status(
                                        ctx.backend.as_ref(),
                                        ctx.sessions,
                                        ctx.stats,
                                    );
                                    response_queue.push_back(QueuedMessage::response(
                                        ServerResponse::StatusResponse { request_id, status },
                                    ));
                                },
                                ClientRequest::SetFilters {
                                    request_id,
                                    filters,
                                } => {
                                    active_session.set_filters(filters);
                                    response_queue.push_back(QueuedMessage::response(
                                        ServerResponse::Ok { request_id },
                                    ));
                                },
                                ClientRequest::SetRawFrameTap {
                                    request_id,
                                    enabled,
                                } => {
                                    if !ctx.raw_tap.lock().unwrap().allowed() {
                                        Self::queue_error_response(
                                            &mut response_queue,
                                            request_id,
                                            ErrorCode::PermissionDenied,
                                            "raw frame tap is disabled by bridge host policy",
                                        );
                                        continue;
                                    }

                                    match ctx.raw_tap.lock().unwrap().set_session_subscription(
                                        ctx.sessions,
                                        ctx.backend,
                                        authority.session_key(),
                                        enabled,
                                    ) {
                                        Ok(()) => {
                                            response_queue.push_back(QueuedMessage::response(
                                                ServerResponse::Ok { request_id },
                                            ))
                                        },
                                        Err(RawTapUpdateError::NotConnected) => {
                                            Self::queue_error_response(
                                                &mut response_queue,
                                                request_id,
                                                ErrorCode::NotConnected,
                                                "bridge session was replaced or closed",
                                            )
                                        },
                                        Err(RawTapUpdateError::Backend(err)) => {
                                            Self::queue_error_response(
                                                &mut response_queue,
                                                request_id,
                                                ErrorCode::DeviceError,
                                                err.to_string(),
                                            )
                                        },
                                    }
                                },
                                ClientRequest::AcquireWriterLease {
                                    request_id,
                                    timeout_ms,
                                } => {
                                    if authority.role_granted() != BridgeRole::WriterCandidate {
                                        Self::queue_error_response(
                                            &mut response_queue,
                                            request_id,
                                            ErrorCode::PermissionDenied,
                                            "writer lease requires a WriterCandidate bridge role",
                                        );
                                        continue;
                                    }
                                    match ctx.backend.acquire_maintenance_lease(
                                        &authority,
                                        Duration::from_millis(timeout_ms as u64),
                                    ) {
                                        Ok(LeaseAcquireResult::Granted) => response_queue
                                            .push_back(QueuedMessage::response(
                                                ServerResponse::LeaseGranted {
                                                    request_id,
                                                    session_id: authority.session_id(),
                                                },
                                            )),
                                        Ok(LeaseAcquireResult::Denied { holder_session_id }) => {
                                            response_queue.push_back(QueuedMessage::response(
                                                ServerResponse::LeaseDenied {
                                                    request_id,
                                                    holder_session_id,
                                                },
                                            ));
                                        },
                                        Err(err) => Self::queue_error_response(
                                            &mut response_queue,
                                            request_id,
                                            err.code(),
                                            err.message(),
                                        ),
                                    }
                                },
                                ClientRequest::ReleaseWriterLease { request_id } => {
                                    let _ = ctx.backend.release_maintenance_lease(&authority);
                                    response_queue.push_back(QueuedMessage::response(
                                        ServerResponse::Ok { request_id },
                                    ));
                                },
                                ClientRequest::SendFrame { request_id, frame } => {
                                    match ctx.backend.send_maintenance_frame(&authority, frame) {
                                        Ok(()) => {
                                            ctx.stats
                                                .maintenance_tx_total
                                                .fetch_add(1, Ordering::Relaxed);
                                            response_queue.push_back(QueuedMessage::response(
                                                ServerResponse::Ok { request_id },
                                            ));
                                        },
                                        Err(err) => Self::queue_error_response(
                                            &mut response_queue,
                                            request_id,
                                            err.code(),
                                            err.message(),
                                        ),
                                    }
                                },
                                ClientRequest::Ping { request_id } => {
                                    response_queue.push_back(QueuedMessage::response(
                                        ServerResponse::Ok { request_id },
                                    ));
                                },
                                ClientRequest::Hello { .. } => unreachable!(),
                            }
                        },
                    }
                }
            } else if socket_writable {
                continue;
            }
        }

        if let Some(active_session) = session {
            let authority = SessionAuthority::new(Arc::clone(&active_session), ctx.sessions);
            let _ = ctx.backend.release_maintenance_lease(&authority);
            if let Some(removed) = ctx.sessions.unregister_session(active_session.session_id()) {
                removed.shutdown();
            } else {
                active_session.shutdown();
            }
            if let Err(err) = Self::reconcile_raw_tap(ctx.raw_tap, ctx.sessions, ctx.backend) {
                warn!("failed to reconcile raw frame tap after disconnect: {err}");
            }
        }
        stream.shutdown();
    }
}

fn backend_health_score(health: &HealthStatus) -> u8 {
    if health.fault.is_some() {
        40
    } else if health.connected {
        100
    } else if health.rx_alive || health.tx_alive {
        70
    } else {
        40
    }
}

fn bridge_backend_error_from_driver(error: &DriverError) -> BridgeBackendError {
    let code = match error {
        DriverError::Timeout
        | DriverError::RealtimeDeliveryTimeout
        | DriverError::ReliablePackageTimeout { .. } => ErrorCode::Timeout,
        DriverError::ChannelFull | DriverError::ShutdownConflict => ErrorCode::Busy,
        DriverError::ControlPathClosed
        | DriverError::ReplayModeActive
        | DriverError::CommandAbortedByFault
        | DriverError::CommandAbortedByStateTransition
        | DriverError::RealtimeDeliveryAbortedByFault { .. } => ErrorCode::PermissionDenied,
        DriverError::MaintenanceWriteDenied(_) => ErrorCode::PermissionDenied,
        DriverError::ChannelClosed
        | DriverError::IoThread(_)
        | DriverError::NotDualThread
        | DriverError::NotImplemented(_)
        | DriverError::InvalidInput(_) => ErrorCode::NotConnected,
        DriverError::Protocol(_) => ErrorCode::ProtocolError,
        DriverError::Can(can_error) => match can_error {
            piper_can::CanError::Timeout => ErrorCode::Timeout,
            piper_can::CanError::Device(device) => match device.kind {
                piper_can::CanDeviceErrorKind::NoDevice
                | piper_can::CanDeviceErrorKind::NotFound => ErrorCode::DeviceNotFound,
                piper_can::CanDeviceErrorKind::Busy => ErrorCode::Busy,
                piper_can::CanDeviceErrorKind::InvalidResponse => ErrorCode::ProtocolError,
                _ => ErrorCode::DeviceError,
            },
            _ => ErrorCode::DeviceError,
        },
        DriverError::PoisonedLock
        | DriverError::ReliableDeliveryFailed { .. }
        | DriverError::ReliablePackageDeliveryFailed { .. }
        | DriverError::RealtimeDeliveryFailed { .. }
        | DriverError::RealtimeDeliveryOverwritten => ErrorCode::DeviceError,
    };
    BridgeBackendError::new(code, error.to_string())
}

struct MaintenanceBroker {
    driver: Arc<RobotPiper>,
}

impl MaintenanceBroker {
    fn new(driver: Arc<RobotPiper>) -> Self {
        Self { driver }
    }

    fn ensure_current(&self, authority: &SessionAuthority) -> Result<(), BridgeBackendError> {
        if authority.is_current() {
            Ok(())
        } else {
            Err(BridgeBackendError::new(
                ErrorCode::NotConnected,
                "bridge session was replaced or closed",
            ))
        }
    }

    fn acquire(
        &self,
        authority: &SessionAuthority,
        timeout: Duration,
    ) -> Result<LeaseAcquireResult, BridgeBackendError> {
        self.ensure_current(authority)?;
        match self
            .driver
            .acquire_maintenance_lease_gate(
                authority.session_id(),
                authority.session_key().raw(),
                timeout,
            )
            .map_err(|err| bridge_backend_error_from_driver(&err))?
        {
            MaintenanceLeaseAcquireResult::Granted { .. } => {
                if !authority.is_current() {
                    let _ = self
                        .driver
                        .release_maintenance_lease_gate_if_holder(authority.session_key().raw());
                    return Err(BridgeBackendError::new(
                        ErrorCode::NotConnected,
                        "bridge session was replaced or closed",
                    ));
                }
                Ok(LeaseAcquireResult::Granted)
            },
            MaintenanceLeaseAcquireResult::DeniedHeld { holder_session_id } => {
                Ok(LeaseAcquireResult::Denied { holder_session_id })
            },
            MaintenanceLeaseAcquireResult::DeniedState { state } => {
                let current_state = BridgeMaintenanceState::from(state);
                Err(BridgeBackendError::new(
                    ErrorCode::PermissionDenied,
                    current_state.denial_message(),
                ))
            },
        }
    }

    fn release(&self, authority: &SessionAuthority) -> Result<bool, BridgeBackendError> {
        self.driver
            .release_maintenance_lease_gate_if_holder(authority.session_key().raw())
            .map_err(|err| bridge_backend_error_from_driver(&err))
    }

    fn issue_send_permit(
        &self,
        authority: &SessionAuthority,
    ) -> Result<(u32, u64, u64), BridgeBackendError> {
        self.ensure_current(authority)?;
        let snapshot = self.driver.maintenance_lease_snapshot();
        if snapshot.state() != MaintenanceGateState::AllowedStandby {
            return Err(BridgeBackendError::new(
                ErrorCode::PermissionDenied,
                BridgeMaintenanceState::from(snapshot.state()).denial_message(),
            ));
        }
        if snapshot.holder_session_key() != Some(authority.session_key().raw()) {
            return Err(BridgeBackendError::new(
                ErrorCode::PermissionDenied,
                "maintenance lease required",
            ));
        }
        Ok((
            authority.session_id(),
            authority.session_key().raw(),
            snapshot.lease_epoch(),
        ))
    }

    fn send(
        &self,
        authority: &SessionAuthority,
        frame: PiperFrame,
    ) -> Result<(), BridgeBackendError> {
        let (session_id, session_key, lease_epoch) = self.issue_send_permit(authority)?;

        self.driver
            .send_maintenance_frame_confirmed(
                session_id,
                session_key,
                lease_epoch,
                frame,
                Duration::from_millis(10),
            )
            .map_err(|err| {
                if matches!(
                    err,
                    DriverError::ControlPathClosed | DriverError::MaintenanceWriteDenied(_)
                ) {
                    let _ = self
                        .driver
                        .release_maintenance_lease_gate_if_holder(authority.session_key().raw());
                }
                bridge_backend_error_from_driver(&err)
            })
    }
}

struct PiperBridgeBackend {
    driver: Arc<RobotPiper>,
    broker: MaintenanceBroker,
}

impl PiperBridgeBackend {
    fn from_driver(driver: Arc<RobotPiper>) -> Arc<dyn BridgeControllerBackend> {
        Arc::new(Self {
            broker: MaintenanceBroker::new(Arc::clone(&driver)),
            driver,
        })
    }
}

impl BridgeControllerBackend for PiperBridgeBackend {
    fn status_snapshot(&self) -> BridgeStatusInput {
        BridgeStatusInput {
            health: self.driver.health(),
            usb_stall_count: 0,
            can_bus_off_count: 0,
            can_error_passive_count: 0,
            cpu_usage_percent: 0,
        }
    }

    fn register_maintenance_event_sink(
        &self,
        sink: Sender<MaintenanceRevocationEvent>,
    ) -> Result<(), BridgeBackendError> {
        self.driver.register_maintenance_event_sink(sink);
        Ok(())
    }

    fn acquire_maintenance_lease(
        &self,
        authority: &SessionAuthority,
        timeout: Duration,
    ) -> Result<LeaseAcquireResult, BridgeBackendError> {
        self.broker.acquire(authority, timeout)
    }

    fn release_maintenance_lease(
        &self,
        authority: &SessionAuthority,
    ) -> Result<bool, BridgeBackendError> {
        self.broker.release(authority)
    }

    fn send_maintenance_frame(
        &self,
        authority: &SessionAuthority,
        frame: PiperFrame,
    ) -> Result<(), BridgeBackendError> {
        self.broker.send(authority, frame)
    }

    fn install_raw_frame_tap(
        &self,
        tx: Sender<PiperFrame>,
    ) -> Result<RawTapSubscription, BridgeBackendError> {
        let callback = Arc::new(RawFrameTap { tx }) as Arc<dyn FrameCallback>;
        let handle = self
            .driver
            .hooks()
            .write()
            .map_err(|_| {
                BridgeBackendError::new(
                    ErrorCode::DeviceError,
                    "bridge hook registry lock poisoned",
                )
            })?
            .add_callback(callback);
        Ok(RawTapSubscription::new(DriverRawTapCleanup {
            hooks: self.driver.hooks(),
            handle,
        }))
    }
}

fn load_cert_chain(
    path: &std::path::Path,
) -> Result<Vec<rustls::pki_types::CertificateDer<'static>>, BridgeHostError> {
    let bytes = fs::read(path).map_err(|err| {
        BridgeHostError::Io(format!(
            "failed to read certificate bundle {}: {err}",
            path.display()
        ))
    })?;
    let mut cursor = Cursor::new(bytes);
    rustls_pemfile::certs(&mut cursor)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|err| {
            BridgeHostError::Io(format!(
                "failed to parse certificate bundle {}: {err}",
                path.display()
            ))
        })
}

fn load_private_key(path: &std::path::Path) -> Result<PrivateKeyDer<'static>, BridgeHostError> {
    let bytes = fs::read(path).map_err(|err| {
        BridgeHostError::Io(format!(
            "failed to read private key {}: {err}",
            path.display()
        ))
    })?;
    let mut cursor = Cursor::new(bytes);
    rustls_pemfile::private_key(&mut cursor)
        .map_err(|err| {
            BridgeHostError::Io(format!(
                "failed to parse private key {}: {err}",
                path.display()
            ))
        })?
        .ok_or_else(|| {
            BridgeHostError::Listener(format!("no private key found in {}", path.display()))
        })
}

fn parse_cert_fingerprint(input: &str) -> Result<[u8; 32], String> {
    let normalized: String = input.chars().filter(|ch| ch.is_ascii_hexdigit()).collect();
    if normalized.len() != 64 {
        return Err("expected 64 hex characters".to_string());
    }
    let decoded = Vec::from_hex(&normalized).map_err(|err| err.to_string())?;
    let mut fingerprint = [0u8; 32];
    fingerprint.copy_from_slice(&decoded);
    Ok(fingerprint)
}

fn request_id_of(request: &ClientRequest) -> u32 {
    match request {
        ClientRequest::Hello { request_id, .. }
        | ClientRequest::GetStatus { request_id }
        | ClientRequest::SetFilters { request_id, .. }
        | ClientRequest::SetRawFrameTap { request_id, .. }
        | ClientRequest::AcquireWriterLease { request_id, .. }
        | ClientRequest::ReleaseWriterLease { request_id }
        | ClientRequest::SendFrame { request_id, .. }
        | ClientRequest::Ping { request_id } => *request_id,
    }
}

fn message_kind(request: &ClientRequest) -> &'static str {
    match request {
        ClientRequest::Hello { .. } => "Hello",
        ClientRequest::GetStatus { .. } => "GetStatus",
        ClientRequest::SetFilters { .. } => "SetFilters",
        ClientRequest::SetRawFrameTap { .. } => "SetRawFrameTap",
        ClientRequest::AcquireWriterLease { .. } => "AcquireWriterLease",
        ClientRequest::ReleaseWriterLease { .. } => "ReleaseWriterLease",
        ClientRequest::SendFrame { .. } => "SendFrame",
        ClientRequest::Ping { .. } => "Ping",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use piper_can::{
        BackendCapability, CanError, ExtendedCanId, RealtimeTxAdapter, RxAdapter, StandardCanId,
    };
    use piper_protocol::ids::ID_JOINT_DRIVER_LOW_SPEED_BASE;
    use std::collections::VecDeque;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    struct NoopWake;

    impl ConnectionWake for NoopWake {
        fn wake(&self) {}
    }

    fn token(byte: u8) -> SessionToken {
        SessionToken::new([byte; 16])
    }

    #[test]
    fn raw_frame_tap_forwards_rx_only() {
        let (tx, rx) = bounded(4);
        let tap = RawFrameTap { tx };
        let rx_frame = PiperFrame::new_standard(0x251, [0x01]).unwrap();
        let tx_frame = PiperFrame::new_standard(0x155, [0x02]).unwrap();

        tap.on_frame(RecordedFrameEvent {
            frame: tx_frame,
            direction: piper_driver::recording::RecordedFrameDirection::Tx,
            timestamp_provenance: piper_driver::recording::TimestampProvenance::Userspace,
        });
        tap.on_frame(RecordedFrameEvent {
            frame: rx_frame,
            direction: piper_driver::recording::RecordedFrameDirection::Rx,
            timestamp_provenance: piper_driver::recording::TimestampProvenance::Kernel,
        });

        let frames: Vec<_> = rx.try_iter().collect();
        assert_eq!(frames, vec![rx_frame]);
    }

    struct TestRawTapCleanup {
        uninstalls: Arc<AtomicUsize>,
    }

    impl RawTapCleanup for TestRawTapCleanup {
        fn uninstall(&self) {
            self.uninstalls.fetch_add(1, Ordering::Relaxed);
        }
    }

    struct TestBridgeBackend {
        install_fail: AtomicBool,
        installs: Arc<AtomicUsize>,
        uninstalls: Arc<AtomicUsize>,
    }

    impl TestBridgeBackend {
        fn new() -> Self {
            Self {
                install_fail: AtomicBool::new(false),
                installs: Arc::new(AtomicUsize::new(0)),
                uninstalls: Arc::new(AtomicUsize::new(0)),
            }
        }
    }

    impl BridgeControllerBackend for TestBridgeBackend {
        fn status_snapshot(&self) -> BridgeStatusInput {
            BridgeStatusInput {
                health: HealthStatus {
                    connected: true,
                    last_feedback_age: Duration::ZERO,
                    rx_alive: true,
                    tx_alive: true,
                    fault: None,
                },
                usb_stall_count: 0,
                can_bus_off_count: 0,
                can_error_passive_count: 0,
                cpu_usage_percent: 0,
            }
        }

        fn register_maintenance_event_sink(
            &self,
            _sink: Sender<MaintenanceRevocationEvent>,
        ) -> Result<(), BridgeBackendError> {
            Ok(())
        }

        fn acquire_maintenance_lease(
            &self,
            _authority: &SessionAuthority,
            _timeout: Duration,
        ) -> Result<LeaseAcquireResult, BridgeBackendError> {
            Ok(LeaseAcquireResult::Denied {
                holder_session_id: None,
            })
        }

        fn release_maintenance_lease(
            &self,
            _authority: &SessionAuthority,
        ) -> Result<bool, BridgeBackendError> {
            Ok(false)
        }

        fn send_maintenance_frame(
            &self,
            _authority: &SessionAuthority,
            _frame: PiperFrame,
        ) -> Result<(), BridgeBackendError> {
            Ok(())
        }

        fn install_raw_frame_tap(
            &self,
            _tx: Sender<PiperFrame>,
        ) -> Result<RawTapSubscription, BridgeBackendError> {
            if self.install_fail.load(Ordering::Relaxed) {
                return Err(BridgeBackendError::new(
                    ErrorCode::DeviceError,
                    "tap install failed",
                ));
            }
            self.installs.fetch_add(1, Ordering::Relaxed);
            Ok(RawTapSubscription::new(TestRawTapCleanup {
                uninstalls: Arc::clone(&self.uninstalls),
            }))
        }
    }

    struct PanickingRxAdapter;

    impl RxAdapter for PanickingRxAdapter {
        fn receive(&mut self) -> std::result::Result<piper_can::ReceivedFrame, CanError> {
            panic!("test rx panic")
        }

        fn backend_capability(&self) -> BackendCapability {
            BackendCapability::SoftRealtime
        }
    }

    fn joint_driver_low_speed_frame(
        joint_index: u8,
        enabled: bool,
        timestamp_us: u64,
    ) -> PiperFrame {
        let id = ID_JOINT_DRIVER_LOW_SPEED_BASE + u32::from(joint_index.saturating_sub(1));
        let mut data = [0u8; 8];
        data[0..2].copy_from_slice(&240u16.to_be_bytes());
        data[2..4].copy_from_slice(&45i16.to_be_bytes());
        data[4] = 50;
        data[5] = if enabled { 0x40 } else { 0x00 };
        data[6..8].copy_from_slice(&5000u16.to_be_bytes());
        PiperFrame::new_standard(id, data).unwrap().with_timestamp_us(timestamp_us)
    }

    fn standard_filter(min: u32, max: u32) -> CanIdFilter {
        CanIdFilter::standard(
            StandardCanId::new(min).unwrap(),
            StandardCanId::new(max).unwrap(),
        )
        .unwrap()
    }

    struct BootstrappedFeedbackRxAdapter {
        frames: VecDeque<PiperFrame>,
    }

    impl RxAdapter for BootstrappedFeedbackRxAdapter {
        fn receive(&mut self) -> std::result::Result<piper_can::ReceivedFrame, CanError> {
            self.frames
                .pop_front()
                .map(|frame| {
                    piper_can::ReceivedFrame::new(frame, piper_can::TimestampProvenance::None)
                })
                .ok_or(CanError::Timeout)
        }

        fn backend_capability(&self) -> BackendCapability {
            BackendCapability::SoftRealtime
        }
    }

    struct NoopTxAdapter;

    impl RealtimeTxAdapter for NoopTxAdapter {
        fn send_control(
            &mut self,
            _frame: PiperFrame,
            budget: Duration,
        ) -> std::result::Result<(), CanError> {
            if budget.is_zero() {
                return Err(CanError::Timeout);
            }
            Ok(())
        }

        fn send_shutdown_until(
            &mut self,
            _frame: PiperFrame,
            deadline: Instant,
        ) -> std::result::Result<(), CanError> {
            if deadline <= Instant::now() {
                return Err(CanError::Timeout);
            }
            Ok(())
        }
    }

    #[test]
    fn bridge_maintenance_state_maps_driver_unknown_state() {
        assert_eq!(
            BridgeMaintenanceState::from(MaintenanceGateState::DeniedDriveStateUnknown),
            BridgeMaintenanceState::DeniedDriveStateUnknown
        );
    }

    #[test]
    fn bridge_maintenance_denial_message_matches_driver_unknown_state() {
        assert_eq!(
            BridgeMaintenanceState::DeniedDriveStateUnknown.denial_message(),
            MaintenanceGateState::DeniedDriveStateUnknown.denial_message()
        );
    }

    #[test]
    fn maintenance_broker_reports_denied_faulted_after_rx_panic() {
        let driver = Arc::new(
            RobotPiper::new_dual_thread_parts(PanickingRxAdapter, NoopTxAdapter, None)
                .expect("driver should start without strict startup validation"),
        );
        driver.set_maintenance_gate_state(MaintenanceGateState::AllowedStandby);

        let deadline = Instant::now() + Duration::from_millis(200);
        while driver.health().rx_alive {
            assert!(
                Instant::now() < deadline,
                "RX panic should become visible before broker acquire test begins"
            );
            std::thread::yield_now();
        }

        let manager = Arc::new(SessionManager::new());
        let register = manager.commit_prepared(manager.prepare_session(
            token(7),
            BridgeRole::WriterCandidate,
            vec![],
            Arc::new(NoopWake),
        ));
        let authority = SessionAuthority::new(register.session, &manager);
        let broker = MaintenanceBroker::new(Arc::clone(&driver));

        let err = broker
            .acquire(&authority, Duration::from_millis(10))
            .expect_err("RX-dead runtime must deny maintenance lease acquisition");

        assert_eq!(err.code(), ErrorCode::PermissionDenied);
        assert_eq!(
            err.message(),
            BridgeMaintenanceState::DeniedFaulted.denial_message()
        );
    }

    #[test]
    fn maintenance_broker_send_fails_closed_when_runtime_closes_after_lease_grant() {
        let driver = Arc::new(
            RobotPiper::new_dual_thread_parts(
                BootstrappedFeedbackRxAdapter {
                    frames: (1..=6)
                        .map(|joint_index| {
                            joint_driver_low_speed_frame(joint_index, false, joint_index as u64)
                        })
                        .collect(),
                },
                NoopTxAdapter,
                None,
            )
            .expect("driver should start without strict startup validation"),
        );
        let deadline = Instant::now() + Duration::from_millis(200);
        while driver.maintenance_lease_snapshot().state() != MaintenanceGateState::AllowedStandby {
            assert!(
                Instant::now() < deadline,
                "bootstrap low-speed feedback should make maintenance standby confirmed before acquiring lease"
            );
            std::thread::yield_now();
        }

        let manager = Arc::new(SessionManager::new());
        let register = manager.commit_prepared(manager.prepare_session(
            token(8),
            BridgeRole::WriterCandidate,
            vec![],
            Arc::new(NoopWake),
        ));
        let authority = SessionAuthority::new(register.session, &manager);
        let broker = MaintenanceBroker::new(Arc::clone(&driver));

        assert_eq!(
            broker
                .acquire(&authority, Duration::from_millis(10))
                .expect("initial maintenance acquire should succeed"),
            LeaseAcquireResult::Granted
        );

        driver.set_mode(piper_driver::DriverMode::Replay);
        let snapshot = driver.maintenance_lease_snapshot();
        assert_eq!(snapshot.state(), MaintenanceGateState::DeniedFaulted);
        assert_eq!(snapshot.holder_session_key(), None);

        let err = broker
            .send(&authority, PiperFrame::new_standard(0x321, [0x01]).unwrap())
            .expect_err("runtime-closed maintenance send must be rejected before driver send");

        assert_eq!(err.code(), ErrorCode::PermissionDenied);
        assert_eq!(
            err.message(),
            BridgeMaintenanceState::DeniedFaulted.denial_message()
        );
    }

    #[test]
    fn same_token_replacement_invalidates_old_session_without_counting_it_as_active() {
        let manager = Arc::new(SessionManager::new());
        let first = manager.commit_prepared(manager.prepare_session(
            token(1),
            BridgeRole::WriterCandidate,
            vec![],
            Arc::new(NoopWake),
        ));

        let second = manager.commit_prepared(manager.prepare_session(
            token(1),
            BridgeRole::WriterCandidate,
            vec![],
            Arc::new(NoopWake),
        ));

        assert!(first.replaced.is_none());
        assert!(second.replaced.is_some());
        let old = second.replaced.as_ref().unwrap().clone();
        old.replace_and_close();
        assert!(!old.is_active());
        assert!(second.session.is_active());
        assert!(!manager.set_filters(old.session_id(), vec![standard_filter(0x100, 0x1FF)]));
        assert!(manager.set_filters(
            second.session.session_id(),
            vec![standard_filter(0x100, 0x1FF)]
        ));
    }

    #[test]
    fn session_authority_is_invalidated_by_same_token_replacement() {
        let manager = Arc::new(SessionManager::new());
        let register = manager.commit_prepared(manager.prepare_session(
            token(2),
            BridgeRole::WriterCandidate,
            vec![],
            Arc::new(NoopWake),
        ));
        let authority = SessionAuthority::new(Arc::clone(&register.session), &manager);
        assert!(authority.is_current());

        let replacement = manager.commit_prepared(manager.prepare_session(
            token(2),
            BridgeRole::WriterCandidate,
            vec![],
            Arc::new(NoopWake),
        ));
        assert!(replacement.replaced.is_some());
        assert!(!authority.is_current());
        assert!(SessionAuthority::new(replacement.session, &manager).is_current());
    }

    #[test]
    fn same_token_replacement_clears_raw_tap_subscription() {
        let manager = Arc::new(SessionManager::new());
        let first = manager.commit_prepared(manager.prepare_session(
            token(9),
            BridgeRole::Observer,
            vec![],
            Arc::new(NoopWake),
        ));
        assert!(manager.set_raw_tap_subscription(first.session.session_key(), true));
        assert_eq!(manager.raw_tap_subscriber_count(), 1);

        let second = manager.commit_prepared(manager.prepare_session(
            token(9),
            BridgeRole::Observer,
            vec![],
            Arc::new(NoopWake),
        ));
        assert!(second.replaced.is_some());
        assert_eq!(manager.raw_tap_subscriber_count(), 0);
        assert!(!manager.is_raw_tap_subscribed(first.session.session_key()));
        assert!(!manager.is_raw_tap_subscribed(second.session.session_key()));
    }

    #[test]
    fn replace_and_close_uses_priority_control_queue() {
        let (event_tx, _event_rx) = SessionManager::new_connection_queue();
        let (control_tx, control_rx) = crossbeam_channel::unbounded();
        let session = BridgeSession::new(
            1,
            SessionKey(1),
            token(2),
            BridgeRole::WriterCandidate,
            vec![],
            event_tx,
            control_tx,
            Arc::new(NoopWake),
        );

        for _ in 0..OUTBOUND_QUEUE_CAPACITY {
            assert_eq!(
                session.enqueue_frame(PiperFrame::new_standard(0x123, [1]).unwrap()),
                EnqueueFrameResult::Delivered
            );
        }

        session.replace_and_close();
        assert_eq!(session.lifecycle(), SessionLifecycle::Replaced);
        match control_rx.try_recv().expect("priority close event should be queued") {
            ConnectionOutput::CloseAfterEvent(BridgeEvent::SessionReplaced) => {},
            other => panic!("unexpected control output: {other:?}"),
        }
    }

    #[test]
    fn inactive_sessions_do_not_count_as_queue_drops() {
        let manager = Arc::new(SessionManager::new());
        let register = manager.commit_prepared(manager.prepare_session(
            token(3),
            BridgeRole::Observer,
            vec![],
            Arc::new(NoopWake),
        ));
        assert!(manager.set_raw_tap_subscription(register.session.session_key(), true));
        register.session.mark_closing();

        assert_eq!(
            manager.broadcast_frame(PiperFrame::new_standard(0x120, [1, 2, 3]).unwrap()),
            BroadcastFrameStats {
                dropped: 0,
                inactive: 1,
            }
        );
    }

    #[test]
    fn raw_tap_dispatch_routes_standard_and_extended_same_raw_id_separately() {
        let manager = Arc::new(SessionManager::new());
        let standard_filter = CanIdFilter::standard(
            StandardCanId::new(0x123).unwrap(),
            StandardCanId::new(0x123).unwrap(),
        )
        .unwrap();
        let extended_filter = CanIdFilter::extended(
            ExtendedCanId::new(0x123).unwrap(),
            ExtendedCanId::new(0x123).unwrap(),
        )
        .unwrap();

        let standard_prepared = manager.prepare_session(
            token(10),
            BridgeRole::Observer,
            vec![standard_filter],
            Arc::new(NoopWake),
        );
        let standard_rx = standard_prepared.event_rx.clone();
        let standard = manager.commit_prepared(standard_prepared);
        assert!(manager.set_raw_tap_subscription(standard.session.session_key(), true));

        let extended_prepared = manager.prepare_session(
            token(11),
            BridgeRole::Observer,
            vec![extended_filter],
            Arc::new(NoopWake),
        );
        let extended_rx = extended_prepared.event_rx.clone();
        let extended = manager.commit_prepared(extended_prepared);
        assert!(manager.set_raw_tap_subscription(extended.session.session_key(), true));

        assert_eq!(
            manager.broadcast_frame(PiperFrame::new_standard(0x123, [1]).unwrap()),
            BroadcastFrameStats {
                dropped: 0,
                inactive: 0
            }
        );
        assert_eq!(
            manager.broadcast_frame(PiperFrame::new_extended(0x123, &[2]).unwrap()),
            BroadcastFrameStats {
                dropped: 0,
                inactive: 0
            }
        );

        match standard_rx.try_recv().expect("standard session should receive standard frame") {
            ConnectionOutput::Event(BridgeEvent::ReceiveFrame(frame)) => {
                assert!(frame.is_standard());
                assert_eq!(frame.raw_id(), 0x123);
            },
            other => panic!("unexpected standard output: {other:?}"),
        }
        assert!(standard_rx.try_recv().is_err());

        match extended_rx.try_recv().expect("extended session should receive extended frame") {
            ConnectionOutput::Event(BridgeEvent::ReceiveFrame(frame)) => {
                assert!(frame.is_extended());
                assert_eq!(frame.raw_id(), 0x123);
            },
            other => panic!("unexpected extended output: {other:?}"),
        }
        assert!(extended_rx.try_recv().is_err());
    }

    #[test]
    fn failed_raw_tap_enable_does_not_arm_later_subscription() {
        let manager = Arc::new(SessionManager::new());
        let register = manager.commit_prepared(manager.prepare_session(
            token(4),
            BridgeRole::Observer,
            vec![],
            Arc::new(NoopWake),
        ));
        let backend = Arc::new(TestBridgeBackend::new());
        backend.install_fail.store(true, Ordering::Relaxed);
        let (tx, _rx) = bounded::<PiperFrame>(RAW_FRAME_TAP_QUEUE_CAPACITY);
        let mut raw_tap = RawTapManager::new(true, tx);

        assert!(matches!(
            raw_tap.set_session_subscription(
                &manager,
                &(backend.clone() as Arc<dyn BridgeControllerBackend>),
                register.session.session_key(),
                true,
            ),
            Err(RawTapUpdateError::Backend(_))
        ));
        assert_eq!(manager.raw_tap_subscriber_count(), 0);
        assert!(!manager.is_raw_tap_subscribed(register.session.session_key()));

        backend.install_fail.store(false, Ordering::Relaxed);
        raw_tap
            .reconcile(
                &manager,
                &(backend.clone() as Arc<dyn BridgeControllerBackend>),
            )
            .expect("reconcile without subscribers should succeed");
        assert_eq!(backend.installs.load(Ordering::Relaxed), 0);
        assert_eq!(backend.uninstalls.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn supported_bridge_endpoint_count_ignores_uds_when_platform_does_not_support_it() {
        let config = BridgeHostConfig {
            uds: Some(BridgeUdsListenerConfig {
                path: PathBuf::from("/tmp/piper_bridge.sock"),
                granted_role: BridgeRole::Observer,
            }),
            tcp_tls: None,
            allow_raw_frame_tap: false,
        };

        assert_eq!(supported_bridge_endpoint_count(&config, false), 0);
        assert_eq!(supported_bridge_endpoint_count(&config, true), 1);
    }
}
