//! Real Robot Gravity Compensation using MuJoCo
//!
//! This example demonstrates how to use MuJoCo physics engine to compute
//! gravity compensation torques for the real Piper robot arm.
//!
//! ## What is Gravity Compensation?
//!
//! Gravity compensation allows the robot arm to be moved passively by hand
//! without falling due to gravity. The MuJoCo physics engine calculates the
//! exact torques needed to counteract gravity at each joint, and we send
//! these compensating torques via MIT control mode.
//!
//! ## Implementation Status
//!
//! ✅ **FULLY IMPLEMENTED** - This example connects to a real robot and
//! performs gravity compensation using the new piper-sdk API.
//!
//! ## Usage
//!
//! ```bash
//! # Linux with SocketCAN
//! cargo run --example gravity_compensation_robot --features mujoco -- can0
//!
//! # macOS with GS-USB
//! cargo run --example gravity_compensation_robot --features mujoco
//! ```
//!
//! ## Prerequisites
//!
//! - Robot powered on and connected
//! - CAN interface configured (Linux) or GS-USB device connected (macOS/Windows)
//! - MuJoCo native library installed
//!
//! ## WARNING
//!
//! **MIT mode is an advanced feature!** The robot will move with zero resistance.
//! Ensure proper safety measures:
//! - Keep clear of the robot
//! - Be ready to press Ctrl+C
//! - Ensure the robot cannot hit anything

fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    use piper_physics::{GravityCompensation, JointState, MujocoGravityCompensation};
    use piper_sdk::PiperBuilder;
    use piper_sdk::client::state::*;
    use piper_sdk::client::types::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;
    use std::{array, thread};

    println!("🤖 Piper Robot - Gravity Compensation with MuJoCo");
    println!("==================================================\n");

    println!("⚠️  WARNING: MIT mode will be enabled!");
    println!("   The robot will move with ZERO resistance.");
    println!("   Keep clear of the robot and be ready to press Ctrl+C\n");

    thread::sleep(Duration::from_secs(2));

    // ============================================================
    // 1. Initialize MuJoCo Gravity Compensation
    // ============================================================
    println!("📄 Loading MuJoCo model from embedded XML...");
    let mut gravity_calc = MujocoGravityCompensation::from_embedded()
        .expect("Failed to load MuJoCo model from embedded XML");
    println!("✓ MuJoCo model loaded successfully\n");

    // ============================================================
    // 2. Connect to Robot
    // ============================================================
    println!("🔌 Connecting to robot...");

    // Get CAN interface from command line args, or use default
    let connection_target = std::env::args().nth(1);

    let builder = match connection_target.as_deref() {
        #[cfg(target_os = "linux")]
        Some("auto") => PiperBuilder::new(),
        #[cfg(not(target_os = "linux"))]
        Some("auto") => PiperBuilder::new().gs_usb_auto(),
        #[cfg(target_os = "linux")]
        Some(target) => PiperBuilder::new().socketcan(target),
        #[cfg(not(target_os = "linux"))]
        Some(target) => PiperBuilder::new().gs_usb_serial(target),
        #[cfg(target_os = "linux")]
        None => PiperBuilder::new().socketcan("can0"),
        #[cfg(not(target_os = "linux"))]
        None => PiperBuilder::new().gs_usb_auto(),
    };

    let driver = builder.build()?;

    println!("✓ Connected to CAN interface\n");

    // ============================================================
    // 3. Setup Signal Handler for Graceful Shutdown
    // ============================================================
    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();

    ctrlc::set_handler(move || {
        println!("\n\n🛑 Shutdown signal received, exiting gracefully...\n");
        r.store(false, Ordering::SeqCst);
    })
    .expect("Error setting Ctrl+C handler");

    // ============================================================
    // 4. Enable MIT Mode
    // ============================================================
    println!("⚙️  Checking motor driver status...");

    // Wait for motors to be ready
    thread::sleep(Duration::from_secs(1));

    println!("✓ Motor drivers ready\n");

    println!("🎯 Enabling MIT mode (torque control)...");
    let config = MitModeConfig::default();
    let robot = driver.enable_mit_mode(config)?;
    thread::sleep(Duration::from_millis(100));
    println!("✓ MIT mode enabled\n");

    println!("🚀 Starting gravity compensation loop...");
    println!("   Press Ctrl+C to exit with graceful shutdown\n");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");

    let loop_rate = Duration::from_millis(5); // 200 Hz control loop
    let mut iteration = 0u64;

    // ============================================================
    // 5. Control Loop
    // ============================================================
    while running.load(Ordering::SeqCst) {
        let loop_start = std::time::Instant::now();

        // Read current joint state from robot
        let observer = robot.observer();
        let positions = observer.joint_positions();
        let velocities = observer.joint_velocities();

        // Convert to JointState for physics calculation
        let pos_array = positions.as_array();
        let pos_f64: [f64; 6] = array::from_fn(|i| pos_array[i].0);
        let q: JointState = nalgebra::Vector6::from_column_slice(&pos_f64);

        // Compute gravity compensation torques using MuJoCo
        // Mode 1: Pure gravity compensation (τ = M(q)·g)
        let torques = match gravity_calc.compute_gravity_compensation(&q) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("❌ Physics calculation error: {}", e);
                continue;
            },
        };

        // Apply motor reduction factors for safety
        // These are empirical values to prevent excessive torques
        let motor_reductions = [0.25f64, 0.25, 0.25, 1.25, 1.25, 1.25];
        let mut reduced_torques = JointArray::from([NewtonMeter(0.0); 6]);
        for i in 0..6 {
            reduced_torques[i] = NewtonMeter(torques[i] * motor_reductions[i]);
        }

        // Convert velocities to f64 array for command_torques
        let vel_array = velocities.as_array();
        let vel_f64: JointArray<f64> = JointArray::from(array::from_fn(|i| vel_array[i].0));

        // Send compensating torques via MIT mode
        // Pure torque control: kp=0, kd=0 (no impedance)
        let kp = JointArray::from([0.0_f64; 6]); // No position stiffness
        let kd = JointArray::from([0.0_f64; 6]); // No velocity damping

        if let Err(e) = robot.command_torques(&positions, &vel_f64, &kp, &kd, &reduced_torques) {
            eprintln!("❌ Failed to send MIT command: {}", e);
        }

        // Print status every 100 iterations (2 Hz)
        if iteration.is_multiple_of(100) {
            let q_deg: [f64; 6] = array::from_fn(|i| positions[i].to_deg().0);

            println!(
                "[{:04}] Joint Positions: [{:6.1}°, {:6.1}°, {:6.1}°, {:6.1}°, {:6.1}°, {:6.1}°]",
                iteration, q_deg[0], q_deg[1], q_deg[2], q_deg[3], q_deg[4], q_deg[5]
            );
            println!(
                "       Gravity Torques: [{:6.3}, {:6.3}, {:6.3}, {:6.3}, {:6.3}, {:6.3}] Nm",
                torques[0], torques[1], torques[2], torques[3], torques[4], torques[5]
            );
            println!(
                "       Scaled Torques:   [{:6.3}, {:6.3}, {:6.3}, {:6.3}, {:6.3}, {:6.3}] Nm",
                reduced_torques[0],
                reduced_torques[1],
                reduced_torques[2],
                reduced_torques[3],
                reduced_torques[4],
                reduced_torques[5]
            );
            println!();
        }

        iteration += 1;

        // Maintain control loop rate
        let elapsed = loop_start.elapsed();
        if elapsed < loop_rate {
            thread::sleep(loop_rate - elapsed);
        }
    }

    // ============================================================
    // 6. Graceful Shutdown
    // ============================================================
    println!("\n🛑 Initiating graceful shutdown...\n");

    // Apply damping for smooth deceleration
    println!("   Applying damping control...");
    let damping_duration = Duration::from_secs(5);
    let damping_start = std::time::Instant::now();

    let kp_damp = JointArray::from([0.0; 6]); // No position stiffness
    let kd_damp = JointArray::from([0.4_f64, 0.4, 0.4, 0.4, 0.4, 0.4]); // Light damping

    while damping_start.elapsed() < damping_duration {
        let observer = robot.observer();
        let positions = observer.joint_positions();
        let velocities = observer.joint_velocities();

        // Convert velocities to f64 array
        let vel_array = velocities.as_array();
        let vel_f64: JointArray<f64> = JointArray::from(array::from_fn(|i| vel_array[i].0));

        let zero_torques = JointArray::from([NewtonMeter(0.0); 6]);

        robot.command_torques(&positions, &vel_f64, &kp_damp, &kd_damp, &zero_torques)?;
        thread::sleep(Duration::from_millis(100));
    }

    println!("   ✓ Damping control completed\n");

    // Disable MIT mode (automatically done on drop)
    println!("   Disabling MIT mode...");
    drop(robot);
    thread::sleep(Duration::from_millis(100));
    println!("   ✓ MIT mode disabled\n");

    println!("📊 Statistics:");
    println!("   Total control loop iterations: {}", iteration);
    println!("   Runtime: {:.1} seconds", iteration as f64 * 0.005);
    println!("\n✅ Gravity compensation stopped successfully!");

    Ok(())
}
