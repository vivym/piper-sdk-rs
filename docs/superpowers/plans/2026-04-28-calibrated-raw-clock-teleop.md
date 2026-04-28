# Calibrated Raw Clock Teleop Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build an explicitly experimental GS-USB dual-arm `master-follower` teleop path using calibrated SocketCAN `hw_raw` timestamps, without weakening existing `StrictRealtime` behavior.

**Architecture:** Keep production StrictRealtime teleop unchanged. Add raw timestamp extraction in `piper-can`, a pure raw-clock estimator in `piper-tools`, a read-only probe example in `piper-sdk`, and an experimental SoftRealtime dual-arm runtime in `piper-client` that `piper-cli` enables only with `--experimental-calibrated-raw`.

**Tech Stack:** Rust 2024, SocketCAN `SCM_TIMESTAMPING`, `piper-can`, `piper-tools`, `piper-client` type-state API, `piper-cli` clap/serde report flow, cargo unit/example tests, manual HIL probe/teleop checks.

---

## Scope Check

This plan implements the reviewed spec:

- `docs/superpowers/specs/2026-04-28-calibrated-raw-clock-teleop-design.md`

It is a single connected feature with clear crate boundaries. It touches several crates, but each task produces independently testable software:

- `piper-tools`: pure estimator with unit tests.
- `piper-can`: raw timestamp samples with unit tests and existing SocketCAN behavior preserved.
- `piper-sdk`: read-only probe example.
- `piper-client`: experimental SoftRealtime command/runtime path.
- `piper-cli`: opt-in flag, config, workflow, reports, and docs.

Out of scope:

- Do not redefine `StrictRealtime`.
- Do not make calibrated raw timing satisfy `StrictCapability`.
- Do not support bilateral force reflection in this path.
- Do not require a prior probe artifact for teleop startup.
- Do not add MuJoCo or network teleop.

## File Structure

Create:

- `crates/piper-tools/src/raw_clock.rs`: pure raw-clock estimator, thresholds, health metrics, reportable stats.
- `crates/piper-can/src/raw_timestamp.rs`: shared raw timestamp sample structs that do not depend on SocketCAN internals.
- `crates/piper-sdk/examples/socketcan_raw_clock_probe.rs`: read-only two-interface probe tool.
- `crates/piper-client/src/dual_arm_raw_clock.rs`: experimental SoftRealtime dual-arm calibrated raw-clock runtime.

Modify:

- `crates/piper-tools/src/lib.rs`: export `raw_clock`.
- `crates/piper-can/src/lib.rs`: export raw timestamp structs.
- `crates/piper-can/src/socketcan/split.rs`: preserve `hw_raw`, expose raw timestamp sample reads, keep existing startup classification unchanged.
- `crates/piper-can/src/socketcan/mod.rs`: keep adapter behavior aligned with split RX timestamp extraction, expose raw timestamp details if direct adapter APIs need them.
- `crates/piper-driver/src/state.rs`: carry the newest raw-timestamped feedback frame timing that contributed to motion snapshots.
- `crates/piper-driver/src/heartbeat.rs`: delegate driver monotonic time to the shared `piper-can` monotonic helper so raw timestamp samples and driver feedback ages share one epoch.
- `crates/piper-driver/src/pipeline.rs`: propagate raw timestamp metadata from `ReceivedFrame` into position and dynamic feedback groups.
- `crates/piper-driver/src/piper.rs`: expose raw feedback timing through `AlignedMotionState` without changing strict/soft capability classification.
- `crates/piper-client/src/lib.rs`: export experimental raw-clock dual-arm types.
- `crates/piper-client/src/state/machine.rs`: add a typed SoftRealtime MIT passthrough helper that accepts position/velocity/kp/kd/torque arrays and uses confirmed SoftRealtime delivery.
- `apps/cli/src/commands/teleop.rs`: add experimental flag and threshold CLI args.
- `apps/cli/src/teleop/config.rs`: add raw-clock config merge and validation.
- `apps/cli/src/teleop/workflow.rs`: route experimental calibrated-raw runs to a separate workflow branch.
- `apps/cli/src/teleop/report.rs`: add timing/report fields.
- `apps/cli/TELEOP_DUAL_ARM.md`: document experimental calibrated raw-clock path and its non-production status.
- `crates/piper-sdk/Cargo.toml`: add `serde` as an example/test dependency and add an explicit `[[example]]` entry for `socketcan_raw_clock_probe` when this Cargo manifest uses explicit example metadata; otherwise leave example metadata unchanged and rely on Cargo's default example discovery.
- `crates/piper-sdk/examples/README.md`: list the probe example.

Do not move the existing `crates/piper-client/src/dual_arm.rs` file in this plan. It is large, but the safer first step is to add a focused `dual_arm_raw_clock.rs` module that reuses public dual-arm types.

## Task 1: Add Pure Raw-Clock Estimator

**Files:**
- Create: `crates/piper-tools/src/raw_clock.rs`
- Modify: `crates/piper-tools/src/lib.rs`
- Test: `crates/piper-tools/src/raw_clock.rs`

- [ ] **Step 1: Add failing estimator tests**

