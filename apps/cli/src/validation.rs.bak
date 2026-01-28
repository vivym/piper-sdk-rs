//! 输入验证模块
//!
//! 提供各种输入验证功能

use anyhow::{Context, Result};
use std::path::Path;

/// 关节位置验证器
pub struct JointValidator {
    /// 最小角度（弧度）
    min_angle: f64,
    /// 最大角度（弧度）
    max_angle: f64,
}

impl JointValidator {
    /// 创建新的关节验证器
    ///
    /// # 参数
    /// * `min_angle` - 最小角度（弧度），默认 -π
    /// * `max_angle` - 最大角度（弧度），默认 π
    pub fn new(min_angle: Option<f64>, max_angle: Option<f64>) -> Self {
        Self {
            min_angle: min_angle.unwrap_or(-std::f64::consts::PI),
            max_angle: max_angle.unwrap_or(std::f64::consts::PI),
        }
    }

    /// 使用默认范围创建验证器（-π 到 π）
    pub fn default_range() -> Self {
        Self::new(None, None)
    }

    /// 验证单个关节位置
    ///
    /// # 错误
    /// 如果位置超出范围，返回错误
    pub fn validate_joint(&self, index: usize, position: f64) -> Result<()> {
        if position < self.min_angle || position > self.max_angle {
            anyhow::bail!(
                "关节 J{} 位置 {:.3} rad 超出范围 [{:.3}, {:.3}]",
                index + 1,
                position,
                self.min_angle,
                self.max_angle
            );
        }
        Ok(())
    }

    /// 验证所有关节位置
    ///
    /// # 参数
    /// * `positions` - 关节位置数组
    ///
    /// # 错误
    /// 如果：
    /// - 位置数量不是 6 个
    /// - 任何位置超出范围
    /// - 位置为 NaN 或无穷大
    #[allow(dead_code)]
    pub fn validate_joints(&self, positions: &[f64]) -> Result<()> {
        if positions.len() != 6 {
            anyhow::bail!("需要 6 个关节位置，得到 {} 个", positions.len());
        }

        for (i, &pos) in positions.iter().enumerate() {
            // 检查 NaN 和无穷大
            if !pos.is_finite() {
                anyhow::bail!(
                    "关节 J{} 位置无效: {}",
                    i + 1,
                    if pos.is_nan() { "NaN" } else { "无穷大" }
                );
            }

            self.validate_joint(i, pos)?;
        }

        Ok(())
    }

    /// 验证并限制关节位置到有效范围
    #[allow(dead_code)]
    pub fn clamp_joints(&self, positions: &mut [f64]) -> Result<()> {
        if positions.len() != 6 {
            anyhow::bail!("需要 6 个关节位置，得到 {} 个", positions.len());
        }

        for (i, pos) in positions.iter_mut().enumerate() {
            if !pos.is_finite() {
                anyhow::bail!("关节 J{} 位置无效", i + 1);
            }

            if *pos < self.min_angle {
                *pos = self.min_angle;
            } else if *pos > self.max_angle {
                *pos = self.max_angle;
            }
        }

        Ok(())
    }
}

/// 文件路径验证器
#[allow(dead_code)]
pub struct PathValidator {
    /// 是否检查文件存在
    check_exists: bool,
    /// 是否要求文件可读
    check_readable: bool,
}

#[allow(dead_code)]
impl PathValidator {
    /// 创建新的路径验证器
    pub fn new() -> Self {
        Self {
            check_exists: false,
            check_readable: false,
        }
    }

    /// 要求文件存在
    pub fn must_exist(mut self) -> Self {
        self.check_exists = true;
        self
    }

    /// 要求文件可读
    #[allow(dead_code)]
    pub fn must_be_readable(mut self) -> Self {
        self.check_readable = true;
        self
    }

