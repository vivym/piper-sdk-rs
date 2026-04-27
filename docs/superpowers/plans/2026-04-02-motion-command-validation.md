# Motion Command Validation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a complete test matrix for motion-command correctness across PositionMode, MitMode, ReplayMode, and gripper control, with layered coverage from protocol/state-machine tests up through hardware-in-the-loop validation.

**Architecture:** Keep fast feedback in lower layers and reserve real hardware only for behavior that cannot be proven with mocks. Treat command correctness as five separate properties: frame encoding, mode gating, state confirmation, observed motion result, and safe shutdown/cancel semantics. Add one small HIL helper per motion family instead of one oversized “do everything” test binary.

**Tech Stack:** Rust, Cargo tests, existing `piper-client` state machine tests, `piper-sdk` examples, SocketCAN/GS-USB HIL execution.

---

## File Map

**Modify**
- `crates/piper-client/src/state/machine.rs`
  Purpose: extend mode-gating, confirmation, and drop-safety unit tests for each motion family.
- `crates/piper-client/src/raw_commander.rs`
  Purpose: add command-dispatch tests that assert the expected frame families are emitted for joint/cartesian/linear/circular/gripper operations.
- `crates/piper-sdk/tests/high_level_integration_v2.rs`
  Purpose: replace loose “is_ok() || is_err()” checks with targeted API contract tests where mocks can support it.
- `docs/v0/position_control_user_guide.md`
  Purpose: document which command API maps to which `MotionType`, and which validation helper/example should be used to test it.

**Create**
- `crates/piper-sdk/examples/hil_cartesian_pose_check.rs`
  Purpose: HIL helper for `MotionType::Cartesian` using end-pose convergence checks.
- `crates/piper-sdk/examples/hil_linear_motion_check.rs`
  Purpose: HIL helper for `MotionType::Linear` using end-pose convergence checks and progress detection.
- `crates/piper-sdk/examples/hil_circular_motion_check.rs`
  Purpose: HIL helper for `MotionType::Circular` validating via-point/target command flow and final pose convergence.
- `crates/piper-sdk/examples/hil_mit_hold_check.rs`
  Purpose: HIL helper for `MitMode` to verify enable/command/hold/disable semantics without requiring aggressive torque motion.
- `crates/piper-sdk/examples/hil_replay_mode_check.rs`
  Purpose: HIL helper that enters replay mode, replays a known-safe recording, and verifies mode restoration.
- `crates/piper-sdk/examples/hil_gripper_check.rs`
  Purpose: HIL helper for gripper open/close/hold feedback.
- `docs/v0/testing/motion_command_validation_matrix.md`
  Purpose: operator-facing matrix of what to run, expected pass criteria, and residual risk per mode.

**Test Entry Points**
- `cargo test -p piper-client`
- `cargo test -p piper-sdk --test high_level_integration_v2`
- `cargo run -p piper-sdk --example hil_joint_position_check -- ...`
- `cargo run -p piper-sdk --example hil_cartesian_pose_check -- ...`
- `cargo run -p piper-sdk --example hil_linear_motion_check -- ...`
- `cargo run -p piper-sdk --example hil_circular_motion_check -- ...`
- `cargo run -p piper-sdk --example hil_mit_hold_check -- ...`
- `cargo run -p piper-sdk --example hil_replay_mode_check -- ...`
- `cargo run -p piper-sdk --example hil_gripper_check -- ...`

### Task 1: Lock Down Motion-Type API Contracts

**Files:**
- Modify: `crates/piper-client/src/state/machine.rs`
- Test: `crates/piper-client/src/state/machine.rs`

- [ ] **Step 1: Write failing tests for legal/illegal command combinations**

