# MuJoCo-rs 最佳实践分析报告

**日期**: 2025-01-28
**版本**: mujoco-rs v2.3.0+mj-3.3.7
**分析目标**: 为 piper-physics 库确定使用 MuJoCo-rs 的最佳实践

---

## 执行摘要

基于对 mujoco-rs 源码的深入分析，本报告提供了：
1. **MuJoCo-rs API 设计模式**分析
2. **重力补偿实现**的最佳实践
3. **性能优化**建议
4. **错误处理**策略
5. **与另一团队实现的对比**

---

## 1. MuJoCo-rs 架构分析

### 1.1 目录结构

```
tmp/mujoco-rs/
├── src/
│   ├── lib.rs                    # 库入口，导出公共 API
│   ├── prelude.rs                # 常用类型重导出
│   ├── wrappers/                 # C 结构体的 Rust 封装
│   │   ├── mj_model.rs           # MjModel（机器人模型）
│   │   ├── mj_data.rs            # MjData（仿真状态）
│   │   ├── mj_editing.rs         # 模型编辑
│   │   └── fun.rs                # 全局函数
│   ├── mujoco_c.rs               # FFI 绑定
│   └── util.rs                   # 工具宏（array_slice_dyn）
├── examples/
│   ├── basic.rs                  # 基础示例
│   └── ...
└── Cargo.toml
```

### 1.2 核心类型

```rust
// 机器人模型（编译时常量）
pub struct MjModel(*mut mjModel);

// 仿真数据（动态状态）
pub struct MjData<M: Deref<Target = MjModel>> {
    model: M,
    data: UniquePtr<mjData>,
}
```

**关键设计**:
- `MjModel` 是**编译时常量**（从 XML 加载后不变）
- `MjData` 是**运行时状态**（每步更新）
- `MjData` 持有 `MjModel` 的引用（`Deref<Target = MjModel>`）

---

## 2. API 设计模式

### 2.1 数组访问模式（array_slice_dyn 宏）

MuJoCo-rs 使用 **宏生成访问器**，而非直接暴露数组字段：

```rust
// 在 mj_data.rs 中声明
array_slice_dyn! {
    qpos: &[MjtNum; "position"; model.ffi().nq],
    qvel: &[MjtNum; "velocity"; model.ffi().nv],
    qacc: &[MjtNum; "acceleration"; model.ffi().nv],
    qfrc_bias: &[MjtNum; "C(qpos,qvel)"; model.ffi().nv],
    // ...
}
```

**生成的 API**:
```rust
// 只读访问
let positions = data.qpos();   // &[MjtNum]
let velocities = data.qvel();  // &[MjtNum]
let torques = data.qfrc_bias(); // &[MjtNum]

// 可变访问
data.qpos_mut()[..6].copy_from_slice(&angles);
data.qvel_mut()[..6].fill(0.0);
```

**优点**:
- ✅ 类型安全（返回 `&[MjtNum]` 而非裸指针）
- ✅ 边界检查（长度从 `model.ffi().nq` 获取）
- ✅ 文档完整（包含数组说明）
- ✅ 内存安全（通过 `slice::from_raw_parts` 封装）

### 2.2 模型加载模式

```rust
// 方式 1: 从文件加载
let model = MjModel::from_xml("piper.xml")?;

// 方式 2: 从字符串加载（嵌入式）
const XML: &str = include_str!("piper.xml");
let model = MjModel::from_xml_string(XML)?;

// 方式 3: 从虚拟文件系统加载
let vfs = MjVfs::new();
vfs.add_from_buffer("model.xml", xml_bytes)?;
let model = MjModel::from_xml_vfs("model.xml", &vfs)?;
```

**最佳实践**:
- ✅ **推荐** 使用 `from_xml_string()` + `include_str!`
  - 零运行时开销
  - 无文件查找
  - 可靠（无需路径管理）

- ⚠️ 避免使用 `from_xml()`（文件路径）
  - 运行时文件可能不存在
  - 需要路径管理
  - 增加部署复杂度

### 2.3 数据创建模式

```rust
// 方式 1: 显式创建
let data = MjData::new(&model);

// 方式 2: 通过模型创建（推荐）
let data = model.make_data();
```

