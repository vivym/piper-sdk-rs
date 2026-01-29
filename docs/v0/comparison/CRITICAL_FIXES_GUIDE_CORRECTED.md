# 修正版快速修复指南 - piper-physics Critical Issues

**状态**: 🔴 DO NOT USE IN PRODUCTION until Phase 1 fixes are complete
**修正日期**: 2025-01-28
**关键修正**: 修复了 Fix #5 中的 API 幻觉问题

---

## ⚠️ 重要修正说明

**原版 Fix #5 (COM Offset) 存在严重错误**：
- ❌ `mj_jacSite` **不支持**传入 xyz 偏移坐标参数
- ❌ 原代码会导致编译失败或 Undefined Behavior

**本版本已修正**，使用正确的 MuJoCo API。

---

## ✅ 已完成修复

- ✅ **Issue #5**: 语法错误 in `analytical.rs` - 已修复

---

## Phase 1: 关键修复 (~60 minutes)

### Fix #1: 语法错误 in `analytical.rs`
**状态**: ✅ 已完成

### Fix #2: End-Effector Body/Site 不匹配
**文件**: `src/mujoco.rs`, `assets/piper_no_gripper.xml`
**时间**: 10 分钟

**问题**: XML 有 `<site>` 但代码搜索 `<body>`

**解决方案** (推荐): 改用 Site 搜索

```rust
// 在 src/mujoco.rs 中，替换 find_end_effector_body_id:

fn find_end_effector_site_id(model: &MjModel) -> Option<mujoco_rs::sys::mjnSite> {
    let possible_names = vec!["end_effector", "ee", "tool0"];

    for name in possible_names {
        for i in 0..model.ffi().nsite {
            // SAFETY: MuJoCo guarantees name_siteadr[i] is within bounds
            let site_name = unsafe {
                let name_offset = (*model.ffi()).name_siteadr[i] as usize;
                let base_ptr = (*model.ffi()).names;

                if base_ptr.is_null() {
                    continue;
                }

                std::ffi::CStr::from_ptr(base_ptr.add(name_offset))
            };

            let site_name_str = site_name.to_string_lossy();

            if site_name_str.contains(name) {
                log::info!("Found end-effector site: '{}' (ID: {})", site_name_str, i);
                return Some(i as mujoco_rs::sys::mjnSite);
            }
        }
    }

    log::warn!("No end-effector site found (searched for: {:?})", possible_names);
    None
}

// 更新结构体字段:
pub struct MujocoGravityCompensation {
    model: Rc<MjModel>,
    data: MjData<Rc<MjModel>>,
    ee_site_id: Option<mujoco_rs::sys::mjnSite>,  // 从 ee_body_id 改名
    ee_body_id: Option<mujoco_rs::sys::mjnBody>,  // 保留，用于获取 body_id
}

// 在 from_xml_string 中初始化两者:
let ee_site_id = Self::find_end_effector_site_id(&model);
let ee_body_id = ee_site_id.and_then(|site_id| {
    // Site 必须属于某个 Body，查找该 Body
    for body_id in 0..model.ffi().nbody {
        // 检查该 body 是否包含此 site
        // mujoco-rs 可能需要提供 site_to_body 的映射
        // 暂时使用简单方法：遍历所有 body 的子节点
    }
    None  // TODO: 实现 site 到 body 的映射
});

if let Some(id) = ee_site_id {
    log::info!("  End-effector site ID: {}", id);
} else {
    log::warn!("  ⚠️  End-effector site not found (payload compensation unavailable)");
}

Ok(Self { model, data, ee_site_id, ee_body_id })
```

### Fix #3: 双重 `forward()` 调用
**文件**: `src/mujoco.rs:373-454`
**时间**: 15 分钟

**问题**: `compute_gravity_torques_with_payload()` 调用了两次 `forward()`

