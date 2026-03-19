//! GS-USB UDP/UDS 适配器
//!
//! 通过守护进程访问 GS-USB 设备的客户端库。

pub mod protocol;

use crate::{CanAdapter, CanError, PiperFrame, RxAdapter, SplittableAdapter, TxAdapter};
use protocol::{CanIdFilter, Message};
use std::collections::VecDeque;
use std::net::{SocketAddr, UdpSocket};
#[cfg(unix)]
use std::os::unix::net::UnixDatagram;
#[cfg(unix)]
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
#[cfg(unix)]
use std::time::{SystemTime, UNIX_EPOCH};

/// GS-USB UDP/UDS 适配器
///
/// 通过守护进程访问 GS-USB 设备，支持 UDS（Unix Domain Socket）和 UDP 两种传输方式。
pub struct GsUsbUdpAdapter {
    session: Arc<DaemonSession>,
    rx_buffer: VecDeque<PiperFrame>,
    receive_timeout: Duration,
}

/// daemon 会话的共享状态。
struct DaemonSession {
    client_id: AtomicU32,
    daemon_addr: DaemonAddr,
    socket: Arc<Socket>,
    seq_counter: AtomicU32,
    heartbeat_stop: Arc<AtomicBool>,
    heartbeat_handle: Mutex<Option<thread::JoinHandle<()>>>,
    connected: AtomicBool,
    #[cfg(unix)]
    client_socket_path: Option<PathBuf>,
}

/// 守护进程地址（支持 UDS 和 UDP）
#[derive(Debug, Clone)]
enum DaemonAddr {
    #[cfg(unix)]
    Unix(String),
    Udp(SocketAddr),
}

/// Socket（支持 UDS 和 UDP）
enum Socket {
    #[cfg(unix)]
    Unix(UnixDatagram),
    Udp(UdpSocket),
}

#[doc(hidden)]
pub struct GsUsbUdpRxAdapter {
    session: Arc<DaemonSession>,
    rx_buffer: VecDeque<PiperFrame>,
}

#[doc(hidden)]
pub struct GsUsbUdpTxAdapter {
    session: Arc<DaemonSession>,
}

impl Socket {
    fn send_to_daemon(&self, data: &[u8], daemon_addr: &DaemonAddr) -> Result<(), CanError> {
        match (self, daemon_addr) {
            #[cfg(unix)]
            (Socket::Unix(socket), DaemonAddr::Unix(path)) => {
                socket.send_to(data, path).map_err(CanError::Io)?;
            },
            (Socket::Udp(socket), DaemonAddr::Udp(addr)) => {
                socket.send_to(data, *addr).map_err(CanError::Io)?;
            },
            #[cfg(unix)]
            _ => {
                return Err(CanError::Device("Socket and address type mismatch".into()));
            },
        }

        Ok(())
    }

    fn recv_from_daemon(&self, buf: &mut [u8]) -> Result<usize, CanError> {
        match self {
            #[cfg(unix)]
            Socket::Unix(socket) => socket.recv(buf).map_err(|err| {
                if matches!(
                    err.kind(),
                    std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                ) {
                    CanError::Timeout
                } else {
                    CanError::Io(err)
                }
            }),
            Socket::Udp(socket) => socket.recv(buf).map_err(|err| {
                if matches!(
                    err.kind(),
                    std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                ) {
                    CanError::Timeout
                } else {
                    CanError::Io(err)
                }
            }),
        }
    }

    fn set_read_timeout(&self, timeout: Option<Duration>) -> Result<(), CanError> {
        match self {
            #[cfg(unix)]
            Socket::Unix(socket) => socket.set_read_timeout(timeout).map_err(CanError::Io),
            Socket::Udp(socket) => socket.set_read_timeout(timeout).map_err(CanError::Io),
        }
    }
}

impl DaemonSession {
    fn new(
        daemon_addr: DaemonAddr,
        socket: Arc<Socket>,
        #[cfg(unix)] client_socket_path: Option<PathBuf>,
    ) -> Self {
        Self {
            client_id: AtomicU32::new(0),
            daemon_addr,
            socket,
            seq_counter: AtomicU32::new(0),
            heartbeat_stop: Arc::new(AtomicBool::new(false)),
            heartbeat_handle: Mutex::new(None),
            connected: AtomicBool::new(false),
            #[cfg(unix)]
            client_socket_path,
        }
    }

    fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Acquire)
    }

    fn set_receive_timeout(&self, timeout: Duration) -> Result<(), CanError> {
        self.socket.set_read_timeout(Some(timeout))
    }

    fn send_to_daemon(&self, data: &[u8]) -> Result<(), CanError> {
        self.socket.send_to_daemon(data, &self.daemon_addr)
    }

    fn recv_from_daemon(&self, buf: &mut [u8]) -> Result<usize, CanError> {
        self.socket.recv_from_daemon(buf)
    }

    fn mark_transport_lost(&self) {
        self.connected.store(false, Ordering::Release);
        self.stop_heartbeat();
    }

    fn start_heartbeat(&self) -> Result<(), CanError> {
        self.stop_heartbeat();
        self.heartbeat_stop.store(false, Ordering::Release);

        let client_id = self.client_id.load(Ordering::Acquire);
        let daemon_addr = self.daemon_addr.clone();
        let socket = Arc::clone(&self.socket);
        let stop_flag = Arc::clone(&self.heartbeat_stop);

        let handle = thread::Builder::new()
            .name("heartbeat".into())
            .spawn(move || {
                let mut buf = [0u8; 12];
                loop {
                    if stop_flag.load(Ordering::Acquire) {
                        return;
                    }

                    let encoded = protocol::encode_heartbeat(client_id, 0, &mut buf);
                    match socket.send_to_daemon(encoded, &daemon_addr) {
                        Ok(()) => {},
                        Err(_) => return,
                    }

                    for _ in 0..50 {
                        if stop_flag.load(Ordering::Acquire) {
                            return;
                        }
                        thread::sleep(Duration::from_millis(100));
                    }
                }
            })
            .map_err(CanError::Io)?;

        *self.heartbeat_handle.lock().expect("heartbeat lock poisoned") = Some(handle);
        Ok(())
    }

    fn stop_heartbeat(&self) {
        self.heartbeat_stop.store(true, Ordering::Release);
        if let Some(handle) = self.heartbeat_handle.lock().expect("heartbeat lock poisoned").take()
        {
            let _ = handle.join();
        }
    }

    fn disconnect(&self) -> Result<(), CanError> {
        self.stop_heartbeat();

        if !self.connected.swap(false, Ordering::AcqRel) {
            return Ok(());
        }

        let mut buf = [0u8; 12];
        let encoded =
            protocol::encode_disconnect(self.client_id.load(Ordering::Acquire), 0, &mut buf);
        self.send_to_daemon(encoded)
    }
}

impl Drop for DaemonSession {
    fn drop(&mut self) {
        let _ = self.disconnect();

        #[cfg(unix)]
        if let Some(path) = self.client_socket_path.as_ref()
            && path.exists()
        {
            let _ = std::fs::remove_file(path);
        }
    }
}

#[cfg(unix)]
fn unique_client_socket_path() -> PathBuf {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    let timestamp =
        SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos() as u64;
    let thread_id = format!("{:?}", thread::current().id())
        .replace("ThreadId(", "")
        .replace(")", "");

    PathBuf::from(format!(
        "/tmp/gs_usb_client_{}_{}_{}_{}.sock",
        std::process::id(),
        timestamp,
        counter,
        thread_id
    ))
}

fn send_frame(session: &DaemonSession, frame: PiperFrame) -> Result<(), CanError> {
    if !session.is_connected() {
        return Err(CanError::Device("Not connected to daemon".into()));
    }

    let seq = session.seq_counter.fetch_add(1, Ordering::Relaxed);
    let mut buf = [0u8; 64];
    let encoded = protocol::encode_send_frame_with_seq(&frame, seq, &mut buf)
        .map_err(|err| CanError::Device(format!("Failed to encode send frame: {err:?}").into()))?;

    if let Err(error) = session.send_to_daemon(encoded) {
        session.mark_transport_lost();
        return Err(error);
    }

    Ok(())
}

fn receive_frame(
    session: &DaemonSession,
    rx_buffer: &mut VecDeque<PiperFrame>,
) -> Result<PiperFrame, CanError> {
    if !session.is_connected() {
        return Err(CanError::Device("Not connected to daemon".into()));
    }

    if let Some(frame) = rx_buffer.pop_front() {
        return Ok(frame);
    }

    let mut buf = [0u8; 1024];
    let mut received_any = false;

    for _ in 0..100 {
        let len = match session.recv_from_daemon(&mut buf) {
            Ok(len) => len,
            Err(CanError::Timeout) => {
                if !received_any {
                    return Err(CanError::Timeout);
                }
                break;
            },
            Err(error) => {
                tracing::warn!("[GsUsbUdpAdapter] recv_from_daemon error: {:?}", error);
                session.mark_transport_lost();
                return Err(error);
            },
        };

        received_any = true;

        let msg = match protocol::decode_message(&buf[..len]) {
            Ok(msg) => msg,
            Err(_) => continue,
        };

        match msg {
            Message::ReceiveFrame(frame) => rx_buffer.push_back(frame),
            Message::Error { code, message } => {
                return Err(CanError::Device(
                    format!("Error {:?}: {}", code, message).into(),
                ));
            },
            Message::SendAck { status, .. } => {
                if status != 0 {
                    return Err(CanError::Device(
                        format!("Send failed with status: {status}").into(),
                    ));
                }
            },
            Message::ConnectAck { .. } | Message::Heartbeat { .. } => {},
            _ => {},
        }
    }

    rx_buffer.pop_front().ok_or(CanError::Timeout)
}

