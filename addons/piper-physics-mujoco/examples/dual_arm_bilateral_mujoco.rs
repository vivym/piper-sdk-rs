//! Dual-arm bilateral teleoperation with MuJoCo dynamics compensation.
//!
//! Example usage:
//! ```bash
//! cargo run --manifest-path addons/piper-physics-mujoco/Cargo.toml --example dual_arm_bilateral_mujoco -- \
//!   --left-interface can0 \
//!   --right-interface can1 \
//!   --teleop-mode bilateral
//! ```

use std::error::Error;
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use clap::{Parser, ValueEnum};
use piper_physics::{
    DualArmMujocoCompensatorConfig, DynamicsMode, MujocoDualArmCompensator, PayloadSpec,
    SharedModeState, SharedPayloadState,
};
use piper_sdk::client::state::MitModeConfig;
use piper_sdk::prelude::*;

type AppResult<T> = std::result::Result<T, Box<dyn Error>>;

#[derive(Debug, Clone, Copy, ValueEnum)]
enum TeleopMode {
    MasterFollower,
    Bilateral,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum DynamicsModeArg {
    Gravity,
    Partial,
    Full,
}

impl From<DynamicsModeArg> for DynamicsMode {
    fn from(value: DynamicsModeArg) -> Self {
        match value {
            DynamicsModeArg::Gravity => Self::PureGravity,
            DynamicsModeArg::Partial => Self::PartialInverseDynamics,
            DynamicsModeArg::Full => Self::FullInverseDynamics,
        }
    }
}

#[derive(Debug, Parser)]
#[command(name = "dual_arm_bilateral_mujoco")]
#[command(about = "双臂 MuJoCo 重力补偿 / 双边 MIT 遥操示例")]
struct Args {
    /// 左臂 SocketCAN 接口名（Linux）
    #[arg(long)]
    left_interface: Option<String>,

    /// 右臂 SocketCAN 接口名（Linux）
    #[arg(long)]
    right_interface: Option<String>,

    /// 左臂 GS-USB 序列号
    #[arg(long)]
    left_serial: Option<String>,

    /// 右臂 GS-USB 序列号
    #[arg(long)]
    right_serial: Option<String>,

    /// MuJoCo 模型目录
    #[arg(long)]
    model_dir: Option<PathBuf>,

    /// 从标准路径加载模型
    #[arg(long)]
    use_standard_path: bool,

    /// 从嵌入 XML 加载模型（仅开发测试）
    #[arg(long)]
    use_embedded: bool,

    /// 遥操模式
    #[arg(long, value_enum, default_value = "bilateral")]
    teleop_mode: TeleopMode,

    /// 主臂补偿模式
    #[arg(long, value_enum, default_value = "gravity")]
    master_dynamics_mode: DynamicsModeArg,

    /// 从臂补偿模式
    #[arg(long, value_enum, default_value = "partial")]
    slave_dynamics_mode: DynamicsModeArg,

    /// 主臂 payload 质量（kg）
    #[arg(long, default_value_t = 0.0)]
    master_payload_mass: f64,

    /// 主臂 payload 质心（x,y,z）
    #[arg(long, value_parser = parse_vec3, default_value = "0,0,0")]
    master_payload_com: [f64; 3],

    /// 从臂 payload 质量（kg）
    #[arg(long, default_value_t = 0.0)]
    slave_payload_mass: f64,

    /// 从臂 payload 质心（x,y,z）
    #[arg(long, value_parser = parse_vec3, default_value = "0,0,0")]
    slave_payload_com: [f64; 3],

    /// 控制循环频率
    #[arg(long, default_value_t = 200.0)]
    frequency_hz: f64,

    /// 从臂跟踪 Kp
    #[arg(long, default_value_t = 8.0)]
    track_kp: f64,

    /// 从臂跟踪 Kd
    #[arg(long, default_value_t = 1.0)]
    track_kd: f64,

    /// 主臂阻尼
    #[arg(long, default_value_t = 0.4)]
    master_damping: f64,

    /// 双边反射增益
    #[arg(long, default_value_t = 0.25)]
    reflection_gain: f64,

    /// 全逆动力学加速度低通截止频率
    #[arg(long, default_value_t = 20.0)]
    qacc_lpf_cutoff_hz: f64,

    /// 全逆动力学加速度限幅
    #[arg(long, default_value_t = 50.0)]
    max_abs_qacc: f64,

    /// 禁用夹爪镜像
    #[arg(long)]
    disable_gripper_mirror: bool,

