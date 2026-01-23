# Piper Rust SDK 高层 API 设计方案 (v3.0 - 工业级)

> **日期**: 2026-01-23
> **版本**: v3.0 (基于深度技术审查改进)
> **设计目标**: 工业级机器人控制库，充分利用 Rust 类型系统和并发优势

---

## 📋 执行摘要

本方案在 v2.0 基础上，基于深度技术审查进行了重大改进，将设计从"Python SDK 的 Rust 实现"提升为**充分利用 Rust 类型系统优势的工业级机器人控制库**。

### 核心改进

1. **Type State Pattern 成为核心设计**：编译期防止非法状态转换
2. **控制权反转**：Tick/Iterator 模式替代内部 Loop，可集成到任何事件循环
3. **读写分离**：支持并发监控和控制，适合多线程架构
4. **强类型单位**：NewType idiom 防止单位错误（度 vs 弧度）
5. **Heartbeat 机制**：后台线程保证安全，不依赖 Drop
6. **真正的实时性**：spin_sleep、deadline 检查、jitter 监控
7. **可恢复错误**：区分 Recoverable 和 Fatal 错误

---

## 🎯 设计原则（修订版）

### 1. 编译期安全优先 (Compile-Time Safety First)
- **Type State Pattern**: 非法状态转换无法编译通过
- **强类型单位**: `Rad`/`Deg`/`NewtonMeter` 防止单位混淆
- **关节索引安全**: 枚举替代魔数

### 2. 控制权交给用户 (User Controls the Loop)
- **No Hidden Loops**: 所有阻塞操作都应该是可选的高层封装
- **Tick/Iterator Pattern**: 用户拥有事件循环的控制权
- **可集成性**: 可以集成到 Tokio、嵌入式 RTOS、游戏引擎等任何系统

### 3. 并发友好 (Concurrency-Friendly)
- **读写分离**: Commander/Observer 模式
- **内部可变性**: Arc + Mutex/RwLock/ArcSwap 合理使用
- **Send + Sync**: 所有类型都应该是线程安全的

### 4. 真正的实时性 (True Real-Time)
- **Deadline 监控**: 检测控制循环 jitter
- **Spin Sleep 选项**: 高频控制的低抖动睡眠
- **性能统计**: 内置延迟和频率监控

### 5. 安全第一，但不依赖单一机制 (Layered Safety)
- **Heartbeat**: 后台线程独立保证安全
- **Drop 作为备份**: Best effort 清理
- **固件超时**: 固件侧超时保护

---

## 🏗️ 改进后的架构设计

```
┌───────────────────────────────────────────────────────────┐
│  Layer 4: High-Level Planners & Policies                  │
│  - TrajectoryPlanner (Iterator<Item=Command>)             │
│  - GravityCompensator (Filter trait)                      │
│  - CollisionAvoidance                                     │
│  - 用户自定义 Controllers                                  │
└───────────────────────────────────────────────────────────┘
                        ↓ 使用
┌───────────────────────────────────────────────────────────┐
│  Layer 3: Typed Controllers (Type State)                  │
│  - Piper<Standby> / Piper<MitActive> / Piper<PositionMode>│
│  - 编译期保证状态转换合法                                   │
│  - 强类型单位 (Rad, NewtonMeter)                          │
└───────────────────────────────────────────────────────────┘
                        ↓ 使用
┌───────────────────────────────────────────────────────────┐
│  Layer 2: Concurrent Client (Reader-Writer Split)         │
│  - Commander: 发送命令 (Clone-able)                        │
│  - Observer: 读取状态 (Clone-able)                         │
│  - Heartbeat: 后台线程独立保证安全                         │
└───────────────────────────────────────────────────────────┘
                        ↓ 使用
┌───────────────────────────────────────────────────────────┐
│  Layer 1: Async/Sync I/O (现有 SDK 扩展)                   │
│  - Protocol encoding/decoding                             │
│  - SocketCAN wrapper                                      │
│  - 状态同步 (ArcSwap)                                      │
└───────────────────────────────────────────────────────────┘
```

---

## 📦 Layer 1: 强类型系统基础

### 1.1 单位类型 (NewType Pattern)

```rust
// src/types/units.rs

use std::ops::{Add, Sub, Mul, Div};

/// 弧度（SI 单位）
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub struct Rad(pub f64);

/// 度
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub struct Deg(pub f64);

/// 牛顿·米（力矩）
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub struct NewtonMeter(pub f64);

/// 弧度每秒（角速度）
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub struct RadPerSec(pub f64);

// 自动转换
impl From<Deg> for Rad {
    fn from(deg: Deg) -> Self {
        Rad(deg.0 * std::f64::consts::PI / 180.0)
    }
}

impl From<Rad> for Deg {
    fn from(rad: Rad) -> Self {
        Deg(rad.0 * 180.0 / std::f64::consts::PI)
    }
}

// 支持基本运算
impl Add for Rad {
    type Output = Self;
    fn add(self, rhs: Self) -> Self {
        Rad(self.0 + rhs.0)
    }
}

impl Sub for Rad {
    type Output = Self;
    fn sub(self, rhs: Self) -> Self {
        Rad(self.0 - rhs.0)
    }
}

impl Mul<f64> for Rad {
    type Output = Self;
    fn mul(self, rhs: f64) -> Self {
        Rad(self.0 * rhs)
    }
}

impl Div<f64> for Rad {
    type Output = Self;
    fn div(self, rhs: f64) -> Self {
        Rad(self.0 / rhs)
    }
}

// 便捷宏
#[macro_export]
macro_rules! rad {
    ($val:expr) => { Rad($val) };
}

#[macro_export]
macro_rules! deg {
    ($val:expr) => { Deg($val) };
}
```

