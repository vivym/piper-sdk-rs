//! Real robot gravity compensation using the reusable runner

fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    use piper_physics::{
        GravityCompensationRunStats, GravityCompensationRunner, GravityCompensationRunnerConfig,
        MujocoGravityCompensation,
    };
    use piper_sdk::{ConnectedPiper, PiperBuilder};
    use piper_sdk::client::state::MitModeConfig;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread;
    use std::time::Duration;

    fn print_stats(stats: GravityCompensationRunStats) {
        println!("Control loop iterations: {}", stats.iterations);
        println!("Runtime: {:.3} s", stats.runtime.as_secs_f64());
    }

    println!("Piper Robot - Gravity Compensation with MuJoCo");
    println!("WARNING: MIT mode will be enabled and the arm will become backdrivable.");
    thread::sleep(Duration::from_secs(2));

    println!("Loading MuJoCo model...");
    let mut gravity = MujocoGravityCompensation::from_embedded()?;
    println!("MuJoCo model ready.");

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

    println!("Connecting to robot...");
    let standby = builder.build()?;
    println!("Connected.");

    let running = Arc::new(AtomicBool::new(true));
    let signal = running.clone();
    ctrlc::set_handler(move || {
        signal.store(false, Ordering::SeqCst);
    })?;

    println!("Enabling MIT mode...");
    let robot = match standby {
        ConnectedPiper::Strict(standby) => standby.enable_mit_mode(MitModeConfig::default())?,
        ConnectedPiper::Soft(_) | ConnectedPiper::Monitor(_) => {
            return Err("gravity compensation requires a strict-realtime backend".into());
        },
    };
    thread::sleep(Duration::from_millis(100));
    println!("MIT mode enabled.");

    let config = GravityCompensationRunnerConfig::default();
    println!(
        "Starting gravity compensation loop at {:.1} Hz.",
        1.0 / config.loop_period.as_secs_f64()
    );
    println!("Torque safety scale: {:?}", config.torque_safety_scale);

    let mut runner = GravityCompensationRunner::new(&robot, &mut gravity, config);
    let result = runner.run_until_stopped(|| running.load(Ordering::SeqCst));

    println!("Dropping MIT session...");
    drop(robot);
    thread::sleep(Duration::from_millis(100));

    match result {
        Ok(stats) => {
            print_stats(stats);
            println!("Gravity compensation stopped cleanly.");
            Ok(())
        },
        Err(failure) => {
            eprintln!("Gravity compensation stopped with error: {}", failure.error);
            print_stats(failure.stats);
            Err(Box::new(failure))
        },
    }
}
