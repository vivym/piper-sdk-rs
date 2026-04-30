use piper_client::dual_arm::StopAttemptResult;
use piper_client::dual_arm_raw_clock::{
    RawClockRuntimeExitReason, RawClockRuntimeReport, RawClockSide,
};
use piper_sdk::BilateralExitReason;
use piper_svs_collect::collector::{
    FakeCollectorHarness, FakeMujocoFrame, GripperTiming, read_manifest_toml, read_report_json,
};
use piper_svs_collect::episode::manifest::EpisodeStatus;
use piper_svs_collect::episode::wire::read_steps_file;
use piper_tools::raw_clock::RawClockHealth;

fn assert_faulted_manifest_and_report(path: &std::path::Path) {
    let manifest = read_manifest_toml(&path.join("manifest.toml")).unwrap();
    assert_eq!(manifest.status, EpisodeStatus::Faulted);
    let report = read_report_json(&path.join("report.json")).unwrap();
    assert_eq!(report.status, EpisodeStatus::Faulted);
}

fn raw_clock_health_for_fake_tests() -> RawClockHealth {
    RawClockHealth {
        healthy: true,
        sample_count: 2_000,
        window_duration_us: 20_000_000,
        drift_ppm: 0.0,
        residual_p50_us: 50,
        residual_p95_us: 100,
        residual_p99_us: 150,
        residual_max_us: 200,
        sample_gap_max_us: 10_000,
        last_sample_age_us: 1_000,
        raw_timestamp_regressions: 0,
        failure_kind: None,
        reason: None,
    }
}

fn raw_clock_report_for_fake_tests(iterations: usize) -> RawClockRuntimeReport {
    RawClockRuntimeReport {
        master: raw_clock_health_for_fake_tests(),
        slave: raw_clock_health_for_fake_tests(),
        joint_motion: None,
        max_inter_arm_skew_us: 2_000,
        inter_arm_skew_p95_us: 1_000,
        alignment_lag_us: 5_000,
        latest_inter_arm_skew_max_us: 2_000,
        latest_inter_arm_skew_p95_us: 1_000,
        selected_inter_arm_skew_max_us: 2_000,
        selected_inter_arm_skew_p95_us: 1_000,
        clock_health_failures: 0,
        compensation_faults: 0,
        controller_faults: 0,
        telemetry_sink_faults: 0,
        alignment_buffer_misses: 0,
        alignment_buffer_miss_consecutive_max: 0,
        alignment_buffer_miss_consecutive_failures: 0,
        master_residual_max_spikes: 0,
        slave_residual_max_spikes: 0,
        master_residual_max_consecutive_failures: 0,
        slave_residual_max_consecutive_failures: 0,
        read_faults: 0,
        submission_faults: 0,
        last_submission_failed_side: None,
        peer_command_may_have_applied: false,
        runtime_faults: 0,
        master_tx_realtime_overwrites_total: 0,
        slave_tx_realtime_overwrites_total: 0,
        master_tx_frames_sent_total: iterations as u64,
        slave_tx_frames_sent_total: iterations as u64,
        master_tx_fault_aborts_total: 0,
        slave_tx_fault_aborts_total: 0,
        last_runtime_fault_master: None,
        last_runtime_fault_slave: None,
        iterations,
        exit_reason: Some(RawClockRuntimeExitReason::MaxIterations),
        master_stop_attempt: StopAttemptResult::NotAttempted,
        slave_stop_attempt: StopAttemptResult::NotAttempted,
        last_error: None,
    }
}