### 1.2 关节索引安全

```rust
// src/types/joint.rs

/// 关节索引（编译期保证有效性）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Joint {
    J1 = 0,
    J2 = 1,
    J3 = 2,
    J4 = 3,
    J5 = 4,
    J6 = 5,
}

impl Joint {
    pub const ALL: [Joint; 6] = [
        Joint::J1, Joint::J2, Joint::J3,
        Joint::J4, Joint::J5, Joint::J6,
    ];

    pub fn index(self) -> usize {
        self as usize
    }

    pub fn motor_id(self) -> u8 {
        (self as u8) + 1
    }
}

/// 6 轴关节配置（强类型数组）
#[derive(Debug, Clone, Copy)]
pub struct JointArray<T> {
    data: [T; 6],
}

impl<T> JointArray<T> {
    pub fn new(data: [T; 6]) -> Self {
        Self { data }
    }

    pub fn get(&self, joint: Joint) -> &T {
        &self.data[joint.index()]
    }

    pub fn get_mut(&mut self, joint: Joint) -> &mut T {
        &mut self.data[joint.index()]
    }

    pub fn set(&mut self, joint: Joint, value: T) {
        self.data[joint.index()] = value;
    }
}

impl<T> std::ops::Index<Joint> for JointArray<T> {
    type Output = T;
    fn index(&self, joint: Joint) -> &Self::Output {
        self.get(joint)
    }
}

impl<T> std::ops::IndexMut<Joint> for JointArray<T> {
    fn index_mut(&mut self, joint: Joint) -> &mut Self::Output {
        self.get_mut(joint)
    }
}

// 类型别名
pub type JointPositions = JointArray<Rad>;
pub type JointVelocities = JointArray<RadPerSec>;
pub type JointTorques = JointArray<NewtonMeter>;
```

### 1.3 错误类型（区分可恢复性）

```rust
// src/error.rs

use thiserror::Error;

#[derive(Debug, Error)]
pub enum RobotError {
    // ========== Recoverable Errors ==========

    #[error("Timeout: {0}")]
    Timeout(String),

    #[error("Joint limit exceeded: {joint:?} at {position:?} (limit: {limit:?})")]
    JointLimitExceeded {
        joint: Joint,
        position: Rad,
        limit: (Rad, Rad),
    },

    #[error("Motion not completed within deadline")]
    DeadlineMissed,

    #[error("Communication retry exhausted")]
    CommunicationRetry,

    // ========== Fatal Errors ==========

    #[error("Emergency stop triggered: {reason}")]
    EmergencyStop { reason: String },

    #[error("Motor overheat detected: {joint:?}")]
    MotorOverheat { joint: Joint },

    #[error("Driver error on {joint:?}: {details}")]
    DriverError { joint: Joint, details: String },

    #[error("Collision detected")]
    Collision,

    #[error("Hardware fault: {0}")]
    HardwareFault(String),

    #[error("Firmware incompatible: expected {expected}, got {actual}")]
    FirmwareIncompatible { expected: String, actual: String },

    // ========== System Errors ==========

    #[error("CAN interface error: {0}")]
    CanError(#[from] CanError),

    #[error("Invalid state transition: from {from} to {to}")]
    InvalidStateTransition { from: String, to: String },

    #[error("Heartbeat lost")]
    HeartbeatLost,
}

impl RobotError {
    /// 判断错误是否可恢复
    pub fn is_recoverable(&self) -> bool {
        matches!(
            self,
            RobotError::Timeout(_)
                | RobotError::JointLimitExceeded { .. }
                | RobotError::DeadlineMissed
                | RobotError::CommunicationRetry
        )
    }

    /// 判断是否需要紧急停止
    pub fn requires_emergency_stop(&self) -> bool {
        matches!(
            self,
            RobotError::MotorOverheat { .. }
                | RobotError::Collision
                | RobotError::HardwareFault(_)
        )
    }
}
```

---

## 📦 Layer 2: 读写分离的并发客户端

### 2.1 核心架构

