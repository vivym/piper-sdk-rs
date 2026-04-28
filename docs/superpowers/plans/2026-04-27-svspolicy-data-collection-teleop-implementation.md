# SVSPolicy Data-Collection Teleop Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the v1 `piper-svs-collect` data-collection teleoperation binary described by `docs/superpowers/specs/2026-04-27-svspolicy-data-collection-teleop-design.md`.

**Architecture:** Keep the normal SDK and `piper-cli` MuJoCo-free by adding only generic hooks to `piper-driver` and `piper-client`. Put all MuJoCo-specific and SVSPolicy-specific logic in excluded addon crates: extend `addons/piper-physics-mujoco` for end-effector kinematics and create `addons/piper-svs-collect` for config, calibration, cue computation, episode writing, and collector orchestration. Implement in dependency order so every task has local tests and a commit.

**Tech Stack:** Rust 2024, cargo workspaces, SocketCAN StrictRealtime, crossbeam-channel, serde/toml/bincode 1.3, sha2, ryu, nalgebra, mujoco-rs, clap, tempfile, piper-driver, piper-client, piper-control, piper-tools.

---

## Scope Check

This is a large plan, but it is one integrated deliverable: the collector cannot run without driver finished timestamps, client telemetry, MuJoCo end-effector APIs, canonical episode writing, and the SVS control loop. The plan is still decomposed into independent commits that can be reviewed and tested one at a time.

Out of scope:

- Camera capture.
- Policy training, VLA bridge, deployment action chunks, or fast Cartesian executor.
- GS-USB or SoftRealtime dual-arm runtime support.
- Adding MuJoCo to `piper-cli`, `piper-control`, or the default workspace dependency graph.

Use @superpowers:test-driven-development for each implementation task. Use @superpowers:requesting-code-review after each task when executing with subagents. Use @superpowers:verification-before-completion before every commit.

For Rust tasks that create a new module file with tests, the failing-test step also wires the module in the nearest `lib.rs`/`mod.rs` before running `cargo test <filter>`. Do not accept a "0 tests" run as the expected failure.

## File Structure

Modify generic SDK crates:

- `crates/piper-driver/src/command.rs`: add delivery finished receipt data with optional TX-finished timestamp.
- `crates/piper-driver/src/pipeline.rs`: timestamp successful package completion after the final frame send.
- `crates/piper-driver/src/piper.rs`: expose confirmed package helpers that return `MitBatchTxFinished`.
- `crates/piper-driver/src/lib.rs`: re-export `MitBatchTxFinished` so client crates can use `piper_driver::MitBatchTxFinished`.
- `crates/piper-driver/src/state.rs`: keep raw gripper hardware/host timestamps available.
- `crates/piper-client/src/raw_commander.rs`: return confirmed batch timestamps for validated MIT packages.
- `crates/piper-client/src/state/machine.rs`: expose post-quirk MIT command telemetry and confirmed TX-finished APIs.
- `crates/piper-client/src/observer.rs`: expose gripper hardware and host receive timestamps through a generic public type.
- `crates/piper-client/src/dual_arm.rs`: add opt-in confirmed submission mode, generic telemetry sink, per-second slew semantics, gripper timing, and tests.
- `crates/piper-client/src/lib.rs`, `crates/piper-sdk/src/lib.rs`, `crates/piper-sdk/src/prelude.rs`: re-export new generic public types.

Modify MuJoCo addon:

- `addons/piper-physics-mujoco/src/end_effector.rs`: new focused API for site resolution, pose, rotation, translational Jacobian, condition number, and model/runtime identity helpers.
- `addons/piper-physics-mujoco/src/mujoco.rs`: expose narrow model/data accessors needed by `end_effector.rs` without duplicating FFI in the collector.
- `addons/piper-physics-mujoco/src/dual_arm.rs`: keep existing model-torque compensation helpers reusable by the collector.
- `addons/piper-physics-mujoco/src/lib.rs`: re-export end-effector types.
- `addons/piper-physics-mujoco/Cargo.toml`: add test-only dependencies if needed.

Create collector addon:

- `addons/piper-svs-collect/Cargo.toml`: standalone package, not a workspace member.
- `addons/piper-svs-collect/src/main.rs`: CLI entrypoint.
- `addons/piper-svs-collect/src/lib.rs`: module wiring for tests.
- `addons/piper-svs-collect/src/args.rs`: clap options and command shape.
- `addons/piper-svs-collect/src/target.rs`: StrictRealtime SocketCAN-only target validation.
- `addons/piper-svs-collect/src/profile.rs`: task profile structs, canonical effective profile writer, validation.
- `addons/piper-svs-collect/src/calibration.rs`: canonical calibration and mirror-map load/capture/save/persist.
- `addons/piper-svs-collect/src/model_hash.rs`: canonical model tree and embedded model hashing.
- `addons/piper-svs-collect/src/episode/wire.rs`: strict `SvsHeaderV1` and `SvsStepV1` bincode schema.
- `addons/piper-svs-collect/src/episode/manifest.rs`: manifest and report models.
- `addons/piper-svs-collect/src/episode/writer.rs`: bounded non-blocking writer and atomic finalization.
- `addons/piper-svs-collect/src/mujoco_bridge.rs`: collector-owned MuJoCo dynamics bridge implementing SDK compensation and publishing `SvsDynamicsFrame`.
- `addons/piper-svs-collect/src/tick_frame.rs`: snapshot-keyed per-tick staging between MuJoCo bridge, SVS controller, and telemetry sink.
- `addons/piper-svs-collect/src/cue.rs`: residual-to-cue DLS and transforms.
- `addons/piper-svs-collect/src/stiffness.rs`: `SvsStiffnessState` deterministic update.
- `addons/piper-svs-collect/src/controller.rs`: phase-1 data-collection bilateral controller.
- `addons/piper-svs-collect/src/cancel.rs`: shared cancellation token and Ctrl+C handler wiring.
- `addons/piper-svs-collect/src/collector.rs`: startup, loop integration, fault mapping, shutdown.
- `addons/piper-svs-collect/src/raw_can.rs`: optional diagnostic raw side recording status.
- `addons/piper-svs-collect/tests/*.rs`: integration tests using fake backends and fake MuJoCo providers.

Modify workspace metadata:

- `Cargo.toml`: add `addons/piper-svs-collect` to `[workspace].exclude`; keep `addons/piper-physics-mujoco` excluded.
- `.gitignore`: no further change expected.

## Task 1: Add Collector Addon Skeleton and Workspace Exclusion

**Files:**
- Modify: `Cargo.toml`
- Create: `addons/piper-svs-collect/Cargo.toml`
- Create: `addons/piper-svs-collect/src/main.rs`
- Create: `addons/piper-svs-collect/src/lib.rs`
- Create: `addons/piper-svs-collect/src/args.rs`
- Test: `addons/piper-svs-collect/src/args.rs`

- [ ] **Step 1: Write the failing CLI shape test**

Add this unit test to `addons/piper-svs-collect/src/args.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn parses_required_svs_collect_args() {
        let args = Args::try_parse_from([
            "piper-svs-collect",
            "--master-target", "socketcan:can0",
            "--slave-target", "socketcan:can1",
            "--model-dir", "/tmp/model",
            "--output-dir", "/tmp/out",
            "--task", "wiping",
            "--operator", "viv",
            "--yes",
        ])
        .expect("args should parse");

        assert_eq!(args.master_target, "socketcan:can0");
        assert_eq!(args.slave_target, "socketcan:can1");
        assert_eq!(args.task.as_deref(), Some("wiping"));
        assert!(args.yes);
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --manifest-path addons/piper-svs-collect/Cargo.toml parses_required_svs_collect_args`

Expected: FAIL because the package and `Args` do not exist.

- [ ] **Step 3: Create the standalone package**

Create `addons/piper-svs-collect/Cargo.toml`:

```toml
[package]
name = "piper-svs-collect"
version = "0.0.3"
edition = "2024"
authors = ["Ming Yang"]
license = "MIT"
repository = "https://github.com/vivym/piper-sdk-rs"
description = "SVSPolicy data-collection teleoperation collector for Piper dual-arm setups"

[dependencies]
anyhow = "1.0"
bincode = "1.3"
clap = { version = "4.5", features = ["derive"] }
crossbeam-channel = "0.5.15"
ctrlc = "3.5"
nalgebra = { version = "0.32", features = ["std"] }
piper-client = { path = "../../crates/piper-client", version = "0.0.3", default-features = false, features = ["socketcan"] }
piper-control = { path = "../../crates/piper-control", version = "0.0.3" }
piper-driver = { path = "../../crates/piper-driver", version = "0.0.3", default-features = false, features = ["socketcan"] }
piper-physics = { path = "../piper-physics-mujoco", version = "0.0.3" }
piper-sdk = { path = "../../crates/piper-sdk", version = "0.0.3", default-features = false, features = ["socketcan"] }
piper-tools = { path = "../../crates/piper-tools", version = "0.0.3" }
ryu = "1.0"
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
sha2 = "0.10"
getrandom = "0.3"
tempfile = "3.24"
thiserror = "2.0.17"
toml = "0.9"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }

[dev-dependencies]
assert_cmd = "2.0"
predicates = "3.1"
```

Modify root `Cargo.toml`:

```toml
exclude = ["addons/piper-physics-mujoco", "addons/piper-svs-collect"]
```

Create `src/lib.rs`:

```rust
pub mod args;
```

Create `src/main.rs`:

```rust
use anyhow::Result;
use clap::Parser;
use piper_svs_collect::args::Args;

fn main() -> Result<()> {
    let _args = Args::parse();
    Ok(())
}
```

Create `src/args.rs`:

```rust
use std::path::PathBuf;

use clap::Parser;

#[derive(Debug, Parser)]
#[command(name = "piper-svs-collect")]
pub struct Args {
    #[arg(long)]
    pub master_target: String,
    #[arg(long)]
    pub slave_target: String,
    #[arg(long)]
    pub baud_rate: Option<u32>,
    #[arg(long)]
    pub model_dir: Option<PathBuf>,
    #[arg(long)]
    pub use_standard_model_path: bool,
    #[arg(long)]
    pub use_embedded_model: bool,
    #[arg(long)]
    pub task_profile: Option<PathBuf>,
    #[arg(long)]
    pub output_dir: PathBuf,
    #[arg(long)]
    pub calibration_file: Option<PathBuf>,
    #[arg(long)]
    pub save_calibration: Option<PathBuf>,
    #[arg(long)]
    pub calibration_max_error_rad: Option<f64>,
    #[arg(long)]
    pub mirror_map: Option<String>,
    #[arg(long)]
    pub operator: Option<String>,
    #[arg(long)]
    pub task: Option<String>,
    #[arg(long)]
    pub notes: Option<String>,
    #[arg(long)]
    pub raw_can: bool,
    #[arg(long)]
    pub disable_gripper_mirror: bool,
    #[arg(long)]
    pub max_iterations: Option<u64>,
    #[arg(long, default_value = "spin")]
    pub timing_mode: String,
    #[arg(long)]
    pub yes: bool,
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test --manifest-path addons/piper-svs-collect/Cargo.toml parses_required_svs_collect_args`

