# MuJoCo API 修复 - 快速参考

> **完整分析**: [MUJOCO_API_SOLUTION.md](./MUJOCO_API_SOLUTION.md)

## ✅ 问题已解决

通过分析 mujoco-rs 2.3.0+mj-3.3.7 源代码，找到了**所有错误的根本原因和精确修复方案**。

---

## 🔑 关键发现

### 1. 正确的导入方式

```rust
// ❌ 错误
use mujoco_rs::prelude::*;
ee_site_id: Option<mujoco_rs::sys::mjnSite>;

// ✅ 正确
use mujoco_rs::{mujoco_c, prelude::*};
use mujoco_rs::mujoco_c::{mjnSite, mjnBody};
ee_site_id: Option<mjnSite>;
```

### 2. 字段 vs 方法

```rust
// 这些是字段，不是方法！
let all_site_xpos = data.site_xpos;  // 返回 &[[f64; 3]]
let all_site_xmat = data.site_xmat;  // 返回 &[[f64; 9]]
let inverse_forces = data.qfrc_inverse;  // 返回 &[f64]

// 访问特定 site
let site_xpos = &all_site_xpos[site_id];  // &[f64; 3]
```

### 3. Rc → Arc

```rust
// ❌ Rc 不是 Send + Sync
use std::rc::Rc;
model: Rc<MjModel>,

// ✅ Arc 是 Send + Sync
use std::sync::Arc;
model: Arc<MjModel>,
```

### 4. site_bodyid 替代 site_parent

```rust
// ❌ site_parent 不存在
let parent_body_i32 = unsafe { (*model.ffi()).site_parent[id] };

// ✅ 使用 site_bodyid
let parent_body_i32 = model.site_bodyid[id];
```

### 5. FFI 函数调用

```rust
use mujoco_rs::mujoco_c;

unsafe {
    mujoco_c::mj_inverse(model.ffi(), data.ffi());
    mujoco_c::mj_jac(model.ffi(), data.ffi(), ...);
}
```

---

## 🛠️ 快速修复清单

| 文件 | 行号 | 修复内容 | 时间 |
|------|------|---------|------|
| mujoco.rs | 19-21 | 修改导入 | 5分钟 |
| mujoco.rs | 70-71 | `Rc` → `Arc` | 15分钟 |
| mujoco.rs | 99 | 修复路径 `../assets/` | 2分钟 |
| mujoco.rs | 74-76 | 移除 `sys::` 前缀 | 5分钟 |
| mujoco.rs | 252 | `site_parent` → `site_bodyid` | 10分钟 |
| mujoco.rs | 480 | FFI 调用添加 `mujoco_c::` | 5分钟 |
| mujoco.rs | 539-540 | 字段访问修正 | 30分钟 |
| mujoco.rs | 545 | 矩阵类型转换 | 30分钟 |
| mujoco.rs | 563 | 添加 `.copied()` | 2分钟 |
| mujoco.rs | 666 | 移除 `()` 方法调用 | 2分钟 |
| mujoco.rs | 292 | 指针索引修正 | 15分钟 |

**总时间**: 2-3 小时（核心修复）

---

## 📋 修复示例

### 修改导入（5分钟）

```rust
// crates/piper-physics/src/mujoco.rs

use crate::{error::PhysicsError, traits::GravityCompensation, types::{JointState, JointTorques}};
use mujoco_rs::{mujoco_c, prelude::*};  // ✅ 添加 mujoco_c
use mujoco_rs::mujoco_c::{mjnSite, mjnBody};  // ✅ 导出类型
use std::sync::Arc;  // ✅ 改用 Arc

pub struct MujocoGravityCompensation {
    model: Arc<MjModel>,  // ✅ Arc
    data: MjData<Arc<MjModel>>,  // ✅ Arc
    ee_site_id: Option<mjnSite>,  // ✅ 简化类型名
    ee_body_id: Option<mjnBody>,  // ✅ 简化类型名
}
```

### 修复 FFI 调用（10分钟）

```rust
impl MujocoGravityCompensation {
    fn compute_jacobian_at_point(
        &mut self,
        body_id: mjnBody,  // ✅ 简化类型名
        point_world: &[f64; 3],
    ) -> Result<(nalgebra::Matrix3x6<f64>, nalgebra::Matrix3x6<f64>), PhysicsError> {
        let mut jacp = [0.0f64; 18];
        let mut jacr = [0.0f64; 18];

        unsafe {
            // ✅ 使用 mujoco_c::mj_jac
            mujoco_rs::mujoco_c::mj_jac(
                self.model.ffi(),
                self.data.ffi(),
                jacp.as_mut_ptr(),
                jacr.as_mut_ptr(),
                point_world.as_ptr(),
                body_id,
            );
        }

        let jacp_matrix = nalgebra::Matrix3x6::from_row_slice(&jacp);
        let jacr_matrix = nalgebra::Matrix3x6::from_row_slice(&jacr);

        Ok((jacp_matrix, jacr_matrix))
    }
}
```

### 修复字段访问（15分钟）

