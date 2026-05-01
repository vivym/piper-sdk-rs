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
- `joint_map`
- `load_profile`
- `torque_convention`
- `basis`

`arm_id` is required. The manager must not silently use `can0` or `can1` as the
physical identity because CAN interface names can be swapped.

`target` is not part of profile identity. It is the current execution
connection and per-artifact acquisition provenance. If the same physical arm is
moved from `can1` to `can0`, the operator updates `target` in `profile.toml`;
existing artifacts remain compatible because compatibility is based on
`arm_id`, not on the historical target.

`profile.toml` stores the canonical target string. CLI actions may accept
`--interface can1` as an ergonomic shortcut, but it must be resolved to
`target = "socketcan:can1"` before profile validation and manifest writes.

Default profile name:

```text
<role>-<arm-id>-<load-profile>
```

Example:

```text
slave-piper-left-normal-gripper-d405
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
name = "slave-piper-left-normal-gripper-d405"
role = "slave"
arm_id = "piper-left"
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
holdout_group_key = "source_path_id"

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
good_margin_fraction = 0.25
torque_delta_epsilon_nm = 0.05
```

The gate defaults are conservative starting points for hardware experiments.
They must be configurable per profile because payloads and arms may differ.

Profile hashes are computed from parsed, normalized configuration, not raw TOML
bytes. The canonical input is JSON with sorted object keys, stable field names,
and no whitespace. Formatting changes and TOML comments do not change hashes.

The manager stores two hashes:

- `profile_identity_sha256`: hash of only the identity fields.
- `profile_config_sha256`: hash of the full typed profile configuration,
  including runtime target and gate settings.

Changing `target` changes `profile_config_sha256` but not
`profile_identity_sha256`. Changing any identity field means the operator is
describing a different profile; the manager must refuse to reuse the existing
manifest for that identity.

## Manifest

`manifest.json` tracks registered artifacts, rounds, decisions, and events.

Top-level shape:

```json
{
  "schema_version": 1,
  "profile_name": "slave-piper-left-normal-gripper-d405",
  "profile_identity_sha256": "...",
  "profile_config_sha256": "...",
  "status": "needs_train_data",
  "next_artifact_seq": 1,
  "next_round_seq": 1,
  "current_best_model": null,
  "artifacts": [],
  "rounds": [],
  "events": []
}
```

On command startup, the manager recomputes both profile hashes. If
`profile_identity_sha256` differs from the manifest, the command fails because
the directory is for a different profile. If only `profile_config_sha256`
differs, the manager records a config-change event and updates the manifest hash
before running the requested action.

IDs are allocated monotonically from manifest counters. Round IDs use
`round-0001`, `round-0002`, and so on. Artifact IDs use exactly
`<kind>-<YYYYMMDD-HHMMSS>-<seq4>`, for example
`samples-20260502-001530-0001`. `next_artifact_seq` is a profile-wide global
counter shared by all artifact kinds; it is not reset per kind or per day.

Sample artifact entries include:

```json
{
  "id": "samples-20260502-001530-0002",
  "kind": "samples",
  "split": "validation",
  "active": true,
  "path": "data/validation/samples/samples-20260502-001530-0002.samples.jsonl",
  "sha256": "...",
  "source_path_id": "path-20260502-001000-0001",
  "role": "slave",
  "arm_id": "piper-left",
  "target": "socketcan:can1",
  "joint_map": "identity",
  "load_profile": "normal-gripper-d405",
  "torque_convention": "piper-sdk-normalized-nm-v1",
  "basis": "trig-v1",
  "sample_count": 260,
  "waypoint_count": 180,
  "created_at_unix_ms": 1770000000000
}
```

Path artifact entries use the same common metadata and set
`source_path_id = null`:

```json
{
  "id": "path-20260502-001000-0001",
  "kind": "path",
  "split": "validation",
  "active": true,
  "path": "data/validation/paths/path-20260502-001000-0001.path.jsonl",
  "sha256": "...",
  "source_path_id": null,
  "role": "slave",
  "arm_id": "piper-left",
  "target": "socketcan:can1",
  "joint_map": "identity",
  "load_profile": "normal-gripper-d405",
  "torque_convention": "piper-sdk-normalized-nm-v1",
  "basis": "trig-v1",
  "waypoint_count": 180,
  "created_at_unix_ms": 1770000000000
}
```

`source_path_id` is required for samples generated by profile
`replay-sample`. It may be `null` only for imported samples whose source path is
unknown.

Artifact lifecycle rules:

- New artifacts are registered with `active = true`.
- Promotion mutates the existing artifact entries in place: `split` changes from
  `validation` to `train`, `path` changes to the new `data/train/...` location,
  and `sha256` remains unchanged.
- Promotion adds `previous_paths` and `promoted_from_round_id` fields to each
  moved artifact entry.
- Artifact IDs do not change during promotion, so historical round files remain
  valid references to the same data.
- `retired-validation/` is reserved for future manual quarantine and is not used
  by v1 promotion.

The manager must validate imported or generated artifacts before registration:

- role matches the profile
- arm id matches the profile
- joint map matches
- load profile matches
- torque convention matches
- basis matches the profile and is supported
- file hash is stable after registration
- the same file is not registered under two active splits

Artifact target mismatch with the current profile target is allowed when
`arm_id` matches. The target remains useful provenance and should be shown in
`status` if active artifacts were collected through multiple targets.

Manifest round entries include enough provenance to reproduce the decision:

```json
{
  "id": "round-0001",
  "status": "validation_failed",
  "model_path": "models/round-0001.model.toml",
  "model_sha256": "...",
  "report_path": "reports/round-0001.assess.json",
  "report_sha256": "...",
  "round_path": "rounds/round-0001.json",
  "round_sha256": "...",
  "train_sample_artifact_ids": ["samples-20260502-001530-0001"],
  "validation_sample_artifact_ids": ["samples-20260502-002100-0002"],
  "validation_path_artifact_ids": ["path-20260502-002000-0001"],
  "profile_identity_sha256": "...",
  "profile_config_sha256": "...",
  "gate_config": {},
  "created_at_unix_ms": 1770000000000
}
```

`current_best_model` is either `null` or:

```json
{
  "round_id": "round-0003",
  "path": "models/best.model.toml",
  "sha256": "...",
  "source_model_path": "models/round-0003.model.toml",
  "source_model_sha256": "...",
  "promoted_at_unix_ms": 1770000000000
}
```

Each `rounds/round-N.json` file contains the full immutable round provenance:

```json
{
  "schema_version": 1,
  "id": "round-0001",
  "profile": {
    "name": "slave-piper-left-normal-gripper-d405",
    "identity_sha256": "...",
    "config_sha256": "..."
  },
  "fit": {
    "basis": "trig-v1",
    "ridge_lambda": 0.0001,
    "holdout_ratio": 0.2,
    "holdout_group_key": "source_path_id",
    "holdout_train_group_keys": [],
    "holdout_validation_group_keys": [],
    "final_train_policy": "refit_all_train_after_holdout"
  },
  "gate_config": {},
  "inputs": {
    "train_sample_artifact_ids": [],
    "validation_sample_artifact_ids": [],
    "validation_path_artifact_ids": []
  },
  "outputs": {
    "model_path": "models/round-0001.model.toml",
    "model_sha256": "...",
    "report_path": "reports/round-0001.assess.json",
    "report_sha256": "..."
  },
  "decision": {
    "pass": false,
    "grade": "risky",
    "failed_checks": []
  },
  "created_at_unix_ms": 1770000000000
}
```

