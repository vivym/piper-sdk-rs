# TODO 项目分析报告

**生成时间**: 2026-01-23
**项目**: piper-sdk-rs
**范围**: 全代码库 TODO 审查

---

## 执行摘要

本报告对代码库中的所有 TODO 项进行了全面审查。共发现 **10 个 TODO 项**，分为以下类别：

- 🟢 **可立即实现** (3 项)
- 🟡 **需要额外调研** (2 项)
- 🔵 **Phase 3 后续任务** (3 项)
- 🟣 **可移除/过时** (2 项)

---

## 详细分析

### 🟢 优先级 1：可立即实现

#### 1.1 启用 `wait_for_disabled()` 调用

**位置**:
- `src/high_level/state/machine.rs:322`
- `src/high_level/state/machine.rs:373`

**当前代码**:
```rust
pub fn disable(self) -> Result<Piper<Standby>> {
    // 1. 失能机械臂
    self.raw_commander.disable_arm()?;

    // 2. TODO: 等待失能完成
    // self.wait_for_disabled()?;

    // 3. 类型转换
    ...
}
```

**问题分析**:
- `wait_for_disabled()` 方法已经实现（第 251-266 行）
- 被注释的原因可能是早期开发时不确定超时参数

**解决方案**:
```rust
pub fn disable(self, timeout: Duration) -> Result<Piper<Standby>> {
    self.raw_commander.disable_arm()?;
    self.wait_for_disabled(timeout)?;  // 启用等待

    let new_piper = Piper {
        raw_commander: self.raw_commander.clone(),
        observer: self.observer.clone(),
        _state: PhantomData,
    };

    std::mem::forget(self);
    Ok(new_piper)
}
```

**风险评估**: 低
**工作量**: 30 分钟
**建议**: ✅ 立即实现

---

#### 1.2 实现 `last_error()` 详细信息获取

**位置**: `src/high_level/state/machine.rs:418`

**当前代码**:
```rust
pub fn last_error(&self) -> Option<String> {
    if self.is_valid() {
        None
    } else {
        // TODO: 从 StateTracker 获取详细错误
        Some("State poisoned".to_string())
    }
}
```

**问题分析**:
- `StateTracker` 内部有 `poison_reason: Option<String>` 字段（state_tracker.rs:71）
- 但缺少公开的 API 来获取该信息

**解决方案**:

**步骤 1**: 在 `StateTracker` 添加公开方法
```rust
// src/high_level/client/state_tracker.rs
impl StateTracker {
    /// 获取 poison 原因（如果状态被标记为异常）
    pub fn poison_reason(&self) -> Option<String> {
        self.details.read().poison_reason.clone()
    }
}
```

**步骤 2**: 更新 `last_error()` 实现
```rust
// src/high_level/state/machine.rs
pub fn last_error(&self) -> Option<String> {
    self.raw_commander.state_tracker().poison_reason()
}
```

**风险评估**: 极低
**工作量**: 15 分钟
**建议**: ✅ 立即实现

---

#### 1.3 完善控制模式一致性检查

**位置**: `src/high_level/client/state_monitor.rs:135-140`

**当前代码**:
```rust
// 3. 检查控制模式一致性 TODO:
// 注意：这里需要从 Observer 读取实际的控制模式
// 如果硬件反馈中包含模式信息，可以在此检查
// let actual_mode = observer.control_mode();
// let expected_mode = state_tracker.expected_mode();
// if actual_mode != expected_mode { ... }
```

**问题分析**:
- 目前只检查了 `arm_enabled` 和 `emergency_stop`
- 缺少控制模式（MitMode/PositionMode）的一致性检查

**需要调研**:
1. 硬件反馈中是否包含当前控制模式？
2. `Observer` 是否已经暴露 `control_mode()` 方法？

