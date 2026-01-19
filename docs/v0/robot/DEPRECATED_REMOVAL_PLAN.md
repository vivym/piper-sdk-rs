# 废弃代码全面移除执行方案

> **版本**：v0.0.1
> **创建日期**：2024年
> **目标**：全面移除所有 `#[deprecated]` 标记的代码，包括结构体、方法、字段和所有相关引用

---

## 📋 执行摘要

本方案旨在全面移除代码库中所有废弃的代码，包括：

- **4 个废弃状态结构体**：`CoreMotionState`、`ControlStatusState`、`DiagnosticState`、`ConfigState`
- **4 个废弃 API 方法**：`get_core_motion()`、`get_control_status()`、`get_diagnostic_state()`、`get_config_state()`
- **4 个废弃字段**：`PiperContext.core_motion`、`PiperContext.control_status`、`PiperContext.diagnostics`、`PiperContext.config`
- **多个废弃 FPS 统计字段**：`FpsStatistics`、`FpsResult`、`FpsCounts` 中的相关字段
- **所有相关测试代码**：使用废弃 API 的测试用例
- **所有 `#[allow(deprecated)]` 标记**：移除所有抑制废弃警告的代码

---

## 🔍 详细分析

### 1. 废弃状态结构体

#### 1.1 `CoreMotionState`

**位置**：`src/robot/state.rs:115`

**废弃原因**：已拆分为 `JointPositionState` 和 `EndPoseState`

**使用情况**：
- ✅ **已移除 Pipeline 更新逻辑**（阶段 3.4.2 完成）
- ❌ **仍在使用**：
  - `src/robot/robot_impl.rs:67` - `get_core_motion()` 方法
  - `src/robot/robot_impl.rs:247` - `get_motion_state()` 方法
  - `src/robot/robot_impl.rs:282` - `get_aligned_motion()` 方法
  - `src/robot/robot_impl.rs:327` - `wait_for_feedback()` 方法
  - `src/robot/robot_impl.rs:485` - 测试代码
  - `src/robot/robot_impl.rs:710` - 测试代码
  - `src/robot/state.rs:886` - `CombinedMotionState` 结构体
  - `src/robot/state.rs:922-1090` - 测试代码（多个测试函数）
  - `src/robot/state.rs:1040` - 测试代码
  - `src/robot/state.rs:1058` - 测试代码
  - `src/robot/state.rs:680` - 测试代码
  - `src/robot/state.rs:715` - 测试代码
  - `src/robot/builder.rs:118` - 文档注释
  - `src/can/socketcan/mod.rs:543` - 注释
  - `src/can/mod.rs:50` - 注释

**移除影响**：
- 需要重构 `get_motion_state()` 和 `get_aligned_motion()` 方法
- 需要重构 `CombinedMotionState` 结构体
- 需要移除或重构所有相关测试

#### 1.2 `ControlStatusState`

**位置**：`src/robot/state.rs:327`

**废弃原因**：已拆分为 `RobotControlState` 和 `GripperState`

**使用情况**：
- ✅ **已移除 Pipeline 更新逻辑**（阶段 3.4.2 完成）
- ❌ **仍在使用**：
  - `src/robot/robot_impl.rs:90` - `get_control_status()` 方法
  - `src/robot/robot_impl.rs:587` - 测试代码
  - `src/robot/state.rs:1047` - 测试代码
  - `src/robot/state.rs:1088` - 测试代码
  - `src/robot/state.rs:1107` - 测试代码

**移除影响**：
- 需要移除 `get_control_status()` 方法
- 需要移除或重构相关测试

#### 1.3 `DiagnosticState`

**位置**：`src/robot/state.rs:651`

**废弃原因**：已拆分为 `JointDriverLowSpeedState` 和 `CollisionProtectionState`

**使用情况**：
- ✅ **已移除 Pipeline 更新逻辑**（阶段 3.4.2 完成）
- ❌ **仍在使用**：
  - `src/robot/robot_impl.rs:253` - `get_diagnostic_state()` 方法
  - `src/robot/robot_impl.rs:598` - 测试代码
  - `src/robot/state.rs:1014` - 测试代码
  - `src/robot/state.rs:1115` - 测试代码
  - `src/robot/state.rs:726-727` - 测试代码
  - `src/robot/state.rs:730-731` - 测试代码

