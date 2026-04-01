# Piper HIL Execution Checklist

Use this checklist with [piper_hil_handbook.md](/home/viv/projs/piper-sdk-rs/docs/v0/piper_hil_handbook.md) for one real Piper arm on Linux with SocketCAN.

## Run Setup

- [ ] Record the git SHA, date, operator, and supervisor
- [ ] Record the host OS, kernel, `rustc`, and `cargo` versions
- [ ] Record the robot model, firmware version, CAN interface, and bitrate
- [ ] Confirm the workspace is clear and the emergency stop is reachable
- [ ] Confirm the arm is unloaded
- [ ] Confirm logging is ready before the first connection attempt

## Phase 0: Preflight and Safety Baseline

- [ ] Check `can0` status and counters with `ip -details link show can0`
- [ ] Check `can0` statistics with `ip -statistics link show can0`
- [ ] If needed, bring `can0` up with the configured bitrate
- [ ] Confirm the robot is in a safe initial pose
- [ ] Confirm CAN feedback is present before any motion attempt with `cargo run -p piper-sdk --example robot_monitor -- --interface can0`

Pass if:

- [ ] Host and interface metadata are recorded
- [ ] `can0` is up and configured correctly
- [ ] Safety conditions are confirmed in writing
- [ ] Live feedback is visible on the bus

## Phase 1: Connection and Read-Only Observation

- [ ] Run `cargo run -p piper-sdk --example client_monitor_hil_check -- --interface can0 --baud-rate 1000000 --observation-window-secs 900`
- [ ] Confirm connection succeeds within `<= 5s`
- [ ] Confirm the first complete monitor snapshot arrives within `<= 200ms`
- [ ] Keep the helper running for the full `15 min` observation window
- [ ] Use `cargo run -p piper-sdk --example robot_monitor -- --interface can0` when you need continuous corroboration
- [ ] Use `cargo run -p piper-sdk --example state_api_demo -- --interface can0` when you need a one-shot corroboration

Pass if:

- [ ] Connection succeeds within `<= 5s`
- [ ] The first complete snapshot arrives within `<= 200ms`
- [ ] The full `15 min` observation window completes
- [ ] No helper timeout, missed first snapshot, or unexplained disconnect occurs

## Phase 2: Safe Lifecycle and State Transitions

- [ ] Run `cargo run -p piper-sdk --example hil_joint_position_check -- --interface can0 --baud-rate 1000000 --joint 1 --delta-rad 0.02 --speed-percent 10`
- [ ] Confirm Standby before enabling motion
- [ ] Confirm enable enters `PositionMode + MotionType::Joint`
- [ ] Confirm the commanded joint moves and returns to start
- [ ] For explicit disable, use `cargo run -p piper-cli -- shell`, then `connect socketcan:can0`, `enable`, `disable`, `exit`
- [ ] For drop or emergency-style stop, use `cargo run -p piper-cli -- stop --target socketcan:can0`
- [ ] For reconnect behavior, start a fresh helper run and re-check the `<= 5s` and `<= 200ms` budgets

Pass if:

- [ ] Helper output shows Standby, enable, move, and return lines
- [ ] CLI `disable` completes without error and does not itself trigger motion
- [ ] `robot_monitor` or `state_api_demo` shows the robot is back in Standby or another non-driving state
- [ ] The drop or emergency-style stop leaves the robot in a non-driving state
- [ ] A fresh helper run reconnects within `<= 5s`
- [ ] A fresh helper run produces its first complete snapshot within `<= 200ms`

## Phase 3: Low-Risk Motion Validation

- [ ] Keep the arm unloaded
- [ ] Keep the selected joint away from limits
- [ ] Keep motion within the low-risk envelope
- [ ] Confirm `PositionMode + MotionType::Joint only`
- [ ] Confirm `speed_percent <= 10`
- [ ] Confirm `abs(delta) <= 0.035 rad`
- [ ] Confirm each step is manually confirmed before the next step
- [ ] Confirm no MIT mode, Cartesian, Linear, or Circular motion is used

Pass if:

- [ ] Motion direction matches the command
- [ ] Feedback trend matches the physical move
- [ ] The commanded joint begins moving within `2s`
- [ ] Each commanded step settles within `10s`
- [ ] Return-to-start error is `<= 0.05 rad`

## Phase 4: Fault and Recovery Validation

- [ ] Induce one controlled `can0` interruption while not moving
- [ ] Restore the interface and confirm a fresh helper run reconnects within `<= 5s`
- [ ] Confirm a fresh helper run prints its first complete snapshot within `<= 200ms`
- [ ] Induce one controlled controller-side interruption while not moving if possible
- [ ] Before declaring recovery complete, run the shell probe `move --joints 0.02 --force`
- [ ] Confirm the shell rejects the probe or otherwise fails without causing motion
- [ ] Confirm post-fault motion remains gated until the system is back in a safe known-good state

Pass if:

- [ ] Faults are explicit in logs or return values
- [ ] The system degrades toward safety
- [ ] Recovery returns the robot to readable state
- [ ] The post-fault shell motion probe is rejected or fails without causing motion

## Release Gates

- [ ] `Gate 1: Read Path Credible` passed
- [ ] `Gate 2: Core Control Credible` passed
- [ ] `Gate 3: Recovery Credible` passed
- [ ] Final verdict recorded as `PASS`, `CONDITIONAL PASS`, or `FAIL`

