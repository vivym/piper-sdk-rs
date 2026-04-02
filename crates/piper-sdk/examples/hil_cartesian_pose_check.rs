use clap::Parser;
use piper_sdk::PiperBuilder;
use piper_sdk::client::state::machine::MotionType;
use piper_sdk::client::state::{
    ConnectedPiper, MotionCapability, Piper, PositionModeConfig, Standby,
};
use piper_sdk::client::types::{EulerAngles, Position3D, Result as ClientResult, RobotError};
use piper_sdk::client::{MotionConnectedPiper, MotionConnectedState};
use std::error::Error;
use std::time::{Duration, Instant};

const MAX_SPEED_PERCENT: u8 = 10;
const MAX_TRANSLATION_DELTA_M: f64 = 0.02;
const DEFAULT_SETTLE_TIMEOUT_MS: u64 = 10_000;
const POSITION_SETTLE_TOLERANCE_M: f64 = 0.003;
const MIN_PROGRESS_M: f64 = 0.001;
const INITIAL_MONITOR_SNAPSHOT_TIMEOUT: Duration = Duration::from_millis(200);
const INITIAL_MONITOR_SNAPSHOT_POLL_INTERVAL: Duration = Duration::from_millis(5);
const POSITION_SETTLE_POLL_INTERVAL: Duration = Duration::from_millis(10);

#[derive(Parser, Debug)]
#[command(name = "hil_cartesian_pose_check")]
#[command(about = "Safe Cartesian position HIL helper for one real Piper arm")]
struct Args {
    /// Linux: SocketCAN interface; macOS/Windows: GS-USB serial.
    #[cfg_attr(target_os = "linux", arg(long, default_value = "can0"))]
    #[cfg_attr(not(target_os = "linux"), arg(long))]
    interface: String,

    /// CAN baud rate.
    #[arg(long, default_value_t = 1_000_000)]
    baud_rate: u32,

    /// Signed X translation delta in meters.
    #[arg(long, default_value_t = 0.01)]
    delta_x: f64,

    /// Signed Y translation delta in meters.
    #[arg(long, default_value_t = 0.0)]
    delta_y: f64,

    /// Signed Z translation delta in meters.
    #[arg(long, default_value_t = 0.0)]
    delta_z: f64,

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
            run_cartesian_check(robot, args)
        },
        MotionConnectedPiper::Soft(MotionConnectedState::Standby(robot)) => {
            println!("[PASS] connected and confirmed Standby");
            run_cartesian_check(robot, args)
        },
        MotionConnectedPiper::Strict(MotionConnectedState::Maintenance(_))
        | MotionConnectedPiper::Soft(MotionConnectedState::Maintenance(_)) => {
            Err("robot is not in confirmed Standby; run stop first".into())
        },
    }
}

#[allow(dead_code)]
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

fn run_cartesian_check<Capability>(
    standby: Piper<Standby, Capability>,
    args: &Args,
) -> Result<(), Box<dyn Error>>
where
    Capability: MotionCapability,
{
    let robot = standby.enable_position_mode(PositionModeConfig {
        speed_percent: args.speed_percent,
        motion_type: MotionType::Cartesian,
        ..Default::default()
    })?;
    println!(
        "[PASS] enabled PositionMode motion=Cartesian speed_percent={}",
        args.speed_percent
    );

    let observer = robot.observer();
    let initial_end_pose = wait_for_monitor_snapshot(
        INITIAL_MONITOR_SNAPSHOT_TIMEOUT,
        INITIAL_MONITOR_SNAPSHOT_POLL_INTERVAL,
        || observer.end_pose(),
    )?;

    let initial_position = position_from_end_pose(initial_end_pose.end_pose);
    let target_position = Position3D::new(
        initial_position.x + args.delta_x,
        initial_position.y + args.delta_y,
        initial_position.z + args.delta_z,
    );
    let target_orientation = target_orientation_degrees(initial_end_pose.end_pose);
    let target_delta_m = translation_distance(initial_position, target_position);

    println!(
        "[PASS] initial snapshot position_m=({:.6}, {:.6}, {:.6})",
        initial_position.x, initial_position.y, initial_position.z
    );
    println!(
        "[PASS] command step=move target_m=({:.6}, {:.6}, {:.6}) delta_m={:.6} speed_percent={}",
        target_position.x, target_position.y, target_position.z, target_delta_m, args.speed_percent
    );

    robot.command_cartesian_pose(target_position, target_orientation)?;

    let moved_pose = wait_for_end_pose_settle(
        observer,
        initial_position,
        target_position,
        Duration::from_millis(args.settle_timeout_ms),
        POSITION_SETTLE_POLL_INTERVAL,
    )?;
    let moved_position = position_from_end_pose(moved_pose.end_pose);
    let observed_delta = translation_distance(moved_position, initial_position);
    let move_error = translation_distance(moved_position, target_position);

    println!(
        "[PASS] settle step=move observed_delta_m={:.6} target_delta_m={:.6} position_error_m={:.6}",
        observed_delta, target_delta_m, move_error
    );

    robot.command_cartesian_pose(initial_position, target_orientation)?;
    println!(
        "[PASS] command step=return target_m=({:.6}, {:.6}, {:.6})",
        initial_position.x, initial_position.y, initial_position.z
    );

    let returned_pose = wait_for_end_pose_settle(
        observer,
        target_position,
        initial_position,
        Duration::from_millis(args.settle_timeout_ms),
        POSITION_SETTLE_POLL_INTERVAL,
    )?;
    let returned_position = position_from_end_pose(returned_pose.end_pose);
    let return_error = translation_distance(returned_position, initial_position);

    println!(
        "[PASS] settle step=return final_position_m=({:.6}, {:.6}, {:.6}) return_error_m={:.6} tolerance_m={:.6}",
        returned_position.x,
        returned_position.y,
        returned_position.z,
        return_error,
        POSITION_SETTLE_TOLERANCE_M
    );
    println!("[PASS] hil_cartesian_pose_check complete");

    Ok(())
}

