//! GS-USB 守护进程主入口
//!
//! 参考：`daemon_implementation_plan.md`

mod client_manager;
mod daemon;
mod macos_qos;
mod singleton;

use clap::Parser;
use daemon::{Daemon, DaemonConfig};
use singleton::SingletonLock;
use std::process;
use std::time::Duration;
use tracing::{error, info, warn};
use tracing_subscriber::prelude::*;

/// GS-USB 守护进程
///
/// 用户态守护进程，始终保持与 GS-USB 设备的连接，通过 UDS/UDP 向客户端提供 CAN 总线访问
#[derive(Parser, Debug)]
#[command(name = "gs_usb_daemon")]
#[command(about = "GS-USB Daemon - Persistent CAN bus access via UDS/UDP", long_about = None)]
struct Args {
    /// UDP 监听地址（默认传输方式）
    ///
    /// 格式: IP:PORT (例如: 127.0.0.1:18888)
    /// 默认: 127.0.0.1:18888
    #[arg(long, default_value = "127.0.0.1:18888")]
    udp: String,

    /// UDS Socket 路径（Unix Domain Socket，可选）
    ///
    /// 默认: 不使用 UDS（仅使用 UDP）
    #[arg(long)]
    uds: Option<String>,

    /// CAN 波特率（bps）
    ///
    /// 默认: 1000000 (1Mbps)
    #[arg(long, default_value = "1000000")]
    bitrate: u32,

    /// 设备序列号（可选，用于多设备场景）
    ///
    /// 如果不指定，自动选择第一个找到的设备
    #[arg(long)]
    serial: Option<String>,

    /// 锁文件路径
    ///
    /// 默认: 自动选择用户可写目录（XDG_RUNTIME_DIR 或 /tmp）
    /// 非 root 用户无法在 /var/run 创建文件，建议使用默认值
    #[arg(long)]
    lock_file: Option<String>,

    /// 重连间隔（秒）
    ///
    /// 默认: 1
    #[arg(long, default_value = "1")]
    reconnect_interval: u64,

    /// 重连去抖动时间（毫秒）
    ///
    /// 默认: 500
    #[arg(long, default_value = "500")]
    reconnect_debounce: u64,

    /// 客户端超时时间（秒）
    ///
    /// 默认: 30
    #[arg(long, default_value = "30")]
    client_timeout: u64,
}

/// 获取默认锁文件路径
///
/// 优先使用用户可写的目录，避免权限问题：
/// 1. XDG_RUNTIME_DIR（Linux，通常为 /run/user/{uid}）
/// 2. 系统临时目录（跨平台）
/// 3. 用户主目录下的 .cache/piper 目录（最后备选）
///
/// 使用 `dirs` crate 确保跨平台兼容性和 XDG 规范合规性
fn get_default_lock_file() -> String {
    // ✅ 优先使用 XDG_RUNTIME_DIR（Linux/macOS，符合 XDG 规范）
    if let Some(runtime_dir) = dirs::runtime_dir() {
        let path = runtime_dir.join("gs_usb_daemon.lock");
        // 确保目录存在且可写
        if let Some(parent) = path.parent()
            && (parent.exists() || std::fs::create_dir_all(parent).is_ok())
        {
            return path.to_string_lossy().to_string();
        }
    }

    // ✅ 其次使用系统临时目录（跨平台）
    // - Linux/macOS: /tmp 或 $TMPDIR
    // - Windows: %TEMP%
    let temp_dir = std::env::temp_dir();
    let temp_path = temp_dir.join("gs_usb_daemon.lock");
    if temp_path.parent().map(|p| p.exists()).unwrap_or(false) {
        return temp_path.to_string_lossy().to_string();
    }

    // ✅ 最后备选：用户缓存目录（跨平台）
    // - Linux: ~/.cache/
    // - macOS: ~/Library/Caches/
    // - Windows: %LOCALAPPDATA%
    if let Some(cache_dir) = dirs::cache_dir() {
        let piper_cache = cache_dir.join("piper");
        if std::fs::create_dir_all(&piper_cache).is_ok() {
            let path = piper_cache.join("gs_usb_daemon.lock");
            return path.to_string_lossy().to_string();
        }
    }

    // ❌ 最后回退：使用系统临时目录（可能失败，但至少给出一个路径）
    std::env::temp_dir().join("gs_usb_daemon.lock").to_string_lossy().to_string()
}

