//! RawCommander - 内部命令发送器（简化版，移除 StateTracker 依赖）
//!
//! **设计说明：**
//! - 在引入 Type State Pattern 后，类型系统已经保证了状态正确性
//! - `Piper<Active<MitMode>>` 类型本身就保证了当前处于 MIT 模式
//! - 不再需要通过运行时的 `StateTracker` 来检查状态
//! - `RawCommander` 现在只负责"纯指令发送"，不负责状态管理
//! - 使用引用而不是 Arc，避免高频调用时的原子操作开销

use crate::client::types::*;
use crate::driver::Piper as RobotPiper;
use crate::protocol::constants::*;
use crate::protocol::control::*;

/// 原始命令发送器（简化版，移除 StateTracker 依赖）
///
/// **设计说明：**
/// - 在引入 Type State Pattern 后，类型系统已经保证了状态正确性
/// - `Piper<Active<MitMode>>` 类型本身就保证了当前处于 MIT 模式
/// - 不再需要通过运行时的 `StateTracker` 来检查状态
/// - `RawCommander` 现在只负责"纯指令发送"，不负责状态管理
/// - 使用引用而不是 Arc，避免高频调用时的原子操作开销
pub(crate) struct RawCommander<'a> {
    /// Driver 实例（使用引用，零开销）
    driver: &'a RobotPiper,
    // ✅ 移除 state_tracker: Arc<StateTracker>
    // ✅ 移除 send_lock: Mutex<()>
}

impl<'a> RawCommander<'a> {
    /// 创建新的 RawCommander
    ///
    /// **性能优化：** 使用引用而不是 Arc，避免高频调用时的 `Arc::clone` 原子操作开销
    pub(crate) fn new(driver: &'a RobotPiper) -> Self {
        RawCommander { driver }
    }

    /// 计算 MIT 控制指令的 CRC 校验值（4位）
    ///
    /// # 算法说明
    ///
    /// 根据官方 SDK 实现（`piper_protocol_v2.py`），CRC 计算方式为：
    /// ```python
    /// crc = (data[0] ^ data[1] ^ data[2] ^ data[3] ^ data[4] ^ data[5] ^ data[6]) & 0x0F
    /// ```
    ///
    /// 即：对前 7 个字节进行异或（XOR）运算，然后取低 4 位。
    ///
    /// # 参数
    ///
    /// - `data`: CAN 帧数据（前 7 字节，不包含 CRC 本身）
    /// - `_joint_index`: 关节索引（1-6），当前未使用
    ///
    /// # 返回
    ///
    /// 4 位 CRC 值（0-15）
    pub(crate) fn calculate_mit_crc(data: &[u8; 7], _joint_index: u8) -> u8 {
        // 根据官方 SDK：对前 7 个字节进行异或运算，然后取低 4 位
        let crc = data[0] ^ data[1] ^ data[2] ^ data[3] ^ data[4] ^ data[5] ^ data[6];
        crc & 0x0F
    }

    /// 发送 MIT 模式指令（无锁，实时命令）
    ///
    /// **注意：** 此方法不再检查状态，因为调用者（Type State）已经保证了上下文正确
    pub(crate) fn send_mit_command(
        &self,
        joint: Joint,
        position: Rad,
        velocity: f64,
        kp: f64,
        kd: f64,
        torque: NewtonMeter,
    ) -> Result<()> {
        let joint_index = joint.index() as u8;
        let pos_ref = position.0 as f32;
        let vel_ref = velocity as f32;
        let kp_f32 = kp as f32;
        let kd_f32 = kd as f32;
        let t_ref = torque.0 as f32;

        // 计算 CRC：先创建临时命令获取编码后的数据（前 7 字节），然后计算 CRC
        // 根据官方 SDK：CRC = (data[0] ^ data[1] ^ ... ^ data[6]) & 0x0F
        let cmd_temp =
            MitControlCommand::new(joint_index, pos_ref, vel_ref, kp_f32, kd_f32, t_ref, 0x00);
        let frame_temp = cmd_temp.to_frame();

        // 提取前 7 个字节用于 CRC 计算
        let data_for_crc = [
            frame_temp.data[0],
            frame_temp.data[1],
            frame_temp.data[2],
            frame_temp.data[3],
            frame_temp.data[4],
            frame_temp.data[5],
            frame_temp.data[6],
        ];

        // 计算 CRC
        let crc = Self::calculate_mit_crc(&data_for_crc, joint_index);

        // 使用计算出的 CRC 重新创建命令
        let cmd = MitControlCommand::new(joint_index, pos_ref, vel_ref, kp_f32, kd_f32, t_ref, crc);
        let frame = cmd.to_frame();

        // 验证 frame ID 是否正确（可选，用于调试）
        debug_assert_eq!(frame.id, ID_MIT_CONTROL_BASE + joint_index as u32);

        // ✅ 直接调用，无锁（实时命令，使用邮箱模式）
        self.driver.send_realtime(frame)?;

        Ok(())
    }

