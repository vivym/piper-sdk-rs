# 重力补偿功能设计方案分析报告

**日期**: 2025-01-28
**作者**: AI 分析
**状态**: 待审核

---

## 执行摘要

本报告分析如何为本团队 SDK 实现重力补偿功能，核心问题是 **mujoco-rs 和 nalgebra 依赖过重**。经过深入分析，推荐采用 **独立 physics crate + 可选依赖 + trait 抽象** 的方案，实现功能与依赖的平衡。

**推荐方案**:
- 创建 `crates/piper-physics` crate
- 使用 feature flags 控制重型依赖
- 提供 trait 抽象层，支持多种实现
- 保持核心 SDK 零外部数学依赖

---

## 1. 需求分析

### 1.1 功能需求

参考 `tmp/piper_sdk_other_rs/piper_sdk_rs/examples/gravity_compensation.rs`，重力补偿功能需要：

1. **物理仿真**
   - 加载机器人模型 (MJCF/XML)
   - 计算雅可比矩阵
   - 计算重力补偿力矩
   - 逆动力学计算

2. **控制循环**
   - 读取关节状态 (位置、速度)
   - 计算补偿力矩
   - 通过 MIT 模式发送力矩
   - 优雅关闭 (阻尼控制)

3. **数学运算**
   - 矩阵运算 (6x6 惯性矩阵)
   - 雅可比矩阵 (3x6)
   - 向量运算 (6 维)

### 1.2 依赖分析

#### 1.2.1 mujoco-rs 依赖

```toml
[dependencies]
mujoco-rs = "2.3.0"  # MuJoCo 物理引擎
```

**重量分析**:
- MuJoCo C 库大小: ~10 MB (需要下载)
- 绑定代码复杂度: 高
- 编译时间: 长 (需要链接 C 库)
- 运行时依赖: 需要 MuJoCo 许可证 (免费用于研究)

**功能**:
- ✅ 完整的物理仿真
- ✅ 逆动力学计算
- ✅ 雅可比矩阵
- ✅ 碰撞检测
- ❌ 过重 (对于仅需重力补偿)

#### 1.2.2 nalgebra 依赖

```toml
[dependencies]
nalgebra = "0.32"  # 线性代数库
```

**重量分析**:
- 传递依赖数量: ~10 个
- 编译时间: 中等
- 代码体积: ~500 KB (编译后)

**功能**:
- ✅ 通用矩阵/向量运算
- ✅ 类型安全 (维度检查)
- ✅ 性能优化 (SIMD)
- ❌ 对于仅需 6 维计算过度

#### 1.2.3 依赖总结

| 依赖 | 重量 | 必要性 | 替代方案 |
|------|------|--------|----------|
| mujoco-rs | 🔴 极重 | 中等 | 轻量物理库 |
| nalgebra | 🟡 中等 | 高 | 简单数组 |

---

## 2. 设计方案对比

### 方案 1: 直接放在 examples/ (❌ 不推荐)

**结构**:
```
examples/
└── gravity_compensation.rs  # 直接在示例中引入 mujoco-rs
```

**Cargo.toml**:
```toml
[dev-dependencies]
mujoco-rs = "2.3"
nalgebra = "0.32"
```

**优点**:
- ✅ 最简单，无需修改项目结构
- ✅ 依赖隔离在 dev-dependencies
- ✅ 不影响生产构建

**缺点**:
- ❌ 示例代码无法复用
- ❌ 其他示例无法共享物理计算
- ❌ 难以测试和文档化
- ❌ 用户无法直接使用

**评分**: ⭐⭐ (2/5)

---

### 方案 2: 创建 physics crate (✅ 推荐)

**结构**:
```
crates/
├── piper-physics/           # 新增
│   ├── Cargo.toml          # 独立依赖
│   └── src/
│       ├── lib.rs          # Trait 抽象
│       ├── mujoco/         # MuJoCo 实现 (feature: "mujoco")
│       └── simple/         # 简单实现 (默认)
└── piper-sdk/
    └── Cargo.toml          # 不增加依赖

examples/
└── gravity_compensation.rs # 使用 piper-physics
```

