#[cfg(target_os = "linux")]
use clap::Parser;
#[cfg(target_os = "linux")]
use piper_can::{
    CanAdapter, CanError, RawTimestampSample, SocketCanAdapter, SplittableAdapter, monotonic_micros,
};
#[cfg(target_os = "linux")]
use piper_tools::raw_clock::{
    RawClockEstimator, RawClockHealth, RawClockSample, RawClockThresholds,
};
#[cfg(target_os = "linux")]
use serde::Serialize;
#[cfg(target_os = "linux")]
use std::fs;
#[cfg(target_os = "linux")]
use std::path::{Path, PathBuf};
#[cfg(target_os = "linux")]
use std::process::Command;
#[cfg(target_os = "linux")]
use std::time::{Duration, Instant};

#[cfg(not(target_os = "linux"))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    Err("socketcan_raw_clock_probe is SocketCAN/Linux only".into())
}

#[cfg(target_os = "linux")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    piper_sdk::init_logger!();

    let args = Args::parse();
    let report = run_probe(&args)?;

    if let Some(out) = &args.out {
        write_report_atomic(out, &report)?;
        println!("wrote raw clock probe report to {}", out.display());
    } else {
        println!("{}", serde_json::to_string_pretty(&report)?);
    }

    Ok(())
}

#[cfg(target_os = "linux")]
#[derive(Parser, Debug)]
#[command(name = "socketcan_raw_clock_probe")]
#[command(about = "Read-only dual-interface SocketCAN raw timestamp clock probe")]
struct Args {
    #[arg(long)]
    left_interface: String,
    #[arg(long)]
    right_interface: String,
    #[arg(long, default_value_t = 300)]
    duration_secs: u64,
    #[arg(long)]
    out: Option<PathBuf>,
}

#[cfg(target_os = "linux")]
#[derive(Debug, Serialize)]
struct ProbeReport {
    schema_version: u8,
    left_interface: String,
    right_interface: String,
    left_metadata: ProbeInterfaceMetadata,
    right_metadata: ProbeInterfaceMetadata,
    left_timestamp_capabilities: TimestampCapabilitySummary,
    right_timestamp_capabilities: TimestampCapabilitySummary,
    pass: bool,
    left: ProbeSideReport,
    right: ProbeSideReport,
    raw_samples: Vec<ProbeRawFrameSample>,
    raw_clock_push_errors: Vec<ProbeRawClockPushError>,
    estimated_inter_arm_skew_p95_us: Option<u64>,
    max_estimated_inter_arm_skew_us: Option<u64>,
}

#[cfg(target_os = "linux")]
struct ProbeReportParts {
    left_interface: String,
    right_interface: String,
    left_metadata: ProbeInterfaceMetadata,
    right_metadata: ProbeInterfaceMetadata,
    left_timestamp_capabilities: TimestampCapabilitySummary,
    right_timestamp_capabilities: TimestampCapabilitySummary,
    raw_samples: Vec<ProbeRawFrameSample>,
    raw_clock_push_errors: Vec<ProbeRawClockPushError>,
    skew_samples_us: Vec<u64>,
    left_health: RawClockHealth,
    right_health: RawClockHealth,
}

#[cfg(target_os = "linux")]
impl ProbeReport {
    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    fn from_health(
        left_interface: &str,
        right_interface: &str,
        left_metadata: ProbeInterfaceMetadata,
        right_metadata: ProbeInterfaceMetadata,
        left_timestamp_capabilities: TimestampCapabilitySummary,
        right_timestamp_capabilities: TimestampCapabilitySummary,
        raw_samples: Vec<ProbeRawFrameSample>,
        left_health: RawClockHealth,
        right_health: RawClockHealth,
    ) -> Self {
        let skew_samples_us = derive_inter_arm_skew_samples(&raw_samples);
        Self::from_parts(ProbeReportParts {
            left_interface: left_interface.to_string(),
            right_interface: right_interface.to_string(),
            left_metadata,
            right_metadata,
            left_timestamp_capabilities,
            right_timestamp_capabilities,
            raw_samples,
            raw_clock_push_errors: Vec::new(),
            skew_samples_us,
            left_health,
            right_health,
        })
    }

