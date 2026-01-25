//! 状态 API 使用演示
//!
//! 本示例演示如何使用新的状态 API 读取机器人状态，适合作为 API 参考。
//! 展示的功能包括：
//! - 关节位置和末端位姿（500Hz，无锁）
//! - 机器人控制状态和夹爪状态（100Hz，无锁）
//! - 关节驱动器诊断状态（40Hz，无锁）
//! - 配置状态（按需查询，读锁）
//!
//! **注意**：此示例一次性读取所有状态后退出，适合学习 API 用法。
//! 如需实时监控，请使用 `robot_monitor` 示例。

use piper_sdk::driver::PiperBuilder;
use std::time::Duration;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 创建 Piper 实例
    // 注意：此示例需要实际的 CAN 适配器
    // 对于测试，可以使用 MockCanAdapter
    let robot = PiperBuilder::new()
        .interface("can0")  // Linux: SocketCAN 接口名
        .baud_rate(1_000_000)  // CAN 波特率
        .build()?;

    // 等待接收到第一个有效反馈
    println!("Waiting for robot feedback...");
    robot.wait_for_feedback(Duration::from_secs(5))?;
    println!("Robot feedback received!");

    // 运行一段时间，收集数据
    println!("\n=== 状态 API 演示 ===\n");

    // 1. 读取关节位置和末端位姿（500Hz，无锁）
    println!("--- 运动状态（500Hz）---");

    // 方案1：分别获取（适合需要独立时间戳的场景）
    let joint_pos = robot.get_joint_position();
    let end_pose = robot.get_end_pose();

    println!("Joint positions: {:?}", joint_pos.joint_pos);
    println!("End pose: {:?}", end_pose.end_pose);
    println!(
        "Joint hardware timestamp: {} us",
        joint_pos.hardware_timestamp_us
    );
    println!(
        "End pose hardware timestamp: {} us",
        end_pose.hardware_timestamp_us
    );

    // 检查帧完整性
    if joint_pos.is_fully_valid() {
        println!("✓ All joint position frames received");
    } else {
        println!(
            "⚠ Missing joint position frames: {:?}",
            joint_pos.missing_frames()
        );
    }

    if end_pose.is_fully_valid() {
        println!("✓ All end pose frames received");
    } else {
        println!("⚠ Missing end pose frames: {:?}", end_pose.missing_frames());
    }

    // 方案2：使用快照（适合需要逻辑原子性的场景）
    let snapshot = robot.capture_motion_snapshot();
    println!("\nMotion snapshot:");
    println!("  Joint positions: {:?}", snapshot.joint_position.joint_pos);
    println!("  End pose: {:?}", snapshot.end_pose.end_pose);

    // 2. 读取机器人控制状态（100Hz，无锁）
    println!("\n--- 控制状态（100Hz）---");
    let control = robot.get_robot_control();

    println!("Control mode: {}", control.control_mode);
    println!("Robot status: {}", control.robot_status);
    println!("Is enabled: {}", control.is_enabled);
    println!("Feedback counter: {}", control.feedback_counter);

    // 检查故障码（位掩码）
    println!("\nFault codes:");
    for i in 0..6 {
        if control.is_angle_limit(i) {
            println!("  Joint {}: Angle limit reached", i + 1);
        }
        if control.is_comm_error(i) {
            println!("  Joint {}: Communication error", i + 1);
        }
    }

    // 3. 读取夹爪状态（100Hz，无锁）
    println!("\n--- 夹爪状态（100Hz）---");
    let gripper = robot.get_gripper();

    println!("Travel: {:.2} mm", gripper.travel);
    println!("Torque: {:.2} N·m", gripper.torque);
    println!("Status code: 0x{:02X}", gripper.status_code);
    println!("Is moving: {}", gripper.is_moving());

    // 检查状态
    if gripper.is_voltage_low() {
        println!("⚠ Gripper voltage low!");
    }
    if gripper.is_motor_over_temp() {
        println!("⚠ Gripper motor over temperature!");
    }

    // 4. 读取关节驱动器诊断状态（40Hz，无锁，Wait-Free）
    println!("\n--- 关节驱动器诊断状态（40Hz）---");
    let driver_state = robot.get_joint_driver_low_speed();

    println!("Motor temperatures:");
    for i in 0..6 {
        println!("  Joint {}: {:.1}°C", i + 1, driver_state.motor_temps[i]);
    }

    println!("\nDriver temperatures:");
    for i in 0..6 {
        println!("  Joint {}: {:.1}°C", i + 1, driver_state.driver_temps[i]);
    }

    println!("\nVoltages:");
    for i in 0..6 {
        println!("  Joint {}: {:.2}V", i + 1, driver_state.joint_voltage[i]);
    }

    println!("\nCurrents:");
    for i in 0..6 {
        println!(
            "  Joint {}: {:.2}A",
            i + 1,
            driver_state.joint_bus_current[i]
        );
    }

    // 检查状态（位掩码）
    println!("\nDriver status:");
    for i in 0..6 {
        if driver_state.is_voltage_low(i) {
            println!("  Joint {}: ⚠ Voltage low", i + 1);
        }
        if driver_state.is_motor_over_temp(i) {
            println!("  Joint {}: ⚠ Motor over temperature", i + 1);
        }
        if driver_state.is_over_current(i) {
            println!("  Joint {}: ⚠ Over current", i + 1);
        }
        if driver_state.is_enabled(i) {
            println!("  Joint {}: ✓ Driver enabled", i + 1);
        }
    }

    // 检查完整性
    if driver_state.is_fully_valid() {
        println!("\n✓ All joint driver states received");
    } else {
        println!("\n⚠ Missing joints: {:?}", driver_state.missing_joints());
    }

    // 5. 读取配置状态（按需查询，读锁）
    println!("\n--- 配置状态（按需查询）---");

    // 碰撞保护状态
    if let Ok(protection) = robot.get_collision_protection() {
        println!("Collision protection levels:");
        for i in 0..6 {
            println!(
                "  Joint {}: Level {}",
                i + 1,
                protection.protection_levels[i]
            );
        }
    }

    // 关节限制配置
    if let Ok(limits) = robot.get_joint_limit_config() {
        println!("\nJoint limits:");
        for i in 0..6 {
            println!(
                "  Joint {}: [{:.2}, {:.2}] rad, max vel: {:.2} rad/s",
                i + 1,
                limits.joint_limits_min[i],
                limits.joint_limits_max[i],
                limits.joint_max_velocity[i]
            );
        }

        if limits.is_fully_valid() {
            println!("✓ All joint limits received");
        } else {
            println!("⚠ Missing joints: {:?}", limits.missing_joints());
        }
    }

    // 关节加速度限制配置
    if let Ok(accel_limits) = robot.get_joint_accel_config() {
        println!("\nJoint acceleration limits:");
        for i in 0..6 {
            println!(
                "  Joint {}: {:.2} rad/s²",
                i + 1,
                accel_limits.max_acc_limits[i]
            );
        }

        if accel_limits.is_fully_valid() {
            println!("✓ All acceleration limits received");
        } else {
            println!("⚠ Missing joints: {:?}", accel_limits.missing_joints());
        }
    }

    // 末端限制配置
    if let Ok(end_limits) = robot.get_end_limit_config() {
        println!("\nEnd-effector limits:");
        println!(
            "  Max linear velocity: {:.2} m/s",
            end_limits.max_end_linear_velocity
        );
        println!(
            "  Max angular velocity: {:.2} rad/s",
            end_limits.max_end_angular_velocity
        );
        println!(
            "  Max linear accel: {:.2} m/s²",
            end_limits.max_end_linear_accel
        );
        println!(
            "  Max angular accel: {:.2} rad/s²",
            end_limits.max_end_angular_accel
        );

        if end_limits.is_valid {
            println!("✓ End limits are valid");
        }
    }

    // 6. 读取 FPS 统计
    println!("\n--- FPS 统计 ---");
    let fps = robot.get_fps();
    println!("Joint position FPS: {:.2}", fps.joint_position);
    println!("End pose FPS: {:.2}", fps.end_pose);
    println!("Robot control FPS: {:.2}", fps.robot_control);
    println!("Gripper FPS: {:.2}", fps.gripper);
    println!(
        "Joint driver low speed FPS: {:.2}",
        fps.joint_driver_low_speed
    );

    println!("\n=== 示例完成 ===");
    Ok(())
}
