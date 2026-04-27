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
collector must follow the same dependency-isolation pattern.

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
The repository root must add `addons/piper-svs-collect` to
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
--calibration-max-error-rad <RAD>
--mirror-map <left-right|PATH>
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

## Calibration and Mirror Map

Safe teleoperation requires an explicit `JointMirrorMap`. The task profile or
CLI must select one of:

- `mirror_map_kind = "left-right"` to use
  `JointMirrorMap::left_right_mirror()`
- `mirror_map_kind = "custom"` plus a resolved `[calibration.mirror_map]`
  table with `permutation`, `position_sign`,
  `velocity_sign`, and `torque_sign`

`--mirror-map` overrides the task profile. The collector validates that the
permutation contains each joint exactly once and every sign is exactly `-1.0`
or `1.0`. V1 must not capture calibration with an implicit or guessed mirror
map.

The effective profile always serializes the resolved `JointMirrorMap` inline as
`[calibration.mirror_map]` regardless of whether the source was `left-right` or a
custom file. `mirror_map_kind` records the source selection; the inline table is
the value used by calibration, teleoperation, manifest metadata, and replay.

File-backed mirror maps are canonical TOML with `schema_version = 1`,
`permutation`, `position_sign`, `velocity_sign`, and `torque_sign` in that exact
order:

```toml
schema_version = 1
permutation = [0, 1, 2, 3, 4, 5]
position_sign = [-1.0, 1.0, 1.0, -1.0, 1.0, -1.0]
velocity_sign = [-1.0, 1.0, 1.0, -1.0, 1.0, -1.0]
torque_sign = [-1.0, 1.0, 1.0, -1.0, 1.0, -1.0]
```

`permutation` is a six-element zero-based array indexed by slave joint and
containing the corresponding master joint index, so `[0, 1, 2, 3, 4, 5]` is
identity order. File-backed mirror map serialization uses UTF-8, LF line
endings, a single trailing newline, no blank lines, table/field order exactly as
shown, fixed inline array order, base-10 integer formatting, and the same
floating-point formatting rule used by calibration files. The source mirror-map
hash recorded in the manifest is the SHA-256 of the exact loaded canonical bytes;
non-canonical loaded bytes are rejected before hardware connect.

Calibration files are canonical TOML with this exact field order:

```toml
schema_version = 1
created_unix_ms = 0
master_zero_rad = [0.0, 0.0, 0.0, 0.0, 0.0, 0.0]
slave_zero_rad = [0.0, 0.0, 0.0, 0.0, 0.0, 0.0]

[mirror_map]
permutation = [0, 1, 2, 3, 4, 5]
position_sign = [-1.0, 1.0, 1.0, -1.0, 1.0, -1.0]
velocity_sign = [-1.0, 1.0, 1.0, -1.0, 1.0, -1.0]
torque_sign = [-1.0, 1.0, 1.0, -1.0, 1.0, -1.0]
```

Canonical calibration serialization uses UTF-8, LF line endings, one blank line
before `[mirror_map]`, a single trailing newline, table/field order exactly as
shown, fixed array order, and the shortest decimal representation that
round-trips to the same IEEE-754 `f64` using the Rust `ryu` algorithm. The
episode manifest records the calibration hash as the SHA-256 of the exact loaded
calibration file bytes, or these exact canonical bytes for a newly captured
calibration even when `--save-calibration` is not used. When
`--calibration-file` is provided, the loaded bytes must be byte-identical to the
canonical serialization of the parsed calibration, the loaded mirror map must
exactly match the effective mirror map, and the current connected posture must
match the calibration zero poses within `calibration-max-error-rad` on every
joint. `--calibration-max-error-rad` is finite and positive; the default belongs
to the effective profile and is written to `effective_profile.toml`.

Every episode writes `calibration.toml` containing the exact canonical
calibration bytes whose SHA-256 is recorded in the manifest. This is required
even when `--calibration-file` was supplied and even when `--save-calibration`
is not used, so zero poses and mirror-map values are auditable from the episode
directory alone.

When capturing calibration without `--calibration-file`, the collector captures
from the connected `DualArmStandby` using the already validated effective mirror
map. `--save-calibration` writes the canonical bytes with no-overwrite
semantics: if the target path already exists, startup fails before MIT enable.
On Linux, the implementation must write and fsync a unique temporary file in the
same directory, then use an atomic no-replace persist operation such as
`linkat`/`renameat2(RENAME_NOREPLACE)` or `tempfile::persist_noclobber`, then
fsync the parent directory when supported. If the platform cannot provide a
no-overwrite persist primitive, this collector must fail rather than risk
overwriting an existing calibration.

## Runtime Data Flow

Startup:

1. Parse collector config, target specs, task profile, model source, and output
   directory.
2. Validate static inputs that do not depend on profile resolution or model
   loading: target syntax, distinct SocketCAN interfaces, model source
   selection, output parent directory, raw task name, and task profile TOML
   syntax.
3. Resolve the effective task profile after applying built-in defaults, profile
   TOML, and CLI overrides. Compute the effective profile hash.
4. Validate the effective task profile before hardware connect, including task
   slug length, calibration mirror map, scalar ranges, matrix shapes, and
   end-effector selector strings.
5. Load two MuJoCo calculators from the selected model source, resolve the
   configured master/slave end-effector sites against the loaded models, and
   compute the model hash.
6. Connect both arms and build `DualArmStandby`.
7. Resolve calibration:
   - with `--calibration-file`, load calibration and check current connected
     posture against `calibration-max-error-rad`;
   - without `--calibration-file`, capture calibration from the connected
     `DualArmStandby`, optionally write `--save-calibration`, then hash the
     captured calibration.
8. Create the episode directory, write `effective_profile.toml`, write the
   resolved canonical `calibration.toml`, and write an initial `manifest.toml`
   in `running` status. No episode directory is created before static
   validation, effective profile validation, MuJoCo site resolution, successful
   hardware connect, and calibration resolution.
9. Start the structured asynchronous episode writer.
10. If `--raw-can` is enabled, start raw CAN side recording. Failure before MIT
   enable is a startup error and finalizes the manifest as `faulted`.
11. Ask for operator confirmation unless `--yes` is set.
12. Enable MIT mode and enter the 200 Hz control loop.

Per 200 Hz control tick:

1. Read a fresh `DualArmSnapshot` from the existing dual-arm loop.
2. Compute MuJoCo model torques for master and slave.
3. Compute cue residual torques:

   ```text
   tau_slave_residual =
     tau_slave_measured - tau_slave_model_mujoco

   tau_master_effort_residual =
     tau_master_measured
     - tau_master_model_mujoco
     - tau_master_feedback_subtracted
   ```

   `tau_master_feedback_subtracted` is selected from the prior applied-command
   history. It is zero before the first matching rendered sample. The
   operator-effort cue must not include the system's own rendered feedback
   torque.
4. Compute end-effector pose and translational Jacobian for master and slave.
5. Map residual torques into raw end-effector-frame cue vectors and low-pass
   them into the filtered cues used by the controller.
6. Update the contact gate and `SvsStiffnessState` from the filtered cues.
7. Generate the data-collection teleoperation command.
8. Let the existing dual-arm loop apply output shaping, torque limits, slew
   limits, passivity damping, submission ordering, gripper mirroring, and
   shutdown behavior.
9. Capture the generic dual-arm loop telemetry for the shaped/applied command
   and gripper decision.
10. Enqueue an `SvsStepV1` record to the writer without blocking the realtime
   loop.

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
Any SDK hook added for this work must be generic and MuJoCo-free; it must not
mention SVSPolicy, MuJoCo, or dataset writing.

The collector needs shaped/applied command and gripper telemetry that the
current dual-arm loop does not expose through `BilateralController`. V1 must add
a MuJoCo-free generic hook in `piper-client`, for example an optional
`BilateralLoopTelemetrySink` on `BilateralLoopConfig`. The hook fires once per
accepted control tick after output shaping, passivity damping, torque assembly,
submission attempts, and gripper mirror decision. It must expose:

- the `BilateralControlFrame` input snapshot and optional compensation
- the controller command before SDK output shaping
- the shaped MIT command and final master/slave feedforward torques submitted
  to the SDK
- master/slave TX finished host monotonic timestamps from the TX worker after
  the MIT command batch has actually been sent
- post-quirk MIT `t_ref` values used for protocol encoding
- the master interaction torque that was actually rendered or attempted on the
  master side
- the time-aligned master/slave gripper availability, hardware timestamp, host
  receive timestamp, age, position, and effort read for this tick
- the gripper mirror decision, command value, and submission outcome
- generic loop timing fields including scheduler tick-start monotonic time,
  control-frame host monotonic time, previous control-frame host monotonic time,
  unclamped raw delta, clamped delta, nominal period, submission deadline, and
  deadline-missed status

The hook must be non-blocking from the loop's perspective. The collector uses it
only to fill `SvsStepV1`; the normal SDK and CLI must remain independent of SVS
or MuJoCo concepts.

These telemetry fields use generic SDK names, not SVS names. The collector maps
`control_frame_host_mono_us` to `SvsStepV1.host_mono_us` and
`clamped_dt_us` to `SvsStepV1.dt_us`. `scheduler_tick_start_host_mono_us`,
`previous_control_frame_host_mono_us`, `raw_dt_us`, `nominal_period_us`, and the
submission deadline are sink/report diagnostics in v1; they are not encoded in
`steps.bin`.

Confirmed MIT submission is opt-in. The existing dual-arm SDK loop and
`piper-cli teleop dual-arm` keep their current unconfirmed steady-state MIT send
path by default. The generic SDK change should add an explicit submission mode,
for example `BilateralSubmissionMode::Unconfirmed` as the default and
`BilateralSubmissionMode::ConfirmedTxFinished` for collectors that need
TX-finished telemetry. The SVS collector must select the confirmed mode; the
general CLI must not silently switch to confirmed sends.

The SVS collector must use a generic confirmed MIT submission path that exposes
TX-finished timestamps from the driver. A control tick is written to `steps.bin`
only after both arms have a successful TX-finished timestamp. If either arm
fails submission or no TX-finished timestamp is available within the tick's
bounded submission path, the collector exits through the dual-arm fault path and
must not write a successful-looking `SvsStepV1` for that tick.

TX-finished means the TX worker sampled
`piper_driver::heartbeat::monotonic_micros` after the final frame in that arm's
MIT command batch returned success from the CAN send path. It is not the current
`DeliveryPhase::Committed` timestamp, because that timestamp can be emitted
before the package frames are sent. The SVS path must use or add a confirmed
result type that returns:

```rust
pub struct MitBatchTxFinished {
    pub host_finished_mono_us: u64,
}
```

The wait for this result is bounded by the submission deadline through the
finished acknowledgement, not only through the pre-send commit point. Failure,
timeout, cancellation, or partial package delivery returns no
`MitBatchTxFinished` timestamp. Post-quirk MIT `t_ref` telemetry must be captured
from the same validated command batch whose frames were encoded for this
confirmed send.

The submission deadline for both arms in a control tick is
`tick_start_instant + nominal_period`. The slave/right arm is submitted first and
the master/left arm uses the remaining time before that same absolute deadline.
If the slave submission consumes the whole budget, the master submission is not
attempted, the tick faults, and no `SvsStepV1` is written. `deadline_missed` is
set to `1` only when tick execution or confirmed submission crosses this
absolute deadline before a fault exits the loop; otherwise it is `0`.

MuJoCo pose and Jacobian access belongs in `addons/piper-physics-mujoco`, not
as duplicated MuJoCo FFI in `addons/piper-svs-collect`. The implementation must
add a narrow addon-local public API such as:

```rust
pub struct EndEffectorSelector {
    pub site_name: String,
}

pub struct EndEffectorKinematics {
    pub position_base_m: [f64; 3],
    pub rotation_base_from_ee: [[f64; 3]; 3],
    pub translational_jacobian_base: [[f64; 6]; 3],
    pub jacobian_condition: f64,
}
```

This API remains outside the default workspace dependency graph because
`addons/piper-physics-mujoco` is still excluded. `piper-svs-collect` consumes
that API to build `SvsMujocoBridge`; it must not duplicate model loading or
raw `mj_jac` FFI logic.

The end-effector selector is part of the effective task profile and manifest.
V1 requires explicit master and slave MuJoCo site names. The bridge fails before
MIT enable if either site name is missing, resolves to zero or multiple sites,
or belongs to a model body that cannot provide a 6-DoF Jacobian. The collector
must not infer the end-effector site from naming heuristics.

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

For the slave arm:

```text
tau_slave_residual = tau_slave_measured - tau_slave_model_mujoco
```

