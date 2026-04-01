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

- `client_monitor_hil_check` covers Phase 1 only.
  - It connects to the robot, measures the `<= 5s` connection budget, waits for the first complete monitor snapshot within `<= 200ms`, and then runs the `15 min` read-only observation window.
  - It is the primary evidence source for connection timing and client monitor warmup behavior.
  - It does not enable motion, disable motion, reconnect after a fault, or validate rejected-state or fault-gating behavior.
- `hil_joint_position_check` covers the helper-driven parts of Phases 2 and 3 only.
  - It connects, confirms Standby, enables `PositionMode + MotionType::Joint`, sends one low-risk joint move, and returns the commanded joint to the start position.
  - It is the primary evidence source for the low-risk position-control path.
  - It does not validate explicit disable commands, reconnect after interruption, rejected-state gating, or fault recovery.

Use these exact helper commands:

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
- Manual CLI support path for lifecycle and recovery checks:
  - `piper-cli shell`
    - `connect socketcan:can0`
    - `enable`
    - `stop`
    - `exit`
  - `piper-cli stop --target socketcan:can0`

Use the supporting driver-level tools as evidence collectors, not as hidden automation:

- Use `robot_monitor` when you need continuous evidence for joint dynamic state or gripper state during Phase 1 or when checking recovery after a manual interruption.
- Use `state_api_demo` when you need a one-shot corroboration of control-state or driver-state fields at the start or end of a phase.
- Use `client_monitor_hil_check` when you need the timed Phase 1 proof: connect, first snapshot, and the full observation window.
- Use `hil_joint_position_check` when you need the timed Phase 2 and Phase 3 proof: Standby, enable, one low-risk move, and return-to-start.

The manual checks that are not covered by a helper are:

- explicit disable behavior
- reconnect after interruption
- rejected-state command gating
- fault and recovery gating after `can0` loss or controller-side interruption

For those checks, use `piper-cli shell` for a live session or `piper-cli stop --target socketcan:can0` for a direct stop, and use the helper output plus `robot_monitor` or `state_api_demo` to confirm the robot state before and after the operator action.

## Phase 0: Preflight and Safety Baseline

### Purpose

Confirm the test environment is safe and the transport is configured before any SDK verdict is made.

### Preconditions

- `can0` exists and matches the controller bitrate
- The arm is unloaded and clear of obstacles
- The emergency stop is reachable
- Logging is ready before the first connection attempt

### Execution

1. Record the test metadata:
   ```bash
   git rev-parse HEAD
   uname -a
   rustc --version
   cargo --version
   ```
2. Record the `can0` configuration and counters:
   ```bash
   ip -details link show can0
   ip -statistics link show can0
   ```
3. If `can0` is down, bring it up with the configured bitrate and record the command used:
   ```bash
   sudo ip link set can0 up type can bitrate 1000000
   ```
4. Verify the robot is in a safe initial pose and the workspace is clear.
5. Confirm that CAN feedback is present before any motion attempt:
   ```bash
   cargo run -p piper-sdk --example robot_monitor -- --interface can0
   ```
   Treat the first live joint or gripper update as the preflight evidence that feedback is present on the bus.

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

1. Start `client_monitor_hil_check` and use its log lines as the connection and snapshot timing evidence.
2. Confirm the helper prints a connection time within `<= 5s`.
3. Confirm the helper prints the first complete client monitor snapshot within `<= 200ms`.
4. Keep the helper running for the full `15 min` observation window.
5. Run `robot_monitor` alongside the helper when you need continuous evidence for:
   - joint dynamic state
   - gripper state
6. Use `state_api_demo` at the beginning and end of the window when you need a one-shot corroboration of:
   - robot control state
   - end pose
   - any state that is harder to inspect from the read-only helper alone
7. During the window, treat a helper timeout, a missed `<= 200ms` first snapshot, or an unexplained disconnect as the observable stale/incomplete failure mode.
8. If you need corroboration, use `robot_monitor` or `state_api_demo` to show that live feedback is still present while the helper is timing out or restarting.
9. Do not score hidden client-side retry counts; the observable criterion is whether the helper produces the required first snapshot and remains connected for `15 min`.