```rust
// src/client/mod.rs

use std::sync::Arc;
use parking_lot::RwLock;
use crossbeam::channel;

/// 机器人客户端（内部结构，用户不直接使用）
pub(crate) struct PiperClient {
    // CAN 接口
    can_tx: Arc<dyn CanSender>,
    can_rx: Arc<dyn CanReceiver>,

    // 共享状态（无锁读取）
    state: Arc<ArcSwap<RobotState>>,

    // Heartbeat 线程句柄
    heartbeat_handle: Option<JoinHandle<()>>,
    heartbeat_tx: channel::Sender<HeartbeatCommand>,

    // 配置
    config: ClientConfig,
}

/// 命令发送器（可 Clone，多线程安全）
#[derive(Clone)]
pub struct Commander {
    client: Arc<PiperClient>,
}

/// 状态观察器（可 Clone，多线程安全）
#[derive(Clone)]
pub struct Observer {
    client: Arc<PiperClient>,
}

/// Heartbeat 管理器
pub struct HeartbeatManager {
    client: Arc<PiperClient>,
    enabled: Arc<AtomicBool>,
    interval: Duration,
}

impl PiperClient {
    /// 创建客户端并分离读写
    pub fn new(config: ClientConfig) -> Result<(Commander, Observer, HeartbeatManager), RobotError> {
        let client = Arc::new(Self::new_internal(config)?);

        let commander = Commander { client: client.clone() };
        let observer = Observer { client: client.clone() };
        let heartbeat = HeartbeatManager::new(client.clone());

        Ok((commander, observer, heartbeat))
    }
}

impl Commander {
    /// 发送原始 CAN 帧
    pub fn send_frame(&self, frame: PiperFrame) -> Result<(), RobotError> {
        self.client.can_tx.send(frame)?;
        Ok(())
    }

    /// 发送实时帧（邮箱模式）
    pub fn send_realtime(&self, frame: PiperFrame) -> Result<(), RobotError> {
        self.client.can_tx.send_mailbox(frame)?;
        Ok(())
    }

    /// 紧急停止
    pub fn emergency_stop(&self) -> Result<(), RobotError> {
        let cmd = EmergencyStopCommand::emergency_stop();
        self.send_frame(cmd.to_frame())
    }

    /// 发送 MIT 控制命令
    pub fn send_mit_command(
        &self,
        joint: Joint,
        position: Rad,
        velocity: RadPerSec,
        kp: f64,
        kd: f64,
        torque: NewtonMeter,
    ) -> Result<(), RobotError> {
        // 验证参数
        if kp < 0.0 || kp > 500.0 {
            return Err(RobotError::InvalidParameter(
                format!("kp out of range: {kp}")
            ));
        }

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

impl Observer {
    /// 获取最新的机器人状态（无锁读取）
    pub fn state(&self) -> Arc<RobotState> {
        self.client.state.load()
    }

    /// 获取关节位置
    pub fn joint_positions(&self) -> JointPositions {
        let state = self.state();
        state.joint_positions
    }

    /// 获取关节速度
    pub fn joint_velocities(&self) -> JointVelocities {
        let state = self.state();
        state.joint_velocities
    }

    /// 获取关节力矩
    pub fn joint_torques(&self) -> JointTorques {
        let state = self.state();
        state.joint_torques
    }

    /// 检查机械臂是否已使能
    pub fn is_arm_enabled(&self) -> bool {
        let state = self.state();
        state.all_joints_enabled()
    }

    /// 等待条件满足（带超时）
    pub fn wait_for<F>(&self, condition: F, timeout: Duration) -> Result<(), RobotError>
    where
        F: Fn(&RobotState) -> bool,
    {
        let start = Instant::now();
        loop {
            let state = self.state();
            if condition(&*state) {
                return Ok(());
            }
            if start.elapsed() > timeout {
                return Err(RobotError::Timeout("Wait condition not met".to_string()));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

impl HeartbeatManager {
    /// 启动 Heartbeat（后台线程）
    pub fn start(&mut self, interval: Duration) -> Result<(), RobotError> {
        self.enabled.store(true, Ordering::SeqCst);

        let client = self.client.clone();
        let enabled = self.enabled.clone();

        let handle = std::thread::spawn(move || {
            while enabled.load(Ordering::SeqCst) {
                // 发送 Heartbeat 帧
                let _ = client.send_heartbeat();
                std::thread::sleep(interval);
            }
        });

        self.client.heartbeat_handle = Some(handle);
        Ok(())
    }

    /// 停止 Heartbeat
    pub fn stop(&mut self) {
        self.enabled.store(false, Ordering::SeqCst);
        if let Some(handle) = self.client.heartbeat_handle.take() {
            let _ = handle.join();
        }
    }
}
```

### 2.2 使用示例

```rust
// 创建客户端
let (commander, observer, mut heartbeat) = PiperClient::new(config)?;

// 启动 Heartbeat（独立线程保证安全）
heartbeat.start(Duration::from_millis(100))?;

// 线程 1: 控制循环
let cmd = commander.clone();
std::thread::spawn(move || {
    loop {
        cmd.send_mit_command(Joint::J1, rad!(0.5), ...)?;
        std::thread::sleep(Duration::from_millis(5));
    }
});

// 线程 2: 监控和日志
let obs = observer.clone();
std::thread::spawn(move || {
    loop {
        let state = obs.state();
        log::info!("Position: {:?}", state.joint_positions);
        std::thread::sleep(Duration::from_millis(50));
    }
});
```

---

## 📦 Layer 3: Type State Pattern 核心设计

### 3.1 状态类型定义

