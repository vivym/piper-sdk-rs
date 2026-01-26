//! # 安全配置
//!
//! 机器人运动控制的安全限制和配置

use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::Path;

/// 安全配置
///
/// 定义机器人运动控制的安全限制
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SafetyConfig {
    /// 安全限制
    pub limits: SafetyLimits,

    /// 确认设置
    pub confirmation: ConfirmationSettings,

    /// E-Stop 设置
    pub estop: EStopSettings,
}

impl SafetyConfig {
    /// 创建默认配置
    pub fn default_config() -> Self {
        Self {
            limits: SafetyLimits::default(),
            confirmation: ConfirmationSettings::default(),
            estop: EStopSettings::default(),
        }
    }

    /// 从文件加载配置
    ///
    /// 配置文件路径：
    /// - Linux/macOS: `~/.config/piper/safety.toml`
    /// - Windows: `%APPDATA%\piper\safety.toml`
    pub fn load_from_file<P: AsRef<Path>>(path: P) -> Result<Self, io::Error> {
        let _content = fs::read_to_string(path)?;

        // ⚠️ 简化实现：实际应该使用 TOML 解析
        // 这里提供一个框架，需要添加 toml 依赖
        // let config: SafetyConfig = toml::from_str(&content)?;

        // 暂时返回默认配置
        Ok(Self::default_config())
    }

    /// 保存配置到文件
    pub fn save_to_file<P: AsRef<Path>>(&self, path: P) -> Result<(), io::Error> {
        // ⚠️ 简化实现：实际应该序列化为 TOML
        let content = format!(
            r#"[safety]
max_velocity = {}
max_acceleration = {}
"#,
            self.limits.max_velocity, self.limits.max_acceleration
        );

        fs::write(path, content)
    }

    /// 检查速度是否在限制内
    pub fn check_velocity(&self, velocity: f64) -> bool {
        velocity.abs() <= self.limits.max_velocity
    }

    /// 检查加速度是否在限制内
    pub fn check_acceleration(&self, acceleration: f64) -> bool {
        acceleration.abs() <= self.limits.max_acceleration
    }

    /// 检查关节位置是否在限制内
    pub fn check_joint_position(&self, joint_index: usize, position: f64) -> bool {
        if joint_index >= self.limits.joints_min.len() {
            return false;
        }

        let min = self.limits.joints_min[joint_index];
        let max = self.limits.joints_max[joint_index];

        position >= min && position <= max
    }

    /// 检查是否需要确认
    pub fn requires_confirmation(&self, max_delta_angle: f64) -> bool {
        max_delta_angle > self.confirmation.threshold_degrees
    }
}

/// 安全限制
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SafetyLimits {
    /// 最大速度（rad/s）
    pub max_velocity: f64,

    /// 最大加速度（rad/s²）
    pub max_acceleration: f64,

    /// 关节位置下限（rad）
    pub joints_min: Vec<f64>,

    /// 关节位置上限（rad）
    pub joints_max: Vec<f64>,

    /// 单步最大角度（度）
    pub max_step_angle: f64,
}

impl Default for SafetyLimits {
    fn default() -> Self {
        Self {
            // ⚠️ 这些值应该根据实际机器人参数调整
            max_velocity: 3.0,      // rad/s
            max_acceleration: 10.0, // rad/s²
            joints_min: vec![
                -std::f64::consts::PI,
                -std::f64::consts::FRAC_PI_2,
                -std::f64::consts::FRAC_PI_2,
                -std::f64::consts::FRAC_PI_2,
                -std::f64::consts::FRAC_PI_2,
                -std::f64::consts::PI, // 6关节机器人
            ],
            joints_max: vec![
                std::f64::consts::PI,
                std::f64::consts::FRAC_PI_2,
                std::f64::consts::FRAC_PI_2,
                std::f64::consts::FRAC_PI_2,
                std::f64::consts::FRAC_PI_2,
                std::f64::consts::PI,
            ],
            max_step_angle: 30.0, // 度
        }
    }
}

/// 确认设置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfirmationSettings {
    /// 确认阈值（度）
    ///
    /// 单步移动角度超过此值时需要用户确认
    pub threshold_degrees: f64,

    /// 启用确认
    pub enabled: bool,
}

impl Default for ConfirmationSettings {
    fn default() -> Self {
        Self {
            threshold_degrees: 10.0, // 10度以上需要确认
            enabled: true,
        }
    }
}

/// E-Stop 设置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EStopSettings {
    /// 启用软件急停
    pub enabled: bool,

    /// 急停响应超时（ms）
    pub timeout_ms: u64,
}

impl Default for EStopSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            timeout_ms: 50, // 50ms 内必须响应
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = SafetyConfig::default_config();
        assert!(config.check_velocity(1.0));
        assert!(config.check_acceleration(5.0));
    }

    #[test]
    fn test_velocity_limit() {
        let config = SafetyConfig::default_config();

        // 在限制内
        assert!(config.check_velocity(1.0));
        assert!(config.check_velocity(3.0));

        // 超出限制
        assert!(!config.check_velocity(4.0));
        assert!(!config.check_velocity(-4.0));
    }

    #[test]
    fn test_acceleration_limit() {
        let config = SafetyConfig::default_config();

        // 在限制内
        assert!(config.check_acceleration(5.0));
        assert!(config.check_acceleration(10.0));

        // 超出限制
        assert!(!config.check_acceleration(15.0));
    }

    #[test]
    fn test_joint_position_limit() {
        let config = SafetyConfig::default_config();

        // 在限制内
        assert!(config.check_joint_position(0, 0.0));
        assert!(config.check_joint_position(0, 3.0));

        // 超出限制
        assert!(!config.check_joint_position(0, 3.2));
        assert!(!config.check_joint_position(0, -3.2));

        // 无效的关节索引
        assert!(!config.check_joint_position(10, 0.0));
    }

    #[test]
    fn test_confirmation_required() {
        let config = SafetyConfig::default_config();

        // 小幅度移动，无需确认
        assert!(!config.requires_confirmation(5.0));

        // 大幅度移动，需要确认
        assert!(config.requires_confirmation(15.0));
    }

    #[test]
    fn test_safety_limits() {
        let limits = SafetyLimits::default();
        assert_eq!(limits.max_velocity, 3.0);
        assert_eq!(limits.max_acceleration, 10.0);
        assert_eq!(limits.max_step_angle, 30.0);
        assert_eq!(limits.joints_min.len(), 6);
        assert_eq!(limits.joints_max.len(), 6);
    }
}
