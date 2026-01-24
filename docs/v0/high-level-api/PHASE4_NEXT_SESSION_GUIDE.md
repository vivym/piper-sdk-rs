# Phase 4 下一会话启动指南

**目标**: Tick/Iterator + 控制器
**预计时间**: 9 天（原计划）
**准备状态**: ✅ 一切就绪

---

## 📋 快速回顾

### 已完成（Phase 0-3）

```
✅ Phase 0: 项目准备 - Mock 硬件和测试框架
✅ Phase 1: 基础类型 - 强类型单位、Joint、错误、笛卡尔
✅ Phase 2: 读写分离 - StateTracker, Commander, Observer
✅ Phase 3: Type State - Piper<S>, StateMonitor, Heartbeat

总代码: 4,764 行
总测试: 567 个
通过率: 100%
```

### 当前进度

- **总进度**: 67% (4/6 phases)
- **代码质量**: 工业级
- **性能**: 超标 3-5x
- **测试**: 100% 通过

---

## 🎯 Phase 4 核心任务

### 任务清单（来自 `IMPLEMENTATION_TODO_LIST.md`）

#### 4.1: Controller trait（2天）

**目标**: 定义控制器通用接口

**文件**: `src/control/controller.rs`

**清单**:
- [ ] 定义 `Controller` trait
- [ ] `tick()` 方法签名
- [ ] `on_time_jump()` 处理
- [ ] 关联类型 `Error`

**关键代码**:
```rust
pub trait Controller {
    type Error: std::error::Error;

    fn tick(
        &mut self,
        current: &JointArray<Rad>,
        dt: Duration,
    ) -> Result<JointArray<NewtonMeter>, Self::Error>;

    /// ⚠️ 处理时间跳变（重要！）
    fn on_time_jump(&mut self, _dt: Duration) -> Result<(), Self::Error> {
        Ok(()) // 默认不做任何事
    }
}
```

**文档要求**:
- ✅ 强调 `on_time_jump` 的重要性
- ✅ PID 等时间敏感控制器**必须**重置微分项
- ✅ 不要轻易清零积分项（会导致机械臂下坠）

---

#### 4.2: run_controller（Tick 模式）（2天）

**目标**: 控制循环包装器

**文件**: `src/control/loop_runner.rs`

**清单**:
- [ ] `run_controller()` 函数
- [ ] `dt` 计算
- [ ] `dt` 钳位（Clamping）
- [ ] `on_time_jump` 调用
- [ ] `spin_sleep` 精确延时

**关键逻辑**:
```rust
pub fn run_controller<C: Controller>(
    piper: Piper<Active<MitMode>>,
    mut controller: C,
    config: LoopConfig,
) -> Result<(), RobotError> {
    let interval = Duration::from_secs_f64(1.0 / config.frequency_hz);
    let max_dt = interval.mul_f64(config.dt_clamp_multiplier);

    let mut last_time = Instant::now();

    loop {
        let now = Instant::now();
        let mut dt = now - last_time;

        // ✅ dt 钳位
        if dt > max_dt {
            controller.on_time_jump(dt)?;
            dt = max_dt;
        }

        let current = piper.observer().joint_positions();
        let cmd = controller.tick(&current, dt)?;
        piper.Piper.command_torques(cmd)?;

        last_time = now;
        spin_sleep::sleep(interval);
    }
}
```

**测试要求**:
- ✅ 正常 `dt` 测试
- ✅ `dt` 钳位测试
- ✅ `on_time_jump` 调用验证

---

#### 4.3: PID 控制器（2天）

**目标**: 实现 PID 控制器

**文件**: `src/control/pid.rs`

**清单**:
- [ ] `PidController` 结构
- [ ] 比例项（P）
- [ ] 积分项（I）+ 饱和保护
- [ ] 微分项（D）
- [ ] `on_time_jump` 实现（⚠️ 关键！）