**移除影响**：
- 需要移除 `get_diagnostic_state()` 方法
- 需要移除或重构相关测试

#### 1.4 `ConfigState`

**位置**：`src/robot/state.rs:705`

**废弃原因**：已拆分为 `JointLimitConfigState`、`JointAccelConfigState` 和 `EndLimitConfigState`

**使用情况**：
- ✅ **已移除 Pipeline 更新逻辑**（阶段 3.4.2 完成）
- ❌ **仍在使用**：
  - `src/robot/robot_impl.rs:262` - `get_config_state()` 方法
  - `src/robot/robot_impl.rs:609` - 测试代码
  - `src/robot/state.rs:1026` - 测试代码
  - `src/robot/state.rs:1131` - 测试代码
  - `src/robot/state.rs:740-744` - 测试代码
  - `src/robot/state.rs:1053-1054` - 测试代码

**移除影响**：
- 需要移除 `get_config_state()` 方法
- 需要移除或重构相关测试

---

### 2. 废弃 API 方法

#### 2.1 `get_core_motion() -> CoreMotionState`

**位置**：`src/robot/robot_impl.rs:67`

**替代方案**：`get_joint_position()` 和 `get_end_pose()` 或 `capture_motion_snapshot()`

**使用情况**：
- `src/robot/robot_impl.rs:247` - `get_motion_state()` 方法
- `src/robot/robot_impl.rs:282` - `get_aligned_motion()` 方法
- `src/robot/robot_impl.rs:327` - `wait_for_feedback()` 方法
- `src/robot/robot_impl.rs:485` - 测试代码
- `src/robot/robot_impl.rs:710` - 测试代码

**移除步骤**：
1. 重构 `get_motion_state()` 使用新 API
2. 重构 `get_aligned_motion()` 使用新 API
3. 重构 `wait_for_feedback()` 使用新 API
4. 移除或重构测试代码

#### 2.2 `get_control_status() -> ControlStatusState`

**位置**：`src/robot/robot_impl.rs:90`

**替代方案**：`get_robot_control()` 和 `get_gripper()`

**使用情况**：
- `src/robot/robot_impl.rs:587` - 测试代码

**移除步骤**：
1. 直接移除方法
2. 移除或重构测试代码

#### 2.3 `get_diagnostic_state() -> Result<DiagnosticState, RobotError>`

**位置**：`src/robot/robot_impl.rs:253`

**替代方案**：`get_joint_driver_low_speed()` 和 `get_collision_protection()`

**使用情况**：
- `src/robot/robot_impl.rs:598` - 测试代码

**移除步骤**：
1. 直接移除方法
2. 移除或重构测试代码

#### 2.4 `get_config_state() -> Result<ConfigState, RobotError>`

**位置**：`src/robot/robot_impl.rs:262`

**替代方案**：`get_joint_limit_config()`、`get_joint_accel_config()` 和 `get_end_limit_config()`

**使用情况**：
- `src/robot/robot_impl.rs:609` - 测试代码

**移除步骤**：
1. 直接移除方法
2. 移除或重构测试代码

---

### 3. 废弃字段

#### 3.1 `PiperContext` 中的废弃字段

**位置**：`src/robot/state.rs:763-795`

**废弃字段**：
- `core_motion: Arc<ArcSwap<CoreMotionState>>` (line 795)
- `control_status: Arc<ArcSwap<ControlStatusState>>` (line 763)
- `diagnostics: Arc<RwLock<DiagnosticState>>` (line 772)
- `config: Arc<RwLock<ConfigState>>` (line 785)

**使用情况**：
- `src/robot/state.rs:830` - 初始化（`#[allow(deprecated)]`）
- `src/robot/state.rs:837` - 初始化（`#[allow(deprecated)]`）
- `src/robot/state.rs:839` - 初始化（`#[allow(deprecated)]`）
- `src/robot/state.rs:846` - 初始化（`#[allow(deprecated)]`）
- `src/robot/robot_impl.rs:68` - `get_core_motion()` 方法
- `src/robot/robot_impl.rs:91` - `get_control_status()` 方法
- `src/robot/robot_impl.rs:255` - `get_diagnostic_state()` 方法
- `src/robot/robot_impl.rs:264` - `get_config_state()` 方法
- `src/robot/state.rs:1040` - 测试代码
- `src/robot/state.rs:1047` - 测试代码
- `src/robot/state.rs:1050-1054` - 测试代码

