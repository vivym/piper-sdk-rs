//! PID Controller - 比例-积分-微分控制器
//!
//! 实现经典的 PID 控制算法，适用于关节位置控制。
//!
//! # 算法
//!
//! ```text
//! output = Kp * e + Ki * ∫e dt + Kd * de/dt
//! ```
//!
//! 其中：
//! - `e` = 目标位置 - 当前位置（误差）
//! - `∫e dt` = 累积误差（积分项）
//! - `de/dt` = 误差变化率（微分项）
//!
//! # 特性
//!
//! - **积分饱和保护**: 限制积分项累积，防止积分饱和（Integral Windup）
//! - **时间跳变处理**: 正确处理 `dt` 异常，只重置微分项，保留积分项
//! - **强类型单位**: 使用 `Rad` 和 `NewtonMeter` 确保单位正确
//!
//! # 示例
//!
//! ```rust,no_run
//! use piper_sdk::high_level::control::{PidController, Controller};
//! use piper_sdk::high_level::types::{JointArray, Rad};
//!
//! // 创建 PID 控制器
//! let target = JointArray::from([Rad(1.0); 6]);
//! let mut pid = PidController::new(target)
//!     .with_gains(10.0, 0.5, 0.1)
//!     .with_integral_limit(5.0)
//!     .with_output_limit(50.0);
//!
//! // 在控制循环中使用
//! # use std::time::Duration;
//! # let current = JointArray::from([Rad(0.5); 6]);
//! # let dt = Duration::from_millis(10);
//! let output = pid.tick(&current, dt).unwrap();
//! ```

use super::controller::Controller;
use crate::high_level::types::{JointArray, NewtonMeter, Rad};
use std::time::Duration;

/// PID 控制器
///
/// 实现经典的比例-积分-微分控制算法。
#[derive(Debug, Clone)]
pub struct PidController {
    /// 目标位置
    target: JointArray<Rad>,

    /// 比例增益 (Kp)
    kp: f64,

    /// 积分增益 (Ki)
    ki: f64,

    /// 微分增益 (Kd)
    kd: f64,

    /// 积分项累积值
    integral: JointArray<f64>,

    /// 上一次的误差（用于计算微分）
    last_error: JointArray<f64>,

    /// 积分项限制（防止积分饱和）
    integral_limit: f64,

    /// 输出力矩限制
    output_limit: f64,
}

impl PidController {
    /// 创建新的 PID 控制器
    ///
    /// # 参数
    ///
    /// - `target`: 目标关节位置
    ///
    /// # 默认参数
    ///
    /// - Kp = 0.0, Ki = 0.0, Kd = 0.0（需要手动设置）
    /// - 积分限制 = 10.0
    /// - 输出限制 = 100.0 Nm
    ///
    /// # 示例
    ///
    /// ```rust
    /// # use piper_sdk::high_level::control::PidController;
    /// # use piper_sdk::high_level::types::{JointArray, Rad};
    /// let target = JointArray::from([Rad(1.0); 6]);
    /// let pid = PidController::new(target);
    /// ```
    pub fn new(target: JointArray<Rad>) -> Self {
        PidController {
            target,
            kp: 0.0,
            ki: 0.0,
            kd: 0.0,
            integral: JointArray::from([0.0; 6]),
            last_error: JointArray::from([0.0; 6]),
            integral_limit: 10.0,
            output_limit: 100.0,
        }
    }

    /// 设置 PID 增益
    ///
    /// # 参数
    ///
    /// - `kp`: 比例增益
    /// - `ki`: 积分增益
    /// - `kd`: 微分增益
    ///
    /// # 示例
    ///
    /// ```rust
    /// # use piper_sdk::high_level::control::PidController;
    /// # use piper_sdk::high_level::types::{JointArray, Rad};
    /// # let target = JointArray::from([Rad(1.0); 6]);
    /// let pid = PidController::new(target)
    ///     .with_gains(10.0, 0.5, 0.1);
    /// ```
    pub fn with_gains(mut self, kp: f64, ki: f64, kd: f64) -> Self {
        self.kp = kp;
        self.ki = ki;
        self.kd = kd;
        self
    }

