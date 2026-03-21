//! Embedded controller-owned bridge host example.
//!
//! This example keeps hardware ownership inside the controller process and
//! attaches the non-realtime bridge host to that in-process controller surface.

use clap::Parser;
use piper_sdk::{BridgeHostConfig, BridgeTlsServerConfig, PiperBuilder};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "embedded_bridge_host")]
#[command(about = "Run the non-realtime bridge host inside the controller process")]
struct Args {
    /// Linux SocketCAN interface, e.g. can0.
    #[arg(long)]
    socketcan: Option<String>,

    /// GS-USB serial number.
    #[arg(long)]
    gs_usb_serial: Option<String>,

    /// CAN bitrate in bps.
    #[arg(long, default_value = "1000000")]
    bitrate: u32,

    /// Unix stream socket path.
    #[arg(long, default_value = "/tmp/piper_bridge.sock")]
    uds: String,

    /// TLS-protected TCP listen address.
    #[arg(long)]
    tcp_tls: Option<String>,

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

    /// Explicitly allow raw frame tap subscriptions.
    #[arg(long, default_value_t = false)]
    allow_raw_frame_tap: bool,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    piper_sdk::init_logger!();

    let args = Args::parse();

    let mut builder = PiperBuilder::new().baud_rate(args.bitrate);
    if let Some(iface) = args.socketcan {
        builder = builder.socketcan(iface);
    } else if let Some(serial) = args.gs_usb_serial {
        builder = builder.gs_usb_serial(serial);
    } else {
        builder = builder.gs_usb_auto();
    }

    let piper = builder.build()?;
    let tcp_tls = match args.tcp_tls {
        Some(addr) => Some(BridgeTlsServerConfig {
            listen_addr: addr.parse()?,
            server_cert_pem: PathBuf::from(
                args.tls_server_cert.ok_or("--tcp-tls requires --tls-server-cert")?,
            ),
            server_key_pem: PathBuf::from(
                args.tls_server_key.ok_or("--tcp-tls requires --tls-server-key")?,
            ),
            client_ca_cert_pem: PathBuf::from(
                args.tls_client_ca.ok_or("--tcp-tls requires --tls-client-ca")?,
            ),
            allowed_client_cert_sha256: args.tls_allowed_client_cert_sha256,
            handshake_timeout: std::time::Duration::from_secs(5),
        }),
        None => None,
    };

    let host = piper.attach_bridge_host(BridgeHostConfig {
        uds_path: Some(PathBuf::from(args.uds)),
        tcp_tls,
        maintenance_mode: false,
        allow_raw_frame_tap: args.allow_raw_frame_tap,
    });

    tracing::info!("embedded bridge host starting");
    host.run()?;
    Ok(())
}
