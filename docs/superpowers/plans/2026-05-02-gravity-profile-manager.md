# Gravity Profile Manager Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the high-level `piper-cli gravity profile` manager described in `docs/superpowers/specs/2026-05-02-gravity-profile-manager-design.md`.

**Architecture:** Add a focused `apps/cli/src/gravity/profile/` module that owns profile config, manifest, artifact registration, status derivation, assessment gates, and workflow orchestration. Keep existing low-level gravity commands as reusable building blocks; expose small `fit`/`eval` helper APIs where profile assessment needs in-process metrics.

**Tech Stack:** Rust, clap, serde/serde_json, toml, sha2, tempfile, existing `piper-cli` gravity modules.

---

## Scope And Constraints

- Work in a dedicated feature worktree so `main` remains usable for hardware experiments.
- Do not add SQLite, dashboards, collision planning, or automatic validation promotion.
- Do not change existing low-level gravity command defaults except where required to expose reusable helpers.
- Keep every task independently buildable and testable.
- Commit after each task.

## Worktree Setup

- [ ] **Step 1: Create a feature worktree**

Run from `/home/viv/projs/piper-sdk-rs`:

```bash
git worktree add ../piper-sdk-rs-gravity-profile -b feature/gravity-profile-manager
cd ../piper-sdk-rs-gravity-profile
```

Expected: new clean worktree on branch `feature/gravity-profile-manager`.

- [ ] **Step 2: Verify baseline**

```bash
git status --short
cargo test -p piper-cli gravity:: -- --nocapture
```

Expected: clean status and existing gravity tests pass.

## File Structure

Create:

- `apps/cli/src/gravity/profile/mod.rs`
  - Module entry point and `run(args)` dispatcher.
- `apps/cli/src/gravity/profile/config.rs`
  - `ProfileConfig`, defaults, validation, canonical hash input.
- `apps/cli/src/gravity/profile/manifest.rs`
  - `Manifest`, artifact/round/event entries, atomic load/save, ID allocation.
- `apps/cli/src/gravity/profile/context.rs`
  - Load profile config + manifest together, classify config changes, append config-change events.
- `apps/cli/src/gravity/profile/artifacts.rs`
  - Analyze path/sample files, compute hashes, validate metadata, copy/import/register artifacts.
- `apps/cli/src/gravity/profile/status.rs`
  - Status enum, readiness derivation, `next` recommendation text.
- `apps/cli/src/gravity/profile/assessment.rs`
  - Assessment report structs, strict-v1 gate checks, grade/decision logic.
- `apps/cli/src/gravity/profile/workflow.rs`
  - Profile actions: init, status, next, record-path, replay-sample, import-samples, fit-assess, promote-validation.
- `apps/cli/src/gravity/profile/holdout.rs`
  - Deterministic diagnostic holdout selection from manifest sample artifact groups.

Modify:

- `apps/cli/src/commands/gravity.rs`
  - Add `Profile` subcommand and typed args.
- `apps/cli/src/gravity/mod.rs`
  - Export `profile`.
- `apps/cli/src/gravity/fit.rs`
  - Expose reusable fitting helpers for caller-provided row sets.
- `apps/cli/src/gravity/eval.rs`
  - Expose reusable evaluation helpers.
- `apps/cli/src/gravity/artifact.rs`
  - Add small metadata/count helper functions if needed.

Do not split unrelated teleop or hardware code.

## Task 1: CLI Skeleton And Module Wiring

**Files:**
- Modify: `apps/cli/src/commands/gravity.rs`
- Modify: `apps/cli/src/gravity/mod.rs`
- Create: `apps/cli/src/gravity/profile/mod.rs`
- Test: `apps/cli/src/commands/gravity.rs`

- [ ] **Step 1: Add failing CLI parse test**

Add tests in `apps/cli/src/commands/gravity.rs`:

```rust
#[test]
fn gravity_profile_init_command_parses_identity_and_target() {
    let cmd = GravityCommand::try_parse_from([
        "gravity",
        "profile",
        "init",
        "--role",
        "slave",
        "--arm-id",
        "piper-left",
        "--target",
        "socketcan:can1",
        "--joint-map",
        "identity",
        "--load-profile",
        "normal-gripper-d405",
    ])
    .expect("gravity profile init should parse");

    match cmd.action {
        GravityAction::Profile(args) => match args.action {
            GravityProfileAction::Init(init) => {
                assert_eq!(init.arm_id, "piper-left");
                assert_eq!(init.target, "socketcan:can1");
                assert_eq!(init.name, None);
                assert_eq!(init.profile, None);
            },
            _ => panic!("expected profile init"),
        },
        _ => panic!("expected profile action"),
    }
}

#[test]
fn gravity_profile_fit_assess_command_parses_profile_path() {
    let cmd = GravityCommand::try_parse_from([
        "gravity",
        "profile",
        "fit-assess",
        "--profile",
        "artifacts/gravity/profiles/slave-piper-left-normal-gripper-d405",
    ])
    .expect("gravity profile fit-assess should parse");

    assert!(matches!(
        cmd.action,
        GravityAction::Profile(GravityProfileArgs {
            action: GravityProfileAction::FitAssess(_),
            ..
        })
    ));
}
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo test -p piper-cli commands::gravity::tests::gravity_profile -- --nocapture
```

Expected: compile failure because `GravityProfileAction` and `GravityAction::Profile` do not exist.

- [ ] **Step 3: Add clap structs and dispatcher**

In `apps/cli/src/commands/gravity.rs`, add:

```rust
#[derive(Debug, Args, Clone)]
pub struct GravityProfileArgs {
    #[command(subcommand)]
    pub action: GravityProfileAction,
}

#[derive(Debug, Subcommand, Clone)]
pub enum GravityProfileAction {
    Init(GravityProfileInitArgs),
    Status(GravityProfilePathArgs),
    Next(GravityProfilePathArgs),
    RecordPath(GravityProfileRecordPathArgs),
    ReplaySample(GravityProfileReplaySampleArgs),
    ImportSamples(GravityProfileImportSamplesArgs),
    FitAssess(GravityProfilePathArgs),
    PromoteValidation(GravityProfilePathArgs),
}

#[derive(Debug, Args, Clone)]
pub struct GravityProfilePathArgs {
    #[arg(long)]
    pub profile: PathBuf,
}

#[derive(Debug, Args, Clone)]
pub struct GravityProfileInitArgs {
    #[arg(long)]
    pub profile: Option<PathBuf>,
    #[arg(long)]
    pub name: Option<String>,
    #[arg(long)]
    pub role: String,
    #[arg(long)]
    pub arm_id: String,
    #[arg(long)]
    pub target: String,
    #[arg(long)]
    pub joint_map: String,
    #[arg(long)]
    pub load_profile: String,
}

#[derive(Debug, Args, Clone)]
pub struct GravityProfileRecordPathArgs {
    #[arg(long)]
    pub profile: PathBuf,
    #[arg(long, value_parser = ["train", "validation"])]
    pub split: String,
    #[arg(long)]
    pub notes: Option<String>,
}

#[derive(Debug, Args, Clone)]
pub struct GravityProfileReplaySampleArgs {
    #[arg(long)]
    pub profile: PathBuf,
    #[arg(long, value_parser = ["train", "validation"])]
    pub split: String,
    #[arg(long, default_value = "latest")]
    pub path: String,
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, Args, Clone)]
pub struct GravityProfileImportSamplesArgs {
    #[arg(long)]
    pub profile: PathBuf,
    #[arg(long, value_parser = ["train", "validation"])]
    pub split: String,
    #[arg(long, required = true)]
    pub samples: Vec<PathBuf>,
}
```

Add `Profile(GravityProfileArgs)` to `GravityAction` and route it:

```rust
GravityAction::Profile(args) => crate::gravity::profile::run(args).await,
```

In `apps/cli/src/gravity/mod.rs`, add:

```rust
pub mod profile;
```

In `apps/cli/src/gravity/profile/mod.rs`, add a temporary dispatcher:

```rust
use anyhow::Result;

pub async fn run(args: crate::commands::gravity::GravityProfileArgs) -> Result<()> {
    match args.action {
        crate::commands::gravity::GravityProfileAction::Init(_) => {
            anyhow::bail!("gravity profile init is not implemented yet")
        },
        crate::commands::gravity::GravityProfileAction::Status(_) => {
            anyhow::bail!("gravity profile status is not implemented yet")
        },
        crate::commands::gravity::GravityProfileAction::Next(_) => {
            anyhow::bail!("gravity profile next is not implemented yet")
        },
        crate::commands::gravity::GravityProfileAction::RecordPath(_) => {
            anyhow::bail!("gravity profile record-path is not implemented yet")
        },
        crate::commands::gravity::GravityProfileAction::ReplaySample(_) => {
            anyhow::bail!("gravity profile replay-sample is not implemented yet")
        },
        crate::commands::gravity::GravityProfileAction::ImportSamples(_) => {
            anyhow::bail!("gravity profile import-samples is not implemented yet")
        },
        crate::commands::gravity::GravityProfileAction::FitAssess(_) => {
            anyhow::bail!("gravity profile fit-assess is not implemented yet")
        },
        crate::commands::gravity::GravityProfileAction::PromoteValidation(_) => {
            anyhow::bail!("gravity profile promote-validation is not implemented yet")
        },
    }
}
```

- [ ] **Step 4: Run tests**

```bash
cargo test -p piper-cli commands::gravity::tests::gravity_profile -- --nocapture
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add apps/cli/src/commands/gravity.rs apps/cli/src/gravity/mod.rs apps/cli/src/gravity/profile/mod.rs
git commit -m "Add gravity profile CLI skeleton"
```

## Task 2: Profile Config, Hashing, And Validation

**Files:**
- Create: `apps/cli/src/gravity/profile/config.rs`
- Modify: `apps/cli/src/gravity/profile/mod.rs`
- Test: `apps/cli/src/gravity/profile/config.rs`

- [ ] **Step 1: Write failing config tests**