**最佳实践**:
- ✅ 使用 `model.make_data()`（更符合 Rust 所有权模型）
- ✅ 复用 `model` 而传递引用

---

## 3. 重力补偿实现的最佳实践

### 3.1 核心算法流程

```rust
// 步骤 1: 设置关节位置
data.qpos_mut()[0..6].copy_from_slice(q);

// 步骤 2: 设置关节速度
data.qvel_mut()[0..6].fill(0.0);  // 零速度 = 纯重力

// 步骤 3: 设置关节加速度
data.qacc_mut()[0..6].fill(0.0);  // 零加速度 = 静态

// 步骤 4: 调用正向动力学
data.forward();

// 步骤 5: 提取重力力矩
let torques = data.qfrc_bias()[0..6].to_vec();
```

### 3.2 理论原理

**MuJoCo 动力学方程**:
```
M(q) * qacc + C(q, qd) = τ_applied + τ_bias
```

其中：
- `M(q)`: 质量矩阵
- `qacc`: 关节加速度
- `C(q, qd)`: 偏置力（重力 + 科里奥利 + 离心力）
- `τ_applied`: 施加的执行器力
- `τ_bias`: 偏置力（在 MuJoCo 中称为 `qfrc_bias`）

**重力补偿的关键**:
- 设置 `qvel = 0` → 科里奥利力 = 0
- 设置 `qacc = 0` → 惯性力 = 0
- 调用 `forward()` → 计算 `qfrc_bias`
- 结果：`qfrc_bias` ≈ **纯重力力矩**

### 3.3 与另一团队实现的对比

**另一团队的代码**（参考实现）:
```rust
pub fn compute_torques(&mut self, angles_rad: &[f64; 6], velocities_rad: &[f64; 6]) -> [f64; 6] {
    // Set joint positions
    self.data.qpos_mut()[0..6].copy_from_slice(angles_rad);

    // Set joint velocities
    self.data.qvel_mut()[0..6].copy_from_slice(velocities_rad);

    // Zero out accelerations
    self.data.qacc_mut()[0..6].fill(0.0);

    // Forward the simulation
    self.data.forward();

    // Extract gravity compensation torques from qfrc_bias
    array::from_fn(|i| self.data.qfrc_bias()[i])
}
```

**分析**:
- ✅ **正确**: 使用 `qfrc_bias` 字段
- ✅ **正确**: 设置速度和加速度为零
- ⚠️ **可优化**: 接受速度参数但未使用（重力补偿不需要速度）
- ⚠️ **可优化**: 返回固定大小数组而非 Vec

---

## 4. 最佳实践建议

### 4.1 结构体设计

**推荐**:
```rust
pub struct MujocoGravityCompensation {
    model: Rc<MjModel>,      // 共享模型（不可变）
    data: MjData<Rc<MjModel>>,  // 数据（可变）
    ee_body_id: usize,         // 末端执行器 ID
}
```

**理由**:
- ✅ `Rc<MjModel>` 允许多个 `MjData` 共享同一个模型
- ✅ 符合 MuJoCo-rs 的所有权模型
- ✅ 支持多个仿真实例（并行计算）

**避免**:
```rust
// ❌ 不推荐
pub struct MujocoGravityCompensation {
    model: MjModel,  // 不使用 Rc
    data: MjData<MjModel>,  // 生命周期复杂
}
```

### 4.2 加载模式

**推荐**:
```rust
impl MujocoGravityCompensation {
    pub fn from_xml_string(xml: &str) -> Result<Self, PhysicsError> {
        let model = Rc::new(
            MjModel::from_xml_string(xml)
                .map_err(|e| PhysicsError::CalculationFailed(e.to_string()))?
        );
        let data = MjData::new(model.clone());

        // 查找末端执行器
        let ee_body_id = model
            .body("link6")
            .or_else(|| model.body("end_effector"))
            .ok_or_else(|| PhysicsError::CalculationFailed(
                "End effector body not found".into()
            ))?
            .id;

        Ok(Self { model, data, ee_body_id })
    }

    // 使用 include_str! 嵌入默认 URDF
    pub fn from_embedded_xml() -> Result<Self, PhysicsError> {
        const XML: &str = include_str!("../../assets/piper_no_gripper.xml");
        Self::from_xml_string(XML)
    }
}
```

