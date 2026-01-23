//! 客户端接口
//!
//! 实现 Commander/Observer 读写分离模式。
//!
//! # 架构设计
//!
//! - `state_tracker`: 无锁状态跟踪（原子操作）
//! - `raw_commander`: 内部命令发送器（pub(crate)）
//! - `motion_commander`: 公开的运动命令接口
//! - `observer`: 状态观察器
//!
//! # 文档引用
//!
//! 详见 `docs/v0/high-level-api/rust_high_level_api_design_v3.2_final.md`

pub mod heartbeat;
pub mod motion_commander;
pub mod observer;
pub(crate) mod raw_commander;
pub mod state_monitor;
pub mod state_tracker;

pub use motion_commander::MotionCommander;
pub use observer::Observer;
pub use state_tracker::StateTracker;
