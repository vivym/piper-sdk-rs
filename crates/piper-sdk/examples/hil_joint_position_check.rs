use clap::Parser;
use piper_sdk::PiperBuilder;
use piper_sdk::client::state::machine::MotionType;
use piper_sdk::client::state::{
    ConnectedPiper, MotionCapability, Piper, PositionModeConfig, Standby,
};
use piper_sdk::client::types::{JointArray, Rad, Result as ClientResult, RobotError};
use piper_sdk::client::{MotionConnectedPiper, MotionConnectedState};
use std::error::Error;
use std::time::{Duration, Instant};

const MAX_SPEED_PERCENT: u8 = 10;
const MAX_DELTA_RAD: f64 = 0.035;
const DEFAULT_SETTLE_TIMEOUT_MS: u64 = 10_000;
const POSITION_SETTLE_TOLERANCE_RAD: f64 = 0.05;
const INITIAL_MONITOR_SNAPSHOT_TIMEOUT: Duration = Duration::from_millis(200);
const INITIAL_MONITOR_SNAPSHOT_POLL_INTERVAL: Duration = Duration::from_millis(5);
const POSITION_SETTLE_POLL_INTERVAL: Duration = Duration::from_millis(10);

#[derive(Parser, Debug)]
#[command(name = "hil_joint_position_check")]
#[command(about = "Safe joint position HIL helper for one real Piper arm")]
struct Args {
    /// Linux: SocketCAN interface; macOS/Windows: GS-USB serial.
    #[cfg_attr(target_os = "linux", arg(long, default_value = "can0"))]
    #[cfg_attr(not(target_os = "linux"), arg(long))]
    interface: String,

    /// CAN baud rate.
    #[arg(long, default_value_t = 1_000_000)]
    baud_rate: u32,

    /// Joint to move, numbered 1 through 6.
    #[arg(long)]
    joint: u8,

    /// Signed joint delta in radians.
    #[arg(long)]
    delta_rad: f64,

    /// Position-mode speed percentage.
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
            run_position_check(robot, args)
        },
        MotionConnectedPiper::Soft(MotionConnectedState::Standby(robot)) => {
            println!("[PASS] connected and confirmed Standby");
            run_position_check(robot, args)
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

fn run_position_check<Capability>(
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

    println!(
        "[PASS] initial snapshot joint=J{} position_rad={:.6}",
        args.joint, initial_joint.0
    );
    println!(
        "[PASS] command step=move joint=J{} target_rad={:.6} delta_rad={:.6} speed_percent={}",
        args.joint, target_joint.0, args.delta_rad, args.speed_percent
    );

    robot.send_position_command(&target_positions)?;

    let moved_positions = wait_for_joint_settle(
        observer,
        joint_index,
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

    robot.send_position_command(&initial_positions)?;
    println!(
        "[PASS] command step=return joint=J{} target_rad={:.6}",
        args.joint, initial_joint.0
    );

    let returned_positions = wait_for_joint_settle(
        observer,
        joint_index,
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
    println!("[PASS] hil_joint_position_check complete");

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
    target_joint: Rad,
    timeout: Duration,
    poll_interval: Duration,
) -> ClientResult<JointArray<Rad>>
where
    Capability: MotionCapability,
{
    let start = Instant::now();
    loop {
        if start.elapsed() >= timeout {
            return Err(RobotError::Timeout {
                timeout_ms: timeout.as_millis() as u64,
            });
        }

        let remaining = timeout.saturating_sub(start.elapsed());
        let positions =
            wait_for_monitor_snapshot(remaining, poll_interval, || observer.joint_positions())?;

        if (positions[joint_index].0 - target_joint.0).abs() <= POSITION_SETTLE_TOLERANCE_RAD {
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
    if args.speed_percent > MAX_SPEED_PERCENT {
        return Err("speed_percent must be <= 10 for manual HIL".to_string());
    }
    if args.delta_rad.abs() > MAX_DELTA_RAD {
        return Err("delta_rad must be <= 0.035 rad for manual HIL".to_string());
    }
    if args.settle_timeout_ms == 0 {
        return Err("settle_timeout_ms must be > 0".to_string());
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_args_rejects_excessive_speed() {
        let args = Args {
            interface: "can0".to_string(),
            baud_rate: 1_000_000,
            joint: 1,
            delta_rad: 0.02,
            speed_percent: 11,
            settle_timeout_ms: 10_000,
        };

        let error = validate_args(&args).expect_err("speed > 10 must be rejected");
        assert!(error.contains("speed_percent"));
    }

    #[test]
    fn validate_args_rejects_excessive_delta() {
        let args = Args {
            interface: "can0".to_string(),
            baud_rate: 1_000_000,
            joint: 1,
            delta_rad: 0.04,
            speed_percent: 10,
            settle_timeout_ms: 10_000,
        };

        let error = validate_args(&args).expect_err("delta > 0.035 rad must be rejected");
        assert!(error.contains("delta_rad"));
    }
}
