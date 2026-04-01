# Piper HIL Handbook Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a manual hardware-in-the-loop handbook, templates, and focused helper examples for Linux + SocketCAN + one real Piper arm so the repo can execute the approved HIL acceptance flow consistently.

**Architecture:** The implementation is docs-first, but it also adds two narrow `piper-sdk` examples so the handbook has concrete client/driver entry points instead of vague operator instructions. The docs own the normative process and thresholds, while the examples own lightweight runtime checks for Phase 1 and the low-risk Phase 2/3 joint-position path.

**Tech Stack:** Markdown docs, Rust examples in `crates/piper-sdk/examples`, existing `clap`/`piper_sdk` APIs, `cargo test`, `cargo fmt`

---

## File Map

- Create: `docs/v0/piper_hil_handbook.md`
  Purpose: normative manual HIL handbook derived from the approved spec, including phases, thresholds, evidence requirements, and release gates.
- Create: `docs/v0/piper_hil_execution_checklist.md`
  Purpose: operator-facing checklist with copy/paste boxes for Phase 0-4 execution.
- Create: `docs/v0/piper_hil_results_template.md`
  Purpose: run metadata, per-test result blocks, and phase summary templates.
- Create: `crates/piper-sdk/examples/client_monitor_hil_check.rs`
  Purpose: read-only client-side helper for Phase 1 that validates connection timing, first feedback, first monitor snapshot, and warmup-state handling.
- Create: `crates/piper-sdk/examples/hil_joint_position_check.rs`
  Purpose: conservative Phase 2/3 helper that enforces `PositionMode + MotionType::Joint`, `speed_percent <= 10`, and `abs(delta) <= 0.035 rad`.
- Modify: `crates/piper-sdk/examples/README.md`
  Purpose: add the new HIL helpers and clarify that `position_control_demo` remains a teaching demo, not the primary manual HIL command path.

## Task 1: Client Monitor HIL Helper

**Files:**
- Create: `crates/piper-sdk/examples/client_monitor_hil_check.rs`

- [ ] **Step 1: Write the failing test for warmup retry semantics**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use piper_sdk::client::types::{MonitorStateSource, RobotError};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    #[test]
    fn wait_for_monitor_snapshot_retries_warmup_errors_until_ready() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let read = {
            let attempts = Arc::clone(&attempts);
            move || {
                let current = attempts.fetch_add(1, Ordering::SeqCst);
                if current < 2 {
                    Err(RobotError::monitor_state_incomplete(
                        MonitorStateSource::JointPosition,
                        0b001,
                        0b111,
                    ))
                } else {
                    Ok(42_u8)
                }
            }
        };

        let value = wait_for_monitor_snapshot(Duration::from_millis(50), Duration::from_millis(1), read)
            .expect("helper should retry until the snapshot is ready");
        assert_eq!(value, 42);
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
    }
}
```

- [ ] **Step 2: Run the targeted test to verify it fails**

Run: `cargo test -p piper-sdk --example client_monitor_hil_check wait_for_monitor_snapshot_retries_warmup_errors_until_ready -- --exact`
Expected: FAIL with `cannot find function 'wait_for_monitor_snapshot'` or equivalent compile error because the example/helper does not exist yet.

- [ ] **Step 3: Implement the minimal read-only helper example**

```rust
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const INITIAL_MONITOR_SNAPSHOT_TIMEOUT: Duration = Duration::from_millis(200);
const INITIAL_MONITOR_SNAPSHOT_POLL_INTERVAL: Duration = Duration::from_millis(5);
const OBSERVATION_WINDOW: Duration = Duration::from_secs(15 * 60);

