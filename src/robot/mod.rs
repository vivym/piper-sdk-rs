//! Robot 模块
//!
//! Robot 模块是 SDK 的核心业务逻辑层，负责：
//! - IO 线程管理：后台线程处理 CAN 通讯，避免阻塞控制循环
//! - 状态同步：使用 ArcSwap 实现无锁状态共享，支持 500Hz 高频读取
//! - 帧解析与聚合：将多个 CAN 帧聚合为完整的状态快照（Frame Commit + Buffered Commit 机制）
//! - 时间戳管理：按时间同步性拆分状态，解决不同 CAN 帧时间戳不同步的问题
//! - 对外 API：提供简洁的 `Piper` 结构体，封装底层细节

mod builder;
mod error;
mod pipeline;
mod robot_impl;
mod state;

pub use builder::PiperBuilder;
pub use error::RobotError;
pub use pipeline::{PipelineConfig, io_loop};
pub use robot_impl::Piper;
pub use state::*;
