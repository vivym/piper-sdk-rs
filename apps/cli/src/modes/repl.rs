//! REPL 模式（交互式 Shell）
//!
//! 使用方案 B：专用输入线程 + mpsc 通道
//! 保留历史记录，不阻塞 tokio

use crate::connection::client_builder;
use anyhow::Result;
use crossbeam_channel::{Receiver, bounded};
use piper_client::Piper;
use piper_client::state::{Active, DisableConfig, PositionMode, PositionModeConfig, Standby};
use piper_client::types::{JointArray, Rad};
use rustyline::Editor;
use std::panic;
use std::thread;

/// REPL 会话状态
pub enum ReplState {
    /// 未连接
    Disconnected,
    /// 已连接但未使能
    Standby(Piper<Standby>),
    /// 已使能位置模式
    ActivePosition(Piper<Active<PositionMode>>),
}

impl ReplState {
    /// 获取状态描述
    pub fn description(&self) -> &str {
        match self {
            ReplState::Disconnected => "未连接",
            ReplState::Standby(_) => "已连接 (Standby)",
            ReplState::ActivePosition(_) => "已使能 (Position Mode)",
        }
    }

    /// 是否已连接
    pub fn is_connected(&self) -> bool {
        !matches!(self, ReplState::Disconnected)
    }

    /// 是否已使能
    pub fn is_enabled(&self) -> bool {
        matches!(self, ReplState::ActivePosition(_))
    }
}

/// REPL 会话（保持机器人连接）
pub struct ReplSession {
    state: ReplState,
}

impl ReplSession {
    /// 创建新会话
    pub fn new() -> Self {
        Self {
            state: ReplState::Disconnected,
        }
    }

    /// 连接到机器人
    pub fn connect(&mut self, interface: Option<&str>) -> Result<()> {
        if self.state.is_connected() {
            println!("⚠️  已经连接");
            return Ok(());
        }

        println!("⏳ 连接到机器人...");

        let builder = client_builder(interface, None, None);

        let robot = builder.build()?;
        self.state = ReplState::Standby(robot);

        println!("✅ 已连接");
        Ok(())
    }

    /// 断开连接
    pub fn disconnect(&mut self) -> Result<()> {
        if !self.state.is_connected() {
            println!("⚠️  未连接");
            return Ok(());
        }

        println!("⏳ 断开连接...");
        self.state = ReplState::Disconnected;
        println!("✅ 已断开");
        Ok(())
    }

    /// 使能电机
    pub fn enable(&mut self) -> Result<()> {
        match &self.state {
            ReplState::ActivePosition(_) => {
                // ✅ 已经使能，直接返回（不需要恢复状态）
                println!("⚠️  已经使能");
                Ok(())
            },
            _ => {
                // ✅ 使用 take() 避免状态恢复问题
                let old_state = std::mem::replace(&mut self.state, ReplState::Disconnected);
                match old_state {
                    ReplState::Standby(robot) => {
                        println!("⏳ 使能电机...");
                        let config = PositionModeConfig::default();
                        let robot = robot.enable_position_mode(config)?;
                        self.state = ReplState::ActivePosition(robot);
                        println!("✅ 已使能 Position Mode");
                        Ok(())
                    },
                    ReplState::Disconnected => {
                        self.state = ReplState::Disconnected;
                        anyhow::bail!("未连接，请先使用 connect 命令");
                    },
                    ReplState::ActivePosition(_) => {
                        // 不可能：已经在第一个分支处理了
                        unreachable!()
                    },
                }
            },
        }
    }

