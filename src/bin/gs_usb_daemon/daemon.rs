//! 守护进程核心逻辑
//!
//! 实现多线程阻塞架构、设备状态机、热拔插恢复
//!
//! 参考：`daemon_implementation_plan.md` 第 4.1.3 节

use crate::client_manager::{ClientAddr, ClientManager};
use piper_sdk::can::CanAdapter;
use piper_sdk::can::gs_usb::GsUsbCanAdapter;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::{Duration, Instant};

/// 设备状态机
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceState {
    /// 设备已连接，正常工作
    Connected,
    /// 设备断开（物理拔出或错误）
    Disconnected,
    /// 正在重连中
    Reconnecting,
}

/// 守护进程配置
#[derive(Debug, Clone)]
pub struct DaemonConfig {
    /// UDS Socket 路径（默认 /tmp/gs_usb_daemon.sock）
    pub uds_path: Option<String>,

    /// UDP 监听地址（可选，如 "127.0.0.1:8888"）
    pub udp_addr: Option<String>,

    /// CAN 波特率（默认 1000000）
    pub bitrate: u32,

    /// 设备序列号（可选，用于多设备场景）
    pub serial_number: Option<String>,

    /// 重连间隔（秒，默认 1 秒）
    pub reconnect_interval: Duration,

    /// 重连冷却时间（防止 USB 枚举抖动，默认 500ms）
    pub reconnect_debounce: Duration,

    /// 客户端超时时间（默认 30 秒）
    pub client_timeout: Duration,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            uds_path: Some("/tmp/gs_usb_daemon.sock".to_string()),
            udp_addr: None,
            bitrate: 1_000_000,
            serial_number: None,
            reconnect_interval: Duration::from_secs(1),
            reconnect_debounce: Duration::from_millis(500),
            client_timeout: Duration::from_secs(30),
        }
    }
}

/// 守护进程错误类型
#[derive(Debug, Clone)]
pub enum DaemonError {
    DeviceInit(String),
    DeviceConfig(String),
    SocketInit(String),
    Io(String),
}

impl std::fmt::Display for DaemonError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DaemonError::DeviceInit(msg) => write!(f, "Device init error: {}", msg),
            DaemonError::DeviceConfig(msg) => write!(f, "Device config error: {}", msg),
            DaemonError::SocketInit(msg) => write!(f, "Socket init error: {}", msg),
            DaemonError::Io(e) => write!(f, "IO error: {}", e),
        }
    }
}

impl std::error::Error for DaemonError {}

impl From<std::io::Error> for DaemonError {
    fn from(e: std::io::Error) -> Self {
        DaemonError::Io(e.to_string())
    }
}

/// 守护进程统计信息
#[derive(Debug)]
struct DaemonStats {
    /// 接收到的 CAN 帧总数（从 USB）
    rx_frames: AtomicU64,
    /// 发送的 CAN 帧总数（到 USB）
    tx_frames: AtomicU64,
    /// 发送到客户端的帧总数（IPC）
    ipc_sent: AtomicU64,
    /// 从客户端接收的帧总数（IPC）
    ipc_received: AtomicU64,
    /// 统计开始时间
    start_time: Instant,
}

impl DaemonStats {
    fn new() -> Self {
        Self {
            rx_frames: AtomicU64::new(0),
            tx_frames: AtomicU64::new(0),
            ipc_sent: AtomicU64::new(0),
            ipc_received: AtomicU64::new(0),
            start_time: Instant::now(),
        }
    }

    fn increment_rx(&self) {
        self.rx_frames.fetch_add(1, Ordering::Relaxed);
    }

    fn increment_tx(&self) {
        self.tx_frames.fetch_add(1, Ordering::Relaxed);
    }

    fn increment_ipc_sent(&self) {
        self.ipc_sent.fetch_add(1, Ordering::Relaxed);
    }

    fn increment_ipc_received(&self) {
        self.ipc_received.fetch_add(1, Ordering::Relaxed);
    }

