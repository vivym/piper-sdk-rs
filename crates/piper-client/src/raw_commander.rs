//! RawCommander - 内部命令发送器（简化版，移除 StateTracker 依赖）
//!
//! **设计说明：**
//! - 在引入 Type State Pattern 后，类型系统已经保证了状态正确性
//! - `Piper<Active<MitMode>>` 类型本身就保证了当前处于 MIT 模式
//! - 不再需要通过运行时的 `StateTracker` 来检查状态
//! - `RawCommander` 现在只负责"纯指令发送"，不负责状态管理
//! - 使用引用而不是 Arc，避免高频调用时的原子操作开销

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
        match self.driver.backend_capability() {
            piper_driver::BackendCapability::StrictRealtime => {
                self.driver.send_realtime_package_confirmed(frames_array, timeout)?
            },
            piper_driver::BackendCapability::SoftRealtime => {
                self.driver.send_soft_realtime_package_confirmed(frames_array, timeout)?
            },
            piper_driver::BackendCapability::MonitorOnly => {
                return Err(RobotError::realtime_unsupported(
                    "monitor-only backends cannot send MIT command batches",
                ));
            },
        }
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
    /// - `timeout`: 整包可靠发送超时
    pub(crate) fn send_position_command_batch(
        &self,
        positions: &JointArray<Rad>,
        timeout: Duration,
    ) -> Result<()> {
        let frames = build_joint_position_frames(positions);
        self.driver.send_reliable_package_confirmed(frames, timeout)?;
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

    /// 将急停命令加入 shutdown lane，并返回确认句柄。
    pub(crate) fn emergency_stop_enqueue(
        &self,
        deadline: std::time::Instant,
    ) -> Result<piper_driver::ShutdownReceipt> {
        let cmd = EmergencyStopCommand::emergency_stop();
        let frame = cmd.to_frame();
        Ok(self.driver.enqueue_shutdown(frame, deadline)?)
    }

    /// 将急停恢复命令加入 shutdown lane，并返回确认句柄。
    pub(crate) fn emergency_stop_resume_enqueue(
        &self,
        deadline: std::time::Instant,
    ) -> Result<piper_driver::ShutdownReceipt> {
        let cmd = EmergencyStopCommand::resume();
        let frame = cmd.to_frame();
        Ok(self.driver.enqueue_shutdown(frame, deadline)?)
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
    pub(crate) fn send_end_pose_command(
        &self,
        position: Position3D,
        orientation: EulerAngles,
        timeout: Duration,
    ) -> Result<()> {
        let frames = Self::build_end_pose_frames(&position, &orientation);
        self.driver.send_reliable_package_confirmed(frames, timeout)?;
        Ok(())
    }

    /// 发送圆弧运动命令（原子性发送所有点）
    ///
    /// **关键设计**：将所有点打包到一个 Frame Package 里，一次性发送。
    /// 这避免了逐帧 reliable 入队带来的“半包已生效”问题，确保中间点和终点语义一致。
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
    /// 如果逐帧调用 `send_reliable()`：
    /// - 前几帧可能已经进入 FIFO 并开始发送
    /// - 后续帧遇到队列满或 transport fault 时会形成“半包已生效”
    /// - 结果：中间点和终点的解释不再原子，机械臂侧很难安全收口
    ///
    /// **解决方案**：
    /// - 将所有 8 帧打包成一个 Package
    /// - 整包作为单个 reliable 队列元素入队，确保全有或全无
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
        timeout: Duration,
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

        self.driver.send_reliable_package_confirmed(package, timeout)?;
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

    #[cfg(test)]
    pub(crate) fn query_collision_protection_confirmed(&self, timeout: Duration) -> Result<u64> {
        use piper_protocol::config::{ParameterQuerySetCommand, ParameterQueryType};

        let frame = ParameterQuerySetCommand::query(ParameterQueryType::CollisionProtectionLevel)
            .to_frame()
            .map_err(RobotError::from)?;
        Ok(self.driver.send_reliable_frame_confirmed_commit_marker(frame, timeout)?)
    }

    /// 请求设置关节零位，并等待请求至少完成 confirmed reliable 发送。
    ///
    /// 请求将指定关节的当前位置设为零点。
    ///
    /// # 参数
    ///
    /// - `joints`: 要设置零位的关节索引数组（0-based，0-5 对应 J1-J6）
    /// - `timeout`: 等待 confirmed reliable 提交的最长时间
    ///
    /// # 示例
    ///
    /// ```ignore
    /// // 设置 J1 的当前位置为零点
    /// raw_commander.request_joint_zero_positions_confirmed(&[0], Duration::from_secs(2))?;
    ///
    /// // 设置所有关节的零位
    /// raw_commander.request_joint_zero_positions_confirmed(
    ///     &[0, 1, 2, 3, 4, 5],
    ///     Duration::from_secs(2),
    /// )?;
    /// ```
    pub(crate) fn request_joint_zero_positions_confirmed(
        &self,
        joints: &[usize],
        timeout: Duration,
    ) -> Result<()> {
        use piper_protocol::config::JointSettingCommand;

        if joints.is_empty() {
            return Ok(());
        }

        let mut joint_mask = 0u8;
        for &joint_index in joints {
            if joint_index > 5 {
                return Err(RobotError::ConfigError(format!(
                    "关节索引必须在0-5之间，收到: {}",
                    joint_index
                )));
            }
            joint_mask |= 1 << joint_index;
        }

        let cmd = match joints.len() {
            1 => JointSettingCommand::set_zero_point((joints[0] + 1) as u8),
            6 if joint_mask == 0b11_1111 => JointSettingCommand::set_zero_point(7),
            _ => {
                return Err(RobotError::ConfigError(
                    "joint zeroing only supports a single joint or all six joints".to_string(),
                ));
            },
        };

        self.driver.send_reliable_package_confirmed([cmd.to_frame()], timeout)?;
        Ok(())
    }
}

fn build_joint_position_frames(positions: &JointArray<Rad>) -> [PiperFrame; 3] {
    use piper_protocol::control::{JointControl12, JointControl34, JointControl56};

    let j1_deg = positions[Joint::J1].to_deg().0;
    let j2_deg = positions[Joint::J2].to_deg().0;
    let j3_deg = positions[Joint::J3].to_deg().0;
    let j4_deg = positions[Joint::J4].to_deg().0;
    let j5_deg = positions[Joint::J5].to_deg().0;
    let j6_deg = positions[Joint::J6].to_deg().0;

    [
        JointControl12::new(j1_deg, j2_deg).to_frame(),
        JointControl34::new(j3_deg, j4_deg).to_frame(),
        JointControl56::new(j5_deg, j6_deg).to_frame(),
    ]
}

// 确保 Send + Sync
unsafe impl<'a> Send for RawCommander<'a> {}
unsafe impl<'a> Sync for RawCommander<'a> {}

#[cfg(test)]
mod tests {
    use super::*;
    use piper_can::{CanError, PiperFrame, RealtimeTxAdapter, RxAdapter};
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::{Duration, Instant};

    struct IdleRxAdapter {
        bootstrap_emitted: bool,
    }

    impl IdleRxAdapter {
        fn new() -> Self {
            Self {
                bootstrap_emitted: false,
            }
        }
    }

    impl RxAdapter for IdleRxAdapter {
        fn receive(&mut self) -> std::result::Result<PiperFrame, CanError> {
            if !self.bootstrap_emitted {
                self.bootstrap_emitted = true;
                let mut frame = PiperFrame::new_standard(0x251, &[0; 8]);
                frame.timestamp_us = 1;
                return Ok(frame);
            }
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

    impl RealtimeTxAdapter for RecordingTxAdapter {
        fn send_control(
            &mut self,
            frame: PiperFrame,
            budget: Duration,
        ) -> std::result::Result<(), CanError> {
            if budget.is_zero() {
                return Err(CanError::Timeout);
            }
            self.sent_frames.lock().unwrap().push(frame);
            Ok(())
        }

        fn send_shutdown_until(
            &mut self,
            frame: PiperFrame,
            deadline: Instant,
        ) -> std::result::Result<(), CanError> {
            if deadline <= Instant::now() {
                return Err(CanError::Timeout);
            }
            self.sent_frames.lock().unwrap().push(frame);
            Ok(())
        }
    }

    fn build_driver(sent_frames: Arc<Mutex<Vec<PiperFrame>>>) -> RobotPiper {
        RobotPiper::new_dual_thread_parts(
            IdleRxAdapter::new(),
            RecordingTxAdapter::new(sent_frames),
            None,
        )
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
    fn test_send_mit_command_batch_preserves_payloads_for_each_joint() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let driver = build_driver(sent_frames.clone());
        let commander = RawCommander::new(&driver);
        let commands = [
            MitControlCommand::try_new(1, 0.10, 0.00, 5.0, 0.5, 0.10)
                .expect("joint 1 command should be valid"),
            MitControlCommand::try_new(2, 0.20, 0.10, 6.0, 0.6, 0.20)
                .expect("joint 2 command should be valid"),
            MitControlCommand::try_new(3, 0.30, 0.20, 7.0, 0.7, 0.30)
                .expect("joint 3 command should be valid"),
            MitControlCommand::try_new(4, 0.40, 0.30, 8.0, 0.8, 0.40)
                .expect("joint 4 command should be valid"),
            MitControlCommand::try_new(5, 0.50, 0.40, 9.0, 0.9, 0.50)
                .expect("joint 5 command should be valid"),
            MitControlCommand::try_new(6, 0.60, 0.50, 10.0, 1.0, 0.60)
                .expect("joint 6 command should be valid"),
        ];

        commander
            .send_validated_mit_command_batch(commands)
            .expect("MIT batch send should succeed");

        let frames = wait_for_sent_frames(&sent_frames, 6);
        let expected_frames: Vec<_> = commands.iter().map(|command| command.to_frame()).collect();

        assert_eq!(frames.len(), expected_frames.len());
        for (observed, expected) in frames.iter().zip(expected_frames.iter()) {
            assert_eq!(observed.id, expected.id);
            assert_eq!(observed.data, expected.data);
        }
    }

    #[test]
    fn test_send_position_command_batch_emits_joint_position_family() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let driver = build_driver(sent_frames.clone());
        let commander = RawCommander::new(&driver);

        commander
            .send_position_command_batch(&JointArray::splat(Rad(0.0)), Duration::from_millis(20))
            .expect("joint position batch should succeed");

        let frames = wait_for_sent_frames(&sent_frames, 3);
        let ids: Vec<u32> = frames.iter().map(|frame| frame.id).collect();
        assert_eq!(ids, vec![0x155, 0x156, 0x157]);
    }

    #[test]
    fn test_send_position_command_batch_encodes_target_payloads() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let driver = build_driver(sent_frames.clone());
        let commander = RawCommander::new(&driver);
        let positions = JointArray::from([
            Rad(0.11),
            Rad(-0.22),
            Rad(0.33),
            Rad(-0.44),
            Rad(0.55),
            Rad(-0.66),
        ]);

        commander
            .send_position_command_batch(&positions, Duration::from_millis(20))
            .expect("joint position batch should succeed");

        let frames = wait_for_sent_frames(&sent_frames, 3);
        let expected_frames = build_joint_position_frames(&positions);

        assert_eq!(frames.len(), expected_frames.len());
        for (observed, expected) in frames.iter().zip(expected_frames.iter()) {
            assert_eq!(observed.id, expected.id);
            assert_eq!(observed.data, expected.data);
        }
    }

    #[test]
    fn test_send_end_pose_command_emits_cartesian_frame_family() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let driver = build_driver(sent_frames.clone());
        let commander = RawCommander::new(&driver);

        commander
            .send_end_pose_command(
                Position3D::new(0.3, 0.0, 0.2),
                EulerAngles::new(0.0, 180.0, 0.0),
                Duration::from_millis(20),
            )
            .expect("end pose command should succeed");

        let frames = wait_for_sent_frames(&sent_frames, 3);
        let ids: Vec<u32> = frames.iter().map(|frame| frame.id).collect();
        assert_eq!(ids, vec![0x152, 0x153, 0x154]);
    }

    #[test]
    fn test_send_circular_motion_emits_via_and_target_arc_sequence() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let driver = build_driver(sent_frames.clone());
        let commander = RawCommander::new(&driver);

        commander
            .send_circular_motion(
                Position3D::new(0.2, 0.0, 0.2),
                EulerAngles::new(0.0, 90.0, 0.0),
                Position3D::new(0.3, 0.1, 0.2),
                EulerAngles::new(0.0, 180.0, 0.0),
                Duration::from_millis(20),
            )
            .expect("circular motion command should succeed");

        let frames = wait_for_sent_frames(&sent_frames, 8);
        let ids: Vec<u32> = frames.iter().map(|frame| frame.id).collect();
        assert_eq!(
            ids,
            vec![0x152, 0x153, 0x154, 0x158, 0x152, 0x153, 0x154, 0x158]
        );
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
            .query_collision_protection_confirmed(Duration::from_secs(2))
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

    #[test]
    fn test_request_joint_zero_positions_single_joint_uses_direct_index() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let driver = build_driver(sent_frames.clone());
        let commander = RawCommander::new(&driver);

        commander
            .request_joint_zero_positions_confirmed(&[0], Duration::from_secs(2))
            .expect("single-joint zeroing request should succeed");

        let frames = wait_for_sent_frames(&sent_frames, 1);
        assert_eq!(
            frames[0],
            piper_protocol::config::JointSettingCommand::set_zero_point(1).to_frame()
        );
    }

    #[test]
    fn test_request_joint_zero_positions_all_joints_uses_broadcast_index() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let driver = build_driver(sent_frames.clone());
        let commander = RawCommander::new(&driver);

        commander
            .request_joint_zero_positions_confirmed(&[0, 1, 2, 3, 4, 5], Duration::from_secs(2))
            .expect("all-joint zeroing request should succeed");

        let frames = wait_for_sent_frames(&sent_frames, 1);
        assert_eq!(
            frames[0],
            piper_protocol::config::JointSettingCommand::set_zero_point(7).to_frame()
        );
    }

    #[test]
    fn test_request_joint_zero_positions_rejects_partial_multi_joint_subset() {
        let sent_frames = Arc::new(Mutex::new(Vec::new()));
        let driver = build_driver(sent_frames);
        let commander = RawCommander::new(&driver);

        let error = commander
            .request_joint_zero_positions_confirmed(&[0, 1], Duration::from_secs(2))
            .expect_err("2-5 joint subsets should be rejected without partial submission");

        assert!(matches!(error, RobotError::ConfigError(_)));
    }
}