    /// 验证文件路径
    pub fn validate_path(&self, path: &str) -> Result<()> {
        let path = Path::new(path);

        // 检查路径格式
        if path.as_os_str().is_empty() {
            anyhow::bail!("文件路径为空");
        }

        // 检查文件存在
        if self.check_exists && !path.exists() {
            anyhow::bail!("文件不存在: {}", path.display());
        }

        // 检查文件可读
        if self.check_readable {
            if !path.exists() {
                anyhow::bail!("文件不存在，无法读取: {}", path.display());
            }

            // 尝试打开文件以验证可读性
            std::fs::File::open(path)
                .with_context(|| format!("无法读取文件: {}", path.display()))?;
        }

        Ok(())
    }

    /// 验证输出路径（目录必须存在）
    #[allow(dead_code)]
    pub fn validate_output_path(&self, path: &str) -> Result<()> {
        let path = Path::new(path);

        if path.as_os_str().is_empty() {
            anyhow::bail!("文件路径为空");
        }

        // 检查父目录是否存在
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
            && !parent.exists()
        {
            anyhow::bail!("输出目录不存在: {}", parent.display());
        }

        Ok(())
    }
}

impl Default for PathValidator {
    fn default() -> Self {
        Self::new()
    }
}

/// CAN ID 验证器
#[allow(dead_code)]
pub struct CanIdValidator;

#[allow(dead_code)]
impl CanIdValidator {
    /// 验证标准 CAN ID (11-bit)
    pub fn validate_standard(id: u32) -> Result<()> {
        if id > 0x7FF {
            anyhow::bail!("标准 CAN ID 必须小于 0x7FF，得到: 0x{:03X}", id);
        }
        Ok(())
    }

    /// 验证扩展 CAN ID (29-bit)
    pub fn validate_extended(id: u32) -> Result<()> {
        if id > 0x1FFFFFFF {
            anyhow::bail!("扩展 CAN ID 必须小于 0x1FFFFFFF，得到: 0x{:08X}", id);
        }
        Ok(())
    }

    /// 验证 CAN ID（自动检测标准或扩展）
    pub fn validate(id: u32) -> Result<()> {
        if id <= 0x7FF {
            Self::validate_standard(id)
        } else if id <= 0x1FFFFFFF {
            Self::validate_extended(id)
        } else {
            anyhow::bail!("CAN ID 无效: 0x{:08X}", id)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_joint_validator_valid() {
        let validator = JointValidator::default_range();
        let positions = [0.0, 0.1, 0.2, 0.3, 0.4, 0.5];
        assert!(validator.validate_joints(&positions).is_ok());
    }

    #[test]
    fn test_joint_validator_out_of_range() {
        let validator = JointValidator::default_range();
        let positions = [0.0, 0.1, 4.0, 0.3, 0.4, 0.5]; // 4.0 > π
        assert!(validator.validate_joints(&positions).is_err());
    }

    #[test]
    fn test_joint_validator_nan() {
        let validator = JointValidator::default_range();
        let positions = [0.0, f64::NAN, 0.2, 0.3, 0.4, 0.5];
        assert!(validator.validate_joints(&positions).is_err());
    }

    #[test]
    fn test_joint_validator_wrong_count() {
        let validator = JointValidator::default_range();
        let positions = [0.0, 0.1, 0.2]; // 只有 3 个
        assert!(validator.validate_joints(&positions).is_err());
    }

    #[test]
    fn test_path_validator_exists() {
        let validator = PathValidator::new().must_exist();
        // 测试不存在的文件
        assert!(validator.validate_path("/nonexistent/file.txt").is_err());
        // 测试当前目录（应该存在）
        assert!(validator.validate_path(".").is_ok());
    }

    #[test]
    fn test_can_id_validator() {
        assert!(CanIdValidator::validate(0x123).is_ok()); // 标准 ID
        assert!(CanIdValidator::validate(0x12345678).is_ok()); // 扩展 ID
        assert!(CanIdValidator::validate(0x2FFFFFFF).is_err()); // 无效 ID
    }
}
