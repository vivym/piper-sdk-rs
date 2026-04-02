# Parking Safe Disable Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a low-speed parking workflow for non-emergency success paths, reuse the existing `park` / `park_blocking()` naming, and make `hil_joint_position_check` park before disable by default without changing `stop`, `disable()`, or `Drop` semantics.

**Architecture:** Keep parking in the `piper-control` / `piper-sdk` experience layer. Standby-entry parking continues to use `park_blocking()`, but with an explicit low-speed park config. Active-session helpers reuse borrowed joint-motion utilities so parking errors do not require a new by-value `Active` API that would conflict with drop-time disable semantics.

**Tech Stack:** Rust, Cargo, existing `piper-control` workflow helpers, `piper-sdk` examples, CLI TOML config, SocketCAN/GS-USB HIL examples.

---

## File Map

**Modify**
- `crates/piper-control/src/profile.rs`
  Purpose: add dedicated parking-speed configuration and expose `park_position_mode_config()`.
- `crates/piper-control/src/workflow.rs`
  Purpose: make standby-entry `park_blocking()` use the dedicated parking config and add a borrowed active-session parking helper for already-enabled joint mode.
- `crates/piper-control/src/lib.rs`
  Purpose: re-export any new parking helper needed by `piper-sdk` examples.
- `apps/cli/src/commands/config.rs`
  Purpose: carry the new parking-speed field through `CliConfig`, defaults, summaries, and tests.
- `apps/cli/src/script.rs`
  Purpose: update manual `ControlProfile` construction to include the new parking-speed field.
- `crates/piper-sdk/Cargo.toml`
  Purpose: add `piper-control` as a dev-dependency for examples/tests if the example reuses its borrowed parking helper.
- `crates/piper-sdk/examples/hil_joint_position_check.rs`
  Purpose: add success-path parking, `--no-park`, and park-orientation selection using the shared parking pose source.
- `docs/v0/piper_hil_operator_runbook.md`
  Purpose: document that successful joint HIL runs now park before disable by default.
- `docs/v0/testing/motion_command_validation_matrix.md`
  Purpose: update the operator matrix to reflect the new success-path cleanup.
- `apps/cli/README.md`
  Purpose: clarify the split between `disable`, standby-entry `park`, and REPL `park`.

**Test Entry Points**
- `cargo test -p piper-control`
- `cargo test -p piper-sdk --example hil_joint_position_check`
- `cargo check -p piper-sdk --example hil_joint_position_check`
- `cargo test -p piper-cli`
- `cargo fmt --all -- --check`

### Task 1: Add Dedicated Parking Speed Config

**Files:**
- Modify: `crates/piper-control/src/profile.rs`
- Modify: `apps/cli/src/commands/config.rs`
- Modify: `apps/cli/src/script.rs`
- Test: `crates/piper-control/src/profile.rs`
- Test: `apps/cli/src/commands/config.rs`

- [ ] **Step 1: Write the failing tests for parking config defaults**

```rust
#[test]
fn park_position_mode_config_defaults_to_slow_speed() {
    let profile = ControlProfile {
        target: ConnectionTarget::AutoStrict,
        orientation: ParkOrientation::Upright,
        rest_pose_override: None,
        park_speed_percent: 5,
        safety: SafetyConfig::default_config(),
        wait: MotionWaitConfig::default(),
    };

    let config = profile.park_position_mode_config();
    assert_eq!(config.speed_percent, 5);
    assert_eq!(config.install_position, profile.orientation.install_position());
}

#[test]
fn cli_default_config_uses_safe_park_speed() {
    let config = CliConfig::default();
    let profile = config.control_profile(None);
    assert_eq!(profile.park_speed_percent, 5);
}
```

- [ ] **Step 2: Run focused tests and verify they fail**

Run:
- `cargo test -p piper-control park_position_mode_config_defaults_to_slow_speed`
- `cargo test -p piper-cli cli_default_config_uses_safe_park_speed`

