//! Controller-owned non-realtime bridge host.
//!
//! The bridge host is intentionally separate from the realtime control path:
//! it reads committed driver state and optional raw frame taps, and all
//! maintenance writes are mediated through driver-level runtime checks.

use crossbeam_channel::{Receiver, Sender, bounded};
use hex::FromHex;
use piper_can::PiperFrame;
use piper_can::bridge::protocol::{
    self, BridgeDeviceState, BridgeEvent, BridgeRole, BridgeStatus, CanIdFilter, ClientRequest,
    ErrorCode, ServerMessage, ServerResponse, SessionToken,
};
use piper_driver::hooks::FrameCallback;
use piper_driver::{DriverError, HealthStatus, HookHandle, Piper as RobotPiper};
use rustls::pki_types::PrivateKeyDer;
use rustls::server::{ServerConfig, ServerConnection, WebPkiClientVerifier};
use rustls::{RootCertStore, StreamOwned};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;
use std::io::Cursor;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
#[cfg(unix)]
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::thread;
use std::time::{Duration, Instant};
use tracing::{error, warn};

const OUTBOUND_QUEUE_CAPACITY: usize = 256;
const RAW_FRAME_TAP_QUEUE_CAPACITY: usize = 1024;
const AUTH_LOG_WINDOW: Duration = Duration::from_secs(1);
const TLS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const TLS_IO_POLL_INTERVAL: Duration = Duration::from_millis(5);

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
}

impl BridgeMaintenanceState {
    fn allows_lease(self) -> bool {
        matches!(self, Self::AllowedStandby)
    }

    fn denial_message(self) -> &'static str {
        match self {
            Self::DeniedFaulted => {
                "maintenance writes are disabled while a runtime fault is latched"
            },
            Self::DeniedActiveControl => {
                "maintenance writes are disabled while active control is enabled"
            },
            Self::DeniedTransportDown => {
                "maintenance writes are disabled while transport is disconnected"
            },
            Self::AllowedStandby => "maintenance allowed",
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
    fn acquire_maintenance_lease(
        &self,
        session_id: u32,
        timeout: Duration,
    ) -> Result<LeaseAcquireResult, BridgeBackendError>;
    fn release_maintenance_lease(&self, session_id: u32) -> Result<bool, BridgeBackendError>;
    fn wait_for_lease_revocation(&self) -> Result<u32, BridgeBackendError>;
    fn send_maintenance_frame(
        &self,
        session_id: u32,
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

trait SessionControl: Send + Sync {
    fn shutdown(&self);
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

enum ServerStream {
    #[cfg(unix)]
    Unix(UnixStream),
    Tcp(TcpStream),
    TcpTls(Box<StreamOwned<ServerConnection, TcpStream>>),
}

impl ServerStream {
    fn try_clone(&self) -> std::io::Result<Self> {
        match self {
            #[cfg(unix)]
            Self::Unix(stream) => Ok(Self::Unix(stream.try_clone()?)),
            Self::Tcp(stream) => Ok(Self::Tcp(stream.try_clone()?)),
            Self::TcpTls(_) => Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "tls bridge streams are not cloneable",
            )),
        }
    }

    fn shutdown(&self) {
        match self {
            #[cfg(unix)]
            Self::Unix(stream) => {
                let _ = stream.shutdown(std::net::Shutdown::Both);
            },
            Self::Tcp(stream) => {
                let _ = stream.shutdown(std::net::Shutdown::Both);
            },
            Self::TcpTls(stream) => {
                let _ = stream.sock.shutdown(std::net::Shutdown::Both);
            },
        }
    }

    fn peer_label(&self) -> String {
        match self {
            #[cfg(unix)]
            Self::Unix(stream) => format!("{:?}", stream.peer_addr()),
            Self::Tcp(stream) => stream
                .peer_addr()
                .map(|addr| addr.to_string())
                .unwrap_or_else(|_| "<tcp-peer-unknown>".to_string()),
            Self::TcpTls(stream) => stream
                .sock
                .peer_addr()
                .map(|addr| addr.to_string())
                .unwrap_or_else(|_| "<tcp-tls-peer-unknown>".to_string()),
        }
    }

    fn set_read_timeout(&self, timeout: Option<Duration>) -> std::io::Result<()> {
        match self {
            #[cfg(unix)]
            Self::Unix(stream) => stream.set_read_timeout(timeout),
            Self::Tcp(stream) => stream.set_read_timeout(timeout),
            Self::TcpTls(stream) => stream.sock.set_read_timeout(timeout),
        }
    }
}

impl Read for ServerStream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            #[cfg(unix)]
            Self::Unix(stream) => stream.read(buf),
            Self::Tcp(stream) => stream.read(buf),
            Self::TcpTls(stream) => stream.read(buf),
        }
    }
}

impl Write for ServerStream {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            #[cfg(unix)]
            Self::Unix(stream) => stream.write(buf),
            Self::Tcp(stream) => stream.write(buf),
            Self::TcpTls(stream) => stream.write(buf),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            #[cfg(unix)]
            Self::Unix(stream) => stream.flush(),
            Self::Tcp(stream) => stream.flush(),
            Self::TcpTls(stream) => stream.flush(),
        }
    }
}

struct StreamControl {
    writer: Arc<Mutex<ServerStream>>,
}

impl SessionControl for StreamControl {
    fn shutdown(&self) {
        self.writer.lock().unwrap().shutdown();
    }
}

struct TcpShutdownControl {
    sock: Mutex<TcpStream>,
}

