//! Observer - 状态观察器（View 模式）
//!
//! 直接持有 `driver::Piper` 引用，零拷贝、零延迟地读取底层状态。
//! 不再使用缓存层，避免数据延迟和锁竞争。
//!
//! # 设计目标
//!
//! - **零延迟**: 直接从 `driver::Piper` 读取，无缓存层
//! - **零拷贝**: 使用 ArcSwap 的 wait-free 读取
//! - **类型安全**: 返回强类型单位（Rad, RadPerSecond, NewtonMeter）
//! - **控制安全**: 提供 `control_snapshot()`，只返回对齐且新鲜的控制状态
//!
//! # 使用示例
//!
//! ```rust,no_run
//! # use piper_client::observer::Observer;
//! # use piper_client::observer::ControlReadPolicy;
//! # use piper_client::types::*;
//! # fn example(observer: Observer) -> Result<()> {
//! // 读取关节位置
//! let positions = observer.joint_positions();
//! println!("J1 position: {}", positions[Joint::J1].to_deg());
//!
//! // 使用 control_snapshot 获取可直接用于闭环控制的数据
//! let snapshot = observer.control_snapshot(ControlReadPolicy::default())?;
//! println!("Position: {:?}, Velocity: {:?}", snapshot.position, snapshot.velocity);
//!
//! // 克隆 Observer 用于另一个线程
//! let observer2 = observer.clone();
//! std::thread::spawn(move || {
//!     loop {
//!         let snapshot = observer2.control_snapshot(ControlReadPolicy::default());
//!         // ... 监控状态 ...
//!     }
//! });
//! # Ok(())
//! # }
//! ```

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::types::*;
use piper_driver::{
    AlignmentResult, DriverError, HealthStatus, Piper as RobotPiper, RuntimeFaultKind,
};
use piper_protocol::constants::*;

/// 状态观察器（只读接口，View 模式）
///
/// 直接持有 `driver::Piper` 引用，零拷贝、零延迟地读取底层状态。
/// 不再使用缓存层，避免数据延迟和锁竞争。
#[derive(Clone)]
pub struct Observer {
    /// Driver 实例（直接持有，零拷贝）
    driver: Arc<RobotPiper>,
}

/// 碰撞保护配置快照
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CollisionProtectionSnapshot {
    /// 设备硬件时间戳（微秒）
    pub hardware_timestamp_us: u64,
    /// 主机接收时间戳（微秒）
    pub system_timestamp_us: u64,
    /// `[J1, J2, J3, J4, J5, J6]` 的碰撞防护等级
    pub levels: [u8; 6],
}

impl CollisionProtectionSnapshot {
    /// 判断快照是否严格晚于给定时间基线。
    pub fn is_newer_than(self, hardware_timestamp_us: u64, system_timestamp_us: u64) -> bool {
        self.hardware_timestamp_us > hardware_timestamp_us
            || self.system_timestamp_us > system_timestamp_us
    }
}

impl From<piper_driver::CollisionProtectionState> for CollisionProtectionSnapshot {
    fn from(value: piper_driver::CollisionProtectionState) -> Self {
        Self {
            hardware_timestamp_us: value.hardware_timestamp_us,
            system_timestamp_us: value.system_timestamp_us,
            levels: value.protection_levels,
        }
    }
}

/// 高频控制读取策略
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ControlReadPolicy {
    /// 允许的位置/动态状态最大时间偏差（微秒）
    pub max_state_skew_us: u64,
    /// 允许的最大反馈年龄
    pub max_feedback_age: Duration,
}

impl Default for ControlReadPolicy {
    fn default() -> Self {
        Self {
            max_state_skew_us: 5_000,
            max_feedback_age: Duration::from_millis(50),
        }
    }
}

/// 可直接用于控制闭环的对齐快照
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ControlSnapshot {
    /// 关节位置
    pub position: JointArray<Rad>,
    /// 关节速度
    pub velocity: JointArray<RadPerSecond>,
    /// 关节力矩
    pub torque: JointArray<NewtonMeter>,
    /// 位置反馈硬件时间戳
    pub position_timestamp_us: u64,
    /// 动态反馈硬件时间戳
    pub dynamic_timestamp_us: u64,
    /// 有符号时间偏差（dynamic - position）
    pub skew_us: i64,
}

/// 可直接用于双臂协调的完整控制快照
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ControlSnapshotFull {
    /// 对齐后的控制状态
    pub state: ControlSnapshot,
    /// 位置反馈主机时间戳
    pub position_system_timestamp_us: u64,
    /// 动态反馈主机时间戳
    pub dynamic_system_timestamp_us: u64,
    /// 反馈年龄（取位置/动态中的较大值）
    pub feedback_age: Duration,
}

impl ControlSnapshotFull {
    /// 获取该快照的最新主机时间戳（微秒）
    pub fn latest_system_timestamp_us(self) -> u64 {
        self.position_system_timestamp_us.max(self.dynamic_system_timestamp_us)
    }
}

