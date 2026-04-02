# Piper HIL 操作员执行手册

## 文档定位

这是一份面向操作员的执行伴侣文档，用来把既有 HIL 规范翻译成可按顺序执行的现场步骤。它**不替代**以下三份文档：

- [piper_hil_handbook.md](/home/viv/projs/piper-sdk-rs/docs/v0/piper_hil_handbook.md)
- [piper_hil_execution_checklist.md](/home/viv/projs/piper-sdk-rs/docs/v0/piper_hil_execution_checklist.md)
- [piper_hil_results_template.md](/home/viv/projs/piper-sdk-rs/docs/v0/piper_hil_results_template.md)

使用关系很简单：

- handbook 负责规范性判据和阈值
- checklist 负责现场勾选
- results template 负责事后留证
- 本 runbook 负责把执行顺序、终端分工、预期输出和停测条件讲清楚

## 适用对象

适用于熟悉终端操作、但不希望自己补全步骤的混合开发 / 测试人员。本文仍然只覆盖以下范围：

- Linux
- SocketCAN
- 一台真实 Piper arm
- low-risk motion only

本文**不新增**以下内容：

- GS-USB
- cross-platform coverage
- MIT mode
- high-risk motion

## 依赖文档

执行本 runbook 时，请始终把下面三份文档放在旁边：

- [piper_hil_handbook.md](/home/viv/projs/piper-sdk-rs/docs/v0/piper_hil_handbook.md)
- [piper_hil_execution_checklist.md](/home/viv/projs/piper-sdk-rs/docs/v0/piper_hil_execution_checklist.md)
- [piper_hil_results_template.md](/home/viv/projs/piper-sdk-rs/docs/v0/piper_hil_results_template.md)

推荐理解方式：

- handbook = 这一步为什么算通过
- checklist = 这一步要不要打勾
- results template = 这一步把哪条证据写进去

## 测试前安全基线

在 Phase 0 之前，先用操作员语言再次确认安全前提。下面这些条件必须全部成立：

- 只有一名操作员执行测试
- 在任何 motion enabled 之前，必须有第二人监督
- 机械臂已卸载
- 工作空间清空，且无碰撞风险
- 在任何 motion step 之前，急停按钮可直接触达
- 运动始终保持在本版本允许的 low-risk envelope 内
- 一旦观察到的行为越出这个 envelope，立即停测

如果这些条件没有被写下来或没有被现场确认，不要进入 Phase 0 的后续步骤。

## 终端布局建议

建议固定使用三个终端，便于分工和留证：

- `Terminal 1`：主执行终端，跑 Phase 0 和主要 helper
- `Terminal 2`：只读佐证终端，跑 `robot_monitor` 或 `state_api_demo`
- `Terminal 3`：CLI 生命周期 / 恢复 / 记录终端，跑 `piper-cli`

不需要每个 phase 都同时开着三个终端，但建议保持这个布局不变，避免现场切换时漏看输出。

## 全局判据与阈值

本文沿用 handbook 中已经接受的阈值，不新增任何门槛：

- connection budget `<= 5s`
- reconnect budget `<= 5s`
- initial complete monitor snapshot `<= 200ms`
- observation window `15 min`
- `PositionMode + MotionType::Joint only`
- `speed_percent <= 10`
- `abs(delta) <= 0.035 rad`
- return-to-start tolerance `<= 0.05 rad`

这些阈值在 Phase 1-4 中都沿用相同含义：

- `client_monitor_hil_check` 负责 connection budget、first snapshot budget 和 observation window
- `hil_joint_position_check` 负责 Standby、enable、move、return 的低风险运动证据
- `piper-cli` 负责 disable、stop、recovery gating 和故障后最小安全验证

## Phase 0: Preflight and Safety Baseline

### 目的

确认测试环境、主机信息和总线状态都已就绪，并且在任何 motion 之前看到了真实反馈。

### 使用终端

- `Terminal 1`
- `Terminal 2` 可选，用于交叉观察 `robot_monitor`

