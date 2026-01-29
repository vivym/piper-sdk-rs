# MuJoCo-rs 最佳实践 - 关键修正（CRITICAL CORRECTIONS）

**日期**: 2025-01-28
**状态**: ⚠️ 严重工程缺陷已识别，需立即修正

---

## 执行摘要

原最佳实践报告在**代码结构、API 模式和性能优化**方面是正确的，但在**实际工程落地**上存在**4个致命缺陷**，如果直接照此执行，代码会运行失败或产生严重问题。

---

## 🔴 致命缺陷 #1: `include_str!` 与外部网格文件（Mesh Files）

### 问题描述

原报告在 `4.2` 和 `9.1` 中强烈推荐：

```rust
// ❌ 错误做法（会导致运行时失败）
const XML: &str = include_str!("../../assets/piper_no_gripper.xml");
Self::from_xml_string(XML)
```

**为什么会导致失败**：

1. **Piper 机器人模型依赖外部 STL/OBJ 网格文件**
   - MJCF XML 中通常包含：`<mesh file="link1.stl"/>` 或 `<mesh file="link2.obj"/>`
   - 这些网格文件用于碰撞检测、可视化、惯性计算

2. **`include_str!` 只嵌入了 XML 文本**
   - XML 文本被嵌入到二进制中 ✅
   - STL/OBJ 文件**没有**被嵌入 ❌

3. **运行时加载失败**
   - MuJoCo 解析 XML 字符串时，会尝试在**当前工作目录**下寻找 `link1.stl`
   - 由于 STL 文件不存在 → **100% 加载失败**
   - 错误信息类似：`Error: Cannot open mesh file "link1.stl"`

### 验证问题

检查您的 MJCF XML 文件：

```xml
<geom name="link_1_geom" type="mesh" mesh="link1_mesh"/>
<!-- 或者 -->
<mesh name="link1_mesh" file="link1.stl" scale="1 1 1"/>
```

如果包含 `<mesh file="..."/>`，则 `include_str!` 方案**不可行**。

### ✅ 修正方案 1: 使用 MuJoCo 虚拟文件系统（VFS）

**适用于**: 需要零配置、嵌入式部署的场景

```rust
use mujoco_rs::prelude::*;
use std::ffi::CString;

const PIPER_XML: &str = include_str!("../../assets/piper_no_gripper.xml");
const LINK1_STL: &[u8] = include_bytes!("../../assets/link1.stl");
const LINK2_STL: &[u8] = include_bytes!("../../assets/link2.stl");
// ... 所有 STL 文件

impl MujocoGravityCompensation {
    pub fn from_embedded() -> Result<Self, PhysicsError> {
        // 1. 创建 VFS（虚拟文件系统）
        let vfs = mujoco_rs::util::Vfs::new();

        // 2. 将所有 STL 文件写入 VFS
        vfs.add_file("link1.stl", LINK1_STL)
            .map_err(|e| PhysicsError::ModelLoadError(e.to_string()))?;
        vfs.add_file("link2.stl", LINK2_STL)
            .map_err(|e| PhysicsError::ModelLoadError(e.to_string()))?;
        // ... 添加所有网格文件

        // 3. 从 VFS 加载模型
        let model = Rc::new(
            MjModel::from_xml_string_vfs(PIPER_XML, &vfs)
                .map_err(|e| PhysicsError::ModelLoadError(e.to_string()))?
        );

        let data = MjData::new(model.clone());

        Ok(Self { model, data })
    }
}
```

**优点**：
- ✅ 真正的零配置（所有资源嵌入二进制）
- ✅ 部署简单（单文件）

**缺点**：
- ❌ 二进制体积激增（STL 文件可能 10MB+）
- ❌ 编译时间长
- ❌ 更新模型需要重新编译

### ✅ 修正方案 2: 文件路径加载（推荐）

**适用于**: 通用库、需要灵活更新模型的场景

