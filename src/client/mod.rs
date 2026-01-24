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

pub mod builder; // Client 层 Builder
pub mod control;
pub mod heartbeat;
pub mod motion; // 原 motion_commander.rs
pub mod observer;
pub(crate) mod raw_commander;
pub mod state;
pub mod types;

// 重新导出常用类型
pub use builder::PiperBuilder;
pub use motion::MotionCommander;
pub use observer::Observer;
pub use state::Piper; // Type State Pattern 的状态机
pub use types::*;
