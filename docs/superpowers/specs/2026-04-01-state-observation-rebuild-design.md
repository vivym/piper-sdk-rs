# State Observation Rebuild Design

## Summary

Rebuild the configuration and state observation path so the SDK no longer exposes ambiguous state through default-zero structs, ad hoc `valid_mask` semantics, or example-only validation. The new design treats observation validity as a first-class API concept, uses strict typed states in the public API, and routes malformed or semantically invalid protocol frames into a dedicated diagnostics channel instead of polluting normal state.

This is a breaking change by design. The project is pre-release, so the design optimizes for a coherent long-term model rather than backward compatibility.

## Problem Statement

The current implementation has three linked problems:

1. Different state families use different readiness semantics.
   - Some use `valid_mask`
   - Some use `is_valid`
   - Some rely on “all zeros means not initialized”
2. Semantically invalid protocol values can enter normal state.
   - Example: collision protection feedback `0x47B` can carry out-of-range levels, yet the underlying state still stores raw `u8` values as if they were valid business data.
3. Examples and metrics can mislead users.
   - A read before a query can print default-zero config that looks real.
   - “FPS” counters mix raw frame cadence with complete grouped observation cadence.
   - “any feedback received” is weaker than “the state I care about is ready”.

These issues are architectural, not isolated bugs. Fixing them permanently requires a single observation model that all relevant state paths share.

## Goals

- Make validity, completeness, and staleness explicit in the public API
- Keep semantically invalid protocol frames out of normal state
- Provide a separate diagnostics path for malformed, unsupported, or out-of-range frames
- Unify single-frame and grouped multi-frame state handling
- Replace ambiguous readiness APIs with observation-specific waiting/query APIs
- Make metrics explicit about whether they count raw frames or complete observations
- Remove default-zero-as-state semantics from configuration reads

## Non-Goals

- Rebuild the entire SDK around event sourcing
- Preserve existing public API compatibility
- Solve unrelated control-path or motion-path architectural issues
- Introduce new protocol features beyond what is required to model validity and diagnostics correctly

## Design Principles

- One observation model for all state reads
- Business state contains business data only
- Validity belongs to observation wrappers, not embedded ad hoc flags
- Invalid protocol values are diagnostics, not normal data
- Query APIs either return a fresh complete result or fail
- Metrics must state exactly what they measure

## Chosen Approach

Adopt a unified observation model with two parallel outputs from the receive pipeline:

1. Typed observations for valid business data
2. Diagnostics for malformed or semantically invalid protocol input

This is a “clean-slate observation rebuild” rather than a targeted patch. It replaces the current mixture of `valid_mask`, `is_valid`, and example-layer heuristics with a strict type-driven API.

## Alternatives Considered

### Alternative A: Minimal patching

Keep the current data structures and patch individual bugs:
- add more `is_valid` fields
- keep `valid_mask` where already present
- improve examples and comments

Rejected because it preserves fragmented semantics and leaves the next state family likely to repeat the same mistakes.

### Alternative B: Unified observation model without diagnostics split

Expose invalid values in the main typed API as `Invalid(...)`.

Rejected because it mixes business state and protocol pathology. The user explicitly prefers a dedicated raw/diagnostic channel instead of allowing invalid frames to contaminate primary state.

### Alternative C: Full event-sourced state engine

Store every frame as an event and materialize state projections on demand.

Rejected because it is much larger than the actual problem and would delay the real fix behind an infrastructure rewrite.

## Architecture

The receive path becomes:

`CAN frame -> protocol decode -> typed observation or diagnostic -> driver store -> public API`

### Protocol Layer

The protocol layer stops treating `TryFrom<PiperFrame>` as the only decode shape for stateful feedback paths that need semantic validation. For the rebuilt state families, decoding returns one of:

```rust
pub enum DecodeResult<T> {
    Data(TypedFrame<T>),
    Diagnostic(ProtocolDiagnostic),
    Ignore,
}
```

`TypedFrame<T>` contains:

- decoded typed payload
- CAN ID
- optional hardware timestamp

`ProtocolDiagnostic` captures at least:

- `InvalidLength`
- `InvalidEnum`
- `OutOfRange`
- `UnsupportedValue`
- `MalformedGroupMember`

Rules:

- Byte-level invalidity becomes `Diagnostic`
- Semantic invalidity becomes `Diagnostic`
- Only business-valid values become `Data`
- `Ignore` is reserved for frames that are irrelevant to the consumer or intentionally unsupported in that path

