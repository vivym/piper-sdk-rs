//! Robot 模块状态结构定义

use crate::robot::fps_stats::FpsStatistics;
use arc_swap::ArcSwap;
use std::sync::{Arc, RwLock};

/// 关节位置状态（帧组同步）
///
/// 更新频率：~500Hz
/// CAN ID：0x2A5-0x2A7
#[derive(Debug, Clone, Default)]
pub struct JointPositionState {
    /// 硬件时间戳（微秒，来自完整帧组的最后一帧）
    ///
    /// **注意**：这是CAN硬件时间戳，反映数据在CAN总线上的实际传输时间。
    pub hardware_timestamp_us: u64,

    /// 系统接收时间戳（微秒，系统接收到完整帧组的时间）
    ///
    /// **注意**：这是系统时间戳，用于计算接收延迟和系统处理时间。
    pub system_timestamp_us: u64,

    /// 关节位置（弧度）[J1, J2, J3, J4, J5, J6]
    pub joint_pos: [f64; 6],

    /// 帧组有效性掩码（Bit 0-2 对应 0x2A5, 0x2A6, 0x2A7）
    /// - 1 表示该CAN帧已收到
    /// - 0 表示该CAN帧未收到（可能丢包）
    pub frame_valid_mask: u8,
}

impl JointPositionState {
    /// 检查是否接收到了完整的帧组 (0x2A5, 0x2A6, 0x2A7)
    ///
    /// **返回值**：
    /// - `true`：所有3个CAN帧都已收到，数据完整
    /// - `false`：部分CAN帧丢失，数据不完整
    pub fn is_fully_valid(&self) -> bool {
        self.frame_valid_mask == 0b0000_0111 // Bit 0-2 全部为 1
    }

    /// 获取丢失的CAN帧索引（用于调试）
    ///
    /// **返回值**：丢失的CAN帧索引列表（0=0x2A5, 1=0x2A6, 2=0x2A7）
    pub fn missing_frames(&self) -> Vec<usize> {
        (0..3).filter(|&i| (self.frame_valid_mask & (1 << i)) == 0).collect()
    }
}

/// 末端位姿状态（帧组同步）
///
/// 更新频率：~500Hz
/// CAN ID：0x2A2-0x2A4
#[derive(Debug, Clone, Default)]
pub struct EndPoseState {
    /// 硬件时间戳（微秒，来自完整帧组的最后一帧）
    pub hardware_timestamp_us: u64,

    /// 系统接收时间戳（微秒，系统接收到完整帧组的时间）
    pub system_timestamp_us: u64,

    /// 末端位姿 [X, Y, Z, Rx, Ry, Rz]
    /// - X, Y, Z: 位置（米）
    ///   - **注意**：`EndPoseFeedback1.x()`, `.y()`, `EndPoseFeedback2.z()` 返回的是**毫米**，需要除以 1000.0 转换为米
    /// - Rx, Ry, Rz: 姿态角（弧度，欧拉角或旋转向量）
    pub end_pose: [f64; 6],

    /// 帧组有效性掩码（Bit 0-2 对应 0x2A2, 0x2A3, 0x2A4）
    pub frame_valid_mask: u8,
}

impl EndPoseState {
    /// 检查是否接收到了完整的帧组 (0x2A2, 0x2A3, 0x2A4)
    ///
    /// **返回值**：
    /// - `true`：所有3个CAN帧都已收到，数据完整
    /// - `false`：部分CAN帧丢失，数据不完整
    pub fn is_fully_valid(&self) -> bool {
        self.frame_valid_mask == 0b0000_0111 // Bit 0-2 全部为 1
    }

    /// 获取丢失的CAN帧索引（用于调试）
    ///
    /// **返回值**：丢失的CAN帧索引列表（0=0x2A2, 1=0x2A3, 2=0x2A4）
    pub fn missing_frames(&self) -> Vec<usize> {
        (0..3).filter(|&i| (self.frame_valid_mask & (1 << i)) == 0).collect()
    }
}

/// 运动状态快照（逻辑原子性）
///
/// 用于在同一时刻捕获多个运动相关状态，保证逻辑上的原子性。
/// 这是一个栈上对象（Stack Allocated），开销极小。
#[derive(Debug, Clone)]
pub struct MotionSnapshot {
    /// 关节位置状态
    pub joint_position: JointPositionState,

    /// 末端位姿状态
    pub end_pose: EndPoseState,
    // 关节动态状态（可选，未来可能需要）
    // pub joint_dynamic: JointDynamicState,
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
    ///
    /// **注意**：扭矩可以通过 `get_torque(joint_index)` 方法从电流值实时计算得到。
    /// - 关节 1-3 (J1, J2, J3): `torque = current * COEFFICIENT_1_3` (1.18125)
    /// - 关节 4-6 (J4, J5, J6): `torque = current * COEFFICIENT_4_6` (0.95844)
    pub joint_current: [f64; 6],

    /// 每个关节的具体更新时间（用于调试或高阶插值）
    pub timestamps: [u64; 6],

    /// 有效性掩码（Bit 0-5 对应 Joint 1-6）
    /// - 1 表示本周期内已更新
    /// - 0 表示未更新（可能是丢帧）
    pub valid_mask: u8,
}

impl JointDynamicState {
    /// 关节 1-3 的力矩系数（CAN ID: 0x251~0x253）
    ///
    /// 根据官方参考实现，关节 1、2、3 使用此系数计算力矩。
    /// 公式：torque = current * COEFFICIENT_1_3
    pub const COEFFICIENT_1_3: f64 = 1.18125;

    /// 关节 4-6 的力矩系数（CAN ID: 0x254~0x256）
    ///
    /// 根据官方参考实现，关节 4、5、6 使用此系数计算力矩。
    /// 公式：torque = current * COEFFICIENT_4_6
    pub const COEFFICIENT_4_6: f64 = 0.95844;

    /// 根据关节索引和电流值计算扭矩（N·m）
    ///
    /// # 参数
    /// - `joint_index`: 关节索引（0-5，对应 J1-J6）
    /// - `current`: 电流值（A）
    ///
    /// # 返回值
    /// 计算得到的力矩值（N·m）
    ///
    /// # 示例
    /// ```rust
    /// # use piper_sdk::robot::JointDynamicState;
    /// // 计算 J1（索引 0）的扭矩，电流为 2.0A
    /// let torque = JointDynamicState::calculate_torque(0, 2.0);
    /// // 结果：2.0 * 1.18125 = 2.3625 N·m
    /// ```
    pub fn calculate_torque(joint_index: usize, current: f64) -> f64 {
        let coefficient = if joint_index < 3 {
            Self::COEFFICIENT_1_3
        } else {
            Self::COEFFICIENT_4_6
        };
        current * coefficient
    }

    /// 获取指定关节的扭矩（N·m）
    ///
    /// 从当前存储的电流值实时计算扭矩，无需额外存储空间。
    ///
    /// # 参数
    /// - `joint_index`: 关节索引（0-5，对应 J1-J6）
    ///
    /// # 返回值
    /// 关节扭矩值（N·m），如果索引超出范围则返回 0.0
    ///
    /// # 示例
    /// ```rust
    /// # use piper_sdk::robot::JointDynamicState;
    /// let mut state = JointDynamicState::default();
    /// state.joint_current[0] = 2.0; // 设置 J1 的电流为 2.0A
    /// let torque_j1 = state.get_torque(0); // 获取 J1 的扭矩：2.0 * 1.18125 = 2.3625 N·m
    /// ```
    pub fn get_torque(&self, joint_index: usize) -> f64 {
        if joint_index < 6 {
            Self::calculate_torque(joint_index, self.joint_current[joint_index])
        } else {
            0.0
        }
    }

    /// 获取所有关节的扭矩（N·m）
    ///
    /// 一次性计算并返回所有6个关节的扭矩值，比多次调用 `get_torque()` 更高效。
    ///
    /// # 返回值
    /// 包含所有关节扭矩的数组 `[J1, J2, J3, J4, J5, J6]`（N·m）
    ///
    /// # 示例
    /// ```rust
    /// # use piper_sdk::robot::JointDynamicState;
    /// let mut state = JointDynamicState::default();
    /// state.joint_current = [1.0, 2.0, 0.5, 1.0, 2.0, 0.5];
    /// let all_torques = state.get_all_torques();
    /// // all_torques[0] = 1.0 * 1.18125 = 1.18125 N·m (J1)
    /// // all_torques[3] = 1.0 * 0.95844 = 0.95844 N·m (J4)
    /// ```
    pub fn get_all_torques(&self) -> [f64; 6] {
        [
            Self::calculate_torque(0, self.joint_current[0]),
            Self::calculate_torque(1, self.joint_current[1]),
            Self::calculate_torque(2, self.joint_current[2]),
            Self::calculate_torque(3, self.joint_current[3]),
            Self::calculate_torque(4, self.joint_current[4]),
            Self::calculate_torque(5, self.joint_current[5]),
        ]
    }

    /// 检查所有关节是否都已更新（`valid_mask == 0x3F`）
    pub fn is_complete(&self) -> bool {
        self.valid_mask == 0b111111
    }

    /// 获取未更新的关节索引（用于调试）
    pub fn missing_joints(&self) -> Vec<usize> {
        (0..6).filter(|&i| (self.valid_mask & (1 << i)) == 0).collect()
    }
}

/// 机器人控制状态
///
/// 更新频率：~200Hz
/// CAN ID：0x2A1
#[derive(Debug, Clone, Default)]
pub struct RobotControlState {
    /// 硬件时间戳（微秒）
    pub hardware_timestamp_us: u64,

    /// 系统接收时间戳（微秒）
    pub system_timestamp_us: u64,

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