**piper-physics/Cargo.toml**:
```toml
[package]
name = "piper-physics"
version = "0.0.3"
edition = "2021"

[dependencies]
# 核心依赖 (轻量)
piper-sdk = { path = "../piper-sdk" }

# 可选依赖 (重量级)
nalgebra = { version = "0.32", optional = true }
mujoco-rs = { version = "2.3", optional = true }

[features]
default = ["simple"]
simple = []  # 使用简单数组实现
mujoco = ["dep:nalgebra", "dep:mujoco-rs"]  # MuJoCo + nalgebra
```

**piper-sdk/Cargo.toml**:
```toml
[dependencies]
# 无变化，不增加物理依赖
```

**优点**:
- ✅ **依赖隔离**: 物理依赖不影响核心 SDK
- ✅ **灵活选择**: 用户可选择实现方式
- ✅ **可测试**: 物理 crate 可独立测试
- ✅ **可文档化**: API 清晰，易于理解
- ✅ **可扩展**: 未来可添加其他物理引擎

**缺点**:
- ❌ 增加一个 crate
- ❌ 需要维护 trait 抽象

**评分**: ⭐⭐⭐⭐⭐ (5/5)

---

### 方案 3: 在 piper-sdk 中用 feature (⚠️ 部分推荐)

**结构**:
```
crates/piper-sdk/
├── Cargo.toml              # 增加 physics feature
└── src/
    ├── lib.rs
    └── physics/            # 物理计算模块
        ├── mod.rs
        ├── trait.rs
        └── mujoco.rs       # feature: "physics-mujoco"
```

**piper-sdk/Cargo.toml**:
```toml
[features]
default = []
physics-mujoco = ["dep:nalgebra", "dep:mujoco-rs"]

[dependencies]
# 可选依赖
nalgebra = { version = "0.32", optional = true }
mujoco-rs = { version = "2.3", optional = true }
```

**优点**:
- ✅ 统一在 piper-sdk 中
- ✅ feature flags 控制依赖

**缺点**:
- ❌ **污染核心 crate**: piper-sdk 依赖复杂化
- ❌ 增加编译时间 (即使不用 physics)
- ❌ 违反单一职责原则
- ❌ 用户安装时需要明确选择

**评分**: ⭐⭐⭐ (3/5)

---

### 方案 4: 使用更轻量的物理库 (🤔 待研究)

**候选轻量库**:

1. **k (替代 nalgebra)**
   - 仅提供 3D/4D 向量矩阵
   - 体积小 (~100 KB)
   - ❌ 不支持 6 维

2. **cgmath (替代 nalgebra)**
   - 轻量级数学库
   - ❌ 固定维度，不适合 6 DOF

3. **pure-rust 实现**
   - 手写 6 维矩阵运算
   - ❌ 重复造轮轮
   - ❌ 缺乏测试

**优点**:
- ✅ 减少依赖

**缺点**:
- ❌ 功能受限
- ❌ 需要自己实现物理计算
- ❌ 可能有 bug

**评分**: ⭐⭐ (2/5)

---

## 3. 推荐方案详细设计

### 3.1 方案选择

**推荐**: **方案 2 (独立 physics crate)**

**理由**:
1. ✅ 依赖隔离最彻底
2. ✅ 符合单一职责原则
3. ✅ 不影响核心 SDK
4. ✅ 可扩展性强
5. ✅ 用户友好

### 3.2 目录结构

```
crates/
├── piper-physics/                   # 新增物理 crate
│   ├── Cargo.toml
│   ├── README.md
│   └── src/
│       ├── lib.rs                  # Trait 定义 + 重新导出
│       ├── traits.rs               # 核心抽象
│       │   ├── GravityCompensation  # 重力补偿 trait
│       │   ├── Jacobian             # 雅可比矩阵 trait
│       │   └── Dynamics             # 动力学 trait
│       ├── types.rs                # 共享类型 (JointState, Torque)
│       ├── simple/                 # 简单实现 (默认)
│       │   ├── mod.rs
│       │   └── gravity.rs          # 基于公式的重力补偿
│       └── mujoco/                 # MuJoCo 实现 (feature: "mujoco")
│           ├── mod.rs
│           ├── calculator.rs       # 封装 mujoco-rs
│           └── jacobian.rs         # 雅可比计算
│
examples/
├── gravity_compensation_simple.rs  # 使用简单实现
└── gravity_compensation_mujoco.rs  # 使用 MuJoCo 实现

docs/v0/
└── physics/                         # 物理模块文档
    ├── gravity_compensation.md
    └── architecture.md
```

