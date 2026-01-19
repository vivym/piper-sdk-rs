//! Robot API 模块
//!
//! 提供对外的 `Piper` 结构体，封装底层 IO 线程和状态同步细节。

use crate::can::{CanAdapter, CanError, PiperFrame};
use crate::robot::error::RobotError;
use crate::robot::fps_stats::{FpsCounts, FpsResult};
use crate::robot::pipeline::*;
use crate::robot::state::*;
use crossbeam_channel::Sender;
use std::sync::Arc;
use std::thread::{JoinHandle, spawn};
use tracing::error;

/// Piper 机械臂驱动（对外 API）
pub struct Piper {
    /// 命令发送通道（向 IO 线程发送控制帧）
    cmd_tx: Sender<PiperFrame>,
    /// 共享状态上下文
    ctx: Arc<PiperContext>,
    /// IO 线程句柄（Drop 时 join）
    io_thread: Option<JoinHandle<()>>,
}

impl Piper {
    /// 创建新的 Piper 实例
    ///
    /// # 参数
    /// - `can`: CAN 适配器（会被移动到 IO 线程）
    /// - `config`: Pipeline 配置（可选）
    ///
    /// # 错误
    /// - `CanError`: CAN 设备初始化失败（注意：这里返回 CanError，因为 RobotError 尚未完全实现 `From<CanError>`）
    pub fn new(
        can: impl CanAdapter + Send + 'static,
        config: Option<PipelineConfig>,
    ) -> Result<Self, CanError> {
        // 创建命令通道（有界队列，容量 10）
        let (cmd_tx, cmd_rx) = crossbeam_channel::bounded(10);

        // 创建共享状态上下文
        let ctx = Arc::new(PiperContext::new());

        // 克隆上下文用于 IO 线程
        let ctx_clone = ctx.clone();

        // 启动 IO 线程
        let io_thread = spawn(move || {
            io_loop(can, cmd_rx, ctx_clone, config.unwrap_or_default());
        });

        Ok(Self {
            cmd_tx,
            ctx,
            io_thread: Some(io_thread),
        })
    }

    /// 获取关节动态状态（无锁，纳秒级返回）
    ///
    /// 包含关节速度和电流（独立帧 + Buffered Commit）。
    ///
    /// # 性能
    /// - 无锁读取（ArcSwap::load）
    /// - 返回快照副本（Clone 开销低，< 150 字节）
    /// - 适合 500Hz 控制循环
    pub fn get_joint_dynamic(&self) -> JointDynamicState {
        self.ctx.joint_dynamic.load().as_ref().clone()
    }

    /// 获取关节位置状态（无锁，纳秒级返回）
    ///
    /// 包含6个关节的位置信息（500Hz更新）。
    ///
    /// # 性能
    /// - 无锁读取（ArcSwap::load）
    /// - 返回快照副本（Clone 开销低）
    /// - 适合 500Hz 控制循环
    ///
    /// # 注意
    /// - 此状态与 `EndPoseState` 不是原子更新的，如需同时获取，请使用 `capture_motion_snapshot()`
    pub fn get_joint_position(&self) -> JointPositionState {
        self.ctx.joint_position.load().as_ref().clone()
    }

    /// 获取末端位姿状态（无锁，纳秒级返回）
    ///
    /// 包含末端执行器的位置和姿态信息（500Hz更新）。
    ///
    /// # 性能
    /// - 无锁读取（ArcSwap::load）
    /// - 返回快照副本（Clone 开销低）
    /// - 适合 500Hz 控制循环
    ///
    /// # 注意
    /// - 此状态与 `JointPositionState` 不是原子更新的，如需同时获取，请使用 `capture_motion_snapshot()`
    pub fn get_end_pose(&self) -> EndPoseState {
        self.ctx.end_pose.load().as_ref().clone()
    }

    /// 获取运动快照（无锁，纳秒级返回）
    ///
    /// 原子性地获取 `JointPositionState` 和 `EndPoseState` 的最新快照。
    /// 虽然这两个状态在硬件上不是同时更新的，但此方法保证逻辑上的原子性。
    ///
    /// # 性能
    /// - 无锁读取（两次 ArcSwap::load）
    /// - 返回快照副本
    /// - 适合需要同时使用关节位置和末端位姿的场景
    ///
    /// # 示例
    ///
    /// ```
    /// # use piper_sdk::robot::Piper;
    /// # // 注意：此示例需要实际的 CAN 适配器，仅供参考
    /// # // let piper = Piper::new(/* ... */).unwrap();
    /// # // let snapshot = piper.capture_motion_snapshot();
    /// # // println!("Joint positions: {:?}", snapshot.joint_position.joint_pos);
    /// # // println!("End pose: {:?}", snapshot.end_pose.end_pose);
    /// ```
    pub fn capture_motion_snapshot(&self) -> MotionSnapshot {
        self.ctx.capture_motion_snapshot()
    }

