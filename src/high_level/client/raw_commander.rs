//! RawCommander - 内部命令发送器
//!
//! 提供完整的命令发送能力，但仅对 crate 内部可见。
//! 这是实现"能力安全"的关键：外部只能获得受限的 MotionCommander。
//!
//! # 设计目标
//!
//! - **完整权限**: 可以修改状态、控制模式
//! - **内部可见**: 所有状态修改方法都是 `pub(crate)`
//! - **性能优化**: 使用 StateTracker 快速检查
//! - **类型安全**: 使用强类型单位（Rad, NewtonMeter）
//!
//! # 架构
//!
//! ```text
//! ┌──────────────────┐
//! │  RawCommander    │
//! ├──────────────────┤
//! │ state_tracker    │ ← 快速状态检查
//! │ can_sender       │ ← CAN 帧发送（抽象）
//! └──────────────────┘
//! ```

use parking_lot::Mutex;
use std::sync::Arc;

use super::state_tracker::{ArmController, ControlMode, StateTracker};
use crate::high_level::types::*;

/// CAN 帧发送接口（抽象）
///
/// 这个 trait 允许在实现时使用实际的 CAN 接口，
/// 在测试时使用 Mock 实现。
pub trait CanSender: Send + Sync {
    /// 发送 CAN 帧
    fn send_frame(&self, id: u32, data: &[u8]) -> Result<()>;

    /// 接收 CAN 帧（可选，用于同步命令）
    fn recv_frame(&self, timeout_ms: u64) -> Result<(u32, Vec<u8>)>;
}

/// 内部命令发送器（完整权限）
///
/// 仅对 crate 内部可见，提供所有命令发送和状态修改能力。
pub(crate) struct RawCommander {
    /// 状态跟踪器
    state_tracker: Arc<StateTracker>,
    /// CAN 发送接口
    can_sender: Arc<dyn CanSender>,
    /// 发送锁（保证帧序）
    send_lock: Mutex<()>,
}

impl RawCommander {
    /// 创建新的 RawCommander
    #[allow(dead_code)]
    pub(crate) fn new(state_tracker: Arc<StateTracker>, can_sender: Arc<dyn CanSender>) -> Self {
        RawCommander {
            state_tracker,
            can_sender,
            send_lock: Mutex::new(()),
        }
    }

    /// 获取 StateTracker 引用（内部使用）
    pub(crate) fn state_tracker(&self) -> &Arc<StateTracker> {
        &self.state_tracker
    }

    /// 发送 MIT 模式指令（热路径优化）
    ///
    /// # 性能
    ///
    /// - 状态检查: ~10ns (原子操作)
    /// - CAN 发送: ~10-50μs (取决于硬件)
    /// - 总延迟: < 100μs
    pub(crate) fn send_mit_command(
        &self,
        joint: Joint,
        position: Rad,
        velocity: f64,
        kp: f64,
        kd: f64,
        torque: NewtonMeter,
    ) -> Result<()> {
        // ✅ 快速状态检查（无锁，~10ns）
        self.state_tracker.check_valid_fast()?;

        // 构建 MIT 模式 CAN 帧
        let frame_id = 0x100 + joint.index() as u32;
        let data = self.build_mit_frame_data(position, velocity, kp, kd, torque);

        // 发送（保证顺序）
        let _guard = self.send_lock.lock();
        self.can_sender.send_frame(frame_id, &data)?;

        Ok(())
    }

    /// 发送位置模式指令
    pub(crate) fn send_position_command(
        &self,
        joint: Joint,
        position: Rad,
        velocity: f64,
    ) -> Result<()> {
        self.state_tracker.check_valid_fast()?;

        let frame_id = 0x200 + joint.index() as u32;
        let data = self.build_position_frame_data(position, velocity);

        let _guard = self.send_lock.lock();
        self.can_sender.send_frame(frame_id, &data)?;

        Ok(())
    }

    /// 控制夹爪
    pub(crate) fn send_gripper_command(&self, position: f64, effort: f64) -> Result<()> {
        self.state_tracker.check_valid_fast()?;

        let frame_id = 0x300;
        let data = self.build_gripper_frame_data(position, effort);

        let _guard = self.send_lock.lock();
        self.can_sender.send_frame(frame_id, &data)?;

        Ok(())
    }

