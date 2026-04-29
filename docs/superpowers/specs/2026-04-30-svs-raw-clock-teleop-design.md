# SVS Raw-Clock Teleoperation Design

Date: 2026-04-30

## Context

The `piper-svs-collect` addon already implements the SVS data-collection
teleoperation path with gravity compensation:

- `SvsController` implements `BilateralController`.
- `SvsMujocoBridge` implements `BilateralDynamicsCompensator`.
- `SvsTelemetrySink` writes `SvsEpisodeV1` steps from
  `BilateralLoopTelemetry`.
- The collector currently runs through `DualArmBuilder::build()`, which requires
  `StrictRealtime`, and then calls
  `DualArmActiveMit::run_bilateral_with_compensation()`.

Recent hardware work stabilized calibrated hardware raw-clock teleoperation on
the SoftRealtime path:

- `timing_source=calibrated_hw_raw`
- `joint-map=identity`
- default smoke script master/follower roles:
  `MASTER_IFACE=can1`, `SLAVE_IFACE=can0`
- low master damping works best in hardware, currently around `0.05`
- raw-clock alignment, estimator health, residual debouncing, state skew, and
  selected-sample-age gates are now part of the teleop CLI path

The raw-clock bilateral teleop implementation plan was tracked separately:

- `docs/superpowers/plans/2026-04-30-raw-clock-bilateral-teleop.md`

That prerequisite is now satisfied on `main`. The current raw-clock runtime
accepts bilateral mode and the main teleop CLI can run raw-clock bilateral
without gravity compensation. SVS raw-clock integration should build on that
runtime rather than reintroducing a separate control loop.

## Goal

Add an explicit experimental calibrated raw-clock backend to `piper-svs-collect`
so SVS can collect gravity-compensated bilateral episodes without requiring
`StrictRealtime`.

The raw-clock SVS path should reuse the same control, compensation, telemetry,
writer, calibration, and cancellation concepts as the existing StrictRealtime
SVS path, while adopting the raw-clock timing, alignment, diagnostics, startup
UX, and safety gates explored in the main teleop CLI.

## Non-Goals

- Do not make calibrated raw-clock the default SVS backend.
- Do not remove or weaken the existing StrictRealtime SVS backend.
- Do not add MuJoCo dependencies to the default workspace, `piper-cli`, or
  normal SDK crates.
- Do not move master-follower smoke testing into SVS.
- Do not change `SvsEpisodeV1` per-step schema in this phase.
- Do not silently fall back from raw-clock to StrictRealtime after the operator
  selects the raw-clock backend.
- Do not implement a new gravity compensation algorithm. Reuse the existing SVS
  MuJoCo bridge.

## Prerequisite Status

The raw-clock bilateral teleop prerequisite is complete and merged:

- `docs/superpowers/plans/2026-04-30-raw-clock-bilateral-teleop.md`
- `6e49aba Allow raw-clock bilateral runtime mode`
- `8552c66 Thread raw-clock teleop mode through CLI config`
- `453dc23 Run raw-clock teleop through runtime controller`
- `08c3d12 Add bilateral smoke reflection gain`
- `1455b9e Apply raw-clock master interaction torque`

Those changes provide a stable raw-clock runtime that can run bilateral mode
without gravity compensation. SVS now adds the compensation, telemetry, and
episode-recording layer on top of that runtime boundary.

## Architecture Boundary

The design has three ownership layers.

### SDK Raw-Clock Runtime

`piper-client::dual_arm_raw_clock` owns calibrated raw-clock runtime mechanics:

- calibrated raw-clock estimator health
- aligned snapshot selection
- selected/latest inter-arm skew reporting
- state-skew gates
- sample-age and sample-gap gates
- residual p95/max gates
- alignment buffer miss debounce
- submission and bounded shutdown behavior

This crate must stay MuJoCo-free. It should provide generic hooks that are useful
to any bilateral controller, including SVS.

### SVS Collector

`addons/piper-svs-collect` owns research-specific behavior:

- CLI/profile selection of SVS runtime backend
- MuJoCo model loading
- `SvsMujocoBridge`
- `SvsController`
- `SvsTelemetrySink`
- episode directory, manifest, and report writing
- raw CAN side recording where already supported

The collector may depend on MuJoCo through addon crates. It must remain excluded
from the default workspace dependency graph.

### General Teleop CLI

`piper-cli teleop dual-arm` remains the general hardware smoke and operator
teleop surface. It owns master-follower raw-clock smoke testing and low-gain
raw-clock bilateral link validation. SVS does not reimplement these CLI modes.