### 执行步骤

1. 记录主机和源码基线：
   ```bash
   git rev-parse HEAD
   uname -a
   rustc --version
   cargo --version
   ```
2. 记录 `can0` 状态和计数：
   ```bash
   ip -details link show can0
   ip -statistics link show can0
   ```
3. 如果 `can0` 处于 down 状态，按现场配置把它拉起。命令必须如实记录：
   ```bash
   sudo ip link set can0 up type can bitrate 1000000
   ```
4. 确认机械臂处于安全初始姿态，且工作区清空。
5. 在任何 motion 之前，先确认总线上有真实反馈：
   ```bash
   cargo run -p piper-sdk --example robot_monitor -- --interface can0
   ```
6. 把第一次出现的 live joint 或 gripper update 当作“反馈已存在”的证据。

### 预期输出 / 观察点

- 主机和版本信息被记录
- `can0` 的状态和统计信息可读
- `robot_monitor` 能看到 live feedback
- 第一条 live joint 或 gripper update 明确出现

### 何时判失败并停止

- `can0` 状态不清楚或 bitrate 无法确认
- 安全前提没有写入记录
- 看不到 live feedback
- 现场无法确认机械臂处于安全初始姿态

### 需要填写到结果模板的字段与 checklist 勾选

- Checklist: `Run Setup` 里的 git SHA、date、operator、supervisor、host OS、kernel、`rustc`、`cargo`、robot model、firmware version、CAN interface、bitrate、workspace clear、arm unloaded、logging ready
- Checklist: `Phase 0: Preflight and Safety Baseline` 里的 `can0` status/counters、bring `can0` up、safe initial pose、`robot_monitor` live feedback present
- Results template: `Run Metadata`，`Phase 0: Preflight and Safety Baseline`，重点记录 host / interface 元数据、`can0` 配置、live feedback 证据、第一条 live joint 或 gripper update

## Phase 1: Connection and Read-Only Observation

### 目的

验证只读路径的端到端连通性：SocketCAN、protocol decode、driver sync、first snapshot warmup 和持续观测窗口。

### 使用终端

- `Terminal 1`：主 helper
- `Terminal 2`：`robot_monitor` 或 `state_api_demo`

### 执行步骤

1. 启动主 helper：
   ```bash
   cargo run -p piper-sdk --example client_monitor_hil_check -- --interface can0 --baud-rate 1000000 --observation-window-secs 900
   ```
2. 在 helper 输出里找实际的 timing 行，通常会出现：
   - `Connected in ...`
   - `First feedback arrived in ...`
   - `First complete monitor snapshot in ...`
   - `Observation window finished after ...`
3. 记录规则如下：
   - `Connected in ...` 记录为 connection budget 证据，必须 `<= 5s`
   - `First complete monitor snapshot in ...` 记录为 first snapshot budget 证据，必须 `<= 200ms`
   - `Observation window finished after ...` 记录为 `15 min` observation window 完成证据
   - `First feedback arrived in ...` 作为辅助佐证，帮助判断是否真的看到了 live feedback
4. 让 helper 完整运行满 `15 min` observation window。
5. 必要时在 `Terminal 2` 并行运行：
   ```bash
   cargo run -p piper-sdk --example robot_monitor -- --interface can0
   ```
   或：
   ```bash
   cargo run -p piper-sdk --example state_api_demo -- --interface can0
   ```
6. 如果需要补充佐证，用 `Terminal 2` 继续看只读状态，不要在这个 phase 里启用 motion。

### 预期输出 / 观察点

- connection 成功且在 `<= 5s`
- first complete snapshot 在 `<= 200ms`
- 整个 `15 min` observation window 持续稳定
- `robot_monitor` 或 `state_api_demo` 可以作为旁证，但不替代主 helper

### 何时判失败并停止

- helper timeout
- first snapshot 超时或缺失
- 意外 disconnect
- 观察窗口没有完整跑完

