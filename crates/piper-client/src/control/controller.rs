//! Controller trait - 控制器通用接口
//!
//! 定义了所有控制器必须实现的接口。
//!
//! # 设计理念
//!
//! - **Tick 模式**: 用户控制循环，控制器只负责计算
//! - **时间感知**: 显式传入 `dt`，便于单元测试
//! - **完整反馈**: `ControlSnapshot` 提供位置、速度、力矩和对齐时间戳
//! - **类型安全**: 使用强类型单位（`Rad`, `NewtonMeter`）
//! - **错误处理**: 关联类型 `Error` 允许自定义错误
//!
//! # 时间跳变处理
//!
//! 当检测到异常的 `dt` 时（如系统卡顿、线程调度延迟），
//! `on_time_jump()` 方法会被调用。
//!
//! ⚠️ **重要**: 对于维护内部微分/滤波状态的控制器，**强烈建议**覆盖此方法：
//!
//! - ✅ **可以重置**: 微分项或滤波器内部状态，防止异常 `dt` 污染状态
//! - ❌ **不要清零**: 积分项（I term），否则机械臂会瞬间失去抗重力能力
//!
//! # 示例
//!
//! ```rust,no_run
//! use piper_client::control::Controller;
//! use piper_client::ControlSnapshot;
//! use piper_client::types::{JointArray, Rad, NewtonMeter};
//! use std::time::Duration;
//!
//! struct MyController {
//!     target: JointArray<Rad>,
//!     last_error: JointArray<f64>,
//! }
//!
//! impl Controller for MyController {
//!     type Error = std::io::Error;
//!
//!     fn tick(
//!         &mut self,
//!         snapshot: &ControlSnapshot,
//!         dt: Duration,
//!     ) -> Result<JointArray<NewtonMeter>, Self::Error> {
//!         let error = self.target.map_with(snapshot.position, |t, c| t - c);
//!         let damping = snapshot.velocity.map(|v| -0.5 * v.0);
//!         let output = error
//!             .map(|e| e.0 * 10.0)
//!             .map_with(damping, |p, d| NewtonMeter((p + d).clamp(-50.0, 50.0)));
//!
//!         let _measured_torque = snapshot.torque;
//!         let _timing = (
//!             snapshot.position_timestamp_us,
//!             snapshot.dynamic_timestamp_us,
//!             snapshot.skew_us,
//!         );
//!         let _ = dt;
//!
//!         self.last_error = error.map(|e| e.0);
//!         Ok(output)
//!     }
//!
//!     fn on_time_jump(&mut self, _dt: Duration) -> Result<(), Self::Error> {
//!         // ✅ 重置微分项相关状态
//!         self.last_error = JointArray::from([0.0; 6]);
//!         // ❌ 不要清零积分项！
//!         Ok(())
//!     }
//! }
//! ```

use crate::observer::ControlSnapshot;
use crate::types::{JointArray, NewtonMeter};
use std::time::Duration;

/// 控制器通用接口
///
/// 所有控制器都必须实现此 trait。
///
/// # 生命周期
///
/// - **初始化**: 在控制器构造时设置目标、参数
/// - **运行**: 循环调用 `tick()`，传入对齐后的控制快照和时间步长
/// - **异常**: 当 `dt` 异常时，调用 `on_time_jump()`
/// - **清理**: 控制器 `Drop` 时自动清理
///
/// # 线程安全
///
/// `Controller` 本身不要求 `Send` 或 `Sync`。
/// 如果需要在多线程中使用，请将其包装在 `Mutex` 中。
pub trait Controller {
    /// 控制器错误类型
    ///
    /// 允许每个控制器定义自己的错误类型。
    type Error: std::error::Error + Send + 'static;