```rust
// src/state_machine/states.rs

use std::marker::PhantomData;

/// 未连接状态（初始状态）
pub struct Disconnected;

/// Standby 状态（已连接，未使能）
pub struct Standby;

/// 位置速度控制模式（使能，使用内置控制器）
pub struct PositionVelocityMode;

/// MIT 控制模式（使能，直接力矩控制）
pub struct MitMode;

/// 状态化的 Piper 机器人
pub struct Piper<State> {
    commander: Commander,
    observer: Observer,
    heartbeat: HeartbeatManager,
    config: RobotConfig,
    _state: PhantomData<State>,
}

/// 机器人配置
#[derive(Debug, Clone)]
pub struct RobotConfig {
    pub arm_type: PiperArmType,
    pub installation_pos: ArmInstallationPos,
    pub joint_limits: JointArray<(Rad, Rad)>,
    pub torque_limits: JointArray<NewtonMeter>,
}
```

### 3.2 状态转换实现

```rust
// src/state_machine/transitions.rs

// ========== 初始化：Disconnected -> Standby ==========

impl Piper<Disconnected> {
    /// 连接到机器人
    pub fn connect(can_interface: &str) -> Result<Piper<Standby>, RobotError> {
        let config = ClientConfig::new(can_interface);
        let (commander, observer, heartbeat) = PiperClient::new(config)?;

        // 验证连接
        observer.wait_for(
            |state| state.hardware_timestamp_us > 0,
            Duration::from_secs(5),
        )?;

        Ok(Piper {
            commander,
            observer,
            heartbeat,
            config: RobotConfig::default(),
            _state: PhantomData,
        })
    }
}

// ========== Standby -> PositionVelocityMode ==========

impl Piper<Standby> {
    /// 使能机械臂（位置速度模式）
    pub fn enable_position_mode(
        mut self,
        timeout: Duration,
    ) -> Result<Piper<PositionVelocityMode>, RobotError> {
        // 1. 启动 Heartbeat
        self.heartbeat.start(Duration::from_millis(100))?;

        // 2. 使能电机（自动重试）
        let start = Instant::now();
        loop {
            self.commander.enable_arm()?;
            std::thread::sleep(Duration::from_millis(100));

            if self.observer.is_arm_enabled() {
                break;
            }

            if start.elapsed() > timeout {
                return Err(RobotError::Timeout("Failed to enable arm".to_string()));
            }

            std::thread::sleep(Duration::from_millis(400));
        }

        // 3. 设置控制模式
        self.commander.set_control_mode(
            ControlMode::CanCommand,
            MoveMode::Joint,
            ArmController::PositionVelocity,
        )?;

        std::thread::sleep(Duration::from_millis(100));

        Ok(Piper {
            commander: self.commander,
            observer: self.observer,
            heartbeat: self.heartbeat,
            config: self.config,
            _state: PhantomData,
        })
    }

    /// 使能机械臂（MIT 模式）
    pub fn enable_mit_mode(
        mut self,
        timeout: Duration,
    ) -> Result<Piper<MitMode>, RobotError> {
        // 1. 启动 Heartbeat
        self.heartbeat.start(Duration::from_millis(100))?;

        // 2. 使能电机
        let start = Instant::now();
        loop {
            self.commander.enable_arm()?;
            std::thread::sleep(Duration::from_millis(100));

            if self.observer.is_arm_enabled() {
                break;
            }

            if start.elapsed() > timeout {
                return Err(RobotError::Timeout("Failed to enable arm".to_string()));
            }

            std::thread::sleep(Duration::from_millis(400));
        }

        // 3. 设置 MIT 模式
        self.commander.set_control_mode(
            ControlMode::CanCommand,
            MoveMode::Mit,
            ArmController::Mit,
        )?;

        std::thread::sleep(Duration::from_millis(100));

        Ok(Piper {
            commander: self.commander,
            observer: self.observer,
            heartbeat: self.heartbeat,
            config: self.config,
            _state: PhantomData,
        })
    }
}

// ========== PositionVelocityMode 方法 ==========

impl Piper<PositionVelocityMode> {
    /// 命令关节位置
    pub fn command_position(&self, target: JointPositions) -> Result<(), RobotError> {
        // 验证关节限位
        for joint in Joint::ALL {
            let pos = target[joint];
            let limit = self.config.joint_limits[joint];
            if pos < limit.0 || pos > limit.1 {
                return Err(RobotError::JointLimitExceeded {
                    joint,
                    position: pos,
                    limit,
                });
            }
        }

        // 发送命令
        self.commander.send_joint_position_command(target)
    }

    /// 禁用并返回 Standby
    pub fn disable(mut self) -> Result<Piper<Standby>, RobotError> {
        self.commander.disable_arm()?;
        self.heartbeat.stop();

        // 等待进入 Standby
        self.observer.wait_for(
            |state| state.control_mode == ControlMode::Standby,
            Duration::from_secs(5),
        )?;

        Ok(Piper {
            commander: self.commander,
            observer: self.observer,
            heartbeat: self.heartbeat,
            config: self.config,
            _state: PhantomData,
        })
    }
}

// ========== MitMode 方法 ==========

impl Piper<MitMode> {
    /// 发送 MIT 控制命令（单关节）
    pub fn command_joint(
        &self,
        joint: Joint,
        position: Rad,
        velocity: RadPerSec,
        kp: f64,
        kd: f64,
        torque: NewtonMeter,
    ) -> Result<(), RobotError> {
        // 验证力矩限制
        let limit = self.config.torque_limits[joint];
        if torque.0.abs() > limit.0 {
            return Err(RobotError::TorqueLimitExceeded { joint, torque, limit });
        }

        self.commander.send_mit_command(joint, position, velocity, kp, kd, torque)
    }

    /// 发送纯力矩命令（所有关节）
    pub fn command_torques(&self, torques: JointTorques) -> Result<(), RobotError> {
        for joint in Joint::ALL {
            self.command_joint(
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

    /// 读取当前状态
    pub fn observe(&self) -> &Observer {
        &self.observer
    }

    /// 禁用并返回 Standby
    pub fn disable(mut self) -> Result<Piper<Standby>, RobotError> {
        // 先放松关节（逐渐降低力矩）
        self.relax_joints(Duration::from_secs(2))?;

        self.commander.disable_arm()?;
        self.heartbeat.stop();

        self.observer.wait_for(
            |state| state.control_mode == ControlMode::Standby,
            Duration::from_secs(5),
        )?;

        Ok(Piper {
            commander: self.commander,
            observer: self.observer,
            heartbeat: self.heartbeat,
            config: self.config,
            _state: PhantomData,
        })
    }

    /// 逐渐放松关节
    fn relax_joints(&self, duration: Duration) -> Result<(), RobotError> {
        let num_steps = (duration.as_secs_f64() * 200.0) as usize;
        let current_pos = self.observer.joint_positions();

        for step in 0..num_steps {
            let progress = step as f64 / num_steps as f64;
            let kp = 2.0 * (1.0 - progress).powf(2.0) + 0.01;
            let kd = 1.0 * (1.0 - progress).powf(2.0) + 0.01;

            for joint in Joint::ALL {
                self.command_joint(
                    joint,
                    current_pos[joint],
                    RadPerSec(0.0),
                    kp,
                    kd,
                    NewtonMeter(0.0),
                )?;
            }

            std::thread::sleep(Duration::from_millis(5));
        }

        Ok(())
    }
}

// ========== Drop 实现（备份安全机制）==========

impl<State> Drop for Piper<State> {
    fn drop(&mut self) {
        // Best effort 清理
        let _ = self.commander.emergency_stop();
        self.heartbeat.stop();
    }
}
```

