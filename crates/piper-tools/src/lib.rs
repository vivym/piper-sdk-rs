//! # Piper Tools - 共享数据结构和算法
//!
//! **依赖原则**: 只依赖 `piper-protocol`，避免依赖 `piper-client`
//!
//! ## 包含模块
//!
//! - `recording` - 录制格式定义（纯数据结构）
//! - `statistics` - 统计算法（纯函数，可选）
//! - `safety` - 安全配置（只读结构）
//! - `timestamp` - 时间戳处理（纯函数）
//!
//! ## Feature Flags
//!
//! - `default` - 无默认 features
//! - `full` - 启用所有功能（包含 statistics）
//! - `statistics` - 启用统计模块
//!
//! ## 使用示例
//!
//! ```toml
//! # apps/cli/Cargo.toml - 需要统计
//! [dependencies]
//! piper-tools = { workspace = true, features = ["full"] }
//!
//! # tools/can-sniffer/Cargo.toml - 不需要统计
//! [dependencies]
//! piper-tools = { workspace = true }
//! ```

// ⚠️ 禁止引入 piper-client
// use piper_client::*;  // ❌ 禁止

pub mod recording;
pub mod timestamp;

// ⭐ 可选模块（通过 feature flags 控制）
#[cfg(feature = "statistics")]
pub mod statistics;

pub mod safety;

// 重新导出常用类型
pub use recording::{PiperRecording, RecordingMetadata, TimestampedFrame};
pub use safety::{SafetyConfig, SafetyLimits};
pub use timestamp::{TimestampSource, detect_timestamp_source};
// extract_timestamp 已弃用，不导出（由 piper-can 层处理实际时间戳提取）