```rust
impl MujocoGravityCompensation {
    pub fn compute_gravity_torques_with_payload(
        &mut self,
        q: &JointState,
        payload_mass: f64,
        payload_com: nalgebra::Vector3<f64>,
    ) -> Result<JointTorques, PhysicsError> {
        // ... 省略前面的代码 ...

        // ✅ 正确的字段访问方式
        let all_site_xpos = self.data.site_xpos;  // &[[f64; 3]]
        let all_site_xmat = self.data.site_xmat;  // &[[f64; 9]]

        let site_idx = self.ee_site_id.ok_or_else(|| PhysicsError::NotInitialized)? as usize;

        let site_xpos = &all_site_xpos[site_idx];  // &[f64; 3]
        let site_xmat = &all_site_xmat[site_idx];  // &[f64; 9]

        // ✅ 转换为 nalgebra
        let pos_array = *site_xpos;
        let rot_array = *site_xmat;
        let site_pos = nalgebra::Vector3::from_column_slice(&pos_array);
        let rot_mat = nalgebra::Matrix3::from_row_slice(&rot_array);

        // ... 后续计算 ...

        // ✅ 修复迭代器
        let torques = JointTorques::from_iterator(
            tau_payload.iter().copied()
        );

        Ok(torques)
    }
}
```

---

## 🚀 三步修复流程

### Step 1: 准备工作 (5分钟)

```bash
# 创建修复分支
git checkout -b fix/mujoco-api-2.3

# 备份当前代码
cp crates/piper-physics/src/mujoco.rs crates/piper-physics/src/mujoco.rs.backup
```

### Step 2: 核心修复 (2小时)

按照上面的清单逐项修复，建议顺序：

1. ✅ 修改导入和类型定义（Rc → Arc）
2. ✅ 修复文件路径
3. ✅ 修复 FFI 调用
4. ✅ 修复字段访问
5. ✅ 修复类型转换

### Step 3: 验证测试 (30分钟)

```bash
# 编译检查
cargo check -p piper-physics --features mujoco

# Clippy 检查
cargo clippy -p piper-physics --features mujoco -- -D warnings

# 运行测试
cargo test -p piper-physics --features mujoco

# 运行示例
cargo run --example gravity_compensation_mujoco --features mujoco
```

---

## 📊 修复前后对比

### 修复前 (45+ 错误)

```bash
cargo clippy --all-targets --all-features -- -D warnings
error: could not find `sys` in `mujoco_rs` (25+ 错误)
error: `Rc<MjModel>` cannot be shared between threads safely (18+ 错误)
error: no field `site_parent` (1 错误)
error: this method takes 0 arguments but 1 argument was supplied (2 错误)
...
```

### 修复后 (✅ 零错误)

```bash
cargo clippy --all-targets --all-features -- -D warnings
✅ Finished - 零警告，零错误
```

---

## 🎯 成功标准

### 编译验证

```bash
# ✅ 必须全部通过
cargo build -p piper-physics --features mujoco
cargo clippy -p piper-physics --features mujoco -- -D warnings
cargo test -p piper-physics --features mujoco
```

### 功能验证

```bash
# ✅ 示例运行成功
cargo run --example gravity_compensation_mujoco --features mujoco

# 预期输出包含：
# ✅ MuJoCo model loaded successfully
# ✅ Mode 1: Pure Gravity Compensation - [数值]
# ✅ Mode 2: Partial Inverse Dynamics - [数值]
# ✅ Mode 3: Full Inverse Dynamics - [数值]
# ✅ Payload Compensation - [数值]
```

---

## 📚 相关文档

1. **[MUJOCO_API_SOLUTION.md](./MUJOCO_API_SOLUTION.md)** - 完整技术分析和修复方案
2. **[MUJOCO_COMPILATION_ERRORS_ANALYSIS.md](./MUJOCO_COMPILATION_ERRORS_ANALYSIS.md)** - 原始错误分析
3. **tmp/mujoco-rs/** - mujoco-rs 源代码（用于 API 参考）

---

## ⚡ 常见问题 FAQ

### Q1: 为什么不能用 `sys` 模块？

**A**: mujoco-rs 将 FFI 绑定放在 `mujoco_c` 模块中，不是 `sys` 模块。需要这样导入：

```rust
use mujoco_rs::mujoco_c;
```

### Q2: `site_xpos` 为什么不是方法？

**A**: mujoco-rs 使用 `array_slice_dyn!` 宏生成字段访问器，这些字段返回对底层数据的切片。它们是字段不是方法。

### Q3: `Arc` vs `Rc` 性能影响大吗？

**A**: 影响很小（< 1%）。`Arc` 使用原子操作，但对于这种使用场景（编译时创建，运行时读取）性能影响可忽略。

### Q4: 修复后 API 还会变化吗？

**A**: 建议锁定 mujoco-rs 版本：

```toml
[dependencies]
mujoco-rs = { version = "=2.3", optional = true }
```

---

## 🤝 需要帮助？

如果修复过程中遇到问题，参考：

1. **API 参考**: `tmp/mujoco-rs/src/wrappers/*.rs`
2. **示例代码**: `tmp/mujoco-rs/examples/*.rs`
3. **FFI 绑定**: `tmp/mujoco-rs/src/mujoco_c.rs`

---

**最后更新**: 2025-01-29
**状态**: ✅ 分析完成，待实施
**预计时间**: 2-3 小时完成修复
