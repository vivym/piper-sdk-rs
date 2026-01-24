# Piper 移除执行计划

**文档版本**：v1.1
**创建日期**：2026-01-24
**最后更新**：2026-01-24
**状态**：已完成（所有阶段 0-5 完成）
**关联文档**：[motion_commander_design_flaw_analysis.md](./motion_commander_design_flaw_analysis.md)

---

## 1. 执行摘要

### 1.1 目标

完全移除 `Piper`，将所有运动命令方法直接放在 `Piper<Active<M>>` 上，确保 Type State Pattern 的安全性不被绕过。

### 1.2 预期收益

| 收益 | 说明 |
|------|------|
| ✅ Type State 安全性保证 | 编译期确保只有正确状态才能发送命令 |
| ✅ 消除性能开销 | 无 Arc clone，无临时对象创建 |
| ✅ 简化架构 | 移除不必要的中间层 |
| ✅ 防止误用 | 用户无法"提取"并保存命令接口 |
| ✅ 防止硬件损坏 | 杜绝在错误状态下发送命令导致的安全问题 |

### 1.3 影响范围

| 类型 | 文件数 | 说明 |
|------|--------|------|
| 核心代码 | 3 | machine.rs, motion.rs, mod.rs |
| 示例代码 | 4 | position_control_demo.rs, high_level_*.rs, **multi_threaded_demo.rs (新增)** |
| 文档 | ~25 | README, docs/v0/*.md |

### 1.4 前置检查结果

| 检查项 | 结果 | 说明 |
|--------|------|------|
| `motion.rs` 是否有非 Piper 的依赖？ | ✅ 无 | 文件只包含 Piper，可安全删除 |
| `RawCommander` 可见性是否正确？ | ✅ 已正确 | `pub(crate) struct` + `pub(crate) fn new()` |
| 是否存在 TeachMode 需要处理？ | ✅ 不存在 | 当前代码库没有 TeachMode |

---

## 2. 变更清单

### 2.1 核心代码变更

#### 文件 1：`src/client/state/machine.rs`

**变更类型**：修改

**当前状态**：
```rust
impl Piper<Active<MitMode>> {
    pub fn command_torques(&self, ...) -> Result<()> {
        let motion = self.Piper;  // ❌ 创建 Piper
        motion.send_mit_command(...)
    }

    pub fn Piper -> Piper {
        Piper::new(self.driver.clone())  // ❌ Arc clone
    }
}
```

**目标状态**：
```rust
impl Piper<Active<MitMode>> {
    pub fn command_torques(&self, ...) -> Result<()> {
        let raw = RawCommander::new(&self.driver);  // ✅ 直接使用引用
        raw.send_mit_command_batch(...)
    }

    // ✅ Piper 方法被移除
}
```

**具体修改**：

| impl 块 | 方法 | 操作 | 说明 |
|---------|------|------|------|
| `Piper<Active<MitMode>>` | `command_torques()` | 修改 | 直接使用 RawCommander |
| `Piper<Active<MitMode>>` | `Piper` | 删除 | |
| `Piper<Active<MitMode>>` | `set_gripper()` | **新增** | 从 Piper 迁移 |
| `Piper<Active<MitMode>>` | `open_gripper()` | **新增** | 从 Piper 迁移 |
| `Piper<Active<MitMode>>` | `close_gripper()` | **新增** | 从 Piper 迁移 |
| `Piper<Active<PositionMode>>` | `command_cartesian_pose()` | 修改 | 直接使用 RawCommander |
| `Piper<Active<PositionMode>>` | `move_linear()` | 修改 | 直接使用 RawCommander |
| `Piper<Active<PositionMode>>` | `move_circular()` | 修改 | 直接使用 RawCommander |
| `Piper<Active<PositionMode>>` | `command_position()` | 修改 | 直接使用 RawCommander |
| `Piper<Active<PositionMode>>` | `Piper` | 删除 | |
| `Piper<Active<PositionMode>>` | `send_position_command()` | **新增** | 批量发送位置命令 |
| `Piper<Active<PositionMode>>` | `set_gripper()` | **新增** | 从 Piper 迁移 |
| `Piper<Active<PositionMode>>` | `open_gripper()` | **新增** | 从 Piper 迁移 |
| `Piper<Active<PositionMode>>` | `close_gripper()` | **新增** | 从 Piper 迁移 |

---

#### 文件 2：`src/client/motion.rs`

**变更类型**：删除（或标记为 deprecated）

**选项 A：直接删除**（推荐）
- 删除整个文件
- 简单直接，无歧义

**选项 B：标记为 deprecated**
- 保留文件，所有方法标记 `#[deprecated]`
- 给用户迁移时间
- 需要维护两套代码

**推荐**：选项 A（直接删除），因为：
1. 这是内部 API，不是公开的稳定 API
2. 保留 deprecated 方法会让 Type State 安全性问题持续存在
3. 清晰的破坏性变更比缓慢的弃用更好

---

#### 文件 3：`src/client/mod.rs`

**变更类型**：修改

**当前状态**：
```rust
pub mod motion;
pub use motion::Piper;
```

**目标状态**：
```rust
// pub mod motion;  // 删除
// pub use motion::Piper;  // 删除
```

---

### 2.2 示例代码变更

#### 文件 4：`examples/position_control_demo.rs`

**变更类型**：修改

**当前状态**：
```rust
let motion = robot.Piper;
motion.send_position_command(&target_positions)?;
// ...
motion.send_position_command(&target_positions)?;  // 保持位置
// ...
motion.send_position_command(&current_positions)?;  // 回到原位
```

**目标状态**：
```rust
robot.send_position_command(&target_positions)?;
// ...
robot.send_position_command(&target_positions)?;  // 保持位置
// ...
robot.send_position_command(&current_positions)?;  // 回到原位
```

---

#### 文件 5：`examples/high_level_gripper_control.rs`

**变更类型**：修改

**当前状态**：
```rust
println!("   // let commander = piper.Piper;");
println!("   commander.open_gripper()?;");
```

**目标状态**：
```rust
println!("   // 直接在 piper 上调用");
println!("   piper.open_gripper()?;");
```

---

#### 文件 6：`examples/high_level_simple_move.rs`

**变更类型**：修改

**当前状态**：
```rust
// piper.Piper.command_positions(position)?;
```

**目标状态**：
```rust
// piper.send_position_command(&position)?;
```

---

#### 文件 7（新增）：`examples/multi_threaded_demo.rs`

**变更类型**：**新增**

**说明**：演示如何在多线程环境下安全地共享 `Piper` 并发送指令，帮助用户从旧的"fire and forget"模式迁移。

**示例内容**：

```rust
//! 多线程控制演示
//!
//! 演示如何在多线程环境下安全地控制机械臂。
//! 由于 Type State Pattern 的设计，不能再"提取" Piper 传递给其他线程。
//! 正确的做法是使用 Arc<Mutex<Piper>> 来共享机器人实例。

use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use piper_sdk::client::state::*;
use piper_sdk::client::types::*;
use piper_sdk::PiperBuilder;

fn main() -> Result<()> {
    // 连接并使能机械臂
    let robot = PiperBuilder::new()
        .interface("can0")
        .build()?;
    let robot = robot.enable_position_mode(PositionModeConfig::default())?;

    // ✅ 使用 Arc<Mutex<>> 共享机器人实例
    let robot = Arc::new(Mutex::new(robot));

    // 创建控制线程
    let robot_clone = robot.clone();
    let control_thread = thread::spawn(move || {
        let positions = JointArray::from([Rad(0.0); 6]);
        for _ in 0..100 {
            // 获取锁并发送命令
            if let Ok(robot) = robot_clone.lock() {
                let _ = robot.send_position_command(&positions);
            }
            thread::sleep(Duration::from_millis(10));
        }
    });

    // 主线程可以进行其他操作（如监控状态）
    let observer = {
        let robot = robot.lock().unwrap();
        robot.observer().clone()
    };

    for _ in 0..10 {
        let positions = observer.joint_positions();
        println!("Current positions: {:?}", positions);
        thread::sleep(Duration::from_millis(100));
    }

    control_thread.join().unwrap();

    // 失能机械臂
    let robot = Arc::try_unwrap(robot)
        .expect("Other threads still hold reference")
        .into_inner()
        .unwrap();
    let _robot = robot.disable(DisableConfig::default())?;

    Ok(())
}
```

**设计说明**：

此示例展示了新架构下的多线程控制模式：
- **使用 `Arc<Mutex<Piper>>`**：而非提取 Piper
- **锁粒度控制**：每次发送命令时获取锁，完成后立即释放
- **Observer 可以 clone**：用于只读监控，不需要持有锁

---

### 2.3 文档变更

#### 文件 7-8：`README.md` / `README.zh-CN.md`

**变更类型**：修改

**当前状态**：
```rust
let motion = robot.Piper;
let observer = robot.observer();

// 发送命令
motion.send_mit_command(...)?;
```

**目标状态**：
```rust
let observer = robot.observer();

// 发送命令
robot.command_torques(...)?;
```

---

#### 文件 9+：`docs/v0/*.md`

**需要更新的文档**（共约 20 个文件）：

| 文件 | 优先级 | 说明 |
|------|--------|------|
| position_control_user_guide.md | P0 | 用户指南，必须更新 |
| motion_commander_interface_analysis.md | P1 | 添加"已废弃"说明 |
| high-level-api/*.md | P2 | 批量搜索替换 |
| 其他 docs/v0/*.md | P3 | 批量搜索替换 |

---

## 3. 新增方法设计

### 3.1 `Piper<Active<MitMode>>` 新增方法

```rust
impl Piper<Active<MitMode>> {
    /// 控制夹爪
    ///
    /// # 参数
    ///
    /// - `position`: 夹爪开口（0.0-1.0，1.0 = 完全打开）
    /// - `effort`: 夹持力度（0.0-1.0，1.0 = 最大力度）
    pub fn set_gripper(&self, position: f64, effort: f64) -> Result<()> {
        // 参数验证
        if !(0.0..=1.0).contains(&position) {
            return Err(RobotError::ConfigError(
                "Gripper position must be in [0.0, 1.0]".to_string(),
            ));
        }
        if !(0.0..=1.0).contains(&effort) {
            return Err(RobotError::ConfigError(
                "Gripper effort must be in [0.0, 1.0]".to_string(),
            ));
        }

        let raw = RawCommander::new(&self.driver);
        raw.send_gripper_command(position, effort)
    }

    /// 打开夹爪
    ///
    /// 便捷方法，相当于 `set_gripper(1.0, 0.3)`
    pub fn open_gripper(&self) -> Result<()> {
        self.set_gripper(1.0, 0.3)
    }

    /// 关闭夹爪
    ///
    /// 便捷方法，相当于 `set_gripper(0.0, effort)`
    pub fn close_gripper(&self, effort: f64) -> Result<()> {
        self.set_gripper(0.0, effort)
    }
}
```

### 3.2 `Piper<Active<PositionMode>>` 新增方法

```rust
impl Piper<Active<PositionMode>> {
    /// 发送位置命令（批量发送所有关节）
    ///
    /// 一次性发送所有 6 个关节的目标位置。
    ///
    /// # 参数
    ///
    /// - `positions`: 各关节目标位置
    ///
    /// # 示例
    ///
    /// ```rust,ignore
    /// let positions = JointArray::from([
    ///     Rad(1.0), Rad(0.5), Rad(0.0), Rad(0.0), Rad(0.0), Rad(0.0)
    /// ]);
    /// robot.send_position_command(&positions)?;
    /// ```
    pub fn send_position_command(&self, positions: &JointArray<Rad>) -> Result<()> {
        let raw = RawCommander::new(&self.driver);
        raw.send_position_command_batch(positions)
    }

    /// 控制夹爪（同 MitMode）
    pub fn set_gripper(&self, position: f64, effort: f64) -> Result<()> {
        // ... 同 MitMode 实现
    }

    /// 打开夹爪（同 MitMode）
    pub fn open_gripper(&self) -> Result<()> {
        self.set_gripper(1.0, 0.3)
    }

    /// 关闭夹爪（同 MitMode）
    pub fn close_gripper(&self, effort: f64) -> Result<()> {
        self.set_gripper(0.0, effort)
    }
}
```

### 3.3 夹爪代码复用：宏 vs Trait vs 直接复制

夹爪方法在两个 impl 块中完全相同，有三种实现方式：

#### 方案 A：使用宏（推荐）

```rust
macro_rules! impl_gripper_methods {
    ($state:ty) => {
        impl Piper<Active<$state>> {
            pub fn set_gripper(&self, position: f64, effort: f64) -> Result<()> {
                if !(0.0..=1.0).contains(&position) {
                    return Err(RobotError::ConfigError(
                        "Gripper position must be in [0.0, 1.0]".to_string(),
                    ));
                }
                if !(0.0..=1.0).contains(&effort) {
                    return Err(RobotError::ConfigError(
                        "Gripper effort must be in [0.0, 1.0]".to_string(),
                    ));
                }
                let raw = RawCommander::new(&self.driver);
                raw.send_gripper_command(position, effort)
            }

            pub fn open_gripper(&self) -> Result<()> {
                self.set_gripper(1.0, 0.3)
            }

            pub fn close_gripper(&self, effort: f64) -> Result<()> {
                self.set_gripper(0.0, effort)
            }
        }
    };
}

impl_gripper_methods!(MitMode);
impl_gripper_methods!(PositionMode);
```

**优点**：
- 代码不重复，修改一处即可
- 不引入额外的公开 API（如 Trait）
- 编译后与直接复制效果相同

#### 方案 B：使用 Trait

```rust
pub trait GripperControl {
    fn driver(&self) -> &Arc<RobotPiper>;

    fn set_gripper(&self, position: f64, effort: f64) -> Result<()> {
        // ... 默认实现
    }
    // ...
}

impl GripperControl for Piper<Active<MitMode>> {
    fn driver(&self) -> &Arc<RobotPiper> { &self.driver }
}
impl GripperControl for Piper<Active<PositionMode>> {
    fn driver(&self) -> &Arc<RobotPiper> { &self.driver }
}
```

**缺点**：
- 引入公开 Trait，增加 API 复杂度
- 用户可能困惑什么时候用 Trait 方法
- 不如宏简洁

#### 方案 C：直接复制

**优点**：代码直观，无需理解宏

**缺点**：代码重复，修改需要同步两处

#### 决策

**推荐方案 A（宏）**，原因：
1. 夹爪方法逻辑完全相同，适合宏
2. 不引入公开 API 复杂度
3. 宏定义放在 `machine.rs` 顶部，易于维护

**注意**：如果将来需要为不同模式定制夹爪行为，可以改为直接复制。

---

## 4. API 迁移指南

### 4.1 MIT 模式

| 旧 API | 新 API |
|--------|--------|
| `robot.Piper.send_mit_command(&pos, &vel, kp, kd, &torques)` | `robot.command_torques(&pos, &vel, kp, kd, &torques)` |
| `robot.Piper.command_torques(torques)` | *(直接使用 command_torques，设置 kp=kd=0)* |
| `robot.Piper.set_gripper(pos, effort)` | `robot.set_gripper(pos, effort)` |
| `robot.Piper.open_gripper()` | `robot.open_gripper()` |
| `robot.Piper.close_gripper(effort)` | `robot.close_gripper(effort)` |

### 4.2 位置模式

| 旧 API | 新 API |
|--------|--------|
| `robot.Piper.send_position_command(&positions)` | `robot.send_position_command(&positions)` |
| `robot.Piper.send_cartesian_pose(pos, ori)` | `robot.command_cartesian_pose(pos, ori)` |
| `robot.Piper.move_linear(pos, ori)` | `robot.move_linear(pos, ori)` |
| `robot.Piper.move_circular(...)` | `robot.move_circular(...)` |
| `robot.Piper.set_gripper(pos, effort)` | `robot.set_gripper(pos, effort)` |
| `robot.Piper.open_gripper()` | `robot.open_gripper()` |
| `robot.Piper.close_gripper(effort)` | `robot.close_gripper(effort)` |

### 4.3 无法迁移的模式（破坏性变更）

以下模式**不再支持**，这是**有意为之**：

```rust
// ❌ 旧代码：获取 Piper 并保存
let motion = robot.Piper;
std::thread::spawn(move || {
    loop {
        motion.send_position_command(&positions)?;  // ❌ 不再可能
    }
});

// ✅ 新代码：传递整个 robot（需要同步机制）
let robot = Arc::new(Mutex::new(robot));
let robot_clone = robot.clone();
std::thread::spawn(move || {
    loop {
        robot_clone.lock().unwrap().send_position_command(&positions)?;
    }
});
```

#### 为什么这样做对您有好处？

这不是一个"限制"，而是一个**保护**：

| 旧模式的风险 | 新模式的保护 |
|-------------|-------------|
| 机器人 `disable()` 后，Piper 仍可发送命令 | 无法在 Standby 状态发送命令（编译错误） |
| 急停后，Piper 仍可发送命令 | 急停后状态转换为 ErrorState，无法发送命令 |
| 模式切换后，旧 Commander 可能发送错误类型的命令 | 每个模式只能发送该模式支持的命令 |
| **潜在后果**：机械臂失控、碰撞、硬件损坏 | **安全保证**：编译期防止非法操作 |

**核心原则**：宁可让您多写几行代码（`Arc<Mutex<>>`），也不能让您的机器人在错误状态下失控。

详细的多线程使用方式请参考 `examples/multi_threaded_demo.rs`。

---

## 5. 执行步骤

### 阶段 0：前置检查（约 10 分钟）

| 步骤 | 任务 | 说明 | 预计时间 |
|------|------|------|----------|
| 0.1 | 检查 `motion.rs` 内容 | 确认只包含 Piper，无其他共用类型 | 5 min |
| 0.2 | 确认 `RawCommander` 可见性 | 确保 `pub(crate) struct` 和 `pub(crate) fn new()` | 2 min |
| 0.3 | 检查是否存在其他 Active 模式 | 搜索 TeachMode、DragMode 等 | 3 min |

**检查结果**：
- ✅ `motion.rs` 只包含 Piper，可安全删除
- ✅ `RawCommander` 可见性正确
- ✅ 不存在其他需要处理的 Active 模式

### 阶段 1：核心代码修改（约 2 小时）

| 步骤 | 任务 | 文件 | 预计时间 |
|------|------|------|----------|
| 1.1 | 定义夹爪控制宏（可选） | machine.rs | 10 min |
| 1.2 | 修改 `Piper<Active<MitMode>>` 方法 | machine.rs | 30 min |
| 1.3 | 修改 `Piper<Active<PositionMode>>` 方法 | machine.rs | 30 min |
| 1.4 | 添加夹爪方法（使用宏或直接实现） | machine.rs | 20 min |
| 1.5 | 删除 `motion.rs` | motion.rs | 5 min |
| 1.6 | 更新 `mod.rs` | mod.rs | 5 min |
| 1.7 | 修复编译错误 | - | 20 min |

### 阶段 2：示例代码更新（约 45 分钟）

| 步骤 | 任务 | 文件 | 预计时间 |
|------|------|------|----------|
| 2.1 | 更新 position_control_demo.rs | examples/ | 10 min |
| 2.2 | 更新 high_level_gripper_control.rs | examples/ | 10 min |
| 2.3 | 更新 high_level_simple_move.rs | examples/ | 5 min |
| 2.4 | **新增 multi_threaded_demo.rs** | examples/ | **15 min** |
| 2.5 | 运行示例验证 | - | 5 min |

### 阶段 3：测试（约 30 分钟）

| 步骤 | 任务 | 说明 | 预计时间 |
|------|------|------|----------|
| 3.1 | 运行单元测试 | `cargo test` | 10 min |
| 3.2 | 运行集成测试 | `cargo test --test '*'` | 10 min |
| 3.3 | 修复失败的测试 | - | 10 min |

### 阶段 4：文档更新（约 1 小时）

| 步骤 | 任务 | 文件 | 预计时间 |
|------|------|------|----------|
| 4.1 | 更新 README | README.md, README.zh-CN.md | 15 min |
| 4.2 | 更新用户指南 | position_control_user_guide.md | 15 min |
| 4.3 | 批量更新 docs/v0/*.md | grep + sed | 20 min |
| 4.4 | 更新 CHANGELOG | CHANGELOG.md | 10 min |

### 阶段 5：最终验证（约 15 分钟）

| 步骤 | 任务 | 说明 |
|------|------|------|
| 5.1 | `cargo build` | 确保编译通过 |
| 5.2 | `cargo test` | 确保测试通过 |
| 5.3 | `cargo doc` | 确保文档生成正确 |
| 5.4 | `cargo clippy` | 确保无警告 |
| 5.5 | 验证 multi_threaded_demo 可运行 | 确保新示例正确 |

---

## 6. 详细代码变更

### 6.1 machine.rs：`Piper<Active<MitMode>>` 修改

```rust
// ==================== Active<MitMode> 状态 ====================

impl Piper<Active<MitMode>> {
    /// 发送 MIT 模式控制指令
    ///
    /// 对所有关节发送位置、速度、力矩的混合控制指令。
    ///
    /// # 参数
    ///
    /// - `positions`: 各关节目标位置（Rad）
    /// - `velocities`: 各关节目标速度（rad/s）
    /// - `kp`: 位置增益（所有关节相同）
    /// - `kd`: 速度增益（所有关节相同）
    /// - `torques`: 各关节前馈力矩（NewtonMeter）
    pub fn command_torques(
        &self,
        positions: &JointArray<Rad>,
        velocities: &JointArray<f64>,
        kp: f64,
        kd: f64,
        torques: &JointArray<NewtonMeter>,
    ) -> Result<()> {
        // ✅ 直接使用 RawCommander，避免创建 Piper
        let raw = RawCommander::new(&self.driver);
        raw.send_mit_command_batch(positions, velocities, kp, kd, torques)
    }

    /// 控制夹爪
    ///
    /// # 参数
    ///
    /// - `position`: 夹爪开口（0.0-1.0，1.0 = 完全打开）
    /// - `effort`: 夹持力度（0.0-1.0，1.0 = 最大力度）
    pub fn set_gripper(&self, position: f64, effort: f64) -> Result<()> {
        if !(0.0..=1.0).contains(&position) {
            return Err(RobotError::ConfigError(
                "Gripper position must be in [0.0, 1.0]".to_string(),
            ));
        }
        if !(0.0..=1.0).contains(&effort) {
            return Err(RobotError::ConfigError(
                "Gripper effort must be in [0.0, 1.0]".to_string(),
            ));
        }

        let raw = RawCommander::new(&self.driver);
        raw.send_gripper_command(position, effort)
    }

    /// 打开夹爪
    ///
    /// 便捷方法，相当于 `set_gripper(1.0, 0.3)`
    pub fn open_gripper(&self) -> Result<()> {
        self.set_gripper(1.0, 0.3)
    }

    /// 关闭夹爪
    ///
    /// 便捷方法，相当于 `set_gripper(0.0, effort)`
    pub fn close_gripper(&self, effort: f64) -> Result<()> {
        self.set_gripper(0.0, effort)
    }

    // ❌ 删除 Piper 方法

    /// 获取 Observer（只读）
    pub fn observer(&self) -> &Observer {
        &self.observer
    }

    // ... disable() 等其他方法保持不变
}
```

### 6.2 machine.rs：`Piper<Active<PositionMode>>` 修改

```rust
// ==================== Active<PositionMode> 状态 ====================

impl Piper<Active<PositionMode>> {
    /// 发送位置命令（批量发送所有关节）
    ///
    /// 一次性发送所有 6 个关节的目标位置。
    ///
    /// # 参数
    ///
    /// - `positions`: 各关节目标位置
    pub fn send_position_command(&self, positions: &JointArray<Rad>) -> Result<()> {
        let raw = RawCommander::new(&self.driver);
        raw.send_position_command_batch(positions)
    }

    /// 发送末端位姿命令（笛卡尔空间控制）
    pub fn command_cartesian_pose(
        &self,
        position: Position3D,
        orientation: EulerAngles,
    ) -> Result<()> {
        let raw = RawCommander::new(&self.driver);
        raw.send_end_pose_command(position, orientation)
    }

    /// 发送直线运动命令
    pub fn move_linear(
        &self,
        position: Position3D,
        orientation: EulerAngles,
    ) -> Result<()> {
        let raw = RawCommander::new(&self.driver);
        raw.send_end_pose_command(position, orientation)
    }

    /// 发送圆弧运动命令
    pub fn move_circular(
        &self,
        via_position: Position3D,
        via_orientation: EulerAngles,
        target_position: Position3D,
        target_orientation: EulerAngles,
    ) -> Result<()> {
        let raw = RawCommander::new(&self.driver);
        raw.send_circular_motion(
            via_position,
            via_orientation,
            target_position,
            target_orientation,
        )
    }

    /// 更新单个关节位置（保持其他关节不变）
    pub fn command_position(&self, joint: Joint, position: Rad) -> Result<()> {
        let mut positions = self.observer.joint_positions();
        positions[joint] = position;
        self.send_position_command(&positions)
    }

    /// 控制夹爪
    pub fn set_gripper(&self, position: f64, effort: f64) -> Result<()> {
        if !(0.0..=1.0).contains(&position) {
            return Err(RobotError::ConfigError(
                "Gripper position must be in [0.0, 1.0]".to_string(),
            ));
        }
        if !(0.0..=1.0).contains(&effort) {
            return Err(RobotError::ConfigError(
                "Gripper effort must be in [0.0, 1.0]".to_string(),
            ));
        }

        let raw = RawCommander::new(&self.driver);
        raw.send_gripper_command(position, effort)
    }

    /// 打开夹爪
    pub fn open_gripper(&self) -> Result<()> {
        self.set_gripper(1.0, 0.3)
    }

    /// 关闭夹爪
    pub fn close_gripper(&self, effort: f64) -> Result<()> {
        self.set_gripper(0.0, effort)
    }

    // ❌ 删除 Piper 方法

    /// 获取 Observer（只读）
    pub fn observer(&self) -> &Observer {
        &self.observer
    }

    // ... disable() 等其他方法保持不变
}
```

### 6.3 mod.rs 修改

```rust
//! 客户端接口模块

pub mod builder;
pub mod control;
pub mod heartbeat;
// pub mod motion;  // ❌ 删除或注释掉
pub mod observer;
pub(crate) mod raw_commander;
pub mod state;
pub mod types;

// 重新导出常用类型
pub use builder::PiperBuilder;
// pub use motion::Piper;  // ❌ 删除
pub use observer::Observer;
pub use state::Piper;
pub use types::*;
```

---

## 7. 风险评估

### 7.1 技术风险

| 风险 | 影响 | 概率 | 缓解措施 |
|------|------|------|----------|
| 编译错误遗漏 | 低 | 中 | 全量编译 + clippy |
| 测试遗漏 | 中 | 低 | 运行完整测试套件 |
| 文档遗漏 | 低 | 高 | grep 搜索 + 人工审查 |

### 7.2 兼容性风险

| 风险 | 影响 | 说明 |
|------|------|------|
| API 破坏性变更 | 高 | **这是有意为之的破坏性变更** |
| 用户代码需要修改 | 高 | 提供迁移指南 |

**重要**：这是一个**有意的破坏性变更**，因为旧 API 存在安全隐患。

---

## 8. 验收标准

### 8.1 功能验收

- [ ] `cargo build` 编译通过
- [ ] `cargo test` 所有测试通过
- [ ] `cargo clippy` 无警告
- [ ] `cargo doc` 文档生成正确

### 8.2 安全验收

- [ ] 无法再获取独立的 Piper
- [ ] 所有运动命令只能通过 `Piper<Active<M>>` 发送
- [ ] Type State 安全性得到保证

### 8.3 示例验收

- [ ] `cargo run --example position_control_demo` 运行正常
- [ ] 其他示例运行正常

---

## 9. 回滚计划

如果执行过程中出现严重问题，可以通过 git 回滚：

```bash
git checkout HEAD~1 -- src/client/
git checkout HEAD~1 -- examples/
```

**注意**：由于这是架构级变更，回滚后需要确保所有依赖正确。

---

## 10. 总结

### 10.1 关键变更

1. **删除 `Piper`** - 移除中间层，直接在 Piper 上调用
2. **保持 Type State 安全性** - 命令只能在正确状态下发送
3. **简化架构** - 减少不必要的抽象

### 10.2 预计工作量

| 阶段 | 预计时间 |
|------|----------|
| 前置检查 | 10 分钟 |
| 核心代码修改 | 2 小时 |
| 示例代码更新 | 45 分钟 |
| 测试 | 30 分钟 |
| 文档更新 | 1 小时 |
| 最终验证 | 15 分钟 |
| **总计** | **约 5 小时** |

---

## 附录 A：审查意见采纳记录

本文档根据以下审查意见进行了更新（v1.1）：

| 建议 | 状态 | 说明 |
|------|------|------|
| 检查 `motion.rs` 是否有非 Piper 依赖 | ✅ 已采纳 | 添加到阶段 0 |
| 添加多线程示例 `multi_threaded_demo.rs` | ✅ 已采纳 | 添加到阶段 2 |
| 确认 `RawCommander` 可见性 | ✅ 已采纳 | 添加到阶段 0 |
| 检查是否存在 TeachMode | ✅ 已采纳 | 添加到阶段 0（结果：不存在） |
| 改进迁移指南语气，解释"为什么对用户有好处" | ✅ 已采纳 | 更新 4.3 节 |
| 考虑使用宏减少夹爪代码重复 | ✅ 已采纳 | 添加到阶段 1.1 |

---

**文档版本**：v1.1
**创建日期**：2026-01-24
**最后更新**：2026-01-24
**状态**：✅ 已完成执行