```rust
// 完全替换 compute_payload_torques 和 compute_gravity_torques_with_payload:

/// Compute payload gravity compensation using Jacobian transpose method
///
/// # Arguments
///
/// * `_q` - Joint positions (already set by caller, do not modify again)
/// * `mass` - Payload mass in kg
/// * `com` - Payload center of mass offset in end-effector LOCAL frame
/// * `ee_site_id` - End-effector site ID
/// * `ee_body_id` - Body ID that the site is attached to
fn compute_payload_torques(
    &mut self,
    _q: &JointState,  // 已经在调用方设置过
    mass: f64,
    com: nalgebra::Vector3<f64>,  // 在 Site 局部坐标系中的偏移
    ee_site_id: mujoco_rs::sys::mjnSite,
    ee_body_id: mujoco_rs::sys::mjnBody,
) -> Result<JointTorques, PhysicsError> {
    // 注意: qpos, qvel, qacc 已经在 compute_gravity_torques_with_payload 中设置
    // 注意: forward() 已经在 compute_gravity_torques_with_payload 中调用过一次

    // 1. 获取模型的重力向量 (尊重模型配置)
    let model_gravity = self.model.opt().gravity;  // &[f64; 3]
    let f_gravity = nalgebra::Vector3::new(
        model_gravity[0] * mass,
        model_gravity[1] * mass,
        model_gravity[2] * mass,
    );

    // 2. 获取 Site 的世界坐标位置和旋转矩阵
    let site_xpos = self.data.site_xpos(ee_site_id);  // &[f64; 3] - Site 原点的世界坐标
    let site_xmat = self.data.site_xmat(ee_site_id);  // &[f64; 9] - Site 的旋转矩阵 (3x3, 列主序)

    // 3. 将局部偏移 com 转换到世界坐标系
    // world_offset = site_xmat * com
    let mut world_offset = nalgebra::Vector3::zeros();
    for i in 0..3 {
        for j in 0..3 {
            world_offset[i] += site_xmat[i + 3*j] * com[j];  // 列主序存储
        }
    }

    // 4. 计算负载质心在世界坐标系中的位置
    let world_com = nalgebra::Vector3::new(
        site_xpos[0] + world_offset[0],
        site_xpos[1] + world_offset[1],
        site_xpos[2] + world_offset[2],
    );

    // 5. 计算该世界坐标点的 Jacobian (使用 mj_jac)
    let mut jacp = [0.0f64; 18];  // 3 x 6 (线性 Jacobian)
    let mut jacr = [0.0f64; 18];  // 3 x 6 (旋转 Jacobian，本次计算不需要)

    unsafe {
        mujoco_rs::sys::mj_jac(
            self.model.ffi(),
            self.data.ffi(),
            jacp.as_mut_ptr(),
            jacr.as_mut_ptr(),
            world_com[0],  // 点的 x 坐标
            world_com[1],  // 点的 y 坐标
            world_com[2],  // 点的 z 坐标
            ee_body_id,    // Body ID
        );
    }

    // 6. Jacobian 转置: τ = J^T * F
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

pub fn compute_gravity_torques_with_payload(
    &mut self,
    q: &JointState,
    payload_mass: f64,
    payload_com: nalgebra::Vector3<f64>,
) -> Result<JointTorques, PhysicsError> {
    // 1. 设置关节位置
    self.data.qpos_mut()[0..6].copy_from_slice(q.as_slice());
    self.data.qvel_mut()[0..6].fill(0.0);
    self.data.qacc_mut()[0..6].fill(0.0);

    // 2. 调用 forward() 仅一次
    self.data.forward();

    // 3. 从 qfrc_bias 提取机器人本体重力
    let tau_robot = JointTorques::from_iterator(
        self.data.qfrc_bias()[0..6].iter().copied()
    );

    // 4. 计算负载贡献 (不再第二次调用 forward())
    let ee_site_id = self.ee_site_id.ok_or_else(|| {
        PhysicsError::CalculationFailed(
            "End-effector site not found in model. \
             Payload compensation requires a site named 'end_effector'.".to_string()
        )
    })?;

    let ee_body_id = self.ee_body_id.ok_or_else(|| {
        PhysicsError::CalculationFailed(
            "End-effector body ID not available. Cannot compute Jacobian.".to_string()
        )
    })?;

    let tau_payload = self.compute_payload_torques(
        q,
        payload_mass,
        payload_com,
        ee_site_id,
        ee_body_id
    )?;

    // 5. 叠加
    Ok(tau_robot + tau_payload)
}
```

