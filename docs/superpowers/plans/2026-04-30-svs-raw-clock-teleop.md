# SVS Raw-Clock Teleoperation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add an explicit experimental calibrated raw-clock backend to `piper-svs-collect` so SVS can collect gravity-compensated bilateral episodes without `StrictRealtime`.

**Architecture:** Keep MuJoCo and SVS episode writing inside `addons/piper-svs-collect`; extend `piper-client::dual_arm_raw_clock` only with generic bilateral hooks for compensation, output shaping, tx-finished telemetry, and telemetry-sink fault accounting. The SVS addon selects the raw-clock backend only through `--experimental-calibrated-raw`, reuses `SvsController`, `SvsMujocoBridge`, and `SvsTelemetrySink`, and records raw-clock diagnostics in optional manifest/report sections without changing `SvsEpisodeV1`.

**Tech Stack:** Rust 2024, `piper-client` SoftRealtime raw-clock runtime, `piper-svs-collect` addon, SocketCAN, `clap`, `serde`/`toml`/`serde_json`, MuJoCo addon bridge, cargo unit/integration tests.

---

## Source Spec

- `docs/superpowers/specs/2026-04-30-svs-raw-clock-teleop-design.md`
- Prerequisite plan already merged: `docs/superpowers/plans/2026-04-30-raw-clock-bilateral-teleop.md`

## File Structure

- `crates/piper-client/src/dual_arm.rs`
  - Owns shared bilateral control structures, compensation traits, output shaping, gripper telemetry helpers, and StrictRealtime bilateral loop.
  - Add a small public output-shaping config type and make selected helper functions `pub(crate)` so raw-clock can reuse the same shaping/telemetry semantics.
- `crates/piper-client/src/dual_arm_raw_clock.rs`
  - Owns calibrated raw-clock estimator, aligned snapshot selection, runtime health gates, SoftRealtime command submission, raw-clock reports, and raw-clock active runners.
  - Add compensation-capable raw-clock execution, confirmed-finished telemetry submission, telemetry sink emission, explicit compensation/controller/telemetry fault counters, and public `run_with_controller_and_compensation`.
- `apps/cli/src/teleop/workflow.rs`
  - Owns general teleop report conversion from raw-clock reports.
  - Update enum matching/report defaults for the new raw-clock exit reasons and counters; do not add SVS behavior here.
- `addons/piper-svs-collect/src/raw_clock.rs`
  - New addon-owned module for SVS raw-clock settings, defaults, validation, CLI/profile precedence, conversion into `ExperimentalRawClockConfig`, conversion into `ExperimentalRawClockRunConfig`, and conversion from `RawClockRuntimeReport` into SVS manifest/report JSON.
- `addons/piper-svs-collect/src/args.rs`
  - Add explicit raw-clock opt-in and raw-clock threshold CLI flags.
- `addons/piper-svs-collect/src/profile.rs`
  - Add `[raw_clock]` effective-profile settings and canonical TOML serialization/validation.
- `addons/piper-svs-collect/src/collector.rs`
  - Split current real collector flow into `run_strict_realtime_collector` and `run_raw_clock_collector`; add raw-clock startup UX, SoftRealtime connection/warmup/enable, active-zero calibration capture/check, raw-clock loop invocation, and raw-clock report finalization.
- `addons/piper-svs-collect/src/episode/manifest.rs`
  - Add optional `ManifestV1.raw_clock` and `ReportJson.raw_clock` sections with compatibility-preserving serde defaults.
- `addons/piper-svs-collect/src/lib.rs`
  - Export the new `raw_clock` module.
- `addons/piper-svs-collect/tests/collector_fake.rs`
  - Add fake collector coverage for raw-clock runtime selection, optional manifest/report sections, and telemetry sink failure behavior.
- `addons/piper-svs-collect/tests/dependency_boundaries.rs`
  - Add coverage that the new raw-clock profile table is accepted and does not change addon isolation.
- No changes to `SvsEpisodeV1` in `addons/piper-svs-collect/src/episode/wire.rs`.
- No MuJoCo dependency changes in the default workspace.

## Shared Constants

Use the same stable lab defaults that the current teleop smoke script settled on:

```rust
pub const DEFAULT_RAW_CLOCK_WARMUP_SECS: u64 = 10;
pub const DEFAULT_RAW_CLOCK_RESIDUAL_P95_US: u64 = 2000;
pub const DEFAULT_RAW_CLOCK_RESIDUAL_MAX_US: u64 = 3000;
pub const DEFAULT_RAW_CLOCK_DRIFT_ABS_PPM: f64 = 500.0;
pub const DEFAULT_RAW_CLOCK_SAMPLE_GAP_MAX_MS: u64 = 50;
pub const DEFAULT_RAW_CLOCK_LAST_SAMPLE_AGE_MS: u64 = 20;
pub const DEFAULT_RAW_CLOCK_SELECTED_SAMPLE_AGE_MS: u64 = 50;
pub const DEFAULT_RAW_CLOCK_STATE_SKEW_MAX_US: u64 = 10_000;
pub const DEFAULT_RAW_CLOCK_INTER_ARM_SKEW_MAX_US: u64 = 20_000;
pub const DEFAULT_RAW_CLOCK_RESIDUAL_MAX_CONSECUTIVE_FAILURES: u32 = 3;
pub const DEFAULT_RAW_CLOCK_ALIGNMENT_LAG_US: u64 = 5_000;
pub const DEFAULT_RAW_CLOCK_ALIGNMENT_SEARCH_WINDOW_US: u64 = 25_000;
pub const DEFAULT_RAW_CLOCK_ALIGNMENT_BUFFER_MISS_CONSECUTIVE_FAILURES: u32 = 3;

pub const MAX_RAW_CLOCK_WARMUP_SECS: u64 = 3600;
pub const MAX_RAW_CLOCK_RESIDUAL_P95_US: u64 = 100_000;
pub const MAX_RAW_CLOCK_RESIDUAL_MAX_US: u64 = 250_000;
pub const MAX_RAW_CLOCK_DRIFT_ABS_PPM: f64 = 1000.0;
pub const MAX_RAW_CLOCK_SAMPLE_GAP_MAX_MS: u64 = 1000;
pub const MAX_RAW_CLOCK_LAST_SAMPLE_AGE_MS: u64 = 1000;
pub const MAX_RAW_CLOCK_SELECTED_SAMPLE_AGE_MS: u64 = 1000;
pub const MAX_RAW_CLOCK_STATE_SKEW_MAX_US: u64 = 100_000;
pub const MAX_RAW_CLOCK_INTER_ARM_SKEW_MAX_US: u64 = 100_000;
pub const MAX_RAW_CLOCK_RESIDUAL_MAX_CONSECUTIVE_FAILURES: u32 = 100;
pub const MAX_RAW_CLOCK_ALIGNMENT_LAG_US: u64 = 100_000;
pub const MAX_RAW_CLOCK_ALIGNMENT_SEARCH_WINDOW_US: u64 = 100_000;
pub const MAX_RAW_CLOCK_ALIGNMENT_BUFFER_MISS_CONSECUTIVE_FAILURES: u32 = 100;
```

Keep validation maxima aligned with `apps/cli/src/teleop/config.rs` unless hardware data justifies a tighter SVS-specific limit.

## Task 1: Extract Reusable Bilateral Loop Helpers

**Files:**
- Modify: `crates/piper-client/src/dual_arm.rs`

- [ ] **Step 1: Write the failing helper config test**

Add this test near the existing bilateral loop tests in `crates/piper-client/src/dual_arm.rs`:

```rust
#[test]
fn bilateral_output_shaping_config_copies_loop_limits() {
    let cfg = BilateralLoopConfig {
        master_interaction_lpf_cutoff_hz: 11.0,
        master_interaction_limit: JointArray::splat(NewtonMeter(1.2)),
        slave_feedforward_limit: JointArray::splat(NewtonMeter(3.4)),
        master_interaction_slew_limit_nm_per_s: JointArray::splat(NewtonMeter(5.6)),
        master_passivity_enabled: false,
        master_passivity_max_damping: JointArray::splat(0.7),
        ..BilateralLoopConfig::default()
    };

    let shaping = BilateralOutputShapingConfig::from_loop_config(&cfg);

    assert_eq!(shaping.master_interaction_lpf_cutoff_hz, 11.0);
    assert_eq!(shaping.master_interaction_limit, JointArray::splat(NewtonMeter(1.2)));
    assert_eq!(shaping.slave_feedforward_limit, JointArray::splat(NewtonMeter(3.4)));
    assert_eq!(
        shaping.master_interaction_slew_limit_nm_per_s,
        JointArray::splat(NewtonMeter(5.6))
    );
    assert!(!shaping.master_passivity_enabled);
    assert_eq!(shaping.master_passivity_max_damping, JointArray::splat(0.7));
}
```

- [ ] **Step 2: Run the test to verify RED**

Run:

```bash
cargo test -p piper-client bilateral_output_shaping_config_copies_loop_limits -- --nocapture
```

Expected: FAIL because `BilateralOutputShapingConfig` does not exist.

- [ ] **Step 3: Add reusable output-shaping config**

In `crates/piper-client/src/dual_arm.rs`, below `BilateralLoopConfig`, add:

```rust
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BilateralOutputShapingConfig {
    pub master_interaction_lpf_cutoff_hz: f64,
    pub master_interaction_limit: JointArray<NewtonMeter>,
    pub slave_feedforward_limit: JointArray<NewtonMeter>,
    pub master_interaction_slew_limit_nm_per_s: JointArray<NewtonMeter>,
    pub master_passivity_enabled: bool,
    pub master_passivity_max_damping: JointArray<f64>,
}

impl Default for BilateralOutputShapingConfig {
    fn default() -> Self {
        Self {
            master_interaction_lpf_cutoff_hz: 20.0,
            master_interaction_limit: JointArray::splat(NewtonMeter(1.5)),
            slave_feedforward_limit: JointArray::splat(NewtonMeter(4.0)),
            master_interaction_slew_limit_nm_per_s: JointArray::splat(NewtonMeter(50.0)),
            master_passivity_enabled: true,
            master_passivity_max_damping: JointArray::splat(1.0),
        }
    }
}

impl BilateralOutputShapingConfig {
    pub fn from_loop_config(cfg: &BilateralLoopConfig) -> Self {
        Self {
            master_interaction_lpf_cutoff_hz: cfg.master_interaction_lpf_cutoff_hz,
            master_interaction_limit: cfg.master_interaction_limit,
            slave_feedforward_limit: cfg.slave_feedforward_limit,
            master_interaction_slew_limit_nm_per_s: cfg.master_interaction_slew_limit_nm_per_s,
            master_passivity_enabled: cfg.master_passivity_enabled,
            master_passivity_max_damping: cfg.master_passivity_max_damping,
        }
    }
}
```

Change `OutputShapingState` to:

```rust
#[derive(Debug)]
pub(crate) struct BilateralOutputShapingState {
    master_interaction_filtered: JointArray<NewtonMeter>,
    last_master_interaction: JointArray<NewtonMeter>,
    passivity_energy: f64,
}

impl Default for BilateralOutputShapingState {
    fn default() -> Self {
        Self {
            master_interaction_filtered: JointArray::splat(NewtonMeter::ZERO),
            last_master_interaction: JointArray::splat(NewtonMeter::ZERO),
            passivity_energy: 0.0,
        }
    }
}
```

Update the StrictRealtime loop local variable from `OutputShapingState::default()` to `BilateralOutputShapingState::default()`.

To keep existing in-file tests/helpers compiling while later tasks migrate names, add this compatibility alias immediately after `BilateralOutputShapingState`:

```rust
pub(crate) type OutputShapingState = BilateralOutputShapingState;
```

- [ ] **Step 4: Make helper functions reusable**

Change these existing helpers in `crates/piper-client/src/dual_arm.rs`:

```rust
pub(crate) fn duration_micros_u64(duration: Duration) -> u64;
pub(crate) fn clamp_control_dt_us(raw_dt_us: u64, max_dt_us: u64) -> u64;
pub(crate) fn deadline_missed_after_submission(
    submission_deadline_mono_us: u64,
    master_tx_finished_host_mono_us: Option<u64>,
    slave_tx_finished_host_mono_us: Option<u64>,
    post_submission_host_mono_us: u64,
) -> bool;
pub(crate) fn assemble_final_torques(
    command: &BilateralCommand,
    compensation: Option<BilateralDynamicsCompensation>,
) -> BilateralFinalTorques;
pub(crate) fn build_gripper_telemetry(
    cfg: &GripperTeleopConfig,
    mirror_enabled: bool,
    master: crate::observer::GripperState,
    slave: crate::observer::GripperState,
    control_frame_host_mono_us: u64,
) -> BilateralLoopGripperTelemetry;
```

Change `apply_output_shaping` into:

```rust
pub(crate) fn apply_output_shaping(
    cfg: &BilateralOutputShapingConfig,
    snapshot: &DualArmSnapshot,
    dt: Duration,
    state: &mut BilateralOutputShapingState,
    command: &mut BilateralCommand,
);
```

Keep the existing implementation body for `apply_output_shaping`; the only logic change is that the `cfg` parameter type becomes `&BilateralOutputShapingConfig`.

In the StrictRealtime loop, replace the call with:

```rust
let shaping_cfg = BilateralOutputShapingConfig::from_loop_config(&cfg);
apply_output_shaping(
    &shaping_cfg,
    &frame.snapshot,
    control_dt,
    &mut shaping_state,
    &mut command,
);
```

Keep the existing output-shaping math unchanged.

- [ ] **Step 5: Run focused client tests**

Run:

```bash
cargo test -p piper-client bilateral_output_shaping_config_copies_loop_limits -- --nocapture
cargo test -p piper-client run_bilateral_with_compensation -- --test-threads=1 --nocapture
```

Expected: PASS. If the second command matches no tests, run:

```bash
cargo test -p piper-client dual_arm -- --test-threads=1 --nocapture
```

- [ ] **Step 6: Commit**

```bash
git add crates/piper-client/src/dual_arm.rs
git commit -m "Expose shared bilateral loop helpers"
```

## Task 2: Add Raw-Clock Fault Counters And Exit Reasons

**Files:**
- Modify: `crates/piper-client/src/dual_arm_raw_clock.rs`
- Modify: `apps/cli/src/teleop/workflow.rs`

- [ ] **Step 1: Write failing report default test**

In `crates/piper-client/src/dual_arm_raw_clock.rs`, add near the existing report tests:

```rust
#[test]
fn raw_clock_report_defaults_include_generic_fault_counters() {
    let report = RawClockRuntimeTiming::new(thresholds_for_tests())
        .report(120_000, 0, Some(RawClockRuntimeExitReason::MaxIterations));

    assert_eq!(report.compensation_faults, 0);
    assert_eq!(report.controller_faults, 0);
    assert_eq!(report.telemetry_sink_faults, 0);
}
```

- [ ] **Step 2: Run the test to verify RED**

Run:

```bash
cargo test -p piper-client raw_clock_report_defaults_include_generic_fault_counters -- --nocapture
```

Expected: FAIL because the new fields do not exist.

- [ ] **Step 3: Add counters and exit reasons**

In `RawClockRuntimeReport`, add fields after `clock_health_failures`:

```rust
pub compensation_faults: u32,
pub controller_faults: u32,
pub telemetry_sink_faults: u32,
```

In `RawClockRuntimeExitReason`, add:

```rust
CompensationFault,
ControllerFault,
RuntimeConfigFault,
TelemetrySinkFault,
```

If `ControllerFault` already exists in the branch, do not add a duplicate variant; keep the existing discriminant/name and only wire the new counter behavior below.

In `RawClockRuntimeTiming::report`, initialize all three new counters to `0`.

In `run_raw_clock_runtime_core`, when controller tick fails, update the report closure:

```rust
|report| report.controller_faults = report.controller_faults.saturating_add(1)
```

Do not add compensation/telemetry increments until Task 3 introduces those paths.

- [ ] **Step 4: Update CLI raw-clock mapping**

In `apps/cli/src/teleop/workflow.rs`, update `map_raw_clock_exit_reason`:

```rust
RawClockRuntimeExitReason::CompensationFault => BilateralExitReason::CompensationFault,
RawClockRuntimeExitReason::ControllerFault => BilateralExitReason::ControllerFault,
RawClockRuntimeExitReason::RuntimeConfigFault => BilateralExitReason::RuntimeTransportFault,
RawClockRuntimeExitReason::TelemetrySinkFault => BilateralExitReason::TelemetrySinkFault,
```

`RuntimeConfigFault` remains a raw-clock-specific final failure kind for post-enable SDK guardrails. Until `BilateralExitReason` grows a dedicated config variant, CLI/SVS status mapping may group it under `RuntimeTransportFault`, but raw-clock JSON must preserve `final_failure_kind = "RuntimeConfigFault"`.

Update every struct literal that constructs `RawClockRuntimeReport` in workflow tests and `raw_clock_error_report` with:

```rust
compensation_faults: 0,
controller_faults: 0,
telemetry_sink_faults: 0,
```

- [ ] **Step 5: Run focused tests**

Run:

```bash
cargo test -p piper-client raw_clock_report_defaults_include_generic_fault_counters -- --nocapture
cargo test -p piper-cli raw_clock -- --nocapture
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/piper-client/src/dual_arm_raw_clock.rs apps/cli/src/teleop/workflow.rs
git commit -m "Track raw-clock generic runtime faults"
```

## Task 3: Add Compensation-Capable Raw-Clock Runtime

**Files:**
- Modify: `crates/piper-client/src/dual_arm_raw_clock.rs`

- [ ] **Step 1: Write failing compensation flow tests**

Add these test helpers inside `#[cfg(test)] mod tests` in `dual_arm_raw_clock.rs`:

```rust
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
struct FakeCompensatorError(&'static str);

struct RecordingCompensator {
    reset_calls: Arc<AtomicUsize>,
    compute_calls: Arc<AtomicUsize>,
    time_jump_calls: Arc<AtomicUsize>,
    seen_snapshots: Arc<Mutex<Vec<DualArmSnapshot>>>,
    fail_reset: bool,
    fail_compute: bool,
    fail_time_jump: bool,
    compensation: BilateralDynamicsCompensation,
}

impl Default for RecordingCompensator {
    fn default() -> Self {
        Self {
            reset_calls: Arc::new(AtomicUsize::new(0)),
            compute_calls: Arc::new(AtomicUsize::new(0)),
            time_jump_calls: Arc::new(AtomicUsize::new(0)),
            seen_snapshots: Arc::new(Mutex::new(Vec::new())),
            fail_reset: false,
            fail_compute: false,
            fail_time_jump: false,
            compensation: BilateralDynamicsCompensation::zero(),
        }
    }
}

impl BilateralDynamicsCompensator for RecordingCompensator {
    type Error = FakeCompensatorError;

    fn compute(
        &mut self,
        snapshot: &DualArmSnapshot,
        _dt: Duration,
    ) -> std::result::Result<BilateralDynamicsCompensation, Self::Error> {
        self.compute_calls.fetch_add(1, AtomicOrdering::SeqCst);
        self.seen_snapshots
            .lock()
            .expect("snapshot lock")
            .push(snapshot.clone());
        if self.fail_compute {
            return Err(FakeCompensatorError("compensator compute failed"));
        }
        Ok(self.compensation)
    }

    fn on_time_jump(&mut self, _dt: Duration) -> std::result::Result<(), Self::Error> {
        self.time_jump_calls.fetch_add(1, AtomicOrdering::SeqCst);
        if self.fail_time_jump {
            return Err(FakeCompensatorError("compensator time jump failed"));
        }
        Ok(())
    }

    fn reset(&mut self) -> std::result::Result<(), Self::Error> {
        self.reset_calls.fetch_add(1, AtomicOrdering::SeqCst);
        if self.fail_reset {
            return Err(FakeCompensatorError("compensator reset failed"));
        }
        Ok(())
    }
}

struct RecordingCompensationController {
    seen_compensation: Arc<Mutex<Vec<Option<BilateralDynamicsCompensation>>>>,
    fail_tick: bool,
}

impl BilateralController for RecordingCompensationController {
    type Error = FakeControllerError;

    fn tick(
        &mut self,
        _snapshot: &DualArmSnapshot,
        _dt: Duration,
    ) -> std::result::Result<BilateralCommand, Self::Error> {
        unreachable!("raw-clock compensation path should call tick_with_compensation")
    }

    fn tick_with_compensation(
        &mut self,
        frame: &BilateralControlFrame,
        _dt: Duration,
    ) -> std::result::Result<BilateralCommand, Self::Error> {
        if self.fail_tick {
            return Err(FakeControllerError("controller tick failed"));
        }
        self.seen_compensation.lock().expect("seen comp lock").push(frame.compensation);
        Ok(test_command())
    }
}

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
struct FakeControllerError(&'static str);

struct RecordingTimeJumpController {
    time_jump_calls: Arc<AtomicUsize>,
    fail_time_jump: bool,
}

impl BilateralController for RecordingTimeJumpController {
    type Error = FakeControllerError;

    fn tick(
        &mut self,
        _snapshot: &DualArmSnapshot,
        _dt: Duration,
    ) -> std::result::Result<BilateralCommand, Self::Error> {
        Ok(test_command())
    }

    fn on_time_jump(&mut self, _dt: Duration) -> std::result::Result<(), Self::Error> {
        self.time_jump_calls.fetch_add(1, AtomicOrdering::SeqCst);
        if self.fail_time_jump {
            return Err(FakeControllerError("controller time jump failed"));
        }
        Ok(())
    }
}
```

Then add:

```rust
#[test]
fn raw_clock_compensation_path_resets_computes_and_reaches_controller() {
    let compensation = BilateralDynamicsCompensation {
        master_model_torque: JointArray::splat(NewtonMeter(0.1)),
        slave_model_torque: JointArray::splat(NewtonMeter(0.2)),
        master_external_torque_est: JointArray::splat(NewtonMeter(0.3)),
        slave_external_torque_est: JointArray::splat(NewtonMeter(0.4)),
    };
    let reset_calls = Arc::new(AtomicUsize::new(0));
    let compute_calls = Arc::new(AtomicUsize::new(0));
    let compensator = RecordingCompensator {
        reset_calls: reset_calls.clone(),
        compute_calls: compute_calls.clone(),
        compensation,
        ..RecordingCompensator::default()
    };
    let seen = Arc::new(Mutex::new(Vec::new()));
    let controller = RecordingCompensationController {
        seen_compensation: seen.clone(),
        fail_tick: false,
    };

    let (_state, report) = run_fake_runtime_with_controller_and_compensation(
        FakeRuntimeIo::new().with_reads(ready_reads_for_iterations(1)),
        ready_timing_for_tests(),
        1,
        RawClockRuntimeThresholds::for_tests(),
        controller,
        compensator,
        ExperimentalRawClockRunConfig::default(),
    );

    assert_eq!(report.exit_reason, Some(RawClockRuntimeExitReason::MaxIterations));
    assert_eq!(reset_calls.load(AtomicOrdering::SeqCst), 1);
    assert_eq!(compute_calls.load(AtomicOrdering::SeqCst), 1);
    assert_eq!(seen.lock().expect("seen comp lock").as_slice(), &[Some(compensation)]);
}

#[test]
fn raw_clock_compensator_receives_aligned_selected_snapshot() {
    let seen_snapshots = Arc::new(Mutex::new(Vec::new()));
    let master_positions = [0.11, 0.12, 0.13, 0.14, 0.15, 0.16];
    let slave_positions = [0.21, 0.22, 0.23, 0.24, 0.25, 0.26];
    let compensator = RecordingCompensator {
        seen_snapshots: seen_snapshots.clone(),
        ..RecordingCompensator::default()
    };

    let (_state, report) = run_fake_runtime_with_controller_and_compensation(
        FakeRuntimeIo::new().with_reads(vec![FakeRead::pair(
            raw_clock_snapshot_with_positions_for_tests(110_000, 100_000, master_positions),
            raw_clock_snapshot_with_positions_for_tests(110_500, 100_500, slave_positions),
        )]),
        ready_timing_for_tests(),
        1,
        RawClockRuntimeThresholds::for_tests(),
        TestCommandController,
        compensator,
        ExperimentalRawClockRunConfig::default(),
    );

    assert_eq!(report.exit_reason, Some(RawClockRuntimeExitReason::MaxIterations));
    let snapshots = seen_snapshots.lock().expect("snapshot lock");
    assert_eq!(snapshots.len(), 1);
    assert_eq!(
        snapshots[0].left.state.position,
        JointArray::new(master_positions.map(Rad))
    );
    assert_eq!(
        snapshots[0].right.state.position,
        JointArray::new(slave_positions.map(Rad))
    );
    assert!(
        u64::try_from(snapshots[0].inter_arm_skew.as_micros()).unwrap_or(u64::MAX)
            <= RawClockRuntimeThresholds::for_tests().inter_arm_skew_max_us
    );
}

#[test]
fn raw_clock_controller_tick_failure_faults_report() {
    let controller = RecordingCompensationController {
        seen_compensation: Arc::new(Mutex::new(Vec::new())),
        fail_tick: true,
    };

    let (_state, report) = run_fake_runtime_with_controller_and_config(
        FakeRuntimeIo::new().with_reads(ready_reads_for_iterations(1)),
        ready_timing_for_tests(),
        1,
        RawClockRuntimeThresholds::for_tests(),
        controller,
        ExperimentalRawClockRunConfig::default(),
    );

    assert_eq!(report.exit_reason, Some(RawClockRuntimeExitReason::ControllerFault));
    assert_eq!(report.controller_faults, 1);
}

#[test]
fn raw_clock_compensator_reset_failure_faults_before_first_iteration() {
    let compensator = RecordingCompensator {
        fail_reset: true,
        ..RecordingCompensator::default()
    };
    let (_state, report) = run_fake_runtime_with_controller_and_compensation(
        FakeRuntimeIo::new().with_reads(ready_reads_for_iterations(1)),
        ready_timing_for_tests(),
        1,
        RawClockRuntimeThresholds::for_tests(),
        TestCommandController,
        compensator,
        ExperimentalRawClockRunConfig::default(),
    );

    assert_eq!(report.exit_reason, Some(RawClockRuntimeExitReason::CompensationFault));
    assert_eq!(report.compensation_faults, 1);
    assert_eq!(report.iterations, 0);
}

#[test]
fn raw_clock_time_jump_notifies_controller_and_compensator() {
    let controller_calls = Arc::new(AtomicUsize::new(0));
    let compensator_calls = Arc::new(AtomicUsize::new(0));
    let compensator = RecordingCompensator {
        time_jump_calls: compensator_calls.clone(),
        ..RecordingCompensator::default()
    };
    let controller = RecordingTimeJumpController {
        time_jump_calls: controller_calls.clone(),
        fail_time_jump: false,
    };

    let (_state, report) = run_fake_runtime_with_controller_and_compensation(
        FakeRuntimeIo::new().with_reads(ready_reads_for_host_rx_us(&[100_000, 140_000])),
        ready_timing_for_tests(),
        2,
        RawClockRuntimeThresholds::for_tests(),
        controller,
        compensator,
        ExperimentalRawClockRunConfig {
            dt_clamp_multiplier: 1.0,
            ..ExperimentalRawClockRunConfig::default()
        },
    );

    assert_eq!(report.exit_reason, Some(RawClockRuntimeExitReason::MaxIterations));
    assert_eq!(controller_calls.load(AtomicOrdering::SeqCst), 1);
    assert_eq!(compensator_calls.load(AtomicOrdering::SeqCst), 1);
}

#[test]
fn raw_clock_controller_time_jump_failure_faults_report() {
    let controller = RecordingTimeJumpController {
        time_jump_calls: Arc::new(AtomicUsize::new(0)),
        fail_time_jump: true,
    };

    let (_state, report) = run_fake_runtime_with_controller_and_config(
        FakeRuntimeIo::new().with_reads(ready_reads_for_host_rx_us(&[100_000, 140_000])),
        ready_timing_for_tests(),
        2,
        RawClockRuntimeThresholds::for_tests(),
        controller,
        ExperimentalRawClockRunConfig {
            dt_clamp_multiplier: 1.0,
            ..ExperimentalRawClockRunConfig::default()
        },
    );

    assert_eq!(report.exit_reason, Some(RawClockRuntimeExitReason::ControllerFault));
    assert_eq!(report.controller_faults, 1);
}

#[test]
fn raw_clock_compensator_time_jump_failure_faults_report() {
    let compensator = RecordingCompensator {
        fail_time_jump: true,
        ..RecordingCompensator::default()
    };

    let (_state, report) = run_fake_runtime_with_controller_and_compensation(
        FakeRuntimeIo::new().with_reads(ready_reads_for_host_rx_us(&[100_000, 140_000])),
        ready_timing_for_tests(),
        2,
        RawClockRuntimeThresholds::for_tests(),
        TestCommandController,
        compensator,
        ExperimentalRawClockRunConfig {
            dt_clamp_multiplier: 1.0,
            ..ExperimentalRawClockRunConfig::default()
        },
    );

    assert_eq!(report.exit_reason, Some(RawClockRuntimeExitReason::CompensationFault));
    assert_eq!(report.compensation_faults, 1);
}

#[test]
fn raw_clock_no_telemetry_no_compensator_preserves_submission_order_and_default_gripper() {
    let (state, report) = run_fake_runtime_with_controller_and_config(
        FakeRuntimeIo::new().with_reads(ready_reads_for_iterations(1)),
        ready_timing_for_tests(),
        1,
        RawClockRuntimeThresholds::for_tests(),
        TestCommandController,
        ExperimentalRawClockRunConfig::default(),
    );

    assert_eq!(report.exit_reason, Some(RawClockRuntimeExitReason::MaxIterations));
    assert_eq!(
        state.command_log.lock().expect("command log lock").as_slice(),
        &["slave", "master"]
    );
    let commands = state.commands.lock().expect("commands lock");
    assert_eq!(commands.len(), 1);
    assert_eq!(commands[0].master_interaction_torque, JointArray::splat(NewtonMeter::ZERO));
    assert_eq!(commands[0].slave_feedforward_torque, JointArray::splat(NewtonMeter::ZERO));
}

#[test]
fn raw_clock_compensation_final_torques_are_submitted_without_telemetry() {
    let compensation = BilateralDynamicsCompensation {
        master_model_torque: JointArray::splat(NewtonMeter(0.7)),
        slave_model_torque: JointArray::splat(NewtonMeter(0.9)),
        master_external_torque_est: JointArray::splat(NewtonMeter::ZERO),
        slave_external_torque_est: JointArray::splat(NewtonMeter::ZERO),
    };
    let compensator = RecordingCompensator {
        compensation,
        ..RecordingCompensator::default()
    };

    let (state, report) = run_fake_runtime_with_controller_and_compensation(
        FakeRuntimeIo::new().with_reads(ready_reads_for_iterations(1)),
        ready_timing_for_tests(),
        1,
        RawClockRuntimeThresholds::for_tests(),
        TestCommandController,
        compensator,
        ExperimentalRawClockRunConfig::default(),
    );

    assert_eq!(report.exit_reason, Some(RawClockRuntimeExitReason::MaxIterations));
    let submitted = state.submitted_final_torques.lock().expect("submitted torques lock");
    assert_eq!(submitted.len(), 1);
    assert_eq!(submitted[0].master, JointArray::splat(NewtonMeter(0.7)));
    assert_eq!(submitted[0].slave, JointArray::splat(NewtonMeter(0.9)));
}

#[test]
fn raw_clock_gripper_mirroring_config_is_rejected_by_sdk_runtime() {
    let io = FakeRuntimeIo::new().with_reads(ready_reads_for_iterations(1));
    let state = io.state.clone();
    let core = RawClockRuntimeCore {
        io,
        timing: ready_timing_for_tests(),
        config: ExperimentalRawClockConfig {
            mode: ExperimentalRawClockMode::Bilateral,
            frequency_hz: 10_000.0,
            max_iterations: Some(1),
            thresholds: RawClockRuntimeThresholds::for_tests(),
            estimator_thresholds: thresholds_for_tests(),
        },
    };
    let mut run_config = ExperimentalRawClockRunConfig::default();
    run_config.gripper.enabled = true;

    let exit = run_raw_clock_runtime_core(core, TestCommandController, None, run_config)
        .expect("gripper rejection should return a faulted exit, not drop active arms");

    let report = match exit {
        RawClockCoreExit::Faulted { report, .. } => report,
        RawClockCoreExit::Standby { .. } => panic!("gripper rejection should fault"),
    };

    assert_eq!(
        report.exit_reason,
        Some(RawClockRuntimeExitReason::RuntimeConfigFault)
    );
    assert!(report.last_error.as_deref().unwrap_or_default().contains("gripper"));
    assert!(state.commands.lock().expect("commands lock").is_empty());
}

#[test]
fn raw_clock_core_invalid_config_faults_instead_of_outer_error() {
    let io = FakeRuntimeIo::new().with_reads(ready_reads_for_iterations(1));
    let state = io.state.clone();
    let core = RawClockRuntimeCore {
        io,
        timing: ready_timing_for_tests(),
        config: ExperimentalRawClockConfig {
            mode: ExperimentalRawClockMode::Bilateral,
            frequency_hz: 0.0,
            max_iterations: Some(1),
            thresholds: RawClockRuntimeThresholds::for_tests(),
            estimator_thresholds: thresholds_for_tests(),
        },
    };

    let exit = run_raw_clock_runtime_core(
        core,
        TestCommandController,
        None,
        ExperimentalRawClockRunConfig::default(),
    )
    .expect("invalid config should be converted to a faulted exit, not an outer error");

    let report = match exit {
        RawClockCoreExit::Faulted { report, .. } => report,
        RawClockCoreExit::Standby { .. } => panic!("invalid config should fault"),
    };

    assert_eq!(
        report.exit_reason,
        Some(RawClockRuntimeExitReason::RuntimeConfigFault)
    );
    assert!(report.last_error.as_deref().unwrap_or_default().contains("frequency_hz"));
    assert_eq!(report.runtime_faults, 1);
    assert!(state.commands.lock().expect("commands lock").is_empty());
}
```

Use existing helper patterns for `ready_reads_for_iterations`; if it does not exist, add:

```rust
fn ready_reads_for_iterations(iterations: usize) -> Vec<FakeRead> {
    (0..iterations)
        .map(|index| {
            let base = 110_000 + index as u64 * 1_000;
            FakeRead::pair(
                raw_clock_snapshot_for_tests(base, base + 100_000),
                raw_clock_snapshot_for_tests(base + 500, base + 100_500),
            )
        })
        .collect()
}

fn ready_reads_for_host_rx_us(host_rx_us: &[u64]) -> Vec<FakeRead> {
    host_rx_us
        .iter()
        .copied()
        .enumerate()
        .map(|(index, host)| {
            let raw = 110_000 + index as u64 * 1_000;
            FakeRead::pair(
                raw_clock_snapshot_for_tests(raw, host),
                raw_clock_snapshot_for_tests(raw + 500, host + 500),
            )
        })
        .collect()
}
```

Update the fake runtime helpers so tests can pass non-default run config and optional compensation:

```rust
fn run_fake_runtime_with_controller<C>(
    io: FakeRuntimeIo,
    timing: RawClockRuntimeTiming,
    max_iterations: usize,
    thresholds: RawClockRuntimeThresholds,
    controller: C,
) -> (Arc<FakeIoState>, RawClockRuntimeReport)
where
    C: BilateralController,
{
    run_fake_runtime_with_controller_and_config(
        io,
        timing,
        max_iterations,
        thresholds,
        controller,
        ExperimentalRawClockRunConfig::default(),
    )
}

fn run_fake_runtime_with_controller_and_config<C>(
    io: FakeRuntimeIo,
    timing: RawClockRuntimeTiming,
    max_iterations: usize,
    thresholds: RawClockRuntimeThresholds,
    controller: C,
    run_config: ExperimentalRawClockRunConfig,
) -> (Arc<FakeIoState>, RawClockRuntimeReport)
where
    C: BilateralController,
{
    run_fake_runtime_with_optional_compensation(
        io,
        timing,
        max_iterations,
        thresholds,
        controller,
        None,
        run_config,
    )
}

fn run_fake_runtime_with_controller_and_compensation<C, D>(
    io: FakeRuntimeIo,
    timing: RawClockRuntimeTiming,
    max_iterations: usize,
    thresholds: RawClockRuntimeThresholds,
    controller: C,
    compensator: D,
    run_config: ExperimentalRawClockRunConfig,
) -> (Arc<FakeIoState>, RawClockRuntimeReport)
where
    C: BilateralController,
    D: BilateralDynamicsCompensator,
{
    let mut compensator = RawClockCompensatorAdapter::new(compensator);
    run_fake_runtime_with_optional_compensation(
        io,
        timing,
        max_iterations,
        thresholds,
        controller,
        Some(&mut compensator),
        run_config,
    )
}

fn run_fake_runtime_with_optional_compensation<C>(
    io: FakeRuntimeIo,
    timing: RawClockRuntimeTiming,
    max_iterations: usize,
    thresholds: RawClockRuntimeThresholds,
    controller: C,
    compensator: Option<&mut dyn RawClockDynamicsCompensator>,
    run_config: ExperimentalRawClockRunConfig,
) -> (Arc<FakeIoState>, RawClockRuntimeReport)
where
    C: BilateralController,
{
    let state = io.state.clone();
    let core = RawClockRuntimeCore {
        io,
        timing,
        config: ExperimentalRawClockConfig {
            mode: ExperimentalRawClockMode::Bilateral,
            frequency_hz: 10_000.0,
            max_iterations: Some(max_iterations),
            thresholds,
            estimator_thresholds: thresholds_for_tests(),
        },
    };
    let exit = run_raw_clock_runtime_core(core, controller, compensator, run_config)
        .expect("fake runtime core should not return outer error");
    let report = match exit {
        RawClockCoreExit::Standby { report, .. } | RawClockCoreExit::Faulted { report, .. } => {
            report
        },
    };
    (state, report)
}
```

- [ ] **Step 2: Run the compensation tests to verify RED**

Run:

```bash
cargo test -p piper-client raw_clock_compensation -- --test-threads=1 --nocapture
```

Expected: FAIL because there is no compensation-capable helper/runtime.

- [ ] **Step 3: Import shared helpers and define raw-clock compensation adapter**

At the top of `dual_arm_raw_clock.rs`, expand the `crate::dual_arm` import:

```rust
use crate::dual_arm::{
    apply_output_shaping, assemble_final_torques, build_gripper_telemetry,
    clamp_control_dt_us, deadline_missed_after_submission, duration_micros_u64,
    BilateralCommand, BilateralControlFrame, BilateralController,
    BilateralDynamicsCompensation, BilateralDynamicsCompensator,
    BilateralFinalTorques, BilateralLoopGripperTelemetry, BilateralLoopTelemetry,
    BilateralLoopTelemetrySink, BilateralLoopTimingTelemetry, BilateralOutputShapingConfig,
    BilateralOutputShapingState, BilateralTelemetrySinkError, GripperTeleopConfig,
    MasterFollowerController, StopAttemptResult,
};
```

Add a private adapter in `dual_arm_raw_clock.rs`:

```rust
trait RawClockDynamicsCompensator {
    fn compute(
        &mut self,
        snapshot: &DualArmSnapshot,
        dt: Duration,
    ) -> std::result::Result<BilateralDynamicsCompensation, String>;

    fn on_time_jump(&mut self, dt: Duration) -> std::result::Result<(), String>;

    fn reset(&mut self) -> std::result::Result<(), String>;
}

struct RawClockCompensatorAdapter<C> {
    inner: C,
}

impl<C> RawClockCompensatorAdapter<C> {
    fn new(inner: C) -> Self {
        Self { inner }
    }
}

impl<C> RawClockDynamicsCompensator for RawClockCompensatorAdapter<C>
where
    C: BilateralDynamicsCompensator,
{
    fn compute(
        &mut self,
        snapshot: &DualArmSnapshot,
        dt: Duration,
    ) -> std::result::Result<BilateralDynamicsCompensation, String> {
        self.inner.compute(snapshot, dt).map_err(|error| error.to_string())
    }

    fn on_time_jump(&mut self, dt: Duration) -> std::result::Result<(), String> {
        self.inner.on_time_jump(dt).map_err(|error| error.to_string())
    }

    fn reset(&mut self) -> std::result::Result<(), String> {
        self.inner.reset().map_err(|error| error.to_string())
    }
}
```

- [ ] **Step 4: Extend raw-clock run config without changing current defaults**

Extend `ExperimentalRawClockRunConfig`:

```rust
#[derive(Clone)]
pub struct ExperimentalRawClockRunConfig {
    pub read_policy: ControlReadPolicy,
    pub command_timeout: Duration,
    pub disable_config: DisableConfig,
    pub cancel_signal: Option<Arc<AtomicBool>>,
    pub dt_clamp_multiplier: f64,
    pub telemetry_sink: Option<Arc<dyn BilateralLoopTelemetrySink>>,
    pub gripper: GripperTeleopConfig,
    pub output_shaping: Option<BilateralOutputShapingConfig>,
}
```

Replace the old `#[derive(Debug, Clone)]` with `#[derive(Clone)]` because `Arc<dyn BilateralLoopTelemetrySink>` is not `Debug`. Add a manual debug impl that hides the sink:

```rust
impl std::fmt::Debug for ExperimentalRawClockRunConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExperimentalRawClockRunConfig")
            .field("read_policy", &self.read_policy)
            .field("command_timeout", &self.command_timeout)
            .field("disable_config", &self.disable_config)
            .field("cancel_signal", &self.cancel_signal.as_ref().map(|_| "<set>"))
            .field("dt_clamp_multiplier", &self.dt_clamp_multiplier)
            .field("telemetry_sink", &self.telemetry_sink.as_ref().map(|_| "<set>"))
            .field("gripper", &self.gripper)
            .field("output_shaping", &self.output_shaping)
            .finish()
    }
}
```

Default:

```rust
Self {
    read_policy: ControlReadPolicy {
        max_state_skew_us: 2_000,
        max_feedback_age: DEFAULT_CONTROL_MAX_FEEDBACK_AGE,
    },
    command_timeout: Duration::from_millis(20),
    disable_config: DisableConfig::default(),
    cancel_signal: None,
    dt_clamp_multiplier: 2.0,
    telemetry_sink: None,
    gripper: GripperTeleopConfig {
        enabled: false,
        ..GripperTeleopConfig::default()
    },
    output_shaping: None,
}
```

`output_shaping: None` preserves current CLI raw-clock command behavior.

Add validation used by both SDK entry points and SVS conversion:

```rust
impl ExperimentalRawClockRunConfig {
    pub fn validate(&self) -> Result<(), RawClockRuntimeError> {
        if self.command_timeout.is_zero() {
            return Err(RawClockRuntimeError::Config(
                "raw-clock command_timeout must be positive".to_string(),
            ));
        }
        if !self.dt_clamp_multiplier.is_finite() || self.dt_clamp_multiplier <= 0.0 {
            return Err(RawClockRuntimeError::Config(
                "raw-clock dt_clamp_multiplier must be finite and positive".to_string(),
            ));
        }
        if self.read_policy.max_state_skew_us == 0 {
            return Err(RawClockRuntimeError::Config(
                "raw-clock read_policy.max_state_skew_us must be positive".to_string(),
            ));
        }
        if self.read_policy.max_feedback_age.is_zero() {
            return Err(RawClockRuntimeError::Config(
                "raw-clock read_policy.max_feedback_age must be positive".to_string(),
            ));
        }
        Ok(())
    }
}
```

- [ ] **Step 5: Add explicit compensation errors**

Add to `RawClockRuntimeError`:

```rust
#[error("compensation fault: {0}")]
Compensation(String),
#[error("raw-clock telemetry sink fault: {0}")]
TelemetrySink(String),
```

- [ ] **Step 6: Add public active entry point**

In `impl ExperimentalRawClockDualArmActive`, add:

```rust
pub fn run_with_controller_and_compensation<C, D>(
    self,
    controller: C,
    compensator: D,
    cfg: ExperimentalRawClockRunConfig,
) -> Result<ExperimentalRawClockRunExit, RawClockRuntimeError>
where
    C: BilateralController,
    D: BilateralDynamicsCompensator,
{
    let mut compensator = RawClockCompensatorAdapter::new(compensator);
    self.run_with_controller_and_optional_compensation(controller, Some(&mut compensator), cfg)
}

fn run_with_controller_and_optional_compensation<C>(
    self,
    controller: C,
    compensator: Option<&mut dyn RawClockDynamicsCompensator>,
    cfg: ExperimentalRawClockRunConfig,
) -> Result<ExperimentalRawClockRunExit, RawClockRuntimeError>
where
    C: BilateralController,
{
    if let Err(error) = cfg.validate() {
        let timing_report = self.timing.report(
            piper_can::monotonic_micros(),
            0,
            Some(RawClockRuntimeExitReason::RuntimeConfigFault),
        );
        let (arms, shutdown) = self.fault_shutdown(FAULT_SHUTDOWN_TIMEOUT);
        let mut report = timing_report.with_shutdown(shutdown);
        report.runtime_faults = report.runtime_faults.saturating_add(1);
        report.last_error = Some(error.to_string());
        return Ok(ExperimentalRawClockRunExit::Faulted {
            arms: Box::new(arms),
            report,
        });
    }
    let ExperimentalRawClockDualArmActive {
        master,
        slave,
        timing,
        config,
    } = self;
    let core = RawClockRuntimeCore {
        io: RealRawClockRuntimeIo { master, slave },
        timing,
        config,
    };

    match run_raw_clock_runtime_core(core, controller, compensator, cfg)? {
        RawClockCoreExit::Standby {
            arms,
            timing,
            config,
            report,
        } => Ok(ExperimentalRawClockRunExit::Standby {
            arms: Box::new(ExperimentalRawClockDualArmStandby {
                master: arms.master,
                slave: arms.slave,
                timing: *timing,
                config: *config,
            }),
            report,
        }),
        RawClockCoreExit::Faulted { arms, report } => Ok(ExperimentalRawClockRunExit::Faulted {
            arms: Box::new(arms),
            report,
        }),
    }
}
```

Keep the SDK ownership contract explicit: after the active arms are moved into `RawClockRuntimeCore`, loop/runtime/read/submission/controller/compensation/telemetry failures must be converted into `RawClockCoreExit::Faulted` with bounded shutdown telemetry, not returned as an outer `Err`. Validation failures at the public active API boundary must be converted to `ExperimentalRawClockRunExit::Faulted` before destructuring `self`, as shown above.

Change existing `run_with_controller` to:

```rust
pub fn run_with_controller<C>(
    self,
    controller: C,
    cfg: ExperimentalRawClockRunConfig,
) -> Result<ExperimentalRawClockRunExit, RawClockRuntimeError>
where
    C: BilateralController,
{
    self.run_with_controller_and_optional_compensation(controller, None, cfg)
}
```

- [ ] **Step 7: Thread optional compensator through the core loop**

Change core signature to:

```rust
fn run_raw_clock_runtime_core<I, C>(
    core: RawClockRuntimeCore<I>,
    mut controller: C,
    mut compensator: Option<&mut dyn RawClockDynamicsCompensator>,
    cfg: ExperimentalRawClockRunConfig,
) -> Result<RawClockCoreExit<I::StandbyArms, I::ErrorArms>, RawClockRuntimeError>
where
    I: RawClockRuntimeIo,
    C: BilateralController,
```

Update existing call sites to pass `None`.

Change the IO trait so all raw-clock submissions can send final compensated torques:

```rust
fn submit_command(
    &mut self,
    command: &BilateralCommand,
    final_torques: &BilateralFinalTorques,
    timeout: Duration,
) -> Result<(), RawClockRuntimeError>;
```

Update `RealRawClockRuntimeIo::submit_command` and `FakeRuntimeIo::submit_command` to use this signature. In the real implementation, pass `final_torques.slave` and `final_torques.master` to the underlying MIT command calls. This preserves old no-compensator behavior because `assemble_final_torques(command, None)` equals the command's existing feedforward/interaction torque fields, and it makes compensation effective even when no telemetry sink is configured.

In the fake IO state, add:

```rust
submitted_final_torques: Mutex<Vec<BilateralFinalTorques>>,
```

In `FakeRuntimeIo::submit_command`, push `*final_torques` into `state.submitted_final_torques` before returning success. This is required by `raw_clock_compensation_final_torques_are_submitted_without_telemetry`.

`ExperimentalRawClockRunConfig` is validated in `run_with_controller_and_optional_compensation`; if validation fails, convert it to `ExperimentalRawClockRunExit::Faulted` with `self.fault_shutdown(...)` before building the core. `ExperimentalRawClockConfig` is normally validated when `ExperimentalRawClockDualArmStandby::new` constructs the standby, but direct fake/core tests can still construct `RawClockRuntimeCore` manually. Therefore `run_raw_clock_runtime_core` must also validate `config` at core entry and convert failures to a `RawClockCoreExit::Faulted`, not an outer `Err`.

At the top of `run_raw_clock_runtime_core`, after destructuring `core` and before any command/controller/compensation work, add these faulted-exit guards. Here `cfg.gripper.enabled` means “send gripper mirror commands”; raw-clock SVS rejects that before enable, so this SDK guard is only a runtime-config backstop:

```rust
if let Err(error) = config.validate() {
    let report = fault_report_from_timing(
        &timing,
        0,
        &RawClockJointMotionAccumulator::default(),
        RawClockRuntimeExitReason::RuntimeConfigFault,
        error.to_string(),
        |report| report.runtime_faults = report.runtime_faults.saturating_add(1),
    );
    let fault = io.fault_shutdown(FAULT_SHUTDOWN_TIMEOUT);
    return Ok(RawClockCoreExit::Faulted {
        arms: fault.arms,
        report: report.with_shutdown(fault.shutdown).with_telemetry(fault.telemetry),
    });
}

if cfg.gripper.enabled {
    let report = fault_report_from_timing(
        &timing,
        0,
        &RawClockJointMotionAccumulator::default(),
        RawClockRuntimeExitReason::RuntimeConfigFault,
        "raw-clock gripper mirroring is not implemented; pass disabled gripper config",
        |report| report.runtime_faults = report.runtime_faults.saturating_add(1),
    );
    let fault = io.fault_shutdown(FAULT_SHUTDOWN_TIMEOUT);
    return Ok(RawClockCoreExit::Faulted {
        arms: fault.arms,
        report: report.with_shutdown(fault.shutdown).with_telemetry(fault.telemetry),
    });
}
if let Some(compensator) = compensator.as_deref_mut()
    && let Err(error) = compensator.reset()
{
    let report = fault_report_from_timing(
        &timing,
        0,
        &RawClockJointMotionAccumulator::default(),
        RawClockRuntimeExitReason::CompensationFault,
        error,
        |report| report.compensation_faults = report.compensation_faults.saturating_add(1),
    );
    let fault = io.fault_shutdown(FAULT_SHUTDOWN_TIMEOUT);
    return Ok(RawClockCoreExit::Faulted {
        arms: fault.arms,
        report: report.with_shutdown(fault.shutdown).with_telemetry(fault.telemetry),
    });
}
```

Replace every `?` in `run_raw_clock_runtime_core` after `let RawClockRuntimeCore { mut io, ... } = core;` with an explicit faulted-exit branch that calls `io.fault_shutdown(FAULT_SHUTDOWN_TIMEOUT)` while the arms are still owned by the runtime IO. In particular, do not use `?` on telemetry sink, controller, compensation, read, submission, or final disable errors after active ownership transfer.

Use raw-clock loop timing:

```rust
let nominal_period_us = duration_micros_u64(nominal_period).max(1);
let max_dt_us =
    ((nominal_period_us as f64) * cfg.dt_clamp_multiplier)
        .ceil()
        .max(1.0)
        .min(u64::MAX as f64) as u64;
let mut previous_control_frame_host_mono_us: Option<u64> = None;
let mut shaping_state = BilateralOutputShapingState::default();
```

