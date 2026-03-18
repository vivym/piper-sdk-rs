//! State Machine - Type State Pattern 实现
//!
//! 使用 Rust 的类型系统在编译期强制执行正确的状态转换。
//!
//! # 设计目标
//!
//! - **编译期安全**: 非法状态转换无法编译
//! - **零开销**: 状态标记是零大小类型（ZST）
//! - **RAII**: Drop 自动失能，返回安全状态
//! - **可读性**: 状态转换在类型签名中明确
//!
//! # 状态机
//!
//! ```text
//! Disconnected
//!     ↓ connect()
//! Standby
//!     ↓ enable_mit_mode() / enable_position_mode()
//! Active<MitMode> / Active<PositionMode>
//!     ↓ disable()
//! Standby
//!     ↓ Drop
//! (自动失能)
//! ```
//!
//! # 使用示例
//!
//! ```rust,ignore
//! # use piper_client::{PiperBuilder, state::MitModeConfig, types::*};
//! # fn example() -> Result<()> {
//! // 连接
//! let robot = PiperBuilder::new().socketcan("can0").build()?; // Piper<Standby>
//!
//! // 使能 MIT 模式
//! let robot = robot.enable_mit_mode(MitModeConfig::default())?; // Piper<Active<MitMode>>
//!
//! // 发送命令
//! let positions = JointArray::splat(Rad(1.0));
//! let velocities = JointArray::splat(RadPerSecond(0.0));
//! let kp = JointArray::splat(10.0);
//! let kd = JointArray::splat(2.0);
//! let torques = JointArray::splat(NewtonMeter(5.0));
//! robot.command_torques(&positions, &velocities, &kp, &kd, &torques)?;
//!
//! // 失能
//! let _robot = robot.disable()?; // Piper<Standby>
//!
//! // Drop 自动失能
//! # Ok(())
//! # }
//! ```

pub mod machine;

pub use machine::{
    Active,
    // 配置类型
    ConnectionConfig,
    DisableConfig,
    // 状态类型
    Disconnected,
    // 控制模式
    MitMode,
    MitModeConfig,
    Piper,
    PositionMode,
    PositionModeConfig,
    ReplayMode,
    Standby,
};