**移除步骤**：
1. 移除字段定义
2. 移除初始化代码
3. 移除所有访问这些字段的代码

#### 3.2 `FpsStatistics` 中的废弃字段

**位置**：`src/robot/fps_stats.rs:24-42`

**废弃字段**：
- `control_status_updates: AtomicU64` (line 25)
- `diagnostics_updates: AtomicU64` (line 36)
- `config_updates: AtomicU64` (line 37)
- `core_motion_updates: AtomicU64` (line 41)

**使用情况**：
- `src/robot/fps_stats.rs:63-69` - 初始化（`#[allow(deprecated)]`）
- `src/robot/fps_stats.rs:90-96` - `reset()` 方法（`#[allow(deprecated)]`）
- `src/robot/fps_stats.rs:125-131` - `calculate_fps()` 方法（`#[allow(deprecated)]`）
- `src/robot/fps_stats.rs:155-161` - `get_counts()` 方法（`#[allow(deprecated)]`）

**移除步骤**：
1. 移除字段定义
2. 移除初始化代码
3. 移除 `reset()` 中的重置代码
4. 移除 `calculate_fps()` 中的计算代码
5. 移除 `get_counts()` 中的读取代码

#### 3.3 `FpsResult` 中的废弃字段

**位置**：`src/robot/fps_stats.rs:209-220`

**废弃字段**：
- `control_status: f64` (line 210)
- `diagnostics: f64` (line 213)
- `config: f64` (line 215)
- `core_motion: f64` (line 220)

**使用情况**：
- `src/robot/fps_stats.rs:125-131` - `calculate_fps()` 方法（`#[allow(deprecated)]`）

**移除步骤**：
1. 移除字段定义
2. 移除 `calculate_fps()` 中的赋值代码

#### 3.4 `FpsCounts` 中的废弃字段

**位置**：`src/robot/fps_stats.rs:249-260`

**废弃字段**：
- `control_status: u64` (line 250)
- `diagnostics: u64` (line 253)
- `config: u64` (line 255)
- `core_motion: u64` (line 260)

**使用情况**：
- `src/robot/fps_stats.rs:155-161` - `get_counts()` 方法（`#[allow(deprecated)]`）
- `src/robot/fps_stats.rs:274-278` - 测试代码
- `src/robot/fps_stats.rs:333-341` - 测试代码

**移除步骤**：
1. 移除字段定义
2. 移除 `get_counts()` 中的赋值代码
3. 移除或重构测试代码

---

### 4. 废弃结构体引用

#### 4.1 `CombinedMotionState`

**位置**：`src/robot/state.rs:886`

**问题**：包含 `core: CoreMotionState` 字段

**使用情况**：
- `src/robot/robot_impl.rs:94-99` - `get_motion_state()` 方法

**移除/重构方案**：
- 方案1：移除 `CombinedMotionState`，`get_motion_state()` 返回元组或新结构体
- 方案2：重构 `CombinedMotionState` 使用新状态结构

---

### 5. `#[allow(deprecated)]` 标记

**位置**：
- `src/robot/state.rs:829, 837, 839, 846` - `PiperContext::new()`
- `src/robot/state.rs:995-1109` - 测试代码（多个位置）
- `src/robot/fps_stats.rs:62-68` - `FpsStatistics::new()`
- `src/robot/fps_stats.rs:90-96` - `FpsStatistics::reset()`
- `src/robot/fps_stats.rs:125-131` - `FpsStatistics::calculate_fps()`
- `src/robot/fps_stats.rs:155-161` - `FpsStatistics::get_counts()`

**移除步骤**：
1. 移除所有 `#[allow(deprecated)]` 标记
2. 移除或重构相关代码

---

## 📝 详细执行计划

### 阶段 1：重构依赖废弃 API 的方法

#### 1.1 重构 `get_motion_state()`