```rust
impl MujocoGravityCompensation {
    /// 从文件系统加载（推荐用于生产环境）
    pub fn from_model_dir(dir: &std::path::Path) -> Result<Self, PhysicsError> {
        let xml_path = dir.join("piper_no_gripper.xml");

        // 检查 XML 文件是否存在
        if !xml_path.exists() {
            return Err(PhysicsError::ModelLoadError(format!(
                "MJCF XML file not found: {:?}. \
                 Please ensure the model files are in the correct directory.",
                xml_path
            )));
        }

        // MuJoCo 会自动从同一目录加载 STL/OBJ 文件
        let model = Rc::new(
            MjModel::from_xml_path(&xml_path)
                .map_err(|e| PhysicsError::ModelLoadError(e.to_string()))?
        );

        let data = MjData::new(model.clone());

        Ok(Self { model, data })
    }

    /// 从标准路径加载（环境变量或默认路径）
    pub fn from_standard_path() -> Result<Self, PhysicsError> {
        // 1. 尝试环境变量
        if let Ok(dir) = std::env::var("PIPER_MODEL_PATH") {
            return Self::from_model_dir(std::path::Path::new(&dir));
        }

        // 2. 尝试用户目录
        let home_dirs = vec![
            std::path::PathBuf::from("~/.piper/models"),
            std::path::PathBuf::from("/usr/local/share/piper/models"),
            std::path::PathBuf::from("./assets"),  // 开发环境
        ];

        for dir in home_dirs {
            if dir.exists() {
                return Self::from_model_dir(&dir);
            }
        }

        Err(PhysicsError::ModelLoadError(
            "Model files not found. Set PIPER_MODEL_PATH environment variable \
             or ensure model files are in ~/.piper/models/".to_string()
        ))
    }
}
```

**优点**：
- ✅ 模型文件独立，易于更新
- ✅ 二进制体积小
- ✅ 符合 Linux/Unix 标准实践
- ✅ 用户可以自定义模型

**缺点**：
- ❌ 需要文件路径配置
- ❌ 部署时需要复制模型文件

### ✅ 修正方案 3: 混合方案（最佳实践）

仅用于**简单几何体**（无网格文件）的模型使用 `include_str!`：

```rust
impl MujocoGravityCompensation {
    /// 从嵌入式 XML 加载（仅用于无网格文件的简单模型）
    ///
    /// ⚠️ 警告: 此方法仅适用于使用基本几何体（box、cylinder、sphere）的 MJCF 文件。
    /// 如果 MJCF 文件引用了外部 STL/OBJ 网格文件，此方法将失败。
    ///
    /// 对于包含网格文件的模型，请使用 `from_model_dir()` 或 `from_standard_path()`。
    pub fn from_embedded_simple_geometry() -> Result<Self, PhysicsError> {
        const XML: &str = include_str!("../../assets/piper_simple.xml");  // 仅包含基本几何体

        // 验证 XML 不包含 mesh 引用（简单的字符串检查）
        if XML.contains("<mesh") || XML.contains("file=\"") {
            return Err(PhysicsError::InvalidInput(
                "Embedded XML cannot contain mesh file references. \
                 Use from_model_dir() for models with mesh files.".to_string()
            ));
        }

        Self::from_xml_string(XML)
    }
}
```

---

## ⚠️ 严重缺陷 #2: 僵化的负载配置策略

### 问题描述

原报告在 `12.3` 中建议：

```rust
// ❌ 僵化做法（枚举式 XML）
const DEFAULT_PIPER_NO_GRIPPER: &str = include_str!("piper_no_gripper.xml");
const DEFAULT_PIPER_SMALL_GRIPPER: &str = include_str!("piper_small_gripper.xml");
const DEFAULT_PIPER_LARGE_GRIPPER: &str = include_str!("piper_large_gripper.xml");
```

**为什么不可行**：

1. **无法覆盖所有场景**
   - 用户抓取 325g 物体怎么办？
   - 用户换了一个 780g 的非标夹爪怎么办？
   - 无法为每克重量都准备一个 XML 文件

2. **动态负载需求**
   - 机器人运行过程中，抓取物体的重量会变化
   - 不可能在运行时重新加载 MJCF XML

3. **MuJoCo 模型通常被视为只读**
   - `MjModel` 在加载后理论上应该 immutable
   - 修改 `body_mass` 等字段虽然可能，但破坏了 MuJoCo 的设计假设

### ✅ 修正方案: 混合重力补偿算法

**核心思想**: MuJoCo 计算机器人本体 + 手动计算末端负载

