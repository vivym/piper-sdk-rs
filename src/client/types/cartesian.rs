//! 笛卡尔空间类型
//!
//! 提供3D位姿、速度和力的表示，用于笛卡尔空间控制。
//!
//! # 设计目标
//!
//! - **完整表示**: 位姿（位置+姿态）、速度、力
//! - **数值稳定**: 四元数归一化防止NaN传播
//! - **易用转换**: 欧拉角 ↔ 四元数
//!
//! # 示例
//!
//! ```rust
//! use piper_sdk::client::types::{CartesianPose, Quaternion, Rad};
//!
//! // 创建位姿
//! let pose = CartesianPose::from_position_euler(
//!     0.5, 0.0, 0.3,  // x, y, z (米)
//!     Rad(0.0), Rad(0.0), Rad(1.57),  // roll, pitch, yaw
//! );
//!
//! // 四元数转欧拉角
//! let (roll, pitch, yaw) = pose.orientation.to_euler();
//! ```

use super::units::Rad;
use std::fmt;

/// 四元数归一化阈值（避免除零）
///
/// 当四元数的模平方小于此值时，归一化会返回单位四元数。
const QUATERNION_NORM_THRESHOLD: f64 = 1e-10;

/// 三维位置向量（米）
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Position3D {
    /// X 坐标（米）
    pub x: f64,
    /// Y 坐标（米）
    pub y: f64,
    /// Z 坐标（米）
    pub z: f64,
}

impl Position3D {
    /// 创建新的三维位置
    pub const fn new(x: f64, y: f64, z: f64) -> Self {
        Position3D { x, y, z }
    }

    /// 零向量
    pub const ZERO: Self = Position3D::new(0.0, 0.0, 0.0);

    /// 计算向量长度（范数）
    pub fn norm(&self) -> f64 {
        (self.x * self.x + self.y * self.y + self.z * self.z).sqrt()
    }

    /// 归一化（单位向量）
    pub fn normalize(&self) -> Self {
        let n = self.norm();
        if n < 1e-10 {
            return Position3D::ZERO;
        }
        Position3D {
            x: self.x / n,
            y: self.y / n,
            z: self.z / n,
        }
    }

    /// 点积
    pub fn dot(&self, other: &Position3D) -> f64 {
        self.x * other.x + self.y * other.y + self.z * other.z
    }

    /// 叉积
    pub fn cross(&self, other: &Position3D) -> Position3D {
        Position3D {
            x: self.y * other.z - self.z * other.y,
            y: self.z * other.x - self.x * other.z,
            z: self.x * other.y - self.y * other.x,
        }
    }
}

impl fmt::Display for Position3D {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "({:.3}, {:.3}, {:.3})", self.x, self.y, self.z)
    }
}

/// 四元数（用于表示3D旋转）
///
/// 四元数是表示3D旋转的数学工具，避免了欧拉角的万向节锁问题。
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Quaternion {
    /// 实部
    pub w: f64,
    /// 虚部 i
    pub x: f64,
    /// 虚部 j
    pub y: f64,
    /// 虚部 k
    pub z: f64,
}

impl Quaternion {
    /// 单位四元数（无旋转）
    pub const IDENTITY: Self = Quaternion {
        w: 1.0,
        x: 0.0,
        y: 0.0,
        z: 0.0,
    };

    /// 从欧拉角创建四元数（Roll-Pitch-Yaw, ZYX顺序）
    ///
    /// # 参数
    ///
    /// - `roll`: 绕X轴旋转
    /// - `pitch`: 绕Y轴旋转
    /// - `yaw`: 绕Z轴旋转
    pub fn from_euler(roll: Rad, pitch: Rad, yaw: Rad) -> Self {
        let cr = (roll.0 / 2.0).cos();
        let sr = (roll.0 / 2.0).sin();
        let cp = (pitch.0 / 2.0).cos();
        let sp = (pitch.0 / 2.0).sin();
        let cy = (yaw.0 / 2.0).cos();
        let sy = (yaw.0 / 2.0).sin();

        Quaternion {
            w: cr * cp * cy + sr * sp * sy,
            x: sr * cp * cy - cr * sp * sy,
            y: cr * sp * cy + sr * cp * sy,
            z: cr * cp * sy - sr * sp * cy,
        }
    }