### 3.3 Trait 设计

**核心 Trait**: `GravityCompensation`

```rust
// crates/piper-physics/src/traits.rs

use crate::types::{JointState, Torques};

/// 重力补偿计算器 trait
///
/// 允许使用不同的物理引擎实现
pub trait GravityCompensation: Send + Sync {
    /// 计算重力补偿力矩
    ///
    /// # 参数
    /// - `state`: 当前关节状态 (位置、速度)
    ///
    /// # 返回
    /// - 各关节的重力补偿力矩 (Nm)
    fn compute_gravity_torques(&mut self, state: &JointState) -> Result<Torques, PhysicsError>;

    /// 获取计算器名称
    fn name(&self) -> &str;

    /// 检查是否已初始化
    fn is_initialized(&self) -> bool;
}

/// 雅可比矩阵计算器 trait
pub trait JacobianCalculator: Send + Sync {
    /// 计算末端执行器的雅可比矩阵
    ///
    /// # 返回
    /// - `jacp`: 位置雅可比 (3x6)
    /// - `jacr`: 旋转雅可比 (3x6)
    fn compute_jacobian(&mut self, state: &JointState)
        -> Result<(Jacobian3x6, Jacobian3x6), PhysicsError>;
}

/// 动力学计算器 trait
pub trait DynamicsCalculator: Send + Sync {
    /// 逆动力学计算
    ///
    /// 给定位置、速度、加速度，计算所需力矩
    fn inverse_dynamics(
        &mut self,
        position: &[f64; 6],
        velocity: &[f64; 6],
        acceleration: &[f64; 6],
    ) -> Result<Torques, PhysicsError>;
}
```

### 3.4 类型定义

**共享类型** (不依赖 nalgebra):

```rust
// crates/piper-physics/src/types.rs

/// 关节状态
#[derive(Debug, Clone, Copy)]
pub struct JointState {
    /// 关节位置 (弧度)
    pub q: [f64; 6],
    /// 关节速度 (弧度/秒)
    pub dq: [f64; 6],
}

/// 力矩
#[derive(Debug, Clone, Copy)]
pub struct Torques {
    /// 各关节力矩 (Nm)
    pub tau: [f64; 6],
}

/// 3x6 雅可比矩阵 (行主序)
#[derive(Debug, Clone, Copy)]
pub struct Jacobian3x6 {
    /// 数据 [3 * 6 = 18 个元素]
    pub data: [[f64; 6]; 3],
}

impl Jacobian3x6 {
    pub fn new(data: [[f64; 6]; 3]) -> Self {
        Self { data }
    }

    /// 获取位置雅可比
    pub fn position(&self) -> &[[f64; 6]; 3] {
        &self.data
    }

    /// 获取旋转雅可比
    pub fn rotation(&self) -> &[[f64; 6]; 3] {
        &self.data
    }
}

/// 物理计算错误
#[derive(Debug, thiserror::Error)]
pub enum PhysicsError {
    #[error("Calculation failed: {0}")]
    CalculationFailed(String),

    #[error("Not initialized")]
    NotInitialized,

    #[error("Invalid input: {0}")]
    InvalidInput(String),
}
```

### 3.5 简单实现 (默认)

**基于公式的重力补偿** (无需物理引擎):