```rust
use nalgebra::{Vector3, Vector6, Matrix3x6};

impl MujocoGravityCompensation {
    /// 计算重力补偿力矩（机器人本体 + 末端负载）
    pub fn compute_gravity_torques_with_payload(
        &mut self,
        q: &JointState,
        payload_mass: f64,        // 负载质量 (kg)
        payload_com: Vector3<f64>, // 负载质心位置 (相对于末端坐标系)
    ) -> Result<JointTorques, PhysicsError> {
        // 1. 计算机器人本体重力补偿（通过 MuJoCo）
        let tau_robot = self.compute_gravity_torques(q, None)?;

        // 2. 计算负载重力补偿（手动 Jacobian 转置方法）
        let tau_payload = self.compute_payload_torques(q, payload_mass, payload_com)?;

        // 3. 叠加
        Ok(tau_robot + tau_payload)
    }

    /// 计算负载重力补偿力矩（使用 Jacobian 转置）
    fn compute_payload_torques(
        &mut self,
        q: &JointState,
        mass: f64,
        com: Vector3<f64>,  // 在末端坐标系中
    ) -> Result<JointTorques, PhysicsError> {
        // 1. 设置关节位置
        self.data.qpos_mut()[0..6].copy_from_slice(q.as_slice());
        self.data.qvel_mut()[0..6].fill(0.0);
        self.data.qacc_mut()[0..6].fill(0.0);

        // 2. 调用 forward() 更新几何信息
        self.data.forward();

        // 3. 获取末端执行器 Jacobian (3x6 矩阵，线性速度部分)
        // 假设 ee_body_id 已经通过名称查找获取
        let jacp = self.data.jacp(self.ee_body_id);  // Point Jacobian (3 x nv)

        // 4. 负载的重力矢量（世界坐标系）
        let gravity = self.model.opt().gravity;  // [0, 0, -9.81]
        let f_gravity = Vector3::new(0.0, 0.0, -mass * 9.81);

        // 5. Jacobian 转置法: τ = J^T * F
        // jacp 是 3x6 矩阵，f_gravity 是 3x1 矢量
        let jacp_matrix = Matrix3x6::from_column_iterator(
            jacp.iter().copied()
        );
        let tau_payload = jacp_matrix.transpose() * f_gravity;

        Ok(JointTorques::from_iterator(tau_payload.iter()))
    }
}
```

**API 使用示例**：

```rust
// 空载
let tau_empty = gravity_comp.compute_gravity_torques(&q, None)?;

// 抓取 500g 物体（质心在末端坐标系原点）
let tau_with_load = gravity_comp.compute_gravity_torques_with_payload(
    &q,
    0.5,                           // 500g
    Vector3::new(0.0, 0.0, 0.0),  // 质心在原点
)?;

// 抓取不规则物体（质心偏移）
let tau_irregular = gravity_comp.compute_gravity_torques_with_payload(
    &q,
    0.325,                          // 325g
    Vector3::new(0.05, 0.02, 0.1), // 质心向前 5cm, 向右 2cm, 向上 1cm
)?;
```

**优点**：
- ✅ 完全动态的负载配置
- ✅ 可以在运行时随意改变负载
- ✅ 精确的质心位置控制
- ✅ 符合物理原理（Jacobian 转置法）

**数学原理**：

```
τ_payload = J^T * F_gravity

其中：
- J: 末端执行器的 Jacobian 矩阵 (6x6)，取线性速度部分 (3x6)
- F_gravity: 负载重力矢量 (3x1) = [0, 0, -m*g]^T
- τ_payload: 关节力矩 (6x1)
```

---

## ⚠️ 严重缺陷 #3: 编译环境要求未说明

### 问题描述

原报告**完全没有提及** `mujoco-rs` 的编译环境要求。

**问题**：
- `mujoco-rs` 不是纯 Rust 库
- 需要用户系统上安装 MuJoCo C++ 库
- 需要设置环境变量 `MUJOCO_DIR` 或 `LD_LIBRARY_PATH`
- 如果不处理，用户运行 `cargo build` 会看到复杂的链接错误

### ✅ 修正方案 1: 修改 Feature 策略

**原 Cargo.toml（错误）**：
```toml
[features]
default = ["mujoco"]  # ❌ 默认启用会导致所有用户编译失败
mujoco = ["dep:mujoco-rs"]
```

**修正后（正确）**：
```toml
[features]
default = []  # ✅ 默认不启用 MuJoCo，避免编译失败
kinematics = ["dep:k"]  # k crate（纯 Rust，无需外部依赖）
mujoco = ["dep:mujoco-rs"]  # MuJoCo（需要额外安装）

# 注意：用户必须显式启用 mujoco feature
```