    fn get_rx_fps(&self) -> f64 {
        let elapsed = self.start_time.elapsed().as_secs_f64();
        if elapsed > 0.0 {
            self.rx_frames.load(Ordering::Relaxed) as f64 / elapsed
        } else {
            0.0
        }
    }

    fn get_tx_fps(&self) -> f64 {
        let elapsed = self.start_time.elapsed().as_secs_f64();
        if elapsed > 0.0 {
            self.tx_frames.load(Ordering::Relaxed) as f64 / elapsed
        } else {
            0.0
        }
    }

    fn get_ipc_sent_fps(&self) -> f64 {
        let elapsed = self.start_time.elapsed().as_secs_f64();
        if elapsed > 0.0 {
            self.ipc_sent.load(Ordering::Relaxed) as f64 / elapsed
        } else {
            0.0
        }
    }

    fn get_ipc_received_fps(&self) -> f64 {
        let elapsed = self.start_time.elapsed().as_secs_f64();
        if elapsed > 0.0 {
            self.ipc_received.load(Ordering::Relaxed) as f64 / elapsed
        } else {
            0.0
        }
    }

    /// 重置统计信息（重置计数器和开始时间）
    fn reset(&mut self) {
        self.rx_frames.store(0, Ordering::Relaxed);
        self.tx_frames.store(0, Ordering::Relaxed);
        self.ipc_sent.store(0, Ordering::Relaxed);
        self.ipc_received.store(0, Ordering::Relaxed);
        self.start_time = Instant::now();
    }
}

/// 守护进程状态
pub struct Daemon {
    /// GS-USB 适配器（使用 RwLock 优化读取性能）
    adapter: Arc<RwLock<Option<GsUsbCanAdapter>>>,

    /// 设备状态
    device_state: Arc<RwLock<DeviceState>>,

    /// UDS Socket（Unix Domain Socket）
    socket_uds: Option<std::os::unix::net::UnixDatagram>,

    /// UDP Socket（可选，用于跨机器调试）
    socket_udp: Option<std::net::UdpSocket>,

    /// UDS Socket 路径（用于退出时清理）
    uds_path: Option<String>,

    /// 客户端管理器（使用 RwLock 优化读取性能）
    clients: Arc<RwLock<ClientManager>>,

    /// 守护进程配置
    config: DaemonConfig,

    /// 统计信息
    stats: Arc<RwLock<DaemonStats>>,
}

impl Daemon {
    /// 创建新的守护进程实例
    pub fn new(config: DaemonConfig) -> Result<Self, DaemonError> {
        Ok(Self {
            adapter: Arc::new(RwLock::new(None)),
            device_state: Arc::new(RwLock::new(DeviceState::Disconnected)),
            socket_uds: None,
            socket_udp: None,
            uds_path: config.uds_path.clone(),
            clients: Arc::new(RwLock::new(ClientManager::with_timeout(
                config.client_timeout,
            ))),
            config,
            stats: Arc::new(RwLock::new(DaemonStats::new())),
        })
    }

    /// 清理 UDS Socket 文件
    ///
    /// 在守护进程退出时调用，删除 socket 文件
    fn cleanup_uds_socket(&self) {
        if let Some(ref uds_path) = self.uds_path
            && std::path::Path::new(uds_path).exists()
        {
            if let Err(e) = std::fs::remove_file(uds_path) {
                eprintln!(
                    "Warning: Failed to remove UDS socket file {}: {}",
                    uds_path, e
                );
            } else {
                eprintln!("Cleaned up UDS socket file: {}", uds_path);
            }
        }
    }

