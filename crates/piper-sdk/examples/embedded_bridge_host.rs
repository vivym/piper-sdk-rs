//! Embedded controller-owned bridge host example.
//!
//! This example keeps hardware ownership inside the controller process and
//! attaches the non-realtime bridge host to that in-process controller surface.
//!
//! - Linux defaults to `socketcan:can0`
//! - Other platforms default to `gs_usb_auto()`
//! - Unix defaults to a UDS listener at `/tmp/piper_bridge.sock`
//! - Non-Unix platforms must pass `--tcp-tls`, `--tls-server-cert`,
//!   `--tls-server-key`, and `--tls-client-ca`

use clap::Parser;
use piper_sdk::{
    BridgeHostConfig, BridgeRole, BridgeTlsClientPolicy, BridgeTlsServerConfig,
    BridgeUdsListenerConfig, ConnectedPiper, MotionConnectedState, PiperBuilder,
};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "embedded_bridge_host")]
#[command(about = "Run the non-realtime bridge host inside the controller process")]
struct Args {
    /// Linux SocketCAN interface, e.g. can0.
    #[arg(long, conflicts_with = "gs_usb_serial")]
    socketcan: Option<String>,

    /// GS-USB serial number.
    #[arg(long, conflicts_with = "socketcan")]
    gs_usb_serial: Option<String>,

    /// CAN bitrate in bps.
    #[arg(long, default_value = "1000000")]
    bitrate: u32,

    /// Unix stream socket path.
    #[cfg_attr(unix, arg(long, default_value = "/tmp/piper_bridge.sock"))]
    #[cfg_attr(not(unix), arg(long))]
    uds: Option<String>,

    /// Granted bridge role for the UDS listener: observer | writer-candidate.
    #[arg(long, default_value = "observer")]
    uds_role: String,

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

fn parse_bridge_role(raw: &str) -> Result<BridgeRole, Box<dyn std::error::Error>> {
    match raw {
        "observer" => Ok(BridgeRole::Observer),
        "writer-candidate" => Ok(BridgeRole::WriterCandidate),
        other => Err(format!(
            "invalid bridge role `{other}`; expected `observer` or `writer-candidate`"
        )
        .into()),
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    piper_sdk::init_logger!();

    let args = Args::parse();
    let uds_role = parse_bridge_role(&args.uds_role)?;

    #[cfg(not(target_os = "linux"))]
    if args.socketcan.is_some() {
        return Err("--socketcan is only supported on Linux".into());
    }
    #[cfg(not(unix))]
    if args.uds.is_some() {
        return Err("--uds is only supported on Unix platforms".into());
    }
    #[cfg(not(unix))]
    if args.tcp_tls.is_none() {
        return Err(
            "non-Unix platforms require --tcp-tls because UDS listeners are unavailable".into(),
        );
    }

    let mut builder = PiperBuilder::new().baud_rate(args.bitrate);
    if let Some(iface) = args.socketcan {
        builder = builder.socketcan(iface);
    } else if let Some(serial) = args.gs_usb_serial {
        builder = builder.gs_usb_serial(serial);
    } else {
        #[cfg(target_os = "linux")]
        {
            builder = builder.socketcan("can0");
        }
        #[cfg(not(target_os = "linux"))]
        {
            builder = builder.gs_usb_auto();
        }
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
            client_policies: args
                .tls_allowed_client_cert_sha256
                .into_iter()
                .map(|fingerprint_sha256| BridgeTlsClientPolicy {
                    fingerprint_sha256,
                    granted_role: BridgeRole::WriterCandidate,
                })
                .collect(),
            handshake_timeout: std::time::Duration::from_secs(5),
        }),
        None => None,
    };

    let host_config = BridgeHostConfig {
        uds: args.uds.map(|path| BridgeUdsListenerConfig {
            path: PathBuf::from(path),
            granted_role: uds_role,
        }),
        tcp_tls,
        allow_raw_frame_tap: args.allow_raw_frame_tap,
    };
    let host = match piper {
        ConnectedPiper::Strict(MotionConnectedState::Standby(piper)) => {
            piper.attach_bridge_host(host_config.clone())
        },
        ConnectedPiper::Strict(MotionConnectedState::Maintenance(piper)) => {
            piper.attach_bridge_host(host_config.clone())
        },
        ConnectedPiper::Soft(MotionConnectedState::Standby(piper)) => {
            piper.attach_bridge_host(host_config.clone())
        },
        ConnectedPiper::Soft(MotionConnectedState::Maintenance(piper)) => {
            piper.attach_bridge_host(host_config.clone())
        },
        ConnectedPiper::Monitor(piper) => piper.attach_bridge_host(host_config),
    };

    tracing::info!("embedded bridge host starting");
    host.run()?;
    Ok(())
}