    /// 计算一步控制输出
    ///
    /// # 参数
    ///
    /// - `snapshot`: 对齐后的控制快照
    ///   - `snapshot.position`: 当前关节位置
    ///   - `snapshot.velocity`: 当前关节速度
    ///   - `snapshot.torque`: 当前实测关节力矩
    ///   - `snapshot.position_timestamp_us` / `snapshot.dynamic_timestamp_us` / `snapshot.skew_us`:
    ///     高级控制和诊断用的对齐时序元数据
    /// - `dt`: 时间步长（自上次 `tick` 以来的时间）
    ///
    /// # 返回
    ///
    /// - `Ok(output)`: 关节力矩命令
    /// - `Err(e)`: 控制器内部错误
    ///
    /// # 注意
    ///
    /// - `dt` 可能会被钳位（clamped），不一定等于实际时间
    /// - 如果 `dt` 被钳位，`on_time_jump()` 会先被调用
    /// - 输出力矩应该被钳位到安全范围内
    ///
    /// # 示例
    ///
    /// ```rust,no_run
    /// # use piper_client::control::Controller;
    /// # use piper_client::ControlSnapshot;
    /// # use piper_client::types::{JointArray, Rad, NewtonMeter};
    /// # use std::time::Duration;
    /// # struct MyController {
    /// #     target: JointArray<Rad>,
    /// # }
    /// # impl Controller for MyController {
    /// #     type Error = std::io::Error;
    /// fn tick(
    ///     &mut self,
    ///     snapshot: &ControlSnapshot,
    ///     dt: Duration,
    /// ) -> Result<JointArray<NewtonMeter>, Self::Error> {
    ///     // 1. 计算误差
    ///     let error = self.target.map_with(snapshot.position, |t, c| (t - c).0);
    ///
    ///     // 2. 使用实测速度/力矩更新内部状态
    ///     let damping = snapshot.velocity.map(|v| -0.5 * v.0);
    ///     let _measured_torque = snapshot.torque;
    ///
    ///     // 3. 计算输出
    ///     let output = error
    ///         .map(|e| e * 10.0)
    ///         .map_with(damping, |p, d| NewtonMeter(p + d));
    ///
    ///     // 4. 钳位输出到安全范围
    ///     let _ = dt;
    ///     Ok(output.map(|t| t.clamp(NewtonMeter(-50.0), NewtonMeter(50.0))))
    /// }
    /// #     fn on_time_jump(&mut self, _dt: Duration) -> Result<(), Self::Error> { Ok(()) }
    /// # }
    /// ```
    fn tick(
        &mut self,
        snapshot: &ControlSnapshot,
        dt: Duration,
    ) -> Result<JointArray<NewtonMeter>, Self::Error>;

    /// 处理时间跳变
    ///
    /// 当检测到 `dt` 超过预期（通常是由于系统卡顿、线程调度延迟等），
    /// 此方法会被调用，允许控制器重置或调整内部状态。
    ///
    /// # 参数
    ///
    /// - `dt`: 实际经过的时间（未钳位前）
    ///
    /// # 默认实现
    ///
    /// 默认实现不做任何事情（`Ok(())`）。
    ///
    /// # ⚠️ 重要提示
    ///
    /// 对于维护内部微分/滤波状态的控制器，**强烈建议** 覆盖此方法：
    ///
    /// ## ✅ 应该重置的状态
    ///
    /// - **微分/滤波状态**: `last_error`、滤波器缓存等
    ///   - 原因：异常 `dt` 可能污染内部动态状态
    ///
    /// ## ❌ 不应该清零的状态
    ///
    /// - **积分项（I term）**: 累积误差
    ///   - 原因：机械臂可能依赖积分项对抗重力
    ///   - 后果：清零会导致机械臂瞬间下坠（Sagging）
    ///
    /// # 示例
    ///
    /// ```rust,no_run
    /// # use piper_client::control::Controller;
    /// # use piper_client::ControlSnapshot;
    /// # use piper_client::types::{JointArray, Rad, NewtonMeter};
    /// # use std::time::Duration;
    /// struct FilteredController {
    ///     integral: JointArray<f64>,  // 积分项
    ///     derivative_state: JointArray<f64>, // 内部滤波/微分状态
    /// }
    ///
    /// impl Controller for FilteredController {
    ///     type Error = std::io::Error;
    ///
    ///     fn tick(&mut self, snapshot: &ControlSnapshot, dt: Duration)
    ///         -> Result<JointArray<NewtonMeter>, Self::Error> {
    ///         // ... 实现 ...
    ///         let _ = (snapshot.position, dt);
    ///         Ok(JointArray::from([NewtonMeter(0.0); 6]))
    ///     }
    ///
    ///     fn on_time_jump(&mut self, _dt: Duration) -> Result<(), Self::Error> {
    ///         // ✅ 重置微分/滤波状态
    ///         self.derivative_state = JointArray::from([0.0; 6]);
    ///
    ///         // ❌ 不要清零积分项！
    ///         // self.integral = JointArray::from([0.0; 6]); // 会导致机械臂下坠
    ///
    ///         Ok(())
    ///     }
    /// }
    /// ```
    ///
    /// # 何时调用
    ///
    /// 通常在 `run_controller()` 中，当检测到 `dt` 超过阈值时：
    ///
    /// ```rust,ignore
    /// if real_dt > max_dt {
    ///     controller.on_time_jump(real_dt)?;
    ///     dt = max_dt; // 钳位后传入 tick()
    /// }
    /// ```
    fn on_time_jump(&mut self, _dt: Duration) -> Result<(), Self::Error> {
        // 默认：什么都不做
        // 时间敏感的控制器（如 PID）应该覆盖此方法
        Ok(())
    }

