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
use piper_sdk::driver::observation::{Freshness, Observation, ObservationPayload};
use piper_sdk::driver::{
    CollisionProtection, CollisionProtectionLevel, DiagnosticEvent, EndLimitConfig, EndPose,
    FamilyObservationMetrics, JointAccelConfig, JointDriverLowSpeed, JointLimitConfig,
    PartialEndPose, PartialJointAccelConfig, PartialJointDriverLowSpeed, PartialJointLimitConfig,
    PiperBuilder,
};
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
    let diagnostics_rx = robot.subscribe_diagnostics();

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
        observation_is_complete_and_fresh,
    )
    .unwrap_or_else(|| robot.get_end_pose());

    println!("Joint positions: {:?}", joint_pos.joint_pos);
    print_end_pose_observation(&end_pose);
    println!(
        "Joint hardware timestamp: {} us",
        joint_pos.hardware_timestamp_us
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
        observation_is_complete_and_fresh,
    )
    .unwrap_or_else(|| robot.get_joint_driver_low_speed());
    print_low_speed_observation(&driver_state);

    // 5. 读取配置状态（按需查询，读锁）
    println!("\n--- 配置状态（按需查询）---");
    println!("Cached query-backed observations before querying:");
    println!(
        "{}",
        render_query_backed_observation_section(
            &robot.get_collision_protection(),
            &robot.get_joint_limit_config(),
            &robot.get_joint_accel_config(),
            &robot.get_end_limit_config(),
        )
    );

    match robot.query_collision_protection(query_timeout) {
        Ok(protection) => {
            println!("Collision protection levels:");
            for i in 0..6 {
                println!(
                    "  Joint {}: {}",
                    i + 1,
                    render_collision_protection_level(protection.value.levels[i])
                );
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
                    limits.value.joints[i].min_angle_rad,
                    limits.value.joints[i].max_angle_rad,
                    limits.value.joints[i].max_velocity_rad_s
                );
            }
            println!("✓ All joint limits received");
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
                    accel_limits.value.max_accel_rad_s2[i]
                );
            }
            println!("✓ All acceleration limits received");
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
                end_limits.value.max_linear_velocity_m_s
            );
            println!(
                "  Max angular velocity: {:.2} rad/s",
                end_limits.value.max_angular_velocity_rad_s
            );
            println!(
                "  Max linear accel: {:.2} m/s²",
                end_limits.value.max_linear_accel_m_s2
            );
            println!(
                "  Max angular accel: {:.2} rad/s²",
                end_limits.value.max_angular_accel_rad_s2
            );
            println!("✓ End limits are valid");
        },
        Err(err) => {
            println!("\nEnd-effector limit query failed: {err}");
        },
    }

    println!("\nCached query-backed observations after querying:");
    println!(
        "{}",
        render_query_backed_observation_section(
            &robot.get_collision_protection(),
            &robot.get_joint_limit_config(),
            &robot.get_joint_accel_config(),
            &robot.get_end_limit_config(),
        )
    );

    // 6. 读取诊断事件
    println!("\n--- 诊断事件 ---");
    let retained_diagnostics = robot.snapshot_diagnostics();
    if retained_diagnostics.is_empty() {
        println!("Retained diagnostics: none");
    } else {
        println!("Retained diagnostics:");
        for event in &retained_diagnostics {
            println!("  {}", format_diagnostic_event(event));
        }
    }

    let live_diagnostics: Vec<_> = diagnostics_rx.try_iter().collect();
    if live_diagnostics.is_empty() {
        println!("Live diagnostics observed during this run: none");
    } else {
        println!("Live diagnostics observed during this run:");
        for event in &live_diagnostics {
            println!("  {}", format_diagnostic_event(event));
        }
    }

    // 7. 读取速率统计
    println!("\n--- 速率统计 ---");
    let fps = robot.get_fps();
    println!("Joint position FPS: {:.2}", fps.joint_position);
    println!("Robot control FPS: {:.2}", fps.robot_control);
    println!("Gripper FPS: {:.2}", fps.gripper);

    let observation_metrics = robot.get_observation_metrics();
    print_family_observation_metrics("End pose", observation_metrics.end_pose);
    print_family_observation_metrics("Joint driver low speed", observation_metrics.low_speed);
    print_family_observation_metrics(
        "Collision protection",
        observation_metrics.collision_protection,
    );
    print_family_observation_metrics("Joint limit config", observation_metrics.joint_limit_config);
    print_family_observation_metrics("Joint accel config", observation_metrics.joint_accel_config);
    print_family_observation_metrics("End limit config", observation_metrics.end_limit_config);

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

