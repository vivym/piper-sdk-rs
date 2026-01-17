//! Piper SDK - 松灵机械臂 Rust SDK
//!
//! 高性能、跨平台、零抽象开销的 Rust SDK，专用于松灵 Piper 机械臂的高频力控（>1kHz）。

pub mod can;
pub mod protocol;
pub mod robot;

// Re-export 核心类型（简化用户导入）
pub use can::{CanAdapter, CanError, PiperFrame};
pub use protocol::ProtocolError;
pub use robot::{Piper, PiperBuilder, RobotError};
