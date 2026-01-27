//! 客户端接口模块
//!
//! 本模块提供 Piper 机械臂的用户友好接口，包括：
//! - Type State Pattern（编译期状态安全）
//! - Commander/Observer 模式（读写分离）
//! - 强类型单位（Rad、Deg、NewtonMeter）
//! - 轨迹规划和控制
//!
//! # 使用场景
//!
//! 这是大多数用户应该使用的模块，提供了类型安全、易于使用的 API。
//! 如果需要直接控制 CAN 帧或需要更高性能，可以使用 [`driver`](crate::driver) 模块。
//!
//! # 高级诊断接口
//!
//! 对于需要底层访问的场景（如自定义录制、调试），参见 [`diagnostics`](self::diagnostics) 模块。
//!
//! # 标准录制 API
//!
//! 对于常规录制场景，参见 [`recording`](self::recording) 模块。

pub mod builder; // Client 层 Builder
pub mod control;
pub mod diagnostics;
pub mod heartbeat;
pub mod observer;
pub(crate) mod raw_commander;
pub mod recording;
pub mod state;
pub mod types;

// 测试模块
#[cfg(test)]
mod recording_tests;

// 重新导出常用类型
pub use builder::PiperBuilder;
pub use diagnostics::PiperDiagnostics;
pub use observer::Observer;
pub use recording::{
    RecordingConfig, RecordingHandle, RecordingMetadata, RecordingStats, StopCondition,
};
pub use state::Piper; // Type State Pattern 的状态机
pub use types::*;
