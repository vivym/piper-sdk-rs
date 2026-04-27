# SVSPolicy Data-Collection Teleoperation Design

## Summary

Build a research-specific `piper-svs-collect` binary for collecting the
bilateral impedance teleoperation demonstrations described in the SVSPolicy
paper draft. The tool uses two physical Piper arms: a master arm operated by a
human and a slave arm interacting with the task environment. It records motion,
gripper state, MuJoCo-derived sensorless residuals, end-effector-frame cue
signals, contact state, and the continuous translational stiffness label
`K_tele`.

This first implementation is a data-collection system, not the full policy
training or deployment stack. It deliberately excludes camera capture, VLA
training, policy action chunk execution, GS-USB runtime support, and the final
fast Cartesian impedance executor. It preserves timestamp and schema hooks so
those later systems can align to the collected robot data without changing the
core episode format.

MuJoCo is required for this paper-specific collector, but it must not become a
default dependency of `piper-cli` or the normal workspace. The collector lives
as an excluded addon-style crate and is built with its own manifest.

## Goals

- Add a dedicated `piper-svs-collect` binary for SVSPolicy demonstration
  collection.
- Keep the normal `piper-cli` and default workspace free of MuJoCo native
  dependencies.
- Reuse the existing StrictRealtime dual-arm SocketCAN bring-up, MIT control
  loop, calibration, bounded shutdown, and report semantics.
- Use MuJoCo as the model-torque, end-effector pose, and Jacobian provider.
- Compute sensorless residuals as measured joint torque minus MuJoCo model
  torque.
- Map master and slave residuals into end-effector-frame cue vectors with a
  damped least-squares Jacobian-transpose inverse.
- Maintain a continuous cue-driven translational stiffness state `K_tele`.
- Let `K_tele` affect the data-collection teleoperation controller while
  recording it as the stiffness supervision label.
- Persist each run as a strict, versioned `SvsEpisode v1` directory.
- Provide deterministic unit and integration tests without hardware, plus a
  documented manual HIL acceptance path.

## Non-Goals

- Do not add MuJoCo to `piper-cli`, `piper-control`, or the default workspace
  dependency graph.
- Do not make `piper-cli teleop dual-arm` paper-specific.
- Do not implement camera capture in this phase.
- Do not implement VLA training, dataset loaders for training frameworks, or
  policy deployment.
- Do not implement the final fast Cartesian impedance executor for
  `(q^d, g, K^x)` policy action chunks.
- Do not support GS-USB or SoftRealtime dual-arm runtime execution.
- Do not introduce manual discrete stiffness modes such as `soft` or `hard`.
- Do not claim calibrated end-effector force estimation or force tracking.

## Existing Context

The repository already contains a production-oriented dual-arm teleoperation
CLI design and implementation:

- `docs/superpowers/specs/2026-04-27-isomorphic-dual-arm-teleop-cli-design.md`
- `apps/cli/TELEOP_DUAL_ARM.md`
- `apps/cli/src/commands/teleop.rs`
- `apps/cli/src/teleop/*`
- `crates/piper-client/src/dual_arm.rs`

The existing dual-arm runtime already provides:

- `DualArmBuilder`
- `DualArmStandby`
- `DualArmActiveMit`
- `DualArmObserver`
- `DualArmSnapshot`
- `DualArmCalibration`
- `JointMirrorMap`
- `BilateralLoopConfig`
- `DualArmSafetyConfig`
- `BilateralController`
- `BilateralDynamicsCompensator`
- `BilateralDynamicsCompensation`
- `DualArmLoopExit`
- `BilateralRunReport`

The MuJoCo addon is currently excluded from the default workspace:

- `addons/piper-physics-mujoco`

It already provides gravity and inverse-dynamics compensation, including a
dual-arm compensation bridge in `addons/piper-physics-mujoco/src/dual_arm.rs`.
That addon is the correct place to depend on MuJoCo native libraries. The SVS
collector should follow the same dependency-isolation pattern.

