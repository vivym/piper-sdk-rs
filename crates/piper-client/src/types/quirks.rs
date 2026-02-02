//! 固件特性（Device Quirks）
//!
//! 在连接时确定的固件特性，用于处理不同固件版本的兼容性问题。
//! 所有 quirks 在连接时确定并在热路径中以零成本访问。
//!
//! # 设计原则
//!
//! - **静态分发**：quirks 在连接时确定，热路径中使用编译器内联
//! - **零运行时开销**：避免在 200Hz+ 控制回路中进行版本检查
//! - **类型安全**：编译时保证所有 quirks 已处理
//!
//! # 示例
//!
//! ```rust
//! use piper_client::types::DeviceQuirks;
//! use piper_client::types::Joint;
//! use semver::Version;
//!
//! // 从固件版本号生成 quirks（连接时调用一次）
//! let quirks = DeviceQuirks::from_firmware_version(Version::new(1, 7, 2));
//!
//! // 热路径中使用（编译器完全内联）
//! let (position, torque) = quirks.apply_flip(Joint::J1, 0.5, 1.0);
//! let scaled_torque = quirks.scale_torque(Joint::J1, 1.0);
//! ```

use crate::types::Joint;
use semver::Version;

/// 固件特性（在连接时确定，之后只读）
///
/// 包含不同固件版本特有的行为差异，如关节 flip 映射和力矩缩放因子。
/// 这些 quirks 在连接时根据固件版本号确定，并在热路径中以零成本访问。
///
/// # 字段说明
///
/// - `firmware_version`: 固件版本号
/// - `joint_flip_map`: 关节 flip 标志（v1.7-3 之前有 bug）
/// - `torque_scaling`: 力矩缩放因子（旧固件 J1-3 力矩被放大 4x）
///
/// # 性能
///
/// 所有方法都是 `#[inline]`，编译器会完全内联到调用点，
/// 热路径开销接近零（~2ns vs Python 的 ~200ns）。
#[derive(Debug, Clone)]
pub struct DeviceQuirks {
    /// 固件版本号
    pub firmware_version: Version,

    /// 关节 flip 映射（v1.7-3 之前的 bug 修复）
    ///
    /// v1.7-3 之前：某些关节的命令位置和前馈力矩需要取反
    /// v1.7-3 及之后：所有关节不需要 flip
    pub joint_flip_map: [bool; 6],

    /// 力矩缩放因子（v1.8-2 及之前的缩放问题）
    ///
    /// v1.8-2 及之前：J1-3 的命令力矩被固件执行为 4x
    /// v1.8-2 之后：所有关节无缩放（因子 = 1.0）
    pub torque_scaling: [f64; 6],
}

impl DeviceQuirks {
    /// 从固件版本号生成 quirks（连接时调用一次）
    ///
    /// # 参数
    ///
    /// * `version` - 固件版本号（例如 "1.7.2" 或 "1.8.0"）
    ///
    /// # 返回
    ///
    /// 包含版本特定 quirks 的 `DeviceQuirks` 结构体
    ///
    /// # 示例
    ///
    /// ```rust
    /// use semver::Version;
    /// use piper_client::types::DeviceQuirks;
    ///
    /// let quirks = DeviceQuirks::from_firmware_version(Version::new(1, 7, 2));
    /// assert!(quirks.joint_flip_map[0]); // v1.7-2 之前，J1 需要 flip
    /// ```
    pub fn from_firmware_version(version: Version) -> Self {
        // 处理 v1.7-3 前后的 joint flip bug
        let joint_flip_map = if version < Version::new(1, 7, 3) {
            // v1.7-3 之前的 bug
            [true, true, false, true, false, true]
        } else {
            // v1.7-3 及之后已修复
            [false, false, false, false, false, false]
        };

        // 处理 v1.8-2 及之前的力矩缩放问题
        let torque_scaling = if version <= Version::new(1, 8, 2) {
            // J1-3: 命令力矩被执行为 4x，所以需要除以 4
            [0.25, 0.25, 0.25, 1.0, 1.0, 1.0]
        } else {
            // 所有关节无缩放
            [1.0; 6]
        };

        Self {
            firmware_version: version,
            joint_flip_map,
            torque_scaling,
        }
    }

