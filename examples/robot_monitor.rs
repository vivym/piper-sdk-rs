//! 机器人实时监控工具
//!
//! 此示例演示如何连接到松灵 Piper 机械臂并实时监控反馈信息。
//! 特点：
//! - 持续循环读取状态（1Hz 刷新频率）
//! - 显示关节位置、速度、电流等实时数据
//! - 包含中文状态转换函数，便于理解
//! - 支持 Ctrl+C 优雅退出
//! - 支持通过 UDS 连接守护进程（macOS/Windows）
//!
//! **注意**：此示例用于实时监控，不发送任何控制指令，仅被动监听。
//! 如需学习 API 用法，请参考 `state_api_demo` 示例。
//!
//! 使用方式：
//! ```bash
//! # 直接连接（Linux: SocketCAN, macOS/Windows: GS-USB）
//! cargo run --example robot_monitor
//!
//! # 通过 UDS 连接守护进程（macOS/Windows）
//! cargo run --example robot_monitor -- --uds /tmp/gs_usb_daemon.sock
//!
//! # 指定 CAN 接口（Linux）
//! cargo run --example robot_monitor -- --interface can0
//! ```

use clap::Parser;
use piper_sdk::robot::{
    EndPoseState, FpsResult, GripperState, JointDynamicState, JointPositionState, PiperBuilder,
    RobotControlState,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// 命令行参数
#[derive(Parser, Debug)]
#[command(name = "robot_monitor")]
#[command(about = "机器人实时监控工具")]
struct Args {
    /// CAN 接口名称（Linux: "can0", macOS/Windows: 设备序列号）
    #[arg(long)]
    interface: Option<String>,

    /// CAN 波特率（默认: 1000000）
    #[arg(long, default_value = "1000000")]
    baud_rate: u32,

    /// UDS Socket 路径（通过守护进程连接，macOS/Windows）
    ///
    /// 如果指定此参数，将通过 gs_usb_daemon 连接，而不是直接连接 GS-USB 设备。
    /// 默认: /tmp/gs_usb_daemon.sock
    #[arg(long)]
    uds: Option<String>,
}

/// 控制模式转换为字符串
fn control_mode_to_string(mode: u8) -> &'static str {
    match mode {
        0x00 => "待机模式",
        0x01 => "CAN指令控制模式",
        0x02 => "示教模式",
        0x03 => "以太网控制模式",
        0x04 => "wifi控制模式",
        0x05 => "遥控器控制模式",
        0x06 => "联动示教输入模式",
        0x07 => "离线轨迹模式",
        _ => "未知模式",
    }
}

/// 机器人状态转换为字符串
fn robot_status_to_string(status: u8) -> &'static str {
    match status {
        0x00 => "正常",
        0x01 => "急停",
        0x02 => "无解",
        0x03 => "奇异点",
        0x04 => "目标角度超过限",
        0x05 => "关节通信异常",
        0x06 => "关节抱闸未打开",
        0x07 => "机械臂发生碰撞",
        0x08 => "拖动示教时超速",
        0x09 => "关节状态异常",
        0x0A => "其它异常",
        0x0B => "示教记录",
        0x0C => "示教执行",
        0x0D => "示教暂停",
        0x0E => "主控NTC过温",
        0x0F => "释放电阻NTC过温",
        _ => "未知状态",
    }
}

/// MOVE模式转换为字符串
fn move_mode_to_string(mode: u8) -> &'static str {
    match mode {
        0x00 => "MOVE P",
        0x01 => "MOVE J",
        0x02 => "MOVE L",
        0x03 => "MOVE C",
        0x04 => "MOVE M",
        _ => "未知",
    }
}

/// 运动状态转换为字符串
fn motion_status_to_string(status: u8) -> &'static str {
    match status {
        0x00 => "到达指定点位",
        0x01 => "未到达指定点位",
        _ => "未知",
    }
}