/// Driver 运行时健康状态快照
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeHealthSnapshot {
    /// 是否仍在接收任意反馈
    pub connected: bool,
    /// 最近一次任意反馈距离现在的年龄
    pub last_feedback_age: Duration,
    /// RX 线程是否存活
    pub rx_alive: bool,
    /// TX 线程是否存活
    pub tx_alive: bool,
    /// 最近一次运行时故障
    pub fault: Option<RuntimeFaultKind>,
}

impl From<HealthStatus> for RuntimeHealthSnapshot {
    fn from(value: HealthStatus) -> Self {
        Self {
            connected: value.connected,
            last_feedback_age: value.last_feedback_age,
            rx_alive: value.rx_alive,
            tx_alive: value.tx_alive,
            fault: value.fault,
        }
    }
}

impl Observer {
    /// 创建新的 Observer
    ///
    /// **注意：** 此方法通常不直接调用，Observer 应该通过 `Piper` 状态机的 `observer()` 方法获取。
    /// 此方法为 `pub` 以支持内部测试和性能基准测试。
    ///
    /// **基准测试：** 为了支持性能基准测试，此方法在 benches 中也可访问。
    pub fn new(driver: Arc<RobotPiper>) -> Self {
        Observer { driver }
    }

    /// 获取可直接用于控制闭环的对齐状态
    ///
    /// 与监控/诊断接口不同，此方法会严格检查：
    /// - 最近反馈是否仍然新鲜
    /// - 位置状态和动态状态是否在允许的时间偏差内
    ///
    /// 任一条件不满足都会返回错误，不会返回“半可用”数据。
    pub fn control_snapshot(&self, policy: ControlReadPolicy) -> Result<ControlSnapshot> {
        self.control_snapshot_full(policy).map(|snapshot| snapshot.state)
    }

    /// 获取带主机时间戳和反馈年龄的完整控制快照
    pub fn control_snapshot_full(&self, policy: ControlReadPolicy) -> Result<ControlSnapshotFull> {
        match self.driver.get_aligned_motion(policy.max_state_skew_us) {
            AlignmentResult::Ok(state) => {
                if !state.is_complete() {
                    return Err(RobotError::control_state_incomplete(
                        state.position_frame_valid_mask,
                        state.dynamic_valid_mask,
                    ));
                }

                let age = control_feedback_age(
                    state.position_system_timestamp_us,
                    state.dynamic_system_timestamp_us,
                );
                if age > policy.max_feedback_age {
                    return Err(RobotError::feedback_stale(age, policy.max_feedback_age));
                }

                Ok(ControlSnapshotFull {
                    state: ControlSnapshot {
                        position: JointArray::new(state.joint_pos.map(Rad)),
                        velocity: JointArray::new(state.joint_vel.map(RadPerSecond)),
                        torque: JointArray::new(std::array::from_fn(|index| {
                            NewtonMeter(piper_driver::JointDynamicState::calculate_torque(
                                index,
                                state.joint_current[index],
                            ))
                        })),
                        position_timestamp_us: state.position_timestamp_us,
                        dynamic_timestamp_us: state.dynamic_timestamp_us,
                        skew_us: state.skew_us,
                    },
                    position_system_timestamp_us: state.position_system_timestamp_us,
                    dynamic_system_timestamp_us: state.dynamic_system_timestamp_us,
                    feedback_age: age,
                })
            },
            AlignmentResult::Misaligned { state, .. } => {
                if !state.is_complete() {
                    return Err(RobotError::control_state_incomplete(
                        state.position_frame_valid_mask,
                        state.dynamic_valid_mask,
                    ));
                }

                let age = control_feedback_age(
                    state.position_system_timestamp_us,
                    state.dynamic_system_timestamp_us,
                );
                if age > policy.max_feedback_age {
                    return Err(RobotError::feedback_stale(age, policy.max_feedback_age));
                }

                Err(RobotError::state_misaligned(
                    state.skew_us,
                    policy.max_state_skew_us,
                ))
            },
        }
    }

    /// 获取关节位置（监控/诊断接口）
    ///
    /// # 注意
    ///
    /// 控制闭环不要使用此接口拼接多路状态；请改用 `control_snapshot()`。
    /// 该接口可能返回部分帧组提交后的状态，只适合监控/诊断。
    pub fn joint_positions(&self) -> JointArray<Rad> {
        let raw_pos = self.driver.get_joint_position();
        JointArray::new(raw_pos.joint_pos.map(Rad))
    }