    /// 批量发送位置控制指令（一次性发送所有 6 个关节）
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
    /// - 依次发送 3 个 CAN 帧：
    ///   - 0x155: J1 + J2
    ///   - 0x156: J3 + J4
    ///   - 0x157: J5 + J6
    ///
    /// **关于速度控制：**
    /// - 位置控制指令（0x155、0x156、0x157）只包含位置信息，不包含速度
    /// - 速度需要通过控制模式指令（0x151）的 Byte 2（speed_percent）来设置
    ///
    /// # 参数
    ///
    /// - `positions`: 各关节目标位置（弧度）
    pub(crate) fn send_position_command_batch(&self, positions: &JointArray<Rad>) -> Result<()> {
        use crate::protocol::control::{JointControl12, JointControl34, JointControl56};

        // 准备所有关节的角度（度）
        let j1_deg = positions[Joint::J1].to_deg().0;
        let j2_deg = positions[Joint::J2].to_deg().0;
        let j3_deg = positions[Joint::J3].to_deg().0;
        let j4_deg = positions[Joint::J4].to_deg().0;
        let j5_deg = positions[Joint::J5].to_deg().0;
        let j6_deg = positions[Joint::J6].to_deg().0;

        // 创建 3 个 CAN 帧（使用数组，栈上分配，零堆内存分配）
        let frames = [
            JointControl12::new(j1_deg, j2_deg).to_frame(), // 0x155
            JointControl34::new(j3_deg, j4_deg).to_frame(), // 0x156
            JointControl56::new(j5_deg, j6_deg).to_frame(), // 0x157
        ];

        // 原子性发送所有帧（传入数组，内部转为 SmallVec，全程无堆分配）
        self.driver.send_realtime_package(frames)?;

        Ok(())
    }

    /// 控制夹爪（无锁）
    pub(crate) fn send_gripper_command(&self, position: f64, effort: f64) -> Result<()> {
        // ✅ 移除 state_tracker 检查（Type State 已保证状态正确）

        let position_mm = position * GRIPPER_POSITION_SCALE;
        let torque_nm = effort * GRIPPER_FORCE_SCALE;
        let enable = true;

        let cmd = GripperControlCommand::new(position_mm, torque_nm, enable);
        let frame = cmd.to_frame();

        // ✅ 直接调用，无锁
        self.driver.send_reliable(frame)?;

        Ok(())
    }

    /// 急停（无锁）
    pub(crate) fn emergency_stop(&self) -> Result<()> {
        // 急停不检查状态（安全优先）
        let cmd = EmergencyStopCommand::emergency_stop();
        let frame = cmd.to_frame();

        // ✅ 直接调用，无锁
        self.driver.send_reliable(frame)?;
        // ✅ 注意：RawCommander 是无状态的纯指令发送器，不负责更新软件状态。
        // Poison / Error 状态由调用层（Type State 状态机）在调用后进行状态转换处理。
        Ok(())
    }
}

// 确保 Send + Sync
unsafe impl<'a> Send for RawCommander<'a> {}
unsafe impl<'a> Sync for RawCommander<'a> {}

#[cfg(test)]
mod tests {
    use super::*;
    // 注意：这些测试需要真实的 robot 实例，应该在集成测试中完成

    // 注意：这些测试需要真实的 robot 实例，应该在集成测试中完成
    // 这里只测试类型系统和基本逻辑

    #[test]
    fn test_raw_commander_creation() {
        // 测试 RawCommander 可以创建（需要真实的 robot 实例）
        // 这个测试应该在集成测试中完成
    }

    #[test]
    fn test_send_sync() {
        // 注意：RawCommander 现在有生命周期参数，需要特殊处理
        // 在实际使用中，它会被包含在有生命周期的上下文中
    }

    #[test]
    fn test_calculate_mit_crc() {
        // 测试 CRC 计算：根据官方 SDK，CRC = (data[0] ^ ... ^ data[6]) & 0x0F

        // 测试用例 1：全零数据
        let data1 = [0u8; 7];
        let crc1 = RawCommander::calculate_mit_crc(&data1, 1);
        assert_eq!(crc1, 0x00, "全零数据的 CRC 应该为 0");

        // 测试用例 2：单个字节为 1
        let data2 = [1u8, 0, 0, 0, 0, 0, 0];
        let crc2 = RawCommander::calculate_mit_crc(&data2, 1);
        assert_eq!(crc2, 0x01, "单个字节为 1 的 CRC 应该为 1");

        // 测试用例 3：两个字节异或
        let data3 = [0x0F, 0xF0, 0, 0, 0, 0, 0];
        let crc3 = RawCommander::calculate_mit_crc(&data3, 1);
        assert_eq!(crc3, 0x0F, "0x0F ^ 0xF0 = 0xFF, 取低4位应该是 0x0F");
        assert_eq!(crc3, 0x0F);

        // 测试用例 4：多个字节异或
        let data4 = [0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC, 0xDE];
        let crc4 = RawCommander::calculate_mit_crc(&data4, 1);
        let expected = (0x12 ^ 0x34 ^ 0x56 ^ 0x78 ^ 0x9A ^ 0xBC ^ 0xDE) & 0x0F;
        assert_eq!(crc4, expected, "CRC 应该等于所有字节异或后的低4位");

        // 测试用例 5：验证 CRC 只返回 4 位（0-15）
        let data5 = [0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF];
        let crc5 = RawCommander::calculate_mit_crc(&data5, 1);
        assert!(crc5 <= 0x0F, "CRC 应该只返回 4 位（0-15）");
        // 全 0xFF 异或：0xFF ^ 0xFF ^ ... ^ 0xFF = 0xFF（奇数个），取低4位 = 0x0F
        assert_eq!(crc5, 0x0F);
    }
}
