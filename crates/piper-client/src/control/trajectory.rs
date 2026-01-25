//! Trajectory Planner - 轨迹规划器
//!
//! 使用三次样条插值生成平滑的关节空间轨迹。
//!
//! # 算法
//!
//! 使用三次多项式插值：
//! ```text
//! p(t) = a0 + a1*t + a2*t² + a3*t³
//! v(t) = a1 + 2*a2*t + 3*a3*t²
//! ```
//!
//! 边界条件：起止速度为 0
//!
//! # 特性
//!
//! - **Iterator 模式**: 按需生成轨迹点，内存高效
//! - **平滑性保证**: C² 连续（加速度连续）
//! - **强类型**: 使用 `Rad` 确保单位正确
//!
//! # 示例
//!
//! ```rust,no_run
//! use piper_client::control::TrajectoryPlanner;
//! use piper_client::types::{JointArray, Rad};
//! use std::time::Duration;
//!
//! let start = JointArray::from([Rad(0.0); 6]);
//! let end = JointArray::from([Rad(1.0); 6]);
//! let duration = Duration::from_secs(5);
//! let frequency_hz = 100.0;  // 100Hz
//!
//! let mut planner = TrajectoryPlanner::new(start, end, duration, frequency_hz);
//!
//! for (position, velocity) in &mut planner {
//!     // 使用 position 和 velocity
//!     println!("pos: {:?}, vel: {:?}", position, velocity);
//! }
//! ```

use crate::types::{JointArray, Rad};
use std::time::Duration;

/// 三次样条系数
///
/// 表示 `p(t) = a0 + a1*t + a2*t² + a3*t³`
#[derive(Debug, Clone, Copy)]
struct CubicCoeffs {
    a0: f64,
    a1: f64,
    a2: f64,
    a3: f64,
}

impl CubicCoeffs {
    /// 在归一化时间 t ∈ [0, 1] 处计算位置
    fn position(&self, t: f64) -> f64 {
        self.a0 + self.a1 * t + self.a2 * t * t + self.a3 * t * t * t
    }

    /// 在归一化时间 t ∈ [0, 1] 处计算速度
    ///
    /// 注意：这是对归一化时间的导数，需要除以实际时间长度
    fn velocity(&self, t: f64) -> f64 {
        self.a1 + 2.0 * self.a2 * t + 3.0 * self.a3 * t * t
    }
}

/// 轨迹规划器
///
/// 生成从起点到终点的平滑轨迹。
pub struct TrajectoryPlanner {
    /// 每个关节的样条系数
    spline_coeffs: JointArray<CubicCoeffs>,

    /// 轨迹总时长
    duration: Duration,

    /// 当前迭代索引
    current_index: usize,

    /// 总采样点数
    total_samples: usize,
}

impl TrajectoryPlanner {
    /// 创建新的轨迹规划器
    ///
    /// # 参数
    ///
    /// - `start`: 起始位置
    /// - `end`: 终止位置
    /// - `duration`: 轨迹时长
    /// - `frequency_hz`: 采样频率（Hz）
    ///
    /// # 边界条件
    ///
    /// - 起始速度: 0
    /// - 终止速度: 0
    ///
    /// # 错误
    ///
    /// 如果 `frequency_hz` 不是正数，将 panic。
    ///
    /// # 示例
    ///
    /// ```rust
    /// # use piper_client::control::TrajectoryPlanner;
    /// # use piper_client::types::{JointArray, Rad};
    /// # use std::time::Duration;
    /// let start = JointArray::from([Rad(0.0); 6]);
    /// let end = JointArray::from([Rad(1.57); 6]);
    /// let planner = TrajectoryPlanner::new(
    ///     start,
    ///     end,
    ///     Duration::from_secs(3),
    ///     100.0,
    /// );
    /// ```
    pub fn new(
        start: JointArray<Rad>,
        end: JointArray<Rad>,
        duration: Duration,
        frequency_hz: f64,
    ) -> Self {
        // ✅ 输入验证
        assert!(
            frequency_hz > 0.0,
            "frequency_hz must be positive, got: {}",
            frequency_hz
        );

        let duration_sec = duration.as_secs_f64();

        // ⚠️ 重要：未来支持 Via Points（途径点）时，
        // 需要将物理速度乘以 duration_sec 进行时间缩放
        // 例如: v_start_normalized = v_start_physical * duration_sec
        let v_start = 0.0; // 当前：起始速度为 0
        let v_end = 0.0; // 当前：终止速度为 0

        // 为每个关节计算样条系数
        let spline_coeffs = start.map_with(end, |s, e| {
            Self::compute_cubic_spline(s.0, v_start, e.0, v_end)
        });

        let total_samples = (duration_sec * frequency_hz).ceil() as usize;

        TrajectoryPlanner {
            spline_coeffs,
            duration,
            current_index: 0,
            total_samples,
        }
    }