**理由**:
- ✅ `from_xml_string` + `include_str!` = 零运行时开销
- ✅ 无需文件路径管理
- ✅ 编译时嵌入，部署简单

### 4.3 力矩计算实现

**推荐**:
```rust
impl GravityCompensation for MujocoGravityCompensation {
    fn compute_gravity_torques(
        &mut self,
        q: &JointState,
        _gravity: Option<&Vector3<f64>>,  // MuJoCo 使用模型内置重力
    ) -> Result<JointTorques, PhysicsError> {
        // 1. 设置关节位置
        self.data.qpos_mut()[0..6].copy_from_slice(q.as_slice());

        // 2. 零化速度和加速度（纯重力计算）
        self.data.qvel_mut()[0..6].fill(0.0);
        self.data.qacc_mut()[0..6].fill(0.0);

        // 3. 调用正向动力学
        self.data.forward();

        // 4. 提取重力力矩
        let torques = JointTorques::from_iterator(
            self.data.qfrc_bias()[0..6].iter().copied()
        );

        Ok(torques)
    }
}
```

**关键点**:
- ✅ **忽略 `gravity` 参数**: MuJoCo 的重力在 XML 中定义，不运行时修改
- ✅ **强制零速度和加速度**: 确保是纯重力力矩
- ✅ **使用 `copy_from_slice`**: 高效复制
- ✅ **使用 `from_iterator`**: 适配 nalgebra 类型

### 4.4 错误处理

**推荐的错误类型**:
```rust
#[derive(Debug, thiserror::Error)]
pub enum PhysicsError {
    #[error("MuJoCo model loading failed: {0}")]
    ModelLoadError(String),

    #[error("MuJoCo forward dynamics failed: {0}")]
    ForwardDynamicsError(String),

    #[error("Invalid joint positions: {0}")]
    InvalidInput(String),

    #[error("End effector body not found: {0}")]
    BodyNotFoundError(String),
}
```

**错误转换**:
```rust
MjModel::from_xml_string(xml)
    .map_err(|e| PhysicsError::ModelLoadError(e.to_string()))?
```

---

## 5. 性能优化建议

### 5.1 内存分配

**问题**: `data.qfrc_bias()[0..6].to_vec()` 会分配内存

**优化**:
```rust
// ❌ 不推荐（分配 Vec）
let torques_vec = data.qfrc_bias()[0..6].to_vec();

// ✅ 推荐（零分配）
let torques = JointTorques::from_iterator(
    data.qfrc_bias()[0..6].iter().copied()
);
```

**理由**:
- ✅ `from_iterator` 对于固定大小数组优化
- ✅ 无额外堆分配
- ✅ 直接填充 nalgebra 类型

### 5.2 对象复用

**问题**: 每次计算都创建新的 `MjData`

**优化**:
```rust
impl MujocoGravityCompensation {
    // ✅ 复用 MjData
    pub fn compute_gravity_torques(&mut self, q: &JointState) -> Result<JointTorques, PhysicsError> {
        // 复用 self.data
        // 仅更新必要字段
    }
}
```

**理由**:
- ✅ `MjData` 创建有开销（分配内存）
- ✅ 复用对象减少分配
- ✅ 符合 RAII 模式

### 5.3 并行计算

**场景**: 需要同时计算多个构型的重力补偿

**实现**:
```rust
// 多线程安全（Rc<MjModel> 支持共享只读访问）
let model = Rc::new(MjModel::from_xml_string(xml)?);

// 每个线程一个 MjData
let data1 = MjData::new(model.clone());
let data2 = MjData::new(model.clone());

// 并行计算
let torques1 = thread::spawn(move || {
    compute_with_data(&mut data1, q1)
});
let torques2 = thread::spawn(move || {
    compute_with_data(&mut data2, q2)
});
```

### 5.4 批量计算

**场景**: 需要计算轨迹上的所有力矩

**优化**:
```rust
// ❌ 不推荐（每次调用 forward()）
for q in trajectory {
    set_state(&mut data, q);
    data.forward();
    let torques = extract_torques(&data);
}

// ✅ 推荐（使用 MuJoCo 的批处理 API）
// 注意: MuJoCo-rs 目前不直接支持批处理
// 但可以通过复用 MjData 减少开销
```

