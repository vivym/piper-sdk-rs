# 实施清单变更日志

## v1.2 修订 (2026-01-23) - 数学严谨性增强

### 📌 修订原因

经过代码级审查，从**数学严谨性**和**数值稳定性**角度发现3个潜在问题，虽然不影响当前功能，但在未来扩展和极端情况下可能导致数值错误或测试不稳定。

---

## ✅ v1.2 修正内容

### 1. 🧮 TrajectoryPlanner 时间缩放陷阱修正

**位置**: Phase 4 任务 4.3

**问题分析**:
- 当前实现使用归一化时间 `t ∈ [0, 1]` 来计算三次样条
- 边界条件设为 `v0 = v1 = 0`（起止速度为 0）
- **潜在隐患**: 如果未来支持 Via Points（中间点速度 ≠ 0），必须进行时间缩放

**数学原理**:
```text
归一化时间导数关系：
dp/dt_physical = (dp/dt_normalized) * (dt_normalized/dt_physical)
                = (dp/dt_normalized) / T

因此：v_normalized = v_physical * T
```

**修正方案**:
- 在 `TrajectoryPlanner::new` 中添加详细注释，说明时间缩放的必要性
- 在 `compute_cubic_spline` 中添加完整的数学文档，解释时间缩放原理
- 提供未来扩展的代码示例（Via Points）

**代码改进**:
```rust
// 当前：起止速度均为 0（点对点运动）
Self::compute_cubic_spline(s.0, 0.0, e.0, 0.0)

// 未来扩展示例（Via Points）：
// let v_start_scaled = v_start_physical * duration_sec;
// let v_end_scaled = v_end_physical * duration_sec;
// Self::compute_cubic_spline(s.0, v_start_scaled, e.0, v_end_scaled)
```

**影响**:
- 防止未来扩展时的数学错误
- 提高代码可维护性

---

### 2. 🛡️ Quaternion 数值稳定性修正

**位置**: Phase 1 任务 1.5

**问题分析**:
- `Quaternion::normalize` 在四元数接近 0 时会除零 → NaN
- 理论上不应发生，但在初始化错误、序列化错误或数值累积误差时可能出现

**修正方案**:
- 添加 `norm_sq < 1e-10` 检查
- 返回默认单位四元数 `(1, 0, 0, 0)`（无旋转）
- 记录警告日志

**代码改进**:
```rust
pub fn normalize(&self) -> Self {
    let norm_sq = self.w * self.w + self.x * self.x +
                  self.y * self.y + self.z * self.z;

    // ✅ 数值稳定性检查：避免除零
    if norm_sq < 1e-10 {
        log::warn!("Normalizing near-zero quaternion, returning identity");
        return Quaternion { w: 1.0, x: 0.0, y: 0.0, z: 0.0 };
    }

    let norm = norm_sq.sqrt();
    // ... 正常归一化
}
```

**测试改进**:
- 新增 `test_quaternion_near_zero_stability` 测试
- 验证近零和完全为零的情况不产生 NaN

**影响**:
- 提高鲁棒性
- 防止 NaN 扩散

---

### 3. 🧪 轨迹平滑性测试方法改进

**位置**: Phase 4 任务 4.3

**问题分析**:
- 原测试使用数值微分计算加速度：`accel = (vel - last_vel) / dt`
- 离散采样会引入数值噪声
- 可能导致 Flaky Test（偶尔失败）

**改进方案**:

**方法 1: 速度连续性检查（更可靠）**
```rust
// 检查相邻速度变化
let max_vel_jump = velocities_samples
    .windows(2)
    .map(|w| (w[1] - w[0]).abs())
    .max_by(|a, b| a.partial_cmp(b).unwrap())
    .unwrap_or(0.0);

// 以 1kHz 采样，速度变化应 < 0.02 rad/s per ms
assert!(max_vel_jump < 0.02);
```

**方法 2: 方向变化次数检查**
```rust
// 三次样条应该只有 1 个拐点（加速 -> 减速）
let direction_changes = velocities_samples
    .windows(3)
    .filter(|w| {
        let d1 = w[1] - w[0];
        let d2 = w[2] - w[1];
        d1.signum() != d2.signum() && d1.abs() > 1e-6
    })
    .count();

assert!(direction_changes <= 2);
```

