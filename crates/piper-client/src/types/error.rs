//! 错误类型体系
//!
//! 分层错误处理，区分致命错误（Fatal）和可恢复错误（Recoverable）。
//!
//! # 设计目标
//!
//! - **分层管理**: 区分致命错误和可恢复错误
//! - **清晰信息**: 提供详细的错误上下文
//! - **可重试**: 标记可重试的错误
//! - **日志友好**: 集成 tracing 日志
//!
//! # 示例
//!
//! ```rust
//! use piper_client::types::RobotError;
//!
//! fn handle_error(err: RobotError) {
//!     if err.is_fatal() {
//!         eprintln!("致命错误: {}", err);
//!         // 停止系统
//!     } else if err.is_retryable() {
//!         eprintln!("可重试错误: {}", err);
//!         // 重试操作
//!     } else {
//!         eprintln!("错误: {}", err);
//!         // 记录并继续
//!     }
//! }
//! ```

use super::joint::Joint;
use piper_driver::RuntimeFaultKind;
use piper_protocol::{MitControlField, ProtocolError};
use std::time::Duration;
use thiserror::Error;

/// 监控快照不完整的状态来源
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MonitorStateSource {
    /// 关节位置组（0x2A5-0x2A7）
    JointPosition,
    /// 末端位姿组（0x2A2-0x2A4）
    EndPose,
    /// 关节动态组（J1-J6 高速反馈）
    JointDynamic,
}

impl std::fmt::Display for MonitorStateSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::JointPosition => f.write_str("joint position"),
            Self::EndPose => f.write_str("end pose"),
            Self::JointDynamic => f.write_str("joint dynamic"),
        }
    }
}

/// 机器人错误类型
///
/// 分层错误类型，支持致命错误和可恢复错误的区分。
#[derive(Debug, Error)]
pub enum RobotError {
    // ==================== Fatal Errors (不可恢复) ====================
    /// 硬件通信失败
    #[error("Hardware communication failed: {0}")]
    HardwareFailure(String),

    /// 状态机损坏
    #[error("State machine poisoned: {reason}")]
    StatePoisoned {
        /// 损坏原因
        reason: String,
    },

    /// 急停触发
    #[error("Emergency stop triggered")]
    EmergencyStop,

    /// CAN 总线错误（致命）
    #[error("CAN bus fatal error: {0}")]
    CanBusFatal(String),

    // ==================== Recoverable Errors ====================
    /// 命令超时
    #[error("Command timeout after {timeout_ms}ms")]
    Timeout {
        /// 超时时间（毫秒）
        timeout_ms: u64,
    },

    /// 无效的状态转换
    #[error("Invalid state transition: {from} -> {to}")]
    InvalidTransition {
        /// 起始状态
        from: String,
        /// 目标状态
        to: String,
    },

    /// 控制闭环读取到的反馈已过期
    #[error("Control feedback is stale: age {age_ms}ms exceeds allowed {max_age_ms}ms")]
    FeedbackStale {
        /// 反馈年龄
        age: Duration,
        /// 允许的最大反馈年龄
        max_age: Duration,
        /// 便于日志直接打印的毫秒值
        age_ms: u128,
        /// 便于日志直接打印的毫秒值
        max_age_ms: u128,
    },

    /// 控制闭环读取到的位置/动态状态时间未对齐
    #[error("Control state misaligned: skew {skew_us}us exceeds allowed {max_skew_us}us")]
    StateMisaligned {
        /// 有符号时间偏差（dynamic - position）
        skew_us: i64,
        /// 允许的最大绝对偏差
        max_skew_us: u64,
    },