impl GsUsbUdpAdapter {
    /// 创建新的适配器（UDS）
    #[cfg(unix)]
    pub fn new_uds(uds_path: impl AsRef<str>) -> Result<Self, CanError> {
        let receive_timeout = Duration::from_millis(2);
        let client_socket_path = unique_client_socket_path();

        if client_socket_path.exists() {
            let _ = std::fs::remove_file(&client_socket_path);
        }

        let socket = Arc::new(Socket::Unix(
            UnixDatagram::bind(&client_socket_path).map_err(CanError::Io)?,
        ));
        socket.set_read_timeout(Some(receive_timeout))?;

        let session = Arc::new(DaemonSession::new(
            DaemonAddr::Unix(uds_path.as_ref().to_string()),
            socket,
            Some(client_socket_path),
        ));

        Ok(Self {
            session,
            rx_buffer: VecDeque::new(),
            receive_timeout,
        })
    }

    /// 创建新的适配器（UDP）
    pub fn new_udp(udp_addr: impl AsRef<str>) -> Result<Self, CanError> {
        let receive_timeout = Duration::from_millis(2);
        let addr: SocketAddr = udp_addr
            .as_ref()
            .parse()
            .map_err(|err| CanError::Device(format!("Invalid UDP address: {}", err).into()))?;

        let socket = Arc::new(Socket::Udp(
            UdpSocket::bind("0.0.0.0:0").map_err(CanError::Io)?,
        ));
        socket.set_read_timeout(Some(receive_timeout))?;

        let session = Arc::new(DaemonSession::new(
            DaemonAddr::Udp(addr),
            socket,
            #[cfg(unix)]
            None,
        ));

        Ok(Self {
            session,
            rx_buffer: VecDeque::new(),
            receive_timeout,
        })
    }

    /// 连接到守护进程
    pub fn connect(&mut self, filters: Vec<CanIdFilter>) -> Result<(), CanError> {
        if self.session.is_connected() {
            let _ = self.session.disconnect();
        }

        self.rx_buffer.clear();

        let mut buf = [0u8; 256];
        let encoded = protocol::encode_connect(0, &filters, 0, &mut buf)
            .map_err(|err| CanError::Device(format!("Failed to encode connect: {err:?}").into()))?;

        self.session.send_to_daemon(encoded)?;

        let mut ack_buf = [0u8; 1024];
        let start_time = std::time::Instant::now();
        let timeout = Duration::from_secs(5);
        let poll_interval = Duration::from_millis(10);

        loop {
            if start_time.elapsed() > timeout {
                return Err(CanError::Device("Connection timeout".into()));
            }

            match self.session.recv_from_daemon(&mut ack_buf) {
                Ok(len) => {
                    let Ok(msg) = protocol::decode_message(&ack_buf[..len]) else {
                        continue;
                    };

                    match msg {
                        Message::ConnectAck { client_id, status } => {
                            if status != 0 {
                                return Err(CanError::Device(
                                    format!("Connect failed with status: {}", status).into(),
                                ));
                            }

                            self.session.client_id.store(client_id, Ordering::Release);
                            self.session.connected.store(true, Ordering::Release);
                            self.session.start_heartbeat()?;
                            return Ok(());
                        },
                        Message::Error { code, message } => {
                            return Err(CanError::Device(
                                format!("Connect error {:?}: {}", code, message).into(),
                            ));
                        },
                        _ => continue,
                    }
                },
                Err(CanError::Timeout) => {
                    thread::sleep(poll_interval);
                },
                Err(error) => return Err(error),
            }
        }
    }

    /// 断开连接
    pub fn disconnect(&mut self) -> Result<(), CanError> {
        self.session.disconnect()
    }

    /// 检查连接状态
    pub fn is_connected(&self) -> bool {
        self.session.is_connected()
    }