    /// 获取关节速度（监控/诊断接口）
    ///
    /// # 注意
    ///
    /// 控制闭环不要使用此接口拼接多路状态；请改用 `control_snapshot()`。
    /// 该接口可能返回 timeout 提交的部分动态组，只适合监控/诊断。
    ///
    /// # 返回值
    ///
    /// 返回 `JointArray<RadPerSecond>`，保持类型安全。
    pub fn joint_velocities(&self) -> JointArray<RadPerSecond> {
        let dyn_state = self.driver.get_joint_dynamic();
        // ✅ 使用类型安全的单位
        JointArray::new(dyn_state.joint_vel.map(RadPerSecond))
    }

    /// 获取关节力矩（监控/诊断接口）
    ///
    /// # 注意
    ///
    /// 控制闭环不要使用此接口拼接多路状态；请改用 `control_snapshot()`。
    /// 该接口可能返回 timeout 提交的部分动态组，只适合监控/诊断。
    pub fn joint_torques(&self) -> JointArray<NewtonMeter> {
        let dyn_state = self.driver.get_joint_dynamic();
        JointArray::new(dyn_state.get_all_torques().map(NewtonMeter))
    }

    /// 获取夹爪状态
    pub fn gripper_state(&self) -> GripperState {
        let gripper = self.driver.get_gripper();
        GripperState {
            position: (gripper.travel / GRIPPER_POSITION_SCALE).clamp(0.0, 1.0),
            effort: (gripper.torque / GRIPPER_FORCE_SCALE).clamp(0.0, 1.0),
            enabled: gripper.is_enabled(),
        }
    }

    /// 获取夹爪位置 (0.0-1.0)
    pub fn gripper_position(&self) -> f64 {
        self.gripper_state().position
    }

    /// 获取夹爪力度 (0.0-1.0)
    pub fn gripper_effort(&self) -> f64 {
        self.gripper_state().effort
    }

    /// 检查夹爪是否使能
    pub fn is_gripper_enabled(&self) -> bool {
        self.driver.get_gripper().is_enabled()
    }

    /// 获取使能掩码（Bit 0-5 对应 J1-J6）
    pub fn joint_enabled_mask(&self) -> u8 {
        let driver_state = self.driver.get_joint_driver_low_speed();
        driver_state.driver_enabled_mask
    }

    /// 检查指定关节是否使能
    pub fn is_joint_enabled(&self, joint_index: usize) -> bool {
        let driver_state = self.driver.get_joint_driver_low_speed();
        (driver_state.driver_enabled_mask & (1 << joint_index)) != 0
    }

    /// 获取末端位姿（独立读取，可能与其他状态有时间偏斜）
    ///
    /// # 返回值
    ///
    /// 返回 `EndPoseState`，包含：
    /// - `end_pose`: [X, Y, Z, Rx, Ry, Rz]
    ///   - X, Y, Z: 位置（米）
    ///   - Rx, Ry, Rz: 姿态角（弧度）
    ///
    /// # 示例
    ///
    /// ```rust,no_run
    /// # use piper_client::observer::Observer;
    /// # fn example(observer: Observer) {
    /// let end_pose = observer.end_pose();
    /// println!("Position: X={:.4}, Y={:.4}, Z={:.4}",
    ///     end_pose.end_pose[0], end_pose.end_pose[1], end_pose.end_pose[2]);
    /// # }
    /// ```
    pub fn end_pose(&self) -> piper_driver::state::EndPoseState {
        self.driver.get_end_pose()
    }

    /// 检查是否全部使能
    pub fn is_all_enabled(&self) -> bool {
        self.joint_enabled_mask() == 0b111111
    }

    /// 检查是否全部失能
    pub fn is_all_disabled(&self) -> bool {
        self.joint_enabled_mask() == 0
    }

    /// 检查是否部分使能
    pub fn is_partially_enabled(&self) -> bool {
        let mask = self.joint_enabled_mask();
        mask != 0 && mask != 0b111111
    }

    /// 检查机械臂是否使能（兼容旧 API）
    ///
    /// 如果所有关节都使能，返回 `true`。
    pub fn is_arm_enabled(&self) -> bool {
        self.is_all_enabled()
    }

    /// 获取单个关节的状态
    ///
    /// 返回 (position, velocity, torque) 元组。
    /// **注意**：此方法独立读取，可能与其他状态有时间偏斜。
    /// 如需控制闭环使用，请改用 `control_snapshot()`。
    pub fn joint_state(&self, joint: Joint) -> (Rad, RadPerSecond, NewtonMeter) {
        let pos = self.driver.get_joint_position();
        let dyn_state = self.driver.get_joint_dynamic();
        (
            Rad(pos.joint_pos[joint.index()]),
            RadPerSecond(dyn_state.joint_vel[joint.index()]),
            NewtonMeter(dyn_state.get_torque(joint.index())),
        )
    }

    // ============================================================
    // 连接监控 API
    // ============================================================

