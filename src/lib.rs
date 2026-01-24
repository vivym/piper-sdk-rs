//! Piper SDK - 松灵机械臂 Rust SDK
//!
//! 高性能、跨平台、零抽象开销的 Rust SDK，专用于松灵 Piper 机械臂的高频力控（>1kHz）。
//!
//! # 架构设计
//!
//! 本 SDK 采用分层架构，从底层到高层：
//!
//! - **CAN 层** (`can`): CAN 硬件抽象，支持 SocketCAN 和 GS-USB
//! - **协议层** (`protocol`): 类型安全的协议编码/解码
//! - **驱动层** (`driver`): IO 线程管理、状态同步、帧解析
//! - **客户端层** (`client`): 类型安全、易用的控制接口
//!
//! # 快速开始
//!
//! 大多数用户应该使用高层 API（客户端接口）：
//!
//! ```rust
//! use piper_sdk::prelude::*;
//! // 或
//! use piper_sdk::{Piper, MotionCommander, Observer};
//! ```
//!
//! 需要直接控制 CAN 帧或需要更高性能的用户可以使用驱动层：
//!
//! ```rust
//! use piper_sdk::driver::{Piper as Driver, PiperBuilder};
//! ```

// 内部模块结构（按功能划分 - 方案 B）
pub mod can;
pub mod client;
pub mod driver;
pub mod protocol;

// Prelude 模块
pub mod prelude;

// --- 用户以此为界 ---
// 以下是通过 Facade Pattern 提供的公共 API

// CAN 层常用类型
pub use can::{CanAdapter, CanError, PiperFrame};

// 协议层错误
pub use protocol::ProtocolError;

// 驱动层（高级用户使用）- 通过模块路径访问，避免命名冲突
// 注意：不直接导出 driver::Piper，因为与 client::Piper 冲突
// 用户可以通过 driver::Piper 或类型别名访问
// 注意：RobotError 已重命名为 DriverError，以保持与模块命名一致
// 注意：不直接导出 driver::PiperBuilder，因为与 client::PiperBuilder 冲突
// 高级用户可以通过 driver::PiperBuilder 访问
pub use driver::DriverError;

// 客户端层（普通用户使用）- 这是推荐的入口点
// 导出 client::Piper 为 Piper（这是大多数用户应该使用的）
pub use client::Piper; // Type State Pattern 的状态机
pub use client::{
    MotionCommander,
    Observer,
    PiperBuilder, // Client 层 Builder（推荐使用）
                  // 类型系统通过 types 模块导出
};

// 类型别名：为驱动层提供清晰的别名
pub type Driver = driver::Piper; // 高级用户可以使用这个别名
