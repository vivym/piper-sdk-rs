//! 自定义诊断示例
//!
//! 展示如何使用诊断接口进行高级 CAN 帧录制和分析
//!
//! # 使用说明
//!
//! ```bash
//! cargo run --example custom_diagnostics -- --interface can0
//! ```

use piper_client::PiperBuilder;
use piper_driver::recording::AsyncRecordingHook;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 初始化日志
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("custom_diagnostics=info"),
        )
        .init();

    println!("════════════════════════════════════════");
    println!("       自定义诊断示例");
    println!("════════════════════════════════════════");
    println!();

    // === 1. 构建并使能 ===

    println!("⏳ 连接到机器人...");
    let robot = PiperBuilder::new()
        .interface("can0")
        .build()?;

    let standby = robot.connect()?;
    println!("✅ 已连接");
    println!();

    // === 2. 使能机器人 ===

    println!("⏳ 使能机器人...");
    let active = standby.enable_position_mode(Default::default())?;
    println!("✅ 已使能");
    println!();

    // === 3. 获取诊断接口 ===

    println!("🔧 获取诊断接口...");
    let diag = active.diagnostics();
    println!("✅ 已获取诊断接口（可独立使用）");
    println!();

    // === 4. 创建自定义录制钩子 ===

    println!("📊 创建自定义 CAN 帧录制...");

    let (hook, rx) = AsyncRecordingHook::new();
    let dropped_counter = hook.dropped_frames().clone();

    // 注册钩子
    let callback = Arc::new(hook) as Arc<dyn piper_driver::FrameCallback>;
    diag.register_callback(callback)?;

    println!("✅ 自定义录制已启动");
    println!();

    // === 5. 在后台线程处理录制的帧 ===

    println!("🔄 启动后台帧处理线程...");
    thread::spawn(move || {
        let mut frame_count = 0;
        let start_time = std::time::Instant::now();

        println!("   后台线程：等待帧...");

        while let Ok(frame) = rx.recv() {
            frame_count += 1;

            // 示例：每收到 1000 帧打印一次
            if frame_count % 1000 == 0 {
                let elapsed = start_time.elapsed().as_secs_f64();
                let fps = frame_count as f64 / elapsed;

                println!("   后台线程：已接收 {} 帧，平均 FPS: {:.1}", frame_count, fps);

                // 示例：分析 CAN ID 分布
                println!("   帧ID: 0x{:03X}", frame.id);
            }
        }

        let elapsed = start_time.elapsed();
        let fps = frame_count as f64 / elapsed.as_secs_f64();

        println!();
        println!("════════════════════════════════════════");
        println!("        后台线程总结");
        println!("════════════════════════════════════════");
        println!();
        println!("📊 总帧数: {}", frame_count);
        println!("⏱️  总时长: {:.2} 秒", elapsed.as_secs_f64());
        println!("📈 平均 FPS: {:.1}", fps);
        println!("💔 丢帧数: {}", dropped_counter.load(std::sync::atomic::Ordering::Relaxed));
    });

    println!("✅ 后台线程已启动");
    println!();

    // === 6. 主线程执行操作 ===

    println!("⏳ 主线程执行 5 秒操作...");
    println!("   （所有 CAN 帧都会被后台线程录制）");
    println!();

    tokio::time::sleep(Duration::from_secs(5)).await;

    // === 7. 优雅关闭 ===

    println!("⏳ 优雅关闭...");
    let _standby = active.shutdown()?;
    println!("✅ 已关闭");

    println!();
    println!("💡 提示：后台线程将继续处理帧，直到通道关闭");

    // 等待后台线程完成
    thread::sleep(Duration::from_secs(1));

    Ok(())
}
