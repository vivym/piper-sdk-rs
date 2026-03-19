# Dual-Arm MuJoCo Robot Bring-Up Guide

This guide is the practical bring-up path for:

- two independent Piper arms
- MIT mode on both arms
- MuJoCo-based dynamics compensation
- bilateral teleoperation with force reflection

It assumes you use the addon example:

```bash
cargo run --manifest-path addons/piper-physics-mujoco/Cargo.toml --example dual_arm_bilateral_mujoco -- \
  --left-interface can0 \
  --right-interface can1 \
  --teleop-mode bilateral
```

## Scope

Supported by this guide:

- two independent CAN links or two independent GS-USB devices
- joint-space teleoperation
- master gravity compensation
- slave partial or full inverse dynamics compensation
- runtime payload and mode updates from stdin

Not covered by this guide:

- shared-bus dual-arm offset mode
- Cartesian bilateral control
- network-distributed teleoperation
- gripper force reflection

## Recommended Topology

Use this wiring layout:

- left arm: `can0`
- right arm: `can1`
- one CAN adapter per arm
- one process controlling both arms

Do not start with:

- one shared CAN bus for both arms
- any firmware-side master/slave transparent mode

## Prerequisites

Before the first hardware session:

1. Verify the main SDK passes:
   `cargo clippy --all-targets --all-features -- -D warnings`
2. Verify the MuJoCo addon passes:
   `just check-physics`
3. If needed, download and configure MuJoCo once:
   `just check-physics`
4. Confirm both CAN links are stable and unique.
5. Confirm the workcell is clear and both arms can move through the intended mirrored workspace.

## Startup Order

Use this order every time:

1. Power both arms.
2. Bring up `can0` and `can1`, or connect the two GS-USB devices.
3. Run the demo in `master-follower` mode first.
4. Move both arms by hand to the mirrored zero pose.
5. Press Enter to capture calibration.
6. Verify unilateral mirroring is stable before enabling bilateral reflection.

Recommended first command on Linux:

```bash
cargo run --manifest-path addons/piper-physics-mujoco/Cargo.toml --example dual_arm_bilateral_mujoco -- \
  --left-interface can0 \
  --right-interface can1 \
  --teleop-mode master-follower \
  --master-dynamics-mode gravity \
  --slave-dynamics-mode partial \
  --track-kp 8.0 \
  --track-kd 1.0 \
  --master-damping 0.4
```

Recommended first command on macOS or Windows:

```bash
cargo run --manifest-path addons/piper-physics-mujoco/Cargo.toml --example dual_arm_bilateral_mujoco -- \
  --left-serial LEFT123 \
  --right-serial RIGHT456 \
  --teleop-mode master-follower
```

## Recommended Initial Parameters

Use these as the first real-hardware settings:

| Item | Recommended Start |
|------|-------------------|
| `teleop-mode` | `master-follower` |
| master dynamics | `gravity` |
| slave dynamics | `partial` |
| `frequency-hz` | `200.0` |
| `track-kp` | `8.0` |
| `track-kd` | `1.0` |
| `master-damping` | `0.4` |
| `reflection-gain` | `0.20` to `0.25` |
| `qacc-lpf-cutoff-hz` | `20.0` |
| `max-abs-qacc` | `50.0` |

Why this default split works:

- master `gravity`: keeps the operator side light and predictable
- slave `partial`: reduces tracking effort without the noise sensitivity of full inverse dynamics
- bilateral reflection gain kept low at first: lowers the chance of oscillation while validating sign and wiring

## Bring-Up Sequence

### Phase 1: Connection and Calibration

Success criteria:

- both arms connect without retries
- calibration capture succeeds
- entering MIT mode does not trigger immediate fault or disable

If MIT enable is unstable:

- stop
- reduce external load
- check CAN quality
- confirm no stale feedback or runtime health fault is already present

### Phase 2: Unilateral Mirror Validation

Run `master-follower` first.

Check:

- slave follows the master without obvious oscillation
- no sustained buzzing at rest
- no rapid growth in realtime overwrites
- mirrored directions are correct for all six joints

If the slave is soft and lags:

- raise `track-kp` gradually
- then raise `track-kd` slightly

If the slave oscillates:

- lower `track-kp`
- or raise `track-kd`
- confirm payload is not badly wrong

### Phase 3: Bilateral Reflection Ramp-Up

Once master-follower is stable, switch to bilateral:

```bash
cargo run --manifest-path addons/piper-physics-mujoco/Cargo.toml --example dual_arm_bilateral_mujoco -- \
  --left-interface can0 \
  --right-interface can1 \
  --teleop-mode bilateral \
  --master-dynamics-mode gravity \
  --slave-dynamics-mode partial \
  --reflection-gain 0.20
```