    /// 重连（自动重试）
    pub fn reconnect(
        &mut self,
        filters: Vec<CanIdFilter>,
        max_retries: u32,
        retry_interval: Duration,
    ) -> Result<(), CanError> {
        for attempt in 0..max_retries {
            match self.connect(filters.clone()) {
                Ok(()) => return Ok(()),
                Err(error) => {
                    if attempt < max_retries - 1 {
                        eprintln!(
                            "Reconnect attempt {} failed: {}. Retrying in {:?}...",
                            attempt + 1,
                            error,
                            retry_interval
                        );
                        thread::sleep(retry_interval);
                    } else {
                        return Err(error);
                    }
                },
            }
        }

        Err(CanError::Device(
            "Reconnect failed after max retries".into(),
        ))
    }
}

impl CanAdapter for GsUsbUdpAdapter {
    fn send(&mut self, frame: PiperFrame) -> Result<(), CanError> {
        send_frame(&self.session, frame)
    }

    fn receive(&mut self) -> Result<PiperFrame, CanError> {
        receive_frame(&self.session, &mut self.rx_buffer)
    }

    fn set_receive_timeout(&mut self, timeout: Duration) {
        self.receive_timeout = timeout;
        let _ = self.session.set_receive_timeout(timeout);
    }
}

impl RxAdapter for GsUsbUdpRxAdapter {
    fn receive(&mut self) -> Result<PiperFrame, CanError> {
        receive_frame(&self.session, &mut self.rx_buffer)
    }
}

impl TxAdapter for GsUsbUdpTxAdapter {
    fn send_until(&mut self, frame: PiperFrame, deadline: Instant) -> Result<(), CanError> {
        if deadline <= Instant::now() {
            return Err(CanError::Timeout);
        }
        send_frame(&self.session, frame)
    }
}

impl SplittableAdapter for GsUsbUdpAdapter {
    type RxAdapter = GsUsbUdpRxAdapter;
    type TxAdapter = GsUsbUdpTxAdapter;