    /// 转换为欧拉角（Roll-Pitch-Yaw）
    ///
    /// 返回 `(roll, pitch, yaw)`
    pub fn to_euler(self) -> (Rad, Rad, Rad) {
        // Roll (x-axis rotation)
        let sinr_cosp = 2.0 * (self.w * self.x + self.y * self.z);
        let cosr_cosp = 1.0 - 2.0 * (self.x * self.x + self.y * self.y);
        let roll = Rad(sinr_cosp.atan2(cosr_cosp));

        // Pitch (y-axis rotation)
        let sinp = 2.0 * (self.w * self.y - self.z * self.x);
        let pitch = if sinp.abs() >= 1.0 {
            // Gimbal lock
            Rad(std::f64::consts::FRAC_PI_2.copysign(sinp))
        } else {
            Rad(sinp.asin())
        };

        // Yaw (z-axis rotation)
        let siny_cosp = 2.0 * (self.w * self.z + self.x * self.y);
        let cosy_cosp = 1.0 - 2.0 * (self.y * self.y + self.z * self.z);
        let yaw = Rad(siny_cosp.atan2(cosy_cosp));

        (roll, pitch, yaw)
    }

    /// 归一化（确保单位四元数）
    ///
    /// # 数值稳定性
    ///
    /// 如果四元数的模接近 0（< 1e-10），返回默认单位四元数 (1, 0, 0, 0)
    /// 以避免除零错误和 NaN 扩散。
    ///
    /// 这种情况理论上不应发生，但在初始化错误、序列化错误或数值计算
    /// 累积误差时可能出现。
    pub fn normalize(&self) -> Self {
        let norm_sq = self.w * self.w + self.x * self.x + self.y * self.y + self.z * self.z;

        // ✅ 数值稳定性检查：避免除零
        if norm_sq < QUATERNION_NORM_THRESHOLD {
            // 返回默认单位四元数（无旋转）
            tracing::warn!(
                "Normalizing near-zero quaternion (norm²={:.2e} < {:.2e}): Q({:.3}, {:.3}, {:.3}, {:.3}), returning identity",
                norm_sq,
                QUATERNION_NORM_THRESHOLD,
                self.w,
                self.x,
                self.y,
                self.z
            );
            return Quaternion::IDENTITY;
        }

        let norm = norm_sq.sqrt();
        Quaternion {
            w: self.w / norm,
            x: self.x / norm,
            y: self.y / norm,
            z: self.z / norm,
        }
    }

    /// 四元数乘法（组合旋转）
    pub fn multiply(&self, other: &Quaternion) -> Quaternion {
        Quaternion {
            w: self.w * other.w - self.x * other.x - self.y * other.y - self.z * other.z,
            x: self.w * other.x + self.x * other.w + self.y * other.z - self.z * other.y,
            y: self.w * other.y - self.x * other.z + self.y * other.w + self.z * other.x,
            z: self.w * other.z + self.x * other.y - self.y * other.x + self.z * other.w,
        }
    }

    /// 共轭（逆旋转）
    pub fn conjugate(&self) -> Quaternion {
        Quaternion {
            w: self.w,
            x: -self.x,
            y: -self.y,
            z: -self.z,
        }
    }
}

impl fmt::Display for Quaternion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Q({:.3}, {:.3}, {:.3}, {:.3})",
            self.w, self.x, self.y, self.z
        )
    }
}

/// 笛卡尔空间位姿（位置 + 姿态）
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct CartesianPose {
    /// 位置（米）
    pub position: Position3D,
    /// 姿态（四元数）
    pub orientation: Quaternion,
}

impl CartesianPose {
    /// 从位置和欧拉角创建
    pub fn from_position_euler(x: f64, y: f64, z: f64, roll: Rad, pitch: Rad, yaw: Rad) -> Self {
        CartesianPose {
            position: Position3D::new(x, y, z),
            orientation: Quaternion::from_euler(roll, pitch, yaw),
        }
    }

    /// 从位置和四元数创建
    pub fn from_position_quaternion(position: Position3D, orientation: Quaternion) -> Self {
        CartesianPose {
            position,
            orientation,
        }
    }

    /// 零位姿（原点，无旋转）
    pub const ZERO: Self = CartesianPose {
        position: Position3D::ZERO,
        orientation: Quaternion::IDENTITY,
    };
}

impl fmt::Display for CartesianPose {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Pose(pos: {}, quat: {})",
            self.position, self.orientation
        )
    }
}

