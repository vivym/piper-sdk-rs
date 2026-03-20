//! Bridge v2 daemon core.
//!
//! The daemon keeps exclusive ownership of the GS-USB device and exposes a
//! non-realtime stream bridge for debug / record / replay workloads.

use crate::session_manager::{
    ConnectionOutput, LeaseAcquireResult, SessionControl, SessionManager,
};
use piper_can::BridgeTxAdapter;
use piper_can::CanError;
use piper_can::gs_usb::{GsUsbCanAdapter, split::GsUsbRxAdapter};
use piper_can::gs_usb_bridge::protocol::{
    self, BridgeDeviceState, BridgeStatus, ClientRequest, ErrorCode, ServerMessage, ServerResponse,
};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
#[cfg(unix)]
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::atomic::{AtomicU8, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use tracing::{error, info, warn};

const STATUS_PRINT_INTERVAL: Duration = Duration::from_secs(5);
const CPU_MONITOR_INTERVAL: Duration = Duration::from_secs(1);
const AUTH_LOG_WINDOW: Duration = Duration::from_secs(1);

fn bridge_error_code(error: &CanError) -> ErrorCode {
    match error {
        CanError::Timeout => ErrorCode::Timeout,
        CanError::Device(device) => match device.kind {
            piper_can::CanDeviceErrorKind::NoDevice | piper_can::CanDeviceErrorKind::NotFound => {
                ErrorCode::DeviceNotFound
            },
            piper_can::CanDeviceErrorKind::Busy => ErrorCode::DeviceBusy,
            piper_can::CanDeviceErrorKind::InvalidResponse => ErrorCode::ProtocolError,
            _ => ErrorCode::DeviceError,
        },
        _ => ErrorCode::DeviceError,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum DeviceState {
    Connected,
    Disconnected,
    Reconnecting,
}

impl DeviceState {
    fn as_u8(self) -> u8 {
        self as u8
    }

    fn from_u8(value: u8) -> Self {
        match value {
            x if x == Self::Connected as u8 => Self::Connected,
            x if x == Self::Reconnecting as u8 => Self::Reconnecting,
            _ => Self::Disconnected,
        }
    }

    fn as_bridge_state(self) -> BridgeDeviceState {
        match self {
            Self::Connected => BridgeDeviceState::Connected,
            Self::Disconnected => BridgeDeviceState::Disconnected,
            Self::Reconnecting => BridgeDeviceState::Reconnecting,
        }
    }
}

#[derive(Debug)]
struct DeviceStateCell {
    state: AtomicU8,
}

impl DeviceStateCell {
    fn new(initial: DeviceState) -> Self {
        Self {
            state: AtomicU8::new(initial.as_u8()),
        }
    }

    fn load(&self) -> DeviceState {
        DeviceState::from_u8(self.state.load(Ordering::Acquire))
    }

    fn store(&self, state: DeviceState) {
        self.state.store(state.as_u8(), Ordering::Release);
    }
}

#[derive(Debug, Clone)]
pub struct DaemonConfig {
    pub uds_path: Option<String>,
    pub tcp_addr: Option<String>,
    pub bitrate: u32,
    pub serial_number: Option<String>,
    pub reconnect_interval: Duration,
    pub reconnect_debounce: Duration,
    pub bridge_tx_timeout: Duration,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            #[cfg(unix)]
            uds_path: Some("/tmp/gs_usb_daemon.sock".to_string()),
            #[cfg(not(unix))]
            uds_path: None,
            #[cfg(unix)]
            tcp_addr: None,
            #[cfg(not(unix))]
            tcp_addr: Some("127.0.0.1:18888".to_string()),
            bitrate: 1_000_000,
            serial_number: None,
            reconnect_interval: Duration::from_secs(1),
            reconnect_debounce: Duration::from_millis(500),
            bridge_tx_timeout: Duration::from_millis(100),
        }
    }
}

#[derive(Debug, Clone)]
pub enum DaemonError {
    SocketInit(String),
    DeviceInit(String),
    DeviceConfig(String),
    Io(String),
}

impl std::fmt::Display for DaemonError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SocketInit(msg) => write!(f, "{msg}"),
            Self::DeviceInit(msg) => write!(f, "{msg}"),
            Self::DeviceConfig(msg) => write!(f, "{msg}"),
            Self::Io(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for DaemonError {}

#[derive(Debug)]
struct DaemonStats {
    started_at: Instant,
    rx_total: AtomicU64,
    tx_total: AtomicU64,
    ipc_in_total: AtomicU64,
    ipc_out_total: AtomicU64,
    queue_drop_total: AtomicU64,
    usb_stall_count: AtomicU64,
    can_bus_off_count: AtomicU64,
    can_error_passive_count: AtomicU64,
    cpu_usage_percent: AtomicU32,
}

impl DaemonStats {
    fn new() -> Self {
        Self {
            started_at: Instant::now(),
            rx_total: AtomicU64::new(0),
            tx_total: AtomicU64::new(0),
            ipc_in_total: AtomicU64::new(0),
            ipc_out_total: AtomicU64::new(0),
            queue_drop_total: AtomicU64::new(0),
            usb_stall_count: AtomicU64::new(0),
            can_bus_off_count: AtomicU64::new(0),
            can_error_passive_count: AtomicU64::new(0),
            cpu_usage_percent: AtomicU32::new(0),
        }
    }

    fn inc_rx(&self) {
        self.rx_total.fetch_add(1, Ordering::Relaxed);
    }

    fn inc_tx(&self) {
        self.tx_total.fetch_add(1, Ordering::Relaxed);
    }

    fn inc_ipc_in(&self) {
        self.ipc_in_total.fetch_add(1, Ordering::Relaxed);
    }

    fn inc_ipc_out(&self) {
        self.ipc_out_total.fetch_add(1, Ordering::Relaxed);
    }

    fn inc_queue_drops(&self, dropped: u64) {
        self.queue_drop_total.fetch_add(dropped, Ordering::Relaxed);
    }

    fn elapsed_secs(&self) -> f64 {
        self.started_at.elapsed().as_secs_f64().max(0.001)
    }

    fn fps(counter: &AtomicU64, elapsed: f64) -> f64 {
        counter.load(Ordering::Relaxed) as f64 / elapsed
    }

    fn rx_fps(&self) -> f64 {
        Self::fps(&self.rx_total, self.elapsed_secs())
    }

    fn tx_fps(&self) -> f64 {
        Self::fps(&self.tx_total, self.elapsed_secs())
    }

    fn ipc_in_fps(&self) -> f64 {
        Self::fps(&self.ipc_in_total, self.elapsed_secs())
    }

    fn ipc_out_fps(&self) -> f64 {
        Self::fps(&self.ipc_out_total, self.elapsed_secs())
    }

    fn health_score(&self, device_state: DeviceState) -> u8 {
        let mut score = match device_state {
            DeviceState::Connected => 100i32,
            DeviceState::Reconnecting => 70,
            DeviceState::Disconnected => 40,
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
}

impl ServerStream {
    fn shutdown(&self) {
        match self {
            #[cfg(unix)]
            Self::Unix(stream) => {
                let _ = stream.shutdown(std::net::Shutdown::Both);
            },
            Self::Tcp(stream) => {
                let _ = stream.shutdown(std::net::Shutdown::Both);
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
        }
    }
}

impl Read for ServerStream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            #[cfg(unix)]
            Self::Unix(stream) => stream.read(buf),
            Self::Tcp(stream) => stream.read(buf),
        }
    }
}

impl Write for ServerStream {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            #[cfg(unix)]
            Self::Unix(stream) => stream.write(buf),
            Self::Tcp(stream) => stream.write(buf),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            #[cfg(unix)]
            Self::Unix(stream) => stream.flush(),
            Self::Tcp(stream) => stream.flush(),
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

struct ConnectionContext<'a> {
    sessions: &'a Arc<SessionManager>,
    tx_adapter: &'a Arc<Mutex<Option<Box<dyn BridgeTxAdapter + Send>>>>,
    device_state: &'a Arc<DeviceStateCell>,
    stats: &'a Arc<DaemonStats>,
    warn_limiter: &'a Arc<WarnRateLimiter>,
}

pub struct Daemon {
    rx_adapter: Arc<Mutex<Option<GsUsbRxAdapter>>>,
    tx_adapter: Arc<Mutex<Option<Box<dyn BridgeTxAdapter + Send>>>>,
    device_state: Arc<DeviceStateCell>,
    sessions: Arc<SessionManager>,
    config: DaemonConfig,
    stats: Arc<DaemonStats>,
    warn_limiter: Arc<WarnRateLimiter>,
}

impl Daemon {
    pub fn new(config: DaemonConfig) -> Result<Self, DaemonError> {
        Ok(Self {
            rx_adapter: Arc::new(Mutex::new(None)),
            tx_adapter: Arc::new(Mutex::new(None)),
            device_state: Arc::new(DeviceStateCell::new(DeviceState::Disconnected)),
            sessions: Arc::new(SessionManager::new()),
            config,
            stats: Arc::new(DaemonStats::new()),
            warn_limiter: Arc::new(WarnRateLimiter::new(AUTH_LOG_WINDOW)),
        })
    }

    #[cfg(unix)]
    fn init_unix_listener(&self) -> Result<Option<UnixListener>, DaemonError> {
        let Some(path) = self.config.uds_path.as_ref() else {
            return Ok(None);
        };
        if std::path::Path::new(path).exists() {
            std::fs::remove_file(path).map_err(|err| {
                DaemonError::SocketInit(format!(
                    "failed to remove existing uds socket {path}: {err}"
                ))
            })?;
        }
        let listener = UnixListener::bind(path).map_err(|err| {
            DaemonError::SocketInit(format!("failed to bind uds listener: {err}"))
        })?;
        Ok(Some(listener))
    }

    #[cfg(not(unix))]
    fn init_unix_listener(&self) -> Result<Option<()>, DaemonError> {
        Ok(None)
    }

    fn init_tcp_listener(&self) -> Result<Option<TcpListener>, DaemonError> {
        let Some(addr) = self.config.tcp_addr.as_ref() else {
            return Ok(None);
        };
        let listener = TcpListener::bind(addr).map_err(|err| {
            DaemonError::SocketInit(format!("failed to bind tcp listener: {err}"))
        })?;
        Ok(Some(listener))
    }

    fn try_connect_device(
        config: &DaemonConfig,
        stats: Arc<DaemonStats>,
    ) -> Result<(GsUsbRxAdapter, Box<dyn BridgeTxAdapter + Send>), DaemonError> {
        let mut adapter = GsUsbCanAdapter::new_with_serial(config.serial_number.as_deref())
            .map_err(|err| DaemonError::DeviceInit(err.to_string()))?;
        adapter.set_receive_timeout(Duration::from_millis(2));
        adapter
            .configure(config.bitrate)
            .map_err(|err| DaemonError::DeviceConfig(err.to_string()))?;

        let stats_for_stall = Arc::clone(&stats);
        adapter.set_stall_count_callback(move || {
            stats_for_stall.usb_stall_count.fetch_add(1, Ordering::Relaxed);
        });

        let (mut rx_adapter, tx_adapter) = adapter
            .split()
            .map_err(|err| DaemonError::DeviceInit(format!("failed to split adapter: {err}")))?;

        let stats_for_bus_off = Arc::clone(&stats);
        rx_adapter.set_bus_off_callback(move |is_bus_off| {
            if is_bus_off {
                stats_for_bus_off.can_bus_off_count.fetch_add(1, Ordering::Relaxed);
            }
        });

        let stats_for_error_passive = Arc::clone(&stats);
        rx_adapter.set_error_passive_callback(move |is_error_passive| {
            if is_error_passive {
                stats_for_error_passive.can_error_passive_count.fetch_add(1, Ordering::Relaxed);
            }
        });

        Ok((
            rx_adapter,
            Box::new(tx_adapter.into_bridge(config.bridge_tx_timeout)),
        ))
    }

    fn device_manager_loop(
        rx_adapter: Arc<Mutex<Option<GsUsbRxAdapter>>>,
        tx_adapter: Arc<Mutex<Option<Box<dyn BridgeTxAdapter + Send>>>>,
        device_state: Arc<DeviceStateCell>,
        stats: Arc<DaemonStats>,
        config: DaemonConfig,
    ) {
        let mut last_disconnect = None;

        loop {
            match device_state.load() {
                DeviceState::Connected => thread::sleep(Duration::from_millis(100)),
                DeviceState::Disconnected => {
                    let now = Instant::now();
                    if let Some(last) = last_disconnect {
                        let since_last = now.duration_since(last);
                        if since_last < config.reconnect_debounce {
                            thread::sleep(config.reconnect_debounce - since_last);
                        }
                    }
                    last_disconnect = Some(now);
                    *rx_adapter.lock().unwrap() = None;
                    *tx_adapter.lock().unwrap() = None;
                    device_state.store(DeviceState::Reconnecting);
                    warn!("bridge daemon entering reconnecting state");
                },
                DeviceState::Reconnecting => {
                    match Self::try_connect_device(&config, Arc::clone(&stats)) {
                        Ok((new_rx, new_tx)) => {
                            *rx_adapter.lock().unwrap() = Some(new_rx);
                            *tx_adapter.lock().unwrap() = Some(new_tx);
                            device_state.store(DeviceState::Connected);
                            info!("bridge daemon connected to GS-USB device");
                        },
                        Err(err) => {
                            warn!("failed to connect GS-USB device: {err}");
                            thread::sleep(config.reconnect_interval);
                        },
                    }
                },
            }
        }
    }

    fn usb_receive_loop(
        rx_adapter: Arc<Mutex<Option<GsUsbRxAdapter>>>,
        device_state: Arc<DeviceStateCell>,
        sessions: Arc<SessionManager>,
        stats: Arc<DaemonStats>,
    ) {
        crate::macos_qos::set_high_priority();
        loop {
            if device_state.load() != DeviceState::Connected {
                thread::sleep(Duration::from_millis(100));
                continue;
            }

            let frame = {
                let mut guard = rx_adapter.lock().unwrap();
                let Some(adapter) = guard.as_mut() else {
                    thread::sleep(Duration::from_millis(100));
                    continue;
                };
                match adapter.receive() {
                    Ok(frame) => frame,
                    Err(CanError::Timeout) => continue,
                    Err(err) => {
                        warn!("bridge daemon RX error: {err}");
                        device_state.store(DeviceState::Disconnected);
                        thread::sleep(Duration::from_millis(100));
                        continue;
                    },
                }
            };

            stats.inc_rx();
            let dropped = sessions.broadcast_frame(frame);
            if dropped > 0 {
                stats.inc_queue_drops(dropped);
            }
        }
    }

    fn cpu_monitor_loop(stats: Arc<DaemonStats>) {
        use sysinfo::{CpuRefreshKind, RefreshKind, System};

        crate::macos_qos::set_low_priority();
        let mut sys = System::new_with_specifics(
            RefreshKind::nothing().with_cpu(CpuRefreshKind::everything()),
        );

        loop {
            thread::sleep(CPU_MONITOR_INTERVAL);
            sys.refresh_cpu_all();
            stats.cpu_usage_percent.store(sys.global_cpu_usage() as u32, Ordering::Relaxed);
        }
    }

    fn status_print_loop(
        sessions: Arc<SessionManager>,
        device_state: Arc<DeviceStateCell>,
        stats: Arc<DaemonStats>,
    ) {
        crate::macos_qos::set_low_priority();
        loop {
            thread::sleep(STATUS_PRINT_INTERVAL);
            info!(
                "state={:?} sessions={} rx_fps={:.1} tx_fps={:.1} ipc_in_fps={:.1} ipc_out_fps={:.1} queue_drops={} cpu={}%",
                device_state.load(),
                sessions.count(),
                stats.rx_fps(),
                stats.tx_fps(),
                stats.ipc_in_fps(),
                stats.ipc_out_fps(),
                stats.queue_drop_total.load(Ordering::Relaxed),
                stats.cpu_usage_percent.load(Ordering::Relaxed),
            );
        }
    }

    fn build_status(
        device_state: DeviceState,
        sessions: &SessionManager,
        stats: &DaemonStats,
    ) -> BridgeStatus {
        BridgeStatus {
            device_state: device_state.as_bridge_state(),
            rx_fps_x1000: (stats.rx_fps() * 1000.0) as u32,
            tx_fps_x1000: (stats.tx_fps() * 1000.0) as u32,
            ipc_out_fps_x1000: (stats.ipc_out_fps() * 1000.0) as u32,
            ipc_in_fps_x1000: (stats.ipc_in_fps() * 1000.0) as u32,
            health_score: stats.health_score(device_state),
            usb_stall_count: stats.usb_stall_count.load(Ordering::Relaxed),
            can_bus_off_count: stats.can_bus_off_count.load(Ordering::Relaxed),
            can_error_passive_count: stats.can_error_passive_count.load(Ordering::Relaxed),
            cpu_usage_percent: stats.cpu_usage_percent.load(Ordering::Relaxed) as u8,
            session_count: sessions.count(),
            queue_drop_count: stats.queue_drop_total.load(Ordering::Relaxed),
        }
    }

    fn spawn_writer_thread(
        writer: Arc<Mutex<ServerStream>>,
        rx: crossbeam_channel::Receiver<ConnectionOutput>,
        stats: Arc<DaemonStats>,
    ) -> Result<(), DaemonError> {
        thread::Builder::new()
            .name("bridge_writer".into())
            .spawn(move || {
                while let Ok(output) = rx.recv() {
                    match output {
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
                }
                writer.lock().unwrap().shutdown();
            })
            .map(|_| ())
            .map_err(|err| DaemonError::Io(format!("failed to spawn bridge writer thread: {err}")))
    }

    fn write_server_message(
        writer: &Arc<Mutex<ServerStream>>,
        message: &ServerMessage,
        stats: &DaemonStats,
    ) -> Result<(), DaemonError> {
        let encoded = protocol::encode_server_message(message)
            .map_err(|err| DaemonError::Io(format!("failed to encode server message: {err}")))?;
        let mut guard = writer.lock().unwrap();
        protocol::write_framed(&mut *guard, &encoded)
            .map_err(|err| DaemonError::Io(format!("failed to write server message: {err}")))?;
        stats.inc_ipc_out();
        Ok(())
    }

    fn send_error(
        writer: &Arc<Mutex<ServerStream>>,
        stats: &DaemonStats,
        request_id: u32,
        code: ErrorCode,
        message: impl Into<String>,
    ) -> Result<(), DaemonError> {
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
        let mut session_id = None;

        loop {
            let payload = match protocol::read_framed(&mut reader) {
                Ok(payload) => payload,
                Err(protocol::ProtocolError::Io(_)) => break,
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
            ctx.stats.inc_ipc_in();

            match request {
                ClientRequest::Hello {
                    request_id,
                    session_token,
                    role_request,
                    filters,
                } => {
                    if session_id.is_some() {
                        let _ = Self::send_error(
                            &writer,
                            ctx.stats,
                            request_id,
                            ErrorCode::InvalidMessage,
                            "hello already completed for this connection",
                        );
                        continue;
                    }

                    let (event_tx, event_rx) = SessionManager::new_connection_queue();
                    let control_for_session: Arc<dyn SessionControl> = control.clone();
                    let prepared = ctx.sessions.prepare_session(
                        session_token,
                        role_request,
                        filters,
                        event_tx,
                        control_for_session,
                    );

                    let hello_ack = ServerMessage::Response(ServerResponse::HelloAck {
                        request_id,
                        session_id: prepared.session_id(),
                        role_granted: prepared.role_granted(),
                    });
                    if Self::write_server_message(&writer, &hello_ack, ctx.stats).is_err() {
                        break;
                    }

                    let register = ctx.sessions.commit_prepared(prepared);
                    session_id = Some(register.session_id);
                    if let Err(err) = Self::spawn_writer_thread(
                        Arc::clone(&writer),
                        event_rx,
                        Arc::clone(ctx.stats),
                    ) {
                        error!("failed to start bridge writer thread: {err}");
                        let removed = ctx.sessions.unregister_session(register.session_id);
                        if let Some(session) = removed {
                            session.shutdown();
                        }
                        break;
                    }
                    if let Some(replaced) = register.replaced {
                        replaced.replace_and_close();
                    }
                },
                other => {
                    let Some(active_session_id) = session_id else {
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

                    match other {
                        ClientRequest::GetStatus { request_id } => {
                            let status = Self::build_status(
                                ctx.device_state.load(),
                                ctx.sessions,
                                ctx.stats,
                            );
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
                            ctx.sessions.set_filters(active_session_id, filters);
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
                            match ctx.sessions.acquire_writer_lease(
                                active_session_id,
                                Duration::from_millis(timeout_ms as u64),
                            ) {
                                LeaseAcquireResult::Granted => {
                                    let _ = Self::write_server_message(
                                        &writer,
                                        &ServerMessage::Response(ServerResponse::LeaseGranted {
                                            request_id,
                                            session_id: active_session_id,
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
                            }
                        },
                        ClientRequest::ReleaseWriterLease { request_id } => {
                            ctx.sessions.release_writer_lease(active_session_id);
                            let _ = Self::write_server_message(
                                &writer,
                                &ServerMessage::Response(ServerResponse::Ok { request_id }),
                                ctx.stats,
                            );
                        },
                        ClientRequest::SendFrame { request_id, frame } => {
                            if !ctx.sessions.has_writer_lease(active_session_id) {
                                ctx.warn_limiter.warn("send-without-lease", || {
                                    format!(
                                        "rejecting send-frame without writer lease from session {} ({peer_label})",
                                        active_session_id
                                    )
                                });
                                let _ = Self::send_error(
                                    &writer,
                                    ctx.stats,
                                    request_id,
                                    ErrorCode::PermissionDenied,
                                    "writer lease required",
                                );
                                continue;
                            }

                            let mut tx_guard = ctx.tx_adapter.lock().unwrap();
                            if let Some(adapter) = tx_guard.as_mut() {
                                match adapter.send_bridge(frame) {
                                    Ok(()) => {
                                        ctx.stats.inc_tx();
                                        let _ = Self::write_server_message(
                                            &writer,
                                            &ServerMessage::Response(ServerResponse::Ok {
                                                request_id,
                                            }),
                                            ctx.stats,
                                        );
                                    },
                                    Err(err) => {
                                        drop(tx_guard);
                                        ctx.device_state.store(DeviceState::Disconnected);
                                        let _ = Self::send_error(
                                            &writer,
                                            ctx.stats,
                                            request_id,
                                            bridge_error_code(&err),
                                            err.to_string(),
                                        );
                                    },
                                }
                            } else {
                                drop(tx_guard);
                                let _ = Self::send_error(
                                    &writer,
                                    ctx.stats,
                                    request_id,
                                    ErrorCode::NotConnected,
                                    "bridge TX adapter not available",
                                );
                            }
                        },
                        ClientRequest::Ping { request_id } => {
                            let _ = Self::write_server_message(
                                &writer,
                                &ServerMessage::Response(ServerResponse::Ok { request_id }),
                                ctx.stats,
                            );
                        },
                        ClientRequest::Hello { .. } => unreachable!(),
                    }
                },
            }
        }

        if let Some(active_session_id) = session_id
            && let Some(session) = ctx.sessions.unregister_session(active_session_id)
        {
            session.shutdown();
        }
        control.shutdown();
    }

    #[cfg(unix)]
    fn unix_accept_loop(
        listener: UnixListener,
        sessions: Arc<SessionManager>,
        tx_adapter: Arc<Mutex<Option<Box<dyn BridgeTxAdapter + Send>>>>,
        device_state: Arc<DeviceStateCell>,
        stats: Arc<DaemonStats>,
        warn_limiter: Arc<WarnRateLimiter>,
    ) {
        for stream in listener.incoming() {
            match stream {
                Ok(stream) => {
                    let writer = match stream.try_clone() {
                        Ok(writer) => Arc::new(Mutex::new(ServerStream::Unix(writer))),
                        Err(err) => {
                            error!("failed to clone unix stream: {err}");
                            continue;
                        },
                    };
                    let sessions = Arc::clone(&sessions);
                    let tx_adapter = Arc::clone(&tx_adapter);
                    let device_state = Arc::clone(&device_state);
                    let stats = Arc::clone(&stats);
                    let warn_limiter = Arc::clone(&warn_limiter);
                    thread::Builder::new()
                        .name("bridge_conn_uds".into())
                        .spawn(move || {
                            Daemon::handle_connection(
                                ServerStream::Unix(stream),
                                writer,
                                ConnectionContext {
                                    sessions: &sessions,
                                    tx_adapter: &tx_adapter,
                                    device_state: &device_state,
                                    stats: &stats,
                                    warn_limiter: &warn_limiter,
                                },
                            );
                        })
                        .ok();
                },
                Err(err) => warn!("failed to accept unix bridge connection: {err}"),
            }
        }
    }

    fn tcp_accept_loop(
        listener: TcpListener,
        sessions: Arc<SessionManager>,
        tx_adapter: Arc<Mutex<Option<Box<dyn BridgeTxAdapter + Send>>>>,
        device_state: Arc<DeviceStateCell>,
        stats: Arc<DaemonStats>,
        warn_limiter: Arc<WarnRateLimiter>,
    ) {
        for stream in listener.incoming() {
            match stream {
                Ok(stream) => {
                    if let Err(err) = stream.set_nodelay(true) {
                        warn!("failed to enable tcp_nodelay: {err}");
                    }
                    let writer = match stream.try_clone() {
                        Ok(writer) => Arc::new(Mutex::new(ServerStream::Tcp(writer))),
                        Err(err) => {
                            error!("failed to clone tcp stream: {err}");
                            continue;
                        },
                    };
                    let sessions = Arc::clone(&sessions);
                    let tx_adapter = Arc::clone(&tx_adapter);
                    let device_state = Arc::clone(&device_state);
                    let stats = Arc::clone(&stats);
                    let warn_limiter = Arc::clone(&warn_limiter);
                    thread::Builder::new()
                        .name("bridge_conn_tcp".into())
                        .spawn(move || {
                            Daemon::handle_connection(
                                ServerStream::Tcp(stream),
                                writer,
                                ConnectionContext {
                                    sessions: &sessions,
                                    tx_adapter: &tx_adapter,
                                    device_state: &device_state,
                                    stats: &stats,
                                    warn_limiter: &warn_limiter,
                                },
                            );
                        })
                        .ok();
                },
                Err(err) => warn!("failed to accept tcp bridge connection: {err}"),
            }
        }
    }

    pub fn run(&mut self) -> Result<(), DaemonError> {
        #[cfg(unix)]
        let unix_listener = self.init_unix_listener()?;
        let tcp_listener = self.init_tcp_listener()?;

        let rx_adapter = Arc::clone(&self.rx_adapter);
        let tx_adapter = Arc::clone(&self.tx_adapter);
        let device_state = Arc::clone(&self.device_state);
        let stats = Arc::clone(&self.stats);
        let config = self.config.clone();
        thread::Builder::new()
            .name("device_manager".into())
            .spawn(move || {
                Self::device_manager_loop(rx_adapter, tx_adapter, device_state, stats, config);
            })
            .map_err(|err| DaemonError::Io(format!("failed to spawn device manager: {err}")))?;

        let rx_adapter = Arc::clone(&self.rx_adapter);
        let device_state = Arc::clone(&self.device_state);
        let sessions = Arc::clone(&self.sessions);
        let stats = Arc::clone(&self.stats);
        thread::Builder::new()
            .name("bridge_rx_fanout".into())
            .spawn(move || {
                Self::usb_receive_loop(rx_adapter, device_state, sessions, stats);
            })
            .map_err(|err| DaemonError::Io(format!("failed to spawn usb receive loop: {err}")))?;

        let stats = Arc::clone(&self.stats);
        thread::Builder::new()
            .name("bridge_cpu_monitor".into())
            .spawn(move || {
                Self::cpu_monitor_loop(stats);
            })
            .map_err(|err| DaemonError::Io(format!("failed to spawn cpu monitor: {err}")))?;

        let sessions = Arc::clone(&self.sessions);
        let device_state = Arc::clone(&self.device_state);
        let stats = Arc::clone(&self.stats);
        thread::Builder::new()
            .name("bridge_status_print".into())
            .spawn(move || {
                Self::status_print_loop(sessions, device_state, stats);
            })
            .map_err(|err| DaemonError::Io(format!("failed to spawn status printer: {err}")))?;

        #[cfg(unix)]
        if let Some(listener) = unix_listener {
            let sessions = Arc::clone(&self.sessions);
            let tx_adapter = Arc::clone(&self.tx_adapter);
            let device_state = Arc::clone(&self.device_state);
            let stats = Arc::clone(&self.stats);
            let warn_limiter = Arc::clone(&self.warn_limiter);
            thread::Builder::new()
                .name("bridge_accept_uds".into())
                .spawn(move || {
                    Self::unix_accept_loop(
                        listener,
                        sessions,
                        tx_adapter,
                        device_state,
                        stats,
                        warn_limiter,
                    );
                })
                .map_err(|err| {
                    DaemonError::Io(format!("failed to spawn unix accept loop: {err}"))
                })?;
        }

        if let Some(listener) = tcp_listener {
            let sessions = Arc::clone(&self.sessions);
            let tx_adapter = Arc::clone(&self.tx_adapter);
            let device_state = Arc::clone(&self.device_state);
            let stats = Arc::clone(&self.stats);
            let warn_limiter = Arc::clone(&self.warn_limiter);
            thread::Builder::new()
                .name("bridge_accept_tcp".into())
                .spawn(move || {
                    Self::tcp_accept_loop(
                        listener,
                        sessions,
                        tx_adapter,
                        device_state,
                        stats,
                        warn_limiter,
                    );
                })
                .map_err(|err| {
                    DaemonError::Io(format!("failed to spawn tcp accept loop: {err}"))
                })?;
        }

        info!("GS-USB bridge daemon started");
        loop {
            thread::park();
        }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(path) = self.config.uds_path.as_ref()
            && std::path::Path::new(path).exists()
        {
            let _ = std::fs::remove_file(path);
        }
    }
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
    use piper_can::PiperFrame;
    use piper_can::gs_usb_bridge::protocol::{BridgeEvent, BridgeRole};
    use piper_can::gs_usb_bridge::protocol::{CanIdFilter, SessionToken};
    use std::sync::Arc;

    type ConnectedTestContext = (
        Arc<SessionManager>,
        Arc<Mutex<Option<Box<dyn BridgeTxAdapter + Send>>>>,
        Arc<DeviceStateCell>,
        Arc<DaemonStats>,
        Arc<WarnRateLimiter>,
    );

    struct CountingBridgeTxAdapter {
        count: Arc<AtomicU64>,
    }

    impl BridgeTxAdapter for CountingBridgeTxAdapter {
        fn send_bridge(&mut self, _frame: PiperFrame) -> Result<(), CanError> {
            self.count.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
    }

    fn connected_test_context(tx_count: Arc<AtomicU64>) -> ConnectedTestContext {
        (
            Arc::new(SessionManager::new()),
            Arc::new(Mutex::new(Some(
                Box::new(CountingBridgeTxAdapter { count: tx_count })
                    as Box<dyn BridgeTxAdapter + Send>,
            ))),
            Arc::new(DeviceStateCell::new(DeviceState::Connected)),
            Arc::new(DaemonStats::new()),
            Arc::new(WarnRateLimiter::new(Duration::from_secs(1))),
        )
    }

    #[test]
    fn get_status_requires_hello_handshake() {
        let tx_count = Arc::new(AtomicU64::new(0));
        let (sessions, tx_adapter, device_state, stats, warn_limiter) =
            connected_test_context(Arc::clone(&tx_count));

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let writer = Arc::new(Mutex::new(ServerStream::Tcp(stream.try_clone().unwrap())));
            Daemon::handle_connection(
                ServerStream::Tcp(stream),
                writer,
                ConnectionContext {
                    sessions: &sessions,
                    tx_adapter: &tx_adapter,
                    device_state: &device_state,
                    stats: &stats,
                    warn_limiter: &warn_limiter,
                },
            );
        });

        let mut client = TcpStream::connect(addr).unwrap();
        let request = ClientRequest::GetStatus { request_id: 1 };
        let encoded = protocol::encode_client_request(&request).unwrap();
        protocol::write_framed(&mut client, &encoded).unwrap();
        let payload = protocol::read_framed(&mut client).unwrap();
        let message = protocol::decode_server_message(&payload).unwrap();
        match message {
            ServerMessage::Response(ServerResponse::Error { code, .. }) => {
                assert_eq!(code, ErrorCode::NotConnected)
            },
            other => panic!("unexpected response: {other:?}"),
        }
        drop(client);
        handle.join().unwrap();
    }

    #[test]
    fn hello_then_send_requires_writer_lease() {
        let tx_count = Arc::new(AtomicU64::new(0));
        let (sessions, tx_adapter, device_state, stats, warn_limiter) =
            connected_test_context(Arc::clone(&tx_count));

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let writer = Arc::new(Mutex::new(ServerStream::Tcp(stream.try_clone().unwrap())));
            Daemon::handle_connection(
                ServerStream::Tcp(stream),
                writer,
                ConnectionContext {
                    sessions: &sessions,
                    tx_adapter: &tx_adapter,
                    device_state: &device_state,
                    stats: &stats,
                    warn_limiter: &warn_limiter,
                },
            );
        });

        let mut client = TcpStream::connect(addr).unwrap();
        let hello = ClientRequest::Hello {
            request_id: 1,
            session_token: SessionToken::new([1; 16]),
            role_request: BridgeRole::WriterCandidate,
            filters: vec![CanIdFilter::new(0x100, 0x1FF)],
        };
        protocol::write_framed(
            &mut client,
            &protocol::encode_client_request(&hello).unwrap(),
        )
        .unwrap();
        let _ =
            protocol::decode_server_message(&protocol::read_framed(&mut client).unwrap()).unwrap();

        let send = ClientRequest::SendFrame {
            request_id: 2,
            frame: PiperFrame::new_standard(0x123, &[1, 2, 3, 4]),
        };
        protocol::write_framed(
            &mut client,
            &protocol::encode_client_request(&send).unwrap(),
        )
        .unwrap();
        let message =
            protocol::decode_server_message(&protocol::read_framed(&mut client).unwrap()).unwrap();
        match message {
            ServerMessage::Response(ServerResponse::Error { code, .. }) => {
                assert_eq!(code, ErrorCode::PermissionDenied)
            },
            other => panic!("unexpected response: {other:?}"),
        }
        assert_eq!(tx_count.load(Ordering::Relaxed), 0);
        drop(client);
        handle.join().unwrap();
    }

    #[test]
    fn same_token_reconnect_replaces_old_session() {
        let tx_count = Arc::new(AtomicU64::new(0));
        let (sessions, tx_adapter, device_state, stats, warn_limiter) =
            connected_test_context(Arc::clone(&tx_count));

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let sessions_clone = Arc::clone(&sessions);
        let tx_adapter_clone = Arc::clone(&tx_adapter);
        let device_state_clone = Arc::clone(&device_state);
        let stats_clone = Arc::clone(&stats);
        let warn_limiter_clone = Arc::clone(&warn_limiter);

        thread::spawn(move || {
            for _ in 0..2 {
                let (stream, _) = listener.accept().unwrap();
                let writer = Arc::new(Mutex::new(ServerStream::Tcp(stream.try_clone().unwrap())));
                let sessions = Arc::clone(&sessions_clone);
                let tx_adapter = Arc::clone(&tx_adapter_clone);
                let device_state = Arc::clone(&device_state_clone);
                let stats = Arc::clone(&stats_clone);
                let warn_limiter = Arc::clone(&warn_limiter_clone);
                thread::spawn(move || {
                    Daemon::handle_connection(
                        ServerStream::Tcp(stream),
                        writer,
                        ConnectionContext {
                            sessions: &sessions,
                            tx_adapter: &tx_adapter,
                            device_state: &device_state,
                            stats: &stats,
                            warn_limiter: &warn_limiter,
                        },
                    );
                });
            }
        });

        let mut client_a = piper_can::gs_usb_bridge::GsUsbBridgeClient::connect(
            piper_can::gs_usb_bridge::BridgeEndpoint::Tcp(addr),
            piper_can::gs_usb_bridge::BridgeClientOptions {
                session_token: SessionToken::new([9; 16]),
                role_request: BridgeRole::Observer,
                filters: vec![],
                connect_timeout: Duration::from_secs(1),
                request_timeout: Duration::from_secs(1),
            },
        )
        .unwrap();
        let first_session = client_a.session_id();

        let client_b = piper_can::gs_usb_bridge::GsUsbBridgeClient::connect(
            piper_can::gs_usb_bridge::BridgeEndpoint::Tcp(addr),
            piper_can::gs_usb_bridge::BridgeClientOptions {
                session_token: SessionToken::new([9; 16]),
                role_request: BridgeRole::Observer,
                filters: vec![],
                connect_timeout: Duration::from_secs(1),
                request_timeout: Duration::from_secs(1),
            },
        )
        .unwrap();
        let second_session = client_b.session_id();
        assert_ne!(first_session, second_session);
        assert_eq!(
            client_a.recv_event(Duration::from_secs(1)).unwrap(),
            BridgeEvent::SessionReplaced
        );
    }
}
