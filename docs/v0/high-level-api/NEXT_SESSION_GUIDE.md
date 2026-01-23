# 下一会话实施指南

**创建日期**: 2026-01-23
**当前进度**: Phase 2（40% 完成）
**上次停止点**: StateTracker 实现完成

---

## ✅ 已完成工作回顾

### Phase 0: 项目准备 ✅
- 项目结构、Mock 硬件、测试框架
- **28 个测试通过**

### Phase 1: 基础类型系统 ✅
- 强类型单位、Joint 数组、错误体系、笛卡尔类型
- **90 个测试通过**

### Phase 2: 读写分离（部分）⏳
- ✅ StateTracker（无锁状态跟踪）
- **10 个测试通过，性能: 97M ops/s**

**总计**: 128 个测试，全部通过 ✅

---

## 🎯 下一步任务：Phase 2 剩余工作

### 任务优先级

#### 🔴 高优先级：核心功能

**任务 2.2: RawCommander 实现**
- **文件**: `src/high_level/client/raw_commander.rs`
- **功能**: 内部命令发送器（pub(crate)）
- **依赖**: StateTracker（已完成）
- **时间**: 1-2 天
- **文档**: `IMPLEMENTATION_TODO_LIST.md` 第 930-1050 行

**关键点**:
```rust
pub(crate) struct RawCommander {
    state_tracker: Arc<StateTracker>,
    can_interface: /* 待定 */,
}

impl RawCommander {
    pub(crate) fn send_mit_command(...) -> Result<()> {
        // 1. 快速状态检查（原子操作）
        self.state_tracker.check_valid_fast()?;

        // 2. 发送 CAN 帧
        // ...
    }
}
```

---

**任务 2.3: MotionCommander 实现**
- **文件**: `src/high_level/client/motion_commander.rs`
- **功能**: 公开的运动命令接口
- **依赖**: RawCommander
- **时间**: 1 天
- **文档**: `IMPLEMENTATION_TODO_LIST.md` 第 1050-1150 行

**关键点**:
```rust
pub struct MotionCommander {
    raw: Arc<RawCommander>,
}

impl MotionCommander {
    pub fn send_mit_command(...) -> Result<()> {
        self.raw.send_mit_command(...)
    }

    pub fn send_position_command(...) -> Result<()> {
        self.raw.send_position_command(...)
    }

    pub fn set_gripper(...) -> Result<()> {
        self.raw.send_gripper_command(...)
    }

    // ❌ 无状态修改方法（安全设计）
}
```

---

**任务 2.4: Observer 实现**
- **文件**: `src/high_level/client/observer.rs`
- **功能**: 状态观察器（只读）
- **依赖**: 独立（读写分离）
- **时间**: 1 天
- **文档**: `IMPLEMENTATION_TODO_LIST.md` 第 1150-1250 行

**关键点**:
```rust
pub struct Observer {
    state_cache: Arc<RwLock<RobotState>>,
    update_thread: /* 后台线程 */,
}

impl Observer {
    pub fn joint_positions(&self) -> JointArray<Rad> { ... }
    pub fn joint_velocities(&self) -> JointArray<Rad> { ... }
    pub fn joint_torques(&self) -> JointArray<NewtonMeter> { ... }
    pub fn gripper_state(&self) -> GripperState { ... }
}
```

---

#### 🟡 中优先级：验证

**任务 2.5: Phase 2 性能测试**
- **文件**: `benches/phase2_performance.rs`
- **功能**: 完整的性能基准测试
- **依赖**: 所有 Phase 2 组件
- **时间**: 0.5 天
- **工具**: criterion

**测试项目**:
- 热路径延迟（StateTracker）
- 命令发送吞吐量
- 并发读写性能
- 内存分配开销

---

## 📋 实施步骤建议

### Step 1: 准备工作（30 分钟）
1. 阅读本文档
2. 查看 `PROJECT_STATUS.md`
3. 阅读 `IMPLEMENTATION_TODO_LIST.md` Phase 2 部分
4. 运行测试验证当前状态：
   ```bash
   cd /home/viv/projs/piper-sdk-rs
   cargo test --lib high_level
   ```

### Step 2: 实施 RawCommander（1-2 天）
1. 创建 `src/high_level/client/raw_commander.rs`
2. 实现基本结构和 StateTracker 集成
3. 实现 CAN 帧发送（可能需要 Mock）
4. 编写单元测试（目标：15+ 测试）
5. 验证性能（快速路径 < 100ns）

### Step 3: 实施 MotionCommander（1 天）
1. 创建 `src/high_level/client/motion_commander.rs`
2. 实现公开 API
3. 验证权限控制（无状态修改方法）
4. 编写单元测试（目标：10+ 测试）
5. 集成测试

### Step 4: 实施 Observer（1 天）
1. 创建 `src/high_level/client/observer.rs`
2. 实现状态缓存
3. 实现后台更新线程（可选）
4. 编写单元测试（目标：10+ 测试）
5. 并发测试

