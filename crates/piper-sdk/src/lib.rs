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
//! use piper_sdk::{Piper, Observer};
//! ```
//!
//! 需要直接控制 CAN 帧或需要更高性能的用户可以使用驱动层：
//!
//! ```rust
//! use piper_sdk::driver::{Piper as Driver, PiperBuilder};
//! ```

// 内部模块结构（重新导出各个层）
pub mod can {
    #[cfg(feature = "mock")]
    pub use piper_can::MockCanAdapter;
    pub use piper_can::{
        BridgeTxAdapter, CanAdapter, CanDeviceError, CanDeviceErrorKind, CanError, PiperFrame,
        RealtimeTxAdapter, RxAdapter, SplittableAdapter,
    };
}

pub mod protocol {
    pub use piper_protocol::*;
}

pub mod driver {
    pub use piper_driver::*;
}

pub mod client {
    pub use piper_client::*;
}

// Prelude 模块
pub mod prelude;

// --- 用户以此为界 ---

// CAN 层常用类型
pub use can::{CanAdapter, CanError, PiperFrame};

// 协议层错误
pub use protocol::ProtocolError;

// 驱动层（高级用户使用）- 通过模块路径访问，避免命名冲突
// 注意：不直接导出 driver::Piper，因为与 client::Piper 冲突
// 用户可以通过 driver::Piper 或类型别名访问
pub use driver::DriverError;

// 客户端层（普通用户使用）- 这是推荐的入口点
// 导出 client::Piper 为 Piper（这是大多数用户应该使用的）
pub use client::Piper; // Type State Pattern 的状态机
pub use client::{
    BilateralCommand,
    BilateralControlFrame,
    BilateralController,
    BilateralDynamicsCompensation,
    BilateralDynamicsCompensator,
    BilateralExitReason,
    BilateralLoopConfig,
    BilateralRunReport,
    BridgeClientOptions,
    BridgeDeviceState,
    BridgeEndpoint,
    BridgeError,
    BridgeEvent,
    BridgeHostConfig,
    BridgeMaintenanceState,
    BridgeResult,
    BridgeRole,
    BridgeStatus,
    BridgeTlsClientConfig,
    BridgeTlsClientPolicy,
    BridgeTlsServerConfig,
    BridgeUdsListenerConfig,
    CanIdFilter,
    ConnectedPiper,
    DualArmActiveMit,
    DualArmBuilder,
    DualArmCalibration,
    DualArmError,
    DualArmErrorState,
    DualArmHoldAnchor,
    DualArmLoopExit,
    DualArmObserver,
    DualArmReadPolicy,
    DualArmRuntimeHealth,
    DualArmSafetyConfig,
    DualArmSnapshot,
    GripperTeleopConfig,
    JointMirrorMap,
    JointSpaceBilateralController,
    LoopTimingMode,
    MaintenanceLease,
    MasterFollowerController,
    MonitorOnly,
    MonitorReadPolicy,
    MotionConnectedPiper,
    Observer,
    PiperBridgeClient,
    PiperBridgeHost,
    PiperBuilder, // Client 层 Builder（推荐使用）
    // 类型系统通过 types 模块导出
    RuntimeFaultKind,
    RuntimeHealthSnapshot,
    SessionToken,
    SoftRealtime,
    StopAttemptResult,
    StrictRealtime,
};

// 导出 recording 模块的常用类型
pub use client::recording::{
    RecordingConfig, RecordingHandle, RecordingMetadata, RecordingStats, StopCondition,
};

// 类型别名：为驱动层提供清晰的别名
pub type Driver = driver::Piper; // 高级用户可以使用这个别名

// ============================================================
// 日志初始化宏
// ============================================================

/// 初始化日志系统（便捷宏）
///
/// ## 功能
///
/// - 兼容 `log` crate（通过 `tracing_log::LogTracer` 桥接）
/// - 幂等：可安全重复调用
/// - 如果宿主程序已安装 `tracing` subscriber 或 `log` logger，会静默跳过
/// - 默认级别：`INFO`
/// - 格式：compact，隐藏 target（易读）
/// - 如果设置了 `RUST_LOG`，仅在 SDK 实际接管日志初始化时生效
///
/// ## 使用示例
///
/// ```rust,no_run
/// use piper_sdk::prelude::*;
///
/// fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
///     // 在 main 函数开头初始化日志
///     piper_sdk::init_logger!();
///
///     // 连接机器人
///     let driver = PiperBuilder::new().socketcan("can0").build()?;
///
///     // 现在可以使用 tracing::info!, tracing::warn! 等宏
///     tracing::info!("Connected to robot");
///
///     Ok(())
/// }
/// ```
#[doc(hidden)]
pub fn __init_logger() {
    // 保守策略：只有在 SDK 同时接管 tracing + log 全局状态时，才安装 stdout subscriber。
    // 如果宿主已拥有任一日志栈，直接 no-op，避免半初始化把同步 I/O 带回实时进程。
    if ::tracing::dispatcher::has_been_set() {
        return;
    }
    if ::log::max_level() != ::log::LevelFilter::Off {
        return;
    }

    let has_rust_log = ::std::env::var_os("RUST_LOG").is_some();
    let bridge_max_level = if has_rust_log {
        ::log::LevelFilter::Trace
    } else {
        ::log::LevelFilter::Info
    };

    if ::tracing_log::LogTracer::builder()
        .with_max_level(bridge_max_level)
        .init()
        .is_err()
    {
        return;
    }

    if has_rust_log {
        let subscriber = ::tracing_subscriber::fmt()
            .with_env_filter(::tracing_subscriber::EnvFilter::from_default_env())
            .with_target(false)
            .compact()
            .finish();
        let _ = ::tracing::subscriber::set_global_default(subscriber);
    } else {
        let subscriber = ::tracing_subscriber::fmt()
            .with_max_level(::tracing::Level::INFO)
            .with_target(false)
            .compact()
            .finish();
        let _ = ::tracing::subscriber::set_global_default(subscriber);
    }
}

#[macro_export]
macro_rules! init_logger {
    () => {
        $crate::__init_logger();
    };
}
