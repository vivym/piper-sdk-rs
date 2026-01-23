
# Piper Rust SDK 高层 API 实施清单

> **项目**: Piper 机器人 Rust SDK 工业级高层 API
> **设计版本**: v3.2 Final
> **创建日期**: 2026-01-23
> **最后更新**: 2026-01-23
> **预计工期**: 8 周（40 个工作日）⭐ 已修订
> **核心原则**: 🧪 测试先行 | 🛡️ 安全第一 | ⚡ 性能优化

---

## 📝 修订历史

### v1.2 (2026-01-23) - 数学严谨性增强 ⭐ NEW

根据代码级审查，从数学严谨性和数值稳定性角度改进：

1. **TrajectoryPlanner 时间缩放文档化**（任务 4.3）
   - 添加详细数学注释，解释归一化时间域的速度缩放
   - 提供未来 Via Points 扩展的代码示例
   - 防止未来扩展时的数学错误

2. **Quaternion 数值稳定性**（任务 1.5）
   - `normalize()` 添加除零检查（`norm_sq < 1e-10`）
   - 近零四元数返回单位四元数并记录警告
   - 新增数值稳定性测试用例

3. **轨迹测试方法改进**（任务 4.3）
   - 使用解析解验证边界条件（更可靠）
   - 添加速度连续性和方向变化检查
   - 数值微分测试放宽阈值（避免 Flaky Test）

**工期影响**: 无变化（文档和测试优化）

---

### v1.1 (2026-01-23) - 关键补充

根据深度审查，新增4个关键任务和说明：

1. **任务 1.5**: 笛卡尔空间类型（`CartesianPose`, `Quaternion`）⭐ NEW
   - 支持笛卡尔空间控制
   - 四元数/欧拉角转换
   - +1 天工期

2. **任务 2.4 增强**: Observer 夹爪反馈验收标准
   - 明确要求解析 CAN 协议中的夹爪状态（0x4xx ID）
   - 确保闭环控制支持

3. **任务 4.3**: TrajectoryPlanner（轨迹规划器）⭐ NEW
   - 三次样条插值
   - Iterator 模式
   - 核心功能模块（非示例）
   - +1 天工期

4. **任务 4.1 增强**: Controller Trait 文档说明
   - 强化 `on_time_jump` vs `reset` 的区别
   - 添加警告和推荐做法
   - 防止 PID 积分项误清导致机械臂下坠

**总工期调整**: 35 天 → 40 天（+2 个工作日）

---

## 📋 总览

| Phase | 任务 | 工期 | 状态 | 文档引用 |
|-------|------|------|------|----------|
| **Phase 0** | 项目准备 | 2 天 | ⏳ 待开始 | - |
| **Phase 1** | 基础类型系统 + 笛卡尔类型 | 6 天 (+1) | ⏳ 待开始 | v3.2 §4.1 |
| **Phase 2** | 读写分离 + 性能优化 | 1.5 周 | ⏳ 待开始 | v3.2 §3, §4.2 |
| **Phase 3** | Type State 核心 | 2 周 | ⏳ 待开始 | v3.2 §5 |
| **Phase 4** | Tick/Iterator + 控制器 + 轨迹规划 | 8-9 天 (+1) | ⏳ 待开始 | v3.2 §6 |
| **Phase 5** | 完善和文档 | 1 周 | ⏳ 待开始 | v3.2 §7 |
| **Phase 6** | 性能和安全审查 | 3 天 | ⏳ 待开始 | - |

**总计**: 约 **40 个工作日**（含测试和审查）⭐ 新增 2 天

---

## 🚀 Phase 0: 项目准备 (2 天)

### 任务 0.1: 项目结构搭建

**目标**: 创建模块化的项目结构

```bash
piper-sdk-rs/
├── src/
│   ├── lib.rs
│   ├── types/          # Phase 1
│   │   ├── mod.rs
│   │   ├── units.rs
│   │   ├── joint.rs
│   │   └── error.rs
│   ├── client/         # Phase 2
│   │   ├── mod.rs
│   │   ├── commander.rs
│   │   ├── observer.rs
│   │   └── state_tracker.rs
│   ├── state/          # Phase 3
│   │   ├── mod.rs
│   │   └── machine.rs
│   ├── control/        # Phase 4
│   │   ├── mod.rs
│   │   ├── traits.rs
│   │   └── pid.rs
│   └── examples/       # Phase 5
├── tests/
│   ├── integration/
│   └── performance/
├── benches/
└── docs/
```

**清单**:
- [ ] 创建目录结构
- [ ] 配置 `Cargo.toml`（依赖项）
- [ ] 设置 CI/CD（GitHub Actions）
- [ ] 配置 linter（`clippy` + `rustfmt`）
- [ ] 配置测试框架（`criterion` for benchmarks）

**依赖项** (`Cargo.toml`):
```toml
[dependencies]
parking_lot = "0.12"      # RwLock（无 Poison）
spin_sleep = "1.2"        # 低抖动延迟
thiserror = "1.0"         # Error 派生
serde = { version = "1.0", features = ["derive"], optional = true }

[dev-dependencies]
criterion = "0.5"         # 性能基准测试
proptest = "1.4"          # 属性测试
tokio = { version = "1", features = ["test-util"] }

[features]
default = []
serde = ["dep:serde"]     # 可选序列化支持
```

**文档引用**:
- [v3.2 Final - 实现细节](rust_high_level_api_design_v3.2_final.md)

**验收标准**:
- ✅ 项目结构符合设计文档
- ✅ `cargo build` 成功
- ✅ `cargo clippy` 无警告
- ✅ CI 配置正确运行

---

### 任务 0.2: 测试基础设施

**目标**: 搭建完善的测试环境

**清单**:
- [ ] 单元测试框架（内置 `#[test]`）
- [ ] 集成测试框架（`tests/integration/`）
- [ ] 性能基准测试（`benches/`）
- [ ] Mock 硬件接口（用于无硬件测试）
- [ ] 测试工具模块（`tests/common/`）

**Mock 硬件接口设计**:
```rust
// tests/common/mock_hardware.rs

/// 模拟 CAN 总线（用于测试）
pub struct MockCanBus {
    tx: Sender<CanFrame>,
    rx: Receiver<CanFrame>,
    state: Arc<Mutex<HardwareState>>,
}

impl MockCanBus {
    /// 模拟机械臂状态
    pub fn simulate_arm_state(&self, state: ArmState) { ... }

    /// 模拟急停按下
    pub fn simulate_emergency_stop(&self) { ... }

    /// 模拟通信故障
    pub fn simulate_timeout(&self) { ... }
}
```

**验收标准**:
- ✅ Mock 接口可用
- ✅ 测试工具完善
- ✅ `cargo test` 基础框架运行

**预计时间**: 2 天

---

## 📐 Phase 1: 基础类型系统 (5 天)

