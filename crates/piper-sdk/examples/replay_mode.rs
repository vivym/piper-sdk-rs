//! 回放模式示例
//!
//! 展示如何使用 ReplayMode 安全地回放预先录制的 CAN 帧。
//!
//! # 使用说明
//!
//! 1. 先录制一个文件（使用 standard_recording 示例）
//! 2. 然后运行此示例进行回放
//!
//! ```bash
//! # Linux (SocketCAN)
//! cargo run -p piper-sdk --example standard_recording -- --interface can0
//! cargo run -p piper-sdk --example replay_mode -- --interface can0
//! cargo run -p piper-sdk --example replay_mode -- --interface can0 --speed 2.0
//!
//! # macOS/Windows (GS-USB serial)
//! cargo run -p piper-sdk --example standard_recording -- --interface ABC123456
//! cargo run -p piper-sdk --example replay_mode -- --interface ABC123456
//! cargo run -p piper-sdk --example replay_mode -- --interface ABC123456 --speed 2.0
//! ```

use anyhow::Result;
use clap::Parser;
use piper_sdk::PiperBuilder;
use piper_sdk::client::state::{MotionCapability, Piper, Standby};
use piper_sdk::client::{MotionConnectedPiper, MotionConnectedState};
use piper_tools::PiperRecording;
use std::fmt::Write;
use std::path::PathBuf;
use std::time::{Duration, UNIX_EPOCH};

#[derive(Debug, Parser)]
struct Args {
    /// Linux: SocketCAN interface; macOS/Windows: GS-USB serial.
    #[cfg_attr(target_os = "linux", arg(long, default_value = "can0"))]
    #[cfg_attr(not(target_os = "linux"), arg(long))]
    interface: String,
    /// Recording file to replay
    #[arg(long, default_value = "demo_recording.bin")]
    recording_file: PathBuf,
    /// Replay speed multiplier
    #[arg(long, default_value_t = 1.0)]
    speed: f64,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("replay_mode=info".parse()?),
        )
        .init();

    let args = Args::parse();

    println!("════════════════════════════════════════");
    println!("           回放模式示例");
    println!("════════════════════════════════════════");
    println!();

    println!("⏳ 连接到机器人...");
    let connected = {
        #[cfg(target_os = "linux")]
        {
            PiperBuilder::new().socketcan(&args.interface).build()?
        }
        #[cfg(not(target_os = "linux"))]
        {
            PiperBuilder::new().gs_usb_serial(&args.interface).build()?
        }
    }
    .require_motion()?;

    match connected {
        MotionConnectedPiper::Strict(MotionConnectedState::Standby(standby)) => {
            run_replay(standby, &args)
        },
        MotionConnectedPiper::Soft(MotionConnectedState::Standby(standby)) => {
            run_replay(standby, &args)
        },
        MotionConnectedPiper::Strict(MotionConnectedState::Maintenance(_))
        | MotionConnectedPiper::Soft(MotionConnectedState::Maintenance(_)) => {
            anyhow::bail!("robot is not in confirmed Standby")
        },
    }
}

fn run_replay<C>(standby: Piper<Standby, C>, args: &Args) -> Result<()>
where
    C: MotionCapability,
{
    let metadata = load_recording_info(&args.recording_file)?;

    println!("✅ 已连接");
    println!();

    println!("📁 录制文件已验证");
    println!("   文件: {}", args.recording_file.display());
    println!("   录制时间: {}", timestamp_to_string(metadata.start_time));
    println!("   接口: {}", metadata.interface);
    println!("   总线速度: {} bps", metadata.bus_speed);
    println!("   操作员: {}", metadata.operator);
    println!("   备注: {}", metadata.notes);
    println!();

    if args.speed > 2.0 {
        println!("⚠️  警告: 速度 {:.1}x 超过推荐值 (2.0x)", args.speed);
        println!();
        println!("请确保:");
        println!("  • 回放环境安全，无人员/障碍物");
        println!("  • 有急停准备");
        println!("  • 机器人状态正常");
        println!();

        println!("按 Enter 继续回放，或输入其他内容取消...");
        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;

        if !input.trim().is_empty() {
            println!("❌ 操作已取消");
            return Ok(());
        }

        println!();
    }

    println!("⏳ 进入回放模式...");
    let replay = standby.enter_replay_mode()?;
    println!("✅ 已进入回放模式（Driver tx_loop 已暂停）");
    println!();

    println!("🔄 开始回放...");
    println!();
    println!("   速度: {:.1}x", args.speed);
    println!();

    let _standby = replay.replay_recording(&args.recording_file, args.speed)?;

    println!();
    println!("✅ 回放完成");
    println!("   已退出回放模式（Driver tx_loop 已恢复）");
    println!();

    Ok(())
}

#[derive(Debug)]
struct RecordingInfo {
    start_time: u64,
    interface: String,
    bus_speed: u32,
    operator: String,
    notes: String,
}

fn load_recording_info(path: &PathBuf) -> Result<RecordingInfo> {
    let recording = PiperRecording::load(path)?;

    Ok(RecordingInfo {
        start_time: recording.metadata.start_time,
        interface: recording.metadata.interface,
        bus_speed: recording.metadata.bus_speed,
        operator: recording.metadata.operator,
        notes: recording.metadata.notes,
    })
}

fn timestamp_to_string(ts: u64) -> String {
    let datetime = UNIX_EPOCH + Duration::from_secs(ts);
    let mut s = String::new();
    let _ = write!(&mut s, "{datetime:?}");
    s
}