Create `crates/piper-tools/src/raw_clock.rs` with tests first. Start with these tests and minimal type references that do not compile yet:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn sample(raw_us: u64, host_us: u64) -> RawClockSample {
        RawClockSample {
            raw_us,
            host_rx_mono_us: host_us,
        }
    }

    #[test]
    fn stable_clock_becomes_healthy_and_maps_raw_to_host() {
        let thresholds = RawClockThresholds {
            warmup_samples: 4,
            warmup_window_us: 3_000,
            residual_p95_us: 20,
            residual_max_us: 50,
            drift_abs_ppm: 100.0,
            sample_gap_max_us: 2_000,
            last_sample_age_us: 2_000,
        };
        let mut estimator = RawClockEstimator::new(thresholds);

        estimator.push(sample(10_000, 110_000)).unwrap();
        estimator.push(sample(11_000, 111_003)).unwrap();
        estimator.push(sample(12_000, 112_001)).unwrap();
        estimator.push(sample(13_000, 113_002)).unwrap();

        let health = estimator.health(113_100);
        assert!(health.healthy, "{health:?}");
        assert!((estimator.map_raw_us(12_500).unwrap() as i64 - 112_500).abs() <= 10);
    }

    #[test]
    fn raw_timestamp_regression_fails_closed() {
        let mut estimator = RawClockEstimator::new(RawClockThresholds::for_tests());
        estimator.push(sample(10_000, 110_000)).unwrap();

        let err = estimator.push(sample(9_999, 110_100)).unwrap_err();
        assert!(matches!(err, RawClockError::RawTimestampRegression { .. }));
        assert!(!estimator.health(110_100).healthy);
    }

    #[test]
    fn excessive_residual_marks_unhealthy() {
        let mut estimator = RawClockEstimator::new(RawClockThresholds {
            residual_p95_us: 50,
            residual_max_us: 100,
            ..RawClockThresholds::for_tests()
        });

        for i in 0..8 {
            estimator.push(sample(10_000 + i * 1_000, 110_000 + i * 1_000)).unwrap();
        }
        estimator.push(sample(19_000, 120_000)).unwrap();

        let health = estimator.health(120_000);
        assert!(!health.healthy);
        assert!(health.residual_max_us > 100);
    }

    #[test]
    fn excessive_drift_marks_unhealthy() {
        let mut estimator = RawClockEstimator::new(RawClockThresholds {
            drift_abs_ppm: 10.0,
            ..RawClockThresholds::for_tests()
        });

        estimator.push(sample(10_000, 110_000)).unwrap();
        estimator.push(sample(20_000, 120_500)).unwrap();
        estimator.push(sample(30_000, 131_000)).unwrap();
        estimator.push(sample(40_000, 141_500)).unwrap();

        let health = estimator.health(141_500);
        assert!(!health.healthy);
        assert!(health.drift_ppm.abs() > 10.0);
    }

    #[test]
    fn positive_receive_delay_outlier_does_not_move_lower_envelope_fit() {
        let mut estimator = RawClockEstimator::new(RawClockThresholds {
            warmup_samples: 4,
            warmup_window_us: 3_000,
            residual_p95_us: 100,
            residual_max_us: 250,
            ..RawClockThresholds::for_tests()
        });

        estimator.push(sample(10_000, 110_000)).unwrap();
        estimator.push(sample(11_000, 111_002)).unwrap();
        estimator.push(sample(12_000, 112_001)).unwrap();
        estimator.push(sample(12_500, 115_500)).unwrap();
        estimator.push(sample(13_000, 113_001)).unwrap();

        let mapped = estimator.map_raw_us(12_500).unwrap();
        assert!(
            (mapped as i64 - 112_500).abs() <= 20,
            "positive receive-delay outlier must not pull the fit upward: {mapped}"
        );
    }
}
```

- [ ] **Step 2: Run tests and verify they fail to compile**

Run: `cargo test -p piper-tools raw_clock -- --nocapture`

Expected: FAIL with missing `RawClockSample`, `RawClockEstimator`, `RawClockThresholds`, and `RawClockError`.

- [ ] **Step 3: Implement estimator types**

Add these public types in `crates/piper-tools/src/raw_clock.rs`:

```rust
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RawClockSample {
    pub raw_us: u64,
    pub host_rx_mono_us: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RawClockThresholds {
    pub warmup_samples: usize,
    pub warmup_window_us: u64,
    pub residual_p95_us: u64,
    pub residual_max_us: u64,
    pub drift_abs_ppm: f64,
    pub sample_gap_max_us: u64,
    pub last_sample_age_us: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RawClockHealth {
    pub healthy: bool,
    pub sample_count: usize,
    pub window_duration_us: u64,
    pub drift_ppm: f64,
    pub residual_p50_us: u64,
    pub residual_p95_us: u64,
    pub residual_p99_us: u64,
    pub residual_max_us: u64,
    pub sample_gap_max_us: u64,
    pub last_sample_age_us: u64,
    pub raw_timestamp_regressions: u64,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RawClockError {
    RawTimestampRegression { previous_raw_us: u64, raw_us: u64 },
}
```

Implement `Display` and `std::error::Error` for `RawClockError`.

- [ ] **Step 4: Implement conservative lower-envelope line fit**

Implement `RawClockEstimator` with:

```rust
pub struct RawClockEstimator {
    thresholds: RawClockThresholds,
    samples: VecDeque<RawClockSample>,
    raw_timestamp_regressions: u64,
    slope: Option<f64>,
    offset: Option<f64>,
}
```

Rules:

- `push()` rejects `raw_us <= previous_raw_us`.
- Keep samples within `warmup_window_us.max(sample_gap_max_us * 4)` of the newest host time.
- Build the fit from low-delay samples, because `host_rx_mono_us` includes positive USB/kernel/scheduler delay.
- Bucket retained samples by raw time:

```rust
let bucket_width_us =
    (thresholds.warmup_window_us / thresholds.warmup_samples.max(1) as u64).max(1);
let bucket = (sample.raw_us - first_raw_us) / bucket_width_us;
```

- For each bucket, keep the sample with the smallest `host_rx_mono_us.saturating_sub(raw_us)` delay score. This approximates the lower envelope without needing another clock source.
- Before fitting, remove selected samples that are isolated above the local lower envelope:
  - Compute `fit_outlier_us = thresholds.residual_p95_us.min(thresholds.residual_max_us).max(50)`.
  - For each selected sample, find its nearest selected neighbor before and after in raw time.
  - If both neighbors exist, linearly interpolate the neighbor host time at the sample's raw time.
  - If `sample.host_rx_mono_us > interpolated_host_us + fit_outlier_us`, drop the sample from the fit set for this recomputation.
  - If only one neighbor exists, keep the endpoint.
- Fit `host = slope * raw + offset` using centered least squares over the filtered lower-envelope samples:

```rust
let raw_mean = selected.iter().map(|s| s.raw_us as f64).sum::<f64>() / selected.len() as f64;
let host_mean = selected.iter().map(|s| s.host_rx_mono_us as f64).sum::<f64>() / selected.len() as f64;
let variance = selected.iter().map(|s| {
    let dr = s.raw_us as f64 - raw_mean;
    dr * dr
}).sum::<f64>();
let covariance = selected.iter().map(|s| {
    let dr = s.raw_us as f64 - raw_mean;
    let dh = s.host_rx_mono_us as f64 - host_mean;
    dr * dh
}).sum::<f64>();
let slope = covariance / variance;
let offset = host_mean - slope * raw_mean;
```

- This is intentionally asymmetric: only high host receive delay outliers are dropped from the fit. Low outliers are not expected from userspace receive timestamps and should make health fail if they appear.
- Require at least two selected samples with non-zero raw variance before `map_raw_us()` returns `Some`.
- Compute residuals over all retained samples as `abs(map_raw_us(raw) - host_rx_mono_us)`. Positive receive-delay outliers may make health unhealthy, but they must not bias the mapping upward.
- `health(now_host_us)` is healthy only when:
  - sample count >= `warmup_samples`
  - window duration >= `warmup_window_us`
  - `residual_p95_us <= threshold`
  - `residual_max_us <= threshold`
  - `abs(drift_ppm) <= threshold`
  - `sample_gap_max_us <= threshold`
  - `last_sample_age_us <= threshold`
  - `raw_timestamp_regressions == 0`

Use sorting over residual vectors for percentile calculation. Do not add a `statrs` dependency.

- [ ] **Step 5: Export module**

Modify `crates/piper-tools/src/lib.rs`:

```rust
pub mod raw_clock;

pub use raw_clock::{
    RawClockError, RawClockEstimator, RawClockHealth, RawClockSample, RawClockThresholds,
};
```

- [ ] **Step 6: Run estimator tests**

Run: `cargo test -p piper-tools raw_clock -- --nocapture`

Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/piper-tools/src/lib.rs crates/piper-tools/src/raw_clock.rs
git commit -m "feat: add raw clock estimator"
```

## Task 2: Preserve SocketCAN Raw Timestamp Samples

**Files:**
- Create: `crates/piper-can/src/raw_timestamp.rs`
- Modify: `crates/piper-can/src/lib.rs`
- Modify: `crates/piper-can/src/socketcan/split.rs`
- Modify: `crates/piper-can/src/socketcan/mod.rs`
- Modify: `crates/piper-driver/src/state.rs`
- Modify: `crates/piper-driver/src/heartbeat.rs`
- Modify: `crates/piper-driver/src/pipeline.rs`
- Modify: `crates/piper-driver/src/piper.rs`
- Test: `crates/piper-can/src/raw_timestamp.rs`
- Test: `crates/piper-can/src/socketcan/split.rs`
- Test: `crates/piper-driver/src/state.rs`
- Test: `crates/piper-driver/src/pipeline.rs`
- Test: `crates/piper-driver/src/piper.rs`

- [ ] **Step 1: Write raw timestamp type tests**

Create `crates/piper-can/src/raw_timestamp.rs` with tests first:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_reports_hw_raw_presence() {
        let sample = RawTimestampSample {
            iface: "can0".to_string(),
            info: RawTimestampInfo {
                can_id: 0x251,
                host_rx_mono_us: 123,
                system_ts_us: Some(100),
                hw_trans_us: None,
                hw_raw_us: Some(99),
            },
        };

        assert!(sample.has_hw_raw_without_hw_trans());
    }

    #[test]
    fn sample_prefers_hw_trans_when_present() {
        let sample = RawTimestampSample {
            iface: "can0".to_string(),
            info: RawTimestampInfo {
                can_id: 0x251,
                host_rx_mono_us: 123,
                system_ts_us: Some(100),
                hw_trans_us: Some(101),
                hw_raw_us: Some(99),
            },
        };

        assert!(!sample.has_hw_raw_without_hw_trans());
    }
}
```

- [ ] **Step 2: Run tests and verify they fail to compile**

Run: `cargo test -p piper-can raw_timestamp -- --nocapture`

Expected: FAIL with missing `RawTimestampSample`.

- [ ] **Step 3: Implement raw timestamp structs**

Add:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RawTimestampInfo {
    pub can_id: u32,
    pub host_rx_mono_us: u64,
    pub system_ts_us: Option<u64>,
    pub hw_trans_us: Option<u64>,
    pub hw_raw_us: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawTimestampSample {
    pub iface: String,
    pub info: RawTimestampInfo,
}

impl RawTimestampSample {
    pub fn has_hw_raw_without_hw_trans(&self) -> bool {
        self.info.hw_raw_us.is_some() && self.info.hw_trans_us.is_none()
    }
}
```

Keep `RawTimestampInfo` `Copy` so it can be stored in `ReceivedFrame` and real-time state structs without heap allocation. Add convenience constructors or accessors only if tests require them.

If `piper-can/serde` exists and this struct needs JSON probe output directly, gate serde derives behind the existing serde feature:

```rust
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
```

- [ ] **Step 4: Export raw timestamp module**

Modify `crates/piper-can/src/lib.rs`:

```rust
pub mod raw_timestamp;
pub use raw_timestamp::{monotonic_micros, RawTimestampInfo, RawTimestampSample};
```

Extend `ReceivedFrame` without breaking existing call sites:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ReceivedFrame {
    pub frame: PiperFrame,
    pub timestamp_provenance: TimestampProvenance,
    pub raw_timestamp: Option<RawTimestampInfo>,
}

impl ReceivedFrame {
    pub fn new(frame: PiperFrame, timestamp_provenance: TimestampProvenance) -> Self {
        Self {
            frame,
            timestamp_provenance,
            raw_timestamp: None,
        }
    }

    pub fn with_raw_timestamp(mut self, raw_timestamp: RawTimestampInfo) -> Self {
        self.raw_timestamp = Some(raw_timestamp);
        self
    }
}
```

- [ ] **Step 5: Extend `TimestampInfo` without changing classification**

Modify both SocketCAN timestamp extraction paths:

- `crates/piper-can/src/socketcan/split.rs`
- `crates/piper-can/src/socketcan/mod.rs`

Extend private `TimestampInfo`:

```rust
#[derive(Debug, Clone, Copy)]
struct TimestampInfo {
    timestamp_us: u64,
    source: TimestampSource,
    system_ts_us: Option<u64>,
    hw_trans_us: Option<u64>,
    hw_raw_us: Option<u64>,
}
```

Keep existing startup behavior:

- `hw_trans` still maps to `TimestampSource::Hardware`.
- `system` still maps to `TimestampSource::Software`.
- `hw_raw` alone must not make startup `StrictRealtime`.

- [ ] **Step 6: Add public raw sample receive method on SocketCAN RX**

In `crates/piper-can/src/socketcan/split.rs`, add a public method on `SocketCanRxAdapter`:

```rust
impl SocketCanRxAdapter {
    pub fn receive_raw_timestamp_sample(
        &mut self,
        timeout: Duration,
    ) -> Result<RawTimestampSample, CanError> {
        let received = self.receive_live(timeout)?;
        Ok(received.raw_sample)
    }
}
```

Update `SocketCanReceivedFrame` to carry both `raw_timestamp: Option<RawTimestampInfo>` and, for the probe API, `raw_sample: RawTimestampSample`. Capture `host_rx_mono_us` immediately after `recvmsg` returns, before parsing work:

```rust
let host_rx_mono_us = crate::monotonic_micros();
```

Add this helper in `crates/piper-can/src/raw_timestamp.rs` and export it from `crates/piper-can/src/lib.rs` so all crates use one monotonic origin for raw timestamp calibration:

```rust
pub fn monotonic_micros() -> u64 {
    static ORIGIN: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    let origin = ORIGIN.get_or_init(std::time::Instant::now);
    origin.elapsed().as_micros().min(u128::from(u64::MAX)) as u64
}
```

Modify `crates/piper-driver/src/heartbeat.rs`:

```rust
pub fn monotonic_micros() -> u64 {
    piper_can::monotonic_micros()
}
```

Remove the driver-local `APP_START` static so `piper-can` raw samples, driver `host_rx_mono_us`, mapped host times, and `last_sample_age_us` all share one monotonic epoch. Do not make `piper-can` depend on `piper-driver`.

Store the interface name in `SocketCanRxAdapter` at split construction time so probe samples are populated by construction:

```rust
pub struct SocketCanRxAdapter {
    iface: String,
    // existing fields
}
```

When building `RawTimestampSample`, set `iface: self.iface.clone()`.

- [ ] **Step 7: Add tests preserving startup classification and raw metadata**

In `crates/piper-can/src/socketcan/split.rs`, add tests:

```rust
#[test]
fn startup_probe_does_not_treat_raw_only_timestamp_as_strict() {
    let frame = PiperFrame::new_standard(ID_JOINT_FEEDBACK_12.raw() as u32, [0; 8]).unwrap();
    assert_eq!(
        classify_startup_probe_frame(&frame, TimestampSource::None),
        None
    );
}

#[test]
fn timestamp_info_with_hw_raw_only_is_not_hardware_source() {
    let info = TimestampInfo {
        timestamp_us: 123,
        source: TimestampSource::Software,
        system_ts_us: Some(123),
        hw_trans_us: None,
        hw_raw_us: Some(100),
    };
    assert_eq!(info.source, TimestampSource::Software);
    assert!(info.hw_raw_us.is_some());
    assert!(info.hw_trans_us.is_none());
}

#[test]
fn received_frame_can_carry_raw_timestamp_without_changing_provenance() {
    let frame = PiperFrame::new_standard(ID_JOINT_FEEDBACK_12.raw() as u32, [0; 8]).unwrap();
    let raw = RawTimestampInfo {
        can_id: ID_JOINT_FEEDBACK_12.raw() as u32,
        host_rx_mono_us: 123,
        system_ts_us: Some(123),
        hw_trans_us: None,
        hw_raw_us: Some(100),
    };

    let received = ReceivedFrame::new(frame, TimestampProvenance::Kernel).with_raw_timestamp(raw);
    assert_eq!(received.timestamp_provenance, TimestampProvenance::Kernel);
    assert_eq!(received.raw_timestamp, Some(raw));
}
```

The intent is to lock the invariant: raw-only does not become StrictRealtime.

- [ ] **Step 8: Propagate raw feedback timing through driver state**

In `crates/piper-driver/src/state.rs`, add a Copy timing struct:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RawFeedbackTiming {
    pub raw_us: u64,
    pub host_rx_mono_us: u64,
    pub can_id: u32,
}

impl RawFeedbackTiming {
    pub fn from_raw_timestamp(raw: piper_can::RawTimestampInfo) -> Option<Self> {
        raw.hw_raw_us.map(|raw_us| Self {
            raw_us,
            host_rx_mono_us: raw.host_rx_mono_us,
            can_id: raw.can_id,
        })
    }

    pub fn newest(a: Option<Self>, b: Option<Self>) -> Option<Self> {
        match (a, b) {
            (Some(left), Some(right)) => Some(if right.raw_us >= left.raw_us { right } else { left }),
            (Some(value), None) | (None, Some(value)) => Some(value),
            (None, None) => None,
        }
    }
}
```

Add fields:

```rust
pub struct JointPositionState {
    pub raw_feedback_timing: Option<RawFeedbackTiming>,
    // existing fields unchanged
}

pub struct JointDynamicState {
    pub raw_feedback_timing: Option<RawFeedbackTiming>,
    // existing fields unchanged
}

pub struct AlignedMotionState {
    pub position_raw_feedback_timing: Option<RawFeedbackTiming>,
    pub dynamic_raw_feedback_timing: Option<RawFeedbackTiming>,
    // existing fields unchanged
}

impl AlignedMotionState {
    pub fn newest_raw_feedback_timing(&self) -> Option<RawFeedbackTiming> {
        RawFeedbackTiming::newest(
            self.position_raw_feedback_timing,
            self.dynamic_raw_feedback_timing,
        )
    }
}
```

Default values stay `None`, preserving existing non-experimental behavior.

- [ ] **Step 9: Update pipeline parsing to pass `ReceivedFrame` metadata**

Modify `crates/piper-driver/src/pipeline.rs` so `io_loop()` and `rx_loop()` pass the full `ReceivedFrame` into parsing:

```rust
let received = can.receive()?;
let parsed = parse_and_update_state(
    &received,
    backend_capability,
    &ctx,
    &config,
    &mut state,
    &metrics,
);
```

Change the parser signature:

```rust
fn parse_and_update_state(
    received: &piper_can::ReceivedFrame,
    backend_capability: BackendCapability,
    ctx: &Arc<PiperContext>,
    config: &PipelineConfig,
    state: &mut ParserState,
    metrics: &Arc<PiperMetrics>,
) -> ParsedFeedbackOutcome {
    let frame = &received.frame;
    let raw_feedback = received
        .raw_timestamp
        .and_then(RawFeedbackTiming::from_raw_timestamp);
    // existing match on frame.id().as_standard()
}
```

Add an experimental helper next to `control_grade_group_ready()`:

```rust
fn experimental_raw_clock_group_ready(
    group: &PendingFrameGroup<3>,
    backend_capability: BackendCapability,
    raw_timings: &[Option<RawFeedbackTiming>; 3],
) -> bool {
    backend_capability.is_soft_realtime()
        && complete_group_ready(group.mask)
        && raw_timings.iter().all(Option::is_some)
}
```

For position feedback groups:

- keep a `[Option<RawFeedbackTiming>; 3]` in `ParserState`
- write the slot for `0x2A5`, `0x2A6`, and `0x2A7`
- publish `JointPositionState.raw_feedback_timing` as the newest raw timing among the slots that contributed to that position group
- publish to the existing control-pair cell when either:
  - `control_grade_group_ready(...)` is true for production StrictRealtime, or
  - `experimental_raw_clock_group_ready(...)` is true for SoftRealtime raw-clock experiments

For dynamic feedback groups:

- keep `state.pending_joint_dynamic.raw_feedback_timing` as the newest raw timing among `0x251..=0x256` frames in the buffered group
- when the complete dynamic group is committed, publish to the existing control-pair cell when either:
  - StrictRealtime group span is within `STRICT_GROUP_MAX_SPAN_US`, or
  - backend is `SoftRealtime` and every velocity frame in the group had `hw_raw`
- reset raw timing with the rest of the pending dynamic group after commit or timeout

Modify `crates/piper-driver/src/piper.rs` so `get_aligned_motion()` copies these values into `AlignedMotionState`.

This is an experiment-only publication path: SoftRealtime raw-clock groups may populate the internal control-pair cell so `get_aligned_motion()` has coherent state for the experimental runtime, but `Observer<SoftRealtime>::control_snapshot*()` remains unavailable because the public observer API still requires `StrictCapability`.

- [ ] **Step 10: Add driver tests for raw timing propagation**

Add tests that build received frames with raw metadata and verify:

```rust
#[test]
fn aligned_motion_exposes_newest_raw_feedback_timing_from_contributing_frames() {
    let piper = make_test_piper();
    piper.ctx.publish_control_joint_position(JointPositionState {
        hardware_timestamp_us: 1_000,
        host_rx_mono_us: 11_000,
        raw_feedback_timing: Some(RawFeedbackTiming {
            raw_us: 10_003,
            host_rx_mono_us: 11_003,
            can_id: 0x2A7,
        }),
        joint_pos: [0.0; 6],
        frame_valid_mask: 0b0000_0111,
    });
    piper.ctx.publish_control_joint_dynamic(JointDynamicState {
        group_timestamp_us: 1_001,
        group_host_rx_mono_us: 11_005,
        raw_feedback_timing: Some(RawFeedbackTiming {
            raw_us: 10_006,
            host_rx_mono_us: 11_006,
            can_id: 0x256,
        }),
        joint_vel: [0.0; 6],
        joint_current: [0.0; 6],
        timestamps: [1_001; 6],
        valid_mask: 0b0011_1111,
    });

    let state = match piper.get_aligned_motion(5_000, Duration::from_secs(3600)) {
        AlignmentResult::Ok(state) => state,
        other => panic!("expected aligned state, got {other:?}"),
    };

    assert_eq!(state.newest_raw_feedback_timing().unwrap().raw_us, 10_006);
}
```

Use the existing piper-driver test helper names in `piper.rs`; the required invariant is that `AlignedMotionState::newest_raw_feedback_timing()` returns the newest raw-timestamped robot feedback frame that actually contributed to the coherent position/dynamic control pair.

Add pipeline tests:

```rust
#[test]
fn soft_realtime_with_complete_hw_raw_groups_publishes_experimental_control_pair() {
    let ctx = Arc::new(PiperContext::new_for_test());
    let mut state = ParserState::new();
    let metrics = Arc::new(PiperMetrics::new());
    for received in soft_realtime_position_and_dynamic_frames_with_hw_raw_for_tests() {
        parse_and_update_state(
            &received,
            BackendCapability::SoftRealtime,
            &ctx,
            &PipelineConfig::default(),
            &mut state,
            &metrics,
        );
    }

    let view = ctx.capture_control_read_view();
    assert!(view.pair.position_sequence > 0);
    assert!(view.pair.dynamic_sequence > 0);
    assert!(view.pair.joint_position.raw_feedback_timing.is_some());
    assert!(view.pair.joint_dynamic.raw_feedback_timing.is_some());
}

#[test]
fn soft_realtime_without_hw_raw_does_not_publish_experimental_control_pair() {
    let ctx = Arc::new(PiperContext::new_for_test());
    let mut state = ParserState::new();
    let metrics = Arc::new(PiperMetrics::new());
    for received in soft_realtime_position_and_dynamic_frames_without_hw_raw_for_tests() {
        parse_and_update_state(
            &received,
            BackendCapability::SoftRealtime,
            &ctx,
            &PipelineConfig::default(),
            &mut state,
            &metrics,
        );
    }

    let view = ctx.capture_control_read_view();
    assert_eq!(view.pair.position_sequence, 0);
    assert_eq!(view.pair.dynamic_sequence, 0);
}
```

- [ ] **Step 11: Run targeted tests**

Run: `cargo test -p piper-can raw_timestamp -- --nocapture`

Expected: PASS.

Run: `cargo test -p piper-can startup_probe -- --nocapture`

Expected: PASS.

Run: `cargo test -p piper-driver raw_feedback_timing -- --nocapture`

Expected: PASS.

Run: `cargo test -p piper-driver aligned_motion_exposes_newest_raw_feedback_timing_from_contributing_frames -- --nocapture`

Expected: PASS.

Run: `cargo test -p piper-driver monotonic_micros_delegates_to_piper_can_epoch -- --nocapture`

Expected: PASS.

- [ ] **Step 12: Commit**

```bash
git add crates/piper-can/src/lib.rs crates/piper-can/src/raw_timestamp.rs crates/piper-can/src/socketcan/split.rs crates/piper-can/src/socketcan/mod.rs crates/piper-driver/src/state.rs crates/piper-driver/src/heartbeat.rs crates/piper-driver/src/pipeline.rs crates/piper-driver/src/piper.rs
git commit -m "feat: expose socketcan raw timestamp samples"
```

## Task 3: Add Read-Only Raw Clock Probe Example

**Files:**
- Create: `crates/piper-sdk/examples/socketcan_raw_clock_probe.rs`
- Modify: `crates/piper-sdk/Cargo.toml`
- Modify: `crates/piper-sdk/examples/README.md`
- Test: `crates/piper-sdk/examples/socketcan_raw_clock_probe.rs`

- [ ] **Step 1: Write argument/report tests**

Create `crates/piper-sdk/examples/socketcan_raw_clock_probe.rs` with testable argument parsing and report structs:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn parses_required_dual_interface_args() {
        let args = Args::parse_from([
            "socketcan_raw_clock_probe",
            "--left-interface",
            "can0",
            "--right-interface",
            "can1",
            "--duration-secs",
            "300",
            "--out",
            "artifacts/teleop/raw-clock-probe.json",
        ]);

        assert_eq!(args.left_interface, "can0");
        assert_eq!(args.right_interface, "can1");
        assert_eq!(args.duration_secs, 300);
    }

    #[test]
    fn report_marks_failed_when_one_side_unhealthy() {
        let report = ProbeReport::from_health(
            "can0",
            "can1",
            ProbeInterfaceMetadata::for_tests("can0"),
            ProbeInterfaceMetadata::for_tests("can1"),
            TimestampCapabilitySummary::unknown_for_tests(),
            TimestampCapabilitySummary::unknown_for_tests(),
            Vec::new(),
            RawClockHealth::healthy_for_tests(),
            RawClockHealth {
                healthy: false,
                reason: Some("no hw_raw".to_string()),
                ..RawClockHealth::empty_for_tests()
            },
        );

        assert!(!report.pass);
    }

    #[test]
    fn report_keeps_raw_samples_and_inter_arm_skew_metrics() {
        let samples = vec![
            ProbeRawFrameSample {
                side: ProbeSide::Left,
                can_id: 0x251,
                host_rx_mono_us: 110_000,
                system_ts_us: Some(110_010),
                hw_trans_us: None,
                hw_raw_us: Some(10_000),
                mapped_host_us: Some(110_000),
            },
            ProbeRawFrameSample {
                side: ProbeSide::Right,
                can_id: 0x251,
                host_rx_mono_us: 110_700,
                system_ts_us: Some(110_710),
                hw_trans_us: None,
                hw_raw_us: Some(20_000),
                mapped_host_us: Some(110_700),
            },
        ];

        let report = ProbeReport::from_samples_for_tests(samples);
        assert_eq!(report.raw_samples.len(), 2);
        assert_eq!(report.max_estimated_inter_arm_skew_us, Some(700));
        assert_eq!(report.estimated_inter_arm_skew_p95_us, Some(700));
    }
}
```

If `RawClockHealth::healthy_for_tests()` and `empty_for_tests()` do not exist yet, add test-only constructors in `piper-tools` or build explicit values in the test.

- [ ] **Step 2: Run test and verify it fails**

Run: `cargo test -p piper-sdk --example socketcan_raw_clock_probe parses_required_dual_interface_args -- --exact`

Expected: FAIL because the example does not exist or types are missing.

- [ ] **Step 3: Implement probe CLI skeleton**

Use this structure:

```rust
#[cfg(target_os = "linux")]
#[derive(clap::Parser, Debug)]
struct Args {
    #[arg(long)]
    left_interface: String,
    #[arg(long)]
    right_interface: String,
    #[arg(long, default_value_t = 300)]
    duration_secs: u64,
    #[arg(long)]
    out: Option<std::path::PathBuf>,
}

#[cfg(target_os = "linux")]
#[derive(Debug, serde::Serialize)]
struct ProbeReport {
    schema_version: u8,
    left_interface: String,
    right_interface: String,
    left_metadata: ProbeInterfaceMetadata,
    right_metadata: ProbeInterfaceMetadata,
    left_timestamp_capabilities: TimestampCapabilitySummary,
    right_timestamp_capabilities: TimestampCapabilitySummary,
    pass: bool,
    left: ProbeSideReport,
    right: ProbeSideReport,
    raw_samples: Vec<ProbeRawFrameSample>,
    estimated_inter_arm_skew_p95_us: Option<u64>,
    max_estimated_inter_arm_skew_us: Option<u64>,
}

#[cfg(target_os = "linux")]
#[derive(Debug, serde::Serialize)]
struct ProbeInterfaceMetadata {
    name: String,
    if_index: Option<u32>,
    mtu: Option<u32>,
    driver: Option<String>,
}

#[cfg(target_os = "linux")]
#[derive(Debug, serde::Serialize)]
struct TimestampCapabilitySummary {
    source: String,
    so_timestamping_enabled: bool,
    hardware_transmit: Option<bool>,
    hardware_receive: Option<bool>,
    hardware_raw_clock: Option<bool>,
    raw_text: Option<String>,
    error: Option<String>,
}

#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Copy, serde::Serialize, PartialEq, Eq)]
enum ProbeSide {
    Left,
    Right,
}

#[cfg(target_os = "linux")]
#[derive(Debug, Clone, serde::Serialize)]
struct ProbeRawFrameSample {
    side: ProbeSide,
    can_id: u32,
    host_rx_mono_us: u64,
    system_ts_us: Option<u64>,
    hw_trans_us: Option<u64>,
    hw_raw_us: Option<u64>,
    mapped_host_us: Option<u64>,
}
```

For non-Linux, return an error explaining the probe is SocketCAN/Linux only.

Implement metadata helpers in this example:

- `read_interface_metadata(iface: &str) -> ProbeInterfaceMetadata`
  - read `/sys/class/net/<iface>/ifindex`
  - read `/sys/class/net/<iface>/mtu`
  - read `/sys/class/net/<iface>/device/driver` symlink basename when present
- `read_timestamp_capabilities(iface: &str) -> TimestampCapabilitySummary`
  - run `ethtool -T <iface>` with `std::process::Command`
  - parse stdout for `hardware-transmit`, `hardware-receive`, and `hardware-raw-clock`
  - if `ethtool` is unavailable or exits non-zero, keep `raw_text: None`, set `error`, and continue

- [ ] **Step 4: Implement read-only sampling loop**

Implementation outline:

```rust
let mut left = SocketCanAdapter::new(&args.left_interface)?;
let mut right = SocketCanAdapter::new(&args.right_interface)?;
left.set_receive_timeout(Duration::from_millis(10));
right.set_receive_timeout(Duration::from_millis(10));
let (mut left_rx, _) = left.split()?;
let (mut right_rx, _) = right.split()?;

let mut left_estimator = RawClockEstimator::new(thresholds);
let mut right_estimator = RawClockEstimator::new(thresholds);
let mut raw_samples = Vec::new();
let mut skew_samples_us = Vec::new();
let mut latest_left_mapped = None;
let mut latest_right_mapped = None;

while start.elapsed() < Duration::from_secs(args.duration_secs) {
    if let Ok(sample) = left_rx.receive_raw_timestamp_sample(Duration::from_millis(5)) {
        let mut mapped_host_us = None;
        if let Some(hw_raw_us) = sample.info.hw_raw_us {
            let _ = left_estimator.push(RawClockSample {
                raw_us: hw_raw_us,
                host_rx_mono_us: sample.info.host_rx_mono_us,
            });
            mapped_host_us = left_estimator.map_raw_us(hw_raw_us);
            latest_left_mapped = mapped_host_us;
        }
        raw_samples.push(ProbeRawFrameSample {
            side: ProbeSide::Left,
            can_id: sample.info.can_id,
            host_rx_mono_us: sample.info.host_rx_mono_us,
            system_ts_us: sample.info.system_ts_us,
            hw_trans_us: sample.info.hw_trans_us,
            hw_raw_us: sample.info.hw_raw_us,
            mapped_host_us,
        });
    }
    if let Ok(sample) = right_rx.receive_raw_timestamp_sample(Duration::from_millis(5)) {
        let mut mapped_host_us = None;
        if let Some(hw_raw_us) = sample.info.hw_raw_us {
            let _ = right_estimator.push(RawClockSample {
                raw_us: hw_raw_us,
                host_rx_mono_us: sample.info.host_rx_mono_us,
            });
            mapped_host_us = right_estimator.map_raw_us(hw_raw_us);
            latest_right_mapped = mapped_host_us;
        }
        raw_samples.push(ProbeRawFrameSample {
            side: ProbeSide::Right,
            can_id: sample.info.can_id,
            host_rx_mono_us: sample.info.host_rx_mono_us,
            system_ts_us: sample.info.system_ts_us,
            hw_trans_us: sample.info.hw_trans_us,
            hw_raw_us: sample.info.hw_raw_us,
            mapped_host_us,
        });
    }
    if let (Some(left_us), Some(right_us)) = (latest_left_mapped, latest_right_mapped) {
        skew_samples_us.push(left_us.abs_diff(right_us));
    }
}
```

The probe must not send motor enable, MIT, position, gripper, or disable commands.

Use `skew_samples_us` to compute `estimated_inter_arm_skew_p95_us` and `max_estimated_inter_arm_skew_us` by sorting the vector. Keep `raw_samples` in the JSON output; this is intentionally verbose so a failed calibration can be debugged without rerunning the full probe.

- [ ] **Step 5: Write JSON atomically when `--out` is provided**

Use a temp sibling file and rename, or reuse an existing atomic-write helper if available. If no helper exists, implement this narrow helper inside the example:

```rust
fn write_json_report(path: &Path, report: &ProbeReport) -> anyhow::Result<()> {
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(report)?)?;
    std::fs::rename(tmp, path)?;
    Ok(())
}
```

- [ ] **Step 6: Register and document example**

Add `serde` for example/test builds in `crates/piper-sdk/Cargo.toml`:

```toml
[dev-dependencies]
serde = { workspace = true, features = ["derive"] }
```

If required by current metadata, add to `crates/piper-sdk/Cargo.toml`:

```toml
[[example]]
name = "socketcan_raw_clock_probe"
path = "examples/socketcan_raw_clock_probe.rs"
required-features = ["target-socketcan"]
```

Add to `crates/piper-sdk/examples/README.md` under hardware diagnostics:

```markdown
- `socketcan_raw_clock_probe.rs` - read-only dual-interface raw timestamp calibration probe
```

- [ ] **Step 7: Run example tests**

Run: `cargo test -p piper-sdk --example socketcan_raw_clock_probe`

Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add crates/piper-sdk/Cargo.toml crates/piper-sdk/examples/socketcan_raw_clock_probe.rs crates/piper-sdk/examples/README.md
git commit -m "feat: add socketcan raw clock probe"
```

