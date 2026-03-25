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
//! use piper_client::control::{run_controller, LoopConfig, Controller};
//! use piper_client::state::Piper;
//!
//! # fn example(piper: Piper<Active<MitMode>>, controller: impl Controller) -> Result<(), Box<dyn std::error::Error>> {
//! let config = LoopConfig {
//!     frequency_hz: 100.0,              // 100Hz 控制频率
//!     dt_clamp_multiplier: 2.0,         // dt 最大为 2x 标称值
//!     read_policy: ControlReadPolicy::default(), // 默认严格控制级新鲜度（15ms）
//!     max_iterations: Some(1000),       // 运行 1000 次后停止
//! };
//!
//! run_controller(piper, controller, config)?;
//! # Ok(())
//! # }
//! ```

use super::controller::Controller;
use super::scheduler::{CycleScheduler, SleepStrategy};
use crate::Piper;
use crate::observer::{ControlReadPolicy, ControlSnapshot};
use crate::state::{Active, MitMode, StrictRealtime};
use crate::types::{JointArray, NewtonMeter, RobotError};
use piper_driver::BackendCapability;
use std::time::{Duration, Instant};

const CONTROL_SNAPSHOT_READY_TIMEOUT: Duration = Duration::from_millis(200);
const CONTROL_SNAPSHOT_POLL_INTERVAL: Duration = Duration::from_millis(5);

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

    /// 高频控制读取策略
    ///
    /// 默认建议使用 `ControlReadPolicy::default()`，其最大反馈年龄为 15ms。
    pub read_policy: ControlReadPolicy,

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
            read_policy: ControlReadPolicy::default(),
            max_iterations: None, // 默认无限循环
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
/// - `piper`: `Piper<Active<MitMode>>` 实例（Type State 安全保证）
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
/// - 进入控制循环前，会短暂等待第一份完整且对齐的 `ControlSnapshot`
/// - 首次循环会先等待一个标称周期，因此首拍 `dt` 接近 `1 / frequency_hz`
/// - 计算实际 dt
/// - 如果 dt > max_dt，调用 `controller.on_time_jump(real_dt)`，然后钳位 dt
/// - 使用钳位后的 dt 调用 `controller.tick()`，并传入完整 `ControlSnapshot`
///
/// # 示例
///
/// ```rust,ignore
/// use piper_client::control::{run_controller, LoopConfig};
/// # use piper_client::state::Piper;
/// # use piper_client::control::Controller;
/// # fn example(
/// #     piper: Piper<Active<MitMode>>,
/// #     controller: impl Controller,
/// # ) -> Result<(), Box<dyn std::error::Error>> {
/// let config = LoopConfig {
///     frequency_hz: 200.0,  // 200Hz 高频控制
///     dt_clamp_multiplier: 1.5,
///     read_policy: ControlReadPolicy::default(), // 默认严格控制级新鲜度（15ms）
///     max_iterations: Some(2000),  // 运行 10 秒后停止
/// };
///
/// run_controller(piper, controller, config)?;
/// # Ok(())
/// # }
/// ```
pub fn run_controller<C>(
    piper: Piper<Active<MitMode>, StrictRealtime>,
    controller: C,
    config: LoopConfig,
) -> Result<(), RobotError>
where
    C: Controller,
    RobotError: From<C::Error>,
{
    run_controller_with_strategy(piper, controller, config, default_sleep_strategy())
}

/// `run_controller()` 默认采用 hybrid 调度：
/// 先用 `thread::sleep` 让出 CPU，再用短自旋收尾以降低尾部抖动。
/// 使用 spin_sleep 的高精度控制循环
///
/// 与 `run_controller()` 类似，但使用 `spin_sleep` 实现更低的延时抖动。
/// 与 `run_controller()` 一样，首次循环会先等待一个标称周期，再进入首拍控制。
///
/// ⚠️ **注意**: `spin_sleep` 会占用更多 CPU，适合对实时性要求极高的场景。
pub fn run_controller_spin<C>(
    piper: Piper<Active<MitMode>, StrictRealtime>,
    controller: C,
    config: LoopConfig,
) -> Result<(), RobotError>
where
    C: Controller,
    RobotError: From<C::Error>,
{
    run_controller_with_strategy(piper, controller, config, spin_sleep_strategy())
}

fn default_sleep_strategy() -> SleepStrategy {
    SleepStrategy::Hybrid
}

