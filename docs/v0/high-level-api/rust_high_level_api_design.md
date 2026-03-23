# Piper Rust SDK 高层 API 设计方案

> **日期**: 2026-01-23
> **版本**: v2.0
> **基于**: `piper_control` Python 项目深度分析
> **目标**: 设计符合 Rust 习惯、用户友好的高层 API

---

## 📋 执行摘要

本文档基于对 Python `piper_control` 项目的深度调研，提出了一套完整的 Rust 高层 API 设计方案。**核心发现**：

1. **Python 项目采用三层架构**：
   - **Layer 1 (piper_interface)**: 薄封装层，隐藏底层 C SDK 细节
   - **Layer 2 (piper_init)**: 阻塞式辅助函数，自动重试和等待
   - **Layer 3 (piper_control)**: Controller 模式，生命周期管理

2. **关键用户体验优势**：
   - ✅ **自动等待完成**: 使能/失能等操作自动重试直到成功
   - ✅ **上下文管理器**: 自动清理资源，退出时回到安全位置
   - ✅ **单位统一**: 隐藏底层缩放因子，统一使用 SI 单位
   - ✅ **类型安全**: 枚举类型代替魔数
   - ✅ **参数验证**: 自动裁剪关节限位，防止非法命令

3. **Rust 设计策略**：
   - 采用 **Builder 模式** 替代 Python 的上下文管理器
   - 利用 **类型状态** (Type State Pattern) 强制正确的操作序列
   - 提供 **同步和异步** 两种 API 风格
   - 使用 **trait-based 扩展** 保持核心 API 简洁

---

## 🎯 设计目标

### 1. 易用性 (Ease of Use)
- **零魔数**: 所有常量使用枚举和命名常量
- **最少样板代码**: 常用操作 1-2 行完成
- **智能默认值**: 合理的默认参数
- **自动等待**: 使能等操作自动阻塞直到完成

### 2. 安全性 (Safety)
- **编译时检查**: 类型状态机防止非法操作序列
- **自动资源管理**: RAII 和 Drop trait 确保清理
- **参数验证**: 关节限位、力矩限制等编译时或运行时检查
- **明确的错误处理**: Result 类型，详细错误信息

### 3. 性能 (Performance)
- **零开销抽象**: 高层 API 不应引入性能损失
- **灵活的控制**: 提供高频控制路径（邮箱模式）
- **无锁状态读取**: 继承底层 SDK 的性能优势

### 4. Rust 习惯 (Idiomatic Rust)
- **所有权明确**: 借用检查器友好
- **trait-based 设计**: 可扩展性
- **零成本抽象**: 泛型和单态化
- **文档和示例**: Rustdoc 和丰富的 examples

---

## 🏗️ 三层架构设计

```
┌────────────────────────────────────────────────────┐
│  Layer 3: Controller Traits & Implementations      │
│  - JointPositionController                         │
│  - MitController                                   │
│  - GripperController                               │
│  - 生命周期管理、上下文感知                         │
└────────────────────────────────────────────────────┘
                      ↓ 使用
┌────────────────────────────────────────────────────┐
│  Layer 2: High-Level Helpers                       │
│  - Piper::enable_arm_blocking()                    │
│  - Piper::move_to_position_blocking()              │
│  - 阻塞式操作、自动重试、便捷方法                    │
└────────────────────────────────────────────────────┘
                      ↓ 使用
┌────────────────────────────────────────────────────┐
│  Layer 1: Thin Wrappers (现有 SDK + 扩展)           │
│  - Piper::emergency_stop()                         │
│  - Piper::set_motor_enable()                       │
│  - Piper::send_joint_mit_control()                 │
│  - 单位转换、枚举封装、一行调用                      │
└────────────────────────────────────────────────────┘
                      ↓ 使用
┌────────────────────────────────────────────────────┐
│  Layer 0: Low-Level Protocol (现有 SDK)             │
│  - Protocol structs (MitControlCommand, etc.)      │
│  - CAN frame I/O                                   │
└────────────────────────────────────────────────────┘
```

### 用户可以选择任何层次：
- **高级用户**: 使用 Layer 3 Controller，最简洁
- **中级用户**: 使用 Layer 2 阻塞式方法
- **高级控制**: 使用 Layer 1 直接方法
- **专家级**: 直接使用 Layer 0 protocol

---

## 📦 Layer 1: 薄封装层设计

### 1.1 核心扩展方法

在 `Piper` 结构体上添加高层方法：

```rust
// src/robot/high_level.rs

impl Piper {
    // ==================== 紧急停止 ====================

    /// 紧急停止机器人
    ///
    /// 立即停止所有运动，保持当前位置。
    pub fn emergency_stop(&self) -> Result<(), RobotError> {
        let cmd = EmergencyStopCommand::emergency_stop();
        let frame = cmd.to_frame();
        self.send_frame(frame)
    }

    /// 恢复紧急停止状态
    pub fn resume_from_emergency_stop(&self) -> Result<(), RobotError> {
        let cmd = EmergencyStopCommand::resume();
        let frame = cmd.to_frame();
        self.send_frame(frame)
    }

    // ==================== 电机使能 ====================

    /// 设置所有电机使能状态
    ///
    /// # 注意
    /// 此方法是非阻塞的，不会等待电机实际使能完成。
    /// 如果需要等待使能完成，请使用 `enable_arm_blocking()`。
    pub fn set_motor_enable(&self, enable: bool) -> Result<(), RobotError> {
        let cmd = if enable {
            MotorEnableCommand::enable_all()
        } else {
            MotorEnableCommand::disable_all()
        };
        let frame = cmd.to_frame();
        self.send_frame(frame)
    }

    /// 使能机械臂（非阻塞）
    #[inline]
    pub fn enable_arm(&self) -> Result<(), RobotError> {
        self.set_motor_enable(true)
    }

    /// 失能机械臂（非阻塞）
    #[inline]
    pub fn disable_arm(&self) -> Result<(), RobotError> {
        self.set_motor_enable(false)
    }

    // ==================== MIT 模式控制 ====================

    /// 启用或禁用 MIT 控制模式
    ///
    /// MIT 模式允许直接控制电机扭矩，用于高级力控应用。
    ///
    /// # 警告
    /// MIT 模式是高级功能，使用不当可能导致机器人损坏。
    pub fn enable_mit_mode(&self, enable: bool) -> Result<(), RobotError> {
        let cmd = ControlModeCommandFrame::new(
            ControlModeCommand::CanControl,
            MoveMode::MoveP,
            0,
            if enable { MitMode::Mit } else { MitMode::PositionVelocity },
            0,
            InstallPosition::Invalid,
        );
        let frame = cmd.to_frame();
        self.send_frame(frame)
    }

    /// 发送 MIT 控制命令到指定关节
    ///
    /// # 参数
    /// - `motor_id`: 电机 ID (1-6)
    /// - `pos_ref`: 位置参考值（弧度）
    /// - `vel_ref`: 速度参考值（rad/s）
    /// - `kp`: 比例增益
    /// - `kd`: 微分增益
    /// - `torque`: 扭矩参考值（N·m）
    pub fn send_joint_mit_control(
        &self,
        motor_id: u8,
        pos_ref: f32,
        vel_ref: f32,
        kp: f32,
        kd: f32,
        torque: f32,
    ) -> Result<(), RobotError> {
        if !(1..=6).contains(&motor_id) {
            return Err(RobotError::InvalidParameter(
                format!("Invalid motor_id: {motor_id}. Expected 1-6")
            ));
        }

        let cmd = MitControlCommand::new(
            motor_id,
            pos_ref,
            vel_ref,
            kp,
            kd,
            torque,
            0x00,
        );
        let frame = cmd.to_frame();
        self.send_frame(frame)
    }

    /// 发送 MIT 控制命令（实时模式，低延迟）
    ///
    /// 使用邮箱模式发送，适用于高频控制循环（>500Hz）。
    pub fn send_joint_mit_control_realtime(
        &self,
        motor_id: u8,
        pos_ref: f32,
        vel_ref: f32,
        kp: f32,
        kd: f32,
        torque: f32,
    ) -> Result<(), RobotError> {
        if !(1..=6).contains(&motor_id) {
            return Err(RobotError::InvalidParameter(
                format!("Invalid motor_id: {motor_id}. Expected 1-6")
            ));
        }

        let cmd = MitControlCommand::new(motor_id, pos_ref, vel_ref, kp, kd, torque, 0x00);
        let frame = cmd.to_frame();
        self.send_realtime(frame)
    }

    // ==================== 关节位置控制 ====================

    /// 命令关节位置（使用内置位置控制器）
    ///
    /// # 注意
    /// 机器人需要处于 POSITION_VELOCITY 控制模式和 JOINT 移动模式。
    pub fn command_joint_positions(&self, positions: &[f64; 6]) -> Result<(), RobotError> {
        // 验证和裁剪关节限位
        let mut clipped = *positions;
        for (i, pos) in clipped.iter_mut().enumerate() {
            let limits = JOINT_LIMITS[i];
            *pos = pos.clamp(limits.0, limits.1);
        }

        // TODO: 发送关节位置命令
        // 这需要底层 protocol 支持 JointCtrl 命令
        todo!("Implement joint position command")
    }

    // ==================== 状态查询（组合） ====================

    /// 获取完整的关节状态（位置 + 速度 + 力矩）
    ///
    /// 这是一个便捷方法，组合了多个底层状态查询。
    pub fn get_joint_state(&self) -> JointState {
        let position = self.get_joint_position();
        let dynamic = self.get_joint_dynamic();

        JointState {
            positions: position.joint_pos,
            velocities: dynamic.joint_vel,
            efforts: dynamic.joint_current,
            timestamp_us: position.hardware_timestamp_us,
        }
    }

    /// 检查机械臂是否已使能
    pub fn is_arm_enabled(&self) -> bool {
        let state = self.get_joint_driver_low_speed();
        // 检查所有 6 个关节的驱动器是否都已使能
        state.joint1.enabled
            && state.joint2.enabled
            && state.joint3.enabled
            && state.joint4.enabled
            && state.joint5.enabled
            && state.joint6.enabled
    }

    /// 获取 CAN 接口名称
    pub fn interface_name(&self) -> &str {
        &self.interface_name
    }
}
```

### 1.2 新增类型定义

