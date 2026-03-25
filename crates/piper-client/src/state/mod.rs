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
//! # use piper_client::{
//! #     MotionConnectedPiper, MotionConnectedState, PiperBuilder, state::MitModeConfig, types::*,
//! # };
//! # fn example() -> Result<(), Box<dyn std::error::Error>> {
//! let connected = PiperBuilder::new().socketcan("can0").build()?.require_motion()?;
//! let standby = match connected {
//!     MotionConnectedPiper::Strict(MotionConnectedState::Standby(robot)) => robot,
//!     MotionConnectedPiper::Soft(MotionConnectedState::Standby(robot)) => robot,
//!     MotionConnectedPiper::Strict(MotionConnectedState::Maintenance(_))
//!     | MotionConnectedPiper::Soft(MotionConnectedState::Maintenance(_)) => {
//!         return Err("robot is not in confirmed Standby".into());
//!     }
//! };
//!
//! // 使能 MIT 模式
//! let robot = standby.enable_mit_mode(MitModeConfig::default())?;
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
//! let _robot = robot.disable()?;
//! # Ok(())
//! # }
//! ```

pub mod capability;
pub mod machine;

pub use capability::{
    CapabilityMarker, MonitorOnly, MotionCapability, SoftRealtime, StrictCapability,
    StrictRealtime, UnspecifiedCapability,
};
pub use machine::{
    Active,
    ConnectedPiper,
    // 配置类型
    ConnectionConfig,
    DisableConfig,
    // 状态类型
    Disconnected,
    ErrorState,
    Maintenance,
    // 控制模式
    MitMode,
    MitModeConfig,
    MitPassthroughMode,
    MotionConnectedPiper,
    MotionConnectedState,
    Piper,
    PositionMode,
    PositionModeConfig,
    ReplayMode,
    Standby,
};