    /// 禁用被动性阻尼注入
    #[arg(long)]
    disable_passivity: bool,
}

fn main() -> AppResult<()> {
    let args = Args::parse();
    validate_args(&args)?;

    println!("=== Dual-Arm MuJoCo Bilateral Demo ===");
    println!("teleop mode: {:?}", args.teleop_mode);
    println!(
        "master dynamics: {}, slave dynamics: {}",
        DynamicsMode::from(args.master_dynamics_mode),
        DynamicsMode::from(args.slave_dynamics_mode)
    );
    println!("frequency: {:.1} Hz", args.frequency_hz);

    let master_mode = SharedModeState::new(args.master_dynamics_mode.into());
    let slave_mode = SharedModeState::new(args.slave_dynamics_mode.into());
    let master_payload = SharedPayloadState::try_new(PayloadSpec::try_new(
        args.master_payload_mass,
        args.master_payload_com,
    )?)?;
    let slave_payload = SharedPayloadState::try_new(PayloadSpec::try_new(
        args.slave_payload_mass,
        args.slave_payload_com,
    )?)?;

    print_runtime_state(&master_mode, &slave_mode, &master_payload, &slave_payload);

    let left = build_arm_builder(
        "left",
        args.left_interface.as_deref(),
        args.left_serial.as_deref(),
    )?;
    let right = build_arm_builder(
        "right",
        args.right_interface.as_deref(),
        args.right_serial.as_deref(),
    )?;

    println!("\nconnecting both arms...");
    let standby = DualArmBuilder::new(left, right).build()?;
    println!("both arms connected.");

    println!("\nmove both arms to the mirrored zero pose before calibration.");
    wait_for_enter("press Enter to capture calibration...")?;
    let calibration = standby.capture_calibration(JointMirrorMap::left_right_mirror())?;
    println!("calibration captured.");

    println!("enabling both arms in MIT mode...");
    let active = standby.enable_mit(MitModeConfig::default(), MitModeConfig::default())?;
    println!("MIT mode enabled on both arms.");

    let cancel_signal = Arc::new(AtomicBool::new(false));
    install_ctrlc(cancel_signal.clone())?;
    spawn_console(
        master_mode.clone(),
        slave_mode.clone(),
        master_payload.clone(),
        slave_payload.clone(),
        cancel_signal.clone(),
    );

    println!("\nstdin commands:");
    println!("  show");
    println!("  master payload <mass> <x> <y> <z>");
    println!("  slave payload <mass> <x> <y> <z>");
    println!("  master mode <gravity|partial|full>");
    println!("  slave mode <gravity|partial|full>");
    println!("  quit");
    println!("Ctrl+C also exits cleanly.\n");

    let compensator_cfg = DualArmMujocoCompensatorConfig {
        master_mode: master_mode.clone(),
        slave_mode: slave_mode.clone(),
        master_payload: master_payload.clone(),
        slave_payload: slave_payload.clone(),
        qacc_lpf_cutoff_hz: args.qacc_lpf_cutoff_hz,
        max_abs_qacc: args.max_abs_qacc,
    };

    let loop_cfg = BilateralLoopConfig {
        frequency_hz: args.frequency_hz,
        cancel_signal: Some(cancel_signal.clone()),
        gripper: GripperTeleopConfig {
            enabled: !args.disable_gripper_mirror,
            ..Default::default()
        },
        master_passivity_enabled: !args.disable_passivity,
        ..Default::default()
    };

    let exit = match args.teleop_mode {
        TeleopMode::MasterFollower => {
            let controller = MasterFollowerController::new(calibration)
                .with_track_gains(
                    JointArray::splat(args.track_kp),
                    JointArray::splat(args.track_kd),
                )
                .with_master_damping(JointArray::splat(args.master_damping));
            let compensator = build_compensator(&args, compensator_cfg.clone())?;
            active.run_bilateral_with_compensation(controller, compensator, loop_cfg)?
        },
        TeleopMode::Bilateral => {
            let controller = JointSpaceBilateralController::new(calibration)
                .with_track_gains(
                    JointArray::splat(args.track_kp),
                    JointArray::splat(args.track_kd),
                )
                .with_master_damping(JointArray::splat(args.master_damping))
                .with_reflection_gain(JointArray::splat(args.reflection_gain));
            let compensator = build_compensator(&args, compensator_cfg)?;
            active.run_bilateral_with_compensation(controller, compensator, loop_cfg)?
        },
    };

    cancel_signal.store(true, Ordering::Release);

    match exit {
        DualArmLoopExit::Standby { report, .. } => {
            println!("\ndual-arm loop exited cleanly to Standby.");
            print_report(&report);
        },
        DualArmLoopExit::Faulted { report, .. } => {
            eprintln!("\ndual-arm loop entered Faulted after a bounded shutdown attempt.");
            print_report(&report);
        },
    }

    Ok(())
}

fn validate_args(args: &Args) -> AppResult<()> {
    if args.left_interface.is_some() && args.left_serial.is_some() {
        return Err("left arm cannot specify both interface and serial".into());
    }
    if args.right_interface.is_some() && args.right_serial.is_some() {
        return Err("right arm cannot specify both interface and serial".into());
    }
    if args.frequency_hz <= 0.0 {
        return Err("frequency_hz must be > 0".into());
    }
    if args.track_kp < 0.0 || args.track_kd < 0.0 || args.master_damping < 0.0 {
        return Err("gains must be >= 0".into());
    }
    if args.reflection_gain < 0.0 {
        return Err("reflection_gain must be >= 0".into());
    }
    let model_flags = usize::from(args.model_dir.is_some())
        + usize::from(args.use_standard_path)
        + usize::from(args.use_embedded);
    if model_flags > 1 {
        return Err("choose only one of --model-dir, --use-standard-path, --use-embedded".into());
    }
    if !cfg!(target_os = "linux")
        && (args.left_interface.is_some() || args.right_interface.is_some())
    {
        return Err("SocketCAN is only supported on Linux".into());
    }
    Ok(())
}

fn build_compensator(
    args: &Args,
    config: DualArmMujocoCompensatorConfig,
) -> AppResult<MujocoDualArmCompensator> {
    if let Some(dir) = &args.model_dir {
        println!("MuJoCo model source: {}", dir.display());
        return Ok(MujocoDualArmCompensator::from_model_dir_pair(dir, config)?);
    }
    if args.use_embedded {
        println!("MuJoCo model source: embedded XML");
        return Ok(MujocoDualArmCompensator::from_embedded_pair(config)?);
    }

    println!("MuJoCo model source: standard path search");
    Ok(MujocoDualArmCompensator::from_standard_path_pair(config)?)
}

fn build_arm_builder(
    role: &str,
    interface: Option<&str>,
    serial: Option<&str>,
) -> AppResult<PiperBuilder> {
    let builder = PiperBuilder::new();

    if let Some(serial) = serial {
        println!("{role} arm target: gs_usb_serial={serial}");
        return Ok(builder.gs_usb_serial(serial));
    }

    if let Some(interface) = interface {
        println!("{role} arm target: socketcan={interface}");
        return Ok(builder.socketcan(interface));
    }

    #[cfg(target_os = "linux")]
    {
        let default_iface = if role == "left" { "can0" } else { "can1" };
        println!("{role} arm target: socketcan={default_iface} (default)");
        Ok(builder.socketcan(default_iface))
    }

    #[cfg(not(target_os = "linux"))]
    {
        Err(format!(
            "{role} arm requires --{role}-serial on this platform; dual-arm auto scan is not supported"
        )
        .into())
    }
}

fn install_ctrlc(cancel_signal: Arc<AtomicBool>) -> AppResult<()> {
    ctrlc::set_handler(move || {
        if !cancel_signal.swap(true, Ordering::AcqRel) {
            eprintln!("\nreceived Ctrl+C, stopping dual-arm loop...");
        }
    })?;
    Ok(())
}

fn spawn_console(
    master_mode: SharedModeState,
    slave_mode: SharedModeState,
    master_payload: SharedPayloadState,
    slave_payload: SharedPayloadState,
    cancel_signal: Arc<AtomicBool>,
) {
    thread::spawn(move || {
        let stdin = io::stdin();
        loop {
            if cancel_signal.load(Ordering::Acquire) {
                break;
            }

            print!("dual-arm> ");
            if io::stdout().flush().is_err() {
                break;
            }

            let mut line = String::new();
            match stdin.read_line(&mut line) {
                Ok(0) => {
                    cancel_signal.store(true, Ordering::Release);
                    break;
                },
                Ok(_) => {
                    if let Err(error) = handle_console_command(
                        line.trim(),
                        &master_mode,
                        &slave_mode,
                        &master_payload,
                        &slave_payload,
                        &cancel_signal,
                    ) {
                        eprintln!("console error: {error}");
                    }
                },
                Err(error) => {
                    eprintln!("stdin read error: {error}");
                    cancel_signal.store(true, Ordering::Release);
                    break;
                },
            }
        }
    });
}

fn handle_console_command(
    line: &str,
    master_mode: &SharedModeState,
    slave_mode: &SharedModeState,
    master_payload: &SharedPayloadState,
    slave_payload: &SharedPayloadState,
    cancel_signal: &Arc<AtomicBool>,
) -> AppResult<()> {
    if line.is_empty() {
        return Ok(());
    }

    let tokens: Vec<&str> = line.split_whitespace().collect();
    match tokens.as_slice() {
        ["show"] => {
            print_runtime_state(master_mode, slave_mode, master_payload, slave_payload);
        },
        ["quit"] => {
            cancel_signal.store(true, Ordering::Release);
        },
        [arm, "mode", mode] => {
            let parsed = mode.parse::<DynamicsMode>()?;
            match *arm {
                "master" => master_mode.store(parsed),
                "slave" => slave_mode.store(parsed),
                _ => return Err(format!("unknown arm selector: {arm}").into()),
            }
            print_runtime_state(master_mode, slave_mode, master_payload, slave_payload);
        },
        [arm, "payload", mass, x, y, z] => {
            let spec = PayloadSpec::try_new(mass.parse()?, [x.parse()?, y.parse()?, z.parse()?])?;
            match *arm {
                "master" => master_payload.store(spec)?,
                "slave" => slave_payload.store(spec)?,
                _ => return Err(format!("unknown arm selector: {arm}").into()),
            }
            print_runtime_state(master_mode, slave_mode, master_payload, slave_payload);
        },
        _ => {
            return Err(
                "unknown command; expected show | <master|slave> mode <...> | <master|slave> payload <mass> <x> <y> <z> | quit"
                    .into(),
            );
        },
    }
    Ok(())
}

fn print_runtime_state(
    master_mode: &SharedModeState,
    slave_mode: &SharedModeState,
    master_payload: &SharedPayloadState,
    slave_payload: &SharedPayloadState,
) {
    let master_payload = master_payload.load();
    let slave_payload = slave_payload.load();
    println!(
        "master mode={}, payload={}kg @ [{:.3}, {:.3}, {:.3}]",
        master_mode.load(),
        master_payload.mass_kg,
        master_payload.com_m[0],
        master_payload.com_m[1],
        master_payload.com_m[2]
    );
    println!(
        "slave mode={}, payload={}kg @ [{:.3}, {:.3}, {:.3}]",
        slave_mode.load(),
        slave_payload.mass_kg,
        slave_payload.com_m[0],
        slave_payload.com_m[1],
        slave_payload.com_m[2]
    );
}

fn wait_for_enter(prompt: &str) -> io::Result<()> {
    print!("{prompt}");
    io::stdout().flush()?;
    let mut line = String::new();
    io::stdin().read_line(&mut line)?;
    Ok(())
}

fn print_report(report: &BilateralRunReport) {
    println!("iterations: {}", report.iterations);
    println!("exit_reason: {:?}", report.exit_reason);
    println!("read_faults: {}", report.read_faults);
    println!("submission_faults: {}", report.submission_faults);
    println!("left_stop_attempt: {:?}", report.left_stop_attempt);
    println!("right_stop_attempt: {:?}", report.right_stop_attempt);
    println!(
        "max_inter_arm_skew: {} us",
        report.max_inter_arm_skew.as_micros()
    );
    println!(
        "left tx realtime overwrites: {}",
        report.left_tx_realtime_overwrites
    );
    println!(
        "right tx realtime overwrites: {}",
        report.right_tx_realtime_overwrites
    );
    println!("left tx frames total: {}", report.left_tx_frames_total);
    println!("right tx frames total: {}", report.right_tx_frames_total);
    println!("last_runtime_fault_left: {:?}", report.last_runtime_fault_left);
    println!("last_runtime_fault_right: {:?}", report.last_runtime_fault_right);
    if let Some(last_error) = &report.last_error {
        println!("last_error: {last_error}");
    }
}

fn parse_vec3(value: &str) -> std::result::Result<[f64; 3], String> {
    let parts: Vec<_> = value.split(',').collect();
    if parts.len() != 3 {
        return Err("expected x,y,z".to_string());
    }
    let x = parts[0]
        .trim()
        .parse()
        .map_err(|error| format!("invalid x component: {error}"))?;
    let y = parts[1]
        .trim()
        .parse()
        .map_err(|error| format!("invalid y component: {error}"))?;
    let z = parts[2]
        .trim()
        .parse()
        .map_err(|error| format!("invalid z component: {error}"))?;
    Ok([x, y, z])
}
