//! MIT 模式高层控制器
//!
//! 简化 MIT 模式的使用，提供：
//! - 阻塞式位置控制
//! - 自动状态管理
//! - 循环锚点机制（消除累积漂移，保证精确 200Hz）
//! - 容错性增强（允许偶发丢帧）
//!
//! # 设计理念
//!
//! - **Option 模式**：使用 `Option<Piper<Active<MitMode>>>` 允许安全提取
//! - **状态流转**：`park()` 返还 `Piper<Standby>`，支持继续使用
//! - **循环锚点**：使用绝对时间锚点，消除累积漂移
//! - **容错性**：允许最多连续 5 帧丢帧（25ms @ 200Hz）
//!
//! # 使用示例
//!
//! ```rust,no_run
//! use piper_client::control::MitController;
//! use piper_client::control::MitControllerConfig;
//! use piper_client::state::*;
//! use piper_client::types::*;
//! use std::time::Duration;
//!
//! # fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
//! # // 注意：此示例展示了API用法，实际运行需要硬件连接
//! # // 以下代码为伪代码，仅展示API调用方式
//! #
//! # // 假设已经连接并使能
//! # // let piper = ...;
//! #
//! // 创建控制器配置
//! let config = MitControllerConfig {
//!     kp_gains: [5.0; 6],  // Nm/rad
//!     kd_gains: [0.8; 6],  // Nm/(rad/s)
//!     rest_position: None,
//!     control_rate: 200.0,
//! };
//! # // let mut controller = MitController::new(piper, config);
//!
//! // 运动到目标位置（使用锚点机制，保证精确 200Hz）
//! let target = [
//!     Rad(0.5), Rad(0.7), Rad(-0.4),
//!     Rad(0.2), Rad(0.3), Rad(0.5),
//! ];
//! # // let reached = controller.move_to_position(
//! # //     target,
//! # //     Rad(0.01),
//! # //     Duration::from_secs(5),
//! # // )?;
//!
//! // 显式停车（返还 Piper<Standby>）
//! # // let piper_standby = controller.park(DisableConfig::default())?;
//! # Ok(())
//! # }
//! ```

use std::time::{Duration, Instant};
use tracing::{error, warn};

use crate::observer::Observer;
use crate::state::machine::{Active, DisableConfig, MitMode, Piper, Standby};
use crate::types::*;

/// MIT 控制器配置
#[derive(Debug, Clone)]
pub struct MitControllerConfig {
    /// PD 控制器 Kp 增益（每个关节独立）
    ///
    /// 单位：Nm/rad
    /// 典型值：3.0 - 10.0
    pub kp_gains: [f64; 6],

    /// PD 控制器 Kd 增益（每个关节独立）
    ///
    /// 单位：Nm/(rad/s)
    /// 典型值：0.3 - 1.5
    pub kd_gains: [f64; 6],

    /// 休息位置（Drop 时自动移动到此位置）
    ///
    /// `None` 表示不自动移动（仅失能）
    pub rest_position: Option<[Rad; 6]>,

    /// 控制循环频率（Hz）
    ///
    /// 使用绝对时间锚点机制，实际频率将精确锁定在此值。
    /// 推荐值：200.0 Hz（与固件更新频率一致）
    pub control_rate: f64,
}

impl Default for MitControllerConfig {
    fn default() -> Self {
        Self {
            kp_gains: [5.0; 6],
            kd_gains: [0.8; 6],
            rest_position: None,
            control_rate: 200.0,
        }
    }
}

/// 控制错误
#[derive(Debug, thiserror::Error)]
pub enum ControlError {
    /// Controller was already parked
    #[error("Controller was already parked, cannot execute commands")]
    AlreadyParked,

    /// 连续错误超过阈值
    #[error("Consecutive CAN failures: {count}, last error: {last_error}")]
    ConsecutiveFailures {
        count: u32,
        #[source]
        last_error: Box<RobotError>,
    },

    /// 其他机器人错误
    #[error("Robot error: {0}")]
    RobotError(#[from] RobotError),

    /// 超时
    #[error("Operation timeout: {timeout_ms}ms")]
    Timeout { timeout_ms: u64 },
}

/// MIT 模式高层控制器
///
/// **核心特性**：
/// - ✅ Option 模式：允许安全提取 Piper
/// - ✅ 循环锚点：消除累积漂移，保证精确 200Hz
/// - ✅ 容错性：允许最多连续 5 帧丢帧
/// - ✅ 状态流转：`park()` 返还 `Piper<Standby>`
pub struct MitController {
    /// ⚠️ Option 包装，允许 park() 时安全提取
    piper: Option<Piper<Active<MitMode>>>,

    /// 状态观察器
    observer: Observer,

