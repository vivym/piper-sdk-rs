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
const MAX_LINEAR_DELTA_M: f64 = 0.02;
const DEFAULT_SETTLE_TIMEOUT_MS: u64 = 10_000;
const POSITION_SETTLE_TOLERANCE_M: f64 = 0.003;
const MIN_PROGRESS_M: f64 = 0.001;
const MIN_LINE_DEVIATION_TOLERANCE_M: f64 = 0.0015;
const INITIAL_MONITOR_SNAPSHOT_TIMEOUT: Duration = Duration::from_millis(200);
const INITIAL_MONITOR_SNAPSHOT_POLL_INTERVAL: Duration = Duration::from_millis(5);
const POSITION_SETTLE_POLL_INTERVAL: Duration = Duration::from_millis(10);

#[derive(Parser, Debug)]
#[command(name = "hil_linear_motion_check")]
#[command(about = "Safe linear motion HIL helper for one real Piper arm")]
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
    delta_m: f64,

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
            run_linear_check(robot, args)
        },
        MotionConnectedPiper::Soft(MotionConnectedState::Standby(robot)) => {
            println!("[PASS] connected and confirmed Standby");
            run_linear_check(robot, args)
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

fn run_linear_check<Capability>(
    standby: Piper<Standby, Capability>,
    args: &Args,
) -> Result<(), Box<dyn Error>>
where
    Capability: MotionCapability,
{
    let robot = standby.enable_position_mode(PositionModeConfig {
        speed_percent: args.speed_percent,
        motion_type: MotionType::Linear,
        ..Default::default()
    })?;
    println!(
        "[PASS] enabled PositionMode motion=Linear speed_percent={}",
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
        initial_position.x + args.delta_m,
        initial_position.y,
        initial_position.z,
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

    robot.move_linear(target_position, target_orientation)?;
    let moved = wait_for_linear_settle(
        observer,
        initial_position,
        target_position,
        Duration::from_millis(args.settle_timeout_ms),
        POSITION_SETTLE_POLL_INTERVAL,
    )?;
    let moved_position = position_from_end_pose(moved.end_pose.end_pose);
    let observed_delta = translation_distance(moved_position, initial_position);
    let move_error = translation_distance(moved_position, target_position);

    println!(
        "[PASS] settle step=move observed_delta_m={:.6} target_delta_m={:.6} position_error_m={:.6} max_line_deviation_m={:.6}",
        observed_delta, target_delta_m, move_error, moved.max_orthogonal_deviation_m
    );

    robot.move_linear(initial_position, target_orientation)?;
    println!(
        "[PASS] command step=return target_m=({:.6}, {:.6}, {:.6})",
        initial_position.x, initial_position.y, initial_position.z
    );

    let returned = wait_for_linear_settle(
        observer,
        target_position,
        initial_position,
        Duration::from_millis(args.settle_timeout_ms),
        POSITION_SETTLE_POLL_INTERVAL,
    )?;
    let returned_position = position_from_end_pose(returned.end_pose.end_pose);
    let return_error = translation_distance(returned_position, initial_position);

    println!(
        "[PASS] settle step=return final_position_m=({:.6}, {:.6}, {:.6}) return_error_m={:.6} max_line_deviation_m={:.6} tolerance_m={:.6}",
        returned_position.x,
        returned_position.y,
        returned_position.z,
        return_error,
        returned.max_orthogonal_deviation_m,
        POSITION_SETTLE_TOLERANCE_M
    );
    println!("[PASS] hil_linear_motion_check complete");

    Ok(())
}

fn validate_args(args: &Args) -> Result<(), String> {
    if !args.delta_m.is_finite() {
        return Err("delta_m must be finite for manual HIL".to_string());
    }
    if args.speed_percent == 0 || args.speed_percent > MAX_SPEED_PERCENT {
        return Err("speed_percent must be between 1 and 10 for manual HIL".to_string());
    }
    if args.delta_m == 0.0 {
        return Err("delta_m must be non-zero for manual HIL".to_string());
    }
    if args.delta_m.abs() > MAX_LINEAR_DELTA_M {
        return Err("delta_m must be <= 0.02 m for manual HIL".to_string());
    }
    if args.settle_timeout_ms == 0 {
        return Err("settle_timeout_ms must be > 0".to_string());
    }

    Ok(())
}

fn position_from_end_pose(end_pose: [f64; 6]) -> Position3D {
    Position3D::new(end_pose[0], end_pose[1], end_pose[2])
}

fn translation_distance(start: Position3D, target: Position3D) -> f64 {
    let dx = start.x - target.x;
    let dy = start.y - target.y;
    let dz = start.z - target.z;
    (dx * dx + dy * dy + dz * dz).sqrt()
}

fn progress_threshold(total_delta_m: f64) -> f64 {
    (total_delta_m * 0.25).max(MIN_PROGRESS_M).min(total_delta_m)
}

fn line_deviation_tolerance(total_delta_m: f64) -> f64 {
    (total_delta_m * 0.2).max(MIN_LINE_DEVIATION_TOLERANCE_M)
}

fn orthogonal_deviation_from_segment(
    start: Position3D,
    target: Position3D,
    sample: Position3D,
) -> f64 {
    let segment = Position3D::new(target.x - start.x, target.y - start.y, target.z - start.z);
    let sample_offset = Position3D::new(sample.x - start.x, sample.y - start.y, sample.z - start.z);
    let segment_len_sq = segment.dot(&segment);
    if segment_len_sq <= f64::EPSILON {
        return sample_offset.norm();
    }

    let projection = (sample_offset.dot(&segment) / segment_len_sq).clamp(0.0, 1.0);
    let closest = Position3D::new(
        start.x + segment.x * projection,
        start.y + segment.y * projection,
        start.z + segment.z * projection,
    );
    translation_distance(sample, closest)
}

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

struct LinearSettleResult {
    end_pose: piper_sdk::driver::state::EndPoseState,
    max_orthogonal_deviation_m: f64,
}

fn wait_for_linear_settle<Capability>(
    observer: &piper_sdk::client::observer::Observer<Capability>,
    start_position: Position3D,
    target_position: Position3D,
    timeout: Duration,
    poll_interval: Duration,
) -> ClientResult<LinearSettleResult>
where
    Capability: MotionCapability,
{
    let start = Instant::now();
    let total_delta = translation_distance(start_position, target_position);
    let min_progress = progress_threshold(total_delta);
    let deviation_limit = line_deviation_tolerance(total_delta);
    let mut progress_seen = false;
    let mut max_orthogonal_deviation_m = 0.0_f64;

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
        let orthogonal_deviation =
            orthogonal_deviation_from_segment(start_position, target_position, observed_position);
        max_orthogonal_deviation_m = max_orthogonal_deviation_m.max(orthogonal_deviation);

        if orthogonal_deviation > deviation_limit {
            return Err(RobotError::ConfigError(format!(
                "linear trajectory deviation {:.6} m exceeded tolerance {:.6} m",
                orthogonal_deviation, deviation_limit
            )));
        }

        if !progress_seen && observed_delta >= min_progress {
            progress_seen = true;
        }

        if progress_seen && remaining_error <= POSITION_SETTLE_TOLERANCE_M {
            return Ok(LinearSettleResult {
                end_pose: pose,
                max_orthogonal_deviation_m,
            });
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

#[test]
fn validate_args_rejects_excessive_linear_delta() {
    let args = Args {
        interface: "can0".to_string(),
        baud_rate: 1_000_000,
        delta_m: 0.05,
        speed_percent: 5,
        settle_timeout_ms: 10_000,
    };

    let error = validate_args(&args).expect_err("delta > 0.02 m must be rejected");
    assert!(error.contains("delta_m"));
}

#[test]
fn orthogonal_deviation_is_zero_for_points_on_segment() {
    let start = Position3D::new(0.0, 0.0, 0.0);
    let target = Position3D::new(0.02, 0.0, 0.0);
    let sample = Position3D::new(0.01, 0.0, 0.0);

    let deviation = orthogonal_deviation_from_segment(start, target, sample);
    assert!(deviation <= 1e-9);
}
