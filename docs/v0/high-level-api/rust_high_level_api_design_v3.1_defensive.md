# Piper Rust SDK 高层 API 设计方案 v3.1 - 防御性编程补充

> **日期**: 2026-01-23
> **版本**: v3.1 (防御性编程强化)
> **基于**: v3.0 + 深度安全性审查
> **目标**: 解决"最后一英里"的可靠性问题

---

## 📋 执行摘要

v3.0 设计在架构层面已经是"优异"级别，但在通往**极致可靠性**的最后一英里，存在三个关键的防御性编程问题：

1. **"后门"漏洞**: Commander 权限控制不够严格，可能绕过 Type State
2. **状态断裂**: 物理状态与类型状态可能不一致（急停、断线、过热）
3. **dt 抖动**: 控制循环卡顿恢复后可能导致力矩突变

本文档提供**工业级**的解决方案。

---

## 🔒 问题 1: "后门"漏洞 - Commander 权限控制

### 问题描述

```rust
// 用户代码可能这样写：
let (commander, observer, heartbeat) = PiperClient::new(config)?;

// 创建状态机
let piper = Piper {
    commander: commander.clone(),  // Piper 持有一个副本
    observer,
    ...
};

// 用户保留了另一个副本！
let my_commander = commander.clone();

// 线程 1: 通过状态机正常操作
let piper = piper.enable_mit_mode(timeout)?;  // Piper<MitMode>

// 线程 2: 绕过状态机直接操作！❌
std::thread::spawn(move || {
    my_commander.disable_arm()?;  // 物理机器已断电！
});

// 线程 1: 类型系统认为是 MitMode，但物理已经 Standby
piper.command_torques(torques)?;  // 类型检查通过，但实际无效或出错
```

**后果**: Type State Pattern 的保证被破坏。

### 解决方案: 分层权限控制

#### 1.1 内部 RawCommander（完全权限）

```rust
// src/client/raw_commander.rs

/// 原始命令器（仅内部使用，拥有完全权限）
pub(crate) struct RawCommander {
    can_tx: Arc<dyn CanSender>,
    state_tracker: Arc<RwLock<StateTracker>>,  // 追踪物理状态
}

impl RawCommander {
    /// 内部方法：改变控制模式
    pub(crate) fn set_control_mode(
        &self,
        mode: ControlMode,
        move_mode: MoveMode,
        controller: ArmController,
    ) -> Result<(), RobotError> {
        let cmd = ControlModeCommandFrame::new(mode, move_mode, 100, controller, 0, 0);
        self.send_frame(cmd.to_frame())?;

        // 更新状态追踪
        self.state_tracker.write().expect_mode_transition(mode, controller);

        Ok(())
    }

    /// 内部方法：使能/失能
    pub(crate) fn set_motor_enable(&self, enable: bool) -> Result<(), RobotError> {
        let cmd = if enable {
            MotorEnableCommand::enable_all()
        } else {
            MotorEnableCommand::disable_all()
        };
        self.send_frame(cmd.to_frame())?;

        // 更新状态追踪
        self.state_tracker.write().record_enable_command(enable);

        Ok(())
    }

    /// 内部方法：紧急停止
    pub(crate) fn emergency_stop(&self) -> Result<(), RobotError> {
        let cmd = EmergencyStopCommand::emergency_stop();
        self.send_frame(cmd.to_frame())?;

        // 立即标记状态失效
        self.state_tracker.write().mark_emergency_stopped();

        Ok(())
    }

    /// 公开方法：发送运动命令（不改变状态机）
    pub(crate) fn send_mit_command(
        &self,
        joint: Joint,
        position: Rad,
        velocity: RadPerSec,
        kp: f64,
        kd: f64,
        torque: NewtonMeter,
    ) -> Result<(), RobotError> {
        // 先检查状态是否有效
        self.state_tracker.read().check_valid()?;

        let cmd = MitControlCommand::new(
            joint.motor_id(),
            position.0 as f32,
            velocity.0 as f32,
            kp as f32,
            kd as f32,
            torque.0 as f32,
            0x00,
        );
        self.send_realtime(cmd.to_frame())
    }

    fn send_frame(&self, frame: PiperFrame) -> Result<(), RobotError> {
        self.can_tx.send(frame).map_err(Into::into)
    }

    fn send_realtime(&self, frame: PiperFrame) -> Result<(), RobotError> {
        self.can_tx.send_mailbox(frame).map_err(Into::into)
    }
}
```