    /// 设置积分项限制
    ///
    /// 防止积分饱和（Integral Windup）。
    ///
    /// # 参数
    ///
    /// - `limit`: 积分项绝对值的最大值
    ///
    /// # 示例
    ///
    /// ```rust
    /// # use piper_sdk::high_level::control::PidController;
    /// # use piper_sdk::high_level::types::{JointArray, Rad};
    /// # let target = JointArray::from([Rad(1.0); 6]);
    /// let pid = PidController::new(target)
    ///     .with_integral_limit(5.0);
    /// ```
    pub fn with_integral_limit(mut self, limit: f64) -> Self {
        self.integral_limit = limit;
        self
    }

    /// 设置输出力矩限制
    ///
    /// # 参数
    ///
    /// - `limit`: 输出力矩绝对值的最大值（Nm）
    ///
    /// # 示例
    ///
    /// ```rust
    /// # use piper_sdk::high_level::control::PidController;
    /// # use piper_sdk::high_level::types::{JointArray, Rad};
    /// # let target = JointArray::from([Rad(1.0); 6]);
    /// let pid = PidController::new(target)
    ///     .with_output_limit(50.0);
    /// ```
    pub fn with_output_limit(mut self, limit: f64) -> Self {
        self.output_limit = limit;
        self
    }

    /// 更新目标位置
    ///
    /// # 参数
    ///
    /// - `target`: 新的目标关节位置
    ///
    /// # 示例
    ///
    /// ```rust
    /// # use piper_sdk::high_level::control::PidController;
    /// # use piper_sdk::high_level::types::{JointArray, Rad};
    /// # let target = JointArray::from([Rad(1.0); 6]);
    /// let mut pid = PidController::new(target);
    /// pid.set_target(JointArray::from([Rad(2.0); 6]));
    /// ```
    pub fn set_target(&mut self, target: JointArray<Rad>) {
        self.target = target;
    }

    /// 获取当前目标位置
    pub fn target(&self) -> JointArray<Rad> {
        self.target
    }

    /// 获取当前积分项
    ///
    /// 用于调试和监控。
    pub fn integral(&self) -> JointArray<f64> {
        self.integral
    }
}

impl Controller for PidController {
    type Error = std::io::Error;

    fn tick(
        &mut self,
        current: &JointArray<Rad>,
        dt: Duration,
    ) -> Result<JointArray<NewtonMeter>, Self::Error> {
        let dt_sec = dt.as_secs_f64();

        // 防止除零
        if dt_sec <= 0.0 {
            tracing::warn!(
                "PID controller received zero or negative dt: {:?}, returning zero output",
                dt
            );
            return Ok(JointArray::from([NewtonMeter(0.0); 6]));
        }

        // 1. 计算误差
        let error = self.target.map_with(*current, |t, c| (t - c).0);

        // 2. 比例项（P）
        let p_term = error.map(|e| self.kp * e);

        // 3. 积分项（I）+ 饱和保护
        self.integral = self.integral.map_with(error, |i, e| {
            let new_i = i + e * dt_sec;
            // 钳位到 [-integral_limit, +integral_limit]
            new_i.clamp(-self.integral_limit, self.integral_limit)
        });
        let i_term = self.integral.map(|i| self.ki * i);

        // 4. 微分项（D）
        let d_term = error.map_with(self.last_error, |e, le| self.kd * (e - le) / dt_sec);

        // 5. 更新上一次误差
        self.last_error = error;

        // 6. 计算总输出
        let output = p_term.map_with(i_term, |p, i| p + i).map_with(d_term, |pi, d| pi + d);

        // 7. 钳位输出
        let clamped_output =
            output.map(|o| NewtonMeter(o.clamp(-self.output_limit, self.output_limit)));

        Ok(clamped_output)
    }

