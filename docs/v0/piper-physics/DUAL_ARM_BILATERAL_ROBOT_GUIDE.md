# 双臂 MuJoCo 双边控制实机联调指南

**状态**: 当前有效  
**适用代码**:

- [addons/piper-physics-mujoco/src/dual_arm.rs](/Users/viv/projs/piper-sdk-rs/addons/piper-physics-mujoco/src/dual_arm.rs)
- [addons/piper-physics-mujoco/examples/dual_arm_bilateral_mujoco.rs](/Users/viv/projs/piper-sdk-rs/addons/piper-physics-mujoco/examples/dual_arm_bilateral_mujoco.rs)
- [crates/piper-client/src/dual_arm.rs](/Users/viv/projs/piper-sdk-rs/crates/piper-client/src/dual_arm.rs)

**最后更新**: 2026-03-19

**配套模板**: [DUAL_ARM_BILATERAL_TUNING_LOG_TEMPLATE.md](./DUAL_ARM_BILATERAL_TUNING_LOG_TEMPLATE.md)

---

## 1. 目标与范围

本文档用于实机联调以下方案：

- 两条机械臂分别连接到两条独立 CAN 链路
- 双臂都运行在 MIT 模式
- 通过 MuJoCo 计算重力补偿 / 部分逆动力学 / 全逆动力学
- 在 SDK 双臂会话层上运行单向遥操或双边控制

本文档覆盖：

- 接线拓扑
- 上线顺序
- 推荐初始参数
- `master-follower` 到 `bilateral` 的切换顺序
- payload 与模式的运行时热更新
- 故障检查与验收标准

本文档不覆盖：

- 单总线双臂
- 固件透明主从
- 笛卡尔双边控制
- 夹爪双边力反馈

---

## 2. 当前架构

当前推荐架构如下：

1. 左臂和右臂分别保留独立 driver runtime。
2. 双臂协调逻辑在 `piper-client` 的 `dual_arm` 层统一执行。
3. MuJoCo 只存在于 addon `piper-physics-mujoco` 中，不进入主 SDK 默认依赖链。
4. 主臂和从臂各自持有独立 MuJoCo calculator。
5. 双边控制运行在单线程协调循环中。

推荐控制模式：

- 主臂：`gravity`
- 从臂：`partial`

这是当前最稳妥的默认起点：

- 主臂更轻，人工拖动更自然
- 从臂比纯重力补偿更容易稳定跟踪
- 相比一开始就用 `full`，对速度微分噪声更不敏感

---

## 3. 推荐硬件拓扑

强烈建议使用：

- 左臂: `can0`
- 右臂: `can1`
- 每条机械臂一条独立总线
- 同一个进程控制两臂

不建议作为首版联调方案：

- 两臂共用一条 CAN 总线
- 依赖固件 `master/slave` 模式完成双边控制

原因很直接：

- 当前 SDK 的双臂协调层是按“两条独立链路”设计的
- 跨臂同步以 host/system timestamp 为准
- 单总线与 shifted ID 不在当前交付范围内

---

## 4. 环境准备

第一次实机前，先做以下检查：

1. 主 workspace 检查通过：
   ```bash
   cargo clippy --all-targets --all-features -- -D warnings
   ```
2. MuJoCo addon 检查通过：
   ```bash
   just check-physics
   just test-physics --lib
   just clippy-physics
   ```
3. 两条 CAN 链路已单独验证可收发。
4. 工作空间无遮挡，双臂镜像运动不会互撞。
5. 已准备好软障碍物用于接触测试，不要一开始拿硬碰撞体测试双边反射。

---

## 5. 启动顺序

每次实机都按这个顺序：

1. 给左右臂上电。
2. 启动 `can0/can1` 或接好两只 GS-USB。
3. 先运行 `master-follower`，不要直接上 bilateral。
4. 手动把双臂摆到镜像零位。
5. 按 Enter 抓取零位标定。
6. 确认单向镜像稳定后，再切换 bilateral。