For each selected tick immediately after building `snapshot` from the selected master/slave raw-clock snapshots, compute:

```rust
let scheduler_tick_start_host_mono_us = piper_can::monotonic_micros();
let control_frame_host_mono_us = selection
    .master
    .host_rx_mono_us
    .max(selection.slave.host_rx_mono_us);
let raw_dt_us = previous_control_frame_host_mono_us
    .map(|previous| control_frame_host_mono_us.saturating_sub(previous))
    .unwrap_or(nominal_period_us);
let clamped_dt_us = clamp_control_dt_us(raw_dt_us, max_dt_us);
let control_dt = Duration::from_micros(clamped_dt_us);
let time_jump = raw_dt_us > max_dt_us;
if time_jump {
    let raw_dt = Duration::from_micros(raw_dt_us);
    if let Err(error) = controller.on_time_jump(raw_dt) {
        let report = fault_report_from_timing(
            &timing,
            iterations,
            &joint_motion,
            RawClockRuntimeExitReason::ControllerFault,
            error.to_string(),
            |report| report.controller_faults = report.controller_faults.saturating_add(1),
        );
        let fault = io.fault_shutdown(FAULT_SHUTDOWN_TIMEOUT);
        return Ok(RawClockCoreExit::Faulted {
            arms: fault.arms,
            report: report.with_shutdown(fault.shutdown).with_telemetry(fault.telemetry),
        });
    }
    if let Some(compensator) = compensator.as_deref_mut()
        && let Err(error) = compensator.on_time_jump(raw_dt)
    {
        let report = fault_report_from_timing(
            &timing,
            iterations,
            &joint_motion,
            RawClockRuntimeExitReason::CompensationFault,
            error,
            |report| report.compensation_faults = report.compensation_faults.saturating_add(1),
        );
        let fault = io.fault_shutdown(FAULT_SHUTDOWN_TIMEOUT);
        return Ok(RawClockCoreExit::Faulted {
            arms: fault.arms,
            report: report.with_shutdown(fault.shutdown).with_telemetry(fault.telemetry),
        });
    }
}
```

Build compensation and frame:

```rust
let compensation = match compensator.as_deref_mut() {
    Some(compensator) => match compensator.compute(&snapshot, control_dt) {
        Ok(compensation) => Some(compensation),
        Err(error) => {
            let report = fault_report_from_timing(
                &timing,
                iterations,
                &joint_motion,
                RawClockRuntimeExitReason::CompensationFault,
                error,
                |report| {
                    report.compensation_faults =
                        report.compensation_faults.saturating_add(1)
                },
            );
            let fault = io.fault_shutdown(FAULT_SHUTDOWN_TIMEOUT);
            return Ok(RawClockCoreExit::Faulted {
                arms: fault.arms,
                report: report.with_shutdown(fault.shutdown).with_telemetry(fault.telemetry),
            });
        },
    },
    None => None,
};
let frame = BilateralControlFrame { snapshot, compensation };
```

Call `controller.tick_with_compensation` with an explicit fault branch:

```rust
let mut command = match controller.tick_with_compensation(&frame, control_dt) {
    Ok(command) => command,
    Err(error) => {
        let report = fault_report_from_timing(
            &timing,
            iterations,
            &joint_motion,
            RawClockRuntimeExitReason::ControllerFault,
            error.to_string(),
            |report| report.controller_faults = report.controller_faults.saturating_add(1),
        );
        let fault = io.fault_shutdown(FAULT_SHUTDOWN_TIMEOUT);
        return Ok(RawClockCoreExit::Faulted {
            arms: fault.arms,
            report: report.with_shutdown(fault.shutdown).with_telemetry(fault.telemetry),
        });
    },
};
```

After `controller.tick_with_compensation`, clone telemetry data only when needed:

```rust
let collect_telemetry = cfg.telemetry_sink.is_some();
let controller_command = collect_telemetry.then(|| command.clone());
if let Some(shaping_cfg) = &cfg.output_shaping {
    apply_output_shaping(
        shaping_cfg,
        &frame.snapshot,
        control_dt,
        &mut shaping_state,
        &mut command,
    );
}
let shaped_command = collect_telemetry.then(|| command.clone());
let final_torques = assemble_final_torques(&command, frame.compensation);
```

Record joint motion using the shaped command. In Task 3, before telemetry receipts exist, keep the existing raw-clock submission error handling but pass `&final_torques` into `io.submit_command`. After that submission succeeds, update the previous-frame timestamp:

```rust
previous_control_frame_host_mono_us = Some(control_frame_host_mono_us);
```

This update is required in the no-telemetry path so the Task 3 time-jump tests can observe the second control frame.

- [ ] **Step 8: Run compensation tests to GREEN**

Run:

```bash
cargo test -p piper-client raw_clock_compensation -- --test-threads=1 --nocapture
cargo test -p piper-client raw_clock_compensator_receives_aligned_selected_snapshot -- --test-threads=1 --nocapture
cargo test -p piper-client raw_clock_controller_tick_failure_faults_report -- --test-threads=1 --nocapture
cargo test -p piper-client raw_clock_core_invalid_config_faults_instead_of_outer_error -- --test-threads=1 --nocapture
cargo test -p piper-client run_raw_clock_runtime_core -- --test-threads=1 --nocapture
```

Expected: PASS. Existing no-compensator tests must still pass with `output_shaping: None`.

- [ ] **Step 9: Commit**

```bash
git add crates/piper-client/src/dual_arm_raw_clock.rs
git commit -m "Add raw-clock compensation runtime path"
```

## Task 4: Add Raw-Clock Tx-Finished Telemetry Sink Support

**Files:**
- Modify: `crates/piper-client/src/dual_arm_raw_clock.rs`

- [ ] **Step 1: Write failing telemetry sink test**

Add inside raw-clock tests:

```rust
#[derive(Default)]
struct RecordingRawClockTelemetrySink {
    rows: Mutex<Vec<BilateralLoopTelemetry>>,
}

impl RecordingRawClockTelemetrySink {
    fn rows(&self) -> Vec<BilateralLoopTelemetry> {
        self.rows.lock().expect("rows lock").clone()
    }
}

impl BilateralLoopTelemetrySink for RecordingRawClockTelemetrySink {
    fn on_tick(
        &self,
        telemetry: &BilateralLoopTelemetry,
    ) -> std::result::Result<(), BilateralTelemetrySinkError> {
        self.rows.lock().expect("rows lock").push(telemetry.clone());
        Ok(())
    }
}

#[test]
fn raw_clock_telemetry_sink_receives_tx_finished_and_final_torques() {
    let sink = Arc::new(RecordingRawClockTelemetrySink::default());
    let io = FakeRuntimeIo::new()
        .with_reads(ready_reads_for_iterations(1))
        .with_gripper_states(
            crate::observer::GripperState {
                position: 0.25,
                effort: 0.10,
                enabled: true,
                hardware_timestamp_us: 209_000,
                host_rx_mono_us: 210_000,
            },
            crate::observer::GripperState {
                position: 0.20,
                effort: 0.08,
                enabled: true,
                hardware_timestamp_us: 209_500,
                host_rx_mono_us: 210_500,
            },
        )
        .with_submit_receipts([RawClockSubmitReceipt {
            master_tx_finished_host_mono_us: Some(123_000),
            slave_tx_finished_host_mono_us: Some(122_000),
            master_t_ref_nm: Some([1.0; 6]),
            slave_t_ref_nm: Some([2.0; 6]),
        }]);

    let (_state, report) = run_fake_runtime_with_controller_and_config(
        io,
        ready_timing_for_tests(),
        1,
        RawClockRuntimeThresholds::for_tests(),
        TestCommandController,
        ExperimentalRawClockRunConfig {
            telemetry_sink: Some(sink.clone()),
            output_shaping: Some(BilateralOutputShapingConfig::default()),
            ..ExperimentalRawClockRunConfig::default()
        },
    );

    assert_eq!(report.exit_reason, Some(RawClockRuntimeExitReason::MaxIterations));
    let rows = sink.rows();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].master_tx_finished_host_mono_us, Some(123_000));
    assert_eq!(rows[0].slave_tx_finished_host_mono_us, Some(122_000));
    assert_eq!(rows[0].master_t_ref_nm, Some([1.0; 6]));
    assert_eq!(rows[0].slave_t_ref_nm, Some([2.0; 6]));
    assert_eq!(rows[0].final_torques.master.as_array(), &[NewtonMeter(0.0); 6]);
    assert_eq!(rows[0].final_torques.slave.as_array(), &[NewtonMeter(0.0); 6]);
    assert!(!rows[0].gripper.mirror_enabled);
    assert!(rows[0].gripper.master_available);
    assert!(rows[0].gripper.slave_available);
    assert_eq!(rows[0].gripper.master_host_rx_mono_us, 210_000);
    assert_eq!(rows[0].gripper.slave_host_rx_mono_us, 210_500);
    assert_eq!(rows[0].gripper.master_position, 0.25);
    assert_eq!(rows[0].gripper.slave_position, 0.20);
}
```

Also add combined compensation + telemetry coverage:

```rust
#[test]
fn raw_clock_telemetry_sink_receives_compensation_and_compensated_final_torques() {
    let sink = Arc::new(RecordingRawClockTelemetrySink::default());
    let compensation = BilateralDynamicsCompensation {
        master_model_torque: JointArray::splat(NewtonMeter(0.7)),
        slave_model_torque: JointArray::splat(NewtonMeter(0.9)),
        master_external_torque_est: JointArray::splat(NewtonMeter(0.3)),
        slave_external_torque_est: JointArray::splat(NewtonMeter(0.4)),
    };
    let compensator = RecordingCompensator {
        compensation,
        ..RecordingCompensator::default()
    };

    let (_state, report) = run_fake_runtime_with_controller_and_compensation(
        FakeRuntimeIo::new()
            .with_reads(ready_reads_for_iterations(1))
            .with_submit_receipts([RawClockSubmitReceipt {
                master_tx_finished_host_mono_us: Some(123_000),
                slave_tx_finished_host_mono_us: Some(122_000),
                master_t_ref_nm: Some([0.7; 6]),
                slave_t_ref_nm: Some([0.9; 6]),
            }]),
        ready_timing_for_tests(),
        1,
        RawClockRuntimeThresholds::for_tests(),
        TestCommandController,
        compensator,
        ExperimentalRawClockRunConfig {
            telemetry_sink: Some(sink.clone()),
            output_shaping: Some(BilateralOutputShapingConfig::default()),
            ..ExperimentalRawClockRunConfig::default()
        },
    );

    assert_eq!(report.exit_reason, Some(RawClockRuntimeExitReason::MaxIterations));
    let rows = sink.rows();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].control_frame.compensation, Some(compensation));
    assert_eq!(rows[0].compensation, Some(compensation));
    assert_eq!(rows[0].final_torques.master, JointArray::splat(NewtonMeter(0.7)));
    assert_eq!(rows[0].final_torques.slave, JointArray::splat(NewtonMeter(0.9)));
    assert_eq!(rows[0].master_tx_finished_host_mono_us, Some(123_000));
    assert_eq!(rows[0].slave_tx_finished_host_mono_us, Some(122_000));
}
```

Also add:

```rust
struct FailingRawClockTelemetrySink;

impl BilateralLoopTelemetrySink for FailingRawClockTelemetrySink {
    fn on_tick(
        &self,
        _telemetry: &BilateralLoopTelemetry,
    ) -> std::result::Result<(), BilateralTelemetrySinkError> {
        Err(BilateralTelemetrySinkError {
            message: "sink failed".to_string(),
        })
    }
}

#[test]
fn raw_clock_telemetry_sink_failure_faults_report() {
    let sink = Arc::new(FailingRawClockTelemetrySink);
    let (_state, report) = run_fake_runtime_with_controller_and_config(
        FakeRuntimeIo::new().with_reads(ready_reads_for_iterations(1)),
        ready_timing_for_tests(),
        1,
        RawClockRuntimeThresholds::for_tests(),
        TestCommandController,
        ExperimentalRawClockRunConfig {
            telemetry_sink: Some(sink),
            ..ExperimentalRawClockRunConfig::default()
        },
    );

    assert_eq!(report.exit_reason, Some(RawClockRuntimeExitReason::TelemetrySinkFault));
    assert_eq!(report.telemetry_sink_faults, 1);
}
```

Also add a sink that enforces the SVS tx-finished contract:

```rust
struct RequiresTxFinishedTelemetrySink;

impl BilateralLoopTelemetrySink for RequiresTxFinishedTelemetrySink {
    fn on_tick(
        &self,
        telemetry: &BilateralLoopTelemetry,
    ) -> std::result::Result<(), BilateralTelemetrySinkError> {
        if telemetry.master_tx_finished_host_mono_us.filter(|value| *value > 0).is_none()
            || telemetry.slave_tx_finished_host_mono_us.filter(|value| *value > 0).is_none()
        {
            return Err(BilateralTelemetrySinkError {
                message: "missing nonzero tx-finished timestamp".to_string(),
            });
        }
        Ok(())
    }
}

#[test]
fn raw_clock_missing_tx_finished_timestamps_fault_through_sink() {
    for receipt in [
        RawClockSubmitReceipt {
            master_tx_finished_host_mono_us: None,
            slave_tx_finished_host_mono_us: Some(122_000),
            master_t_ref_nm: Some([1.0; 6]),
            slave_t_ref_nm: Some([2.0; 6]),
        },
        RawClockSubmitReceipt {
            master_tx_finished_host_mono_us: Some(0),
            slave_tx_finished_host_mono_us: Some(122_000),
            master_t_ref_nm: Some([1.0; 6]),
            slave_t_ref_nm: Some([2.0; 6]),
        },
        RawClockSubmitReceipt {
            master_tx_finished_host_mono_us: Some(123_000),
            slave_tx_finished_host_mono_us: Some(0),
            master_t_ref_nm: Some([1.0; 6]),
            slave_t_ref_nm: Some([2.0; 6]),
        },
    ] {
        let sink = Arc::new(RequiresTxFinishedTelemetrySink);
        let (_state, report) = run_fake_runtime_with_controller_and_config(
            FakeRuntimeIo::new()
                .with_reads(ready_reads_for_iterations(1))
                .with_submit_receipts([receipt]),
            ready_timing_for_tests(),
            1,
            RawClockRuntimeThresholds::for_tests(),
            TestCommandController,
            ExperimentalRawClockRunConfig {
                telemetry_sink: Some(sink),
                ..ExperimentalRawClockRunConfig::default()
            },
        );

        assert_eq!(report.exit_reason, Some(RawClockRuntimeExitReason::TelemetrySinkFault));
        assert_eq!(report.telemetry_sink_faults, 1);
    }
}
```

- [ ] **Step 2: Run telemetry tests to verify RED**

Run:

```bash
cargo test -p piper-client raw_clock_telemetry -- --test-threads=1 --nocapture
```

Expected: FAIL because receipts and telemetry emission are absent.

- [ ] **Step 3: Add submit receipt type and trait method**

In `dual_arm_raw_clock.rs`, add:

```rust
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct RawClockSubmitReceipt {
    pub master_tx_finished_host_mono_us: Option<u64>,
    pub slave_tx_finished_host_mono_us: Option<u64>,
    pub master_t_ref_nm: Option<[f64; 6]>,
    pub slave_t_ref_nm: Option<[f64; 6]>,
}
```

Extend `RawClockRuntimeIo`:

```rust
fn gripper_states(&self) -> (crate::observer::GripperState, crate::observer::GripperState) {
    (crate::observer::GripperState::default(), crate::observer::GripperState::default())
}

fn submit_command_with_receipt(
    &mut self,
    command: &BilateralCommand,
    final_torques: &BilateralFinalTorques,
    timeout: Duration,
) -> Result<RawClockSubmitReceipt, RawClockRuntimeError> {
    self.submit_command(command, final_torques, timeout)?;
    Ok(RawClockSubmitReceipt::default())
}
```

Override `gripper_states` in `RealRawClockRuntimeIo`:

```rust
fn gripper_states(&self) -> (crate::observer::GripperState, crate::observer::GripperState) {
    (
        self.master.observer().gripper_state(),
        self.slave.observer().gripper_state(),
    )
}
```

In `RealRawClockRuntimeIo`, override it to use `command_torques_confirmed_finished` on slave first, then master:

```rust
fn submit_command_with_receipt(
    &mut self,
    command: &BilateralCommand,
    final_torques: &BilateralFinalTorques,
    timeout: Duration,
) -> Result<RawClockSubmitReceipt, RawClockRuntimeError> {
    let slave = self.slave.command_torques_confirmed_finished(
        &command.slave_position,
        &command.slave_velocity,
        &command.slave_kp,
        &command.slave_kd,
        &final_torques.slave,
        timeout,
    ).map_err(|source| RawClockRuntimeError::SubmissionFault {
        side: RawClockSide::Slave.as_str(),
        peer_command_may_have_applied: false,
        source,
    })?;

    let master = self.master.command_torques_confirmed_finished(
        &command.master_position,
        &command.master_velocity,
        &command.master_kp,
        &command.master_kd,
        &final_torques.master,
        timeout,
    ).map_err(|source| RawClockRuntimeError::SubmissionFault {
        side: RawClockSide::Master.as_str(),
        peer_command_may_have_applied: true,
        source,
    })?;

    Ok(RawClockSubmitReceipt {
        master_tx_finished_host_mono_us: Some(master.tx_finished.host_finished_mono_us),
        slave_tx_finished_host_mono_us: Some(slave.tx_finished.host_finished_mono_us),
        master_t_ref_nm: Some(master.mit_t_ref_nm),
        slave_t_ref_nm: Some(slave.mit_t_ref_nm),
    })
}
```

The no-telemetry path still uses `submit_command`, but that method now receives `final_torques`; compensation is applied regardless of telemetry.

- [ ] **Step 4: Emit telemetry after submission**

In the raw-clock loop, before submission:

```rust
let timing_telemetry = BilateralLoopTimingTelemetry {
    scheduler_tick_start_host_mono_us,
    control_frame_host_mono_us,
    previous_control_frame_host_mono_us,
    raw_dt_us,
    clamped_dt_us,
    nominal_period_us,
    submission_deadline_mono_us: scheduler_tick_start_host_mono_us
        .saturating_add(nominal_period_us),
    deadline_missed: raw_dt_us > max_dt_us,
};
let (master_gripper, slave_gripper) = io.gripper_states();
let gripper = build_gripper_telemetry(
    &cfg.gripper,
    false,
    master_gripper,
    slave_gripper,
    control_frame_host_mono_us,
);
```

Do not use `cfg.gripper.enabled` to decide whether telemetry samples are populated. It controls command mirroring only; `cfg.gripper.max_feedback_age` still controls freshness for `SvsGripperStepV1`. SVS collector rejects effective mirroring before enable, while this telemetry path remains active when a telemetry sink is configured.

Submit:

```rust
let receipt_result = if collect_telemetry {
    io.submit_command_with_receipt(&command, &final_torques, cfg.command_timeout)
} else {
    io.submit_command(&command, &final_torques, cfg.command_timeout)
        .map(|_| RawClockSubmitReceipt::default())
};

let receipt = match receipt_result {
    Ok(receipt) => receipt,
    Err(error) => {
        let report = fault_report_from_timing(
            &timing,
            iterations,
            &joint_motion,
            RawClockRuntimeExitReason::SubmissionFault,
            error.to_string(),
            |report| {
                report.submission_faults = report.submission_faults.saturating_add(1);
                if let RawClockRuntimeError::SubmissionFault {
                    side,
                    peer_command_may_have_applied,
                    ..
                } = &error
                {
                    report.last_submission_failed_side = RawClockSide::from_str(side);
                    report.peer_command_may_have_applied = *peer_command_may_have_applied;
                }
            },
        );
        let fault = io.fault_shutdown(FAULT_SHUTDOWN_TIMEOUT);
        return Ok(RawClockCoreExit::Faulted {
            arms: fault.arms,
            report: report.with_shutdown(fault.shutdown).with_telemetry(fault.telemetry),
        });
    },
};

previous_control_frame_host_mono_us = Some(control_frame_host_mono_us);
```

This update must happen after every successful submission, including no-telemetry runs, so raw-clock time-jump detection works without a telemetry sink.

If `cfg.telemetry_sink` is `Some`, bind the sink and build telemetry:

```rust
if let Some(sink) = cfg.telemetry_sink.as_ref() {
    let mut telemetry = BilateralLoopTelemetry {
        control_frame: frame,
        controller_command: controller_command.expect("telemetry command clone exists"),
        shaped_command: shaped_command.expect("telemetry shaped command clone exists"),
        compensation: frame.compensation,
        gripper,
        final_torques,
        master_t_ref_nm: receipt.master_t_ref_nm,
        slave_t_ref_nm: receipt.slave_t_ref_nm,
        master_tx_finished_host_mono_us: receipt.master_tx_finished_host_mono_us,
        slave_tx_finished_host_mono_us: receipt.slave_tx_finished_host_mono_us,
        timing: timing_telemetry,
    };
    telemetry.timing.deadline_missed |= deadline_missed_after_submission(
        telemetry.timing.submission_deadline_mono_us,
        telemetry.master_tx_finished_host_mono_us,
        telemetry.slave_tx_finished_host_mono_us,
        piper_can::monotonic_micros(),
    );
    if let Err(error) = sink.on_tick(&telemetry) {
        let mut report = fault_report_from_timing(
            &timing,
            iterations,
            &joint_motion,
            RawClockRuntimeExitReason::TelemetrySinkFault,
            error.message,
            |report| report.telemetry_sink_faults = report.telemetry_sink_faults.saturating_add(1),
        );
        let fault = io.fault_shutdown(FAULT_SHUTDOWN_TIMEOUT);
        return Ok(RawClockCoreExit::Faulted {
            arms: fault.arms,
            report: report.with_shutdown(fault.shutdown).with_telemetry(fault.telemetry),
        });
    }
}
```

Do not update `previous_control_frame_host_mono_us` inside the telemetry-only block; it was already updated after successful submission.

- [ ] **Step 5: Update fake IO helpers**

Extend `FakeRuntimeIo` with:

```rust
submit_receipts: VecDeque<RawClockSubmitReceipt>,
master_gripper: crate::observer::GripperState,
slave_gripper: crate::observer::GripperState,
```

Add:

```rust
fn with_submit_receipts(
    mut self,
    receipts: impl IntoIterator<Item = RawClockSubmitReceipt>,
) -> Self {
    self.submit_receipts = receipts.into_iter().collect();
    self
}

fn with_gripper_states(
    mut self,
    master: crate::observer::GripperState,
    slave: crate::observer::GripperState,
) -> Self {
    self.master_gripper = master;
    self.slave_gripper = slave;
    self
}
```

Implement `gripper_states` for `FakeRuntimeIo` by returning `(self.master_gripper, self.slave_gripper)`.

Implement `submit_command_with_receipt` by calling `submit_command` and returning the next provided receipt, or a default nonzero receipt for tests:

```rust
Ok(self.submit_receipts.pop_front().unwrap_or(RawClockSubmitReceipt {
    master_tx_finished_host_mono_us: Some(10_000),
    slave_tx_finished_host_mono_us: Some(9_900),
    master_t_ref_nm: Some([0.0; 6]),
    slave_t_ref_nm: Some([0.0; 6]),
}))
```

- [ ] **Step 6: Run telemetry tests to GREEN**

Run:

```bash
cargo test -p piper-client raw_clock_telemetry -- --test-threads=1 --nocapture
cargo test -p piper-client dual_arm_raw_clock -- --test-threads=1 --nocapture
```

Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/piper-client/src/dual_arm_raw_clock.rs
git commit -m "Emit raw-clock bilateral loop telemetry"
```

## Task 5: Add SVS Raw-Clock Settings And CLI/Profile Precedence

**Files:**
- Create: `addons/piper-svs-collect/src/raw_clock.rs`
- Modify: `addons/piper-svs-collect/src/lib.rs`
- Modify: `addons/piper-svs-collect/src/args.rs`
- Modify: `addons/piper-svs-collect/src/profile.rs`
- Modify: `addons/piper-svs-collect/src/collector.rs`
- Test: `addons/piper-svs-collect/tests/dependency_boundaries.rs`

- [ ] **Step 1: Write failing args/profile tests**

In `addons/piper-svs-collect/src/args.rs`, extend `parses_required_svs_collect_args` with raw-clock assertions by adding flags:

```rust
"--experimental-calibrated-raw",
"--raw-clock-warmup-secs",
"12",
"--raw-clock-residual-p95-us",
"2100",
"--raw-clock-residual-max-us",
"3200",
"--raw-clock-drift-abs-ppm",
"750",
"--raw-clock-residual-max-consecutive-failures",
"4",
"--raw-clock-sample-gap-max-ms",
"60",
"--raw-clock-last-sample-age-ms",
"25",
"--raw-clock-selected-sample-age-ms",
"55",
"--raw-clock-inter-arm-skew-max-us",
"22000",
"--raw-clock-state-skew-max-us",
"11000",
"--raw-clock-alignment-lag-us",
"6000",
"--raw-clock-alignment-search-window-us",
"26000",
"--raw-clock-alignment-buffer-miss-consecutive-failures",
"5",
```

And assert:

```rust
assert!(args.experimental_calibrated_raw);
assert_eq!(args.raw_clock_warmup_secs, Some(12));
assert_eq!(args.raw_clock_residual_p95_us, Some(2100));
assert_eq!(args.raw_clock_residual_max_us, Some(3200));
assert_eq!(args.raw_clock_drift_abs_ppm, Some(750.0));
assert_eq!(args.raw_clock_residual_max_consecutive_failures, Some(4));
assert_eq!(args.raw_clock_sample_gap_max_ms, Some(60));
assert_eq!(args.raw_clock_last_sample_age_ms, Some(25));
assert_eq!(args.raw_clock_selected_sample_age_ms, Some(55));
assert_eq!(args.raw_clock_inter_arm_skew_max_us, Some(22_000));
assert_eq!(args.raw_clock_state_skew_max_us, Some(11_000));
assert_eq!(args.raw_clock_alignment_lag_us, Some(6_000));
assert_eq!(args.raw_clock_alignment_search_window_us, Some(26_000));
assert_eq!(args.raw_clock_alignment_buffer_miss_consecutive_failures, Some(5));
```

In `addons/piper-svs-collect/src/profile.rs`, add:

```rust
#[test]
fn raw_clock_profile_defaults_match_lab_settings() {
    let profile = EffectiveProfile::default_for_tests();

    assert_eq!(profile.raw_clock.warmup_secs, 10);
    assert_eq!(profile.raw_clock.residual_p95_us, 2000);
    assert_eq!(profile.raw_clock.residual_max_us, 3000);
    assert_eq!(profile.raw_clock.sample_gap_max_ms, 50);
    assert_eq!(profile.raw_clock.inter_arm_skew_max_us, 20_000);
    assert_eq!(profile.raw_clock.state_skew_max_us, 10_000);
    assert_eq!(profile.raw_clock.alignment_lag_us, 5_000);
    assert_eq!(profile.raw_clock.alignment_search_window_us, 25_000);
}
```

In `addons/piper-svs-collect/tests/dependency_boundaries.rs`, add:

```rust
#[test]
fn profile_canonical_toml_accepts_raw_clock_table() {
    let mut profile = piper_svs_collect::profile::EffectiveProfile::default_for_tests();
    profile.raw_clock.warmup_secs = 11;
    profile.raw_clock.residual_p95_us = 2_100;
    profile.raw_clock.residual_max_us = 3_200;
    let bytes = profile
        .to_canonical_toml_bytes()
        .expect("profile should serialize");
    let text = String::from_utf8(bytes).expect("canonical profile should be utf8");

    assert!(text.contains("[raw_clock]"));
    assert!(text.contains("warmup_secs = 11"));

    let parsed: piper_svs_collect::profile::EffectiveProfile =
        toml::from_str(&text).expect("profile should parse");
    parsed.validate().expect("profile should validate");
    assert_eq!(parsed.raw_clock.warmup_secs, 11);
}

