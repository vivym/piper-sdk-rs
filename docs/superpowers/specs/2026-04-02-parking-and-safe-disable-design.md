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
- `park` is already a controlled motion workflow that parks and then disables.

The design must preserve those semantics.

## Goals

- Make controlled parking a first-class success-path workflow for motion tools.
- Reuse existing park orientation and pose concepts instead of inventing a parallel system.
- Reuse the existing `park` / `park_blocking()` concept instead of introducing a second synonymous command.
- Keep `stop`, `disable()`, and drop-time safety nets as direct disable paths.
- Provide one shared parking workflow that HIL helpers and CLI can both use.
- Avoid silently changing emergency behavior.
- Ensure parking uses an explicit low-speed profile instead of inheriting `PositionModeConfig::default()`.

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
- `park`: controlled non-emergency shutdown workflow that parks and then disables.

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

Phase 1 should reuse the existing blocking workflow names in `piper-control` instead of
introducing a second synonymous API.

The existing `park_blocking()` remains the shared standby-entry workflow:

```rust
pub fn park_blocking<Capability>(
    standby: Piper<Standby, Capability>,
    profile: &ControlProfile,
) -> Result<Piper<Standby, Capability>>
where
    Capability: MotionCapability
```

For already-active joint-position flows, Phase 1 should not add a by-value
`active_park_then_disable_blocking(active, ...) -> Result<Piper<Standby, ...>>` API.

Reason:

- an owned `Piper<Active<...>>` that errors during parking will be dropped on the error path
- dropping `Active` currently triggers best-effort disable
- therefore such an API cannot truthfully promise "return an error and leave the active robot
  in the caller's control"

Instead, Phase 1 helper integrations should explicitly:

1. reuse the existing active robot they already hold
2. move to `profile.park_pose()` using borrowed active-motion utilities
3. call `disable()` only after park motion succeeds

A reusable active parking helper can be added later, but only with an API shape that makes
its error/ownership behavior explicit.

## Park Pose Source

Parking pose resolution will continue to use existing `ControlProfile` behavior:

- explicit `rest_pose_override` if configured
- otherwise `ParkOrientation::default_rest_pose()`

This keeps the source of truth single and preserves existing CLI config behavior.

No new pose registry is needed in this change.

## Park Speed Policy

Parking must not implicitly inherit `PositionModeConfig::default()`, because that would use the
current default speed percentage of 50%.

Phase 1 should add a dedicated low-speed park configuration in `ControlProfile`, for example:

- `park_speed_percent`
- and a corresponding `park_position_mode_config()`

Recommended default:

- `park_speed_percent = 5`

This keeps parking deliberately slow and aligns it with the low-risk HIL operating envelope that
is already being used manually.

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

- keep `park` as the explicit non-emergency command
- do not add a second synonymous command in Phase 1

`disable` remains direct disable.

Shell behavior in phase 1 should stay conservative:

- do not silently change existing `disable`
- do not automatically park on every successful interactive command yet

That behavior can be revisited after HIL validation.

## Failure Handling

### Parking motion fails before disable

Return an error.

Do not silently continue to disable.

Important caveat:

- standby-based `park_blocking()` can cleanly return ownership on success
- helper code that still owns an `Active` robot cannot promise "keep control with the caller" on
  every error path, because dropping `Active` triggers best-effort disable today

Phase 1 therefore must not specify stronger failure guarantees than the current ownership model
can actually provide.

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
- `park_blocking()` uses `profile.park_pose()`
- `park_position_mode_config()` uses the dedicated low-speed park config instead of the default 50%

### `piper-sdk` HIL helper tests

Add example-local tests for:

- argument parsing for `--no-park`
- success-path planning helper chooses parking by default
- opt-out disables parking stage

### CLI

Add command tests for:

- `park` documentation / command flow continues to resolve profile-driven parking config correctly

### Documentation

Update:

- HIL operator runbook
- motion validation matrix
- CLI README

to distinguish:

- `stop`
- `disable`
- `park`

## Rollout Plan

1. Add dedicated low-speed park config to `ControlProfile` / `piper-control`.
2. Reuse the existing `park_blocking()` standby workflow with that config.
3. Integrate success-path parking into `hil_joint_position_check`.
4. Update docs to clarify that `park` already implies `park -> disable`, while `disable` does not.
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

Implement parking as a shared experience-layer workflow centered in `piper-control`, reuse existing `ControlProfile::park_pose()` and `ParkOrientation`, keep the existing `park` / `park_blocking()` naming, and adopt it first in `hil_joint_position_check`.

Do not modify `piper-client::disable()`, `stop`, or drop-time disable semantics.
