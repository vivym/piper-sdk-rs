//! GS-USB UDP/UDS 适配器
//!
//! 通过守护进程访问 GS-USB 设备的客户端库
//!
//! 参考：`daemon_implementation_plan.md` 第 4.2 节

pub mod protocol;

use crate::can::{CanAdapter, CanError, PiperFrame};
use protocol::{CanIdFilter, Message};
use std::collections::VecDeque;
use std::net::SocketAddr;
use std::os::unix::net::UnixDatagram;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// GS-USB UDP/UDS 适配器
///
/// 通过守护进程访问 GS-USB 设备，支持 UDS（Unix Domain Socket）和 UDP 两种传输方式
pub struct GsUsbUdpAdapter {
    /// 客户端 ID（由守护进程分配）
    client_id: u32,

    /// 守护进程地址（UDS 路径或 UDP 地址）
    daemon_addr: DaemonAddr,

    /// Socket（UDS 或 UDP）
    socket: Socket,

    /// 接收缓冲区（用于缓存接收到的 CAN 帧）
    rx_buffer: VecDeque<PiperFrame>,

    /// 序列号（用于 SendFrame 消息）
    seq_counter: Arc<AtomicU32>,

    /// 心跳线程停止标志
    heartbeat_stop: Arc<AtomicBool>,

    /// 心跳线程句柄
    _heartbeat_handle: Option<thread::JoinHandle<()>>,

    /// 是否已连接
    connected: bool,
}

/// 守护进程地址（支持 UDS 和 UDP）
#[derive(Debug, Clone)]
enum DaemonAddr {
    Unix(String),    // UDS 路径
    Udp(SocketAddr), // UDP 地址
}

/// Socket（支持 UDS 和 UDP）
enum Socket {
    Unix(UnixDatagram),
    Udp(std::net::UdpSocket),
}

impl GsUsbUdpAdapter {
    /// 创建新的适配器（UDS）
    ///
    /// # 参数
    /// - `uds_path`: UDS Socket 路径（如 "/tmp/gs_usb_daemon.sock"）
    ///
    /// # 返回
    /// - `Ok(Self)`: 成功创建适配器
    /// - `Err`: Socket 创建失败
    pub fn new_uds(uds_path: impl AsRef<str>) -> Result<Self, CanError> {
        // 创建临时路径用于客户端 socket（守护进程需要知道这个路径才能发送数据）
        // 使用进程ID、时间戳和计数器确保唯一性，避免测试并行运行时的竞争条件
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
        let timestamp =
            SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos() as u64;
        let thread_id = format!("{:?}", thread::current().id())
            .replace("ThreadId(", "")
            .replace(")", "");
        let client_socket_path = format!(
            "/tmp/gs_usb_client_{}_{}_{}_{}.sock",
            std::process::id(),
            timestamp,
            counter,
            thread_id
        );

        // 如果临时文件已存在，先删除它（可能是上次异常退出留下的）
        if std::path::Path::new(&client_socket_path).exists() {
            let _ = std::fs::remove_file(&client_socket_path);
        }

        // 绑定到临时路径（这样守护进程才能通过路径发送数据）
        let socket = UnixDatagram::bind(&client_socket_path).map_err(CanError::Io)?;

        // 设置接收超时（用于非阻塞接收，避免在等待 ConnectAck 时无限阻塞）
        // 注意：UnixDatagram 不支持 set_read_timeout，我们需要使用非阻塞模式或轮询
        // 这里我们使用非阻塞模式，然后在接收循环中处理
        socket.set_nonblocking(true).map_err(CanError::Io)?;

        Ok(Self {
            client_id: 0, // 将在连接时分配
            daemon_addr: DaemonAddr::Unix(uds_path.as_ref().to_string()),
            socket: Socket::Unix(socket),
            rx_buffer: VecDeque::new(),
            seq_counter: Arc::new(AtomicU32::new(0)),
            heartbeat_stop: Arc::new(AtomicBool::new(false)),
            _heartbeat_handle: None,
            connected: false,
        })
    }