    fn on_time_jump(&mut self, dt: Duration) -> Result<(), Self::Error> {
        tracing::warn!(
            "PID controller detected time jump: {:?}, resetting derivative term only",
            dt
        );

        // ✅ 只重置微分项
        self.last_error = JointArray::from([0.0; 6]);

        // ❌ 不要清零积分项！
        // 原因：机械臂可能依赖积分项对抗重力
        // 清零会导致机械臂瞬间下坠（Sagging）

        Ok(())
    }

    fn reset(&mut self) -> Result<(), Self::Error> {
        // 完全重置控制器状态
        self.integral = JointArray::from([0.0; 6]);
        self.last_error = JointArray::from([0.0; 6]);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pid_new() {
        let target = JointArray::from([Rad(1.0); 6]);
        let pid = PidController::new(target);

        assert_eq!(pid.kp, 0.0);
        assert_eq!(pid.ki, 0.0);
        assert_eq!(pid.kd, 0.0);
        assert_eq!(pid.integral_limit, 10.0);
        assert_eq!(pid.output_limit, 100.0);
    }

    #[test]
    fn test_pid_builder() {
        let target = JointArray::from([Rad(1.0); 6]);
        let pid = PidController::new(target)
            .with_gains(10.0, 0.5, 0.1)
            .with_integral_limit(5.0)
            .with_output_limit(50.0);

        assert_eq!(pid.kp, 10.0);
        assert_eq!(pid.ki, 0.5);
        assert_eq!(pid.kd, 0.1);
        assert_eq!(pid.integral_limit, 5.0);
        assert_eq!(pid.output_limit, 50.0);
    }

    #[test]
    fn test_pid_proportional_only() {
        let target = JointArray::from([Rad(1.0); 6]);
        let mut pid = PidController::new(target).with_gains(10.0, 0.0, 0.0);

        let current = JointArray::from([Rad(0.5); 6]);
        let dt = Duration::from_millis(10);

        let output = pid.tick(&current, dt).unwrap();

        // 误差 = 1.0 - 0.5 = 0.5
        // 输出 = 10.0 * 0.5 = 5.0
        assert!((output[0].0 - 5.0).abs() < 1e-10);
    }

    #[test]
    fn test_pid_integral_accumulation() {
        let target = JointArray::from([Rad(1.0); 6]);
        let mut pid = PidController::new(target).with_gains(0.0, 1.0, 0.0); // 只有积分项

        let current = JointArray::from([Rad(0.5); 6]);
        let dt = Duration::from_millis(100); // 0.1 秒

        // 第一次 tick
        let output1 = pid.tick(&current, dt).unwrap();
        // 误差 = 0.5, 积分 = 0.5 * 0.1 = 0.05
        // 输出 = 1.0 * 0.05 = 0.05
        assert!((output1[0].0 - 0.05).abs() < 1e-10);

        // 第二次 tick
        let output2 = pid.tick(&current, dt).unwrap();
        // 积分 = 0.05 + 0.5 * 0.1 = 0.1
        // 输出 = 1.0 * 0.1 = 0.1
        assert!((output2[0].0 - 0.1).abs() < 1e-10);
    }

    #[test]
    fn test_pid_integral_saturation() {
        let target = JointArray::from([Rad(1.0); 6]);
        let mut pid = PidController::new(target).with_gains(0.0, 1.0, 0.0).with_integral_limit(0.5); // 积分限制

        let current = JointArray::from([Rad(0.0); 6]);
        let dt = Duration::from_secs(1);

        // 误差 = 1.0, 每秒累积 1.0
        // 但积分被限制在 0.5
        for _ in 0..10 {
            pid.tick(&current, dt).unwrap();
        }

        // 积分应该被钳位到 0.5
        assert!((pid.integral()[0] - 0.5).abs() < 1e-10);
    }

    #[test]
    fn test_pid_derivative_term() {
        let target = JointArray::from([Rad(1.0); 6]);
        let mut pid = PidController::new(target).with_gains(0.0, 0.0, 1.0); // 只有微分项

        let dt = Duration::from_millis(100);

        // 第一次：误差从 0 变化
        let current1 = JointArray::from([Rad(0.5); 6]);
        let output1 = pid.tick(&current1, dt).unwrap();
        // 误差 = 0.5, 上次误差 = 0, 变化率 = 0.5 / 0.1 = 5.0
        // 输出 = 1.0 * 5.0 = 5.0
        assert!((output1[0].0 - 5.0).abs() < 1e-10);

        // 第二次：误差不变
        let output2 = pid.tick(&current1, dt).unwrap();
        // 误差变化 = 0, 输出 = 0
        assert!((output2[0].0 - 0.0).abs() < 1e-10);
    }

    #[test]
    fn test_pid_output_clamping() {
        let target = JointArray::from([Rad(100.0); 6]);
        let mut pid =
            PidController::new(target).with_gains(100.0, 0.0, 0.0).with_output_limit(50.0);

        let current = JointArray::from([Rad(0.0); 6]);
        let dt = Duration::from_millis(10);

        let output = pid.tick(&current, dt).unwrap();

        // 理论输出 = 100.0 * 100.0 = 10000.0
        // 但被钳位到 50.0
        assert!((output[0].0 - 50.0).abs() < 1e-10);
    }

    #[test]
    fn test_pid_on_time_jump_preserves_integral() {
        let target = JointArray::from([Rad(1.0); 6]);
        let mut pid = PidController::new(target).with_gains(0.0, 1.0, 1.0);

        let current = JointArray::from([Rad(0.5); 6]);
        let dt = Duration::from_secs(1);

        // 累积一些积分
        pid.tick(&current, dt).unwrap();
        let integral_before = pid.integral()[0];
        assert!(integral_before > 0.0);

        // 调用 on_time_jump
        pid.on_time_jump(Duration::from_secs(10)).unwrap();

        // ✅ 积分应该保留
        let integral_after = pid.integral()[0];
        assert_eq!(integral_before, integral_after);

        // ✅ 微分项应该被重置
        assert_eq!(pid.last_error[0], 0.0);
    }

    #[test]
    fn test_pid_reset() {
        let target = JointArray::from([Rad(1.0); 6]);
        let mut pid = PidController::new(target).with_gains(1.0, 1.0, 1.0);

        let current = JointArray::from([Rad(0.5); 6]);
        let dt = Duration::from_secs(1);

        // 累积一些状态
        pid.tick(&current, dt).unwrap();
        assert!(pid.integral()[0] != 0.0);
        assert!(pid.last_error[0] != 0.0);

        // 重置
        pid.reset().unwrap();

        // 所有状态应该被清零
        assert_eq!(pid.integral()[0], 0.0);
        assert_eq!(pid.last_error[0], 0.0);
    }

    #[test]
    fn test_pid_set_target() {
        let target1 = JointArray::from([Rad(1.0); 6]);
        let mut pid = PidController::new(target1);

        let target2 = JointArray::from([Rad(2.0); 6]);
        pid.set_target(target2);

        assert_eq!(pid.target()[0].0, 2.0);
    }

    #[test]
    fn test_pid_zero_dt() {
        let target = JointArray::from([Rad(1.0); 6]);
        let mut pid = PidController::new(target).with_gains(10.0, 1.0, 1.0);

        let current = JointArray::from([Rad(0.5); 6]);
        let dt = Duration::from_secs(0);

        // dt = 0 应该返回零输出
        let output = pid.tick(&current, dt).unwrap();
        assert_eq!(output[0].0, 0.0);
    }
}