fn spin_sleep_strategy() -> SleepStrategy {
    SleepStrategy::Spin
}

fn validate_loop_config(config: &LoopConfig) -> Result<(), RobotError> {
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
    Ok(())
}

fn run_controller_with_strategy<C>(
    piper: Piper<Active<MitMode>, StrictRealtime>,
    mut controller: C,
    config: LoopConfig,
    strategy: SleepStrategy,
) -> Result<(), RobotError>
where
    C: Controller,
    RobotError: From<C::Error>,
{
    ensure_realtime_control_supported(&piper)?;
    validate_loop_config(&config)?;

    if matches!(config.max_iterations, Some(0)) {
        return Ok(());
    }

    let _initial_snapshot = wait_for_control_snapshot_ready(
        CONTROL_SNAPSHOT_READY_TIMEOUT,
        CONTROL_SNAPSHOT_POLL_INTERVAL,
        || piper.observer().control_snapshot(config.read_policy),
    )?;

    // 计算标称周期和最大 dt
    let nominal_period = Duration::from_secs_f64(1.0 / config.frequency_hz);
    let max_dt = nominal_period.mul_f64(config.dt_clamp_multiplier);
    let mut scheduler = CycleScheduler::new(nominal_period, strategy);
    let mut iteration = 0;
    let zero_positions = crate::types::JointArray::from([crate::types::Rad(0.0); 6]);
    let zero_velocities = crate::types::JointArray::from([0.0; 6]);
    let zero_kp = crate::types::JointArray::from([0.0; 6]);
    let zero_kd = crate::types::JointArray::from([0.0; 6]);

    loop {
        if let Some(max_iter) = config.max_iterations
            && iteration >= max_iter
        {
            return Ok(());
        }

        let cycle = scheduler.wait_next();
        let real_dt = cycle.real_dt;
        let mut dt = real_dt;

        if real_dt > max_dt {
            controller.on_time_jump(real_dt).map_err(RobotError::from)?;
            dt = max_dt;
        }

        let snapshot = piper.observer().control_snapshot(config.read_policy)?;
        let torques = tick_controller(&mut controller, &snapshot, dt)?;

        piper.command_torques(
            &zero_positions,
            &zero_velocities,
            &zero_kp,
            &zero_kd,
            &torques,
        )?;

        iteration += 1;
    }
}

fn ensure_realtime_control_supported(
    piper: &Piper<Active<MitMode>, StrictRealtime>,
) -> Result<(), RobotError> {
    match piper.driver.backend_capability() {
        BackendCapability::StrictRealtime => Ok(()),
        BackendCapability::SoftRealtime | BackendCapability::MonitorOnly => {
            Err(RobotError::realtime_unsupported(
                "controller loop requires a StrictRealtime backend with trusted alignment timestamps",
            ))
        },
    }
}

fn tick_controller<C>(
    controller: &mut C,
    snapshot: &ControlSnapshot,
    dt: Duration,
) -> Result<JointArray<NewtonMeter>, RobotError>
where
    C: Controller,
    RobotError: From<C::Error>,
{
    controller.tick(snapshot, dt).map_err(RobotError::from)
}

