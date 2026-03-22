//! 驱动层错误类型定义

use piper_can::CanError;
use piper_protocol::ProtocolError;
use thiserror::Error;

/// 驱动层错误类型
#[derive(Error, Debug)]
pub enum DriverError {
    /// CAN 驱动错误
    #[error("CAN driver error: {0}")]
    Can(#[from] CanError),

    /// 协议解析错误
    #[error("Protocol error: {0}")]
    Protocol(#[from] ProtocolError),

    /// 命令通道已关闭（IO 线程退出）
    #[error("Command channel closed")]
    ChannelClosed,

    /// 正常控制路径已关闭（故障锁存后只允许停机命令）
    #[error("Normal control path closed")]
    ControlPathClosed,

    /// Driver 已处于 Replay 模式，普通控制入口被封锁
    #[error("Replay mode active; normal control paths are disabled")]
    ReplayModeActive,

    /// 命令通道已满（缓冲区容量 10）
    #[error("Command channel full (buffer size: 10)")]
    ChannelFull,

    /// 已有不同停机帧正在执行单飞急停
    #[error("Shutdown lane already carries a different in-flight stop frame")]
    ShutdownConflict,

    /// 未使用双线程模式
    ///
    /// 某些方法（如 `send_realtime()`）只能在双线程模式下使用。
    #[error("Not in dual-thread mode. Use `new_dual_thread()` instead of `new()`")]
    NotDualThread,

    /// 锁被毒化（线程 panic）
    #[error("Poisoned lock (thread panic)")]
    PoisonedLock,

    /// IO 线程错误
    #[error("IO thread error: {0}")]
    IoThread(String),

    /// 功能未实现
    #[error("Not implemented: {0}")]
    NotImplemented(String),

    /// 操作超时
    #[error("Operation timeout")]
    Timeout,

    /// 无效输入（如空帧包）
    #[error("Invalid input: {0}")]
    InvalidInput(String),

    /// 已确认的实时命令在进入 TX 线程前被新命令覆盖
    #[error("Realtime delivery was overwritten before transmission")]
    RealtimeDeliveryOverwritten,

    /// 已确认的实时命令在 TX 线程中发送失败
    #[error("Realtime delivery failed after sending {sent}/{total} frames: {source}")]
    RealtimeDeliveryFailed {
        /// 已成功发送的帧数
        sent: usize,
        /// 计划发送的总帧数
        total: usize,
        /// 底层 CAN 发送错误
        #[source]
        source: CanError,
    },

    /// 已确认的实时命令在故障锁存后被中止
    #[error("Realtime delivery aborted by fault after sending {sent}/{total} frames")]
    RealtimeDeliveryAbortedByFault {
        /// 已成功发送的帧数
        sent: usize,
        /// 计划发送的总帧数
        total: usize,
    },

    /// 已确认的可靠命令在 TX 线程中发送失败
    #[error("Reliable delivery failed: {source}")]
    ReliableDeliveryFailed {
        /// 底层 CAN 发送错误
        #[source]
        source: CanError,
    },

    /// 已确认的可靠帧包在 TX 线程中发送失败
    #[error("Reliable package delivery failed after sending {sent}/{total} frames: {source}")]
    ReliablePackageDeliveryFailed {
        /// 已成功发送的帧数
        sent: usize,
        /// 计划发送的总帧数
        total: usize,
        /// 底层 CAN 发送错误
        #[source]
        source: CanError,
    },

    /// 已确认的可靠帧包在发送过程中超时
    #[error("Reliable package timed out after sending {sent}/{total} frames")]
    ReliablePackageTimeout {
        /// 已成功发送的帧数
        sent: usize,
        /// 计划发送的总帧数
        total: usize,
    },

    /// 命令因故障锁存而在 TX 线程中止
    #[error("Command aborted because runtime fault latched")]
    CommandAbortedByFault,

    /// 维护写入在发送点被运行时状态拒绝
    #[error("Maintenance write denied: {0}")]
    MaintenanceWriteDenied(String),

    /// 已确认的实时命令等待 TX 线程确认超时
    #[error("Realtime delivery confirmation timed out")]
    RealtimeDeliveryTimeout,
}

#[cfg(test)]
mod tests {
    use super::DriverError;
    use piper_can::CanError;
    use piper_protocol::ProtocolError;

    /// 测试 DriverError 的 Display 实现
    #[test]
    fn test_driver_error_display() {
        // 测试 Can 错误
        let can_error = CanError::Timeout;
        let driver_error = DriverError::Can(can_error);
        let msg = format!("{}", driver_error);
        assert!(
            msg.contains("Read timeout") || msg.contains("CAN"),
            "Can error message: {}",
            msg
        );

        // 测试 Protocol 错误
        let protocol_error = ProtocolError::InvalidLength {
            expected: 8,
            actual: 4,
        };
        let driver_error = DriverError::Protocol(protocol_error);
        let msg = format!("{}", driver_error);
        assert!(
            msg.contains("Invalid frame length"),
            "Protocol error message: {}",
            msg
        );

        // 测试 ChannelClosed
        let driver_error = DriverError::ChannelClosed;
        let msg = format!("{}", driver_error);
        assert_eq!(msg, "Command channel closed");

        let driver_error = DriverError::ControlPathClosed;
        let msg = format!("{}", driver_error);
        assert_eq!(msg, "Normal control path closed");

        let driver_error = DriverError::ReplayModeActive;
        let msg = format!("{}", driver_error);
        assert!(msg.contains("Replay mode active"));

        // 测试 ChannelFull
        let driver_error = DriverError::ChannelFull;
        let msg = format!("{}", driver_error);
        assert!(msg.contains("channel full") || msg.contains("ChannelFull"));

        // 测试 PoisonedLock
        let driver_error = DriverError::PoisonedLock;
        let msg = format!("{}", driver_error);
        assert!(msg.contains("Poisoned lock") || msg.contains("PoisonedLock"));

        // 测试 IoThread
        let driver_error = DriverError::IoThread("test error".to_string());
        let msg = format!("{}", driver_error);
        assert!(msg.contains("IO thread") && msg.contains("test error"));

        // 测试 NotImplemented
        let driver_error = DriverError::NotImplemented("feature".to_string());
        let msg = format!("{}", driver_error);
        assert!(msg.contains("Not implemented") && msg.contains("feature"));

        // 测试 Timeout
        let driver_error = DriverError::Timeout;
        let msg = format!("{}", driver_error);
        assert_eq!(msg, "Operation timeout");

        let driver_error = DriverError::RealtimeDeliveryOverwritten;
        let msg = format!("{}", driver_error);
        assert!(msg.contains("overwritten"));

        let driver_error = DriverError::RealtimeDeliveryFailed {
            sent: 1,
            total: 6,
            source: CanError::Timeout,
        };
        let msg = format!("{}", driver_error);
        assert!(msg.contains("1/6"));

        let driver_error = DriverError::ReliableDeliveryFailed {
            source: CanError::Timeout,
        };
        let msg = format!("{}", driver_error);
        assert!(msg.contains("Reliable delivery failed"));

        let driver_error = DriverError::ReliablePackageDeliveryFailed {
            sent: 2,
            total: 3,
            source: CanError::Timeout,
        };
        let msg = format!("{}", driver_error);
        assert!(msg.contains("2/3"));

        let driver_error = DriverError::ReliablePackageTimeout { sent: 1, total: 3 };
        let msg = format!("{}", driver_error);
        assert!(msg.contains("1/3"));

        let driver_error = DriverError::RealtimeDeliveryAbortedByFault { sent: 1, total: 6 };
        let msg = format!("{}", driver_error);
        assert!(msg.contains("aborted by fault"));

        let driver_error = DriverError::CommandAbortedByFault;
        let msg = format!("{}", driver_error);
        assert!(msg.contains("runtime fault latched"));

        let driver_error = DriverError::MaintenanceWriteDenied("standby required".to_string());
        let msg = format!("{}", driver_error);
        assert!(msg.contains("Maintenance write denied"));

        let driver_error = DriverError::RealtimeDeliveryTimeout;
        let msg = format!("{}", driver_error);
        assert!(msg.contains("timed out"));
    }

    /// 测试 From<CanError> 转换
    #[test]
    fn test_from_can_error() {
        let can_error = CanError::Timeout;
        let driver_error: DriverError = can_error.into();
        match driver_error {
            DriverError::Can(e) => assert!(matches!(e, CanError::Timeout)),
            _ => panic!("Expected Can variant"),
        }
    }

    /// 测试 From<ProtocolError> 转换
    #[test]
    fn test_from_protocol_error() {
        let protocol_error = ProtocolError::InvalidCanId { id: 0x123 };
        let driver_error: DriverError = protocol_error.into();
        match driver_error {
            DriverError::Protocol(e) => match e {
                ProtocolError::InvalidCanId { id } => assert_eq!(id, 0x123),
                _ => panic!("Expected InvalidCanId variant"),
            },
            _ => panic!("Expected Protocol variant"),
        }
    }
}