```rust
// src/robot/types.rs

/// 完整的关节状态
#[derive(Debug, Clone)]
pub struct JointState {
    /// 关节位置（弧度）
    pub positions: [f64; 6],
    /// 关节速度（rad/s）
    pub velocities: [f64; 6],
    /// 关节力矩/电流
    pub efforts: [f64; 6],
    /// 硬件时间戳（微秒）
    pub timestamp_us: u64,
}

/// 机械臂类型（不同型号有不同的关节限位）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PiperArmType {
    Piper,
    PiperH,
    PiperX,
    PiperL,
}

impl PiperArmType {
    /// 获取该型号的关节限位（弧度）
    pub fn joint_limits(&self) -> [(f64, f64); 6] {
        match self {
            PiperArmType::Piper => [
                (-2.687, 2.687),
                (0.0, 3.403),
                (-3.054, 0.0),
                (-1.745, 1.954),
                (-1.309, 1.309),
                (-1.745, 1.745),
            ],
            PiperArmType::PiperH => [
                (-2.687, 2.687),
                (0.0, 3.403),
                (-3.054, 0.0),
                (-2.216, 2.216),
                (-1.570, 1.570),
                (-2.967, 2.967),
            ],
            PiperArmType::PiperX => [
                (-2.687, 2.687),
                (0.0, 3.403),
                (-3.054, 0.0),
                (-1.570, 1.570),
                (-1.570, 1.570),
                (-2.879, 2.879),
            ],
            PiperArmType::PiperL => [
                (-2.687, 2.687),
                (0.0, 3.403),
                (-3.054, 0.0),
                (-2.216, 2.216),
                (-1.570, 1.570),
                (-2.967, 2.967),
            ],
        }
    }
}

/// 机械臂安装位置
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArmInstallationPos {
    /// 垂直向上安装
    Upright = 0x01,
    /// 侧面左安装
    Left = 0x02,
    /// 侧面右安装
    Right = 0x03,
}

/// 标准的休息位置配置
pub mod rest_positions {
    /// 垂直安装的休息位置
    pub const UPRIGHT: [f64; 6] = [0.0, 0.0, 0.0, 0.02, 0.5, 0.0];

    /// 左侧安装的休息位置
    pub const LEFT: [f64; 6] = [1.71, 2.96, -2.65, 1.41, -0.081, -0.190];

    /// 右侧安装的休息位置
    pub const RIGHT: [f64; 6] = [-1.66, 2.91, -2.74, 0.0545, -0.271, 0.0979];
}
```

---

## 📦 Layer 2: 阻塞式辅助方法

Python `piper_init.py` 的核心价值在于 **自动重试和等待**。Rust 实现需要提供类似体验：

