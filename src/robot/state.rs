//! Robot 模块状态结构定义

use arc_swap::ArcSwap;
use std::sync::{Arc, RwLock};

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
    ///
    /// 初始化所有状态结构，包括：
    /// - 热数据（ArcSwap）：`core_motion`, `joint_dynamic`
    /// - 温数据（ArcSwap）：`control_status`
    /// - 冷数据（RwLock）：`diagnostics`, `config`
    ///
    /// # Example
    ///
    /// ```
    /// use piper_sdk::robot::PiperContext;
    ///
    /// let ctx = PiperContext::new();
    /// let core = ctx.core_motion.load();
    /// assert_eq!(core.timestamp_us, 0);
    /// ```
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

impl Default for PiperContext {
    fn default() -> Self {
        Self::new()
    }
}

/// 组合运动状态（向后兼容）
pub struct CombinedMotionState {
    pub core: CoreMotionState,
    pub joint_dynamic: JointDynamicState,
}

/// 时间对齐后的运动状态
///
/// 用于力控算法，确保位置和速度数据的时间戳差异在可接受范围内。
#[derive(Debug)]
pub struct AlignedMotionState {
    pub joint_pos: [f64; 6],
    pub joint_vel: [f64; 6],
    pub joint_current: [f64; 6],
    pub end_pose: [f64; 6],
    pub timestamp: u64,    // 基准时间戳（来自位置数据）
    pub time_diff_us: i64, // 速度数据与位置数据的时间差（用于调试）
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

#[cfg(test)]
mod tests {
    use super::CoreMotionState;

    #[test]
    fn test_core_motion_state_default() {
        let state = CoreMotionState::default();
        assert_eq!(state.timestamp_us, 0);
        assert_eq!(state.joint_pos, [0.0; 6]);
        assert_eq!(state.end_pose, [0.0; 6]);
    }

    #[test]
    fn test_core_motion_state_clone() {
        let state = CoreMotionState {
            timestamp_us: 12345,
            joint_pos: [1.0, 2.0, 3.0, 4.0, 5.0, 6.0],
            end_pose: [0.1, 0.2, 0.3, 0.4, 0.5, 0.6],
        };
        let cloned = state.clone();
        assert_eq!(state.timestamp_us, cloned.timestamp_us);
        assert_eq!(state.joint_pos, cloned.joint_pos);
        assert_eq!(state.end_pose, cloned.end_pose);
    }

    use super::JointDynamicState;

    #[test]
    fn test_joint_dynamic_state_is_complete() {
        let state = JointDynamicState {
            valid_mask: 0b111111, // 所有关节已更新
            ..Default::default()
        };
        assert!(state.is_complete());

        let state2 = JointDynamicState {
            valid_mask: 0b111110, // J6 未更新
            ..Default::default()
        };
        assert!(!state2.is_complete());
    }

    #[test]
    fn test_joint_dynamic_state_missing_joints() {
        // 0b111100 = 0b00111100 (二进制)，表示 bit 0-1 未更新 (J1, J2)
        // 如果要表示 J5, J6 未更新，应该使用 0b001111 (bit 4-5 为 0)
        let state = JointDynamicState {
            valid_mask: 0b001111, // J5, J6 未更新 (只有 bit 0-3 为 1)
            ..Default::default()
        };
        let missing = state.missing_joints();
        assert_eq!(missing, vec![4, 5]); // 索引 4, 5 (J5, J6)

        // 测试另一个场景：只有 J1 和 J3 已更新
        let state2 = JointDynamicState {
            valid_mask: 0b000101, // bit 0 和 bit 2 为 1
            ..Default::default()
        };
        let missing = state2.missing_joints();
        assert_eq!(missing, vec![1, 3, 4, 5]); // J2, J4, J5, J6 未更新
    }

    #[test]
    fn test_joint_dynamic_state_default() {
        let state = JointDynamicState::default();
        assert_eq!(state.group_timestamp_us, 0);
        assert_eq!(state.joint_vel, [0.0; 6]);
        assert_eq!(state.joint_current, [0.0; 6]);
        assert_eq!(state.timestamps, [0; 6]);
        assert_eq!(state.valid_mask, 0);
        assert!(!state.is_complete());
        assert_eq!(state.missing_joints(), vec![0, 1, 2, 3, 4, 5]);
    }

