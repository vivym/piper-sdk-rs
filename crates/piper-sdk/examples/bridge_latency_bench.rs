//! Controller-owned bridge 延迟基准测试。
//!
//! 该程序评估非实时 bridge/debug 链路的 host-side 开销：
//! - `get_status()` request/response 延迟
//! - `SendFrame`（持有 writer lease）请求延迟
//! - `ReceiveFrame/Gap` 事件等待延迟

use clap::Parser;
use piper_sdk::{
    BridgeClientOptions, BridgeEndpoint, BridgeEvent, BridgeRole, BridgeTlsClientConfig,
    PiperBridgeClient, PiperFrame, SessionToken,
};
use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::time::{Duration, Instant};

#[derive(Parser, Debug)]
#[command(name = "bridge_latency_bench")]
#[command(about = "Benchmark the non-realtime controller-owned Piper bridge")]
struct Args {
    /// Bridge endpoint.
    #[arg(long)]
    endpoint: Option<String>,

    /// Benchmark mode: send | status | receive | all
    #[arg(long, default_value = "all")]
    mode: String,

    /// Iteration count for send/status benchmarks.
    #[arg(long, default_value = "2000")]
    count: u32,

    /// Maximum events to wait for in receive mode.
    #[arg(long, default_value = "100")]
    receive_count: u32,

    /// Per-request timeout in milliseconds.
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

struct LatencyStats {
    samples: Vec<Duration>,
    started_at: Instant,
}

impl LatencyStats {
    fn new() -> Self {
        Self {
            samples: Vec::new(),
            started_at: Instant::now(),
        }
    }

    fn push(&mut self, duration: Duration) {
        self.samples.push(duration);
    }

    fn fps(&self) -> f64 {
        let elapsed = self.started_at.elapsed().as_secs_f64();
        if elapsed <= 0.0 {
            0.0
        } else {
            self.samples.len() as f64 / elapsed
        }
    }

    fn percentiles(&mut self) -> Option<(Duration, Duration, Duration, Duration)> {
        if self.samples.is_empty() {
            return None;
        }
        self.samples.sort_unstable();
        let len = self.samples.len();
        Some((
            self.samples[len * 50 / 100],
            self.samples[len * 99 / 100],
            self.samples[if len >= 1000 {
                len * 999 / 1000
            } else {
                len - 1
            }],
            self.samples[len - 1],
        ))
    }
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

fn stable_session_token(endpoint: &str, tool_name: &str) -> Result<SessionToken, std::io::Error> {
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
    let options = BridgeClientOptions {
        session_token: stable_session_token(endpoint_raw, "bridge_latency_bench")?,
        role_request: role,
        filters: Vec::new(),
        connect_timeout: Duration::from_secs(5),
        request_timeout: timeout,
        tcp_tls: maybe_tls_config(&endpoint, args)?,
    };
    Ok(PiperBridgeClient::connect(endpoint, options)?)
}

fn print_stats(label: &str, stats: &mut LatencyStats) {
    if let Some((p50, p99, p999, max)) = stats.percentiles() {
        println!(
            "{}: samples={} p50={:?} p99={:?} p999={:?} max={:?} fps={:.1}",
            label,
            stats.samples.len(),
            p50,
            p99,
            p999,
            max,
            stats.fps()
        );
    } else {
        println!("{}: no samples", label);
    }
}

fn run_status_bench(
    endpoint: &str,
    args: &Args,
    count: u32,
    timeout: Duration,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut client = connect_client(endpoint, args, BridgeRole::Observer, timeout)?;
    let mut stats = LatencyStats::new();
    for _ in 0..count {
        let start = Instant::now();
        let _ = client.get_status()?;
        stats.push(start.elapsed());
    }
    print_stats("status", &mut stats);
    Ok(())
}

fn run_send_bench(
    endpoint: &str,
    args: &Args,
    count: u32,
    timeout: Duration,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut client = connect_client(endpoint, args, BridgeRole::WriterCandidate, timeout)?;
    let mut lease = client.acquire_maintenance_lease(Duration::from_millis(500))?;
    let mut stats = LatencyStats::new();
    for index in 0..count {
        let frame = PiperFrame::new_standard(
            0x300 + (index as u16 % 0x20),
            &[index as u8, 0xAA, 0xBB, 0xCC],
        );
        let start = Instant::now();
        lease.send_frame(frame)?;
        stats.push(start.elapsed());
    }
    lease.release()?;
    print_stats("send", &mut stats);
    Ok(())
}

fn run_receive_bench(
    endpoint: &str,
    args: &Args,
    receive_count: u32,
    timeout: Duration,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut client = connect_client(endpoint, args, BridgeRole::Observer, timeout)?;
    let mut stats = LatencyStats::new();
    let deadline = Instant::now() + Duration::from_secs(10);
    while stats.samples.len() < receive_count as usize && Instant::now() < deadline {
        let start = Instant::now();
        match client.recv_event(timeout) {
            Ok(BridgeEvent::ReceiveFrame(_)) | Ok(BridgeEvent::Gap { .. }) => {
                stats.push(start.elapsed());
            },
            Ok(BridgeEvent::SessionReplaced) => {
                println!("receive benchmark interrupted: session replaced");
                break;
            },
            Ok(BridgeEvent::MaintenanceLeaseRevoked) => {},
            Err(err) => {
                println!("receive benchmark stopped: {}", err);
                break;
            },
        }
    }
    print_stats("receive", &mut stats);
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let endpoint = args.endpoint.clone().unwrap_or_else(default_endpoint);
    let timeout = Duration::from_millis(args.timeout_ms);

    println!("Piper bridge latency benchmark");
    println!("endpoint: {}", endpoint);
    println!("non-realtime bridge/debug path only");

    match args.mode.as_str() {
        "status" => run_status_bench(&endpoint, &args, args.count, timeout)?,
        "send" => run_send_bench(&endpoint, &args, args.count, timeout)?,
        "receive" => run_receive_bench(&endpoint, &args, args.receive_count, timeout)?,
        "all" => {
            run_status_bench(&endpoint, &args, args.count, timeout)?;
            run_send_bench(&endpoint, &args, args.count, timeout)?;
            run_receive_bench(&endpoint, &args, args.receive_count, timeout)?;
        },
        other => return Err(format!("unsupported mode: {}", other).into()),
    }

    Ok(())
}