    /// 故障码：角度超限位（位掩码，Bit 0-5 对应 J1-J6）
    ///
    /// **优化**：使用位掩码而非 `[bool; 6]`，节省内存并提高Cache Locality
    pub fault_angle_limit_mask: u8,

    /// 故障码：通信异常（位掩码，Bit 0-5 对应 J1-J6）
    pub fault_comm_error_mask: u8,

    /// 使能状态（从 robot_status 推导）
    pub is_enabled: bool,

    /// 反馈指令计数器（如果协议支持，用于检测链路卡死）
    ///
    /// **注意**：如果协议中没有循环计数器，此字段为 0
    pub feedback_counter: u8,
}

impl RobotControlState {
    /// 检查指定关节是否角度超限位
    pub fn is_angle_limit(&self, joint_index: usize) -> bool {
        if joint_index >= 6 {
            return false;
        }
        (self.fault_angle_limit_mask >> joint_index) & 1 == 1
    }

    /// 检查指定关节是否通信异常
    pub fn is_comm_error(&self, joint_index: usize) -> bool {
        if joint_index >= 6 {
            return false;
        }
        (self.fault_comm_error_mask >> joint_index) & 1 == 1
    }
}

/// 夹爪状态
///
/// 更新频率：~200Hz
/// CAN ID：0x2A8
#[derive(Debug, Clone, Default)]
pub struct GripperState {
    /// 硬件时间戳（微秒）
    pub hardware_timestamp_us: u64,

    /// 系统接收时间戳（微秒）
    pub system_timestamp_us: u64,

    /// 夹爪行程（mm）
    pub travel: f64,

    /// 夹爪扭矩（N·m）
    pub torque: f64,

    /// 夹爪状态码（原始状态字节，来自 0x2A8 Byte 6）
    ///
    /// **优化**：保持原始数据的纯度，通过方法解析状态位
    pub status_code: u8,

    /// 上次行程值（用于计算是否在运动）
    ///
    /// **注意**：用于判断夹爪是否在运动（通过 travel 变化率推算）
    pub last_travel: f64,
}

impl GripperState {
    /// 检查电压是否过低
    pub fn is_voltage_low(&self) -> bool {
        self.status_code & 1 == 1
    }

    /// 检查电机是否过温
    pub fn is_motor_over_temp(&self) -> bool {
        (self.status_code >> 1) & 1 == 1
    }

    /// 检查是否过流
    pub fn is_over_current(&self) -> bool {
        (self.status_code >> 2) & 1 == 1
    }

    /// 检查驱动器是否过温
    pub fn is_driver_over_temp(&self) -> bool {
        (self.status_code >> 3) & 1 == 1
    }

    /// 检查传感器是否异常
    pub fn is_sensor_error(&self) -> bool {
        (self.status_code >> 4) & 1 == 1
    }

    /// 检查驱动器是否错误
    pub fn is_driver_error(&self) -> bool {
        (self.status_code >> 5) & 1 == 1
    }

    /// 检查是否使能
    pub fn is_enabled(&self) -> bool {
        (self.status_code >> 6) & 1 == 1
    }

    /// 检查是否已回零
    pub fn is_homed(&self) -> bool {
        (self.status_code >> 7) & 1 == 1
    }

    /// 检查夹爪是否在运动（通过 travel 变化率判断）
    ///
    /// **阈值**：如果 travel 变化超过 0.1mm，认为在运动
    pub fn is_moving(&self) -> bool {
        (self.travel - self.last_travel).abs() > 0.1
    }
}

/// 关节驱动器低速反馈状态
///
/// 更新频率：~40Hz
/// CAN ID：0x261-0x266（每个关节一个CAN ID）
/// 同步机制：ArcSwap（Wait-Free，适合高频读）
///
/// **优化**：使用位掩码而非 `[bool; 6]`，节省内存并提高Cache Locality
#[derive(Debug, Clone, Default)]
pub struct JointDriverLowSpeedState {
    /// 硬件时间戳（微秒，来自最新一帧）
    pub hardware_timestamp_us: u64,

    /// 系统接收时间戳（微秒）
    pub system_timestamp_us: u64,

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

    // === 驱动器状态（来自 0x261-0x266，位掩码优化） ===
    /// 驱动器状态：电压过低（位掩码，Bit 0-5 对应 J1-J6）
    pub driver_voltage_low_mask: u8,
    /// 驱动器状态：电机过温（位掩码，Bit 0-5 对应 J1-J6）
    pub driver_motor_over_temp_mask: u8,
    /// 驱动器状态：过流（位掩码，Bit 0-5 对应 J1-J6）
    pub driver_over_current_mask: u8,
    /// 驱动器状态：驱动器过温（位掩码，Bit 0-5 对应 J1-J6）
    pub driver_over_temp_mask: u8,
    /// 驱动器状态：碰撞保护触发（位掩码，Bit 0-5 对应 J1-J6）
    pub driver_collision_protection_mask: u8,
    /// 驱动器状态：驱动器错误（位掩码，Bit 0-5 对应 J1-J6）
    pub driver_error_mask: u8,
    /// 驱动器状态：使能状态（位掩码，Bit 0-5 对应 J1-J6）
    pub driver_enabled_mask: u8,
    /// 驱动器状态：堵转保护触发（位掩码，Bit 0-5 对应 J1-J6）
    pub driver_stall_protection_mask: u8,

    // === 时间戳（每个关节独立） ===
    /// 每个关节的硬件时间戳（微秒）[J1, J2, J3, J4, J5, J6]
    pub hardware_timestamps: [u64; 6],
    /// 每个关节的系统接收时间戳（微秒）[J1, J2, J3, J4, J5, J6]
    pub system_timestamps: [u64; 6],

    // === 有效性掩码 ===
    /// 有效性掩码（Bit 0-5 对应 J1-J6）
    /// - 1 表示本周期内已更新
    /// - 0 表示未更新（可能是丢帧）
    pub valid_mask: u8,
}

impl JointDriverLowSpeedState {
    /// 检查所有关节是否都已更新（`valid_mask == 0x3F`）
    pub fn is_fully_valid(&self) -> bool {
        self.valid_mask == 0b111111
    }

    /// 获取未更新的关节索引（用于调试）
    pub fn missing_joints(&self) -> Vec<usize> {
        (0..6).filter(|&i| (self.valid_mask & (1 << i)) == 0).collect()
    }

    /// 检查指定关节是否电压过低
    pub fn is_voltage_low(&self, joint_index: usize) -> bool {
        if joint_index >= 6 {
            return false;
        }
        (self.driver_voltage_low_mask >> joint_index) & 1 == 1
    }

    /// 检查指定关节是否电机过温
    pub fn is_motor_over_temp(&self, joint_index: usize) -> bool {
        if joint_index >= 6 {
            return false;
        }
        (self.driver_motor_over_temp_mask >> joint_index) & 1 == 1
    }

    /// 检查指定关节是否过流
    pub fn is_over_current(&self, joint_index: usize) -> bool {
        if joint_index >= 6 {
            return false;
        }
        (self.driver_over_current_mask >> joint_index) & 1 == 1
    }

    /// 检查指定关节是否驱动器过温
    pub fn is_driver_over_temp(&self, joint_index: usize) -> bool {
        if joint_index >= 6 {
            return false;
        }
        (self.driver_over_temp_mask >> joint_index) & 1 == 1
    }

    /// 检查指定关节是否碰撞保护触发
    pub fn is_collision_protection(&self, joint_index: usize) -> bool {
        if joint_index >= 6 {
            return false;
        }
        (self.driver_collision_protection_mask >> joint_index) & 1 == 1
    }

    /// 检查指定关节是否驱动器错误
    pub fn is_driver_error(&self, joint_index: usize) -> bool {
        if joint_index >= 6 {
            return false;
        }
        (self.driver_error_mask >> joint_index) & 1 == 1
    }

    /// 检查指定关节是否使能
    pub fn is_enabled(&self, joint_index: usize) -> bool {
        if joint_index >= 6 {
            return false;
        }
        (self.driver_enabled_mask >> joint_index) & 1 == 1
    }

    /// 检查指定关节是否堵转保护触发
    pub fn is_stall_protection(&self, joint_index: usize) -> bool {
        if joint_index >= 6 {
            return false;
        }
        (self.driver_stall_protection_mask >> joint_index) & 1 == 1
    }
}

/// 碰撞保护状态（冷数据）
///
/// 更新频率：按需查询（通常只在设置碰撞保护等级后收到反馈）
/// CAN ID：0x47B
/// 同步机制：RwLock（按需查询，更新频率极低）
///
/// **注意**：碰撞保护等级范围是 0-8，其中 0 表示不检测碰撞。
#[derive(Debug, Clone, Default)]
pub struct CollisionProtectionState {
    /// 硬件时间戳（微秒，来自 CAN 帧）
    pub hardware_timestamp_us: u64,

    /// 系统接收时间戳（微秒）
    pub system_timestamp_us: u64,

    /// 各关节碰撞保护等级（0-8）[J1, J2, J3, J4, J5, J6]
    ///
    /// **注意**：
    /// - 0：不检测碰撞
    /// - 1-8：碰撞保护等级（数字越大，保护越严格）
    pub protection_levels: [u8; 6],
}

/// 关节限制配置状态（冷数据）
///
/// 更新频率：按需查询（需要查询6次，每个关节一次）
/// CAN ID：0x473（MotorLimitFeedback）
/// 同步机制：RwLock（按需查询，更新频率极低）
///
/// **注意**：
/// - `MotorLimitFeedback.max_angle()` 和 `.min_angle()` 返回的是**度**，需要转换为弧度。
/// - `MotorLimitFeedback.max_velocity()` 已经返回 rad/s，无需转换。
#[derive(Debug, Clone, Default)]
pub struct JointLimitConfigState {
    /// 最后更新时间戳（硬件时间戳，微秒）
    pub last_update_hardware_timestamp_us: u64,

