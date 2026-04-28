#![allow(dead_code)]

use anyhow::{Context, Result};
use piper_client::RuntimeFaultKind;
use piper_client::dual_arm::{
    BilateralExitReason, BilateralRunReport, StopAttemptResult, SubmissionArm,
};
use serde::Serialize;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::Path;
use std::time::Duration;

use crate::teleop::config::{
    TeleopControlSettings, TeleopMode, TeleopProfile, TeleopSafetySettings,
};
use crate::teleop::target::{ConcreteTeleopTarget, RoleTargets, TeleopPlatform};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TeleopExitStatus {
    Success,
    Failure,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct TeleopJsonReport {
    pub schema_version: u8,
    pub command: &'static str,
    pub platform: String,
    pub targets: ReportTargets,
    pub profile: String,
    pub mode: ReportMode,
    pub control: ReportControl,
    pub calibration: ReportCalibration,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timing: Option<ReportTiming>,
    pub exit: ReportExit,
    pub metrics: ReportMetrics,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ReportTargets {
    pub master: String,
    pub slave: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ReportMode {
    pub initial: String,
    #[serde(rename = "final")]
    pub final_: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ReportControl {
    pub frequency_hz: f64,
    pub track_kp: f64,
    pub track_kd: f64,
    pub master_damping: f64,
    pub reflection_gain: f64,
    pub gripper_mirror: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ReportCalibration {
    pub source: String,
    pub path: Option<String>,
    pub created_at_unix_ms: Option<u64>,
    pub max_error_rad: f64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ReportTiming {
    pub timing_source: String,
    pub experimental: bool,
    pub strict_realtime: bool,
    pub master_clock_drift_ppm: Option<f64>,
    pub slave_clock_drift_ppm: Option<f64>,
    pub master_residual_p95_us: Option<u64>,
    pub slave_residual_p95_us: Option<u64>,
    pub max_estimated_inter_arm_skew_us: Option<u64>,
    pub estimated_inter_arm_skew_p95_us: Option<u64>,
    pub clock_health_failures: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ReportExit {
    pub clean: bool,
    pub reason: Option<String>,
    pub faulted: bool,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ReportMetrics {
    pub iterations: usize,
    pub read_faults: u32,
    pub submission_faults: u32,
    pub last_submission_failed_role: Option<String>,
    pub peer_command_may_have_applied: bool,
    pub deadline_misses: u64,
    pub max_inter_arm_skew_us: u64,
    pub max_real_dt_us: u64,
    pub max_cycle_lag_us: u64,
    pub master_tx_frames_sent_total: u64,
    pub slave_tx_frames_sent_total: u64,
    pub master_tx_realtime_overwrites_total: u64,
    pub slave_tx_realtime_overwrites_total: u64,
    pub master_tx_fault_aborts_total: u64,
    pub slave_tx_fault_aborts_total: u64,
    pub master_stop_attempt: String,
    pub slave_stop_attempt: String,
    pub master_runtime_fault: Option<String>,
    pub slave_runtime_fault: Option<String>,
}

#[derive(Debug, Clone)]
pub struct TeleopReportInput<'a> {
    pub platform: TeleopPlatform,
    pub targets: RoleTargets,
    pub profile: TeleopProfile,
    pub initial_mode: TeleopMode,
    pub final_mode: TeleopMode,
    pub control: TeleopControlSettings,
    pub safety: TeleopSafetySettings,
    pub calibration: ReportCalibration,
    pub timing: Option<ReportTiming>,
    pub faulted: bool,
    pub report: &'a BilateralRunReport,
}

pub fn classify_exit(faulted: bool, report: &BilateralRunReport) -> TeleopExitStatus {
    if !faulted
        && matches!(
            report.exit_reason,
            Some(BilateralExitReason::Cancelled | BilateralExitReason::MaxIterations)
        )
    {
        TeleopExitStatus::Success
    } else {
        TeleopExitStatus::Failure
    }
}

impl TeleopJsonReport {
    pub fn from_run(input: TeleopReportInput<'_>) -> Self {
        let report = input.report;
        let clean = classify_exit(input.faulted, report) == TeleopExitStatus::Success;

        Self {
            schema_version: 1,
            command: "teleop dual-arm",
            platform: format_platform(input.platform).to_string(),
            targets: ReportTargets {
                master: format_target(&input.targets.master),
                slave: format_target(&input.targets.slave),
            },
            profile: format_profile(input.profile).to_string(),
            mode: ReportMode {
                initial: format_mode(input.initial_mode).to_string(),
                final_: format_mode(input.final_mode).to_string(),
            },
            control: ReportControl {
                frequency_hz: input.control.frequency_hz,
                track_kp: input.control.track_kp,
                track_kd: input.control.track_kd,
                master_damping: input.control.master_damping,
                reflection_gain: input.control.reflection_gain,
                gripper_mirror: input.safety.gripper_mirror,
            },
            calibration: input.calibration,
            timing: input.timing,
            exit: ReportExit {
                clean,
                reason: report.exit_reason.map(format_exit_reason).map(str::to_string),
                faulted: input.faulted,
                last_error: report.last_error.clone(),
            },
            metrics: ReportMetrics {
                iterations: report.iterations,
                read_faults: report.read_faults,
                submission_faults: report.submission_faults,
                last_submission_failed_role: report
                    .last_submission_failed_arm
                    .map(format_submission_arm)
                    .map(str::to_string),
                peer_command_may_have_applied: report.peer_command_may_have_applied,
                deadline_misses: report.deadline_misses,
                max_inter_arm_skew_us: duration_us(report.max_inter_arm_skew),
                max_real_dt_us: duration_us(report.max_real_dt),
                max_cycle_lag_us: duration_us(report.max_cycle_lag),
                master_tx_frames_sent_total: report.left_tx_frames_sent_total,
                slave_tx_frames_sent_total: report.right_tx_frames_sent_total,
                master_tx_realtime_overwrites_total: report.left_tx_realtime_overwrites_total,
                slave_tx_realtime_overwrites_total: report.right_tx_realtime_overwrites_total,
                master_tx_fault_aborts_total: report.left_tx_fault_aborts_total,
                slave_tx_fault_aborts_total: report.right_tx_fault_aborts_total,
                master_stop_attempt: format_stop_attempt(report.left_stop_attempt).to_string(),
                slave_stop_attempt: format_stop_attempt(report.right_stop_attempt).to_string(),
                master_runtime_fault: report
                    .last_runtime_fault_left
                    .map(format_runtime_fault)
                    .map(str::to_string),
                slave_runtime_fault: report
                    .last_runtime_fault_right
                    .map(format_runtime_fault)
                    .map(str::to_string),
            },
        }
    }
}

#[allow(dead_code)]
pub fn print_human_report(report: &TeleopJsonReport, elapsed: Duration) {
    let _ = write_human_report(io::stdout().lock(), report, elapsed);
}

pub fn write_human_report<W: Write>(
    mut writer: W,
    report: &TeleopJsonReport,
    elapsed: Duration,
) -> io::Result<()> {
    writeln!(writer, "teleop dual-arm report")?;
    writeln!(
        writer,
        "exit: clean={} reason={} faulted={} elapsed_us={}",
        report.exit.clean,
        report.exit.reason.as_deref().unwrap_or("unknown"),
        report.exit.faulted,
        duration_us(elapsed)
    )?;
    writeln!(
        writer,
        "mode: {} -> {} profile={}",
        report.mode.initial, report.mode.final_, report.profile
    )?;
    writeln!(
        writer,
        "targets: master={} slave={}",
        report.targets.master, report.targets.slave
    )?;
    writeln!(
        writer,
        "metrics: iterations={} read_faults={} submission_faults={} last_submission_failed_role={} peer_command_may_have_applied={} deadline_misses={}",
        report.metrics.iterations,
        report.metrics.read_faults,
        report.metrics.submission_faults,
        report.metrics.last_submission_failed_role.as_deref().unwrap_or("none"),
        report.metrics.peer_command_may_have_applied,
        report.metrics.deadline_misses
    )?;
    writeln!(
        writer,
        "timing: max_inter_arm_skew_us={} max_real_dt_us={} max_cycle_lag_us={}",
        report.metrics.max_inter_arm_skew_us,
        report.metrics.max_real_dt_us,
        report.metrics.max_cycle_lag_us
    )?;
    if let Some(timing) = &report.timing {
        writeln!(writer, "timing_source={}", timing.timing_source)?;
        writeln!(writer, "experimental={}", timing.experimental)?;
        writeln!(writer, "strict_realtime={}", timing.strict_realtime)?;
        writeln!(
            writer,
            "raw_clock max_skew_us={} p95_skew_us={}",
            timing.max_estimated_inter_arm_skew_us.unwrap_or(0),
            timing.estimated_inter_arm_skew_p95_us.unwrap_or(0)
        )?;
    }
    writeln!(
        writer,
        "master: tx_frames={} overwrites={} fault_aborts={} stop_attempt={} master_runtime_fault={}",
        report.metrics.master_tx_frames_sent_total,
        report.metrics.master_tx_realtime_overwrites_total,
        report.metrics.master_tx_fault_aborts_total,
        report.metrics.master_stop_attempt,
        report.metrics.master_runtime_fault.as_deref().unwrap_or("none")
    )?;
    writeln!(
        writer,
        "slave: tx_frames={} overwrites={} fault_aborts={} stop_attempt={} slave_runtime_fault={}",
        report.metrics.slave_tx_frames_sent_total,
        report.metrics.slave_tx_realtime_overwrites_total,
        report.metrics.slave_tx_fault_aborts_total,
        report.metrics.slave_stop_attempt,
        report.metrics.slave_runtime_fault.as_deref().unwrap_or("none")
    )?;
    if let Some(last_error) = &report.exit.last_error {
        writeln!(writer, "last_error: {last_error}")?;
    }
    Ok(())
}

pub fn write_json_report(path: &Path, report: &TeleopJsonReport) -> Result<()> {
    let contents =
        serde_json::to_string_pretty(report).context("failed to serialize teleop report")?;
    publish_report_atomically(path, contents.as_bytes())
        .with_context(|| format!("failed to write teleop report {}", path.display()))
}

fn publish_report_atomically(path: &Path, contents: &[u8]) -> io::Result<()> {
    let parent = path.parent().filter(|parent| !parent.as_os_str().is_empty());
    let dir = parent.unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "report path has no file name"))?
        .to_string_lossy();
    let temp_path = dir.join(format!(
        ".{file_name}.{}.{}.tmp",
        std::process::id(),
        unique_temp_suffix()
    ));

    let result = (|| {
        let mut file = File::options().write(true).create_new(true).open(&temp_path)?;
        file.write_all(contents)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temp_path, path)?;
        sync_directory(dir);
        Ok(())
    })();

    if result.is_err() {
        let _ = fs::remove_file(&temp_path);
    }

    result
}

fn unique_temp_suffix() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0)
}

fn sync_directory(dir: &Path) {
    if let Ok(file) = File::open(dir) {
        let _ = file.sync_all();
    }
}

fn duration_us(duration: Duration) -> u64 {
    duration.as_micros().min(u128::from(u64::MAX)) as u64
}

fn format_mode(mode: TeleopMode) -> &'static str {
    match mode {
        TeleopMode::MasterFollower => "master_follower",
        TeleopMode::Bilateral => "bilateral",
    }
}

fn format_profile(profile: TeleopProfile) -> &'static str {
    match profile {
        TeleopProfile::Production => "production",
        TeleopProfile::Debug => "debug",
    }
}