#[test]
fn profile_partial_raw_clock_table_uses_field_defaults() {
    let text = r#"
[raw_clock]
warmup_secs = 12
residual_p95_us = 2200
"#;

    let parsed: piper_svs_collect::profile::EffectiveProfile =
        piper_svs_collect::profile::EffectiveProfile::from_overlay_toml(text)
            .expect("partial raw_clock overlay should parse");

    parsed.validate().expect("partial profile should validate");
    assert_eq!(parsed.raw_clock.warmup_secs, 12);
    assert_eq!(parsed.raw_clock.residual_p95_us, 2_200);
    assert_eq!(parsed.raw_clock.residual_max_us, 3_000);
    assert_eq!(parsed.raw_clock.alignment_lag_us, 5_000);
    assert_eq!(parsed.raw_clock.alignment_search_window_us, 25_000);
}
```

In `addons/piper-svs-collect/src/raw_clock.rs`, add resolver precedence tests:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{args::Args, profile::EffectiveProfile};
    use std::path::PathBuf;
    use std::time::Duration;

    fn args_for_raw_clock_resolve_tests() -> Args {
        Args {
            master_target: "socketcan:can0".to_string(),
            slave_target: "socketcan:can1".to_string(),
            baud_rate: None,
            model_dir: Some(PathBuf::from("/tmp/model")),
            use_standard_model_path: false,
            use_embedded_model: false,
            task_profile: None,
            output_dir: PathBuf::from("/tmp/out"),
            calibration_file: None,
            save_calibration: None,
            calibration_max_error_rad: None,
            mirror_map: None,
            operator: None,
            task: Some("test".to_string()),
            notes: None,
            raw_can: false,
            disable_gripper_mirror: true,
            max_iterations: None,
            timing_mode: "spin".to_string(),
            yes: true,
            experimental_calibrated_raw: true,
            raw_clock_warmup_secs: None,
            raw_clock_residual_p95_us: None,
            raw_clock_residual_max_us: None,
            raw_clock_drift_abs_ppm: None,
            raw_clock_sample_gap_max_ms: None,
            raw_clock_last_sample_age_ms: None,
            raw_clock_selected_sample_age_ms: None,
            raw_clock_inter_arm_skew_max_us: None,
            raw_clock_state_skew_max_us: None,
            raw_clock_residual_max_consecutive_failures: None,
            raw_clock_alignment_lag_us: None,
            raw_clock_alignment_search_window_us: None,
            raw_clock_alignment_buffer_miss_consecutive_failures: None,
        }
    }

    #[test]
    fn raw_clock_settings_cli_overrides_profile_values() {
        let mut args = args_for_raw_clock_resolve_tests();
        let mut profile = EffectiveProfile::default_for_tests();
        profile.raw_clock.warmup_secs = 11;
        profile.raw_clock.residual_p95_us = 2_100;
        args.raw_clock_warmup_secs = Some(12);
        args.raw_clock_residual_p95_us = Some(2_200);

        let settings = SvsRawClockSettings::resolve(&args, &profile).unwrap();

        assert_eq!(settings.warmup_secs, 12);
        assert_eq!(settings.residual_p95_us, 2_200);
    }

    #[test]
    fn raw_clock_settings_profile_overrides_builtin_defaults() {
        let args = args_for_raw_clock_resolve_tests();
        let mut profile = EffectiveProfile::default_for_tests();
        profile.raw_clock.warmup_secs = 17;
        profile.raw_clock.alignment_lag_us = 7_000;

        let settings = SvsRawClockSettings::resolve(&args, &profile).unwrap();

        assert_eq!(settings.warmup_secs, 17);
        assert_eq!(settings.alignment_lag_us, 7_000);
    }

    #[test]
    fn raw_clock_settings_reject_invalid_resolved_values() {
        let mut args = args_for_raw_clock_resolve_tests();
        let profile = EffectiveProfile::default_for_tests();
        args.raw_clock_residual_p95_us = Some(0);

        let err = SvsRawClockSettings::resolve(&args, &profile).unwrap_err();

        assert!(err.to_string().contains("residual_p95_us"));
    }

    #[test]
    fn raw_clock_settings_conversion_copies_runtime_threshold_fields() {
        let mut args = args_for_raw_clock_resolve_tests();
        args.raw_clock_warmup_secs = Some(12);
        args.raw_clock_residual_p95_us = Some(2_100);
        args.raw_clock_residual_max_us = Some(3_200);
        args.raw_clock_drift_abs_ppm = Some(750.0);
        args.raw_clock_sample_gap_max_ms = Some(60);
        args.raw_clock_last_sample_age_ms = Some(25);
        args.raw_clock_selected_sample_age_ms = Some(55);
        args.raw_clock_inter_arm_skew_max_us = Some(22_000);
        args.raw_clock_state_skew_max_us = Some(11_000);
        args.raw_clock_residual_max_consecutive_failures = Some(4);
        args.raw_clock_alignment_lag_us = Some(6_000);
        args.raw_clock_alignment_search_window_us = Some(26_000);
        args.raw_clock_alignment_buffer_miss_consecutive_failures = Some(5);
        let profile = EffectiveProfile::default_for_tests();

        let settings = SvsRawClockSettings::resolve(&args, &profile).unwrap();
        let cfg = settings.to_experimental_config(100.0, Some(123));
        let read_policy = settings.read_policy();

        assert_eq!(cfg.frequency_hz, 100.0);
        assert_eq!(cfg.max_iterations, Some(123));
        assert_eq!(cfg.estimator_thresholds.warmup_window_us, 12_000_000);
        assert_eq!(cfg.estimator_thresholds.residual_p95_us, 2_100);
        assert_eq!(cfg.estimator_thresholds.residual_max_us, 3_200);
        assert_eq!(cfg.estimator_thresholds.drift_abs_ppm, 750.0);
        assert_eq!(cfg.estimator_thresholds.sample_gap_max_us, 60_000);
        assert_eq!(cfg.estimator_thresholds.last_sample_age_us, 25_000);
        assert_eq!(cfg.thresholds.inter_arm_skew_max_us, 22_000);
        assert_eq!(cfg.thresholds.last_sample_age_us, 25_000);
        assert_eq!(cfg.thresholds.selected_sample_age_us, 55_000);
        assert_eq!(cfg.thresholds.residual_max_consecutive_failures, 4);
        assert_eq!(cfg.thresholds.alignment_lag_us, 6_000);
        assert_eq!(cfg.thresholds.alignment_search_window_us, 26_000);
        assert_eq!(cfg.thresholds.alignment_buffer_miss_consecutive_failures, 5);
        assert_eq!(read_policy.max_state_skew_us, 11_000);
        assert_eq!(read_policy.max_feedback_age, Duration::from_millis(25));
    }
}
```

- [ ] **Step 2: Run tests to verify RED**

Run:

```bash
cargo test --manifest-path addons/piper-svs-collect/Cargo.toml parses_required_svs_collect_args -- --nocapture
cargo test --manifest-path addons/piper-svs-collect/Cargo.toml raw_clock_profile_defaults_match_lab_settings -- --nocapture
cargo test --manifest-path addons/piper-svs-collect/Cargo.toml profile_canonical_toml_accepts_raw_clock_table -- --nocapture
cargo test --manifest-path addons/piper-svs-collect/Cargo.toml profile_partial_raw_clock_table_uses_field_defaults -- --nocapture
cargo test --manifest-path addons/piper-svs-collect/Cargo.toml raw_clock_settings_cli_overrides_profile_values -- --nocapture
cargo test --manifest-path addons/piper-svs-collect/Cargo.toml raw_clock_settings_profile_overrides_builtin_defaults -- --nocapture
cargo test --manifest-path addons/piper-svs-collect/Cargo.toml raw_clock_settings_reject_invalid_resolved_values -- --nocapture
cargo test --manifest-path addons/piper-svs-collect/Cargo.toml raw_clock_settings_conversion_copies_runtime_threshold_fields -- --nocapture
```

Expected: FAIL because fields do not exist.

- [ ] **Step 3: Add raw-clock args**

In `addons/piper-svs-collect/src/args.rs`, add to `Args`:

```rust
#[arg(long)]
pub experimental_calibrated_raw: bool,
#[arg(long)]
pub raw_clock_warmup_secs: Option<u64>,
#[arg(long)]
pub raw_clock_residual_p95_us: Option<u64>,
#[arg(long)]
pub raw_clock_residual_max_us: Option<u64>,
#[arg(long)]
pub raw_clock_drift_abs_ppm: Option<f64>,
#[arg(long)]
pub raw_clock_sample_gap_max_ms: Option<u64>,
#[arg(long)]
pub raw_clock_last_sample_age_ms: Option<u64>,
#[arg(long)]
pub raw_clock_selected_sample_age_ms: Option<u64>,
#[arg(long)]
pub raw_clock_inter_arm_skew_max_us: Option<u64>,
#[arg(long)]
pub raw_clock_state_skew_max_us: Option<u64>,
#[arg(long)]
pub raw_clock_residual_max_consecutive_failures: Option<u32>,
#[arg(long)]
pub raw_clock_alignment_lag_us: Option<u64>,
#[arg(long)]
pub raw_clock_alignment_search_window_us: Option<u64>,
#[arg(long)]
pub raw_clock_alignment_buffer_miss_consecutive_failures: Option<u32>,
```

- [ ] **Step 4: Add `RawClockProfile`**

In `addons/piper-svs-collect/src/profile.rs`, add `pub raw_clock: RawClockProfile` to `EffectiveProfile` with `#[serde(default)]` so existing profile TOML files still parse:

```rust
#[serde(default)]
pub raw_clock: RawClockProfile,
```

Add:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawClockProfile {
    #[serde(default = "default_raw_clock_warmup_secs")]
    pub warmup_secs: u64,
    #[serde(default = "default_raw_clock_residual_p95_us")]
    pub residual_p95_us: u64,
    #[serde(default = "default_raw_clock_residual_max_us")]
    pub residual_max_us: u64,
    #[serde(default = "default_raw_clock_drift_abs_ppm")]
    pub drift_abs_ppm: f64,
    #[serde(default = "default_raw_clock_sample_gap_max_ms")]
    pub sample_gap_max_ms: u64,
    #[serde(default = "default_raw_clock_last_sample_age_ms")]
    pub last_sample_age_ms: u64,
    #[serde(default = "default_raw_clock_selected_sample_age_ms")]
    pub selected_sample_age_ms: u64,
    #[serde(default = "default_raw_clock_inter_arm_skew_max_us")]
    pub inter_arm_skew_max_us: u64,
    #[serde(default = "default_raw_clock_state_skew_max_us")]
    pub state_skew_max_us: u64,
    #[serde(default = "default_raw_clock_residual_max_consecutive_failures")]
    pub residual_max_consecutive_failures: u32,
    #[serde(default = "default_raw_clock_alignment_lag_us")]
    pub alignment_lag_us: u64,
    #[serde(default = "default_raw_clock_alignment_search_window_us")]
    pub alignment_search_window_us: u64,
    #[serde(default = "default_raw_clock_alignment_buffer_miss_consecutive_failures")]
    pub alignment_buffer_miss_consecutive_failures: u32,
}

fn default_raw_clock_warmup_secs() -> u64 {
    DEFAULT_RAW_CLOCK_WARMUP_SECS
}

fn default_raw_clock_residual_p95_us() -> u64 {
    DEFAULT_RAW_CLOCK_RESIDUAL_P95_US
}

fn default_raw_clock_residual_max_us() -> u64 {
    DEFAULT_RAW_CLOCK_RESIDUAL_MAX_US
}

fn default_raw_clock_drift_abs_ppm() -> f64 {
    DEFAULT_RAW_CLOCK_DRIFT_ABS_PPM
}

fn default_raw_clock_sample_gap_max_ms() -> u64 {
    DEFAULT_RAW_CLOCK_SAMPLE_GAP_MAX_MS
}

fn default_raw_clock_last_sample_age_ms() -> u64 {
    DEFAULT_RAW_CLOCK_LAST_SAMPLE_AGE_MS
}

fn default_raw_clock_selected_sample_age_ms() -> u64 {
    DEFAULT_RAW_CLOCK_SELECTED_SAMPLE_AGE_MS
}

fn default_raw_clock_inter_arm_skew_max_us() -> u64 {
    DEFAULT_RAW_CLOCK_INTER_ARM_SKEW_MAX_US
}

fn default_raw_clock_state_skew_max_us() -> u64 {
    DEFAULT_RAW_CLOCK_STATE_SKEW_MAX_US
}

fn default_raw_clock_residual_max_consecutive_failures() -> u32 {
    DEFAULT_RAW_CLOCK_RESIDUAL_MAX_CONSECUTIVE_FAILURES
}

fn default_raw_clock_alignment_lag_us() -> u64 {
    DEFAULT_RAW_CLOCK_ALIGNMENT_LAG_US
}

fn default_raw_clock_alignment_search_window_us() -> u64 {
    DEFAULT_RAW_CLOCK_ALIGNMENT_SEARCH_WINDOW_US
}

fn default_raw_clock_alignment_buffer_miss_consecutive_failures() -> u32 {
    DEFAULT_RAW_CLOCK_ALIGNMENT_BUFFER_MISS_CONSECUTIVE_FAILURES
}

impl Default for RawClockProfile {
    fn default() -> Self {
        Self {
            warmup_secs: DEFAULT_RAW_CLOCK_WARMUP_SECS,
            residual_p95_us: DEFAULT_RAW_CLOCK_RESIDUAL_P95_US,
            residual_max_us: DEFAULT_RAW_CLOCK_RESIDUAL_MAX_US,
            drift_abs_ppm: DEFAULT_RAW_CLOCK_DRIFT_ABS_PPM,
            sample_gap_max_ms: DEFAULT_RAW_CLOCK_SAMPLE_GAP_MAX_MS,
            last_sample_age_ms: DEFAULT_RAW_CLOCK_LAST_SAMPLE_AGE_MS,
            selected_sample_age_ms: DEFAULT_RAW_CLOCK_SELECTED_SAMPLE_AGE_MS,
            inter_arm_skew_max_us: DEFAULT_RAW_CLOCK_INTER_ARM_SKEW_MAX_US,
            state_skew_max_us: DEFAULT_RAW_CLOCK_STATE_SKEW_MAX_US,
            residual_max_consecutive_failures:
                DEFAULT_RAW_CLOCK_RESIDUAL_MAX_CONSECUTIVE_FAILURES,
            alignment_lag_us: DEFAULT_RAW_CLOCK_ALIGNMENT_LAG_US,
            alignment_search_window_us: DEFAULT_RAW_CLOCK_ALIGNMENT_SEARCH_WINDOW_US,
            alignment_buffer_miss_consecutive_failures:
                DEFAULT_RAW_CLOCK_ALIGNMENT_BUFFER_MISS_CONSECUTIVE_FAILURES,
        }
    }
}
```

Place the constants at the top of `profile.rs` or import them from the new `raw_clock.rs`. Prefer defining them in `raw_clock.rs` and importing into `profile.rs`:

```rust
use crate::raw_clock::{
    DEFAULT_RAW_CLOCK_ALIGNMENT_BUFFER_MISS_CONSECUTIVE_FAILURES,
    DEFAULT_RAW_CLOCK_ALIGNMENT_LAG_US,
    DEFAULT_RAW_CLOCK_ALIGNMENT_SEARCH_WINDOW_US,
    DEFAULT_RAW_CLOCK_DRIFT_ABS_PPM,
    DEFAULT_RAW_CLOCK_INTER_ARM_SKEW_MAX_US,
    DEFAULT_RAW_CLOCK_LAST_SAMPLE_AGE_MS,
    DEFAULT_RAW_CLOCK_RESIDUAL_MAX_CONSECUTIVE_FAILURES,
    DEFAULT_RAW_CLOCK_RESIDUAL_MAX_US,
    DEFAULT_RAW_CLOCK_RESIDUAL_P95_US,
    DEFAULT_RAW_CLOCK_SAMPLE_GAP_MAX_MS,
    DEFAULT_RAW_CLOCK_SELECTED_SAMPLE_AGE_MS,
    DEFAULT_RAW_CLOCK_STATE_SKEW_MAX_US,
    DEFAULT_RAW_CLOCK_WARMUP_SECS,
    MAX_RAW_CLOCK_ALIGNMENT_BUFFER_MISS_CONSECUTIVE_FAILURES,
    MAX_RAW_CLOCK_ALIGNMENT_LAG_US,
    MAX_RAW_CLOCK_ALIGNMENT_SEARCH_WINDOW_US,
    MAX_RAW_CLOCK_DRIFT_ABS_PPM,
    MAX_RAW_CLOCK_INTER_ARM_SKEW_MAX_US,
    MAX_RAW_CLOCK_LAST_SAMPLE_AGE_MS,
    MAX_RAW_CLOCK_RESIDUAL_MAX_CONSECUTIVE_FAILURES,
    MAX_RAW_CLOCK_RESIDUAL_MAX_US,
    MAX_RAW_CLOCK_RESIDUAL_P95_US,
    MAX_RAW_CLOCK_SAMPLE_GAP_MAX_MS,
    MAX_RAW_CLOCK_SELECTED_SAMPLE_AGE_MS,
    MAX_RAW_CLOCK_STATE_SKEW_MAX_US,
    MAX_RAW_CLOCK_WARMUP_SECS,
};
```

Call `self.raw_clock.validate()?` from `EffectiveProfile::validate`.

Move the task-profile overlay merge helper out of `collector.rs` into `profile.rs` so the raw-clock profile precedence path is testable and reusable. Add:

```rust
impl EffectiveProfile {
    pub fn from_overlay_toml(overlay_text: &str) -> Result<Self, ProfileError> {
        let default_text = String::from_utf8(Self::default().to_canonical_toml_bytes()?)
            .map_err(|error| ProfileError::Invalid(error.to_string()))?;
        let mut merged: toml::Value =
            toml::from_str(&default_text).map_err(|error| ProfileError::Invalid(error.to_string()))?;
        let overlay: toml::Value =
            toml::from_str(overlay_text).map_err(|error| ProfileError::Invalid(error.to_string()))?;
        merge_toml_overlay(&mut merged, overlay)?;
        let profile: Self = merged
            .try_into()
            .map_err(|error: toml::de::Error| ProfileError::Invalid(error.to_string()))?;
        profile.validate()?;
        Ok(profile)
    }
}

fn merge_toml_overlay(base: &mut toml::Value, overlay: toml::Value) -> Result<(), ProfileError> {
    match (base, overlay) {
        (toml::Value::Table(base_table), toml::Value::Table(overlay_table)) => {
            for (key, value) in overlay_table {
                match base_table.get_mut(&key) {
                    Some(base_value) => merge_toml_overlay(base_value, value)?,
                    None => {
                        base_table.insert(key, value);
                    },
                }
            }
            Ok(())
        },
        (base_slot, overlay_value) => {
            *base_slot = overlay_value;
            Ok(())
        },
    }
}
```

Then update `collector.rs`'s existing private `load_effective_profile_overlay` helper to delegate to this public method:

```rust
fn load_effective_profile_overlay(overlay_text: &str) -> Result<EffectiveProfile> {
    EffectiveProfile::from_overlay_toml(overlay_text).map_err(anyhow::Error::from)
}
```

The direct `toml::from_str::<EffectiveProfile>` path is only for fully canonical profiles. Task profile TOML files are overlays, so partial `[raw_clock]` tables must be tested through `from_overlay_toml`.

Serialize canonical TOML after `[gripper]` and before `[writer]`:

```rust
push_table(&mut out, "raw_clock");
push_u64_line(&mut out, "warmup_secs", self.raw_clock.warmup_secs);
push_u64_line(&mut out, "residual_p95_us", self.raw_clock.residual_p95_us);
push_u64_line(&mut out, "residual_max_us", self.raw_clock.residual_max_us);
push_f64_line(&mut out, "drift_abs_ppm", self.raw_clock.drift_abs_ppm);
push_u64_line(&mut out, "sample_gap_max_ms", self.raw_clock.sample_gap_max_ms);
push_u64_line(&mut out, "last_sample_age_ms", self.raw_clock.last_sample_age_ms);
push_u64_line(&mut out, "selected_sample_age_ms", self.raw_clock.selected_sample_age_ms);
push_u64_line(&mut out, "inter_arm_skew_max_us", self.raw_clock.inter_arm_skew_max_us);
push_u64_line(&mut out, "state_skew_max_us", self.raw_clock.state_skew_max_us);
push_u32_line(
    &mut out,
    "residual_max_consecutive_failures",
    self.raw_clock.residual_max_consecutive_failures,
);
push_u64_line(&mut out, "alignment_lag_us", self.raw_clock.alignment_lag_us);
push_u64_line(
    &mut out,
    "alignment_search_window_us",
    self.raw_clock.alignment_search_window_us,
);
push_u32_line(
    &mut out,
    "alignment_buffer_miss_consecutive_failures",
    self.raw_clock.alignment_buffer_miss_consecutive_failures,
);
```

Validation:

```rust
impl RawClockProfile {
    fn validate(&self) -> Result<(), ProfileError> {
        validate_u64_range("raw_clock.warmup_secs", self.warmup_secs, 1, MAX_RAW_CLOCK_WARMUP_SECS)?;
        validate_u64_range("raw_clock.residual_p95_us", self.residual_p95_us, 1, MAX_RAW_CLOCK_RESIDUAL_P95_US)?;
        validate_u64_range("raw_clock.residual_max_us", self.residual_max_us, 1, MAX_RAW_CLOCK_RESIDUAL_MAX_US)?;
        validate_u64_range("raw_clock.sample_gap_max_ms", self.sample_gap_max_ms, 1, MAX_RAW_CLOCK_SAMPLE_GAP_MAX_MS)?;
        validate_u64_range("raw_clock.last_sample_age_ms", self.last_sample_age_ms, 1, MAX_RAW_CLOCK_LAST_SAMPLE_AGE_MS)?;
        validate_u64_range("raw_clock.selected_sample_age_ms", self.selected_sample_age_ms, 1, MAX_RAW_CLOCK_SELECTED_SAMPLE_AGE_MS)?;
        validate_u64_range("raw_clock.inter_arm_skew_max_us", self.inter_arm_skew_max_us, 1, MAX_RAW_CLOCK_INTER_ARM_SKEW_MAX_US)?;
        validate_u64_range("raw_clock.state_skew_max_us", self.state_skew_max_us, 1, MAX_RAW_CLOCK_STATE_SKEW_MAX_US)?;
        validate_u64_range("raw_clock.alignment_lag_us", self.alignment_lag_us, 1, MAX_RAW_CLOCK_ALIGNMENT_LAG_US)?;
        validate_u64_range("raw_clock.alignment_search_window_us", self.alignment_search_window_us, 1, MAX_RAW_CLOCK_ALIGNMENT_SEARCH_WINDOW_US)?;
        if self.residual_p95_us > self.residual_max_us {
            return invalid("raw_clock.residual_p95_us must be <= raw_clock.residual_max_us");
        }
        validate_positive("raw_clock.drift_abs_ppm", self.drift_abs_ppm)?;
        if self.drift_abs_ppm > MAX_RAW_CLOCK_DRIFT_ABS_PPM {
            return invalid("raw_clock.drift_abs_ppm is above supported maximum");
        }
        validate_u32_range(
            "raw_clock.residual_max_consecutive_failures",
            self.residual_max_consecutive_failures,
            1,
            MAX_RAW_CLOCK_RESIDUAL_MAX_CONSECUTIVE_FAILURES,
        )?;
        validate_u32_range(
            "raw_clock.alignment_buffer_miss_consecutive_failures",
            self.alignment_buffer_miss_consecutive_failures,
            1,
            MAX_RAW_CLOCK_ALIGNMENT_BUFFER_MISS_CONSECUTIVE_FAILURES,
        )?;
        if self.selected_sample_age_ms < self.last_sample_age_ms {
            return invalid("raw_clock.selected_sample_age_ms must be >= raw_clock.last_sample_age_ms");
        }
        if self.alignment_lag_us >= self.selected_sample_age_ms.saturating_mul(1_000) {
            return invalid("raw_clock.alignment_lag_us must be less than selected_sample_age_ms");
        }
        if self.inter_arm_skew_max_us > self.alignment_search_window_us {
            return invalid(
                "raw_clock.inter_arm_skew_max_us must be <= raw_clock.alignment_search_window_us",
            );
        }
        Ok(())
    }
}
```

If `profile.rs` does not already have integer range helpers, add them near `validate_positive`:

```rust
fn validate_u64_range(name: &str, value: u64, min: u64, max: u64) -> Result<(), ProfileError> {
    if !(min..=max).contains(&value) {
        return invalid(format!("{name} must be in {min}..={max}"));
    }
    Ok(())
}