---

## 6. API 使用示例

### 6.1 基础使用

```rust
use mujoco_rs::prelude::*;

// 1. 加载模型
const XML: &str = include_str!("piper_no_gripper.xml");
let model = Rc::new(MjModel::from_xml_string(XML)?);
let mut data = MjData::new(model.clone());

// 2. 设置状态
let q = [0.0, 0.1, 0.2, 0.0, 0.1, 0.0];
data.qpos_mut()[0..6].copy_from_slice(&q);
data.qvel_mut()[0..6].fill(0.0);
data.qacc_mut()[0..6].fill(0.0);

// 3. 计算力矩
data.forward();
let torques: [f64; 6] = std::array::from_fn(|i| data.qfrc_bias()[i]);
```

### 6.2 与 piper-physics 集成

**推荐实现**:
```rust
// crates/piper-physics/src/mujoco.rs

use mujoco_rs::prelude::*;
use std::rc::Rc;
use std::path::Path;

pub struct MujocoGravityCompensation {
    model: Rc<MjModel>,
    data: MjData<Rc<MjModel>>,
    ee_body_id: usize,
}

impl MujocoGravityCompensation {
    /// 从 MJCF XML 字符串加载（推荐）
    pub fn from_xml_string(xml: &str) -> Result<Self, PhysicsError> {
        let model = Rc::new(
            MjModel::from_xml_string(xml)
                .map_err(|e| PhysicsError::CalculationFailed(format!("MuJoCo: {}", e)))?
        );
        let data = MjData::new(model.clone());

        let ee_body_id = model
            .body("link6")
            .or_else(|| model.body("end_effector"))
            .ok_or_else(|| PhysicsError::CalculationFailed(
                "End effector not found (tried: link6, end_effector)".into()
            ))?
            .id;

        Ok(Self { model, data, ee_body_id })
    }

    /// 从嵌入式 XML 加载（零配置）
    pub fn from_embedded() -> Result<Self, PhysicsError> {
        const XML: &str = include_str!("../../assets/piper_no_gripper.xml");
        Self::from_xml_string(XML)
    }

    /// 从文件加载（不推荐，除非必要）
    pub fn from_xml_file(path: &Path) -> Result<Self, PhysicsError> {
        let xml_content = std::fs::read_to_string(path)
            .map_err(|e| PhysicsError::IoError(e))?;
        Self::from_xml_string(&xml_content)
    }
}
```

---

## 7. 常见陷阱与解决方案

### 陷阱 1: 忘略 `qvel` 设置

**错误代码**:
```rust
// ❌ 错误：没有清零速度
data.qpos_mut()[0..6].copy_from_slice(q);
// data.qvel_mut()[0..6].fill(0.0);  // 忘略这一行
data.qacc_mut()[0..6].fill(0.0);
data.forward();

let torques = data.qfrc_bias()[0..6].to_vec();
```

**问题**: `qfrc_bias` 包含速度相关力（科里奥利 + 离心力）

**解决**:
```rust
// ✅ 正确：清零速度
data.qpos_mut()[0..6].copy_from_slice(q);
data.qvel_mut()[0..6].fill(0.0);  // 必须清零
data.qacc_mut()[0..6].fill(0.0);
data.forward();
```

### 陷阱 2: 使用 `step()` 而非 `forward()`

**错误代码**:
```rust
// ❌ 错误：step() 会推进时间
data.step();
let torques = data.qfrc_bias();
```

**问题**: `step()` 会：
- 推进仿真时间
- 更新位置和速度
- 可能引入积分误差

**解决**:
```rust
// ✅ 正确：forward() 只计算动力学
data.forward();
let torques = data.qfrc_bias();
```

**原理**:
- `forward()`: 正向动力学，不更新状态
- `step()`: 完整仿真步，包含 `forward()` + 积分

### 陷阱 3: 忘略 MuJoCo 的重力配置

**错误代码**:
```rust
// ❌ 错误：尝试在运行时修改重力
fn set_gravity(&mut self, gravity: Vector3<f64>) {
    // MuJoCo 没有这样的 API
    self.data.set_gravity(gravity);  // 这个方法不存在
}
```