fn format_target(target: &ConcreteTeleopTarget) -> String {
    match target {
        ConcreteTeleopTarget::SocketCan { iface } => format!("socketcan:{iface}"),
        ConcreteTeleopTarget::GsUsbSerial { serial } => format!("gs-usb-serial:{serial}"),
        ConcreteTeleopTarget::GsUsbBusAddress { bus, address } => {
            format!("gs-usb-bus-address:{bus}:{address}")
        },
    }
}

fn format_platform(platform: TeleopPlatform) -> &'static str {
    match platform {
        TeleopPlatform::Linux => "linux",
        TeleopPlatform::Other => "other",
    }
}

fn format_exit_reason(reason: BilateralExitReason) -> &'static str {
    match reason {
        BilateralExitReason::MaxIterations => "max_iterations",
        BilateralExitReason::Cancelled => "cancelled",
        BilateralExitReason::ReadFault => "read_fault",
        BilateralExitReason::ControllerFault => "controller_fault",
        BilateralExitReason::CompensationFault => "compensation_fault",
        BilateralExitReason::SubmissionFault => "submission_fault",
        BilateralExitReason::RuntimeTransportFault => "runtime_transport_fault",
        BilateralExitReason::RuntimeManualFault => "runtime_manual_fault",
    }
}

fn format_stop_attempt(result: StopAttemptResult) -> &'static str {
    match result {
        StopAttemptResult::NotAttempted => "not_attempted",
        StopAttemptResult::ConfirmedSent => "confirmed_sent",
        StopAttemptResult::Timeout => "timeout",
        StopAttemptResult::ChannelClosed => "channel_closed",
        StopAttemptResult::QueueRejected => "queue_rejected",
        StopAttemptResult::TransportFailed => "transport_failed",
    }
}

