# Raw Clock Residual Max Debounce Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make experimental calibrated raw-clock teleop tolerate isolated `residual_max_us` spikes while preserving fail-fast behavior for sustained max residual failures and all non-debounceable timing faults.

**Architecture:** Keep the estimator in `piper-tools` responsible for typed health classification, and keep policy in `piper-client` responsible for stateful runtime debounce. `piper-cli` only plumbs operator configuration and reports counters; the smoke script passes the new setting without changing production StrictRealtime paths.

**Tech Stack:** Rust 2024, `serde`, `clap`, existing `piper-tools` raw-clock estimator, `piper-client` experimental dual-arm raw-clock runtime, `piper-cli` teleop config/report flow, Bash smoke script, cargo unit tests.

---

## Scope Check

This plan implements the reviewed spec:

- `docs/superpowers/specs/2026-04-29-raw-clock-residual-max-debounce-design.md`

The scope is one connected change across three layers:

- `piper-tools`: classify raw-clock health failures with a typed enum.
- `piper-client`: debounce only isolated `ResidualMax` failures at runtime.
- `piper-cli` and `scripts`: expose configuration, report counters, and pass script defaults.

Out of scope:

- Do not redesign the raw-clock estimator.
- Do not add trace logging.
- Do not change production `StrictRealtime`.
- Do not debounce p95, skew, drift, sample gap, freshness, or raw timestamp regressions.

## File Structure

Modify:

- `crates/piper-tools/src/raw_clock.rs`: add `RawClockUnhealthyKind`, attach it to `RawClockHealth`, and classify health failures in the required priority order.
- `crates/piper-client/src/dual_arm_raw_clock.rs`: add runtime threshold/counters, stateful debounce checks, startup counter reset, report fields, and unit tests.
- `apps/cli/src/commands/teleop.rs`: add `--raw-clock-residual-max-consecutive-failures`.
- `apps/cli/src/teleop/config.rs`: add raw-clock config field, resolved setting, default, validation, and config tests.
- `apps/cli/src/teleop/workflow.rs`: pass the setting into `ExperimentalRawClockConfig`, update `RawClockHealth` literals, and map report counters into CLI timing.
- `apps/cli/src/teleop/report.rs`: serialize and print residual-max spike counters.
- `crates/piper-sdk/examples/socketcan_raw_clock_probe.rs`: update probe example/test `RawClockHealth` literals so workspace examples continue compiling after the new field is added.
- `scripts/run_teleop_smoke.sh`: add `RAW_CLOCK_RESIDUAL_MAX_CONSECUTIVE_FAILURES` default and pass the new CLI arg.

Do not create new Rust modules. The touched files are already the ownership boundaries for this behavior.

## Task 1: Add Typed Raw-Clock Health Failure Classification

**Files:**
- Modify: `crates/piper-tools/src/raw_clock.rs`
- Modify: `crates/piper-client/src/dual_arm_raw_clock.rs`
- Modify: `apps/cli/src/teleop/workflow.rs`
- Modify: `crates/piper-sdk/examples/socketcan_raw_clock_probe.rs`

- [ ] **Step 1: Add failing tests for failure kinds**

In `crates/piper-tools/src/raw_clock.rs`, add tests near the existing raw-clock health tests:

```rust
#[test]
fn health_reports_residual_max_failure_kind_when_only_max_exceeds_threshold() {
    let mut estimator = RawClockEstimator::new(RawClockThresholds {
        residual_p95_us: 10_000,
        residual_max_us: 100,
        drift_abs_ppm: 1_000_000.0,
        ..RawClockThresholds::for_tests()
    });

    for i in 0..8 {
        estimator
            .push(RawClockSample {
                raw_us: 10_000 + i * 1_000,
                host_rx_mono_us: 110_000 + i * 1_000,
            })
            .unwrap();
    }
    estimator
        .push(RawClockSample {
            raw_us: 18_500,
            host_rx_mono_us: 118_800,
        })
        .unwrap();

    let health = estimator.health(118_800);

    assert!(!health.healthy);
    assert_eq!(health.failure_kind, Some(RawClockUnhealthyKind::ResidualMax));
    assert!(health.reason.as_deref().unwrap().contains("residual max"));
}

#[test]
fn health_reports_residual_p95_failure_kind_before_residual_max() {
    let mut estimator = RawClockEstimator::new(RawClockThresholds {
        residual_p95_us: 10,
        residual_max_us: 20,
        drift_abs_ppm: 1_000_000.0,
        ..RawClockThresholds::for_tests()
    });

    for i in 0..8 {
        let raw_base = 10_000 + i * 1_000;
        let host_base = 110_000 + i * 1_000;
        estimator
            .push(RawClockSample {
                raw_us: raw_base,
                host_rx_mono_us: host_base,
            })
            .unwrap();
        estimator
            .push(RawClockSample {
                raw_us: raw_base + 100,
                host_rx_mono_us: host_base + 500,
            })
            .unwrap();
    }

    let health = estimator.health(118_500);

    assert!(!health.healthy);
    assert!(health.residual_p95_us > 10);
    assert!(health.residual_max_us > 20);
    assert_eq!(health.failure_kind, Some(RawClockUnhealthyKind::ResidualP95));
    assert!(health.reason.as_deref().unwrap().contains("residual p95"));
}
```

