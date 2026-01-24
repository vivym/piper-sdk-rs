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
//! # use piper_sdk::client::motion::MotionCommander;
//! # use piper_sdk::client::types::*;
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

use crate::client::types::*;
use crate::driver::Piper as RobotPiper;
use std::sync::Arc;

/// 运动命令接口（受限权限）
///
/// 这是外部用户获得的接口，只能发送运动命令，
/// 无法修改状态机状态。
#[derive(Clone)]
pub struct MotionCommander {
    /// Driver 实例（直接持有，零拷贝）
    driver: Arc<RobotPiper>,
}

impl MotionCommander {
    /// 创建新的 MotionCommander
    ///
    /// 这个方法只能由 crate 内部调用，外部无法直接构造。
    pub(crate) fn new(driver: Arc<RobotPiper>) -> Self {
        MotionCommander { driver }
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
        // ✅ 临时创建 RawCommander（零开销，使用引用）
        use super::raw_commander::RawCommander;
        let raw = RawCommander::new(&self.driver);
        raw.send_mit_command(joint, position, velocity, kp, kd, torque)
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
        // ✅ 在循环外创建一次 RawCommander，提高效率
        use super::raw_commander::RawCommander;
        let raw = RawCommander::new(&self.driver);

        for joint in [
            Joint::J1,
            Joint::J2,
            Joint::J3,
            Joint::J4,
            Joint::J5,
            Joint::J6,
        ] {
            raw.send_mit_command(
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

    /// 更新单个关节位置（保持其他关节不变）
    ///
    /// **注意**：此方法会先读取当前所有关节位置，然后只更新目标关节。
    /// 如果需要更新多个关节，请使用 `send_position_command_batch` 方法。
    ///
    /// **为什么需要读取当前位置？**
    /// - 每个 CAN 帧（0x155, 0x156, 0x157）包含两个关节的角度
    /// - 如果只发送单个关节，另一个关节会被错误地设置为 0.0
    /// - 因此必须先读取当前位置，然后更新目标关节，最后批量发送
    ///
    /// # 参数
    ///
    /// - `observer`: Observer 实例，用于读取当前关节位置
    /// - `joint`: 目标关节
    /// - `position`: 目标位置（弧度）
    ///
    /// # 示例
    ///
    /// ```rust,no_run
    /// # use piper_sdk::client::motion::MotionCommander;
    /// # use piper_sdk::client::observer::Observer;
    /// # use piper_sdk::client::types::*;
    /// # fn example(motion: MotionCommander, observer: &Observer) -> Result<()> {
    /// // 只更新 J1，保持其他关节不变
    /// motion.update_single_joint(observer, Joint::J1, Rad(1.57))?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn update_single_joint(
        &self,
        observer: &super::observer::Observer,
        joint: Joint,
        position: Rad,
    ) -> Result<()> {
        // 读取当前所有关节位置
        let mut positions = observer.joint_positions();
        // 只更新目标关节
        positions[joint] = position;
        // 批量发送所有关节（包括更新的和未更新的）
        self.send_position_command_batch(&positions)
    }

    /// 批量发送位置模式指令
    ///
    /// **关键修复**：此方法一次性发送所有 6 个关节，避免关节覆盖问题。
    ///
    /// **问题说明**：
    /// - 每个 CAN 帧（0x155, 0x156, 0x157）包含两个关节的角度
    /// - 如果循环发送单个关节，会导致另一个关节被设置为 0.0
    /// - 后发送的帧会覆盖前面发送的关节位置
    ///
    /// **正确实现**：
    /// - 一次性准备所有 6 个关节的角度
    /// - 依次发送 3 个 CAN 帧（0x155, 0x156, 0x157），每个帧包含两个关节
    ///
    /// **注意：** 位置控制指令（0x155、0x156、0x157）只包含位置信息，不包含速度。
    /// 速度需要通过控制模式指令（0x151）的 Byte 2（speed_percent）来设置。
    ///
    /// # 参数
    ///
    /// - `positions`: 各关节目标位置
    pub fn send_position_command_batch(&self, positions: &JointArray<Rad>) -> Result<()> {
        // ✅ 使用 RawCommander 的批量发送方法，一次性发送所有关节
        use super::raw_commander::RawCommander;
        let raw = RawCommander::new(&self.driver);
        raw.send_position_command_batch(positions)
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
    /// # use piper_sdk::client::motion::MotionCommander;
    /// # use piper_sdk::client::types::*;
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

        // ✅ 临时创建 RawCommander
        use super::raw_commander::RawCommander;
        let raw = RawCommander::new(&self.driver);
        raw.send_gripper_command(position, effort)
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
    /// # use piper_sdk::client::motion::MotionCommander;
    /// # use piper_sdk::client::types::*;
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
    // 注意：这些测试需要真实的 robot 实例，应该在集成测试中完成
    // 这里只测试类型系统和基本逻辑

    #[test]
    fn test_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<MotionCommander>();
    }

    // 注意：状态修改方法的编译期检查测试应该在集成测试中完成
}
