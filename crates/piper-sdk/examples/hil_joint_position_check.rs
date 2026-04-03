use clap::{Parser, ValueEnum};
use piper_control::{
    ControlProfile, MotionProgressSnapshot, MotionWaitConfig, ParkOrientation,
    active_park_blocking_with_progress,
};
use piper_sdk::PiperBuilder;
use piper_sdk::client::state::machine::MotionType;
use piper_sdk::client::state::{
    ConnectedPiper, DisableConfig, MotionCapability, Piper, PositionModeConfig, Standby,
};
use piper_sdk::client::types::{JointArray, Rad, Result as ClientResult, RobotError};
use piper_sdk::client::{MotionConnectedPiper, MotionConnectedState};
use piper_sdk::driver::ConnectionTarget;
use piper_sdk::driver::state::JointLimitConfig;
use piper_tools::SafetyConfig;
use std::error::Error;
use std::time::{Duration, Instant};

#[cfg(test)]
use clap::CommandFactory;

const MAX_SPEED_PERCENT: u8 = 10;
const MAX_DELTA_RAD: f64 = 0.035;
const DEFAULT_SETTLE_TIMEOUT_MS: u64 = 10_000;
const POSITION_SETTLE_TOLERANCE_RAD: f64 = 0.002;
const MIN_PROGRESS_RAD: f64 = 0.0005;
const INITIAL_MONITOR_SNAPSHOT_TIMEOUT: Duration = Duration::from_millis(200);
const INITIAL_MONITOR_SNAPSHOT_POLL_INTERVAL: Duration = Duration::from_millis(5);
const POSITION_SETTLE_POLL_INTERVAL: Duration = Duration::from_millis(10);
const PREFLIGHT_QUERY_TIMEOUT: Duration = Duration::from_secs(1);
const PREFLIGHT_TARGET_JOINT_FAIL_MARGIN_RAD: f64 = 0.05;
const PARK_PROGRESS_LOG_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum CliParkOrientation {
    Upright,
    Left,
    Right,
}

impl From<CliParkOrientation> for ParkOrientation {
    fn from(value: CliParkOrientation) -> Self {
        match value {
            CliParkOrientation::Upright => ParkOrientation::Upright,
            CliParkOrientation::Left => ParkOrientation::Left,
            CliParkOrientation::Right => ParkOrientation::Right,
        }
    }
}

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

    /// Skip success-path parking before disable.
    #[arg(long)]
    no_park: bool,

    /// Parking orientation used after a successful return-to-initial check.
    #[arg(
        long,
        value_enum,
        value_name = "upright|left|right",
        default_value_t = CliParkOrientation::Upright
    )]
    park_orientation: CliParkOrientation,
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
    let preflight_positions = {
        let observer = standby.observer();
        wait_for_monitor_snapshot(
            INITIAL_MONITOR_SNAPSHOT_TIMEOUT,
            INITIAL_MONITOR_SNAPSHOT_POLL_INTERVAL,
            || observer.joint_positions(),
        )?
    };
    match standby.query_joint_limit_config(PREFLIGHT_QUERY_TIMEOUT) {
        Ok(limits) => {
            let preflight = evaluate_joint_limit_preflight(
                &preflight_positions,
                &limits.value,
                usize::from(args.joint - 1),
            );
            for line in &preflight.lines {
                println!("{line}");
            }
            if let Some(error) = preflight.error {
                return Err(error.into());
            }
        },
        Err(error) => {
            println!("[WARN] preflight joint-limit query failed: {error}");
        },
    }

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

    robot.send_position_command(&initial_positions)?;
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

    if args.no_park {
        let _robot = robot.disable(DisableConfig::default())?;
        println!("[PASS] skipped parking via --no-park and disabled robot");
    } else {
        let park_profile = park_profile(args);
        let park_target = park_profile.park_pose();
        println!(
            "[PASS] command step=park orientation={} target_rad={}",
            ParkOrientation::from(args.park_orientation),
            format_joint_values(&park_target)
        );
        let mut next_park_log_at = PARK_PROGRESS_LOG_INTERVAL;
        active_park_blocking_with_progress(&robot, &park_profile, |snapshot| {
            if should_emit_park_progress_log(snapshot.elapsed, &mut next_park_log_at) {
                println!("{}", format_park_progress_line(snapshot));
            }
        })?;
        let _robot = robot.disable(DisableConfig::default())?;
        println!(
            "[PASS] parked orientation={} before disable",
            ParkOrientation::from(args.park_orientation)
        );
    }

    println!("{}", final_success_line(args));

    Ok(())
}