Expected: FAIL because `park_speed_percent` / `park_position_mode_config()` do not exist yet.

- [ ] **Step 3: Implement the minimal profile/config plumbing**

Implementation notes:
- Add `park_speed_percent: u8` to `ControlProfile`.
- Add `park_position_mode_config()` beside `position_mode_config()`.
- Keep `position_mode_config()` unchanged so ordinary motion paths do not silently slow down.
- Add `park_speed_percent` to `CliConfig::ParkConfig` with default `5`.
- Print the new field in `CliConfig::print_summary()`.
- Update `ScriptExecutor::new()` to fill the new `ControlProfile` field.

- [ ] **Step 4: Run focused tests and the relevant crate suites**

Run:
- `cargo test -p piper-control`
- `cargo test -p piper-cli`

Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/piper-control/src/profile.rs apps/cli/src/commands/config.rs apps/cli/src/script.rs
git commit -m "feat: add dedicated park speed configuration"
```

### Task 2: Reuse Existing Parking Workflow Names And Add Borrowed Active Parking

**Files:**
- Modify: `crates/piper-control/src/workflow.rs`
- Modify: `crates/piper-control/src/lib.rs`
- Test: `crates/piper-control/src/workflow.rs`

- [ ] **Step 1: Write the failing tests for standby-entry and active-session parking helpers**

```rust
#[test]
fn park_blocking_uses_profile_park_pose_and_park_speed_config() {
    let profile = test_profile_with_park_speed(5);
    let config = profile.park_position_mode_config();
    assert_eq!(config.speed_percent, 5);
    assert_eq!(profile.park_pose(), [0.0, 0.0, 0.0, 0.02, 0.5, 0.0]);
}

#[test]
fn active_park_blocking_reuses_existing_joint_motion_helper() {
    let profile = test_profile_with_park_speed(5);
    assert_eq!(profile.park_pose(), ParkOrientation::Upright.default_rest_pose());
}
```

- [ ] **Step 2: Run the focused tests and verify they fail or are missing**

Run: `cargo test -p piper-control park_`

Expected: at least one new test fails before implementation.

- [ ] **Step 3: Implement the minimal workflow changes**

Implementation notes:
- Change `park_blocking()` so it does not call `move_to_joint_target_blocking()` with the general motion config.
- Instead, enable with `profile.park_position_mode_config()`, move to `profile.park_pose()`, then disable.
- Add a borrowed helper:

```rust
pub fn active_park_blocking<Capability>(
    robot: &Piper<Active<PositionMode>, Capability>,
    profile: &ControlProfile,
) -> Result<()>
where
    Capability: MotionCapability
```

- `active_park_blocking()` should only move to `profile.park_pose()` using `active_move_to_joint_target_blocking()`.
- Do not add a by-value `Active -> Standby` helper in this task.
- Re-export `active_park_blocking` from `crates/piper-control/src/lib.rs`.

- [ ] **Step 4: Run the crate tests**

Run: `cargo test -p piper-control`

Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/piper-control/src/workflow.rs crates/piper-control/src/lib.rs
git commit -m "feat: add borrowed active parking workflow"
```

### Task 3: Integrate Success-Path Parking Into `hil_joint_position_check`

**Files:**
- Modify: `crates/piper-sdk/Cargo.toml`
- Modify: `crates/piper-sdk/examples/hil_joint_position_check.rs`
- Test: `crates/piper-sdk/examples/hil_joint_position_check.rs`

- [ ] **Step 1: Write the failing example-local tests for the new flags and planning behavior**

```rust
#[test]
fn validate_args_accepts_no_park() {
    let args = Args {
        interface: "can0".to_string(),
        baud_rate: 1_000_000,
        joint: 1,
        delta_rad: 0.02,
        speed_percent: 5,
        settle_timeout_ms: 10_000,
        no_park: true,
        park_orientation: piper_control::ParkOrientation::Upright,
    };

    validate_args(&args).expect("no-park should be accepted");
}

#[test]
fn park_pose_defaults_to_selected_orientation() {
    assert_eq!(
        piper_control::ParkOrientation::Left.default_rest_pose(),
        [1.71, 2.96, -2.65, 1.41, -0.081, -0.190]
    );
}
```

