//! 脚本系统
//!
//! JSON 脚本执行和回放

use anyhow::{Context, Result};
use piper_client::PiperBuilder;
use piper_client::state::PositionModeConfig;
use piper_client::types::{JointArray, Rad};
use serde::{Deserialize, Serialize};
use std::fs;

/// 脚本命令序列
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Script {
    /// 脚本名称
    pub name: String,

    /// 脚本描述
    pub description: String,

    /// 命令序列
    pub commands: Vec<ScriptCommand>,
}

/// 脚本命令
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ScriptCommand {
    /// 移动命令
    Move {
        joints: Vec<f64>,
        #[serde(default)]
        force: bool,
    },

    /// 等待命令
    Wait { duration_ms: u64 },

    /// 查询位置
    Position,

    /// 回零位
    Home,

    /// 急停
    Stop,
}

/// 脚本执行器
pub struct ScriptExecutor {
    /// 当前配置
    config: ScriptConfig,
}

/// 脚本配置
#[derive(Debug, Clone)]
pub struct ScriptConfig {
    /// CAN 接口
    pub interface: Option<String>,

    /// 设备序列号
    #[allow(dead_code)]
    pub serial: Option<String>,

    /// 失败时是否继续
    pub continue_on_error: bool,

    /// 执行延迟（毫秒）
    pub execution_delay_ms: u64,
}

impl Default for ScriptConfig {
    fn default() -> Self {
        Self {
            interface: None,
            serial: None,
            continue_on_error: false,
            execution_delay_ms: 100,
        }
    }
}

impl ScriptExecutor {
    /// 创建新的脚本执行器
    pub fn new() -> Self {
        Self {
            config: ScriptConfig::default(),
        }
    }

    /// 设置配置
    pub fn with_config(mut self, config: ScriptConfig) -> Self {
        self.config = config;
        self
    }

    /// 加载脚本文件
    pub fn load_script<P: AsRef<std::path::Path>>(path: P) -> Result<Script> {
        let content = fs::read_to_string(path).context("读取脚本文件失败")?;

        let script: Script = serde_json::from_str(&content).context("解析脚本 JSON 失败")?;

        Ok(script)
    }

    /// 保存脚本文件
    #[allow(dead_code)]
    pub fn save_script<P: AsRef<std::path::Path>>(path: P, script: &Script) -> Result<()> {
        let content = serde_json::to_string_pretty(script).context("序列化脚本失败")?;

        fs::write(path, content).context("写入脚本文件失败")?;

        Ok(())
    }

    /// 执行脚本
    pub async fn execute(&mut self, script: &Script) -> Result<ScriptResult> {
        println!("📜 执行脚本: {}", script.name);
        println!("📝 {}", script.description);
        println!();

        // 连接到机器人
        println!("🔌 连接到机器人...");
        let mut builder = PiperBuilder::new();
        if let Some(interface) = &self.config.interface {
            builder = builder.interface(interface);
        }

        let robot = builder.build()?;
        println!("✅ 已连接");

        // 使能位置模式
        let config_mode = PositionModeConfig::default();
        let robot = robot.enable_position_mode(config_mode)?;
        println!("⚡ 已使能 Position Mode\n");

        let mut result = ScriptResult {
            script_name: script.name.clone(),
            total_commands: script.commands.len(),
            succeeded: Vec::new(),
            failed: Vec::new(),
            start_time: std::time::SystemTime::now(),
            end_time: None,
            duration_secs: 0.0,
        };

        for (i, cmd) in script.commands.iter().enumerate() {
            println!("命令 {}/{}:", i + 1, result.total_commands);

            match self.execute_command(&robot, cmd).await {
                Ok(_) => {
                    println!("  ✅ 成功");
                    result.succeeded.push(i);
                },

                Err(err) => {
                    println!("  ❌ 失败: {}", err);
                    result.failed.push((i, err.to_string()));

                    if !self.config.continue_on_error {
                        println!();
                        println!("❌ 脚本执行失败，停止执行");
                        break;
                    }
                },
            }

            // 执行延迟
            if i < script.commands.len() - 1 {
                tokio::time::sleep(tokio::time::Duration::from_millis(
                    self.config.execution_delay_ms,
                ))
                .await;
            }
        }

        result.end_time = Some(std::time::SystemTime::now());
        result.duration_secs = result
            .end_time
            .unwrap()
            .duration_since(result.start_time)
            .unwrap_or_default()
            .as_secs_f64();

        println!();
        println!("📊 脚本执行结果:");
        println!("  总命令数: {}", result.total_commands);
        println!("  成功: {}", result.succeeded.len());
        println!("  失败: {}", result.failed.len());

        // robot 在这里 drop，自动 disable
        Ok(result)
    }

