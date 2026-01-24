# High Level API 最佳实践

**版本：** v0.0.2
**更新日期：** 2025-01-24

---

## 概述

本文档提供 High Level API 的使用建议和最佳实践，帮助用户编写高效、安全的机械臂控制代码。

---

## 1. 状态读取

### ✅ 推荐：使用 `snapshot()` 获取时间一致的数据

```rust
let snapshot = observer.snapshot();
let position = snapshot.position[Joint::J1];
let velocity = snapshot.velocity[Joint::J1];
let torque = snapshot.torque[Joint::J1];
// 这些数据是在同一时刻读取的，时间一致
```

**优势：**
- 时间一致性：所有数据在同一时刻读取
- 适合控制算法：阻抗控制、力矩控制等需要时间一致性的算法

### ⚠️ 注意：独立读取可能有时间偏斜

```rust
let position = observer.joint_positions()[Joint::J1];
let velocity = observer.joint_velocities()[Joint::J1];
// 这两个读取之间可能有时间差，导致数据不一致
```

**适用场景：**
- 只需要单个状态值
- 对时间一致性要求不高

---

## 2. 多线程使用

### ✅ 推荐：克隆 Observer 到不同线程

```rust
let observer = robot.observer().clone();

// 监控线程
std::thread::spawn(move || {
    loop {
        let snapshot = observer.snapshot();
        // ... 记录数据、监控状态 ...
        std::thread::sleep(Duration::from_millis(100));
    }
});

// 控制线程
loop {
    robot.command_torques(...)?;
    std::thread::sleep(Duration::from_millis(10));
}
```

**优势：**
- Observer 是只读的，可以安全地在多线程中使用
- 零拷贝：多个 Observer 共享同一个 `robot::Piper` 实例
- 无锁竞争：使用 ArcSwap 的 wait-free 读取

---

## 3. 控制频率

### ✅ 推荐：使用固定频率控制循环

```rust
let control_freq = 100.0; // 100Hz
let dt = Duration::from_secs_f64(1.0 / control_freq);

loop {
    let loop_start = Instant::now();

    // 读取状态
    let snapshot = observer.snapshot();

    // 计算控制输出
    // ...

    // 发送命令
    robot.command_torques(...)?;

    // 控制频率
    let elapsed = loop_start.elapsed();
    if elapsed < dt {
        std::thread::sleep(dt - elapsed);
    }
}
```

**优势：**
- 稳定的控制频率
- 可预测的延迟

---

## 4. 错误处理

### ✅ 推荐：使用 `?` 操作符和早期返回

```rust
fn control_loop(robot: Piper<Active<MitMode>>) -> Result<()> {
    let observer = robot.observer();

    for _ in 0..1000 {
        let snapshot = observer.snapshot();

        // 检查状态
        if !snapshot.is_arm_enabled {
            return Err(RobotError::HardwareFailure {
                message: "Arm disabled".to_string(),
            });
        }

        robot.command_torques(...)?;
        std::thread::sleep(Duration::from_millis(10));
    }

    Ok(())
}
```

---

## 5. 配置参数

### ✅ 推荐：根据场景调整配置参数

```rust
// 快速响应场景
let fast_config = MitModeConfig {
    timeout: Duration::from_secs(1),
    debounce_threshold: 2,  // 降低阈值，更快响应
    poll_interval: Duration::from_millis(5),  // 更频繁轮询
};

// 稳定场景
let stable_config = MitModeConfig {
    timeout: Duration::from_secs(3),
    debounce_threshold: 5,  // 提高阈值，更稳定
    poll_interval: Duration::from_millis(20),  // 降低轮询频率
};
```

---

## 6. 急停处理

### ✅ 推荐：使用 Type State 保证安全

```rust
// 发生紧急情况
let robot = robot.emergency_stop()?;
// robot 现在是 Piper<ErrorState>，无法继续发送命令
// 这保证了在错误状态下不会意外发送命令
```

---

## 7. 资源管理

### ✅ 推荐：使用 `Drop` trait 自动清理

```rust
{
    let robot = Piper::connect(adapter, config)?;
    let robot = robot.enable_mit_mode(MitModeConfig::default())?;

    // ... 使用 robot ...

} // robot 在这里自动 Drop，会发送失能命令
```

**注意：** `Piper` 实现了 `Drop` trait，会在析构时自动发送失能命令。

---

## 8. 性能优化

### ✅ 推荐：在高频控制循环中优化

1. **使用 `snapshot()` 而不是多次独立读取**
2. **在循环外创建 Observer**
3. **使用 `send_realtime` 确保实时性**（已自动使用）

```rust
// ✅ 推荐
let observer = robot.observer();
loop {
    let snapshot = observer.snapshot();  // 一次读取，时间一致
    robot.command_torques(...)?;
}

// ❌ 不推荐
loop {
    let pos = observer.joint_positions();  // 多次读取，可能有时间偏斜
    let vel = observer.joint_velocities();
    robot.command_torques(...)?;
}
```

---

## 9. 类型安全

### ✅ 推荐：利用 Type State Pattern

```rust
// 编译期保证状态正确
let robot = Piper::connect(adapter, config)?;
// robot 是 Piper<Standby>，无法调用 command_torques（编译错误）

let robot = robot.enable_mit_mode(MitModeConfig::default())?;
// robot 是 Piper<Active<MitMode>>，可以调用 command_torques

robot.command_torques(...)?; // ✅ 编译通过
```

---

## 10. 测试

### ✅ 推荐：使用 MockCanAdapter 进行单元测试

```rust
use piper_sdk::can::MockCanAdapter;

#[test]
fn test_control_loop() {
    let adapter = MockCanAdapter::new();
    let config = ConnectionConfig::default();
    let robot = Piper::connect(adapter, config)?;
    // ... 测试逻辑 ...
}
```

---

## 常见错误

### ❌ 错误：在 Standby 状态下调用 command_*

```rust
let robot = Piper::connect(adapter, config)?;
robot.command_torques(...)?; // ❌ 编译错误
```

**修正：**
```rust
let robot = Piper::connect(adapter, config)?;
let robot = robot.enable_mit_mode(MitModeConfig::default())?;
robot.command_torques(...)?; // ✅ 正确
```

---

### ❌ 错误：在 ErrorState 状态下继续发送命令

```rust
let robot = robot.emergency_stop()?;
robot.command_torques(...)?; // ❌ 编译错误
```

**修正：**
```rust
let robot = robot.emergency_stop()?;
// 需要重新连接才能继续使用
```

---

### ❌ 错误：多次独立读取导致时间偏斜

```rust
let pos = observer.joint_positions()[Joint::J1];
let vel = observer.joint_velocities()[Joint::J1];
// 这两个读取之间可能有时间差
```

**修正：**
```rust
let snapshot = observer.snapshot();
let pos = snapshot.position[Joint::J1];
let vel = snapshot.velocity[Joint::J1];
// 时间一致
```

---

## 性能指标

- **Observer 读取延迟：** ~20ns（ArcSwap wait-free 读取）
- **无锁竞争：** ArcSwap 是 Wait-Free 的
- **零拷贝：** 直接访问底层数据，无中间拷贝

---

## 更多资源

- [快速开始指南](./QUICK_START.md)
- [迁移指南](./MIGRATION_GUIDE.md)
- [API 文档](../api.md)

