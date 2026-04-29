# Raw-Clock Bilateral Teleop Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Enable low-gain bilateral teleop on the existing calibrated raw-clock SoftRealtime path while preserving the stable master-follower baseline.

**Architecture:** Reuse the current raw-clock runtime loop, which is already generic over `BilateralController`. Make the existing controller-based raw-clock active runner public, flow the resolved teleop mode into raw-clock config/warmup, and have the CLI raw-clock workflow run `RuntimeTeleopController` instead of a hard-coded master-follower controller. Keep gravity compensation out of this change.

**Tech Stack:** Rust 2024, `piper-client` raw-clock runtime, `piper-cli` teleop workflow/config/reporting, `clap`, `serde`, Bash smoke script, cargo unit tests.

---

## Source Spec

- `docs/superpowers/specs/2026-04-30-raw-clock-bilateral-teleop-design.md`

## File Structure

- `crates/piper-client/src/dual_arm_raw_clock.rs`
  - Owns experimental raw-clock runtime config validation and active runtime entry points.
  - Change only the bilateral mode rejection and the visibility of the existing controller-based run method.
- `apps/cli/src/teleop/config.rs`
  - Owns CLI/file resolution and validation.
  - Remove the `experimental_calibrated_raw + bilateral` rejection and invert the existing rejection test.
- `apps/cli/src/teleop/workflow.rs`
  - Owns experimental raw-clock workflow, backend trait, fake backend tests, and conversion to `ExperimentalRawClockConfig`.
  - Flow `TeleopMode` into warmup config, rename the raw-clock backend run method, and use `RuntimeTeleopController`.
- `apps/cli/src/teleop/report.rs`
  - Owns JSON report construction and human report rendering.
  - Add regression coverage that human output prints `mode: bilateral -> bilateral`.
- `scripts/run_teleop_smoke.sh`
  - Owns the one-command hardware smoke flow.
  - Add explicit `REFLECTION_GAIN` plumbing and bilateral-specific safety text.
- No new runtime modules.
- No gravity compensation changes.

## Task 1: Client Raw-Clock Runtime Accepts Bilateral Mode

**Files:**
- Modify: `crates/piper-client/src/dual_arm_raw_clock.rs`

- [ ] **Step 1: Write the failing client config test**

Replace the existing `experimental_config_rejects_bilateral_mode` test with:

```rust
#[test]
fn experimental_config_accepts_bilateral_mode() {
    let config = ExperimentalRawClockConfig::default()
        .with_mode(ExperimentalRawClockMode::Bilateral)
        .expect("bilateral should be accepted for experimental raw-clock runtime");

    assert_eq!(config.mode, ExperimentalRawClockMode::Bilateral);
    config.validate().expect("bilateral raw-clock config should validate");
}
```

- [ ] **Step 2: Run the test to verify RED**

Run:

```bash
cargo test -p piper-client experimental_config_accepts_bilateral_mode -- --test-threads=1 --nocapture
```

Expected: FAIL because `with_mode(ExperimentalRawClockMode::Bilateral)` currently returns a config error mentioning `master-follower`.

- [ ] **Step 3: Implement the minimal client runtime change**

In `ExperimentalRawClockConfig::with_mode`, remove the early rejection:

```rust
pub fn with_mode(
    mut self,
    mode: ExperimentalRawClockMode,
) -> Result<Self, RawClockRuntimeError> {
    self.mode = mode;
    Ok(self)
}
```

In `ExperimentalRawClockConfig::validate`, remove only the mode rejection block:

```rust
if matches!(self.mode, ExperimentalRawClockMode::Bilateral) {
    return Err(RawClockRuntimeError::Config(
        "experimental raw-clock runtime currently supports master-follower mode only".to_string(),
    ));
}
```

Make the existing private active runner public. Change:

```rust
fn run_with_controller<C>(
```

to:

```rust
pub fn run_with_controller<C>(
```

Do not duplicate `run_raw_clock_runtime_core` plumbing.

- [ ] **Step 4: Run the client tests to verify GREEN**

Run:

```bash
cargo test -p piper-client experimental_config_accepts_bilateral_mode -- --test-threads=1 --nocapture
cargo test -p piper-client run_raw_clock_runtime_core -- --test-threads=1 --nocapture
```

Expected: PASS. If the second command matches no tests, run:

```bash
cargo test -p piper-client dual_arm_raw_clock -- --test-threads=1 --nocapture
```

- [ ] **Step 5: Commit**

```bash
git add crates/piper-client/src/dual_arm_raw_clock.rs
git commit -m "Allow raw-clock bilateral runtime mode"
```

## Task 2: CLI Config And Warmup Carry Bilateral Mode

**Files:**
- Modify: `apps/cli/src/teleop/config.rs`
- Modify: `apps/cli/src/teleop/workflow.rs`

- [ ] **Step 1: Write the failing CLI config test**

Replace `experimental_raw_clock_rejects_bilateral_mode` in `apps/cli/src/teleop/config.rs` with:

```rust
#[test]
fn experimental_raw_clock_accepts_bilateral_mode() {
    let args = TeleopDualArmArgs {
        experimental_calibrated_raw: true,
        mode: Some(TeleopMode::Bilateral),
        ..TeleopDualArmArgs::default_for_tests()
    };

    let resolved = ResolvedTeleopConfig::resolve(args, None)
        .expect("experimental raw-clock should accept bilateral mode");

    assert_eq!(resolved.control.mode, TeleopMode::Bilateral);
    assert!(resolved.raw_clock.experimental_calibrated_raw);
}
```

- [ ] **Step 2: Run the CLI config test to verify RED**

Run:

```bash
cargo test -p piper-cli experimental_raw_clock_accepts_bilateral_mode -- --nocapture
```

Expected: FAIL at runtime because config still rejects bilateral with `experimental calibrated raw clock requires master-follower mode`.

- [ ] **Step 3: Write the failing raw-clock config mapping test**

In `apps/cli/src/teleop/workflow.rs`, add near `experimental_raw_clock_settings_convert_to_runtime_thresholds`:

```rust
#[test]
fn experimental_raw_clock_config_maps_bilateral_mode() {
    let settings = TeleopRawClockSettings {
        experimental_calibrated_raw: true,
        warmup_secs: 10,
        residual_p95_us: 500,
        residual_max_us: 2000,
        drift_abs_ppm: 500.0,
        sample_gap_max_ms: 20,
        last_sample_age_ms: 20,
        inter_arm_skew_max_us: 20_000,
        state_skew_max_us: 10_000,
        selected_sample_age_ms: 50,
        residual_max_consecutive_failures: 3,
        alignment_lag_us: 5_000,
        alignment_search_window_us: 25_000,
        alignment_buffer_miss_consecutive_failures: 3,
    };

    let config =
        experimental_raw_clock_config_from_settings(&settings, TeleopMode::Bilateral, 100.0, None);

    assert_eq!(config.mode, ExperimentalRawClockMode::Bilateral);
}
```

- [ ] **Step 4: Run the raw-clock config mapping test to verify RED**

Run:

```bash
cargo test -p piper-cli experimental_raw_clock_config_maps_bilateral_mode -- --nocapture
```

Expected: FAIL to compile or run because `experimental_raw_clock_config_from_settings` does not yet accept `TeleopMode`.

- [ ] **Step 5: Remove the CLI config rejection**

In `ResolvedTeleopConfig::resolve`, delete:

```rust
if args.experimental_calibrated_raw && control.mode != TeleopMode::MasterFollower {
    bail!("experimental calibrated raw clock requires master-follower mode");
}
```

Keep all existing gain validation intact.

- [ ] **Step 6: Add mode mapping for raw-clock runtime config**

In `apps/cli/src/teleop/workflow.rs`, add:

```rust
fn experimental_raw_clock_mode_from_teleop_mode(mode: TeleopMode) -> ExperimentalRawClockMode {
    match mode {
        TeleopMode::MasterFollower => ExperimentalRawClockMode::MasterFollower,
        TeleopMode::Bilateral => ExperimentalRawClockMode::Bilateral,
    }
}
```