#### 1.2 公开的 Piper（受限权限）

```rust
// src/client/motion_commander.rs

/// 运动命令器（公开给用户，仅能发送运动指令）
#[derive(Clone)]
pub struct Piper {
    raw: Arc<RawCommander>,
}

impl Piper {
    pub(crate) fn new(raw: Arc<RawCommander>) -> Self {
        Self { raw }
    }

    /// 发送 MIT 控制命令（纯运动指令）
    pub fn send_mit_command(
        &self,
        joint: Joint,
        position: Rad,
        velocity: RadPerSec,
        kp: f64,
        kd: f64,
        torque: NewtonMeter,
    ) -> Result<(), RobotError> {
        self.raw.send_mit_command(joint, position, velocity, kp, kd, torque)
    }

    /// 发送关节位置命令
    pub fn send_position_command(&self, positions: JointPositions) -> Result<(), RobotError> {
        self.raw.send_position_command(positions)
    }

    // ❌ 没有 set_control_mode()
    // ❌ 没有 enable_arm()
    // ❌ 没有 disable_arm()
}
```

#### 1.3 修改 PiperClient::new() 返回

```rust
// src/client/mod.rs

impl PiperClient {
    /// 创建客户端（不再返回完全权限的 Commander）
    pub fn new(
        config: ClientConfig,
    ) -> Result<(Piper, Observer, HeartbeatManager), RobotError> {
        let raw_commander = Arc::new(RawCommander::new(config.can_interface)?);
        let observer = Observer::new(raw_commander.state_tracker.clone());
        let heartbeat = HeartbeatManager::new(raw_commander.clone());

        // 只返回受限的 Piper
        let motion_commander = Piper::new(raw_commander.clone());

        Ok((motion_commander, observer, heartbeat))
    }
}
```

#### 1.4 Piper 状态机持有 RawCommander

```rust
// src/state_machine/mod.rs

pub struct Piper<State> {
    raw_commander: Arc<RawCommander>,  // 内部持有完全权限
    observer: Observer,
    heartbeat: HeartbeatManager,
    config: RobotConfig,
    _state: PhantomData<State>,
}

impl Piper<Standby> {
    /// 使能 MIT 模式（使用内部完全权限）
    pub fn enable_mit_mode(
        mut self,
        timeout: Duration,
    ) -> Result<Piper<MitMode>, RobotError> {
        // 使用 raw_commander 的 pub(crate) 方法
        self.raw_commander.set_motor_enable(true)?;
        // ... 等待使能完成 ...

        self.raw_commander.set_control_mode(
            ControlMode::CanControl,
            MoveMode::Mit,
            ArmController::Mit,
        )?;

        Ok(Piper {
            raw_commander: self.raw_commander,
            observer: self.observer,
            heartbeat: self.heartbeat,
            config: self.config,
            _state: PhantomData,
        })
    }
}

impl Piper<MitMode> {
    /// 用户可以获取受限的 Piper
    pub fn Piper -> Piper {
        Piper::new(self.raw_commander.clone())
    }

    /// 发送力矩命令（直接使用内部方法）
    pub fn command_torques(&self, torques: JointTorques) -> Result<(), RobotError> {
        for joint in Joint::ALL {
            self.raw_commander.send_mit_command(
                joint,
                rad!(0.0),
                RadPerSec(0.0),
                0.0,
                0.0,
                torques[joint],
            )?;
        }
        Ok(())
    }
}
```

