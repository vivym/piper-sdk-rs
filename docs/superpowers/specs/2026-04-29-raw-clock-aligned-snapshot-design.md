# Raw Clock Aligned Snapshot Design

## Summary

Add an alignment layer to the experimental calibrated raw-clock dual-arm teleop
runtime so the controller consumes time-aligned master/slave snapshots instead
of whatever latest feedback happened to be available on each independent CAN
interface.

Current raw-clock calibration maps both arms' hardware raw timestamps onto one
common time axis, but the runtime still compares and controls from the latest
master snapshot and latest slave snapshot read in the current loop. Because the
two CAN feedback streams are independent, those latest snapshots can be several
milliseconds apart. Recent hardware smoke runs showed healthy estimator
residuals and tracking while inter-arm raw-clock skew exceeded the 6ms smoke
gate. Expanding that gate indefinitely would hide the real issue: sample
pairing is not time-aligned.

The fix is to keep a short per-arm snapshot history, select snapshots for a
common target time in the recent past, and run the existing controller and
runtime gates on that selected pair. This applies only to the experimental
calibrated raw-clock path and must not affect production `StrictRealtime` or
non-raw-clock teleop modes.

## Goals

- Replace latest/latest raw-clock control pairing with aligned snapshot pairing
  in the experimental raw-clock runtime.
- Make the alignment lag configurable, defaulting to `5000us`.
- Use a conservative first-version selector: latest snapshot at or before the
  target time on each arm.
- Tolerate short alignment buffer misses without sending new commands, then
  fail after a configurable consecutive miss threshold.
- Keep estimator health gates fail-fast for residual p95, drift, sample gap,
  freshness, raw timestamp regressions, and sustained residual max failures.
- Report selected-pair skew separately from latest/latest diagnostic skew.
- Preserve existing master-follower control semantics and raw-clock calibration
  behavior.

## Non-Goals

- Do not add interpolation in the first version.
- Do not change the raw-clock estimator fit algorithm.
- Do not change driver hot/cold snapshot commit behavior.
- Do not relax raw-clock health thresholds as the primary fix.
- Do not change production `StrictRealtime` or non-raw-clock dual-arm teleop.
- Do not introduce bilateral/force-reflection behavior in this change.

## Existing Context

The experimental runtime lives mainly in
`crates/piper-client/src/dual_arm_raw_clock.rs`.

Important current pieces:

- `ExperimentalRawClockSnapshot` holds a `ControlSnapshot`, latest raw feedback
  timing, and feedback age.
- `RawClockRuntimeTiming::tick_from_snapshots()` ingests the latest master and
  slave snapshots, maps each side's raw hardware timestamp onto the common time
  axis, records `inter_arm_skew_us`, and returns a `RawClockTickTiming`.
- `RawClockRuntimeTiming::check_tick_with_debounce()` applies runtime gates,
  including the residual-max debounce added during smoke stabilization.
- The active runtime reads latest snapshots from both arms every loop, calls
  `tick_from_snapshots()`, and passes the resulting latest/latest
  `inter_arm_skew_us` into `raw_dual_arm_snapshot()` and the controller.
- CLI settings are resolved in `apps/cli/src/teleop/config.rs` and converted to
  `ExperimentalRawClockConfig` in `apps/cli/src/teleop/workflow.rs`.
- Report conversion and JSON/human output live in
  `apps/cli/src/teleop/workflow.rs` and `apps/cli/src/teleop/report.rs`.
- `scripts/run_teleop_smoke.sh` currently uses a temporary one-cycle
  `RAW_CLOCK_SKEW_US=10000` smoke gate so damping tests can continue while this
  design is implemented.

Recent hardware evidence:

```text
MASTER_DAMPING=0.8: clean, max_skew=4850us, p95_skew=2042us
MASTER_DAMPING=0.4: read freshness boundary before alignment fix
MASTER_DAMPING=0.2: estimator healthy, residual p95 < 400us, max_skew=6146us
```

The residual metrics stayed healthy while latest/latest inter-arm skew failed,
which points to sample pairing rather than estimator instability.

## Design

### Runtime Configuration

Extend `RawClockRuntimeThresholds` or an adjacent experimental raw-clock runtime
settings struct with:

```rust
pub alignment_lag_us: u64,
pub alignment_buffer_miss_consecutive_failures: u32,
```

Defaults:

```text
alignment_lag_us = 5000
alignment_buffer_miss_consecutive_failures = 3
```

These defaults assume both experimental raw-clock freshness gates are at least
`20000us`:

- `RawClockThresholds::last_sample_age_us`, used by estimator health
- `RawClockRuntimeThresholds::last_sample_age_us`, used by runtime gating

The current CLI raw-clock default already uses `last_sample_age_ms = 20` and
feeds both thresholds from that setting. The low-level
`ExperimentalRawClockConfig::default()` must also be internally valid after this
change. If either default freshness value is currently smaller than `20000us`,
update the experimental default to `20000us` rather than lowering the CLI/smoke
alignment lag.

CLI settings:

```text
--raw-clock-alignment-lag-us <US>
--raw-clock-alignment-buffer-miss-consecutive-failures <N>
```