Change the config converter signature from:

```rust
fn experimental_raw_clock_config_from_settings(
    raw_clock: &TeleopRawClockSettings,
    frequency_hz: f64,
    max_iterations: Option<usize>,
) -> ExperimentalRawClockConfig
```

to:

```rust
fn experimental_raw_clock_config_from_settings(
    raw_clock: &TeleopRawClockSettings,
    mode: TeleopMode,
    frequency_hz: f64,
    max_iterations: Option<usize>,
) -> ExperimentalRawClockConfig
```

Set:

```rust
ExperimentalRawClockConfig {
    mode: experimental_raw_clock_mode_from_teleop_mode(mode),
    frequency_hz,
    max_iterations,
    thresholds,
    estimator_thresholds,
}
```

- [ ] **Step 7: Flow mode through warmup**

Change the `ExperimentalRawClockTeleopBackend::warmup_raw_clock` trait signature to include mode:

```rust
fn warmup_raw_clock(
    &mut self,
    settings: &TeleopRawClockSettings,
    mode: TeleopMode,
    frequency_hz: f64,
    max_iterations: Option<usize>,
    cancel_signal: Arc<AtomicBool>,
) -> Result<RawClockWarmupSummary>;
```

Update both calls in `run_experimental_raw_clock_workflow`:

```rust
backend.warmup_raw_clock(
    &resolved.raw_clock,
    resolved.control.mode,
    resolved.control.frequency_hz,
    resolved.max_iterations,
    cancel_signal.clone(),
)
```

Update `RealTeleopBackend::warmup_raw_clock` and the fake backend implementation with the same mode parameter. In the real backend, call:

```rust
let config = experimental_raw_clock_config_from_settings(
    settings,
    mode,
    frequency_hz,
    max_iterations,
);
```

The fake backend can accept `_mode: TeleopMode` until a later test needs it.

- [ ] **Step 8: Update existing converter call sites**

Update existing workflow tests that call `experimental_raw_clock_config_from_settings` directly:

```rust
let config = experimental_raw_clock_config_from_settings(
    &settings,
    TeleopMode::MasterFollower,
    333.0,
    Some(12),
);
```

and:

```rust
let config = experimental_raw_clock_config_from_settings(
    &settings,
    TeleopMode::MasterFollower,
    100.0,
    Some(300),
);
```

- [ ] **Step 9: Run tests to verify GREEN**

Run:

```bash
cargo test -p piper-cli experimental_raw_clock_accepts_bilateral_mode -- --nocapture
cargo test -p piper-cli experimental_raw_clock_config_maps_bilateral_mode -- --nocapture
cargo test -p piper-cli experimental_raw_clock -- --nocapture
```

Expected: PASS.

- [ ] **Step 10: Commit**

```bash
git add apps/cli/src/teleop/config.rs apps/cli/src/teleop/workflow.rs
git commit -m "Thread raw-clock teleop mode through CLI config"
```

## Task 3: Raw-Clock Workflow Runs RuntimeTeleopController

**Files:**
- Modify: `apps/cli/src/teleop/workflow.rs`
- Modify: `apps/cli/src/teleop/report.rs`

- [ ] **Step 1: Add workflow coverage for bilateral raw-clock settings and JSON report**

Add a workflow test near the existing experimental raw-clock workflow tests:

```rust
#[test]
fn experimental_raw_clock_bilateral_run_reports_bilateral_mode_and_gain() {
    let trace = WorkflowTrace::default();
    let backend =
        FakeTeleopBackend::with_trace(trace.clone()).with_experimental_raw_clock_success();
    let io = FakeTeleopIo {
        trace: trace.clone(),
        ..FakeTeleopIo::default()
    };
    let report_slot = io.last_report.clone();
    let args = TeleopDualArmArgs {
        mode: Some(TeleopMode::Bilateral),
        reflection_gain: Some(0.05),
        report_json: Some(PathBuf::from("report.json")),
        ..experimental_args()
    };

    let status = run_workflow_for_test_with_io(args, backend.clone(), io).unwrap();

    assert_eq!(status, TeleopExitStatus::Success);

    let settings = backend
        .experimental_run_settings()
        .expect("experimental raw-clock run settings should be captured");
    assert_eq!(settings.mode, TeleopMode::Bilateral);
    assert_eq!(settings.reflection_gain, 0.05);

    let report = report_slot
        .lock()
        .expect("report lock poisoned")
        .clone()
        .expect("report should be written");
    assert_eq!(report.mode.initial, "bilateral");
    assert_eq!(report.mode.final_, "bilateral");
    assert_eq!(report.control.reflection_gain, 0.05);
}
```