#[test]
fn fake_workflow_writes_complete_episode() {
    let out = tempfile::tempdir().unwrap();
    let harness = FakeCollectorHarness::new()
        .with_two_socketcan_targets()
        .with_fake_mujoco()
        .with_iterations(3);

    let result = harness.run(out.path()).expect("collector should complete");

    assert_eq!(result.status, EpisodeStatus::Complete);
    assert!(result.path.join("manifest.toml").exists());
    assert!(result.path.join("effective_profile.toml").exists());
    assert!(result.path.join("calibration.toml").exists());
    assert!(result.path.join("steps.bin").exists());
    let manifest = read_manifest_toml(&result.path.join("manifest.toml")).unwrap();
    assert!(manifest.raw_clock.is_none());
    let manifest_text = std::fs::read_to_string(result.path.join("manifest.toml")).unwrap();
    assert!(!manifest_text.contains("[raw_clock]"));
    let report = read_report_json(&result.path.join("report.json")).unwrap();
    assert!(report.raw_clock.is_none());
    let report_text = std::fs::read_to_string(result.path.join("report.json")).unwrap();
    assert!(!report_text.contains("\"raw_clock\""));
    assert_eq!(
        read_steps_file(result.path.join("steps.bin")).unwrap().steps.len(),
        3
    );
}

#[test]
fn writer_startup_failure_finalizes_faulted_episode() {
    let out = tempfile::tempdir().unwrap();
    let harness = FakeCollectorHarness::new()
        .with_two_socketcan_targets()
        .with_fake_mujoco()
        .with_writer_capacity(0);

    let result = harness.run(out.path()).expect("writer startup failure should finalize episode");

    assert_eq!(result.status, EpisodeStatus::Faulted);
    assert_faulted_manifest_and_report(&result.path);
    let report = read_report_json(&result.path.join("report.json")).unwrap();
    assert!(!report.final_flush_result.success);
    assert!(report.writer.flush_failed);
}

#[test]
fn corrupt_steps_file_faults_episode_instead_of_claiming_complete() {
    let out = tempfile::tempdir().unwrap();
    let harness = FakeCollectorHarness::new()
        .with_two_socketcan_targets()
        .with_fake_mujoco()
        .with_iterations(1)
        .with_corrupt_steps_after_finish();

    let result = harness
        .run(out.path())
        .expect("collector should finalize corrupt steps as faulted");

    assert_eq!(result.status, EpisodeStatus::Faulted);
    assert_faulted_manifest_and_report(&result.path);
    let report = read_report_json(&result.path.join("report.json")).unwrap();
    assert!(report.dual_arm.last_error.as_deref().unwrap_or_default().contains("steps.bin"));
}

#[test]
fn fake_workflow_pairs_dynamics_controller_and_shaped_telemetry_per_tick() {
    let out = tempfile::tempdir().unwrap();
    let harness = FakeCollectorHarness::new()
        .with_two_socketcan_targets()
        .with_fake_mujoco_sequence([
            FakeMujocoFrame::new(10_000, 10_100)
                .with_slave_residual_nm([1.0, 0.0, 0.0, 0.0, 0.0, 0.0]),
            FakeMujocoFrame::new(20_000, 20_100)
                .with_slave_residual_nm([2.0, 0.0, 0.0, 0.0, 0.0, 0.0]),
        ])
        .with_iterations(2);

    let result = harness.run(out.path()).expect("collector should complete");
    let steps = read_steps_file(result.path.join("steps.bin")).unwrap();

    assert_eq!(steps.steps[0].master.dynamic_host_rx_mono_us, 10_000);
    assert_eq!(steps.steps[1].master.dynamic_host_rx_mono_us, 20_000);
    assert!(steps.steps[1].r_ee[0] > steps.steps[0].r_ee[0]);
    assert!(steps.steps[0].command.master_tx_finished_host_mono_us > 0);
    assert_ne!(steps.steps[0].command.mit_master_t_ref_nm, [0.0; 6]);
}

#[test]
fn tx_finished_failure_faults_without_successful_row() {
    let out = tempfile::tempdir().unwrap();
    let harness = FakeCollectorHarness::new()
        .with_two_socketcan_targets()
        .with_fake_mujoco()
        .with_master_tx_finished_timeout_at_step(1);

    let result = harness.run(out.path()).expect("collector should finalize faulted episode");

    assert_eq!(result.status, EpisodeStatus::Faulted);
    let steps = read_steps_file(result.path.join("steps.bin")).unwrap();
    assert_eq!(steps.steps.len(), 1);
}

