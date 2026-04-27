# Isomorphic Dual-Arm Teleoperation CLI Design

## Summary

Build a production-oriented dual-arm teleoperation command in `piper-cli` for two
Piper arms. The command provides an operator-facing host program for isomorphic
teleoperation: one arm acts as the master, the other as the slave, with optional
bilateral force reflection after a conservative master-follower bring-up path.

The first implementation reuses the existing `piper-client::dual_arm` control
loop and controllers. That current SDK path requires `StrictRealtime`, so the
first runnable version supports StrictRealtime dual-arm targets only. Today that
means SocketCAN with trusted hardware timestamp capability. GS-USB dual-arm
teleoperation remains a product goal, but it requires additional SDK design for
SoftRealtime dual-arm control before the CLI may claim runtime support.

The CLI does not reimplement MIT frame emission. It owns production concerns:
target selection, configuration merging, calibration loading/capture, startup
confirmation, runtime console commands, structured reports, and documented
hardware bring-up.

## Goals

- Add a production CLI entry point for two physical Piper arms.
- Support master-follower and bilateral joint-space teleoperation.
- Support production dual-arm teleoperation on StrictRealtime SocketCAN targets
  in the first implementation.
- Document concrete GS-USB target grammar, but reject GS-USB runtime execution
  in v1 with an actionable error until SDK SoftRealtime dual-arm support exists.
- Use master/slave terminology consistently in CLI, config, docs, and reports.
- Keep SDK dual-arm realtime loop semantics as the single source of control
  behavior.
- Provide explicit calibration, safety profile, fault response, and report
  semantics.
- Keep real-hardware tests out of the default test suite while documenting
  bring-up and manual acceptance checks.

## Non-Goals

- Do not implement Cartesian teleoperation.
- Do not support shared-bus dual-arm offset mode.
- Do not add network-distributed teleoperation.
- Do not implement gripper force reflection.
- Do not bypass or duplicate `piper-client::dual_arm` safety paths.
- Do not make runtime console commands a full general-purpose REPL in the first
  version.
- Do not add GS-USB / SoftRealtime dual-arm execution in this CLI v1. That needs
  a separate SDK spec and plan.

## Existing Context

The SDK already contains a dual-arm MIT coordination layer in
`crates/piper-client/src/dual_arm.rs`. It provides:

- `DualArmBuilder`
- `DualArmStandby`
- `DualArmActiveMit`
- `DualArmObserver`
- `DualArmCalibration`
- `JointMirrorMap`
- `BilateralLoopConfig`
- `DualArmSafetyConfig`
- `MasterFollowerController`
- `JointSpaceBilateralController`
- `BilateralController`
- `DualArmLoopExit`
- `BilateralRunReport`

The SDK also has a non-production example:

- `crates/piper-sdk/examples/dual_arm_bilateral_control.rs`

The MuJoCo addon has a richer bilateral example and bring-up guide:

- `addons/piper-physics-mujoco/examples/dual_arm_bilateral_mujoco.rs`
- `addons/piper-physics-mujoco/docs/dual_arm_bilateral_robot_guide.md`

The new CLI command should productize the validated SDK path rather than move
example-only code into production unchanged.

Important current limitation: `DualArmBuilder::build()` calls
`ConnectedPiper::require_strict()` for both arms. GS-USB backends currently
expose `SoftRealtime` when hardware timestamps are available and `MonitorOnly`
otherwise. Therefore GS-USB targets cannot create the existing
`DualArmStandby` runtime session without additional SDK work.

## Command Shape

Add a top-level `teleop` command group under `piper-cli`:

```text
piper-cli teleop dual-arm [OPTIONS]
```

Basic SocketCAN usage:

```bash
piper-cli teleop dual-arm \
  --master-interface can0 \
  --slave-interface can1 \
  --mode master-follower
```

Future GS-USB target syntax:

```bash
piper-cli teleop dual-arm \
  --master-serial MASTER123 \
  --slave-serial SLAVE456 \
  --mode bilateral
```

The first runnable implementation must reject this at runtime with an explicit
message that GS-USB dual-arm teleoperation requires future SDK SoftRealtime
dual-arm support.

Canonical target usage:

```bash
piper-cli teleop dual-arm \
  --master-target socketcan:can0 \
  --slave-target socketcan:can1
```

Linux may default to `can0` and `can1` when no per-arm target is specified.
Non-Linux platforms have no supported runtime backend in v1 unless a future
backend exposes `StrictRealtime`.

Each arm target is exclusive:

- master: exactly one of `--master-target`, `--master-interface`,
  `--master-serial`, `--master-gs-usb-bus-address`, or inherited config/default
  target
- slave: exactly one of `--slave-target`, `--slave-interface`,
  `--slave-serial`, `--slave-gs-usb-bus-address`, or inherited config/default
  target
- master and slave must not resolve to the same target

First-version teleop target parsing only accepts concrete target strings:

- `socketcan:<iface>`
- `gs-usb-serial:<serial>`
- `gs-usb-bus-address:<bus>:<address>`

The command rejects `auto-strict`, `auto-any`, and `gs-usb-auto`. If Linux
defaults are used, `can0` and `can1` are materialized as concrete SocketCAN
targets before validation. If both GS-USB arms are configured with different
selector kinds, such as serial for master and bus/address for slave, v1 rejects
the configuration unless an implementation adds a pre-connect enumeration step
that normalizes both selectors to the same physical-device identity.

After parsing, runtime target validation rejects any non-StrictRealtime target
for v1. In practice, GS-USB targets are parsed for future-proof config and help
text but are not accepted for actual teleop execution until the SDK provides a
dual-arm SoftRealtime path.

User-facing naming is always `master` and `slave`. The SDK currently exposes
some lower-level left/right names; the CLI adapts master to SDK left and slave
to SDK right internally. That mapping must not leak into CLI help, config, or
reports except when explaining low-level implementation details.

## CLI Options

Core options:

```text
--config <PATH>
--mode <master-follower|bilateral>
--profile <production|debug>
--frequency-hz <HZ>
--track-kp <VALUE>
--track-kd <VALUE>
--master-damping <VALUE>
--reflection-gain <VALUE>
--disable-gripper-mirror
--calibration-file <PATH>
--calibration-max-error-rad <RAD>
--save-calibration <PATH>
--report-json <PATH>
--yes
```

Target options:

```text
--master-target <SPEC>
--slave-target <SPEC>
--master-interface <IFACE>
--slave-interface <IFACE>
--master-serial <SERIAL>
--slave-serial <SERIAL>
--master-gs-usb-bus-address <BUS:ADDRESS>
--slave-gs-usb-bus-address <BUS:ADDRESS>
--baud-rate <BAUD>
```

Debug/test options:

```text
--max-iterations <N>
--timing-mode <sleep|spin>
```

`--yes` only skips operator confirmation prompts. It must not skip hard safety
checks such as invalid frequency, duplicated targets, malformed calibration,
unsupported platform defaults, or invalid gains.

## Configuration Model

Configuration precedence:

1. CLI arguments
2. Teleop TOML loaded via `--config`
3. built-in profile defaults

Initial TOML shape:

```toml
[arms.master]
target = "socketcan:can0"

[arms.slave]
target = "socketcan:can1"

[control]
mode = "master-follower"
frequency_hz = 200.0
track_kp = 8.0
track_kd = 1.0
master_damping = 0.4
reflection_gain = 0.25

[safety]
profile = "production"
gripper_mirror = true

[calibration]
file = "calibration.toml"
max_error_rad = 0.05
```

The teleop config should be separate from the existing single-arm default
target config. The single-arm CLI config is optimized for one Piper; dual-arm
teleoperation needs explicit per-role target resolution and should not infer a
slave arm from the single-arm target.

