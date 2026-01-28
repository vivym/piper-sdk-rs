//! 守护进程核心逻辑
//!
//! 实现多线程阻塞架构、设备状态机、热拔插恢复
//!
//! 参考：`daemon_implementation_plan.md` 第 4.1.3 节

use crate::client_manager::{ClientAddr, ClientManager};
use piper_sdk::can::gs_usb::{
    GsUsbCanAdapter,
    split::{GsUsbRxAdapter, GsUsbTxAdapter},
};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::thread;
use std::time::{Duration, Instant};

/// 获取临时 socket 路径（XDG 合规）
///
/// 使用 `dirs` crate 确保跨平台兼容性和 XDG 规范合规性：
/// - Linux: $XDG_RUNTIME_DIR 或 /tmp
/// - macOS: $TMPDIR 或 /tmp
/// - Windows: %TEMP%
fn get_temp_socket_path(socket_name: &str) -> String {
    // ✅ 优先使用 XDG_RUNTIME_DIR（Linux/macOS，符合 XDG 规范）
    if let Some(runtime_dir) = dirs::runtime_dir() {
        let path = runtime_dir.join(socket_name);
        if let Some(parent) = path.parent()
            && (parent.exists() || std::fs::create_dir_all(parent).is_ok())
        {
            return path.to_string_lossy().to_string();
        }
    }

    // ✅ 其次使用系统临时目录（跨平台）
    std::env::temp_dir().join(socket_name).to_string_lossy().to_string()
}

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
    /// UDS Socket 路径（可选，默认不使用）
    pub uds_path: Option<String>,

    /// UDP 监听地址（默认传输方式，如 "127.0.0.1:18888"）
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
            uds_path: None,                                // UDS 不再作为默认，使用 UDP
            udp_addr: Some("127.0.0.1:18888".to_string()), // UDP 作为默认传输方式
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

/// 守护进程统计信息（基础版本）
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
    /// 客户端发送阻塞次数（WouldBlock/ENOBUFS）
    client_send_blocked: AtomicU64,
    /// 主动断开的客户端数量
    client_disconnected: AtomicU64,
    /// 统计开始时间
    start_time: Instant,
    /// 详细统计信息（健康度监控）
    detailed: Arc<RwLock<DetailedStats>>,
}

/// 详细统计信息（健康度监控）
#[derive(Debug)]
struct DetailedStats {
    // USB 传输错误
    usb_transfer_errors: AtomicU64, // libusb 底层错误计数
    usb_timeout_count: AtomicU64,   // 超时次数（区分于其他错误）
    usb_stall_count: AtomicU64,     // USB 端点 STALL 次数
    usb_no_device_count: AtomicU64, // NoDevice 错误次数

    // CAN 总线健康度
    can_error_frames: AtomicU64,              // CAN 总线错误帧计数
    can_bus_off_count: AtomicU64,             // Bus Off 事件发生次数
    is_bus_off: AtomicBool,                   // Bus Off 状态标志（用于防抖）
    bus_off_monitoring_supported: AtomicBool, // 设备是否支持 Bus Off 检测（Log Once 使用）
    can_error_passive_count: AtomicU64,       // Error Passive 状态进入次数
    is_error_passive: AtomicBool,             // Error Passive 状态标志（用于防抖）

    // 客户端健康度
    /// 降频的客户端数（仅在 Unix 平台上使用）
    #[cfg_attr(not(unix), allow(dead_code))]
    client_degraded: AtomicU64,

    // 系统资源
    cpu_usage_percent: AtomicU32, // CPU 占用率（0-100）
    memory_usage_mb: AtomicU32,   // 内存占用（MB）

    // 性能基线
    baseline_rx_fps: AtomicU64, // 基线 RX 帧率（f64 位模式存储）
    baseline_tx_fps: AtomicU64, // 基线 TX 帧率（f64 位模式存储）
    is_warmed_up: AtomicBool,   // 预热期标志
}

impl DetailedStats {
    fn new() -> Self {
        Self {
            usb_transfer_errors: AtomicU64::new(0),
            usb_timeout_count: AtomicU64::new(0),
            usb_stall_count: AtomicU64::new(0),
            usb_no_device_count: AtomicU64::new(0),
            can_error_frames: AtomicU64::new(0),
            can_bus_off_count: AtomicU64::new(0),
            is_bus_off: AtomicBool::new(false),
            bus_off_monitoring_supported: AtomicBool::new(true), // 默认为 true（乐观策略），现代 GS-USB 设备普遍支持
            can_error_passive_count: AtomicU64::new(0),
            is_error_passive: AtomicBool::new(false),
            client_degraded: AtomicU64::new(0),
            cpu_usage_percent: AtomicU32::new(0),
            memory_usage_mb: AtomicU32::new(0),
            baseline_rx_fps: AtomicU64::new(0), // 初始化为 0.0 的位模式
            baseline_tx_fps: AtomicU64::new(0), // 初始化为 0.0 的位模式
            is_warmed_up: AtomicBool::new(false), // 预热期标志
        }
    }

    // ============================================================
    // 基线计算配置常量（集中管理，便于调优）
    // ============================================================

    /// 预热期时长（秒）
    const WARMUP_PERIOD_SECS: u64 = 10;

    /// EWMA 平滑因子（0.0 - 1.0）
    /// 值越小，基线更新越慢，越稳定；值越大，基线更新越快，越敏感
    const EWMA_ALPHA: f64 = 0.01;

    // ============================================================
    // 性能基线访问方法（位模式转换）
    // ============================================================

    /// 获取 RX 基线 FPS
    fn baseline_rx_fps(&self) -> f64 {
        f64::from_bits(self.baseline_rx_fps.load(Ordering::Relaxed))
    }

    /// 设置 RX 基线 FPS
    fn set_baseline_rx_fps(&self, fps: f64) {
        self.baseline_rx_fps.store(fps.to_bits(), Ordering::Relaxed);
    }

    /// 获取 TX 基线 FPS
    fn baseline_tx_fps(&self) -> f64 {
        f64::from_bits(self.baseline_tx_fps.load(Ordering::Relaxed))
    }

    /// 设置 TX 基线 FPS
    fn set_baseline_tx_fps(&self, fps: f64) {
        self.baseline_tx_fps.store(fps.to_bits(), Ordering::Relaxed);
    }

    /// 检查是否已预热完成
    fn is_warmed_up(&self) -> bool {
        self.is_warmed_up.load(Ordering::Relaxed)
    }

    /// 更新性能基线（启动后稳定期计算 + 动态更新）
    ///
    /// # 参数
    /// - `rx_fps`: 当前 RX 帧率
    /// - `tx_fps`: 当前 TX 帧率
    /// - `elapsed`: 从启动开始经过的时间
    ///
    /// # 注意
    /// - **必须由定时器触发**，确保固定时间间隔（例如每秒一次）
    /// - 如果调用间隔波动很大，EWMA 的衰减速度会变得不稳定
    /// - ⚠️ 虽然 `AtomicU64` 保证了单个值的原子性，但两个基线的更新不是原子的
    ///   在极少数情况下，可能会读到一个"旧的 RX 基线"和一个"新的 TX 基线"
    ///   这对监控指标来说通常不是问题，因为这只是统计数据
    pub fn update_baseline(&self, rx_fps: f64, tx_fps: f64, elapsed: Duration) {
        let elapsed_secs = elapsed.as_secs();

        // 预热期：累积计算平均值
        if elapsed_secs < Self::WARMUP_PERIOD_SECS {
            let samples = elapsed_secs.max(1); // 避免除零
            let current_rx = self.baseline_rx_fps();
            let current_tx = self.baseline_tx_fps();

            // 累积平均
            let new_rx = (current_rx * (samples - 1) as f64 + rx_fps) / samples as f64;
            let new_tx = (current_tx * (samples - 1) as f64 + tx_fps) / samples as f64;

            self.set_baseline_rx_fps(new_rx);
            self.set_baseline_tx_fps(new_tx);
        } else {
            // 预热期结束，标记为已预热
            if !self.is_warmed_up() {
                self.is_warmed_up.store(true, Ordering::Relaxed);
                tracing::info!(
                    "Performance baseline established: RX={:.1} fps, TX={:.1} fps",
                    self.baseline_rx_fps(),
                    self.baseline_tx_fps()
                );
            }

            // 动态基线更新：使用指数加权移动平均 (EWMA)
            // ⚠️ 注意：此公式假设 update_baseline 以固定时间间隔（例如每秒）调用
            // 如果调用间隔波动很大，EWMA 的衰减速度会变得不稳定
            // 解决方案：必须由定时器（Timer）触发，或根据 elapsed 动态调整 ALPHA
            // 简化实施：对于守护进程的统计报告，通常就是每秒一次，固定 ALPHA 足够
            let current_rx = self.baseline_rx_fps();
            let current_tx = self.baseline_tx_fps();

            let new_rx = current_rx * (1.0 - Self::EWMA_ALPHA) + rx_fps * Self::EWMA_ALPHA;
            let new_tx = current_tx * (1.0 - Self::EWMA_ALPHA) + tx_fps * Self::EWMA_ALPHA;

            self.set_baseline_rx_fps(new_rx);
            self.set_baseline_tx_fps(new_tx);
        }
    }