    fn from_parts(parts: ProbeReportParts) -> Self {
        let mut sorted_skew_samples = parts.skew_samples_us;
        sorted_skew_samples.sort_unstable();
        let estimated_inter_arm_skew_p95_us = percentile_sorted(&sorted_skew_samples, 95);
        let max_estimated_inter_arm_skew_us = sorted_skew_samples.last().copied();
        let pass = parts.left_health.healthy && parts.right_health.healthy;

        Self {
            schema_version: 1,
            left_interface: parts.left_interface.clone(),
            right_interface: parts.right_interface.clone(),
            left_metadata: parts.left_metadata,
            right_metadata: parts.right_metadata,
            left_timestamp_capabilities: parts.left_timestamp_capabilities,
            right_timestamp_capabilities: parts.right_timestamp_capabilities,
            pass,
            left: ProbeSideReport::from_samples(
                ProbeSide::Left,
                &parts.left_interface,
                parts.left_health,
                &parts.raw_samples,
                &parts.raw_clock_push_errors,
            ),
            right: ProbeSideReport::from_samples(
                ProbeSide::Right,
                &parts.right_interface,
                parts.right_health,
                &parts.raw_samples,
                &parts.raw_clock_push_errors,
            ),
            raw_samples: parts.raw_samples,
            raw_clock_push_errors: parts.raw_clock_push_errors,
            estimated_inter_arm_skew_p95_us,
            max_estimated_inter_arm_skew_us,
        }
    }

    #[cfg(test)]
    fn from_samples_for_tests(raw_samples: Vec<ProbeRawFrameSample>) -> Self {
        Self::from_health(
            "can0",
            "can1",
            ProbeInterfaceMetadata::for_tests("can0"),
            ProbeInterfaceMetadata::for_tests("can1"),
            TimestampCapabilitySummary::unknown_for_tests(),
            TimestampCapabilitySummary::unknown_for_tests(),
            raw_samples,
            healthy_raw_clock_health_for_tests(),
            healthy_raw_clock_health_for_tests(),
        )
    }
}

#[cfg(target_os = "linux")]
#[derive(Debug, Serialize)]
struct ProbeSideReport {
    side: ProbeSide,
    interface: String,
    health: RawClockHealth,
    raw_sample_count: usize,
    hw_raw_sample_count: usize,
    mapped_sample_count: usize,
    system_timestamp_sample_count: usize,
    hardware_transmit_sample_count: usize,
    raw_clock_push_error_count: usize,
}

#[cfg(target_os = "linux")]
impl ProbeSideReport {
    fn from_samples(
        side: ProbeSide,
        interface: &str,
        health: RawClockHealth,
        samples: &[ProbeRawFrameSample],
        raw_clock_push_errors: &[ProbeRawClockPushError],
    ) -> Self {
        let side_samples = samples.iter().filter(|sample| sample.side == side);

        Self {
            side,
            interface: interface.to_string(),
            health,
            raw_sample_count: side_samples.clone().count(),
            hw_raw_sample_count: side_samples
                .clone()
                .filter(|sample| sample.hw_raw_us.is_some())
                .count(),
            mapped_sample_count: side_samples
                .clone()
                .filter(|sample| sample.mapped_host_us.is_some())
                .count(),
            system_timestamp_sample_count: side_samples
                .clone()
                .filter(|sample| sample.system_ts_us.is_some())
                .count(),
            hardware_transmit_sample_count: side_samples
                .filter(|sample| sample.hw_trans_us.is_some())
                .count(),
            raw_clock_push_error_count: raw_clock_push_errors
                .iter()
                .filter(|error| error.side == side)
                .count(),
        }
    }
}

#[cfg(target_os = "linux")]
#[derive(Debug, Serialize)]
struct ProbeInterfaceMetadata {
    name: String,
    if_index: Option<u32>,
    mtu: Option<u32>,
    driver: Option<String>,
}

#[cfg(target_os = "linux")]
impl ProbeInterfaceMetadata {
    #[cfg(test)]
    fn for_tests(name: &str) -> Self {
        Self {
            name: name.to_string(),
            if_index: Some(1),
            mtu: Some(16),
            driver: Some("test-driver".to_string()),
        }
    }
}

#[cfg(target_os = "linux")]
#[derive(Debug, Serialize)]
struct TimestampCapabilitySummary {
    source: String,
    so_timestamping_enabled: bool,
    hardware_transmit: Option<bool>,
    hardware_receive: Option<bool>,
    hardware_raw_clock: Option<bool>,
    raw_text: Option<String>,
    error: Option<String>,
}

