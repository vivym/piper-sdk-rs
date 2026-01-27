//! 驱动层模块
//!
//! 本模块提供 Piper 机械臂的设备驱动功能，包括：
//! - IO 线程管理（单线程/双线程模式）
//! - 状态同步（ArcSwap 无锁读取）
//! - 帧解析与聚合
//! - 命令优先级管理
//! - 钩子系统（v1.2.1）：异步录制、自定义回调
//!
//! # 使用场景
//!
//! 适用于需要直接控制 CAN 帧、需要高性能状态读取的场景。
//! 大多数用户应该使用 [`client`](crate::client) 模块提供的更高级接口。

mod builder;
pub mod command;
mod error;
mod fps_stats;
pub mod heartbeat;
pub mod hooks;
pub mod metrics;
pub mod mode;
pub mod pipeline;
mod piper; // 原 robot_impl.rs
pub mod recording;
pub mod state;

pub use builder::{DriverType, PiperBuilder};
pub use command::{CommandPriority, PiperCommand};
pub use error::DriverError; // 原 DriverError
pub use fps_stats::{FpsCounts, FpsResult};
pub use heartbeat::ConnectionMonitor;
pub use hooks::{FrameCallback, HookManager};
pub use metrics::{MetricsSnapshot, PiperMetrics};
pub use mode::{AtomicDriverMode, DriverMode};
pub use pipeline::{PipelineConfig, io_loop, rx_loop, tx_loop, tx_loop_mailbox};
pub use piper::Piper;
pub use recording::{AsyncRecordingHook, TimestampedFrame};
pub use state::*;
