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
  --interface can0 \
  --joint-map identity \
  --load-profile normal-gripper-d405 \
  --out artifacts/gravity/slave-d405.path.jsonl
```

The path file is not used directly for fitting. It is a safety and coverage
input for `replay-sample`.

Each JSONL row should include:

- host monotonic timestamp
- optional hardware/raw timestamp where available
- `q_rad[6]`
- `dq_rad_s[6]`
- `tau_nm[6]`
- dynamic/position valid masks
- role, target, joint map, and load profile metadata in a header row

### replay-sample

`replay-sample` replays the operator path slowly and samples only stable
quasi-static windows. This is the preferred source of fitting data.

Example:

```bash
piper-cli gravity replay-sample \
  --role slave \
  --interface can0 \
  --path artifacts/gravity/slave-d405.path.jsonl \
  --out artifacts/gravity/slave-d405.samples.jsonl \
  --max-velocity-rad-s 0.08 \
  --settle-ms 500 \
  --sample-ms 300
```

Replay behavior:

1. Load the recorded path.
2. Smooth and simplify the path into waypoints while preserving the operator's
   safe envelope.
3. Move through waypoints using bounded joint velocity, bounded joint step, and
   conservative acceleration behavior.
4. At each accepted waypoint, wait for stable conditions:
   - joint velocity below threshold
   - tracking error below threshold
   - torque variance below threshold
   - feedback masks complete
5. Record multiple samples from the stable window.
6. Stop on operator interrupt, feedback staleness, excessive torque, excessive
   tracking error, or path/limit violation.

The first implementation should prefer low-speed joint-space moves over any
automatic Cartesian planner. The key safety property is that replay follows an
operator-proven path rather than inventing new large motions.

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

The first basis should be compact and explainable:

```text
1
sin(q_i), cos(q_i)
sin(q_i + q_j), cos(q_i + q_j) for selected coupled joints
```

The fitted model stores one linear coefficient vector per output joint. This is
data-driven but not unconstrained: the feature basis encodes the physical prior
that gravity-like torques vary smoothly and periodically with joint angles.

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
created_at_unix_ms = 1770000000000
sample_count = 120000
frequency_hz = 100.0
```

Required safety/quality sections:

```toml
[training_range]
q_min_rad = [...]
q_max_rad = [...]
dq_abs_p95_rad_s = [...]
tau_min_nm = [...]
tau_max_nm = [...]

[fit_quality]
rms_residual_nm = [...]
p95_residual_nm = [...]
max_residual_nm = [...]
holdout_rms_residual_nm = [...]
holdout_p95_residual_nm = [...]
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
- optionally add small local perturbations around safe waypoints in a later
  version
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
```

`model.rs` should contain the serializable model and pure `eval(q)` logic.
`compensation.rs` should adapt loaded models into `BilateralDynamicsCompensator`
for teleop.

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
- A hardware runbook validates:
  - raw torque decreases after reflection compensation in free-space replay
  - assist ratio 0.2 is stable before higher ratios are tried
  - combined mode is only tested after separate master/slave validation

## Open Questions

- Whether replay-sample should use existing blocking joint-position moves first
  or a dedicated low-speed MIT joint-space stepper.
- Whether local waypoint perturbation belongs in the first implementation or a
  follow-up.
- What default sample stability thresholds should be used on the current
  hardware.