    /// 检查机器人是否仍在响应
    ///
    /// 如果在超时窗口内收到反馈，返回 `true`。
    /// 这可用于检测机器人是否断电、CAN 线缆断开或固件崩溃。
    ///
    /// # 示例
    ///
    /// ```rust,ignore
    /// # use piper_client::observer::Observer;
    /// # fn example(observer: Observer) {
    /// if observer.is_connected() {
    ///     println!("Robot is still responding");
    /// } else {
    ///     println!("Robot connection lost!");
    /// }
    /// # }
    /// ```
    pub fn is_connected(&self) -> bool {
        self.driver.is_connected()
    }

    /// 获取自上次反馈以来的时间
    ///
    /// 返回自上次成功处理 CAN 帧以来的时间。
    /// 可用于连接质量监控或诊断。
    ///
    /// # 示例
    ///
    /// ```rust,ignore
    /// # use piper_client::observer::Observer;
    /// # fn example(observer: Observer) {
    /// let age = observer.connection_age();
    /// if age.as_millis() > 100 {
    ///     println!("Connection is degrading: {}ms since last feedback", age.as_millis());
    /// }
    /// # }
    /// ```
    pub fn connection_age(&self) -> std::time::Duration {
        self.driver.connection_age()
    }

    /// 获取 driver 运行时健康快照。
    pub fn runtime_health(&self) -> RuntimeHealthSnapshot {
        self.driver.health().into()
    }

    /// 获取当前缓存的碰撞保护快照
    ///
    /// 返回 driver 中最近一次收到的碰撞保护状态快照。
    ///
    /// # 返回
    ///
    /// 返回碰撞保护等级及其时间戳；如果底层状态不可读，则返回错误。
    ///
    /// # 示例
    ///
    /// ```rust,ignore
    /// # use piper_client::Observer;
    /// # fn example(observer: Observer) {
    /// let snapshot = observer.collision_protection_snapshot().unwrap();
    /// println!("J1-J6 碰撞保护级别: {:?}", snapshot.levels);
    ///
    /// // 检查某个关节的保护等级
    /// if snapshot.levels[0] == 0 {
    ///     println!("J1 未启用碰撞保护");
    /// }
    /// # }
    /// ```
    pub fn collision_protection_snapshot(
        &self,
    ) -> std::result::Result<CollisionProtectionSnapshot, DriverError> {
        self.driver.get_collision_protection().map(CollisionProtectionSnapshot::from)
    }
}

fn control_feedback_age(
    position_system_timestamp_us: u64,
    dynamic_system_timestamp_us: u64,
) -> Duration {
    let position_age = system_timestamp_age(position_system_timestamp_us);
    let dynamic_age = system_timestamp_age(dynamic_system_timestamp_us);
    position_age.max(dynamic_age)
}

fn system_timestamp_age(timestamp_us: u64) -> Duration {
    if timestamp_us == 0 {
        return Duration::MAX;
    }

    let now_us = match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => duration.as_micros() as u64,
        Err(_) => return Duration::MAX,
    };

    if now_us < timestamp_us {
        return Duration::MAX;
    }

    Duration::from_micros(now_us - timestamp_us)
}

/// 夹爪状态
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GripperState {
    /// 位置 (0.0-1.0)
    pub position: f64,
    /// 力度 (0.0-1.0)
    pub effort: f64,
    /// 使能状态
    pub enabled: bool,
}

impl Default for GripperState {
    fn default() -> Self {
        GripperState {
            position: 0.0,
            effort: 0.0,
            enabled: false,
        }
    }
}

// 确保 Send + Sync
unsafe impl Send for Observer {}
unsafe impl Sync for Observer {}

#[cfg(test)]
mod tests {
    use super::*;
    use piper_can::{CanError, PiperFrame, RxAdapter, TxAdapter};
    use piper_protocol::ids::{
        ID_GRIPPER_FEEDBACK, ID_JOINT_DRIVER_HIGH_SPEED_BASE, ID_JOINT_FEEDBACK_12,
        ID_JOINT_FEEDBACK_34, ID_JOINT_FEEDBACK_56,
    };
    use std::collections::VecDeque;
    use std::thread;
    use std::time::Duration;

    struct ScriptedRxAdapter {
        frames: VecDeque<PiperFrame>,
    }

    impl ScriptedRxAdapter {
        fn new(frames: Vec<PiperFrame>) -> Self {
            Self {
                frames: frames.into(),
            }
        }
    }

    impl RxAdapter for ScriptedRxAdapter {
        fn receive(&mut self) -> std::result::Result<PiperFrame, CanError> {
            self.frames.pop_front().ok_or(CanError::Timeout)
        }
    }

    struct TimedFrame {
        delay: Duration,
        frame: PiperFrame,
    }

    struct PacedRxAdapter {
        frames: VecDeque<TimedFrame>,
    }

    impl PacedRxAdapter {
        fn new(frames: Vec<TimedFrame>) -> Self {
            Self {
                frames: frames.into(),
            }
        }
    }