#[test]
fn writer_queue_full_faults_episode_and_does_not_claim_complete() {
    let out = tempfile::tempdir().unwrap();
    let harness = FakeCollectorHarness::new()
        .with_two_socketcan_targets()
        .with_fake_mujoco()
        .with_iterations(3)
        .with_writer_capacity(1)
        .with_paused_writer_until_shutdown();

    let result = harness.run(out.path()).expect("collector should finalize faulted episode");

    assert_eq!(result.status, EpisodeStatus::Faulted);
    assert_eq!(
        result.dual_arm_exit_reason,
        Some(BilateralExitReason::TelemetrySinkFault)
    );
    assert!(result.loop_stopped_before_requested_iterations);
    let report = read_report_json(&result.path.join("report.json")).unwrap();
    assert!(report.writer.queue_full_events > 0);
    assert!(report.writer.dropped_step_count > 0);
}

#[test]
fn writer_flush_failure_prevents_complete_manifest() {
    let out = tempfile::tempdir().unwrap();
    let harness = FakeCollectorHarness::new()
        .with_two_socketcan_targets()
        .with_fake_mujoco()
        .with_iterations(1)
        .with_writer_flush_failure();

    let result = harness.run(out.path()).expect("collector should finalize faulted episode");

    assert_eq!(result.status, EpisodeStatus::Faulted);
    let manifest = read_manifest_toml(&result.path.join("manifest.toml")).unwrap();
    assert_eq!(manifest.status, EpisodeStatus::Faulted);
    let report = read_report_json(&result.path.join("report.json")).unwrap();
    assert!(!report.final_flush_result.success);
    assert!(report.writer.flush_failed);
}

#[test]
fn stale_or_future_gripper_feedback_encodes_unavailable() {
    let out = tempfile::tempdir().unwrap();
    let harness = FakeCollectorHarness::new()
        .with_two_socketcan_targets()
        .with_fake_mujoco()
        .with_iterations(2)
        .with_gripper_feedback_at_step(0, GripperTiming::stale_by_ms(101))
        .with_gripper_feedback_at_step(1, GripperTiming::future_by_ms(1));

    let result = harness.run(out.path()).expect("collector should complete");
    let steps = read_steps_file(result.path.join("steps.bin")).unwrap();

    assert_eq!(steps.steps[0].gripper.master_available, 0);
    assert_eq!(steps.steps[0].gripper.master_host_rx_mono_us, 0);
    assert_eq!(steps.steps[0].gripper.master_age_us, 0);
    assert_eq!(steps.steps[0].gripper.master_position, 0.0);
    assert_eq!(steps.steps[1].gripper.master_available, 0);
    assert_eq!(steps.steps[1].gripper.master_host_rx_mono_us, 0);
    assert_eq!(steps.steps[1].gripper.master_age_us, 0);
    assert_eq!(steps.steps[1].gripper.master_position, 0.0);
}

#[test]
fn raw_can_degradation_sets_status_without_faulting_episode() {
    let out = tempfile::tempdir().unwrap();
    let harness = FakeCollectorHarness::new()
        .with_two_socketcan_targets()
        .with_fake_mujoco()
        .with_iterations(2)
        .with_raw_can_degraded_after_step(0);

    let result = harness.run(out.path()).expect("collector should complete with raw degradation");
    let steps = read_steps_file(result.path.join("steps.bin")).unwrap();

    assert_eq!(result.status, EpisodeStatus::Complete);
    assert_eq!(steps.steps[0].raw_can_status, 1);
    assert_eq!(steps.steps[1].raw_can_status, 2);
    let manifest = read_manifest_toml(&result.path.join("manifest.toml")).unwrap();
    assert_eq!(manifest.status, EpisodeStatus::Complete);
    assert_eq!(
        manifest.raw_can.finalizer_status.as_deref(),
        Some("degraded")
    );
    let report = read_report_json(&result.path.join("report.json")).unwrap();
    assert!(report.raw_can_enabled);
    assert!(report.raw_can_degraded);
    assert_eq!(report.raw_can_finalizer_status.as_deref(), Some("degraded"));
}