### Step 5: 性能测试（0.5 天）
1. 创建 `benches/phase2_performance.rs`
2. 使用 criterion 编写基准测试
3. 运行并记录结果
4. 创建 Phase 2 完成报告

---

## 🔧 技术注意事项

### RawCommander 实现挑战
1. **CAN 接口集成**:
   - 可能需要抽象 CAN 接口
   - 建议先用 Mock 实现

2. **状态检查开销**:
   - 已有 StateTracker（97M ops/s）
   - 额外开销应 < 50ns

3. **错误处理**:
   - 使用已有的 RobotError 类型
   - 区分 Fatal vs Recoverable

### MotionCommander 实现挑战
1. **权限控制**:
   - 确保无法调用状态修改方法
   - 只能通过 RawCommander 内部调用

2. **API 设计**:
   - 简洁易用
   - 类型安全（使用 JointArray<Rad> 等）

### Observer 实现挑战
1. **读写分离**:
   - 与 Commander 完全独立
   - 可并发访问

2. **状态更新**:
   - 可以是被动的（手动调用）
   - 或主动的（后台线程）
   - 建议先实现被动版本

### 性能测试挑战
1. **基准测试**:
   - 使用 criterion 框架
   - 避免优化器消除测试代码

2. **并发测试**:
   - 多线程压力测试
   - 验证无竞争条件

---

## 📊 预期成果

### 完成 Phase 2 后
- **代码**: +1,500 行
- **测试**: +45 个（总计 ~173）
- **文档**: Phase 2 完成报告
- **性能**: 全部基准测试达标

### Phase 2 完成标准
- ✅ RawCommander 实现并测试
- ✅ MotionCommander 实现并测试
- ✅ Observer 实现并测试
- ✅ 所有单元测试通过
- ✅ 性能基准达标
- ✅ 并发安全验证
- ✅ Phase 2 完成报告

---

## 📝 文档更新清单

完成每个任务后，更新以下文档：

1. **`IMPLEMENTATION_PROGRESS.md`**
   - 标记任务完成
   - 更新进度百分比
   - 记录遇到的问题

2. **`PROJECT_STATUS.md`**
   - 更新 Phase 2 状态
   - 更新总体进度

3. **创建 `PHASE2_COMPLETION_REPORT.md`**
   - 类似 Phase 1 报告格式
   - 包含性能数据
   - 技术亮点总结

---

## 🎯 快速启动命令

### 验证当前状态
```bash
cd /home/viv/projs/piper-sdk-rs

# 运行所有测试
cargo test --lib

# 运行高层 API 测试
cargo test --lib high_level

# 检查代码质量
cargo clippy --all-targets

# 查看文档
cat docs/v0/high-level-api/PROJECT_STATUS.md
```

### 创建新文件
```bash
# RawCommander
touch src/high_level/client/raw_commander.rs

# MotionCommander
touch src/high_level/client/motion_commander.rs

# Observer
touch src/high_level/client/observer.rs

# 性能测试
mkdir -p benches
touch benches/phase2_performance.rs
```

### 更新模块导出
编辑 `src/high_level/client/mod.rs`:
```rust
pub mod state_tracker;
pub mod raw_commander;
pub mod motion_commander;
pub mod observer;
```

---

## 💡 实施建议

### 如果遇到困难
1. 先实现最小可用版本（MVP）
2. 使用 Mock 替代复杂依赖
3. 编写测试验证核心功能
4. 逐步完善

### 如果时间紧张
**最小完成**:
- RawCommander（基本功能）
- MotionCommander（薄包装）
- Observer（简单版本）
- 基础测试

**完整完成**:
- 上述 + 性能优化
- 上述 + 完整测试
- 上述 + 性能基准
- 上述 + 完成报告

---

## 🔗 关键文件引用

### 设计文档
- `rust_high_level_api_design_v3.2_final.md` - 完整设计
- 第 3 节：热路径性能优化
- 第 4.2 节：读写分离

### 实施清单
- `IMPLEMENTATION_TODO_LIST.md`
- 第 769-1300 行：Phase 2 详细任务

### 当前状态
- `PROJECT_STATUS.md` - 项目状态
- `SESSION_SUMMARY.md` - 上次会话总结

---

## ✨ 成功标准

Phase 2 完成后，应该能够：

1. **编译通过**: 无错误，无 Clippy 警告
2. **测试通过**: 所有测试 100% 通过
3. **性能达标**:
   - StateTracker: > 50M ops/s ✅ (97M ops/s)
   - RawCommander: < 100ns 延迟
   - 并发无竞争
4. **文档完整**: 所有公开 API 有文档
5. **代码质量**: 清晰、可维护、类型安全

---

## 📞 需要帮助？

遇到问题时，可以：
1. 查看设计文档中的相关章节
2. 参考已完成的代码（types/, state_tracker.rs）
3. 查看测试用例了解预期行为
4. 检查 RobotError 类型了解错误处理

---

**下次会话**: 从任务 2.2（RawCommander）开始
**预计工期**: 3-4 天完成 Phase 2
**准备状态**: ✅ 文档齐全，基础完善

**祝实施顺利！** 🚀