fn render_collision_protection_level(level: CollisionProtectionLevel) -> &'static str {
    match level {
        CollisionProtectionLevel::Disabled => "Disabled",
        CollisionProtectionLevel::Level1 => "Level 1",
        CollisionProtectionLevel::Level2 => "Level 2",
        CollisionProtectionLevel::Level3 => "Level 3",
        CollisionProtectionLevel::Level4 => "Level 4",
        CollisionProtectionLevel::Level5 => "Level 5",
        CollisionProtectionLevel::Level6 => "Level 6",
        CollisionProtectionLevel::Level7 => "Level 7",
        CollisionProtectionLevel::Level8 => "Level 8",
    }
}

fn format_freshness(freshness: &Freshness) -> String {
    match freshness {
        Freshness::Fresh => "fresh".to_owned(),
        Freshness::Stale { stale_for } => format!("stale for {} ms", stale_for.as_millis()),
    }
}

fn format_observation_status<T, TPartial>(observation: &Observation<T, TPartial>) -> String {
    match observation {
        Observation::Unavailable => "Unavailable".to_owned(),
        Observation::Available(available) => match &available.payload {
            ObservationPayload::Complete(_) => {
                format!(
                    "Available (complete, {})",
                    format_freshness(&available.freshness)
                )
            },
            ObservationPayload::Partial { missing, .. } => format!(
                "Available (partial, {}, missing {:?})",
                format_freshness(&available.freshness),
                missing.missing_indices
            ),
        },
    }
}

fn render_query_backed_observation_section(
    collision: &Observation<CollisionProtection>,
    joint_limits: &Observation<JointLimitConfig, PartialJointLimitConfig>,
    joint_accel: &Observation<JointAccelConfig, PartialJointAccelConfig>,
    end_limits: &Observation<EndLimitConfig>,
) -> String {
    [
        format!(
            "Collision protection: {}",
            format_observation_status(collision)
        ),
        format!("Joint limits: {}", format_observation_status(joint_limits)),
        format!(
            "Joint acceleration limits: {}",
            format_observation_status(joint_accel)
        ),
        format!(
            "End-effector limits: {}",
            format_observation_status(end_limits)
        ),
    ]
    .join("\n")
}

fn format_diagnostic_event(event: &DiagnosticEvent) -> String {
    format!("{event:?}")
}

fn print_family_observation_metrics(label: &str, metrics: FamilyObservationMetrics) {
    println!(
        "{label}: raw={:.2} complete={:.2} diagnostic={:.2}",
        metrics.raw_frame_rate, metrics.complete_observation_rate, metrics.diagnostic_rate
    );
}

fn observation_is_complete_and_fresh<T, TPartial>(state: &Observation<T, TPartial>) -> bool {
    matches!(
        state,
        Observation::Available(available)
            if matches!(available.payload, ObservationPayload::Complete(_))
                && matches!(available.freshness, Freshness::Fresh)
    )
}

