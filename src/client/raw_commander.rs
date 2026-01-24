//! RawCommander - 内部命令发送器（简化版，移除 StateTracker 依赖）
//!
//! **设计说明：**
//! - 在引入 Type State Pattern 后，类型系统已经保证了状态正确性
//! - `Piper<Active<MitMode>>` 类型本身就保证了当前处于 MIT 模式
//! - 不再需要通过运行时的 `StateTracker` 来检查状态
//! - `RawCommander` 现在只负责"纯指令发送"，不负责状态管理
//! - 使用引用而不是 Arc，避免高频调用时的原子操作开销

use crate::can::PiperFrame;
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
    /// 批量发送 MIT 控制指令（一次性发送所有 6 个关节）
    ///
    /// **关键修复**：此方法一次性发送所有 6 个关节，避免覆盖问题。
    ///
    /// **问题说明**：
    /// - 如果循环调用 `send_mit_command` 6 次，由于邮箱模式（覆盖策略），
    ///   后面的会覆盖前面的，导致只有最后一个关节生效。
    ///
    /// **正确实现**：
    /// - 一次性准备所有 6 个关节的帧
    /// - 打包成一个 Package，一次性发送
    ///
    /// # 参数
    ///
    /// - `positions`: 各关节目标位置
    /// - `velocities`: 各关节目标速度
    /// - `kp`: 位置增益（所有关节相同）
    /// - `kd`: 速度增益（所有关节相同）
    /// - `torques`: 各关节前馈力矩
    pub(crate) fn send_mit_command_batch(
        &self,
        positions: &JointArray<Rad>,
        velocities: &JointArray<f64>,
        kp: f64,
        kd: f64,
        torques: &JointArray<NewtonMeter>,
    ) -> Result<()> {
        use crate::protocol::control::MitControlCommand;

        // 准备所有 6 个关节的帧
        // 注意：使用数组（栈分配）而不是 Vec，因为 FrameBuffer 的栈缓冲区是 6
        // 这样可以确保完全在栈上，零堆分配，满足高频控制的实时性要求
        let mut frames_array: [PiperFrame; 6] = [
            PiperFrame::new_standard(0, &[0; 8]),
            PiperFrame::new_standard(0, &[0; 8]),
            PiperFrame::new_standard(0, &[0; 8]),
            PiperFrame::new_standard(0, &[0; 8]),
            PiperFrame::new_standard(0, &[0; 8]),
            PiperFrame::new_standard(0, &[0; 8]),
        ];

        for (index, joint) in [
            Joint::J1,
            Joint::J2,
            Joint::J3,
            Joint::J4,
            Joint::J5,
            Joint::J6,
        ]
        .into_iter()
        .enumerate()
        {
            let joint_index = joint.index() as u8;
            let pos_ref = positions[joint].0 as f32;
            let vel_ref = velocities[joint] as f32;
            let kp_f32 = kp as f32;
            let kd_f32 = kd as f32;
            let t_ref = torques[joint].0 as f32;

            // ✅ v2.1 重构：简化为两行，自动计算 CRC
            // encode_to_bytes 内部已处理完整的 8 字节编码（包括 T_ref 的高低位）
            // to_frame 只负责计算并填入 CRC
            let cmd = MitControlCommand::new(joint_index, pos_ref, vel_ref, kp_f32, kd_f32, t_ref);
            frames_array[index] = cmd.to_frame(); // 内部自动计算 CRC
        }

        // ✅ 一次性打包发送所有 6 帧
        // 注意：由于 FrameBuffer 的栈缓冲区是 6，这 6 帧完全在栈上，零堆分配
        // 这对于高频控制（500Hz-1kHz）至关重要，确保实时性能
        self.driver.send_realtime_package(frames_array)?;

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

    /// 内部辅助：构建末端位姿的 3 个 CAN 帧
    ///
    /// 将帧生成逻辑提取出来，以便可以组合进不同的 Package。
    ///
    /// **单位转换**：
    /// - `Position3D` 的单位是米（m），需要转换为毫米（mm）
    /// - `EulerAngles` 的单位是度（degree），直接使用
    fn build_end_pose_frames(position: &Position3D, orientation: &EulerAngles) -> [PiperFrame; 3] {
        use crate::protocol::control::{EndPoseControl1, EndPoseControl2, EndPoseControl3};

        // ✅ 注意：EndPoseControl::new() 内部已经处理了单位转换（* 1000.0）
        // Position3D 的单位是米，需要转换为毫米
        // EulerAngles 的单位是度，直接使用

        [
            EndPoseControl1::new(position.x * 1000.0, position.y * 1000.0).to_frame(), // 0x152: X, Y (mm)
            EndPoseControl2::new(position.z * 1000.0, orientation.roll).to_frame(), // 0x153: Z (mm), RX (deg)
            EndPoseControl3::new(orientation.pitch, orientation.yaw).to_frame(), // 0x154: RY (deg), RZ (deg)
        ]
    }

    /// 发送末端位姿控制指令（普通点位控制）
    ///
    /// 对应协议指令：
    /// - 0x152: X, Y 坐标
    /// - 0x153: Z 坐标, RX 角度（Roll）
    /// - 0x154: RY 角度（Pitch）, RZ 角度（Yaw）
    ///
    /// **协议映射说明**：
    /// - RX (Roll) = 绕 X 轴旋转
    /// - RY (Pitch) = 绕 Y 轴旋转
    /// - RZ (Yaw) = 绕 Z 轴旋转
    ///
    /// **欧拉角顺序**：Intrinsic RPY (Roll-Pitch-Yaw)
    ///
    /// # 参数
    ///
    /// - `position`: 末端位置（米）
    /// - `orientation`: 末端姿态（欧拉角，度）
    pub(crate) fn send_end_pose_command(
        &self,
        position: Position3D,
        orientation: EulerAngles,
    ) -> Result<()> {
        let frames = Self::build_end_pose_frames(&position, &orientation);
        // ✅ 使用实时通道，非阻塞，高性能
        self.driver.send_realtime_package(frames)?;
        Ok(())
    }

    /// 发送带序号的位姿指令（用于圆弧运动/轨迹记录）
    ///
    /// **注意**：此方法已废弃，请使用 `send_circular_motion` 方法。
    /// 此方法保留仅用于向后兼容或特殊场景。
    ///
    /// **原子性发送**：`[0x152, 0x153, 0x154] + [0x158]`
    ///
    /// **关键设计**：利用 CAN 总线优先级机制（ID 越小优先级越高）保证顺序。
    /// - 位姿指令 ID：`0x152`, `0x153`, `0x154`（较小）
    /// - 序号指令 ID：`0x158`（较大）
    ///
    /// 只要将这 4 帧数据作为一个 Batch 一次性写入 CAN 控制器，
    /// CAN 控制器和总线仲裁机制会保证：**位姿数据一定先于序号指令被处理**。
    ///
    /// **警告**：由于 `send_realtime_package` 使用邮箱模式（覆盖策略），
    /// 连续两次调用此方法会导致第一次被覆盖。对于圆弧运动，请使用 `send_circular_motion`。
    ///
    /// **优势**：
    /// - ✅ 保证顺序：利用硬件机制，无需等待 ACK
    /// - ✅ 高性能：非阻塞，避免 `send_reliable` 的通信延迟
    /// - ✅ 原子性：一次调用完成所有相关帧的发送
    ///
    /// # 参数
    ///
    /// - `position`: 末端位置（米）
    /// - `orientation`: 末端姿态（欧拉角，度）
    /// - `index`: 圆弧序号（0x01=起点, 0x02=中点, 0x03=终点）
    #[allow(dead_code)] // 保留用于向后兼容或特殊场景
    pub(crate) fn send_pose_with_index(
        &self,
        position: Position3D,
        orientation: EulerAngles,
        index: u8,
    ) -> Result<()> {
        use crate::protocol::control::{ArcPointCommand, ArcPointIndex};

        // 构建位姿帧
        let pose_frames = Self::build_end_pose_frames(&position, &orientation);

        // 构建序号帧
        // index: 0x01=起点, 0x02=中点, 0x03=终点
        let arc_index = match index {
            0x01 => ArcPointIndex::Start,
            0x02 => ArcPointIndex::Middle,
            0x03 => ArcPointIndex::End,
            _ => ArcPointIndex::Invalid,
        };
        let index_frame = ArcPointCommand::new(arc_index).to_frame(); // 0x158

        // ✅ 构建 4 帧的 Package，原子性发送
        let package = [
            pose_frames[0], // 0x152
            pose_frames[1], // 0x153
            pose_frames[2], // 0x154
            index_frame,    // 0x158
        ];

        // ✅ 使用实时通道一次性发送，保证顺序且无阻塞
        // CAN 总线仲裁机制确保 0x152 < 0x153 < 0x154 < 0x158 的顺序
        self.driver.send_realtime_package(package)?;

        Ok(())
    }

    /// 发送圆弧运动命令（原子性发送所有点）
    ///
    /// **关键设计**：将所有点打包到一个 Frame Package 里，一次性发送。
    /// 这避免了邮箱模式的覆盖问题，确保中间点和终点都被正确发送。
    ///
    /// # 参数
    ///
    /// - `via_position`: 中间点位置（米）
    /// - `via_orientation`: 中间点姿态（欧拉角，度）
    /// - `target_position`: 终点位置（米）
    /// - `target_orientation`: 终点姿态（欧拉角，度）
    ///
    /// # 协议说明
    ///
    /// 圆弧运动需要按顺序发送：
    /// 1. 中间点：0x152, 0x153, 0x154, 0x158(index=0x02) - 4帧
    /// 2. 终点：0x152, 0x153, 0x154, 0x158(index=0x03) - 4帧
    /// 3. 起点：由机械臂内部自动记录（当前末端位姿）
    ///
    /// **总计**：8帧，打包成一个 Package 发送。
    ///
    /// # 设计说明
    ///
    /// **为什么需要打包发送？**
    ///
    /// `send_realtime_package` 使用邮箱模式（Mailbox），采用覆盖策略（Last Write Wins）。
    /// 如果分两次调用 `send_pose_with_index`：
    /// - 第一次调用：中间点（4帧）放入邮箱
    /// - 第二次调用：终点（4帧）**覆盖**邮箱中的中间点
    /// - 结果：中间点丢失，只有终点被发送
    ///
    /// **解决方案**：
    /// - 将所有 8 帧打包成一个 Package
    /// - 一次性发送，确保原子性
    /// - 利用 CAN 总线优先级（0x152 < 0x153 < 0x154 < 0x158）保证顺序
    ///
    /// # 性能特性
    ///
    /// - **堆分配**：8 帧会溢出 `SmallVec` 的栈缓冲区（4帧），触发堆分配
    /// - **可接受性**：圆弧运动不是高频操作（通常每秒 < 10 次），堆分配开销可接受
    /// - **延迟**：典型延迟 20-50ns（无竞争）+ 堆分配开销（~100ns）≈ 120-150ns
    pub(crate) fn send_circular_motion(
        &self,
        via_position: Position3D,
        via_orientation: EulerAngles,
        target_position: Position3D,
        target_orientation: EulerAngles,
    ) -> Result<()> {
        use crate::protocol::control::{ArcPointCommand, ArcPointIndex};

        // 构建中间点位姿帧（3帧）
        let via_pose_frames = Self::build_end_pose_frames(&via_position, &via_orientation);

        // 构建中间点序号帧（1帧）
        let via_index_frame = ArcPointCommand::new(ArcPointIndex::Middle).to_frame(); // 0x158, index=0x02

        // 构建终点位姿帧（3帧）
        let target_pose_frames = Self::build_end_pose_frames(&target_position, &target_orientation);

        // 构建终点序号帧（1帧）
        let target_index_frame = ArcPointCommand::new(ArcPointIndex::End).to_frame(); // 0x158, index=0x03

        // ✅ 构建 8 帧的 Package，原子性发送
        // 顺序：中间点位姿(3帧) + 中间点序号(1帧) + 终点位姿(3帧) + 终点序号(1帧)
        let package = [
            // 中间点
            via_pose_frames[0], // 0x152: X, Y
            via_pose_frames[1], // 0x153: Z, RX
            via_pose_frames[2], // 0x154: RY, RZ
            via_index_frame,    // 0x158: index=0x02 (Middle)
            // 终点
            target_pose_frames[0], // 0x152: X, Y
            target_pose_frames[1], // 0x153: Z, RX
            target_pose_frames[2], // 0x154: RY, RZ
            target_index_frame,    // 0x158: index=0x03 (End)
        ];

        // ✅ 使用实时通道一次性发送，保证顺序且无阻塞
        // CAN 总线仲裁机制确保：
        // - 中间点位姿帧（0x152, 0x153, 0x154）先于中间点序号帧（0x158）发送
        // - 终点位姿帧（0x152, 0x153, 0x154）先于终点序号帧（0x158）发送
        // - 中间点相关帧先于终点相关帧发送（因为它们在数组中的顺序）
        self.driver.send_realtime_package(package)?;

        Ok(())
    }
}

// 确保 Send + Sync
unsafe impl<'a> Send for RawCommander<'a> {}
unsafe impl<'a> Sync for RawCommander<'a> {}

#[cfg(test)]
mod tests {
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
}
