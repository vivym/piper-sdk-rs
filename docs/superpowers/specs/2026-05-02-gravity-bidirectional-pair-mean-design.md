# Gravity Bidirectional Pair-Mean Design

## Context

The gravity profile workflow can fit a model and produce low absolute residuals, but recent
validation rounds still fail `compensated_delta_ratio`. The failure is not a solver failure:
models are generated and validation RMS/P95 residual gates pass. The remaining failure is caused
by evaluating a q-only gravity model against raw row-level torque span.

Bidirectional replay data shows significant forward/backward torque differences at nearly the same
waypoints. Those differences are direction-dependent friction, stiction, backlash, or drive state,
not gravity. A q-only gravity model should fit the direction-even component of torque and report the
direction-dependent component as a diagnostic.

## Goals

- Fit gravity from the direction-even torque component when bidirectional samples are available.
- Keep raw row evaluation for diagnostics, but stop using it as the primary gravity pass/fail gate.
- Preserve the current profile manager workflow and artifact formats where possible.
- Make reports explain why a model passes or fails in terms of effective paired data, coverage, and
  hysteresis.
- Keep the first implementation q-only. Do not introduce a friction model yet.

## Non-Goals

- No online friction compensation.
- No MuJoCo or robot-structure model.
- No change to CAN collection or replay control behavior.
- No schema change to existing `.samples.jsonl` artifacts.

## Design Summary

Add a sample reduction layer between raw quasi-static samples and fit/eval. New gravity profiles
created by `gravity profile init` should write this profile setting explicitly:

```text
bidirectional-pair-mean-v1
```

Profiles that omit `fit.sample_reduction` must load as `raw-rows` for backward compatibility.
Existing profiles opt in by editing `profile.toml` or by a future migration command.

For each source sample artifact, group accepted samples by waypoint identity. If a group has both
`forward` and `backward` rows, create one effective gravity row:

```text
q_gravity   = mean(q_forward, q_backward)
tau_gravity = mean(tau_forward, tau_backward)
dq          = mean(dq_forward, dq_backward)
```

The same pair also produces diagnostics:

```text
pair_q_error_rad[joint]       = abs(q_forward[joint] - q_backward[joint])
direction_torque_delta_nm[j]  = abs(tau_forward[j] - tau_backward[j])
```

Rows without a valid forward/backward pair do not participate in gravity fit by default. They are
counted as unpaired diagnostics so the operator can decide whether to collect better bidirectional
coverage.

## Components

### `gravity::sample_reduction`

New module that exposes:

```rust
pub enum ReductionMode {
    RawRows,
    BidirectionalPairMeanV1,
}

pub struct SourceSampleArtifact {
    pub source_id: String,
    pub path: PathBuf,
    pub header: SamplesHeader,
    pub rows: Vec<QuasiStaticSampleRow>,
}

pub struct EffectiveSampleRow {
    pub source_id: String,
    pub group_id: String,
    pub waypoint_id: u64,
    pub q_rad: [f64; 6],
    pub dq_rad_s: [f64; 6],
    pub tau_nm: [f64; 6],
}

pub struct ReductionReport {
    pub mode: ReductionMode,
    pub raw_sample_count: usize,
    pub effective_sample_count: usize,
    pub paired_waypoint_count: usize,
    pub unpaired_waypoint_count: usize,
    pub skipped_pair_q_error_count: usize,
    pub duplicate_forward_count: usize,
    pub duplicate_backward_count: usize,
    pub pair_q_error_p95_rad: [f64; 6],
    pub pair_q_error_max_rad: [f64; 6],
    pub direction_torque_delta_p95_nm: [f64; 6],
    pub direction_torque_delta_max_nm: [f64; 6],
}
```

The reducer must preserve file/source identity, because separate sample artifacts can reuse
`waypoint_id`. A pair key is:

```text
(source_id, waypoint_id)
```

Do not use `segment_id` alone as a pairing key. `segment_id` can identify a path segment containing
many waypoints, while `waypoint_id` identifies the replay waypoint. If `segment_id` is useful for
diagnostics, include it in diagnostic output only, or use it as part of a non-pairing label.

For generated/imported profile samples, `source_id` must be the profile sample artifact id. For
standalone fit/eval commands that do not know manifest IDs, use a canonical input file path as the
source id.