## Task 4: Add SoftRealtime MIT Array Helper

**Files:**
- Modify: `crates/piper-client/src/state/machine.rs`
- Test: `crates/piper-client/src/state/machine.rs`

- [ ] **Step 1: Write failing SoftRealtime helper test**

In `crates/piper-client/src/state/machine.rs`, add a test near `enable_mit_passthrough_succeeds_with_fresh_matching_robot_and_echo`:

```rust
#[test]
fn soft_mit_passthrough_command_torques_confirmed_applies_firmware_quirks() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let standby = build_soft_standby_piper_with_tx(
        FreshMitFeedbackRxAdapter::new(),
        RecordingTxAdapter::new(sent.clone()),
    );
    let active = standby.enable_mit_passthrough(MitModeConfig::default()).unwrap();

    active
        .command_torques_confirmed(
            &JointArray::splat(Rad(0.0)),
            &JointArray::splat(0.0),
            &JointArray::splat(8.0),
            &JointArray::splat(1.0),
            &JointArray::splat(NewtonMeter(0.0)),
            Duration::from_millis(20),
        )
        .expect("soft passthrough torque command should send");

    let frames = wait_for_sent_frames(&sent, 6);
    assert_eq!(frames.len(), 6);
}
```

Use the existing `build_soft_standby_piper_with_tx`, `FreshMitFeedbackRxAdapter`, `RecordingTxAdapter`, and `wait_for_sent_frames` helpers if they are still present in `state/machine.rs`. The important behavior is that SoftRealtime passthrough can send a full validated MIT command batch from typed arrays, not only prebuilt raw commands.