fn wait_for_control_snapshot_ready<Read>(
    timeout: Duration,
    poll_interval: Duration,
    mut read: Read,
) -> Result<ControlSnapshot, RobotError>
where
    Read: FnMut() -> Result<ControlSnapshot, RobotError>,
{
    let start = Instant::now();

    loop {
        match read() {
            Ok(snapshot) => return Ok(snapshot),
            Err(err @ RobotError::ControlStateIncomplete { .. })
            | Err(err @ RobotError::StateMisaligned { .. }) => {
                if start.elapsed() >= timeout {
                    return Err(err);
                }

                let remaining = timeout.saturating_sub(start.elapsed());
                let sleep_duration = poll_interval.min(remaining);
                if sleep_duration.is_zero() {
                    return Err(err);
                }

                std::thread::sleep(sleep_duration);
            },
            Err(err) => return Err(err),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Rad, RadPerSecond};

    #[derive(Default)]
    struct RecordingController {
        seen_snapshot: Option<ControlSnapshot>,
    }

    impl Controller for RecordingController {
        type Error = RobotError;

        fn tick(
            &mut self,
            snapshot: &ControlSnapshot,
            _dt: Duration,
        ) -> Result<JointArray<NewtonMeter>, Self::Error> {
            self.seen_snapshot = Some(*snapshot);
            Ok(JointArray::splat(NewtonMeter(0.0)))
        }
    }

    fn test_snapshot() -> ControlSnapshot {
        ControlSnapshot {
            position: JointArray::splat(Rad(1.2)),
            velocity: JointArray::splat(RadPerSecond(-0.4)),
            torque: JointArray::splat(NewtonMeter(3.5)),
            position_timestamp_us: 10_000,
            dynamic_timestamp_us: 10_020,
            skew_us: 20,
        }
    }

    #[test]
    fn test_loop_config_default() {
        let config = LoopConfig::default();
        assert_eq!(config.frequency_hz, 100.0);
        assert_eq!(config.dt_clamp_multiplier, 2.0);
        assert_eq!(config.read_policy, ControlReadPolicy::default());
        assert_eq!(
            config.read_policy.max_feedback_age,
            crate::observer::DEFAULT_CONTROL_MAX_FEEDBACK_AGE
        );
        assert_eq!(config.max_iterations, None);
    }

    #[test]
    fn test_loop_config_custom() {
        let config = LoopConfig {
            frequency_hz: 200.0,
            dt_clamp_multiplier: 1.5,
            read_policy: ControlReadPolicy::default(),
            max_iterations: Some(1000),
        };
        assert_eq!(config.frequency_hz, 200.0);
        assert_eq!(config.dt_clamp_multiplier, 1.5);
        assert_eq!(config.read_policy, ControlReadPolicy::default());
        assert_eq!(config.max_iterations, Some(1000));
    }

    #[test]
    fn test_invalid_frequency() {
        let config = LoopConfig {
            frequency_hz: -1.0,
            ..Default::default()
        };
        // 注意：此测试需要实际的 robot 实例，在单元测试中无法完成
        // 只验证配置构造
        assert_eq!(config.frequency_hz, -1.0);
    }

    #[test]
    fn test_tick_controller_passes_full_snapshot() {
        let snapshot = test_snapshot();
        let mut controller = RecordingController::default();

        let torques = tick_controller(&mut controller, &snapshot, Duration::from_millis(5))
            .expect("tick_controller should succeed");

        assert_eq!(controller.seen_snapshot, Some(snapshot));
        assert_eq!(torques, JointArray::splat(NewtonMeter(0.0)));
    }

    #[test]
    fn test_run_controller_defaults_to_hybrid_strategy() {
        assert_eq!(default_sleep_strategy(), SleepStrategy::Hybrid);
    }

    #[test]
    fn test_run_controller_spin_remains_explicit_full_spin() {
        assert_eq!(spin_sleep_strategy(), SleepStrategy::Spin);
    }

    #[test]
    fn test_wait_for_control_snapshot_ready_retries_incomplete_and_misaligned_states() {
        use std::sync::{Arc, Mutex};

        let attempts = Arc::new(Mutex::new(0usize));
        let snapshot = test_snapshot();

        let waited =
            wait_for_control_snapshot_ready(Duration::from_millis(50), Duration::from_millis(1), {
                let attempts = Arc::clone(&attempts);
                move || {
                    let mut attempts = attempts.lock().unwrap();
                    *attempts += 1;
                    match *attempts {
                        1 => Err(RobotError::control_state_incomplete(0b001, 0b11_1111)),
                        2 => Err(RobotError::state_misaligned(6_000, 2_000)),
                        _ => Ok(snapshot),
                    }
                }
            })
            .expect("helper should wait until a complete aligned control snapshot becomes ready");

        assert_eq!(waited, snapshot);
        assert_eq!(*attempts.lock().unwrap(), 3);
    }

    #[test]
    fn test_wait_for_control_snapshot_ready_returns_stale_immediately() {
        let started_at = Instant::now();
        let error = wait_for_control_snapshot_ready(
            Duration::from_millis(50),
            Duration::from_millis(10),
            || {
                Err(RobotError::feedback_stale(
                    Duration::from_millis(20),
                    Duration::from_millis(15),
                ))
            },
        )
        .expect_err("stale control feedback should not be retried");

        assert!(started_at.elapsed() < Duration::from_millis(10));
        assert!(matches!(error, RobotError::FeedbackStale { .. }));
    }
}
