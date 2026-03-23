//! Driver 模块状态结构定义

use crate::fps_stats::FpsStatistics;
use arc_swap::ArcSwap;
use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};

/// 固定槽位实时快照单元。
///
/// 该类型用于 500Hz 热路径的单写者、多读者发布场景：
/// - 写者只能写入当前未发布、且没有读者持有的槽位
/// - 读者先增加 reader count，再复核 published 槽位，确保读取稳定快照
/// - 发布只切换槽位索引，不涉及堆分配或锁
///
/// # Safety
///
/// 内部 `unsafe` 依赖以下不变量：
/// - 只有单个写线程调用 `store()`
/// - 写者只会写入 `reader_count == 0` 的非已发布槽位
/// - 读者在持有 reader count 期间，写者不会复用该槽位
struct RealtimeSnapshotCell<T: Copy + Default, const N: usize = 3> {
    slots: [UnsafeCell<T>; N],
    reader_counts: [AtomicUsize; N],
    published_slot: AtomicUsize,
}

impl<T: Copy + Default, const N: usize> RealtimeSnapshotCell<T, N> {
    fn new(initial: T) -> Self {
        assert!(N >= 3, "RealtimeSnapshotCell requires at least 3 slots");
        Self {
            slots: std::array::from_fn(|_| UnsafeCell::new(initial)),
            reader_counts: std::array::from_fn(|_| AtomicUsize::new(0)),
            published_slot: AtomicUsize::new(0),
        }
    }

    fn load(&self) -> T {
        loop {
            let slot = self.published_slot.load(Ordering::Acquire);
            self.reader_counts[slot].fetch_add(1, Ordering::AcqRel);

            if self.published_slot.load(Ordering::Acquire) == slot {
                // SAFETY:
                // - 读者已经持有该槽位的 reader count
                // - 写者不会复用 reader count > 0 的槽位
                // - T: Copy，只返回按值副本
                let value = unsafe { *self.slots[slot].get() };
                self.reader_counts[slot].fetch_sub(1, Ordering::AcqRel);
                return value;
            }

            self.reader_counts[slot].fetch_sub(1, Ordering::AcqRel);
            std::hint::spin_loop();
        }
    }

    fn store(&self, value: T) {
        loop {
            let published = self.published_slot.load(Ordering::Acquire);

            for slot in 0..N {
                if slot == published {
                    continue;
                }
                if self.reader_counts[slot].load(Ordering::Acquire) != 0 {
                    continue;
                }

                // SAFETY:
                // - 单写者模型下不存在并发写
                // - 该槽位不是当前 published，新的读者无法进入
                // - reader_count == 0，没有旧读者仍持有该槽位
                unsafe {
                    *self.slots[slot].get() = value;
                }
                self.published_slot.store(slot, Ordering::Release);
                return;
            }

            std::hint::spin_loop();
        }
    }

    #[cfg(test)]
    fn published_slot_for_test(&self) -> usize {
        self.published_slot.load(Ordering::Acquire)
    }

    #[cfg(test)]
    fn pin_slot_for_test(&self, slot: usize) -> RealtimeSnapshotSlotGuard<'_, T, N> {
        self.reader_counts[slot].fetch_add(1, Ordering::AcqRel);
        RealtimeSnapshotSlotGuard { cell: self, slot }
    }
}

impl<T: Copy + Default, const N: usize> Default for RealtimeSnapshotCell<T, N> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

// SAFETY:
// - 内部可变性仅用于受 reader count 协调保护的槽位写入
// - 跨线程传递/共享时只暴露按值快照
unsafe impl<T: Copy + Default + Send, const N: usize> Send for RealtimeSnapshotCell<T, N> {}
// SAFETY:
// - 并发访问通过 published 索引和 per-slot reader count 协调
// - 读者不会获得内部可变引用
unsafe impl<T: Copy + Default + Send, const N: usize> Sync for RealtimeSnapshotCell<T, N> {}

#[cfg(test)]
struct RealtimeSnapshotSlotGuard<'a, T: Copy + Default, const N: usize = 3> {
    cell: &'a RealtimeSnapshotCell<T, N>,
    slot: usize,
}

#[cfg(test)]
impl<T: Copy + Default, const N: usize> RealtimeSnapshotSlotGuard<'_, T, N> {
    fn slot(&self) -> usize {
        self.slot
    }
}

#[cfg(test)]
impl<T: Copy + Default, const N: usize> Drop for RealtimeSnapshotSlotGuard<'_, T, N> {
    fn drop(&mut self) {
        self.cell.reader_counts[self.slot].fetch_sub(1, Ordering::AcqRel);
    }
}

/// 关节位置状态（帧组同步）
///
/// 更新频率：~500Hz
/// CAN ID：0x2A5-0x2A7
#[derive(Debug, Clone, Copy, Default)]
pub struct JointPositionState {
    /// 硬件时间戳（微秒，来自完整帧组的最后一帧）
    ///
    /// **注意**：这是CAN硬件时间戳，反映数据在CAN总线上的实际传输时间。
    pub hardware_timestamp_us: u64,

    /// 系统接收时间戳（微秒，系统接收到完整帧组的时间）
    ///
    /// **注意**：这是系统时间戳，用于计算接收延迟和系统处理时间。
    pub host_rx_mono_us: u64,

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
#[derive(Debug, Clone, Copy, Default)]
pub struct EndPoseState {
    /// 硬件时间戳（微秒，来自完整帧组的最后一帧）
    pub hardware_timestamp_us: u64,

    /// 系统接收时间戳（微秒，系统接收到完整帧组的时间）
    pub host_rx_mono_us: u64,

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

/// 单次原子可见的监控快照
///
/// 同时保存最近一份完整监控快照和当前 raw 诊断状态，
/// 用于避免 monitor 读路径跨两份状态读取导致的竞态。
#[derive(Debug, Clone, Copy, Default)]
pub struct MonitorSnapshot<T: Copy + Default> {
    /// 最近一份完整 monitor-complete 快照
    latest_complete: Option<T>,
    /// 当前 raw 状态（允许部分帧/部分关节）
    latest_raw: T,
}

impl<T: Copy + Default> MonitorSnapshot<T> {
    /// 构造同时更新 complete/raw 的监控快照
    pub fn from_complete(complete: T) -> Self {
        Self {
            latest_complete: Some(complete),
            latest_raw: complete,
        }
    }

    /// 构造保留上一份完整快照、仅更新 raw 的监控快照
    pub fn with_raw(latest_complete: Option<T>, latest_raw: T) -> Self {
        Self {
            latest_complete,
            latest_raw,
        }
    }

    /// 返回最近一份完整监控快照。
    pub fn latest_complete(&self) -> Option<&T> {
        self.latest_complete.as_ref()
    }

    /// 返回最近一份完整监控快照的副本。
    pub fn latest_complete_cloned(&self) -> Option<T> {
        self.latest_complete().cloned()
    }

    /// 返回当前 raw 诊断状态。
    pub fn latest_raw(&self) -> &T {
        &self.latest_raw
    }
}

/// 关节位置监控快照
pub type JointPositionMonitorSnapshot = MonitorSnapshot<JointPositionState>;
/// 末端位姿监控快照
pub type EndPoseMonitorSnapshot = MonitorSnapshot<EndPoseState>;
/// 关节动态监控快照
pub type JointDynamicMonitorSnapshot = MonitorSnapshot<JointDynamicState>;

/// 运动状态快照（逻辑原子性）
///
/// 用于在同一时刻捕获多个运动相关状态，保证逻辑上的原子性。
/// 这是一个栈上对象（Stack Allocated），开销极小。
#[derive(Debug, Clone, Copy, Default)]
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
#[derive(Debug, Clone, Copy, Default)]
pub struct JointDynamicState {
    /// 整个组的大致时间戳（最新一帧的时间，微秒）
    ///
    /// **注意**：存储的是硬件时间戳（来自 `PiperFrame.timestamp_us`），不是 UNIX 时间戳。
    /// 硬件时间戳是设备相对时间，用于帧间时间差计算，不能直接与系统时间戳比较。
    pub group_timestamp_us: u64,
    /// 整个组在主机侧提交时的系统时间戳（微秒）
    pub group_host_rx_mono_us: u64,

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

    /// 计算当前动态组内最早/最晚关节时间戳的跨度。
    pub fn group_span_us(&self) -> u64 {
        let mut min_ts = u64::MAX;
        let mut max_ts = 0;
        let mut count = 0;

        for &timestamp in &self.timestamps {
            if timestamp == 0 {
                continue;
            }
            min_ts = min_ts.min(timestamp);
            max_ts = max_ts.max(timestamp);
            count += 1;
        }

        if count == 0 {
            0
        } else {
            max_ts.saturating_sub(min_ts)
        }
    }

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
    /// # use piper_driver::JointDynamicState;
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
    /// # use piper_driver::JointDynamicState;
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
    /// # use piper_driver::JointDynamicState;
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
    pub host_rx_mono_us: u64,

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

    /// 驱动器使能位掩码（Bit 0-5 对应 J1-J6）
    pub driver_enabled_mask: u8,

    /// 是否至少有一个驱动器保持使能
    pub any_drive_enabled: bool,

    /// 使能状态（6 轴驱动器全部使能）
    pub is_enabled: bool,

    /// 已确认的驱动器使能位掩码（完整且新鲜的 6 轴低速反馈）
    pub confirmed_driver_enabled_mask: Option<u8>,

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

    /// 检查是否已确认 6 轴全部使能。
    pub fn is_fully_enabled_confirmed(&self) -> bool {
        self.confirmed_driver_enabled_mask == Some(0b11_1111)
    }