```rust
// src/robot/blocking_helpers.rs

use std::time::{Duration, Instant};

impl Piper {
    /// 使能机械臂（阻塞，直到使能完成）
    ///
    /// 此方法会自动重试直到所有电机使能成功，或超时。
    ///
    /// # 参数
    /// - `timeout`: 超时时间
    ///
    /// # Example
    /// ```no_run
    /// use piper_sdk::robot::PiperBuilder;
    /// use std::time::Duration;
    ///
    /// let piper = PiperBuilder::new().build()?;
    /// piper.enable_arm_blocking(Duration::from_secs(10))?;
    /// ```
    pub fn enable_arm_blocking(&self, timeout: Duration) -> Result<(), RobotError> {
        let start = Instant::now();

        loop {
            // 发送使能命令
            self.enable_arm()?;

            // 等待一小段时间
            std::thread::sleep(Duration::from_millis(100));

            // 检查是否已使能
            if self.is_arm_enabled() {
                return Ok(());
            }

            // 检查超时
            if start.elapsed() > timeout {
                return Err(RobotError::Timeout(
                    "Failed to enable arm within timeout".to_string()
                ));
            }

            // 长等待后重试
            std::thread::sleep(Duration::from_millis(400));
        }
    }

    /// 失能机械臂（阻塞，直到失能完成）
    ///
    /// # 警告
    /// 此操作会断电，机械臂会掉落！确保机械臂有支撑。
    pub fn disable_arm_blocking(&self, timeout: Duration) -> Result<(), RobotError> {
        let start = Instant::now();

        loop {
            // 发送恢复紧急停止（进入 Standby 模式）
            self.resume_from_emergency_stop()?;
            std::thread::sleep(Duration::from_millis(100));

            // 检查是否进入 Standby 模式
            let status = self.get_arm_status();
            if status.control_mode == ControlMode::Standby
                && status.arm_status == ArmStatus::Normal {
                return Ok(());
            }

            if start.elapsed() > timeout {
                return Err(RobotError::Timeout(
                    "Failed to disable arm within timeout".to_string()
                ));
            }

            std::thread::sleep(Duration::from_millis(400));
        }
    }

    /// 重置机械臂（失能 -> 使能）
    ///
    /// # 警告
    /// 此操作会断电，机械臂会掉落！
    pub fn reset_arm_blocking(
        &self,
        arm_controller: ArmController,
        move_mode: MoveMode,
        timeout: Duration,
    ) -> Result<(), RobotError> {
        self.disable_arm_blocking(timeout)?;
        self.enable_arm_blocking(timeout)?;

        // 设置控制模式
        self.set_arm_mode(arm_controller, move_mode)?;
        std::thread::sleep(Duration::from_millis(500));

        Ok(())
    }

    /// 移动到目标位置（阻塞，直到到达或超时）
    ///
    /// # 参数
    /// - `target`: 目标关节位置（弧度）
    /// - `threshold`: 到达阈值（弧度）
    /// - `timeout`: 超时时间
    ///
    /// # 返回
    /// - `Ok(true)`: 成功到达目标
    /// - `Ok(false)`: 超时但未到达
    pub fn move_to_position_blocking(
        &self,
        target: &[f64; 6],
        threshold: f64,
        timeout: Duration,
    ) -> Result<bool, RobotError> {
        let start = Instant::now();

        loop {
            // 发送目标位置命令
            self.command_joint_positions(target)?;

            // 检查是否到达
            let current = self.get_joint_state();
            let mut reached = true;
            for i in 0..6 {
                if (current.positions[i] - target[i]).abs() > threshold {
                    reached = false;
                    break;
                }
            }

            if reached {
                return Ok(true);
            }

            if start.elapsed() > timeout {
                return Ok(false);
            }

            // 控制频率：200Hz
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// 设置控制模式
    pub fn set_arm_mode(
        &self,
        arm_controller: ArmController,
        move_mode: MoveMode,
    ) -> Result<(), RobotError> {
        let cmd = ControlModeCommandFrame::new(
            ControlModeCommand::CanControl,
            move_mode,
            100, // speed
            arm_controller,
            0,
            InstallPosition::Invalid,
        );
        let frame = cmd.to_frame();
        self.send_frame(frame)
    }
}

/// 控制模式
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArmController {
    PositionVelocity = 0x00,
    Mit = 0xAD,
}

/// 移动模式
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MoveMode {
    Position = 0x00,
    Joint = 0x01,
    Linear = 0x02,
    Circular = 0x03,
    Mit = 0x04,
}
```

---

## 📦 Layer 3: Controller 模式设计

Python 的 Controller 模式非常优雅，Rust 可以用 **Builder + RAII** 实现类似效果：

### 3.1 Controller Trait

```rust
// src/robot/controller/mod.rs

/// 关节位置控制器 trait
pub trait JointPositionController {
    /// 启动控制器
    fn start(&mut self) -> Result<(), RobotError>;

    /// 停止控制器（会执行清理，如返回休息位置）
    fn stop(&mut self) -> Result<(), RobotError>;

    /// 命令关节位置
    fn command_joints(&mut self, target: &[f64; 6]) -> Result<(), RobotError>;

    /// 移动到目标位置（阻塞）
    fn move_to_position(
        &mut self,
        target: &[f64; 6],
        threshold: f64,
        timeout: Duration,
    ) -> Result<bool, RobotError>;
}

/// 夹爪控制器 trait
pub trait GripperController {
    /// 打开夹爪
    fn command_open(&mut self) -> Result<(), RobotError>;

    /// 关闭夹爪
    fn command_close(&mut self) -> Result<(), RobotError>;

    /// 命令夹爪位置
    fn command_position(&mut self, position: f64, effort: f64) -> Result<(), RobotError>;
}
```

### 3.2 MIT 控制器实现

```rust
// src/robot/controller/mit_controller.rs

/// MIT 模式关节位置控制器
///
/// 使用 MIT 模式实现关节位置控制，可以自定义 PD 增益。
///
/// # Example
/// ```no_run
/// use piper_sdk::robot::{PiperBuilder, MitJointController};
/// use std::time::Duration;
///
/// let piper = PiperBuilder::new().build()?;
///
/// let mut controller = MitJointController::builder(&piper)
///     .kp_gains([5.0; 6])
///     .kd_gains([0.8; 6])
///     .rest_position([0.0, 0.0, 0.0, 0.02, 0.5, 0.0])
///     .build()?;
///
/// // 控制器会自动启动 MIT 模式
/// controller.move_to_position(&[0.5, 0.7, -0.4, 0.2, 0.3, 0.5], 0.01, Duration::from_secs(5))?;
///
/// // Drop 时自动返回休息位置并停止
/// ```
pub struct MitJointController<'a> {
    piper: &'a Piper,
    kp_gains: [f64; 6],
    kd_gains: [f64; 6],
    rest_position: Option<[f64; 6]>,
    joint_flip_map: [bool; 6],
    started: bool,
}

impl<'a> MitJointController<'a> {
    /// 创建 Builder
    pub fn builder(piper: &'a Piper) -> MitJointControllerBuilder<'a> {
        MitJointControllerBuilder {
            piper,
            kp_gains: [5.0; 6],
            kd_gains: [0.8; 6],
            rest_position: Some(rest_positions::UPRIGHT),
        }
    }

    /// 命令关节扭矩（纯力矩控制）
    pub fn command_torques(&mut self, torques: &[f64; 6]) -> Result<(), RobotError> {
        for (i, &torque) in torques.iter().enumerate() {
            let motor_id = (i + 1) as u8;
            let mut t = torque;

            // 处理固件版本的关节翻转问题
            if self.joint_flip_map[i] {
                t = -t;
            }

            // 裁剪到力矩限制
            t = t.clamp(-MIT_TORQUE_LIMITS[i], MIT_TORQUE_LIMITS[i]);

            self.piper.send_joint_mit_control_realtime(
                motor_id,
                0.0, // pos_ref
                0.0, // vel_ref
                0.0, // kp
                0.0, // kd
                t as f32,
            )?;
        }
        Ok(())
    }

    /// 逐渐放松关节（降低增益）
    pub fn relax_joints(&mut self, duration: Duration) -> Result<(), RobotError> {
        let num_steps = (duration.as_secs_f64() * 200.0) as usize;
        let current_pos = self.piper.get_joint_state().positions;

        for step in 0..num_steps {
            let progress = step as f64 / num_steps as f64;
            // 指数衰减增益
            let kp = 2.0 * (1.0 - progress).powf(2.0) + 0.01;
            let kd = 1.0 * (1.0 - progress).powf(2.0) + 0.01;

            let kp_gains = [kp; 6];
            let kd_gains = [kd; 6];

            self.command_joints_with_gains(&current_pos, &kp_gains, &kd_gains, &[0.0; 6])?;
            std::thread::sleep(Duration::from_millis(5));
        }

        Ok(())
    }

    fn command_joints_with_gains(
        &mut self,
        target: &[f64; 6],
        kp_gains: &[f64; 6],
        kd_gains: &[f64; 6],
        torques_ff: &[f64; 6],
    ) -> Result<(), RobotError> {
        for i in 0..6 {
            let motor_id = (i + 1) as u8;
            let mut pos = target[i];
            let mut torque = torques_ff[i];

            // 裁剪位置到关节限位
            let limits = self.piper.arm_type().joint_limits()[i];
            pos = pos.clamp(limits.0, limits.1);

            // 处理固件翻转
            if self.joint_flip_map[i] {
                pos = -pos;
                torque = -torque;
            }

            // 裁剪力矩
            torque = torque.clamp(-MIT_TORQUE_LIMITS[i], MIT_TORQUE_LIMITS[i]);

            self.piper.send_joint_mit_control(
                motor_id,
                pos as f32,
                0.0,
                kp_gains[i] as f32,
                kd_gains[i] as f32,
                torque as f32,
            )?;
        }
        Ok(())
    }
}

impl<'a> JointPositionController for MitJointController<'a> {
    fn start(&mut self) -> Result<(), RobotError> {
        if self.started {
            return Ok(());
        }

        // 设置为 MIT 模式
        self.piper.set_arm_mode(ArmController::Mit, MoveMode::Mit)?;
        std::thread::sleep(Duration::from_millis(100));

        self.started = true;
        Ok(())
    }

