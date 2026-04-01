# Piper HIL Operator Runbook Design

## Goal

Add a new operator-facing runbook at `docs/v0/piper_hil_operator_runbook.md` for the existing Linux + SocketCAN + one real Piper arm HIL flow.

The runbook is for mixed development/test readers. It should be executable enough for a test operator to follow step by step, while still giving enough context for an engineer to understand what each phase is proving and how to react when a step fails.

## Why A New Document

The repo already has three distinct HIL documents:

- `docs/v0/piper_hil_handbook.md` is the normative acceptance handbook
- `docs/v0/piper_hil_execution_checklist.md` is the terse operator checklist
- `docs/v0/piper_hil_results_template.md` is the recording template

What is still missing is a practical runbook that bridges those three artifacts:

- tells an operator which terminal to use for what
- gives the exact command sequence in execution order
- explains what output or behavior to look for
- tells the reader when to stop and which phase should be marked failed
- points the reader to the exact results-template fields that should be filled

This runbook is an execution companion. It does not replace the handbook, checklist, or results template.

## Audience

The target reader is a mixed development/test operator:

- comfortable with terminal execution
- not expected to infer acceptance intent from sparse notes
- likely to need brief explanations for why a phase exists and what a failure means

The document should therefore prefer:

- Chinese prose
- English commands and output keywords preserved exactly
- clear phase boundaries
- immediate failure handling guidance

## Scope

The new runbook covers the same tested scope as the accepted handbook:

- one real Piper arm
- Linux host
- SocketCAN
- manual HIL acceptance
- low-risk motion only

It should cover:

- setup and terminal layout
- safety baseline and stop conditions before any command execution
- Phase 0 through Phase 4 execution
- explicit references to helper examples and CLI support paths
- pass/fail interpretation in operator language
- how to capture evidence into the checklist and results template

It should not expand scope to:

- GS-USB
- cross-platform validation
- bridge-host acceptance
- automation strategy
- high-risk motion or MIT-mode coverage

## Document Style

The runbook should be written in Chinese, with:

- commands in code blocks
- tool names and output keywords kept in English
- short explanatory paragraphs before each phase
- explicit terminal labels such as `Terminal 1`, `Terminal 2`, and `Terminal 3`

The writing style should prioritize field usability over completeness-by-default. Each phase should be easy to execute in order without having to cross-read multiple other docs.

## Safety Baseline Requirements

The runbook must restate the accepted safety baseline in operator language before Phase 0 begins.

At minimum it must explicitly require:

- one operator runs the test
- a second person supervises whenever motion is enabled
- the arm is unloaded
- the workspace is clear and collision-free
- the emergency stop is reachable before any motion step
- motion stays within the existing low-risk envelope
- the run stops immediately if the observed behavior exits that envelope

This is required so the runbook does not collapse into a command script that loses the handbook's safety preconditions.

## Proposed Structure

The runbook should contain these sections:

1. `Purpose and Document Relationship`
2. `Who Should Use This Document`
3. `Terminal Layout and Materials`
4. `Global Acceptance Thresholds`
5. `Phase 0: Preflight and Safety Baseline`
6. `Phase 1: Connection and Read-Only Observation`
7. `Phase 2: Safe Lifecycle and State Transitions`
8. `Phase 3: Low-Risk Motion Validation`
9. `Phase 4: Fault and Recovery Validation`
10. `How To Record Results`
11. `Common Stop Conditions`
12. `Minimal End-to-End Command Sequence`

## Per-Phase Content Rules

Each phase section should use a consistent internal structure:

- `目的`
- `使用终端`
- `执行步骤`
- `预期输出 / 观察点`
- `何时判失败并停止`
- `需要填写到结果模板的字段`

This is intentionally more operational than the handbook.

## Terminal Layout

The runbook should recommend a stable terminal layout:

- `Terminal 1`: primary execution
- `Terminal 2`: read-only corroboration using `robot_monitor` or `state_api_demo`
- `Terminal 3`: manual CLI lifecycle/recovery operations and note taking

The terminal layout guidance should also explain that not every phase needs all three terminals active at once.