### 效果

```rust
// ✅ 用户无法获取完全权限的 Commander
let (motion_cmd, observer, heartbeat) = PiperClient::new(config)?;

// ❌ 编译错误：Piper 没有 disable_arm() 方法
motion_cmd.disable_arm()?;  // ERROR: no method `disable_arm`

// ✅ 只能通过状态机操作
let piper = Piper::<Disconnected>::connect("can0")?
    .enable_mit_mode(timeout)?;

// ✅ 可以获取受限的 Piper 用于多线程
let motion_cmd = piper.Piper;
std::thread::spawn(move || {
    motion_cmd.send_mit_command(...)?;  // OK: 仅运动指令
    // motion_cmd.disable_arm()?;  // ERROR: 方法不存在
});
```

---

## 🔄 问题 2: 状态断裂 - 物理与类型状态不一致

### 问题描述

物理世界的不可控事件：
1. **急停按钮**: 用户按下机械臂上的急停拍
2. **固件保护**: 过热、过流自动保护
3. **通信断开**: CAN 线断开
4. **电源故障**: 外部电源掉电

此时：
- Rust 类型: `Piper<MitMode>`
- 物理状态: `Standby` / `Error` / `Disconnected`

### 解决方案: 状态追踪 + Poisoned 机制

#### 2.1 StateTracker（物理状态追踪器）

```rust
// src/client/state_tracker.rs

use parking_lot::RwLock;
use std::sync::Arc;

/// 物理状态追踪器（实时监控物理状态）
#[derive(Debug)]
pub(crate) struct StateTracker {
    /// 当前期望的控制模式
    expected_mode: ControlMode,
    /// 当前期望的控制器类型
    expected_controller: ArmController,
    /// 状态是否有效（Poisoned 标记）
    valid: bool,
    /// Poison 原因
    poison_reason: Option<String>,
    /// 最后一次状态更新时间
    last_update: Instant,
}

impl StateTracker {
    pub fn new() -> Self {
        Self {
            expected_mode: ControlMode::Standby,
            expected_controller: ArmController::PositionVelocity,
            valid: true,
            poison_reason: None,
            last_update: Instant::now(),
        }
    }

    /// 记录期望的模式转换
    pub fn expect_mode_transition(&mut self, mode: ControlMode, controller: ArmController) {
        self.expected_mode = mode;
        self.expected_controller = controller;
        self.last_update = Instant::now();
    }

    /// 检查状态是否有效
    pub fn check_valid(&self) -> Result<(), RobotError> {
        if !self.valid {
            return Err(RobotError::StatePoisoned {
                reason: self.poison_reason.clone().unwrap_or_default(),
            });
        }
        Ok(())
    }

    /// 标记紧急停止（立即失效）
    pub fn mark_emergency_stopped(&mut self) {
        self.valid = false;
        self.poison_reason = Some("Emergency stop triggered".to_string());
    }

    /// 从 Observer 更新物理状态（定期调用）
    pub fn update_from_hardware(&mut self, hw_state: &RobotState) -> Result<(), RobotError> {
        self.last_update = Instant::now();

        // 检查物理状态是否与期望一致
        if hw_state.control_mode != self.expected_mode {
            log::warn!(
                "State drift detected: expected {:?}, but hardware is {:?}",
                self.expected_mode,
                hw_state.control_mode
            );

            // 如果不一致，且是严重错误，标记为 Poisoned
            if hw_state.arm_status.is_error() {
                self.valid = false;
                self.poison_reason = Some(format!(
                    "Hardware entered error state: {:?}",
                    hw_state.arm_status
                ));
                return Err(RobotError::StateDrift {
                    expected: self.expected_mode,
                    actual: hw_state.control_mode,
                });
            }
        }

        // 检查驱动器错误
        for joint in Joint::ALL {
            if hw_state.driver_errors[joint] {
                self.valid = false;
                self.poison_reason = Some(format!("Driver error on {:?}", joint));
                return Err(RobotError::DriverError {
                    joint,
                    details: "Driver fault detected".to_string(),
                });
            }
        }

        Ok(())
    }

    /// 检查超时（如果长时间未更新，可能断线）
    pub fn check_timeout(&self, timeout: Duration) -> Result<(), RobotError> {
        if self.last_update.elapsed() > timeout {
            return Err(RobotError::StateTimeout {
                elapsed: self.last_update.elapsed(),
            });
        }
        Ok(())
    }

    /// 重置状态（重新连接后）
    pub fn reset(&mut self) {
        self.valid = true;
        self.poison_reason = None;
        self.expected_mode = ControlMode::Standby;
        self.last_update = Instant::now();
    }
}
```

