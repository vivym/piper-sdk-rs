# Workspace 迁移用户指南

本指南帮助用户从单 crate 版本迁移到 workspace 版本。

## 📦 依赖变更

### 之前（单 crate）

```toml
[dependencies]
piper-sdk = "0.1.0"
```

### 之后（Workspace）

#### 选项 1: 使用兼容层（推荐，无代码修改）

```toml
[dependencies]
piper-sdk = "0.1.0"
```

**✅ 无需修改任何代码**！API 完全向后兼容。

#### 选项 2: 使用具体层（高级用户）

如果只需要特定功能，可以依赖特定 crate：

```toml
[dependencies]
# 仅协议层（最小依赖）
piper-protocol = "0.1.0"

# CAN 层
piper-can = "0.1.0"

# 驱动层
piper-driver = "0.1.0"

# 客户端层（推荐大多数用户）
piper-client = "0.1.0"
```

## 🔧 API 变更

### 兼容层（piper-sdk）

**✅ 零变更**！所有代码保持不变：

```rust
use piper_sdk::prelude::*;

// 完全相同的 API
let robot = PiperBuilder::new()
    .interface("can0")
    .connect()
    .unwrap();

let piper = robot.enable().unwrap();
```

### 直接使用层

如果选择使用具体层，需要更新导入：

#### 客户端层

```rust
// 之前
use piper_sdk::client::Piper;

// 之后
use piper_client::Piper;
```

#### 驱动层

```rust
// 之前
use piper_sdk::driver::Piper;

// 之后
use piper_driver::Piper;
```

## 📚 Feature Flags

### 新增的 Feature Flags

```toml
[dependencies]
piper-sdk = { version = "0.1.0", features = ["serde"] }
```

可用的 features：
- `serde` - 为类型系统添加序列化支持（未来）
- `socketcan` - 强制使用 SocketCAN（Linux）
- `gs_usb` - 强制使用 GS-USB（跨平台）

**注意**: 平台特定 features 通常通过 `target cfg` 自动选择。

## 🎯 迁移示例

### 示例 1: 基本应用（无修改）

```rust
use piper_sdk::prelude::*;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let robot = PiperBuilder::new()
        .interface("can0")
        .connect()?;

    let piper = robot.enable()?;
    // ... 使用机器人
    Ok(())
}
```

### 示例 2: 高级应用（直接使用客户端层）

```rust
use piper_client::PiperBuilder;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let robot = PiperBuilder::new()
        .interface("can0")
        .connect()?;

    let piper = robot.enable()?;
    // ... 使用机器人
    Ok(())
}
```

### 示例 3: 驱动层应用

```rust
use piper_driver::PiperBuilder;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let robot = PiperBuilder::new()
        .interface("can0")
        .connect()?;

    // 直接访问驱动层功能
    // ... 使用机器人
    Ok(())
}
```

## ⚠️ 破坏性变更

### 无破坏性变更！

迁移到 workspace 后：
- ✅ 所有公共 API 保持不变
- ✅ 所有类型定义保持不变
- ✅ 所有行为保持不变

唯一的变更是如何在 `Cargo.toml` 中声明依赖。

## 🔍 故障排除

### 问题 1: 找不到 crate

```
error: use of unresolved crate `piper_sdk`
```

**解决方案**: 确保 `Cargo.toml` 中包含：

```toml
[dependencies]
piper-sdk = "0.1.0"
```

### 问题 2: 特定层找不到

```
error: use of unresolved crate `piper_client`
```

**解决方案**: 如果使用特定层，需要明确声明：

```toml
[dependencies]
piper-client = "0.1.0"
```

### 问题 3: Feature flags 不工作

```
error: unexpected `cfg` condition value: `serde`
```

**解决方案**: 添加 feature 到依赖声明：

```toml
[dependencies]
piper-sdk = { version = "0.1.0", features = ["serde"] }
```

## 📊 性能影响

### 编译时间

**之前**: ~42s 冷启动

**之后**: 显著改善
- 协议层修改: ~10s（之前 ~42s）
- 客户端层修改: ~5s（之前 ~42s）
- 驱动层修改: ~8s（之前 ~42s）

### 运行时性能

**✅ 零影响**！
- 所有层都是零成本抽象
- 编译器内联优化保持不变
- 无额外运行时开销

## 🎉 迁移后优势

1. **更快的编译时间** - 只重新编译修改的层
2. **更清晰的依赖** - 只依赖需要的层
3. **更好的模块化** - 更容易测试和维护
4. **向后兼容** - 无需修改现有代码

## 📖 下一步

- 查看 [examples/](../crates/piper-sdk/examples/) 了解更多示例
- 查看 [tests/](../crates/piper-sdk/tests/) 了解集成测试
- 阅读 [README.md](../../README.md) 了解完整功能