Config file fields under `[raw_clock]`:

```toml
alignment_lag_us = 5000
alignment_buffer_miss_consecutive_failures = 3
```

Validation:

- `alignment_lag_us >= 1`
- `alignment_lag_us < RawClockThresholds::last_sample_age_us`
- `alignment_lag_us < RawClockRuntimeThresholds::last_sample_age_us`
- `alignment_buffer_miss_consecutive_failures >= 1`
- keep upper bounds consistent with existing raw-clock lab guardrails

The smoke script should pass the defaults explicitly. The existing temporary
10ms smoke skew gate remains a smoke-script default until aligned selection is
proven on hardware.

### Snapshot Buffer

Add a small per-arm buffer type in `piper-client`:

```rust
struct RawClockAlignedSnapshot {
    feedback_time_us: u64,
    snapshot: ExperimentalRawClockSnapshot,
}

struct RawClockSnapshotBuffer {
    samples: VecDeque<RawClockAlignedSnapshot>,
}
```

`feedback_time_us` is the calibrated common-clock time produced by
`RawClockEstimator::map_raw_us(raw_hw_us)`. The buffer should retain enough
history to cover alignment lag, control jitter, and a few missed frames.

Initial retention can be derived from thresholds:

```text
retention_us = max(
  alignment_lag_us + sample_gap_max_us * 2,
  last_sample_age_us
)
```

Prune samples older than the latest inserted `feedback_time_us - retention_us`.
The exact capacity can also be bounded with a conservative fixed cap to prevent
unbounded memory use.

### Target Time Selection

Each loop still reads and ingests the latest master and slave snapshots. After
mapping their raw times:

```text
latest_master_time = map(master_latest_raw)
latest_slave_time = map(slave_latest_raw)
target_time = min(latest_master_time, latest_slave_time) - alignment_lag_us
```

The subtraction must be checked, not saturating. If the latest common time is
earlier than `alignment_lag_us`, selection should be treated as an alignment
buffer miss. Saturating to zero could accidentally select an unrelated earliest
sample and hide startup alignment problems.

The selector then asks each per-arm buffer for the latest sample whose
`feedback_time_us <= target_time`.

This is intentionally "latest-before", not interpolation. It never invents an
unobserved robot state and never uses feedback from after the target time. The
selected samples can be understood and debugged as real frames that the robot
actually reported.

The runtime should compute two skew streams:

- `latest_inter_arm_skew_us`: skew between the newly read latest master/slave
  snapshots. This is diagnostic only.
- `selected_inter_arm_skew_us`: skew between the two selected snapshots. This
  is the skew passed to runtime gates, controller snapshots, and reports.

`selected_inter_arm_skew_us` should replace the current latest/latest
`inter_arm_skew_us` as the control-loop health gate input.

The implementation must not call the existing `tick_from_snapshots()` on
selected historical snapshots. That helper currently both ingests snapshots into
the monotonic raw timestamp estimators and records skew; re-ingesting older
buffered snapshots could create false raw timestamp regressions or corrupt
diagnostics.

Instead, split the flow into explicit helpers:

```rust
struct RawClockLatestTiming {
    master_feedback_time_us: u64,
    slave_feedback_time_us: u64,
    latest_inter_arm_skew_us: u64,
    master_health: RawClockHealth,
    slave_health: RawClockHealth,
}

fn ingest_latest_snapshots(...) -> Result<RawClockLatestTiming, RawClockRuntimeError>;

fn selected_tick_from_buffered_snapshots(
    latest: &RawClockLatestTiming,
    selected_master_time_us: u64,
    selected_slave_time_us: u64,
    now_host_us: u64,
) -> Result<RawClockTickTiming, RawClockRuntimeError>;
```

`ingest_latest_snapshots()` is the only step that updates estimators and raw
timestamp monotonicity state. `selected_tick_from_buffered_snapshots()` builds
the gate tick from selected feedback times, selected skew, selected sample age,
and the current latest estimator health.

### Runtime Loop Behavior

The active runtime loop should become:

1. Read latest master snapshot.
2. Read latest slave snapshot.
3. Ingest both latest snapshots into the estimators.
4. Map latest raw hardware timestamps into common time and record latest/latest
   skew as diagnostics.
5. Add both snapshots to their per-arm buffers.
6. Compute `target_time`.
7. Select latest-before snapshots from both buffers.
8. If selection succeeds:
   - reset consecutive alignment miss count
   - compute selected skew
   - compute selected sample age for each side
   - run runtime gates on a `RawClockTickTiming` built from selected times,
     selected skew, selected sample age, and current estimator health
   - build `DualArmSnapshot` from the selected pair
   - tick the controller and send commands
9. If selection misses:
   - increment total and consecutive miss counters
   - if consecutive misses are below threshold, skip this cycle without sending
     a new command
   - if consecutive misses reach threshold, fail the runtime and execute the
     existing bounded shutdown path

Skipping a cycle on a transient miss is preferable to sending a command based on
misaligned data. Existing transmit state should not be updated from a missed
selection.

