//! One-shot 模式

use anyhow::Result;
use piper_control::TargetSpec;
use piper_sdk::driver::{
    EndPoseState, FpsResult, GripperState, JointDynamicState, JointPositionState, RobotControlState,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::signal;

use crate::commands::config::CliConfig;
use crate::connection::{driver_builder, resolved_target};

pub struct OneShotMode {
    config: CliConfig,
}

impl OneShotMode {
    pub async fn new() -> Result<Self> {
        Ok(Self {
            config: CliConfig::load()?,
        })
    }

    pub async fn monitor(
        &mut self,
        frequency: u32,
        override_target: Option<&TargetSpec>,
    ) -> Result<()> {
        println!("⏳ 连接到机器人...");
        let target = resolved_target(&self.config, override_target);
        let builder = driver_builder(&target);
        let piper = builder.build()?;

        println!("✅ 已连接");
        println!("📊 监控中 ({} Hz)...", frequency);
        println!("按 Ctrl+C 停止\n");

        let running = Arc::new(AtomicBool::new(true));
        let running_for_signal = Arc::clone(&running);

        tokio::spawn(async move {
            #[cfg(unix)]
            {
                if let Ok(mut sig) = signal::unix::signal(signal::unix::SignalKind::interrupt()) {
                    sig.recv().await;
                    running_for_signal.store(false, Ordering::SeqCst);
                    println!("\n收到退出信号，正在关闭...");
                }
            }
            #[cfg(windows)]
            {
                if let Ok(mut sig) = signal::windows::ctrl_c() {
                    sig.recv().await;
                    running_for_signal.store(false, Ordering::SeqCst);
                    println!("\n收到退出信号，正在关闭...");
                }
            }
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        piper.reset_fps_stats();
        let mut fps_window_start = std::time::Instant::now();

        while running.load(Ordering::SeqCst) {
            let joint_pos: JointPositionState = piper.get_joint_position();
            let end_pose: EndPoseState = piper.get_end_pose();
            let dynamics: JointDynamicState = piper.get_joint_dynamic();
            let control: RobotControlState = piper.get_robot_control();
            let gripper: GripperState = piper.get_gripper();

            if fps_window_start.elapsed().as_secs_f64() >= 1.0 {
                let fps: FpsResult = piper.get_fps();
                fps_window_start = std::time::Instant::now();

                print!("\x1B[2J\x1B[1;1H");
                println!("════════════════════════════════════════════════════════════════");
                println!("  Piper Robot Monitor");
                println!("════════════════════════════════════════════════════════════════");
                println!();

                println!("📍 Joint Positions:");
                for (index, position) in joint_pos.joint_pos.iter().enumerate() {
                    println!(
                        "  J{}: {:>8.3} rad ({:>6.1}°)",
                        index + 1,
                        position,
                        (*position).to_degrees()
                    );
                }

                println!();
                println!("🌀 Joint Dynamics:");
                for (index, velocity) in dynamics.joint_vel.iter().enumerate() {
                    println!(
                        "  J{}: vel={:>7.3} rad/s current={:>7.3} A",
                        index + 1,
                        velocity,
                        dynamics.joint_current[index]
                    );
                }

                println!();
                println!("📌 End Pose:");
                println!(
                    "  X={:>7.4} Y={:>7.4} Z={:>7.4}",
                    end_pose.end_pose[0], end_pose.end_pose[1], end_pose.end_pose[2]
                );
                println!(
                    "  Rx={:>7.4} Ry={:>7.4} Rz={:>7.4}",
                    end_pose.end_pose[3], end_pose.end_pose[4], end_pose.end_pose[5]
                );

                println!();
                println!("🤖 Control State:");
                println!("  Control mode: {}", control.control_mode);
                println!("  Robot status: {}", control.robot_status);
                println!("  Move mode: {}", control.move_mode);
                println!("  Motion status: {}", control.motion_status);
                println!("  Enabled: {}", control.is_enabled);

                println!();
                println!("🦾 Gripper:");
                println!(
                    "  Travel={:.3} mm Torque={:.3} Nm",
                    gripper.travel, gripper.torque
                );
                println!("  Status code={:#04x}", gripper.status_code);

                println!();
                println!("📈 FPS:");
                println!(
                    "  Position={:.1} Dynamics={:.1} EndPose={:.1} RobotControl={:.1} Gripper={:.1}",
                    fps.joint_position,
                    fps.joint_dynamic,
                    fps.end_pose,
                    fps.robot_control,
                    fps.gripper
                );
                println!();
                println!("按 Ctrl+C 停止");
            }

            tokio::time::sleep(tokio::time::Duration::from_millis(
                (1000 / frequency.max(1)) as u64,
            ))
            .await;
        }

        println!("✅ 已停止监控");
        Ok(())
    }
}