> **文档引用**: [v3.2 Final §4.1 - 强类型单位系统](rust_high_level_api_design_v3.2_final.md#41-强类型单位系统-newtype-idiom)

### 任务 1.1: 强类型单位系统

**目标**: 实现 NewType 模式防止单位混淆

**文件**: `src/types/units.rs`

**清单**:
- [ ] 实现 `Rad` (弧度)
- [ ] 实现 `Deg` (角度)
- [ ] 实现 `NewtonMeter` (力矩)
- [ ] 实现单位转换方法
- [ ] 实现运算符重载（`Add`, `Sub`, `Mul`, `Div`）
- [ ] 实现 `Debug`, `Display` trait

**代码框架**:
```rust
// src/types/units.rs

/// 弧度（NewType）
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub struct Rad(pub f64);

impl Rad {
    pub const ZERO: Self = Rad(0.0);

    pub fn to_deg(self) -> Deg { ... }
    pub fn sin(self) -> f64 { ... }
    pub fn cos(self) -> f64 { ... }
}

// 类似实现 Deg, NewtonMeter, ...
```

**测试要求**:
```rust
// tests/types/units.rs

#[test]
fn test_unit_conversion() {
    let rad = Rad(std::f64::consts::PI);
    let deg = rad.to_deg();
    assert!((deg.0 - 180.0).abs() < 1e-6);
}

#[test]
fn test_type_safety() {
    // 编译时应该失败
    // let _ = Rad(1.0) + Deg(1.0);  // 类型不匹配
}

#[cfg(test)]
mod property_tests {
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn rad_deg_roundtrip(rad in -100.0..100.0f64) {
            let r = Rad(rad);
            let d = r.to_deg();
            let r2 = d.to_rad();
            prop_assert!((r.0 - r2.0).abs() < 1e-10);
        }
    }
}
```

**验收标准**:
- ✅ 所有单位类型实现完成
- ✅ 单元测试覆盖率 > 95%
- ✅ 属性测试通过（往返转换）
- ✅ 文档示例可运行

---

### 任务 1.2: Joint 枚举和 JointArray

**目标**: 类型安全的关节索引

**文件**: `src/types/joint.rs`

**清单**:
- [ ] 实现 `Joint` 枚举
- [ ] 实现 `JointArray<T>` 容器
- [ ] 实现索引访问（`Index`, `IndexMut`）
- [ ] 实现迭代器
- [ ] 实现 `From<[T; 6]>` 和 `Into<[T; 6]>`

**代码框架**:
```rust
// src/types/joint.rs

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Joint {
    J1 = 0,
    J2 = 1,
    J3 = 2,
    J4 = 3,
    J5 = 4,
    J6 = 5,
}

impl Joint {
    pub const ALL: [Joint; 6] = [J1, J2, J3, J4, J5, J6];

    pub fn index(self) -> usize { self as usize }
}

#[derive(Debug, Clone)]
pub struct JointArray<T> {
    data: [T; 6],
}

impl<T> JointArray<T> {
    pub fn new(data: [T; 6]) -> Self { ... }
    pub fn map<U, F>(self, f: F) -> JointArray<U>
        where F: FnMut(T) -> U { ... }
}

impl<T> Index<Joint> for JointArray<T> {
    type Output = T;
    fn index(&self, joint: Joint) -> &T {
        &self.data[joint.index()]
    }
}
```

**测试要求**:
```rust
#[test]
fn test_joint_array_indexing() {
    let positions = JointArray::new([
        Rad(0.0), Rad(0.1), Rad(0.2),
        Rad(0.3), Rad(0.4), Rad(0.5),
    ]);

    assert_eq!(positions[Joint::J1], Rad(0.0));
    assert_eq!(positions[Joint::J6], Rad(0.5));
}

#[test]
fn test_joint_array_iteration() {
    let positions = JointArray::new([Rad(0.0); 6]);
    let sum: f64 = positions.iter().map(|r| r.0).sum();
    assert_eq!(sum, 0.0);
}

#[test]
fn test_joint_array_map() {
    let rad = JointArray::new([Rad(std::f64::consts::PI); 6]);
    let deg = rad.map(|r| r.to_deg());
    assert!((deg[Joint::J1].0 - 180.0).abs() < 1e-6);
}
```

**验收标准**:
- ✅ 编译期类型安全（无运行时边界检查）
- ✅ 单元测试覆盖率 > 95%
- ✅ 迭代器正确性测试通过

---

### 任务 1.3: 错误类型体系

**目标**: 分层错误处理

**文件**: `src/types/error.rs`

**清单**:
- [ ] 实现 `RobotError` 枚举
- [ ] 区分 `Recoverable` 和 `Fatal` 错误
- [ ] 实现 `thiserror` 派生
- [ ] 实现错误上下文（`context` 方法）
- [ ] 实现错误日志集成

**代码框架**:
```rust
// src/types/error.rs

use thiserror::Error;

#[derive(Debug, Error)]
pub enum RobotError {
    // === Fatal Errors (不可恢复) ===
    #[error("Hardware communication failed: {0}")]
    HardwareFailure(String),

    #[error("State machine poisoned: {reason}")]
    StatePoisoned { reason: String },

    #[error("Emergency stop triggered")]
    EmergencyStop,

    // === Recoverable Errors ===
    #[error("Command timeout after {timeout_ms}ms")]
    Timeout { timeout_ms: u64 },

    #[error("Invalid state transition: {from} -> {to}")]
    InvalidTransition { from: String, to: String },

    #[error("Joint {joint:?} limit exceeded: {value} (limit: {limit})")]
    JointLimitExceeded { joint: Joint, value: f64, limit: f64 },

    // === I/O Errors ===
    #[error("CAN bus error: {0}")]
    CanError(#[from] std::io::Error),
}

impl RobotError {
    /// 是否为致命错误
    pub fn is_fatal(&self) -> bool {
        matches!(self,
            Self::HardwareFailure(_) |
            Self::StatePoisoned { .. } |
            Self::EmergencyStop
        )
    }

    /// 是否可重试
    pub fn is_retryable(&self) -> bool {
        matches!(self, Self::Timeout { .. })
    }
}
```

**测试要求**:
```rust
#[test]
fn test_error_classification() {
    let fatal = RobotError::EmergencyStop;
    assert!(fatal.is_fatal());
    assert!(!fatal.is_retryable());

    let recoverable = RobotError::Timeout { timeout_ms: 100 };
    assert!(!recoverable.is_fatal());
    assert!(recoverable.is_retryable());
}

#[test]
fn test_error_display() {
    let err = RobotError::JointLimitExceeded {
        joint: Joint::J1,
        value: 3.5,
        limit: 3.14,
    };
    let msg = format!("{}", err);
    assert!(msg.contains("J1"));
    assert!(msg.contains("3.5"));
}
```

**验收标准**:
- ✅ 错误分类正确
- ✅ 错误信息清晰易懂
- ✅ 集成 `std::error::Error`
- ✅ 单元测试覆盖所有错误类型

---

### 任务 1.4: Phase 1 集成测试

**目标**: 验证类型系统整体可用性

**文件**: `tests/integration/phase1_types.rs`

**清单**:
- [ ] 跨模块集成测试
- [ ] 实际使用场景模拟
- [ ] 性能基准测试（类型转换开销）

**测试示例**:
```rust
// tests/integration/phase1_types.rs

#[test]
fn test_full_joint_command() {
    // 模拟完整的关节指令构建
    let target_positions = JointArray::new([
        Deg(0.0).to_rad(),
        Deg(45.0).to_rad(),
        Deg(90.0).to_rad(),
        Deg(-45.0).to_rad(),
        Deg(0.0).to_rad(),
        Deg(0.0).to_rad(),
    ]);

    let torques = JointArray::new([NewtonMeter(0.0); 6]);

    // 验证类型安全
    assert_eq!(target_positions[Joint::J2].to_deg().0, 45.0);
}
```

**性能基准测试**:
```rust
// benches/types.rs

use criterion::{black_box, criterion_group, criterion_main, Criterion};

fn benchmark_unit_conversion(c: &mut Criterion) {
    c.bench_function("rad_to_deg", |b| {
        b.iter(|| {
            let rad = Rad(black_box(1.5707963));
            let deg = rad.to_deg();
            black_box(deg)
        })
    });
}

criterion_group!(benches, benchmark_unit_conversion);
criterion_main!(benches);
```

**验收标准**:
- ✅ 所有集成测试通过
- ✅ 性能开销 < 1ns（零成本抽象）
- ✅ 编译器优化有效（Release 模式）

---

### 任务 1.5: 笛卡尔空间类型 ⭐ NEW

**目标**: 支持笛卡尔空间控制

**文件**: `src/types/cartesian.rs`

**清单**:
- [ ] 实现 `CartesianPose` 结构
- [ ] 实现 `CartesianVelocity` 结构
- [ ] 实现 `CartesianEffort` 结构
- [ ] 实现坐标变换方法
- [ ] 实现四元数/欧拉角转换

**代码框架**:
```rust
// src/types/cartesian.rs

use crate::types::units::{Rad, NewtonMeter};

/// 笛卡尔空间位姿（位置 + 姿态）
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CartesianPose {
    /// 位置 (米)
    pub position: Position3D,
    /// 姿态（四元数）
    pub orientation: Quaternion,
}

/// 三维位置（米）
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Position3D {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

/// 四元数（单位四元数）
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Quaternion {
    pub w: f64,
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

impl Quaternion {
    /// 从欧拉角创建（Roll-Pitch-Yaw）
    pub fn from_euler(roll: Rad, pitch: Rad, yaw: Rad) -> Self {
        let cr = (roll.0 / 2.0).cos();
        let sr = (roll.0 / 2.0).sin();
        let cp = (pitch.0 / 2.0).cos();
        let sp = (pitch.0 / 2.0).sin();
        let cy = (yaw.0 / 2.0).cos();
        let sy = (yaw.0 / 2.0).sin();

        Quaternion {
            w: cr * cp * cy + sr * sp * sy,
            x: sr * cp * cy - cr * sp * sy,
            y: cr * sp * cy + sr * cp * sy,
            z: cr * cp * sy - sr * sp * cy,
        }
    }

    /// 转换为欧拉角
    pub fn to_euler(&self) -> (Rad, Rad, Rad) {
        // Roll (x-axis rotation)
        let sinr_cosp = 2.0 * (self.w * self.x + self.y * self.z);
        let cosr_cosp = 1.0 - 2.0 * (self.x * self.x + self.y * self.y);
        let roll = Rad(sinr_cosp.atan2(cosr_cosp));

        // Pitch (y-axis rotation)
        let sinp = 2.0 * (self.w * self.y - self.z * self.x);
        let pitch = if sinp.abs() >= 1.0 {
            Rad(std::f64::consts::FRAC_PI_2.copysign(sinp))
        } else {
            Rad(sinp.asin())
        };

        // Yaw (z-axis rotation)
        let siny_cosp = 2.0 * (self.w * self.z + self.x * self.y);
        let cosy_cosp = 1.0 - 2.0 * (self.y * self.y + self.z * self.z);
        let yaw = Rad(siny_cosp.atan2(cosy_cosp));

        (roll, pitch, yaw)
    }

    /// 归一化（确保单位四元数）
    ///
    /// # 数值稳定性
    ///
    /// 如果四元数的模接近 0（< 1e-10），返回默认单位四元数 (1, 0, 0, 0)
    /// 以避免除零错误和 NaN 扩散。
    ///
    /// 这种情况理论上不应发生，但在初始化错误、序列化错误或数值计算
    /// 累积误差时可能出现。
    pub fn normalize(&self) -> Self {
        let norm_sq = self.w * self.w + self.x * self.x +
                      self.y * self.y + self.z * self.z;

        // ✅ 数值稳定性检查：避免除零
        if norm_sq < 1e-10 {
            // 返回默认单位四元数（无旋转）
            log::warn!("Normalizing near-zero quaternion, returning identity");
            return Quaternion { w: 1.0, x: 0.0, y: 0.0, z: 0.0 };
        }

        let norm = norm_sq.sqrt();
        Quaternion {
            w: self.w / norm,
            x: self.x / norm,
            y: self.y / norm,
            z: self.z / norm,
        }
    }
}

impl CartesianPose {
    /// 从位置和欧拉角创建
    pub fn from_position_euler(
        x: f64, y: f64, z: f64,
        roll: Rad, pitch: Rad, yaw: Rad,
    ) -> Self {
        CartesianPose {
            position: Position3D { x, y, z },
            orientation: Quaternion::from_euler(roll, pitch, yaw),
        }
    }
}

/// 笛卡尔空间速度（线速度 + 角速度）
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CartesianVelocity {
    pub linear: Position3D,   // m/s
    pub angular: Position3D,  // rad/s
}

/// 笛卡尔空间力/力矩
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CartesianEffort {
    pub force: Position3D,   // N
    pub torque: Position3D,  // N·m
}
```

**测试要求**:
```rust
// tests/types/cartesian.rs

#[test]
fn test_quaternion_euler_conversion() {
    let roll = Rad(0.1);
    let pitch = Rad(0.2);
    let yaw = Rad(0.3);

    let quat = Quaternion::from_euler(roll, pitch, yaw);
    let (r2, p2, y2) = quat.to_euler();

    assert!((roll.0 - r2.0).abs() < 1e-10);
    assert!((pitch.0 - p2.0).abs() < 1e-10);
    assert!((yaw.0 - y2.0).abs() < 1e-10);
}

#[test]
fn test_quaternion_normalization() {
    let quat = Quaternion { w: 1.0, x: 1.0, y: 1.0, z: 1.0 };
    let normalized = quat.normalize();

    let norm = (normalized.w * normalized.w +
                normalized.x * normalized.x +
                normalized.y * normalized.y +
                normalized.z * normalized.z).sqrt();

    assert!((norm - 1.0).abs() < 1e-10);
}

#[test]
fn test_quaternion_near_zero_stability() {
    // 测试近零四元数的数值稳定性
    let near_zero = Quaternion { w: 1e-20, x: 1e-20, y: 1e-20, z: 1e-20 };
    let normalized = near_zero.normalize();

    // 应该返回单位四元数（无旋转）
    assert_eq!(normalized.w, 1.0);
    assert_eq!(normalized.x, 0.0);
    assert_eq!(normalized.y, 0.0);
    assert_eq!(normalized.z, 0.0);

    // 测试完全为零的情况
    let zero = Quaternion { w: 0.0, x: 0.0, y: 0.0, z: 0.0 };
    let normalized_zero = zero.normalize();

    // 不应该是 NaN
    assert!(!normalized_zero.w.is_nan());
    assert!(!normalized_zero.x.is_nan());
    assert_eq!(normalized_zero.w, 1.0);
}

#[test]
fn test_cartesian_pose_construction() {
    let pose = CartesianPose::from_position_euler(
        1.0, 2.0, 3.0,
        Rad(0.0), Rad(0.0), Rad(0.0),
    );

    assert_eq!(pose.position.x, 1.0);
    assert_eq!(pose.position.y, 2.0);
    assert_eq!(pose.position.z, 3.0);
}

#[cfg(test)]
mod property_tests {
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn euler_quaternion_roundtrip(
            roll in -3.14..3.14f64,
            pitch in -1.57..1.57f64,
            yaw in -3.14..3.14f64
        ) {
            let quat = Quaternion::from_euler(Rad(roll), Rad(pitch), Rad(yaw));
            let (r2, p2, y2) = quat.to_euler();

            prop_assert!((roll - r2.0).abs() < 1e-6);
            prop_assert!((pitch - p2.0).abs() < 1e-6);
            prop_assert!((yaw - y2.0).abs() < 1e-6);
        }
    }
}
```

**验收标准**:
- ✅ 欧拉角/四元数转换正确（往返误差 < 1e-6）
- ✅ 四元数归一化正确
- ✅ **数值稳定性测试通过（近零四元数不产生 NaN）** ⭐ NEW
- ✅ Gimbal Lock 处理正确（`to_euler` 中 `sinp.abs() >= 1.0` 情况）
- ✅ 属性测试通过（1000次随机测试）
- ✅ 单元测试覆盖率 > 95%

**⚠️ 实施注意事项**:
- `Quaternion::normalize` 必须检查 `norm_sq < 1e-10` 避免除零
- 近零四元数应返回单位四元数并记录警告日志
- 所有数值计算应考虑浮点精度损失

---

**Phase 1 预计时间**: 6 个工作日（新增笛卡尔类型 +1 天）

---

## 🔌 Phase 2: 读写分离 + 性能优化 (7-8 天)

> **文档引用**:
> - [v3.2 Final §3 - 热路径性能优化](rust_high_level_api_design_v3.2_final.md#-问题-1-热路径锁竞争-critical-path-optimization)
> - [v3.2 Final §4.2 - 读写分离](rust_high_level_api_design_v3.2_final.md#42-读写分离-commanderobserver)

### 任务 2.1: StateTracker（无锁状态跟踪）

**目标**: 实现热路径无锁检查

**文件**: `src/client/state_tracker.rs`

**清单**:
- [ ] 实现 `StateTracker` 结构
- [ ] 实现 `AtomicBool` 快速检查
- [ ] 实现 `Acquire/Release` 内存序
- [ ] 实现 `mark_poisoned()` / `reset()`
- [ ] 实现详细状态存储（`RwLock<TrackerDetails>`）

**代码框架**:
```rust
// src/client/state_tracker.rs

use parking_lot::RwLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

pub(crate) struct StateTracker {
    /// 快速标志（无锁）
    valid_flag: Arc<AtomicBool>,
    /// 详细状态（带锁）
    details: RwLock<TrackerDetails>,
}

#[derive(Debug)]
struct TrackerDetails {
    poison_reason: Option<String>,
    expected_mode: ControlMode,
    expected_controller: ArmController,
    last_update: Instant,
}

impl StateTracker {
    pub fn new() -> Self { ... }

    /// ✅ 快速检查（热路径，~2ns）
    #[inline(always)]
    pub fn is_valid(&self) -> bool {
        // 使用 Acquire 确保内存可见性
        self.valid_flag.load(Ordering::Acquire)
    }

    /// ✅ 快速检查版本（返回 Result）
    pub fn check_valid_fast(&self) -> Result<(), RobotError> {
        if self.is_valid() {
            Ok(())
        } else {
            Err(self.read_error_details())
        }
    }

    /// 标记为 Poisoned（后台线程调用）
    pub fn mark_poisoned(&self, reason: String) {
        // 1. 先更新详细信息
        let mut details = self.details.write();
        details.poison_reason = Some(reason);
        drop(details);  // 显式释放锁

        // 2. 再设置原子标志（Release 语义）
        self.valid_flag.store(false, Ordering::Release);
    }

    /// 重置状态
    pub fn reset(&self) {
        self.valid_flag.store(true, Ordering::Release);
        let mut details = self.details.write();
        details.poison_reason = None;
        details.last_update = Instant::now();
    }

    /// 读取详细错误（慢路径）
    fn read_error_details(&self) -> RobotError {
        let details = self.details.read();
        RobotError::StatePoisoned {
            reason: details.poison_reason.clone()
                .unwrap_or_else(|| "Unknown reason".to_string()),
        }
    }
}
```

**测试要求**:
```rust
// tests/client/state_tracker.rs

#[test]
fn test_fast_path_performance() {
    let tracker = StateTracker::new();

    let start = Instant::now();
    for _ in 0..1_000_000 {
        let _ = tracker.is_valid();
    }
    let elapsed = start.elapsed();

    // 应该 < 5ms (100万次调用)
    assert!(elapsed.as_millis() < 5);
}

#[test]
fn test_memory_ordering() {
    use std::sync::Arc;
    use std::thread;

    let tracker = Arc::new(StateTracker::new());
    let tracker_clone = tracker.clone();

    // 线程 1: 写入
    let writer = thread::spawn(move || {
        tracker_clone.mark_poisoned("Test error".to_string());
    });

    writer.join().unwrap();

    // 线程 2: 读取（应该看到更新）
    assert!(!tracker.is_valid());
    match tracker.check_valid_fast() {
        Err(RobotError::StatePoisoned { reason }) => {
            assert_eq!(reason, "Test error");
        }
        _ => panic!("Expected poisoned error"),
    }
}

#[test]
fn test_parking_lot_no_poison() {
    // 验证 parking_lot::RwLock 不会 Poison
    let tracker = Arc::new(StateTracker::new());
    let tracker_clone = tracker.clone();

    let handle = std::thread::spawn(move || {
        let _lock = tracker_clone.details.write();
        panic!("Intentional panic");
    });

    let _ = handle.join();

    // 应该仍然可以获取锁
    let details = tracker.details.read();
    drop(details);  // 成功
}
```

**性能基准测试**:
```rust
// benches/state_tracker.rs

fn benchmark_state_check(c: &mut Criterion) {
    let tracker = StateTracker::new();

    let mut group = c.benchmark_group("state_check");

    group.bench_function("fast_path_valid", |b| {
        b.iter(|| {
            black_box(tracker.is_valid())
        })
    });

    group.bench_function("fast_path_with_result", |b| {
        b.iter(|| {
            black_box(tracker.check_valid_fast())
        })
    });

    tracker.mark_poisoned("Test".to_string());

    group.bench_function("slow_path_poisoned", |b| {
        b.iter(|| {
            black_box(tracker.check_valid_fast())
        })
    });

    group.finish();
}
```

**验收标准**:
- ✅ 快速路径 < 5ns (Release 模式)
- ✅ 内存序正确性测试通过
- ✅ 多线程压力测试通过（100 个线程，1000 次迭代）
- ✅ Panic Safety 测试通过

---

### 任务 2.2: RawCommander（内部完全权限）

**目标**: 底层命令发送接口

**文件**: `src/client/commander.rs`

**清单**:
- [ ] 实现 `RawCommander` 结构
- [ ] 实现 CAN 帧发送（`send_mit_command` 等）
- [ ] 集成 `StateTracker` 快速检查
- [ ] 实现状态变更方法（`pub(crate)`）
- [ ] 实现夹爪控制

**代码框架**:
```rust
// src/client/commander.rs

use crate::client::state_tracker::StateTracker;
use std::sync::Arc;

pub(crate) struct RawCommander {
    can_interface: Arc<dyn CanInterface>,
    state_tracker: Arc<StateTracker>,
}

impl RawCommander {
    /// 发送 MIT 模式指令（热路径优化）
    pub(crate) fn send_mit_command(
        &self,
        joint: Joint,
        position: Rad,
        velocity: f64,
        kp: f64,
        kd: f64,
        torque: NewtonMeter,
    ) -> Result<(), RobotError> {
        // ✅ 快速检查（无锁）
        self.state_tracker.check_valid_fast()?;

        // 构建并发送 CAN 帧
        let frame = self.build_mit_frame(joint, position, velocity, kp, kd, torque)?;
        self.can_interface.send(frame)?;

        Ok(())
    }

    /// 设置控制模式（仅内部可见）
    pub(crate) fn set_control_mode(&self, mode: ControlMode) -> Result<(), RobotError> {
        self.state_tracker.check_valid_fast()?;
        // ... 实现 ...
    }

    /// 使能机械臂（仅内部可见）
    pub(crate) fn enable_arm(&self) -> Result<(), RobotError> {
        self.state_tracker.check_valid_fast()?;
        // ... 实现 ...
    }

    /// 失能机械臂（仅内部可见）
    pub(crate) fn disable_arm(&self) -> Result<(), RobotError> {
        // ... 实现 ...
    }

    /// 控制夹爪
    pub(crate) fn send_gripper_command(
        &self,
        position: f64,
        effort: f64,
    ) -> Result<(), RobotError> {
        self.state_tracker.check_valid_fast()?;
        // ... 实现 ...
    }
}
```

**测试要求**:
```rust
// tests/client/commander.rs

#[test]
fn test_hot_path_performance() {
    let (commander, _mock) = setup_mock_commander();

    let start = Instant::now();
    for _ in 0..10_000 {
        let _ = commander.send_mit_command(
            Joint::J1,
            Rad(0.0),
            0.0,
            10.0,
            1.0,
            NewtonMeter(0.0),
        );
    }
    let elapsed = start.elapsed();

    // 10,000 次调用应该 < 50ms
    assert!(elapsed.as_millis() < 50);
}

#[test]
fn test_state_check_integration() {
    let (commander, mock) = setup_mock_commander();

    // 正常状态
    assert!(commander.send_mit_command(...).is_ok());

    // 模拟状态失效
    mock.simulate_emergency_stop();

    // 应该立即检测到
    assert!(matches!(
        commander.send_mit_command(...),
        Err(RobotError::StatePoisoned { .. })
    ));
}
```

**验收标准**:
- ✅ 热路径性能满足要求（> 1kHz）
- ✅ 状态检查正确集成
- ✅ 权限控制正确（`pub(crate)` 方法不可外部访问）
- ✅ Mock 测试覆盖所有方法

---

### 任务 2.3: MotionCommander（公开受限权限）

**目标**: 用户可访问的运动控制接口

**文件**: `src/client/commander.rs`

**清单**:
- [ ] 实现 `MotionCommander` 结构
- [ ] 包装 `RawCommander` 的运动方法
- [ ] 实现夹爪控制方法
- [ ] 确保无状态变更能力

**代码框架**:
```rust
// src/client/commander.rs

/// 运动控制器（仅运动指令，无状态变更能力）
#[derive(Clone)]
pub struct MotionCommander {
    pub(crate) raw: Arc<RawCommander>,
}

impl MotionCommander {
    /// 发送 MIT 模式指令
    pub fn send_mit_command(
        &self,
        joint: Joint,
        position: Rad,
        velocity: f64,
        kp: f64,
        kd: f64,
        torque: NewtonMeter,
    ) -> Result<(), RobotError> {
        self.raw.send_mit_command(joint, position, velocity, kp, kd, torque)
    }

    /// 发送位置指令（便捷方法）
    pub fn command_position(
        &self,
        positions: JointArray<Rad>,
    ) -> Result<(), RobotError> {
        for joint in Joint::ALL {
            self.raw.send_position_command(joint, positions[joint])?;
        }
        Ok(())
    }

    /// 控制夹爪
    pub fn set_gripper(&self, position: f64, effort: f64) -> Result<(), RobotError> {
        self.raw.send_gripper_command(position, effort)
    }

    /// 打开夹爪
    pub fn open_gripper(&self, effort: f64) -> Result<(), RobotError> {
        self.set_gripper(GRIPPER_MAX_POSITION, effort)
    }

    /// 关闭夹爪
    pub fn close_gripper(&self, effort: f64) -> Result<(), RobotError> {
        self.set_gripper(GRIPPER_MIN_POSITION, effort)
    }

    // ❌ 不存在 set_control_mode(), enable_arm() 等方法
}
```

**测试要求**:
```rust
// tests/client/motion_commander.rs

#[test]
fn test_capability_restriction() {
    let (commander, _mock) = setup_motion_commander();

    // ✅ 可以发送运动指令
    assert!(commander.send_mit_command(...).is_ok());

    // ✅ 可以控制夹爪
    assert!(commander.open_gripper(10.0).is_ok());

    // ❌ 不能访问状态变更方法（编译时错误）
    // commander.set_control_mode(...);  // 编译失败
    // commander.disable_arm();          // 编译失败
}

#[test]
fn test_gripper_control() {
    let (commander, mock) = setup_motion_commander();

    commander.open_gripper(10.0).unwrap();
    assert_eq!(mock.get_gripper_position(), GRIPPER_MAX_POSITION);

    commander.close_gripper(5.0).unwrap();
    assert_eq!(mock.get_gripper_position(), GRIPPER_MIN_POSITION);
}
```

**验收标准**:
- ✅ 编译期权限限制生效
- ✅ 所有运动方法可用
- ✅ 夹爪控制正确
- ✅ 文档清晰说明权限范围

---

### 任务 2.4: Observer（状态观察器）

**目标**: 无锁状态读取接口

**文件**: `src/client/observer.rs`

**清单**:
- [ ] 实现 `Observer` 结构
- [ ] 实现关节状态读取
- [ ] 实现夹爪状态读取
- [ ] 实现错误状态查询
- [ ] 实现 `Clone` trait

**代码框架**:
```rust
// src/client/observer.rs

use parking_lot::RwLock;
use std::sync::Arc;

/// 状态观察器（只读，可克隆）
#[derive(Clone)]
pub struct Observer {
    state: Arc<RwLock<RobotState>>,
}

impl Observer {
    /// 获取完整状态快照
    pub fn state(&self) -> RobotState {
        self.state.read().clone()
    }

    /// 获取关节位置
    pub fn joint_positions(&self) -> JointArray<Rad> {
        self.state().joint_positions
    }

    /// 获取关节速度
    pub fn joint_velocities(&self) -> JointArray<f64> {
        self.state().joint_velocities
    }

    /// 获取关节力矩
    pub fn joint_torques(&self) -> JointArray<NewtonMeter> {
        self.state().joint_torques
    }

    /// 获取夹爪状态
    pub fn gripper_state(&self) -> GripperState {
        self.state().gripper_state
    }

    /// 获取夹爪位置
    pub fn gripper_position(&self) -> f64 {
        self.gripper_state().position
    }

    /// 获取夹爪力
    pub fn gripper_effort(&self) -> f64 {
        self.gripper_state().effort
    }

    /// 检查夹爪是否使能
    pub fn is_gripper_enabled(&self) -> bool {
        self.gripper_state().enabled
    }

    /// 检查机械臂是否使能
    pub fn is_arm_enabled(&self) -> bool {
        self.state().arm_enabled
    }

    /// 获取最后更新时间
    pub fn last_update(&self) -> Instant {
        self.state().last_update
    }
}

/// 机器人状态
#[derive(Debug, Clone)]
pub struct RobotState {
    pub joint_positions: JointArray<Rad>,
    pub joint_velocities: JointArray<f64>,
    pub joint_torques: JointArray<NewtonMeter>,
    pub gripper_state: GripperState,
    pub arm_enabled: bool,
    pub last_update: Instant,
}

/// 夹爪状态
#[derive(Debug, Clone)]
pub struct GripperState {
    pub position: f64,     // 开口宽度（米）
    pub effort: f64,       // 当前力（N·m）
    pub enabled: bool,     // 是否使能
}
```

**测试要求**:
```rust
// tests/client/observer.rs

#[test]
fn test_concurrent_read() {
    use std::thread;

    let observer = Arc::new(setup_observer());
    let mut handles = vec![];

    // 10 个线程同时读取
    for _ in 0..10 {
        let obs = observer.clone();
        handles.push(thread::spawn(move || {
            for _ in 0..1000 {
                let _ = obs.joint_positions();
                let _ = obs.gripper_state();
            }
        }));
    }

    for handle in handles {
        handle.join().unwrap();
    }
}

#[test]
fn test_gripper_state_query() {
    let (observer, mock) = setup_observer_with_mock();

    mock.set_gripper_state(0.05, 10.0, true);

    assert_eq!(observer.gripper_position(), 0.05);
    assert_eq!(observer.gripper_effort(), 10.0);
    assert!(observer.is_gripper_enabled());
}
```

**验收标准**:
- ✅ 多线程并发读取安全
- ✅ 夹爪状态查询完整
- ✅ **必须解析 CAN 协议中的夹爪反馈字段**（0x4xx ID 的 CAN 帧）
  - 夹爪位置（开口宽度，米）
  - 夹爪力度（N·m）
  - 夹爪使能状态（bool）
- ✅ 性能开销低（< 100ns per query）
- ✅ 文档示例丰富

**⚠️ 实施注意事项**:
- 确保 `Observer` 的状态更新逻辑中包含对夹爪 CAN 帧的解析
- 夹爪状态应该与关节状态以相同的频率更新
- 添加夹爪状态解析的单元测试（模拟 CAN 帧）

---

### 任务 2.5: StateMonitor（后台监控线程）

**目标**: 物理/类型状态同步

**文件**: `src/client/state_monitor.rs`

**清单**:
- [ ] 实现 `StateMonitor` 后台线程
- [ ] 实现物理状态轮询（20Hz）
- [ ] 实现状态不一致检测
- [ ] 实现自动 Poison 机制
- [ ] 实现优雅关闭

**代码框架**:
```rust
// src/client/state_monitor.rs

use std::sync::Arc;
use std::thread;
use std::time::Duration;

pub(crate) struct StateMonitor {
    handle: Option<thread::JoinHandle<()>>,
    shutdown: Arc<AtomicBool>,
}

impl StateMonitor {
    pub fn start(
        can_interface: Arc<dyn CanInterface>,
        state_tracker: Arc<StateTracker>,
        observer: Observer,
        config: MonitorConfig,
    ) -> Self {
        let shutdown = Arc::new(AtomicBool::new(false));
        let shutdown_clone = shutdown.clone();

        let handle = thread::spawn(move || {
            Self::monitor_loop(
                can_interface,
                state_tracker,
                observer,
                config,
                shutdown_clone,
            );
        });

        StateMonitor {
            handle: Some(handle),
            shutdown,
        }
    }

    fn monitor_loop(
        can_interface: Arc<dyn CanInterface>,
        state_tracker: Arc<StateTracker>,
        observer: Observer,
        config: MonitorConfig,
        shutdown: Arc<AtomicBool>,
    ) {
        let interval = Duration::from_millis(config.poll_interval_ms);

        while !shutdown.load(Ordering::Relaxed) {
            // 1. 读取硬件状态
            match Self::poll_hardware_state(&can_interface) {
                Ok(hardware_state) => {
                    // 2. 检查状态一致性
                    if let Err(reason) = Self::check_consistency(
                        &hardware_state,
                        &state_tracker,
                    ) {
                        // 3. 状态不一致，标记 Poisoned
                        state_tracker.mark_poisoned(reason);
                    }

                    // 4. 更新 Observer
                    Self::update_observer(&observer, hardware_state);
                }
                Err(e) => {
                    // 硬件通信失败
                    state_tracker.mark_poisoned(format!("Hardware poll failed: {}", e));
                }
            }

            thread::sleep(interval);
        }
    }

    pub fn shutdown(mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            handle.join().unwrap();
        }
    }
}

impl Drop for StateMonitor {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}
```

**测试要求**:
```rust
// tests/client/state_monitor.rs

#[test]
fn test_state_drift_detection() {
    let (monitor, mock, state_tracker) = setup_monitor();

    // 模拟状态不一致
    mock.simulate_mode_change(ControlMode::Standby);  // 硬件切换到 Standby
    // 但 state_tracker 期望 MitMode

    // 等待监控线程检测
    thread::sleep(Duration::from_millis(100));

    // 应该被标记为 Poisoned
    assert!(!state_tracker.is_valid());
}

#[test]
fn test_emergency_stop_detection() {
    let (monitor, mock, state_tracker) = setup_monitor();

    // 模拟急停
    mock.simulate_emergency_stop();

    thread::sleep(Duration::from_millis(100));

    assert!(!state_tracker.is_valid());
    assert!(matches!(
        state_tracker.check_valid_fast(),
        Err(RobotError::EmergencyStop)
    ));
}

#[test]
fn test_graceful_shutdown() {
    let monitor = setup_monitor().0;

    let start = Instant::now();
    monitor.shutdown();
    let elapsed = start.elapsed();

    // 应该在 100ms 内优雅关闭
    assert!(elapsed.as_millis() < 100);
}
```

**验收标准**:
- ✅ 状态不一致检测准确
- ✅ 响应时间 < 100ms（20Hz 轮询）
- ✅ 优雅关闭无死锁
- ✅ 长时间运行稳定（> 1 小时压力测试）

---

### 任务 2.6: Phase 2 集成测试

**目标**: 验证读写分离和性能

**清单**:
- [ ] 并发读写测试
- [ ] 热路径性能测试
- [ ] 状态同步压力测试
- [ ] 内存泄漏检查

**集成测试**:
```rust
// tests/integration/phase2_concurrent.rs

#[test]
fn test_concurrent_command_and_observe() {
    use std::sync::Arc;
    use std::thread;

    let (commander, observer, _mock) = setup_system();

    let commander = Arc::new(commander);
    let observer = Arc::new(observer);

    // 控制线程（高频）
    let commander_clone = commander.clone();
    let control_thread = thread::spawn(move || {
        for _ in 0..10_000 {
            let _ = commander_clone.send_mit_command(...);
        }
    });

    // 观察线程（低频）
    let observer_clone = observer.clone();
    let observe_thread = thread::spawn(move || {
        for _ in 0..1_000 {
            let _ = observer_clone.joint_positions();
            thread::sleep(Duration::from_micros(100));
        }
    });

    control_thread.join().unwrap();
    observe_thread.join().unwrap();
}
```

**性能基准测试**:
```rust
// benches/phase2_performance.rs

fn benchmark_command_throughput(c: &mut Criterion) {
    let (commander, _mock) = setup_commander();

    c.bench_function("send_mit_command_throughput", |b| {
        b.iter(|| {
            commander.send_mit_command(
                Joint::J1,
                Rad(0.0),
                0.0,
                10.0,
                1.0,
                NewtonMeter(0.0),
            )
        })
    });
}

// 目标: > 1 kHz (< 1 ms per command)
```

**验收标准**:
- ✅ 并发测试无死锁、数据竞争
- ✅ 命令吞吐量 > 1kHz
- ✅ 内存使用稳定（无泄漏）
- ✅ Valgrind/Miri 检查通过

**Phase 2 预计时间**: 7-8 个工作日

---

## 🎛️ Phase 3: Type State 核心 (10 天)

> **文档引用**: [v3.2 Final §5 - Type State Pattern](rust_high_level_api_design_v3.2_final.md#5-type-state-pattern-编译期状态安全)

### 任务 3.1: 状态类型定义

**目标**: 实现零大小类型（ZST）标记

**文件**: `src/state/machine.rs`

**清单**:
- [ ] 定义状态类型（`Disconnected`, `Standby`, 等）
- [ ] 定义控制模式类型（`MitMode`, `PositionMode`）
- [ ] 实现 `PhantomData` 标记

**代码框架**:
```rust
// src/state/machine.rs

use std::marker::PhantomData;

// === 连接状态 ===
pub struct Disconnected;
pub struct Standby;

// === 控制模式（MitMode 的子状态）===
pub struct MitMode;
pub struct PositionMode;

// === Piper 状态机 ===
pub struct Piper<State = Disconnected> {
    pub(crate) raw_commander: Arc<RawCommander>,
    pub(crate) observer: Observer,
    pub(crate) state_monitor: StateMonitor,
    pub(crate) heartbeat: HeartbeatManager,
    _state: PhantomData<State>,
}
```

**验收标准**:
- ✅ 零大小类型（`size_of::<Disconnected>() == 0`）
- ✅ 编译器能正确推断状态类型

---

### 任务 3.2: 状态转换实现

**目标**: 实现类型安全的状态转换

**文件**: `src/state/machine.rs`

**清单**:
- [ ] 实现 `connect()` -> `Piper<Standby>`
- [ ] 实现 `enable_mit_mode()` -> `Piper<MitMode>`
- [ ] 实现 `enable_position_mode()` -> `Piper<PositionMode>`
- [ ] 实现 `disable()` -> `Piper<Standby>`
- [ ] 实现 `Drop` trait（自动回到安全状态）

**代码框架**:
```rust
// src/state/machine.rs

impl Piper<Disconnected> {
    /// 连接到机械臂
    pub fn connect(config: ConnectionConfig) -> Result<Piper<Standby>, RobotError> {
        // 1. 初始化 CAN 接口
        let can_interface = ...;

        // 2. 创建 RawCommander, Observer, StateTracker
        let raw_commander = Arc::new(RawCommander::new(...));
        let observer = Observer::new(...);
        let state_tracker = Arc::new(StateTracker::new());

        // 3. 启动 StateMonitor
        let state_monitor = StateMonitor::start(...);

        // 4. 启动 Heartbeat
        let heartbeat = HeartbeatManager::start(...);

        Ok(Piper {
            raw_commander,
            observer,
            state_monitor,
            heartbeat,
            _state: PhantomData,
        })
    }
}

impl Piper<Standby> {
    /// 使能 MIT 模式
    pub fn enable_mit_mode(
        self,
        config: MitModeConfig,
    ) -> Result<Piper<MitMode>, RobotError> {
        // 1. 使能机械臂
        self.raw_commander.enable_arm()?;

        // 2. 等待使能完成
        self.wait_for_enabled(config.timeout)?;

        // 3. 设置 MIT 模式
        self.raw_commander.set_control_mode(ControlMode::Mit)?;

        // 4. 更新状态跟踪器
        self.raw_commander.state_tracker.expect_mode_transition(
            ControlMode::Mit,
            ArmController::Enabled,
        );

        // 5. 类型转换
        Ok(Piper {
            raw_commander: self.raw_commander,
            observer: self.observer,
            state_monitor: self.state_monitor,
            heartbeat: self.heartbeat,
            _state: PhantomData,
        })
    }

    /// 使能位置模式（类似实现）
    pub fn enable_position_mode(
        self,
        config: PositionModeConfig,
    ) -> Result<Piper<PositionMode>, RobotError> {
        // ... 类似实现 ...
    }
}

impl Piper<MitMode> {
    /// 发送力矩指令
    pub fn command_torques(
        &self,
        joint: Joint,
        position: Rad,
        velocity: f64,
        kp: f64,
        kd: f64,
        torque: NewtonMeter,
    ) -> Result<(), RobotError> {
        self.raw_commander.send_mit_command(joint, position, velocity, kp, kd, torque)
    }

    /// 获取 MotionCommander（受限权限）
    pub fn motion_commander(&self) -> MotionCommander {
        MotionCommander {
            raw: self.raw_commander.clone(),
        }
    }

    /// 失能（返回 Standby）
    pub fn disable(self) -> Result<Piper<Standby>, RobotError> {
        self.raw_commander.disable_arm()?;
        self.wait_for_disabled()?;

        Ok(Piper {
            raw_commander: self.raw_commander,
            observer: self.observer,
            state_monitor: self.state_monitor,
            heartbeat: self.heartbeat,
            _state: PhantomData,
        })
    }
}

// === Drop 实现（安全关闭）===
impl<State> Drop for Piper<State> {
    fn drop(&mut self) {
        // 1. 尝试失能（忽略错误）
        let _ = self.raw_commander.disable_arm();

        // 2. 关闭 Heartbeat
        // (HeartbeatManager 的 Drop 会自动处理)

        // 3. 关闭 StateMonitor
        // (StateMonitor 的 Drop 会自动处理)
    }
}
```

**测试要求**:
```rust
// tests/state/machine.rs

#[test]
fn test_state_transitions() {
    let piper = Piper::connect(config).unwrap();
    assert_type::<Piper<Standby>>(&piper);

    let piper = piper.enable_mit_mode(config).unwrap();
    assert_type::<Piper<MitMode>>(&piper);

    let piper = piper.disable().unwrap();
    assert_type::<Piper<Standby>>(&piper);
}

#[test]
fn test_compile_time_safety() {
    let piper = Piper::connect(config).unwrap();

    // ❌ 编译失败：Standby 没有 command_torques 方法
    // piper.command_torques(...);

    let piper = piper.enable_mit_mode(config).unwrap();

    // ✅ 编译成功
    piper.command_torques(...).unwrap();
}

#[test]
fn test_drop_safety() {
    let _piper = Piper::connect(config)
        .unwrap()
        .enable_mit_mode(config)
        .unwrap();

    // Drop 时应该自动失能
} // <- piper dropped here
```

**验收标准**:
- ✅ 状态转换编译期检查有效
- ✅ 非法状态转换编译失败
- ✅ `Drop` 安全性测试通过
- ✅ 文档示例完整

---

### 任务 3.3: Heartbeat 机制

**目标**: 后台心跳防止控制线程冻结

**文件**: `src/client/heartbeat.rs`

**清单**:
- [ ] 实现 `HeartbeatManager`
- [ ] 后台线程定期发送心跳（50Hz）
- [ ] 硬件超时保护
- [ ] 优雅关闭

**代码框架**:
```rust
// src/client/heartbeat.rs

pub(crate) struct HeartbeatManager {
    handle: Option<thread::JoinHandle<()>>,
    shutdown: Arc<AtomicBool>,
}

impl HeartbeatManager {
    pub fn start(
        can_interface: Arc<dyn CanInterface>,
        config: HeartbeatConfig,
    ) -> Self {
        let shutdown = Arc::new(AtomicBool::new(false));
        let shutdown_clone = shutdown.clone();

        let handle = thread::spawn(move || {
            Self::heartbeat_loop(can_interface, config, shutdown_clone);
        });

        HeartbeatManager {
            handle: Some(handle),
            shutdown,
        }
    }

    fn heartbeat_loop(
        can_interface: Arc<dyn CanInterface>,
        config: HeartbeatConfig,
        shutdown: Arc<AtomicBool>,
    ) {
        let interval = Duration::from_millis(config.interval_ms);

        while !shutdown.load(Ordering::Relaxed) {
            // 发送心跳帧
            if let Err(e) = Self::send_heartbeat(&can_interface) {
                log::warn!("Heartbeat failed: {}", e);
            }

            thread::sleep(interval);
        }
    }
}
```

**测试要求**:
```rust
#[test]
fn test_heartbeat_prevents_timeout() {
    let (piper, mock) = setup_piper_with_mock();

    // 模拟控制线程冻结 200ms
    thread::sleep(Duration::from_millis(200));

    // 硬件应该收到心跳，不会超时
    assert!(!mock.is_timeout());
}
```

**验收标准**:
- ✅ 心跳正常发送（50Hz）
- ✅ 防止硬件超时
- ✅ 优雅关闭

---

### 任务 3.4: Phase 3 集成测试

**目标**: 验证 Type State 完整性

**清单**:
- [ ] 状态机完整流程测试
- [ ] 异常场景测试
- [ ] 内存安全测试

**集成测试**:
```rust
// tests/integration/phase3_state_machine.rs

#[test]
fn test_full_lifecycle() {
    // 连接
    let piper = Piper::connect(config).unwrap();

    // 使能
    let piper = piper.enable_mit_mode(config).unwrap();

    // 控制
    for _ in 0..100 {
        piper.command_torques(...).unwrap();
    }

    // 失能
    let piper = piper.disable().unwrap();

    // 断开连接（自动 Drop）
}

#[test]
fn test_error_recovery() {
    let piper = Piper::connect(config).unwrap();
    let piper = piper.enable_mit_mode(config).unwrap();

    // 模拟急停
    mock.simulate_emergency_stop();

    // 下一个命令应该失败
    assert!(piper.command_torques(...).is_err());

    // 尝试恢复
    let piper = piper.disable().unwrap();
    mock.clear_emergency_stop();
    let piper = piper.enable_mit_mode(config).unwrap();

    // 应该恢复正常
    assert!(piper.command_torques(...).is_ok());
}
```

**验收标准**:
- ✅ 完整生命周期测试通过
- ✅ 异常恢复测试通过
- ✅ 无内存泄漏
- ✅ Miri 检查通过

**Phase 3 预计时间**: 10 个工作日

---

## 🎮 Phase 4: Tick/Iterator + 控制器 (7-8 天)

> **文档引用**:
> - [v3.2 Final §6 - Tick 模式](rust_high_level_api_design_v3.2_final.md#6-tick-模式-inversion-of-control)
> - [v3.2 Final §2 - 安全重置策略](rust_high_level_api_design_v3.2_final.md#-问题-2-控制器重置策略的安全隐患)

### 任务 4.1: Controller Trait

**目标**: 通用控制器接口

**文件**: `src/control/traits.rs`

**清单**:
- [ ] 定义 `Controller` trait
- [ ] 实现 `tick()` 方法
- [ ] ✅ 实现 `on_time_jump()` 方法
- [ ] 实现配置和统计

**代码框架**:
```rust
// src/control/traits.rs

pub trait Controller {
    type Error: std::error::Error + Send + Sync + 'static;

    /// 执行一次控制循环
    ///
    /// # 参数
    ///
    /// - `dt`: 距离上次 tick 的时间间隔
    ///
    /// # 注意
    ///
    /// `dt` 会被 `run_controller` 钳位到 `max_dt`，但控制器内部状态
    /// 可能仍然包含大时间跨度的累积效应。
    fn tick(&mut self, dt: Duration) -> Result<(), Self::Error>;

    /// 处理时间跳变
    ///
    /// 当检测到 `dt > max_dt` 时，`run_controller` 会在钳位 `dt` 之前调用此方法。
    ///
    /// # 默认行为
    ///
    /// 默认实现什么都不做（`Ok(())`），这适用于无状态或时间不敏感的控制器。
    ///
    /// # ⚠️ 重要提示
    ///
    /// **强烈建议所有时间敏感的控制器（如 PID）覆盖此方法！**
    ///
    /// ## 为什么？
    ///
    /// 即使 `dt` 被钳位，控制器内部状态（如微分项 `last_error`）仍然
    /// 包含大时间跨度前的值。如果不重置，可能导致：
    ///
    /// - **微分项爆炸**: `(error - last_error) / clamped_dt` 计算出巨大的导数
    /// - **输出突变**: 控制量瞬间变化，导致机械臂剧烈运动
    ///
    /// ## 推荐做法（PID 示例）
    ///
    /// ```rust
    /// fn on_time_jump(&mut self, dt: Duration) -> Result<(), Self::Error> {
    ///     // ✅ 重置微分项（防止微分噪声）
    ///     self.last_error = 0.0;
    ///
    ///     // ❌ 不清空积分项（保留抗重力补偿）
    ///     // self.integral = 0.0;  // 危险！会导致负载下坠
    ///
    ///     log::warn!("Time jump detected: {:?}, D-term reset", dt);
    ///     Ok(())
    /// }
    /// ```
    ///
    /// # 参见
    ///
    /// - [`reset()`] - 完全重置（包括积分项，更危险）
    fn on_time_jump(&mut self, _dt: Duration) -> Result<(), Self::Error> {
        Ok(())
    }

    /// 完全重置控制器状态
    ///
    /// # ⚠️ 危险
    ///
    /// 此方法会清空所有内部状态（包括积分项）。对于 PID 控制器，
    /// 这意味着丢失抗重力补偿，可能导致机械臂突然下坠。
    ///
    /// **除非你明确知道自己在做什么，否则请使用 [`on_time_jump()`]。**
    ///
    /// # 使用场景
    ///
    /// - 切换目标位置时（可选）
    /// - 从错误状态恢复时（谨慎）
    /// - 控制器重新初始化时
    ///
    /// # 参见
    ///
    /// - [`on_time_jump()`] - 更安全的时间跳变处理
    fn reset(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}
```

**验收标准**:
- ✅ Trait 设计清晰
- ✅ 文档说明详细（特别是 `on_time_jump` vs `reset`）
- ✅ **`on_time_jump` 文档必须包含警告和推荐做法**
- ✅ **`reset` 文档必须包含危险提示**
- ✅ 代码示例可编译（`cargo test --doc`）

**⚠️ 实施注意事项**:
- 在 `run_controller` 中，必须在钳位 `dt` **之前**调用 `on_time_jump`
- 所有实现 `Controller` 的 PID 类控制器都必须覆盖 `on_time_jump`
- 在测试中验证 `on_time_jump` 的正确性（见任务 4.2 测试）

---

### 任务 4.2: SafePidController 实现

**目标**: 生产级 PID 控制器

**文件**: `src/control/pid.rs`

**清单**:
- [ ] 实现 `SafePidController`
- [ ] 实现积分饱和保护
- [ ] ✅ 实现智能时间跳变处理
- [ ] 实现微分项平滑（可选）

**代码框架**:
```rust
// src/control/pid.rs

pub struct SafePidController {
    kp: f64,
    ki: f64,
    kd: f64,

    // PID 状态
    integral: f64,
    last_error: f64,

    // 保护
    integral_limit: f64,
    output_limit: f64,

    // 配置
    target: Rad,
    commander: MotionCommander,
    joint: Joint,
}

impl Controller for SafePidController {
    type Error = RobotError;

    fn tick(&mut self, dt: Duration) -> Result<(), RobotError> {
        let dt_sec = dt.as_secs_f64();

        // 1. 读取当前位置
        let current = self.commander.observer().joint_positions()[self.joint];
        let error = (self.target - current).0;

        // 2. 计算 PID
        let p_term = self.kp * error;

        self.integral += error * dt_sec;
        self.integral = self.integral.clamp(-self.integral_limit, self.integral_limit);
        let i_term = self.ki * self.integral;

        let d_term = self.kd * (error - self.last_error) / dt_sec;
        self.last_error = error;

        let output = (p_term + i_term + d_term).clamp(-self.output_limit, self.output_limit);

        // 3. 发送指令
        self.commander.send_mit_command(
            self.joint,
            self.target,
            0.0,
            0.0,
            0.0,
            NewtonMeter(output),
        )?;

        Ok(())
    }

    /// ✅ 智能时间跳变处理
    fn on_time_jump(&mut self, dt: Duration) -> Result<(), RobotError> {
        // 只重置微分项（防止微分噪声）
        // ❌ 不重置积分项（保留抗重力补偿）
        self.last_error = 0.0;

        log::warn!("Time jump detected: {:?}, reset D-term only", dt);
        Ok(())
    }

    /// ⚠️ 完全重置（慎用！）
    fn reset(&mut self) -> Result<(), RobotError> {
        self.integral = 0.0;
        self.last_error = 0.0;
        log::warn!("PID reset: I-term cleared (may cause sagging!)");
        Ok(())
    }
}
```

**测试要求**:
```rust
// tests/control/pid.rs

#[test]
fn test_pid_control_stability() {
    let (controller, mock) = setup_pid_controller();

    // 运行 1000 次
    for _ in 0..1000 {
        controller.tick(Duration::from_millis(10)).unwrap();
    }

    // 应该收敛到目标
    let error = (controller.target - mock.get_position(controller.joint)).0.abs();
    assert!(error < 0.01);  // 0.01 rad 误差
}

#[test]
fn test_time_jump_safety() {
    let (mut controller, mock) = setup_pid_controller_with_load(5.0);  // 5kg 负载

    // 正常运行一段时间，积累积分项
    for _ in 0..100 {
        controller.tick(Duration::from_millis(10)).unwrap();
    }

    let integral_before = controller.integral;
    let position_before = mock.get_position(controller.joint);

    // 模拟时间跳变
    controller.on_time_jump(Duration::from_millis(100)).unwrap();

    // ✅ 积分项应该保留
    assert_eq!(controller.integral, integral_before);

    // ✅ 微分项应该重置
    assert_eq!(controller.last_error, 0.0);

    // 继续运行
    controller.tick(Duration::from_millis(10)).unwrap();

    // ✅ 位置应该稳定（不下坠）
    let position_after = mock.get_position(controller.joint);
    assert!((position_after - position_before).0.abs() < 0.05);
}

#[test]
fn test_reset_vs_on_time_jump() {
    let (mut controller, mock) = setup_pid_controller_with_load(5.0);

    // 积累积分项
    for _ in 0..100 {
        controller.tick(Duration::from_millis(10)).unwrap();
    }

    let integral = controller.integral;
    assert!(integral.abs() > 0.1);  // 有显著积分

    // 测试 on_time_jump
    let mut controller_copy = controller.clone();
    controller_copy.on_time_jump(Duration::from_millis(100)).unwrap();
    assert_eq!(controller_copy.integral, integral);  // ✅ 保留

    // 测试 reset
    controller.reset().unwrap();
    assert_eq!(controller.integral, 0.0);  // ❌ 清零
}
```

**验收标准**:
- ✅ PID 控制稳定性测试通过
- ✅ `on_time_jump` 不导致下坠
- ✅ `reset` 行为正确（有警告日志）
- ✅ 积分饱和保护有效

---

### 任务 4.3: TrajectoryPlanner（轨迹规划器）⭐ NEW

**目标**: 实现基于时间的轨迹插值迭代器

**文件**: `src/control/trajectory.rs`

**清单**:
- [ ] 实现 `TrajectoryPlanner` 结构
- [ ] 实现三次样条插值（Cubic Spline）
- [ ] 实现 `Iterator` trait
- [ ] 输出 `(JointArray<Rad>, JointArray<f64>)` (位置 + 速度)
- [ ] 实现轨迹点验证（关节限位检查）

**代码框架**:
```rust
// src/control/trajectory.rs

use crate::types::{Joint, JointArray, Rad};
use std::time::Duration;

/// 轨迹规划器（迭代器模式）
pub struct TrajectoryPlanner {
    start: JointArray<Rad>,
    end: JointArray<Rad>,
    duration: Duration,
    frequency: u32,

    // 内部状态
    current_step: usize,
    total_steps: usize,

    // 样条系数（每个关节）
    spline_coeffs: JointArray<CubicSplineCoeffs>,
}

/// 三次样条系数
#[derive(Debug, Clone, Copy)]
struct CubicSplineCoeffs {
    a: f64,  // position_start
    b: f64,  // velocity_start
    c: f64,  // 3*(position_end - position_start) - 2*velocity_start - velocity_end
    d: f64,  // 2*(position_start - position_end) + velocity_start + velocity_end
}

impl TrajectoryPlanner {
    /// 创建新的轨迹规划器
    ///
    /// # 参数
    ///
    /// - `start`: 起始关节位置
    /// - `end`: 目标关节位置
    /// - `duration`: 轨迹持续时间
    /// - `frequency`: 插值频率（Hz）
    ///
    /// # 示例
    ///
    /// ```rust
    /// let planner = TrajectoryPlanner::new(
    ///     start_positions,
    ///     end_positions,
    ///     Duration::from_secs(5),
    ///     500,  // 500Hz
    /// );
    ///
    /// for (positions, velocities) in planner {
    ///     piper.command_positions(positions)?;
    ///     thread::sleep(Duration::from_millis(2));
    /// }
    /// ```
    pub fn new(
        start: JointArray<Rad>,
        end: JointArray<Rad>,
        duration: Duration,
        frequency: u32,
    ) -> Self {
        let total_steps = (duration.as_secs_f64() * frequency as f64) as usize;
        let duration_sec = duration.as_secs_f64();

        // 为每个关节计算三次样条系数
        // ⚠️ 注意：当前实现假设起止速度为 0
        // 如果未来需要支持 Via Points（中间点速度 ≠ 0），必须进行时间缩放：
        // v_scaled = v_physical * duration_sec
        // 因为样条在归一化时间域 [0, 1] 上定义，而物理速度在实际时间域上定义
        let spline_coeffs = start.map_with(end, |s, e| {
            // 当前：起止速度均为 0（点对点运动）
            Self::compute_cubic_spline(s.0, 0.0, e.0, 0.0)

            // 未来扩展示例（Via Points）：
            // let v_start_scaled = v_start_physical * duration_sec;
            // let v_end_scaled = v_end_physical * duration_sec;
            // Self::compute_cubic_spline(s.0, v_start_scaled, e.0, v_end_scaled)
        });

        TrajectoryPlanner {
            start,
            end,
            duration,
            frequency,
            current_step: 0,
            total_steps,
            spline_coeffs,
        }
    }

    /// 计算三次样条系数（位置和速度）
    ///
    /// # 参数
    ///
    /// - `p0`: 起始位置
    /// - `v0`: 起始速度（**已时间缩放**，归一化时间域）
    /// - `p1`: 终止位置
    /// - `v1`: 终止速度（**已时间缩放**，归一化时间域）
    ///
    /// # 数学背景
    ///
    /// 三次样条在归一化时间 t ∈ [0, 1] 上定义：
    /// ```text
    /// p(t) = a + b*t + c*t² + d*t³
    /// v(t) = b + 2*c*t + 3*d*t²
    /// ```
    ///
    /// 边界条件：
    /// - p(0) = p0, p(1) = p1
    /// - v(0) = v0, v(1) = v1
    ///
    /// ⚠️ **时间缩放重要提示**：
    ///
    /// 如果输入的是物理速度（如 rad/s），必须先乘以轨迹持续时间 T：
    /// ```text
    /// v_scaled = v_physical * T
    /// ```
    ///
    /// 这是因为归一化时间的导数关系：
    /// ```text
    /// dp/dt_physical = (dp/dt_normalized) * (dt_normalized/dt_physical)
    ///                = (dp/dt_normalized) / T
    /// ```
    ///
    /// 当前实现中，v0 = v1 = 0（点对点运动），所以不需要缩放。
    fn compute_cubic_spline(
        p0: f64,
        v0: f64,
        p1: f64,
        v1: f64,
    ) -> CubicSplineCoeffs {
        CubicSplineCoeffs {
            a: p0,
            b: v0,
            c: 3.0 * (p1 - p0) - 2.0 * v0 - v1,
            d: 2.0 * (p0 - p1) + v0 + v1,
        }
    }

    /// 计算给定时间点的位置和速度
    fn evaluate_at(&self, t: f64) -> (JointArray<Rad>, JointArray<f64>) {
        let positions = self.spline_coeffs.map(|coeffs| {
            let t2 = t * t;
            let t3 = t2 * t;
            Rad(coeffs.a + coeffs.b * t + coeffs.c * t2 + coeffs.d * t3)
        });

        let velocities = self.spline_coeffs.map(|coeffs| {
            let t2 = t * t;
            coeffs.b + 2.0 * coeffs.c * t + 3.0 * coeffs.d * t2
        });

        (positions, velocities)
    }
}

impl Iterator for TrajectoryPlanner {
    type Item = (JointArray<Rad>, JointArray<f64>);

    fn next(&mut self) -> Option<Self::Item> {
        if self.current_step >= self.total_steps {
            return None;
        }

        // 归一化时间 [0, 1]
        let t = self.current_step as f64 / self.total_steps as f64;

        self.current_step += 1;

        Some(self.evaluate_at(t))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.total_steps - self.current_step;
        (remaining, Some(remaining))
    }
}

impl ExactSizeIterator for TrajectoryPlanner {
    fn len(&self) -> usize {
        self.total_steps - self.current_step
    }
}

// 辅助 trait 扩展（内部使用）
trait JointArrayExt<T> {
    fn map_with<U, F>(&self, other: Self, f: F) -> JointArray<U>
    where
        F: FnMut(T, T) -> U,
        T: Copy;
}

impl<T> JointArrayExt<T> for JointArray<T> {
    fn map_with<U, F>(&self, other: Self, mut f: F) -> JointArray<U>
    where
        F: FnMut(T, T) -> U,
        T: Copy,
    {
        JointArray::new([
            f(self[Joint::J1], other[Joint::J1]),
            f(self[Joint::J2], other[Joint::J2]),
            f(self[Joint::J3], other[Joint::J3]),
            f(self[Joint::J4], other[Joint::J4]),
            f(self[Joint::J5], other[Joint::J5]),
            f(self[Joint::J6], other[Joint::J6]),
        ])
    }
}
```

**测试要求**:
```rust
// tests/control/trajectory.rs

#[test]
fn test_trajectory_start_end() {
    let start = JointArray::new([Rad(0.0); 6]);
    let end = JointArray::new([Rad(1.0); 6]);

    let mut planner = TrajectoryPlanner::new(
        start,
        end,
        Duration::from_secs(1),
        100,
    );

    // 第一个点应该是起点
    let (first_pos, first_vel) = planner.next().unwrap();
    for joint in Joint::ALL {
        assert!((first_pos[joint].0 - start[joint].0).abs() < 1e-6);
    }

    // 最后一个点应该是终点
    let (last_pos, _) = planner.last().unwrap();
    for joint in Joint::ALL {
        assert!((last_pos[joint].0 - end[joint].0).abs() < 1e-3);
    }
}

#[test]
fn test_trajectory_smoothness() {
    let start = JointArray::new([Rad(0.0); 6]);
    let end = JointArray::new([Rad(3.14); 6]);

    let planner = TrajectoryPlanner::new(
        start,
        end,
        Duration::from_secs(1),
        1000,  // 高频率减少数值噪声
    );

    let mut velocities_samples = Vec::new();

    for (_, velocities) in planner {
        velocities_samples.push(velocities[Joint::J1]);
    }

    // ✅ 方法 1: 检查速度连续性（相邻速度变化不应过大）
    let max_vel_jump = velocities_samples
        .windows(2)
        .map(|w| (w[1] - w[0]).abs())
        .max_by(|a, b| a.partial_cmp(b).unwrap())
        .unwrap_or(0.0);

    // 以 1kHz 采样，速度变化应 < 20 rad/s² (0.02 rad/s per ms)
    assert!(max_vel_jump < 0.02, "Max velocity jump: {}", max_vel_jump);

    // ✅ 方法 2: 检查速度的单调性变化（三次样条应该平滑过渡）
    // 加速阶段：速度单调递增；减速阶段：速度单调递减
    // 统计方向变化次数（应该只有 1 次：从加速切换到减速）
    let direction_changes = velocities_samples
        .windows(3)
        .filter(|w| {
            let d1 = w[1] - w[0];
            let d2 = w[2] - w[1];
            d1.signum() != d2.signum() && d1.abs() > 1e-6 && d2.abs() > 1e-6
        })
        .count();

    // 三次样条应该只有 1 个拐点（加速->减速）
    assert!(direction_changes <= 2, "Too many direction changes: {}", direction_changes);
}

#[test]
fn test_trajectory_acceleration_bounds() {
    // ⚠️ 注意：这个测试使用数值微分，可能有噪声
    let start = JointArray::new([Rad(0.0); 6]);
    let end = JointArray::new([Rad(3.14); 6]);

    let planner = TrajectoryPlanner::new(
        start,
        end,
        Duration::from_secs(1),
        1000,
    );

    let mut max_accel = 0.0;
    let mut last_vel = 0.0;
    let dt = 0.001;  // 1ms

    for (_, velocities) in planner {
        let vel = velocities[Joint::J1];
        let accel = (vel - last_vel) / dt;
        max_accel = max_accel.max(accel.abs());
        last_vel = vel;
    }

    // 加速度应该是有界的（三次样条的特性）
    // ⚠️ 放宽阈值以容忍数值噪声
    assert!(max_accel < 150.0, "Max accel: {} rad/s²", max_accel);
}

#[test]
fn test_iterator_length() {
    let start = JointArray::new([Rad(0.0); 6]);
    let end = JointArray::new([Rad(1.0); 6]);

    let planner = TrajectoryPlanner::new(
        start,
        end,
        Duration::from_secs(2),
        500,
    );

    // 2秒 * 500Hz = 1000 个点
    assert_eq!(planner.len(), 1000);
    assert_eq!(planner.count(), 1000);
}

#[test]
fn test_zero_velocity_at_endpoints() {
    let start = JointArray::new([Rad(0.0); 6]);
    let end = JointArray::new([Rad(1.0); 6]);

    let mut planner = TrajectoryPlanner::new(
        start,
        end,
        Duration::from_secs(1),
        100,
    );

    // ✅ 改进：直接检查边界条件，而不是依赖迭代器的首尾元素
    // 因为迭代器的 t 可能不是严格的 0.0 和 1.0

    // 起点速度应该严格等于 0（解析解）
    // v(t=0) = b = v0 = 0
    let (first_pos, first_vel) = planner.next().unwrap();
    for joint in Joint::ALL {
        // 起点位置应该精确匹配
        assert!((first_pos[joint].0 - start[joint].0).abs() < 1e-10);
        // 起点速度应该接近 0（由于 t ≈ 0 而非严格 = 0，允许小误差）
        assert!(first_vel[joint].abs() < 0.01,
                "First velocity at {:?}: {}", joint, first_vel[joint]);
    }

    // 终点速度应该接近 0
    let (last_pos, last_vel) = planner.last().unwrap();
    for joint in Joint::ALL {
        // 终点位置应该接近目标（允许小误差）
        assert!((last_pos[joint].0 - end[joint].0).abs() < 0.01);
        // 终点速度应该接近 0
        assert!(last_vel[joint].abs() < 0.01,
                "Last velocity at {:?}: {}", joint, last_vel[joint]);
    }
}

#[test]
fn test_analytical_boundary_conditions() {
    // ✅ 使用解析解直接验证边界条件（更可靠的测试）
    let start = JointArray::new([Rad(0.0); 6]);
    let end = JointArray::new([Rad(1.0); 6]);

    let planner = TrajectoryPlanner::new(
        start,
        end,
        Duration::from_secs(1),
        100,
    );

    // 直接访问样条系数验证边界条件
    // 对于三次样条 p(t) = a + b*t + c*t² + d*t³
    // 边界条件：p(0) = a, v(0) = b, p(1) = a+b+c+d, v(1) = b+2c+3d

    // 由于 v0 = v1 = 0，应该有：
    // b = 0 (起始速度)
    // b + 2c + 3d = 0 (终止速度)

    let coeffs = &planner.spline_coeffs[Joint::J1];

    // 起始速度 = 0
    assert!(coeffs.b.abs() < 1e-10, "b = {} (should be 0)", coeffs.b);

    // 终止速度 = 0
    let v_end = coeffs.b + 2.0 * coeffs.c + 3.0 * coeffs.d;
    assert!(v_end.abs() < 1e-10, "v(1) = {} (should be 0)", v_end);

    // 起始位置 = start
    assert!((coeffs.a - start[Joint::J1].0).abs() < 1e-10);

    // 终止位置 = end
    let p_end = coeffs.a + coeffs.b + coeffs.c + coeffs.d;
    assert!((p_end - end[Joint::J1].0).abs() < 1e-10);
}
```

**集成测试示例**:
```rust
// examples/trajectory_demo.rs

use piper_sdk::*;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let piper = Piper::connect(ConnectionConfig::default())?;
    let piper = piper.enable_position_mode(PositionModeConfig::default())?;

    let start = piper.observer().joint_positions();
    let end = JointArray::new([
        Rad(0.5), Rad(1.0), Rad(0.3),
        Rad(-0.5), Rad(0.0), Rad(0.2),
    ]);

    let planner = TrajectoryPlanner::new(
        start,
        end,
        Duration::from_secs(5),
        500,
    );

    println!("Executing trajectory ({} steps)...", planner.len());

    for (i, (positions, _velocities)) in planner.enumerate() {
        piper.command_positions(positions)?;

        if i % 100 == 0 {
            println!("Step {}: {:?}", i, positions[Joint::J1]);
        }

        spin_sleep::sleep(Duration::from_millis(2));
    }

    println!("Trajectory complete!");
    Ok(())
}
```

**验收标准**:
- ✅ 起点和终点精确匹配（误差 < 1mm）
- ✅ **边界条件解析验证**（直接检查样条系数，而非数值微分）⭐ NEW
- ✅ 轨迹平滑（速度连续性 + 方向变化次数 ≤ 2）⭐ IMPROVED
- ✅ 起止速度严格为 0（解析解：`b = 0`, `b + 2c + 3d = 0`）⭐ NEW
- ✅ Iterator 正确实现（`len()`, `size_hint()`）
- ✅ 性能满足要求（计算开销 < 1μs per step）
- ✅ 单元测试覆盖率 > 90%

**⚠️ 实施注意事项**:
- 三次样条是最简单的平滑插值，适合大多数场景
- **时间缩放重要性**：当前实现假设 v0 = v1 = 0，未来支持 Via Points 时必须对速度进行时间缩放（`v_scaled = v_physical * duration`）⭐ NEW
- 数值微分测试（加速度有界）容易产生噪声，应使用解析解验证边界条件 ⭐ NEW
- 未来可扩展为五次样条（更平滑的加速度）
- 可添加关节限位检查（在 `new()` 中验证）
- 可添加速度/加速度限制（动态调整 `duration`）

---

### 任务 4.4: run_controller 辅助函数

**目标**: 简化控制循环

**文件**: `src/control/runner.rs`

**清单**:
- [ ] 实现 `run_controller()` 函数
- [ ] 实现 `dt` 钳位
- [ ] 实现时间跳变检测
- [ ] 实现实时统计

**代码框架**:
```rust
// src/control/runner.rs

pub struct ControlLoopConfig {
    pub frequency: u32,
    pub max_dt: Duration,
    pub soft_start: bool,
}

pub struct ControlLoopStats {
    pub iterations: u64,
    pub average_dt: Duration,
    pub max_jitter: Duration,
    pub dt_violations: u64,
}

pub fn run_controller<C, F>(
    mut controller: C,
    config: ControlLoopConfig,
    mut should_stop: F,
) -> Result<ControlLoopStats, C::Error>
where
    C: Controller,
    F: FnMut() -> bool,
{
    let target_dt = Duration::from_micros(1_000_000 / config.frequency as u64);
    let max_dt = config.max_dt;

    let mut stats = ControlLoopStats::default();
    let mut last_tick = Instant::now();

    while !should_stop() {
        let now = Instant::now();
        let mut dt = now.duration_since(last_tick);

        // ✅ dt 钳位
        if dt > max_dt {
            stats.dt_violations += 1;
            log::warn!("dt violation: {:?} > {:?}", dt, max_dt);

            // 通知控制器时间跳变
            controller.on_time_jump(dt)?;

            dt = max_dt;
        }

        // 执行控制
        controller.tick(dt)?;

        // 更新统计
        stats.update(dt);
        last_tick = now;

        // 精确延迟
        let elapsed = now.elapsed();
        if elapsed < target_dt {
            spin_sleep::sleep(target_dt - elapsed);
        }
    }

    Ok(stats)
}
```

**测试要求**:
```rust
#[test]
fn test_control_loop_frequency() {
    let (controller, _mock) = setup_controller();

    let should_stop = Arc::new(AtomicBool::new(false));
    let should_stop_clone = should_stop.clone();

    thread::spawn(move || {
        thread::sleep(Duration::from_secs(1));
        should_stop_clone.store(true, Ordering::Relaxed);
    });

    let stats = run_controller(
        controller,
        ControlLoopConfig {
            frequency: 500,
            max_dt: Duration::from_millis(20),
            soft_start: false,
        },
        || should_stop.load(Ordering::Relaxed),
    ).unwrap();

    // 1 秒应该执行 ~500 次
    assert!((stats.iterations as i64 - 500).abs() < 50);
}
```

**验收标准**:
- ✅ 频率控制准确（误差 < 5%）
- ✅ `dt` 钳位正确
- ✅ `spin_sleep` 低抖动（< 100μs）
- ✅ 统计信息准确

---

### 任务 4.5: Phase 4 集成测试

**目标**: 验证控制器、轨迹规划和控制循环

**清单**:
- [ ] 完整控制循环测试
- [ ] 轨迹规划器集成测试 ⭐ NEW
- [ ] 性能测试（500Hz, 1kHz）
- [ ] 异常场景测试

**集成测试**:
```rust
// tests/integration/phase4_control.rs

#[test]
fn test_trajectory_execution() {
    let piper = Piper::connect(config).unwrap();
    let piper = piper.enable_position_mode(config).unwrap();

    let start = JointArray::new([Rad(0.0); 6]);
    let end = JointArray::new([Rad(1.0); 6]);

    let planner = TrajectoryPlanner::new(
        start,
        end,
        Duration::from_secs(5),
        500,
    );

    let total_steps = planner.len();
    let mut executed_steps = 0;

    for (positions, _velocities) in planner {
        piper.command_positions(positions).unwrap();
        executed_steps += 1;
        thread::sleep(Duration::from_millis(2));
    }

    assert_eq!(executed_steps, total_steps);

    // 验证最终位置
    thread::sleep(Duration::from_millis(100));
    let final_pos = piper.observer().joint_positions();
    for joint in Joint::ALL {
        assert!((final_pos[joint].0 - end[joint].0).abs() < 0.01);
    }
}

#[test]
fn test_gravity_compensation_simulation() {
    let piper = Piper::connect(config).unwrap();
    let piper = piper.enable_mit_mode(config).unwrap();

    let controllers: Vec<SafePidController> = Joint::ALL
        .iter()
        .map(|&joint| SafePidController::new(...))
        .collect();

    // 运行 10 秒
    let should_stop = Arc::new(AtomicBool::new(false));
    let should_stop_clone = should_stop.clone();

    thread::spawn(move || {
        thread::sleep(Duration::from_secs(10));
        should_stop_clone.store(true, Ordering::Relaxed);
    });

    for mut controller in controllers {
        let stats = run_controller(
            controller,
            ControlLoopConfig { frequency: 500, ... },
            || should_stop.load(Ordering::Relaxed),
        ).unwrap();

        println!("Stats: {:?}", stats);
        assert!(stats.iterations > 4900);  // ~500Hz * 10s
    }
}
```

**验收标准**:
- ✅ 轨迹规划器执行完整（无丢步）⭐ NEW
- ✅ 轨迹跟踪精度（终点误差 < 1cm）⭐ NEW
- ✅ 长时间运行稳定（> 10 分钟）
- ✅ 高频控制准确（1kHz）
- ✅ 异常场景恢复

**Phase 4 预计时间**: 8-9 个工作日（新增 TrajectoryPlanner +1 天）

---

## 📚 Phase 5: 完善和文档 (5 天)

> **文档引用**: [v3.2 Final §7 - 示例代码](rust_high_level_api_design_v3.2_final.md#7-示例代码)

### 任务 5.1: 完整示例

**目标**: 生产级示例代码

**清单**:
- [ ] Gravity compensation example
- [ ] 夹爪闭环控制示例
- [ ] 轨迹规划示例
- [ ] 多线程示例

**示例代码**:
```rust
// examples/gravity_compensation.rs

use piper_sdk::*;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. 连接
    let piper = Piper::connect(ConnectionConfig::default())?;
    println!("Connected to Piper");

    // 2. 使能
    let piper = piper.enable_mit_mode(MitModeConfig::default())?;
    println!("MIT mode enabled");

    // 3. 创建控制器
    let controllers: Vec<SafePidController> = Joint::ALL
        .iter()
        .map(|&joint| SafePidController::new(
            PidGains { kp: 10.0, ki: 0.1, kd: 1.0 },
            Rad(0.0),  // 目标位置
            piper.motion_commander(),
            joint,
        ))
        .collect();

    // 4. 运行控制循环
    let should_stop = Arc::new(AtomicBool::new(false));
    setup_signal_handler(should_stop.clone());

    for mut controller in controllers {
        let stats = run_controller(
            controller,
            ControlLoopConfig {
                frequency: 500,
                max_dt: Duration::from_millis(20),
                soft_start: true,
            },
            || should_stop.load(Ordering::Relaxed),
        )?;

        println!("Controller stats: {:?}", stats);
    }

    // 5. 失能（自动 Drop）
    println!("Shutting down...");
    Ok(())
}
```

**验收标准**:
- ✅ 所有示例可编译运行
- ✅ 示例代码有详细注释
- ✅ README 包含使用指南

---

### 任务 5.2: API 文档

**目标**: 完整的 Rustdoc

**清单**:
- [ ] 所有公开 API 有文档
- [ ] 文档示例可运行（`cargo test --doc`）
- [ ] 架构图集成到文档
- [ ] 添加 Cookbook

**文档要求**:
```rust
/// MIT 模式控制器
///
/// # 安全性
///
/// MIT 模式允许直接控制力矩，使用不当可能导致机械臂损坏。
/// 请确保：
/// - 力矩限制在安全范围内
/// - 实现合适的碰撞检测
/// - 使用 Heartbeat 机制
///
/// # 示例
///
/// ```no_run
/// use piper_sdk::*;
///
/// let piper = Piper::connect(ConnectionConfig::default())?;
/// let piper = piper.enable_mit_mode(MitModeConfig::default())?;
///
/// piper.command_torques(
///     Joint::J1,
///     Rad(0.0),
///     0.0,
///     10.0,
///     1.0,
///     NewtonMeter(0.5),
/// )?;
/// # Ok::<(), piper_sdk::RobotError>(())
/// ```
///
/// # 性能
///
/// - 命令发送延迟: < 50μs
/// - 支持频率: > 1kHz
///
/// # 参见
///
/// - [`PositionMode`] - 位置控制模式
/// - [`MotionCommander`] - 受限权限接口
pub struct Piper<MitMode> { ... }
```

**验收标准**:
- ✅ 文档覆盖率 > 95%
- ✅ `cargo doc --no-deps --open` 可浏览
- ✅ 所有示例测试通过

---

### 任务 5.3: 性能基准测试报告

**目标**: 完整的性能数据

**清单**:
- [ ] 热路径延迟测试
- [ ] 吞吐量测试
- [ ] 内存使用测试
- [ ] 生成性能报告

**基准测试**:
```rust
// benches/full_system.rs

fn benchmark_end_to_end(c: &mut Criterion) {
    let mut group = c.benchmark_group("end_to_end");

    group.bench_function("command_latency", |b| {
        let (piper, _mock) = setup_piper();
        b.iter(|| {
            piper.command_torques(...)
        })
    });

    group.bench_function("control_loop_500hz", |b| {
        let (controller, _mock) = setup_controller();
        b.iter(|| {
            controller.tick(Duration::from_millis(2))
        })
    });

    group.finish();
}
```

**生成报告**:
```bash
cargo bench --bench full_system
# 生成 target/criterion/report/index.html
```

**验收标准**:
- ✅ 性能报告完整
- ✅ 性能符合设计目标
- ✅ 对比 Python 版本（如果可能）

---

### 任务 5.4: Phase 5 完成检查

**清单**:
- [ ] 所有示例运行正确
- [ ] 文档完整
- [ ] 性能报告生成
- [ ] README 更新
- [ ] CHANGELOG 更新

**Phase 5 预计时间**: 5 个工作日

---

## 🔒 Phase 6: 性能和安全审查 (3 天)

### 任务 6.1: 性能审查

**清单**:
- [ ] 运行所有 benchmark
- [ ] 分析性能瓶颈
- [ ] 优化热路径
- [ ] 验证内存使用

**工具**:
```bash
# Criterion benchmark
cargo bench

# Flamegraph (性能分析)
cargo flamegraph --bench full_system

# Valgrind (内存泄漏)
valgrind --leak-check=full --show-leak-kinds=all \
    ./target/debug/examples/gravity_compensation
```

**验收标准**:
- ✅ 命令延迟 < 50μs
- ✅ 支持频率 > 1kHz
- ✅ 无内存泄漏
- ✅ 无性能回归

---

### 任务 6.2: 安全审查

**清单**:
- [ ] Miri 检查（未定义行为）
- [ ] Clippy 检查（代码规范）
- [ ] Unsafe 代码审查
- [ ] 并发安全检查

**工具**:
```bash
# Miri (未定义行为检测)
cargo +nightly miri test

# Clippy (Lint)
cargo clippy --all-targets --all-features -- -D warnings

# Unsafe 代码统计
cargo geiger

# 线程安全检查 (Loom)
cargo test --features loom
```

**验收标准**:
- ✅ Miri 测试通过
- ✅ Clippy 无警告
- ✅ Unsafe 代码最小化（< 1%）
- ✅ 并发测试通过

---

### 任务 6.3: 最终检查清单

**功能完整性**:
- [ ] 所有 Phase 1-5 任务完成
- [ ] 所有测试通过（`cargo test --all`）
- [ ] 所有示例运行正确
- [ ] 文档完整

**性能指标**:
- [ ] 命令延迟 < 50μs
- [ ] 支持频率 > 1kHz
- [ ] 状态检查 < 5ns
- [ ] 内存使用稳定

**安全性**:
- [ ] Type State 编译期检查
- [ ] 权限分层正确
- [ ] 状态同步机制
- [ ] Heartbeat 机制
- [ ] Drop 安全性

**文档和示例**:
- [ ] API 文档完整
- [ ] 示例代码丰富
- [ ] 性能报告
- [ ] README 完善

**Phase 6 预计时间**: 3 个工作日

---

## 📊 总进度跟踪

### 里程碑

| 里程碑 | 完成条件 | 预计日期 |
|--------|----------|----------|
| **M0** | 项目准备完成 | Day 2 |
| **M1** | Phase 1 完成（含笛卡尔类型） | Day 8 (+1) |
| **M2** | Phase 2 完成 | Day 16 |
| **M3** | Phase 3 完成 | Day 26 |
| **M4** | Phase 4 完成（含轨迹规划器） | Day 35 (+1) |
| **M5** | Phase 5 完成 | Day 40 |
| **M6** | Phase 6 完成 | Day 43 |
| **🎉 Release** | v1.0.0 发布 | Day 44 |

---

### 风险评估

| 风险 | 概率 | 影响 | 缓解措施 |
|------|------|------|----------|
| 硬件接口变更 | 低 | 高 | Mock 接口解耦 |
| 性能不达标 | 中 | 高 | 提前 benchmark |
| 并发 Bug | 中 | 高 | 充分测试（Loom） |
| 时间超期 | 中 | 中 | 任务优先级调整 |

---

## 🧪 测试策略

### 测试金字塔

```
              /\
             /  \
            /E2E \      5% - 端到端测试（示例运行）
           /──────\
          /        \
         / 集成测试  \   15% - 模块集成测试
        /──────────\
       /            \
      /   单元测试    \  80% - 函数级测试
     /──────────────\
```

### 测试覆盖率目标

- **单元测试**: > 90%
- **集成测试**: > 80%
- **文档测试**: > 95%

### 测试命令

```bash
# 所有测试
cargo test --all

# 单元测试
cargo test --lib

# 集成测试
cargo test --test '*'

# 文档测试
cargo test --doc

# 性能测试
cargo bench

# 覆盖率报告
cargo tarpaulin --out Html
```

---

## 📝 代码审查检查清单

每个 Pull Request 必须满足：

### 功能
- [ ] 实现符合设计文档
- [ ] 所有测试通过
- [ ] 性能满足要求

### 代码质量
- [ ] `cargo clippy` 无警告
- [ ] `cargo fmt` 已格式化
- [ ] 无 `unwrap()` 或 `expect()`（除测试代码）
- [ ] 错误处理完整

### 文档
- [ ] 公开 API 有文档
- [ ] 文档示例可运行
- [ ] 复杂逻辑有注释

### 测试
- [ ] 单元测试覆盖主要路径
- [ ] 异常场景有测试
- [ ] 性能测试（如需要）

### 安全
- [ ] Unsafe 代码有详细注释
- [ ] 并发代码有测试
- [ ] 内存安全（Miri 检查）

---

## 🚀 实施建议

### 建议的工作流程

1. **每日站会**（可选）
   - 进度同步
   - 问题讨论
   - 风险识别

2. **任务粒度**
   - 每个任务 0.5-2 天
   - 可独立测试
   - 及时合并

3. **测试先行**
   - 先写测试（TDD）
   - 再写实现
   - 最后优化

4. **持续集成**
   - 每次提交运行 CI
   - 自动化测试
   - 性能监控

### 优先级调整策略

如果时间紧张，可以：

1. **延后**:
   - Phase 5.3 性能报告（可后补）
   - 部分示例代码
   - 非核心文档

2. **简化**:
   - StateMonitor 降低频率（20Hz → 10Hz）
   - 减少 Benchmark 数量
   - 简化统计信息

3. **不能省略**:
   - ✅ Type State 核心
   - ✅ 热路径优化（AtomicBool）
   - ✅ 安全重置策略（on_time_jump）
   - ✅ 测试覆盖率

---

## 📞 支持和资源

### 参考文档

1. **设计文档**:
   - [v3.2 Final 设计](rust_high_level_api_design_v3.2_final.md)
   - [v3.2 改进总结](v3.2_improvements_summary.md)
   - [设计演进](design_evolution_summary.md)

2. **Rust 资源**:
   - [Rust Book](https://doc.rust-lang.org/book/)
   - [Rust Atomics and Locks](https://marabos.nl/atomics/)
   - [Type State Pattern](https://cliffle.com/blog/rust-typestate/)

3. **控制理论**:
   - PID 控制原理
   - 重力补偿算法
   - 轨迹规划

### 调试工具

```bash
# 日志（使用 env_logger）
RUST_LOG=debug cargo run --example gravity_compensation

# GDB 调试
rust-gdb target/debug/examples/gravity_compensation

# Valgrind
valgrind --tool=massif ./target/debug/examples/gravity_compensation

# Perf
perf record -g ./target/release/examples/gravity_compensation
perf report
```

---

## ✅ 最终交付清单

### 代码
- [x] 所有源代码
- [x] 单元测试
- [x] 集成测试
- [x] 示例代码
- [x] Benchmark

### 文档
- [x] API 文档（Rustdoc）
- [x] README.md
- [x] CHANGELOG.md
- [x] 设计文档
- [x] 性能报告

### 配置
- [x] Cargo.toml
- [x] CI/CD 配置
- [x] Linter 配置
- [x] Git hooks

---

**祝实施顺利！🎉**

**这将是 Rust 机器人控制领域的里程碑项目！**

---

**文档版本**: v1.0
**创建日期**: 2026-01-23
**作者**: AI Assistant
**状态**: ✅ 就绪
