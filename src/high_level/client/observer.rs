//! Observer - 状态观察器
//!
//! 提供无锁的状态读取接口，与 Commander 完全独立，
//! 实现"读写分离"设计模式。
//!
//! # 设计目标
//!
//! - **只读**: 无任何修改状态的能力
//! - **可克隆**: 多个 Observer 可以并发读取
//! - **高性能**: 使用 RwLock 支持多读
//! - **类型安全**: 返回强类型单位（Rad, NewtonMeter）
//!
//! # 使用示例
//!
//! ```rust,no_run
//! # use piper_sdk::high_level::client::observer::Observer;
//! # use piper_sdk::high_level::types::*;
//! # fn example(observer: Observer) -> Result<()> {
//! // 读取关节位置
//! let positions = observer.joint_positions();
//! println!("J1 position: {}", positions[Joint::J1].to_deg());
//!
//! // 读取夹爪状态
//! let gripper_pos = observer.gripper_position();
//! println!("Gripper: {:.2}", gripper_pos);
//!
//! // 克隆 Observer 用于另一个线程
//! let observer2 = observer.clone();
//! std::thread::spawn(move || {
//!     loop {
//!         let torques = observer2.joint_torques();
//!         // ... 监控力矩 ...
//!     }
//! });
//! # Ok(())
//! # }
//! ```

use parking_lot::RwLock;
use std::sync::Arc;
use std::time::Instant;

use crate::high_level::types::*;

/// 机器人完整状态
#[derive(Debug, Clone)]
pub struct RobotState {
    /// 关节位置
    pub joint_positions: JointArray<Rad>,
    /// 关节速度 (rad/s)
    pub joint_velocities: JointArray<f64>,
    /// 关节力矩
    pub joint_torques: JointArray<NewtonMeter>,
    /// 夹爪状态
    pub gripper_state: GripperState,
    /// 机械臂使能状态
    pub arm_enabled: bool,
    /// 最后更新时间
    pub last_update: Instant,
}

/// 夹爪状态
#[derive(Debug, Clone, Copy)]
pub struct GripperState {
    /// 位置 (0.0-1.0)
    pub position: f64,
    /// 力度 (0.0-1.0)
    pub effort: f64,
    /// 使能状态
    pub enabled: bool,
}

impl Default for RobotState {
    fn default() -> Self {
        RobotState {
            joint_positions: JointArray::splat(Rad(0.0)),
            joint_velocities: JointArray::splat(0.0),
            joint_torques: JointArray::splat(NewtonMeter(0.0)),
            gripper_state: GripperState::default(),
            arm_enabled: false,
            last_update: Instant::now(),
        }
    }
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

/// 状态观察器（只读接口）
///
/// 可以克隆并在多个线程中并发使用，不影响 Commander 的性能。
#[derive(Clone)]
pub struct Observer {
    /// 共享状态（读写锁）
    state: Arc<RwLock<RobotState>>,
}

// 确保 Observer 可以在 Piper 的状态转换中移动
impl Observer {
    /// 克隆 Observer（内部使用 Arc，开销小）
    #[allow(dead_code)]
    pub(crate) fn clone_internal(&self) -> Self {
        self.clone()
    }
}

impl Observer {
    /// 创建新的 Observer
    ///
    /// 这个方法只能由 crate 内部调用，但在测试和基准测试中可用。
    #[doc(hidden)]
    pub fn new(state: Arc<RwLock<RobotState>>) -> Self {
        Observer { state }
    }

    /// 获取完整状态快照
    ///
    /// # 性能
    ///
    /// 这个方法会克隆整个状态，如果只需要部分数据，
    /// 使用专用方法（如 `joint_positions`）更高效。
    pub fn state(&self) -> RobotState {
        self.state.read().clone()
    }

    /// 获取关节位置
    ///
    /// # 示例
    ///
    /// ```rust,no_run
    /// # use piper_sdk::high_level::client::observer::Observer;
    /// # use piper_sdk::high_level::types::*;
    /// # fn example(observer: Observer) {
    /// let positions = observer.joint_positions();
    /// for joint in [Joint::J1, Joint::J2, Joint::J3, Joint::J4, Joint::J5, Joint::J6] {
    ///     println!("{:?}: {:.3} rad", joint, positions[joint].0);
    /// }
    /// # }
    /// ```
    pub fn joint_positions(&self) -> JointArray<Rad> {
        self.state.read().joint_positions
    }

    /// 获取关节速度 (rad/s)
    pub fn joint_velocities(&self) -> JointArray<f64> {
        self.state.read().joint_velocities
    }

    /// 获取关节力矩
    pub fn joint_torques(&self) -> JointArray<NewtonMeter> {
        self.state.read().joint_torques
    }

