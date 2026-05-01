# Gravity Profile Manager Design

Date: 2026-05-02

## Context

The repository now has a lightweight gravity fitting pipeline:

- `piper-cli gravity record-path`
- `piper-cli gravity replay-sample`
- `piper-cli gravity fit`
- `piper-cli gravity eval`
- `tools/gravity-reference/gravity_fit_reference.py`

Those commands are useful low-level building blocks, but they do not manage a
long-running training workflow. A usable quasi-static torque model needs repeated
collection, fitting, validation, and promotion for a specific physical arm and
load. Today that state lives in filenames and operator memory.

The next layer should make each physical arm/load combination a managed gravity
profile. The profile owns all train and validation artifacts, all model rounds,
assessment reports, and the current best model.

## Goals

Add a high-level profile manager for gravity fitting that can:

- manage one training task per physical arm and load profile
- keep separate train and validation pools
- keep an internal fit holdout in addition to the explicit validation pool
- fit a model from all current train samples
- assess the model against all current validation samples
- mark a model as usable only when a configurable gate passes
- explicitly promote failed validation data into train data
- tell the operator what to do next
- preserve all artifact provenance through hashes, manifests, and round reports

## Non-Goals

Version 1 does not:

- automatically explore new poses
- perform collision checking or workspace planning
- provide a UI dashboard
- use SQLite or another database
- automatically promote validation data
- automatically enable teleop compensation
- merge data across multiple physical arms
- replace the existing low-level gravity commands

## Profile Identity

A gravity profile is bound to one physical arm and one load condition.

The identity fields are:

- `role`
- `arm_id`
- `target` or `interface`
- `joint_map`
- `load_profile`
- `torque_convention`
- `basis`

`arm_id` is required. The manager must not silently use `can0` or `can1` as the
physical identity because CAN interface names can be swapped. The target still
records where the arm is connected for execution.

Default profile name:

```text
<role>-<arm-id>-<load-profile>
```

Example:

```text
slave-piper-sv18-6-can1-normal-gripper-d405
```

## Directory Layout

Profiles live under:

```text
artifacts/gravity/profiles/<profile-name>/
```

Each profile uses this layout:

```text
profile.toml
manifest.json
data/
  train/
    paths/
    samples/
  validation/
    paths/
    samples/
  retired-validation/
models/
  round-0001.model.toml
  round-0002.model.toml
  best.model.toml
reports/
  round-0001.assess.json
  round-0002.assess.json
rounds/
  round-0001.json
  round-0002.json
```

`profile.toml` is the operator-editable configuration. `manifest.json` is the
machine-owned state file. Manifest writes must be atomic: write a temporary file,
fsync when practical, then rename.

## Profile Configuration

`profile.toml` contains stable configuration:

```toml
name = "slave-piper-sv18-6-can1-normal-gripper-d405"
role = "slave"
arm_id = "piper-sv18-6-can1"
target = "socketcan:can1"
joint_map = "identity"
load_profile = "normal-gripper-d405"
torque_convention = "piper-sdk-normalized-nm-v1"
basis = "trig-v1"

[replay]
max_velocity_rad_s = 0.08
max_step_rad = 0.02
settle_ms = 500
sample_ms = 300
bidirectional = true

[fit]
ridge_lambda = 0.0001
holdout_ratio = 0.2

[gate.strict_v1]
min_train_samples = 300
min_validation_samples = 80
min_train_waypoints = 150
min_validation_waypoints = 40
max_validation_p95_residual_nm = [0.8, 1.2, 1.2, 0.8, 0.6, 0.4]
max_validation_rms_residual_nm = [0.4, 0.7, 0.7, 0.4, 0.3, 0.2]
max_validation_train_p95_ratio = 2.0
max_validation_train_rms_ratio = 2.0
max_compensated_delta_ratio = 0.65
max_training_range_violations = 0
```

The gate defaults are conservative starting points for hardware experiments.
They must be configurable per profile because payloads and arms may differ.

## Manifest