    fn split(self) -> Result<(Self::RxAdapter, Self::TxAdapter), CanError> {
        Ok((
            GsUsbUdpRxAdapter {
                session: Arc::clone(&self.session),
                rx_buffer: self.rx_buffer,
            },
            GsUsbUdpTxAdapter {
                session: self.session,
            },
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    fn adapter_or_skip(
        result: Result<GsUsbUdpAdapter, CanError>,
        transport: &str,
    ) -> Option<GsUsbUdpAdapter> {
        match result {
            Ok(adapter) => Some(adapter),
            Err(CanError::Io(err))
                if matches!(
                    err.kind(),
                    std::io::ErrorKind::PermissionDenied
                        | std::io::ErrorKind::AddrNotAvailable
                        | std::io::ErrorKind::AddrInUse
                ) =>
            {
                eprintln!("skipping {transport} socket test in restricted environment: {err}");
                None
            },
            Err(err) => panic!("unexpected {transport} socket error: {err}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn test_gs_usb_udp_adapter_new_uds() {
        let Some(_adapter) = adapter_or_skip(
            GsUsbUdpAdapter::new_uds("/tmp/test_gs_usb_daemon.sock"),
            "uds",
        ) else {
            return;
        };
    }

    #[test]
    fn test_gs_usb_udp_adapter_new_udp() {
        let Some(_adapter) = adapter_or_skip(GsUsbUdpAdapter::new_udp("127.0.0.1:8888"), "udp")
        else {
            return;
        };
    }

    #[test]
    fn test_gs_usb_udp_adapter_invalid_udp() {
        assert!(GsUsbUdpAdapter::new_udp("invalid").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn test_gs_usb_udp_adapter_connection_state() {
        let Some(mut adapter) = adapter_or_skip(
            GsUsbUdpAdapter::new_uds("/tmp/test_gs_usb_daemon.sock"),
            "uds",
        ) else {
            return;
        };

        assert!(!adapter.is_connected());
        assert!(adapter.connect(vec![]).is_err());
        assert!(!adapter.is_connected());
    }

    #[cfg(unix)]
    #[test]
    fn test_gs_usb_udp_adapter_send_not_connected() {
        let Some(mut adapter) = adapter_or_skip(
            GsUsbUdpAdapter::new_uds("/tmp/test_gs_usb_daemon.sock"),
            "uds",
        ) else {
            return;
        };

        let frame = PiperFrame::new_standard(0x123, &[0x01, 0x02, 0x03]);
        assert!(adapter.send(frame).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn test_gs_usb_udp_adapter_receive_not_connected() {
        let Some(mut adapter) = adapter_or_skip(
            GsUsbUdpAdapter::new_uds("/tmp/test_gs_usb_daemon.sock"),
            "uds",
        ) else {
            return;
        };

        assert!(adapter.receive().is_err());
    }

    #[cfg(unix)]
    #[test]
    fn test_gs_usb_udp_adapter_sequence_number() {
        let Some(mut adapter) = adapter_or_skip(
            GsUsbUdpAdapter::new_uds("/tmp/test_gs_usb_daemon.sock"),
            "uds",
        ) else {
            return;
        };

        let seq1 = adapter.session.seq_counter.load(Ordering::Relaxed);
        assert_eq!(seq1, 0);

        let frame = PiperFrame::new_standard(0x123, &[0x01]);
        assert!(adapter.send(frame).is_err());

        let seq2 = adapter.session.seq_counter.load(Ordering::Relaxed);
        assert_eq!(seq2, 0);
    }

    #[cfg(unix)]
    #[test]
    fn test_gs_usb_udp_adapter_reconnect_logic() {
        let Some(mut adapter) = adapter_or_skip(
            GsUsbUdpAdapter::new_uds("/tmp/test_gs_usb_daemon.sock"),
            "uds",
        ) else {
            return;
        };

        assert!(adapter.reconnect(vec![], 3, Duration::from_millis(10)).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn test_gs_usb_udp_adapter_disconnect() {
        let Some(mut adapter) = adapter_or_skip(
            GsUsbUdpAdapter::new_uds("/tmp/test_gs_usb_daemon.sock"),
            "uds",
        ) else {
            return;
        };

        assert!(adapter.disconnect().is_ok());
        assert!(!adapter.is_connected());
    }

    #[cfg(unix)]
    #[test]
    fn test_gs_usb_udp_adapter_drop_removes_exact_uds_socket_path() {
        let Some(adapter) = adapter_or_skip(
            GsUsbUdpAdapter::new_uds("/tmp/test_gs_usb_daemon.sock"),
            "uds",
        ) else {
            return;
        };

        let path = adapter
            .session
            .client_socket_path
            .as_ref()
            .expect("uds path should exist")
            .clone();
        assert!(path.exists());

        drop(adapter);
        assert!(!path.exists());
    }

    #[test]
    fn test_gs_usb_udp_adapter_split_udp_roundtrip() {
        let server = UdpSocket::bind("127.0.0.1:0").unwrap();
        server.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let server_addr = server.local_addr().unwrap();
        let (ready_tx, ready_rx) = mpsc::channel();

        let server_handle = thread::spawn(move || {
            let mut buf = [0u8; 1024];
            let mut client_addr = None;

            while let Ok((len, addr)) = server.recv_from(&mut buf) {
                let msg = protocol::decode_message(&buf[..len]).unwrap();
                match msg {
                    Message::Connect { .. } => {
                        client_addr = Some(addr);
                        let mut ack_buf = [0u8; 13];
                        let encoded = protocol::encode_connect_ack(42, 0, 0, &mut ack_buf);
                        server.send_to(encoded, addr).unwrap();
                        ready_tx.send(()).unwrap();
                    },
                    Message::SendFrame { seq, frame } => {
                        let mut ack_buf = [0u8; 12];
                        let encoded = protocol::encode_send_ack(seq, 0, &mut ack_buf);
                        server.send_to(encoded, addr).unwrap();

                        if frame.id == 0x321 {
                            let mut frame_buf = [0u8; 64];
                            let encoded =
                                protocol::encode_receive_frame_zero_copy(&frame, &mut frame_buf)
                                    .unwrap();
                            server.send_to(encoded, addr).unwrap();
                            break;
                        }
                    },
                    _ => {
                        if let Some(client_addr) = client_addr {
                            let mut frame_buf = [0u8; 64];
                            let frame = PiperFrame::new_standard(0x111, &[1, 2, 3, 4]);
                            let encoded =
                                protocol::encode_receive_frame_zero_copy(&frame, &mut frame_buf)
                                    .unwrap();
                            server.send_to(encoded, client_addr).unwrap();
                        }
                    },
                }
            }
        });

        let mut adapter = GsUsbUdpAdapter::new_udp(server_addr.to_string()).unwrap();
        adapter.connect(vec![]).unwrap();
        ready_rx.recv_timeout(Duration::from_secs(1)).unwrap();

        let (mut rx, mut tx) = adapter.split().unwrap();
        let outbound = PiperFrame::new_standard(0x321, &[9, 8, 7, 6]);
        tx.send_until(outbound, Instant::now() + Duration::from_millis(50)).unwrap();

        let inbound = rx.receive().unwrap();
        assert_eq!(inbound.id, outbound.id);
        assert_eq!(
            inbound.data[..inbound.len as usize],
            outbound.data[..outbound.len as usize]
        );

        server_handle.join().unwrap();
    }
}