Skipped alignment-miss cycles should not increment the existing control
`iterations` counter and should not count toward `max_iterations`. They are
reported only through the new alignment miss counters. On the next successful
selection, the controller should keep using the existing nominal control period
semantics; the first version should not accumulate skipped-cycle elapsed time
into a larger controller `dt`.

### Error Handling

Add an explicit runtime error for alignment misses, for example:

```rust
AlignmentBufferMiss {
    side: &'static str,
    target_time_us: u64,
    latest_available_time_us: Option<u64>,
}
```

The first two consecutive misses with the default threshold of 3 are recorded
but do not exit. The third consecutive miss exits the runtime with a transport
fault style reason and triggers the same bounded shutdown behavior as other
runtime timing faults.

Existing fail-fast behavior stays unchanged for:

- raw timestamp regressions
- residual p95
- drift
- sample gap
- last sample age
- unknown unhealthy kinds
- selected inter-arm skew above threshold
- selected sample age above runtime freshness threshold
- residual max after configured consecutive failure threshold

`RawClockTickTiming` should be extended, or paired with an adjacent structure,
so the runtime gate can distinguish latest estimator health from selected
sample freshness. Current estimator health should still validate raw-clock fit
quality, residuals, drift, sample gaps, and latest sample freshness. Selected
sample age should separately ensure the controller is not acting on an aligned
snapshot older than the runtime freshness threshold.

Alignment miss diagnostics should include a reason precise enough for planning
and reports:

```rust
enum AlignmentBufferMissKind {
    TargetUnderflow,
    Master,
    Slave,
    Both,
}
```

The public error can carry this kind directly or format it into the existing
runtime error string.

### Reporting

Extend `RawClockRuntimeReport` with alignment diagnostics:

```rust
pub alignment_lag_us: u64,
pub latest_inter_arm_skew_max_us: u64,
pub latest_inter_arm_skew_p95_us: u64,
pub selected_inter_arm_skew_max_us: u64,
pub selected_inter_arm_skew_p95_us: u64,
pub alignment_buffer_misses: u64,
pub alignment_buffer_miss_consecutive_max: u32,
pub alignment_buffer_miss_consecutive_failures: u32,
```

The existing `max_inter_arm_skew_us` and `inter_arm_skew_p95_us` fields should
continue to represent the skew that gates control. In the aligned runtime, those
fields should be populated from selected skew for compatibility. The new
`latest_*` fields expose the raw latest/latest tail for diagnosis.

CLI JSON timing should include the new fields. Human report output should add a
compact line when experimental raw-clock alignment is active:

```text
raw_clock alignment lag_us=5000 selected_skew max=... p95=... latest_skew max=... p95=... misses=... consecutive_max=...
```

### CLI and Smoke Script

Add CLI/config plumbing for:

```text
--raw-clock-alignment-lag-us
--raw-clock-alignment-buffer-miss-consecutive-failures
```

The smoke script should pass:

```text
RAW_CLOCK_ALIGNMENT_LAG_US=5000
RAW_CLOCK_ALIGNMENT_BUFFER_MISS_CONSECUTIVE_FAILURES=3
```

and include both values in `environment.txt`.

### Testing

Unit tests:

- Selector returns each side's latest sample at or before `target_time`.
- Selector returns buffer miss when no side sample covers `target_time`.
- Selector returns buffer miss when only one side covers `target_time`.
- Selector treats target-time underflow as a buffer miss.
- Selection never returns a future sample.
- Latest snapshots are ingested exactly once; selected buffered snapshots are
  not re-ingested into raw timestamp monotonicity state.
- Runtime records latest/latest skew separately from selected skew.
- Runtime gates on selected skew, not latest/latest skew.
- Runtime gates on selected sample age as well as latest estimator health.
- Runtime skips command submission while consecutive alignment misses are below
  threshold.
- Runtime exits and shutdowns when consecutive alignment misses reach the
  threshold.
- A successful aligned selection resets the consecutive miss counter.
- Config and CLI merge alignment defaults, file values, and CLI overrides.
- Config validation rejects alignment lag greater than or equal to either
  estimator or runtime last-sample-age freshness threshold.
- JSON report serializes alignment diagnostics.
- Human report prints alignment diagnostics.
- Smoke script dry-run includes alignment CLI args and env values.

Hardware acceptance:

```bash
MASTER_IFACE=can1 SLAVE_IFACE=can0 MASTER_DAMPING=0.2 MAX_ITERATIONS=30000 ./scripts/run_teleop_smoke.sh
```

Expected:

- `exit.clean=true`
- `exit.reason=max_iterations`
- `timing.clock_health_failures=0`
- selected skew p95/max are materially lower than latest/latest skew p95/max
- buffer miss counters may be nonzero, but consecutive max stays below 3
- master feedback delta and slave command delta stay equal
- slave feedback delta follows within expected mechanical lag

## Open Follow-Up

Interpolation can be added later as an optional mode if latest-before selection
proves too laggy. It is intentionally out of scope for the first implementation
because it would synthesize states that were not directly observed and would
make safety/debugging harder during bring-up.