#### 2.2 后台状态监控线程

```rust
// src/client/state_monitor.rs

/// 状态监控器（后台线程定期检查物理状态）
pub(crate) struct StateMonitor {
    state_tracker: Arc<RwLock<StateTracker>>,
    observer: Observer,
    check_interval: Duration,
    handle: Option<JoinHandle<()>>,
    shutdown_tx: channel::Sender<()>,
}

impl StateMonitor {
    pub fn new(
        state_tracker: Arc<RwLock<StateTracker>>,
        observer: Observer,
    ) -> Self {
        let (shutdown_tx, shutdown_rx) = channel::bounded(1);

        Self {
            state_tracker,
            observer,
            check_interval: Duration::from_millis(50),  // 20Hz 检查
            handle: None,
            shutdown_tx,
        }
    }

    /// 启动监控线程
    pub fn start(&mut self) -> Result<(), RobotError> {
        let state_tracker = self.state_tracker.clone();
        let observer = self.observer.clone();
        let check_interval = self.check_interval;
        let shutdown_rx = self.shutdown_tx.subscribe();

        let handle = std::thread::spawn(move || {
            loop {
                // 检查是否收到停止信号
                if shutdown_rx.try_recv().is_ok() {
                    break;
                }

                // 获取硬件状态
                let hw_state = observer.state();

                // 更新状态追踪器
                if let Err(e) = state_tracker.write().update_from_hardware(&hw_state) {
                    log::error!("State monitor detected error: {}", e);
                    // 继续监控，但已标记为 Poisoned
                }

                // 检查超时
                if let Err(e) = state_tracker.read().check_timeout(Duration::from_secs(1)) {
                    log::error!("State timeout: {}", e);
                }

                std::thread::sleep(check_interval);
            }
        });

        self.handle = Some(handle);
        Ok(())
    }

    /// 停止监控线程
    pub fn stop(&mut self) {
        let _ = self.shutdown_tx.send(());
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}
```

#### 2.3 集成到 RawCommander

```rust
// src/client/raw_commander.rs

impl RawCommander {
    pub(crate) fn send_mit_command(
        &self,
        joint: Joint,
        position: Rad,
        velocity: RadPerSec,
        kp: f64,
        kd: f64,
        torque: NewtonMeter,
    ) -> Result<(), RobotError> {
        // ✅ 快速状态校验（每次发送前检查）
        self.state_tracker.read().check_valid()?;

        let cmd = MitControlCommand::new(
            joint.motor_id(),
            position.0 as f32,
            velocity.0 as f32,
            kp as f32,
            kd as f32,
            torque.0 as f32,
            0x00,
        );

        self.send_realtime(cmd.to_frame())
    }
}
```

#### 2.4 用户体验

```rust
// 用户代码
let piper = Piper::<Disconnected>::connect("can0")?
    .enable_mit_mode(timeout)?;  // Piper<MitMode>

// 控制循环
loop {
    // 假设在此期间，用户按下了急停按钮
    // StateMonitor 检测到硬件进入 Emergency Stop 状态
    // StateTracker 被标记为 Poisoned

    let result = piper.command_torques(torques);

    match result {
        Ok(_) => { /* 正常 */ }
        Err(RobotError::StatePoisoned { reason }) => {
            eprintln!("State poisoned: {}", reason);
            eprintln!("Physical state has diverged from type state!");
            eprintln!("Please re-initialize the robot.");
            break;
        }
        Err(e) => { /* 其他错误 */ }
    }
}
```