**解决方案（条件性）**:
```rust
// 如果硬件反馈包含模式信息
if let Some(actual_mode) = observer.control_mode() {
    let expected_mode = state_tracker.expected_mode();
    if actual_mode != expected_mode {
        state_tracker.mark_poisoned(format!(
            "Control mode mismatch: expected {:?}, got {:?}",
            expected_mode, actual_mode
        ));
        break;
    }
}
```

**风险评估**: 中（需要硬件协议确认）
**工作量**: 1-2 小时（含调研）
**建议**: 🔍 先调研硬件反馈格式

---

### 🟡 优先级 2：需要额外调研

#### 2.1 固件版本数据完整性判断

**位置**: `src/robot/pipeline.rs:738-739`

**当前代码**:
```rust
// 尝试解析版本字符串
firmware_state.parse_version();

// TODO: 判断数据是否完整的逻辑（例如收到特定结束标记）
// firmware_state.is_complete = ...
```

**问题分析**:
- 固件版本通过 CAN 分段传输
- 不清楚协议中是否定义了"结束标记"或"总长度字段"

**需要调研**:
1. 查阅 Piper CAN 协议文档中的固件版本上报格式
2. 确认是否有以下机制：
   - 特殊的结束帧（如 `0x00`）
   - 总长度字段
   - CRC 校验

**可能的解决方案**:
```rust
// 方案 A: 检查结束符
if firmware_state.raw_data.ends_with(&[0x00]) {
    firmware_state.is_complete = true;
}

// 方案 B: 超时判断
if firmware_state.last_update.elapsed() > Duration::from_millis(100) {
    firmware_state.is_complete = true;
}

// 方案 C: 固定长度
if firmware_state.raw_data.len() >= EXPECTED_VERSION_LENGTH {
    firmware_state.is_complete = true;
}
```

**风险评估**: 中
**工作量**: 2-4 小时（含协议调研）
**建议**: 📚 查阅硬件文档或实验测试

---

#### 2.2 关节位置反馈单位确认

**位置**: `src/protocol/feedback.rs:753`

**当前代码**:
```rust
pub struct JointDriverHighSpeedFeedback {
    pub joint_index: u8,
    pub speed_rad_s: i16,   // 单位 0.001rad/s
    pub current_a: i16,     // 单位 0.001A
    pub position_rad: i32,  // 单位 rad (TODO: 需要确认真实单位)
}
```

**问题分析**:
- 速度和电流都有明确的缩放因子（0.001）
- 位置字段注释为 `rad`，但不确定是否有缩放因子

**需要调研**:
1. 查阅 Piper CAN 协议文档（高速反馈帧格式）
2. 如果文档不明确，进行实验：
   - 移动关节到已知位置（如 90°）
   - 读取 `position_rad` 原始值
   - 计算缩放因子

**可能的单位**:
- 选项 A: `rad`（无缩放，值为 `-3.14 ~ 3.14` 范围）
- 选项 B: `0.001 rad`（与速度/电流一致）
- 选项 C: `0.0001 rad` 或其他缩放因子
- 选项 D: `encoder ticks`（需要转换为 rad）

**风险评估**: 高（直接影响关节控制精度）
**工作量**: 1-3 小时
**建议**: ⚠️ **高优先级调研**，可能需要硬件实验

---

### 🔵 优先级 3：Phase 3 后续任务

#### 3.1 集成 StateMonitor 和 Heartbeat 到 Piper

**位置**: `src/high_level/state/machine.rs:112`

**当前代码**:
```rust
pub struct Piper<State = Disconnected> {
    pub(crate) raw_commander: Arc<RawCommander>,
    pub(crate) observer: Observer,
    // TODO: Phase 3 后续任务会添加 state_monitor 和 heartbeat
    _state: PhantomData<State>,
}
```

**问题分析**:
- `StateMonitor` 和 `HeartbeatManager` 已经实现
- 但未集成到 `Piper` 结构体中
- 这导致后台监控和心跳功能未激活

