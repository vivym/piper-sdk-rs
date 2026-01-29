# 重力补偿功能设计 - 快速决策指南

## 核心问题

**问题**: mujoco-rs (~10 MB) + nalgebra (~500 KB) 依赖过重

**影响**:
- 污染核心 SDK
- 增加编译时间
- 用户被迫安装不需要的依赖

## 推荐方案 ✅

### 创建独立 `piper-physics` crate

```
crates/
├── piper-physics/           # 新增 (独立依赖)
│   ├── simple/             # 默认实现 (无外部依赖)
│   └── mujoco/             # MuJoCo 实现 (可选 feature)
└── piper-sdk/              # 核心SDK (无变化，零物理依赖)
```

### 依赖隔离效果

| 组件 | piper-sdk | piper-physics (simple) | piper-physics (mujoco) |
|------|-----------|----------------------|------------------------|
| 外部依赖 | 0 个 | 0 个 | 2 个 (nalgebra, mujoco-rs) |
| 编译时间 | 基准 | +5% | +30% |
| 二进制大小 | 基准 | +50 KB | +10 MB |

## 用户使用指南

### 场景 1: 学习和演示 (90% 用户)

```bash
# 使用简单实现，无需 mujoco
cargo run --example gravity_compensation_simple
```

**特点**:
- ✅ 零外部依赖
- ✅ 快速编译
- ✅ 基于物理公式
- ⚠️ 精度略低 (5-10% 误差)

### 场景 2: 生产环境 (10% 用户)

```bash
# 使用 MuJoCo，高精度
cargo run --example gravity_compensation_mujoco --features mujoco
```

**特点**:
- ✅ 高精度 (<1% 误差)
- ✅ 完整物理仿真
- ❌ 需要安装 MuJoCo
- ❌ 编译时间长

## API 对比

### 另一团队 SDK (耦合)

```rust
// 必须使用 mujoco-rs + nalgebra
use mujoco_rs::prelude::*;
use nalgebra::SMatrix;

let mut calc = GravityCompensationCalculator::new(xml_path)?;
let torques = calc.compute_torques(&angles, &velocities)?;
```

**问题**:
- ❌ 强制依赖 mujoco-rs
- ❌ 无法替代实现
- ❌ 学习成本高

### 本团队 SDK (解耦)

```rust
// 默认使用简单实现 (无外部依赖)
use piper_physics::{SimpleGravityCompensation, GravityCompensation};

let mut calc = SimpleGravityCompensation::new();
let torques = calc.compute_gravity_torques(&state)?;

// 或者使用 MuJoCo (可选)
#[cfg(feature = "mujoco")]
use piper_physics::{MujocoGravityCompensation, GravityCompensation};

let mut calc = MujocoGravityCompensation::from_xml(xml_path)?;
let torques = calc.compute_gravity_torques(&state)?;
```

**优势**:
- ✅ 用户可选实现方式
- ✅ 核心 SDK 零物理依赖
- ✅ trait 抽象，易扩展

## 方案对比

| 方案 | 优点 | 缺点 | 评分 |
|------|------|------|------|
| **1. examples/** | 简单 | 无法复用 | ⭐⭐ (2/5) |
| **2. 独立 crate** | **隔离彻底** | 增加一个 crate | **⭐⭐⭐⭐⭐ (5/5)** |
| 3. feature | 统一管理 | 污染核心 SDK | ⭐⭐⭐ (3/5) |
| 4. 轻量库 | 减少依赖 | 功能受限 | ⭐⭐ (2/5) |

## 实施计划

### 阶段 1: 基础框架 (1-2 天)

- [ ] 创建 `crates/piper-physics`
- [ ] 定义 `GravityCompensation` trait
- [ ] 实现 `SimpleGravityCompensation`
- [ ] 编写文档

**交付**: 用户可以使用简单实现

### 阶段 2: MuJoCo 集成 (2-3 天)

- [ ] 添加 `mujoco` feature
- [ ] 实现 `MujocoGravityCompensation`
- [ ] 编写示例代码
- [ ] 添加测试

**交付**: 高级用户可以使用 MuJoCo

### 阶段 3: 示例和文档 (1-2 天)

- [ ] `gravity_compensation_simple.rs`
- [ ] `gravity_compensation_mujoco.rs`
- [ ] 用户指南

**交付**: 用户可以快速上手

**总计**: 6-10 天

## 快速开始

### 1. 创建 crate

```bash
mkdir -p crates/piper-physics/src
cd crates/piper-physics
cargo init --lib
```

### 2. Cargo.toml

```toml
[package]
name = "piper-physics"
version = "0.0.3"
edition = "2021"

[dependencies]
piper-sdk = { path = "../piper-sdk" }

# 可选依赖
nalgebra = { version = "0.32", optional = true }
mujoco-rs = { version = "2.3", optional = true }

[features]
default = ["simple"]
simple = []
mujoco = ["dep:nalgebra", "dep:mujoco-rs"]
```

### 3. 定义 trait

```rust
// src/lib.rs

pub trait GravityCompensation: Send + Sync {
    fn compute_gravity_torques(&mut self, state: &JointState) -> Result<Torques>;
    fn name(&self) -> &str;
}
```

### 4. 实现简单版本

```rust
// src/simple.rs

impl GravityCompensation for SimpleGravityCompensation {
    fn compute_gravity_torques(&mut self, state: &JointState) -> Result<Torques> {
        // 基于物理公式计算 (无需外部依赖)
        Ok(calculate_gravity(&state.q))
    }
}
```

## 关键决策

### ✅ 推荐

1. **独立 crate** - 彻底隔离依赖
2. **Trait 抽象** - 支持多种实现
3. **默认简单** - 90% 用户无需 mujoco
4. **可选 MuJoCo** - 10% 高级用户按需启用

### ❌ 不推荐

1. 放在 examples/ - 无法复用
2. 污染 piper-sdk - 违反单一职责
3. 仅用轻量库 - 功能受限

## 常见问题

**Q: 为什么不直接用 mujoco-rs?**

A: 因为 90% 用户只需要基础重力补偿，不需要完整物理仿真。强制依赖 mujoco-rs 会：
- 增加编译时间 30%
- 增加二进制大小 10 MB
- 需要安装 MuJoCo 许可证

**Q: 简单实现精度够吗?**

A: 对于教学、演示、原型开发，完全够用。误差通常在 5-10% 范围内。
如果需要高精度 (<1%)，可启用 mujoco feature。

**Q: 可以用其他物理引擎吗?**

A: 可以！trait 抽象支持任何实现：
- Pinocchio (未来可集成)
- Robotics Toolbox (未来可集成)
- 用户自定义实现

**Q: 影响核心 SDK 吗?**

A: 完全不影响！piper-physics 是独立 crate，piper-sdk 零物理依赖。

## 下一步行动

1. **阅读详细报告**: `gravity_compensation_design_analysis.md`
2. **创建 crate**: 按照上述步骤创建 `piper-physics`
3. **实现 trait**: 先实现简单版本
4. **测试验证**: 确保功能正常

## 总结

**推荐方案**: 独立 `piper-physics` crate

**核心优势**:
- ✅ 依赖隔离 (核心 SDK 零污染)
- ✅ 灵活选择 (simple vs mujoco)
- ✅ 易于扩展 (trait 抽象)
- ✅ 用户友好 (默认零外部依赖)

**实施时间**: 6-10 天

---

**相关文档**:
- 详细设计: `gravity_compensation_design_analysis.md`
- 架构设计: `../architecture.md`
- SDK 对比: `piper_sdk_comparison_report.md`