fn validate_u32_range(name: &str, value: u32, min: u32, max: u32) -> Result<(), ProfileError> {
    if !(min..=max).contains(&value) {
        return invalid(format!("{name} must be in {min}..={max}"));
    }
    Ok(())
}
```

- [ ] **Step 5: Create `raw_clock.rs` settings resolver**

Create `addons/piper-svs-collect/src/raw_clock.rs`:

```rust
use std::time::Duration;

use anyhow::{Result, bail};
use piper_client::dual_arm::{
    BilateralLoopConfig, BilateralOutputShapingConfig, GripperTeleopConfig,
    StopAttemptResult,
};
use piper_client::dual_arm_raw_clock::{
    ExperimentalRawClockConfig, ExperimentalRawClockMode, ExperimentalRawClockRunConfig,
    RawClockRuntimeExitReason, RawClockRuntimeReport, RawClockRuntimeThresholds,
};
use piper_client::observer::ControlReadPolicy;
use piper_tools::raw_clock::{RawClockHealth, RawClockThresholds};

use crate::args::Args;
use crate::profile::{EffectiveProfile, RawClockProfile};

pub const DEFAULT_RAW_CLOCK_WARMUP_SECS: u64 = 10;
pub const DEFAULT_RAW_CLOCK_RESIDUAL_P95_US: u64 = 2000;
pub const DEFAULT_RAW_CLOCK_RESIDUAL_MAX_US: u64 = 3000;
pub const DEFAULT_RAW_CLOCK_DRIFT_ABS_PPM: f64 = 500.0;
pub const DEFAULT_RAW_CLOCK_SAMPLE_GAP_MAX_MS: u64 = 50;
pub const DEFAULT_RAW_CLOCK_LAST_SAMPLE_AGE_MS: u64 = 20;
pub const DEFAULT_RAW_CLOCK_SELECTED_SAMPLE_AGE_MS: u64 = 50;
pub const DEFAULT_RAW_CLOCK_STATE_SKEW_MAX_US: u64 = 10_000;
pub const DEFAULT_RAW_CLOCK_INTER_ARM_SKEW_MAX_US: u64 = 20_000;
pub const DEFAULT_RAW_CLOCK_RESIDUAL_MAX_CONSECUTIVE_FAILURES: u32 = 3;
pub const DEFAULT_RAW_CLOCK_ALIGNMENT_LAG_US: u64 = 5_000;
pub const DEFAULT_RAW_CLOCK_ALIGNMENT_SEARCH_WINDOW_US: u64 = 25_000;
pub const DEFAULT_RAW_CLOCK_ALIGNMENT_BUFFER_MISS_CONSECUTIVE_FAILURES: u32 = 3;