    impl RxAdapter for PacedRxAdapter {
        fn receive(&mut self) -> std::result::Result<PiperFrame, CanError> {
            match self.frames.pop_front() {
                Some(timed) => {
                    if !timed.delay.is_zero() {
                        thread::sleep(timed.delay);
                    }
                    Ok(timed.frame)
                },
                None => Err(CanError::Timeout),
            }
        }
    }

    struct IdleTxAdapter;

    impl TxAdapter for IdleTxAdapter {
        fn send(&mut self, _frame: PiperFrame) -> std::result::Result<(), CanError> {
            Ok(())
        }
    }

    // 注意：单元测试中创建真实的 robot 实例需要真实的 CAN 适配器
    // 这里只测试类型和基本逻辑，集成测试会测试完整功能

    // 注意：这些测试需要真实的 robot 实例，应该在集成测试中完成
    // 这里只测试类型系统和基本逻辑

    #[test]
    fn test_control_snapshot_structure() {
        let snapshot = ControlSnapshot {
            position: JointArray::splat(Rad(0.0)),
            velocity: JointArray::splat(RadPerSecond(0.0)),
            torque: JointArray::splat(NewtonMeter(0.0)),
            position_timestamp_us: 100,
            dynamic_timestamp_us: 100,
            skew_us: 0,
        };

        let _: RadPerSecond = snapshot.velocity[Joint::J1];
        let _: JointArray<Rad> = snapshot.position;
        let _: JointArray<NewtonMeter> = snapshot.torque;
    }

    #[test]
    fn test_control_snapshot_full_structure() {
        let snapshot = ControlSnapshotFull {
            state: ControlSnapshot {
                position: JointArray::splat(Rad(0.0)),
                velocity: JointArray::splat(RadPerSecond(0.0)),
                torque: JointArray::splat(NewtonMeter(0.0)),
                position_timestamp_us: 100,
                dynamic_timestamp_us: 100,
                skew_us: 0,
            },
            position_system_timestamp_us: 1_000,
            dynamic_system_timestamp_us: 2_000,
            feedback_age: Duration::from_millis(5),
        };

        assert_eq!(snapshot.latest_system_timestamp_us(), 2_000);
    }

    fn joint_feedback_frame(
        can_id: u16,
        first_deg_milli: i32,
        second_deg_milli: i32,
        timestamp_us: u64,
    ) -> PiperFrame {
        let mut data = [0u8; 8];
        data[0..4].copy_from_slice(&first_deg_milli.to_be_bytes());
        data[4..8].copy_from_slice(&second_deg_milli.to_be_bytes());
        let mut frame = PiperFrame::new_standard(can_id, &data);
        frame.timestamp_us = timestamp_us;
        frame
    }

    fn joint_dynamic_frame(
        joint_index: u8,
        speed_millirad_per_sec: i16,
        current_milliamp: i16,
        timestamp_us: u64,
    ) -> PiperFrame {
        let mut data = [0u8; 8];
        data[0..2].copy_from_slice(&speed_millirad_per_sec.to_be_bytes());
        data[2..4].copy_from_slice(&current_milliamp.to_be_bytes());
        data[4..8].copy_from_slice(&0i32.to_be_bytes());
        let mut frame = PiperFrame::new_standard(
            (ID_JOINT_DRIVER_HIGH_SPEED_BASE + u32::from(joint_index - 1)) as u16,
            &data,
        );
        frame.timestamp_us = timestamp_us;
        frame
    }

    fn gripper_feedback_frame(timestamp_us: u64) -> PiperFrame {
        let travel_raw = 50_000i32.to_be_bytes();
        let torque_raw = 1_000i16.to_be_bytes();
        let mut data = [0u8; 8];
        data[0..4].copy_from_slice(&travel_raw);
        data[4..6].copy_from_slice(&torque_raw);
        data[6] = 0b0100_0000;

        let mut frame = PiperFrame::new_standard(ID_GRIPPER_FEEDBACK as u16, &data);
        frame.timestamp_us = timestamp_us;
        frame
    }

    fn start_observer_with_frames(frames: Vec<PiperFrame>) -> (Arc<RobotPiper>, Observer) {
        let driver = Arc::new(
            RobotPiper::new_dual_thread_parts(ScriptedRxAdapter::new(frames), IdleTxAdapter, None)
                .expect("driver should start"),
        );
        let observer = Observer::new(driver.clone());
        (driver, observer)
    }

    fn start_observer_with_timed_frames(frames: Vec<TimedFrame>) -> (Arc<RobotPiper>, Observer) {
        let driver = Arc::new(
            RobotPiper::new_dual_thread_parts(PacedRxAdapter::new(frames), IdleTxAdapter, None)
                .expect("driver should start"),
        );
        let observer = Observer::new(driver.clone());
        (driver, observer)
    }