**解决方案**:
```rust
use super::state_monitor::StateMonitor;
use super::heartbeat::HeartbeatManager;

pub struct Piper<State = Disconnected> {
    pub(crate) raw_commander: Arc<RawCommander>,
    pub(crate) observer: Observer,
    pub(crate) state_monitor: Arc<StateMonitor>,
    pub(crate) heartbeat: Arc<HeartbeatManager>,
    _state: PhantomData<State>,
}

impl Piper<Disconnected> {
    pub fn connect(config: ConnectionConfig) -> Result<Piper<Standby>> {
        // ... 现有连接逻辑 ...

        // 启动后台服务
        let state_monitor = Arc::new(StateMonitor::new(
            state_tracker.clone(),
            observer.clone(),
        ));
        state_monitor.start()?;

        let heartbeat = Arc::new(HeartbeatManager::new(
            raw_commander.clone(),
        ));
        heartbeat.start()?;

        Ok(Piper {
            raw_commander,
            observer,
            state_monitor,
            heartbeat,
            _state: PhantomData,
        })
    }
}
```

**相关 TODO**: 第 3.2 项（Drop 中关闭服务）

**风险评估**: 中
**工作量**: 2-3 小时
**建议**: ⏳ Phase 3 时一起处理

---

#### 3.2 在 Drop 中关闭后台服务

**位置**: `src/high_level/state/machine.rs:397-399`

**当前代码**:
```rust
impl<State> Drop for Piper<State> {
    fn drop(&mut self) {
        let _ = self.raw_commander.disable_arm();

        // TODO: Phase 3 后续任务
        // - 关闭 Heartbeat
        // - 关闭 StateMonitor
    }
}
```

**依赖**: 必须先完成第 3.1 项

**解决方案**:
```rust
impl<State> Drop for Piper<State> {
    fn drop(&mut self) {
        // 1. 关闭后台服务（避免悬空引用）
        self.heartbeat.stop();
        self.state_monitor.stop();

        // 2. 失能机械臂
        let _ = self.raw_commander.disable_arm();
    }
}
```

**风险评估**: 中
**工作量**: 30 分钟（在 3.1 完成后）
**建议**: ⏳ 与 3.1 一起实现

---

#### 3.3 扩展集成测试

**位置**: `src/high_level/state/machine.rs:458`