pub const MAX_RAW_CLOCK_WARMUP_SECS: u64 = 3600;
pub const MAX_RAW_CLOCK_RESIDUAL_P95_US: u64 = 100_000;
pub const MAX_RAW_CLOCK_RESIDUAL_MAX_US: u64 = 250_000;
pub const MAX_RAW_CLOCK_DRIFT_ABS_PPM: f64 = 1000.0;
pub const MAX_RAW_CLOCK_SAMPLE_GAP_MAX_MS: u64 = 1000;
pub const MAX_RAW_CLOCK_LAST_SAMPLE_AGE_MS: u64 = 1000;
pub const MAX_RAW_CLOCK_SELECTED_SAMPLE_AGE_MS: u64 = 1000;
pub const MAX_RAW_CLOCK_STATE_SKEW_MAX_US: u64 = 100_000;
pub const MAX_RAW_CLOCK_INTER_ARM_SKEW_MAX_US: u64 = 100_000;
pub const MAX_RAW_CLOCK_RESIDUAL_MAX_CONSECUTIVE_FAILURES: u32 = 100;
pub const MAX_RAW_CLOCK_ALIGNMENT_LAG_US: u64 = 100_000;
pub const MAX_RAW_CLOCK_ALIGNMENT_SEARCH_WINDOW_US: u64 = 100_000;
pub const MAX_RAW_CLOCK_ALIGNMENT_BUFFER_MISS_CONSECUTIVE_FAILURES: u32 = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SvsRuntimeKind {
    StrictRealtime,
    CalibratedRawClock,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SvsRawClockSettings {
    pub warmup_secs: u64,
    pub residual_p95_us: u64,
    pub residual_max_us: u64,
    pub drift_abs_ppm: f64,
    pub sample_gap_max_ms: u64,
    pub last_sample_age_ms: u64,
    pub selected_sample_age_ms: u64,
    pub inter_arm_skew_max_us: u64,
    pub state_skew_max_us: u64,
    pub residual_max_consecutive_failures: u32,
    pub alignment_lag_us: u64,
    pub alignment_search_window_us: u64,
    pub alignment_buffer_miss_consecutive_failures: u32,
}

impl SvsRuntimeKind {
    pub fn from_args(args: &Args) -> Self {
        if args.experimental_calibrated_raw {
            Self::CalibratedRawClock
        } else {
            Self::StrictRealtime
        }
    }
}

impl SvsRawClockSettings {
    pub fn resolve(args: &Args, profile: &EffectiveProfile) -> Result<Self> {
        let raw = &profile.raw_clock;
        let settings = Self {
            warmup_secs: args.raw_clock_warmup_secs.unwrap_or(raw.warmup_secs),
            residual_p95_us: args.raw_clock_residual_p95_us.unwrap_or(raw.residual_p95_us),
            residual_max_us: args.raw_clock_residual_max_us.unwrap_or(raw.residual_max_us),
            drift_abs_ppm: args.raw_clock_drift_abs_ppm.unwrap_or(raw.drift_abs_ppm),
            sample_gap_max_ms: args.raw_clock_sample_gap_max_ms.unwrap_or(raw.sample_gap_max_ms),
            last_sample_age_ms: args
                .raw_clock_last_sample_age_ms
                .unwrap_or(raw.last_sample_age_ms),
            selected_sample_age_ms: args
                .raw_clock_selected_sample_age_ms
                .unwrap_or(raw.selected_sample_age_ms),
            inter_arm_skew_max_us: args
                .raw_clock_inter_arm_skew_max_us
                .unwrap_or(raw.inter_arm_skew_max_us),
            state_skew_max_us: args
                .raw_clock_state_skew_max_us
                .unwrap_or(raw.state_skew_max_us),
            residual_max_consecutive_failures: args
                .raw_clock_residual_max_consecutive_failures
                .unwrap_or(raw.residual_max_consecutive_failures),
            alignment_lag_us: args.raw_clock_alignment_lag_us.unwrap_or(raw.alignment_lag_us),
            alignment_search_window_us: args
                .raw_clock_alignment_search_window_us
                .unwrap_or(raw.alignment_search_window_us),
            alignment_buffer_miss_consecutive_failures: args
                .raw_clock_alignment_buffer_miss_consecutive_failures
                .unwrap_or(raw.alignment_buffer_miss_consecutive_failures),
        };
        settings.validate()?;
        Ok(settings)
    }

    pub fn validate(&self) -> Result<()> {
        validate_u64_range("warmup_secs", self.warmup_secs, 1, MAX_RAW_CLOCK_WARMUP_SECS)?;
        validate_u64_range(
            "residual_p95_us",
            self.residual_p95_us,
            1,
            MAX_RAW_CLOCK_RESIDUAL_P95_US,
        )?;
        validate_u64_range(
            "residual_max_us",
            self.residual_max_us,
            1,
            MAX_RAW_CLOCK_RESIDUAL_MAX_US,
        )?;
        if self.residual_p95_us > self.residual_max_us {
            bail!("raw-clock residual_p95_us must be <= residual_max_us");
        }
        if !self.drift_abs_ppm.is_finite()
            || self.drift_abs_ppm <= 0.0
            || self.drift_abs_ppm > MAX_RAW_CLOCK_DRIFT_ABS_PPM
        {
            bail!("raw-clock drift_abs_ppm must be finite and in 0..={MAX_RAW_CLOCK_DRIFT_ABS_PPM}");
        }
        validate_u64_range(
            "sample_gap_max_ms",
            self.sample_gap_max_ms,
            1,
            MAX_RAW_CLOCK_SAMPLE_GAP_MAX_MS,
        )?;
        validate_u64_range(
            "last_sample_age_ms",
            self.last_sample_age_ms,
            1,
            MAX_RAW_CLOCK_LAST_SAMPLE_AGE_MS,
        )?;
        validate_u64_range(
            "selected_sample_age_ms",
            self.selected_sample_age_ms,
            1,
            MAX_RAW_CLOCK_SELECTED_SAMPLE_AGE_MS,
        )?;
        validate_u64_range(
            "inter_arm_skew_max_us",
            self.inter_arm_skew_max_us,
            1,
            MAX_RAW_CLOCK_INTER_ARM_SKEW_MAX_US,
        )?;
        validate_u64_range(
            "state_skew_max_us",
            self.state_skew_max_us,
            1,
            MAX_RAW_CLOCK_STATE_SKEW_MAX_US,
        )?;
        validate_u32_range(
            "residual_max_consecutive_failures",
            self.residual_max_consecutive_failures,
            1,
            MAX_RAW_CLOCK_RESIDUAL_MAX_CONSECUTIVE_FAILURES,
        )?;
        validate_u64_range(
            "alignment_lag_us",
            self.alignment_lag_us,
            1,
            MAX_RAW_CLOCK_ALIGNMENT_LAG_US,
        )?;
        validate_u64_range(
            "alignment_search_window_us",
            self.alignment_search_window_us,
            1,
            MAX_RAW_CLOCK_ALIGNMENT_SEARCH_WINDOW_US,
        )?;
        validate_u32_range(
            "alignment_buffer_miss_consecutive_failures",
            self.alignment_buffer_miss_consecutive_failures,
            1,
            MAX_RAW_CLOCK_ALIGNMENT_BUFFER_MISS_CONSECUTIVE_FAILURES,
        )?;
        if self.selected_sample_age_ms < self.last_sample_age_ms {
            bail!("raw-clock selected_sample_age_ms must be >= last_sample_age_ms");
        }
        if self.alignment_lag_us >= self.selected_sample_age_ms.saturating_mul(1_000) {
            bail!("raw-clock alignment_lag_us must be less than selected_sample_age_ms");
        }
        if self.inter_arm_skew_max_us > self.alignment_search_window_us {
            bail!("raw-clock inter_arm_skew_max_us must be <= alignment_search_window_us");
        }
        Ok(())
    }

    pub fn to_experimental_config(
        &self,
        frequency_hz: f64,
        max_iterations: Option<usize>,
    ) -> ExperimentalRawClockConfig {
        ExperimentalRawClockConfig {
            mode: ExperimentalRawClockMode::Bilateral,
            frequency_hz,
            max_iterations,
            estimator_thresholds: RawClockThresholds {
                warmup_samples: raw_clock_warmup_sample_threshold(self),
                warmup_window_us: self.warmup_secs * 1_000_000,
                residual_p95_us: self.residual_p95_us,
                residual_max_us: self.residual_max_us,
                drift_abs_ppm: self.drift_abs_ppm,
                sample_gap_max_us: self.sample_gap_max_ms * 1_000,
                last_sample_age_us: self.last_sample_age_ms * 1_000,
            },
            thresholds: RawClockRuntimeThresholds {
                inter_arm_skew_max_us: self.inter_arm_skew_max_us,
                last_sample_age_us: self.last_sample_age_ms * 1_000,
                selected_sample_age_us: self.selected_sample_age_ms * 1_000,
                residual_max_consecutive_failures: self.residual_max_consecutive_failures,
                alignment_lag_us: self.alignment_lag_us,
                alignment_search_window_us: self.alignment_search_window_us,
                alignment_buffer_miss_consecutive_failures:
                    self.alignment_buffer_miss_consecutive_failures,
            },
        }
    }

    pub fn read_policy(&self) -> ControlReadPolicy {
        ControlReadPolicy {
            max_state_skew_us: self.state_skew_max_us,
            max_feedback_age: Duration::from_millis(self.last_sample_age_ms),
        }
    }
}

pub fn raw_clock_warmup_sample_threshold(settings: &SvsRawClockSettings) -> usize {
    let warmup_ms = settings.warmup_secs.saturating_mul(1_000);
    let sample_gap_max_ms = settings.sample_gap_max_ms.max(1);
    let samples = warmup_ms.saturating_add(sample_gap_max_ms - 1) / sample_gap_max_ms;
    usize::try_from(samples.max(4)).unwrap_or(usize::MAX)
}

fn validate_u64_range(name: &str, value: u64, min: u64, max: u64) -> Result<()> {
    if !(min..=max).contains(&value) {
        bail!("raw-clock {name} must be in {min}..={max}; got {value}");
    }
    Ok(())
}

fn validate_u32_range(name: &str, value: u32, min: u32, max: u32) -> Result<()> {
    if !(min..=max).contains(&value) {
        bail!("raw-clock {name} must be in {min}..={max}; got {value}");
    }
    Ok(())
}
```

Add `pub mod raw_clock;` to `addons/piper-svs-collect/src/lib.rs`.

- [ ] **Step 6: Run tests to GREEN**

Run:

```bash
cargo test --manifest-path addons/piper-svs-collect/Cargo.toml raw_clock -- --nocapture
cargo test --manifest-path addons/piper-svs-collect/Cargo.toml parses_required_svs_collect_args -- --nocapture
```

Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add addons/piper-svs-collect/src/raw_clock.rs addons/piper-svs-collect/src/lib.rs addons/piper-svs-collect/src/args.rs addons/piper-svs-collect/src/profile.rs addons/piper-svs-collect/src/collector.rs addons/piper-svs-collect/tests/dependency_boundaries.rs
git commit -m "Add SVS raw-clock settings"
```

## Task 6: Add Optional Raw-Clock Manifest And Report Sections

**Files:**
- Modify: `addons/piper-svs-collect/src/episode/manifest.rs`
- Modify: `addons/piper-svs-collect/src/raw_clock.rs`
- Modify: `addons/piper-svs-collect/src/collector.rs`
- Test: `addons/piper-svs-collect/tests/collector_fake.rs`

- [ ] **Step 1: Write failing compatibility tests**

In `addons/piper-svs-collect/src/episode/manifest.rs`, add:

```rust
#[test]
fn strict_realtime_test_manifest_has_no_raw_clock_section() {
    let manifest = ManifestV1::for_test_complete();
    assert!(manifest.raw_clock.is_none());

    let text = toml::to_string_pretty(&manifest).expect("manifest should serialize");
    assert!(!text.contains("[raw_clock]"));
    let decoded: ManifestV1 = toml::from_str(&text).expect("manifest should deserialize");
    assert!(decoded.raw_clock.is_none());
}

#[test]
fn strict_realtime_test_report_has_no_raw_clock_section() {
    let report = ReportJson::for_test_faulted();
    assert!(report.raw_clock.is_none());

    let text = serde_json::to_string_pretty(&report).expect("report should serialize");
    assert!(!text.contains("\"raw_clock\""));
    let decoded: ReportJson = serde_json::from_str(&text).expect("report should deserialize");
    assert!(decoded.raw_clock.is_none());
}
```

- [ ] **Step 2: Run tests to verify RED**

Run:

```bash
cargo test --manifest-path addons/piper-svs-collect/Cargo.toml strict_realtime_test_manifest_has_no_raw_clock_section -- --nocapture
cargo test --manifest-path addons/piper-svs-collect/Cargo.toml strict_realtime_test_report_has_no_raw_clock_section -- --nocapture
```

Expected: FAIL because `raw_clock` fields do not exist.

- [ ] **Step 3: Add manifest/report structs**

In `manifest.rs`, add:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RawClockManifest {
    pub timing_source: String,
    pub strict_realtime: bool,
    pub experimental: bool,
    pub warmup_secs: u64,
    pub residual_p95_us: u64,
    pub residual_max_us: u64,
    pub drift_abs_ppm: f64,
    pub sample_gap_max_ms: u64,
    pub last_sample_age_ms: u64,
    pub selected_sample_age_ms: u64,
    pub inter_arm_skew_max_us: u64,
    pub state_skew_max_us: u64,
    pub residual_max_consecutive_failures: u32,
    pub alignment_lag_us: u64,
    pub alignment_search_window_us: u64,
    pub alignment_buffer_miss_consecutive_failures: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RawClockReportJson {
    pub timing_source: String,
    pub strict_realtime: bool,
    pub experimental: bool,
    pub warmup_secs: u64,
    pub residual_p95_us: u64,
    pub residual_max_us: u64,
    pub drift_abs_ppm: f64,
    pub sample_gap_max_ms: u64,
    pub last_sample_age_ms: u64,
    pub selected_sample_age_ms: u64,
    pub inter_arm_skew_max_us: u64,
    pub state_skew_max_us: u64,
    pub residual_max_consecutive_failures: u32,
    pub alignment_buffer_miss_consecutive_failure_threshold: u32,
    pub master_clock_drift_ppm: f64,
    pub slave_clock_drift_ppm: f64,
    pub master_residual_p95_us: u64,
    pub slave_residual_p95_us: u64,
    pub selected_inter_arm_skew_max_us: u64,
    pub selected_inter_arm_skew_p95_us: u64,
    pub latest_inter_arm_skew_max_us: u64,
    pub latest_inter_arm_skew_p95_us: u64,
    pub alignment_lag_us: u64,
    pub alignment_search_window_us: u64,
    pub alignment_buffer_misses: u64,
    pub alignment_buffer_miss_consecutive_max: u32,
    pub alignment_buffer_miss_consecutive_failures: u32,
    pub master_residual_max_spikes: u64,
    pub slave_residual_max_spikes: u64,
    pub master_residual_max_consecutive_failures: u32,
    pub slave_residual_max_consecutive_failures: u32,
    pub clock_health_failures: u64,
    pub read_faults: u32,
    pub submission_faults: u32,
    pub runtime_faults: u32,
    pub compensation_faults: u32,
    pub controller_faults: u32,
    pub telemetry_sink_faults: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_failure_kind: Option<String>,
}
```

Add optional fields:

```rust
#[serde(default, skip_serializing_if = "Option::is_none")]
pub raw_clock: Option<RawClockManifest>,
```

to `ManifestV1`, and:

```rust
#[serde(default, skip_serializing_if = "Option::is_none")]
pub raw_clock: Option<RawClockReportJson>,
```

to `ReportJson`.

Because `RawClockManifest` and `RawClockReportJson` contain `f64`, remove `Eq`
from the derive list on both `ManifestV1` and `ReportJson`; both should derive
exactly `Debug, Clone, Serialize, Deserialize, PartialEq`.

In `ManifestV1::for_test_complete()` and `ReportJson::for_test_faulted()`, set `raw_clock: None`.

Update every existing `ManifestV1 { ... }` and `ReportJson { ... }` struct literal in `addons/piper-svs-collect/src/episode/manifest.rs`, `addons/piper-svs-collect/src/collector.rs`, and `addons/piper-svs-collect/tests/collector_fake.rs` in this same task. StrictRealtime and fake helpers must set `raw_clock: None` until later tasks explicitly populate it. In particular:

- `write_manifest` in `collector.rs`: after `let mut manifest = ManifestV1::for_test_complete();`, keep `manifest.raw_clock = None` for the strict fake path.
- `write_report` in `collector.rs`: add `raw_clock: None,` to the direct `ReportJson` literal.
- Any test fixture constructors in `manifest.rs`: add `raw_clock: None,`.

Add a report-specific finite helper so invalid report fields produce `InvalidReport`, not `InvalidManifest`:

```rust
fn validate_report_finite(name: &str, value: f64) -> Result<(), ManifestError> {
    if !value.is_finite() {
        return Err(ManifestError::InvalidReport(format!("{name} must be finite")));
    }
    Ok(())
}
```

In `ManifestV1::validate`, call:

```rust
if let Some(raw_clock) = &self.raw_clock {
    raw_clock.validate()?;
}
```

In `ReportJson::validate`, call the report equivalent.

Implement basic validation:

```rust
impl RawClockManifest {
    fn validate(&self) -> Result<(), ManifestError> {
        require_nonempty(&self.timing_source, "raw_clock.timing_source")?;
        if self.timing_source != "calibrated_hw_raw" {
            return Err(ManifestError::InvalidManifest(
                "raw_clock.timing_source must be calibrated_hw_raw".to_string(),
            ));
        }
        if self.strict_realtime || !self.experimental {
            return Err(ManifestError::InvalidManifest(
                "raw_clock must be experimental and non-strict".to_string(),
            ));
        }
        validate_finite("raw_clock.drift_abs_ppm", self.drift_abs_ppm)
    }
}

impl RawClockReportJson {
    fn validate(&self) -> Result<(), ManifestError> {
        require_nonempty(&self.timing_source, "raw_clock.timing_source")?;
        if self.timing_source != "calibrated_hw_raw" {
            return Err(ManifestError::InvalidReport(
                "raw_clock.timing_source must be calibrated_hw_raw".to_string(),
            ));
        }
        if self.strict_realtime || !self.experimental {
            return Err(ManifestError::InvalidReport(
                "raw_clock report must be experimental and non-strict".to_string(),
            ));
        }
        validate_report_finite("raw_clock.drift_abs_ppm", self.drift_abs_ppm)?;
        validate_report_finite("raw_clock.master_clock_drift_ppm", self.master_clock_drift_ppm)?;
        validate_report_finite("raw_clock.slave_clock_drift_ppm", self.slave_clock_drift_ppm)
    }
}
```

- [ ] **Step 4: Add conversion helpers in `raw_clock.rs`**

Append:

```rust
// Reuse the imports added when creating raw_clock.rs in Task 5.
use crate::episode::manifest::{RawClockManifest, RawClockReportJson};

impl SvsRawClockSettings {
    pub fn to_manifest(&self) -> RawClockManifest {
        RawClockManifest {
            timing_source: "calibrated_hw_raw".to_string(),
            strict_realtime: false,
            experimental: true,
            warmup_secs: self.warmup_secs,
            residual_p95_us: self.residual_p95_us,
            residual_max_us: self.residual_max_us,
            drift_abs_ppm: self.drift_abs_ppm,
            sample_gap_max_ms: self.sample_gap_max_ms,
            last_sample_age_ms: self.last_sample_age_ms,
            selected_sample_age_ms: self.selected_sample_age_ms,
            inter_arm_skew_max_us: self.inter_arm_skew_max_us,
            state_skew_max_us: self.state_skew_max_us,
            residual_max_consecutive_failures: self.residual_max_consecutive_failures,
            alignment_lag_us: self.alignment_lag_us,
            alignment_search_window_us: self.alignment_search_window_us,
            alignment_buffer_miss_consecutive_failures:
                self.alignment_buffer_miss_consecutive_failures,
        }
    }
}

pub fn raw_clock_report_json(
    report: &RawClockRuntimeReport,
    settings: &SvsRawClockSettings,
) -> RawClockReportJson {
    RawClockReportJson {
        timing_source: "calibrated_hw_raw".to_string(),
        strict_realtime: false,
        experimental: true,
        warmup_secs: settings.warmup_secs,
        residual_p95_us: settings.residual_p95_us,
        residual_max_us: settings.residual_max_us,
        drift_abs_ppm: settings.drift_abs_ppm,
        sample_gap_max_ms: settings.sample_gap_max_ms,
        last_sample_age_ms: settings.last_sample_age_ms,
        selected_sample_age_ms: settings.selected_sample_age_ms,
        inter_arm_skew_max_us: settings.inter_arm_skew_max_us,
        state_skew_max_us: settings.state_skew_max_us,
        residual_max_consecutive_failures: settings.residual_max_consecutive_failures,
        alignment_buffer_miss_consecutive_failure_threshold:
            settings.alignment_buffer_miss_consecutive_failures,
        master_clock_drift_ppm: report.master.drift_ppm,
        slave_clock_drift_ppm: report.slave.drift_ppm,
        master_residual_p95_us: report.master.residual_p95_us,
        slave_residual_p95_us: report.slave.residual_p95_us,
        selected_inter_arm_skew_max_us: report.selected_inter_arm_skew_max_us,
        selected_inter_arm_skew_p95_us: report.selected_inter_arm_skew_p95_us,
        latest_inter_arm_skew_max_us: report.latest_inter_arm_skew_max_us,
        latest_inter_arm_skew_p95_us: report.latest_inter_arm_skew_p95_us,
        alignment_lag_us: report.alignment_lag_us,
        alignment_search_window_us: settings.alignment_search_window_us,
        alignment_buffer_misses: report.alignment_buffer_misses,
        alignment_buffer_miss_consecutive_max: report.alignment_buffer_miss_consecutive_max,
        alignment_buffer_miss_consecutive_failures:
            report.alignment_buffer_miss_consecutive_failures,
        master_residual_max_spikes: report.master_residual_max_spikes,
        slave_residual_max_spikes: report.slave_residual_max_spikes,
        master_residual_max_consecutive_failures:
            report.master_residual_max_consecutive_failures,
        slave_residual_max_consecutive_failures: report.slave_residual_max_consecutive_failures,
        clock_health_failures: report.clock_health_failures,
        read_faults: report.read_faults,
        submission_faults: report.submission_faults,
        runtime_faults: report.runtime_faults,
        compensation_faults: report.compensation_faults,
        controller_faults: report.controller_faults,
        telemetry_sink_faults: report.telemetry_sink_faults,
        final_failure_kind: report.exit_reason.and_then(|reason| match reason {
            RawClockRuntimeExitReason::MaxIterations => None,
            other => Some(format!("{other:?}")),
        }),
    }
}

pub fn raw_clock_startup_report_json(
    settings: &SvsRawClockSettings,
    final_failure_kind: Option<String>,
) -> RawClockReportJson {
    RawClockReportJson {
        timing_source: "calibrated_hw_raw".to_string(),
        strict_realtime: false,
        experimental: true,
        warmup_secs: settings.warmup_secs,
        residual_p95_us: settings.residual_p95_us,
        residual_max_us: settings.residual_max_us,
        drift_abs_ppm: settings.drift_abs_ppm,
        sample_gap_max_ms: settings.sample_gap_max_ms,
        last_sample_age_ms: settings.last_sample_age_ms,
        selected_sample_age_ms: settings.selected_sample_age_ms,
        inter_arm_skew_max_us: settings.inter_arm_skew_max_us,
        state_skew_max_us: settings.state_skew_max_us,
        residual_max_consecutive_failures: settings.residual_max_consecutive_failures,
        alignment_buffer_miss_consecutive_failure_threshold:
            settings.alignment_buffer_miss_consecutive_failures,
        master_clock_drift_ppm: 0.0,
        slave_clock_drift_ppm: 0.0,
        master_residual_p95_us: 0,
        slave_residual_p95_us: 0,
        selected_inter_arm_skew_max_us: 0,
        selected_inter_arm_skew_p95_us: 0,
        latest_inter_arm_skew_max_us: 0,
        latest_inter_arm_skew_p95_us: 0,
        alignment_lag_us: settings.alignment_lag_us,
        alignment_search_window_us: settings.alignment_search_window_us,
        alignment_buffer_misses: 0,
        alignment_buffer_miss_consecutive_max: 0,
        alignment_buffer_miss_consecutive_failures: 0,
        master_residual_max_spikes: 0,
        slave_residual_max_spikes: 0,
        master_residual_max_consecutive_failures: 0,
        slave_residual_max_consecutive_failures: 0,
        clock_health_failures: 0,
        read_faults: 0,
        submission_faults: 0,
        runtime_faults: 0,
        compensation_faults: 0,
        controller_faults: 0,
        telemetry_sink_faults: 0,
        final_failure_kind,
    }
}
```

Add conversion tests in the existing `raw_clock.rs` test module. Extend the test-module imports with:

```rust
use piper_client::dual_arm::StopAttemptResult;
use piper_client::dual_arm_raw_clock::{RawClockRuntimeExitReason, RawClockRuntimeReport};
use piper_tools::raw_clock::RawClockHealth;
```

Then add:

```rust
#[test]
fn raw_clock_settings_conversion_copies_manifest_fields() {
    let mut args = args_for_raw_clock_resolve_tests();
    args.raw_clock_warmup_secs = Some(12);
    args.raw_clock_alignment_buffer_miss_consecutive_failures = Some(5);
    let profile = EffectiveProfile::default_for_tests();

    let settings = SvsRawClockSettings::resolve(&args, &profile).unwrap();
    let manifest = settings.to_manifest();

    assert_eq!(manifest.timing_source, "calibrated_hw_raw");
    assert!(!manifest.strict_realtime);
    assert!(manifest.experimental);
    assert_eq!(manifest.warmup_secs, 12);
    assert_eq!(manifest.alignment_buffer_miss_consecutive_failures, 5);
}

#[test]
fn raw_clock_report_json_copies_runtime_counters_and_failure_kind() {
    let args = args_for_raw_clock_resolve_tests();
    let profile = EffectiveProfile::default_for_tests();
    let settings = SvsRawClockSettings::resolve(&args, &profile).unwrap();
    let report = RawClockRuntimeReport {
        master: RawClockHealth {
            healthy: true,
            sample_count: 2_000,
            window_duration_us: 20_000_000,
            drift_ppm: -12.5,
            residual_p50_us: 10,
            residual_p95_us: 111,
            residual_p99_us: 120,
            residual_max_us: 130,
            sample_gap_max_us: 10_000,
            last_sample_age_us: 500,
            raw_timestamp_regressions: 0,
            failure_kind: None,
            reason: None,
        },
        slave: RawClockHealth {
            drift_ppm: 34.5,
            residual_p95_us: 222,
            ..RawClockHealth {
                healthy: true,
                sample_count: 2_000,
                window_duration_us: 20_000_000,
                drift_ppm: 0.0,
                residual_p50_us: 10,
                residual_p95_us: 100,
                residual_p99_us: 120,
                residual_max_us: 130,
                sample_gap_max_us: 10_000,
                last_sample_age_us: 500,
                raw_timestamp_regressions: 0,
                failure_kind: None,
                reason: None,
            }
        },
        joint_motion: None,
        max_inter_arm_skew_us: 4_321,
        inter_arm_skew_p95_us: 2_345,
        alignment_lag_us: 5_000,
        latest_inter_arm_skew_max_us: 4_000,
        latest_inter_arm_skew_p95_us: 2_000,
        selected_inter_arm_skew_max_us: 4_321,
        selected_inter_arm_skew_p95_us: 2_345,
        clock_health_failures: 7,
        compensation_faults: 1,
        controller_faults: 2,
        telemetry_sink_faults: 3,
        alignment_buffer_misses: 11,
        alignment_buffer_miss_consecutive_max: 4,
        alignment_buffer_miss_consecutive_failures: 5,
        master_residual_max_spikes: 6,
        slave_residual_max_spikes: 8,
        master_residual_max_consecutive_failures: 2,
        slave_residual_max_consecutive_failures: 3,
        read_faults: 9,
        submission_faults: 10,
        last_submission_failed_side: None,
        peer_command_may_have_applied: false,
        runtime_faults: 12,
        master_tx_realtime_overwrites_total: 0,
        slave_tx_realtime_overwrites_total: 0,
        master_tx_frames_sent_total: 100,
        slave_tx_frames_sent_total: 100,
        master_tx_fault_aborts_total: 0,
        slave_tx_fault_aborts_total: 0,
        last_runtime_fault_master: None,
        last_runtime_fault_slave: None,
        iterations: 100,
        exit_reason: Some(RawClockRuntimeExitReason::TelemetrySinkFault),
        master_stop_attempt: StopAttemptResult::NotAttempted,
        slave_stop_attempt: StopAttemptResult::NotAttempted,
        last_error: Some("sink failed".to_string()),
    };

    let json = raw_clock_report_json(&report, &settings);

    assert_eq!(json.master_clock_drift_ppm, -12.5);
    assert_eq!(json.slave_clock_drift_ppm, 34.5);
    assert_eq!(json.master_residual_p95_us, 111);
    assert_eq!(json.slave_residual_p95_us, 222);
    assert_eq!(json.selected_inter_arm_skew_max_us, 4_321);
    assert_eq!(json.latest_inter_arm_skew_p95_us, 2_000);
    assert_eq!(json.alignment_buffer_misses, 11);
    assert_eq!(json.alignment_buffer_miss_consecutive_max, 4);
    assert_eq!(json.alignment_buffer_miss_consecutive_failures, 5);
    assert_eq!(json.compensation_faults, 1);
    assert_eq!(json.controller_faults, 2);
    assert_eq!(json.telemetry_sink_faults, 3);
    assert_eq!(json.clock_health_failures, 7);
    assert_eq!(json.final_failure_kind.as_deref(), Some("TelemetrySinkFault"));
}

#[test]
fn raw_clock_report_json_omits_failure_kind_for_clean_max_iterations() {
    let args = args_for_raw_clock_resolve_tests();
    let profile = EffectiveProfile::default_for_tests();
    let settings = SvsRawClockSettings::resolve(&args, &profile).unwrap();
    let report = raw_clock_report_for_tests(RawClockRuntimeExitReason::MaxIterations);

    let json = raw_clock_report_json(&report, &settings);
    let serialized = serde_json::to_string(&json).unwrap();

    assert!(json.final_failure_kind.is_none());
    assert!(
        !serialized.contains("final_failure_kind"),
        "clean raw-clock report should omit final_failure_kind instead of serializing null: {serialized}"
    );
}
```

Add this private test helper in the same test module:

```rust
fn raw_clock_report_for_tests(exit_reason: RawClockRuntimeExitReason) -> RawClockRuntimeReport {
    fn health() -> RawClockHealth {
        RawClockHealth {
            healthy: true,
            sample_count: 2_000,
            window_duration_us: 20_000_000,
            drift_ppm: 0.0,
            residual_p50_us: 0,
            residual_p95_us: 0,
            residual_p99_us: 0,
            residual_max_us: 0,
            sample_gap_max_us: 10_000,
            last_sample_age_us: 1_000,
            raw_timestamp_regressions: 0,
            failure_kind: None,
            reason: None,
        }
    }

    RawClockRuntimeReport {
        master: health(),
        slave: health(),
        joint_motion: None,
        max_inter_arm_skew_us: 0,
        inter_arm_skew_p95_us: 0,
        alignment_lag_us: 0,
        latest_inter_arm_skew_max_us: 0,
        latest_inter_arm_skew_p95_us: 0,
        selected_inter_arm_skew_max_us: 0,
        selected_inter_arm_skew_p95_us: 0,
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
        master_tx_frames_sent_total: 0,
        slave_tx_frames_sent_total: 0,
        master_tx_fault_aborts_total: 0,
        slave_tx_fault_aborts_total: 0,
        last_runtime_fault_master: None,
        last_runtime_fault_slave: None,
        iterations: 0,
        exit_reason: Some(exit_reason),
        master_stop_attempt: StopAttemptResult::NotAttempted,
        slave_stop_attempt: StopAttemptResult::NotAttempted,
        last_error: None,
    }
}
```

Always call this helper with the resolved `SvsRawClockSettings` used for the run.

- [ ] **Step 5: Update collector contexts**

In `RealEpisodeBase`, add:

```rust
raw_clock: Option<crate::episode::manifest::RawClockManifest>,
```

In `RealRunContext`, add:

```rust
raw_clock_report: Option<piper_client::dual_arm_raw_clock::RawClockRuntimeReport>,
raw_clock_settings: Option<crate::raw_clock::SvsRawClockSettings>,
```

In `write_real_manifest`, copy `context.base.raw_clock` to `manifest.raw_clock`.

In `write_real_report`, compute raw-clock JSON before constructing `ReportJson`:

```rust
let raw_clock_json = match (
    context.raw_clock_report.as_ref(),
    context.raw_clock_settings.as_ref(),
) {
    (Some(raw_report), Some(settings)) => {
        Some(crate::raw_clock::raw_clock_report_json(raw_report, settings))
    },
    (None, Some(settings)) => Some(crate::raw_clock::raw_clock_startup_report_json(
        settings,
        report
            .exit_reason
            .map(|reason| format!("{reason:?}"))
            .or_else(|| report.last_error.clone()),
    )),
    (Some(_), None) => {
        return Err(anyhow!(
            "raw-clock report is present but raw-clock settings are missing"
        ));
    },
    (None, _) => None,
};
```

Then set the `ReportJson` field:

```rust
raw_clock: raw_clock_json,
```

Set these fields to `None` in the StrictRealtime path.

- [ ] **Step 6: Run compatibility tests**

Run:

```bash
cargo test --manifest-path addons/piper-svs-collect/Cargo.toml strict_realtime_test_manifest_has_no_raw_clock_section -- --nocapture
cargo test --manifest-path addons/piper-svs-collect/Cargo.toml strict_realtime_test_report_has_no_raw_clock_section -- --nocapture
cargo test --manifest-path addons/piper-svs-collect/Cargo.toml fake_workflow_writes_complete_episode -- --nocapture
cargo test --manifest-path addons/piper-svs-collect/Cargo.toml raw_clock_settings_conversion_copies_manifest_fields -- --nocapture
cargo test --manifest-path addons/piper-svs-collect/Cargo.toml raw_clock_report_json_copies_runtime_counters_and_failure_kind -- --nocapture
cargo test --manifest-path addons/piper-svs-collect/Cargo.toml raw_clock_report_json_omits_failure_kind_for_clean_max_iterations -- --nocapture
```

Expected: PASS, and StrictRealtime fake outputs do not contain raw-clock sections.

- [ ] **Step 7: Commit**

```bash
git add addons/piper-svs-collect/src/episode/manifest.rs addons/piper-svs-collect/src/raw_clock.rs addons/piper-svs-collect/src/collector.rs addons/piper-svs-collect/tests/collector_fake.rs
git commit -m "Add SVS raw-clock metadata sections"
```

## Task 7: Wire Real SVS Raw-Clock Collector Backend

**Files:**
- Modify: `crates/piper-client/src/dual_arm_raw_clock.rs`
- Modify: `addons/piper-svs-collect/src/collector.rs`
- Modify: `addons/piper-svs-collect/src/raw_clock.rs`

- [ ] **Step 1: Write failing runtime selection unit test**

In `addons/piper-svs-collect/src/raw_clock.rs`, append this to the existing `#[cfg(test)] mod tests` created in Task 5. Do not add a second top-level `mod tests`:

```rust
    fn args_for_tests() -> Args {
        Args {
            master_target: "socketcan:can1".to_string(),
            slave_target: "socketcan:can0".to_string(),
            baud_rate: None,
            model_dir: None,
            use_standard_model_path: false,
            use_embedded_model: true,
            task_profile: None,
            output_dir: "out".into(),
            calibration_file: None,
            save_calibration: None,
            calibration_max_error_rad: None,
            mirror_map: Some("left-right".to_string()),
            operator: None,
            task: None,
            notes: None,
            raw_can: false,
            disable_gripper_mirror: true,
            max_iterations: Some(10),
            timing_mode: "spin".to_string(),
            yes: true,
            experimental_calibrated_raw: false,
            raw_clock_warmup_secs: None,
            raw_clock_residual_p95_us: None,
            raw_clock_residual_max_us: None,
            raw_clock_drift_abs_ppm: None,
            raw_clock_sample_gap_max_ms: None,
            raw_clock_last_sample_age_ms: None,
            raw_clock_selected_sample_age_ms: None,
            raw_clock_inter_arm_skew_max_us: None,
            raw_clock_state_skew_max_us: None,
            raw_clock_residual_max_consecutive_failures: None,
            raw_clock_alignment_lag_us: None,
            raw_clock_alignment_search_window_us: None,
            raw_clock_alignment_buffer_miss_consecutive_failures: None,
        }
    }

    #[test]
    fn runtime_kind_requires_explicit_raw_clock_opt_in() {
        let mut args = args_for_tests();
        assert_eq!(SvsRuntimeKind::from_args(&args), SvsRuntimeKind::StrictRealtime);
        args.experimental_calibrated_raw = true;
        assert_eq!(
            SvsRuntimeKind::from_args(&args),
            SvsRuntimeKind::CalibratedRawClock
        );
    }
```

- [ ] **Step 2: Run test to verify current behavior**

Run:

```bash
cargo test --manifest-path addons/piper-svs-collect/Cargo.toml runtime_kind_requires_explicit_raw_clock_opt_in -- --nocapture
```

Expected: PASS if Task 5 already added `SvsRuntimeKind`; if it fails, complete Task 5 first.

- [ ] **Step 3: Split current real collector flow**

In `addons/piper-svs-collect/src/collector.rs`, rename the existing `run_real_collector` function to `run_strict_realtime_collector`, keeping its current body intact except for new `RealRunContext` raw-clock fields set to `None`. Then add a new dispatcher named `run_real_collector`:

```rust
fn run_real_collector(args: Args, cancel: CollectorCancelToken) -> Result<CollectorRunResult> {
    match crate::raw_clock::SvsRuntimeKind::from_args(&args) {
        crate::raw_clock::SvsRuntimeKind::StrictRealtime => {
            run_strict_realtime_collector(args, cancel)
        },
        crate::raw_clock::SvsRuntimeKind::CalibratedRawClock => {
            run_raw_clock_collector(args, cancel)
        },
    }
}
```

The renamed `run_strict_realtime_collector(args, cancel)` is not a new implementation. It is the original `run_real_collector` body moved under the new name. In that moved body, only initialize the new raw-clock fields: set `RealRunContext::raw_clock_report` and `RealRunContext::raw_clock_settings` to `None`, and set `RealEpisodeBase::raw_clock` to `None`.

- [ ] **Step 4: Add SoftRealtime connection helper**

First add failing SDK snapshot tests in `crates/piper-client/src/dual_arm_raw_clock.rs`:

```rust
#[test]
fn raw_clock_active_snapshot_reads_dual_arm_snapshot() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let active = build_active_runtime_for_tests(events);

    let snapshot = active
        .snapshot(ControlReadPolicy {
            max_state_skew_us: 10_000,
            max_feedback_age: Duration::from_millis(50),
        })
        .expect("active raw-clock snapshot should be readable");

    assert_eq!(snapshot.left.state.position.as_array().len(), 6);
    assert_eq!(snapshot.right.state.position.as_array().len(), 6);
}

#[test]
fn raw_clock_standby_snapshot_reads_dual_arm_snapshot() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let standby = ExperimentalRawClockDualArmStandby {
        master: build_soft_standby_piper(
            PacedRxAdapter::new(ready_snapshot_script("master", events.clone())),
            LabeledRecordingTxAdapter::new("master", events.clone()),
        ),
        slave: build_soft_standby_piper(
            PacedRxAdapter::new(ready_snapshot_script("slave", events.clone())),
            LabeledRecordingTxAdapter::new("slave", events),
        ),
        timing: ready_timing_for_tests(),
        config: ExperimentalRawClockConfig::default(),
    };

    let snapshot = standby
        .snapshot(ControlReadPolicy {
            max_state_skew_us: 10_000,
            max_feedback_age: Duration::from_millis(50),
        })
        .expect("standby raw-clock snapshot should be readable");

    assert_eq!(snapshot.left.state.position.as_array().len(), 6);
    assert_eq!(snapshot.right.state.position.as_array().len(), 6);
}
```

Add the small `ready_snapshot_script` helper by reusing the existing raw feedback frame builders used by `build_active_raw_clock_piper`.

Run:

```bash
cargo test -p piper-client raw_clock_active_snapshot_reads_dual_arm_snapshot -- --nocapture
cargo test -p piper-client raw_clock_standby_snapshot_reads_dual_arm_snapshot -- --nocapture
```

Expected: FAIL until `snapshot()` exists on both raw-clock active and standby states.

Then add `snapshot()` to both `ExperimentalRawClockDualArmStandby` and `ExperimentalRawClockDualArmActive`:

```rust
pub fn snapshot(
    &self,
    policy: ControlReadPolicy,
) -> Result<DualArmSnapshot, RawClockRuntimeError> {
    let master = read_experimental_snapshot_from_driver(
        &self.master.driver,
        RawClockSide::Master,
        policy,
    )?;
    let slave = read_experimental_snapshot_from_driver(
        &self.slave.driver,
        RawClockSide::Slave,
        policy,
    )?;
    let inter_arm_skew_us = master
        .newest_raw_feedback_timing
        .host_rx_mono_us
        .abs_diff(slave.newest_raw_feedback_timing.host_rx_mono_us);
    Ok(raw_dual_arm_snapshot(&master, &slave, inter_arm_skew_us))
}
```

Rerun both tests. Expected: PASS.

In `collector.rs`, import:

```rust
use piper_client::dual_arm_raw_clock::{
    ExperimentalRawClockDualArmActive, ExperimentalRawClockDualArmStandby,
    ExperimentalRawClockRunExit, RawClockRuntimeExitReason, RawClockRuntimeReport,
};
use piper_client::state::{Piper, SoftRealtime, Standby};
use piper_client::{MotionConnectedPiper, MotionConnectedState};
```

Add:

```rust
fn connect_soft_socketcan_standby(
    role: &'static str,
    target: &SocketCanTarget,
    baud_rate: Option<u32>,
) -> Result<Piper<Standby, SoftRealtime>> {
    let mut builder = PiperBuilder::new().socketcan(target.iface.clone());
    if let Some(baud_rate) = baud_rate {
        builder = builder.baud_rate(baud_rate);
    }
    let connected = builder
        .build()
        .with_context(|| format!("failed to connect raw-clock {role} target socketcan:{}", target.iface))?;

    match connected.require_motion()? {
        MotionConnectedPiper::Soft(MotionConnectedState::Standby(standby)) => Ok(standby),
        MotionConnectedPiper::Soft(MotionConnectedState::Maintenance(_))
        | MotionConnectedPiper::Strict(MotionConnectedState::Maintenance(_)) => {
            Err(anyhow!("maintenance required before SVS raw-clock teleop"))
        },
        MotionConnectedPiper::Strict(MotionConnectedState::Standby(_)) => Err(anyhow!(
            "SVS raw-clock expected SoftRealtime; use normal SVS StrictRealtime backend"
        )),
    }
}
```

`PiperBuilder::new().socketcan(...)` performs the startup capability probe and returns a typed `MotionConnectedPiper::Soft(...)` when the SocketCAN feedback stream exposes SoftRealtime raw-clock timing. The helper must reject StrictRealtime and maintenance states as shown above; this is the explicit SoftRealtime request boundary for SVS raw-clock. Add a unit test or fake-builder coverage if the implementation has an injectable builder seam; otherwise keep the runtime type check and error message exact.

- [ ] **Step 5: Add raw-clock startup status prints**

In `collector.rs`, add:

```rust
fn print_raw_clock_startup_stage(message: &str) {
    eprintln!("{message}");
}
```

Add a startup summary before operator confirmation:

```rust
fn print_raw_clock_startup_summary(
    master_target: &SocketCanTarget,
    slave_target: &SocketCanTarget,
    profile: &EffectiveProfile,
    raw_clock: &crate::raw_clock::SvsRawClockSettings,
    gripper_mirror_effective: bool,
    calibration_source: &str,
    output_dir: &Path,
) {
    eprintln!("svs raw-clock startup summary");
    eprintln!("  master target: socketcan:{}", master_target.iface);
    eprintln!("  slave target: socketcan:{}", slave_target.iface);
    eprintln!("  mode: bilateral");
    eprintln!("  frequency: {:.1} Hz", profile.control.loop_frequency_hz);
    eprintln!("  timing_source=calibrated_hw_raw");
    eprintln!("  experimental=true");
    eprintln!("  strict_realtime=false");
    eprintln!("  raw-clock warmup: {}s", raw_clock.warmup_secs);
    eprintln!(
        "  raw-clock gates: skew={}us state_skew={}us residual_p95={}us residual_max={}us",
        raw_clock.inter_arm_skew_max_us,
        raw_clock.state_skew_max_us,
        raw_clock.residual_p95_us,
        raw_clock.residual_max_us,
    );
    eprintln!("  calibration: {calibration_source}");
    eprintln!("  gripper mirror: {gripper_mirror_effective}");
    eprintln!("  output dir: {}", output_dir.display());
}
```

Use exact messages from the spec:

```rust
print_raw_clock_startup_stage("startup: refreshing raw-clock timing (~10s)...");
print_raw_clock_startup_stage("startup: enabling MIT passthrough...");
print_raw_clock_startup_stage("startup: reading active snapshot...");
print_raw_clock_startup_stage("startup: capturing active-zero calibration...");
print_raw_clock_startup_stage("startup: starting SVS raw-clock bilateral loop...");
```

- [ ] **Step 6: Implement raw-clock run config conversion**

In `raw_clock.rs`, add:

```rust
impl SvsRawClockSettings {
    pub fn to_run_config(
        &self,
        loop_config: &BilateralLoopConfig,
    ) -> ExperimentalRawClockRunConfig {
        ExperimentalRawClockRunConfig {
            read_policy: self.read_policy(),
            command_timeout: Duration::from_millis(20),
            disable_config: loop_config.disable_config.clone(),
            cancel_signal: loop_config.cancel_signal.clone(),
            dt_clamp_multiplier: loop_config.dt_clamp_multiplier,
            telemetry_sink: loop_config.telemetry_sink.clone(),
            gripper: GripperTeleopConfig {
                enabled: false,
                ..loop_config.gripper
            },
            output_shaping: Some(BilateralOutputShapingConfig::from_loop_config(loop_config)),
        }
    }
}
```

Add this test to the existing `raw_clock.rs` test module after `to_run_config` exists. Extend the test-module imports with:

```rust
use piper_client::dual_arm::{
    BilateralLoopConfig, BilateralLoopTelemetry, BilateralLoopTelemetrySink,
    BilateralTelemetrySinkError, GripperTeleopConfig,
};
use piper_client::types::JointArray;
use piper_client::types::units::NewtonMeter;
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
```

Then add:

```rust
struct NoopSink;

impl BilateralLoopTelemetrySink for NoopSink {
    fn on_tick(
        &self,
        _telemetry: &BilateralLoopTelemetry,
    ) -> std::result::Result<(), BilateralTelemetrySinkError> {
        Ok(())
    }
}

#[test]
fn raw_clock_run_config_copies_loop_config_and_disables_gripper_mirror() {
    let args = args_for_raw_clock_resolve_tests();
    let profile = EffectiveProfile::default_for_tests();
    let settings = SvsRawClockSettings::resolve(&args, &profile).unwrap();
    let cancel = Arc::new(AtomicBool::new(false));
    cancel.store(true, Ordering::SeqCst);
    let sink: Arc<dyn BilateralLoopTelemetrySink> = Arc::new(NoopSink);
    let loop_config = BilateralLoopConfig {
        dt_clamp_multiplier: 3.0,
        cancel_signal: Some(cancel.clone()),
        telemetry_sink: Some(sink.clone()),
        gripper: GripperTeleopConfig {
            enabled: true,
            max_feedback_age: Duration::from_millis(77),
            ..GripperTeleopConfig::default()
        },
        master_interaction_lpf_cutoff_hz: 9.0,
        master_interaction_limit: JointArray::splat(NewtonMeter(1.1)),
        slave_feedforward_limit: JointArray::splat(NewtonMeter(2.2)),
        ..BilateralLoopConfig::default()
    };

    let run_config = settings.to_run_config(&loop_config);

    assert_eq!(run_config.read_policy.max_state_skew_us, settings.state_skew_max_us);
    assert_eq!(run_config.dt_clamp_multiplier, 3.0);
    assert!(run_config
        .cancel_signal
        .as_ref()
        .is_some_and(|value| Arc::ptr_eq(value, &cancel)));
    assert!(run_config
        .telemetry_sink
        .as_ref()
        .is_some_and(|value| Arc::ptr_eq(value, &sink)));
    assert!(!run_config.gripper.enabled);
    assert_eq!(run_config.gripper.max_feedback_age, Duration::from_millis(77));
    let shaping = run_config.output_shaping.expect("output shaping should be configured");
    assert_eq!(shaping.master_interaction_lpf_cutoff_hz, 9.0);
    assert_eq!(shaping.master_interaction_limit, JointArray::splat(NewtonMeter(1.1)));
    assert_eq!(shaping.slave_feedforward_limit, JointArray::splat(NewtonMeter(2.2)));
}
```

This intentionally disables raw-clock gripper mirroring for first implementation. It does not disable gripper telemetry: the raw-clock runtime still reads master/slave gripper states when a telemetry sink is configured and uses `loop_config.gripper.max_feedback_age` through the copied `GripperTeleopConfig`.
The collector must reject effective gripper mirroring before enable.
Compute that rejection from the effective operator setting, not directly from the profile default:

```rust
pub(crate) fn effective_gripper_mirror_enabled(args: &Args, profile: &EffectiveProfile) -> bool {
    profile.gripper.mirror_enabled && !args.disable_gripper_mirror
}
```

- [ ] **Step 7: Implement `run_raw_clock_collector`**

Create `fn run_raw_clock_collector(args: Args, cancel: CollectorCancelToken) -> Result<CollectorRunResult>` in `collector.rs`.

Use this sequence:

```rust
let (master_target, slave_target) = validate_targets(&args.master_target, &args.slave_target)?;
let resolved_profile = resolve_profile_from_args(&args)?;
let raw_clock_settings = crate::raw_clock::SvsRawClockSettings::resolve(&args, &resolved_profile.profile)?;
let gripper_mirror_effective = crate::raw_clock::effective_gripper_mirror_enabled(
    &args,
    &resolved_profile.profile,
);
let started_unix_ns = current_unix_ns();
let timestamp = utc_timestamp_from_unix_ns(started_unix_ns)?;
let episode_start_host_mono_us = piper_driver::heartbeat::monotonic_micros();
let task_name = args.task.clone().unwrap_or_else(|| DEFAULT_TASK_NAME.to_string());
validate_static_startup_inputs(&args, &task_name)?;

let stager = Arc::new(SvsTickStager::new());
let dynamics_slot = Arc::new(SvsDynamicsSlot::new());
let feedback_history = Arc::new(Mutex::new(AppliedMasterFeedbackHistory::default()));
let resolved_mujoco = resolve_mujoco_from_args(&args, &resolved_profile.profile)?;
let bridge = SvsMujocoBridge::new(
    SvsMujocoBridgeConfig {
        model_source: resolved_mujoco.bridge_source.clone(),
        compensator: resolved_mujoco.compensator.clone(),
        master_ee_site: resolved_profile.profile.mujoco.master_ee_site.clone(),
        slave_ee_site: resolved_profile.profile.mujoco.slave_ee_site.clone(),
    },
    Arc::clone(&dynamics_slot),
)
.context("failed to initialize MuJoCo bridge before MIT enable")?;

let master = connect_soft_socketcan_standby("master", &master_target, args.baud_rate)?;
let slave = connect_soft_socketcan_standby("slave", &slave_target, args.baud_rate)?;
let master_quirks = master.quirks();
let slave_quirks = slave.quirks();
tracing::info!(
    master_firmware = %master_quirks.firmware_version,
    slave_firmware = %slave_quirks.firmware_version,
    "SVS raw-clock firmware/profile context"
);
let raw_config = raw_clock_settings.to_experimental_config(
    resolved_profile.profile.control.loop_frequency_hz,
    args.max_iterations.map(|value| usize::try_from(value).unwrap_or(usize::MAX)),
);
raw_config.validate()?;
let standby = ExperimentalRawClockDualArmStandby::new(master, slave, raw_config)?;
```

Use the existing public `ExperimentalRawClockDualArmStandby::new` API from `crates/piper-client/src/dual_arm_raw_clock.rs`. If the implementation branch does not have that constructor, add it before wiring the collector with this shape: validate `ExperimentalRawClockConfig`, initialize `RawClockRuntimeTiming::new_with_runtime_thresholds(config.estimator_thresholds, config.thresholds)`, and store the SoftRealtime standby arms. Do not invent a separate SVS-only builder.

The `connect_soft_socketcan_standby` helper calls `PiperBuilder::build()`, which already waits for feedback, reads firmware, logs `Detected firmware version`, resolves `DeviceQuirks`, and classifies the initial motion state through `initialize_connected_driver`. The explicit `master.quirks()` / `slave.quirks()` read above records that firmware/profile context in the SVS raw-clock startup path before warmup.

Before the long raw-clock warmup, create a valid startup calibration artifact so Ctrl-C or warmup faults can still finalize an episode with a manifest/report:

```rust
let loaded_calibration = load_raw_clock_calibration_if_present(
    &args,
    resolved_profile.runtime_mirror_map,
)?;

let startup_snapshot = standby
    .snapshot(raw_clock_settings.read_policy())?;

let startup_calibration = match &loaded_calibration {
    Some(loaded) => loaded.clone(),
    None => provisional_raw_clock_startup_calibration(
        &startup_snapshot,
        resolved_profile.runtime_mirror_map,
        started_unix_ns / 1_000_000,
    )?,
};
```

Implement `load_raw_clock_calibration_if_present` in `collector.rs` to return `Result<Option<ResolvedRealCalibration>>` strictly for `--calibration-file`: read `args.calibration_file`, parse `CalibrationFile`, call `resolve_episode_calibration(Some(file), runtime_map, None)`, then build `DualArmCalibration` from `resolved.calibration`. If `args.calibration_file` is absent, return `Ok(None)`. Do not synthesize capture-mode calibration in this helper.

Implement `provisional_raw_clock_startup_calibration` in `collector.rs` for capture mode only: build a provisional `DualArmCalibration` from `startup_snapshot.left/right.state.position`, convert it with `calibration_file_from_dual_arm`, then call `resolve_episode_calibration(None, runtime_map, Some(captured_file))` to obtain a `ResolvedRealCalibration` suitable for the mandatory early episode manifest. This provisional value is not the active-zero calibration and must never be passed as a loaded calibration to `resolve_raw_clock_active_calibration`.

The raw-clock path still performs static setup before episode allocation: profile/model resolution, MuJoCo bridge initialization, SoftRealtime SocketCAN connection, raw-clock config validation, standby construction, optional calibration file read, and one startup snapshot. Failures in this pre-allocation phase intentionally abort without writing an episode because no episode directory has been reserved yet. The long 10s warmup and every operator-visible runtime stage must not run until after the episode directory, manifest, writer, and raw-clock report context exist, so warmup/cancel/timing faults can finalize an episode.

`validate_raw_clock_calibration_posture` must apply the calibration map to the current master/slave positions and fail with the same max-error semantics as the existing SVS calibration compatibility check.

Use `startup_calibration` to write the initial `calibration.toml` and `ManifestV1::calibration` when creating the episode artifacts. For loaded mode, this is the loaded calibration. For capture mode, this is only a provisional startup artifact. After active-zero capture later in this task, replace `calibration.toml` atomically with the active calibration, recompute its hash, update the mutable context/base calibration fields, and rewrite the running manifest before entering the control loop.

Now reserve the episode directory using `EpisodeId::reserve_directory`, write `effective_profile.toml`, write `startup_calibration` to the initial `calibration.toml`, create the `RawCanStatusTracker`, create `SvsHeaderV1`, create `EpisodeWriter::new_with_backpressure_thresholds`, and write the running manifest exactly as the StrictRealtime flow does. In the raw-clock base/context, set `calibration` metadata from `startup_calibration`, set `raw_clock: Some(raw_clock_settings.to_manifest())`, `raw_clock_settings: Some(raw_clock_settings)`, and leave `raw_clock_report: None` until the loop returns. Declare the context binding mutable so the final raw-clock report and, in capture mode, the active-zero calibration hash can be attached before finalization.

Before confirmation, start raw CAN side recording exactly as StrictRealtime does.

Then run the first raw-clock warmup before operator confirmation, matching the spec's “validate before operator enable” requirement. This first warmup happens after episode allocation so Ctrl-C or timing faults still finalize an episode:

```rust
print_raw_clock_startup_stage("startup: refreshing raw-clock timing (~10s)...");
let standby = match standby.warmup(
    raw_clock_settings.read_policy(),
    Duration::from_secs(raw_clock_settings.warmup_secs),
    cancel.loop_signal().as_ref(),
) {
    Ok(standby) => standby,
    Err(error) => {
        let status = if matches!(&error, RawClockRuntimeError::Cancelled) {
            EpisodeStatus::Cancelled
        } else {
            EpisodeStatus::Faulted
        };
        let mut report = BilateralRunReport {
            exit_reason: Some(if status == EpisodeStatus::Cancelled {
                BilateralExitReason::Cancelled
            } else {
                BilateralExitReason::RuntimeTransportFault
            }),
            last_error: Some(error.to_string()),
            ..BilateralRunReport::default()
        };
        return finish_writer_and_finalize(
            &context,
            &writer,
            status,
            &mut report,
            0,
            false,
            raw_can_recording,
        );
    },
};
```

After the context and raw CAN recording handle exist, but still before confirmation, perform the pre-enable posture compatibility check. On failure, finalize a faulted episode without enabling either arm:

```rust
if gripper_mirror_effective {
    let mut report = BilateralRunReport {
        exit_reason: Some(BilateralExitReason::RuntimeTransportFault),
        last_error: Some(
            "SVS raw-clock gripper mirroring is not implemented yet; pass --disable-gripper-mirror"
                .to_string(),
        ),
        ..BilateralRunReport::default()
    };
    return finish_writer_and_finalize(
        &context,
        &writer,
        EpisodeStatus::Faulted,
        &mut report,
        0,
        false,
        raw_can_recording,
    );
}

if let Some(loaded) = &loaded_calibration {
    let standby_snapshot = match standby.snapshot(raw_clock_settings.read_policy()) {
        Ok(snapshot) => snapshot,
        Err(error) => {
            let mut report = BilateralRunReport {
                exit_reason: Some(BilateralExitReason::RuntimeTransportFault),
                last_error: Some(error.to_string()),
                ..BilateralRunReport::default()
            };
            return finish_writer_and_finalize(
                &context,
                &writer,
                EpisodeStatus::Faulted,
                &mut report,
                0,
                false,
                raw_can_recording,
            );
        },
    };
    if let Err(error) = validate_raw_clock_calibration_posture(
        "pre-enable",
        &loaded.resolved.calibration,
        &standby_snapshot,
        resolved_profile.profile.calibration.calibration_max_error_rad,
    ) {
        let mut report = BilateralRunReport {
            exit_reason: Some(BilateralExitReason::RuntimeTransportFault),
            last_error: Some(error.to_string()),
            ..BilateralRunReport::default()
        };
        return finish_writer_and_finalize(
            &context,
            &writer,
            EpisodeStatus::Faulted,
            &mut report,
            0,
            false,
            raw_can_recording,
        );
    }
}
```

Before confirmation, print the startup summary:

```rust
print_raw_clock_startup_summary(
    &master_target,
    &slave_target,
    &resolved_profile.profile,
    &raw_clock_settings,
    gripper_mirror_effective,
    if args.calibration_file.is_some() { "loaded" } else { "captured-after-enable" },
    &episode_dir,
);
```

Then require operator confirmation before enabling MIT:

```rust
let operator_confirmed = match confirm_start_if_needed(&args, &cancel) {
    Ok(confirmed) => confirmed,
    Err(error) => {
        let mut report = BilateralRunReport {
            last_error: Some(error.to_string()),
            ..BilateralRunReport::default()
        };
        return finish_writer_and_finalize(
            &context,
            &writer,
            EpisodeStatus::Faulted,
            &mut report,
            0,
            false,
            raw_can_recording,
        );
    },
};

if cancel.is_cancelled() || !operator_confirmed {
    let mut report = BilateralRunReport {
        exit_reason: Some(BilateralExitReason::Cancelled),
        last_error: Some("collector cancelled before MIT enable".to_string()),
        ..BilateralRunReport::default()
    };
    return finish_writer_and_finalize(
        &context,
        &writer,
        EpisodeStatus::Cancelled,
        &mut report,
        0,
        false,
        raw_can_recording,
    );
}
```

The raw-clock path must not call `enable_mit_passthrough` before this block succeeds.

After confirmation, every fallible step before the control loop must finalize the episode report. Do not use bare `?` in this section. Add helpers:

```rust
fn raw_clock_startup_fault_report(error: impl std::fmt::Display) -> BilateralRunReport {
    BilateralRunReport {
        exit_reason: Some(BilateralExitReason::RuntimeTransportFault),
        last_error: Some(error.to_string()),
        ..BilateralRunReport::default()
    }
}

fn finalize_raw_clock_startup_fault(
    context: &RealRunContext,
    writer: &Arc<SharedEpisodeWriter>,
    raw_can_recording: Option<RawCanRecordingHandle>,
    error: impl std::fmt::Display,
    disable_called: bool,
) -> Result<CollectorRunResult> {
    let mut report = raw_clock_startup_fault_report(error);
    finish_writer_and_finalize(
        context,
        writer,
        EpisodeStatus::Faulted,
        &mut report,
        0,
        disable_called,
        raw_can_recording,
    )
}
```

Use this pattern. This is the second raw-clock timing refresh required after operator confirmation and before MIT enable:

```rust
print_raw_clock_startup_stage("startup: refreshing raw-clock timing (~10s)...");
let standby = match standby.warmup(
    raw_clock_settings.read_policy(),
    Duration::from_secs(raw_clock_settings.warmup_secs),
    cancel.loop_signal().as_ref(),
) {
    Ok(standby) => standby,
    Err(error) => {
        let status = if matches!(&error, RawClockRuntimeError::Cancelled) {
            EpisodeStatus::Cancelled
        } else {
            EpisodeStatus::Faulted
        };
        let mut report = BilateralRunReport {
            exit_reason: Some(if status == EpisodeStatus::Cancelled {
                BilateralExitReason::Cancelled
            } else {
                BilateralExitReason::RuntimeTransportFault
            }),
            last_error: Some(error.to_string()),
            ..BilateralRunReport::default()
        };
        return finish_writer_and_finalize(
            &context,
            &writer,
            status,
            &mut report,
            0,
            false,
            raw_can_recording,
        );
    },
};

print_raw_clock_startup_stage("startup: enabling MIT passthrough...");
let active = match standby.enable_mit_passthrough(MitModeConfig::default(), MitModeConfig::default()) {
    Ok(active) => active,
    Err(error) => {
        return finalize_raw_clock_startup_fault(
            &context,
            &writer,
            raw_can_recording,
            error,
            false,
        );
    },
};

// disable_called=false: enable_mit_passthrough returned before yielding an active
// handle, so the collector has no active arms to disable. If the SDK later
// exposes a partial-enable shutdown receipt, replace this with explicit bounded
// shutdown reporting; do not claim disable was called without that receipt.

print_raw_clock_startup_stage("startup: reading active snapshot...");
let active_snapshot = match read_raw_clock_active_snapshot(&active, &raw_clock_settings) {
    Ok(snapshot) => snapshot,
    Err(error) => {
        let (_error_state, shutdown) = active.fault_shutdown(Duration::from_millis(20));
        let mut report = raw_clock_startup_fault_report(error);
        report.left_stop_attempt = shutdown.master_stop_attempt;
        report.right_stop_attempt = shutdown.slave_stop_attempt;
        return finish_writer_and_finalize(
            &context,
            &writer,
            EpisodeStatus::Faulted,
            &mut report,
            1,
            true,
            raw_can_recording,
        );
    },
};

print_raw_clock_startup_stage("startup: capturing active-zero calibration...");
let resolved_calibration = match resolve_raw_clock_active_calibration(
    &args,
    &active_snapshot,
    loaded_calibration.as_ref(),
    resolved_profile.runtime_mirror_map,
    started_unix_ns / 1_000_000,
    resolved_profile.profile.calibration.calibration_max_error_rad,
) {
    Ok(calibration) => calibration,
    Err(error) => {
        let (_error_state, shutdown) = active.fault_shutdown(Duration::from_millis(20));
        let mut report = raw_clock_startup_fault_report(error);
        report.left_stop_attempt = shutdown.master_stop_attempt;
        report.right_stop_attempt = shutdown.slave_stop_attempt;
        return finish_writer_and_finalize(
            &context,
            &writer,
            EpisodeStatus::Faulted,
            &mut report,
            1,
            true,
            raw_can_recording,
        );
    },
};
```

For the active failure branches above, the returned `error_state` only proves the bounded stop path executed; bind it to `_error_state` if it is unused. Every `finish_writer_and_finalize` call after `enable_mit_passthrough` returns `Ok(active)` must pass `enable_mit_calls = 1`; only failures before active ownership exists pass `0`.

Then implement `read_raw_clock_active_snapshot` in `collector.rs` as:

```rust
fn read_raw_clock_active_snapshot(
    active: &ExperimentalRawClockDualArmActive,
    settings: &crate::raw_clock::SvsRawClockSettings,
) -> Result<DualArmSnapshot> {
    active
        .snapshot(settings.read_policy())
        .context("failed to read raw-clock active snapshot")
}
```

Implement `resolve_raw_clock_active_calibration` in `collector.rs` by mirroring `resolve_real_calibration` but using the active `DualArmSnapshot` positions instead of `DualArmStandby::observer().snapshot(...)`. Return the existing `ResolvedRealCalibration` shape: `{ resolved: ResolvedCalibration, runtime: DualArmCalibration }`.

If `loaded_calibration` is `Some`, validate the post-enable active posture against the loaded calibration before returning it:

```rust
if let Some(loaded) = loaded_calibration {
    validate_raw_clock_calibration_posture(
        "post-enable",
        &loaded.resolved.calibration,
        active_snapshot,
        calibration_max_error_rad,
    )?;
    return Ok(loaded.clone());
}
```

If `args.calibration_file` is absent, capture:

```rust
let calibration = DualArmCalibration {
    master_zero: active_snapshot.left.state.position,
    slave_zero: active_snapshot.right.state.position,
    map: runtime_map,
};
let captured = calibration_file_from_dual_arm(&calibration, created_unix_ms);
let resolved = resolve_episode_calibration(None, runtime_map, Some(captured))?;
Ok(ResolvedRealCalibration {
    resolved,
    runtime: calibration,
})
```

In capture mode, atomically replace the provisional `calibration.toml` with this active-zero calibration, recompute the calibration hash, update the mutable raw-clock context/base calibration metadata, and rewrite the running manifest before starting the loop. Also write `args.save_calibration` after this capture/check. In loaded-calibration mode, keep the already-written loaded calibration artifact unchanged.

Build controller/loop config after calibration. These steps still happen with MIT enabled, so wrap errors and call `active.fault_shutdown(Duration::from_millis(20))` before finalization:

```rust
let sink = Arc::new(SvsTelemetrySink::new(
    Arc::clone(&stager),
    Arc::clone(&feedback_history),
    Arc::clone(&writer),
    raw_can.clone(),
    writer_backpressure_monitor_from_profile(&resolved_profile.profile),
    episode_start_host_mono_us,
));
let controller = match SvsController::with_shared(
    resolved_profile.profile.clone(),
    resolved_calibration.runtime,
    stager,
    dynamics_slot,
    feedback_history,
) {
    Ok(controller) => controller,
    Err(error) => {
        let (_error_state, shutdown) = active.fault_shutdown(Duration::from_millis(20));
        let mut report = raw_clock_startup_fault_report(error);
        report.left_stop_attempt = shutdown.master_stop_attempt;
        report.right_stop_attempt = shutdown.slave_stop_attempt;
        return finish_writer_and_finalize(
            &context,
            &writer,
            EpisodeStatus::Faulted,
            &mut report,
            1,
            true,
            raw_can_recording,
        );
    },
};
let telemetry_sink: Arc<dyn BilateralLoopTelemetrySink> = sink;
let loop_config = match bilateral_loop_config_from_profile(
    &resolved_profile.profile,
    &args,
    &cancel,
    telemetry_sink,
) {
    Ok(loop_config) => loop_config,
    Err(error) => {
        let (_error_state, shutdown) = active.fault_shutdown(Duration::from_millis(20));
        let mut report = raw_clock_startup_fault_report(error);
        report.left_stop_attempt = shutdown.master_stop_attempt;
        report.right_stop_attempt = shutdown.slave_stop_attempt;
        return finish_writer_and_finalize(
            &context,
            &writer,
            EpisodeStatus::Faulted,
            &mut report,
            1,
            true,
            raw_can_recording,
        );
    },
};
let run_config = raw_clock_settings.to_run_config(&loop_config);
if let Err(error) = run_config.validate() {
    let (_error_state, shutdown) = active.fault_shutdown(Duration::from_millis(20));
    let mut report = raw_clock_startup_fault_report(error);
    report.left_stop_attempt = shutdown.master_stop_attempt;
    report.right_stop_attempt = shutdown.slave_stop_attempt;
    return finish_writer_and_finalize(
        &context,
        &writer,
        EpisodeStatus::Faulted,
        &mut report,
        1,
        true,
        raw_can_recording,
    );
}

print_raw_clock_startup_stage("startup: starting SVS raw-clock bilateral loop...");
let loop_exit = match active.run_with_controller_and_compensation(controller, bridge, run_config) {
    Ok(exit) => exit,
    Err(error) => {
        debug_assert!(
            false,
            "raw-clock runtime returned an outer error after the collector pre-validated run_config"
        );
        let message = error.to_string();
        let mut report = raw_clock_startup_fault_report(&message);
        report.last_error = Some(format!(
            "raw-clock runtime returned an outer error after MIT enable: {message}"
        ));
        return finish_writer_and_finalize(
            &context,
            &writer,
            EpisodeStatus::Faulted,
            &mut report,
            1,
            false,
            raw_can_recording,
        );
    },
};
```

Task 3 must keep the SDK contract that runtime/read/submission/controller/compensation/telemetry/final-disable faults after MIT ownership transfer are returned as `ExperimentalRawClockRunExit::Faulted` with bounded stop attempts already recorded in the raw-clock report. The collector `Err` arm above is only a defensive guard for unexpected SDK API errors; because the active handle has already moved into the call, it must use `disable_called=false` and must not claim stop attempts unless the returned SDK error carries shutdown telemetry.

Map `ExperimentalRawClockRunExit` into `LoopOutcome` with a new helper:

```rust
fn loop_outcome_from_raw_clock(report: RawClockRuntimeReport) -> LoopOutcome {
    let status = match report.exit_reason {
        Some(RawClockRuntimeExitReason::MaxIterations) | None => EpisodeStatus::Complete,
        Some(RawClockRuntimeExitReason::Cancelled) => EpisodeStatus::Cancelled,
        Some(_) => EpisodeStatus::Faulted,
    };
    LoopOutcome {
        status,
        attempted_iterations: report.iterations as u64,
        loop_stopped_before_requested_iterations: status != EpisodeStatus::Complete,
        report: bilateral_report_from_raw_clock_for_svs(&report),
        disable_called: true,
    }
}
```

Create `bilateral_report_from_raw_clock_for_svs` in `collector.rs` so existing finalization still writes `dual_arm`:

```rust
fn bilateral_report_from_raw_clock_for_svs(report: &RawClockRuntimeReport) -> BilateralRunReport {
    BilateralRunReport {
        iterations: report.iterations,
        read_faults: report.read_faults,
        submission_faults: report.submission_faults,
        peer_command_may_have_applied: report.peer_command_may_have_applied,
        max_inter_arm_skew: Duration::from_micros(report.max_inter_arm_skew_us),
        left_tx_realtime_overwrites_total: report.master_tx_realtime_overwrites_total,
        right_tx_realtime_overwrites_total: report.slave_tx_realtime_overwrites_total,
        left_tx_frames_sent_total: report.master_tx_frames_sent_total,
        right_tx_frames_sent_total: report.slave_tx_frames_sent_total,
        left_tx_fault_aborts_total: report.master_tx_fault_aborts_total,
        right_tx_fault_aborts_total: report.slave_tx_fault_aborts_total,
        last_runtime_fault_left: report.last_runtime_fault_master,
        last_runtime_fault_right: report.last_runtime_fault_slave,
        exit_reason: report.exit_reason.map(raw_clock_exit_to_bilateral_exit),
        left_stop_attempt: report.master_stop_attempt,
        right_stop_attempt: report.slave_stop_attempt,
        last_error: report.last_error.clone(),
        ..BilateralRunReport::default()
    }
}
```

Add `raw_clock_exit_to_bilateral_exit` mapping all variants.

Declare the raw-clock collector context binding mutable. Before finalizing, store `context.raw_clock_report = Some(report.clone())`, then call the existing `finish_writer_and_finalize` path. Do not add a second finalization API for raw-clock.

- [ ] **Step 8: Run cargo check**

Run:

```bash
cargo test --manifest-path addons/piper-svs-collect/Cargo.toml raw_clock_run_config_copies_loop_config_and_disables_gripper_mirror -- --nocapture
cargo check --manifest-path addons/piper-svs-collect/Cargo.toml --all-targets
cargo check -p piper-client --all-targets
```

Expected: PASS.

- [ ] **Step 9: Commit**

```bash
git add addons/piper-svs-collect/src/collector.rs addons/piper-svs-collect/src/raw_clock.rs crates/piper-client/src/dual_arm_raw_clock.rs
git commit -m "Wire SVS raw-clock collector backend"
```

## Task 8: Add Fake Harness Coverage For Raw-Clock Metadata And Faults

**Files:**
- Modify: `addons/piper-svs-collect/src/collector.rs`
- Modify: `addons/piper-svs-collect/tests/collector_fake.rs`

- [ ] **Step 1: Extend fake harness with runtime kind**

In `FakeCollectorHarness`, add:

```rust
use piper_client::dual_arm::StopAttemptResult;
use piper_client::dual_arm_raw_clock::{RawClockRuntimeExitReason, RawClockRuntimeReport};
use piper_tools::raw_clock::RawClockHealth;
```

```rust
runtime_kind: crate::raw_clock::SvsRuntimeKind,
raw_clock_report: Option<piper_client::dual_arm_raw_clock::RawClockRuntimeReport>,
raw_clock_loaded_calibration_pre_enable_mismatch: bool,
raw_clock_loaded_calibration_post_enable_mismatch: bool,
raw_clock_gripper_mirror_enabled: bool,
```

Default:

```rust
runtime_kind: crate::raw_clock::SvsRuntimeKind::StrictRealtime,
raw_clock_report: None,
raw_clock_loaded_calibration_pre_enable_mismatch: false,
raw_clock_loaded_calibration_post_enable_mismatch: false,
raw_clock_gripper_mirror_enabled: false,
```

Add builder methods:

```rust
pub fn with_raw_clock_runtime(mut self) -> Self {
    self.runtime_kind = crate::raw_clock::SvsRuntimeKind::CalibratedRawClock;
    self.raw_clock_report = None;
    self
}

pub fn with_raw_clock_runtime_before_loop(mut self) -> Self {
    self.runtime_kind = crate::raw_clock::SvsRuntimeKind::CalibratedRawClock;
    self.raw_clock_report = None;
    self
}

pub fn with_raw_clock_fault_report(
    mut self,
    report: piper_client::dual_arm_raw_clock::RawClockRuntimeReport,
) -> Self {
    self.runtime_kind = crate::raw_clock::SvsRuntimeKind::CalibratedRawClock;
    self.raw_clock_report = Some(report);
    self
}

pub fn with_raw_clock_loaded_calibration_pre_enable_mismatch(mut self) -> Self {
    self.runtime_kind = crate::raw_clock::SvsRuntimeKind::CalibratedRawClock;
    self.raw_clock_loaded_calibration_pre_enable_mismatch = true;
    self
}

pub fn with_raw_clock_loaded_calibration_post_enable_mismatch(mut self) -> Self {
    self.runtime_kind = crate::raw_clock::SvsRuntimeKind::CalibratedRawClock;
    self.raw_clock_loaded_calibration_post_enable_mismatch = true;
    self
}

pub fn with_raw_clock_gripper_mirror_enabled(mut self) -> Self {
    self.runtime_kind = crate::raw_clock::SvsRuntimeKind::CalibratedRawClock;
    self.raw_clock_gripper_mirror_enabled = true;
    self
}
```

Add private helpers `raw_clock_health_for_fake()` and `raw_clock_report_for_fake(iterations, exit_reason)` in `addons/piper-svs-collect/src/collector.rs` for the fake harness' internal report synthesis. Keep both helpers private to `collector.rs`; they are not part of the production library API. The regular raw-clock fake path should synthesize this report from the fake loop outcome; only `with_raw_clock_fault_report(...)` should force a prebuilt report override. Use this exact helper shape so the fake harness does not need to infer the large report literal inline:

```rust
fn raw_clock_health_for_fake() -> piper_tools::raw_clock::RawClockHealth {
    piper_tools::raw_clock::RawClockHealth {
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

fn raw_clock_report_for_fake(
    iterations: usize,
    exit_reason: RawClockRuntimeExitReason,
) -> RawClockRuntimeReport {
    fn flag(value: bool) -> u32 {
        if value { 1 } else { 0 }
    }

    RawClockRuntimeReport {
        master: raw_clock_health_for_fake(),
        slave: raw_clock_health_for_fake(),
        joint_motion: None,
        max_inter_arm_skew_us: 2_000,
        inter_arm_skew_p95_us: 1_000,
        alignment_lag_us: 5_000,
        latest_inter_arm_skew_max_us: 2_000,
        latest_inter_arm_skew_p95_us: 1_000,
        selected_inter_arm_skew_max_us: 2_000,
        selected_inter_arm_skew_p95_us: 1_000,
        clock_health_failures: 0,
        compensation_faults: flag(exit_reason == RawClockRuntimeExitReason::CompensationFault),
        controller_faults: flag(exit_reason == RawClockRuntimeExitReason::ControllerFault),
        telemetry_sink_faults: flag(exit_reason == RawClockRuntimeExitReason::TelemetrySinkFault),
        alignment_buffer_misses: 0,
        alignment_buffer_miss_consecutive_max: 0,
        alignment_buffer_miss_consecutive_failures: 0,
        master_residual_max_spikes: 0,
        slave_residual_max_spikes: 0,
        master_residual_max_consecutive_failures: 0,
        slave_residual_max_consecutive_failures: 0,
        read_faults: flag(exit_reason == RawClockRuntimeExitReason::ReadFault),
        submission_faults: flag(exit_reason == RawClockRuntimeExitReason::SubmissionFault),
        last_submission_failed_side: None,
        peer_command_may_have_applied: false,
        runtime_faults: flag(matches!(
            exit_reason,
            RawClockRuntimeExitReason::RuntimeTransportFault
                | RawClockRuntimeExitReason::RuntimeManualFault
        )),
        master_tx_realtime_overwrites_total: 0,
        slave_tx_realtime_overwrites_total: 0,
        master_tx_frames_sent_total: iterations as u64,
        slave_tx_frames_sent_total: iterations as u64,
        master_tx_fault_aborts_total: 0,
        slave_tx_fault_aborts_total: 0,
        last_runtime_fault_master: None,
        last_runtime_fault_slave: None,
        iterations,
        exit_reason: Some(exit_reason),
        master_stop_attempt: StopAttemptResult::NotAttempted,
        slave_stop_attempt: StopAttemptResult::NotAttempted,
        last_error: (exit_reason != RawClockRuntimeExitReason::MaxIterations)
            .then(|| format!("{exit_reason:?}")),
    }
}
```

- [ ] **Step 2: Write failing fake tests**

In `collector_fake.rs`, add:

```rust
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
    let manifest = read_manifest_toml(&result.path.join("manifest.toml")).unwrap();
    assert!(manifest.raw_clock.is_some());
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
    assert_eq!(
        report.raw_clock.as_ref().unwrap().final_failure_kind.as_deref(),
        Some("Cancelled")
    );
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
    assert_eq!(
        report.raw_clock.as_ref().unwrap().final_failure_kind.as_deref(),
        Some("Cancelled")
    );
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
    let steps = read_steps_file(result.path.join("steps.bin")).unwrap();
    assert_eq!(steps.steps.len(), 1);
    let report = read_report_json(&result.path.join("report.json")).unwrap();
    assert_eq!(report.status, EpisodeStatus::Faulted);
    assert!(report.raw_clock.is_some());
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
    assert!(report.raw_clock.is_some());
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
    assert!(report.dual_arm.last_error.as_deref().unwrap_or_default().contains("post-enable"));
    assert!(report.raw_clock.is_some());
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

    let result = harness.run(out.path()).expect("collector should reject unsupported gripper mirror");

    assert_eq!(result.status, EpisodeStatus::Faulted);
    assert_eq!(result.enable_mit_calls, 0);
    assert!(!result.disable_called);
    let report = read_report_json(&result.path.join("report.json")).unwrap();
    assert!(report
        .dual_arm
        .last_error
        .as_deref()
        .unwrap_or_default()
        .contains("--disable-gripper-mirror"));
    assert!(report.raw_clock.is_some());
}
```

- [ ] **Step 3: Run tests to verify RED**

Run:

```bash
cargo test --manifest-path addons/piper-svs-collect/Cargo.toml raw_clock_fake_workflow_writes_optional_raw_clock_sections -- --nocapture
cargo test --manifest-path addons/piper-svs-collect/Cargo.toml strict_fake_workflow_keeps_raw_clock_sections_absent -- --nocapture
cargo test --manifest-path addons/piper-svs-collect/Cargo.toml raw_clock_cancel_before_enable_still_writes_raw_clock_report_section -- --nocapture
cargo test --manifest-path addons/piper-svs-collect/Cargo.toml raw_clock_cancel_during_active_control_finalizes_partial_episode -- --nocapture
cargo test --manifest-path addons/piper-svs-collect/Cargo.toml raw_clock_gripper_telemetry_maps_into_svs_step -- --nocapture
cargo test --manifest-path addons/piper-svs-collect/Cargo.toml raw_clock_fake_workflow_writes_compensation_and_dynamics_steps -- --nocapture
cargo test --manifest-path addons/piper-svs-collect/Cargo.toml raw_clock_missing_tx_finished_finalizes_faulted_episode -- --nocapture
cargo test --manifest-path addons/piper-svs-collect/Cargo.toml raw_clock_loaded_calibration_pre_enable_mismatch_fails_before_enable -- --nocapture
cargo test --manifest-path addons/piper-svs-collect/Cargo.toml raw_clock_loaded_calibration_post_enable_mismatch_disables_without_loop -- --nocapture
cargo test --manifest-path addons/piper-svs-collect/Cargo.toml raw_clock_capture_replaces_provisional_calibration_with_active_zero_and_save_copy -- --nocapture
cargo test --manifest-path addons/piper-svs-collect/Cargo.toml raw_clock_gripper_mirror_enabled_rejects_before_enable -- --nocapture
```

Expected: raw-clock tests FAIL until fake finalization writes raw-clock sections for both runtime-report and pre-loop cases.

- [ ] **Step 4: Thread fake raw-clock metadata into manifest/report**

In `FakeManifestWrite`, add:

```rust
raw_clock: Option<RawClockManifest>,
```

In `FakeReportWrite`, add:

```rust
raw_clock_report: Option<RawClockRuntimeReport>,
raw_clock_settings: Option<crate::raw_clock::SvsRawClockSettings>,
```

For capture-mode calibration replacement, extend the fake harness with explicit startup and active snapshots plus the optional save-calibration path:

```rust
raw_clock_startup_positions: Option<([f64; 6], [f64; 6])>,
raw_clock_active_positions: Option<([f64; 6], [f64; 6])>,
save_calibration_path: Option<PathBuf>,
```

Add builder helpers:

```rust
fn with_raw_clock_capture_positions(
    mut self,
    startup_master: [f64; 6],
    startup_slave: [f64; 6],
    active_master: [f64; 6],
    active_slave: [f64; 6],
) -> Self {
    self.raw_clock_startup_positions = Some((startup_master, startup_slave));
    self.raw_clock_active_positions = Some((active_master, active_slave));
    self
}

fn with_save_calibration_path(mut self, path: PathBuf) -> Self {
    self.save_calibration_path = Some(path);
    self
}
```

When `runtime_kind == CalibratedRawClock` and no loaded calibration is configured, the fake path must first write `calibration.toml` and the initial manifest from `raw_clock_startup_positions` or default zeros, then after the fake MIT-enable stage replace `calibration.toml` with a calibration built from `raw_clock_active_positions`, recompute `sha256_hex(&active_bytes)`, update `manifest.calibration.master_zero_rad`, `manifest.calibration.slave_zero_rad`, and `manifest.calibration.sha256_hex`, rewrite `manifest.toml`, and persist the same canonical active bytes to `save_calibration_path` using the same no-overwrite helper used by the real path. This mirrors the real collector's provisional-startup artifact and post-enable active-zero replacement.

When `runtime_kind == CalibratedRawClock`, populate these from default resolved fake settings:

```rust
let fake_raw_clock_settings =
    (input.runtime_kind == crate::raw_clock::SvsRuntimeKind::CalibratedRawClock).then(|| {
        let raw = &fake_profile.raw_clock;
        crate::raw_clock::SvsRawClockSettings {
            warmup_secs: raw.warmup_secs,
            residual_p95_us: raw.residual_p95_us,
            residual_max_us: raw.residual_max_us,
            drift_abs_ppm: raw.drift_abs_ppm,
            sample_gap_max_ms: raw.sample_gap_max_ms,
            last_sample_age_ms: raw.last_sample_age_ms,
            selected_sample_age_ms: raw.selected_sample_age_ms,
            inter_arm_skew_max_us: raw.inter_arm_skew_max_us,
            state_skew_max_us: raw.state_skew_max_us,
            residual_max_consecutive_failures: raw.residual_max_consecutive_failures,
            alignment_lag_us: raw.alignment_lag_us,
            alignment_search_window_us: raw.alignment_search_window_us,
            alignment_buffer_miss_consecutive_failures:
                raw.alignment_buffer_miss_consecutive_failures,
        }
    });
```

Use the same conversion helpers as real finalization:

```rust
let synthesized_raw_clock_report = if input.runtime_kind
    == crate::raw_clock::SvsRuntimeKind::CalibratedRawClock
{
    let reason = match dual_arm_exit_reason {
        Some(BilateralExitReason::Cancelled) => RawClockRuntimeExitReason::Cancelled,
        Some(BilateralExitReason::TelemetrySinkFault) => {
            RawClockRuntimeExitReason::TelemetrySinkFault
        },
        Some(BilateralExitReason::CompensationFault) => RawClockRuntimeExitReason::CompensationFault,
        Some(BilateralExitReason::ControllerFault) => RawClockRuntimeExitReason::ControllerFault,
        Some(BilateralExitReason::SubmissionFault) => RawClockRuntimeExitReason::SubmissionFault,
        Some(BilateralExitReason::RuntimeTransportFault) => {
            RawClockRuntimeExitReason::RuntimeTransportFault
        },
        Some(_) => RawClockRuntimeExitReason::RuntimeTransportFault,
        None if status == EpisodeStatus::Complete => RawClockRuntimeExitReason::MaxIterations,
        None if status == EpisodeStatus::Cancelled => RawClockRuntimeExitReason::Cancelled,
        None => RawClockRuntimeExitReason::RuntimeTransportFault,
    };
    Some(raw_clock_report_for_fake(input.attempted_iterations as usize, reason))
} else {
    None
};

let raw_clock_report = input
    .raw_clock_report
    .as_ref()
    .or(synthesized_raw_clock_report.as_ref());

let fake_raw_clock_json = match (raw_clock_report, fake_raw_clock_settings.as_ref()) {
    (Some(report), Some(settings)) => {
        Some(crate::raw_clock::raw_clock_report_json(report, settings))
    },
    (None, Some(settings)) if input.runtime_kind == crate::raw_clock::SvsRuntimeKind::CalibratedRawClock => {
        let final_failure_kind = dual_arm_exit_reason
            .map(|reason| format!("{reason:?}"))
            .or_else(|| report_last_error.clone());
        Some(crate::raw_clock::raw_clock_startup_report_json(
            settings,
            final_failure_kind,
        ))
    },
    _ => None,
};
```

This prevents `with_raw_clock_runtime()` from masking later fake loop faults such as missing tx-finished timestamps; telemetry sink faults must synthesize a `TelemetrySinkFault` raw-clock report.

The fake StrictRealtime path must continue writing `None`.

For the two loaded-calibration mismatch harness flags, make the fake raw-clock path finalize exactly like the real path:

- `raw_clock_loaded_calibration_pre_enable_mismatch`: fault before incrementing `enable_mit_calls`; write manifest/report with `raw_clock` sections and an error string containing `pre-enable`.
- `raw_clock_loaded_calibration_post_enable_mismatch`: increment `enable_mit_calls`, set `disable_called = true`, skip loop iterations, write manifest/report with `raw_clock` sections and an error string containing `post-enable`.
- `raw_clock_gripper_mirror_enabled`: fault before incrementing `enable_mit_calls`; write manifest/report with `raw_clock` sections and an error string containing `--disable-gripper-mirror`.

For raw-clock fake runs, keep using the existing `telemetry_for_fake_control_frame`/`SvsTelemetrySink` path so `with_master_tx_finished_timeout_at_step` and gripper timing tests exercise the same tx-finished and `SvsGripperStepV1` code as StrictRealtime.

- [ ] **Step 5: Run fake collector tests**

Run:

```bash
cargo test --manifest-path addons/piper-svs-collect/Cargo.toml --test collector_fake -- --nocapture
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add addons/piper-svs-collect/src/collector.rs addons/piper-svs-collect/tests/collector_fake.rs
git commit -m "Cover SVS raw-clock episode metadata"
```

## Task 9: Add SVS Raw-Clock Fault Mapping Tests

**Files:**
- Modify: `addons/piper-svs-collect/src/collector.rs`
- Modify: `addons/piper-svs-collect/tests/collector_fake.rs`

- [ ] **Step 1: Write failing compensation/telemetry fault tests**

In `collector.rs`, add direct sink contract coverage:

```rust
#[test]
fn svs_sink_rejects_missing_or_zero_tx_finished_timestamps() {
    assert!(matches!(
        required_tx_finished("master", None),
        Err(SvsTelemetrySinkFault::MissingTxFinished { arm: "master" })
    ));
    assert!(matches!(
        required_tx_finished("slave", Some(0)),
        Err(SvsTelemetrySinkFault::MissingTxFinished { arm: "slave" })
    ));
    assert_eq!(required_tx_finished("master", Some(1)).unwrap(), 1);
}
```

In `collector_fake.rs`, add:

```rust
#[test]
fn raw_clock_compensation_fault_finalizes_faulted_episode() {
    let out = tempfile::tempdir().unwrap();
    let mut report = raw_clock_report_for_fake_tests(0);
    report.exit_reason = Some(piper_client::dual_arm_raw_clock::RawClockRuntimeExitReason::CompensationFault);
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
    let mut report = raw_clock_report_for_fake_tests(1);
    report.exit_reason = Some(piper_client::dual_arm_raw_clock::RawClockRuntimeExitReason::TelemetrySinkFault);
    report.telemetry_sink_faults = 1;
    report.last_error = Some("sink failed".to_string());

    let harness = FakeCollectorHarness::new()
        .with_two_socketcan_targets()
        .with_fake_mujoco()
        .with_raw_clock_fault_report(report);

    let result = harness.run(out.path()).expect("collector should finalize fault");

    assert_eq!(result.status, EpisodeStatus::Faulted);
    let report = read_report_json(&result.path.join("report.json")).unwrap();
    assert_eq!(report.raw_clock.as_ref().unwrap().telemetry_sink_faults, 1);
}
```

- [ ] **Step 2: Run tests to verify RED**

Run:

```bash
cargo test --manifest-path addons/piper-svs-collect/Cargo.toml raw_clock_compensation_fault_finalizes_faulted_episode -- --nocapture
cargo test --manifest-path addons/piper-svs-collect/Cargo.toml raw_clock_telemetry_sink_fault_finalizes_faulted_episode -- --nocapture
```

Expected: FAIL until fake harness maps raw-clock report exit reasons into status/report.

- [ ] **Step 3: Add integration-test local fake report helper**

In `addons/piper-svs-collect/tests/collector_fake.rs`, add local test helpers instead of exposing the private `collector.rs` fake construction helpers from the production library API. This is a deliberate small duplication: `collector.rs::raw_clock_report_for_fake` stays private for internal fake harness synthesis, while `collector_fake.rs::raw_clock_report_for_fake_tests` is only for integration tests that inject explicit raw-clock fault reports.

```rust
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
```

Do not add `raw_clock_report_for_fake_tests` to `collector.rs` or `lib.rs`, and do not expose either fake helper from the production crate.

- [ ] **Step 4: Map raw-clock status in fake harness**

When fake raw-clock report is present, derive status from `report.exit_reason`:

```rust
let raw_status = match report.exit_reason {
    Some(RawClockRuntimeExitReason::MaxIterations) | None => EpisodeStatus::Complete,
    Some(RawClockRuntimeExitReason::Cancelled) => EpisodeStatus::Cancelled,
    Some(_) => EpisodeStatus::Faulted,
};
```

Use `bilateral_report_from_raw_clock_for_svs(&report)` to populate `dual_arm`.

- [ ] **Step 5: Run tests to GREEN**

Run:

```bash
cargo test --manifest-path addons/piper-svs-collect/Cargo.toml raw_clock_compensation_fault_finalizes_faulted_episode -- --nocapture
cargo test --manifest-path addons/piper-svs-collect/Cargo.toml raw_clock_telemetry_sink_fault_finalizes_faulted_episode -- --nocapture
cargo test --manifest-path addons/piper-svs-collect/Cargo.toml --test collector_fake -- --nocapture
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add addons/piper-svs-collect/src/collector.rs addons/piper-svs-collect/tests/collector_fake.rs
git commit -m "Map SVS raw-clock fault reports"
```

## Task 10: Final Validation And Hardware Runbook Notes

**Files:**
- Modify: `addons/piper-svs-collect/README.md`

- [ ] **Step 1: Add README usage example**

Append a short experimental raw-clock section:

```markdown
## Experimental Calibrated Raw-Clock SVS

Use this only after the general teleop raw-clock smoke script is stable on the same interfaces.

```bash
cargo run --manifest-path addons/piper-svs-collect/Cargo.toml -- \
  --master-target socketcan:can1 \
  --slave-target socketcan:can0 \
  --use-embedded-model \
  --output-dir artifacts/svs \
  --mirror-map identity \
  --disable-gripper-mirror \
  --experimental-calibrated-raw \
  --raw-clock-warmup-secs 10 \
  --raw-clock-residual-p95-us 2000 \
  --raw-clock-residual-max-us 3000 \
  --raw-clock-residual-max-consecutive-failures 3 \
  --raw-clock-sample-gap-max-ms 50 \
  --raw-clock-inter-arm-skew-max-us 20000 \
  --raw-clock-state-skew-max-us 10000 \
  --raw-clock-alignment-lag-us 5000 \
  --raw-clock-alignment-search-window-us 25000
```

The raw-clock backend is explicit and never falls back to StrictRealtime. The first implementation requires `--disable-gripper-mirror`.
```

- [ ] **Step 2: Run formatting**

Run:

```bash
cargo fmt --all
cargo fmt --manifest-path addons/piper-svs-collect/Cargo.toml --all
```

Expected: both commands exit 0. The second command is required because `addons/piper-svs-collect` is excluded from the root workspace.

- [ ] **Step 3: Run client checks**

Run:

```bash
cargo test -p piper-client dual_arm_raw_clock -- --test-threads=1 --nocapture
cargo test -p piper-client run_bilateral_with_compensation -- --test-threads=1 --nocapture
```

Expected: PASS.

- [ ] **Step 4: Run CLI compatibility checks**

Run:

```bash
cargo test -p piper-cli raw_clock -- --nocapture
```

Expected: PASS.

- [ ] **Step 5: Run SVS addon checks**

Run:

```bash
cargo test --manifest-path addons/piper-svs-collect/Cargo.toml --all-targets -- --nocapture
```

Expected: PASS.

- [ ] **Step 6: Run default workspace checks**

Run:

```bash
cargo check --all-targets
cargo test --lib
```

Expected: PASS and no MuJoCo dependency is pulled into normal workspace commands.

- [ ] **Step 7: Commit**

```bash
git add addons/piper-svs-collect/README.md
git commit -m "Document SVS raw-clock collection"
```

## Hardware Acceptance

Run after all automated checks pass.

1. Baseline general teleop smoke:

```bash
MASTER_IFACE=can1 SLAVE_IFACE=can0 MASTER_DAMPING=0.05 MAX_ITERATIONS=30000 ./scripts/run_teleop_smoke.sh
```

Expected:
- clean max-iteration exit
- `read_faults == 0`
- `submission_faults == 0`
- `clock_health_failures == 0`

2. Short SVS raw-clock run:

```bash
cargo run --manifest-path addons/piper-svs-collect/Cargo.toml -- \
  --master-target socketcan:can1 \
  --slave-target socketcan:can0 \
  --use-embedded-model \
  --output-dir artifacts/svs \
  --mirror-map identity \
  --disable-gripper-mirror \
  --experimental-calibrated-raw \
  --max-iterations 1000 \
  --raw-clock-warmup-secs 10 \
  --raw-clock-residual-p95-us 2000 \
  --raw-clock-residual-max-us 3000 \
  --raw-clock-residual-max-consecutive-failures 3 \
  --raw-clock-sample-gap-max-ms 50 \
  --raw-clock-inter-arm-skew-max-us 20000 \
  --raw-clock-state-skew-max-us 10000 \
  --raw-clock-alignment-lag-us 5000 \
  --raw-clock-alignment-search-window-us 25000
```

Expected:
- visible startup stage markers
- `manifest.toml` contains `[raw_clock]`
- `report.json` contains `"raw_clock"`
- step file decodes with unchanged `SvsEpisodeV1`

3. 30k iteration SVS raw-clock run with light motion.

Expected:
- complete max-iteration exit
- `raw_clock.read_faults == 0`
- `raw_clock.submission_faults == 0`
- `raw_clock.clock_health_failures == 0`
- `raw_clock.compensation_faults == 0`
- `raw_clock.telemetry_sink_faults == 0`

4. 10 minute SVS raw-clock run.

Expected:
- episode finalizes on clean, faulted, or Ctrl-C paths
- manifest/report remain readable through `read_manifest_toml` and `read_report_json`

## Completion Criteria

- `piper-client` raw-clock runtime can run with optional compensation and telemetry sink.
- Existing raw-clock master-follower/bilateral CLI behavior still passes tests when no compensator/telemetry sink is configured.
- SVS raw-clock backend is explicit, SoftRealtime-only, and never silently falls back.
- SVS raw-clock writes optional manifest/report raw-clock sections.
- `SvsEpisodeV1` per-step schema is unchanged.
- Raw-clock gripper mirroring is rejected before enabling arms until implemented.
- All automated validation commands in Task 10 pass.
