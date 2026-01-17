# Driver 模块实现方案

## 1. 概述

Driver 模块是 SDK 的核心业务逻辑层，负责：
- **IO 线程管理**：后台线程处理 CAN 通讯，避免阻塞控制循环
- **状态同步**：使用 ArcSwap 实现无锁状态共享，支持 500Hz 高频读取
- **帧解析与聚合**：将多个 CAN 帧聚合为完整的状态快照（Frame Commit + Buffered Commit 机制）
- **时间戳管理**：按时间同步性拆分状态，解决不同 CAN 帧时间戳不同步的问题
- **对外 API**：提供简洁的 `Piper` 结构体，封装底层细节

**核心设计理念**（方案 4+）：
- **按时间同步性拆分**：`CoreMotionState`（帧组同步）、`JointDynamicState`（独立帧 + Buffered Commit）
- **Buffered Commit 机制**：收集 6 个关节的速度帧，集齐或超时后一次性原子提交，避免状态撕裂
- **时间对齐 API**：提供 `get_aligned_motion()`，确保位置和速度数据的时间戳差异在可接受范围内

## 2. 模块结构

```
src/driver/
├── mod.rs              # 模块导出
├── state.rs            # 状态结构定义（MotionState, DiagnosticState, ConfigState）
├── pipeline.rs         # IO 线程循环和 Frame Commit 逻辑
├── robot.rs            # 对外 API（Piper 结构体）
└── builder.rs          # Builder 模式（PiperBuilder）
```

## 3. 状态结构设计（方案 4+：混合方案 + Buffered Commit Strategy）

根据时间戳同步性分析报告（`timestamp_synchronization_analysis.md`），采用**方案 4+**，按时间同步性拆分状态，解决不同 CAN 帧时间戳不同步的问题。

### 3.1. 核心运动状态（热数据 - 500Hz，帧组同步）

用于高频力控循环，必须使用 `ArcSwap` 实现无锁读取。

**来源**：关节位置来自 0x2A5-0x2A7（3 帧组），末端位姿来自 0x2A2-0x2A4（3 帧组）。这些帧组内部是同步的（微秒级延迟）。

```rust
// src/driver/state.rs

use arc_swap::ArcSwap;
use std::sync::{Arc, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

/// 核心运动状态（帧组同步）
///
/// 更新频率：500Hz
/// 大小：< 200 字节，Clone 开销低
/// 同步机制：Frame Commit（收到完整帧组后原子更新）
/// 时间同步性：帧组内的字段是同步的（微秒级延迟）
#[derive(Debug, Clone, Default)]
pub struct CoreMotionState {
    /// 时间戳（微秒）
    ///
    /// **注意**：存储的是硬件时间戳（来自 `PiperFrame.timestamp_us`），不是 UNIX 时间戳。
    /// 硬件时间戳是设备相对时间，用于帧间时间差计算，不能直接与系统时间戳比较。
    pub timestamp_us: u64,

    // === 关节位置（来自 0x2A5-0x2A7，帧组同步） ===
    /// 关节位置（弧度）[J1, J2, J3, J4, J5, J6]
    pub joint_pos: [f64; 6],

    // === 末端位姿（来自 0x2A2-0x2A4，帧组同步） ===
    /// 末端位姿 [X, Y, Z, Rx, Ry, Rz]
    /// - X, Y, Z: 位置（米）
    ///   - **注意**：`EndPoseFeedback1.x()`, `.y()`, `EndPoseFeedback2.z()` 返回的是**毫米**，需要除以 1000.0 转换为米
    /// - Rx, Ry, Rz: 姿态角（弧度，欧拉角或旋转向量）
    pub end_pose: [f64; 6],
}
```

### 3.2. 关节动态状态（热数据 - 500Hz，独立帧 + Buffered Commit）

关节速度和电流来自 0x251-0x256（6 个独立帧），需要 Buffered Commit 机制保证原子性。

```rust
/// 关节动态状态（独立帧，但通过缓冲提交保证一致性）
///
/// 更新频率：500Hz
/// 大小：< 150 字节，Clone 开销低
/// 同步机制：Buffered Commit（收集 6 个关节的速度帧，集齐或超时后一次性提交）
/// 时间同步性：通过 Group Commit 机制，保证 6 个关节数据来自同一 CAN 传输周期
#[derive(Debug, Clone, Default)]
pub struct JointDynamicState {
    /// 整个组的大致时间戳（最新一帧的时间，微秒）
    ///
    /// **注意**：存储的是硬件时间戳（来自 `PiperFrame.timestamp_us`），不是 UNIX 时间戳。
    /// 硬件时间戳是设备相对时间，用于帧间时间差计算，不能直接与系统时间戳比较。
    pub group_timestamp_us: u64,

    // === 关节速度/电流（来自 0x251-0x256，独立帧） ===
    /// 关节速度（rad/s）[J1, J2, J3, J4, J5, J6]
    pub joint_vel: [f64; 6],
    /// 关节电流（A）[J1, J2, J3, J4, J5, J6]
    pub joint_current: [f64; 6],

    /// 每个关节的具体更新时间（用于调试或高阶插值）
    pub timestamps: [u64; 6],

    /// 有效性掩码（Bit 0-5 对应 Joint 1-6）
    /// - 1 表示本周期内已更新
    /// - 0 表示未更新（可能是丢帧）
    pub valid_mask: u8,
}

impl JointDynamicState {
    /// 检查所有关节是否都已更新（`valid_mask == 0x3F`）
    pub fn is_complete(&self) -> bool {
        self.valid_mask == 0b111111
    }

    /// 获取未更新的关节索引（用于调试）
    pub fn missing_joints(&self) -> Vec<usize> {
        (0..6).filter(|&i| (self.valid_mask & (1 << i)) == 0).collect()
    }
}
```

**关键设计点**：
1. **Buffered Commit 机制**：收集 6 个关节的速度帧，集齐（`mask == 0x3F`）或超时（1.2ms）后一次性原子提交
2. **有效性标记**：通过 `valid_mask` 标记哪些关节已更新，哪些可能丢失
3. **细粒度时间戳**：`timestamps` 数组记录每个关节的更新时间，支持调试和插值

### 3.3. 控制状态（温数据 - 100Hz）

控制状态来自 0x2A1（RobotStatusFeedback）和 0x2A8（GripperFeedback），更新频率较低。

```rust
/// 控制状态（温数据）
///
/// 更新频率：100Hz 或更低
/// 同步机制：ArcSwap（读取频率中等，但需要原子性）
/// 时间同步性：来自单个 CAN 帧，内部字段同步
#[derive(Debug, Clone, Default)]
pub struct ControlStatusState {
    /// 时间戳（微秒）
    ///
    /// **注意**：存储的是硬件时间戳（来自 `PiperFrame.timestamp_us`），不是 UNIX 时间戳。
    /// 硬件时间戳是设备相对时间，用于帧间时间差计算，不能直接与系统时间戳比较。
    pub timestamp_us: u64,

    // === 控制状态（来自 0x2A1） ===
    /// 控制模式
    pub control_mode: u8,
    /// 机器人状态
    pub robot_status: u8,
    /// MOVE 模式
    pub move_mode: u8,
    /// 示教状态
    pub teach_status: u8,
    /// 运动状态
    pub motion_status: u8,
    /// 轨迹点索引
    pub trajectory_point_index: u8,
    /// 故障码：角度超限位 [J1, J2, J3, J4, J5, J6]
    pub fault_angle_limit: [bool; 6],
    /// 故障码：通信异常 [J1, J2, J3, J4, J5, J6]
    pub fault_comm_error: [bool; 6],
    /// 使能状态（从 robot_status 推导）
    pub is_enabled: bool,

    // === 夹爪状态（来自 0x2A8） ===
    /// 夹爪行程（mm）
    pub gripper_travel: f64,
    /// 夹爪扭矩（N·m）
    pub gripper_torque: f64,
}
```

### 3.4. DiagnosticState（冷数据 - 10Hz）

