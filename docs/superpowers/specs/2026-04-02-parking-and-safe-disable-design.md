# Parking And Safe Disable Design

## Context

Current behavior is split across layers:

- `piper-client` treats `disable()`, `stop`, and `Drop` as direct motor-disable paths.
- `piper-control` already contains parking concepts:
  - `ParkOrientation::{Upright, Left, Right}`
  - `ControlProfile::park_pose()`
  - `park_blocking()`
- CLI already exposes `park`, while HIL helpers still mostly rely on explicit return-to-start plus implicit drop-time disable.

This is a problem on hardware configurations where a direct disable causes the arm to sag or fall because the system does not hold position after motors are disabled.

The repository also already distinguishes emergency behavior from non-emergency workflows:

- `stop` / `Ctrl+C` are emergency-style operators that cancel work and disable immediately.
- `disable()` is a low-level state transition primitive.
- `park` is a controlled motion workflow.

The design must preserve those semantics.

## Goals

- Make controlled parking a first-class success-path workflow for motion tools.
- Reuse existing park orientation and pose concepts instead of inventing a parallel system.
- Keep `stop`, `disable()`, and drop-time safety nets as direct disable paths.
- Provide one shared parking workflow that HIL helpers and CLI can both use.
- Avoid silently changing emergency behavior.

## Non-Goals

- Do not change `piper-client::disable()` to perform implicit motion.
- Do not change `stop` into a park-like operation.
- Do not add implicit parking to `Drop`.
- Do not attempt to solve Cartesian / Linear / Circular motion failures in this change.

## Existing Relevant Code

- `crates/piper-control/src/profile.rs`
  - `ParkOrientation`
  - `default_rest_pose()`
  - `ControlProfile::park_pose()`
- `crates/piper-control/src/workflow.rs`
  - `move_to_joint_target_blocking()`
  - `home_zero_blocking()`
  - `park_blocking()`
- `apps/cli/src/commands/park.rs`
- `apps/cli/src/modes/repl.rs`
- `crates/piper-client/src/state/machine.rs`
  - `disable()`
  - drop-time best-effort disable
- `crates/piper-sdk/examples/hil_joint_position_check.rs`
  - current success path returns to start, then depends on disable/drop semantics

## Design Summary

Parking should become a unified high-level workflow in the `piper-control` / `piper-sdk` experience layer, not a new implicit behavior inside `piper-client`.

The system will continue to have three distinct meanings:

- `stop`: immediate safe interruption, no parking.
- `disable()`: direct disable primitive, no parking.
- `park_then_disable`: controlled non-emergency shutdown workflow.

The success path for selected HIL helpers should default to:

1. Perform the intended test motion.
2. Return to the helper-specific anchor/start state if applicable.
3. Move to a configured parking pose.
4. Explicitly disable and confirm `Standby`.

Failure paths should not automatically attempt parking.

## Layering Decision

### `piper-client`

Keep current semantics unchanged.

Rationale:

- `disable()` is a primitive state transition.
- `stop` must remain usable as an emergency path.
- implicit motion in `disable()` or `Drop` would violate current safety assumptions and create surprising behavior under faults.

Possible changes in this layer are documentation-only, clarifying that parking is a higher-level workflow.

### `piper-control`

This is the correct home for the shared parking workflow.

Reasons:

- It already owns `ControlProfile`, `ParkOrientation`, and default park poses.
- It already implements blocking joint-motion workflows on top of `piper-client`.
- CLI already depends on this layer for `home` / `park` style operations.

### `piper-sdk`

Examples and HIL helpers should call the shared parking workflow from `piper-control` or an equivalent shared helper, instead of open-coding their own success-path cleanup.

## Workflow API

Add a unified blocking workflow in `piper-control`.

Candidate names:

- `park_then_disable_blocking()`
- `move_to_park_and_disable_blocking()`

Recommended name:

- `park_then_disable_blocking()`

Proposed shape:

```rust
pub fn park_then_disable_blocking<Capability>(
    standby: Piper<Standby, Capability>,
    profile: &ControlProfile,
) -> Result<Piper<Standby, Capability>>
where
    Capability: MotionCapability
```

and a companion for already-active joint-position flows:

```rust
pub fn active_park_then_disable_blocking<Capability>(
    active: Piper<Active<PositionMode>, Capability>,
    profile: &ControlProfile,
) -> Result<Piper<Standby, Capability>>
where
    Capability: MotionCapability
```

The second function avoids unnecessary re-enable/re-enter-mode cycles for helpers that are already in joint position mode.

## Park Pose Source

Parking pose resolution will continue to use existing `ControlProfile` behavior:

- explicit `rest_pose_override` if configured
- otherwise `ParkOrientation::default_rest_pose()`

This keeps the source of truth single and preserves existing CLI config behavior.

No new pose registry is needed in this change.

## Success-Path Policy

Default policy:

- On successful completion of a motion helper, perform `park -> disable`.
- On failed motion helper execution, do not attempt parking automatically.
- On `stop` / `Ctrl+C` / emergency interruption, do not attempt parking.

Rationale:

- If parking fails, automatically disabling anyway may still cause sagging.
- Failure handling must remain explicit and conservative.
- Emergency paths must not enqueue extra motion.

## HIL Helper Changes

Phase 1 scope:

- `hil_joint_position_check`

Behavior change:

- current:
  - move
  - return to initial snapshot
  - implicit disable on drop / explicit state teardown
- new:
  - move
  - return to initial snapshot
  - park to `ControlProfile::park_pose()`
  - explicit disable to confirmed `Standby`

CLI/user control:

- default enabled
- allow an opt-out flag such as `--no-park`

Reason for opt-out:

- debugging
- constrained workspaces
- preserving current experimental workflows when needed

Future candidates after validation:

- `hil_linear_motion_check`
- `hil_circular_motion_check`
- `hil_mit_hold_check`

But those should not be changed in the first patch.

## CLI Changes

CLI already has `park`.

Phase 1 recommendation:

- keep `park` as-is
- add a new explicit non-emergency command:
  - `park-and-disable`
  - or `safe-disable`

Recommended name:

- `park-and-disable`

Reason:

- explicit and unsurprising
- distinguishes itself from raw `disable`
- matches the workflow semantics exactly

`disable` remains direct disable.

Shell behavior in phase 1 should stay conservative:

- do not silently change existing `disable`
- do not automatically park on every successful interactive command yet

That behavior can be revisited after HIL validation.

## Failure Handling

### Parking motion fails before disable

Return an error and keep control with the caller.

Do not silently continue to disable.

Operator response remains explicit:

- inspect state
- choose `stop` if needed
- decide whether to retry or manually recover

### Disable fails after successful park

Return the disable error.

This is still an improvement over the current sag-on-disable risk because the arm is at least in a better pose before failure handling.

### Emergency stop / cancellation

No parking.

This remains:

- cancel current motion if possible
- disable immediately

## Testing Strategy

### `piper-control`

Add unit coverage for:

- `ParkOrientation` pose selection remains unchanged
- `park_then_disable_blocking()` uses `profile.park_pose()`
- active variant parks and disables without re-entering an unrelated mode

### `piper-sdk` HIL helper tests

Add example-local tests for:

- argument parsing for `--no-park`
- success-path planning helper chooses parking by default
- opt-out disables parking stage

### CLI

Add command tests for:

- `park-and-disable` command construction and config resolution

### Documentation

Update:

- HIL operator runbook
- motion validation matrix
- CLI README

to distinguish:

- `stop`
- `disable`
- `park`
- `park-and-disable`

## Rollout Plan

1. Add shared parking workflow to `piper-control`.
2. Integrate it into `hil_joint_position_check`.
3. Add CLI `park-and-disable`.
4. Update docs.
5. Validate on hardware with the existing low-risk joint HIL path before expanding to more helpers.

## Risks

- Default park poses may not be safe for every physical installation or payload.
- Some workspaces may not have clearance for the configured park pose.
- If users assume `disable` now parks automatically, they may still trigger sagging through direct disable paths.

Mitigations:

- preserve explicit naming
- keep opt-out on helper auto-parking
- continue to support per-profile pose override
- document that only the parking workflow performs controlled motion before disable

## Recommendation

Implement parking as a shared experience-layer workflow centered in `piper-control`, reuse existing `ControlProfile::park_pose()` and `ParkOrientation`, and adopt it first in `hil_joint_position_check` plus a new CLI `park-and-disable` command.

Do not modify `piper-client::disable()`, `stop`, or drop-time disable semantics.
