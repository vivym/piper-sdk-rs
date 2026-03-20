//! Controller-owned bridge host entrypoint.

mod daemon;
mod singleton;

use clap::Parser;
use daemon::{Daemon, DaemonConfig};
use singleton::SingletonLock;
use std::process;
use tracing::{error, info, warn};
use tracing_subscriber::prelude::*;

/// Controller-owned bridge host.
///
/// This process remains a non-realtime bridge/debug service over UDS/TCP-TLS.
/// It must not be used as the MIT / dual-arm / fault-stop main control path.
#[derive(Parser, Debug)]
#[command(name = "piper_bridge_host")]
#[command(
    about = "Controller-owned non-realtime bridge/debug host via UDS/TCP-TLS",
    long_about = None
)]
struct Args {
    /// TLS-protected TCP listen address.
    ///
    /// Example: 127.0.0.1:18888
    /// On Unix, the default transport is UDS and TCP/TLS is disabled unless this is set.
    /// On non-Unix platforms, TCP/TLS defaults to 127.0.0.1:18888.
    #[arg(long)]
    tcp_tls: Option<String>,

    /// Unix stream socket path.
    ///
    /// Example: /tmp/piper_bridge.sock
    /// On Unix, defaults to /tmp/piper_bridge.sock.
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

    /// TLS server certificate PEM path for --tcp-tls.
    #[arg(long)]
    tls_server_cert: Option<String>,

    /// TLS server private key PEM path for --tcp-tls.
    #[arg(long)]
    tls_server_key: Option<String>,

    /// Client CA certificate PEM path for --tcp-tls mutual TLS.
    #[arg(long)]
    tls_client_ca: Option<String>,

    /// Allowed client certificate SHA-256 fingerprints in hex.
    #[arg(long = "tls-allow-client-cert-sha256")]
    tls_allowed_client_cert_sha256: Vec<String>,
}

fn get_default_lock_file() -> String {
    if let Some(runtime_dir) = dirs::runtime_dir() {
        let path = runtime_dir.join("piper_bridge_host.lock");
        if let Some(parent) = path.parent()
            && (parent.exists() || std::fs::create_dir_all(parent).is_ok())
        {
            return path.to_string_lossy().to_string();
        }
    }

    let temp_path = std::env::temp_dir().join("piper_bridge_host.lock");
    if temp_path.parent().map(|p| p.exists()).unwrap_or(false) {
        return temp_path.to_string_lossy().to_string();
    }

    if let Some(cache_dir) = dirs::cache_dir() {
        let piper_cache = cache_dir.join("piper");
        if std::fs::create_dir_all(&piper_cache).is_ok() {
            let path = piper_cache.join("piper_bridge_host.lock");
            return path.to_string_lossy().to_string();
        }
    }

    std::env::temp_dir()
        .join("piper_bridge_host.lock")
        .to_string_lossy()
        .to_string()
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

    let file_appender = tracing_appender::rolling::daily(&log_dir, "piper_bridge_host.log");
    let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);

    let env_filter = tracing_subscriber::EnvFilter::from_default_env()
        .add_directive("piper_bridge_host=info".parse().unwrap())
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
            error!("another instance of piper_bridge_host may be running");
            error!("lock file: {lock_file}");
            process::exit(1);
        },
    };

    let uds_path = {
        #[cfg(unix)]
        {
            args.uds.clone().or_else(|| Some("/tmp/piper_bridge.sock".to_string()))
        }
        #[cfg(not(unix))]
        {
            None
        }
    };

    let tcp_tls_addr = {
        #[cfg(unix)]
        {
            args.tcp_tls.clone()
        }
        #[cfg(not(unix))]
        {
            args.tcp_tls.clone().or_else(|| Some("127.0.0.1:18888".to_string()))
        }
    };

    if uds_path.is_none() && tcp_tls_addr.is_none() {
        error!("at least one bridge endpoint must be enabled via --uds or --tcp-tls");
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
        tcp_tls_addr,
        tls_server_cert: args.tls_server_cert.clone(),
        tls_server_key: args.tls_server_key.clone(),
        tls_client_ca: args.tls_client_ca.clone(),
        tls_allowed_client_cert_sha256: args.tls_allowed_client_cert_sha256.clone(),
        bitrate: args.bitrate,
        serial_number: args.serial.clone(),
    };

    info!("controller-owned bridge host starting");
    if let Some(ref uds) = config.uds_path {
        info!("UDS listener: {}", uds);
    }
    if let Some(ref tcp) = config.tcp_tls_addr {
        info!("TCP/TLS listener: {}", tcp);
    }
    info!("bitrate: {} bps", config.bitrate);
    if let Some(ref serial) = config.serial_number {
        info!("serial: {}", serial);
    }
    info!("lock file: {}", lock_file);

    let daemon = match Daemon::new(config) {
        Ok(daemon) => daemon,
        Err(err) => {
            error!("failed to create daemon: {err}");
            process::exit(1);
        },
    };

    info!("bridge host started. Press Ctrl+C to stop.");
    if let Err(err) = daemon.run() {
        error!("daemon error: {err}");
        process::exit(1);
    }
}