    fn stop(&mut self) -> Result<(), RobotError> {
        if !self.started {
            return Ok(());
        }

        // 如需回到休息位置，应显式调用 move_to_rest()
        if let Some(rest_pos) = self.rest_position {
            let _ = self.move_to_position(&rest_pos, 0.1, Duration::from_secs(2));
        }

        // 放松关节
        self.relax_joints(Duration::from_secs(2))?;

        self.started = false;
        Ok(())
    }

    fn command_joints(&mut self, target: &[f64; 6]) -> Result<(), RobotError> {
        self.command_joints_with_gains(target, &self.kp_gains, &self.kd_gains, &[0.0; 6])
    }

    fn move_to_position(
        &mut self,
        target: &[f64; 6],
        threshold: f64,
        timeout: Duration,
    ) -> Result<bool, RobotError> {
        let start = Instant::now();

        loop {
            self.command_joints(target)?;

            let current = self.piper.get_joint_state().positions;
            let mut reached = true;
            for i in 0..6 {
                if (current[i] - target[i]).abs() > threshold {
                    reached = false;
                    break;
                }
            }

            if reached {
                return Ok(true);
            }

            if start.elapsed() > timeout {
                return Ok(false);
            }

            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

impl<'a> Drop for MitJointController<'a> {
    fn drop(&mut self) {
        // 自动清理：返回休息位置并停止
        let _ = self.stop();
    }
}

/// MIT 控制器 Builder
pub struct MitJointControllerBuilder<'a> {
    piper: &'a Piper,
    kp_gains: [f64; 6],
    kd_gains: [f64; 6],
    rest_position: Option<[f64; 6]>,
}

impl<'a> MitJointControllerBuilder<'a> {
    pub fn kp_gains(mut self, gains: [f64; 6]) -> Self {
        self.kp_gains = gains;
        self
    }

    pub fn kd_gains(mut self, gains: [f64; 6]) -> Self {
        self.kd_gains = gains;
        self
    }

    pub fn rest_position(mut self, position: [f64; 6]) -> Self {
        self.rest_position = Some(position);
        self
    }

    pub fn no_rest_position(mut self) -> Self {
        self.rest_position = None;
        self
    }

    pub fn build(self) -> Result<MitJointController<'a>, RobotError> {
        // 检查固件版本以确定关节翻转映射
        let firmware_version = self.piper.get_firmware_version()?;
        let joint_flip_map = if firmware_version < "1.7-3" {
            [true, true, false, true, false, true]
        } else {
            [false; 6]
        };

        let mut controller = MitJointController {
            piper: self.piper,
            kp_gains: self.kp_gains,
            kd_gains: self.kd_gains,
            rest_position: self.rest_position,
            joint_flip_map,
            started: false,
        };

        // 自动启动
        controller.start()?;

        Ok(controller)
    }
}

const MIT_TORQUE_LIMITS: [f64; 6] = [10.0, 10.0, 10.0, 10.0, 10.0, 10.0];
```

### 3.3 内置位置控制器

```rust
// src/robot/controller/builtin_controller.rs

/// 使用机器人内置位置控制器的关节控制器
pub struct BuiltinJointController<'a> {
    piper: &'a Piper,
    rest_position: Option<[f64; 6]>,
    started: bool,
}

impl<'a> BuiltinJointController<'a> {
    pub fn new(piper: &'a Piper, rest_position: Option<[f64; 6]>) -> Self {
        Self {
            piper,
            rest_position,
            started: false,
        }
    }
}

impl<'a> JointPositionController for BuiltinJointController<'a> {
    fn start(&mut self) -> Result<(), RobotError> {
        if self.started {
            return Ok(());
        }

        self.piper.set_arm_mode(
            ArmController::PositionVelocity,
            MoveMode::Joint,
        )?;
        std::thread::sleep(Duration::from_millis(100));

        self.started = true;
        Ok(())
    }

    fn stop(&mut self) -> Result<(), RobotError> {
        if !self.started {
            return Ok(());
        }

        // 如需回到休息位置，应显式调用 move_to_rest()
        if let Some(rest_pos) = self.rest_position {
            let _ = self.move_to_position(&rest_pos, 0.01, Duration::from_secs(3));
        }

        self.started = false;
        Ok(())
    }