fn park_profile(args: &Args) -> ControlProfile {
    ControlProfile {
        target: ConnectionTarget::AutoStrict,
        orientation: args.park_orientation.into(),
        rest_pose_override: None,
        park_speed_percent: args.speed_percent,
        safety: SafetyConfig::default_config(),
        wait: MotionWaitConfig {
            threshold_rad: POSITION_SETTLE_TOLERANCE_RAD,
            poll_interval: POSITION_SETTLE_POLL_INTERVAL,
            republish_interval: POSITION_SETTLE_POLL_INTERVAL,
            timeout: Duration::from_millis(args.settle_timeout_ms),
        },
    }
}

fn final_success_line(args: &Args) -> String {
    if args.no_park {
        "[PASS] hil_joint_position_check complete without parking (--no-park)".to_string()
    } else {
        format!(
            "[PASS] hil_joint_position_check complete after parking orientation={}",
            ParkOrientation::from(args.park_orientation)
        )
    }
}

fn should_emit_park_progress_log(elapsed: Duration, next_log_at: &mut Duration) -> bool {
    if elapsed >= *next_log_at {
        *next_log_at = elapsed + PARK_PROGRESS_LOG_INTERVAL;
        true
    } else {
        false
    }
}

fn format_park_progress_line(snapshot: &MotionProgressSnapshot) -> String {
    format!(
        "[INFO] park progress t={:.1}s current_rad={} joint_error_rad={} max_error_rad={:.4}",
        snapshot.elapsed.as_secs_f64(),
        format_joint_values(&snapshot.current),
        format_joint_values(&snapshot.joint_errors),
        snapshot.max_error,
    )
}

fn format_joint_values(values: &[f64; 6]) -> String {
    format!(
        "[J1={:.4}, J2={:.4}, J3={:.4}, J4={:.4}, J5={:.4}, J6={:.4}]",
        values[0], values[1], values[2], values[3], values[4], values[5]
    )
}

#[cfg(test)]
fn parse_args_for_test<const N: usize>(extra_args: [&str; N]) -> Args {
    let mut argv = vec![
        "hil_joint_position_check",
        "--interface",
        "test-interface",
        "--joint",
        "1",
        "--delta-rad",
        "0.02",
    ];
    argv.extend(extra_args);
    Args::try_parse_from(argv).expect("cli args should parse")
}