    /// 重置控制器到初始状态（可选）
    ///
    /// 用于在不销毁控制器的情况下重新开始。
    ///
    /// # 默认实现
    ///
    /// 默认不实现（返回错误），控制器可以选择性实现。
    ///
    /// # 示例
    ///
    /// ```rust,no_run
    /// # use piper_client::control::Controller;
    /// # use piper_client::ControlSnapshot;
    /// # use piper_client::types::{JointArray, NewtonMeter};
    /// # use std::time::Duration;
    /// # struct MyController { integral: JointArray<f64>, derivative_state: JointArray<f64> }
    /// # impl Controller for MyController {
    /// #     type Error = std::io::Error;
    /// #     fn tick(&mut self, _: &ControlSnapshot, _: Duration) -> Result<JointArray<NewtonMeter>, Self::Error> { Ok(JointArray::from([NewtonMeter(0.0); 6])) }
    /// fn reset(&mut self) -> Result<(), Self::Error> {
    ///     // 重置所有内部状态
    ///     self.integral = JointArray::from([0.0; 6]);
    ///     self.derivative_state = JointArray::from([0.0; 6]);
    ///     Ok(())
    /// }
    /// # }
    /// ```
    fn reset(&mut self) -> Result<(), Self::Error> {
        // 默认不支持重置
        // 控制器可以选择性实现此方法
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Rad, RadPerSecond};

    fn test_snapshot(position: JointArray<Rad>) -> ControlSnapshot {
        ControlSnapshot {
            position,
            velocity: JointArray::splat(RadPerSecond(0.0)),
            torque: JointArray::splat(NewtonMeter(0.0)),
            position_timestamp_us: 1_000,
            dynamic_timestamp_us: 1_000,
            skew_us: 0,
        }
    }

    /// 简单的测试控制器（比例控制）
    struct TestController {
        target: JointArray<Rad>,
        kp: f64,
    }

    impl Controller for TestController {
        type Error = std::io::Error;

        fn tick(
            &mut self,
            snapshot: &ControlSnapshot,
            _dt: Duration,
        ) -> Result<JointArray<NewtonMeter>, Self::Error> {
            let error = self.target.map_with(snapshot.position, |t, c| t - c);
            let output = error.map(|e| NewtonMeter(self.kp * e.0));
            Ok(output)
        }
    }

    #[test]
    fn test_controller_trait_basic() {
        let target = JointArray::from([Rad(1.0); 6]);
        let mut controller = TestController { target, kp: 10.0 };

        let snapshot = test_snapshot(JointArray::from([Rad(0.5); 6]));
        let dt = Duration::from_millis(10);

        let output = controller.tick(&snapshot, dt).unwrap();

        // 误差 = 1.0 - 0.5 = 0.5
        // 输出 = 10.0 * 0.5 = 5.0
        assert!((output[0].0 - 5.0).abs() < 1e-10);
    }

    #[test]
    fn test_on_time_jump_default() {
        let target = JointArray::from([Rad(1.0); 6]);
        let mut controller = TestController { target, kp: 10.0 };

        // 默认实现应该什么都不做，不报错
        let result = controller.on_time_jump(Duration::from_secs(1));
        assert!(result.is_ok());
    }

    #[test]
    fn test_reset_default() {
        let target = JointArray::from([Rad(1.0); 6]);
        let mut controller = TestController { target, kp: 10.0 };

        // 默认实现应该什么都不做，不报错
        let result = controller.reset();
        assert!(result.is_ok());
    }
}
