# High Level API 迁移指南

**版本：** v0.0.2
**更新日期：** 2025-01-24

---

## 概述

本文档帮助用户从旧版本的 High Level API 迁移到重构后的新 API。新 API 采用了 Type State Pattern，提供了更好的类型安全性和性能。

---

## 主要变更

### 1. 状态管理：Type State Pattern

**旧 API：**
```rust
// 状态通过运行时检查
let robot = Robot::new();
if robot.is_enabled() {
    robot.send_command(...);
}
```

**新 API：**
```rust
// 状态通过类型系统保证
let robot = Piper::connect(adapter, config)?;
let robot = robot.enable_mit_mode(MitModeConfig::default())?;
// robot 现在是 Piper<Active<MitMode>>，可以调用 command_torques
robot.command_torques(Joint::J1, Rad(0.0), 0.0, 10.0, 0.8, NewtonMeter(0.0))?;
```

**优势：**
- 编译期类型安全，无法在错误状态下调用方法
- 无需运行时状态检查，性能更好
- 代码更清晰，意图更明确

---

### 2. Observer 模式：零拷贝访问

**旧 API：**
```rust
// 可能使用缓存层，有延迟
let state = robot.get_state();
let position = state.joint_positions[Joint::J1];
```

**新 API：**
```rust
// 零拷贝，直接从 robot::Piper 读取
let observer = robot.observer();
let position = observer.joint_positions()[Joint::J1];

// 或使用 snapshot 获取时间一致的数据
let snapshot = observer.snapshot();
let position = snapshot.position[Joint::J1];
let velocity = snapshot.velocity[Joint::J1];
```

**优势：**
- 零延迟：直接从底层读取，无缓存层
- 零拷贝：使用 ArcSwap 的 wait-free 读取
- 时间一致性：`snapshot()` 方法保证数据时间一致性

---

### 3. 配置参数化

**旧 API：**
```rust
// 硬编码参数
robot.enable_mit_mode();
```

**新 API：**
```rust
// 可配置参数
let config = MitModeConfig {
    timeout: Duration::from_secs(2),
    debounce_threshold: 3,
    poll_interval: Duration::from_millis(10),
};
robot.enable_mit_mode(config)?;
```

**优势：**
- 灵活性：可以根据场景调整参数
- 可维护性：参数集中管理

---

### 4. 位置控制：移除速度参数

**旧 API：**
```rust
robot.send_position_command(Joint::J1, Rad(1.57), 0.5)?;
```

**新 API：**
```rust
// 位置控制指令不包含速度，速度需要通过控制模式指令设置
robot.command_position(Joint::J1, Rad(1.57))?;
```

**注意：** 位置控制指令（0x155、0x156、0x157）只包含位置信息。速度需要通过控制模式指令（0x151）的 Byte 2（speed_percent）来设置。

---

### 5. 急停：Type State 转换

**旧 API：**
```rust
robot.emergency_stop();
// 可能仍然可以调用其他方法（运行时检查）
```

**新 API：**
```rust
let robot = robot.emergency_stop()?;
// robot 现在是 Piper<ErrorState>，无法调用 command_* 方法（编译期检查）
```

**优势：**
- 编译期安全：无法在错误状态下继续发送命令
- 无需运行时检查

---

## 迁移步骤

### 步骤 1：更新连接代码

**旧代码：**
```rust
let robot = Robot::new(can_adapter)?;
```

**新代码：**
```rust
use piper_sdk::high_level::state::*;

let adapter = /* CAN 适配器 */;
let config = ConnectionConfig::default();
let robot = Piper::connect(adapter, config)?;
```

---

### 步骤 2：更新使能代码

**旧代码：**
```rust
robot.enable_arm()?;
robot.set_control_mode(ControlMode::Mit)?;
```

**新代码：**
```rust
let config = MitModeConfig::default();
let robot = robot.enable_mit_mode(config)?;
// robot 现在是 Piper<Active<MitMode>>
```

