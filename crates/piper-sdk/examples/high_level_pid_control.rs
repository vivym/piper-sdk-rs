//! PID 控制示例 - 展示 PID 控制器使用
//!
//! 这个示例展示了如何使用 PID 控制器进行位置控制。
//!
//! # 运行
//!
//! ```bash
//! cargo run -p piper-sdk --example high_level_pid_control
//! ```

use piper_sdk::client::ControlSnapshot;
use piper_sdk::client::control::{Controller, PidController};
use piper_sdk::client::types::{JointArray, NewtonMeter, Rad, RadPerSecond};
use std::time::Duration;

fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    // 初始化日志
    piper_sdk::init_logger!();

    println!("🎯 Piper SDK - PID Control Example");
    println!("===================================\n");

    // 1. 创建 PID 控制器
    let target_position = JointArray::from([Rad(1.0); 6]);

    let mut pid = PidController::new(target_position)
        .with_gains(10.0, 0.5, 0.1)    // Kp=10, Ki=0.5, Kd=0.1
        .with_integral_limit(5.0)       // 积分饱和保护
        .with_output_limit(50.0); // 输出力矩限制 50 Nm

    println!("🔧 PID 控制器配置:");
    println!("   - Kp (比例增益): 10.0");
    println!("   - Ki (积分增益): 0.5");
    println!("   - Kd (微分增益): 0.1");
    println!("   - 积分限制: 5.0");
    println!("   - 输出限制: 50.0 Nm");
    println!("   - 目标位置: {:?}", target_position[0]);
    println!();

    // 2. 模拟控制循环
    let dt = Duration::from_millis(10); // 10ms = 100Hz
    let mut current_position = JointArray::from([Rad(0.0); 6]);
    let mut current_velocity = JointArray::from([RadPerSecond(0.0); 6]);

    println!("▶️  开始控制循环 (模拟)...\n");

    for iteration in 0..100 {
        let snapshot = ControlSnapshot {
            position: current_position,
            velocity: current_velocity,
            torque: JointArray::from([NewtonMeter(0.0); 6]),
            position_timestamp_us: iteration as u64 * 10_000,
            dynamic_timestamp_us: iteration as u64 * 10_000,
            skew_us: 0,
        };

        // 计算控制输出
        let output = pid.tick(&snapshot, dt)?;

        // 模拟系统响应（简化的一阶系统）
        // 实际应用中，这里会发送命令到机器人
        for i in 0..6 {
            let force = output[i].0;
            // 简化的动力学：力矩 -> 角加速度 -> 速度 -> 位置
            let acceleration = force * 0.01;
            current_velocity[i] =
                RadPerSecond(current_velocity[i].0 + acceleration * dt.as_secs_f64());
            current_position[i] =
                Rad(current_position[i].0 + current_velocity[i].0 * dt.as_secs_f64());
        }

        // 每 10 次迭代打印一次状态
        if iteration % 10 == 0 {
            let error = (target_position[0].0 - current_position[0].0).abs();
            let integral = pid.integral()[0];

            println!(
                "   Iter {}: 位置: {:.4} | 误差: {:.4} | 积分: {:.4} | 输出: {:.2} Nm",
                iteration, current_position[0].0, error, integral, output[0].0
            );
        }

        // 模拟控制周期
        std::thread::sleep(Duration::from_millis(1));
    }

    println!("\n✅ 控制循环完成！");
    println!("   最终位置: {:?}", current_position[0]);
    println!("   目标位置: {:?}", target_position[0]);
    println!(
        "   最终误差: {:.6} rad",
        (target_position[0].0 - current_position[0].0).abs()
    );
    println!();

    // 3. 展示时间跳变处理
    println!("⚠️  模拟时间跳变...");

    // 模拟系统卡顿（大的 dt）
    let large_dt = Duration::from_millis(100);
    pid.on_time_jump(large_dt)?;

    println!("   ✅ on_time_jump() 调用成功");
    println!("   ✅ 积分项保留 (防止机械臂下坠)");
    println!("   ✅ 微分项直接使用实测速度，不依赖旧误差差分");
    println!();

    // 4. 展示重置功能
    println!("🔄 重置控制器...");
    pid.reset()?;

    println!("   ✅ 所有内部状态已清零");
    println!("   ✅ 积分项: {:.6}", pid.integral()[0]);
    println!();

    // 5. 展示 API 特性
    println!("💡 PID 控制器特性:");
    println!("   ✨ Builder 模式 (链式配置)");
    println!("   ✨ 积分饱和保护 (防止 Integral Windup)");
    println!("   ✨ 输出钳位 (安全保护)");
    println!("   ✨ 时间跳变处理 (保留积分项)");
    println!("   ✨ 强类型单位 (Rad, NewtonMeter)");
    println!();

    println!("🎉 示例完成！");

    Ok(())
}