    /// 创建新的适配器（UDP）
    ///
    /// # 参数
    /// - `udp_addr`: UDP 地址（如 "127.0.0.1:8888"）
    ///
    /// # 返回
    /// - `Ok(Self)`: 成功创建适配器
    /// - `Err`: Socket 创建失败
    pub fn new_udp(udp_addr: impl AsRef<str>) -> Result<Self, CanError> {
        let addr: SocketAddr = udp_addr
            .as_ref()
            .parse()
            .map_err(|e| CanError::Device(format!("Invalid UDP address: {}", e).into()))?;

        let socket = std::net::UdpSocket::bind("0.0.0.0:0").map_err(CanError::Io)?;

        // 设置接收超时（用于阻塞接收）
        socket
            .set_read_timeout(Some(Duration::from_millis(100)))
            .map_err(CanError::Io)?;

        Ok(Self {
            client_id: 0, // 将在连接时分配
            daemon_addr: DaemonAddr::Udp(addr),
            socket: Socket::Udp(socket),
            rx_buffer: VecDeque::new(),
            seq_counter: Arc::new(AtomicU32::new(0)),
            heartbeat_stop: Arc::new(AtomicBool::new(false)),
            _heartbeat_handle: None,
            connected: false,
        })
    }

    /// 连接到守护进程
    ///
    /// # 参数
    /// - `filters`: CAN ID 过滤规则（可选）
    ///
    /// # 返回
    /// - `Ok(())`: 连接成功
    /// - `Err`: 连接失败
    pub fn connect(&mut self, filters: Vec<CanIdFilter>) -> Result<(), CanError> {
        // 如果已经连接，先断开
        if self.connected {
            let _ = self.disconnect();
        }

        // 统一使用自动 ID 分配（client_id = 0 表示自动分配）
        // 这样无论 UDS 还是 UDP 都使用相同策略，避免冲突
        let request_client_id = 0u32;

        // 编码 Connect 消息
        let mut buf = [0u8; 256];
        let encoded = protocol::encode_connect(
            request_client_id,
            &filters,
            0, // seq = 0 for connect
            &mut buf,
        )
        .map_err(|e| CanError::Device(format!("Failed to encode connect: {:?}", e).into()))?;

        // 发送 Connect 消息
        self.send_to_daemon(encoded)?;

        // 等待 ConnectAck（带超时）
        let mut ack_buf = [0u8; 1024];
        let start_time = std::time::Instant::now();
        let timeout = Duration::from_secs(5);
        let poll_interval = Duration::from_millis(10); // 轮询间隔

        loop {
            if start_time.elapsed() > timeout {
                return Err(CanError::Device("Connection timeout".into()));
            }

            // 尝试接收消息（非阻塞，使用轮询）
            // 关键：持续接收并清空缓冲区，防止被 CAN 帧填满
            match self.recv_from_daemon(&mut ack_buf) {
                Ok(len) => {
                    // 解析消息
                    if let Ok(msg) = protocol::decode_message(&ack_buf[..len]) {
                        match msg {
                            Message::ConnectAck {
                                client_id, // 守护进程分配的 ID
                                status,
                            } => {
                                if status == 0 {
                                    // 连接成功，保存守护进程分配的 ID
                                    self.client_id = client_id;
                                    self.connected = true;
                                    // 启动心跳线程
                                    self.start_heartbeat_thread();
                                    return Ok(());
                                } else {
                                    return Err(CanError::Device(
                                        format!("Connect failed with status: {}", status).into(),
                                    ));
                                }
                            },
                            Message::Error { code, message } => {
                                return Err(CanError::Device(
                                    format!("Connect error {:?}: {}", code, message).into(),
                                ));
                            },
                            _ => {
                                // 收到非 ConnectAck 消息（可能是 CAN 帧）
                                // 继续接收以清空缓冲区，防止缓冲区被填满
                                // 不休眠，立即继续接收
                                continue;
                            },
                        }
                    } else {
                        // 解析失败，继续接收
                        continue;
                    }
                },
                Err(CanError::Timeout) => {
                    // 非阻塞模式下，没有数据时返回 Timeout
                    // 短暂休眠后继续轮询（避免 CPU 占用过高）
                    thread::sleep(poll_interval);
                    continue;
                },
                Err(_e) => {
                    return Err(_e);
                },
            }
        }
    }

    /// 断开连接
    fn disconnect(&mut self) -> Result<(), CanError> {
        if !self.connected {
            return Ok(());
        }

        // 停止心跳线程
        self.heartbeat_stop.store(true, Ordering::Relaxed);

        // 发送断开连接消息
        let mut buf = [0u8; 12];
        let encoded = protocol::encode_disconnect(self.client_id, 0, &mut buf);
        let _ = self.send_to_daemon(encoded);

        self.connected = false;
        Ok(())
    }

