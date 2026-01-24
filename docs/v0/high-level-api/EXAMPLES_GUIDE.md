# 示例程序使用指南

本文档介绍 Piper SDK 高级 API 的示例程序。

---

## 📚 示例列表

### 1. 简单移动示例 (`high_level_simple_move.rs`)

**目标**: 快速入门，展示轨迹规划的基本使用

**运行**:
```bash
cargo run --example high_level_simple_move
```

**展示功能**:
- ✨ JointArray 强类型单位
- ✨ TrajectoryPlanner 基本使用
- ✨ Iterator 模式
- ✨ 进度跟踪

**输出示例**:
```
🚀 Piper SDK - Simple Move Example
===================================

📍 起始位置: Rad(0.0)
🎯 目标位置: Rad(0.5)

📈 轨迹规划:
   - 持续时间: 5s
   - 采样频率: 100 Hz
   - 总采样点: 500

▶️  执行轨迹...

   Step 20/500: 进度 4.0% | J1 位置: 0.002 rad | J1 速度: 0.022 rad/s
   ...
   Step 500/500: 进度 100.0% | J1 位置: 0.498 rad | J1 速度: 0.003 rad/s

✅ 轨迹执行完成！
```

**适合人群**: 初学者，想快速了解 API 基本用法

---

### 2. PID 控制示例 (`high_level_pid_control.rs`)

**目标**: 展示 PID 控制器的使用和配置

**运行**:
```bash
cargo run --example high_level_pid_control
```

**展示功能**:
- ✨ PidController Builder 模式
- ✨ 积分饱和保护
- ✨ 输出钳位
- ✨ on_time_jump 时间跳变处理
- ✨ reset 重置功能

**输出示例**:
```
🎯 Piper SDK - PID Control Example
===================================

🔧 PID 控制器配置:
   - Kp (比例增益): 10.0
   - Ki (积分增益): 0.5
   - Kd (微分增益): 0.1
   - 积分限制: 5.0
   - 输出限制: 50.0 Nm
   - 目标位置: Rad(1.0)

▶️  开始控制循环 (模拟)...

   Iter 0: 位置: 0.0020 | 误差: 0.9980 | 积分: 0.0100 | 输出: 20.01 Nm
   ...
   Iter 90: 位置: 0.0898 | 误差: 0.9102 | 积分: 0.8688 | 输出: 9.54 Nm

✅ 控制循环完成！
```

**关键学习点**:
1. **Builder 模式**: 链式配置参数
2. **安全保护**: 积分饱和和输出钳位
3. **时间处理**: `on_time_jump` 保留积分项防止下坠
4. **状态管理**: `reset` vs `on_time_jump` 的区别

**适合人群**: 需要实现控制算法的开发者

---

### 3. 轨迹规划演示 (`high_level_trajectory_demo.rs`)

**目标**: 深入展示轨迹规划器的高级特性

**运行**:
```bash
cargo run --example high_level_trajectory_demo
```

**展示功能**:
- ✨ 高频采样 (200Hz)
- ✨ 边界条件验证
- ✨ 平滑性分析
- ✨ 轨迹重置和重用
- ✨ 性能统计

**输出示例**:
```
📈 Piper SDK - Trajectory Planner Demo
======================================

🎯 轨迹配置:
   - 起点: J1=0.000 rad
   - 终点: J1=1.570 rad (90.0°)
   - 持续时间: 3s
   - 采样频率: 200 Hz
   - 总采样点: 600

▶️  执行轨迹...
   ...

✅ 轨迹执行完成！
   执行时间: 167.582µs
   总步数: 600
   平均每步: 279ns

🔍 边界条件验证:
   起点位置: 0.000000 rad (期望: 0.000000)
   终点位置: 1.570000 rad (期望: 1.570000)
   起点速度: 0.000000 rad/s (期望: 0)
   终点速度: 0.000000 rad/s (期望: 0)

   ✅ 起点误差: 0.00e0 rad
   ✅ 终点误差: 2.22e-16 rad

📊 平滑性分析:
   最大速度: 0.7850 rad/s
   速度方向变化次数: 0
   ✅ 轨迹单调平滑（方向变化 ≤ 2）
```

**关键学习点**:
1. **Iterator 高效**: O(1) 内存，按需生成
2. **数学精度**: 边界条件误差 < 1e-15
3. **平滑保证**: C² 连续，单调平滑
4. **性能优异**: 每步 < 300ns

**适合人群**: 需要高性能轨迹规划的开发者

---

## 🎓 学习路径

### 初学者路径

1. **第一步**: 运行 `high_level_simple_move.rs`
   - 理解基本 API 结构
   - 学习 JointArray 和 Rad 类型
   - 了解 Iterator 模式

2. **第二步**: 运行 `high_level_pid_control.rs`
   - 学习控制器配置
   - 理解 Builder 模式
   - 掌握安全保护机制