fn format_submission_arm(arm: SubmissionArm) -> &'static str {
    match arm {
        SubmissionArm::Left => "master",
        SubmissionArm::Right => "slave",
    }
}

fn format_runtime_fault(fault: RuntimeFaultKind) -> &'static str {
    match fault {
        RuntimeFaultKind::RxExited => "rx_exited",
        RuntimeFaultKind::TxExited => "tx_exited",
        RuntimeFaultKind::TransportError => "transport_error",
        RuntimeFaultKind::ManualFault => "manual_fault",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::teleop::config::{
        TeleopControlSettings, TeleopMode, TeleopProfile, TeleopSafetySettings,
    };
    use crate::teleop::target::{ConcreteTeleopTarget, RoleTargets, TeleopPlatform};
    use piper_client::RuntimeFaultKind;
    use piper_client::dual_arm::{
        BilateralExitReason, BilateralRunReport, StopAttemptResult, SubmissionArm,
    };
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::time::Duration;

    #[test]
    fn cancelled_report_is_success() {
        let report = BilateralRunReport {
            exit_reason: Some(BilateralExitReason::Cancelled),
            ..BilateralRunReport::default()
        };

        assert_eq!(classify_exit(false, &report), TeleopExitStatus::Success);
    }

    #[test]
    fn standby_read_fault_is_failure() {
        let report = BilateralRunReport {
            exit_reason: Some(BilateralExitReason::ReadFault),
            ..BilateralRunReport::default()
        };

        assert_eq!(classify_exit(false, &report), TeleopExitStatus::Failure);
    }

    #[test]
    fn faulted_cancelled_report_is_failure() {
        let report = BilateralRunReport {
            exit_reason: Some(BilateralExitReason::Cancelled),
            ..BilateralRunReport::default()
        };

        assert_eq!(classify_exit(true, &report), TeleopExitStatus::Failure);
    }

    #[test]
    fn missing_exit_reason_is_failure() {
        let report = BilateralRunReport::default();

        assert_eq!(classify_exit(false, &report), TeleopExitStatus::Failure);
    }

    #[test]
    fn json_report_uses_master_slave_names_and_us_units() {
        let json = serde_json::to_value(sample_json_report()).unwrap();

        assert!(json["metrics"]["max_inter_arm_skew_us"].is_number());
        assert!(json["metrics"].get("left_tx_frames_sent_total").is_none());
        assert!(json["metrics"]["master_tx_frames_sent_total"].is_number());
    }

    #[test]
    fn json_maps_exit_reason_and_stop_attempts_to_snake_case() {
        let json = serde_json::to_value(sample_json_report()).unwrap();

        assert_eq!(json["exit"]["reason"], "submission_fault");
        assert_eq!(json["metrics"]["master_stop_attempt"], "confirmed_sent");
        assert_eq!(json["metrics"]["slave_stop_attempt"], "queue_rejected");
    }

    #[test]
    fn json_report_maps_all_left_right_sdk_metrics_to_master_slave() {
        let json = serde_json::to_value(sample_json_report()).unwrap();

        assert_eq!(json["metrics"]["master_tx_frames_sent_total"], 6);
        assert_eq!(json["metrics"]["slave_tx_frames_sent_total"], 7);
        assert_eq!(json["metrics"]["master_tx_realtime_overwrites_total"], 4);
        assert_eq!(json["metrics"]["slave_tx_realtime_overwrites_total"], 5);
        assert_eq!(json["metrics"]["master_tx_fault_aborts_total"], 8);
        assert_eq!(json["metrics"]["slave_tx_fault_aborts_total"], 9);
        assert_eq!(json["metrics"]["master_stop_attempt"], "confirmed_sent");
        assert_eq!(json["metrics"]["slave_stop_attempt"], "queue_rejected");
    }

    #[test]
    fn json_report_maps_right_submission_arm_to_slave() {
        let sdk_report = BilateralRunReport {
            last_submission_failed_arm: Some(SubmissionArm::Right),
            exit_reason: Some(BilateralExitReason::SubmissionFault),
            ..BilateralRunReport::default()
        };

        let json =
            serde_json::to_value(TeleopJsonReport::from_run(sample_input(false, &sdk_report)))
                .unwrap();

        assert_eq!(json["metrics"]["last_submission_failed_role"], "slave");
    }

    #[test]
    fn json_report_maps_runtime_faults_with_explicit_stable_names() {
        let sdk_report = BilateralRunReport {
            last_runtime_fault_left: Some(RuntimeFaultKind::RxExited),
            last_runtime_fault_right: Some(RuntimeFaultKind::TransportError),
            exit_reason: Some(BilateralExitReason::RuntimeTransportFault),
            ..BilateralRunReport::default()
        };

        let json =
            serde_json::to_value(TeleopJsonReport::from_run(sample_input(true, &sdk_report)))
                .unwrap();

        assert_eq!(json["metrics"]["master_runtime_fault"], "rx_exited");
        assert_eq!(json["metrics"]["slave_runtime_fault"], "transport_error");
    }

    #[test]
    fn json_null_fields_are_present_as_null() {
        let json = serde_json::to_value(sample_json_report_without_optionals()).unwrap();

        assert!(json["calibration"].get("path").unwrap().is_null());
        assert!(json["calibration"].get("created_at_unix_ms").unwrap().is_null());
        assert!(json["exit"].get("last_error").unwrap().is_null());
        assert!(json["metrics"].get("last_submission_failed_role").unwrap().is_null());
        assert!(json["metrics"].get("master_runtime_fault").unwrap().is_null());
        assert!(json["metrics"].get("slave_runtime_fault").unwrap().is_null());
    }

    #[test]
    fn report_serializes_experimental_raw_clock_timing() {
        let sdk_report = BilateralRunReport::default();
        let mut input = sample_input(false, &sdk_report);
        input.timing = Some(ReportTiming {
            timing_source: "calibrated_hw_raw".to_string(),
            experimental: true,
            strict_realtime: false,
            master_clock_drift_ppm: Some(3.0),
            slave_clock_drift_ppm: Some(-2.0),
            master_residual_p95_us: Some(120),
            slave_residual_p95_us: Some(130),
            max_estimated_inter_arm_skew_us: Some(900),
            estimated_inter_arm_skew_p95_us: Some(400),
            clock_health_failures: 0,
        });

        let value = serde_json::to_value(TeleopJsonReport::from_run(input)).unwrap();
        assert_eq!(value["timing"]["timing_source"], "calibrated_hw_raw");
        assert_eq!(value["timing"]["experimental"], true);
        assert_eq!(value["timing"]["strict_realtime"], false);
    }

    #[test]
    fn normal_report_omits_timing() {
        let value = serde_json::to_value(sample_json_report()).unwrap();

        assert!(value.get("timing").is_none());
    }

    #[test]
    fn human_report_includes_experimental_timing_when_present() {
        let mut output = Vec::new();
        let mut report = sample_json_report();
        report.timing = Some(ReportTiming {
            timing_source: "calibrated_hw_raw".to_string(),
            experimental: true,
            strict_realtime: false,
            master_clock_drift_ppm: None,
            slave_clock_drift_ppm: None,
            master_residual_p95_us: None,
            slave_residual_p95_us: None,
            max_estimated_inter_arm_skew_us: Some(900),
            estimated_inter_arm_skew_p95_us: Some(400),
            clock_health_failures: 0,
        });

        write_human_report(&mut output, &report, Duration::from_micros(9876)).unwrap();

        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("timing_source=calibrated_hw_raw"));
        assert!(output.contains("experimental=true"));
        assert!(output.contains("strict_realtime=false"));
        assert!(output.contains("raw_clock max_skew_us=900 p95_skew_us=400"));
    }

    #[test]
    fn human_report_includes_required_diagnostics() {
        let mut output = Vec::new();
        let report = sample_json_report();

        write_human_report(&mut output, &report, Duration::from_micros(9876)).unwrap();

        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("elapsed_us=9876"));
        assert!(output.contains("last_submission_failed_role=master"));
        assert!(output.contains("peer_command_may_have_applied=true"));
        assert!(output.contains("master_runtime_fault=none"));
        assert!(output.contains("slave_runtime_fault=none"));
    }

    #[test]
    fn write_json_report_overwrites_existing_report_via_temp_publish() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("teleop-report.json");
        fs::write(&path, "old report").unwrap();
        let report = sample_json_report();

        write_json_report(&path, &report).unwrap();

        let contents = fs::read_to_string(&path).unwrap();
        assert!(contents.starts_with("{\n"));
        assert!(contents.contains("  \"schema_version\": 1"));
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&contents).unwrap(),
            serde_json::to_value(report).unwrap()
        );
        let temp_entries = fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with(".teleop-report.json.") && name.ends_with(".tmp"))
            .collect::<Vec<_>>();
        assert!(
            temp_entries.is_empty(),
            "leftover temp files: {temp_entries:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn write_json_report_preserves_existing_report_if_temp_publish_fails() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("teleop-report.json");
        let original = "existing valid report";
        fs::write(&path, original).unwrap();

        let original_mode = fs::metadata(dir.path()).unwrap().permissions().mode();
        let mut readonly = fs::metadata(dir.path()).unwrap().permissions();
        readonly.set_mode(0o555);
        fs::set_permissions(dir.path(), readonly).unwrap();

        let probe_path = dir.path().join(".permission-probe.tmp");
        if fs::File::options().write(true).create_new(true).open(&probe_path).is_ok() {
            let _ = fs::remove_file(&probe_path);
            let mut restored = fs::metadata(dir.path()).unwrap().permissions();
            restored.set_mode(original_mode);
            fs::set_permissions(dir.path(), restored).unwrap();
            return;
        }

        let result = write_json_report(&path, &sample_json_report());

        let mut restored = fs::metadata(dir.path()).unwrap().permissions();
        restored.set_mode(original_mode);
        fs::set_permissions(dir.path(), restored).unwrap();

        assert!(result.is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), original);
    }

    fn sample_json_report() -> TeleopJsonReport {
        let sdk_report = BilateralRunReport {
            iterations: 42,
            read_faults: 1,
            submission_faults: 2,
            last_submission_failed_arm: Some(SubmissionArm::Left),
            peer_command_may_have_applied: true,
            deadline_misses: 3,
            max_inter_arm_skew: Duration::from_micros(1200),
            max_real_dt: Duration::from_micros(5100),
            max_cycle_lag: Duration::from_micros(200),
            left_tx_realtime_overwrites_total: 4,
            right_tx_realtime_overwrites_total: 5,
            left_tx_frames_sent_total: 6,
            right_tx_frames_sent_total: 7,
            left_tx_fault_aborts_total: 8,
            right_tx_fault_aborts_total: 9,
            exit_reason: Some(BilateralExitReason::SubmissionFault),
            left_stop_attempt: StopAttemptResult::ConfirmedSent,
            right_stop_attempt: StopAttemptResult::QueueRejected,
            last_error: Some("submission failed".to_string()),
            ..BilateralRunReport::default()
        };

        TeleopJsonReport::from_run(sample_input(false, &sdk_report))
    }

    fn sample_json_report_without_optionals() -> TeleopJsonReport {
        let sdk_report = BilateralRunReport {
            exit_reason: Some(BilateralExitReason::MaxIterations),
            ..BilateralRunReport::default()
        };

        TeleopJsonReport::from_run(TeleopReportInput {
            calibration: ReportCalibration {
                source: "none".to_string(),
                path: None,
                created_at_unix_ms: None,
                max_error_rad: 0.05,
            },
            ..sample_input(false, &sdk_report)
        })
    }

    fn sample_input<'a>(faulted: bool, report: &'a BilateralRunReport) -> TeleopReportInput<'a> {
        TeleopReportInput {
            platform: TeleopPlatform::Linux,
            targets: RoleTargets {
                master: ConcreteTeleopTarget::SocketCan {
                    iface: "can0".to_string(),
                },
                slave: ConcreteTeleopTarget::SocketCan {
                    iface: "can1".to_string(),
                },
            },
            profile: TeleopProfile::Production,
            initial_mode: TeleopMode::MasterFollower,
            final_mode: TeleopMode::Bilateral,
            control: TeleopControlSettings {
                mode: TeleopMode::Bilateral,
                frequency_hz: 200.0,
                track_kp: 8.0,
                track_kd: 1.0,
                master_damping: 0.4,
                reflection_gain: 0.25,
            },
            safety: TeleopSafetySettings {
                profile: TeleopProfile::Production,
                gripper_mirror: true,
            },
            calibration: ReportCalibration {
                source: "file".to_string(),
                path: Some("calibration.toml".to_string()),
                created_at_unix_ms: Some(1_770_000_000_000),
                max_error_rad: 0.05,
            },
            timing: None,
            faulted,
            report,
        }
    }
}