    /// 检查是否已确认 6 轴全部失能。
    pub fn is_fully_disabled_confirmed(&self) -> bool {
        self.confirmed_driver_enabled_mask == Some(0)
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
    pub host_rx_mono_us: u64,

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
    pub host_rx_mono_us: u64,

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
    pub host_rx_mono_timestamps: [u64; 6],

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

    /// 返回已确认的 6 轴驱动器使能掩码。
    ///
    /// 只有当全部 6 个关节都收到过低速反馈，且每个关节的 host monotonic
    /// 时间戳都仍在 freshness 窗口内时，才认为整组使能状态已确认。
    pub(crate) fn confirmed_driver_enabled_mask(
        &self,
        now_host_mono_us: u64,
        freshness_window_us: u64,
    ) -> Option<u8> {
        for timestamp in self.host_rx_mono_timestamps {
            if timestamp == 0 {
                return None;
            }
            if now_host_mono_us.saturating_sub(timestamp) > freshness_window_us {
                return None;
            }
        }

        Some(self.driver_enabled_mask)
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
    pub host_rx_mono_us: u64,

    /// 各关节碰撞保护等级（0-8）[J1, J2, J3, J4, J5, J6]
    ///
    /// **注意**：
    /// - 0：不检测碰撞
    /// - 1-8：碰撞保护等级（数字越大，保护越严格）
    pub protection_levels: [u8; 6],
}

/// 设置指令应答状态（冷数据）
///
/// 更新频率：按需查询（通常由配置/维护类操作触发）
/// CAN ID：0x476
/// 同步机制：RwLock（更新频率极低）
#[derive(Debug, Clone, Default)]
pub struct SettingResponseState {
    /// 硬件时间戳（微秒，来自 CAN 帧）
    pub hardware_timestamp_us: u64,

    /// 系统接收时间戳（微秒）
    pub host_rx_mono_us: u64,

    /// 应答指令索引（例如 0x75 对应 0x475）
    pub response_index: u8,

    /// 零点设置是否成功
    pub zero_point_success: bool,

    /// 是否已经收到过有效应答
    pub is_valid: bool,
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
    pub last_update_host_rx_mono_us: u64,

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
    pub joint_update_host_rx_mono_timestamps: [u64; 6],

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
    pub last_update_host_rx_mono_us: u64,

    // === 关节加速度限制配置（来自 0x47C，需要查询6次） ===
    /// 各关节最大加速度（rad/s²）[J1, J2, J3, J4, J5, J6]
    pub max_acc_limits: [f64; 6],

    // === 时间戳（每个关节独立） ===
    /// 每个关节的硬件时间戳（微秒）[J1, J2, J3, J4, J5, J6]
    pub joint_update_hardware_timestamps: [u64; 6],
    /// 每个关节的系统接收时间戳（微秒）[J1, J2, J3, J4, J5, J6]
    pub joint_update_host_rx_mono_timestamps: [u64; 6],

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
    pub last_update_host_rx_mono_us: u64,

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
    pub host_rx_mono_us: u64,

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
    /// 清空累积的固件数据（用于开始新的查询）
    ///
    /// 在发送新的固件版本查询命令前调用此方法，清空之前累积的数据。
    pub fn clear(&mut self) {
        self.firmware_data.clear();
        self.version_string = None;
        self.is_complete = false;
        self.hardware_timestamp_us = 0;
        self.host_rx_mono_us = 0;
    }

    /// 检查数据是否完整（是否找到 S-V 标记且有足够数据）
    ///
    /// 数据完整的条件：
    /// 1. 找到 "S-V" 标记
    /// 2. 从 S-V 开始至少有 8 字节数据
    ///
    /// # 返回值
    /// 如果数据完整，返回 `true` 并更新 `is_complete` 字段
    pub fn check_completeness(&mut self) -> bool {
        if let Some(version_start) = self.firmware_data.windows(3).position(|w| w == b"S-V") {
            // 找到 S-V 标记，检查是否有足够的数据（至少 8 字节）
            let required_length = version_start + 8;
            self.is_complete = self.firmware_data.len() >= required_length;
        } else {
            self.is_complete = false;
        }
        self.is_complete
    }

    /// 尝试从累积数据中解析版本字符串
    ///
    /// 解析成功时会自动更新 `version_string` 和 `is_complete` 状态。
    pub fn parse_version(&mut self) -> Option<String> {
        // 导入 FirmwareReadFeedback 的 parse_version_string 方法
        use piper_protocol::feedback::FirmwareReadFeedback;
        if !self.check_completeness() {
            self.version_string = None;
            return None;
        }

        if let Some(version) = FirmwareReadFeedback::parse_version_string(&self.firmware_data) {
            self.version_string = Some(version.clone());
            self.is_complete = true;
            Some(version)
        } else {
            self.version_string = None;
            self.is_complete = false;
            None
        }
    }

    /// 获取版本字符串（如果已解析）
    pub fn version_string(&self) -> Option<&String> {
        self.version_string.as_ref().filter(|_| self.is_complete)
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
    pub host_rx_mono_us: u64,

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
    pub host_rx_mono_us: u64,

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
    pub host_rx_mono_us: u64,

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

/// 钩子管理器（v1.2.1）
///
/// 专门管理运行时回调列表。
///
/// # 设计理由
///
/// - **Config vs Context 分离**:
///   - `PipelineConfig` 应该是 POD（Plain Old Data），用于序列化
///   - `PiperContext` 管理运行时状态和动态组件（如回调）
///
/// # 线程安全
///
/// 使用 `RwLock<HookManager>` 确保回调列表的线程安全。
///
/// # 使用示例
///
/// ```rust
/// use piper_driver::hooks::HookManager;
/// use piper_driver::hooks::FrameCallback;
/// use piper_driver::recording::AsyncRecordingHook;
/// use piper_driver::state::PiperContext;
/// use std::sync::Arc;
///
/// // 添加录制钩子
/// let context = PiperContext::new();
/// let (hook, _rx) = AsyncRecordingHook::new();
/// let callback = Arc::new(hook) as Arc<dyn FrameCallback>;
///
/// if let Ok(mut hooks) = context.hooks.write() {
///     hooks.add_callback(callback);
/// }
/// ```
use crate::hooks::HookManager;

/// Piper 上下文（所有状态的聚合）
pub struct PiperContext {
    // === 热数据（500Hz，高频运动数据）===
    // 使用固定槽位快照，无锁读取，适合高频控制循环
    /// 关节位置监控快照（完整监控 + raw 诊断，共享一次原子发布）
    joint_position_monitor: Arc<RealtimeSnapshotCell<JointPositionMonitorSnapshot>>,
    /// 末端位姿监控快照（完整监控 + raw 诊断，共享一次原子发布）
    end_pose_monitor: Arc<RealtimeSnapshotCell<EndPoseMonitorSnapshot>>,
    /// 完整监控运动状态快照（单次 load 保证逻辑原子）
    motion_snapshot: Arc<RealtimeSnapshotCell<MotionSnapshot>>,
    /// 控制级关节位置状态（完整帧组 + 对齐跨度约束）
    control_joint_position: Arc<RealtimeSnapshotCell<JointPositionState>>,
    /// 关节动态监控快照（完整监控 + raw 诊断，共享一次原子发布）
    joint_dynamic_monitor: Arc<RealtimeSnapshotCell<JointDynamicMonitorSnapshot>>,
    /// 控制级关节动态状态（完整 6 关节组 + 组内跨度约束）
    control_joint_dynamic: Arc<RealtimeSnapshotCell<JointDynamicState>>,
    /// 原始运动状态快照（单次 load 保证逻辑原子）
    raw_motion_snapshot: Arc<RealtimeSnapshotCell<MotionSnapshot>>,

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

    /// 设置指令应答状态（按需查询：0x476）
    pub setting_response: Arc<RwLock<SettingResponseState>>,

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
    ///
    /// 使用 `ArcSwap` 支持在运行中原子性"重置统计窗口"（替换为新的 `FpsStatistics`），
    /// 且不引入每帧的锁开销。
    pub fps_stats: Arc<ArcSwap<FpsStatistics>>,

    // === 连接监控 ===
    /// 连接监控（用于检测机器人是否仍在响应）
    ///
    /// 使用 App Start Relative Time 模式，确保时间单调性。
    pub connection_monitor: crate::heartbeat::ConnectionMonitor,
    /// 第一次带可信设备时间戳的反馈到达主机的单调时间（微秒）。
    pub first_timestamped_feedback_host_rx_mono_us: AtomicU64,

    // === 钩子管理（v1.2.1）===
    /// 钩子管理器（用于运行时回调注册）
    ///
    /// 使用 `RwLock` 支持动态添加/移除回调，同时保证线程安全。
    ///
    /// # 设计理由（v1.2.1）
    ///
    /// - **Config vs Context 分离**: `PipelineConfig` 保持为 POD（Plain Old Data），
    ///   `PiperContext` 管理运行时状态和动态组件（如回调）
    /// - **动态注册**: 运行时可以添加/移除回调，无需重新配置 pipeline
    /// - **线程安全**: 使用 `RwLock` 保证并发访问安全
    ///
    /// # 示例
    ///
    /// ```rust
    /// use piper_driver::recording::AsyncRecordingHook;
    /// use piper_driver::hooks::FrameCallback;
    /// use piper_driver::state::PiperContext;
    /// use std::sync::Arc;
    ///
    /// // 创建上下文和录制钩子
    /// let context = PiperContext::new();
    /// let (hook, _rx) = AsyncRecordingHook::new();
    ///
    /// // 注册为回调
    /// if let Ok(mut hooks) = context.hooks.write() {
    ///     hooks.add_callback(Arc::new(hook) as Arc<dyn FrameCallback>);
    /// }
    /// ```
    pub hooks: Arc<RwLock<HookManager>>,
}

impl PiperContext {
    /// 创建新的上下文
    ///
    /// 初始化所有状态结构，包括：
    /// - 热数据（固定槽位快照）：`joint_position_monitor`, `end_pose_monitor`, `joint_dynamic_monitor`
    /// - 温数据（ArcSwap）：`robot_control`, `gripper`, `joint_driver_low_speed`
    /// - 冷数据（RwLock）：`collision_protection`, `joint_limit_config`, `joint_accel_config`, `end_limit_config`
    /// - FPS 统计：`fps_stats`
    ///
    /// # Example
    ///
    /// ```
    /// use piper_driver::PiperContext;
    ///
    /// let ctx = PiperContext::new();
    /// let joint_pos = ctx.capture_joint_position_monitor_snapshot();
    /// assert!(joint_pos.latest_complete().is_none());
    /// ```
    pub fn new() -> Self {
        Self {
            // 热数据：固定槽位快照，无锁读取
            joint_position_monitor: Arc::new(RealtimeSnapshotCell::new(
                JointPositionMonitorSnapshot::default(),
            )),
            end_pose_monitor: Arc::new(
                RealtimeSnapshotCell::new(EndPoseMonitorSnapshot::default()),
            ),
            motion_snapshot: Arc::new(RealtimeSnapshotCell::new(MotionSnapshot::default())),
            control_joint_position: Arc::new(RealtimeSnapshotCell::new(
                JointPositionState::default(),
            )),
            joint_dynamic_monitor: Arc::new(RealtimeSnapshotCell::new(
                JointDynamicMonitorSnapshot::default(),
            )),
            control_joint_dynamic: Arc::new(
                RealtimeSnapshotCell::new(JointDynamicState::default()),
            ),
            raw_motion_snapshot: Arc::new(RealtimeSnapshotCell::new(MotionSnapshot::default())),

            // 温数据：ArcSwap
            robot_control: Arc::new(ArcSwap::from_pointee(RobotControlState::default())),
            gripper: Arc::new(ArcSwap::from_pointee(GripperState::default())),
            joint_driver_low_speed: Arc::new(ArcSwap::from_pointee(
                JointDriverLowSpeedState::default(),
            )),

            // 冷数据：RwLock
            collision_protection: Arc::new(RwLock::new(CollisionProtectionState::default())),
            setting_response: Arc::new(RwLock::new(SettingResponseState::default())),
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
            fps_stats: Arc::new(ArcSwap::from_pointee(FpsStatistics::new())),

            // 连接监控：1秒超时（如果1秒内没有收到任何反馈帧，认为连接丢失）
            connection_monitor: crate::heartbeat::ConnectionMonitor::new(
                std::time::Duration::from_secs(1),
            ),
            first_timestamped_feedback_host_rx_mono_us: AtomicU64::new(0),

            // 钩子管理器（v1.2.1）
            hooks: Arc::new(RwLock::new(HookManager::new())),
        }
    }

    pub fn register_timestamped_robot_feedback(&self, host_rx_mono_us: u64) {
        let _ = self.first_timestamped_feedback_host_rx_mono_us.compare_exchange(
            0,
            host_rx_mono_us,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }

    pub fn first_timestamped_feedback_host_rx_mono_us(&self) -> u64 {
        self.first_timestamped_feedback_host_rx_mono_us.load(Ordering::Acquire)
    }

    /// 捕获运动状态快照（逻辑原子性）
    ///
    /// 虽然不能保证物理上的完全同步（因为CAN帧本身就不是同时到的），
    /// 但可以保证逻辑上的原子性（在同一时刻读取多个状态）。
    ///
    /// **注意**：返回的状态可能来自不同的CAN传输周期。
    ///
    /// **性能**：返回栈上对象，开销极小（固定槽位读 + 按值复制）
    ///
    /// # Example
    ///
    /// ```
    /// use piper_driver::PiperContext;
    ///
    /// let ctx = PiperContext::new();
    /// let snapshot = ctx.capture_motion_snapshot();
    /// println!("Joint positions: {:?}", snapshot.joint_position.joint_pos);
    /// println!("End pose: {:?}", snapshot.end_pose.end_pose);
    /// ```
    pub fn capture_motion_snapshot(&self) -> MotionSnapshot {
        self.motion_snapshot.load()
    }

    /// 捕获关节位置监控快照（完整监控 + raw 诊断）
    pub fn capture_joint_position_monitor_snapshot(&self) -> JointPositionMonitorSnapshot {
        self.joint_position_monitor.load()
    }

    /// 捕获末端位姿监控快照（完整监控 + raw 诊断）
    pub fn capture_end_pose_monitor_snapshot(&self) -> EndPoseMonitorSnapshot {
        self.end_pose_monitor.load()
    }

    /// 捕获关节动态监控快照（完整监控 + raw 诊断）
    pub fn capture_joint_dynamic_monitor_snapshot(&self) -> JointDynamicMonitorSnapshot {
        self.joint_dynamic_monitor.load()
    }

    /// 捕获原始运动状态快照（允许部分帧组，仅供诊断）
    pub fn capture_raw_motion_snapshot(&self) -> MotionSnapshot {
        self.raw_motion_snapshot.load()
    }

    pub(crate) fn capture_control_joint_position(&self) -> JointPositionState {
        self.control_joint_position.load()
    }

    pub(crate) fn capture_control_joint_dynamic(&self) -> JointDynamicState {
        self.control_joint_dynamic.load()
    }

    /// 发布新的关节位置完整监控快照，并与当前末端位姿组合成逻辑原子快照。
    pub fn publish_joint_position(&self, joint_position: JointPositionState) {
        let end_pose = self.end_pose_monitor.load();
        self.joint_position_monitor
            .store(JointPositionMonitorSnapshot::from_complete(joint_position));
        self.motion_snapshot.store(MotionSnapshot {
            joint_position,
            end_pose: end_pose.latest_complete_cloned().unwrap_or_default(),
        });
        self.raw_motion_snapshot.store(MotionSnapshot {
            joint_position,
            end_pose: *end_pose.latest_raw(),
        });
    }

    /// 发布新的原始关节位置，并与当前原始末端位姿组合成逻辑原子快照。
    pub fn publish_raw_joint_position(&self, joint_position: JointPositionState) {
        let current = self.joint_position_monitor.load();
        let end_pose = self.end_pose_monitor.load();
        self.joint_position_monitor.store(JointPositionMonitorSnapshot::with_raw(
            current.latest_complete,
            joint_position,
        ));
        self.raw_motion_snapshot.store(MotionSnapshot {
            joint_position,
            end_pose: *end_pose.latest_raw(),
        });
    }

    /// 发布新的末端位姿完整监控快照，并与当前关节位置组合成逻辑原子快照。
    pub fn publish_end_pose(&self, end_pose: EndPoseState) {
        let joint_position = self.joint_position_monitor.load();
        self.end_pose_monitor.store(EndPoseMonitorSnapshot::from_complete(end_pose));
        self.motion_snapshot.store(MotionSnapshot {
            joint_position: joint_position.latest_complete_cloned().unwrap_or_default(),
            end_pose,
        });
        self.raw_motion_snapshot.store(MotionSnapshot {
            joint_position: *joint_position.latest_raw(),
            end_pose,
        });
    }

    /// 发布新的控制级关节位置。
    pub fn publish_control_joint_position(&self, joint_position: JointPositionState) {
        self.control_joint_position.store(joint_position);
    }

    /// 发布新的原始末端位姿，并与当前原始关节位置组合成逻辑原子快照。
    pub fn publish_raw_end_pose(&self, end_pose: EndPoseState) {
        let current = self.end_pose_monitor.load();
        let joint_position = self.joint_position_monitor.load();
        self.end_pose_monitor.store(EndPoseMonitorSnapshot::with_raw(
            current.latest_complete,
            end_pose,
        ));
        self.raw_motion_snapshot.store(MotionSnapshot {
            joint_position: *joint_position.latest_raw(),
            end_pose,
        });
    }

    /// 发布新的完整关节动态监控快照。
    pub fn publish_joint_dynamic(&self, joint_dynamic: JointDynamicState) {
        self.joint_dynamic_monitor
            .store(JointDynamicMonitorSnapshot::from_complete(joint_dynamic));
    }

    /// 发布新的控制级关节动态状态。
    pub fn publish_control_joint_dynamic(&self, joint_dynamic: JointDynamicState) {
        self.control_joint_dynamic.store(joint_dynamic);
    }

    /// 发布新的原始关节动态状态。
    pub fn publish_raw_joint_dynamic(&self, joint_dynamic: JointDynamicState) {
        let current = self.joint_dynamic_monitor.load();
        self.joint_dynamic_monitor.store(JointDynamicMonitorSnapshot::with_raw(
            current.latest_complete,
            joint_dynamic,
        ));
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
    pub position_timestamp_us: u64,
    pub dynamic_timestamp_us: u64,
    pub position_host_rx_mono_us: u64,
    pub dynamic_host_rx_mono_us: u64,
    pub position_frame_valid_mask: u8,
    pub dynamic_valid_mask: u8,
    pub dynamic_group_span_us: u64,
    pub skew_us: i64,
}

impl AlignedMotionState {
    /// 位置反馈帧组是否完整（0x2A5-0x2A7 都已到达）。
    pub fn position_complete(&self) -> bool {
        self.position_frame_valid_mask == 0b0000_0111
    }

    /// 动态反馈组是否完整（J1-J6 都已到达）。
    pub fn dynamic_complete(&self) -> bool {
        self.dynamic_valid_mask == 0b0011_1111
    }

    /// 位置和动态反馈是否都完整。
    pub fn is_complete(&self) -> bool {
        self.position_complete() && self.dynamic_complete()
    }
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
struct TestCountingAllocator;

#[cfg(test)]
#[global_allocator]
static TEST_ALLOCATOR: TestCountingAllocator = TestCountingAllocator;

#[cfg(test)]
std::thread_local! {
    static TEST_ALLOC_COUNTING_DEPTH: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static TEST_ALLOC_COUNT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn record_test_allocation() {
    TEST_ALLOC_COUNTING_DEPTH.with(|depth| {
        if depth.get() > 0 {
            TEST_ALLOC_COUNT.with(|count| count.set(count.get().saturating_add(1)));
        }
    });
}

#[cfg(test)]
unsafe impl std::alloc::GlobalAlloc for TestCountingAllocator {
    unsafe fn alloc(&self, layout: std::alloc::Layout) -> *mut u8 {
        record_test_allocation();
        // SAFETY: 直接委托给系统分配器。
        unsafe { std::alloc::System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: std::alloc::Layout) {
        // SAFETY: 直接委托给系统分配器。
        unsafe { std::alloc::System.dealloc(ptr, layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: std::alloc::Layout) -> *mut u8 {
        record_test_allocation();
        // SAFETY: 直接委托给系统分配器。
        unsafe { std::alloc::System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: std::alloc::Layout, new_size: usize) -> *mut u8 {
        record_test_allocation();
        // SAFETY: 直接委托给系统分配器。
        unsafe { std::alloc::System.realloc(ptr, layout, new_size) }
    }
}

#[cfg(test)]
fn count_thread_allocations<F>(f: F) -> usize
where
    F: FnOnce(),
{
    struct CountingGuard;

    impl Drop for CountingGuard {
        fn drop(&mut self) {
            TEST_ALLOC_COUNTING_DEPTH.with(|depth| depth.set(depth.get().saturating_sub(1)));
        }
    }

    TEST_ALLOC_COUNT.with(|count| count.set(0));
    TEST_ALLOC_COUNTING_DEPTH.with(|depth| depth.set(depth.get().saturating_add(1)));
    let _guard = CountingGuard;
    f();
    TEST_ALLOC_COUNT.with(|count| count.get())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::PI;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::{Arc, Barrier};
    use std::thread;
    use std::time::Duration;

    #[derive(Debug, Clone, Copy)]
    struct SequenceSnapshot {
        seq: u64,
        seq_complement: u64,
    }

    impl Default for SequenceSnapshot {
        fn default() -> Self {
            Self::new(0)
        }
    }

    impl SequenceSnapshot {
        fn new(seq: u64) -> Self {
            Self {
                seq,
                seq_complement: !seq,
            }
        }

        fn is_valid(self) -> bool {
            self.seq_complement == !self.seq
        }
    }

    fn sample_joint_position_state(seq: u64, mask: u8) -> JointPositionState {
        JointPositionState {
            hardware_timestamp_us: seq,
            host_rx_mono_us: seq.saturating_add(1_000),
            joint_pos: std::array::from_fn(|index| seq as f64 + index as f64),
            frame_valid_mask: mask,
        }
    }

    fn sample_end_pose_state(seq: u64, mask: u8) -> EndPoseState {
        EndPoseState {
            hardware_timestamp_us: seq,
            host_rx_mono_us: seq.saturating_add(2_000),
            end_pose: std::array::from_fn(|index| (seq + index as u64) as f64 / 10.0),
            frame_valid_mask: mask,
        }
    }

    fn sample_joint_dynamic_state(seq: u64, mask: u8) -> JointDynamicState {
        JointDynamicState {
            group_timestamp_us: seq,
            group_host_rx_mono_us: seq.saturating_add(3_000),
            joint_vel: std::array::from_fn(|index| seq as f64 + index as f64 * 0.1),
            joint_current: std::array::from_fn(|index| seq as f64 + index as f64 * 0.01),
            timestamps: std::array::from_fn(|index| seq.saturating_add(index as u64)),
            valid_mask: mask,
        }
    }

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
    fn test_register_timestamped_robot_feedback_keeps_first_value() {
        let ctx = PiperContext::new();
        ctx.register_timestamped_robot_feedback(123);
        ctx.register_timestamped_robot_feedback(456);

        assert_eq!(ctx.first_timestamped_feedback_host_rx_mono_us(), 123);
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
        assert_eq!(state.group_host_rx_mono_us, 0);
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
    #[test]
    fn test_piper_context_new() {
        let ctx = PiperContext::new();
        // 验证热路径快照、温数据和冷数据都已初始化
        let joint_pos = ctx.capture_joint_position_monitor_snapshot();
        assert!(joint_pos.latest_complete().is_none());
        assert_eq!(joint_pos.latest_raw().hardware_timestamp_us, 0);

        let end_pose = ctx.capture_end_pose_monitor_snapshot();
        assert!(end_pose.latest_complete().is_none());
        assert_eq!(end_pose.latest_raw().hardware_timestamp_us, 0);

        let joint_dynamic = ctx.capture_joint_dynamic_monitor_snapshot();
        assert!(joint_dynamic.latest_complete().is_none());
        assert_eq!(joint_dynamic.latest_raw().group_timestamp_us, 0);

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
            group_host_rx_mono_us: 2000,
            joint_vel: [1.0, 2.0, 3.0, 4.0, 5.0, 6.0],
            joint_current: [0.1, 0.2, 0.3, 0.4, 0.5, 0.6],
            timestamps: [100, 200, 300, 400, 500, 600],
            valid_mask: 0b111111,
        };
        let cloned = state;
        assert_eq!(state.group_timestamp_us, cloned.group_timestamp_us);
        assert_eq!(state.group_host_rx_mono_us, cloned.group_host_rx_mono_us);
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
            position_timestamp_us: 1000,
            dynamic_timestamp_us: 1500,
            dynamic_group_span_us: 0,
            position_host_rx_mono_us: 2000,
            dynamic_host_rx_mono_us: 2500,
            position_frame_valid_mask: 0b111,
            dynamic_valid_mask: 0b111111,
            skew_us: 500,
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
            position_timestamp_us: 1000,
            dynamic_timestamp_us: 1500,
            dynamic_group_span_us: 0,
            position_host_rx_mono_us: 2000,
            dynamic_host_rx_mono_us: 2500,
            position_frame_valid_mask: 0b111,
            dynamic_valid_mask: 0b111111,
            skew_us: 500,
        };
        let result_ok = AlignmentResult::Ok(state);
        let debug_str = format!("{:?}", result_ok);
        assert!(debug_str.contains("Ok") || debug_str.contains("AlignmentResult"));

        let state2 = AlignedMotionState {
            joint_pos: [1.0; 6],
            joint_vel: [2.0; 6],
            joint_current: [3.0; 6],
            position_timestamp_us: 1000,
            dynamic_timestamp_us: 1500,
            dynamic_group_span_us: 0,
            position_host_rx_mono_us: 2000,
            dynamic_host_rx_mono_us: 2500,
            position_frame_valid_mask: 0b111,
            dynamic_valid_mask: 0b111111,
            skew_us: 500,
        };
        let result_mis = AlignmentResult::Misaligned {
            state: state2,
            diff_us: 10000,
        };
        let debug_str2 = format!("{:?}", result_mis);
        assert!(debug_str2.contains("Misaligned") || debug_str2.contains("AlignmentResult"));
    }

    #[test]
    fn test_aligned_motion_state_completeness_helpers() {
        let state = AlignedMotionState {
            joint_pos: [0.0; 6],
            joint_vel: [0.0; 6],
            joint_current: [0.0; 6],
            position_timestamp_us: 0,
            dynamic_timestamp_us: 0,
            dynamic_group_span_us: 0,
            position_host_rx_mono_us: 0,
            dynamic_host_rx_mono_us: 0,
            position_frame_valid_mask: 0b111,
            dynamic_valid_mask: 0b111111,
            skew_us: 0,
        };
        assert!(state.position_complete());
        assert!(state.dynamic_complete());
        assert!(state.is_complete());

        let incomplete = AlignedMotionState {
            position_frame_valid_mask: 0b011,
            dynamic_valid_mask: 0b011111,
            ..state
        };
        assert!(!incomplete.position_complete());
        assert!(!incomplete.dynamic_complete());
        assert!(!incomplete.is_complete());
    }

    // ============================================================
    // 测试新状态结构：JointPositionState 和 EndPoseState
    // ============================================================

    #[test]
    fn test_joint_position_state_default() {
        let state = JointPositionState::default();
        assert_eq!(state.hardware_timestamp_us, 0);
        assert_eq!(state.host_rx_mono_us, 0);
        assert_eq!(state.joint_pos, [0.0; 6]);
        assert_eq!(state.frame_valid_mask, 0);
    }

    #[test]
    fn test_joint_position_state_is_fully_valid() {
        // 完整帧组（所有3帧都收到）
        let state = JointPositionState {
            hardware_timestamp_us: 1000,
            host_rx_mono_us: 2000,
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
            host_rx_mono_us: 2000,
            joint_pos: [1.0, 2.0, 3.0, 4.0, 5.0, 6.0],
            frame_valid_mask: 0b0000_0111,
        };
        let cloned = state;
        assert_eq!(state.hardware_timestamp_us, cloned.hardware_timestamp_us);
        assert_eq!(state.host_rx_mono_us, cloned.host_rx_mono_us);
        assert_eq!(state.joint_pos, cloned.joint_pos);
        assert_eq!(state.frame_valid_mask, cloned.frame_valid_mask);
        assert_eq!(state.is_fully_valid(), cloned.is_fully_valid());
    }

    #[test]
    fn test_end_pose_state_default() {
        let state = EndPoseState::default();
        assert_eq!(state.hardware_timestamp_us, 0);
        assert_eq!(state.host_rx_mono_us, 0);
        assert_eq!(state.end_pose, [0.0; 6]);
        assert_eq!(state.frame_valid_mask, 0);
    }

    #[test]
    fn test_end_pose_state_is_fully_valid() {
        // 完整帧组（所有3帧都收到）
        let state = EndPoseState {
            hardware_timestamp_us: 1000,
            host_rx_mono_us: 2000,
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
            host_rx_mono_us: 2000,
            end_pose: [0.1, 0.2, 0.3, 0.4, 0.5, 0.6],
            frame_valid_mask: 0b0000_0111,
        };
        let cloned = state;
        assert_eq!(state.hardware_timestamp_us, cloned.hardware_timestamp_us);
        assert_eq!(state.host_rx_mono_us, cloned.host_rx_mono_us);
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
                host_rx_mono_us: 2000,
                joint_pos: [1.0, 2.0, 3.0, 4.0, 5.0, 6.0],
                frame_valid_mask: 0b0000_0111,
            },
            end_pose: EndPoseState {
                hardware_timestamp_us: 1500,
                host_rx_mono_us: 2500,
                end_pose: [0.1, 0.2, 0.3, 0.4, 0.5, 0.6],
                frame_valid_mask: 0b0000_0111,
            },
        };
        let cloned = snapshot;
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
    fn test_piper_context_motion_snapshot_matches_published_state() {
        let ctx = PiperContext::new();

        let joint_position = JointPositionState {
            hardware_timestamp_us: 101,
            host_rx_mono_us: 202,
            joint_pos: [1.0; 6],
            frame_valid_mask: 0b111,
        };
        ctx.publish_joint_position(joint_position);

        let snapshot_after_joint = ctx.capture_motion_snapshot();
        assert_eq!(
            snapshot_after_joint.joint_position.hardware_timestamp_us,
            joint_position.hardware_timestamp_us
        );
        assert_eq!(snapshot_after_joint.end_pose.hardware_timestamp_us, 0);

        let end_pose = EndPoseState {
            hardware_timestamp_us: 303,
            host_rx_mono_us: 404,
            end_pose: [2.0; 6],
            frame_valid_mask: 0b111,
        };
        ctx.publish_end_pose(end_pose);

        let snapshot_after_end = ctx.capture_motion_snapshot();
        assert_eq!(
            snapshot_after_end.joint_position.hardware_timestamp_us,
            joint_position.hardware_timestamp_us
        );
        assert_eq!(
            snapshot_after_end.end_pose.hardware_timestamp_us,
            end_pose.hardware_timestamp_us
        );
        assert_eq!(snapshot_after_end.end_pose.end_pose, end_pose.end_pose);
    }

    #[test]
    fn test_piper_context_new_states() {
        let ctx = PiperContext::new();

        // 验证新状态字段存在且为默认值
        let joint_pos = ctx.capture_joint_position_monitor_snapshot();
        assert!(joint_pos.latest_complete().is_none());
        assert_eq!(joint_pos.latest_raw().hardware_timestamp_us, 0);

        let end_pose = ctx.capture_end_pose_monitor_snapshot();
        assert!(end_pose.latest_complete().is_none());
        assert_eq!(end_pose.latest_raw().hardware_timestamp_us, 0);
    }

    #[test]
    fn test_joint_position_monitor_snapshot_preserves_complete_on_raw_updates() {
        let ctx = PiperContext::new();
        let complete = JointPositionState {
            hardware_timestamp_us: 100,
            host_rx_mono_us: 200,
            joint_pos: [1.0; 6],
            frame_valid_mask: 0b111,
        };
        ctx.publish_joint_position(complete);

        let first = ctx.capture_joint_position_monitor_snapshot();
        assert_eq!(
            first.latest_complete().expect("complete snapshot should exist").joint_pos,
            [1.0; 6]
        );
        assert_eq!(first.latest_raw().joint_pos, [1.0; 6]);

        ctx.publish_raw_joint_position(JointPositionState {
            hardware_timestamp_us: 101,
            host_rx_mono_us: 201,
            joint_pos: [2.0; 6],
            frame_valid_mask: 0b001,
        });

        let second = ctx.capture_joint_position_monitor_snapshot();
        assert_eq!(second.latest_raw().frame_valid_mask, 0b001);
        assert_eq!(
            second.latest_complete().expect("complete snapshot should remain").joint_pos,
            [1.0; 6]
        );
    }

    #[test]
    fn test_end_pose_monitor_snapshot_preserves_complete_on_raw_updates() {
        let ctx = PiperContext::new();
        let complete = EndPoseState {
            hardware_timestamp_us: 300,
            host_rx_mono_us: 400,
            end_pose: [1.5; 6],
            frame_valid_mask: 0b111,
        };
        ctx.publish_end_pose(complete);

        let first = ctx.capture_end_pose_monitor_snapshot();
        assert_eq!(
            first.latest_complete().expect("complete snapshot should exist").end_pose,
            [1.5; 6]
        );
        assert_eq!(first.latest_raw().end_pose, [1.5; 6]);

        ctx.publish_raw_end_pose(EndPoseState {
            hardware_timestamp_us: 301,
            host_rx_mono_us: 401,
            end_pose: [2.5; 6],
            frame_valid_mask: 0b001,
        });

        let second = ctx.capture_end_pose_monitor_snapshot();
        assert_eq!(second.latest_raw().frame_valid_mask, 0b001);
        assert_eq!(
            second.latest_complete().expect("complete snapshot should remain").end_pose,
            [1.5; 6]
        );
    }

    #[test]
    fn test_joint_dynamic_monitor_snapshot_preserves_complete_on_raw_updates() {
        let ctx = PiperContext::new();
        let complete = JointDynamicState {
            group_timestamp_us: 500,
            group_host_rx_mono_us: 600,
            joint_vel: [1.0; 6],
            joint_current: [2.0; 6],
            timestamps: [10; 6],
            valid_mask: 0b11_1111,
        };
        ctx.publish_joint_dynamic(complete);

        let first = ctx.capture_joint_dynamic_monitor_snapshot();
        assert_eq!(
            first.latest_complete().expect("complete snapshot should exist").valid_mask,
            0b11_1111
        );
        assert_eq!(first.latest_raw().valid_mask, 0b11_1111);

        ctx.publish_raw_joint_dynamic(JointDynamicState {
            group_timestamp_us: 501,
            group_host_rx_mono_us: 601,
            joint_vel: [3.0; 6],
            joint_current: [4.0; 6],
            timestamps: [20; 6],
            valid_mask: 0b000001,
        });

        let second = ctx.capture_joint_dynamic_monitor_snapshot();
        assert_eq!(second.latest_raw().valid_mask, 0b000001);
        assert_eq!(
            second.latest_complete().expect("complete snapshot should remain").valid_mask,
            0b11_1111
        );
    }

    #[test]
    fn test_capture_raw_motion_snapshot_tracks_partial_updates() {
        let ctx = PiperContext::new();

        ctx.publish_raw_joint_position(sample_joint_position_state(10, 0b001));
        let snapshot_after_joint = ctx.capture_raw_motion_snapshot();
        assert_eq!(snapshot_after_joint.joint_position.frame_valid_mask, 0b001);
        assert_eq!(snapshot_after_joint.end_pose.frame_valid_mask, 0);

        ctx.publish_raw_end_pose(sample_end_pose_state(20, 0b010));
        let snapshot_after_end = ctx.capture_raw_motion_snapshot();
        assert_eq!(snapshot_after_end.joint_position.frame_valid_mask, 0b001);
        assert_eq!(snapshot_after_end.end_pose.frame_valid_mask, 0b010);
        assert_eq!(snapshot_after_end.end_pose.hardware_timestamp_us, 20);
    }

    #[test]
    fn test_control_grade_cells_return_latest_published_value() {
        let ctx = PiperContext::new();

        let joint_position = sample_joint_position_state(42, 0b111);
        let joint_dynamic = sample_joint_dynamic_state(84, 0b11_1111);

        ctx.publish_control_joint_position(joint_position);
        ctx.publish_control_joint_dynamic(joint_dynamic);

        assert_eq!(
            ctx.capture_control_joint_position().hardware_timestamp_us,
            42
        );
        assert_eq!(ctx.capture_control_joint_position().frame_valid_mask, 0b111);
        assert_eq!(ctx.capture_control_joint_dynamic().group_timestamp_us, 84);
        assert_eq!(ctx.capture_control_joint_dynamic().valid_mask, 0b11_1111);
    }

    #[test]
    fn test_realtime_snapshot_cell_single_writer_multi_reader_stress() {
        const WRITES: u64 = 20_000;
        const READERS: usize = 4;

        let cell = Arc::new(RealtimeSnapshotCell::<SequenceSnapshot>::default());
        let start = Arc::new(Barrier::new(READERS + 1));
        let stop = Arc::new(AtomicBool::new(false));
        let max_written = Arc::new(AtomicU64::new(0));

        let writer_cell = Arc::clone(&cell);
        let writer_start = Arc::clone(&start);
        let writer_stop = Arc::clone(&stop);
        let writer_max = Arc::clone(&max_written);
        let writer = thread::spawn(move || {
            writer_start.wait();
            for seq in 1..=WRITES {
                writer_max.store(seq, Ordering::Release);
                writer_cell.store(SequenceSnapshot::new(seq));
            }
            writer_stop.store(true, Ordering::Release);
        });

        let mut readers = Vec::new();
        for _ in 0..READERS {
            let reader_cell = Arc::clone(&cell);
            let reader_start = Arc::clone(&start);
            let reader_stop = Arc::clone(&stop);
            let reader_max = Arc::clone(&max_written);
            readers.push(thread::spawn(move || {
                let mut last_seen = 0;
                reader_start.wait();

                loop {
                    let snapshot = reader_cell.load();
                    assert!(snapshot.is_valid(), "reader observed torn snapshot");
                    let max_seen = reader_max.load(Ordering::Acquire);
                    assert!(
                        snapshot.seq <= max_seen,
                        "reader observed unpublished snapshot: {} > {}",
                        snapshot.seq,
                        max_seen
                    );
                    assert!(
                        snapshot.seq >= last_seen,
                        "reader observed rollback: {} < {}",
                        snapshot.seq,
                        last_seen
                    );
                    last_seen = snapshot.seq;

                    if reader_stop.load(Ordering::Acquire)
                        && last_seen >= reader_max.load(Ordering::Acquire)
                    {
                        break;
                    }
                }
            }));
        }

        writer.join().unwrap();
        for reader in readers {
            reader.join().unwrap();
        }

        assert_eq!(cell.load().seq, WRITES);
    }

    #[test]
    fn test_realtime_snapshot_cell_waits_for_free_slot_before_reuse() {
        let cell = Arc::new(RealtimeSnapshotCell::<SequenceSnapshot>::default());

        let held_oldest = cell.pin_slot_for_test(cell.published_slot_for_test());
        cell.store(SequenceSnapshot::new(1));
        let held_middle = cell.pin_slot_for_test(cell.published_slot_for_test());
        cell.store(SequenceSnapshot::new(2));

        assert_ne!(held_oldest.slot(), held_middle.slot());

        let blocked_published_slot = cell.published_slot_for_test();
        let finished = Arc::new(AtomicBool::new(false));

        let writer_cell = Arc::clone(&cell);
        let writer_finished = Arc::clone(&finished);
        let writer = thread::spawn(move || {
            writer_cell.store(SequenceSnapshot::new(3));
            writer_finished.store(true, Ordering::Release);
        });

        thread::sleep(Duration::from_millis(10));
        assert!(
            !finished.load(Ordering::Acquire),
            "writer should wait until one spare slot is released"
        );
        assert_eq!(cell.published_slot_for_test(), blocked_published_slot);

        drop(held_middle);
        writer.join().unwrap();

        assert!(finished.load(Ordering::Acquire));
        assert_eq!(cell.load().seq, 3);
    }

    #[test]
    fn test_hot_publish_paths_do_not_allocate() {
        let ctx = PiperContext::new();

        ctx.publish_joint_position(sample_joint_position_state(1, 0b111));
        ctx.publish_raw_joint_dynamic(sample_joint_dynamic_state(2, 0b000001));
        ctx.publish_control_joint_dynamic(sample_joint_dynamic_state(3, 0b11_1111));

        let allocations = count_thread_allocations(|| {
            for seq in 10..1_010 {
                ctx.publish_joint_position(sample_joint_position_state(seq, 0b111));
                ctx.publish_raw_joint_dynamic(sample_joint_dynamic_state(seq, 0b000001));
                ctx.publish_control_joint_dynamic(sample_joint_dynamic_state(seq, 0b11_1111));
            }
        });

        assert_eq!(allocations, 0, "hot publish path should not allocate");
    }

    // ============================================================
    // 测试新状态结构：GripperState 和 RobotControlState
    // ============================================================

    #[test]
    fn test_gripper_state_default() {
        let state = GripperState::default();
        assert_eq!(state.hardware_timestamp_us, 0);
        assert_eq!(state.host_rx_mono_us, 0);
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
            host_rx_mono_us: 2000,
            travel: 50.5,
            torque: 2.5,
            status_code: 0b1100_0011,
            last_travel: 50.0,
        };
        let cloned = state.clone();
        assert_eq!(state.hardware_timestamp_us, cloned.hardware_timestamp_us);
        assert_eq!(state.host_rx_mono_us, cloned.host_rx_mono_us);
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
        assert_eq!(state.host_rx_mono_us, 0);
        assert_eq!(state.control_mode, 0);
        assert_eq!(state.robot_status, 0);
        assert_eq!(state.fault_angle_limit_mask, 0);
        assert_eq!(state.fault_comm_error_mask, 0);
        assert_eq!(state.driver_enabled_mask, 0);
        assert!(!state.any_drive_enabled);
        assert_eq!(state.feedback_counter, 0);
        assert!(!state.is_enabled);
        assert_eq!(state.confirmed_driver_enabled_mask, None);
        assert!(!state.is_fully_enabled_confirmed());
        assert!(!state.is_fully_disabled_confirmed());
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
            host_rx_mono_us: 2000,
            control_mode: 1,
            robot_status: 2,
            move_mode: 3,
            teach_status: 4,
            motion_status: 5,
            trajectory_point_index: 10,
            fault_angle_limit_mask: 0b0011_0001,
            fault_comm_error_mask: 0b0000_0100,
            driver_enabled_mask: 0b11_1111,
            any_drive_enabled: true,
            is_enabled: true,
            confirmed_driver_enabled_mask: Some(0b11_1111),
            feedback_counter: 5,
        };
        let cloned = state.clone();
        assert_eq!(state.hardware_timestamp_us, cloned.hardware_timestamp_us);
        assert_eq!(state.control_mode, cloned.control_mode);
        assert_eq!(state.fault_angle_limit_mask, cloned.fault_angle_limit_mask);
        assert_eq!(state.fault_comm_error_mask, cloned.fault_comm_error_mask);
        assert_eq!(state.driver_enabled_mask, cloned.driver_enabled_mask);
        assert_eq!(state.any_drive_enabled, cloned.any_drive_enabled);
        assert_eq!(state.is_enabled, cloned.is_enabled);
        assert_eq!(
            state.confirmed_driver_enabled_mask,
            cloned.confirmed_driver_enabled_mask
        );
        assert_eq!(state.is_angle_limit(0), cloned.is_angle_limit(0));
        assert_eq!(state.is_comm_error(2), cloned.is_comm_error(2));
        assert!(cloned.is_fully_enabled_confirmed());
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
        assert_eq!(state.host_rx_mono_us, 0);
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
            host_rx_mono_us: 2000,
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
            host_rx_mono_timestamps: [1100, 1200, 1300, 1400, 1500, 1600],
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
    fn test_joint_driver_low_speed_state_confirmed_driver_enabled_mask_requires_all_fresh_joints() {
        let now_host_mono_us = 20_000;
        let freshness_window_us = 5_000;
        let state = JointDriverLowSpeedState {
            driver_enabled_mask: 0b11_1111,
            host_rx_mono_timestamps: [19_500, 19_600, 19_700, 19_800, 19_900, 20_000],
            valid_mask: 0b11_1111,
            ..Default::default()
        };

        assert_eq!(
            state.confirmed_driver_enabled_mask(now_host_mono_us, freshness_window_us),
            Some(0b11_1111)
        );

        let partial_refresh_only = JointDriverLowSpeedState {
            driver_enabled_mask: 0b11_1111,
            host_rx_mono_timestamps: [19_500, 10_000, 10_000, 10_000, 10_000, 10_000],
            valid_mask: 0b11_1111,
            ..Default::default()
        };
        assert_eq!(
            partial_refresh_only
                .confirmed_driver_enabled_mask(now_host_mono_us, freshness_window_us),
            None
        );
    }

    #[test]
    fn test_joint_driver_low_speed_state_confirmed_driver_enabled_mask_requires_all_timestamps() {
        let state = JointDriverLowSpeedState {
            driver_enabled_mask: 0,
            host_rx_mono_timestamps: [1_000, 1_100, 1_200, 1_300, 1_400, 0],
            ..Default::default()
        };

        assert_eq!(state.confirmed_driver_enabled_mask(2_000, 2_000), None);
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
        assert_eq!(state.host_rx_mono_us, 0);
        assert_eq!(state.protection_levels, [0; 6]);
    }

    #[test]
    fn test_collision_protection_state_clone() {
        let state = CollisionProtectionState {
            hardware_timestamp_us: 1000,
            host_rx_mono_us: 2000,
            protection_levels: [5, 5, 5, 4, 4, 4],
        };
        let cloned = state.clone();
        assert_eq!(state.hardware_timestamp_us, cloned.hardware_timestamp_us);
        assert_eq!(state.host_rx_mono_us, cloned.host_rx_mono_us);
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
        assert_eq!(state.host_rx_mono_us, 0);
        assert_eq!(state.protection_levels, [0; 6]);
    }

    #[test]
    fn test_setting_response_state_default() {
        let state = SettingResponseState::default();
        assert_eq!(state.hardware_timestamp_us, 0);
        assert_eq!(state.host_rx_mono_us, 0);
        assert_eq!(state.response_index, 0);
        assert!(!state.zero_point_success);
        assert!(!state.is_valid);
    }

    #[test]
    fn test_setting_response_state_clone() {
        let state = SettingResponseState {
            hardware_timestamp_us: 1000,
            host_rx_mono_us: 2000,
            response_index: 0x75,
            zero_point_success: true,
            is_valid: true,
        };
        let cloned = state.clone();
        assert_eq!(state.hardware_timestamp_us, cloned.hardware_timestamp_us);
        assert_eq!(state.host_rx_mono_us, cloned.host_rx_mono_us);
        assert_eq!(state.response_index, cloned.response_index);
        assert_eq!(state.zero_point_success, cloned.zero_point_success);
        assert_eq!(state.is_valid, cloned.is_valid);
    }

    #[test]
    fn test_piper_context_setting_response() {
        let ctx = PiperContext::new();

        let state = ctx.setting_response.read().unwrap();
        assert_eq!(state.hardware_timestamp_us, 0);
        assert_eq!(state.host_rx_mono_us, 0);
        assert_eq!(state.response_index, 0);
        assert!(!state.zero_point_success);
        assert!(!state.is_valid);
    }

    // ============================================================
    // 测试新状态结构：JointLimitConfigState
    // ============================================================

    #[test]
    fn test_joint_limit_config_state_default() {
        let state = JointLimitConfigState::default();
        assert_eq!(state.last_update_hardware_timestamp_us, 0);
        assert_eq!(state.last_update_host_rx_mono_us, 0);
        assert_eq!(state.joint_limits_max, [0.0; 6]);
        assert_eq!(state.joint_limits_min, [0.0; 6]);
        assert_eq!(state.joint_max_velocity, [0.0; 6]);
        assert_eq!(state.joint_update_hardware_timestamps, [0; 6]);
        assert_eq!(state.joint_update_host_rx_mono_timestamps, [0; 6]);
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
            last_update_host_rx_mono_us: 2000,
            joint_limits_max: [1.57, 1.57, 1.57, 1.57, 1.57, 1.57], // 90度 = π/2 弧度
            joint_limits_min: [-1.57, -1.57, -1.57, -1.57, -1.57, -1.57], // -90度
            joint_max_velocity: [PI, PI, PI, PI, PI, PI],           // 180度/s = π rad/s
            joint_update_hardware_timestamps: [100, 200, 300, 400, 500, 600],
            joint_update_host_rx_mono_timestamps: [1100, 1200, 1300, 1400, 1500, 1600],
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
        assert_eq!(state.last_update_host_rx_mono_us, 0);
        assert_eq!(state.max_acc_limits, [0.0; 6]);
        assert_eq!(state.joint_update_hardware_timestamps, [0; 6]);
        assert_eq!(state.joint_update_host_rx_mono_timestamps, [0; 6]);
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
            last_update_host_rx_mono_us: 2000,
            max_acc_limits: [10.0, 10.0, 10.0, 10.0, 10.0, 10.0], // 10 rad/s²
            joint_update_hardware_timestamps: [100, 200, 300, 400, 500, 600],
            joint_update_host_rx_mono_timestamps: [1100, 1200, 1300, 1400, 1500, 1600],
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
        assert_eq!(state.last_update_host_rx_mono_us, 0);
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
            last_update_host_rx_mono_us: 2000,
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

    // ============================================================
    // 测试固件版本状态：FirmwareVersionState
    // ============================================================

    #[test]
    fn test_firmware_version_state_default() {
        let state = FirmwareVersionState::default();
        assert_eq!(state.hardware_timestamp_us, 0);
        assert_eq!(state.host_rx_mono_us, 0);
        assert!(state.firmware_data.is_empty());
        assert!(!state.is_complete);
        assert!(state.version_string.is_none());
    }

    #[test]
    fn test_firmware_version_state_clear() {
        let mut state = FirmwareVersionState {
            hardware_timestamp_us: 1000,
            host_rx_mono_us: 2000,
            firmware_data: vec![b'S', b'-', b'V', b'1', b'.', b'6', b'-', b'3'],
            is_complete: true,
            version_string: Some("S-V1.6-3".to_string()),
        };

        state.clear();

        assert_eq!(state.hardware_timestamp_us, 0);
        assert_eq!(state.host_rx_mono_us, 0);
        assert!(state.firmware_data.is_empty());
        assert!(!state.is_complete);
        assert!(state.version_string.is_none());
    }

    #[test]
    fn test_firmware_version_state_check_completeness() {
        // 测试：没有 S-V 标记
        let mut state = FirmwareVersionState {
            firmware_data: b"Some data".to_vec(),
            ..Default::default()
        };
        assert!(!state.check_completeness());
        assert!(!state.is_complete);

        // 测试：有 S-V 标记但数据不足 8 字节
        state.firmware_data = b"S-V1.6".to_vec();
        assert!(!state.check_completeness());
        assert!(!state.is_complete);

        // 测试：有 S-V 标记且数据足够（正好 8 字节）
        state.firmware_data = b"S-V1.6-3".to_vec();
        assert!(state.check_completeness());
        assert!(state.is_complete);

        // 测试：有 S-V 标记且数据足够（超过 8 字节）
        state.firmware_data = b"Prefix S-V1.6-3\nSuffix".to_vec();
        assert!(state.check_completeness());
        assert!(state.is_complete);
    }

    #[test]
    fn test_firmware_version_state_parse_version() {
        // 测试：成功解析版本
        let mut state = FirmwareVersionState {
            firmware_data: b"Some prefix S-V1.6-3\nOther data".to_vec(),
            ..Default::default()
        };
        let version = state.parse_version();
        assert_eq!(version, Some("S-V1.6-3".to_string()));
        assert_eq!(state.version_string, Some("S-V1.6-3".to_string()));
        assert!(state.is_complete);

        // 测试：未找到版本
        state.firmware_data = b"Some data without version".to_vec();
        let version = state.parse_version();
        assert_eq!(version, None);
        assert!(state.version_string.is_none());
        assert!(!state.is_complete);
    }

    #[test]
    fn test_firmware_version_state_parse_version_requires_complete_payload() {
        let mut state = FirmwareVersionState {
            firmware_data: b"S-V1.6".to_vec(),
            ..Default::default()
        };

        let version = state.parse_version();
        assert_eq!(version, None);
        assert!(state.version_string.is_none());
        assert!(!state.is_complete);
    }

    #[test]
    fn test_firmware_version_state_version_string() {
        let mut state = FirmwareVersionState::default();

        // 测试：未解析时返回 None
        assert!(state.version_string().is_none());

        // 测试：解析后可以获取版本字符串
        state.firmware_data = b"S-V1.6-3".to_vec();
        state.parse_version();
        assert_eq!(state.version_string(), Some(&"S-V1.6-3".to_string()));
    }

    #[test]
    fn test_piper_context_firmware_version() {
        let ctx = PiperContext::new();

        // 验证 firmware_version 字段存在且为默认值
        let state = ctx.firmware_version.read().unwrap();
        assert_eq!(state.hardware_timestamp_us, 0);
        assert_eq!(state.host_rx_mono_us, 0);
        assert!(state.firmware_data.is_empty());
        assert!(!state.is_complete);
        assert!(state.version_string.is_none());
    }
}
