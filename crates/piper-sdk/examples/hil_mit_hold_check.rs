use clap::Parser;
use piper_sdk::PiperBuilder;
use piper_sdk::client::state::{ConnectedPiper, MitModeConfig, Piper, Standby, StrictCapability};
use piper_sdk::client::types::{JointArray, NewtonMeter, Rad, Result as ClientResult, RobotError};
use piper_sdk::client::{MotionConnectedPiper, MotionConnectedState};
use std::error::Error;
use std::time::{Duration, Instant};

const MAX_SPEED_PERCENT: u8 = 10;
const MAX_DELTA_RAD: f64 = 0.02;
const DEFAULT_SETTLE_TIMEOUT_MS: u64 = 5_000;
const POSITION_SETTLE_TOLERANCE_RAD: f64 = 0.004;
const MIN_PROGRESS_RAD: f64 = 0.001;
const INITIAL_MONITOR_SNAPSHOT_TIMEOUT: Duration = Duration::from_millis(200);
const INITIAL_MONITOR_SNAPSHOT_POLL_INTERVAL: Duration = Duration::from_millis(5);
const POSITION_SETTLE_POLL_INTERVAL: Duration = Duration::from_millis(10);

#[derive(Parser, Debug)]
#[command(name = "hil_mit_hold_check")]
#[command(about = "Safe MIT-mode HIL helper for one real Piper arm")]
struct Args {
    /// Linux: SocketCAN interface; macOS/Windows: GS-USB serial.
    #[cfg_attr(target_os = "linux", arg(long, default_value = "can0"))]
    #[cfg_attr(not(target_os = "linux"), arg(long))]
    interface: String,

    /// CAN baud rate.
    #[arg(long, default_value_t = 1_000_000)]
    baud_rate: u32,

    /// Joint to bias, numbered 1 through 6.
    #[arg(long, default_value_t = 1)]
    joint: u8,

    /// Signed joint delta in radians.
    #[arg(long, default_value_t = 0.01)]
    delta_rad: f64,

    /// Proportional gain for the selected joint.
    #[arg(long, default_value_t = 5.0)]
    kp: f64,

    /// Derivative gain for the selected joint.
    #[arg(long, default_value_t = 0.5)]
    kd: f64,

    /// MIT mode speed percentage used during enable.
    #[arg(long, default_value_t = MAX_SPEED_PERCENT)]
    speed_percent: u8,

