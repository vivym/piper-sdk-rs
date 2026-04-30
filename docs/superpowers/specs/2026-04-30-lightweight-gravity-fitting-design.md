# Lightweight Gravity Fitting Design

Date: 2026-04-30

## Context

Raw-clock bilateral teleoperation is now stable enough to expose force
reflection on the lab hardware. The current bilateral controller reflects the
follower measured joint torque directly:

```text
master_reflected_tau = -reflection_gain * slave_measured_tau
```

This works as a smoke test, but the measured torque is not pure environment
contact force. It includes gravity, payload, friction, control effort, and
motion effects. With the follower carrying a normal gripper and a D405 camera,
vertical end-effector motion can make gravity dominate the reflected torque.

The repository already has a heavy MuJoCo-based SVS teleop stack. That stack is
valuable for research collection, but it is too heavy for the immediate goal:
make the general `piper-cli teleop dual-arm` bilateral mode feel more
transparent without introducing MuJoCo, MJCF, FFI, or addon dependencies into the
normal CLI path.

The existing SDK boundary is already suitable for a lighter compensator:

- `BilateralDynamicsCompensation` can carry `master_model_torque`,
  `slave_model_torque`, and `slave_external_torque_est`.
- The bilateral controller already prefers `slave_external_torque_est` when a
  compensator is present.
- Final torque assembly already adds model torques to master/slave commands.

This design adds a CLI-owned empirical joint-space quasi-static torque model and
data workflow that can feed that existing compensation interface.

## Goal

Add a general `piper-cli gravity` workflow for collecting, fitting, evaluating,
and using empirical quasi-static torque models for master and follower arms.

The first implementation should support:

- model collection for either role: `master` or `slave`
- separate load profiles, e.g. `master-teach-gripper` and
  `slave-normal-gripper-d405`
- manual safe-path recording
- slow replay sampling over recorded safe paths
- offline fitting to a small, deterministic, explainable trigonometric model
- model evaluation and diagnostics
- teleop runtime integration for:
  - follower reflection-only compensation
  - master gravity assist
  - follower gravity assist
  - combined use after separate validation

## Non-Goals

- Do not replace the MuJoCo SVS compensator.
- Do not implement full rigid-body dynamics, inverse dynamics, Coriolis, or
  inertia compensation.
- Do not use a neural network or opaque black-box model in the first version.
- Do not automatically explore the full robot workspace.
- Do not rely on collision geometry, URDF, or MJCF in the first version.
- Do not enable gravity assist by default.
- Do not silently extrapolate outside the training envelope.

## Terminology

The model should be called a "joint-space quasi-static torque model", not a full
gravity model. It estimates the slowly varying torque bias associated with pose,
payload, and hardware configuration:

```text
tau_quasi_static_hat = f(q)
```

For slow teleop this is expected to remove much of the gravity and static bias
from measured torque. It is not expected to explain high-speed inertial effects.

## CLI Workflow

The workflow has four commands:

```bash
piper-cli gravity record-path ...
piper-cli gravity replay-sample ...
piper-cli gravity fit ...
piper-cli gravity eval ...
```

Gravity commands should reuse the same target grammar as the rest of
`piper-cli`: `--target socketcan:can0`, `--target gs-usb-serial:...`, and the
configured default target. A short `--interface can0` convenience may be added
for lab ergonomics, but persisted metadata should store the resolved canonical
target string.

All collected and fitted values must use the same physical sign convention and
units as `DualArmSnapshot` and `BilateralDynamicsCompensation`:

- joint positions are radians in SDK joint order
- joint velocities are rad/s in SDK joint order
- measured torque is Nm after firmware quirk scaling/sign normalization
- model torque is emitted in the local arm's joint frame
- `slave_external_torque_est` is computed in the slave frame before the
  bilateral calibration maps it to the master frame

The teleop command then consumes fitted models:

```bash
piper-cli teleop dual-arm \
  ... \
  --slave-gravity-model artifacts/gravity/slave-d405.model.toml \
  --gravity-reflection-compensation
```

### record-path

`record-path` is a passive operator-guided path recorder. It must not drive the
arm. The operator manually moves the arm through known-safe poses, and the CLI
records the safe path envelope.

Example:

```bash
piper-cli gravity record-path \
  --role slave \
  --target socketcan:can0 \
  --joint-map identity \
  --load-profile normal-gripper-d405 \
  --out artifacts/gravity/slave-d405.path.jsonl
```

The path file is not used directly for fitting. It is a safety and coverage
input for `replay-sample`.

Torque in the path file is diagnostic only. During manual path recording the arm
may be disabled or otherwise manually guided, so torque samples may be zero,
invalid, or contaminated by operator handling. `fit` must reject path files and
accept only `replay-sample` output.

The JSONL file should use explicit row types. The first row is a header:

- `type = "header"`
- `artifact_kind = "path"`
- schema version
- role
- resolved target
- joint map
- load profile
- torque convention
- operator notes
- SDK/git version where available

Subsequent rows are samples:

- `type = "path-sample"`
- host monotonic timestamp
- optional hardware/raw timestamp where available
- `q_rad[6]`
- `dq_rad_s[6]`
- `tau_nm[6]`
- dynamic/position valid masks
- optional operator segment label

### replay-sample

`replay-sample` replays the operator path slowly and samples only stable
quasi-static windows. This is the preferred source of fitting data.

Example:

```bash
piper-cli gravity replay-sample \
  --role slave \
  --target socketcan:can0 \
  --path artifacts/gravity/slave-d405.path.jsonl \
  --out artifacts/gravity/slave-d405.samples.jsonl \
  --max-velocity-rad-s 0.08 \
  --settle-ms 500 \
  --sample-ms 300 \
  --bidirectional
```

Replay behavior:

1. Load the recorded path.
   - the path header role, joint map, and load profile must match CLI args
   - the resolved target should match unless the operator passes an explicit
     unsafe `--allow-target-mismatch` override
2. Smooth and simplify the path into waypoints while preserving the operator's
   safe envelope.
3. Move through waypoints using a dedicated low-speed MIT joint-space stepper.
   The first implementation should not use the opaque firmware PositionMode
   trajectory planner for this command because fitting needs explicit velocity,
   step, gain, and hold-window control.
4. At each accepted waypoint, wait for stable conditions:
   - joint velocity below threshold
   - tracking error below threshold
   - torque variance below threshold
   - feedback masks complete
5. Record multiple samples from the stable window.
6. Stop on operator interrupt, feedback staleness, excessive torque, excessive
   tracking error, or path/limit violation.

The default sampling pass should be bidirectional: traverse the simplified path
forward and then backward unless the operator disables that behavior. This helps
expose friction/hysteresis residuals instead of letting the fitter mistake
one-direction friction for gravity.

Path simplification must be conservative. It may remove redundant adjacent
samples, but it must preserve waypoint order, enforce a max joint-space
deviation from the recorded path, and avoid creating long shortcut segments
through unrecorded space. The dry run should report removed waypoint count and
max simplification deviation.

The MIT stepper should command small joint-space increments with zero model
feedforward, conservative `kp/kd`, explicit velocity limits, and a stable hold
at each waypoint. Samples collected while the target is still moving must be
discarded. Only hold-window samples are valid for fitting. The key safety
property is that replay follows an operator-proven path rather than inventing new
large motions.

`replay-sample` must require operator confirmation before enabling the arm. The
confirmation text should include the target, role, load profile, maximum joint
velocity, waypoint count, and output path.

`replay-sample --dry-run` should print waypoint count, min/max joint range, and
estimated duration without enabling the arm. This gives the operator a chance to
reject an unsafe path before active replay.

Initial lab defaults should be conservative and CLI-configurable:

```text
max_velocity_rad_s = 0.08
max_step_rad = 0.02
settle_ms = 500
sample_ms = 300
stable_velocity_rad_s = 0.01
stable_tracking_error_rad = 0.03
stable_torque_std_nm = 0.08
```

The replay output JSONL must have its own header:

- `type = "header"`
- `artifact_kind = "quasi-static-samples"`
- source path file and content hash
- role, resolved target, joint map, and load profile
- torque convention
- replay parameters and stability thresholds
- waypoint count, accepted waypoint count, and rejected waypoint count

Replay sample rows should use `type = "quasi-static-sample"` so `fit` can
reject manual path rows unambiguously. Each accepted sample row should include:

- `waypoint_id`
- `segment_id` or `null`
- `pass_direction = "forward" | "backward"`
- host monotonic timestamp
- optional hardware/raw timestamp where available
- `q_rad[6]`
- `dq_rad_s[6]`
- `tau_nm[6]`
- feedback masks and stability metrics used for acceptance

### fit

`fit` builds a deterministic trigonometric regression model from samples.

Example:

```bash
piper-cli gravity fit \
  --samples artifacts/gravity/slave-d405.samples.jsonl \
  --out artifacts/gravity/slave-d405.model.toml \
  --basis trig-v1 \
  --ridge-lambda 1e-4 \
  --holdout-ratio 0.2
```

`--samples` may be passed more than once to merge multiple safe replay
segments. All input files must have `artifact_kind = "quasi-static-samples"` and
matching role, joint map, load profile, and torque convention. Mismatches must
be rejected unless a future explicit merge/retarget command exists.

The first basis should be compact and explainable:

```text
1
sin(q_i), cos(q_i)
sin(q_i + q_{i+1}), cos(q_i + q_{i+1}) for adjacent joint pairs
```

`trig-v1` should use one shared feature vector for all six output joints:

```text
1 bias feature
12 single-joint sin/cos features
10 adjacent-pair sum sin/cos features
23 total features
```

The fitted model stores one linear coefficient vector per output joint. This is
data-driven but not unconstrained: the feature basis encodes the physical prior
that gravity-like torques vary smoothly and periodically with joint angles.

The holdout split must be path/segment based, not random row based. Adjacent
stable-window samples are strongly correlated; random row holdout would
overstate generalization. When segment labels are present, whole segments should
be assigned to train or holdout. When no segment labels are present, the fitter
should split blocked waypoint groups, never individual rows. The selected split
must be deterministic and stored in the model so `eval` and the Python reference
tool can reproduce it exactly. The fitter should reject models with too few
independent waypoints per coefficient, with a first-version default of at least
10 independent accepted waypoints per feature. It should report the feature
matrix condition number or an equivalent conditioning metric.

The fitter should report forward/backward residual differences when samples were
collected bidirectionally. The first model does not attempt to identify a
friction model; large direction-dependent residuals should be surfaced as a
quality warning and may require slower replay, more settle time, or future
friction modeling.

The production fitter should be implemented in Rust. Use a small linear algebra
dependency such as `nalgebra` and keep the fitting algorithm explicit in the CLI
or tooling crate:

```text
G = X^T X
B = X^T Y
(G + lambda I) C = B
```

The implementation should stream samples into `G` and `B` rather than requiring
all replay rows in memory. The bias term should not be regularized by default.
The primary solve path should use a positive-definite solve after ridge
regularization, with a deterministic fallback and explicit diagnostics when the
matrix is ill-conditioned.

### Python reference validation

The repository must also include a Python reference validation tool managed by
`uv`. This tool is not the production fitting path and must not be required for
normal teleop use. Its purpose is to verify that the Rust solver, feature
generation, holdout split, coefficient layout, and residual metrics match an
independent NumPy implementation.

Suggested layout:

```text
tools/gravity-reference/
  pyproject.toml
  uv.lock
  gravity_fit_reference.py
```

The script should support at least:

```bash
uv run --project tools/gravity-reference \
  python tools/gravity-reference/gravity_fit_reference.py \
  --samples artifacts/gravity/slave-d405.samples.jsonl \
  --rust-model artifacts/gravity/slave-d405.model.toml \
  --out artifacts/gravity/slave-d405.reference-check.json
```

It should independently parse the JSONL samples, build the same `trig-v1`
feature matrix, solve the same ridge regression with NumPy, and compare against
the Rust model. The comparison report should include coefficient max-abs
difference, per-joint residual metric differences, holdout split identity, and a
clear pass/fail result with configured tolerances. CI and local verification
should be able to run this tool with `uv run --project tools/gravity-reference`;
no global Python environment should be assumed.

### eval

`eval` reports fit quality and coverage before a model is used in teleop.

Example:

```bash
piper-cli gravity eval \
  --model artifacts/gravity/slave-d405.model.toml \
  --samples artifacts/gravity/slave-d405.validation.jsonl
```