## Architecture Boundary

The normal SDK and CLI remain lightweight:

- `piper-client` may receive MuJoCo-free generic hooks only if the existing
  `BilateralController` and `BilateralDynamicsCompensator` interfaces are not
  sufficient.
- `piper-control` may continue to provide target/config helpers only.
- `piper-cli` remains the general-purpose operator CLI and does not link
  MuJoCo.

The research collector lives outside the default workspace, under an addon-style
path:

```text
addons/piper-svs-collect/
```

It builds a binary named `piper-svs-collect` and may depend on:

- `piper-sdk`
- `piper-client`
- `piper-tools`
- `addons/piper-physics-mujoco`
- serialization and CLI crates needed only by the collector

It is intentionally not included in `[workspace].members` during this phase.
The repository root should add `addons/piper-svs-collect` to
`[workspace].exclude` so default workspace commands cannot accidentally pull in
MuJoCo through path discovery or future workspace edits. The normal command:

```bash
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

must not require MuJoCo. The collector has its own explicit verification command
using `--manifest-path addons/piper-svs-collect/Cargo.toml`.

## Command Shape

Installed binary:

```bash
piper-svs-collect [OPTIONS]
```

Development invocation:

```bash
cargo run --manifest-path addons/piper-svs-collect/Cargo.toml -- \
  --master-target socketcan:can0 \
  --slave-target socketcan:can1 \
  --model-dir /path/to/piper/mujoco/model \
  --task-profile profiles/wiping.toml \
  --output-dir data/svs