**安装说明（必须放在文档显著位置）**：

```markdown
## 安装 MuJoCo

### 方法 1: 使用包管理器（推荐）

**macOS**:
```bash
brew install pkgconf mujoco
```

**Ubuntu/Debian**:
```bash
sudo apt-get update
sudo apt-get install libmujoco-dev pkg-config
```

### 方法 2: 手动安装

1. 下载 MuJoCo: https://github.com/google-deepmind/mujoco/releases
2. 解压到 `/opt/mujoco`
3. 设置环境变量：

```bash
# 添加到 ~/.bashrc 或 ~/.zshrc
export MUJOCO_DIR=/opt/mujoco
export LD_LIBRARY_PATH=$MUJOCO_DIR/lib:$LD_LIBRARY_PATH
export PKG_CONFIG_PATH=$MUJOCO_DIR/lib/pkgconfig:$PKG_CONFIG_PATH
```

### 验证安装

```bash
# 应该输出 MuJoCo 版本信息
pkg-config --modversion mujoco

# 或
echo $MUJOCO_DIR
```

## 使用

```toml
# Cargo.toml
[dependencies]
piper-physics = { version = "0.0.3", features = ["mujoco"] }
```
```

### ✅ 修正方案 2: 添加 Build Script 检查

在 `crates/piper-physics/build.rs` 中：

```rust
fn main() {
    #[cfg(feature = "mujoco")]
    {
        println!("cargo:warning=Checking for MuJoCo installation...");

        // 尝试使用 pkg-config 检查
        let pkg_config_ok = std::process::Command::new("pkg-config")
            .args(&["--exists", "mujoco"])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);

        if !pkg_config_ok {
            println!("cargo:warning=⚠️  MuJoCo not found via pkg-config");
            println!("cargo:warning=Please install MuJoCo:");
            println!("cargo:warning=  macOS: brew install mujoco");
            println!("cargo:warning=  Ubuntu: sudo apt-get install libmujoco-dev");
            println!("cargo:warning=See: https://github.com/google-deepmind/mujoco/blob/main/BUILD.md");
            println!("cargo:warning=Alternatively, disable the 'mujoco' feature:");
            println!("cargo:warning=  cargo build --no-default-features");
        } else {
            let version = std::process::Command::new("pkg-config")
                .args(&["--modversion", "mujoco"])
                .output()
                .ok();

            if let Some(output) = version {
                let version_str = String::from_utf8_lossy(&output.stdout);
                println!("cargo:warning=✓ MuJoCo found: {}", version_str.trim());
            }
        }
    }
}
```

---

## ⚠️ 隐患 #4: 摩擦力与阻尼缺失

### 问题描述

原报告在 `3.2` 中声称：

> `qfrc_bias` = 纯重力力矩（当 qvel=0 时）

**这是不完整的**：

1. **`qfrc_bias` 的实际组成**：
   ```
   qfrc_bias = gravity + coriolis + centrifugal + spring + damper + fluid drag
   ```
   - 当 `qvel=0` 时：Coriolis = 0, centrifugal = 0, damper = 0, fluid drag = 0
   - 但是：**关节干摩擦（stiction）依然存在**

2. **工程实践中的问题**：
   - 仅补偿重力，机器人仍会缓慢下垂
   - 这是因为关节内部存在**静摩擦（stiction）**和**库仑摩擦**
   - 需要额外的摩擦补偿

### ✅ 修正方案: 明确界定范围

在文档中添加明确的说明：