Teleop target values are strings using the same concrete-target grammar as the
CLI:

- `socketcan:can0`
- `gs-usb-serial:ABC123`
- `gs-usb-bus-address:1:8`

The implementation parses these strings with `TargetSpec::from_str` and then
converts to `ConnectionTarget` / `PiperBuilder`. It must not deserialize these
fields directly as the current `TargetSpec` tagged-enum TOML representation.

Duplicate detection before connecting:

- same SocketCAN interface is a duplicate
- same GS-USB serial is a duplicate
- same GS-USB bus/address pair is a duplicate
- mixed GS-USB selector kinds are rejected in v1 unless normalized by explicit
  enumeration before connection
- SocketCAN and GS-USB targets are different backend families and cannot be the
  same physical target

## Calibration

The command supports two calibration paths:

- capture from current physical poses
- load from a calibration TOML file

Default behavior is manual capture. The operator moves both arms to the
isomorphic zero pose and presses Enter. The CLI then calls
`DualArmStandby::capture_calibration(JointMirrorMap::left_right_mirror())`
using the production read policy.

Calibration file version 1:

```toml
version = 1
created_at_unix_ms = 1770000000000
note = "bench A mirrored zero"

[map]
permutation = ["J1", "J2", "J3", "J4", "J5", "J6"]
position_sign = [-1, 1, 1, -1, 1, -1]
velocity_sign = [-1, 1, 1, -1, 1, -1]
torque_sign = [-1, 1, 1, -1, 1, -1]

[zero]
master = [0.0, 0.1, 0.2, 0.0, 0.5, 0.0]
slave = [0.0, -0.1, 0.2, 0.0, 0.5, 0.0]
```

Validation rules:

- `version` must be exactly `1`.
- `permutation` must contain each joint exactly once.
- sign arrays must contain only `-1` or `1`.
- zero arrays must contain six finite radians.
- malformed files fail before MIT enable.

Loaded calibration files require a pre-enable posture compatibility check. After
connecting both arms and loading the file, the CLI reads a current standby
snapshot with the configured dual-arm read policy, computes:

```text
expected_slave = calibration.master_to_slave_position(current_master)
max_error = max(abs(expected_slave[j] - current_slave[j]))
```

If `max_error > calibration.max_error_rad`, the command fails before MIT enable
and instructs the operator to recapture calibration or move both arms back to the
saved isomorphic zero relationship. The default threshold is `0.05 rad`.
First-version values must be finite and satisfy `0.0 < max_error_rad <= 0.05`.
This option can only tighten the default tolerance; it cannot loosen the
jump-prevention check.

The compatibility check applies to loaded and captured calibration. The command
must validate the current posture immediately before MIT enable, after the final
operator confirmation, because either arm can be moved between initial
calibration and confirmation. After MIT enable, before starting the normal
bilateral loop, the command must take one active snapshot and run the same
relationship check again. If that active check fails, the CLI must not enter
`run_bilateral`; it disables both arms and exits non-zero.

`--save-calibration` writes the captured calibration after successful capture.
It must not overwrite an existing file unless a future explicit overwrite flag
is added.

## Startup Flow

The command runs these phases:

1. Parse CLI and optional TOML config.
2. Load and schema-validate a calibration file if one was provided.
3. Reject an existing `--save-calibration` destination before connecting.
4. Resolve and validate concrete master/slave targets.
5. Install Ctrl+C cancellation handling before any torque-enable operation.
6. Connect both arms through `DualArmBuilder`.
7. Read initial runtime health and fail if either arm is unhealthy.
8. Capture calibration if no file was loaded.
9. Optionally save captured calibration.
10. Print an enable summary: targets, mode, profile, frequency, gains,
   calibration source, gripper mirroring, and report path.
11. In production profile, ask for explicit operator confirmation unless
   `--yes` is present.