    /// 检测性能异常（仅在预热期后检测）
    ///
    /// # 参数
    /// - `current_rx_fps`: 当前 RX 帧率
    /// - `current_tx_fps`: 当前 TX 帧率
    ///
    /// # 返回
    /// - `true`: 性能下降（当前 FPS 低于基线的 50%）
    /// - `false`: 性能正常或预热期未结束
    pub fn is_performance_degraded(&self, current_rx_fps: f64, current_tx_fps: f64) -> bool {
        // 预热期内不进行异常检测（避免误报）
        if !self.is_warmed_up() {
            return false;
        }

        let baseline_rx = self.baseline_rx_fps();
        let baseline_tx = self.baseline_tx_fps();

        if baseline_rx == 0.0 || baseline_tx == 0.0 {
            return false; // 基线未建立
        }

        // 如果当前 FPS 低于基线的 50%，认为性能下降
        current_rx_fps < baseline_rx * 0.5 || current_tx_fps < baseline_tx * 0.5
    }

    /// 健康度评分（0-100）
    ///
    /// # 参数
    /// - `current_rx_fps`: 当前 RX 帧率（用于性能基线异常检测）
    /// - `current_tx_fps`: 当前 TX 帧率（用于性能基线异常检测）
    pub fn health_score(&self, current_rx_fps: f64, current_tx_fps: f64) -> u8 {
        let mut score = 100u8;

        // Bus Off 检测（最高优先级，系统瘫痪级别故障）
        let bus_off_count = self.can_bus_off_count.load(Ordering::Relaxed);
        if bus_off_count > 0 {
            // Bus Off 是系统瘫痪级别故障，直接设为 0
            score = 0;
            // 或者严重扣分（根据业务需求）
            // score = score.saturating_sub(50);
        }

        // 如果设备不支持 Bus Off 检测，给予固定扣分（-5 分）
        // 反映监控能力的缺失（对于安全关键应用很重要）
        if !self.bus_off_monitoring_supported.load(Ordering::Relaxed) {
            score = score.saturating_sub(5); // 扣 5 分（监控能力缺失）
        }

        // USB 错误扣分（包括 STALL）
        let usb_errors = self.usb_transfer_errors.load(Ordering::Relaxed);
        let usb_stalls = self.usb_stall_count.load(Ordering::Relaxed);
        let total_usb_errors = usb_errors + usb_stalls;

        if total_usb_errors > 100 {
            score = score.saturating_sub(20);
        } else if total_usb_errors > 10 {
            score = score.saturating_sub(10);
        }

        // CAN 错误扣分
        let can_errors = self.can_error_frames.load(Ordering::Relaxed);
        if can_errors > 1000 {
            score = score.saturating_sub(30);
        } else if can_errors > 100 {
            score = score.saturating_sub(15);
        }

        // 性能基线异常检测（性能下降扣分）
        // 如果当前 FPS 低于基线的 50%，认为性能下降，扣 10 分
        // 这反映了系统性能问题，可能导致控制延迟
        if self.is_performance_degraded(current_rx_fps, current_tx_fps) {
            score = score.saturating_sub(10); // 扣 10 分（性能下降）
        }

        // 客户端问题扣分（通过 DaemonStats 访问）
        // 注意：这里我们只评估 USB/CAN 错误，客户端问题在 DaemonStats 中

        // CPU 占用扣分
        let cpu = self.cpu_usage_percent.load(Ordering::Relaxed);
        if cpu > 90 {
            score = score.saturating_sub(15);
        } else if cpu > 70 {
            score = score.saturating_sub(10);
        } else if cpu > 50 {
            score = score.saturating_sub(5);
        }

        // 内存占用扣分（通常不是问题，但监控）
        let memory = self.memory_usage_mb.load(Ordering::Relaxed);
        if memory > 1000 {
            score = score.saturating_sub(5);
        }

        score
    }

    /// 检查并更新 Bus Off 状态（带防抖）
    ///
    /// 只有状态从 false -> true 的转换才计数（上升沿检测）
    /// 统计的是"Bus Off 事件发生的次数"，而不是"处于 Bus Off 状态的帧数"
    pub fn update_bus_off_status(&self, is_bus_off_now: bool) {
        let was_bus_off = self.is_bus_off.load(Ordering::Relaxed);

        // 上升沿检测：只有从 false -> true 的转换才计数
        if !was_bus_off && is_bus_off_now {
            // 新进入 Bus Off 状态，计数加 1
            let count = self.can_bus_off_count.fetch_add(1, Ordering::Relaxed) + 1;
            self.is_bus_off.store(true, Ordering::Relaxed);

            tracing::error!("CAN Bus Off detected! Total occurrences: {}", count);
            // 注意：Bus Off 发生后，健康度评分会被设为 0（系统瘫痪级别）
            // 可以考虑触发额外的告警或自动恢复流程（例如：通知监控系统、自动重启设备等）
        } else if was_bus_off && !is_bus_off_now {
            // 从 Bus Off 状态恢复，重置标志
            self.is_bus_off.store(false, Ordering::Relaxed);
            tracing::info!("CAN Bus Off recovered. Ready for next detection.");
        }
        // 如果状态未变化，不做任何操作
    }

    /// 强制重置 Bus Off 状态标志（设备重连或手动复位后调用）
    pub fn reset_bus_off_status(&self) {
        let was_bus_off = self.is_bus_off.swap(false, Ordering::Relaxed);
        if was_bus_off {
            tracing::info!("Bus Off status flag reset (device reconnected or manually reset)");
        }
    }

    // ============================================================
    // Error Passive 检测（Bus Off 之前的警告）
    // ============================================================

    /// 检查并更新 Error Passive 状态（带防抖，与 Bus Off 相同）
    ///
    /// # 参数
    /// - `is_error_passive_now`: 当前是否处于 Error Passive 状态
    ///
    /// 统计的是"Error Passive 事件发生的次数"，而不是"处于 Error Passive 状态的帧数"
    pub fn update_error_passive_status(&self, is_error_passive_now: bool) {
        let was_error_passive = self.is_error_passive.load(Ordering::Relaxed);

        // 上升沿检测：只有从 false -> true 的转换才计数
        if !was_error_passive && is_error_passive_now {
            // 新进入 Error Passive 状态，计数加 1
            let count = self.can_error_passive_count.fetch_add(1, Ordering::Relaxed) + 1;
            self.is_error_passive.store(true, Ordering::Relaxed);

            tracing::warn!(
                "CAN Error Passive detected! Total occurrences: {} (warning: may lead to Bus Off)",
                count
            );
        } else if was_error_passive && !is_error_passive_now {
            // 从 Error Passive 状态恢复，重置标志
            self.is_error_passive.store(false, Ordering::Relaxed);
            tracing::info!("CAN Error Passive recovered. Ready for next detection.");
        }
        // 如果状态未变化，不做任何操作
    }

