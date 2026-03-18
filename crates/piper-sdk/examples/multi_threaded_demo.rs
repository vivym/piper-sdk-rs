//! 多线程控制演示
//!
//! 演示如何在多线程环境下安全地控制机械臂。
//! 由于 Type State Pattern 的设计，不能再"提取" MotionCommander 传递给其他线程。
//! 正确的做法是使用 Arc<Mutex<Piper>> 来共享机器人实例。

use clap::Parser;
use piper_sdk::client::state::MitModeConfig;
use piper_sdk::client::types::{JointArray, NewtonMeter, Rad};
use piper_sdk::prelude::*;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

/// 命令行参数
#[derive(Parser, Debug)]
#[command(name = "multi_threaded_demo")]
#[command(about = "多线程控制演示 - 展示如何在多线程环境下安全地共享 Piper 实例")]
struct Args {
    /// CAN 接口名称或设备序列号
    #[arg(long, default_value = "can0")]
    interface: String,

    /// CAN 波特率（默认: 1000000）
    #[arg(long, default_value = "1000000")]
    baud_rate: u32,

    /// 控制频率（Hz，默认: 100）
    #[arg(long, default_value = "100")]
    frequency_hz: f64,

    /// 控制时长（秒，默认: 5）
    #[arg(long, default_value = "5")]
    duration_sec: u64,
}

fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    // 初始化日志
    piper_sdk::init_logger!();

    let args = Args::parse();

    println!("🤖 Piper SDK - 多线程控制演示");
    println!("=========================\n");

    // ==================== 步骤 1: 连接并使能机械臂 ====================
    println!("📡 步骤 1: 连接并使能机械臂...");

    let robot = {
        #[cfg(target_os = "linux")]
        {
            PiperBuilder::new()
                .socketcan(&args.interface)
                .baud_rate(args.baud_rate)
                .build()?
        }
        #[cfg(not(target_os = "linux"))]
        {
            PiperBuilder::new()
                .gs_usb_serial(&args.interface)
                .baud_rate(args.baud_rate)
                .build()?
        }
    };
    let robot = robot.enable_mit_mode(MitModeConfig::default())?;
    println!("   ✅ 使能成功\n");

    // ✅ 使用 Arc<Mutex<>> 共享机器人实例
    let robot = Arc::new(Mutex::new(robot));
    println!("🔒 机器人已包装在 Arc<Mutex<>> 中，可安全跨线程共享\n");

    // ==================== 步骤 2: 启动控制线程 ====================
    println!(
        "⚙️  步骤 2: 启动控制线程 ({} Hz，{} 秒)...",
        args.frequency_hz, args.duration_sec
    );

    let robot_clone = Arc::clone(&robot);
    let control_thread = thread::spawn(move || {
        let period = Duration::from_secs_f64(1.0 / args.frequency_hz);
        let start_time = Instant::now();
        let mut iteration = 0;

        println!("   📝 控制线程已启动");

        loop {
            // 计算目标位置（简单的正弦波运动）
            let elapsed = start_time.elapsed().as_secs_f64();
            let amplitude = 0.2;
            let frequency = 0.5;
            let phase = 2.0 * std::f64::consts::PI * frequency * elapsed;
            let j1_target = amplitude * phase.sin();

            // 准备所有关节位置（其他关节保持为 0）
            let positions = JointArray::from([
                Rad(j1_target),
                Rad(0.0),
                Rad(0.0),
                Rad(0.0),
                Rad(0.0),
                Rad(0.0),
            ]);

            // ✅ 获取锁并发送命令
            if let Ok(robot) = robot_clone.lock() {
                let velocities = JointArray::from([0.0; 6]);
                let kp = JointArray::from([0.0; 6]);
                let kd = JointArray::from([0.0; 6]);
                let torques = JointArray::from([NewtonMeter(0.0); 6]);

                if let Err(e) = robot.command_torques(&positions, &velocities, &kp, &kd, &torques) {
                    eprintln!("   ❌ 发送命令失败: {:?}", e);
                    break;
                }
            } else {
                // 获取锁失败（不应该发生）
                eprintln!("   ❌ 获取锁失败");
                break;
            }

            // 检查是否超时
            if elapsed >= args.duration_sec as f64 {
                println!("   📝 控制线程结束，总迭代次数: {}", iteration);
                break;
            }

            iteration += 1;

            // 休眠到下一个周期
            std::thread::sleep(period);
        }
    });

    // ==================== 步骤 3: 主线程监控状态 ====================
    println!("📊 步骤 3: 主线程监控机械臂状态...\n");

    let monitor_start = Instant::now();
    let mut sample_count = 0;

    while monitor_start.elapsed() < Duration::from_secs(args.duration_sec) {
        // 克隆 Observer 用于只读监控（不需要锁）
        let observer = {
            let robot = robot.lock().unwrap();
            robot.observer().clone()
        };

        let positions = observer.joint_positions();
        sample_count += 1;

        // 每秒输出一次状态
        if sample_count % (args.frequency_hz as u32) == 0 {
            println!(
                "   📍 J1 = {:.4} rad ({:.2} deg) - 样本 #{:04}",
                positions[Joint::J1].0,
                positions[Joint::J1].to_deg().0,
                sample_count
            );
        }

        std::thread::sleep(Duration::from_millis(10));
    }

    // 等待控制线程完成
    control_thread.join().unwrap();
    println!("\n   ✅ 控制线程已结束\n");

    // ==================== 步骤 4: 失能机械臂 ====================
    println!("🛑 步骤 4: 失能机械臂...");

    // 从 Arc 中获取所有权
    let _robot = robot.lock().unwrap();
    // 注意：不需要 disable，因为 MutexGuard 会释放

    println!("   ✅ 演示完成！");
    println!("\n💡 关键要点：");
    println!("   1. 使用 Arc<Mutex<Piper>> 而非提取 MotionCommander");
    println!("   2. 每次发送命令时获取锁，发送后立即释放");
    println!("   3. Observer 可以 clone 用于只读监控（不需要锁）");
    println!("   4. 这种模式保证了 Type State 安全性");

    Ok(())
}
