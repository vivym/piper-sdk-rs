# 重力补偿实施检查清单（v2.1）

**日期**: 2025-01-28
**版本**: v2.1
**状态**: 可用于指导开发

---

## 🔴 极其重要的安全检查

在开始实施前，请确认以下**关键点**：

### 1. 数学算法验证

- [ ] **确认使用 RNE 算法**，而非简单累加
  - ❌ `m * g * r * cos(q)` 对 6-DOF 机械臂是**错误的**
  - ✅ 必须使用递归牛顿-欧拉算法（RNE）
  - ✅ 推荐使用 `k` crate 的实现

### 2. 关节映射验证

- [ ] **初始化时验证 CAN ID 与 URDF 顺序一致**
  ```rust
  let gravity_calc = AnalyticalGravityCompensation::from_piper_urdf()?;
  // 输出应该显示：
  //   Joint 1 (CAN ID 1): joint_1
  //   Joint 2 (CAN ID 2): joint_2
  ```
  - ⚠️ 如果映射错误，机器人会失控

### 3. API 参数传递

- [ ] **使用 `as_slice()` 而非直接传递 Vector6**
  ```rust
  // ❌ 错误
  self.chain.set_joint_positions(q);

  // ✅ 正确
  self.chain.set_joint_positions(q.as_slice());
  ```

---

## 📋 实施检查清单

### 阶段 1: 创建 crate（第 1 天）

- [ ] 创建 `crates/piper-physics` 目录
- [ ] 创建 `Cargo.toml`（包含 nalgebra 必选依赖）
- [ ] 在 `lib.rs` 中 re-export nalgebra
- [ ] 添加 `assets/` 目录用于存放 URDF

```bash
mkdir -p crates/piper-physics/src
mkdir -p crates/piper-physics/assets
cd crates/piper-physics
cargo init --lib
```

### 阶段 2: 核心类型（第 1-2 天）

- [ ] 定义类型别名（`types.rs`）
- [ ] 定义 Trait（`traits.rs`）
- [ ] 添加错误类型（`PhysicsError`）

**关键检查**:
```rust
// lib.rs
pub use nalgebra;  // 🌟 Re-export 必须

// types.rs
pub type JointState = nalgebra::Vector6<f64>;
pub type Jacobian3x6 = nalgebra::Matrix3x6<f64>;
```

### 阶段 3: 解析法实现（第 2-4 天）

- [ ] 集成 `k` crate
- [ ] 实现 `AnalyticalGravityCompensation`
- [ ] 实现 `from_urdf_with_validation`（包含关节映射验证）
- [ ] 实现 `from_default_piper`（使用 include_str!）
- [ ] 添加 helper 方法（as_slice 封装）
- [ ] 准备多个 URDF 文件（空载、带夹爪等）

**关键检查**:
```rust
// 验证 slice 转换
self.chain.set_joint_positions(q.as_slice());

// 验证关节映射
Self::validate_joint_mapping(&chain)?;

// 嵌入 URDF
const DEFAULT_PIPER_URDF: &str = include_str!("../../../assets/piper_description.urdf");
```

### 阶段 4: 测试（第 4-5 天）

- [ ] 单元测试（RNE 算法验证）
- [ ] 集成测试（需要硬件）
- [ ] 关节映射测试（验证映射正确性）

### 阶段 5: 示例代码（第 5-6 天）

- [ ] `gravity_compensation_analytical.rs`
- [ ] 用户文档

---

## 🚨 实施陷阱

### 陷阱 1: 忘记 Slice 参数

**错误代码**:
```rust
self.chain.set_joint_positions(q);  // q 是 Vector6
```

**正确代码**:
```rust
self.chain.set_joint_positions(q.as_slice());  // 转为 &[f64]
```

**检测**: 编译时会报错（类型不匹配）

### 陷阱 2: 忘记关节映射验证

**错误**: 假设 CAN ID 顺序 = URDF 顺序

**正确**: 初始化时验证并打印

**检测**: 首次运行时检查输出是否正确

### 陷阱 3: 忘记 URDF 嵌入

**错误**: 运行时查找 URDF 文件（可能不存在）

**正确**: 编译时嵌入（`include_str!`）

---

## ✅ 验收标准

### 数学正确性

- [ ] RNE 算法实现（非简单累加）
- [ ] 关节映射验证通过
- [ ] 单元测试覆盖（与 MuJoCo 对比）