    /// 最后更新时间戳（系统时间戳，微秒）
    pub last_update_system_timestamp_us: u64,

    // === 关节限制配置（来自 0x473，需要查询6次） ===
    /// 关节角度上限（弧度）[J1, J2, J3, J4, J5, J6]
    pub joint_limits_max: [f64; 6],
    /// 关节角度下限（弧度）[J1, J2, J3, J4, J5, J6]
    pub joint_limits_min: [f64; 6],
    /// 各关节最大速度（rad/s）[J1, J2, J3, J4, J5, J6]
    pub joint_max_velocity: [f64; 6],

    // === 时间戳（每个关节独立） ===
    /// 每个关节的硬件时间戳（微秒）[J1, J2, J3, J4, J5, J6]
    pub joint_update_hardware_timestamps: [u64; 6],
    /// 每个关节的系统接收时间戳（微秒）[J1, J2, J3, J4, J5, J6]
    pub joint_update_system_timestamps: [u64; 6],

    // === 有效性掩码 ===
    /// 有效性掩码（Bit 0-5 对应 J1-J6）
    /// - 1 表示该关节的配置已更新
    /// - 0 表示该关节的配置未更新（可能未查询）
    pub valid_mask: u8,
}

impl JointLimitConfigState {
    /// 检查所有关节是否都已更新（`valid_mask == 0x3F`）
    pub fn is_fully_valid(&self) -> bool {
        self.valid_mask == 0b111111
    }

    /// 获取未更新的关节索引（用于调试）
    pub fn missing_joints(&self) -> Vec<usize> {
        (0..6).filter(|&i| (self.valid_mask & (1 << i)) == 0).collect()
    }
}

/// 关节加速度限制配置状态（冷数据）
///
/// 更新频率：按需查询（需要查询6次，每个关节一次）
/// CAN ID：0x47C（MotorMaxAccelFeedback）
/// 同步机制：RwLock（按需查询，更新频率极低）
///
/// **注意**：`MotorMaxAccelFeedback.max_accel()` 已经返回 rad/s²，无需转换。
#[derive(Debug, Clone, Default)]
pub struct JointAccelConfigState {
    /// 最后更新时间戳（硬件时间戳，微秒）
    pub last_update_hardware_timestamp_us: u64,

    /// 最后更新时间戳（系统时间戳，微秒）
    pub last_update_system_timestamp_us: u64,

    // === 关节加速度限制配置（来自 0x47C，需要查询6次） ===
    /// 各关节最大加速度（rad/s²）[J1, J2, J3, J4, J5, J6]
    pub max_acc_limits: [f64; 6],

    // === 时间戳（每个关节独立） ===
    /// 每个关节的硬件时间戳（微秒）[J1, J2, J3, J4, J5, J6]
    pub joint_update_hardware_timestamps: [u64; 6],
    /// 每个关节的系统接收时间戳（微秒）[J1, J2, J3, J4, J5, J6]
    pub joint_update_system_timestamps: [u64; 6],

    // === 有效性掩码 ===
    /// 有效性掩码（Bit 0-5 对应 J1-J6）
    /// - 1 表示该关节的配置已更新
    /// - 0 表示该关节的配置未更新（可能未查询）
    pub valid_mask: u8,
}

impl JointAccelConfigState {
    /// 检查所有关节是否都已更新（`valid_mask == 0x3F`）
    pub fn is_fully_valid(&self) -> bool {
        self.valid_mask == 0b111111
    }

    /// 获取未更新的关节索引（用于调试）
    pub fn missing_joints(&self) -> Vec<usize> {
        (0..6).filter(|&i| (self.valid_mask & (1 << i)) == 0).collect()
    }
}

/// 末端限制配置状态（冷数据）
///
/// 更新频率：按需查询（单帧响应）
/// CAN ID：0x478（EndVelocityAccelFeedback）
/// 同步机制：RwLock（按需查询，更新频率极低）
///
/// **注意**：所有字段的单位已经在协议层转换完成，无需额外转换。
#[derive(Debug, Clone, Default)]
pub struct EndLimitConfigState {
    /// 最后更新时间戳（硬件时间戳，微秒）
    pub last_update_hardware_timestamp_us: u64,

    /// 最后更新时间戳（系统时间戳，微秒）
    pub last_update_system_timestamp_us: u64,

    // === 末端限制配置（来自 0x478，单帧响应） ===
    /// 末端最大线速度（m/s）
    pub max_end_linear_velocity: f64,
    /// 末端最大角速度（rad/s）
    pub max_end_angular_velocity: f64,
    /// 末端最大线加速度（m/s²）
    pub max_end_linear_accel: f64,
    /// 末端最大角加速度（rad/s²）
    pub max_end_angular_accel: f64,

    // === 有效性标记 ===
    /// 是否已更新（单帧响应，收到即有效）
    pub is_valid: bool,
}

// ============================================================================
// 固件版本状态
// ============================================================================

/// 固件版本状态
///
/// 更新频率：按需查询
/// CAN ID：0x4AF（多帧累积）
/// 同步机制：RwLock（冷数据，更新频率低）
#[derive(Debug, Clone, Default)]
pub struct FirmwareVersionState {
    /// 硬件时间戳（微秒，最后一帧的时间）
    pub hardware_timestamp_us: u64,

    /// 系统接收时间戳（微秒）
    pub system_timestamp_us: u64,

    /// 累积的固件数据（字节数组）
    /// 注意：版本字符串需要从累积数据中解析
    pub firmware_data: Vec<u8>,

    /// 是否已收到完整数据
    /// 注意：判断条件需要根据实际情况确定（例如收到特定结束标记）
    pub is_complete: bool,

    /// 解析后的版本字符串（缓存）
    /// 如果 firmware_data 中包含有效的版本字符串，这里存储解析结果
    pub version_string: Option<String>,
}

impl FirmwareVersionState {
    /// 尝试从累积数据中解析版本字符串
    pub fn parse_version(&mut self) -> Option<String> {
        // 导入 FirmwareReadFeedback 的 parse_version_string 方法
        use crate::protocol::feedback::FirmwareReadFeedback;
        if let Some(version) = FirmwareReadFeedback::parse_version_string(&self.firmware_data) {
            self.version_string = Some(version.clone());
            Some(version)
        } else {
            None
        }
    }

    /// 获取版本字符串（如果已解析）
    pub fn version_string(&self) -> Option<&String> {
        self.version_string.as_ref()
    }
}

// ============================================================================
// 主从模式控制指令状态
// ============================================================================

/// 主从模式控制模式指令状态
///
/// 更新频率：~200Hz（取决于主臂发送频率）
/// CAN ID：0x151
/// 同步机制：ArcSwap（温数据，高频访问）
#[derive(Debug, Clone, Default)]
pub struct MasterSlaveControlModeState {
    /// 硬件时间戳（微秒）
    pub hardware_timestamp_us: u64,

    /// 系统接收时间戳（微秒）
    pub system_timestamp_us: u64,

    /// 控制模式指令（来自 0x151）
    pub control_mode: u8, // ControlModeCommand as u8
    pub move_mode: u8, // MoveMode as u8
    pub speed_percent: u8,
    pub mit_mode: u8, // MitMode as u8
    pub trajectory_stay_time: u8,
    pub install_position: u8, // InstallPosition as u8

    /// 是否有效（已收到至少一帧）
    pub is_valid: bool,
}

/// 主从模式关节控制指令状态（帧组同步）
///
/// 更新频率：~500Hz（取决于主臂发送频率）
/// CAN ID：0x155-0x157（帧组：J1-J2, J3-J4, J5-J6）
/// 同步机制：ArcSwap（温数据，帧组同步）
///
/// **注意**：这是一个帧组，类似于 `JointPositionState`，需要集齐 3 帧后一起提交
#[derive(Debug, Clone, Default)]
pub struct MasterSlaveJointControlState {
    /// 硬件时间戳（微秒，来自完整帧组的最后一帧）
    pub hardware_timestamp_us: u64,

    /// 系统接收时间戳（微秒，系统接收到完整帧组的时间）
    pub system_timestamp_us: u64,

    /// 关节目标角度（度，0.001°单位）[J1, J2, J3, J4, J5, J6]
    pub joint_target_deg: [i32; 6],

    /// 帧组有效性掩码（Bit 0-2 对应 0x155, 0x156, 0x157）
    /// - 1 表示该CAN帧已收到
    /// - 0 表示该CAN帧未收到（可能丢包）
    pub frame_valid_mask: u8,
}

impl MasterSlaveJointControlState {
    /// 检查是否接收到了完整的帧组 (0x155, 0x156, 0x157)
    ///
    /// **返回值**：
    /// - `true`：所有3个CAN帧都已收到，数据完整
    /// - `false`：部分CAN帧丢失，数据不完整
    pub fn is_fully_valid(&self) -> bool {
        self.frame_valid_mask == 0b0000_0111 // Bit 0-2 全部为 1
    }

    /// 获取丢失的CAN帧索引（用于调试）
    ///
    /// **返回值**：丢失的CAN帧索引列表（0=0x155, 1=0x156, 2=0x157）
    pub fn missing_frames(&self) -> Vec<usize> {
        (0..3).filter(|&i| (self.frame_valid_mask & (1 << i)) == 0).collect()
    }

    /// 获取关节目标角度（度）
    pub fn joint_target_deg(&self, joint_index: usize) -> Option<f64> {
        if joint_index < 6 {
            Some(self.joint_target_deg[joint_index] as f64 / 1000.0)
        } else {
            None
        }
    }

    /// 获取关节目标角度（弧度）
    pub fn joint_target_rad(&self, joint_index: usize) -> Option<f64> {
        self.joint_target_deg(joint_index).map(|deg| deg * std::f64::consts::PI / 180.0)
    }
}