Expected: PASS.

- [ ] **Step 5: Verify default workspace still excludes collector**

Run: `cargo metadata --no-deps --format-version 1 | rg '"name":"piper-svs-collect"'`

Expected: FAIL/no output because the collector is excluded from the default workspace.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml addons/piper-svs-collect
git commit -m "feat: scaffold svs collector addon"
```

## Task 2: Add Driver TX-Finished Timestamp Receipts

**Files:**
- Modify: `crates/piper-driver/src/command.rs`
- Modify: `crates/piper-driver/src/pipeline.rs`
- Modify: `crates/piper-driver/src/piper.rs`
- Modify: `crates/piper-driver/src/lib.rs`
- Test: `crates/piper-driver/src/piper.rs`
- Test: `crates/piper-driver/src/pipeline.rs`

- [ ] **Step 1: Write failing tests for finished timestamp after package completion**

Add focused tests next to existing delivery/ack tests in `crates/piper-driver/src/piper.rs`:

```rust
#[test]
fn realtime_confirmed_package_returns_finished_timestamp_after_success() {
    let sent_frames = Arc::new(Mutex::new(Vec::new()));
    let send_return_times = Arc::new(Mutex::new(Vec::new()));
    let driver = Piper::new_dual_thread_parts_unvalidated(
        MockRxAdapter,
        RecordingTxAdapter {
            sent_frames: sent_frames.clone(),
            send_return_times: send_return_times.clone(),
        },
        None,
    )
    .unwrap();
    let frames = [
        test_frame(0x15a),
        test_frame(0x15b),
        test_frame(0x15c),
        test_frame(0x15d),
        test_frame(0x15e),
        test_frame(0x15f),
    ];

    let receipt = driver
        .send_realtime_package_confirmed_finished(frames, Duration::from_millis(50))
        .expect("confirmed package should succeed");

    assert!(receipt.host_finished_mono_us > 0);
    assert_eq!(sent_frames.lock().unwrap().len(), 6);
    assert!(
        receipt.host_finished_mono_us >= *send_return_times.lock().unwrap().last().unwrap(),
        "finished timestamp must be sampled after the last successful frame send"
    );
}

#[test]
fn realtime_confirmed_package_timeout_returns_no_finished_timestamp() {
    let driver = Piper::new_dual_thread_parts_unvalidated(
        MockRxAdapter,
        FailOnNthTxAdapter { fail_on: 3, sends: 0 },
        None,
    )
    .unwrap();
    let err = driver
        .send_realtime_package_confirmed_finished(test_mit_frames(), Duration::from_millis(200))
        .expect_err("partial delivery must fail");

    assert!(matches!(err, DriverError::RealtimeDeliveryFailed { .. } | DriverError::RealtimeDeliveryTimeout));
}
```

Extend the existing `RecordingTxAdapter` test helper with a `send_return_times: Arc<Mutex<Vec<u64>>>` field, or create a nearby `TimingRecordingTxAdapter`, and push `crate::heartbeat::monotonic_micros().max(1)` immediately after every successful `send_control` return point. The behavior matters: success returns a timestamp after the last frame send, failure returns no timestamp.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p piper-driver confirmed_package -- --nocapture`

Expected: FAIL because no finished timestamp API exists.

- [ ] **Step 3: Add receipt types and preserve existing APIs**

In `crates/piper-driver/src/command.rs`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeliveryReceipt {
    pub host_finished_mono_us: Option<u64>,
}

impl DeliveryReceipt {
    pub const fn none() -> Self {
        Self {
            host_finished_mono_us: None,
        }
    }

    pub const fn finished_at(host_finished_mono_us: u64) -> Self {
        Self {
            host_finished_mono_us: Some(host_finished_mono_us),
        }
    }
}

#[derive(Debug)]
pub enum DeliveryPhase {
    Committed { host_commit_mono_us: u64 },
    Finished(Result<DeliveryReceipt, DriverError>),
}
```

Update helper construction sites from `Finished(Ok(()))` to `Finished(Ok(DeliveryReceipt::none()))` where no finished timestamp is meaningful.

- [ ] **Step 4: Timestamp package finish in the TX worker**

In `crates/piper-driver/src/pipeline.rs`, when a realtime package completes with `sent_count == total_frames` and no error:

```rust
let receipt = if no_delivery_error && sent_count == total_frames {
    crate::command::DeliveryReceipt::finished_at(crate::heartbeat::monotonic_micros().max(1))
} else {
    crate::command::DeliveryReceipt::none()
};
let _ = ack.send(crate::command::DeliveryPhase::Finished(
    delivery_error.map_or(Ok(receipt), Err),
));
```

Do the same for reliable package paths if the helper will be shared. Do not use the existing `Committed` timestamp as the finished timestamp.

- [ ] **Step 5: Add a new public helper without breaking old callers**

In `crates/piper-driver/src/piper.rs`, keep existing `wait_for_delivery_result` returning `Result<()>`. Add:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MitBatchTxFinished {
    pub host_finished_mono_us: u64,
}

pub fn wait_for_delivery_finished<F>(
    ack_rx: Receiver<DeliveryPhase>,
    deadline: Instant,
    timeout_error: F,
) -> Result<MitBatchTxFinished, DriverError>
where
    F: Fn() -> DriverError,
{
    let receipt = wait_for_delivery_result_with_receipt(ack_rx, deadline, timeout_error)?;
    let host_finished_mono_us = receipt
        .host_finished_mono_us
        .ok_or(DriverError::Timeout)?;
    Ok(MitBatchTxFinished {
        host_finished_mono_us,
    })
}
```

Implement `wait_for_delivery_result_with_receipt` next to `wait_for_delivery_result_with_commit`. It must wait for the final `DeliveryPhase::Finished(Ok(receipt))` only until the caller's absolute deadline and return `timeout_error()` if `Finished` does not arrive before that deadline, even if a `Committed` phase already arrived. This differs intentionally from old commit-marker semantics because the SVS collector's confirmed path is bounded by the control tick submission deadline.

Expose `send_realtime_package_confirmed_finished` or the nearest equivalent on the strict realtime driver path. Keep old `send_realtime_package_confirmed` API behavior.

In `crates/piper-driver/src/lib.rs`, re-export the public receipt type:

```rust
pub use piper::MitBatchTxFinished;
```

- [ ] **Step 6: Run driver tests**

Run: `cargo test -p piper-driver confirmed_package -- --nocapture`

Expected: PASS.

- [ ] **Step 7: Run existing delivery tests**

Run: `cargo test -p piper-driver DeliveryPhase -- --nocapture`

Expected: PASS or no tests matched. If no tests matched, run these separately:

```bash
cargo test -p piper-driver --lib command -- --nocapture
cargo test -p piper-driver --lib pipeline -- --nocapture
```

- [ ] **Step 8: Commit**

```bash
git add crates/piper-driver/src/command.rs crates/piper-driver/src/pipeline.rs crates/piper-driver/src/piper.rs crates/piper-driver/src/lib.rs
git commit -m "feat: expose tx-finished delivery timestamps"
```

## Task 3: Add Client MIT Batch Receipts and Gripper Timestamp State

**Files:**
- Modify: `crates/piper-client/src/raw_commander.rs`
- Modify: `crates/piper-client/src/state/machine.rs`
- Modify: `crates/piper-client/src/observer.rs`
- Modify: `crates/piper-client/src/lib.rs`
- Modify: `crates/piper-sdk/src/lib.rs`
- Modify: `crates/piper-sdk/src/prelude.rs`
- Test: `crates/piper-client/src/state/machine.rs`
- Test: `crates/piper-client/src/observer.rs`

- [ ] **Step 1: Write failing test for post-quirk `t_ref` and TX-finished receipt**

In `crates/piper-client/src/state/machine.rs`, add:

```rust
#[test]
fn command_torques_confirmed_finished_returns_post_quirk_t_refs() {
    let robot = active_mit_robot_with_quirks_and_successful_tx();
    let positions = JointArray::from([Rad(0.0); 6]);
    let velocities = JointArray::from([0.0; 6]);
    let kp = JointArray::from([0.0; 6]);
    let kd = JointArray::from([0.0; 6]);
    let torques = JointArray::from([
        NewtonMeter(0.1),
        NewtonMeter(0.2),
        NewtonMeter(0.3),
        NewtonMeter(0.4),
        NewtonMeter(0.5),
        NewtonMeter(0.6),
    ]);

    let receipt = robot
        .command_torques_confirmed_finished(&positions, &velocities, &kp, &kd, &torques, Duration::from_millis(50))
        .expect("confirmed send should succeed");

    assert!(receipt.tx_finished.host_finished_mono_us > 0);
    assert_eq!(receipt.mit_t_ref_nm.len(), 6);
    assert!(receipt.mit_t_ref_nm.iter().all(|value| value.is_finite()));
}
```

- [ ] **Step 2: Write failing test for gripper timestamp exposure**

In `crates/piper-client/src/observer.rs`, add:

```rust
#[test]
fn gripper_state_exposes_hardware_and_host_timestamps() {
    let observer = observer_with_gripper_feedback(12_345, 67_890, 50.0, 1.0);
    let gripper = observer.gripper_state();

    assert_eq!(gripper.hardware_timestamp_us, 12_345);
    assert_eq!(gripper.host_rx_mono_us, 67_890);
    assert_eq!(gripper.position, 0.5);
    assert!(gripper.enabled);
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run:

```bash
cargo test -p piper-client command_torques_confirmed_finished -- --nocapture
cargo test -p piper-client gripper_state_exposes -- --nocapture
```

Expected: FAIL because the receipt API and public gripper timestamp fields do not exist.

- [ ] **Step 4: Add public receipt types**

In `crates/piper-client/src/state/machine.rs` or a small nearby module:

```rust
#[derive(Debug, Clone, PartialEq)]
pub struct ConfirmedMitBatch {
    pub tx_finished: piper_driver::MitBatchTxFinished,
    pub mit_t_ref_nm: [f64; 6],
}
```

Add `command_torques_confirmed_finished` that reuses `build_validated_mit_command_batch`, captures post-quirk `t_ref` from the resulting `MitControlCommand`s, and calls `RawCommander::send_validated_mit_command_batch_confirmed_finished`.

- [ ] **Step 5: Add raw commander finished API**

In `crates/piper-client/src/raw_commander.rs`:

```rust
pub(crate) fn send_validated_mit_command_batch_confirmed_finished(
    &self,
    commands: [MitControlCommand; 6],
    timeout: Duration,
) -> Result<piper_driver::MitBatchTxFinished> {
    let frames = commands.map(MitControlCommand::to_frame);
    self.driver
        .send_realtime_package_confirmed_finished(frames, timeout)
        .map_err(Into::into)
}
```

Use the actual driver method name from Task 2.

- [ ] **Step 6: Extend public gripper state**

In `crates/piper-client/src/observer.rs`:

```rust
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GripperState {
    pub position: f64,
    pub effort: f64,
    pub enabled: bool,
    pub hardware_timestamp_us: u64,
    pub host_rx_mono_us: u64,
}
```

Populate these from `piper_driver::state::GripperState`.

- [ ] **Step 7: Run client tests**

Run:

```bash
cargo test -p piper-client command_torques_confirmed_finished -- --nocapture
cargo test -p piper-client gripper_state_exposes -- --nocapture
```

Expected: PASS.

- [ ] **Step 8: Run compatibility tests**

Run:

```bash
cargo test -p piper-client gripper_state -- --nocapture
cargo test -p piper-client command_torques_confirmed -- --nocapture
```

Expected: PASS. Existing tests may need expected structs updated with timestamp defaults.

- [ ] **Step 9: Commit**

```bash
git add crates/piper-client/src/raw_commander.rs crates/piper-client/src/state/machine.rs crates/piper-client/src/observer.rs crates/piper-client/src/lib.rs crates/piper-sdk/src/lib.rs crates/piper-sdk/src/prelude.rs
git commit -m "feat: return confirmed mit telemetry"
```

## Task 4: Add Dual-Arm Submission Mode, Telemetry Sink, and Nm/s Slew

**Files:**
- Modify: `crates/piper-client/src/dual_arm.rs`
- Modify: `apps/cli/src/teleop/config.rs`
- Modify: `apps/cli/TELEOP_DUAL_ARM.md`
- Test: `crates/piper-client/src/dual_arm.rs`
- Test: `apps/cli/src/teleop/config.rs`

- [ ] **Step 1: Write failing tests for default unconfirmed submission**

In `crates/piper-client/src/dual_arm.rs`:

```rust
#[test]
fn bilateral_loop_config_defaults_to_unconfirmed_submission() {
    let cfg = BilateralLoopConfig::default();
    assert_eq!(cfg.submission_mode, BilateralSubmissionMode::Unconfirmed);
}