### 需要填写到结果模板的字段与 checklist 勾选

- Checklist: `Phase 1: Connection and Read-Only Observation` 里的 `client_monitor_hil_check`、`<= 5s` connection budget、`<= 200ms` first snapshot、`15 min` observation window、`robot_monitor` / `state_api_demo` 旁证
- Results template: `Phase 1: Connection and Read-Only Observation`，重点写 `Connection budget` 对应 `Connected in ...`，`First snapshot budget` 对应 `First complete monitor snapshot in ...`，`Observation window` 对应 `Observation window finished after ...`，并补充 `Observed` / `Notes` / `Artifacts`

## Phase 2: Safe Lifecycle and State Transitions

### 目的

验证生命周期切换、Standby 确认、enable / disable、拒绝态 gating、以及恢复连接的可重复性。

### 使用终端

- `Terminal 1`：`hil_joint_position_check`
- `Terminal 3`：`piper-cli shell` 和 `piper-cli stop`
- `Terminal 2`：可选，只读佐证

### 执行步骤

1. 用 helper 覆盖这部分的主证据：
   ```bash
   cargo run -p piper-sdk --example hil_joint_position_check -- --interface can0 --baud-rate 1000000 --joint 1 --delta-rad 0.02 --speed-percent 10
   ```
2. `hil_joint_position_check` 的成功路径默认按下面顺序收尾：先完成 `move`，再回到初始 snapshot，随后按配置的 rest pose 停靠，最后才会 disable。
3. 如果显式传入 `--no-park`，则只跳过“停靠后再 disable”这一步，其他通过判据不变。
4. 重点看 helper 中下面这些 accepted evidence lines：
   - `[PASS] connected and confirmed Standby`
   - `[PASS] enabled PositionMode motion=Joint speed_percent=...`
   - `[PASS] settle step=move ...`
   - `[PASS] settle step=return ...`
5. 对 explicit disable path，使用：
   ```bash
   cargo run -p piper-cli -- shell
   connect socketcan:can0
   enable
   disable
   exit
   ```
6. `disable` 后，立刻用只读 helper 确认系统回到 non-driving state：
   ```bash
   cargo run -p piper-sdk --example robot_monitor -- --interface can0
   ```
   或：
   ```bash
   cargo run -p piper-sdk --example state_api_demo -- --interface can0
   ```
   这一步要作为 disable 的一部分来执行，不要省略。
7. 对外部 stop path，使用：
   ```bash
   cargo run -p piper-cli -- stop --target socketcan:can0
   ```
8. `stop` 之后，同样立刻用 `robot_monitor` 或 `state_api_demo` 确认 non-driving state。
9. 对 rejected-state gating，使用一个可复现的流程：
   - `Terminal 3` 打开：
     ```bash
     cargo run -p piper-cli -- shell
     ```
   - 在该 session 内执行：
     ```text
     connect socketcan:can0
     enable
     ```
   - 保持这个 session 继续打开，不要执行 `disable`
   - 在 `Terminal 1` 重新运行：
   ```bash
   cargo run -p piper-sdk --example hil_joint_position_check -- --interface can0 --baud-rate 1000000 --joint 1 --delta-rad 0.02 --speed-percent 10
   ```
   - 期望看到 `robot is not in confirmed Standby; run stop first`
   - 然后在 `Terminal 3` 执行 `disable`，再 `exit`，把 baseline 恢复
8. 对 reconnect 复检，重新启动 helper，重新核对 `<= 5s` 和 `<= 200ms`。
9. 只要发生 state 变化不一致、或拒绝态没有按预期阻断，就立刻停下来，不要继续往 Phase 3 推。

### 预期输出 / 观察点

- Standby、enable、move、return 这四类证据都出现
- disable 不触发运动
- `piper-cli stop` 让系统回到非 driving 状态，并且只读 helper 重新确认该状态
- 重连后仍满足 `<= 5s` 和 `<= 200ms`
- 受限状态下的 motion probe 被拒绝