Use the existing `ReportMode.final_` field. Do not add a new report schema field.

- [ ] **Step 2: Add human report coverage for bilateral mode output**

In `apps/cli/src/teleop/report.rs`, add near the existing human report tests:

```rust
#[test]
fn human_report_includes_bilateral_mode_transition() {
    let mut output = Vec::new();
    let mut report = sample_json_report();
    report.mode = ReportMode {
        initial: "bilateral".to_string(),
        final_: "bilateral".to_string(),
    };

    write_human_report(&mut output, &report, Duration::from_micros(9876)).unwrap();

    let output = String::from_utf8(output).unwrap();
    assert!(output.contains("mode: bilateral -> bilateral profile=production"));
}
```

This test may already pass before implementation; it is regression coverage for the spec requirement that human output reflects bilateral mode.

- [ ] **Step 3: Rename the backend method in the test/fake to create RED**

Change the fake backend method name from:

```rust
fn run_master_follower_raw_clock(
```

to:

```rust
fn run_raw_clock(
```

Run before changing the trait to verify the compiler catches the missing trait method.

- [ ] **Step 4: Run tests to verify RED**

Run:

```bash
cargo test -p piper-cli experimental_raw_clock_bilateral_run_reports_bilateral_mode_and_gain -- --nocapture
cargo test -p piper-cli human_report_includes_bilateral_mode_transition -- --nocapture
```

Expected:
- The workflow test FAILS to compile because `ExperimentalRawClockTeleopBackend` has no `run_raw_clock` method, or FAILS because workflow still calls `run_master_follower_raw_clock`.
- The human report test PASSes or is blocked by the same compile failure. If run alone before the fake rename, it should PASS.

- [ ] **Step 5: Rename trait and workflow call**

In `ExperimentalRawClockTeleopBackend`, rename:

```rust
fn run_master_follower_raw_clock(
```

to:

```rust
fn run_raw_clock(
```

Update the call in `run_experimental_raw_clock_workflow`:

```rust
let loop_exit = backend.run_raw_clock(
    settings_handle.clone(),
    resolved.raw_clock.clone(),
    cancel_signal,
)?;
```

Update fake and real backend implementations to the same method name.

- [ ] **Step 6: Change the real backend to use RuntimeTeleopController**

In `RealTeleopBackend::run_raw_clock`, remove:

```rust
let runtime_settings = settings.snapshot();
if runtime_settings.mode != TeleopMode::MasterFollower {
    bail!("experimental calibrated raw clock currently supports master-follower mode only");
}
```

Build the controller from the handle:

```rust
let controller = RuntimeTeleopController::new(settings.clone());
```

Replace:

```rust
let gains = experimental_raw_clock_master_follower_gains(&runtime_settings);
match active.run_master_follower_with_gains(runtime_settings.calibration, gains, run_config)
```

with:

```rust
match active.run_with_controller(controller, run_config)
```

Preserve the existing `Standby`, `Faulted`, and `Err` handling exactly.

- [ ] **Step 7: Remove obsolete CLI-only raw-clock gains helper**

If `experimental_raw_clock_master_follower_gains` is now unused in `apps/cli/src/teleop/workflow.rs`, remove the entire `fn experimental_raw_clock_master_follower_gains(...)` function block.

Remove its dedicated workflow test if it only checks the helper. Do not remove the client-layer
`ExperimentalRawClockMasterFollowerGains` type or client tests; the compatibility wrapper still uses
that type.