For the master arm, the effort cue subtracts the known force-reflection torque
that the controller rendered to the operator:

```text
tau_master_effort_residual =
  tau_master_measured
  - tau_master_model_mujoco
  - tau_master_feedback_subtracted
```

This subtraction is required to avoid feeding the controller's own reflected
torque back into the operator-intent cue. The implementation keeps a ring buffer
of shaped/applied master interaction commands:

```text
AppliedMasterFeedback {
  master_tx_finished_host_mono_us,
  shaped_master_interaction_nm,
}
```

For a control snapshot with `master.dynamic_host_rx_mono_us`, the subtracted
feedback is the latest buffered `shaped_master_interaction_nm` whose
`master_tx_finished_host_mono_us <= master.dynamic_host_rx_mono_us`. If no such
record exists, it is `[0; 6]`. The timestamp must come from the TX worker after
the master MIT command batch has actually been sent, not from enqueue or
post-submission caller time. The implementation must not subtract the current
tick's newly computed shaped interaction torque, because that command is
produced after the snapshot being used for the cue.

Let `Jp` be the 3x6 translational Jacobian in the arm base frame. For each arm,
the collector maps the relevant cue residual torque into a raw task-space cue
with damped least squares:

```text
f_proxy_base_raw =
  (Jp * Jp^T + lambda^2 * I)^-1 * Jp * tau_cue_residual
```

`dls_lambda` in the task profile is the damping coefficient `lambda`; the
matrix equation uses `lambda^2 * I`.

For the slave arm, the cue is expressed in the slave end-effector frame:

```text
R_slave_ee_from_base = R_slave_base_from_ee^T
r_ee_raw = R_slave_ee_from_base * f_slave_proxy_base_raw
```

For the master arm, the cue is first rotated into the slave base frame, then
expressed in the slave end-effector frame:

```text
u_slave_base_raw =
  R_slave_base_from_master_base * f_master_proxy_base_raw
u_ee_raw = R_slave_ee_from_base * u_slave_base_raw
```

The raw cues are then low-pass filtered with the profile's
`master_lpf_cutoff_hz` and `slave_lpf_cutoff_hz`:

```text
u_ee = LPF_master(u_ee_raw)
r_ee = LPF_slave(r_ee_raw)
```

The filtered master-side cue is named `u_ee`. The filtered slave-side cue is
named `r_ee`. Both have units of uncalibrated Newton-like proxy magnitude. The
values are used only as normalized interaction cues.

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

K_state_raw =
  K_base(contact_state)
  + W_u * phi_u(u_ee)
  + W_r * phi_r(r_ee)

K_state_clipped = clip(K_state_raw, K_min, K_max)
K_tele = clip(rate_limit(lpf(K_state_clipped)), K_min, K_max)
```

All vectors are three-dimensional translational quantities expressed in the
slave end-effector frame. `K_min`, `K_max`, `K_base`, thresholds, filter cutoff,
rate limits, `W_u`, `W_r`, `phi_u`, and `phi_r` come from the task profile.
`K_tele` has units of N/m.

The cue-composed stiffness is clipped before filtering and rate limiting to
avoid filter windup from extreme cue spikes. The final clip is still required
because the LPF and rate limiter are stateful.

State updates are discrete and deterministic:

- The loop records a scheduler tick-start monotonic timestamp before snapshot
  reads for deadline diagnostics. `SvsStepV1.host_mono_us` is then sampled with
  `piper_driver::heartbeat::monotonic_micros` immediately after the
  `DualArmSnapshot` and time-aligned gripper state for that accepted post-warmup
  tick are read, before MuJoCo calculation, controller execution, command
  submission, or writer enqueue.
- `episode_elapsed_us = host_mono_us - episode_start_host_mono_us`; a monotonic
  timestamp earlier than the episode start is a hard failure.
- `nominal_period_us = round(1_000_000 / loop_frequency_hz)`, which is `5000`
  in v1.
- `raw_dt_us` is `host_mono_us - previous_control_frame_host_mono_us` after the first
  post-warmup SVS tick. The first post-warmup tick uses
  `raw_dt_us = nominal_period_us`; warmup tick timestamps do not seed
  `previous_control_frame_host_mono_us`.
- `max_dt_us = ceil(nominal_period_us * dt_clamp_multiplier)`.
- `dt_us = clamp(raw_dt_us, 1, max_dt_us)`. This clamped value is the `dt`
  passed to the SVS controller and written to `SvsStepV1`.
- `dt_sec = dt_us / 1_000_000`.
- All first-order LPFs use `rc = 1 / (2 * pi * cutoff_hz)`,
  `alpha = dt_sec / (rc + dt_sec)`, and `y = y + alpha * (x - y)`.
- Cue LPF states initialize to `[0, 0, 0]` at active-loop start.
- The stiffness LPF state and previous `K_tele` initialize to
  `clip(K_base_free, K_min, K_max)`.
- The rate limiter is per-axis:
  `delta = clamp(k_lpf - previous_K_tele,
  -max_delta_per_second * dt_sec, max_delta_per_second * dt_sec)`.
  Then `K_tele = clip(previous_K_tele + delta, K_min, K_max)`.
- Contact state initializes to `free`. Let
  `min_hold_ticks = max(1, ceil(min_hold_ms * loop_frequency_hz / 1000))`.
  In `free`, increment an enter counter while `norm(r_ee) >= residual_enter`
  and reset it otherwise; switch to `contact` when the counter reaches
  `min_hold_ticks`. In `contact`, increment an exit counter while
  `norm(r_ee) <= residual_exit` and reset it otherwise; switch to `free` when
  the counter reaches `min_hold_ticks`.

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

V1 runs the active control loop at exactly 200 Hz. `loop_frequency_hz` remains in
`effective_profile.toml` and `manifest.toml` as an explicit reproducibility
field, but validation rejects any value other than `200.0`.

`warmup_cycles` is bounded to `0..=100` and is recorded in the effective
profile. Warmup cycles are not dataset steps. During warmup the dual-arm loop
reads snapshots, updates timing diagnostics, and sends safe hold/mirrored
low-gain commands as needed by the existing runtime, but it does not call the
SVS controller, update cue/stiffness state, or write `SvsStepV1`. The first
post-warmup SVS controller tick is `step_index = 0`.

## Task Profiles

Task profiles are TOML files. Initial profiles must cover:

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

[calibration]
mirror_map_kind = "left-right"
calibration_max_error_rad = 0.05

[calibration.mirror_map]
permutation = [0, 1, 2, 3, 4, 5]
position_sign = [-1.0, 1.0, 1.0, -1.0, 1.0, -1.0]
velocity_sign = [-1.0, 1.0, 1.0, -1.0, 1.0, -1.0]
torque_sign = [-1.0, 1.0, 1.0, -1.0, 1.0, -1.0]

[mujoco]
master_ee_site = "master_tool_center"
slave_ee_site = "slave_tool_center"

[cue]
dls_lambda = 0.01
max_jacobian_condition = 250.0
master_lpf_cutoff_hz = 20.0
slave_lpf_cutoff_hz = 20.0
w_u = [
  [0.0, 0.0, 0.0],
  [0.0, 0.0, 0.0],
  [0.0, 0.0, 0.0],
]
w_r = [
  [0.0, 0.0, 0.0],
  [0.0, 0.0, 0.0],
  [0.0, 0.0, 0.0],
]

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

[control]
loop_frequency_hz = 200.0
dt_clamp_multiplier = 2.0
warmup_cycles = 3
track_kp_min = [2.0, 2.0, 2.0, 1.0, 1.0, 1.0]
track_kp_max = [10.0, 10.0, 10.0, 4.0, 4.0, 4.0]
track_kd = [1.0, 1.0, 1.0, 0.4, 0.4, 0.4]
master_kp = [0.0, 0.0, 0.0, 0.0, 0.0, 0.0]
master_kd = [0.3, 0.3, 0.3, 0.15, 0.15, 0.15]
reflection_gain_min = [0.05, 0.05, 0.05]
reflection_gain_max = [0.30, 0.30, 0.30]
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
]
reflection_residual_deadband = 3.0
reflection_residual_attenuation = 0.15
reflection_residual_min_scale = 0.2
master_interaction_lpf_cutoff_hz = 20.0
master_interaction_limit_nm = [1.5, 1.5, 1.5, 1.0, 1.0, 1.0]
master_interaction_slew_limit_nm_per_s = [50.0, 50.0, 50.0, 30.0, 30.0, 30.0]
master_passivity_enabled = true
master_passivity_max_damping = [1.0, 1.0, 1.0, 0.5, 0.5, 0.5]
slave_feedforward_limit_nm = [4.0, 4.0, 4.0, 2.5, 2.5, 2.5]

[gripper]
mirror_enabled = true
update_divider = 4
position_deadband = 0.02
effort_scale = 1.0
max_feedback_age_ms = 100

[writer]
queue_capacity = 8192
queue_full_stop_events = 10
queue_full_stop_duration_ms = 100
flush_timeout_ms = 5000
```