#[cfg(test)]
fn expected_park_pose(orientation: CliParkOrientation) -> [f64; 6] {
    ParkOrientation::from(orientation).default_rest_pose()
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
    Capability: MotionCapability,
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

struct JointLimitPreflight {
    lines: Vec<String>,
    error: Option<String>,
}

fn evaluate_joint_limit_preflight(
    joint_positions: &JointArray<Rad>,
    limits: &JointLimitConfig,
    target_joint_index: usize,
) -> JointLimitPreflight {
    let violations: Vec<(usize, String, f64)> = joint_positions
        .iter()
        .zip(limits.joints.iter())
        .enumerate()
        .filter_map(|(index, (position, limit))| {
            let below = (limit.min_angle_rad - position.0).max(0.0);
            let above = (position.0 - limit.max_angle_rad).max(0.0);
            let violation_margin = below.max(above);
            if violation_margin > 0.0 {
                Some((
                    index,
                    format!(
                        "J{}={:.6} rad outside [{:.6}, {:.6}]",
                        index + 1,
                        position.0,
                        limit.min_angle_rad,
                        limit.max_angle_rad
                    ),
                    violation_margin,
                ))
            } else {
                None
            }
        })
        .collect();

    if violations.is_empty() {
        JointLimitPreflight {
            lines: vec![
                "[PASS] preflight joint-limit check all joints within queried limits".to_string(),
            ],
            error: None,
        }
    } else {
        let mut lines = vec![
            "[WARN] preflight joint-limit check found joints outside queried limits".to_string(),
        ];
        lines.extend(violations.iter().map(|(_, violation, _)| format!("[WARN] {violation}")));
        let target_joint_violation = violations
            .iter()
            .find(|(index, _, _)| *index == target_joint_index)
            .map(|(_, _, margin)| *margin)
            .unwrap_or(0.0);
        JointLimitPreflight {
            lines,
            error: (target_joint_violation > PREFLIGHT_TARGET_JOINT_FAIL_MARGIN_RAD).then_some(
                "preflight joint-limit check failed; target joint is significantly outside queried limits"
                    .to_string(),
            ),
        }
    }
}

#[test]
fn validate_args_rejects_excessive_speed() {
    let args = Args {
        interface: "can0".to_string(),
        baud_rate: 1_000_000,
        joint: 1,
        delta_rad: 0.02,
        speed_percent: 11,
        settle_timeout_ms: 10_000,
        no_park: false,
        park_orientation: CliParkOrientation::Upright,
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
        no_park: false,
        park_orientation: CliParkOrientation::Upright,
    };

    let error = validate_args(&args).expect_err("delta > 0.035 rad must be rejected");
    assert!(error.contains("delta_rad"));
}

#[test]
fn validate_args_rejects_non_finite_delta() {
    let args = Args {
        interface: "can0".to_string(),
        baud_rate: 1_000_000,
        joint: 1,
        delta_rad: f64::NAN,
        speed_percent: 10,
        settle_timeout_ms: 10_000,
        no_park: false,
        park_orientation: CliParkOrientation::Upright,
    };

    let error = validate_args(&args).expect_err("non-finite delta must be rejected");
    assert!(error.contains("delta_rad"));
    assert!(error.contains("finite"));
}

#[test]
fn validate_args_rejects_invalid_joint() {
    let args = Args {
        interface: "can0".to_string(),
        baud_rate: 1_000_000,
        joint: 0,
        delta_rad: 0.02,
        speed_percent: 10,
        settle_timeout_ms: 10_000,
        no_park: false,
        park_orientation: CliParkOrientation::Upright,
    };

    let error = validate_args(&args).expect_err("joint outside 1..=6 must be rejected");
    assert!(error.contains("joint"));
}

#[test]
fn validate_args_rejects_zero_settle_timeout() {
    let args = Args {
        interface: "can0".to_string(),
        baud_rate: 1_000_000,
        joint: 1,
        delta_rad: 0.02,
        speed_percent: 10,
        settle_timeout_ms: 0,
        no_park: false,
        park_orientation: CliParkOrientation::Upright,
    };

    let error = validate_args(&args).expect_err("zero settle timeout must be rejected");
    assert!(error.contains("settle_timeout_ms"));
}

#[test]
fn validate_args_accepts_no_park() {
    let args = Args {
        interface: "can0".to_string(),
        baud_rate: 1_000_000,
        joint: 1,
        delta_rad: 0.02,
        speed_percent: 5,
        settle_timeout_ms: 10_000,
        no_park: true,
        park_orientation: CliParkOrientation::Upright,
    };

    validate_args(&args).expect("no-park should be accepted");
}

#[test]
fn cli_parses_no_park_flag() {
    let args = parse_args_for_test(["--no-park"]);

    assert!(args.no_park);
}

#[test]
fn cli_parses_park_orientation_left() {
    let args = parse_args_for_test(["--park-orientation", "left"]);

    assert_eq!(args.park_orientation, CliParkOrientation::Left);
}

#[test]
fn cli_defaults_park_orientation_to_upright() {
    let args = parse_args_for_test([]);

    assert_eq!(args.park_orientation, CliParkOrientation::Upright);
}

#[test]
fn final_success_line_distinguishes_no_park_path() {
    let no_park_args = Args {
        interface: "can0".to_string(),
        baud_rate: 1_000_000,
        joint: 1,
        delta_rad: 0.02,
        speed_percent: 5,
        settle_timeout_ms: 10_000,
        no_park: true,
        park_orientation: CliParkOrientation::Upright,
    };
    let parked_args = Args {
        interface: "can0".to_string(),
        baud_rate: 1_000_000,
        joint: 1,
        delta_rad: 0.02,
        speed_percent: 5,
        settle_timeout_ms: 10_000,
        no_park: false,
        park_orientation: CliParkOrientation::Left,
    };

    assert_eq!(
        final_success_line(&no_park_args),
        "[PASS] hil_joint_position_check complete without parking (--no-park)"
    );
    assert_eq!(
        final_success_line(&parked_args),
        "[PASS] hil_joint_position_check complete after parking orientation=left"
    );
}

#[test]
fn park_orientation_help_lists_explicit_allowed_values() {
    let mut command = Args::command();
    let mut buffer = Vec::new();
    command.write_long_help(&mut buffer).expect("help should render");
    let help = String::from_utf8(buffer).expect("help should be utf-8");

    assert!(help.contains("--park-orientation <upright|left|right>"));
    assert!(help.contains("[possible values: upright, left, right]"));
}

#[test]
fn park_profile_maps_orientation_and_wait_fields() {
    let args = Args {
        interface: "test-interface".to_string(),
        baud_rate: 1_000_000,
        joint: 1,
        delta_rad: 0.02,
        speed_percent: 7,
        settle_timeout_ms: 12_345,
        no_park: false,
        park_orientation: CliParkOrientation::Left,
    };

    let profile = park_profile(&args);

    assert_eq!(profile.orientation, ParkOrientation::Left);
    assert_eq!(
        profile.park_pose(),
        expected_park_pose(CliParkOrientation::Left)
    );
    assert_eq!(profile.park_speed_percent, 7);
    assert_eq!(profile.wait.threshold_rad, POSITION_SETTLE_TOLERANCE_RAD);
    assert_eq!(profile.wait.timeout, Duration::from_millis(12_345));
}

#[test]
fn park_progress_log_interval_throttles_to_once_per_second() {
    let mut next_log_at = Duration::from_secs(1);

    assert!(!should_emit_park_progress_log(
        Duration::from_millis(900),
        &mut next_log_at
    ));
    assert_eq!(next_log_at, Duration::from_secs(1));

    assert!(should_emit_park_progress_log(
        Duration::from_secs(1),
        &mut next_log_at
    ));
    assert_eq!(next_log_at, Duration::from_secs(2));

    assert!(!should_emit_park_progress_log(
        Duration::from_millis(1500),
        &mut next_log_at
    ));
    assert_eq!(next_log_at, Duration::from_secs(2));

    assert!(should_emit_park_progress_log(
        Duration::from_secs(2),
        &mut next_log_at
    ));
    assert_eq!(next_log_at, Duration::from_secs(3));
}

#[test]
fn park_progress_log_interval_does_not_burst_after_large_gap() {
    let mut next_log_at = Duration::from_secs(1);

    assert!(should_emit_park_progress_log(
        Duration::from_millis(3200),
        &mut next_log_at
    ));
    assert_eq!(next_log_at, Duration::from_millis(4200));

    assert!(!should_emit_park_progress_log(
        Duration::from_millis(3210),
        &mut next_log_at
    ));
    assert!(!should_emit_park_progress_log(
        Duration::from_millis(4199),
        &mut next_log_at
    ));

    assert!(should_emit_park_progress_log(
        Duration::from_millis(4200),
        &mut next_log_at
    ));
    assert_eq!(next_log_at, Duration::from_millis(5200));
}

#[test]
fn evaluate_joint_limit_preflight_reports_out_of_range_joints() {
    let positions = JointArray::from([
        Rad(0.0),
        Rad(-0.04),
        Rad(0.03),
        Rad(0.0),
        Rad(0.0),
        Rad(0.0),
    ]);
    let limits = JointLimitConfig {
        joints: [
            piper_sdk::driver::state::JointLimit {
                min_angle_rad: -1.0,
                max_angle_rad: 1.0,
                max_velocity_rad_s: 0.3,
            },
            piper_sdk::driver::state::JointLimit {
                min_angle_rad: 0.0,
                max_angle_rad: 1.0,
                max_velocity_rad_s: 0.3,
            },
            piper_sdk::driver::state::JointLimit {
                min_angle_rad: -1.0,
                max_angle_rad: 0.0,
                max_velocity_rad_s: 0.3,
            },
            piper_sdk::driver::state::JointLimit {
                min_angle_rad: -1.0,
                max_angle_rad: 1.0,
                max_velocity_rad_s: 0.3,
            },
            piper_sdk::driver::state::JointLimit {
                min_angle_rad: -1.0,
                max_angle_rad: 1.0,
                max_velocity_rad_s: 0.3,
            },
            piper_sdk::driver::state::JointLimit {
                min_angle_rad: -1.0,
                max_angle_rad: 1.0,
                max_velocity_rad_s: 0.3,
            },
        ],
    };

    let preflight = evaluate_joint_limit_preflight(&positions, &limits, 0);
    let lines = preflight.lines;
    assert_eq!(
        lines.first().expect("summary line"),
        "[WARN] preflight joint-limit check found joints outside queried limits"
    );
    assert!(preflight.error.is_none());
    assert!(
        lines
            .iter()
            .any(|line| line.contains("J2=-0.040000 rad outside [0.000000, 1.000000]"))
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("J3=0.030000 rad outside [-1.000000, 0.000000]"))
    );
}

