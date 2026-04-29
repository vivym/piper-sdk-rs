# Raw Clock Residual Max Debounce Design

## Summary

Add a narrow runtime health policy improvement for experimental calibrated
raw-clock teleoperation: keep `residual_max_us` as a hard safety gate, but
require consecutive `residual_max` health failures before stopping the control
loop.

The current implementation marks the estimator unhealthy when any sample in the
health window exceeds `residual_max_us`. Long hardware smoke runs have shown
isolated max residual spikes just above the configured threshold while
`residual_p95_us`, inter-arm skew, sample gaps, drift, and command/feedback
tracking remain acceptable. A single spike should be observable and counted, but
it should not abort the run unless the condition persists.

This change applies only to the experimental calibrated raw-clock path. It must
not change production `StrictRealtime` behavior or relax fail-fast handling for
skew, freshness, drift, sample gaps, raw timestamp regressions, or p95 residual
failures.

## Goals

- Preserve `residual_max_us` as a bounded hard gate.
- Avoid aborting long teleop smoke runs on one isolated max residual spike.
- Abort when `residual_max_us` is exceeded for a configurable number of
  consecutive control ticks.
- Keep `residual_p95_us`, inter-arm skew, drift, sample gap, freshness, and raw
  timestamp regression failures fail-fast.
- Expose residual-max spike counters in runtime reports and CLI JSON output.
- Make the debounce threshold configurable from CLI and config file.
- Keep defaults conservative for lab bring-up.

## Non-Goals

- Do not remove or ignore the `residual_max_us` threshold.
- Do not broaden debounce to p95, skew, drift, sample gap, freshness, or raw
  timestamp regression failures.
- Do not redesign the raw-clock estimator or fit algorithm in this change.
- Do not add a trace file format in this change.
- Do not change non-raw-clock teleop behavior.

## Existing Context

The calibrated raw-clock estimator lives in
`crates/piper-tools/src/raw_clock.rs`. `RawClockEstimator::health()` computes
residual p50/p95/p99/max and returns `RawClockHealth` with:

- `healthy: bool`
- residual metrics
- drift, sample gap, freshness, and regression metrics
- `reason: Option<String>`

The unhealthy decision is currently string-only and made in
`RawClockEstimator::unhealthy_reason()`. A `residual_max_us` breach sets
`healthy=false` in the same way as p95, drift, sample gap, or regression
failures.

The experimental dual-arm raw-clock loop lives in
`crates/piper-client/src/dual_arm_raw_clock.rs`. `RawClockRuntimeGate` is
currently stateless. It receives a `RawClockTickTiming`, checks both health
objects, and immediately returns `ClockUnhealthy` when either side is unhealthy.
The runtime loop records one clock health failure and faults the arms on that
first error.

The CLI raw-clock settings live in:

- `apps/cli/src/commands/teleop.rs`
- `apps/cli/src/teleop/config.rs`
- `apps/cli/src/teleop/workflow.rs`

The smoke script is `scripts/run_teleop_smoke.sh`.

## Design

### Structured Health Failure Kind

Add a typed failure classifier in `piper-tools`:

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

Extend `RawClockHealth` with:

```rust
pub failure_kind: Option<RawClockUnhealthyKind>,
```

Keep `reason: Option<String>` as the human-readable message used in existing
errors and reports. The estimator should produce both fields from one internal
decision function so the typed kind and string reason cannot drift apart.

Failure classification must prefer non-debounceable failures over
`ResidualMax`. `ResidualMax` is debounceable only when it is the sole failing
steady-state health threshold. If multiple thresholds fail in the same health
sample, classify the failure using this priority:

1. `FitUnavailable`
2. `WarmupSamples`
3. `WarmupWindow`
4. `ResidualP95`
5. `Drift`
6. `SampleGap`
7. `LastSampleAge`
8. `RawTimestampRegression`
9. `ResidualMax`

In particular, if both `ResidualP95` and `ResidualMax` fail, the kind must be
`ResidualP95` so the runtime fails immediately.

This is intentionally not a string parser in `piper-client`. Runtime policy
should depend on a stable enum, not on the exact wording of error messages.

