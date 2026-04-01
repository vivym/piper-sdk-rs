# Piper HIL Results Template

## Run Metadata

Run ID:
Date:
Operator:
Supervisor:
Git SHA:
Robot model:
Firmware version:
Host OS:
Kernel:
`rustc`:
`cargo`:
CAN interface:
Bitrate:

## Phase 0: Preflight and Safety Baseline

Result:

Observed:

Notes:

Artifacts:

## Per-Test Record

Test ID:
Phase:
Expected result:
Actual result:
Pass/Fail:
Notes:
Artifacts:

## Phase 1: Connection and Read-Only Observation

Result:

Connection budget:

First snapshot budget:

Observation window:

Observed:

Notes:

Artifacts:

## Phase 2: Safe Lifecycle and State Transitions

Result:

Standby evidence:

Enable evidence:

Disable evidence:

Drop or emergency-stop evidence:

Rejected-state gating evidence:

Reconnect evidence:

Observed:

Notes:

Artifacts:

## Phase 3: Low-Risk Motion Validation

Result:

Joint:

Delta rad:

Speed percent:

Move evidence:

Return evidence:

Return-to-start error:

Jump / oscillation / overshoot notes:

Repeated small moves consistency:

Observed:

Notes:

Artifacts:

## Phase 4: Fault and Recovery Validation

Result:

Fault type:

Fault evidence:

Timeout or dropped-feedback evidence:

Recovery evidence:

Readable-state recovery evidence:

Shell probe connection:

Motion-gating probe:

Observed:

Notes:

Artifacts:

## Phase Summary

Phase 0:

Phase 1:

Phase 2:

Phase 3:

Phase 4:

Gate 1:

Gate 2:

Gate 3:

Go/No-Go decision:

Final verdict:

## Sign-off

Operator:

Supervisor:

Date:
