//! 脚本系统

use anyhow::{Context, Result};
use piper_client::MotionConnectedPiper;
use piper_client::state::{MotionCapability, Piper, Standby};
use piper_control::{
    ControlProfile, home_zero_blocking, move_to_joint_target_blocking, park_blocking, prepare_move,
    set_joint_zero_blocking,
};
use piper_sdk::driver::ConnectionTarget;
use serde::{Deserialize, Serialize};
use std::fs;

use crate::connection::client_builder;
use crate::parsing::normalize_joint_indices;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Script {
    pub name: String,
    pub description: String,
    pub commands: Vec<ScriptCommand>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ScriptCommand {
    Move {
        joints: Vec<f64>,
        #[serde(default)]
        force: bool,
    },
    Wait {
        duration_ms: u64,
    },
    Position,
    Home,
    Park,
    SetZero {
        #[serde(default)]
        joints: Option<Vec<usize>>,
        #[serde(default)]
        force: bool,
    },
    Stop,
}

pub struct ScriptExecutor {
    config: ScriptConfig,
}

#[derive(Debug, Clone)]
pub struct ScriptConfig {
    pub profile: ControlProfile,
    pub continue_on_error: bool,
    pub execution_delay_ms: u64,
}

impl ScriptExecutor {
    pub fn new() -> Self {
        Self {
            config: ScriptConfig {
                profile: ControlProfile {
                    target: ConnectionTarget::AutoStrict,
                    orientation: piper_control::ParkOrientation::Upright,
                    rest_pose_override: None,
                    safety: piper_tools::SafetyConfig::default_config(),
                    wait: piper_control::MotionWaitConfig::default(),
                },
                continue_on_error: false,
                execution_delay_ms: 100,
            },
        }
    }

    pub fn with_config(mut self, config: ScriptConfig) -> Self {
        self.config = config;
        self
    }

    pub fn load_script<P: AsRef<std::path::Path>>(path: P) -> Result<Script> {
        let content = fs::read_to_string(path).context("读取脚本文件失败")?;
        serde_json::from_str(&content).context("解析脚本 JSON 失败")
    }

    #[allow(dead_code)]
    pub fn save_script<P: AsRef<std::path::Path>>(path: P, script: &Script) -> Result<()> {
        let content = serde_json::to_string_pretty(script).context("序列化脚本失败")?;
        fs::write(path, content).context("写入脚本文件失败")
    }

    pub async fn execute(&mut self, script: &Script) -> Result<ScriptResult> {
        println!("📜 执行脚本: {}", script.name);
        println!("📝 {}", script.description);
        println!();

        let builder = client_builder(&self.config.profile.target);
        let reconnect_target = self.config.profile.target.clone();

        println!("🔌 连接到机器人...");
        let connected = builder.build()?.require_motion()?;
        println!("✅ 已连接\n");

        match connected {
            MotionConnectedPiper::Strict(standby) => self.execute_motion_script(standby, script, || {
                match client_builder(&reconnect_target).build()?.require_motion()? {
                    MotionConnectedPiper::Strict(standby) => Ok(standby),
                    MotionConnectedPiper::Soft(_) => Err(anyhow::anyhow!(
                        "reconnect changed backend capability from StrictRealtime to SoftRealtime"
                    )),
                }
            })
            .await,
            MotionConnectedPiper::Soft(standby) => self.execute_motion_script(standby, script, || {
                match client_builder(&reconnect_target).build()?.require_motion()? {
                    MotionConnectedPiper::Soft(standby) => Ok(standby),
                    MotionConnectedPiper::Strict(_) => Err(anyhow::anyhow!(
                        "reconnect changed backend capability from SoftRealtime to StrictRealtime"
                    )),
                }
            })
            .await,
        }
    }

    async fn execute_motion_script<Capability, Reconnect>(
        &mut self,
        mut standby: Piper<Standby, Capability>,
        script: &Script,
        reconnect: Reconnect,
    ) -> Result<ScriptResult>
    where
        Capability: MotionCapability,
        Reconnect: Fn() -> Result<Piper<Standby, Capability>>,
    {
        let mut result = ScriptResult {
            script_name: script.name.clone(),
            total_commands: script.commands.len(),
            succeeded: Vec::new(),
            failed: Vec::new(),
            start_time: std::time::SystemTime::now(),
            end_time: None,
            duration_secs: 0.0,
        };

        for (index, command) in script.commands.iter().enumerate() {
            println!("命令 {}/{}:", index + 1, result.total_commands);

            match self.execute_command(standby, command).await {
                Ok(ExecutionOutcome::Continue(next)) => {
                    println!("  ✅ 成功");
                    standby = next;
                    result.succeeded.push(index);
                },
                Ok(ExecutionOutcome::Stop) => {
                    println!("  ✅ 已停止");
                    result.succeeded.push(index);
                    break;
                },
                Err(failure) => {
                    println!("  ❌ 失败: {}", failure.error);
                    result.failed.push((index, failure.error.to_string()));
                    if !self.config.continue_on_error {
                        println!();
                        println!("❌ 脚本执行失败，停止执行");
                        break;
                    }

                    standby = if let Some(standby) = failure.standby {
                        standby
                    } else {
                        println!("  ↻ 当前状态不可安全复用，重新连接到 Standby...");
                        reconnect()?
                    };
                },
            }

            if index < script.commands.len() - 1 {
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

        Ok(result)
    }

    async fn execute_command<Capability>(
        &self,
        standby: Piper<Standby, Capability>,
        command: &ScriptCommand,
    ) -> std::result::Result<ExecutionOutcome<Capability>, CommandFailure<Capability>>
    where
        Capability: MotionCapability,
    {
        match command {
            ScriptCommand::Move { joints, force } => {
                println!("  移动: joints = {:?}", joints);
                let positions =
                    standby.observer().joint_positions().map_err(CommandFailure::lost_standby)?;
                let current = std::array::from_fn(|index| positions[index].0);
                let prepared =
                    match prepare_move(current, joints, &self.config.profile.safety, *force) {
                        Ok(prepared) => prepared,
                        Err(error) => return Err(CommandFailure::recoverable(error, standby)),
                    };
                if prepared.requires_confirmation {
                    return Err(CommandFailure::recoverable(
                        anyhow::anyhow!("脚本中的大幅移动必须显式设置 force=true"),
                        standby,
                    ));
                }
                let next = move_to_joint_target_blocking(
                    standby,
                    &self.config.profile,
                    prepared.effective_target,
                )
                .map_err(CommandFailure::lost_standby)?;
                Ok(ExecutionOutcome::Continue(next))
            },
            ScriptCommand::Wait { duration_ms } => {
                println!("  等待: {} ms", duration_ms);
                tokio::time::sleep(tokio::time::Duration::from_millis(*duration_ms)).await;
                Ok(ExecutionOutcome::Continue(standby))
            },
            ScriptCommand::Position => {
                println!("  查询位置");
                let positions =
                    standby.observer().joint_positions().map_err(CommandFailure::lost_standby)?;
                for (index, position) in positions.iter().enumerate() {
                    println!(
                        "    J{}: {:.3} rad ({:.1}°)",
                        index + 1,
                        position.0,
                        position.to_deg().0
                    );
                }
                Ok(ExecutionOutcome::Continue(standby))
            },
            ScriptCommand::Home => {
                println!("  回零");
                let next = home_zero_blocking(standby, &self.config.profile)
                    .map_err(CommandFailure::lost_standby)?;
                Ok(ExecutionOutcome::Continue(next))
            },
            ScriptCommand::Park => {
                println!("  停靠");
                let next = park_blocking(standby, &self.config.profile)
                    .map_err(CommandFailure::lost_standby)?;
                Ok(ExecutionOutcome::Continue(next))
            },
            ScriptCommand::SetZero { joints, force } => {
                println!("  设置零点");
                if !force {
                    return Err(CommandFailure::recoverable(
                        anyhow::anyhow!("脚本中的 set-zero 必须显式设置 force=true"),
                        standby,
                    ));
                }
                let joints = match normalize_set_zero_joints(joints.as_deref()) {
                    Ok(joints) => joints,
                    Err(error) => return Err(CommandFailure::recoverable(error, standby)),
                };
                if let Err(error) = set_joint_zero_blocking(&standby, &joints) {
                    return Err(CommandFailure::recoverable(error, standby));
                }
                Ok(ExecutionOutcome::Continue(standby))
            },
            ScriptCommand::Stop => {
                println!("  🛑 急停");
                if let Err(error) = standby.disable_all() {
                    return Err(CommandFailure::recoverable(error, standby));
                }
                Ok(ExecutionOutcome::Stop)
            },
        }
    }
}

impl Default for ScriptExecutor {
    fn default() -> Self {
        Self::new()
    }
}

enum ExecutionOutcome<Capability> {
    Continue(Piper<Standby, Capability>),
    Stop,
}

fn normalize_set_zero_joints(joints: Option<&[usize]>) -> Result<Vec<usize>> {
    let Some(joints) = joints else {
        return Ok((0..6).collect());
    };
    normalize_joint_indices(joints)
}

struct CommandFailure<Capability> {
    error: anyhow::Error,
    standby: Option<Piper<Standby, Capability>>,
}

impl<Capability> CommandFailure<Capability> {
    fn recoverable(error: impl Into<anyhow::Error>, standby: Piper<Standby, Capability>) -> Self {
        Self {
            error: error.into(),
            standby: Some(standby),
        }
    }

    fn lost_standby(error: impl Into<anyhow::Error>) -> Self {
        Self {
            error: error.into(),
            standby: None,
        }
    }
}

#[derive(Debug)]
pub struct ScriptResult {
    #[allow(dead_code)]
    pub script_name: String,
    pub total_commands: usize,
    pub succeeded: Vec<usize>,
    pub failed: Vec<(usize, String)>,
    pub start_time: std::time::SystemTime,
    pub end_time: Option<std::time::SystemTime>,
    pub duration_secs: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn script_serialization_round_trip() {
        let script = Script {
            name: "测试脚本".to_string(),
            description: "测试".to_string(),
            commands: vec![
                ScriptCommand::Home,
                ScriptCommand::Park,
                ScriptCommand::SetZero {
                    joints: Some(vec![1, 3, 6]),
                    force: true,
                },
            ],
        };

        let json = serde_json::to_string(&script).unwrap();
        let decoded: Script = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.commands.len(), 3);
    }

    #[test]
    fn normalize_set_zero_joints_defaults_to_all() {
        assert_eq!(
            normalize_set_zero_joints(None).unwrap(),
            vec![0, 1, 2, 3, 4, 5]
        );
    }

    #[test]
    fn normalize_set_zero_joints_converts_to_zero_based() {
        assert_eq!(
            normalize_set_zero_joints(Some(&[1, 3, 6])).unwrap(),
            vec![0, 2, 5]
        );
    }
}
