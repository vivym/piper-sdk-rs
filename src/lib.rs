//! Piper SDK - 松灵机械臂 Rust SDK
//!
//! 高性能、跨平台、零抽象开销的 Rust SDK，专用于松灵 Piper 机械臂的高频力控（>1kHz）。
//!
//! # 架构层次
//!
//! - **低层 API** (`can`, `protocol`, `robot`): 直接硬件访问，零抽象开销
//! - **高层 API** (`high_level`): 类型安全、易用的控制接口 (v0.1.0+)

pub mod can;
pub mod protocol;
pub mod robot;

// 高层 API（工业级类型安全接口）
pub mod high_level;

// Re-export 核心类型（简化用户导入）
pub use can::{CanAdapter, CanError, PiperFrame};
pub use protocol::ProtocolError;
pub use robot::{Piper, PiperBuilder, RobotError};
