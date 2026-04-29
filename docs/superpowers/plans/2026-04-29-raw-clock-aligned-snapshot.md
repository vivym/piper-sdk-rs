# Raw Clock Aligned Snapshot Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make experimental calibrated raw-clock teleop control from time-aligned master/slave snapshots instead of latest/latest samples from independent CAN streams.

**Architecture:** Add a small alignment layer inside `piper-client` raw-clock runtime: latest snapshots still feed estimators exactly once, while buffered latest-before snapshots are selected for a configurable target time and used for controller input and skew gates. CLI/config/report/smoke plumbing exposes alignment lag, buffer miss limits, and diagnostics without touching production `StrictRealtime` or non-raw-clock teleop.

**Tech Stack:** Rust 2024, existing `piper-client` raw-clock runtime, `piper-tools` raw-clock estimator, `serde`, `clap`, CLI report JSON, Bash smoke script, cargo unit tests.

---

## Source Spec

- `docs/superpowers/specs/2026-04-29-raw-clock-aligned-snapshot-design.md`

## File Structure

- `crates/piper-client/src/dual_arm_raw_clock.rs`
  - Owns raw-clock alignment data structures, selector, runtime miss counters, selected/latest skew statistics, runtime gate integration, and unit tests.
  - Keep this work in the existing file for the first implementation because raw-clock runtime types are private and tightly coupled there. Do not split modules in this plan.
- `apps/cli/src/commands/teleop.rs`
  - Adds CLI flags and clap parsing tests.
- `apps/cli/src/teleop/config.rs`
  - Adds config-file fields, resolved settings, defaults, validation, and merge tests.
- `apps/cli/src/teleop/workflow.rs`
  - Converts resolved CLI settings into `ExperimentalRawClockConfig` and maps runtime report fields into `ReportTiming`.
- `apps/cli/src/teleop/report.rs`
  - Adds JSON report fields and human report line tests.
- `scripts/run_teleop_smoke.sh`
  - Passes explicit alignment defaults and records them in `environment.txt`.

## Task 1: Runtime Alignment Types And Selector

**Files:**
- Modify: `crates/piper-client/src/dual_arm_raw_clock.rs`

- [ ] **Step 1: Write failing selector tests**

Add tests near existing `dual_arm_raw_clock::tests`:

```rust
#[test]
fn snapshot_buffer_selects_latest_before_target_time() {
    let mut buffer = RawClockSnapshotBuffer::new(20_000);
    buffer.push(RawClockAlignedSnapshot::new(
        100_000,
        raw_clock_snapshot_for_tests(10_000, 100_000),
    ));
    buffer.push(RawClockAlignedSnapshot::new(
        105_000,
        raw_clock_snapshot_for_tests(15_000, 105_000),
    ));
    buffer.push(RawClockAlignedSnapshot::new(
        110_000,
        raw_clock_snapshot_for_tests(20_000, 110_000),
    ));

    let selected = buffer
        .latest_before_or_at(107_000)
        .expect("sample before target should be selected");

    assert_eq!(selected.feedback_time_us, 105_000);
}

#[test]
fn snapshot_buffer_never_selects_future_sample() {
    let mut buffer = RawClockSnapshotBuffer::new(20_000);
    buffer.push(RawClockAlignedSnapshot::new(
        105_000,
        raw_clock_snapshot_for_tests(15_000, 105_000),
    ));

    assert!(buffer.latest_before_or_at(104_999).is_none());
}

#[test]
fn snapshot_buffer_prunes_by_retention_window() {
    let mut buffer = RawClockSnapshotBuffer::new(10_000);
    buffer.push(RawClockAlignedSnapshot::new(
        100_000,
        raw_clock_snapshot_for_tests(10_000, 100_000),
    ));
    buffer.push(RawClockAlignedSnapshot::new(
        111_000,
        raw_clock_snapshot_for_tests(21_000, 111_000),
    ));

    assert!(buffer.latest_before_or_at(100_000).is_none());
    assert_eq!(
        buffer.latest_before_or_at(111_000).map(|sample| sample.feedback_time_us),
        Some(111_000)
    );
}
```

- [ ] **Step 2: Run tests to verify RED**

Run:

```bash
cargo test -p piper-client snapshot_buffer_ -- --test-threads=1 --nocapture
```

Expected: FAIL because `RawClockSnapshotBuffer` and `RawClockAlignedSnapshot` do not exist.

- [ ] **Step 3: Implement minimal buffer types**

Add near `ExperimentalRawClockSnapshot`:

```rust
#[derive(Debug, Clone, PartialEq)]
struct RawClockAlignedSnapshot {
    feedback_time_us: u64,
    snapshot: ExperimentalRawClockSnapshot,
}

impl RawClockAlignedSnapshot {
    fn new(feedback_time_us: u64, snapshot: ExperimentalRawClockSnapshot) -> Self {
        Self {
            feedback_time_us,
            snapshot,
        }
    }
}

#[derive(Debug, Clone)]
struct RawClockSnapshotBuffer {
    samples: VecDeque<RawClockAlignedSnapshot>,
    retention_us: u64,
}

impl RawClockSnapshotBuffer {
    fn new(retention_us: u64) -> Self {
        Self {
            samples: VecDeque::new(),
            retention_us,
        }
    }

    fn clear(&mut self) {
        self.samples.clear();
    }

    fn push(&mut self, sample: RawClockAlignedSnapshot) {
        let newest = sample.feedback_time_us;
        self.samples.push_back(sample);
        while self.samples.front().is_some_and(|front| {
            newest.saturating_sub(front.feedback_time_us) > self.retention_us
        }) {
            self.samples.pop_front();
        }
    }

    fn latest_before_or_at(&self, target_time_us: u64) -> Option<&RawClockAlignedSnapshot> {
        self.samples
            .iter()
            .rev()
            .find(|sample| sample.feedback_time_us <= target_time_us)
    }
}
```

- [ ] **Step 4: Run selector tests to verify GREEN**

Run:

```bash
cargo test -p piper-client snapshot_buffer_ -- --test-threads=1 --nocapture
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/piper-client/src/dual_arm_raw_clock.rs
git commit -m "Add raw-clock snapshot buffer"
```

## Task 2: Alignment Config In Client Runtime

**Files:**
- Modify: `crates/piper-client/src/dual_arm_raw_clock.rs`