    /// 应用 joint flip（热路径，内联）
    ///
    /// 根据固件版本特性，对指定的关节位置和前馈力矩应用 flip 取反操作。
    ///
    /// # 参数
    ///
    /// * `joint` - 关节索引
    /// * `position` - 位置（弧度）
    /// * `torque_ff` - 前馈力矩
    ///
    /// # 返回
    ///
    /// 可能取反后的 (position, torque_ff) 元组
    ///
    /// # 性能
    ///
    /// 此方法是 `#[inline]`，编译器会完全内联到调用点，
    /// 在热路径中开销接近零（~1-2 CPU 周期）。
    ///
    /// # 示例
    ///
    /// ```rust
    /// # use piper_client::types::{Joint, DeviceQuirks};
    /// # use semver::Version;
    /// #
    /// # let quirks = DeviceQuirks::from_firmware_version(Version::new(1, 7, 2));
    ///
    /// // v1.7-2 之前，J1 需要 flip
    /// let (pos, torque) = quirks.apply_flip(Joint::J1, 0.5, 1.0);
    /// assert_eq!(pos, -0.5);
    /// assert_eq!(torque, -1.0);
    /// ```
    #[inline]
    pub fn apply_flip(&self, joint: Joint, position: f64, torque_ff: f64) -> (f64, f64) {
        if self.joint_flip_map[joint as usize] {
            (-position, -torque_ff)
        } else {
            (position, torque_ff)
        }
    }

    /// 应用力矩缩放（热路径，内联）
    ///
    /// 根据固件版本特性，对指定关节的力矩应用缩放因子。
    ///
    /// # 参数
    ///
    /// * `joint` - 关节索引
    /// * `torque` - 原始力矩值
    ///
    /// # 返回
    ///
    /// 缩放后的力矩值
    ///
    /// # 性能
    ///
    /// 此方法是 `#[inline]`，编译器会完全内联到调用点，
    /// 在热路径中开销接近零（~1-2 CPU 周期）。
    ///
    /// # 示例
    ///
    /// ```rust
    /// # use piper_client::types::{Joint, DeviceQuirks};
    /// # use semver::Version;
    /// #
    /// # let quirks = DeviceQuirks::from_firmware_version(Version::new(1, 8, 0));
    ///
    /// // v1.8-2 及之前，J1 力矩被放大 4x
    /// let scaled = quirks.scale_torque(Joint::J1, 4.0);
    /// assert_eq!(scaled, 1.0); // 4.0 * 0.25 = 1.0
    /// ```
    #[inline]
    pub fn scale_torque(&self, joint: Joint, torque: f64) -> f64 {
        torque * self.torque_scaling[joint as usize]
    }

    /// 检查是否需要对指定关节应用 flip
    ///
    /// # 参数
    ///
    /// * `joint` - 关节索引
    ///
    /// # 返回
    ///
    /// 如果需要 flip 返回 `true`
    #[inline]
    pub fn needs_flip(&self, joint: Joint) -> bool {
        self.joint_flip_map[joint as usize]
    }