12. Revalidate current master/slave posture against the loaded or captured
    calibration.
13. Check whether Ctrl+C cancellation was requested before enabling; if yes,
    exit cleanly without enabling.
14. Enable MIT mode on both arms.
15. If enable fails, cleanup must cover any arm that may have received an enable
    or MIT-mode command, even if confirmation failed before the SDK returned an
    `Active` type-state value. The CLI reports non-zero and does not continue.
16. Check whether Ctrl+C cancellation was requested during enable; if yes,
    disable both arms and exit cleanly.
17. Take an active snapshot and revalidate posture before the first normal
    controller output; on mismatch, disable both arms and exit non-zero.
18. Start runtime console input.
19. Run the dual-arm loop until `quit`, Ctrl+C, max iterations, or a fault.
20. Print and optionally save the final report.
21. Exit with zero only for clean standby exits.

No step after connection should command motion before calibration and MIT enable
confirmation complete.

The CLI must not implement custom partial-enable sequencing unless it also owns
explicit cleanup for every arm that may have received an enable or mode command.
When using `DualArmStandby::enable_mit`, the implementation must preserve the
SDK's partial-active drop/disable safety behavior and must not bypass it. If the
SDK cannot prove cleanup after an enable command was sent but confirmation
failed, the implementation must add or use an SDK cleanup path before this CLI
can be considered production-safe.

## Runtime Controller

The existing `run_bilateral` API accepts a fixed `BilateralController`. Runtime
mode and gain changes require a controller that reads mutable settings on each
tick.

Add a CLI-local `RuntimeTeleopController` implementing
`piper_client::dual_arm::BilateralController`.

Responsibilities:

- hold the current teleop mode
- hold current gains
- compute master-follower commands equivalent to `MasterFollowerController`
- compute bilateral commands equivalent to `JointSpaceBilateralController`
- apply runtime mode/gain updates on the next tick

This keeps the SDK realtime loop unchanged. The CLI controller only affects
which `BilateralCommand` is produced for a snapshot.

Runtime settings should be shared through a small synchronization primitive
such as `Arc<RwLock<RuntimeTeleopSettings>>` or a channel plus local cached
settings. The implementation should keep lock duration short and avoid blocking
inside `tick`.

## Runtime Console

The console is intentionally small:

```text
status
mode master-follower
mode bilateral
gain track-kp <value>
gain track-kd <value>
gain master-damping <value>
gain reflection-gain <value>
quit
```

Semantics:

- `status` prints current mode, gains, elapsed time, and whether cancellation
  has been requested.
- `mode` changes runtime controller behavior on the next tick.
- `gain` validates finite values within the hard safety caps before applying.
  Invalid runtime updates leave the prior setting unchanged.
- `quit` sets the same cancellation flag used by Ctrl+C.
- Unknown commands print help and do not affect control.

The console must not send raw motor commands directly. It only changes runtime
settings or requests cancellation.

First-version `status` must not claim live `BilateralRunReport` counters. The
current SDK returns `BilateralRunReport` only after `run_bilateral` exits. If
live loop counters become required later, add an explicit telemetry/shared
metrics mechanism rather than reading report state that does not exist.

## Safety Profiles

Hard limits apply to CLI arguments, config values, and runtime console updates.
They are intentionally stricter than the low-level SDK can represent:

| Parameter | v1 limit |
|-----------|----------|
| `frequency_hz` | `10.0 <= value <= 500.0` |
| `track_kp` | `0.0 <= value <= 20.0` |
| `track_kd` | `0.0 <= value <= 5.0` |
| `master_damping` | `0.0 <= value <= 2.0` |
| `reflection_gain` | `0.0 <= value <= 0.5` |
| `calibration_max_error_rad` | `0.0 < value <= 0.05` |
| `max_inter_arm_skew` | `<= 10 ms` on physical hardware |
| `safe_hold_max_duration` | `<= 100 ms` on physical hardware |
| `consecutive_read_failures_before_disable` | `<= 3` on physical hardware |

