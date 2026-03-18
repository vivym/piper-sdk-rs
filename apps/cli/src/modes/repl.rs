//! REPL 模式（交互式 Shell）

use crate::commands::config::CliConfig;
use crate::connection::client_builder;
use crate::parsing::{parse_collision_levels, parse_joint_indices_arg};
use anyhow::{Result, bail};
use piper_client::ControlReadPolicy;
use piper_client::Piper;
use piper_client::state::{Active, DisableConfig, Piper as StatePiper, PositionMode, Standby};
use piper_control::{
    ControlProfile, MotionExecutionOutcome, PreparedMove, TargetSpec,
    active_move_to_joint_target_with_cancel, prepare_move, query_collision_protection_blocking,
    set_collision_protection_verified, set_joint_zero_blocking,
};
use rustyline::Editor;
use std::panic;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use tokio::sync::{mpsc, oneshot};

#[cfg(test)]
use std::sync::atomic::AtomicUsize;

pub enum ReplState {
    Disconnected,
    Standby(StatePiper<Standby>),
    ActivePosition(StatePiper<Active<PositionMode>>),
}

impl ReplState {
    pub fn description(&self) -> &str {
        match self {
            ReplState::Disconnected => "未连接",
            ReplState::Standby(_) => "已连接 (Standby)",
            ReplState::ActivePosition(_) => "已使能 (Position Mode)",
        }
    }

    pub fn is_connected(&self) -> bool {
        !matches!(self, ReplState::Disconnected)
    }
}

pub struct ReplSession {
    state: ReplState,
    config: CliConfig,
    profile: ControlProfile,
    #[cfg(test)]
    test_observer_positions: Option<[f64; 6]>,
}

impl ReplSession {
    pub fn new(config: CliConfig) -> Self {
        let profile = config.control_profile(None);
        Self {
            state: ReplState::Disconnected,
            config,
            profile,
            #[cfg(test)]
            test_observer_positions: None,
        }
    }

    #[cfg(test)]
    fn set_test_observer_positions(&mut self, positions: [f64; 6]) {
        self.test_observer_positions = Some(positions);
    }

    pub fn connect(&mut self, target: Option<TargetSpec>) -> Result<()> {
        if self.state.is_connected() {
            println!("⚠️  已经连接");
            return Ok(());
        }

        self.profile = self.config.control_profile(target.as_ref());
        println!("⏳ 连接到机器人...");
        println!(
            "🎯 target: {}",
            TargetSpec::from(self.profile.target.clone())
        );

        let builder = client_builder(&self.profile.target);
        let robot = builder.build()?;
        self.state = ReplState::Standby(robot);

        println!("✅ 已连接");
        Ok(())
    }

    pub fn disconnect(&mut self) {
        if !self.state.is_connected() {
            println!("⚠️  未连接");
            return;
        }

        println!("⏳ 断开连接...");
        self.state = ReplState::Disconnected;
        println!("✅ 已断开");
    }

    pub fn enable(&mut self) -> Result<()> {
        match std::mem::replace(&mut self.state, ReplState::Disconnected) {
            ReplState::Standby(robot) => {
                println!("⏳ 使能电机...");
                let robot = robot.enable_position_mode(self.profile.position_mode_config())?;
                self.state = ReplState::ActivePosition(robot);
                println!("✅ 已使能 Position Mode");
                Ok(())
            },
            ReplState::Disconnected => {
                self.state = ReplState::Disconnected;
                bail!("未连接，请先使用 connect 命令");
            },
            ReplState::ActivePosition(robot) => {
                self.state = ReplState::ActivePosition(robot);
                println!("⚠️  已经使能");
                Ok(())
            },
        }
    }

    pub fn disable(&mut self) -> Result<()> {
        match std::mem::replace(&mut self.state, ReplState::Disconnected) {
            ReplState::ActivePosition(robot) => {
                println!("⏳ 去使能电机...");
                let robot = robot.disable(DisableConfig::default())?;
                self.state = ReplState::Standby(robot);
                println!("✅ 已去使能");
                Ok(())
            },
            ReplState::Standby(robot) => {
                self.state = ReplState::Standby(robot);
                println!("⚠️  未使能");
                Ok(())
            },
            ReplState::Disconnected => {
                self.state = ReplState::Disconnected;
                bail!("未连接");
            },
        }
    }

    pub fn status(&self) -> &str {
        self.state.description()
    }