**当前代码**:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    // TODO: 更多集成测试在 Phase 3 后续任务中添加
}
```

**建议测试用例**:
1. **状态转换测试**:
   - Disconnected → Standby → Active<MitMode> → Standby
   - 非法转换编译时错误验证

2. **StateMonitor 测试**:
   - 检测 arm_enabled 不一致
   - 检测紧急停止

3. **Heartbeat 测试**:
   - 心跳正常发送
   - 心跳停止后硬件超时检测

4. **错误处理测试**:
   - `wait_for_enabled()` 超时
   - StateTracker poison 后命令拒绝

**风险评估**: 低
**工作量**: 4-6 小时
**建议**: ⏳ Phase 3 完成后补充

---

### 🟣 优先级 4：可移除/已过时

#### 4.1 未使用的导入注释

**位置**: `src/high_level/client/state_monitor.rs:26`

**当前代码**:
```rust
use super::state_tracker::StateTracker;
use super::observer::Observer;
// use crate::high_level::types::Result;  // TODO: 未来实现时使用
```

**问题分析**:
- 这是一个"预留"导入，但实际未使用
- `Result` 类型应该按需导入，不需要提前注释

**解决方案**:
```rust
// 删除这一行注释
```

**风险评估**: 无
**工作量**: 1 分钟
**建议**: ✅ 立即移除

---

## 实施优先级矩阵

| 优先级 | TODO 项 | 风险 | 工作量 | 建议时间 |
|--------|---------|------|--------|----------|
| 🔴 P0 | 2.2 位置单位确认 | 高 | 1-3h | 本周内 |
| 🟠 P1 | 1.1 启用 wait_for_disabled | 低 | 30min | 本周内 |
| 🟠 P1 | 1.2 last_error 详细信息 | 极低 | 15min | 本周内 |
| 🟠 P1 | 4.1 移除无用注释 | 无 | 1min | 随时 |
| 🟡 P2 | 1.3 控制模式检查 | 中 | 1-2h | 下周 |
| 🟡 P2 | 2.1 固件版本完整性 | 中 | 2-4h | 下周 |
| 🔵 P3 | 3.1 集成后台服务 | 中 | 2-3h | Phase 3 |
| 🔵 P3 | 3.2 Drop 关闭服务 | 中 | 30min | Phase 3 |
| 🔵 P3 | 3.3 扩展测试 | 低 | 4-6h | Phase 3 |

---

## 行动建议

### 立即实施（本周）
1. ✅ **移除无用注释**（1 分钟）
2. ✅ **实现 `last_error()` 详细信息**（15 分钟）
3. ✅ **启用 `wait_for_disabled()` 调用**（30 分钟）
4. ⚠️ **调研关节位置单位**（1-3 小时，高优先级）

### 短期规划（1-2 周）
1. 🔍 完成控制模式一致性检查
2. 📚 调研固件版本完整性判断

### Phase 3 规划
1. 集成 StateMonitor 和 Heartbeat
2. 更新 Drop 实现
3. 补充集成测试

---

## 风险评估

### 🔴 高风险项
- **关节位置单位不明确**（feedback.rs:753）
  - **影响**: 可能导致关节控制精度错误，机械臂运动异常
  - **缓解**: 尽快通过文档或实验确认

### 🟡 中风险项
- **控制模式一致性未检查**（state_monitor.rs:135）
  - **影响**: 可能无法及时检测到类型状态与硬件状态的不一致
  - **缓解**: 当前已有 arm_enabled 检查，部分覆盖

### 🟢 低风险项
- 其他 TODO 项均为功能增强或代码清理，不影响现有功能

---

## 附录

### A. StateTracker 详细错误获取实现

```rust
// src/high_level/client/state_tracker.rs
impl StateTracker {
    /// 获取 poison 原因（如果状态被标记为异常）
    ///
    /// # 返回
    ///
    /// - `Some(reason)`: 状态已被标记为异常，返回原因
    /// - `None`: 状态正常
    pub fn poison_reason(&self) -> Option<String> {
        self.details.read().poison_reason.clone()
    }
}

// src/high_level/state/machine.rs
impl<State> Piper<State> {
    /// 获取最后的错误信息
    ///
    /// 如果状态跟踪器标记为 poisoned，返回详细错误原因。
    pub fn last_error(&self) -> Option<String> {
        self.raw_commander.state_tracker().poison_reason()
    }
}
```

### B. disable() 方法签名更新

```rust
// src/high_level/state/machine.rs

impl Piper<Active<MitMode>> {
    /// 失能机械臂（返回 Standby 状态）
    ///
    /// # 参数
    ///
    /// - `timeout`: 等待失能完成的超时时间
    pub fn disable(self, timeout: Duration) -> Result<Piper<Standby>> {
        self.raw_commander.disable_arm()?;
        self.wait_for_disabled(timeout)?;

        let new_piper = Piper {
            raw_commander: self.raw_commander.clone(),
            observer: self.observer.clone(),
            _state: PhantomData,
        };

        std::mem::forget(self);
        Ok(new_piper)
    }
}

impl Piper<Active<PositionMode>> {
    /// 失能机械臂（返回 Standby 状态）
    ///
    /// # 参数
    ///
    /// - `timeout`: 等待失能完成的超时时间
    pub fn disable(self, timeout: Duration) -> Result<Piper<Standby>> {
        self.raw_commander.disable_arm()?;
        self.wait_for_disabled(timeout)?;

        let new_piper = Piper {
            raw_commander: self.raw_commander.clone(),
            observer: self.observer.clone(),
            _state: PhantomData,
        };

        std::mem::forget(self);
        Ok(new_piper)
    }
}
```

---

**报告结束**