All numeric values must be finite. Invalid values fail before connection, or for
runtime console updates, are rejected without changing the active setting.

Production profile defaults:

- mode: `master-follower`
- frequency: `200.0 Hz`
- timing mode: platform default from `BilateralLoopConfig`
- warmup cycles: SDK default
- `DualArmSafetyConfig::safe_hold_max_duration`: about `100 ms`
- conservative read-failure threshold from SDK default
- max inter-arm skew: SDK default `10 ms`
- gripper mirror enabled
- startup confirmation required unless `--yes`
- clean Ctrl+C or `quit` exits through disable both
- runtime transport or submission faults use SDK fault shutdown

Debug profile:

- may allow `--max-iterations`
- may use more verbose logs
- may use sleep timing for easier debugging
- uses the same physical-hardware safety caps as production
- must still converge to disable or fault shutdown

Neither profile may leave both arms torque-enabled without a bounded shutdown
path.

Any future simulator-only profile that relaxes these physical-hardware caps must
be explicitly named as simulator-only and must not be selectable for real
hardware targets.

## Reports

Human-readable exit summary:

- exit reason
- elapsed time
- mode at exit
- profile
- iterations
- read faults
- submission faults
- last failed submission arm
- whether a peer command may have applied
- max inter-arm skew
- max real dt
- max cycle lag
- deadline misses
- master/slave TX frames sent
- master/slave realtime overwrites
- master/slave fault aborts
- stop attempt results
- last error

Optional JSON report from `--report-json`:

- command version/schema version
- resolved targets
- platform
- profile
- initial mode
- final mode
- control parameters
- calibration source and metadata
- runtime metrics from `BilateralRunReport`
- clean/faulted exit classification

JSON report schema v1 uses stable CLI-owned field names rather than serializing
SDK structs directly. Durations are integer microseconds with `_us` suffix.
Enums use snake_case strings. SDK `left` metrics are reported as `master`; SDK
`right` metrics are reported as `slave`.

Example:

```json
{
  "schema_version": 1,
  "command": "teleop dual-arm",
  "platform": "linux",
  "targets": {
    "master": "socketcan:can0",
    "slave": "socketcan:can1"
  },
  "profile": "production",
  "mode": {
    "initial": "master_follower",
    "final": "bilateral"
  },
  "control": {
    "frequency_hz": 200.0,
    "track_kp": 8.0,
    "track_kd": 1.0,
    "master_damping": 0.4,
    "reflection_gain": 0.25,
    "gripper_mirror": true
  },
  "calibration": {
    "source": "file",
    "path": "calibration.toml",
    "created_at_unix_ms": 1770000000000,
    "max_error_rad": 0.05
  },
  "exit": {
    "clean": true,
    "reason": "cancelled",
    "faulted": false,
    "last_error": null
  },
  "metrics": {
    "iterations": 12000,
    "read_faults": 0,
    "submission_faults": 0,
    "last_submission_failed_role": null,
    "peer_command_may_have_applied": false,
    "deadline_misses": 0,
    "max_inter_arm_skew_us": 1200,
    "max_real_dt_us": 5100,
    "max_cycle_lag_us": 200,
    "master_tx_frames_sent_total": 72000,
    "slave_tx_frames_sent_total": 72000,
    "master_tx_realtime_overwrites_total": 0,
    "slave_tx_realtime_overwrites_total": 0,
    "master_tx_fault_aborts_total": 0,
    "slave_tx_fault_aborts_total": 0,
    "master_stop_attempt": "not_attempted",
    "slave_stop_attempt": "not_attempted",
    "master_runtime_fault": null,
    "slave_runtime_fault": null
  }
}
```

Required null rules:

- absent optional data is encoded as `null`, not omitted
- `last_submission_failed_role` is `master`, `slave`, or `null`
- `last_error`, `master_runtime_fault`, and `slave_runtime_fault` are strings
  or `null`

