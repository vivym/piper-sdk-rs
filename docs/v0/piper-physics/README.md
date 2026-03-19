# piper-physics 文档索引（当前入口 + 历史记录）

## 当前状态

`piper-physics-mujoco` 当前已经有可用实现，且支持：

- 双臂 MuJoCo 补偿桥接
- 双臂 MIT 遥操 / 双边控制 demo
- 运行时 payload / dynamics mode 热更新

当前有效入口：

- **正式联调文档**: [DUAL_ARM_BILATERAL_ROBOT_GUIDE.md](./DUAL_ARM_BILATERAL_ROBOT_GUIDE.md)
- **联调记录模板**: [DUAL_ARM_BILATERAL_TUNING_LOG_TEMPLATE.md](./DUAL_ARM_BILATERAL_TUNING_LOG_TEMPLATE.md)
- **Addon README**: [../../../addons/piper-physics-mujoco/README.md](../../../addons/piper-physics-mujoco/README.md)
- **双臂 demo**: [../../../addons/piper-physics-mujoco/examples/dual_arm_bilateral_mujoco.rs](../../../addons/piper-physics-mujoco/examples/dual_arm_bilateral_mujoco.rs)
- **双臂 MuJoCo bridge**: [../../../addons/piper-physics-mujoco/src/dual_arm.rs](../../../addons/piper-physics-mujoco/src/dual_arm.rs)

建议阅读顺序：

1. [DUAL_ARM_BILATERAL_ROBOT_GUIDE.md](./DUAL_ARM_BILATERAL_ROBOT_GUIDE.md)
2. [DUAL_ARM_BILATERAL_TUNING_LOG_TEMPLATE.md](./DUAL_ARM_BILATERAL_TUNING_LOG_TEMPLATE.md)
3. [../../../addons/piper-physics-mujoco/README.md](../../../addons/piper-physics-mujoco/README.md)
4. [../../../addons/piper-physics-mujoco/examples/dual_arm_bilateral_mujoco.rs](../../../addons/piper-physics-mujoco/examples/dual_arm_bilateral_mujoco.rs)

---

## 历史说明

本文件下方内容保留的是 **2025 年早期阶段的历史问题分析**，主要记录当时 MuJoCo 编译错误和修复思路。

它们仍然有参考价值，但**不应再被视为当前实现状态**。

如果你关注当前可运行方案，请以上面的“当前有效入口”为准。

---

# MuJoCo Feature 编译错误摘要（历史文档）

> **详细分析报告**: [MUJOCO_COMPILATION_ERRORS_ANALYSIS.md](./MUJOCO_COMPILATION_ERRORS_ANALYSIS.md)

## 🚨 当前状态

**mujoco feature 目前有 45+ 编译错误，完全无法使用。**

### 快速决策指南

| 你的需求 | 推荐方案 | 预计工时 |
|---------|---------|---------|
| mujoco 是核心功能，必须修复 | 策略 A: 完整修复 | 15-20 小时 |
| 可以暂时禁用，后续修复 | 策略 B: 临时禁用 | 1 小时 |
| 需要基本功能即可 | 策略 C: API 降级 | 8-10 小时 |

---

## 📊 错误分类

### 🔴 CRITICAL (43 错误)

1. **mujoco_rs::sys 模块不存在** (25+ 处)
   - 所有使用 `mujoco_rs::sys::mjnSite`, `mjnBody` 等的地方
   - FFI 函数调用: `mj_jac`, `mj_inverse` 等

2. **Rc<MjModel> 不满足 Send + Sync** (18 处)
   - `GravityCompensation` trait 要求 `Send + Sync`
   - `std::rc::Rc` 不是 thread-safe 的

### 🟡 MEDIUM (1 错误)

3. **文件路径错误**
   - `include_str!("../../assets/piper_no_gripper.xml")` → 应为 `../assets/...`

### 🟢 LOW (3 错误)

4. **测试宏缺失**: `assert_relative_eq!`
5. **未使用的导入**: `info`, `warn`
6. **类型转换问题**: 矩阵乘法、迭代器等

---

## 🚀 快速修复 (策略 B: 临时禁用)

如果选择临时禁用 mujoco feature:

### 1. 修改 Cargo.toml

```toml
# crates/piper-physics/Cargo.toml
[features]
default = ["kinematics"]
kinematics = []  # Analytical RNE (no external deps)

# mujoco feature is currently broken due to API changes in mujoco-rs 2.3
# See docs/v0/piper-physics/MUJOCO_COMPILATION_ERRORS_ANALYSIS.md for details
# mujoco = ["dep:mujoco-rs"]
```

### 2. 在主 README.md 添加警告

```markdown
## ⚠️ Known Issues

### MuJoCo Feature

The `mujoco` feature is **currently broken** due to breaking API changes in `mujoco-rs` 2.3.
- Use the `kinematics` feature for basic functionality
- See [analysis report](docs/v0/piper-physics/MUJOCO_COMPILATION_ERRORS_ANALYSIS.md) for details
- Track progress in [Issue #XXX](https://github.com/xxx/issues/XXX)

**Workaround**: The `kinematics` feature provides basic gravity compensation (returns zeros pending RNE implementation).
```

### 3. 更新示例代码

```rust
// examples/gravity_compensation_mujoco.rs
#[cfg(not(feature = "mujoco"))]
{
    println!("⚠️ MuJoCo feature is currently unavailable due to API changes.");
    println!("   Please use the 'kinematics' feature instead.");
    println!("   See: docs/v0/piper-physics/MUJOCO_COMPILATION_ERRORS_ANALYSIS.md");
}
```

---

## 🔧 完整修复指南 (策略 A)

如需完整修复，按照以下阶段执行:

### Phase 1: 阻塞性问题 (1-2 小时)

```rust
// 1. 修复 Rc → Arc
use std::sync::Arc;
pub struct MujocoGravityCompensation {
    model: Arc<MjModel>,
    data: MjData<Arc<MjModel>>,
    // ...
}

// 2. 修复路径
const XML: &str = include_str!("../assets/piper_no_gripper.xml");
```

### Phase 2: API 适配 (8-12 小时)

需要调研 mujoco-rs 新版 API 并修复:
- `mujoco_rs::sys` 模块访问
- `site_xpos` / `site_xmat` 调用方式
- `qfrc_inverse` 访问方式
- 指针索引和字段访问

### Phase 3: 测试和文档 (2-3 小时)

- 添加缺失的测试宏
- 移除未使用的导入
- 更新文档

---

## 📚 相关文档

- [详细分析报告](./MUJOCO_COMPILATION_ERRORS_ANALYSIS.md) - 完整的技术分析
- [GRAVITY_COMPARISON_ANALYSIS_REVISED.md](../GRAVITY_COMPARISON_ANALYSIS_REVISED.md) - 物理实现对比
- [README.md](../README.md) - piper-physics 用户指南

---

## 🤝 贡献指南

如果你想帮助修复 mujoco feature:

1. 阅读 [详细分析报告](./MUJOCO_COMPILATION_ERRORS_ANALYSIS.md)
2. 在 GitHub 创建 issue 并注明你要修复的部分
3. 参考 Phase 1-3 的修复指南
4. 提交 PR 前确保所有测试通过

---

**最后更新**: 2025-01-29
**维护者**: Piper SDK Team