用于监控和诊断，使用 `RwLock` 即可（读取频率低）。

```rust
/// 诊断状态（冷数据）
///
/// 更新频率：10Hz 或更低
/// 同步机制：RwLock（读写分离，减少锁竞争）
/// 时间同步性：来自低速反馈帧（0x261-0x266），各关节独立更新
#[derive(Debug, Clone, Default)]
pub struct DiagnosticState {
    /// 时间戳（微秒）
    ///
    /// **注意**：存储的是硬件时间戳（来自 `PiperFrame.timestamp_us`），不是 UNIX 时间戳。
    /// 硬件时间戳是设备相对时间，用于帧间时间差计算，不能直接与系统时间戳比较。
    pub timestamp_us: u64,

    // === 温度（来自 0x261-0x266） ===
    /// 电机温度（°C）[J1, J2, J3, J4, J5, J6]
    pub motor_temps: [f32; 6],
    /// 驱动器温度（°C）[J1, J2, J3, J4, J5, J6]
    pub driver_temps: [f32; 6],

    // === 电压/电流（来自 0x261-0x266） ===
    /// 各关节电压（V）[J1, J2, J3, J4, J5, J6]
    pub joint_voltage: [f32; 6],
    /// 各关节母线电流（A）[J1, J2, J3, J4, J5, J6]
    pub joint_bus_current: [f32; 6],

    // === 保护状态（来自 0x47B） ===
    /// 各关节碰撞保护等级（0-8）[J1, J2, J3, J4, J5, J6]
    pub protection_levels: [u8; 6],

    // === 驱动器状态（来自 0x261-0x266） ===
    /// 驱动器状态：电压过低 [J1, J2, J3, J4, J5, J6]
    pub driver_voltage_low: [bool; 6],
    /// 驱动器状态：电机过温 [J1, J2, J3, J4, J5, J6]
    pub driver_motor_over_temp: [bool; 6],
    /// 驱动器状态：过流 [J1, J2, J3, J4, J5, J6]
    pub driver_over_current: [bool; 6],
    /// 驱动器状态：驱动器过温 [J1, J2, J3, J4, J5, J6]
    pub driver_over_temp: [bool; 6],
    /// 驱动器状态：碰撞保护触发 [J1, J2, J3, J4, J5, J6]
    pub driver_collision_protection: [bool; 6],
    /// 驱动器状态：驱动器错误 [J1, J2, J3, J4, J5, J6]
    pub driver_error: [bool; 6],
    /// 驱动器状态：使能状态 [J1, J2, J3, J4, J5, J6]
    pub driver_enabled: [bool; 6],
    /// 驱动器状态：堵转保护触发 [J1, J2, J3, J4, J5, J6]
    pub driver_stall_protection: [bool; 6],

    // === 夹爪状态（来自 0x2A8） ===
    /// 夹爪：电压过低
    pub gripper_voltage_low: bool,
    /// 夹爪：电机过温
    pub gripper_motor_over_temp: bool,
    /// 夹爪：过流
    pub gripper_over_current: bool,
    /// 夹爪：驱动器过温
    pub gripper_over_temp: bool,
    /// 夹爪：传感器异常
    pub gripper_sensor_error: bool,
    /// 夹爪：驱动器错误
    pub gripper_driver_error: bool,
    /// 夹爪：使能状态（注意：反向逻辑）
    pub gripper_enabled: bool,
    /// 夹爪：回零状态
    pub gripper_homed: bool,

    // === 连接状态 ===
    /// 连接状态（是否收到数据）
    pub connection_status: bool,
}
```

### 3.5. ConfigState（冷数据 - 仅初始化时更新）

配置参数，几乎只读。来自配置反馈帧（按需查询），不是周期性反馈。

```rust
/// 配置状态（冷数据）
///
/// 更新频率：仅初始化或手动更新时
/// 同步机制：RwLock
/// 时间同步性：来自配置反馈帧（按需查询），时间戳不重要
#[derive(Debug, Clone, Default)]
pub struct ConfigState {
    /// 固件版本号（无法从协议获取，可选）
    pub firmware_version: Option<String>,

    // === 关节限制（来自 0x473，需要查询 6 次） ===
    /// 关节角度上限（弧度）[J1, J2, J3, J4, J5, J6]
    ///
    /// **注意**：`MotorLimitFeedback.max_angle()` 返回的是**度**，需要转换为弧度。
    pub joint_limits_max: [f64; 6],
    /// 关节角度下限（弧度）[J1, J2, J3, J4, J5, J6]
    ///
    /// **注意**：`MotorLimitFeedback.min_angle()` 返回的是**度**，需要转换为弧度。
    pub joint_limits_min: [f64; 6],
    /// 各关节最大速度（rad/s）[J1, J2, J3, J4, J5, J6]
    pub joint_max_velocity: [f64; 6],
    /// 各关节最大加速度（rad/s²）[J1, J2, J3, J4, J5, J6]
    ///
    /// **注意**：来自 `MotorMaxAccelFeedback` (0x47C)，需要查询 6 次（每个关节一次）。
    pub max_acc_limits: [f64; 6],

    // === 末端限制（来自 0x478） ===
    /// 末端最大线速度（m/s）
    pub max_end_linear_velocity: f64,
    /// 末端最大角速度（rad/s）
    pub max_end_angular_velocity: f64,
    /// 末端最大线加速度（m/s²）
    pub max_end_linear_accel: f64,
    /// 末端最大角加速度（rad/s²）
    pub max_end_angular_accel: f64,
}
```

### 3.6. PiperContext（总上下文）

聚合所有状态，按热/温/冷数据分类，使用不同的同步机制。

```rust
use std::sync::RwLock;

/// Piper 上下文（所有状态的聚合）
pub struct PiperContext {
    // === 热数据（500Hz，高频运动数据）===
    // 使用 ArcSwap，无锁读取，适合高频控制循环
    /// 核心运动状态（帧组同步：关节位置 + 末端位姿）
    pub core_motion: Arc<ArcSwap<CoreMotionState>>,
    /// 关节动态状态（独立帧 + Buffered Commit：关节速度 + 电流）
    pub joint_dynamic: Arc<ArcSwap<JointDynamicState>>,

    // === 温数据（100Hz，控制状态）===
    // 使用 ArcSwap，更新频率中等，但需要原子性
    /// 控制状态（单个 CAN 帧：控制模式、机器人状态、夹爪状态）
    pub control_status: Arc<ArcSwap<ControlStatusState>>,

    // === 冷数据（10Hz 或按需，诊断和配置）===
    // 使用 RwLock，读取频率低，避免内存分配
    /// 诊断状态（低速反馈帧：温度、电压、电流、状态）
    pub diagnostics: Arc<RwLock<DiagnosticState>>,
    /// 配置状态（配置反馈帧：限制参数）
    pub config: Arc<RwLock<ConfigState>>,
}

impl PiperContext {
    /// 创建新的上下文
    pub fn new() -> Self {
        Self {
            // 热数据：ArcSwap，无锁读取
            core_motion: Arc::new(ArcSwap::from_pointee(CoreMotionState::default())),
            joint_dynamic: Arc::new(ArcSwap::from_pointee(JointDynamicState::default())),

            // 温数据：ArcSwap
            control_status: Arc::new(ArcSwap::from_pointee(ControlStatusState::default())),

            // 冷数据：RwLock
            diagnostics: Arc::new(RwLock::new(DiagnosticState::default())),
            config: Arc::new(RwLock::new(ConfigState::default())),
        }
    }
}
```

## 4. Pipeline IO 循环设计

### 4.1. Frame Commit 机制

**问题**：如果一帧完整状态由多个 CAN 帧组成（如 0x2A5, 0x2A6, 0x2A7 各包含 2 个关节），
每个 CAN 帧单独更新 `ArcSwap` 会导致读取线程看到"撕裂"数据（J1 是新的，J6 是旧的）。