- [ ] **Step 2: Run test and verify it fails**

Run: `cargo test -p piper-client soft_mit_passthrough_command_torques_confirmed_applies_firmware_quirks -- --nocapture`

Expected: FAIL with missing `command_torques_confirmed` on `Piper<Active<MitPassthroughMode>, SoftRealtime>`.

- [ ] **Step 3: Implement SoftRealtime typed helper**

Add to `impl Piper<Active<MitPassthroughMode>, SoftRealtime>`:

```rust
pub fn command_torques_confirmed(
    &self,
    positions: &JointArray<Rad>,
    velocities: &JointArray<f64>,
    kp: &JointArray<f64>,
    kd: &JointArray<f64>,
    torques: &JointArray<NewtonMeter>,
    timeout: Duration,
) -> Result<()> {
    let raw = RawCommander::new(&self.driver);
    let commands = self.build_validated_mit_command_batch(
        positions, velocities, kp, kd, torques,
    )?;
    raw.send_validated_mit_command_batch_confirmed(commands, timeout)
}
```

Do not make this method available on MonitorOnly.

- [ ] **Step 4: Run targeted tests**

Run: `cargo test -p piper-client soft_mit_passthrough_command_torques_confirmed_applies_firmware_quirks -- --nocapture`

Expected: PASS.

