#![allow(dead_code)]

use anyhow::{Context, Result};
use piper_client::dual_arm::{
    BilateralExitReason, BilateralRunReport, StopAttemptResult, SubmissionArm,
};
use serde::Serialize;
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
                    .map(|fault| debug_to_snake_case(&format!("{fault:?}"))),
                slave_runtime_fault: report
                    .last_runtime_fault_right
                    .map(|fault| debug_to_snake_case(&format!("{fault:?}"))),
            },
        }
    }
}

#[allow(dead_code)]
pub fn print_human_report(report: &TeleopJsonReport) {
    println!("teleop dual-arm report");
    println!(
        "exit: clean={} reason={} faulted={}",
        report.exit.clean,
        report.exit.reason.as_deref().unwrap_or("unknown"),
        report.exit.faulted
    );
    println!(
        "mode: {} -> {} profile={}",
        report.mode.initial, report.mode.final_, report.profile
    );
    println!(
        "targets: master={} slave={}",
        report.targets.master, report.targets.slave
    );
    println!(
        "metrics: iterations={} read_faults={} submission_faults={} deadline_misses={}",
        report.metrics.iterations,
        report.metrics.read_faults,
        report.metrics.submission_faults,
        report.metrics.deadline_misses
    );
    println!(
        "timing: max_inter_arm_skew_us={} max_real_dt_us={} max_cycle_lag_us={}",
        report.metrics.max_inter_arm_skew_us,
        report.metrics.max_real_dt_us,
        report.metrics.max_cycle_lag_us
    );
    println!(
        "master: tx_frames={} overwrites={} fault_aborts={} stop_attempt={}",
        report.metrics.master_tx_frames_sent_total,
        report.metrics.master_tx_realtime_overwrites_total,
        report.metrics.master_tx_fault_aborts_total,
        report.metrics.master_stop_attempt
    );
    println!(
        "slave: tx_frames={} overwrites={} fault_aborts={} stop_attempt={}",
        report.metrics.slave_tx_frames_sent_total,
        report.metrics.slave_tx_realtime_overwrites_total,
        report.metrics.slave_tx_fault_aborts_total,
        report.metrics.slave_stop_attempt
    );
    if let Some(last_error) = &report.exit.last_error {
        println!("last_error: {last_error}");
    }
}

pub fn write_json_report(path: &Path, report: &TeleopJsonReport) -> Result<()> {
    let contents =
        serde_json::to_string_pretty(report).context("failed to serialize teleop report")?;
    std::fs::write(path, contents)
        .with_context(|| format!("failed to write teleop report {}", path.display()))
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

fn debug_to_snake_case(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    for (index, ch) in value.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if index > 0 {
                output.push('_');
            }
            output.push(ch.to_ascii_lowercase());
        } else {
            output.push(ch);
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::teleop::config::{
        TeleopControlSettings, TeleopMode, TeleopProfile, TeleopSafetySettings,
    };
    use crate::teleop::target::{ConcreteTeleopTarget, RoleTargets, TeleopPlatform};
    use piper_client::dual_arm::{
        BilateralExitReason, BilateralRunReport, StopAttemptResult, SubmissionArm,
    };
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
    fn write_json_report_writes_pretty_json_to_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("teleop-report.json");
        let report = sample_json_report();

        write_json_report(&path, &report).unwrap();

        let contents = std::fs::read_to_string(path).unwrap();
        assert!(contents.starts_with("{\n"));
        assert!(contents.contains("  \"schema_version\": 1"));
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&contents).unwrap(),
            serde_json::to_value(report).unwrap()
        );
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
            faulted,
            report,
        }
    }
}