- [ ] **Step 2: Run the focused failing tests**

Run:

```bash
cargo test -p piper-tools raw_clock::tests::health_reports_residual -- --nocapture
```

Expected: compile failure because `RawClockUnhealthyKind` and `RawClockHealth::failure_kind` do not exist.

- [ ] **Step 3: Add `RawClockUnhealthyKind` and health field**

In `crates/piper-tools/src/raw_clock.rs`, add after `RawClockThresholds`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RawClockUnhealthyKind {
    FitUnavailable,
    WarmupSamples,
    WarmupWindow,
    ResidualP95,
    ResidualMax,
    Drift,
    SampleGap,
    LastSampleAge,
    RawTimestampRegression,
}
```

Extend `RawClockHealth`:

```rust
pub failure_kind: Option<RawClockUnhealthyKind>,
```

Replace the string-only `unhealthy_reason` path with a single helper that returns both kind and reason. Use a small private struct:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
struct RawClockUnhealthyState {
    kind: RawClockUnhealthyKind,
    reason: String,
}
```

Implementation rule: the helper must check failures in this priority order:

1. fit unavailable
2. warmup sample count
3. warmup window duration
4. residual p95
5. drift
6. sample gap
7. last sample age
8. raw timestamp regressions
9. residual max

That means `ResidualMax` must move below drift/gap/age/regression in the current code.

- [ ] **Step 4: Return both fields from `health()`**

In `RawClockEstimator::health()`:

```rust
let unhealthy = self.unhealthy_state(metrics);

RawClockHealth {
    healthy: unhealthy.is_none(),
    ...
    failure_kind: unhealthy.as_ref().map(|state| state.kind),
    reason: unhealthy.map(|state| state.reason),
}
```

- [ ] **Step 5: Update existing `RawClockHealth` literals**

Run:

```bash
rg -n "RawClockHealth \\{" crates apps
```

Update every literal outside `crates/piper-tools/src/raw_clock.rs` with:

```rust
failure_kind: None,
```

At minimum, update these known files:

- `crates/piper-client/src/dual_arm_raw_clock.rs`: `healthy_for_tests()`
- `apps/cli/src/teleop/workflow.rs`: `empty_raw_clock_health_for_error()` and `raw_clock_health_for_tests()`
- `crates/piper-sdk/examples/socketcan_raw_clock_probe.rs`: `healthy_raw_clock_health_for_tests()`, inline test health literals, `healthy_raw_clock_health()`, and `empty_raw_clock_health()`

Do not assign a concrete kind in these helper literals unless the helper is intentionally constructing an unhealthy estimator result. Existing generic "empty" or "healthy" helpers should use `None`.

- [ ] **Step 6: Run piper-tools tests and workspace compile check**

Run:

```bash
cargo test -p piper-tools raw_clock::tests::health_reports_residual -- --nocapture
cargo test -p piper-tools
cargo check --workspace --all-targets
```

Expected: all commands pass. The workspace check specifically catches missed `RawClockHealth` literals in examples and downstream crates.

- [ ] **Step 7: Commit**

```bash
git add crates/piper-tools/src/raw_clock.rs crates/piper-client/src/dual_arm_raw_clock.rs apps/cli/src/teleop/workflow.rs crates/piper-sdk/examples/socketcan_raw_clock_probe.rs
git commit -m "Classify raw-clock health failures"
```

## Task 2: Add Stateful Residual-Max Debounce in `piper-client`

**Files:**
- Modify: `crates/piper-client/src/dual_arm_raw_clock.rs`

- [ ] **Step 1: Add failing runtime debounce tests**

In `crates/piper-client/src/dual_arm_raw_clock.rs`, update the test imports:

```rust
use piper_tools::raw_clock::{RawClockHealth, RawClockThresholds, RawClockUnhealthyKind};
```

Confirm `healthy_for_tests()` already includes the field from Task 1:

```rust
failure_kind: None,
```

Add this helper in the test module:

```rust
fn unhealthy_for_tests(kind: RawClockUnhealthyKind, reason: &str) -> RawClockHealth {
    RawClockHealth {
        healthy: false,
        failure_kind: Some(kind),
        reason: Some(reason.to_string()),
        ..healthy_for_tests()
    }
}

fn unhealthy_without_kind_for_tests(reason: &str) -> RawClockHealth {
    RawClockHealth {
        healthy: false,
        failure_kind: None,
        reason: Some(reason.to_string()),
        ..healthy_for_tests()
    }
}

fn tick_for_health(master_health: RawClockHealth, slave_health: RawClockHealth) -> RawClockTickTiming {
    RawClockTickTiming {
        master_feedback_time_us: 100_000,
        slave_feedback_time_us: 100_500,
        inter_arm_skew_us: 500,
        master_health,
        slave_health,
    }
}
```