    #[test]
    fn test_gripper_state_structure() {
        let gripper = GripperState {
            position: 0.5,
            effort: 0.7,
            enabled: true,
        };

        // 验证归一化范围
        assert!(gripper.position >= 0.0 && gripper.position <= 1.0);
        assert!(gripper.effort >= 0.0 && gripper.effort <= 1.0);
    }

    #[test]
    fn test_gripper_effort_full_scale_matches_five_nm_feedback() {
        let travel_raw = 50_000i32.to_be_bytes();
        let torque_raw = 5_000i16.to_be_bytes();
        let mut data = [0u8; 8];
        data[0..4].copy_from_slice(&travel_raw);
        data[4..6].copy_from_slice(&torque_raw);
        data[6] = 0b0100_0000; // enabled = true

        let frame = PiperFrame::new_standard(ID_GRIPPER_FEEDBACK as u16, &data);
        let driver = Arc::new(
            RobotPiper::new_dual_thread_parts(
                ScriptedRxAdapter::new(vec![frame]),
                IdleTxAdapter,
                None,
            )
            .expect("driver should start"),
        );
        let observer = Observer::new(driver.clone());

        driver
            .wait_for_feedback(Duration::from_millis(200))
            .expect("gripper feedback should arrive");

        let gripper = observer.gripper_state();
        assert_eq!(gripper.position, 0.5);
        assert_eq!(gripper.effort, 1.0);
        assert!(gripper.enabled);
    }

    #[test]
    fn test_control_snapshot_returns_aligned_state() {
        let position_timestamp_us = 1_000;
        let dynamic_timestamp_us = 1_000;
        let frames = vec![
            joint_feedback_frame(ID_JOINT_FEEDBACK_12 as u16, 0, 0, position_timestamp_us),
            joint_feedback_frame(ID_JOINT_FEEDBACK_34 as u16, 0, 0, position_timestamp_us),
            joint_feedback_frame(ID_JOINT_FEEDBACK_56 as u16, 0, 0, position_timestamp_us),
            joint_dynamic_frame(1, 1000, 1000, dynamic_timestamp_us),
            joint_dynamic_frame(2, 1000, 1000, dynamic_timestamp_us),
            joint_dynamic_frame(3, 1000, 1000, dynamic_timestamp_us),
            joint_dynamic_frame(4, 1000, 1000, dynamic_timestamp_us),
            joint_dynamic_frame(5, 1000, 1000, dynamic_timestamp_us),
            joint_dynamic_frame(6, 1000, 1000, dynamic_timestamp_us),
        ];
        let (driver, observer) = start_observer_with_frames(frames);

        driver
            .wait_for_feedback(Duration::from_millis(200))
            .expect("feedback should arrive");
        thread::sleep(Duration::from_millis(20));

        let snapshot = observer
            .control_snapshot(ControlReadPolicy {
                max_state_skew_us: 500,
                max_feedback_age: Duration::from_millis(200),
            })
            .expect("aligned snapshot should succeed");

        assert_eq!(snapshot.position_timestamp_us, position_timestamp_us);
        assert_eq!(snapshot.dynamic_timestamp_us, dynamic_timestamp_us);
        assert_eq!(snapshot.skew_us, 0);
        assert_eq!(snapshot.velocity[Joint::J1], RadPerSecond(1.0));
    }

    #[test]
    fn test_control_snapshot_full_exposes_metadata() {
        let position_timestamp_us = 1_000;
        let dynamic_timestamp_us = 1_000;
        let frames = vec![
            joint_feedback_frame(ID_JOINT_FEEDBACK_12 as u16, 0, 0, position_timestamp_us),
            joint_feedback_frame(ID_JOINT_FEEDBACK_34 as u16, 0, 0, position_timestamp_us),
            joint_feedback_frame(ID_JOINT_FEEDBACK_56 as u16, 0, 0, position_timestamp_us),
            joint_dynamic_frame(1, 1000, 1000, dynamic_timestamp_us),
            joint_dynamic_frame(2, 1000, 1000, dynamic_timestamp_us),
            joint_dynamic_frame(3, 1000, 1000, dynamic_timestamp_us),
            joint_dynamic_frame(4, 1000, 1000, dynamic_timestamp_us),
            joint_dynamic_frame(5, 1000, 1000, dynamic_timestamp_us),
            joint_dynamic_frame(6, 1000, 1000, dynamic_timestamp_us),
        ];
        let (driver, observer) = start_observer_with_frames(frames);

        driver
            .wait_for_feedback(Duration::from_millis(200))
            .expect("feedback should arrive");
        thread::sleep(Duration::from_millis(20));

        let snapshot = observer
            .control_snapshot_full(ControlReadPolicy {
                max_state_skew_us: 500,
                max_feedback_age: Duration::from_millis(200),
            })
            .expect("aligned full snapshot should succeed");

        assert_eq!(snapshot.state.position_timestamp_us, position_timestamp_us);
        assert_eq!(snapshot.state.dynamic_timestamp_us, dynamic_timestamp_us);
        assert!(snapshot.position_system_timestamp_us > 0);
        assert!(snapshot.dynamic_system_timestamp_us > 0);
        assert!(snapshot.feedback_age < Duration::from_millis(200));
    }