    /// 检查连接状态
    pub fn is_connected(&self) -> bool {
        self.connected
    }

    /// 重连（自动重试）
    ///
    /// # 参数
    /// - `filters`: CAN ID 过滤规则
    /// - `max_retries`: 最大重试次数
    /// - `retry_interval`: 重试间隔
    ///
    /// # 返回
    /// - `Ok(())`: 重连成功
    /// - `Err`: 重连失败（达到最大重试次数）
    pub fn reconnect(
        &mut self,
        filters: Vec<CanIdFilter>,
        max_retries: u32,
        retry_interval: Duration,
    ) -> Result<(), CanError> {
        for attempt in 0..max_retries {
            match self.connect(filters.clone()) {
                Ok(_) => return Ok(()),
                Err(e) => {
                    if attempt < max_retries - 1 {
                        eprintln!(
                            "Reconnect attempt {} failed: {}. Retrying in {:?}...",
                            attempt + 1,
                            e,
                            retry_interval
                        );
                        thread::sleep(retry_interval);
                    } else {
                        return Err(e);
                    }
                },
            }
        }
        Err(CanError::Device(
            "Reconnect failed after max retries".into(),
        ))
    }

    /// 发送数据到守护进程
    fn send_to_daemon(&self, data: &[u8]) -> Result<(), CanError> {
        match (&self.socket, &self.daemon_addr) {
            (Socket::Unix(socket), DaemonAddr::Unix(path)) => {
                socket.send_to(data, path).map_err(CanError::Io)?;
            },
            (Socket::Udp(socket), DaemonAddr::Udp(addr)) => {
                socket.send_to(data, *addr).map_err(CanError::Io)?;
            },
            _ => {
                return Err(CanError::Device("Socket and address type mismatch".into()));
            },
        }
        Ok(())
    }

    /// 从守护进程接收数据
    fn recv_from_daemon(&self, buf: &mut [u8]) -> Result<usize, CanError> {
        match &self.socket {
            Socket::Unix(socket) => {
                // Unix Domain Socket 使用非阻塞模式
                match socket.recv(buf) {
                    Ok(len) => Ok(len),
                    Err(e) => {
                        if e.kind() == std::io::ErrorKind::WouldBlock {
                            // 非阻塞模式下，没有数据时返回 WouldBlock，转换为 Timeout
                            Err(CanError::Timeout)
                        } else {
                            Err(CanError::Io(e))
                        }
                    },
                }
            },
            Socket::Udp(socket) => socket.recv(buf).map_err(|e| {
                if e.kind() == std::io::ErrorKind::TimedOut {
                    CanError::Timeout
                } else {
                    CanError::Io(e)
                }
            }),
        }
    }

    /// 启动心跳线程
    fn start_heartbeat_thread(&mut self) {
        let client_id = self.client_id;
        let daemon_addr = self.daemon_addr.clone();
        let socket = self.clone_socket();
        let stop_flag = Arc::clone(&self.heartbeat_stop);

        let handle = thread::Builder::new()
            .name("heartbeat".into())
            .spawn(move || {
                let mut buf = [0u8; 12];
                loop {
                    if stop_flag.load(Ordering::Relaxed) {
                        break;
                    }

                    // 编码心跳消息
                    let encoded = protocol::encode_heartbeat(client_id, 0, &mut buf);

                    // 发送心跳
                    match (&socket, &daemon_addr) {
                        (Socket::Unix(s), DaemonAddr::Unix(path)) => {
                            let _ = s.send_to(encoded, path);
                        },
                        (Socket::Udp(s), DaemonAddr::Udp(addr)) => {
                            let _ = s.send_to(encoded, *addr);
                        },
                        _ => break,
                    }

                    // 每 5 秒发送一次心跳，但要支持快速退出：
                    // Drop() 会 join() 该线程；如果这里直接 sleep(5s)，退出时会“卡住”最多 5 秒。
                    // 这里使用分段睡眠（总计 5s），每 100ms 检查一次 stop_flag，保证快速退出。
                    for _ in 0..50 {
                        if stop_flag.load(Ordering::Relaxed) {
                            return;
                        }
                        thread::sleep(Duration::from_millis(100));
                    }
                }
            })
            .ok();

        self._heartbeat_handle = handle;
    }