**解决方案**：Cache & Commit 模式
- 在 IO 线程中维护一个线程局部的 `pending_state`
- 收到 CAN 帧时更新 `pending_state`，但不立即提交
- 只有当收到**完整帧组**的最后一帧时，才原子地提交整个状态

### 4.2. Pipeline 结构

```rust
// src/driver/pipeline.rs

use crate::can::{CanAdapter, PiperFrame, CanError};
use crate::protocol::feedback::*;
use crate::protocol::ids::*;
use crate::driver::state::*;
use crossbeam_channel::{Receiver, Sender};
use std::sync::Arc;
use std::time::Duration;
use tracing::{error, warn, debug, trace};

/// Pipeline 配置
pub struct PipelineConfig {
    /// CAN 接收超时（毫秒）
    pub receive_timeout_ms: u64,
    /// 帧组超时（毫秒）
    /// 如果收到部分帧后，超过此时间未收到完整帧组，则丢弃缓存
    pub frame_group_timeout_ms: u64,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            receive_timeout_ms: 2,
            frame_group_timeout_ms: 10,
        }
    }
}

/// IO 线程循环
///
/// # 参数
/// - `can`: CAN 适配器（可变借用，但会在循环中独占）
/// - `cmd_rx`: 命令接收通道（从控制线程接收控制帧）
/// - `ctx`: 共享状态上下文
/// - `config`: Pipeline 配置
pub fn io_loop(
    mut can: impl CanAdapter,
    cmd_rx: Receiver<PiperFrame>,
    ctx: Arc<PiperContext>,
    config: PipelineConfig,
) {
    // === 核心运动状态：帧组同步 ===
    // 为 joint_pos 和 end_pose 分别维护独立的 pending 状态，避免帧组交错导致的状态撕裂
    let mut pending_joint_pos: [f64; 6] = [0.0; 6];
    let mut pending_end_pose: [f64; 6] = [0.0; 6];
    let mut joint_pos_ready = false;  // 关节位置帧组是否完整
    let mut end_pose_ready = false;   // 末端位姿帧组是否完整

    // === 关节动态状态：缓冲提交（关键改进） ===
    let mut pending_joint_dynamic = JointDynamicState::default();
    let mut vel_update_mask: u8 = 0;        // 位掩码：已收到的关节（Bit 0-5 对应 Joint 1-6）
    let mut last_vel_commit_time_us: u32 = 0;  // 上次速度帧提交时间（硬件时间戳，用于判断提交）
    let mut last_vel_packet_time_us: u32 = 0;  // 上次速度帧到达时间（硬件时间戳，用于判断提交）
    let mut last_vel_packet_instant = None::<std::time::Instant>;  // 上次速度帧到达时间（系统时间，用于超时检查）

    // 注意：receive_timeout 当前未使用，因为 CanAdapter::receive() 的超时是在适配器内部处理的
    // 如果需要未来扩展（例如动态调整接收超时），可以使用 config.receive_timeout_ms
    let frame_group_timeout = Duration::from_millis(config.frame_group_timeout_ms);
    let mut last_frame_time = std::time::Instant::now();

    loop {
        // ============================================================
        // 1. 接收 CAN 帧（带超时，避免阻塞）
        // ============================================================
        let frame = match can.receive() {
            Ok(frame) => frame,
            Err(CanError::Timeout) => {
                // 超时是正常情况，检查各个 pending 状态的年龄

                // === 检查关节位置/末端位姿帧组超时 ===
                // 使用系统时间 Instant，因为它们不依赖硬件时间戳
                let elapsed = last_frame_time.elapsed();
                if elapsed > frame_group_timeout {
                    // 重置核心运动状态的 pending 缓存（避免数据过期）
                    pending_joint_pos = [0.0; 6];
                    pending_end_pose = [0.0; 6];
                    joint_pos_ready = false;
                    end_pose_ready = false;
                }

                // === 检查速度帧缓冲区超时（关键：避免僵尸缓冲区） ===
                // 使用系统时间 Instant 检查，因为硬件时间戳和系统时间戳不能直接比较
                // 如果缓冲区不为空，且距离上次速度帧到达已经超时，强制提交或丢弃
                if vel_update_mask != 0 {
                    if let Some(last_vel_instant) = last_vel_packet_instant {
                        let elapsed_since_last_vel = last_vel_instant.elapsed();
                        let vel_timeout_threshold = Duration::from_micros(2000);  // 2ms 超时（防止僵尸数据）

                        if elapsed_since_last_vel > vel_timeout_threshold {
                            // 超时：强制提交不完整的数据（设置 valid_mask 标记不完整）
                            warn!(
                                "Velocity buffer timeout: mask={:06b}, forcing commit with incomplete data",
                                vel_update_mask
                            );
                            // 注意：这里使用上次记录的硬件时间戳（如果为 0，说明没有收到过，此时不应该提交）
                            if last_vel_packet_time_us > 0 {
                                pending_joint_dynamic.group_timestamp_us = last_vel_packet_time_us as u64;
                                pending_joint_dynamic.valid_mask = vel_update_mask;
                                ctx.joint_dynamic.store(Arc::new(pending_joint_dynamic.clone()));

                                // 重置状态
                                vel_update_mask = 0;
                                last_vel_commit_time_us = last_vel_packet_time_us;
                                last_vel_packet_instant = None;
                            } else {
                                // 如果时间戳为 0，说明没有收到过有效帧，直接丢弃
                                vel_update_mask = 0;
                                last_vel_packet_instant = None;
                            }
                        }
                    }
                }

                // 继续循环，检查命令通道
                continue;
            }
            Err(e) => {
                error!("CAN receive error: {}", e);
                // 继续循环，尝试恢复
                continue;
            }
        };

        last_frame_time = std::time::Instant::now();

        // ============================================================
        // 2. 根据 CAN ID 解析帧并更新状态
        // ============================================================
        match frame.id {
            // === 核心运动状态（帧组同步） ===

            // 关节反馈 12 (0x2A5)
            ID_JOINT_FEEDBACK_12 => {
                if let Ok(feedback) = JointFeedback12::try_from(frame) {
                    pending_joint_pos[0] = feedback.j1_rad();
                    pending_joint_pos[1] = feedback.j2_rad();
                    joint_pos_ready = false;  // 重置，等待完整帧组
                } else {
                    warn!("Failed to parse JointFeedback12: CAN ID 0x{:X}", frame.id);
                }
            }

            // 关节反馈 34 (0x2A6)
            ID_JOINT_FEEDBACK_34 => {
                if let Ok(feedback) = JointFeedback34::try_from(frame) {
                    pending_joint_pos[2] = feedback.j3_rad();
                    pending_joint_pos[3] = feedback.j4_rad();
                    joint_pos_ready = false;  // 重置，等待完整帧组
                } else {
                    warn!("Failed to parse JointFeedback34: CAN ID 0x{:X}", frame.id);
                }
            }

            // 关节反馈 56 (0x2A7) - 【Frame Commit】这是完整帧组的最后一帧
            ID_JOINT_FEEDBACK_56 => {
                if let Ok(feedback) = JointFeedback56::try_from(frame) {
                    pending_joint_pos[4] = feedback.j5_rad();
                    pending_joint_pos[5] = feedback.j6_rad();
                    joint_pos_ready = true;  // 标记关节位置帧组已完整

                    // 【Frame Commit】如果两个帧组都准备好，则提交完整状态
                    // 否则，从当前状态读取另一个字段，只更新关节位置
                    // 注意：硬件时间戳是 u32，但状态中使用 u64（与其他时间戳统一）
                    if end_pose_ready {
                        // 两个帧组都完整，提交完整状态
                        let new_state = CoreMotionState {
                            timestamp_us: frame.timestamp_us as u64,
                            joint_pos: pending_joint_pos,
                            end_pose: pending_end_pose,
                        };
                        ctx.core_motion.store(Arc::new(new_state));
                        trace!("Core motion committed: both joint_pos and end_pose updated");
                        // 重置标志，准备下一轮
                        joint_pos_ready = false;
                        end_pose_ready = false;
                    } else {
                        // 只有关节位置完整，从当前状态读取 end_pose 并更新
                        let current = ctx.core_motion.load();
                        let new_state = CoreMotionState {
                            timestamp_us: frame.timestamp_us as u64,
                            joint_pos: pending_joint_pos,
                            end_pose: current.end_pose,  // 保留当前值
                        };
                        ctx.core_motion.store(Arc::new(new_state));
                        trace!("Core motion committed: joint_pos updated (end_pose not ready)");
                    }
                } else {
                    warn!("Failed to parse JointFeedback56: CAN ID 0x{:X}", frame.id);
                }
            }

            // 末端位姿反馈 1 (0x2A2)
            ID_END_POSE_1 => {
                if let Ok(feedback) = EndPoseFeedback1::try_from(frame) {
                    pending_end_pose[0] = feedback.x() / 1000.0;  // mm → m
                    pending_end_pose[1] = feedback.y() / 1000.0;  // mm → m
                    end_pose_ready = false;  // 重置，等待完整帧组
                }
            }

            // 末端位姿反馈 2 (0x2A3)
            ID_END_POSE_2 => {
                if let Ok(feedback) = EndPoseFeedback2::try_from(frame) {
                    pending_end_pose[2] = feedback.z() / 1000.0;  // mm → m
                    pending_end_pose[3] = feedback.rx_rad();
                    end_pose_ready = false;  // 重置，等待完整帧组
                }
            }

            // 末端位姿反馈 3 (0x2A4) - 【Frame Commit】这是完整帧组的最后一帧
            ID_END_POSE_3 => {
                if let Ok(feedback) = EndPoseFeedback3::try_from(frame) {
                    pending_end_pose[4] = feedback.ry_rad();
                    pending_end_pose[5] = feedback.rz_rad();
                    end_pose_ready = true;  // 标记末端位姿帧组已完整

                    // 【Frame Commit】如果两个帧组都准备好，则提交完整状态
                    // 否则，从当前状态读取另一个字段，只更新末端位姿
                    // 注意：硬件时间戳是 u32，但状态中使用 u64（与其他时间戳统一）
                    if joint_pos_ready {
                        // 两个帧组都完整，提交完整状态
                        let new_state = CoreMotionState {
                            timestamp_us: frame.timestamp_us as u64,
                            joint_pos: pending_joint_pos,
                            end_pose: pending_end_pose,
                        };
                        ctx.core_motion.store(Arc::new(new_state));
                        trace!("Core motion committed: both joint_pos and end_pose updated");
                        // 重置标志，准备下一轮
                        joint_pos_ready = false;
                        end_pose_ready = false;
                    } else {
                        // 只有末端位姿完整，从当前状态读取 joint_pos 并更新
                        let current = ctx.core_motion.load();
                        let new_state = CoreMotionState {
                            timestamp_us: frame.timestamp_us as u64,
                            joint_pos: current.joint_pos,  // 保留当前值
                            end_pose: pending_end_pose,
                        };
                        ctx.core_motion.store(Arc::new(new_state));
                        trace!("Core motion committed: end_pose updated (joint_pos not ready)");
                    }
                }
            }

            // === 关节动态状态（缓冲提交策略 - 核心改进） ===
            id if id >= ID_JOINT_DRIVER_HIGH_SPEED_BASE && id <= ID_JOINT_DRIVER_HIGH_SPEED_BASE + 5 => {
                let joint_index = (id - ID_JOINT_DRIVER_HIGH_SPEED_BASE) as usize;

                if let Ok(feedback) = JointDriverHighSpeedFeedback::try_from(frame) {
                    // 1. 更新缓冲区（而不是立即提交）
                    pending_joint_dynamic.joint_vel[joint_index] = feedback.speed();
                    pending_joint_dynamic.joint_current[joint_index] = feedback.current();
                    // 注意：硬件时间戳是 u32，但状态中使用 u64（用于与其他时间戳比较）
                    pending_joint_dynamic.timestamps[joint_index] = frame.timestamp_us as u64;

                    // 2. 标记该关节已更新
                    vel_update_mask |= 1 << joint_index;
                    // 更新硬件时间戳和系统时间戳（用于不同场景的检查）
                    last_vel_packet_time_us = frame.timestamp_us;  // 硬件时间戳（u32）
                    last_vel_packet_instant = Some(std::time::Instant::now());  // 系统时间（用于超时检查）

                    // 3. 判断是否提交（混合策略：集齐或超时）
                    let all_received = vel_update_mask == 0b111111;  // 0x3F，6 个关节全部收到
                    // 注意：硬件时间戳之间可以比较（来自同一个设备），但不能与系统时间戳比较
                    let time_since_last_commit = if frame.timestamp_us >= last_vel_commit_time_us {
                        frame.timestamp_us - last_vel_commit_time_us
                    } else {
                        // 硬件时间戳可能回绕（u32 微秒，约 71 分钟回绕一次）
                        // 当回绕发生时，认为时间差为 0（立即提交）
                        // 这是安全的：即使这次不提交，下一帧（约 2ms 后，500Hz 控制周期）到来时，all_received 逻辑会处理
                        0
                    };
                    let timeout_threshold_us = 1200;  // 1.2ms 超时（防止丢帧导致死锁，单位：硬件时间戳微秒）

                    // 策略 A：集齐 6 个关节（严格同步）
                    // 策略 B：超时提交（容错）
                    if all_received || time_since_last_commit > timeout_threshold_us {
                        // 原子性地一次性提交所有关节的速度
                        // 注意：硬件时间戳是 u32，但状态中使用 u64（与其他时间戳统一）
                        pending_joint_dynamic.group_timestamp_us = frame.timestamp_us as u64;
                        pending_joint_dynamic.valid_mask = vel_update_mask;

                        ctx.joint_dynamic.store(Arc::new(pending_joint_dynamic.clone()));

                        // 重置状态（准备下一轮）
                        vel_update_mask = 0;
                        last_vel_commit_time_us = frame.timestamp_us;  // 硬件时间戳（u32）
                        last_vel_packet_instant = None;  // 重置系统时间戳

                        // 如果超时提交，记录警告（可能丢帧）
                        if !all_received {
                            warn!(
                                "Velocity frame commit timeout: mask={:06b}, incomplete data",
                                vel_update_mask
                            );
                        } else {
                            trace!("Joint dynamic committed: 6 joints velocity/current updated");
                        }
                    }
                }
            }

            // === 控制状态（独立帧，立即提交） ===

            // 机械臂状态反馈 (0x2A1)
            ID_ROBOT_STATUS => {
                if let Ok(feedback) = RobotStatusFeedback::try_from(frame) {
                    ctx.control_status.rcu(|state| {
                        let mut new = state.clone();
                        new.control_mode = feedback.control_mode as u8;
                        new.robot_status = feedback.robot_status as u8;
                        new.move_mode = feedback.move_mode as u8;
                        new.teach_status = feedback.teach_status as u8;
                        new.motion_status = feedback.motion_status as u8;
                        new.trajectory_point_index = feedback.trajectory_point_index;
                        new.fault_angle_limit = [
                            feedback.fault_code_angle_limit.joint1_limit(),
                            feedback.fault_code_angle_limit.joint2_limit(),
                            feedback.fault_code_angle_limit.joint3_limit(),
                            feedback.fault_code_angle_limit.joint4_limit(),
                            feedback.fault_code_angle_limit.joint5_limit(),
                            feedback.fault_code_angle_limit.joint6_limit(),
                        ];
                        new.fault_comm_error = [
                            feedback.fault_code_comm_error.joint1_comm_error(),
                            feedback.fault_code_comm_error.joint2_comm_error(),
                            feedback.fault_code_comm_error.joint3_comm_error(),
                            feedback.fault_code_comm_error.joint4_comm_error(),
                            feedback.fault_code_comm_error.joint5_comm_error(),
                            feedback.fault_code_comm_error.joint6_comm_error(),
                        ];
                        new.is_enabled = feedback.robot_status == RobotStatus::Normal;
                        // 注意：硬件时间戳是 u32，但状态中使用 u64（与其他时间戳统一）
                        new.timestamp_us = frame.timestamp_us as u64;
                        new
                    });
                }
            }

            // 夹爪反馈 (0x2A8)
            ID_GRIPPER_FEEDBACK => {
                if let Ok(feedback) = GripperFeedback::try_from(frame) {
                    // 1. 更新 ControlStatusState（数据：行程和扭矩）
                    ctx.control_status.rcu(|state| {
                        let mut new = state.clone();
                        new.gripper_travel = feedback.travel();  // mm
                        new.gripper_torque = feedback.torque();  // N·m
                        // 注意：硬件时间戳是 u32，但状态中使用 u64（与其他时间戳统一）
                        new.timestamp_us = frame.timestamp_us as u64;
                        new
                    });

                    // 2. 更新 DiagnosticState（状态位域）
                    // 注意：使用 try_write() 避免 IO 线程被用户线程的 read 锁阻塞
                    if let Ok(mut diag) = ctx.diagnostics.try_write() {
                        let status = feedback.status();
                        diag.gripper_voltage_low = status.voltage_low();
                        diag.gripper_motor_over_temp = status.motor_over_temp();
                        diag.gripper_over_current = status.driver_over_current();
                        diag.gripper_over_temp = status.driver_over_temp();
                        diag.gripper_sensor_error = status.sensor_error();
                        diag.gripper_driver_error = status.driver_error();
                        diag.gripper_enabled = status.enabled();  // 注意：反向逻辑已在 GripperStatus 中处理
                        diag.gripper_homed = status.homed();
                        // 注意：硬件时间戳是 u32，但状态中使用 u64（与其他时间戳统一）
                        diag.timestamp_us = frame.timestamp_us as u64;
                    } else {
                        // 锁被占用，跳过本次更新
                        trace!("Skipped gripper status update due to lock contention");
                    }
                }
            }

            // === 诊断状态反馈（使用 RwLock） ===

            // 关节驱动器低速反馈（温度、电压、电流、状态）(0x261-0x266)
            id if id >= ID_JOINT_DRIVER_LOW_SPEED_BASE && id <= ID_JOINT_DRIVER_LOW_SPEED_BASE + 5 => {
                let joint_index = (id - ID_JOINT_DRIVER_LOW_SPEED_BASE) as usize;
                if let Ok(feedback) = JointDriverLowSpeedFeedback::try_from(frame) {
                    // 使用 try_write() 避免 IO 线程被用户线程的 read 锁阻塞
                    if let Ok(mut diag) = ctx.diagnostics.try_write() {
                        diag.motor_temps[joint_index] = feedback.motor_temp();
                        diag.driver_temps[joint_index] = feedback.driver_temp();
                        diag.joint_voltage[joint_index] = feedback.voltage();
                        diag.joint_bus_current[joint_index] = feedback.bus_current();

                        // 更新驱动器状态位
                        let status = feedback.status();
                        diag.driver_voltage_low[joint_index] = status.voltage_low();
                        diag.driver_motor_over_temp[joint_index] = status.motor_over_temp();
                        diag.driver_over_current[joint_index] = status.driver_over_current();
                        diag.driver_over_temp[joint_index] = status.driver_over_temp();
                        diag.driver_collision_protection[joint_index] = status.collision_protection();
                        diag.driver_error[joint_index] = status.driver_error();
                        diag.driver_enabled[joint_index] = status.enabled();
                        diag.driver_stall_protection[joint_index] = status.stall_protection();

                        // 注意：硬件时间戳是 u32，但状态中使用 u64（与其他时间戳统一）
                        diag.timestamp_us = frame.timestamp_us as u64;
                    } else {
                        // 锁被占用，跳过本次更新（诊断数据不可靠没关系，IO 线程流畅最重要）
                        trace!("Skipped diagnostic update due to lock contention (joint {})", joint_index + 1);
                    }
                }
            }

            // 碰撞保护等级反馈 (0x47B)
            ID_COLLISION_PROTECTION_LEVEL_FEEDBACK => {
                if let Ok(feedback) = CollisionProtectionLevelFeedback::try_from(frame) {
                    // 使用 try_write() 避免 IO 线程被用户线程的 read 锁阻塞
                    if let Ok(mut diag) = ctx.diagnostics.try_write() {
                        diag.protection_levels = feedback.levels;
                        // 注意：硬件时间戳是 u32，但状态中使用 u64（与其他时间戳统一）
                        diag.timestamp_us = frame.timestamp_us as u64;
                    } else {
                        // 锁被占用，跳过本次更新
                        trace!("Skipped collision protection level update due to lock contention");
                    }
                }
            }

            // === 配置状态反馈（使用 RwLock，按需查询） ===

            // 电机限制反馈 (0x473) - 需要查询 6 次，每个关节一次
            ID_MOTOR_LIMIT_FEEDBACK => {
                if let Ok(feedback) = MotorLimitFeedback::try_from(frame) {
                    let joint_index = (feedback.joint_index - 1) as usize;  // 关节序号从 1 开始
                    // 使用 try_write() 避免 IO 线程被用户线程的 read 锁阻塞
                    if let Ok(mut config) = ctx.config.try_write() {
                        // 单位转换：度 → 弧度
                        config.joint_limits_max[joint_index] = feedback.max_angle().to_radians();
                        config.joint_limits_min[joint_index] = feedback.min_angle().to_radians();
                        // 最大速度已经是 rad/s，无需转换
                        config.joint_max_velocity[joint_index] = feedback.max_velocity();
                    }
                }
            }

            // 电机最大加速度反馈 (0x47C) - 需要查询 6 次，每个关节一次
            ID_MOTOR_MAX_ACCEL_FEEDBACK => {
                if let Ok(feedback) = MotorMaxAccelFeedback::try_from(frame) {
                    let joint_index = (feedback.joint_index - 1) as usize;  // 关节序号从 1 开始
                    // 使用 try_write() 避免 IO 线程被用户线程的 read 锁阻塞
                    // 注意：MotorMaxAccelFeedback.max_accel() 返回的是 rad/s²（已验证），无需转换
                    if let Ok(mut config) = ctx.config.try_write() {
                        config.max_acc_limits[joint_index] = feedback.max_accel();
                    }
                }
            }

            // 末端速度/加速度参数反馈 (0x478)
            ID_END_VELOCITY_ACCEL_FEEDBACK => {
                if let Ok(feedback) = EndVelocityAccelFeedback::try_from(frame) {
                    // 使用 try_write() 避免 IO 线程被用户线程的 read 锁阻塞
                    if let Ok(mut config) = ctx.config.try_write() {
                        config.max_end_linear_velocity = feedback.max_linear_velocity();  // m/s
                        config.max_end_angular_velocity = feedback.max_angular_velocity();  // rad/s
                        config.max_end_linear_accel = feedback.max_linear_accel();  // m/s²
                        config.max_end_angular_accel = feedback.max_angular_accel();  // rad/s²
                    }
                }
            }

            // 其他反馈帧...
            _ => {
                trace!("Unhandled CAN ID: 0x{:X}", frame.id);
            }
        }

        // ============================================================
        // 3. 检查命令通道（非阻塞）
        // ============================================================
        // 非阻塞地检查命令通道，发送所有待发送的控制帧
        while let Ok(cmd_frame) = cmd_rx.try_recv() {
            if let Err(e) = can.send(cmd_frame) {
                error!("Failed to send control frame: {}", e);
                // 继续处理，不中断循环
            }
        }
        // 如果通道为空，继续接收 CAN 帧（回到循环开始）
        // 如果通道断开，继续循环（下次 try_recv 会返回 Disconnected）
    }
}


/// 获取当前时间戳（微秒）
fn current_timestamp_us() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros() as u64
}
```

