//! Controller-owned bridge 调试示例。
//!
//! 该示例通过 UDS/TCP-TLS stream 连接 `piper_bridge_host`，用于 bridge/debug/replay。
//! 它不是 MIT / 双臂 / fault-stop 的实时控制路径。

use clap::Parser;
use piper_sdk::{
    BridgeClientOptions, BridgeEndpoint, BridgeEvent, BridgeRole, BridgeTlsClientConfig,
    PiperBridgeClient, PiperFrame, SessionToken,
};
use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::io::{self, Write};
use std::path::PathBuf;
use std::time::Duration;

#[derive(Parser, Debug)]
#[command(name = "bridge_test")]
#[command(about = "测试 controller-owned Piper bridge (UDS/TCP-TLS stream)")]
struct Args {
    /// Bridge endpoint.
    ///
    /// Unix: /tmp/piper_bridge.sock
    /// TCP: 127.0.0.1:18888
    #[arg(long)]
    endpoint: Option<String>,

    /// Mode: status | send | receive | interactive
    #[arg(long, default_value = "status")]
    mode: String,

    /// Number of frames to send or receive.
    #[arg(long, default_value = "10")]
    count: u32,

    /// Interval between sends in milliseconds.
    #[arg(long, default_value = "100")]
    interval_ms: u64,

    /// Bridge request timeout in milliseconds.
    #[arg(long, default_value = "100")]
    timeout_ms: u64,

    /// TLS CA certificate PEM path for TCP/TLS endpoints.
    #[arg(long)]
    tls_ca: Option<String>,

    /// TLS client certificate PEM path for TCP/TLS endpoints.
    #[arg(long)]
    tls_client_cert: Option<String>,

    /// TLS client private key PEM path for TCP/TLS endpoints.
    #[arg(long)]
    tls_client_key: Option<String>,

    /// TLS server name used for certificate verification on TCP/TLS endpoints.
    #[arg(long)]
    tls_server_name: Option<String>,
}

fn default_endpoint() -> String {
    #[cfg(unix)]
    {
        "/tmp/piper_bridge.sock".to_string()
    }
    #[cfg(not(unix))]
    {
        "127.0.0.1:18888".to_string()
    }
}

fn parse_endpoint(raw: &str) -> Result<BridgeEndpoint, String> {
    if raw.starts_with('/') || raw.starts_with("unix:") {
        #[cfg(unix)]
        {
            let path = raw.strip_prefix("unix:").unwrap_or(raw);
            Ok(BridgeEndpoint::Unix(PathBuf::from(path)))
        }
        #[cfg(not(unix))]
        {
            Err("unix endpoints are not supported on this platform".to_string())
        }
    } else {
        raw.parse()
            .map(BridgeEndpoint::TcpTls)
            .map_err(|err| format!("invalid TCP/TLS endpoint: {err}"))
    }
}

fn maybe_tls_config(
    endpoint: &BridgeEndpoint,
    args: &Args,
) -> Result<Option<BridgeTlsClientConfig>, Box<dyn std::error::Error>> {
    match endpoint {
        BridgeEndpoint::Unix(_) => Ok(None),
        BridgeEndpoint::TcpTls(_) => Ok(Some(BridgeTlsClientConfig {
            ca_cert_pem: PathBuf::from(
                args.tls_ca.clone().ok_or("--tls-ca is required for TCP/TLS bridge endpoints")?,
            ),
            client_cert_pem: PathBuf::from(
                args.tls_client_cert
                    .clone()
                    .ok_or("--tls-client-cert is required for TCP/TLS bridge endpoints")?,
            ),
            client_key_pem: PathBuf::from(
                args.tls_client_key
                    .clone()
                    .ok_or("--tls-client-key is required for TCP/TLS bridge endpoints")?,
            ),
            server_name: args
                .tls_server_name
                .clone()
                .ok_or("--tls-server-name is required for TCP/TLS bridge endpoints")?,
        })),
    }
}

fn token_cache_dir() -> PathBuf {
    if let Some(xdg_cache_home) = std::env::var_os("XDG_CACHE_HOME") {
        PathBuf::from(xdg_cache_home).join("piper-sdk").join("bridge_tokens")
    } else if let Some(home) = std::env::var_os("HOME") {
        PathBuf::from(home).join(".cache").join("piper-sdk").join("bridge_tokens")
    } else {
        std::env::temp_dir().join("piper-sdk-bridge-tokens")
    }
}

fn stable_session_token(endpoint: &str, tool_name: &str) -> Result<SessionToken, io::Error> {
    let mut hasher = DefaultHasher::new();
    tool_name.hash(&mut hasher);
    endpoint.hash(&mut hasher);
    let key = format!("{:016x}", hasher.finish());

    let dir = token_cache_dir();
    fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{key}.token"));

    match fs::read(&path) {
        Ok(bytes) if bytes.len() == 16 => {
            let mut token = [0u8; 16];
            token.copy_from_slice(&bytes);
            Ok(SessionToken::new(token))
        },
        _ => {
            let token = SessionToken::random();
            fs::write(&path, token.as_bytes())?;
            Ok(token)
        },
    }
}

fn connect_client(
    endpoint_raw: &str,
    args: &Args,
    role: BridgeRole,
    timeout: Duration,
) -> Result<PiperBridgeClient, Box<dyn std::error::Error>> {
    let endpoint = parse_endpoint(endpoint_raw)?;
    let token = stable_session_token(endpoint_raw, "bridge_test")?;
    let options = BridgeClientOptions {
        session_token: token,
        role_request: role,
        filters: Vec::new(),
        connect_timeout: Duration::from_secs(5),
        request_timeout: timeout,
        tcp_tls: maybe_tls_config(&endpoint, args)?,
    };
    Ok(PiperBridgeClient::connect(endpoint, options)?)
}