    /// 计算三次样条系数
    ///
    /// 给定边界条件 `p(0) = p0`, `v(0) = v0`, `p(1) = p1`, `v(1) = v1`，
    /// 计算 `p(t) = a0 + a1*t + a2*t² + a3*t³` 的系数。
    ///
    /// # 参数
    ///
    /// - `p0`: 起始位置
    /// - `v0`: 起始速度（归一化时间）
    /// - `p1`: 终止位置
    /// - `v1`: 终止速度（归一化时间）
    ///
    /// # 返回
    ///
    /// 三次样条系数
    fn compute_cubic_spline(p0: f64, v0: f64, p1: f64, v1: f64) -> CubicCoeffs {
        // 边界条件：
        // p(0) = a0 = p0
        // v(0) = a1 = v0
        // p(1) = a0 + a1 + a2 + a3 = p1
        // v(1) = a1 + 2*a2 + 3*a3 = v1

        let a0 = p0;
        let a1 = v0;

        // 解线性方程组：
        // a2 + a3 = p1 - p0 - v0
        // 2*a2 + 3*a3 = v1 - v0
        // =>
        // a3 = -2*p1 + 2*p0 + v0 + v1
        // a2 = 3*p1 - 3*p0 - 2*v0 - v1

        let a2 = 3.0 * (p1 - p0) - 2.0 * v0 - v1;
        let a3 = -2.0 * (p1 - p0) + v0 + v1;

        CubicCoeffs { a0, a1, a2, a3 }
    }

    /// 在指定时间计算位置和速度
    ///
    /// # 参数
    ///
    /// - `t`: 归一化时间 [0, 1]
    ///
    /// # 返回
    ///
    /// `(position, velocity)` 元组
    fn evaluate_at(&self, t: f64) -> (JointArray<Rad>, JointArray<f64>) {
        let duration_sec = self.duration.as_secs_f64();

        let position = self.spline_coeffs.map(|coeff| Rad(coeff.position(t)));

        // 速度：需要除以时间长度（从归一化时间导数转换为物理速度）
        let velocity = self.spline_coeffs.map(|coeff| coeff.velocity(t) / duration_sec);

        (position, velocity)
    }

    /// 重置迭代器到起点
    pub fn reset(&mut self) {
        self.current_index = 0;
    }

    /// 获取总采样点数
    pub fn total_samples(&self) -> usize {
        self.total_samples
    }

    /// 获取当前进度（0.0 到 1.0）
    pub fn progress(&self) -> f64 {
        if self.total_samples == 0 {
            1.0
        } else {
            (self.current_index as f64) / (self.total_samples as f64)
        }
    }
}

impl Iterator for TrajectoryPlanner {
    type Item = (JointArray<Rad>, JointArray<f64>);