### 何时判失败并停止

- `[PASS] connected and confirmed Standby` 没出现
- `[PASS] enabled PositionMode motion=Joint speed_percent=...` 没出现
- `robot is not in confirmed Standby; run stop first` 没出现但本应拒绝
- disable 后仍表现为可疑 motion
- reconnect 的 budget 超限

### 需要填写到结果模板的字段与 checklist 勾选

- Checklist: `Phase 2: Safe Lifecycle and State Transitions` 里的 `hil_joint_position_check`、确认 Standby、确认 enable 进入 `PositionMode + MotionType::Joint`、确认 move / return、explicit disable、`piper-cli stop`、rejected-state gating、reconnect re-check
- Results template: `Phase 2: Safe Lifecycle and State Transitions`，必填 `Standby evidence`，`Enable evidence`，`Disable evidence`，`Drop or emergency-stop evidence`，`Rejected-state gating evidence`，`Reconnect evidence`，并补充 `Observed` / `Notes` / `Artifacts`；其中 disable / stop 之后的 `robot_monitor` 或 `state_api_demo` 输出也要放进 `Disable evidence` 或 `Drop or emergency-stop evidence`

## Phase 3: Low-Risk Motion Validation

### 目的

在真实硬件上验证最小可行的低风险控制回路：发出命令、发生运动、反馈返回、并且 SDK 视图与物理行为一致。

### 使用终端

- `Terminal 1`：`hil_joint_position_check`
- `Terminal 2`：可选，用于肉眼观察和 `robot_monitor`

### 执行步骤

1. 重新确认低风险约束：
   - `PositionMode + MotionType::Joint only`
   - `speed_percent <= 10`
   - `abs(delta) <= 0.035 rad`
2. 运行低风险 helper：
   ```bash
   cargo run -p piper-sdk --example hil_joint_position_check -- --interface can0 --baud-rate 1000000 --joint 1 --delta-rad 0.02 --speed-percent 10
   ```
3. 观察物理动作是否方向正确，且在 `2s` 内开始移动。
4. 观察是否有明显 jump、oscillation、overshoot 或异常 transient。
5. 观察每个 step 是否在 `10s` 内 settle。
6. 如果要重复验证，继续做另一组安全的小步长，但不要超出同一组阈值。
7. 关注 helper 的：
   - `[PASS] settle step=move ...`
   - `[PASS] settle step=return ...`
8. 关心的不是“看起来差不多”，而是 return-to-start error 是否 `<= 0.05 rad`。

### 预期输出 / 观察点

- motion direction 与命令一致
- feedback trend 与物理动作一致
- 运动在 `2s` 内开始
- 每步在 `10s` 内 settle
- return-to-start error `<= 0.05 rad`
- repeated small moves 保持一致

### 何时判失败并停止

- wrong direction
- jump / oscillation / overshoot
- abnormal transient
- feedback 和物理动作明显背离
- 运动越出 low-risk envelope

### 需要填写到结果模板的字段与 checklist 勾选

- Checklist: `Phase 3: Low-Risk Motion Validation` 里的低风险约束确认、`hil_joint_position_check`、`PositionMode + MotionType::Joint only`、`speed_percent <= 10`、`abs(delta) <= 0.035 rad`、每步人工确认、无 MIT / Cartesian / Linear / Circular motion
- Results template: `Phase 3: Low-Risk Motion Validation`，必填 `Joint`，`Delta rad`，`Speed percent`，`Move evidence`，`Return evidence`，`Return-to-start error`，`Jump / oscillation / overshoot notes`，`Repeated small moves consistency`，并补充 `Observed` / `Notes` / `Artifacts`

## Phase 4: Fault and Recovery Validation

### 目的

验证常见现场故障能明确降级为安全状态，并且在恢复后仍能重新证明 readable state 和 motion gating。

### 使用终端