    /// 初始化 Socket（UDS 优先，UDP 可选）
    fn init_sockets(&mut self) -> Result<(), DaemonError> {
        // 初始化 UDS Socket
        if let Some(ref uds_path) = self.config.uds_path {
            // 如果 socket 文件已存在，先删除它（可能是上次异常退出留下的）
            if std::path::Path::new(uds_path).exists() {
                if let Err(e) = std::fs::remove_file(uds_path) {
                    eprintln!(
                        "Warning: Failed to remove existing UDS socket file {}: {}",
                        uds_path, e
                    );
                    // 继续尝试绑定，如果文件被占用会失败
                } else {
                    eprintln!("Removed existing UDS socket file: {}", uds_path);
                }
            }

            let socket = std::os::unix::net::UnixDatagram::bind(uds_path).map_err(|e| {
                DaemonError::SocketInit(format!("Failed to bind UDS socket: {}", e))
            })?;

            // 设置非阻塞模式（用于后续的阻塞接收）
            // 注意：虽然我们使用阻塞 IO，但这里先设置为非阻塞，后续在接收线程中会使用阻塞模式
            self.socket_uds = Some(socket);
        }

        // 初始化 UDP Socket（可选）
        if let Some(ref udp_addr) = self.config.udp_addr {
            let socket = std::net::UdpSocket::bind(udp_addr).map_err(|e| {
                DaemonError::SocketInit(format!("Failed to bind UDP socket: {}", e))
            })?;
            self.socket_udp = Some(socket);
        }

        Ok(())
    }

    /// 尝试连接设备
    fn try_connect_device(config: &DaemonConfig) -> Result<GsUsbCanAdapter, DaemonError> {
        // 1. 扫描设备
        eprintln!(
            "[Daemon] Scanning for GS-USB devices (serial: {:?})...",
            config.serial_number
        );

        // 先扫描所有设备，打印信息
        use piper_sdk::can::gs_usb::device::GsUsbDevice;
        match GsUsbDevice::scan() {
            Ok(devices) => {
                eprintln!("[Daemon] Found {} GS-USB device(s):", devices.len());
                for (i, device) in devices.iter().enumerate() {
                    eprintln!("  [{}] Serial: {:?}", i, device.serial_number());
                }
            },
            Err(e) => {
                eprintln!("[Daemon] Warning: Failed to scan devices for info: {}", e);
            },
        }

        let mut adapter = GsUsbCanAdapter::new_with_serial(config.serial_number.as_deref())
            .map_err(|e| DaemonError::DeviceInit(format!("{}", e)))?;

        eprintln!("[Daemon] Device found and initialized successfully");

        // 2. 配置设备
        eprintln!(
            "[Daemon] Configuring device: bitrate={} bps, mode=NORMAL|HW_TIMESTAMP",
            config.bitrate
        );
        adapter
            .configure(config.bitrate)
            .map_err(|e| DaemonError::DeviceConfig(format!("{}", e)))?;

        eprintln!("[Daemon] Device configured and started successfully");
        Ok(adapter)
    }

    /// 设备管理循环（状态机 + 热拔插恢复）
    ///
    /// **关键**：无论 USB 发生什么错误，守护进程都不应退出，而是进入重连模式。
    ///
    /// **去抖动机制**：在进入 `Reconnecting` 状态前，增加冷却时间，避免 macOS USB 枚举抖动。
    fn device_manager_loop(
        adapter: Arc<RwLock<Option<GsUsbCanAdapter>>>,
        device_state: Arc<RwLock<DeviceState>>,
        config: DaemonConfig,
    ) {
        // 去抖动：记录最后一次断开时间
        let mut last_disconnect_time: Option<Instant> = None;

        loop {
            let current_state = *device_state.read().unwrap();

            match current_state {
                DeviceState::Connected => {
                    // 检查设备是否仍然可用（可选：定期健康检查）
                    // 如果检测到错误，转入 Disconnected
                    // 注意：这里可以添加定期健康检查逻辑
                    thread::sleep(Duration::from_millis(100)); // 设备管理线程可以 sleep
                },
                DeviceState::Disconnected => {
                    // **去抖动**：检查是否在冷却期内
                    let now = Instant::now();
                    if let Some(last_time) = last_disconnect_time
                        && now.duration_since(last_time) < config.reconnect_debounce
                    {
                        // 仍在冷却期内，等待
                        thread::sleep(config.reconnect_debounce - now.duration_since(last_time));
                    }
                    last_disconnect_time = Some(now);

                    // 进入重连状态
                    *device_state.write().unwrap() = DeviceState::Reconnecting;
                },
                DeviceState::Reconnecting => {
                    // 尝试连接设备
                    match Self::try_connect_device(&config) {
                        Ok(new_adapter) => {
                            *adapter.write().unwrap() = Some(new_adapter);
                            *device_state.write().unwrap() = DeviceState::Connected;
                            last_disconnect_time = None; // 重置去抖动计时器
                            eprintln!("Device connected successfully");
                        },
                        Err(e) => {
                            eprintln!(
                                "Failed to connect device: {}. Retrying in {:?}...",
                                e, config.reconnect_interval
                            );
                            thread::sleep(config.reconnect_interval);
                            // 保持 Reconnecting 状态，继续重试
                        },
                    }
                },
            }
        }
    }

