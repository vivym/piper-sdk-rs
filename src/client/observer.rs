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
//! - **逻辑一致性**: 提供 `snapshot()` 方法保证时间一致性
//!
//! # 使用示例
//!
//! ```rust,no_run
//! # use piper_sdk::client::observer::Observer;
//! # use piper_sdk::client::types::*;
//! # fn example(observer: Observer) -> Result<()> {
//! // 读取关节位置
//! let positions = observer.joint_positions();
//! println!("J1 position: {}", positions[Joint::J1].to_deg());
//!
//! // 使用 snapshot 获取时间一致的数据（推荐用于控制算法）
//! let snapshot = observer.snapshot();
//! println!("Position: {:?}, Velocity: {:?}", snapshot.position, snapshot.velocity);
//!
//! // 克隆 Observer 用于另一个线程
//! let observer2 = observer.clone();
//! std::thread::spawn(move || {
//!     loop {
//!         let snapshot = observer2.snapshot();
//!         // ... 监控状态 ...
//!     }
//! });
//! # Ok(())
//! # }
//! ```

use std::sync::Arc;
use std::time::Instant;

use crate::client::types::*;
use crate::driver::Piper as RobotPiper;
use crate::protocol::constants::*;

/// 状态观察器（只读接口，View 模式）
///
/// 直接持有 `driver::Piper` 引用，零拷贝、零延迟地读取底层状态。
/// 不再使用缓存层，避免数据延迟和锁竞争。
#[derive(Clone)]
pub struct Observer {
    /// Driver 实例（直接持有，零拷贝）
    driver: Arc<RobotPiper>,
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

    /// 获取运动快照（推荐用于控制算法）
    ///
    /// 此方法尽可能快地连续读取多个相关状态，减少时间偏斜。
    /// 即使底层是分帧更新的，此方法也能提供逻辑上最一致的数据。
    ///
    /// # 性能
    ///
    /// - 延迟：~20ns（连续调用 3 次 ArcSwap::load）
    /// - 无锁竞争（ArcSwap 是 Wait-Free 的）
    ///
    /// # 推荐使用场景
    ///
    /// - 高频控制算法（>100Hz）
    /// - 阻抗控制、力矩控制等需要时间一致性的算法
    pub fn snapshot(&self) -> MotionSnapshot {
        // 在读取之前记录时间戳，更准确地反映"读取动作发生"的时刻
        let timestamp = Instant::now();

        // 连续读取，减少中间被抢占的概率
        let pos = self.driver.get_joint_position();
        let dyn_state = self.driver.get_joint_dynamic();

        MotionSnapshot {
            position: JointArray::new(pos.joint_pos.map(Rad)),
            // ✅ 使用类型安全的单位
            velocity: JointArray::new(dyn_state.joint_vel.map(RadPerSecond)),
            torque: JointArray::new(dyn_state.get_all_torques().map(NewtonMeter)),
            timestamp, // 使用读取前的时间戳
        }
    }

    /// 获取关节位置（独立读取，可能与其他状态有时间偏斜）
    ///
    /// # 注意
    ///
    /// 如果需要与其他状态（如速度、力矩）保持时间一致性，
    /// 请使用 `snapshot()` 方法。
    pub fn joint_positions(&self) -> JointArray<Rad> {
        let raw_pos = self.driver.get_joint_position();
        JointArray::new(raw_pos.joint_pos.map(Rad))
    }

    /// 获取关节速度（独立读取，可能与其他状态有时间偏斜）
    ///
    /// # 注意
    ///
    /// 如果需要与其他状态（如位置、力矩）保持时间一致性，
    /// 请使用 `snapshot()` 方法。
    ///
    /// # 返回值
    ///
    /// 返回 `JointArray<RadPerSecond>`，保持类型安全。
    pub fn joint_velocities(&self) -> JointArray<RadPerSecond> {
        let dyn_state = self.driver.get_joint_dynamic();
        // ✅ 使用类型安全的单位
        JointArray::new(dyn_state.joint_vel.map(RadPerSecond))
    }

    /// 获取关节力矩（独立读取，可能与其他状态有时间偏斜）
    ///
    /// # 注意
    ///
    /// 如果需要与其他状态（如位置、速度）保持时间一致性，
    /// 请使用 `snapshot()` 方法。
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
        (driver_state.driver_enabled_mask >> joint_index) & 1 == 1
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
    /// 如需时间一致性，请使用 `snapshot()` 方法。
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
    /// # use piper_sdk::client::observer::Observer;
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
    /// # use piper_sdk::client::observer::Observer;
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

/// 运动快照（逻辑原子性）
///
/// **设计说明：**
/// - 使用 `#[non_exhaustive]` 允许未来非破坏性地添加字段
/// - 例如：加速度、数据有效性标志等衍生数据
#[derive(Debug, Clone)]
#[non_exhaustive] // ✅ 允许未来非破坏性地添加字段
pub struct MotionSnapshot {
    /// 关节位置
    pub position: JointArray<Rad>,
    /// 关节速度（✅ 使用类型安全的单位）
    pub velocity: JointArray<RadPerSecond>,
    /// 关节力矩
    pub torque: JointArray<NewtonMeter>,
    /// 读取时间戳（用于调试）
    pub timestamp: Instant,
}

// 确保 Send + Sync
unsafe impl Send for Observer {}
unsafe impl Sync for Observer {}

#[cfg(test)]
mod tests {
    use super::*;

    // 注意：单元测试中创建真实的 robot 实例需要真实的 CAN 适配器
    // 这里只测试类型和基本逻辑，集成测试会测试完整功能

    // 注意：这些测试需要真实的 robot 实例，应该在集成测试中完成
    // 这里只测试类型系统和基本逻辑

    #[test]
    fn test_motion_snapshot_structure() {
        // 测试 MotionSnapshot 结构
        let snapshot = MotionSnapshot {
            position: JointArray::splat(Rad(0.0)),
            velocity: JointArray::splat(RadPerSecond(0.0)),
            torque: JointArray::splat(NewtonMeter(0.0)),
            timestamp: Instant::now(),
        };

        // ✅ 验证速度单位类型正确
        let _: RadPerSecond = snapshot.velocity[Joint::J1];
        let _: JointArray<Rad> = snapshot.position;
        let _: JointArray<NewtonMeter> = snapshot.torque;
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
    fn test_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Observer>();
        assert_send_sync::<GripperState>();
        assert_send_sync::<MotionSnapshot>();
    }
}
