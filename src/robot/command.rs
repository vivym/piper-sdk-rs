//! 命令类型定义模块
//!
//! 提供命令优先级和类型区分机制，优化丢弃策略。

use crate::can::PiperFrame;

/// 命令优先级
///
/// 用于区分不同类型的命令，优化发送策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandPriority {
    /// 实时控制命令（可丢弃）
    ///
    /// 用于高频控制命令（500Hz-1kHz），如关节位置控制。
    /// 如果队列满，新命令会覆盖旧命令（Overwrite 策略）。
    /// 这确保了最新的控制命令总是被发送，即使意味着丢弃旧命令。
    RealtimeControl,

    /// 可靠命令（不可丢弃）
    ///
    /// 用于配置帧、状态机切换帧等关键命令。
    /// 使用 FIFO 队列，按顺序发送，不会覆盖。
    /// 如果队列满，会阻塞或返回错误（取决于 API）。
    ReliableCommand,
}

/// 带优先级的命令
///
/// 封装 CAN 帧和优先级信息，用于类型安全的命令发送。
#[derive(Debug, Clone, Copy)]
pub struct PiperCommand {
    /// CAN 帧
    pub frame: PiperFrame,
    /// 命令优先级
    pub priority: CommandPriority,
}

impl PiperCommand {
    /// 创建实时控制命令
    pub fn realtime(frame: PiperFrame) -> Self {
        Self {
            frame,
            priority: CommandPriority::RealtimeControl,
        }
    }

    /// 创建可靠命令
    pub fn reliable(frame: PiperFrame) -> Self {
        Self {
            frame,
            priority: CommandPriority::ReliableCommand,
        }
    }

    /// 获取命令帧
    pub fn frame(&self) -> PiperFrame {
        self.frame
    }

    /// 获取命令优先级
    pub fn priority(&self) -> CommandPriority {
        self.priority
    }
}

impl From<PiperFrame> for PiperCommand {
    /// 默认转换为可靠命令（向后兼容）
    fn from(frame: PiperFrame) -> Self {
        Self::reliable(frame)
    }
}

impl From<PiperCommand> for PiperFrame {
    fn from(cmd: PiperCommand) -> Self {
        cmd.frame
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_command_priority() {
        let frame = PiperFrame::new_standard(0x123, &[1, 2, 3]);

        let realtime_cmd = PiperCommand::realtime(frame);
        assert_eq!(realtime_cmd.priority(), CommandPriority::RealtimeControl);

        let reliable_cmd = PiperCommand::reliable(frame);
        assert_eq!(reliable_cmd.priority(), CommandPriority::ReliableCommand);
    }

    #[test]
    fn test_command_from_frame() {
        let frame = PiperFrame::new_standard(0x123, &[1, 2, 3]);
        let cmd: PiperCommand = frame.into();

        // 默认转换为可靠命令
        assert_eq!(cmd.priority(), CommandPriority::ReliableCommand);
        assert_eq!(cmd.frame().id, 0x123);
    }

    #[test]
    fn test_command_to_frame() {
        let frame = PiperFrame::new_standard(0x123, &[1, 2, 3]);
        let cmd = PiperCommand::realtime(frame);

        let converted_frame: PiperFrame = cmd.into();
        assert_eq!(converted_frame.id, 0x123);
    }
}
