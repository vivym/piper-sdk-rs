//! Loop Runner - 控制循环包装器
//!
//! 提供高级控制循环接口，处理定时、dt 钳位、异常检测等。
//!
//! # 核心功能
//!
//! - **精确定时**: 使用 `spin_sleep` 实现低抖动延时
//! - **dt 钳位**: 限制异常大的时间步长
//! - **时间跳变处理**: 自动调用 `on_time_jump()`
//! - **错误传播**: 透明传播控制器和命令错误
//!
//! # 使用场景
//!
//! ```rust,ignore
//! use piper_sdk::client::control::{run_controller, LoopConfig, Controller};
//! use piper_sdk::client::state::Piper;
//!
//! # fn example(piper: Piper<()>, controller: impl Controller) -> Result<(), Box<dyn std::error::Error>> {
//! let config = LoopConfig {
//!     frequency_hz: 100.0,              // 100Hz 控制频率
//!     dt_clamp_multiplier: 2.0,         // dt 最大为 2x 标称值
//!     max_iterations: Some(1000),       // 运行 1000 次后停止
//! };
//!
//! run_controller(piper, controller, config)?;
//! # Ok(())
//! # }
//! ```

use super::controller::Controller;
use crate::client::types::RobotError;
use crate::client::{MotionCommander, Observer};
use std::time::{Duration, Instant};

/// 控制循环配置
#[derive(Debug, Clone)]
pub struct LoopConfig {
    /// 控制频率（Hz）
    ///
    /// 例如：100.0 表示 100Hz（10ms 周期）
    pub frequency_hz: f64,

    /// dt 钳位倍数
    ///
    /// 当实际 dt 超过标称周期的此倍数时，将触发 `on_time_jump()` 并钳位 dt。
    ///
    /// 例如：2.0 表示 dt 最大为 2 * (1 / frequency_hz)
    pub dt_clamp_multiplier: f64,

    /// 最大迭代次数（None 表示无限循环）
    ///
    /// 用于测试或定时运行。
    pub max_iterations: Option<usize>,
}

impl Default for LoopConfig {
    fn default() -> Self {
        LoopConfig {
            frequency_hz: 100.0,      // 默认 100Hz
            dt_clamp_multiplier: 2.0, // 默认 2x
            max_iterations: None,     // 默认无限循环
        }
    }
}

/// 运行控制循环
///
/// 这是一个阻塞函数，会持续运行控制循环直到：
/// - 发生错误
/// - 达到 `max_iterations`（如果设置）
/// - 用户中断（Ctrl+C）
///
/// # 参数
///
/// - `observer`: 状态观察器（用于读取当前状态）
/// - `commander`: 命令发送器（用于发送控制命令）
/// - `controller`: 控制器（实现 `Controller` trait）
/// - `config`: 循环配置
///
/// # 返回
///
/// - `Ok(())`: 正常结束（达到 max_iterations）
/// - `Err(RobotError)`: 发生错误
///
/// # 时间处理
///
/// - 计算实际 dt
/// - 如果 dt > max_dt，调用 `controller.on_time_jump(real_dt)`，然后钳位 dt
/// - 使用钳位后的 dt 调用 `controller.tick()`
///
/// # 示例
///
/// ```rust,ignore
/// use piper_sdk::client::control::{run_controller, LoopConfig};
/// # use piper_sdk::client::client::{Observer, MotionCommander};
/// # use piper_sdk::client::control::Controller;
/// # fn example(
/// #     observer: Observer,
/// #     commander: MotionCommander,
/// #     controller: impl Controller,
/// # ) -> Result<(), Box<dyn std::error::Error>> {
///
/// let config = LoopConfig {
///     frequency_hz: 200.0,  // 200Hz 高频控制
///     dt_clamp_multiplier: 1.5,
///     max_iterations: Some(2000),  // 运行 10 秒后停止
/// };
///
/// run_controller(observer, commander, controller, config)?;
/// # Ok(())
/// # }
/// ```
pub fn run_controller<C>(
    observer: Observer,
    commander: MotionCommander,
    mut controller: C,
    config: LoopConfig,
) -> Result<(), RobotError>
where
    C: Controller,
    RobotError: From<C::Error>,
{
    // ✅ 输入验证
    if config.frequency_hz <= 0.0 {
        return Err(RobotError::ConfigError(format!(
            "Invalid frequency_hz: {} (must be > 0)",
            config.frequency_hz
        )));
    }
    if config.frequency_hz > 10000.0 {
        tracing::warn!(
            "Very high control frequency: {} Hz. This may cause performance issues.",
            config.frequency_hz
        );
    }
    if config.dt_clamp_multiplier <= 0.0 {
        return Err(RobotError::ConfigError(format!(
            "Invalid dt_clamp_multiplier: {} (must be > 0)",
            config.dt_clamp_multiplier
        )));
    }

    // 计算标称周期和最大 dt
    let nominal_period = Duration::from_secs_f64(1.0 / config.frequency_hz);
    let max_dt = nominal_period.mul_f64(config.dt_clamp_multiplier);

    let mut last_time = Instant::now();
    let mut iteration = 0;

    loop {
        // 检查是否达到最大迭代次数
        if let Some(max_iter) = config.max_iterations
            && iteration >= max_iter
        {
            return Ok(());
        }

        // 1. 计算 dt
        let now = Instant::now();
        let real_dt = now - last_time;
        let mut dt = real_dt;

        // 2. dt 钳位
        if real_dt > max_dt {
            // 调用 on_time_jump 处理异常
            controller.on_time_jump(real_dt).map_err(RobotError::from)?;

            // 钳位 dt
            dt = max_dt;
        }

        // 3. 读取当前状态
        let current = observer.joint_positions();

        // 4. 调用控制器
        let output = controller.tick(&current, dt).map_err(RobotError::from)?;

        // 5. 发送命令
        commander.command_torques(output)?;

        // 6. 更新时间
        last_time = now;
        iteration += 1;

        // 7. 休眠到下一个周期
        std::thread::sleep(nominal_period);
    }
}