#[test]
fn raw_can_requested_pre_mit_fault_is_not_reported_as_degraded() {
    let out = tempfile::tempdir().unwrap();
    let harness = FakeCollectorHarness::new()
        .with_two_socketcan_targets()
        .with_fake_mujoco()
        .with_raw_can_requested()
        .with_startup_fault_before_enable_mit();

    let result = harness.run(out.path()).expect("collector should finalize pre-MIT fault");

    assert_eq!(result.status, EpisodeStatus::Faulted);
    assert_eq!(result.enable_mit_calls, 0);
    let manifest = read_manifest_toml(&result.path.join("manifest.toml")).unwrap();
    assert_eq!(
        manifest.raw_can.finalizer_status.as_deref(),
        Some("not_started")
    );
    let report = read_report_json(&result.path.join("report.json")).unwrap();
    assert!(report.raw_can_enabled);
    assert!(!report.raw_can_degraded);
    assert_eq!(
        report.raw_can_finalizer_status.as_deref(),
        Some("not_started")
    );
}

#[test]
fn cancel_before_mit_enable_finalizes_cancelled_after_flush() {
    let out = tempfile::tempdir().unwrap();
    let harness = FakeCollectorHarness::new()
        .with_two_socketcan_targets()
        .with_fake_mujoco()
        .with_cancel_before_enable_mit();

    let result = harness.run(out.path()).expect("collector should finalize cancelled episode");

    assert_eq!(result.status, EpisodeStatus::Cancelled);
    assert_eq!(result.enable_mit_calls, 0);
    let report = read_report_json(&result.path.join("report.json")).unwrap();
    assert_eq!(report.writer.dropped_step_count, 0);
    assert_eq!(report.writer.queue_full_events, 0);
    assert!(report.final_flush_result.success);
}

#[test]
fn operator_declines_confirmation_finalizes_cancelled_before_mit_enable() {
    let out = tempfile::tempdir().unwrap();
    let harness = FakeCollectorHarness::new()
        .with_two_socketcan_targets()
        .with_fake_mujoco()
        .with_operator_confirmation(false);

    let result = harness.run(out.path()).expect("collector should finalize cancelled episode");

    assert_eq!(result.status, EpisodeStatus::Cancelled);
    assert_eq!(result.enable_mit_calls, 0);
    let manifest = read_manifest_toml(&result.path.join("manifest.toml")).unwrap();
    assert_eq!(manifest.status, EpisodeStatus::Cancelled);
}

#[test]
fn cancel_during_active_control_uses_loop_cancel_signal_and_disables() {
    let out = tempfile::tempdir().unwrap();
    let harness = FakeCollectorHarness::new()
        .with_two_socketcan_targets()
        .with_fake_mujoco()
        .with_cancel_during_active_control_after_steps(1);

    let result = harness.run(out.path()).expect("collector should finalize cancelled episode");

    assert_eq!(result.status, EpisodeStatus::Cancelled);
    assert!(result.disable_called);
    let manifest = read_manifest_toml(&result.path.join("manifest.toml")).unwrap();
    assert_eq!(manifest.status, EpisodeStatus::Cancelled);
}

#[test]
fn raw_clock_fake_workflow_writes_optional_raw_clock_sections() {
    let out = tempfile::tempdir().unwrap();
    let harness = FakeCollectorHarness::new()
        .with_two_socketcan_targets()
        .with_fake_mujoco()
        .with_iterations(2)
        .with_raw_clock_runtime();

    let result = harness.run(out.path()).expect("collector should complete");

    assert_eq!(result.status, EpisodeStatus::Complete);
    assert!(result.disable_called);
    let manifest = read_manifest_toml(&result.path.join("manifest.toml")).unwrap();
    assert!(manifest.raw_clock.is_some());
    assert!(!manifest.gripper.mirror_enabled);
    assert!(manifest.gripper.disable_gripper_effective);
    let report = read_report_json(&result.path.join("report.json")).unwrap();
    assert!(report.raw_clock.is_some());
    assert_eq!(
        report.raw_clock.as_ref().unwrap().timing_source,
        "calibrated_hw_raw"
    );
}

