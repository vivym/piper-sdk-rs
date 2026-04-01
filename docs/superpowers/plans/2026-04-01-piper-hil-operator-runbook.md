# Piper HIL Operator Runbook Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a detailed operator-facing runbook at `docs/v0/piper_hil_operator_runbook.md` that translates the accepted HIL handbook into a step-by-step execution guide for mixed development/test readers.

**Architecture:** The implementation is doc-only. The new runbook will sit between the existing handbook, checklist, and results template: more operational than the handbook, more explanatory than the checklist, and explicit about where each observation should be recorded in the results template. The runbook must not introduce new scope, thresholds, or tool entry points; it only reorganizes the accepted HIL flow into an executable narrative with terminal layout, expected evidence, stop conditions, and recording guidance.

**Tech Stack:** Markdown documentation, existing HIL docs in `docs/v0`, `rg`, `sed`, `git`

---

## File Map

- Create: `docs/v0/piper_hil_operator_runbook.md`
  Purpose: operator-facing step-by-step HIL execution guide for Linux + SocketCAN + one real Piper arm.
- Read only: `docs/v0/piper_hil_handbook.md`
  Purpose: normative source for thresholds, pass/fail policy, command set, and accepted evidence.
- Read only: `docs/v0/piper_hil_execution_checklist.md`
  Purpose: live execution checklist that the runbook must map to while explaining each phase.
- Read only: `docs/v0/piper_hil_results_template.md`
  Purpose: evidence recording template that the runbook must reference phase by phase.

## Task 1: Write The Operator Runbook

**Files:**
- Create: `docs/v0/piper_hil_operator_runbook.md`
- Read: `docs/v0/piper_hil_handbook.md`
- Read: `docs/v0/piper_hil_execution_checklist.md`
- Read: `docs/v0/piper_hil_results_template.md`

- [ ] **Step 1: Verify the runbook file does not already exist**

Run: `test -f docs/v0/piper_hil_operator_runbook.md`
Expected: non-zero exit status because the runbook is new.

- [ ] **Step 2: Write the runbook skeleton with all required top-level sections**

```markdown
# Piper HIL 操作员执行手册

## 文档定位
## 适用对象
## 依赖文档
## 测试前安全基线
## 终端布局建议
## 全局判据与阈值
## Phase 0：上电前检查与总线确认
## Phase 1：只读连接与观测
## Phase 2：生命周期与状态切换
## Phase 3：低风险运动验证
## Phase 4：故障与恢复验证
## 结果记录方法
## 立即停测条件
## 最小完整执行序列
```

- [ ] **Step 3: Fill in the document relationship and scope guardrails**

Required content:
- state clearly that this runbook does not replace:
  - `docs/v0/piper_hil_handbook.md`
  - `docs/v0/piper_hil_execution_checklist.md`
  - `docs/v0/piper_hil_results_template.md`
- state that the scope is still:
  - Linux
  - SocketCAN
  - one real Piper arm
  - low-risk motion only
- state that the runbook does not add:
  - GS-USB
  - cross-platform coverage
  - MIT mode
  - high-risk motion

- [ ] **Step 4: Write the safety baseline and terminal-layout sections**

Required content:
- one operator runs the test
- a second person supervises whenever motion is enabled
- arm unloaded
- workspace clear
- E-stop reachable
- stop immediately if motion exits the low-risk envelope
- terminal layout:
  - `Terminal 1`: primary execution
  - `Terminal 2`: read-only corroboration
  - `Terminal 3`: CLI lifecycle / recovery / notes
- explain briefly that not all three terminals are needed in every phase

- [ ] **Step 5: Write the global thresholds section with exact accepted values**

Required thresholds:
- `<= 5s` connection and reconnect
- `<= 200ms` first complete monitor snapshot
- `15 min` read-only observation window
- `PositionMode + MotionType::Joint only`
- `speed_percent <= 10`
- `abs(delta) <= 0.035 rad`
- `<= 0.05 rad` return-to-start tolerance

Expected result:
- the runbook repeats these values exactly and does not introduce new thresholds

- [ ] **Step 6: Write the Phase 0 section**

Required content:
- purpose in Chinese
- terminals used
- exact commands:
  - `git rev-parse HEAD`
  - `uname -a`
  - `rustc --version`
  - `cargo --version`
  - `ip -details link show can0`
  - `ip -statistics link show can0`
  - optional `sudo ip link set can0 up type can bitrate 1000000`
  - `cargo run -p piper-sdk --example robot_monitor -- --interface can0`
- tell the operator to treat the first live joint or gripper update as feedback-present evidence
- tell the operator to stop the run here if live feedback cannot be confirmed
- map this phase to:
  - checklist Phase 0 items
  - results template `Phase 0` block

- [ ] **Step 7: Write the Phase 1 section**

Required content:
- exact primary command:
  - `cargo run -p piper-sdk --example client_monitor_hil_check -- --interface can0 --baud-rate 1000000 --observation-window-secs 900`
- optional corroboration commands:
  - `cargo run -p piper-sdk --example robot_monitor -- --interface can0`
  - `cargo run -p piper-sdk --example state_api_demo -- --interface can0`
