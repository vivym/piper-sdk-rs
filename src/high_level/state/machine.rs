//! Type State Machine - 编译期状态安全
//!
//! 使用零大小类型（ZST）标记实现状态机，在编译期防止非法状态转换。

use std::marker::PhantomData;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::high_level::client::{
    motion_commander::MotionCommander, observer::Observer, raw_commander::RawCommander,
    state_tracker::ControlMode,
};
use crate::high_level::types::*;

// ==================== 状态类型（零大小类型）====================

/// 未连接状态
///
/// 这是初始状态，在此状态下无法进行任何操作。
pub struct Disconnected;

/// 待机状态
///
/// 已连接但未使能。可以读取状态，但不能发送运动命令。
pub struct Standby;

/// 活动状态（带控制模式）
///
/// 机械臂已使能，可以发送运动命令。
pub struct Active<Mode>(PhantomData<Mode>);

// ==================== 控制模式类型（零大小类型）====================

/// MIT 模式
///
/// 支持位置、速度、力矩的混合控制。
pub struct MitMode;

/// 位置模式
///
/// 纯位置控制模式。
pub struct PositionMode;

// ==================== 连接配置 ====================

/// 连接配置
#[derive(Debug, Clone)]
pub struct ConnectionConfig {
    /// CAN 接口名称（如 "can0"）
    pub interface: String,
    /// 连接超时
    pub timeout: Duration,
}

impl Default for ConnectionConfig {
    fn default() -> Self {
        ConnectionConfig {
            interface: "can0".to_string(),
            timeout: Duration::from_secs(5),
        }
    }
}

/// MIT 模式配置
#[derive(Debug, Clone)]
pub struct MitModeConfig {
    /// 使能超时
    pub timeout: Duration,
}

impl Default for MitModeConfig {
    fn default() -> Self {
        MitModeConfig {
            timeout: Duration::from_secs(2),
        }
    }
}

/// 位置模式配置
#[derive(Debug, Clone)]
pub struct PositionModeConfig {
    /// 使能超时
    pub timeout: Duration,
}

impl Default for PositionModeConfig {
    fn default() -> Self {
        PositionModeConfig {
            timeout: Duration::from_secs(2),
        }
    }
}

// ==================== Piper 状态机 ====================

/// Piper 机械臂（Type State Pattern）
///
/// 使用泛型参数 `State` 在编译期强制执行正确的状态转换。
///
/// # 类型参数
///
/// - `State`: 当前状态（`Disconnected`, `Standby`, `Active<Mode>`）
///
/// # 零开销
///
/// 状态类型是零大小类型（ZST），不占用任何运行时内存。
pub struct Piper<State = Disconnected> {
    pub(crate) raw_commander: Arc<RawCommander>,
    pub(crate) observer: Observer,
    // TODO: Phase 3 后续任务会添加 state_monitor 和 heartbeat
    _state: PhantomData<State>,
}

// ==================== Disconnected 状态 ====================

impl Piper<Disconnected> {
    /// 创建未连接的 Piper 实例
    ///
    /// 这个方法通常不直接使用，而是通过 `connect()` 直接连接。
    pub fn new() -> Self {
        unimplemented!("Use Piper::connect() instead")
    }
}

impl Default for Piper<Disconnected> {
    fn default() -> Self {
        Self::new()
    }
}

// ==================== Standby 状态 ====================

impl Piper<Standby> {
    /// 使能 MIT 模式
    ///
    /// # 错误
    ///
    /// - `RobotError::Timeout`: 使能超时
    /// - `RobotError::HardwareError`: 硬件响应异常
    ///
    /// # 示例
    ///
    /// ```rust,ignore
    /// # use piper_sdk::high_level::state::*;
    /// # use piper_sdk::high_level::types::*;
    /// # fn example(robot: Piper<Standby>) -> Result<()> {
    /// let robot = robot.enable_mit_mode(MitModeConfig::default())?;
    /// // 现在可以发送力矩命令
    /// # Ok(())
    /// # }
    /// ```
    pub fn enable_mit_mode(self, config: MitModeConfig) -> Result<Piper<Active<MitMode>>> {
        // 1. 使能机械臂
        self.raw_commander.enable_arm()?;

        // 2. 等待使能完成
        self.wait_for_enabled(config.timeout)?;

        // 3. 设置 MIT 模式
        self.raw_commander.set_control_mode(ControlMode::MitMode)?;

        // 4. 类型转换（必须 clone，因为 Piper 实现了 Drop）
        let new_piper = Piper {
            raw_commander: self.raw_commander.clone(),
            observer: self.observer.clone(),
            _state: PhantomData,
        };

        // 5. 阻止 Drop 执行（避免状态转换时自动 disable）
        std::mem::forget(self);

        Ok(new_piper)
    }

