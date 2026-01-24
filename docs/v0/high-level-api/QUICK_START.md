# High Level API 快速开始指南

**版本：** v0.0.2
**更新日期：** 2025-01-24

---

## 概述

本指南帮助新用户快速上手 High Level API。High Level API 提供了类型安全、高性能的机械臂控制接口。

---

## 安装

在 `Cargo.toml` 中添加依赖：

```toml
[dependencies]
piper-sdk = { version = "0.0.2", features = [] }
```

---

## 基本使用

### 1. 连接机械臂

```rust
use piper_sdk::high_level::state::*;
use piper_sdk::can::SocketCanAdapter;

// 创建 CAN 适配器
let adapter = SocketCanAdapter::new("can0")?;

// 连接配置
let config = ConnectionConfig::default();
let robot = Piper::connect(adapter, config)?;
// robot 现在是 Piper<Standby>
```

---

### 2. 使能机械臂

#### MIT 模式（力矩控制）

```rust
use piper_sdk::high_level::types::*;

let config = MitModeConfig::default();
let robot = robot.enable_mit_mode(config)?;
// robot 现在是 Piper<Active<MitMode>>

// 发送力矩命令
robot.command_torques(
    Joint::J1,
    Rad(0.0),        // 位置参考
    0.0,            // 速度参考
    10.0,           // kp
    0.8,            // kd
    NewtonMeter(0.0), // 力矩参考
)?;
```

#### 位置模式

```rust
let config = PositionModeConfig::default();
let robot = robot.enable_position_mode(config)?;
// robot 现在是 Piper<Active<PositionMode>>

// 发送位置命令
robot.command_position(Joint::J1, Rad(1.57))?;
```

---

### 3. 读取状态

```rust
// 获取 Observer
let observer = robot.observer();

// 读取关节位置
let positions = observer.joint_positions();
println!("J1 position: {} deg", positions[Joint::J1].to_deg());

// 读取关节速度
let velocities = observer.joint_velocities();
println!("J1 velocity: {} rad/s", velocities[Joint::J1].value());

// 使用 snapshot 获取时间一致的数据（推荐）
let snapshot = observer.snapshot();
println!("Position: {:?}", snapshot.position);
println!("Velocity: {:?}", snapshot.velocity);
println!("Torque: {:?}", snapshot.torque);
```

---

### 4. 控制循环示例

```rust
use std::time::{Duration, Instant};

let robot = robot.enable_mit_mode(MitModeConfig::default())?;
let observer = robot.observer();

let start = Instant::now();
let control_freq = 100.0; // 100Hz
let dt = Duration::from_secs_f64(1.0 / control_freq);

loop {
    let loop_start = Instant::now();

    // 读取状态
    let snapshot = observer.snapshot();

    // 计算控制输出
    let position_ref = Rad(0.0);
    let velocity_ref = 0.0;
    let kp = 10.0;
    let kd = 0.8;
    let torque_ref = NewtonMeter(0.0);

    // 发送命令
    robot.command_torques(
        Joint::J1,
        position_ref,
        velocity_ref,
        kp,
        kd,
        torque_ref,
    )?;

    // 控制频率
    let elapsed = loop_start.elapsed();
    if elapsed < dt {
        std::thread::sleep(dt - elapsed);
    }

    // 运行 10 秒
    if start.elapsed() > Duration::from_secs(10) {
        break;
    }
}
```

---

### 5. 急停

```rust
// 发生紧急情况
let robot = robot.emergency_stop()?;
// robot 现在是 Piper<ErrorState>，无法继续发送命令
```

---

### 6. 失能

```rust
let disable_config = DisableConfig::default();
let robot = robot.disable(disable_config)?;
// robot 现在是 Piper<Standby>
```

---

## 多线程使用

### Observer 在多线程中使用

```rust
let observer = robot.observer().clone();

// 在另一个线程中监控状态
std::thread::spawn(move || {
    loop {
        let snapshot = observer.snapshot();
        // ... 处理数据 ...
        std::thread::sleep(Duration::from_millis(100));
    }
});

// 主线程继续控制
loop {
    robot.command_torques(...)?;
    std::thread::sleep(Duration::from_millis(10));
}
```