    use super::*;

    #[test]
    fn test_control_status_state_default() {
        let state = ControlStatusState::default();
        assert_eq!(state.timestamp_us, 0);
        assert_eq!(state.control_mode, 0);
        assert_eq!(state.robot_status, 0);
        assert_eq!(state.fault_angle_limit, [false; 6]);
        assert_eq!(state.fault_comm_error, [false; 6]);
        assert!(!state.is_enabled);
        assert_eq!(state.gripper_travel, 0.0);
        assert_eq!(state.gripper_torque, 0.0);
    }

    #[test]
    fn test_diagnostic_state_default() {
        let state = DiagnosticState::default();
        assert_eq!(state.timestamp_us, 0);
        assert_eq!(state.motor_temps, [0.0; 6]);
        assert_eq!(state.driver_temps, [0.0; 6]);
        assert_eq!(state.joint_voltage, [0.0; 6]);
        assert_eq!(state.protection_levels, [0; 6]);
        assert_eq!(state.driver_voltage_low, [false; 6]);
        assert!(!state.gripper_voltage_low);
        assert!(!state.connection_status);
    }

    #[test]
    fn test_config_state_default() {
        let state = ConfigState::default();
        assert_eq!(state.firmware_version, None);
        assert_eq!(state.joint_limits_max, [0.0; 6]);
        assert_eq!(state.joint_limits_min, [0.0; 6]);
        assert_eq!(state.joint_max_velocity, [0.0; 6]);
        assert_eq!(state.max_acc_limits, [0.0; 6]);
        assert_eq!(state.max_end_linear_velocity, 0.0);
    }

    #[test]
    fn test_piper_context_new() {
        let ctx = PiperContext::new();
        // 验证所有 Arc/ArcSwap 都已初始化
        let core = ctx.core_motion.load();
        assert_eq!(core.timestamp_us, 0);
        assert_eq!(core.joint_pos, [0.0; 6]);

        let joint_dynamic = ctx.joint_dynamic.load();
        assert_eq!(joint_dynamic.group_timestamp_us, 0);

        let control_status = ctx.control_status.load();
        assert_eq!(control_status.timestamp_us, 0);

        let diagnostics = ctx.diagnostics.read().unwrap();
        assert_eq!(diagnostics.timestamp_us, 0);

        let config = ctx.config.read().unwrap();
        assert_eq!(config.firmware_version, None);
    }

    #[test]
    fn test_core_motion_state_debug() {
        let state = CoreMotionState {
            timestamp_us: 12345,
            joint_pos: [1.0, 2.0, 3.0, 4.0, 5.0, 6.0],
            end_pose: [0.1, 0.2, 0.3, 0.4, 0.5, 0.6],
        };
        let debug_str = format!("{:?}", state);
        assert!(debug_str.contains("CoreMotionState"));
        assert!(debug_str.contains("12345"));
    }

    #[test]
    fn test_joint_dynamic_state_clone() {
        let state = JointDynamicState {
            group_timestamp_us: 1000,
            joint_vel: [1.0, 2.0, 3.0, 4.0, 5.0, 6.0],
            joint_current: [0.1, 0.2, 0.3, 0.4, 0.5, 0.6],
            timestamps: [100, 200, 300, 400, 500, 600],
            valid_mask: 0b111111,
        };
        let cloned = state.clone();
        assert_eq!(state.group_timestamp_us, cloned.group_timestamp_us);
        assert_eq!(state.joint_vel, cloned.joint_vel);
        assert_eq!(state.joint_current, cloned.joint_current);
        assert_eq!(state.timestamps, cloned.timestamps);
        assert_eq!(state.valid_mask, cloned.valid_mask);
        assert_eq!(state.is_complete(), cloned.is_complete());
    }