#[cfg(target_os = "linux")]
impl TimestampCapabilitySummary {
    #[cfg(test)]
    fn unknown_for_tests() -> Self {
        Self {
            source: "test".to_string(),
            so_timestamping_enabled: false,
            hardware_transmit: None,
            hardware_receive: None,
            hardware_raw_clock: None,
            raw_text: None,
            error: None,
        }
    }

    fn mark_so_timestamping_enabled(&mut self) {
        self.so_timestamping_enabled = true;
    }
}

#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
enum ProbeSide {
    Left,
    Right,
}

#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Serialize)]
struct ProbeRawFrameSample {
    side: ProbeSide,
    can_id: u32,
    host_rx_mono_us: u64,
    system_ts_us: Option<u64>,
    hw_trans_us: Option<u64>,
    hw_raw_us: Option<u64>,
    mapped_host_us: Option<u64>,
}

#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Serialize)]
struct ProbeRawClockPushError {
    side: ProbeSide,
    can_id: u32,
    host_rx_mono_us: u64,
    hw_raw_us: u64,
    error: String,
}

#[cfg(target_os = "linux")]
fn run_probe(args: &Args) -> Result<ProbeReport, Box<dyn std::error::Error>> {
    validate_args(args)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidInput, error))?;

    let left_metadata = read_interface_metadata(&args.left_interface);
    let right_metadata = read_interface_metadata(&args.right_interface);
    let mut left_timestamp_capabilities = read_timestamp_capabilities(&args.left_interface);
    let mut right_timestamp_capabilities = read_timestamp_capabilities(&args.right_interface);

    let mut left = SocketCanAdapter::new(&args.left_interface)?;
    let mut right = SocketCanAdapter::new(&args.right_interface)?;
    left_timestamp_capabilities.mark_so_timestamping_enabled();
    right_timestamp_capabilities.mark_so_timestamping_enabled();

    left.set_receive_timeout(Duration::from_millis(10));
    right.set_receive_timeout(Duration::from_millis(10));
    let (mut left_rx, _left_tx) = left.split()?;
    let (mut right_rx, _right_tx) = right.split()?;

    let thresholds = default_estimator_thresholds(args.duration_secs);
    let mut left_estimator = RawClockEstimator::new(thresholds);
    let mut right_estimator = RawClockEstimator::new(thresholds);
    let mut raw_samples = Vec::new();
    let mut raw_clock_push_errors = Vec::new();
    let mut skew_samples_us = Vec::new();
    let mut latest_left_mapped = None;
    let mut latest_right_mapped = None;
    let start = Instant::now();
    let receive_timeout = Duration::from_millis(5);

    while start.elapsed() < Duration::from_secs(args.duration_secs) {
        let mut mapped_updated = false;

        match left_rx.receive_raw_timestamp_sample(receive_timeout) {
            Ok(sample) => {
                if record_sample(
                    ProbeSide::Left,
                    sample,
                    &mut left_estimator,
                    &mut raw_samples,
                    &mut raw_clock_push_errors,
                    &mut latest_left_mapped,
                ) {
                    mapped_updated = true;
                }
            },
            Err(CanError::Timeout) => {},
            Err(error) => return Err(Box::new(error)),
        }

        match right_rx.receive_raw_timestamp_sample(receive_timeout) {
            Ok(sample) => {
                if record_sample(
                    ProbeSide::Right,
                    sample,
                    &mut right_estimator,
                    &mut raw_samples,
                    &mut raw_clock_push_errors,
                    &mut latest_right_mapped,
                ) {
                    mapped_updated = true;
                }
            },
            Err(CanError::Timeout) => {},
            Err(error) => return Err(Box::new(error)),
        }

        if mapped_updated
            && let (Some(left_us), Some(right_us)) = (latest_left_mapped, latest_right_mapped)
        {
            skew_samples_us.push(left_us.abs_diff(right_us));
        }
    }

    let now_host_us = monotonic_micros();
    let left_health = left_estimator.health(now_host_us);
    let right_health = right_estimator.health(now_host_us);

    Ok(ProbeReport::from_parts(ProbeReportParts {
        left_interface: args.left_interface.clone(),
        right_interface: args.right_interface.clone(),
        left_metadata,
        right_metadata,
        left_timestamp_capabilities,
        right_timestamp_capabilities,
        raw_samples,
        raw_clock_push_errors,
        skew_samples_us,
        left_health,
        right_health,
    }))
}