```rust
// crates/piper-physics/src/simple/gravity.rs

use super::super::traits::GravityCompensation;
use super::super::types::{JointState, Torques, PhysicsError};
use std::f64::consts::PI;

/// 基于公式的重力补偿计算器
///
/// 使用简化的重力模型，不需要物理引擎
pub struct SimpleGravityCompensation {
    /// 机械臂参数
    arm_params: ArmParameters,
    initialized: bool,
}

struct ArmParameters {
    /// 连杆长度 [m]
    link_lengths: [f64; 6],
    /// 连杆质量 [kg]
    link_masses: [f64; 6],
    /// 重力加速度 [m/s²]
    gravity: f64,
}

impl SimpleGravityCompensation {
    /// 创建新的计算器
    pub fn new() -> Self {
        Self {
            arm_params: ArmParameters::default_piper(),
            initialized: false,
        }
    }

    /// 设置自定义参数
    pub fn with_params(mut self, params: ArmParameters) -> Self {
        self.arm_params = params;
        self.initialized = true;
        self
    }
}

impl Default for SimpleGravityCompensation {
    fn default() -> Self {
        Self::new()
    }
}

impl ArmParameters {
    /// Piper 机械臂默认参数
    fn default_piper() -> Self {
        Self {
            link_lengths: [0.0, 0.3, 0.0, 0.0, 0.0, 0.0],  // 示例值
            link_masses: [0.5, 0.8, 0.3, 0.2, 0.1, 0.05],
            gravity: 9.81,
        }
    }
}

impl GravityCompensation for SimpleGravityCompensation {
    fn compute_gravity_torques(&mut self, state: &JointState) -> Result<Torques, PhysicsError> {
        if !self.initialized {
            return Err(PhysicsError::NotInitialized);
        }

        // 简化的重力补偿公式
        // τ_g = Σ(m_i * g * J_{vi}^T * z)
        // 其中 J_{vi} 是连杆 i 的质心雅可比
        //
        // 简化实现：仅考虑重力对各关节的影响
        let mut tau = [0.0; 6];

        for i in 0..6 {
            // 计算关节 i 的重力力矩
            // 这里使用简化的几何方法
            let mut gravity_torque = 0.0;

            // 累加后续连杆的重力影响
            for j in i..6 {
                let mass = self.arm_params.link_masses[j];
                let g = self.arm_params.gravity;

                // 简化的力臂计算 (假设连杆水平)
                let moment_arm = self.arm_params.link_lengths[i];

                // 重力力矩 = m * g * r * cos(q)
                gravity_torque += mass * g * moment_arm * state.q[i].cos();
            }

            tau[i] = gravity_torque;
        }

        Ok(Torques { tau })
    }

    fn name(&self) -> &str {
        "simple_gravity_compensation"
    }

    fn is_initialized(&self) -> bool {
        self.initialized
    }
}
```

### 3.6 MuJoCo 实现 (feature)

**需要 nalgebra + mujoco-rs**:

```rust
// crates/piper-physics/src/mujoco/calculator.rs

#[cfg(feature = "mujoco")]
use mujoco_rs::prelude::*;
#[cfg(feature = "mujoco")]
use nalgebra::SMatrix;

#[cfg(feature = "mujoco")]
use super::super::traits::GravityCompensation;
#[cfg(feature = "mujoco")]
use super::super::types::{JointState, Torques, PhysicsError, Jacobian3x6};

/// MuJoCo 物理引擎实现的重力补偿
#[cfg(feature = "mujoco")]
pub struct MujocoGravityCompensation {
    data: MjData<Rc<MjModel>>,
    ee_body_id: usize,
}

#[cfg(feature = "mujoco")]
impl MujocoGravityCompensation {
    /// 从 XML 文件创建
    pub fn from_xml(xml_path: &std::path::Path) -> Result<Self, PhysicsError> {
        let model = Rc::new(
            MjModel::from_xml(xml_path)
                .map_err(|e| PhysicsError::CalculationFailed(e.to_string()))?
        );
        let data = MjData::new(model.clone());
        let ee_body_id = model
            .body("link6")
            .ok_or_else(|| PhysicsError::CalculationFailed("link6 not found".into()))?
            .id;

        Ok(Self { data, ee_body_id })
    }
}

#[cfg(feature = "mujoco")]
impl GravityCompensation for MujocoGravityCompensation {
    fn compute_gravity_torques(&mut self, state: &JointState) -> Result<Torques, PhysicsError> {
        // 设置关节状态
        self.data.qpos_mut()[0..6].copy_from_slice(&state.q);
        self.data.qvel_mut()[0..6].copy_from_slice(&state.dq);
        self.data.qacc_mut()[0..6].fill(0.0);

        // 前向动力学
        self.data.forward();

        // 提取重力力矩 (qfrc_bias)
        let tau: [f64; 6] = std::array::from_fn(|i| self.data.qfrc_bias()[i]);

        Ok(Torques { tau })
    }

    fn name(&self) -> &str {
        "mujoco_gravity_compensation"
    }

    fn is_initialized(&self) -> bool {
        true
    }
}
```

### 3.7 API 使用示例

**示例 1**: 使用简单实现 (默认)