Add tests:

```rust
#[test]
fn residual_max_single_spike_is_counted_without_faulting_before_limit() {
    let mut timing = RawClockRuntimeTiming::new(thresholds_for_tests());
    let thresholds = RawClockRuntimeThresholds {
        residual_max_consecutive_failures: 3,
        ..RawClockRuntimeThresholds::for_tests()
    };

    let result = timing.check_tick_with_debounce(
        tick_for_health(
            unhealthy_for_tests(RawClockUnhealthyKind::ResidualMax, "residual max 3001us exceeds threshold 3000us"),
            healthy_for_tests(),
        ),
        thresholds,
    );

    assert!(result.is_ok());
    assert_eq!(timing.master_residual_max_spikes, 1);
    assert_eq!(timing.master_residual_max_consecutive_failures, 1);
}

#[test]
fn residual_max_faults_after_configured_consecutive_failures() {
    let mut timing = RawClockRuntimeTiming::new(thresholds_for_tests());
    let thresholds = RawClockRuntimeThresholds {
        residual_max_consecutive_failures: 2,
        ..RawClockRuntimeThresholds::for_tests()
    };

    let first = timing.check_tick_with_debounce(
        tick_for_health(
            unhealthy_for_tests(RawClockUnhealthyKind::ResidualMax, "residual max 3001us exceeds threshold 3000us"),
            healthy_for_tests(),
        ),
        thresholds,
    );
    assert!(first.is_ok());

    let err = timing
        .check_tick_with_debounce(
            tick_for_health(
                unhealthy_for_tests(RawClockUnhealthyKind::ResidualMax, "residual max 3002us exceeds threshold 3000us"),
                healthy_for_tests(),
            ),
            thresholds,
        )
        .unwrap_err();

    assert!(matches!(err, RawClockRuntimeError::ClockUnhealthy { side: "master", .. }));
    assert_eq!(timing.master_residual_max_spikes, 2);
    assert_eq!(timing.master_residual_max_consecutive_failures, 2);
}

#[test]
fn healthy_tick_resets_residual_max_consecutive_counter() {
    let mut timing = RawClockRuntimeTiming::new(thresholds_for_tests());
    let thresholds = RawClockRuntimeThresholds {
        residual_max_consecutive_failures: 2,
        ..RawClockRuntimeThresholds::for_tests()
    };

    timing
        .check_tick_with_debounce(
            tick_for_health(
                unhealthy_for_tests(RawClockUnhealthyKind::ResidualMax, "residual max"),
                healthy_for_tests(),
            ),
            thresholds,
        )
        .unwrap();
    timing
        .check_tick_with_debounce(tick_for_health(healthy_for_tests(), healthy_for_tests()), thresholds)
        .unwrap();

    assert_eq!(timing.master_residual_max_consecutive_failures, 0);
    assert_eq!(timing.master_residual_max_spikes, 1);
}

#[test]
fn non_residual_max_failure_resets_counter_and_fails_immediately() {
    let mut timing = RawClockRuntimeTiming::new(thresholds_for_tests());
    let thresholds = RawClockRuntimeThresholds {
        residual_max_consecutive_failures: 3,
        ..RawClockRuntimeThresholds::for_tests()
    };

    timing
        .check_tick_with_debounce(
            tick_for_health(
                unhealthy_for_tests(RawClockUnhealthyKind::ResidualMax, "residual max"),
                healthy_for_tests(),
            ),
            thresholds,
        )
        .unwrap();

    let err = timing
        .check_tick_with_debounce(
            tick_for_health(
                unhealthy_for_tests(RawClockUnhealthyKind::ResidualP95, "residual p95"),
                healthy_for_tests(),
            ),
            thresholds,
        )
        .unwrap_err();

    assert!(matches!(err, RawClockRuntimeError::ClockUnhealthy { side: "master", .. }));
    assert_eq!(timing.master_residual_max_consecutive_failures, 0);
}

#[test]
fn mixed_same_tick_failure_counts_residual_max_side_before_fault() {
    let mut timing = RawClockRuntimeTiming::new(thresholds_for_tests());
    let thresholds = RawClockRuntimeThresholds {
        residual_max_consecutive_failures: 3,
        ..RawClockRuntimeThresholds::for_tests()
    };

    let err = timing
        .check_tick_with_debounce(
            tick_for_health(
                unhealthy_for_tests(RawClockUnhealthyKind::ResidualMax, "residual max"),
                unhealthy_for_tests(RawClockUnhealthyKind::ResidualP95, "residual p95"),
            ),
            thresholds,
        )
        .unwrap_err();

    assert!(matches!(err, RawClockRuntimeError::ClockUnhealthy { side: "slave", .. }));
    assert_eq!(timing.master_residual_max_spikes, 1);
    assert_eq!(timing.master_residual_max_consecutive_failures, 1);
}

#[test]
fn dual_residual_max_tick_updates_both_sides_before_threshold_error() {
    let mut timing = RawClockRuntimeTiming::new(thresholds_for_tests());
    let thresholds = RawClockRuntimeThresholds {
        residual_max_consecutive_failures: 1,
        ..RawClockRuntimeThresholds::for_tests()
    };

    let err = timing
        .check_tick_with_debounce(
            tick_for_health(
                unhealthy_for_tests(RawClockUnhealthyKind::ResidualMax, "master residual max"),
                unhealthy_for_tests(RawClockUnhealthyKind::ResidualMax, "slave residual max"),
            ),
            thresholds,
        )
        .unwrap_err();

    assert!(matches!(err, RawClockRuntimeError::ClockUnhealthy { .. }));
    assert_eq!(timing.master_residual_max_spikes, 1);
    assert_eq!(timing.slave_residual_max_spikes, 1);
    assert_eq!(timing.master_residual_max_consecutive_failures, 1);
    assert_eq!(timing.slave_residual_max_consecutive_failures, 1);
}

#[test]
fn final_pre_run_gate_bypasses_residual_max_debounce() {
    let gate = RawClockRuntimeGate::new(RawClockRuntimeThresholds {
        residual_max_consecutive_failures: 3,
        ..RawClockRuntimeThresholds::for_tests()
    });

    let err = gate
        .check_tick(tick_for_health(
            unhealthy_for_tests(RawClockUnhealthyKind::ResidualMax, "residual max"),
            healthy_for_tests(),
        ))
        .unwrap_err();

    assert!(matches!(err, RawClockRuntimeError::ClockUnhealthy { side: "master", .. }));
}

#[test]
fn unknown_unhealthy_kind_fails_immediately_and_resets_counter() {
    let mut timing = RawClockRuntimeTiming::new(thresholds_for_tests());
    let thresholds = RawClockRuntimeThresholds {
        residual_max_consecutive_failures: 3,
        ..RawClockRuntimeThresholds::for_tests()
    };

    timing
        .check_tick_with_debounce(
            tick_for_health(
                unhealthy_for_tests(RawClockUnhealthyKind::ResidualMax, "residual max"),
                healthy_for_tests(),
            ),
            thresholds,
        )
        .unwrap();

    let err = timing
        .check_tick_with_debounce(
            tick_for_health(
                unhealthy_without_kind_for_tests("legacy unknown unhealthy reason"),
                healthy_for_tests(),
            ),
            thresholds,
        )
        .unwrap_err();

    assert!(matches!(err, RawClockRuntimeError::ClockUnhealthy { side: "master", .. }));
    assert_eq!(timing.master_residual_max_consecutive_failures, 0);
}

#[test]
fn inter_arm_skew_remains_fail_fast_with_debounce_gate() {
    let mut timing = RawClockRuntimeTiming::new(thresholds_for_tests());
    let thresholds = RawClockRuntimeThresholds {
        inter_arm_skew_max_us: 2_000,
        residual_max_consecutive_failures: 3,
        ..RawClockRuntimeThresholds::for_tests()
    };

    let err = timing
        .check_tick_with_debounce(
            RawClockTickTiming {
                master_feedback_time_us: 100_000,
                slave_feedback_time_us: 103_001,
                inter_arm_skew_us: 3_001,
                master_health: healthy_for_tests(),
                slave_health: healthy_for_tests(),
            },
            thresholds,
        )
        .unwrap_err();

    assert!(matches!(err, RawClockRuntimeError::InterArmSkew { .. }));
}

#[test]
fn stale_sample_age_remains_fail_fast_even_when_health_is_otherwise_healthy() {
    let mut timing = RawClockRuntimeTiming::new(thresholds_for_tests());
    let thresholds = RawClockRuntimeThresholds {
        last_sample_age_us: 20,
        residual_max_consecutive_failures: 3,
        ..RawClockRuntimeThresholds::for_tests()
    };
    let mut stale_master = healthy_for_tests();
    stale_master.last_sample_age_us = 21;

    let err = timing
        .check_tick_with_debounce(
            tick_for_health(stale_master, healthy_for_tests()),
            thresholds,
        )
        .unwrap_err();

    assert!(matches!(err, RawClockRuntimeError::ClockUnhealthy { side: "master", .. }));
    assert_eq!(timing.master_residual_max_consecutive_failures, 0);
}

#[test]
fn reset_runtime_residual_max_counters_clears_warmup_state() {
    let mut timing = RawClockRuntimeTiming::new(thresholds_for_tests());
    let thresholds = RawClockRuntimeThresholds {
        residual_max_consecutive_failures: 3,
        ..RawClockRuntimeThresholds::for_tests()
    };

    timing
        .check_tick_with_debounce(
            tick_for_health(
                unhealthy_for_tests(RawClockUnhealthyKind::ResidualMax, "residual max"),
                healthy_for_tests(),
            ),
            thresholds,
        )
        .unwrap();
    timing.reset_runtime_residual_max_counters();

    assert_eq!(timing.master_residual_max_spikes, 0);
    assert_eq!(timing.master_residual_max_consecutive_failures, 0);
}
```