    /// 控制闭环读取到的不完整运动状态
    #[error(
        "Control state incomplete: position mask {position_frame_valid_mask:03b}, dynamic mask {dynamic_valid_mask:06b}"
    )]
    ControlStateIncomplete {
        /// 位置反馈帧组有效性掩码（0x2A5-0x2A7）
        position_frame_valid_mask: u8,
        /// 动态反馈组有效性掩码（J1-J6）
        dynamic_valid_mask: u8,
    },

    /// 监控/诊断读取到的不完整状态
    #[error(
        "Monitor state incomplete for {state_source}: valid mask {valid_mask:b}, required mask {required_mask:b}"
    )]
    MonitorStateIncomplete {
        /// 不完整状态的来源
        state_source: MonitorStateSource,
        /// 当前有效掩码
        valid_mask: u8,
        /// 所需完整掩码
        required_mask: u8,
    },

    /// 当前后端不支持主机侧实时闭环
    #[error("Realtime control unsupported on current backend: {reason}")]
    RealtimeUnsupported {
        /// 原因说明
        reason: String,
    },

    /// 运行时健康状态异常
    #[error("Runtime health unhealthy: rx_alive={rx_alive}, tx_alive={tx_alive}, fault={fault:?}")]
    RuntimeHealthUnhealthy {
        /// RX 线程是否存活
        rx_alive: bool,
        /// TX 线程是否存活
        tx_alive: bool,
        /// 最近一次运行时故障
        fault: Option<RuntimeFaultKind>,
    },

    /// 关节限位超出
    #[error("Joint {joint} limit exceeded: {value:.3} (limit: {limit:.3})")]
    JointLimitExceeded {
        /// 关节索引
        joint: Joint,
        /// 实际值
        value: f64,
        /// 限位值
        limit: f64,
    },

    /// 速度限制超出
    #[error("Velocity limit exceeded for joint {joint}: {value:.3} (limit: {limit:.3})")]
    VelocityLimitExceeded {
        /// 关节索引
        joint: Joint,
        /// 实际速度
        value: f64,
        /// 限速
        limit: f64,
    },

    /// MIT 位置参考超范围
    #[error(
        "MIT position reference out of range for joint {joint}: {value:.3} not in [{min:.3}, {max:.3}]"
    )]
    PositionReferenceOutOfRange {
        joint: Joint,
        value: f64,
        min: f64,
        max: f64,
    },

    /// MIT 速度参考超范围
    #[error(
        "MIT velocity reference out of range for joint {joint}: {value:.3} not in [{min:.3}, {max:.3}]"
    )]
    VelocityReferenceOutOfRange {
        joint: Joint,
        value: f64,
        min: f64,
        max: f64,
    },

    /// MIT Kp 超范围
    #[error("MIT Kp out of range for joint {joint}: {value:.3} not in [{min:.3}, {max:.3}]")]
    KpGainOutOfRange {
        joint: Joint,
        value: f64,
        min: f64,
        max: f64,
    },

    /// MIT Kd 超范围
    #[error("MIT Kd out of range for joint {joint}: {value:.3} not in [{min:.3}, {max:.3}]")]
    KdGainOutOfRange {
        joint: Joint,
        value: f64,
        min: f64,
        max: f64,
    },

    /// 力矩限制超出
    #[error("Torque limit exceeded for joint {joint}: {value:.3} not in [{min:.3}, {max:.3}]")]
    TorqueLimitExceeded {
        /// 关节索引
        joint: Joint,
        /// 实际力矩
        value: f64,
        /// 最小允许值
        min: f64,
        /// 最大允许值
        max: f64,
    },

    // ==================== I/O Errors ====================
    /// CAN 总线 I/O 错误（可恢复）
    #[error("CAN bus I/O error: {0}")]
    CanIoError(String),

    /// 序列化错误
    #[error("Serialization error: {0}")]
    SerializationError(String),

    // ==================== Protocol Errors ====================
    /// 协议错误
    #[error("Protocol encoding error: {0}")]
    Protocol(ProtocolError),

    /// 驱动层错误（自动转换自 driver::DriverError）
    #[error("Driver infrastructure error: {0}")]
    Infrastructure(#[from] piper_driver::DriverError),

    /// CAN 适配器错误（自动转换自 can::CanError）
    #[error("CAN adapter error: {0}")]
    CanAdapter(#[from] piper_can::CanError),

    /// 无效的帧ID
    #[error("Invalid CAN frame ID: 0x{id:03X}")]
    InvalidFrameId {
        /// 帧 ID
        id: u32,
    },

    /// 无效的数据长度
    #[error("Invalid data length: expected {expected}, got {actual}")]
    InvalidDataLength {
        /// 期望长度
        expected: usize,
        /// 实际长度
        actual: usize,
    },

    // ==================== Configuration Errors ====================
    /// 配置错误
    #[error("Configuration error: {0}")]
    ConfigError(String),

    /// 参数无效
    #[error("Invalid parameter '{param}': {reason}")]
    InvalidParameter {
        /// 参数名
        param: String,
        /// 原因
        reason: String,
    },

    // ==================== Other ====================
    /// 未知错误
    #[error("Unknown error: {0}")]
    Unknown(String),
}

impl RobotError {
    /// 是否为致命错误
    ///
    /// 致命错误表示系统处于不安全状态，必须立即停止。
    pub fn is_fatal(&self) -> bool {
        matches!(
            self,
            Self::HardwareFailure(_)
                | Self::StatePoisoned { .. }
                | Self::EmergencyStop
                | Self::CanBusFatal(_)
                | Self::RuntimeHealthUnhealthy { .. }
        )
    }

    /// 是否可重试
    ///
    /// 可重试错误表示重新执行操作可能会成功。
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            Self::Timeout { .. }
                | Self::CanIoError(_)
                | Self::Protocol(_)
                | Self::FeedbackStale { .. }
                | Self::StateMisaligned { .. }
                | Self::ControlStateIncomplete { .. }
                | Self::MonitorStateIncomplete { .. }
        )
    }

    /// 是否为配置错误
    pub fn is_config_error(&self) -> bool {
        matches!(
            self,
            Self::ConfigError(_) | Self::InvalidParameter { .. } | Self::RealtimeUnsupported { .. }
        )
    }

    /// 是否为限位错误
    pub fn is_limit_error(&self) -> bool {
        matches!(
            self,
            Self::JointLimitExceeded { .. }
                | Self::VelocityLimitExceeded { .. }
                | Self::PositionReferenceOutOfRange { .. }
                | Self::VelocityReferenceOutOfRange { .. }
                | Self::KpGainOutOfRange { .. }
                | Self::KdGainOutOfRange { .. }
                | Self::TorqueLimitExceeded { .. }
        )
    }

    /// 添加上下文信息
    pub fn context(self, context: impl Into<String>) -> Self {
        match self {
            Self::Unknown(msg) => Self::Unknown(format!("{}: {}", context.into(), msg)),
            _ => self,
        }
    }

    /// 创建硬件故障错误
    pub fn hardware_failure(msg: impl Into<String>) -> Self {
        Self::HardwareFailure(msg.into())
    }

    /// 创建状态损坏错误
    pub fn state_poisoned(reason: impl Into<String>) -> Self {
        Self::StatePoisoned {
            reason: reason.into(),
        }
    }

    /// 创建超时错误
    pub fn timeout(timeout_ms: u64) -> Self {
        Self::Timeout { timeout_ms }
    }

    /// 创建无效状态转换错误
    pub fn invalid_transition(from: impl Into<String>, to: impl Into<String>) -> Self {
        Self::InvalidTransition {
            from: from.into(),
            to: to.into(),
        }
    }

    /// 创建反馈过期错误
    pub fn feedback_stale(age: Duration, max_age: Duration) -> Self {
        Self::FeedbackStale {
            age,
            max_age,
            age_ms: age.as_millis(),
            max_age_ms: max_age.as_millis(),
        }
    }

    /// 创建控制状态未对齐错误
    pub fn state_misaligned(skew_us: i64, max_skew_us: u64) -> Self {
        Self::StateMisaligned {
            skew_us,
            max_skew_us,
        }
    }

    /// 创建控制状态不完整错误
    pub fn control_state_incomplete(position_frame_valid_mask: u8, dynamic_valid_mask: u8) -> Self {
        Self::ControlStateIncomplete {
            position_frame_valid_mask,
            dynamic_valid_mask,
        }
    }

    /// 创建监控状态不完整错误
    pub fn monitor_state_incomplete(
        state_source: MonitorStateSource,
        valid_mask: u8,
        required_mask: u8,
    ) -> Self {
        Self::MonitorStateIncomplete {
            state_source,
            valid_mask,
            required_mask,
        }
    }

    /// 创建实时闭环不支持错误
    pub fn realtime_unsupported(reason: impl Into<String>) -> Self {
        Self::RealtimeUnsupported {
            reason: reason.into(),
        }
    }

    /// 创建运行时健康异常错误
    pub fn runtime_health_unhealthy(
        rx_alive: bool,
        tx_alive: bool,
        fault: Option<RuntimeFaultKind>,
    ) -> Self {
        Self::RuntimeHealthUnhealthy {
            rx_alive,
            tx_alive,
            fault,
        }
    }

    /// 创建关节限位错误
    pub fn joint_limit(joint: Joint, value: f64, limit: f64) -> Self {
        Self::JointLimitExceeded {
            joint,
            value,
            limit,
        }
    }

    /// 创建速度限制错误
    pub fn velocity_limit(joint: Joint, value: f64, limit: f64) -> Self {
        Self::VelocityLimitExceeded {
            joint,
            value,
            limit,
        }
    }

    /// 创建力矩限制错误
    pub fn torque_limit(joint: Joint, value: f64, min: f64, max: f64) -> Self {
        Self::TorqueLimitExceeded {
            joint,
            value,
            min,
            max,
        }
    }
}