#[test]
fn strict_fake_workflow_keeps_raw_clock_sections_absent() {
    let out = tempfile::tempdir().unwrap();
    let harness = FakeCollectorHarness::new()
        .with_two_socketcan_targets()
        .with_fake_mujoco()
        .with_iterations(1);

    let result = harness.run(out.path()).expect("collector should complete");
    let manifest = read_manifest_toml(&result.path.join("manifest.toml")).unwrap();
    let report = read_report_json(&result.path.join("report.json")).unwrap();

    assert!(manifest.raw_clock.is_none());
    assert!(report.raw_clock.is_none());
}

#[test]
fn raw_clock_cancel_before_enable_still_writes_raw_clock_report_section() {
    let out = tempfile::tempdir().unwrap();
    let harness = FakeCollectorHarness::new()
        .with_two_socketcan_targets()
        .with_fake_mujoco()
        .with_raw_clock_runtime_before_loop()
        .with_cancel_before_enable_mit();

    let result = harness.run(out.path()).expect("collector should finalize cancellation");

    assert_eq!(result.status, EpisodeStatus::Cancelled);
    let manifest = read_manifest_toml(&result.path.join("manifest.toml")).unwrap();
    let report = read_report_json(&result.path.join("report.json")).unwrap();
    assert!(manifest.raw_clock.is_some());
    assert!(report.raw_clock.is_some());
    assert!(report.raw_clock.as_ref().unwrap().final_failure_kind.is_none());
}

#[test]
fn raw_clock_cancel_during_active_control_finalizes_partial_episode() {
    let out = tempfile::tempdir().unwrap();
    let harness = FakeCollectorHarness::new()
        .with_two_socketcan_targets()
        .with_fake_mujoco()
        .with_iterations(3)
        .with_raw_clock_runtime()
        .with_cancel_during_active_control_after_steps(1);

    let result = harness.run(out.path()).expect("collector should finalize cancelled episode");

    assert_eq!(result.status, EpisodeStatus::Cancelled);
    assert!(result.disable_called);
    let manifest = read_manifest_toml(&result.path.join("manifest.toml")).unwrap();
    assert_eq!(manifest.status, EpisodeStatus::Cancelled);
    assert!(manifest.raw_clock.is_some());
    let report = read_report_json(&result.path.join("report.json")).unwrap();
    assert_eq!(report.status, EpisodeStatus::Cancelled);
    assert!(report.raw_clock.is_some());
    assert!(report.raw_clock.as_ref().unwrap().final_failure_kind.is_none());
    let steps = read_steps_file(result.path.join("steps.bin")).unwrap();
    assert_eq!(steps.steps.len(), 1);
}

#[test]
fn raw_clock_gripper_telemetry_maps_into_svs_step() {
    let out = tempfile::tempdir().unwrap();
    let harness = FakeCollectorHarness::new()
        .with_two_socketcan_targets()
        .with_fake_mujoco()
        .with_iterations(1)
        .with_raw_clock_runtime()
        .with_gripper_feedback_at_step(0, GripperTiming::stale_by_ms(10));

    let result = harness.run(out.path()).expect("collector should complete");
    let steps = read_steps_file(result.path.join("steps.bin")).unwrap();

    assert_eq!(steps.steps.len(), 1);
    assert_eq!(steps.steps[0].gripper.master_available, 1);
    assert_eq!(steps.steps[0].gripper.slave_available, 1);
    assert!(steps.steps[0].gripper.master_host_rx_mono_us > 0);
    assert!(steps.steps[0].gripper.slave_host_rx_mono_us > 0);
    assert_eq!(steps.steps[0].gripper.master_position, 0.25);
    assert_eq!(steps.steps[0].gripper.slave_position, 0.25);
}