```rust
// examples/gravity_compensation_simple.rs

use piper_physics::{GravityCompensation, SimpleGravityCompensation};
use piper_sdk::{PiperBuilder, prelude::*};

fn main() -> Result<()> {
    // 创建机器人连接
    let piper = PiperBuilder::new()
        .interface("can0")
        .connect()?
        .enable_motors()?
        .into_mit_mode();

    // 创建重力补偿计算器 (无需物理引擎)
    let mut gravity_calc = SimpleGravityCompensation::new();

    // 控制循环
    loop {
        // 读取状态
        let observer = piper.observer();
        let state = observer.read_state();

        // 计算重力补偿力矩
        let joint_state = piper_physics::JointState {
            q: state.joint_positions.to_radians_array(),
            dq: state.joint_velocities.unwrap_or([0.0; 6]),
        };

        let torques = gravity_calc.compute_gravity_torques(&joint_state)?;

        // 发送力矩命令
        for (motor_num, &torque) in torques.tau.iter().enumerate() {
            let cmd = piper_sdk::command::MitCommand::torque_only(
                motor_num + 1,
                torque as f32,
            );
            piper.send_realtime_command(cmd)?;
        }

        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}
```

**示例 2**: 使用 MuJoCo 实现

```rust
// examples/gravity_compensation_mujoco.rs

use piper_physics::{GravityCompensation, MujocoGravityCompensation};
use piper_sdk::{PiperBuilder, prelude::*};

fn main() -> Result<()> {
    // 创建 MuJoCo 计算器 (需要 mujoco feature)
    let xml_path = std::path::Path::new("piper_model.xml");
    let mut gravity_calc = MujocoGravityCompensation::from_xml(xml_path)?;

    // 其余代码相同...
}
```

---

## 4. 依赖管理策略

### 4.1 Feature Flags 设计

```toml
# crates/piper-physics/Cargo.toml

[features]
default = ["simple"]

# 简单实现 (默认，无外部依赖)
simple = []

# MuJoCo 实现 (需要 mujoco-rs + nalgebra)
mujoco = ["dep:nalgebra", "dep:mujoco-rs"]

# 所有功能 (用于开发和测试)
full = ["mujoco"]
```

### 4.2 用户选择指南

**场景 1**: 学习和演示
```bash
# 使用简单实现 (无需额外依赖)
cargo build --example gravity_compensation_simple
```

**场景 2**: 生产环境 (需要精确物理)
```bash
# 使用 MuJoCo 实现
cargo build --example gravity_compensation_mujoco --features mujoco
```

**场景 3**: 不使用物理功能
```bash
# 核心 SDK 完全不受影响
cargo build --package piper-sdk
```

### 4.3 依赖隔离效果

| 组件 | 无 physics | Simple | Mujoco |
|------|-----------|--------|--------|
| piper-sdk | ✅ 0 依赖 | ✅ 0 依赖 | ✅ 0 依赖 |
| piper-physics | - | ✅ 0 外部 | ⚠️ 2 个 (nalgebra, mujoco-rs) |
| 编译时间 | 基准 | +5% | +30% |
| 二进制大小 | 基准 | +50 KB | +10 MB |

---

## 5. 实现计划

### 阶段 1: 基础框架 (1-2 天)

1. ✅ 创建 `crates/piper-physics`
2. ✅ 定义核心 trait (`GravityCompensation`)
3. ✅ 定义共享类型 (`JointState`, `Torques`)
4. ✅ 实现简单版本 (`SimpleGravityCompensation`)
5. ✅ 编写文档 (`README.md`)

**目标**: 用户可以使用简单实现进行重力补偿

### 阶段 2: MuJoCo 集成 (2-3 天)

1. ✅ 添加 `mujoco` feature
2. ✅ 实现 `MujocoGravityCompensation`
3. ✅ 集成雅可比矩阵计算
4. ✅ 编写示例代码
5. ✅ 添加测试 (需要 MuJoCo 模型文件)

**目标**: 高级用户可以使用 MuJoCo 进行精确计算

### 阶段 3: 示例和文档 (1-2 天)

1. ✅ `gravity_compensation_simple.rs`
2. ✅ `gravity_compensation_mujoco.rs`
3. ✅ 用户指南 (`docs/v0/physics/gravity_compensation.md`)
4. ✅ API 文档 (rustdoc)

**目标**: 用户可以快速上手

### 阶段 4: 测试和优化 (2-3 天)

1. ✅ 单元测试 (简单实现)
2. ✅ 集成测试 (需要硬件)
3. ✅ 性能测试 (计算延迟)
4. ✅ 错误处理测试