impl From<ProtocolError> for RobotError {
    fn from(value: ProtocolError) -> Self {
        match value {
            ProtocolError::MitInputOutOfRange {
                joint_index,
                field,
                value,
                min,
                max,
            } => {
                let joint = match Joint::from_index((joint_index.saturating_sub(1)) as usize) {
                    Some(joint) => joint,
                    None => {
                        return Self::Protocol(ProtocolError::InvalidJointIndex { joint_index });
                    },
                };

                match field {
                    MitControlField::PositionReference => Self::PositionReferenceOutOfRange {
                        joint,
                        value: value as f64,
                        min: min as f64,
                        max: max as f64,
                    },
                    MitControlField::VelocityReference => Self::VelocityReferenceOutOfRange {
                        joint,
                        value: value as f64,
                        min: min as f64,
                        max: max as f64,
                    },
                    MitControlField::Kp => Self::KpGainOutOfRange {
                        joint,
                        value: value as f64,
                        min: min as f64,
                        max: max as f64,
                    },
                    MitControlField::Kd => Self::KdGainOutOfRange {
                        joint,
                        value: value as f64,
                        min: min as f64,
                        max: max as f64,
                    },
                    MitControlField::TorqueReference => {
                        Self::torque_limit(joint, value as f64, min as f64, max as f64)
                    },
                }
            },
            other => Self::Protocol(other),
        }
    }
}

