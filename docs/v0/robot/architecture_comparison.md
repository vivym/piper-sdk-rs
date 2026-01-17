# 目录结构对比分析

## 设计文档规划 vs 实际实现

### 设计文档规划（TDD.md）

```
src/
├── lib.rs              # 库入口
├── error.rs            # 顶层：全局 PiperError
├── builder.rs          # 顶层：PiperBuilder
├── can/
│   ├── mod.rs
│   ├── socket.rs       # [Linux] SocketCAN
│   └── gs_usb/
│       ├── mod.rs
│       ├── protocol.rs
│       └── device.rs
├── protocol/
│   ├── mod.rs
│   ├── ids.rs
│   ├── feedback.rs
│   └── control.rs
└── driver/
    ├── mod.rs
    ├── robot.rs        # Piper 对象
    └── pipeline.rs     # IO Loop
```

### 实际实现

```
src/
├── lib.rs              # 库入口
├── can/
│   ├── mod.rs          # CanError, CanAdapter trait, PiperFrame
│   └── gs_usb/
│       ├── mod.rs
│       ├── protocol.rs
│       ├── device.rs
│       ├── frame.rs    # GS-USB 帧结构（新增）
│       └── error.rs    # GsUsbError（新增）
├── protocol/
│   ├── mod.rs          # ProtocolError
│   ├── ids.rs
│   ├── feedback.rs
│   ├── control.rs
│   └── config.rs       # 配置帧（新增）
└── driver/
    ├── mod.rs
    ├── robot.rs        # Piper 对象
    ├── pipeline.rs     # IO Loop
    ├── state.rs        # 状态结构定义（新增）
    ├── error.rs        # DriverError（新增）
    └── builder.rs      # PiperBuilder（新增）
```

## 关键差异分析

### 1. Builder 位置

| 方面 | 设计文档 | 实际实现 | 评价 |
|------|---------|---------|------|
| 位置 | `src/builder.rs` (顶层) | `src/driver/builder.rs` | ✅ **实际更好** |

**理由：**
- `PiperBuilder` 是创建 `Piper` 实例的工具，属于 driver 模块的职责
- 与 `Piper` 在同一个模块，内聚性更高
- 模块化更清晰：`driver::PiperBuilder` vs `piper_sdk::PiperBuilder`

### 2. Error 处理架构

| 方面 | 设计文档 | 实际实现 | 评价 |
|------|---------|---------|------|
| 顶层错误 | `src/error.rs` (PiperError) | ❌ 无顶层统一错误 | ⚠️ **有差异** |
| CAN 层错误 | `can::DriverError` | `can::CanError` | ✅ **实际更好** |
| Driver 层错误 | 顶层 PiperError 包含 | `driver::DriverError` | ✅ **实际更好** |
| 协议错误 | 顶层 PiperError 包含 | `protocol::ProtocolError` | ✅ **实际更好** |

**理由：**
- **分层清晰**：每层有自己的错误类型
  - `CanError`: CAN 适配层错误（USB、SocketCAN 底层错误）
  - `DriverError`: Driver 模块错误（通道、状态、超时等）
  - `ProtocolError`: 协议解析错误（帧格式、校验等）
- **关注点分离**：避免顶层错误枚举过于庞大
- **灵活性**：各层可以独立扩展错误类型

**建议改进：**
- 可以在顶层 `lib.rs` 添加 `pub use driver::DriverError as PiperError;` 作为别名，保持向后兼容

### 3. State 模块化

| 方面 | 设计文档 | 实际实现 | 评价 |
|------|---------|---------|------|
| 状态定义 | 可能在 `robot.rs` 或 `pipeline.rs` | `driver/state.rs` (独立模块) | ✅ **实际更好** |

**理由：**
- `state.rs` 独立成模块，职责清晰
- 状态结构定义复杂（5 个状态结构 + PiperContext + 组合状态）
- 便于测试和维护
- 符合单一职责原则

### 4. CAN 层细节

| 方面 | 设计文档 | 实际实现 | 评价 |
|------|---------|---------|------|
| GS-USB 帧 | 未提及 | `gs_usb/frame.rs` | ✅ **实际更好** |
| GS-USB 错误 | 未提及 | `gs_usb/error.rs` | ✅ **实际更好** |

**理由：**
- `frame.rs`: GS-USB 协议特有的帧结构，与通用 `PiperFrame` 分离
- `error.rs`: GS-USB 特有的错误类型，更精确的错误处理

### 5. Protocol 层扩展

| 方面 | 设计文档 | 实际实现 | 评价 |
|------|---------|---------|------|
| 配置帧 | 未提及 | `protocol/config.rs` | ✅ **实际更好** |

**理由：**
- 配置帧数量多，独立成模块避免 `control.rs` 过于庞大
- 职责清晰：feedback/control/config 三大类协议

## 对比总结

### 优势：实际实现更优

1. ✅ **模块化更好**：Builder、Error、State 按职责划分到对应模块
2. ✅ **分层清晰**：错误类型分层（Can/Driver/Protocol），符合 Rust 最佳实践
3. ✅ **可维护性高**：各模块职责单一，便于测试和维护
4. ✅ **扩展性好**：每层可以独立扩展，不影响其他层

### 可以改进的地方

1. ⚠️ **顶层统一错误**：设计文档建议的顶层 `PiperError` 可以作为类型别名
   ```rust
   // src/lib.rs
   pub use driver::DriverError as PiperError; // 向后兼容别名
   ```

2. ⚠️ **Builder 导出**：当前用户需要 `use piper_sdk::driver::PiperBuilder`，可以改为顶层导出
   ```rust
   // src/lib.rs
   pub use driver::PiperBuilder;
   ```

3. ℹ️ **SocketCAN**：设计文档规划的 `can/socket.rs` 尚未实现（Linux 支持待完善）

## 推荐方案

**保持当前结构，但优化顶层导出：**

```rust
// src/lib.rs
pub mod can;
pub mod driver;
pub mod protocol;

// Re-export 核心类型（简化用户导入）
pub use can::{CanAdapter, CanError, PiperFrame};
pub use driver::{DriverError, PiperBuilder, Piper}; // 顶层导出
pub use protocol::ProtocolError; // 如果需要

// 可选：向后兼容的顶层错误别名
#[deprecated(note = "Use driver::DriverError instead")]
pub type PiperError = DriverError;
```

**用户使用方式：**
```rust
// 当前方式（需要知道模块结构）
use piper_sdk::driver::{Piper, PiperBuilder, DriverError};

// 推荐方式（顶层导出）
use piper_sdk::{Piper, PiperBuilder, DriverError};
```

## 结论

**当前实现的结构优于设计文档的规划**，主要体现在：

1. 更好的模块化和关注点分离
2. 清晰的错误分层（Can/Driver/Protocol）
3. 独立的状态模块便于维护
4. 符合 Rust 模块组织的最佳实践

**建议**：
- ✅ 保持当前结构
- ✅ 在 `lib.rs` 中添加顶层 re-export，简化用户导入
- ✅ 可选：添加 `PiperError` 作为 `DriverError` 的类型别名（向后兼容）