fn print_end_pose_observation(observation: &Observation<EndPose, PartialEndPose>) {
    match observation {
        Observation::Available(available) => {
            println!(
                "End pose freshness: {}",
                format_freshness(&available.freshness)
            );
            println!("End pose meta: {:?}", available.meta);
            match &available.payload {
                ObservationPayload::Complete(end_pose) => {
                    println!("End pose: {:?}", end_pose.end_pose);
                    println!("✓ All end pose frames received");
                },
                ObservationPayload::Partial { partial, missing } => {
                    println!("Partial end pose: {:?}", partial.end_pose);
                    println!("⚠ Missing end pose members: {:?}", missing.missing_indices);
                },
            }
        },
        Observation::Unavailable => {
            println!("End pose observation unavailable");
        },
    }
}

fn print_low_speed_observation(
    observation: &Observation<JointDriverLowSpeed, PartialJointDriverLowSpeed>,
) {
    match observation {
        Observation::Available(available) => {
            println!(
                "Low-speed freshness: {}",
                format_freshness(&available.freshness)
            );
            println!("Low-speed meta: {:?}", available.meta);
            match &available.payload {
                ObservationPayload::Complete(driver_state) => {
                    println!("✓ All joint driver states received");
                    for (index, joint) in driver_state.joints.iter().enumerate() {
                        println!(
                            "  Joint {}: motor={:.1}°C driver={:.1}°C voltage={:.2}V current={:.2}A enabled={}",
                            index + 1,
                            joint.motor_temp_c,
                            joint.driver_temp_c,
                            joint.joint_voltage_v,
                            joint.joint_bus_current_a,
                            joint.enabled
                        );
                    }
                },
                ObservationPayload::Partial { partial, missing } => {
                    println!("⚠ Missing joints: {:?}", missing.missing_indices);
                    for (index, joint) in partial.joints.iter().enumerate() {
                        if let Some(joint) = joint {
                            println!(
                                "  Joint {}: motor={:.1}°C driver={:.1}°C voltage={:.2}V current={:.2}A enabled={}",
                                index + 1,
                                joint.motor_temp_c,
                                joint.driver_temp_c,
                                joint.joint_voltage_v,
                                joint.joint_bus_current_a,
                                joint.enabled
                            );
                        }
                    }
                },
            }
        },
        Observation::Unavailable => {
            println!("No joint driver low-speed feedback received yet.");
            println!("Expected CAN IDs: 0x261-0x266.");
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use piper_sdk::driver::{
        CollisionProtection, CollisionProtectionLevel, EndLimitConfig, JointAccelConfig,
        JointLimitConfig, PartialJointAccelConfig, PartialJointLimitConfig,
    };
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn render_collision_protection_level_uses_typed_values_only() {
        assert_eq!(
            render_collision_protection_level(CollisionProtectionLevel::Disabled),
            "Disabled"
        );
        assert_eq!(
            render_collision_protection_level(CollisionProtectionLevel::Level8),
            "Level 8"
        );
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

    #[test]
    fn state_api_demo_formats_unavailable_without_fake_zero_values() {
        let collision = Observation::<CollisionProtection>::Unavailable;
        let joint_limits = Observation::<JointLimitConfig, PartialJointLimitConfig>::Unavailable;
        let joint_accel = Observation::<JointAccelConfig, PartialJointAccelConfig>::Unavailable;
        let end_limits = Observation::<EndLimitConfig>::Unavailable;

        let rendered = render_query_backed_observation_section(
            &collision,
            &joint_limits,
            &joint_accel,
            &end_limits,
        );

        assert!(rendered.contains("Collision protection: Unavailable"));
        assert!(rendered.contains("Joint limits: Unavailable"));
        assert!(rendered.contains("Joint acceleration limits: Unavailable"));
        assert!(rendered.contains("End-effector limits: Unavailable"));
        assert!(!rendered.contains("Level 0"));
        assert!(!rendered.contains("0.00 rad/s"));
        assert!(!rendered.contains("0.00 rad/s²"));
        assert!(!rendered.contains("0.00 m/s"));
        assert!(!rendered.contains("0.00 m/s²"));
    }
}