### 4.3. Frame Commit 的关键点

1. **帧组识别**：需要知道哪些帧属于同一个帧组
   - **关节位置**：0x2A5, 0x2A6, 0x2A7 → 最后一帧（0x2A7）触发提交
   - **末端位姿**：0x2A2, 0x2A3, 0x2A4 → 最后一帧（0x2A4）触发提交

**✅ CoreMotionState 的帧组处理策略**
   - `CoreMotionState` 包含两个独立的帧组：`joint_pos`（0x2A5-0x2A7）和 `end_pose`（0x2A2-0x2A4）
   - **为每个帧组维护独立的 pending 状态**：`pending_joint_pos` 和 `pending_end_pose`
   - **提交策略**：
     - 如果两个帧组都完整，提交完整状态（两个字段都是新值）
     - 如果只有一个帧组完整，从当前状态读取另一个字段，只更新已完整的帧组（避免状态撕裂）
   - 这样设计既保证了原子性（不会出现部分字段是新值、部分是旧值的情况），又保证了及时性（单个帧组完整时立即更新）

2. **Buffered Commit（核心改进）**：对于独立帧（如关节速度 0x251-0x256），
   - **收集阶段**：收到速度帧后先更新缓冲区，不立即提交
   - **提交判断**：集齐 6 帧（`mask == 0x3F`）或超时（1.2ms）后一次性提交
   - **原子性保证**：一次性提交所有 6 个关节的数据，避免状态撕裂