#[cfg(target_os = "linux")]
fn record_sample(
    side: ProbeSide,
    sample: RawTimestampSample,
    estimator: &mut RawClockEstimator,
    raw_samples: &mut Vec<ProbeRawFrameSample>,
    raw_clock_push_errors: &mut Vec<ProbeRawClockPushError>,
    latest_mapped_host_us: &mut Option<u64>,
) -> bool {
    let mut mapped_host_us = None;

    if let Some(hw_raw_us) = sample.info.hw_raw_us {
        match estimator.push(RawClockSample {
            raw_us: hw_raw_us,
            host_rx_mono_us: sample.info.host_rx_mono_us,
        }) {
            Ok(()) => {
                mapped_host_us = estimator.map_raw_us(hw_raw_us);
                *latest_mapped_host_us = mapped_host_us;
            },
            Err(error) => {
                raw_clock_push_errors.push(ProbeRawClockPushError {
                    side,
                    can_id: sample.info.can_id,
                    host_rx_mono_us: sample.info.host_rx_mono_us,
                    hw_raw_us,
                    error: error.to_string(),
                });
            },
        }
    }

    raw_samples.push(ProbeRawFrameSample {
        side,
        can_id: sample.info.can_id,
        host_rx_mono_us: sample.info.host_rx_mono_us,
        system_ts_us: sample.info.system_ts_us,
        hw_trans_us: sample.info.hw_trans_us,
        hw_raw_us: sample.info.hw_raw_us,
        mapped_host_us,
    });

    mapped_host_us.is_some()
}

#[cfg(target_os = "linux")]
fn validate_args(args: &Args) -> Result<(), String> {
    if args.left_interface == args.right_interface {
        return Err(format!(
            "--left-interface and --right-interface must differ; both were '{}'",
            args.left_interface
        ));
    }

    Ok(())
}

#[cfg(target_os = "linux")]
fn default_estimator_thresholds(duration_secs: u64) -> RawClockThresholds {
    let duration_us = duration_secs.saturating_mul(1_000_000);
    let warmup_window_us = (duration_us / 10).clamp(1_000_000, 10_000_000);

    RawClockThresholds {
        warmup_samples: 128,
        warmup_window_us,
        residual_p95_us: 500,
        residual_max_us: 2_000,
        drift_abs_ppm: 100.0,
        sample_gap_max_us: 200_000,
        last_sample_age_us: 1_000_000,
    }
}

#[cfg(target_os = "linux")]
fn read_interface_metadata(iface: &str) -> ProbeInterfaceMetadata {
    let base = Path::new("/sys/class/net").join(iface);
    let if_index = read_trimmed(base.join("ifindex")).and_then(|text| text.parse().ok());
    let mtu = read_trimmed(base.join("mtu")).and_then(|text| text.parse().ok());
    let driver = fs::read_link(base.join("device/driver"))
        .ok()
        .and_then(|path| path.file_name().map(|name| name.to_string_lossy().into_owned()));

    ProbeInterfaceMetadata {
        name: iface.to_string(),
        if_index,
        mtu,
        driver,
    }
}

#[cfg(target_os = "linux")]
fn read_timestamp_capabilities(iface: &str) -> TimestampCapabilitySummary {
    let source = format!("ethtool -T {iface}");
    let output = Command::new("ethtool").arg("-T").arg(iface).output();

    match output {
        Ok(output) if output.status.success() => {
            let raw_text = String::from_utf8_lossy(&output.stdout).into_owned();
            TimestampCapabilitySummary {
                source,
                so_timestamping_enabled: false,
                hardware_transmit: Some(ethtool_has_capability(&raw_text, "hardware-transmit")),
                hardware_receive: Some(ethtool_has_capability(&raw_text, "hardware-receive")),
                hardware_raw_clock: Some(ethtool_has_capability(&raw_text, "hardware-raw-clock")),
                raw_text: Some(raw_text),
                error: None,
            }
        },
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            TimestampCapabilitySummary {
                source,
                so_timestamping_enabled: false,
                hardware_transmit: None,
                hardware_receive: None,
                hardware_raw_clock: None,
                raw_text: None,
                error: Some(format!("ethtool exited with {}: {stderr}", output.status)),
            }
        },
        Err(error) => TimestampCapabilitySummary {
            source,
            so_timestamping_enabled: false,
            hardware_transmit: None,
            hardware_receive: None,
            hardware_raw_clock: None,
            raw_text: None,
            error: Some(format!("failed to run ethtool: {error}")),
        },
    }
}