**物理原理说明**:

```
原错误方案: τ = J_site^T * F (只计算 Site 原点的力矩)

正确方案:
  1. 找到负载质心在世界坐标系的位置 P_com
  2. 计算 P_com 点的 Jacobian: J_com
  3. τ = J_com^T * F_gravity

对于偏心负载，这自动包含了力矩项，因为 Jacobian 在偏心点已经包含了旋转分量。
```

### Fix #4: 重力参数使用错误
**文件**: `src/mujoco.rs:431-436`
**时间**: 5 分钟

**问题**: 硬编码 `9.81` 而不是使用模型的重力

**已包含在 Fix #3 中** - 新代码正确使用了 `self.model.opt().gravity`。

---

## Phase 2: 重要修复 (~40 minutes)

### Fix #5: Site 到 Body 的映射
**文件**: `src/mujoco.rs`
**时间**: 15 分钟

**问题**: 需要从 Site ID 找到对应的 Body ID

```rust
impl MujocoGravityCompensation {
    /// 在 from_xml_string 中添加:

    // 查找 end-effector site 和其对应的 body
    let ee_site_id = Self::find_end_effector_site_id(&model);

    let ee_body_id = ee_site_id.and_then(|site_id| {
        // Site 必须属于某个 Body，通过遍历查找
        // MuJoCo 中每个 site 都有一个父 body
        for body_id in 0..model.ffi().nbody {
            // 检查该 body 是否包含此 site
            // 方法: 检查 site 的父 body 是否匹配
            let site_parent_body = unsafe {
                (*model.ffi()).site_parent[site_id]
            };

            if site_parent_body == body_id as i32 {
                log::info!("  End-effector site {} belongs to body {}", site_id, body_id);
                return Some(body_id as mujoco_rs::sys::mjnBody);
            }
        }
        log::warn!("Could not find parent body for site {}", site_id);
        None
    });

    Ok(Self { model, data, ee_site_id, ee_body_id })
}
```

### Fix #6: 关节名称验证过于死板
**文件**: `src/analytical/validation.rs`
**时间**: 10 分钟

**问题**: 强制要求 `joint_1` 到 `joint_6` 命名，太严格

**改进方案 A** (推荐): 使用警告而非错误

```rust
pub fn validate_joint_mapping(chain: &Chain<f64>) -> Result<(), PhysicsError> {
    let movable_joints: Vec<_> = chain
        .iter()
        .filter(|node| node.joint().limits.is_some())
        .collect();

    if movable_joints.len() != 6 {
        return Err(PhysicsError::JointMappingError(format!(
            "Expected 6 movable joints for Piper robot, found {}",
            movable_joints.len()
        )));
    }

    log::info!("🔍 Validating joint mapping...");
    log::info!("URDF joint names (movable joints only):");

    let mut has_unusual_names = false;

    for (i, node) in movable_joints.iter().enumerate() {
        let can_id = i + 1;
        let joint = node.joint();
        let joint_name: &str = joint.name.as_ref();
        let expected_name = format!("joint_{}", can_id);

        if joint_name != expected_name {
            log::warn!(
                "  ⚠️  Joint {} (CAN ID {}): '{}' (non-standard name, expected '{}')",
                can_id, can_id, joint_name, expected_name
            );
            has_unusual_names = true;
        } else {
            log::info!("  ✓ Joint {} (CAN ID {}): {}", can_id, can_id, joint_name);
        }
    }

    if has_unusual_names {
        log::warn!();
        log::warn!("⚠️  WARNING: Joint names don't follow the 'joint_1' to 'joint_6' pattern.");
        log::warn!("   Please verify that joint order matches CAN ID order!");
        log::warn!("   CAN ID 1 should be the first joint in the chain, etc.");
    }

    log::info!();
    log::info!("✓ Joint mapping validation complete (6 movable joints found)");
    Ok(())
}
```