```rust
#[test]
fn position_mode_joint_rejects_cartesian_pose_commands() {
    let robot = build_active_position_piper(MotionType::Joint, IdleRxAdapter::new());
    let error = robot
        .command_cartesian_pose(Position3D::new(0.3, 0.0, 0.2), EulerAngles::new(0.0, 180.0, 0.0))
        .expect_err("Joint mode must reject cartesian commands");
    assert!(matches!(error, RobotError::ConfigError(_)));
}

#[test]
fn position_mode_linear_accepts_linear_commands() {
    let robot = build_active_position_piper(MotionType::Linear, IdleRxAdapter::new());
    let _ = robot.move_linear(
        Position3D::new(0.3, 0.0, 0.2),
        EulerAngles::new(0.0, 180.0, 0.0),
    );
}
```

- [ ] **Step 2: Run the focused tests and verify they fail for missing coverage**

Run: `cargo test -p piper-client motion_type_`

Expected: at least one new test fails before implementation.

- [ ] **Step 3: Extend existing state-machine coverage instead of duplicating it**

Implementation notes:
- Reuse existing builder helpers near the `socketcan_without_control_mode_echo_*` tests.
- Reuse and extend the existing runtime guard coverage near `enable_position_mode_rejects_continuous_position_velocity_without_sending_any_frame` and `position_mode_runtime_motion_type_guard_rejects_mismatched_helpers_without_sending`.
- Add one test per `MotionType`:
  - `Joint`: allows `send_position_command`, rejects `command_cartesian_pose`, `move_linear`, `move_circular`
  - `Cartesian`: allows `command_cartesian_pose`, rejects joint/linear/circular
  - `Linear`: allows `move_linear`, rejects joint/cartesian/circular
  - `Circular`: allows `move_circular`, rejects joint/cartesian/linear
  - `ContinuousPositionVelocity`: rejects entry or all public send methods until implementation exists

- [ ] **Step 4: Run the focused tests and the full client test suite**

Run:
- `cargo test -p piper-client motion_type_`
- `cargo test -p piper-client`

Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/piper-client/src/state/machine.rs
git commit -m "test: cover motion type command contracts"
```

### Task 2: Assert Command Frame Families, Not Just API Return Values

**Files:**
- Modify: `crates/piper-client/src/raw_commander.rs`
- Modify: `crates/piper-sdk/tests/high_level_integration_v2.rs`
- Test: `crates/piper-client/src/raw_commander.rs`

- [ ] **Step 1: Write failing raw commander tests for emitted frame IDs**

```rust
#[test]
fn joint_position_command_emits_joint_position_frame_batch() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let raw = build_raw_commander_with_recording_tx(sent.clone());
    raw.send_position_command_batch(&JointArray::from([Rad(0.0); 6]), Duration::from_millis(20))
        .expect("joint position command should encode");
    let ids: Vec<u32> = sent.lock().unwrap().iter().map(|frame| frame.raw_id()).collect();
    assert_eq!(ids, vec![ID_JOINT_CONTROL_12, ID_JOINT_CONTROL_34, ID_JOINT_CONTROL_56]);
}
```

- [ ] **Step 2: Run the focused tests and verify failure**

Run: `cargo test -p piper-client raw_commander`

Expected: FAIL until assertions/helpers exist.

- [ ] **Step 3: Add frame-family tests for each command surface**

Coverage:
- Joint position batch sends `0x155-0x157`
- Cartesian and linear send end-pose command family `0x152-0x154`
- Circular sends end-pose family plus circular auxiliary command `0x158`
- Gripper sends gripper control frame
- MIT command path sends the expected MIT control family

Also tighten `high_level_integration_v2.rs` so it stops accepting meaningless `is_ok() || is_err()` checks when a mock can inspect sent frames.

- [ ] **Step 4: Run tests**

Run:
- `cargo test -p piper-client raw_commander`
- `cargo test -p piper-sdk --test high_level_integration_v2`

Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/piper-client/src/raw_commander.rs crates/piper-sdk/tests/high_level_integration_v2.rs
git commit -m "test: assert emitted frame families for motion commands"
```