**方法 3: 解析解边界条件验证（最可靠）**
```rust
// 直接访问样条系数验证边界条件
let coeffs = &planner.spline_coeffs[Joint::J1];

// 起始速度 = 0
assert!(coeffs.b.abs() < 1e-10);

// 终止速度 = 0
let v_end = coeffs.b + 2.0 * coeffs.c + 3.0 * coeffs.d;
assert!(v_end.abs() < 1e-10);
```

**影响**:
- 测试更稳定（避免 Flaky Test）
- 验证更精确（使用解析解）
- 数值微分测试保留但放宽阈值

---

## 📊 总体影响

### 代码质量提升

| 维度 | 提升 |
|------|------|
| **数学严谨性** | ✅ 时间缩放文档化，防止未来错误 |
| **数值稳定性** | ✅ 四元数除零保护，避免 NaN |
| **测试可靠性** | ✅ 解析解验证，消除 Flaky Test |
| **可维护性** | ✅ 详细数学注释，便于扩展 |

### 工期影响

- **无变化**: 这些改进都是文档和测试优化，不影响总工期

---

## v1.1 修订 (2026-01-23)

### 📌 修订原因

经过对 v3.2 设计方案的深度审查，发现实施清单遗漏了4个关键要素，可能导致实施过程中的功能缺失或安全隐患。

---

## ✅ 修正内容

### 1. 🧩 新增任务 1.5：笛卡尔空间类型

**位置**: Phase 1（基础类型系统）

**问题**:
- v3.2 设计中 `Piper` 有 `send_cartesian_command(pose: CartesianPose)` 方法
- 但实施清单中缺少 `CartesianPose` 等笛卡尔空间类型的实现任务

**解决方案**:
- 新增文件: `src/types/cartesian.rs`
- 实现类型:
  - `CartesianPose` (位置 + 姿态)
  - `Position3D` (x, y, z)
  - `Quaternion` (四元数)
  - `CartesianVelocity` (线速度 + 角速度)
  - `CartesianEffort` (力 + 力矩)
- 实现功能:
  - 欧拉角 ↔ 四元数转换
  - 四元数归一化
  - 完整的单元测试和属性测试

**影响**:
- 工期: Phase 1 增加 1 天（5 天 → 6 天）
- 总工期: +1 天

**文件变更**:
```diff
+ 任务 1.5: 笛卡尔空间类型 ⭐ NEW
  - [ ] 实现 CartesianPose 结构
  - [ ] 实现 Quaternion 及转换方法
  - [ ] 单元测试和属性测试
```

---

### 2. 📉 增强任务 2.4：Observer 夹爪反馈验收标准

**位置**: Phase 2（读写分离 + 性能优化）

**问题**:
- 代码框架中包含 `gripper_state()` 方法
- 但验收标准未明确要求解析 CAN 协议中的夹爪反馈字段
- 可能导致实施时遗漏底层 CAN 帧解析逻辑

**解决方案**:
- 在验收标准中明确要求:
  - ✅ **必须解析 CAN 协议中的夹爪反馈字段**（0x4xx ID 的 CAN 帧）
  - 夹爪位置（开口宽度，米）
  - 夹爪力度（N·m）
  - 夹爪使能状态（bool）
- 添加实施注意事项:
  - 确保 `Observer` 的状态更新逻辑中包含对夹爪 CAN 帧的解析
  - 夹爪状态应该与关节状态以相同的频率更新
  - 添加夹爪状态解析的单元测试（模拟 CAN 帧）

**影响**:
- 工期: 无变化
- 质量: 提高（防止遗漏）

**文件变更**:
```diff
  **验收标准**:
  - ✅ 多线程并发读取安全
  - ✅ 夹爪状态查询完整
+ - ✅ **必须解析 CAN 协议中的夹爪反馈字段**（0x4xx ID 的 CAN 帧）
+   - 夹爪位置（开口宽度，米）
+   - 夹爪力度（N·m）
+   - 夹爪使能状态（bool）
  - ✅ 性能开销低（< 100ns per query）
```

---

### 3. 🔄 新增任务 4.3：TrajectoryPlanner（轨迹规划器）