### 效果

```rust
// 场景：用户按下急停
let piper = Piper::<MitMode>::...;

// 后台 StateMonitor 检测到硬件状态变化
// StateTracker 被标记为 Poisoned

// ❌ 所有后续调用都会返回 StatePoisoned 错误
piper.command_torques(torques)?;  // Error: StatePoisoned

// ✅ 用户必须重新初始化
drop(piper);  // 释放旧实例
let piper = Piper::<Disconnected>::connect("can0")?;  // 重新连接
```

---

## ⏱️ 问题 3: dt 抖动处理

### 问题描述

```rust
// 控制循环卡顿示例
loop {
    let dt = now - last_tick;  // 正常: 5ms

    // 假设此时 OS 调度卡顿...
    // dt 变成 50ms！

    controller.tick(&state, dt)?;  // 积分项爆炸！
}
```

对于 PID 控制器：
- **积分项**: `I += error * dt` → dt 突然变大，积分饱和
- **微分项**: `D = (error - last_error) / dt` → dt 变大，微分噪声

### 解决方案: dt 钳位 + Soft Restart

#### 3.1 改进 ControlLoopConfig

```rust
// src/controller/mod.rs

/// 控制循环配置
#[derive(Debug, Clone)]
pub struct ControlLoopConfig {
    /// 目标控制周期
    pub period: Duration,

    /// Deadline（超过此时间认为发生 jitter）
    pub deadline: Duration,

    /// ✅ 新增：dt 最大值（钳位阈值）
    pub max_dt: Duration,

    /// ✅ 新增：dt 过大时是否重置控制器
    pub reset_on_large_dt: bool,

    /// 超时时间
    pub timeout: Duration,

    /// 使用 spin_sleep
    pub use_spin_sleep: bool,
}

impl Default for ControlLoopConfig {
    fn default() -> Self {
        Self {
            period: Duration::from_millis(5),
            deadline: Duration::from_millis(10),
            max_dt: Duration::from_millis(20),  // 4x period
            reset_on_large_dt: true,
            timeout: Duration::from_secs(30),
            use_spin_sleep: false,
        }
    }
}
```

#### 3.2 改进 Controller Trait

```rust
// src/controller/mod.rs

pub trait Controller {
    type Command;
    type State;
    type Error;

    fn init(&mut self) -> Result<(), Self::Error>;

    fn tick(&mut self, state: &Self::State, dt: Duration) -> Result<Option<Self::Command>, Self::Error>;

    fn is_finished(&self, state: &Self::State) -> bool;

    /// ✅ 新增：重置控制器内部状态
    fn reset(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }

    fn cleanup(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}
```

#### 3.3 改进 run_controller

