//! GS-USB 守护进程
//!
//! 用户态守护进程，始终保持与 GS-USB 设备的连接，通过 UDS/TCP 向客户端提供
//! 非实时 bridge/debug CAN 总线访问。
//!
//! 参考：`daemon_implementation_plan.md`

pub mod macos_qos;
pub mod singleton;
pub mod session_manager;
pub mod daemon;

pub use daemon::{Daemon, DaemonConfig, DaemonError, DeviceState};