## SDK Raw-Clock Runtime Design

The raw-clock runtime needs a compensation-capable entry point analogous to
StrictRealtime `run_bilateral_with_compensation()`.

The current raw-clock active runner already exposes
`ExperimentalRawClockDualArmActive::run_with_controller()` and accepts
`ExperimentalRawClockMode::Bilateral`. The missing SDK work is not mode
selection; it is compensation, telemetry, and gripper telemetry support for the
same raw-clock runtime core.

The new path should reuse the existing raw-clock runtime core instead of
creating a second control loop. The runtime core should optionally accept:

- a `BilateralController`
- a `BilateralDynamicsCompensator`
- a raw-clock run/config object
- an optional telemetry sink
- gripper telemetry/mirroring settings compatible with `BilateralLoopConfig`

The existing raw-clock CLI smoke path must remain low risk. Compensation,
confirmed-finished telemetry submission, and gripper telemetry/mirroring should
be enabled only through the new compensation/telemetry-capable raw-clock entry
point or equivalent explicit config. Existing raw-clock runs with no telemetry
sink and no compensator should keep their current command-submission and gripper
behavior.

The compensation-capable entry point must also preserve the lifecycle semantics
used by the StrictRealtime bilateral loop:

- reset the compensator before entering the runtime loop
- keep generic `run_with_controller()` controller lifecycle unchanged unless an
  explicit reset-capable wrapper is added
- if elapsed control-frame time exceeds the configured dt clamp, call
  `controller.on_time_jump(raw_dt)`
- if a compensator is present and the same time jump occurs, call
  `compensator.on_time_jump(raw_dt)`
- map controller time-jump failures to controller faults
- map compensator reset/time-jump failures to compensation faults

SVS should either construct a fresh `SvsController` for each run, as it does
today, or explicitly reset it before handing it to the raw-clock runtime. The SDK
runtime must not silently skip compensator reset because `SvsMujocoBridge`
contains stateful MuJoCo compensation.

For each aligned tick:

1. Select an aligned `DualArmSnapshot` through the raw-clock alignment path.
2. If a compensator is present, call
   `BilateralDynamicsCompensator::compute(&snapshot, nominal_period)`.
3. Build `BilateralControlFrame { snapshot, compensation }`.
4. Call `controller.tick_with_compensation(&frame, nominal_period)`.
5. Assemble final torques including model compensation.
6. When telemetry is enabled, submit both arms through the SoftRealtime
   confirmed-finished command path so per-tick tx-finished timestamps are
   available. Non-telemetry raw-clock callers may keep the current confirmed
   submission behavior.
7. When telemetry or gripper mirroring is enabled, collect gripper telemetry and,
   if enabled, perform gripper mirroring with the same bounded semantics as the
   StrictRealtime bilateral loop.
8. Emit loop telemetry after arm command submission and gripper handling when a
   telemetry sink is present.
9. Apply the raw-clock runtime's normal timing accounting and fault handling.

The existing raw-clock master-follower and bilateral wrappers should continue to
work without a compensator.

### Telemetry Shape

The first-stage design must emit the existing
`piper_client::dual_arm::BilateralLoopTelemetry` from the raw-clock runtime.
That structure is already consumed by `SvsTelemetrySink`, and it contains the
control frame, compensation, controller command, shaped command, final torques,
and tx completion timestamps that SVS uses to write `SvsEpisodeV1` steps.

Raw-clock telemetry emitted to `SvsTelemetrySink` must populate the fields SVS
requires to build a step:

- `control_frame.snapshot`
- `control_frame.compensation`
- `controller_command`
- `shaped_command`
- `compensation`
- `final_torques`
- `master_t_ref_nm`
- `slave_t_ref_nm`
- `master_tx_finished_host_mono_us`
- `slave_tx_finished_host_mono_us`
- `timing.control_frame_host_mono_us`
- `timing.clamped_dt_us`
- `timing.deadline_missed`
- `gripper`

`master_tx_finished_host_mono_us` and `slave_tx_finished_host_mono_us` must be
`Some(nonzero)`. The existing SVS sink treats missing tx-finished timestamps as
a telemetry sink fault.

Raw-clock gripper telemetry must be valid for the existing `SvsGripperStepV1`
schema. If gripper mirroring is enabled in the SVS profile, the raw-clock
runtime must either implement the same mirror semantics as the StrictRealtime
loop or reject the raw-clock SVS configuration before enabling the arms. It must
not silently emit incomplete gripper fields.

Raw-clock-specific diagnostics should remain in raw-clock reports during this
phase, not per-step SVS episode rows.