3. **超时处理**：
   - **帧组超时**：如果帧组不完整（例如只收到 0x2A5 和 0x2A6），超过 `frame_group_timeout` 后应丢弃缓存
   - **速度帧超时**：如果 1.2ms 内未集齐 6 个关节的速度帧，也进行提交（防止丢帧导致死锁）

4. **混合更新**：部分状态（如 `ControlStatusState`）可以单独更新，不需要等待帧组。

5. **配置查询的累积逻辑**：
   - `MotorLimitFeedback` (0x473) 和 `MotorMaxAccelFeedback` (0x47C) 是**单关节反馈**，需要查询 6 次（每个关节一次）
   - 每次收到反馈帧时，根据 `joint_index` 更新对应的数组元素
   - 不需要等待集齐 6 个关节，每个关节的配置独立更新
   - **单位转换**：`MotorLimitFeedback.max_angle()` 和 `.min_angle()` 返回**度**，需要转换为**弧度**（使用 `.to_radians()`）

## 5. 对外 API 设计（Piper 结构体）

### 5.1. Piper 结构

```rust
// src/driver/robot.rs

use crate::can::{CanAdapter, CanError};
use crate::driver::state::*;
use crate::driver::pipeline::*;
use crossbeam_channel::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread::{JoinHandle, spawn};

/// Piper 机械臂驱动（对外 API）
pub struct Piper {
    /// 命令发送通道（向 IO 线程发送控制帧）
    cmd_tx: Sender<PiperFrame>,
    /// 共享状态上下文
    ctx: Arc<PiperContext>,
    /// IO 线程句柄（Drop 时 join）
    io_thread: Option<JoinHandle<()>>,
}

impl Piper {
    /// 创建新的 Piper 实例
    ///
    /// # 参数
    /// - `can`: CAN 适配器（会被移动到 IO 线程）
    /// - `config`: Pipeline 配置（可选）
    ///
    /// # 错误
    /// - `CanError`: CAN 设备初始化失败
    pub fn new(can: impl CanAdapter + Send + 'static, config: Option<PipelineConfig>) -> Result<Self, CanError> {
        // 创建命令通道（有界队列，容量 10）
        let (cmd_tx, cmd_rx) = crossbeam_channel::bounded(10);

        // 创建共享状态上下文
        let ctx = Arc::new(PiperContext::new());

        // 克隆上下文用于 IO 线程
        let ctx_clone = ctx.clone();

        // 启动 IO 线程
        let io_thread = spawn(move || {
            io_loop(can, cmd_rx, ctx_clone, config.unwrap_or_default());
        });

        Ok(Self {
            cmd_tx,
            ctx,
            io_thread: Some(io_thread),
        })
    }

    /// 获取核心运动状态（无锁，纳秒级返回）
    ///
    /// 包含关节位置和末端位姿（帧组同步）。
    ///
    /// # 性能
    /// - 无锁读取（ArcSwap::load）
    /// - 返回快照副本（Clone 开销低，< 200 字节）
    /// - 适合 500Hz 控制循环
    pub fn get_core_motion(&self) -> CoreMotionState {
        (**self.ctx.core_motion.load()).clone()
    }

    /// 获取关节动态状态（无锁，纳秒级返回）
    ///
    /// 包含关节速度和电流（独立帧 + Buffered Commit）。
    ///
    /// # 性能
    /// - 无锁读取（ArcSwap::load）
    /// - 返回快照副本（Clone 开销低，< 150 字节）
    /// - 适合 500Hz 控制循环
    pub fn get_joint_dynamic(&self) -> JointDynamicState {
        (**self.ctx.joint_dynamic.load()).clone()
    }

    /// 获取控制状态（无锁）
    ///
    /// 包含控制模式、机器人状态、夹爪状态等（低频更新）。
    pub fn get_control_status(&self) -> ControlStatusState {
        (**self.ctx.control_status.load()).clone()
    }

    /// 获取时间对齐的运动状态（推荐用于力控算法）
    ///
    /// 以 `core.timestamp_us` 为基准时间，检查时间戳差异。
    /// 即使时间戳差异超过阈值，也返回状态数据（让用户有选择权）。
    ///
    /// # 参数
    /// - `max_time_diff_us`: 允许的最大时间戳差异（微秒），推荐值：5000（5ms）
    ///
    /// # 返回值
    /// - `AlignmentResult::Ok(state)`: 时间戳差异在可接受范围内
    /// - `AlignmentResult::Misaligned { state, diff_us }`: 时间戳差异过大，但仍返回状态数据
    ///
    /// # 使用场景
    /// 用于力控算法。如果返回 `Misaligned`，用户可以选择：
    /// - 基于未对齐数据继续运行（容忍延迟）
    /// - 急停（严格要求同步）
    pub fn get_aligned_motion(&self, max_time_diff_us: u64) -> AlignmentResult {
        let core = self.get_core_motion();
        let joint_dynamic = self.get_joint_dynamic();

        let time_diff = core.timestamp_us.abs_diff(joint_dynamic.group_timestamp_us);

        let state = AlignedMotionState {
            joint_pos: core.joint_pos,
            joint_vel: joint_dynamic.joint_vel,
            joint_current: joint_dynamic.joint_current,
            end_pose: core.end_pose,
            timestamp: core.timestamp_us,  // 使用位置数据的时间戳作为基准
            time_diff_us: time_diff as i64,
        };

        // 检查速度数据是否完整
        // 注意：使用 debug! 而不是 warn!，避免在高频循环中（500Hz）出现持续丢帧时刷屏
        // 持续的 warn! 日志可能导致 I/O 瓶颈和性能抖动
        if !joint_dynamic.is_complete() {
            let missing = joint_dynamic.missing_joints();
            debug!("Velocity data incomplete: missing joints {:?}", missing);
        }

        if time_diff > max_time_diff_us {
            AlignmentResult::Misaligned {
                state,
                diff_us: time_diff,
            }
        } else {
            AlignmentResult::Ok(state)
        }
    }

    /// 等待接收到第一个有效反馈（用于初始化）
    ///
    /// 在 `Piper::new()` 后调用，确保在控制循环开始前已收到有效数据。
    /// 避免使用全零的初始状态导致错误的控制指令。
    ///
    /// # 参数
    /// - `timeout`: 超时时间（秒）
    ///
    /// # 错误
    /// - `DriverError::Timeout`: 超时未收到有效反馈
    ///
    /// # 示例
    /// ```rust
    /// let piper = PiperBuilder::new().build()?;
    /// piper.wait_for_feedback(Duration::from_secs(2))?;  // 等待 2 秒
    /// // 现在可以安全地调用 get_core_motion()，数据不会是全零
    /// ```
    pub fn wait_for_feedback(&self, timeout: Duration) -> Result<(), DriverError> {
        let start = std::time::Instant::now();

        loop {
            let state = self.get_core_motion();
            // 假设有效帧时间戳不为 0（Default 时 timestamp_us = 0）
            if state.timestamp_us > 0 {
                return Ok(());
            }

            if start.elapsed() > timeout {
                return Err(DriverError::Timeout);
            }

            // 避免 CPU 自旋，每 10ms 检查一次
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// 获取组合运动状态（所有热数据）
    ///
    /// 注意：不同子状态的时间戳可能不同步（差异通常在毫秒级）。
    /// 如果需要时间对齐的状态，请使用 `get_aligned_motion()`。
    pub fn get_motion_state(&self) -> CombinedMotionState {
        CombinedMotionState {
            core: self.get_core_motion(),
            joint_dynamic: self.get_joint_dynamic(),
        }
    }

    /// 获取诊断状态（读锁，10Hz 以下）
    pub fn get_diagnostic_state(&self) -> Result<DiagnosticState, DriverError> {
        self.ctx.diagnostics.read()
            .map(|guard| guard.clone())
            .map_err(|_| DriverError::PoisonedLock)
    }

    /// 获取配置状态（读锁）
    pub fn get_config_state(&self) -> Result<ConfigState, DriverError> {
        self.ctx.config.read()
            .map(|guard| guard.clone())
            .map_err(|_| DriverError::PoisonedLock)
    }

    /// 发送控制帧
    ///
    /// # 参数
    /// - `frame`: 控制帧（已构建的 `PiperFrame`）
    ///
    /// # 错误
    /// - `DriverError::ChannelClosed`: 命令通道已关闭（IO 线程退出）
    /// - `DriverError::ChannelFull`: 命令队列已满（缓冲区容量 10）
    pub fn send_frame(&self, frame: PiperFrame) -> Result<(), DriverError> {
        self.cmd_tx.try_send(frame)
            .map_err(|e| match e {
                crossbeam_channel::TrySendError::Full(_) => DriverError::ChannelFull,
                crossbeam_channel::TrySendError::Disconnected(_) => DriverError::ChannelClosed,
            })
    }

    /// 阻塞发送控制帧（等待队列有空间，带超时）
    ///
    /// # 参数
    /// - `frame`: 控制帧（已构建的 `PiperFrame`）
    /// - `timeout`: 超时时间（推荐：2-4ms，不应超过控制周期太多）
    ///
    /// # 注意
    /// - 使用 `send_timeout` 避免在 IO 线程死循环时永远阻塞
    /// - 对于 500Hz（2ms 周期）的控制循环，超时时间不应超过 4ms
    ///   如果 IO 线程卡顿超过 4ms，下一周期的控制指令可能已经失效
    /// - 在高频控制循环中，**强烈建议使用 `send_frame()` 并处理 `ChannelFull` 错误**，
    ///   而不是使用此阻塞方法
    ///
    /// # 错误
    /// - `DriverError::ChannelClosed`: 命令通道已关闭（IO 线程退出）
    /// - `DriverError::Timeout`: 超时未发送（队列已满且超时）
    pub fn send_frame_blocking(&self, frame: PiperFrame, timeout: Duration) -> Result<(), DriverError> {
        self.cmd_tx.send_timeout(frame, timeout)
            .map_err(|e| match e {
                crossbeam_channel::SendTimeoutError::Timeout(_) => DriverError::Timeout,
                crossbeam_channel::SendTimeoutError::Disconnected(_) => DriverError::ChannelClosed,
            })
    }
}

impl Drop for Piper {
    fn drop(&mut self) {
        // 关闭命令通道（通知 IO 线程退出）
        drop(self.cmd_tx);

        // 等待 IO 线程退出
        if let Some(handle) = self.io_thread.take() {
            if let Err(e) = handle.join() {
                error!("IO thread panicked: {:?}", e);
            }
        }
    }
}
```

### 5.2. 组合状态和时间对齐结构

```rust
/// 组合运动状态（向后兼容）
pub struct CombinedMotionState {
    pub core: CoreMotionState,
    pub joint_dynamic: JointDynamicState,
}

/// 时间对齐后的运动状态
///
/// 用于力控算法，确保位置和速度数据的时间戳差异在可接受范围内。
pub struct AlignedMotionState {
    pub joint_pos: [f64; 6],
    pub joint_vel: [f64; 6],
    pub joint_current: [f64; 6],
    pub end_pose: [f64; 6],
    pub timestamp: u64,          // 基准时间戳（来自位置数据）
    pub time_diff_us: i64,       // 速度数据与位置数据的时间差（用于调试）
}

/// 时间对齐结果
///
/// 即使时间戳不对齐，也返回状态数据，让用户有选择权（是急停还是继续运行）。
#[derive(Debug)]
pub enum AlignmentResult {
    /// 时间戳对齐，数据可靠
    Ok(AlignedMotionState),
    /// 时间戳不对齐，但数据仍然返回（让用户决定是急停还是容忍延迟）
    Misaligned {
        state: AlignedMotionState,
        diff_us: u64,
    },
}
```

### 5.3. 错误定义

```rust
// src/driver/error.rs

use thiserror::Error;
use crate::can::CanError;
use crate::protocol::ProtocolError;

#[derive(Error, Debug)]
pub enum DriverError {
    #[error("CAN driver error: {0}")]
    Can(#[from] CanError),

    #[error("Protocol error: {0}")]
    Protocol(#[from] ProtocolError),

    #[error("Command channel closed")]
    ChannelClosed,

    #[error("Command channel full (buffer size: 10)")]
    ChannelFull,

    #[error("Poisoned lock (thread panic)")]
    PoisonedLock,

    #[error("IO thread error: {0}")]
    IoThread(String),

    #[error("Not implemented: {0}")]
    NotImplemented(String),

    #[error("Operation timeout")]
    Timeout,
}
```

## 6. Builder 模式（PiperBuilder）

```rust
// src/driver/builder.rs

use crate::can::{CanAdapter, GsUsbCanAdapter};
use crate::driver::robot::Piper;
use crate::driver::pipeline::PipelineConfig;
use crate::driver::error::DriverError;

/// Piper Builder（链式构造）
pub struct PiperBuilder {
    /// CAN 接口名称（Linux: "can0", macOS/Windows: None 或 VID:PID）
    interface: Option<String>,
    /// CAN 波特率（1M, 500K, 250K 等）
    baud_rate: Option<u32>,
    /// Pipeline 配置
    pipeline_config: Option<PipelineConfig>,
}

impl PiperBuilder {
    /// 创建新的 Builder
    pub fn new() -> Self {
        Self {
            interface: None,
            baud_rate: None,
            pipeline_config: None,
        }
    }

    /// 设置 CAN 接口（可选，默认自动检测）
    pub fn interface(mut self, interface: impl Into<String>) -> Self {
        self.interface = Some(interface.into());
        self
    }

    /// 设置 CAN 波特率（可选，默认 1M）
    pub fn baud_rate(mut self, baud_rate: u32) -> Self {
        self.baud_rate = Some(baud_rate);
        self
    }

    /// 设置 Pipeline 配置（可选）
    pub fn pipeline_config(mut self, config: PipelineConfig) -> Self {
        self.pipeline_config = Some(config);
        self
    }

    /// 构建 Piper 实例
    pub fn build(self) -> Result<Piper, DriverError> {
        // 创建 CAN 适配器
        #[cfg(not(target_os = "linux"))]
        let can = GsUsbCanAdapter::new(
            self.interface.as_deref(),
            self.baud_rate.unwrap_or(1_000_000),
        )?;

        #[cfg(target_os = "linux")]
        // TODO: 实现 SocketCAN 适配器
        return Err(DriverError::NotImplemented("SocketCAN not implemented yet"));

        // 创建 Piper 实例
        Piper::new(can, self.pipeline_config)
            .map_err(DriverError::from)
    }
}

impl Default for PiperBuilder {
    fn default() -> Self {
        Self::new()
    }
}
```

**使用示例：**
```rust
use piper_sdk::driver::PiperBuilder;
use std::time::Duration;

let piper = PiperBuilder::new()
    .interface("can0")  // 可选
    .baud_rate(1_000_000)  // 可选
    .build()?;

// 等待收到第一个有效反馈，避免使用全零的初始状态
piper.wait_for_feedback(Duration::from_secs(2))?;

// 在控制循环中读取状态（推荐使用时间对齐的状态）
loop {
    match piper.get_aligned_motion(5000) {  // 允许 5ms 时间差
        AlignmentResult::Ok(state) => {
            // 时间戳对齐，数据可靠
            // state.joint_pos, state.joint_vel, state.joint_current, state.end_pose 都是时间对齐的
            // 计算控制指令...
            piper.send_frame(control_frame)?;
        }
        AlignmentResult::Misaligned { state, diff_us } => {
            // 时间戳不对齐，但数据仍然返回（让用户决定）
            warn!("Motion state time mismatch: {}us, continuing with misaligned data", diff_us);
            // 用户可以选择：
            // 1. 基于未对齐数据继续运行（容忍延迟）
            // 2. 急停（严格要求同步）
            // 这里选择继续运行
            // 计算控制指令...
            piper.send_frame(control_frame)?;
        }
    }
}
```

## 7. 模块导出

```rust
// src/driver/mod.rs

pub mod state;
pub mod pipeline;
pub mod robot;
pub mod builder;
mod error;

pub use state::*;
pub use robot::Piper;
pub use builder::PiperBuilder;
pub use error::DriverError;
```

## 8. 实现优先级

### Phase 1: 基础框架
1. ✅ 实现 `state.rs`（MotionState, DiagnosticState, ConfigState）
2. ✅ 实现 `pipeline.rs`（基础 IO 循环，不带 Frame Commit）
3. ✅ 实现 `robot.rs`（基础 API）
4. ✅ 实现 `builder.rs`（Builder 模式）

### Phase 2: Frame Commit 机制
1. ✅ 在 `pipeline.rs` 中实现 Frame Commit 逻辑
2. ✅ 处理帧组超时（避免数据过期）
3. ✅ 混合更新（部分状态单独更新）

### Phase 3: 完整协议支持
1. ✅ 实现所有反馈帧的解析（已在 `protocol` 模块实现）
2. ✅ 实现所有控制帧的发送（已在 `protocol` 模块实现）
3. ✅ 添加诊断状态更新逻辑

### Phase 4: 错误处理和测试
1. ✅ 完善错误处理（IO 线程错误上报）
2. ✅ 添加单元测试
3. ✅ 添加集成测试（模拟 CAN 帧序列）

## 9. 关键设计决策

### 9.1. 为什么使用 ArcSwap 而不是 Mutex？

- **性能**：ArcSwap 的 `load()` 是无锁操作（原子指针交换），适合高频读取
- **一致性**：`load()` 返回的是完整的快照，不会看到撕裂数据
- **开销**：Clone 整个 MotionState（< 256 字节）的开销远低于锁竞争

### 9.2. 为什么 Frame Commit 是必须的？

如果一帧完整状态由多个 CAN 帧组成（如 3 个关节反馈帧），
每个帧单独更新会导致控制循环读到不一致的数据（部分关节是旧的，部分是新的）。

**Frame Commit 保证**：**只有当完整帧组都收到时，才原子地提交整个状态**。

**Buffered Commit 的作用**：
- 对于独立帧（如关节速度 0x251-0x256），通过收集-提交模式，保证 6 个关节的数据来自同一 CAN 传输周期
- 避免状态撕裂（前 3 个关节是新的，后 3 个是旧的）
- 将 6 次 `ArcSwap` 写操作合并为 1 次，减少 cache thrashing

### 9.3. 为什么命令通道容量设为 10？

- **避免无界队列**：如果 IO 线程发送失败，无界队列会导致内存无限增长
- **感知阻塞**：容量小（10），控制线程可以感知到 IO 阻塞（try_send 返回 ChannelFull）
- **延迟控制**：旧指令不会堆积，适合实时控制

### 9.4. 为什么 DiagnosticState 使用 RwLock？

**原设计考虑**：
- **读取频率低**：诊断数据不需要高频读取（10Hz 以下）
- **避免内存分配**：RwLock 不需要 Clone，只返回借用
- **写频率低**：诊断状态更新频率低，锁竞争不严重

**⚠️ 重要修正：优先级反转风险**

**问题**：如果用户线程持有了 `read` 锁进行耗时的操作（如序列化日志、打印 Debug 信息），IO 线程会被阻塞在 `write()` 上，导致运动控制帧堆积。

**解决方案**：
- **IO 线程使用 `try_write()`**：如果拿不到锁，丢弃本次诊断数据更新（诊断数据不可靠没关系，IO 线程流畅最重要）
- **或者统一使用 `ArcSwap`**：虽然诊断数据结构稍大（几百字节），但在现代 CPU 上 memcpy 的开销通常小于锁竞争和上下文切换的开销

**当前设计**：IO 线程使用 `try_write()`，避免被阻塞。如果写入失败，记录警告并继续处理其他帧。

## 10. 性能考虑

### 10.1. 读取性能
- `get_core_motion()`: 无锁，纳秒级（ArcSwap::load + Clone，< 200 字节）
- `get_joint_dynamic()`: 无锁，纳秒级（ArcSwap::load + Clone，< 150 字节）
- `get_control_status()`: 无锁，纳秒级（ArcSwap::load + Clone）
- `get_aligned_motion()`: 无锁，纳秒级（读取两个子状态并检查时间戳）
- `get_diagnostic_state()`: 读锁，微秒级（RwLock::read）

### 10.2. 更新性能
- `CoreMotionState` 更新：微秒级（ArcSwap::store，帧组最后一帧触发）
- `JointDynamicState` 更新：微秒级（ArcSwap::store，集齐 6 帧或超时后触发，**仅 1 次写入**）
- `ControlStatusState` 更新：微秒级（ArcSwap::rcu，单个 CAN 帧触发）
- `DiagnosticState` 更新：微秒级（RwLock::write，更新频率低）

### 10.3. 控制延迟
- `send_frame()`: 微秒级（try_send，无阻塞）
- IO 线程处理延迟：取决于 CAN 硬件和 USB 延迟（通常 < 1ms）

## 11. 测试策略

### 11.1. 单元测试
- `state.rs`: 测试状态结构的 Clone 和 Default
- `pipeline.rs`: 测试帧解析和 Frame Commit 逻辑（模拟 CAN 帧序列）

### 11.2. 集成测试
- 端到端测试：创建 `Piper` 实例 → 模拟 CAN 帧输入 → 验证状态更新
- 性能测试：测量 `get_motion_state()` 的延迟（应在纳秒级）

### 11.3. 压力测试
- 高频读取：500Hz 循环调用 `get_motion_state()`，验证无锁性能
- 帧组超时：模拟不完整帧组，验证超时处理

## 12. 后续扩展

### 12.1. 回调机制
如果用户需要事件通知（如错误发生、状态变化），可以添加回调机制：
```rust
pub struct PiperBuilder {
    // ...
    on_error: Option<Box<dyn Fn(DriverError) + Send + Sync>>,
    on_state_change: Option<Box<dyn Fn(&MotionState) + Send + Sync>>,
}
```

### 12.2. 统计信息
添加性能统计（帧丢失率、延迟分布等）：
```rust
pub struct PipelineStats {
    pub frames_received: u64,
    pub frames_dropped: u64,
    pub average_latency_us: f64,
}
```

### 12.3. 重连机制
如果 CAN 连接断开，自动重连：
```rust
impl Piper {
    pub fn reconnect(&mut self) -> Result<(), DriverError> {
        // 重启 IO 线程，重新连接 CAN
    }
}
```

---

**文档版本**: v1.0
**最后更新**: 2024-12
**作者**: Driver 模块设计团队