#[test]
fn raw_clock_fake_workflow_writes_compensation_and_dynamics_steps() {
    let out = tempfile::tempdir().unwrap();
    let harness = FakeCollectorHarness::new()
        .with_two_socketcan_targets()
        .with_fake_mujoco_sequence([
            FakeMujocoFrame::new(10_000, 10_100)
                .with_master_residual_nm([0.7, 0.0, 0.0, 0.0, 0.0, 0.0])
                .with_slave_residual_nm([1.1, 0.0, 0.0, 0.0, 0.0, 0.0]),
            FakeMujocoFrame::new(20_000, 20_100)
                .with_master_residual_nm([0.9, 0.0, 0.0, 0.0, 0.0, 0.0])
                .with_slave_residual_nm([1.3, 0.0, 0.0, 0.0, 0.0, 0.0]),
        ])
        .with_iterations(2)
        .with_raw_clock_runtime();

    let result = harness.run(out.path()).expect("collector should complete");
    let steps = read_steps_file(result.path.join("steps.bin")).unwrap();

    assert_eq!(steps.steps.len(), 2);
    assert_eq!(steps.steps[0].master.dynamic_host_rx_mono_us, 10_000);
    assert_eq!(steps.steps[1].master.dynamic_host_rx_mono_us, 20_000);
    assert_eq!(steps.steps[0].master.tau_model_mujoco_nm, [0.1; 6]);
    assert_eq!(steps.steps[0].slave.tau_model_mujoco_nm, [0.2; 6]);
    assert_eq!(steps.steps[0].master.tau_residual_nm[0], 0.7);
    assert_eq!(steps.steps[1].slave.tau_residual_nm[0], 1.3);
    assert!(steps.steps[1].r_ee[0] > steps.steps[0].r_ee[0]);
    assert!(steps.steps[0].command.master_tx_finished_host_mono_us > 0);
    assert_ne!(steps.steps[0].command.mit_master_t_ref_nm, [0.0; 6]);
}

#[test]
fn raw_clock_missing_tx_finished_finalizes_faulted_episode() {
    let out = tempfile::tempdir().unwrap();
    let harness = FakeCollectorHarness::new()
        .with_two_socketcan_targets()
        .with_fake_mujoco()
        .with_iterations(3)
        .with_raw_clock_runtime()
        .with_master_tx_finished_timeout_at_step(1);

    let result = harness.run(out.path()).expect("collector should finalize faulted episode");

    assert_eq!(result.status, EpisodeStatus::Faulted);
    assert_eq!(
        result.dual_arm_exit_reason,
        Some(BilateralExitReason::TelemetrySinkFault)
    );
    assert!(result.disable_called);
    let steps = read_steps_file(result.path.join("steps.bin")).unwrap();
    assert_eq!(steps.steps.len(), 1);
    let report = read_report_json(&result.path.join("report.json")).unwrap();
    assert_eq!(report.status, EpisodeStatus::Faulted);
    let raw_clock = report.raw_clock.as_ref().expect("raw-clock report");
    assert_eq!(
        raw_clock.final_failure_kind.as_deref(),
        Some("TelemetrySinkFault")
    );
}

#[test]
fn raw_clock_compensation_fault_finalizes_faulted_episode() {
    let out = tempfile::tempdir().unwrap();
    let mut report = raw_clock_report_for_fake_tests(2);
    report.exit_reason = Some(RawClockRuntimeExitReason::CompensationFault);
    report.compensation_faults = 1;
    report.last_error = Some("compensation fault".to_string());

    let harness = FakeCollectorHarness::new()
        .with_two_socketcan_targets()
        .with_fake_mujoco()
        .with_raw_clock_fault_report(report);

    let result = harness.run(out.path()).expect("collector should finalize fault");

    assert_eq!(result.status, EpisodeStatus::Faulted);
    let report = read_report_json(&result.path.join("report.json")).unwrap();
    assert_eq!(report.raw_clock.as_ref().unwrap().compensation_faults, 1);
    assert_eq!(
        report.dual_arm.exit_reason.as_deref(),
        Some("CompensationFault")
    );
}