JSON report writes should be best-effort only after both arms have already
entered standby or faulted state. Report I/O failure must not keep arms enabled.

## Error Handling

Fail before connecting when:

- CLI arguments are internally inconsistent
- config file is malformed
- required target is missing
- master and slave resolve to the same target
- numeric parameters are non-finite or out of range

Fail before MIT enable when:

- either arm cannot connect
- runtime health is unhealthy
- calibration cannot load or capture
- loaded calibration does not match the current master/slave posture within the
  configured max joint error
- operator confirmation is declined

During control:

- Ctrl+C and `quit` set cancellation and should exit through standby.
- read faults use SDK safe-hold/disable behavior.
- controller errors exit through safe-hold/disable.
- submission faults and runtime transport faults use SDK fault shutdown.

Exit code policy:

- `0`: `report.exit_reason` is exactly `Cancelled` or `MaxIterations`
- non-zero: config failure, connection failure, calibration failure, controller
  fault, read fault, compensation fault, submission fault, runtime transport
  fault, runtime manual fault, missing/unknown exit reason, or any `Faulted`
  loop exit

The CLI must classify process status by `report.exit_reason`, not only by
`DualArmLoopExit::Standby` vs `DualArmLoopExit::Faulted`. The SDK can return
`Standby` for `ReadFault`, `ControllerFault`, and `CompensationFault` after it
has safely disabled both arms; these are still failed sessions.

## Testability Architecture

The production command should keep clap parsing and terminal I/O thin. The core
teleop startup workflow must depend on a small injectable backend/factory trait
instead of directly constructing real hardware sessions inside every branch.

The production implementation wraps:

- `DualArmBuilder`
- `DualArmStandby`
- `DualArmActiveMit`
- `DualArmObserver`

The test implementation can fake:

- connect success/failure
- standby snapshots
- calibration capture
- MIT enable success, confirmation failure after command dispatch, failure
  before/after one arm may have received an enable command, and cleanup
- active snapshots before entering the loop
- loop exits with specific `BilateralRunReport` values
- disable/fault-shutdown calls

This seam is required so default tests can verify safety ordering without
hardware. Pure functions are still preferred for parsing, config merging,
calibration math, report serialization, and exit-code classification.

## Testing Strategy

Default tests must not require hardware.

Unit tests:

- CLI/config precedence
- target exclusivity and duplicate-target detection
- rejection of `auto-*`, `gs-usb-auto`, and mixed GS-USB selector forms without
  pre-connect normalization
- rejection of concrete GS-USB runtime targets until SDK SoftRealtime dual-arm
  support exists
- production/debug profile mapping into `BilateralLoopConfig`
- rejection of out-of-range safety-critical numeric values
- calibration TOML roundtrip
- calibration validation failures
- loaded-calibration posture compatibility success/failure
- runtime console parser
- report serialization
- exit-code classification, including `Standby` reports with `ReadFault`,
  `ControllerFault`, and `CompensationFault`

Controller tests:

- `RuntimeTeleopController` master-follower output matches
  `MasterFollowerController` for the same snapshot and gains
- bilateral output matches `JointSpaceBilateralController`
- mode update takes effect on the next tick
- gain update takes effect on the next tick
- invalid updates are rejected before changing runtime state

CLI tests:

- `piper-cli teleop dual-arm --help`
- illegal target combinations fail fast
- invalid frequency/gain fails fast
- missing non-Linux target fails fast
- malformed calibration file fails fast before connecting
- existing `--save-calibration` destination fails fast before connecting
- loaded calibration that does not match current posture fails before MIT enable
- post-enable active-snapshot calibration mismatch disables both arms, does not
  enter `run_bilateral`, and exits non-zero