```

Core options:

```text
--master-target <socketcan:IFACE>
--slave-target <socketcan:IFACE>
--baud-rate <BAUD>
--model-dir <PATH>
--use-standard-model-path
--use-embedded-model
--task-profile <PATH>
--output-dir <DIR>
--calibration-file <PATH>
--save-calibration <PATH>
--operator <NAME>
--task <NAME>
--notes <TEXT>
--raw-can
--disable-gripper-mirror
--max-iterations <N>
--timing-mode <sleep|spin>
--yes
```

The initial runtime supports only concrete StrictRealtime SocketCAN targets.
GS-USB selectors are not accepted by this collector until a separate
SoftRealtime dual-arm SDK design exists.

Exactly one MuJoCo model source may be selected. `--model-dir`,
`--use-standard-model-path`, and `--use-embedded-model` are mutually exclusive.
If none is supplied, the collector defaults to `--use-standard-model-path`.

Configuration precedence is:

1. CLI arguments
2. task profile TOML
3. built-in collector defaults

For example, `[gripper].mirror_enabled = true` in the task profile enables
gripper mirroring by default, while `--disable-gripper-mirror` forces it off for
that run. CLI metadata flags such as `--task`, `--operator`, and `--notes`
override profile/default metadata for the episode manifest.

## Runtime Data Flow

Startup:

1. Parse collector config, target specs, task profile, and output directory.
2. Validate that master and slave targets are distinct StrictRealtime SocketCAN
   interfaces.
3. Load or capture dual-arm calibration.
4. Load two MuJoCo calculators from the selected model source.
5. Create an episode directory with an initial `manifest.toml` in `running`
   status.
6. Start the asynchronous episode writer.
7. Connect both arms and enable MIT mode after operator confirmation.

Per 200 Hz control tick:

1. Read a fresh `DualArmSnapshot` from the existing dual-arm loop.
2. Compute MuJoCo model torques for master and slave.
3. Compute residual torques:

   ```text
   tau_residual = tau_measured - tau_model_mujoco
   ```

4. Compute end-effector pose and translational Jacobian for master and slave.
5. Map residual torques into end-effector-frame cue vectors.
6. Update the contact gate and `SvsStiffnessState`.
7. Generate the data-collection teleoperation command.
8. Enqueue an `SvsStepV1` record to the writer without blocking the realtime
   loop.
9. Let the existing dual-arm loop apply output shaping, torque limits, slew
   limits, passivity damping, submission ordering, and shutdown behavior.

Shutdown:

1. Disable or fault-shutdown both arms through the existing dual-arm runtime.
2. Flush the episode writer.
3. Write `report.json`.
4. Rewrite `manifest.toml` with final status `complete`, `cancelled`, or
   `faulted`.

## MuJoCo Bridge

The collector defines an `SvsMujocoBridge` inside the addon crate. It must keep
MuJoCo-specific logic out of default workspace crates.

The bridge provides per-arm:

- model torque
- measured-minus-model residual
- end-effector position in base frame
- end-effector rotation matrix
- translational Jacobian in base frame
- condition metrics for Jacobian DLS mapping

All rotation matrices in this spec use the notation `R_target_from_source`,
meaning they left-multiply a vector expressed in `source` coordinates and return
the same vector expressed in `target` coordinates. The bridge exposes
`R_slave_base_from_ee`, whose columns are the slave end-effector axes expressed
in the slave base frame. The collector therefore uses
`R_slave_ee_from_base = R_slave_base_from_ee^T` to express base-frame cues in
the slave end-effector frame.

The collector also requires a profile-provided
`R_slave_base_from_master_base` for master cue normalization. That transform is
a 3x3 proper rotation matrix mapping vectors from the master arm base frame into
the slave arm base frame. It is required in the collector config or task
profile; v1 must not infer it from the joint mirror map because joint signs are
not a complete Cartesian frame transform.

The bridge may be split into a standard
`BilateralDynamicsCompensator` implementation plus a single-slot
`SvsDynamicsFrame` side channel consumed by the SVS controller. This keeps the
existing dual-arm loop as the owner of timing, command submission, and safety.
If implementation requires a new SDK hook, that hook must be generic and
MuJoCo-free; it must not mention SVSPolicy, MuJoCo, or dataset writing.

MuJoCo pose and Jacobian access belongs in `addons/piper-physics-mujoco`, not
as duplicated MuJoCo FFI in `addons/piper-svs-collect`. The implementation
should add a narrow addon-local public API such as:

```rust
pub struct EndEffectorKinematics {
    pub position_base_m: [f64; 3],
    pub rotation_base_from_ee: [[f64; 3]; 3],
    pub translational_jacobian_base: [[f64; 6]; 3],
    pub jacobian_condition: f64,
}
```

This API remains outside the default workspace dependency graph because
`addons/piper-physics-mujoco` is still excluded. `piper-svs-collect` consumes
that API to build `SvsMujocoBridge`; it should not duplicate model loading or
raw `mj_jac` FFI logic.

`tau_model_mujoco` must be reproducible from the episode manifest and task
profile. V1 uses per-arm MuJoCo mode fields:

- `gravity`: model torque from zero-velocity gravity compensation
- `partial`: model torque from gravity plus velocity-dependent bias terms
- `full`: model torque from full inverse dynamics with finite-difference
  acceleration

The default v1 profile uses `master_mode = "gravity"` and
`slave_mode = "partial"`, matching the existing dual-arm MuJoCo example's
conservative intent: keep operator-side reflection simple and compensate
slave-side movement bias during contact. Payload and full-ID acceleration
filtering are explicit profile fields and are recorded in the manifest. If
`full` is selected for either arm, the collector must use the configured
finite-difference acceleration low-pass cutoff and acceleration clamp; otherwise
the run is rejected before MIT enable.

## Cue Definition

The paper's cue-driven formulation is implemented directly.

For each arm:

```text
tau_residual = tau_measured - tau_model_mujoco
```

Let `Jp` be the 3x6 translational Jacobian in the arm base frame. The collector
maps residual joint torques into a task-space cue with damped least squares:

```text
f_proxy_base = (Jp * Jp^T + lambda * I)^-1 * Jp * tau_residual
```

For the slave arm, the cue is expressed in the slave end-effector frame:

```text
R_slave_ee_from_base = R_slave_base_from_ee^T
r_ee = R_slave_ee_from_base * f_slave_proxy_base
```

For the master arm, the cue is first rotated into the slave base frame, then
expressed in the slave end-effector frame:

```text
u_slave_base = R_slave_base_from_master_base * f_master_proxy_base
u_ee = R_slave_ee_from_base * u_slave_base
```

The master-side cue is named `u_ee`. The slave-side cue is named `r_ee`.
Both have units of uncalibrated Newton-like proxy magnitude. The values are
used only as normalized interaction cues.

These values are not calibrated force estimates. They are robot-centric
interaction cues used for contact gating and compliance-intent state updates.
The spec and docs must avoid wording that implies accurate wrench
reconstruction.

## Stiffness State

The collector maintains a continuous translational stiffness state:

```text
K_tele = [kx, ky, kz]
```

The update rule is:

```text
contact_gate = hysteresis(norm(r_ee), enter_threshold, exit_threshold, min_hold)