Run: `cargo test -p piper-client command_torques_confirmed_applies_firmware_quirks -- --nocapture`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/piper-client/src/state/machine.rs
git commit -m "feat: add soft realtime mit torque helper"
```

## Task 5: Add Experimental Raw-Clock Dual-Arm Runtime

**Files:**
- Create: `crates/piper-client/src/dual_arm_raw_clock.rs`
- Modify: `crates/piper-client/src/lib.rs`
- Test: `crates/piper-client/src/dual_arm_raw_clock.rs`

- [ ] **Step 1: Write failing runtime gate tests**

Create `crates/piper-client/src/dual_arm_raw_clock.rs` with tests first:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn experimental_config_rejects_bilateral_mode() {
        let err = ExperimentalRawClockConfig::default()
            .with_mode(ExperimentalRawClockMode::Bilateral)
            .unwrap_err();

        assert!(err.to_string().contains("master-follower"));
    }

    #[test]
    fn skew_above_threshold_fails_health_gate() {
        let gate = RawClockRuntimeGate::new(RawClockRuntimeThresholds {
            inter_arm_skew_max_us: 2_000,
            ..RawClockRuntimeThresholds::for_tests()
        });

        let err = gate
            .check_tick(RawClockTickTiming {
                master_feedback_time_us: 100_000,
                slave_feedback_time_us: 103_000,
                inter_arm_skew_us: 3_000,
                master_health: RawClockHealth::healthy_for_tests(),
                slave_health: RawClockHealth::healthy_for_tests(),
            })
            .unwrap_err();

        assert!(matches!(err, RawClockRuntimeError::InterArmSkew { .. }));
    }

    #[test]
    fn tick_timing_uses_newest_raw_feedback_from_control_snapshots() {
        let mut timing = RawClockRuntimeTiming::new(RawClockThresholds::for_tests());
        let master = raw_clock_snapshot_for_tests(10_000, 110_000);
        let slave = raw_clock_snapshot_for_tests(20_000, 110_800);

        timing.seed_ready_for_tests(&[
            raw_clock_snapshot_for_tests(9_000, 109_000),
            raw_clock_snapshot_for_tests(9_500, 109_500),
            raw_clock_snapshot_for_tests(10_000, 110_000),
        ], &[
            raw_clock_snapshot_for_tests(19_000, 109_800),
            raw_clock_snapshot_for_tests(19_500, 110_300),
            raw_clock_snapshot_for_tests(20_000, 110_800),
        ]);
        let tick = timing.tick_from_snapshots(&master, &slave, 110_900).unwrap();

        assert_eq!(tick.master_feedback_time_us, 110_000);
        assert_eq!(tick.slave_feedback_time_us, 110_800);
        assert_eq!(tick.inter_arm_skew_us, 800);
    }

    #[test]
    fn missing_raw_feedback_timing_fails_closed() {
        let mut timing = RawClockRuntimeTiming::new(RawClockThresholds::for_tests());
        let master = raw_clock_snapshot_for_tests(10_000, 110_000);
        let slave = raw_clock_snapshot_without_raw_timing_for_tests();

        let err = timing.tick_from_snapshots(&master, &slave, 110_900).unwrap_err();
        assert!(matches!(err, RawClockRuntimeError::MissingRawFeedbackTiming { .. }));
    }

    #[test]
    fn warmup_not_ready_samples_do_not_abort() {
        let mut timing = RawClockRuntimeTiming::new(RawClockThresholds::for_tests());
        let master = raw_clock_snapshot_for_tests(10_000, 110_000);
        let slave = raw_clock_snapshot_for_tests(20_000, 110_800);

        timing.ingest_snapshots(&master, &slave).unwrap();
        let err = timing.tick_from_snapshots(&master, &slave, 110_900).unwrap_err();
        assert!(matches!(err, RawClockRuntimeError::EstimatorNotReady { .. }));
        assert_eq!(timing.sample_counts_for_tests(), (1, 1));
    }
}
```

Use explicit test constructors if `RawClockHealth::healthy_for_tests()` is not public.

- [ ] **Step 2: Run tests and verify they fail**

Run: `cargo test -p piper-client dual_arm_raw_clock -- --nocapture`

Expected: FAIL because the module and types do not exist.

- [ ] **Step 3: Implement runtime config and health gate**

Add public experimental types:

```rust
pub enum ExperimentalRawClockMode {
    MasterFollower,
    Bilateral,
}

pub struct ExperimentalRawClockConfig {
    pub frequency_hz: f64,
    pub max_iterations: Option<usize>,
    pub thresholds: RawClockRuntimeThresholds,
    pub estimator_thresholds: RawClockThresholds,
}

pub struct RawClockRuntimeThresholds {
    pub inter_arm_skew_max_us: u64,
    pub last_sample_age_us: u64,
}

pub struct RawClockTickTiming {
    pub master_feedback_time_us: u64,
    pub slave_feedback_time_us: u64,
    pub inter_arm_skew_us: u64,
    pub master_health: RawClockHealth,
    pub slave_health: RawClockHealth,
}

pub struct RawClockRuntimeReport {
    pub master: RawClockHealth,
    pub slave: RawClockHealth,
    pub max_inter_arm_skew_us: u64,
    pub inter_arm_skew_p95_us: u64,
    pub clock_health_failures: u64,
    pub iterations: usize,
    pub exit_reason: String,
}
```

Implement `RawClockRuntimeGate::check_tick()` to fail when either estimator is unhealthy or skew exceeds threshold.

- [ ] **Step 4: Implement experimental raw-clock snapshot extraction**

Add a snapshot type in `crates/piper-client/src/dual_arm_raw_clock.rs`:

```rust
pub struct ExperimentalRawClockSnapshot {
    pub state: ControlSnapshot,
    pub newest_raw_feedback_timing: piper_driver::RawFeedbackTiming,
    pub feedback_age: Duration,
}
```

Add a private helper that converts `piper_driver::AlignedMotionState`:

```rust
fn experimental_snapshot_from_aligned(
    state: piper_driver::AlignedMotionState,
) -> Result<ExperimentalRawClockSnapshot, RawClockRuntimeError> {
    let newest_raw_feedback_timing = state
        .newest_raw_feedback_timing()
        .ok_or(RawClockRuntimeError::MissingRawFeedbackTiming { side: "unknown" })?;

    Ok(ExperimentalRawClockSnapshot {
        state: ControlSnapshot {
            position: JointArray::new(state.joint_pos.map(Rad)),
            velocity: JointArray::new(state.joint_vel.map(RadPerSecond)),
            torque: JointArray::new(std::array::from_fn(|index| {
                NewtonMeter(piper_driver::JointDynamicState::calculate_torque(
                    index,
                    state.joint_current[index],
                ))
            })),
            position_timestamp_us: state.position_timestamp_us,
            dynamic_timestamp_us: state.dynamic_timestamp_us,
            skew_us: state.skew_us,
        },
        newest_raw_feedback_timing,
        feedback_age: state.feedback_age(),
    })
}
```

Do not make this a `StrictCapability` API. It exists only inside the experimental raw-clock module and reads `Piper<_, SoftRealtime>.driver.get_aligned_motion(...)` directly because `Piper.driver` is `pub(crate)`.

- [ ] **Step 5: Add runtime timing state tied to snapshots**

Implement `RawClockRuntimeTiming`:

```rust
pub struct RawClockRuntimeTiming {
    master: RawClockEstimator,
    slave: RawClockEstimator,
    skew_samples_us: Vec<u64>,
    clock_health_failures: u64,
    last_master_raw_us: Option<u64>,
    last_slave_raw_us: Option<u64>,
}

impl RawClockRuntimeTiming {
    pub fn ingest_snapshots(
        &mut self,
        master: &ExperimentalRawClockSnapshot,
        slave: &ExperimentalRawClockSnapshot,
    ) -> Result<(), RawClockRuntimeError> {
        if self
            .last_master_raw_us
            .is_some_and(|previous| master.newest_raw_feedback_timing.raw_us < previous)
        {
            return Err(RawClockRuntimeError::RawTimestampRegression { side: "master" });
        }
        if self
            .last_slave_raw_us
            .is_some_and(|previous| slave.newest_raw_feedback_timing.raw_us < previous)
        {
            return Err(RawClockRuntimeError::RawTimestampRegression { side: "slave" });
        }
        if self.last_master_raw_us != Some(master.newest_raw_feedback_timing.raw_us) {
            self.master.push(RawClockSample {
                raw_us: master.newest_raw_feedback_timing.raw_us,
                host_rx_mono_us: master.newest_raw_feedback_timing.host_rx_mono_us,
            })?;
            self.last_master_raw_us = Some(master.newest_raw_feedback_timing.raw_us);
        }
        if self.last_slave_raw_us != Some(slave.newest_raw_feedback_timing.raw_us) {
            self.slave.push(RawClockSample {
                raw_us: slave.newest_raw_feedback_timing.raw_us,
                host_rx_mono_us: slave.newest_raw_feedback_timing.host_rx_mono_us,
            })?;
            self.last_slave_raw_us = Some(slave.newest_raw_feedback_timing.raw_us);
        }
        Ok(())
    }

    pub fn tick_from_snapshots(
        &mut self,
        master: &ExperimentalRawClockSnapshot,
        slave: &ExperimentalRawClockSnapshot,
        now_host_us: u64,
    ) -> Result<RawClockTickTiming, RawClockRuntimeError> {
        self.ingest_snapshots(master, slave)?;
        let master_feedback_time_us = self
            .master
            .map_raw_us(master.newest_raw_feedback_timing.raw_us)
            .ok_or(RawClockRuntimeError::EstimatorNotReady { side: "master" })?;
        let slave_feedback_time_us = self
            .slave
            .map_raw_us(slave.newest_raw_feedback_timing.raw_us)
            .ok_or(RawClockRuntimeError::EstimatorNotReady { side: "slave" })?;
        let inter_arm_skew_us = master_feedback_time_us.abs_diff(slave_feedback_time_us);
        self.skew_samples_us.push(inter_arm_skew_us);

        Ok(RawClockTickTiming {
            master_feedback_time_us,
            slave_feedback_time_us,
            inter_arm_skew_us,
            master_health: self.master.health(now_host_us),
            slave_health: self.slave.health(now_host_us),
        })
    }
}
```

The raw sample used here is specifically the newest raw-timestamped feedback frame that contributed to the coherent position/dynamic control snapshot. Do not use unrelated frames from a parallel receive socket for runtime skew.

Repeated reads of the same coherent snapshot must not be pushed into `RawClockEstimator` again, because the estimator correctly rejects non-increasing raw timestamps. Equal raw timestamps are treated as "no new sample"; older raw timestamps remain a fail-closed regression. `ingest_snapshots()` is usable during warmup before the estimator can map samples; `tick_from_snapshots()` is used only when the caller wants mapped times and skew.

- [ ] **Step 6: Add experimental standby/active wrappers**

Implement wrappers around SoftRealtime arms:

```rust
pub struct ExperimentalRawClockDualArmStandby {
    master: Piper<Standby, SoftRealtime>,
    slave: Piper<Standby, SoftRealtime>,
    timing: RawClockRuntimeTiming,
    config: ExperimentalRawClockConfig,
}

pub struct ExperimentalRawClockDualArmActive {
    master: Piper<Active<MitPassthroughMode>, SoftRealtime>,
    slave: Piper<Active<MitPassthroughMode>, SoftRealtime>,
    timing: RawClockRuntimeTiming,
    config: ExperimentalRawClockConfig,
}
```

Expose only experimental constructors. Do not modify `DualArmBuilder::build()`.

- [ ] **Step 7: Add standby warmup before enable**

Add `ExperimentalRawClockDualArmStandby::warmup()`:

```rust
pub fn warmup(
    mut self,
    policy: ControlReadPolicy,
    warmup: Duration,
    cancel_signal: &AtomicBool,
) -> Result<Self, RawClockRuntimeError> {
    let deadline = Instant::now() + warmup;
    while Instant::now() < deadline {
        if cancel_signal.load(Ordering::Relaxed) {
            return Err(RawClockRuntimeError::Cancelled);
        }
        let master = self.read_master_experimental_snapshot(policy)?;
        let slave = self.read_slave_experimental_snapshot(policy)?;
        self.timing.ingest_snapshots(&master, &slave)?;
        match self
            .timing
            .tick_from_snapshots(&master, &slave, piper_can::monotonic_micros())
        {
            Ok(tick) => RawClockRuntimeGate::new(self.config.thresholds).check_tick(tick)?,
            Err(RawClockRuntimeError::EstimatorNotReady { .. }) => {},
            Err(err) => return Err(err),
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    let master = self.read_master_experimental_snapshot(policy)?;
    let slave = self.read_slave_experimental_snapshot(policy)?;
    let final_tick = self
        .timing
        .tick_from_snapshots(&master, &slave, piper_can::monotonic_micros())?;
    RawClockRuntimeGate::new(self.config.thresholds).check_tick(final_tick)?;
    Ok(self)
}
```