    /// 去使能电机
    pub fn disable(&mut self) -> Result<()> {
        match &self.state {
            ReplState::Standby(_) => {
                // ✅ 已经是 Standby 状态，直接返回
                println!("⚠️  未使能");
                Ok(())
            },
            _ => {
                // ✅ 使用 take() 避免状态恢复问题
                let old_state = std::mem::replace(&mut self.state, ReplState::Disconnected);
                match old_state {
                    ReplState::ActivePosition(robot) => {
                        println!("⏳ 去使能电机...");
                        let config = DisableConfig::default();
                        let robot = robot.disable(config)?;
                        self.state = ReplState::Standby(robot);
                        println!("✅ 已去使能");
                        Ok(())
                    },
                    ReplState::Disconnected => {
                        self.state = ReplState::Disconnected;
                        anyhow::bail!("未连接");
                    },
                    ReplState::Standby(_) => {
                        // 不可能：已经在第一个分支处理了
                        unreachable!()
                    },
                }
            },
        }
    }

    /// 获取状态描述
    pub fn status(&self) -> &str {
        self.state.description()
    }

    /// 检查是否已使能
    pub fn check_enabled(&self) -> Result<()> {
        if !self.state.is_enabled() {
            anyhow::bail!("电机未使能，请先使用 enable 命令");
        }
        Ok(())
    }

    /// 发送位置命令（需要在 Active 状态）
    pub fn send_position_command(&self, joints: &[f64]) -> Result<()> {
        if let ReplState::ActivePosition(robot) = &self.state {
            // 转换为 JointArray<Rad>
            let mut joint_array = JointArray::from([Rad(0.0); 6]);
            for (i, &pos) in joints.iter().enumerate() {
                if i < 6 {
                    joint_array[i] = Rad(pos);
                }
            }

            robot.send_position_command(&joint_array)?;
            Ok(())
        } else {
            anyhow::bail!("电机未使能");
        }
    }

    /// 查询当前位置
    pub fn get_position(&self) -> Result<Vec<Rad>> {
        match &self.state {
            ReplState::Standby(robot) => {
                let observer = robot.observer();
                let snapshot = observer.snapshot();
                Ok(snapshot.position.iter().copied().collect())
            },
            ReplState::ActivePosition(robot) => {
                let observer = robot.observer();
                let snapshot = observer.snapshot();
                Ok(snapshot.position.iter().copied().collect())
            },
            ReplState::Disconnected => {
                anyhow::bail!("未连接");
            },
        }
    }

    /// 检查是否已使能
    pub fn is_enabled(&self) -> bool {
        self.state.is_enabled()
    }

    /// 检查是否已连接
    pub fn is_connected(&self) -> bool {
        self.state.is_connected()
    }
}

/// REPL 输入（方案 B：专用输入线程）
pub struct ReplInput {
    command_rx: Receiver<String>,
    _input_thread: thread::JoinHandle<Result<()>>,
}

impl ReplInput {
    /// 创建专用输入线程（保留历史记录）
    pub fn new() -> Self {
        let (command_tx, command_rx) = bounded::<String>(10);

        // ⭐ 关键：在专用线程内创建 Editor（生命周期 = REPL 会话）
        let input_thread = thread::spawn(move || {
            use rustyline::history::DefaultHistory;

            let mut rl = Editor::<(), DefaultHistory>::new()
                .map_err(|e| anyhow::anyhow!("Failed to initialize readline: {}", e))?;

            // 配置历史记录
            let history_path = ".piper_history";
            rl.load_history(history_path).ok(); // 忽略错误（首次运行）

            println!("Piper CLI v{} - 交互式 Shell", env!("CARGO_PKG_VERSION"));
            println!("输入 'help' 查看帮助，'exit' 退出");
            println!();

            loop {
                let readline = rl.readline("piper> ");

                match readline {
                    Ok(line) => {
                        let line: String = line.trim().to_string();

                        if line.is_empty() {
                            continue;
                        }

                        if line == "exit" || line == "quit" {
                            rl.save_history(history_path).ok();
                            let _ = command_tx.send(line);
                            break;
                        }

                        // 添加到历史
                        let _ = rl.add_history_entry(line.clone());

                        // 发送到主线程
                        if command_tx.send(line).is_err() {
                            break; // 主线程已关闭
                        }
                    },

                    Err(rustyline::error::ReadlineError::Interrupted) => {
                        // Ctrl+C：在主线程处理急停
                        println!("^C");
                        // 发送特殊命令表示 Ctrl+C
                        let _ = command_tx.send("SIGINT".to_string());
                    },

                    Err(rustyline::error::ReadlineError::Eof) => {
                        // Ctrl+D：退出
                        rl.save_history(history_path).ok();
                        break;
                    },

                    Err(err) => {
                        eprintln!("Error: {:?}", err);
                        break;
                    },
                }
            }

            Ok(())
        });

        Self {
            command_rx,
            _input_thread: input_thread,
        }
    }

