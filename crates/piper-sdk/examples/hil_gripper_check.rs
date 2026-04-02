use clap::Parser;
use piper_sdk::PiperBuilder;
use piper_sdk::client::state::machine::MotionType;
use piper_sdk::client::state::{
    ConnectedPiper, MotionCapability, Piper, PositionModeConfig, Standby,
};
use piper_sdk::client::{MotionConnectedPiper, MotionConnectedState};
use std::error::Error;
use std::time::{Duration, Instant};

const MAX_SPEED_PERCENT: u8 = 10;
const DEFAULT_SETTLE_TIMEOUT_MS: u64 = 5_000;
const DEFAULT_CLOSE_EFFORT: f64 = 0.3;
const GRIPPER_POSITION_TOLERANCE: f64 = 0.08;
const MIN_GRIPPER_PROGRESS: f64 = 0.05;
const GRIPPER_POLL_INTERVAL: Duration = Duration::from_millis(20);

#[derive(Parser, Debug)]
#[command(name = "hil_gripper_check")]
#[command(about = "Safe gripper HIL helper for one real Piper arm")]
struct Args {
    /// Linux: SocketCAN interface; macOS/Windows: GS-USB serial.
    #[cfg_attr(target_os = "linux", arg(long, default_value = "can0"))]
    #[cfg_attr(not(target_os = "linux"), arg(long))]
    interface: String,

    /// CAN baud rate.
    #[arg(long, default_value_t = 1_000_000)]
    baud_rate: u32,

    /// Joint-position mode speed percentage used while enabling the arm.
    #[arg(long, default_value_t = MAX_SPEED_PERCENT)]
    speed_percent: u8,

    /// Closing effort in [0.0, 1.0].
    #[arg(long, default_value_t = DEFAULT_CLOSE_EFFORT)]
    close_effort: f64,

    /// Settle timeout for each gripper command, in milliseconds.
    #[arg(long, default_value_t = DEFAULT_SETTLE_TIMEOUT_MS)]
    settle_timeout_ms: u64,
}

fn main() -> Result<(), Box<dyn Error>> {
    piper_sdk::init_logger!();

    let args = Args::parse();
    if let Err(error) = validate_args(&args) {
        eprintln!("[FAIL] validation error: {error}");
        return Err(error.into());
    }

    if let Err(error) = run(&args) {
        eprintln!("[FAIL] {error}");
        return Err(error);
    }

    Ok(())
}

fn run(args: &Args) -> Result<(), Box<dyn Error>> {
    let connected = build_connected(args)?;

    match connected.require_motion()? {
        MotionConnectedPiper::Strict(MotionConnectedState::Standby(robot)) => {
            println!("[PASS] connected and confirmed Standby");
            run_gripper_check(robot, args)
        },
        MotionConnectedPiper::Soft(MotionConnectedState::Standby(robot)) => {
            println!("[PASS] connected and confirmed Standby");
            run_gripper_check(robot, args)
        },
        MotionConnectedPiper::Strict(MotionConnectedState::Maintenance(_))
        | MotionConnectedPiper::Soft(MotionConnectedState::Maintenance(_)) => {
            Err("robot is not in confirmed Standby; run stop first".into())
        },
    }
}

fn build_connected(args: &Args) -> Result<ConnectedPiper, Box<dyn Error>> {
    #[cfg(target_os = "linux")]
    {
        Ok(PiperBuilder::new()
            .socketcan(&args.interface)
            .baud_rate(args.baud_rate)
            .build()?)
    }
    #[cfg(not(target_os = "linux"))]
    {
        Ok(PiperBuilder::new()
            .gs_usb_serial(&args.interface)
            .baud_rate(args.baud_rate)
            .build()?)
    }
}

