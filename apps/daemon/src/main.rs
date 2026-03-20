//! GS-USB bridge v2 daemon entrypoint.

mod daemon;
mod macos_qos;
mod session_manager;
mod singleton;

use clap::Parser;
use daemon::{Daemon, DaemonConfig};
use singleton::SingletonLock;
use std::process;
use std::time::Duration;
use tracing::{error, info, warn};
use tracing_subscriber::prelude::*;

/// GS-USB bridge daemon.
///
/// This daemon is a non-realtime bridge/debug service over UDS/TCP.
/// It must not be used as the MIT / dual-arm / fault-stop main control path.
#[derive(Parser, Debug)]
#[command(name = "gs_usb_daemon")]
#[command(
    about = "GS-USB daemon for non-realtime bridge/debug access via UDS/TCP",
    long_about = None
)]
struct Args {
    /// TCP listen address.
    ///
    /// Example: 127.0.0.1:18888
    /// On Unix, the default transport is UDS and TCP is disabled unless this is set.
    /// On non-Unix platforms, TCP defaults to 127.0.0.1:18888.
    #[arg(long)]
    tcp: Option<String>,

    /// Unix stream socket path.
    ///
    /// Example: /tmp/gs_usb_daemon.sock
    /// On Unix, defaults to /tmp/gs_usb_daemon.sock.
    #[arg(long)]
    uds: Option<String>,

    /// CAN bitrate in bps.
    #[arg(long, default_value = "1000000")]
    bitrate: u32,

    /// GS-USB device serial number.
    #[arg(long)]
    serial: Option<String>,

    /// Lock file path.
    #[arg(long)]
    lock_file: Option<String>,

    /// Reconnect interval in seconds.
    #[arg(long, default_value = "1")]
    reconnect_interval: u64,

    /// Reconnect debounce in milliseconds.
    #[arg(long, default_value = "500")]
    reconnect_debounce: u64,

    /// bridge/debug send timeout in milliseconds.
    #[arg(long, default_value = "100")]
    bridge_tx_timeout_ms: u64,
}

fn get_default_lock_file() -> String {
    if let Some(runtime_dir) = dirs::runtime_dir() {
        let path = runtime_dir.join("gs_usb_daemon.lock");
        if let Some(parent) = path.parent()
            && (parent.exists() || std::fs::create_dir_all(parent).is_ok())
        {
            return path.to_string_lossy().to_string();
        }
    }

    let temp_path = std::env::temp_dir().join("gs_usb_daemon.lock");
    if temp_path.parent().map(|p| p.exists()).unwrap_or(false) {
        return temp_path.to_string_lossy().to_string();
    }

    if let Some(cache_dir) = dirs::cache_dir() {
        let piper_cache = cache_dir.join("piper");
        if std::fs::create_dir_all(&piper_cache).is_ok() {
            let path = piper_cache.join("gs_usb_daemon.lock");
            return path.to_string_lossy().to_string();
        }
    }

    std::env::temp_dir().join("gs_usb_daemon.lock").to_string_lossy().to_string()
}

fn init_logging() {
    use tracing_subscriber::fmt;

    let log_dir = if let Some(cache_dir) = dirs::cache_dir() {
        cache_dir.join("piper").join("logs")
    } else {
        std::env::temp_dir().join("piper").join("logs")
    };

    if let Some(parent) = log_dir.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let file_appender = tracing_appender::rolling::daily(&log_dir, "gs_usb_daemon.log");
    let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);

    let env_filter = tracing_subscriber::EnvFilter::from_default_env()
        .add_directive("gs_usb_daemon=info".parse().unwrap())
        .add_directive("piper_driver=warn".parse().unwrap())
        .add_directive("piper_can=warn".parse().unwrap())
        .add_directive("piper_protocol=warn".parse().unwrap());

    tracing_subscriber::registry()
        .with(env_filter)
        .with(fmt::layer().with_target(false).compact())
        .with(fmt::layer().with_writer(non_blocking).with_target(true).with_thread_ids(true))
        .init();
}

fn main() {
    init_logging();

    let mut args = Args::parse();
    let lock_file = args.lock_file.take().unwrap_or_else(get_default_lock_file);

    let _lock = match SingletonLock::try_lock(&lock_file) {
        Ok(lock) => lock,
        Err(err) => {
            error!("failed to acquire singleton lock: {err}");
            error!("another instance of gs_usb_daemon may be running");
            error!("lock file: {lock_file}");
            process::exit(1);
        },
    };

    let uds_path = {
        #[cfg(unix)]
        {
            args.uds.clone().or_else(|| Some("/tmp/gs_usb_daemon.sock".to_string()))
        }
        #[cfg(not(unix))]
        {
            None
        }
    };

    let tcp_addr = {
        #[cfg(unix)]
        {
            args.tcp.clone()
        }
        #[cfg(not(unix))]
        {
            args.tcp.clone().or_else(|| Some("127.0.0.1:18888".to_string()))
        }
    };

    if uds_path.is_none() && tcp_addr.is_none() {
        error!("at least one bridge endpoint must be enabled via --uds or --tcp");
        process::exit(1);
    }

    let uds_path_for_cleanup = uds_path.clone();
    ctrlc::set_handler(move || {
        warn!("received interrupt signal, shutting down");
        #[cfg(unix)]
        if let Some(ref path) = uds_path_for_cleanup
            && std::path::Path::new(path).exists()
            && let Err(err) = std::fs::remove_file(path)
        {
            warn!("failed to remove UDS socket file {}: {}", path, err);
        }
        process::exit(0);
    })
    .expect("failed to set signal handler");

    let config = DaemonConfig {
        uds_path,
        tcp_addr,
        bitrate: args.bitrate,
        serial_number: args.serial.clone(),
        reconnect_interval: Duration::from_secs(args.reconnect_interval),
        reconnect_debounce: Duration::from_millis(args.reconnect_debounce),
        bridge_tx_timeout: Duration::from_millis(args.bridge_tx_timeout_ms),
    };

    info!("GS-USB bridge daemon starting");
    if let Some(ref uds) = config.uds_path {
        info!("UDS listener: {}", uds);
    }
    if let Some(ref tcp) = config.tcp_addr {
        info!("TCP listener: {}", tcp);
    }
    info!("bitrate: {} bps", config.bitrate);
    if let Some(ref serial) = config.serial_number {
        info!("serial: {}", serial);
    }
    info!("lock file: {}", lock_file);

    let mut daemon = match Daemon::new(config) {
        Ok(daemon) => daemon,
        Err(err) => {
            error!("failed to create daemon: {err}");
            process::exit(1);
        },
    };

    info!("GS-USB bridge daemon started. Press Ctrl+C to stop.");
    if let Err(err) = daemon.run() {
        error!("daemon error: {err}");
        process::exit(1);
    }
}