    /// USB 接收循环（高优先级线程，阻塞 IO）
    ///
    /// **关键**：使用阻塞 IO，数据到达时内核立即唤醒线程（微秒级）
    /// **严禁**：不要使用 sleep 或轮询
    /// **优化**：使用 RwLock 读取锁，减少锁竞争
    fn usb_receive_loop(
        adapter: Arc<RwLock<Option<GsUsbCanAdapter>>>,
        device_state: Arc<RwLock<DeviceState>>,
        clients: Arc<RwLock<ClientManager>>,
        socket_uds: Option<std::os::unix::net::UnixDatagram>,
        socket_udp: Option<std::net::UdpSocket>,
        stats: Arc<RwLock<DaemonStats>>,
    ) {
        loop {
            // 1. 检查设备状态（快速检查，不要阻塞）
            let adapter_guard = adapter.read().unwrap();
            match adapter_guard.as_ref() {
                Some(_a) => {
                    // 设备已连接，继续处理
                },
                None => {
                    // 设备未连接，短暂等待后重试（设备管理线程会处理重连）
                    drop(adapter_guard);
                    thread::sleep(Duration::from_millis(100));
                    continue;
                },
            };

            // 2. 从 USB 设备读取 CAN 帧（阻塞 IO）
            // **关键**：receive() 内部使用阻塞的 rusb.read_bulk()，没有数据时线程挂起
            // 注意：需要获取可变引用，所以需要先 drop 读取锁，再获取写入锁
            drop(adapter_guard);

            let frame = {
                let mut adapter_guard = adapter.write().unwrap();
                match adapter_guard.as_mut() {
                    Some(a) => match a.receive() {
                        Ok(f) => {
                            // 更新统计信息
                            stats.read().unwrap().increment_rx();
                            f
                        },
                        Err(piper_sdk::can::CanError::Timeout) => {
                            // 超时是正常的（receive 内部有超时设置），继续循环
                            // 注意：这里的超时是 USB 层面的超时（如 2ms），不是 sleep
                            continue;
                        },
                        Err(e) => {
                            // 其他错误：可能是设备断开，通知设备管理线程
                            eprintln!("[Daemon] USB receive error: {:?}", e);
                            *device_state.write().unwrap() = DeviceState::Disconnected;
                            // 短暂等待后重试，避免死循环
                            thread::sleep(Duration::from_millis(100));
                            continue;
                        },
                    },
                    None => {
                        // 设备未连接，短暂等待后重试
                        thread::sleep(Duration::from_millis(100));
                        continue;
                    },
                }
            };

            // 3. 向符合条件的客户端发送（使用读取锁，支持并发）
            let mut failed_clients = Vec::new();
            {
                let clients_guard = clients.read().unwrap();
                for client in clients_guard.iter() {
                    // 检查过滤规则
                    if !client.matches_filter(frame.id) {
                        continue;
                    }

                    // 零拷贝编码（使用栈上缓冲区）
                    let mut buf = [0u8; 64];
                    let encoded =
                        match piper_sdk::can::gs_usb_udp::protocol::encode_receive_frame_zero_copy(
                            &frame, &mut buf,
                        ) {
                            Ok(data) => data,
                            Err(e) => {
                                eprintln!("Failed to encode frame: {}", e);
                                continue;
                            },
                        };

                    // 根据客户端地址类型发送
                    let send_failed = match &client.addr {
                        ClientAddr::Unix(uds_path) => {
                            if let Some(ref socket) = socket_uds {
                                // UDS 发送（使用路径字符串）
                                match socket.send_to(encoded, uds_path) {
                                    Ok(_) => {
                                        stats.read().unwrap().increment_ipc_sent();
                                        false
                                    },
                                    Err(e) => {
                                        // 检查是否是文件不存在的错误（客户端可能已断开）
                                        if e.kind() == std::io::ErrorKind::NotFound {
                                            eprintln!(
                                                "Client {} socket not found (may have disconnected): {}",
                                                client.id, uds_path
                                            );
                                            true // 标记为失败，需要清理
                                        } else {
                                            eprintln!(
                                                "Failed to send frame to client {} (UDS): {} (path: {})",
                                                client.id, e, uds_path
                                            );
                                            false // 其他错误，不立即清理
                                        }
                                    },
                                }
                            } else {
                                false
                            }
                        },
                        ClientAddr::Udp(addr) => {
                            if let Some(ref socket) = socket_udp {
                                match socket.send_to(encoded, *addr) {
                                    Ok(_) => {
                                        stats.read().unwrap().increment_ipc_sent();
                                        false
                                    },
                                    Err(e) => {
                                        eprintln!(
                                            "Failed to send frame to client {}: {}",
                                            client.id, e
                                        );
                                        false
                                    },
                                }
                            } else {
                                false
                            }
                        },
                    };

                    // 如果发送失败且是文件不存在错误，记录需要清理的客户端
                    if send_failed {
                        failed_clients.push(client.id);
                    }
                }
            }

            // 清理发送失败的客户端（在释放读取锁后）
            if !failed_clients.is_empty() {
                let mut clients_guard = clients.write().unwrap();
                for client_id in failed_clients {
                    eprintln!("Removing disconnected client {}", client_id);
                    clients_guard.unregister(client_id);
                }
            }
        }
    }

