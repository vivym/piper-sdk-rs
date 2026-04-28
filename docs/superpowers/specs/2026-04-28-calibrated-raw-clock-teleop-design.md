# Calibrated Raw Clock Teleoperation Design

## Summary

Add an experimental GS-USB dual-arm teleoperation path based on calibrated raw
hardware timestamps. The goal is to let lab users test isomorphic
`master-follower` teleoperation with two Linux SocketCAN `gs_usb` interfaces
that expose `hardware-raw-clock` but do not expose a PTP hardware clock or
kernel-transformed hardware timestamps.

This design deliberately does not redefine `StrictRealtime`. Existing
StrictRealtime semantics remain tied to trusted timestamps that are already in a
common system time domain. The new path is an explicit lab mode that estimates a
per-interface mapping from raw device time to host monotonic time, continuously
checks the error bounds, and shuts down if the estimate becomes unhealthy.

The first implementation should prove the timing model with a read-only probe
before enabling motion. The probe produces operator evidence, but the teleop
command does not consume a prior probe artifact in v1; it runs its own inline
warmup calibration before asking for enable confirmation. Motion support is
limited to the existing isomorphic dual-arm `master-follower` CLI path and must
be opt-in with an experimental flag.

## Goals

- Support lab testing with two kernel `gs_usb` SocketCAN interfaces such as
  `can0` and `can1`.
- Use raw hardware receive timestamps when `hw_raw` is present but `hw_trans`
  is not.
- Estimate `host_mono_us ~= slope * hw_raw_us + offset` per interface.
- Continuously report calibration quality, drift, residuals, freshness, and
  inter-arm skew.
- Add a read-only probe tool that can run before any motor enable path.
- Add an explicitly experimental teleop mode only after its inline warmup
  calibration passes and the same estimator can enforce runtime health gates.
- Preserve current `StrictRealtime` behavior and keep the existing production
  teleop path unchanged.
- Make every report state that calibrated raw-clock teleop is experimental and
  not StrictRealtime.

## Non-Goals

- Do not treat `hardware-raw-clock` as existing `StrictRealtime`.
- Do not bypass `ConnectedPiper::require_strict()` in the production path.
- Do not claim production safety equivalence with a true common-time hardware
  timestamp source.
- Do not implement bilateral force reflection in the first experimental raw
  clock path.
- Do not support network-distributed teleoperation.
- Do not require MuJoCo or any addon dependency.
- Do not change the default behavior of `piper-cli teleop dual-arm`.

## Existing Context

The current dual-arm CLI is documented in:

- `docs/superpowers/specs/2026-04-27-isomorphic-dual-arm-teleop-cli-design.md`
- `apps/cli/TELEOP_DUAL_ARM.md`

The current SDK dual-arm session is implemented in
`crates/piper-client/src/dual_arm.rs`. `DualArmBuilder::build()` calls
`ConnectedPiper::require_strict()` for both arms, so it only accepts
StrictRealtime connections.

SocketCAN timestamp startup probing lives in
`crates/piper-can/src/socketcan/split.rs`. It currently classifies robot
feedback as:

- `hw_trans` hardware timestamp -> `StrictRealtime`
- `system` software timestamp -> `SoftRealtime`
- no usable timestamp -> no realtime capability

The `timestamp_verification` example prints the three relevant Linux
`SCM_TIMESTAMPING` fields:

- `system`
- `hw_trans`
- `hw_raw`

On the target lab setup, `ethtool -T can0` and `ethtool -T can1` show
`hardware-raw-clock` but `PTP Hardware Clock: none`. That means Linux can expose
raw device timestamps, but the kernel does not provide a synchronized
hardware-to-system conversion for those interfaces.

## Architecture

Introduce a small timing subsystem with three layers:

1. Raw timestamp extraction
2. Per-interface raw-to-host calibration
3. Experimental teleop health gating

The production `StrictRealtime` path remains unchanged. The experimental path
must use separate names, separate flags, and separate report fields so operators
cannot confuse calibrated raw-clock timing with StrictRealtime.

Recommended internal capability name:

```text
CalibratedRawRealtime
```

This name may be represented as a separate enum variant, a wrapper around
SoftRealtime, or an experiment-only runtime object. The important API rule is
that it must not satisfy `StrictCapability`.

## V1 Crate Boundaries

The first implementation should use these ownership boundaries:

- `piper-can`: expose Linux SocketCAN raw timestamp samples behind the existing
  SocketCAN feature gate. This layer owns `SCM_TIMESTAMPING` extraction and the
  per-frame `system` / `hw_trans` / `hw_raw` fields. It must not own teleop
  policy.
- `piper-tools`: own the pure raw-clock estimator and health metrics. The
  estimator should accept generic `(raw_time, host_time)` samples and must not
  depend on `piper-can`, `piper-driver`, or `piper-client`.
- `piper-client`: own the experimental dual-arm runtime glue. It may reuse
  SoftRealtime MIT passthrough and the `piper-tools` estimator, but it must not
  expose calibrated raw timing as `StrictCapability`.
- `piper-cli`: own operator-facing flags, startup summaries, reports, and the
  decision to enable the experimental mode.
- `piper-sdk` examples: own the read-only probe binary that combines
  `piper-can` raw samples with the `piper-tools` estimator.

`piper-driver` should not receive new public timing semantics in v1 unless the
implementation plan discovers an unavoidable integration point. Keeping the
estimator outside `piper-driver` prevents the experimental lab path from
changing production StrictRealtime behavior.

## Raw Timestamp Extraction

The SocketCAN RX path should preserve raw hardware timestamp data when Linux
provides it. Existing `ReceivedFrame` metadata only carries a normalized
timestamp and provenance. The experimental path needs access to:

```rust
struct RawTimestampSample {
    iface: String,
    can_id: u32,
    host_rx_mono_us: u64,
    system_ts_us: Option<u64>,
    hw_trans_us: Option<u64>,
    hw_raw_us: Option<u64>,
}
```

This can be implemented as an experimental diagnostics API without changing the
normal `PiperFrame` timestamp contract. The raw probe and calibrated teleop
path should consume this richer sample type directly.

If `hw_trans_us` is present, the existing StrictRealtime path should keep using
it. The calibrated raw path is for the case where `hw_raw_us` is present and
`hw_trans_us` is absent.

## Clock Estimator

Each interface owns an independent estimator:

```text
host_mono_us = slope * hw_raw_us + offset
```

The estimator consumes tuples:

```text
(hw_raw_us, host_rx_mono_us)
```

The host receive timestamp is an upper-bound observation of when the frame
arrived at userspace. It includes USB, kernel, and scheduling latency. For that
reason, the estimator must be robust against positive delay outliers.

Initial implementation requirements:

- Maintain a sliding window of recent samples.
- Reject non-monotonic raw timestamps in v1. Wrap/reset recovery is future
  work; the first implementation should fail closed instead of attempting to
  repair a clock discontinuity.
- Estimate slope and offset using low-delay samples, not the full mean of all
  samples.
- Track residuals after mapping raw time into host time.
- Expose health metrics:
  - sample count
  - window duration
  - estimated drift in ppm
  - p50/p95/p99 residual
  - maximum residual
  - maximum sample gap
  - last sample age
  - raw timestamp regression count

A simple first estimator is acceptable if it is conservative:

1. Collect samples for a fixed warmup window.
2. Bucket samples by time.
3. Prefer the lowest host delay samples in each bucket.
4. Fit a line through the selected samples.
5. Keep updating with a bounded sliding window.
6. Mark the estimator unhealthy when residuals or drift exceed thresholds.

## Read-Only Probe

Add a probe before any motion path:

```bash
cargo run -p piper-sdk --example socketcan_raw_clock_probe -- \
  --left-interface can0 \
  --right-interface can1 \
  --duration-secs 300 \
  --out artifacts/teleop/raw-clock-probe.json
```

The probe must not enable motors. It should record:

- resolved interface metadata
- `ethtool -T`-equivalent timestamp capability summary when available
- per-frame raw timestamp samples
- per-interface estimator metrics
- estimated inter-arm skew metrics
- pass/fail decision for the configured thresholds

The JSON output should be machine-readable and include enough detail to debug a
failed calibration without rerunning the full experiment.

## Inter-Arm Skew Metric

V1 uses paired feedback-time skew, not estimator offset uncertainty or command
submission skew.

For each control tick, each arm must expose the mapped host-time of the newest
raw-timestamped robot feedback frame that contributed to the snapshot used by
the controller:

```text
master_feedback_time_us = master_estimator.map(master_hw_raw_us)
slave_feedback_time_us = slave_estimator.map(slave_hw_raw_us)
inter_arm_skew_us = abs(master_feedback_time_us - slave_feedback_time_us)
```

If a snapshot cannot identify a raw-timestamped feedback frame for either arm,
that tick is a timing-health failure. The runtime threshold applies to every
control tick: any `inter_arm_skew_us > inter_arm_skew_max_us` fails the run and
enters bounded shutdown. Reports should include both max and p95 skew over the
run, but the health gate is the per-tick maximum.

## Experimental Teleop Flow

Add a CLI opt-in flag:

```bash
piper-cli teleop dual-arm \
  --master-interface can0 \
  --slave-interface can1 \
  --mode master-follower \
  --disable-gripper-mirror \
  --experimental-calibrated-raw
```

Startup sequence:

1. Resolve concrete SocketCAN targets.
2. Verify both interfaces expose `hw_raw` receive timestamps.
3. Run a read-only calibration window, default 5-10 seconds.
4. Refuse to continue if estimator health does not meet thresholds.
5. Print a startup summary that clearly says:
   `timing_source=calibrated_hw_raw`, `experimental=true`,
   `strict_realtime=false`.
6. Require operator confirmation unless `--yes` is passed.
7. Enable the experimental dual-arm motion path.
8. Run only `master-follower` in the first implementation.
9. Disable both arms on normal completion, cancellation, or timing health
   failure.

Runtime health gates:

- `hw_raw` must remain present on both arms.
- Raw timestamps must remain monotonic. Any regression is a v1 timing-health
  failure.
- Last calibration sample age must remain below the configured maximum.
- p95 residual must remain below the configured threshold.
- Drift estimate must remain below the configured threshold.
- Estimated inter-arm skew must remain below the configured threshold.
- Read faults, submission faults, or runtime transport faults must use the same
  bounded shutdown behavior as the existing dual-arm loop.

Default thresholds should start conservative and be documented as lab defaults,
not production guarantees. Initial suggested values:

```text
warmup_secs = 10
residual_p95_us <= 500
residual_max_us <= 2000
drift_abs_ppm <= 100
sample_gap_max_ms <= 20
last_sample_age_ms <= 20
inter_arm_skew_max_us <= 2000
```

These values should be configurable for lab investigation, but a run using
overrides must record the overrides in the report.

## Reporting

The human and JSON reports must add calibrated raw-clock fields:

```text
timing_source = calibrated_hw_raw
experimental = true
strict_realtime = false
master_clock_drift_ppm = ...
slave_clock_drift_ppm = ...
master_residual_p95_us = ...
slave_residual_p95_us = ...
max_estimated_inter_arm_skew_us = ...
estimated_inter_arm_skew_p95_us = ...
clock_health_failures = ...
```

The existing clean-exit interpretation still applies: `cancelled` and
`max_iterations` are clean only if the timing health gates stayed healthy and
the arms returned to standby. Any timing-health failure is a non-success run
even if bounded shutdown succeeds.

## Safety Semantics

This path is for lab validation. It must not be silently enabled by config
defaults, platform defaults, or target syntax. Operators must pass an
experimental flag.

The implementation must keep these boundaries:

- Production `StrictRealtime` remains stricter than calibrated raw-clock timing.
- Calibrated raw-clock timing may enable an experiment-only teleop session but
  must not satisfy APIs that require `StrictCapability`.
- The report must make the timing source unambiguous.
- Timing health failures must shut down motion, not merely warn.
- If the estimator cannot prove freshness or skew, the controller must not send
  another normal command cycle.

## Testing

Unit tests:

- raw timestamp sample parsing preserves `hw_raw`
- estimator rejects non-monotonic raw timestamps
- estimator detects excessive residuals
- estimator detects excessive drift
- estimator maps two synthetic device clocks onto a common host timeline
- report serialization includes experimental timing fields
- CLI rejects `--experimental-calibrated-raw` without concrete SocketCAN targets
- CLI rejects bilateral mode for the first experimental implementation

Integration tests without hardware:

- fake dual interfaces with stable raw clocks pass warmup
- fake dual interfaces with delayed outliers remain healthy when the robust
  estimator can reject them
- fake dual interfaces with drift beyond threshold fail before enable
- fake runtime health failure triggers bounded shutdown report classification

Manual hardware tests:

1. Run the read-only probe for 5 minutes on `can0` and `can1`.
2. Inspect residuals, drift, sample gaps, and inter-arm skew.
3. Run experimental `master-follower` for a short bounded iteration count.
4. Confirm report fields mark the run experimental and non-strict.
5. Disconnect one feedback path and confirm bounded shutdown.

## Post-V1 Calibration Research

These questions should not block the v1 implementation plan:

- Whether the conservative v1 estimator should later be replaced by
  low-percentile line fit, robust regression, or a small Kalman-style clock
  filter.
- Which thresholds are realistic for the target GS-USB hardware over 5, 15, and
  60 minute runs after probe data is collected.
- Whether the raw device clock ever wraps or resets during normal operation on
  the target adapters, and whether a future version should recover instead of
  failing closed.
- Whether calibrated raw timing should later move deeper into `piper-driver`
  after the lab path has enough evidence to justify broader reuse.
