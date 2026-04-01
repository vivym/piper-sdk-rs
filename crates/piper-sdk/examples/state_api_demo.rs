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
//!
//! ```bash
//! # Linux (SocketCAN)
//! cargo run -p piper-sdk --example state_api_demo -- --interface can0
//!
//! # macOS/Windows (GS-USB serial)
//! cargo run -p piper-sdk --example state_api_demo -- --interface ABC123456
//! ```

use clap::Parser;
use piper_sdk::driver::PiperBuilder;
use std::time::Duration;

#[derive(Parser, Debug)]
#[command(name = "state_api_demo")]
#[command(about = "One-shot state API demo")]
struct Args {
    /// Linux: SocketCAN interface; macOS/Windows: GS-USB serial.
    #[cfg_attr(target_os = "linux", arg(long, default_value = "can0"))]
    #[cfg_attr(not(target_os = "linux"), arg(long))]
    interface: String,

    /// CAN bitrate in bps.
    #[arg(long, default_value_t = 1_000_000)]
    baud_rate: u32,

    /// Wait timeout for the first feedback in seconds.
    #[arg(long, default_value_t = 5)]
    feedback_timeout_secs: u64,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    piper_sdk::init_logger!();

    let args = Args::parse();

    // 创建 Piper 实例
    // 注意：此示例需要实际的 CAN 适配器
    // 对于测试，可以使用 MockCanAdapter
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

    // 等待接收到第一个有效反馈
    println!("Waiting for robot feedback on {}...", args.interface);
    robot.wait_for_feedback(Duration::from_secs(args.feedback_timeout_secs))?;
    println!("Robot feedback received!");

    // 运行一段时间，收集数据
    println!("\n=== 状态 API 演示 ===\n");
    let query_timeout = Duration::from_secs(args.feedback_timeout_secs);
    let motion_wait_timeout = query_timeout.min(Duration::from_millis(100));
    let low_speed_wait_timeout = query_timeout.min(Duration::from_millis(250));
    let poll_interval = Duration::from_millis(2);

    // 1. 读取关节位置和末端位姿（500Hz，无锁）
    println!("--- 运动状态（500Hz）---");

    // 方案1：分别获取（适合需要独立时间戳的场景）
    let joint_pos = wait_for_ready_state(
        motion_wait_timeout,
        poll_interval,
        || robot.get_joint_position(),
        |state| state.is_fully_valid(),
    )
    .unwrap_or_else(|| robot.get_joint_position());
    let end_pose = wait_for_ready_state(
        motion_wait_timeout,
        poll_interval,
        || robot.get_end_pose(),
        |state| state.is_fully_valid(),
    )
    .unwrap_or_else(|| robot.get_end_pose());

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
    let snapshot = wait_for_ready_state(
        motion_wait_timeout,
        poll_interval,
        || robot.capture_motion_snapshot(),
        |state| state.joint_position.is_fully_valid() && state.end_pose.is_fully_valid(),
    )
    .unwrap_or_else(|| robot.capture_motion_snapshot());
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
    let driver_state = wait_for_ready_state(
        low_speed_wait_timeout,
        poll_interval,
        || robot.get_joint_driver_low_speed(),
        |state| state.is_fully_valid(),
    )
    .unwrap_or_else(|| robot.get_joint_driver_low_speed());

    if driver_state.valid_mask == 0 {
        println!("No joint driver low-speed feedback received yet.");
        println!("Expected CAN IDs: 0x261-0x266.");
    } else {
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

        if driver_state.is_fully_valid() {
            println!("\n✓ All joint driver states received");
        } else {
            println!("\n⚠ Missing joints: {:?}", driver_state.missing_joints());
        }
    }

    // 5. 读取配置状态（按需查询，读锁）
    println!("\n--- 配置状态（按需查询）---");

    match robot.query_collision_protection(query_timeout) {
        Ok(protection) => {
            println!("Collision protection levels:");
            let mut has_invalid_level = false;
            for i in 0..6 {
                let level = protection.protection_levels[i];
                if level > 8 {
                    has_invalid_level = true;
                }
                println!("  Joint {}: {}", i + 1, format_collision_level(level));
            }
            if has_invalid_level {
                println!("⚠ Collision protection feedback contains out-of-range values.");
            }
        },
        Err(err) => {
            println!("Collision protection query failed: {err}");
        },
    }

    match robot.query_joint_limit_config(query_timeout) {
        Ok(limits) => {
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
        },
        Err(err) => {
            println!("\nJoint limit query failed: {err}");
        },
    }

    match robot.query_joint_accel_config(query_timeout) {
        Ok(accel_limits) => {
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
        },
        Err(err) => {
            println!("\nJoint acceleration query failed: {err}");
        },
    }

    match robot.query_end_limit_config(query_timeout) {
        Ok(end_limits) => {
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
        },
        Err(err) => {
            println!("\nEnd-effector limit query failed: {err}");
        },
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

fn wait_for_ready_state<T, Read, Ready>(
    timeout: Duration,
    poll_interval: Duration,
    mut read: Read,
    mut is_ready: Ready,
) -> Option<T>
where
    Read: FnMut() -> T,
    Ready: FnMut(&T) -> bool,
{
    let start = std::time::Instant::now();

    loop {
        let state = read();
        if is_ready(&state) {
            return Some(state);
        }

        if start.elapsed() >= timeout {
            return None;
        }

        let remaining = timeout.saturating_sub(start.elapsed());
        let sleep_duration = poll_interval.min(remaining);
        if sleep_duration.is_zero() {
            return None;
        }

        std::thread::sleep(sleep_duration);
    }
}

fn format_collision_level(level: u8) -> String {
    if level <= 8 {
        format!("Level {level}")
    } else {
        format!("invalid ({level})")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn format_collision_level_marks_invalid_values() {
        assert_eq!(format_collision_level(255), "invalid (255)");
        assert_eq!(format_collision_level(250), "invalid (250)");
    }

    #[test]
    fn format_collision_level_preserves_valid_range() {
        assert_eq!(format_collision_level(0), "Level 0");
        assert_eq!(format_collision_level(8), "Level 8");
    }

    #[test]
    fn wait_for_ready_state_retries_until_state_is_ready() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let read = {
            let attempts = Arc::clone(&attempts);
            move || {
                let current = attempts.fetch_add(1, Ordering::SeqCst);
                current >= 2
            }
        };

        let ready = wait_for_ready_state(
            Duration::from_millis(50),
            Duration::from_millis(1),
            read,
            |state| *state,
        )
        .expect("helper should retry until the state is ready");

        assert!(ready);
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn wait_for_ready_state_returns_none_after_timeout() {
        let started_at = std::time::Instant::now();
        let value = wait_for_ready_state(
            Duration::from_millis(20),
            Duration::from_millis(5),
            || false,
            |state| *state,
        );

        assert!(started_at.elapsed() >= Duration::from_millis(20));
        assert!(value.is_none());
    }
}
