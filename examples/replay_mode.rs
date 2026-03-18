//! 回放模式示例
//!
//! 展示如何使用 ReplayMode 安全地回放预先录制的 CAN 帧
//!
//! # 使用说明
//!
//! 1. 先录制一个文件（使用 standard_recording 示例）
//! 2. 然后运行此示例进行回放
//!
//! ```bash
//! # 步骤 1: 录制
//! cargo run --example standard_recording -- --interface can0
//!
//! # 步骤 2: 回放（原始速度）
//! cargo run --example replay_mode -- --interface can0
//!
//! # 步骤 2: 回放（快速 2.0x）
//! cargo run --example replay_mode -- --interface can0 --speed 2.0
//! ```

use piper_client::PiperBuilder;
use std::path::PathBuf;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 初始化日志
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("replay_mode=info"),
        )
        .init();

    println!("════════════════════════════════════════");
    println!("           回放模式示例");
    println!("════════════════════════════════════════");
    println!();

    // === 解析命令行参数 ===

    let args = parse_args()?;

    // === 1. 构建并连接 ===

    println!("⏳ 连接到机器人...");
    let standby = PiperBuilder::new()
        .socketcan(&args.interface)
        .build()?;
    println!("✅ 已连接");
    println!();

    // === 2. 安全检查 ===

    if args.speed > 2.0 {
        println!("⚠️  警告: 速度 {:.1}x 超过推荐值 (2.0x)", args.speed);
        println!();
        println!("请确保:");
        println!("  • 回放环境安全，无人员/障碍物");
        println!("  • 有急停准备");
        println!("  • 机器人状态正常");
        println!();

        println!("按 Enter 继续回放，或其他键取消...");
        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;

        if !input.trim().is_empty() && !input.trim().to_lowercase().starts_with('y') {
            println!("❌ 操作已取消");
            return Ok(());
        }

        println!();
    }

    // === 3. 进入回放模式 ===

    println!("⏳ 进入回放模式...");
    let replay = standby.enter_replay_mode()?;
    println!("✅ 已进入回放模式（Driver tx_loop 已暂停）");
    println!();

    // === 4. 回放录制 ===

    println!("🔄 开始回放...");
    println!();
    println!("   文件: {}", args.recording_file);
    println!("   速度: {:.1}x", args.speed);
    println!();

    let standby = replay.replay_recording(&args.recording_file, args.speed)?;

    // === 5. 完成 ===

    println!();
    println!("✅ 回放完成");
    println!("   已退出回放模式（Driver tx_loop 已恢复）");
    println!();

    // === 6. 显示文件信息 ===

    if let Some(metadata) = get_recording_info(&args.recording_file)? {
        println!("════════════════════════════════════════");
        println!("           录制文件信息");
        println!("════════════════════════════════════════");
        println!();
        println!("📁 文件: {}", args.recording_file);
        println!("📅 录制时间: {}", timestamp_to_string(metadata.start_time));
        println!("🔌 接口: {}", metadata.interface);
        println!("🚀 总线速度: {} bps", metadata.bus_speed);
        println!("📝 备注: {}", metadata.notes);
        println!();
    }

    // 任何连接都会在这里自动 Drop 并断开
    Ok(())
}

/// 命令行参数
struct Args {
    interface: String,
    recording_file: String,
    speed: f64,
}

fn parse_args() -> Args {
    let interface = std::env::var("PIPER_INTERFACE")
        .unwrap_or_else(|_| "can0".to_string());

    let recording_file = std::env::var("PIPER_RECORDING")
        .unwrap_or_else(|_| "demo_recording.bin".to_string());

    let speed_str = std::env::var("PIPER_SPEED")
        .unwrap_or_else(|_| "1.0".to_string());

    let speed: f64 = speed_str
        .parse()
        .unwrap_or(1.0);

    Args {
        interface,
        recording_file,
        speed,
    }
}

/// 录制元数据（简化版，用于显示）
#[derive(Debug)]
struct RecordingInfo {
    start_time: u64,
    interface: String,
    bus_speed: u32,
    notes: String,
}

fn get_recording_info(path: &str) -> anyhow::Result<Option<RecordingInfo>> {
    use piper_tools::PiperRecording;

    let path = std::path::Path::new(path);
    if !path.exists() {
        return Ok(None);
    }

    let recording = PiperRecording::load(path)?;

    Ok(Some(RecordingInfo {
        start_time: recording.metadata.start_time,
        interface: recording.metadata.interface,
        bus_speed: recording.metadata.bus_speed,
        notes: recording.metadata.notes,
    }))
}

fn timestamp_to_string(ts: u64) -> String {
    use std::time::{UNIX_EPOCH, Duration};

    let datetime = UNIX_EPOCH + Duration::from_secs(ts);
    use std::fmt::Write;

    let mut s = String::new();
    write!(&mut s, "{:?}", datetime).unwrap();
    s
}
