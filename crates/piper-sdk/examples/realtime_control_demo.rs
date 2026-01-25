//! Real-time Control Demo
//!
//! This example demonstrates how to use dual-threaded architecture
//! for high-frequency control (500Hz-1kHz) with command priority scheduling.
//!
//! Features demonstrated:
//! - Dual-threaded IO architecture
//! - Real-time vs reliable command sending
//! - Performance metrics monitoring
//! - Thread health checking

use piper_sdk::can::PiperFrame;
use piper_sdk::driver::{PiperBuilder, PiperCommand};
use std::time::{Duration, Instant};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize logging (optional, requires tracing-subscriber dependency)
    // tracing_subscriber::fmt::init();

    println!("=== Real-time Control Demo ===\n");

    // Create Piper instance with dual-threaded mode
    println!("1. Creating Piper instance with dual-threaded mode...");
    // Note: Dual-threaded mode is automatically enabled when the adapter supports splitting
    let robot = PiperBuilder::new()
        .interface("can0")  // Linux: SocketCAN interface name
        .baud_rate(1_000_000)  // CAN baud rate
        .build()?;
    println!("   ✓ Piper instance created (dual-threaded if supported)\n");

    // Check thread health
    println!("2. Checking thread health...");
    if robot.is_healthy() {
        println!("   ✓ All threads are running normally\n");
    } else {
        let (rx_alive, tx_alive) = robot.check_health();
        if !rx_alive {
            eprintln!("   ✗ RX thread has stopped!");
        }
        if !tx_alive {
            eprintln!("   ✗ TX thread has stopped!");
        }
        return Err("Thread health check failed".into());
    }

    // Monitor performance metrics
    println!("3. Monitoring performance metrics...");
    let start_metrics = robot.get_metrics();
    println!("   Initial metrics:");
    println!("     - RX frames total: {}", start_metrics.rx_frames_total);
    println!("     - TX frames total: {}", start_metrics.tx_frames_total);
    println!("     - RX timeouts: {}", start_metrics.rx_timeouts);
    println!("     - TX timeouts: {}", start_metrics.tx_timeouts);
    println!();

    // Simulate high-frequency control loop (500Hz)
    println!("4. Running high-frequency control loop (500Hz) for 5 seconds...");
    let control_duration = Duration::from_secs(5);
    let control_interval = Duration::from_millis(2); // 500Hz = 2ms
    let start_time = Instant::now();
    let mut iteration_count = 0u32;

    while start_time.elapsed() < control_duration {
        let loop_start = Instant::now();

        // Read current state (lock-free, nanosecond-level)
        let joint_pos = robot.get_joint_position();
        let joint_dynamic = robot.get_joint_dynamic();

        // Simulate control computation
        // In real application, you would compute control commands here
        let _control_output = compute_control(&joint_pos, &joint_dynamic);

        // Send real-time control command (overwritable)
        let frame = PiperFrame::new_standard(
            0x1A1, // Example control frame ID
            &[iteration_count as u8; 8],
        );
        match robot.send_realtime(frame) {
            Ok(_) => {},
            Err(e) => {
                eprintln!("   Warning: Failed to send realtime command: {}", e);
            },
        }

        iteration_count += 1;

        // Maintain 500Hz frequency
        let elapsed = loop_start.elapsed();
        if elapsed < control_interval {
            std::thread::sleep(control_interval - elapsed);
        }
    }

    println!(
        "   ✓ Control loop completed ({} iterations)\n",
        iteration_count
    );

    // Check final metrics
    println!("5. Final performance metrics:");
    let end_metrics = robot.get_metrics();
    println!(
        "     - RX frames total: {} (+{})",
        end_metrics.rx_frames_total,
        end_metrics.rx_frames_total - start_metrics.rx_frames_total
    );
    println!(
        "     - TX frames total: {} (+{})",
        end_metrics.tx_frames_total,
        end_metrics.tx_frames_total - start_metrics.tx_frames_total
    );
    println!(
        "     - RX timeouts: {} (+{})",
        end_metrics.rx_timeouts,
        end_metrics.rx_timeouts - start_metrics.rx_timeouts
    );
    println!(
        "     - TX timeouts: {} (+{})",
        end_metrics.tx_timeouts,
        end_metrics.tx_timeouts - start_metrics.tx_timeouts
    );
    println!(
        "     - Realtime overwrites: {}",
        end_metrics.tx_realtime_overwrites
    );
    println!("     - Reliable drops: {}", end_metrics.tx_reliable_drops);
    println!();

    // Demonstrate command priority
    println!("6. Demonstrating command priority...");

    // Send reliable command (configuration)
    let config_frame = PiperFrame::new_standard(0x1A2, &[0x01, 0x02, 0x03, 0x04]);
    match robot.send_reliable(config_frame) {
        Ok(_) => println!("   ✓ Reliable command sent (FIFO queue)"),
        Err(e) => eprintln!("   ✗ Failed to send reliable command: {}", e),
    }

    // Send real-time command using PiperCommand
    let control_frame = PiperFrame::new_standard(0x1A1, &[0x05, 0x06, 0x07, 0x08]);
    let cmd = PiperCommand::realtime(control_frame);
    match robot.send_command(cmd) {
        Ok(_) => println!("   ✓ Real-time command sent (overwritable queue)"),
        Err(e) => eprintln!("   ✗ Failed to send real-time command: {}", e),
    }
    println!();

    // Final health check
    println!("7. Final thread health check...");
    if robot.is_healthy() {
        println!("   ✓ All threads are still running normally");
    } else {
        eprintln!("   ✗ Thread health check failed!");
    }

    println!("\n=== Demo completed successfully ===");
    Ok(())
}

/// Simulate control computation
fn compute_control(
    _joint_pos: &piper_sdk::driver::JointPositionState,
    _joint_dynamic: &piper_sdk::driver::JointDynamicState,
) -> [f64; 6] {
    // In real application, this would compute control commands
    // based on current state and desired trajectory
    [0.0; 6]
}
