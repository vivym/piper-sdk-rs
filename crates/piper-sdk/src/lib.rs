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
    #[cfg(all(
        target_os = "linux",
        any(
            feature = "socketcan",
            feature = "auto-backend",
            feature = "target-socketcan"
        )
    ))]
    pub use piper_can::SocketCanAdapter;
    #[cfg(any(feature = "gs_usb", feature = "auto-backend"))]
    pub use piper_can::gs_usb::GsUsbCanAdapter;
    pub use piper_can::{
        BridgeTxAdapter, CanAdapter, CanData, CanDeviceError, CanDeviceErrorKind, CanError, CanId,
        ExtendedCanId, FrameError, PiperFrame, RealtimeTxAdapter, ReceivedFrame, RxAdapter,
        SplittableAdapter, StandardCanId, TimestampProvenance,
    };
}

pub mod protocol {
    pub use piper_protocol::*;
}

pub mod ids {
    pub use piper_protocol::ids::*;
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
pub use can::{CanAdapter, CanError};
pub use piper_can::{ReceivedFrame, TimestampProvenance};
pub use piper_protocol::{
    CanData, CanId, ExtendedCanId, FrameError, JointIndex, PiperFrame, StandardCanId,
};

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
    MotionConnectedState,
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

use std::sync::{Mutex, OnceLock};

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
/// - 如果当前线程已有活跃 `tracing` subscriber，或宿主已安装 `log` logger，会静默跳过
/// - 默认级别：`INFO`
/// - 格式：compact，隐藏 target（易读）
/// - 如果设置了 `RUST_LOG`，仅在 SDK 实际接管日志初始化时生效
/// - `RUST_LOG` 分支会把 `log` 全局门限收窄到 `EnvFilter` 的最大启用级别
/// - 如果 `EnvFilter` 无法给出静态级别提示，会保守回退到 `TRACE`
/// - 历史上曾设置过但已 drop 的 scoped subscriber 不会阻止后续初始化
/// - SDK 会串行化自身的初始化调用
/// - 如果 bridge 已安装但输掉 tracing 全局竞态，会立即关闭 `log` fast path，避免残留 bridge-only 状态
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
///     // 连接机器人（顶层 builder 返回 ConnectedPiper facade）
///     let robot = PiperBuilder::new().socketcan("can0").build()?;
///     tracing::info!("Connected via {:?}", robot.backend_capability());
///
///     // 现在可以使用 tracing::info!, tracing::warn! 等宏
///     Ok(())
/// }
/// ```
static LOGGER_INIT_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

enum LoggerInitConfig {
    EnvFilter {
        env_filter: Box<::tracing_subscriber::EnvFilter>,
        bridge_max_level: ::log::LevelFilter,
    },
    Default,
}

impl LoggerInitConfig {
    fn detect() -> Self {
        if ::std::env::var_os("RUST_LOG").is_some() {
            let env_filter = ::tracing_subscriber::EnvFilter::from_default_env();
            let bridge_max_level = env_filter
                .max_level_hint()
                .map(|hint| ::tracing_log::AsLog::as_log(&hint))
                .unwrap_or(::log::LevelFilter::Trace);

            Self::EnvFilter {
                env_filter: Box::new(env_filter),
                bridge_max_level,
            }
        } else {
            Self::Default
        }
    }

    fn bridge_max_level(&self) -> ::log::LevelFilter {
        match self {
            Self::EnvFilter {
                bridge_max_level, ..
            } => *bridge_max_level,
            Self::Default => ::log::LevelFilter::Info,
        }
    }

    fn into_dispatch(self) -> ::tracing::Dispatch {
        match self {
            Self::EnvFilter {
                env_filter,
                bridge_max_level: _,
            } => ::tracing::Dispatch::new(
                ::tracing_subscriber::fmt()
                    .with_env_filter(*env_filter)
                    .with_target(false)
                    .compact()
                    .finish(),
            ),
            Self::Default => ::tracing::Dispatch::new(
                ::tracing_subscriber::fmt()
                    .with_max_level(::tracing::Level::INFO)
                    .with_target(false)
                    .compact()
                    .finish(),
            ),
        }
    }
}

fn logger_init_lock() -> &'static Mutex<()> {
    LOGGER_INIT_LOCK.get_or_init(|| Mutex::new(()))
}

fn has_active_tracing_subscriber() -> bool {
    ::tracing::dispatcher::get_default(|dispatch| {
        !dispatch.is::<::tracing::subscriber::NoSubscriber>()
    })
}

fn host_owns_log_path() -> bool {
    ::log::max_level() != ::log::LevelFilter::Off
}

fn init_logger_inner(after_bridge: impl FnOnce()) {
    // 只串行化 SDK 自己的初始化路径；外部宿主的竞态仍可能发生，
    // 所以在输掉 tracing 全局安装时要主动关闭 log fast path。
    let _guard = logger_init_lock().lock().unwrap_or_else(|poisoned| poisoned.into_inner());

    if has_active_tracing_subscriber() || host_owns_log_path() {
        return;
    }

    let config = LoggerInitConfig::detect();
    if ::tracing_log::LogTracer::builder()
        .with_max_level(config.bridge_max_level())
        .init()
        .is_err()
    {
        return;
    }

    after_bridge();

    if ::tracing::dispatcher::set_global_default(config.into_dispatch()).is_err() {
        ::log::set_max_level(::log::LevelFilter::Off);
    }
}

#[doc(hidden)]
pub fn __init_logger() {
    init_logger_inner(|| {});
}

#[macro_export]
macro_rules! init_logger {
    () => {
        $crate::__init_logger();
    };
}

#[cfg(test)]
mod tests {
    #[test]
    fn init_logger_disables_bridge_if_subscriber_race_is_lost() {
        super::init_logger_inner(|| {
            ::tracing::subscriber::set_global_default(::tracing_subscriber::registry())
                .expect("test hook should win the tracing global-init race");
        });

        assert!(
            ::tracing::dispatcher::get_default(|dispatch| {
                dispatch.is::<::tracing_subscriber::registry::Registry>()
            }),
            "external subscriber should remain the active global dispatcher after the race",
        );
        assert_eq!(
            ::log::max_level(),
            ::log::LevelFilter::Off,
            "losing the subscriber race should disable the SDK log fast path",
        );
    }
}