Linux 推荐首条命令：

```bash
cargo run --manifest-path addons/piper-physics-mujoco/Cargo.toml --example dual_arm_bilateral_mujoco -- \
  --left-interface can0 \
  --right-interface can1 \
  --teleop-mode master-follower \
  --master-dynamics-mode gravity \
  --slave-dynamics-mode partial \
  --track-kp 8.0 \
  --track-kd 1.0 \
  --master-damping 0.4
```

macOS / Windows 推荐首条命令：

```bash
cargo run --manifest-path addons/piper-physics-mujoco/Cargo.toml --example dual_arm_bilateral_mujoco -- \
  --left-serial LEFT123 \
  --right-serial RIGHT456 \
  --teleop-mode master-follower
```

---

## 6. 推荐初始参数

首轮实机建议从下面这一组开始：

| 参数 | 建议初值 |
|------|---------|
| `teleop-mode` | `master-follower` |
| `master-dynamics-mode` | `gravity` |
| `slave-dynamics-mode` | `partial` |
| `frequency-hz` | `200.0` |
| `track-kp` | `8.0` |
| `track-kd` | `1.0` |
| `master-damping` | `0.4` |
| `reflection-gain` | `0.20 ~ 0.25` |
| `qacc-lpf-cutoff-hz` | `20.0` |
| `max-abs-qacc` | `50.0` |

建议不要一开始就做这些事：

- 主从两侧都用 `full`
- `reflection-gain` 直接拉高到 `0.4+`
- payload 一开始就填很大但未经验证的值

---

## 7. 联调阶段划分

### 阶段 A：连接与标定

目标：

- 双臂连接成功
- 零位抓取成功
- 进入 MIT 后不立即 fault

如果 MIT 一打开就不稳定：

- 立即停下
- 检查 CAN 反馈是否完整
- 检查当前姿态是否接近关节限位
- 检查末端是否挂了未建模重物

### 阶段 B：单向镜像验证

在 `master-follower` 下检查：

- 从臂是否能稳定跟随主臂
- 6 个关节镜像方向是否都正确
- 静止时是否持续抖动
- 是否存在明显重力下垂

如果从臂过软、跟随滞后：

- 先升 `track-kp`
- 再小幅升 `track-kd`

如果从臂振荡：

- 先降 `track-kp`
- 必要时升 `track-kd`

### 阶段 C：双边反射上线

只有阶段 B 稳定了，才切 bilateral：

```bash
cargo run --manifest-path addons/piper-physics-mujoco/Cargo.toml --example dual_arm_bilateral_mujoco -- \
  --left-interface can0 \
  --right-interface can1 \
  --teleop-mode bilateral \
  --master-dynamics-mode gravity \
  --slave-dynamics-mode partial \
  --reflection-gain 0.20
```

反射增益建议按这个顺序爬升：

1. `0.20`
2. `0.25`
3. `0.30`

只有满足以下条件时才继续升：

- 自由空间稳定
- 接触软障碍物时反射方向正确
- 离开接触后不明显振荡

---

## 8. 三种动力学模式怎么选

### `gravity`

适合：

- 人手拖动主臂
- 验证镜像方向与标定
- 低速动作

优点：

- 最轻
- 最直观
- 对加速度估计不敏感

### `partial`

适合：

- 从臂中等速度跟踪
- 比纯重力补偿更稳
- 作为默认生产起点

优点：

- 比 `gravity` 少一些动态滞后
- 比 `full` 更保守

### `full`

适合：

- 从臂动作更快
- 加速度明显
- `partial` 仍然存在可感知的动态补偿不足

代价：

- 更依赖速度差分得到的 `qacc` 质量
- 对噪声更敏感

建议：

- 不要主从两侧一开始都用 `full`
- 优先尝试 `master=gravity, slave=partial`

---

## 9. Payload 处理流程

当前 demo 支持运行时热更新 payload，不需要重建 compensator。

stdin 命令：