`manifest.json` tracks registered artifacts, rounds, decisions, and events.

Each artifact entry includes:

```json
{
  "id": "samples-20260502-001",
  "kind": "samples",
  "split": "validation",
  "path": "data/validation/samples/20260502-001.samples.jsonl",
  "sha256": "...",
  "source_path_id": "path-20260502-001",
  "role": "slave",
  "arm_id": "piper-sv18-6-can1",
  "target": "socketcan:can1",
  "joint_map": "identity",
  "load_profile": "normal-gripper-d405",
  "sample_count": 260,
  "waypoint_count": 180,
  "created_at_unix_ms": 1770000000000
}
```

The manager must validate imported or generated artifacts before registration:

- role matches the profile
- target matches the profile unless a documented override is used
- joint map matches
- load profile matches
- torque convention matches
- basis is supported
- file hash is stable after registration
- the same file is not registered under two active splits

## CLI

Add a high-level command group:

```bash
piper-cli gravity profile <action>
```

Actions:

```text
init
status
next
record-path
replay-sample
import-samples
fit-assess
promote-validation
```

The low-level gravity commands remain available and keep their current behavior.
Profile commands orchestrate those commands and update the manifest.

### init

Creates the profile directory, writes `profile.toml`, and initializes
`manifest.json`.

Example:

```bash
piper-cli gravity profile init \
  --name slave-piper-sv18-6-can1-normal-gripper-d405 \
  --role slave \
  --arm-id piper-sv18-6-can1 \
  --target socketcan:can1 \
  --joint-map identity \
  --load-profile normal-gripper-d405
```

### status

Prints current state:

- profile identity
- train and validation artifact counts
- sample and waypoint counts
- latest round
- best model path if any
- current status
- last failed checks

### next

Prints the next recommended operator action. It does not mutate state.

Examples:

```text
needs_train_data -> collect train samples
needs_validation_data -> collect validation samples
ready_to_fit -> run fit-assess
validation_failed -> run promote-validation, then collect new validation samples
passed -> model is usable; optionally collect more validation
```

### record-path

Runs low-level `gravity record-path` using profile defaults and stores output in
the requested split:

```bash
piper-cli gravity profile record-path --profile <DIR> --split train
piper-cli gravity profile record-path --profile <DIR> --split validation
```

Generated path artifacts are registered in the manifest.

### replay-sample

Runs low-level `gravity replay-sample` against a profile path artifact and stores
samples under the same split unless explicitly overridden.

The command supports `--path latest` for the latest path in a split.

### import-samples

Registers existing sample files into a profile split after validation and hash
calculation. This supports data collected before the profile manager exists.

### fit-assess

Runs a complete training and assessment round:

1. Load profile and manifest.
2. Gather all active train samples.
3. Gather all active validation samples.
4. Fit `models/round-N.model.toml` from train samples.
5. Preserve the existing internal deterministic group holdout inside `fit`.
6. Evaluate the model on all train samples.
7. Evaluate the model on all validation samples.
8. Build `reports/round-N.assess.json`.
9. Write `rounds/round-N.json`.
10. If the gate passes, update `models/best.model.toml` and status `passed`.
11. If the gate fails, set status `validation_failed`.

`fit-assess` must not promote validation data automatically.

### promote-validation

Requires explicit operator invocation. Moves current validation artifacts into
the train split and records a promotion event. After promotion, the profile
status becomes `needs_validation_data`.

Validation artifacts are physically moved into `data/train/...` so the directory
view matches the manifest. The event records original paths and original split.

## State Machine

Profile status values:

```text
needs_train_data
needs_validation_data
ready_to_fit
fit_failed
validation_failed
passed
```

Transitions:

```text
needs_train_data
  -> needs_validation_data
  -> ready_to_fit
  -> fit_failed | validation_failed | passed

validation_failed
  -> promote-validation
  -> needs_validation_data
```

The manager derives status from manifest state when possible and persists the
last explicit status for UX.

## Assessment Report

