# Gravity Bidirectional Pair-Mean Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement source-aware bidirectional pair-mean gravity fitting so q-only gravity models are gated on direction-even torque, while raw row hysteresis remains diagnostic.

**Architecture:** Add a focused `gravity::sample_reduction` module that converts source-tagged raw sample artifacts into effective gravity rows and reduction diagnostics. Refactor fit/eval internals to operate on effective rows without changing `.samples.jsonl` artifacts, then wire profile `fit-assess` to load, reduce, gate, fit, assess, and report pair-mean metrics.

**Tech Stack:** Rust, Cargo, serde/TOML/JSON, nalgebra normal equations, existing `piper-cli` gravity profile workflow.

---

## Implementation Context

Spec: `docs/superpowers/specs/2026-05-02-gravity-bidirectional-pair-mean-design.md`

Run implementation in an isolated worktree. Keep `main` free for hardware experiments.

Expected final verification:

```bash
cargo test -p piper-cli gravity::sample_reduction -- --nocapture
cargo test -p piper-cli gravity::fit -- --nocapture
cargo test -p piper-cli gravity::eval -- --nocapture
cargo test -p piper-cli gravity::profile -- --nocapture
cargo fmt --all -- --check
cargo clippy -p piper-cli --all-targets --all-features -- -D warnings
```

Manual profile validation after implementation:

```bash
SRC_PROFILE=/home/viv/projs/piper-sdk-rs/artifacts/gravity/profiles/slave-piper-follower-normal-gripper-d405
TMP_PROFILE="artifacts/gravity/profiles/_tmp-pair-mean-validation-$(date +%Y%m%d-%H%M%S)"
test -d "$SRC_PROFILE"
test ! -e "$TMP_PROFILE"
mkdir -p "$(dirname "$TMP_PROFILE")"
cp -a "$SRC_PROFILE" "$TMP_PROFILE"
# Edit "$TMP_PROFILE/profile.toml" to opt in to bidirectional-pair-mean-v1 before running fit-assess.
cargo run -p piper-cli -- gravity profile fit-assess --profile "$TMP_PROFILE"
cargo run -p piper-cli -- gravity profile status --profile "$TMP_PROFILE"
REPORT=$(jq -r '.rounds[-1].report_path' "$TMP_PROFILE/manifest.json")
jq '{reduction, raw_rows, hysteresis, derived, decision}' "$TMP_PROFILE/$REPORT"
```

Never run `fit-assess` directly on the live experiment profile from this plan. It mutates the
manifest, rounds, reports, and models.

## File Structure

- Create `apps/cli/src/gravity/sample_reduction.rs`
  - Owns `ReductionMode`, `SourceSampleArtifact`, `EffectiveSampleRow`, `ReductionReport`, and raw/pair-mean reduction logic.
  - Does not know profile manifests except via caller-provided `source_id`.

- Modify `apps/cli/src/gravity/mod.rs`
  - Export `sample_reduction`.

- Modify `apps/cli/src/gravity/profile/config.rs`
  - Add config fields with serde defaults:
    - `fit.sample_reduction`
    - `fit.pair_q_error_max_rad`
    - `gate.strict_v1.min_train_effective_pairs`
    - `gate.strict_v1.min_validation_effective_pairs`
  - Preserve compatibility for existing profiles that omit these fields.

- Modify `apps/cli/src/gravity/fit.rs`
  - Introduce fit input abstraction for effective rows.
  - Keep public/CLI raw-row behavior intact.
  - Add `fit_model_from_effective_rows(...)`.

- Modify `apps/cli/src/gravity/eval.rs`
  - Introduce `evaluate_model_on_effective_rows(...)`.
  - Keep public/CLI raw-row behavior intact.
  - Keep `evaluate_model_on_rows(...)` as a raw diagnostic API.

- Modify `apps/cli/src/gravity/profile/assessment.rs`
  - Extend report schema with `reduction`, `raw_rows`, `hysteresis`, explicit gravity/raw ratios, and compatibility alias `compensated_delta_ratio`.
  - Gate on `gravity_compensated_delta_ratio`.

- Modify `apps/cli/src/gravity/profile/workflow.rs`
  - Load source-aware artifacts in `fit-assess`.
  - Move pair-mean count gating after reduction.
  - Reduce diagnostic holdout before fitting/evaluation.
  - Add status output for effective pairs, q-error, hysteresis, gravity ratio, and raw diagnostic ratio.

- Optional docs update after implementation:
  - `docs/superpowers/specs/2026-05-02-gravity-bidirectional-pair-mean-design.md` only if implementation requires a deliberate spec adjustment.

---

### Task 0: Create Isolated Worktree

**Files:**
- No code files.

- [ ] **Step 1: Verify plan and spec are committed on main**

This task starts after the reviewed plan has been committed. A new worktree only contains committed
files, so do not start implementation from an untracked plan document.

Run:

```bash
git status --short docs/superpowers/specs/2026-05-02-gravity-bidirectional-pair-mean-design.md docs/superpowers/plans/2026-05-05-gravity-bidirectional-pair-mean.md
git log --oneline -3 -- docs/superpowers/plans/2026-05-05-gravity-bidirectional-pair-mean.md
```

Expected: `git status` prints no changes for the spec/plan paths, and `git log` shows the plan
commit.

- [ ] **Step 2: Create a feature worktree**

Run:

```bash
git worktree add ../piper-sdk-rs-gravity-pair-mean -b gravity-pair-mean
cd ../piper-sdk-rs-gravity-pair-mean
```

Expected: new branch `gravity-pair-mean` checked out in the new directory.

- [ ] **Step 3: Confirm spec and plan are available**

Run:

```bash
test -f docs/superpowers/specs/2026-05-02-gravity-bidirectional-pair-mean-design.md
test -f docs/superpowers/plans/2026-05-05-gravity-bidirectional-pair-mean.md
```

Expected: both commands exit 0.

---

### Task 1: Add Config Fields With Backward-Compatible Defaults

**Files:**
- Modify: `apps/cli/src/gravity/profile/config.rs`

- [ ] **Step 1: Write failing config tests**

Add tests in `apps/cli/src/gravity/profile/config.rs`:

```rust
#[test]
fn missing_pair_mean_fields_default_to_raw_rows_for_legacy_profiles() {
    let input = r#"
name = "legacy"
role = "slave"
arm_id = "piper-follower"
target = "socketcan:can1"
joint_map = "identity"
load_profile = "normal-gripper-d405"
"#;

    let config = ProfileConfig::from_toml_str(input).unwrap();

    assert_eq!(config.fit.sample_reduction, SampleReductionMode::RawRows);
    assert_eq!(config.fit.pair_q_error_max_rad, 0.05);
    assert_eq!(config.gate.strict_v1.min_train_effective_pairs, 300);
    assert_eq!(config.gate.strict_v1.min_validation_effective_pairs, 80);
}

#[test]
fn pair_mean_config_round_trips_in_kebab_case() {
    let mut config = ProfileConfig::new(
        "pair-mean",
        "slave",
        "piper-follower",
        "socketcan:can1",
        "identity",
        "normal-gripper-d405",
    );
    config.fit.sample_reduction = SampleReductionMode::BidirectionalPairMeanV1;
    let toml = toml::to_string_pretty(&config).unwrap();

    assert!(toml.contains("sample_reduction = \"bidirectional-pair-mean-v1\""));

    let decoded = ProfileConfig::from_toml_str(&toml).unwrap();
    assert_eq!(decoded.fit.sample_reduction, SampleReductionMode::BidirectionalPairMeanV1);
}

#[test]
fn new_profiles_explicitly_default_to_pair_mean() {
    let config = ProfileConfig::new(
        "new-profile",
        "slave",
        "piper-follower",
        "socketcan:can1",
        "identity",
        "normal-gripper-d405",
    );

    assert_eq!(config.fit.sample_reduction, SampleReductionMode::BidirectionalPairMeanV1);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run:

```bash
cargo test -p piper-cli gravity::profile::config::tests::missing_pair_mean_fields_default_to_raw_rows_for_legacy_profiles -- --nocapture
cargo test -p piper-cli gravity::profile::config::tests::pair_mean_config_round_trips_in_kebab_case -- --nocapture
cargo test -p piper-cli gravity::profile::config::tests::new_profiles_explicitly_default_to_pair_mean -- --nocapture
```

Expected: fail because `SampleReductionMode` and new fields do not exist.

- [ ] **Step 3: Implement config types and defaults**

In `apps/cli/src/gravity/profile/config.rs`, add:

```rust
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum SampleReductionMode {
    RawRows,
    BidirectionalPairMeanV1,
}

fn default_sample_reduction() -> SampleReductionMode {
    SampleReductionMode::RawRows
}

fn default_pair_q_error_max_rad() -> f64 {
    0.05
}

fn default_min_train_effective_pairs() -> usize {
    300
}

fn default_min_validation_effective_pairs() -> usize {
    80
}
```

Extend `FitConfig`:

```rust
#[serde(default = "default_sample_reduction")]
pub sample_reduction: SampleReductionMode,
#[serde(default = "default_pair_q_error_max_rad")]
pub pair_q_error_max_rad: f64,
```

Extend `StrictGateConfig`:

```rust
#[serde(default = "default_min_train_effective_pairs")]
pub min_train_effective_pairs: usize,
#[serde(default = "default_min_validation_effective_pairs")]
pub min_validation_effective_pairs: usize,
```

Update `Default` impls and `validate()`:

```rust
validate_positive_f64("fit.pair_q_error_max_rad", self.fit.pair_q_error_max_rad)?;
validate_positive_usize(
    "gate.strict_v1.min_train_effective_pairs",
    strict.min_train_effective_pairs,
)?;
validate_positive_usize(
    "gate.strict_v1.min_validation_effective_pairs",
    strict.min_validation_effective_pairs,
)?;
```

Important compatibility rule:

- `FitConfig::default()` must keep `sample_reduction = RawRows` so old profiles with omitted
  `[fit]` or omitted `sample_reduction` load as legacy raw-row profiles.
- `ProfileConfig::new(...)` must override the fit config to
  `SampleReductionMode::BidirectionalPairMeanV1` so newly initialized profiles write the new mode
  explicitly.

Implementation sketch:

```rust
impl ProfileConfig {
    pub fn new(...) -> Self {
        let mut fit = FitConfig::default();
        fit.sample_reduction = SampleReductionMode::BidirectionalPairMeanV1;
        Self {
            // existing fields...
            fit,
            gate: GateConfig::default(),
        }
    }
}
```

- [ ] **Step 4: Run config tests**

Run:

```bash
cargo test -p piper-cli gravity::profile::config -- --nocapture
```

Expected: all profile config tests pass.

- [ ] **Step 5: Commit**

```bash
git add apps/cli/src/gravity/profile/config.rs
git commit -m "Add gravity sample reduction config"
```

---

### Task 2: Implement Source-Aware Sample Reduction

**Files:**
- Create: `apps/cli/src/gravity/sample_reduction.rs`
- Modify: `apps/cli/src/gravity/mod.rs`

- [ ] **Step 1: Write failing reducer tests and export the module**

Create `apps/cli/src/gravity/sample_reduction.rs` with tests first.

Also add the module export before running the failing test:

```rust
pub mod sample_reduction;
```

Without this export, `cargo test -p piper-cli gravity::sample_reduction` may run zero tests instead
of compiling the new test module.

Add the test-only helpers used below in the same `#[cfg(test)]` module:

- `source_for_tests(source_id, rows) -> SourceSampleArtifact`
- `row_for_tests(waypoint_id, pass_direction, q_rad, tau_nm) -> QuasiStaticSampleRow`
- A `sample_header_for_tests()` helper if one is not already reusable from another gravity test
  module.

Then add these tests:

```rust
#[test]
fn pair_mean_averages_forward_and_backward_rows() {
    let source = source_for_tests("samples-a", vec![
        row_for_tests(7, PassDirection::Forward, [1.0; 6], [10.0; 6]),
        row_for_tests(7, PassDirection::Backward, [3.0; 6], [14.0; 6]),
    ]);

    let reduced = reduce_samples(
        ReductionMode::BidirectionalPairMeanV1,
        &[source],
        ReductionOptions { pair_q_error_max_rad: 3.0 },
    ).unwrap();

    assert_eq!(reduced.rows.len(), 1);
    assert_eq!(reduced.rows[0].source_id, "samples-a");
    assert_eq!(reduced.rows[0].waypoint_id, 7);
    assert_eq!(reduced.rows[0].q_rad, [2.0; 6]);
    assert_eq!(reduced.rows[0].tau_nm, [12.0; 6]);
    assert_eq!(reduced.report.paired_waypoint_count, 1);
}

#[test]
fn reducer_does_not_pair_rows_from_different_sources_with_same_waypoint_id() {
    let sources = vec![
        source_for_tests("samples-a", vec![row_for_tests(7, PassDirection::Forward, [1.0; 6], [10.0; 6])]),
        source_for_tests("samples-b", vec![row_for_tests(7, PassDirection::Backward, [3.0; 6], [14.0; 6])]),
    ];

    let reduced = reduce_samples(
        ReductionMode::BidirectionalPairMeanV1,
        &sources,
        ReductionOptions { pair_q_error_max_rad: 3.0 },
    ).unwrap();

    assert!(reduced.rows.is_empty());
    assert_eq!(reduced.report.unpaired_waypoint_count, 2);
}

#[test]
fn reducer_does_not_pair_only_by_segment_id() {
    let mut forward = row_for_tests(1, PassDirection::Forward, [1.0; 6], [10.0; 6]);
    forward.segment_id = Some("segment-a".to_string());
    let mut backward = row_for_tests(2, PassDirection::Backward, [1.0; 6], [12.0; 6]);
    backward.segment_id = Some("segment-a".to_string());

    let source = source_for_tests("samples-a", vec![forward, backward]);
    let reduced = reduce_samples(
        ReductionMode::BidirectionalPairMeanV1,
        &[source],
        ReductionOptions { pair_q_error_max_rad: 0.05 },
    ).unwrap();

    assert!(reduced.rows.is_empty());
    assert_eq!(reduced.report.unpaired_waypoint_count, 2);
}

#[test]
fn reducer_skips_pairs_with_large_q_error() {
    let source = source_for_tests("samples-a", vec![
        row_for_tests(7, PassDirection::Forward, [0.0; 6], [10.0; 6]),
        row_for_tests(7, PassDirection::Backward, [0.2; 6], [14.0; 6]),
    ]);

    let reduced = reduce_samples(
        ReductionMode::BidirectionalPairMeanV1,
        &[source],
        ReductionOptions { pair_q_error_max_rad: 0.05 },
    ).unwrap();

    assert!(reduced.rows.is_empty());
    assert_eq!(reduced.report.skipped_pair_q_error_count, 1);
}

#[test]
fn reducer_averages_duplicate_rows_per_direction_before_pair_mean() {
    let source = source_for_tests("samples-a", vec![
        row_for_tests(7, PassDirection::Forward, [1.0; 6], [10.0; 6]),
        row_for_tests(7, PassDirection::Forward, [3.0; 6], [14.0; 6]),
        row_for_tests(7, PassDirection::Backward, [5.0; 6], [18.0; 6]),
        row_for_tests(7, PassDirection::Backward, [7.0; 6], [22.0; 6]),
    ]);

    let reduced = reduce_samples(
        ReductionMode::BidirectionalPairMeanV1,
        &[source],
        ReductionOptions { pair_q_error_max_rad: 4.0 },
    ).unwrap();

    assert_eq!(reduced.rows.len(), 1);
    assert_eq!(reduced.rows[0].q_rad, [4.0; 6]);
    assert_eq!(reduced.rows[0].tau_nm, [16.0; 6]);
    assert_eq!(reduced.report.duplicate_forward_count, 1);
    assert_eq!(reduced.report.duplicate_backward_count, 1);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run:

```bash
cargo test -p piper-cli gravity::sample_reduction -- --nocapture
```

Expected: fail because module/API does not exist.

- [ ] **Step 3: Implement reducer API**

Implement these public structs/functions in `apps/cli/src/gravity/sample_reduction.rs`:

```rust
use std::{collections::BTreeMap, path::PathBuf};
use anyhow::{Result, bail};
use serde::Serialize;

use crate::gravity::{
    artifact::{PassDirection, QuasiStaticSampleRow, SamplesHeader},
    model::JOINT_COUNT,
    profile::config::SampleReductionMode,
};

pub type ReductionMode = SampleReductionMode;