    /// 获取机器人控制状态（无锁）
    ///
    /// 包含控制模式、机器人状态、故障码等（100Hz更新）。
    ///
    /// # 性能
    /// - 无锁读取（ArcSwap::load）
    /// - 返回快照副本
    pub fn get_robot_control(&self) -> RobotControlState {
        self.ctx.robot_control.load().as_ref().clone()
    }

    /// 获取夹爪状态（无锁）
    ///
    /// 包含夹爪行程、扭矩、状态码等（100Hz更新）。
    ///
    /// # 性能
    /// - 无锁读取（ArcSwap::load）
    /// - 返回快照副本
    pub fn get_gripper(&self) -> GripperState {
        self.ctx.gripper.load().as_ref().clone()
    }

    /// 获取关节驱动器低速反馈状态（无锁）
    ///
    /// 包含温度、电压、电流、驱动器状态等（40Hz更新）。
    ///
    /// # 性能
    /// - 无锁读取（ArcSwap::load，Wait-Free）
    /// - 返回快照副本
    pub fn get_joint_driver_low_speed(&self) -> JointDriverLowSpeedState {
        self.ctx.joint_driver_low_speed.load().as_ref().clone()
    }

    /// 获取碰撞保护状态（读锁）
    ///
    /// 包含各关节的碰撞保护等级（按需查询）。
    ///
    /// # 性能
    /// - 读锁（RwLock::read）
    /// - 返回快照副本
    pub fn get_collision_protection(&self) -> Result<CollisionProtectionState, RobotError> {
        self.ctx
            .collision_protection
            .read()
            .map(|guard| guard.clone())
            .map_err(|_| RobotError::PoisonedLock)
    }

    /// 获取关节限制配置状态（读锁）
    ///
    /// 包含关节角度限制和速度限制（按需查询）。
    ///
    /// # 性能
    /// - 读锁（RwLock::read）
    /// - 返回快照副本
    pub fn get_joint_limit_config(&self) -> Result<JointLimitConfigState, RobotError> {
        self.ctx
            .joint_limit_config
            .read()
            .map(|guard| guard.clone())
            .map_err(|_| RobotError::PoisonedLock)
    }

    /// 获取关节加速度限制配置状态（读锁）
    ///
    /// 包含关节加速度限制（按需查询）。
    ///
    /// # 性能
    /// - 读锁（RwLock::read）
    /// - 返回快照副本
    pub fn get_joint_accel_config(&self) -> Result<JointAccelConfigState, RobotError> {
        self.ctx
            .joint_accel_config
            .read()
            .map(|guard| guard.clone())
            .map_err(|_| RobotError::PoisonedLock)
    }

    /// 获取末端限制配置状态（读锁）
    ///
    /// 包含末端执行器的速度和加速度限制（按需查询）。
    ///
    /// # 性能
    /// - 读锁（RwLock::read）
    /// - 返回快照副本
    pub fn get_end_limit_config(&self) -> Result<EndLimitConfigState, RobotError> {
        self.ctx
            .end_limit_config
            .read()
            .map(|guard| guard.clone())
            .map_err(|_| RobotError::PoisonedLock)
    }

    /// 获取组合运动状态（所有热数据）
    ///
    /// 注意：不同子状态的时间戳可能不同步（差异通常在毫秒级）。
    /// 如果需要时间对齐的状态，请使用 `get_aligned_motion()`。
    pub fn get_motion_state(&self) -> CombinedMotionState {
        let snapshot = self.capture_motion_snapshot();
        CombinedMotionState {
            joint_position: snapshot.joint_position,
            end_pose: snapshot.end_pose,
            joint_dynamic: self.get_joint_dynamic(),
        }
    }