### Stateful Runtime Debounce

Add raw-clock runtime debounce state in `RawClockRuntimeTiming`, because the
current `RawClockRuntimeGate` is recreated for each check and cannot remember
consecutive failures.

New counters:

```rust
master_residual_max_spikes: u64,
slave_residual_max_spikes: u64,
master_residual_max_consecutive_failures: u32,
slave_residual_max_consecutive_failures: u32,
```

`*_residual_max_spikes` counts every control tick whose health failure kind is
`ResidualMax`, not just transitions from healthy to failing. The consecutive
counters track the current run of adjacent `ResidualMax` failures and reset when
that side becomes healthy.

Add a runtime threshold:

```rust
pub residual_max_consecutive_failures: u32,
```

to `RawClockRuntimeThresholds`.

Replace runtime-loop calls to the stateless gate with a timing-owned check such
as:

```rust
timing.check_tick_with_debounce(tick, config.thresholds)
```

The behavior is:

- If inter-arm skew exceeds `inter_arm_skew_max_us`, fail immediately.
- If either health is healthy, reset that side's residual-max consecutive
  counter.
- If a side is unhealthy with `ResidualMax`, increment that side's total spike
  counter and consecutive counter.
- If the consecutive counter reaches `residual_max_consecutive_failures`, return
  `ClockUnhealthy`.
- If the consecutive counter is still below the threshold, continue the loop.
- If a side is unhealthy for any other kind, return `ClockUnhealthy`
  immediately after resetting that side's residual-max consecutive counter.
- If a health object is unhealthy but has no `failure_kind`, fail immediately;
  that is a conservative compatibility fallback and also resets that side's
  residual-max consecutive counter.
- If one side has a debounceable `ResidualMax` failure while the other side has
  a fail-fast health failure on the same tick, still count the debounceable
  side's residual-max spike before returning the fail-fast error. This keeps
  mixed-failure report counters deterministic without changing shutdown
  behavior.
- If both sides have `ResidualMax` failures on the same tick, evaluate both
  sides and update both spike/consecutive counters before returning any
  threshold-reached error.

Warmup behavior should stay tolerant. Existing warmup code already ignores
temporary `ClockUnhealthy` until the final health check. The final pre-run
health check must be explicit:

- During a multi-tick warmup or post-confirmation refresh, `ResidualMax` may use
  the same consecutive-failure debounce state as runtime.
- If a final pre-run check is made from a single snapshot with no debounce
  history, any unhealthy result, including `ResidualMax`, must fail fast.
- A single final pre-run snapshot must bypass residual-max debounce regardless
  of prior warmup or refresh counters.
- A single final snapshot must never pass p95, skew, drift, sample-gap,
  freshness, raw regression, or unknown health failures.
- After the final pre-run check passes and before entering the active teleop
  control loop, clear residual-max debounce state and spike counters so runtime
  reporting and first runtime failure decisions start from the active loop, not
  from warmup or refresh history.

The implementation should expose that startup transition as an explicit helper,
for example:

```rust
fn reset_runtime_residual_max_counters(&mut self)
```

so it is easy to test and hard to skip.

### CLI and Config

Add an operator setting:

```text
--raw-clock-residual-max-consecutive-failures <N>
```

Config file field:

```toml
[raw_clock]
residual_max_consecutive_failures = 3
```

Resolved config field:

```rust
pub residual_max_consecutive_failures: u32
```

Default:

```rust
DEFAULT_RAW_CLOCK_RESIDUAL_MAX_CONSECUTIVE_FAILURES = 3
```

Validation:

- Minimum: `1`
- Maximum: `100`

`1` preserves current effective behavior for users who want fail-fast max
residual handling.

### Reports

Extend `RawClockRuntimeReport` with:

```rust
pub master_residual_max_spikes: u64,
pub slave_residual_max_spikes: u64,
pub master_residual_max_consecutive_failures: u32,
pub slave_residual_max_consecutive_failures: u32,
```

Include these values in CLI report mapping and JSON summary. Console output may
stay compact, but if nonzero spikes occurred it should print a short raw-clock
line so the operator sees that the run tolerated isolated max-residual events.