**当前代码**：
```rust
pub fn get_motion_state(&self) -> CombinedMotionState {
    CombinedMotionState {
        core: self.get_core_motion(),
        joint_dynamic: self.get_joint_dynamic(),
    }
}
```

**重构方案**：
```rust
pub fn get_motion_state(&self) -> CombinedMotionState {
    let snapshot = self.capture_motion_snapshot();
    CombinedMotionState {
        joint_position: snapshot.joint_position,
        end_pose: snapshot.end_pose,
        joint_dynamic: self.get_joint_dynamic(),
    }
}
```

**需要修改**：
- 重构 `CombinedMotionState` 结构体

#### 1.2 重构 `get_aligned_motion()`

**当前代码**：
```rust
pub fn get_aligned_motion(&self, max_time_diff_us: u64) -> AlignmentResult {
    let core = self.get_core_motion();
    let joint_dynamic = self.get_joint_dynamic();
    // ...
}
```

**重构方案**：
```rust
pub fn get_aligned_motion(&self, max_time_diff_us: u64) -> AlignmentResult {
    let snapshot = self.capture_motion_snapshot();
    let joint_dynamic = self.get_joint_dynamic();

    let time_diff = snapshot.joint_position.hardware_timestamp_us
        .abs_diff(joint_dynamic.group_timestamp_us);

    let state = AlignedMotionState {
        joint_pos: snapshot.joint_position.joint_pos,
        joint_vel: joint_dynamic.joint_vel,
        joint_current: joint_dynamic.joint_current,
        end_pose: snapshot.end_pose.end_pose,
        timestamp: snapshot.joint_position.hardware_timestamp_us,
        time_diff_us: (joint_dynamic.group_timestamp_us as i64)
            - (snapshot.joint_position.hardware_timestamp_us as i64),
    };
    // ...
}
```

#### 1.3 重构 `wait_for_feedback()`

**当前代码**：
```rust
let core_motion = self.get_core_motion();
if core_motion.timestamp_us > 0 {
    return Ok(());
}
```

**重构方案**：
```rust
let joint_pos = self.get_joint_position();
if joint_pos.hardware_timestamp_us > 0 {
    return Ok(());
}
```

---

### 阶段 2：移除废弃 API 方法

#### 2.1 移除 `get_core_motion()`

**步骤**：
1. 删除方法定义（`src/robot/robot_impl.rs:67-69`）
2. 移除所有调用（已在阶段 1 重构）

#### 2.2 移除 `get_control_status()`

**步骤**：
1. 删除方法定义（`src/robot/robot_impl.rs:90-92`）
2. 移除测试代码（`src/robot/robot_impl.rs:587-596`）

#### 2.3 移除 `get_diagnostic_state()`

**步骤**：
1. 删除方法定义（`src/robot/robot_impl.rs:253-258`）
2. 移除测试代码（`src/robot/robot_impl.rs:598-607`）

#### 2.4 移除 `get_config_state()`

**步骤**：
1. 删除方法定义（`src/robot/robot_impl.rs:262-267`）
2. 移除测试代码（`src/robot/robot_impl.rs:609-617`）

---

### 阶段 3：重构 `CombinedMotionState`

**当前结构**：
```rust
pub struct CombinedMotionState {
    pub core: CoreMotionState,
    pub joint_dynamic: JointDynamicState,
}
```

**重构方案**：
```rust
pub struct CombinedMotionState {
    pub joint_position: JointPositionState,
    pub end_pose: EndPoseState,
    pub joint_dynamic: JointDynamicState,
}
```

---

### 阶段 4：移除废弃字段

#### 4.1 移除 `PiperContext` 中的废弃字段

**步骤**：
1. 删除字段定义（`src/robot/state.rs:763, 772, 785, 795`）
2. 删除初始化代码（`src/robot/state.rs:830, 837, 839, 846`）
3. 移除所有 `#[allow(deprecated)]` 标记

#### 4.2 移除 `FpsStatistics` 中的废弃字段