phi_axis(v, mode, deadband, scale, limit) =
  clip(scale * deadband_transform(v, mode, deadband), -limit, limit)

K_raw =
  K_base(contact_state)
  + W_u * phi_u(u_ee)
  + W_r * phi_r(r_ee)

K_tele = clip(rate_limit(lpf(K_raw)), K_min, K_max)
```

All vectors are three-dimensional translational quantities expressed in the
slave end-effector frame. `K_min`, `K_max`, `K_base`, thresholds, filter cutoff,
rate limits, `W_u`, `W_r`, `phi_u`, and `phi_r` come from the task profile.
`K_tele` has units of N/m.

V1 supports only these deterministic `deadband_transform` modes:

- `signed`: `sign(v) * max(abs(v) - deadband, 0)`
- `absolute`: `max(abs(v) - deadband, 0)`
- `positive`: `max(v - deadband, 0)`
- `negative`: `max(-v - deadband, 0)`

The mapping functions are task-axis mappings. They are not required to be
monotone in every direction after multiplication by `W_u` and `W_r`, because
high interaction intensity can indicate either a need for stronger support or a
need to soften an axis to avoid jamming.

Manual discrete stiffness modes are not part of v1. Runtime profile tuning is
also deferred in v1: profile parameters are immutable for an episode. Runtime
controls may pause, stop, or print status, but they must not change stiffness
profile parameters or create operator-selected `soft` or `hard` labels.

`max_jacobian_condition` applies to the singular-value condition number of the
undamped translational Jacobian `Jp`. The DLS damping term improves numerical
stability, but it must not hide a rejected near-singular kinematic state.

## Task Profiles

Task profiles are TOML files. Initial profiles should cover:

- wiping
- peg insertion
- surface following

Profile shape:

```toml
[stiffness]
k_min = [50.0, 50.0, 50.0]
k_max = [800.0, 800.0, 800.0]
k_base_free = [120.0, 120.0, 120.0]
k_base_contact = [220.0, 220.0, 180.0]
lpf_cutoff_hz = 8.0
max_delta_per_second = [300.0, 300.0, 300.0]

[contact]
residual_enter = 3.0
residual_exit = 1.5
min_hold_ms = 80

[frames]
master_to_slave_rotation = [
  [1.0, 0.0, 0.0],
  [0.0, 1.0, 0.0],
  [0.0, 0.0, 1.0],
]

[cue]
dls_lambda = 0.01
max_jacobian_condition = 250.0
master_lpf_cutoff_hz = 20.0
slave_lpf_cutoff_hz = 20.0
w_u = [[0.0, 0.0, 0.0], [0.0, 0.0, 0.0], [0.0, 0.0, 0.0]]
w_r = [[0.0, 0.0, 0.0], [0.0, 0.0, 0.0], [0.0, 0.0, 0.0]]

