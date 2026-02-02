//! 基础类型系统
//!
//! 提供强类型单位、关节索引和错误类型。

pub mod cartesian;
pub mod error;
pub mod joint;
pub mod quirks;
pub mod units;

pub use cartesian::*;
pub use error::*;
pub use joint::*;
pub use quirks::*;
pub use units::*;