It should report at least:

- per-joint RMS residual in Nm
- per-joint p95 residual in Nm
- max residual in Nm
- raw torque delta vs compensated external torque delta
- number of samples
- training envelope violations
- nearest-neighbor/coverage distance summary
- per-segment residual summary when segment labels are available

## Model File

The model should be TOML so it can be reviewed and committed with experiments.

Required metadata:

```toml
schema_version = 1
model_kind = "joint-space-quasi-static-torque"
basis = "trig-v1"
role = "slave"
joint_map = "identity"
load_profile = "normal-gripper-d405"
torque_convention = "piper-sdk-normalized-nm-v1"
created_at_unix_ms = 1770000000000
sample_count = 120000
frequency_hz = 100.0
```

Required safety/quality sections:

```toml
[fit]
ridge_lambda = 1e-4
regularize_bias = false
solver = "nalgebra-cholesky"
fallback_solver = "nalgebra-svd"
holdout_strategy = "segment-or-blocked-waypoint"
holdout_ratio = 0.2
train_group_ids = [...]
holdout_group_ids = [...]

[training_range]
q_min_rad = [...]
q_max_rad = [...]
dq_abs_p95_rad_s = [...]
tau_min_nm = [...]
tau_max_nm = [...]
waypoint_count = 640
segment_count = 8

[fit_quality]
rms_residual_nm = [...]
p95_residual_nm = [...]
max_residual_nm = [...]
holdout_rms_residual_nm = [...]
holdout_p95_residual_nm = [...]
condition_number = 123.4
```

Required model section:

```toml
[model]
feature_names = [...]
coefficients_nm = [
  [...], # J1
  [...], # J2
  [...], # J3
  [...], # J4
  [...], # J5
  [...], # J6
]

[coverage]
# Optional decimated training anchors used for runtime distance/confidence
# checks. If omitted, runtime must fall back to range-only gating.
anchor_q_rad = [
  [...],
]
```

## Teleop Integration

The teleop CLI should add explicit model and enable flags:

```bash
--master-gravity-model <path>
--slave-gravity-model <path>
--gravity-reflection-compensation
--master-gravity-assist-ratio <0.0..0.6>
--slave-gravity-assist-ratio <0.0..0.6>
```

The first runtime integration target is the calibrated raw-clock teleop path
used by the current hardware experiments. StrictRealtime teleop may use the same
model and compensator later, but raw-clock support is the acceptance target for
the first implementation.

Runtime compensation should compute:

```text
master_hat = eval(master_model, q_master) if provided else 0
slave_hat  = eval(slave_model, q_slave) if provided else 0

slave_external_torque_est =
  if gravity_reflection_compensation:
      slave_measured_tau - slave_hat
  else:
      slave_measured_tau

master_model_torque = master_hat * master_gravity_assist_ratio
slave_model_torque  = slave_hat  * slave_gravity_assist_ratio
```

This maps directly onto `BilateralDynamicsCompensation`.

`slave_hat` must not be mapped to the master frame inside the compensator. The
compensator emits `slave_external_torque_est` in the slave frame, and the
bilateral controller's existing calibration maps slave-frame external torque to
master-frame reflected torque. `master_hat` is used only for
`master_model_torque` in the master frame.

Compensation uses the current snapshot and affects commands submitted for the
next cycle. This avoids an algebraic loop: any assist torque sent in this cycle
will influence measured torque only in later feedback, where the same model
subtraction remains valid.

The first recommended validation sequence is:

1. Use only `--gravity-reflection-compensation` with the slave model.
2. Validate master assist separately with `--master-gravity-assist-ratio 0.2`.
3. Validate follower assist separately with `--slave-gravity-assist-ratio 0.2`.
4. Combine only after the individual modes are stable.

## Safety Behavior

All assist paths must default to disabled:

```text
gravity_reflection_compensation = false
master_gravity_assist_ratio = 0.0
slave_gravity_assist_ratio = 0.0
```

Startup validation:

- model role must match the selected arm
- model joint map must match `--joint-map`
- model load profile should be printed in startup summary
- model fit quality should be printed in startup summary
- model schema version and basis must be supported
- model torque sign convention must match the current SDK schema version
- operator confirmation should name any enabled assist ratios