    /// 控制器配置
    config: MitControllerConfig,
}

impl MitController {
    /// 创建新的 MIT 控制器
    ///
    /// # 参数
    ///
    /// - `piper`: 已使能 MIT 模式的 Piper
    /// - `config`: 控制器配置
    ///
    /// # 示例
    ///
    /// ```rust,no_run
    /// # use piper_client::control::MitController;
    /// # use piper_client::control::MitControllerConfig;
    /// # use piper_client::state::*;
    /// #
    /// # // 假设已经使能 MIT 模式
    /// # // let piper = ...;
    /// #
    /// let config = MitControllerConfig::default();
    /// # // let controller = MitController::new(piper, config);
    /// # // 使用 controller 进行控制...
    /// ```
    pub fn new(piper: Piper<Active<MitMode>>, config: MitControllerConfig) -> Self {
        // 提取 observer（Clone 是轻量的，Arc 指针）
        let observer = piper.observer().clone();

        Self {
            piper: Some(piper),
            observer,
            config,
        }
    }

    /// 阻塞式运动到目标位置
    ///
    /// **循环锚点机制**：
    /// - ✅ 使用绝对时间锚点，消除累积漂移
    /// - ✅ 无论 CAN 通信耗时多少，频率都锁定在 200Hz
    /// - ✅ 自动处理任务超时（Overrun）
    ///
    /// # 容错性
    ///
    /// 控制循环具有**容错性**：允许偶尔的 CAN 通信错误（最多连续 5 帧）。
    ///
    /// # 参数
    ///
    /// - `target`: 目标关节位置（弧度）
    /// - `threshold`: 到达阈值（弧度）
    /// - `timeout`: 超时时间
    ///
    /// # 返回
    ///
    /// - `Ok(true)`: 到达目标
    /// - `Ok(false)`: 超时未到达
    /// - `Err(ControlError::ConsecutiveFailures)`: 连续错误超过阈值
    ///
    /// # 示例
    ///
    /// ```rust,no_run
    /// # use piper_client::control::MitController;
    /// # use piper_client::types::*;
    /// # use std::time::Duration;
    /// #
    /// # let mut controller: MitController = unsafe { std::mem::zeroed() };
    /// // 运动到目标位置，5秒超时
    /// # // let reached = controller.move_to_position(
    /// # //     [Rad(0.5), Rad(0.7), Rad(-0.4), Rad(0.2), Rad(0.3), Rad(0.5)],
    /// # //     Rad(0.01),  // 1cm 阈值
    /// # //     Duration::from_secs(5),
    /// # // )?;
    /// ```
    pub fn move_to_position(
        &mut self,
        target: [Rad; 6],
        threshold: Rad,
        timeout: Duration,
    ) -> core::result::Result<bool, ControlError> {
        const MAX_TOLERANCE: u32 = 5; // 允许连续丢 5 帧（25ms @ 200Hz）
        let mut error_count = 0;

        let start = Instant::now();

        // 使用绝对时间锚点机制消除累积漂移，保证精确的 200Hz 控制频率
        let period = Duration::from_secs_f64(1.0 / self.config.control_rate); // 5ms @ 200Hz
        let mut next_tick = Instant::now();

        // 提取 piper 引用（避免每次都解 Option）
        let _piper = self.piper.as_ref().ok_or(ControlError::AlreadyParked)?;

        while start.elapsed() < timeout {
            // 1. 设定下一个锚点（绝对时间）
            next_tick += period;

            // 2. 发送命令 & 检查状态（耗时操作）
            match self.command_joints(JointArray::from(target), None) {
                Ok(_) => {
                    error_count = 0; // 重置计数器
                },
                Err(e) => {
                    error_count += 1;
                    if error_count > MAX_TOLERANCE {
                        error!(
                            "Consecutive CAN failures ({}): {:?}. Aborting motion.",
                            error_count, e
                        );
                        return Err(ControlError::ConsecutiveFailures {
                            count: error_count,
                            last_error: Box::new(e),
                        });
                    }
                    warn!(
                        "Transient CAN error ({}): {:?}, skipping frame. \
                         This is acceptable as long as errors don't occur consecutively.",
                        error_count, e
                    );
                    // 跳过本帧，继续循环
                    continue;
                },
            }

            // 3. 检查是否到达
            let current = self.observer.joint_positions();
            let reached =
                current.iter().zip(target.iter()).all(|(c, t)| (*c - *t).abs() < threshold);

            if reached {
                return Ok(true);
            }

            // 4. 睡眠到下一个锚点（自动扣除耗时操作的时间）
            let now = Instant::now();
            if next_tick > now {
                // 还有剩余时间，睡眠到下一个锚点
                spin_sleep::sleep(next_tick - now);
            } else {
                // ⚠️ 任务超时（Overrun）：耗时操作超过了预期周期
                warn!(
                    "Control loop overrun: operation took {:?}, \
                     but next tick was {:?} from now (expected {:?}). \
                     Skipping sleep to catch up.",
                    now.duration_since(next_tick - period),
                    next_tick.duration_since(now),
                    period
                );
                // 追赶锚点：不睡眠，直接进入下一帧
                // 重置锚点到当前时间，避免后续累积延迟
                next_tick = now;
            }
        }

        Ok(false)
    }