```text
show
master payload <mass> <x> <y> <z>
slave payload <mass> <x> <y> <z>
master mode <gravity|partial|full>
slave mode <gravity|partial|full>
quit
```

推荐流程：

1. 两侧 payload 初始都设为 `0kg`
2. 先验证自由空间行为
3. 先更新从臂 payload
4. 再观察静态保持、自由空间跟随、接触反射
5. 如果主臂末端也挂了明显工具，再更新主臂 payload

示例：

```text
slave payload 0.45 0.00 0.00 0.08
show
```

payload 典型异常：

- 从臂静止时下垂：payload 偏小
- 某个方向特别硬、反方向特别轻：COM 偏差较大
- 自由空间里主臂一直有偏置“拉手感”：从臂模型或 payload 不准

---

## 10. 运行中重点观察项

每次结束都看 demo 输出的这些字段：

- `iterations`
- `read_faults`
- `submission_faults`
- `runtime_fault_exits`
- `max_inter_arm_skew`
- `left tx realtime overwrites`
- `right tx realtime overwrites`
- `last_error`

首轮联调的健康预期：

- `submission_faults = 0`
- `runtime_fault_exits = 0`
- 稳态运行时 `read_faults = 0`
- `max_inter_arm_skew` 保持在默认阈值以内
- overwrite 计数不会持续上涨

---

## 11. 故障收敛预期

当前实现的预期行为：

- 任一臂反馈 stale / misaligned:
  `safe_hold -> disable`
- 任一臂 runtime health unhealthy:
  `next-cycle emergency stop path`
- MuJoCo compensator 返回错误:
  `safe_hold -> disable`

强制验证至少要做一次：

1. 中断一侧反馈
2. 确认循环走安全退出路径
3. 确认两臂不会长期保持 torque enabled

---

## 12. 调参建议

每次只改一个方向：

- 从臂跟随滞后：
  增加 `track-kp`
- 从臂过冲：
  增加 `track-kd`
- 主臂拖动太粘：
  降低 `master-damping`
- 主臂接触反射发噪：
  降低 `reflection-gain`
- 接触反射太弱：
  在 payload 正确后提高 `reflection-gain`
- 从臂快速动作仍然动态滞后：
  尝试 `--slave-dynamics-mode full`
- `full` 模式下反射变脏：
  降低 `qacc-lpf-cutoff-hz` 或减小 `max-abs-qacc`

---

## 13. 验收标准

一组配置只有在下面都通过后才算可接受：

1. 零位抓取可重复成功
2. `master-follower` 可稳定运行数分钟
3. 六个关节镜像方向全部正确
4. 从臂静态姿态无明显重力塌陷
5. bilateral 下软接触反射方向正确且无持续振荡
6. 断开单侧反馈时系统能安全退出

---

## 14. 常用命令

主 SDK 校验：

```bash
cargo test -p piper-client --lib
cargo clippy --all-targets --all-features -- -D warnings
```

MuJoCo addon 校验：

```bash
just check-physics
just test-physics --lib
just clippy-physics
```

无物理依赖的双臂基线 demo：

```bash
cargo run --example dual_arm_bilateral_control -- \
  --left-interface can0 \
  --right-interface can1 \
  --mode master-follower
```

MuJoCo 双臂 bilateral demo：

```bash
cargo run --manifest-path addons/piper-physics-mujoco/Cargo.toml --example dual_arm_bilateral_mujoco -- \
  --left-interface can0 \
  --right-interface can1 \
  --teleop-mode bilateral \
  --master-dynamics-mode gravity \
  --slave-dynamics-mode partial \
  --reflection-gain 0.25
```

---

## 15. 联调记录建议

建议每次实机联调都同步填写：

- [DUAL_ARM_BILATERAL_TUNING_LOG_TEMPLATE.md](./DUAL_ARM_BILATERAL_TUNING_LOG_TEMPLATE.md)

最少也要记录：

1. 本次命令行参数
2. 主从补偿模式
3. payload 数值
4. 参数改动顺序
5. 运行报告字段
6. 结论与下次动作