    /// 使能位置模式
    ///
    /// # 错误
    ///
    /// - `RobotError::Timeout`: 使能超时
    /// - `RobotError::HardwareError`: 硬件响应异常
    pub fn enable_position_mode(
        self,
        config: PositionModeConfig,
    ) -> Result<Piper<Active<PositionMode>>> {
        // 1. 使能机械臂
        self.raw_commander.enable_arm()?;

        // 2. 等待使能完成
        self.wait_for_enabled(config.timeout)?;

        // 3. 设置位置模式
        self.raw_commander.set_control_mode(ControlMode::PositionMode)?;

        // 4. 类型转换（必须 clone，因为 Piper 实现了 Drop）
        let new_piper = Piper {
            raw_commander: self.raw_commander.clone(),
            observer: self.observer.clone(),
            _state: PhantomData,
        };

        // 5. 阻止 Drop 执行（避免状态转换时自动 disable）
        std::mem::forget(self);

        Ok(new_piper)
    }

    /// 获取 Observer（只读）
    ///
    /// 即使在 Standby 状态，也可以读取机械臂状态。
    pub fn observer(&self) -> &Observer {
        &self.observer
    }

    /// 等待机械臂使能完成
    ///
    /// # 参数
    ///
    /// - `timeout`: 超时时间
    ///
    /// # 错误
    ///
    /// - `RobotError::Timeout`: 等待超时
    /// - `RobotError::HardwareFailure`: 硬件反馈异常
    fn wait_for_enabled(&self, timeout: Duration) -> Result<()> {
        let start = Instant::now();
        let poll_interval = Duration::from_millis(10); // 100Hz 轮询

        loop {
            // 检查超时
            if start.elapsed() > timeout {
                return Err(RobotError::Timeout {
                    timeout_ms: timeout.as_millis() as u64,
                });
            }

            // 读取使能状态
            if self.observer.is_arm_enabled() {
                return Ok(());
            }

            // 短暂休眠避免占用 CPU
            std::thread::sleep(poll_interval);
        }
    }
}

// ==================== 所有状态共享的辅助方法 ====================

impl<State> Piper<State> {
    /// 检查状态是否有效
    ///
    /// 如果 StateTracker 检测到异常，返回 false。
    pub fn is_valid(&self) -> bool {
        self.raw_commander.state_tracker().is_valid()
    }

    /// 获取最后的错误信息
    ///
    /// 如果状态跟踪器标记为 poisoned，返回详细错误原因。
    pub fn last_error(&self) -> Option<String> {
        self.raw_commander.state_tracker().poison_reason()
    }

    /// 等待机械臂失能完成（所有状态共享）
    ///
    /// # 参数
    ///
    /// - `timeout`: 超时时间
    ///
    /// # 错误
    ///
    /// - `RobotError::Timeout`: 等待超时
    /// - `RobotError::HardwareFailure`: 硬件反馈异常
    fn wait_for_disabled(&self, timeout: Duration) -> Result<()> {
        let start = Instant::now();
        let poll_interval = Duration::from_millis(10); // 100Hz 轮询

        loop {
            // 检查超时
            if start.elapsed() > timeout {
                return Err(RobotError::Timeout {
                    timeout_ms: timeout.as_millis() as u64,
                });
            }

            // 读取失能状态
            if !self.observer.is_arm_enabled() {
                return Ok(());
            }

            // 短暂休眠避免占用 CPU
            std::thread::sleep(poll_interval);
        }
    }
}

// ==================== Active<MitMode> 状态 ====================