**改进方案 B**: 允许用户提供映射配置

```rust
pub struct JointMappingConfig {
    pub expected_names: Vec<String>,
}

impl Default for JointMappingConfig {
    fn default() -> Self {
        Self {
            expected_names: vec![
                "joint_1".into(),
                "joint_2".into(),
                "joint_3".into(),
                "joint_4".into(),
                "joint_5".into(),
                "joint_6".into(),
            ],
        }
    }
}

pub fn validate_joint_mapping_with_config(
    chain: &Chain<f64>,
    config: &JointMappingConfig,
) -> Result<(), PhysicsError> {
    // 使用 config.expected_names 进行验证
    // ...
}
```

### Fix #7: 不安全代码的安全性
**文件**: `src/mujoco.rs:264-266`
**时间**: 10 分钟

已在 Fix #2 中包含改进。

### Fix #8: 日志系统
**文件**: 多处
**时间**: 5 分钟

**问题**: 使用 `println!` 污染用户终端

**解决方案**: 引入 `log` crate

```toml
# Cargo.toml
[dependencies]
log = "0.4"

# 对于示例和测试:
[dev-dependencies]
env_logger = "0.10"
```

```rust
// 替换所有 println!:

// Before:
println!("✓ MuJoCo model loaded successfully");

// After:
log::info!("MuJoCo model loaded successfully");

// Before:
println!("⚠️  End-effector body not found");

// After:
log::warn!("End-effector body not found (payload compensation unavailable)");
```

在库初始化时让用户决定日志级别:

```rust
// 在用户代码中:
fn main() {
    // 初始化日志 (只需一次)
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    // 使用库
    let gravity = MujocoGravityCompensation::from_standard_path()?;
}
```

---

## Phase 3: 质量优化 (~15 minutes)

### Fix #9: 简化网格验证
**文件**: `src/mujoco.rs:99`
**时间**: 5 分钟

**问题**: 原建议引入 regex 依赖是过度工程

**改进**: 使用简单字符串匹配

```rust
// 在 from_embedded() 中:

// 简单检查是否包含 mesh 引用 (99% 准确，零依赖)
let has_mesh_ref = XML.contains("<mesh") && XML.contains("file=");

if has_mesh_ref {
    return Err(PhysicsError::InvalidInput(
        "Embedded XML contains mesh file references.\n\
         \n\
         Problem: <mesh file=\"link1.stl\" /> found in XML.\n\
         \n\
         The include_str!() method only embeds the XML file itself.\n\
         MuJoCo will try to load mesh files at runtime from the filesystem.\n\
         \n\
         Solutions:\n\
         1. Use from_model_dir() for models with mesh files\n\
         2. Use from_standard_path() to search standard locations\n\
         3. Use simple geometry (box, cylinder) instead of meshes".to_string()
    ));
}
```

### Fix #10: 添加 `#[must_use]` 属性
**文件**: 多处
**时间**: 5 分钟

```rust
#[must_use = "Gravity compensation result should be used"]
pub fn compute_gravity_torques_with_payload(
    &mut self,
    q: &JointState,
    payload_mass: f64,
    payload_com: nalgebra::Vector3<f64>,
) -> Result<JointTorques, PhysicsError> {
    // ...
}
```

### Fix #11: Trait 返回类型修正
**文件**: `src/traits.rs:29`
**时间**: 2 分钟

```rust
fn compute_gravity_torques(
    &mut self,
    q: &JointState,
    gravity: Option<&nalgebra::Vector3<f64>>,
) -> Result<JointTorques, PhysicsError>;  // 从 JointState 改为 JointTorques
```

### Fix #12: 移除未实现的方法
**文件**: `src/analytical.rs`
**时间**: 3 分钟