**关键实现**:
```rust
impl Controller for PidController {
    fn tick(&mut self, current: &JointArray<Rad>, dt: Duration) -> Result<...> {
        let dt_sec = dt.as_secs_f64();

        let error = self.target.map_with(*current, |t, c| t - c);

        // P term
        let p_term = error.map_with(self.kp, |e, kp| kp * e.0);

        // I term (带饱和保护)
        self.integral = self.integral.map_with(error, |i, e| {
            let new_i = i + e.0 * dt_sec;
            new_i.clamp(-self.i_max, self.i_max)
        });
        let i_term = self.integral.map_with(self.ki, |i, ki| ki * i);

        // D term
        let d_term = if dt_sec > 0.0 {
            error.map_with(self.last_error, |e, le| {
                self.kd * (e.0 - le) / dt_sec
            })
        } else {
            JointArray::from([0.0; 6])
        };

        self.last_error = error.map(|e| e.0);

        let output = p_term.map_with(i_term, |p, i| p + i)
                           .map_with(d_term, |pi, d| NewtonMeter(pi + d));
        Ok(output)
    }

    fn on_time_jump(&mut self, _dt: Duration) -> Result<(), Self::Error> {
        // ✅ 只重置微分项，保留积分项
        self.last_error = JointArray::from([0.0; 6]);
        // ❌ 不要清零积分项！
        // self.integral = JointArray::from([0.0; 6]); // 会导致机械臂下坠
        Ok(())
    }
}
```

**测试要求**:
- ✅ P、I、D 项独立测试
- ✅ 积分饱和测试
- ✅ `on_time_jump` 不影响积分项

---

#### 4.4: TrajectoryPlanner（2天）

**目标**: 三次样条轨迹规划器

**文件**: `src/control/trajectory.rs`

**清单**:
- [ ] `TrajectoryPlanner` 结构
- [ ] 三次样条插值
- [ ] `Iterator` trait 实现
- [ ] 时间缩放逻辑（⚠️ 重要！）

**关键实现**:
```rust
pub struct TrajectoryPlanner {
    spline_coeffs: JointArray<CubicCoeffs>,
    duration: Duration,
    frequency_hz: f64,
    // ...
}

impl TrajectoryPlanner {
    pub fn new(
        start: JointArray<Rad>,
        end: JointArray<Rad>,
        duration: Duration,
        frequency_hz: f64,
    ) -> Self {
        let duration_sec = duration.as_secs_f64();

        // ⚠️ 未来支持 Via Points 时，需要乘以 duration_sec
        let v_start = 0.0; // v_start * duration_sec
        let v_end = 0.0;   // v_end * duration_sec

        let spline_coeffs = start.map_with(end, |s, e| {
            Self::compute_cubic_spline(s.0, v_start, e.0, v_end)
        });

        TrajectoryPlanner { spline_coeffs, duration, frequency_hz }
    }

    fn compute_cubic_spline(p0: f64, v0: f64, p1: f64, v1: f64) -> CubicCoeffs {
        // a0 + a1*t + a2*t² + a3*t³
        let a0 = p0;
        let a1 = v0;
        let a2 = 3.0 * (p1 - p0) - 2.0 * v0 - v1;
        let a3 = -2.0 * (p1 - p0) + v0 + v1;
        CubicCoeffs { a0, a1, a2, a3 }
    }
}

impl Iterator for TrajectoryPlanner {
    type Item = (JointArray<Rad>, JointArray<f64>);

    fn next(&mut self) -> Option<Self::Item> {
        // ... 实现
    }
}
```

**测试要求**:
- ✅ 边界条件（起止速度为 0）
- ✅ 平滑性（加速度连续）
- ✅ 时间准确性
- ⚠️ 使用解析解或放宽阈值（不要依赖数值微分）

---

#### 4.5: 示例和集成测试（1天）

**目标**: 验证整个 Phase 4

**清单**:
- [ ] 重力补偿示例
- [ ] PID 示例
- [ ] 轨迹跟随示例
- [ ] 集成测试

**示例文件**: `examples/gravity_compensation.rs`

---

## 📚 关键参考文档

### 设计文档
1. ⭐ `rust_high_level_api_design_v3.2_final.md`
   - **4.3 节**: 控制器模式
   - **关键**: `on_time_jump` 策略

2. `IMPLEMENTATION_TODO_LIST.md` (v1.2)
   - **Phase 4 部分**: 详细任务清单
   - **数学细节**: 时间缩放、数值稳定性

