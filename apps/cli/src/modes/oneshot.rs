//! One-shot 模式
//!
//! 每个命令独立执行：
//! 1. 读取配置
//! 2. 连接机器人
//! 3. 执行操作
//! 4. 断开连接

use anyhow::Result;
use piper_client::PiperBuilder as ClientPiperBuilder;
use piper_sdk::driver::{
    EndPoseState, FpsResult, GripperState, JointDynamicState, JointPositionState,
    PiperBuilder as DriverPiperBuilder, RobotControlState,
};
use piper_tools::SafetyConfig;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::signal;

use crate::commands::{MoveCommand, PositionCommand, RecordCommand, StopCommand};
use crate::safety;

/// One-shot 模式配置
#[derive(Debug, Clone)]
pub struct OneShotConfig {
    /// CAN 接口
    pub interface: Option<String>,

    /// 设备序列号
    #[allow(dead_code)]
    pub serial: Option<String>,

    /// 安全配置
    pub safety: SafetyConfig,
}

impl OneShotConfig {
    /// 从命令行参数创建配置
    pub fn from_args(interface: Option<String>, serial: Option<String>) -> Self {
        Self {
            interface,
            serial,
            safety: SafetyConfig::default_config(),
        }
    }
}

/// One-shot 模式
pub struct OneShotMode {
    config: OneShotConfig,
}

impl OneShotMode {
    /// 创建新的 One-shot 模式实例
    pub async fn new() -> Result<Self> {
        // ✅ 从配置文件加载
        use crate::commands::config::CliConfig;

        let cli_config = CliConfig::load().unwrap_or_else(|e| {
            eprintln!("⚠️  加载配置文件失败: {}，使用默认配置", e);
            CliConfig::default()
        });

        let config = OneShotConfig {
            interface: cli_config.interface,
            serial: cli_config.serial,
            safety: SafetyConfig::default_config(),
        };

        Ok(Self { config })
    }

    /// 移动命令
    pub async fn move_to(&mut self, args: MoveCommand) -> Result<()> {
        // 安全检查
        let positions = args.parse_joints()?;

        if args.requires_confirmation(&positions, &self.config.safety) {
            // ✅ 使用 SafetyChecker 的确认方法
            let checker = safety::SafetyChecker::new();
            let confirmed = checker.show_confirmation_prompt(&positions)?;

            if !confirmed {
                println!("❌ 操作已取消");
                return Ok(());
            }

            println!("✅ 已确认");
        }

        // ✅ execute() 方法内部会处理连接和移动
        let config = OneShotConfig::from_args(args.interface.clone(), args.serial.clone());
        args.execute(&config).await?;

        Ok(())
    }

    /// 位置查询
    pub async fn get_position(&mut self, args: PositionCommand) -> Result<()> {
        // ✅ execute() 方法内部会处理连接
        let config = OneShotConfig::from_args(args.interface.clone(), args.serial.clone());
        args.execute(&config).await?;

        Ok(())
    }

    /// 急停
    pub async fn stop(&mut self, args: StopCommand) -> Result<()> {
        // ✅ execute() 方法内部会处理连接
        let config = OneShotConfig::from_args(args.interface.clone(), args.serial.clone());
        args.execute(&config).await?;

        Ok(())
    }

    /// 回零位
    pub async fn home(&mut self) -> Result<()> {
        // ✅ 实际连接并执行回零
        // 🟡 P1-2 修复：优先使用 serial（如果提供），其次使用 interface
        let connection_target = self.config.serial.clone().or(self.config.interface.clone());

        let builder = if let Some(target) = connection_target {
            #[cfg(target_os = "linux")]
            {
                println!("使用 CAN 接口: {} (SocketCAN)", target);
            }
            #[cfg(not(target_os = "linux"))]
            {
                println!("使用设备序列号: {}", target);
            }
            ClientPiperBuilder::new().interface(target)
        } else {
            #[cfg(target_os = "linux")]
            {
                println!("使用默认 CAN 接口: can0 (SocketCAN)");
                ClientPiperBuilder::new().interface("can0")
            }
            #[cfg(target_os = "macos")]
            {
                let default_daemon = "127.0.0.1:18888";
                println!("使用默认守护进程: {} (UDP)", default_daemon);
                ClientPiperBuilder::new().with_daemon(default_daemon)
            }
            #[cfg(not(any(target_os = "linux", target_os = "macos")))]
            {
                println!("自动扫描 GS-USB 设备...");
                ClientPiperBuilder::new()
            }
        };

        println!("⏳ 连接到机器人...");
        let robot = builder.build()?;
        println!("✅ 已连接");

        println!("⏳ 回到零位...");

        // ✅ 实现回零逻辑：发送零位置命令
        let observer = robot.observer();
        let current_positions = observer.snapshot().position;

        // 使能 Position Mode
        use piper_client::state::PositionModeConfig;
        let config_mode = PositionModeConfig::default();
        let robot = robot.enable_position_mode(config_mode)?;

        // 发送零位置命令
        robot.send_position_command(&current_positions)?;

        // 等待运动完成
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

        // robot 在这里 drop，自动 disable
        println!("✅ 回零完成");

        Ok(())
    }