- Ctrl+C requested before MIT enable exits without enabling
- Ctrl+C requested during enable disables any active arm before exit
- enable command dispatch followed by confirmation failure disables or
  fault-shuts down any arm that may have received the enable/mode command before
  returning non-zero
- report write failure occurs only after disabled/faulted state and cannot keep
  either arm enabled
- submission/runtime fault reports produce non-zero exit and include stop attempt
  results

Manual hardware acceptance:

1. Connect two independent StrictRealtime CAN links.
2. Run `master-follower` at default gains.
3. Capture calibration manually.
4. Verify every mirrored joint direction.
5. Run for several minutes with zero read/submission faults.
6. Switch to bilateral mode with low reflection gain.
7. Verify soft-contact reflection direction.
8. Trigger Ctrl+C and confirm clean disable.
9. Disconnect one feedback path and confirm bounded shutdown.

## Documentation

Add an operator guide under `apps/cli` or `docs/v0` covering:

- wiring and target selection
- startup order
- calibration procedure
- recommended first-run parameters
- runtime console commands
- report interpretation
- fault response expectations
- manual acceptance checklist

The guide should explicitly state that two independent StrictRealtime SocketCAN
links are the supported topology for the first version.

## Open Implementation Notes

- Teleop config target fields are string specs parsed through
  `TargetSpec::from_str`; do not deserialize them as `TargetSpec` TOML tables.
- `RuntimeTeleopController` should stay inside the CLI unless it proves useful
  as a reusable SDK abstraction.
- The injectable workflow backend/factory is a CLI testing seam, not a new public
  SDK abstraction unless implementation proves it should be shared.
- If the runtime console needs richer live metrics later, add an explicit
  telemetry channel. Do not make the controller write to stdout from `tick`.
- MuJoCo dynamics compensation remains out of scope for this CLI first version.
  A later extension can add optional compensation after the base program is
  stable.

## Acceptance Criteria

- `piper-cli teleop dual-arm --help` documents master/slave target options,
  modes, profiles, calibration, and reports.
- CLI rejects conflicting or duplicate arm targets before connecting.
- CLI rejects non-concrete auto targets for dual-arm teleop v1.
- CLI supports StrictRealtime SocketCAN dual-arm runtime targets in v1.
- CLI parses concrete GS-USB target syntax but rejects GS-USB runtime execution
  with an explicit SDK SoftRealtime dual-arm prerequisite error.
- CLI validates malformed calibration files and existing save destinations before
  connecting.
- CLI can run with manual calibration and with calibration loaded from TOML.
- Loaded calibration files are checked against current arm posture before MIT
  enable, using a bounded max joint error.
- Loaded and captured calibration are rechecked immediately before MIT enable
  and again after MIT enable before normal controller output.
- CLI can save a captured calibration without overwriting existing files.
- Default mode is `master-follower`.
- `bilateral` mode requires explicit config or CLI selection.
- Ctrl+C handling is installed before MIT enable, and cancellation during enable
  cannot leave a partially enabled arm running.
- Enable confirmation failure after command dispatch cannot leave an arm that
  may have received enable/mode commands running.
- Safety-critical numeric inputs and runtime updates are finite and within the
  documented hard caps.
- Runtime console supports `status`, `mode`, `gain`, and `quit`.
- Runtime mode/gain updates affect the next controller tick without restarting
  the session.
- Ctrl+C and `quit` converge through SDK cancellation and disable paths.
- All exit reasons except `Cancelled` and `MaxIterations` produce non-zero
  process status, even if the SDK returned `DualArmLoopExit::Standby`.
- Clean cancellation prints a final report and exits zero.
- JSON report output follows schema version 1 with master/slave field names and
  explicit duration units.
- Default automated tests require no hardware.
- Mock-backed workflow tests cover enable cancellation, enable-confirmation
  failure after command dispatch, pre-enable calibration mismatch, post-enable
  calibration mismatch, and post-disable report write failure.
- Hardware bring-up and acceptance steps are documented.