fn validate_args(args: &Args) -> Result<(), String> {
    let delta = Position3D::new(args.delta_x, args.delta_y, args.delta_z);
    if args.speed_percent == 0 {
        return Err("speed_percent must be > 0 for manual HIL".to_string());
    }
    if args.speed_percent > MAX_SPEED_PERCENT {
        return Err("speed_percent must be <= 10 for manual HIL".to_string());
    }
    if !args.delta_x.is_finite() || !args.delta_y.is_finite() || !args.delta_z.is_finite() {
        return Err("cartesian delta must be finite for manual HIL".to_string());
    }
    if delta.norm() == 0.0 {
        return Err("cartesian delta norm must be > 0 for manual HIL".to_string());
    }
    if delta.norm() > MAX_TRANSLATION_DELTA_M {
        return Err("cartesian delta norm must be <= 0.02 m for manual HIL".to_string());
    }
    if args.settle_timeout_ms == 0 {
        return Err("settle_timeout_ms must be > 0".to_string());
    }
    Ok(())
}

#[allow(dead_code)]
fn translation_distance(current: Position3D, target: Position3D) -> f64 {
    let dx = current.x - target.x;
    let dy = current.y - target.y;
    let dz = current.z - target.z;
    (dx * dx + dy * dy + dz * dz).sqrt()
}

#[allow(dead_code)]
fn progress_threshold(total_delta_m: f64) -> f64 {
    (total_delta_m * 0.25).max(MIN_PROGRESS_M).min(total_delta_m)
}

#[allow(dead_code)]
fn target_orientation_degrees(end_pose: [f64; 6]) -> EulerAngles {
    EulerAngles::new(
        end_pose[3].to_degrees(),
        end_pose[4].to_degrees(),
        end_pose[5].to_degrees(),
    )
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

fn wait_for_end_pose_settle<Capability>(
    observer: &piper_sdk::client::observer::Observer<Capability>,
    start_position: Position3D,
    target_position: Position3D,
    timeout: Duration,
    poll_interval: Duration,
) -> ClientResult<piper_sdk::driver::state::EndPoseState>
where
    Capability: MotionCapability,
{
    let start = Instant::now();
    let total_delta = translation_distance(start_position, target_position);
    let min_progress = progress_threshold(total_delta);
    let mut progress_seen = false;

    loop {
        if start.elapsed() >= timeout {
            return Err(RobotError::Timeout {
                timeout_ms: timeout.as_millis() as u64,
            });
        }

        let remaining = timeout.saturating_sub(start.elapsed());
        let pose = wait_for_monitor_snapshot(remaining, poll_interval, || observer.end_pose())?;
        let observed_position = position_from_end_pose(pose.end_pose);
        let observed_delta = translation_distance(observed_position, start_position);
        let remaining_error = translation_distance(observed_position, target_position);

        if !progress_seen && observed_delta >= min_progress {
            progress_seen = true;
        }

        if progress_seen && remaining_error <= POSITION_SETTLE_TOLERANCE_M {
            return Ok(pose);
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

fn position_from_end_pose(end_pose: [f64; 6]) -> Position3D {
    Position3D::new(end_pose[0], end_pose[1], end_pose[2])
}

#[test]
fn validate_args_rejects_non_positive_speed_percent() {
    let args = Args {
        interface: "can0".to_string(),
        baud_rate: 1_000_000,
        delta_x: 0.01,
        delta_y: 0.0,
        delta_z: 0.0,
        speed_percent: 0,
        settle_timeout_ms: 10_000,
    };

    let error = validate_args(&args).expect_err("speed 0 must be rejected");
    assert!(error.contains("speed_percent"));
}

#[test]
fn validate_args_rejects_zero_translation_delta() {
    let args = Args {
        interface: "can0".to_string(),
        baud_rate: 1_000_000,
        delta_x: 0.0,
        delta_y: 0.0,
        delta_z: 0.0,
        speed_percent: 5,
        settle_timeout_ms: 10_000,
    };

    let error = validate_args(&args).expect_err("zero translation must be rejected");
    assert!(error.contains("delta"));
}

#[test]
fn progress_threshold_requires_meaningful_motion_progress() {
    let threshold = progress_threshold(0.008);
    assert!(threshold >= MIN_PROGRESS_M);
    assert!(threshold > 0.0);
}

#[test]
fn target_orientation_degrees_preserves_end_pose_angles() {
    let orientation = target_orientation_degrees([0.0, 0.0, 0.0, 1.0, -0.5, 0.25]);
    assert!((orientation.roll - 57.295_779_513).abs() < 1e-6);
    assert!((orientation.pitch + 28.647_889_757).abs() < 1e-6);
    assert!((orientation.yaw - 14.323_944_878).abs() < 1e-6);
}