---

## 配置参数

### MitModeConfig

```rust
let config = MitModeConfig {
    timeout: Duration::from_secs(2),
    debounce_threshold: 3,  // 连续 3 次读到 Enabled 才认为成功
    poll_interval: Duration::from_millis(10),
};
```

### DisableConfig

```rust
let config = DisableConfig {
    timeout: Duration::from_secs(2),
    debounce_threshold: 3,  // 连续 3 次读到 Disabled 才认为成功
    poll_interval: Duration::from_millis(10),
};
```

---

## 错误处理

```rust
use piper_sdk::high_level::types::Result;

match robot.enable_mit_mode(MitModeConfig::default()) {
    Ok(active_robot) => {
        // 成功
    }
    Err(e) => {
        eprintln!("Error: {}", e);
        // 处理错误
    }
}
```

---

## 类型安全

新 API 使用 Type State Pattern，在编译期保证状态正确：

```rust
let robot = Piper::connect(adapter, config)?;
// robot 是 Piper<Standby>，无法调用 command_torques

let robot = robot.enable_mit_mode(MitModeConfig::default())?;
// robot 是 Piper<Active<MitMode>>，可以调用 command_torques

robot.command_torques(...)?; // ✅ 编译通过

let robot = robot.emergency_stop()?;
// robot 是 Piper<ErrorState>，无法调用 command_torques
robot.command_torques(...)?; // ❌ 编译错误
```

---

## 性能优化建议

1. **使用 `snapshot()` 而不是多次独立读取**
   ```rust
   // ❌ 不推荐：多次独立读取，可能有时间偏斜
   let pos = observer.joint_positions();
   let vel = observer.joint_velocities();

   // ✅ 推荐：使用 snapshot，保证时间一致性
   let snapshot = observer.snapshot();
   ```

2. **在高频控制循环中，Observer 可以克隆到不同线程**
   ```rust
   let observer = robot.observer().clone();
   // 在控制线程中使用 observer，不影响主线程
   ```

3. **使用 `send_realtime` 而不是 `send_reliable`**
   - 位置控制和 MIT 控制已经使用 `send_realtime`
   - 确保实时性，避免指令积压

---

## 完整示例

```rust
use piper_sdk::high_level::state::*;
use piper_sdk::high_level::types::*;
use piper_sdk::can::SocketCanAdapter;
use std::time::Duration;

fn main() -> Result<()> {
    // 1. 连接
    let adapter = SocketCanAdapter::new("can0")?;
    let config = ConnectionConfig::default();
    let robot = Piper::connect(adapter, config)?;

    // 2. 使能 MIT 模式
    let config = MitModeConfig::default();
    let robot = robot.enable_mit_mode(config)?;

    // 3. 获取 Observer
    let observer = robot.observer();

    // 4. 控制循环
    for _ in 0..1000 {
        let snapshot = observer.snapshot();

        // 简单的 PD 控制
        let position_ref = Rad(0.0);
        let position_error = position_ref - snapshot.position[Joint::J1];
        let velocity_ref = 0.0;
        let velocity_error = velocity_ref - snapshot.velocity[Joint::J1].value();

        let kp = 10.0;
        let kd = 0.8;
        let torque = NewtonMeter(kp * position_error.0 + kd * velocity_error);

        robot.command_torques(
            Joint::J1,
            position_ref,
            velocity_ref,
            kp,
            kd,
            torque,
        )?;

        std::thread::sleep(Duration::from_millis(10));
    }

    // 5. 失能
    let disable_config = DisableConfig::default();
    let _robot = robot.disable(disable_config)?;

    Ok(())
}
```

---

## 下一步

- 阅读 [迁移指南](./MIGRATION_GUIDE.md) 了解从旧 API 迁移的详细信息
- 阅读 [最佳实践](./BEST_PRACTICES.md) 了解使用建议
- 查看 [API 文档](../api.md) 了解完整的 API 参考