    /// 执行单个命令
    async fn execute_command(
        &self,
        robot: &piper_client::state::Piper<
            piper_client::state::Active<piper_client::state::PositionMode>,
        >,
        cmd: &ScriptCommand,
    ) -> Result<()> {
        match cmd {
            ScriptCommand::Move { joints, force: _ } => {
                println!("  移动: joints = {:?}", joints);

                // 转换为 JointArray<Rad>
                let mut joint_array = JointArray::from([Rad(0.0); 6]);
                for (i, pos) in joints.iter().enumerate() {
                    if i < 6 {
                        joint_array[i] = Rad(*pos);
                    }
                }

                // 发送位置命令
                robot.send_position_command(&joint_array)?;

                // 等待一小段时间让运动开始
                tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

                Ok(())
            },

            ScriptCommand::Wait { duration_ms } => {
                println!("  等待: {} ms", duration_ms);
                tokio::time::sleep(tokio::time::Duration::from_millis(*duration_ms)).await;
                Ok(())
            },

            ScriptCommand::Position => {
                println!("  查询位置");

                // 获取 Observer 并读取位置
                let observer = robot.observer();
                let snapshot = observer.snapshot();

                for (i, pos) in snapshot.position.iter().enumerate() {
                    let deg = pos.to_deg();
                    println!("    J{}: {:.3} rad ({:.1}°)", i + 1, pos.0, deg.0);
                }

                Ok(())
            },

            ScriptCommand::Home => {
                println!("  回零位");

                // 移动到零位
                let zero_array =
                    JointArray::from([Rad(0.0), Rad(0.0), Rad(0.0), Rad(0.0), Rad(0.0), Rad(0.0)]);

                robot.send_position_command(&zero_array)?;

                // 等待回零完成
                tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

                Ok(())
            },

            ScriptCommand::Stop => {
                println!("  急停");

                // 注意：这里不能直接 disable，因为我们在 Active 状态
                // 应该发送失能命令，但这会转换状态
                // 简化实现：仅提示
                println!("    ⚠️  脚本中的急停不会立即生效");
                println!("    💡 建议：使用 Ctrl+C 或单独的 stop 命令");

                Ok(())
            },
        }
    }
}

impl Default for ScriptExecutor {
    fn default() -> Self {
        Self::new()
    }
}

/// 脚本执行结果
#[derive(Debug)]
pub struct ScriptResult {
    /// 脚本名称
    #[allow(dead_code)]
    pub script_name: String,

    /// 总命令数
    pub total_commands: usize,

    /// 成功的命令索引
    pub succeeded: Vec<usize>,

    /// 失败的命令索引和错误
    pub failed: Vec<(usize, String)>,

    /// 开始时间
    pub start_time: std::time::SystemTime,

    /// 结束时间
    pub end_time: Option<std::time::SystemTime>,

    /// 脚本执行时长（秒）
    pub duration_secs: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_script_serialization() {
        let script = Script {
            name: "测试脚本".to_string(),
            description: "测试脚本描述".to_string(),
            commands: vec![
                ScriptCommand::Home,
                ScriptCommand::Move {
                    joints: vec![0.1, 0.2, 0.3],
                    force: false,
                },
                ScriptCommand::Wait { duration_ms: 1000 },
            ],
        };

        let json = serde_json::to_string(&script).unwrap();
        assert!(json.contains("测试脚本"));
    }

    #[test]
    fn test_script_deserialization() {
        let json = r#"
        {
            "name": "测试脚本",
            "description": "测试描述",
            "commands": [
                {
                    "type": "Home"
                },
                {
                    "type": "Move",
                    "joints": [0.1, 0.2, 0.3],
                    "force": false
                }
            ]
        }
        "#;

        let script: Script = serde_json::from_str(json).unwrap();
        assert_eq!(script.name, "测试脚本");
        assert_eq!(script.commands.len(), 2);
    }
}