### Fault Accounting

`RawClockRuntimeReport` should distinguish at least:

- read faults
- submission faults
- clock health failures
- alignment buffer miss consecutive failures
- compensation faults
- controller faults
- telemetry sink faults

The SDK should add explicit raw-clock exit reasons and counters for
compensation and telemetry sink failures rather than relying only on
`last_error` strings:

- `RawClockRuntimeExitReason::CompensationFault`
- `RawClockRuntimeExitReason::TelemetrySinkFault`
- `RawClockRuntimeReport::compensation_faults`
- `RawClockRuntimeReport::controller_faults`
- `RawClockRuntimeReport::telemetry_sink_faults`

Compensation, controller, or telemetry sink errors should fault the run. The
first implementation should fail closed rather than attempting a complex hold
trajectory.

## SVS Collector Runtime Design

Add an explicit runtime selection to the SVS collector:

```rust
enum SvsRuntimeKind {
    StrictRealtime,
    CalibratedRawClock,
}
```

Default behavior remains `StrictRealtime`.

The raw-clock backend is selected only by an explicit opt-in flag, for example:

```text
--experimental-calibrated-raw
```

The raw-clock backend supports SVS bilateral collection only. If a future SVS
mode selector exists, raw-clock SVS should reject non-bilateral SVS modes rather
than silently running a different control law.

### Raw-Clock Configuration

SVS should expose raw-clock settings through CLI flags and task profile fields
with the same semantics as the main teleop CLI:

- warmup seconds
- residual p95 threshold
- residual max threshold
- residual max consecutive failure threshold
- drift threshold
- sample gap threshold
- last sample age threshold
- selected sample age threshold
- inter-arm skew threshold
- per-arm state skew threshold
- alignment lag
- alignment search window
- alignment buffer miss consecutive failure threshold

Configuration precedence:

1. CLI arguments
2. task profile TOML
3. SVS built-in defaults

SVS should not depend on `apps/cli`. It may duplicate the stable raw-clock
defaults in an addon-owned `SvsRawClockSettings` type, but field names and
operator-facing semantics should match the teleop CLI.

### Startup Flow

The raw-clock SVS startup sequence should match the stable CLI flow:

1. Connect both arms through SoftRealtime SocketCAN.
2. Read firmware/profile context.
3. Warm up and validate calibrated raw-clock timing before operator enable.
4. Print startup summary and require operator confirmation unless `--yes` is
   supplied.
5. After confirmation, refresh raw-clock timing again.
6. Enable MIT passthrough.
7. Read the active snapshot.
8. Capture or validate active-zero calibration.
9. Start the SVS raw-clock bilateral loop.

Operator UX should print visible stage markers:

```text
startup: refreshing raw-clock timing (~10s)...
startup: enabling MIT passthrough...
startup: reading active snapshot...
startup: capturing active-zero calibration...
startup: starting SVS raw-clock bilateral loop...
```

If `--calibration-file` is used, pre-enable and post-enable posture
compatibility checks should stay aligned with the existing SVS calibration
behavior. If `--save-calibration` is used, raw-clock SVS should capture the
active-zero calibration after MIT passthrough enable, matching the stable CLI
behavior.

## Data and Report Design

Do not change the `SvsEpisodeV1` per-step schema in this phase.

Raw-clock metadata and diagnostics should be added as optional manifest/report
sections with precise schema locations:

```text
ManifestV1.raw_clock: Option<RawClockManifest>
ReportJson.raw_clock: Option<RawClockReportJson>
```

The fields should use `#[serde(skip_serializing_if = "Option::is_none")]` where
appropriate so StrictRealtime SVS manifests and reports stay compact. Missing
optional fields must deserialize as `None` for compatibility with previously
written episodes.

The optional raw-clock section should include:

- timing source
- strict realtime flag
- experimental flag
- warmup and runtime thresholds
- clock drift per arm
- residual p95 per arm
- selected inter-arm skew max/p95
- latest inter-arm skew max/p95
- alignment lag
- alignment search window
- alignment buffer miss counts
- residual max spike/consecutive-failure counters
- clock health failure count
- read/submission/runtime fault counters
- compensation/controller/telemetry sink fault counters
- final failure kind when present

This keeps old episode readers compatible while allowing analysis tools to
differentiate StrictRealtime SVS data from calibrated raw-clock SVS data.

## Safety and Error Handling

The raw-clock SVS backend must not silently degrade to StrictRealtime. If the
operator explicitly selected raw-clock, the resulting episode and report must
have clear raw-clock timing semantics.

Failure behavior:

- Startup failures before enable should reject the run without enabling arms.
- Failures after confirmation but before loop start should attempt bounded
  stop/disable for any enabled arm and finalize the report.
- Loop failures should generate a faulted report and attempt bounded shutdown.
- Operator Ctrl-C should use the existing SVS cancellation path, finalize
  manifest/report, and record a cancellation exit reason.

The first raw-clock SVS implementation should fail closed on compensation,
controller, or telemetry sink errors. A more complex hold behavior can be
designed later if hardware evidence shows it is needed.

## Test Plan

### `piper-client`

Add unit coverage for the raw-clock compensation-capable runtime:

- compensator receives the aligned `DualArmSnapshot`
- compensator `reset()` is called before the first tick, and reset failure maps
  to a compensation fault
- controller and compensator `on_time_jump()` are called when raw dt exceeds the
  configured clamp, and their failures map to controller/compensation faults
- compensation is included in `BilateralControlFrame`
- controller receives the compensation frame
- telemetry sink receives the same logical fields used by StrictRealtime SVS,
  including nonzero tx-finished timestamps and `master_t_ref_nm`/`slave_t_ref_nm`
- raw-clock telemetry contains valid gripper telemetry when gripper mirroring is
  disabled
- raw-clock gripper mirroring either behaves like StrictRealtime or rejects the
  configuration before enable if unsupported
- compensation faults produce a faulted raw-clock report
- controller faults produce a faulted raw-clock report
- telemetry sink faults produce a faulted raw-clock report
- raw-clock compensation/controller/telemetry fault counters are set correctly
- no-telemetry, no-compensator raw-clock runs preserve the existing CLI smoke
  submission/gripper behavior
- existing raw-clock master-follower and bilateral tests continue to pass

### `piper-svs-collect`

Add fake/harness tests for the collector:

- default collector runtime remains StrictRealtime
- `--experimental-calibrated-raw` selects the raw-clock backend
- raw-clock backend writes a complete `SvsEpisodeV1` without changing step schema
- raw-clock manifest/report sections are present only for raw-clock runs
- StrictRealtime manifest/report validation still passes with no raw-clock section
- raw-clock compensation data flows through the existing `SvsTelemetrySink`
- raw-clock telemetry missing tx-finished timestamps faults the run before
  writing an invalid step
- raw-clock gripper telemetry maps into `SvsGripperStepV1`
- calibration capture/check behavior matches the StrictRealtime path
- compensation fault finalizes a faulted episode
- writer/telemetry fault finalizes a faulted episode
- cancellation before enable and during loop finalizes cleanly

### Dependency Boundary

Verify normal workspace commands still do not require MuJoCo:

```bash
cargo check --all-targets
cargo test --lib
```

Verify the SVS addon explicitly:

```bash
cargo test --manifest-path addons/piper-svs-collect/Cargo.toml --all-targets
```

### Hardware Acceptance

Use the stable raw-clock teleop CLI as a baseline before SVS runs:

```bash
MASTER_IFACE=can1 SLAVE_IFACE=can0 MAX_ITERATIONS=30000 ./scripts/run_teleop_smoke.sh
```

Then run SVS raw-clock bilateral in stages:

1. short static or light-motion run
2. 30k iteration run
3. 10 minute run
4. comparison against StrictRealtime SVS episode completeness and compensation
   behavior

Pass criteria:

- run exits cleanly for max-iteration acceptance runs
- `read_faults == 0`
- `submission_faults == 0`
- `clock_health_failures == 0`
- raw-clock selected/latest skew metrics remain within configured thresholds
- episode manifests and reports are written on clean, faulted, and cancelled runs
- `SvsEpisodeV1` step schema remains backward compatible

## Implementation Sequence

1. Treat `2026-04-30-raw-clock-bilateral-teleop.md` as satisfied and build on
   the merged raw-clock bilateral runtime.
2. Extend `piper-client::dual_arm_raw_clock` with compensation, gripper, and
   telemetry hooks.
3. Add raw-clock runtime selection and settings to `piper-svs-collect`.
4. Wire SVS raw-clock backend to `SvsController`, `SvsMujocoBridge`, and
   `SvsTelemetrySink`.
5. Add manifest/report raw-clock optional sections.
6. Add automated tests at SDK and addon layers.
7. Run hardware acceptance in short, 30k iteration, then 10 minute stages.

## Open Follow-Ups

Per-step raw-clock diagnostics are intentionally postponed. If analysis later
needs raw-clock timing at every SVS step, introduce a separate `SvsEpisodeV2`
schema rather than extending `SvsEpisodeV1` in place.