[dynamics]
master_mode = "gravity"
slave_mode = "partial"
qacc_lpf_cutoff_hz = 20.0
max_abs_qacc = 50.0

[dynamics.master_payload]
mass_kg = 0.0
com_m = [0.0, 0.0, 0.0]

[dynamics.slave_payload]
mass_kg = 0.0
com_m = [0.0, 0.0, 0.0]

[cue.master_phi]
mode = ["signed", "signed", "signed"]
deadband = [0.2, 0.2, 0.2]
scale = [1.0, 1.0, 1.0]
limit = [10.0, 10.0, 10.0]

[cue.slave_phi]
mode = ["signed", "signed", "signed"]
deadband = [0.2, 0.2, 0.2]
scale = [1.0, 1.0, 1.0]
limit = [10.0, 10.0, 10.0]

[control]
track_kp_min = [2.0, 2.0, 2.0, 1.0, 1.0, 1.0]
track_kp_max = [10.0, 10.0, 10.0, 4.0, 4.0, 4.0]
track_kd = [1.0, 1.0, 1.0, 0.4, 0.4, 0.4]
reflection_gain_min = [0.05, 0.05, 0.05, 0.02, 0.02, 0.02]
reflection_gain_max = [0.30, 0.30, 0.30, 0.10, 0.10, 0.10]
joint_stiffness_projection = [
  [1.0, 0.0, 0.0],
  [0.0, 1.0, 0.0],
  [0.0, 0.0, 1.0],
  [0.3, 0.3, 0.0],
  [0.0, 0.3, 0.3],
  [0.3, 0.0, 0.3],
]
reflection_projection = [
  [1.0, 0.0, 0.0],
  [0.0, 1.0, 0.0],
  [0.0, 0.0, 1.0],
  [0.3, 0.3, 0.0],
  [0.0, 0.3, 0.3],
  [0.3, 0.0, 0.3],
]
reflection_residual_deadband = 3.0
reflection_residual_attenuation = 0.15
reflection_residual_min_scale = 0.2

[gripper]
mirror_enabled = true
update_divider = 4
position_deadband = 0.02
effort_scale = 1.0

[writer]
queue_capacity = 8192
queue_full_stop_events = 10
queue_full_stop_duration_ms = 100
flush_timeout_ms = 5000
```

The numeric defaults are safe placeholders for validation and tests, not
paper-result hyperparameters. Real task profiles must be tuned through HIL
bring-up and committed as named profiles after validation.

## Phase 1 Control Coupling

`K_tele` participates in data-collection teleoperation, but it is not the final
deployment-time Cartesian impedance executor.

In v1:

- The slave command still follows the mirrored master joint reference.
- `K_tele` schedules slave tracking/compliance only through bounded
  joint-space tracking gains.
- `K_tele` and slave residual cues schedule master force-reflection gain.
- All resulting MIT commands remain subject to existing output shaping,
  per-joint torque limits, slew limits, and passivity damping.

The v1 mapping from translational stiffness to joint-space command parameters
is deterministic:

```text
alpha_xyz = clamp((K_tele - K_min) / (K_max - K_min), 0, 1)
alpha_joint = clamp(joint_stiffness_projection * alpha_xyz, 0, 1)
slave_kp = lerp(track_kp_min, track_kp_max, alpha_joint)
slave_kd = track_kd

reflection_alpha = clamp(reflection_projection * alpha_xyz, 0, 1)
residual_excess = max(norm(r_ee) - reflection_residual_deadband, 0)
residual_scale =
  max(reflection_residual_min_scale,
      1 / (1 + reflection_residual_attenuation * residual_excess))
master_reflection_gain =
  lerp(reflection_gain_min, reflection_gain_max, reflection_alpha)
  * residual_scale