**位置**: Phase 4（Tick/Iterator + 控制器）

**问题**:
- v3.0/3.2 设计中多次提到 `TrajectoryPlanner` (Iterator 模式) 是核心亮点
- 但实施清单中只在 Phase 5 的示例里略微带过
- 这是一个核心功能模块，不应只作为示例存在

**解决方案**:
- 新增文件: `src/control/trajectory.rs`
- 实现类型: `TrajectoryPlanner` (实现 `Iterator` trait)
- 实现功能:
  - 三次样条插值（Cubic Spline）
  - 输出 `(JointArray<Rad>, JointArray<f64>)` (位置 + 速度)
  - 轨迹点验证（关节限位检查）
  - 平滑的起止速度（柔性启停）
- 完整的单元测试:
  - 起点/终点精度测试
  - 轨迹平滑性测试
  - 零速度边界条件测试
  - Iterator 正确性测试
- 集成测试:
  - 轨迹执行完整性测试
  - 轨迹跟踪精度测试

**影响**:
- 工期: Phase 4 增加 1 天（7-8 天 → 8-9 天）
- 总工期: +1 天
- 质量: 提高（核心功能完整）

**文件变更**:
```diff
+ ### 任务 4.3: TrajectoryPlanner（轨迹规划器）⭐ NEW
+ - [ ] 实现 TrajectoryPlanner 结构
+ - [ ] 实现三次样条插值
+ - [ ] 实现 Iterator trait
+ - [ ] 单元测试和集成测试

  ### 任务 4.4: run_controller 辅助函数  // 原 4.3
  ...

  ### 任务 4.5: Phase 4 集成测试  // 原 4.4
+ - [ ] 轨迹规划器集成测试 ⭐ NEW
```

---

### 4. ⚠️ 增强任务 4.1：Controller Trait 文档说明

**位置**: Phase 4（Tick/Iterator + 控制器）

**问题**:
- `Controller` trait 的 `on_time_jump` 默认实现是 `Ok(())`（什么都不做）
- `run_controller` 中的逻辑: `if dt > max_dt { controller.on_time_jump(dt)?; dt = max_dt; }`
- 逻辑风险:
  - 如果 PID 控制器也使用默认空实现，只钳位了 `dt`
  - 但内部状态（如 `last_error`）未重置
  - `(error - last_error) / clamped_dt` 可能计算出巨大的导数
  - 导致输出突变，机械臂剧烈运动

**解决方案**:
- 大幅增强 `on_time_jump` 的文档注释:
  - 解释默认行为（什么都不做）
  - ⚠️ 强烈建议所有时间敏感的控制器覆盖此方法
  - 详细说明为什么需要（微分项爆炸风险）
  - 提供推荐做法（PID 示例）:
    - ✅ 重置微分项（`last_error = 0.0`）
    - ❌ 不清空积分项（保留抗重力补偿）
- 增强 `reset` 的文档注释:
  - ⚠️ 危险提示
  - 说明可能导致机械臂下坠
  - 说明适用场景

**影响**:
- 工期: 无变化
- 安全性: 提高（防止误用导致危险）

**文件变更**:
```diff
  pub trait Controller {
      /// 执行一次控制循环
+     ///
+     /// # 参数
+     /// - `dt`: 距离上次 tick 的时间间隔
+     ///
+     /// # 注意
+     /// `dt` 会被 `run_controller` 钳位到 `max_dt`，但控制器内部状态
+     /// 可能仍然包含大时间跨度的累积效应。
      fn tick(&mut self, dt: Duration) -> Result<(), Self::Error>;

      /// 处理时间跳变
+     ///
+     /// 当检测到 `dt > max_dt` 时，`run_controller` 会在钳位 `dt` 之前调用此方法。
+     ///
+     /// # 默认行为
+     /// 默认实现什么都不做（`Ok(())`），这适用于无状态或时间不敏感的控制器。
+     ///
+     /// # ⚠️ 重要提示
+     /// **强烈建议所有时间敏感的控制器（如 PID）覆盖此方法！**
+     ///
+     /// ## 为什么？
+     /// 即使 `dt` 被钳位，控制器内部状态（如微分项 `last_error`）仍然
+     /// 包含大时间跨度前的值。如果不重置，可能导致：
+     /// - **微分项爆炸**: `(error - last_error) / clamped_dt` 计算出巨大的导数
+     /// - **输出突变**: 控制量瞬间变化，导致机械臂剧烈运动
+     ///
+     /// ## 推荐做法（PID 示例）
+     /// ```rust
+     /// fn on_time_jump(&mut self, dt: Duration) -> Result<(), Self::Error> {
+     ///     // ✅ 重置微分项（防止微分噪声）
+     ///     self.last_error = 0.0;
+     ///     // ❌ 不清空积分项（保留抗重力补偿）
+     ///     log::warn!("Time jump detected: {:?}, D-term reset", dt);
+     ///     Ok(())
+     /// }
+     /// ```
      fn on_time_jump(&mut self, _dt: Duration) -> Result<(), Self::Error> {
          Ok(())
      }

      /// 完全重置控制器状态