    /// 获取指定关节的力矩缩放因子
    ///
    /// # 参数
    ///
    /// * `joint` - 关节索引
    ///
    /// # 返回
    ///
    /// 缩放因子（例如 0.25 表示力矩被放大 4x）
    #[inline]
    pub fn torque_scaling_factor(&self, joint: Joint) -> f64 {
        self.torque_scaling[joint as usize]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_device_quirks_pre_1_7_3() {
        let quirks = DeviceQuirks::from_firmware_version(Version::new(1, 7, 2));

        // v1.7-3 之前，某些关节需要 flip
        assert!(quirks.needs_flip(Joint::J1));
        assert!(quirks.needs_flip(Joint::J2));
        assert!(!quirks.needs_flip(Joint::J3));
        assert!(quirks.needs_flip(Joint::J4));
        assert!(!quirks.needs_flip(Joint::J5));
        assert!(quirks.needs_flip(Joint::J6));

        // 测试 apply_flip
        let (pos, torque) = quirks.apply_flip(Joint::J1, 0.5, 1.0);
        assert_eq!(pos, -0.5);
        assert_eq!(torque, -1.0);

        let (pos, torque) = quirks.apply_flip(Joint::J3, 0.5, 1.0);
        assert_eq!(pos, 0.5); // J3 不 flip
        assert_eq!(torque, 1.0);
    }

    #[test]
    fn test_device_quirks_post_1_7_3() {
        let quirks = DeviceQuirks::from_firmware_version(Version::new(1, 7, 3));

        // v1.7-3 及之后，所有关节不需要 flip
        assert!(!quirks.needs_flip(Joint::J1));
        assert!(!quirks.needs_flip(Joint::J2));
        assert!(!quirks.needs_flip(Joint::J3));
        assert!(!quirks.needs_flip(Joint::J4));
        assert!(!quirks.needs_flip(Joint::J5));
        assert!(!quirks.needs_flip(Joint::J6));

        // 测试 apply_flip
        let (pos, torque) = quirks.apply_flip(Joint::J1, 0.5, 1.0);
        assert_eq!(pos, 0.5); // 不 flip
        assert_eq!(torque, 1.0);
    }

    #[test]
    fn test_device_quirks_torque_scaling_old() {
        let quirks = DeviceQuirks::from_firmware_version(Version::new(1, 8, 0));

        // v1.8-2 及之前，J1-3 需要缩放
        assert_eq!(quirks.torque_scaling_factor(Joint::J1), 0.25);
        assert_eq!(quirks.torque_scaling_factor(Joint::J2), 0.25);
        assert_eq!(quirks.torque_scaling_factor(Joint::J3), 0.25);
        assert_eq!(quirks.torque_scaling_factor(Joint::J4), 1.0);
        assert_eq!(quirks.torque_scaling_factor(Joint::J5), 1.0);
        assert_eq!(quirks.torque_scaling_factor(Joint::J6), 1.0);

        // 测试 scale_torque
        let scaled = quirks.scale_torque(Joint::J1, 4.0);
        assert_eq!(scaled, 1.0); // 4.0 * 0.25 = 1.0

        let scaled = quirks.scale_torque(Joint::J4, 2.0);
        assert_eq!(scaled, 2.0); // J4 无缩放
    }

    #[test]
    fn test_device_quirks_torque_scaling_new() {
        let quirks = DeviceQuirks::from_firmware_version(Version::new(1, 9, 0));

        // v1.8-2 之后，所有关节无缩放
        assert_eq!(quirks.torque_scaling_factor(Joint::J1), 1.0);
        assert_eq!(quirks.torque_scaling_factor(Joint::J2), 1.0);
        assert_eq!(quirks.torque_scaling_factor(Joint::J3), 1.0);
        assert_eq!(quirks.torque_scaling_factor(Joint::J4), 1.0);
        assert_eq!(quirks.torque_scaling_factor(Joint::J5), 1.0);
        assert_eq!(quirks.torque_scaling_factor(Joint::J6), 1.0);

        // 测试 scale_torque
        let scaled = quirks.scale_torque(Joint::J1, 2.0);
        assert_eq!(scaled, 2.0); // 无缩放
    }

    #[test]
    fn test_device_quirks_apply_flip_combined() {
        let quirks = DeviceQuirks::from_firmware_version(Version::new(1, 7, 2));

        // 组合测试：flip + scaling
        let (pos, torque) = quirks.apply_flip(Joint::J1, 1.0, 2.0);
        let torque_scaled = quirks.scale_torque(Joint::J1, torque);

        assert_eq!(pos, -1.0); // flip
        assert_eq!(torque_scaled, -0.5); // flip + scale (2.0 * -0.25 = -0.5)
    }
}