`validation_path_artifact_ids` contains the non-null `source_path_id` values for
the validation sample artifacts used by that round. It may be empty when all
validation samples were imported without known source paths.

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
  --name slave-piper-left-normal-gripper-d405 \
  --role slave \
  --arm-id piper-left \
  --target socketcan:can1 \
  --joint-map identity \
  --load-profile normal-gripper-d405
```

### status

Prints current state:

- profile identity
- current execution target
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
insufficient_data -> collect more train or validation samples
fit_failed -> inspect fit error, fix data/config, rerun fit-assess
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
samples under the same split as the source path artifact. Cross-split replay is
forbidden in v1 because it makes provenance and promotion ambiguous.

The command supports `--path latest` for the latest path in a split.

### import-samples

Registers existing sample files into a profile split after validation and hash
calculation. This supports data collected before the profile manager exists.

### fit-assess

Runs a complete training and assessment round:

1. Load profile and manifest.
2. Gather all active train samples.
3. Gather all active validation samples.
4. Run a diagnostic fit using the deterministic internal group holdout.
5. Fit the final `models/round-N.model.toml` from all active train samples.
6. Evaluate the final model on all train samples.
7. Evaluate the final model on all validation samples.
8. Build `reports/round-N.assess.json`, including the diagnostic holdout
   metrics from step 4.
9. Write `rounds/round-N.json`.
10. If the gate passes, update `models/best.model.toml` and status `passed`.
11. If data counts are below gate minimums, set status `insufficient_data`.
12. If fitting or serialization fails after data is sufficient, set status
    `fit_failed`.
13. Otherwise, set status `validation_failed`.

`fit-assess` must not promote validation data automatically.

The final round model is always trained on all active train samples. The
internal holdout is only an assessment signal; it must not permanently remove
holdout samples from the promoted model.

The diagnostic holdout split is deterministic:

1. Group train samples by `holdout_group_key`. The v1 default is
   `source_path_id`; imported samples with `source_path_id = null` use their own
   sample artifact ID as the group key.
2. Sort group keys lexicographically.
3. Select holdout groups by hashing
   `<profile_identity_sha256>:<round_id>:<group_key>` with SHA-256, sorting by
   hash, and taking enough whole groups to meet or slightly exceed
   `holdout_ratio` by sample count.
4. If there is only one group, the diagnostic holdout is empty and the report
   records `fit_internal_holdout.available = false`.

The selected train and holdout group keys are stored in `rounds/round-N.json`.

`models/best.model.toml` is a regular file copy of the passing round model, not
a symlink. This keeps downstream teleop commands independent of filesystem
symlink support.

### promote-validation

Requires explicit operator invocation. Promotion operates on the validation
sample artifact IDs and source path artifact IDs captured by the latest
`validation_failed` round. If the active validation sample set has changed since
that round, the command must reject and tell the operator to run `fit-assess`
again.

Promoted validation artifacts are physically moved into `data/train/...` so the
directory view matches the manifest. The event records original paths, original
split, destination paths, and the source failed round ID. After promotion, the
profile status becomes `needs_validation_data`.

Promotion updates the existing manifest artifact entries in place rather than
creating replacement IDs. Each moved artifact keeps the same `id`, `kind`, and
`sha256`, changes `split` and `path`, and receives:

```json
{
  "promoted_from_round_id": "round-0002",
  "previous_paths": [
    {
      "split": "validation",
      "path": "data/validation/samples/samples-20260502-001530-0002.samples.jsonl"
    }
  ]
}
```

## State Machine

Profile status values:

```text
needs_train_data
needs_validation_data
ready_to_fit
insufficient_data
fit_failed
validation_failed
passed
```

Transitions:

```text
needs_train_data
  -> needs_validation_data
  -> ready_to_fit
  -> insufficient_data | fit_failed | validation_failed | passed

validation_failed
  -> promote-validation
  -> needs_validation_data

insufficient_data | fit_failed | passed
  -> sample-pool mutation
  -> needs_train_data | needs_validation_data | ready_to_fit