The numeric defaults and MuJoCo site names shown above are placeholders for
validation and tests, not paper-result hyperparameters or guaranteed model
names. Real task profiles must name sites that exist in the selected model, be
tuned through HIL bring-up, and be committed as named profiles after validation.
Source task profiles may omit `[calibration.mirror_map]` when
`mirror_map_kind = "left-right"`; `effective_profile.toml` never omits the
resolved mirror-map table.

## Phase 1 Control Coupling

`K_tele` participates in data-collection teleoperation, but it is not the final
deployment-time Cartesian impedance executor.

In v1:

- The slave command still follows the mirrored master joint reference.
- `K_tele` schedules slave tracking/compliance only through bounded
  joint-space tracking gains.
- `K_tele` and filtered slave residual cues schedule task-axis master
  force-reflection gain.
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
master_reflection_gain_xyz =
  lerp(reflection_gain_min, reflection_gain_max, reflection_alpha)
  * residual_scale
```

`joint_stiffness_projection` is a 6x3 matrix and `reflection_projection` is a
3x3 matrix from the task profile. Rows are clamped after projection, so profiles
can express task-specific coupling while the runtime keeps every joint gain or
reflection axis gain within the configured bounds. V1 does not modulate slave
feedforward torque from `K_tele`; model compensation remains owned by the MuJoCo
compensation path.

The SVS controller emits master-side stabilizer fields deterministically:

```text
master_position = snapshot.master.q
master_velocity = [0; 6]
master_kp = profile.control.master_kp
master_kd = profile.control.master_kd
```

After the controller emits a command, the generic dual-arm loop applies
profile-owned output shaping in this order:

1. First-order LPF on `master_interaction_torque` using
   `master_interaction_lpf_cutoff_hz`.
2. Per-axis slew limit using
   `master_interaction_slew_limit_nm_per_s * dt_sec`. Existing dual-arm output
   shaping currently treats `BilateralLoopConfig::master_interaction_slew_limit`
   as a per-tick delta. The SVS work must replace that generic SDK field with an
   explicitly named per-second field, for example
   `master_interaction_slew_limit_nm_per_s`, before this collector is
   implemented. Existing construction sites, CLI config/docs, and tests must be
   migrated so old per-tick values are not silently reinterpreted. The SDK
   default should preserve the current nominal 200 Hz behavior by changing from
   `0.25` Nm/tick to `50.0` Nm/s.
3. Clamp to `master_interaction_limit_nm`.
4. Optional master passivity damping, clamped by
   `master_passivity_max_damping`.
5. Clamp slave feedforward torque to `slave_feedforward_limit_nm`.
6. Add MuJoCo model torque compensation outside the interaction/feedforward
   limits.

The shaped master interaction torque before model-torque addition is recorded as
`command.shaped_master_interaction_nm`. The shaped slave feedforward torque
before model-torque addition is recorded as `command.shaped_slave_feedforward_nm`.
The final values passed as MIT `t_ref` inputs to the SDK before device-specific
quirk flips/scaling are recorded as `command.sdk_master_feedforward_nm` and
`command.sdk_slave_feedforward_nm`. The post-quirk values actually supplied to
`MitControlCommand::try_new` are recorded as `command.mit_master_t_ref_nm` and
`command.mit_slave_t_ref_nm`; these are the values checked against the protocol
range `[-8, 8]`.

Each stiffness axis must satisfy `K_min[i] < K_max[i]`. Equal bounds are
rejected rather than treated as a constant axis because the normalized control
mapping above depends on a non-zero denominator.

The reflected master interaction torque follows the paper's force-reflection
form and renders the filtered slave EE residual through the master translational
Jacobian:

```text
f_reflect_slave_ee =
  diag(master_reflection_gain_xyz) * r_ee