- [ ] **Step 2: Run focused tests to verify they fail**

Run:

```bash
cargo test -p piper-client residual_max -- --test-threads=1 --nocapture
```

Expected: compile failures for missing `RawClockUnhealthyKind` import consumers, new counters, and `check_tick_with_debounce`.

- [ ] **Step 3: Extend runtime thresholds and reports**

In `RawClockRuntimeThresholds`, add:

```rust
pub residual_max_consecutive_failures: u32,
```

Set defaults:

```rust
impl Default for RawClockRuntimeThresholds {
    fn default() -> Self {
        Self {
            inter_arm_skew_max_us: 2_000,
            last_sample_age_us: 5_000,
            residual_max_consecutive_failures: 1,
        }
    }
}
```

Keep this lower-layer default at `1` intentionally. SDK/runtime defaults should
preserve existing fail-fast behavior unless a caller explicitly opts into
debounce. The operator-facing CLI/config default becomes `3` in Task 3 and is
passed into `RawClockRuntimeThresholds` only for the experimental teleop command.

Set `for_tests()` to `1` unless a test overrides it:

```rust
residual_max_consecutive_failures: 1,
```

In `RawClockRuntimeReport`, add:

```rust
pub master_residual_max_spikes: u64,
pub slave_residual_max_spikes: u64,
pub master_residual_max_consecutive_failures: u32,
pub slave_residual_max_consecutive_failures: u32,
```