Update imports to remove `ExperimentalRawClockMasterFollowerGains` from `apps/cli/src/teleop/workflow.rs` if unused.

- [ ] **Step 8: Verify the real backend source no longer hard-codes master-follower**

Run:

```bash
! rg -n 'currently supports master-follower mode only|run_master_follower_with_gains|experimental_raw_clock_master_follower_gains' apps/cli/src/teleop/workflow.rs
rg -n 'RuntimeTeleopController::new\(settings\.clone\(\)\)' apps/cli/src/teleop/workflow.rs
rg -n 'active\.run_with_controller\(controller, run_config\)' apps/cli/src/teleop/workflow.rs
```

Expected:
- First command returns no matches in `apps/cli/src/teleop/workflow.rs`.
- Second and third commands each find the real backend implementation.

- [ ] **Step 9: Run tests to verify GREEN**

Run:

```bash
cargo test -p piper-cli experimental_raw_clock_bilateral_run_reports_bilateral_mode_and_gain -- --nocapture
cargo test -p piper-cli human_report_includes_bilateral_mode_transition -- --nocapture
cargo test -p piper-cli experimental_raw_clock -- --nocapture
```

Expected: PASS.

- [ ] **Step 10: Commit**

```bash
git add apps/cli/src/teleop/workflow.rs apps/cli/src/teleop/report.rs
git commit -m "Run raw-clock teleop through runtime controller"
```

## Task 4: Smoke Script Passes Reflection Gain And Shows Bilateral Safety Text

**Files:**
- Modify: `scripts/run_teleop_smoke.sh`

- [ ] **Step 1: Verify current script RED for reflection gain**

Run:

```bash
DRY_RUN=1 SKIP_BUILD=1 TELEOP_MODE=bilateral REFLECTION_GAIN=0.05 TIMESTAMP=plan-red ./scripts/run_teleop_smoke.sh > /tmp/teleop-smoke-bilateral-red.out
rg -- '--reflection-gain 0\.05' /tmp/teleop-smoke-bilateral-red.out
rg -- 'reflection_gain=0\.05' /tmp/teleop-smoke-bilateral-red.out
rg 'both arms may receive reflected torque' /tmp/teleop-smoke-bilateral-red.out
```

Expected: at least one `rg` command FAILS because the script currently does not pass or record `REFLECTION_GAIN`, and it prints master-follower-specific text for bilateral mode.

- [ ] **Step 2: Add reflection gain environment plumbing**

Near `MASTER_DAMPING`, add:

```bash
REFLECTION_GAIN="${REFLECTION_GAIN:-0.05}"
```

In `cmd=(...)`, after `--master-damping`, add:

```bash
--reflection-gain "${REFLECTION_GAIN}"
```

In the environment file block, after `master_damping`, add:

```bash
echo "reflection_gain=${REFLECTION_GAIN}"
```

- [ ] **Step 3: Add mode-specific safety text**

Replace the unconditional master-follower guidance:

```bash
echo "Master-follower input is read from ${MASTER_IFACE}; move that physical arm."
echo "If that is not the physical master arm, swap MASTER_IFACE/SLAVE_IFACE and rerun."
```

with:

```bash
if [[ "${MODE}" == "bilateral" ]]; then
    echo "Bilateral mode: both arms may receive reflected torque."
    echo "Start with low REFLECTION_GAIN; current REFLECTION_GAIN=${REFLECTION_GAIN}."
    echo "Stop immediately if the master feels pulled, oscillatory, or unstable."
else
    echo "Master-follower input is read from ${MASTER_IFACE}; move that physical arm."
    echo "If that is not the physical master arm, swap MASTER_IFACE/SLAVE_IFACE and rerun."
fi
```

Keep the existing zero-pose and confirmation notes.

- [ ] **Step 4: Verify script GREEN**

Run:

```bash
bash -n scripts/run_teleop_smoke.sh
DRY_RUN=1 SKIP_BUILD=1 TELEOP_MODE=bilateral REFLECTION_GAIN=0.05 TIMESTAMP=plan-green ./scripts/run_teleop_smoke.sh > /tmp/teleop-smoke-bilateral-green.out
rg -- '--reflection-gain 0\.05' /tmp/teleop-smoke-bilateral-green.out
rg -- 'reflection_gain=0\.05' /tmp/teleop-smoke-bilateral-green.out
rg 'both arms may receive reflected torque' /tmp/teleop-smoke-bilateral-green.out
DRY_RUN=1 SKIP_BUILD=1 TELEOP_MODE=master-follower TIMESTAMP=plan-master ./scripts/run_teleop_smoke.sh > /tmp/teleop-smoke-master-green.out
rg 'Master-follower input is read from' /tmp/teleop-smoke-master-green.out
```

Expected: all commands PASS.

- [ ] **Step 5: Commit**

```bash
git add scripts/run_teleop_smoke.sh
git commit -m "Add bilateral smoke reflection gain"
```

## Task 5: Final Regression Verification

**Files:**
- Verify only unless a regression appears.

- [ ] **Step 1: Run focused raw-clock tests**

Run:

```bash
cargo test -p piper-cli experimental_raw_clock -- --nocapture
cargo test -p piper-cli raw_clock -- --nocapture
cargo test -p piper-client dual_arm_raw_clock -- --test-threads=1 --nocapture
```

Expected: PASS.

- [ ] **Step 2: Run CLI full unit tests**

Run:

```bash
cargo test -p piper-cli
```

Expected: PASS.

- [ ] **Step 3: Run workspace lib tests**

Run:

```bash
cargo test --lib -- --test-threads=1
```

Expected: PASS.

- [ ] **Step 4: Run formatting and lint gates**

Run:

```bash
cargo fmt --all -- --check
git diff --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

Expected: PASS.

- [ ] **Step 5: Run final dry-run hardware commands**

Run:

```bash
DRY_RUN=1 SKIP_BUILD=1 TELEOP_MODE=bilateral MASTER_IFACE=can1 SLAVE_IFACE=can0 REFLECTION_GAIN=0.05 MAX_ITERATIONS=3000 TIMESTAMP=raw-clock-bilateral-plan ./scripts/run_teleop_smoke.sh > /tmp/teleop-smoke-bilateral-final.out
rg -- '--mode bilateral' /tmp/teleop-smoke-bilateral-final.out
rg -- '--reflection-gain 0\.05' /tmp/teleop-smoke-bilateral-final.out
rg -- 'reflection_gain=0\.05' /tmp/teleop-smoke-bilateral-final.out
rg 'Bilateral mode: both arms may receive reflected torque\.' /tmp/teleop-smoke-bilateral-final.out
```

Expected: all `rg` commands PASS.

- [ ] **Step 6: Commit any final test-only or polish changes**

If Task 5 required fixes, commit them:

```bash
git status --short
git add crates/piper-client/src/dual_arm_raw_clock.rs apps/cli/src/teleop/config.rs apps/cli/src/teleop/workflow.rs apps/cli/src/teleop/report.rs scripts/run_teleop_smoke.sh
git commit -m "Verify raw-clock bilateral teleop"
```

If there are no changes, do not create an empty commit.

## Hardware Handoff After Implementation

First short run:

```bash
TELEOP_MODE=bilateral MASTER_IFACE=can1 SLAVE_IFACE=can0 REFLECTION_GAIN=0.05 MAX_ITERATIONS=3000 ./scripts/run_teleop_smoke.sh
```

If clean, longer run:

```bash
TELEOP_MODE=bilateral MASTER_IFACE=can1 SLAVE_IFACE=can0 REFLECTION_GAIN=0.05 MAX_ITERATIONS=30000 ./scripts/run_teleop_smoke.sh
```

Pass criteria:

- `exit.reason == "max_iterations"`
- `metrics.read_faults == 0`
- `metrics.submission_faults == 0`
- `timing.clock_health_failures == 0`
- no oscillation or uncomfortable pull at the master arm
- joint motion report shows expected follower response
