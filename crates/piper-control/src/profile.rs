use piper_driver::ConnectionTarget;
use piper_protocol::control::InstallPosition;
use piper_tools::SafetyConfig;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;
use std::time::Duration;

pub const DEFAULT_PARK_SPEED_PERCENT: u8 = 5;

#[derive(Debug, Clone)]
pub struct ControlProfile {
    pub target: ConnectionTarget,
    pub orientation: ParkOrientation,
    pub rest_pose_override: Option<[f64; 6]>,
    pub park_speed_percent: u8,
    pub safety: SafetyConfig,
    pub wait: MotionWaitConfig,
}

impl ControlProfile {
    pub fn park_pose(&self) -> [f64; 6] {
        self.rest_pose_override.unwrap_or_else(|| self.orientation.default_rest_pose())
    }

    pub fn position_mode_config(&self) -> piper_client::state::PositionModeConfig {
        piper_client::state::PositionModeConfig {
            install_position: self.orientation.install_position(),
            ..piper_client::state::PositionModeConfig::default()
        }
    }

    pub fn park_position_mode_config(
        &self,
    ) -> anyhow::Result<piper_client::state::PositionModeConfig> {
        anyhow::ensure!(
            (1..=100).contains(&self.park_speed_percent),
            "park_speed_percent must be between 1 and 100"
        );

        Ok(piper_client::state::PositionModeConfig {
            speed_percent: self.park_speed_percent,
            install_position: self.orientation.install_position(),
            ..piper_client::state::PositionModeConfig::default()
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct MotionWaitConfig {
    pub threshold_rad: f64,
    pub poll_interval: Duration,
    pub republish_interval: Duration,
    pub timeout: Duration,
}

impl Default for MotionWaitConfig {
    fn default() -> Self {
        Self {
            threshold_rad: 0.02,
            poll_interval: Duration::from_millis(50),
            republish_interval: Duration::from_millis(200),
            timeout: Duration::from_secs(5),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum ParkOrientation {
    #[default]
    Upright,
    Left,
    Right,
}

impl ParkOrientation {
    pub fn install_position(self) -> InstallPosition {
        match self {
            ParkOrientation::Upright => InstallPosition::Horizontal,
            ParkOrientation::Left => InstallPosition::SideLeft,
            ParkOrientation::Right => InstallPosition::SideRight,
        }
    }

    pub fn default_rest_pose(self) -> [f64; 6] {
        match self {
            ParkOrientation::Upright => [0.0, 0.0, 0.0, 0.02, 0.5, 0.0],
            ParkOrientation::Left => [1.71, 2.96, -2.65, 1.41, -0.081, -0.190],
            ParkOrientation::Right => [-1.66, 2.91, -2.74, 0.0545, -0.271, 0.0979],
        }
    }
}

impl fmt::Display for ParkOrientation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParkOrientation::Upright => write!(f, "upright"),
            ParkOrientation::Left => write!(f, "left"),
            ParkOrientation::Right => write!(f, "right"),
        }
    }
}

impl FromStr for ParkOrientation {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "upright" => Ok(Self::Upright),
            "left" => Ok(Self::Left),
            "right" => Ok(Self::Right),
            _ => Err(format!("unsupported orientation: {s}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orientation_maps_to_install_position_and_rest_pose() {
        assert_eq!(
            ParkOrientation::Upright.install_position(),
            InstallPosition::Horizontal
        );
        assert_eq!(
            ParkOrientation::Left.default_rest_pose(),
            [1.71, 2.96, -2.65, 1.41, -0.081, -0.190]
        );
        assert_eq!(
            ParkOrientation::Right.default_rest_pose(),
            [-1.66, 2.91, -2.74, 0.0545, -0.271, 0.0979]
        );
    }

    #[test]
    fn park_position_mode_config_defaults_to_slow_speed() {
        let profile = ControlProfile {
            target: ConnectionTarget::AutoStrict,
            orientation: ParkOrientation::Upright,
            rest_pose_override: None,
            park_speed_percent: 5,
            safety: SafetyConfig::default_config(),
            wait: MotionWaitConfig::default(),
        };

        let config = profile.park_position_mode_config();
        assert_eq!(config.unwrap().speed_percent, 5);
    }

    #[test]
    fn position_mode_config_keeps_normal_speed() {
        let profile = ControlProfile {
            target: ConnectionTarget::AutoStrict,
            orientation: ParkOrientation::Upright,
            rest_pose_override: None,
            park_speed_percent: 5,
            safety: SafetyConfig::default_config(),
            wait: MotionWaitConfig::default(),
        };

        let config = profile.position_mode_config();
        assert_eq!(config.speed_percent, 50);
        assert_eq!(
            config.install_position,
            profile.orientation.install_position()
        );
    }

    #[test]
    fn park_position_mode_config_rejects_zero_speed() {
        let profile = ControlProfile {
            target: ConnectionTarget::AutoStrict,
            orientation: ParkOrientation::Upright,
            rest_pose_override: None,
            park_speed_percent: 0,
            safety: SafetyConfig::default_config(),
            wait: MotionWaitConfig::default(),
        };

        assert!(profile.park_position_mode_config().is_err());
    }

    #[test]
    fn park_position_mode_config_rejects_speed_above_hundred() {
        let profile = ControlProfile {
            target: ConnectionTarget::AutoStrict,
            orientation: ParkOrientation::Upright,
            rest_pose_override: None,
            park_speed_percent: 101,
            safety: SafetyConfig::default_config(),
            wait: MotionWaitConfig::default(),
        };

        assert!(profile.park_position_mode_config().is_err());
    }
}