    fn command_joints(&mut self, target: &[f64; 6]) -> Result<(), RobotError> {
        self.piper.command_joint_positions(target)
    }

    fn move_to_position(
        &mut self,
        target: &[f64; 6],
        threshold: f64,
        timeout: Duration,
    ) -> Result<bool, RobotError> {
        self.piper.move_to_position_blocking(target, threshold, timeout)
    }
}

impl<'a> Drop for BuiltinJointController<'a> {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}
```

---

## 🎨 使用示例对比

### Python piper_control 风格

```python
# Python
piper = piper_interface.PiperInterface("can0")
piper.set_installation_pos(ArmInstallationPos.UPRIGHT)

# 阻塞式重置
piper_init.reset_arm(
    piper,
    arm_controller=ArmController.MIT,
    move_mode=MoveMode.MIT,
)

# Controller 模式（上下文管理器）
with piper_control.MitJointPositionController(
    piper,
    kp_gains=5.0,
    kd_gains=0.8,
    rest_position=(0.0, 0.0, 0.0, 0.02, 0.5, 0.0),
) as controller:
    success = controller.move_to_position(
        [0.5, 0.7, -0.4, 0.2, 0.3, 0.5],
        threshold=0.01,
        timeout=5.0,
    )
    print(f"reached: {success}")

# 自动返回休息位置并停止
```

### Rust 对应风格（方案 1：显式 Drop）

```rust
// Rust - 显式作用域
use piper_sdk::robot::{PiperBuilder, MitJointController, ArmInstallationPos};
use std::time::Duration;

let piper = PiperBuilder::new()
    .interface("can0")
    .build()?;

piper.set_installation_pos(ArmInstallationPos::Upright)?;

// 阻塞式重置
piper.reset_arm_blocking(
    ArmController::Mit,
    MoveMode::Mit,
    Duration::from_secs(10),
)?;

// Controller 模式（RAII 自动清理）
{
    let mut controller = MitJointController::builder(&piper)
        .kp_gains([5.0; 6])
        .kd_gains([0.8; 6])
        .rest_position([0.0, 0.0, 0.0, 0.02, 0.5, 0.0])
        .build()?;

    let success = controller.move_to_position(
        &[0.5, 0.7, -0.4, 0.2, 0.3, 0.5],
        0.01,
        Duration::from_secs(5),
    )?;
    println!("reached: {}", success);

    // Drop 自动触发：返回休息位置 + 放松关节
}
```

### Rust 对应风格（方案 2：显式 stop）

```rust
// Rust - 显式 stop
let mut controller = MitJointController::builder(&piper)
    .kp_gains([5.0; 6])
    .kd_gains([0.8; 6])
    .rest_position([0.0, 0.0, 0.0, 0.02, 0.5, 0.0])
    .build()?;

controller.move_to_position(
    &[0.5, 0.7, -0.4, 0.2, 0.3, 0.5],
    0.01,
    Duration::from_secs(5),
)?;

// 显式停止（也可以等待 Drop）
controller.stop()?;
```

---

## 🔄 重力补偿示例对比

### Python 版本

```python
# Python gravity compensation
grav_model = GravityCompensationModel(model_type=ModelType.DIRECT)
piper = piper_interface.PiperInterface("can0")

piper_init.reset_arm(piper, ArmController.MIT, MoveMode.MIT)

controller = piper_control.MitJointPositionController(
    piper,
    kp_gains=[5.0, 5.0, 5.0, 5.6, 20.0, 6.0],
    kd_gains=0.8,
)

try:
    while True:
        qpos = piper.get_joint_positions()
        qvel = np.array(piper.get_joint_velocities())

        hover_torque = grav_model.predict(qpos)
        stability_torque = -qvel * 1.0
        applied_torque = hover_torque + stability_torque