---

### 步骤 3：更新状态读取代码

**旧代码：**
```rust
let state = robot.get_state();
let position = state.joint_positions[Joint::J1];
```

**新代码：**
```rust
let observer = robot.observer();
let position = observer.joint_positions()[Joint::J1];

// 或使用 snapshot（推荐用于控制算法）
let snapshot = observer.snapshot();
let position = snapshot.position[Joint::J1];
let velocity = snapshot.velocity[Joint::J1];
```

---

### 步骤 4：更新命令发送代码

**旧代码：**
```rust
robot.send_mit_command(Joint::J1, 0.0, 0.0, 10.0, 0.8, 0.0)?;
```

**新代码：**
```rust
use piper_sdk::high_level::types::*;

robot.command_torques(
    Joint::J1,
    Rad(0.0),
    0.0,  // velocity
    10.0, // kp
    0.8,  // kd
    NewtonMeter(0.0),
)?;
```

---

### 步骤 5：更新位置控制代码

**旧代码：**
```rust
robot.send_position_command(Joint::J1, 1.57, 0.5)?;
```

**新代码：**
```rust
robot.command_position(Joint::J1, Rad(1.57))?;
// 注意：速度需要通过控制模式指令设置
```

---

### 步骤 6：更新急停代码

**旧代码：**
```rust
robot.emergency_stop()?;
// 可能仍然可以调用其他方法
```

**新代码：**
```rust
let robot = robot.emergency_stop()?;
// robot 现在是 Piper<ErrorState>，无法调用 command_* 方法
```

---

## 常见问题

### Q: 如何在不同线程中使用 Observer？

**A:** Observer 实现了 `Clone`，可以在不同线程中使用：

```rust
let observer = robot.observer();
let observer2 = observer.clone();

std::thread::spawn(move || {
    loop {
        let snapshot = observer2.snapshot();
        // ... 处理数据 ...
    }
});
```

---

### Q: 如何设置位置控制的速度？

**A:** 速度需要通过控制模式指令（0x151）设置，而不是位置控制指令。请参考协议文档。

---

### Q: 如何从 ErrorState 恢复？

**A:** 当前版本中，`Piper<ErrorState>` 不提供恢复方法。如果需要恢复，需要重新连接。

---

### Q: 性能如何？

**A:** 新 API 采用了零拷贝设计，性能优于旧版本：
- Observer 读取：~20ns（ArcSwap wait-free 读取）
- 无锁竞争
- 无缓存延迟

---

## 完整示例

```rust
use piper_sdk::high_level::state::*;
use piper_sdk::high_level::types::*;
use std::time::Duration;

// 1. 连接
let adapter = /* CAN 适配器 */;
let config = ConnectionConfig::default();
let robot = Piper::connect(adapter, config)?;

// 2. 使能 MIT 模式
let mit_config = MitModeConfig::default();
let robot = robot.enable_mit_mode(mit_config)?;

// 3. 获取 Observer（可以在不同线程使用）
let observer = robot.observer().clone();
std::thread::spawn(move || {
    loop {
        let snapshot = observer.snapshot();
        // ... 监控状态 ...
    }
});

// 4. 发送控制命令
for _ in 0..100 {
    robot.command_torques(
        Joint::J1,
        Rad(0.0),
        0.0,
        10.0,
        0.8,
        NewtonMeter(0.0),
    )?;
    std::thread::sleep(Duration::from_millis(10));
}

// 5. 急停
let robot = robot.emergency_stop()?;
// robot 现在是 Piper<ErrorState>

// 6. 失能
let disable_config = DisableConfig::default();
let robot = robot.disable(disable_config)?;
// robot 现在是 Piper<Standby>
```

---

## 更多资源

- [快速开始指南](./QUICK_START.md)
- [最佳实践](./BEST_PRACTICES.md)
- [API 文档](../api.md)