### 3.3 使用示例

```rust
// 编译期保证状态转换合法
let piper = Piper::<Disconnected>::connect("can0")?;  // Piper<Standby>

// ❌ 编译错误：Standby 状态无法发送 MIT 命令
// piper.command_torques(...);  // ERROR: no method `command_torques` for `Piper<Standby>`

// ✅ 正确：先切换到 MIT 模式
let piper = piper.enable_mit_mode(Duration::from_secs(10))?;  // Piper<MitMode>

// ✅ 现在可以发送力矩命令
let torques = JointTorques::new([
    NewtonMeter(1.5),
    NewtonMeter(2.0),
    NewtonMeter(0.5),
    NewtonMeter(0.3),
    NewtonMeter(0.2),
    NewtonMeter(0.1),
]);
piper.command_torques(torques)?;

// 安全退出
let piper = piper.disable()?;  // 返回 Piper<Standby>
```

---

## 📦 Layer 4: 控制权反转 - Tick/Iterator 模式

### 4.1 核心 Trait 设计

```rust
// src/controller/mod.rs

use std::time::{Duration, Instant};

/// 控制器 Trait（Tick 模式）
pub trait Controller {
    type Command;
    type State;
    type Error;

    /// 初始化控制器
    fn init(&mut self) -> Result<(), Self::Error>;

    /// 更新控制器（每个控制周期调用一次）
    ///
    /// # 参数
    /// - `state`: 当前机器人状态
    /// - `dt`: 距离上次调用的时间间隔
    ///
    /// # 返回
    /// - `Some(Command)`: 需要发送的命令
    /// - `None`: 本周期无需发送命令
    fn tick(&mut self, state: &Self::State, dt: Duration) -> Result<Option<Self::Command>, Self::Error>;

    /// 检查控制器是否已完成目标
    fn is_finished(&self, state: &Self::State) -> bool;

    /// 清理资源
    fn cleanup(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

/// 运行控制器的辅助函数（用户拥有循环控制权）
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
        let dt = loop_start - last_tick;
        last_tick = loop_start;

        // 获取状态
        let state = get_state();

        // 检查是否完成
        if controller.is_finished(&state) {
            break;
        }

        // Tick 控制器
        if let Some(command) = controller.tick(&state, dt)? {
            send_command(command)?;
        }

        // 更新统计
        stats.update(loop_start.elapsed());

        // Deadline 检查
        if dt > config.deadline {
            log::warn!(
                "Control loop deadline missed: {:?} > {:?}",
                dt,
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
        }

        // 超时检查
        if stats.elapsed() > config.timeout {
            return Err(C::Error::from(RobotError::Timeout("Controller timeout".into())));
        }
    }

    controller.cleanup()?;
    Ok(stats)
}

/// 控制循环配置
#[derive(Debug, Clone)]
pub struct ControlLoopConfig {
    /// 控制周期
    pub period: Duration,
    /// Deadline（超过此时间认为发生 jitter）
    pub deadline: Duration,
    /// 超时时间（控制器未完成的最大允许时间）
    pub timeout: Duration,
    /// 使用 spin_sleep（低抖动，但占 CPU）
    pub use_spin_sleep: bool,
}

impl Default for ControlLoopConfig {
    fn default() -> Self {
        Self {
            period: Duration::from_millis(5),   // 200Hz
            deadline: Duration::from_millis(10), // 2x period
            timeout: Duration::from_secs(30),
            use_spin_sleep: false,
        }
    }
}

/// 控制循环统计
#[derive(Debug, Clone)]
pub struct ControlLoopStats {
    pub iterations: u64,
    pub deadline_misses: u64,
    pub min_latency: Duration,
    pub max_latency: Duration,
    pub avg_latency: Duration,
    start_time: Instant,
}

impl ControlLoopStats {
    fn new() -> Self {
        Self {
            iterations: 0,
            deadline_misses: 0,
            min_latency: Duration::MAX,
            max_latency: Duration::ZERO,
            avg_latency: Duration::ZERO,
            start_time: Instant::now(),
        }
    }

    fn update(&mut self, latency: Duration) {
        self.iterations += 1;
        self.min_latency = self.min_latency.min(latency);
        self.max_latency = self.max_latency.max(latency);

        // 增量平均
        let delta = latency.as_secs_f64() - self.avg_latency.as_secs_f64();
        self.avg_latency = Duration::from_secs_f64(
            self.avg_latency.as_secs_f64() + delta / self.iterations as f64
        );
    }

    pub fn elapsed(&self) -> Duration {
        self.start_time.elapsed()
    }

    pub fn frequency(&self) -> f64 {
        self.iterations as f64 / self.elapsed().as_secs_f64()
    }
}
```