/// 欧拉角（用于表示3D旋转姿态）
///
/// 使用 **Intrinsic RPY (Roll-Pitch-Yaw)** 顺序，即：
/// - 先绕 X 轴旋转（Roll）
/// - 再绕 Y 轴旋转（Pitch）
/// - 最后绕 Z 轴旋转（Yaw）
///
/// **协议映射**：
/// - Roll (RX): 对应协议 0x153 的 RX 角度
/// - Pitch (RY): 对应协议 0x154 的 RY 角度
/// - Yaw (RZ): 对应协议 0x154 的 RZ 角度
///
/// **单位**：度（degree）
#[derive(Debug, Clone, Copy, PartialEq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct EulerAngles {
    /// Roll：绕 X 轴旋转（度）
    pub roll: f64,
    /// Pitch：绕 Y 轴旋转（度）
    pub pitch: f64,
    /// Yaw：绕 Z 轴旋转（度）
    pub yaw: f64,
}

impl EulerAngles {
    /// 创建新的欧拉角
    pub fn new(roll: f64, pitch: f64, yaw: f64) -> Self {
        EulerAngles { roll, pitch, yaw }
    }

    /// 零角度（无旋转）
    pub const ZERO: Self = EulerAngles {
        roll: 0.0,
        pitch: 0.0,
        yaw: 0.0,
    };
}

impl fmt::Display for EulerAngles {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Euler(roll: {:.2}°, pitch: {:.2}°, yaw: {:.2}°)",
            self.roll, self.pitch, self.yaw
        )
    }
}

/// 笛卡尔空间速度（线速度 + 角速度）
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct CartesianVelocity {
    /// 线速度（米/秒）
    pub linear: Position3D,
    /// 角速度（弧度/秒）
    pub angular: Position3D,
}

impl CartesianVelocity {
    /// 创建新的笛卡尔速度
    pub fn new(linear: Position3D, angular: Position3D) -> Self {
        CartesianVelocity { linear, angular }
    }

    /// 零速度
    pub const ZERO: Self = CartesianVelocity {
        linear: Position3D::ZERO,
        angular: Position3D::ZERO,
    };
}

/// 笛卡尔空间力/力矩
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct CartesianEffort {
    /// 力（牛顿）
    pub force: Position3D,
    /// 力矩（牛顿·米）
    pub torque: Position3D,
}

impl CartesianEffort {
    /// 创建新的笛卡尔力/力矩
    pub fn new(force: Position3D, torque: Position3D) -> Self {
        CartesianEffort { force, torque }
    }