Example JSON shape:

```json
{
  "timing": {
    "clock_health_failures": 0,
    "master_residual_max_spikes": 0,
    "slave_residual_max_spikes": 1,
    "master_residual_max_consecutive_failures": 0,
    "slave_residual_max_consecutive_failures": 0
  }
}
```

### Smoke Script

Update `scripts/run_teleop_smoke.sh` to pass:

```bash
RAW_CLOCK_RESIDUAL_MAX_CONSECUTIVE_FAILURES="${RAW_CLOCK_RESIDUAL_MAX_CONSECUTIVE_FAILURES:-3}"
--raw-clock-residual-max-consecutive-failures "${RAW_CLOCK_RESIDUAL_MAX_CONSECUTIVE_FAILURES}"
```

The current lab defaults may keep `RAW_CLOCK_RESIDUAL_P95_US=1500` and
`RAW_CLOCK_RESIDUAL_MAX_US=3000` while the debounce is validated. After several
stable long runs, operators may reduce `RAW_CLOCK_RESIDUAL_MAX_US` back toward
`2000` or `2500` because isolated spikes no longer abort immediately.

## Error Handling

When the debounce threshold is exceeded, the final error should remain a
`ClockUnhealthy` error containing the original `RawClockHealth`. The reason text
should still identify the threshold breach, for example:

```text
residual max 3004us exceeds threshold 3000us
```

The report counters distinguish this from other health faults.

If the typed failure kind is missing on an unhealthy health object, fail
immediately. That avoids accidentally treating unknown future failures as
debounceable.

## Testing

Add unit tests in `crates/piper-tools/src/raw_clock.rs`:

- `RawClockHealth` reports `failure_kind=ResidualMax` when only max residual
  exceeds threshold.
- `RawClockHealth` reports `failure_kind=ResidualP95` when p95 exceeds
  threshold.
- If both p95 and max residual thresholds fail, `failure_kind=ResidualP95`.
- Existing string reasons remain present.

Add unit tests in `crates/piper-client/src/dual_arm_raw_clock.rs`:

- A single `ResidualMax` failure increments the appropriate spike and
  consecutive counters but does not return an error when the configured limit is
  greater than one.
- Consecutive `ResidualMax` failures return `ClockUnhealthy` once the configured
  limit is reached.
- A healthy tick resets that side's consecutive counter.
- A non-`ResidualMax` unhealthy tick resets that side's consecutive counter and
  fails immediately.
- `ResidualP95` still fails immediately.
- Inter-arm skew still fails immediately.
- A single final pre-run snapshot bypasses residual-max debounce and fails
  immediately on `ResidualMax`.
- Runtime residual-max counters are reset after the final pre-run check passes,
  so warmup or refresh spikes cannot make the first runtime spike fault.
- Mixed same-tick failure behavior is deterministic: if one side has
  `ResidualMax` and the other side has a fail-fast health failure, the
  residual-max spike is counted before shutdown.
- If both sides report `ResidualMax` on the same tick and one side reaches the
  consecutive-failure threshold, both sides' spike counters are updated before
  returning the error.

Add CLI config tests in `apps/cli/src/teleop/config.rs`:

- CLI value overrides file/default.
- File value is accepted.
- `0` is rejected.
- Excessively large values are rejected.

Add workflow/report tests where practical:

- Raw-clock report JSON includes residual-max spike counters.
- Existing successful reports default counters to zero.

Run at least:

```bash
cargo fmt --all -- --check
cargo test -p piper-tools
cargo test -p piper-client -- --test-threads=1
cargo test -p piper-cli
bash -n scripts/run_teleop_smoke.sh
DRY_RUN=1 ./scripts/run_teleop_smoke.sh
```

Hardware validation remains manual:

```bash
MAX_ITERATIONS=30000 ./scripts/run_teleop_smoke.sh
```

Acceptance criteria for the first hardware run:

- No abort for isolated residual-max spikes below the consecutive threshold.
- Any p95, skew, drift, sample-gap, freshness, or regression failure still
  aborts.
- Report shows spike counters if spikes occurred.
- Joint motion diagnostics remain present.
