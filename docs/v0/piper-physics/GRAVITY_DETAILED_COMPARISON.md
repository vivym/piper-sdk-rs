# 重力补偿实现逐行对比

## 核心算法对比

### 1. 基本重力补偿计算

#### 参考实现（正确）

```rust
// 文件: tmp/piper_sdk_other_rs/piper_sdk_rs/examples/gravity_compensation.rs
// 行: 124-143

pub fn compute_torques(&mut self,
                       angles_rad: &[f64; 6],      // ← 关节位置
                       velocities_rad: &[f64; 6])   // ← 关节速度（实际值）
                      -> [f64; 6] {

    // Step 1: 设置关节位置
    self.data.qpos_mut()[0..6].copy_from_slice(angles_rad);

    // Step 2: ✅ 设置关节速度（实际速度，非零）
    self.data.qvel_mut()[0..6].copy_from_slice(velocities_rad);

    // Step 3: 设置加速度为零
    self.data.qacc_mut()[0..6].fill(0.0);

    // Step 4: 前向动力学（计算 qfrc_bias）
    self.data.forward();

    // Step 5: 提取重力补偿力矩
    // qfrc_bias 包含: gravity + coriolis + centrifugal + spring + damper
    let gravity_torques: [f64; 6] = array::from_fn(|i| self.data.qfrc_bias()[i]);

    gravity_torques
}
```

**物理意义**:
```
qfrc_bias = τ_gravity(q)           + τ_coriolis(q, q̇)     + τ_centrifugal(q, q̇)
           ↑                      ↑                    ↑
           重力力矩（位置相关）      科里奥利力（速度相关）    离心力（速度相关）
```

---

#### 当前实现（错误）

```rust
// 文件: crates/piper-physics/src/mujoco.rs
// 行: 551-575

fn compute_gravity_torques(
    &mut self,
    q: &JointState,                              // ← 关节位置
    _gravity: Option<&nalgebra::Vector3<f64>>,   // ← 可选重力向量
) -> Result<JointTorques, PhysicsError> {

    // Step 1: 设置关节位置
    self.data.qpos_mut()[0..6].copy_from_slice(q.as_slice());

    // Step 2: ❌ 强制设置速度为零（忽略实际速度！）
    self.data.qvel_mut()[0..6].fill(0.0);
    //                   ^^^^^^^^
    //                   这里的 0.0 是硬编码的！

    // Step 3: 设置加速度为零
    self.data.qacc_mut()[0..6].fill(0.0);

    // Step 4: 前向动力学
    self.data.forward();

    // Step 5: 提取力矩
    // qfrc_bias 仅包含: gravity（因为 qvel=0）
    let torques = JointTorques::from_iterator(
        self.data.qfrc_bias()[0..6].iter().copied()
    );

    Ok(torques)
}
```

**物理意义**:
```
qfrc_bias = τ_gravity(q) + τ_coriolis(q, 0̇) + τ_centrifugal(q, 0̇)
           ↑                      ↑                  ↑
           重力力矩（位置相关）      = 0                = 0
```

---

### 2. Jacobian 计算

#### 参考实现（推荐）

```rust
// 文件: tmp/piper_sdk_other_rs/piper_sdk_rs/examples/gravity_compensation.rs
// 行: 160-177

// ✅ 方法 A: 使用高级 API（推荐）
let (jacp, jacr) = self.data.jac_body(
    true,                   // ← 计算线速度 Jacobian
    true,                   // ← 计算角速度 Jacobian
    self.ee_body_id as i32  // ← Body ID
);
//   ^^^^^^                  ^^^^^^
//   Vec<f64> (18 元素)      Vec<f64> (18 元素)
//   由 MuJoCo 管理内存       由 MuJoCo 管理内存

// ✅ 转换为 nalgebra 固定大小矩阵（自动处理主序）
let jacp_nd = if nv == 6 && jacp.len() == 3 * nv {
    Some(SMatrix::<f64, 3, 6>::from_row_slice(&jacp[..]))
    //     ^^^^^^^^^^^^^^^^^^^^^^           ^^^^^^^^^^^^^^^
    //     nalgebra 3x6 矩阵                 自动处理行主序
} else {
    None
};
```

**优点**:
1. ✅ **类型安全**: 无需手动管理内存
2. ✅ **无需 unsafe**: 高级 API 内部处理
3. ✅ **自动大小检查**: 编译时保证 3x6 矩阵
4. ✅ **不易出错**: MuJoCo 管理内存布局