Query-correlation failures are out of scope for protocol decode. A valid frame
that does not satisfy the currently pending query is emitted later by the
driver/query coordination layer as a query diagnostic, not as a
`ProtocolDiagnostic`.

### Driver Observation Layer

The driver introduces a reusable observation module rather than baking validity flags into each state struct.

Core types:

```rust
pub enum Observation<TComplete, TPartial = TComplete> {
    Available(Available<TComplete, TPartial>),
    Unavailable,
}

pub struct Available<TComplete, TPartial = TComplete> {
    pub payload: ObservationPayload<TComplete, TPartial>,
    pub freshness: Freshness,
    pub meta: ObservationMeta,
}

pub enum ObservationPayload<TComplete, TPartial = TComplete> {
    Complete(TComplete),
    Partial {
        partial: TPartial,
        missing: MissingSet,
    },
}

pub enum Freshness {
    Fresh,
    Stale {
        stale_for: Duration,
    },
}

pub struct ObservationMeta {
    pub hardware_timestamp_us: Option<u64>,
    pub host_rx_mono_us: Option<u64>,
    pub source: ObservationSource,
}
```

For grouped observations, `TPartial` is not a default-filled or previously
preserved `TComplete`. It is a family-specific partial view that contains only
currently present members.
Missing members must be structurally absent from the payload, not represented by
default values and not backfilled from prior complete observations.

This makes completeness and freshness orthogonal:

- `Observation::Unavailable` means no usable observation exists
- `Observation::Available { payload: Complete(_), freshness: Fresh }` means fully ready
- `Observation::Available { payload: Partial { .. }, freshness: Fresh }` means incomplete but current
- `Observation::Available { payload: Complete(_), freshness: Stale { .. } }` means complete but aged out
- `Observation::Available { payload: Partial { .. }, freshness: Stale { .. } }` means both incomplete and aged out

No precedence rule is needed because “partial” and “stale” are no longer
competing enum variants.

Driver storage is split into two reusable primitives:

- `SingleFrameStore<T>`
- `FrameGroupStore<TSlot, const N: usize, TAssembled, TPartial = Vec<Option<TSlot>>>`

Responsibilities of `FrameGroupStore`:

- store the latest typed slot value for each member
- track per-slot timestamps
- assemble a business value only from valid slot members
- report `Available` or `Unavailable`, with completeness and freshness evaluated independently
- expose missing-slot information
- support query freshness boundaries so query APIs can demand post-query data

`FrameGroupStore` is allowed to expose a distinct `TPartial` because grouped
state is assembled from slot members whose partial shape is not generally the
same as the final complete business value. `Vec<Option<TSlot>>` is an internal
store-level default partial representation only. It is not part of the frozen
public getter surface for the first migration wave. Public getters for rebuilt
families must expose named domain-specific partial types where this spec names
them.

For single-frame observations, `ObservationPayload::Partial` is not used and the
default shorthand `Observation<TComplete>` remains sufficient.

### Business State Types

Business state structs stop carrying validity flags and masks. They only describe valid domain data.

Examples:

```rust
pub struct CollisionProtection {
    pub levels: [CollisionProtectionLevel; 6],
}

pub struct JointLimitConfig {
    pub joints: [JointLimit; 6],
}

pub struct JointLimit {
    pub min_angle_rad: f64,
    pub max_angle_rad: f64,
    pub max_velocity_rad_s: f64,
}
```

`CollisionProtectionLevel` is a typed domain enum/value type constrained to valid levels only. Out-of-range raw bytes never create this type.

Grouped rebuilt families also define named partial-domain types, for example:

- `PartialJointLimitConfig`
- `PartialJointAccelConfig`
- `PartialJointDriverLowSpeed`
- `PartialEndPose`

These types are allowed to differ structurally from their corresponding complete
business types. A partial type should model “only the members currently
present,” not “a complete value with defaults filled in.”

### Diagnostics Channel

Diagnostics are stored and exposed independently from normal state.

Minimum API:

- `subscribe_diagnostics() -> Receiver<DiagnosticEvent>`
- `snapshot_diagnostics() -> Vec<DiagnosticEvent>`

Where:

```rust
pub enum DiagnosticEvent {
    Protocol(ProtocolDiagnostic),
    Query(QueryDiagnostic),
}
```

