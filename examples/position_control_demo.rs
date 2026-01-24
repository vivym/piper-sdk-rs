//! 位置控制演示 - 完整的机械臂控制流程
//!
//! 这个示例展示了完整的机械臂控制流程：
//! 1. 连接机械臂
//! 2. 使能机械臂
//! 3. 获取当前关节位置
//! 4. 移动到目标位置
//! 5. 保持一段时间
//! 6. 移动回原位置
//! 7. 失能机械臂
//!
//! # 运行
//!
//! ```bash
//! # Linux (SocketCAN)
//! cargo run --example position_control_demo -- --interface can0
//!
//! # 所有平台 (GS-USB)
//! cargo run --example position_control_demo -- --interface ABC123456
//! ```

use clap::Parser;
use piper_sdk::PiperBuilder;
use piper_sdk::client::state::*;
use piper_sdk::client::types::*;
use std::time::{Duration, Instant};

/// 命令行参数
#[derive(Parser, Debug)]
#[command(name = "position_control_demo")]
#[command(about = "位置控制演示 - 完整的机械臂控制流程")]
struct Args {
    /// CAN 接口名称或设备序列号
    ///
    /// - Linux: "can0"/"can1" 等 SocketCAN 接口名，或设备序列号（使用 GS-USB）
    /// - macOS/Windows: GS-USB 设备序列号
    #[arg(long, default_value = "can0")]
    interface: String,

    /// CAN 波特率（默认: 1000000）
    #[arg(long, default_value = "1000000")]
    baud_rate: u32,
}

fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    println!("🤖 Piper SDK - 位置控制演示");
    println!("============================\n");

    // ==================== 步骤 1: 连接机械臂 ====================
    println!("📡 步骤 1: 连接机械臂...");

    // 使用新的 Builder API 连接（自动处理平台差异）
    let robot = PiperBuilder::new()
        .interface(&args.interface)
        .baud_rate(args.baud_rate)
        .build()?;
    println!("   ✅ 连接成功\n");

    // ==================== 步骤 2: 使能机械臂 ====================
    println!("⚡ 步骤 2: 使能机械臂（位置模式）...");
    let robot = robot.enable_position_mode(PositionModeConfig::default())?;
    println!("   ✅ 使能成功\n");

    std::thread::sleep(Duration::from_secs(2));

    // ==================== 步骤 3: 获取当前关节位置 ====================
    println!("📍 步骤 3: 获取当前关节位置...");
    let observer = robot.observer();
    let current_positions = observer.joint_positions();

    println!("   当前关节位置:");
    for (i, pos) in current_positions.iter().enumerate() {
        println!(
            "     J{}: {:.4} rad ({:.2} deg)",
            i + 1,
            pos.0,
            pos.to_deg().0
        );
    }
    println!();

    // ==================== 步骤 4: 移动到目标位置 ====================
    println!("🎯 步骤 4: 移动到目标位置...");

    // 定义目标位置（直接指定关节角度）
    let target_positions = JointArray::from([
        Rad(0.074125),    // J1
        Rad(0.1162963),   // J2
        Rad(-0.47472),    // J3
        Rad(-0.67663265), // J4
        Rad(0.77636364),  // J5
        Rad(0.80553846),  // J6
    ]);

    println!("   目标关节位置:");
    for (i, pos) in target_positions.iter().enumerate() {
        println!(
            "     J{}: {:.4} rad ({:.2} deg)",
            i + 1,
            pos.0,
            pos.to_deg().0
        );
    }
    println!();

    // 发送位置命令（只发送一次，与 Python SDK 一致）
    let motion = robot.motion_commander();
    motion.send_position_command_batch(&target_positions)?;
    println!("   ✅ 位置命令已发送");

    // 等待运动完成（简单方法：等待一段时间）
    // 注意：实际应用中应该监控位置误差，直到到达目标位置
    println!("   ⏳ 等待运动完成...");
    std::thread::sleep(Duration::from_secs(10));

    // 读取实际位置并验证
    let actual_positions = observer.joint_positions();
    println!("   ✅ 运动完成");
    println!("\n   📊 目标位置 vs 实际位置对比:");
    let mut max_error = 0.0;
    let mut max_error_joint = 0;
    for (i, (target, actual)) in target_positions.iter().zip(actual_positions.iter()).enumerate() {
        let error = (target.0 - actual.0).abs();
        let error_deg = error * 180.0 / std::f64::consts::PI;
        if error > max_error {
            max_error = error;
            max_error_joint = i;
        }
        println!(
            "     J{}: 目标={:.4} rad ({:.2} deg), 实际={:.4} rad ({:.2} deg), 误差={:.4} rad ({:.2} deg)",
            i + 1,
            target.0,
            target.to_deg().0,
            actual.0,
            actual.to_deg().0,
            error,
            error_deg
        );
    }
    println!(
        "\n   📈 最大误差: J{} = {:.4} rad ({:.2} deg)\n",
        max_error_joint + 1,
        max_error,
        max_error * 180.0 / std::f64::consts::PI
    );

    // ==================== 步骤 5: 保持位置一段时间 ====================
    println!("⏸️  步骤 5: 保持位置 2 秒...");
    let hold_start = Instant::now();
    let hold_duration = Duration::from_secs(2);

    // 在保持期间，持续发送位置命令以保持位置
    while hold_start.elapsed() < hold_duration {
        motion.send_position_command_batch(&target_positions)?;
        std::thread::sleep(Duration::from_millis(200)); // 5Hz 控制频率
    }

    // 验证保持后的位置
    let hold_positions = observer.joint_positions();
    println!("   ✅ 保持完成");
    println!("\n   📊 保持后位置验证:");
    for (i, (target, actual)) in target_positions.iter().zip(hold_positions.iter()).enumerate() {
        let error = (target.0 - actual.0).abs();
        let error_deg = error * 180.0 / std::f64::consts::PI;
        println!(
            "     J{}: 目标={:.4} rad ({:.2} deg), 实际={:.4} rad ({:.2} deg), 误差={:.4} rad ({:.2} deg)",
            i + 1,
            target.0,
            target.to_deg().0,
            actual.0,
            actual.to_deg().0,
            error,
            error_deg
        );
    }
    println!();

    // ==================== 步骤 6: 移动回原位置 ====================
    println!("🔙 步骤 6: 移动回原位置...");
    motion.send_position_command_batch(&current_positions)?;
    println!("   ✅ 位置命令已发送");
    println!("   ⏳ 等待运动完成...");
    std::thread::sleep(Duration::from_secs(10));
    println!("   ✅ 运动完成\n");

    // 验证是否回到原位置
    let final_positions = observer.joint_positions();
    println!("   最终关节位置（与初始位置对比）:");
    let mut max_return_error = 0.0;
    let mut max_return_error_joint = 0;
    for (i, (final_pos, initial_pos)) in
        final_positions.iter().zip(current_positions.iter()).enumerate()
    {
        let error = (final_pos.0 - initial_pos.0).abs();
        let error_deg = error * 180.0 / std::f64::consts::PI;
        if error > max_return_error {
            max_return_error = error;
            max_return_error_joint = i;
        }
        println!(
            "     J{}: 初始={:.4} rad ({:.2} deg), 最终={:.4} rad ({:.2} deg), 误差={:.4} rad ({:.2} deg)",
            i + 1,
            initial_pos.0,
            initial_pos.to_deg().0,
            final_pos.0,
            final_pos.to_deg().0,
            error,
            error_deg
        );
    }
    println!(
        "\n   📈 最大回位误差: J{} = {:.4} rad ({:.2} deg)\n",
        max_return_error_joint + 1,
        max_return_error,
        max_return_error * 180.0 / std::f64::consts::PI
    );

    // ==================== 步骤 7: 失能机械臂 ====================
    println!("🛑 步骤 7: 失能机械臂...");
    let _robot = robot.disable(DisableConfig::default())?;
    println!("   ✅ 失能成功\n");

    println!("🎉 演示完成！");

    Ok(())
}
