//! # Piper CLI
//!
//! Command-line interface for Piper robot arm control.
//!
//! ## 双模式架构
//!
//! ### One-shot 模式（推荐用于 CI/脚本）
//!
//! ```bash
//! # 配置默认连接目标
//! piper-cli config set-target socketcan:can0
//!
//! # 执行操作（内部：连接 -> 移动 -> 断开）
//! piper-cli move --joints 0.1,0.2,0.3,0.4,0.5,0.6
//! ```
//!
//! ### REPL 模式（推荐用于调试）
//!
//! ```bash
//! $ piper-cli shell
//! piper> connect socketcan:can0
//! piper> enable
//! piper> move --joints 0.1,0.2,0.3,0.4,0.5,0.6
//! piper> stop
//! piper> exit
//! ```

use anyhow::Result;
use clap::{Parser, Subcommand};

mod commands;
mod connection;
mod modes;
mod parsing;
mod safety;
mod script;
mod utils;
mod validation;

use commands::config::CliConfig;
use commands::{
    CollisionProtectionCommand, ConfigCommand, HomeCommand, MoveCommand, ParkCommand,
    PositionCommand, RecordCommand, ReplayCommand, RunCommand, SetZeroCommand, StopCommand,
};
use connection::TargetArgs;
use modes::oneshot::OneShotMode;
use modes::repl::run_repl;

/// Piper CLI - 机器人臂命令行工具
#[derive(Parser, Debug)]
#[command(name = "piper-cli")]
#[command(about = "Command-line interface for Piper robot arm control", long_about = None)]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// 配置管理
    #[command(subcommand)]
    Config(ConfigCommand),

    /// 移动关节到目标位置
    Move {
        #[command(flatten)]
        args: MoveCommand,
    },

    /// 查询当前关节位置
    Position {
        #[command(flatten)]
        args: PositionCommand,
    },

    /// 急停
    Stop {
        #[command(flatten)]
        args: StopCommand,
    },

    /// 启动交互式 Shell（REPL 模式）
    Shell,

    /// 回到零位
    Home {
        #[command(flatten)]
        args: HomeCommand,
    },

    /// 前往安全停靠位
    Park {
        #[command(flatten)]
        args: ParkCommand,
    },

    /// 将当前位置写入关节零点
    SetZero {
        #[command(flatten)]
        args: SetZeroCommand,
    },

    /// 读取或设置碰撞保护等级
    CollisionProtection {
        #[command(flatten)]
        args: CollisionProtectionCommand,
    },

    /// 监控机器人状态
    Monitor {
        /// 更新频率（Hz）
        #[arg(short, long, default_value_t = 10)]
        frequency: u32,

        #[command(flatten)]
        target: TargetArgs,
    },

    /// 录制 CAN 总线数据
    Record {
        #[command(flatten)]
        args: RecordCommand,
    },

    /// 执行脚本
    Run {
        #[command(flatten)]
        args: RunCommand,
    },

    /// 回放录制
    Replay {
        #[command(flatten)]
        args: ReplayCommand,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    // 初始化日志（compact 格式，隐藏 target，易读）
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("piper_cli=info".parse().unwrap())
                .add_directive("piper_driver=warn".parse().unwrap())
                .add_directive("piper_can=warn".parse().unwrap())
                .add_directive("piper_protocol=warn".parse().unwrap()),
        )
        .with_target(false)
        .compact()
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Config(cmd) => {
            // One-shot 模式：配置管理
            cmd.execute().await
        },

        Commands::Move { args } => {
            let config = CliConfig::load()?;
            args.execute(&config).await
        },

        Commands::Position { args } => {
            let config = CliConfig::load()?;
            args.execute(&config).await
        },

        Commands::Stop { args } => {
            let config = CliConfig::load()?;
            args.execute(&config).await
        },

        Commands::Home { args } => {
            let config = CliConfig::load()?;
            args.execute(&config).await
        },

        Commands::Park { args } => {
            let config = CliConfig::load()?;
            args.execute(&config).await
        },

        Commands::SetZero { args } => {
            let config = CliConfig::load()?;
            args.execute(&config).await
        },

        Commands::CollisionProtection { args } => {
            let config = CliConfig::load()?;
            args.execute(&config).await
        },

        Commands::Monitor { frequency, target } => {
            let mut mode = OneShotMode::new().await?;
            mode.monitor(frequency, target.target.as_ref()).await?;
            Ok(())
        },

        Commands::Record { args } => args.execute().await,

        Commands::Run { args } => {
            // One-shot 模式：执行脚本
            args.execute().await?;
            Ok(())
        },

        Commands::Replay { args } => {
            // One-shot 模式：回放
            args.execute().await?;
            Ok(())
        },

        Commands::Shell => {
            // REPL 模式：交互式 Shell
            run_repl().await
        },
    }
}
