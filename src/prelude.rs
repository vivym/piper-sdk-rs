//! Prelude - 常用类型的便捷导入
//!
//! 大多数用户应该使用这个模块来导入常用类型：
//!
//! ```rust
//! use piper_sdk::prelude::*;
//! ```

// 客户端层（推荐使用）
pub use crate::client::Piper;
pub use crate::client::{MotionCommander, Observer, PiperBuilder}; // Client 层 Builder（推荐使用）
// 类型系统（通过 types 模块导出）
pub use crate::client::types::*;

// CAN 层（常用 Trait）
pub use crate::can::CanAdapter;

// 驱动层（高级用户使用）
// 注意：不导出 driver::PiperBuilder，避免与 client::PiperBuilder 命名冲突
// 高级用户可以通过 driver::PiperBuilder 访问
pub use crate::driver::Piper as Driver;

// 错误类型
pub use crate::can::CanError;
pub use crate::driver::DriverError;
pub use crate::protocol::ProtocolError;