    /// 零力/力矩
    pub const ZERO: Self = CartesianEffort {
        force: Position3D::ZERO,
        torque: Position3D::ZERO,
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_position3d_basic() {
        let pos = Position3D::new(1.0, 2.0, 3.0);
        assert_eq!(pos.x, 1.0);
        assert_eq!(pos.y, 2.0);
        assert_eq!(pos.z, 3.0);
    }

    #[test]
    fn test_position3d_norm() {
        let pos = Position3D::new(3.0, 4.0, 0.0);
        assert!((pos.norm() - 5.0).abs() < 1e-10);
    }

    #[test]
    fn test_position3d_normalize() {
        let pos = Position3D::new(3.0, 4.0, 0.0);
        let normalized = pos.normalize();
        assert!((normalized.norm() - 1.0).abs() < 1e-10);
    }

    #[test]
    fn test_position3d_dot() {
        let a = Position3D::new(1.0, 2.0, 3.0);
        let b = Position3D::new(4.0, 5.0, 6.0);
        assert_eq!(a.dot(&b), 32.0); // 1*4 + 2*5 + 3*6
    }

    #[test]
    fn test_position3d_cross() {
        let a = Position3D::new(1.0, 0.0, 0.0);
        let b = Position3D::new(0.0, 1.0, 0.0);
        let c = a.cross(&b);
        assert_eq!(c.x, 0.0);
        assert_eq!(c.y, 0.0);
        assert_eq!(c.z, 1.0);
    }

    #[test]
    fn test_quaternion_identity() {
        let quat = Quaternion::IDENTITY;
        assert_eq!(quat.w, 1.0);
        assert_eq!(quat.x, 0.0);
    }

    #[test]
    fn test_quaternion_euler_conversion() {
        let roll = Rad(0.1);
        let pitch = Rad(0.2);
        let yaw = Rad(0.3);

        let quat = Quaternion::from_euler(roll, pitch, yaw);
        let (r2, p2, y2) = quat.to_euler();

        assert!((roll.0 - r2.0).abs() < 1e-10);
        assert!((pitch.0 - p2.0).abs() < 1e-10);
        assert!((yaw.0 - y2.0).abs() < 1e-10);
    }

    #[test]
    fn test_euler_angles_new() {
        let euler = EulerAngles::new(10.0, 20.0, 30.0);
        assert_eq!(euler.roll, 10.0);
        assert_eq!(euler.pitch, 20.0);
        assert_eq!(euler.yaw, 30.0);
    }

    #[test]
    fn test_euler_angles_zero() {
        let euler = EulerAngles::ZERO;
        assert_eq!(euler.roll, 0.0);
        assert_eq!(euler.pitch, 0.0);
        assert_eq!(euler.yaw, 0.0);
    }

    #[test]
    fn test_euler_angles_default() {
        let euler = EulerAngles::default();
        assert_eq!(euler.roll, 0.0);
        assert_eq!(euler.pitch, 0.0);
        assert_eq!(euler.yaw, 0.0);
    }

    #[test]
    fn test_quaternion_normalization() {
        let quat = Quaternion {
            w: 1.0,
            x: 1.0,
            y: 1.0,
            z: 1.0,
        };
        let normalized = quat.normalize();

        let norm = (normalized.w * normalized.w
            + normalized.x * normalized.x
            + normalized.y * normalized.y
            + normalized.z * normalized.z)
            .sqrt();

        assert!((norm - 1.0).abs() < 1e-10);
    }

    #[test]
    fn test_quaternion_near_zero_stability() {
        // 测试近零四元数的数值稳定性
        let near_zero = Quaternion {
            w: 1e-20,
            x: 1e-20,
            y: 1e-20,
            z: 1e-20,
        };
        let normalized = near_zero.normalize();

        // 应该返回单位四元数（无旋转）
        assert_eq!(normalized.w, 1.0);
        assert_eq!(normalized.x, 0.0);
        assert_eq!(normalized.y, 0.0);
        assert_eq!(normalized.z, 0.0);

        // 测试完全为零的情况
        let zero = Quaternion {
            w: 0.0,
            x: 0.0,
            y: 0.0,
            z: 0.0,
        };
        let normalized_zero = zero.normalize();

        // 不应该是 NaN
        assert!(!normalized_zero.w.is_nan());
        assert!(!normalized_zero.x.is_nan());
        assert_eq!(normalized_zero.w, 1.0);
    }

    #[test]
    fn test_quaternion_norm_threshold() {
        // 验证阈值常量的正确使用
        assert_eq!(QUATERNION_NORM_THRESHOLD, 1e-10);
    }

    #[test]
    fn test_quaternion_multiply() {
        let q1 = Quaternion::from_euler(Rad(0.1), Rad(0.0), Rad(0.0));
        let q2 = Quaternion::from_euler(Rad(0.2), Rad(0.0), Rad(0.0));
        let q3 = q1.multiply(&q2);
        let q_expected = Quaternion::from_euler(Rad(0.3), Rad(0.0), Rad(0.0));

        assert!((q3.w - q_expected.w).abs() < 1e-10);
        assert!((q3.x - q_expected.x).abs() < 1e-10);
    }

    #[test]
    fn test_quaternion_conjugate() {
        let quat = Quaternion {
            w: 0.7,
            x: 0.1,
            y: 0.2,
            z: 0.3,
        };
        let conj = quat.conjugate();

        assert_eq!(conj.w, 0.7);
        assert_eq!(conj.x, -0.1);
        assert_eq!(conj.y, -0.2);
        assert_eq!(conj.z, -0.3);
    }

    #[test]
    fn test_cartesian_pose_construction() {
        let pose = CartesianPose::from_position_euler(1.0, 2.0, 3.0, Rad(0.0), Rad(0.0), Rad(0.0));

        assert_eq!(pose.position.x, 1.0);
        assert_eq!(pose.position.y, 2.0);
        assert_eq!(pose.position.z, 3.0);
    }

    #[test]
    fn test_cartesian_velocity() {
        let vel = CartesianVelocity::new(
            Position3D::new(0.1, 0.2, 0.3),
            Position3D::new(0.01, 0.02, 0.03),
        );

        assert_eq!(vel.linear.x, 0.1);
        assert_eq!(vel.angular.z, 0.03);
    }

    #[test]
    fn test_cartesian_effort() {
        let effort = CartesianEffort::new(
            Position3D::new(10.0, 20.0, 30.0),
            Position3D::new(1.0, 2.0, 3.0),
        );

        assert_eq!(effort.force.x, 10.0);
        assert_eq!(effort.torque.z, 3.0);
    }

    #[test]
    fn test_zero_constants() {
        assert_eq!(Position3D::ZERO.x, 0.0);
        assert_eq!(CartesianPose::ZERO.position.x, 0.0);
        assert_eq!(CartesianVelocity::ZERO.linear.x, 0.0);
        assert_eq!(CartesianEffort::ZERO.force.x, 0.0);
    }
}