If a pair key contains multiple accepted rows for the same direction, average all forward rows first
and all backward rows first, then average the two direction means into the effective gravity row.
Record duplicate direction counts in `ReductionReport`.

A pair is valid iff:

```text
max_j abs(q_forward_mean[j] - q_backward_mean[j]) <= pair_q_error_max_rad
```

Pairs above that threshold are skipped and counted in `skipped_pair_q_error_count`.

### Fit

Refactor the fitter so normal-equation accumulation operates on an internal row trait/struct rather
than directly on `QuasiStaticSampleRow`.

Profile `fit-assess` should:

1. Load active train artifacts with source IDs.
2. Reduce train rows with `bidirectional-pair-mean-v1`.
3. Fit the final model on effective train rows.
4. Record both raw sample count and effective sample count.

Low-level `gravity fit` can keep raw behavior initially for compatibility, but the reusable internal
function should support effective rows so the profile workflow uses the reduced target.

Do not add reduction fields to the model TOML in the first implementation. `QuasiStaticTorqueModel`
uses strict schema validation. Store raw/effective counts and reduction diagnostics in assessment
reports and round provenance. `model.sample_count` should remain the number of rows actually used
for fitting, which is the effective row count in pair-mean mode.

### Eval

Profile assessment should compute two evaluations:

- `gravity_target`: evaluation on effective pair-mean rows. This is the primary gate input.
- `raw_rows`: evaluation on raw rows. This remains diagnostic.

The existing `train` and `validation` sections should represent the gravity target metrics after
reduction. Add explicit diagnostic sections for raw rows and hysteresis:

```json
{
  "reduction": {
    "mode": "bidirectional-pair-mean-v1",
    "train": { "...": "ReductionReport" },
    "validation": { "...": "ReductionReport" }
  },
  "raw_rows": {
    "validation": {
      "rms_residual_nm": [...],
      "p95_residual_nm": [...],
      "raw_torque_delta_nm": [...],
      "compensated_external_torque_delta_nm": [...]
    }
  },
  "hysteresis": {
    "validation_direction_torque_delta_p95_nm": [...],
    "validation_direction_torque_delta_max_nm": [...]
  }
}
```

### Gate

`gravity_compensated_delta_ratio` should be computed from the gravity-target validation rows. This is
the metric that participates in strict gate decisions:

```text
gravity_compensated_delta_ratio
```

The raw row compensated ratio should be reported as:

```text
raw_row_compensated_delta_ratio
```

For rollout compatibility, keep the old `compensated_delta_ratio` report field as an alias of
`gravity_compensated_delta_ratio` for at least one implementation cycle. New code and status output
should prefer the explicit gravity/raw-row names.

Define the same meaningful/skipped semantics for gravity ratio as the old ratio:

```text
gravity_compensated_delta_ratio_meaningful[j] =
  gravity_raw_torque_delta_nm[j] >= torque_delta_epsilon_nm
```

If a joint is not meaningful, skip that joint and show the skip in the decision output. A strict pass
requires at least one meaningful gravity-ratio joint. Zero meaningful joints should be `usable` at
most, not `good`.

Only `gravity_compensated_delta_ratio` participates in strict pass/fail decisions. Raw row ratio and
hysteresis are diagnostics.

Add a minimum effective pair gate so profiles cannot pass with too few reduced rows. In pair-mean
mode, one effective row equals one valid paired waypoint, so do not add duplicate sample and waypoint
thresholds:

```toml
[gate.strict_v1]
min_train_effective_pairs = 300
min_validation_effective_pairs = 80
```

For compatibility, omitted fields default to conservative values. These fields only apply when
`sample_reduction = "bidirectional-pair-mean-v1"`.

`fit-assess` workflow order must change for pair-mean mode:

1. Verify active artifacts.
2. Load source-aware train and validation samples.
3. Reduce train and validation samples.
4. Gate on effective pair counts.
5. Fit/evaluate only if effective counts pass.

This prevents “raw counts pass, reduction yields zero pairs” from being recorded as a solver or
fit failure.

### Status UX

`gravity profile status` should show:

```text
Effective train samples: 1234 paired waypoints
Effective validation samples: 207 paired waypoints
Unpaired validation waypoints: 309
Validation pair q-error p95 rad: [...]
Validation hysteresis p95 Nm: [...]
Gravity compensated ratio: [...]
Raw-row compensated ratio: [...] (diagnostic)
```

When a model fails, failed checks should distinguish gravity gate failures from raw-row diagnostics.