fn wait_for_monitor_snapshot<T, Read>(
    timeout: Duration,
    poll_interval: Duration,
    mut read: Read,
) -> piper_sdk::client::types::Result<T>
where
    Read: FnMut() -> piper_sdk::client::types::Result<T>,
{
    let start = Instant::now();
    loop {
        match read() {
            Ok(value) => return Ok(value),
            Err(RobotError::MonitorStateIncomplete { .. } | RobotError::MonitorStateStale { .. }) => {}
            Err(other) => return Err(other),
        }

        if start.elapsed() >= timeout {
            return Err(RobotError::Timeout { timeout_ms: timeout.as_millis() as u64 });
        }

        std::thread::sleep(poll_interval.min(timeout.saturating_sub(start.elapsed())));
    }
}
```

Implementation notes:
- Use the client builder, not the driver builder.
- Record connect timing and first snapshot timing to stdout.
- Read at least `observer.joint_positions()`, `observer.end_pose()`, and one control-state method in the observation loop.
- Keep the observation window configurable from CLI, but default it to the spec’s `15 min`.

- [ ] **Step 4: Run the targeted test to verify it passes**

Run: `cargo test -p piper-sdk --example client_monitor_hil_check wait_for_monitor_snapshot_retries_warmup_errors_until_ready -- --exact`
Expected: PASS

- [ ] **Step 5: Run the full example test target**

Run: `cargo test -p piper-sdk --example client_monitor_hil_check`
Expected: PASS with all unit tests green; no hardware access should be required for the unit tests.

- [ ] **Step 6: Commit**

```bash
git add crates/piper-sdk/examples/client_monitor_hil_check.rs
git commit -m "feat: add client monitor HIL helper example"
```

## Task 2: Safe Joint Position HIL Helper

**Files:**
- Create: `crates/piper-sdk/examples/hil_joint_position_check.rs`

- [ ] **Step 1: Write the failing tests for safety-bound validation**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_args_rejects_excessive_speed() {
        let args = Args {
            interface: "can0".to_string(),
            baud_rate: 1_000_000,
            joint: 1,
            delta_rad: 0.02,
            speed_percent: 11,
            settle_timeout_ms: 10_000,
        };

        let error = validate_args(&args).expect_err("speed > 10 must be rejected");
        assert!(error.contains("speed_percent"));
    }

    #[test]
    fn validate_args_rejects_excessive_delta() {
        let args = Args {
            interface: "can0".to_string(),
            baud_rate: 1_000_000,
            joint: 1,
            delta_rad: 0.04,
            speed_percent: 10,
            settle_timeout_ms: 10_000,
        };

        let error = validate_args(&args).expect_err("delta > 0.035 rad must be rejected");
        assert!(error.contains("delta_rad"));
    }
}
```

- [ ] **Step 2: Run the targeted tests to verify they fail**

Run: `cargo test -p piper-sdk --example hil_joint_position_check validate_args_rejects_excessive_speed -- --exact`
Expected: FAIL with `cannot find function 'validate_args'` or equivalent compile error because the example/helper does not exist yet.

- [ ] **Step 3: Implement the conservative position-mode helper**

```rust
const MAX_SPEED_PERCENT: u8 = 10;
const MAX_DELTA_RAD: f64 = 0.035;
const DEFAULT_SETTLE_TIMEOUT_MS: u64 = 10_000;

fn validate_args(args: &Args) -> Result<(), String> {
    if !(1..=6).contains(&args.joint) {
        return Err("joint must be between 1 and 6".to_string());
    }
    if args.speed_percent > MAX_SPEED_PERCENT {
        return Err("speed_percent must be <= 10 for manual HIL".to_string());
    }
    if args.delta_rad.abs() > MAX_DELTA_RAD {
        return Err("delta_rad must be <= 0.035 rad for manual HIL".to_string());
    }
    Ok(())
}
```

Implementation notes:
- Use `enable_position_mode(PositionModeConfig { speed_percent: args.speed_percent, motion_type: MotionType::Joint, ..Default::default() })`.
- Read current positions with the same monitor-snapshot retry pattern before sending the command.
- Move one joint by `delta_rad`, wait for settle, report observed delta, then command return-to-start.
- Print structured PASS/FAIL lines that the handbook can quote directly.
- Reject MIT mode, Cartesian motion, and out-of-policy values in CLI validation instead of silently clamping.

- [ ] **Step 4: Run the targeted tests to verify they pass**

Run: `cargo test -p piper-sdk --example hil_joint_position_check validate_args_rejects_excessive_speed -- --exact`
Expected: PASS

- [ ] **Step 5: Run the full example test target**

Run: `cargo test -p piper-sdk --example hil_joint_position_check`
Expected: PASS with all unit tests green; no hardware access should be required for the unit tests.

- [ ] **Step 6: Commit**

```bash
git add crates/piper-sdk/examples/hil_joint_position_check.rs
git commit -m "feat: add safe joint position HIL helper"
```

## Task 3: Handbook Document

**Files:**
- Create: `docs/v0/piper_hil_handbook.md`
- Modify: `docs/superpowers/specs/2026-04-01-piper-hil-test-design.md` (reference only; do not edit unless an implementation gap forces a spec correction)

- [ ] **Step 1: Verify the handbook file is absent before writing it**

Run: `test -f docs/v0/piper_hil_handbook.md`
Expected: non-zero exit status because the handbook does not exist yet.

- [ ] **Step 2: Write the handbook skeleton with all required sections**

```markdown
# Piper HIL Handbook

## Scope
## Safety Baseline
## Acceptance Thresholds
## Tooling Entry Points
## Phase 0: Preflight and Safety Baseline
## Phase 1: Connection and Read-Only Observation
## Phase 2: Safe Lifecycle and State Transitions
## Phase 3: Low-Risk Motion Validation
## Phase 4: Fault and Recovery Validation
## Release Gates
## Artifacts to Capture
```

