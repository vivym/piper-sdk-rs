//! 轨迹规划演示 - 展示 TrajectoryPlanner 的高级特性
//!
//! 这个示例展示了轨迹规划器的各种功能，包括：
//! - Iterator 模式
//! - 进度跟踪
//! - 重置和重用
//! - 平滑性验证
//!
//! # 运行
//!
//! ```bash
//! cargo run --example high_level_trajectory_demo
//! ```

use piper_sdk::client::control::TrajectoryPlanner;
use piper_sdk::client::types::{Joint, JointArray, Rad};
use std::time::{Duration, Instant};

fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    println!("📈 Piper SDK - Trajectory Planner Demo");
    println!("======================================\n");

    // 1. 创建轨迹规划器
    let start = JointArray::from([
        Rad(0.0), // J1
        Rad(0.0), // J2
        Rad(0.0), // J3
        Rad(0.0), // J4
        Rad(0.0), // J5
        Rad(0.0), // J6
    ]);

    let end = JointArray::from([
        Rad(1.57), // J1 - 90 度
        Rad(1.0),  // J2
        Rad(0.5),  // J3
        Rad(-0.5), // J4
        Rad(0.3),  // J5
        Rad(0.8),  // J6
    ]);

    let duration = Duration::from_secs(3);
    let frequency_hz = 200.0; // 200Hz 高频采样

    let mut planner = TrajectoryPlanner::new(start, end, duration, frequency_hz);

    println!("🎯 轨迹配置:");
    println!("   - 起点: J1={:.3} rad", start[Joint::J1].0);
    println!(
        "   - 终点: J1={:.3} rad ({:.1}°)",
        end[Joint::J1].0,
        end[Joint::J1].0 * 180.0 / std::f64::consts::PI
    );
    println!("   - 持续时间: {:?}", duration);
    println!("   - 采样频率: {} Hz", frequency_hz);
    println!("   - 总采样点: {}", planner.total_samples());
    println!();

    // 2. 执行轨迹并收集数据
    println!("▶️  执行轨迹...\n");

    let start_time = Instant::now();
    let mut positions = Vec::new();
    let mut velocities = Vec::new();
    let mut step_count = 0;
    let total_samples = planner.total_samples();

    for (position, velocity) in &mut planner {
        positions.push(position[Joint::J1].0);
        velocities.push(velocity[Joint::J1]);
        step_count += 1;

        // 每 40 步打印一次
        if step_count % 40 == 0 {
            let progress = (step_count as f64) / (total_samples as f64);
            println!(
                "   Step {}/{}: 进度 {:.1}% | J1 位置: {:.4} rad | J1 速度: {:.4} rad/s",
                step_count,
                total_samples,
                progress * 100.0,
                position[Joint::J1].0,
                velocity[Joint::J1]
            );
        }
    }

    let elapsed = start_time.elapsed();

    println!("\n✅ 轨迹执行完成！");
    println!("   执行时间: {:?}", elapsed);
    println!("   总步数: {}", step_count);
    println!("   平均每步: {:?}", elapsed / step_count as u32);
    println!();

    // 3. 验证边界条件
    println!("🔍 边界条件验证:");

    let first_pos = positions.first().unwrap();
    let last_pos = positions.last().unwrap();
    let first_vel = velocities.first().unwrap();
    let last_vel = velocities.last().unwrap();

    println!(
        "   起点位置: {:.6} rad (期望: {:.6})",
        first_pos,
        start[Joint::J1].0
    );
    println!(
        "   终点位置: {:.6} rad (期望: {:.6})",
        last_pos,
        end[Joint::J1].0
    );
    println!("   起点速度: {:.6} rad/s (期望: 0)", first_vel);
    println!("   终点速度: {:.6} rad/s (期望: 0)", last_vel);

    let position_error_start = (first_pos - start[Joint::J1].0).abs();
    let position_error_end = (last_pos - end[Joint::J1].0).abs();

    println!("\n   ✅ 起点误差: {:.2e} rad", position_error_start);
    println!("   ✅ 终点误差: {:.2e} rad", position_error_end);
    println!("   ✅ 起点速度: {:.2e} rad/s", first_vel.abs());
    println!("   ✅ 终点速度: {:.2e} rad/s", last_vel.abs());
    println!();

    // 4. 平滑性分析
    println!("📊 平滑性分析:");

    let mut max_velocity = 0.0f64;
    let mut velocity_changes = 0;
    let mut last_vel_sign = velocities[0].signum();

    for &vel in &velocities {
        max_velocity = max_velocity.max(vel.abs());

        let vel_sign = vel.signum();
        if vel_sign != last_vel_sign && vel.abs() > 0.01 {
            velocity_changes += 1;
            last_vel_sign = vel_sign;
        }
    }

    println!("   最大速度: {:.4} rad/s", max_velocity);
    println!("   速度方向变化次数: {}", velocity_changes);

    if velocity_changes <= 2 {
        println!("   ✅ 轨迹单调平滑（方向变化 ≤ 2）");
    } else {
        println!("   ⚠️  轨迹有多次方向变化");
    }
    println!();

    // 5. 重置和重用
    println!("🔄 重置轨迹规划器...");

    planner.reset();
    println!("   ✅ 规划器已重置");
    println!("   ✅ 进度: {:.1}%", planner.progress() * 100.0);

    // 重新执行前几步
    let mut rerun_count = 0;
    for (position, _) in planner.take(10) {
        rerun_count += 1;
        if rerun_count == 1 {
            println!("   ✅ 第一步位置: {:.6} rad", position[Joint::J1].0);
        }
    }

    println!("   ✅ 重新执行了 {} 步", rerun_count);
    println!();

    // 6. 展示 API 特性
    println!("💡 TrajectoryPlanner 特性:");
    println!("   ✨ Iterator 模式 (内存高效, O(1))");
    println!("   ✨ 三次样条插值 (C² 连续)");
    println!("   ✨ 进度跟踪 (progress())");
    println!("   ✨ 可重置 (reset())");
    println!("   ✨ 边界条件保证 (起止速度为 0)");
    println!("   ✨ 强类型单位 (Rad)");
    println!();

    println!("🎉 演示完成！");

    Ok(())
}