## Required Command Coverage

The runbook must include the exact repo-local commands already accepted elsewhere:

- `cargo run -p piper-sdk --example robot_monitor -- --interface can0`
- `cargo run -p piper-sdk --example state_api_demo -- --interface can0`
- `cargo run -p piper-sdk --example client_monitor_hil_check -- --interface can0 --baud-rate 1000000 --observation-window-secs 900`
- `cargo run -p piper-sdk --example hil_joint_position_check -- --interface can0 --baud-rate 1000000 --joint 1 --delta-rad 0.02 --speed-percent 10`
- `cargo run -p piper-cli -- shell`
- `cargo run -p piper-cli -- stop --target socketcan:can0`

The runbook should also preserve the already-accepted REPL commands and failure strings:

- `connect socketcan:can0`
- `enable`
- `disable`
- `move --joints 0.02 --force`
- `robot is not in confirmed Standby; run stop first`
- `未连接`
- `电机未使能，请先使用 enable 命令`

For Phase 2 and Phase 4, the runbook must also call out the accepted evidence lines and decisions that the operator should look for, instead of paraphrasing them loosely.

At minimum this includes:

- `[PASS] connected and confirmed Standby`
- `[PASS] enabled PositionMode motion=Joint speed_percent=...`
- `[PASS] settle step=move ...`
- `[PASS] settle step=return ...`
- the rejected-state failure `robot is not in confirmed Standby; run stop first`
- the Phase 4 motion-gating decision:
  - pass if the shell rejects the probe with `未连接`, `电机未使能，请先使用 enable 命令`, or another failure that does not move the robot
  - fail if the command is accepted as normal motion before the safe baseline has been re-established

## Acceptance Thresholds

The runbook must restate and consistently use the handbook thresholds:

- connection and reconnect `<= 5s`
- initial complete monitor snapshot `<= 200ms`
- observation window `15 min`
- `PositionMode + MotionType::Joint only`
- `speed_percent <= 10`
- `abs(delta) <= 0.035 rad`
- return-to-start tolerance `<= 0.05 rad`

It should not introduce new thresholds.

## Relationship To Existing Docs

The runbook should explicitly tell the reader:

- use the handbook for the normative pass/fail policy
- use the checklist for live execution tracking
- use the results template for evidence capture

It should reference these exact files:

- `docs/v0/piper_hil_handbook.md`
- `docs/v0/piper_hil_execution_checklist.md`
- `docs/v0/piper_hil_results_template.md`

The runbook should also map each phase to the relevant checklist items, so the operator knows what to tick while executing, not only what to record afterward.

## Failure Handling

The runbook should provide practical failure handling without becoming a troubleshooting manual.

It should include:

- stop the run if Phase 0 cannot confirm live feedback
- stop the run if Phase 1 misses timing budgets or disconnects unexpectedly
- stop motion immediately if Phase 3 shows wrong direction, jump, oscillation, overshoot, or abnormal transient behavior
- do not declare recovery complete in Phase 4 until readable-state recovery and motion-gating checks are both observed

It should not attempt root-cause diagnosis beyond short operator-facing guidance.

## Checklist And Results Recording Guidance

The runbook should include a short mapping from each phase to:

- the relevant checks in `docs/v0/piper_hil_execution_checklist.md`
- the key fields in `docs/v0/piper_hil_results_template.md`

so the operator knows both what to tick during execution and where to record evidence afterward:

- timings
- observed helper outputs
- CLI evidence
- fault type and recovery evidence
- final gate decisions

This mapping is required because the checklist is terse during live execution and the results template is intentionally terse for evidence capture.

## Deliverable

Implementation will add one new file:

- `docs/v0/piper_hil_operator_runbook.md`

No existing handbook/checklist/template content needs to be modified unless a conflict is discovered during writing.

## Review Criteria

The runbook is acceptable if:

- it is clearly distinct from the handbook/checklist/template
- a mixed development/test reader can execute the phases in order without inventing missing steps
- all commands and thresholds stay aligned with accepted HIL docs
- it stays within the current Linux + SocketCAN + one-arm scope
