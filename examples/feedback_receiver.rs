//! 机械臂反馈接收示例
//!
//! 此示例演示如何连接到松灵 Piper 机械臂并接收反馈信息，
//! 不发送任何控制指令，仅被动监听。

use piper_sdk::robot::{ControlStatusState, CoreMotionState, JointDynamicState, PiperBuilder};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

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
    core_motion: &CoreMotionState,
    joint_dynamic: &JointDynamicState,
    control_status: &ControlStatusState,
) {
    // 清屏（可选，用于实时刷新效果）
    // print!("\x1B[2J\x1B[1;1H"); // Unix/Linux/macOS
    // 或者保留历史记录，每次打印新的一行

    println!("========================================");

    // 控制状态
    println!(
        "控制模式: {}",
        control_mode_to_string(control_status.control_mode)
    );
    println!(
        "机器人状态: {}",
        robot_status_to_string(control_status.robot_status)
    );
    println!(
        "MOVE模式: {}",
        move_mode_to_string(control_status.move_mode)
    );
    println!(
        "运动状态: {}",
        motion_status_to_string(control_status.motion_status)
    );

    // 关节角度（弧度转度）
    println!("\n关节角度 (°):");
    for (i, &angle) in core_motion.joint_pos.iter().enumerate() {
        let angle_deg = angle.to_degrees();
        print!("  J{}: {:7.2}", i + 1, angle_deg);
    }
    println!();

    // 末端位姿（米）
    println!("\n末端位置 (m):");
    println!(
        "  X: {:7.4}  Y: {:7.4}  Z: {:7.4}",
        core_motion.end_pose[0], core_motion.end_pose[1], core_motion.end_pose[2]
    );

    println!("\n末端姿态 (rad):");
    println!(
        "  Rx: {:7.4}  Ry: {:7.4}  Rz: {:7.4}",
        core_motion.end_pose[3], core_motion.end_pose[4], core_motion.end_pose[5]
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
    println!("  行程: {:6.2} mm", control_status.gripper_travel);
    println!("  扭矩: {:6.3} N·m", control_status.gripper_torque);

    // 故障检测
    let has_faults = control_status.fault_angle_limit.iter().any(|&x| x)
        || control_status.fault_comm_error.iter().any(|&x| x);
    if has_faults {
        println!("\n⚠ 故障检测:");
        for (i, &limit) in control_status.fault_angle_limit.iter().enumerate() {
            if limit {
                println!("  J{} 角度超限位", i + 1);
            }
        }
        for (i, &comm) in control_status.fault_comm_error.iter().enumerate() {
            if comm {
                println!("  J{} 通信异常", i + 1);
            }
        }
    }

    println!("========================================\n");
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 设置 Ctrl+C 处理
    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();

    ctrlc::set_handler(move || {
        r.store(false, Ordering::SeqCst);
        println!("\n收到退出信号，正在关闭...");
    })?;

    println!("正在连接到机械臂...");

    // 1. 创建 Piper 实例
    // Linux: 使用 can0 接口；其他平台: 使用默认配置
    let mut builder = PiperBuilder::new();

    #[cfg(target_os = "linux")]
    {
        builder = builder.interface("can0");
        println!("使用 CAN 接口: can0");
    }

    #[cfg(not(target_os = "linux"))]
    {
        println!("使用默认 CAN 接口配置");
    }

    let piper = builder
        .baud_rate(1_000_000)  // CAN 波特率 1M (协议要求)
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
        let core_motion = piper.get_core_motion();
        let joint_dynamic = piper.get_joint_dynamic();
        let control_status = piper.get_control_status();

        // 打印反馈信息
        println!("[第 {} 次更新]", iteration);
        print_feedback(&core_motion, &joint_dynamic, &control_status);

        // 控制刷新频率（1Hz，每秒打印一次）
        std::thread::sleep(Duration::from_secs(1));
    }

    println!("✓ 已关闭");
    Ok(())
}
