//! Piper SDK - 松灵机械臂 Rust SDK
//!
//! 高性能、跨平台、零抽象开销的 Rust SDK，专用于松灵 Piper 机械臂的高频力控（>1kHz）。

pub mod can;

// Re-export 核心类型
pub use can::{CanAdapter, CanError, PiperFrame};