- [ ] **Step 4: Add debounce state and reset helper**

In `RawClockRuntimeTiming`, add fields:

```rust
master_residual_max_spikes: u64,
slave_residual_max_spikes: u64,
master_residual_max_consecutive_failures: u32,
slave_residual_max_consecutive_failures: u32,
```

Initialize them in `new()`. Reset them in `reset_for_warmup()`.

Add:

```rust
fn reset_runtime_residual_max_counters(&mut self) {
    self.master_residual_max_spikes = 0;
    self.slave_residual_max_spikes = 0;
    self.master_residual_max_consecutive_failures = 0;
    self.slave_residual_max_consecutive_failures = 0;
}
```

- [ ] **Step 5: Implement `check_tick_with_debounce`**

Add a method on `RawClockRuntimeTiming`:

```rust
fn check_tick_with_debounce(
    &mut self,
    tick: RawClockTickTiming,
    thresholds: RawClockRuntimeThresholds,
) -> Result<(), RawClockRuntimeError> {
    if tick.inter_arm_skew_us > thresholds.inter_arm_skew_max_us {
        return Err(RawClockRuntimeError::InterArmSkew {
            inter_arm_skew_us: tick.inter_arm_skew_us,
            max_us: thresholds.inter_arm_skew_max_us,
        });
    }

    let master_error = self.apply_health_with_debounce(RawClockSide::Master, &tick.master_health, thresholds);
    let slave_error = self.apply_health_with_debounce(RawClockSide::Slave, &tick.slave_health, thresholds);

    master_error.or(slave_error).map_or(Ok(()), Err)
}
```

Add a private helper with this behavior:

- if `health.last_sample_age_us > thresholds.last_sample_age_us`: reset that side's consecutive counter and return `ClockUnhealthy` immediately, even when `health.healthy` is `true`
- healthy side: reset that side's consecutive counter, return `None`
- `failure_kind == Some(RawClockUnhealthyKind::ResidualMax)`: increment spike and consecutive counters; return `ClockUnhealthy` only when consecutive count reaches `thresholds.residual_max_consecutive_failures`
- any other unhealthy kind or `None`: reset that side's consecutive counter and return `ClockUnhealthy`

Use `saturating_add` for counters.

- [ ] **Step 6: Preserve fail-fast final pre-run gate and reset before runtime**

Keep `RawClockRuntimeGate::check_tick()` as fail-fast. It is used for final pre-run checks and should reject even a single `ResidualMax`.

In `ExperimentalRawClockDualArmStandby::warmup()`, after:

```rust
RawClockRuntimeGate::new(self.config.thresholds).check_tick(final_tick)?;
```

add:

```rust
self.timing.reset_runtime_residual_max_counters();
```

This ensures warmup/refresh counters do not pollute runtime reporting or the first runtime failure decision.

- [ ] **Step 7: Use debounce in the active runtime loop**

In `run_raw_clock_runtime_core()`, replace:

```rust
if let Err(err) = RawClockRuntimeGate::new(config.thresholds).check_tick(tick) {
```

with:

```rust
if let Err(err) = timing.check_tick_with_debounce(tick, config.thresholds) {
```

Keep the existing fault/report path. It should still call `timing.record_clock_health_failure()` when the returned error is `ClockUnhealthy`.

- [ ] **Step 8: Include counters in reports**

In `RawClockRuntimeTiming::report()`, populate the four new `RawClockRuntimeReport` fields from `self`.

Update every `RawClockRuntimeReport` literal in tests and helper functions with zeros until Task 4 maps them into CLI reports.

- [ ] **Step 9: Run piper-client tests**

Run:

```bash
cargo test -p piper-client residual_max -- --test-threads=1 --nocapture
cargo test -p piper-client -- --test-threads=1
```