#[test]
fn raw_clock_telemetry_sink_fault_finalizes_faulted_episode() {
    let out = tempfile::tempdir().unwrap();
    let mut report = raw_clock_report_for_fake_tests(2);
    report.exit_reason = Some(RawClockRuntimeExitReason::TelemetrySinkFault);
    report.telemetry_sink_faults = 1;
    report.submission_faults = 2;
    report.last_submission_failed_side = Some(RawClockSide::Slave);
    report.peer_command_may_have_applied = true;
    report.max_inter_arm_skew_us = 1_234;
    report.master_tx_frames_sent_total = 7;
    report.slave_tx_frames_sent_total = 8;
    report.master_tx_fault_aborts_total = 3;
    report.slave_tx_fault_aborts_total = 4;
    report.master_stop_attempt = StopAttemptResult::Timeout;
    report.slave_stop_attempt = StopAttemptResult::QueueRejected;
    report.last_error = Some("sink failed".to_string());

    let harness = FakeCollectorHarness::new()
        .with_two_socketcan_targets()
        .with_fake_mujoco()
        .with_raw_clock_fault_report(report);

    let result = harness.run(out.path()).expect("collector should finalize fault");

    assert_eq!(result.status, EpisodeStatus::Faulted);
    let report = read_report_json(&result.path.join("report.json")).unwrap();
    assert_eq!(report.raw_clock.as_ref().unwrap().telemetry_sink_faults, 1);
    assert_eq!(report.dual_arm.submission_faults, 2);
    assert_eq!(
        report.dual_arm.last_submission_failed_arm.as_deref(),
        Some("Right")
    );
    assert!(report.dual_arm.peer_command_may_have_applied);
    assert_eq!(report.dual_arm.max_inter_arm_skew_ns, 1_234_000);
    assert_eq!(report.dual_arm.left_tx_frames_sent_total, 7);
    assert_eq!(report.dual_arm.right_tx_frames_sent_total, 8);
    assert_eq!(report.dual_arm.left_tx_fault_aborts_total, 3);
    assert_eq!(report.dual_arm.right_tx_fault_aborts_total, 4);
    assert_eq!(report.dual_arm.left_stop_attempt, "Timeout");
    assert_eq!(report.dual_arm.right_stop_attempt, "QueueRejected");
    assert_eq!(report.dual_arm.last_error.as_deref(), Some("sink failed"));
}

#[test]
fn raw_clock_loaded_calibration_pre_enable_mismatch_fails_before_enable() {
    let out = tempfile::tempdir().unwrap();
    let harness = FakeCollectorHarness::new()
        .with_two_socketcan_targets()
        .with_fake_mujoco()
        .with_raw_clock_loaded_calibration_pre_enable_mismatch();

    let result = harness.run(out.path()).expect("collector should finalize faulted episode");

    assert_eq!(result.status, EpisodeStatus::Faulted);
    assert_eq!(result.enable_mit_calls, 0);
    assert!(!result.disable_called);
    let report = read_report_json(&result.path.join("report.json")).unwrap();
    assert!(report.dual_arm.last_error.as_deref().unwrap_or_default().contains("pre-enable"));
    let raw_clock = report.raw_clock.as_ref().expect("raw-clock report");
    assert_eq!(
        raw_clock.final_failure_kind.as_deref(),
        Some("RuntimeTransportFault")
    );
    assert_eq!(raw_clock.master_residual_p95_us, 0);
    assert_eq!(raw_clock.runtime_faults, 0);
}