### Task 3: Extend Mode-Transition Confirmation Coverage

**Files:**
- Modify: `crates/piper-client/src/state/machine.rs`
- Test: `crates/piper-client/src/state/machine.rs`

- [ ] **Step 1: Write failing tests for enable/disable confirmation across modes**

```rust
#[test]
fn enable_position_mode_linear_requires_matching_robot_status_move_mode() {
    let standby = build_standby_piper_with_feedback(/* enabled + wrong move mode */);
    let error = standby.enable_position_mode(PositionModeConfig {
        motion_type: MotionType::Linear,
        ..Default::default()
    }).expect_err("wrong 0x2A1 move mode must reject Linear mode");
    assert!(matches!(error, RobotError::Timeout { .. } | RobotError::ConfigError(_)));
}
```

- [ ] **Step 2: Run focused tests**

Run: `cargo test -p piper-client enable_`

Expected: FAIL for the new unimplemented cases.

- [ ] **Step 3: Add missing tests only, not behavior changes unless proven necessary**

Coverage:
- `enable_position_mode` success for `Joint`, `Cartesian`, `Linear`, `Circular`
- stale enabled feedback rejected
- fresh mismatched move mode rejected
- `disable()` returns to confirmed standby
- `ReplayMode` drop restores driver mode without disable side effects

- [ ] **Step 4: Run tests**

Run: `cargo test -p piper-client`

Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/piper-client/src/state/machine.rs
git commit -m "test: expand mode transition confirmation coverage"
```

### Task 4: Add Cartesian HIL Validation Helper

**Files:**
- Create: `crates/piper-sdk/examples/hil_cartesian_pose_check.rs`
- Test: `crates/piper-sdk/examples/hil_cartesian_pose_check.rs`

- [ ] **Step 1: Write argument validation tests first**

```rust
#[test]
fn validate_args_rejects_non_positive_speed_percent() {
    let args = Args { speed_percent: 0, ..test_args() };
    let error = validate_args(&args).expect_err("speed 0 must be rejected");
    assert!(error.contains("speed_percent"));
}
```

- [ ] **Step 2: Run the example-local unit tests**

Run: `cargo test -p piper-sdk --example hil_cartesian_pose_check`

Expected: FAIL until helper exists.

- [ ] **Step 3: Implement the smallest safe HIL helper**

Behavior:
- require confirmed `Standby`
- enable `PositionModeConfig { motion_type: MotionType::Cartesian, speed_percent <= 10 }`
- capture initial end pose
- command a small safe Cartesian delta
- wait for end-pose progress and final error tolerance
- command return
- print `[PASS]/[FAIL]` lines matching the style of `hil_joint_position_check`

- [ ] **Step 4: Run non-hardware verification**

Run:
- `cargo test -p piper-sdk --example hil_cartesian_pose_check`
- `cargo check -p piper-sdk --example hil_cartesian_pose_check`

Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/piper-sdk/examples/hil_cartesian_pose_check.rs
git commit -m "feat: add cartesian pose HIL validation helper"
```

### Task 5: Add Linear and Circular HIL Helpers

**Files:**
- Create: `crates/piper-sdk/examples/hil_linear_motion_check.rs`
- Create: `crates/piper-sdk/examples/hil_circular_motion_check.rs`
- Test: `crates/piper-sdk/examples/hil_linear_motion_check.rs`
- Test: `crates/piper-sdk/examples/hil_circular_motion_check.rs`

- [ ] **Step 1: Write unit tests for argument validation and trajectory criteria**

```rust
#[test]
fn validate_args_rejects_excessive_linear_delta() {
    let args = Args { delta_m: 0.05, ..test_args() };
    assert!(validate_args(&args).is_err());
}
```

- [ ] **Step 2: Run the example-local tests**

Run:
- `cargo test -p piper-sdk --example hil_linear_motion_check`
- `cargo test -p piper-sdk --example hil_circular_motion_check`