#[cfg(target_os = "linux")]
fn ethtool_has_capability(raw_text: &str, capability: &str) -> bool {
    raw_text
        .lines()
        .any(|line| line.split_whitespace().any(|part| part == capability))
}

#[cfg(target_os = "linux")]
fn read_trimmed(path: PathBuf) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
}

#[cfg(target_os = "linux")]
fn write_report_atomic(
    path: &Path,
    report: &ProbeReport,
) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)?;
    }

    let file_name = path.file_name().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "output path has no file name",
        )
    })?;
    let temp_name = format!(
        ".{}.{}.tmp",
        file_name.to_string_lossy(),
        std::process::id()
    );
    let temp_path = path.with_file_name(temp_name);
    let json = serde_json::to_vec_pretty(report)?;

    fs::write(&temp_path, json)?;
    fs::rename(&temp_path, path)?;
    Ok(())
}

#[cfg(all(test, target_os = "linux"))]
fn derive_inter_arm_skew_samples(samples: &[ProbeRawFrameSample]) -> Vec<u64> {
    let mut latest_left_mapped = None;
    let mut latest_right_mapped = None;
    let mut skew_samples_us = Vec::new();

    for sample in samples {
        let Some(mapped_host_us) = sample.mapped_host_us else {
            continue;
        };

        match sample.side {
            ProbeSide::Left => latest_left_mapped = Some(mapped_host_us),
            ProbeSide::Right => latest_right_mapped = Some(mapped_host_us),
        }

        if let (Some(left_us), Some(right_us)) = (latest_left_mapped, latest_right_mapped) {
            skew_samples_us.push(left_us.abs_diff(right_us));
        }
    }

    skew_samples_us
}

#[cfg(target_os = "linux")]
fn percentile_sorted(sorted: &[u64], percentile: u64) -> Option<u64> {
    if sorted.is_empty() {
        return None;
    }

    let rank = (percentile as usize * (sorted.len() - 1)).div_ceil(100);
    sorted.get(rank.min(sorted.len() - 1)).copied()
}