`reports/round-N.assess.json` contains raw metrics, derived metrics, and a
decision.

Raw metrics:

```json
{
  "train": {
    "sample_count": 1200,
    "waypoint_count": 500,
    "rms_residual_nm": [],
    "p95_residual_nm": [],
    "max_residual_nm": []
  },
  "validation": {
    "sample_count": 260,
    "waypoint_count": 120,
    "rms_residual_nm": [],
    "p95_residual_nm": [],
    "max_residual_nm": [],
    "raw_torque_delta_nm": [],
    "compensated_external_torque_delta_nm": [],
    "training_range_violations": 0,
    "max_range_violation_rad": 0.0
  },
  "fit_internal_holdout": {
    "rms_residual_nm": [],
    "p95_residual_nm": [],
    "max_residual_nm": []
  }
}
```

Derived metrics:

```json
{
  "validation_train_p95_ratio": [],
  "validation_train_rms_ratio": [],
  "compensated_delta_ratio": [],
  "worst_joint_by_p95": "J3",
  "worst_joint_p95_nm": 1.62
}
```

Decision:

```json
{
  "pass": false,
  "grade": "risky",
  "failed_checks": [
    {
      "check": "validation_p95_residual_nm",
      "joint": "J3",
      "value": 1.62,
      "threshold": 1.2
    }
  ],
  "next_action": "promote_validation_then_collect_new_validation"
}
```

Grades:

```text
good   -> all strict checks pass with margin
usable -> all strict checks pass
risky  -> some checks fail but data is sufficient
bad    -> insufficient data, range violations, or large residuals
```

Pass is true only for `good` or `usable`.

## Gate Checks

The strict v1 gate checks:

- train sample count
- validation sample count
- train waypoint count
- validation waypoint count
- validation RMS residual per joint
- validation P95 residual per joint
- validation/train RMS ratio per joint
- validation/train P95 ratio per joint
- compensated torque delta ratio per joint
- training range violations

`compensated_delta_ratio[j]` is:

```text
validation.compensated_external_torque_delta_nm[j]
/
max(validation.raw_torque_delta_nm[j], epsilon)
```

If the raw torque delta is near zero for a joint, the report should mark that
joint's improvement ratio as not meaningful rather than failing only on division
noise.

## Iteration Policy

When validation fails:

1. `fit-assess` writes a failed assessment and recommends next action.
2. The operator reviews the report.
3. The operator runs `promote-validation`.
4. The manager moves validation paths and samples into train.
5. The manager records a promotion event.
6. The profile status becomes `needs_validation_data`.
7. The operator collects fresh validation data.
8. The next `fit-assess` trains on the expanded train pool and validates on the
   new validation pool.

This loop continues until the gate passes.

## Error Handling

The manager must refuse to continue on:

- missing profile files
- malformed `profile.toml`
- malformed `manifest.json`
- unknown schema versions
- artifact hash mismatch
- role, target, joint map, load profile, or torque convention mismatch
- missing train samples for `fit-assess`
- missing validation samples for `fit-assess`
- output path collision
- attempt to promote when status is not `validation_failed`

Errors must name the affected profile, artifact id, and file path when possible.

## Testing

Unit tests should cover:

- profile name and directory initialization
- profile config parsing and validation
- manifest round-trip and atomic write behavior
- artifact registration and hash mismatch detection
- split mismatch rejection
- target/joint-map/load-profile mismatch rejection
- train/validation aggregation
- strict-v1 gate pass and fail cases
- derived metric calculations
- `fit-assess` status transitions with synthetic samples
- `promote-validation` movement and manifest event recording
- CLI parsing for each action

Integration-style tests should run without hardware by using synthetic sample
artifacts and temporary directories.

## Migration and Compatibility

Existing low-level gravity artifacts remain valid. `import-samples` can register
existing `.samples.jsonl` files into a new profile if metadata matches.

The current scripts can become thin wrappers around profile commands. They must
not be the source of truth for profile state.

The manager must not change existing low-level command defaults in v1 except
where already required for correctness.