    /// 监控
    pub async fn monitor(&mut self, frequency: u32) -> Result<()> {
        println!("⏳ 连接到机器人...");

        // 创建 Piper 实例（使用 driver 层 API 以支持 FPS 统计）
        // 🟡 P1-2 修复：优先使用 serial（如果提供），其次使用 interface
        let connection_target = self.config.serial.clone().or(self.config.interface.clone());

        let builder = if let Some(target) = connection_target {
            #[cfg(target_os = "linux")]
            {
                println!("使用 CAN 接口: {} (SocketCAN)", target);
            }
            #[cfg(not(target_os = "linux"))]
            {
                println!("使用设备序列号: {}", target);
            }
            DriverPiperBuilder::new().interface(target)
        } else {
            #[cfg(target_os = "linux")]
            {
                println!("使用默认 CAN 接口: can0 (SocketCAN)");
                DriverPiperBuilder::new().interface("can0")
            }
            #[cfg(target_os = "macos")]
            {
                let default_daemon = "127.0.0.1:18888";
                println!("使用默认守护进程: {} (UDP)", default_daemon);
                DriverPiperBuilder::new().with_daemon(default_daemon)
            }
            #[cfg(not(any(target_os = "linux", target_os = "macos")))]
            {
                println!("自动扫描 GS-USB 设备...");
                DriverPiperBuilder::new()
            }
        };

        let piper = builder.build()?;
        println!("✅ 已连接");
        println!("📊 监控中 ({} Hz)...", frequency);
        println!("按 Ctrl+C 停止\n");

        // 设置 Ctrl+C 处理
        let running = Arc::new(AtomicBool::new(true));
        let r = running.clone();

        tokio::spawn(async move {
            #[cfg(unix)]
            {
                if let Ok(mut sig) = signal::unix::signal(signal::unix::SignalKind::interrupt()) {
                    sig.recv().await;
                    r.store(false, Ordering::SeqCst);
                    println!("\n收到退出信号，正在关闭...");
                }
            }
            #[cfg(windows)]
            {
                match signal::windows::ctrl_c() {
                    Ok(mut sig) => {
                        sig.recv().await;
                        r.store(false, Ordering::SeqCst);
                        println!("\n收到退出信号，正在关闭...");
                    },
                    Err(_) => {},
                }
            }
        });

        // 等待初始反馈
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        // 重置 FPS 统计
        piper.reset_fps_stats();
        let mut fps_window_start = std::time::Instant::now();

        // 主循环
        let mut iteration = 0u64;
        let sleep_duration = tokio::time::Duration::from_secs(1);
        let frequency_interval = if frequency > 0 {
            tokio::time::Duration::from_secs_f64(1.0 / frequency as f64)
        } else {
            sleep_duration
        };

        while running.load(Ordering::SeqCst) {
            iteration += 1;

            // 读取状态
            let joint_position = piper.get_joint_position();
            let end_pose = piper.get_end_pose();
            let joint_dynamic = piper.get_joint_dynamic();
            let robot_control = piper.get_robot_control();
            let gripper = piper.get_gripper();
            let fps = piper.get_fps();

            // 打印反馈
            print_monitor_output(
                iteration,
                &joint_position,
                &end_pose,
                &joint_dynamic,
                &robot_control,
                &gripper,
                &fps,
            );

            // 每隔 5 秒重置 FPS 统计
            if fps_window_start.elapsed() >= std::time::Duration::from_secs(5) {
                fps_window_start = std::time::Instant::now();
                piper.reset_fps_stats();
            }

            // 控制刷新频率
            tokio::time::sleep(frequency_interval).await;
        }

        println!("✅ 监控已结束");
        Ok(())
    }

    /// 录制
    pub async fn record(&mut self, args: RecordCommand) -> Result<()> {
        // 直接调用 RecordCommand 的 execute 方法（无需 config）
        args.execute().await?;

        Ok(())
    }
}

/// 打印监控输出
fn print_monitor_output(
    _iteration: u64,
    joint_position: &JointPositionState,
    end_pose: &EndPoseState,
    joint_dynamic: &JointDynamicState,
    _robot_control: &RobotControlState,
    gripper: &GripperState,
    fps: &FpsResult,
) {
    println!("========================================");

    // FPS 统计
    println!("\n状态更新频率 (FPS):");
    println!("  关节位置状态: {:6.2} Hz", fps.joint_position);
    println!("  末端位姿状态: {:6.2} Hz", fps.end_pose);
    println!("  关节动态状态: {:6.2} Hz", fps.joint_dynamic);
    println!("  机器人控制状态: {:6.2} Hz", fps.robot_control);
    println!("  夹爪状态:     {:6.2} Hz", fps.gripper);

    // 关节角度（弧度转度）
    println!("\n关节角度 (°):");
    for (i, angle) in joint_position.joint_pos.iter().enumerate() {
        let angle_deg: f64 = angle.to_degrees();
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

    // 夹爪状态
    println!("\n夹爪状态:");
    println!("  行程: {:6.2} mm", gripper.travel);
    println!("  扭矩: {:6.3} N·m", gripper.torque);
    println!(
        "  是否在运动: {}",
        if gripper.is_moving() { "是" } else { "否" }
    );

    println!("========================================\n");
}
