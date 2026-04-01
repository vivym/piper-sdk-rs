# State Observation Rebuild Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace ambiguous state/config observation semantics with a strict typed observation model, a dedicated diagnostics channel, and deterministic query behavior for the first migration wave.

**Architecture:** Add protocol diagnostics and driver-side observation/diagnostic primitives first, then migrate the six rebuilt state families onto those primitives, then switch query/wait/metrics/example surfaces to the new contracts. Keep completeness and freshness orthogonal, keep invalid protocol data out of normal state, and keep query concurrency globally serialized with fail-fast busy errors.

**Tech Stack:** Rust, Cargo workspace, existing `piper-protocol` / `piper-driver` / `piper-sdk` crates, built-in unit tests, example tests, `cargo fmt`, `cargo clippy`

---

## Worktree Prerequisite

Execute this plan in a dedicated git worktree before touching Rust code. The spec was written in the main workspace, but implementation should happen in an isolated worktree so commits stay clean and reversible.

Recommended setup:

```bash
git worktree add ../piper-sdk-rs-state-observation -b feat/state-observation-rebuild
cd ../piper-sdk-rs-state-observation
```

## File Structure

### New Files

- `crates/piper-protocol/src/diagnostics.rs`
  - protocol-layer diagnostic enums and decode result wrapper for rebuilt feedback families
- `crates/piper-driver/src/observation.rs`
  - `Observation<T>`, `Available<T>`, `ObservationPayload<T>`, `Freshness`, `ObservationMeta`, reusable stores
- `crates/piper-driver/src/diagnostics.rs`
  - `DiagnosticEvent`, `QueryDiagnostic`, bounded retention buffer, subscriber fan-out
- `crates/piper-driver/src/query_coordinator.rs`
  - single in-flight query coordinator, fail-fast busy semantics, post-query freshness boundaries
- `crates/piper-sdk/tests/observation_api_tests.rs`
  - public API regression tests for the rebuilt observation/query/diagnostic surface

### Existing Files To Modify

- `crates/piper-protocol/src/lib.rs`
  - export diagnostics module and shared decode types
- `crates/piper-protocol/src/config.rs`
  - migrate rebuilt config/collision decode paths from `TryFrom<PiperFrame>`-only behavior to `DecodeResult<T>`
- `crates/piper-driver/src/lib.rs`
  - export new observation/diagnostic/query coordinator types
- `crates/piper-driver/src/state.rs`
  - replace rebuilt public business structs with validity-free domain data structs; remove rebuilt-family `valid_mask` / `is_valid`
- `crates/piper-driver/src/pipeline.rs`
  - route rebuilt feedback frames through decode -> observation store or diagnostics buffer
- `crates/piper-driver/src/piper.rs`
  - freeze public getters/query/wait methods to the spec’d signatures and wire in `QueryError::Busy`
- `crates/piper-driver/src/fps_stats.rs`
  - retire rebuilt-family ambiguous FPS counters from public use
- `crates/piper-driver/src/metrics.rs`
  - add `ObservationMetrics` public surface for rebuilt families
- `crates/piper-sdk/examples/state_api_demo.rs`
  - update demo to print `Unavailable` / `Available + Freshness` states and diagnostics
- `crates/piper-sdk/examples/README.md`
  - document the new state/diagnostic semantics for `state_api_demo`
- `crates/piper-sdk/tests/robot_protocol_tests.rs`
  - stop asserting legacy rebuilt-family state semantics

### Existing Files To Read Before Implementing

- `docs/superpowers/specs/2026-04-01-state-observation-rebuild-design.md`
- `crates/piper-protocol/src/config.rs`
- `crates/piper-driver/src/state.rs`
- `crates/piper-driver/src/pipeline.rs`
- `crates/piper-driver/src/piper.rs`
- `crates/piper-driver/src/fps_stats.rs`
- `crates/piper-driver/src/metrics.rs`
- `crates/piper-sdk/examples/state_api_demo.rs`

## Task 1: Add Protocol Diagnostics Core

**Files:**
- Create: `crates/piper-protocol/src/diagnostics.rs`
- Modify: `crates/piper-protocol/src/lib.rs`
- Modify: `crates/piper-protocol/src/config.rs`
- Test: `crates/piper-protocol/src/diagnostics.rs`
- Test: `crates/piper-protocol/src/config.rs`