    /// 克隆 Socket（用于心跳线程）
    fn clone_socket(&self) -> Socket {
        match &self.socket {
            Socket::Unix(s) => Socket::Unix(s.try_clone().expect("Failed to clone Unix socket")),
            Socket::Udp(s) => Socket::Udp(s.try_clone().expect("Failed to clone UDP socket")),
        }
    }
}

impl CanAdapter for GsUsbUdpAdapter {
    /// 发送 CAN 帧
    fn send(&mut self, frame: PiperFrame) -> Result<(), CanError> {
        if !self.connected {
            return Err(CanError::Device("Not connected to daemon".into()));
        }

        // 获取并递增序列号
        let seq = self.seq_counter.fetch_add(1, Ordering::Relaxed);

        // 编码 SendFrame 消息
        let mut buf = [0u8; 64];
        let encoded = protocol::encode_send_frame_with_seq(&frame, seq, &mut buf).map_err(|e| {
            CanError::Device(format!("Failed to encode send frame: {:?}", e).into())
        })?;

        // 发送到守护进程（带重试）
        match self.send_to_daemon(encoded) {
            Ok(_) => Ok(()),
            Err(e) => {
                // 发送失败，标记为未连接
                self.connected = false;
                Err(e)
            },
        }
    }

    /// 接收 CAN 帧
    fn receive(&mut self) -> Result<PiperFrame, CanError> {
        if !self.connected {
            return Err(CanError::Device("Not connected to daemon".into()));
        }

        // 1. 先检查缓冲区
        if let Some(frame) = self.rx_buffer.pop_front() {
            return Ok(frame);
        }

        // 2. 批量接收：尽可能多地接收数据，填充缓冲区
        // 这样可以处理高吞吐量场景（守护进程发送速度很快）
        let mut buf = [0u8; 1024];
        let mut received_any = false;

        // 批量接收，最多接收 100 个消息（避免无限循环）
        for _ in 0..100 {
            let len = match self.recv_from_daemon(&mut buf) {
                Ok(len) => len,
                Err(CanError::Timeout) => {
                    // 如果没有收到任何数据，返回超时
                    if !received_any {
                        return Err(CanError::Timeout);
                    }
                    // 如果已经收到了一些数据，从缓冲区返回第一个
                    break;
                },
                Err(e) => {
                    // 接收失败，标记为未连接
                    tracing::warn!("[GsUsbUdpAdapter] recv_from_daemon error: {:?}", e);
                    self.connected = false;
                    return Err(e);
                },
            };

            received_any = true;

            // 解析消息
            let msg = match protocol::decode_message(&buf[..len]) {
                Ok(msg) => msg,
                Err(_e) => {
                    // 解析失败，继续接收下一个消息
                    continue;
                },
            };

            match msg {
                Message::ReceiveFrame(frame) => {
                    // 将 CAN 帧放入缓冲区
                    self.rx_buffer.push_back(frame);
                },
                Message::Error { code, message } => {
                    tracing::warn!(
                        "[GsUsbUdpAdapter] Received Error message: {:?}, {}",
                        code,
                        message
                    );
                    // 错误消息：如果缓冲区为空，直接返回错误
                    // 否则，先返回缓冲区中的帧，下次再返回错误
                    // 注意：由于函数立即返回，不需要增加 other_message_count
                    if self.rx_buffer.is_empty() {
                        return Err(CanError::Device(
                            format!("Error {:?}: {}", code, message).into(),
                        ));
                    } else {
                        // 将错误消息也放入缓冲区（作为特殊标记）
                        // 这里简化处理，直接返回错误
                        return Err(CanError::Device(
                            format!("Error {:?}: {}", code, message).into(),
                        ));
                    }
                },
                Message::SendAck { seq: _, status } => {
                    // 发送确认（可选处理）
                    if status != 0 {
                        // 发送失败，但继续接收
                        continue;
                    }
                    continue;
                },
                Message::ConnectAck {
                    client_id: _,
                    status: _,
                } => {
                    // 连接确认（不应该在连接后收到，但忽略）
                    continue;
                },
                Message::Heartbeat { client_id: _ } => {
                    // 心跳（不应该从守护进程收到，但忽略）
                    continue;
                },
                _ => {
                    // 忽略其他消息类型，继续接收
                    continue;
                },
            }
        }

        // 3. 从缓冲区返回第一个帧
        if let Some(frame) = self.rx_buffer.pop_front() {
            Ok(frame)
        } else {
            // 缓冲区为空，返回超时
            Err(CanError::Timeout)
        }
    }
}