3. **第三步**: 运行 `high_level_trajectory_demo.rs`
   - 深入理解轨迹规划
   - 学习性能优化
   - 掌握高级特性

### 进阶开发者路径

1. **阅读设计文档**: `rust_high_level_api_design_v3.2_final.md`
2. **研究核心实现**: `src/high_level/` 源码
3. **运行性能基准**: `cargo bench`
4. **编写自定义控制器**: 实现 `Controller` trait

---

## 💡 常见使用模式

### 模式 1: 简单点对点移动

```rust
use piper_sdk::high_level::*;

// 1. 创建轨迹规划器
let planner = TrajectoryPlanner::new(
    start_positions,
    end_positions,
    Duration::from_secs(5),
    100.0,  // 100Hz
);

// 2. 执行轨迹
for (position, _velocity) in planner {
    piper.Piper.command_positions(position)?;
    thread::sleep(Duration::from_millis(10));
}
```

**适用场景**: 简单的点对点运动

---

### 模式 2: PID 位置控制

```rust
use piper_sdk::high_level::*;

// 1. 创建 PID 控制器
let mut pid = PidController::new(target_position)
    .with_gains(10.0, 0.5, 0.1)
    .with_integral_limit(5.0)
    .with_output_limit(50.0);

// 2. 控制循环
loop {
    let current = piper.observer().joint_positions();
    let output = pid.tick(&current, dt)?;
    piper.Piper.command_torques(output)?;
    thread::sleep(Duration::from_millis(10));
}
```

**适用场景**: 精确位置控制，抗干扰

---

### 模式 3: 自定义控制器

```rust
use piper_sdk::high_level::*;

struct MyController { /* ... */ }

impl Controller for MyController {
    type Error = RobotError;

    fn tick(&mut self, current: &JointArray<Rad>, dt: Duration)
        -> Result<JointArray<NewtonMeter>, Self::Error>
    {
        // 你的控制算法
        todo!()
    }

    fn on_time_jump(&mut self, dt: Duration) -> Result<(), Self::Error> {
        // 处理时间跳变
        Ok(())
    }
}

// 使用
let mut controller = MyController::new();
run_controller(observer, commander, controller, config)?;
```

**适用场景**: 需要自定义控制算法

---

## 🚀 性能提示

### 1. 轨迹规划性能

```rust
// ✅ 推荐: 使用 Iterator 模式（内存 O(1)）
for (pos, vel) in planner {
    // 按需生成，无内存分配
}

// ❌ 不推荐: 预先生成所有点（内存 O(n)）
let all_points: Vec<_> = planner.collect();
```

### 2. 控制循环频率

```rust
// ✅ 推荐: 使用 spin_sleep 低抖动
use spin_sleep::sleep;
sleep(Duration::from_millis(1));

// ❌ 不推荐: 标准 sleep 抖动大
std::thread::sleep(Duration::from_millis(1));
```

### 3. 状态读取优化

```rust
// ✅ 推荐: 一次读取多个状态
let positions = observer.joint_positions();
let velocities = observer.joint_velocities();

// ⚠️  可接受: 多次读取（有小开销）
for joint in Joint::all() {
    let pos = observer.joint_positions()[joint];
}
```

---

## 🛡️ 安全提示

### 1. PID 积分项保护

```rust
// ✅ 正确: 使用积分饱和保护
let pid = PidController::new(target)
    .with_integral_limit(5.0);  // 限制积分项

// ❌ 危险: 无积分保护可能导致 Integral Windup
```

### 2. 输出力矩限制

```rust
// ✅ 正确: 限制输出力矩
let pid = PidController::new(target)
    .with_output_limit(50.0);  // 限制输出到 50 Nm

// ❌ 危险: 无输出限制可能损坏硬件
```

### 3. 时间跳变处理

```rust
// ✅ 正确: 覆盖 on_time_jump
impl Controller for MyPID {
    fn on_time_jump(&mut self, _dt: Duration) -> Result<(), Self::Error> {
        self.last_error = JointArray::from([0.0; 6]);  // 重置 D 项
        // ⚠️ 不重置 integral，防止机械臂下坠
        Ok(())
    }
}
```

---

## 📖 延伸阅读

- **设计文档**: `docs/v0/high-level-api/rust_high_level_api_design_v3.2_final.md`
- **实施清单**: `docs/v0/high-level-api/IMPLEMENTATION_TODO_LIST.md`
- **API 文档**: `cargo doc --open`
- **性能基准**: `cargo bench`
- **测试覆盖**: `cargo test`

---

## 🤝 贡献示例

如果你创建了新的示例程序，欢迎贡献！

**要求**:
1. 清晰的注释和文档
2. 展示特定的使用场景
3. 包含错误处理
4. 性能考虑
5. 安全最佳实践

**提交位置**: `examples/high_level_*.rs`

---

**最后更新**: 2026-01-23
**版本**: v1.0-alpha