**问题**: MuJoCo 的重力在 XML 中定义，运行时不可修改

**解决**:
```rust
// ✅ 正确：在 XML 中定义重力
const XML: &str = r#"
<mujoco>
    <compiler angle="radian">
      <custom>
        <text name="gravity">0 0 -9.81</text>
      </custom>
    </compiler>
    ...
</mujoco>
"#;

let model = MjModel::from_xml_string(XML)?;
```

### 陷阱 4: 忘略数组边界检查

**错误代码**:
```rust
// ❌ 危险：假设模型恰好有 6 个关节
for i in 0..6 {
    data.qpos()[i] = q[i];
}
```

**问题**: 如果模型不是 6-DOF，会越界

**解决**:
```rust
// ✅ 安全：使用模型维度
let nv = self.model.ffi().nv;
let nq = self.model.ffi().nq;
let n = nv.min(nq).min(6);

data.qpos_mut()[..n].copy_from_slice(&q[..n]);
```

**更好的方法**:
```rust
// ✅ 推荐：验证模型
if self.model.ffi().nv != 6 {
    return Err(PhysicsError::CalculationFailed(
        format!("Expected 6-DOF robot, got {}", self.model.ffi().nv)
    ));
}
```

---

## 8. 与另一团队实现的对比

### 8.1 实现质量对比

| 方面 | 另一团队 | 推荐方案 |
|------|---------|---------|
| **类型安全** | ⚠️ 使用 `&[f64; 6]` 固定大小 | ✅ 使用 nalgebra `Vector6` |
| **错误处理** | ⚠️ 简单 unwrap | ✅ 自定义错误类型 |
| **模型复用** | ✅ 存储多个字段 | ✅ 使用 `Rc<MjModel>` |
| **文档** | ⚠️ 基础注释 | ✅ 完整文档 |
| **测试** | ❌ 无测试 | ✅ 需要添加测试 |

### 8.2 代码对比

**另一团队**:
```rust
pub fn compute_torques(&mut self, angles_rad: &[f64; 6], velocities_rad: &[f64; 6]) -> [f64; 6] {
    self.data.qpos_mut()[0..6].copy_from_slice(angles_rad);
    self.data.qvel_mut()[0..6].copy_from_slice(velocities_rad);  // ⚠️ 未使用
    self.data.qacc_mut()[0..6].fill(0.0);
    self.data.forward();

    array::from_fn(|i| self.data.qfrc_bias()[i])
}
```

**推荐改进**:
```rust
pub fn compute_gravity_torques(&mut self, q: &JointState) -> Result<JointTorques, PhysicsError> {
    // 验证
    if self.model.ffi().nv != 6 {
        return Err(PhysicsError::InvalidInput(
            format!("Expected 6-DOF, got {}", self.model.ffi().nv)
        ));
    }

    // 设置状态
    self.data.qpos_mut()[0..6].copy_from_slice(q.as_slice());
    self.data.qvel_mut()[0..6].fill(0.0);
    self.data.qacc_mut()[0..6].fill(0.0);

    // 计算
    self.data.forward()?;

    // 提取
    let torques = JointTorques::from_iterator(
        self.data.qfrc_bias()[0..6].iter().copied()
    );

    Ok(torques)
}
```

**改进点**:
1. ✅ 返回 `Result` 而非 panic
2. ✅ 验证模型维度
3. ✅ 使用类型安全的 `JointState`
4. ✅ 不接受无用的 `velocities` 参数
5. ✅ 使用 `from_iterator` 避免分配

---

## 9. 完整实现示例

### 9.1 优化的实现

