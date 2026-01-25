//! 命令类型定义模块
//!
//! 提供命令优先级和类型区分机制，优化丢弃策略。

use piper_can::PiperFrame;
use smallvec::SmallVec;

// 编译期断言：确保 PiperFrame 永远实现 Copy，这对 SmallVec 性能至关重要
// 如果未来有人给 PiperFrame 添加非 Copy 字段（如 String），这里会编译失败
#[cfg(test)]
const _: () = {
    fn assert_copy<T: Copy>() {}
    fn check() {
        assert_copy::<piper_can::PiperFrame>();
    }
    // 调用 check 以触发编译期检查
    let _ = check;
};

/// 帧缓冲区类型
///
/// 使用 SmallVec 在栈上预留 6 个位置，足以覆盖：
/// - MIT 控制：6 帧（0x15A, 0x15B, 0x15C, 0x15D, 0x15E, 0x15F）- **高频控制协议**
/// - 位置控制：3 帧（0x155, 0x156, 0x157）
/// - 末端位姿控制：3 帧（0x152, 0x153, 0x154）
/// - 单个帧：1 帧（向后兼容）
///
/// **为什么是 6？**
/// - MIT 控制是高频控制协议（通常 500Hz-1kHz），需要同时控制所有 6 个关节
/// - 每个关节需要 1 帧（CAN ID: 0x15A + joint_index）
/// - 总共需要 6 帧，必须一次性打包发送以避免覆盖问题
/// - 使用栈缓冲区（6 帧）可以避免堆分配，确保实时性能
///
/// 占用空间约：24 bytes * 6 + overhead ≈ 150 bytes，对于 Mutex 内容来说仍然轻量
///
/// **性能要求**：`PiperFrame` 必须实现 `Copy` Trait，这样 `SmallVec` 在收集和迭代时
/// 会编译为高效的内存拷贝指令（`memcpy`），避免调用 `Clone::clone`。
///
/// **确认**：`PiperFrame` 已实现 `Copy` Trait（见 `src/can/mod.rs:35`），满足性能要求。
pub type FrameBuffer = SmallVec<[PiperFrame; 6]>;

/// 实时命令类型（统一使用 FrameBuffer）
///
/// **设计决策**：不再区分 Single 和 Package，统一使用 FrameBuffer。
/// - Single 只是 len=1 的 FrameBuffer
/// - 简化 TX 线程逻辑（不需要 match 分支）
/// - 消除 CPU 分支预测压力
#[derive(Debug, Clone)]
pub struct RealtimeCommand {
    frames: FrameBuffer,
}

impl RealtimeCommand {
    /// 创建单个帧命令（向后兼容）
    ///
    /// **性能优化**：添加 `#[inline]` 属性，因为此方法处于热路径（Hot Path）上。
    #[inline]
    pub fn single(frame: PiperFrame) -> Self {
        let mut buffer = FrameBuffer::new();
        buffer.push(frame); // 不会分配堆内存（len=1 < 6）
        RealtimeCommand { frames: buffer }
    }

    /// 创建帧包命令
    ///
    /// **性能优化**：添加 `#[inline]` 属性，因为此方法处于热路径（Hot Path）上。
    ///
    /// **注意**：如果用户传入 `Vec<PiperFrame>`，`into_iter()` 会消耗这个 `Vec`。
    /// 如果 `Vec` 长度 > 6，`SmallVec` 可能会尝试重用 `Vec` 的堆内存或重新分配。
    /// 虽然这是安全的，但为了最佳性能，建议用户传入数组（栈分配）。
    #[inline]
    pub fn package(frames: impl IntoIterator<Item = PiperFrame>) -> Self {
        let buffer: FrameBuffer = frames.into_iter().collect();
        RealtimeCommand { frames: buffer }
    }

    /// 获取帧数量
    #[inline]
    pub fn len(&self) -> usize {
        self.frames.len()
    }

    /// 检查是否为空
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    /// 获取帧迭代器（用于 TX 线程发送）
    #[inline]
    pub fn iter(&self) -> impl Iterator<Item = &PiperFrame> {
        self.frames.iter()
    }

    /// 消费并获取帧（用于 TX 线程发送）
    #[inline]
    pub fn into_frames(self) -> FrameBuffer {
        self.frames
    }
}

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

#[cfg(test)]
mod realtime_command_tests {
    use super::*;

    #[test]
    fn test_realtime_command_single() {
        let frame = PiperFrame::new_standard(0x123, &[0x01, 0x02]);
        let cmd = RealtimeCommand::single(frame);
        assert_eq!(cmd.len(), 1);
        assert!(!cmd.is_empty());
    }

    #[test]
    fn test_realtime_command_package() {
        let frames = [
            PiperFrame::new_standard(0x155, &[0x01]),
            PiperFrame::new_standard(0x156, &[0x02]),
            PiperFrame::new_standard(0x157, &[0x03]),
        ];
        let cmd = RealtimeCommand::package(frames);
        assert_eq!(cmd.len(), 3);
        assert!(!cmd.is_empty());
    }

    #[test]
    fn test_realtime_command_empty() {
        let frames: [PiperFrame; 0] = [];
        let cmd = RealtimeCommand::package(frames);
        assert_eq!(cmd.len(), 0);
        assert!(cmd.is_empty());
    }

    #[test]
    fn test_realtime_command_iter() {
        let frames = [
            PiperFrame::new_standard(0x155, &[0x01]),
            PiperFrame::new_standard(0x156, &[0x02]),
        ];
        let cmd = RealtimeCommand::package(frames);
        let collected: Vec<_> = cmd.iter().collect();
        assert_eq!(collected.len(), 2);
    }

    #[test]
    fn test_realtime_command_into_frames() {
        let frames = [
            PiperFrame::new_standard(0x155, &[0x01]),
            PiperFrame::new_standard(0x156, &[0x02]),
        ];
        let cmd = RealtimeCommand::package(frames);
        let buffer = cmd.into_frames();
        assert_eq!(buffer.len(), 2);
    }
}