impl SessionControl for TcpShutdownControl {
    fn shutdown(&self) {
        let _ = self.sock.lock().unwrap().shutdown(std::net::Shutdown::Both);
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
    fn on_frame_received(&self, frame: &PiperFrame) {
        let _ = self.tx.try_send(*frame);
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
        let desired = self.allow && sessions.raw_frame_tap_subscriber_count() > 0;
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
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct BroadcastFrameStats {
    dropped: u64,
    inactive: u64,
}

struct BridgeSession {
    session_id: u32,
    session_token: SessionToken,
    role_granted: BridgeRole,
    filters: RwLock<Vec<CanIdFilter>>,
    event_tx: Sender<ConnectionOutput>,
    control_tx: Sender<ConnectionOutput>,
    pending_gap: AtomicU32,
    lifecycle: AtomicU8,
    replacement_close_queued: AtomicBool,
    raw_frame_tap_enabled: AtomicBool,
    control: Arc<dyn SessionControl>,
}

impl BridgeSession {
    fn new(
        session_id: u32,
        session_token: SessionToken,
        role_granted: BridgeRole,
        filters: Vec<CanIdFilter>,
        event_tx: Sender<ConnectionOutput>,
        control_tx: Sender<ConnectionOutput>,
        control: Arc<dyn SessionControl>,
    ) -> Self {
        Self {
            session_id,
            session_token,
            role_granted,
            filters: RwLock::new(filters),
            event_tx,
            control_tx,
            pending_gap: AtomicU32::new(0),
            lifecycle: AtomicU8::new(SessionLifecycle::Active as u8),
            replacement_close_queued: AtomicBool::new(false),
            raw_frame_tap_enabled: AtomicBool::new(false),
            control,
        }
    }

    fn session_id(&self) -> u32 {
        self.session_id
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

    fn is_active(&self) -> bool {
        self.lifecycle() == SessionLifecycle::Active
    }

    fn mark_replaced(&self) -> bool {
        self.lifecycle
            .compare_exchange(
                SessionLifecycle::Active as u8,
                SessionLifecycle::Replaced as u8,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    fn mark_closing(&self) {
        self.lifecycle.store(SessionLifecycle::Closing as u8, Ordering::Release);
    }

    fn mark_closed(&self) {
        self.lifecycle.store(SessionLifecycle::Closed as u8, Ordering::Release);
    }

    fn set_filters(&self, filters: Vec<CanIdFilter>) {
        *self.filters.write().unwrap() = filters;
    }

    fn raw_frame_tap_enabled(&self) -> bool {
        self.raw_frame_tap_enabled.load(Ordering::Acquire)
    }

    fn set_raw_frame_tap_enabled(&self, enabled: bool) {
        self.raw_frame_tap_enabled.store(enabled, Ordering::Release);
    }

    fn matches_filter(&self, can_id: u32) -> bool {
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
            Ok(()) => EnqueueFrameResult::Delivered,
            Err(_) => {
                self.pending_gap.fetch_add(1, Ordering::AcqRel);
                EnqueueFrameResult::QueueFull
            },
        }
    }

    fn send_priority_event(&self, event: BridgeEvent) {
        if self.control_tx.try_send(ConnectionOutput::Event(event)).is_err() {
            self.control.shutdown();
        }
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
            self.control.shutdown();
        }
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
        self.control.shutdown();
    }
}

struct PreparedSession {
    session_id: u32,
    session_token: SessionToken,
    role_granted: BridgeRole,
    filters: Vec<CanIdFilter>,
    event_tx: Sender<ConnectionOutput>,
    event_rx: Receiver<ConnectionOutput>,
    control_tx: Sender<ConnectionOutput>,
    control_rx: Receiver<ConnectionOutput>,
    control: Arc<dyn SessionControl>,
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
}

struct SessionManager {
    registry: RwLock<Registry>,
    next_session_id: AtomicU32,
}

impl SessionManager {
    fn new() -> Self {
        Self {
            registry: RwLock::new(Registry {
                sessions: HashMap::new(),
                token_to_session: HashMap::new(),
            }),
            next_session_id: AtomicU32::new(1),
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

    fn prepare_session(
        &self,
        session_token: SessionToken,
        role_granted: BridgeRole,
        filters: Vec<CanIdFilter>,
        control: Arc<dyn SessionControl>,
    ) -> PreparedSession {
        let (event_tx, event_rx) = Self::new_connection_queue();
        let (control_tx, control_rx) = Self::new_connection_queue();
        PreparedSession {
            session_id: self.next_session_id(),
            session_token,
            role_granted,
            filters,
            event_tx,
            event_rx,
            control_tx,
            control_rx,
            control,
        }
    }

    fn commit_prepared(&self, prepared: PreparedSession) -> RegisterResult {
        let mut registry = self.registry.write().unwrap();

        let replaced = registry
            .token_to_session
            .remove(&prepared.session_token)
            .and_then(|old_id| registry.sessions.remove(&old_id));

        if let Some(ref old_session) = replaced {
            old_session.mark_replaced();
        }

        let session = Arc::new(BridgeSession::new(
            prepared.session_id,
            prepared.session_token,
            prepared.role_granted,
            prepared.filters,
            prepared.event_tx,
            prepared.control_tx,
            prepared.control,
        ));
        registry.token_to_session.insert(prepared.session_token, prepared.session_id);
        registry.sessions.insert(prepared.session_id, Arc::clone(&session));
        drop(registry);

        RegisterResult { session, replaced }
    }

    fn unregister_session(&self, session_id: u32) -> Option<Arc<BridgeSession>> {
        let removed = {
            let mut registry = self.registry.write().unwrap();
            let removed = registry.sessions.remove(&session_id)?;
            if registry.token_to_session.get(&removed.session_token()).copied() == Some(session_id)
            {
                registry.token_to_session.remove(&removed.session_token());
            }
            Some(removed)
        };

        if let Some(ref session) = removed {
            session.mark_closed();
        }
        removed
    }

    fn session(&self, session_id: u32) -> Option<Arc<BridgeSession>> {
        self.registry.read().unwrap().sessions.get(&session_id).map(Arc::clone)
    }

    fn active_session(&self, session_id: u32) -> Option<Arc<BridgeSession>> {
        self.session(session_id).filter(|session| session.is_active())
    }

    fn count(&self) -> u32 {
        self.registry.read().unwrap().sessions.len() as u32
    }

    fn raw_frame_tap_subscriber_count(&self) -> usize {
        self.registry
            .read()
            .unwrap()
            .sessions
            .values()
            .filter(|session| session.raw_frame_tap_enabled())
            .count()
    }

    fn broadcast_frame(&self, frame: PiperFrame) -> BroadcastFrameStats {
        let registry = self.registry.read().unwrap();
        let mut stats = BroadcastFrameStats::default();
        for session in registry.sessions.values() {
            if !session.raw_frame_tap_enabled() || !session.matches_filter(frame.id) {
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

    fn set_filters(&self, session_id: u32, filters: Vec<CanIdFilter>) -> bool {
        if let Some(session) = self.active_session(session_id) {
            session.set_filters(filters);
            true
        } else {
            false
        }
    }

    fn set_raw_frame_tap(&self, session_id: u32, enabled: bool) -> bool {
        if let Some(session) = self.active_session(session_id) {
            session.set_raw_frame_tap_enabled(enabled);
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
        if self.config.uds.is_none() && self.config.tcp_tls.is_none() {
            return Err(BridgeHostError::InvalidConfig(
                "at least one bridge endpoint must be enabled".to_string(),
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
            let backend = Arc::clone(&self.backend);
            let sessions = Arc::clone(&self.sessions);
            thread::Builder::new()
                .name("bridge_lease_revoker".into())
                .spawn(move || Self::lease_revocation_loop(backend, sessions))
                .map_err(|err| {
                    BridgeHostError::Io(format!("failed to spawn lease revocation loop: {err}"))
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
                                    let backend = Arc::clone(&backend);
                                    let sessions = Arc::clone(&sessions);
                                    let raw_tap_manager = Arc::clone(&raw_tap_manager);
                                    let stats = Arc::clone(&stats);
                                    let warn_limiter = Arc::clone(&warn_limiter);
                                    thread::spawn(move || {
                                        let reader = ServerStream::Unix(stream);
                                        let writer = Arc::new(Mutex::new(
                                            reader.try_clone().expect("failed to clone stream"),
                                        ));
                                        Self::handle_connection(
                                            reader,
                                            writer,
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
                                            Ok((stream, control, granted_role)) => {
                                                Self::handle_tls_connection(
                                                    stream,
                                                    control,
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

    fn lease_revocation_loop(
        backend: Arc<dyn BridgeControllerBackend>,
        sessions: Arc<SessionManager>,
    ) {
        loop {
            match backend.wait_for_lease_revocation() {
                Ok(holder_session_id) => {
                    if let Some(holder) = sessions.session(holder_session_id) {
                        holder.revoke_maintenance_lease();
                    }
                },
                Err(err) => {
                    warn!("failed to wait for maintenance lease revocation: {err}");
                },
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
    ) -> Result<(ServerStream, Arc<dyn SessionControl>, BridgeRole), BridgeHostError> {
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
        let shutdown_sock = tcp_stream.try_clone().map_err(|err| {
            BridgeHostError::Io(format!("failed to clone tcp tls shutdown socket: {err}"))
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

        stream.sock.set_read_timeout(None).map_err(|err| {
            BridgeHostError::Io(format!("failed to clear tcp tls read timeout: {err}"))
        })?;
        stream.sock.set_write_timeout(None).map_err(|err| {
            BridgeHostError::Io(format!("failed to clear tcp tls write timeout: {err}"))
        })?;

        Ok((
            ServerStream::TcpTls(Box::new(stream)),
            Arc::new(TcpShutdownControl {
                sock: Mutex::new(shutdown_sock),
            }),
            granted_role,
        ))
    }

    fn spawn_writer_thread(
        writer: Arc<Mutex<ServerStream>>,
        control_rx: Receiver<ConnectionOutput>,
        event_rx: Receiver<ConnectionOutput>,
        stats: Arc<BridgeHostStats>,
        session: Arc<BridgeSession>,
    ) -> Result<(), BridgeHostError> {
        thread::Builder::new()
            .name("bridge_writer".into())
            .spawn(move || {
                loop {
                    crossbeam_channel::select_biased! {
                        recv(control_rx) -> control => {
                            match control {
                                Ok(ConnectionOutput::Event(event)) => {
                                    if !Self::handle_session_event_output(
                                        &writer,
                                        &session,
                                        event,
                                        &stats,
                                    ) {
                                        break;
                                    }
                                },
                                Ok(ConnectionOutput::CloseAfterEvent(event)) => {
                                    let mut discarded = 0u64;
                                    while event_rx.try_recv().is_ok() {
                                        discarded += 1;
                                    }
                                    if discarded > 0 {
                                        stats
                                            .session_replacement_discard_total
                                            .fetch_add(discarded, Ordering::Relaxed);
                                    }
                                    let _ = Self::write_server_message(&writer, &ServerMessage::Event(event), &stats);
                                    break;
                                },
                                Ok(ConnectionOutput::Shutdown) | Err(_) => break,
                            }
                        },
                        recv(event_rx) -> output => {
                            match output {
                                Ok(ConnectionOutput::Event(event)) => {
                                    if !Self::handle_session_event_output(
                                        &writer,
                                        &session,
                                        event,
                                        &stats,
                                    ) {
                                        break;
                                    }
                                },
                                Ok(ConnectionOutput::CloseAfterEvent(event)) => {
                                    let _ = Self::write_server_message(&writer, &ServerMessage::Event(event), &stats);
                                    break;
                                },
                                Ok(ConnectionOutput::Shutdown) | Err(_) => break,
                            }
                        }
                    }
                }
                session.mark_closed();
                writer.lock().unwrap().shutdown();
            })
            .map(|_| ())
            .map_err(|err| BridgeHostError::Io(format!("failed to spawn bridge writer thread: {err}")))
    }

    fn write_server_message_direct(
        writer: &mut ServerStream,
        message: &ServerMessage,
        stats: &BridgeHostStats,
    ) -> Result<(), BridgeHostError> {
        let encoded = protocol::encode_server_message(message).map_err(|err| {
            BridgeHostError::Io(format!("failed to encode bridge message: {err}"))
        })?;
        protocol::write_framed(writer, &encoded)
            .map_err(|err| BridgeHostError::Io(format!("failed to write bridge message: {err}")))?;
        stats.ipc_out_total.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    fn handle_outbound_direct(
        writer: &mut ServerStream,
        output: ConnectionOutput,
        session: &BridgeSession,
        stats: &BridgeHostStats,
    ) -> bool {
        match output {
            ConnectionOutput::Event(event) => {
                if !session.is_active() {
                    stats.session_replacement_discard_total.fetch_add(1, Ordering::Relaxed);
                    true
                } else {
                    Self::write_server_message_direct(writer, &ServerMessage::Event(event), stats)
                        .is_ok()
                }
            },
            ConnectionOutput::CloseAfterEvent(event) => {
                let _ =
                    Self::write_server_message_direct(writer, &ServerMessage::Event(event), stats);
                false
            },
            ConnectionOutput::Shutdown => false,
        }
    }

    fn drain_direct_outbound(
        writer: &mut ServerStream,
        session: &BridgeSession,
        control_rx: Option<&Receiver<ConnectionOutput>>,
        event_rx: Option<&Receiver<ConnectionOutput>>,
        stats: &BridgeHostStats,
    ) -> Result<bool, BridgeHostError> {
        let mut drained_any = false;

        if let Some(rx) = control_rx {
            loop {
                match rx.try_recv() {
                    Ok(output) => {
                        drained_any = true;
                        if !Self::handle_outbound_direct(writer, output, session, stats) {
                            return Err(BridgeHostError::Io(
                                "bridge connection closed by control output".to_string(),
                            ));
                        }
                    },
                    Err(crossbeam_channel::TryRecvError::Empty) => break,
                    Err(crossbeam_channel::TryRecvError::Disconnected) => {
                        return Err(BridgeHostError::Io(
                            "bridge control queue disconnected".to_string(),
                        ));
                    },
                }
            }
        }

        if let Some(rx) = event_rx {
            loop {
                if let Some(control_rx) = control_rx {
                    loop {
                        match control_rx.try_recv() {
                            Ok(output) => {
                                drained_any = true;
                                if !Self::handle_outbound_direct(writer, output, session, stats) {
                                    return Err(BridgeHostError::Io(
                                        "bridge connection closed by control output".to_string(),
                                    ));
                                }
                            },
                            Err(crossbeam_channel::TryRecvError::Empty) => break,
                            Err(crossbeam_channel::TryRecvError::Disconnected) => {
                                return Err(BridgeHostError::Io(
                                    "bridge control queue disconnected".to_string(),
                                ));
                            },
                        }
                    }
                }

                match rx.try_recv() {
                    Ok(output) => {
                        drained_any = true;
                        if !Self::handle_outbound_direct(writer, output, session, stats) {
                            return Err(BridgeHostError::Io(
                                "bridge connection closed by outbound event".to_string(),
                            ));
                        }
                    },
                    Err(crossbeam_channel::TryRecvError::Empty) => break,
                    Err(crossbeam_channel::TryRecvError::Disconnected) => {
                        return Err(BridgeHostError::Io(
                            "bridge event queue disconnected".to_string(),
                        ));
                    },
                }
            }
        }

        Ok(drained_any)
    }

    fn write_server_message(
        writer: &Arc<Mutex<ServerStream>>,
        message: &ServerMessage,
        stats: &BridgeHostStats,
    ) -> Result<(), BridgeHostError> {
        let encoded = protocol::encode_server_message(message).map_err(|err| {
            BridgeHostError::Io(format!("failed to encode bridge message: {err}"))
        })?;
        let mut guard = writer.lock().unwrap();
        protocol::write_framed(&mut *guard, &encoded)
            .map_err(|err| BridgeHostError::Io(format!("failed to write bridge message: {err}")))?;
        stats.ipc_out_total.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    fn handle_session_event_output(
        writer: &Arc<Mutex<ServerStream>>,
        session: &BridgeSession,
        event: BridgeEvent,
        stats: &BridgeHostStats,
    ) -> bool {
        if !session.is_active() {
            stats.session_replacement_discard_total.fetch_add(1, Ordering::Relaxed);
            return true;
        }

        Self::write_server_message(writer, &ServerMessage::Event(event), stats).is_ok()
    }

    fn send_error(
        writer: &Arc<Mutex<ServerStream>>,
        stats: &BridgeHostStats,
        request_id: u32,
        code: ErrorCode,
        message: impl Into<String>,
    ) -> Result<(), BridgeHostError> {
        Self::write_server_message(
            writer,
            &ServerMessage::Response(ServerResponse::Error {
                request_id,
                code,
                message: message.into(),
            }),
            stats,
        )
    }

    fn handle_connection(
        mut reader: ServerStream,
        writer: Arc<Mutex<ServerStream>>,
        granted_role: BridgeRole,
        ctx: ConnectionContext<'_>,
    ) {
        let peer_label = reader.peer_label();
        let control = Arc::new(StreamControl {
            writer: Arc::clone(&writer),
        });
        let mut session: Option<Arc<BridgeSession>> = None;

        loop {
            let payload = match protocol::read_framed(&mut reader) {
                Ok(payload) => payload,
                Err(protocol::ProtocolError::Io { .. }) => break,
                Err(err) => {
                    ctx.warn_limiter.warn("protocol-read-error", || {
                        format!("bridge protocol read error from {peer_label}: {err}")
                    });
                    break;
                },
            };

            let request = match protocol::decode_client_request(&payload) {
                Ok(request) => request,
                Err(err) => {
                    ctx.warn_limiter.warn("protocol-decode-error", || {
                        format!("bridge protocol decode error from {peer_label}: {err}")
                    });
                    break;
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
                        let _ = Self::send_error(
                            &writer,
                            ctx.stats,
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
                        control.clone(),
                    );

                    let hello_ack = ServerMessage::Response(ServerResponse::HelloAck {
                        request_id,
                        session_id: prepared.session_id(),
                        role_granted: prepared.role_granted(),
                    });
                    if Self::write_server_message(&writer, &hello_ack, ctx.stats).is_err() {
                        break;
                    }

                    let control_rx = prepared.control_rx.clone();
                    let event_rx = prepared.event_rx.clone();
                    let register = ctx.sessions.commit_prepared(prepared);
                    session = Some(Arc::clone(&register.session));
                    if let Err(err) = Self::spawn_writer_thread(
                        Arc::clone(&writer),
                        control_rx,
                        event_rx,
                        Arc::clone(ctx.stats),
                        Arc::clone(&register.session),
                    ) {
                        error!("failed to start bridge writer thread: {err}");
                        if let Some(removed) =
                            ctx.sessions.unregister_session(register.session.session_id())
                        {
                            removed.shutdown();
                        }
                        break;
                    }
                    if let Some(replaced) = register.replaced {
                        let _ = ctx.backend.release_maintenance_lease(replaced.session_id());
                        replaced.replace_and_close();
                    }
                    if let Err(err) =
                        Self::reconcile_raw_tap(ctx.raw_tap, ctx.sessions, ctx.backend)
                    {
                        warn!("failed to reconcile raw frame tap after hello: {err}");
                    }
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
                        let _ = Self::send_error(
                            &writer,
                            ctx.stats,
                            request_id,
                            ErrorCode::NotConnected,
                            "hello handshake required before requests",
                        );
                        continue;
                    };

                    if !active_session.is_active() {
                        let _ = Self::send_error(
                            &writer,
                            ctx.stats,
                            request_id_of(&other),
                            ErrorCode::NotConnected,
                            "bridge session was replaced or closed",
                        );
                        break;
                    }

                    match other {
                        ClientRequest::GetStatus { request_id } => {
                            let status =
                                Self::build_status(ctx.backend.as_ref(), ctx.sessions, ctx.stats);
                            let _ = Self::write_server_message(
                                &writer,
                                &ServerMessage::Response(ServerResponse::StatusResponse {
                                    request_id,
                                    status,
                                }),
                                ctx.stats,
                            );
                        },
                        ClientRequest::SetFilters {
                            request_id,
                            filters,
                        } => {
                            if !ctx.sessions.set_filters(active_session.session_id(), filters) {
                                let _ = Self::send_error(
                                    &writer,
                                    ctx.stats,
                                    request_id,
                                    ErrorCode::NotConnected,
                                    "bridge session was replaced or closed",
                                );
                                break;
                            }
                            let _ = Self::write_server_message(
                                &writer,
                                &ServerMessage::Response(ServerResponse::Ok { request_id }),
                                ctx.stats,
                            );
                        },
                        ClientRequest::SetRawFrameTap {
                            request_id,
                            enabled,
                        } => {
                            if !ctx.raw_tap.lock().unwrap().allowed() {
                                let _ = Self::send_error(
                                    &writer,
                                    ctx.stats,
                                    request_id,
                                    ErrorCode::PermissionDenied,
                                    "raw frame tap is disabled by bridge host policy",
                                );
                                continue;
                            }
                            if !ctx.sessions.set_raw_frame_tap(active_session.session_id(), enabled)
                            {
                                let _ = Self::send_error(
                                    &writer,
                                    ctx.stats,
                                    request_id,
                                    ErrorCode::NotConnected,
                                    "bridge session was replaced or closed",
                                );
                                break;
                            }
                            match Self::reconcile_raw_tap(ctx.raw_tap, ctx.sessions, ctx.backend) {
                                Ok(()) => {
                                    let _ = Self::write_server_message(
                                        &writer,
                                        &ServerMessage::Response(ServerResponse::Ok { request_id }),
                                        ctx.stats,
                                    );
                                },
                                Err(err) => {
                                    let _ = Self::send_error(
                                        &writer,
                                        ctx.stats,
                                        request_id,
                                        ErrorCode::DeviceError,
                                        err.to_string(),
                                    );
                                },
                            }
                        },
                        ClientRequest::AcquireWriterLease {
                            request_id,
                            timeout_ms,
                        } => {
                            if active_session.role_granted() != BridgeRole::WriterCandidate {
                                let _ = Self::send_error(
                                    &writer,
                                    ctx.stats,
                                    request_id,
                                    ErrorCode::PermissionDenied,
                                    "writer lease requires a WriterCandidate bridge role",
                                );
                                continue;
                            }
                            match ctx.backend.acquire_maintenance_lease(
                                active_session.session_id(),
                                Duration::from_millis(timeout_ms as u64),
                            ) {
                                Ok(LeaseAcquireResult::Granted) => {
                                    let _ = Self::write_server_message(
                                        &writer,
                                        &ServerMessage::Response(ServerResponse::LeaseGranted {
                                            request_id,
                                            session_id: active_session.session_id(),
                                        }),
                                        ctx.stats,
                                    );
                                },
                                Ok(LeaseAcquireResult::Denied { holder_session_id }) => {
                                    let _ = Self::write_server_message(
                                        &writer,
                                        &ServerMessage::Response(ServerResponse::LeaseDenied {
                                            request_id,
                                            holder_session_id,
                                        }),
                                        ctx.stats,
                                    );
                                },
                                Err(err) => {
                                    let _ = Self::send_error(
                                        &writer,
                                        ctx.stats,
                                        request_id,
                                        err.code(),
                                        err.message(),
                                    );
                                },
                            }
                        },
                        ClientRequest::ReleaseWriterLease { request_id } => {
                            let _ =
                                ctx.backend.release_maintenance_lease(active_session.session_id());
                            let _ = Self::write_server_message(
                                &writer,
                                &ServerMessage::Response(ServerResponse::Ok { request_id }),
                                ctx.stats,
                            );
                        },
                        ClientRequest::SendFrame { request_id, frame } => {
                            match ctx
                                .backend
                                .send_maintenance_frame(active_session.session_id(), frame)
                            {
                                Ok(()) => {
                                    ctx.stats.maintenance_tx_total.fetch_add(1, Ordering::Relaxed);
                                    let _ = Self::write_server_message(
                                        &writer,
                                        &ServerMessage::Response(ServerResponse::Ok { request_id }),
                                        ctx.stats,
                                    );
                                },
                                Err(err) => {
                                    let _ = Self::send_error(
                                        &writer,
                                        ctx.stats,
                                        request_id,
                                        err.code(),
                                        err.message(),
                                    );
                                },
                            }
                        },
                        ClientRequest::Ping { request_id } => {
                            let _ = Self::write_server_message(
                                &writer,
                                &ServerMessage::Response(ServerResponse::Ok { request_id }),
                                ctx.stats,
                            );
                        },
                        ClientRequest::Hello { .. } => unreachable!("hello handled above"),
                    }
                },
            }
        }

        if let Some(active_session) = session
            && let Some(removed) = ctx.sessions.unregister_session(active_session.session_id())
        {
            let _ = ctx.backend.release_maintenance_lease(active_session.session_id());
            removed.shutdown();
            if let Err(err) = Self::reconcile_raw_tap(ctx.raw_tap, ctx.sessions, ctx.backend) {
                warn!("failed to reconcile raw frame tap after disconnect: {err}");
            }
        }
    }

    fn send_error_direct(
        writer: &mut ServerStream,
        stats: &BridgeHostStats,
        request_id: u32,
        code: ErrorCode,
        message: impl Into<String>,
    ) -> Result<(), BridgeHostError> {
        Self::write_server_message_direct(
            writer,
            &ServerMessage::Response(ServerResponse::Error {
                request_id,
                code,
                message: message.into(),
            }),
            stats,
        )
    }

    fn handle_tls_connection(
        mut stream: ServerStream,
        control: Arc<dyn SessionControl>,
        granted_role: BridgeRole,
        ctx: ConnectionContext<'_>,
    ) {
        let peer_label = stream.peer_label();
        let mut session: Option<Arc<BridgeSession>> = None;
        let mut control_rx: Option<Receiver<ConnectionOutput>> = None;
        let mut event_rx: Option<Receiver<ConnectionOutput>> = None;

        'connection: loop {
            if let Some(active_session) = session.as_ref() {
                match Self::drain_direct_outbound(
                    &mut stream,
                    active_session,
                    control_rx.as_ref(),
                    event_rx.as_ref(),
                    ctx.stats,
                ) {
                    Ok(true) => continue,
                    Ok(false) => {},
                    Err(_) => break 'connection,
                }
            }

            if let Err(err) = stream.set_read_timeout(Some(TLS_IO_POLL_INTERVAL)) {
                warn!("bridge tls read-timeout update failed for {peer_label}: {err}");
                break;
            }

            let payload = match protocol::read_framed(&mut stream) {
                Ok(payload) => payload,
                Err(protocol::ProtocolError::Io { kind, .. })
                    if kind == std::io::ErrorKind::TimedOut
                        || kind == std::io::ErrorKind::WouldBlock =>
                {
                    continue;
                },
                Err(protocol::ProtocolError::Io { .. }) => break,
                Err(err) => {
                    ctx.warn_limiter.warn("protocol-read-error", || {
                        format!("bridge tls protocol read error from {peer_label}: {err}")
                    });
                    break;
                },
            };

            let request = match protocol::decode_client_request(&payload) {
                Ok(request) => request,
                Err(err) => {
                    ctx.warn_limiter.warn("protocol-decode-error", || {
                        format!("bridge tls protocol decode error from {peer_label}: {err}")
                    });
                    break;
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
                        let _ = Self::send_error_direct(
                            &mut stream,
                            ctx.stats,
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
                        Arc::clone(&control),
                    );
                    let prepared_control_rx = prepared.control_rx.clone();
                    let prepared_event_rx = prepared.event_rx.clone();

                    let hello_ack = ServerMessage::Response(ServerResponse::HelloAck {
                        request_id,
                        session_id: prepared.session_id(),
                        role_granted: prepared.role_granted(),
                    });
                    if Self::write_server_message_direct(&mut stream, &hello_ack, ctx.stats)
                        .is_err()
                    {
                        break;
                    }

                    let register = ctx.sessions.commit_prepared(prepared);
                    control_rx = Some(prepared_control_rx);
                    event_rx = Some(prepared_event_rx);
                    session = Some(Arc::clone(&register.session));
                    if let Some(replaced) = register.replaced {
                        let _ = ctx.backend.release_maintenance_lease(replaced.session_id());
                        replaced.replace_and_close();
                    }
                    if let Err(err) =
                        Self::reconcile_raw_tap(ctx.raw_tap, ctx.sessions, ctx.backend)
                    {
                        warn!("failed to reconcile raw frame tap after tls hello: {err}");
                    }
                },
                other => {
                    let Some(active_session) = session.as_ref().cloned() else {
                        let request_id = request_id_of(&other);
                        ctx.warn_limiter.warn("request-before-hello", || {
                            format!(
                                "rejecting {} from unauthenticated bridge tls connection {}",
                                message_kind(&other),
                                peer_label
                            )
                        });
                        let _ = Self::send_error_direct(
                            &mut stream,
                            ctx.stats,
                            request_id,
                            ErrorCode::NotConnected,
                            "hello handshake required before requests",
                        );
                        continue;
                    };

                    if !active_session.is_active() {
                        let _ = Self::send_error_direct(
                            &mut stream,
                            ctx.stats,
                            request_id_of(&other),
                            ErrorCode::NotConnected,
                            "bridge session was replaced or closed",
                        );
                        break;
                    }

                    match other {
                        ClientRequest::GetStatus { request_id } => {
                            let status =
                                Self::build_status(ctx.backend.as_ref(), ctx.sessions, ctx.stats);
                            let _ = Self::write_server_message_direct(
                                &mut stream,
                                &ServerMessage::Response(ServerResponse::StatusResponse {
                                    request_id,
                                    status,
                                }),
                                ctx.stats,
                            );
                        },
                        ClientRequest::SetFilters {
                            request_id,
                            filters,
                        } => {
                            if !ctx.sessions.set_filters(active_session.session_id(), filters) {
                                let _ = Self::send_error_direct(
                                    &mut stream,
                                    ctx.stats,
                                    request_id,
                                    ErrorCode::NotConnected,
                                    "bridge session was replaced or closed",
                                );
                                break;
                            }
                            let _ = Self::write_server_message_direct(
                                &mut stream,
                                &ServerMessage::Response(ServerResponse::Ok { request_id }),
                                ctx.stats,
                            );
                        },
                        ClientRequest::SetRawFrameTap {
                            request_id,
                            enabled,
                        } => {
                            if !ctx.raw_tap.lock().unwrap().allowed() {
                                let _ = Self::send_error_direct(
                                    &mut stream,
                                    ctx.stats,
                                    request_id,
                                    ErrorCode::PermissionDenied,
                                    "raw frame tap is disabled by bridge host policy",
                                );
                                continue;
                            }
                            if !ctx.sessions.set_raw_frame_tap(active_session.session_id(), enabled)
                            {
                                let _ = Self::send_error_direct(
                                    &mut stream,
                                    ctx.stats,
                                    request_id,
                                    ErrorCode::NotConnected,
                                    "bridge session was replaced or closed",
                                );
                                break;
                            }
                            match Self::reconcile_raw_tap(ctx.raw_tap, ctx.sessions, ctx.backend) {
                                Ok(()) => {
                                    let _ = Self::write_server_message_direct(
                                        &mut stream,
                                        &ServerMessage::Response(ServerResponse::Ok { request_id }),
                                        ctx.stats,
                                    );
                                },
                                Err(err) => {
                                    let _ = Self::send_error_direct(
                                        &mut stream,
                                        ctx.stats,
                                        request_id,
                                        ErrorCode::DeviceError,
                                        err.to_string(),
                                    );
                                },
                            }
                        },
                        ClientRequest::AcquireWriterLease {
                            request_id,
                            timeout_ms,
                        } => {
                            if active_session.role_granted() != BridgeRole::WriterCandidate {
                                let _ = Self::send_error_direct(
                                    &mut stream,
                                    ctx.stats,
                                    request_id,
                                    ErrorCode::PermissionDenied,
                                    "writer lease requires a WriterCandidate bridge role",
                                );
                                continue;
                            }
                            match ctx.backend.acquire_maintenance_lease(
                                active_session.session_id(),
                                Duration::from_millis(timeout_ms as u64),
                            ) {
                                Ok(LeaseAcquireResult::Granted) => {
                                    let _ = Self::write_server_message_direct(
                                        &mut stream,
                                        &ServerMessage::Response(ServerResponse::LeaseGranted {
                                            request_id,
                                            session_id: active_session.session_id(),
                                        }),
                                        ctx.stats,
                                    );
                                },
                                Ok(LeaseAcquireResult::Denied { holder_session_id }) => {
                                    let _ = Self::write_server_message_direct(
                                        &mut stream,
                                        &ServerMessage::Response(ServerResponse::LeaseDenied {
                                            request_id,
                                            holder_session_id,
                                        }),
                                        ctx.stats,
                                    );
                                },
                                Err(err) => {
                                    let _ = Self::send_error_direct(
                                        &mut stream,
                                        ctx.stats,
                                        request_id,
                                        err.code(),
                                        err.message(),
                                    );
                                },
                            }
                        },
                        ClientRequest::ReleaseWriterLease { request_id } => {
                            let _ =
                                ctx.backend.release_maintenance_lease(active_session.session_id());
                            let _ = Self::write_server_message_direct(
                                &mut stream,
                                &ServerMessage::Response(ServerResponse::Ok { request_id }),
                                ctx.stats,
                            );
                        },
                        ClientRequest::SendFrame { request_id, frame } => {
                            match ctx
                                .backend
                                .send_maintenance_frame(active_session.session_id(), frame)
                            {
                                Ok(()) => {
                                    ctx.stats.maintenance_tx_total.fetch_add(1, Ordering::Relaxed);
                                    let _ = Self::write_server_message_direct(
                                        &mut stream,
                                        &ServerMessage::Response(ServerResponse::Ok { request_id }),
                                        ctx.stats,
                                    );
                                },
                                Err(err) => {
                                    let _ = Self::send_error_direct(
                                        &mut stream,
                                        ctx.stats,
                                        request_id,
                                        err.code(),
                                        err.message(),
                                    );
                                },
                            }
                        },
                        ClientRequest::Ping { request_id } => {
                            let _ = Self::write_server_message_direct(
                                &mut stream,
                                &ServerMessage::Response(ServerResponse::Ok { request_id }),
                                ctx.stats,
                            );
                        },
                        ClientRequest::Hello { .. } => unreachable!("hello handled above"),
                    }
                },
            }
        }

        if let Some(active_session) = session
            && let Some(removed) = ctx.sessions.unregister_session(active_session.session_id())
        {
            let _ = ctx.backend.release_maintenance_lease(active_session.session_id());
            removed.shutdown();
            if let Err(err) = Self::reconcile_raw_tap(ctx.raw_tap, ctx.sessions, ctx.backend) {
                warn!("failed to reconcile raw frame tap after tls disconnect: {err}");
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
        DriverError::Timeout | DriverError::RealtimeDeliveryTimeout => ErrorCode::Timeout,
        DriverError::ChannelFull | DriverError::ShutdownConflict => ErrorCode::Busy,
        DriverError::ControlPathClosed
        | DriverError::CommandAbortedByFault
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
        | DriverError::RealtimeDeliveryFailed { .. }
        | DriverError::RealtimeDeliveryOverwritten => ErrorCode::DeviceError,
    };
    BridgeBackendError::new(code, error.to_string())
}

struct PiperBridgeBackend {
    driver: Arc<RobotPiper>,
}

impl PiperBridgeBackend {
    fn from_driver(driver: Arc<RobotPiper>) -> Arc<dyn BridgeControllerBackend> {
        Arc::new(Self { driver })
    }

    fn maintenance_state(&self) -> BridgeMaintenanceState {
        if !self.driver.maintenance_runtime_open() {
            return BridgeMaintenanceState::DeniedTransportDown;
        }
        let health = self.driver.health();
        if health.fault.is_some() {
            return BridgeMaintenanceState::DeniedFaulted;
        }
        if !health.rx_alive || !health.tx_alive || !health.connected {
            return BridgeMaintenanceState::DeniedTransportDown;
        }

        let control = self.driver.get_robot_control();
        if control.is_enabled {
            return BridgeMaintenanceState::DeniedActiveControl;
        }

        BridgeMaintenanceState::AllowedStandby
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

    fn acquire_maintenance_lease(
        &self,
        session_id: u32,
        timeout: Duration,
    ) -> Result<LeaseAcquireResult, BridgeBackendError> {
        let deadline = Instant::now() + timeout;
        loop {
            let current_state = self.maintenance_state();
            if !current_state.allows_lease() {
                return Err(BridgeBackendError::new(
                    ErrorCode::PermissionDenied,
                    current_state.denial_message(),
                ));
            }

            let (granted, _epoch, holder_session_id) =
                self.driver.acquire_maintenance_lease_gate(session_id);
            if granted {
                self.driver.note_maintenance_state_hint();
                return Ok(LeaseAcquireResult::Granted);
            }

            let now = Instant::now();
            if now >= deadline {
                return Ok(LeaseAcquireResult::Denied { holder_session_id });
            }

            let observed_epoch = self.driver.maintenance_state_epoch();
            self.driver.wait_for_maintenance_state_change_after(
                observed_epoch,
                Some(deadline.saturating_duration_since(now)),
            );
        }
    }

    fn release_maintenance_lease(&self, session_id: u32) -> Result<bool, BridgeBackendError> {
        Ok(self.driver.release_maintenance_lease_gate_if_holder(session_id))
    }

    fn wait_for_lease_revocation(&self) -> Result<u32, BridgeBackendError> {
        loop {
            let (holder_session_id, _lease_epoch) = self.driver.current_maintenance_lease();
            let Some(_holder_session_id) = holder_session_id else {
                let observed_epoch = self.driver.maintenance_state_epoch();
                self.driver.wait_for_maintenance_state_change_after(observed_epoch, None);
                continue;
            };

            let state = self.maintenance_state();
            if !state.allows_lease() {
                if let Some(revoked_holder) = self.driver.revoke_maintenance_lease_gate() {
                    return Ok(revoked_holder);
                }
                continue;
            }

            let observed_epoch = self.driver.maintenance_state_epoch();
            self.driver.wait_for_maintenance_state_change_after(
                observed_epoch,
                self.driver.connection_timeout_remaining(),
            );
        }
    }

    fn send_maintenance_frame(
        &self,
        session_id: u32,
        frame: PiperFrame,
    ) -> Result<(), BridgeBackendError> {
        let state = self.maintenance_state();
        if !state.allows_lease() {
            return Err(BridgeBackendError::new(
                ErrorCode::PermissionDenied,
                state.denial_message(),
            ));
        }

        let (holder_session_id, lease_epoch) = self.driver.current_maintenance_lease();
        if holder_session_id != Some(session_id) {
            return Err(BridgeBackendError::new(
                ErrorCode::PermissionDenied,
                "maintenance lease required",
            ));
        }

        self.driver
            .send_maintenance_frame_confirmed(
                session_id,
                lease_epoch,
                frame,
                Duration::from_millis(10),
            )
            .map_err(|err| {
                if matches!(
                    err,
                    DriverError::ControlPathClosed | DriverError::MaintenanceWriteDenied(_)
                ) {
                    let _ = self.driver.release_maintenance_lease_gate_if_holder(session_id);
                }
                bridge_backend_error_from_driver(&err)
            })
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
    use std::io::Read;
    use std::sync::Arc;
    use std::time::Duration;

    #[cfg(unix)]
    use std::os::unix::net::UnixStream;

    struct NoopControl;

    impl SessionControl for NoopControl {
        fn shutdown(&self) {}
    }

    fn token(byte: u8) -> SessionToken {
        SessionToken::new([byte; 16])
    }

    #[test]
    fn same_token_replacement_invalidates_old_session_without_counting_it_as_active() {
        let manager = SessionManager::new();
        let first = manager.commit_prepared(manager.prepare_session(
            token(1),
            BridgeRole::WriterCandidate,
            vec![],
            Arc::new(NoopControl),
        ));

        let second = manager.commit_prepared(manager.prepare_session(
            token(1),
            BridgeRole::WriterCandidate,
            vec![],
            Arc::new(NoopControl),
        ));

        assert!(first.replaced.is_none());
        assert!(second.replaced.is_some());
        let old = second.replaced.as_ref().unwrap().clone();
        old.replace_and_close();
        assert!(!old.is_active());
        assert!(second.session.is_active());
        assert!(!manager.set_filters(old.session_id(), vec![CanIdFilter::new(0x100, 0x1FF)]));
        assert!(manager.set_filters(
            second.session.session_id(),
            vec![CanIdFilter::new(0x100, 0x1FF)]
        ));
    }

    #[test]
    fn replace_and_close_uses_priority_control_queue_instead_of_waiting_for_event_backlog() {
        let (event_tx, _event_rx) = SessionManager::new_connection_queue();
        let (control_tx, control_rx) = SessionManager::new_connection_queue();
        let session = BridgeSession::new(
            1,
            token(2),
            BridgeRole::WriterCandidate,
            vec![],
            event_tx,
            control_tx,
            Arc::new(NoopControl),
        );
        session.set_raw_frame_tap_enabled(true);

        for _ in 0..OUTBOUND_QUEUE_CAPACITY {
            assert_eq!(
                session.enqueue_frame(PiperFrame::new_standard(0x123, &[1])),
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
        let manager = SessionManager::new();
        let register = manager.commit_prepared(manager.prepare_session(
            token(3),
            BridgeRole::Observer,
            vec![],
            Arc::new(NoopControl),
        ));
        register.session.set_raw_frame_tap_enabled(true);
        register.session.mark_closing();

        assert_eq!(
            manager.broadcast_frame(PiperFrame::new_standard(0x120, &[1, 2, 3])),
            BroadcastFrameStats {
                dropped: 0,
                inactive: 1,
            }
        );
    }

    #[cfg(unix)]
    #[test]
    fn replaced_session_drops_direct_outbound_events_without_writing() {
        let (server, mut client) = UnixStream::pair().expect("unix pair");
        client
            .set_read_timeout(Some(Duration::from_millis(20)))
            .expect("set read timeout");

        let (event_tx, _event_rx) = SessionManager::new_connection_queue();
        let (control_tx, _control_rx) = SessionManager::new_connection_queue();
        let session = BridgeSession::new(
            7,
            token(7),
            BridgeRole::WriterCandidate,
            vec![],
            event_tx,
            control_tx,
            Arc::new(NoopControl),
        );
        assert!(session.mark_replaced());

        let mut writer = ServerStream::Unix(server);
        let stats = BridgeHostStats::new();
        let frame = PiperFrame::new_standard(0x123, &[1, 2, 3, 4]);
        assert!(PiperBridgeHost::handle_outbound_direct(
            &mut writer,
            ConnectionOutput::Event(BridgeEvent::ReceiveFrame(frame)),
            &session,
            &stats,
        ));
        assert_eq!(
            stats.session_replacement_discard_total.load(Ordering::Relaxed),
            1
        );

        let mut buf = [0u8; 1];
        let err = client.read(&mut buf).expect_err("replaced session should not write");
        assert!(matches!(
            err.kind(),
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
        ));
    }

    #[cfg(unix)]
    #[test]
    fn drain_direct_outbound_flushes_all_pending_events_before_returning_to_read_poll() {
        let (server, mut client) = UnixStream::pair().expect("unix pair");
        client
            .set_read_timeout(Some(Duration::from_millis(50)))
            .expect("set read timeout");

        let (event_tx, event_rx) = SessionManager::new_connection_queue();
        let (control_tx, control_rx) = SessionManager::new_connection_queue();
        let session = BridgeSession::new(
            9,
            token(9),
            BridgeRole::Observer,
            vec![],
            event_tx,
            control_tx,
            Arc::new(NoopControl),
        );

        let frame_a = PiperFrame::new_standard(0x120, &[1, 2, 3]);
        let frame_b = PiperFrame::new_standard(0x121, &[4, 5, 6]);
        let frame_c = PiperFrame::new_standard(0x122, &[7, 8, 9]);
        session
            .event_tx
            .try_send(ConnectionOutput::Event(BridgeEvent::ReceiveFrame(frame_a)))
            .expect("queue frame a");
        session
            .event_tx
            .try_send(ConnectionOutput::Event(BridgeEvent::ReceiveFrame(frame_b)))
            .expect("queue frame b");
        session
            .event_tx
            .try_send(ConnectionOutput::Event(BridgeEvent::ReceiveFrame(frame_c)))
            .expect("queue frame c");

        let mut writer = ServerStream::Unix(server);
        let stats = BridgeHostStats::new();
        assert!(
            PiperBridgeHost::drain_direct_outbound(
                &mut writer,
                &session,
                Some(&control_rx),
                Some(&event_rx),
                &stats,
            )
            .expect("outbound drain should succeed")
        );

        let payload_a = protocol::read_framed(&mut client).expect("read frame a");
        let payload_b = protocol::read_framed(&mut client).expect("read frame b");
        let payload_c = protocol::read_framed(&mut client).expect("read frame c");
        assert_eq!(
            protocol::decode_server_message(&payload_a).expect("decode frame a"),
            ServerMessage::Event(BridgeEvent::ReceiveFrame(frame_a))
        );
        assert_eq!(
            protocol::decode_server_message(&payload_b).expect("decode frame b"),
            ServerMessage::Event(BridgeEvent::ReceiveFrame(frame_b))
        );
        assert_eq!(
            protocol::decode_server_message(&payload_c).expect("decode frame c"),
            ServerMessage::Event(BridgeEvent::ReceiveFrame(frame_c))
        );
    }
}