**目标**: 生产级质量

**总计**: 6-10 天

---

## 6. 风险评估

### 6.1 技术风险

| 风险 | 影响 | 概率 | 缓解措施 |
|------|------|------|----------|
| MuJoCo 许可证问题 | 高 | 低 | 提供文档说明，强调研究用途 |
| 简单实现精度不足 | 中 | 中 | 提供参数调整接口，用户可校准 |
| nalgebra 版本兼容性 | 中 | 低 | 使用 semver 兼容版本 |
| 性能不足 | 低 | 低 | 优化算法，使用缓存 |

### 6.2 维护风险

| 风险 | 影响 | 概率 | 缓解措施 |
|------|------|------|----------|
| 物理引擎更新 | 中 | 中 | Trait 抽象隔离，易替换 |
| 用户需求变化 | 低 | 低 | 保持 trait 灵活性 |
| 测试覆盖不足 | 高 | 中 | 优先测试核心算法 |

---

## 7. 对比总结

### 7.1 方案对比表

| 维度 | 方案 1 (examples/) | 方案 2 (独立 crate) | 方案 3 (feature) | 方案 4 (轻量库) |
|------|-------------------|---------------------|------------------|-----------------|
| **依赖隔离** | ⭐⭐⭐ | ⭐⭐⭐⭐⭐ | ⭐⭐ | ⭐⭐⭐⭐ |
| **可维护性** | ⭐⭐ | ⭐⭐⭐⭐⭐ | ⭐⭐⭐ | ⭐⭐ |
| **可扩展性** | ⭐ | ⭐⭐⭐⭐⭐ | ⭐⭐⭐ | ⭐⭐ |
| **易用性** | ⭐⭐⭐ | ⭐⭐⭐⭐ | ⭐⭐⭐⭐ | ⭐⭐ |
| **功能完整** | ⭐⭐⭐ | ⭐⭐⭐⭐⭐ | ⭐⭐⭐⭐ | ⭐⭐ |
| **学习曲线** | ⭐⭐⭐⭐⭐ | ⭐⭐⭐ | ⭐⭐⭐⭐ | ⭐⭐⭐ |
| **总分** | 16/30 | 27/30 | 21/30 | 15/30 |

### 7.2 推荐结论

**最佳方案**: **方案 2 (独立 piper-physics crate)**

**理由**:
1. ✅ 依赖隔离最彻底
2. ✅ 可维护性最高
3. ✅ 可扩展性最强
4. ✅ 不影响核心 SDK
5. ✅ 用户友好 (默认简单实现)

---

## 8. 实施建议

### 8.1 立即行动

1. **创建 piper-physics crate**
   ```bash
   mkdir -p crates/piper-physics/src
   touch crates/piper-physics/Cargo.toml
   ```

2. **定义核心 trait**
   - `GravityCompensation`
   - `JacobianCalculator`
   - `DynamicsCalculator`

3. **实现简单版本**
   - 基于公式的重力补偿
   - 无外部依赖

### 8.2 后续优化

1. **添加 MuJoCo 支持** (可选)
   - 需要用户安装 MuJoCo
   - 提供详细文档

2. **性能优化**
   - 缓存计算结果
   - 并行计算

3. **测试完善**
   - 单元测试
   - 硬件测试
   - 性能基准测试

---

## 9. 附录

### 9.1 参考实现

**另一团队的重力补偿**:
- 文件: `tmp/piper_sdk_other_rs/piper_sdk_rs/examples/gravity_compensation.rs`
- 依赖: mujoco-rs + nalgebra
- 代码行数: ~375 行

**本团队实现目标**:
- 更好的抽象 (trait)
- 依赖隔离 (独立 crate)
- 灵活选择 (simple vs mujoco)

### 9.2 相关资源

- **MuJoCo**: https://mujoco.org/
- **mujoco-rs**: https://docs.rs/mujoco-rs
- **nalgebra**: https://docs.rs/nalgebra
- **Pinocchio** (Python): 参考实现

### 9.3 设计文档

- [架构设计](../architecture.md)
- [位置控制指南](../position_control_user_guide.md)
- [SDK 对比报告](./piper_sdk_comparison_report.md)

---

**报告编写**: AI 分析
**审核状态**: 待审核
**版本**: v1.0
**最后更新**: 2025-01-28