```

Status derivation uses gate minimums:

- `needs_train_data`: no active train samples exist.
- `needs_validation_data`: train samples exist but no active validation samples
  exist.
- `ready_to_fit`: both train and validation samples exist. This status does not
  require gate minimums to be met; it means `fit-assess` can produce a useful
  report.
- `insufficient_data`: `fit-assess` ran but train or validation sample/waypoint
  counts were below configured gate minimums.
- `fit_failed`: fitting or model serialization failed after inputs were
  well-formed and sufficient to attempt fitting.
- `validation_failed`: fitting succeeded, data was sufficient, and at least one
  validation gate check failed.
- `passed`: fitting succeeded and all gate checks passed.

After every mutating command, the manager applies these rules:

- `record-path`: registers a path artifact but does not change sample readiness.
- `replay-sample` and `import-samples`: register sample artifacts, invalidate
  any previous round-result status, and recompute readiness from active samples.
- `fit-assess`: consumes the current active train and validation sample IDs and
  sets one of `insufficient_data`, `fit_failed`, `validation_failed`, or
  `passed`.
- `promote-validation`: allowed only from the latest `validation_failed` round
  when active validation sample IDs still match that round; after promotion,
  recompute readiness, which should normally be `needs_validation_data`.

Malformed data, metadata mismatches, hash mismatches, and missing files are hard
command errors. They do not become profile statuses because they require
operator repair.

The manager derives data-readiness status from active sample artifacts and
persists the last explicit round-result status for UX. If a sample-pool mutation
occurs after `passed`, the previous best model remains recorded in
`current_best_model`, but the profile status returns to `ready_to_fit` or a
lower readiness state because the latest active data has not been assessed yet.

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
  "compensated_delta_ratio_meaningful": [true, true, true, true, true, true],
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
good   -> all strict checks pass with configured good margin
usable -> all strict checks pass
risky  -> some checks fail but data is sufficient
bad    -> insufficient data, range violations, or large residuals
```

Pass is true only for `good` or `usable`.

The default good margin is 25 percent. A passing metric is `good` if it is at or
below `0.75 * threshold`. Count and coverage minimums are `good` if they are at
or above `1.25 * threshold`. The margin is configurable in `profile.toml`:

```toml
[gate.strict_v1]
good_margin_fraction = 0.25
```

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

The default `epsilon` is `gate.strict_v1.torque_delta_epsilon_nm`, initially
`0.05 Nm`. For joint `j`:

- if `raw_torque_delta_nm[j] >= epsilon`, compute and gate
  `compensated_delta_ratio[j]`
- if `raw_torque_delta_nm[j] < epsilon`, set
  `compensated_delta_ratio[j] = null`,
  `compensated_delta_ratio_meaningful[j] = false`, and skip that joint for the
  compensated-delta gate

Skipping a near-zero joint is not a pass by itself; the model still must pass
all residual and range gates.

## Iteration Policy

When validation fails:

1. `fit-assess` writes a failed assessment and recommends next action.
2. The operator reviews the report.
3. The operator runs `promote-validation`.
4. The manager verifies the active validation sample IDs still match the failed
   round.
5. The manager moves that round's validation paths and samples into train.
6. The manager records a promotion event.
7. The profile status becomes `needs_validation_data`.
8. The operator collects fresh validation data.
9. The next `fit-assess` trains on the expanded train pool and validates on the
   new validation pool.

This loop continues until the gate passes.

## Error Handling

The manager must refuse to continue on:

- missing profile files
- malformed `profile.toml`
- malformed `manifest.json`
- unknown schema versions
- artifact hash mismatch
- role, arm id, joint map, load profile, torque convention, or basis mismatch
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
- arm-id/joint-map/load-profile/torque-convention/basis mismatch rejection
- train/validation aggregation
- deterministic diagnostic holdout group selection
- strict-v1 gate pass and fail cases
- near-zero torque-delta gate skip behavior
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