Use `piper_can::monotonic_micros()` here, not a module-local `Instant` helper. That is the same epoch used by SocketCAN raw samples and by `piper_driver::heartbeat::monotonic_micros()` after Task 2. Warmup runs after SoftRealtime connection is established but before MIT enable/confirmation, so it uses the same driver snapshots the runtime will use.

During warmup, `EstimatorNotReady` is not fatal. The loop keeps collecting raw samples until the configured warmup deadline, then performs one final mapped tick and health/skew gate. Raw timestamp regressions, missing raw timing, stale feedback, cancellation, and any other `tick_from_snapshots()` error remain fatal immediately.

- [ ] **Step 8: Add public active transition and typed command submission helper**

Add a public transition on `ExperimentalRawClockDualArmStandby` so `piper-cli` can warm up, ask for operator confirmation, then enable both SoftRealtime MIT passthrough arms without losing the calibrated timing state:

```rust
impl ExperimentalRawClockDualArmStandby {
    pub fn enable_mit_passthrough(
        self,
        master: MitModeConfig,
        slave: MitModeConfig,
    ) -> Result<ExperimentalRawClockDualArmActive, RawClockRuntimeError> {
        let master = self.master.enable_mit_passthrough(master)?;
        let slave = self.slave.enable_mit_passthrough(slave)?;
        Ok(ExperimentalRawClockDualArmActive {
            master,
            slave,
            timing: self.timing,
            config: self.config,
        })
    }
}
```

If one arm enables and the other fails, immediately fault/stop the enabled arm and return an error that records the failed side. Do not leave one enabled arm hidden behind an error.

Add a method on `ExperimentalRawClockDualArmActive`:

```rust
fn submit_command(
    &self,
    command: &BilateralCommand,
    timeout: Duration,
) -> Result<(), RobotError> {
    self.slave.command_torques_confirmed(
        &command.slave_position,
        &command.slave_velocity,
        &command.slave_kp,
        &command.slave_kd,
        &command.slave_feedforward_torque,
        timeout,
    )?;
    self.master.command_torques_confirmed(
        &command.master_position,
        &command.master_velocity,
        &command.master_kp,
        &command.master_kd,
        &command.master_interaction_torque,
        timeout,
    )?;
    Ok(())
}
```

The first implementation should use master-follower mode, so `master_interaction_torque` remains zero.

- [ ] **Step 9: Implement per-tick runtime loop and report**

Add a run method that:

1. Reads master and slave experimental snapshots from the two SoftRealtime drivers.
2. Calls `RawClockRuntimeTiming::tick_from_snapshots()`.
3. Calls `RawClockRuntimeGate::check_tick()`.
4. Builds the existing `BilateralCommand` using the master-follower controller.
5. Submits the command with `submit_command()`.
6. Checks runtime health for both SoftRealtime drivers each loop.
7. On any active-loop read fault, raw-clock timing failure, controller/config fault, command submission fault, or runtime transport fault, stops normal command submission, calls `fault_shutdown()`, records per-side stop attempts, and returns a failure report/error.

Do not use `?` directly on snapshot reads, controller ticks, timing gates, runtime-health checks, or command submission inside the active loop. Match each failure, classify it, then call `fault_shutdown(FAULT_SHUTDOWN_TIMEOUT)` before returning.

Expose `RawClockRuntimeReport` with `master`, `slave`, `max_inter_arm_skew_us`, `inter_arm_skew_p95_us`, `clock_health_failures`, `read_faults`, `submission_faults`, `runtime_faults`, `iterations`, `exit_reason`, `master_stop_attempt`, and `slave_stop_attempt`. Compute p95 by sorting `skew_samples_us`.

- [ ] **Step 10: Implement bounded shutdown path**

Implement clean disable and fault shutdown. Clean disable should attempt both sides and report both results; active-loop faults must use `fault_shutdown()`, not clean disable.

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExperimentalFaultShutdown {
    pub master_stop_attempt: StopAttemptResult,
    pub slave_stop_attempt: StopAttemptResult,
}

impl ExperimentalRawClockDualArmActive {
    pub fn disable_both(self, cfg: DisableConfig) -> Result<ExperimentalRawClockDualArmStandby, RawClockRuntimeError> {
        let master_result = self.master.disable(cfg.clone());
        let slave_result = self.slave.disable(cfg);
        match (master_result, slave_result) {
            (Ok(master), Ok(slave)) => Ok(ExperimentalRawClockDualArmStandby {
                master,
                slave,
                timing: self.timing,
                config: self.config,
            }),
            (master, slave) => Err(RawClockRuntimeError::DisableBothFailed {
                master: master.err().map(|err| err.to_string()),
                slave: slave.err().map(|err| err.to_string()),
            }),
        }
    }

    pub fn fault_shutdown(self, timeout: Duration) -> (ExperimentalRawClockErrorState, ExperimentalFaultShutdown) {
        let deadline = Instant::now() + timeout;
        self.master.driver.latch_fault();
        self.slave.driver.latch_fault();
        let master_pending = enqueue_experimental_stop_attempt(&self.master, deadline);
        let slave_pending = enqueue_experimental_stop_attempt(&self.slave, deadline);
        let master_stop_attempt = resolve_experimental_stop_attempt(master_pending);
        let slave_stop_attempt = resolve_experimental_stop_attempt(slave_pending);
        self.master.driver.request_stop();
        self.slave.driver.request_stop();
        (
            ExperimentalRawClockErrorState::from_active(self),
            ExperimentalFaultShutdown {
                master_stop_attempt,
                slave_stop_attempt,
            },
        )
    }
}
```

`enqueue_experimental_stop_attempt()` should mirror the existing dual-arm `fault_shutdown()` helper, but accept `Piper<Active<MitPassthroughMode>, SoftRealtime>`. If the existing stop-attempt helpers are private in `dual_arm.rs`, either make the reusable pieces `pub(crate)` or duplicate the small helper in `dual_arm_raw_clock.rs`. The invariant is that both arms are latched faulted and both stop attempts are made under the same bounded deadline even if one attempt fails immediately.

- [ ] **Step 11: Add unit tests for shutdown, timing, and command ordering**

Add unit seams in `dual_arm_raw_clock.rs` for snapshot input and command recording so these tests do not open hardware. Reuse state-machine fake adapters only when a test needs actual `Piper<Active<MitPassthroughMode>, SoftRealtime>` command serialization. Test:

- slave command submits before master command, matching existing dual-arm order expectations.
- runtime gate failure prevents another normal command.
- timing failure calls `fault_shutdown()` and records both master/slave stop attempts.
- snapshot read failure calls `fault_shutdown()` and records both master/slave stop attempts.
- command submission failure calls `fault_shutdown()` and records both master/slave stop attempts.
- runtime transport fault calls `fault_shutdown()` and records both master/slave stop attempts.
- missing raw feedback timing fails closed before command submission.
- per-tick skew is computed from mapped `newest_raw_feedback_timing`, not from host receive timestamps or command send time.
- warmup starting with an empty estimator collects not-ready samples instead of aborting on the first tick.
- runtime report includes max and p95 inter-arm skew.
- `disable_both` attempts both sides on clean cancellation.

Run: `cargo test -p piper-client dual_arm_raw_clock -- --nocapture`

Expected: PASS.

- [ ] **Step 12: Export experimental module**

Modify `crates/piper-client/src/lib.rs`:

```rust
pub mod dual_arm_raw_clock;
```

Re-export only the types needed by `piper-cli`, with names that include `Experimental`:

```rust
pub use dual_arm_raw_clock::{
    ExperimentalRawClockConfig, ExperimentalRawClockDualArmActive,
    ExperimentalRawClockDualArmStandby, RawClockRuntimeReport,
};
```

- [ ] **Step 13: Commit**

```bash
git add crates/piper-client/src/lib.rs crates/piper-client/src/dual_arm_raw_clock.rs
git commit -m "feat: add experimental raw clock dual arm runtime"
```

## Task 6: Add CLI Flags and Config for Experimental Raw Clock

**Files:**
- Modify: `apps/cli/src/commands/teleop.rs`
- Modify: `apps/cli/src/teleop/config.rs`
- Test: `apps/cli/src/commands/teleop.rs`
- Test: `apps/cli/src/teleop/config.rs`

- [ ] **Step 1: Write failing CLI parse test**

In `apps/cli/src/commands/teleop.rs`, add:

```rust
#[test]
fn dual_arm_command_parses_experimental_calibrated_raw_options() {
    let cmd = TeleopCommand::try_parse_from([
        "teleop",
        "dual-arm",
        "--master-interface",
        "can0",
        "--slave-interface",
        "can1",
        "--mode",
        "master-follower",
        "--experimental-calibrated-raw",
        "--raw-clock-warmup-secs",
        "10",
        "--raw-clock-inter-arm-skew-max-us",
        "2000",
    ])
    .expect("experimental raw clock command should parse");

    match cmd.action {
        TeleopAction::DualArm(args) => {
            assert!(args.experimental_calibrated_raw);
            assert_eq!(args.raw_clock_warmup_secs, Some(10));
            assert_eq!(args.raw_clock_inter_arm_skew_max_us, Some(2000));
        },
    }
}
```

- [ ] **Step 2: Run parse test and verify it fails**

Run: `cargo test -p piper-cli dual_arm_command_parses_experimental_calibrated_raw_options -- --nocapture`

Expected: FAIL with missing fields.

- [ ] **Step 3: Add CLI args**

Modify `TeleopDualArmArgs`:

```rust
#[arg(long)]
pub experimental_calibrated_raw: bool,
#[arg(long)]
pub raw_clock_warmup_secs: Option<u64>,
#[arg(long)]
pub raw_clock_residual_p95_us: Option<u64>,
#[arg(long)]
pub raw_clock_residual_max_us: Option<u64>,
#[arg(long)]
pub raw_clock_drift_abs_ppm: Option<f64>,
#[arg(long)]
pub raw_clock_sample_gap_max_ms: Option<u64>,
#[arg(long)]
pub raw_clock_last_sample_age_ms: Option<u64>,
#[arg(long)]
pub raw_clock_inter_arm_skew_max_us: Option<u64>,
```

Update `default_for_tests()`.

- [ ] **Step 4: Add config model**

In `apps/cli/src/teleop/config.rs`, add:

```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct TeleopRawClockConfig {
    pub warmup_secs: Option<u64>,
    pub residual_p95_us: Option<u64>,
    pub residual_max_us: Option<u64>,
    pub drift_abs_ppm: Option<f64>,
    pub sample_gap_max_ms: Option<u64>,
    pub last_sample_age_ms: Option<u64>,
    pub inter_arm_skew_max_us: Option<u64>,
}
```

Add `raw_clock: Option<TeleopRawClockConfig>` to `TeleopConfigFile`.

Add resolved settings:

```rust
pub struct TeleopRawClockSettings {
    pub experimental_calibrated_raw: bool,
    pub warmup_secs: u64,
    pub residual_p95_us: u64,
    pub residual_max_us: u64,
    pub drift_abs_ppm: f64,
    pub sample_gap_max_ms: u64,
    pub last_sample_age_ms: u64,
    pub inter_arm_skew_max_us: u64,
}
```

Defaults:

```rust
experimental_calibrated_raw = args.experimental_calibrated_raw
warmup_secs = 10
residual_p95_us = 500
residual_max_us = 2000
drift_abs_ppm = 100.0
sample_gap_max_ms = 20
last_sample_age_ms = 20
inter_arm_skew_max_us = 2000
```

`experimental_calibrated_raw` is CLI-only. The config file may set thresholds, but it must not enable experimental motion. Keep the explicit opt-in flag in `TeleopDualArmArgs`; resolved config copies only that boolean from CLI args.

- [ ] **Step 5: Write config validation tests**

Add tests in `apps/cli/src/teleop/config.rs`:

```rust
#[test]
fn experimental_raw_clock_rejects_bilateral_mode() {
    let args = TeleopDualArmArgs {
        experimental_calibrated_raw: true,
        mode: Some(TeleopMode::Bilateral),
        ..TeleopDualArmArgs::default_for_tests()
    };

    let err = ResolvedTeleopConfig::resolve(args, None).unwrap_err();
    assert!(err.to_string().contains("master-follower"));
}

