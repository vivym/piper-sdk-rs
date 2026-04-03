use clap::Parser;
use piper_sdk::PiperBuilder;
use piper_sdk::client::state::machine::MotionType;
use piper_sdk::client::state::{
    ConnectedPiper, MotionCapability, Piper, PositionModeConfig, Standby,
};
use piper_sdk::client::types::{EulerAngles, Position3D, Result as ClientResult, RobotError};
use piper_sdk::client::{MotionConnectedPiper, MotionConnectedState};
use piper_sdk::driver::RobotControlState;
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

#[derive(Debug, Clone)]
struct CartesianSettleRequest {
    start_position: Position3D,
    target_position: Position3D,
    baseline_robot_control: RobotControlState,
    timeout: Duration,
    poll_interval: Duration,
    trace_robot_status: bool,
}

#[derive(Debug, Clone)]
struct CartesianSettleProgress {
    min_progress_m: f64,
    max_observed_delta_m: f64,
    remaining_error_m: f64,
    last_pose: Option<piper_sdk::driver::state::EndPoseState>,
    first_control_change: Option<(RobotControlState, f64)>,
}

#[derive(Debug, Clone)]
struct CartesianTimeoutDiagnostics {
    request: CartesianSettleRequest,
    progress: CartesianSettleProgress,
    current_robot_control: RobotControlState,
    health: piper_sdk::client::observer::RuntimeHealthSnapshot,
}

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

    /// Trace robot_control changes during settle; prints only when the state changes.
    #[arg(long, default_value_t = false)]
    trace_robot_status: bool,
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

    let move_baseline_control = observer.robot_control_snapshot();
    robot.command_cartesian_pose(target_position, target_orientation)?;

    let moved_pose = wait_for_end_pose_settle(
        observer,
        CartesianSettleRequest {
            start_position: initial_position,
            target_position,
            baseline_robot_control: move_baseline_control,
            timeout: Duration::from_millis(args.settle_timeout_ms),
            poll_interval: POSITION_SETTLE_POLL_INTERVAL,
            trace_robot_status: args.trace_robot_status,
        },
    )?;
    let moved_position = position_from_end_pose(moved_pose.end_pose);
    let observed_delta = translation_distance(moved_position, initial_position);
    let move_error = translation_distance(moved_position, target_position);

    println!(
        "[PASS] settle step=move observed_delta_m={:.6} target_delta_m={:.6} position_error_m={:.6}",
        observed_delta, target_delta_m, move_error
    );

    let return_baseline_control = observer.robot_control_snapshot();
    robot.command_cartesian_pose(initial_position, target_orientation)?;
    println!(
        "[PASS] command step=return target_m=({:.6}, {:.6}, {:.6})",
        initial_position.x, initial_position.y, initial_position.z
    );

    let returned_pose = wait_for_end_pose_settle(
        observer,
        CartesianSettleRequest {
            start_position: target_position,
            target_position: initial_position,
            baseline_robot_control: return_baseline_control,
            timeout: Duration::from_millis(args.settle_timeout_ms),
            poll_interval: POSITION_SETTLE_POLL_INTERVAL,
            trace_robot_status: args.trace_robot_status,
        },
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

#[cfg(test)]
fn parse_args_for_test(extra_args: &[&str]) -> Args {
    let mut argv = vec![
        "hil_cartesian_pose_check",
        "--interface",
        "test-interface",
        "--delta-x",
        "0.01",
    ];
    argv.extend_from_slice(extra_args);
    Args::try_parse_from(argv).expect("cli args should parse")
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

#[allow(dead_code)]
fn format_cartesian_timeout_diagnostics(diagnostics: &CartesianTimeoutDiagnostics) -> String {
    let last_position = diagnostics
        .progress
        .last_pose
        .map(|pose| {
            format!(
                "({:.6}, {:.6}, {:.6})",
                pose.end_pose[0], pose.end_pose[1], pose.end_pose[2]
            )
        })
        .unwrap_or_else(|| "<unavailable>".to_string());

    let first_control_change = diagnostics
        .progress
        .first_control_change
        .as_ref()
        .map(|(state, observed_delta_m)| {
            format!(
                "robot_status={} control_mode={} move_mode={} motion_status={} observed_delta_m={:.6}",
                state.robot_status,
                state.control_mode,
                state.move_mode,
                state.motion_status,
                *observed_delta_m,
            )
        })
        .unwrap_or_else(|| "<none>".to_string());

    format!(
        "target_m=({:.6}, {:.6}, {:.6}) start_m=({:.6}, {:.6}, {:.6}) min_progress_m={:.6} max_observed_delta_m={:.6} remaining_error_m={:.6} last_position_m={} baseline_control={{robot_status={}, control_mode={}, move_mode={}, motion_status={}}} first_control_change={} robot_status={} control_mode={} move_mode={} is_enabled={} health={:?}",
        diagnostics.request.target_position.x,
        diagnostics.request.target_position.y,
        diagnostics.request.target_position.z,
        diagnostics.request.start_position.x,
        diagnostics.request.start_position.y,
        diagnostics.request.start_position.z,
        diagnostics.progress.min_progress_m,
        diagnostics.progress.max_observed_delta_m,
        diagnostics.progress.remaining_error_m,
        last_position,
        diagnostics.request.baseline_robot_control.robot_status,
        diagnostics.request.baseline_robot_control.control_mode,
        diagnostics.request.baseline_robot_control.move_mode,
        diagnostics.request.baseline_robot_control.motion_status,
        first_control_change,
        diagnostics.current_robot_control.robot_status,
        diagnostics.current_robot_control.control_mode,
        diagnostics.current_robot_control.move_mode,
        diagnostics.current_robot_control.is_enabled,
        diagnostics.health,
    )
}

fn format_confirmed_driver_enabled_mask(mask: Option<u8>) -> String {
    match mask {
        Some(mask) => format!("{mask:06b}"),
        None => "<pending>".to_string(),
    }
}

fn format_robot_control_trace_line(
    elapsed: Duration,
    observed_delta_m: f64,
    state: &RobotControlState,
) -> String {
    format!(
        "[TRACE] t={:.3}s robot_status={} control_mode={} move_mode={} motion_status={} driver_enabled_mask={:06b} confirmed_driver_enabled_mask={} observed_delta_m={:.6}",
        elapsed.as_secs_f64(),
        state.robot_status,
        state.control_mode,
        state.move_mode,
        state.motion_status,
        state.driver_enabled_mask,
        format_confirmed_driver_enabled_mask(state.confirmed_driver_enabled_mask),
        observed_delta_m,
    )
}

fn maybe_emit_robot_control_trace<EmitTrace>(
    trace_enabled: bool,
    elapsed: Duration,
    observed_delta_m: f64,
    current_control: &RobotControlState,
    last_traced_control: &mut RobotControlState,
    mut emit_trace: EmitTrace,
) where
    EmitTrace: FnMut(String),
{
    if trace_enabled && has_control_state_changed(last_traced_control, current_control) {
        emit_trace(format_robot_control_trace_line(
            elapsed,
            observed_delta_m,
            current_control,
        ));
        *last_traced_control = current_control.clone();
    }
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
    request: CartesianSettleRequest,
) -> ClientResult<piper_sdk::driver::state::EndPoseState>
where
    Capability: MotionCapability,
{
    wait_for_end_pose_settle_with_hooks(
        request,
        |remaining, poll_interval| {
            wait_for_monitor_snapshot(remaining, poll_interval, || observer.end_pose())
        },
        || observer.robot_control_snapshot(),
        |request, progress| {
            emit_cartesian_timeout_warning(observer, request, progress);
        },
        std::thread::sleep,
    )
}

fn wait_for_end_pose_settle_with_hooks<ReadPose, ReadControl, EmitWarning, Sleep>(
    request: CartesianSettleRequest,
    mut read_pose: ReadPose,
    mut read_control: ReadControl,
    mut emit_warning: EmitWarning,
    mut sleep: Sleep,
) -> ClientResult<piper_sdk::driver::state::EndPoseState>
where
    ReadPose: FnMut(Duration, Duration) -> ClientResult<piper_sdk::driver::state::EndPoseState>,
    ReadControl: FnMut() -> RobotControlState,
    EmitWarning: FnMut(&CartesianSettleRequest, &CartesianSettleProgress),
    Sleep: FnMut(Duration),
{
    let start = Instant::now();
    let total_delta = translation_distance(request.start_position, request.target_position);
    let min_progress = progress_threshold(total_delta);
    let mut progress_seen = false;
    let mut last_traced_control = request.baseline_robot_control.clone();
    let mut progress = CartesianSettleProgress {
        min_progress_m: min_progress,
        max_observed_delta_m: 0.0,
        remaining_error_m: total_delta,
        last_pose: None,
        first_control_change: None,
    };

    loop {
        if start.elapsed() >= request.timeout {
            emit_warning(&request, &progress);
            return Err(RobotError::Timeout {
                timeout_ms: request.timeout.as_millis() as u64,
            });
        }

        let remaining = request.timeout.saturating_sub(start.elapsed());
        let pose = match read_pose(remaining, request.poll_interval) {
            Ok(pose) => pose,
            Err(timeout_error @ RobotError::Timeout { .. }) => {
                emit_warning(&request, &progress);
                return Err(timeout_error);
            },
            Err(other) => return Err(other),
        };
        let observed_position = position_from_end_pose(pose.end_pose);
        let observed_delta = translation_distance(observed_position, request.start_position);
        let remaining_error = translation_distance(observed_position, request.target_position);
        let current_control = read_control();
        maybe_emit_robot_control_trace(
            request.trace_robot_status,
            start.elapsed(),
            observed_delta,
            &current_control,
            &mut last_traced_control,
            |line| println!("{line}"),
        );
        progress.max_observed_delta_m = progress.max_observed_delta_m.max(observed_delta);
        progress.remaining_error_m = remaining_error;
        progress.last_pose = Some(pose);
        if progress.first_control_change.is_none()
            && has_control_state_changed(&request.baseline_robot_control, &current_control)
        {
            progress.first_control_change = Some((current_control, observed_delta));
        }

        if !progress_seen && observed_delta >= progress.min_progress_m {
            progress_seen = true;
        }

        if progress_seen && remaining_error <= POSITION_SETTLE_TOLERANCE_M {
            return Ok(pose);
        }

        let sleep_duration =
            request.poll_interval.min(request.timeout.saturating_sub(start.elapsed()));
        if sleep_duration.is_zero() {
            emit_warning(&request, &progress);
            return Err(RobotError::Timeout {
                timeout_ms: request.timeout.as_millis() as u64,
            });
        }

        sleep(sleep_duration);
    }
}

fn position_from_end_pose(end_pose: [f64; 6]) -> Position3D {
    Position3D::new(end_pose[0], end_pose[1], end_pose[2])
}

fn has_control_state_changed(baseline: &RobotControlState, current: &RobotControlState) -> bool {
    baseline.robot_status != current.robot_status
        || baseline.control_mode != current.control_mode
        || baseline.move_mode != current.move_mode
        || baseline.motion_status != current.motion_status
        || baseline.fault_angle_limit_mask != current.fault_angle_limit_mask
        || baseline.fault_comm_error_mask != current.fault_comm_error_mask
        || baseline.driver_enabled_mask != current.driver_enabled_mask
        || baseline.confirmed_driver_enabled_mask != current.confirmed_driver_enabled_mask
}

fn emit_cartesian_timeout_warning<Capability>(
    observer: &piper_sdk::client::observer::Observer<Capability>,
    request: &CartesianSettleRequest,
    progress: &CartesianSettleProgress,
) where
    Capability: MotionCapability,
{
    let diagnostics = CartesianTimeoutDiagnostics {
        request: request.clone(),
        progress: CartesianSettleProgress {
            last_pose: progress.last_pose.or_else(|| observer.last_complete_end_pose().ok()),
            ..progress.clone()
        },
        current_robot_control: observer.robot_control_snapshot(),
        health: observer.runtime_health(),
    };
    let diagnostics = format_cartesian_timeout_diagnostics(&diagnostics);
    tracing::warn!("Timed out waiting for Cartesian pose settle: {diagnostics}");
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
        trace_robot_status: false,
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
        trace_robot_status: false,
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

#[test]
fn format_cartesian_timeout_diagnostics_includes_last_pose_and_robot_state() {
    let diagnostics = format_cartesian_timeout_diagnostics(&CartesianTimeoutDiagnostics {
        request: CartesianSettleRequest {
            start_position: Position3D::new(0.04, -0.02, 0.16),
            target_position: Position3D::new(0.05, -0.02, 0.16),
            baseline_robot_control: RobotControlState {
                robot_status: 0,
                control_mode: 1,
                move_mode: 0,
                motion_status: 0,
                is_enabled: true,
                ..Default::default()
            },
            timeout: Duration::from_secs(10),
            poll_interval: Duration::from_millis(10),
            trace_robot_status: false,
        },
        progress: CartesianSettleProgress {
            min_progress_m: 0.0025,
            max_observed_delta_m: 0.0012,
            remaining_error_m: 0.0088,
            last_pose: Some(piper_sdk::driver::state::EndPoseState {
                end_pose: [0.041, -0.020, 0.160, 0.0, 0.0, 0.0],
                ..Default::default()
            }),
            first_control_change: Some((
                RobotControlState {
                    robot_status: 4,
                    control_mode: 1,
                    move_mode: 0,
                    motion_status: 0,
                    is_enabled: true,
                    ..Default::default()
                },
                0.0,
            )),
        },
        current_robot_control: RobotControlState {
            robot_status: 3,
            control_mode: 1,
            move_mode: 2,
            is_enabled: true,
            ..Default::default()
        },
        health: piper_sdk::client::observer::RuntimeHealthSnapshot {
            connected: true,
            last_feedback_age: Duration::from_millis(2),
            rx_alive: true,
            tx_alive: true,
            fault: None,
        },
    });

    assert!(diagnostics.contains("target_m=(0.050000, -0.020000, 0.160000)"));
    assert!(diagnostics.contains("max_observed_delta_m=0.001200"));
    assert!(diagnostics.contains("remaining_error_m=0.008800"));
    assert!(diagnostics.contains("last_position_m=(0.041000, -0.020000, 0.160000)"));
    assert!(diagnostics.contains(
        "baseline_control={robot_status=0, control_mode=1, move_mode=0, motion_status=0}"
    ));
    assert!(diagnostics.contains(
        "first_control_change=robot_status=4 control_mode=1 move_mode=0 motion_status=0 observed_delta_m=0.000000"
    ));
    assert!(diagnostics.contains("robot_status=3"));
    assert!(diagnostics.contains("control_mode=1"));
    assert!(diagnostics.contains("move_mode=2"));
    assert!(diagnostics.contains("is_enabled=true"));
}

#[test]
fn format_cartesian_timeout_diagnostics_marks_missing_last_pose() {
    let diagnostics = format_cartesian_timeout_diagnostics(&CartesianTimeoutDiagnostics {
        request: CartesianSettleRequest {
            start_position: Position3D::new(0.04, 0.0, 0.16),
            target_position: Position3D::new(0.05, 0.0, 0.16),
            baseline_robot_control: RobotControlState::default(),
            timeout: Duration::from_secs(10),
            poll_interval: Duration::from_millis(10),
            trace_robot_status: false,
        },
        progress: CartesianSettleProgress {
            min_progress_m: 0.0025,
            max_observed_delta_m: 0.0,
            remaining_error_m: 0.01,
            last_pose: None,
            first_control_change: None,
        },
        current_robot_control: RobotControlState::default(),
        health: piper_sdk::client::observer::RuntimeHealthSnapshot {
            connected: false,
            last_feedback_age: Duration::from_millis(50),
            rx_alive: true,
            tx_alive: true,
            fault: None,
        },
    });

    assert!(diagnostics.contains("last_position_m=<unavailable>"));
    assert!(diagnostics.contains("first_control_change=<none>"));
    assert!(diagnostics.contains("robot_status=0"));
}

#[test]
fn has_control_state_changed_detects_robot_status_flip() {
    let baseline = RobotControlState {
        robot_status: 0,
        control_mode: 1,
        move_mode: 0,
        motion_status: 0,
        driver_enabled_mask: 0b11_1111,
        confirmed_driver_enabled_mask: Some(0b11_1111),
        ..Default::default()
    };
    let changed = RobotControlState {
        robot_status: 4,
        ..baseline
    };

    assert!(has_control_state_changed(&baseline, &changed));
    assert!(!has_control_state_changed(&baseline, &baseline));
}

#[test]
fn settle_emits_timeout_warning_when_end_pose_read_times_out() {
    use std::cell::Cell;

    let warned = Cell::new(false);
    let baseline = RobotControlState {
        robot_status: 0,
        control_mode: 1,
        move_mode: 0,
        motion_status: 0,
        is_enabled: true,
        ..Default::default()
    };

    let result = wait_for_end_pose_settle_with_hooks(
        CartesianSettleRequest {
            start_position: Position3D::new(0.04, -0.02, 0.16),
            target_position: Position3D::new(0.05, -0.02, 0.16),
            baseline_robot_control: baseline.clone(),
            timeout: Duration::from_millis(10),
            poll_interval: Duration::from_millis(1),
            trace_robot_status: false,
        },
        |_remaining, _poll_interval| Err(RobotError::Timeout { timeout_ms: 10 }),
        || baseline.clone(),
        |_request, _progress| {
            warned.set(true);
        },
        |_duration| {},
    );

    match result {
        Err(RobotError::Timeout { timeout_ms }) => assert_eq!(timeout_ms, 10),
        other => panic!("expected timeout, got {other:?}"),
    }
    assert!(warned.get(), "timeout path should emit diagnostics");
}

#[test]
fn cli_parses_trace_robot_status_flag() {
    let args = parse_args_for_test(&["--trace-robot-status"]);
    assert!(args.trace_robot_status);
}

#[test]
fn robot_control_trace_only_emits_on_change() {
    let baseline = RobotControlState {
        robot_status: 0,
        control_mode: 1,
        move_mode: 0,
        motion_status: 0,
        driver_enabled_mask: 0,
        confirmed_driver_enabled_mask: Some(0),
        ..Default::default()
    };
    let changed = RobotControlState {
        robot_status: 4,
        driver_enabled_mask: 0b11_1111,
        confirmed_driver_enabled_mask: Some(0b11_1111),
        ..baseline.clone()
    };
    let mut last_traced = baseline.clone();
    let mut lines: Vec<String> = Vec::new();

    maybe_emit_robot_control_trace(
        true,
        Duration::from_millis(10),
        0.0,
        &baseline,
        &mut last_traced,
        |line| lines.push(line),
    );
    maybe_emit_robot_control_trace(
        true,
        Duration::from_millis(20),
        0.0,
        &changed,
        &mut last_traced,
        |line| lines.push(line),
    );
    maybe_emit_robot_control_trace(
        true,
        Duration::from_millis(30),
        0.001,
        &changed,
        &mut last_traced,
        |line| lines.push(line),
    );

    assert_eq!(lines.len(), 1);
    assert!(lines[0].contains("t=0.020s"));
    assert!(lines[0].contains("robot_status=4"));
    assert!(lines[0].contains("driver_enabled_mask=111111"));
}
