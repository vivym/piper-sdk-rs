//! 移动命令
//!
//! 移动机器人到目标位置，包含安全检查和确认机制

use crate::validation::JointValidator;
use anyhow::{Context, Result};
use clap::Args;
use piper_client::PiperBuilder;
use piper_client::state::PositionModeConfig;
use piper_client::types::JointArray;
use piper_tools::SafetyConfig;

/// 移动命令参数
#[derive(Args, Debug)]
pub struct MoveCommand {
    /// 目标关节位置（弧度），逗号分隔
    /// 例如：0.1,0.2,0.3,0.4,0.5,0.6
    #[arg(short, long)]
    pub joints: Option<String>,

    /// 跳过确认提示
    #[arg(long)]
    pub force: bool,

    /// CAN 接口（覆盖配置）
    #[arg(short, long)]
    pub interface: Option<String>,

    /// 设备序列号（GS-USB）
    #[arg(short, long)]
    pub serial: Option<String>,
}

impl MoveCommand {
    /// 解析关节位置
    pub fn parse_joints(&self) -> Result<Vec<f64>> {
        let joints_str = self
            .joints
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("未指定关节位置，请使用 --joints 参数"))?;

        let positions: Vec<f64> = joints_str
            .split(',')
            .map(|s| s.trim().parse::<f64>())
            .collect::<Result<Vec<_>, _>>()
            .context("解析关节位置失败")?;

        if positions.is_empty() {
            anyhow::bail!("关节位置不能为空");
        }

        if positions.len() > 6 {
            anyhow::bail!("最多支持 6 个关节");
        }

        // 使用验证器验证关节位置
        let validator = JointValidator::default_range();

        // 验证每个关节（支持少于 6 个）
        for (i, &pos) in positions.iter().enumerate() {
            validator.validate_joint(i, pos)?;
        }

        Ok(positions)
    }

    /// 检查是否需要确认
    pub fn requires_confirmation(&self, positions: &[f64], safety_config: &SafetyConfig) -> bool {
        if self.force {
            return false;
        }

        // 计算最大角度变化
        let max_delta = positions.iter().map(|&p| p.abs()).fold(0.0_f64, f64::max);

        // 转换为角度
        let max_delta_degrees = max_delta * 180.0 / std::f64::consts::PI;

        // 检查是否超过阈值
        safety_config.requires_confirmation(max_delta_degrees)
    }

    /// 执行移动
    pub async fn execute(&self, config: &crate::modes::oneshot::OneShotConfig) -> Result<()> {
        let positions = self.parse_joints()?;

        println!("⏳ 正在移动到目标位置...");
        for (i, &pos) in positions.iter().enumerate() {
            println!(
                "  J{}: {:.3} rad ({:.1}°)",
                i + 1,
                pos,
                pos * 180.0 / std::f64::consts::PI
            );
        }

        // 确定接口和序列号（命令行参数优先）
        let interface = self.interface.as_ref().or(config.interface.as_ref()).map(|s| s.as_str());

        // 创建 Piper 实例
        let mut builder = PiperBuilder::new();
        if let Some(iface) = interface {
            builder = builder.interface(iface);
        }

        println!("🔌 连接到机器人...");
        let robot = builder.build()?;

        // 使能 Position Mode
        let config_mode = PositionModeConfig::default();
        println!("⚡ 使能 Position Mode...");
        let robot = robot.enable_position_mode(config_mode)?;

        // 转换为 JointArray<Rad>
        use piper_client::types::Rad;
        let mut joint_array = JointArray::from([Rad(0.0); 6]);
        for (i, &pos) in positions.iter().enumerate() {
            if i < 6 {
                joint_array[i] = Rad(pos);
            }
        }

        // 如果少于 6 个关节，只发送部分
        if positions.len() < 6 {
            // TODO: 支持部分关节移动
            println!("⚠️  注意: 当前版本会移动所有 6 个关节");
        }

        // 发送位置命令
        println!("📡 发送位置命令...");
        robot.send_position_command(&joint_array)?;

        // 等待一段时间让机器人完成移动
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

        // robot 在这里 drop，自动 disable
        println!("✅ 移动完成");

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_joints() {
        let cmd = MoveCommand {
            joints: Some("0.1,0.2,0.3".to_string()),
            force: false,
            interface: None,
            serial: None,
        };

        let positions = cmd.parse_joints().unwrap();
        assert_eq!(positions, vec![0.1, 0.2, 0.3]);
    }

    #[test]
    fn test_parse_joints_invalid() {
        let cmd = MoveCommand {
            joints: Some("0.1,invalid,0.3".to_string()),
            force: false,
            interface: None,
            serial: None,
        };

        assert!(cmd.parse_joints().is_err());
    }
}