    /// 使能机械臂（仅内部可见）
    pub(crate) fn enable_arm(&self) -> Result<()> {
        self.state_tracker.check_valid_fast()?;

        let frame_id = 0x01;
        let data = vec![0x01]; // 使能命令

        let _guard = self.send_lock.lock();
        self.can_sender.send_frame(frame_id, &data)?;

        // 更新期望状态
        self.state_tracker.set_expected_controller(ArmController::Enabled);

        Ok(())
    }

    /// 失能机械臂（仅内部可见）
    pub(crate) fn disable_arm(&self) -> Result<()> {
        // 失能不检查状态（安全操作）
        let frame_id = 0x02;
        let data = vec![0x00]; // 失能命令

        let _guard = self.send_lock.lock();
        self.can_sender.send_frame(frame_id, &data)?;

        // 更新期望状态
        self.state_tracker.set_expected_controller(ArmController::Standby);

        Ok(())
    }

    /// 设置控制模式（仅内部可见）
    pub(crate) fn set_control_mode(&self, mode: ControlMode) -> Result<()> {
        self.state_tracker.check_valid_fast()?;

        let frame_id = 0x03;
        let data = match mode {
            ControlMode::PositionMode => vec![0x01],
            ControlMode::MitMode => vec![0x02],
            ControlMode::Unknown => {
                return Err(RobotError::ConfigError("Invalid control mode".to_string()));
            },
        };

        let _guard = self.send_lock.lock();
        self.can_sender.send_frame(frame_id, &data)?;

        // 更新期望状态
        self.state_tracker.set_expected_mode(mode);

        Ok(())
    }

    /// 急停（仅内部可见）
    #[allow(dead_code)]
    pub(crate) fn emergency_stop(&self) -> Result<()> {
        // 急停不检查状态（安全优先）
        let frame_id = 0xFF;
        let data = vec![0xFF]; // 急停命令

        let _guard = self.send_lock.lock();
        self.can_sender.send_frame(frame_id, &data)?;

        // 标记为损坏状态
        self.state_tracker.mark_poisoned("Emergency stop triggered");

        Ok(())
    }

    // ==================== 私有辅助方法 ====================

    /// 构建 MIT 模式帧数据
    fn build_mit_frame_data(
        &self,
        position: Rad,
        velocity: f64,
        kp: f64,
        kd: f64,
        torque: NewtonMeter,
    ) -> Vec<u8> {
        // 简化实现：实际应该按照协议编码
        let mut data = Vec::with_capacity(8);

        // 位置 (2 bytes, 缩放)
        let pos_scaled = (position.0 * 1000.0) as i16;
        data.extend_from_slice(&pos_scaled.to_le_bytes());

        // 速度 (2 bytes, 缩放)
        let vel_scaled = (velocity * 100.0) as i16;
        data.extend_from_slice(&vel_scaled.to_le_bytes());

        // kp (1 byte)
        data.push((kp * 10.0) as u8);

        // kd (1 byte)
        data.push((kd * 10.0) as u8);

        // 力矩 (2 bytes, 缩放)
        let torque_scaled = (torque.0 * 100.0) as i16;
        data.extend_from_slice(&torque_scaled.to_le_bytes());

        data
    }

    /// 构建位置模式帧数据
    fn build_position_frame_data(&self, position: Rad, velocity: f64) -> Vec<u8> {
        let mut data = Vec::with_capacity(8);

        let pos_scaled = (position.0 * 1000.0) as i32;
        data.extend_from_slice(&pos_scaled.to_le_bytes());

        let vel_scaled = (velocity * 100.0) as i16;
        data.extend_from_slice(&vel_scaled.to_le_bytes());

        data
    }

    /// 构建夹爪帧数据
    fn build_gripper_frame_data(&self, position: f64, effort: f64) -> Vec<u8> {
        let mut data = Vec::with_capacity(8);

        let pos_scaled = (position * 1000.0) as u16;
        data.extend_from_slice(&pos_scaled.to_le_bytes());

        let effort_scaled = (effort * 100.0) as u16;
        data.extend_from_slice(&effort_scaled.to_le_bytes());

        data
    }
}

// 确保 Send + Sync
unsafe impl Send for RawCommander {}
unsafe impl Sync for RawCommander {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    type SentFrames = Arc<StdMutex<Vec<(u32, Vec<u8>)>>>;

