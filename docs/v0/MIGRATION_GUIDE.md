# 模块重构迁移指南

## 概述

Piper SDK 进行了模块组织重构，将模块结构从功能导向命名改为更符合 Rust 社区习惯的组织方式。本指南帮助您将代码从旧 API 迁移到新 API。

**注意**：由于项目尚未发布，旧模块路径已完全移除，必须使用新 API。

## 主要变更

### 模块重命名

| 旧模块 | 新模块 | 说明 |
|--------|--------|------|
| `robot` | `driver` | 设备驱动层 |
| `high_level` | `client` | 客户端接口层 |
| - | `prelude` | 便捷导入模块（新增） |

### 类型重命名

| 旧类型 | 新类型 | 说明 |
|--------|--------|------|
| `robot::Piper` | `driver::Piper` | 驱动层 API |
| `high_level::Piper` | `client::Piper` | 客户端 API |
| `RobotError` | `DriverError` | 错误类型 |

## 迁移步骤

### 1. 驱动层 API 迁移

#### 旧代码

```rust
use piper_sdk::robot::{PiperBuilder, RobotError};

fn main() -> Result<(), RobotError> {
    let robot = PiperBuilder::new()
        .interface("can0")?
        .baud_rate(1_000_000)?
        .build()?;

    let state = robot.get_joint_position();
    Ok(())
}
```

#### 新代码

```rust
use piper_sdk::driver::{PiperBuilder, DriverError};

fn main() -> Result<(), DriverError> {
    let robot = PiperBuilder::new()
        .interface("can0")?
        .baud_rate(1_000_000)?
        .build()?;

    let state = robot.get_joint_position();
    Ok(())
}
```

#### 或者使用类型别名

```rust
use piper_sdk::Driver;  // 类型别名，等同于 driver::Piper
use piper_sdk::driver::PiperBuilder;
```

### 2. 客户端 API 迁移

#### 旧代码

```rust
use piper_sdk::high_level::{
    control::TrajectoryPlanner,
    types::{JointArray, Rad},
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let positions = JointArray::from([Rad(0.0); 6]);
    // ...
    Ok(())
}
```

#### 新代码（推荐：使用 prelude）

```rust
use piper_sdk::prelude::*;
use piper_sdk::client::control::TrajectoryPlanner;

fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let positions = JointArray::from([Rad(0.0); 6]);
    // ...
    Ok(())
}
```

#### 或者直接导入

```rust
use piper_sdk::client::control::TrajectoryPlanner;
use piper_sdk::client::types::{JointArray, Rad};
```

### 3. 错误类型迁移

#### 旧代码

```rust
use piper_sdk::robot::RobotError;

fn handle_error(err: RobotError) {
    match err {
        RobotError::Can(e) => { /* ... */ }
        RobotError::Protocol(e) => { /* ... */ }
        _ => { /* ... */ }
    }
}
```

#### 新代码

```rust
use piper_sdk::driver::DriverError;

fn handle_error(err: DriverError) {
    match err {
        DriverError::Can(e) => { /* ... */ }
        DriverError::Protocol(e) => { /* ... */ }
        _ => { /* ... */ }
    }
}
```

### 4. 使用 Prelude 模块（推荐）

Prelude 模块提供了常用类型的便捷导入：

```rust
use piper_sdk::prelude::*;

// 现在可以直接使用：
// - Piper (客户端 API)
// - Piper, Observer
// - JointArray, Rad, Deg, NewtonMeter
// - CanAdapter
// - Driver (驱动层类型别名)
// - 错误类型：CanError, ProtocolError, DriverError
```

## 常见问题

### Q: 我的代码仍然使用旧模块路径，会编译失败吗？

**A**: 是的。由于项目尚未发布，旧模块路径已完全移除，必须使用新 API。请按照本指南进行迁移。

### Q: 如何快速迁移？

**A**: 使用查找替换功能：
- `piper_sdk::robot::` → `piper_sdk::driver::`
- `piper_sdk::high_level::` → `piper_sdk::client::`
- `RobotError` → `DriverError`

### Q: 新 API 和旧 API 的性能有差异吗？

**A**: 没有。新 API 只是模块重命名，底层实现完全相同，性能没有变化。

### Q: 我应该使用 `client` 还是 `driver` 模块？

**A**:
- **大多数用户**：使用 `client` 模块（类型安全、易于使用）
- **高级用户**：需要直接控制 CAN 帧或追求最高性能时，使用 `driver` 模块

## 迁移检查清单

- [ ] 更新所有 `use piper_sdk::robot::` → `use piper_sdk::driver::`
- [ ] 更新所有 `use piper_sdk::high_level::` → `use piper_sdk::client::`
- [ ] 更新所有 `RobotError` → `DriverError`
- [ ] 更新示例代码和测试
- [ ] 运行 `cargo build` 检查编译警告
- [ ] 运行所有测试确保功能正常
- [ ] 更新文档和注释

## 获取帮助

如果您在迁移过程中遇到问题，请：

1. 查看 [API 文档](https://docs.rs/piper-sdk)
2. 查看 [示例代码](../examples/)
3. 提交 Issue 到项目仓库

---

**注意**：由于项目尚未发布，所有旧模块路径已完全移除，必须使用新 API。

**最后更新**: 2024-01-24