/// 打印反馈信息
fn print_feedback(
    joint_position: &JointPositionState,
    end_pose: &EndPoseState,
    joint_dynamic: &JointDynamicState,
    robot_control: &RobotControlState,
    gripper: &GripperState,
    fps: &FpsResult,
) {
    // 清屏（可选，用于实时刷新效果）
    // print!("\x1B[2J\x1B[1;1H"); // Unix/Linux/macOS
    // 或者保留历史记录，每次打印新的一行

    println!("========================================");

    // FPS 统计
    println!("\n状态更新频率 (FPS):");
    println!("  关节位置状态: {:6.2} Hz", fps.joint_position);
    println!("  末端位姿状态: {:6.2} Hz", fps.end_pose);
    println!("  关节动态状态: {:6.2} Hz", fps.joint_dynamic);
    println!("  机器人控制状态: {:6.2} Hz", fps.robot_control);
    println!("  夹爪状态:     {:6.2} Hz", fps.gripper);

    // 控制状态
    println!(
        "控制模式: {}",
        control_mode_to_string(robot_control.control_mode)
    );
    println!(
        "机器人状态: {}",
        robot_status_to_string(robot_control.robot_status)
    );
    println!("MOVE模式: {}", move_mode_to_string(robot_control.move_mode));
    println!(
        "运动状态: {}",
        motion_status_to_string(robot_control.motion_status)
    );

    // 关节角度（弧度转度）
    println!("\n关节角度 (°):");
    for (i, &angle) in joint_position.joint_pos.iter().enumerate() {
        let angle_deg = angle.to_degrees();
        print!("  J{}: {:7.2}", i + 1, angle_deg);
    }
    println!();

    // 末端位姿（米）
    println!("\n末端位置 (m):");
    println!(
        "  X: {:7.4}  Y: {:7.4}  Z: {:7.4}",
        end_pose.end_pose[0], end_pose.end_pose[1], end_pose.end_pose[2]
    );

    println!("\n末端姿态 (rad):");
    println!(
        "  Rx: {:7.4}  Ry: {:7.4}  Rz: {:7.4}",
        end_pose.end_pose[3], end_pose.end_pose[4], end_pose.end_pose[5]
    );

    // 关节速度
    println!("\n关节速度 (rad/s):");
    for (i, &vel) in joint_dynamic.joint_vel.iter().enumerate() {
        print!("  J{}: {:7.3}", i + 1, vel);
    }
    println!();

    // 关节电流
    println!("\n关节电流 (A):");
    for (i, &current) in joint_dynamic.joint_current.iter().enumerate() {
        print!("  J{}: {:7.3}", i + 1, current);
    }
    println!();

    // 数据完整性检查
    if joint_dynamic.is_complete() {
        println!("\n✓ 所有关节数据完整");
    } else {
        let missing = joint_dynamic.missing_joints();
        println!("\n⚠ 缺失关节: {:?}", missing);
    }

    // 夹爪状态
    println!("\n夹爪状态:");
    println!("  行程: {:6.2} mm", gripper.travel);
    println!("  扭矩: {:6.3} N·m", gripper.torque);
    println!(
        "  是否在运动: {}",
        if gripper.is_moving() { "是" } else { "否" }
    );

    // 故障检测
    let has_faults = (0..6).any(|i| robot_control.is_angle_limit(i))
        || (0..6).any(|i| robot_control.is_comm_error(i));
    if has_faults {
        println!("\n⚠ 故障检测:");
        for i in 0..6 {
            if robot_control.is_angle_limit(i) {
                println!("  J{} 角度超限位", i + 1);
            }
        }
        for i in 0..6 {
            if robot_control.is_comm_error(i) {
                println!("  J{} 通信异常", i + 1);
            }
        }
    }

    println!("========================================\n");
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 解析命令行参数
    let args = Args::parse();

    // 设置 Ctrl+C 处理
    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();

    ctrlc::set_handler(move || {
        r.store(false, Ordering::SeqCst);
        println!("\n收到退出信号，正在关闭...");
    })?;

    println!("正在连接到机械臂...");

    // 1. 创建 Piper 实例
    let builder = {
        #[cfg(target_os = "linux")]
        {
            // Linux: SocketCAN
            let interface = args.interface.as_deref().unwrap_or("can0");
            println!("使用 CAN 接口: {}", interface);
            PiperBuilder::new().interface(interface)
        }
        #[cfg(not(target_os = "linux"))]
        {
            // macOS/Windows: GS-USB 或守护进程模式
            if let Some(uds_path) = &args.uds {
                // 守护进程模式（UDS）
                println!("使用守护进程模式 (UDS): {}", uds_path);
                PiperBuilder::new().with_daemon(uds_path)
            } else if let Some(interface) = &args.interface {
                // 直接连接，指定设备序列号
                println!("使用设备序列号: {}", interface);
                PiperBuilder::new().interface(interface)
            } else {
                // 直接连接，自动检测设备
                println!("使用默认 CAN 接口配置（自动检测设备）");
                PiperBuilder::new()
            }
        }
    };

    let piper = builder
        .baud_rate(args.baud_rate)  // CAN 波特率（默认 1M）
        .build()
        .map_err(|e| format!("连接失败: {}", e))?;

    println!("✓ 已连接到机械臂");
    println!("正在监听反馈信息...");
    println!("按 Ctrl+C 退出\n");

    // 2. 等待初始反馈（给设备一点时间建立连接）
    std::thread::sleep(Duration::from_millis(100));

    // 3. 主循环：定期读取并打印反馈
    let mut iteration = 0u64;
    while running.load(Ordering::SeqCst) {
        iteration += 1;

        // 读取各种状态
        let joint_position = piper.get_joint_position();
        let end_pose = piper.get_end_pose();
        let joint_dynamic = piper.get_joint_dynamic();
        let robot_control = piper.get_robot_control();
        let gripper = piper.get_gripper();
        let fps = piper.get_fps();

        // 打印反馈信息
        println!("[第 {} 次更新]", iteration);
        print_feedback(
            &joint_position,
            &end_pose,
            &joint_dynamic,
            &robot_control,
            &gripper,
            &fps,
        );

        // 控制刷新频率（1Hz，每秒打印一次）
        std::thread::sleep(Duration::from_secs(1));
    }

    println!("✓ 已关闭");
    Ok(())
}
