//! run 命令

use anyhow::Result;
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

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use piper_control::TargetSpec;

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
}