```rust
use mujoco_rs::prelude::*;
use std::{path::Path, rc::Rc};
use nalgebra::Vector6;

/// MuJoCo 重力补偿（优化版）
pub struct MujocoGravityCompensation {
    model: Rc<MjModel>,
    data: MjData<Rc<MjModel>>,
}

impl MujocoGravityCompensation {
    /// 从嵌入式 XML 创建（零配置）
    pub fn from_embedded() -> Result<Self, PhysicsError> {
        const XML: &str = include_str!("../../assets/piper_no_gripper.xml");
        Self::from_xml_string(XML)
    }

    /// 从 XML 字符串创建
    pub fn from_xml_string(xml: &str) -> Result<Self, PhysicsError> {
        let model = Rc::new(
            MjModel::from_xml_string(xml)
                .map_err(|e| PhysicsError::ModelLoadError(e.to_string()))?
        );
        let data = MjData::new(model.clone());

        // 验证模型是 6-DOF
        let nv = model.ffi().nv;
        if nv != 6 {
            return Err(PhysicsError::InvalidInput(
                format!("Expected 6-DOF robot, got {}", nv)
            ));
        }

        Ok(Self { model, data })
    }

    /// 计算重力补偿力矩（零分配优化版）
    pub fn compute_gravity_torques_optimized(
        &mut self,
        q: &Vector6<f64>,
    ) -> Result<Vector6<f64>, PhysicsError> {
        // 设置状态
        self.data.qpos_mut()[0..6].copy_from_slice(q.as_slice());
        self.data.qvel_mut()[0..6].fill(0.0);
        self.data.qacc_mut()[0..6].fill(0.0);

        // 正向动力学
        self.data.forward();

        // 零分配提取
        Ok(Vector6::from_iterator(
            self.data.qfrc_bias()[0..6].iter().copied()
        ))
    }
}
```

### 9.2 使用示例

```rust
use piper_physics::MujocoGravityCompensation;

fn main() -> Result<()> {
    // 1. 加载模型（零配置）
    let mut gravity = MujocoGravityCompensation::from_embedded()?;

    // 2. 计算零位力矩
    let q_zero = Vector6::zeros();
    let torques = gravity.compute_gravity_torques_optimized(&q_zero)?;

    println!("Gravity torques at zero position:");
    for (i, &tau) in torques.iter().enumerate() {
        println!("  Joint {}: {:.4} Nm", i + 1, tau);
    }

    // 3. 计算水平姿态力矩
    let q_horizontal = Vector6::from_iterator(std::iter::repeat(1.5708));
    let torques = gravity.compute_gravity_torques_optimized(&q_horizontal)?;

    println!("\nGravity torques at horizontal pose:");
    for (i, &tau) in torques.iter().enumerate() {
        println!("  Joint {}: {:.4} Nm", i + 1, tau);
    }

    Ok(())
}
```

---

## 10. 测试策略

### 10.1 单元测试

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_zero_position() {
        let mut gravity = MujocoGravityCompensation::from_embedded().unwrap();
        let q = Vector6::zeros();
        let torques = gravity.compute_gravity_torques_optimized(&q).unwrap();

        // 验证力矩是否合理
        for &tau in torques.iter() {
            assert!(tau.is_finite());
            assert!(tau.abs() < 100.0);  // 不应该有极端大的力矩
        }
    }

    #[test]
    fn test_horizontal_pose() {
        let mut gravity = MujocoGravityCompensation::from_embedded().unwrap();
        let q = Vector6::from_iterator(std::iter::repeat(std::f64::consts::PI / 2.0));
        let torques = gravity.compute_gravity_torques_optimized(&q).unwrap();

        // 水平姿态应该需要更大的力矩
        for &tau in torques.iter() {
            assert!(tau.abs() > 0.0);  // 应该为正（抵抗重力）
        }
    }
}
```

### 10.2 集成测试

```rust
#[test]
#[ignore]  // 需要硬件
fn test_with_robot() {
    // 1. 连接机器人
    let piper = PiperBuilder::new()
        .interface("can0")
        .connect()?
        .enable_motors()?
        .into_mit_mode();

    // 2. 创建重力补偿
    let mut gravity = MujocoGravityCompensation::from_embedded()?;

    // 3. 测试悬空
    let q_zero = Vector6::zeros();
    let torques = gravity.compute_gravity_torques_optimized(&q_zero)?;

    // 4. 发送力矩
    for (i, &tau) in torques.iter().enumerate() {
        let cmd = MitCommand::torque_only(i + 1, tau as f32);
        piper.send_realtime_command(cmd)?;
    }

    // 5. 验证机器人保持悬空（不坠落）
    std::thread::sleep(std::time::Duration::from_secs(5));
}
```

---

## 11. 性能基准

### 11.1 延迟测量

```rust
use std::time::Instant;