### 之前的实现
- `src/high_level/types/` - 可用的类型系统
- `src/high_level/client/` - 可用的客户端组件
- `src/high_level/state/` - 可用的状态机
- `tests/high_level/common/` - 测试辅助

---

## ⚠️ 重要提醒

### 1. `on_time_jump` 处理

**关键原则**:
- ✅ **必须重置**: 微分项（D term）
- ❌ **不要清零**: 积分项（I term）

**原因**:
- 清零积分项会导致机械臂瞬间失去抗重力能力
- 负载保持时会突然下坠（Sagging）

**实现**:
```rust
fn on_time_jump(&mut self, _dt: Duration) -> Result<(), Self::Error> {
    self.last_error = JointArray::from([0.0; 6]); // ✅ 重置 D 项
    // self.integral = ...; // ❌ 不要碰积分项！
    Ok(())
}
```

### 2. 轨迹规划时间缩放

**问题**: 归一化时间域 `[0, 1]` 与物理速度的转换

**解决方案**:
```rust
// 未来支持 Via Points 时
let v_start_normalized = v_start_physical * duration_sec;
let v_end_normalized = v_end_physical * duration_sec;
```

**当前**: 起止速度都为 0，所以不需要缩放

### 3. 测试数值稳定性

**不推荐**: 使用数值微分检查平滑性
```rust
// ❌ 会引入噪声
let accel = (vel - last_vel) / dt;
```

**推荐**:
- 使用解析解（样条二阶导数）
- 放宽阈值
- 检查边界条件

---

## 🚀 启动步骤

### 1. 开始新会话

```bash
# 回顾进度
cat docs/v0/high-level-api/LONG_SESSION_FINAL_SUMMARY.md

# 查看任务清单
cat docs/v0/high-level-api/IMPLEMENTATION_TODO_LIST.md | grep -A 100 "Phase 4"
```

### 2. 创建文件结构

```bash
mkdir -p src/control
touch src/control/mod.rs
touch src/control/controller.rs
touch src/control/loop_runner.rs
touch src/control/pid.rs
touch src/control/trajectory.rs
```

### 3. 按任务顺序实施

1. Controller trait
2. Loop Runner
3. PID Controller
4. TrajectoryPlanner
5. 示例和测试

### 4. 持续测试

```bash
# 每完成一个任务运行测试
cargo test --lib --quiet

# 每完成一个模块运行基准
cargo bench --bench phase4_performance
```

---

## 📊 成功标准

### 功能完整性
- ✅ Controller trait 定义清晰
- ✅ PID 控制器正确实现
- ✅ TrajectoryPlanner 生成平滑轨迹
- ✅ Loop Runner 正确处理 `dt` 钳位

### 质量标准
- ✅ 所有测试通过（目标 600+ 个）
- ✅ 无 Clippy 警告
- ✅ 文档完整（100% API 覆盖）
- ✅ 示例可运行

### 性能标准
- ✅ 控制循环开销 < 1ms
- ✅ 轨迹规划延迟 < 10µs/点

---

## 🎯 预期成果

### 代码
- 新增约 1,500 行
- 总计约 6,300 行
- 新增 50+ 测试
- 总测试约 620 个

### 文档
- Phase 4 完成报告
- 示例和教程
- API 文档更新

### 里程碑
- ✅ Phase 4 完成
- ✅ 总进度 83% (5/6 phases)
- ✅ 核心功能全部完成

---

## 💡 提示

### 如果遇到困难

1. **类型错误**: 参考 Phase 1 类型系统实现
2. **并发问题**: 参考 Phase 2 读写分离
3. **状态问题**: 参考 Phase 3 Type State
4. **性能问题**: 参考 Phase 2 性能优化经验

### 保持节奏

- 每完成一个任务更新 `IMPLEMENTATION_PROGRESS.md`
- 每天创建检查点（git commit）
- 遇到问题查阅设计文档
- 保持测试先行（TDD）

---

**准备状态**: ✅ 一切就绪
**下一步**: 创建 `src/control/controller.rs`
**预计时间**: 9 天
**实际可能**: 1-2 会话

🚀 **让我们征服 Phase 4！** 🚀