### 4.2 重力补偿控制器示例

```rust
// src/controller/gravity_compensation.rs

/// 重力补偿控制器（Tick 模式）
pub struct GravityCompensationController {
    model: GravityCompensationModel,
    damping: f64,
}

impl Controller for GravityCompensationController {
    type Command = JointTorques;
    type State = RobotState;
    type Error = RobotError;

    fn init(&mut self) -> Result<(), RobotError> {
        log::info!("Gravity compensation controller initialized");
        Ok(())
    }

    fn tick(&mut self, state: &RobotState, _dt: Duration) -> Result<Option<JointTorques>, RobotError> {
        // 计算重力补偿力矩
        let hover_torque = self.model.predict(&state.joint_positions)?;

        // 阻尼力矩（稳定性）
        let mut stability_torque = JointTorques::new([NewtonMeter(0.0); 6]);
        for joint in Joint::ALL {
            let damping_torque = -state.joint_velocities[joint].0 * self.damping;
            stability_torque[joint] = NewtonMeter(damping_torque);
        }

        // 组合力矩
        let mut total_torque = JointTorques::new([NewtonMeter(0.0); 6]);
        for joint in Joint::ALL {
            total_torque[joint] = NewtonMeter(
                hover_torque[joint].0 + stability_torque[joint].0
            );
        }

        Ok(Some(total_torque))
    }

    fn is_finished(&self, _state: &RobotState) -> bool {
        // 重力补偿是持续运行的
        false
    }
}

// ========== 使用示例 ==========

fn main() -> Result<(), RobotError> {
    let piper = Piper::<Disconnected>::connect("can0")?
        .enable_mit_mode(Duration::from_secs(10))?;

    let mut controller = GravityCompensationController {
        model: GravityCompensationModel::new()?,
        damping: 1.0,
    };

    // 用户拥有循环控制权！
    let stats = run_controller(
        &mut controller,
        || piper.observe().state().as_ref().clone(),  // 获取状态
        |cmd| piper.command_torques(cmd),              // 发送命令
        ControlLoopConfig {
            period: Duration::from_millis(5),
            use_spin_sleep: true,  // 低抖动模式
            ..Default::default()
        },
    )?;

    println!("Control loop finished:");
    println!("  Iterations: {}", stats.iterations);
    println!("  Frequency: {:.1} Hz", stats.frequency());
    println!("  Avg latency: {:?}", stats.avg_latency);
    println!("  Deadline misses: {}", stats.deadline_misses);

    Ok(())
}
```

### 4.3 轨迹规划器（Iterator 模式）