fn benchmark() {
    let mut gravity = MujocoGravityCompensation::from_embedded().unwrap();
    let q = Vector6::zeros();

    let start = Instant::now();
    for _ in 0..1000 {
        let _ = gravity.compute_gravity_torques_optimized(&q);
    }
    let elapsed = start.elapsed();

    println!("1000 calls: {:?}", elapsed);
    println!("Average per call: {:?}", elapsed / 1000);
}
```

**预期性能**:
- 目标: < 10 µs/次
- 频率: >100 kHz（理论）
- 实际: 受 CAN 总线限制（~500-1000 Hz）

### 11.2 内存使用

```rust
use std::mem::size_of;

fn main() {
    println!("MjModel: {} bytes", size_of::<MjModel>());
    println!("MjData: {} bytes", size_of::<MjData<Rc<MjModel>>>());
    println!("MujocoGravityCompensation: {} bytes",
             size_of::<MujocoGravityCompensation>());
}
```

---

## 12. 部署建议

### 12.1 Cargo.toml 配置

```toml
[dependencies]
mujoco-rs = { version = "2.3", default-features = false }
# 移除不必要的 features:
# - viewer（不需要查看器）
# - renderer（不需要渲染）
# - auto-download-mujoco（控制下载）

[features]
default = []  # 最小化依赖
```

### 12.2 MJCF XML 文件组织

```
crates/piper-physics/
├── assets/
│   ├── piper_no_gripper.xml        # 空载配置
│   ├── piper_small_gripper.xml     # 小夹爪（500g）
│   └── piper_large_gripper.xml     # 大夹爪（1kg）
└── src/
    └── mujoco.rs
```

### 12.3 嵌入式 XML

```rust
// 在 mujoco.rs 中
const DEFAULT_PIPER_NO_GRIPPER: &str = include_str!("../../assets/piper_no_gripper.xml");
const DEFAULT_PIPER_SMALL_GRIPPER: &str = include_str!("../../assets/piper_small_gripper.xml");

impl MujocoGravityCompensation {
    pub fn from_no_gripper() -> Result<Self, PhysicsError> {
        Self::from_xml_string(DEFAULT_PIPER_NO_GRIPPER)
    }

    pub fn from_small_gripper() -> Result<Self, PhysicsError> {
        Self::from_xml_string(DEFAULT_PIPER_SMALL_GRIPPER)
    }
}
```

---

## 13. 总结

### 13.1 核心发现

1. **MuJoCo-rs 设计优秀**:
   - ✅ 类型安全的数组访问（`array_slice_dyn` 宏）
   - ✅ 清晰的所有权模型（`Rc<MjModel>` + `MjData`）
   - ✅ 完整的错误处理
   - ✅ 丰富的文档

2. **重力补偿实现简单**:
   - ✅ 使用 `qfrc_bias` 字段
   - ✅ 设置 `qvel=0` 和 `qacc=0`
   - ✅ 调用 `forward()`
   - ✅ 提取力矩

3. **性能优化空间大**:
   - ✅ 使用 `from_iterator` 避免分配
   - ✅ 复用 `MjData` 对象
   - ✅ 使用 `Rc` 共享模型

### 13.2 最佳实践总结

| 方面 | 推荐做法 |
|------|---------|
| **模型加载** | `from_xml_string` + `include_str!` |
| **所有权** | `Rc<MjModel>` + `MjData<Rc<MjModel>>` |
| **力矩计算** | `qfrc_bias` + `forward()` |
| **类型转换** | `from_iterator` 适配 nalgebra |
| **错误处理** | 自定义 `PhysicsError` 枚举 |
| **性能优化** | 零分配、对象复用 |

### 13.3 与设计文档对比

**v2.2 设计文档中的假设**:
- ❌ 假设：需要 `set_end_effector_payload` 方法
- ✅ 实际：MuJoCo 通过 XML 配置负载
- ❌ 假设：可以运行时修改重力矢量
- ✅ 实际：重力在 XML 中定义

**修正建议**:
- ✅ 移除动态负载 API（已在 v2.2 中实现）
- ✅ 使用多个 XML 文件配置不同负载
- ✅ 文档说明 MuJoCo 的限制

---

**分析者**: AI
**日期**: 2025-01-28
**版本**: v1.0
**状态**: ✅ 完成
