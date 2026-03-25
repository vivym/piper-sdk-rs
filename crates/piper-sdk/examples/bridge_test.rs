//! Controller-owned bridge 调试示例。
//!
//! 该示例通过 UDS/TCP-TLS stream 连接内嵌式 bridge host，用于 bridge/debug/replay。
//! 它不是 MIT / 双臂 / fault-stop 的实时控制路径。
//!
//! - Unix 平台默认连接 `/tmp/piper_bridge.sock`
//! - 非 Unix 平台必须显式传 `--endpoint`
//! - TCP/TLS endpoint 需要同时传 `--tls-ca` / `--tls-client-cert`
//!   / `--tls-client-key` / `--tls-server-name`

use clap::{Parser, ValueEnum};
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
    /// Non-Unix platforms must pass this explicitly.
    #[arg(long)]
    endpoint: Option<String>,

    /// Mode: status | send | receive | interactive
    #[arg(long, value_enum, default_value_t = BridgeTestMode::Status)]
    mode: BridgeTestMode,

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum BridgeTestMode {
    Status,
    Send,
    Receive,
    Interactive,
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

fn resolved_endpoint_arg(
    endpoint: Option<&str>,
    unix_supported: bool,
) -> Result<String, Box<dyn std::error::Error>> {
    match endpoint {
        Some(value) => Ok(value.to_string()),
        None if unix_supported => Ok(default_endpoint()),
        None => Err(
            "non-Unix platforms require an explicit --endpoint for bridge_test; TCP/TLS endpoints also require --tls-ca, --tls-client-cert, --tls-client-key, and --tls-server-name"
                .into(),
        ),
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
    } else if let Some(local_app_data) = std::env::var_os("LOCALAPPDATA") {
        PathBuf::from(local_app_data).join("piper-sdk").join("bridge_tokens")
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
    required_role: BridgeRole,
    timeout: Duration,
) -> Result<PiperBridgeClient, Box<dyn std::error::Error>> {
    let endpoint = parse_endpoint(endpoint_raw)?;
    let token = stable_session_token(endpoint_raw, "bridge_test")?;
    let options = BridgeClientOptions {
        session_token: token,
        filters: Vec::new(),
        connect_timeout: Duration::from_secs(5),
        request_timeout: timeout,
        tcp_tls: maybe_tls_config(&endpoint, args)?,
    };
    let client = PiperBridgeClient::connect(endpoint, options)?;
    if required_role == BridgeRole::WriterCandidate
        && client.role_granted() != BridgeRole::WriterCandidate
    {
        return Err(
            "bridge server did not grant WriterCandidate role; maintenance write requires a writer-capable listener or TLS client policy"
                .into(),
        );
    }
    Ok(client)
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
    let mut received = 0;
    while received < count {
        match client.recv_event(timeout)? {
            BridgeEvent::ReceiveFrame(frame) => {
                received += 1;
                println!(
                    "recv #{:03}: id=0x{:03X} len={} data={:02X?} ts_us={}",
                    received,
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

fn parse_interactive_send_frame(parts: &[&str]) -> Result<PiperFrame, Box<dyn std::error::Error>> {
    let id = u32::from_str_radix(parts[1].trim_start_matches("0x"), 16)?;
    if id > 0x7FF {
        return Err(format!("standard CAN ID must be <= 0x7FF, got 0x{id:X}").into());
    }

    let data_tokens = &parts[2..];
    if data_tokens.len() > 8 {
        return Err("at most 8 data bytes are allowed".into());
    }

    let mut data = [0u8; 8];
    for (index, token) in data_tokens.iter().enumerate() {
        data[index] = u8::from_str_radix(token.trim_start_matches("0x"), 16)?;
    }

    Ok(PiperFrame {
        id,
        data,
        len: data_tokens.len() as u8,
        is_extended: false,
        timestamp_us: 0,
    })
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
                let frame = parse_interactive_send_frame(&parts)?;
                let mut lease = client.acquire_maintenance_lease(Duration::from_millis(500))?;
                lease.send_frame(frame)?;
                lease.release()?;
                println!("sent frame id=0x{:03X}", frame.id);
            },
            "quit" | "exit" => break,
            other => println!("unknown command: {}", other),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_interactive_send_frame_rejects_standard_id_overflow() {
        let parts = vec!["send", "800", "01"];
        assert!(parse_interactive_send_frame(&parts).is_err());
    }

    #[test]
    fn parse_interactive_send_frame_rejects_more_than_eight_data_bytes() {
        let parts = vec![
            "send", "123", "00", "01", "02", "03", "04", "05", "06", "07", "08",
        ];
        assert!(parse_interactive_send_frame(&parts).is_err());
    }

    #[test]
    fn resolved_endpoint_arg_rejects_missing_non_unix_default() {
        assert!(resolved_endpoint_arg(None, false).is_err());
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let endpoint = resolved_endpoint_arg(args.endpoint.as_deref(), cfg!(unix))?;
    let timeout = Duration::from_millis(args.timeout_ms);

    println!("Piper bridge test client");
    println!("endpoint: {}", endpoint);
    println!("non-realtime bridge/debug path only");

    let role = if matches!(
        args.mode,
        BridgeTestMode::Send | BridgeTestMode::Interactive
    ) {
        BridgeRole::WriterCandidate
    } else {
        BridgeRole::Observer
    };
    let mut client = connect_client(&endpoint, &args, role, timeout)?;
    if matches!(
        args.mode,
        BridgeTestMode::Receive | BridgeTestMode::Interactive
    ) {
        client.set_raw_frame_tap(true)?;
    }

    match args.mode {
        BridgeTestMode::Status => print_status(&mut client)?,
        BridgeTestMode::Send => run_send(
            &mut client,
            args.count,
            Duration::from_millis(args.interval_ms),
        )?,
        BridgeTestMode::Receive => run_receive(&mut client, args.count, timeout)?,
        BridgeTestMode::Interactive => run_interactive(&mut client, timeout)?,
    }

    Ok(())
}