- explain what to look for:
  - connection within `<= 5s`
  - first complete snapshot within `<= 200ms`
  - stable `15 min` observation window
- explain what counts as an observable failure:
  - helper timeout
  - missed first snapshot budget
  - unexplained disconnect
- map this phase to:
  - checklist Phase 1 items
  - results template `Connection budget`, `First snapshot budget`, `Observation window`

- [ ] **Step 8: Write the Phase 2 section**

Required content:
- exact helper command:
  - `cargo run -p piper-sdk --example hil_joint_position_check -- --interface can0 --baud-rate 1000000 --joint 1 --delta-rad 0.02 --speed-percent 10`
- exact accepted evidence lines:
  - `[PASS] connected and confirmed Standby`
  - `[PASS] enabled PositionMode motion=Joint speed_percent=...`
  - `[PASS] settle step=move ...`
  - `[PASS] settle step=return ...`
- explicit disable path:
  - `cargo run -p piper-cli -- shell`
  - `connect socketcan:can0`
  - `enable`
  - `disable`
  - `exit`
- external stop path:
  - `cargo run -p piper-cli -- stop --target socketcan:can0`
- rejected-state gating path and failure text:
  - run `hil_joint_position_check` while not in Standby
  - expect `robot is not in confirmed Standby; run stop first`
- reconnect re-check against `<= 5s` and `<= 200ms`
- map this phase to:
  - checklist Phase 2 items
  - results template `Standby evidence`, `Enable evidence`, `Disable evidence`, `Drop or emergency-stop evidence`, `Rejected-state gating evidence`, `Reconnect evidence`

- [ ] **Step 9: Write the Phase 3 section**

Required content:
- restate low-risk envelope:
  - `PositionMode + MotionType::Joint only`
  - `speed_percent <= 10`
  - `abs(delta) <= 0.035 rad`
- explain what the operator should watch physically:
  - correct direction
  - movement starts within `2s`
  - no obvious jump / oscillation / overshoot
  - repeated small moves remain consistent
  - settle within `10s`
  - return-to-start error `<= 0.05 rad`
- tell the operator to stop motion immediately if behavior exits the low-risk envelope
- map this phase to:
  - checklist Phase 3 items
  - results template `Move evidence`, `Return evidence`, `Return-to-start error`, `Jump / oscillation / overshoot notes`, `Repeated small moves consistency`

- [ ] **Step 10: Write the Phase 4 section**

Required content:
- controlled `can0` interruption and restore
- controller-side interruption if possible
- readable-state recovery with:
  - `cargo run -p piper-sdk --example robot_monitor -- --interface can0`
  - or `cargo run -p piper-sdk --example state_api_demo -- --interface can0`
- shell motion-gating probe:
  - `cargo run -p piper-cli -- shell`
  - `connect socketcan:can0`
  - `move --joints 0.02 --force`
- exact pass/fail wording:
  - pass if the shell rejects the probe with `未连接`, `电机未使能，请先使用 enable 命令`, or another failure that does not move the robot
  - fail if the command is accepted as normal motion before the safe baseline has been re-established
- tell the operator not to declare recovery complete until readable-state recovery and motion-gating checks are both observed
- map this phase to:
  - checklist Phase 4 items
  - results template `Fault type`, `Fault evidence`, `Timeout or dropped-feedback evidence`, `Recovery evidence`, `Readable-state recovery evidence`, `Shell probe connection`, `Motion-gating probe`

- [ ] **Step 11: Write the results-recording, stop-conditions, and minimal-command-sequence sections**

Required content:
- a short “how to record results” guide that points each phase to:
  - the checklist items to tick
  - the results template fields to fill
- a short “stop immediately” section covering:
  - Phase 0 no feedback
  - Phase 1 timing/disconnect failures
  - Phase 3 wrong direction / jump / oscillation / overshoot / abnormal transient
  - Phase 4 recovery not yet credible
- a final minimal end-to-end command sequence with the main commands in execution order

- [ ] **Step 12: Verify required headings, commands, and evidence strings**

Run:
`rg -n "文档定位|测试前安全基线|终端布局建议|全局判据与阈值|Phase 0|Phase 1|Phase 2|Phase 3|Phase 4|结果记录方法|立即停测条件|最小完整执行序列|client_monitor_hil_check|hil_joint_position_check|connect socketcan:can0|move --joints 0.02 --force|\\[PASS\\] connected and confirmed Standby|robot is not in confirmed Standby; run stop first|未连接|电机未使能，请先使用 enable 命令" docs/v0/piper_hil_operator_runbook.md`

Expected:
- matches for all required sections, commands, and evidence strings

- [ ] **Step 13: Verify no unfinished placeholders remain**

Run:
`rg -n "TODO|TBD|FIXME|XXX" docs/v0/piper_hil_operator_runbook.md`

Expected:
- no matches

- [ ] **Step 14: Commit**

```bash
git add docs/v0/piper_hil_operator_runbook.md
git commit -m "docs: add piper HIL operator runbook"
```
