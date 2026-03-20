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
use piper_driver::{DriverError, HealthStatus, Piper as RobotPiper};
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
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::thread;
use std::time::{Duration, Instant};
use tracing::{error, warn};

const OUTBOUND_QUEUE_CAPACITY: usize = 256;
const RAW_FRAME_TAP_QUEUE_CAPACITY: usize = 1024;
const AUTH_LOG_WINDOW: Duration = Duration::from_secs(1);
const LEASE_MONITOR_INTERVAL: Duration = Duration::from_millis(20);
const TLS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const TLS_IO_POLL_INTERVAL: Duration = Duration::from_millis(5);

#[derive(Debug, Clone)]
pub struct BridgeTlsServerConfig {
    pub listen_addr: SocketAddr,
    pub server_cert_pem: PathBuf,
    pub server_key_pem: PathBuf,
    pub client_ca_cert_pem: PathBuf,
    pub allowed_client_cert_sha256: Vec<String>,
    pub handshake_timeout: Duration,
}

impl BridgeTlsServerConfig {
    pub fn with_addr(listen_addr: SocketAddr) -> Self {
        Self {
            listen_addr,
            server_cert_pem: PathBuf::new(),
            server_key_pem: PathBuf::new(),
            client_ca_cert_pem: PathBuf::new(),
            allowed_client_cert_sha256: Vec::new(),
            handshake_timeout: TLS_HANDSHAKE_TIMEOUT,
        }
    }
}

#[derive(Debug, Clone)]
pub struct BridgeHostConfig {
    pub uds_path: Option<PathBuf>,
    pub tcp_tls: Option<BridgeTlsServerConfig>,
    pub maintenance_mode: bool,
    pub enable_raw_frame_tap: bool,
}

impl Default for BridgeHostConfig {
    fn default() -> Self {
        Self {
            #[cfg(unix)]
            uds_path: Some(PathBuf::from("/tmp/piper_bridge.sock")),
            #[cfg(not(unix))]
            uds_path: None,
            tcp_tls: None,
            maintenance_mode: false,
            enable_raw_frame_tap: true,
        }
    }
}

#[derive(Debug)]
pub enum BridgeHostError {
    Listener(String),
    Io(String),
    InvalidConfig(String),
    Driver(String),
    AlreadyRunning,
}

impl std::fmt::Display for BridgeHostError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Listener(message) => write!(f, "{message}"),
            Self::Io(message) => write!(f, "{message}"),
            Self::InvalidConfig(message) => write!(f, "{message}"),
            Self::Driver(message) => write!(f, "{message}"),
            Self::AlreadyRunning => write!(f, "bridge host is already running"),
        }
    }
}

impl std::error::Error for BridgeHostError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LeaseAcquireResult {
    Granted,
    Denied { holder_session_id: Option<u32> },
    NotConnected,
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

    fn health_score(&self, health: HealthStatus) -> u8 {
        let mut score = if health.fault.is_some() {
            40i32
        } else if health.connected {
            100
        } else if health.rx_alive || health.tx_alive {
            70
        } else {
            40
        };
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
    allowed_fingerprints: Arc<Vec<[u8; 32]>>,
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

struct BridgeSession {
    session_id: u32,
    session_token: SessionToken,
    filters: RwLock<Vec<CanIdFilter>>,
    event_tx: Sender<ConnectionOutput>,
    control_tx: Sender<ConnectionOutput>,
    pending_gap: AtomicU32,
    lifecycle: AtomicU8,
    control: Arc<dyn SessionControl>,
}

impl BridgeSession {
    fn new(
        session_id: u32,
        session_token: SessionToken,
        filters: Vec<CanIdFilter>,
        event_tx: Sender<ConnectionOutput>,
        control_tx: Sender<ConnectionOutput>,
        control: Arc<dyn SessionControl>,
    ) -> Self {
        Self {
            session_id,
            session_token,
            filters: RwLock::new(filters),
            event_tx,
            control_tx,
            pending_gap: AtomicU32::new(0),
            lifecycle: AtomicU8::new(SessionLifecycle::Active as u8),
            control,
        }
    }

    fn session_id(&self) -> u32 {
        self.session_id
    }

    fn session_token(&self) -> SessionToken {
        self.session_token
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

    fn matches_filter(&self, can_id: u32) -> bool {
        let filters = self.filters.read().unwrap();
        if filters.is_empty() {
            return true;
        }
        filters.iter().any(|filter| filter.matches(can_id))
    }

    fn enqueue_frame(&self, frame: PiperFrame) -> bool {
        if !self.is_active() {
            return false;
        }

        let dropped = self.pending_gap.swap(0, Ordering::AcqRel);
        if dropped > 0
            && self
                .event_tx
                .try_send(ConnectionOutput::Event(BridgeEvent::Gap { dropped }))
                .is_err()
        {
            self.pending_gap.fetch_add(dropped + 1, Ordering::AcqRel);
            return false;
        }

        match self
            .event_tx
            .try_send(ConnectionOutput::Event(BridgeEvent::ReceiveFrame(frame)))
        {
            Ok(()) => true,
            Err(_) => {
                self.pending_gap.fetch_add(1, Ordering::AcqRel);
                false
            },
        }
    }

    fn send_priority_event(&self, event: BridgeEvent) {
        if self.control_tx.try_send(ConnectionOutput::Event(event)).is_err() {
            self.control.shutdown();
        }
    }

    fn replace_and_close(&self) {
        if !self.mark_replaced() {
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
    maintenance_lease: Mutex<Option<u32>>,
    maintenance_cv: Condvar,
}

impl SessionManager {
    fn new() -> Self {
        Self {
            registry: RwLock::new(Registry {
                sessions: HashMap::new(),
                token_to_session: HashMap::new(),
            }),
            next_session_id: AtomicU32::new(1),
            maintenance_lease: Mutex::new(None),
            maintenance_cv: Condvar::new(),
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
        role_request: BridgeRole,
        filters: Vec<CanIdFilter>,
        control: Arc<dyn SessionControl>,
    ) -> PreparedSession {
        let (event_tx, event_rx) = Self::new_connection_queue();
        let (control_tx, control_rx) = Self::new_connection_queue();
        PreparedSession {
            session_id: self.next_session_id(),
            session_token,
            role_granted: role_request,
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
            prepared.filters,
            prepared.event_tx,
            prepared.control_tx,
            prepared.control,
        ));
        registry.token_to_session.insert(prepared.session_token, prepared.session_id);
        registry.sessions.insert(prepared.session_id, Arc::clone(&session));
        drop(registry);

        if let Some(ref old_session) = replaced {
            let mut lease = self.maintenance_lease.lock().unwrap();
            if lease.as_ref().copied() == Some(old_session.session_id()) {
                *lease = Some(prepared.session_id);
                self.maintenance_cv.notify_all();
            }
        }

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

        let mut lease = self.maintenance_lease.lock().unwrap();
        if lease.as_ref().copied() == Some(session_id) {
            *lease = None;
            self.maintenance_cv.notify_all();
        }

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

    fn broadcast_frame(&self, frame: PiperFrame) -> u64 {
        let registry = self.registry.read().unwrap();
        let mut dropped = 0u64;
        for session in registry.sessions.values() {
            if session.matches_filter(frame.id) && !session.enqueue_frame(frame) {
                dropped += 1;
            }
        }
        dropped
    }

    fn set_filters(&self, session_id: u32, filters: Vec<CanIdFilter>) -> bool {
        if let Some(session) = self.active_session(session_id) {
            session.set_filters(filters);
            true
        } else {
            false
        }
    }

    fn has_maintenance_lease(&self, session_id: u32) -> bool {
        self.active_session(session_id).is_some()
            && self.maintenance_lease.lock().unwrap().as_ref().copied() == Some(session_id)
    }

    fn acquire_maintenance_lease(&self, session_id: u32, timeout: Duration) -> LeaseAcquireResult {
        if self.active_session(session_id).is_none() {
            return LeaseAcquireResult::NotConnected;
        }

        let deadline = Instant::now() + timeout;
        let mut guard = self.maintenance_lease.lock().unwrap();
        loop {
            match *guard {
                None => {
                    *guard = Some(session_id);
                    return LeaseAcquireResult::Granted;
                },
                Some(owner) if owner == session_id => return LeaseAcquireResult::Granted,
                Some(owner) => {
                    let now = Instant::now();
                    if now >= deadline {
                        return LeaseAcquireResult::Denied {
                            holder_session_id: Some(owner),
                        };
                    }
                    let remaining = deadline.saturating_duration_since(now);
                    let (next_guard, wait_result) =
                        self.maintenance_cv.wait_timeout(guard, remaining).unwrap();
                    guard = next_guard;
                    if wait_result.timed_out() {
                        return LeaseAcquireResult::Denied {
                            holder_session_id: guard.as_ref().copied(),
                        };
                    }
                },
            }
        }
    }

    fn release_maintenance_lease(&self, session_id: u32) -> bool {
        let mut guard = self.maintenance_lease.lock().unwrap();
        if guard.as_ref().copied() == Some(session_id) {
            *guard = None;
            self.maintenance_cv.notify_all();
            return true;
        }
        false
    }

    fn revoke_maintenance_lease(&self) -> Option<Arc<BridgeSession>> {
        let holder = {
            let mut guard = self.maintenance_lease.lock().unwrap();
            let holder = *guard;
            if holder.is_some() {
                *guard = None;
                self.maintenance_cv.notify_all();
            }
            holder
        };
        holder.and_then(|session_id| self.session(session_id))
    }
}

struct ConnectionContext<'a> {
    driver: &'a Arc<RobotPiper>,
    sessions: &'a Arc<SessionManager>,
    maintenance_mode: &'a Arc<AtomicBool>,
    stats: &'a Arc<BridgeHostStats>,
    warn_limiter: &'a Arc<WarnRateLimiter>,
}

pub struct PiperBridgeHost {
    driver: Arc<RobotPiper>,
    config: BridgeHostConfig,
    sessions: Arc<SessionManager>,
    maintenance_mode: Arc<AtomicBool>,
    stats: Arc<BridgeHostStats>,
    warn_limiter: Arc<WarnRateLimiter>,
    started: AtomicBool,
}

impl PiperBridgeHost {
    pub fn from_driver(driver: Arc<RobotPiper>, config: BridgeHostConfig) -> Self {
        Self {
            driver,
            maintenance_mode: Arc::new(AtomicBool::new(config.maintenance_mode)),
            config,
            sessions: Arc::new(SessionManager::new()),
            stats: Arc::new(BridgeHostStats::new()),
            warn_limiter: Arc::new(WarnRateLimiter::new(AUTH_LOG_WINDOW)),
            started: AtomicBool::new(false),
        }
    }

    pub fn from_piper<State>(piper: &crate::state::Piper<State>, config: BridgeHostConfig) -> Self {
        Self::from_driver(Arc::clone(&piper.driver), config)
    }

    pub fn set_maintenance_mode(&self, enabled: bool) {
        self.maintenance_mode.store(enabled, Ordering::Release);
    }

    pub fn maintenance_mode(&self) -> bool {
        self.maintenance_mode.load(Ordering::Acquire)
    }

    pub fn run(self) -> Result<(), BridgeHostError> {
        if self.started.swap(true, Ordering::AcqRel) {
            return Err(BridgeHostError::AlreadyRunning);
        }
        if self.config.uds_path.is_none() && self.config.tcp_tls.is_none() {
            return Err(BridgeHostError::InvalidConfig(
                "at least one bridge endpoint must be enabled".to_string(),
            ));
        }

        let raw_frame_rx = if self.config.enable_raw_frame_tap {
            Some(self.install_raw_frame_tap()?)
        } else {
            None
        };

        if let Some(rx) = raw_frame_rx {
            let sessions = Arc::clone(&self.sessions);
            let stats = Arc::clone(&self.stats);
            thread::Builder::new()
                .name("bridge_frame_fanout".into())
                .spawn(move || Self::frame_fanout_loop(rx, sessions, stats))
                .map_err(|err| {
                    BridgeHostError::Io(format!("failed to spawn frame fanout loop: {err}"))
                })?;
        }

        {
            let driver = Arc::clone(&self.driver);
            let sessions = Arc::clone(&self.sessions);
            let maintenance_mode = Arc::clone(&self.maintenance_mode);
            thread::Builder::new()
                .name("bridge_lease_monitor".into())
                .spawn(move || Self::lease_monitor_loop(driver, sessions, maintenance_mode))
                .map_err(|err| {
                    BridgeHostError::Io(format!("failed to spawn lease monitor loop: {err}"))
                })?;
        }

        let mut handles = Vec::new();

        #[cfg(unix)]
        if let Some(path) = &self.config.uds_path {
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
            let driver = Arc::clone(&self.driver);
            let sessions = Arc::clone(&self.sessions);
            let maintenance_mode = Arc::clone(&self.maintenance_mode);
            let stats = Arc::clone(&self.stats);
            let warn_limiter = Arc::clone(&self.warn_limiter);
            handles.push(
                thread::Builder::new()
                    .name("bridge_accept_uds".into())
                    .spawn(move || {
                        for incoming in listener.incoming() {
                            match incoming {
                                Ok(stream) => {
                                    let driver = Arc::clone(&driver);
                                    let sessions = Arc::clone(&sessions);
                                    let maintenance_mode = Arc::clone(&maintenance_mode);
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
                                            ConnectionContext {
                                                driver: &driver,
                                                sessions: &sessions,
                                                maintenance_mode: &maintenance_mode,
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
            let driver = Arc::clone(&self.driver);
            let sessions = Arc::clone(&self.sessions);
            let maintenance_mode = Arc::clone(&self.maintenance_mode);
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
                                    let driver = Arc::clone(&driver);
                                    let sessions = Arc::clone(&sessions);
                                    let maintenance_mode = Arc::clone(&maintenance_mode);
                                    let stats = Arc::clone(&stats);
                                    let warn_limiter = Arc::clone(&warn_limiter);
                                    let tls_listener = Arc::clone(&tls_listener);
                                    thread::spawn(move || {
                                        match Self::accept_tls_stream(stream, &tls_listener) {
                                            Ok((stream, control)) => Self::handle_tls_connection(
                                                stream,
                                                control,
                                                ConnectionContext {
                                                    driver: &driver,
                                                    sessions: &sessions,
                                                    maintenance_mode: &maintenance_mode,
                                                    stats: &stats,
                                                    warn_limiter: &warn_limiter,
                                                },
                                            ),
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

    fn install_raw_frame_tap(&self) -> Result<Receiver<PiperFrame>, BridgeHostError> {
        let (tx, rx) = bounded(RAW_FRAME_TAP_QUEUE_CAPACITY);
        let callback = Arc::new(RawFrameTap { tx }) as Arc<dyn FrameCallback>;
        self.driver
            .hooks()
            .write()
            .map_err(|_| BridgeHostError::Driver("bridge hook registry lock poisoned".to_string()))?
            .add_callback(callback);
        Ok(rx)
    }

    fn frame_fanout_loop(
        rx: Receiver<PiperFrame>,
        sessions: Arc<SessionManager>,
        stats: Arc<BridgeHostStats>,
    ) {
        while let Ok(frame) = rx.recv() {
            stats.frame_rx_total.fetch_add(1, Ordering::Relaxed);
            let dropped = sessions.broadcast_frame(frame);
            if dropped > 0 {
                stats.queue_drop_total.fetch_add(dropped, Ordering::Relaxed);
            }
        }
    }

    fn lease_monitor_loop(
        driver: Arc<RobotPiper>,
        sessions: Arc<SessionManager>,
        maintenance_mode: Arc<AtomicBool>,
    ) {
        loop {
            thread::sleep(LEASE_MONITOR_INTERVAL);
            if !Self::maintenance_allowed(&driver, &maintenance_mode)
                && let Some(holder) = sessions.revoke_maintenance_lease()
            {
                holder.revoke_maintenance_lease();
            }
        }
    }

    fn maintenance_allowed(driver: &RobotPiper, maintenance_mode: &AtomicBool) -> bool {
        let health = driver.health();
        if health.fault.is_some() || !health.rx_alive || !health.tx_alive {
            return false;
        }

        let control = driver.get_robot_control();
        if control.is_enabled {
            return false;
        }

        health.connected || maintenance_mode.load(Ordering::Acquire)
    }

    fn build_status(
        driver: &RobotPiper,
        sessions: &SessionManager,
        stats: &BridgeHostStats,
    ) -> BridgeStatus {
        let health = driver.health();
        let device_state = if health.connected {
            BridgeDeviceState::Connected
        } else if health.rx_alive || health.tx_alive {
            BridgeDeviceState::Reconnecting
        } else {
            BridgeDeviceState::Disconnected
        };

        BridgeStatus {
            device_state,
            rx_fps_x1000: (stats.frame_rx_fps() * 1000.0) as u32,
            tx_fps_x1000: (stats.maintenance_tx_fps() * 1000.0) as u32,
            ipc_out_fps_x1000: (stats.ipc_out_fps() * 1000.0) as u32,
            ipc_in_fps_x1000: (stats.ipc_in_fps() * 1000.0) as u32,
            health_score: stats.health_score(health),
            usb_stall_count: 0,
            can_bus_off_count: 0,
            can_error_passive_count: 0,
            cpu_usage_percent: 0,
            session_count: sessions.count(),
            queue_drop_count: stats.queue_drop_total.load(Ordering::Relaxed),
        }
    }

    fn build_tls_listener_config(
        config: &BridgeTlsServerConfig,
    ) -> Result<TlsListenerConfig, BridgeHostError> {
        if config.allowed_client_cert_sha256.is_empty() {
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

        let mut fingerprints = Vec::with_capacity(config.allowed_client_cert_sha256.len());
        for fingerprint in &config.allowed_client_cert_sha256 {
            fingerprints.push(parse_cert_fingerprint(fingerprint).map_err(|err| {
                BridgeHostError::InvalidConfig(format!(
                    "invalid tls client fingerprint {fingerprint}: {err}"
                ))
            })?);
        }

        Ok(TlsListenerConfig {
            server_config: Arc::new(server_config),
            allowed_fingerprints: Arc::new(fingerprints),
            handshake_timeout: config.handshake_timeout,
        })
    }

    fn accept_tls_stream(
        tcp_stream: TcpStream,
        tls_listener: &TlsListenerConfig,
    ) -> Result<(ServerStream, Arc<dyn SessionControl>), BridgeHostError> {
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
        if !tls_listener.allowed_fingerprints.contains(&fingerprint_bytes) {
            return Err(BridgeHostError::Listener(
                "tcp tls client certificate is not in the allowlist".to_string(),
            ));
        }

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
                    if let Ok(control) = control_rx.try_recv() {
                        match control {
                            ConnectionOutput::Event(event) => {
                                if Self::write_server_message(
                                    &writer,
                                    &ServerMessage::Event(event),
                                    &stats,
                                )
                                .is_err()
                                {
                                    break;
                                }
                            },
                            ConnectionOutput::CloseAfterEvent(event) => {
                                let _ = Self::write_server_message(
                                    &writer,
                                    &ServerMessage::Event(event),
                                    &stats,
                                );
                                break;
                            },
                            ConnectionOutput::Shutdown => break,
                        }
                        continue;
                    }

                    crossbeam_channel::select! {
                        recv(control_rx) -> control => {
                            match control {
                                Ok(ConnectionOutput::Event(event)) => {
                                    if Self::write_server_message(&writer, &ServerMessage::Event(event), &stats).is_err() {
                                        break;
                                    }
                                },
                                Ok(ConnectionOutput::CloseAfterEvent(event)) => {
                                    let _ = Self::write_server_message(&writer, &ServerMessage::Event(event), &stats);
                                    break;
                                },
                                Ok(ConnectionOutput::Shutdown) | Err(_) => break,
                            }
                        },
                        recv(event_rx) -> output => {
                            match output {
                                Ok(ConnectionOutput::Event(event)) => {
                                    if Self::write_server_message(&writer, &ServerMessage::Event(event), &stats).is_err() {
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
        stats: &BridgeHostStats,
    ) -> bool {
        match output {
            ConnectionOutput::Event(event) => {
                Self::write_server_message_direct(writer, &ServerMessage::Event(event), stats)
                    .is_ok()
            },
            ConnectionOutput::CloseAfterEvent(event) => {
                let _ =
                    Self::write_server_message_direct(writer, &ServerMessage::Event(event), stats);
                false
            },
            ConnectionOutput::Shutdown => false,
        }
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
                    role_request,
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
                        role_request,
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
                        replaced.replace_and_close();
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
                            let status = Self::build_status(ctx.driver, ctx.sessions, ctx.stats);
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
                        ClientRequest::AcquireWriterLease {
                            request_id,
                            timeout_ms,
                        } => {
                            if !Self::maintenance_allowed(ctx.driver, ctx.maintenance_mode) {
                                let _ = Self::send_error(
                                    &writer,
                                    ctx.stats,
                                    request_id,
                                    ErrorCode::PermissionDenied,
                                    "maintenance lease only available while robot is not actively controlling and no runtime fault is latched",
                                );
                                continue;
                            }
                            match ctx.sessions.acquire_maintenance_lease(
                                active_session.session_id(),
                                Duration::from_millis(timeout_ms as u64),
                            ) {
                                LeaseAcquireResult::Granted => {
                                    let _ = Self::write_server_message(
                                        &writer,
                                        &ServerMessage::Response(ServerResponse::LeaseGranted {
                                            request_id,
                                            session_id: active_session.session_id(),
                                        }),
                                        ctx.stats,
                                    );
                                },
                                LeaseAcquireResult::Denied { holder_session_id } => {
                                    let _ = Self::write_server_message(
                                        &writer,
                                        &ServerMessage::Response(ServerResponse::LeaseDenied {
                                            request_id,
                                            holder_session_id,
                                        }),
                                        ctx.stats,
                                    );
                                },
                                LeaseAcquireResult::NotConnected => {
                                    let _ = Self::send_error(
                                        &writer,
                                        ctx.stats,
                                        request_id,
                                        ErrorCode::NotConnected,
                                        "bridge session was replaced or closed",
                                    );
                                    break;
                                },
                            }
                        },
                        ClientRequest::ReleaseWriterLease { request_id } => {
                            ctx.sessions.release_maintenance_lease(active_session.session_id());
                            let _ = Self::write_server_message(
                                &writer,
                                &ServerMessage::Response(ServerResponse::Ok { request_id }),
                                ctx.stats,
                            );
                        },
                        ClientRequest::SendFrame { request_id, frame } => {
                            if !Self::maintenance_allowed(ctx.driver, ctx.maintenance_mode) {
                                let _ = Self::send_error(
                                    &writer,
                                    ctx.stats,
                                    request_id,
                                    ErrorCode::PermissionDenied,
                                    "maintenance writes are disabled while active control or runtime fault is present",
                                );
                                continue;
                            }
                            if !ctx.sessions.has_maintenance_lease(active_session.session_id()) {
                                ctx.warn_limiter.warn("send-without-lease", || {
                                    format!(
                                        "rejecting send-frame without maintenance lease from session {} ({peer_label})",
                                        active_session.session_id()
                                    )
                                });
                                let _ = Self::send_error(
                                    &writer,
                                    ctx.stats,
                                    request_id,
                                    ErrorCode::PermissionDenied,
                                    "maintenance lease required",
                                );
                                continue;
                            }

                            match ctx.driver.send_frame(frame) {
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
                                        driver_error_code(&err),
                                        err.to_string(),
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
            removed.shutdown();
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
        ctx: ConnectionContext<'_>,
    ) {
        let peer_label = stream.peer_label();
        let mut session: Option<Arc<BridgeSession>> = None;
        let mut control_rx: Option<Receiver<ConnectionOutput>> = None;
        let mut event_rx: Option<Receiver<ConnectionOutput>> = None;

        'connection: loop {
            if let Some(rx) = control_rx.as_ref() {
                loop {
                    match rx.try_recv() {
                        Ok(output) => {
                            if !Self::handle_outbound_direct(&mut stream, output, ctx.stats) {
                                break 'connection;
                            }
                        },
                        Err(crossbeam_channel::TryRecvError::Empty) => break,
                        Err(crossbeam_channel::TryRecvError::Disconnected) => break 'connection,
                    }
                }
            }

            if let Some(rx) = event_rx.as_ref() {
                for _ in 0..32 {
                    match rx.try_recv() {
                        Ok(output) => {
                            if !Self::handle_outbound_direct(&mut stream, output, ctx.stats) {
                                break 'connection;
                            }
                        },
                        Err(crossbeam_channel::TryRecvError::Empty) => break,
                        Err(crossbeam_channel::TryRecvError::Disconnected) => break 'connection,
                    }
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
                    role_request,
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
                        role_request,
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
                        replaced.replace_and_close();
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
                            let status = Self::build_status(ctx.driver, ctx.sessions, ctx.stats);
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
                        ClientRequest::AcquireWriterLease {
                            request_id,
                            timeout_ms,
                        } => {
                            if !Self::maintenance_allowed(ctx.driver, ctx.maintenance_mode) {
                                let _ = Self::send_error_direct(
                                    &mut stream,
                                    ctx.stats,
                                    request_id,
                                    ErrorCode::PermissionDenied,
                                    "maintenance lease only available while robot is not actively controlling and no runtime fault is latched",
                                );
                                continue;
                            }
                            match ctx.sessions.acquire_maintenance_lease(
                                active_session.session_id(),
                                Duration::from_millis(timeout_ms as u64),
                            ) {
                                LeaseAcquireResult::Granted => {
                                    let _ = Self::write_server_message_direct(
                                        &mut stream,
                                        &ServerMessage::Response(ServerResponse::LeaseGranted {
                                            request_id,
                                            session_id: active_session.session_id(),
                                        }),
                                        ctx.stats,
                                    );
                                },
                                LeaseAcquireResult::Denied { holder_session_id } => {
                                    let _ = Self::write_server_message_direct(
                                        &mut stream,
                                        &ServerMessage::Response(ServerResponse::LeaseDenied {
                                            request_id,
                                            holder_session_id,
                                        }),
                                        ctx.stats,
                                    );
                                },
                                LeaseAcquireResult::NotConnected => {
                                    let _ = Self::send_error_direct(
                                        &mut stream,
                                        ctx.stats,
                                        request_id,
                                        ErrorCode::NotConnected,
                                        "bridge session was replaced or closed",
                                    );
                                    break;
                                },
                            }
                        },
                        ClientRequest::ReleaseWriterLease { request_id } => {
                            ctx.sessions.release_maintenance_lease(active_session.session_id());
                            let _ = Self::write_server_message_direct(
                                &mut stream,
                                &ServerMessage::Response(ServerResponse::Ok { request_id }),
                                ctx.stats,
                            );
                        },
                        ClientRequest::SendFrame { request_id, frame } => {
                            if !Self::maintenance_allowed(ctx.driver, ctx.maintenance_mode) {
                                let _ = Self::send_error_direct(
                                    &mut stream,
                                    ctx.stats,
                                    request_id,
                                    ErrorCode::PermissionDenied,
                                    "maintenance writes are disabled while active control or runtime fault is present",
                                );
                                continue;
                            }
                            if !ctx.sessions.has_maintenance_lease(active_session.session_id()) {
                                ctx.warn_limiter.warn("send-without-lease", || {
                                    format!(
                                        "rejecting send-frame without maintenance lease from session {} ({peer_label})",
                                        active_session.session_id()
                                    )
                                });
                                let _ = Self::send_error_direct(
                                    &mut stream,
                                    ctx.stats,
                                    request_id,
                                    ErrorCode::PermissionDenied,
                                    "maintenance lease required",
                                );
                                continue;
                            }

                            match ctx.driver.send_frame(frame) {
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
                                        driver_error_code(&err),
                                        err.to_string(),
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
            removed.shutdown();
        }
        stream.shutdown();
    }
}

fn driver_error_code(error: &DriverError) -> ErrorCode {
    match error {
        DriverError::Timeout | DriverError::RealtimeDeliveryTimeout => ErrorCode::Timeout,
        DriverError::ChannelFull | DriverError::ShutdownConflict => ErrorCode::Busy,
        DriverError::ControlPathClosed
        | DriverError::CommandAbortedByFault
        | DriverError::RealtimeDeliveryAbortedByFault { .. } => ErrorCode::PermissionDenied,
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
        ClientRequest::AcquireWriterLease { .. } => "AcquireWriterLease",
        ClientRequest::ReleaseWriterLease { .. } => "ReleaseWriterLease",
        ClientRequest::SendFrame { .. } => "SendFrame",
        ClientRequest::Ping { .. } => "Ping",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    struct NoopControl;

    impl SessionControl for NoopControl {
        fn shutdown(&self) {}
    }

    fn token(byte: u8) -> SessionToken {
        SessionToken::new([byte; 16])
    }

    #[test]
    fn same_token_replacement_invalidates_old_session_and_preserves_lease_on_new_active_session() {
        let manager = SessionManager::new();
        let first = manager.commit_prepared(manager.prepare_session(
            token(1),
            BridgeRole::WriterCandidate,
            vec![],
            Arc::new(NoopControl),
        ));
        assert_eq!(
            manager.acquire_maintenance_lease(first.session.session_id(), Duration::from_millis(1)),
            LeaseAcquireResult::Granted
        );

        let second = manager.commit_prepared(manager.prepare_session(
            token(1),
            BridgeRole::WriterCandidate,
            vec![],
            Arc::new(NoopControl),
        ));

        assert!(first.replaced.is_none());
        assert!(second.replaced.is_some());
        let old = second.replaced.as_ref().unwrap().clone();
        assert!(!old.is_active());
        assert!(second.session.is_active());
        assert_eq!(
            manager.acquire_maintenance_lease(old.session_id(), Duration::from_millis(1)),
            LeaseAcquireResult::NotConnected
        );
        assert!(manager.has_maintenance_lease(second.session.session_id()));
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
            vec![],
            event_tx,
            control_tx,
            Arc::new(NoopControl),
        );

        for _ in 0..OUTBOUND_QUEUE_CAPACITY {
            assert!(session.enqueue_frame(PiperFrame::new_standard(0x123, &[1])));
        }

        session.replace_and_close();
        assert_eq!(session.lifecycle(), SessionLifecycle::Replaced);
        match control_rx.try_recv().expect("priority close event should be queued") {
            ConnectionOutput::CloseAfterEvent(BridgeEvent::SessionReplaced) => {},
            other => panic!("unexpected control output: {other:?}"),
        }
    }
}