    /// IPC 接收循环（高优先级线程，阻塞 IO）
    ///
    /// **关键**：使用阻塞 IO，数据到达时内核立即唤醒线程（微秒级）
    /// **严禁**：不要使用 sleep 或轮询
    fn ipc_receive_loop(
        socket: std::os::unix::net::UnixDatagram,
        adapter: Arc<RwLock<Option<GsUsbCanAdapter>>>,
        device_state: Arc<RwLock<DeviceState>>,
        clients: Arc<RwLock<ClientManager>>,
        stats: Arc<RwLock<DaemonStats>>,
    ) {
        // 设置高优先级（macOS QoS）
        crate::macos_qos::set_high_priority();

        let mut buf = [0u8; 1024];

        loop {
            // **关键**：阻塞接收，没有数据时线程挂起
            match socket.recv_from(&mut buf) {
                Ok((len, client_addr)) => {
                    // 解析消息
                    if let Ok(msg) =
                        piper_sdk::can::gs_usb_udp::protocol::decode_message(&buf[..len])
                    {
                        // 更新统计（接收 IPC 消息）
                        stats.read().unwrap().increment_ipc_received();
                        Self::handle_ipc_message(
                            msg,
                            client_addr,
                            &adapter,
                            &device_state,
                            &clients,
                            &socket,
                            &stats,
                        );
                    }
                },
                Err(e) => {
                    eprintln!("IPC Recv Error: {}", e);
                    // 只有出错时才 sleep 一下防止死循环日志
                    thread::sleep(Duration::from_millis(100));
                },
            }
        }
    }