f_reflect_slave_base =
  R_slave_base_from_ee * f_reflect_slave_ee
f_reflect_master_base =
  R_slave_base_from_master_base^T * f_reflect_slave_base
master_interaction_torque =
  -Jp_master^T * f_reflect_master_base
```

The negative sign renders resistance against continued operator motion. The
collector records the current tick's shaped and passivity-adjusted
`master_interaction_torque` as `command.shaped_master_interaction_nm`. A later
tick may select that command from the TX-finished command history as
`tau_master_feedback_subtracted_nm` when its timestamp is not newer than the
master dynamic feedback timestamp. The existing
`JointSpaceBilateralController` may remain for the general CLI, but the SVS
collector uses this task-space reflection law.

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
  effective_profile.toml
  calibration.toml
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
fails before MIT enable. If the arms are already connected in `DualArmStandby`,
the collector disables or disconnects them through the normal non-active
shutdown path. No `manifest.toml` is written if no episode directory was
created. The directory is reserved with an atomic create-directory operation
after calibration resolution; v1 does not create a pre-connect reservation
directory.

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
- start and end wall-clock timestamps in Unix nanoseconds
- episode start host monotonic timestamp in microseconds from
  `piper_driver::heartbeat::monotonic_micros`
- final status: `running`, `complete`, `cancelled`, or `faulted`
- master/slave targets
- MuJoCo model source and content hash
- MuJoCo runtime version/build string and native library identity
- master/slave MuJoCo end-effector site names
- per-arm MuJoCo dynamics mode, payload, qacc filter, and qacc clamp
- task profile source, source path when file-backed, and source content hash
  when file-backed
- effective profile path and content hash
- gripper mirror configuration
- calibration path, calibration hash, calibration zero poses, and effective
  `JointMirrorMap`
- mirror-map source kind, source path when file-backed, and source content hash
  when file-backed
- collector binary version or git revision when available
- whether raw CAN side recording was enabled
- encoded `step_count` and optional `last_step_index`

Hashes use SHA-256. Task profile hash is the SHA-256 of the exact profile file
bytes loaded for the episode when a file-backed task profile is used. If no
task profile file is supplied, the profile source is recorded as
`built-in-defaults` and the source content hash is absent.

The effective profile is the fully resolved configuration after applying
built-in defaults, the optional task profile file, and CLI overrides such as
`--disable-gripper-mirror`. The collector writes it to
`effective_profile.toml`; its hash is the SHA-256 of the exact bytes written.
This effective profile hash, not the optional source profile hash, is the
primary reproducibility identifier for stiffness, cue, dynamics, control,
gripper, and writer parameters.

`effective_profile.toml` serialization must be deterministic. The canonical
table order is exactly the order shown in the profile shape:
`[stiffness]`, `[contact]`, `[frames]`, `[calibration]`,
`[calibration.mirror_map]`, `[mujoco]`, `[cue]`, `[cue.master_phi]`,
`[cue.slave_phi]`, `[dynamics]`, `[dynamics.master_payload]`,
`[dynamics.slave_payload]`, `[control]`, `[gripper]`, `[writer]`. Fields within
each table are written in the order shown, arrays keep fixed element order, and
floating-point values use the shortest decimal representation that round-trips
to the same IEEE-754 `f64` using the Rust `ryu` algorithm. The canonical writer
must be explicit rather than delegating layout to a general TOML serializer. It
writes UTF-8, LF line endings, no trailing spaces, one blank line between
tables, and exactly one trailing newline. Scalar lines use `key = value`.
Booleans are lowercase `true`/`false`. Integers are base-10 with no plus sign or
leading zero except for `0`. Strings use TOML basic strings with deterministic
escaping of `\\`, `"`, and control characters. One-dimensional arrays are inline
with comma-space separators. Matrix fields are written in the multi-line row
layout shown in the profile example, with two-space indentation and a trailing
comma after each row. The canonical effective profile contains no comments.
Tests must compare exact bytes for the same effective profile.

`effective_profile.toml` always serializes the resolved mirror map inline in the
same canonical shape as a captured calibration. A custom mirror map therefore
looks like:

```toml
[calibration]
mirror_map_kind = "custom"
calibration_max_error_rad = 0.05

[calibration.mirror_map]
permutation = [0, 1, 2, 3, 4, 5]
position_sign = [-1.0, 1.0, 1.0, -1.0, 1.0, -1.0]
velocity_sign = [-1.0, 1.0, 1.0, -1.0, 1.0, -1.0]
torque_sign = [-1.0, 1.0, 1.0, -1.0, 1.0, -1.0]
```

The source mirror-map path and hash are manifest metadata only and are absent
when the source is not file-backed. The effective profile hash is computed from
the inline resolved mirror map.

MuJoCo model hash is:

- for `--model-dir` or `--use-standard-model-path`: SHA-256 over this canonical
  byte stream:
  `b"PIPER_MUJOCO_MODEL_HASH_V1\n"`, followed by every regular file under the
  resolved model directory sorted by UTF-8 relative path. Paths use `/` as the
  separator, must be relative, and must not contain `.` or `..` components.
  Non-UTF-8 paths, symlinks, device files, and duplicate normalized paths are
  rejected before MIT enable. Each file entry is:
  `b"F"`, `relative_path_len` as little-endian `u64`, raw UTF-8 relative path
  bytes, `content_len` as little-endian `u64`, and raw file content bytes.
  The manifest records the selected root XML relative path. All MuJoCo includes,
  meshes, textures, plugins, and other files that can affect parsing or dynamics
  must resolve to regular files inside this hashed tree; resolution outside the
  tree or through symlinks is a startup error.
- for `--use-embedded-model`: SHA-256 over
  `b"PIPER_MUJOCO_EMBEDDED_MODEL_HASH_V1\n"` followed by the embedded model XML
  bytes. The v1 embedded model must be a single self-contained XML document with
  no external includes, meshes, textures, plugins, or other asset resolution.
  If embedded assets are needed later, a new schema must define a canonical
  embedded asset byte stream before those assets affect model torques.

The manifest records the hash algorithm, resolved model source, resolved
profile source, and resolved effective profile path.

The manifest also records the MuJoCo runtime identity used to compute model
torques: the MuJoCo version/build string reported by the native library, the
Rust binding crate version when available, and either the SHA-256 of the loaded
native shared library bytes or an explicit statically linked native build
identity. If the collector cannot record a native library hash or statically
linked build identity, it fails before MIT enable rather than producing
non-reproducible residual labels.

