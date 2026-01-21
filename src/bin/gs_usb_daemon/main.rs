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
/// 2. /tmp（所有 Unix 系统）
/// 3. 用户主目录下的 .cache/piper 目录（最后备选）
fn get_default_lock_file() -> String {
    // 优先使用 XDG_RUNTIME_DIR（符合 XDG 规范）
    if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
        let path = std::path::Path::new(&runtime_dir).join("gs_usb_daemon.lock");
        // 确保目录存在且可写
        if let Some(parent) = path.parent()
            && (parent.exists() || std::fs::create_dir_all(parent).is_ok())
        {
            return path.to_string_lossy().to_string();
        }
    }

    // 其次使用 /tmp（所有用户都有写权限）
    let tmp_path = std::path::Path::new("/tmp").join("gs_usb_daemon.lock");
    if tmp_path.parent().map(|p| p.exists()).unwrap_or(false) {
        return tmp_path.to_string_lossy().to_string();
    }

    // 最后备选：用户主目录下的 .cache/piper
    if let Ok(home) = std::env::var("HOME") {
        let cache_dir = std::path::Path::new(&home).join(".cache").join("piper");
        if std::fs::create_dir_all(&cache_dir).is_ok() {
            let path = cache_dir.join("gs_usb_daemon.lock");
            return path.to_string_lossy().to_string();
        }
    }

    // 如果都不行，仍然返回 /tmp 路径（虽然可能也会失败）
    "/tmp/gs_usb_daemon.lock".to_string()
}

fn main() {
    // 解析命令行参数
    let mut args = Args::parse();

    // 如果没有指定锁文件路径，使用智能默认值
    let lock_file = args.lock_file.take().unwrap_or_else(get_default_lock_file);

    // 1. 尝试获取单例锁（确保只有一个守护进程实例）
    let _lock = match SingletonLock::try_lock(&lock_file) {
        Ok(lock) => lock,
        Err(e) => {
            eprintln!("Failed to acquire singleton lock: {}", e);
            eprintln!("Another instance of gs_usb_daemon may be running.");
            eprintln!("Lock file: {}", lock_file);
            process::exit(1);
        },
    };

    // 2. 设置信号处理（Ctrl+C 优雅退出）
    let uds_path_for_cleanup = args.uds.clone();
    ctrlc::set_handler(move || {
        eprintln!("\nReceived interrupt signal. Shutting down...");
        // 清理 UDS socket 文件（如果使用了 UDS）
        if let Some(ref uds_path) = uds_path_for_cleanup
            && std::path::Path::new(uds_path).exists()
            && let Err(e) = std::fs::remove_file(uds_path)
        {
            eprintln!(
                "Warning: Failed to remove UDS socket file {}: {}",
                uds_path, e
            );
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
    eprintln!("GS-USB Daemon starting...");
    eprintln!("  UDP: {} (default)", args.udp);
    if let Some(ref uds) = args.uds {
        eprintln!("  UDS: {} (optional)", uds);
    }
    eprintln!("  Bitrate: {} bps", args.bitrate);
    if let Some(ref serial) = args.serial {
        eprintln!("  Serial: {}", serial);
    }
    eprintln!("  Lock file: {}", lock_file);

    // 4. 创建守护进程实例
    let mut daemon = match Daemon::new(config) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("Failed to create daemon: {}", e);
            process::exit(1);
        },
    };

    // 5. 启动守护进程（阻塞直到退出）
    eprintln!("GS-USB Daemon started. Press Ctrl+C to stop.");
    if let Err(e) = daemon.run() {
        eprintln!("Daemon error: {}", e);
        process::exit(1);
    }
}
