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
//! use piper_sdk::high_level::types::RobotError;
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
use thiserror::Error;

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

    /// 力矩限制超出
    #[error("Torque limit exceeded for joint {joint}: {value:.3} (limit: {limit:.3})")]
    TorqueLimitExceeded {
        /// 关节索引
        joint: Joint,
        /// 实际力矩
        value: f64,
        /// 限矩
        limit: f64,
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
    #[error("Protocol error: {0}")]
    ProtocolError(String),

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
        )
    }

    /// 是否可重试
    ///
    /// 可重试错误表示重新执行操作可能会成功。
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            Self::Timeout { .. } | Self::CanIoError(_) | Self::ProtocolError(_)
        )
    }

    /// 是否为配置错误
    pub fn is_config_error(&self) -> bool {
        matches!(self, Self::ConfigError(_) | Self::InvalidParameter { .. })
    }

    /// 是否为限位错误
    pub fn is_limit_error(&self) -> bool {
        matches!(
            self,
            Self::JointLimitExceeded { .. }
                | Self::VelocityLimitExceeded { .. }
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
    pub fn torque_limit(joint: Joint, value: f64, limit: f64) -> Self {
        Self::TorqueLimitExceeded {
            joint,
            value,
            limit,
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
    }

    #[test]
    fn test_limit_errors() {
        let joint_limit = RobotError::joint_limit(Joint::J1, 3.5, std::f64::consts::PI);
        assert!(joint_limit.is_limit_error());
        assert!(!joint_limit.is_fatal());

        let velocity_limit = RobotError::velocity_limit(Joint::J2, 10.0, 5.0);
        assert!(velocity_limit.is_limit_error());

        let torque_limit = RobotError::torque_limit(Joint::J3, 15.0, 10.0);
        assert!(torque_limit.is_limit_error());
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
    fn test_error_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<RobotError>();
    }
}