- [ ] **Step 1: Write the failing protocol diagnostic tests**

```rust
#[test]
fn decode_collision_protection_out_of_range_returns_diagnostic() {
    let frame = PiperFrame::new_standard(0x47B, &[255, 0, 0, 0, 0, 0, 0, 0]);
    match decode_collision_protection_feedback(frame) {
        DecodeResult::Diagnostic(ProtocolDiagnostic::OutOfRange { field, .. }) => {
            assert_eq!(field, "collision_protection_level");
        }
        other => panic!("expected out-of-range diagnostic, got {other:?}"),
    }
}

#[test]
fn decode_motor_limit_valid_frame_returns_data() {
    let frame = PiperFrame::new_standard(0x473, &[1, 0x07, 0x08, 0xF8, 0xF8, 0x01, 0x2C, 0x00]);
    assert!(matches!(
        decode_motor_limit_feedback(frame),
        DecodeResult::Data(_)
    ));
}
```

- [ ] **Step 2: Run the protocol tests to verify they fail**

Run: `cargo test -p piper-protocol decode_collision_protection_out_of_range_returns_diagnostic -- --nocapture`

Expected: FAIL because `decode_collision_protection_feedback` and `ProtocolDiagnostic` do not exist yet.

- [ ] **Step 3: Implement the protocol diagnostics module and exports**

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProtocolDiagnostic {
    InvalidLength { can_id: u32, expected: usize, actual: usize },
    InvalidEnum { field: &'static str, raw: u8 },
    OutOfRange { field: &'static str, raw: u32, min: u32, max: u32 },
    UnsupportedValue { field: &'static str, raw: u32 },
    MalformedGroupMember { can_id: u32, member: &'static str },
}

#[derive(Debug, Clone, PartialEq)]
pub enum DecodeResult<T> {
    Data(TypedFrame<T>),
    Diagnostic(ProtocolDiagnostic),
    Ignore,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TypedFrame<T> {
    pub can_id: u32,
    pub payload: T,
    pub hardware_timestamp_us: Option<u64>,
}
```

- [ ] **Step 4: Run the focused protocol tests to verify they pass**

Run: `cargo test -p piper-protocol decode_ -- --nocapture`

Expected: PASS for the new decode-result tests.

- [ ] **Step 5: Commit**

```bash
git add crates/piper-protocol/src/diagnostics.rs crates/piper-protocol/src/lib.rs crates/piper-protocol/src/config.rs
git commit -m "feat: add protocol diagnostics for rebuilt state decode"
```

## Task 2: Add Driver Observation Core

**Files:**
- Create: `crates/piper-driver/src/observation.rs`
- Modify: `crates/piper-driver/src/lib.rs`
- Test: `crates/piper-driver/src/observation.rs`

- [ ] **Step 1: Write the failing observation core tests**

```rust
#[test]
fn group_store_can_return_stale_partial_observation() {
    let mut store = FrameGroupStore::<u8, 3, [u8; 3]>::new();
    store.record_slot(0, 10, 1_000, Some(10));
    let observation = store.observe(1_100, 50, |slots| [slots[0]?, slots[1]?, slots[2]?]);

    match observation {
        Observation::Available(available) => {
            assert!(matches!(available.payload, ObservationPayload::Partial { .. }));
            assert!(matches!(available.freshness, Freshness::Stale { .. }));
        }
        other => panic!("expected available partial stale, got {other:?}"),
    }
}

#[test]
fn single_frame_store_never_returns_partial() {
    let mut store = SingleFrameStore::new();
    store.record(42u8, 100, Some(99));
    let observation = store.observe(100, 50);
    assert!(matches!(
        observation,
        Observation::Available(Available {
            payload: ObservationPayload::Complete(42),
            freshness: Freshness::Fresh,
            ..
        })
    ));
}
```

- [ ] **Step 2: Run the driver observation tests to verify they fail**

Run: `cargo test -p piper-driver group_store_can_return_stale_partial_observation -- --nocapture`

Expected: FAIL because the observation module and stores do not exist yet.

- [ ] **Step 3: Implement observation core types and reusable stores**

```rust
pub enum Observation<T> {
    Available(Available<T>),
    Unavailable,
}

pub struct Available<T> {
    pub payload: ObservationPayload<T>,
    pub freshness: Freshness,
    pub meta: ObservationMeta,
}

pub enum ObservationPayload<T> {
    Complete(T),
    Partial { partial: PartialPayload<T>, missing: MissingSet },
}

pub struct Complete<T> {
    pub value: T,
    pub meta: ObservationMeta,
}

pub trait PartialPayload<T>: Sized {
    fn from_present_slots(slots: &[Option<T>]) -> Self;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissingSet {
    pub missing_indices: Vec<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObservationSource {
    Stream,
    Query,
}
```

- [ ] **Step 4: Run the observation test set**

Run: `cargo test -p piper-driver observation -- --nocapture`

Expected: PASS for the new observation-core tests.

- [ ] **Step 5: Commit**

```bash
git add crates/piper-driver/src/observation.rs crates/piper-driver/src/lib.rs
git commit -m "feat: add driver observation core types"
```

## Task 3: Add Driver Diagnostics Buffer And Query Coordinator

**Files:**
- Create: `crates/piper-driver/src/diagnostics.rs`
- Create: `crates/piper-driver/src/query_coordinator.rs`
- Modify: `crates/piper-driver/src/lib.rs`
- Test: `crates/piper-driver/src/diagnostics.rs`
- Test: `crates/piper-driver/src/query_coordinator.rs`

- [ ] **Step 1: Write the failing diagnostics/query coordinator tests**

```rust
#[test]
fn diagnostics_buffer_retains_recent_events() {
    let mut buffer = DiagnosticBuffer::new(2);
    buffer.push(DiagnosticEvent::Query(QueryDiagnostic::Busy));
    buffer.push(DiagnosticEvent::Protocol(ProtocolDiagnostic::UnsupportedValue {
        field: "x",
        raw: 7,
    }));
    assert_eq!(buffer.snapshot().len(), 2);
}

#[test]
fn query_coordinator_is_fail_fast_when_busy() {
    let coordinator = QueryCoordinator::new();
    let _guard = coordinator.try_begin(QueryKind::JointLimit).unwrap();
    let err = coordinator.try_begin(QueryKind::CollisionProtection).unwrap_err();
    assert_eq!(err, QueryError::Busy);
}
```

- [ ] **Step 2: Run the focused tests to verify they fail**

Run: `cargo test -p piper-driver query_coordinator_is_fail_fast_when_busy -- --nocapture`

Expected: FAIL because `QueryCoordinator`, `QueryKind`, `DiagnosticBuffer`, and `QueryDiagnostic` do not exist.

- [ ] **Step 3: Implement the bounded diagnostics buffer and single-flight query coordinator**

```rust
pub enum QueryDiagnostic {
    Busy,
    UnexpectedFrameForActiveQuery { query: QueryKind, can_id: u32 },
    DiagnosticsOnlyTimeout { query: QueryKind },
}

pub struct QueryCoordinator {
    in_flight: Mutex<Option<QueryKind>>,
}

pub struct QueryGuard<'a> {
    slot: MutexGuard<'a, Option<QueryKind>>,
}

impl QueryCoordinator {
    pub fn try_begin(&self, kind: QueryKind) -> Result<QueryGuard<'_>, QueryError> {
        let mut slot = self.in_flight.lock().unwrap();
        if slot.is_some() {
            return Err(QueryError::Busy);
        }
        *slot = Some(kind);
        Ok(QueryGuard { slot })
    }
}

impl Piper {
    pub fn subscribe_diagnostics(&self) -> Receiver<DiagnosticEvent> {
        self.ctx.diagnostics.subscribe()
    }

    pub fn snapshot_diagnostics(&self) -> Vec<DiagnosticEvent> {
        self.ctx.diagnostics.snapshot()
    }
}
```

- [ ] **Step 4: Run the diagnostics/query coordinator tests**

Run: `cargo test -p piper-driver diagnostics_buffer_retains_recent_events -- --nocapture`

Expected: PASS for the new diagnostics/query coordinator tests.

- [ ] **Step 5: Commit**

```bash
git add crates/piper-driver/src/diagnostics.rs crates/piper-driver/src/query_coordinator.rs crates/piper-driver/src/lib.rs
git commit -m "feat: add observation diagnostics buffer and query coordinator"
```

## Task 4: Migrate Collision Protection And Config Families

**Files:**
- Modify: `crates/piper-driver/src/state.rs`
- Modify: `crates/piper-driver/src/pipeline.rs`
- Modify: `crates/piper-driver/src/piper.rs`
- Modify: `crates/piper-sdk/tests/robot_protocol_tests.rs`
- Test: `crates/piper-driver/src/state.rs`
- Test: `crates/piper-driver/src/piper.rs`
- Test: `crates/piper-sdk/tests/robot_protocol_tests.rs`

- [ ] **Step 1: Write the failing rebuilt-family driver tests**

```rust
#[test]
fn collision_protection_invalid_frame_goes_to_diagnostics_not_state() {
    let (piper, diagnostics) = build_test_piper_with_diagnostics();
    queue_frame(&piper, PiperFrame::new_standard(0x47B, &[255, 0, 0, 0, 0, 0, 0, 0]));

    assert!(matches!(piper.get_collision_protection(), Observation::Unavailable));
    assert!(diagnostics.snapshot().iter().any(|event| matches!(
        event,
        DiagnosticEvent::Protocol(ProtocolDiagnostic::OutOfRange { .. })
    )));
}

#[test]
fn unqueried_joint_limit_config_is_unavailable() {
    let piper = build_test_piper();
    assert!(matches!(piper.get_joint_limit_config(), Observation::Unavailable));
}
```

- [ ] **Step 2: Run the focused rebuilt-family tests to verify they fail**

Run: `cargo test -p piper-driver unqueried_joint_limit_config_is_unavailable -- --nocapture`

Expected: FAIL because rebuilt getters still return legacy state structs.

- [ ] **Step 3: Implement business structs and observation-backed getters/query methods**

```rust
pub struct CollisionProtection {
    pub levels: [CollisionProtectionLevel; 6],
}

pub fn get_joint_limit_config(&self) -> Observation<JointLimitConfig> {
    self.ctx.joint_limit_store.observe(now_host_mono_us())
}

pub fn query_joint_limit_config(
    &self,
    timeout: Duration,
) -> Result<Complete<JointLimitConfig>, QueryError> {
    let _guard = self.query_coordinator.try_begin(QueryKind::JointLimit)?;
    self.clear_joint_limit_store();
    self.send_joint_limit_queries()?;
    self.wait_for_complete_joint_limit_config(timeout)
}

pub fn query_joint_accel_config(
    &self,
    timeout: Duration,
) -> Result<Complete<JointAccelConfig>, QueryError> {
    let _guard = self.query_coordinator.try_begin(QueryKind::JointAccel)?;
    self.clear_joint_accel_store();
    self.send_joint_accel_queries()?;
    self.wait_for_complete_joint_accel_config(timeout)
}

pub fn query_end_limit_config(
    &self,
    timeout: Duration,
) -> Result<Complete<EndLimitConfig>, QueryError> {
    let _guard = self.query_coordinator.try_begin(QueryKind::EndLimit)?;
    self.clear_end_limit_store();
    self.send_end_limit_query()?;
    self.wait_for_complete_end_limit_config(timeout)
}
```

- [ ] **Step 4: Run the rebuilt-family test set**

Run: `cargo test -p piper-driver collision_protection_ -- --nocapture`

Run: `cargo test -p piper-driver joint_limit_config -- --nocapture`

Expected: PASS for collision/config observation and query tests.

- [ ] **Step 5: Commit**

```bash
git add crates/piper-driver/src/state.rs crates/piper-driver/src/pipeline.rs crates/piper-driver/src/piper.rs crates/piper-sdk/tests/robot_protocol_tests.rs
git commit -m "feat: migrate collision and config observations"
```

## Task 5: Migrate Low-Speed And End-Pose Grouped Runtime Observations

**Files:**
- Modify: `crates/piper-driver/src/state.rs`
- Modify: `crates/piper-driver/src/pipeline.rs`
- Modify: `crates/piper-driver/src/piper.rs`
- Test: `crates/piper-driver/src/state.rs`
- Test: `crates/piper-driver/src/pipeline.rs`

- [ ] **Step 1: Write the failing grouped observation tests**

```rust
#[test]
fn low_speed_group_can_be_partial_and_stale_at_once() {
    let piper = build_test_piper();
    inject_low_speed_joint(&piper, 1, /*host*/ 1_000, /*hw*/ Some(100));
    let observation = piper.get_joint_driver_low_speed_at(1_200);

    match observation {
        Observation::Available(available) => {
            assert!(matches!(available.payload, ObservationPayload::Partial { .. }));
            assert!(matches!(available.freshness, Freshness::Stale { .. }));
        }
        other => panic!("expected available observation, got {other:?}"),
    }
}

#[test]
fn low_speed_group_can_be_partial_and_fresh() {
    let piper = build_test_piper();
    inject_low_speed_joint(&piper, 1, /*host*/ 1_000, /*hw*/ Some(100));
    let observation = piper.get_joint_driver_low_speed_at(1_010);

    match observation {
        Observation::Available(available) => {
            assert!(matches!(available.payload, ObservationPayload::Partial { .. }));
            assert!(matches!(available.freshness, Freshness::Fresh));
        }
        other => panic!("expected fresh partial observation, got {other:?}"),
    }
}

#[test]
fn complete_end_pose_wait_returns_complete_payload() {
    let piper = build_test_piper();
    inject_end_pose_group(&piper);
    let result = piper.wait_for_complete_end_pose(Duration::from_millis(10)).unwrap();
    assert!(result.meta.host_rx_mono_us.is_some());
}

#[test]
fn low_speed_group_can_be_complete_and_stale() {
    let piper = build_test_piper();
    inject_all_low_speed_joints(&piper, /*host*/ 1_000);
    let observation = piper.get_joint_driver_low_speed_at(1_200);

    match observation {
        Observation::Available(available) => {
            assert!(matches!(available.payload, ObservationPayload::Complete(_)));
            assert!(matches!(available.freshness, Freshness::Stale { .. }));
        }
        other => panic!("expected complete stale observation, got {other:?}"),
    }
}

#[test]
fn wait_for_complete_low_speed_state_returns_complete_payload() {
    let piper = build_test_piper();
    inject_all_low_speed_joints(&piper, /*host*/ 1_000);
    let result = piper
        .wait_for_complete_low_speed_state(Duration::from_millis(10))
        .unwrap();
    assert!(result.meta.host_rx_mono_us.is_some());
}
```

- [ ] **Step 2: Run the focused grouped observation tests to verify they fail**

Run: `cargo test -p piper-driver low_speed_group_can_be_partial_and_stale_at_once -- --nocapture`

Run: `cargo test -p piper-driver low_speed_group_can_be_partial_and_fresh -- --nocapture`

Expected: FAIL because low-speed/end-pose still use legacy grouped state semantics.

- [ ] **Step 3: Replace rebuilt grouped stores with `FrameGroupStore`**

```rust
self.ctx.low_speed_store.record_slot(
    joint_index,
    typed_slot,
    host_rx_mono_us,
    nonzero_hw_timestamp(frame.timestamp_us),
);

self.ctx.end_pose_store.record_slot(
    member_index,
    typed_member,
    host_rx_mono_us,
    nonzero_hw_timestamp(frame.timestamp_us),
);
```

- [ ] **Step 4: Run grouped observation tests**

Run: `cargo test -p piper-driver low_speed_ -- --nocapture`

Run: `cargo test -p piper-driver end_pose_ -- --nocapture`

Run: `cargo test -p piper-driver wait_for_complete_low_speed_state -- --nocapture`

Expected: PASS for grouped completeness/freshness tests and wait helpers.

- [ ] **Step 5: Commit**

```bash
git add crates/piper-driver/src/state.rs crates/piper-driver/src/pipeline.rs crates/piper-driver/src/piper.rs
git commit -m "feat: migrate grouped runtime observations"
```

## Task 6: Replace Rebuilt Metrics Surface

**Files:**
- Modify: `crates/piper-driver/src/fps_stats.rs`
- Modify: `crates/piper-driver/src/metrics.rs`
- Modify: `crates/piper-driver/src/lib.rs`
- Test: `crates/piper-driver/src/metrics.rs`

- [ ] **Step 1: Write the failing observation metrics tests**

```rust
#[test]
fn observation_metrics_separate_raw_and_complete_rates() {
    let metrics = ObservationMetrics::default();
    metrics.low_speed_raw_frames.fetch_add(6, Ordering::Relaxed);
    metrics.low_speed_complete_observations.fetch_add(1, Ordering::Relaxed);

    let snapshot = metrics.snapshot(Duration::from_secs(1));
    assert_eq!(snapshot.low_speed.raw_frame_rate, 6.0);
    assert_eq!(snapshot.low_speed.complete_observation_rate, 1.0);
}
```

- [ ] **Step 2: Run the observation metrics test to verify it fails**

Run: `cargo test -p piper-driver observation_metrics_separate_raw_and_complete_rates -- --nocapture`

Expected: FAIL because `ObservationMetrics` does not exist yet.

- [ ] **Step 3: Add the rebuilt-family observation metrics surface**

```rust
pub struct FamilyObservationMetrics {
    pub raw_frame_rate: f64,
    pub complete_observation_rate: f64,
    pub diagnostic_rate: f64,
}

pub struct ObservationMetrics {
    pub low_speed: FamilyObservationMetrics,
    pub end_pose: FamilyObservationMetrics,
    pub collision_protection: FamilyObservationMetrics,
    pub joint_limit_config: FamilyObservationMetrics,
    pub joint_accel_config: FamilyObservationMetrics,
    pub end_limit_config: FamilyObservationMetrics,
}

impl Piper {
    pub fn get_observation_metrics(&self) -> ObservationMetrics {
        self.ctx.observation_metrics.snapshot(self.ctx.metrics_window.elapsed())
    }
}
```

- [ ] **Step 4: Run the rebuilt metrics tests**

Run: `cargo test -p piper-driver observation_metrics_ -- --nocapture`

Expected: PASS for rebuilt-family metrics tests.

- [ ] **Step 5: Commit**

```bash
git add crates/piper-driver/src/fps_stats.rs crates/piper-driver/src/metrics.rs crates/piper-driver/src/lib.rs
git commit -m "feat: add observation metrics for rebuilt families"
```

## Task 7: Update Public Examples And Public API Regression Tests

**Files:**
- Modify: `crates/piper-sdk/examples/state_api_demo.rs`
- Modify: `crates/piper-sdk/examples/README.md`
- Create: `crates/piper-sdk/tests/observation_api_tests.rs`
- Test: `crates/piper-sdk/tests/observation_api_tests.rs`
- Test: `crates/piper-sdk/examples/state_api_demo.rs`

- [ ] **Step 1: Write the failing example/API tests**

```rust
#[test]
fn state_api_demo_formats_unavailable_without_fake_zero_values() {
    let output = render_joint_limit_section(Observation::Unavailable);
    assert!(output.contains("Unavailable"));
    assert!(!output.contains("0.00 rad/s"));
}

#[test]
fn busy_query_returns_query_error_busy() {
    let piper = build_test_piper_with_active_query(QueryKind::JointLimit);
    let err = piper.query_collision_protection(Duration::from_millis(1)).unwrap_err();
    assert_eq!(err, QueryError::Busy);
}

#[test]
fn diagnostics_snapshot_and_subscription_expose_protocol_events() {
    let (piper, rx) = build_test_piper_with_diagnostic_subscription();
    inject_invalid_collision_frame(&piper);

    assert!(piper
        .snapshot_diagnostics()
        .iter()
        .any(|event| matches!(event, DiagnosticEvent::Protocol(_))));
    assert!(matches!(
        rx.recv_timeout(Duration::from_millis(10)),
        Ok(DiagnosticEvent::Protocol(_))
    ));
}
```

- [ ] **Step 2: Run the public API/example tests to verify they fail**

Run: `cargo test -p piper-sdk busy_query_returns_query_error_busy -- --nocapture`

Expected: FAIL because the example and public surface still use legacy contracts.

- [ ] **Step 3: Update the example output and public regression tests**

```rust
match robot.get_joint_limit_config() {
    Observation::Unavailable => println!("Joint limits: Unavailable"),
    Observation::Available(available) => render_joint_limit_observation(&available),
}

let metrics = robot.get_observation_metrics();
println!("low-speed raw rate: {:.2}", metrics.low_speed.raw_frame_rate);

for event in robot.snapshot_diagnostics() {
    println!("diagnostic: {event}");
}

let _diag_rx = robot.subscribe_diagnostics();
```

- [ ] **Step 4: Run example/public regression coverage**

Run: `cargo test -p piper-sdk --test observation_api_tests -- --nocapture`

Run: `cargo test -p piper-sdk --example state_api_demo -- --nocapture`

Expected: PASS for the new public observation semantics.

- [ ] **Step 5: Commit**

```bash
git add crates/piper-sdk/examples/state_api_demo.rs crates/piper-sdk/examples/README.md crates/piper-sdk/tests/observation_api_tests.rs
git commit -m "feat: update state api demo for observation rebuild"
```

## Task 8: Remove Legacy Rebuilt-Family Contracts And Run Workspace Verification

**Files:**
- Modify: `crates/piper-driver/src/state.rs`
- Modify: `crates/piper-driver/src/piper.rs`
- Modify: `crates/piper-sdk/tests/robot_protocol_tests.rs`
- Modify: `crates/piper-driver/src/lib.rs`
- Modify: `crates/piper-sdk/tests/observation_api_tests.rs`
- Test: workspace-wide verification

- [ ] **Step 1: Remove the legacy rebuilt-family APIs/fields and finish cleanup**

```rust
#[test]
fn queried_joint_limit_config_stays_complete_until_invalidation() {
    let piper = build_test_piper();
    inject_complete_joint_limit_query_response(&piper);
    let _ = piper.query_joint_limit_config(Duration::from_millis(10)).unwrap();

    let observation = piper.get_joint_limit_config();
    assert!(matches!(
        observation,
        Observation::Available(Available {
            payload: ObservationPayload::Complete(_),
            freshness: Freshness::Fresh,
            ..
        })
    ));
}

#[test]
fn query_timeout_does_not_invalidate_prior_complete_joint_limit_config() {
    let piper = build_test_piper();
    inject_complete_joint_limit_query_response(&piper);
    let _ = piper.query_joint_limit_config(Duration::from_millis(10)).unwrap();

    let err = piper.query_joint_limit_config(Duration::from_millis(1)).unwrap_err();
    assert!(matches!(err, QueryError::Timeout));

    let observation = piper.get_joint_limit_config();
    assert!(matches!(
        observation,
        Observation::Available(Available {
            payload: ObservationPayload::Complete(_),
            freshness: Freshness::Fresh,
            ..
        })
    ));
}

// Delete rebuilt-family `valid_mask` / `is_valid` fields from public business structs.
// Delete or rename legacy getters that return those structs directly.
// Remove rebuilt-family assertions from robot_protocol_tests that depend on old semantics.
// Add a regression test proving query-backed config stays complete after a successful
// query until explicit invalidation occurs.
```

- [ ] **Step 2: Run full verification**

Run: `cargo fmt --all -- --check`
Expected: PASS

Run: `cargo clippy --all-targets --all-features -- -D warnings`
Expected: PASS

Run: `cargo test -p piper-protocol`
Expected: PASS

Run: `cargo test -p piper-driver`
Expected: PASS

Run: `cargo test -p piper-sdk --test observation_api_tests -- --nocapture`
Expected: PASS

Run: `cargo test -p piper-sdk --example state_api_demo -- --nocapture`
Expected: PASS

Run: `cargo test -p piper-sdk diagnostics_snapshot_and_subscription_expose_protocol_events -- --nocapture`
Expected: PASS

Run: `cargo test -p piper-sdk query_timeout_does_not_invalidate_prior_complete_joint_limit_config -- --nocapture`
Expected: PASS

Run: `cargo check --all-targets`
Expected: PASS

- [ ] **Step 3: Commit**

```bash
git add crates/piper-protocol/src crates/piper-driver/src crates/piper-sdk/examples crates/piper-sdk/tests
git commit -m "refactor: remove legacy rebuilt observation contracts"
```

## Notes For The Implementer

- Use @superpowers:test-driven-development before each implementation task. The steps above assume the classic write-fail-pass cycle.
- Use @superpowers:verification-before-completion before claiming the rebuild is done.
- Do not expand scope into `JointPositionState`, `JointDynamicState`, `RobotControlState`, `GripperState`, firmware state, or master/slave state families in this wave.
- Do not add implicit query queuing. `QueryError::Busy` is the required external behavior.
- Keep `docs/superpowers/specs/2026-04-01-state-observation-rebuild-design.md` as the source of truth for public API behavior. If implementation uncovers a needed public contract change, revise the spec first, then update the plan.