/// 使用 spin_sleep 的高精度控制循环
///
/// 与 `run_controller()` 类似，但使用 `spin_sleep` 实现更低的延时抖动。
///
/// ⚠️ **注意**: `spin_sleep` 会占用更多 CPU，适合对实时性要求极高的场景。
pub fn run_controller_spin<C>(
    observer: Observer,
    commander: MotionCommander,
    mut controller: C,
    config: LoopConfig,
) -> Result<(), RobotError>
where
    C: Controller,
    RobotError: From<C::Error>,
{
    use spin_sleep::SpinSleeper;

    // ✅ 输入验证
    if config.frequency_hz <= 0.0 {
        return Err(RobotError::ConfigError(format!(
            "Invalid frequency_hz: {} (must be > 0)",
            config.frequency_hz
        )));
    }
    if config.frequency_hz > 10000.0 {
        tracing::warn!(
            "Very high control frequency: {} Hz. This may cause performance issues.",
            config.frequency_hz
        );
    }
    if config.dt_clamp_multiplier <= 0.0 {
        return Err(RobotError::ConfigError(format!(
            "Invalid dt_clamp_multiplier: {} (must be > 0)",
            config.dt_clamp_multiplier
        )));
    }

    let nominal_period = Duration::from_secs_f64(1.0 / config.frequency_hz);
    let max_dt = nominal_period.mul_f64(config.dt_clamp_multiplier);
    let sleeper = SpinSleeper::default();

    let mut last_time = Instant::now();
    let mut iteration = 0;

    loop {
        if let Some(max_iter) = config.max_iterations
            && iteration >= max_iter
        {
            return Ok(());
        }

        let now = Instant::now();
        let real_dt = now - last_time;
        let mut dt = real_dt;

        if real_dt > max_dt {
            controller.on_time_jump(real_dt).map_err(RobotError::from)?;
            dt = max_dt;
        }

        let current = observer.joint_positions();
        let output = controller.tick(&current, dt).map_err(RobotError::from)?;
        commander.command_torques(output)?;

        last_time = now;
        iteration += 1;

        sleeper.sleep(nominal_period);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_loop_config_default() {
        let config = LoopConfig::default();
        assert_eq!(config.frequency_hz, 100.0);
        assert_eq!(config.dt_clamp_multiplier, 2.0);
        assert_eq!(config.max_iterations, None);
    }

    #[test]
    fn test_loop_config_custom() {
        let config = LoopConfig {
            frequency_hz: 200.0,
            dt_clamp_multiplier: 1.5,
            max_iterations: Some(1000),
        };
        assert_eq!(config.frequency_hz, 200.0);
        assert_eq!(config.dt_clamp_multiplier, 1.5);
        assert_eq!(config.max_iterations, Some(1000));
    }
}