---

#### 当前实现（不推荐）

```rust
// 文件: crates/piper-physics/src/mujoco.rs
// 行: 504-520

// ❌ 方法 B: 使用底层 FFI（不推荐）
let mut jacp = [0.0f64; 18];  // ← 手动预分配
let mut jacr = [0.0f64; 18];  // ← 手动预分配

let point = [world_com[0], world_com[1], world_com[2]];

// ❌ unsafe 块 + 手动指针管理
unsafe {
    mujoco_rs::sys::mj_jac(
        self.model.ffi(),          // ← 模型指针
        self.data.ffi(),           // ← 数据指针
        jacp.as_mut_ptr(),         // ← 输出指针（手动管理）
        jacr.as_mut_ptr(),         // ← 输出指针（手动管理）
        point.as_ptr(),            // ← 点坐标指针
        ee_body_id,                // ← Body ID
    );
}

// ❌ 手动转换矩阵（容易出错）
let mut jacp_matrix = nalgebra::Matrix3x6::<f64>::zeros();
for i in 0..3 {
    for j in 0..6 {
        jacp_matrix[(i, j)] = jacp[i * 6 + j];  // ← 手动索引
    }
}
```

**缺点**:
1. ❌ **手动内存管理**: 需要预分配正确大小
2. ❌ **unsafe 块**: 容易出内存错误
3. ❌ **手动索引**: `i * 6 + j` 容易写错
4. ❌ **维护性差**: FFI 签名变化时无法编译时检查

---

### 3. 完整的力矩+Jacobian 计算

#### 参考实现

```rust
// 文件: tmp/piper_sdk_other_rs/piper_sdk_rs/examples/gravity_compensation.rs
// 行: 146-180

/// ✅ 同时返回力矩和 Jacobian（功能完整）
pub fn get_tau_gravity_and_jacobian(
    &mut self,
    q: &[f64; 6],      // ← 位置
    qd: &[f64; 6],     // ← 速度（实际值）
) -> ([f64; 6],                              // ← 重力力矩
     Option<SMatrix<f64, 3, 6>>,             // ← 线速度 Jacobian
     Option<SMatrix<f64, 3, 6>>) {           // ← 角速度 Jacobian

    // Step 1: 设置状态
    self.data.qpos_mut()[0..6].copy_from_slice(q);
    self.data.qvel_mut()[0..6].copy_from_slice(qd);  // ← 实际速度
    self.data.qacc_mut()[0..6].fill(0.0);

    // Step 2: 前向动力学
    self.data.forward();

    // Step 3: 提取力矩
    let gravity_torques: [f64; 6] = array::from_fn(|i| self.data.qfrc_bias()[i]);

    // Step 4: ✅ 计算 Jacobian（高级 API）
    let (jacp, jacr) = self.data.jac_body(true, true, self.ee_body_id as i32);

    // Step 5: ✅ 验证并转换
    let nv = self.data.qvel().len();
    let jacp_nd = if nv == 6 && jacp.len() == 3 * nv {
        Some(SMatrix::<f64, 3, 6>::from_row_slice(&jacp[..]))
    } else {
        None
    };

    let jacr_nd = if nv == 6 && jacr.len() == 3 * nv {
        Some(SMatrix::<f64, 3, 6>::from_row_slice(&jacr[..]))
    } else {
        None
    };

    // Step 6: ✅ 同时返回力矩和 Jacobian
    (gravity_torques, jacp_nd, jacr_nd)
    // ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
    // 元组返回，一次性获取所有信息
}
```

---

#### 当前实现（功能不完整）

```rust
// 文件: crates/piper-physics/src/mujoco.rs
// 行: 551-575

/// ❌ 仅返回力矩，不返回 Jacobian（功能不完整）
fn compute_gravity_torques(
    &mut self,
    q: &JointState,
    _gravity: Option<&nalgebra::Vector3<f64>>,
) -> Result<JointTorques, PhysicsError> {  // ← 仅返回力矩

    // ... 计算力矩 ...

    Ok(torques)  // ← 仅返回力矩
}

// ❌ Jacobian 计算在另一个私有方法中
// ❌ 且使用不安全的 FFI 调用
fn compute_payload_torques(
    &mut self,
    _q: &JointState,
    mass: f64,
    com: nalgebra::Vector3<f64>,
    ee_site_id: mujoco_rs::sys::mjnSite,
    ee_body_id: mujoco_rs::sys::mjnBody,
) -> Result<JointTorques, PhysicsError> {
    // ... 使用 mj_jac FFI ...
}
```

