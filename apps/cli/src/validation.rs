//! 输入验证模块
//!
//! 提供各种输入验证功能

use anyhow::{Context, Result};
use std::path::Path;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_path_validator_exists() {
        let validator = PathValidator::new().must_exist();
        // 测试不存在的文件
        assert!(validator.validate_path("/nonexistent/file.txt").is_err());
        // 测试当前目录（应该存在）
        assert!(validator.validate_path(".").is_ok());
    }
}
