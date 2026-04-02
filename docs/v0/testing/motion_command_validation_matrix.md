# Motion Command Validation Matrix

本页给操作员提供一套可重复执行的运动指令验证矩阵。目标不是“把所有模式一次性跑完”，而是按模式逐个验证：

- 指令 API 是否和 `MotionType` 匹配
- 机器人是否从确认的 `Standby` 起步
- HIL helper 是否观察到真实进展
- 运行后是否回到安全状态

## 运行前检查

- 确认机器人处于已连接、已确认失能的 `Standby`
- 确认工作空间清空，留出小范围安全运动空间
- 准备好 `stop` 或急停
- SocketCAN 默认示例使用 `--interface can0`
- 所有 helper 都会在失败时打印 `[FAIL] ...`

## `hil_joint_position_check` 成功路径说明

这个 helper 额外支持两个收尾参数：

- `--park-orientation upright|left|right`：选择成功路径上使用的停靠朝向，默认 `upright`
- `--no-park`：跳过成功路径上的停靠步骤，直接在回到初始 snapshot 后 disable

默认成功路径仍然是：`move` -> 回到初始 snapshot -> 按 `--park-orientation` 对应的 rest pose 停靠 -> disable。

## 模式矩阵

| 模式 | Command API | HIL helper | 预期通过条件 | 安全参数上限 | 清理/停止 |
|---|---|---|---|---|---|
| Joint Position | `send_position_command()` | `hil_joint_position_check` | 去程/回程都观察到关节位移，最终误差在容差内 | `delta_rad <= 0.035`, `speed_percent <= 10` | 默认成功路径先按 `--park-orientation` 停靠再 disable；`--no-park` 则在回到初始 snapshot 后直接 disable；失败时先 `stop` |
| Cartesian | `command_cartesian_pose()` | `hil_cartesian_pose_check` | 观察到末端位姿平移进展，目标与回程都在平移容差内 | `||delta|| <= 0.02 m`, `speed_percent <= 10` | helper 完成后自动 drop；失败时先 `stop` |
| Linear | `move_linear()` | `hil_linear_motion_check` | 观察到末端平移进展，最终误差在容差内，采样轨迹未明显偏离直线段 | `|delta_x| <= 0.02 m`, `speed_percent <= 10` | helper 完成后自动 drop；失败时先 `stop` |
| Circular | `move_circular()` | `hil_circular_motion_check` | 至少一个采样点进入 via 点邻域，最终点单独收敛 | `|delta_x| <= 0.02 m`, `|via_offset| <= 0.015 m`, `speed_percent <= 10` | helper 完成后自动 drop；失败时先 `stop` |
| MIT | `command_torques()` | `hil_mit_hold_check` | 选定关节出现小但可观测的位置响应，回程误差在容差内 | `delta_rad <= 0.02`, `speed_percent <= 10`, 低增益起步 | helper 完成后自动 drop；失败时先 `stop` |
| ReplayMode | `replay_recording()` | `hil_replay_mode_check` | 成功进入 `ReplayMode`，回放完成后返回确认的 `Standby` | `speed <= 5.0`; 推荐 `<= 2.0` | 失败时检查录制文件并重新 `stop` |
| Gripper | `open_gripper()`, `close_gripper()`, `set_gripper()` | `hil_gripper_check` | 打开、闭合、再次打开都观察到夹爪位置变化 | `close_effort <= 1.0`, `speed_percent <= 10` | helper 完成后自动 drop；失败时先 `stop` |

## 推荐命令

```bash
cargo run -p piper-sdk --example hil_joint_position_check -- --interface can0 --joint 1 --delta-rad 0.02 --speed-percent 5
cargo run -p piper-sdk --example hil_cartesian_pose_check -- --interface can0 --delta-x 0.01 --delta-y 0.0 --delta-z 0.0 --speed-percent 5
cargo run -p piper-sdk --example hil_linear_motion_check -- --interface can0 --delta-m 0.01 --speed-percent 5
cargo run -p piper-sdk --example hil_circular_motion_check -- --interface can0 --delta-m 0.012 --via-offset-m 0.006 --speed-percent 5
cargo run -p piper-sdk --example hil_mit_hold_check -- --interface can0 --joint 1 --delta-rad 0.01 --kp 5.0 --kd 0.5 --speed-percent 5
cargo run -p piper-sdk --example hil_replay_mode_check -- --interface can0 --recording-file demo_recording.bin --speed 1.0
cargo run -p piper-sdk --example hil_gripper_check -- --interface can0 --close-effort 0.3 --speed-percent 5
```

## Soak 命令

以下命令用于抓时序和偶发问题。任何一轮失败都应立即中止并排查。

```bash
for i in $(seq 1 50); do
  cargo run -p piper-sdk --example hil_joint_position_check -- --interface can0 --joint 1 --delta-rad 0.02 --speed-percent 5 || break
done

for i in $(seq 1 50); do
  cargo run -p piper-sdk --example hil_cartesian_pose_check -- --interface can0 --delta-x 0.01 --delta-y 0.0 --delta-z 0.0 --speed-percent 5 || break
done

for i in $(seq 1 50); do
  cargo run -p piper-sdk --example hil_linear_motion_check -- --interface can0 --delta-m 0.01 --speed-percent 5 || break
done

for i in $(seq 1 50); do
  cargo run -p piper-sdk --example hil_circular_motion_check -- --interface can0 --delta-m 0.012 --via-offset-m 0.006 --speed-percent 5 || break
done

for i in $(seq 1 50); do
  cargo run -p piper-sdk --example hil_mit_hold_check -- --interface can0 --joint 1 --delta-rad 0.01 --kp 5.0 --kd 0.5 --speed-percent 5 || break
done

for i in $(seq 1 20); do
  cargo run -p piper-sdk --example hil_replay_mode_check -- --interface can0 --recording-file demo_recording.bin --speed 1.0 || break
done

for i in $(seq 1 50); do
  cargo run -p piper-sdk --example hil_gripper_check -- --interface can0 --close-effort 0.3 --speed-percent 5 || break
done
```

## 剩余风险

- HIL helper 依赖机器人自身反馈，不是外部测量系统
- Linear/Circular 的轨迹判据建立在 monitor 采样点之上，不等于高精度轨迹重建
- ReplayMode helper 只验证回放流程和状态返回，不验证录制内容本身是否业务正确
- Gripper helper 默认假设夹爪为空载；带物体时闭合终点可能提前停止