- [ ] **Step 3: Fill in the handbook with the spec’s thresholds and helper example commands**

Required content:
- `<= 5s` connection and reconnect budget
- `<= 200ms` initial client monitor snapshot budget
- `15 min` observation window
- `PositionMode + MotionType::Joint only`
- `speed_percent <= 10`
- `abs(delta) <= 0.035 rad`
- `<= 0.05 rad` return-to-start tolerance
- direct command examples for `client_monitor_hil_check` and `hil_joint_position_check`

- [ ] **Step 4: Verify the handbook contains the required sections and thresholds**

Run: `rg -n "Phase 0:|Phase 1:|Phase 2:|Phase 3:|Phase 4:|Release Gates|0.035 rad|200ms|15 min" docs/v0/piper_hil_handbook.md`
Expected: one or more matches for every required heading/threshold.

- [ ] **Step 5: Commit**

```bash
git add docs/v0/piper_hil_handbook.md
git commit -m "docs: add piper HIL handbook"
```

## Task 4: Execution Checklist and Results Template

**Files:**
- Create: `docs/v0/piper_hil_execution_checklist.md`
- Create: `docs/v0/piper_hil_results_template.md`

- [ ] **Step 1: Verify the template files are absent before writing them**

Run: `test -f docs/v0/piper_hil_execution_checklist.md && test -f docs/v0/piper_hil_results_template.md`
Expected: non-zero exit status because at least one file is still missing.

- [ ] **Step 2: Write the execution checklist**

```markdown
# Piper HIL Execution Checklist

- [ ] Record run metadata
- [ ] Verify `can0` status and bitrate
- [ ] Confirm workspace clear and E-stop reachable
- [ ] Run Phase 1 read-only checks
- [ ] Run Phase 2 lifecycle checks
- [ ] Run Phase 3 low-risk joint-position checks
- [ ] Run Phase 4 fault/recovery checks
- [ ] Collect logs, video, and optional CAN capture
```

- [ ] **Step 3: Write the reusable results template**

```markdown
# Piper HIL Results Template

## Run Metadata

Run ID:
Date:
Operator:
Supervisor:
Git SHA:

## Per-Test Record

Test ID:
Phase:
Expected result:
Actual result:
Pass/Fail:

## Phase Summary

Phase:
Passed:
Failed:
Blocked:
Go/No-Go decision:
```

- [ ] **Step 4: Verify the template headings and placeholders**

Run: `rg -n "Run Metadata|Per-Test Record|Phase Summary|Git SHA|Go/No-Go decision" docs/v0/piper_hil_execution_checklist.md docs/v0/piper_hil_results_template.md`
Expected: matches in both files, with no TODO/TBD placeholders.

- [ ] **Step 5: Commit**

```bash
git add docs/v0/piper_hil_execution_checklist.md docs/v0/piper_hil_results_template.md
git commit -m "docs: add piper HIL checklist and results template"
```

## Task 5: README Integration and End-to-End Verification

**Files:**
- Modify: `crates/piper-sdk/examples/README.md`

- [ ] **Step 1: Write the failing documentation check**

Run: `rg -n "client_monitor_hil_check|hil_joint_position_check" crates/piper-sdk/examples/README.md`
Expected: no matches because the new examples are not documented yet.

- [ ] **Step 2: Update the examples README**

Required edits:
- Add `client_monitor_hil_check` under practical hardware helpers.
- Add `hil_joint_position_check` under practical hardware helpers.
- Clarify that `position_control_demo` remains a teaching example with broad moves and fixed waits.
- Link the handbook path `docs/v0/piper_hil_handbook.md`.

- [ ] **Step 3: Run code/document verification**

Run: `cargo test -p piper-sdk --example client_monitor_hil_check && cargo test -p piper-sdk --example hil_joint_position_check && cargo fmt --all -- --check`
Expected: PASS for both example test targets and formatting check.

- [ ] **Step 4: Run a compile-only examples sweep**

Run: `cargo check -p piper-sdk --examples`
Expected: PASS

- [ ] **Step 5: Verify the docs have no unfinished placeholders**

Run: `rg -n "TODO|TBD|FIXME|XXX" docs/v0/piper_hil_handbook.md docs/v0/piper_hil_execution_checklist.md docs/v0/piper_hil_results_template.md crates/piper-sdk/examples/README.md`
Expected: no matches

- [ ] **Step 6: Commit**

```bash
git add crates/piper-sdk/examples/README.md
git commit -m "docs: wire HIL helpers into examples README"
```
