# 修正对比 - API 幻觉问题详解

**日期**: 2025-01-28
**问题**: 原版《快速修复指南》Fix #5 存在严重的 API 幻觉错误

---

## 🔴 错误代码 (原版 Fix #5)

### 错误的函数调用

```rust
// ❌ 这段代码是错误的！
unsafe {
    mujoco_rs::sys::mj_jacSite(
        self.model.ffi(),
        self.data.ffi(),
        jacp.as_mut_ptr(),
        jacr.as_mut_ptr(),
        ee_site_id as i32,
        com[0], com[1], com[2],  // ← 错误！mj_jacSite 不接受这些参数
    );
}
```

### 为什么这是错的？

1. **编译失败**: 参数数量不匹配
   ```c
   // MuJoCo 实际的 API 签名
   void mj_jacSite(const mjModel* m, const mjData* d,
                   mjtNum* jacp, mjtNum* jacr,
                   int site);  // ← 只有 5 个参数！
   ```

2. **如果是强行调用**: 会导致 Undefined Behavior
   - 多余的参数会读取栈上的垃圾数据
   - 可能导致段错误 (segmentation fault)
   - 可能计算出完全错误的结果

3. **物理逻辑错误**: 即使 API 支持偏移参数（实际上不支持），这个实现也不完整
   - **只计算了力** (Force)
   - **忽略了力矩** (Moment/Torque)
   - 对于偏心负载，必须考虑 `τ = r × F`

---

## ✅ 正确代码 (修正版 Fix #5)

### 正确的实现步骤

```rust
// ✅ 使用正确的 MuJoCo API

fn compute_payload_torques(
    &mut self,
    _q: &JointState,
    mass: f64,
    com: nalgebra::Vector3<f64>,  // 在 Site 局部坐标系中的偏移
    ee_site_id: mujoco_rs::sys::mjnSite,
    ee_body_id: mujoco_rs::sys::mjnBody,
) -> Result<JointTorques, PhysicsError> {
    // 步骤 1: 获取重力向量 (使用模型配置)
    let model_gravity = self.model.opt().gravity;
    let f_gravity = nalgebra::Vector3::new(
        model_gravity[0] * mass,
        model_gravity[1] * mass,
        model_gravity[2] * mass,
    );

    // 步骤 2: 获取 Site 在世界坐标系中的位置和姿态
    let site_xpos = self.data.site_xpos(ee_site_id);  // Site 原点的世界坐标 [x, y, z]
    let site_xmat = self.data.site_xmat(ee_site_id);  // Site 的旋转矩阵 (3x3, 列主序)

    // 步骤 3: 将局部偏移转换到世界坐标系
    // world_offset = R * local_offset
    let mut world_offset = nalgebra::Vector3::zeros();
    for i in 0..3 {
        for j in 0..3 {
            // site_xmat 是列主序存储的 3x3 矩阵
            world_offset[i] += site_xmat[i + 3*j] * com[j];
        }
    }

    // 步骤 4: 计算负载质心在世界坐标系中的位置
    let world_com = nalgebra::Vector3::new(
        site_xpos[0] + world_offset[0],
        site_xpos[1] + world_offset[1],
        site_xpos[2] + world_offset[2],
    );

    // 步骤 5: 使用 mj_jac 计算该点的 Jacobian
    let mut jacp = [0.0f64; 18];  // 3x6 线性 Jacobian
    let mut jacr = [0.0f64; 18];  // 3x6 旋转 Jacobian (本次不需要)

    unsafe {
        mujoco_rs::sys::mj_jac(  // ← 正确的 API!
            self.model.ffi(),
            self.data.ffi(),
            jacp.as_mut_ptr(),
            jacr.as_mut_ptr(),
            world_com[0],  // ← 世界坐标点的 x
            world_com[1],  // ← 世界坐标点的 y
            world_com[2],  // ← 世界坐标点的 z
            ee_body_id,    // ← Body ID
        );
    }

    // 步骤 6: Jacobian 转置法计算力矩
    // τ = J^T * F
    let mut jacp_matrix = nalgebra::Matrix3x6::<f64>::zeros();
    for i in 0..3 {
        for j in 0..6 {
            jacp_matrix[(i, j)] = jacp[i * 6 + j];
        }
    }

    let tau_payload = jacp_matrix.transpose() * f_gravity;
    let torques = JointTorques::from_iterator(tau_payload.iter());

    Ok(torques)
}
```