/// 初始化日志系统
///
/// ## 配置说明
///
/// - **终端输出**: compact 格式，隐藏 target，仅显示重要日志
/// - **文件输出**: JSON 格式，每日轮转，保留 7 天
/// - **默认级别**: info（可通过 RUST_LOG 环境变量覆盖）
///
/// ## 日志级别策略
///
/// - `gs_usb_daemon=info`: 守护进程自身的日志
/// - `piper_driver=warn`: 驱动层仅警告和错误
/// - `piper_can=warn`: CAN 层仅警告和错误
/// - `piper_protocol=warn`: 协议层仅警告和错误
///
/// ## 使用示例
///
/// ```bash
/// # 默认级别
/// cargo run --bin gs_usb_daemon
///
/// # 启用详细调试日志
/// RUST_LOG=debug cargo run --bin gs_usb_daemon
///
/// # 仅启用特定模块的 trace 日志
/// RUST_LOG=gs_usb_daemon=trace,piper_driver=info cargo run --bin gs_usb_daemon
/// ```
fn init_logging() {
    use tracing_subscriber::fmt;

    // 确定日志目录（优先使用 XDG_CACHE_DIR 或系统临时目录）
    let log_dir = if let Some(cache_dir) = dirs::cache_dir() {
        cache_dir.join("piper").join("logs")
    } else {
        std::env::temp_dir().join("piper").join("logs")
    };

    // 创建日志目录（如果不存在）
    if let Some(parent) = log_dir.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    // 非阻塞文件日志（每日轮转，保留 7 天）
    let file_appender = tracing_appender::rolling::daily(&log_dir, "gs_usb_daemon.log");
    let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);

    // 从环境变量读取日志级别，默认为 info
    let env_filter = tracing_subscriber::EnvFilter::from_default_env()
        .add_directive("gs_usb_daemon=info".parse().unwrap())
        .add_directive("piper_driver=warn".parse().unwrap())
        .add_directive("piper_can=warn".parse().unwrap())
        .add_directive("piper_protocol=warn".parse().unwrap());

    // 组合多个 subscriber layer
    tracing_subscriber::registry()
        .with(env_filter)
        // 终端输出：compact 格式，无 target，易读
        .with(
            fmt::layer()
                .with_target(false)
                .compact()
        )
        // 文件输出：完整格式，用于调试和审计
        .with(
            fmt::layer()
                .with_writer(non_blocking)
                .with_target(true)
                .with_thread_ids(true)
        )
        .init();

    // 日志初始化信息（延迟打印，避免被 tracing 初始化前的输出干扰）
    // 注意：此时尚未连接到设备，仅打印日志路径信息
    // 实际的启动信息在 main() 中打印
}

fn main() {
    // ============================================================
    // 初始化日志（必须在所有其他操作之前）
    // ============================================================
    init_logging();

    // 解析命令行参数
    let mut args = Args::parse();

    // 如果没有指定锁文件路径，使用智能默认值
    let lock_file = args.lock_file.take().unwrap_or_else(get_default_lock_file);

    // 1. 尝试获取单例锁（确保只有一个守护进程实例）
    let _lock = match SingletonLock::try_lock(&lock_file) {
        Ok(lock) => lock,
        Err(e) => {
            error!("Failed to acquire singleton lock: {}", e);
            error!("Another instance of gs_usb_daemon may be running.");
            error!("Lock file: {}", lock_file);
            process::exit(1);
        },
    };

    // 2. 设置信号处理（Ctrl+C 优雅退出）
    let uds_path_for_cleanup = args.uds.clone();
    ctrlc::set_handler(move || {
        warn!("Received interrupt signal. Shutting down...");
        // 清理 UDS socket 文件（如果使用了 UDS）
        if let Some(ref uds_path) = uds_path_for_cleanup
            && std::path::Path::new(uds_path).exists()
            && let Err(e) = std::fs::remove_file(uds_path)
        {
            warn!("Failed to remove UDS socket file {}: {}", uds_path, e);
        }
        process::exit(0);
    })
    .expect("Failed to set signal handler");

    // 3. 创建守护进程配置
    let config = DaemonConfig {
        uds_path: args.uds.clone(),
        udp_addr: Some(args.udp.clone()),
        bitrate: args.bitrate,
        serial_number: args.serial.clone(),
        reconnect_interval: Duration::from_secs(args.reconnect_interval),
        reconnect_debounce: Duration::from_millis(args.reconnect_debounce),
        client_timeout: Duration::from_secs(args.client_timeout),
    };

    // 打印启动信息
    info!("GS-USB Daemon starting...");
    info!("UDP: {} (default)", args.udp);
    if let Some(ref uds) = args.uds {
        info!("UDS: {} (optional)", uds);
    }
    info!("Bitrate: {} bps", args.bitrate);
    if let Some(ref serial) = args.serial {
        info!("Serial: {}", serial);
    }
    info!("Lock file: {}", lock_file);

    // 4. 创建守护进程实例
    let mut daemon = match Daemon::new(config) {
        Ok(d) => d,
        Err(e) => {
            error!("Failed to create daemon: {}", e);
            process::exit(1);
        },
    };

    // 5. 启动守护进程（阻塞直到退出）
    info!("GS-USB Daemon started. Press Ctrl+C to stop.");
    if let Err(e) = daemon.run() {
        error!("Daemon error: {}", e);
        process::exit(1);
    }
}