fn print_status(client: &mut PiperBridgeClient) -> Result<(), Box<dyn std::error::Error>> {
    let status = client.get_status()?;
    println!("session_id={}", client.session_id());
    println!("role={:?}", client.role_granted());
    println!("device_state={:?}", status.device_state);
    println!("rx_fps={:.3}", status.rx_fps_x1000 as f64 / 1000.0);
    println!("tx_fps={:.3}", status.tx_fps_x1000 as f64 / 1000.0);
    println!("ipc_in_fps={:.3}", status.ipc_in_fps_x1000 as f64 / 1000.0);
    println!(
        "ipc_out_fps={:.3}",
        status.ipc_out_fps_x1000 as f64 / 1000.0
    );
    println!("health_score={}", status.health_score);
    println!("session_count={}", status.session_count);
    println!("queue_drop_count={}", status.queue_drop_count);
    Ok(())
}

fn run_send(
    client: &mut PiperBridgeClient,
    count: u32,
    interval: Duration,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut lease = client.acquire_maintenance_lease(Duration::from_millis(500))?;
    for index in 0..count {
        let frame = PiperFrame::new_standard(
            0x120 + (index as u16 % 0x10),
            &[
                index as u8,
                (index.wrapping_mul(2)) as u8,
                (index.wrapping_mul(3)) as u8,
                0xAA,
                0xBB,
                0xCC,
                0xDD,
                0xEE,
            ],
        );
        lease.send_frame(frame)?;
        println!(
            "sent #{:03}: id=0x{:03X} data={:02X?}",
            index + 1,
            frame.id,
            &frame.data[..frame.len as usize]
        );
        if index + 1 != count {
            std::thread::sleep(interval);
        }
    }
    lease.release()?;
    Ok(())
}

fn run_receive(
    client: &mut PiperBridgeClient,
    count: u32,
    timeout: Duration,
) -> Result<(), Box<dyn std::error::Error>> {
    for index in 0..count {
        match client.recv_event(timeout)? {
            BridgeEvent::ReceiveFrame(frame) => {
                println!(
                    "recv #{:03}: id=0x{:03X} len={} data={:02X?} ts_us={}",
                    index + 1,
                    frame.id,
                    frame.len,
                    &frame.data[..frame.len as usize],
                    frame.timestamp_us
                );
            },
            BridgeEvent::Gap { dropped } => {
                println!("gap: dropped {} events", dropped);
            },
            BridgeEvent::SessionReplaced => {
                println!("session replaced by a newer connection");
                break;
            },
            BridgeEvent::MaintenanceLeaseRevoked => {
                println!("maintenance lease revoked");
            },
        }
    }
    Ok(())
}

fn run_interactive(
    client: &mut PiperBridgeClient,
    timeout: Duration,
) -> Result<(), Box<dyn std::error::Error>> {
    println!("Commands: status | send <id-hex> <b0> [b1..b7] | recv | quit");
    let mut line = String::new();
    loop {
        print!("bridge> ");
        io::stdout().flush()?;
        line.clear();
        if io::stdin().read_line(&mut line)? == 0 {
            break;
        }
        let parts: Vec<_> = line.split_whitespace().collect();
        if parts.is_empty() {
            continue;
        }
        match parts[0] {
            "status" => print_status(client)?,
            "recv" => {
                run_receive(client, 1, timeout)?;
            },
            "send" => {
                if parts.len() < 3 {
                    println!("usage: send <id-hex> <b0> [b1..b7]");
                    continue;
                }
                let id = u32::from_str_radix(parts[1].trim_start_matches("0x"), 16)?;
                let mut data = [0u8; 8];
                let mut len = 0u8;
                for (index, token) in parts.iter().skip(2).take(8).enumerate() {
                    data[index] = u8::from_str_radix(token.trim_start_matches("0x"), 16)?;
                    len += 1;
                }
                let frame = PiperFrame {
                    id,
                    data,
                    len,
                    is_extended: false,
                    timestamp_us: 0,
                };
                let mut lease = client.acquire_maintenance_lease(Duration::from_millis(500))?;
                lease.send_frame(frame)?;
                lease.release()?;
                println!("sent frame id=0x{:03X}", id);
            },
            "quit" | "exit" => break,
            other => println!("unknown command: {}", other),
        }
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let endpoint = args.endpoint.clone().unwrap_or_else(default_endpoint);
    let timeout = Duration::from_millis(args.timeout_ms);

    println!("Piper bridge test client");
    println!("endpoint: {}", endpoint);
    println!("non-realtime bridge/debug path only");

    let role = if matches!(args.mode.as_str(), "send" | "interactive") {
        BridgeRole::WriterCandidate
    } else {
        BridgeRole::Observer
    };
    let mut client = connect_client(&endpoint, &args, role, timeout)?;

    match args.mode.as_str() {
        "status" => print_status(&mut client)?,
        "send" => run_send(
            &mut client,
            args.count,
            Duration::from_millis(args.interval_ms),
        )?,
        "receive" => run_receive(&mut client, args.count, timeout)?,
        "interactive" => run_interactive(&mut client, timeout)?,
        other => return Err(format!("unsupported mode: {}", other).into()),
    }

    Ok(())
}
