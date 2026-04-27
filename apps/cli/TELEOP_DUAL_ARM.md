# Piper CLI Dual-Arm Teleoperation

## Supported Topology

v1 supports two independent StrictRealtime SocketCAN links, normally `can0` and
`can1`. GS-USB target syntax is accepted by config/help for future compatibility,
but runtime execution is rejected until SDK SoftRealtime dual-arm support exists.

## First Run

```bash
piper-cli teleop dual-arm \
  --master-interface can0 \
  --slave-interface can1 \
  --mode master-follower
```

## Calibration

Move both arms to the isomorphic zero pose before capture. Loaded calibration
files are checked against the current posture before enabling.

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
microseconds. `exit_reason = cancelled` is a clean operator stop. `read_faults`
or `submission_faults` mean the run is unsafe to continue without inspecting the
CAN link and arm status.

## Exit Codes

Exit code `0` means the operator intentionally stopped the loop and post-disable
reporting succeeded. Nonzero means startup validation failed, runtime faulted,
or the JSON report could not be written after the arms were already disabled or
faulted.

## Fault Response

Read faults, command submission faults, unsupported runtime targets, posture
mismatch, and Ctrl+C during startup all trigger bounded shutdown behavior. If a
fault occurs after enable, the CLI attempts to stop both arms and records the
stop attempt result.

## Stop Attempt Results

`stop_attempt = disabled` means both arms were commanded back to standby.
`stop_attempt = fault_shutdown` means normal disable did not complete and the
backend used its fault shutdown path. `stop_attempt = unavailable` only applies
before MIT enable succeeds.

## JSON Report Schema

`schema_version = 1` is intentionally incompatible with future schemas unless a
new version is declared. Consumers must check `schema_version`, `exit_reason`,
`read_faults`, `submission_faults`, and `stop_attempt` before treating data as a
successful run.

## Manual Acceptance Checklist

1. Confirm both links are independent StrictRealtime SocketCAN links.
2. Run master-follower at default gains.
3. Verify every mirrored joint direction.
4. Run for several minutes with zero read/submission faults.
5. Switch to bilateral with low reflection gain.
6. Verify soft-contact reflection direction.
7. Ctrl+C exits cleanly.
8. Disconnect one feedback path and confirm bounded shutdown.