**步骤**：
1. 删除字段定义（`src/robot/fps_stats.rs:25, 36, 37, 41`）
2. 删除初始化代码（`src/robot/fps_stats.rs:63, 65, 67, 69`）
3. 删除 `reset()` 中的重置代码（`src/robot/fps_stats.rs:90, 92, 94, 96`）
4. 删除 `calculate_fps()` 中的计算代码（`src/robot/fps_stats.rs:125, 127, 129, 131`）
5. 删除 `get_counts()` 中的读取代码（`src/robot/fps_stats.rs:155, 157, 159, 161`）
6. 移除所有 `#[allow(deprecated)]` 标记

#### 4.3 移除 `FpsResult` 中的废弃字段

**步骤**：
1. 删除字段定义（`src/robot/fps_stats.rs:210, 213, 215, 220`）
2. 删除 `calculate_fps()` 中的赋值代码（`src/robot/fps_stats.rs:125, 127, 129, 131`）

#### 4.4 移除 `FpsCounts` 中的废弃字段

**步骤**：
1. 删除字段定义（`src/robot/fps_stats.rs:250, 253, 255, 260`）
2. 删除 `get_counts()` 中的赋值代码（`src/robot/fps_stats.rs:155, 157, 159, 161`）
3. 移除或重构测试代码（`src/robot/fps_stats.rs:274-278, 333-341`）

---

### 阶段 5：移除废弃状态结构体

#### 5.1 移除 `CoreMotionState`

**步骤**：
1. 删除结构体定义（`src/robot/state.rs:115-133`）
2. 移除所有测试代码（`src/robot/state.rs:922-1090` 中的相关测试）
3. 更新注释和文档

#### 5.2 移除 `ControlStatusState`

**步骤**：
1. 删除结构体定义（`src/robot/state.rs:327-360`）
2. 移除所有测试代码（`src/robot/state.rs:994-1115` 中的相关测试）

#### 5.3 移除 `DiagnosticState`

**步骤**：
1. 删除结构体定义（`src/robot/state.rs:651-704`）
2. 移除所有测试代码（`src/robot/state.rs:1014-1131` 中的相关测试）

#### 5.4 移除 `ConfigState`

**步骤**：
1. 删除结构体定义（`src/robot/state.rs:705-761`）
2. 移除所有测试代码（`src/robot/state.rs:1026-1131` 中的相关测试）

---

### 阶段 6：清理注释和文档

#### 6.1 更新代码注释

**需要更新的文件**：
- `src/robot/builder.rs:118` - 更新文档注释
- `src/can/socketcan/mod.rs:543` - 更新注释
- `src/can/mod.rs:50` - 更新注释

#### 6.2 更新文档

**需要更新的文件**：
- `docs/v0/robot/API_REFERENCE.md` - 移除废弃 API 章节
- `docs/v0/robot/MIGRATION_GUIDE.md` - 标记为已完成迁移
- `README.md` - 更新示例代码

---

## ⚠️ 风险评估

### 高风险项

1. **`get_motion_state()` 和 `get_aligned_motion()` 的重构**
   - **风险**：可能影响现有用户代码
   - **缓解**：确保新 API 行为一致，提供迁移指南

2. **测试代码的移除**
   - **风险**：可能丢失测试覆盖
   - **缓解**：确保新状态结构有完整的测试覆盖

### 中风险项

1. **`CombinedMotionState` 的重构**
   - **风险**：可能影响序列化/反序列化
   - **缓解**：检查是否有外部依赖

2. **FPS 统计字段的移除**
   - **风险**：可能影响监控工具
   - **缓解**：确保新字段已就绪

### 低风险项

1. **注释和文档的更新**
   - **风险**：低
   - **缓解**：仔细检查所有引用

---

## 📊 执行检查清单

### 阶段 1：重构依赖废弃 API 的方法
- [ ] 重构 `get_motion_state()` 使用新 API
- [ ] 重构 `get_aligned_motion()` 使用新 API
- [ ] 重构 `wait_for_feedback()` 使用新 API
- [ ] 运行测试，确保功能正常

### 阶段 2：移除废弃 API 方法
- [ ] 移除 `get_core_motion()` 方法
- [ ] 移除 `get_control_status()` 方法
- [ ] 移除 `get_diagnostic_state()` 方法
- [ ] 移除 `get_config_state()` 方法
- [ ] 移除相关测试代码
- [ ] 运行测试，确保无编译错误