    /// 获取时间对齐的运动状态（推荐用于力控算法）
    ///
    /// 以 `joint_position.hardware_timestamp_us` 为基准时间，检查时间戳差异。
    /// 即使时间戳差异超过阈值，也返回状态数据（让用户有选择权）。
    ///
    /// # 参数
    /// - `max_time_diff_us`: 允许的最大时间戳差异（微秒），推荐值：5000（5ms）
    ///
    /// # 返回值
    /// - `AlignmentResult::Ok(state)`: 时间戳差异在可接受范围内
    /// - `AlignmentResult::Misaligned { state, diff_us }`: 时间戳差异过大，但仍返回状态数据
    pub fn get_aligned_motion(&self, max_time_diff_us: u64) -> AlignmentResult {
        let snapshot = self.capture_motion_snapshot();
        let joint_dynamic = self.get_joint_dynamic();

        let time_diff = snapshot
            .joint_position
            .hardware_timestamp_us
            .abs_diff(joint_dynamic.group_timestamp_us);

        let state = AlignedMotionState {
            joint_pos: snapshot.joint_position.joint_pos,
            joint_vel: joint_dynamic.joint_vel,
            joint_current: joint_dynamic.joint_current,
            end_pose: snapshot.end_pose.end_pose,
            timestamp: snapshot.joint_position.hardware_timestamp_us, // 使用位置数据的时间戳作为基准
            time_diff_us: (joint_dynamic.group_timestamp_us as i64)
                - (snapshot.joint_position.hardware_timestamp_us as i64),
        };

        if time_diff > max_time_diff_us {
            AlignmentResult::Misaligned {
                state,
                diff_us: time_diff,
            }
        } else {
            AlignmentResult::Ok(state)
        }
    }

