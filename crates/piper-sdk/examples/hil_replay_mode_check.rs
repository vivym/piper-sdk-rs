use clap::Parser;
use piper_sdk::PiperBuilder;
use piper_sdk::client::state::{MotionCapability, Piper, Standby};
use piper_sdk::client::{MotionConnectedPiper, MotionConnectedState};
use piper_tools::PiperRecording;
use std::error::Error;
use std::path::PathBuf;

const MAX_REPLAY_SPEED: f64 = 5.0;

#[derive(Parser, Debug)]
#[command(name = "hil_replay_mode_check")]
#[command(about = "Safe replay-mode HIL helper for one real Piper arm")]
struct Args {
    /// Linux: SocketCAN interface; macOS/Windows: GS-USB serial.
    #[cfg_attr(target_os = "linux", arg(long, default_value = "can0"))]
    #[cfg_attr(not(target_os = "linux"), arg(long))]
    interface: String,

    /// CAN baud rate.
    #[arg(long, default_value_t = 1_000_000)]
    baud_rate: u32,

    /// Recording file to replay.
    #[arg(long)]
    recording_file: PathBuf,

    /// Replay speed multiplier.
    #[arg(long, default_value_t = 1.0)]
    speed: f64,
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
    let connected = build_connected(args)?.require_motion()?;

    match connected {
        MotionConnectedPiper::Strict(MotionConnectedState::Standby(standby)) => {
            println!("[PASS] connected and confirmed Standby");
            run_replay_check(standby, args)
        },
        MotionConnectedPiper::Soft(MotionConnectedState::Standby(standby)) => {
            println!("[PASS] connected and confirmed Standby");
            run_replay_check(standby, args)
        },
        MotionConnectedPiper::Strict(MotionConnectedState::Maintenance(_))
        | MotionConnectedPiper::Soft(MotionConnectedState::Maintenance(_)) => {
            Err("robot is not in confirmed Standby; run stop first".into())
        },
    }
}

fn build_connected(args: &Args) -> Result<piper_sdk::ConnectedPiper, Box<dyn Error>> {
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

fn run_replay_check<C>(standby: Piper<Standby, C>, args: &Args) -> Result<(), Box<dyn Error>>
where
    C: MotionCapability,
{
    let recording = PiperRecording::load(&args.recording_file)?;
    println!(
        "[PASS] loaded recording file={} frames={} speed={:.2}x",
        args.recording_file.display(),
        recording.frames.len(),
        args.speed
    );

    let replay = standby.enter_replay_mode()?;
    println!("[PASS] entered ReplayMode");

    let standby = replay.replay_recording(&args.recording_file, args.speed)?;
    if !standby.observer().is_all_disabled_confirmed() {
        return Err("replay finished without confirmed Standby".into());
    }

    println!("[PASS] replay completed and returned to confirmed Standby");
    println!("[PASS] hil_replay_mode_check complete");
    Ok(())
}

fn validate_args(args: &Args) -> Result<(), String> {
    if !args.speed.is_finite() {
        return Err("speed must be finite".to_string());
    }
    if args.speed <= 0.0 {
        return Err("speed must be > 0".to_string());
    }
    if args.speed > MAX_REPLAY_SPEED {
        return Err("speed must be <= 5.0".to_string());
    }
    Ok(())
}

#[test]
fn validate_args_rejects_invalid_replay_speed() {
    let args = Args {
        interface: "can0".to_string(),
        baud_rate: 1_000_000,
        recording_file: PathBuf::from("demo_recording.bin"),
        speed: 0.0,
    };

    let error = validate_args(&args).expect_err("speed 0 must be rejected");
    assert!(error.contains("speed"));
}

#[test]
fn validate_args_rejects_excessive_replay_speed() {
    let args = Args {
        interface: "can0".to_string(),
        baud_rate: 1_000_000,
        recording_file: PathBuf::from("demo_recording.bin"),
        speed: 6.0,
    };

    let error = validate_args(&args).expect_err("speed above 5.0 must be rejected");
    assert!(error.contains("speed"));
}
