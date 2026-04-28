# Piper CLI Dual-Arm Teleoperation

## Supported Topology

v1 supports two independent StrictRealtime SocketCAN links, normally `can0` and
`can1`. GS-USB target syntax is accepted by config/help for future compatibility,
but runtime execution is rejected until SDK SoftRealtime dual-arm support exists.

## First Run

If you do not pass `--calibration-file`, place both arms in the intended
mirrored zero pose before running the command. Startup captures that posture
automatically before the enable confirmation.

```bash
piper-cli teleop dual-arm \
  --master-interface can0 \
  --slave-interface can1 \
  --mode master-follower
```

## Calibration

With `--calibration-file`, the CLI loads the baseline and checks the current
posture before enabling. Without it, startup connects to both arms, checks
runtime health, then captures the current posture as the baseline before asking
for operator confirmation. `--save-calibration <path>` writes that captured
baseline before confirmation if no calibration file is supplied.

## Runtime Console

- `status`
- `mode master-follower`
- `mode bilateral`
- `gain track-kp <value>`
- `gain track-kd <value>`
- `gain master-damping <value>`
- `gain reflection-gain <value>`
- `quit`

## Runtime Control Defaults

The CLI resolves dual-arm teleop to the SDK's unconfirmed submission mode by
default. This preserves the low-jitter behavior used by earlier teleop builds.

The SDK runtime now stores the master interaction torque slew limit in Nm/s. The
CLI keeps the previous profile behavior by migrating the former per-tick value
to Nm/s at the resolved loop rate; the default remains equivalent to
`0.25 Nm/tick` at the selected control frequency.

## Report Interpretation

The human report and JSON report use master/slave naming even though the SDK
internally stores the two arms as left/right. Durations are integer
microseconds. In JSON, `exit.reason = cancelled` is a clean operator stop, and
the human report prints the same value as `reason=cancelled`.
`metrics.read_faults` or `metrics.submission_faults` mean the run is unsafe to
continue without inspecting the CAN link and arm status.

## Exit Codes

Exit code `0` means the loop ended cleanly with `exit.reason = cancelled` or
`exit.reason = max_iterations` and post-disable reporting succeeded. Nonzero
means startup validation failed, runtime faulted, or the JSON report could not
be written after the arms were already disabled or faulted.

## Fault Response

Unsupported runtime targets are rejected before hardware connect. Posture
mismatch and Ctrl+C before MIT enable stop startup without entering active
control. After enable, clean cancellation and `max_iterations` exit through
normal disable. Read, controller, and compensation faults also attempt to return
both arms to standby when possible. Submission faults and runtime transport
faults use the SDK fault-shutdown path and record per-arm stop-attempt results.

## Stop Attempt Results

The human report prints separate master/slave lines with `stop_attempt=...`.
JSON reports store the same values at `metrics.master_stop_attempt` and
`metrics.slave_stop_attempt`.

Possible values are `not_attempted`, `confirmed_sent`, `timeout`,
`channel_closed`, `queue_rejected`, and `transport_failed`. `not_attempted` is
expected for clean normal-disable exits such as `cancelled` and
`max_iterations`, and it can also appear when a non-clean path returned to
standby without using fault shutdown. For submission or runtime transport
faults, inspect these per-arm fields before the next run; `confirmed_sent` means
the fault-shutdown stop command was accepted, while timeout, closed-channel,
queue-rejected, or transport-failed values require hardware and CAN-link
inspection.

## JSON Report Schema

`schema_version = 1` is intentionally incompatible with future schemas unless a
new version is declared. Consumers must check `schema_version`, `exit.reason`,
`metrics.read_faults`, `metrics.submission_faults`,
`metrics.master_stop_attempt`, and `metrics.slave_stop_attempt` before treating
data as a successful run.

## Experimental Calibrated Raw-Clock Mode

This mode is for lab validation with Linux SocketCAN `gs_usb` interfaces that
expose `hardware-raw-clock` but cannot satisfy the production StrictRealtime
path. It is not StrictRealtime and must be enabled explicitly. The experimental
path is master-follower only; it does not support bilateral force reflection or
runtime console switching with `mode bilateral`.

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

The JSON report must show `timing.timing_source=calibrated_hw_raw`,
`timing.experimental=true`, and `timing.strict_realtime=false`. The human
report prints the same timing values with bare field names.

### Experimental Acceptance Checklist

1. Probe completes the requested 300-second run and JSON shows `pass=true`.
2. Probe JSON shows `left.health.healthy=true` and
   `right.health.healthy=true`.
3. Probe JSON shows `left.health.raw_timestamp_regressions=0` and
   `right.health.raw_timestamp_regressions=0`.
4. Probe JSON residuals are within the configured thresholds, including
   `left.health.residual_p95_us`, `right.health.residual_p95_us`,
   `left.health.residual_max_us`, and `right.health.residual_max_us`.
5. Probe JSON shows `max_estimated_inter_arm_skew_us` within the teleop
   raw-clock threshold for the trial, `--raw-clock-inter-arm-skew-max-us`, or
   its default if the flag is omitted.
6. The bounded normal `master-follower` trial exits cleanly with
   `exit.clean=true`, `exit.reason=max_iterations`, and `exit.faulted=false`.
7. Teleop JSON report marks the run experimental and non-strict with
   `timing.timing_source=calibrated_hw_raw`, `timing.experimental=true`, and
   `timing.strict_realtime=false`.
8. The bounded normal teleop report shows no clock-health, runtime, read, or
   submission faults: `timing.clock_health_failures=0`, `exit.last_error=null`,
   `metrics.read_faults=0`, and `metrics.submission_faults=0`.
9. A separate disconnect trial with one feedback path removed causes bounded
   shutdown instead of hanging.

## Manual Acceptance Checklist

1. Confirm both links are independent StrictRealtime SocketCAN links.
2. Run master-follower at default gains.
3. Verify every mirrored joint direction.
4. Run for several minutes with zero read/submission faults.
5. Switch to bilateral with low reflection gain.
6. Verify soft-contact reflection direction.
7. Ctrl+C exits cleanly.
8. Disconnect one feedback path and confirm bounded shutdown.
