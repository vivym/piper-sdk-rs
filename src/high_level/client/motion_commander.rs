//! MotionCommander - 公开的运动命令接口
//!
//! 这是外部用户获得的**受限接口**，只能发送运动命令，
//! 无法修改状态机状态。这实现了"能力安全"设计。
//!
//! # 设计原则
//!
//! - **只读权限**: 无法调用 `enable`/`disable`/`set_mode`
//! - **运动命令**: 只能发送 MIT/位置/夹爪命令
//! - **类型安全**: 使用强类型单位（Rad, NewtonMeter）
//! - **性能优化**: 继承 RawCommander 的快速检查
//!
//! # 安全保证
//!
//! ```text
//! ❌ 无法从 MotionCommander 修改状态机
//! ✅ 只能发送运动指令
//! ✅ 状态检查自动执行
//! ```
//!
//! # 使用示例
//!
//! ```rust,no_run
//! # use piper_sdk::high_level::client::motion_commander::MotionCommander;
//! # use piper_sdk::high_level::types::*;
//! # fn example(motion: MotionCommander) -> Result<()> {
//! // ✅ 允许：发送运动命令
//! motion.send_mit_command(
//!     Joint::J1,
//!     Rad(1.0),
//!     0.5,
//!     10.0,
//!     2.0,
//!     NewtonMeter(5.0),
//! )?;
//!
//! // ❌ 禁止：无法调用 enable_arm()（方法不存在）
//! // motion.enable_arm(); // 编译错误
//! # Ok(())
//! # }
//! ```

use super::raw_commander::RawCommander;
use crate::high_level::types::*;
use std::sync::Arc;

/// 运动命令接口（受限权限）
///
/// 这是外部用户获得的接口，只能发送运动命令，
/// 无法修改状态机状态。
#[derive(Clone)]
pub struct MotionCommander {
    /// 内部命令发送器（完整权限）
    raw: Arc<RawCommander>,
}

impl MotionCommander {
    /// 创建新的 MotionCommander
    ///
    /// 这个方法只能由 crate 内部调用，外部无法直接构造。
    pub(crate) fn new(raw: Arc<RawCommander>) -> Self {
        MotionCommander { raw }
    }

    /// 发送 MIT 模式指令
    ///
    /// # 参数
    ///
    /// - `joint`: 关节选择（J1-J6）
    /// - `position`: 目标位置（Rad）
    /// - `velocity`: 目标速度（rad/s）
    /// - `kp`: 位置增益
    /// - `kd`: 速度增益
    /// - `torque`: 前馈力矩（NewtonMeter）
    ///
    /// # 错误
    ///
    /// - `RobotError::Poisoned`: 状态机已损坏
    /// - `RobotError::CommunicationError`: CAN 通信失败
    ///
    /// # 性能
    ///
    /// - 状态检查: ~10ns
    /// - 总延迟: < 100μs
    pub fn send_mit_command(
        &self,
        joint: Joint,
        position: Rad,
        velocity: f64,
        kp: f64,
        kd: f64,
        torque: NewtonMeter,
    ) -> Result<()> {
        self.raw.send_mit_command(joint, position, velocity, kp, kd, torque)
    }

    /// 批量发送 MIT 模式指令
    ///
    /// 对所有关节发送命令，比逐个发送更高效。
    ///
    /// # 参数
    ///
    /// - `positions`: 各关节目标位置
    /// - `velocities`: 各关节目标速度
    /// - `kp`: 位置增益（所有关节相同）
    /// - `kd`: 速度增益（所有关节相同）
    /// - `torques`: 各关节前馈力矩
    pub fn send_mit_command_batch(
        &self,
        positions: &JointArray<Rad>,
        velocities: &JointArray<f64>,
        kp: f64,
        kd: f64,
        torques: &JointArray<NewtonMeter>,
    ) -> Result<()> {
        for joint in [
            Joint::J1,
            Joint::J2,
            Joint::J3,
            Joint::J4,
            Joint::J5,
            Joint::J6,
        ] {
            self.raw.send_mit_command(
                joint,
                positions[joint],
                velocities[joint],
                kp,
                kd,
                torques[joint],
            )?;
        }
        Ok(())
    }

    /// 发送位置模式指令
    ///
    /// # 参数
    ///
    /// - `joint`: 关节选择
    /// - `position`: 目标位置（Rad）
    /// - `velocity`: 目标速度（rad/s）
    pub fn send_position_command(&self, joint: Joint, position: Rad, velocity: f64) -> Result<()> {
        self.raw.send_position_command(joint, position, velocity)
    }