Expected: FAIL until implementation exists.

- [ ] **Step 3: Implement the helpers**

Requirements:
- Linear helper uses `MotionType::Linear` + `move_linear()`
- Circular helper uses `MotionType::Circular` + `move_circular()`
- both require:
  - safe small deltas
  - progress threshold before success
  - final end-pose tolerance
  - explicit return or safe stop path
  - consistent `[PASS]/[FAIL]` output
- Linear-specific acceptance:
  - sample intermediate end-pose observations while moving
  - compute orthogonal deviation from the start-to-target line segment
  - fail if deviation exceeds a conservative bound derived from the commanded delta
- Circular-specific acceptance:
  - sample intermediate end-pose observations while moving
  - require at least one sampled pose to enter a via-point neighborhood before final convergence
  - require final pose tolerance separately from via-point observation
- Do not treat “endpoint reached” as sufficient proof for Linear or Circular mode correctness.

- [ ] **Step 4: Run non-hardware verification**

Run:
- `cargo test -p piper-sdk --example hil_linear_motion_check`
- `cargo test -p piper-sdk --example hil_circular_motion_check`
- `cargo check -p piper-sdk --example hil_linear_motion_check`
- `cargo check -p piper-sdk --example hil_circular_motion_check`

Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/piper-sdk/examples/hil_linear_motion_check.rs crates/piper-sdk/examples/hil_circular_motion_check.rs
git commit -m "feat: add linear and circular HIL validation helpers"
```

### Task 6: Add MIT, Replay, and Gripper Validation Helpers

**Files:**
- Create: `crates/piper-sdk/examples/hil_mit_hold_check.rs`
- Create: `crates/piper-sdk/examples/hil_replay_mode_check.rs`
- Create: `crates/piper-sdk/examples/hil_gripper_check.rs`
- Test: `crates/piper-sdk/examples/hil_mit_hold_check.rs`
- Test: `crates/piper-sdk/examples/hil_replay_mode_check.rs`
- Test: `crates/piper-sdk/examples/hil_gripper_check.rs`

- [ ] **Step 1: Write validation tests for the helper CLI arguments**

```rust
#[test]
fn validate_args_rejects_invalid_replay_speed() {
    let args = Args { speed: 0.0, ..test_args() };
    assert!(validate_args(&args).is_err());
}
```

- [ ] **Step 2: Run the example-local test targets**

Run:
- `cargo test -p piper-sdk --example hil_mit_hold_check`
- `cargo test -p piper-sdk --example hil_replay_mode_check`
- `cargo test -p piper-sdk --example hil_gripper_check`

Expected: FAIL until files exist.

- [ ] **Step 3: Implement the helpers with conservative success criteria**

Definitions:
- `hil_mit_hold_check`: enter MIT mode, send a bounded neutral/hold command stream for a short window, verify mode/enable stability, then disable
- `hil_replay_mode_check`: verify confirmed standby precondition, enter replay mode, replay a known-safe recording, confirm return to standby and restored driver mode
- `hil_gripper_check`: command open/close at safe effort, observe travel/status changes, verify return/stop conditions

MIT-specific acceptance must cover two layers:
- low-layer command semantics:
  - add raw/frame assertions in `piper-client` tests proving `command_torques()` encodes the expected MIT control fields and CAN IDs
- HIL-level bounded response:
  - use a conservative non-zero command on one joint only
  - require a small but observable change in the relevant monitored state, or a stable hold/current response that differs from the neutral baseline
  - fail if the helper only proves “no fault happened”

- [ ] **Step 4: Run non-hardware verification**

Run:
- `cargo test -p piper-sdk --example hil_mit_hold_check`
- `cargo test -p piper-sdk --example hil_replay_mode_check`
- `cargo test -p piper-sdk --example hil_gripper_check`
- `cargo check -p piper-sdk --example hil_mit_hold_check`
- `cargo check -p piper-sdk --example hil_replay_mode_check`
- `cargo check -p piper-sdk --example hil_gripper_check`

Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/piper-sdk/examples/hil_mit_hold_check.rs crates/piper-sdk/examples/hil_replay_mode_check.rs crates/piper-sdk/examples/hil_gripper_check.rs
git commit -m "feat: add mit replay and gripper validation helpers"
```