---

## 关键差异对比

| 方面 | 错误版本 (Fix #5 原版) | 正确版本 (Fix #5 修正) |
|------|----------------------|----------------------|
| **API 使用** | `mj_jacSite(..., com[0], com[1], com[2])` | `mj_jac(..., world_com[0], world_com[1], world_com[2], body_id)` |
| **编译状态** | ❌ 编译失败 (参数数量错误) | ✅ 可以编译 |
| **坐标系统** | 未处理坐标系转换 | 正确转换局部偏移 → 世界坐标 |
| **物理完整性** | ❌ 不完整 (只有力，无力矩) | ✅ 完整 (通过 Jacobian 自动包含力矩项) |
| **API 文档** | 不符合 MuJoCo 实际 API | 符合 [MuJoCo API 文档](https://mujoco.readthedocs.io/en/latest/APIreference/#programming) |

---

## 物理原理解释

### 为什么需要坐标转换？

```
局部坐标系 (Site Frame)          世界坐标系 (World Frame)
        ↑                                 ↑
        |  com = [0.05, 0, 0]             |  world_com = ?
        |  (5cm forward)                  |  (需要计算)
        |                                 |
    ────●────                           ●────────────
    Site                              Site (rotated)

计算过程:
  world_com = site_xpos + site_xmat * com
            ↑             ↑
         Site 世界位置  Site 旋转矩阵 * 局部偏移
```

### 为什么 Jacobian 转置法就够了？

对于偏心负载，我们通常需要考虑:

```
τ_total = J^T * F + J_rot^T * M

其中:
- J: 线性 Jacobian (3x6)
- J_rot: 旋转 Jacobian (3x6)
- F: 重力力 [0, 0, -mg]^T
- M: 力矩 (由于偏心产生)
```

**但是**，当我们计算**偏心点**的 Jacobian 时，该点的运动已经包含了旋转分量:

```
如果负载质心不在 Site 原点:
  当关节运动时 → 质点会同时产生线速度和角速度
  → 该点的 Jacobian (mj_jac) 已经包含了完整信息
  → 只需 τ = J_com^T * F_gravity
```

这就是为什么我们使用 `mj_jac` 计算偏心点，而不是使用 `mj_jacSite` 计算 Site 原点。

---

## API 签名详解

### mj_jacSite (错误的 API 用于此场景)

```c
void mj_jacSite(const mjModel* m,     // 模型
                const mjData* d,     // 数据
                mjtNum* jacp,         // 输出: 线性 Jacobian (3 x nv)
                mjtNum* jacr,         // 输出: 旋转 Jacobian (3 x nv)
                int site);           // 输入: Site ID

// 用途: 计算 Site 原点的 Jacobian
// 限制: Site 原点是固定的，不能指定任意偏移点
```

### mj_jac (正确的 API)

```c
void mj_jac(const mjModel* m,     // 模型
            const mjData* d,     // 数据
            mjtNum* jacp,         // 输出: 线性 Jacobian (3 x nv)
            mjtNum* jacr,         // 输出: 旋转 Jacobian (3 x nv)
            const mjtNum point[3],// 输入: 世界坐标系中的点坐标
            int body);            // 输入: Body ID

// 用途: 计算任意点的 Jacobian
// 灵活性: 可以指定世界坐标系中的任意点
```

### mj_jacBody (替代方案)

```c
void mj_jacBody(const mjModel* m,     // 模型
                 const mjData* d,     // 数据
                 mjtNum* jacp,         // 输出: 线性 Jacobian (3 x nv)
                 mjtNum* jacr,         // 输出: 旋转 Jacobian (3 x nv)
                 int body,            // 输入: Body ID
                 mjtNum point[3]);    // 输入: Body 局部坐标系中的点坐标

// 用途: 计算 Body 局部坐标系中某点的 Jacobian
// 优势: 可以直接使用局部偏移 (com)，无需手动转换到世界坐标
```

**使用 mj_jacBody 的简化版本**:

```rust
// 更简单的实现 (如果 mujoco-rs 暴露了 mj_jacBody)
unsafe {
    mujoco_rs::sys::mj_jacBody(
        self.model.ffi(),
        self.data.ffi(),
        jacp.as_mut_ptr(),
        jacr.as_mut_ptr(),
        ee_body_id,              // Body ID
        [com[0], com[1], com[2]], // Body 局部坐标系中的偏移
    );
}
// 然后直接使用 jacp 计算，无需手动坐标转换
```

---

## 完整的正确实现 (使用 mj_jacBody)

如果 `mujoco-rs` 暴露了 `mj_jacBody` API，实现会更简洁:

```rust
fn compute_payload_torques(
    &mut self,
    _q: &JointState,
    mass: f64,
    com: nalgebra::Vector3<f64>,  // Body 局部坐标系中的偏移
    _ee_site_id: mujoco_rs::sys::mjnSite,
    ee_body_id: mujoco_rs::sys::mjnBody,
) -> Result<JointTorques, PhysicsError> {
    // 1. 获取重力向量
    let model_gravity = self.model.opt().gravity;
    let f_gravity = nalgebra::Vector3::new(
        model_gravity[0] * mass,
        model_gravity[1] * mass,
        model_gravity[2] * mass,
    );

    // 2. 计算偏心点的 Jacobian (mj_jacBody 自动处理坐标转换)
    let mut jacp = [0.0f64; 18];
    let mut jacr = [0.0f64; 18];

    unsafe {
        mujoco_rs::sys::mj_jacBody(  // ← 更简单的 API!
            self.model.ffi(),
            self.data.ffi(),
            jacp.as_mut_ptr(),
            jacr.as_mut_ptr(),
            ee_body_id,                     // Body ID
            [com[0], com[1], com[2]],       // Body 局部坐标系中的偏移点
        );
    }

    // 3. Jacobian 转置
    let mut jacp_matrix = nalgebra::Matrix3x6::<f64>::zeros();
    for i in 0..3 {
        for j in 0..6 {
            jacp_matrix[(i, j)] = jacp[i * 6 + j];
        }
    }

    let tau_payload = jacp_matrix.transpose() * f_gravity;
    let torques = JointTorques::from_iterator(tau_payload.iter());

    Ok(torques)
}
```

---

## 验证 API 可用性

在 mujoco-rs 中检查哪些 API 被暴露:

```bash
# 搜索 mujoco_rs 的 FFI 绑定
grep -r "mj_jac" tmp/mujoco-rs/src/

# 预期结果:
# src/mujoco_c.rs 或类似文件中应该有:
# pub fn mj_jac(...)
# pub fn mj_jacBody(...)
# pub fn mj_jacSite(...)
```

如果 `mj_jacBody` 不可用，使用修正版 Fix #5 中的 `mj_jac` 实现（需要手动坐标转换）。

---

## 总结

| 项目 | 错误版本 | 正确版本 |
|------|---------|---------|
| **API** | `mj_jacSite` (不支持偏移) | `mj_jac` 或 `mj_jacBody` (支持偏移) |
| **参数** | `com[0], com[1], com[2]` (编译失败) | `world_com[0], world_com[1], world_com[2], body_id` |
| **坐标处理** | 无 | 局部 → 世界坐标转换 |
| **代码行数** | ~10 行 (但无法编译) | ~30 行 (完整正确) |
| **维护性** | N/A | 清晰的物理原理注释 |

**教训**: 在编写 FFI 绑定代码时，**务必查阅官方 API 文档**，不要凭直觉假设函数签名。
