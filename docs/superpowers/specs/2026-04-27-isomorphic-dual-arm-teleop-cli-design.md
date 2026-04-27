# Isomorphic Dual-Arm Teleoperation CLI Design

## Summary

Build a production-oriented dual-arm teleoperation command in `piper-cli` for two
Piper arms. The command provides an operator-facing host program for isomorphic
teleoperation: one arm acts as the master, the other as the slave, with optional
bilateral force reflection after a conservative master-follower bring-up path.

The first implementation reuses the existing `piper-client::dual_arm` control
loop and controllers. It does not reimplement MIT frame emission in the CLI.
The CLI owns production concerns: target selection, configuration merging,
calibration loading/capture, startup confirmation, runtime console commands,
structured reports, and documented hardware bring-up.

## Goals

- Add a production CLI entry point for two physical Piper arms.
- Support master-follower and bilateral joint-space teleoperation.
- Support both independent SocketCAN interfaces and two GS-USB devices.
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

GS-USB usage:

```bash
piper-cli teleop dual-arm \
  --master-serial MASTER123 \
  --slave-serial SLAVE456 \
  --mode bilateral
```

Linux may default to `can0` and `can1` when neither interface nor serial is
specified. Non-Linux platforms must specify GS-USB serials unless a future
backend provides another explicit target type.

Each arm target is exclusive:

- master: exactly one of `--master-interface`, `--master-serial`, or inherited
  config/default target
- slave: exactly one of `--slave-interface`, `--slave-serial`, or inherited
  config/default target
- master and slave must not resolve to the same target

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
--save-calibration <PATH>
--report-json <PATH>
--yes
```

Target options:

```text
--master-interface <IFACE>
--slave-interface <IFACE>
--master-serial <SERIAL>
--slave-serial <SERIAL>
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
```

The teleop config should be separate from the existing single-arm default
target config. The single-arm CLI config is optimized for one Piper; dual-arm
teleoperation needs explicit per-role target resolution and should not infer a
slave arm from the single-arm target.

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

`--save-calibration` writes the captured calibration after successful capture.
It must not overwrite an existing file unless a future explicit overwrite flag
is added.

## Startup Flow

The command runs these phases:

1. Parse CLI and optional TOML config.
2. Resolve and validate master/slave targets.
3. Connect both arms through `DualArmBuilder`.
4. Read initial runtime health and fail if either arm is unhealthy.
5. Load calibration file or prompt for manual capture.
6. Optionally save captured calibration.
7. Print an enable summary: targets, mode, profile, frequency, gains,
   calibration source, gripper mirroring, and report path.
8. In production profile, ask for explicit operator confirmation unless
   `--yes` is present.
9. Enable MIT mode on both arms.
10. Start runtime console input and Ctrl+C handling.
11. Run the dual-arm loop until `quit`, Ctrl+C, max iterations, or a fault.
12. Print and optionally save the final report.
13. Exit with zero only for clean standby exits.

No step after connection should command motion before calibration and MIT enable
confirmation complete.

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

- `status` prints current mode, gains, elapsed time, last report counters that
  are available without blocking the control loop, and whether cancellation has
  been requested.
- `mode` changes runtime controller behavior on the next tick.
- `gain` validates finite non-negative values before applying.
- `quit` sets the same cancellation flag used by Ctrl+C.
- Unknown commands print help and do not affect control.

The console must not send raw motor commands directly. It only changes runtime
settings or requests cancellation.

## Safety Profiles

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
- may tolerate a longer read-failure window
- may use sleep timing for easier debugging
- must still converge to disable or fault shutdown

Neither profile may leave both arms torque-enabled without a bounded shutdown
path.

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
- operator confirmation is declined

During control:

- Ctrl+C and `quit` set cancellation and should exit through standby.
- read faults use SDK safe-hold/disable behavior.
- controller errors exit through safe-hold/disable.
- submission faults and runtime transport faults use SDK fault shutdown.

Exit code policy:

- `0`: clean `Standby` exit caused by `Cancelled` or `MaxIterations`
- non-zero: config failure, connection failure, calibration failure, controller
  fault, submission fault, runtime transport fault, or faulted exit

## Testing Strategy

Default tests must not require hardware.

Unit tests:

- CLI/config precedence
- target exclusivity and duplicate-target detection
- production/debug profile mapping into `BilateralLoopConfig`
- calibration TOML roundtrip
- calibration validation failures
- runtime console parser
- report serialization

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
- malformed calibration file fails fast without connecting when possible

Manual hardware acceptance:

1. Connect two independent CAN links or two GS-USB devices.
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

The guide should explicitly state that two independent buses/devices are the
recommended topology for the first version.

## Open Implementation Notes

- The CLI should reuse `piper_control::TargetSpec` if it can represent both
  per-arm targets cleanly. If not, add a small teleop-specific target parser
  that converts into `PiperBuilder`.
- `RuntimeTeleopController` should stay inside the CLI unless it proves useful
  as a reusable SDK abstraction.
- If the runtime console needs richer live metrics later, add an explicit
  telemetry channel. Do not make the controller write to stdout from `tick`.
- MuJoCo dynamics compensation remains out of scope for this CLI first version.
  A later extension can add optional compensation after the base program is
  stable.

## Acceptance Criteria

- `piper-cli teleop dual-arm --help` documents master/slave target options,
  modes, profiles, calibration, and reports.
- CLI rejects conflicting or duplicate arm targets before connecting.
- CLI supports SocketCAN and GS-USB dual-arm target selection.
- CLI can run with manual calibration and with calibration loaded from TOML.
- CLI can save a captured calibration without overwriting existing files.
- Default mode is `master-follower`.
- `bilateral` mode requires explicit config or CLI selection.
- Runtime console supports `status`, `mode`, `gain`, and `quit`.
- Runtime mode/gain updates affect the next controller tick without restarting
  the session.
- Ctrl+C and `quit` converge through SDK cancellation and disable paths.
- Fault exits produce non-zero process status.
- Clean cancellation prints a final report and exits zero.
- JSON report output is supported.
- Default automated tests require no hardware.
- Hardware bring-up and acceptance steps are documented.