        controller.command_torques(applied_torque)
        time.sleep(0.005)
finally:
    controller.stop()
    piper_init.disable_arm(piper)
```

### Rust 版本（建议）

```rust
// Rust gravity compensation
use piper_sdk::robot::{PiperBuilder, MitJointController};
use piper_sdk::gravity_compensation::GravityCompensationModel;
use std::time::Duration;

let piper = PiperBuilder::new()
    .interface("can0")
    .build()?;

piper.reset_arm_blocking(
    ArmController::Mit,
    MoveMode::Mit,
    Duration::from_secs(10),
)?;

let grav_model = GravityCompensationModel::new(ModelType::Direct)?;

let mut controller = MitJointController::builder(&piper)
    .kp_gains([5.0, 5.0, 5.0, 5.6, 20.0, 6.0])
    .kd_gains([0.8; 6])
    .build()?;

loop {
    let state = piper.get_joint_state();

    let hover_torque = grav_model.predict(&state.positions);
    let stability_torque: [f64; 6] = state.velocities
        .iter()
        .map(|&v| -v * 1.0)
        .collect::<Vec<_>>()
        .try_into()
        .unwrap();

    let applied_torque: [f64; 6] = hover_torque
        .iter()
        .zip(stability_torque.iter())
        .map(|(h, s)| h + s)
        .collect::<Vec<_>>()
        .try_into()
        .unwrap();

    controller.command_torques(&applied_torque)?;

    std::thread::sleep(Duration::from_millis(5));
}

// Drop 自动清理
```

---

## 🔍 关键设计决策

### 决策 1: 阻塞 vs 异步

**Python 方案**: 全部使用阻塞 + 手动 `time.sleep()`

**Rust 方案**:
- **Layer 1**: 提供非阻塞方法（`enable_arm()`, `emergency_stop()`）
- **Layer 2**: 提供阻塞方法（`enable_arm_blocking()`, `move_to_position_blocking()`）
- **未来**: 可选的异步 API（`enable_arm_async()` 返回 `Future`）

**理由**:
- 大多数用户需要简单的阻塞 API
- 高级用户可以用非阻塞 API 自己实现异步逻辑
- 未来可以添加 `async` feature gate

### 决策 2: Controller 生命周期管理

**Python 方案**: 上下文管理器 (`with` 语句)

**Rust 方案**: RAII + Drop trait

**理由**:
- Rust 的 Drop trait 提供确定性析构
- 用户无需记得调用 `stop()`
- 更符合 Rust 习惯

### 决策 3: Builder 模式 vs 构造函数参数

**Python 方案**: 构造函数接受大量参数

**Rust 方案**: Builder 模式

**理由**:
- Rust 没有默认参数，Builder 更清晰
- 链式调用风格更优雅
- 可选参数更易管理

### 决策 4: 单位转换

**Python 方案**: 在 `PiperInterface` 层自动转换所有单位

**Rust 方案**:
- Layer 0 保留原始单位（为了性能）
- Layer 1+ 统一使用 SI 单位

**理由**:
- 避免不必要的转换开销
- 高层 API 用户体验更好
- 清晰的分层边界

### 决策 5: 错误处理

**Python 方案**: 异常 + 日志

**Rust 方案**: Result + 详细错误类型

```rust
#[derive(Debug, thiserror::Error)]
pub enum RobotError {
    #[error("Timeout: {0}")]
    Timeout(String),

    #[error("Invalid parameter: {0}")]
    InvalidParameter(String),

    #[error("Arm not enabled")]
    ArmNotEnabled,

