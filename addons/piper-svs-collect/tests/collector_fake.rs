use piper_sdk::BilateralExitReason;
use piper_svs_collect::collector::{
    FakeCollectorHarness, FakeMujocoFrame, GripperTiming, read_manifest_toml, read_report_json,
};
use piper_svs_collect::episode::manifest::EpisodeStatus;
use piper_svs_collect::episode::wire::read_steps_file;

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
    assert_eq!(
        read_steps_file(result.path.join("steps.bin")).unwrap().steps.len(),
        3
    );
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