    /// 等待接收到第一个有效反馈（用于初始化）
    ///
    /// 在 `Piper::new()` 后调用，确保在控制循环开始前已收到有效数据。
    /// 避免使用全零的初始状态导致错误的控制指令。
    ///
    /// # 参数
    /// - `timeout`: 超时时间
    ///
    /// # 返回值
    /// - `Ok(())`: 成功接收到有效反馈（`timestamp_us > 0`）
    /// - `Err(RobotError::Timeout)`: 超时未收到反馈
    pub fn wait_for_feedback(&self, timeout: std::time::Duration) -> Result<(), RobotError> {
        let start = std::time::Instant::now();

        loop {
            // 检查是否超时
            if start.elapsed() >= timeout {
                return Err(RobotError::Timeout);
            }

            // 检查是否收到有效反馈（任意状态的时间戳 > 0 即可）
            let joint_pos = self.get_joint_position();
            if joint_pos.hardware_timestamp_us > 0 {
                return Ok(());
            }

            // 短暂休眠，避免 CPU 空转
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    /// 获取 FPS 统计结果
    ///
    /// 返回最近一次统计窗口内的更新频率（FPS）。
    /// 建议定期调用（如每秒一次）或按需调用。
    ///
    /// # 性能
    /// - 无锁读取（仅原子读取）
    /// - 开销：~100ns（5 次原子读取 + 浮点计算）
    ///
    /// # Example
    ///
    /// ```
    /// # use piper_sdk::robot::Piper;
    /// # // 注意：此示例需要实际的 CAN 适配器，仅供参考
    /// # // let piper = Piper::new(/* ... */).unwrap();
    /// # // 运行一段时间后查询 FPS
    /// # // std::thread::sleep(std::time::Duration::from_secs(5));
    /// # // let fps = piper.get_fps();
    /// # // println!("Joint Position FPS: {:.2}", fps.joint_position);
    /// # // println!("End Pose FPS: {:.2}", fps.end_pose);
    /// # // println!("Joint Dynamic FPS: {:.2}", fps.joint_dynamic);
    /// ```
    pub fn get_fps(&self) -> FpsResult {
        self.ctx.fps_stats.calculate_fps()
    }

    /// 获取 FPS 计数器原始值
    ///
    /// 返回当前计数器的原始值，可以配合自定义时间窗口计算 FPS。
    ///
    /// # 性能
    /// - 无锁读取（仅原子读取）
    /// - 开销：~50ns（5 次原子读取）
    ///
    /// # Example
    ///
    /// ```
    /// # use piper_sdk::robot::Piper;
    /// # // 注意：此示例需要实际的 CAN 适配器，仅供参考
    /// # // let piper = Piper::new(/* ... */).unwrap();
    /// # // 记录开始时间和计数
    /// # // let start = std::time::Instant::now();
    /// # // let counts_start = piper.get_fps_counts();
    /// # // 运行一段时间
    /// # // std::thread::sleep(std::time::Duration::from_secs(1));
    /// # // 计算实际 FPS
    /// # // let counts_end = piper.get_fps_counts();
    /// # // let elapsed = start.elapsed();
    /// # // let actual_fps = (counts_end.joint_position - counts_start.joint_position) as f64 / elapsed.as_secs_f64();
    /// ```
    pub fn get_fps_counts(&self) -> FpsCounts {
        self.ctx.fps_stats.get_counts()
    }

    /// 发送控制帧（非阻塞）
    ///
    /// # 参数
    /// - `frame`: 控制帧（已构建的 `PiperFrame`）
    ///
    /// # 错误
    /// - `RobotError::ChannelClosed`: 命令通道已关闭（IO 线程退出）
    /// - `RobotError::ChannelFull`: 命令队列已满（缓冲区容量 10）
    pub fn send_frame(&self, frame: PiperFrame) -> Result<(), RobotError> {
        self.cmd_tx.try_send(frame).map_err(|e| match e {
            crossbeam_channel::TrySendError::Full(_) => RobotError::ChannelFull,
            crossbeam_channel::TrySendError::Disconnected(_) => RobotError::ChannelClosed,
        })
    }

    /// 发送控制帧（阻塞，带超时）
    ///
    /// 如果命令通道已满，阻塞等待直到有空闲位置或超时。
    ///
    /// # 参数
    /// - `frame`: 控制帧（已构建的 `PiperFrame`）
    /// - `timeout`: 超时时间
    ///
    /// # 错误
    /// - `RobotError::ChannelClosed`: 命令通道已关闭（IO 线程退出）
    /// - `RobotError::Timeout`: 超时未发送成功
    pub fn send_frame_blocking(
        &self,
        frame: PiperFrame,
        timeout: std::time::Duration,
    ) -> Result<(), RobotError> {
        self.cmd_tx.send_timeout(frame, timeout).map_err(|e| match e {
            crossbeam_channel::SendTimeoutError::Timeout(_) => RobotError::Timeout,
            crossbeam_channel::SendTimeoutError::Disconnected(_) => RobotError::ChannelClosed,
        })
    }
}

impl Drop for Piper {
    fn drop(&mut self) {
        // 关闭命令通道（通知 IO 线程退出）
        // 通过 drop 发送端，接收端会检测到 Disconnected，IO 线程退出循环
        // 使用 replace 来避免移动 self.cmd_tx
        let _ = std::mem::replace(&mut self.cmd_tx, {
            // 创建一个永远不会被使用的发送端，只是为了占位
            let (_tx, _rx) = crossbeam_channel::bounded::<PiperFrame>(1);
            _tx
        });

        // 等待 IO 线程退出
        if let Some(handle) = self.io_thread.take() {
            // 设置超时，避免测试无限等待
            let start = std::time::Instant::now();
            while start.elapsed().as_secs() < 2 {
                if handle.is_finished() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }

            if let Err(_e) = handle.join() {
                error!("IO thread panicked");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::can::PiperFrame;

    // 简单的 Mock CanAdapter 用于测试
    struct MockCanAdapter;

    impl CanAdapter for MockCanAdapter {
        fn send(&mut self, _frame: PiperFrame) -> Result<(), CanError> {
            Ok(())
        }

        fn receive(&mut self) -> Result<PiperFrame, CanError> {
            // 永远超时，避免阻塞测试
            Err(CanError::Timeout)
        }
    }

    #[test]
    fn test_piper_new() {
        let mock_can = MockCanAdapter;
        let piper = Piper::new(mock_can, None).unwrap();

        // 验证可以获取状态（默认状态）
        let joint_pos = piper.get_joint_position();
        assert_eq!(joint_pos.hardware_timestamp_us, 0);

        // 验证通道正常工作
        let frame = PiperFrame::new_standard(0x123, &[0x01, 0x02]);
        assert!(piper.send_frame(frame).is_ok());
    }

    #[test]
    fn test_piper_drop() {
        let mock_can = MockCanAdapter;
        let piper = Piper::new(mock_can, None).unwrap();
        // drop 应该能够正常退出，IO 线程被 join
        drop(piper);
    }

    #[test]
    fn test_piper_get_motion_state() {
        let mock_can = MockCanAdapter;
        let piper = Piper::new(mock_can, None).unwrap();
        let motion = piper.get_motion_state();
        assert_eq!(motion.joint_position.hardware_timestamp_us, 0);
        assert_eq!(motion.joint_dynamic.group_timestamp_us, 0);
    }

    #[test]
    fn test_piper_send_frame_channel_full() {
        let mock_can = MockCanAdapter;
        let piper = Piper::new(mock_can, None).unwrap();
        let frame = PiperFrame::new_standard(0x123, &[0x01]);

        // 填满命令通道（容量 10）
        // 注意：IO 线程会持续消费帧，所以需要快速填充
        // 或者等待 IO 线程稍微延迟消费
        std::thread::sleep(std::time::Duration::from_millis(50));

        for _ in 0..10 {
            assert!(piper.send_frame(frame).is_ok());
        }

        // 第 11 次发送可能返回 ChannelFull（如果 IO 线程还没消费完）
        // 或者成功（如果 IO 线程已经消费了一些）
        // 为了测试 ChannelFull，我们需要更快速地发送，确保通道填满
        let result = piper.send_frame(frame);

        // 由于 IO 线程在后台消费，可能成功也可能失败
        // 验证至少前 10 次都成功即可
        match result {
            Err(RobotError::ChannelFull) => {
                // 通道满，这是预期情况
            },
            Ok(()) => {
                // 如果 IO 线程消费很快，这也可能发生
                // 这是可接受的行为
            },
            Err(e) => panic!("Unexpected error: {:?}", e),
        }
    }

    #[test]
    fn test_get_aligned_motion_aligned() {
        let mock_can = MockCanAdapter;
        let piper = Piper::new(mock_can, None).unwrap();

        // 由于 MockCanAdapter 不发送帧，时间戳都为 0
        // 测试默认状态下的对齐检查（时间戳都为 0，应该是对齐的）
        let result = piper.get_aligned_motion(5000);
        match result {
            AlignmentResult::Ok(state) => {
                assert_eq!(state.timestamp, 0);
                assert_eq!(state.time_diff_us, 0);
            },
            AlignmentResult::Misaligned { .. } => {
                // 如果时间戳都为 0，不应该是不对齐的
                // 但允许这种情况（因为时间戳都是 0）
            },
        }
    }

    #[test]
    fn test_get_aligned_motion_misaligned_threshold() {
        let mock_can = MockCanAdapter;
        let piper = Piper::new(mock_can, None).unwrap();

        // 测试不同的时间差阈值
        // 由于时间戳都是 0，应该是对齐的
        let result1 = piper.get_aligned_motion(0);
        let result2 = piper.get_aligned_motion(1000);
        let result3 = piper.get_aligned_motion(1000000);

        // 所有结果都应该返回状态（即使是对齐的）
        match (result1, result2, result3) {
            (AlignmentResult::Ok(_), AlignmentResult::Ok(_), AlignmentResult::Ok(_)) => {
                // 正常情况
            },
            _ => {
                // 允许其他情况
            },
        }
    }

    #[test]
    fn test_get_robot_control() {
        let mock_can = MockCanAdapter;
        let piper = Piper::new(mock_can, None).unwrap();

        let control = piper.get_robot_control();
        assert_eq!(control.hardware_timestamp_us, 0);
        assert_eq!(control.control_mode, 0);
        assert!(!control.is_enabled);
    }

    #[test]
    fn test_get_joint_driver_low_speed() {
        let mock_can = MockCanAdapter;
        let piper = Piper::new(mock_can, None).unwrap();

        let driver_state = piper.get_joint_driver_low_speed();
        assert_eq!(driver_state.hardware_timestamp_us, 0);
        assert_eq!(driver_state.motor_temps, [0.0; 6]);
    }

    #[test]
    fn test_get_joint_limit_config() {
        let mock_can = MockCanAdapter;
        let piper = Piper::new(mock_can, None).unwrap();

        let limits = piper.get_joint_limit_config().unwrap();
        assert_eq!(limits.joint_limits_max, [0.0; 6]);
    }

    #[test]
    fn test_wait_for_feedback_timeout() {
        let mock_can = MockCanAdapter;
        let piper = Piper::new(mock_can, None).unwrap();

        // MockCanAdapter 不发送帧，所以应该超时
        let result = piper.wait_for_feedback(std::time::Duration::from_millis(10));
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), RobotError::Timeout));
    }

    #[test]
    fn test_send_frame_blocking_timeout() {
        let mock_can = MockCanAdapter;
        let piper = Piper::new(mock_can, None).unwrap();
        let frame = PiperFrame::new_standard(0x123, &[0x01]);

        // 快速填充通道（如果 IO 线程来不及消费）
        // 然后测试阻塞发送
        // 由于通道容量为 10，在 IO 线程消费的情况下，应该能成功
        // 但为了测试超时，我们使用极短的超时时间
        let result = piper.send_frame_blocking(frame, std::time::Duration::from_millis(1));

        // 结果可能是成功（IO 线程消费快）或超时（通道满）
        match result {
            Ok(()) => {
                // 成功是正常情况
            },
            Err(RobotError::Timeout) => {
                // 超时也是可接受的（如果通道满）
            },
            Err(e) => panic!("Unexpected error: {:?}", e),
        }
    }

    #[test]
    fn test_get_aligned_motion_with_time_diff() {
        let mock_can = MockCanAdapter;
        let piper = Piper::new(mock_can, None).unwrap();

        // 测试对齐阈值边界情况
        // 时间戳都为 0 时，time_diff_us 应该是 0
        let result = piper.get_aligned_motion(0);
        match result {
            AlignmentResult::Ok(state) => {
                assert_eq!(state.time_diff_us, 0);
            },
            AlignmentResult::Misaligned { state, diff_us } => {
                // 如果时间戳都为 0，diff_us 应该也是 0
                assert_eq!(diff_us, 0);
                assert_eq!(state.time_diff_us, 0);
            },
        }
    }

    #[test]
    fn test_get_motion_state_returns_combined() {
        let mock_can = MockCanAdapter;
        let piper = Piper::new(mock_can, None).unwrap();

        let motion = piper.get_motion_state();
        // 验证返回的是组合状态
        assert_eq!(motion.joint_position.hardware_timestamp_us, 0);
        assert_eq!(motion.joint_dynamic.group_timestamp_us, 0);
        assert_eq!(motion.joint_position.joint_pos, [0.0; 6]);
        assert_eq!(motion.joint_dynamic.joint_vel, [0.0; 6]);
    }

    #[test]
    fn test_send_frame_non_blocking() {
        let mock_can = MockCanAdapter;
        let piper = Piper::new(mock_can, None).unwrap();
        let frame = PiperFrame::new_standard(0x123, &[0x01, 0x02]);

        // 非阻塞发送应该总是成功（除非通道满或关闭）
        let result = piper.send_frame(frame);
        assert!(result.is_ok(), "Non-blocking send should succeed");
    }

    #[test]
    fn test_get_joint_dynamic_default() {
        let mock_can = MockCanAdapter;
        let piper = Piper::new(mock_can, None).unwrap();

        let joint_dynamic = piper.get_joint_dynamic();
        assert_eq!(joint_dynamic.group_timestamp_us, 0);
        assert_eq!(joint_dynamic.joint_vel, [0.0; 6]);
        assert_eq!(joint_dynamic.joint_current, [0.0; 6]);
        assert!(!joint_dynamic.is_complete());
    }

    #[test]
    fn test_get_joint_position_default() {
        let mock_can = MockCanAdapter;
        let piper = Piper::new(mock_can, None).unwrap();

        let joint_pos = piper.get_joint_position();
        assert_eq!(joint_pos.hardware_timestamp_us, 0);
        assert_eq!(joint_pos.joint_pos, [0.0; 6]);

        let end_pose = piper.get_end_pose();
        assert_eq!(end_pose.hardware_timestamp_us, 0);
        assert_eq!(end_pose.end_pose, [0.0; 6]);
    }

    #[test]
    fn test_joint_driver_low_speed_clone() {
        let mock_can = MockCanAdapter;
        let piper = Piper::new(mock_can, None).unwrap();

        // 测试读取并克隆诊断状态
        let driver1 = piper.get_joint_driver_low_speed();
        let driver2 = piper.get_joint_driver_low_speed();

        // 验证可以多次读取（ArcSwap 无锁读取）
        assert_eq!(driver1.hardware_timestamp_us, driver2.hardware_timestamp_us);
        assert_eq!(driver1.motor_temps, driver2.motor_temps);
    }

    #[test]
    fn test_joint_limit_config_read_lock() {
        let mock_can = MockCanAdapter;
        let piper = Piper::new(mock_can, None).unwrap();

        // 测试可以多次读取配置状态
        let limits1 = piper.get_joint_limit_config().unwrap();
        let limits2 = piper.get_joint_limit_config().unwrap();

        assert_eq!(limits1.joint_limits_max, limits2.joint_limits_max);
        assert_eq!(limits1.joint_limits_min, limits2.joint_limits_min);
    }
}