## Data Compatibility

Existing `.samples.jsonl` files already include:

- `waypoint_id`
- `segment_id`
- `pass_direction`
- `q_rad`
- `tau_nm`

No data migration is required. Existing profile data can be re-assessed with the new reducer.

## Error Handling

- If bidirectional mode is requested but no valid pairs exist, `fit-assess` should fail the round as
  insufficient data, not as a solver failure.
- If a pair has `q` mismatch above the configured threshold, skip the pair and count it in
  `skipped_pair_q_error_count`.
- If too few effective pairs remain, the report should be count-only with a clear next action:
  collect bidirectional samples or improve tracking acceptance.
- Raw rows with only one direction are retained for diagnostics but not used for gravity target fit.

## Configuration

Add profile config:

```toml
[fit]
sample_reduction = "bidirectional-pair-mean-v1"
pair_q_error_max_rad = 0.05
```

`sample_reduction = "raw-rows"` remains available for debugging and legacy behavior.

Use kebab-case strings in TOML and report JSON:

```text
raw-rows
bidirectional-pair-mean-v1
```

The Rust enum variants may stay `RawRows` and `BidirectionalPairMeanV1`.

Because profile config structs use strict field validation, all new config fields must have serde
defaults. Existing `profile.toml` files without these fields must continue to load.

Diagnostic holdout should use the same sample reduction mode as the final fit. Holdout selection may
still choose artifact groups using existing manifest metadata, but once train/holdout artifacts are
chosen, each side must be reduced before fitting or evaluating holdout metrics.

## Operator Next Actions

`gravity profile next` and status summaries should distinguish these cases:

- Too few effective pairs: collect bidirectional samples or improve acceptance.
- High q-error skips: lower replay velocity, increase settle time, or loosen tracking only after
  inspecting safety.
- High unpaired count but enough pairs: continue; report unpaired as diagnostic.
- Gravity gates pass but raw-row hysteresis remains high: model is usable for gravity compensation,
  and hysteresis is expected unmodeled friction.
- Gravity compensated ratio fails: promote validation to train and collect a new validation path, or
  inspect whether the path is outside training range.

## Acceptance Criteria

The implementation should be validated against the existing failing profile artifacts, not only
synthetic tests. For the current slave follower profile data:

- Re-assessment should report effective train/validation pair counts.
- Re-assessment should report pair q-error p95/max and hysteresis p95/max.
- `gravity_compensated_delta_ratio` should be the strict gate metric.
- `raw_row_compensated_delta_ratio` may remain high and must be diagnostic only.
- If any gravity ratio still fails, the report must show whether the failure is caused by gravity
  target residuals, training range, or insufficient effective pairs.

## Testing

Unit tests:

- Reducer averages matched forward/backward rows.
- Reducer keeps source IDs separate when two files reuse the same waypoint IDs.
- Reducer does not pair rows that only share `segment_id`.
- Reducer reports unpaired rows and q-error skips.
- Reducer averages duplicate rows per direction before pair-meaning.
- Fitter recovers a synthetic gravity model when raw rows include opposite direction friction.
- Raw-row fitting fails or has worse span on the same synthetic friction data, proving the regression.
- Assessment gates on gravity compensated ratio and reports raw-row ratio only as diagnostic.
- Status output includes effective sample counts and failed gravity checks.
- Existing profiles without `fit.sample_reduction` load as `raw-rows`.

Integration-style profile workflow tests:

- Existing train/validation artifacts can be re-assessed without data migration.
- A validation set with paired data and high raw hysteresis can pass if gravity target metrics pass.
- A validation set with too few pairs fails with an actionable insufficient-data report.
- Raw manifest counts passing but effective pair counts failing is recorded as insufficient data, not
  `fit_failed`.
- Diagnostic holdout is reduced before fitting/evaluation in pair-mean mode.

## Rollout

1. Implement reducer and tests.
2. Refactor fit/eval internals to accept effective rows while preserving low-level CLI behavior.
3. Wire profile `fit-assess` through pair-mean reduction.
4. Extend report schema and status UX.
5. Re-run existing profile data with `cargo run -p piper-cli -- gravity profile fit-assess`.
6. Compare round reports before/after:
   - `gravity_compensated_delta_ratio` should improve.
   - `raw_row_compensated_delta_ratio` may remain high and should be interpreted as hysteresis.