### Pass Criteria

- Connection succeeds within `<= 5s`
- The first complete client monitor snapshot arrives within `<= 200ms`
- The full `15 min` observation window completes
- No helper timeout, missed first snapshot, or unexplained disconnect occurs during the observation window
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

Standby is concrete here: `hil_joint_position_check` prints `[PASS] connected and confirmed Standby`, and the supporting read-only tools show the robot is not in a motion-enabled state before the enable step begins.

### Execution

1. Run `hil_joint_position_check` to cover the helper-driven portion of this phase.
2. Use its `[PASS] connected and confirmed Standby` line as the Standby evidence.
3. Use its `[PASS] enabled PositionMode motion=Joint speed_percent=...` line as the enabled-state evidence.
4. Use the `[PASS] settle step=move ...` and `[PASS] settle step=return ...` lines as the motion and return evidence.
5. For explicit disable behavior, stop the active control session manually and confirm the robot leaves the active drive state with `robot_monitor` or `state_api_demo`.
6. For drop-to-disable behavior, terminate the active control process and confirm the robot returns to Standby or a non-driving state.
7. For reconnect behavior, start a fresh helper run and confirm the new connection and first snapshot again meet the `<= 5s` and `<= 200ms` budgets.
8. For rejected-state gating, start `hil_joint_position_check` while the robot is not in Standby and confirm it fails with `robot is not in confirmed Standby; run stop first`.

### Pass Criteria

- The helper prints the expected Standby, enable, move, and return lines
- After the manual disable or drop step, `robot_monitor` or `state_api_demo` shows the robot is no longer in an active drive state
- A fresh helper run reconnects within `<= 5s`
- A fresh helper run produces its first complete snapshot within `<= 200ms`
- The helper's printed state matches the observed robot state in `robot_monitor` or `state_api_demo`
- Stopping the control process causes the robot to return to Standby or another non-driving state
- A reconnect does not reuse stale monitor state from the interrupted run

### Fail Criteria

- A transition reports success while the robot remains in the wrong state
- A transition reports failure after the robot has already changed state
- The robot remains in an active drive state after the control process is stopped
- A reconnect reuses stale monitor state from the interrupted run
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
2. Run `hil_joint_position_check` for one small positive or negative delta that stays within `abs(delta) <= 0.035 rad`.
3. Use the helper's `[PASS] command step=move ...` and `[PASS] settle step=move ...` lines as the evidence that the commanded joint began moving and settled.
4. Use the helper's `[PASS] command step=return ...` and `[PASS] settle step=return ...` lines as the evidence that the robot returned to the start pose.
5. If you want a second manual check, repeat with another safe joint and keep the same limits.
6. Allow up to `10s` for settling before judging each step.
7. The only return criterion for this phase is the return to the initial pose of the commanded joint.

### Pass Criteria

- Motion direction matches the command
- Feedback trend matches the physical move
- The commanded joint begins moving within `2s`
- No unexpected jump, oscillation, or obvious overshoot occurs
- Repeated small moves remain consistent
- Each commanded step settles within `10s`
- Return-to-start error is `<= 0.05 rad`

### Fail Criteria

- Wrong-direction motion
- Large unexpected transient motion
- Feedback and observed motion diverge

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
2. Restore the interface and verify a fresh helper run reconnects within `<= 5s`.
3. Verify that fresh helper run prints its first complete snapshot within `<= 200ms`.
4. Induce one controlled controller-side interruption, again while not moving if possible.
5. Verify stale or missing feedback does not look like healthy control.
6. After each interruption, verify motion commands are blocked until the system is back in a safe known-good state.

### Pass Criteria

- Faults are explicit in logs or return values
- The system degrades toward safety
- After recovery, a fresh helper run reconnects within `<= 5s` and prints its first complete snapshot within `<= 200ms`
- `robot_monitor` or `state_api_demo` again shows readable state after recovery
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