    /// 强制重置 Error Passive 状态标志（设备重连或手动复位后调用）
    pub fn reset_error_passive_status(&self) {
        let was_error_passive = self.is_error_passive.swap(false, Ordering::Relaxed);
        if was_error_passive {
            tracing::info!(
                "Error Passive status flag reset (device reconnected or manually reset)"
            );
        }
    }
}

impl DaemonStats {
    fn new() -> Self {
        Self {
            rx_frames: AtomicU64::new(0),
            tx_frames: AtomicU64::new(0),
            ipc_sent: AtomicU64::new(0),
            ipc_received: AtomicU64::new(0),
            client_send_blocked: AtomicU64::new(0),
            client_disconnected: AtomicU64::new(0),
            start_time: Instant::now(),
            detailed: Arc::new(RwLock::new(DetailedStats::new())),
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
        self.client_send_blocked.store(0, Ordering::Relaxed);
        self.client_disconnected.store(0, Ordering::Relaxed);
        self.start_time = Instant::now();
        // 详细统计不重置（累积历史数据）
    }

    /// 获取健康度评分
    ///
    /// # 参数
    /// - `rx_fps`: 当前 RX 帧率（用于性能基线异常检测）
    /// - `tx_fps`: 当前 TX 帧率（用于性能基线异常检测）
    fn health_score(&self, rx_fps: f64, tx_fps: f64) -> u8 {
        self.detailed.read().unwrap().health_score(rx_fps, tx_fps)
    }
}

/// 守护进程状态
pub struct Daemon {
    /// RX 适配器（只读，用于 USB 接收线程）
    rx_adapter: Arc<Mutex<Option<GsUsbRxAdapter>>>,

    /// TX 适配器（只写，用于 IPC 接收线程）
    tx_adapter: Arc<Mutex<Option<GsUsbTxAdapter>>>,

    /// 设备状态
    device_state: Arc<RwLock<DeviceState>>,

    /// UDS Socket（Unix Domain Socket）
    #[cfg(unix)]
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
            // 分离 RX 和 TX adapter
            rx_adapter: Arc::new(Mutex::new(None)),
            tx_adapter: Arc::new(Mutex::new(None)),
            device_state: Arc::new(RwLock::new(DeviceState::Disconnected)),
            #[cfg(unix)]
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
        #[cfg(unix)]
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

            // 设置非阻塞模式（关键修复）
            // 防止故障客户端缓冲区满时阻塞整个 daemon
            socket.set_nonblocking(true).map_err(|e| {
                DaemonError::SocketInit(format!("Failed to set non-blocking mode: {}", e))
            })?;