```rust
// src/controller/run.rs

pub fn run_controller<C, S>(
    controller: &mut C,
    get_state: impl Fn() -> S,
    send_command: impl Fn(C::Command) -> Result<(), C::Error>,
    config: ControlLoopConfig,
) -> Result<ControlLoopStats, C::Error>
where
    C: Controller<State = S>,
{
    controller.init()?;

    let mut stats = ControlLoopStats::new();
    let mut last_tick = Instant::now();

    loop {
        let loop_start = Instant::now();

        // 计算实际 dt
        let raw_dt = loop_start - last_tick;
        last_tick = loop_start;

        // ✅ dt 钳位
        let dt = if raw_dt > config.max_dt {
            log::warn!(
                "Large dt detected: {:?} > {:?}, clamping to max_dt",
                raw_dt,
                config.max_dt
            );
            stats.large_dt_events += 1;

            // ✅ 可选：重置控制器
            if config.reset_on_large_dt {
                log::warn!("Resetting controller due to large dt");
                controller.reset()?;
            }

            config.max_dt
        } else {
            raw_dt
        };

        // 获取状态
        let state = get_state();

        // 检查是否完成
        if controller.is_finished(&state) {
            break;
        }

        // Tick 控制器（使用钳位后的 dt）
        if let Some(command) = controller.tick(&state, dt)? {
            send_command(command)?;
        }

        // 更新统计
        stats.update(loop_start.elapsed(), raw_dt);

        // Deadline 检查
        if raw_dt > config.deadline {
            log::warn!(
                "Control loop deadline missed: {:?} > {:?}",
                raw_dt,
                config.deadline
            );
            stats.deadline_misses += 1;
        }

        // 睡眠策略
        let elapsed = loop_start.elapsed();
        if elapsed < config.period {
            let sleep_time = config.period - elapsed;
            if config.use_spin_sleep {
                spin_sleep::sleep(sleep_time);
            } else {
                std::thread::sleep(sleep_time);
            }
        } else {
            // 本周期已超时，不睡眠
            stats.overrun_cycles += 1;
        }

        // 超时检查
        if stats.elapsed() > config.timeout {
            return Err(C::Error::from(RobotError::Timeout("Controller timeout".into())));
        }
    }

    controller.cleanup()?;
    Ok(stats)
}
```

#### 3.4 改进 ControlLoopStats

```rust
// src/controller/stats.rs

#[derive(Debug, Clone)]
pub struct ControlLoopStats {
    pub iterations: u64,
    pub deadline_misses: u64,
    pub large_dt_events: u64,  // ✅ 新增
    pub overrun_cycles: u64,   // ✅ 新增
    pub min_latency: Duration,
    pub max_latency: Duration,
    pub avg_latency: Duration,
    pub min_dt: Duration,      // ✅ 新增
    pub max_dt: Duration,      // ✅ 新增
    pub avg_dt: Duration,      // ✅ 新增
    start_time: Instant,
}

impl ControlLoopStats {
    fn new() -> Self {
        Self {
            iterations: 0,
            deadline_misses: 0,
            large_dt_events: 0,
            overrun_cycles: 0,
            min_latency: Duration::MAX,
            max_latency: Duration::ZERO,
            avg_latency: Duration::ZERO,
            min_dt: Duration::MAX,
            max_dt: Duration::ZERO,
            avg_dt: Duration::ZERO,
            start_time: Instant::now(),
        }
    }

    fn update(&mut self, latency: Duration, dt: Duration) {
        self.iterations += 1;

        // 更新延迟统计
        self.min_latency = self.min_latency.min(latency);
        self.max_latency = self.max_latency.max(latency);
        let delta_lat = latency.as_secs_f64() - self.avg_latency.as_secs_f64();
        self.avg_latency = Duration::from_secs_f64(
            self.avg_latency.as_secs_f64() + delta_lat / self.iterations as f64
        );

        // ✅ 更新 dt 统计
        self.min_dt = self.min_dt.min(dt);
        self.max_dt = self.max_dt.max(dt);
        let delta_dt = dt.as_secs_f64() - self.avg_dt.as_secs_f64();
        self.avg_dt = Duration::from_secs_f64(
            self.avg_dt.as_secs_f64() + delta_dt / self.iterations as f64
        );
    }

    /// 打印详细统计
    pub fn print_summary(&self) {
        println!("Control Loop Statistics:");
        println!("  Total iterations: {}", self.iterations);
        println!("  Duration: {:?}", self.elapsed());
        println!("  Frequency: {:.1} Hz", self.frequency());
        println!();
        println!("  Latency (command execution time):");
        println!("    Min: {:?}", self.min_latency);
        println!("    Max: {:?}", self.max_latency);
        println!("    Avg: {:?}", self.avg_latency);
        println!();
        println!("  dt (time between iterations):");
        println!("    Min: {:?}", self.min_dt);
        println!("    Max: {:?}", self.max_dt);
        println!("    Avg: {:?}", self.avg_dt);
        println!();
        println!("  Issues:");
        println!("    Deadline misses: {}", self.deadline_misses);
        println!("    Large dt events: {}", self.large_dt_events);
        println!("    Overrun cycles: {}", self.overrun_cycles);
    }
}
```