impl Piper<Active<MitMode>> {
    /// 发送 MIT 模式力矩指令
    ///
    /// # 参数
    ///
    /// - `joint`: 目标关节
    /// - `position`: 目标位置（Rad）
    /// - `velocity`: 目标速度（rad/s）
    /// - `kp`: 位置增益
    /// - `kd`: 速度增益
    /// - `torque`: 前馈力矩（NewtonMeter）
    ///
    /// # 示例
    ///
    /// ```rust,ignore
    /// # use piper_sdk::high_level::state::*;
    /// # use piper_sdk::high_level::types::*;
    /// # fn example(robot: Piper<Active<MitMode>>) -> Result<()> {
    /// robot.command_torques(
    ///     Joint::J1,
    ///     Rad(1.0),
    ///     0.5,
    ///     10.0,
    ///     2.0,
    ///     NewtonMeter(5.0),
    /// )?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn command_torques(
        &self,
        joint: Joint,
        position: Rad,
        velocity: f64,
        kp: f64,
        kd: f64,
        torque: NewtonMeter,
    ) -> Result<()> {
        self.raw_commander.send_mit_command(joint, position, velocity, kp, kd, torque)
    }

    /// 获取 MotionCommander（受限权限）
    ///
    /// 返回一个可以克隆和传递的命令接口，但无法修改状态机。
    pub fn motion_commander(&self) -> MotionCommander {
        MotionCommander::new(self.raw_commander.clone())
    }

    /// 获取 Observer（只读）
    pub fn observer(&self) -> &Observer {
        &self.observer
    }

    /// 失能机械臂（返回 Standby 状态）
    ///
    /// # 参数
    ///
    /// - `timeout`: 等待失能完成的超时时间
    ///
    /// # 示例
    ///
    /// ```rust,ignore
    /// # use piper_sdk::high_level::state::*;
    /// # use std::time::Duration;
    /// # fn example(robot: Piper<Active<MitMode>>) -> Result<()> {
    /// let robot = robot.disable(Duration::from_secs(2))?;  // Piper<Standby>
    /// # Ok(())
    /// # }
    /// ```
    pub fn disable(self, timeout: Duration) -> Result<Piper<Standby>> {
        // 1. 失能机械臂
        self.raw_commander.disable_arm()?;

        // 2. 等待失能完成
        self.wait_for_disabled(timeout)?;

        // 3. 类型转换（必须 clone，因为 Piper 实现了 Drop）
        let new_piper = Piper {
            raw_commander: self.raw_commander.clone(),
            observer: self.observer.clone(),
            _state: PhantomData,
        };

        // 4. 阻止 Drop 执行（避免重复 disable）
        std::mem::forget(self);

        Ok(new_piper)
    }
}

// ==================== Active<PositionMode> 状态 ====================

impl Piper<Active<PositionMode>> {
    /// 发送位置指令
    ///
    /// # 参数
    ///
    /// - `joint`: 目标关节
    /// - `position`: 目标位置（Rad）
    /// - `velocity`: 目标速度（rad/s）
    pub fn command_position(&self, joint: Joint, position: Rad, velocity: f64) -> Result<()> {
        self.raw_commander.send_position_command(joint, position, velocity)
    }

    /// 获取 MotionCommander（受限权限）
    pub fn motion_commander(&self) -> MotionCommander {
        MotionCommander::new(self.raw_commander.clone())
    }

    /// 获取 Observer（只读）
    pub fn observer(&self) -> &Observer {
        &self.observer
    }

    /// 失能机械臂（返回 Standby 状态）
    ///
    /// # 参数
    ///
    /// - `timeout`: 等待失能完成的超时时间
    pub fn disable(self, timeout: Duration) -> Result<Piper<Standby>> {
        // 1. 失能机械臂
        self.raw_commander.disable_arm()?;

        // 2. 等待失能完成
        self.wait_for_disabled(timeout)?;

        // 3. 类型转换（必须 clone，因为 Piper 实现了 Drop）
        let new_piper = Piper {
            raw_commander: self.raw_commander.clone(),
            observer: self.observer.clone(),
            _state: PhantomData,
        };

        // 4. 阻止 Drop 执行（避免重复 disable）
        std::mem::forget(self);

        Ok(new_piper)
    }
}

// ==================== Drop 实现（安全关闭）====================

impl<State> Drop for Piper<State> {
    fn drop(&mut self) {
        // 尝试失能（忽略错误，因为可能已经失能）
        let _ = self.raw_commander.disable_arm();

        // TODO: Phase 3 后续任务
        // - 关闭 Heartbeat
        // - 关闭 StateMonitor
    }
}

// ==================== 测试 ====================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_state_types_are_zero_sized() {
        assert_eq!(std::mem::size_of::<Disconnected>(), 0);
        assert_eq!(std::mem::size_of::<Standby>(), 0);
        assert_eq!(std::mem::size_of::<Active<MitMode>>(), 0);
        assert_eq!(std::mem::size_of::<Active<PositionMode>>(), 0);
        assert_eq!(std::mem::size_of::<MitMode>(), 0);
        assert_eq!(std::mem::size_of::<PositionMode>(), 0);
    }

    #[test]
    fn test_mode_types_are_zero_sized() {
        assert_eq!(std::mem::size_of::<MitMode>(), 0);
        assert_eq!(std::mem::size_of::<PositionMode>(), 0);
    }

    #[test]
    fn test_phantom_data_overhead() {
        assert_eq!(std::mem::size_of::<PhantomData<Disconnected>>(), 0);
        assert_eq!(std::mem::size_of::<PhantomData<Active<MitMode>>>(), 0);
    }

    // TODO: 更多集成测试在 Phase 3 后续任务中添加
}
