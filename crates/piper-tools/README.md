# Piper Tools

共享数据结构和工具库，用于 Piper SDK 应用程序。

## 概述

`piper-tools` 提供了 Piper 应用程序之间共享的数据结构和算法。它只依赖 `piper-protocol`，避免了对 `piper-client` 的依赖，从而保持轻量和快速编译。

## 模块

- **`recording`** - 统一的 CAN 帧录制格式
- **`timestamp`** - 时间戳处理和来源检测
- **`safety`** - 安全配置和限制
- **`statistics`** - 统计工具（可选）

## Feature Flags

### 默认（无 features）
```toml
[dependencies]
piper-tools = { workspace = true }
```
只包含核心功能（recording, timestamp, safety），不包含统计模块。

### 完整功能
```toml
[dependencies]
piper-tools = { workspace = true, features = ["full"] }
```
包含所有功能，包括统计模块。

### 仅统计模块
```toml
[dependencies]
piper-tools = { workspace = true, features = ["statistics"] }
```
显式启用统计模块。

## 使用示例

### 录制 CAN 帧

```rust
use piper_tools::{PiperRecording, RecordingMetadata, TimestampedFrame, TimestampSource};

// 创建新的录制
let metadata = RecordingMetadata::new("can0".to_string(), 1_000_000);
let mut recording = PiperRecording::new(metadata);

// 添加帧
recording.add_frame(TimestampedFrame::new(
    1234567890,  // 时间戳（微秒）
    0x100,       // CAN ID
    vec![1, 2, 3, 4],  // 数据
    TimestampSource::Hardware,
));

// 保存或分析
println!("Frame count: {}", recording.frame_count());
```

### 安全配置

```rust
use piper_tools::{SafetyConfig, SafetyLimits};

// 创建默认配置
let config = SafetyConfig::default_config();

// 检查限制
if config.check_velocity(2.5) {
    println!("速度在限制内");
}

// 检查是否需要确认
if config.requires_confirmation(15.0) {
    println!("大幅度移动，需要用户确认");
}
```

### 时间戳处理

```rust
use piper_tools::{detect_timestamp_source, TimestampSource};

// 检测时间戳来源
let source = detect_timestamp_source();
println!("时间戳来源: {:?}", source);
println!("精度: {}μs", source.precision_us());
```

## 依赖原则

**只依赖 `piper-protocol`**，避免依赖 `piper-client` 和 `piper-driver`。

依赖层级：
```
apps/cli → piper-client → piper-protocol
tools/ → piper-protocol ✅ (不依赖 client)
```

## 编译时间对比

| Feature | 编译时间 | 说明 |
|---------|----------|------|
| default | ~5s | 无统计模块 |
| statistics | ~12s | 包含 statrs 及其依赖 |

## 许可证

MIT OR Apache-2.0