Ramp reflection in this order:

1. `0.20`
2. `0.25`
3. `0.30`

Do not raise reflection gain until:

- free-space motion is stable
- slave contact against a soft obstacle reflects in the correct direction
- release after contact does not ring

## When To Use Each Dynamics Mode

Use `gravity` when:

- the arm is mostly backdriven by hand
- you want the cleanest, lightest operator feel
- you are validating basic sign and calibration

Use `partial` when:

- the slave is moving at moderate speed
- you want lower tracking gains than a pure-gravity slave
- you want a conservative production default

Use `full` when:

- slave motion is fast
- accelerations are significant
- the partial mode still leaves noticeable dynamic lag

Do not start with `full` on both arms. Start with:

- master: `gravity`
- slave: `partial`

## Payload Workflow

The demo supports runtime payload updates without rebuilding the compensator.

Available stdin commands:

```text
show
master payload <mass> <x> <y> <z>
slave payload <mass> <x> <y> <z>
master mode <gravity|partial|full>
slave mode <gravity|partial|full>
quit
```

Recommended payload procedure:

1. Start with both payloads at `0kg`.
2. Validate free-space behavior.
3. Add the slave payload first.
4. Re-check free-space tracking.
5. Re-check contact reflection.
6. Only add master payload if the operator arm itself carries tooling or meaningful end load.

Example:

```text
slave payload 0.45 0.00 0.00 0.08
show
```

Payload symptoms:

- slave droops in a static pose: payload is under-modeled
- slave feels too stiff upward and too light downward: payload or COM is wrong
- reflected force always feels biased even in free space: payload or master gravity model is wrong

## Runtime Checks

Watch these outputs after each session:

- `read_faults`
- `command_faults`
- `max_inter_arm_skew`
- `left tx realtime overwrites`
- `right tx realtime overwrites`
- `last_error`

Healthy first-session expectations:

- `command_faults = 0`
- `read_faults = 0` in steady state
- `max_inter_arm_skew` stays comfortably below the default threshold
- realtime overwrite counters do not climb continuously

## Fault Response Expectations

The loop should safely converge if:

- one arm stops reporting
- one arm becomes runtime unhealthy
- the compensator returns an error

Expected behavior:

- stale or misaligned state: safe-hold, then disable
- runtime health unhealthy: emergency stop path
- compensator failure: safe-hold, then disable

During testing, intentionally verify at least:

1. unplug or stop one feedback path
2. ensure the loop exits through the safety path
3. confirm both arms do not stay torque-enabled indefinitely

## Tuning Heuristics

Use these adjustments one at a time:

- slave lag in free space:
  increase `track-kp`
- slave overshoot:
  increase `track-kd`
- master feels sticky:
  reduce `master-damping`
- master feels noisy on contact:
  reduce `reflection-gain`
- contact reflection is too weak:
  increase `reflection-gain` after payload is correct
- fast slave motion still lags:
  try `--slave-dynamics-mode full`
- full mode feels noisy:
  lower `qacc-lpf-cutoff-hz` or reduce `max-abs-qacc`

## Recommended Acceptance Test

Sign off a configuration only after it passes all of these:

1. calibration capture succeeds repeatedly
2. `master-follower` runs for several minutes without read or command faults
3. mirrored motion is directionally correct for all joints
4. slave holds a static pose without obvious gravity sag
5. bilateral mode reflects soft contact cleanly
6. disconnecting one arm causes safe shutdown behavior

## Useful Commands

Quick validation:

```bash
just check-physics
just test-physics --lib
just clippy-physics
```

Non-physics dual-arm baseline:

```bash
cargo run --example dual_arm_bilateral_control -- \
  --left-interface can0 \
  --right-interface can1 \
  --mode master-follower
```

MuJoCo bilateral demo:

```bash
cargo run --manifest-path addons/piper-physics-mujoco/Cargo.toml --example dual_arm_bilateral_mujoco -- \
  --left-interface can0 \
  --right-interface can1 \
  --teleop-mode bilateral \
  --master-dynamics-mode gravity \
  --slave-dynamics-mode partial \
  --reflection-gain 0.25
```

## Session Log

For formal lab records, use:

- [../../docs/v0/piper-physics/DUAL_ARM_BILATERAL_TUNING_LOG_TEMPLATE.md](../../docs/v0/piper-physics/DUAL_ARM_BILATERAL_TUNING_LOG_TEMPLATE.md)