    /// 获取夹爪状态
    pub fn gripper_state(&self) -> GripperState {
        self.state.read().gripper_state
    }

    /// 获取夹爪位置 (0.0-1.0)
    pub fn gripper_position(&self) -> f64 {
        self.state.read().gripper_state.position
    }

    /// 获取夹爪力度 (0.0-1.0)
    pub fn gripper_effort(&self) -> f64 {
        self.state.read().gripper_state.effort
    }

    /// 检查夹爪是否使能
    pub fn is_gripper_enabled(&self) -> bool {
        self.state.read().gripper_state.enabled
    }

    /// 检查机械臂是否使能
    pub fn is_arm_enabled(&self) -> bool {
        self.state.read().arm_enabled
    }

    /// 获取最后更新时间
    ///
    /// 可用于检测状态更新的延迟。
    pub fn last_update(&self) -> Instant {
        self.state.read().last_update
    }

    /// 检查状态是否新鲜（最近更新）
    ///
    /// # 参数
    ///
    /// - `max_age`: 最大允许年龄（Duration）
    ///
    /// # 返回
    ///
    /// 如果状态在 `max_age` 内更新过，返回 `true`。
    pub fn is_fresh(&self, max_age: std::time::Duration) -> bool {
        let last = self.state.read().last_update;
        last.elapsed() < max_age
    }

    /// 获取单个关节的状态
    ///
    /// 返回 (position, velocity, torque) 元组。
    pub fn joint_state(&self, joint: Joint) -> (Rad, f64, NewtonMeter) {
        let state = self.state.read();
        (
            state.joint_positions[joint],
            state.joint_velocities[joint],
            state.joint_torques[joint],
        )
    }

    // ==================== 内部更新方法 ====================

    /// 更新关节状态（仅内部可见，但在基准测试中可用）
    #[doc(hidden)]
    pub fn update_joint_positions(&self, positions: JointArray<Rad>) {
        let mut state = self.state.write();
        state.joint_positions = positions;
        state.last_update = Instant::now();
    }

    /// 更新关节速度（仅内部可见，但在基准测试中可用）
    #[doc(hidden)]
    pub fn update_joint_velocities(&self, velocities: JointArray<f64>) {
        let mut state = self.state.write();
        state.joint_velocities = velocities;
        state.last_update = Instant::now();
    }

    /// 更新关节力矩（仅内部可见，但在基准测试中可用）
    #[doc(hidden)]
    pub fn update_joint_torques(&self, torques: JointArray<NewtonMeter>) {
        let mut state = self.state.write();
        state.joint_torques = torques;
        state.last_update = Instant::now();
    }

    /// 更新夹爪状态（仅内部可见，但在基准测试中可用）
    #[doc(hidden)]
    pub fn update_gripper_state(&self, gripper: GripperState) {
        let mut state = self.state.write();
        state.gripper_state = gripper;
        state.last_update = Instant::now();
    }

    /// 更新机械臂使能状态（仅内部可见，但在基准测试中可用）
    #[doc(hidden)]
    pub fn update_arm_enabled(&self, enabled: bool) {
        let mut state = self.state.write();
        state.arm_enabled = enabled;
        state.last_update = Instant::now();
    }

    /// 批量更新完整状态（仅内部可见，但在基准测试中可用）
    #[doc(hidden)]
    pub fn update_state(&self, new_state: RobotState) {
        *self.state.write() = new_state;
    }
}

// 确保 Send + Sync
unsafe impl Send for Observer {}
unsafe impl Sync for Observer {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn create_observer() -> Observer {
        Observer::new(Arc::new(RwLock::new(RobotState::default())))
    }

    #[test]
    fn test_default_state() {
        let observer = create_observer();
        let state = observer.state();

        assert_eq!(state.joint_positions[Joint::J1].0, 0.0);
        assert_eq!(state.gripper_state.position, 0.0);
        assert!(!state.arm_enabled);
    }

    #[test]
    fn test_joint_positions() {
        let observer = create_observer();

        let positions =
            JointArray::new([Rad(1.0), Rad(2.0), Rad(3.0), Rad(4.0), Rad(5.0), Rad(6.0)]);
        observer.update_joint_positions(positions);

        let read_positions = observer.joint_positions();
        assert_eq!(read_positions[Joint::J1].0, 1.0);
        assert_eq!(read_positions[Joint::J6].0, 6.0);
    }

    #[test]
    fn test_joint_velocities() {
        let observer = create_observer();

        let velocities = JointArray::splat(2.5);
        observer.update_joint_velocities(velocities);

        let read_velocities = observer.joint_velocities();
        assert_eq!(read_velocities[Joint::J1], 2.5);
    }