    /// 阻塞等待用户输入（在 tokio 任务中使用）
    pub async fn recv_command(&self) -> Option<String> {
        // ⭐ 使用 spawn_blocking 将 crossbeam::recv 转为 Future
        let rx = self.command_rx.clone();
        tokio::task::spawn_blocking(move || rx.recv())
            .await
            .ok()
            .and_then(|result| result.ok())
    }
}

/// 运行 REPL 模式
pub async fn run_repl() -> Result<()> {
    let mut session = ReplSession::new();
    let input = ReplInput::new(); // ⭐ 一次性创建，保留历史

    println!();
    println!("💡 提示: 使用 'connect' 连接到机器人，然后 'enable' 使能电机");
    println!("💡 提示: 按 Ctrl+C 执行急停并退出");
    println!();

    loop {
        tokio::select! {
            // ⭐ 优先级1：用户输入
            Some(line) = input.recv_command() => {
                if line == "SIGINT" {
                    // Ctrl+C 处理
                    eprintln!("\n🛑 Emergency stop activated!");
                    continue;
                }

                match line.as_str() {
                    "exit" | "quit" => {
                        println!("👋 再见！");
                        break;
                    }

                    "help" => {
                        print_help();
                    }

                    "status" => {
                        println!("📊 状态: {}", session.status());
                    }

                    _ => {
                        // ⭐ 错误隔离：防止 panic 导致 REPL 崩溃
                        if let Err(panic_err) = panic::catch_unwind(
                            std::panic::AssertUnwindSafe(|| {
                                // 在阻塞上下文中执行命令
                                tokio::runtime::Handle::current()
                                    .block_on(handle_command(&line, &mut session))
                            })
                        ) {
                            eprintln!("❌ Command panicked: {:?}", panic_err);
                        } else if let Err(err) = handle_command(&line, &mut session).await {
                            eprintln!("❌ Error: {}", err);
                            print_help_hint(&line);
                        }
                    }
                }
            }

            // ⭐ 优先级2：Ctrl+C 急停
            _ = tokio::signal::ctrl_c() => {
                eprintln!("\n🛑 Emergency stop activated!");

                // ✅ 实现急停逻辑：如果已使能，先失能
                if session.is_enabled() {
                    eprintln!("⏳ 正在停止电机...");
                    if let Err(err) = session.disable() {
                        eprintln!("⚠️  失能失败: {}", err);
                    } else {
                        eprintln!("✅ 电机已停止");
                    }
                }

                // 断开连接
                if session.is_connected() {
                    let _ = session.disconnect();
                }

                break;
            }
        }
    }

    Ok(())
}

/// 处理命令
async fn handle_command(line: &str, session: &mut ReplSession) -> Result<()> {
    let parts: Vec<&str> = line.split_whitespace().collect();

    if parts.is_empty() {
        return Ok(());
    }

    match parts[0] {
        "connect" => {
            let interface = parts.get(1).copied();
            session.connect(interface)?;
        },

        "disconnect" => {
            session.disconnect()?;
        },

        "enable" => {
            session.enable()?;
        },

        "disable" => {
            session.disable()?;
        },

        "move" => {
            session.check_enabled()?;
            handle_move(session, parts)?;
        },

        "position" => {
            handle_position(session)?;
        },

        "home" => {
            session.check_enabled()?;
            handle_home(session)?;
        },

        "stop" => {
            handle_stop(session)?;
        },

        _ => {
            anyhow::bail!("未知命令: {}", parts[0]);
        },
    }

    Ok(())
}