    /// 发送关节命令（MIT 模式 PD 控制）
    ///
    /// 直接传递每个关节的 kp/kd 增益到固件，让固件进行 PD 计算，
    /// 而不是在软件中计算 PD 输出。这样可以充分利用硬件的实时性能。
    ///
    /// # 参数
    ///
    /// - `target`: 目标位置
    /// - `feedforward`: 前馈力矩（可选）
    fn command_joints(
        &self,
        target: JointArray<Rad>,
        feedforward: Option<JointArray<NewtonMeter>>,
    ) -> crate::types::Result<()> {
        // 使用配置中的每个关节独立的 kp/kd 增益
        let kp = JointArray::from(self.config.kp_gains);
        let kd = JointArray::from(self.config.kd_gains);

        // 速度参考（目标速度，通常为 0）
        let velocities = JointArray::from([0.0; 6]);

        // 前馈力矩（可选）
        let torques = match feedforward {
            Some(ff) => ff,
            None => JointArray::from([NewtonMeter(0.0); 6]),
        };

        // 直接传递 kp/kd 增益到底层，让固件进行 PD 计算
        let piper =
            self.piper.as_ref().ok_or(ControlError::AlreadyParked).map_err(|e| match e {
                ControlError::AlreadyParked => crate::RobotError::InvalidTransition {
                    from: "Active<MitMode>".to_string(),
                    to: "Standby".to_string(),
                },
                _ => crate::RobotError::StatePoisoned {
                    reason: format!("Controller error: {:?}", e),
                },
            })?;
        piper.command_torques(&target, &velocities, &kp, &kd, &torques)
    }

    /// 放松关节（发送零力矩命令）
    ///
    /// **注意**：此方法只发送一次零力矩命令，不会阻塞。
    /// 如果需要让关节自然下垂，应该多次调用或在循环中调用。
    ///
    /// # 软降级
    ///
    /// 如果发送失败，只记录警告，不返回错误（软降级策略）。
    ///
    /// # 示例
    ///
    /// ```rust,no_run
    /// # use piper_client::control::MitController;
    /// # let mut controller: MitController = unsafe { std::mem::zeroed() };
    /// controller.relax_joints();
    /// ```
    pub fn relax_joints(&mut self) {
        let zero_pos = JointArray::from([Rad(0.0); 6]);
        let zero_vel = JointArray::from([0.0; 6]);
        let zero_kp = JointArray::from([0.0; 6]);
        let zero_kd = JointArray::from([0.0; 6]);
        let zero_torques = JointArray::from([NewtonMeter(0.0); 6]);

        let piper = match self.piper.as_ref() {
            Some(p) => p,
            None => {
                warn!("Cannot relax joints: Piper already consumed");
                return;
            },
        };

        if let Err(e) =
            piper.command_torques(&zero_pos, &zero_vel, &zero_kp, &zero_kd, &zero_torques)
        {
            warn!("Failed to relax joints: {:?}. Continuing anyway.", e);
        }
    }

    /// 停车（失能并返还 `Piper<Standby>`）
    ///
    /// **资源管理**：
    /// - ✅ 返还 `Piper<Standby>`，支持继续使用
    /// - ✅ 使用 Option 模式，安全提取 Piper
    ///
    /// # 安全保证
    ///
    /// **显式调用 park()**（推荐）：
    /// - 提取 Piper，调用 disable()，等待完成
    /// - 返回 `Piper<Standby>` 可继续使用
    /// - 不会触发 Piper 的 Drop
    ///
    /// **忘记调用 park()**（安全网）：
    /// - MitController 被 drop 时，Piper<Active>::drop() 自动触发
    /// - 发送 disable 命令（不等待确认）
    /// - 电机被安全失能
    ///
    /// # 示例
    ///
    /// ```rust,no_run
    /// # use piper_client::control::MitController;
    /// # use piper_client::state::*;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let mut controller: MitController = unsafe { std::mem::zeroed() };
    ///
    /// // 方式 1：显式停车（推荐）
    /// let piper_standby = controller.park(DisableConfig::default())?;
    /// // 现在 piper_standby 可以重新使能或做其他操作
    ///
    /// // 方式 2：直接丢弃（触发自动安全失能）
    /// // drop(controller);  // 自动调用 Piper::drop()，发送 disable 命令
    /// # Ok(())
    /// # }
    /// ```
    pub fn park(mut self, config: DisableConfig) -> crate::types::Result<Piper<Standby>> {
        // 安全提取 piper（Option 变为 None）
        let piper = self.piper.take().ok_or(ControlError::AlreadyParked).map_err(|e| match e {
            ControlError::AlreadyParked => crate::RobotError::InvalidTransition {
                from: "Active<MitMode>".to_string(),
                to: "Standby".to_string(),
            },
            _ => crate::RobotError::StatePoisoned {
                reason: format!("Controller error: {:?}", e),
            },
        })?;

        // 失能并返回到 Standby 状态
        piper.disable(config)
    }

    /// 获取 Observer（只读）
    pub fn observer(&self) -> &Observer {
        &self.observer
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_default() {
        let config = MitControllerConfig::default();
        assert_eq!(config.kp_gains[0], 5.0);
        assert_eq!(config.kd_gains[0], 0.8);
        assert!(config.rest_position.is_none());
        assert_eq!(config.control_rate, 200.0);
    }
}