    /// 处理 IPC 消息
    fn handle_ipc_message(
        msg: piper_sdk::can::gs_usb_udp::protocol::Message,
        client_addr: std::os::unix::net::SocketAddr,
        adapter: &Arc<RwLock<Option<GsUsbCanAdapter>>>,
        _device_state: &Arc<RwLock<DeviceState>>,
        clients: &Arc<RwLock<ClientManager>>,
        socket: &std::os::unix::net::UnixDatagram,
        stats: &Arc<RwLock<DaemonStats>>,
    ) {
        match msg {
            piper_sdk::can::gs_usb_udp::protocol::Message::Heartbeat { client_id } => {
                // 更新客户端活动时间
                clients.write().unwrap().update_activity(client_id);
            },
            piper_sdk::can::gs_usb_udp::protocol::Message::Connect { client_id, filters } => {
                // 注册客户端（使用从 recv_from 获取的真实地址）
                // 尝试从 UnixSocketAddr 获取路径（如果可用）
                let addr_str = match client_addr.as_pathname() {
                    Some(path) => match path.to_str() {
                        Some(s) => s.to_string(),
                        None => {
                            eprintln!(
                                "Warning: Client {} path contains invalid UTF-8, using fallback",
                                client_id
                            );
                            format!("/tmp/gs_usb_client_{}.sock", client_id)
                        },
                    },
                    None => {
                        // 如果是抽象地址，尝试使用 client_id 构造路径
                        // 注意：客户端应该绑定到 /tmp/gs_usb_client_{pid}.sock
                        // 如果获取不到路径，可能是客户端没有正确绑定
                        eprintln!(
                            "Warning: Client {} address is abstract, using fallback path",
                            client_id
                        );
                        format!("/tmp/gs_usb_client_{}.sock", client_id)
                    },
                };

                eprintln!(
                    "Client {} connected from {:?} (path: {})",
                    client_id, client_addr, addr_str
                );

                let addr = ClientAddr::Unix(addr_str.clone());
                let register_result = clients.write().unwrap().register_with_unix_addr(
                    client_id,
                    addr,
                    &client_addr, // 传递引用
                    filters,
                );

                // 发送 ConnectAck 消息
                let mut ack_buf = [0u8; 13];
                let status = if register_result.is_ok() {
                    0 // 成功
                } else {
                    1 // 失败（通常是客户端 ID 已存在）
                };
                let encoded_ack = piper_sdk::can::gs_usb_udp::protocol::encode_connect_ack(
                    client_id,
                    status,
                    0, // seq = 0 for ConnectAck
                    &mut ack_buf,
                );

                // 发送 ConnectAck 到客户端
                if let Err(e) = socket.send_to(encoded_ack, &addr_str) {
                    eprintln!("Failed to send ConnectAck to client {}: {}", client_id, e);
                } else {
                    eprintln!(
                        "Sent ConnectAck to client {} (status: {})",
                        client_id, status
                    );
                }

                if let Err(e) = register_result {
                    eprintln!("Failed to register client {}: {}", client_id, e);
                }
            },
            piper_sdk::can::gs_usb_udp::protocol::Message::Disconnect { client_id } => {
                clients.write().unwrap().unregister(client_id);
            },
            piper_sdk::can::gs_usb_udp::protocol::Message::SendFrame { frame, seq: _seq } => {
                // 发送 CAN 帧到 USB 设备
                let mut adapter_guard = adapter.write().unwrap();
                if let Some(ref mut adapter_ref) = *adapter_guard {
                    match adapter_ref.send(frame) {
                        Ok(_) => {
                            // 更新统计（USB 发送成功）
                            stats.read().unwrap().increment_tx();
                            // 发送成功，可以发送 SendAck（可选）
                        },
                        Err(e) => {
                            eprintln!("Failed to send frame: {}", e);
                            // 可以发送 Error 消息回客户端（带 seq）
                        },
                    }
                }
            },
            _ => {
                // 其他消息类型暂未实现
            },
        }
    }

    /// 客户端清理循环（低优先级线程）
    ///
    /// 定期清理超时客户端，避免客户端列表无限增长
    fn client_cleanup_loop(clients: Arc<RwLock<ClientManager>>) {
        // 设置低优先级（设备管理线程）
        crate::macos_qos::set_low_priority();

        loop {
            // 每 5 秒清理一次超时客户端
            thread::sleep(Duration::from_secs(5));
            clients.write().unwrap().cleanup_timeout();
        }
    }