- [ ] **Step 1: Write failing config/default tests**

Add tests near `experimental_config_rejects_bilateral_mode`:

```rust
#[test]
fn experimental_raw_clock_default_alignment_is_internally_valid() {
    let config = ExperimentalRawClockConfig::default();

    assert_eq!(config.thresholds.alignment_lag_us, 5_000);
    assert_eq!(config.thresholds.alignment_buffer_miss_consecutive_failures, 3);
    assert!(config.thresholds.alignment_lag_us < config.thresholds.last_sample_age_us);
    assert!(config.thresholds.alignment_lag_us < config.estimator_thresholds.last_sample_age_us);
    config.validate().expect("default raw-clock config should validate");
}

#[test]
fn experimental_raw_clock_config_rejects_alignment_lag_at_runtime_freshness() {
    let config = ExperimentalRawClockConfig {
        thresholds: RawClockRuntimeThresholds {
            alignment_lag_us: 20_000,
            last_sample_age_us: 20_000,
            ..RawClockRuntimeThresholds::default()
        },
        estimator_thresholds: RawClockThresholds {
            last_sample_age_us: 25_000,
            ..thresholds_for_tests()
        },
        ..ExperimentalRawClockConfig::default()
    };

    assert!(config.validate().is_err());
}

#[test]
fn experimental_raw_clock_config_rejects_alignment_lag_at_estimator_freshness() {
    let config = ExperimentalRawClockConfig {
        thresholds: RawClockRuntimeThresholds {
            alignment_lag_us: 20_000,
            last_sample_age_us: 25_000,
            ..RawClockRuntimeThresholds::default()
        },
        estimator_thresholds: RawClockThresholds {
            last_sample_age_us: 20_000,
            ..thresholds_for_tests()
        },
        ..ExperimentalRawClockConfig::default()
    };

    assert!(config.validate().is_err());
}
```

- [ ] **Step 2: Run tests to verify RED**

Run:

```bash
cargo test -p piper-client experimental_raw_clock_config_ -- --test-threads=1 --nocapture
```

Expected: FAIL because alignment fields do not exist.

- [ ] **Step 3: Add alignment fields and defaults**

Modify `RawClockRuntimeThresholds`:

```rust
pub struct RawClockRuntimeThresholds {
    pub inter_arm_skew_max_us: u64,
    pub last_sample_age_us: u64,
    pub residual_max_consecutive_failures: u32,
    pub alignment_lag_us: u64,
    pub alignment_buffer_miss_consecutive_failures: u32,
}
```

Update defaults:

```rust
impl Default for RawClockRuntimeThresholds {
    fn default() -> Self {
        Self {
            inter_arm_skew_max_us: 2_000,
            last_sample_age_us: 20_000,
            residual_max_consecutive_failures: 1,
            alignment_lag_us: 5_000,
            alignment_buffer_miss_consecutive_failures: 3,
        }
    }
}
```

Update `ExperimentalRawClockConfig::default().estimator_thresholds.last_sample_age_us` to `20_000`.

Update `ExperimentalRawClockConfig::validate()`:

```rust
if self.thresholds.alignment_lag_us == 0 {
    return Err(RawClockRuntimeError::Config(
        "alignment_lag_us must be greater than 0".to_string(),
    ));
}
if self.thresholds.alignment_buffer_miss_consecutive_failures == 0 {
    return Err(RawClockRuntimeError::Config(
        "alignment_buffer_miss_consecutive_failures must be greater than 0".to_string(),
    ));
}
if self.thresholds.alignment_lag_us >= self.thresholds.last_sample_age_us {
    return Err(RawClockRuntimeError::Config(format!(
        "alignment_lag_us {} must be less than runtime last_sample_age_us {}",
        self.thresholds.alignment_lag_us, self.thresholds.last_sample_age_us
    )));
}
if self.thresholds.alignment_lag_us >= self.estimator_thresholds.last_sample_age_us {
    return Err(RawClockRuntimeError::Config(format!(
        "alignment_lag_us {} must be less than estimator last_sample_age_us {}",
        self.thresholds.alignment_lag_us, self.estimator_thresholds.last_sample_age_us
    )));
}
```

Update all `RawClockRuntimeThresholds` literals in tests with `..RawClockRuntimeThresholds::default()` where possible.

- [ ] **Step 4: Run config tests**

Run:

```bash
cargo test -p piper-client experimental_raw_clock_config_ -- --test-threads=1 --nocapture
```

Expected: PASS.

- [ ] **Step 5: Run focused raw-clock tests**

Run:

```bash
cargo test -p piper-client dual_arm_raw_clock::tests -- --test-threads=1
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/piper-client/src/dual_arm_raw_clock.rs
git commit -m "Add raw-clock alignment thresholds"
```

## Task 3: Latest Timing And Selected Tick Construction

**Files:**
- Modify: `crates/piper-client/src/dual_arm_raw_clock.rs`

- [ ] **Step 1: Write failing tests for no re-ingest and selected age**

Add tests:

```rust
#[test]
fn selected_tick_uses_selected_skew_without_reingesting_raw_timestamps() {
    let mut timing = RawClockRuntimeTiming::new(thresholds_for_tests());
    timing.seed_ready_for_tests(
        &[
            raw_clock_snapshot_for_tests(7_000, 107_000),
            raw_clock_snapshot_for_tests(8_000, 108_000),
            raw_clock_snapshot_for_tests(9_000, 109_000),
            raw_clock_snapshot_for_tests(10_000, 110_000),
        ],
        &[
            raw_clock_snapshot_for_tests(17_000, 107_800),
            raw_clock_snapshot_for_tests(18_000, 108_800),
            raw_clock_snapshot_for_tests(19_000, 109_800),
            raw_clock_snapshot_for_tests(20_000, 110_800),
        ],
    );
    let latest = timing
        .ingest_latest_snapshots(
            &raw_clock_snapshot_for_tests(11_000, 111_000),
            &raw_clock_snapshot_for_tests(21_000, 111_800),
            112_000,
        )
        .unwrap();
    let selected = timing
        .selected_tick_from_buffered_times(&latest, 109_000, 109_800, 112_000)
        .unwrap();

    assert_eq!(selected.inter_arm_skew_us, 800);
    assert_eq!(selected.master_health.raw_timestamp_regressions, 0);
    assert_eq!(selected.slave_health.raw_timestamp_regressions, 0);
}

#[test]
fn selected_tick_uses_selected_sample_age_for_runtime_gate() {
    let mut timing = ready_timing_for_tests();
    let latest = timing
        .ingest_latest_snapshots(
            &raw_clock_snapshot_for_tests(11_000, 111_000),
            &raw_clock_snapshot_for_tests(21_000, 111_800),
            112_000,
        )
        .unwrap();
    let selected = timing
        .selected_tick_from_buffered_times(&latest, 100_000, 111_800, 112_000)
        .unwrap();

    let err = timing
        .check_tick_with_debounce(
            selected,
            RawClockRuntimeThresholds {
                last_sample_age_us: 5_000,
                ..RawClockRuntimeThresholds::default()
            },
        )
        .unwrap_err();

    assert!(matches!(
        err,
        RawClockRuntimeError::ClockUnhealthy { side: "master", .. }
    ));
}
```

