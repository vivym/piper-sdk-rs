//! 标准录制示例
//!
//! 展示如何使用标准录制 API 记录 CAN 总线数据。
//!
//! # 使用说明
//!
//! ```bash
//! # Linux (SocketCAN)
//! cargo run -p piper-sdk --example standard_recording -- --interface can0
//!
//! # macOS/Windows (GS-USB serial)
//! cargo run -p piper-sdk --example standard_recording -- --interface ABC123456
//! ```

use anyhow::Result;
use clap::Parser;
use piper_sdk::{
    ConnectedPiper, PiperBuilder, RecordingConfig, RecordingMetadata, StopCondition,
    client::state::{CapabilityMarker, Piper, Standby},
};
use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, Parser)]
struct Args {
    /// Linux: SocketCAN interface; macOS/Windows: GS-USB serial.
    #[cfg_attr(target_os = "linux", arg(long, default_value = "can0"))]
    #[cfg_attr(not(target_os = "linux"), arg(long))]
    interface: String,
    /// Output recording file path
    #[arg(long, default_value = "demo_recording.bin")]
    output: PathBuf,
    /// Recording duration in seconds
    #[arg(long, default_value_t = 10)]
    duration: u64,
    /// Metadata operator name
    #[arg(long, default_value = "DemoUser")]
    operator: String,
    /// Metadata notes
    #[arg(long, default_value = "标准录制示例")]
    notes: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("standard_recording=info".parse()?),
        )
        .init();

    let args = Args::parse();

    println!("════════════════════════════════════════");
    println!("       标准录制示例");
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
    };

    match connected {
        ConnectedPiper::Strict(state) => run_recording(state.require_standby()?, &args).await,
        ConnectedPiper::Soft(state) => run_recording(state.require_standby()?, &args).await,
        ConnectedPiper::Monitor(standby) => run_recording(standby, &args).await,
    }
}

async fn run_recording<C>(robot: Piper<Standby, C>, args: &Args) -> Result<()>
where
    C: CapabilityMarker,
{
    println!("✅ 已连接");
    println!();

    println!("📼 启动录制...");

    let (robot, handle) = robot.start_recording(RecordingConfig {
        output_path: args.output.clone(),
        stop_condition: StopCondition::Duration(args.duration),
        metadata: RecordingMetadata {
            notes: args.notes.clone(),
            operator: args.operator.clone(),
        },
    })?;

    println!("✅ 录制已启动，开始执行操作...");
    println!();

    println!("⏳ 等待 {} 秒...", args.duration);
    println!("   （在此期间的所有 CAN 帧都会被录制）");
    println!();

    tokio::time::sleep(Duration::from_secs(args.duration)).await;

    println!("🛑 停止录制...");
    let (_robot, stats) = robot.stop_recording(handle)?;

    println!("✅ 录制已保存");
    println!();

    println!("════════════════════════════════════════");
    println!("           录制统计");
    println!("════════════════════════════════════════");
    println!();
    println!("📊 帧数: {}", stats.frame_count);
    println!("⏱️  时长: {:.2} 秒", stats.duration.as_secs_f64());
    println!("📉 丢帧: {}", stats.dropped_frames);
    println!("💾 文件: {:?}", stats.output_path);
    println!();

    Ok(())
}
