//! 标准录制示例
//!
//! 展示如何使用标准录制 API 记录 CAN 总线数据
//!
//! # 使用说明
//!
//! ```bash
//! cargo run --example standard_recording -- --interface can0
//! ```

use piper_client::{PiperBuilder, recording::{RecordingConfig, RecordingMetadata, StopCondition}};
use std::time::Duration;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 初始化日志
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("standard_recording=info"),
        )
        .init();

    println!("════════════════════════════════════════");
    println!("       标准录制示例");
    println!("════════════════════════════════════════");
    println!();

    // === 1. 构建并连接 ===

    println!("⏳ 连接到机器人...");
    let robot = PiperBuilder::new()
        .interface("can0")
        .build()?;

    println!("✅ 已连接");
    println!();

    // === 2. 启动录制 ===

    println!("📼 启动录制...");

    let (robot, handle) = robot.start_recording(RecordingConfig {
        output_path: "demo_recording.bin".into(),
        stop_condition: StopCondition::Duration(10), // 录制 10 秒
        metadata: RecordingMetadata {
            notes: "标准录制示例".to_string(),
            operator: "DemoUser".to_string(),
        },
    })?;

    println!("✅ 录制已启动，开始执行操作...");
    println!();

    // === 3. 执行一些操作（会被录制）===

    println!("⏳ 等待 10 秒...");
    println!("   （在此期间的所有 CAN 帧都会被录制）");
    println!();

    tokio::time::sleep(Duration::from_secs(10)).await;

    // === 4. 停止录制并保存 ===

    println!("🛑 停止录制...");
    let (robot, stats) = robot.stop_recording(handle)?;

    println!("✅ 录制已保存");
    println!();

    // === 5. 显示统计信息 ===

    println!("════════════════════════════════════════");
    println!("           录制统计");
    println!("════════════════════════════════════════");
    println!();
    println!("📊 帧数: {}", stats.frame_count);
    println!("⏱️  时长: {:.2} 秒", stats.duration.as_secs_f64());
    println!("📉 丢帧: {}", stats.dropped_frames);
    println!("💾 文件: {:?}", stats.output_path);
    println!();

    // 任何连接都会在这里自动 Drop 并断开
    Ok(())
}