#[test]
fn raw_clock_loaded_calibration_post_enable_mismatch_disables_without_loop() {
    let out = tempfile::tempdir().unwrap();
    let harness = FakeCollectorHarness::new()
        .with_two_socketcan_targets()
        .with_fake_mujoco()
        .with_raw_clock_loaded_calibration_post_enable_mismatch();

    let result = harness.run(out.path()).expect("collector should finalize faulted episode");

    assert_eq!(result.status, EpisodeStatus::Faulted);
    assert_eq!(result.enable_mit_calls, 1);
    assert!(result.disable_called);
    let report = read_report_json(&result.path.join("report.json")).unwrap();
    assert!(
        report
            .dual_arm
            .last_error
            .as_deref()
            .unwrap_or_default()
            .contains("post-enable")
    );
    let raw_clock = report.raw_clock.as_ref().expect("raw-clock report");
    assert_eq!(
        raw_clock.final_failure_kind.as_deref(),
        Some("RuntimeTransportFault")
    );
    assert_eq!(raw_clock.master_residual_p95_us, 0);
    assert_eq!(raw_clock.runtime_faults, 0);
}

#[test]
fn raw_clock_capture_replaces_provisional_calibration_with_active_zero_and_save_copy() {
    let out = tempfile::tempdir().unwrap();
    let save_dir = tempfile::tempdir().unwrap();
    let save_path = save_dir.path().join("saved-active.toml");
    let startup_master = [0.10, 0.11, 0.12, 0.13, 0.14, 0.15];
    let startup_slave = [0.20, 0.21, 0.22, 0.23, 0.24, 0.25];
    let active_master = [1.10, 1.11, 1.12, 1.13, 1.14, 1.15];
    let active_slave = [1.20, 1.21, 1.22, 1.23, 1.24, 1.25];
    let harness = FakeCollectorHarness::new()
        .with_two_socketcan_targets()
        .with_fake_mujoco()
        .with_iterations(1)
        .with_raw_clock_runtime()
        .with_raw_clock_capture_positions(
            startup_master,
            startup_slave,
            active_master,
            active_slave,
        )
        .with_save_calibration_path(save_path.clone());

    let result = harness.run(out.path()).expect("collector should complete");

    assert_eq!(result.status, EpisodeStatus::Complete);
    let calibration_path = result.path.join("calibration.toml");
    let calibration_bytes = std::fs::read(&calibration_path).unwrap();
    let episode_calibration =
        piper_svs_collect::calibration::CalibrationFile::from_canonical_bytes(&calibration_bytes)
            .unwrap();
    assert_eq!(episode_calibration.master_zero_rad, active_master);
    assert_eq!(episode_calibration.slave_zero_rad, active_slave);
    assert_ne!(episode_calibration.master_zero_rad, startup_master);
    assert_ne!(episode_calibration.slave_zero_rad, startup_slave);

    let saved_bytes = std::fs::read(&save_path).unwrap();
    assert_eq!(saved_bytes, calibration_bytes);

    let manifest = read_manifest_toml(&result.path.join("manifest.toml")).unwrap();
    assert_eq!(manifest.calibration.master_zero_rad, active_master);
    assert_eq!(manifest.calibration.slave_zero_rad, active_slave);
    assert_eq!(
        manifest.calibration.sha256_hex,
        piper_svs_collect::calibration::sha256_hex(&calibration_bytes)
    );
    assert!(manifest.raw_clock.is_some());
}

#[test]
fn raw_clock_gripper_mirror_enabled_rejects_before_enable() {
    let out = tempfile::tempdir().unwrap();
    let harness = FakeCollectorHarness::new()
        .with_two_socketcan_targets()
        .with_fake_mujoco()
        .with_raw_clock_gripper_mirror_enabled();

    let result = harness
        .run(out.path())
        .expect("collector should reject unsupported gripper mirror");

    assert_eq!(result.status, EpisodeStatus::Faulted);
    assert_eq!(result.enable_mit_calls, 0);
    assert!(!result.disable_called);
    let report = read_report_json(&result.path.join("report.json")).unwrap();
    assert!(
        report
            .dual_arm
            .last_error
            .as_deref()
            .unwrap_or_default()
            .contains("--disable-gripper-mirror")
    );
    assert!(report.raw_clock.is_some());
}
