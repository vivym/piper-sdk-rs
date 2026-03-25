//! CLI 配置管理

use anyhow::{Context, Result, bail};
use clap::Subcommand;
use piper_control::{ControlProfile, MotionWaitConfig, ParkOrientation, TargetSpec};
use piper_tools::SafetyConfig;
use std::fmt;
use std::fs;
use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

fn config_dir() -> Result<PathBuf> {
    let mut path = dirs::config_dir().ok_or_else(|| anyhow::anyhow!("无法确定配置目录"))?;
    path.push("piper");
    Ok(path)
}

pub fn config_file() -> Result<PathBuf> {
    let mut path = config_dir()?;
    fs::create_dir_all(&path).context("创建配置目录失败")?;
    path.push("config.toml");
    Ok(path)
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CliConfig {
    #[serde(default)]
    pub target: TargetSpec,
    #[serde(default)]
    pub park: ParkConfig,
    #[serde(default)]
    pub safety: CliSafetySettings,
    #[serde(default)]
    pub motion: CliMotionSettings,
}

impl Default for CliConfig {
    fn default() -> Self {
        Self {
            target: TargetSpec::AutoStrict,
            park: ParkConfig::default(),
            safety: CliSafetySettings::default(),
            motion: CliMotionSettings::default(),
        }
    }
}

impl CliConfig {
    pub fn load() -> Result<Self> {
        let path = config_file()?;
        if !path.exists() {
            return Ok(Self::default());
        }

        let content = fs::read_to_string(&path).context("读取配置文件失败")?;
        let value: toml::Value = toml::from_str(&content).context("解析 TOML 配置失败")?;
        if let Some(table) = value.as_table()
            && (table.contains_key("interface") || table.contains_key("serial"))
        {
            bail!(
                "检测到旧版 CLI 配置（interface/serial），已不再兼容；请删除 {} 并重新使用 `piper-cli config set-target <SPEC>` 初始化",
                path.display()
            );
        }

        toml::from_str(&content).context("解析 CLI 配置失败")
    }

    pub fn save(&self) -> Result<()> {
        let path = config_file()?;
        let content = toml::to_string_pretty(self).context("序列化配置为 TOML 失败")?;
        fs::write(&path, content).context("写入配置文件失败")
    }

    pub fn resolved_target_spec(&self, override_target: Option<&TargetSpec>) -> TargetSpec {
        override_target.cloned().unwrap_or_else(|| self.target.clone())
    }

    pub fn control_profile(&self, override_target: Option<&TargetSpec>) -> ControlProfile {
        let mut safety = SafetyConfig::default_config();
        safety.confirmation.enabled = self.safety.confirm_large_motion;
        safety.confirmation.threshold_degrees = self.safety.confirmation_threshold_deg;

        ControlProfile {
            target: self.resolved_target_spec(override_target).into_connection_target(),
            orientation: self.park.orientation,
            rest_pose_override: self.park.rest_pose_override,
            safety,
            wait: self.motion.to_wait_config(),
        }
    }

    fn print_summary(&self) {
        println!("Piper CLI 配置:");
        println!("  target = {}", self.target);
        println!("  orientation = {}", self.park.orientation);
        println!(
            "  rest_pose_override = {}",
            format_optional_pose(self.park.rest_pose_override)
        );
        println!(
            "  confirmation = enabled:{} threshold:{:.1}°",
            self.safety.confirm_large_motion, self.safety.confirmation_threshold_deg
        );
        println!(
            "  motion = threshold:{:.3}rad poll:{}ms republish:{}ms timeout:{}ms",
            self.motion.threshold_rad,
            self.motion.poll_interval_ms,
            self.motion.republish_interval_ms,
            self.motion.timeout_ms
        );
    }
}

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct ParkConfig {
    #[serde(default)]
    pub orientation: ParkOrientation,
    #[serde(default)]
    pub rest_pose_override: Option<[f64; 6]>,
}

impl Default for ParkConfig {
    fn default() -> Self {
        Self {
            orientation: ParkOrientation::Upright,
            rest_pose_override: None,
        }
    }
}

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct CliSafetySettings {
    #[serde(default = "default_confirm_large_motion")]
    pub confirm_large_motion: bool,
    #[serde(default = "default_confirmation_threshold_deg")]
    pub confirmation_threshold_deg: f64,
}

impl Default for CliSafetySettings {
    fn default() -> Self {
        Self {
            confirm_large_motion: default_confirm_large_motion(),
            confirmation_threshold_deg: default_confirmation_threshold_deg(),
        }
    }
}

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct CliMotionSettings {
    #[serde(default = "default_threshold_rad")]
    pub threshold_rad: f64,
    #[serde(default = "default_poll_interval_ms")]
    pub poll_interval_ms: u64,
    #[serde(default = "default_republish_interval_ms")]
    pub republish_interval_ms: u64,
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,
}

impl CliMotionSettings {
    pub fn to_wait_config(self) -> MotionWaitConfig {
        MotionWaitConfig {
            threshold_rad: self.threshold_rad,
            poll_interval: Duration::from_millis(self.poll_interval_ms),
            republish_interval: Duration::from_millis(self.republish_interval_ms),
            timeout: Duration::from_millis(self.timeout_ms),
        }
    }
}

impl Default for CliMotionSettings {
    fn default() -> Self {
        Self {
            threshold_rad: default_threshold_rad(),
            poll_interval_ms: default_poll_interval_ms(),
            republish_interval_ms: default_republish_interval_ms(),
            timeout_ms: default_timeout_ms(),
        }
    }
}

fn default_confirm_large_motion() -> bool {
    true
}

fn default_confirmation_threshold_deg() -> f64 {
    10.0
}

fn default_threshold_rad() -> f64 {
    0.02
}

fn default_poll_interval_ms() -> u64 {
    50
}

fn default_republish_interval_ms() -> u64 {
    200
}

fn default_timeout_ms() -> u64 {
    5_000
}

#[derive(Subcommand, Debug)]
pub enum ConfigCommand {
    SetTarget { spec: TargetSpec },
    Get { key: ConfigKey },
    SetOrientation { orientation: ParkOrientation },
    SetRestPose { pose: String },
    ClearRestPose,
    Check,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigKey {
    Target,
}

impl fmt::Display for ConfigKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigKey::Target => write!(f, "target"),
        }
    }
}