### Task 7: Add a Repeatable Soak Runner

**Files:**
- Create: `docs/v0/testing/motion_command_validation_matrix.md`
- Modify: `docs/v0/position_control_user_guide.md`

- [ ] **Step 1: Write the operator matrix before adding automation prose**

Document structure:
- mode
- command API
- helper/example
- expected pass condition
- safe parameter limits
- cleanup/stop instruction

- [ ] **Step 2: Add a soak section with exact commands**

Example commands:

```bash
for i in $(seq 1 50); do
  cargo run -p piper-sdk --example hil_joint_position_check -- --interface can0 --joint 1 --delta-rad 0.02 --speed-percent 5 || break
done
```

Include variants for Cartesian, Linear, Circular, MIT, Replay, and gripper.

- [ ] **Step 3: Update the user guide to point to the right validator per mode**

Required notes:
- `send_position_command()` only for `MotionType::Joint`
- `command_cartesian_pose()` for `MotionType::Cartesian`
- `move_linear()` for `MotionType::Linear`
- `move_circular()` for `MotionType::Circular`
- `ReplayMode` and `MitMode` have separate helpers and acceptance criteria

- [ ] **Step 4: Verify docs build/format expectations**

Run:
- `cargo fmt --all -- --check`
- `rg -n "hil_.*check|MotionType::" docs/v0/position_control_user_guide.md docs/v0/testing/motion_command_validation_matrix.md`

Expected: doc references are consistent and easy to follow.

- [ ] **Step 5: Commit**

```bash
git add docs/v0/position_control_user_guide.md docs/v0/testing/motion_command_validation_matrix.md
git commit -m "docs: add motion command validation matrix"
```

### Task 8: Final Verification Sweep

**Files:**
- Modify: none
- Test: entire motion-command validation surface

- [ ] **Step 1: Run the full automated verification suite**

Run:
- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`
- `cargo test -p piper-client`
- `cargo test -p piper-sdk --test high_level_integration_v2`
- `cargo test -p piper-cli`

Expected: PASS

- [ ] **Step 2: Run the HIL smoke suite on real hardware**

Run in safe order:
1. `cargo run -p piper-sdk --example state_api_demo -- --interface can0`
2. `cargo run -p piper-sdk --example hil_joint_position_check -- --interface can0 --joint 1 --delta-rad 0.02 --speed-percent 5`
3. `cargo run -p piper-sdk --example hil_cartesian_pose_check -- --interface can0 ...`
4. `cargo run -p piper-sdk --example hil_linear_motion_check -- --interface can0 ...`
5. `cargo run -p piper-sdk --example hil_circular_motion_check -- --interface can0 ...`
6. `cargo run -p piper-sdk --example hil_mit_hold_check -- --interface can0 ...`
7. `cargo run -p piper-sdk --example hil_replay_mode_check -- --interface can0 ...`
8. `cargo run -p piper-sdk --example hil_gripper_check -- --interface can0 ...`

Expected: each helper prints explicit `[PASS]` lines with observable progress and bounded final error.

- [ ] **Step 3: Capture residual risk**

Document any remaining gaps:
- trajectory shape is only validated at monitor sampling resolution, not with an external motion-capture ground truth
- MIT correctness still depends on conservative arm-side observables unless a dedicated force/torque fixture exists
- replay correctness depends on the quality and safety of the recording file

- [ ] **Step 4: Commit final cleanup if needed**

```bash
git add -A
git commit -m "test: complete motion command validation coverage"
```