            self.socket_uds = Some(socket);
        }
        #[cfg(not(unix))]
        if self.config.uds_path.is_some() {
            return Err(DaemonError::SocketInit(
                "Unix Domain Sockets are not supported on this platform".to_string(),
            ));
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

    /// 尝试连接设备并分离为 RX/TX adapter
    ///
    /// 返回分离的 RX 和 TX adapter，支持并发访问
    fn try_connect_device(
        config: &DaemonConfig,
        stats: Arc<RwLock<DaemonStats>>,
    ) -> Result<(GsUsbRxAdapter, GsUsbTxAdapter), DaemonError> {
        // 1. 扫描设备
        eprintln!(
            "[Daemon] Scanning for GS-USB devices (serial: {:?})...",
            config.serial_number
        );

        // 先扫描所有设备，打印信息
        use piper_sdk::can::gs_usb::device::GsUsbDevice;
        match GsUsbDevice::scan_info() {
            Ok(infos) => {
                eprintln!("[Daemon] Found {} GS-USB device(s):", infos.len());
                for (i, info) in infos.iter().enumerate() {
                    eprintln!(
                        "  [{}] VID:PID={:04x}:{:04x} bus={} addr={} serial={:?}",
                        i,
                        info.vendor_id,
                        info.product_id,
                        info.bus_number,
                        info.address,
                        info.serial_number.as_deref()
                    );
                }
            },
            Err(e) => {
                eprintln!("[Daemon] Warning: Failed to scan devices for info: {}", e);
            },
        }

        let mut adapter = GsUsbCanAdapter::new_with_serial(config.serial_number.as_deref())
            .map_err(|e| DaemonError::DeviceInit(format!("{}", e)))?;

        eprintln!("[Daemon] Device found and initialized successfully");

        // 设定接收超时（从 200ms 减小到 2ms，降低延迟抖动）
        // 注意：虽然会增加 CPU 占用，但对于实时控制场景，延迟比功耗更重要
        adapter.set_receive_timeout(Duration::from_millis(2));

        // 打印已打开设备信息（避免后续排障只能靠枚举列表推断）
        let (vid, pid, bus, addr, serial) = adapter.device_info();
        eprintln!(
            "[Daemon] Opened device: VID:PID={:04x}:{:04x} bus={} addr={} serial={:?}",
            vid, pid, bus, addr, serial
        );

        // 2. 配置设备
        eprintln!(
            "[Daemon] Configuring device: bitrate={} bps, mode=NORMAL|HW_TIMESTAMP",
            config.bitrate
        );
        adapter
            .configure(config.bitrate)
            .map_err(|e| DaemonError::DeviceConfig(format!("{}", e)))?;

        eprintln!("[Daemon] Device configured and started successfully");

        // 设置 USB STALL 计数回调（必须在 split() 之前）
        let stats_for_stall = Arc::clone(&stats);
        adapter.set_stall_count_callback(move || {
            let stats_guard = stats_for_stall.read().unwrap();
            let detailed = stats_guard.detailed.read().unwrap();
            detailed.usb_stall_count.fetch_add(1, Ordering::Relaxed);
        });

        // 分离为 RX 和 TX adapter
        let (rx_adapter, tx_adapter) = adapter
            .split()
            .map_err(|e| DaemonError::DeviceInit(format!("Failed to split adapter: {}", e)))?;

        // 注意：Bus Off 和 Error Passive 回调在 device_manager_loop 中设置
        // 因为在设备重连时需要重新设置回调，且需要访问 stats
        // 详见 device_manager_loop 中的回调设置代码

        eprintln!("[Daemon] Adapter split into RX and TX adapters");
        Ok((rx_adapter, tx_adapter))
    }

    /// 设备管理循环（状态机 + 热拔插恢复）
    ///
    /// **关键**：无论 USB 发生什么错误，守护进程都不应退出，而是进入重连模式。
    ///
    /// **去抖动机制**：在进入 `Reconnecting` 状态前，增加冷却时间，避免 macOS USB 枚举抖动。
    ///
    /// 使用分离的 RX 和 TX adapter
    fn device_manager_loop(
        rx_adapter: Arc<Mutex<Option<GsUsbRxAdapter>>>,
        tx_adapter: Arc<Mutex<Option<GsUsbTxAdapter>>>,
        device_state: Arc<RwLock<DeviceState>>,
        stats: Arc<RwLock<DaemonStats>>,
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

                    // 进入重连状态前，先清空 adapters
                    {
                        let mut state_guard = device_state.write().unwrap();
                        {
                            let mut rx_guard = rx_adapter.lock().unwrap();
                            let mut tx_guard = tx_adapter.lock().unwrap();

                            *rx_guard = None;
                            *tx_guard = None;
                        }
                        *state_guard = DeviceState::Reconnecting;
                        eprintln!("[DeviceManager] Entering reconnecting state");
                    }
                },
                DeviceState::Reconnecting => {
                    // 尝试连接设备并原子性更新 RX/TX adapter
                    match Self::try_connect_device(&config, Arc::clone(&stats)) {
                        Ok((mut new_rx_adapter, new_tx_adapter)) => {
                            // 设备重连成功后，重置 Bus Off 状态标志
                            // 设备重连成功后，重置 Error Passive 状态标志
                            // 重置设备支持标志位（连接新设备后重新检测）
                            {
                                let stats_guard = stats.read().unwrap();
                                let detailed = stats_guard.detailed.read().unwrap();
                                detailed.reset_bus_off_status();
                                detailed.reset_error_passive_status();
                                // ✅ 重置支持标志位为 true（乐观策略），现代 GS-USB 设备普遍支持
                                detailed
                                    .bus_off_monitoring_supported
                                    .store(true, Ordering::Relaxed);
                            }

                            // 设置 Bus Off 状态更新回调（同时标记设备支持检测）
                            let stats_for_bus_off = Arc::clone(&stats);
                            new_rx_adapter.set_bus_off_callback(move |is_bus_off| {
                                let stats_guard = stats_for_bus_off.read().unwrap();
                                let detailed = stats_guard.detailed.read().unwrap();
                                // 检测到 Bus Off 回调，说明设备支持检测（已通过错误帧验证）
                                detailed
                                    .bus_off_monitoring_supported
                                    .store(true, Ordering::Relaxed);
                                detailed.update_bus_off_status(is_bus_off);
                            });

                            // 设置 Error Passive 状态更新回调（同时标记设备支持检测）
                            let stats_for_error_passive = Arc::clone(&stats);
                            new_rx_adapter.set_error_passive_callback(move |is_error_passive| {
                                let stats_guard = stats_for_error_passive.read().unwrap();
                                let detailed = stats_guard.detailed.read().unwrap();
                                // 检测到 Error Passive 回调，说明设备支持错误帧上报（进而支持 Bus Off 检测）
                                detailed
                                    .bus_off_monitoring_supported
                                    .store(true, Ordering::Relaxed);
                                detailed.update_error_passive_status(is_error_passive);
                            });

                            // 乐观策略：默认假设设备支持 Bus Off 检测（现代 GS-USB 设备普遍支持）
                            // 只有在明确检测到不支持时才降级

                            // 原子性更新（在 device_state 写锁保护下）
                            let mut state_guard = device_state.write().unwrap();
                            {
                                // ✅ 锁顺序：device_state → rx_adapter → tx_adapter（防死锁）
                                let mut rx_guard = rx_adapter.lock().unwrap();
                                let mut tx_guard = tx_adapter.lock().unwrap();

                                *rx_guard = Some(new_rx_adapter);
                                *tx_guard = Some(new_tx_adapter);

                                eprintln!("[DeviceManager] Updated RX and TX adapters");
                            } // ✅ 释放 adapter 锁

                            *state_guard = DeviceState::Connected;
                            eprintln!("[DeviceManager] Device reconnected successfully");
                            last_disconnect_time = None; // 重置去抖动计时器
                        },
                        Err(e) => {
                            eprintln!(
                                "[DeviceManager] Failed to connect device: {}. Retrying in {:?}...",
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
    ///
    /// 使用 RX adapter，锁粒度最小化
    fn usb_receive_loop(
        rx_adapter: Arc<Mutex<Option<GsUsbRxAdapter>>>,
        device_state: Arc<RwLock<DeviceState>>,
        clients: Arc<RwLock<ClientManager>>,
        #[cfg(unix)] socket_uds: Option<std::os::unix::net::UnixDatagram>,
        #[cfg(not(unix))] _socket_uds: Option<()>,
        socket_udp: Option<std::net::UdpSocket>,
        stats: Arc<RwLock<DaemonStats>>,
    ) {
        loop {
            // 1. 检查设备状态（快速检查，不要阻塞）
            if *device_state.read().unwrap() != DeviceState::Connected {
                thread::sleep(Duration::from_millis(100));
                continue;
            }

            // 2. 从 USB 设备读取 CAN 帧（锁粒度最小化）
            let frame = {
                let mut adapter_guard = rx_adapter.lock().unwrap();
                match adapter_guard.as_mut() {
                    Some(adapter) => match adapter.receive() {
                        Ok(f) => {
                            // 更新统计信息
                            stats.read().unwrap().increment_rx();
                            f
                        },
                        Err(piper_sdk::can::CanError::Timeout) => {
                            // 超时是正常的（receive 内部有超时设置），继续循环
                            continue;
                        },
                        Err(e) => {
                            // 记录 USB 错误统计
                            {
                                let stats_guard = stats.read().unwrap();
                                let detailed = stats_guard.detailed.read().unwrap();
                                detailed.usb_transfer_errors.fetch_add(1, Ordering::Relaxed);

                                match &e {
                                    piper_sdk::can::CanError::Timeout => {
                                        detailed.usb_timeout_count.fetch_add(1, Ordering::Relaxed);
                                    },
                                    piper_sdk::can::CanError::Device(dev) => {
                                        if dev.kind == piper_sdk::can::CanDeviceErrorKind::NoDevice
                                        {
                                            detailed
                                                .usb_no_device_count
                                                .fetch_add(1, Ordering::Relaxed);
                                        }
                                    },
                                    _ => {},
                                }
                            }

                            // 其他错误：根据错误类型决定是否立即进入断开/重连
                            eprintln!("[Daemon] USB receive error: {:?}", e);

                            let should_disconnect = match &e {
                                piper_sdk::can::CanError::Device(dev) => matches!(
                                    dev.kind,
                                    piper_sdk::can::CanDeviceErrorKind::NoDevice
                                        | piper_sdk::can::CanDeviceErrorKind::NotFound
                                        | piper_sdk::can::CanDeviceErrorKind::AccessDenied
                                ),
                                // IO 错误通常也意味着链路不可靠，进入重连更安全
                                piper_sdk::can::CanError::Io(_) => true,
                                _ => true,
                            };

                            if should_disconnect {
                                *device_state.write().unwrap() = DeviceState::Disconnected;
                            }

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
            }; // ✅ 锁在这里释放（只在 receive() 期间持有）

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

                    // 根据客户端地址类型发送，处理 WouldBlock/ENOBUFS/EPIPE 等错误
                    // 自适应降级机制
                    let send_failed = match &client.addr {
                        #[cfg(unix)]
                        ClientAddr::Unix(uds_path) => {
                            if let Some(ref socket) = socket_uds {
                                // 检查是否需要降级发送（跳过某些帧）
                                let frequency_level =
                                    client.send_frequency_level.load(Ordering::Relaxed);
                                if frequency_level > 0 {
                                    // 降级发送：根据级别跳过帧
                                    // level 1: 每 10 帧发送 1 帧（100Hz）
                                    // level 2: 每 100 帧发送 1 帧（10Hz）
                                    let skip_factor = match frequency_level {
                                        1 => 10,
                                        2 => 100,
                                        _ => 1,
                                    };
                                    let should_skip = {
                                        // 使用 consecutive_errors 作为计数器（临时）
                                        let counter =
                                            client.consecutive_errors.load(Ordering::Relaxed);
                                        counter % skip_factor != 0
                                    };
                                    if should_skip {
                                        // 跳过此帧（降级发送）
                                        continue;
                                    }
                                }

                                match socket.send_to(encoded, uds_path) {
                                    Ok(_) => {
                                        // ✅ 发送成功，重置错误计数和降级级别
                                        stats.read().unwrap().increment_ipc_sent();
                                        client.consecutive_errors.store(0, Ordering::Relaxed);
                                        client.send_frequency_level.store(0, Ordering::Relaxed); // 恢复正常频率
                                        false
                                    },
                                    Err(e)
                                        if e.kind() == std::io::ErrorKind::NotFound
                                            || e.kind()
                                                == std::io::ErrorKind::ConnectionRefused =>
                                    {
                                        // ✅ 客户端 socket 文件不存在或连接被拒绝（进程已退出）
                                        eprintln!(
                                            "[Client {}] Socket not found or refused, removing immediately",
                                            client.id
                                        );
                                        stats
                                            .read()
                                            .unwrap()
                                            .client_disconnected
                                            .fetch_add(1, Ordering::Relaxed);
                                        true // 立即清理
                                    },
                                    Err(e)
                                        if {
                                            #[cfg(unix)]
                                            {
                                                matches!(e.raw_os_error(), Some(libc::EPIPE))
                                            }
                                            #[cfg(not(unix))]
                                            {
                                                false
                                            }
                                        } =>
                                    {
                                        // ✅ Broken pipe：客户端进程已退出
                                        eprintln!(
                                            "[Client {}] Pipe broken (process exited), removing immediately",
                                            client.id
                                        );
                                        stats
                                            .read()
                                            .unwrap()
                                            .client_disconnected
                                            .fetch_add(1, Ordering::Relaxed);
                                        true // 立即清理
                                    },
                                    Err(e)
                                        if e.kind() == std::io::ErrorKind::WouldBlock || {
                                            #[cfg(unix)]
                                            {
                                                matches!(e.raw_os_error(), Some(libc::ENOBUFS))
                                            }
                                            #[cfg(not(unix))]
                                            {
                                                false
                                            }
                                        } =>
                                    {
                                        // WouldBlock 或 ENOBUFS（缓冲区满）
                                        let error_count = client
                                            .consecutive_errors
                                            .fetch_add(1, Ordering::Relaxed)
                                            + 1;
                                        stats
                                            .read()
                                            .unwrap()
                                            .client_send_blocked
                                            .fetch_add(1, Ordering::Relaxed);

                                        // 自适应降级
                                        let current_level =
                                            client.send_frequency_level.load(Ordering::Relaxed);
                                        let new_level = if error_count >= 1000 {
                                            // 1000 次错误（1 秒，1kHz）→ 降级到 10Hz
                                            2
                                        } else if error_count >= 100 {
                                            // 100 次错误（100ms）→ 降级到 100Hz
                                            1
                                        } else {
                                            current_level // 保持当前级别
                                        };

                                        if new_level != current_level {
                                            client
                                                .send_frequency_level
                                                .store(new_level, Ordering::Relaxed);
                                            let level_str = match new_level {
                                                1 => "100Hz",
                                                2 => "10Hz",
                                                _ => "1kHz",
                                            };
                                            eprintln!(
                                                "[Client {}] Degraded to {} ({} errors)",
                                                client.id, level_str, error_count
                                            );
                                            {
                                                let stats_guard = stats.read().unwrap();
                                                let detailed = stats_guard.detailed.read().unwrap();
                                                detailed
                                                    .client_degraded
                                                    .fetch_add(1, Ordering::Relaxed);
                                            }
                                        }

                                        // ✅ 日志限频：只在第一次和每 1000 次打印
                                        if error_count == 1 || error_count % 1000 == 0 {
                                            eprintln!(
                                                "[Client {}] Buffer full, dropped {} frames total",
                                                client.id, error_count
                                            );
                                        }

                                        // ✅ 死客户端检测：连续丢包 2000 次（2 秒，1kHz）视为已死
                                        // 注意：降级后实际丢包会更少，所以阈值提高
                                        if error_count >= 2000 {
                                            eprintln!(
                                                "[Client {}] Buffer full for 2s, disconnecting (considered dead)",
                                                client.id
                                            );
                                            stats
                                                .read()
                                                .unwrap()
                                                .client_disconnected
                                                .fetch_add(1, Ordering::Relaxed);
                                            true // 标记为失败，需要清理
                                        } else {
                                            false // 继续尝试
                                        }
                                    },
                                    Err(e) => {
                                        // 其他错误，记录但不立即清理
                                        eprintln!("[Client {}] Send error: {}", client.id, e);
                                        false
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
                                        client.consecutive_errors.store(0, Ordering::Relaxed);
                                        false
                                    },
                                    Err(e) => {
                                        eprintln!("[Client {}] UDP send error: {}", client.id, e);
                                        false
                                    },
                                }
                            } else {
                                false
                            }
                        },
                    };

                    // 如果发送失败，记录需要清理的客户端
                    if send_failed {
                        failed_clients.push(client.id);
                    }
                }
            }

            // ✅ 清理发送失败的客户端（在释放读取锁后）
            if !failed_clients.is_empty() {
                let mut clients_guard = clients.write().unwrap();
                for client_id in failed_clients {
                    eprintln!("[Client {}] Removing disconnected client", client_id);
                    clients_guard.unregister(client_id);
                }
            }
        }
    }

    /// IPC 接收循环（高优先级线程，阻塞 IO）
    ///
    /// **关键**：使用阻塞 IO，数据到达时内核立即唤醒线程（微秒级）
    /// **严禁**：不要使用 sleep 或轮询
    ///
    /// 使用 TX adapter，与 RX 完全隔离
    #[cfg(unix)]
    fn ipc_receive_loop(
        socket: std::os::unix::net::UnixDatagram,
        tx_adapter: Arc<Mutex<Option<GsUsbTxAdapter>>>,
        device_state: Arc<RwLock<DeviceState>>,
        clients: Arc<RwLock<ClientManager>>,
        stats: Arc<RwLock<DaemonStats>>,
    ) {
        // 设置高优先级（macOS QoS）
        // 注意：在非 macOS 平台上这是空操作，可以安全调用
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
                            &tx_adapter,
                            &device_state,
                            &clients,
                            &socket,
                            &stats,
                        );
                    }
                },
                Err(e) => {
                    // ✅ 非阻塞socket：WouldBlock/EAGAIN 是正常情况，不应该作为错误
                    let e: std::io::Error = e;
                    if e.kind() == std::io::ErrorKind::WouldBlock {
                        // 没有数据可读，继续循环（不打印、不sleep，避免日志刷屏）
                        continue;
                    }
                    // 其他错误才打印并sleep
                    eprintln!("IPC Recv Error: {}", e);
                    thread::sleep(Duration::from_millis(100));
                },
            }
        }
    }

    /// UDP IPC 接收循环（高优先级线程）
    ///
    /// 与 `ipc_receive_loop` 类似，但处理 UDP Socket
    /// 注意：UDP 的 `recv_from` 返回 `SocketAddr`（IP 地址），而不是 `UnixSocketAddr`
    fn ipc_receive_loop_udp(
        socket: std::net::UdpSocket,
        tx_adapter: Arc<Mutex<Option<GsUsbTxAdapter>>>,
        device_state: Arc<RwLock<DeviceState>>,
        clients: Arc<RwLock<ClientManager>>,
        stats: Arc<RwLock<DaemonStats>>,
    ) {
        // 设置高优先级（macOS QoS）
        // 注意：在非 macOS 平台上这是空操作，可以安全调用
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

                        // ✅ 关键：传递 SocketAddr（UDP 地址）而不是 UnixSocketAddr
                        Self::handle_ipc_message_udp(
                            msg,
                            client_addr, // ← SocketAddr（UDP 地址）
                            &tx_adapter,
                            &device_state,
                            &clients,
                            &socket, // ← UdpSocket
                            &stats,
                        );
                    }
                },
                Err(e) => {
                    // ✅ 非阻塞socket：WouldBlock/EAGAIN 是正常情况
                    let e: std::io::Error = e;
                    if e.kind() == std::io::ErrorKind::WouldBlock {
                        continue;
                    }
                    // 其他错误才打印并sleep
                    eprintln!("UDP IPC Recv Error: {}", e);
                    thread::sleep(Duration::from_millis(100));
                },
            }
        }
    }

    /// 处理 IPC 消息
    ///
    /// 使用 TX adapter，与 RX 完全隔离
    #[cfg(unix)]
    fn handle_ipc_message(
        msg: piper_sdk::can::gs_usb_udp::protocol::Message,
        client_addr: std::os::unix::net::SocketAddr,
        tx_adapter: &Arc<Mutex<Option<GsUsbTxAdapter>>>,
        _device_state: &Arc<RwLock<DeviceState>>,
        clients: &Arc<RwLock<ClientManager>>,
        socket: &std::os::unix::net::UnixDatagram,
        stats: &Arc<RwLock<DaemonStats>>,
    ) {
        match msg {
            piper_sdk::can::gs_usb_udp::protocol::Message::Heartbeat { client_id } => {
                // 更新客户端活动时间
                if let Err(e) = clients.write().unwrap().update_activity(client_id) {
                    eprintln!("[Client {}] Failed to update activity: {}", client_id, e);
                }
            },
            piper_sdk::can::gs_usb_udp::protocol::Message::Connect { client_id, filters } => {
                // 注册客户端（使用从 recv_from 获取的真实地址）
                // 尝试从 UnixSocketAddr 获取路径（如果可用）
                // 支持自动 ID 分配：client_id = 0 表示自动分配
                let addr_str = match client_addr.as_pathname() {
                    Some(path) => match path.to_str() {
                        Some(s) => s.to_string(),
                        None => {
                            // 对于自动分配，暂时使用临时路径，实际 ID 会从分配结果获取
                            eprintln!(
                                "Warning: Client path contains invalid UTF-8, using fallback"
                            );
                            // ✅ 使用 XDG 合规路径
                            get_temp_socket_path("gs_usb_client_auto.sock")
                        },
                    },
                    None => {
                        eprintln!("Warning: Client address is abstract, using fallback path");
                        // ✅ 使用 XDG 合规路径
                        get_temp_socket_path("gs_usb_client_auto.sock")
                    },
                };

                let addr = ClientAddr::Unix(addr_str.clone());

                // 支持自动 ID 分配：client_id = 0 表示自动分配
                let (actual_id, register_result) = if client_id == 0 {
                    // 自动分配 ID
                    let mut clients_guard = clients.write().unwrap();
                    // 先保存 client_addr 的调试信息，因为后续需要移动它
                    let unix_addr_debug = format!("{:?}", client_addr);
                    let unix_addr = client_addr;
                    match clients_guard.register_auto(addr.clone(), filters.clone()) {
                        Ok(id) => {
                            // 对于自动分配的 UDS 客户端，需要存储 unix_addr
                            clients_guard.set_unix_addr(id, unix_addr);
                            eprintln!(
                                "Client {} auto-assigned and connected from {} (path: {})",
                                id, unix_addr_debug, addr_str
                            );
                            (id, Ok(()))
                        },
                        Err(e) => {
                            eprintln!("[Client] Failed to register (auto): {}", e);
                            (0, Err(e))
                        },
                    }
                } else {
                    // 手动指定 ID（向后兼容）
                    eprintln!(
                        "Client {} connected from {:?} (path: {})",
                        client_id, client_addr, addr_str
                    );
                    let result = clients.write().unwrap().register_with_unix_addr(
                        client_id,
                        addr,
                        client_addr, // 传递所有权（因为 SocketAddr 不实现 Copy/Clone）
                        filters,
                    );
                    (client_id, result)
                };

                // 发送 ConnectAck 消息（包含实际使用的 ID）
                let mut ack_buf = [0u8; 13];
                let status = if register_result.is_ok() {
                    0 // 成功
                } else {
                    1 // 失败（通常是客户端 ID 已存在）
                };
                let encoded_ack = piper_sdk::can::gs_usb_udp::protocol::encode_connect_ack(
                    actual_id, // 使用实际 ID（自动分配或手动指定）
                    status,
                    0, // seq = 0 for ConnectAck
                    &mut ack_buf,
                );

                // 发送 ConnectAck 到客户端
                if let Err(e) = socket.send_to(encoded_ack, &addr_str) {
                    eprintln!("Failed to send ConnectAck to client {}: {}", actual_id, e);
                } else {
                    eprintln!(
                        "Sent ConnectAck to client {} (status: {}) [auto: {}]",
                        actual_id,
                        status,
                        client_id == 0
                    );
                }

                if let Err(e) = register_result {
                    eprintln!("Failed to register client {}: {}", actual_id, e);
                }
            },
            piper_sdk::can::gs_usb_udp::protocol::Message::Disconnect { client_id } => {
                clients.write().unwrap().unregister(client_id);
            },
            piper_sdk::can::gs_usb_udp::protocol::Message::SendFrame { frame, seq: _seq } => {
                // 发送 CAN 帧到 USB 设备（使用 TX adapter）
                let mut adapter_guard = tx_adapter.lock().unwrap();
                if let Some(ref mut adapter_ref) = *adapter_guard {
                    match adapter_ref.send(frame) {
                        Ok(_) => {
                            // 更新统计（USB 发送成功）
                            stats.read().unwrap().increment_tx();
                            // 发送成功，可以发送 SendAck（可选）
                        },
                        Err(e) => {
                            eprintln!("[Client] Failed to send frame: {}", e);
                            // 可以发送 Error 消息回客户端（带 seq）
                        },
                    }
                } else {
                    eprintln!("[Client] TX adapter not available, frame dropped");
                }
            },
            piper_sdk::can::gs_usb_udp::protocol::Message::SetFilter { client_id, filters } => {
                // ✅ SetFilter 消息处理
                let mut clients_guard = clients.write().unwrap();
                match clients_guard.set_filters(client_id, filters.clone()) {
                    Ok(()) => {
                        eprintln!(
                            "[Client {}] Filters updated: {} rules",
                            client_id,
                            filters.len()
                        );
                    },
                    Err(e) => {
                        eprintln!("[Client {}] Failed to set filters: {}", client_id, e);
                    },
                }
                // 可选：发送确认消息给客户端
            },
            piper_sdk::can::gs_usb_udp::protocol::Message::GetStatus => {
                // ✅ 新增：GetStatus 消息处理
                // 按需提取地址字符串（性能优化：仅在此分支内转换）
                let addr_str = match client_addr.as_pathname() {
                    Some(path) => match path.to_str() {
                        Some(s) => s.to_string(),
                        None => get_temp_socket_path("gs_usb_client.sock"), // ✅ XDG 合规
                    },
                    None => get_temp_socket_path("gs_usb_client.sock"), // ✅ XDG 合规
                };

                let clients_guard = clients.read().unwrap();
                let stats_guard = stats.read().unwrap();
                let device_state_guard = _device_state.read().unwrap();
                let detailed_guard = stats_guard.detailed.read().unwrap();

                let rx_fps = stats_guard.get_rx_fps();
                let tx_fps = stats_guard.get_tx_fps();

                // 构建 StatusResponse
                let status = piper_sdk::can::gs_usb_udp::protocol::StatusResponse {
                    device_state: match *device_state_guard {
                        DeviceState::Connected => 1,
                        DeviceState::Disconnected => 0,
                        DeviceState::Reconnecting => 2,
                    },
                    rx_fps_x1000: (rx_fps * 1000.0) as u32,
                    tx_fps_x1000: (tx_fps * 1000.0) as u32,
                    ipc_sent_fps_x1000: (stats_guard.get_ipc_sent_fps() * 1000.0) as u32,
                    ipc_received_fps_x1000: (stats_guard.get_ipc_received_fps() * 1000.0) as u32,
                    health_score: stats_guard.health_score(rx_fps, tx_fps),
                    usb_stall_count: detailed_guard.usb_stall_count.load(Ordering::Relaxed),
                    can_bus_off_count: detailed_guard.can_bus_off_count.load(Ordering::Relaxed),
                    can_error_passive_count: detailed_guard
                        .can_error_passive_count
                        .load(Ordering::Relaxed),
                    cpu_usage_percent: detailed_guard.cpu_usage_percent.load(Ordering::Relaxed)
                        as u8,
                    client_count: clients_guard.count() as u32, // ← 使用 count() 方法
                    client_send_blocked: stats_guard.client_send_blocked.load(Ordering::Relaxed),
                };

                // 编码并发送 StatusResponse 回请求者
                let mut status_buf = [0u8; 64];
                if let Ok(encoded) = piper_sdk::can::gs_usb_udp::protocol::encode_status_response(
                    &status,
                    0, // seq (GetStatus 不需要序列号，使用 0)
                    &mut status_buf,
                ) {
                    // ✅ 关键：发送到请求者（而不是广播给所有客户端）
                    // ✅ 注意：GetStatus 的请求者可能尚未注册，所以必须使用 recv_from 获取的地址
                    if let Err(e) = socket.send_to(encoded, &addr_str) {
                        eprintln!("Failed to send StatusResponse: {}", e);
                    } else {
                        eprintln!("[GetStatus] Sent StatusResponse to {}", addr_str);
                    }
                }
            },
            _ => {
                // ✅ 未知消息类型必须记录（用于调试）
                eprintln!("⚠️  [Unix] Received unsupported message type: {:?}", msg);
            },
        }
    }

    /// 处理 UDP IPC 消息
    ///
    /// 与 `handle_ipc_message` 类似，但：
    /// 1. `client_addr` 是 `SocketAddr`（UDP 地址）而不是 `UnixSocketAddr`
    /// 2. `socket` 是 `UdpSocket` 而不是 `UnixDatagram`
    /// 3. UDP Connect 消息使用 `register()` 而不是 `register_with_unix_addr()`
    fn handle_ipc_message_udp(
        msg: piper_sdk::can::gs_usb_udp::protocol::Message,
        client_addr: std::net::SocketAddr, // ← UDP 地址（SocketAddr）
        tx_adapter: &Arc<Mutex<Option<GsUsbTxAdapter>>>,
        device_state: &Arc<RwLock<DeviceState>>,
        clients: &Arc<RwLock<ClientManager>>,
        socket: &std::net::UdpSocket, // ← UdpSocket
        stats: &Arc<RwLock<DaemonStats>>,
    ) {
        match msg {
            piper_sdk::can::gs_usb_udp::protocol::Message::Heartbeat { client_id } => {
                // 更新客户端活动时间
                if let Err(e) = clients.write().unwrap().update_activity(client_id) {
                    eprintln!(
                        "[UDP Client {}] Failed to update activity: {}",
                        client_id, e
                    );
                }
            },
            piper_sdk::can::gs_usb_udp::protocol::Message::Connect { client_id, filters } => {
                let addr = ClientAddr::Udp(client_addr); // ← 使用 UDP 地址

                // 支持自动 ID 分配：client_id = 0 表示自动分配
                let (actual_id, register_result) = if client_id == 0 {
                    // 自动分配 ID（UDP 推荐模式）
                    match clients.write().unwrap().register_auto(addr, filters.clone()) {
                        Ok(id) => {
                            eprintln!(
                                "Client {} connected via UDP from {} (auto-assigned)",
                                id, client_addr
                            );
                            (id, Ok(()))
                        },
                        Err(e) => {
                            eprintln!("[UDP Client] Failed to register (auto): {}", e);
                            (0, Err(e))
                        },
                    }
                } else {
                    // 手动指定 ID（向后兼容，但不推荐用于 UDP）
                    eprintln!(
                        "Client {} connected via UDP from {} (manual ID)",
                        client_id, client_addr
                    );
                    let result = clients.write().unwrap().register(client_id, addr, filters);
                    (client_id, result)
                };

                // 发送 ConnectAck 消息（包含实际使用的 ID）
                let mut ack_buf = [0u8; 13];
                let status = if register_result.is_ok() {
                    0 // 成功
                } else {
                    1 // 失败（通常是客户端 ID 已存在）
                };
                let encoded_ack = piper_sdk::can::gs_usb_udp::protocol::encode_connect_ack(
                    actual_id, // 使用实际 ID（自动分配或手动指定）
                    status,
                    0, // seq = 0 for ConnectAck
                    &mut ack_buf,
                );

                // 发送 ConnectAck 到客户端（使用 UDP 地址）
                if let Err(e) = socket.send_to(encoded_ack, client_addr) {
                    eprintln!(
                        "Failed to send ConnectAck to UDP client {}: {}",
                        actual_id, e
                    );
                } else {
                    eprintln!(
                        "Sent ConnectAck to UDP client {} (status: {}) [auto: {}]",
                        actual_id,
                        status,
                        client_id == 0
                    );
                }

                if let Err(e) = register_result {
                    eprintln!("Failed to register UDP client {}: {}", actual_id, e);
                }
            },
            piper_sdk::can::gs_usb_udp::protocol::Message::Disconnect { client_id } => {
                clients.write().unwrap().unregister(client_id);
            },
            piper_sdk::can::gs_usb_udp::protocol::Message::SendFrame { frame, seq: _seq } => {
                // ✅ 发送 CAN 帧到 USB 设备（使用 TX adapter）
                let mut adapter_guard = tx_adapter.lock().unwrap();
                if let Some(ref mut adapter_ref) = *adapter_guard {
                    match adapter_ref.send(frame) {
                        Ok(_) => {
                            stats.read().unwrap().increment_tx();
                        },
                        Err(e) => {
                            eprintln!("[UDP Client] Failed to send frame: {}", e);
                        },
                    }
                } else {
                    eprintln!("[UDP Client] TX adapter not available, frame dropped");
                }
            },
            piper_sdk::can::gs_usb_udp::protocol::Message::SetFilter { client_id, filters } => {
                // ✅ SetFilter 消息处理（UDP）
                let mut clients_guard = clients.write().unwrap();
                match clients_guard.set_filters(client_id, filters.clone()) {
                    Ok(()) => {
                        eprintln!(
                            "[UDP Client {}] Filters updated: {} rules",
                            client_id,
                            filters.len()
                        );
                    },
                    Err(e) => {
                        eprintln!("[UDP Client {}] Failed to set filters: {}", client_id, e);
                    },
                }
            },
            piper_sdk::can::gs_usb_udp::protocol::Message::GetStatus => {
                // ✅ GetStatus 消息处理（UDP）
                // UDP 地址可以直接使用 SocketAddr，无需转换字符串

                let clients_guard = clients.read().unwrap();
                let stats_guard = stats.read().unwrap();
                let device_state_guard = device_state.read().unwrap();
                let detailed_guard = stats_guard.detailed.read().unwrap();

                let rx_fps = stats_guard.get_rx_fps();
                let tx_fps = stats_guard.get_tx_fps();

                // 构建 StatusResponse
                let status = piper_sdk::can::gs_usb_udp::protocol::StatusResponse {
                    device_state: match *device_state_guard {
                        DeviceState::Connected => 1,
                        DeviceState::Disconnected => 0,
                        DeviceState::Reconnecting => 2,
                    },
                    rx_fps_x1000: (rx_fps * 1000.0) as u32,
                    tx_fps_x1000: (tx_fps * 1000.0) as u32,
                    ipc_sent_fps_x1000: (stats_guard.get_ipc_sent_fps() * 1000.0) as u32,
                    ipc_received_fps_x1000: (stats_guard.get_ipc_received_fps() * 1000.0) as u32,
                    health_score: stats_guard.health_score(rx_fps, tx_fps),
                    usb_stall_count: detailed_guard.usb_stall_count.load(Ordering::Relaxed),
                    can_bus_off_count: detailed_guard.can_bus_off_count.load(Ordering::Relaxed),
                    can_error_passive_count: detailed_guard
                        .can_error_passive_count
                        .load(Ordering::Relaxed),
                    cpu_usage_percent: detailed_guard.cpu_usage_percent.load(Ordering::Relaxed)
                        as u8,
                    client_count: clients_guard.count() as u32,
                    client_send_blocked: stats_guard.client_send_blocked.load(Ordering::Relaxed),
                };

                // 编码并发送 StatusResponse 回请求者
                let mut status_buf = [0u8; 64];
                if let Ok(encoded) = piper_sdk::can::gs_usb_udp::protocol::encode_status_response(
                    &status,
                    0, // seq (GetStatus 不需要序列号，使用 0)
                    &mut status_buf,
                ) {
                    // ✅ 关键：发送到 UDP 请求者（使用 SocketAddr）
                    if let Err(e) = socket.send_to(encoded, client_addr) {
                        eprintln!("Failed to send StatusResponse to UDP client: {}", e);
                    } else {
                        eprintln!(
                            "[GetStatus] Sent StatusResponse to UDP client {}",
                            client_addr
                        );
                    }
                }
            },
            _ => {
                // ✅ 未知消息类型必须记录（用于调试）
                eprintln!("⚠️  [UDP] Received unsupported message type: {:?}", msg);
            },
        }
    }

    /// 客户端清理循环（低优先级线程）
    ///
    /// 定期清理超时客户端，避免客户端列表无限增长
    fn client_cleanup_loop(clients: Arc<RwLock<ClientManager>>) {
        // 设置低优先级（设备管理线程）
        // 注意：在非 macOS 平台上这是空操作，可以安全调用
        crate::macos_qos::set_low_priority();

        loop {
            // ✅ 优化：先执行清理，再休眠
            // 这样可以立即清理启动时残留的客户端，而不是等待 5 秒
            clients.write().unwrap().cleanup_timeout();
            // 每 5 秒清理一次超时客户端
            thread::sleep(Duration::from_secs(5));
        }
    }

    /// CPU 监控循环（低优先级线程）
    ///
    /// 定期监控 CPU 使用率，用于健康度评分
    ///
    /// ✅ 使用 `sysinfo` crate 实现跨平台 CPU 使用率监控
    fn cpu_monitor_loop(stats: Arc<RwLock<DaemonStats>>) {
        use sysinfo::{CpuRefreshKind, RefreshKind, System};

        // 设置低优先级
        crate::macos_qos::set_low_priority();

        // ✅ 初始化系统信息（仅 CPU）
        let mut sys = System::new_with_specifics(
            RefreshKind::nothing().with_cpu(CpuRefreshKind::everything()),
        );

        loop {
            thread::sleep(Duration::from_secs(1));

            // ✅ 刷新 CPU 使用率
            sys.refresh_cpu_all();
            let cpu_usage = sys.global_cpu_usage(); // f64 (0-100)

            // 存储到统计中
            stats
                .read()
                .unwrap()
                .detailed
                .read()
                .unwrap()
                .cpu_usage_percent
                .store(cpu_usage as u32, Ordering::Relaxed);
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
        // 注意：在非 macOS 平台上这是空操作，可以安全调用
        crate::macos_qos::set_low_priority();

        loop {
            thread::sleep(interval);

            let (client_count, client_ids) = {
                let clients_guard = clients.read().unwrap();
                let ids: Vec<u32> = clients_guard.iter().map(|client| client.id).collect();
                (clients_guard.count(), ids) // ← 使用 count() 方法，更语义化
            };
            let state = *device_state.read().unwrap();

            // 读取统计信息并计算 FPS + 健康度
            // 更新性能基线（必须由定时器触发，确保固定时间间隔）
            let (
                rx_fps,
                tx_fps,
                ipc_sent_fps,
                ipc_received_fps,
                blocked_count,
                disconnected_count,
                health_score,
                usb_errors,
                can_errors,
                cpu_usage,
                bus_off_count,
            ) = {
                let stats_guard = stats.read().unwrap();
                let elapsed = stats_guard.start_time.elapsed();
                let rx_fps = stats_guard.get_rx_fps();
                let tx_fps = stats_guard.get_tx_fps();

                // 更新性能基线（必须在固定时间间隔调用，例如每秒一次）
                {
                    let detailed = stats_guard.detailed.read().unwrap();
                    detailed.update_baseline(rx_fps, tx_fps, elapsed);

                    // 检测性能异常
                    if detailed.is_performance_degraded(rx_fps, tx_fps) {
                        tracing::warn!(
                            "Performance degraded: RX {:.1} fps (baseline: {:.1}), TX {:.1} fps (baseline: {:.1})",
                            rx_fps,
                            detailed.baseline_rx_fps(),
                            tx_fps,
                            detailed.baseline_tx_fps()
                        );
                    }
                }

                let detailed = stats_guard.detailed.read().unwrap();
                (
                    rx_fps,
                    tx_fps,
                    stats_guard.get_ipc_sent_fps(),
                    stats_guard.get_ipc_received_fps(),
                    stats_guard.client_send_blocked.load(Ordering::Relaxed),
                    stats_guard.client_disconnected.load(Ordering::Relaxed),
                    stats_guard.health_score(rx_fps, tx_fps),
                    detailed.usb_transfer_errors.load(Ordering::Relaxed),
                    detailed.can_error_frames.load(Ordering::Relaxed),
                    detailed.cpu_usage_percent.load(Ordering::Relaxed),
                    detailed.can_bus_off_count.load(Ordering::Relaxed), // Bus Off 计数
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

            // 读取基线信息用于显示
            let (baseline_rx, baseline_tx) = {
                let stats_guard = stats.read().unwrap();
                let detailed = stats_guard.detailed.read().unwrap();
                (detailed.baseline_rx_fps(), detailed.baseline_tx_fps())
            };

            eprintln!(
                "[Status] State: {}, Clients: {} {}, RX: {:.1} fps (baseline: {:.1}), TX: {:.1} fps (baseline: {:.1}), IPC→Client: {:.1} fps, IPC←Client: {:.1} fps, Blocked: {}, Disconnected: {}, Health: {}/100, USB Errors: {}, CAN Errors: {}, Bus Off: {}, CPU: {}%",
                state_str,
                client_count,
                client_ids_str,
                rx_fps,
                baseline_rx, // 显示 RX 基线
                tx_fps,
                baseline_tx, // 显示 TX 基线
                ipc_sent_fps,
                ipc_received_fps,
                blocked_count,
                disconnected_count,
                health_score,
                usb_errors,
                can_errors,
                bus_off_count, // Bus Off 计数
                cpu_usage
            );

            // 健康度告警（< 60 分）
            if health_score < 60 {
                eprintln!(
                    "⚠️  [Health Alert] Daemon health critical: {}/100",
                    health_score
                );
            }

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

        // 启动设备管理线程（状态机 + 热拔插恢复）
        let rx_adapter_clone = Arc::clone(&self.rx_adapter);
        let tx_adapter_clone = Arc::clone(&self.tx_adapter);
        let device_state_clone = Arc::clone(&self.device_state);
        let stats_clone = Arc::clone(&self.stats);
        let config_clone = self.config.clone();

        thread::Builder::new()
            .name("device_manager".into())
            .spawn(move || {
                Self::device_manager_loop(
                    rx_adapter_clone,
                    tx_adapter_clone,
                    device_state_clone,
                    stats_clone,
                    config_clone,
                );
            })
            .map_err(|e| {
                DaemonError::Io(format!("Failed to spawn device manager thread: {}", e))
            })?;

        // 启动 USB 接收线程（从 USB 设备读取 CAN 帧）
        let rx_adapter_clone = Arc::clone(&self.rx_adapter);
        let device_state_clone = Arc::clone(&self.device_state);
        let clients_clone = Arc::clone(&self.clients);
        #[cfg(unix)]
        let socket_uds_clone = self
            .socket_uds
            .as_ref()
            .and_then(|s: &std::os::unix::net::UnixDatagram| s.try_clone().ok());
        #[cfg(not(unix))]
        let socket_uds_clone = None;
        let socket_udp_clone = self.socket_udp.as_ref().and_then(|s| s.try_clone().ok());
        let stats_clone = Arc::clone(&self.stats);

        thread::Builder::new()
            .name("usb_receive".into())
            .spawn(move || {
                Self::usb_receive_loop(
                    rx_adapter_clone,
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

        // 启动 IPC 接收线程（处理客户端消息）
        #[cfg(unix)]
        if let Some(socket_uds) = self.socket_uds.take() {
            let tx_adapter_clone = Arc::clone(&self.tx_adapter);
            let device_state_clone = Arc::clone(&self.device_state);
            let clients_clone = Arc::clone(&self.clients);
            let stats_clone = Arc::clone(&self.stats);

            thread::Builder::new()
                .name("ipc_receive_uds".into())
                .spawn(move || {
                    Self::ipc_receive_loop(
                        socket_uds,
                        tx_adapter_clone,
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
        if let Some(socket_udp) = self.socket_udp.take() {
            let tx_adapter_clone = Arc::clone(&self.tx_adapter);
            let device_state_clone = Arc::clone(&self.device_state);
            let clients_clone = Arc::clone(&self.clients);
            let stats_clone = Arc::clone(&self.stats);

            thread::Builder::new()
                .name("ipc_receive_udp".into())
                .spawn(move || {
                    Self::ipc_receive_loop_udp(
                        socket_udp,
                        tx_adapter_clone,
                        device_state_clone,
                        clients_clone,
                        stats_clone,
                    );
                })
                .map_err(|e| {
                    DaemonError::Io(format!("Failed to spawn UDP IPC receive thread: {}", e))
                })?;

            eprintln!("UDP IPC receive thread started");
        }

        // 启动 CPU 监控线程
        let stats_clone_for_cpu = Arc::clone(&self.stats);
        thread::Builder::new()
            .name("cpu_monitor".into())
            .spawn(move || {
                Self::cpu_monitor_loop(stats_clone_for_cpu);
            })
            .map_err(|e| DaemonError::Io(format!("Failed to spawn CPU monitor thread: {}", e)))?;

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
        // 验证 RX 和 TX adapter 初始为 None
        assert!(daemon.rx_adapter.lock().unwrap().is_none());
        assert!(daemon.tx_adapter.lock().unwrap().is_none());
    }
}