    /// 状态打印循环（低优先级线程）
    ///
    /// 定期打印守护进程状态信息，包括客户端数量、CAN 帧 FPS 等
    /// 每次打印后重置统计信息，使 FPS 显示最近一段时间的平均值
    fn status_print_loop(
        clients: Arc<RwLock<ClientManager>>,
        device_state: Arc<RwLock<DeviceState>>,
        stats: Arc<RwLock<DaemonStats>>,
        interval: Duration,
    ) {
        // 设置低优先级（状态打印线程）
        crate::macos_qos::set_low_priority();

        loop {
            thread::sleep(interval);

            let (client_count, client_ids) = {
                let clients_guard = clients.read().unwrap();
                let ids: Vec<u32> = clients_guard.iter().map(|client| client.id).collect();
                (ids.len(), ids)
            };
            let state = *device_state.read().unwrap();

            // 读取统计信息并计算 FPS
            let (rx_fps, tx_fps, ipc_sent_fps, ipc_received_fps) = {
                let stats_guard = stats.read().unwrap();
                (
                    stats_guard.get_rx_fps(),
                    stats_guard.get_tx_fps(),
                    stats_guard.get_ipc_sent_fps(),
                    stats_guard.get_ipc_received_fps(),
                )
            };

            let state_str = match state {
                DeviceState::Connected => "Connected",
                DeviceState::Disconnected => "Disconnected",
                DeviceState::Reconnecting => "Reconnecting",
            };

            // 格式化客户端 ID 列表
            let client_ids_str = if client_ids.is_empty() {
                "[]".to_string()
            } else {
                format!("{:?}", client_ids)
            };

            eprintln!(
                "[Status] State: {}, Clients: {} {}, RX: {:.1} fps, TX: {:.1} fps, IPC→Client: {:.1} fps, IPC←Client: {:.1} fps",
                state_str,
                client_count,
                client_ids_str,
                rx_fps,
                tx_fps,
                ipc_sent_fps,
                ipc_received_fps
            );

            // 重置统计信息，使下次 FPS 计算基于新的时间段
            stats.write().unwrap().reset();
        }
    }

