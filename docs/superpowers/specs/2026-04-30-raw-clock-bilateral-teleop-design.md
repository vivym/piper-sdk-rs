# Raw-Clock Bilateral Teleop Design

Date: 2026-04-30

## Context

The stable teleop baseline is now:

- `mode=master-follower`
- `joint-map=identity`
- `timing_source=calibrated_hw_raw`
- `MASTER_DAMPING=0.05`
- SocketCAN roles: `MASTER_IFACE=can1`, `SLAVE_IFACE=can0`

Multiple 5 minute and 10 minute smoke runs completed cleanly with no read faults, submission
faults, or raw-clock health failures.

The next experiment is low-gain bilateral teleop. The first attempt failed before hardware enable
because the CLI and raw-clock runtime currently reject `experimental_calibrated_raw + bilateral`.

## Goal

Allow the existing calibrated raw-clock SoftRealtime path to run either:

- `master-follower`, preserving the current stable behavior, or
- `bilateral`, using the existing `RuntimeTeleopController` and low `reflection_gain`.

This is a link-validation phase for bilateral control. It should prove the timing, command,
reporting, and safety-shutdown chain works in bilateral mode before adding gravity compensation.

## Non-Goals

- Do not implement gravity compensation in this change.
- Do not change the default teleop mode from `master-follower`.
- Do not increase `reflection_gain` defaults.
- Do not change the production StrictRealtime bilateral path.
- Do not introduce a second raw-clock control loop.

## Current Blockers

There are three explicit blockers:

1. CLI config rejects raw-clock modes other than master-follower.
2. `ExperimentalRawClockConfig::with_mode` and `validate` reject `Bilateral`.
3. The CLI raw-clock workflow calls `run_master_follower_raw_clock`, which builds a
   `MasterFollowerController` instead of using the runtime controller that already supports both
   modes.

The lower-level raw-clock runtime core is already generic over `C: BilateralController`, so the
architecture can be extended without rewriting the runtime loop.

## Design

### Client Layer

Permit `ExperimentalRawClockMode::Bilateral` in `ExperimentalRawClockConfig`.

Make the existing private `ExperimentalRawClockDualArmActive::run_with_controller` method public so
callers can run any `BilateralController` without duplicating runtime-core plumbing:

```rust
pub fn run_with_controller<C>(
    self,
    controller: C,
    cfg: ExperimentalRawClockRunConfig,
) -> Result<ExperimentalRawClockRunExit, RawClockRuntimeError>
where
    C: BilateralController
```

Keep `run_master_follower` and `run_master_follower_with_gains` as compatibility wrappers. Existing
master-follower code should continue to behave exactly as before.

### CLI Workflow

Rename the backend method from `run_master_follower_raw_clock` to `run_raw_clock`.

The real backend should:

1. Snapshot the already resolved `RuntimeTeleopSettingsHandle`.
2. Require the experimental active phase.
3. Build `ExperimentalRawClockRunConfig` from the raw-clock settings.
4. Build `RuntimeTeleopController::new(settings_handle.clone())`.
5. Call the new client-layer controller-based raw-clock run method.

Remove the real backend guard that rejects modes other than `MasterFollower`. The backend may still
snapshot settings for config construction and reporting, but command generation must be delegated to
`RuntimeTeleopController`.

Change `experimental_raw_clock_config_from_settings` to accept the resolved `TeleopMode` or the
resolved runtime settings, map it to `ExperimentalRawClockMode`, and pass that mode into
`ExperimentalRawClockConfig`. Tests must assert that bilateral produces
`ExperimentalRawClockMode::Bilateral`; otherwise an implementation could remove the CLI guard while
still running the raw-clock runtime with a hard-coded master-follower config.

This keeps mode selection in one place: `RuntimeTeleopController` reads `settings.mode` and emits
master-follower or bilateral commands.

### Control Semantics

For master-follower, behavior remains unchanged:

- slave tracks master position and velocity through calibration
- master receives damping only
- reflected interaction torque is zero

For bilateral, the existing runtime controller behavior is used:

- slave still tracks master through calibration
- master receives damping
- master receives reflected torque from slave feedback torque:
  `master_interaction_torque = -slave_to_master_torque(slave_torque) * reflection_gain`

Without gravity compensation, this is intentionally a low-gain torque-reflection smoke test. The
feedback includes environment contact plus gravity, friction, and model error. Hardware validation
must keep `reflection_gain` low, starting at `0.05`.

### Safety and Reporting

The raw-clock gates stay unchanged:

- calibrated raw-clock estimator health
- selected sample age
- residual p95/max gates
- inter-arm skew gate
- per-arm state skew gate
- alignment buffer miss debounce

Shutdown behavior remains the existing raw-clock fault path:

- read, timing, clock health, controller, and submission faults all attempt bounded fault shutdown
- clean max-iteration and cancellation exits disable both arms

Reports continue to use the existing raw-clock report conversion. The JSON top-level
`mode.initial` and `mode.final`, and the human `mode: ... -> ...` line, should reflect `bilateral`
when selected. Do not add `control.mode` unless intentionally changing the report schema.

### Script UX

`scripts/run_teleop_smoke.sh` should keep `TELEOP_MODE=master-follower` by default.

The script must define `REFLECTION_GAIN`, pass `--reflection-gain "${REFLECTION_GAIN}"`, and record
`reflection_gain=${REFLECTION_GAIN}` in `environment.txt`. For bilateral smoke runs, operators must
set `REFLECTION_GAIN=0.05`; otherwise the CLI default would remain `0.25`, which is too aggressive
for the first raw-clock bilateral validation.

When `TELEOP_MODE=master-follower`, print the current master-arm guidance.

When `TELEOP_MODE=bilateral`, print a different warning:

- both arms may receive reflected torque
- start with low `REFLECTION_GAIN`, recommended `0.05`
- stop immediately if master feels pulled, oscillatory, or unstable

The script should not imply that bilateral has a single input arm in the same way master-follower
does.

## Test Plan

Automated tests:

- CLI config resolves `experimental_calibrated_raw + bilateral`.
- Client raw-clock config accepts `ExperimentalRawClockMode::Bilateral`.
- Existing tests that currently assert bilateral rejection are inverted into accepting tests.
- Raw-clock config conversion maps `TeleopMode::Bilateral` to `ExperimentalRawClockMode::Bilateral`.
- Raw-clock workflow enters the experimental backend for bilateral instead of failing in config.
- Fake backend records that the raw-clock run was invoked with `TeleopMode::Bilateral`.
- Existing master-follower raw-clock tests continue to pass.
- Script dry-run output uses bilateral-specific safety text when `TELEOP_MODE=bilateral`.
- Script dry-run output contains `--reflection-gain 0.05` when `REFLECTION_GAIN=0.05`.
- Report tests assert `mode.initial == "bilateral"`, `mode.final == "bilateral"`, and
  `control.reflection_gain == 0.05`.

Verification commands:

```bash
cargo test -p piper-cli experimental_raw_clock -- --nocapture
cargo test -p piper-cli raw_clock -- --nocapture
cargo test --lib -- --test-threads=1
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

Hardware validation:

```bash
TELEOP_MODE=bilateral MASTER_IFACE=can1 SLAVE_IFACE=can0 REFLECTION_GAIN=0.05 MAX_ITERATIONS=3000 ./scripts/run_teleop_smoke.sh
```

If the short run is clean:

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

## Follow-Up

After low-gain bilateral is stable, design the gravity compensation / external torque estimation
layer. That work should be separate so any instability can be attributed to either raw bilateral
wiring or compensation logic, not both at once.