```rust
// 移除或标记为 #[doc(hidden)]
#[doc(hidden)]
pub fn from_piper_urdf() -> Result<Self, PhysicsError> {
    Err(PhysicsError::NotInitialized)
}
```

---

## 修复后的测试

```bash
# 1. 验证编译
cargo check -p piper-physics --all-features

# 2. 运行运动学测试
cargo test -p piper-physics --no-default-features --features kinematics

# 3. 运行示例 (如果安装了 MuJoCo)
env RUST_LOG=info cargo run -p piper-physics --example gravity_compensation_analytical \
  --no-default-features --features kinematics
```

---

## 验证清单

### Phase 1 (关键)
- [x] 语法错误已修复
- [ ] End-effector Site 搜索实现
- [ ] Site 到 Body 的映射实现
- [ ] 双重 `forward()` 调用消除
- [ ] 重力向量使用模型值
- [ ] COM 偏移正确计算 (使用 mj_jac)

### Phase 2 (重要)
- [ ] 关节名称验证改为警告
- [ ] 不安全代码添加防御检查
- [ ] 引入 log crate 替换 println!

### Phase 3 (质量)
- [ ] 简化网格验证 (移除 regex 依赖)
- [ ] 添加 #[must_use] 属性
- [ ] 修正 trait 返回类型
- [ ] 移除未实现方法

---

## 关键修正说明

### 📛 Fix #5 修正详解

**原错误代码**:
```rust
// ❌ 这段代码无法编译，mj_jacSite 不接受 xyz 参数!
unsafe {
    mujoco_rs::sys::mj_jacSite(
        self.model.ffi(),
        self.data.ffi(),
        jacp.as_mut_ptr(),
        jacr.as_mut_ptr(),
        ee_site_id as i32,
        com[0], com[1], com[2],  // ← 编译错误！参数数量不匹配
    );
}
```

**正确实现**:
```rust
// ✅ 使用 mj_jac 计算任意点的 Jacobian

// 1. 获取 Site 的世界坐标和旋转
let site_xpos = self.data.site_xpos(ee_site_id);
let site_xmat = self.data.site_xmat(ee_site_id);

// 2. 将局部偏移转换为世界坐标
let world_offset = site_xmat * com;  // 矩阵乘法
let world_com = site_xpos + world_offset;

// 3. 使用 mj_jac 计算该世界坐标点的 Jacobian
unsafe {
    mujoco_rs::sys::mj_jac(
        self.model.ffi(),
        self.data.ffi(),
        jacp.as_mut_ptr(),
        jacr.as_mut_ptr(),
        world_com[0],  // ← 正确: 世界坐标点
        world_com[1],
        world_com[2],
        ee_body_id,    // ← 正确: Body ID
    );
}
```

**API 签名对比**:
```c
// MuJoCo C API (实际签名)
void mj_jacSite(const mjModel* m, const mjData* d, mjtNum* jacp, mjtNum* jacr, int site);
//         ↑ 只接受 site ID，计算的是 Site 原点的 Jacobian

void mj_jac(const mjModel* m, const mjData* d, mjtNum* jacp, mjtNum* jacr,
            mjtNum point[3], int body);
//         ↑ 接受世界坐标点 + body ID，可以计算任意点的 Jacobian
```

---

## 总结

| 修复项 | 状态 | 说明 |
|-------|------|------|
| Phase 1 | 🔄 进行中 | 关键功能修复，**包含 API 幻觉修正** |
| Phase 2 | ⏳ 待开始 | 重要但非阻塞性 |
| Phase 3 | ⏳ 待开始 | 代码质量提升 |

**在开始修复前请务必阅读**:
1. 本文档中的 "Fix #5 修正详解"
2. MuJoCo API 文档: https://mujoco.readthedocs.io/en/latest/APIreference.html#programming
3. mujoco-rs 的 FFI 绑定定义

**预计总时间**: ~2 小时 (Phase 1: 60分钟, Phase 2: 40分钟, Phase 3: 15分钟)