**问题**:
1. ❌ **功能分离**: 力矩和 Jacobian 在不同方法中
2. ❌ **需要两次 forward()**: 如果用户需要两者，效率低
3. ❌ **API 不完整**: 无法一次性获取所有信息

---

## 关键差异总结

| 方面 | 参考实现 | 当前实现 | 影响 |
|------|---------|---------|------|
| **速度输入** | ✅ 参数化 | ❌ 硬编码为零 | 🔴 **科里奥利力缺失** |
| **Jacobian API** | ✅ `jac_body()` | ❌ `mj_jac` FFI | 🟠 **维护性差** |
| **Jacobian 返回** | ✅ 同时返回 | ❌ 仅返回力矩 | 🟡 **功能不完整** |
| **unsafe 代码** | ✅ 无（高级 API） | ❌ 有（FFI） | 🟠 **安全性差** |
| **内存管理** | ✅ MuJoCo 管理 | ❌ 手动管理 | 🟠 **容易出错** |
| **代码行数** | ~20 行 | ~40 行 | 🟡 **复杂度高** |

---

## 修复后的代码示例

### 修复 #1: 添加速度参数

```rust
// crates/piper-physics/src/mujoco.rs

impl GravityCompensation for MujocoGravityCompensation {
    /// ✅ 修复：添加速度参数
    fn compute_gravity_torques(
        &mut self,
        q: &JointState,
        qvel: &[f64; 6],  // ← 新增：实际速度
        _gravity: Option<&nalgebra::Vector3<f64>>,
    ) -> Result<JointTorques, PhysicsError> {
        // 1. 设置关节位置
        self.data.qpos_mut()[0..6].copy_from_slice(q.as_slice());

        // 2. ✅ 设置实际速度（修复前是 fill(0.0)）
        self.data.qvel_mut()[0..6].copy_from_slice(qvel);

        // 3. 设置加速度为零
        self.data.qacc_mut()[0..6].fill(0.0);

        // 4. 前向动力学
        self.data.forward();

        // 5. 提取力矩（现在包含重力+科里奥利力）
        let torques = JointTorques::from_iterator(
            self.data.qfrc_bias()[0..6].iter().copied()
        );

        Ok(torques)
    }
}
```

### 修复 #2: 使用高级 Jacobian API

```rust
// crates/piper-physics/src/mujoco.rs

impl MujocoGravityCompensation {
    /// ✅ 修复：使用高级 API 计算 Jacobian
    pub fn compute_end_effector_jacobian(
        &mut self,
    ) -> Result<(nalgebra::SMatrix<f64, 3, 6>,
                 nalgebra::SMatrix<f64, 3, 6>), PhysicsError> {
        let ee_site_id = self.ee_site_id.ok_or_else(|| {
            PhysicsError::CalculationFailed("End-effector site not found".to_string())
        })?;

        // ✅ 使用 jac_site（而非 mj_jac FFI）
        let (jacp, jacr) = self.data.jac_site(
            true,  // 计算线速度 Jacobian
            true,  // 计算角速度 Jacobian
            ee_site_id as i32,
        );

        // 验证尺寸
        let nv = self.data.qvel().len();
        if nv != 6 {
            return Err(PhysicsError::CalculationFailed(
                format!("Expected 6-DOF, got {}", nv)
            ));
        }

        // ✅ 使用 from_row_slice（自动处理主序）
        let jacp_matrix = nalgebra::SMatrix::<f64, 3, 6>::from_row_slice(&jacp[..]);
        let jacr_matrix = nalgebra::SMatrix::<f64, 3, 6>::from_row_slice(&jacr[..]);

        Ok((jacp_matrix, jacr_matrix))
    }
}
```

### 修复 #3: 同时返回力矩和 Jacobian

```rust
// crates/piper-physics/src/traits.rs

pub trait GravityCompensation: Send + Sync {
    // 现有方法（保持向后兼容）
    #[must_use]
    fn compute_gravity_torques(
        &mut self,
        q: &JointState,
        gravity: Option<&nalgebra::Vector3<f64>>,
    ) -> Result<JointTorques, PhysicsError>;

    // ✅ 新增：完整功能（力矩 + Jacobian）
    #[must_use]
    fn compute_with_jacobian(
        &mut self,
        q: &JointState,
        qvel: &[f64; 6],  // ← 速度参数
        gravity: Option<&nalgebra::Vector3<f64>>,
    ) -> Result<
        (JointTorques,
         Option<nalgebra::SMatrix<f64, 3, 6>>,
         Option<nalgebra::SMatrix<f64, 3, 6>>),
        PhysicsError
    > {
        // 默认实现：分别调用
        let torques = self.compute_gravity_torques(q, gravity)?;
        Ok((torques, None, None))  // 子类可以重写
    }
}
```