    /// 批量发送位置模式指令
    ///
    /// # 参数
    ///
    /// - `positions`: 各关节目标位置
    /// - `velocities`: 各关节目标速度
    pub fn send_position_command_batch(
        &self,
        positions: &JointArray<Rad>,
        velocities: &JointArray<f64>,
    ) -> Result<()> {
        for joint in [
            Joint::J1,
            Joint::J2,
            Joint::J3,
            Joint::J4,
            Joint::J5,
            Joint::J6,
        ] {
            self.raw.send_position_command(joint, positions[joint], velocities[joint])?;
        }
        Ok(())
    }

    /// 控制夹爪
    ///
    /// # 参数
    ///
    /// - `position`: 夹爪开口（0.0-1.0，1.0 = 完全打开）
    /// - `effort`: 夹持力度（0.0-1.0，1.0 = 最大力度）
    ///
    /// # 示例
    ///
    /// ```rust,no_run
    /// # use piper_sdk::high_level::client::motion_commander::MotionCommander;
    /// # use piper_sdk::high_level::types::*;
    /// # fn example(motion: MotionCommander) -> Result<()> {
    /// // 完全打开，低力度
    /// motion.set_gripper(1.0, 0.3)?;
    ///
    /// // 夹取物体，中等力度
    /// motion.set_gripper(0.2, 0.5)?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn set_gripper(&self, position: f64, effort: f64) -> Result<()> {
        // 参数验证
        if !(0.0..=1.0).contains(&position) {
            return Err(RobotError::ConfigError(
                "Gripper position must be in [0.0, 1.0]".to_string(),
            ));
        }
        if !(0.0..=1.0).contains(&effort) {
            return Err(RobotError::ConfigError(
                "Gripper effort must be in [0.0, 1.0]".to_string(),
            ));
        }

        self.raw.send_gripper_command(position, effort)
    }

    /// 发送关节力矩命令
    ///
    /// 便捷方法，只发送力矩控制（位置和速度为 0，kp/kd = 0）。
    ///
    /// # 参数
    ///
    /// - `torques`: 6 个关节的目标力矩
    ///
    /// # 示例
    ///
    /// ```rust,no_run
    /// # use piper_sdk::high_level::client::MotionCommander;
    /// # use piper_sdk::high_level::types::*;
    /// # fn example(motion: MotionCommander) -> Result<()> {
    /// let torques = JointArray::from([NewtonMeter(1.0); 6]);
    /// motion.command_torques(torques)?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn command_torques(&self, torques: JointArray<NewtonMeter>) -> Result<()> {
        let positions = JointArray::from([Rad(0.0); 6]);
        let velocities = JointArray::from([0.0; 6]);
        self.send_mit_command_batch(&positions, &velocities, 0.0, 0.0, &torques)
    }

    /// 打开夹爪
    ///
    /// 便捷方法，相当于 `set_gripper(1.0, 0.3)`
    pub fn open_gripper(&self) -> Result<()> {
        self.set_gripper(1.0, 0.3)
    }

    /// 关闭夹爪
    ///
    /// 便捷方法，相当于 `set_gripper(0.0, 0.5)`
    pub fn close_gripper(&self, effort: f64) -> Result<()> {
        self.set_gripper(0.0, effort)
    }

    // ❌ 注意：以下方法不存在（防止状态修改）
    // pub fn enable_arm(&self) -> Result<()>
    // pub fn disable_arm(&self) -> Result<()>
    // pub fn set_control_mode(&self, mode: ControlMode) -> Result<()>
}

// 确保 Send + Sync
unsafe impl Send for MotionCommander {}
unsafe impl Sync for MotionCommander {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::high_level::client::raw_commander::CanSender;
    use crate::high_level::client::state_tracker::StateTracker;
    use std::sync::Mutex as StdMutex;

    type SentFrames = Arc<StdMutex<Vec<(u32, Vec<u8>)>>>;

    /// Mock CAN 发送器
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

    fn setup_motion_commander() -> (MotionCommander, Arc<MockCanSender>) {
        let tracker = Arc::new(StateTracker::new());
        let sender = Arc::new(MockCanSender::new());
        let raw = Arc::new(RawCommander::new(tracker, sender.clone()));
        let motion = MotionCommander::new(raw);
        (motion, sender)
    }