/// 主从模式夹爪控制指令状态
///
/// 更新频率：~200Hz（取决于主臂发送频率）
/// CAN ID：0x159
/// 同步机制：ArcSwap（温数据，高频访问）
#[derive(Debug, Clone, Default)]
pub struct MasterSlaveGripperControlState {
    /// 硬件时间戳（微秒）
    pub hardware_timestamp_us: u64,

    /// 系统接收时间戳（微秒）
    pub system_timestamp_us: u64,

    /// 夹爪目标行程（mm，0.001mm单位）
    pub gripper_target_travel_mm: i32,

    /// 夹爪目标扭矩（N·m，0.001N·m单位）
    pub gripper_target_torque_nm: i16,

    /// 夹爪状态码
    pub gripper_status_code: u8,

    /// 夹爪回零设置
    pub gripper_set_zero: u8,

    /// 是否有效（已收到至少一帧）
    pub is_valid: bool,
}

impl MasterSlaveGripperControlState {
    /// 获取夹爪目标行程（mm）
    pub fn gripper_target_travel(&self) -> f64 {
        self.gripper_target_travel_mm as f64 / 1000.0
    }

    /// 获取夹爪目标扭矩（N·m）
    pub fn gripper_target_torque(&self) -> f64 {
        self.gripper_target_torque_nm as f64 / 1000.0
    }
}

/// Piper 上下文（所有状态的聚合）
pub struct PiperContext {
    // === 热数据（500Hz，高频运动数据）===
    // 使用 ArcSwap，无锁读取，适合高频控制循环
    /// 关节位置状态（帧组同步：0x2A5-0x2A7）
    pub joint_position: Arc<ArcSwap<JointPositionState>>,
    /// 末端位姿状态（帧组同步：0x2A2-0x2A4）
    pub end_pose: Arc<ArcSwap<EndPoseState>>,
    /// 关节动态状态（独立帧 + Buffered Commit：关节速度 + 电流）
    pub joint_dynamic: Arc<ArcSwap<JointDynamicState>>,

    // === 温数据（200Hz，控制状态）===
    // 使用 ArcSwap，更新频率中等，但需要原子性
    /// 机器人控制状态（单个CAN帧：0x2A1）
    pub robot_control: Arc<ArcSwap<RobotControlState>>,

    /// 夹爪状态（单个CAN帧：0x2A8）
    pub gripper: Arc<ArcSwap<GripperState>>,

    // === 温数据（40Hz，诊断数据）===
    // 使用 ArcSwap，Wait-Free 读取，适合高频读
    /// 关节驱动器低速反馈状态（单个CAN帧：0x261-0x266）
    pub joint_driver_low_speed: Arc<ArcSwap<JointDriverLowSpeedState>>,

    // === 冷数据（10Hz 或按需，诊断和配置）===
    // 使用 RwLock，读取频率低，避免内存分配
    /// 碰撞保护状态（按需查询：0x47B）
    pub collision_protection: Arc<RwLock<CollisionProtectionState>>,

    /// 关节限制配置状态（按需查询：0x473）
    pub joint_limit_config: Arc<RwLock<JointLimitConfigState>>,

    /// 关节加速度限制配置状态（按需查询：0x47C）
    pub joint_accel_config: Arc<RwLock<JointAccelConfigState>>,

    /// 末端限制配置状态（按需查询：0x478）
    pub end_limit_config: Arc<RwLock<EndLimitConfigState>>,

    // === 冷数据（固件版本）===
    /// 固件版本状态（按需查询：0x4AF）
    pub firmware_version: Arc<RwLock<FirmwareVersionState>>,

    // === 温数据（主从模式）===
    /// 主从模式控制模式指令状态（主从模式：0x151）
    pub master_slave_control_mode: Arc<ArcSwap<MasterSlaveControlModeState>>,

    /// 主从模式关节控制指令状态（主从模式：0x155-0x157，帧组同步）
    pub master_slave_joint_control: Arc<ArcSwap<MasterSlaveJointControlState>>,

    /// 主从模式夹爪控制指令状态（主从模式：0x159）
    pub master_slave_gripper_control: Arc<ArcSwap<MasterSlaveGripperControlState>>,

    // === FPS 统计 ===
    // 使用原子计数器，无锁读取，适合实时监控
    /// FPS 统计（各状态的更新频率统计）
    pub fps_stats: Arc<FpsStatistics>,
}

impl PiperContext {
    /// 创建新的上下文
    ///
    /// 初始化所有状态结构，包括：
    /// - 热数据（ArcSwap）：`joint_position`, `end_pose`, `joint_dynamic`
    /// - 温数据（ArcSwap）：`robot_control`, `gripper`, `joint_driver_low_speed`
    /// - 冷数据（RwLock）：`collision_protection`, `joint_limit_config`, `joint_accel_config`, `end_limit_config`
    /// - FPS 统计：`fps_stats`
    ///
    /// # Example
    ///
    /// ```
    /// use piper_sdk::robot::PiperContext;
    ///
    /// let ctx = PiperContext::new();
    /// let joint_pos = ctx.joint_position.load();
    /// assert_eq!(joint_pos.hardware_timestamp_us, 0);
    /// ```
    pub fn new() -> Self {
        Self {
            // 热数据：ArcSwap，无锁读取
            joint_position: Arc::new(ArcSwap::from_pointee(JointPositionState::default())),
            end_pose: Arc::new(ArcSwap::from_pointee(EndPoseState::default())),
            joint_dynamic: Arc::new(ArcSwap::from_pointee(JointDynamicState::default())),

            // 温数据：ArcSwap
            robot_control: Arc::new(ArcSwap::from_pointee(RobotControlState::default())),
            gripper: Arc::new(ArcSwap::from_pointee(GripperState::default())),
            joint_driver_low_speed: Arc::new(ArcSwap::from_pointee(
                JointDriverLowSpeedState::default(),
            )),

            // 冷数据：RwLock
            collision_protection: Arc::new(RwLock::new(CollisionProtectionState::default())),
            joint_limit_config: Arc::new(RwLock::new(JointLimitConfigState::default())),
            joint_accel_config: Arc::new(RwLock::new(JointAccelConfigState::default())),
            end_limit_config: Arc::new(RwLock::new(EndLimitConfigState::default())),

            // 冷数据：固件版本
            firmware_version: Arc::new(RwLock::new(FirmwareVersionState::default())),

            // 温数据：主从模式控制指令状态
            master_slave_control_mode: Arc::new(ArcSwap::from_pointee(
                MasterSlaveControlModeState::default(),
            )),
            master_slave_joint_control: Arc::new(ArcSwap::from_pointee(
                MasterSlaveJointControlState::default(),
            )),
            master_slave_gripper_control: Arc::new(ArcSwap::from_pointee(
                MasterSlaveGripperControlState::default(),
            )),

            // FPS 统计：原子计数器
            fps_stats: Arc::new(FpsStatistics::new()),
        }
    }

    /// 捕获运动状态快照（逻辑原子性）
    ///
    /// 虽然不能保证物理上的完全同步（因为CAN帧本身就不是同时到的），
    /// 但可以保证逻辑上的原子性（在同一时刻读取多个状态）。
    ///
    /// **注意**：返回的状态可能来自不同的CAN传输周期。
    ///
    /// **性能**：返回栈上对象，开销极小（仅包含 Arc 的克隆，不复制实际数据）
    ///
    /// # Example
    ///
    /// ```
    /// use piper_sdk::robot::PiperContext;
    ///
    /// let ctx = PiperContext::new();
    /// let snapshot = ctx.capture_motion_snapshot();
    /// println!("Joint positions: {:?}", snapshot.joint_position.joint_pos);
    /// println!("End pose: {:?}", snapshot.end_pose.end_pose);
    /// ```
    pub fn capture_motion_snapshot(&self) -> MotionSnapshot {
        MotionSnapshot {
            joint_position: self.joint_position.load().as_ref().clone(),
            end_pose: self.end_pose.load().as_ref().clone(),
        }
    }
}

impl Default for PiperContext {
    fn default() -> Self {
        Self::new()
    }
}

/// 组合运动状态（所有热数据）
pub struct CombinedMotionState {
    pub joint_position: JointPositionState,
    pub end_pose: EndPoseState,
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
    use super::{EndPoseState, JointPositionState, MotionSnapshot, PiperContext};
    use std::f64::consts::PI;

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
    fn test_joint_dynamic_state_calculate_torque() {
        // 测试关节 1-3（使用 COEFFICIENT_1_3 = 1.18125）
        let torque_j1 = JointDynamicState::calculate_torque(0, 1.0);
        assert!((torque_j1 - 1.18125).abs() < 0.0001);

        let torque_j2 = JointDynamicState::calculate_torque(1, 2.0);
        assert!((torque_j2 - 2.3625).abs() < 0.0001);

        let torque_j3 = JointDynamicState::calculate_torque(2, 0.5);
        assert!((torque_j3 - 0.590625).abs() < 0.0001);

        // 测试关节 4-6（使用 COEFFICIENT_4_6 = 0.95844）
        let torque_j4 = JointDynamicState::calculate_torque(3, 1.0);
        assert!((torque_j4 - 0.95844).abs() < 0.0001);

        let torque_j5 = JointDynamicState::calculate_torque(4, 2.0);
        assert!((torque_j5 - 1.91688).abs() < 0.0001);

        let torque_j6 = JointDynamicState::calculate_torque(5, 0.5);
        assert!((torque_j6 - 0.47922).abs() < 0.0001);
    }