#### 3.5 PID 控制器示例（支持重置）

```rust
// examples/pid_controller.rs

pub struct PidController {
    kp: f64,
    ki: f64,
    kd: f64,
    target: f64,
    integral: f64,       // 积分项
    last_error: f64,     // 上次误差
}

impl PidController {
    pub fn new(kp: f64, ki: f64, kd: f64, target: f64) -> Self {
        Self {
            kp,
            ki,
            kd,
            target,
            integral: 0.0,
            last_error: 0.0,
        }
    }
}

impl Controller for PidController {
    type Command = f64;
    type State = f64;
    type Error = RobotError;

    fn init(&mut self) -> Result<(), RobotError> {
        self.integral = 0.0;
        self.last_error = 0.0;
        Ok(())
    }

    fn tick(&mut self, state: &f64, dt: Duration) -> Result<Option<f64>, RobotError> {
        let dt_sec = dt.as_secs_f64();

        let error = self.target - state;

        // P 项
        let p = self.kp * error;

        // I 项（带积分饱和保护）
        self.integral += error * dt_sec;
        self.integral = self.integral.clamp(-10.0, 10.0);  // 积分限幅
        let i = self.ki * self.integral;

        // D 项
        let d = if dt_sec > 1e-6 {
            self.kd * (error - self.last_error) / dt_sec
        } else {
            0.0
        };

        self.last_error = error;

        let output = p + i + d;
        Ok(Some(output))
    }

    fn is_finished(&self, state: &f64) -> bool {
        (self.target - state).abs() < 0.01
    }

    /// ✅ 重置积分项和上次误差
    fn reset(&mut self) -> Result<(), RobotError> {
        log::info!("Resetting PID controller internal state");
        self.integral = 0.0;
        self.last_error = 0.0;
        Ok(())
    }
}
```

### 效果

```rust
let stats = run_controller(
    &mut pid_controller,
    || get_position(),
    |cmd| send_command(cmd),
    ControlLoopConfig {
        period: Duration::from_millis(5),
        max_dt: Duration::from_millis(20),    // 4x period
        reset_on_large_dt: true,              // 自动重置
        ..Default::default()
    },
)?;

stats.print_summary();
// Output:
//   Large dt events: 3  ← 发生了 3 次卡顿
//   Deadline misses: 3  ← 3 次超过 deadline
//   Overrun cycles: 1   ← 1 次完全超时
```

---

## 🔄 错误类型扩展

```rust
// src/error.rs

#[derive(Debug, Error)]
pub enum RobotError {
    // ... 现有错误 ...

    // ========== 新增：状态相关错误 ==========

    #[error("State poisoned: {reason}")]
    StatePoisoned {
        reason: String,
    },

    #[error("State drift: expected {expected:?}, but hardware is {actual:?}")]
    StateDrift {
        expected: ControlMode,
        actual: ControlMode,
    },

    #[error("State timeout: no update for {elapsed:?}")]
    StateTimeout {
        elapsed: Duration,
    },

    // ... 其他错误 ...
}
```

---

## 📋 完整示例：重力补偿（防御性版本）