#[test]
fn cli_raw_clock_values_override_file_values() {
    let file = TeleopConfigFile {
        raw_clock: Some(TeleopRawClockConfig {
            warmup_secs: Some(30),
            ..Default::default()
        }),
        ..Default::default()
    };
    let args = TeleopDualArmArgs {
        experimental_calibrated_raw: true,
        raw_clock_warmup_secs: Some(10),
        ..TeleopDualArmArgs::default_for_tests()
    };

    let resolved = ResolvedTeleopConfig::resolve(args, Some(file)).unwrap();
    assert!(resolved.raw_clock.experimental_calibrated_raw);
    assert_eq!(resolved.raw_clock.warmup_secs, 10);
}

#[test]
fn config_file_cannot_enable_experimental_raw_clock_without_cli_flag() {
    let config_text = r#"
[raw_clock]
experimental_calibrated_raw = true
"#;

    let err = toml::from_str::<TeleopConfigFile>(config_text).unwrap_err();
    assert!(err.to_string().contains("unknown field"));
}

#[test]
fn raw_clock_threshold_file_does_not_enable_experimental_mode() {
    let file = TeleopConfigFile {
        raw_clock: Some(TeleopRawClockConfig {
            warmup_secs: Some(30),
            ..Default::default()
        }),
        ..Default::default()
    };
    let args = TeleopDualArmArgs {
        experimental_calibrated_raw: false,
        ..TeleopDualArmArgs::default_for_tests()
    };

    let resolved = ResolvedTeleopConfig::resolve(args, Some(file)).unwrap();
    assert!(!resolved.raw_clock.experimental_calibrated_raw);
    assert_eq!(resolved.raw_clock.warmup_secs, 30);
}
```

- [ ] **Step 6: Run CLI/config tests**

Run: `cargo test -p piper-cli dual_arm_command_parses_experimental_calibrated_raw_options -- --nocapture`

Expected: PASS.

Run: `cargo test -p piper-cli experimental_raw_clock_rejects_bilateral_mode -- --nocapture`

Expected: PASS.

Run: `cargo test -p piper-cli cli_raw_clock_values_override_file_values -- --nocapture`

Expected: PASS.

Run: `cargo test -p piper-cli config_file_cannot_enable_experimental_raw_clock_without_cli_flag -- --nocapture`

Expected: PASS.

Run: `cargo test -p piper-cli raw_clock_threshold_file_does_not_enable_experimental_mode -- --nocapture`

Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add apps/cli/src/commands/teleop.rs apps/cli/src/teleop/config.rs
git commit -m "feat: add experimental raw clock teleop config"
```

## Task 7: Add Timing Fields to Teleop Reports

**Files:**
- Modify: `apps/cli/src/teleop/report.rs`
- Test: `apps/cli/src/teleop/report.rs`

- [ ] **Step 1: Write failing report serialization test**

In `apps/cli/src/teleop/report.rs`, add:

```rust
#[test]
fn report_serializes_experimental_raw_clock_timing() {
    let mut input = sample_input(false, &BilateralRunReport::default());
    input.timing = Some(ReportTiming {
        timing_source: "calibrated_hw_raw".to_string(),
        experimental: true,
        strict_realtime: false,
        master_clock_drift_ppm: Some(3.0),
        slave_clock_drift_ppm: Some(-2.0),
        master_residual_p95_us: Some(120),
        slave_residual_p95_us: Some(130),
        max_estimated_inter_arm_skew_us: Some(900),
        estimated_inter_arm_skew_p95_us: Some(400),
        clock_health_failures: 0,
    });

    let value = serde_json::to_value(TeleopJsonReport::from_run(input)).unwrap();
    assert_eq!(value["timing"]["timing_source"], "calibrated_hw_raw");
    assert_eq!(value["timing"]["experimental"], true);
    assert_eq!(value["timing"]["strict_realtime"], false);
}
```

- [ ] **Step 2: Run test and verify it fails**

Run: `cargo test -p piper-cli report_serializes_experimental_raw_clock_timing -- --nocapture`

Expected: FAIL with missing `ReportTiming` or `timing` field.

- [ ] **Step 3: Add report timing model**

Add:

```rust
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ReportTiming {
    pub timing_source: String,
    pub experimental: bool,
    pub strict_realtime: bool,
    pub master_clock_drift_ppm: Option<f64>,
    pub slave_clock_drift_ppm: Option<f64>,
    pub master_residual_p95_us: Option<u64>,
    pub slave_residual_p95_us: Option<u64>,
    pub max_estimated_inter_arm_skew_us: Option<u64>,
    pub estimated_inter_arm_skew_p95_us: Option<u64>,
    pub clock_health_failures: u64,
}
```

Add optional field to `TeleopJsonReport`:

```rust
#[serde(skip_serializing_if = "Option::is_none")]
pub timing: Option<ReportTiming>,
```

Add `timing: Option<ReportTiming>` to `TeleopReportInput<'a>`.

Do not bump existing report schema for normal StrictRealtime runs in this task. The optional `timing` block is serialized only for experimental calibrated-raw runs.

- [ ] **Step 4: Update human report output**

In `print_human_report()`, print timing block only when present:

```rust
if let Some(timing) = &report.timing {
    writeln!(writer, "timing_source={}", timing.timing_source)?;
    writeln!(writer, "experimental={}", timing.experimental)?;
    writeln!(writer, "strict_realtime={}", timing.strict_realtime)?;
    writeln!(
        writer,
        "raw_clock max_skew_us={} p95_skew_us={}",
        timing.max_estimated_inter_arm_skew_us.unwrap_or(0),
        timing.estimated_inter_arm_skew_p95_us.unwrap_or(0)
    )?;
}
```

- [ ] **Step 5: Run report tests**

Run: `cargo test -p piper-cli report_serializes_experimental_raw_clock_timing -- --nocapture`

Expected: PASS.

Run: `cargo test -p piper-cli teleop::report -- --nocapture`

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add apps/cli/src/teleop/report.rs
git commit -m "feat: report experimental raw clock timing"
```

## Task 8: Wire Experimental Raw-Clock Workflow

**Files:**
- Modify: `apps/cli/src/teleop/workflow.rs`
- Modify: `apps/cli/src/teleop/target.rs`
- Test: `apps/cli/src/teleop/workflow.rs`
- Test: `apps/cli/src/teleop/target.rs`

- [ ] **Step 1: Write failing workflow route test**

In `apps/cli/src/teleop/workflow.rs`, extend the fake backend or add an experimental fake backend. Add:

```rust
#[test]
fn experimental_raw_clock_uses_experimental_backend_path() {
    let args = TeleopDualArmArgs {
        experimental_calibrated_raw: true,
        master_interface: Some("can0".to_string()),
        slave_interface: Some("can1".to_string()),
        mode: Some(TeleopMode::MasterFollower),
        yes: true,
        ..TeleopDualArmArgs::default_for_tests()
    };
    let backend = FakeTeleopBackend::default().with_experimental_raw_clock_success();

    let status = run_workflow_for_test(args, backend.clone()).unwrap();

    assert_eq!(status, TeleopExitStatus::Success);
    assert_eq!(backend.calls().experimental_raw_clock_runs, 1);
    assert_eq!(backend.calls().strict_runs, 0);
}
```

- [ ] **Step 2: Run route test and verify it fails**

Run: `cargo test -p piper-cli experimental_raw_clock_uses_experimental_backend_path -- --nocapture`

Expected: FAIL because workflow has no experimental route.

- [ ] **Step 3: Add experimental backend trait boundary**

Keep the production `TeleopBackend` path unchanged. Add a separate trait or methods:

```rust
pub trait ExperimentalRawClockTeleopBackend {
    fn connect_soft(&mut self, targets: &RoleTargets, baud_rate: u32) -> Result<()>;
    fn warmup_raw_clock(
        &mut self,
        settings: &TeleopRawClockSettings,
        cancel_signal: Arc<AtomicBool>,
    ) -> Result<RawClockWarmupSummary>;
    fn capture_calibration(&self, map: JointMirrorMap) -> Result<DualArmCalibration>;
    fn enable_mit_passthrough(&mut self, master: MitModeConfig, slave: MitModeConfig) -> Result<()>;
    fn run_master_follower_raw_clock(
        &mut self,
        settings: RuntimeTeleopSettingsHandle,
        raw_clock: TeleopRawClockSettings,
        cancel_signal: Arc<AtomicBool>,
    ) -> Result<ExperimentalRawClockRunExit>;
}
```

If sharing one backend object with production is cleaner, make `RealTeleopBackend` implement both traits. Keep test fakes explicit so production and experimental calls are distinguishable.

- [ ] **Step 4: Implement experimental route before production strict build**

In `run_workflow_on_platform()`, after resolving config and targets:

```rust
if resolved.raw_clock.experimental_calibrated_raw {
    return run_experimental_raw_clock_workflow(args, backend, io, platform, resolved, config_file);
}
```

The experimental route must:

- require concrete SocketCAN targets
- reject non-Linux
- connect SoftRealtime standby arms
- run inline raw-clock warmup on the same driver snapshots before operator confirmation
- print `experimental=true` and `strict_realtime=false`
- require confirmation unless `--yes`
- reject cancellation before enable as success without motion
- build `ReportTiming`
- classify any timing health failure as failure

- [ ] **Step 5: Add target tests**

In `apps/cli/src/teleop/target.rs`, add tests:

```rust
#[test]
fn experimental_raw_clock_requires_socketcan_targets() {
    let args = TeleopDualArmArgs {
        experimental_calibrated_raw: true,
        master_serial: Some("MASTER".to_string()),
        slave_serial: Some("SLAVE".to_string()),
        ..TeleopDualArmArgs::default_for_tests()
    };

    let err = resolve_role_targets(&args, None, TeleopPlatform::Linux).unwrap_err();
    assert!(err.to_string().contains("GS-USB"));
}

