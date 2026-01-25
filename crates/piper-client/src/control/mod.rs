//! 控制器模块
//!
//! 提供高级控制接口，包括：
//! - `Controller` trait - 控制器通用接口
//! - `PidController` - PID 位置控制器
//! - `TrajectoryPlanner` - 轨迹规划器
//! - Loop Runner - 控制循环包装器

pub mod controller;
pub mod loop_runner;
pub mod pid;
pub mod trajectory;

// 重新导出常用类型
pub use controller::Controller;
pub use loop_runner::{LoopConfig, run_controller};
pub use pid::PidController;
pub use trajectory::TrajectoryPlanner;