    /// Settle timeout for each commanded move, in milliseconds.
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
            run_mit_check(robot, args)
        },
        MotionConnectedPiper::Soft(MotionConnectedState::Standby(_)) => {
            Err("MIT helper requires strict realtime capability".into())
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

fn run_mit_check<Capability>(
    standby: Piper<Standby, Capability>,
    args: &Args,
) -> Result<(), Box<dyn Error>>
where
    Capability: StrictCapability,
{
    let robot = standby.enable_mit_mode(MitModeConfig {
        speed_percent: args.speed_percent,
        ..Default::default()
    })?;
    println!(
        "[PASS] enabled MitMode speed_percent={}",
        args.speed_percent
    );

    let observer = robot.observer();
    let initial_positions = wait_for_monitor_snapshot(
        INITIAL_MONITOR_SNAPSHOT_TIMEOUT,
        INITIAL_MONITOR_SNAPSHOT_POLL_INTERVAL,
        || observer.joint_positions(),
    )?;

    let joint_index = usize::from(args.joint - 1);
    let initial_joint = initial_positions[joint_index];
    let target_joint = Rad(initial_joint.0 + args.delta_rad);

    let mut target_positions = initial_positions;
    target_positions.as_array_mut()[joint_index] = target_joint;

    let velocities = JointArray::splat(0.0);
    let mut kp = JointArray::splat(0.0);
    kp.as_array_mut()[joint_index] = args.kp;
    let mut kd = JointArray::splat(0.0);
    kd.as_array_mut()[joint_index] = args.kd;
    let torques = JointArray::splat(NewtonMeter(0.0));

    println!(
        "[PASS] initial snapshot joint=J{} position_rad={:.6}",
        args.joint, initial_joint.0
    );
    println!(
        "[PASS] command step=move joint=J{} target_rad={:.6} delta_rad={:.6} kp={:.3} kd={:.3}",
        args.joint, target_joint.0, args.delta_rad, args.kp, args.kd
    );

    robot.command_torques(&target_positions, &velocities, &kp, &kd, &torques)?;
    let moved_positions = wait_for_joint_settle(
        observer,
        joint_index,
        initial_joint,
        target_joint,
        Duration::from_millis(args.settle_timeout_ms),
        POSITION_SETTLE_POLL_INTERVAL,
    )?;
    let moved_joint = moved_positions[joint_index];
    let observed_delta = moved_joint.0 - initial_joint.0;
    let move_error = (moved_joint.0 - target_joint.0).abs();

    println!(
        "[PASS] settle step=move joint=J{} observed_delta_rad={:.6} target_delta_rad={:.6} position_error_rad={:.6}",
        args.joint, observed_delta, args.delta_rad, move_error
    );

    robot.command_torques(&initial_positions, &velocities, &kp, &kd, &torques)?;
    println!(
        "[PASS] command step=return joint=J{} target_rad={:.6}",
        args.joint, initial_joint.0
    );

    let returned_positions = wait_for_joint_settle(
        observer,
        joint_index,
        target_joint,
        initial_joint,
        Duration::from_millis(args.settle_timeout_ms),
        POSITION_SETTLE_POLL_INTERVAL,
    )?;
    let returned_joint = returned_positions[joint_index];
    let return_error = (returned_joint.0 - initial_joint.0).abs();

    println!(
        "[PASS] settle step=return joint=J{} final_position_rad={:.6} return_error_rad={:.6} tolerance_rad={:.6}",
        args.joint, returned_joint.0, return_error, POSITION_SETTLE_TOLERANCE_RAD
    );
    println!("[PASS] hil_mit_hold_check complete");

    Ok(())
}

fn wait_for_monitor_snapshot<T, Read>(
    timeout: Duration,
    poll_interval: Duration,
    mut read: Read,
) -> ClientResult<T>
where
    Read: FnMut() -> ClientResult<T>,
{
    let start = Instant::now();
    loop {
        match read() {
            Ok(value) => return Ok(value),
            Err(
                RobotError::MonitorStateIncomplete { .. } | RobotError::MonitorStateStale { .. },
            ) => {},
            Err(other) => return Err(other),
        }

        if start.elapsed() >= timeout {
            return Err(RobotError::Timeout {
                timeout_ms: timeout.as_millis() as u64,
            });
        }

        let remaining = timeout.saturating_sub(start.elapsed());
        let sleep_duration = poll_interval.min(remaining);
        if sleep_duration.is_zero() {
            return Err(RobotError::Timeout {
                timeout_ms: timeout.as_millis() as u64,
            });
        }

        std::thread::sleep(sleep_duration);
    }
}

fn wait_for_joint_settle<Capability>(
    observer: &piper_sdk::client::observer::Observer<Capability>,
    joint_index: usize,
    start_joint: Rad,
    target_joint: Rad,
    timeout: Duration,
    poll_interval: Duration,
) -> ClientResult<JointArray<Rad>>
where
    Capability: StrictCapability,
{
    let start = Instant::now();
    let total_delta = (target_joint.0 - start_joint.0).abs();
    let progress_threshold = (total_delta * 0.25).max(MIN_PROGRESS_RAD).min(total_delta);
    let mut progress_seen = false;

    loop {
        if start.elapsed() >= timeout {
            return Err(RobotError::Timeout {
                timeout_ms: timeout.as_millis() as u64,
            });
        }

        let remaining = timeout.saturating_sub(start.elapsed());
        let positions =
            wait_for_monitor_snapshot(remaining, poll_interval, || observer.joint_positions())?;
        let observed_joint = positions[joint_index];
        let observed_delta = (observed_joint.0 - start_joint.0).abs();
        let remaining_error = (observed_joint.0 - target_joint.0).abs();

        if !progress_seen && observed_delta >= progress_threshold {
            progress_seen = true;
        }

        if progress_seen && remaining_error <= POSITION_SETTLE_TOLERANCE_RAD {
            return Ok(positions);
        }

        let sleep_duration = poll_interval.min(timeout.saturating_sub(start.elapsed()));
        if sleep_duration.is_zero() {
            return Err(RobotError::Timeout {
                timeout_ms: timeout.as_millis() as u64,
            });
        }

        std::thread::sleep(sleep_duration);
    }
}

fn validate_args(args: &Args) -> Result<(), String> {
    if !(1..=6).contains(&args.joint) {
        return Err("joint must be between 1 and 6".to_string());
    }
    if !args.delta_rad.is_finite() {
        return Err("delta_rad must be finite for manual HIL".to_string());
    }
    if args.speed_percent == 0 || args.speed_percent > MAX_SPEED_PERCENT {
        return Err("speed_percent must be between 1 and 10 for manual HIL".to_string());
    }
    if args.delta_rad.abs() > MAX_DELTA_RAD {
        return Err("delta_rad must be <= 0.02 rad for manual HIL".to_string());
    }
    if !args.kp.is_finite() || args.kp <= 0.0 {
        return Err("kp must be finite and > 0".to_string());
    }
    if !args.kd.is_finite() || args.kd < 0.0 {
        return Err("kd must be finite and >= 0".to_string());
    }
    if args.settle_timeout_ms == 0 {
        return Err("settle_timeout_ms must be > 0".to_string());
    }

    Ok(())
}

#[test]
fn validate_args_rejects_invalid_joint() {
    let args = Args {
        interface: "can0".to_string(),
        baud_rate: 1_000_000,
        joint: 0,
        delta_rad: 0.01,
        kp: 5.0,
        kd: 0.5,
        speed_percent: 10,
        settle_timeout_ms: 5_000,
    };

    let error = validate_args(&args).expect_err("joint outside 1..=6 must be rejected");
    assert!(error.contains("joint"));
}

#[test]
fn validate_args_rejects_non_positive_kp() {
    let args = Args {
        interface: "can0".to_string(),
        baud_rate: 1_000_000,
        joint: 1,
        delta_rad: 0.01,
        kp: 0.0,
        kd: 0.5,
        speed_percent: 10,
        settle_timeout_ms: 5_000,
    };

    let error = validate_args(&args).expect_err("kp <= 0 must be rejected");
    assert!(error.contains("kp"));
}
