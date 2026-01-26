//! 控制器模块
//!
//! 提供高级控制接口，包括：
//! - `Controller` trait - 控制器通用接口
//! - `PidController` - PID 位置控制器
//! - `MitController` - MIT 模式高层控制器（循环锚点机制）
//! - `ZeroingConfirmToken` - 关节归零确认令牌
//! - `TrajectoryPlanner` - 轨迹规划器
//! - Loop Runner - 控制循环包装器

pub mod controller;
pub mod loop_runner;
pub mod mit_controller;
pub mod pid;
pub mod trajectory;
pub mod zeroing_token;

// 重新导出常用类型
pub use controller::Controller;
pub use loop_runner::{LoopConfig, run_controller};
pub use mit_controller::{ControlError, MitController, MitControllerConfig};
pub use pid::PidController;
pub use trajectory::TrajectoryPlanner;
pub use zeroing_token::{ZeroingConfirmToken, ZeroingTokenError};