Runtime validation:

- If current `q` exceeds training range by more than a small tolerance:
  - reflection compensation may ramp down and warn
  - gravity assist must ramp to zero or fail closed
- Model output must be clamped by per-joint safe limits.
- Assist torque should use a startup ramp, e.g. 2-5 seconds.
- Assist ratio changes should be slew-limited.
- Any model evaluation fault is a compensation fault and should follow existing
  bounded shutdown behavior.
- Runtime confidence should be computed from range and, when present, nearest
  training-anchor distance. Effective reflection compensation may be scaled by
  confidence. Effective gravity assist must be zero when confidence reaches
  zero.

Suggested first-version hard limits:

```text
master_gravity_assist_ratio: 0.0..0.6
slave_gravity_assist_ratio: 0.0..0.6
full 1.0 assist: not supported unless a future explicit experimental flag exists
```

## Coverage and Generalization

The model is intentionally reliable only within the training envelope. To reduce
poor generalization:

- record multiple operator-safe paths, not one path
- cover the actual teleop workspace, not the full robot workspace
- use `replay-sample` to sample stable points along those paths
- store range and fit metrics in the model
- add runtime confidence/range gating

Single-path models should be treated as local models. Multiple paths can be
merged for a broader task profile.

## Reports and Diagnostics

Teleop reports should add gravity model diagnostics when enabled:

- raw measured torque delta
- model torque delta
- external torque estimate delta
- final reflected torque delta
- master/slave assist final torque delta
- range violations
- compensation confidence / active scaling
- compensation fault count
- model path, role, load profile, fit RMS/p95, and assist ratios
- out-of-range sample count and max range violation

The existing torque diagnostics should remain, and new fields should clarify
whether final master torque came from reflection, gravity assist, or both.

## Module Layout

Suggested CLI structure:

```text
apps/cli/src/commands/gravity.rs
apps/cli/src/gravity/
  mod.rs
  collect.rs
  replay_sample.rs
  fit.rs
  model.rs
  eval.rs
  compensation.rs
tools/gravity-reference/
  pyproject.toml
  uv.lock
  gravity_fit_reference.py
```

`model.rs` should contain the serializable model and pure `eval(q)` logic.
`compensation.rs` should adapt loaded models into `BilateralDynamicsCompensator`
for teleop.

`fit.rs` should contain the Rust production regression implementation. The
Python reference tool should remain test/validation support only and should not
be linked into the CLI runtime path.

The implementation should keep MuJoCo and SVS addon crates out of this path.

## Acceptance Criteria

Functional:

- `record-path` creates a path JSONL with role/load/joint-map metadata.
- `replay-sample` creates stable samples from a recorded path.
- `fit` creates a TOML model with coefficients, training range, and fit metrics.
- `eval` reports residual metrics and range violations.
- teleop can use a slave model for reflection-only compensation.
- teleop can use master and slave models for assist ratios, separately or
  together.

Safety:

- Assist is off by default.
- Model role/joint-map mismatches are rejected before enabling arms.
- Out-of-range assist does not continue silently.
- Ctrl-C and runtime faults still produce bounded shutdown reports.

Validation:

- Unit tests cover feature evaluation, TOML round trip, fit on synthetic data,
  range gating, and compensation mapping.
- CLI parser tests cover new commands and teleop flags.
- Replay tests use a fake MIT stepper to verify that samples are recorded only
  after velocity/tracking/variance stability gates pass.
- Fit tests use segment-based holdout and verify random-row leakage is not the
  implemented default.
- A `uv`-managed Python reference validation script independently fits the same
  samples and verifies Rust coefficients and residual metrics within explicit
  tolerances.
- A hardware runbook validates:
  - raw torque decreases after reflection compensation in free-space replay
  - assist ratio 0.2 is stable before higher ratios are tried
  - combined mode is only tested after separate master/slave validation

## Deferred Work

- Local waypoint perturbation around safe waypoints. This is useful for expanding
  coverage from a path into a local tube, but it should wait until the base
  record/replay/fit/eval loop is validated on hardware.
- Explicit friction modeling. The first version should detect and report
  direction-dependent residuals but not compensate them.