```rust
// src/planner/trajectory.rs

/// 轨迹点
#[derive(Debug, Clone)]
pub struct TrajectoryPoint {
    pub time: Duration,
    pub positions: JointPositions,
    pub velocities: JointVelocities,
    pub accelerations: JointVelocities,
}

/// 轨迹规划器（Iterator 模式）
pub struct TrajectoryPlanner {
    start: JointPositions,
    end: JointPositions,
    duration: Duration,
    current_time: Duration,
    dt: Duration,
}

impl TrajectoryPlanner {
    pub fn new(start: JointPositions, end: JointPositions, duration: Duration) -> Self {
        Self {
            start,
            end,
            duration,
            current_time: Duration::ZERO,
            dt: Duration::from_millis(5),
        }
    }
}

impl Iterator for TrajectoryPlanner {
    type Item = TrajectoryPoint;

    fn next(&mut self) -> Option<Self::Item> {
        if self.current_time > self.duration {
            return None;
        }

        // 三次样条插值
        let t = self.current_time.as_secs_f64() / self.duration.as_secs_f64();
        let s = 3.0 * t.powi(2) - 2.0 * t.powi(3);  // Smooth step
        let ds_dt = (6.0 * t - 6.0 * t.powi(2)) / self.duration.as_secs_f64();

        let mut positions = JointPositions::new([Rad(0.0); 6]);
        let mut velocities = JointVelocities::new([RadPerSec(0.0); 6]);

        for joint in Joint::ALL {
            let p0 = self.start[joint].0;
            let p1 = self.end[joint].0;
            positions[joint] = Rad(p0 + (p1 - p0) * s);
            velocities[joint] = RadPerSec((p1 - p0) * ds_dt);
        }

        let point = TrajectoryPoint {
            time: self.current_time,
            positions,
            velocities,
            accelerations: JointVelocities::new([RadPerSec(0.0); 6]),
        };

        self.current_time += self.dt;
        Some(point)
    }
}

// ========== 使用示例 ==========

fn move_smoothly(
    piper: &Piper<MitMode>,
    target: JointPositions,
    duration: Duration,
) -> Result<(), RobotError> {
    let start = piper.observe().joint_positions();
    let trajectory = TrajectoryPlanner::new(start, target, duration);

    for point in trajectory {
        // 用户可以在这里插入自己的逻辑！
        if collision_detected() {
            piper.command_torques(JointTorques::zero())?;
            return Err(RobotError::Collision);
        }

        // 发送位置命令（使用 MIT 模式的位置控制）
        for joint in Joint::ALL {
            piper.command_joint(
                joint,
                point.positions[joint],
                point.velocities[joint],
                5.0,  // kp
                0.8,  // kd
                NewtonMeter(0.0),
            )?;
        }

        std::thread::sleep(Duration::from_millis(5));
    }

    Ok(())
}
```

---

## 🎓 完整使用示例对比

### Python piper_control 风格

```python
piper = piper_interface.PiperInterface("can0")
piper_init.reset_arm(piper, ArmController.MIT, MoveMode.MIT)

with piper_control.MitJointPositionController(
    piper, kp_gains=5.0, kd_gains=0.8
) as controller:
    controller.move_to_position(target, timeout=5.0)
```

### Rust v3.0 风格（Type State + Tick）

```rust
// 1. 连接并切换到 MIT 模式（Type State 保证安全）
let piper = Piper::<Disconnected>::connect("can0")?
    .enable_mit_mode(Duration::from_secs(10))?;

// 2. Tick 模式控制器（用户拥有循环控制权）
let mut controller = MitPositionController::new(
    JointArray::new([5.0; 6]),  // kp
    JointArray::new([0.8; 6]),  // kd
);

let stats = run_controller(
    &mut controller,
    || piper.observe().state().as_ref().clone(),
    |cmd: MitCommand| piper.command_mit(cmd),
    ControlLoopConfig::default(),
)?;

// 3. 安全退出（Type State 保证先 relax 再 disable）
let piper = piper.disable()?;  // 自动 relax_joints()
```

---

## 📋 改进后的实现优先级

### Phase 1: 基础类型系统（P0）- 1 周

**目标**: 编译期安全和单位类型

1. ✅ 实现 `Rad`, `Deg`, `NewtonMeter` 等强类型单位
2. ✅ 实现 `Joint` 枚举和 `JointArray<T>`
3. ✅ 实现 `RobotError` 并区分 `is_recoverable()`
4. ✅ 编写单元测试
5. ✅ 更新文档

**成果**: 用户永远不会混淆度和弧度

---

### Phase 2: 读写分离客户端（P0）- 1.5 周

**目标**: 并发友好的底层架构

1. ✅ 实现 `Commander` / `Observer` 分离
2. ✅ 实现 `HeartbeatManager` 后台线程
3. ✅ 实现 `Observer::wait_for()` 阻塞等待
4. ✅ 性能测试（延迟、吞吐量）
5. ✅ 集成测试（多线程场景）

**成果**: 可以在控制的同时进行监控和日志

---

### Phase 3: Type State 核心（P1）- 2 周

**目标**: 编译期状态转换安全

1. ✅ 实现 `Piper<Disconnected>` / `<Standby>` / `<MitMode>`
2. ✅ 实现所有状态转换方法
3. ✅ 实现 `enable_xxx_blocking()` 自动重试
4. ✅ 实现 `Drop` trait（Best effort 清理）
5. ✅ 编写状态机测试
6. ✅ 编写文档和示例

**成果**: 用户无法在错误的状态调用方法

---

### Phase 4: Tick/Iterator 控制器（P1）- 1.5 周

**目标**: 控制权反转，用户拥有循环

1. ✅ 实现 `Controller` trait
2. ✅ 实现 `run_controller()` 辅助函数
3. ✅ 实现 `ControlLoopStats` 性能监控
4. ✅ 实现 `GravityCompensationController` 示例
5. ✅ 实现 `TrajectoryPlanner` Iterator
6. ✅ 实现 `spin_sleep` 支持
7. ✅ 编写完整的 gravity compensation example

**成果**: 控制循环可以集成到任何事件系统