#[test]
fn confirmed_submission_fault_writes_no_successful_telemetry() {
    let sink = RecordingTelemetrySink::default();
    let cfg = BilateralLoopConfig {
        submission_mode: BilateralSubmissionMode::ConfirmedTxFinished,
        telemetry_sink: Some(Arc::new(sink.clone())),
        ..BilateralLoopConfig::default()
    };

    let exit = run_fake_dual_arm_with_left_confirm_timeout(cfg);

    assert!(matches!(exit, DualArmLoopExit::Faulted { .. }));
    assert_eq!(sink.accepted_rows(), 0);
}

#[test]
fn telemetry_sink_error_stops_loop_without_another_cycle() {
    let sink = FailingTelemetrySink::new("writer backpressure");
    let cfg = BilateralLoopConfig {
        telemetry_sink: Some(Arc::new(sink.clone())),
        ..BilateralLoopConfig::default()
    };

    let exit = run_fake_dual_arm_one_successful_submission(cfg);

    assert!(matches!(exit, DualArmLoopExit::Standby { .. }));
    assert_eq!(exit.report().exit_reason, Some(BilateralExitReason::TelemetrySinkFault));
    assert_eq!(sink.calls(), 1);
}
```

- [ ] **Step 2: Write failing test for Nm/s slew migration**

```rust
#[test]
fn master_interaction_slew_limit_is_per_second() {
    let cfg = BilateralLoopConfig {
        master_interaction_slew_limit_nm_per_s: JointArray::splat(NewtonMeter(50.0)),
        ..BilateralLoopConfig::default()
    };
    let mut state = OutputShapingState::default();
    let mut command = command_with_master_interaction(NewtonMeter(10.0));

    apply_output_shaping(&cfg, &zero_snapshot(), Duration::from_millis(5), &mut state, &mut command);

    assert_eq!(command.master_interaction_torque[Joint::J1], NewtonMeter(0.25));
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run:

```bash
cargo test -p piper-client submission_mode -- --nocapture
cargo test -p piper-client telemetry_sink_error -- --nocapture
cargo test -p piper-client master_interaction_slew_limit_is_per_second -- --nocapture
```

Expected: FAIL because fields and telemetry sink do not exist and slew is still per tick.

- [ ] **Step 4: Add generic submission and telemetry types**

In `crates/piper-client/src/dual_arm.rs`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BilateralSubmissionMode {
    Unconfirmed,
    ConfirmedTxFinished,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BilateralLoopTimingTelemetry {
    pub scheduler_tick_start_host_mono_us: u64,
    pub control_frame_host_mono_us: u64,
    pub previous_control_frame_host_mono_us: Option<u64>,
    pub raw_dt_us: u64,
    pub clamped_dt_us: u64,
    pub nominal_period_us: u64,
    pub submission_deadline_mono_us: u64,
    pub deadline_missed: bool,
}

#[derive(Debug, thiserror::Error)]
#[error("bilateral telemetry sink error: {message}")]
pub struct BilateralTelemetrySinkError {
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BilateralGripperCommandStatus {
    None,
    Sent,
    Skipped,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BilateralLoopGripperTelemetry {
    pub mirror_enabled: bool,
    pub master_available: bool,
    pub slave_available: bool,
    pub master_hw_timestamp_us: u64,
    pub slave_hw_timestamp_us: u64,
    pub master_host_rx_mono_us: u64,
    pub slave_host_rx_mono_us: u64,
    pub master_age_us: u64,
    pub slave_age_us: u64,
    pub master_position: f64,
    pub master_effort: f64,
    pub slave_position: f64,
    pub slave_effort: f64,
    pub command_status: BilateralGripperCommandStatus,
    pub command_position: f64,
    pub command_effort: f64,
}

pub trait BilateralLoopTelemetrySink: Send + Sync {
    fn on_tick(&self, telemetry: &BilateralLoopTelemetry) -> Result<(), BilateralTelemetrySinkError>;
}
```

Add a `BilateralLoopTelemetry` struct containing the generic `BilateralControlFrame` input, controller command before output shaping, shaped command, compensation, gripper, final torque, post-quirk `t_ref`, TX-finished timestamps, and timing fields.

Add `TelemetrySinkFault` to `BilateralExitReason` so sink-requested stops are distinguishable from operator cancellation and transport faults.

Make per-tick timing concrete in the generic loop:

- Sample `scheduler_tick_start_host_mono_us` before snapshot reads.
- Read the `DualArmSnapshot` and both gripper states for every accepted tick, before compensation, controller execution, command submission, gripper mirroring, or telemetry enqueue.
- Sample `control_frame_host_mono_us = piper_driver::heartbeat::monotonic_micros()` immediately after snapshot plus gripper reads.
- Do not let warmup ticks seed `previous_control_frame_host_mono_us`; the first post-warmup telemetry tick uses `raw_dt_us = nominal_period_us`.
- Compute `raw_dt_us`, `clamped_dt_us`, `nominal_period_us`, `submission_deadline_mono_us`, and `deadline_missed` from these timestamps and carry them in `BilateralLoopTimingTelemetry`.
- Convert `clamped_dt_us` to `Duration` once and use that same clamped control-frame `dt` for `BilateralDynamicsCompensator::compute`, `BilateralController::tick_with_compensation`, SVS stiffness updates, and `apply_output_shaping`. `SvsStepV1.dt_us` must equal the same clamped duration used for the control math.
- Carry the sampled gripper states and their ages relative to `control_frame_host_mono_us` in telemetry. If a gripper host receive timestamp is zero, older than `cfg.gripper.max_feedback_age`, or newer than `control_frame_host_mono_us`, mark that side unavailable and zero the corresponding timestamp, age, position, and effort in the collector's `SvsGripperStepV1`.
- Record the gripper mirror decision and command outcome in `BilateralLoopGripperTelemetry`: `None` when mirror is disabled for the tick, `Skipped` when enabled but no command is sent because feedback is unavailable or inside deadband, `Sent` when `set_gripper` succeeds, and `Failed` when `set_gripper` returns an error. The loop must not discard `set_gripper` errors silently; it records `Failed` in telemetry while preserving the existing control-loop fault policy unless the repo's current gripper path already treats gripper failure as fatal.

Extend `GripperTeleopConfig` with a generic feedback-age bound used for telemetry and mirror decisions:

```rust
pub struct GripperTeleopConfig {
    pub enabled: bool,
    pub update_divider: usize,
    pub position_deadband: f64,
    pub effort_scale: f64,
    pub max_feedback_age: Duration,
}
```

Default `max_feedback_age` to `Duration::from_millis(100)`.

- [ ] **Step 5: Update loop config and output shaping**

Rename `master_interaction_slew_limit` to `master_interaction_slew_limit_nm_per_s` in `BilateralLoopConfig`. Default to `50.0` Nm/s to preserve `0.25` Nm/tick at 200 Hz. Keep existing output-shaping fields (`master_interaction_lpf_cutoff_hz`, `master_interaction_limit`, `slave_feedforward_limit`, `master_passivity_enabled`, and `master_passivity_max_damping`) as generic SDK config fields and make sure telemetry reports their shaped outputs.

In `apply_output_shaping`:

```rust
let limit = cfg.master_interaction_slew_limit_nm_per_s[joint].0 * dt_sec;
let delta = (filtered.0 - last.0).clamp(-limit, limit);
```

`dt_sec` must be derived from the same clamped control-frame `dt` described in Step 4, not from wall-clock time sampled later in the tick.

- [ ] **Step 6: Implement opt-in confirmed submission path**

In `run_bilateral_inner`, branch on `cfg.submission_mode`:

```rust
match cfg.submission_mode {
    BilateralSubmissionMode::Unconfirmed => {
        active.right.command_torques(...)?;
        active.left.command_torques(...)?;
    }
    BilateralSubmissionMode::ConfirmedTxFinished => {
        let deadline = tick_start + nominal_period;
        let right = match active.right.command_torques_confirmed_finished(..., remaining(deadline)) {
            Ok(receipt) => receipt,
            Err(err) => return submission_fault_exit(SubmissionArm::Right, false, err),
        };
        let left = match active.left.command_torques_confirmed_finished(..., remaining(deadline)) {
            Ok(receipt) => receipt,
            Err(err) => return submission_fault_exit(SubmissionArm::Left, true, err),
        };
        telemetry.master_tx_finished_host_mono_us = Some(left.tx_finished.host_finished_mono_us);
        telemetry.slave_tx_finished_host_mono_us = Some(right.tx_finished.host_finished_mono_us);
    }
}
```

The confirmed branch must not call the telemetry sink if either arm lacks a finished timestamp. Confirmed submission failures must preserve the existing submission-fault report fields and fault-shutdown path: increment `submission_faults`, set `last_submission_failed_arm`, set `peer_command_may_have_applied` exactly like the existing sequential send path, set `BilateralExitReason::SubmissionFault`, and return `DualArmLoopExit::Faulted`.

After command submission, gripper mirroring, and telemetry construction, call the optional sink. If it returns `Err`, set `BilateralExitReason::TelemetrySinkFault`, copy the error message into the run report, disable both arms through the bounded normal disable path, and return without starting another control cycle. This error path must remain generic; do not mention SVS, MuJoCo, writer queues, or datasets in `piper-client`.

- [ ] **Step 7: Update CLI config to preserve unconfirmed default**

In `apps/cli/src/teleop/config.rs`, set:

```rust
config.submission_mode = BilateralSubmissionMode::Unconfirmed;
config.master_interaction_slew_limit_nm_per_s =
    old_profile.master_interaction_slew_limit.map(|per_tick| per_tick * resolved.loop_hz);
```

If the existing profile format already stores the value as per tick, rename the config field and update docs to state the CLI's resolved runtime uses Nm/s with behavior-preserving migration.

- [ ] **Step 8: Run tests**

Run:

```bash
cargo test -p piper-client submission_mode -- --nocapture
cargo test -p piper-client telemetry_sink_error -- --nocapture
cargo test -p piper-client master_interaction_slew_limit_is_per_second -- --nocapture
```

Expected: PASS.

Run: `cargo test -p piper-cli teleop -- --nocapture`

Expected: PASS.

- [ ] **Step 9: Commit**

```bash
git add crates/piper-client/src/dual_arm.rs apps/cli/src/teleop/config.rs apps/cli/TELEOP_DUAL_ARM.md
git commit -m "feat: add dual-arm telemetry submission modes"
```

## Task 5: Add MuJoCo End-Effector Kinematics API

**Files:**
- Create: `addons/piper-physics-mujoco/src/end_effector.rs`
- Modify: `addons/piper-physics-mujoco/src/lib.rs`
- Modify: `addons/piper-physics-mujoco/src/mujoco.rs`
- Test: `addons/piper-physics-mujoco/src/end_effector.rs`

- [ ] **Step 1: Write failing tests for explicit site resolution**

Create `addons/piper-physics-mujoco/src/end_effector.rs` with tests first:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_empty_end_effector_site_name() {
        let selector = EndEffectorSelector { site_name: String::new() };
        assert!(selector.validate().is_err());
    }

    #[test]
    fn computes_condition_number_from_singular_values() {
        let values = [10.0, 2.0, 0.5];
        assert_eq!(condition_number_from_singular_values(values), 20.0);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --manifest-path addons/piper-physics-mujoco/Cargo.toml end_effector -- --nocapture`

Expected: FAIL because the module does not exist.

- [ ] **Step 3: Implement public types**

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndEffectorSelector {
    pub site_name: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EndEffectorKinematics {
    pub position_base_m: [f64; 3],
    pub rotation_base_from_ee: [[f64; 3]; 3],
    pub translational_jacobian_base: [[f64; 6]; 3],
    pub jacobian_condition: f64,
}

impl EndEffectorSelector {
    pub fn validate(&self) -> Result<(), PhysicsError> {
        if self.site_name.trim().is_empty() {
            return Err(PhysicsError::InvalidInput("end-effector site name is required".to_string()));
        }
        Ok(())
    }
}
```

- [ ] **Step 4: Add MuJoCo-backed kinematics method**

In `MujocoGravityCompensation`, add:

```rust
pub fn end_effector_kinematics(
    &mut self,
    selector: &EndEffectorSelector,
    q: &JointState,
) -> Result<EndEffectorKinematics, PhysicsError> {
    selector.validate()?;
    self.set_joint_positions(q)?;
    self.forward()?;
    let site_id = self.resolve_unique_site(&selector.site_name)?;
    self.compute_site_kinematics(site_id)
}
```

Use `mj_jacSite` through `mujoco_rs::mujoco_c`, convert MuJoCo row-major matrices correctly, and compute the 3x6 translational Jacobian condition number with nalgebra SVD.

- [ ] **Step 5: Add model/runtime identity helpers**

Add functions that the collector can call:

```rust
pub fn mujoco_runtime_version_string() -> String;
pub fn loaded_mujoco_library_identity() -> Result<MujocoRuntimeIdentity, PhysicsError>;
```

If shared-library hashing is not portable in the first implementation, return a clear error so the collector can fail before MIT enable as the spec requires.

- [ ] **Step 6: Run addon tests**

Run: `cargo test --manifest-path addons/piper-physics-mujoco/Cargo.toml --all-targets`

Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add addons/piper-physics-mujoco/src/end_effector.rs addons/piper-physics-mujoco/src/lib.rs addons/piper-physics-mujoco/src/mujoco.rs
git commit -m "feat: expose mujoco end-effector kinematics"
```

## Task 6: Implement Collector Target, Profile, and Canonical TOML

**Files:**
- Create: `addons/piper-svs-collect/src/target.rs`
- Create: `addons/piper-svs-collect/src/profile.rs`
- Modify: `addons/piper-svs-collect/src/lib.rs`
- Test: `addons/piper-svs-collect/src/target.rs`
- Test: `addons/piper-svs-collect/src/profile.rs`

- [ ] **Step 1: Write failing target validation tests**

In `target.rs`:

```rust
#[test]
fn rejects_non_socketcan_targets_before_connect() {
    assert!(validate_targets("gs-usb:ABC", "socketcan:can1").is_err());
    assert!(validate_targets("auto", "socketcan:can1").is_err());
}

#[test]
fn rejects_duplicate_socketcan_interfaces() {
    assert!(validate_targets("socketcan:can0", "socketcan:can0").is_err());
}

#[cfg(not(target_os = "linux"))]
#[test]
fn rejects_socketcan_on_non_linux() {
    assert!(validate_targets("socketcan:can0", "socketcan:can1").is_err());
}
```

- [ ] **Step 2: Write failing canonical effective profile test**

In `profile.rs`:

```rust
#[test]
fn effective_profile_serialization_is_canonical_and_inlines_mirror_map() {
    let profile = EffectiveProfile::default_for_tests();
    let bytes = profile.to_canonical_toml_bytes().expect("serialize");
    let text = std::str::from_utf8(&bytes).unwrap();

    assert!(text.contains("[calibration.mirror_map]\n"));
    assert!(text.contains("mirror_map_kind = \"left-right\"\n"));
    assert!(text.contains("w_u = [\n  [0.0, 0.0, 0.0],\n"));
    assert!(text.ends_with('\n'));
    assert!(!text.ends_with("\n\n"));
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run:

```bash
cargo test --manifest-path addons/piper-svs-collect/Cargo.toml target -- --nocapture
cargo test --manifest-path addons/piper-svs-collect/Cargo.toml profile -- --nocapture
```

Expected: FAIL because modules do not exist.

- [ ] **Step 4: Implement target validation**

`validate_targets` should return parsed `SocketCanTarget { iface: String }` values and reject anything not exactly `socketcan:<iface>` on Linux.

- [ ] **Step 5: Implement profile structs and validation**

Add structs mirroring the spec sections:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EffectiveProfile {
    pub stiffness: StiffnessProfile,
    pub contact: ContactProfile,
    pub frames: FramesProfile,
    pub calibration: CalibrationProfile,
    pub mujoco: MujocoProfile,
    pub cue: CueProfile,
    pub dynamics: DynamicsProfile,
    pub control: ControlProfile,
    pub gripper: GripperProfile,
    pub writer: WriterProfile,
}
```

Validation must reject all ranges listed in the spec, especially `loop_frequency_hz != 200.0`, invalid mirror maps, non-positive gripper feedback age, and final MIT limits outside protocol bounds.

- [ ] **Step 6: Implement canonical TOML writer manually**

Do not rely on `toml::to_string`. Add a small writer with helpers:

```rust
fn push_f64(out: &mut String, value: f64) {
    let mut buf = ryu::Buffer::new();
    out.push_str(buf.format_finite(value));
}
```

Write table order exactly as the spec states.

- [ ] **Step 7: Run tests**

Run:

```bash
cargo test --manifest-path addons/piper-svs-collect/Cargo.toml target -- --nocapture
cargo test --manifest-path addons/piper-svs-collect/Cargo.toml profile -- --nocapture
```

Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add addons/piper-svs-collect/src/lib.rs addons/piper-svs-collect/src/target.rs addons/piper-svs-collect/src/profile.rs
git commit -m "feat: validate svs targets and profiles"
```

## Task 7: Implement Canonical Calibration and Mirror Maps

**Files:**
- Create: `addons/piper-svs-collect/src/calibration.rs`
- Modify: `addons/piper-svs-collect/src/lib.rs`
- Test: `addons/piper-svs-collect/src/calibration.rs`

- [ ] **Step 1: Write failing canonical calibration tests**

```rust
#[test]
fn calibration_bytes_are_canonical_and_hashable() {
    let calibration = CalibrationFile::identity_for_tests();
    let bytes = calibration.to_canonical_toml_bytes().unwrap();
    assert_eq!(sha256_hex(&bytes), calibration.sha256_hex().unwrap());
    assert!(std::str::from_utf8(&bytes).unwrap().contains("[mirror_map]\n"));
}

#[test]
fn loading_rejects_noncanonical_bytes() {
    let text = "schema_version=1\ncreated_unix_ms=0\n";
    assert!(CalibrationFile::from_canonical_bytes(text.as_bytes()).is_err());
}

#[test]
fn save_calibration_refuses_overwrite() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("calibration.toml");
    std::fs::write(&path, b"existing").unwrap();
    let calibration = CalibrationFile::identity_for_tests();
    assert!(persist_calibration_no_overwrite(&path, &calibration.to_canonical_toml_bytes().unwrap()).is_err());
}

#[test]
fn loaded_calibration_mirror_map_must_match_effective_profile() {
    let mut calibration = CalibrationFile::identity_for_tests();
    calibration.mirror_map.torque_sign[0] *= -1.0;
    let effective = JointMirrorMap::left_right_mirror();

    assert!(matches!(
        resolve_episode_calibration(Some(calibration), effective, None),
        Err(CalibrationError::MirrorMapMismatch { .. })
    ));
}

#[test]
fn episode_calibration_bytes_match_manifest_hash() {
    let calibration = CalibrationFile::identity_for_tests();
    let runtime_map = calibration.mirror_map.to_runtime_map().unwrap();
    let resolved = resolve_episode_calibration(Some(calibration), runtime_map, None).unwrap();

    assert_eq!(sha256_hex(&resolved.canonical_bytes), resolved.sha256_hex);
    assert!(std::str::from_utf8(&resolved.canonical_bytes).unwrap().contains("schema_version = 1\n"));
}

#[test]
fn file_backed_mirror_map_rejects_noncanonical_bytes() {
    let text = "schema_version=1\npermutation=[0,1,2,3,4,5]\n";

    assert!(MirrorMapFile::from_canonical_bytes(text.as_bytes()).is_err());
}

#[test]
fn file_backed_mirror_map_hashes_exact_loaded_bytes() {
    let mirror = MirrorMapFile::left_right_for_tests();
    let bytes = mirror.to_canonical_toml_bytes().unwrap();
    let loaded = load_file_backed_mirror_map_bytes(&bytes).unwrap();

    assert_eq!(loaded.sha256_hex, sha256_hex(&bytes));
    assert_eq!(loaded.runtime_map, mirror.to_runtime_map().unwrap());
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --manifest-path addons/piper-svs-collect/Cargo.toml calibration -- --nocapture`

Expected: FAIL because the module does not exist.

- [ ] **Step 3: Implement mirror map schema**

Use the SDK `JointMirrorMap` as the runtime type. Add file-backed TOML parse/serialize with `schema_version = 1`, `permutation`, `position_sign`, `velocity_sign`, and `torque_sign`.

File-backed custom mirror-map loading must be canonical: parse bytes, reserialize with the canonical writer, and reject the file if the bytes differ exactly. Hash the exact loaded canonical bytes with SHA-256 and return that hash for manifest metadata before any hardware connect.

- [ ] **Step 4: Implement calibration schema**

```rust
pub struct CalibrationFile {
    pub schema_version: u32,
    pub created_unix_ms: u64,
    pub master_zero_rad: [f64; 6],
    pub slave_zero_rad: [f64; 6],
    pub mirror_map: MirrorMapFile,
}
```

Add exact canonical field order and byte-identical load checks.

`resolve_episode_calibration` must compare the loaded or captured calibration's mirror map exactly against the resolved effective-profile `JointMirrorMap` (`permutation`, `position_sign`, `velocity_sign`, and `torque_sign`). A mismatch is a startup error before MIT enable. The function returns canonical `calibration.toml` bytes and their SHA-256; every episode must persist exactly those bytes and use exactly that hash in `manifest.toml`.

- [ ] **Step 5: Implement posture compatibility**

Add:

```rust
pub fn validate_current_posture(
    calibration: &CalibrationFile,
    current_master: [f64; 6],
    current_slave: [f64; 6],
    max_error_rad: f64,
) -> Result<(), CalibrationError>
```

Reject non-finite or out-of-tolerance values per joint.

- [ ] **Step 6: Run tests**

Run: `cargo test --manifest-path addons/piper-svs-collect/Cargo.toml calibration -- --nocapture`

Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add addons/piper-svs-collect/src/lib.rs addons/piper-svs-collect/src/calibration.rs
git commit -m "feat: add canonical svs calibration"
```

## Task 8: Implement Episode Wire Format, Manifest, Report, and Writer

**Files:**
- Create: `addons/piper-svs-collect/src/episode/mod.rs`
- Create: `addons/piper-svs-collect/src/episode/wire.rs`
- Create: `addons/piper-svs-collect/src/episode/manifest.rs`
- Create: `addons/piper-svs-collect/src/episode/writer.rs`
- Modify: `addons/piper-svs-collect/src/lib.rs`
- Test: `addons/piper-svs-collect/src/episode/wire.rs`
- Test: `addons/piper-svs-collect/src/episode/writer.rs`

- [ ] **Step 1: Write failing wire round-trip and corruption tests**

```rust
#[test]
fn svs_episode_v1_round_trips_and_counts_records() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("steps.bin");
    let header = SvsHeaderV1::for_test("20260427T000000Z-test-abcdef123456");
    let step = SvsStepV1::for_test(0);

    write_steps_file(&path, &header, &[step.clone()]).unwrap();
    let decoded = read_steps_file(&path).unwrap();

    assert_eq!(decoded.steps.len(), 1);
    assert_eq!(decoded.steps[0].step_index, 0);
}

#[test]
fn svs_episode_rejects_trailing_garbage() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_test_steps(&dir, 1);
    std::fs::OpenOptions::new().append(true).open(&path).unwrap().write_all(&[0xaa]).unwrap();
    assert!(read_steps_file(&path).is_err());
}

#[test]
fn svs_episode_rejects_nonsequential_step_index() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_test_steps_with_indexes(&dir, &[0, 2]);
    assert!(read_steps_file(&path).is_err());
}

#[test]
fn queue_full_event_prevents_complete_status() {
    let dir = tempfile::tempdir().unwrap();
    let mut writer = EpisodeWriter::for_test_with_capacity(dir.path(), 1).unwrap();
    writer.pause_worker_for_test();

    writer.try_enqueue(SvsStepV1::for_test(0)).unwrap();
    assert!(matches!(
        writer.try_enqueue(SvsStepV1::for_test(1)),
        Err(WriterError::QueueFull)
    ));

    let stats = writer.stats();
    assert_eq!(stats.queue_full_events, 1);
    assert_eq!(stats.dropped_step_count, 1);
    assert_eq!(final_status_for_writer_stats(EpisodeStopReason::MaxIterations, &stats), EpisodeStatus::Faulted);
}

#[test]
fn sustained_queue_full_trips_stop_threshold() {
    let mut monitor = WriterBackpressureMonitor::new(2, Duration::from_millis(100));
    assert!(!monitor.record_queue_full(Instant::now()));
    assert!(monitor.record_queue_full(Instant::now() + Duration::from_millis(50)));

    let mut duration_monitor = WriterBackpressureMonitor::new(10, Duration::from_millis(100));
    let start = Instant::now();
    assert!(!duration_monitor.record_queue_full(start));
    assert!(duration_monitor.record_queue_full(start + Duration::from_millis(101)));
}

#[test]
fn episode_id_uses_task_slug_and_random_suffix() {
    let rng = FixedEpisodeRng::from_hex("a1b2c3d4e5f6");
    let id = EpisodeId::generate("Surface Following Demo!", UtcTimestamp::parse("20260428T010203Z").unwrap(), &rng).unwrap();

    assert_eq!(id.task_slug, "surface-following-demo");
    assert_eq!(id.episode_id, "20260428T010203Z-surface-following-demo-a1b2c3d4e5f6");
    assert_eq!(id.relative_dir, PathBuf::from("surface-following-demo/20260428T010203Z-surface-following-demo-a1b2c3d4e5f6"));
}

#[test]
fn invalid_task_slug_is_rejected_before_directory_creation() {
    assert!(EpisodeId::generate("!!!", UtcTimestamp::for_tests(), &FixedEpisodeRng::zero()).is_err());
    assert!(EpisodeId::generate(&"a".repeat(140), UtcTimestamp::for_tests(), &FixedEpisodeRng::zero()).is_err());
}

#[test]
fn manifest_requires_reproducibility_metadata() {
    let manifest = ManifestV1::for_test_complete()
        .without_mujoco_runtime_identity()
        .without_effective_profile_hash();

    assert!(manifest.validate().is_err());

    let manifest = ManifestV1::for_test_complete();
    assert_eq!(manifest.schema_version, 1);
    assert_eq!(manifest.task_profile.source_kind, ProfileSourceKind::File);
    assert!(manifest.effective_profile.sha256_hex.len() == 64);
    assert!(manifest.mujoco.model.sha256_hex.len() == 64);
    assert!(manifest.calibration.sha256_hex.len() == 64);
    assert!(manifest.mirror_map.source_kind.is_some());
    assert!(manifest.collector.revision.is_some() || manifest.collector.version.is_some());
    assert_eq!(manifest.task.raw_name, "Surface Following Demo!");
    assert_eq!(manifest.task.slug, "surface-following-demo");
}

#[test]
fn report_records_run_and_writer_summary() {
    let report = ReportJson::for_test_faulted();

    assert_eq!(report.schema_version, 1);
    assert!(report.started_unix_ns > 0);
    assert!(report.ended_unix_ns >= report.started_unix_ns);
    assert!(report.dual_arm.iterations >= report.step_count);
    assert!(report.writer.max_queue_depth >= report.writer.final_queue_depth);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --manifest-path addons/piper-svs-collect/Cargo.toml episode -- --nocapture`

Expected: FAIL because episode modules do not exist.

- [ ] **Step 3: Implement wire structs exactly**

Copy the normative `SvsHeaderV1`, `SvsArmStepV1`, `SvsCommandStepV1`, `SvsGripperStepV1`, and `SvsStepV1` fields from the spec. Use serde derives and bincode 1.3 fixed-int little-endian options:

```rust
fn bincode_options() -> impl bincode::Options {
    bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_little_endian()
}
```

Validate finite floats, enum byte codes, episode ID zero padding, exact EOF, sequential indexes, and metadata counts.

- [ ] **Step 4: Implement manifest/report models**

Implement `ManifestV1` with all reproducibility fields from the spec, not only counts:

- `schema_version = 1`, `episode_id`, task name, operator, notes.
- raw task name and derived task slug.
- start/end Unix timestamps in nanoseconds and `episode_start_host_mono_us`.
- final status: `running`, `complete`, `cancelled`, or `faulted`.
- master/slave target specs exactly as resolved.
- MuJoCo model source, root XML relative path, hash algorithm, content hash, runtime version/build string, Rust binding version when available, and native library hash or static build identity.
- master/slave end-effector site names.
- per-arm dynamics mode, payload mass/COM, qacc LPF cutoff, and acceleration clamp.
- task profile source kind, source path and source hash when file-backed.
- effective profile path and SHA-256 of exact written bytes.
- gripper mirror config and `--disable-gripper-mirror` resolution.
- calibration path, calibration SHA-256, zero poses, and resolved effective `JointMirrorMap`.
- mirror-map source kind, source path and source hash when file-backed.
- collector binary version and/or git revision.
- raw CAN enabled/finalizer status.
- encoded `step_count` and optional `last_step_index`.

Implement `EpisodeId` and task slug helpers in `episode/manifest.rs`. The output path is `<output-dir>/<task-slug>/<episode-id>`, where `episode-id` is `<utc-yyyymmddThhmmssZ>-<task-slug>-<12hex>`. Slug generation lowercases ASCII, replaces non-alphanumeric runs with `-`, trims leading/trailing `-`, rejects empty slugs, and rejects slugs that would make the UTF-8 episode ID exceed the 128-byte fixed header field. The random suffix uses OS randomness in production and an injectable deterministic RNG in tests. Directory reservation uses atomic create-directory and retries with a new suffix on collision.

Implement `ReportJson` with the dual-arm `BilateralRunReport`, writer statistics, dropped step count, encoded step count/last step index, maximum/final writer queue depth, final status, final flush result, fault classification, and start/end timestamps. Both manifest and report must validate that `step_count` and `last_step_index` match decoded `steps.bin`.

- [ ] **Step 5: Implement non-blocking writer**

Use a bounded `crossbeam_channel::Sender<SvsStepV1>`. `try_send` never blocks the control loop. On `TrySendError::Full`, increment `writer_queue_full_events`, increment `dropped_step_count`, record the first/latest queue-full monotonic timestamps, return `WriterError::QueueFull`, and do not write that step later. V1 stops the active loop immediately on the first dropped step because any dropped step invalidates `complete`/`cancelled` status. `WriterBackpressureMonitor` still enforces and records both `queue_full_stop_events` and `queue_full_stop_duration_ms` from the effective profile so threshold behavior is testable and reported. Any queue-full event, dropped step, backpressure threshold, or writer flush failure must make `final_status_for_writer_stats` return `EpisodeStatus::Faulted`, never `Complete` or `Cancelled`.

The writer thread owns disk IO and writes temp files followed by fsync and atomic rename.

- [ ] **Step 6: Run tests**

Run: `cargo test --manifest-path addons/piper-svs-collect/Cargo.toml episode -- --nocapture`

Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add addons/piper-svs-collect/src/lib.rs addons/piper-svs-collect/src/episode
git commit -m "feat: add svs episode writer"
```

## Task 9: Implement Model Hashing and Runtime Identity

**Files:**
- Create: `addons/piper-svs-collect/src/model_hash.rs`
- Modify: `addons/piper-svs-collect/src/lib.rs`
- Test: `addons/piper-svs-collect/src/model_hash.rs`

- [ ] **Step 1: Write failing model hash tests**

```rust
#[test]
fn model_dir_hash_rejects_symlinks_and_non_utf8_paths() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("piper_no_gripper.xml"), "<mujoco/>").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink("piper_no_gripper.xml", dir.path().join("link.xml")).unwrap();

    assert!(hash_model_dir(dir.path(), "piper_no_gripper.xml").is_err());
}

#[test]
fn embedded_model_hash_includes_domain_separator() {
    let hash = hash_embedded_model(b"<mujoco/>").unwrap();
    let direct = sha256_hex(b"<mujoco/>");
    assert_ne!(hash.sha256_hex, direct);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --manifest-path addons/piper-svs-collect/Cargo.toml model_hash -- --nocapture`

Expected: FAIL because model hash module does not exist.

- [ ] **Step 3: Implement canonical directory hashing**

Walk the model directory. Reject symlinks, non-UTF-8 paths, non-regular files, `.` or `..` components, and duplicate normalized paths. Prefix with `PIPER_MUJOCO_MODEL_HASH_V1\n`, then encode `F`, little-endian `u64` path length, path bytes, little-endian `u64` content length, and content bytes.

- [ ] **Step 4: Validate root XML and asset containment**

Record selected root XML relative path. Parse XML enough to reject absolute `file=` references or `..` path components. If robust MuJoCo asset resolution is not feasible in this task, conservatively reject any include/plugin/asset file path that cannot be proven to stay inside the hashed tree.

- [ ] **Step 5: Implement runtime identity wrapper**

Call `piper_physics::mujoco_runtime_version_string()` and `loaded_mujoco_library_identity()`. If native library identity is unavailable, return an error before MIT enable.

- [ ] **Step 6: Run tests**

Run: `cargo test --manifest-path addons/piper-svs-collect/Cargo.toml model_hash -- --nocapture`

Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add addons/piper-svs-collect/src/lib.rs addons/piper-svs-collect/src/model_hash.rs
git commit -m "feat: hash svs mujoco model inputs"
```

## Task 10: Implement Cue Mapping and Stiffness State

**Files:**
- Create: `addons/piper-svs-collect/src/cue.rs`
- Create: `addons/piper-svs-collect/src/stiffness.rs`
- Modify: `addons/piper-svs-collect/src/lib.rs`
- Test: `addons/piper-svs-collect/src/cue.rs`
- Test: `addons/piper-svs-collect/src/stiffness.rs`

- [ ] **Step 1: Write failing cue tests**

```rust
#[test]
fn dls_uses_lambda_squared() {
    let j = [
        [1.0, 0.0, 0.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0, 0.0, 0.0],
    ];
    let tau = [1.0, 0.0, 0.0, 0.0, 0.0, 0.0];
    let cue = dls_task_space_proxy(&j, &tau, 2.0).unwrap();
    assert!((cue[0] - 0.2).abs() < 1e-12);
}

#[test]
fn master_feedback_subtraction_uses_latest_not_newer_than_snapshot() {
    let history = FeedbackHistory::from_entries([
        AppliedMasterFeedback { master_tx_finished_host_mono_us: 100, shaped_master_interaction_nm: [1.0; 6] },
        AppliedMasterFeedback { master_tx_finished_host_mono_us: 200, shaped_master_interaction_nm: [2.0; 6] },
    ]);
    assert_eq!(history.select_for_dynamic_rx(150), [1.0; 6]);
}
```

- [ ] **Step 2: Write failing stiffness tests**

```rust
#[test]
fn stiffness_clips_before_lpf_and_rate_limit() {
    let profile = StiffnessProfile::test_with_limits([50.0; 3], [100.0; 3]);
    let mut state = SvsStiffnessState::new(&profile).unwrap();
    let output = state.update([1e9, 0.0, 0.0], [0.0; 3], 5_000).unwrap();
    assert!(output.k_state_clipped_n_per_m[0] <= 100.0);
    assert!(output.k_tele_n_per_m[0] <= 100.0);
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run:

```bash
cargo test --manifest-path addons/piper-svs-collect/Cargo.toml cue -- --nocapture
cargo test --manifest-path addons/piper-svs-collect/Cargo.toml stiffness -- --nocapture
```

Expected: FAIL because modules do not exist.

- [ ] **Step 4: Implement DLS and cue transforms**

Use nalgebra matrices. Reject non-finite values and condition number above profile limit. Express master cue into slave EE frame using `R_slave_base_from_master_base` and `R_slave_ee_from_base`.

- [ ] **Step 5: Implement deterministic LPF/rate/contact state**

Implement exactly:

```rust
let rc = 1.0 / (2.0 * std::f64::consts::PI * cutoff_hz);
let alpha = dt_sec / (rc + dt_sec);
let y = y + alpha * (x - y);
```

Use `dt_sec = dt_us as f64 / 1_000_000.0`, clip before LPF, rate-limit per axis, final clip, and tick-count contact hysteresis.

- [ ] **Step 6: Run tests**

Run:

```bash
cargo test --manifest-path addons/piper-svs-collect/Cargo.toml cue -- --nocapture
cargo test --manifest-path addons/piper-svs-collect/Cargo.toml stiffness -- --nocapture
```

Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add addons/piper-svs-collect/src/lib.rs addons/piper-svs-collect/src/cue.rs addons/piper-svs-collect/src/stiffness.rs
git commit -m "feat: compute svs cues and stiffness"
```

## Task 11: Implement Phase-1 SVS Controller and Tick Staging

**Files:**
- Create: `addons/piper-svs-collect/src/tick_frame.rs`
- Create: `addons/piper-svs-collect/src/controller.rs`
- Modify: `addons/piper-svs-collect/src/lib.rs`
- Test: `addons/piper-svs-collect/src/tick_frame.rs`
- Test: `addons/piper-svs-collect/src/controller.rs`

- [ ] **Step 1: Write failing controller mapping tests**

```rust
#[test]
fn k_tele_schedules_slave_tracking_gains() {
    let stiffness = StiffnessProfile::for_tests();
    let control = ControlProfile::for_tests();
    let command = build_svs_command(&control, &stiffness, [stiffness.k_max[0]; 3], zero_frame()).unwrap();
    assert_eq!(command.controller_slave_kp[0], control.track_kp_max[0]);
}

#[test]
fn master_reflection_uses_task_space_jacobian_transpose() {
    let frame = control_frame_with_identity_jacobian_and_residual([1.0, 0.0, 0.0]);
    let command = build_svs_command(&ControlProfile::for_tests(), [100.0; 3], frame).unwrap();
    assert!(command.controller_master_interaction_nm[0] < 0.0);
}

#[test]
fn tick_stager_pairs_controller_output_with_matching_telemetry_snapshot() {
    let stager = SvsTickStager::default();
    let snapshot = fake_dual_arm_snapshot_with_dynamic_rx_us(10_000, 10_100);
    let key = SnapshotKey::from_snapshot(&snapshot);
    let pending = SvsPendingTick::for_test(key);

    stager.store_controller_tick(pending.clone()).unwrap();

    let telemetry = fake_bilateral_telemetry_for_snapshot(snapshot);
    assert_eq!(stager.take_for_telemetry(&telemetry).unwrap(), pending);
}

#[test]
fn tick_stager_rejects_mismatched_or_stale_telemetry() {
    let stager = SvsTickStager::default();
    stager.store_controller_tick(SvsPendingTick::for_test(SnapshotKey::for_test(1, 2))).unwrap();

    let telemetry = fake_bilateral_telemetry_for_key(SnapshotKey::for_test(3, 4));

    assert!(matches!(
        stager.take_for_telemetry(&telemetry),
        Err(SvsTickFrameError::MismatchedSnapshotKey { .. })
    ));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run:

```bash
cargo test --manifest-path addons/piper-svs-collect/Cargo.toml controller -- --nocapture
cargo test --manifest-path addons/piper-svs-collect/Cargo.toml tick_frame -- --nocapture
```

Expected: FAIL because controller module does not exist.

- [ ] **Step 3: Implement tick-frame staging types**

Create `addons/piper-svs-collect/src/tick_frame.rs`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SnapshotKey {
    pub master_dynamic_host_rx_mono_us: u64,
    pub slave_dynamic_host_rx_mono_us: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SvsDynamicsFrame {
    pub key: SnapshotKey,
    pub master_model_torque_nm: [f64; 6],
    pub slave_model_torque_nm: [f64; 6],
    pub master_residual_nm: [f64; 6],
    pub slave_residual_nm: [f64; 6],
    pub master_ee: EndEffectorKinematics,
    pub slave_ee: EndEffectorKinematics,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SvsPendingTick {
    pub key: SnapshotKey,
    pub dynamics: SvsDynamicsFrame,
    pub cues: SvsCueOutput,
    pub stiffness: SvsStiffnessOutput,
    pub controller_output: SvsControllerOutput,
}

#[derive(Default)]
pub struct SvsDynamicsSlot {
    slot: Mutex<Option<SvsDynamicsFrame>>,
}

#[derive(Default)]
pub struct SvsTickStager {
    slot: Mutex<Option<SvsPendingTick>>,
}
```

`SnapshotKey::from_snapshot` must use the master/left and slave/right dynamic `host_rx_mono_us` values from the same `DualArmSnapshot` that the SDK loop passes to both compensator/controller and telemetry. `SvsDynamicsSlot::store_dynamics` stores the bridge output for exactly one key. `SvsDynamicsSlot::take_dynamics(key)` returns and clears only a matching frame. `SvsTickStager::store_controller_tick` fails if an unconsumed controller slot already exists. `SvsTickStager::take_for_telemetry` computes the key from `BilateralLoopTelemetry.control_frame.snapshot` and returns an error on missing or mismatched keys. This prevents combining MuJoCo data from one accepted tick with shaped telemetry from another.

Add `pub mod tick_frame;` to `addons/piper-svs-collect/src/lib.rs`.

- [ ] **Step 4: Implement controller input/output structs**

Create `SvsControlInput` containing snapshot data, kinematics, model torques, residuals, cues, stiffness output, and calibration mapping. Create `SvsControllerOutput` matching the command telemetry fields before SDK output shaping.

- [ ] **Step 5: Implement deterministic mappings and staging**

Implement:

```text
alpha_xyz = clamp((K_tele - K_min) / (K_max - K_min), 0, 1)
alpha_joint = clamp(joint_stiffness_projection * alpha_xyz, 0, 1)
slave_kp = lerp(track_kp_min, track_kp_max, alpha_joint)
master_interaction_torque = -Jp_master^T * f_reflect_master_base
```

Do not add model torque inside the SVS controller; model torque compensation stays in the generic dual-arm compensation path.

The real `SvsController` must own:

- `Arc<SvsTickStager>` shared with `SvsTelemetrySink`.
- `Arc<SvsDynamicsSlot>` shared with `SvsMujocoBridge`.
- `Arc<Mutex<AppliedMasterFeedbackHistory>>` updated by telemetry after successful master TX-finished submission.

For each `tick_with_compensation`, the controller retrieves the matching `SvsDynamicsFrame`, computes cues and `K_tele`, computes `SvsControllerOutput`, stores `SvsPendingTick` in the stager, and returns a normal `BilateralCommand`. If dynamics are missing or the key mismatches, return a controller error so the dual-arm loop exits through `ControllerFault`; do not synthesize partial rows.

- [ ] **Step 6: Run tests**

Run:

```bash
cargo test --manifest-path addons/piper-svs-collect/Cargo.toml controller -- --nocapture
cargo test --manifest-path addons/piper-svs-collect/Cargo.toml tick_frame -- --nocapture
```

Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add addons/piper-svs-collect/src/lib.rs addons/piper-svs-collect/src/tick_frame.rs addons/piper-svs-collect/src/controller.rs
git commit -m "feat: add svs phase-one controller"
```

## Task 12: Integrate Collector Startup and Fake Workflow

**Files:**
- Create: `addons/piper-svs-collect/src/cancel.rs`
- Create: `addons/piper-svs-collect/src/collector.rs`
- Create: `addons/piper-svs-collect/src/mujoco_bridge.rs`
- Create: `addons/piper-svs-collect/src/raw_can.rs`
- Modify: `addons/piper-svs-collect/src/main.rs`
- Modify: `addons/piper-svs-collect/src/lib.rs`
- Test: `addons/piper-svs-collect/tests/collector_fake.rs`

- [ ] **Step 1: Write failing fake full-workflow test**

```rust
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
    assert_eq!(read_steps_file(&result.path.join("steps.bin")).unwrap().steps.len(), 3);
}

#[test]
fn fake_workflow_pairs_dynamics_controller_and_shaped_telemetry_per_tick() {
    let out = tempfile::tempdir().unwrap();
    let harness = FakeCollectorHarness::new()
        .with_two_socketcan_targets()
        .with_fake_mujoco_sequence([
            FakeMujocoFrame::new(10_000, 10_100).with_slave_residual_nm([1.0, 0.0, 0.0, 0.0, 0.0, 0.0]),
            FakeMujocoFrame::new(20_000, 20_100).with_slave_residual_nm([2.0, 0.0, 0.0, 0.0, 0.0, 0.0]),
        ])
        .with_iterations(2);

    let result = harness.run(out.path()).expect("collector should complete");
    let steps = read_steps_file(&result.path.join("steps.bin")).unwrap();

    assert_eq!(steps.steps[0].master.dynamic_host_rx_mono_us, 10_000);
    assert_eq!(steps.steps[1].master.dynamic_host_rx_mono_us, 20_000);
    assert!(steps.steps[1].r_ee[0] > steps.steps[0].r_ee[0]);
    assert!(steps.steps[0].command.master_tx_finished_host_mono_us > 0);
    assert_ne!(steps.steps[0].command.mit_master_t_ref_nm, [0.0; 6]);
}
```

- [ ] **Step 2: Write failing no-success-row-on-TX-fault test**

```rust
#[test]
fn tx_finished_failure_faults_without_successful_row() {
    let out = tempfile::tempdir().unwrap();
    let harness = FakeCollectorHarness::new()
        .with_two_socketcan_targets()
        .with_fake_mujoco()
        .with_master_tx_finished_timeout_at_step(1);

    let result = harness.run(out.path()).expect("collector should finalize faulted episode");

    assert_eq!(result.status, EpisodeStatus::Faulted);
    let steps = read_steps_file(&result.path.join("steps.bin")).unwrap();
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
    assert_eq!(result.dual_arm_exit_reason, Some(BilateralExitReason::TelemetrySinkFault));
    assert!(result.loop_stopped_before_requested_iterations);
    let report = read_report_json(&result.path.join("report.json")).unwrap();
    assert!(report.writer_queue_full_events > 0);
    assert!(report.dropped_step_count > 0);
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
    assert_eq!(manifest.final_status, EpisodeStatus::Faulted);
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
    let steps = read_steps_file(&result.path.join("steps.bin")).unwrap();

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
    let steps = read_steps_file(&result.path.join("steps.bin")).unwrap();

    assert_eq!(result.status, EpisodeStatus::Complete);
    assert_eq!(steps.steps[0].raw_can_status, 1);
    assert_eq!(steps.steps[1].raw_can_status, 2);
    let report = read_report_json(&result.path.join("report.json")).unwrap();
    assert!(report.raw_can_degraded);
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
    assert_eq!(report.dropped_step_count, 0);
    assert_eq!(report.writer_queue_full_events, 0);
    assert!(report.writer_flush_succeeded);
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
    assert_eq!(manifest.final_status, EpisodeStatus::Cancelled);
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
    assert_eq!(manifest.final_status, EpisodeStatus::Cancelled);
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test --manifest-path addons/piper-svs-collect/Cargo.toml --test collector_fake -- --nocapture`

Expected: FAIL because collector orchestration does not exist.

- [ ] **Step 4: Implement startup order**

Implement the exact startup sequence from the spec:

1. parse config;
2. validate static inputs;
3. resolve and hash effective profile;
4. generate a random episode ID suffix with `getrandom` and retry on path collision without overwriting existing episode directories;
5. validate effective profile;
6. load MuJoCo calculators and resolve sites;
7. connect arms;
8. resolve calibration, verify its mirror map exactly matches the effective profile, and obtain canonical `calibration.toml` bytes plus SHA-256;
9. create episode dir and write `effective_profile.toml`, the canonical `calibration.toml` bytes from step 8, and running manifest using the same calibration hash;
10. create a shared cancellation token and install the Ctrl+C handler;
11. start writer;
12. start raw side recording if requested;
13. confirm unless `--yes`, checking the cancellation token before and after prompt handling;
14. if the operator declines confirmation, flush the writer and finalize `EpisodeStatus::Cancelled` without enabling MIT;
15. if cancellation is already requested before MIT enable, flush the writer and finalize `EpisodeStatus::Cancelled` without enabling MIT;
16. enable MIT and enter loop.

- [ ] **Step 5: Implement fakeable backend traits**

Keep hardware-free tests possible:

```rust
pub trait CollectorBackend {
    type Active;
    fn connect(&self, targets: &ResolvedTargets) -> Result<DualArmStandbyLike>;
    fn enable_mit(&self, standby: DualArmStandbyLike) -> Result<Self::Active>;
    fn run_loop(&self, active: Self::Active, runtime: CollectorRuntime) -> Result<LoopOutcome>;
}
```

Use real `DualArmActiveMit` only in the real backend.

- [ ] **Step 6: Implement MuJoCo bridge and per-tick data flow**

Create `addons/piper-svs-collect/src/mujoco_bridge.rs`:

```rust
pub struct SvsMujocoBridge {
    master: MujocoGravityCompensation,
    slave: MujocoGravityCompensation,
    master_selector: EndEffectorSelector,
    slave_selector: EndEffectorSelector,
    dynamics_slot: Arc<SvsDynamicsSlot>,
}
```

Implement `BilateralDynamicsCompensator` for `SvsMujocoBridge`:

1. Build `SnapshotKey::from_snapshot(snapshot)` immediately from the SDK snapshot.
2. Compute master/slave model torques with the MuJoCo addon using the exact joint positions/velocities from that snapshot and the resolved effective-profile `[dynamics]` configuration.
3. Compute master/slave measured-minus-model residuals from the same snapshot.
4. Compute master/slave end-effector pose, rotation, translational Jacobian, and condition metrics using the configured site selectors.
5. Store one `SvsDynamicsFrame` in `SvsDynamicsSlot` for that key.
6. Return `BilateralDynamicsCompensation` with model torques and external torque estimates matching the stored frame.

The real collector constructs one shared `Arc<SvsDynamicsSlot>` and `Arc<SvsTickStager>`:

```text
SvsMujocoBridge.compute(snapshot, dt)
  -> stores SvsDynamicsFrame keyed by SnapshotKey
  -> returns BilateralDynamicsCompensation to piper-client
SvsController.tick_with_compensation(frame, dt)
  -> takes matching SvsDynamicsFrame
  -> computes cues, K_tele, and SvsControllerOutput
  -> stores SvsPendingTick keyed by SnapshotKey
SvsTelemetrySink.on_tick(telemetry)
  -> takes matching SvsPendingTick using telemetry.control_frame.snapshot
  -> combines SDK shaped/final telemetry, TX-finished times, gripper state, raw CAN status, and writer stats
  -> writes exactly one SvsStepV1 or returns BilateralTelemetrySinkError
```

If any key is missing or mismatched at any stage, fail the current run instead of guessing. The sink updates `AppliedMasterFeedbackHistory` only after the master TX-finished timestamp is available for a successfully submitted tick, so the next tick subtracts only feedback commands not newer than its dynamic RX timestamp.

Map all effective-profile dynamics fields into `SvsMujocoBridge` before MIT enable:

```rust
let bridge = SvsMujocoBridge::new(SvsMujocoBridgeConfig {
    master_mode: profile.dynamics.master_mode,
    slave_mode: profile.dynamics.slave_mode,
    master_payload: profile.dynamics.master_payload,
    slave_payload: profile.dynamics.slave_payload,
    qacc_lpf_cutoff_hz: profile.dynamics.qacc_lpf_cutoff_hz,
    max_abs_qacc: profile.dynamics.max_abs_qacc,
    master_selector: EndEffectorSelector { site_name: profile.mujoco.master_ee_site.clone() },
    slave_selector: EndEffectorSelector { site_name: profile.mujoco.slave_ee_site.clone() },
    dynamics_slot: dynamics_slot.clone(),
})?;
```

`gravity` mode uses zero-velocity gravity compensation. `partial` mode includes velocity-dependent bias terms. `full` mode computes finite-difference acceleration from consecutive accepted snapshots, applies the configured qacc LPF cutoff and `max_abs_qacc` clamp, and rejects startup before MIT enable if those full-mode parameters are missing or invalid. The manifest records these exact modes, payloads, qacc filter, and clamp values.

Add `pub mod mujoco_bridge;` to `addons/piper-svs-collect/src/lib.rs`.

- [ ] **Step 7: Add cancellation token and Ctrl+C handler**

Create `addons/piper-svs-collect/src/cancel.rs`:

```rust
#[derive(Clone, Default)]
pub struct CollectorCancelToken {
    flag: Arc<AtomicBool>,
}

impl CollectorCancelToken {
    pub fn signal(&self) {
        self.flag.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::Acquire)
    }

    pub fn loop_signal(&self) -> Arc<AtomicBool> {
        self.flag.clone()
    }
}

pub fn install_ctrlc_handler(token: CollectorCancelToken) -> anyhow::Result<()> {
    ctrlc::set_handler(move || token.signal())?;
    Ok(())
}
```

Add `pub mod cancel;` to `addons/piper-svs-collect/src/lib.rs`.

In production startup, install the handler once before operator confirmation. In tests, do not install a process-global Ctrl+C handler; inject `CollectorCancelToken` directly into `FakeCollectorHarness`.

- [ ] **Step 8: Integrate real loop**

The real loop must configure:

```rust
cfg.frequency_hz = 200.0;
cfg.dt_clamp_multiplier = profile.control.dt_clamp_multiplier;
cfg.timing_mode = args.timing_mode.parse()?;
cfg.warmup_cycles = profile.control.warmup_cycles;
cfg.max_iterations = args
    .max_iterations
    .map(|dataset_steps| dataset_steps as usize + profile.control.warmup_cycles);
cfg.submission_mode = BilateralSubmissionMode::ConfirmedTxFinished;
cfg.cancel_signal = Some(cancel_token.loop_signal());
cfg.telemetry_sink = Some(Arc::new(SvsTelemetrySink::new(writer_tx, stager.clone(), feedback_history.clone())));
cfg.master_interaction_slew_limit_nm_per_s = profile.control.master_interaction_slew_limit_nm_per_s;
cfg.master_interaction_lpf_cutoff_hz = profile.control.master_interaction_lpf_cutoff_hz;
cfg.master_interaction_limit = profile.control.master_interaction_limit_nm;
cfg.master_passivity_enabled = profile.control.master_passivity_enabled;
cfg.master_passivity_max_damping = profile.control.master_passivity_max_damping;
cfg.slave_feedforward_limit = profile.control.slave_feedforward_limit_nm;
cfg.gripper.enabled = profile.gripper.mirror_enabled && !args.disable_gripper_mirror;
cfg.gripper.update_divider = profile.gripper.update_divider;
cfg.gripper.position_deadband = profile.gripper.position_deadband;
cfg.gripper.effort_scale = profile.gripper.effort_scale;
cfg.gripper.max_feedback_age = Duration::from_millis(profile.gripper.max_feedback_age_ms);
```

If `--max-iterations N` is supplied, the collector must request `N + warmup_cycles` SDK loop iterations and write exactly `N` dataset steps, because warmup cycles are not dataset steps. The `SvsTelemetrySink` must ignore warmup telemetry for step writing but still allow the SDK loop to run warmup holds. The final report records both SDK iterations and dataset step count.

Do not leave unspecified `BilateralLoopConfig` fields at defaults when the effective profile has an equivalent field. The collector must map every effective-profile runtime field listed under `[frames]`, `[control]`, and `[gripper]` into the SDK loop config or into `SvsController`/`SvsTelemetrySink` constructor arguments before MIT enable, and the manifest/report must reflect the same resolved values.

Run the real loop with `run_bilateral_with_compensation(SvsController, SvsMujocoBridge, cfg)`. The sink writes a step only after both TX-finished timestamps are present and after it successfully takes the matching `SvsPendingTick` from the stager. If `SvsTelemetrySink` receives `WriterError::QueueFull`, if the writer backpressure monitor trips either configured threshold, or if tick staging mismatches, return `BilateralTelemetrySinkError` so the dual-arm loop exits through `BilateralExitReason::TelemetrySinkFault` and the collector writes the final manifest/report with `EpisodeStatus::Faulted`. If final writer flush fails after the loop exits, map that finalization failure to `EpisodeStatus::Faulted` as well. None of these paths may report `Complete` or `Cancelled`.

Map `BilateralLoopGripperTelemetry` into `SvsGripperStepV1` directly: `command_status` uses enum codes `0 = none`, `1 = sent`, `2 = skipped`, `3 = failed`; `command_position` and `command_effort` are the attempted normalized command values for `Sent` or `Failed` and zero for `None` or `Skipped`.

Map cancellation explicitly:

- Operator declines confirmation before MIT enable: do not enable MIT; flush the writer; finalize `EpisodeStatus::Cancelled` only if no writer queue-full/dropped-step event occurred and flush succeeded, otherwise `Faulted`.
- Cancellation before MIT enable: do not enable MIT; flush the writer; finalize `EpisodeStatus::Cancelled` only if no writer queue-full/dropped-step event occurred and flush succeeded, otherwise `Faulted`.
- Cancellation during active control: rely on `BilateralLoopConfig::cancel_signal` so the dual-arm loop exits through its normal cancellation path and disables safely; flush the writer; finalize `EpisodeStatus::Cancelled` under the same writer-clean conditions.
- Process termination or handler installation failure is not a clean cancellation; handler installation failure before MIT enable is a startup error.

- [ ] **Step 9: Run fake integration tests**

Run: `cargo test --manifest-path addons/piper-svs-collect/Cargo.toml --test collector_fake -- --nocapture`

Expected: PASS.

- [ ] **Step 10: Commit**

```bash
git add addons/piper-svs-collect/src/main.rs addons/piper-svs-collect/src/lib.rs addons/piper-svs-collect/src/cancel.rs addons/piper-svs-collect/src/collector.rs addons/piper-svs-collect/src/mujoco_bridge.rs addons/piper-svs-collect/src/raw_can.rs addons/piper-svs-collect/tests/collector_fake.rs
git commit -m "feat: orchestrate svs collector workflow"
```

## Task 13: Add End-to-End Validation, Dependency Boundary Checks, and Operator Docs

**Files:**
- Create: `addons/piper-svs-collect/README.md`
- Create: `addons/piper-svs-collect/profiles/wiping.toml`
- Create: `addons/piper-svs-collect/profiles/peg_insertion.toml`
- Create: `addons/piper-svs-collect/profiles/surface_following.toml`
- Modify: `docs/superpowers/specs/2026-04-27-svspolicy-data-collection-teleop-design.md` only if implementation discovers an unavoidable spec correction.
- Test: `addons/piper-svs-collect/tests/dependency_boundaries.rs`

- [ ] **Step 1: Write dependency boundary test script**

Create `addons/piper-svs-collect/tests/dependency_boundaries.rs`:

```rust
#[test]
fn default_workspace_tree_is_mujoco_free() {
    let repo_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|addons| addons.parent())
        .expect("collector crate should live under <repo>/addons/piper-svs-collect");
    let output = std::process::Command::new("cargo")
        .current_dir(repo_root)
        .args(["tree", "--workspace", "--all-features"])
        .output()
        .expect("cargo tree should run");
    assert!(output.status.success());
    let tree = String::from_utf8_lossy(&output.stdout).to_lowercase();
    assert!(!tree.contains("mujoco"));
    assert!(!tree.contains("piper-physics"));
    assert!(!tree.contains("piper-svs-collect"));
}
```

- [ ] **Step 2: Run dependency test to verify current boundary**

Run: `cargo test --manifest-path addons/piper-svs-collect/Cargo.toml --test dependency_boundaries -- --nocapture`

Expected: PASS. If it fails, fix workspace dependencies before proceeding.

- [ ] **Step 3: Add collector README**

Document:

- StrictRealtime SocketCAN-only runtime.
- MuJoCo native dependency.
- Startup order and calibration behavior.
- Episode directory layout.
- HIL acceptance command examples.
- Verification commands from the spec.

- [ ] **Step 4: Add starter profiles**

Add the three profile files using the exact canonical table order:

- `addons/piper-svs-collect/profiles/wiping.toml`
- `addons/piper-svs-collect/profiles/peg_insertion.toml`
- `addons/piper-svs-collect/profiles/surface_following.toml`

Use conservative placeholder gains and explicit site names that must be changed by the operator if the selected model differs.

- [ ] **Step 5: Run addon checks**

Run:

```bash
cargo fmt --manifest-path addons/piper-physics-mujoco/Cargo.toml -- --check
cargo test --manifest-path addons/piper-physics-mujoco/Cargo.toml --all-targets
cargo clippy --manifest-path addons/piper-physics-mujoco/Cargo.toml --all-targets --all-features -- -D warnings
cargo fmt --manifest-path addons/piper-svs-collect/Cargo.toml -- --check
cargo test --manifest-path addons/piper-svs-collect/Cargo.toml --all-targets
cargo clippy --manifest-path addons/piper-svs-collect/Cargo.toml --all-targets --all-features -- -D warnings
```

Expected: all commands exit 0.

- [ ] **Step 6: Commit**

```bash
git add addons/piper-svs-collect/README.md addons/piper-svs-collect/profiles addons/piper-svs-collect/tests/dependency_boundaries.rs
git commit -m "docs: add svs collector operations guide"
```

## Task 14: Final Verification and Integration Commit

**Files:**
- No new files expected.
- If verification fails, return to the task that owns the failing behavior and modify that task's exact files. Do not apply unrelated cleanup in this final task.

- [ ] **Step 1: Run default workspace verification**

Run:

```bash
cargo fmt --all -- --check
cargo test --workspace --all-targets --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo tree --workspace --all-features > /tmp/piper-default-workspace.tree
! rg -i 'mujoco|piper-physics|piper-svs-collect' /tmp/piper-default-workspace.tree
cargo tree -p piper-cli --all-features > /tmp/piper-cli.tree
! rg -i 'mujoco|piper-physics|piper-svs-collect' /tmp/piper-cli.tree
cargo tree -p piper-control --all-features > /tmp/piper-control.tree
! rg -i 'mujoco|piper-physics|piper-svs-collect' /tmp/piper-control.tree
```

Expected: all commands exit 0 and dependency scans produce no matches.

- [ ] **Step 2: Run addon verification**

Run:

```bash
cargo fmt --manifest-path addons/piper-physics-mujoco/Cargo.toml -- --check
cargo test --manifest-path addons/piper-physics-mujoco/Cargo.toml --all-targets
cargo clippy --manifest-path addons/piper-physics-mujoco/Cargo.toml --all-targets --all-features -- -D warnings
cargo fmt --manifest-path addons/piper-svs-collect/Cargo.toml -- --check
cargo test --manifest-path addons/piper-svs-collect/Cargo.toml --all-targets
cargo clippy --manifest-path addons/piper-svs-collect/Cargo.toml --all-targets --all-features -- -D warnings
```

Expected: all commands exit 0.

- [ ] **Step 3: Run CLI behavior regression**

Run: `cargo run -p piper-cli -- teleop dual-arm --help`

Expected: command prints the general-purpose dual-arm teleop help and does not mention SVSPolicy, MuJoCo, or `piper-svs-collect`.

- [ ] **Step 4: Run collector help**

Run:

```bash
cargo run --manifest-path addons/piper-svs-collect/Cargo.toml -- --help
```

Expected: help lists `--master-target`, `--slave-target`, `--model-dir`, `--use-standard-model-path`, `--use-embedded-model`, `--calibration-file`, `--save-calibration`, `--mirror-map`, `--raw-can`, and `--yes`.

- [ ] **Step 5: Handle verification failures**

If any command above fails, do not create a generic final-fixes commit. Return to the owning task, make the smallest fix in that task's listed files, rerun that task's pass command, use that task's commit command, then restart Task 14 from Step 1. If every command passes, no commit is required in this step.

- [ ] **Step 6: Request final code review**

Use @superpowers:requesting-code-review with:

- Spec: `docs/superpowers/specs/2026-04-27-svspolicy-data-collection-teleop-design.md`
- Plan: `docs/superpowers/plans/2026-04-27-svspolicy-data-collection-teleop-implementation.md`
- Base SHA: commit before Task 1
- Head SHA: current implementation head

Expected: reviewer reports no Critical or Important findings before merge.
