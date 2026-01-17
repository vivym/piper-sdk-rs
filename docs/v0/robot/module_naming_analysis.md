# `driver` 模块命名分析

## 当前模块职责

根据 `src/driver/mod.rs` 的注释，`driver` 模块负责：

1. **IO 线程管理**：后台线程处理 CAN 通讯，避免阻塞控制循环
2. **状态同步**：使用 ArcSwap 实现无锁状态共享，支持 500Hz 高频读取
3. **帧解析与聚合**：将多个 CAN 帧聚合为完整的状态快照（Frame Commit + Buffered Commit 机制）
4. **时间戳管理**：按时间同步性拆分状态，解决不同 CAN 帧时间戳不同步的问题
5. **对外 API**：提供简洁的 `Piper` 结构体，封装底层细节

## 命名问题分析

### 1. 语义混淆

**问题：** "driver" 在系统编程中通常指**底层设备驱动**（如 USB driver、CAN driver）

**当前架构：**
```
src/
├── can/          # 真正的底层 CAN 驱动（CanAdapter trait）
│   └── gs_usb/   # GS-USB 设备驱动实现
└── driver/        # 业务逻辑层（高级 API）
```

**混淆点：**
- `can` 模块是真正的"驱动"（底层硬件抽象）
- `driver` 模块是"业务逻辑层"（高级 API）
- 用户可能认为 `driver` 是底层驱动，而 `can` 是协议层

### 2. 不够直观

**问题：** 用户看到 `driver` 模块名，可能不知道这是：
- 机械臂控制 API
- 状态管理
- 高级封装层

**用户期望：**
- 看到模块名就能理解其用途
- 例如：`robot`、`control`、`api` 更直观

### 3. 与其他模块的层次关系不清晰

**当前命名：**
```
can/      → 底层驱动
protocol/ → 协议定义
driver/   → 业务逻辑（高级 API）
```

**问题：** `driver` 这个名字暗示它是底层驱动，但实际上它是最高层的业务逻辑

## 替代方案分析

### 方案 1: `robot` ⭐⭐⭐⭐⭐

**优点：**
- ✅ **最直观**：用户一看就知道是机械臂控制模块
- ✅ **语义清晰**：`piper_sdk::robot::Piper` 比 `piper_sdk::driver::Piper` 更直观
- ✅ **符合直觉**：用户控制的是"robot"，不是"driver"
- ✅ **避免混淆**：不会与底层驱动混淆

**缺点：**
- ⚠️ 模块内已有 `robot.rs` 文件，重命名后需要调整

**示例：**
```rust
use piper_sdk::robot::Piper;  // 清晰直观
use piper_sdk::robot::PiperBuilder;
```

### 方案 2: `control` ⭐⭐⭐⭐

**优点：**
- ✅ **语义准确**：表示控制层
- ✅ **层次清晰**：`can`（驱动）→ `protocol`（协议）→ `control`（控制）
- ✅ **符合领域术语**：机器人控制领域常用词

**缺点：**
- ⚠️ 可能与其他"控制"概念混淆（如控制协议）

**示例：**
```rust
use piper_sdk::control::Piper;
```

### 方案 3: `api` ⭐⭐⭐

**优点：**
- ✅ **明确表示**：这是对外 API 层
- ✅ **层次清晰**：底层（can）→ 协议（protocol）→ API（api）

**缺点：**
- ⚠️ 过于通用，不够具体
- ⚠️ 在 Rust 中，`api` 通常指整个 crate 的公共接口

**示例：**
```rust
use piper_sdk::api::Piper;
```

### 方案 4: `core` ⭐⭐

**优点：**
- ✅ **表示核心**：核心业务逻辑

**缺点：**
- ⚠️ 过于通用
- ⚠️ 在 Rust 生态中，`core` 通常指标准库的 `core` crate

**示例：**
```rust
use piper_sdk::core::Piper;
```

### 方案 5: 保持 `driver` ⭐⭐

**优点：**
- ✅ **无需重构**：保持现状
- ✅ **已有代码**：所有测试和文档都已使用

**缺点：**
- ❌ **语义混淆**：与底层驱动混淆
- ❌ **不够直观**：用户需要查看文档才知道用途

## 推荐方案

### 🏆 首选：`robot`

**理由：**
1. **最直观**：用户控制的是"robot"，不是"driver"
2. **语义清晰**：`piper_sdk::robot::Piper` 比 `piper_sdk::driver::Piper` 更符合直觉
3. **避免混淆**：不会与底层 `can` 驱动混淆
4. **符合领域术语**：机器人 SDK 常用命名

**重构影响：**
- 模块重命名：`src/driver/` → `src/robot/`
- 文件重命名：`src/driver/robot.rs` → `src/robot/piper.rs`（或保持 `robot.rs`）
- 导入路径更新：所有 `use piper_sdk::driver::*` → `use piper_sdk::robot::*`
- 文档更新：所有文档中的 `driver` 引用

### 🥈 备选：`control`

如果 `robot` 被认为过于具体，`control` 是很好的备选：
- 语义准确（控制层）
- 层次清晰
- 符合领域术语

## 其他 Rust SDK 的命名参考

### ROS2 Rust 客户端
- `rclrs` - 使用 `rcl`（ROS Client Library）作为核心模块名
- 高级 API 通常直接暴露在顶层，如 `rclrs::Node`

### 机器人 SDK 常见命名
- `robot` - 最常见（如 `robot_rs`、`robot_sdk`）
- `control` - 控制层（如 `control_loop`）
- `api` - API 层（较少使用，因为整个 crate 就是 API）

### 嵌入式 Rust
- `driver` - 通常指底层硬件驱动（如 `stm32f4xx-hal::gpio::driver`）
- `hal` - Hardware Abstraction Layer（硬件抽象层）

## 结论

**推荐将 `driver` 重命名为 `robot`**，原因：

1. ✅ **最直观**：用户一看就知道是机械臂控制
2. ✅ **避免混淆**：不会与底层 `can` 驱动混淆
3. ✅ **符合直觉**：`piper_sdk::robot::Piper` 比 `piper_sdk::driver::Piper` 更自然
4. ✅ **符合惯例**：机器人 SDK 常用命名

**如果选择保持现状**，建议：
- 在文档中明确说明 `driver` 是业务逻辑层，不是底层驱动
- 在 `driver/mod.rs` 的注释中强调这一点