+     ///
+     /// # ⚠️ 危险
+     /// 此方法会清空所有内部状态（包括积分项）。对于 PID 控制器，
+     /// 这意味着丢失抗重力补偿，可能导致机械臂突然下坠。
+     ///
+     /// **除非你明确知道自己在做什么，否则请使用 [`on_time_jump()`]。**
      fn reset(&mut self) -> Result<(), Self::Error> {
          Ok(())
      }
  }
```

---

## 📊 总体影响

### 工期变化

| 项目 | 原计划 | 修订后 | 变化 |
|------|--------|--------|------|
| Phase 1 | 5 天 | 6 天 | +1 天 |
| Phase 4 | 7-8 天 | 8-9 天 | +1 天 |
| **总工期** | **35 天** | **40 天** | **+5 天** |

### 里程碑调整

| 里程碑 | 原计划 | 修订后 | 变化 |
|--------|--------|--------|------|
| M1 (Phase 1) | Day 7 | Day 8 | +1 天 |
| M2 (Phase 2) | Day 15 | Day 16 | +1 天 |
| M3 (Phase 3) | Day 25 | Day 26 | +1 天 |
| M4 (Phase 4) | Day 33 | Day 35 | +2 天 |
| M5 (Phase 5) | Day 38 | Day 40 | +2 天 |
| M6 (Phase 6) | Day 41 | Day 43 | +2 天 |
| **Release** | **Day 42** | **Day 44** | **+2 天** |

### 质量提升

1. **功能完整性**:
   - ✅ 笛卡尔空间控制支持
   - ✅ 轨迹规划核心功能
   - ✅ 夹爪闭环控制完整

2. **安全性**:
   - ✅ Controller Trait 文档警告
   - ✅ 防止 PID 误用导致机械臂下坠
   - ✅ 明确时间跳变处理策略

3. **可维护性**:
   - ✅ 验收标准更明确
   - ✅ 实施注意事项完善
   - ✅ 防止关键功能遗漏

---

## 🎯 修订后的实施优先级

### 必须完成（P0）

1. ✅ 笛卡尔空间类型（任务 1.5）
2. ✅ Observer 夹爪反馈解析（任务 2.4）
3. ✅ TrajectoryPlanner 核心功能（任务 4.3）
4. ✅ Controller Trait 文档完善（任务 4.1）

### 可延后（P1）

- 五次样条插值（TrajectoryPlanner 扩展）
- 动态速度/加速度限制
- 更复杂的轨迹插值算法

### 可选（P2）

- 笛卡尔空间轨迹规划
- 力控制模式
- 多机械臂协同

---

## 📝 下一步行动

1. ✅ 审查修订后的实施清单
2. ✅ 确认工期和里程碑调整
3. ⏳ 开始 Phase 0（项目准备）
4. ⏳ 实施 Phase 1（包含新增的笛卡尔类型）

---

## ✅ 审查通过

**审查结果**: 实施清单已完善，可以开始执行。

**关键改进**:
- 补充了4个关键任务和说明
- 工期调整合理（+2 天）
- 质量和安全性显著提升
- 验收标准更加明确

**状态**: ✅ 就绪，可以开始实施

---

**文档版本**: v1.1
**修订日期**: 2026-01-23
**审查人**: User
**状态**: ✅ 已批准