    #[test]
    fn test_joint_dynamic_state_get_torque() {
        let state = JointDynamicState {
            joint_current: [1.0, 2.0, 0.5, 1.0, 2.0, 0.5],
            ..Default::default()
        };

        // 测试关节 1-3（使用 COEFFICIENT_1_3 = 1.18125）
        assert!((state.get_torque(0) - 1.18125).abs() < 0.0001); // 1.0 * 1.18125
        assert!((state.get_torque(1) - 2.3625).abs() < 0.0001); // 2.0 * 1.18125
        assert!((state.get_torque(2) - 0.590625).abs() < 0.0001); // 0.5 * 1.18125

        // 测试关节 4-6（使用 COEFFICIENT_4_6 = 0.95844）
        assert!((state.get_torque(3) - 0.95844).abs() < 0.0001); // 1.0 * 0.95844
        assert!((state.get_torque(4) - 1.91688).abs() < 0.0001); // 2.0 * 0.95844
        assert!((state.get_torque(5) - 0.47922).abs() < 0.0001); // 0.5 * 0.95844

        // 测试超出范围的索引
        assert_eq!(state.get_torque(6), 0.0);
        assert_eq!(state.get_torque(100), 0.0);
    }

    #[test]
    fn test_joint_dynamic_state_get_all_torques() {
        let state = JointDynamicState {
            joint_current: [1.0, 2.0, 0.5, 1.0, 2.0, 0.5],
            ..Default::default()
        };

        let all_torques = state.get_all_torques();

        // 验证关节 1-3（使用 COEFFICIENT_1_3 = 1.18125）
        assert!((all_torques[0] - 1.18125).abs() < 0.0001); // 1.0 * 1.18125
        assert!((all_torques[1] - 2.3625).abs() < 0.0001); // 2.0 * 1.18125
        assert!((all_torques[2] - 0.590625).abs() < 0.0001); // 0.5 * 1.18125

        // 验证关节 4-6（使用 COEFFICIENT_4_6 = 0.95844）
        assert!((all_torques[3] - 0.95844).abs() < 0.0001); // 1.0 * 0.95844
        assert!((all_torques[4] - 1.91688).abs() < 0.0001); // 2.0 * 0.95844
        assert!((all_torques[5] - 0.47922).abs() < 0.0001); // 0.5 * 0.95844

        // 验证与单独调用 get_torque() 的一致性
        for (i, &torque) in all_torques.iter().enumerate() {
            assert!((torque - state.get_torque(i)).abs() < 0.0001);
        }
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
        // 测试默认状态下扭矩为 0（因为电流为 0）
        assert_eq!(state.get_torque(0), 0.0);
        assert_eq!(state.get_torque(5), 0.0);
    }

    use super::*;