- `Terminal 1`：故障后重新跑只读 helper
- `Terminal 2`：可选，只读佐证
- `Terminal 3`：shell probe 和恢复确认

### 执行步骤

1. 先准备一个正在运行的只读 helper，用来在 fault active 期间观察缺失反馈：
   ```bash
   cargo run -p piper-sdk --example client_monitor_hil_check -- --interface can0 --baud-rate 1000000 --observation-window-secs 900
   ```
2. 在不运动时，执行一个明确的 `can0` fault：
   ```bash
   sudo ip link set can0 down
   ```
   如果现场确实需要，也可以再做 controller-side interruption 作为附加项，但这不是必需项。
3. 在 fault active 期间，保留 helper 或 monitor 窗口，直接记录 timeout / disconnect / missing-feedback 证据。写结果时优先抄原始输出里的实际行，例如：
   - `timeout`
   - `disconnect`
   - `missing feedback`
   - 任何表明观测中断或数据缺失的原始输出
4. 先不要恢复，先把 fault active 的证据记下来。
5. 现在恢复 `can0`：
   ```bash
   sudo ip link set can0 up type can bitrate 1000000
   ```
6. 故障清除后，先启动一轮 fresh helper：
   ```bash
   cargo run -p piper-sdk --example client_monitor_hil_check -- --interface can0 --baud-rate 1000000 --observation-window-secs 900
   ```
7. 重新核对这轮 fresh helper 是否满足：
   - reconnect within `<= 5s`
   - first complete snapshot within `<= 200ms`
8. 在 fresh helper 证据之外，再用只读 helper 重新确认 readable state：
   ```bash
   cargo run -p piper-sdk --example robot_monitor -- --interface can0
   ```
   或：
   ```bash
   cargo run -p piper-sdk --example state_api_demo -- --interface can0
   ```
9. shell probe 的时点是：**在 fault 已经被诱发之后、但在 recovery 被宣告完成之前**。不要把它放到 fault 之前，也不要跳过它。
10. 具体 shell probe：
   ```bash
   cargo run -p piper-cli -- shell
   connect socketcan:can0
   move --joints 0.02 --force
   ```
11. 这里的 accepted decision 有三种：
   - pass if the shell rejects the probe with `未连接`
   - pass if the shell rejects the probe with `电机未使能，请先使用 enable 命令`
   - pass if the shell gives another failure that does not move the robot
12. 这里明确失败的情况只有一种：
   - fail if the command is accepted as normal motion before the safe baseline has been re-established
13. 不要在 fresh helper 复检、readable-state recovery 和 motion-gating checks 都出现之前宣布 recovery complete。

### 预期输出 / 观察点

- 故障在日志或返回值中是显式的
- 系统向 safety 方向降级
- fresh helper 在恢复后重新证明 `<= 5s` reconnect 和 `<= 200ms` first snapshot
- 故障清除后 readable state 重新出现
- shell probe 被拒绝，或失败但不引发运动
- motion gating 在 safe baseline 未恢复前仍然生效

### 何时判失败并停止

- `sudo ip link set can0 down` 后，helper 仍然像健康控制一样持续输出，完全没有 timeout / disconnect / missing-feedback 迹象
- fresh helper 没有重新证明 `<= 5s` reconnect 和 `<= 200ms` first snapshot
- readable-state recovery 没有出现
- shell probe 在 recovery 完成前被接受成正常 motion
- 恢复后把旧状态误认为新状态
- `move --joints 0.02 --force` 被当作正常 motion 接受
- 在安全基线恢复前，控制仍然可用

### 需要填写到结果模板的字段与 checklist 勾选

- Checklist: `Phase 4: Fault and Recovery Validation` 里的 `sudo ip link set can0 down` fault、故障期间的 timeout / disconnect / missing-feedback 观察、`sudo ip link set can0 up type can bitrate 1000000` restore、fresh helper reconnect 复检、`robot_monitor` / `state_api_demo` readable-state recovery、fault 后的 shell probe 被拒绝、故障后 motion gating 仍然生效
- Results template: `Phase 4: Fault and Recovery Validation`，必填 `Fault type`，`Fault evidence`，`Timeout or dropped-feedback evidence`，`Recovery evidence`，`Readable-state recovery evidence`，`Shell probe connection`，`Motion-gating probe`，并记录 fresh helper 的 `Connection budget` 和 `First snapshot budget`

