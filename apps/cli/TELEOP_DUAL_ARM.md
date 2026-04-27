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
control. After enable, read faults, command submission faults, controller
faults, and Ctrl+C trigger bounded shutdown behavior; the CLI attempts to stop
both arms and records the per-arm stop attempt result.

## Stop Attempt Results

The human report prints separate master/slave lines with `stop_attempt=...`.
JSON reports store the same values at `metrics.master_stop_attempt` and
`metrics.slave_stop_attempt`.

Possible values are `not_attempted`, `confirmed_sent`, `timeout`,
`channel_closed`, `queue_rejected`, and `transport_failed`. Treat anything other
than `confirmed_sent` as requiring operator inspection before the next run.

## JSON Report Schema

`schema_version = 1` is intentionally incompatible with future schemas unless a
new version is declared. Consumers must check `schema_version`, `exit.reason`,
`metrics.read_faults`, `metrics.submission_faults`,
`metrics.master_stop_attempt`, and `metrics.slave_stop_attempt` before treating
data as a successful run.

## Manual Acceptance Checklist

1. Confirm both links are independent StrictRealtime SocketCAN links.
2. Run master-follower at default gains.
3. Verify every mirrored joint direction.
4. Run for several minutes with zero read/submission faults.
5. Switch to bilateral with low reflection gain.
6. Verify soft-contact reflection direction.
7. Ctrl+C exits cleanly.
8. Disconnect one feedback path and confirm bounded shutdown.
