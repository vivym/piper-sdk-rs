# Piper Hardware-in-the-Loop Test Design

> Scope: a manual hardware integration test handbook for the core SDK path on one real Piper arm in the primary environment: Linux + SocketCAN + one operator-supervised robot.

## Goal

Define a repeatable manual HIL acceptance procedure that can prove the core SDK path is correct on real hardware, from SocketCAN transport up through protocol parsing, driver state sync, lifecycle transitions, and low-risk motion control.

## Non-Goals

- Cross-platform validation
- GS-USB validation
- CLI, bridge host, recording/replay, and tooling acceptance
- High-risk motion, payload testing, endurance certification, or production safety sign-off
- Full automation in this phase

## Recommended Environment

- Host: Linux with a fixed kernel version and fixed Rust toolchain
- Transport: SocketCAN on `can0`
- Device under test: one real Piper arm with known firmware version
- Test posture: unloaded, collision-free workspace, emergency stop reachable, second person supervising when motion is enabled
- Motion policy: only low-speed, small-displacement, operator-confirmed moves

## Evidence Required Per Run

- Git commit SHA
- Test date and operator
- Host OS / kernel / `rustc` / `cargo`
- CAN interface name and bitrate
- Robot model and firmware version
- Terminal logs
- Optional video recording for motion phases
- Optional CAN capture when diagnosing failures

## Existing Repo Entry Points

These are not sufficient by themselves, but they are the preferred starting points for manual execution:

- `cargo run -p piper-sdk --example robot_monitor`
- `cargo run -p piper-sdk --example state_api_demo -- --interface can0`
- `cargo run -p piper-sdk --example timestamp_verification -- --interface can0`
- `cargo test -p piper-sdk --test timeout_convergence_tests test_socketcan_timeout_config -- --ignored --nocapture`
- `./scripts/run_realtime_acceptance.sh socketcan-strict`

## Phase Structure

The handbook is organized into five phases. Each phase must define:

- Purpose
- Preconditions
- Execution steps
- Required records
- Pass / fail criteria

The operator may not enter the next phase until the current phase passes or is explicitly waived with a written justification.

## Phase 0: Preflight and Safety Baseline

### Purpose

Establish that the lab environment is trustworthy before any SDK verdict is made.

### Preconditions

- `can0` exists and is configured to the controller bitrate
- Workspace is clear and the arm is unloaded
- Emergency stop is reachable
- Logging and note-taking are prepared before the test begins

### Execution Steps

1. Record `git rev-parse HEAD`, `uname -a`, `rustc --version`, and `cargo --version`.
2. Inspect `can0` status and bitrate, and record interface counters before testing.
3. Confirm the robot is in a safe initial pose and the test area is clear.
4. Confirm there is observable CAN feedback before any motion test is attempted.
5. Start run logging and, if available, video recording.

### Pass Criteria

- Host and interface metadata are recorded
- `can0` is up and configured as expected
- Safety conditions are confirmed in writing
- CAN feedback is present and the physical link is known-good

### Fail Criteria

- Interface state is unknown or unstable
- Bitrate is unverified or mismatched
- Safety conditions are not met
- No feedback can be confirmed on the bus

## Phase 1: Connection and Read-Only Observation

### Purpose

Verify the read path end-to-end: SocketCAN transport, protocol decode, driver synchronization, first snapshot warmup, and stable observer reads.

### Execution Steps

1. Connect in a non-motion path and record time-to-first-successful connection.
2. Verify first complete snapshot arrival and record whether startup produces repeated `incomplete` or `stale` states.
3. Observe the robot continuously and confirm all critical state groups become available:
   - joint position
   - end pose
   - joint dynamic state
   - robot control state
   - gripper state
4. Run a fixed read-only observation window, recommended `10-15` minutes.
5. During the observation window, record:
   - missing state groups
   - repeated stale/incomplete reads
   - disconnects
   - obvious mismatch between on-screen state and real robot condition

### Pass Criteria

- Connection succeeds within the expected timeout budget
- The first complete snapshot arrives reliably
- Critical state groups remain readable through the observation window
- No sustained stale/incomplete condition is observed after warmup
- No unexplained disconnects occur

### Fail Criteria

- First connection is unreliable
- First snapshot cannot be trusted
- Any critical state group remains absent or frequently invalid
- Read path blocks or degrades in a way that obscures robot state

## Phase 2: Safe Lifecycle and State Transitions

### Purpose

Verify that SDK lifecycle transitions match real hardware behavior.

### Required Transition Coverage

- connect -> standby
- standby -> enable
- enable -> disable
- active object drop -> auto-disable
- disconnect -> reconnect
- abnormal interruption -> safe recovery

### Execution Steps