- [ ] **Step 2: Run tests to verify RED**

Run:

```bash
cargo test -p piper-client selected_tick_ -- --test-threads=1 --nocapture
```

Expected: FAIL because helper methods do not exist.

- [ ] **Step 3: Add latest timing and selected tick helpers**

Add private struct:

```rust
#[derive(Debug, Clone)]
struct RawClockLatestTiming {
    master_feedback_time_us: u64,
    slave_feedback_time_us: u64,
    latest_inter_arm_skew_us: u64,
    master_health: RawClockHealth,
    slave_health: RawClockHealth,
}
```

Add fields to `RawClockTickTiming`:

```rust
pub master_selected_sample_age_us: u64,
pub slave_selected_sample_age_us: u64,
```

Update all test constructors (`tick_for_health`) to set both selected ages to `100`.

Refactor existing `tick_from_snapshots()` to call a new helper:

```rust
fn ingest_latest_snapshots(
    &mut self,
    master: &ExperimentalRawClockSnapshot,
    slave: &ExperimentalRawClockSnapshot,
    now_host_us: u64,
) -> Result<RawClockLatestTiming, RawClockRuntimeError> {
    self.ingest_snapshots(master, slave)?;
    let master_raw_us = raw_hw_us(RawClockSide::Master, master)?;
    let slave_raw_us = raw_hw_us(RawClockSide::Slave, slave)?;
    let master_feedback_time_us = self.master.map_raw_us(master_raw_us).ok_or(
        RawClockRuntimeError::EstimatorNotReady {
            side: RawClockSide::Master.as_str(),
        },
    )?;
    let slave_feedback_time_us = self.slave.map_raw_us(slave_raw_us).ok_or(
        RawClockRuntimeError::EstimatorNotReady {
            side: RawClockSide::Slave.as_str(),
        },
    )?;
    let latest_inter_arm_skew_us = master_feedback_time_us.abs_diff(slave_feedback_time_us);
    self.record_latest_skew_sample(latest_inter_arm_skew_us);
    Ok(RawClockLatestTiming {
        master_feedback_time_us,
        slave_feedback_time_us,
        latest_inter_arm_skew_us,
        master_health: self.master.health(now_host_us),
        slave_health: self.slave.health(now_host_us),
    })
}
```

For now, `record_latest_skew_sample` can call the existing `record_skew_sample`; Task 5 splits report fields.

Add:

```rust
fn selected_tick_from_buffered_times(
    &self,
    latest: &RawClockLatestTiming,
    selected_master_time_us: u64,
    selected_slave_time_us: u64,
    now_host_us: u64,
) -> Result<RawClockTickTiming, RawClockRuntimeError> {
    let master_age = now_host_us.saturating_sub(selected_master_time_us);
    let slave_age = now_host_us.saturating_sub(selected_slave_time_us);
    Ok(RawClockTickTiming {
        master_feedback_time_us: selected_master_time_us,
        slave_feedback_time_us: selected_slave_time_us,
        inter_arm_skew_us: selected_master_time_us.abs_diff(selected_slave_time_us),
        master_selected_sample_age_us: master_age,
        slave_selected_sample_age_us: slave_age,
        master_health: latest.master_health.clone(),
        slave_health: latest.slave_health.clone(),
    })
}
```

Keep `tick_from_snapshots()` for warmup and legacy tests by making it ingest latest and then call `selected_tick_from_buffered_times()` using latest times.

- [ ] **Step 4: Ensure gate checks selected sample age**

Update `RawClockRuntimeGate::check_tick()` and `RawClockRuntimeTiming::check_tick_with_debounce()` to fail if:

```rust
tick.master_selected_sample_age_us > thresholds.last_sample_age_us
tick.slave_selected_sample_age_us > thresholds.last_sample_age_us
```

The returned `ClockUnhealthy` should clone the relevant latest health and raise only the clone's `last_sample_age_us` to the selected age before returning the error. Do not mutate the `RawClockLatestTiming` health stored in the selected tick helper.

- [ ] **Step 5: Run selected tick tests**

Run:

```bash
cargo test -p piper-client selected_tick_ -- --test-threads=1 --nocapture
```

Expected: PASS.

- [ ] **Step 6: Run focused raw-clock tests**

Run:

```bash
cargo test -p piper-client dual_arm_raw_clock::tests -- --test-threads=1
```

Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/piper-client/src/dual_arm_raw_clock.rs
git commit -m "Separate raw-clock latest ingest from selected tick"
```

## Task 4: Runtime Alignment Selection And Miss Handling

**Files:**
- Modify: `crates/piper-client/src/dual_arm_raw_clock.rs`

- [ ] **Step 1: Write failing selector miss tests**

Add tests:

```rust
#[test]
fn alignment_selector_reports_target_underflow() {
    let mut timing = ready_timing_for_tests();
    let latest = timing
        .ingest_latest_snapshots(
            &raw_clock_snapshot_for_tests(11_000, 111_000),
            &raw_clock_snapshot_for_tests(21_000, 111_800),
            112_000,
        )
        .unwrap();

    let err = timing
        .select_aligned_snapshots(&latest, 200_000)
        .unwrap_err();

    assert_eq!(err.kind, AlignmentBufferMissKind::TargetUnderflow);
}

