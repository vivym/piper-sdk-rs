//! 双臂 MIT 遥操 / 双边控制示例
//!
//! 本示例基于 SDK 内置的 `dual_arm` 公共 API，演示：
//! - 两条独立 CAN/GS-USB 链路下的双臂会话创建
//! - 基于当前姿态抓取的左右臂零位标定
//! - `master-follower` 与 `bilateral` 两种模式切换
//! - Ctrl+C 触发的优雅退出
//!
//! 推荐使用方式：
//! ```bash
//! # Linux，默认 can0/can1，先用单向镜像联调
//! cargo run --example dual_arm_bilateral_control -- --mode master-follower
//!
//! # Linux，显式指定左右 SocketCAN 接口
//! cargo run --example dual_arm_bilateral_control -- \
//!   --left-interface can0 \
//!   --right-interface can1 \
//!   --mode bilateral
//!
//! # macOS/Windows，显式指定两只 GS-USB 设备序列号
//! cargo run --example dual_arm_bilateral_control -- \
//!   --left-serial LEFT123 \
//!   --right-serial RIGHT456 \
//!   --mode bilateral
//! ```

use std::error::Error;
use std::io::{self, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use clap::{Parser, ValueEnum};
use piper_sdk::client::state::MitModeConfig;
use piper_sdk::prelude::*;

#[derive(Debug, Clone, Copy, ValueEnum)]
enum TeleopMode {
    MasterFollower,
    Bilateral,
}

#[derive(Debug, Parser)]
#[command(name = "dual_arm_bilateral_control")]
#[command(about = "双臂 MIT 遥操 / 双边控制示例")]
struct Args {
    /// 控制模式
    #[arg(long, value_enum, default_value = "master-follower")]
    mode: TeleopMode,

    /// 左臂 SocketCAN 接口名（Linux）
    #[arg(long)]
    left_interface: Option<String>,

    /// 右臂 SocketCAN 接口名（Linux）
    #[arg(long)]
    right_interface: Option<String>,

    /// 左臂 GS-USB 设备序列号
    #[arg(long)]
    left_serial: Option<String>,

    /// 右臂 GS-USB 设备序列号
    #[arg(long)]
    right_serial: Option<String>,

    /// CAN 波特率
    #[arg(long, default_value_t = 1_000_000)]
    baud_rate: u32,

    /// 双臂主循环频率
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

    /// 力反射增益，仅 bilateral 模式生效
    #[arg(long, default_value_t = 0.25)]
    reflection_gain: f64,

    /// 禁用夹爪镜像
    #[arg(long)]
    disable_gripper_mirror: bool,

    /// 禁用被动性阻尼注入
    #[arg(long)]
    disable_passivity: bool,
}

fn main() -> std::result::Result<(), Box<dyn Error>> {
    piper_sdk::init_logger!();

    let args = Args::parse();
    validate_args(&args)?;

    println!("=== Dual-Arm MIT Teleoperation Demo ===");
    println!("模式: {:?}", args.mode);
    println!("频率: {:.1} Hz", args.frequency_hz);
    println!(
        "夹爪镜像: {}",
        if args.disable_gripper_mirror {
            "disabled"
        } else {
            "enabled"
        }
    );
    println!(
        "被动性阻尼: {}",
        if args.disable_passivity {
            "disabled"
        } else {
            "enabled"
        }
    );

    let left = build_arm_builder(
        "left",
        args.left_interface.as_deref(),
        args.left_serial.as_deref(),
        args.baud_rate,
    )?;
    let right = build_arm_builder(
        "right",
        args.right_interface.as_deref(),
        args.right_serial.as_deref(),
        args.baud_rate,
    )?;

    println!("\n连接双臂...");
    let standby = DualArmBuilder::new(left, right).build()?;
    println!("双臂已连接。");

    println!("\n把两条机械臂手动摆到镜像零位。");
    println!("建议：先确认工作空间无遮挡，再进行标定。");
    wait_for_enter("准备好后按 Enter 抓取标定零位...")?;

    let calibration = standby.capture_calibration(JointMirrorMap::left_right_mirror())?;
    println!("标定完成，开始切换双臂到 MIT 模式...");

    let active = standby.enable_mit(MitModeConfig::default(), MitModeConfig::default())?;
    println!("双臂已进入 MIT 模式。按 Ctrl+C 安全退出。");

    let cancel_signal = Arc::new(AtomicBool::new(false));
    install_ctrlc(cancel_signal.clone())?;

    let cfg = BilateralLoopConfig {
        frequency_hz: args.frequency_hz,
        cancel_signal: Some(cancel_signal),
        gripper: GripperTeleopConfig {
            enabled: !args.disable_gripper_mirror,
            ..Default::default()
        },
        master_passivity_enabled: !args.disable_passivity,
        ..Default::default()
    };

    let exit = match args.mode {
        TeleopMode::MasterFollower => {
            let controller = MasterFollowerController::new(calibration)
                .with_track_gains(
                    JointArray::splat(args.track_kp),
                    JointArray::splat(args.track_kd),
                )
                .with_master_damping(JointArray::splat(args.master_damping));
            active.run_bilateral(controller, cfg)?
        },
        TeleopMode::Bilateral => {
            let controller = JointSpaceBilateralController::new(calibration)
                .with_track_gains(
                    JointArray::splat(args.track_kp),
                    JointArray::splat(args.track_kd),
                )
                .with_master_damping(JointArray::splat(args.master_damping))
                .with_reflection_gain(JointArray::splat(args.reflection_gain));
            active.run_bilateral(controller, cfg)?
        },
    };

    match exit {
        DualArmLoopExit::Standby { report, .. } => {
            println!("\n双臂循环已退出，机械臂回到 Standby。");
            print_report(&report);
        },
        DualArmLoopExit::EmergencyStopped { report, .. } => {
            eprintln!("\n双臂循环因故障进入 EmergencyStopped。");
            print_report(&report);
        },
    }

    Ok(())
}

fn validate_args(args: &Args) -> std::result::Result<(), Box<dyn Error>> {
    if args.left_interface.is_some() && args.left_serial.is_some() {
        return Err("left arm target cannot specify both interface and serial".into());
    }
    if args.right_interface.is_some() && args.right_serial.is_some() {
        return Err("right arm target cannot specify both interface and serial".into());
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
    Ok(())
}

fn build_arm_builder(
    role: &str,
    interface: Option<&str>,
    serial: Option<&str>,
    baud_rate: u32,
) -> std::result::Result<PiperBuilder, Box<dyn Error>> {
    let builder = PiperBuilder::new().baud_rate(baud_rate);

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

fn install_ctrlc(cancel_signal: Arc<AtomicBool>) -> std::result::Result<(), Box<dyn Error>> {
    ctrlc::set_handler(move || {
        if !cancel_signal.swap(true, Ordering::AcqRel) {
            eprintln!("\n收到 Ctrl+C，准备退出双臂控制循环...");
        }
    })?;
    Ok(())
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
    println!("read_faults: {}", report.read_faults);
    println!("command_faults: {}", report.command_faults);
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
    if let Some(last_error) = &report.last_error {
        println!("last_error: {last_error}");
    }
}
