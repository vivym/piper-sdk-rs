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

Add a sample reduction layer between raw quasi-static samples and fit/eval. The default profile
reduction mode is:

```text
bidirectional_pair_mean_v1
```

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
    pub pair_q_error_p95_rad: [f64; 6],
    pub pair_q_error_max_rad: [f64; 6],
    pub direction_torque_delta_p95_nm: [f64; 6],
    pub direction_torque_delta_max_nm: [f64; 6],
}
```

The reducer must preserve file/source identity, because separate sample artifacts can reuse
`waypoint_id`. A pair key is:

```text
source_id + segment_id_or_waypoint_id
```

For generated profile samples, `source_id` should be the sample artifact id. For standalone fit/eval
commands that do not know manifest IDs, use the input file path as the source id.

### Fit

Refactor the fitter so normal-equation accumulation operates on an internal row trait/struct rather
than directly on `QuasiStaticSampleRow`.

Profile `fit-assess` should:

1. Load active train artifacts with source IDs.
2. Reduce train rows with `bidirectional_pair_mean_v1`.
3. Fit the final model on effective train rows.
4. Record both raw sample count and effective sample count.

Low-level `gravity fit` can keep raw behavior initially for compatibility, but the reusable internal
function should support effective rows so the profile workflow uses the reduced target.

### Eval

Profile assessment should compute two evaluations:

- `gravity_target`: evaluation on effective pair-mean rows. This is the primary gate input.
- `raw_rows`: evaluation on raw rows. This remains diagnostic.

The existing `train` and `validation` sections should represent the gravity target metrics after
reduction. Add explicit diagnostic sections for raw rows and hysteresis:

```json
{
  "reduction": {
    "mode": "bidirectional_pair_mean_v1",
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

`compensated_delta_ratio` should be computed from the gravity-target validation rows. Rename or
alias it in reports as:

```text
gravity_compensated_delta_ratio
```

The raw row compensated ratio should be reported as:

```text
raw_row_compensated_delta_ratio
```

Only `gravity_compensated_delta_ratio` participates in strict gate decisions. Raw row ratio and
hysteresis are diagnostics.

Add a minimum effective data gate so profiles cannot pass with too few pairs:

```toml
[gate.strict_v1]
min_train_effective_samples = 300
min_validation_effective_samples = 80
min_train_effective_waypoints = 150
min_validation_effective_waypoints = 40
```

For compatibility, these can default to the existing sample/waypoint thresholds if omitted.

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

## Testing

Unit tests:

- Reducer averages matched forward/backward rows.
- Reducer keeps source IDs separate when two files reuse the same waypoint IDs.
- Reducer reports unpaired rows and q-error skips.
- Fitter recovers a synthetic gravity model when raw rows include opposite direction friction.
- Raw-row fitting fails or has worse span on the same synthetic friction data, proving the regression.
- Assessment gates on gravity compensated ratio and reports raw-row ratio only as diagnostic.
- Status output includes effective sample counts and failed gravity checks.

Integration-style profile workflow tests:

- Existing train/validation artifacts can be re-assessed without data migration.
- A validation set with paired data and high raw hysteresis can pass if gravity target metrics pass.
- A validation set with too few pairs fails with an actionable insufficient-data report.

## Rollout

1. Implement reducer and tests.
2. Refactor fit/eval internals to accept effective rows while preserving low-level CLI behavior.
3. Wire profile `fit-assess` through pair-mean reduction.
4. Extend report schema and status UX.
5. Re-run existing profile data with `cargo run -p piper-cli -- gravity profile fit-assess`.
6. Compare round reports before/after:
   - `gravity_compensated_delta_ratio` should improve.
   - `raw_row_compensated_delta_ratio` may remain high and should be interpreted as hysteresis.