    #[test]
    fn test_control_snapshot_rejects_stale_feedback() {
        let timestamp_us = 1_000;
        let frames = vec![
            joint_feedback_frame(ID_JOINT_FEEDBACK_12 as u16, 0, 0, timestamp_us),
            joint_feedback_frame(ID_JOINT_FEEDBACK_34 as u16, 0, 0, timestamp_us),
            joint_feedback_frame(ID_JOINT_FEEDBACK_56 as u16, 0, 0, timestamp_us),
            joint_dynamic_frame(1, 0, 1000, timestamp_us),
            joint_dynamic_frame(2, 0, 1000, timestamp_us),
            joint_dynamic_frame(3, 0, 1000, timestamp_us),
            joint_dynamic_frame(4, 0, 1000, timestamp_us),
            joint_dynamic_frame(5, 0, 1000, timestamp_us),
            joint_dynamic_frame(6, 0, 1000, timestamp_us),
        ];
        let (driver, observer) = start_observer_with_frames(frames);

        driver
            .wait_for_feedback(Duration::from_millis(200))
            .expect("feedback should arrive");
        thread::sleep(Duration::from_millis(30));

        let error = observer
            .control_snapshot(ControlReadPolicy {
                max_state_skew_us: 500,
                max_feedback_age: Duration::from_millis(10),
            })
            .unwrap_err();

        assert!(matches!(error, RobotError::FeedbackStale { .. }));
    }

    #[test]
    fn test_control_snapshot_rejects_incomplete_position_group() {
        let timestamp_us = 1_000;
        let frames = vec![
            joint_feedback_frame(ID_JOINT_FEEDBACK_12 as u16, 0, 0, timestamp_us),
            joint_feedback_frame(ID_JOINT_FEEDBACK_56 as u16, 0, 0, timestamp_us),
            joint_dynamic_frame(1, 0, 1000, timestamp_us),
            joint_dynamic_frame(2, 0, 1000, timestamp_us),
            joint_dynamic_frame(3, 0, 1000, timestamp_us),
            joint_dynamic_frame(4, 0, 1000, timestamp_us),
            joint_dynamic_frame(5, 0, 1000, timestamp_us),
            joint_dynamic_frame(6, 0, 1000, timestamp_us),
        ];
        let (driver, observer) = start_observer_with_frames(frames);

        driver
            .wait_for_feedback(Duration::from_millis(200))
            .expect("feedback should arrive");
        thread::sleep(Duration::from_millis(20));

        let error = observer
            .control_snapshot(ControlReadPolicy {
                max_state_skew_us: 500,
                max_feedback_age: Duration::from_millis(200),
            })
            .unwrap_err();

        assert!(matches!(
            error,
            RobotError::ControlStateIncomplete {
                position_frame_valid_mask: 0b101,
                dynamic_valid_mask: 0b111111,
            }
        ));
    }

    #[test]
    fn test_control_snapshot_rejects_incomplete_dynamic_group() {
        let timestamp_us = 1_000;
        let frames = vec![
            joint_feedback_frame(ID_JOINT_FEEDBACK_12 as u16, 0, 0, timestamp_us),
            joint_feedback_frame(ID_JOINT_FEEDBACK_34 as u16, 0, 0, timestamp_us),
            joint_feedback_frame(ID_JOINT_FEEDBACK_56 as u16, 0, 0, timestamp_us),
            joint_dynamic_frame(1, 0, 1000, timestamp_us),
            joint_dynamic_frame(2, 0, 1000, timestamp_us),
            joint_dynamic_frame(3, 0, 1000, timestamp_us),
            joint_dynamic_frame(4, 0, 1000, timestamp_us),
        ];
        let (driver, observer) = start_observer_with_frames(frames);

        driver
            .wait_for_feedback(Duration::from_millis(200))
            .expect("feedback should arrive");
        thread::sleep(Duration::from_millis(20));

        let error = observer
            .control_snapshot(ControlReadPolicy {
                max_state_skew_us: 500,
                max_feedback_age: Duration::from_millis(200),
            })
            .unwrap_err();

        assert!(matches!(
            error,
            RobotError::ControlStateIncomplete {
                position_frame_valid_mask: 0b111,
                dynamic_valid_mask: 0b001111,
            }
        ));
    }