- [ ] **Step 2: Run the focused tests and verify they fail**

Run: `cargo test -p piper-sdk --example hil_joint_position_check`

Expected: FAIL because the new args and success-path parking do not exist yet.

- [ ] **Step 3: Implement the minimal HIL helper integration**

Implementation notes:
- Add `piper-control` as a `dev-dependency` in `crates/piper-sdk/Cargo.toml` if the example imports `ParkOrientation`, `MotionWaitConfig`, or `active_park_blocking`.
- Add `--no-park` to skip parking during debugging.
- Add `--park-orientation <upright|left|right>` with default `upright`.
- Keep the current move and return-to-initial checks exactly as they are.
- After the successful return step:
  - if `--no-park` is false, build a local `ControlProfile` or equivalent shared parking context
  - call `active_park_blocking(&robot, &profile)` while still in the already-enabled joint mode
  - then call `robot.disable(DisableConfig::default())`
- On any failure before parking completes, do not add a second automatic disable path beyond existing ownership semantics.
- Update final `[PASS]` messages so operators can see whether the helper parked before disable or used `--no-park`.

- [ ] **Step 4: Run example tests and compile checks**

Run:
- `cargo test -p piper-sdk --example hil_joint_position_check`
- `cargo check -p piper-sdk --example hil_joint_position_check`

Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/piper-sdk/Cargo.toml crates/piper-sdk/examples/hil_joint_position_check.rs
git commit -m "feat: park before disable in joint HIL helper"
```

### Task 4: Clarify Operator Documentation

**Files:**
- Modify: `docs/v0/piper_hil_operator_runbook.md`
- Modify: `docs/v0/testing/motion_command_validation_matrix.md`
- Modify: `apps/cli/README.md`

- [ ] **Step 1: Write the documentation changes**

Required edits:
- In the HIL runbook, document that successful `hil_joint_position_check` runs now:
  - move
  - return to the initial snapshot
  - park to the configured rest pose by default
  - disable only after parking
- In the validation matrix, add the new `--no-park` / `--park-orientation` behavior and update the expected success-path cleanup.
- In the CLI README, clarify:
  - `disable` is always a raw disable
  - one-shot CLI `park` / script `Park` use standby-entry park-and-disable
  - REPL `park` still only moves to the park pose in Phase 1

- [ ] **Step 2: Run targeted verification**

Run:
- `cargo fmt --all -- --check`
- `cargo check -p piper-cli`

Expected: PASS

- [ ] **Step 3: Commit**

```bash
git add docs/v0/piper_hil_operator_runbook.md docs/v0/testing/motion_command_validation_matrix.md apps/cli/README.md
git commit -m "docs: clarify parking and disable semantics"
```

### Task 5: Final Verification

**Files:**
- Verify only; no new files.

- [ ] **Step 1: Run the full non-HIL verification set**

Run:
- `cargo test -p piper-control`
- `cargo test -p piper-sdk --example hil_joint_position_check`
- `cargo test -p piper-cli`
- `cargo check --all-targets`
- `cargo fmt --all -- --check`

Expected: PASS

- [ ] **Step 2: Run the manual hardware smoke check**

Run:

```bash
cargo run -p piper-sdk --example hil_joint_position_check -- --interface can0 --joint 1 --delta-rad 0.02 --speed-percent 5
```

Expected:
- move step passes
- return step passes
- helper parks before disable unless `--no-park` was set
- robot ends in confirmed `Standby`

- [ ] **Step 3: Record any residual risk**

Document:
- whether the default `upright` park orientation is acceptable on the tested hardware
- whether REPL `park` semantics should be changed in a future follow-up
- whether additional helpers should adopt the same success-path parking pattern

- [ ] **Step 4: Commit any last verification-only doc touchups if needed**

```bash
git status
```