    fn next(&mut self) -> Option<Self::Item> {
        if self.current_index >= self.total_samples {
            return None;
        }

        // 计算归一化时间 t ∈ [0, 1]
        let t = if self.total_samples <= 1 {
            1.0
        } else {
            (self.current_index as f64) / ((self.total_samples - 1) as f64)
        };

        let result = self.evaluate_at(t);
        self.current_index += 1;

        Some(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cubic_coeffs_position() {
        let coeffs = CubicCoeffs {
            a0: 0.0,
            a1: 0.0,
            a2: 3.0,
            a3: -2.0,
        };

        // t=0: p = 0
        assert!((coeffs.position(0.0) - 0.0).abs() < 1e-10);

        // t=1: p = 0 + 0 + 3 - 2 = 1
        assert!((coeffs.position(1.0) - 1.0).abs() < 1e-10);
    }

    #[test]
    fn test_cubic_coeffs_velocity() {
        let coeffs = CubicCoeffs {
            a0: 0.0,
            a1: 0.0,
            a2: 3.0,
            a3: -2.0,
        };

        // v(t) = 0 + 6*t - 6*t²
        // t=0: v = 0
        assert!((coeffs.velocity(0.0) - 0.0).abs() < 1e-10);

        // t=1: v = 6 - 6 = 0
        assert!((coeffs.velocity(1.0) - 0.0).abs() < 1e-10);
    }

    #[test]
    fn test_compute_cubic_spline_zero_velocity() {
        let coeffs = TrajectoryPlanner::compute_cubic_spline(0.0, 0.0, 1.0, 0.0);

        // 边界条件检查
        assert!((coeffs.position(0.0) - 0.0).abs() < 1e-10);
        assert!((coeffs.position(1.0) - 1.0).abs() < 1e-10);
        assert!((coeffs.velocity(0.0) - 0.0).abs() < 1e-10);
        assert!((coeffs.velocity(1.0) - 0.0).abs() < 1e-10);
    }

    #[test]
    fn test_trajectory_planner_new() {
        let start = JointArray::from([Rad(0.0); 6]);
        let end = JointArray::from([Rad(1.0); 6]);
        let duration = Duration::from_secs(1);
        let frequency_hz = 10.0;

        let planner = TrajectoryPlanner::new(start, end, duration, frequency_hz);

        assert_eq!(planner.total_samples, 10);
        assert_eq!(planner.current_index, 0);
    }

    #[test]
    fn test_trajectory_iterator_basic() {
        let start = JointArray::from([Rad(0.0); 6]);
        let end = JointArray::from([Rad(1.0); 6]);
        let duration = Duration::from_secs(1);
        let frequency_hz = 5.0; // 5 个采样点

        let planner = TrajectoryPlanner::new(start, end, duration, frequency_hz);

        let mut count = 0;
        for (pos, _vel) in planner {
            count += 1;
            // 位置应该在 [0, 1] 范围内
            assert!(pos[0].0 >= -0.1 && pos[0].0 <= 1.1);
        }

        assert_eq!(count, 5);
    }

    #[test]
    fn test_trajectory_boundary_conditions() {
        let start = JointArray::from([Rad(0.0); 6]);
        let end = JointArray::from([Rad(1.57); 6]);
        let duration = Duration::from_secs(2);
        let frequency_hz = 100.0;

        let mut planner = TrajectoryPlanner::new(start, end, duration, frequency_hz);

        // 第一个点
        let (first_pos, first_vel) = planner.next().unwrap();
        assert!((first_pos[0].0 - 0.0).abs() < 1e-6);
        assert!(first_vel[0].abs() < 1e-6); // 起始速度应该接近 0

        // 跳到最后一个点
        let mut last = None;
        for item in planner {
            last = Some(item);
        }

        let (last_pos, last_vel) = last.unwrap();
        assert!((last_pos[0].0 - 1.57).abs() < 1e-6);
        assert!(last_vel[0].abs() < 1e-6); // 终止速度应该接近 0
    }

    #[test]
    fn test_trajectory_reset() {
        let start = JointArray::from([Rad(0.0); 6]);
        let end = JointArray::from([Rad(1.0); 6]);
        let duration = Duration::from_secs(1);
        let frequency_hz = 10.0;

        let mut planner = TrajectoryPlanner::new(start, end, duration, frequency_hz);

        // 迭代几次
        planner.next();
        planner.next();
        assert_eq!(planner.current_index, 2);

        // 重置
        planner.reset();
        assert_eq!(planner.current_index, 0);

        // 应该可以重新迭代
        let (pos, _vel) = planner.next().unwrap();
        assert!((pos[0].0 - 0.0).abs() < 1e-6);
    }

    #[test]
    fn test_trajectory_progress() {
        let start = JointArray::from([Rad(0.0); 6]);
        let end = JointArray::from([Rad(1.0); 6]);
        let duration = Duration::from_secs(1);
        let frequency_hz = 10.0;

        let mut planner = TrajectoryPlanner::new(start, end, duration, frequency_hz);

        assert!((planner.progress() - 0.0).abs() < 1e-10);

        planner.next();
        assert!(planner.progress() > 0.0 && planner.progress() < 1.0);

        // 迭代到最后
        while planner.next().is_some() {}
        assert!((planner.progress() - 1.0).abs() < 1e-10);
    }

    #[test]
    fn test_trajectory_smoothness() {
        let start = JointArray::from([Rad(0.0); 6]);
        let end = JointArray::from([Rad(1.0); 6]);
        let duration = Duration::from_secs(1);
        let frequency_hz = 1000.0; // 高频采样

        let planner = TrajectoryPlanner::new(start, end, duration, frequency_hz);

        let mut last_vel: Option<f64> = None;
        let mut max_accel: f64 = 0.0;
        let dt: f64 = 1.0 / frequency_hz;

        for (_pos, vel) in planner {
            if let Some(lv) = last_vel {
                let accel: f64 = (vel[0] - lv) / dt;
                max_accel = max_accel.max(accel.abs());
            }
            last_vel = Some(vel[0]);
        }

        // 加速度应该是有界的（对于这个简单的轨迹）
        // 由于数值微分可能引入噪声，我们使用一个较宽松的阈值
        assert!(max_accel < 100.0, "Max accel: {}", max_accel);
    }

    #[test]
    fn test_trajectory_single_point() {
        let start = JointArray::from([Rad(0.0); 6]);
        let end = JointArray::from([Rad(0.0); 6]);
        let duration = Duration::from_millis(10);
        let frequency_hz = 100.0;

        let mut planner = TrajectoryPlanner::new(start, end, duration, frequency_hz);

        // 即使起点和终点相同，也应该生成轨迹
        let mut count = 0;
        while planner.next().is_some() {
            count += 1;
        }

        assert!(count > 0);
    }
}