1. Record time to stable standby after connection.
2. Enable control and confirm the robot enters the expected controllable state.
3. Disable control and confirm the robot leaves the active drive state.
4. Validate that releasing the active control object triggers the documented auto-disable behavior.
5. Reconnect and confirm a fresh valid snapshot is established, not stale residual state.
6. Under safe conditions, introduce one controlled interruption and verify the SDK refuses to continue as if nothing happened.

### Pass Criteria

- Transition results match both SDK expectations and real hardware behavior
- No silent mismatch exists between return values and robot state
- Auto-disable on drop is observable and reliable
- Reconnect re-establishes a clean state baseline

### Fail Criteria

- A transition reports success while the robot remains in the wrong state
- A transition reports failure after the robot has already changed state
- Auto-disable does not happen
- Recovery requires restarting the whole application stack

## Phase 3: Low-Risk Motion Validation

### Purpose

Validate the minimum viable control loop on real hardware: command out, motion occurs, feedback returns, and the SDK view matches physical behavior.

### Safety Limits

- Only unloaded tests
- Only low speed
- Only small displacement
- One operator executes, one person supervises when motion is enabled
- Each move is manually confirmed before the next step

### Execution Steps

1. Choose one low-risk joint away from limits.
2. Command a small positive step and verify direction and magnitude trend.
3. Command a small negative step and verify return trend.
4. Repeat the same small move several times to expose intermittent misses or unstable reads.
5. Test sequential single-joint motions across other safe joints.
6. Return the robot to the initial pose or a known safe pose.
7. Attempt a motion command from a state where motion should be rejected and verify that rejection happens.

### Pass Criteria

- Motion direction matches the command
- Feedback position trend matches the physical move
- No unexpected jump, oscillation, or obvious overshoot occurs
- Repeated small moves remain consistent
- Commands are rejected when the SDK state machine says they should be rejected

### Fail Criteria

- Wrong-direction motion
- Large unexpected transient motion
- Feedback and observed motion diverge
- Illegal-state commands still move the robot

## Phase 4: Fault and Recovery Validation

### Purpose

Verify that common field failures lead to explicit, safe degradation and recoverable behavior.

### Required Fault Cases

- Temporary `can0` loss or interface down/up
- Temporary controller reboot or feedback disappearance
- Timeout / dropped-feedback behavior during read or low-risk control
- Post-fault command gating until a safe baseline is re-established

### Execution Steps

1. Induce one controlled interface interruption while not moving and verify the SDK surfaces a real failure.
2. Restore the interface and verify the system can reconnect and rebuild a trustworthy first snapshot.
3. Induce one controlled controller-side interruption, again while not moving if possible.
4. Verify stale or missing feedback does not look like healthy control.
5. After each interruption, verify motion commands are blocked until the system is back in a safe known-good state.

### Pass Criteria

- Faults are explicit in logs or return values
- The system degrades toward safety, not silent continuation
- Recovery restores a clean baseline
- Post-fault motion is gated until recovery is complete

### Fail Criteria

- Faults are silent or ambiguous
- Old state is mistaken for new state after recovery
- Control remains available through an unsafe interruption

## Recording Templates

### Template A: Run Metadata

```text
Run ID:
Date:
Operator:
Supervisor:
Git SHA:
Robot model:
Firmware version:
Host kernel:
rustc:
cargo:
CAN interface:
Bitrate:
Load condition:
Video path:
Log path:
Capture path:
```

### Template B: Per-Test Record

```text
Test ID:
Phase:
Purpose:
Preconditions:
Steps executed:
Expected result:
Actual result:
Pass/Fail:
Artifacts:
Notes:
```

### Template C: Phase Summary

```text
Phase:
Total tests:
Passed:
Failed:
Blocked:
Blocking issues:
Go/No-Go decision:
Approver:
```

## Release Gates

- `Gate 1: Read Path Credible`
  Phase 0 and Phase 1 pass. Outcome: real hardware connection and observation path is trustworthy.

- `Gate 2: Core Control Credible`
  Phase 2 and Phase 3 pass. Outcome: core SDK lifecycle and low-risk control path works on real hardware.

- `Gate 3: Recovery Credible`
  Phase 4 passes. Outcome: common hardware interruptions degrade and recover safely enough for continued integration work.

The final verdict for a run must be one of:

- `PASS`
- `CONDITIONAL PASS`
- `FAIL`

`CONDITIONAL PASS` is only allowed when all failed items are explicitly documented as non-blocking for the intended next step.

## Recommended Next Planning Step

Once this spec is approved, the implementation plan should translate it into:

- one handbook document under repo docs
- one execution checklist template
- one results template
- optional additions to existing examples/tests/scripts to support manual execution without changing the scope to full automation