    #[test]
    fn test_piper_context_new() {
        let ctx = PiperContext::new();
        // 验证所有 Arc/ArcSwap 都已初始化
        let joint_pos = ctx.joint_position.load();
        assert_eq!(joint_pos.hardware_timestamp_us, 0);
        assert_eq!(joint_pos.joint_pos, [0.0; 6]);

        let end_pose = ctx.end_pose.load();
        assert_eq!(end_pose.hardware_timestamp_us, 0);
        assert_eq!(end_pose.end_pose, [0.0; 6]);

        let joint_dynamic = ctx.joint_dynamic.load();
        assert_eq!(joint_dynamic.group_timestamp_us, 0);

        let robot_control = ctx.robot_control.load();
        assert_eq!(robot_control.hardware_timestamp_us, 0);

        let driver_state = ctx.joint_driver_low_speed.load();
        assert_eq!(driver_state.hardware_timestamp_us, 0);

        let limits = ctx.joint_limit_config.read().unwrap();
        assert_eq!(limits.joint_limits_max, [0.0; 6]);
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
        // 验证扭矩计算的一致性
        for i in 0..6 {
            assert!((state.get_torque(i) - cloned.get_torque(i)).abs() < 0.0001);
        }
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

    // ============================================================
    // 测试新状态结构：JointPositionState 和 EndPoseState
    // ============================================================

    #[test]
    fn test_joint_position_state_default() {
        let state = JointPositionState::default();
        assert_eq!(state.hardware_timestamp_us, 0);
        assert_eq!(state.system_timestamp_us, 0);
        assert_eq!(state.joint_pos, [0.0; 6]);
        assert_eq!(state.frame_valid_mask, 0);
    }

    #[test]
    fn test_joint_position_state_is_fully_valid() {
        // 完整帧组（所有3帧都收到）
        let state = JointPositionState {
            hardware_timestamp_us: 1000,
            system_timestamp_us: 2000,
            joint_pos: [1.0, 2.0, 3.0, 4.0, 5.0, 6.0],
            frame_valid_mask: 0b0000_0111, // Bit 0-2 全部为 1
        };
        assert!(state.is_fully_valid());

        // 不完整帧组（只有2帧）
        let state_incomplete = JointPositionState {
            frame_valid_mask: 0b0000_0011, // 只有 Bit 0-1
            ..state
        };
        assert!(!state_incomplete.is_fully_valid());

        // 完全不完整（没有帧）
        let state_empty = JointPositionState {
            frame_valid_mask: 0b0000_0000,
            ..state
        };
        assert!(!state_empty.is_fully_valid());
    }

    #[test]
    fn test_joint_position_state_missing_frames() {
        // 完整帧组
        let state_complete = JointPositionState {
            frame_valid_mask: 0b0000_0111,
            ..Default::default()
        };
        assert_eq!(state_complete.missing_frames(), Vec::<usize>::new());

        // 缺少第一帧（0x2A5）
        let state_missing_first = JointPositionState {
            frame_valid_mask: 0b0000_0110, // Bit 1-2 有，Bit 0 没有
            ..Default::default()
        };
        assert_eq!(state_missing_first.missing_frames(), vec![0]);

        // 缺少中间帧（0x2A6）
        let state_missing_middle = JointPositionState {
            frame_valid_mask: 0b0000_0101, // Bit 0 和 2 有，Bit 1 没有
            ..Default::default()
        };
        assert_eq!(state_missing_middle.missing_frames(), vec![1]);

        // 缺少最后一帧（0x2A7）
        let state_missing_last = JointPositionState {
            frame_valid_mask: 0b0000_0011, // Bit 0-1 有，Bit 2 没有
            ..Default::default()
        };
        assert_eq!(state_missing_last.missing_frames(), vec![2]);

        // 缺少多帧
        let state_missing_multiple = JointPositionState {
            frame_valid_mask: 0b0000_0001, // 只有 Bit 0
            ..Default::default()
        };
        let missing = state_missing_multiple.missing_frames();
        assert_eq!(missing.len(), 2);
        assert!(missing.contains(&1));
        assert!(missing.contains(&2));
    }

    #[test]
    fn test_joint_position_state_clone() {
        let state = JointPositionState {
            hardware_timestamp_us: 1000,
            system_timestamp_us: 2000,
            joint_pos: [1.0, 2.0, 3.0, 4.0, 5.0, 6.0],
            frame_valid_mask: 0b0000_0111,
        };
        let cloned = state.clone();
        assert_eq!(state.hardware_timestamp_us, cloned.hardware_timestamp_us);
        assert_eq!(state.system_timestamp_us, cloned.system_timestamp_us);
        assert_eq!(state.joint_pos, cloned.joint_pos);
        assert_eq!(state.frame_valid_mask, cloned.frame_valid_mask);
        assert_eq!(state.is_fully_valid(), cloned.is_fully_valid());
    }

    #[test]
    fn test_end_pose_state_default() {
        let state = EndPoseState::default();
        assert_eq!(state.hardware_timestamp_us, 0);
        assert_eq!(state.system_timestamp_us, 0);
        assert_eq!(state.end_pose, [0.0; 6]);
        assert_eq!(state.frame_valid_mask, 0);
    }

    #[test]
    fn test_end_pose_state_is_fully_valid() {
        // 完整帧组（所有3帧都收到）
        let state = EndPoseState {
            hardware_timestamp_us: 1000,
            system_timestamp_us: 2000,
            end_pose: [0.1, 0.2, 0.3, 0.4, 0.5, 0.6],
            frame_valid_mask: 0b0000_0111, // Bit 0-2 全部为 1
        };
        assert!(state.is_fully_valid());

        // 不完整帧组
        let state_incomplete = EndPoseState {
            frame_valid_mask: 0b0000_0011, // 只有 Bit 0-1
            ..state
        };
        assert!(!state_incomplete.is_fully_valid());
    }

    #[test]
    fn test_end_pose_state_missing_frames() {
        // 完整帧组
        let state_complete = EndPoseState {
            frame_valid_mask: 0b0000_0111,
            ..Default::default()
        };
        assert_eq!(state_complete.missing_frames(), Vec::<usize>::new());

        // 缺少第一帧（0x2A2）
        let state_missing_first = EndPoseState {
            frame_valid_mask: 0b0000_0110, // Bit 1-2 有，Bit 0 没有
            ..Default::default()
        };
        assert_eq!(state_missing_first.missing_frames(), vec![0]);

        // 缺少中间帧（0x2A3）
        let state_missing_middle = EndPoseState {
            frame_valid_mask: 0b0000_0101, // Bit 0 和 2 有，Bit 1 没有
            ..Default::default()
        };
        assert_eq!(state_missing_middle.missing_frames(), vec![1]);

        // 缺少最后一帧（0x2A4）
        let state_missing_last = EndPoseState {
            frame_valid_mask: 0b0000_0011, // Bit 0-1 有，Bit 2 没有
            ..Default::default()
        };
        assert_eq!(state_missing_last.missing_frames(), vec![2]);
    }

    #[test]
    fn test_end_pose_state_clone() {
        let state = EndPoseState {
            hardware_timestamp_us: 1000,
            system_timestamp_us: 2000,
            end_pose: [0.1, 0.2, 0.3, 0.4, 0.5, 0.6],
            frame_valid_mask: 0b0000_0111,
        };
        let cloned = state.clone();
        assert_eq!(state.hardware_timestamp_us, cloned.hardware_timestamp_us);
        assert_eq!(state.system_timestamp_us, cloned.system_timestamp_us);
        assert_eq!(state.end_pose, cloned.end_pose);
        assert_eq!(state.frame_valid_mask, cloned.frame_valid_mask);
        assert_eq!(state.is_fully_valid(), cloned.is_fully_valid());
    }

    #[test]
    fn test_motion_snapshot_default() {
        let snapshot = MotionSnapshot {
            joint_position: JointPositionState::default(),
            end_pose: EndPoseState::default(),
        };
        assert_eq!(snapshot.joint_position.hardware_timestamp_us, 0);
        assert_eq!(snapshot.end_pose.hardware_timestamp_us, 0);
    }

    #[test]
    fn test_motion_snapshot_clone() {
        let snapshot = MotionSnapshot {
            joint_position: JointPositionState {
                hardware_timestamp_us: 1000,
                system_timestamp_us: 2000,
                joint_pos: [1.0, 2.0, 3.0, 4.0, 5.0, 6.0],
                frame_valid_mask: 0b0000_0111,
            },
            end_pose: EndPoseState {
                hardware_timestamp_us: 1500,
                system_timestamp_us: 2500,
                end_pose: [0.1, 0.2, 0.3, 0.4, 0.5, 0.6],
                frame_valid_mask: 0b0000_0111,
            },
        };
        let cloned = snapshot.clone();
        assert_eq!(
            snapshot.joint_position.joint_pos,
            cloned.joint_position.joint_pos
        );
        assert_eq!(snapshot.end_pose.end_pose, cloned.end_pose.end_pose);
    }

    #[test]
    fn test_piper_context_capture_motion_snapshot() {
        let ctx = PiperContext::new();

        // 初始状态应该是默认值
        let snapshot = ctx.capture_motion_snapshot();
        assert_eq!(snapshot.joint_position.hardware_timestamp_us, 0);
        assert_eq!(snapshot.end_pose.hardware_timestamp_us, 0);
        assert_eq!(snapshot.joint_position.joint_pos, [0.0; 6]);
        assert_eq!(snapshot.end_pose.end_pose, [0.0; 6]);
    }

    #[test]
    fn test_piper_context_new_states() {
        let ctx = PiperContext::new();

        // 验证新状态字段存在且为默认值
        let joint_pos = ctx.joint_position.load();
        assert_eq!(joint_pos.hardware_timestamp_us, 0);
        assert_eq!(joint_pos.joint_pos, [0.0; 6]);

        let end_pose = ctx.end_pose.load();
        assert_eq!(end_pose.hardware_timestamp_us, 0);
        assert_eq!(end_pose.end_pose, [0.0; 6]);
    }

    // ============================================================
    // 测试新状态结构：GripperState 和 RobotControlState
    // ============================================================

    #[test]
    fn test_gripper_state_default() {
        let state = GripperState::default();
        assert_eq!(state.hardware_timestamp_us, 0);
        assert_eq!(state.system_timestamp_us, 0);
        assert_eq!(state.travel, 0.0);
        assert_eq!(state.torque, 0.0);
        assert_eq!(state.status_code, 0);
        assert_eq!(state.last_travel, 0.0);
    }

    #[test]
    fn test_gripper_state_status_flags() {
        // 测试所有状态位标志
        let state_voltage_low = GripperState {
            status_code: 0b0000_0001, // Bit 0
            ..Default::default()
        };
        assert!(state_voltage_low.is_voltage_low());
        assert!(!state_voltage_low.is_motor_over_temp());

        let state_motor_over_temp = GripperState {
            status_code: 0b0000_0010, // Bit 1
            ..Default::default()
        };
        assert!(state_motor_over_temp.is_motor_over_temp());
        assert!(!state_motor_over_temp.is_voltage_low());

        let state_over_current = GripperState {
            status_code: 0b0000_0100, // Bit 2
            ..Default::default()
        };
        assert!(state_over_current.is_over_current());

        let state_driver_over_temp = GripperState {
            status_code: 0b0000_1000, // Bit 3
            ..Default::default()
        };
        assert!(state_driver_over_temp.is_driver_over_temp());

        let state_sensor_error = GripperState {
            status_code: 0b0001_0000, // Bit 4
            ..Default::default()
        };
        assert!(state_sensor_error.is_sensor_error());

        let state_driver_error = GripperState {
            status_code: 0b0010_0000, // Bit 5
            ..Default::default()
        };
        assert!(state_driver_error.is_driver_error());

        let state_enabled = GripperState {
            status_code: 0b0100_0000, // Bit 6
            ..Default::default()
        };
        assert!(state_enabled.is_enabled());

        let state_homed = GripperState {
            status_code: 0b1000_0000, // Bit 7
            ..Default::default()
        };
        assert!(state_homed.is_homed());

        // 测试多个标志同时设置
        let state_multiple = GripperState {
            status_code: 0b1100_0011, // Bit 0, 1, 6, 7
            ..Default::default()
        };
        assert!(state_multiple.is_voltage_low());
        assert!(state_multiple.is_motor_over_temp());
        assert!(state_multiple.is_enabled());
        assert!(state_multiple.is_homed());
        assert!(!state_multiple.is_over_current());
    }

    #[test]
    fn test_gripper_state_is_moving() {
        // 静止状态（变化小于阈值）
        let state_stationary = GripperState {
            travel: 50.0,
            last_travel: 50.05, // 变化 0.05mm < 0.1mm
            ..Default::default()
        };
        assert!(!state_stationary.is_moving());

        // 运动状态（变化超过阈值）
        let state_moving = GripperState {
            travel: 50.0,
            last_travel: 50.2, // 变化 0.2mm > 0.1mm
            ..Default::default()
        };
        assert!(state_moving.is_moving());

        // 反向运动
        let state_moving_backward = GripperState {
            travel: 50.0,
            last_travel: 49.8, // 变化 0.2mm > 0.1mm
            ..Default::default()
        };
        assert!(state_moving_backward.is_moving());
    }

    #[test]
    fn test_gripper_state_clone() {
        let state = GripperState {
            hardware_timestamp_us: 1000,
            system_timestamp_us: 2000,
            travel: 50.5,
            torque: 2.5,
            status_code: 0b1100_0011,
            last_travel: 50.0,
        };
        let cloned = state.clone();
        assert_eq!(state.hardware_timestamp_us, cloned.hardware_timestamp_us);
        assert_eq!(state.system_timestamp_us, cloned.system_timestamp_us);
        assert_eq!(state.travel, cloned.travel);
        assert_eq!(state.torque, cloned.torque);
        assert_eq!(state.status_code, cloned.status_code);
        assert_eq!(state.last_travel, cloned.last_travel);
        assert_eq!(state.is_voltage_low(), cloned.is_voltage_low());
        assert_eq!(state.is_moving(), cloned.is_moving());
    }

    #[test]
    fn test_robot_control_state_default() {
        let state = RobotControlState::default();
        assert_eq!(state.hardware_timestamp_us, 0);
        assert_eq!(state.system_timestamp_us, 0);
        assert_eq!(state.control_mode, 0);
        assert_eq!(state.robot_status, 0);
        assert_eq!(state.fault_angle_limit_mask, 0);
        assert_eq!(state.fault_comm_error_mask, 0);
        assert_eq!(state.feedback_counter, 0);
        assert!(!state.is_enabled);
    }

    #[test]
    fn test_robot_control_state_is_angle_limit() {
        // 测试单个关节角度超限位
        let state_j1 = RobotControlState {
            fault_angle_limit_mask: 0b0000_0001, // J1
            ..Default::default()
        };
        assert!(state_j1.is_angle_limit(0));
        assert!(!state_j1.is_angle_limit(1));
        assert!(!state_j1.is_angle_limit(5));

        // 测试多个关节角度超限位
        // 0b0011_0001 = Bit 0, 5, 6 为 1，对应 J1, J6, J7（但J7不存在，所以只有J1和J6）
        // 实际上应该是 0b0010_0001 = Bit 0, 5 为 1，对应 J1, J6
        let state_multiple = RobotControlState {
            fault_angle_limit_mask: 0b0010_0001, // J1 (Bit 0), J6 (Bit 5)
            ..Default::default()
        };
        assert!(state_multiple.is_angle_limit(0)); // J1
        assert!(!state_multiple.is_angle_limit(1)); // J2
        assert!(!state_multiple.is_angle_limit(2)); // J3
        assert!(!state_multiple.is_angle_limit(3)); // J4
        assert!(!state_multiple.is_angle_limit(4)); // J5
        assert!(state_multiple.is_angle_limit(5)); // J6

        // 测试边界情况
        assert!(!state_j1.is_angle_limit(6)); // 超出范围
        assert!(!state_j1.is_angle_limit(100)); // 超出范围
    }

    #[test]
    fn test_robot_control_state_is_comm_error() {
        // 测试单个关节通信异常
        let state_j3 = RobotControlState {
            fault_comm_error_mask: 0b0000_0100, // J3
            ..Default::default()
        };
        assert!(!state_j3.is_comm_error(0));
        assert!(!state_j3.is_comm_error(1));
        assert!(state_j3.is_comm_error(2));
        assert!(!state_j3.is_comm_error(3));

        // 测试所有关节通信异常
        let state_all = RobotControlState {
            fault_comm_error_mask: 0b0011_1111, // J1-J6
            ..Default::default()
        };
        for i in 0..6 {
            assert!(state_all.is_comm_error(i));
        }

        // 测试边界情况
        assert!(!state_j3.is_comm_error(6)); // 超出范围
    }

    #[test]
    fn test_robot_control_state_clone() {
        let state = RobotControlState {
            hardware_timestamp_us: 1000,
            system_timestamp_us: 2000,
            control_mode: 1,
            robot_status: 2,
            move_mode: 3,
            teach_status: 4,
            motion_status: 5,
            trajectory_point_index: 10,
            fault_angle_limit_mask: 0b0011_0001,
            fault_comm_error_mask: 0b0000_0100,
            is_enabled: true,
            feedback_counter: 5,
        };
        let cloned = state.clone();
        assert_eq!(state.hardware_timestamp_us, cloned.hardware_timestamp_us);
        assert_eq!(state.control_mode, cloned.control_mode);
        assert_eq!(state.fault_angle_limit_mask, cloned.fault_angle_limit_mask);
        assert_eq!(state.fault_comm_error_mask, cloned.fault_comm_error_mask);
        assert_eq!(state.is_enabled, cloned.is_enabled);
        assert_eq!(state.is_angle_limit(0), cloned.is_angle_limit(0));
        assert_eq!(state.is_comm_error(2), cloned.is_comm_error(2));
    }

    #[test]
    fn test_piper_context_gripper_and_robot_control() {
        let ctx = PiperContext::new();

        // 验证 gripper 字段存在且为默认值
        let gripper = ctx.gripper.load();
        assert_eq!(gripper.hardware_timestamp_us, 0);
        assert_eq!(gripper.travel, 0.0);
        assert_eq!(gripper.status_code, 0);

        // 验证 robot_control 字段存在且为默认值
        let robot_control = ctx.robot_control.load();
        assert_eq!(robot_control.hardware_timestamp_us, 0);
        assert_eq!(robot_control.control_mode, 0);
        assert_eq!(robot_control.fault_angle_limit_mask, 0);
        assert!(!robot_control.is_enabled);
    }

    // ============================================================
    // 测试新状态结构：JointDriverLowSpeedState
    // ============================================================

    #[test]
    fn test_joint_driver_low_speed_state_default() {
        let state = JointDriverLowSpeedState::default();
        assert_eq!(state.hardware_timestamp_us, 0);
        assert_eq!(state.system_timestamp_us, 0);
        assert_eq!(state.motor_temps, [0.0; 6]);
        assert_eq!(state.driver_temps, [0.0; 6]);
        assert_eq!(state.joint_voltage, [0.0; 6]);
        assert_eq!(state.joint_bus_current, [0.0; 6]);
        assert_eq!(state.driver_voltage_low_mask, 0);
        assert_eq!(state.valid_mask, 0);
    }

    #[test]
    fn test_joint_driver_low_speed_state_is_fully_valid() {
        // 完整状态（所有6个关节都已更新）
        let state_complete = JointDriverLowSpeedState {
            valid_mask: 0b111111, // Bit 0-5 全部为 1
            ..Default::default()
        };
        assert!(state_complete.is_fully_valid());

        // 不完整状态（只有部分关节更新）
        let state_incomplete = JointDriverLowSpeedState {
            valid_mask: 0b001111, // 只有 Bit 0-3
            ..Default::default()
        };
        assert!(!state_incomplete.is_fully_valid());
    }

    #[test]
    fn test_joint_driver_low_speed_state_missing_joints() {
        // 完整状态
        let state_complete = JointDriverLowSpeedState {
            valid_mask: 0b111111,
            ..Default::default()
        };
        assert_eq!(state_complete.missing_joints(), Vec::<usize>::new());

        // 缺少 J1 和 J6
        let state_missing = JointDriverLowSpeedState {
            valid_mask: 0b0011110, // Bit 1-4 有，Bit 0 和 5 没有
            ..Default::default()
        };
        let missing = state_missing.missing_joints();
        assert_eq!(missing.len(), 2);
        assert!(missing.contains(&0));
        assert!(missing.contains(&5));
    }

    #[test]
    fn test_joint_driver_low_speed_state_status_flags() {
        // 测试单个关节的状态标志
        let state_j1_voltage_low = JointDriverLowSpeedState {
            driver_voltage_low_mask: 0b0000_0001, // J1
            ..Default::default()
        };
        assert!(state_j1_voltage_low.is_voltage_low(0));
        assert!(!state_j1_voltage_low.is_voltage_low(1));

        let state_j3_motor_over_temp = JointDriverLowSpeedState {
            driver_motor_over_temp_mask: 0b0000_0100, // J3
            ..Default::default()
        };
        assert!(state_j3_motor_over_temp.is_motor_over_temp(2));
        assert!(!state_j3_motor_over_temp.is_motor_over_temp(0));

        let state_j6_over_current = JointDriverLowSpeedState {
            driver_over_current_mask: 0b0010_0000, // J6
            ..Default::default()
        };
        assert!(state_j6_over_current.is_over_current(5));
        assert!(!state_j6_over_current.is_over_current(0));

        // 测试多个关节同时设置
        let state_multiple = JointDriverLowSpeedState {
            driver_voltage_low_mask: 0b0010_0001, // J1, J6
            driver_enabled_mask: 0b111111,        // 所有关节使能
            ..Default::default()
        };
        assert!(state_multiple.is_voltage_low(0));
        assert!(!state_multiple.is_voltage_low(1));
        assert!(state_multiple.is_voltage_low(5));
        assert!(state_multiple.is_enabled(0));
        assert!(state_multiple.is_enabled(5));
    }

    #[test]
    fn test_joint_driver_low_speed_state_all_status_methods() {
        let state = JointDriverLowSpeedState {
            driver_voltage_low_mask: 0b0000_0001,          // J1
            driver_motor_over_temp_mask: 0b0000_0010,      // J2
            driver_over_current_mask: 0b0000_0100,         // J3
            driver_over_temp_mask: 0b0000_1000,            // J4
            driver_collision_protection_mask: 0b0001_0000, // J5
            driver_error_mask: 0b0010_0000,                // J6
            driver_enabled_mask: 0b111111,                 // 所有关节使能
            driver_stall_protection_mask: 0b0000_0001,     // J1
            ..Default::default()
        };

        assert!(state.is_voltage_low(0));
        assert!(state.is_motor_over_temp(1));
        assert!(state.is_over_current(2));
        assert!(state.is_driver_over_temp(3));
        assert!(state.is_collision_protection(4));
        assert!(state.is_driver_error(5));
        assert!(state.is_enabled(0));
        assert!(state.is_enabled(5));
        assert!(state.is_stall_protection(0));
    }

    #[test]
    fn test_joint_driver_low_speed_state_clone() {
        let state = JointDriverLowSpeedState {
            hardware_timestamp_us: 1000,
            system_timestamp_us: 2000,
            motor_temps: [25.0, 26.0, 27.0, 28.0, 29.0, 30.0],
            driver_temps: [35.0, 36.0, 37.0, 38.0, 39.0, 40.0],
            joint_voltage: [24.0, 24.1, 24.2, 24.3, 24.4, 24.5],
            joint_bus_current: [1.0, 1.1, 1.2, 1.3, 1.4, 1.5],
            driver_voltage_low_mask: 0b0000_0001,
            driver_motor_over_temp_mask: 0b0000_0010,
            driver_over_current_mask: 0b0000_0100,
            driver_over_temp_mask: 0b0000_1000,
            driver_collision_protection_mask: 0b0001_0000,
            driver_error_mask: 0b0010_0000,
            driver_enabled_mask: 0b111111,
            driver_stall_protection_mask: 0b0000_0001,
            hardware_timestamps: [100, 200, 300, 400, 500, 600],
            system_timestamps: [1100, 1200, 1300, 1400, 1500, 1600],
            valid_mask: 0b111111,
        };
        let cloned = state.clone();
        assert_eq!(state.hardware_timestamp_us, cloned.hardware_timestamp_us);
        assert_eq!(state.motor_temps, cloned.motor_temps);
        assert_eq!(
            state.driver_voltage_low_mask,
            cloned.driver_voltage_low_mask
        );
        assert_eq!(state.valid_mask, cloned.valid_mask);
        assert_eq!(state.is_fully_valid(), cloned.is_fully_valid());
        assert_eq!(state.is_voltage_low(0), cloned.is_voltage_low(0));
    }

    #[test]
    fn test_piper_context_joint_driver_low_speed() {
        let ctx = PiperContext::new();

        // 验证 joint_driver_low_speed 字段存在且为默认值
        let state = ctx.joint_driver_low_speed.load();
        assert_eq!(state.hardware_timestamp_us, 0);
        assert_eq!(state.motor_temps, [0.0; 6]);
        assert_eq!(state.driver_voltage_low_mask, 0);
        assert_eq!(state.valid_mask, 0);
    }

    // ============================================================
    // 测试新状态结构：CollisionProtectionState
    // ============================================================

    #[test]
    fn test_collision_protection_state_default() {
        let state = CollisionProtectionState::default();
        assert_eq!(state.hardware_timestamp_us, 0);
        assert_eq!(state.system_timestamp_us, 0);
        assert_eq!(state.protection_levels, [0; 6]);
    }

    #[test]
    fn test_collision_protection_state_clone() {
        let state = CollisionProtectionState {
            hardware_timestamp_us: 1000,
            system_timestamp_us: 2000,
            protection_levels: [5, 5, 5, 4, 4, 4],
        };
        let cloned = state.clone();
        assert_eq!(state.hardware_timestamp_us, cloned.hardware_timestamp_us);
        assert_eq!(state.system_timestamp_us, cloned.system_timestamp_us);
        assert_eq!(state.protection_levels, cloned.protection_levels);
    }

    #[test]
    fn test_collision_protection_state_protection_levels() {
        // 测试不同保护等级
        let state_all_zero = CollisionProtectionState {
            protection_levels: [0; 6], // 所有关节不检测碰撞
            ..Default::default()
        };
        assert_eq!(state_all_zero.protection_levels, [0; 6]);

        let state_mixed = CollisionProtectionState {
            protection_levels: [8, 7, 6, 5, 4, 3], // 不同等级
            ..Default::default()
        };
        assert_eq!(state_mixed.protection_levels[0], 8);
        assert_eq!(state_mixed.protection_levels[5], 3);
    }

    #[test]
    fn test_piper_context_collision_protection() {
        let ctx = PiperContext::new();

        // 验证 collision_protection 字段存在且为默认值
        let state = ctx.collision_protection.read().unwrap();
        assert_eq!(state.hardware_timestamp_us, 0);
        assert_eq!(state.system_timestamp_us, 0);
        assert_eq!(state.protection_levels, [0; 6]);
    }

    // ============================================================
    // 测试新状态结构：JointLimitConfigState
    // ============================================================

    #[test]
    fn test_joint_limit_config_state_default() {
        let state = JointLimitConfigState::default();
        assert_eq!(state.last_update_hardware_timestamp_us, 0);
        assert_eq!(state.last_update_system_timestamp_us, 0);
        assert_eq!(state.joint_limits_max, [0.0; 6]);
        assert_eq!(state.joint_limits_min, [0.0; 6]);
        assert_eq!(state.joint_max_velocity, [0.0; 6]);
        assert_eq!(state.joint_update_hardware_timestamps, [0; 6]);
        assert_eq!(state.joint_update_system_timestamps, [0; 6]);
        assert_eq!(state.valid_mask, 0);
    }

    #[test]
    fn test_joint_limit_config_state_is_fully_valid() {
        // 完整状态（所有6个关节都已更新）
        let state_complete = JointLimitConfigState {
            valid_mask: 0b111111, // Bit 0-5 全部为 1
            ..Default::default()
        };
        assert!(state_complete.is_fully_valid());

        // 不完整状态（只有部分关节更新）
        let state_incomplete = JointLimitConfigState {
            valid_mask: 0b001111, // 只有 Bit 0-3
            ..Default::default()
        };
        assert!(!state_incomplete.is_fully_valid());
    }

    #[test]
    fn test_joint_limit_config_state_missing_joints() {
        // 完整状态
        let state_complete = JointLimitConfigState {
            valid_mask: 0b111111,
            ..Default::default()
        };
        assert_eq!(state_complete.missing_joints(), Vec::<usize>::new());

        // 缺少 J1 和 J6
        let state_missing = JointLimitConfigState {
            valid_mask: 0b0011110, // Bit 1-4 有，Bit 0 和 5 没有
            ..Default::default()
        };
        let missing = state_missing.missing_joints();
        assert_eq!(missing.len(), 2);
        assert!(missing.contains(&0));
        assert!(missing.contains(&5));
    }

    #[test]
    fn test_joint_limit_config_state_clone() {
        let state = JointLimitConfigState {
            last_update_hardware_timestamp_us: 1000,
            last_update_system_timestamp_us: 2000,
            joint_limits_max: [1.57, 1.57, 1.57, 1.57, 1.57, 1.57], // 90度 = π/2 弧度
            joint_limits_min: [-1.57, -1.57, -1.57, -1.57, -1.57, -1.57], // -90度
            joint_max_velocity: [PI, PI, PI, PI, PI, PI],           // 180度/s = π rad/s
            joint_update_hardware_timestamps: [100, 200, 300, 400, 500, 600],
            joint_update_system_timestamps: [1100, 1200, 1300, 1400, 1500, 1600],
            valid_mask: 0b111111,
        };
        let cloned = state.clone();
        assert_eq!(
            state.last_update_hardware_timestamp_us,
            cloned.last_update_hardware_timestamp_us
        );
        assert_eq!(state.joint_limits_max, cloned.joint_limits_max);
        assert_eq!(state.joint_limits_min, cloned.joint_limits_min);
        assert_eq!(state.joint_max_velocity, cloned.joint_max_velocity);
        assert_eq!(state.valid_mask, cloned.valid_mask);
        assert_eq!(state.is_fully_valid(), cloned.is_fully_valid());
    }

    #[test]
    fn test_piper_context_joint_limit_config() {
        let ctx = PiperContext::new();

        // 验证 joint_limit_config 字段存在且为默认值
        let state = ctx.joint_limit_config.read().unwrap();
        assert_eq!(state.last_update_hardware_timestamp_us, 0);
        assert_eq!(state.joint_limits_max, [0.0; 6]);
        assert_eq!(state.joint_limits_min, [0.0; 6]);
        assert_eq!(state.joint_max_velocity, [0.0; 6]);
        assert_eq!(state.valid_mask, 0);
    }

    // ============================================================
    // 测试新状态结构：JointAccelConfigState
    // ============================================================

    #[test]
    fn test_joint_accel_config_state_default() {
        let state = JointAccelConfigState::default();
        assert_eq!(state.last_update_hardware_timestamp_us, 0);
        assert_eq!(state.last_update_system_timestamp_us, 0);
        assert_eq!(state.max_acc_limits, [0.0; 6]);
        assert_eq!(state.joint_update_hardware_timestamps, [0; 6]);
        assert_eq!(state.joint_update_system_timestamps, [0; 6]);
        assert_eq!(state.valid_mask, 0);
    }

    #[test]
    fn test_joint_accel_config_state_is_fully_valid() {
        // 完整状态（所有6个关节都已更新）
        let state_complete = JointAccelConfigState {
            valid_mask: 0b111111, // Bit 0-5 全部为 1
            ..Default::default()
        };
        assert!(state_complete.is_fully_valid());

        // 不完整状态（只有部分关节更新）
        let state_incomplete = JointAccelConfigState {
            valid_mask: 0b001111, // 只有 Bit 0-3
            ..Default::default()
        };
        assert!(!state_incomplete.is_fully_valid());
    }

    #[test]
    fn test_joint_accel_config_state_missing_joints() {
        // 完整状态
        let state_complete = JointAccelConfigState {
            valid_mask: 0b111111,
            ..Default::default()
        };
        assert_eq!(state_complete.missing_joints(), Vec::<usize>::new());

        // 缺少 J1 和 J6
        let state_missing = JointAccelConfigState {
            valid_mask: 0b0011110, // Bit 1-4 有，Bit 0 和 5 没有
            ..Default::default()
        };
        let missing = state_missing.missing_joints();
        assert_eq!(missing.len(), 2);
        assert!(missing.contains(&0));
        assert!(missing.contains(&5));
    }

    #[test]
    fn test_joint_accel_config_state_clone() {
        let state = JointAccelConfigState {
            last_update_hardware_timestamp_us: 1000,
            last_update_system_timestamp_us: 2000,
            max_acc_limits: [10.0, 10.0, 10.0, 10.0, 10.0, 10.0], // 10 rad/s²
            joint_update_hardware_timestamps: [100, 200, 300, 400, 500, 600],
            joint_update_system_timestamps: [1100, 1200, 1300, 1400, 1500, 1600],
            valid_mask: 0b111111,
        };
        let cloned = state.clone();
        assert_eq!(
            state.last_update_hardware_timestamp_us,
            cloned.last_update_hardware_timestamp_us
        );
        assert_eq!(state.max_acc_limits, cloned.max_acc_limits);
        assert_eq!(state.valid_mask, cloned.valid_mask);
        assert_eq!(state.is_fully_valid(), cloned.is_fully_valid());
    }

    #[test]
    fn test_piper_context_joint_accel_config() {
        let ctx = PiperContext::new();

        // 验证 joint_accel_config 字段存在且为默认值
        let state = ctx.joint_accel_config.read().unwrap();
        assert_eq!(state.last_update_hardware_timestamp_us, 0);
        assert_eq!(state.max_acc_limits, [0.0; 6]);
        assert_eq!(state.valid_mask, 0);
    }

    // ============================================================
    // 测试新状态结构：EndLimitConfigState
    // ============================================================

    #[test]
    fn test_end_limit_config_state_default() {
        let state = EndLimitConfigState::default();
        assert_eq!(state.last_update_hardware_timestamp_us, 0);
        assert_eq!(state.last_update_system_timestamp_us, 0);
        assert_eq!(state.max_end_linear_velocity, 0.0);
        assert_eq!(state.max_end_angular_velocity, 0.0);
        assert_eq!(state.max_end_linear_accel, 0.0);
        assert_eq!(state.max_end_angular_accel, 0.0);
        assert!(!state.is_valid);
    }

    #[test]
    fn test_end_limit_config_state_clone() {
        let state = EndLimitConfigState {
            last_update_hardware_timestamp_us: 1000,
            last_update_system_timestamp_us: 2000,
            max_end_linear_velocity: 1.0,  // 1 m/s
            max_end_angular_velocity: 2.0, // 2 rad/s
            max_end_linear_accel: 0.5,     // 0.5 m/s²
            max_end_angular_accel: 1.5,    // 1.5 rad/s²
            is_valid: true,
        };
        let cloned = state.clone();
        assert_eq!(
            state.last_update_hardware_timestamp_us,
            cloned.last_update_hardware_timestamp_us
        );
        assert_eq!(
            state.max_end_linear_velocity,
            cloned.max_end_linear_velocity
        );
        assert_eq!(
            state.max_end_angular_velocity,
            cloned.max_end_angular_velocity
        );
        assert_eq!(state.max_end_linear_accel, cloned.max_end_linear_accel);
        assert_eq!(state.max_end_angular_accel, cloned.max_end_angular_accel);
        assert_eq!(state.is_valid, cloned.is_valid);
    }

    #[test]
    fn test_piper_context_end_limit_config() {
        let ctx = PiperContext::new();

        // 验证 end_limit_config 字段存在且为默认值
        let state = ctx.end_limit_config.read().unwrap();
        assert_eq!(state.last_update_hardware_timestamp_us, 0);
        assert_eq!(state.max_end_linear_velocity, 0.0);
        assert_eq!(state.max_end_angular_velocity, 0.0);
        assert_eq!(state.max_end_linear_accel, 0.0);
        assert_eq!(state.max_end_angular_accel, 0.0);
        assert!(!state.is_valid);
    }
}