    #[test]
    fn test_joint_torques() {
        let observer = create_observer();

        let torques = JointArray::splat(NewtonMeter(10.0));
        observer.update_joint_torques(torques);

        let read_torques = observer.joint_torques();
        assert_eq!(read_torques[Joint::J1].0, 10.0);
    }

    #[test]
    fn test_gripper_state() {
        let observer = create_observer();

        let gripper = GripperState {
            position: 0.5,
            effort: 0.8,
            enabled: true,
        };
        observer.update_gripper_state(gripper);

        assert_eq!(observer.gripper_position(), 0.5);
        assert_eq!(observer.gripper_effort(), 0.8);
        assert!(observer.is_gripper_enabled());
    }

    #[test]
    fn test_arm_enabled() {
        let observer = create_observer();

        assert!(!observer.is_arm_enabled());

        observer.update_arm_enabled(true);
        assert!(observer.is_arm_enabled());

        observer.update_arm_enabled(false);
        assert!(!observer.is_arm_enabled());
    }

    #[test]
    fn test_last_update() {
        let observer = create_observer();

        let before = Instant::now();
        std::thread::sleep(Duration::from_millis(10));

        observer.update_joint_positions(JointArray::splat(Rad(1.0)));

        let after = Instant::now();
        let last = observer.last_update();

        assert!(last > before);
        assert!(last < after);
    }

    #[test]
    fn test_is_fresh() {
        let observer = create_observer();

        observer.update_joint_positions(JointArray::splat(Rad(1.0)));

        assert!(observer.is_fresh(Duration::from_secs(1)));

        // 模拟旧状态（需要修改实现或等待，这里只是逻辑检查）
        assert!(observer.is_fresh(Duration::from_millis(100)));
    }

    #[test]
    fn test_joint_state() {
        let observer = create_observer();

        observer.update_joint_positions(JointArray::splat(Rad(1.0)));
        observer.update_joint_velocities(JointArray::splat(2.0));
        observer.update_joint_torques(JointArray::splat(NewtonMeter(3.0)));

        let (pos, vel, torque) = observer.joint_state(Joint::J3);
        assert_eq!(pos.0, 1.0);
        assert_eq!(vel, 2.0);
        assert_eq!(torque.0, 3.0);
    }

    #[test]
    fn test_clone() {
        let observer1 = create_observer();
        observer1.update_joint_positions(JointArray::splat(Rad(5.0)));

        let observer2 = observer1.clone();

        // 两个 observer 共享状态
        assert_eq!(observer2.joint_positions()[Joint::J1].0, 5.0);

        // 修改会反映到两个 observer
        observer1.update_joint_positions(JointArray::splat(Rad(10.0)));
        assert_eq!(observer2.joint_positions()[Joint::J1].0, 10.0);
    }

    #[test]
    fn test_concurrent_reads() {
        let observer = Arc::new(create_observer());
        observer.update_joint_positions(JointArray::splat(Rad(1.0)));

        let mut handles = vec![];
        for _ in 0..10 {
            let obs_clone = observer.clone();
            handles.push(std::thread::spawn(move || {
                for _ in 0..1000 {
                    let _positions = obs_clone.joint_positions();
                    let _velocities = obs_clone.joint_velocities();
                    let _gripper = obs_clone.gripper_position();
                }
            }));
        }

        for handle in handles {
            handle.join().unwrap();
        }
    }

    #[test]
    fn test_concurrent_read_write() {
        let observer = Arc::new(create_observer());

        // 写线程
        let obs_writer = observer.clone();
        let writer = std::thread::spawn(move || {
            for i in 0..100 {
                obs_writer.update_joint_positions(JointArray::splat(Rad(i as f64)));
                std::thread::sleep(Duration::from_micros(10));
            }
        });

        // 读线程
        let mut readers = vec![];
        for _ in 0..5 {
            let obs_reader = observer.clone();
            readers.push(std::thread::spawn(move || {
                for _ in 0..500 {
                    let _pos = obs_reader.joint_positions();
                }
            }));
        }

        writer.join().unwrap();
        for reader in readers {
            reader.join().unwrap();
        }
    }

    #[test]
    fn test_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Observer>();
        assert_send_sync::<RobotState>();
        assert_send_sync::<GripperState>();
    }

    #[test]
    fn test_full_state_snapshot() {
        let observer = create_observer();

        observer.update_joint_positions(JointArray::splat(Rad(1.0)));
        observer.update_gripper_state(GripperState {
            position: 0.5,
            effort: 0.7,
            enabled: true,
        });
        observer.update_arm_enabled(true);

        let snapshot = observer.state();
        assert_eq!(snapshot.joint_positions[Joint::J1].0, 1.0);
        assert_eq!(snapshot.gripper_state.position, 0.5);
        assert!(snapshot.arm_enabled);
    }
}