#[derive(Debug, Clone)]
pub struct SourceSampleArtifact {
    pub source_id: String,
    pub path: PathBuf,
    pub header: SamplesHeader,
    pub rows: Vec<QuasiStaticSampleRow>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EffectiveSampleRow {
    pub source_id: String,
    pub group_id: String,
    pub waypoint_id: u64,
    pub q_rad: [f64; JOINT_COUNT],
    pub dq_rad_s: [f64; JOINT_COUNT],
    pub tau_nm: [f64; JOINT_COUNT],
}

#[derive(Debug, Clone, Copy)]
pub struct ReductionOptions {
    pub pair_q_error_max_rad: f64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ReductionReport {
    pub mode: SampleReductionMode,
    pub raw_sample_count: usize,
    pub effective_sample_count: usize,
    pub paired_waypoint_count: usize,
    pub unpaired_waypoint_count: usize,
    pub skipped_pair_q_error_count: usize,
    pub duplicate_forward_count: usize,
    pub duplicate_backward_count: usize,
    pub pair_q_error_p95_rad: [f64; JOINT_COUNT],
    pub pair_q_error_max_rad: [f64; JOINT_COUNT],
    pub direction_torque_delta_p95_nm: [f64; JOINT_COUNT],
    pub direction_torque_delta_max_nm: [f64; JOINT_COUNT],
}

#[derive(Debug, Clone)]
pub struct ReducedSamples {
    pub header: SamplesHeader,
    pub rows: Vec<EffectiveSampleRow>,
    pub report: ReductionReport,
}

pub fn reduce_samples(
    mode: ReductionMode,
    sources: &[SourceSampleArtifact],
    options: ReductionOptions,
) -> Result<ReducedSamples> {
    match mode {
        ReductionMode::RawRows => reduce_raw_rows(sources),
        ReductionMode::BidirectionalPairMeanV1 => reduce_bidirectional_pair_mean(sources, options),
    }
}
```

Implementation rules:

- For `RawRows`, convert every raw row into one `EffectiveSampleRow`.
- For pair-mean, group by `(source_id, waypoint_id)`.
- Populate `EffectiveSampleRow.group_id` with a source-qualified key, for example
  `format!("{}:waypoint:{}", source_id, waypoint_id)`, unless preserving an existing raw-row
  group id in `RawRows` mode. This keeps holdout grouping unambiguous when different artifacts reuse
  waypoint IDs.
- Average duplicate forward rows before pairing; same for backward rows.
- Pair valid iff max joint q error is `<= options.pair_q_error_max_rad`.
- Use percentile helper compatible with existing `ceil(p * n) - 1` behavior.
- Error if `sources` is empty.
- Use first source header as returned header after validating all headers match existing `read_quasi_static_samples` behavior.

- [ ] **Step 4: Confirm module export**

Keep `apps/cli/src/gravity/mod.rs` exporting the module:

```rust
pub mod sample_reduction;
```

- [ ] **Step 5: Run reducer tests**

Run:

```bash
cargo test -p piper-cli gravity::sample_reduction -- --nocapture
```

Expected: reducer tests pass.

- [ ] **Step 6: Commit**

```bash
git add apps/cli/src/gravity/mod.rs apps/cli/src/gravity/sample_reduction.rs
git commit -m "Add gravity sample reduction module"
```

---

### Task 3: Refactor Fit and Eval to Accept Effective Rows

**Files:**
- Modify: `apps/cli/src/gravity/fit.rs`
- Modify: `apps/cli/src/gravity/eval.rs`

- [ ] **Step 1: Write failing fit/eval tests**

In `apps/cli/src/gravity/fit.rs`, add test-only helpers if they do not already exist:

- `synthetic_effective_rows_from_coefficients(coefficients, count) -> Vec<EffectiveSampleRow>`
- `sample_header_for_tests() -> SamplesHeader`

Then add a regression test using effective rows with synthetic direction friction:

```rust
#[test]
fn fitter_recovers_gravity_from_pair_mean_effective_rows_with_direction_friction() {
    let mut truth = vec![vec![0.0; TRIG_V1_FEATURE_COUNT]; JOINT_COUNT];
    truth[0][0] = 1.0;
    truth[1][3] = 2.0;

    let rows = synthetic_effective_rows_from_coefficients(&truth, 400);
    let model = fit_model_from_effective_rows(
        sample_header_for_tests(),
        rows,
        FitOptions { ridge_lambda: 1e-8, holdout_ratio: 0.0, regularize_bias: false },
    ).unwrap();

    assert!((model.model.coefficients_nm[0][0] - 1.0).abs() < 1e-6);
    assert!((model.model.coefficients_nm[1][3] - 2.0).abs() < 1e-6);
}
```

In `apps/cli/src/gravity/eval.rs`, add test-only helpers if they do not already exist:

- `effective_row_for_tests(q_rad, tau_nm) -> EffectiveSampleRow`
- A small model fixture such as `QuasiStaticTorqueModel::for_tests_with_constant_output(...)`,
  or a local helper that builds the same model without changing production APIs.

Then add:

```rust
#[test]
fn eval_effective_rows_reports_delta_metrics() {
    let model = QuasiStaticTorqueModel::for_tests_with_constant_output([1.0; 6]);
    let rows = vec![
        effective_row_for_tests([0.0; 6], [1.0; 6]),
        effective_row_for_tests([0.0; 6], [3.0; 6]),
    ];

    let report = evaluate_model_on_effective_rows(&model, &rows).unwrap();

    assert_eq!(report.sample_count, 2);
    assert_eq!(report.raw_torque_delta_nm, [2.0; 6]);
    assert_eq!(report.compensated_external_torque_delta_nm, [2.0; 6]);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run:

```bash
cargo test -p piper-cli gravity::fit::tests::fitter_recovers_gravity_from_pair_mean_effective_rows_with_direction_friction -- --nocapture
cargo test -p piper-cli gravity::eval::tests::eval_effective_rows_reports_delta_metrics -- --nocapture
```

Expected: fail because effective fit/eval APIs do not exist.

- [ ] **Step 3: Implement fit input abstraction**

In `apps/cli/src/gravity/fit.rs`:

- Import `EffectiveSampleRow`.
- Add `fit_model_from_effective_rows(header, rows, options)`.
- Internally share implementation with raw-row fit by converting raw rows into `EffectiveSampleRow`.
- Keep existing `fit_model_from_rows(...)` signature intact.

Core shape:

```rust
pub(crate) fn fit_model_from_effective_rows(
    header: SamplesHeader,
    rows: Vec<EffectiveSampleRow>,
    options: FitOptions,
) -> Result<QuasiStaticTorqueModel> {
    validate_effective_fit_inputs(&header, &rows, options)?;
    fit_model_from_effective_rows_impl(header, rows, options)
}
```

Update normal equation and residual helpers to use `EffectiveSampleRow`:

```rust
fn group_id_for_effective_row(row: &EffectiveSampleRow) -> String {
    row.group_id.clone()
}

fn waypoint_key_for_effective_row(row: &EffectiveSampleRow) -> (String, u64) {
    (row.source_id.clone(), row.waypoint_id)
}
```

Make all fit-time waypoint counting source-aware:

- The minimum training waypoint check must count unique `(source_id, waypoint_id)` keys, not only
  `waypoint_id`.
- `training_range.waypoint_count` in the output model must also use unique `(source_id,
  waypoint_id)` keys.
- Raw-row compatibility still works because `fit_model_from_rows(...)` converts raw rows with a
  stable synthetic `source_id = "raw"`, preserving the old single-source waypoint behavior.

For raw rows, group id should preserve existing behavior:

```rust
fn effective_row_from_raw(row: &QuasiStaticSampleRow) -> EffectiveSampleRow {
    EffectiveSampleRow {
        source_id: "raw".to_string(),
        group_id: group_id_for_row(row),
        waypoint_id: row.waypoint_id,
        q_rad: row.q_rad,
        dq_rad_s: row.dq_rad_s,
        tau_nm: row.tau_nm,
    }
}
```

- [ ] **Step 4: Implement effective eval**

In `apps/cli/src/gravity/eval.rs`:

- Add `evaluate_model_on_effective_rows(model, rows)`.
- Keep `evaluate_model_on_rows(model, rows)` intact by converting raw rows into effective rows.
- Preserve existing training range and residual semantics.

- [ ] **Step 5: Run fit/eval tests**

Run:

```bash
cargo test -p piper-cli gravity::fit -- --nocapture
cargo test -p piper-cli gravity::eval -- --nocapture
```

Expected: fit/eval tests pass.

- [ ] **Step 6: Commit**

```bash
git add apps/cli/src/gravity/fit.rs apps/cli/src/gravity/eval.rs
git commit -m "Support effective gravity rows in fit and eval"
```

---

### Task 4: Extend Assessment Report and Gate Semantics

**Files:**
- Modify: `apps/cli/src/gravity/profile/assessment.rs`

- [ ] **Step 1: Write failing assessment tests**

Add test-only helpers/builders in `apps/cli/src/gravity/profile/assessment.rs` if they do not
already exist:

- `eval_report_for_tests() -> GravityEvalReport`
- `assessment_counts_for_tests() -> AssessmentCounts`
- `reduction_report_for_tests() -> ReductionReport`
- `QuasiStaticTorqueModel::for_tests_with_constant_output(...)`, or a local model fixture helper.

Then add tests:

```rust
#[test]
fn assessment_gates_on_gravity_ratio_and_keeps_raw_ratio_diagnostic() {
    let gate = StrictGateConfig::default();
    let train_eval = eval_report_for_tests();
    let validation_eval = eval_report_for_tests()
        .with_raw_torque_delta([1.0; JOINT_COUNT])
        .with_compensated_external_torque_delta([0.2; JOINT_COUNT]);
    let raw_validation_eval = eval_report_for_tests()
        .with_raw_torque_delta([1.0; JOINT_COUNT])
        .with_compensated_external_torque_delta([2.0; JOINT_COUNT]);

    let report = build_assessment_report_with_diagnostics(
        &gate,
        assessment_counts_for_tests(),
        reduction_report_for_tests(),
        reduction_report_for_tests(),
        &train_eval,
        &validation_eval,
        Some(&raw_validation_eval),
        &DiagnosticHoldoutMetrics::unavailable(),
        &QuasiStaticTorqueModel::for_tests_with_constant_output([0.0; 6]),
        AssessmentCountMode::EffectivePairs,
    );

    assert!(report.decision.pass);
    assert_eq!(report.derived.gravity_compensated_delta_ratio, [Some(0.2); JOINT_COUNT]);
    assert_eq!(report.derived.raw_row_compensated_delta_ratio, [Some(2.0); JOINT_COUNT]);
    assert_eq!(report.derived.compensated_delta_ratio, [Some(0.2); JOINT_COUNT]);
}

#[test]
fn pair_mean_assessment_uses_effective_pair_gates_not_legacy_raw_count_gates() {
    let mut gate = StrictGateConfig::default();
    gate.min_train_samples = 10_000;
    gate.min_validation_samples = 10_000;
    gate.min_train_waypoints = 10_000;
    gate.min_validation_waypoints = 10_000;
    gate.min_train_effective_pairs = 300;
    gate.min_validation_effective_pairs = 80;

    let report = build_assessment_report_with_diagnostics(
        &gate,
        AssessmentCounts {
            train_samples: 350,
            train_waypoints: 350,
            validation_samples: 90,
            validation_waypoints: 90,
        },
        reduction_report_for_tests(),
        reduction_report_for_tests(),
        &eval_report_for_tests(),
        &eval_report_for_tests(),
        None,
        &DiagnosticHoldoutMetrics::unavailable(),
        &QuasiStaticTorqueModel::for_tests_with_constant_output([0.0; 6]),
        AssessmentCountMode::EffectivePairs,
    );

    assert!(!report
        .decision
        .failed_checks
        .iter()
        .any(|check| matches!(
            check.check.as_str(),
            "train_sample_count"
                | "validation_sample_count"
                | "train_waypoint_count"
                | "validation_waypoint_count"
        )));
    assert!(report.decision.pass);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run:

```bash
cargo test -p piper-cli gravity::profile::assessment::tests::assessment_gates_on_gravity_ratio_and_keeps_raw_ratio_diagnostic -- --nocapture
```

Expected: fail because report fields/API do not exist.

- [ ] **Step 3: Extend report structs**

Add report sections:

```rust
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ReductionReportSection {
    pub mode: SampleReductionMode,
    pub train: ReductionReport,
    pub validation: ReductionReport,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct RawRowsReportSection {
    pub validation: Option<ValidationMetricsSection>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct HysteresisReportSection {
    pub validation_direction_torque_delta_p95_nm: [f64; JOINT_COUNT],
    pub validation_direction_torque_delta_max_nm: [f64; JOINT_COUNT],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssessmentCountMode {
    RawRows,
    EffectivePairs,
}
```

Extend `AssessmentReport`:

```rust
#[serde(skip_serializing_if = "Option::is_none")]
pub reduction: Option<ReductionReportSection>,
#[serde(skip_serializing_if = "Option::is_none")]
pub raw_rows: Option<RawRowsReportSection>,
#[serde(skip_serializing_if = "Option::is_none")]
pub hysteresis: Option<HysteresisReportSection>,
```

Extend `DerivedMetrics`:

```rust
pub gravity_compensated_delta_ratio: [Option<f64>; JOINT_COUNT],
pub gravity_compensated_delta_ratio_meaningful: [bool; JOINT_COUNT],
pub raw_row_compensated_delta_ratio: [Option<f64>; JOINT_COUNT],
// Keep compatibility alias:
pub compensated_delta_ratio: [Option<f64>; JOINT_COUNT],
pub compensated_delta_ratio_meaningful: [bool; JOINT_COUNT],
```

Gate decisions should use `gravity_compensated_delta_ratio`; emit failed check name
`gravity_compensated_delta_ratio`. Keep `compensated_delta_ratio` in report JSON as alias.

Make count gates mode-aware:

- `AssessmentCountMode::RawRows` preserves existing checks:
  - `train_sample_count`
  - `validation_sample_count`
  - `train_waypoint_count`
  - `validation_waypoint_count`
- `AssessmentCountMode::EffectivePairs` skips the legacy raw sample/waypoint checks and checks only:
  - `train_effective_pair_count >= gate.min_train_effective_pairs`
  - `validation_effective_pair_count >= gate.min_validation_effective_pairs`

Update `passes_good_margin(...)` with the same count mode. In pair-mean mode, the good-margin count
check should scale `min_*_effective_pairs`; it must not apply legacy `min_*_samples` or
`min_*_waypoints`.

- [ ] **Step 4: Preserve old builder path**

Keep existing `build_assessment_report(...)` usable by raw mode by delegating to a new
`build_assessment_report_with_diagnostics(...)` with `reduction = None`, `raw_rows = None`, and
`AssessmentCountMode::RawRows`.

Also keep existing `decide_strict_v1(gate, report)` usable for raw-mode tests by delegating to a new
internal `decide_strict_v1_with_count_mode(gate, report, AssessmentCountMode::RawRows)`. Pair-mean
report builders must call the mode-aware decision function with `AssessmentCountMode::EffectivePairs`.

- [ ] **Step 5: Run assessment tests**

Run:

```bash
cargo test -p piper-cli gravity::profile::assessment -- --nocapture
```

Expected: assessment tests pass.

- [ ] **Step 6: Commit**

```bash
git add apps/cli/src/gravity/profile/assessment.rs
git commit -m "Gate gravity assessment on pair-mean metrics"
```

---

### Task 5: Wire Pair-Mean Reduction Into Profile Fit-Assess

**Files:**
- Modify: `apps/cli/src/gravity/profile/workflow.rs`

- [ ] **Step 1: Preserve existing raw-row workflow tests**

Because `ProfileConfig::new(...)` now creates real new profiles with
`sample_reduction = "bidirectional-pair-mean-v1"`, existing workflow tests that use forward-only or
raw-row synthetic samples must explicitly opt in to legacy raw-row mode.

Update the workflow test fixture before adding new pair-mean tests:

- Add a test helper such as `set_sample_reduction_raw_rows()` or make `ProfileFixture::new()` write
  `sample_reduction = "raw-rows"` explicitly for test fixtures that exercise legacy behavior.
- Update existing workflow tests that rely on raw-row fit/eval semantics to use the raw-row fixture
  path.
- Keep at least one config/init test outside the workflow fixture asserting real
  `ProfileConfig::new(...)` still defaults to pair-mean for newly initialized profiles.

Run:

```bash
cargo test -p piper-cli gravity::profile::workflow -- --nocapture
```

Expected: existing workflow tests still pass before pair-mean workflow behavior is added.

- [ ] **Step 2: Write failing pair-mean workflow tests**

Extend `ProfileFixture` test helpers in `apps/cli/src/gravity/profile/workflow.rs` before adding
the tests:

- `set_sample_reduction_pair_mean()` updates the fixture `profile.toml` to opt in to
  `bidirectional-pair-mean-v1` with `pair_q_error_max_rad = 0.05`.
- `register_forward_only_samples(split, id, count)` writes and registers one-direction sample
  artifacts with enough raw rows to pass raw count gates but zero valid pairs.
- `register_bidirectional_pair_samples_with_direction_friction(split, id, pair_count, friction_nm)`
  writes matched forward/backward rows whose pair means follow the synthetic gravity model while raw
  row torque span remains high.
- Use existing fixture artifact registration helpers where possible; these helper names are
  intentionally test-only and must be implemented as part of this task.

Then add tests:

```rust
#[test]
fn pair_mean_fit_assess_records_insufficient_data_when_raw_counts_pass_but_pairs_missing() {
    let fixture = ProfileFixture::new();
    fixture.set_sample_reduction_pair_mean();
    fixture.register_forward_only_samples(Split::Train, "samples-train-0001", 400);
    fixture.register_forward_only_samples(Split::Validation, "samples-validation-0001", 100);

    fit_assess(crate::commands::gravity::GravityProfilePathArgs {
        profile: fixture.profile_dir().to_path_buf(),
    }).unwrap();

    let manifest = Manifest::load(fixture.profile_dir().join("manifest.json")).unwrap();
    assert_eq!(manifest.status, ProfileStatus::InsufficientData);
    assert_eq!(manifest.rounds.last().unwrap().status, ProfileStatus::InsufficientData);
}

#[test]
fn pair_mean_fit_assess_can_pass_when_raw_hysteresis_is_high() {
    let fixture = ProfileFixture::new();
    fixture.set_sample_reduction_pair_mean();
    fixture.register_bidirectional_pair_samples_with_direction_friction(
        Split::Train,
        "samples-train-0001",
        400,
        0.5,
    );
    fixture.register_bidirectional_pair_samples_with_direction_friction(
        Split::Validation,
        "samples-validation-0001",
        120,
        0.5,
    );

    fit_assess(crate::commands::gravity::GravityProfilePathArgs {
        profile: fixture.profile_dir().to_path_buf(),
    }).unwrap();

    let manifest = Manifest::load(fixture.profile_dir().join("manifest.json")).unwrap();
    assert_eq!(manifest.status, ProfileStatus::Passed);
    let report_path = fixture.profile_dir().join(manifest.rounds.last().unwrap().report_path.as_ref().unwrap());
    let report: serde_json::Value = serde_json::from_slice(&std::fs::read(report_path).unwrap()).unwrap();
    assert!(report["derived"]["raw_row_compensated_delta_ratio"].is_array());
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run:

```bash
cargo test -p piper-cli gravity::profile::workflow::tests::pair_mean_fit_assess_records_insufficient_data_when_raw_counts_pass_but_pairs_missing -- --nocapture
cargo test -p piper-cli gravity::profile::workflow::tests::pair_mean_fit_assess_can_pass_when_raw_hysteresis_is_high -- --nocapture
```

Expected: fail because workflow still gates on raw counts and does not reduce samples.

- [ ] **Step 4: Add source-aware loading helpers**

In `apps/cli/src/gravity/profile/workflow.rs`, add helpers:

```rust
fn active_source_sample_artifacts(
    manifest: &Manifest,
    profile_dir: &Path,
    split: Split,
) -> Result<Vec<SourceSampleArtifact>> {
    active_sample_artifacts(manifest, split)
        .into_iter()
        .map(|artifact| {
            let path = profile_dir.join(&artifact.path);
            let loaded = read_quasi_static_samples(&[path.clone()])?;
            Ok(SourceSampleArtifact {
                source_id: artifact.id.clone(),
                path,
                header: loaded.header,
                rows: loaded.rows,
            })
        })
        .collect()
}
```

Use artifact id as source id, not original imported file path.

- [ ] **Step 5: Change `fit_assess` count order for pair-mean**

For `SampleReductionMode::RawRows`, preserve the existing pre-count gate path.

For `SampleReductionMode::BidirectionalPairMeanV1`:

1. Verify active artifacts.
2. Load source-aware train and validation artifacts.
3. Reduce train and validation.
4. Build effective `AssessmentCounts` from reduced reports:

```rust
AssessmentCounts {
    train_samples: train_reduced.rows.len(),
    train_waypoints: train_reduced.report.paired_waypoint_count,
    validation_samples: validation_reduced.rows.len(),
    validation_waypoints: validation_reduced.report.paired_waypoint_count,
}
```

5. Fail as `InsufficientData` if effective pairs are below:

```rust
gate.min_train_effective_pairs
gate.min_validation_effective_pairs
```

Use the same mode when building reports:

- Count-only insufficient-data reports in pair-mean mode must call the count-only report builder with
  `AssessmentCountMode::EffectivePairs`, so failed checks are `train_effective_pair_count` or
  `validation_effective_pair_count`.
- Final pair-mean assessment reports must call `build_assessment_report_with_diagnostics(...)` with
  `AssessmentCountMode::EffectivePairs`, so legacy raw sample/waypoint thresholds do not run after
  the effective pair gate has passed.

- [ ] **Step 6: Reduce diagnostic holdout**

When `diagnostic_split.available`:

- Load diagnostic train/holdout as `SourceSampleArtifact`.
- Reduce both using the same `sample_reduction` and `pair_q_error_max_rad`.
- Fit diagnostic model on effective diagnostic train rows.
- Evaluate on effective diagnostic holdout rows.

- [ ] **Step 7: Build final report with diagnostics**

In pair-mean mode:

- Fit final model on effective train rows.
- Evaluate train/validation on effective rows.
- Evaluate raw validation rows with `evaluate_model_on_rows(...)`.
- Call `build_assessment_report_with_diagnostics(...)` with reduction reports, raw validation eval,
  and `AssessmentCountMode::EffectivePairs`.

- [ ] **Step 8: Run workflow tests**

Run:

```bash
cargo test -p piper-cli gravity::profile::workflow -- --nocapture
```

Expected: workflow tests pass.

- [ ] **Step 9: Commit**

```bash
git add apps/cli/src/gravity/profile/workflow.rs
git commit -m "Use pair-mean reduction in gravity profile fit-assess"
```

---

### Task 6: Update Status and Next UX

**Files:**
- Modify: `apps/cli/src/gravity/profile/workflow.rs`
- Modify if needed: `apps/cli/src/gravity/profile/status.rs`

- [ ] **Step 1: Write failing status and next-action tests**

Add/extend status test in `apps/cli/src/gravity/profile/workflow.rs`:

```rust
#[test]
fn profile_status_lines_include_pair_mean_diagnostics() {
    let fixture = ProfileFixture::new();
    fixture.set_validation_failed_round(
        "round-0001",
        &["samples-train-0001"],
        &["samples-validation-0001"],
        &["path-validation-0001"],
    );
    fixture.write_assessment_report_for_round("round-0001", serde_json::json!({
        "reduction": {
            "mode": "bidirectional-pair-mean-v1",
            "train": {"effective_sample_count": 400, "paired_waypoint_count": 400},
            "validation": {
                "effective_sample_count": 120,
                "paired_waypoint_count": 120,
                "unpaired_waypoint_count": 30,
                "pair_q_error_p95_rad": [0.01,0.02,0.03,0.04,0.05,0.06],
                "direction_torque_delta_p95_nm": [0.1,0.2,0.3,0.4,0.5,0.6]
            }
        },
        "derived": {
            "gravity_compensated_delta_ratio": [0.2,0.3,0.4,0.5,0.6,0.7],
            "raw_row_compensated_delta_ratio": [1.2,1.3,1.4,1.5,1.6,1.7]
        },
        "decision": {"next_action": "ready_to_use_with_caution", "failed_checks": []}
    }));

    let output = status_lines(fixture.profile_dir()).unwrap().join("\n");

    assert!(output.contains("Effective validation samples: 120"));
    assert!(output.contains("Unpaired validation waypoints: 30"));
    assert!(output.contains("Gravity compensated ratio"));
    assert!(output.contains("Raw-row compensated ratio"));
}

#[test]
fn profile_next_uses_latest_assessment_decision_next_action() {
    let fixture = ProfileFixture::new();
    fixture.set_validation_failed_round(
        "round-0001",
        &["samples-train-0001"],
        &["samples-validation-0001"],
        &["path-validation-0001"],
    );
    fixture.write_assessment_report_for_round("round-0001", serde_json::json!({
        "decision": {
            "next_action": "collect bidirectional samples or improve acceptance",
            "failed_checks": [
                {"check": "validation_effective_pair_count", "message": "too few effective pairs"}
            ]
        }
    }));

    let action = next_action_for_profile_dir(fixture.profile_dir()).unwrap();

    assert_eq!(action, "collect bidirectional samples or improve acceptance");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run:

```bash
cargo test -p piper-cli gravity::profile::workflow::tests::profile_status_lines_include_pair_mean_diagnostics -- --nocapture
cargo test -p piper-cli gravity::profile::workflow::tests::profile_next_uses_latest_assessment_decision_next_action -- --nocapture
```

Expected: fail because status does not read new report sections and `profile next` still uses only
the manifest status.

- [ ] **Step 3: Implement status rendering**

Extend `append_assessment_report_summary(...)` to parse:

- `/reduction/validation/effective_sample_count`
- `/reduction/validation/paired_waypoint_count`
- `/reduction/validation/unpaired_waypoint_count`
- `/reduction/validation/pair_q_error_p95_rad`
- `/reduction/validation/direction_torque_delta_p95_nm`
- `/derived/gravity_compensated_delta_ratio`
- `/derived/raw_row_compensated_delta_ratio`

Keep old `compensated_delta_ratio` status output for old reports.

- [ ] **Step 4: Implement report-aware next action**

Add a helper in `apps/cli/src/gravity/profile/workflow.rs`:

```rust
fn latest_assessment_report_value(context: &ProfileContext) -> Result<Option<Value>> {
    let Some(round) = context.manifest.rounds.last() else {
        return Ok(None);
    };
    let Some(report_path) = &round.report_path else {
        return Ok(None);
    };
    let bytes = std::fs::read(context.profile_dir.join(report_path))?;
    Ok(Some(serde_json::from_slice(&bytes)?))
}

fn next_action_for_profile_dir(profile_dir: &Path) -> Result<String> {
    let context = load_profile_context(profile_dir)?;
    if let Some(report) = latest_assessment_report_value(&context)? {
        if let Some(action) = report.pointer("/decision/next_action").and_then(Value::as_str) {
            return Ok(action.to_string());
        }
    }
    Ok(next_action(context.manifest.status).to_string())
}
```

Update `print_next(...)` to print this helper. Keep the existing status-only fallback when no report
or report decision exists.

- [ ] **Step 5: Run status/next tests**

Run:

```bash
cargo test -p piper-cli gravity::profile::workflow::tests::profile_status_lines_include_pair_mean_diagnostics -- --nocapture
cargo test -p piper-cli gravity::profile::workflow::tests::profile_status_lines_include_latest_failed_assessment_checks -- --nocapture
cargo test -p piper-cli gravity::profile::workflow::tests::profile_next_uses_latest_assessment_decision_next_action -- --nocapture
```

Expected: all pass.

- [ ] **Step 6: Commit**

```bash
git add apps/cli/src/gravity/profile/workflow.rs apps/cli/src/gravity/profile/status.rs
git commit -m "Show pair-mean gravity diagnostics in status"
```

---

### Task 7: End-to-End Regression on Existing Profile Data

**Files:**
- No required source changes.
- May modify only if this task exposes a bug.

- [ ] **Step 1: Run targeted test suites**

Run:

```bash
cargo test -p piper-cli gravity::sample_reduction -- --nocapture
cargo test -p piper-cli gravity::fit -- --nocapture
cargo test -p piper-cli gravity::eval -- --nocapture
cargo test -p piper-cli gravity::profile -- --nocapture
```

Expected: all pass.

- [ ] **Step 2: Opt in a copy of the real profile, not the live experiment profile**

Run:

```bash
SRC_PROFILE=/home/viv/projs/piper-sdk-rs/artifacts/gravity/profiles/slave-piper-follower-normal-gripper-d405
TMP_PROFILE="artifacts/gravity/profiles/_tmp-pair-mean-validation-$(date +%Y%m%d-%H%M%S)"
test -d "$SRC_PROFILE"
test ! -e "$TMP_PROFILE"
mkdir -p "$(dirname "$TMP_PROFILE")"
cp -a "$SRC_PROFILE" "$TMP_PROFILE"
```

This uses the live experiment profile only as a read-only source and creates a fresh temporary copy
inside the feature worktree. Do not remove or overwrite an existing temporary profile automatically.

Update the existing `[fit]` table in `"$TMP_PROFILE/profile.toml"` manually or with a safe
TOML-aware edit. Do not append a second `[fit]` table. The final `[fit]` table should include:

```toml
[fit]
sample_reduction = "bidirectional-pair-mean-v1"
pair_q_error_max_rad = 0.05
ridge_lambda = 0.0001
holdout_ratio = 0.2
holdout_group_key = "source_path_id"
```

Do not mutate the live profile until this temporary run is understood.

- [ ] **Step 3: Re-assess temporary profile**

Run:

```bash
cargo run -p piper-cli -- gravity profile fit-assess --profile "$TMP_PROFILE"
cargo run -p piper-cli -- gravity profile status --profile "$TMP_PROFILE"
REPORT=$(jq -r '.rounds[-1].report_path' "$TMP_PROFILE/manifest.json")
jq '{reduction, raw_rows, hysteresis, derived, decision}' "$TMP_PROFILE/$REPORT"
```

Expected:

- Report includes `reduction`, `raw_rows`, `hysteresis`, and explicit gravity/raw ratios.
- `raw_row_compensated_delta_ratio` may remain high and does not fail the decision.
- If decision fails, failure reason is one of:
  - insufficient effective pairs,
  - training range,
  - gravity target residual,
  - gravity compensated ratio.

- [ ] **Step 4: Record findings in commit message or a follow-up note**

If no code changes are needed, do not commit. If a bug is fixed, commit with a focused message:

```bash
git add <fixed files>
git commit -m "Fix pair-mean profile validation issue"
```

---

### Task 8: Final Verification and Integration Prep

**Files:**
- No source changes unless verification finds an issue.

- [ ] **Step 1: Run full required verification**

Run:

```bash
cargo test -p piper-cli gravity::sample_reduction -- --nocapture
cargo test -p piper-cli gravity::fit -- --nocapture
cargo test -p piper-cli gravity::eval -- --nocapture
cargo test -p piper-cli gravity::profile -- --nocapture
cargo fmt --all -- --check
cargo clippy -p piper-cli --all-targets --all-features -- -D warnings
```

Expected: all commands exit 0.

- [ ] **Step 2: Check git state**

Run:

```bash
git status --short
git log --oneline -8
```

Expected: no uncommitted changes; recent commits show Tasks 1-6/7.

- [ ] **Step 3: Summarize implementation**

Prepare a short handoff summary with:

- New reducer mode and config fields.
- How existing profiles stay `raw-rows`.
- How to opt in a profile.
- Manual validation result on `_tmp-pair-mean-validation`.
- Any remaining caveats, especially high hysteresis diagnostics.