impl FromStr for ConfigKey {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "target" => Ok(Self::Target),
            _ => Err(format!("unsupported config key: {s}")),
        }
    }
}

impl ConfigCommand {
    pub async fn execute(self) -> Result<()> {
        match self {
            ConfigCommand::SetTarget { spec } => Self::set_target(spec).await,
            ConfigCommand::Get { key } => Self::get(key).await,
            ConfigCommand::SetOrientation { orientation } => {
                Self::set_orientation(orientation).await
            },
            ConfigCommand::SetRestPose { pose } => Self::set_rest_pose(pose).await,
            ConfigCommand::ClearRestPose => Self::clear_rest_pose().await,
            ConfigCommand::Check => Self::check().await,
        }
    }

    async fn set_target(spec: TargetSpec) -> Result<()> {
        let mut config = CliConfig::load()?;
        config.target = spec.clone();
        config.save()?;
        println!("✅ 默认连接目标已设置为 {}", spec);
        Ok(())
    }

    async fn get(key: ConfigKey) -> Result<()> {
        let config = CliConfig::load()?;
        match key {
            ConfigKey::Target => println!("{}", config.target),
        }
        Ok(())
    }

    async fn set_orientation(orientation: ParkOrientation) -> Result<()> {
        let mut config = CliConfig::load()?;
        config.park.orientation = orientation;
        config.save()?;
        println!("✅ park orientation 已设置为 {}", orientation);
        Ok(())
    }

    async fn set_rest_pose(pose: String) -> Result<()> {
        let mut config = CliConfig::load()?;
        let rest_pose = parse_pose(&pose)?;
        config.park.rest_pose_override = Some(rest_pose);
        config.save()?;
        println!("✅ park rest pose 已设置为 {}", format_pose(rest_pose));
        Ok(())
    }

    async fn clear_rest_pose() -> Result<()> {
        let mut config = CliConfig::load()?;
        config.park.rest_pose_override = None;
        config.save()?;
        println!("✅ park rest pose override 已清除");
        Ok(())
    }

    async fn check() -> Result<()> {
        let config = CliConfig::load()?;
        let path = config_file()?;
        println!("配置文件: {}", path.display());
        config.print_summary();
        Ok(())
    }
}

pub fn parse_pose(value: &str) -> Result<[f64; 6]> {
    let joints: Vec<f64> = value
        .split(',')
        .map(|part| part.trim().parse::<f64>())
        .collect::<Result<Vec<_>, _>>()
        .context("解析关节位姿失败")?;

    if joints.len() != 6 {
        bail!("需要 6 个关节值，得到 {}", joints.len());
    }
    if joints.iter().any(|joint| !joint.is_finite()) {
        bail!("rest pose 中包含非法数值");
    }

    Ok([
        joints[0], joints[1], joints[2], joints[3], joints[4], joints[5],
    ])
}

fn format_pose(pose: [f64; 6]) -> String {
    pose.map(|value| format!("{value:.4}")).join(",")
}

fn format_optional_pose(pose: Option<[f64; 6]>) -> String {
    pose.map(format_pose).unwrap_or_else(|| "(unset)".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_auto_target() {
        let config = CliConfig::default();
        assert_eq!(config.target, TargetSpec::AutoStrict);
        assert_eq!(config.park.orientation, ParkOrientation::Upright);
        assert!(config.park.rest_pose_override.is_none());
    }

    #[test]
    fn control_profile_uses_override_target() {
        let config = CliConfig::default();
        let profile = config.control_profile(Some(&TargetSpec::SocketCan {
            iface: "vcan0".to_string(),
        }));
        assert!(matches!(
            profile.target,
            piper_sdk::driver::ConnectionTarget::SocketCan { ref iface } if iface == "vcan0"
        ));
        assert_eq!(profile.orientation, ParkOrientation::Upright);
    }

    #[test]
    fn parse_rest_pose_requires_six_values() {
        assert!(parse_pose("0,1,2").is_err());
        assert_eq!(
            parse_pose("0,1,2,3,4,5").unwrap(),
            [0.0, 1.0, 2.0, 3.0, 4.0, 5.0]
        );
    }
}
