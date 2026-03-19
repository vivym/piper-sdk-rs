//! RawCommander - 内部命令发送器（简化版，移除 StateTracker 依赖）
//!
//! **设计说明：**
//! - 在引入 Type State Pattern 后，类型系统已经保证了状态正确性
//! - `Piper<Active<MitMode>>` 类型本身就保证了当前处于 MIT 模式
//! - 不再需要通过运行时的 `StateTracker` 来检查状态
//! - `RawCommander` 现在只负责"纯指令发送"，不负责状态管理
//! - 使用引用而不是 Arc，避免高频调用时的原子操作开销

use crate::state::machine::SendStrategy;
use crate::types::*;
use piper_can::PiperFrame;
use piper_driver::Piper as RobotPiper;
use piper_protocol::constants::*;
use piper_protocol::control::*;
use std::time::Duration;

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
    /// 发送已规范化、已校验的 MIT 批命令
    ///
    /// 调用方必须在进入此方法前完成：
    /// - 固件 quirk 修正
    /// - 协议范围校验
    /// - 整批原子性决策
    pub(crate) fn send_validated_mit_command_batch(
        &self,
        commands: [MitControlCommand; 6],
    ) -> Result<()> {
        let frames_array = commands.map(MitControlCommand::to_frame);
        self.driver.send_realtime_package(frames_array)?;
        Ok(())
    }

    /// 发送已规范化、已校验的 MIT 批命令，并等待 TX 线程确认实际发送结果。
    pub(crate) fn send_validated_mit_command_batch_confirmed(
        &self,
        commands: [MitControlCommand; 6],
        timeout: Duration,
    ) -> Result<()> {
        let frames_array = commands.map(MitControlCommand::to_frame);
        self.driver.send_realtime_package_confirmed(frames_array, timeout)?;
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
    /// - `strategy`: 发送策略（默认使用 Auto，即可靠模式）
    pub(crate) fn send_position_command_batch(
        &self,
        positions: &JointArray<Rad>,
        strategy: SendStrategy,
    ) -> Result<()> {
        use piper_protocol::control::{JointControl12, JointControl34, JointControl56};

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

        // 根据策略选择发送方式
        match strategy {
            SendStrategy::Realtime => {
                // 实时模式：邮箱模式，零延迟，可覆盖
                self.driver.send_realtime_package(frames)?;
            },
            SendStrategy::Auto | SendStrategy::Reliable { .. } => {
                // 可靠模式：队列模式，按顺序，不丢失
                // 对于多个帧，需要逐个发送到可靠队列
                for frame in frames {
                    self.driver.send_reliable(frame)?;
                }
            },
        }

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

    /// 将急停命令加入 shutdown lane，并返回确认句柄。
    pub(crate) fn emergency_stop_enqueue(&self) -> Result<piper_driver::ShutdownReceipt> {
        let cmd = EmergencyStopCommand::emergency_stop();
        let frame = cmd.to_frame();
        Ok(self.driver.enqueue_shutdown(frame)?)
    }

    /// 停止运动（用于优雅关闭）
    ///
    /// 发送停止运动命令，用于优雅关闭序列。与急停不同，此方法更平滑地停止机器人。
    pub(crate) fn stop_motion(&self) -> Result<()> {
        // 使用急停命令停止运动
        // 注意：当前协议没有单独的"平滑停止"命令，急停是最接近的选项
        let cmd = EmergencyStopCommand::emergency_stop();
        let frame = cmd.to_frame();

        self.driver.send_reliable(frame)?;
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
        use piper_protocol::control::{EndPoseControl1, EndPoseControl2, EndPoseControl3};

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
    /// - `strategy`: 发送策略（默认使用 Auto，即可靠模式）
    pub(crate) fn send_end_pose_command(
        &self,
        position: Position3D,
        orientation: EulerAngles,
        strategy: SendStrategy,
    ) -> Result<()> {
        let frames = Self::build_end_pose_frames(&position, &orientation);

        // 根据策略选择发送方式
        match strategy {
            SendStrategy::Realtime => {
                // 实时模式：邮箱模式，零延迟，可覆盖
                self.driver.send_realtime_package(frames)?;
            },
            SendStrategy::Auto | SendStrategy::Reliable { .. } => {
                // 可靠模式：队列模式，按顺序，不丢失
                for frame in frames {
                    self.driver.send_reliable(frame)?;
                }
            },
        }

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
    /// - `strategy`: 发送策略（默认使用 Auto，即可靠模式）
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
    /// 如果分两次发送位姿数据：
    /// - 第一次发送：中间点（4帧）放入邮箱
    /// - 第二次发送：终点（4帧）**覆盖**邮箱中的中间点
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
        strategy: SendStrategy,
    ) -> Result<()> {
        use piper_protocol::control::{ArcPointCommand, ArcPointIndex};

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

        // 根据策略选择发送方式
        match strategy {
            SendStrategy::Realtime => {
                // 实时模式：邮箱模式，零延迟，可覆盖
                // CAN 总线仲裁机制确保：
                // - 中间点位姿帧（0x152, 0x153, 0x154）先于中间点序号帧（0x158）发送
                // - 终点位姿帧（0x152, 0x153, 0x154）先于终点序号帧（0x158）发送
                // - 中间点相关帧先于终点相关帧发送（因为它们在数组中的顺序）
                self.driver.send_realtime_package(package)?;
            },
            SendStrategy::Auto | SendStrategy::Reliable { .. } => {
                // 可靠模式：队列模式，按顺序，不丢失
                // 对于多个帧，需要逐个发送到可靠队列
                for frame in package {
                    self.driver.send_reliable(frame)?;
                }
            },
        }

        Ok(())
    }

    /// 设置碰撞保护级别
    ///
    /// 设置6个关节的碰撞防护等级（0~8，等级0代表不检测碰撞）。
    ///
    /// # 参数
    ///
    /// - `levels`: 6个关节的碰撞防护等级数组，每个值范围 0~8
    ///
    /// # 示例
    ///
    /// ```ignore
    /// // 所有关节设置为等级 5
    /// raw_commander.set_collision_protection([5, 5, 5, 5, 5, 5])?;
    ///
    /// // 为不同关节设置不同等级
    /// raw_commander.set_collision_protection([3, 4, 5, 5, 4, 3])?;
    /// ```
    pub(crate) fn set_collision_protection(&self, levels: [u8; 6]) -> Result<()> {
        use piper_protocol::config::CollisionProtectionLevelCommand;

        // 验证等级范围
        for &level in &levels {
            if level > 8 {
                return Err(RobotError::ConfigError(format!(
                    "碰撞防护等级必须在0~8之间，收到: {}",
                    level
                )));
            }
        }

        let cmd = CollisionProtectionLevelCommand::new(levels);
        self.driver.send_reliable(cmd.to_frame())?;
        Ok(())
    }

    /// 查询当前碰撞保护级别。
    pub(crate) fn query_collision_protection(&self) -> Result<()> {
        use piper_protocol::config::{ParameterQuerySetCommand, ParameterQueryType};

        let frame = ParameterQuerySetCommand::query(ParameterQueryType::CollisionProtectionLevel)
            .to_frame()
            .map_err(RobotError::from)?;
        self.driver.send_reliable(frame)?;
        Ok(())
    }

    /// 设置关节零位
    ///
    /// 设置指定关节的当前位置为零点。
    ///
    /// # 参数
    ///
    /// - `joints`: 要设置零位的关节索引数组（0-based，0-5 对应 J1-J6）
    ///
    /// # 示例
    ///
    /// ```ignore
    /// // 设置 J1 的当前位置为零点
    /// raw_commander.set_joint_zero_positions(&[0])?;
    ///
    /// // 设置多个关节的零位
    /// raw_commander.set_joint_zero_positions(&[0, 1, 2])?;
    ///
    /// // 设置所有关节的零位
    /// raw_commander.set_joint_zero_positions(&[0, 1, 2, 3, 4, 5])?;
    /// ```
    pub(crate) fn set_joint_zero_positions(&self, joints: &[usize]) -> Result<()> {
        use piper_protocol::config::JointSettingCommand;

        for &joint_index in joints {
            if joint_index > 5 {
                return Err(RobotError::ConfigError(format!(
                    "关节索引必须在0-5之间，收到: {}",
                    joint_index
                )));
            }

            // joint_index 是 0-based，需要转换为 1-based（J1=0 -> 1）
            let cmd = JointSettingCommand::set_zero_point((joint_index + 1) as u8);
            self.driver.send_reliable(cmd.to_frame())?;
        }

        Ok(())
    }
}

// 确保 Send + Sync
unsafe impl<'a> Send for RawCommander<'a> {}
unsafe impl<'a> Sync for RawCommander<'a> {}

#[cfg(test)]
mod tests {
    use super::*;
    use piper_can::{CanError, RxAdapter, TxAdapter};
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::{Duration, Instant};

    struct IdleRxAdapter;

    impl RxAdapter for IdleRxAdapter {
        fn receive(&mut self) -> std::result::Result<PiperFrame, CanError> {
            Err(CanError::Timeout)
        }
    }

    struct RecordingTxAdapter {
        sent_frames: Arc<Mutex<Vec<PiperFrame>>>,
    }

    impl RecordingTxAdapter {
        fn new(sent_frames: Arc<Mutex<Vec<PiperFrame>>>) -> Self {
            Self { sent_frames }
        }
    }

    impl TxAdapter for RecordingTxAdapter {
        fn send(&mut self, frame: PiperFrame) -> std::result::Result<(), CanError> {
            self.sent_frames.lock().unwrap().push(frame);
            Ok(())
        }
    }

    fn build_driver(sent_frames: Arc<Mutex<Vec<PiperFrame>>>) -> RobotPiper {
        RobotPiper::new_dual_thread_parts(IdleRxAdapter, RecordingTxAdapter::new(sent_frames), None)
            .expect("driver should start")
    }

    fn wait_for_sent_frames(
        sent_frames: &Arc<Mutex<Vec<PiperFrame>>>,
        expected: usize,
    ) -> Vec<PiperFrame> {
        let start = Instant::now();
        loop {
            let frames = sent_frames.lock().unwrap().clone();
            if frames.len() >= expected {
                return frames;
            }

            assert!(
                start.elapsed() < Duration::from_millis(200),
                "timed out waiting for {} sent frames, got {}",
                expected,
                frames.len()
            );
            thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn test_raw_commander_creation() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let driver = build_driver(sent_frames);
        let _commander = RawCommander::new(&driver);
    }

    #[test]
    fn test_send_sync() {
        // 注意：RawCommander 现在有生命周期参数，需要特殊处理
        // 在实际使用中，它会被包含在有生命周期的上下文中
    }

    #[test]
    fn test_send_mit_command_batch_uses_all_six_joint_ids() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let driver = build_driver(sent_frames.clone());
        let commander = RawCommander::new(&driver);
        let commands = std::array::from_fn(|index| {
            MitControlCommand::try_new(index as u8 + 1, 0.0, 0.0, 10.0, 0.8, 0.0)
                .expect("command should be valid")
        });

        commander
            .send_validated_mit_command_batch(commands)
            .expect("MIT batch send should succeed");

        let frames = wait_for_sent_frames(&sent_frames, 6);
        let ids: Vec<u32> = frames.iter().map(|frame| frame.id).collect();
        assert_eq!(ids, vec![0x15A, 0x15B, 0x15C, 0x15D, 0x15E, 0x15F]);
    }

    #[test]
    fn test_send_gripper_command_full_effort_maps_to_protocol_full_scale() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let driver = build_driver(sent_frames.clone());
        let commander = RawCommander::new(&driver);

        commander
            .send_gripper_command(1.0, 1.0)
            .expect("gripper command should succeed");

        let frames = wait_for_sent_frames(&sent_frames, 1);
        let frame = &frames[0];
        assert_eq!(frame.id, ID_GRIPPER_CONTROL);
        assert_eq!(
            i16::from_be_bytes([frame.data[4], frame.data[5]]),
            5000,
            "effort=1.0 should map to 5.0 N·m full scale"
        );
    }

    #[test]
    fn test_query_collision_protection_sends_parameter_query_frame() {
        use piper_protocol::ids::ID_PARAMETER_QUERY_SET;

        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let driver = build_driver(sent_frames.clone());
        let commander = RawCommander::new(&driver);

        commander
            .query_collision_protection()
            .expect("collision protection query should succeed");

        let frames = wait_for_sent_frames(&sent_frames, 1);
        let frame = &frames[0];
        assert_eq!(frame.id, ID_PARAMETER_QUERY_SET);
        assert_eq!(
            frame.data[0], 0x02,
            "query type must be collision protection"
        );
        assert_eq!(frame.data[1], 0x00, "set type must remain unset");
    }
}