#[test]
fn experimental_raw_clock_rejects_default_or_omitted_targets() {
    let args = TeleopDualArmArgs {
        experimental_calibrated_raw: true,
        master_interface: None,
        slave_interface: None,
        master_serial: None,
        slave_serial: None,
        ..TeleopDualArmArgs::default_for_tests()
    };

    let err = resolve_role_targets(&args, None, TeleopPlatform::Linux).unwrap_err();
    assert!(err.to_string().contains("explicit SocketCAN"));
}
```

Keep existing concrete GS-USB rejection in non-experimental path until a separate SoftRealtime selector implementation exists.

- [ ] **Step 6: Add timing health failure test**

In workflow tests:

```rust
#[test]
fn experimental_raw_clock_health_failure_reports_failure() {
    let args = experimental_args();
    let backend = FakeTeleopBackend::default().with_experimental_timing_failure("inter-arm skew");

    let err = run_workflow_for_test(args, backend.clone()).unwrap_err();

    assert!(err.to_string().contains("inter-arm skew"));
    assert!(backend.calls().fault_shutdown_attempted);
    assert_eq!(backend.calls().master_stop_attempts, 1);
    assert_eq!(backend.calls().slave_stop_attempts, 1);
}
```

- [ ] **Step 7: Run workflow tests**

Run: `cargo test -p piper-cli experimental_raw_clock -- --nocapture`

Expected: PASS.

Run: `cargo test -p piper-cli teleop::workflow -- --nocapture`

Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add apps/cli/src/teleop/workflow.rs apps/cli/src/teleop/target.rs
git commit -m "feat: wire experimental raw clock teleop workflow"
```

## Task 9: Implement Real Experimental Backend

**Files:**
- Modify: `apps/cli/src/teleop/workflow.rs`
- Modify: `apps/cli/src/teleop/report.rs`
- Test: `apps/cli/src/teleop/workflow.rs`

- [ ] **Step 1: Write failing real-backend construction test**

Add a test that does not open hardware but verifies target-to-builder mapping:

```rust
#[test]
fn real_experimental_backend_formats_socketcan_targets_for_startup_summary() {
    let targets = RoleTargets {
        master: ConcreteTeleopTarget::SocketCan { iface: "can0".to_string() },
        slave: ConcreteTeleopTarget::SocketCan { iface: "can1".to_string() },
    };

    let summary = StartupSummary::experimental_raw_clock_for_tests(targets);

    assert_eq!(summary.timing_source.as_deref(), Some("calibrated_hw_raw"));
    assert!(summary.experimental);
}
```

- [ ] **Step 2: Run test and verify it fails**

Run: `cargo test -p piper-cli real_experimental_backend_formats_socketcan_targets_for_startup_summary -- --nocapture`

Expected: FAIL with missing experimental summary fields.

- [ ] **Step 3: Implement real connect using `require_motion()`**

In `RealTeleopBackend`, add storage for experimental SoftRealtime standby/active objects:

```rust
experimental_standby: Option<ExperimentalRawClockDualArmStandby>,
experimental_active: Option<ExperimentalRawClockDualArmActive>,
```

Experimental connect should call `PiperBuilder::build()?` for each arm using explicit SocketCAN interfaces from `RoleTargets`, then match:

```rust
match connected.require_motion()? {
    MotionConnectedPiper::Soft(MotionConnectedState::Standby(standby)) => standby,
    MotionConnectedPiper::Strict(_) => bail!("experimental calibrated raw path expected SoftRealtime; use normal StrictRealtime teleop"),
    MotionConnectedPiper::Soft(MotionConnectedState::Maintenance(_))
    | MotionConnectedPiper::Strict(MotionConnectedState::Maintenance(_)) => {
        bail!("maintenance required before experimental teleop")
    }
}
```

This avoids `require_strict()` only in the experimental path.

- [ ] **Step 4: Implement inline warmup using piper-client runtime snapshots**

Do not open a parallel receive socket for runtime warmup. Use the `ExperimentalRawClockDualArmStandby::warmup()` method from Task 5 so warmup consumes the same `AlignedMotionState::newest_raw_feedback_timing()` values that will be used during motion.

Convert `TeleopRawClockSettings` to:

```rust
let estimator_thresholds = RawClockThresholds {
    warmup_samples: (raw_clock.warmup_secs * 400).max(4) as usize,
    warmup_window_us: raw_clock.warmup_secs * 1_000_000,
    residual_p95_us: raw_clock.residual_p95_us,
    residual_max_us: raw_clock.residual_max_us,
    drift_abs_ppm: raw_clock.drift_abs_ppm,
    sample_gap_max_us: raw_clock.sample_gap_max_ms * 1_000,
    last_sample_age_us: raw_clock.last_sample_age_ms * 1_000,
};
let runtime_thresholds = RawClockRuntimeThresholds {
    inter_arm_skew_max_us: raw_clock.inter_arm_skew_max_us,
    last_sample_age_us: raw_clock.last_sample_age_ms * 1_000,
};
```

Warmup succeeds only when:

- warmup duration reaches `raw_clock.warmup_secs`
- both estimator health values are healthy
- both sides have raw feedback timing from `hw_raw`
- inter-arm skew is within `raw_clock.inter_arm_skew_max_us`

If warmup completes unhealthy, return an error before operator confirmation.

- [ ] **Step 5: Implement real run with piper-client runtime**

After operator confirmation, call `ExperimentalRawClockDualArmStandby::enable_mit_passthrough()` to move the warmed standby runtime into active mode while preserving `RawClockRuntimeTiming`. Then call the experimental piper-client runtime added in Task 5. Convert its runtime report to `ReportTiming`:

```rust
ReportTiming {
    timing_source: "calibrated_hw_raw".to_string(),
    experimental: true,
    strict_realtime: false,
    master_clock_drift_ppm: Some(report.master.drift_ppm),
    slave_clock_drift_ppm: Some(report.slave.drift_ppm),
    master_residual_p95_us: Some(report.master.residual_p95_us),
    slave_residual_p95_us: Some(report.slave.residual_p95_us),
    max_estimated_inter_arm_skew_us: Some(report.max_inter_arm_skew_us),
    estimated_inter_arm_skew_p95_us: Some(report.inter_arm_skew_p95_us),
    clock_health_failures: report.clock_health_failures,
}
```

- [ ] **Step 6: Run targeted workflow tests**

Run: `cargo test -p piper-cli experimental_raw_clock -- --nocapture`

Expected: PASS.

Run: `cargo test -p piper-cli teleop::report -- --nocapture`

Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add apps/cli/src/teleop/workflow.rs apps/cli/src/teleop/report.rs
git commit -m "feat: add real experimental raw clock backend"
```

## Task 10: Document Experimental Operator Flow

**Files:**
- Modify: `apps/cli/TELEOP_DUAL_ARM.md`
- Modify: `crates/piper-sdk/examples/README.md`

- [ ] **Step 1: Add docs section**

In `apps/cli/TELEOP_DUAL_ARM.md`, add a section:

```markdown
## Experimental Calibrated Raw-Clock Mode

This mode is for lab validation with Linux SocketCAN `gs_usb` interfaces that
expose `hardware-raw-clock` but cannot satisfy the production StrictRealtime
path. It is not StrictRealtime and must be enabled explicitly.

First run the read-only probe:

```bash
cargo run -p piper-sdk --example socketcan_raw_clock_probe -- \
  --left-interface can0 \
  --right-interface can1 \
  --duration-secs 300 \
  --out artifacts/teleop/raw-clock-probe.json
```

Then run a bounded master-follower trial:

```bash
piper-cli teleop dual-arm \
  --master-interface can0 \
  --slave-interface can1 \
  --mode master-follower \
  --disable-gripper-mirror \
  --experimental-calibrated-raw \
  --max-iterations 12000 \
  --report-json artifacts/teleop/raw-clock-report.json
```

The report must show `timing_source=calibrated_hw_raw`,
`experimental=true`, and `strict_realtime=false`.
```

- [ ] **Step 2: Add manual acceptance checklist**

Add checklist:

- probe runs for 5 minutes without raw timestamp regression
- p95 residual and max skew are within configured thresholds
- bounded `master-follower` exits cleanly
- report marks run experimental and non-strict
- disconnecting one feedback path causes bounded shutdown

- [ ] **Step 3: Run docs grep**

Run: `rg -n "Experimental Calibrated Raw-Clock|socketcan_raw_clock_probe|experimental-calibrated-raw|strict_realtime=false" apps/cli/TELEOP_DUAL_ARM.md crates/piper-sdk/examples/README.md`

Expected: all patterns found.

- [ ] **Step 4: Commit**

```bash
git add apps/cli/TELEOP_DUAL_ARM.md crates/piper-sdk/examples/README.md
git commit -m "docs: add experimental raw clock teleop guide"
```

## Task 11: Final Verification

**Files:**
- No source edits unless verification finds issues.

- [ ] **Step 1: Run formatting**

Run: `cargo fmt --all -- --check`

Expected: PASS.

- [ ] **Step 2: Run focused crate tests**

Run:

```bash
cargo test -p piper-tools raw_clock -- --nocapture
cargo test -p piper-can raw_timestamp -- --nocapture
cargo test -p piper-can startup_probe -- --nocapture
cargo test -p piper-driver raw_feedback_timing -- --nocapture
cargo test -p piper-driver monotonic_micros_delegates_to_piper_can_epoch -- --nocapture
cargo test -p piper-driver aligned_motion_exposes_newest_raw_feedback_timing_from_contributing_frames -- --nocapture
cargo test -p piper-sdk --example socketcan_raw_clock_probe
cargo test -p piper-client dual_arm_raw_clock -- --nocapture
cargo test -p piper-client soft_mit_passthrough_command_torques_confirmed_applies_firmware_quirks -- --nocapture
cargo test -p piper-cli experimental_raw_clock -- --nocapture
cargo test -p piper-cli report_serializes_experimental_raw_clock_timing -- --nocapture
```

Expected: all PASS.

- [ ] **Step 3: Run broader non-hardware tests**

Run:

```bash
cargo test -p piper-tools
cargo test -p piper-can --lib
cargo test -p piper-driver --lib
cargo test -p piper-client --lib
cargo test -p piper-cli --all-targets
cargo test -p piper-sdk --example socketcan_raw_clock_probe
```

Expected: all PASS.

- [ ] **Step 4: Run clippy**

Run: `cargo clippy --workspace --all-targets --all-features -- -D warnings`

Expected: PASS.

- [ ] **Step 5: Verify help text**

Run: `cargo run -p piper-cli -- teleop dual-arm --help`

Expected: help includes `--experimental-calibrated-raw` and raw-clock threshold options.

- [ ] **Step 6: Record manual hardware commands without running by default**

Do not run hardware tests automatically. Record these commands for the operator:

```bash
cargo run -p piper-sdk --example socketcan_raw_clock_probe -- \
  --left-interface can0 \
  --right-interface can1 \
  --duration-secs 300 \
  --out artifacts/teleop/raw-clock-probe.json

cargo run -p piper-cli -- teleop dual-arm \
  --master-interface can0 \
  --slave-interface can1 \
  --mode master-follower \
  --disable-gripper-mirror \
  --experimental-calibrated-raw \
  --max-iterations 12000 \
  --report-json artifacts/teleop/raw-clock-report.json
```

- [ ] **Step 7: Commit final fixes if any**

If verification required fixes:

```bash
git add <fixed-files>
git commit -m "fix: stabilize experimental raw clock teleop"
```

If no fixes were required, do not create an empty commit.

- [ ] **Step 8: Summarize completion**

Report:

- commit list
- verification commands and results
- any hardware checks not run
- remaining post-v1 calibration research notes
