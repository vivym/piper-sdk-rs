//! Robot 模块错误类型定义

use crate::can::CanError;
use crate::protocol::ProtocolError;
use thiserror::Error;

/// Robot 模块错误类型
#[derive(Error, Debug)]
pub enum RobotError {
    /// CAN 驱动错误
    #[error("CAN driver error: {0}")]
    Can(#[from] CanError),

    /// 协议解析错误
    #[error("Protocol error: {0}")]
    Protocol(#[from] ProtocolError),

    /// 命令通道已关闭（IO 线程退出）
    #[error("Command channel closed")]
    ChannelClosed,

    /// 命令通道已满（缓冲区容量 10）
    #[error("Command channel full (buffer size: 10)")]
    ChannelFull,

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
}

#[cfg(test)]
mod tests {
    use super::RobotError;
    use crate::can::CanError;
    use crate::protocol::ProtocolError;

    /// 测试 RobotError 的 Display 实现
    #[test]
    fn test_robot_error_display() {
        // 测试 Can 错误
        let can_error = CanError::Timeout;
        let robot_error = RobotError::Can(can_error);
        let msg = format!("{}", robot_error);
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
        let robot_error = RobotError::Protocol(protocol_error);
        let msg = format!("{}", robot_error);
        assert!(
            msg.contains("Invalid frame length"),
            "Protocol error message: {}",
            msg
        );

        // 测试 ChannelClosed
        let robot_error = RobotError::ChannelClosed;
        let msg = format!("{}", robot_error);
        assert_eq!(msg, "Command channel closed");

        // 测试 ChannelFull
        let robot_error = RobotError::ChannelFull;
        let msg = format!("{}", robot_error);
        assert!(msg.contains("channel full") || msg.contains("ChannelFull"));

        // 测试 PoisonedLock
        let robot_error = RobotError::PoisonedLock;
        let msg = format!("{}", robot_error);
        assert!(msg.contains("Poisoned lock") || msg.contains("PoisonedLock"));

        // 测试 IoThread
        let robot_error = RobotError::IoThread("test error".to_string());
        let msg = format!("{}", robot_error);
        assert!(msg.contains("IO thread") && msg.contains("test error"));

        // 测试 NotImplemented
        let robot_error = RobotError::NotImplemented("feature".to_string());
        let msg = format!("{}", robot_error);
        assert!(msg.contains("Not implemented") && msg.contains("feature"));

        // 测试 Timeout
        let robot_error = RobotError::Timeout;
        let msg = format!("{}", robot_error);
        assert_eq!(msg, "Operation timeout");
    }

    /// 测试 From<CanError> 转换
    #[test]
    fn test_from_can_error() {
        let can_error = CanError::Timeout;
        let robot_error: RobotError = can_error.into();
        match robot_error {
            RobotError::Can(e) => assert!(matches!(e, CanError::Timeout)),
            _ => panic!("Expected Can variant"),
        }
    }

    /// 测试 From<ProtocolError> 转换
    #[test]
    fn test_from_protocol_error() {
        let protocol_error = ProtocolError::InvalidCanId { id: 0x123 };
        let robot_error: RobotError = protocol_error.into();
        match robot_error {
            RobotError::Protocol(e) => match e {
                ProtocolError::InvalidCanId { id } => assert_eq!(id, 0x123),
                _ => panic!("Expected InvalidCanId variant"),
            },
            _ => panic!("Expected Protocol variant"),
        }
    }
}