```markdown
## 范围界定

### 本模块提供的内容

✅ **重力模型补偿** (Gravity Model Compensation)
- 计算机器人各关节在重力场中所需的静态平衡力矩
- 使用 MuJoCo 的 qfrc_bias 字段（qvel=0, qacc=0）
- 适用于：让机械臂在任意姿态"悬停"

### 本模块不提供的内容

❌ **摩擦力补偿** (Friction Compensation)
- 库仑摩擦（Coulomb friction）：常数摩擦力矩
- 静摩擦（Stiction）：启动时的额外阻力
- 粘滞阻尼（Viscous damping）：与速度成正比的阻力

❌ **其他动力学效应**
- Coriolis 力（高速运动时）
- 离心力（高速运动时）

### 工程建议

如果发现机器人即使补偿了重力仍会缓慢下垂：
1. **首先检查**：重力补偿计算是否正确（尝试零姿态，torques 应接近 0）
2. **如果重力计算正确**：说明需要添加摩擦补偿
3. **简单摩擦补偿**：在重力力矩基础上叠加常数
   ```rust
   const FRICTION_COMPENSATION: [f64; 6] = [0.05, 0.05, 0.03, 0.02, 0.01, 0.01];
   let tau_total = tau_gravity + Vector6::from_iterator(FRICTION_COMPENSATION.iter().copied());
   ```
4. **高级摩擦补偿**：使用 LuGre 模型或基于观测器的自适应摩擦补偿

### 完整的阻抗控制

对于高性能的阻抗控制/导纳控制，通常需要：
```
τ_total = τ_gravity + τ_friction + τ_coriolis + τ_centrifugal + τ_control
```

本模块仅负责 `τ_gravity` 部分。
```

---

## 📝 修正后的最佳实践总结

### 模型加载策略

| 场景 | 推荐方案 | 说明 |
|------|---------|------|
| **开发环境** | `from_model_dir("./assets")` | 文件路径加载 |
| **生产环境** | `from_standard_path()` | 环境变量 + 标准路径 |
| **嵌入式部署** | VFS + `include_bytes!` | 仅当必须零配置时 |
| **简单模型** | `include_str!()` | 仅当无网格文件时 |

### 负载配置策略

| 需求 | 推荐方案 |
|------|---------|
| **固定负载** | 不同 XML 文件（开发阶段） |
| **动态负载** | 混合算法（MuJoCo + Jacobian 转置） |
| **任意重量** | `compute_gravity_torques_with_payload()` |

### Feature 配置

```toml
[features]
default = []  # 不启用 MuJoCo，避免编译失败
kinematics = ["dep:k"]
mujoco = ["dep:mujoco-rs"]  # 用户显式启用
```

### 安装文档

必须包含：
1. MuJoCo 安装步骤
2. 环境变量配置
3. 验证安装的方法
4. 编译失败的排查指南

---

## ✅ 修正后的代码示例

### 完整的 MujocoGravityCompensation API

```rust
impl MujocoGravityCompensation {
    // === 模型加载 ===

    /// 推荐: 从标准路径加载（环境变量或系统路径）
    pub fn from_standard_path() -> Result<Self, PhysicsError> { ... }

    /// 从指定目录加载（需要包含 XML 和所有网格文件）
    pub fn from_model_dir(dir: &Path) -> Result<Self, PhysicsError> { ... }

    /// 从 VFS 加载（嵌入式部署，二进制体积大）
    #[cfg(feature = "embedded-vfs")]
    pub fn from_embedded_vfs() -> Result<Self, PhysicsError> { ... }

    /// 从简单几何体加载（无网格文件）
    #[cfg(feature = "embedded-simple")]
    pub fn from_embedded_simple() -> Result<Self, PhysicsError> { ... }

    // === 重力补偿 ===

    /// 基础: 机器人本体重力补偿
    pub fn compute_gravity_torques(&mut self, q: &JointState)
        -> Result<JointTorques, PhysicsError> { ... }

    /// 动态: 机器人本体 + 末端负载
    pub fn compute_gravity_torques_with_payload(
        &mut self,
        q: &JointState,
        payload_mass: f64,
        payload_com: Vector3<f64>,
    ) -> Result<JointTorques, PhysicsError> { ... }

    // === 摩擦补偿（可选）===

    /// 简单的常数摩擦补偿（实验用）
    pub fn set_friction_compensation(&mut self, friction: [f64; 6]) { ... }
}
```

---

## 总结

| 问题 | 严重性 | 修正方案 |
|------|--------|---------|
| `include_str!` 无法加载网格文件 | 🔴 致命 | 使用 VFS 或文件路径加载 |
| 僵化的负载配置 | 🔴 严重 | 混合算法（MuJoCo + Jacobian） |
| 编译环境未说明 | 🔴 严重 | Feature 默认禁用 + 详细安装文档 |
| 摩擦力缺失 | ⚠️ 隐患 | 明确界定范围，提供摩擦补偿建议 |

**修正后的状态**: ✅ 可以安全用于实际工程
