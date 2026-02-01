//! 简单移动示例 - High-Level API 快速入门
//!
//! 这个示例展示了如何使用 Piper SDK 的高级 API 进行简单的关节移动。
//!
//! # 运行
//!
//! ```bash
//! cargo run --example high_level_simple_move
//! ```

use piper_sdk::client::control::TrajectoryPlanner;
use piper_sdk::client::types::{JointArray, Rad};
use std::time::Duration;

fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    // 初始化日志
    piper_sdk::init_logger!();

    println!("🚀 Piper SDK - Simple Move Example");
    println!("===================================\n");

    // 注意：这是一个演示示例，实际使用需要连接真实硬件
    // 当前版本展示 API 使用方式

    // 1. 定义起始和目标位置
    let start_positions = JointArray::from([
        Rad(0.0), // J1
        Rad(0.0), // J2
        Rad(0.0), // J3
        Rad(0.0), // J4
        Rad(0.0), // J5
        Rad(0.0), // J6
    ]);

    let target_positions = JointArray::from([
        Rad(0.5),  // J1 - 向右旋转 0.5 弧度
        Rad(1.0),  // J2 - 向上抬起 1.0 弧度
        Rad(0.3),  // J3
        Rad(-0.5), // J4
        Rad(0.0),  // J5
        Rad(0.2),  // J6
    ]);

    println!("📍 起始位置: {:?}", start_positions[0]);
    println!("🎯 目标位置: {:?}", target_positions[0]);
    println!();

    // 2. 创建轨迹规划器
    let duration = Duration::from_secs(5); // 5 秒完成运动
    let frequency_hz = 100.0; // 100Hz 采样频率

    let mut planner =
        TrajectoryPlanner::new(start_positions, target_positions, duration, frequency_hz);

    println!("📈 轨迹规划:");
    println!("   - 持续时间: {:?}", duration);
    println!("   - 采样频率: {} Hz", frequency_hz);
    println!("   - 总采样点: {}", planner.total_samples());
    println!();

    // 3. 执行轨迹（模拟）
    println!("▶️  执行轨迹...\n");

    let total_samples = planner.total_samples();
    let mut step_count = 0;

    for (position, velocity) in &mut planner {
        step_count += 1;

        // 每 20 步打印一次进度
        if step_count % 20 == 0 {
            let progress = (step_count as f64) / (total_samples as f64) * 100.0;
            println!(
                "   Step {}/{}: 进度 {:.1}% | J1 位置: {:.3} rad | J1 速度: {:.3} rad/s",
                step_count, total_samples, progress, position[0].0, velocity[0]
            );
        }

        // 在实际应用中，这里会发送命令到机器人：
        // piper.motion_commander().command_positions(position)?;

        // 模拟控制周期延迟
        std::thread::sleep(Duration::from_millis(10));
    }

    println!("\n✅ 轨迹执行完成！");
    println!("   总步数: {}", step_count);
    println!();

    // 4. 展示 API 特性
    println!("💡 API 特性:");
    println!("   ✨ 强类型单位 (Rad, Deg, NewtonMeter)");
    println!("   ✨ Iterator 模式 (内存高效)");
    println!("   ✨ 平滑轨迹 (三次样条插值)");
    println!("   ✨ 类型安全 (编译期保证)");
    println!();

    println!("🎉 示例完成！");

    Ok(())
}
