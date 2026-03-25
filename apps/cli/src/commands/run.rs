//! run 命令

use anyhow::{Result, anyhow};
use clap::Args;

use crate::commands::config::CliConfig;
use crate::connection::TargetArgs;
use crate::script::ScriptExecutor;

#[derive(Args, Debug, Clone)]
pub struct RunCommand {
    /// 脚本文件路径
    #[arg(short, long)]
    pub script: String,

    #[command(flatten)]
    pub target: TargetArgs,

    /// 失败时继续执行
    #[arg(long)]
    pub continue_on_error: bool,
}

impl RunCommand {
    fn status_from_result(result: &crate::script::ScriptResult) -> Result<()> {
        if result.failed.is_empty() {
            Ok(())
        } else {
            Err(anyhow!(
                "script finished with {} failed command(s)",
                result.failed.len()
            ))
        }
    }

    pub async fn execute(&self) -> Result<()> {
        println!("📜 加载脚本: {}", self.script);
        let script = ScriptExecutor::load_script(&self.script)?;
        let config = CliConfig::load()?;
        let profile = config.control_profile(self.target.target.as_ref());

        println!("📋 脚本: {}", script.name);
        println!("    {}", script.description);
        println!("    {} 个命令", script.commands.len());
        println!();

        let executor_config = crate::script::ScriptConfig {
            profile,
            continue_on_error: self.continue_on_error,
            execution_delay_ms: 100,
        };

        let mut executor = ScriptExecutor::new().with_config(executor_config);
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

        Self::status_from_result(&result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use piper_control::TargetSpec;
    use std::time::SystemTime;

    #[test]
    fn run_command_creation() {
        let cmd = RunCommand {
            script: "test.json".to_string(),
            target: TargetArgs {
                target: Some(TargetSpec::SocketCan {
                    iface: "can0".to_string(),
                }),
            },
            continue_on_error: true,
        };

        assert_eq!(cmd.script, "test.json");
        assert!(cmd.continue_on_error);
    }

    #[test]
    fn run_command_reports_error_when_script_contains_failures() {
        let result = crate::script::ScriptResult {
            script_name: "test".to_string(),
            total_commands: 2,
            succeeded: vec![0],
            failed: vec![(1, "boom".to_string())],
            start_time: SystemTime::UNIX_EPOCH,
            end_time: Some(SystemTime::UNIX_EPOCH),
            duration_secs: 0.1,
        };

        assert!(RunCommand::status_from_result(&result).is_err());
    }

    #[test]
    fn run_command_accepts_fully_successful_script_result() {
        let result = crate::script::ScriptResult {
            script_name: "test".to_string(),
            total_commands: 2,
            succeeded: vec![0, 1],
            failed: vec![],
            start_time: SystemTime::UNIX_EPOCH,
            end_time: Some(SystemTime::UNIX_EPOCH),
            duration_secs: 0.1,
        };

        assert!(RunCommand::status_from_result(&result).is_ok());
    }
}