impl Drop for GsUsbUdpAdapter {
    fn drop(&mut self) {
        // 停止心跳线程
        self.heartbeat_stop.store(true, Ordering::Relaxed);

        // 等待心跳线程退出
        if let Some(handle) = self._heartbeat_handle.take() {
            let _ = handle.join();
        }

        // 发送断开连接消息
        if self.connected {
            let mut buf = [0u8; 12];
            let encoded = protocol::encode_disconnect(self.client_id, 0, &mut buf);
            let _ = self.send_to_daemon(encoded);
        }

        // 清理客户端 socket 文件（如果使用 UDS）
        if let Socket::Unix(_) = &self.socket {
            let client_socket_path = format!("/tmp/gs_usb_client_{}.sock", std::process::id());
            if std::path::Path::new(&client_socket_path).exists() {
                let _ = std::fs::remove_file(&client_socket_path);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_gs_usb_udp_adapter_new_uds() {
        let adapter = GsUsbUdpAdapter::new_uds("/tmp/test_gs_usb_daemon.sock");
        assert!(adapter.is_ok());
    }

    #[test]
    fn test_gs_usb_udp_adapter_new_udp() {
        let adapter = GsUsbUdpAdapter::new_udp("127.0.0.1:8888");
        assert!(adapter.is_ok());
    }

    #[test]
    fn test_gs_usb_udp_adapter_invalid_udp() {
        let adapter = GsUsbUdpAdapter::new_udp("invalid");
        assert!(adapter.is_err());
    }

    #[test]
    fn test_gs_usb_udp_adapter_connection_state() {
        let mut adapter = GsUsbUdpAdapter::new_uds("/tmp/test_gs_usb_daemon.sock").unwrap();
        assert!(!adapter.is_connected());

        // 尝试连接（会失败，因为没有守护进程，但可以测试状态）
        let result = adapter.connect(vec![]);
        // 连接会超时，但状态管理应该正确
        assert!(result.is_err());
        assert!(!adapter.is_connected());
    }

    #[test]
    fn test_gs_usb_udp_adapter_send_not_connected() {
        let mut adapter = GsUsbUdpAdapter::new_uds("/tmp/test_gs_usb_daemon.sock").unwrap();
        let frame = PiperFrame::new_standard(0x123, &[0x01, 0x02, 0x03]);

        // 未连接时发送应该失败
        let result = adapter.send(frame);
        assert!(result.is_err());
    }

    #[test]
    fn test_gs_usb_udp_adapter_receive_not_connected() {
        let mut adapter = GsUsbUdpAdapter::new_uds("/tmp/test_gs_usb_daemon.sock").unwrap();

        // 未连接时接收应该失败
        let result = adapter.receive();
        assert!(result.is_err());
    }

    #[test]
    fn test_gs_usb_udp_adapter_sequence_number() {
        let mut adapter = GsUsbUdpAdapter::new_uds("/tmp/test_gs_usb_daemon.sock").unwrap();

        // 序列号应该从 0 开始
        let seq1 = adapter.seq_counter.load(Ordering::Relaxed);
        assert_eq!(seq1, 0);

        // 注意：send() 在未连接时会提前返回，不会递增序列号
        // 这是正确的行为，因为未连接时不应该发送
        let frame = PiperFrame::new_standard(0x123, &[0x01]);
        let result = adapter.send(frame);
        assert!(result.is_err()); // 应该失败（未连接）

        // 序列号应该仍然是 0（因为提前返回）
        let seq2 = adapter.seq_counter.load(Ordering::Relaxed);
        assert_eq!(seq2, 0);
    }

    #[test]
    fn test_gs_usb_udp_adapter_reconnect_logic() {
        let mut adapter = GsUsbUdpAdapter::new_uds("/tmp/test_gs_usb_daemon.sock").unwrap();

        // 重连应该失败（没有守护进程），但逻辑应该正确执行
        let result = adapter.reconnect(vec![], 3, Duration::from_millis(10));
        assert!(result.is_err());
    }

    #[test]
    fn test_gs_usb_udp_adapter_disconnect() {
        let mut adapter = GsUsbUdpAdapter::new_uds("/tmp/test_gs_usb_daemon.sock").unwrap();

        // 未连接时断开应该成功（无操作）
        let result = adapter.disconnect();
        assert!(result.is_ok());
        assert!(!adapter.is_connected());
    }
}