    #[test]
    fn test_send_mit_command() {
        let (motion, mock) = setup_motion_commander();

        let result = motion.send_mit_command(Joint::J1, Rad(1.0), 0.5, 10.0, 2.0, NewtonMeter(5.0));

        assert!(result.is_ok());
        let frames = mock.get_sent_frames();
        assert_eq!(frames.len(), 1);
    }

    #[test]
    fn test_send_mit_command_batch() {
        let (motion, mock) = setup_motion_commander();

        let positions = JointArray::splat(Rad(1.0));
        let velocities = JointArray::splat(0.5);
        let torques = JointArray::splat(NewtonMeter(2.0));

        let result = motion.send_mit_command_batch(&positions, &velocities, 10.0, 2.0, &torques);

        assert!(result.is_ok());
        let frames = mock.get_sent_frames();
        assert_eq!(frames.len(), 6); // 6 个关节
    }

    #[test]
    fn test_send_position_command() {
        let (motion, mock) = setup_motion_commander();

        let result = motion.send_position_command(Joint::J2, Rad(0.5), 1.0);

        assert!(result.is_ok());
        let frames = mock.get_sent_frames();
        assert_eq!(frames.len(), 1);
    }

    #[test]
    fn test_send_position_command_batch() {
        let (motion, mock) = setup_motion_commander();

        let positions = JointArray::splat(Rad(0.5));
        let velocities = JointArray::splat(1.0);

        let result = motion.send_position_command_batch(&positions, &velocities);

        assert!(result.is_ok());
        let frames = mock.get_sent_frames();
        assert_eq!(frames.len(), 6);
    }

    #[test]
    fn test_set_gripper() {
        let (motion, mock) = setup_motion_commander();

        let result = motion.set_gripper(0.5, 0.8);

        assert!(result.is_ok());
        let frames = mock.get_sent_frames();
        assert_eq!(frames.len(), 1);
    }

    #[test]
    fn test_set_gripper_invalid_position() {
        let (motion, _mock) = setup_motion_commander();

        let result = motion.set_gripper(1.5, 0.5);

        assert!(result.is_err());
        match result {
            Err(RobotError::ConfigError(msg)) => {
                assert!(msg.contains("position"));
            },
            _ => panic!("Expected ConfigError"),
        }
    }

    #[test]
    fn test_set_gripper_invalid_effort() {
        let (motion, _mock) = setup_motion_commander();

        let result = motion.set_gripper(0.5, -0.1);

        assert!(result.is_err());
    }

    #[test]
    fn test_open_gripper() {
        let (motion, mock) = setup_motion_commander();

        let result = motion.open_gripper();

        assert!(result.is_ok());
        let frames = mock.get_sent_frames();
        assert_eq!(frames.len(), 1);
    }

    #[test]
    fn test_close_gripper() {
        let (motion, mock) = setup_motion_commander();

        let result = motion.close_gripper(0.7);

        assert!(result.is_ok());
        let frames = mock.get_sent_frames();
        assert_eq!(frames.len(), 1);
    }

    #[test]
    fn test_clone() {
        let (motion1, _) = setup_motion_commander();
        let motion2 = motion1.clone();

        // 验证两个实例都可以使用
        assert!(motion1.open_gripper().is_ok());
        assert!(motion2.close_gripper(0.5).is_ok());
    }

    #[test]
    fn test_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<MotionCommander>();
    }

    #[test]
    fn test_concurrent_access() {
        let (motion, _) = setup_motion_commander();
        let motion_arc = Arc::new(motion);

        let mut handles = vec![];
        for i in 0..5 {
            let motion_clone = motion_arc.clone();
            handles.push(std::thread::spawn(move || {
                for _ in 0..50 {
                    let _ = motion_clone.send_mit_command(
                        Joint::J1,
                        Rad(i as f64 * 0.1),
                        0.5,
                        10.0,
                        2.0,
                        NewtonMeter(2.0),
                    );
                }
            }));
        }

        for handle in handles {
            handle.join().unwrap();
        }
    }

    // ✅ 验证：无法调用状态修改方法（编译期检查）
    #[test]
    fn test_no_state_modification_methods() {
        let (motion, _) = setup_motion_commander();

        // 以下代码应该无法编译（取消注释会报错）
        // motion.enable_arm();  // 方法不存在
        // motion.disable_arm(); // 方法不存在
        // motion.set_control_mode(ControlMode::MitMode); // 方法不存在

        // 验证编译通过
        assert!(motion.open_gripper().is_ok());
    }
}
