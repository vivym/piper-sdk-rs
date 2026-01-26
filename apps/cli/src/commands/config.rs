//! 配置管理命令
//!
//! 用于管理 CLI 配置（接口名称等）

use anyhow::{Context, Result};
use clap::Subcommand;
use std::fs;
use std::path::PathBuf;

/// 配置文件路径
fn config_dir() -> Result<PathBuf> {
    let mut path = dirs::config_dir().ok_or_else(|| anyhow::anyhow!("无法确定配置目录"))?;

    path.push("piper");
    Ok(path)
}

fn config_file() -> Result<PathBuf> {
    let mut path = config_dir()?;
    fs::create_dir_all(&path).context("创建配置目录失败")?;

    path.push("config.toml");
    Ok(path)
}

/// CLI 配置
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
struct CliConfig {
    /// 默认 CAN 接口
    interface: Option<String>,

    /// 设备序列号（GS-USB）
    serial: Option<String>,
}

impl CliConfig {
    /// 加载配置
    fn load() -> Result<Self> {
        let path = config_file()?;

        if !path.exists() {
            return Ok(Self::default());
        }

        let _content = fs::read_to_string(&path).context("读取配置文件失败")?;

        // ⚠️ 简化实现：实际应该使用 TOML 解析
        // 这里暂时返回默认配置
        Ok(Self::default())
    }

    /// 保存配置
    fn save(&self) -> Result<()> {
        let path = config_file()?;

        // ⚠️ 简化实现：实际应该序列化为 TOML
        let content = format!(
            r#"# Piper CLI Configuration

[default]
interface = {:?}
serial = {:?}
"#,
            self.interface, self.serial
        );

        fs::write(&path, content).context("写入配置文件失败")?;

        Ok(())
    }
}

/// 配置命令
#[derive(Subcommand, Debug)]
pub enum ConfigCommand {
    /// 设置配置项
    Set {
        /// CAN 接口名称（如 can0, gs-usb）
        #[arg(short, long)]
        interface: Option<String>,

        /// 设备序列号（GS-USB）
        #[arg(short, long)]
        serial: Option<String>,
    },

    /// 获取配置项
    Get {
        /// 配置项名称
        #[arg(default_value = "all")]
        key: String,
    },

    /// 检查配置
    Check,
}

impl ConfigCommand {
    pub async fn execute(self) -> Result<()> {
        match self {
            ConfigCommand::Set { interface, serial } => Self::set_(interface, serial).await,

            ConfigCommand::Get { key } => Self::get_(key).await,

            ConfigCommand::Check => Self::check_().await,
        }
    }

    async fn set_(interface: Option<String>, serial: Option<String>) -> Result<()> {
        let mut config = CliConfig::load()?;

        if let Some(ref iface) = interface {
            config.interface = Some(iface.clone());
            println!("✅ 设置默认接口: {}", iface);
        }

        if let Some(ref s) = serial {
            config.serial = Some(s.clone());
            println!("✅ 设置设备序列号: {}", s);
        }

        config.save()?;
        Ok(())
    }

    async fn get_(key: String) -> Result<()> {
        let config = CliConfig::load()?;

        match key.as_str() {
            "interface" => {
                if let Some(ref iface) = config.interface {
                    println!("{}", iface);
                } else {
                    println!("(未设置)");
                }
            },

            "serial" => {
                if let Some(ref serial) = config.serial {
                    println!("{}", serial);
                } else {
                    println!("(未设置)");
                }
            },

            _ => {
                println!("Piper CLI 配置:");
                println!("  接口: {:?}", config.interface);
                println!("  序列号: {:?}", config.serial);
            },
        }

        Ok(())
    }

    async fn check_() -> Result<()> {
        let config = CliConfig::load()?;
        let path = config_file()?;

        println!("配置文件: {}", path.display());
        println!("  接口: {:?}", config.interface);
        println!("  序列号: {:?}", config.serial);

        Ok(())
    }
}