Expected: both commands pass.

- [ ] **Step 10: Commit**

```bash
git add crates/piper-client/src/dual_arm_raw_clock.rs
git commit -m "Debounce raw-clock residual max faults"
```

## Task 3: Add CLI and Config Plumbing

**Files:**
- Modify: `apps/cli/src/commands/teleop.rs`
- Modify: `apps/cli/src/teleop/config.rs`
- Modify: `apps/cli/src/teleop/workflow.rs`

- [ ] **Step 1: Add failing config tests**

In `apps/cli/src/teleop/config.rs`, add tests near the existing raw-clock validation tests:

```rust
#[test]
fn cli_residual_max_consecutive_failures_overrides_file_value() {
    let file = TeleopConfigFile {
        raw_clock: Some(TeleopRawClockConfig {
            residual_max_consecutive_failures: Some(9),
            ..Default::default()
        }),
        ..Default::default()
    };
    let args = TeleopDualArmArgs {
        raw_clock_residual_max_consecutive_failures: Some(3),
        ..TeleopDualArmArgs::default_for_tests()
    };

    let resolved = ResolvedTeleopConfig::resolve(args, Some(file)).unwrap();

    assert_eq!(resolved.raw_clock.residual_max_consecutive_failures, 3);
}

#[test]
fn file_residual_max_consecutive_failures_is_used_when_cli_missing() {
    let file = TeleopConfigFile {
        raw_clock: Some(TeleopRawClockConfig {
            residual_max_consecutive_failures: Some(4),
            ..Default::default()
        }),
        ..Default::default()
    };

    let resolved =
        ResolvedTeleopConfig::resolve(TeleopDualArmArgs::default_for_tests(), Some(file)).unwrap();

    assert_eq!(resolved.raw_clock.residual_max_consecutive_failures, 4);
}

#[test]
fn raw_clock_validation_rejects_zero_residual_max_consecutive_failures() {
    let args = TeleopDualArmArgs {
        raw_clock_residual_max_consecutive_failures: Some(0),
        ..TeleopDualArmArgs::default_for_tests()
    };

    let err = ResolvedTeleopConfig::resolve(args, None).unwrap_err();
    assert!(err.to_string().contains("residual_max_consecutive_failures"));
}

#[test]
fn raw_clock_validation_rejects_large_residual_max_consecutive_failures() {
    let args = TeleopDualArmArgs {
        raw_clock_residual_max_consecutive_failures: Some(101),
        ..TeleopDualArmArgs::default_for_tests()
    };

    let err = ResolvedTeleopConfig::resolve(args, None).unwrap_err();
    assert!(err.to_string().contains("residual_max_consecutive_failures"));
}
```

- [ ] **Step 2: Run focused config tests to verify they fail**

Run:

```bash
cargo test -p piper-cli residual_max_consecutive -- --nocapture
```

Expected: compile failures for missing config/CLI fields.

- [ ] **Step 3: Add CLI arg and test default**

In `apps/cli/src/commands/teleop.rs`, add to `TeleopDualArmArgs`:

```rust
#[arg(long)]
pub raw_clock_residual_max_consecutive_failures: Option<u32>,
```

Update `TeleopDualArmArgs::default_for_tests()` with:

```rust
raw_clock_residual_max_consecutive_failures: None,
```

- [ ] **Step 4: Add config fields, constants, merge, and validation**

In `apps/cli/src/teleop/config.rs`, add:

```rust
pub const DEFAULT_RAW_CLOCK_RESIDUAL_MAX_CONSECUTIVE_FAILURES: u32 = 3;
pub const MAX_RAW_CLOCK_RESIDUAL_MAX_CONSECUTIVE_FAILURES: u32 = 100;
```

Add to `TeleopRawClockConfig`:

```rust
pub residual_max_consecutive_failures: Option<u32>,
```

Add to `TeleopRawClockSettings`:

```rust
pub residual_max_consecutive_failures: u32,
```

In `TeleopRawClockSettings::validate()`, validate:

```rust
validate_u32_range(
    "residual_max_consecutive_failures",
    self.residual_max_consecutive_failures,
    1,
    MAX_RAW_CLOCK_RESIDUAL_MAX_CONSECUTIVE_FAILURES,
)?;
```

If no `validate_u32_range` helper exists, add one beside the existing `validate_u64_range` helper:

```rust
fn validate_u32_range(name: &str, value: u32, min: u32, max: u32) -> Result<()> {
    if !(min..=max).contains(&value) {
        bail!("{name} must be between {min} and {max}; got {value}");
    }
    Ok(())
}
```

In `ResolvedTeleopConfig::resolve()`, merge CLI over file over default:

```rust
residual_max_consecutive_failures: args
    .raw_clock_residual_max_consecutive_failures
    .or_else(|| file_raw_clock.and_then(|raw_clock| raw_clock.residual_max_consecutive_failures))
    .unwrap_or(DEFAULT_RAW_CLOCK_RESIDUAL_MAX_CONSECUTIVE_FAILURES),
```