### 阶段 3：重构 `CombinedMotionState`
- [ ] 重构 `CombinedMotionState` 结构体
- [ ] 更新 `get_motion_state()` 方法
- [ ] 运行测试，确保功能正常

### 阶段 4：移除废弃字段
- [ ] 移除 `PiperContext` 中的废弃字段
- [ ] 移除 `FpsStatistics` 中的废弃字段
- [ ] 移除 `FpsResult` 中的废弃字段
- [ ] 移除 `FpsCounts` 中的废弃字段
- [ ] 移除所有 `#[allow(deprecated)]` 标记
- [ ] 运行测试，确保无编译错误

### 阶段 5：移除废弃状态结构体
- [ ] 移除 `CoreMotionState` 结构体
- [ ] 移除 `ControlStatusState` 结构体
- [ ] 移除 `DiagnosticState` 结构体
- [ ] 移除 `ConfigState` 结构体
- [ ] 移除所有相关测试代码
- [ ] 运行测试，确保无编译错误

### 阶段 6：清理注释和文档
- [ ] 更新代码注释
- [ ] 更新 API 文档
- [ ] 更新迁移指南
- [ ] 更新 README.md
- [ ] 运行文档测试

### 最终验证
- [ ] 运行所有测试（`cargo test`）
- [ ] 检查编译警告（确保无废弃警告）
- [ ] 检查代码覆盖率
- [ ] 代码审查

---

## 📈 预期结果

### 代码统计

**移除前**：
- 废弃结构体：4 个
- 废弃方法：4 个
- 废弃字段：12+ 个
- `#[allow(deprecated)]` 标记：15+ 个
- 废弃警告：30+ 个

**移除后**：
- 废弃结构体：0 个
- 废弃方法：0 个
- 废弃字段：0 个
- `#[allow(deprecated)]` 标记：0 个
- 废弃警告：0 个

### 代码质量提升

1. **可维护性**：移除废弃代码，减少技术债务
2. **清晰度**：代码更清晰，无废弃警告干扰
3. **性能**：移除不必要的字段初始化，减少内存占用
4. **一致性**：所有代码使用新 API，保持一致性

---

## 🎯 执行时间估算

- **阶段 1**：2-3 小时（重构方法）
- **阶段 2**：1 小时（移除方法）
- **阶段 3**：1 小时（重构结构体）
- **阶段 4**：2-3 小时（移除字段）
- **阶段 5**：2-3 小时（移除结构体）
- **阶段 6**：1-2 小时（清理文档）

**总计**：9-13 小时

---

## 📝 注意事项

1. **向后兼容性**：移除废弃代码会破坏向后兼容性，建议在下一个主版本（v0.1.0）中执行
2. **测试覆盖**：确保新 API 有完整的测试覆盖
3. **文档更新**：及时更新所有相关文档
4. **代码审查**：每个阶段完成后进行代码审查
5. **渐进式移除**：可以分阶段执行，每个阶段完成后运行测试

---

**最后更新**：2024年
**执行状态**：✅ **已完成**

## 📊 执行总结

所有废弃代码已成功移除：

- ✅ **阶段1**：重构依赖废弃API的方法（`get_motion_state`, `get_aligned_motion`, `wait_for_feedback`）
- ✅ **阶段2**：移除废弃API方法（`get_core_motion`, `get_control_status`, `get_diagnostic_state`, `get_config_state`）
- ✅ **阶段3**：重构`CombinedMotionState`结构体
- ✅ **阶段4**：移除废弃字段（`PiperContext`, `FpsStatistics`, `FpsResult`, `FpsCounts`）
- ✅ **阶段5**：移除废弃状态结构体（`CoreMotionState`, `ControlStatusState`, `DiagnosticState`, `ConfigState`）
- ✅ **阶段6**：清理注释和文档

**移除结果**：
- 废弃结构体：0 个（已全部移除）
- 废弃方法：0 个（已全部移除）
- 废弃字段：0 个（已全部移除）
- `#[allow(deprecated)]` 标记：0 个（已全部移除）
- 废弃警告：0 个（编译通过，无废弃警告）

**参考文档**：
- [状态结构重构分析报告](state_structure_refactoring_analysis.md)
- [API 参考文档](API_REFERENCE.md)
- [迁移指南](MIGRATION_GUIDE.md)