`QueryDiagnostic` captures driver/query-layer anomalies such as:

- valid frame observed but not applicable to the pending query
- wrong joint/member set received for the current query
- query window expired after diagnostics-only traffic

Implementation detail:

- driver keeps a bounded ring buffer for recent diagnostics
- optional subscribers receive diagnostics fan-out events

This allows:

- CLI/tools/examples to surface protocol anomalies
- tests to assert that invalid frames are observed
- normal state APIs to remain clean and type-safe

## Public API Shape

### State Reads

Current “raw state struct” getters are replaced with observation-returning getters for the rebuilt families:

- `get_collision_protection() -> Observation<CollisionProtection>`
- `get_joint_limit_config() -> Observation<JointLimitConfig, PartialJointLimitConfig>`
- `get_joint_accel_config() -> Observation<JointAccelConfig, PartialJointAccelConfig>`
- `get_end_limit_config() -> Observation<EndLimitConfig>`
- `get_joint_driver_low_speed() -> Observation<JointDriverLowSpeed, PartialJointDriverLowSpeed>`
- `get_end_pose() -> Observation<EndPose, PartialEndPose>`

This removes the possibility of mistaking default-zero values for a real state snapshot.

### Query APIs

Active query APIs return fresh complete data or fail:

- `query_collision_protection(timeout) -> Result<Complete<CollisionProtection>, QueryError>`
- `query_joint_limit_config(timeout) -> Result<Complete<JointLimitConfig>, QueryError>`
- `query_joint_accel_config(timeout) -> Result<Complete<JointAccelConfig>, QueryError>`
- `query_end_limit_config(timeout) -> Result<Complete<EndLimitConfig>, QueryError>`

These APIs must:

- clear any prior query completion marker
- send the query
- wait only for post-query valid data
- reject incomplete, stale, or diagnostic-only outcomes

Where `Complete<T>` in query results is shorthand for a fresh complete
observation payload:

```rust
pub struct Complete<T> {
    pub value: T,
    pub meta: ObservationMeta,
}
```

Query APIs do not expose partial or stale success results.

### Waiting/Readiness APIs

Deprecate and remove ambiguous readiness helpers such as “wait for any robot feedback”.

Replace them with observation-specific waiting APIs, for example:

- `wait_for_complete_low_speed_state(timeout)`
- `wait_for_complete_end_pose(timeout)`
- `wait_for_complete_joint_limit_config(timeout)`
- `wait_for_observation(predicate, timeout)`

## Query Concurrency Policy

The first migration wave uses a single query coordinator with at most one
in-flight query across all rebuilt query families.

Rules:

- query APIs are globally serialized
- starting a second query while one is in flight is not allowed to create a second concurrent wait context
- callers receive a deterministic fail-fast contention error rather than blocking behind the current query
- because there is only one in-flight query, correlation is performed against one active query context at a time

This is intentional. Query traffic is low-frequency and operationally rare, so
the design favors deterministic semantics over parallelism. Relaxing this to
per-family or multi-query concurrency is out of scope for the first migration
wave.

The required public behavior is:

- if a query is already in flight, a second query call returns `Err(QueryError::Busy)`
- no implicit queuing is performed inside the SDK
- callers that want retry behavior must implement it explicitly outside the SDK

## Freshness and Staleness Policy

Freshness is evaluated against `host_rx_mono_us`. Hardware timestamps are used
for ordering and provenance, but not as the staleness clock because they may be
absent on some transports.

For the first migration wave:

- `joint_driver_low_speed` becomes `Stale` when any required member is older than `3 * nominal_period`
- `end_pose` becomes `Stale` when any required member is older than `3 * nominal_period`
- `collision_protection`, `joint_limit_config`, `joint_accel_config`, and `end_limit_config` do not become stale by elapsed time alone

Initial nominal periods:

- `joint_driver_low_speed`: `25ms` nominal, stale after `75ms`
- `end_pose`: `2ms` nominal, stale after `6ms`

Query-backed configuration observations follow different rules:

- before any successful query: `Unavailable`, unless an active query has already
  produced a post-query partial observation for that family
- after a successful query and before disconnect/reset/invalidation: `Complete`
- during an in-flight query waiting for post-query data: the query API may fail with timeout, but previously published complete data does not automatically become `Stale` or get replaced by an in-flight partial

Query-backed configuration getters publish observations according to these
visibility rules:

- if no successful query has ever published a complete value, the getter stays
  `Unavailable` until post-query data is present
- if no successful query has ever published a complete value and the active
  query has received only some required members, the getter may publish
  `ObservationPayload::Partial` for that query-backed family
- once a successful query has published `Complete`, later in-flight queries do
  not replace that published value with `Partial`; the previously published
  complete observation remains visible until a new query publishes a new
  complete observation or the family is explicitly invalidated
- if an in-flight query times out before publishing `Complete`, any previously
  published complete observation remains visible; otherwise the getter returns
  to `Unavailable`

For this spec, invalidation is limited to:

- explicit driver reset of the relevant observation store
- transport disconnect / session teardown
- receipt of a future protocol-defined invalidation signal for that state family

Application-level elapsed time alone does not invalidate query-backed
configuration in the first migration wave.

This design keeps staleness focused on continuously streaming observations in the
first migration wave and avoids inventing a false time-based expiry for
configuration values that are naturally query-driven.

## Metrics Redesign

The current FPS reporting mixes raw frame cadence and complete grouped observation cadence. The redesign separates them explicitly.

For rebuilt families, metrics expose:

- `raw_frame_rate`
- `complete_observation_rate`
- `diagnostic_rate`

For grouped data such as low-speed driver feedback:

- raw frame rate is expected around `6 * 40Hz`
- complete observation rate is expected around `40Hz`

Examples and tools must label these distinctly and stop using a single ambiguous “FPS” term for both concepts.

### Metrics Migration Boundary

The first migration wave introduces a new public observation metrics API for the
rebuilt families only. It does not require the implementation plan to migrate
every existing counter in the SDK.

Rules for the first migration wave:

- remove the current public `get_fps()` contract from rebuilt examples/tools
- add a new observation metrics surface that reports only rebuilt-family metrics
- legacy counters for non-migrated families may remain as internal implementation detail during transition
- the implementation plan must not expand scope to rewrite metrics for legacy state families in this wave

This keeps the metrics rewrite aligned with the observation rebuild instead of
turning it into a repository-wide telemetry migration.

## Frozen Public API Surface

The first migration wave locks the following public API surface for rebuilt
families.

State getters:

- `get_collision_protection() -> Observation<CollisionProtection>`
- `get_joint_limit_config() -> Observation<JointLimitConfig, PartialJointLimitConfig>`
- `get_joint_accel_config() -> Observation<JointAccelConfig, PartialJointAccelConfig>`
- `get_end_limit_config() -> Observation<EndLimitConfig>`
- `get_joint_driver_low_speed() -> Observation<JointDriverLowSpeed, PartialJointDriverLowSpeed>`
- `get_end_pose() -> Observation<EndPose, PartialEndPose>`

Active query methods:

- `query_collision_protection(timeout) -> Result<Complete<CollisionProtection>, QueryError>`
- `query_joint_limit_config(timeout) -> Result<Complete<JointLimitConfig>, QueryError>`
- `query_joint_accel_config(timeout) -> Result<Complete<JointAccelConfig>, QueryError>`
- `query_end_limit_config(timeout) -> Result<Complete<EndLimitConfig>, QueryError>`

Waiting methods:

- `wait_for_complete_low_speed_state(timeout) -> Result<Complete<JointDriverLowSpeed>, WaitError>`
- `wait_for_complete_end_pose(timeout) -> Result<Complete<EndPose>, WaitError>`
- `wait_for_complete_joint_limit_config(timeout) -> Result<Complete<JointLimitConfig>, WaitError>`

Diagnostics:

- `subscribe_diagnostics() -> Receiver<DiagnosticEvent>`
- `snapshot_diagnostics() -> Vec<DiagnosticEvent>`

Metrics:

- `get_observation_metrics() -> ObservationMetrics`

The implementation plan may refine internal helper types and private store
interfaces, but it must not change these public method names or return-shape
contracts without first revising this spec.

## Scope of First Migration

The first implementation wave covers the state families that exposed the current problems most clearly:

- collision protection (`0x47B`)
- joint limit config (`0x473`)
- joint accel config (`0x47C`)
- end-effector limit config (`0x478`)
- joint driver low-speed state (`0x261`-`0x266`)
- end pose (`0x2A2`-`0x2A4`)

Other state families may continue using the existing model temporarily, but the new observation module must be designed so additional state paths can migrate into it without another model change.