- [ ] **Step 5: Pass setting to `RawClockRuntimeThresholds`**

In `apps/cli/src/teleop/workflow.rs`, update `experimental_raw_clock_config_from_settings()`:

```rust
let thresholds = RawClockRuntimeThresholds {
    inter_arm_skew_max_us: raw_clock.inter_arm_skew_max_us,
    last_sample_age_us: raw_clock.last_sample_age_ms * 1_000,
    residual_max_consecutive_failures: raw_clock.residual_max_consecutive_failures,
};
```

Update any `TeleopRawClockSettings` literals in tests with `residual_max_consecutive_failures`.

- [ ] **Step 6: Verify no config/workflow health literal regressions**

Run:

```bash
rg -n "RawClockHealth \\{" apps/cli/src/teleop crates/piper-sdk/examples/socketcan_raw_clock_probe.rs
```

Expected: every literal includes `failure_kind`. Keep generic test/empty helpers at `None` because they are not estimator-produced typed failures.

- [ ] **Step 7: Run CLI config tests**

Run:

```bash
cargo test -p piper-cli residual_max_consecutive -- --nocapture
cargo test -p piper-cli raw_clock -- --nocapture
```

Expected: both commands pass.

- [ ] **Step 8: Commit**

```bash
git add apps/cli/src/commands/teleop.rs apps/cli/src/teleop/config.rs apps/cli/src/teleop/workflow.rs
git commit -m "Add raw-clock residual max debounce config"
```

## Task 4: Add Report and Console Visibility

**Files:**
- Modify: `apps/cli/src/teleop/report.rs`
- Modify: `apps/cli/src/teleop/workflow.rs`
- Modify: `crates/piper-client/src/dual_arm_raw_clock.rs`

- [ ] **Step 1: Add failing report serialization tests**

In `apps/cli/src/teleop/report.rs`, extend `ReportTiming` tests. Update `report_serializes_experimental_raw_clock_timing()` input:

```rust
master_residual_max_spikes: 2,
slave_residual_max_spikes: 1,
master_residual_max_consecutive_failures: 0,
slave_residual_max_consecutive_failures: 1,
```

Add assertions:

```rust
assert_eq!(value["timing"]["master_residual_max_spikes"], 2);
assert_eq!(value["timing"]["slave_residual_max_spikes"], 1);
assert_eq!(value["timing"]["slave_residual_max_consecutive_failures"], 1);
```

Add a human output test:

```rust
#[test]
fn human_report_prints_nonzero_residual_max_spikes() {
    let mut output = Vec::new();
    let mut report = sample_json_report();
    report.timing = Some(ReportTiming {
        timing_source: "calibrated_hw_raw".to_string(),
        experimental: true,
        strict_realtime: false,
        master_clock_drift_ppm: None,
        slave_clock_drift_ppm: None,
        master_residual_p95_us: Some(100),
        slave_residual_p95_us: Some(120),
        max_estimated_inter_arm_skew_us: Some(900),
        estimated_inter_arm_skew_p95_us: Some(400),
        clock_health_failures: 0,
        master_residual_max_spikes: 2,
        slave_residual_max_spikes: 0,
        master_residual_max_consecutive_failures: 1,
        slave_residual_max_consecutive_failures: 0,
    });

    write_human_report(&mut output, &report, Duration::from_micros(9876)).unwrap();

    let output = String::from_utf8(output).unwrap();
    assert!(output.contains("raw_clock residual_max_spikes master=2 slave=0"));
    assert!(output.contains("consecutive master=1 slave=0"));
}
```

- [ ] **Step 2: Run report tests to verify they fail**

Run:

```bash
cargo test -p piper-cli report_serializes_experimental_raw_clock_timing -- --nocapture
cargo test -p piper-cli human_report_prints_nonzero_residual_max_spikes -- --nocapture
```

Expected: compile failure for missing `ReportTiming` fields.

- [ ] **Step 3: Extend `ReportTiming`**

In `apps/cli/src/teleop/report.rs`, add:

```rust
pub master_residual_max_spikes: u64,
pub slave_residual_max_spikes: u64,
pub master_residual_max_consecutive_failures: u32,
pub slave_residual_max_consecutive_failures: u32,
```

Update all `ReportTiming` literals in tests with zeros unless the test needs nonzero values.

- [ ] **Step 4: Map runtime counters into CLI timing**

In `apps/cli/src/teleop/workflow.rs`, update `report_timing_from_raw_clock()`:

```rust
master_residual_max_spikes: report.master_residual_max_spikes,
slave_residual_max_spikes: report.slave_residual_max_spikes,
master_residual_max_consecutive_failures: report.master_residual_max_consecutive_failures,
slave_residual_max_consecutive_failures: report.slave_residual_max_consecutive_failures,
```

Update `RawClockRuntimeReport` literals in workflow tests with zero counter fields.

- [ ] **Step 5: Print a compact console line only when useful**

In `apps/cli/src/teleop/report.rs`, in `write_human_report()`, after the existing `raw_clock max_skew_us=...` line, print a residual-max line only when any spike/consecutive counter is nonzero:

```rust
if timing.master_residual_max_spikes > 0
    || timing.slave_residual_max_spikes > 0
    || timing.master_residual_max_consecutive_failures > 0
    || timing.slave_residual_max_consecutive_failures > 0
{
    writeln!(
        writer,
        "raw_clock residual_max_spikes master={} slave={} consecutive master={} slave={}",
        timing.master_residual_max_spikes,
        timing.slave_residual_max_spikes,
        timing.master_residual_max_consecutive_failures,
        timing.slave_residual_max_consecutive_failures
    )?;
}
```

- [ ] **Step 6: Run report and workflow tests**

Run:

```bash
cargo test -p piper-cli report_serializes_experimental_raw_clock_timing -- --nocapture
cargo test -p piper-cli human_report_prints_nonzero_residual_max_spikes -- --nocapture
cargo test -p piper-cli raw_clock -- --nocapture
```

All three commands pass.

- [ ] **Step 7: Commit**

```bash
git add apps/cli/src/teleop/report.rs apps/cli/src/teleop/workflow.rs crates/piper-client/src/dual_arm_raw_clock.rs
git commit -m "Report raw-clock residual max debounce counters"
```

## Task 5: Update Smoke Script and Run Full Verification

**Files:**
- Modify: `scripts/run_teleop_smoke.sh`

- [ ] **Step 1: Add the script setting**

In `scripts/run_teleop_smoke.sh`, after `RAW_CLOCK_RESIDUAL_MAX_US`:

```bash
RAW_CLOCK_RESIDUAL_MAX_CONSECUTIVE_FAILURES="${RAW_CLOCK_RESIDUAL_MAX_CONSECUTIVE_FAILURES:-3}"
```

In the CLI command array, after `--raw-clock-residual-max-us "${RAW_CLOCK_RESIDUAL_MAX_US}"`, add:

```bash
--raw-clock-residual-max-consecutive-failures "${RAW_CLOCK_RESIDUAL_MAX_CONSECUTIVE_FAILURES}"
```

In the DRY_RUN environment dump block, add:

```bash
echo "raw_clock_residual_max_consecutive_failures=${RAW_CLOCK_RESIDUAL_MAX_CONSECUTIVE_FAILURES}"
```

- [ ] **Step 2: Verify script syntax and dry-run output**

Run:

```bash
bash -n scripts/run_teleop_smoke.sh
DRY_RUN=1 ./scripts/run_teleop_smoke.sh | rg -- '--raw-clock-residual-max-consecutive-failures 3'
DRY_RUN=1 ./scripts/run_teleop_smoke.sh | rg -- 'raw_clock_residual_max_consecutive_failures=3'
```

Expected: all three commands exit 0.

- [ ] **Step 3: Run full formatting and tests**

Run:

```bash
cargo fmt --all -- --check
cargo test -p piper-tools
cargo test -p piper-client -- --test-threads=1
cargo test -p piper-cli
bash -n scripts/run_teleop_smoke.sh
DRY_RUN=1 ./scripts/run_teleop_smoke.sh
```

Expected: all commands exit 0. Read the output; do not continue if any command fails.

- [ ] **Step 4: Run clippy**

Run:

```bash
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

Expected: exits 0.

- [ ] **Step 5: Commit**

```bash
git add scripts/run_teleop_smoke.sh
git commit -m "Pass raw-clock residual max debounce in smoke script"
```

## Task 6: Manual Hardware Acceptance Handoff

**Files:**
- No code changes.
- Validate using generated artifacts under `artifacts/teleop/<timestamp>/`.

- [ ] **Step 1: Run a 30k iteration hardware smoke test**

Run with supported arms in the identity zero pose:

```bash
MAX_ITERATIONS=30000 ./scripts/run_teleop_smoke.sh
```

Expected:

- CLI prompts for confirmation through `/dev/tty`.
- Startup reaches `startup: starting teleop control loop...`.
- Isolated `ResidualMax` spikes below 3 consecutive failures do not abort the run.
- Any p95, skew, drift, sample-gap, freshness, or regression fault still aborts.

- [ ] **Step 2: Inspect JSON report with `jq`**

Replace `<timestamp>` with the output directory printed by the script:

```bash
jq '.timing | {
  clock_health_failures,
  master_residual_max_spikes,
  slave_residual_max_spikes,
  master_residual_max_consecutive_failures,
  slave_residual_max_consecutive_failures,
  max_estimated_inter_arm_skew_us,
  estimated_inter_arm_skew_p95_us,
  master_residual_p95_us,
  slave_residual_p95_us
}' artifacts/teleop/<timestamp>/report-smoke-hold.json
```

Expected:

- `clock_health_failures` is `0` for a clean run.
- Spike counters may be nonzero.
- Consecutive counters should be below `3` on a clean max-iterations run.

- [ ] **Step 3: Do not commit hardware artifacts**

Run:

```bash
git status --short
```

Expected: no source changes from hardware testing; generated `artifacts/` files remain untracked or ignored.