The host monotonic timestamp is only comparable with logs produced in the same
collector process or by code using the same `piper_driver::heartbeat` anchor.
Future camera capture added to this collector must record the same host
monotonic clock so robot-camera alignment does not depend on wall-clock time.

Structured output writes never overwrite files that existed before this process
reserved the newly created episode directory. Within that newly created episode
directory, `steps.bin`, `report.json`, `effective_profile.toml`, and
`calibration.toml` are written to temporary files, flushed, fsynced when
supported, and atomically renamed into place. The only intentional replacement
is the final `manifest.toml` rewrite: it uses the same temporary-file and
atomic-rename protocol to replace the initial `running` manifest created by this
same collector process. A failure in this protocol is a structured output
finalization failure.

### `steps.bin`

`steps.bin` is a strict bincode 1.3 stream using fixed-int encoding and little
endian byte order, matching the repository's existing strict recording v3
style. Historical incompatible formats are rejected unless a new schema version
is declared.

The V1 wire schema is normative. Writers and readers must encode exactly these
Serde structs with bincode 1.3 fixed-int little-endian encoding. Field order is
the declaration order below. Arrays are fixed length. Enums are stored as `u8`
codes defined here, not as Rust enum variants. All floating-point values are
`f64` and must be finite. There is no padding, compression, checksum,
per-record length prefix, or delimiter between repeated `SvsStepV1` values.

```rust
pub struct SvsHeaderV1 {
    pub magic: [u8; 8],              // b"PIPERSVS"
    pub schema_version: u16,         // 1
    pub encoding_id: u16,            // 1 = bincode-1.3-fixint-little-endian
    pub step_schema_version: u16,    // 1 = SvsStepV1
    pub reserved: u16,               // 0
    pub episode_id_len: u16,         // bytes used in episode_id_utf8
    pub episode_id_utf8: [u8; 128],  // zero-padded UTF-8
    pub created_unix_ms: u64,
    pub episode_start_host_mono_us: u64,
}

pub struct SvsArmStepV1 {
    pub position_hw_timestamp_us: u64,
    pub dynamic_hw_timestamp_us: u64,
    pub position_host_rx_mono_us: u64,
    pub dynamic_host_rx_mono_us: u64,
    pub feedback_age_us: u64,
    pub state_skew_us: i64,
    pub q_rad: [f64; 6],
    pub dq_rad_s: [f64; 6],
    pub tau_measured_nm: [f64; 6],
    pub tau_model_mujoco_nm: [f64; 6],
    pub tau_residual_nm: [f64; 6],
    pub ee_position_base_m: [f64; 3],
    pub rotation_base_from_ee_row_major: [f64; 9],
    pub translational_jacobian_base_row_major: [f64; 18],
    pub jacobian_condition: f64,
}

pub struct SvsCommandStepV1 {
    pub master_tx_finished_host_mono_us: u64,
    pub slave_tx_finished_host_mono_us: u64,
    pub controller_slave_position_rad: [f64; 6],
    pub controller_slave_velocity_rad_s: [f64; 6],
    pub controller_slave_kp: [f64; 6],
    pub controller_slave_kd: [f64; 6],
    pub controller_slave_feedforward_nm: [f64; 6],
    pub controller_master_position_rad: [f64; 6],
    pub controller_master_velocity_rad_s: [f64; 6],
    pub controller_master_kp: [f64; 6],
    pub controller_master_kd: [f64; 6],
    pub controller_master_interaction_nm: [f64; 6],
    pub shaped_master_interaction_nm: [f64; 6],
    pub shaped_slave_feedforward_nm: [f64; 6],
    pub sdk_master_feedforward_nm: [f64; 6],
    pub sdk_slave_feedforward_nm: [f64; 6],
    pub mit_master_t_ref_nm: [f64; 6],
    pub mit_slave_t_ref_nm: [f64; 6],
}

pub struct SvsGripperStepV1 {
    pub mirror_enabled: u8,           // 0 = disabled, 1 = enabled
    pub master_available: u8,         // 0 = unavailable, 1 = available
    pub slave_available: u8,          // 0 = unavailable, 1 = available
    pub command_status: u8,           // 0 = none, 1 = sent, 2 = skipped, 3 = failed
    pub master_hw_timestamp_us: u64,  // 0 if unavailable
    pub slave_hw_timestamp_us: u64,   // 0 if unavailable
    pub master_host_rx_mono_us: u64,  // 0 if unavailable
    pub slave_host_rx_mono_us: u64,   // 0 if unavailable
    pub master_age_us: u64,           // age at SvsStepV1.host_mono_us
    pub slave_age_us: u64,            // age at SvsStepV1.host_mono_us
    pub master_position: f64,         // normalized [0, 1], 0 if unavailable
    pub master_effort: f64,           // normalized [0, 1], 0 if unavailable
    pub slave_position: f64,          // normalized [0, 1], 0 if unavailable
    pub slave_effort: f64,            // normalized [0, 1], 0 if unavailable
    pub command_position: f64,        // normalized [0, 1], 0 if no command
    pub command_effort: f64,          // normalized [0, 1], 0 if no command
}

pub struct SvsStepV1 {
    pub step_index: u64,
    pub host_mono_us: u64,
    pub episode_elapsed_us: u64,
    pub dt_us: u64,
    pub inter_arm_skew_us: u64,
    pub deadline_missed: u8,          // 0 = no, 1 = yes
    pub contact_state: u8,            // 0 = free, 1 = contact
    pub raw_can_status: u8,           // 0 = disabled, 1 = ok, 2 = degraded
    pub master: SvsArmStepV1,
    pub slave: SvsArmStepV1,
    pub tau_master_effort_residual_nm: [f64; 6],
    pub tau_master_feedback_subtracted_nm: [f64; 6],
    pub u_ee_raw: [f64; 3],
    pub r_ee_raw: [f64; 3],
    pub u_ee: [f64; 3],
    pub r_ee: [f64; 3],
    pub k_state_raw_n_per_m: [f64; 3],
    pub k_state_clipped_n_per_m: [f64; 3],
    pub k_tele_n_per_m: [f64; 3],
    pub reflection_gain_xyz: [f64; 3],
    pub reflection_residual_scale: f64,
    pub command: SvsCommandStepV1,
    pub gripper: SvsGripperStepV1,
    pub writer_queue_depth: u32,
    pub writer_queue_full_events: u64,
    pub dropped_step_count: u64,
}
```

