# Piper HIL Handbook

## Scope

This handbook defines the manual hardware-in-the-loop acceptance flow for one real Piper arm on Linux with SocketCAN. It covers the core SDK path from transport and protocol decode through driver state sync, client lifecycle transitions, read-only observation, and low-risk position control.

This is not a general automation guide. It does not cover GS-USB, cross-platform validation, bridge-host acceptance, or high-risk motion.

## Safety Baseline

- One operator runs the test.
- One second person supervises whenever motion is enabled.
- The arm is unloaded.
- The workspace is clear and collision-free.
- The emergency stop is reachable before any motion step.
- Motion stays within the low-risk envelope for this handbook revision.
- If a phase needs an override, the operator records the override and reason before starting that phase.

## Acceptance Thresholds

The default go/no-go thresholds for this handbook are:

- Connection budget: `<= 5s`
- Reconnect budget: `<= 5s`
- Initial client monitor snapshot budget: `<= 200ms`
- Observation window: `15 min`
- Motion mode: `PositionMode + MotionType::Joint only`
- Speed cap: `speed_percent <= 10`
- Single-step displacement cap: `abs(delta) <= 0.035 rad`
- Return-to-start tolerance: `<= 0.05 rad`

If a run intentionally uses different values, document the override before the phase begins.

## Tooling Entry Points

Use these manual helpers as the primary entry points for this handbook:

- Read-only client monitor check:
  `cargo run -p piper-sdk --example client_monitor_hil_check -- --interface can0 --baud-rate 1000000 --observation-window-secs 900`
- Safe joint position check:
  `cargo run -p piper-sdk --example hil_joint_position_check -- --interface can0 --baud-rate 1000000 --joint 1 --delta-rad 0.02 --speed-percent 10`

Supporting entry points:

- Driver-level observation:
  `cargo run -p piper-sdk --example robot_monitor -- --interface can0`
- Driver-level one-shot inspection:
  `cargo run -p piper-sdk --example state_api_demo -- --interface can0`
- Timestamp verification:
  `cargo run -p piper-sdk --example timestamp_verification -- --interface can0`

## Phase 0: Preflight and Safety Baseline

### Purpose

Confirm the test environment is safe and the transport is configured before any SDK verdict is made.

### Preconditions

- `can0` exists and matches the controller bitrate
- The arm is unloaded and clear of obstacles
- The emergency stop is reachable
- Logging is ready before the first connection attempt

### Execution

1. Record `git rev-parse HEAD`, `uname -a`, `rustc --version`, and `cargo --version`.
2. Confirm `can0` is up and configured as expected.
3. Record interface counters before starting the run.
4. Verify the robot is in a safe initial pose.
5. Confirm that CAN feedback is present before any motion attempt.

### Pass Criteria

- Host and interface metadata are recorded
- `can0` is up and configured correctly
- Safety conditions are confirmed in writing
- Feedback is present on the bus

### Fail Criteria

- Interface state is unknown or unstable
- Bitrate is unverified or mismatched
- Safety conditions are not met
- No feedback can be confirmed

## Phase 1: Connection and Read-Only Observation

### Purpose

Verify the read path end to end: SocketCAN transport, protocol decode, driver synchronization, first snapshot warmup, and stable observer reads.

### Preconditions

- Phase 0 passed
- No motion is enabled
- The operator is ready to observe the robot for the full window

### Execution

1. Start the read-only helper with the live interface.
2. Confirm the first connection succeeds within `<= 5s`.
3. Confirm the first complete client monitor snapshot arrives within `<= 200ms`.
4. Observe the robot continuously for `15 min`.
5. During the window, record missing state groups, repeated stale or incomplete reads, disconnects, and any mismatch between on-screen state and the real robot.
6. Verify the current state groups remain readable:
   - joint position
   - end pose
   - joint dynamic state
   - robot control state
   - gripper state

### Pass Criteria

- Connection succeeds within `<= 5s`
- The first complete client monitor snapshot arrives within `<= 200ms`
- The full `15 min` observation window completes
- No sustained stale or incomplete burst longer than `1s` appears after warmup
- No unexplained disconnect occurs

### Fail Criteria

- First connection is unreliable
- The first snapshot budget is missed
- A critical state group remains absent or frequently invalid
- Read path behavior obscures the robot state

## Phase 2: Safe Lifecycle and State Transitions

### Purpose

Verify that SDK lifecycle transitions match real hardware behavior.

### Preconditions

- Phase 1 passed
- The robot is confirmed in Standby before enabling motion
- Only the position-control path is in scope

### Execution

