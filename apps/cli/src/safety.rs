//! 安全检查模块
//!
//! 提供命令执行前的安全验证

use anyhow::{Result, bail};
use piper_tools::SafetyConfig;

/// 安全检查器
#[allow(dead_code)]
pub struct SafetyChecker {
    config: SafetyConfig,
}

#[allow(dead_code)]
impl SafetyChecker {
    /// 创建新的安全检查器
    pub fn new() -> Self {
        Self {
            config: SafetyConfig::default_config(),
        }
    }

    /// 检查关节位置是否在限制内
    pub fn check_joint_positions(&self, positions: &[f64]) -> Result<()> {
        for (i, &pos) in positions.iter().enumerate() {
            if !self.config.check_joint_position(i, pos) {
                bail!("关节 J{} 位置超出限制: {:.3} rad", i + 1, pos);
            }
        }

        Ok(())
    }

    /// 检查是否需要用户确认
    pub fn requires_confirmation(&self, positions: &[f64]) -> bool {
        // 计算最大角度变化
        let max_delta = positions.iter().map(|&p| p.abs()).fold(0.0_f64, f64::max);

        // 转换为角度
        let max_delta_degrees = max_delta * 180.0 / std::f64::consts::PI;

        self.config.requires_confirmation(max_delta_degrees)
    }

    /// 显示确认提示
    #[allow(dead_code)]
    pub fn show_confirmation_prompt(&self, positions: &[f64]) -> Result<bool> {
        let max_delta = positions.iter().map(|&p| p.abs()).fold(0.0_f64, f64::max);

        let max_delta_degrees = max_delta * 180.0 / std::f64::consts::PI;

        println!("⚠️  大幅移动检测");
        println!("  最大角度: {:.1}°", max_delta_degrees);

        // ✅ 使用 inquire 提供更好的交互体验
        let confirmed = inquire::Confirm::new("确定要继续吗？")
            .with_default(false)  // 默认为 No（安全优先）
            .prompt()
            .map_err(|e| anyhow::anyhow!("用户交互失败: {}", e))?;

        Ok(confirmed)
    }
}

impl Default for SafetyChecker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_check_joint_positions() {
        let checker = SafetyChecker::new();

        // 正常范围内的位置
        let positions = vec![0.1, 0.2, 0.3];
        assert!(checker.check_joint_positions(&positions).is_ok());

        // 超出限制的位置
        let positions = vec![10.0]; // 远超 3.14 rad 限制
        assert!(checker.check_joint_positions(&positions).is_err());
    }

    #[test]
    fn test_requires_confirmation() {
        let checker = SafetyChecker::new();

        // 小幅度移动，无需确认
        let positions = vec![0.05]; // < 10度
        assert!(!checker.requires_confirmation(&positions));

        // 大幅度移动，需要确认
        let positions = vec![0.5]; // > 10度
        assert!(checker.requires_confirmation(&positions));
    }
}
