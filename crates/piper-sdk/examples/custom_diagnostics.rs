//! 自定义诊断示例
//!
//! 展示如何使用诊断接口进行高级 CAN 帧录制和分析。
//!
//! # 使用说明
//!
//! ```bash
//! # Linux (SocketCAN)
//! cargo run -p piper-sdk --example custom_diagnostics -- --interface can0 --seconds 5
//!
//! # macOS/Windows (GS-USB serial)
//! cargo run -p piper-sdk --example custom_diagnostics -- --interface ABC123456 --seconds 5
//! ```

use anyhow::Result;
use clap::Parser;
use crossbeam_channel::Receiver;
use piper_sdk::PiperBuilder;
use piper_sdk::client::state::{MotionCapability, Piper, Standby};
use piper_sdk::client::{MotionConnectedPiper, MotionConnectedState};
use piper_sdk::driver::{AsyncRecordingHook, FrameCallback, TimestampedFrame};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::Duration;

#[derive(Debug, Parser)]
struct Args {
    /// Linux: SocketCAN interface; macOS/Windows: GS-USB serial.
    #[cfg_attr(target_os = "linux", arg(long, default_value = "can0"))]
    #[cfg_attr(not(target_os = "linux"), arg(long))]
    interface: String,
    /// How long the main thread should keep the robot active
    #[arg(long, default_value_t = 5)]
    seconds: u64,
}

#[derive(Debug)]
struct DiagnosticsSummary {
    frame_count: u64,
    dropped_frames: u64,
    duration: Duration,
}

fn drain_recorded_frames(
    rx: Receiver<TimestampedFrame>,
    dropped_counter: Arc<AtomicU64>,
) -> DiagnosticsSummary {
    let mut frame_count = 0;
    let start_time = std::time::Instant::now();

    println!("   后台线程：等待帧...");

    while let Ok(frame) = rx.recv() {
        frame_count += 1;

        if frame_count % 1000 == 0 {
            let elapsed = start_time.elapsed().as_secs_f64();
            let fps = frame_count as f64 / elapsed;

            println!(
                "   后台线程：已接收 {} 帧，平均 FPS: {:.1}",
                frame_count, fps
            );
            println!("   帧ID: 0x{:03X}", frame.id);
        }
    }

    DiagnosticsSummary {
        frame_count,
        dropped_frames: dropped_counter.load(Ordering::Relaxed),
        duration: start_time.elapsed(),
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("custom_diagnostics=info".parse()?),
        )
        .init();

    let args = Args::parse();

    println!("════════════════════════════════════════");
    println!("       自定义诊断示例");
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
            run_diagnostics(standby, &args).await
        },
        MotionConnectedPiper::Soft(MotionConnectedState::Standby(standby)) => {
            run_diagnostics(standby, &args).await
        },
        MotionConnectedPiper::Strict(MotionConnectedState::Maintenance(_))
        | MotionConnectedPiper::Soft(MotionConnectedState::Maintenance(_)) => {
            anyhow::bail!("robot is not in confirmed Standby")
        },
    }
}

async fn run_diagnostics<C>(standby: Piper<Standby, C>, args: &Args) -> Result<()>
where
    C: MotionCapability,
{
    println!("✅ 已连接");
    println!();

    println!("⏳ 使能机器人...");
    let active = standby.enable_position_mode(Default::default())?;
    println!("✅ 已使能");
    println!();

    println!("🔧 获取诊断接口...");
    let diag = active.diagnostics();
    println!("✅ 已获取诊断接口（可独立使用）");
    println!();

    println!("📊 创建自定义 CAN 帧录制...");

    let (hook, rx) = AsyncRecordingHook::new();
    let dropped_counter = hook.dropped_frames().clone();

    let callback = Arc::new(hook) as Arc<dyn FrameCallback>;
    let hook_handle = diag.register_callback(callback)?;

    println!("✅ 自定义录制已启动");
    println!();

    println!("🔄 启动后台帧处理线程...");
    let worker = thread::spawn(move || drain_recorded_frames(rx, dropped_counter));

    println!("✅ 后台线程已启动");
    println!();

    println!("⏳ 主线程执行 {} 秒操作...", args.seconds);
    println!("   （所有 CAN 帧都会被后台线程录制）");
    println!();

    tokio::time::sleep(Duration::from_secs(args.seconds)).await;

    println!("🧹 注销诊断回调...");
    let removed = diag.unregister_callback(hook_handle)?;
    println!("✅ 回调已注销: {}", removed);

    println!("⏳ 优雅关闭...");
    let _standby = active.shutdown()?;
    println!("✅ 已关闭");

    let summary = worker.join().map_err(|_| anyhow::anyhow!("diagnostics worker panicked"))?;

    let fps = if summary.duration.is_zero() {
        0.0
    } else {
        summary.frame_count as f64 / summary.duration.as_secs_f64()
    };

    println!();
    println!("════════════════════════════════════════");
    println!("        后台线程总结");
    println!("════════════════════════════════════════");
    println!();
    println!("📊 总帧数: {}", summary.frame_count);
    println!("⏱️  总时长: {:.2} 秒", summary.duration.as_secs_f64());
    println!("📈 平均 FPS: {:.1}", fps);
    println!("💔 丢帧数: {}", summary.dropped_frames);

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossbeam_channel::bounded;
    use piper_sdk::driver::TimestampedFrame;
    use std::sync::atomic::{AtomicU64, Ordering};

    #[test]
    fn drain_recorded_frames_reports_summary_after_channel_closes() {
        let (tx, rx) = bounded(4);
        let dropped_counter = Arc::new(AtomicU64::new(3));

        tx.send(TimestampedFrame {
            timestamp_us: 42,
            id: 0x251,
            data: vec![1, 2, 3, 4],
        })
        .unwrap();
        tx.send(TimestampedFrame {
            timestamp_us: 43,
            id: 0x252,
            data: vec![5, 6, 7, 8],
        })
        .unwrap();
        drop(tx);

        let summary = drain_recorded_frames(rx, dropped_counter.clone());

        assert_eq!(summary.frame_count, 2);
        assert_eq!(summary.dropped_frames, 3);
        assert!(summary.duration.as_secs_f64() >= 0.0);
        assert_eq!(dropped_counter.load(Ordering::Relaxed), 3);
    }
}