1. Record time to stable Standby after connection.
2. Enable position mode with `MotionType::Joint` and `speed_percent <= 10`.
3. Confirm the robot enters the expected controllable state within `<= 5s`.
4. Disable control and confirm the robot leaves the active drive state within `<= 5s`.
5. Drop the active control object and verify auto-disable behavior.
6. Disconnect and reconnect, then confirm the system rebuilds a clean baseline within the same `<= 5s` connection and `<= 200ms` snapshot budgets.
7. Introduce one controlled interruption under safe conditions and verify the SDK does not continue as if nothing happened.

### Pass Criteria

- Transition results match SDK expectations and robot state
- No silent mismatch exists between return values and robot state
- Auto-disable on drop is observable and reliable
- Reconnect re-establishes a clean baseline

### Fail Criteria

- A transition reports success while the robot remains in the wrong state
- A transition reports failure after the robot has already changed state
- Auto-disable does not happen
- Recovery requires restarting the full application stack

## Phase 3: Low-Risk Motion Validation

### Purpose

Validate the minimum viable control loop on real hardware: command out, motion occurs, feedback returns, and the SDK view matches physical behavior.

### Preconditions

- Phase 2 passed
- The arm is unloaded
- The selected joint is away from limits
- Motion remains within the low-risk envelope

### Safety Limits

- `PositionMode + MotionType::Joint only`
- `speed_percent <= 10`
- `abs(delta) <= 0.035 rad`
- Each step is manually confirmed before the next step
- No MIT mode, Cartesian, Linear, or Circular motion

### Execution

1. Choose one low-risk joint away from limits.
2. Command a small positive step and verify the commanded joint begins moving in the expected direction within `2s`.
3. Command a small negative step and verify the return trend within `2s`.
4. Repeat the same small move several times to expose intermittent misses or unstable reads.
5. Test sequential single-joint motions across other safe joints.
6. Allow up to `10s` for settling before judging each step.
7. Return the robot to the initial pose or another known safe pose.
8. Verify the commanded joint returns to within `<= 0.05 rad` of its starting position.
9. Attempt a motion command from a state where motion should be rejected and verify rejection.

### Pass Criteria

- Motion direction matches the command
- Feedback trend matches the physical move
- The commanded joint begins moving within `2s`
- No unexpected jump, oscillation, or obvious overshoot occurs
- Repeated small moves remain consistent
- Each commanded step settles within `10s`
- Return-to-start error is `<= 0.05 rad`
- Rejected-state commands do not move the robot

### Fail Criteria

- Wrong-direction motion
- Large unexpected transient motion
- Feedback and observed motion diverge
- Illegal-state commands still move the robot

## Phase 4: Fault and Recovery Validation

### Purpose

Verify that common field failures lead to explicit, safe degradation and recoverable behavior.

### Preconditions

- Phase 3 passed, or motion is already disabled
- The operator can safely interrupt transport or controller availability

### Required Fault Cases

- Temporary `can0` loss or interface down/up
- Temporary controller reboot or feedback disappearance
- Timeout or dropped-feedback behavior during read or low-risk control
- Post-fault command gating until a safe baseline is re-established

### Execution

1. Induce one controlled interface interruption while not moving and verify the SDK surfaces a real failure.
2. Restore the interface and verify reconnect completes within `<= 5s`.
3. Verify the first trustworthy snapshot rebuilds within `<= 200ms`.
4. Induce one controlled controller-side interruption, again while not moving if possible.
5. Verify stale or missing feedback does not look like healthy control.
6. After each interruption, verify motion commands are blocked until the system is back in a safe known-good state.

### Pass Criteria

- Faults are explicit in logs or return values
- The system degrades toward safety
- Recovery restores a clean baseline
- Post-fault motion is gated until recovery is complete

### Fail Criteria

- Faults are silent or ambiguous
- Old state is mistaken for new state after recovery
- Control remains available through an unsafe interruption

## Release Gates

- `Gate 1: Read Path Credible`
  Phase 0 and Phase 1 pass. The real-hardware connection and observation path are trustworthy.

- `Gate 2: Core Control Credible`
  Phase 2 and Phase 3 pass. The core lifecycle and low-risk control path work on real hardware.

- `Gate 3: Recovery Credible`
  Phase 4 passes. Common interruptions degrade and recover safely enough for continued integration work.

The final verdict for a run must be one of:

- `PASS`
- `CONDITIONAL PASS`
- `FAIL`

`CONDITIONAL PASS` is only allowed when every failed item is explicitly documented as non-blocking for the intended next step.

## Artifacts to Capture

Record the following for every run:

- Git commit SHA
- Test date and operator
- Host OS, kernel, `rustc`, and `cargo`
- CAN interface name and bitrate
- Robot model and firmware version
- Terminal logs
- Optional video recording for motion phases
- Optional CAN capture for failure diagnosis

Use a short run note for each phase:

```text
Run ID:
Phase:
Result:
Observed budgets:
Notes:
Artifacts:
```

