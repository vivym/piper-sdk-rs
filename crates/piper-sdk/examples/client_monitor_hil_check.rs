//! Client-side monitor HIL helper example.
//!
//! This example uses the client builder, waits for the first complete monitor snapshot,
//! and then keeps reading the observer in a read-only observation loop.

use clap::Parser;
use piper_sdk::PiperBuilder;
use piper_sdk::client::state::{MotionCapability, Piper, Standby};
use piper_sdk::client::types::{Result as ClientResult, RobotError};
use piper_sdk::client::{MotionConnectedPiper, MotionConnectedState};
use std::time::{Duration, Instant};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const INITIAL_MONITOR_SNAPSHOT_TIMEOUT: Duration = Duration::from_millis(200);
const INITIAL_MONITOR_SNAPSHOT_POLL_INTERVAL: Duration = Duration::from_millis(5);
const OBSERVATION_WINDOW: Duration = Duration::from_secs(15 * 60);
const DEFAULT_OBSERVATION_WINDOW_SECS: u64 = OBSERVATION_WINDOW.as_secs();

#[derive(Parser, Debug)]
#[command(name = "client_monitor_hil_check")]
#[command(about = "Client-side monitor HIL helper")]
struct Args {
    /// Linux: SocketCAN interface; macOS/Windows: GS-USB serial.
    #[cfg_attr(target_os = "linux", arg(long, default_value = "can0"))]
    #[cfg_attr(not(target_os = "linux"), arg(long))]
    interface: String,

    /// CAN baud rate.
    #[arg(long, default_value_t = 1_000_000)]
    baud_rate: u32,

    /// Observation window in seconds.
    #[arg(long, default_value_t = DEFAULT_OBSERVATION_WINDOW_SECS)]
    observation_window_secs: u64,
}

fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    piper_sdk::init_logger!();

    let args = Args::parse();

    println!("Piper SDK client monitor HIL helper");
    println!("====================================\n");

    let connect_started = Instant::now();
    let connected = {
        #[cfg(target_os = "linux")]
        {
            PiperBuilder::new()
                .socketcan(&args.interface)
                .baud_rate(args.baud_rate)
                .build()?
        }
        #[cfg(not(target_os = "linux"))]
        {
            PiperBuilder::new()
                .gs_usb_serial(&args.interface)
                .baud_rate(args.baud_rate)
                .build()?
        }
    };
    let connect_elapsed = connect_started.elapsed();
    println!("Connected in {:.3?}", connect_elapsed);

    if connect_elapsed > CONNECT_TIMEOUT {
        return Err(RobotError::Timeout {
            timeout_ms: CONNECT_TIMEOUT.as_millis() as u64,
        }
        .into());
    }

    match connected.require_motion()? {
        MotionConnectedPiper::Strict(MotionConnectedState::Standby(robot)) => {
            run_monitor(robot, &args)?
        },
        MotionConnectedPiper::Soft(MotionConnectedState::Standby(robot)) => {
            run_monitor(robot, &args)?
        },
        MotionConnectedPiper::Strict(MotionConnectedState::Maintenance(_))
        | MotionConnectedPiper::Soft(MotionConnectedState::Maintenance(_)) => {
            return Err("robot is not in confirmed Standby; run stop first".into());
        },
    }

    Ok(())
}

fn run_monitor<Capability>(
    robot: Piper<Standby, Capability>,
    args: &Args,
) -> std::result::Result<(), Box<dyn std::error::Error>>
where
    Capability: MotionCapability,
{
    let observation_window = Duration::from_secs(args.observation_window_secs);
    let observation_started = Instant::now();
    let observer = robot.observer().clone();

    println!("Waiting for the first feedback...");
    let first_feedback_started = Instant::now();
    wait_for_first_feedback(
        CONNECT_TIMEOUT,
        INITIAL_MONITOR_SNAPSHOT_POLL_INTERVAL,
        || observer.is_connected(),
    )?;
    println!(
        "First feedback arrived in {:.3?} (connection age {:.3?})",
        first_feedback_started.elapsed(),
        observer.connection_age()
    );

    println!("Waiting for the first complete monitor snapshot...");
    let first_snapshot_started = Instant::now();
    let initial_snapshot = wait_for_monitor_snapshot(
        INITIAL_MONITOR_SNAPSHOT_TIMEOUT,
        INITIAL_MONITOR_SNAPSHOT_POLL_INTERVAL,
        || {
            Ok(InitialMonitorSnapshot {
                joint_positions: observer.joint_positions()?,
                end_pose: observer.end_pose()?,
                control_enabled: observer.is_all_enabled_confirmed(),
            })
        },
    )?;
    let first_snapshot_elapsed = first_snapshot_started.elapsed();
    println!(
        "First complete monitor snapshot in {:.3?}",
        first_snapshot_elapsed
    );
    print_snapshot("initial", &initial_snapshot);

    let mut iteration = 0_u64;
    while observation_started.elapsed() < observation_window {
        iteration += 1;
        std::thread::sleep(Duration::from_secs(1));

        let snapshot = wait_for_monitor_snapshot(
            INITIAL_MONITOR_SNAPSHOT_TIMEOUT,
            INITIAL_MONITOR_SNAPSHOT_POLL_INTERVAL,
            || {
                Ok(InitialMonitorSnapshot {
                    joint_positions: observer.joint_positions()?,
                    end_pose: observer.end_pose()?,
                    control_enabled: observer.is_all_enabled_confirmed(),
                })
            },
        )?;
        print_snapshot(&format!("sample {iteration}"), &snapshot);
    }

    println!(
        "Observation window finished after {:.3?}",
        observation_started.elapsed()
    );

    Ok(())
}

fn print_snapshot(label: &str, snapshot: &InitialMonitorSnapshot) {
    println!(
        "{label}: J1={:.4} rad, end_pose=({:.4}, {:.4}, {:.4}), enabled={}",
        snapshot.joint_positions[0].0,
        snapshot.end_pose.end_pose[0],
        snapshot.end_pose.end_pose[1],
        snapshot.end_pose.end_pose[2],
        snapshot.control_enabled
    );
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

fn wait_for_first_feedback<Check>(
    timeout: Duration,
    poll_interval: Duration,
    mut check: Check,
) -> ClientResult<()>
where
    Check: FnMut() -> bool,
{
    let start = Instant::now();
    loop {
        if check() {
            return Ok(());
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

#[derive(Debug)]
struct InitialMonitorSnapshot {
    joint_positions: piper_sdk::client::types::JointArray<piper_sdk::client::types::Rad>,
    end_pose: piper_sdk::driver::state::EndPoseState,
    control_enabled: bool,
}

#[cfg(test)]
#[test]
fn wait_for_monitor_snapshot_retries_warmup_errors_until_ready() {
    use piper_sdk::client::types::MonitorStateSource;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let attempts = Arc::new(AtomicUsize::new(0));
    let read = {
        let attempts = Arc::clone(&attempts);
        move || {
            let current = attempts.fetch_add(1, Ordering::SeqCst);
            if current < 2 {
                Err(RobotError::monitor_state_incomplete(
                    MonitorStateSource::JointPosition,
                    0b001,
                    0b111,
                ))
            } else {
                Ok(42_u8)
            }
        }
    };

    let value =
        wait_for_monitor_snapshot(Duration::from_millis(50), Duration::from_millis(1), read)
            .expect("helper should retry until the snapshot is ready");
    assert_eq!(value, 42);
    assert_eq!(attempts.load(Ordering::SeqCst), 3);
}