    #[test]
    fn test_control_status_state_clone() {
        let state = ControlStatusState {
            timestamp_us: 5000,
            control_mode: 1,
            robot_status: 2,
            move_mode: 3,
            teach_status: 4,
            motion_status: 5,
            trajectory_point_index: 10,
            fault_angle_limit: [true, false, true, false, false, false],
            fault_comm_error: [false, true, false, true, false, false],
            is_enabled: true,
            gripper_travel: 100.5,
            gripper_torque: 2.5,
        };
        let cloned = state.clone();
        assert_eq!(state.timestamp_us, cloned.timestamp_us);
        assert_eq!(state.control_mode, cloned.control_mode);
        assert_eq!(state.fault_angle_limit, cloned.fault_angle_limit);
        assert_eq!(state.is_enabled, cloned.is_enabled);
    }

    #[test]
    fn test_diagnostic_state_clone() {
        let state = DiagnosticState {
            timestamp_us: 10000,
            motor_temps: [25.0, 26.0, 27.0, 28.0, 29.0, 30.0],
            protection_levels: [1, 2, 3, 4, 5, 6],
            connection_status: true,
            ..Default::default()
        };

        let cloned = state.clone();
        assert_eq!(state.timestamp_us, cloned.timestamp_us);
        assert_eq!(state.motor_temps, cloned.motor_temps);
        assert_eq!(state.protection_levels, cloned.protection_levels);
        assert_eq!(state.connection_status, cloned.connection_status);
    }

    #[test]
    fn test_config_state_clone() {
        let state = ConfigState {
            firmware_version: Some("v1.0.0".to_string()),
            joint_limits_max: [
                std::f64::consts::PI,
                std::f64::consts::PI,
                std::f64::consts::PI,
                std::f64::consts::PI,
                std::f64::consts::PI,
                std::f64::consts::PI,
            ],
            joint_limits_min: [
                -std::f64::consts::PI,
                -std::f64::consts::PI,
                -std::f64::consts::PI,
                -std::f64::consts::PI,
                -std::f64::consts::PI,
                -std::f64::consts::PI,
            ],
            joint_max_velocity: [5.0, 5.0, 5.0, 5.0, 5.0, 5.0],
            max_acc_limits: [10.0, 10.0, 10.0, 10.0, 10.0, 10.0],
            max_end_linear_velocity: 1.0,
            max_end_angular_velocity: 2.0,
            max_end_linear_accel: 3.0,
            max_end_angular_accel: 4.0,
        };
        let cloned = state.clone();
        assert_eq!(state.firmware_version, cloned.firmware_version);
        assert_eq!(state.joint_limits_max, cloned.joint_limits_max);
        assert_eq!(
            state.max_end_linear_velocity,
            cloned.max_end_linear_velocity
        );
    }

    #[test]
    fn test_aligned_motion_state_debug() {
        let state = AlignedMotionState {
            joint_pos: [1.0; 6],
            joint_vel: [2.0; 6],
            joint_current: [3.0; 6],
            end_pose: [4.0; 6],
            timestamp: 1000,
            time_diff_us: 500,
        };
        let debug_str = format!("{:?}", state);
        assert!(debug_str.contains("AlignedMotionState"));
    }

    #[test]
    fn test_alignment_result_debug() {
        let state = AlignedMotionState {
            joint_pos: [1.0; 6],
            joint_vel: [2.0; 6],
            joint_current: [3.0; 6],
            end_pose: [4.0; 6],
            timestamp: 1000,
            time_diff_us: 500,
        };
        let result_ok = AlignmentResult::Ok(state);
        let debug_str = format!("{:?}", result_ok);
        assert!(debug_str.contains("Ok") || debug_str.contains("AlignmentResult"));

        let state2 = AlignedMotionState {
            joint_pos: [1.0; 6],
            joint_vel: [2.0; 6],
            joint_current: [3.0; 6],
            end_pose: [4.0; 6],
            timestamp: 1000,
            time_diff_us: 500,
        };
        let result_mis = AlignmentResult::Misaligned {
            state: state2,
            diff_us: 10000,
        };
        let debug_str2 = format!("{:?}", result_mis);
        assert!(debug_str2.contains("Misaligned") || debug_str2.contains("AlignmentResult"));
    }
}
