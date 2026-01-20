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
    /// UDS Socket 路径（Unix Domain Socket）
    ///
    /// 默认: /tmp/gs_usb_daemon.sock
    #[arg(long, default_value = "/tmp/gs_usb_daemon.sock")]
    uds: String,

    /// UDP 监听地址（可选，用于跨机器调试）
    ///
    /// 格式: IP:PORT (例如: 127.0.0.1:8888)
    #[arg(long)]
    udp: Option<String>,

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
    /// 默认: /var/run/gs_usb_daemon.lock
    #[arg(long, default_value = "/var/run/gs_usb_daemon.lock")]
    lock_file: String,

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

fn main() {
    // 解析命令行参数
    let args = Args::parse();

    // 1. 尝试获取单例锁（确保只有一个守护进程实例）
    let _lock = match SingletonLock::try_lock(&args.lock_file) {
        Ok(lock) => lock,
        Err(e) => {
            eprintln!("Failed to acquire singleton lock: {}", e);
            eprintln!("Another instance of gs_usb_daemon may be running.");
            eprintln!("Lock file: {}", args.lock_file);
            process::exit(1);
        },
    };

    // 2. 设置信号处理（Ctrl+C 优雅退出）
    let uds_path_for_cleanup = args.uds.clone();
    ctrlc::set_handler(move || {
        eprintln!("\nReceived interrupt signal. Shutting down...");
        // 清理 UDS socket 文件
        if std::path::Path::new(&uds_path_for_cleanup).exists()
            && let Err(e) = std::fs::remove_file(&uds_path_for_cleanup)
        {
            eprintln!(
                "Warning: Failed to remove UDS socket file {}: {}",
                uds_path_for_cleanup, e
            );
        }
        process::exit(0);
    })
    .expect("Failed to set signal handler");

    // 3. 创建守护进程配置
    let config = DaemonConfig {
        uds_path: Some(args.uds.clone()),
        udp_addr: args.udp.clone(),
        bitrate: args.bitrate,
        serial_number: args.serial.clone(),
        reconnect_interval: Duration::from_secs(args.reconnect_interval),
        reconnect_debounce: Duration::from_millis(args.reconnect_debounce),
        client_timeout: Duration::from_secs(args.client_timeout),
    };

    // 打印启动信息
    eprintln!("GS-USB Daemon starting...");
    eprintln!("  UDS: {}", args.uds);
    if let Some(ref udp) = args.udp {
        eprintln!("  UDP: {}", udp);
    }
    eprintln!("  Bitrate: {} bps", args.bitrate);
    if let Some(ref serial) = args.serial {
        eprintln!("  Serial: {}", serial);
    }
    eprintln!("  Lock file: {}", args.lock_file);

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