#[test]
fn alignment_selector_reports_one_side_miss() {
    let mut timing = ready_timing_for_tests();
    let latest = timing
        .ingest_latest_snapshots(
            &raw_clock_snapshot_for_tests(11_000, 111_000),
            &raw_clock_snapshot_for_tests(21_000, 111_800),
            112_000,
        )
        .unwrap();
    timing.master_alignment_buffer.push(RawClockAlignedSnapshot::new(
        110_000,
        raw_clock_snapshot_for_tests(10_000, 110_000),
    ));

    let err = timing.select_aligned_snapshots(&latest, 500).unwrap_err();

    assert_eq!(err.kind, AlignmentBufferMissKind::Slave);
}
```

Add runtime miss behavior test using `FakeRuntimeIo`:

```rust
#[test]
fn alignment_buffer_retention_uses_resolved_runtime_lag() {
    let estimator = RawClockThresholds {
        sample_gap_max_us: 5_000,
        last_sample_age_us: 20_000,
        ..thresholds_for_tests()
    };
    let runtime = RawClockRuntimeThresholds {
        alignment_lag_us: 60_000,
        last_sample_age_us: 70_000,
        ..RawClockRuntimeThresholds::default()
    };
    let mut timing = RawClockRuntimeTiming::new_with_runtime_thresholds(estimator, runtime);

    timing.master_alignment_buffer.push(RawClockAlignedSnapshot::new(
        100_000,
        raw_clock_snapshot_for_tests(10_000, 100_000),
    ));
    timing.master_alignment_buffer.push(RawClockAlignedSnapshot::new(
        160_000,
        raw_clock_snapshot_for_tests(70_000, 160_000),
    ));

    assert_eq!(
        timing
            .master_alignment_buffer
            .latest_before_or_at(100_000)
            .map(|sample| sample.feedback_time_us),
        Some(100_000)
    );
}

#[test]
fn alignment_selection_success_resets_consecutive_misses() {
    let mut timing = RawClockRuntimeTiming::new_with_runtime_thresholds(
        thresholds_for_tests(),
        RawClockRuntimeThresholds::default(),
    );

    timing.record_alignment_buffer_miss_for_tests(AlignmentBufferMissKind::Both);
    timing.record_alignment_buffer_miss_for_tests(AlignmentBufferMissKind::Master);
    assert_eq!(timing.alignment_buffer_miss_consecutive_for_tests(), 2);

    timing.record_alignment_selection_success();

    assert_eq!(timing.alignment_buffer_miss_consecutive_for_tests(), 0);
}

#[test]
fn alignment_buffer_misses_skip_commands_until_threshold() {
    let io = FakeRuntimeIo::new().with_reads([
        FakeRead::pair(raw_clock_snapshot_for_tests(31_000, 131_000), raw_clock_snapshot_for_tests(41_000, 131_800)),
        FakeRead::pair(raw_clock_snapshot_for_tests(32_000, 132_000), raw_clock_snapshot_for_tests(42_000, 132_800)),
        FakeRead::pair(raw_clock_snapshot_for_tests(33_000, 133_000), raw_clock_snapshot_for_tests(43_000, 133_800)),
        FakeRead::pair(raw_clock_snapshot_for_tests(34_000, 134_000), raw_clock_snapshot_for_tests(44_000, 134_800)),
    ]);
    let core = RawClockRuntimeCore {
        io,
        timing: ready_timing_for_tests(),
        config: ExperimentalRawClockConfig {
            frequency_hz: 100.0,
            max_iterations: Some(1),
            thresholds: RawClockRuntimeThresholds {
                alignment_lag_us: 50_000,
                alignment_buffer_miss_consecutive_failures: 3,
                last_sample_age_us: 100_000,
                ..RawClockRuntimeThresholds::default()
            },
            estimator_thresholds: RawClockThresholds {
                last_sample_age_us: 100_000,
                ..thresholds_for_tests()
            },
            ..ExperimentalRawClockConfig::default()
        },
    };
    let seen_skew = Arc::new(Mutex::new(Vec::new()));
    let controller = RecordingSkewController {
        seen_skew: seen_skew.clone(),
    };

    let exit = run_raw_clock_runtime_core(
        core,
        controller,
        ExperimentalRawClockRunConfig::default(),
    )
    .expect("runtime should return a faulted exit");

    let report = match exit {
        RawClockCoreExit::Faulted { report, .. } => report,
        RawClockCoreExit::Standby { report, .. } => panic!("expected fault, got {report:?}"),
    };
    assert_eq!(report.iterations, 0);
    assert_eq!(report.alignment_buffer_misses, 3);
    assert_eq!(report.alignment_buffer_miss_consecutive_max, 3);
    assert_eq!(report.alignment_buffer_miss_consecutive_failures, 3);
    assert!(
        report
            .last_error
            .as_deref()
            .is_some_and(|message| message.contains("alignment buffer miss"))
    );
    assert!(seen_skew.lock().expect("seen skew lock").is_empty());
}
```

- [ ] **Step 2: Run tests to verify RED**

Run:

```bash
cargo test -p piper-client alignment_ -- --test-threads=1 --nocapture
```

Expected: FAIL because selection and report fields do not exist.

- [ ] **Step 3: Add alignment miss types and buffers to timing**

Add:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlignmentBufferMissKind {
    TargetUnderflow,
    Master,
    Slave,
    Both,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AlignmentBufferMiss {
    kind: AlignmentBufferMissKind,
    target_time_us: Option<u64>,
    master_latest_time_us: Option<u64>,
    slave_latest_time_us: Option<u64>,
}

struct RawClockAlignedSelection {
    master: RawClockAlignedSnapshot,
    slave: RawClockAlignedSnapshot,
    target_time_us: u64,
}
```

Add `master_alignment_buffer`, `slave_alignment_buffer`, and miss counters to `RawClockRuntimeTiming`. Initialize buffers with `raw_clock_alignment_retention_us(estimator_thresholds, thresholds)` after Task 2 has thresholds available. If `RawClockRuntimeTiming::new()` only takes estimator thresholds, add `RawClockRuntimeTiming::new_with_runtime_thresholds(estimator, runtime)` and keep `new()` delegating to defaults for tests. Update `ExperimentalRawClockDualArmStandby::new()` to use `RawClockRuntimeTiming::new_with_runtime_thresholds(config.estimator_thresholds, config.thresholds)` so production retention uses the resolved runtime alignment config before warmup and the active runtime without discarding estimator state.

Add the alignment miss counters to `RawClockRuntimeReport` in this task because Task 4 tests assert them:

```rust
pub alignment_buffer_misses: u64,
pub alignment_buffer_miss_consecutive_max: u32,
pub alignment_buffer_miss_consecutive_failures: u32,
```

Update `reset_for_warmup()` to clear both alignment buffers and reset total/current/max alignment miss counters. Add these test-only helpers used by the tests above:

```rust
#[cfg(test)]
fn record_alignment_buffer_miss_for_tests(&mut self, kind: AlignmentBufferMissKind) {
    self.record_alignment_buffer_miss(AlignmentBufferMiss {
        kind,
        target_time_us: Some(100_000),
        master_latest_time_us: Some(105_000),
        slave_latest_time_us: Some(105_800),
    });
}

#[cfg(test)]
fn alignment_buffer_miss_consecutive_for_tests(&self) -> u32 {
    self.alignment_buffer_miss_consecutive
}
```

Add an error variant so final threshold failures produce clear report text:

```rust
#[error(
    "alignment buffer miss kind={kind:?} target_time_us={target_time_us:?} master_latest_time_us={master_latest_time_us:?} slave_latest_time_us={slave_latest_time_us:?}"
)]
RawClockRuntimeError::AlignmentBufferMiss {
    kind: AlignmentBufferMissKind,
    target_time_us: Option<u64>,
    master_latest_time_us: Option<u64>,
    slave_latest_time_us: Option<u64>,
}
```

Format it with text containing `alignment buffer miss` and the miss kind. This error is only returned once consecutive misses reach the configured threshold; earlier misses stay internal counters.

Retention helper:

```rust
fn raw_clock_alignment_retention_us(
    estimator: RawClockThresholds,
    runtime: RawClockRuntimeThresholds,
) -> u64 {
    runtime
        .alignment_lag_us
        .saturating_add(estimator.sample_gap_max_us.saturating_mul(2))
        .max(estimator.last_sample_age_us)
        .max(runtime.last_sample_age_us)
}
```

- [ ] **Step 4: Implement `select_aligned_snapshots()`**

Implement:

```rust
fn select_aligned_snapshots(
    &self,
    latest: &RawClockLatestTiming,
    alignment_lag_us: u64,
) -> Result<RawClockAlignedSelection, AlignmentBufferMiss> {
    let latest_common_time = latest
        .master_feedback_time_us
        .min(latest.slave_feedback_time_us);
    let Some(target_time_us) = latest_common_time.checked_sub(alignment_lag_us) else {
        return Err(AlignmentBufferMiss {
            kind: AlignmentBufferMissKind::TargetUnderflow,
            target_time_us: None,
            master_latest_time_us: Some(latest.master_feedback_time_us),
            slave_latest_time_us: Some(latest.slave_feedback_time_us),
        });
    };
    let master = self.master_alignment_buffer.latest_before_or_at(target_time_us);
    let slave = self.slave_alignment_buffer.latest_before_or_at(target_time_us);
    match (master, slave) {
        (Some(master), Some(slave)) => Ok(RawClockAlignedSelection {
            master: master.clone(),
            slave: slave.clone(),
            target_time_us,
        }),
        (None, None) => Err(AlignmentBufferMiss {
            kind: AlignmentBufferMissKind::Both,
            target_time_us: Some(target_time_us),
            master_latest_time_us: Some(latest.master_feedback_time_us),
            slave_latest_time_us: Some(latest.slave_feedback_time_us),
        }),
        (None, Some(_)) => Err(AlignmentBufferMiss {
            kind: AlignmentBufferMissKind::Master,
            target_time_us: Some(target_time_us),
            master_latest_time_us: Some(latest.master_feedback_time_us),
            slave_latest_time_us: Some(latest.slave_feedback_time_us),
        }),
        (Some(_), None) => Err(AlignmentBufferMiss {
            kind: AlignmentBufferMissKind::Slave,
            target_time_us: Some(target_time_us),
            master_latest_time_us: Some(latest.master_feedback_time_us),
            slave_latest_time_us: Some(latest.slave_feedback_time_us),
        }),
    }
}
```

- [ ] **Step 5: Integrate alignment into runtime loop**

In `run_raw_clock_runtime_core()`:

- Replace `tick_from_snapshots(&master, &slave, now_host_us)` with `ingest_latest_snapshots(&master, &slave, now_host_us)`.
- Push latest mapped snapshots into both alignment buffers after successful ingest.
- Call `select_aligned_snapshots(&latest, config.thresholds.alignment_lag_us)`.
- On miss:
  - call `timing.record_alignment_buffer_miss(miss)`
  - if below threshold, `continue` the loop without incrementing `iterations` and without calling controller or `submit_command`
  - if threshold reached, produce a `RawClockRuntimeExitReason::RawClockFault` report and fault shutdown.
- On selection:
  - call `timing.record_alignment_selection_success()`
  - build tick with selected times and `now_host_us`
  - gate with existing `check_tick_with_debounce`
  - build `raw_dual_arm_snapshot(&selection.master.snapshot, &selection.slave.snapshot, selected_skew)`
  - controller and submit as before.

Update existing runtime tests that assert controller skew, especially `controller_snapshot_receives_calibrated_raw_skew_not_host_receive_skew`, so they seed enough buffered history and assert the controller receives selected skew rather than latest/latest skew.

- [ ] **Step 6: Run alignment tests**

Run:

```bash
cargo test -p piper-client alignment_ -- --test-threads=1 --nocapture
```

Expected: PASS.

- [ ] **Step 7: Run focused raw-clock tests**

Run:

```bash
cargo test -p piper-client dual_arm_raw_clock::tests -- --test-threads=1
```

Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add crates/piper-client/src/dual_arm_raw_clock.rs
git commit -m "Align raw-clock runtime snapshots"
```

## Task 5: Runtime Alignment Reporting

**Files:**
- Modify: `crates/piper-client/src/dual_arm_raw_clock.rs`
- Modify: `apps/cli/src/teleop/workflow.rs`
- Modify: `apps/cli/src/teleop/report.rs`

- [ ] **Step 1: Write failing runtime report tests**

In `dual_arm_raw_clock.rs`, add:

```rust
#[test]
fn runtime_report_includes_alignment_diagnostics() {
    let mut timing = RawClockRuntimeTiming::new_with_runtime_thresholds(
        thresholds_for_tests(),
        RawClockRuntimeThresholds {
            alignment_lag_us: 5_000,
            ..RawClockRuntimeThresholds::default()
        },
    );
    timing.record_latest_skew_sample(9_000);
    timing.record_selected_skew_sample(800);
    timing.record_alignment_buffer_miss_for_tests(AlignmentBufferMissKind::Slave);

    let report = timing.report(120_000, 0, None);

    assert_eq!(report.alignment_lag_us, 5_000);
    assert_eq!(report.latest_inter_arm_skew_max_us, 9_000);
    assert_eq!(report.selected_inter_arm_skew_max_us, 800);
    assert_eq!(report.max_inter_arm_skew_us, 800);
    assert_eq!(report.alignment_buffer_misses, 1);
    assert_eq!(report.alignment_buffer_miss_consecutive_failures, 3);
}
```

In `apps/cli/src/teleop/report.rs`, add JSON and human tests mirroring existing residual-max report tests:

```rust
#[test]
fn report_serializes_raw_clock_alignment_diagnostics() {
    let sdk_report = BilateralRunReport::default();
    let mut input = sample_input(false, &sdk_report);
    input.timing = Some(ReportTiming {
        timing_source: "calibrated_hw_raw".to_string(),
        experimental: true,
        strict_realtime: false,
        master_clock_drift_ppm: None,
        slave_clock_drift_ppm: None,
        master_residual_p95_us: None,
        slave_residual_p95_us: None,
        max_estimated_inter_arm_skew_us: Some(800),
        estimated_inter_arm_skew_p95_us: Some(500),
        clock_health_failures: 0,
        master_residual_max_spikes: 0,
        slave_residual_max_spikes: 0,
        master_residual_max_consecutive_failures: 0,
        slave_residual_max_consecutive_failures: 0,
        alignment_lag_us: Some(5_000),
        latest_inter_arm_skew_max_us: Some(9_000),
        latest_inter_arm_skew_p95_us: Some(4_000),
        selected_inter_arm_skew_max_us: Some(800),
        selected_inter_arm_skew_p95_us: Some(500),
        alignment_buffer_misses: 2,
        alignment_buffer_miss_consecutive_max: 1,
        alignment_buffer_miss_consecutive_failures: 3,
    });
    let value = serde_json::to_value(TeleopJsonReport::from_run(input)).unwrap();
    assert_eq!(value["timing"]["alignment_lag_us"], 5_000);
    assert_eq!(value["timing"]["latest_inter_arm_skew_max_us"], 9_000);
    assert_eq!(value["timing"]["selected_inter_arm_skew_max_us"], 800);
}

#[test]
fn human_report_includes_raw_clock_alignment_diagnostics() {
    let mut output = Vec::new();
    let mut report = sample_json_report();
    report.timing = Some(ReportTiming {
        timing_source: "calibrated_hw_raw".to_string(),
        experimental: true,
        strict_realtime: false,
        master_clock_drift_ppm: None,
        slave_clock_drift_ppm: None,
        master_residual_p95_us: None,
        slave_residual_p95_us: None,
        max_estimated_inter_arm_skew_us: Some(800),
        estimated_inter_arm_skew_p95_us: Some(500),
        clock_health_failures: 0,
        master_residual_max_spikes: 0,
        slave_residual_max_spikes: 0,
        master_residual_max_consecutive_failures: 0,
        slave_residual_max_consecutive_failures: 0,
        alignment_lag_us: Some(5_000),
        latest_inter_arm_skew_max_us: Some(9_000),
        latest_inter_arm_skew_p95_us: Some(4_000),
        selected_inter_arm_skew_max_us: Some(800),
        selected_inter_arm_skew_p95_us: Some(500),
        alignment_buffer_misses: 2,
        alignment_buffer_miss_consecutive_max: 1,
        alignment_buffer_miss_consecutive_failures: 3,
    });

    write_human_report(&mut output, &report, Duration::from_micros(9876)).unwrap();

    let output = String::from_utf8(output).unwrap();
    assert!(output.contains("raw_clock alignment lag_us=5000"));
    assert!(output.contains("selected_skew max=800 p95=500"));
    assert!(output.contains("latest_skew max=9000 p95=4000"));
    assert!(output.contains("misses=2 consecutive_max=1"));
}
```

- [ ] **Step 2: Run tests to verify RED**

Run:

```bash
cargo test -p piper-client runtime_report_includes_alignment_diagnostics -- --test-threads=1 --nocapture
cargo test -p piper-cli raw_clock_alignment -- --nocapture
```

Expected: FAIL because skew/lag report fields and CLI timing fields are missing. Alignment miss counter fields were added in Task 4.

- [ ] **Step 3: Add report fields in `piper-client`**

Add the remaining alignment diagnostic fields to `RawClockRuntimeReport`:

```rust
pub alignment_lag_us: u64,
pub latest_inter_arm_skew_max_us: u64,
pub latest_inter_arm_skew_p95_us: u64,
pub selected_inter_arm_skew_max_us: u64,
pub selected_inter_arm_skew_p95_us: u64,
```

Split skew samples in `RawClockRuntimeTiming`:

- latest skew samples and max
- selected skew samples and max

Keep existing `max_inter_arm_skew_us` and `inter_arm_skew_p95_us` populated from selected skew.

- [ ] **Step 4: Add CLI report fields**

In `apps/cli/src/teleop/report.rs`, extend `ReportTiming` with `Option<u64>` for skew/lag values and counters for miss totals.

In `write_human_report()`, print:

```rust
if let Some(alignment_lag_us) = timing.alignment_lag_us {
    writeln!(
        out,
        "raw_clock alignment lag_us={} selected_skew max={} p95={} latest_skew max={} p95={} misses={} consecutive_max={}",
        alignment_lag_us,
        format_optional_u64(timing.selected_inter_arm_skew_max_us),
        format_optional_u64(timing.selected_inter_arm_skew_p95_us),
        format_optional_u64(timing.latest_inter_arm_skew_max_us),
        format_optional_u64(timing.latest_inter_arm_skew_p95_us),
        timing.alignment_buffer_misses,
        timing.alignment_buffer_miss_consecutive_max,
    )?;
}
```

In `apps/cli/src/teleop/workflow.rs`, map report fields from `RawClockRuntimeReport` into `ReportTiming`.

- [ ] **Step 5: Run report tests**

Run:

```bash
cargo test -p piper-client runtime_report_includes_alignment_diagnostics -- --test-threads=1 --nocapture
cargo test -p piper-cli raw_clock_alignment -- --nocapture
```

Expected: PASS.

- [ ] **Step 6: Run broader CLI report tests**

Run:

```bash
cargo test -p piper-cli teleop::report::tests -- --nocapture
cargo test -p piper-cli teleop::workflow::tests::experimental_raw_clock -- --nocapture
```

Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/piper-client/src/dual_arm_raw_clock.rs apps/cli/src/teleop/workflow.rs apps/cli/src/teleop/report.rs
git commit -m "Report raw-clock alignment diagnostics"
```