The explicit legacy boundary for the first migration wave is:

- `JointPositionState`
- `JointDynamicState`
- `RobotControlState`
- `GripperState`
- `SettingResponseState`
- `FirmwareVersionState`
- `MasterSlaveControlModeState`
- `MasterSlaveJointControlState`
- `MasterSlaveGripperControlState`
- combined/aligned motion helper snapshots built on top of those families

Those legacy families are out of scope for this spec and must not be pulled
into the initial implementation plan except where the new observation module
needs compatibility seams around them.

## Breaking Changes

The following are intentional breaking changes:

- remove default-zero config reads as a supported behavior
- remove `valid_mask` and `is_valid` from rebuilt public state types
- replace rebuilt getters with `Observation<TComplete, TPartial>` return types
- remove or rename readiness APIs whose semantics are weaker than observation readiness
- replace ambiguous FPS outputs with explicit metric categories

No compatibility shim is required unless implementation planning later identifies a narrow, high-value migration aid for internal examples/tests.

## Error Handling

### Query Errors

`QueryError` should distinguish:

- timeout waiting for complete post-query data
- transport/send failure
- canceled/interrupted query
- only diagnostics received, no valid data
- query contention / coordinator busy

### Diagnostic Retention

Diagnostics are not fatal by default. They are retained and surfaced, but they do not block unrelated valid state updates.

### Partial State

Partial grouped data is a first-class observation, not an implicit failure and
not a fake complete state. The caller chooses whether partial is acceptable.

For rebuilt grouped families, tests must cover all four meaningful availability
shapes:

- fresh + complete
- fresh + partial
- stale + complete
- stale + partial

## Example and Tooling Changes

`state_api_demo` and related tooling must be updated to reflect the new contracts:

- display `Unavailable`, `Partial`, `Stale`, and `Complete` explicitly
- subscribe to diagnostics and print recent anomalies separately
- stop rendering default numeric zeroes for missing configuration
- label rates as raw frame vs complete observation rates

The example should demonstrate correct usage of the new API rather than embedding fallback heuristics.

## Testing Strategy

### Protocol Tests

- valid frames decode to `DecodeResult::Data`
- malformed frames decode to `DecodeResult::Diagnostic`
- semantically invalid values decode to `DecodeResult::Diagnostic`
- out-of-range collision protection bytes never create typed collision levels

### Driver Tests

- no data yields `Observation::Unavailable`
- incomplete current group yields `Observation::Available` with `ObservationPayload::Partial`
- complete fresh group yields `Observation::Available` with `ObservationPayload::Complete` and `Freshness::Fresh`
- stale complete group yields `Observation::Available` with `ObservationPayload::Complete` and `Freshness::Stale`
- stale partial group yields `Observation::Available` with `ObservationPayload::Partial` and `Freshness::Stale`
- diagnostics do not mutate normal state
- query APIs require post-query fresh complete data
- query contention behavior is deterministic and tested

### Integration/Example Tests

- examples do not print fabricated default-zero config as if real
- metrics distinguish raw frame and complete observation rates
- collision-protection invalid frames appear in diagnostics, not primary state

### Hardware Validation

Hardware validation should confirm:

- known-valid config queries return `Complete`
- missing group members produce `Partial`
- semantically invalid responses are preserved only in diagnostics
- observed metrics match expected raw-vs-complete cadence

## Acceptance Criteria

The design is considered implemented correctly when all of the following are true:

1. Reading unqueried configuration returns `Observation::Unavailable`
2. A complete successful query returns `Result<Complete<T>, QueryError>`
3. Grouped observations model completeness and freshness independently
4. Partial grouped data is represented without default-filled business values
5. Invalid `0x47B` values never appear in normal collision-protection state
6. Invalid protocol input is observable through diagnostics
7. Examples and tools no longer print default-zero placeholders as real values
8. Metrics explicitly distinguish raw frame and complete observation rates
9. Query concurrency semantics are deterministic and documented

## Implementation Notes for Planning

Planning should break the work into at least these stages:

1. introduce protocol diagnostics and observation core types
2. migrate collision protection and config query paths
3. migrate grouped runtime observations for low-speed and end pose
4. replace readiness and metrics APIs
5. migrate examples/tests/tooling
6. remove obsolete API and state fields

The plan should also identify which remaining state families will remain on the legacy model temporarily and how that temporary boundary will be documented during migration.