/// 处理 move 命令
fn handle_move(session: &ReplSession, parts: Vec<&str>) -> Result<()> {
    // 解析 --joints 参数
    let joints_str = parts
        .iter()
        .position(|&s| s == "--joints")
        .and_then(|idx| parts.get(idx + 1))
        .ok_or_else(|| anyhow::anyhow!("缺少 --joints 参数"))?;

    // 解析关节位置
    let positions: Vec<f64> = joints_str
        .split(',')
        .map(|s| s.trim().parse::<f64>())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| anyhow::anyhow!("无效的关节位置格式"))?;

    if positions.len() != 6 {
        anyhow::bail!("需要 6 个关节位置，得到 {}", positions.len());
    }

    println!("⏳ 移动关节:");
    for (i, &pos) in positions.iter().enumerate() {
        let deg = pos * 180.0 / std::f64::consts::PI;
        println!("  J{}: {:.3} rad ({:.1}°)", i + 1, pos, deg);
    }

    // 发送位置命令
    session.send_position_command(&positions)?;

    // 等待一小段时间让运动开始
    std::thread::sleep(std::time::Duration::from_millis(100));

    println!("✅ 移动命令已发送");

    Ok(())
}

/// 处理 position 命令
fn handle_position(session: &ReplSession) -> Result<()> {
    println!("⏳ 查询位置...");

    let positions = session.get_position()?;

    println!("📍 当前位置:");
    for (i, pos) in positions.iter().enumerate() {
        let deg = pos.to_deg();
        println!("  J{}: {:.3} rad ({:.1}°)", i + 1, pos.0, deg.0);
    }

    println!("✅ 位置查询完成");
    Ok(())
}

/// 处理 home 命令
fn handle_home(session: &ReplSession) -> Result<()> {
    println!("⏳ 回到零位...");

    // 发送零位命令
    let zero_joints = [0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
    session.send_position_command(&zero_joints)?;

    // 等待一小段时间让运动开始
    std::thread::sleep(std::time::Duration::from_millis(100));

    println!("✅ 回零完成");
    Ok(())
}

/// 处理 stop 命令
fn handle_stop(session: &mut ReplSession) -> Result<()> {
    println!("🛑 急停...");

    // 如果已使能，先去使能
    if session.state.is_enabled() {
        session.disable()?;
    }

    println!("✅ 已急停");
    Ok(())
}

/// 打印帮助信息
fn print_help() {
    println!("可用命令:");
    println!("  connect [interface]           连接到机器人（可选接口）");
    println!("  disconnect                    断开连接");
    println!("  enable                        使能电机");
    println!("  disable                       去使能电机");
    println!("  move --joints <J1,J2,...>     移动关节（6个值，弧度）");
    println!("  position                      查询当前位置");
    println!("  home                          回到零位");
    println!("  stop                          急停");
    println!("  status                        显示连接状态");
    println!("  help                          显示帮助");
    println!("  exit / quit                   退出");
    println!();
    println!("快捷键:");
    println!("  Ctrl+C                        急停");
    println!("  Ctrl+D                        退出");
    println!();
}

/// 提供基于错误的帮助提示
fn print_help_hint(command: &str) {
    if command.starts_with("move") {
        eprintln!("💡 提示: 使用 'move --joints 0.1,0.2,0.3,0.4,0.5,0.6' 移动关节");
    } else if command.starts_with("connect") {
        eprintln!("💡 提示: 使用 'connect' 或 'connect can0' 连接到机器人");
    } else if command.starts_with("enable") {
        eprintln!("💡 提示: 需要先使用 'connect' 连接到机器人");
    } else {
        eprintln!("💡 提示: 输入 'help' 查看所有命令");
    }
}