```

`joint_stiffness_projection` and `reflection_projection` are 6x3 matrices from
the task profile. Rows are clamped after projection, so profiles can express
task-specific coupling while the runtime keeps every joint gain within the
configured bounds. V1 does not modulate slave feedforward torque from `K_tele`;
model compensation remains owned by the MuJoCo compensation path.

Each stiffness axis must satisfy `K_min[i] < K_max[i]`. Equal bounds are
rejected rather than treated as a constant axis because the normalized control
mapping above depends on a non-zero denominator.

The reflected master interaction torque reuses the existing joint-space
bilateral sign semantics, but uses the slave residual torque rather than raw
measured torque:

```text
slave_residual_for_reflection =
  tau_slave_measured - tau_slave_model_mujoco

mapped_slave_residual =
  calibration.slave_to_master_torque(slave_residual_for_reflection)

master_interaction_torque =
  -mapped_slave_residual * master_reflection_gain
```

`master_reflection_gain` is the per-joint vector computed above. The negative
sign matches the current `JointSpaceBilateralController` convention. The
EE-frame cues affect reflection only through gain scheduling and dataset labels;
v1 does not compute reflected master torque through `J^T f_ee`.

In v1, the collector does not implement:

```text
tau = J^T * Kx * e + J^T * Dx * edot + gravity
```

for policy deployment. That belongs to a later fast-executor spec. The v1
collector records `K_tele` as the continuous compliance-intent supervision
signal and uses it to keep the demonstration controller physically meaningful.

## Episode Format

Each run creates one directory:

```text
<output-dir>/<task-slug>/<episode-id>/
  manifest.toml
  steps.bin
  report.json
  raw_can/
    master.piperrec
    slave.piperrec