/// Result 类型别名
pub type Result<T> = std::result::Result<T, RobotError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_classification() {
        // 致命错误
        let fatal = RobotError::EmergencyStop;
        assert!(fatal.is_fatal());
        assert!(!fatal.is_retryable());

        let hardware_fail = RobotError::hardware_failure("connection lost");
        assert!(hardware_fail.is_fatal());

        let poisoned = RobotError::state_poisoned("state drift detected");
        assert!(poisoned.is_fatal());

        // 可恢复错误
        let recoverable = RobotError::timeout(100);
        assert!(!recoverable.is_fatal());
        assert!(recoverable.is_retryable());

        let can_io = RobotError::CanIoError("temporary failure".to_string());
        assert!(!can_io.is_fatal());
        assert!(can_io.is_retryable());

        let stale =
            RobotError::feedback_stale(Duration::from_millis(60), Duration::from_millis(50));
        assert!(!stale.is_fatal());
        assert!(stale.is_retryable());

        let monitor_incomplete =
            RobotError::monitor_state_incomplete(MonitorStateSource::EndPose, 0b101, 0b111);
        assert!(!monitor_incomplete.is_fatal());
        assert!(monitor_incomplete.is_retryable());
    }

    #[test]
    fn test_limit_errors() {
        let joint_limit = RobotError::joint_limit(Joint::J1, 3.5, std::f64::consts::PI);
        assert!(joint_limit.is_limit_error());
        assert!(!joint_limit.is_fatal());

        let velocity_limit = RobotError::velocity_limit(Joint::J2, 10.0, 5.0);
        assert!(velocity_limit.is_limit_error());

        let torque_limit = RobotError::torque_limit(Joint::J3, 15.0, -8.0, 8.0);
        assert!(torque_limit.is_limit_error());

        let kp_limit = RobotError::KpGainOutOfRange {
            joint: Joint::J4,
            value: 600.0,
            min: 0.0,
            max: 500.0,
        };
        assert!(kp_limit.is_limit_error());
    }

    #[test]
    fn test_config_errors() {
        let config_err = RobotError::ConfigError("invalid frequency".to_string());
        assert!(config_err.is_config_error());
        assert!(!config_err.is_fatal());

        let invalid_param = RobotError::InvalidParameter {
            param: "max_velocity".to_string(),
            reason: "must be positive".to_string(),
        };
        assert!(invalid_param.is_config_error());
    }

    #[test]
    fn test_error_display() {
        let err = RobotError::joint_limit(Joint::J1, 3.5, std::f64::consts::PI);
        let msg = format!("{}", err);
        assert!(msg.contains("J1"));
        assert!(msg.contains("3.5"));
        assert!(msg.contains("3.14"));

        let timeout_err = RobotError::timeout(100);
        let msg = format!("{}", timeout_err);
        assert!(msg.contains("100"));
        assert!(msg.contains("timeout"));

        let monitor_err =
            RobotError::monitor_state_incomplete(MonitorStateSource::EndPose, 0b101, 0b111);
        let msg = format!("{}", monitor_err);
        assert!(msg.contains("end pose"));
        assert!(msg.contains("101"));
        assert!(msg.contains("111"));
    }

    #[test]
    fn test_error_context() {
        let err = RobotError::Unknown("base error".to_string());
        let err_with_context = err.context("during initialization");
        let msg = format!("{}", err_with_context);
        assert!(msg.contains("during initialization"));
        assert!(msg.contains("base error"));
    }

    #[test]
    fn test_invalid_transition() {
        let err = RobotError::invalid_transition("Standby", "Active");
        let msg = format!("{}", err);
        assert!(msg.contains("Standby"));
        assert!(msg.contains("Active"));
    }

    #[test]
    fn test_protocol_mit_range_mapping() {
        let err: RobotError = ProtocolError::MitInputOutOfRange {
            joint_index: 3,
            field: MitControlField::TorqueReference,
            value: 9.0,
            min: -8.0,
            max: 8.0,
        }
        .into();

        assert!(matches!(
            err,
            RobotError::TorqueLimitExceeded {
                joint: Joint::J3,
                value,
                min,
                max,
            } if (value - 9.0).abs() < f64::EPSILON
                && (min + 8.0).abs() < f64::EPSILON
                && (max - 8.0).abs() < f64::EPSILON
        ));
    }

    #[test]
    fn test_protocol_errors() {
        let invalid_id = RobotError::InvalidFrameId { id: 0x123 };
        let msg = format!("{}", invalid_id);
        assert!(msg.contains("0x123"));

        let invalid_len = RobotError::InvalidDataLength {
            expected: 8,
            actual: 4,
        };
        let msg = format!("{}", invalid_len);
        assert!(msg.contains("8"));
        assert!(msg.contains("4"));
    }

    #[test]
    fn test_result_type() {
        let ok: Result<i32> = Ok(42);
        assert!(matches!(ok, Ok(42)));

        let err: Result<i32> = Err(RobotError::EmergencyStop);
        assert!(err.is_err());
    }

    #[test]
    fn test_monitor_state_incomplete_helper() {
        let err = RobotError::monitor_state_incomplete(
            MonitorStateSource::JointDynamic,
            0b001111,
            0b111111,
        );

        assert!(matches!(
            err,
            RobotError::MonitorStateIncomplete {
                state_source: MonitorStateSource::JointDynamic,
                valid_mask: 0b001111,
                required_mask: 0b111111,
            }
        ));
    }

    #[test]
    fn test_monitor_state_incomplete_has_no_error_source() {
        let err =
            RobotError::monitor_state_incomplete(MonitorStateSource::JointPosition, 0b001, 0b111);

        let source = std::error::Error::source(&err);
        assert!(source.is_none());
    }

    #[test]
    fn test_error_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<RobotError>();
    }
}