    #[test]
    fn test_control_snapshot_prioritizes_incomplete_over_stale_and_misaligned() {
        let error = Observer::new(Arc::new(
            RobotPiper::new_dual_thread_parts(
                ScriptedRxAdapter::new(Vec::new()),
                IdleTxAdapter,
                None,
            )
            .expect("driver should start"),
        ))
        .control_snapshot(ControlReadPolicy {
            max_state_skew_us: 0,
            max_feedback_age: Duration::from_millis(1),
        })
        .unwrap_err();

        assert!(matches!(error, RobotError::ControlStateIncomplete { .. }));
    }

    #[test]
    fn test_control_snapshot_rejects_stale_motion_even_if_other_feedback_is_fresh() {
        let position_timestamp_us = 1_000;
        let dynamic_timestamp_us = 1_000;
        let frames = vec![
            TimedFrame {
                delay: Duration::ZERO,
                frame: joint_feedback_frame(
                    ID_JOINT_FEEDBACK_12 as u16,
                    0,
                    0,
                    position_timestamp_us,
                ),
            },
            TimedFrame {
                delay: Duration::ZERO,
                frame: joint_feedback_frame(
                    ID_JOINT_FEEDBACK_34 as u16,
                    0,
                    0,
                    position_timestamp_us,
                ),
            },
            TimedFrame {
                delay: Duration::ZERO,
                frame: joint_feedback_frame(
                    ID_JOINT_FEEDBACK_56 as u16,
                    0,
                    0,
                    position_timestamp_us,
                ),
            },
            TimedFrame {
                delay: Duration::ZERO,
                frame: joint_dynamic_frame(1, 1000, 1000, dynamic_timestamp_us),
            },
            TimedFrame {
                delay: Duration::ZERO,
                frame: joint_dynamic_frame(2, 1000, 1000, dynamic_timestamp_us),
            },
            TimedFrame {
                delay: Duration::ZERO,
                frame: joint_dynamic_frame(3, 1000, 1000, dynamic_timestamp_us),
            },
            TimedFrame {
                delay: Duration::ZERO,
                frame: joint_dynamic_frame(4, 1000, 1000, dynamic_timestamp_us),
            },
            TimedFrame {
                delay: Duration::ZERO,
                frame: joint_dynamic_frame(5, 1000, 1000, dynamic_timestamp_us),
            },
            TimedFrame {
                delay: Duration::ZERO,
                frame: joint_dynamic_frame(6, 1000, 1000, dynamic_timestamp_us),
            },
            TimedFrame {
                delay: Duration::from_millis(40),
                frame: gripper_feedback_frame(2_000),
            },
        ];
        let (driver, observer) = start_observer_with_timed_frames(frames);

        driver
            .wait_for_feedback(Duration::from_millis(200))
            .expect("feedback should arrive");
        thread::sleep(Duration::from_millis(60));

        let error = observer
            .control_snapshot(ControlReadPolicy {
                max_state_skew_us: 500,
                max_feedback_age: Duration::from_millis(30),
            })
            .unwrap_err();

        assert!(matches!(error, RobotError::FeedbackStale { .. }));
    }

    #[test]
    fn test_control_snapshot_rejects_misaligned_state() {
        let position_timestamp_us = 1_000;
        let dynamic_timestamp_us = 9_500;
        let frames = vec![
            joint_feedback_frame(ID_JOINT_FEEDBACK_12 as u16, 0, 0, position_timestamp_us),
            joint_feedback_frame(ID_JOINT_FEEDBACK_34 as u16, 0, 0, position_timestamp_us),
            joint_feedback_frame(ID_JOINT_FEEDBACK_56 as u16, 0, 0, position_timestamp_us),
            joint_dynamic_frame(1, 0, 1000, dynamic_timestamp_us),
            joint_dynamic_frame(2, 0, 1000, dynamic_timestamp_us),
            joint_dynamic_frame(3, 0, 1000, dynamic_timestamp_us),
            joint_dynamic_frame(4, 0, 1000, dynamic_timestamp_us),
            joint_dynamic_frame(5, 0, 1000, dynamic_timestamp_us),
            joint_dynamic_frame(6, 0, 1000, dynamic_timestamp_us),
        ];
        let (driver, observer) = start_observer_with_frames(frames);

        driver
            .wait_for_feedback(Duration::from_millis(200))
            .expect("feedback should arrive");
        thread::sleep(Duration::from_millis(20));

        let error = observer
            .control_snapshot(ControlReadPolicy {
                max_state_skew_us: 1_000,
                max_feedback_age: Duration::from_millis(200),
            })
            .unwrap_err();

        assert!(matches!(
            error,
            RobotError::StateMisaligned {
                skew_us: 8_500,
                max_skew_us: 1_000,
            }
        ));
    }

    #[test]
    fn test_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Observer>();
        assert_send_sync::<GripperState>();
        assert_send_sync::<ControlReadPolicy>();
        assert_send_sync::<ControlSnapshot>();
        assert_send_sync::<ControlSnapshotFull>();
        assert_send_sync::<RuntimeHealthSnapshot>();
    }
}