Gripper availability is true only when the corresponding gripper feedback has a
non-zero host receive timestamp and its age at `SvsStepV1.host_mono_us` is less
than or equal to `gripper.max_feedback_age_ms * 1000` microseconds from the
effective profile.
If the gripper host receive timestamp is greater than `SvsStepV1.host_mono_us`,
the gripper sample is newer than the control-frame timestamp and must be encoded
as unavailable for that tick rather than saturating or producing a negative age.
Unavailable, stale, and future-dated gripper timestamp fields, age fields, and
value fields are encoded as zero. The public client gripper state must expose
hardware and host receive timestamps through a generic MuJoCo-free API before
the collector can claim time-aligned gripper telemetry.

`episode_id_len` must be non-zero and no greater than 128. Bytes after
`episode_id_len` in `episode_id_utf8` must be zero. `task-slug` length is capped
so the full episode ID fits this field. `host_mono_us`,
`episode_start_host_mono_us`, all host receive timestamps, and future in-process
camera timestamps use `piper_driver::heartbeat::monotonic_micros`. Hardware
timestamps are the per-backend timestamps already exposed in `ControlSnapshot`.
For `SvsArmStepV1`, `tau_residual_nm` always means
`tau_measured_nm - tau_model_mujoco_nm`. For the master arm this is the raw
model residual before subtracting rendered feedback; the operator-effort cue is
stored separately as `tau_master_effort_residual_nm`.

`feedback_age_us` is computed per arm as
`host_mono_us - max(position_host_rx_mono_us, dynamic_host_rx_mono_us)` and is a
hard failure if either feedback timestamp is zero or newer than `host_mono_us`.
`state_skew_us` is signed and equals
`dynamic_host_rx_mono_us - position_host_rx_mono_us` for that arm.
`inter_arm_skew_us` is unsigned and equals
`abs(master.dynamic_host_rx_mono_us - slave.dynamic_host_rx_mono_us)`.
`deadline_missed` is the deterministic flag defined by the tick submission
deadline rule above. `raw_can_status` is `0` when `--raw-can` is disabled, `1`
when both raw side writers are healthy at the moment this step is encoded, and
`2` after either raw side writer reports degradation during active control.
Post-loop raw finalization failures are reported only in `report.json` and
`manifest.toml`; they do not retroactively change already encoded
`raw_can_status` values.

After the header, the file stores repeated fixed-schema `SvsStepV1` records
until EOF. The record count is not trusted from the header; readers count records
while decoding and compare against `report.json`/`manifest.toml` metadata.
The final manifest and `report.json` both record `step_count`. If `step_count >
0`, both also record `last_step_index`, which must equal `step_count - 1`.
`last_step_index` is omitted from the final manifest and `null` in `report.json`
when no step was written.

Readers must reject `steps.bin` unless EOF occurs exactly at a `SvsStepV1`
record boundary. They must reject any partial trailing record, trailing garbage,
or decoded record whose `step_index` does not equal its zero-based ordinal.
`step_count` must equal the number of decoded records, and `last_step_index`
must match the final decoded record when present.

The step format does not include camera frames in v1. It provides enough timing
metadata for future camera logs to align by host monotonic time.

### `report.json`

Contains:

- dual-arm `BilateralRunReport` fields
- writer statistics
- dropped step count
- encoded step count and last step index
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
- `cancelled`: operator-requested stop or Ctrl+C before MIT enable or during
  active control, after a clean bounded shutdown and successful writer flush,
  with no writer queue-full event and no dropped step
- `faulted`: startup failure after manifest creation, MuJoCo/controller/runtime
  failure, any writer queue-full event, any dropped step, writer backpressure
  threshold exceeded, writer flush failure, structured output finalization
  failure, or any dual-arm fault report

If validation fails before the episode directory is created, no episode status
is written and the command exits with an error.

V1 has no partial-success final status. A run with any missing `SvsStepV1`
record is `faulted`, even if the missing count is below the threshold that
triggers early shutdown. Downstream training filters may inspect faulted
episodes manually, but the collector must not label them complete.

Optional raw CAN side recording is diagnostic only for training data quality:
failure to start raw CAN recording before MIT enable is a startup error, but a
raw CAN writer or raw CAN finalization failure after MIT enable does not by
itself change `complete` or `cancelled` to `faulted`. Such failures are reported
in `report.json` and `manifest.toml`; `steps.bin` remains the authoritative
dataset stream. `structured output finalization failure` means failure to write,
flush, fsync, or atomically finalize `steps.bin`, `manifest.toml`,
`report.json`, `effective_profile.toml`, or `calibration.toml`; it excludes
optional raw CAN side files after active control has started.

## Safety and Fault Handling

The collector is fail-closed. It must not silently degrade to motion-only or
raw-current-only behavior.

Hard failures:

- malformed config or task profile
- unsupported target type
- duplicate master/slave target
- missing or invalid MuJoCo model
- MuJoCo calculation failure
- non-finite model torque, residual, Jacobian, cue, `K_state_raw`, command
  field, or `K_tele`
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

All numeric profile values must be finite. "Approximately orthonormal" means
`max_abs(R^T R - I) <= 1e-6` and `abs(det(R) - 1.0) <= 1e-6`.

V1 conservative hard limits are:

- stiffness bounds and bases: `0 <= K <= 5000` N/m
- `track_kp`, `master_kp`: `0 <= kp <= 500`
- `track_kd`, `master_kd`: `0 <= kd <= 5`
- `reflection_gain`: `0 <= gain <= 2`
- `master_interaction_limit_nm`: `0 <= limit <= 8`
- `master_interaction_slew_limit_nm_per_s`: `0 <= slew <= 200`
- `master_passivity_max_damping`: `0 <= damping <= 10`
- `slave_feedforward_limit_nm`: `0 <= limit <= 8`
- `max_abs_qacc`: `0 < clamp <= 200` rad/s^2 when `full` dynamics is used
- final SDK MIT `t_ref` after model compensation and device quirk flips/scaling:
  `-8 <= t_ref <= 8` Nm on every joint

Validation rejects:

- model source flags that are not mutually exclusive
- non-positive frequencies or filter cutoffs
- `loop_frequency_hz` not exactly `200.0`
- `warmup_cycles` outside `0..=100`
- non-positive `dt_clamp_multiplier`
- non-positive `calibration_max_error_rad`
- non-canonical calibration TOML bytes
- non-positive DLS lambda
- non-positive Jacobian condition threshold
- `K_min >= K_max` on any axis
- base stiffness outside `[K_min, K_max]`
- negative rate limits
- contact enter threshold less than or equal to exit threshold
- negative contact thresholds
- negative min-hold duration
- cue matrices with the wrong shape
- unsupported phi modes
- phi vectors with shapes other than three elements
- negative phi deadband or scale values
- non-positive phi limit values
- unsupported dynamics modes
- gripper update divider less than 1, negative gripper deadband, or negative
  gripper effort scale
- non-positive gripper max feedback age
- negative payload mass or non-finite payload COM values
- `full` dynamics mode with missing qacc filter, non-positive qacc filter, or
  non-positive acceleration clamp
- missing, empty, or unresolved MuJoCo end-effector site names
- invalid `JointMirrorMap` permutation or sign values
- base-frame rotation matrices that are not finite, approximately orthonormal,
  and approximately determinant `+1`
- joint stiffness projection matrices with shapes other than 6x3
- reflection projection matrices with shapes other than 3x3
- negative tracking gains or `track_kp_min > track_kp_max` on any joint
- negative reflection gain bounds or `reflection_gain_min > reflection_gain_max`
  on any axis
- negative reflection residual deadband or attenuation
- `reflection_residual_min_scale` outside `[0, 1]`
- non-positive writer queue capacity, queue-full thresholds, or flush timeout
- task slugs that are empty or too long to fit the 128-byte episode ID field
- control gains and output-shaping parameters outside the v1 hard limits above
- any generated final SDK MIT `t_ref` outside `[-8, 8]` after adding model
  compensation and applying device quirk flips/scaling

The `--yes` flag may skip confirmation prompts only. It must not skip target,
profile, calibration, model, output path, or safety validation.

## Testing

Unit tests without hardware:

- DLS cue mapping returns finite expected vectors for well-conditioned Jacobians.
- DLS cue mapping uses `lambda^2 * I`, not `lambda * I`.
- DLS rejects non-finite values and conditioning beyond the configured limit.
- Master cue residual subtracts time-aligned rendered feedback torque.
- Master force reflection uses `-Jp_master^T f_reflect_master_base`.
- Cue LPFs determine the filtered `u_ee` and `r_ee` used by contact gating and
  stiffness updates.
- LPF alpha, stiffness rate-limit deltas, and contact hysteresis tick counters
  match the normative discrete update equations.
- `SvsStiffnessState` clips before LPF/rate limiting, applies final clipping,
  and rejects non-finite values.
- Task profile validation rejects invalid ranges and matrix shapes.
- Effective profile serialization and hashing are deterministic with and
  without a source task profile file.
- Effective profile table order matches the exact canonical order, including
  mandatory inline `[calibration.mirror_map]`.
- Effective profile custom mirror-map serialization is valid TOML and has no
  scalar/table key conflict.
- Model directory hashing rejects ambiguous paths and produces byte-identical
  hashes for the same file tree.
- Manifest/report step counts and last step indexes match decoded `steps.bin`;
  partial trailing records, trailing garbage, and non-sequential `step_index`
  values are rejected.
- Calibration canonical TOML bytes and hashes are deterministic; calibration
  loading rejects mirror-map mismatch and out-of-tolerance current posture;
  calibration save uses no-overwrite atomic semantics; every episode persists
  the canonical `calibration.toml` matching the manifest hash.
- `SvsEpisode v1` header and step round-trip.
- `SvsEpisode v1` rejects mismatched field order, enum codes, non-finite
  floating-point values, and episode IDs that do not fit the fixed header.
- Writer queue backpressure marks degradation and trips the stop threshold.
- Controller command scheduling keeps gains and reflection within configured
  bounds.
- Output shaping treats master interaction slew as Nm/s and multiplies by
  `dt_sec`; the SDK config field is renamed/migrated so per-tick values are not
  silently reinterpreted, and default dual-arm CLI behavior remains equivalent
  at 200 Hz.
- Submission mode tests prove the generic dual-arm SDK defaults to unconfirmed
  steady-state MIT sends, `piper-cli teleop dual-arm` keeps that default, and the
  SVS collector explicitly selects confirmed TX-finished submission.
- Final SDK MIT `t_ref` validation rejects values outside `[-8, 8]` after model
  compensation and device quirks.
- Driver confirmed MIT batch tests prove the returned TX-finished timestamp is
  sampled after the last frame send succeeds, not at the commit point, and that
  timeout or partial delivery returns no finished timestamp.
- Client dual-arm telemetry tests prove the sink receives TX-finished
  timestamps, post-quirk `t_ref` values, and no successful telemetry row when
  either arm lacks a finished timestamp.
- Gripper telemetry tests prove public client state exposes hardware/host
  timestamps, availability respects `max_feedback_age_ms`, and stale gripper
  feedback is encoded as unavailable.
- Generic dual-arm loop telemetry exposes shaped command, rendered feedback
  torque, and gripper command without blocking the loop.

Integration tests without hardware:

- Fake dual-arm snapshots plus fake MuJoCo provider run a full collector
  workflow and produce a valid episode directory.
- Target validation rejects GS-USB, auto/default selectors, duplicate SocketCAN
  interfaces, and non-Linux SocketCAN before hardware connect.
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
cargo tree --workspace --all-features > /tmp/piper-default-workspace.tree
! rg -i 'mujoco|piper-physics|piper-svs-collect' /tmp/piper-default-workspace.tree
cargo tree -p piper-cli --all-features > /tmp/piper-cli.tree
! rg -i 'mujoco|piper-physics|piper-svs-collect' /tmp/piper-cli.tree
cargo tree -p piper-control --all-features > /tmp/piper-control.tree
! rg -i 'mujoco|piper-physics|piper-svs-collect' /tmp/piper-control.tree
```

Collector-specific commands are run explicitly:

```bash
cargo fmt --manifest-path addons/piper-physics-mujoco/Cargo.toml -- --check
cargo test --manifest-path addons/piper-physics-mujoco/Cargo.toml --all-targets
cargo clippy --manifest-path addons/piper-physics-mujoco/Cargo.toml --all-targets --all-features -- -D warnings
cargo fmt --manifest-path addons/piper-svs-collect/Cargo.toml -- --check
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