    #[error("Communication error: {0}")]
    Communication(#[from] CanError),

    // ... 更多错误类型
}
```

---

## 📋 实现计划

### Phase 1: Layer 1 核心方法（P0）- 1 周

**目标**: 让 gravity compensation example 能够运行

1. ✅ 实现 `Piper::emergency_stop()` 和 `resume_from_emergency_stop()`
2. ✅ 实现 `Piper::set_motor_enable()`, `enable_arm()`, `disable_arm()`
3. ✅ 实现 `Piper::enable_mit_mode()`
4. ✅ 实现 `Piper::send_joint_mit_control()` 和 `send_joint_mit_control_realtime()`
5. ✅ 实现 `Piper::get_joint_state()`
6. ✅ 实现 `Piper::is_arm_enabled()`
7. ✅ 添加 `PiperArmType`, `ArmInstallationPos` 等类型
8. ✅ 在 `PiperBuilder` 中存储 `interface_name` 和 `arm_type`
9. ✅ 更新文档和示例

**预期成果**:
```rust
// 能够运行的简单示例
let piper = PiperBuilder::new().build()?;
piper.enable_arm()?;
piper.enable_mit_mode(true)?;
for i in 1..=6 {
    piper.send_joint_mit_control(i, 0.0, 0.0, 0.0, 0.0, 1.5)?;
}
```

---

### Phase 2: Layer 2 阻塞式方法（P1）- 1 周

**目标**: 提供用户友好的阻塞式 API

1. ✅ 实现 `enable_arm_blocking()`
2. ✅ 实现 `disable_arm_blocking()`
3. ✅ 实现 `reset_arm_blocking()`
4. ✅ 实现 `move_to_position_blocking()`
5. ✅ 实现 `set_arm_mode()`
6. ✅ 添加 `ArmController`, `MoveMode` 枚举
7. ✅ 添加重试和超时逻辑
8. ✅ 完善错误处理
9. ✅ 编写集成测试

**预期成果**:
```rust
// 用户友好的阻塞式 API
let piper = PiperBuilder::new().build()?;
piper.enable_arm_blocking(Duration::from_secs(10))?;
piper.set_arm_mode(ArmController::PositionVelocity, MoveMode::Joint)?;
piper.move_to_position_blocking(&target, 0.01, Duration::from_secs(5))?;
```

---

### Phase 3: Layer 3 Controller 模式（P1）- 2 周

**目标**: 提供高层 Controller 抽象

1. ✅ 设计 `JointPositionController` trait
2. ✅ 实现 `MitJointController` + Builder
3. ✅ 实现 `BuiltinJointController`
4. ✅ 实现 `GripperController`
5. ✅ 实现 Drop trait 自动清理
6. ✅ 实现 `relax_joints()` 逐渐停止
7. ✅ 添加固件版本检测和关节翻转映射
8. ✅ 编写完整的 gravity compensation example
9. ✅ 编写文档和教程

**预期成果**:
```rust
// 完整的 Controller 模式
let mut controller = MitJointController::builder(&piper)
    .kp_gains([5.0; 6])
    .kd_gains([0.8; 6])
    .rest_position(rest_positions::UPRIGHT)
    .build()?;

controller.move_to_position(&target, 0.01, Duration::from_secs(5))?;
controller.command_torques(&torques)?;

// Drop 自动清理
```

---

### Phase 4: 辅助功能和优化（P2-P3）- 1-2 周

**目标**: 完善细节和优化

1. ✅ 实现 `show_status()` 人类可读的状态显示
2. ✅ 实现 CAN 接口自动发现（类似 `piper_connect.py`）
3. ✅ 添加碰撞检测配置
4. ✅ 添加夹爪控制
5. ✅ 性能优化（利用 realtime 模式）
6. ✅ 添加日志和 tracing
7. ✅ 完善测试覆盖
8. ✅ 编写 cookbook 和 FAQ

---

## 🎓 API 设计原则总结

### 1. 分层清晰
- Layer 0: 底层 protocol（现有）
- Layer 1: 薄封装（单行调用）
- Layer 2: 阻塞式辅助（自动重试）
- Layer 3: Controller 模式（生命周期管理）

### 2. 用户友好
- 统一的 SI 单位（弧度、米、秒）
- 自动参数验证和裁剪
- 阻塞式方法自动等待完成
- RAII 自动清理资源

### 3. 类型安全
- 枚举代替魔数
- Result 类型明确错误
- Builder 模式管理可选参数
- Trait 抽象提供扩展性

### 4. 性能优先
- 零成本抽象
- 提供 realtime 模式
- 无锁状态读取
- 批量操作支持

### 5. Rust 习惯
- RAII 生命周期管理
- Drop trait 自动清理
- Builder 模式
- Trait-based 扩展

---

## 📊 Python vs Rust API 对比表

| 功能 | Python piper_control | Rust SDK (设计) | 优势 |
|------|---------------------|----------------|------|
| 初始化 | `PiperInterface(can_port)` | `PiperBuilder::new().build()` | Rust: 更灵活的配置 |
| 使能等待 | `piper_init.enable_arm()` 自动重试 | `enable_arm_blocking()` 自动重试 | 相同 |
| 上下文管理 | `with Controller(...) as c:` | `{ let c = Controller::builder().build()?; }` + Drop | Rust: 确定性析构 |
| 单位转换 | 自动（在 Interface 层） | Layer 1+ 自动 | 相同 |
| 参数验证 | 运行时裁剪 | 运行时裁剪 + 编译时类型检查 | Rust: 更安全 |
| 错误处理 | 异常 | Result | Rust: 显式错误 |
| 固件兼容 | 运行时版本检查 | 运行时版本检查 | 相同 |
| 并发控制 | GIL 限制 | 真正的并发 | Rust: 更好性能 |
| 力矩控制 | `command_torques([f64; 6])` | `command_torques(&[f64; 6])` | 相同 API |
| 位置控制 | `move_to_position(target)` | `move_to_position(&target)` | 相同 API |

---

## 🔮 未来扩展方向

### 1. 异步 API（Feature Gate）

```rust
#[cfg(feature = "async")]
impl Piper {
    pub async fn enable_arm_async(&self, timeout: Duration) -> Result<(), RobotError> {
        // tokio::time::timeout + 异步重试逻辑
    }
}
```

### 2. 类型状态机（编译时保证操作顺序）

```rust
// 类型状态模式
pub struct Piper<State> {
    inner: PiperInner,
    _state: PhantomData<State>,
}

pub struct Disabled;
pub struct Enabled;

impl Piper<Disabled> {
    pub fn enable(self) -> Result<Piper<Enabled>, RobotError> { ... }
}

impl Piper<Enabled> {
    pub fn send_mit_control(&self, ...) -> Result<(), RobotError> { ... }
    pub fn disable(self) -> Result<Piper<Disabled>, RobotError> { ... }
}
```

### 3. 实时任务抽象

```rust
pub trait RealtimeTask {
    fn init(&mut self, piper: &Piper) -> Result<(), RobotError>;
    fn update(&mut self, piper: &Piper, dt: f64) -> Result<(), RobotError>;
    fn cleanup(&mut self, piper: &Piper) -> Result<(), RobotError>;
}

impl Piper {
    pub fn run_realtime_task<T: RealtimeTask>(
        &self,
        task: &mut T,
        frequency: f64,
    ) -> Result<(), RobotError> {
        // 实时控制循环，自动处理定时
    }
}
```

### 4. 轨迹规划

```rust
pub struct TrajectoryPlanner {
    // 三次样条、梯形速度规划等
}

impl TrajectoryPlanner {
    pub fn plan_joint_trajectory(
        &self,
        start: &[f64; 6],
        end: &[f64; 6],
        duration: Duration,
    ) -> Trajectory;
}
```

### 5. 状态机抽象

```rust
pub trait RobotState {
    fn on_enter(&mut self, piper: &Piper) -> Result<(), RobotError>;
    fn update(&mut self, piper: &Piper) -> Result<Option<Box<dyn RobotState>>, RobotError>;
    fn on_exit(&mut self, piper: &Piper) -> Result<(), RobotError>;
}
```

---

## ✅ 总结

### 核心设计哲学

1. **从 Python 学习用户体验**：
   - 阻塞式等待
   - 自动清理
   - 统一单位
   - 参数验证

2. **用 Rust 实现更好的安全性**：
   - RAII 生命周期
   - Result 错误处理
   - 类型状态机
   - 零成本抽象

3. **保持灵活性**：
   - 三层架构，用户可选择任何层次
   - Trait-based 扩展
   - Feature gates 控制功能

### 下一步行动

1. ✅ **Phase 1**: 实现 Layer 1 核心方法（1 周）
2. ✅ **Phase 2**: 实现 Layer 2 阻塞式方法（1 周）
3. ✅ **Phase 3**: 实现 Layer 3 Controller 模式（2 周）
4. ✅ **Phase 4**: 优化和完善（1-2 周）

**总工作量**: 约 5-6 周，1500-2000 行代码

---

**报告生成日期**: 2026-01-23
**报告作者**: AI Assistant
**版本**: v2.0