---

### Phase 5: 优化和完善（P2）- 1 周

**目标**: 生产级质量

1. ✅ Deadline 检查和 jitter 监控
2. ✅ 碰撞检测集成
3. ✅ 夹爪控制
4. ✅ 日志和 tracing 集成
5. ✅ 性能优化（profile-guided）
6. ✅ 文档完善（Rustdoc + mdBook）
7. ✅ Cookbook 和 FAQ

---

## 🔒 安全性多层保障

### 层次 1: 编译期（Type State）

```rust
// ❌ 编译错误
let piper = Piper::<Standby>::connect("can0")?;
piper.command_torques(...);  // ERROR: no method for Piper<Standby>
```

### 层次 2: 运行时验证

```rust
// ✅ 运行时检查关节限位
pub fn command_position(&self, target: JointPositions) -> Result<...> {
    for joint in Joint::ALL {
        if !self.config.joint_limits[joint].contains(target[joint]) {
            return Err(RobotError::JointLimitExceeded { ... });
        }
    }
}
```

### 层次 3: Heartbeat（后台线程）

```rust
// 控制线程卡死或 Panic，Heartbeat 自动停止
// 固件侧超时 -> 紧急停止
heartbeat.start(Duration::from_millis(100))?;
```

### 层次 4: Drop（Best Effort）

```rust
impl<State> Drop for Piper<State> {
    fn drop(&mut self) {
        let _ = self.commander.emergency_stop();
    }
}
```

### 层次 5: 固件超时（硬件层）

- 固件侧如果 500ms 未收到 Heartbeat，自动进入 Standby

---

## 🎯 设计决策总结

| 决策点 | v2.0 设计 | v3.0 设计（改进） | 理由 |
|--------|-----------|------------------|------|
| 状态安全 | Result 运行时检查 | Type State 编译期检查 | 更安全，零运行时开销 |
| 单位类型 | 原始 f64 | `Rad`/`Deg` NewType | 防止单位混淆 |
| 控制循环 | 内部 `loop` | Tick/Iterator | 用户拥有控制权，可集成到任何系统 |
| 并发 | 单一 `&Piper` | Commander/Observer 分离 | 支持并发监控和控制 |
| 安全机制 | Drop trait | Heartbeat + Drop + Type State | 多层保障，不依赖单一机制 |
| 实时性 | `thread::sleep` | `spin_sleep` + deadline 检查 | 真正的低抖动实时控制 |
| 错误处理 | 单一 Result | Recoverable vs Fatal | 更精细的错误恢复策略 |
| 关节索引 | `u8` (1-6) | `Joint` 枚举 | 编译期防止越界 |

---

## 🚀 未来扩展方向

### 1. 异步 API（Tokio 集成）

```rust
#[cfg(feature = "async")]
impl Piper<Standby> {
    pub async fn enable_mit_mode_async(self) -> Result<Piper<MitMode>, RobotError> {
        tokio::spawn(async move {
            // 异步等待使能完成
        }).await
    }
}
```

### 2. 实时任务调度器

```rust
pub struct RealtimeScheduler {
    tasks: Vec<Box<dyn RealtimeTask>>,
    period: Duration,
}

impl RealtimeScheduler {
    pub fn run(&mut self) {
        // 实时任务调度
    }
}
```

### 3. ROS2 集成

```rust
pub struct Ros2Bridge {
    piper: Arc<Piper<MitMode>>,
    node: rclrs::Node,
}
```

### 4. Safety Monitor

```rust
pub trait SafetyMonitor {
    fn check(&self, state: &RobotState) -> Result<(), SafetyViolation>;
}

pub struct CompositeSafetyMonitor {
    monitors: Vec<Box<dyn SafetyMonitor>>,
}
```

---

## ✅ 总结

### v3.0 相比 v2.0 的核心改进

1. **Type State Pattern**: 从"未来方向"变成"核心设计"
2. **强类型单位**: 编译期防止单位错误
3. **控制权反转**: Tick/Iterator 模式，用户拥有循环
4. **读写分离**: Commander/Observer，并发友好
5. **Heartbeat 机制**: 独立线程保证安全
6. **真正的实时性**: spin_sleep、deadline 检查
7. **多层安全保障**: 编译期 + 运行时 + Heartbeat + Drop + 固件

### 工作量估算（修订）

- **Phase 1**: 基础类型系统（1 周）
- **Phase 2**: 读写分离客户端（1.5 周）
- **Phase 3**: Type State 核心（2 周）
- **Phase 4**: Tick/Iterator 控制器（1.5 周）
- **Phase 5**: 优化和完善（1 周）

**总计**: 约 7 周，2500-3000 行代码

### 关键价值

这不是"Python SDK 的 Rust 翻译"，而是：

✅ **充分利用 Rust 类型系统** 的工业级机器人控制库
✅ **编译期保证安全性**，而不仅仅是运行时检查
✅ **用户拥有控制权**，可集成到任何系统
✅ **并发友好**，支持复杂的多线程架构
✅ **真正的实时性**，适合高频控制
✅ **多层安全保障**，生产环境可用

---

**报告生成日期**: 2026-01-23
**报告作者**: AI Assistant
**版本**: v3.0 (工业级设计)