    /// Mock CAN 发送器（用于测试）
    struct MockCanSender {
        sent_frames: SentFrames,
    }

    impl MockCanSender {
        fn new() -> Self {
            MockCanSender {
                sent_frames: Arc::new(StdMutex::new(Vec::new())),
            }
        }

        fn get_sent_frames(&self) -> Vec<(u32, Vec<u8>)> {
            self.sent_frames.lock().unwrap().clone()
        }

        fn clear(&self) {
            self.sent_frames.lock().unwrap().clear();
        }
    }

    impl CanSender for MockCanSender {
        fn send_frame(&self, id: u32, data: &[u8]) -> Result<()> {
            self.sent_frames.lock().unwrap().push((id, data.to_vec()));
            Ok(())
        }

        fn recv_frame(&self, _timeout_ms: u64) -> Result<(u32, Vec<u8>)> {
            Ok((0, vec![]))
        }
    }

    fn setup_commander() -> (Arc<RawCommander>, Arc<MockCanSender>) {
        let tracker = Arc::new(StateTracker::new());
        let sender = Arc::new(MockCanSender::new());
        let commander = Arc::new(RawCommander::new(tracker, sender.clone()));
        (commander, sender)
    }

    #[test]
    fn test_send_mit_command() {
        let (commander, mock) = setup_commander();
        mock.clear();

        let result =
            commander.send_mit_command(Joint::J1, Rad(1.0), 0.5, 10.0, 2.0, NewtonMeter(5.0));

        assert!(result.is_ok());
        let frames = mock.get_sent_frames();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].0, 0x100); // J1 的 ID
    }

    #[test]
    fn test_send_position_command() {
        let (commander, mock) = setup_commander();

        let result = commander.send_position_command(Joint::J2, Rad(0.5), 1.0);

        assert!(result.is_ok());
        let frames = mock.get_sent_frames();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].0, 0x201); // J2 的 ID
    }

    #[test]
    fn test_send_gripper_command() {
        let (commander, mock) = setup_commander();

        let result = commander.send_gripper_command(0.05, 10.0);

        assert!(result.is_ok());
        let frames = mock.get_sent_frames();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].0, 0x300); // 夹爪 ID
    }

    #[test]
    fn test_enable_arm() {
        let (commander, mock) = setup_commander();

        let result = commander.enable_arm();

        assert!(result.is_ok());
        let frames = mock.get_sent_frames();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].0, 0x01);
    }

    #[test]
    fn test_disable_arm() {
        let (commander, mock) = setup_commander();

        let result = commander.disable_arm();

        assert!(result.is_ok());
        let frames = mock.get_sent_frames();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].0, 0x02);
    }

    #[test]
    fn test_set_control_mode() {
        let (commander, mock) = setup_commander();

        let result = commander.set_control_mode(ControlMode::MitMode);

        assert!(result.is_ok());
        let frames = mock.get_sent_frames();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].0, 0x03);
    }

    #[test]
    fn test_emergency_stop() {
        let (commander, mock) = setup_commander();

        let result = commander.emergency_stop();

        assert!(result.is_ok());
        let frames = mock.get_sent_frames();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].0, 0xFF);

        // 验证状态被标记为损坏
        assert!(!commander.state_tracker.is_valid());
    }

    #[test]
    fn test_state_check_prevents_command() {
        let (commander, mock) = setup_commander();

        // 标记为损坏
        commander.state_tracker.mark_poisoned("Test");

        // 尝试发送命令应该失败
        let result =
            commander.send_mit_command(Joint::J1, Rad(1.0), 0.5, 10.0, 2.0, NewtonMeter(5.0));

        assert!(result.is_err());
        assert_eq!(mock.get_sent_frames().len(), 0);
    }

    #[test]
    fn test_concurrent_commands() {
        let (commander, _mock) = setup_commander();

        let mut handles = vec![];
        for i in 0..10 {
            let cmd_clone = commander.clone();
            handles.push(std::thread::spawn(move || {
                for _ in 0..100 {
                    let _ = cmd_clone.send_mit_command(
                        Joint::J1,
                        Rad(i as f64 * 0.1),
                        0.5,
                        10.0,
                        2.0,
                        NewtonMeter(5.0),
                    );
                }
            }));
        }

        for handle in handles {
            handle.join().unwrap();
        }
    }

    #[test]
    fn test_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<RawCommander>();
    }
}