```rust
use piper_sdk::prelude::*;
use std::time::Duration;

fn main() -> Result<(), RobotError> {
    // 1. 连接（使用受限的 Piper）
    let (motion_cmd, observer, mut heartbeat) = PiperClient::new(
        ClientConfig::new("can0")
    )?;

    // 2. 启动 Heartbeat（独立线程保护）
    heartbeat.start(Duration::from_millis(100))?;

    // 3. 创建状态机（持有内部完全权限）
    let piper = Piper::<Disconnected>::connect_from_client(
        motion_cmd, observer, heartbeat
    )?;

    // 4. 切换到 MIT 模式
    let piper = piper.enable_mit_mode(Duration::from_secs(10))?;

    // 5. 创建控制器
    let mut controller = GravityCompensationController::new(
        GravityCompensationModel::new()?,
        1.0,  // damping
    );

    // 6. 运行控制循环（带防御性保护）
    let result = run_controller(
        &mut controller,
        || piper.observe().state().as_ref().clone(),
        |torques| piper.command_torques(torques),
        ControlLoopConfig {
            period: Duration::from_millis(5),
            deadline: Duration::from_millis(10),
            max_dt: Duration::from_millis(20),      // ✅ dt 钳位
            reset_on_large_dt: true,                // ✅ 自动重置
            use_spin_sleep: true,                   // ✅ 低抖动
            timeout: Duration::from_secs(300),
        },
    );

    // 7. 处理结果
    match result {
        Ok(stats) => {
            println!("✅ Control loop completed successfully");
            stats.print_summary();
        }
        Err(RobotError::StatePoisoned { reason }) => {
            eprintln!("❌ State poisoned: {}", reason);
            eprintln!("Physical state has diverged from type state.");
            eprintln!("This usually happens when:");
            eprintln!("  - Emergency stop button was pressed");
            eprintln!("  - Firmware protection triggered (overheat, overcurrent)");
            eprintln!("  - Communication lost");
        }
        Err(e) => {
            eprintln!("❌ Error: {}", e);
        }
    }

    // 8. 安全退出（自动 relax + disable）
    let piper = piper.disable()?;

    Ok(())
}
```

---

## 📊 改进总结

| 问题 | v3.0 设计 | v3.1 防御性改进 | 效果 |
|------|-----------|----------------|------|
| **后门漏洞** | Commander 公开可用 | RawCommander(内部) + Piper(受限) | ✅ 无法绕过 Type State |
| **状态断裂** | 无检测机制 | StateTracker + StateMonitor | ✅ 检测物理与类型不一致 |
| **dt 抖动** | 原始 dt | dt 钳位 + 自动重置 | ✅ 防止积分饱和和微分噪声 |

---

## 🎯 实现优先级（Quick Wins）

### Priority 0 (立即实施)

1. **收紧 Commander 权限** (1 天)
   - 实现 `RawCommander` (内部) 和 `Piper` (公开)
   - 修改 `PiperClient::new()` 返回值
   - 影响：防止绕过 Type State

2. **dt 钳位** (0.5 天)
   - 修改 `ControlLoopConfig` 添加 `max_dt`
   - 修改 `run_controller` 实现钳位逻辑
   - 影响：防止控制器异常

### Priority 1 (重要)

3. **StateTracker** (2 天)
   - 实现状态追踪器
   - 实现 Poisoned 机制
   - 影响：检测状态断裂

4. **StateMonitor** (1 天)
   - 实现后台监控线程
   - 影响：实时检测硬件状态

### Priority 2 (增强)

5. **完善统计** (0.5 天)
   - 扩展 `ControlLoopStats`
   - 添加 `print_summary()`

---

## ✅ 总结

### v3.1 相比 v3.0 的改进

1. **"后门"防护**: 权限分层，无法绕过状态机
2. **状态一致性**: 实时监控物理状态，检测断裂
3. **鲁棒性**: dt 钳位防止异常恢复时的力矩突变

### 工作量

- **Priority 0**: 1.5 天
- **Priority 1**: 3 天
- **Priority 2**: 0.5 天
- **总计**: 约 5 天（1 周）

### 关键价值

这些改进将 v3.0 从"优异"提升到**"极致可靠"**：

✅ **编译期 + 运行时双重保护**
✅ **物理世界与类型世界同步**
✅ **控制算法鲁棒性**
✅ **真正的工业级可靠性**

---

**文档版本**: v3.1
**创建日期**: 2026-01-23
**作者**: AI Assistant (基于防御性编程审查)