### API 易用性

- [ ] 用户只需调用 `from_piper_urdf()`
- [ ] 验证自动执行（无需手动调用）
- [ ] 错误信息清晰（包含具体问题）

### 依赖管理

- [ ] nalgebra re-export 正确
- [ ] piper-sdk 零物理依赖
- [ ] 版本锁定（nalgebra = "0.32"）

---

## 📚 参考文档顺序

### 必读（按顺序）

1. **v1_to_v2_changes.md** (5 分钟)
   - 了解关键修正

2. **gravity_compensation_design_v2.md** (30 分钟)
   - 完整设计文档
   - **重点关注**: 第 6 节"实现注意事项"

3. **gravity_compensation_quick_decision.md** (5 分钟)
   - 快速决策指南

### 可选

4. **piper_sdk_comparison_report.md** (30 分钟)
   - SDK 对比分析

---

## 🎯 快速开始

### 最简单的实现

```rust
// 用户代码（最简单）

use piper_physics::AnalyticalGravityCompensation;

fn main() -> Result<()> {
    // 一行代码创建（包含验证）
    let mut gravity_calc = AnalyticalGravityCompensation::from_piper_urdf()?;

    // 计算力矩
    let q = nalgebra::Vector6::zeros();
    let torques = gravity_calc.compute_gravity_torques(&q, None)?;

    Ok(())
}
```

### 完整示例

```rust
use piper_physics::AnalyticalGravityCompensation;
use piper_sdk::PiperBuilder;

fn main() -> Result<()> {
    // 1. 创建机器人
    let piper = PiperBuilder::new()
        .interface("can0")
        .connect()?
        .enable_motors()?
        .into_mit_mode();

    // 2. 创建重力补偿（自动验证）
    // 注意：末端负载通过 URDF 文件配置
    let mut gravity_calc = AnalyticalGravityCompensation::from_piper_urdf()?;

    // 3. 控制循环
    loop {
        let state = piper.observer().read_state();
        let q = nalgebra::Vector6::from_iterator(
            state.joint_positions.iter().map(|p| p.as_radians())
        );
        let torques = gravity_calc.compute_gravity_torques(&q, None)?;

        for (i, &torque) in torques.iter().enumerate() {
            let cmd = piper_sdk::command::MitCommand::torque_only(i + 1, torque as f32);
            piper.send_realtime_command(cmd)?;
        }

        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}
```

---

## 🔧 开发工具检查

### 编译检查

```bash
# 检查 nalgebra re-export
cargo doc --package piper-physics --open

# 应该能看到 nalgebra 的文档
```

### 运行时检查

```bash
# 运行示例
cargo run --example gravity_compensation_analytical

# 应该看到验证输出：
# 🔍 Validating joint mapping...
#   Joint 1 (CAN ID 1): joint_1
#   Joint 2 (CAN ID 2): joint_2
# ✓ Joint mapping validation complete
```

---

## 📞 问题诊断

### 问题 1: 编译错误 "type mismatch"

**原因**: 忘记使用 `as_slice()`

**解决**:
```rust
// 检查所有 k crate API 调用
self.chain.set_joint_positions(q.as_slice());
```

### 问题 2: 机器人"发疯"（动作异常）

**原因**: 关节映射错误

**解决**:
```rust
// 检查初始化输出
let gravity_calc = AnalyticalGravityCompensation::from_piper_urdf()?;
// 确认 "Joint X (CAN ID X)" 顺序正确
```

### 问题 3: 编译时找不到 URDF

**原因**: URDF 文件路径错误

**解决**:
```bash
# 检查文件是否存在
ls -la crates/piper-physics/assets/piper_description.urdf
```

---

## ✅ 最终检查清单

**实施前**:
- [ ] 阅读 v1_to_v2_changes.md
- [ ] 阅读 gravity_compensation_design_v2.md 的第 6 节
- [ ] 确认理解 RNE 算法
- [ ] 确认理解关节映射验证

**实施中**:
- [ ] nalgebra re-export 正确
- [ ] 使用 as_slice() 传递参数
- [ ] 实现关节映射验证
- [ ] 使用 include_str! 嵌入 URDF

**实施后**:
- [ ] 编译通过
- [ ] 初始化验证输出正确
- [ ] 单元测试通过
- [ ] 硬件测试时机器人行为正常

---

**报告版本**: v2.1
**最后更新**: 2025-01-28
**维护者**: AI