#[test]
fn evaluate_joint_limit_preflight_reports_all_clear_when_in_range() {
    let positions = JointArray::splat(Rad(0.0));
    let limits = JointLimitConfig {
        joints: [piper_sdk::driver::state::JointLimit {
            min_angle_rad: -1.0,
            max_angle_rad: 1.0,
            max_velocity_rad_s: 0.3,
        }; 6],
    };

    let preflight = evaluate_joint_limit_preflight(&positions, &limits, 0);
    let lines = preflight.lines;
    assert_eq!(
        lines,
        vec!["[PASS] preflight joint-limit check all joints within queried limits".to_string()]
    );
    assert!(preflight.error.is_none());
}

#[test]
fn evaluate_joint_limit_preflight_warns_for_small_violations_on_other_joints() {
    let positions = JointArray::from([
        Rad(0.0),
        Rad(-0.039),
        Rad(0.0),
        Rad(0.0),
        Rad(0.0),
        Rad(0.0),
    ]);
    let limits = JointLimitConfig {
        joints: [
            piper_sdk::driver::state::JointLimit {
                min_angle_rad: -1.0,
                max_angle_rad: 1.0,
                max_velocity_rad_s: 0.3,
            },
            piper_sdk::driver::state::JointLimit {
                min_angle_rad: 0.0,
                max_angle_rad: 1.0,
                max_velocity_rad_s: 0.3,
            },
            piper_sdk::driver::state::JointLimit {
                min_angle_rad: -1.0,
                max_angle_rad: 1.0,
                max_velocity_rad_s: 0.3,
            },
            piper_sdk::driver::state::JointLimit {
                min_angle_rad: -1.0,
                max_angle_rad: 1.0,
                max_velocity_rad_s: 0.3,
            },
            piper_sdk::driver::state::JointLimit {
                min_angle_rad: -1.0,
                max_angle_rad: 1.0,
                max_velocity_rad_s: 0.3,
            },
            piper_sdk::driver::state::JointLimit {
                min_angle_rad: -1.0,
                max_angle_rad: 1.0,
                max_velocity_rad_s: 0.3,
            },
        ],
    };

    let preflight = evaluate_joint_limit_preflight(&positions, &limits, 0);

    assert!(preflight.error.is_none());
    assert!(
        preflight
            .lines
            .iter()
            .any(|line| line.contains("J2=-0.039000 rad outside [0.000000, 1.000000]"))
    );
}

#[test]
fn evaluate_joint_limit_preflight_fails_for_large_violation_on_target_joint() {
    let positions = JointArray::from([Rad(1.12), Rad(0.0), Rad(0.0), Rad(0.0), Rad(0.0), Rad(0.0)]);
    let limits = JointLimitConfig {
        joints: [piper_sdk::driver::state::JointLimit {
            min_angle_rad: -1.0,
            max_angle_rad: 1.0,
            max_velocity_rad_s: 0.3,
        }; 6],
    };

    let preflight = evaluate_joint_limit_preflight(&positions, &limits, 0);

    assert_eq!(
        preflight.error.as_deref(),
        Some(
            "preflight joint-limit check failed; target joint is significantly outside queried limits"
        )
    );
    assert!(
        preflight
            .lines
            .iter()
            .any(|line| line.contains("J1=1.120000 rad outside [-1.000000, 1.000000]"))
    );
}
