//! GS-USB 守护进程
//!
//! 用户态守护进程，始终保持与 GS-USB 设备的连接，通过 UDS/UDP 向客户端提供 CAN 总线访问
//!
//! 参考：`daemon_implementation_plan.md`

pub mod macos_qos;
pub mod singleton;
pub mod client_manager;
pub mod daemon;

pub use client_manager::{ClientAddr, ClientManager, Client, ClientError};
pub use daemon::{Daemon, DaemonConfig, DaemonError, DeviceState};