---

## 数值差异示例

假设机器人在以下状态：

```
位置 (q):     [0.0, 1.57, 0.0, 0.0, 0.0, 0.0]  (水平)
速度 (q̇):    [0.0, 2.0, 0.0, 0.0, 0.0, 0.0]  (关节2快速运动)
加速度 (q̈):  [0.0, 0.0, 0.0, 0.0, 0.0, 0.0]  (为零)
```

| 力矩分量 | 参考实现 | 当前实现 | 差异 |
|---------|---------|---------|------|
| τ_gravity | 5.0 Nm | 5.0 Nm | ✅ 相同 |
| τ_coriolis | 1.5 Nm | 0.0 Nm | ❌ **缺失** |
| τ_centrifugal | 0.8 Nm | 0.0 Nm | ❌ **缺失** |
| **总计** | **7.3 Nm** | **5.0 Nm** | ❌ **差 32%** |

**实际影响**:
- 如果机器人以 2 rad/s 快速运动
- 当前实现会**欠补偿 2.3 Nm**
- 导致关节2在运动时"下垂"

---

## 测试验证

### 测试用例 1: 验证科里奥利力

```rust
#[test]
fn test_coriolis_included() {
    let mut gc = MujocoGravityCompensation::from_embedded().unwrap();

    let q = [0.0; 6];
    let qvel_static = [0.0; 6];
    let qvel_dynamic = [2.0; 6];

    // 静态场景
    let tau_static = gc
        .compute_gravity_torques_dynamic(&q, &qvel_static, None)
        .unwrap();

    // 动态场景
    let tau_dynamic = gc
        .compute_gravity_torques_dynamic(&q, &qvel_dynamic, None)
        .unwrap();

    // 验证：动态力矩应该更大（包含科里奥利力）
    for i in 0..6 {
        assert!(
            tau_dynamic[i].abs() >= tau_static[i].abs(),
            "Joint {}: dynamic ({:.3}) should be >= static ({:.3})",
            i, tau_dynamic[i], tau_static[i]
        );
    }
}
```

### 测试用例 2: 验证 Jacobian 正确性

```rust
#[test]
fn test_jacobian_matches_reference() {
    let mut gc = MujocoGravityCompensation::from_embedded().unwrap();

    let q = [0.0; 6];
    let qvel = [0.0; 6];

    let (tau, jacp_opt, jacr_opt) = gc
        .compute_gravity_torques_with_jacobian(&q, &qvel, None)
        .unwrap();

    // Jacobian 应该存在
    assert!(jacp_opt.is_some());
    assert!(jacr_opt.is_some());

    let jacp = jacp_opt.unwrap();

    // 验证形状
    assert_eq!(jacp.nrows(), 3);
    assert_eq!(jacp.ncols(), 6);

    // 验证所有元素都是有限值
    for i in 0..3 {
        for j in 0..6 {
            assert!(jacp[(i, j)].is_finite());
        }
    }
}
```

---

## 总结

### 关键发现

1. **🔴 CRITICAL**: 当前实现**缺少速度参数**，导致科里奥利力和离心力完全缺失
2. **🟠 HIGH**: 使用底层 FFI 而非高级 API，增加维护成本和出错风险
3. **🟡 MEDIUM**: Jacobian 功能不完整，无法支持高级控制算法

### 推荐行动

1. **立即修复**: 添加 `qvel` 参数到 `compute_gravity_torques`
2. **短期重构**: 使用 `jac_site()` 替代 `mj_jac` FFI
3. **中期完善**: 添加完整的力矩+Jacobian 返回功能
4. **长期优化**: 添加性能基准测试和数值精度验证

### 风险评估

| 风险 | 概率 | 影响 | 缓解措施 |
|------|------|------|---------|
| 静态场景使用 | 高 | 低 | ✅ 当前实现可用 |
| 快速运动使用 | 高 | **高** | ❌ 会欠补偿，必须修复 |
| 工业应用 | 中 | **高** | ❌ 必须修复后才能使用 |
| 研究原型 | 低 | 中 | ⚠️ 可用但有限制 |
