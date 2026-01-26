//! run 命令
//!
//! 执行脚本文件

use anyhow::Result;
use clap::Args;

use crate::script::ScriptExecutor;

/// 脚本执行命令参数
#[derive(Args, Debug)]
pub struct RunCommand {
    /// 脚本文件路径
    #[arg(short, long)]
    pub script: String,

    /// CAN 接口（覆盖配置）
    #[arg(short, long)]
    pub interface: Option<String>,

    /// 设备序列号（GS-USB）
    #[arg(short, long)]
    pub serial: Option<String>,

    /// 失败时继续执行
    #[arg(long)]
    pub continue_on_error: bool,
}

impl RunCommand {
    /// 执行脚本
    pub async fn execute(&self) -> Result<()> {
        println!("📜 加载脚本: {}", self.script);

        let script = ScriptExecutor::load_script(&self.script)?;

        println!("📋 脚本: {}", script.name);
        println!("    {}", script.description);
        println!("    {} 个命令", script.commands.len());
        println!();

        // 创建脚本执行器并配置
        let config = crate::script::ScriptConfig {
            interface: self.interface.clone(),
            serial: self.serial.clone(),
            continue_on_error: self.continue_on_error,
            execution_delay_ms: 100, // 默认延迟
        };

        let mut executor = ScriptExecutor::new().with_config(config);

        // 执行脚本
        let result = executor.execute(&script).await?;

        println!();
        println!("📊 执行结果:");
        println!("  总命令数: {}", result.total_commands);
        println!("  成功: {}", result.succeeded.len());
        println!("  失败: {}", result.failed.len());
        println!("  耗时: {:.2} 秒", result.duration_secs);

        if !result.failed.is_empty() {
            println!();
            println!("❌ 失败的命令:");
            for (idx, err) in &result.failed {
                println!("  命令 {}: {}", idx + 1, err);
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_run_command_creation() {
        let cmd = RunCommand {
            script: "test.json".to_string(),
            interface: Some("can0".to_string()),
            serial: None,
            continue_on_error: true,
        };

        assert_eq!(cmd.script, "test.json");
        assert_eq!(cmd.interface, Some("can0".to_string()));
        assert!(cmd.continue_on_error);
    }

    #[test]
    fn test_run_command_defaults() {
        let cmd = RunCommand {
            script: "test.json".to_string(),
            interface: None,
            serial: None,
            continue_on_error: false,
        };

        assert!(!cmd.continue_on_error);
    }
}