    pub fn emergency_stop(&mut self) -> Result<()> {
        match std::mem::replace(&mut self.state, ReplState::Disconnected) {
            ReplState::Disconnected => {
                self.state = ReplState::Disconnected;
                Ok(())
            },
            ReplState::Standby(robot) => {
                robot.disable_all()?;
                self.state = ReplState::Standby(robot);
                Ok(())
            },
            ReplState::ActivePosition(robot) => {
                let robot = robot.disable_all()?;
                self.state = ReplState::Standby(robot);
                Ok(())
            },
        }
    }

    fn active_robot(&self) -> Result<&Piper<Active<PositionMode>>> {
        match &self.state {
            ReplState::ActivePosition(robot) => Ok(robot),
            _ => bail!("电机未使能，请先使用 enable 命令"),
        }
    }

    fn standby_robot(&self) -> Result<&Piper<Standby>> {
        match &self.state {
            ReplState::Standby(robot) => Ok(robot),
            ReplState::ActivePosition(_) => bail!("请先 disable，再执行此命令"),
            ReplState::Disconnected => bail!("未连接"),
        }
    }

    fn observer_positions(&self) -> Result<[f64; 6]> {
        match &self.state {
            ReplState::Standby(robot) => {
                let snapshot = robot
                    .observer()
                    .control_snapshot(ControlReadPolicy::default())?;
                Ok(std::array::from_fn(|index| snapshot.position[index].0))
            },
            ReplState::ActivePosition(robot) => {
                let snapshot = robot
                    .observer()
                    .control_snapshot(ControlReadPolicy::default())?;
                Ok(std::array::from_fn(|index| snapshot.position[index].0))
            },
            ReplState::Disconnected => {
                #[cfg(test)]
                if let Some(positions) = self.test_observer_positions {
                    return Ok(positions);
                }
                bail!("未连接")
            },
        }
    }
}

pub struct ReplInput {
    command_rx: mpsc::Receiver<String>,
    _input_thread: thread::JoinHandle<Result<()>>,
}