## 结果记录方法

建议按 phase 填写结果，而不是等到最后回忆。最实用的方式是：

- Phase 0：把主机、`can0` 和 live feedback 证据写进 `Run Metadata` 和 `Phase 0`
- Phase 1：把 `client_monitor_hil_check` 的三个时间预算写进 `Phase 1`
- Phase 2：把 Standby / enable / disable / stop / rejected-state / reconnect 证据写进 `Phase 2`
- Phase 3：把运动方向、速度、位移、返回误差和异常观察写进 `Phase 3`
- Phase 4：把故障类型、故障证据、恢复证据和 shell probe 结果写进 `Phase 4`

对应关系可以直接按 checklist 打勾：

- `Run Setup` 对应 `Run Metadata`
- `Phase 0` 对应 `Phase 0`
- `Phase 1` 对应 `Phase 1`
- `Phase 2` 对应 `Phase 2`
- `Phase 3` 对应 `Phase 3`
- `Phase 4` 对应 `Phase 4`

## 立即停测条件

出现下面任一情况，就不要继续往下执行：

- Phase 0 不能确认 live feedback
- Phase 1 出现 timing budget 超限或意外 disconnect
- Phase 3 出现 wrong direction、jump、oscillation、overshoot 或异常 transient
- Phase 4 的 recovery 还不可信，或者 motion gating 还没被再次证明

## 最小完整执行序列

下面是一条最小、按顺序的执行链骨架。它**不替代**上面的 phase 细则，也不能跳过 rejected-state rerun、fault induction/restoration、fresh helper rerun、readable-state recovery confirmation 这些步骤：

```bash
# Phase 0: preflight
git rev-parse HEAD
uname -a
rustc --version
cargo --version
ip -details link show can0
ip -statistics link show can0
cargo run -p piper-sdk --example robot_monitor -- --interface can0

# Phase 1: read-only observation
cargo run -p piper-sdk --example client_monitor_hil_check -- --interface can0 --baud-rate 1000000 --observation-window-secs 900

# Phase 2: lifecycle and rejected-state gating
cargo run -p piper-sdk --example hil_joint_position_check -- --interface can0 --baud-rate 1000000 --joint 1 --delta-rad 0.02 --speed-percent 10
cargo run -p piper-cli -- shell
connect socketcan:can0
enable
# keep this shell session active; do not `disable` yet
cargo run -p piper-sdk --example hil_joint_position_check -- --interface can0 --baud-rate 1000000 --joint 1 --delta-rad 0.02 --speed-percent 10
disable
cargo run -p piper-sdk --example robot_monitor -- --interface can0
exit

# Phase 2: explicit stop path and read-only corroboration
cargo run -p piper-cli -- stop --target socketcan:can0
cargo run -p piper-sdk --example state_api_demo -- --interface can0

# Phase 4: fault active observation, then recovery
cargo run -p piper-sdk --example client_monitor_hil_check -- --interface can0 --baud-rate 1000000 --observation-window-secs 900
sudo ip link set can0 down
# while the fault is active, record timeout / disconnect / missing-feedback evidence from the running helper
sudo ip link set can0 up type can bitrate 1000000
# after recovery, rerun the fresh helper to prove reconnect and first snapshot timing
cargo run -p piper-sdk --example client_monitor_hil_check -- --interface can0 --baud-rate 1000000 --observation-window-secs 900
cargo run -p piper-sdk --example robot_monitor -- --interface can0

# final motion probe only after recovery evidence exists
cargo run -p piper-cli -- shell
connect socketcan:can0
move --joints 0.02 --force
exit
```