    /// 启动守护进程
    ///
    /// 启动所有工作线程并进入主循环
    pub fn run(&mut self) -> Result<(), DaemonError> {
        // 1. 初始化 Socket（UDS 优先，UDP 可选）
        self.init_sockets()?;

        // 2. 启动设备管理线程（状态机 + 热拔插恢复）
        let adapter_clone = Arc::clone(&self.adapter);
        let device_state_clone = Arc::clone(&self.device_state);
        let config_clone = self.config.clone();

        thread::Builder::new()
            .name("device_manager".into())
            .spawn(move || {
                Self::device_manager_loop(adapter_clone, device_state_clone, config_clone);
            })
            .map_err(|e| {
                DaemonError::Io(format!("Failed to spawn device manager thread: {}", e))
            })?;

        // 3. 启动 USB 接收线程（从 USB 设备读取 CAN 帧）
        let adapter_clone = Arc::clone(&self.adapter);
        let device_state_clone = Arc::clone(&self.device_state);
        let clients_clone = Arc::clone(&self.clients);
        let socket_uds_clone = self.socket_uds.as_ref().and_then(|s| s.try_clone().ok());
        let socket_udp_clone = self.socket_udp.as_ref().and_then(|s| s.try_clone().ok());
        let stats_clone = Arc::clone(&self.stats);

        thread::Builder::new()
            .name("usb_receive".into())
            .spawn(move || {
                Self::usb_receive_loop(
                    adapter_clone,
                    device_state_clone,
                    clients_clone,
                    socket_uds_clone,
                    socket_udp_clone,
                    stats_clone,
                );
            })
            .map_err(|e| DaemonError::Io(format!("Failed to spawn USB receive thread: {}", e)))?;

        // 4. 启动客户端清理线程（定期清理超时客户端）
        let clients_clone = Arc::clone(&self.clients);
        thread::Builder::new()
            .name("client_cleanup".into())
            .spawn(move || {
                Self::client_cleanup_loop(clients_clone);
            })
            .map_err(|e| {
                DaemonError::Io(format!("Failed to spawn client cleanup thread: {}", e))
            })?;

        // 5. 启动 IPC 接收线程（处理客户端消息）
        if let Some(socket_uds) = self.socket_uds.take() {
            let adapter_clone = Arc::clone(&self.adapter);
            let device_state_clone = Arc::clone(&self.device_state);
            let clients_clone = Arc::clone(&self.clients);
            let stats_clone = Arc::clone(&self.stats);

            thread::Builder::new()
                .name("ipc_receive_uds".into())
                .spawn(move || {
                    Self::ipc_receive_loop(
                        socket_uds,
                        adapter_clone,
                        device_state_clone,
                        clients_clone,
                        stats_clone,
                    );
                })
                .map_err(|e| {
                    DaemonError::Io(format!("Failed to spawn IPC receive thread: {}", e))
                })?;
        }

        // 6. 如果配置了 UDP，启动 UDP 接收线程
        if let Some(_socket_udp) = self.socket_udp.take() {
            // UDP 接收循环与 UDS 类似，但需要处理 SocketAddr
            // 这里简化处理，可以复用 ipc_receive_loop 的逻辑
            // 注意：UDP 需要不同的处理方式，因为 recv_from 返回 SocketAddr
            // 暂时跳过 UDP 实现，专注于 UDS
        }

        // 7. 启动状态打印线程（定期打印统计信息）
        let clients_clone = Arc::clone(&self.clients);
        let device_state_clone = Arc::clone(&self.device_state);
        let stats_clone = Arc::clone(&self.stats);

        thread::Builder::new()
            .name("status_print".into())
            .spawn(move || {
                Self::status_print_loop(
                    clients_clone,
                    device_state_clone,
                    stats_clone,
                    Duration::from_secs(5), // 每 5 秒打印一次
                );
            })
            .map_err(|e| DaemonError::Io(format!("Failed to spawn status print thread: {}", e)))?;

        // 8. 主线程等待（所有工作都在后台线程中）
        eprintln!("GS-USB Daemon started. Press Ctrl+C to stop.");
        loop {
            thread::park(); // 主线程挂起，等待信号
        }
    }
}

impl Drop for Daemon {
    /// 守护进程退出时自动清理 UDS socket 文件
    fn drop(&mut self) {
        self.cleanup_uds_socket();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_device_state_transitions() {
        let state = Arc::new(RwLock::new(DeviceState::Connected));
        // 验证状态转换
        *state.write().unwrap() = DeviceState::Disconnected;
        assert_eq!(*state.read().unwrap(), DeviceState::Disconnected);

        *state.write().unwrap() = DeviceState::Reconnecting;
        assert_eq!(*state.read().unwrap(), DeviceState::Reconnecting);

        *state.write().unwrap() = DeviceState::Connected;
        assert_eq!(*state.read().unwrap(), DeviceState::Connected);
    }

    #[test]
    fn test_daemon_config_default() {
        let config = DaemonConfig::default();
        assert_eq!(config.bitrate, 1_000_000);
        assert_eq!(config.reconnect_interval, Duration::from_secs(1));
        assert_eq!(config.reconnect_debounce, Duration::from_millis(500));
        assert_eq!(config.client_timeout, Duration::from_secs(30));
    }

    #[test]
    fn test_daemon_new() {
        let config = DaemonConfig::default();
        let daemon = Daemon::new(config).unwrap();

        // 验证初始状态
        assert_eq!(
            *daemon.device_state.read().unwrap(),
            DeviceState::Disconnected
        );
        assert!(daemon.adapter.read().unwrap().is_none());
    }
}