impl ReplInput {
    pub fn new() -> Self {
        let (command_tx, command_rx) = mpsc::channel::<String>(10);

        let input_thread = thread::spawn(move || {
            use rustyline::history::DefaultHistory;

            let mut rl = Editor::<(), DefaultHistory>::new()
                .map_err(|e| anyhow::anyhow!("Failed to initialize readline: {}", e))?;

            let history_path = ".piper_history";
            rl.load_history(history_path).ok();

            println!("Piper CLI v{} - 交互式 Shell", env!("CARGO_PKG_VERSION"));
            println!("输入 'help' 查看帮助，'exit' 退出");
            println!();

            loop {
                match rl.readline("piper> ") {
                    Ok(line) => {
                        let line = line.trim().to_string();
                        if line.is_empty() {
                            continue;
                        }
                        if line == "exit" || line == "quit" {
                            rl.save_history(history_path).ok();
                            let _ = command_tx.blocking_send(line);
                            break;
                        }
                        let _ = rl.add_history_entry(line.clone());
                        if command_tx.blocking_send(line).is_err() {
                            break;
                        }
                    },
                    Err(rustyline::error::ReadlineError::Interrupted) => {
                        println!("^C");
                        let _ = command_tx.blocking_send("SIGINT".to_string());
                    },
                    Err(rustyline::error::ReadlineError::Eof) => {
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

    pub async fn recv_command(&mut self) -> Option<String> {
        self.command_rx.recv().await
    }
}

#[derive(Clone, Default)]
struct ReplHandle {
    cancel_requested: Arc<AtomicBool>,
    motion_in_progress: Arc<AtomicBool>,
    emergency_requested: Arc<AtomicBool>,
}

impl ReplHandle {
    fn clear_runtime_flags(&self) {
        self.cancel_requested.store(false, Ordering::SeqCst);
        self.motion_in_progress.store(false, Ordering::SeqCst);
    }

    fn take_emergency_requested(&self) -> bool {
        self.emergency_requested.swap(false, Ordering::SeqCst)
    }

    fn request_emergency_stop(&self) -> EmergencyStopOutcome {
        self.emergency_requested.store(true, Ordering::SeqCst);
        if self.motion_in_progress.load(Ordering::SeqCst) {
            self.cancel_requested.store(true, Ordering::SeqCst);
            EmergencyStopOutcome::CancellingMotion
        } else {
            EmergencyStopOutcome::QueuedAfterCurrent
        }
    }
}

struct CommandExecutionControl {
    cancel_requested: Arc<AtomicBool>,
    motion_in_progress: Arc<AtomicBool>,
}

impl CommandExecutionControl {
    fn new(handle: &ReplHandle) -> Self {
        Self {
            cancel_requested: Arc::clone(&handle.cancel_requested),
            motion_in_progress: Arc::clone(&handle.motion_in_progress),
        }
    }

    fn cancel_requested(&self) -> bool {
        self.cancel_requested.load(Ordering::SeqCst)
    }

    fn motion_scope(&self) -> MotionExecutionGuard {
        self.motion_in_progress.store(true, Ordering::SeqCst);
        MotionExecutionGuard {
            motion_in_progress: Arc::clone(&self.motion_in_progress),
        }
    }
}

struct MotionExecutionGuard {
    motion_in_progress: Arc<AtomicBool>,
}

impl Drop for MotionExecutionGuard {
    fn drop(&mut self) {
        self.motion_in_progress.store(false, Ordering::SeqCst);
    }
}

enum GuardedCommandOutcome<T> {
    Success(T),
    Error(anyhow::Error),
    Panicked(Box<dyn std::any::Any + Send>),
}

fn run_guarded_command<T, F>(command: F) -> GuardedCommandOutcome<T>
where
    F: FnOnce() -> Result<T>,
{
    match panic::catch_unwind(std::panic::AssertUnwindSafe(command)) {
        Ok(Ok(value)) => GuardedCommandOutcome::Success(value),
        Ok(Err(error)) => GuardedCommandOutcome::Error(error),
        Err(payload) => GuardedCommandOutcome::Panicked(payload),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CommandExecutionOutcome {
    Completed,
    MotionCancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EmergencyStopOutcome {
    StartedImmediate,
    CancellingMotion,
    QueuedAfterCurrent,
}

struct CommandCompletion {
    line: String,
    session: ReplSession,
    outcome: GuardedCommandOutcome<CommandExecutionOutcome>,
    post_command_stop: Option<Result<()>>,
}

struct RunningCommand {
    line: String,
    completion_rx: oneshot::Receiver<CommandCompletion>,
    worker: thread::JoinHandle<()>,
}

struct ReplExecutor {
    session: Option<ReplSession>,
    running: Option<RunningCommand>,
    handle: ReplHandle,
}

impl ReplExecutor {
    fn new(config: CliConfig) -> Self {
        Self {
            session: Some(ReplSession::new(config)),
            running: None,
            handle: ReplHandle::default(),
        }
    }

    fn is_busy(&self) -> bool {
        self.running.is_some()
    }

    fn status(&self) -> &str {
        self.session.as_ref().map(ReplSession::status).unwrap_or("命令执行中")
    }

    fn start_command(&mut self, line: String) -> Result<()> {
        if self.running.is_some() {
            bail!("当前命令仍在执行；请等待完成或先使用 `stop` / Ctrl+C");
        }

        let mut session = self
            .session
            .take()
            .ok_or_else(|| anyhow::anyhow!("REPL session is unavailable"))?;
        self.handle.clear_runtime_flags();
        self.handle.take_emergency_requested();

        let handle = self.handle.clone();
        let worker_line = line.clone();
        let command_line = line.clone();
        let (completion_tx, completion_rx) = oneshot::channel();

        let worker = thread::spawn(move || {
            let completion = run_command_worker(worker_line, &mut session, &handle);
            let _ = completion_tx.send(CommandCompletion {
                line: command_line,
                session,
                outcome: completion.0,
                post_command_stop: completion.1,
            });
        });

        self.running = Some(RunningCommand {
            line: line.clone(),
            completion_rx,
            worker,
        });
        Ok(())
    }

    fn request_emergency_stop(&mut self) -> Result<EmergencyStopOutcome> {
        if self.running.is_some() {
            return Ok(self.handle.request_emergency_stop());
        }

        self.start_command("stop".to_string())?;
        Ok(EmergencyStopOutcome::StartedImmediate)
    }

    async fn wait_for_completion(&mut self) -> Result<CommandCompletion> {
        let running = self.running.take().ok_or_else(|| anyhow::anyhow!("no running command"))?;
        let line = running.line.clone();
        let completion = running
            .completion_rx
            .await
            .map_err(|_| anyhow::anyhow!("REPL command worker exited unexpectedly for `{line}`"))?;
        running
            .worker
            .join()
            .map_err(|_| anyhow::anyhow!("REPL command worker panicked after `{line}`"))?;
        Ok(completion)
    }

    fn finish_completion(&mut self, completion: CommandCompletion) {
        self.handle.clear_runtime_flags();
        let _ = self.handle.take_emergency_requested();
        self.session = Some(completion.session);

        match completion.outcome {
            GuardedCommandOutcome::Success(CommandExecutionOutcome::Completed) => {},
            GuardedCommandOutcome::Success(CommandExecutionOutcome::MotionCancelled) => {
                println!("🛑 当前运动已取消，连接保持在 {}", self.status());
            },
            GuardedCommandOutcome::Error(error) => {
                eprintln!("❌ Error: {}", error);
                print_help_hint(&completion.line);
            },
            GuardedCommandOutcome::Panicked(panic_err) => {
                eprintln!("❌ Command panicked: {:?}", panic_err);
            },
        }

        if let Some(stop_result) = completion.post_command_stop {
            match stop_result {
                Ok(()) => println!("✅ 已执行 disable_all()，连接保持在 {}", self.status()),
                Err(error) => eprintln!("❌ Emergency stop failed: {error}"),
            }
        }
    }
}

fn run_command_worker(
    line: String,
    session: &mut ReplSession,
    handle: &ReplHandle,
) -> (
    GuardedCommandOutcome<CommandExecutionOutcome>,
    Option<Result<()>>,
) {
    let control = CommandExecutionControl::new(handle);
    let outcome = run_guarded_command(|| handle_command(&line, session, &control));

    let post_command_stop = if matches!(
        outcome,
        GuardedCommandOutcome::Success(CommandExecutionOutcome::MotionCancelled)
    ) {
        let _ = handle.take_emergency_requested();
        Some(session.emergency_stop())
    } else if handle.take_emergency_requested() {
        Some(session.emergency_stop())
    } else {
        None
    };

    handle.clear_runtime_flags();
    (outcome, post_command_stop)
}

pub async fn run_repl() -> Result<()> {
    let config = CliConfig::load()?;
    let mut executor = ReplExecutor::new(config);
    let mut input = ReplInput::new();
    let mut exit_after_completion = false;

    println!();
    println!("💡 提示: 使用 'connect' 连接到机器人，然后 'enable' 使能电机");
    println!("💡 提示: 连接目标使用和 CLI 一样的 target spec，例如 socketcan:can0");
    println!("💡 提示: `stop` 或 Ctrl+C 会发送 disable_all()，但保持连接在 Standby");
    println!("💡 提示: 命令执行期间只接受 `stop` / Ctrl+C / exit");
    println!("💡 提示: shell 不做交互式确认；高风险 move / set-zero 请显式加 --force");
    println!();

    loop {
        if executor.is_busy() {
            tokio::select! {
                line = input.recv_command() => {
                    match line {
                        Some(line) => {
                            if handle_line_while_busy(&mut executor, &line, &mut exit_after_completion)? {
                                println!("👋 再见！");
                                break;
                            }
                        },
                        None => {
                            if handle_input_closed_while_busy(&mut executor, &mut exit_after_completion)? {
                                println!("👋 再见！");
                                break;
                            }
                        },
                    }
                }
                _ = tokio::signal::ctrl_c() => {
                    announce_stop_request(executor.request_emergency_stop()?);
                }
                completion = executor.wait_for_completion() => {
                    executor.finish_completion(completion?);
                    if exit_after_completion {
                        println!("👋 再见！");
                        break;
                    }
                }
            }
            continue;
        }

        tokio::select! {
            line = input.recv_command() => {
                match line {
                    Some(line) => {
                        if handle_line_when_idle(&mut executor, &line)? {
                            println!("👋 再见！");
                            break;
                        }
                    },
                    None => {
                        if handle_input_closed_when_idle() {
                            println!("👋 再见！");
                            break;
                        }
                    },
                }
            }
            _ = tokio::signal::ctrl_c() => {
                announce_stop_request(executor.request_emergency_stop()?);
            }
        }
    }

    Ok(())
}

fn handle_line_when_idle(executor: &mut ReplExecutor, line: &str) -> Result<bool> {
    if line == "SIGINT" {
        announce_stop_request(executor.request_emergency_stop()?);
        return Ok(false);
    }

    match line {
        "exit" | "quit" => return Ok(true),
        "help" => {
            print_help();
            return Ok(false);
        },
        "status" => {
            println!("📊 状态: {}", executor.status());
            return Ok(false);
        },
        "stop" => {
            announce_stop_request(executor.request_emergency_stop()?);
            return Ok(false);
        },
        _ => executor.start_command(line.to_string())?,
    }

    Ok(false)
}

fn handle_line_while_busy(
    executor: &mut ReplExecutor,
    line: &str,
    exit_after_completion: &mut bool,
) -> Result<bool> {
    if line == "SIGINT" || line == "stop" {
        announce_stop_request(executor.request_emergency_stop()?);
        return Ok(false);
    }

    if line == "exit" || line == "quit" {
        *exit_after_completion = true;
        announce_stop_request(executor.request_emergency_stop()?);
        return Ok(false);
    }

    println!("⚠️  当前命令仍在执行；请等待完成或先使用 `stop` / Ctrl+C");
    Ok(false)
}

fn handle_input_closed_when_idle() -> bool {
    true
}

fn handle_input_closed_while_busy(
    executor: &mut ReplExecutor,
    exit_after_completion: &mut bool,
) -> Result<bool> {
    *exit_after_completion = true;
    announce_stop_request(executor.request_emergency_stop()?);
    Ok(false)
}

fn announce_stop_request(outcome: EmergencyStopOutcome) {
    match outcome {
        EmergencyStopOutcome::StartedImmediate => {
            eprintln!("\n🛑 Emergency stop activated!");
        },
        EmergencyStopOutcome::CancellingMotion => {
            eprintln!("\n🛑 Emergency stop requested, cancelling current motion...");
        },
        EmergencyStopOutcome::QueuedAfterCurrent => {
            eprintln!(
                "\n🛑 Emergency stop queued; it will run after the current command finishes."
            );
        },
    }
}

fn handle_command(
    line: &str,
    session: &mut ReplSession,
    control: &CommandExecutionControl,
) -> Result<CommandExecutionOutcome> {
    #[cfg(test)]
    if let Some(outcome) = maybe_handle_test_command(line, control) {
        return outcome;
    }

    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.is_empty() {
        return Ok(CommandExecutionOutcome::Completed);
    }

    match parts[0] {
        "connect" => {
            let target = parts
                .get(1)
                .copied()
                .map(TargetSpec::from_str)
                .transpose()
                .map_err(|error| anyhow::anyhow!(error))?;
            session.connect(target)?;
        },
        "disconnect" => session.disconnect(),
        "enable" => session.enable()?,
        "disable" => session.disable()?,
        "move" => return handle_move(session, control, &parts),
        "position" => handle_position(session)?,
        "home" => return handle_home(session, control),
        "park" => return handle_park(session, control),
        "set-zero" => handle_set_zero(session, &parts)?,
        "collision-protection" => handle_collision_protection(session, &parts)?,
        "stop" => handle_stop(session)?,
        _ => bail!("未知命令: {}", parts[0]),
    }

    Ok(CommandExecutionOutcome::Completed)
}

fn handle_move(
    session: &ReplSession,
    control: &CommandExecutionControl,
    parts: &[&str],
) -> Result<CommandExecutionOutcome> {
    let joints_str = parts
        .iter()
        .position(|part| *part == "--joints")
        .and_then(|index| parts.get(index + 1))
        .ok_or_else(|| anyhow::anyhow!("缺少 --joints 参数"))?;
    let force = parts.contains(&"--force");

    let requested = joints_str
        .split(',')
        .map(|value| value.trim().parse::<f64>())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| anyhow::anyhow!("无效的关节位置格式"))?;

    let prepared = prepare_move(
        session.observer_positions()?,
        &requested,
        &session.profile.safety,
        force,
    )?;
    require_repl_move_force(&prepared, force)?;

    let _motion_guard = control.motion_scope();
    let robot = session.active_robot()?;
    match active_move_to_joint_target_with_cancel(
        robot,
        prepared.effective_target,
        &session.profile.wait,
        || control.cancel_requested(),
    )? {
        MotionExecutionOutcome::Reached => {
            println!("✅ 移动完成");
            Ok(CommandExecutionOutcome::Completed)
        },
        MotionExecutionOutcome::Cancelled => Ok(CommandExecutionOutcome::MotionCancelled),
    }
}

fn handle_position(session: &ReplSession) -> Result<()> {
    let positions = session.observer_positions()?;
    println!("📍 当前位置:");
    for (index, position) in positions.iter().enumerate() {
        println!(
            "  J{}: {:.3} rad ({:.1}°)",
            index + 1,
            position,
            position.to_degrees()
        );
    }
    Ok(())
}

fn handle_home(
    session: &ReplSession,
    control: &CommandExecutionControl,
) -> Result<CommandExecutionOutcome> {
    let _motion_guard = control.motion_scope();
    let robot = session.active_robot()?;
    match active_move_to_joint_target_with_cancel(robot, [0.0; 6], &session.profile.wait, || {
        control.cancel_requested()
    })? {
        MotionExecutionOutcome::Reached => {
            println!("✅ 回零完成");
            Ok(CommandExecutionOutcome::Completed)
        },
        MotionExecutionOutcome::Cancelled => Ok(CommandExecutionOutcome::MotionCancelled),
    }
}

fn handle_park(
    session: &ReplSession,
    control: &CommandExecutionControl,
) -> Result<CommandExecutionOutcome> {
    let _motion_guard = control.motion_scope();
    let robot = session.active_robot()?;
    match active_move_to_joint_target_with_cancel(
        robot,
        session.profile.park_pose(),
        &session.profile.wait,
        || control.cancel_requested(),
    )? {
        MotionExecutionOutcome::Reached => {
            println!("✅ 停靠完成");
            Ok(CommandExecutionOutcome::Completed)
        },
        MotionExecutionOutcome::Cancelled => Ok(CommandExecutionOutcome::MotionCancelled),
    }
}

fn handle_set_zero(session: &ReplSession, parts: &[&str]) -> Result<()> {
    let joints = parse_joint_indices_arg(option_value(parts, "--joints"))?;
    let force = parts.contains(&"--force");
    require_repl_set_zero_force(force)?;

    let robot = session.standby_robot()?;
    set_joint_zero_blocking(robot, &joints)?;
    println!("✅ 零点标定命令已发送");
    Ok(())
}

fn require_repl_move_force(prepared: &PreparedMove, force: bool) -> Result<()> {
    if prepared.requires_confirmation && !force {
        bail!(
            "REPL 中大幅移动必须显式加 --force（最大位移 {:.1}°）；若需要交互确认，请使用 one-shot CLI",
            prepared.max_delta_deg
        );
    }
    Ok(())
}

fn require_repl_set_zero_force(force: bool) -> Result<()> {
    if !force {
        bail!("REPL 中 set-zero 必须显式加 --force；若需要交互确认，请使用 one-shot CLI");
    }
    Ok(())
}

fn handle_collision_protection(session: &ReplSession, parts: &[&str]) -> Result<()> {
    let robot = session.standby_robot()?;
    match parts.get(1).copied() {
        Some("get") => {
            let levels = query_collision_protection_blocking(robot, &session.profile.wait)?;
            println!("collision protection levels: {:?}", levels);
            Ok(())
        },
        Some("set") => {
            let level = option_value(parts, "--level")
                .map(|value| value.parse::<u8>())
                .transpose()
                .map_err(|_| anyhow::anyhow!("无效的 --level 值"))?;
            let levels = option_value(parts, "--levels");
            let desired = parse_collision_levels(level, levels)?;
            set_collision_protection_verified(robot, desired, &session.profile.wait)?;
            println!("✅ 碰撞保护等级已写入并校验: {:?}", desired);
            Ok(())
        },
        _ => bail!(
            "用法: collision-protection get | collision-protection set --level <0-8> | --levels <j1,...,j6>"
        ),
    }
}

fn handle_stop(session: &mut ReplSession) -> Result<()> {
    println!("🛑 急停...");
    session.emergency_stop()?;
    println!("✅ 已急停，连接保持在 {}", session.status());
    Ok(())
}

fn option_value<'a>(parts: &'a [&str], flag: &str) -> Option<&'a str> {
    parts
        .iter()
        .position(|part| *part == flag)
        .and_then(|index| parts.get(index + 1).copied())
}

fn print_help() {
    println!("可用命令:");
    println!("  connect [target-spec]                 连接到机器人");
    println!("  disconnect                            断开连接");
    println!("  enable                                使能 Position Mode");
    println!("  disable                               去使能电机");
    println!("  move --joints <J1,..,Jn> [--force]    移动关节（1~6 个值）");
    println!("  position                              查询当前位置");
    println!("  home                                  回到零关节位");
    println!("  park                                  前往停靠位");
    println!("  set-zero [--joints 1,2,3] [--force]   写入零点标定");
    println!("  collision-protection get              主动查询碰撞保护等级");
    println!("  collision-protection set --level 5    设置统一碰撞保护等级");
    println!("  stop                                  急停（disable_all，保持连接）");
    println!("  status                                显示连接状态");
    println!("  help                                  显示帮助");
    println!("  exit / quit                           退出");
    println!("  Ctrl+C                                急停（disable_all，保持连接）");
    println!("  Ctrl+D                                退出 shell");
    println!("  忙碌时                                仅接受 stop / Ctrl+C / exit");
    println!("  需要确认的操作                        shell 中必须显式加 --force");
    println!();
}

fn print_help_hint(command: &str) {
    if command.starts_with("move") {
        eprintln!("💡 提示: 使用 'move --joints 0.1,0.2,0.3 --force'；未指定的关节保持当前位姿");
    } else if command.starts_with("connect") {
        eprintln!("💡 提示: 使用 'connect' 或 'connect socketcan:can0'");
    } else if command.starts_with("set-zero") {
        eprintln!("💡 提示: 使用 'set-zero --force' 或 'set-zero --joints 1,2,3 --force'");
    } else {
        eprintln!("💡 提示: 输入 'help' 查看所有命令");
    }
}

#[cfg(test)]
fn maybe_handle_test_command(
    line: &str,
    control: &CommandExecutionControl,
) -> Option<Result<CommandExecutionOutcome>> {
    match line {
        "__test-once" => {
            TEST_ONCE_COUNT.fetch_add(1, Ordering::SeqCst);
            Some(Ok(CommandExecutionOutcome::Completed))
        },
        "__test-motion" => {
            TEST_MOTION_COUNT.fetch_add(1, Ordering::SeqCst);
            let _motion_guard = control.motion_scope();
            while !control.cancel_requested() {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            Some(Ok(CommandExecutionOutcome::MotionCancelled))
        },
        "__test-busy" => {
            TEST_BUSY_COUNT.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(std::time::Duration::from_millis(10));
            Some(Ok(CommandExecutionOutcome::Completed))
        },
        _ => None,
    }
}

#[cfg(test)]
static TEST_ONCE_COUNT: AtomicUsize = AtomicUsize::new(0);
#[cfg(test)]
static TEST_MOTION_COUNT: AtomicUsize = AtomicUsize::new(0);
#[cfg(test)]
static TEST_BUSY_COUNT: AtomicUsize = AtomicUsize::new(0);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_guarded_command_executes_closure_once() {
        let counter = AtomicUsize::new(0);

        let outcome = run_guarded_command(|| {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok(())
        });

        assert!(matches!(outcome, GuardedCommandOutcome::Success(())));
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn handle_stop_is_noop_when_disconnected() {
        let mut session = ReplSession::new(CliConfig::default());
        handle_stop(&mut session).unwrap();
        assert!(matches!(session.state, ReplState::Disconnected));
    }

    #[test]
    fn handle_input_closed_when_idle_exits_shell() {
        assert!(handle_input_closed_when_idle());
    }

    #[tokio::test]
    async fn handle_input_closed_while_busy_requests_stop_and_defers_exit() {
        let mut executor = ReplExecutor::new(CliConfig::default());
        let mut exit_after_completion = false;

        executor.start_command("__test-motion".to_string()).unwrap();
        for _ in 0..20 {
            if executor.handle.motion_in_progress.load(Ordering::SeqCst) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }

        assert!(
            !handle_input_closed_while_busy(&mut executor, &mut exit_after_completion).unwrap()
        );
        assert!(exit_after_completion);
        assert!(executor.handle.cancel_requested.load(Ordering::SeqCst));

        let completion = executor.wait_for_completion().await.unwrap();
        executor.finish_completion(completion);
    }

    #[test]
    fn repl_move_requires_force_for_large_motion() {
        let prepared = PreparedMove {
            current: [2.5, 0.0, 0.0, 0.0, 0.0, 0.0],
            effective_target: [0.1, 0.0, 0.0, 0.0, 0.0, 0.0],
            max_delta_rad: 2.4,
            max_delta_deg: 2.4_f64.to_degrees(),
            requires_confirmation: true,
        };

        let error = require_repl_move_force(&prepared, false).unwrap_err();
        assert!(error.to_string().contains("REPL 中大幅移动必须显式加 --force"));
        require_repl_move_force(&prepared, true).unwrap();
    }

    #[test]
    fn repl_set_zero_requires_force() {
        let error = require_repl_set_zero_force(false).unwrap_err();
        assert!(error.to_string().contains("REPL 中 set-zero 必须显式加 --force"));
        require_repl_set_zero_force(true).unwrap();
    }

    #[test]
    fn handle_command_move_requires_force_before_motion_path() {
        let mut session = ReplSession::new(CliConfig::default());
        session.set_test_observer_positions([2.5, 0.0, 0.0, 0.0, 0.0, 0.0]);
        let control = CommandExecutionControl::new(&ReplHandle::default());

        let error = handle_command("move --joints 0.1", &mut session, &control).unwrap_err();
        assert!(error.to_string().contains("REPL 中大幅移动必须显式加 --force"));
    }

    #[test]
    fn handle_command_set_zero_requires_force_before_write_path() {
        let mut session = ReplSession::new(CliConfig::default());
        let control = CommandExecutionControl::new(&ReplHandle::default());

        let error = handle_command("set-zero", &mut session, &control).unwrap_err();
        assert!(error.to_string().contains("REPL 中 set-zero 必须显式加 --force"));
    }

    #[test]
    fn repl_handle_requests_motion_cancellation() {
        let handle = ReplHandle::default();
        handle.motion_in_progress.store(true, Ordering::SeqCst);

        assert_eq!(
            handle.request_emergency_stop(),
            EmergencyStopOutcome::CancellingMotion
        );
        assert!(handle.cancel_requested.load(Ordering::SeqCst));
        assert!(handle.emergency_requested.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn repl_executor_runs_command_once() {
        let before = TEST_ONCE_COUNT.load(Ordering::SeqCst);
        let mut executor = ReplExecutor::new(CliConfig::default());

        executor.start_command("__test-once".to_string()).unwrap();
        let completion = executor.wait_for_completion().await.unwrap();
        executor.finish_completion(completion);

        assert_eq!(TEST_ONCE_COUNT.load(Ordering::SeqCst), before + 1);
        assert_eq!(executor.status(), "未连接");
    }

    #[tokio::test]
    async fn repl_executor_rejects_busy_commands_and_allows_stop() {
        let before = TEST_MOTION_COUNT.load(Ordering::SeqCst);
        let mut executor = ReplExecutor::new(CliConfig::default());

        executor.start_command("__test-motion".to_string()).unwrap();
        for _ in 0..20 {
            if executor.handle.motion_in_progress.load(Ordering::SeqCst) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
        assert!(executor.start_command("__test-once".to_string()).is_err());

        assert_eq!(
            executor.request_emergency_stop().unwrap(),
            EmergencyStopOutcome::CancellingMotion
        );

        let completion = executor.wait_for_completion().await.unwrap();
        assert!(matches!(
            completion.outcome,
            GuardedCommandOutcome::Success(CommandExecutionOutcome::MotionCancelled)
        ));
        assert!(completion.post_command_stop.as_ref().is_some_and(Result::is_ok));
        executor.finish_completion(completion);

        assert!(TEST_MOTION_COUNT.load(Ordering::SeqCst) > before);
    }

    #[tokio::test]
    async fn handle_line_while_busy_rejects_regular_commands() {
        let mut executor = ReplExecutor::new(CliConfig::default());
        let mut exit_after_completion = false;

        executor.start_command("__test-motion".to_string()).unwrap();
        for _ in 0..20 {
            if executor.handle.motion_in_progress.load(Ordering::SeqCst) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }

        assert!(
            !handle_line_while_busy(&mut executor, "position", &mut exit_after_completion).unwrap()
        );
        assert!(!exit_after_completion);
        assert!(executor.is_busy());
        assert_eq!(
            executor.request_emergency_stop().unwrap(),
            EmergencyStopOutcome::CancellingMotion
        );

        let completion = executor.wait_for_completion().await.unwrap();
        executor.finish_completion(completion);
    }

    #[tokio::test]
    async fn handle_line_while_busy_exit_queues_stop_and_exit() {
        let mut executor = ReplExecutor::new(CliConfig::default());
        let mut exit_after_completion = false;

        executor.start_command("__test-motion".to_string()).unwrap();
        for _ in 0..20 {
            if executor.handle.motion_in_progress.load(Ordering::SeqCst) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }

        assert!(
            !handle_line_while_busy(&mut executor, "exit", &mut exit_after_completion).unwrap()
        );
        assert!(exit_after_completion);
        assert!(executor.handle.cancel_requested.load(Ordering::SeqCst));

        let completion = executor.wait_for_completion().await.unwrap();
        executor.finish_completion(completion);
    }

    #[tokio::test]
    async fn repl_executor_queues_stop_for_non_motion_busy_command() {
        let before = TEST_BUSY_COUNT.load(Ordering::SeqCst);
        let mut executor = ReplExecutor::new(CliConfig::default());

        executor.start_command("__test-busy".to_string()).unwrap();
        assert_eq!(
            executor.request_emergency_stop().unwrap(),
            EmergencyStopOutcome::QueuedAfterCurrent
        );

        let completion = executor.wait_for_completion().await.unwrap();
        assert!(matches!(
            completion.outcome,
            GuardedCommandOutcome::Success(CommandExecutionOutcome::Completed)
        ));
        assert!(completion.post_command_stop.as_ref().is_some_and(Result::is_ok));
        executor.finish_completion(completion);

        assert!(TEST_BUSY_COUNT.load(Ordering::SeqCst) > before);
    }
}