```

`raw_can/` exists only when `--raw-can` is passed.

`episode-id` is generated before the episode directory is created. V1 format:

```text
<utc-yyyymmddThhmmssZ>-<task-slug>-<12-hex-random>
```

`task-slug` is lowercase ASCII `[a-z0-9-]`, derived from the task name by
replacing non-alphanumeric runs with `-` and trimming leading/trailing `-`. The
random suffix comes from the OS RNG. If the generated directory already exists,
the collector retries with a new random suffix; after a bounded retry count it
fails before connecting hardware.

The directory path uses the same `task-slug`, not the raw task name. The
manifest records both the raw task name and the derived slug. If slug generation
would produce an empty slug, the task name is rejected during validation rather
than replaced with a fallback.

### `manifest.toml`

Contains:

- `schema_version = 1`
- `episode_id`
- task name
- operator
- notes
- start and end wall-clock timestamps
- final status: `running`, `complete`, `cancelled`, or `faulted`
- master/slave targets
- MuJoCo model source and content hash
- per-arm MuJoCo dynamics mode, payload, qacc filter, and qacc clamp
- task profile path and content hash
- gripper mirror configuration
- calibration hash
- collector binary version or git revision when available
- whether raw CAN side recording was enabled

Hashes use SHA-256. Task profile hash is the SHA-256 of the exact profile file
bytes loaded for the episode. MuJoCo model hash is:

- for `--model-dir` or `--use-standard-model-path`: SHA-256 over a canonical
  stream of all regular files under the resolved model directory, sorted by
  UTF-8 relative path, where each entry is `relative_path_len`, `relative_path`,
  `content_len`, and raw file content
- for `--use-embedded-model`: SHA-256 of the embedded model XML bytes

The manifest records the hash algorithm, resolved model source, and resolved
task profile path.

### `steps.bin`

`steps.bin` is a strict bincode 1.3 stream using fixed-int encoding and little
endian byte order, matching the repository's existing strict recording v3
style. Historical incompatible formats are rejected unless a new schema version
is declared.

V1 header fields:

```text
magic = "PIPERSVS"
schema_version = 1
encoding = "bincode-1.3-fixint-little-endian"
episode_id = <episode-id>
created_unix_ms = <u64>
step_record_type = "SvsStepV1"
```

After the header, the file stores repeated fixed-schema `SvsStepV1` records
until EOF. The record count is not trusted from the header; readers count records
while decoding and compare against `report.json`/`manifest.toml` metadata.

Each step contains:

- `step_index`
- elapsed monotonic timestamp in microseconds from episode start
- master and slave hardware timestamps from the control snapshot
- master and slave host receive timestamps
- inter-arm skew and feedback age
- master/slave joint positions
- master/slave joint velocities
- master/slave measured joint torques
- master/slave MuJoCo model torques
- master/slave residual joint torques
- master/slave end-effector poses
- master/slave translational Jacobians
- master cue `u_ee`
- slave cue `r_ee`
- contact gate state
- raw `K_raw`
- filtered/rate-limited/clipped `K_tele`
- command fields generated by the SVS controller before SDK output shaping
- gripper mirror enabled/disabled state, latest master/slave gripper state, and
  gripper command; unavailable hardware state is recorded explicitly as
  `unavailable`, not omitted
- writer and runtime diagnostic counters needed to interpret data quality

The step format does not include camera frames in v1. It provides enough timing
metadata for future camera logs to align by host monotonic time.

### `report.json`

Contains:

- dual-arm `BilateralRunReport` fields
- writer statistics
- dropped step count
- maximum writer queue depth
- final episode status
- final flush result
- fault classification

An episode may be used as successful training data only when:

- manifest final status is `complete` or an explicitly accepted `cancelled`
  collection
- dual-arm report has no read, submission, compensation, controller, or runtime
  transport fault
- writer flush succeeded
- dropped step count is zero

## Writer and Backpressure

The realtime control loop must not perform disk IO. It sends `SvsStepV1` records
to a bounded writer queue with non-blocking enqueue.

Backpressure policy:

- Queue capacity is configured in the collector profile.
- A single queue-full event increments a counter and marks the episode as data
  quality degraded.
- If queue-full events exceed the configured threshold or persist longer than
  the configured duration, the collector stops the episode through the normal
  bounded shutdown path.
- The final manifest cannot be `complete` if writer flush fails.

Final status mapping:

- `complete`: normal finite run completion, including `--max-iterations`, with
  no control fault, no writer queue-full event, no dropped step, and successful
  writer flush
- `cancelled`: operator-requested stop or Ctrl+C after a clean bounded shutdown
  and successful writer flush, with no writer queue-full event and no dropped
  step
- `faulted`: startup failure after manifest creation, MuJoCo/controller/runtime
  failure, any writer queue-full event, any dropped step, writer backpressure
  threshold exceeded, writer flush failure, output finalization failure, or any
  dual-arm fault report

If validation fails before the episode directory is created, no episode status
is written and the command exits with an error.

V1 has no partial-success final status. A run with any missing `SvsStepV1`
record is `faulted`, even if the missing count is below the threshold that
triggers early shutdown. Downstream training filters may inspect faulted
episodes manually, but the collector must not label them complete.

Optional raw CAN side recording is diagnostic only for training data quality:
failure to start raw CAN recording before MIT enable is a startup error, but a
raw CAN writer failure after the structured `steps.bin` writer has started does
not by itself change `complete` or `cancelled` to `faulted`. Such failures are
reported in `report.json` and `manifest.toml`; `steps.bin` remains the
authoritative dataset stream.

## Safety and Fault Handling

The collector is fail-closed. It must not silently degrade to motion-only or
raw-current-only behavior.

Hard failures:

- malformed config or task profile
- unsupported target type
- duplicate master/slave target
- missing or invalid MuJoCo model
- MuJoCo calculation failure
- non-finite model torque, residual, Jacobian, cue, `K_raw`, or `K_tele`
- Jacobian conditioning outside the configured limit
- calibration mismatch
- stale or incomplete control snapshot
- writer backpressure threshold exceeded
- output directory cannot be created or finalized

Failure response:

- Before MIT enable, fail without entering active control.
- After MIT enable, attempt safe hold or normal disable when possible.
- Submission faults and runtime transport faults use the existing dual-arm
  fault-shutdown path.
- Manifest/report writing is attempted after arms are disabled or faulted.

Ctrl+C is a clean operator cancellation only if the dual-arm loop exits through
normal cancellation and writer flush succeeds.

## Configuration Validation

All numeric profile values must be finite. Validation rejects:

- model source flags that are not mutually exclusive
- negative frequencies or filter cutoffs
- non-positive DLS lambda
- non-positive Jacobian condition threshold
- `K_min >= K_max` on any axis
- base stiffness outside `[K_min, K_max]`
- negative rate limits
- contact enter threshold less than or equal to exit threshold
- negative min-hold duration
- cue matrices with the wrong shape
- unsupported phi modes
- unsupported dynamics modes
- invalid gripper update divider, deadband, or effort scale
- negative payload mass or non-finite payload COM values
- `full` dynamics mode with missing qacc filter or acceleration clamp
- phi vectors with shapes other than three elements
- base-frame rotation matrices that are not finite, approximately orthonormal,
  and approximately determinant `+1`
- control projection matrices with shapes other than 6x3
- non-positive writer queue capacity, queue-full thresholds, or flush timeout
- control gains outside conservative hard limits

The `--yes` flag may skip confirmation prompts only. It must not skip target,
profile, calibration, model, output path, or safety validation.

## Testing

Unit tests without hardware:

- DLS cue mapping returns finite expected vectors for well-conditioned Jacobians.
- DLS rejects non-finite values and conditioning beyond the configured limit.
- Contact hysteresis honors enter, exit, and min-hold behavior.
- `SvsStiffnessState` applies LPF, rate limit, clipping, and finite checks.
- Task profile validation rejects invalid ranges and matrix shapes.
- `SvsEpisode v1` header and step round-trip.
- Writer queue backpressure marks degradation and trips the stop threshold.
- Controller command scheduling keeps gains and reflection within configured
  bounds.

Integration tests without hardware:

- Fake dual-arm snapshots plus fake MuJoCo provider run a full collector
  workflow and produce a valid episode directory.
- Fake MuJoCo failure maps to a non-complete episode and safe shutdown.
- Fake stale snapshot maps to the existing dual-arm read fault path.
- Fake writer flush failure prevents a `complete` manifest.
- Ctrl+C before and during active control produces `cancelled` only when flush
  succeeds.

Manual HIL acceptance:

1. Confirm two independent StrictRealtime SocketCAN links.
2. Run low-gain dry run with no contact and verify `K_tele` remains near
   free-space baseline.
3. Apply gentle slave contact and verify residual sign and contact gate.
4. Verify master-side reflection direction with conservative gain.
5. Record a short wiping episode and inspect phase-aligned stiffness traces.
6. Record a short peg-insertion episode and inspect contact-transition labels.
7. Press Ctrl+C and verify clean disable, writer flush, and `cancelled` report.
8. Disconnect one feedback path and verify bounded shutdown and `faulted`
   report.

## Verification Commands

Default workspace commands must remain MuJoCo-free:

```bash
cargo fmt --all -- --check
cargo test --workspace --all-targets --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

Collector-specific commands are run explicitly:

```bash
cargo test --manifest-path addons/piper-svs-collect/Cargo.toml --all-targets
cargo clippy --manifest-path addons/piper-svs-collect/Cargo.toml --all-targets --all-features -- -D warnings
```

No verification command may use `--no-verify` to bypass hooks or lint failures.

## Future Work

Future specs should cover:

- camera capture and robot-camera timestamp alignment
- Python or Arrow/Parquet exporters for training
- the deployment-time fast Cartesian impedance executor
- policy action chunk runtime and VLA bridge
- GS-USB / SoftRealtime dual-arm support
- dataset quality dashboards

These are deliberately out of scope for this first data-collection teleop spec.