Create `apps/cli/src/gravity/profile/config.rs` with tests first:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn config_for_tests() -> ProfileConfig {
        ProfileConfig::new(
            "slave-piper-left-normal-gripper-d405",
            "slave",
            "piper-left",
            "socketcan:can1",
            "identity",
            "normal-gripper-d405",
        )
    }

    #[test]
    fn identity_hash_ignores_target_but_config_hash_does_not() {
        let left = config_for_tests();
        let mut right = config_for_tests();
        right.target = "socketcan:can0".to_string();

        assert_eq!(left.identity_sha256().unwrap(), right.identity_sha256().unwrap());
        assert_ne!(left.config_sha256().unwrap(), right.config_sha256().unwrap());

        right.load_profile = "other-load".to_string();
        assert_ne!(left.identity_sha256().unwrap(), right.identity_sha256().unwrap());
    }

    #[test]
    fn config_defaults_match_spec() {
        let config = config_for_tests();

        assert_eq!(config.torque_convention, crate::gravity::TORQUE_CONVENTION);
        assert_eq!(config.basis, crate::gravity::BASIS_TRIG_V1);
        assert_eq!(config.replay.max_velocity_rad_s, 0.08);
        assert_eq!(config.replay.max_step_rad, 0.02);
        assert_eq!(config.replay.settle_ms, 500);
        assert_eq!(config.replay.sample_ms, 300);
        assert_eq!(config.fit.ridge_lambda, 1e-4);
        assert_eq!(config.fit.holdout_group_key, "source_path_id");
        assert_eq!(config.gate.strict_v1.min_train_samples, 300);
        assert_eq!(config.gate.strict_v1.torque_delta_epsilon_nm, 0.05);
    }

    #[test]
    fn config_rejects_empty_identity_fields() {
        let mut config = config_for_tests();
        config.arm_id.clear();

        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("arm_id"));
    }

    #[test]
    fn profile_hashes_ignore_toml_comments_and_key_order() {
        let first = r#"
            # operator note
            name = "slave-piper-left-normal-gripper-d405"
            role = "slave"
            arm_id = "piper-left"
            target = "socketcan:can1"
            joint_map = "identity"
            load_profile = "normal-gripper-d405"
            torque_convention = "piper-sdk-normalized-nm-v1"
            basis = "trig-v1"

            [fit]
            ridge_lambda = 0.0001
            holdout_ratio = 0.2
            holdout_group_key = "source_path_id"

            [replay]
            sample_ms = 300
            settle_ms = 500
            max_step_rad = 0.02
            max_velocity_rad_s = 0.08
            bidirectional = true

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
        "#;
        let second = r#"
            basis = "trig-v1"
            torque_convention = "piper-sdk-normalized-nm-v1"
            load_profile = "normal-gripper-d405"
            joint_map = "identity"
            target = "socketcan:can1"
            arm_id = "piper-left"
            role = "slave"
            name = "slave-piper-left-normal-gripper-d405"

            [gate.strict_v1]
            torque_delta_epsilon_nm = 0.05
            good_margin_fraction = 0.25
            max_training_range_violations = 0
            max_compensated_delta_ratio = 0.65
            max_validation_train_rms_ratio = 2.0
            max_validation_train_p95_ratio = 2.0
            max_validation_rms_residual_nm = [0.4, 0.7, 0.7, 0.4, 0.3, 0.2]
            max_validation_p95_residual_nm = [0.8, 1.2, 1.2, 0.8, 0.6, 0.4]
            min_validation_waypoints = 40
            min_train_waypoints = 150
            min_validation_samples = 80
            min_train_samples = 300

            [replay]
            bidirectional = true
            max_velocity_rad_s = 0.08
            max_step_rad = 0.02
            settle_ms = 500
            sample_ms = 300

            [fit]
            holdout_group_key = "source_path_id"
            holdout_ratio = 0.2
            ridge_lambda = 0.0001
        "#;

        let first = ProfileConfig::from_toml_str(first).unwrap();
        let second = ProfileConfig::from_toml_str(second).unwrap();

        assert_eq!(first.identity_sha256().unwrap(), second.identity_sha256().unwrap());
        assert_eq!(first.config_sha256().unwrap(), second.config_sha256().unwrap());
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo test -p piper-cli gravity::profile::config -- --nocapture
```

Expected: compile failure because config types do not exist.

- [ ] **Step 3: Implement config types**

Add:

```rust
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ProfileConfig {
    pub name: String,
    pub role: String,
    pub arm_id: String,
    pub target: String,
    pub joint_map: String,
    pub load_profile: String,
    pub torque_convention: String,
    pub basis: String,
    pub replay: ReplayConfig,
    pub fit: FitConfig,
    pub gate: GateConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ReplayConfig {
    pub max_velocity_rad_s: f64,
    pub max_step_rad: f64,
    pub settle_ms: u64,
    pub sample_ms: u64,
    pub bidirectional: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FitConfig {
    pub ridge_lambda: f64,
    pub holdout_ratio: f64,
    pub holdout_group_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct GateConfig {
    pub strict_v1: StrictGateConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct StrictGateConfig {
    pub min_train_samples: usize,
    pub min_validation_samples: usize,
    pub min_train_waypoints: usize,
    pub min_validation_waypoints: usize,
    pub max_validation_p95_residual_nm: [f64; 6],
    pub max_validation_rms_residual_nm: [f64; 6],
    pub max_validation_train_p95_ratio: f64,
    pub max_validation_train_rms_ratio: f64,
    pub max_compensated_delta_ratio: f64,
    pub max_training_range_violations: usize,
    pub good_margin_fraction: f64,
    pub torque_delta_epsilon_nm: f64,
}
```

Implement `Default` for `ReplayConfig`, `FitConfig`, `GateConfig`, and
`StrictGateConfig`. Implement `ProfileConfig::new`, `from_toml_str`,
`validate`, `load`, `save`, `identity_sha256`, `config_sha256`.

For hash canonicalization, recursively convert the typed config to canonical
JSON with sorted object keys, stable scalar encoding, and no whitespace. Do not
hash raw TOML bytes. The hash tests must prove comments, formatting, and key
order do not affect profile hashes.

- [ ] **Step 4: Wire module**

In `apps/cli/src/gravity/profile/mod.rs`:

```rust
pub mod config;
```

- [ ] **Step 5: Run tests**

```bash
cargo test -p piper-cli gravity::profile::config -- --nocapture
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add apps/cli/src/gravity/profile/mod.rs apps/cli/src/gravity/profile/config.rs
git commit -m "Add gravity profile config hashing"
```

## Task 3: Manifest Schema, Atomic Writes, Events, And Status

**Files:**
- Create: `apps/cli/src/gravity/profile/manifest.rs`
- Create: `apps/cli/src/gravity/profile/status.rs`
- Modify: `apps/cli/src/gravity/profile/mod.rs`
- Test: `apps/cli/src/gravity/profile/manifest.rs`
- Test: `apps/cli/src/gravity/profile/status.rs`

- [ ] **Step 1: Write failing manifest tests**

Tests to add in `manifest.rs`:

```rust
#[test]
fn new_manifest_allocates_monotonic_ids() {
    let mut manifest = Manifest::new("profile", "identity-hash", "config-hash");

    assert_eq!(manifest.next_artifact_id("samples", unix_ms_for_tests()), "samples-20260502-001530-0001");
    assert_eq!(manifest.next_artifact_id("path", unix_ms_for_tests()), "path-20260502-001530-0002");
    assert_eq!(manifest.next_round_id(), "round-0001");
    assert_eq!(manifest.next_event_id(), "event-0001");
}

#[test]
fn manifest_atomic_round_trip_preserves_entries() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("manifest.json");
    let mut manifest = Manifest::new("profile", "identity-hash", "config-hash");
    manifest.events.push(EventEntry::profile_config_changed_for_tests("event-0001"));

    manifest.save_atomic(&path).unwrap();
    let loaded = Manifest::load(&path).unwrap();

    assert_eq!(loaded.events.len(), 1);
    assert_eq!(loaded.schema_version, 1);
}
```

Tests to add in `status.rs`:

```rust
#[test]
fn readiness_status_uses_active_sample_splits() {
    let mut manifest = Manifest::new("profile", "identity", "config");
    assert_eq!(derive_readiness_status(&manifest), ProfileStatus::NeedsTrainData);

    manifest.artifacts.push(ArtifactEntry::sample_for_tests("train-1", Split::Train, true, 10, 4));
    assert_eq!(derive_readiness_status(&manifest), ProfileStatus::NeedsValidationData);

    manifest.artifacts.push(ArtifactEntry::sample_for_tests("validation-1", Split::Validation, true, 8, 3));
    assert_eq!(derive_readiness_status(&manifest), ProfileStatus::ReadyToFit);
}

#[test]
fn fit_failed_round_entry_allows_missing_model_and_report_outputs() {
    let round = RoundEntry::fit_failed_for_tests("round-0001", "solver singular matrix");
    let json = serde_json::to_value(&round).unwrap();

    assert_eq!(json["status"], "fit_failed");
    assert!(json["model_path"].is_null());
    assert!(json["model_sha256"].is_null());
    assert!(json["report_path"].is_null());
    assert!(json["report_sha256"].is_null());
    assert_eq!(json["failure"]["kind"], "fit");
    assert!(json["failure"]["message"].as_str().unwrap().contains("solver"));
}
```

- [ ] **Step 2: Run tests to verify failure**

```bash
cargo test -p piper-cli gravity::profile::manifest -- --nocapture
cargo test -p piper-cli gravity::profile::status -- --nocapture
```

Expected: compile failure because manifest/status types do not exist.

- [ ] **Step 3: Implement manifest types**

Define:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProfileStatus {
    NeedsTrainData,
    NeedsValidationData,
    ReadyToFit,
    InsufficientData,
    FitFailed,
    ValidationFailed,
    Passed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Split {
    Train,
    Validation,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub schema_version: u32,
    pub profile_name: String,
    pub profile_identity_sha256: String,
    pub profile_config_sha256: String,
    pub status: ProfileStatus,
    pub next_artifact_seq: u64,
    pub next_event_seq: u64,
    pub next_round_seq: u64,
    pub current_best_model: Option<CurrentBestModel>,
    pub artifacts: Vec<ArtifactEntry>,
    pub rounds: Vec<RoundEntry>,
    pub events: Vec<EventEntry>,
}
```

Include `ArtifactEntry`, `RoundEntry`, `RoundFailure`, `CurrentBestModel`, `EventEntry`,
`PreviousPathEntry`.
`ArtifactEntry` must include `arm_id_source: Option<String>` so legacy imports
without an on-disk arm ID can record `"legacy_import_profile_asserted"`.
`RoundEntry` must support both successful and unsuccessful rounds:

- `status: ProfileStatus`
- `model_path`, `model_sha256`, `report_path`, `report_sha256`: `Option<String>`
- `round_path`, `round_sha256`: `Option<String>`; present when a round provenance file was written
- `failure: Option<RoundFailure>` for `fit_failed` and other failed bookkeeping paths
- sample artifact IDs, validation path artifact IDs, profile hashes, gate config, and `created_at_unix_ms`

Successful `passed` and `validation_failed` rounds have model, report, and round
provenance outputs. `insufficient_data` rounds have no model output but do have a
count-only assessment report and round provenance when writes succeed.
`fit_failed` rounds may have no model or report output; they should still record
failure kind/message and any round provenance file that was successfully written.

Artifact IDs that embed dates must use UTC date/time derived from the Unix
timestamp so IDs are deterministic across operator time zones.

Use `std::fs::OpenOptions::create_new(true)` for temporary files, write pretty JSON, flush,
`sync_all` the temporary file when practical, then `rename`. After rename, fsync the parent
directory on platforms where the standard library makes that practical. Do not silently fall
back to non-atomic manifest rewrites.

- [ ] **Step 4: Implement status derivation**

In `status.rs`, implement:

```rust
pub fn derive_readiness_status(manifest: &Manifest) -> ProfileStatus;
pub fn next_action(status: ProfileStatus) -> &'static str;
pub fn invalidate_round_status_after_sample_pool_change(manifest: &mut Manifest);
```

Only active `kind == "samples"` artifacts count for readiness.

- [ ] **Step 5: Wire modules**

In `profile/mod.rs`:

```rust
pub mod manifest;
pub mod status;
```

- [ ] **Step 6: Run tests**

```bash
cargo test -p piper-cli gravity::profile::manifest -- --nocapture
cargo test -p piper-cli gravity::profile::status -- --nocapture
```

Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add apps/cli/src/gravity/profile/mod.rs apps/cli/src/gravity/profile/manifest.rs apps/cli/src/gravity/profile/status.rs
git commit -m "Add gravity profile manifest state"
```

## Task 4: Profile Load Context And Config-Change Handling

**Files:**
- Create: `apps/cli/src/gravity/profile/context.rs`
- Modify: `apps/cli/src/gravity/profile/mod.rs`
- Test: `apps/cli/src/gravity/profile/context.rs`

- [ ] **Step 1: Write failing config-change tests**

Add:

```rust
#[test]
fn identity_hash_mismatch_rejects_profile_directory() {
    let fixture = ProfileFixture::new();
    fixture.write_config_with_arm_id("different-arm");

    let err = load_profile_context(fixture.profile_dir()).unwrap_err();

    assert!(err.to_string().contains("different profile"));
}

#[test]
fn target_change_preserves_status_and_best_model() {
    let fixture = ProfileFixture::new_passed();
    fixture.write_config_with_target("socketcan:can0");

    let context = load_profile_context(fixture.profile_dir()).unwrap();

    assert_eq!(context.manifest.status, ProfileStatus::Passed);
    assert!(context.manifest.current_best_model.is_some());
    assert!(context.manifest.events.iter().any(|event| event.kind == "profile_config_changed"));
}

#[test]
fn gate_change_invalidates_round_result_status() {
    let fixture = ProfileFixture::new_passed();
    fixture.write_config_with_min_validation_samples(9999);

    let context = load_profile_context(fixture.profile_dir()).unwrap();

    assert_eq!(context.manifest.status, ProfileStatus::ReadyToFit);
    assert!(context.manifest.current_best_model.is_some());
    assert!(context.manifest.events.iter().any(|event| event.kind == "profile_config_changed"));
}
```

- [ ] **Step 2: Run tests to verify failure**

```bash
cargo test -p piper-cli gravity::profile::context -- --nocapture
```

Expected: compile failure.

- [ ] **Step 3: Implement context loader**

Create:

```rust
pub struct ProfileContext {
    pub profile_dir: PathBuf,
    pub config: ProfileConfig,
    pub manifest: Manifest,
}

pub fn load_profile_context(profile_dir: &Path) -> Result<ProfileContext>;
pub fn save_profile_context(context: &ProfileContext) -> Result<()>;
```

`load_profile_context` must:

1. Load `profile.toml` and `manifest.json`.
2. Recompute identity and config hashes.
3. Reject identity hash mismatch.
4. Classify config changes:
   - only `target` or `replay`: preserve status and `current_best_model`
   - `fit` or `gate`: preserve `current_best_model`, recompute readiness status
5. Append a `profile_config_changed` event when config hash changes.
6. Save the manifest atomically when it mutates.

- [ ] **Step 4: Wire module**

In `profile/mod.rs`:

```rust
pub mod context;
```

- [ ] **Step 5: Run tests**

```bash
cargo test -p piper-cli gravity::profile::context -- --nocapture
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add apps/cli/src/gravity/profile/mod.rs apps/cli/src/gravity/profile/context.rs
git commit -m "Add gravity profile context loader"
```

## Task 5: Profile Init, Status, And Next

**Files:**
- Create: `apps/cli/src/gravity/profile/workflow.rs`
- Modify: `apps/cli/src/gravity/profile/mod.rs`
- Test: `apps/cli/src/gravity/profile/workflow.rs`

- [ ] **Step 1: Write failing workflow tests**

Add:

```rust
#[test]
fn init_creates_profile_layout_config_and_manifest() {
    let dir = tempfile::tempdir().unwrap();
    let profile = dir.path().join("slave-piper-left-normal-gripper-d405");

    init_profile(GravityProfileInitArgs {
        profile: Some(profile.clone()),
        name: Some("slave-piper-left-normal-gripper-d405".to_string()),
        role: "slave".to_string(),
        arm_id: "piper-left".to_string(),
        target: "socketcan:can1".to_string(),
        joint_map: "identity".to_string(),
        load_profile: "normal-gripper-d405".to_string(),
    })
    .unwrap();

    assert!(profile.join("profile.toml").exists());
    assert!(profile.join("manifest.json").exists());
    assert!(profile.join("data/train/paths").is_dir());
    assert!(profile.join("data/validation/samples").is_dir());
    assert!(profile.join("models").is_dir());
}

#[test]
fn init_derives_default_name_and_profile_path() {
    let dir = tempfile::tempdir().unwrap();
    let current = std::env::current_dir().unwrap();
    std::env::set_current_dir(dir.path()).unwrap();

    let resolved = resolve_init_profile_location(&GravityProfileInitArgs {
        profile: None,
        name: None,
        role: "slave".to_string(),
        arm_id: "piper-left".to_string(),
        target: "socketcan:can1".to_string(),
        joint_map: "identity".to_string(),
        load_profile: "normal-gripper-d405".to_string(),
    })
    .unwrap();

    std::env::set_current_dir(current).unwrap();

    assert_eq!(resolved.name, "slave-piper-left-normal-gripper-d405");
    assert_eq!(
        resolved.profile_dir,
        dir.path().join("artifacts/gravity/profiles/slave-piper-left-normal-gripper-d405")
    );
}

#[test]
fn init_refuses_existing_profile_manifest() {
    let dir = tempfile::tempdir().unwrap();
    let profile = dir.path().join("profile");
    std::fs::create_dir_all(&profile).unwrap();
    std::fs::write(profile.join("manifest.json"), "{}").unwrap();

    let err = init_profile(init_args_for_tests(profile)).unwrap_err();

    assert!(err.to_string().contains("manifest.json"));
}
```

- [ ] **Step 2: Run test to verify failure**

```bash
cargo test -p piper-cli gravity::profile::workflow::tests::init -- --nocapture
```

Expected: compile failure because workflow functions do not exist.

- [ ] **Step 3: Implement init/status/next**

In `workflow.rs`, implement:

```rust
pub fn init_profile(args: GravityProfileInitArgs) -> Result<()>;
pub fn print_status(args: GravityProfilePathArgs) -> Result<()>;
pub fn print_next(args: GravityProfilePathArgs) -> Result<()>;
```

`init_profile` derives `name = <role>-<arm-id>-<load-profile>` when `--name`
is omitted, and derives
`profile = artifacts/gravity/profiles/<profile-name>` when `--profile` is
omitted.

`init_profile` creates:

```text
profile.toml
manifest.json
data/train/paths
data/train/samples
data/validation/paths
data/validation/samples
data/retired-validation
models
reports
rounds
```

`print_status` should load config + manifest, apply config-change classification, print identity,
current target, distinct active artifact targets when they differ from the current target,
train/validation counts, latest round, best model, status, last failed checks if present.

`print_next` should print `next_action(status)`.

- [ ] **Step 4: Replace temporary dispatcher for first actions**

In `profile/mod.rs`, call workflow functions for `Init`, `Status`, and `Next`.

- [ ] **Step 5: Run tests**

```bash
cargo test -p piper-cli gravity::profile::workflow -- --nocapture
cargo test -p piper-cli commands::gravity::tests::gravity_profile -- --nocapture
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add apps/cli/src/gravity/profile/mod.rs apps/cli/src/gravity/profile/workflow.rs
git commit -m "Implement gravity profile init status next"
```

## Task 6: Artifact Analysis, Import, And Registration

**Files:**
- Create: `apps/cli/src/gravity/profile/artifacts.rs`
- Modify: `apps/cli/src/gravity/profile/mod.rs`
- Modify: `apps/cli/src/gravity/profile/workflow.rs`
- Test: `apps/cli/src/gravity/profile/artifacts.rs`
- Test: `apps/cli/src/gravity/profile/workflow.rs`

- [ ] **Step 1: Write failing artifact tests**

Add tests:

```rust
#[test]
fn analyze_samples_reads_metadata_counts_hash_and_source_path() {
    let dir = tempfile::tempdir().unwrap();
    let samples = write_samples_artifact_for_tests(dir.path(), "sample.samples.jsonl", 12, 4);

    let summary = analyze_samples_artifact(&samples).unwrap();

    assert_eq!(summary.kind, ArtifactKind::Samples);
    assert_eq!(summary.sample_count, Some(12));
    assert_eq!(summary.waypoint_count, 4);
    assert_eq!(summary.role, "slave");
    assert_eq!(summary.source_path_id, None);
    assert_eq!(summary.sha256.len(), 64);
}

#[test]
fn register_samples_rejects_identity_mismatch() {
    let config = ProfileConfig::new(
        "profile",
        "slave",
        "piper-left",
        "socketcan:can1",
        "identity",
        "normal-gripper-d405",
    );
    let mut summary = ArtifactSummary::sample_for_tests();
    summary.role = "master".to_string();

    let err = validate_artifact_summary(&config, &summary).unwrap_err();

    assert!(err.to_string().contains("role"));
}

#[test]
fn legacy_import_without_arm_id_records_profile_asserted_arm_identity() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = ProfileFixture::new_at(dir.path());
    let samples = write_samples_artifact_for_tests(dir.path(), "legacy.samples.jsonl", 12, 4);

    import_samples(GravityProfileImportSamplesArgs {
        profile: fixture.profile_dir().to_path_buf(),
        split: "train".to_string(),
        samples: vec![samples],
    })
    .unwrap();

    let manifest = fixture.load_manifest();
    let artifact = manifest.artifacts.iter().find(|artifact| artifact.kind == "samples").unwrap();
    assert_eq!(artifact.arm_id, "piper-left");
    assert_eq!(artifact.arm_id_source.as_deref(), Some("legacy_import_profile_asserted"));
}

#[test]
fn register_samples_rejects_same_file_content_in_two_active_splits() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = ProfileFixture::new_at(dir.path());
    let samples = write_samples_artifact_for_tests(dir.path(), "same.samples.jsonl", 12, 4);

    import_samples(GravityProfileImportSamplesArgs {
        profile: fixture.profile_dir().to_path_buf(),
        split: "train".to_string(),
        samples: vec![samples.clone()],
    })
    .unwrap();

    let err = import_samples(GravityProfileImportSamplesArgs {
        profile: fixture.profile_dir().to_path_buf(),
        split: "validation".to_string(),
        samples: vec![samples],
    })
    .unwrap_err();

    assert!(err.to_string().contains("already active"));
}

#[test]
fn verify_registered_artifacts_rejects_hash_mismatch_before_consumption() {
    let fixture = ProfileFixture::new_with_train_samples();
    let artifact = fixture
        .load_manifest()
        .artifacts
        .iter()
        .find(|artifact| artifact.kind == "samples" && artifact.active)
        .unwrap()
        .clone();

    std::fs::write(fixture.profile_dir().join(&artifact.path), "tampered\n").unwrap();

    let err = verify_registered_artifacts(fixture.profile_dir(), &[artifact]).unwrap_err();

    assert!(err.to_string().contains("sha256"));
}
```

- [ ] **Step 2: Run tests to verify failure**

```bash
cargo test -p piper-cli gravity::profile::artifacts -- --nocapture
```

Expected: compile failure.

- [ ] **Step 3: Implement artifact analysis helpers**

Functions:

```rust
pub fn file_sha256(path: &Path) -> Result<String>;
pub fn analyze_path_artifact(path: &Path) -> Result<ArtifactSummary>;
pub fn analyze_samples_artifact(path: &Path) -> Result<ArtifactSummary>;
pub fn validate_artifact_summary(config: &ProfileConfig, summary: &ArtifactSummary) -> Result<()>;
pub fn verify_registered_artifacts(profile_dir: &Path, artifacts: &[ArtifactEntry]) -> Result<()>;
pub fn register_imported_samples(profile_dir: &Path, split: Split, samples: &[PathBuf]) -> Result<()>;
```

Use existing `read_path` and `read_quasi_static_samples`. Validate all metadata
present in low-level artifacts: `role`, `joint_map`, `load_profile`, and
`torque_convention`. If a future artifact contains `arm_id`, it must match the
profile. Current legacy low-level artifacts do not contain `arm_id`; v1 imports
them only through an explicit profile directory, sets artifact `arm_id` from the
profile, and records `arm_id_source = "legacy_import_profile_asserted"` in the
manifest entry. Generated profile artifacts use
`arm_id_source = "profile_generated"`.

For imported samples:

- copy into `data/<split>/samples/<artifact-id>.samples.jsonl`
- compute sha after copy
- reject the new active artifact if an active artifact of the same kind in the other split already has the same `sha256`
- `source_path_id = null`
- `active = true`
- invalidate prior round-result status by recomputing readiness
- write event if useful, but do not create a round

`verify_registered_artifacts` recomputes `sha256` for every artifact path passed
to it and fails if any file has changed since registration. Call this helper
before any workflow consumes registered paths or samples (`replay-sample`,
`fit-assess`, and `promote-validation`).

- [ ] **Step 4: Implement workflow import-samples**

In `workflow.rs`, implement `import_samples(args)` and route it from `profile/mod.rs`.

- [ ] **Step 5: Run tests**

```bash
cargo test -p piper-cli gravity::profile::artifacts -- --nocapture
cargo test -p piper-cli gravity::profile::workflow::tests::import -- --nocapture
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add apps/cli/src/gravity/profile/mod.rs apps/cli/src/gravity/profile/artifacts.rs apps/cli/src/gravity/profile/workflow.rs
git commit -m "Add gravity profile artifact import"
```

## Task 7: Profile Record-Path And Replay-Sample Orchestration

**Files:**
- Modify: `apps/cli/src/gravity/profile/workflow.rs`
- Modify: `apps/cli/src/gravity/profile/artifacts.rs`
- Test: `apps/cli/src/gravity/profile/workflow.rs`

- [ ] **Step 1: Write failing dry-run/planning tests**

Add test-only helpers that compute low-level args without touching hardware:

```rust
#[test]
fn profile_record_path_builds_low_level_args_from_config_and_split() {
    let fixture = ProfileFixture::new();

    let planned = plan_record_path(
        fixture.profile_dir(),
        Split::Train,
        Some("operator note".to_string()),
        unix_ms_for_tests(),
    )
    .unwrap();

    assert_eq!(planned.args.role, "slave");
    assert_eq!(planned.args.target.as_deref(), Some("socketcan:can1"));
    assert_eq!(planned.args.joint_map, "identity");
    assert!(planned.args.out.starts_with(fixture.profile_dir().join("data/train/paths")));
}

#[test]
fn profile_replay_sample_uses_latest_path_from_requested_split() {
    let fixture = ProfileFixture::new_with_registered_path(Split::Validation);

    let planned = plan_replay_sample(
        fixture.profile_dir(),
        Split::Validation,
        "latest",
        true,
        unix_ms_for_tests(),
    )
    .unwrap();

    assert_eq!(planned.args.path, fixture.latest_path_file());
    assert!(planned.args.out.starts_with(fixture.profile_dir().join("data/validation/samples")));
    assert!(planned.args.dry_run);
}
```

- [ ] **Step 2: Run tests to verify failure**

```bash
cargo test -p piper-cli gravity::profile::workflow::tests::profile_record_path -- --nocapture
cargo test -p piper-cli gravity::profile::workflow::tests::profile_replay_sample -- --nocapture
```

Expected: compile failure.

- [ ] **Step 3: Implement planning helpers**

Add:

```rust
pub(crate) struct PlannedRecordPath {
    pub artifact_id: String,
    pub args: GravityRecordPathArgs,
}

pub(crate) struct PlannedReplaySample {
    pub artifact_id: String,
    pub source_path_id: String,
    pub args: GravityReplaySampleArgs,
}
```

Use profile config defaults. For `replay-sample`, forbid cross-split replay and resolve `--path latest` to latest active path artifact in that split.

- [ ] **Step 4: Implement actual workflows**

`record_path(args)`:

1. Load profile.
2. Plan output path under `data/<split>/paths`.
3. Call `crate::gravity::record_path::run(low_level_args).await`.
4. Analyze generated path artifact.
5. Register manifest artifact.

`replay_sample(args)`:

1. Load profile.
2. Resolve path artifact.
3. Recompute and verify the resolved path artifact sha256 before replaying it.
4. Call `crate::gravity::replay_sample::run(low_level_args).await`.
5. If `dry_run`, do not register.
6. Analyze generated samples and register with `source_path_id`.

- [ ] **Step 5: Run tests**

```bash
cargo test -p piper-cli gravity::profile::workflow -- --nocapture
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add apps/cli/src/gravity/profile/workflow.rs apps/cli/src/gravity/profile/artifacts.rs
git commit -m "Wire profile record and replay workflows"
```

## Task 8: Expose Fit And Eval Helpers For Profile Assessment

**Files:**
- Modify: `apps/cli/src/gravity/fit.rs`
- Modify: `apps/cli/src/gravity/eval.rs`
- Test: `apps/cli/src/gravity/fit.rs`
- Test: `apps/cli/src/gravity/eval.rs`

- [ ] **Step 1: Write failing helper tests**

In `fit.rs`, add tests for caller-provided final all-train fitting:

```rust
#[test]
fn fit_all_train_policy_records_empty_holdout() {
    let truth = vec![vec![0.0; TRIG_V1_FEATURE_COUNT]; 6];
    let rows = synthetic_rows_from_coefficients(&truth, 260);
    let model = fit_model_from_rows(
        sample_header_for_tests(),
        rows,
        FitOptions {
            ridge_lambda: 1e-8,
            holdout_ratio: 0.0,
            regularize_bias: false,
        },
    )
    .unwrap();

    assert!(model.fit.holdout_group_ids.is_empty());
    assert_eq!(model.sample_count, 260);
}

#[test]
fn fit_all_train_policy_allows_single_group_when_waypoints_are_sufficient() {
    let truth = vec![vec![0.0; TRIG_V1_FEATURE_COUNT]; 6];
    let mut rows = synthetic_rows_from_coefficients(&truth, TRIG_V1_FEATURE_COUNT * 10);
    force_single_segment(&mut rows);

    let model = fit_model_from_rows(
        sample_header_for_tests(),
        rows,
        FitOptions {
            ridge_lambda: 1e-8,
            holdout_ratio: 0.0,
            regularize_bias: false,
        },
    )
    .unwrap();

    assert!(model.fit.holdout_group_ids.is_empty());
}
```

In `eval.rs`, add:

```rust
#[test]
fn evaluate_model_on_rows_is_reusable_by_profile_manager() {
    let model = QuasiStaticTorqueModel::for_tests_with_constant_output([1.0; 6]);
    let rows = vec![sample_row_for_tests([0.0; 6], [1.0; 6])];

    let report = evaluate_model_on_rows(&model, &rows).unwrap();

    assert_eq!(report.sample_count, 1);
    assert_eq!(report.rms_residual_nm, [0.0; 6]);
}
```

- [ ] **Step 2: Run tests to verify failure**

```bash
cargo test -p piper-cli gravity::fit::tests::fit_all_train_policy -- --nocapture
cargo test -p piper-cli gravity::eval::tests::evaluate_model_on_rows -- --nocapture
```

Expected: compile failure because helper names are not public.

- [ ] **Step 3: Refactor helpers**

In `fit.rs`:

- rename private `fit_from_rows` to `pub(crate) fn fit_model_from_rows`
- keep `run(args)` behavior by calling the new helper
- for `holdout_ratio == 0.0`, allow a single training group if sample and waypoint counts are sufficient
- keep current low-level tests updated
- do not introduce profile-specific manifest logic into `fit.rs`

In `eval.rs`:

- expose `pub(crate) fn evaluate_model_on_rows`
- expose `pub(crate) fn validate_model_matches_samples` if needed
- keep `run(args)` behavior unchanged

- [ ] **Step 4: Run tests**

```bash
cargo test -p piper-cli gravity::fit -- --nocapture
cargo test -p piper-cli gravity::eval -- --nocapture
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add apps/cli/src/gravity/fit.rs apps/cli/src/gravity/eval.rs
git commit -m "Expose gravity fit eval helpers"
```

## Task 9: Profile Diagnostic Holdout Selection

**Files:**
- Create: `apps/cli/src/gravity/profile/holdout.rs`
- Modify: `apps/cli/src/gravity/profile/mod.rs`
- Test: `apps/cli/src/gravity/profile/holdout.rs`

- [ ] **Step 1: Write failing holdout tests**

Add:

```rust
#[test]
fn source_path_holdout_is_deterministic_and_records_groups() {
    let artifacts = vec![
        sample_artifact_for_tests("samples-a", Some("path-a"), 50),
        sample_artifact_for_tests("samples-b", Some("path-b"), 50),
        sample_artifact_for_tests("samples-c", Some("path-c"), 50),
    ];

    let split = select_diagnostic_holdout_groups(
        "identity-hash",
        "round-0001",
        "source_path_id",
        0.34,
        &artifacts,
    )
    .unwrap();

    assert_eq!(
        split,
        select_diagnostic_holdout_groups("identity-hash", "round-0001", "source_path_id", 0.34, &artifacts).unwrap()
    );
    assert!(!split.train_group_keys.is_empty());
    assert!(!split.holdout_group_keys.is_empty());
}

#[test]
fn single_group_holdout_is_unavailable_not_error() {
    let artifacts = vec![sample_artifact_for_tests("samples-a", Some("path-a"), 100)];

    let split = select_diagnostic_holdout_groups(
        "identity-hash",
        "round-0001",
        "source_path_id",
        0.2,
        &artifacts,
    )
    .unwrap();

    assert!(!split.available);
    assert!(split.holdout_group_keys.is_empty());
    assert_eq!(split.train_group_keys, ["path-a"]);
}
```

- [ ] **Step 2: Run test to verify failure**

```bash
cargo test -p piper-cli gravity::profile::holdout -- --nocapture
```

Expected: compile failure.

- [ ] **Step 3: Implement holdout selector**

Create:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticHoldoutSplit {
    pub available: bool,
    pub train_group_keys: Vec<String>,
    pub holdout_group_keys: Vec<String>,
    pub train_sample_artifact_ids: Vec<String>,
    pub holdout_sample_artifact_ids: Vec<String>,
}

pub fn select_diagnostic_holdout_groups(
    profile_identity_sha256: &str,
    round_id: &str,
    holdout_group_key: &str,
    holdout_ratio: f64,
    train_sample_artifacts: &[ArtifactEntry],
) -> Result<DiagnosticHoldoutSplit>;
```

For v1, support only `holdout_group_key == "source_path_id"`. Imported samples with `source_path_id = null` use their own artifact ID as the group key. Select holdout groups by SHA-256 of `<profile_identity_sha256>:<round_id>:<group_key>`, sorted lexicographically by hash, until whole groups meet or slightly exceed the requested sample-count ratio.

- [ ] **Step 4: Wire module**

In `profile/mod.rs`:

```rust
pub mod holdout;
```

- [ ] **Step 5: Run tests**

```bash
cargo test -p piper-cli gravity::profile::holdout -- --nocapture
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add apps/cli/src/gravity/profile/mod.rs apps/cli/src/gravity/profile/holdout.rs
git commit -m "Add gravity profile diagnostic holdout"
```

## Task 10: Assessment Report And Strict Gate

**Files:**
- Create: `apps/cli/src/gravity/profile/assessment.rs`
- Modify: `apps/cli/src/gravity/profile/mod.rs`
- Test: `apps/cli/src/gravity/profile/assessment.rs`

- [ ] **Step 1: Write failing gate tests**

Add:

```rust
#[test]
fn strict_gate_passes_usable_report() {
    let gate = StrictGateConfig::default();
    let report = assessment_report_for_tests()
        .with_train_counts(400, 200)
        .with_validation_counts(100, 50)
        .with_validation_p95([0.1; 6])
        .with_validation_rms([0.1; 6])
        .with_compensated_delta_ratio([Some(0.2); 6]);

    let decision = decide_strict_v1(&gate, &report);

    assert!(decision.pass);
    assert!(matches!(decision.grade, AssessmentGrade::Good | AssessmentGrade::Usable));
    assert!(decision.failed_checks.is_empty());
}

#[test]
fn compensated_delta_all_near_zero_is_skipped_not_failed() {
    let gate = StrictGateConfig::default();
    let report = assessment_report_for_tests()
        .with_meaningful_compensated_delta_count(0)
        .with_compensated_delta_ratio([None; 6]);

    let decision = decide_strict_v1(&gate, &report);

    assert!(decision.skipped_checks.iter().any(|check| check.check == "compensated_delta_ratio"));
    assert!(!decision.failed_checks.iter().any(|check| check.check == "compensated_delta_ratio"));
}

#[test]
fn insufficient_counts_grade_bad() {
    let gate = StrictGateConfig::default();
    let report = assessment_report_for_tests().with_train_counts(1, 1);

    let decision = decide_strict_v1(&gate, &report);

    assert!(!decision.pass);
    assert_eq!(decision.grade, AssessmentGrade::Bad);
}

#[test]
fn report_serializes_holdout_availability_range_and_skipped_checks() {
    let gate = StrictGateConfig::default();
    let train_eval = eval_report_for_tests();
    let validation_eval = eval_report_for_tests()
        .with_training_range_violations(2)
        .with_raw_torque_delta([0.0; 6]);
    let holdout = DiagnosticHoldoutMetrics::unavailable();

    let report = build_assessment_report(
        &gate,
        &train_eval,
        &validation_eval,
        &holdout,
        &QuasiStaticTorqueModel::for_tests_with_constant_output([0.0; 6]),
    );
    let json = serde_json::to_value(&report).unwrap();

    assert_eq!(json["fit_internal_holdout"]["available"], false);
    assert_eq!(json["validation"]["training_range_violations"], 2);
    assert_eq!(json["derived"]["meaningful_compensated_delta_joint_count"], 0);
    assert!(json["decision"]["skipped_checks"].as_array().unwrap().iter().any(|check| {
        check["check"] == "compensated_delta_ratio"
    }));
}

#[test]
fn count_only_insufficient_data_report_serializes_without_model_or_eval() {
    let gate = StrictGateConfig::default();

    let report = build_count_only_assessment_report(
        &gate,
        AssessmentCounts {
            train_samples: 3,
            train_waypoints: 1,
            validation_samples: 0,
            validation_waypoints: 0,
        },
        "validation samples below minimum",
    );
    let json = serde_json::to_value(&report).unwrap();

    assert_eq!(json["decision"]["grade"], "bad");
    assert_eq!(json["decision"]["pass"], false);
    assert_eq!(json["train"]["sample_count"], 3);
    assert!(json["validation"]["residual_p95_nm"].is_null());
    assert!(json["model"].is_null());
}
```

- [ ] **Step 2: Run tests to verify failure**

```bash
cargo test -p piper-cli gravity::profile::assessment -- --nocapture
```

Expected: compile failure.

- [ ] **Step 3: Implement report and gate types**

Define serializable structs:

```rust
pub struct AssessmentReport {
    pub train: MetricsSection,
    pub validation: ValidationMetricsSection,
    pub fit_internal_holdout: HoldoutMetricsSection,
    pub derived: DerivedMetrics,
    pub decision: AssessmentDecision,
}

pub struct AssessmentDecision {
    pub pass: bool,
    pub grade: AssessmentGrade,
    pub failed_checks: Vec<AssessmentCheck>,
    pub skipped_checks: Vec<AssessmentCheck>,
    pub next_action: String,
}
```

Implement:

```rust
pub fn build_assessment_report(
    gate: &StrictGateConfig,
    train_eval: &GravityEvalReport,
    validation_eval: &GravityEvalReport,
    diagnostic_holdout: &DiagnosticHoldoutMetrics,
    model: &QuasiStaticTorqueModel,
) -> AssessmentReport;

pub fn build_count_only_assessment_report(
    gate: &StrictGateConfig,
    counts: AssessmentCounts,
    reason: &str,
) -> AssessmentReport;

pub fn decide_strict_v1(gate: &StrictGateConfig, report: &AssessmentReport) -> AssessmentDecision;
```

`AssessmentReport` must support count-only reports by making model/eval-derived
fields nullable. Use this path for `insufficient_data` so the operator still gets
a report and round provenance without requiring a fitted model.

Use `torque_delta_epsilon_nm` to derive `Option<f64>` ratios and
`meaningful_compensated_delta_joint_count`. The report must serialize:

- train residual metrics
- validation residual metrics
- validation raw and compensated torque deltas
- validation training range violations
- diagnostic holdout metrics and `available`
- failed and skipped checks

- [ ] **Step 4: Wire module**

In `profile/mod.rs`:

```rust
pub mod assessment;
```

- [ ] **Step 5: Run tests**

```bash
cargo test -p piper-cli gravity::profile::assessment -- --nocapture
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add apps/cli/src/gravity/profile/mod.rs apps/cli/src/gravity/profile/assessment.rs
git commit -m "Add gravity profile assessment gate"
```

## Task 11: Fit-Assess Workflow

**Files:**
- Modify: `apps/cli/src/gravity/profile/workflow.rs`
- Modify: `apps/cli/src/gravity/profile/manifest.rs`
- Test: `apps/cli/src/gravity/profile/workflow.rs`

- [ ] **Step 1: Write failing fit-assess tests**

Add synthetic fixture tests:

```rust
#[test]
fn fit_assess_writes_round_report_and_best_model_when_gate_passes() {
    let fixture = ProfileFixture::new_with_train_and_validation_samples_that_fit();

    fit_assess(GravityProfilePathArgs {
        profile: fixture.profile_dir().to_path_buf(),
    })
    .unwrap();

    let manifest = fixture.load_manifest();
    assert_eq!(manifest.status, ProfileStatus::Passed);
    assert!(manifest.current_best_model.is_some());
    assert!(fixture.profile_dir().join("models/best.model.toml").exists());
    assert_eq!(manifest.rounds.len(), 1);
    assert!(fixture.profile_dir().join("reports/round-0001.assess.json").exists());
    assert!(fixture.profile_dir().join("rounds/round-0001.json").exists());
}

#[test]
fn fit_assess_sets_insufficient_data_without_model_promotion() {
    let fixture = ProfileFixture::new_with_too_few_samples();

    fit_assess(GravityProfilePathArgs {
        profile: fixture.profile_dir().to_path_buf(),
    })
    .unwrap();

    let manifest = fixture.load_manifest();
    assert_eq!(manifest.status, ProfileStatus::InsufficientData);
    assert!(manifest.current_best_model.is_none());
    assert_eq!(manifest.rounds.len(), 1);
    assert!(fixture.profile_dir().join("reports/round-0001.assess.json").exists());
    assert!(fixture.profile_dir().join("rounds/round-0001.json").exists());
    assert!(manifest.rounds[0].model_path.is_none());
}

#[test]
fn fit_assess_sets_fit_failed_when_solver_or_model_write_fails_after_inputs_pass_count_gate() {
    let fixture = ProfileFixture::new_with_train_and_validation_samples_that_make_fit_fail();

    let err = fit_assess(GravityProfilePathArgs {
        profile: fixture.profile_dir().to_path_buf(),
    })
    .unwrap_err();

    let manifest = fixture.load_manifest();
    assert!(err.to_string().contains("fit"));
    assert_eq!(manifest.status, ProfileStatus::FitFailed);
    assert!(manifest.current_best_model.is_none());
    assert_eq!(manifest.rounds.len(), 1);
}
```

- [ ] **Step 2: Run tests to verify failure**

```bash
cargo test -p piper-cli gravity::profile::workflow::tests::fit_assess -- --nocapture
```

Expected: compile failure or unimplemented action.

- [ ] **Step 3: Implement aggregation helpers**

In `workflow.rs` or a small private helper:

```rust
fn active_sample_paths(manifest: &Manifest, profile_dir: &Path, split: Split) -> Vec<PathBuf>;
fn active_sample_ids(manifest: &Manifest, split: Split) -> Vec<String>;
fn validation_path_ids_for_sample_ids(manifest: &Manifest, validation_sample_ids: &[String]) -> Vec<String>;
```

- [ ] **Step 4: Implement fit-assess**

Flow:

1. Load profile and manifest with config-change handling.
2. Derive counts from active train/validation artifacts.
3. If samples are missing, return hard error as spec says.
4. If counts are below gate minimums, build a count-only assessment report with
   `bad` grade, write `reports/round-N.assess.json`, write `rounds/round-N.json`,
   add a round entry with no model output, set `insufficient_data`, save manifest,
   and return success.
5. Recompute and verify sha256 for all active train/validation sample artifacts before loading them.
6. Otherwise load train samples and validation samples.
7. Use `select_diagnostic_holdout_groups` on active train sample artifacts.
8. If diagnostic holdout is available, fit a diagnostic model on train-group rows and evaluate it on holdout-group rows.
9. Run final all-train fit with `holdout_ratio = 0.0`.
10. If diagnostic fit, final fit, model serialization, report serialization, or required artifact hashing fails after count gates pass, write `rounds/round-N.json` with a `failure` object when possible, add a round entry with optional/missing model and report outputs, set status `fit_failed`, save manifest atomically, then return the original error with context. If even the failure round file cannot be written, still set status `fit_failed` and append a `fit_failed` event before returning the original error with the persistence error attached.
11. Evaluate final model on train and validation rows.
12. Build assessment report and decision, including diagnostic holdout metrics or `available = false`.
13. Write model to `models/round-N.model.toml`.
14. Write report to `reports/round-N.assess.json`.
15. Write provenance to `rounds/round-N.json`, including holdout train/validation group keys.
16. Hash outputs and add manifest round entry.
17. If pass, copy model to `models/best.model.toml` and update `current_best_model`.
18. Set status `passed` or `validation_failed`.
19. Save manifest atomically.

Important: If any write fails after creating files but before manifest save, leave files in place but do not corrupt `manifest.json`.

- [ ] **Step 5: Run tests**

```bash
cargo test -p piper-cli gravity::profile::workflow::tests::fit_assess -- --nocapture
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add apps/cli/src/gravity/profile/workflow.rs apps/cli/src/gravity/profile/manifest.rs
git commit -m "Implement gravity profile fit assess"
```

## Task 12: Promote Validation Workflow

**Files:**
- Modify: `apps/cli/src/gravity/profile/workflow.rs`
- Modify: `apps/cli/src/gravity/profile/manifest.rs`
- Test: `apps/cli/src/gravity/profile/workflow.rs`

- [ ] **Step 1: Write failing promotion tests**

Add:

```rust
#[test]
fn promote_validation_moves_failed_round_validation_artifacts_to_train() {
    let fixture = ProfileFixture::new_with_failed_validation_round();

    promote_validation(GravityProfilePathArgs {
        profile: fixture.profile_dir().to_path_buf(),
    })
    .unwrap();

    let manifest = fixture.load_manifest();
    assert_eq!(manifest.status, ProfileStatus::NeedsValidationData);
    assert!(manifest.events.iter().any(|event| event.kind == "validation_promoted"));

    let promoted = manifest.artifact_by_id("samples-validation-1").unwrap();
    assert_eq!(promoted.split, Split::Train);
    assert!(promoted.path.starts_with("data/train/samples/"));
    assert_eq!(promoted.promoted_from_round_id.as_deref(), Some("round-0001"));
    assert!(!fixture.profile_dir().join("data/validation/samples/samples-validation-1.samples.jsonl").exists());
}

#[test]
fn promote_validation_rejects_when_active_validation_changed() {
    let fixture = ProfileFixture::new_with_failed_validation_round();
    fixture.add_active_validation_sample_after_round();

    let err = promote_validation(GravityProfilePathArgs {
        profile: fixture.profile_dir().to_path_buf(),
    })
    .unwrap_err();

    assert!(err.to_string().contains("fit-assess"));
}
```

- [ ] **Step 2: Run tests to verify failure**

```bash
cargo test -p piper-cli gravity::profile::workflow::tests::promote_validation -- --nocapture
```

Expected: failure.

- [ ] **Step 3: Implement promotion**

Algorithm:

1. Load profile context with `load_profile_context` so profile identity and config
   changes are validated before moving artifacts.
2. Find latest round with `status == validation_failed`.
3. Verify manifest status is `validation_failed`.
4. Verify active validation sample IDs exactly match that round's `validation_sample_artifact_ids`.
5. Recompute and verify sha256 for validation sample artifacts and source path artifacts before moving them.
6. Move each validation sample path to `data/train/samples/<same filename>`.
7. Move source path artifacts listed in `validation_path_artifact_ids` to `data/train/paths/<same filename>`.
8. Mutate artifact entries in place: `split = train`, update `path`, append `previous_paths`, set `promoted_from_round_id`; keep `sha256` unchanged.
9. Add `validation_promoted` event with source/destination paths.
10. Recompute readiness; normally `needs_validation_data`.
11. Save manifest atomically.

- [ ] **Step 4: Run tests**

```bash
cargo test -p piper-cli gravity::profile::workflow::tests::promote_validation -- --nocapture
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add apps/cli/src/gravity/profile/workflow.rs apps/cli/src/gravity/profile/manifest.rs
git commit -m "Implement gravity profile validation promotion"
```

## Task 13: End-To-End CLI Tests Without Hardware

**Files:**
- Modify: `apps/cli/src/gravity/profile/workflow.rs`
- Modify: `apps/cli/src/commands/gravity.rs`
- Test: `apps/cli/src/gravity/profile/workflow.rs`

- [ ] **Step 1: Add integration-style unit test**

Create one test that does:

1. `init_profile`
2. `import_samples` train
3. `import_samples` validation
4. `fit_assess`
5. If validation fails, `promote_validation`

Use synthetic sample files and temp directories only.

Example assertion:

```rust
#[test]
fn profile_manager_import_fit_and_status_cycle_runs_without_hardware() {
    let fixture = ProfileFixture::new();
    init_profile(fixture.init_args()).unwrap();
    import_samples(fixture.import_train_args()).unwrap();
    import_samples(fixture.import_validation_args()).unwrap();

    fit_assess(GravityProfilePathArgs {
        profile: fixture.profile_dir().to_path_buf(),
    })
    .unwrap();

    let manifest = fixture.load_manifest();
    assert!(!manifest.rounds.is_empty());
    assert!(matches!(
        manifest.status,
        ProfileStatus::Passed | ProfileStatus::ValidationFailed | ProfileStatus::InsufficientData
    ));
}
```

- [ ] **Step 2: Run full profile tests**

```bash
cargo test -p piper-cli gravity::profile -- --nocapture
```

Expected: PASS.

- [ ] **Step 3: Run command parser tests**

```bash
cargo test -p piper-cli commands::gravity -- --nocapture
```

Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add apps/cli/src/gravity/profile apps/cli/src/commands/gravity.rs
git commit -m "Add gravity profile end to end tests"
```

## Task 14: Documentation And Operator Examples

**Files:**
- Create: `docs/v0/gravity_profile_manager.md`
- Modify: `docs/superpowers/plans/2026-05-02-gravity-profile-manager.md` if implementation discovers minor command corrections

- [ ] **Step 1: Write operator doc**

Create `docs/v0/gravity_profile_manager.md`:

````markdown
# Gravity Profile Manager

## Create A Profile

```bash
cargo run -p piper-cli -- gravity profile init \
  --profile artifacts/gravity/profiles/slave-piper-left-normal-gripper-d405 \
  --name slave-piper-left-normal-gripper-d405 \
  --role slave \
  --arm-id piper-left \
  --target socketcan:can1 \
  --joint-map identity \
  --load-profile normal-gripper-d405
```

## Collect Train Data

```bash
cargo run -p piper-cli -- gravity profile record-path \
  --profile artifacts/gravity/profiles/slave-piper-left-normal-gripper-d405 \
  --split train

cargo run -p piper-cli -- gravity profile replay-sample \
  --profile artifacts/gravity/profiles/slave-piper-left-normal-gripper-d405 \
  --split train \
  --path latest
```

## Collect Validation Data

```bash
cargo run -p piper-cli -- gravity profile record-path \
  --profile artifacts/gravity/profiles/slave-piper-left-normal-gripper-d405 \
  --split validation

cargo run -p piper-cli -- gravity profile replay-sample \
  --profile artifacts/gravity/profiles/slave-piper-left-normal-gripper-d405 \
  --split validation \
  --path latest
```

## Fit And Assess

```bash
cargo run -p piper-cli -- gravity profile fit-assess \
  --profile artifacts/gravity/profiles/slave-piper-left-normal-gripper-d405
```

## Iterate After Validation Failure

```bash
cargo run -p piper-cli -- gravity profile promote-validation \
  --profile artifacts/gravity/profiles/slave-piper-left-normal-gripper-d405
```
````
```

- [ ] **Step 2: Run doc and formatting checks**

```bash
cargo fmt --all -- --check
git diff --check
```

Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add docs/v0/gravity_profile_manager.md docs/superpowers/plans/2026-05-02-gravity-profile-manager.md
git commit -m "Document gravity profile workflow"
```

## Task 15: Final Verification

**Files:**
- No new files unless previous tasks reveal small fixes.

- [ ] **Step 1: Run focused tests**

```bash
cargo test -p piper-cli gravity::profile -- --nocapture
cargo test -p piper-cli gravity::fit -- --nocapture
cargo test -p piper-cli gravity::eval -- --nocapture
cargo test -p piper-cli commands::gravity -- --nocapture
```

Expected: all pass.

- [ ] **Step 2: Run broader CLI tests**

```bash
cargo test -p piper-cli -- --nocapture
```

Expected: all pass.

- [ ] **Step 3: Run workspace checks**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo build -p piper-cli
```

Expected: all pass.

- [ ] **Step 4: Inspect working tree**

```bash
git status --short
git log --oneline -8
```

Expected: clean working tree except any intentionally uncommitted operator artifacts, and recent commits match task commits.

- [ ] **Step 5: Write final handoff**

Summarize:

- profile commands implemented
- where profile data lives
- which verification commands passed
- any known limitations, especially no collision checking and no automatic validation promotion