#[cfg(all(test, target_os = "linux"))]
fn healthy_raw_clock_health_for_tests() -> RawClockHealth {
    RawClockHealth {
        healthy: true,
        sample_count: 4,
        window_duration_us: 3_000,
        drift_ppm: 0.0,
        residual_p50_us: 0,
        residual_p95_us: 0,
        residual_p99_us: 0,
        residual_max_us: 0,
        sample_gap_max_us: 1_000,
        last_sample_age_us: 0,
        raw_timestamp_regressions: 0,
        reason: None,
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use clap::Parser;
    use piper_tools::raw_clock::RawClockHealth;

    #[test]
    fn parses_required_dual_interface_args() {
        let args = Args::parse_from([
            "socketcan_raw_clock_probe",
            "--left-interface",
            "can0",
            "--right-interface",
            "can1",
            "--duration-secs",
            "300",
            "--out",
            "artifacts/teleop/raw-clock-probe.json",
        ]);

        assert_eq!(args.left_interface, "can0");
        assert_eq!(args.right_interface, "can1");
        assert_eq!(args.duration_secs, 300);
    }

    #[test]
    fn report_marks_failed_when_one_side_unhealthy() {
        let report = ProbeReport::from_health(
            "can0",
            "can1",
            ProbeInterfaceMetadata::for_tests("can0"),
            ProbeInterfaceMetadata::for_tests("can1"),
            TimestampCapabilitySummary::unknown_for_tests(),
            TimestampCapabilitySummary::unknown_for_tests(),
            Vec::new(),
            healthy_raw_clock_health(),
            RawClockHealth {
                healthy: false,
                reason: Some("no hw_raw".to_string()),
                ..empty_raw_clock_health()
            },
        );

        assert!(!report.pass);
    }

    #[test]
    fn report_keeps_raw_samples_and_inter_arm_skew_metrics() {
        let samples = vec![
            ProbeRawFrameSample {
                side: ProbeSide::Left,
                can_id: 0x251,
                host_rx_mono_us: 110_000,
                system_ts_us: Some(110_010),
                hw_trans_us: None,
                hw_raw_us: Some(10_000),
                mapped_host_us: Some(110_000),
            },
            ProbeRawFrameSample {
                side: ProbeSide::Right,
                can_id: 0x251,
                host_rx_mono_us: 110_700,
                system_ts_us: Some(110_710),
                hw_trans_us: None,
                hw_raw_us: Some(20_000),
                mapped_host_us: Some(110_700),
            },
        ];

        let report = ProbeReport::from_samples_for_tests(samples);
        assert_eq!(report.raw_samples.len(), 2);
        assert_eq!(report.max_estimated_inter_arm_skew_us, Some(700));
        assert_eq!(report.estimated_inter_arm_skew_p95_us, Some(700));
    }

    #[test]
    fn same_interface_validation_rejects() {
        let args = Args::parse_from([
            "socketcan_raw_clock_probe",
            "--left-interface",
            "can0",
            "--right-interface",
            "can0",
        ]);

        let err = validate_args(&args).unwrap_err();

        assert!(err.contains("must differ"), "{err}");
        assert!(err.contains("can0"), "{err}");
    }

    #[test]
    fn regression_handling_does_not_update_mapped_or_skew_metrics() {
        let thresholds = RawClockThresholds {
            warmup_samples: 2,
            warmup_window_us: 1_000,
            residual_p95_us: 10,
            residual_max_us: 20,
            drift_abs_ppm: 100.0,
            sample_gap_max_us: 10_000,
            last_sample_age_us: 10_000,
        };
        let mut estimator = RawClockEstimator::new(thresholds);
        estimator
            .push(RawClockSample {
                raw_us: 10_000,
                host_rx_mono_us: 110_000,
            })
            .unwrap();
        estimator
            .push(RawClockSample {
                raw_us: 11_000,
                host_rx_mono_us: 111_000,
            })
            .unwrap();

        let mut raw_samples = Vec::new();
        let mut raw_clock_push_errors = Vec::new();
        let mut latest_mapped_host_us = Some(111_000);

        let mapped_updated = record_sample(
            ProbeSide::Left,
            RawTimestampSample {
                iface: "can0".to_string(),
                info: piper_can::RawTimestampInfo {
                    can_id: 0x251,
                    host_rx_mono_us: 111_500,
                    system_ts_us: Some(111_510),
                    hw_trans_us: None,
                    hw_raw_us: Some(10_500),
                },
            },
            &mut estimator,
            &mut raw_samples,
            &mut raw_clock_push_errors,
            &mut latest_mapped_host_us,
        );

        assert!(!mapped_updated);
        assert_eq!(latest_mapped_host_us, Some(111_000));
        assert_eq!(raw_samples.len(), 1);
        assert_eq!(raw_samples[0].mapped_host_us, None);
        assert_eq!(raw_clock_push_errors.len(), 1);

        let report = ProbeReport::from_parts(ProbeReportParts {
            left_interface: "can0".to_string(),
            right_interface: "can1".to_string(),
            left_metadata: ProbeInterfaceMetadata::for_tests("can0"),
            right_metadata: ProbeInterfaceMetadata::for_tests("can1"),
            left_timestamp_capabilities: TimestampCapabilitySummary::unknown_for_tests(),
            right_timestamp_capabilities: TimestampCapabilitySummary::unknown_for_tests(),
            raw_samples,
            raw_clock_push_errors,
            skew_samples_us: Vec::new(),
            left_health: estimator.health(111_500),
            right_health: healthy_raw_clock_health(),
        });

        assert_eq!(report.max_estimated_inter_arm_skew_us, None);
        assert_eq!(report.left.raw_clock_push_error_count, 1);
        assert_eq!(report.raw_clock_push_errors.len(), 1);
    }

    fn healthy_raw_clock_health() -> RawClockHealth {
        RawClockHealth {
            healthy: true,
            sample_count: 4,
            window_duration_us: 3_000,
            drift_ppm: 0.0,
            residual_p50_us: 0,
            residual_p95_us: 0,
            residual_p99_us: 0,
            residual_max_us: 0,
            sample_gap_max_us: 1_000,
            last_sample_age_us: 0,
            raw_timestamp_regressions: 0,
            reason: None,
        }
    }

    fn empty_raw_clock_health() -> RawClockHealth {
        RawClockHealth {
            healthy: false,
            sample_count: 0,
            window_duration_us: 0,
            drift_ppm: 0.0,
            residual_p50_us: 0,
            residual_p95_us: 0,
            residual_p99_us: 0,
            residual_max_us: 0,
            sample_gap_max_us: 0,
            last_sample_age_us: 0,
            raw_timestamp_regressions: 0,
            reason: None,
        }
    }
}