## Task 6: CLI And Config Plumbing

**Files:**
- Modify: `apps/cli/src/commands/teleop.rs`
- Modify: `apps/cli/src/teleop/config.rs`
- Modify: `apps/cli/src/teleop/workflow.rs`

- [ ] **Step 1: Write failing CLI/config tests**

In `apps/cli/src/commands/teleop.rs`, extend existing parse test:

```rust
assert_eq!(args.raw_clock_alignment_lag_us, Some(5_000));
assert_eq!(args.raw_clock_alignment_buffer_miss_consecutive_failures, Some(3));
```

Include CLI args in the parser test command:

```text
--raw-clock-alignment-lag-us 5000
--raw-clock-alignment-buffer-miss-consecutive-failures 3
```

In `apps/cli/src/teleop/config.rs`, add tests:

```rust
#[test]
fn raw_clock_alignment_cli_lag_overrides_file_value() {
    let file = TeleopConfigFile {
        raw_clock: Some(TeleopRawClockConfig {
            alignment_lag_us: Some(8_000),
            ..Default::default()
        }),
        ..Default::default()
    };
    let args = TeleopDualArmArgs {
        raw_clock_alignment_lag_us: Some(5_000),
        ..TeleopDualArmArgs::default_for_tests()
    };

    let resolved = ResolvedTeleopConfig::resolve(args, Some(file)).unwrap();

    assert_eq!(resolved.raw_clock.alignment_lag_us, 5_000);
}

#[test]
fn raw_clock_alignment_file_settings_are_used_when_cli_missing() {
    let file = TeleopConfigFile {
        raw_clock: Some(TeleopRawClockConfig {
            alignment_lag_us: Some(7_000),
            alignment_buffer_miss_consecutive_failures: Some(5),
            ..Default::default()
        }),
        ..Default::default()
    };

    let resolved =
        ResolvedTeleopConfig::resolve(TeleopDualArmArgs::default_for_tests(), Some(file)).unwrap();

    assert_eq!(resolved.raw_clock.alignment_lag_us, 7_000);
    assert_eq!(
        resolved.raw_clock.alignment_buffer_miss_consecutive_failures,
        5
    );
}

#[test]
fn raw_clock_alignment_validation_rejects_lag_at_last_sample_age() {
    let args = TeleopDualArmArgs {
        raw_clock_alignment_lag_us: Some(20_000),
        raw_clock_last_sample_age_ms: Some(20),
        ..TeleopDualArmArgs::default_for_tests()
    };

    let err = ResolvedTeleopConfig::resolve(args, None).unwrap_err();

    assert!(err.to_string().contains("alignment_lag_us"));
    assert!(err.to_string().contains("last_sample_age_ms"));
}

#[test]
fn raw_clock_alignment_validation_rejects_zero_buffer_miss_failures() {
    let args = TeleopDualArmArgs {
        raw_clock_alignment_buffer_miss_consecutive_failures: Some(0),
        ..TeleopDualArmArgs::default_for_tests()
    };

    let err = ResolvedTeleopConfig::resolve(args, None).unwrap_err();

    assert!(
        err.to_string()
            .contains("alignment_buffer_miss_consecutive_failures")
    );
}
```

- [ ] **Step 2: Run tests to verify RED**

Run:

```bash
cargo test -p piper-cli raw_clock_alignment -- --nocapture
cargo test -p piper-cli dual_arm_command_parses_experimental_calibrated_raw_options -- --nocapture
```

Expected: FAIL because CLI/config fields do not exist.

- [ ] **Step 3: Add CLI arg fields**

In `TeleopDualArmArgs` add:

```rust
#[arg(long)]
pub raw_clock_alignment_lag_us: Option<u64>,
#[arg(long)]
pub raw_clock_alignment_buffer_miss_consecutive_failures: Option<u32>,
```

Update `default_for_tests()`.

- [ ] **Step 4: Add config and resolved settings fields**

In `TeleopRawClockConfig`:

```rust
pub alignment_lag_us: Option<u64>,
pub alignment_buffer_miss_consecutive_failures: Option<u32>,
```

In `TeleopRawClockSettings`:

```rust
pub alignment_lag_us: u64,
pub alignment_buffer_miss_consecutive_failures: u32,
```

Add defaults:

```rust
pub const DEFAULT_RAW_CLOCK_ALIGNMENT_LAG_US: u64 = 5_000;
pub const DEFAULT_RAW_CLOCK_ALIGNMENT_BUFFER_MISS_CONSECUTIVE_FAILURES: u32 = 3;
pub const MAX_RAW_CLOCK_ALIGNMENT_LAG_US: u64 = 100_000;
pub const MAX_RAW_CLOCK_ALIGNMENT_BUFFER_MISS_CONSECUTIVE_FAILURES: u32 = 100;
```

Resolve CLI > file > default in `ResolvedTeleopConfig::from_args_and_file`.

Validate:

```rust
validate_u64_range("alignment_lag_us", self.alignment_lag_us, 1, MAX_RAW_CLOCK_ALIGNMENT_LAG_US)?;
validate_u32_range(
    "alignment_buffer_miss_consecutive_failures",
    self.alignment_buffer_miss_consecutive_failures,
    1,
    MAX_RAW_CLOCK_ALIGNMENT_BUFFER_MISS_CONSECUTIVE_FAILURES,
)?;
if self.alignment_lag_us >= self.last_sample_age_ms.saturating_mul(1_000) {
    bail!(
        "alignment_lag_us must be less than last_sample_age_ms; got alignment_lag_us={} and last_sample_age_ms={}",
        self.alignment_lag_us,
        self.last_sample_age_ms
    );
}
```

- [ ] **Step 5: Pass settings into client config**

In `experimental_raw_clock_config_from_settings()` set:

```rust
thresholds: RawClockRuntimeThresholds {
    inter_arm_skew_max_us: raw_clock.inter_arm_skew_max_us,
    last_sample_age_us: raw_clock.last_sample_age_ms * 1_000,
    residual_max_consecutive_failures: raw_clock.residual_max_consecutive_failures,
    alignment_lag_us: raw_clock.alignment_lag_us,
    alignment_buffer_miss_consecutive_failures:
        raw_clock.alignment_buffer_miss_consecutive_failures,
}
```

Update existing test `experimental_raw_clock_settings_convert_to_runtime_thresholds` to assert alignment values.

- [ ] **Step 6: Run CLI/config tests**

Run:

```bash
cargo test -p piper-cli raw_clock_alignment -- --nocapture
cargo test -p piper-cli dual_arm_command_parses_experimental_calibrated_raw_options -- --nocapture
cargo test -p piper-cli experimental_raw_clock_settings_convert_to_runtime_thresholds -- --nocapture
```

Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add apps/cli/src/commands/teleop.rs apps/cli/src/teleop/config.rs apps/cli/src/teleop/workflow.rs
git commit -m "Add raw-clock alignment config"
```

## Task 7: Smoke Script Defaults

**Files:**
- Modify: `scripts/run_teleop_smoke.sh`

- [ ] **Step 1: Write failing dry-run checks**

Run before editing:

```bash
DRY_RUN=1 ./scripts/run_teleop_smoke.sh | rg -- '--raw-clock-alignment-lag-us 5000'
DRY_RUN=1 ./scripts/run_teleop_smoke.sh | rg -- 'raw_clock_alignment_lag_us=5000'
DRY_RUN=1 ./scripts/run_teleop_smoke.sh | rg -- '--raw-clock-alignment-buffer-miss-consecutive-failures 3'
```

Expected: commands fail because script does not pass alignment settings.

- [ ] **Step 2: Add environment defaults**

After `RAW_CLOCK_SAMPLE_GAP_MAX_MS` add:

```bash
RAW_CLOCK_ALIGNMENT_LAG_US="${RAW_CLOCK_ALIGNMENT_LAG_US:-5000}"
RAW_CLOCK_ALIGNMENT_BUFFER_MISS_CONSECUTIVE_FAILURES="${RAW_CLOCK_ALIGNMENT_BUFFER_MISS_CONSECUTIVE_FAILURES:-3}"
```

Add to `cmd`:

```bash
--raw-clock-alignment-lag-us "${RAW_CLOCK_ALIGNMENT_LAG_US}"
--raw-clock-alignment-buffer-miss-consecutive-failures "${RAW_CLOCK_ALIGNMENT_BUFFER_MISS_CONSECUTIVE_FAILURES}"
```

Add to environment file:

```bash
echo "raw_clock_alignment_lag_us=${RAW_CLOCK_ALIGNMENT_LAG_US}"
echo "raw_clock_alignment_buffer_miss_consecutive_failures=${RAW_CLOCK_ALIGNMENT_BUFFER_MISS_CONSECUTIVE_FAILURES}"
```

- [ ] **Step 3: Verify script**

Run:

```bash
bash -n scripts/run_teleop_smoke.sh
DRY_RUN=1 ./scripts/run_teleop_smoke.sh | rg -- '--raw-clock-alignment-lag-us 5000'
DRY_RUN=1 ./scripts/run_teleop_smoke.sh | rg -- 'raw_clock_alignment_lag_us=5000'
DRY_RUN=1 ./scripts/run_teleop_smoke.sh | rg -- '--raw-clock-alignment-buffer-miss-consecutive-failures 3'
MASTER_IFACE=can1 SLAVE_IFACE=can0 MASTER_DAMPING=0.2 DRY_RUN=1 ./scripts/run_teleop_smoke.sh | rg -- '--raw-clock-inter-arm-skew-max-us 10000|--raw-clock-alignment-lag-us 5000|--master-damping 0\\.2'
```

Expected: all pass.

- [ ] **Step 4: Commit**

```bash
git add scripts/run_teleop_smoke.sh
git commit -m "Pass raw-clock alignment defaults in smoke script"
```

## Task 8: Final Verification And Hardware Acceptance Handoff

**Files:**
- No source changes expected.
- Hardware artifacts remain under timestamped `artifacts/teleop/<timestamp>/` directories and must not be committed.

- [ ] **Step 1: Run full non-hardware verification**

Run:

```bash
cargo fmt --all -- --check
cargo test -p piper-client dual_arm_raw_clock::tests -- --test-threads=1
cargo test -p piper-cli experimental_raw_clock -- --nocapture
cargo test -p piper-cli raw_clock_alignment -- --nocapture
cargo test -p piper-cli teleop::report::tests -- --nocapture
bash -n scripts/run_teleop_smoke.sh
DRY_RUN=1 ./scripts/run_teleop_smoke.sh
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

Expected: all commands pass.

- [ ] **Step 2: Check git status**

Run:

```bash
git status --short
```

Expected: clean except ignored/generated hardware artifacts.

- [ ] **Step 3: Hardware acceptance command for operator**

Do not run automatically unless the operator explicitly requests it. Provide:

```bash
MASTER_IFACE=can1 SLAVE_IFACE=can0 MASTER_DAMPING=0.2 MAX_ITERATIONS=30000 ./scripts/run_teleop_smoke.sh
```

- [ ] **Step 4: Hardware report inspection**

After the run, inspect:

```bash
jq '{
  exit,
  targets,
  timing,
  joint_motion: .metrics.joint_motion | {
    master_feedback_delta_rad,
    slave_command_delta_rad,
    slave_feedback_delta_rad
  }
}' artifacts/teleop/<timestamp>/report-smoke-hold.json
```

Expected:

- `exit.clean=true`
- `exit.reason="max_iterations"`
- `timing.clock_health_failures=0`
- `timing.selected_inter_arm_skew_p95_us` and `timing.selected_inter_arm_skew_max_us` are below the gate
- `timing.latest_inter_arm_skew_max_us` may be larger than selected skew
- `timing.alignment_buffer_miss_consecutive_max < 3`
- `master_feedback_delta_rad` equals `slave_command_delta_rad`
- `slave_feedback_delta_rad` follows within expected mechanical lag

- [ ] **Step 5: Final commit if verification fixes were needed**

If Task 8 required source fixes, commit them separately. If not, no commit is needed.