fn run_gripper_check<Capability>(
    standby: Piper<Standby, Capability>,
    args: &Args,
) -> Result<(), Box<dyn Error>>
where
    Capability: MotionCapability,
{
    let robot = standby.enable_position_mode(PositionModeConfig {
        speed_percent: args.speed_percent,
        motion_type: MotionType::Joint,
        ..Default::default()
    })?;
    println!(
        "[PASS] enabled PositionMode motion=Joint speed_percent={}",
        args.speed_percent
    );

    let observer = robot.observer();
    let initial = observer.gripper_state();
    println!(
        "[PASS] initial snapshot position={:.3} effort={:.3} enabled={}",
        initial.position, initial.effort, initial.enabled
    );

    robot.open_gripper()?;
    println!("[PASS] command step=open effort=0.300");
    let opened = wait_for_gripper_target(
        observer,
        initial.position,
        1.0,
        Duration::from_millis(args.settle_timeout_ms),
    )?;
    println!(
        "[PASS] settle step=open position={:.3} progress={:.3}",
        opened.position,
        (opened.position - initial.position).abs()
    );

    robot.close_gripper(args.close_effort)?;
    println!("[PASS] command step=close effort={:.3}", args.close_effort);
    let closed = wait_for_gripper_target(
        observer,
        opened.position,
        0.0,
        Duration::from_millis(args.settle_timeout_ms),
    )?;
    println!(
        "[PASS] settle step=close position={:.3} progress={:.3}",
        closed.position,
        (closed.position - opened.position).abs()
    );

    robot.open_gripper()?;
    println!("[PASS] command step=reopen effort=0.300");
    let reopened = wait_for_gripper_target(
        observer,
        closed.position,
        1.0,
        Duration::from_millis(args.settle_timeout_ms),
    )?;
    println!(
        "[PASS] settle step=reopen position={:.3} progress={:.3} tolerance={:.3}",
        reopened.position,
        (reopened.position - closed.position).abs(),
        GRIPPER_POSITION_TOLERANCE
    );
    println!("[PASS] hil_gripper_check complete");

    Ok(())
}

fn validate_args(args: &Args) -> Result<(), String> {
    if args.speed_percent == 0 || args.speed_percent > MAX_SPEED_PERCENT {
        return Err("speed_percent must be between 1 and 10 for manual HIL".to_string());
    }
    if !args.close_effort.is_finite() || !(0.0..=1.0).contains(&args.close_effort) {
        return Err("close_effort must be in [0.0, 1.0]".to_string());
    }
    if args.settle_timeout_ms == 0 {
        return Err("settle_timeout_ms must be > 0".to_string());
    }

    Ok(())
}

fn gripper_progress_threshold(total_delta: f64) -> f64 {
    (total_delta * 0.25).max(MIN_GRIPPER_PROGRESS).min(total_delta.abs())
}

fn wait_for_gripper_target<Capability>(
    observer: &piper_sdk::client::observer::Observer<Capability>,
    start_position: f64,
    target_position: f64,
    timeout: Duration,
) -> Result<piper_sdk::client::observer::GripperState, Box<dyn Error>>
where
    Capability: MotionCapability,
{
    let start = Instant::now();
    let total_delta = (target_position - start_position).abs();
    let progress_threshold = gripper_progress_threshold(total_delta);
    let mut progress_seen = total_delta <= GRIPPER_POSITION_TOLERANCE;

    loop {
        if start.elapsed() >= timeout {
            return Err(
                format!("gripper command timed out after {}ms", timeout.as_millis()).into(),
            );
        }

        let state = observer.gripper_state();
        let observed_delta = (state.position - start_position).abs();
        let remaining_error = (state.position - target_position).abs();

        if !progress_seen && observed_delta >= progress_threshold {
            progress_seen = true;
        }

        if progress_seen && remaining_error <= GRIPPER_POSITION_TOLERANCE {
            return Ok(state);
        }

        std::thread::sleep(GRIPPER_POLL_INTERVAL);
    }
}

#[test]
fn validate_args_rejects_invalid_close_effort() {
    let args = Args {
        interface: "can0".to_string(),
        baud_rate: 1_000_000,
        speed_percent: 10,
        close_effort: 1.5,
        settle_timeout_ms: 5_000,
    };

    let error = validate_args(&args).expect_err("close effort above 1.0 must be rejected");
    assert!(error.contains("close_effort"));
}

#[test]
fn gripper_progress_threshold_has_floor() {
    let threshold = gripper_progress_threshold(0.10);
    assert!(threshold >= MIN_GRIPPER_PROGRESS);
    assert!(threshold > 0.0);
}
