//! Piper SDK 性能指标模块
//!
//! 提供零开销的原子计数器，用于监控 IO 链路的健康状态和性能。
//! 所有计数器都使用原子操作，可以在任何线程安全地读取，不会引入锁竞争。

use crate::{
    DiagnosticEvent, FpsCounts, FpsResult, Piper, ProtocolDiagnostic, QueryDiagnostic, QueryKind,
};
use std::sync::atomic::{AtomicU64, Ordering};

/// 重建观察族指标的单族快照。
///
/// 这些指标明确区分：
/// - `raw_frame_rate`: 原始成员帧速率
/// - `complete_observation_rate`: 完整观察速率
/// - `diagnostic_rate`: 诊断事件速率
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct FamilyObservationMetrics {
    pub raw_frame_rate: f64,
    pub complete_observation_rate: f64,
    pub diagnostic_rate: f64,
}

/// 重建观察族指标快照。
///
/// 仅覆盖本轮重建的六个家族。
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ObservationMetrics {
    pub low_speed: FamilyObservationMetrics,
    pub end_pose: FamilyObservationMetrics,
    pub collision_protection: FamilyObservationMetrics,
    pub joint_limit_config: FamilyObservationMetrics,
    pub joint_accel_config: FamilyObservationMetrics,
    pub end_limit_config: FamilyObservationMetrics,
}

fn rate_from_count(count: u64, elapsed_secs: f64) -> f64 {
    if elapsed_secs <= 0.0 {
        return 0.0;
    }
    count as f64 / elapsed_secs
}

fn family_metrics(
    raw_count: u64,
    complete_count: u64,
    diagnostic_count: u64,
    elapsed_secs: f64,
) -> FamilyObservationMetrics {
    FamilyObservationMetrics {
        raw_frame_rate: rate_from_count(raw_count, elapsed_secs),
        complete_observation_rate: rate_from_count(complete_count, elapsed_secs),
        diagnostic_rate: rate_from_count(diagnostic_count, elapsed_secs),
    }
}

fn elapsed_secs_from_legacy_fps(fps: FpsResult, counts: FpsCounts) -> f64 {
    let candidates = [
        (counts.joint_position, fps.joint_position),
        (counts.end_pose, fps.end_pose),
        (counts.joint_dynamic, fps.joint_dynamic),
        (counts.robot_control, fps.robot_control),
        (counts.gripper, fps.gripper),
        (counts.joint_driver_low_speed, fps.joint_driver_low_speed),
        (counts.collision_protection, fps.collision_protection),
        (counts.joint_limit_config, fps.joint_limit_config),
        (counts.joint_accel_config, fps.joint_accel_config),
        (counts.end_limit_config, fps.end_limit_config),
        (counts.firmware_version, fps.firmware_version),
        (
            counts.master_slave_control_mode,
            fps.master_slave_control_mode,
        ),
        (
            counts.master_slave_joint_control,
            fps.master_slave_joint_control,
        ),
        (
            counts.master_slave_gripper_control,
            fps.master_slave_gripper_control,
        ),
    ];

    for (count, rate) in candidates {
        if count > 0 && rate > 0.0 {
            return count as f64 / rate;
        }
    }

    0.0
}

fn count_collision_protection_diagnostics(diagnostics: &[DiagnosticEvent]) -> u64 {
    diagnostics
        .iter()
        .filter(|event| match event {
            DiagnosticEvent::Protocol(ProtocolDiagnostic::InvalidLength { can_id, .. }) => {
                *can_id == 0x47B
            },
            DiagnosticEvent::Protocol(ProtocolDiagnostic::OutOfRange { field, .. }) => {
                *field == "collision_protection_level"
            },
            DiagnosticEvent::Protocol(ProtocolDiagnostic::UnsupportedValue { field, .. }) => {
                *field == "collision_protection_level"
            },
            DiagnosticEvent::Query(QueryDiagnostic::UnexpectedFrameForActiveQuery {
                query,
                ..
            }) => *query == QueryKind::CollisionProtection,
            DiagnosticEvent::Query(QueryDiagnostic::DiagnosticsOnlyTimeout { query }) => {
                *query == QueryKind::CollisionProtection
            },
            _ => false,
        })
        .count() as u64
}

fn count_joint_limit_diagnostics(diagnostics: &[DiagnosticEvent]) -> u64 {
    diagnostics
        .iter()
        .filter(|event| match event {
            DiagnosticEvent::Protocol(ProtocolDiagnostic::InvalidLength { can_id, .. }) => {
                *can_id == 0x473
            },
            DiagnosticEvent::Protocol(ProtocolDiagnostic::OutOfRange { field, .. }) => {
                *field == "joint_index"
            },
            DiagnosticEvent::Protocol(ProtocolDiagnostic::UnsupportedValue { field, .. }) => {
                *field == "motor_limit_feedback"
            },
            DiagnosticEvent::Query(QueryDiagnostic::UnexpectedFrameForActiveQuery {
                query,
                ..
            }) => *query == QueryKind::JointLimit,
            DiagnosticEvent::Query(QueryDiagnostic::DiagnosticsOnlyTimeout { query }) => {
                *query == QueryKind::JointLimit
            },
            _ => false,
        })
        .count() as u64
}

fn count_joint_accel_diagnostics(diagnostics: &[DiagnosticEvent]) -> u64 {
    diagnostics
        .iter()
        .filter(|event| match event {
            DiagnosticEvent::Protocol(ProtocolDiagnostic::InvalidLength { can_id, .. }) => {
                *can_id == 0x47C
            },
            DiagnosticEvent::Protocol(ProtocolDiagnostic::OutOfRange { field, .. }) => {
                *field == "joint_index"
            },
            DiagnosticEvent::Protocol(ProtocolDiagnostic::UnsupportedValue { field, .. }) => {
                *field == "motor_max_accel_feedback"
            },
            DiagnosticEvent::Query(QueryDiagnostic::UnexpectedFrameForActiveQuery {
                query,
                ..
            }) => *query == QueryKind::JointAccel,
            DiagnosticEvent::Query(QueryDiagnostic::DiagnosticsOnlyTimeout { query }) => {
                *query == QueryKind::JointAccel
            },
            _ => false,
        })
        .count() as u64
}

fn count_end_limit_diagnostics(diagnostics: &[DiagnosticEvent]) -> u64 {
    diagnostics
        .iter()
        .filter(|event| match event {
            DiagnosticEvent::Protocol(ProtocolDiagnostic::InvalidLength { can_id, .. }) => {
                *can_id == 0x478
            },
            DiagnosticEvent::Protocol(ProtocolDiagnostic::UnsupportedValue { field, .. }) => {
                *field == "end_velocity_accel_feedback"
            },
            DiagnosticEvent::Query(QueryDiagnostic::UnexpectedFrameForActiveQuery {
                query,
                ..
            }) => *query == QueryKind::EndLimit,
            DiagnosticEvent::Query(QueryDiagnostic::DiagnosticsOnlyTimeout { query }) => {
                *query == QueryKind::EndLimit
            },
            _ => false,
        })
        .count() as u64
}

/// Piper SDK 实时指标
///
/// 用于监控 IO 链路的健康状态和性能。所有计数器都使用原子操作，
/// 可以在任何线程安全地读取，不会引入锁竞争。
///
/// # 使用示例
///
/// ```rust
/// use piper_driver::PiperMetrics;
/// use std::sync::Arc;
/// use std::sync::atomic::Ordering;
///
/// let metrics = Arc::new(PiperMetrics::default());
///
/// // 在 IO 线程中更新指标
/// metrics.rx_frames_total.fetch_add(1, Ordering::Relaxed);
///
/// // 在主线程中读取快照
/// let snapshot = metrics.snapshot();
/// println!("Total RX frames: {}", snapshot.rx_frames_total);
/// ```
#[derive(Debug, Default)]
pub struct PiperMetrics {
    /// RX 接收的总帧数（包括被过滤的 Echo 帧）
    pub rx_frames_total: AtomicU64,

    /// RX 有效帧数（过滤 Echo 后的真实反馈帧）
    pub rx_frames_valid: AtomicU64,
    /// RX 收到的 transport error frame 总数
    pub rx_error_frames_total: AtomicU64,
    /// RX 检测到的 Bus-Off 总次数
    pub rx_bus_off_total: AtomicU64,
    /// RX 检测到的 Error-Passive 总次数
    pub rx_error_passive_total: AtomicU64,

    /// RX 过滤掉的 Echo 帧数（GS-USB 特有）
    pub rx_echo_filtered: AtomicU64,

    /// TX 成功发送到底层适配器的总帧数
    pub tx_frames_sent_total: AtomicU64,

    /// TX 实时命令成功进入 mailbox 的总次数
    pub tx_realtime_enqueued_total: AtomicU64,

    /// TX 实时队列覆盖（Overwrite）次数
    ///
    /// 如果这个值快速增长，说明 TX 线程处理速度跟不上命令生成速度，
    /// 或者总线/设备存在瓶颈。
    pub tx_realtime_overwrites_total: AtomicU64,

    /// TX 普通可靠命令成功进入 FIFO 的总次数
    pub tx_reliable_enqueued_total: AtomicU64,

    /// TX 普通可靠队列满次数
    pub tx_reliable_queue_full_total: AtomicU64,

    /// TX 侧收到的急停请求总次数（包括 coalesced）
    pub tx_shutdown_requests_total: AtomicU64,

    /// TX 侧附着到当前单飞急停请求的次数
    pub tx_shutdown_coalesced_total: AtomicU64,

    /// TX 侧急停请求因不同停机帧冲突而被拒绝的次数
    pub tx_shutdown_conflicts_total: AtomicU64,

    /// TX 停机命令成功发送到底层适配器的总次数
    pub tx_shutdown_sent_total: AtomicU64,
    /// fault-latched Drop 路径发起 bounded shutdown 的次数
    pub tx_drop_shutdown_attempt_total: AtomicU64,
    /// fault-latched Drop 路径 bounded shutdown 成功的次数
    pub tx_drop_shutdown_success_total: AtomicU64,
    /// fault-latched Drop 路径 bounded shutdown 超时的次数
    pub tx_drop_shutdown_timeout_total: AtomicU64,
    /// fault-latched Drop 路径因 TX 不可用或 runtime 已停止而跳过的次数
    pub tx_drop_shutdown_skipped_total: AtomicU64,

    /// 因故障锁存或停止阶段而被主动中止的普通控制命令总次数
    pub tx_fault_aborts_total: AtomicU64,

    /// USB/CAN 设备错误次数
    pub device_errors: AtomicU64,

    /// RX 超时次数（正常现象，无数据时会超时）
    pub rx_timeouts: AtomicU64,

    /// TX 超时次数（异常现象，说明设备响应慢）
    pub tx_timeouts: AtomicU64,

    /// 多帧命令包完整发送成功次数
    pub tx_packages_completed_total: AtomicU64,
    /// 多帧命令包部分发送次数（失败前已发送前缀帧）
    pub tx_packages_partial_total: AtomicU64,
    /// 多帧命令包因故障锁存而中止的次数
    pub tx_packages_fault_aborted_total: AtomicU64,
    /// 多帧命令包因底层 transport 错误而完全失败（0 帧成功发送）的次数
    pub tx_packages_transport_failed_total: AtomicU64,
    /// 关节位置完整组因缺帧/超时而被丢弃的次数
    pub rx_joint_position_incomplete_groups_dropped_total: AtomicU64,
    /// 关节位置完整组不满足控制级跨度约束而被拒绝的次数
    pub rx_joint_position_control_grade_rejected_total: AtomicU64,
    /// 末端位姿完整组因缺帧/超时而被丢弃的次数
    pub rx_end_pose_incomplete_groups_dropped_total: AtomicU64,
    /// 关节动态部分帧组被丢弃的次数
    pub rx_joint_dynamic_groups_dropped_total: AtomicU64,
    /// 关节动态完整组因控制级时间跨度超限而被拒绝的次数
    pub rx_joint_dynamic_control_grade_rejected_total: AtomicU64,
    /// 热路径逻辑快照发布因参与 cell 无空闲槽位而被整体跳过的次数
    ///
    /// 仅统计 joint/end-pose/motion/raw 这些固定槽位快照发布，不包含 control pair。
    pub rx_hot_snapshot_publish_skipped_total: AtomicU64,
    /// 控制级 clean generation 因单边连跳而被整体丢弃的次数
    pub rx_control_pair_generation_invalidated_total: AtomicU64,
    /// SoftRealtime admission 阶段因总预算已过期而被前门拒绝的次数
    pub tx_soft_admission_timeout_total: AtomicU64,
    /// SoftRealtime 控制发送 deadline miss 总次数
    pub tx_soft_deadline_miss_total: AtomicU64,
    /// SoftRealtime 连续 deadline miss 续增总次数
    pub tx_soft_consecutive_deadline_miss_total: AtomicU64,
}

impl PiperMetrics {
    /// 创建新的指标实例（所有计数器初始化为 0）
    pub fn new() -> Self {
        Self::default()
    }

    /// 获取人类可读的指标快照
    ///
    /// 返回一个包含所有计数器当前值的快照结构。
    /// 快照是原子读取的，保证一致性（虽然不同计数器之间可能有微小的时间差）。
    ///
    /// # 性能
    ///
    /// 使用 `Ordering::Relaxed`，性能最优，适合监控场景。
    pub fn snapshot(&self) -> MetricsSnapshot {
        MetricsSnapshot {
            rx_frames_total: self.rx_frames_total.load(Ordering::Relaxed),
            rx_frames_valid: self.rx_frames_valid.load(Ordering::Relaxed),
            rx_error_frames_total: self.rx_error_frames_total.load(Ordering::Relaxed),
            rx_bus_off_total: self.rx_bus_off_total.load(Ordering::Relaxed),
            rx_error_passive_total: self.rx_error_passive_total.load(Ordering::Relaxed),
            rx_echo_filtered: self.rx_echo_filtered.load(Ordering::Relaxed),
            tx_frames_sent_total: self.tx_frames_sent_total.load(Ordering::Relaxed),
            tx_realtime_enqueued_total: self.tx_realtime_enqueued_total.load(Ordering::Relaxed),
            tx_realtime_overwrites_total: self.tx_realtime_overwrites_total.load(Ordering::Relaxed),
            tx_reliable_enqueued_total: self.tx_reliable_enqueued_total.load(Ordering::Relaxed),
            tx_reliable_queue_full_total: self.tx_reliable_queue_full_total.load(Ordering::Relaxed),
            tx_shutdown_requests_total: self.tx_shutdown_requests_total.load(Ordering::Relaxed),
            tx_shutdown_coalesced_total: self.tx_shutdown_coalesced_total.load(Ordering::Relaxed),
            tx_shutdown_conflicts_total: self.tx_shutdown_conflicts_total.load(Ordering::Relaxed),
            tx_shutdown_sent_total: self.tx_shutdown_sent_total.load(Ordering::Relaxed),
            tx_drop_shutdown_attempt_total: self
                .tx_drop_shutdown_attempt_total
                .load(Ordering::Relaxed),
            tx_drop_shutdown_success_total: self
                .tx_drop_shutdown_success_total
                .load(Ordering::Relaxed),
            tx_drop_shutdown_timeout_total: self
                .tx_drop_shutdown_timeout_total
                .load(Ordering::Relaxed),
            tx_drop_shutdown_skipped_total: self
                .tx_drop_shutdown_skipped_total
                .load(Ordering::Relaxed),
            tx_fault_aborts_total: self.tx_fault_aborts_total.load(Ordering::Relaxed),
            device_errors: self.device_errors.load(Ordering::Relaxed),
            rx_timeouts: self.rx_timeouts.load(Ordering::Relaxed),
            tx_timeouts: self.tx_timeouts.load(Ordering::Relaxed),
            tx_packages_completed_total: self.tx_packages_completed_total.load(Ordering::Relaxed),
            tx_packages_partial_total: self.tx_packages_partial_total.load(Ordering::Relaxed),
            tx_packages_fault_aborted_total: self
                .tx_packages_fault_aborted_total
                .load(Ordering::Relaxed),
            tx_packages_transport_failed_total: self
                .tx_packages_transport_failed_total
                .load(Ordering::Relaxed),
            rx_joint_position_incomplete_groups_dropped_total: self
                .rx_joint_position_incomplete_groups_dropped_total
                .load(Ordering::Relaxed),
            rx_joint_position_control_grade_rejected_total: self
                .rx_joint_position_control_grade_rejected_total
                .load(Ordering::Relaxed),
            rx_end_pose_incomplete_groups_dropped_total: self
                .rx_end_pose_incomplete_groups_dropped_total
                .load(Ordering::Relaxed),
            rx_joint_dynamic_groups_dropped_total: self
                .rx_joint_dynamic_groups_dropped_total
                .load(Ordering::Relaxed),
            rx_joint_dynamic_control_grade_rejected_total: self
                .rx_joint_dynamic_control_grade_rejected_total
                .load(Ordering::Relaxed),
            rx_hot_snapshot_publish_skipped_total: self
                .rx_hot_snapshot_publish_skipped_total
                .load(Ordering::Relaxed),
            rx_control_pair_generation_invalidated_total: self
                .rx_control_pair_generation_invalidated_total
                .load(Ordering::Relaxed),
            tx_soft_admission_timeout_total: self
                .tx_soft_admission_timeout_total
                .load(Ordering::Relaxed),
            tx_soft_deadline_miss_total: self.tx_soft_deadline_miss_total.load(Ordering::Relaxed),
            tx_soft_consecutive_deadline_miss_total: self
                .tx_soft_consecutive_deadline_miss_total
                .load(Ordering::Relaxed),
        }
    }

    /// 重置所有计数器（用于性能测试）
    ///
    /// 将所有计数器重置为 0。使用 `Ordering::Relaxed`，性能最优。
    pub fn reset(&self) {
        self.rx_frames_total.store(0, Ordering::Relaxed);
        self.rx_frames_valid.store(0, Ordering::Relaxed);
        self.rx_error_frames_total.store(0, Ordering::Relaxed);
        self.rx_bus_off_total.store(0, Ordering::Relaxed);
        self.rx_error_passive_total.store(0, Ordering::Relaxed);
        self.rx_echo_filtered.store(0, Ordering::Relaxed);
        self.tx_frames_sent_total.store(0, Ordering::Relaxed);
        self.tx_realtime_enqueued_total.store(0, Ordering::Relaxed);
        self.tx_realtime_overwrites_total.store(0, Ordering::Relaxed);
        self.tx_reliable_enqueued_total.store(0, Ordering::Relaxed);
        self.tx_reliable_queue_full_total.store(0, Ordering::Relaxed);
        self.tx_shutdown_requests_total.store(0, Ordering::Relaxed);
        self.tx_shutdown_coalesced_total.store(0, Ordering::Relaxed);
        self.tx_shutdown_conflicts_total.store(0, Ordering::Relaxed);
        self.tx_shutdown_sent_total.store(0, Ordering::Relaxed);
        self.tx_drop_shutdown_attempt_total.store(0, Ordering::Relaxed);
        self.tx_drop_shutdown_success_total.store(0, Ordering::Relaxed);
        self.tx_drop_shutdown_timeout_total.store(0, Ordering::Relaxed);
        self.tx_drop_shutdown_skipped_total.store(0, Ordering::Relaxed);
        self.tx_fault_aborts_total.store(0, Ordering::Relaxed);
        self.device_errors.store(0, Ordering::Relaxed);
        self.rx_timeouts.store(0, Ordering::Relaxed);
        self.tx_timeouts.store(0, Ordering::Relaxed);
        self.tx_packages_completed_total.store(0, Ordering::Relaxed);
        self.tx_packages_partial_total.store(0, Ordering::Relaxed);
        self.tx_packages_fault_aborted_total.store(0, Ordering::Relaxed);
        self.tx_packages_transport_failed_total.store(0, Ordering::Relaxed);
        self.rx_joint_position_incomplete_groups_dropped_total
            .store(0, Ordering::Relaxed);
        self.rx_joint_position_control_grade_rejected_total.store(0, Ordering::Relaxed);
        self.rx_end_pose_incomplete_groups_dropped_total.store(0, Ordering::Relaxed);
        self.rx_joint_dynamic_groups_dropped_total.store(0, Ordering::Relaxed);
        self.rx_joint_dynamic_control_grade_rejected_total.store(0, Ordering::Relaxed);
        self.rx_hot_snapshot_publish_skipped_total.store(0, Ordering::Relaxed);
        self.rx_control_pair_generation_invalidated_total.store(0, Ordering::Relaxed);
        self.tx_soft_admission_timeout_total.store(0, Ordering::Relaxed);
        self.tx_soft_deadline_miss_total.store(0, Ordering::Relaxed);
        self.tx_soft_consecutive_deadline_miss_total.store(0, Ordering::Relaxed);
    }
}

/// 指标快照（不可变，用于读取）
///
/// 包含所有计数器的当前值，用于一次性读取所有指标，避免多次原子操作。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MetricsSnapshot {
    /// RX 接收的总帧数
    pub rx_frames_total: u64,
    /// RX 有效帧数
    pub rx_frames_valid: u64,
    /// RX transport error frame 总数
    pub rx_error_frames_total: u64,
    /// RX Bus-Off 总次数
    pub rx_bus_off_total: u64,
    /// RX Error-Passive 总次数
    pub rx_error_passive_total: u64,
    /// RX 过滤掉的 Echo 帧数
    pub rx_echo_filtered: u64,
    /// TX 成功发送的总帧数
    pub tx_frames_sent_total: u64,
    /// TX 实时命令入队总次数
    pub tx_realtime_enqueued_total: u64,
    /// TX 实时队列覆盖次数
    pub tx_realtime_overwrites_total: u64,
    /// TX 普通可靠命令入队总次数
    pub tx_reliable_enqueued_total: u64,
    /// TX 普通可靠队列满次数
    pub tx_reliable_queue_full_total: u64,
    /// TX 急停请求总次数
    pub tx_shutdown_requests_total: u64,
    /// TX 急停 coalesced 次数
    pub tx_shutdown_coalesced_total: u64,
    /// TX 急停冲突拒绝次数
    pub tx_shutdown_conflicts_total: u64,
    /// TX 停机命令发送总次数
    pub tx_shutdown_sent_total: u64,
    /// fault-latched Drop 路径发起 bounded shutdown 的次数
    pub tx_drop_shutdown_attempt_total: u64,
    /// fault-latched Drop 路径 bounded shutdown 成功的次数
    pub tx_drop_shutdown_success_total: u64,
    /// fault-latched Drop 路径 bounded shutdown 超时的次数
    pub tx_drop_shutdown_timeout_total: u64,
    /// fault-latched Drop 路径因 TX 不可用或 runtime 已停止而跳过的次数
    pub tx_drop_shutdown_skipped_total: u64,
    /// 因故障锁存或停止阶段被主动中止的普通控制命令总次数
    pub tx_fault_aborts_total: u64,
    /// 设备错误次数
    pub device_errors: u64,
    /// RX 超时次数
    pub rx_timeouts: u64,
    /// TX 超时次数
    pub tx_timeouts: u64,
    /// 多帧命令包完整发送成功次数
    pub tx_packages_completed_total: u64,
    /// 多帧命令包部分发送次数（发送失败前已发送前缀帧）
    pub tx_packages_partial_total: u64,
    /// 多帧命令包因故障锁存而中止的次数
    pub tx_packages_fault_aborted_total: u64,
    /// 多帧命令包因 transport 错误在 0 帧成功发送时失败的次数
    pub tx_packages_transport_failed_total: u64,
    /// 关节位置完整组因缺帧/超时而被丢弃的次数
    pub rx_joint_position_incomplete_groups_dropped_total: u64,
    /// 关节位置完整组不满足控制级跨度约束而被拒绝的次数
    pub rx_joint_position_control_grade_rejected_total: u64,
    /// 末端位姿完整组因缺帧/超时而被丢弃的次数
    pub rx_end_pose_incomplete_groups_dropped_total: u64,
    /// 关节动态部分帧组被丢弃的次数
    pub rx_joint_dynamic_groups_dropped_total: u64,
    /// 关节动态完整组因控制级时间跨度超限而被拒绝的次数
    pub rx_joint_dynamic_control_grade_rejected_total: u64,
    /// 热路径逻辑快照发布因参与 cell 无空闲槽位而被整体跳过的次数
    ///
    /// 仅统计 joint/end-pose/motion/raw 这些固定槽位快照发布，不包含 control pair。
    pub rx_hot_snapshot_publish_skipped_total: u64,
    /// 控制级 clean generation 因单边连跳而被整体丢弃的次数
    pub rx_control_pair_generation_invalidated_total: u64,
    /// SoftRealtime admission 阶段因总预算已过期而被前门拒绝的次数
    pub tx_soft_admission_timeout_total: u64,
    /// SoftRealtime 控制发送 deadline miss 总次数
    pub tx_soft_deadline_miss_total: u64,
    /// SoftRealtime 连续 deadline miss 续增总次数
    pub tx_soft_consecutive_deadline_miss_total: u64,
}

impl MetricsSnapshot {
    /// 计算 Echo 帧过滤率（百分比）
    ///
    /// 返回 0.0 到 100.0 之间的值。如果 `rx_frames_total` 为 0，返回 0.0。
    pub fn echo_filter_rate(&self) -> f64 {
        if self.rx_frames_total == 0 {
            return 0.0;
        }
        (self.rx_echo_filtered as f64 / self.rx_frames_total as f64) * 100.0
    }

    /// 计算有效帧率（百分比）
    ///
    /// 返回 0.0 到 100.0 之间的值。如果 `rx_frames_total` 为 0，返回 0.0。
    pub fn valid_frame_rate(&self) -> f64 {
        if self.rx_frames_total == 0 {
            return 0.0;
        }
        (self.rx_frames_valid as f64 / self.rx_frames_total as f64) * 100.0
    }

    /// 计算实时队列覆盖率（百分比）
    ///
    /// 返回 0.0 到 100.0 之间的值。如果 `tx_realtime_enqueued_total` 为 0，返回 0.0。
    ///
    /// # 阈值说明
    /// - < 30%: 正常情况（高频控制，预期行为）
    /// - 30-50%: 中等情况（可能需要优化）
    /// - > 50%: 异常情况（TX 线程瓶颈，需要关注）
    ///
    /// # 示例
    ///
    /// ```rust
    /// # use piper_driver::metrics::PiperMetrics;
    /// # use std::sync::Arc;
    /// # let metrics = Arc::new(PiperMetrics::default());
    /// let snapshot = metrics.snapshot();
    /// let rate = snapshot.overwrite_rate();
    /// if rate > 50.0 {
    ///     eprintln!("Warning: High overwrite rate: {:.1}%", rate);
    /// }
    /// ```
    pub fn overwrite_rate(&self) -> f64 {
        if self.tx_realtime_enqueued_total == 0 {
            return 0.0;
        }
        (self.tx_realtime_overwrites_total as f64 / self.tx_realtime_enqueued_total as f64) * 100.0
    }

    /// 检查覆盖率是否异常
    ///
    /// 返回 `true` 如果覆盖率 > 50%（异常阈值）。
    ///
    /// # 示例
    ///
    /// ```rust
    /// # use piper_driver::metrics::PiperMetrics;
    /// # use std::sync::Arc;
    /// # let metrics = Arc::new(PiperMetrics::default());
    /// let snapshot = metrics.snapshot();
    /// if snapshot.is_overwrite_rate_abnormal() {
    ///     eprintln!("Warning: Abnormal overwrite rate detected");
    /// }
    /// ```
    pub fn is_overwrite_rate_abnormal(&self) -> bool {
        self.overwrite_rate() > 50.0
    }
}

impl Piper {
    /// 获取重建观察族的专用指标快照。
    pub fn get_observation_metrics(&self) -> ObservationMetrics {
        let fps = self.get_fps();
        let counts = self.get_fps_counts();
        let elapsed_secs = elapsed_secs_from_legacy_fps(fps, counts);
        let diagnostics = self.snapshot_diagnostics();

        ObservationMetrics {
            low_speed: family_metrics(
                counts.joint_driver_low_speed.saturating_mul(6),
                counts.joint_driver_low_speed,
                0,
                elapsed_secs,
            ),
            end_pose: family_metrics(
                counts.end_pose.saturating_mul(3),
                counts.end_pose,
                0,
                elapsed_secs,
            ),
            collision_protection: family_metrics(
                counts.collision_protection,
                counts.collision_protection,
                count_collision_protection_diagnostics(&diagnostics),
                elapsed_secs,
            ),
            joint_limit_config: family_metrics(
                counts.joint_limit_config,
                counts.joint_limit_config / 6,
                count_joint_limit_diagnostics(&diagnostics),
                elapsed_secs,
            ),
            joint_accel_config: family_metrics(
                counts.joint_accel_config,
                counts.joint_accel_config / 6,
                count_joint_accel_diagnostics(&diagnostics),
                elapsed_secs,
            ),
            end_limit_config: family_metrics(
                counts.end_limit_config,
                counts.end_limit_config,
                count_end_limit_diagnostics(&diagnostics),
                elapsed_secs,
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn test_metrics_default() {
        let metrics = PiperMetrics::new();
        let snapshot = metrics.snapshot();

        assert_eq!(snapshot.rx_frames_total, 0);
        assert_eq!(snapshot.rx_frames_valid, 0);
        assert_eq!(snapshot.tx_frames_sent_total, 0);
        assert_eq!(snapshot.tx_realtime_enqueued_total, 0);
        assert_eq!(snapshot.rx_hot_snapshot_publish_skipped_total, 0);
        assert_eq!(snapshot.rx_control_pair_generation_invalidated_total, 0);
        assert_eq!(snapshot.tx_soft_admission_timeout_total, 0);
    }

    #[test]
    fn test_metrics_increment() {
        let metrics = Arc::new(PiperMetrics::new());

        metrics.rx_frames_total.fetch_add(10, Ordering::Relaxed);
        metrics.rx_frames_valid.fetch_add(8, Ordering::Relaxed);
        metrics.rx_echo_filtered.fetch_add(2, Ordering::Relaxed);

        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.rx_frames_total, 10);
        assert_eq!(snapshot.rx_frames_valid, 8);
        assert_eq!(snapshot.rx_echo_filtered, 2);
    }

    #[test]
    fn test_metrics_reset() {
        let metrics = Arc::new(PiperMetrics::new());

        metrics.rx_frames_total.fetch_add(100, Ordering::Relaxed);
        metrics.tx_frames_sent_total.fetch_add(50, Ordering::Relaxed);
        metrics.rx_hot_snapshot_publish_skipped_total.fetch_add(7, Ordering::Relaxed);
        metrics
            .rx_control_pair_generation_invalidated_total
            .fetch_add(5, Ordering::Relaxed);
        metrics.tx_soft_admission_timeout_total.fetch_add(3, Ordering::Relaxed);

        let snapshot_before = metrics.snapshot();
        assert_eq!(snapshot_before.rx_frames_total, 100);
        assert_eq!(snapshot_before.tx_frames_sent_total, 50);
        assert_eq!(snapshot_before.rx_hot_snapshot_publish_skipped_total, 7);
        assert_eq!(
            snapshot_before.rx_control_pair_generation_invalidated_total,
            5
        );
        assert_eq!(snapshot_before.tx_soft_admission_timeout_total, 3);

        metrics.reset();

        let snapshot_after = metrics.snapshot();
        assert_eq!(snapshot_after.rx_frames_total, 0);
        assert_eq!(snapshot_after.tx_frames_sent_total, 0);
        assert_eq!(snapshot_after.rx_hot_snapshot_publish_skipped_total, 0);
        assert_eq!(
            snapshot_after.rx_control_pair_generation_invalidated_total,
            0
        );
        assert_eq!(snapshot_after.tx_soft_admission_timeout_total, 0);
    }

    #[test]
    fn test_metrics_concurrent_updates() {
        let metrics = Arc::new(PiperMetrics::new());
        let mut handles = vec![];

        // 启动 10 个线程，每个线程增加 100 次
        for _ in 0..10 {
            let m = metrics.clone();
            let handle = thread::spawn(move || {
                for _ in 0..100 {
                    m.rx_frames_total.fetch_add(1, Ordering::Relaxed);
                }
            });
            handles.push(handle);
        }

        // 等待所有线程完成
        for handle in handles {
            handle.join().unwrap();
        }

        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.rx_frames_total, 1000);
    }

    #[test]
    fn test_metrics_snapshot_rates() {
        let snapshot = MetricsSnapshot {
            rx_frames_total: 100,
            rx_frames_valid: 80,
            rx_echo_filtered: 20,
            tx_frames_sent_total: 50,
            tx_realtime_enqueued_total: 50,
            tx_realtime_overwrites_total: 5,
            tx_reliable_enqueued_total: 0,
            tx_reliable_queue_full_total: 0,
            tx_shutdown_requests_total: 0,
            tx_shutdown_coalesced_total: 0,
            tx_shutdown_conflicts_total: 0,
            tx_shutdown_sent_total: 0,
            tx_fault_aborts_total: 0,
            device_errors: 0,
            rx_timeouts: 10,
            tx_timeouts: 0,
            tx_packages_completed_total: 0,
            tx_packages_partial_total: 0,
            tx_packages_fault_aborted_total: 0,
            tx_packages_transport_failed_total: 0,
            rx_joint_position_incomplete_groups_dropped_total: 0,
            rx_joint_position_control_grade_rejected_total: 0,
            rx_end_pose_incomplete_groups_dropped_total: 0,
            rx_joint_dynamic_groups_dropped_total: 0,
            ..Default::default()
        };

        assert_eq!(snapshot.echo_filter_rate(), 20.0);
        assert_eq!(snapshot.valid_frame_rate(), 80.0);
        assert_eq!(snapshot.overwrite_rate(), 10.0);
    }

    #[test]
    fn test_metrics_snapshot_rates_zero_total() {
        let snapshot = MetricsSnapshot {
            rx_frames_total: 0,
            rx_frames_valid: 0,
            rx_echo_filtered: 0,
            tx_frames_sent_total: 0,
            tx_realtime_enqueued_total: 0,
            tx_realtime_overwrites_total: 0,
            tx_reliable_enqueued_total: 0,
            tx_reliable_queue_full_total: 0,
            tx_shutdown_requests_total: 0,
            tx_shutdown_coalesced_total: 0,
            tx_shutdown_conflicts_total: 0,
            tx_shutdown_sent_total: 0,
            tx_fault_aborts_total: 0,
            device_errors: 0,
            rx_timeouts: 0,
            tx_timeouts: 0,
            tx_packages_completed_total: 0,
            tx_packages_partial_total: 0,
            tx_packages_fault_aborted_total: 0,
            tx_packages_transport_failed_total: 0,
            rx_joint_position_incomplete_groups_dropped_total: 0,
            rx_joint_position_control_grade_rejected_total: 0,
            rx_end_pose_incomplete_groups_dropped_total: 0,
            rx_joint_dynamic_groups_dropped_total: 0,
            ..Default::default()
        };

        assert_eq!(snapshot.echo_filter_rate(), 0.0);
        assert_eq!(snapshot.valid_frame_rate(), 0.0);
        assert_eq!(snapshot.overwrite_rate(), 0.0);
    }

    #[test]
    fn observation_metrics_separate_raw_and_complete_rates() {
        let snapshot = family_metrics(6, 1, 0, 1.0);

        assert_eq!(snapshot.raw_frame_rate, 6.0);
        assert_eq!(snapshot.complete_observation_rate, 1.0);
        assert_eq!(snapshot.diagnostic_rate, 0.0);
    }

    #[test]
    fn test_overwrite_rate() {
        let snapshot = MetricsSnapshot {
            rx_frames_total: 0,
            rx_frames_valid: 0,
            rx_echo_filtered: 0,
            tx_frames_sent_total: 1000,
            tx_realtime_enqueued_total: 1000,
            tx_realtime_overwrites_total: 200,
            tx_reliable_enqueued_total: 0,
            tx_reliable_queue_full_total: 0,
            tx_shutdown_requests_total: 0,
            tx_shutdown_coalesced_total: 0,
            tx_shutdown_conflicts_total: 0,
            tx_shutdown_sent_total: 0,
            tx_fault_aborts_total: 0,
            device_errors: 0,
            rx_timeouts: 0,
            tx_timeouts: 0,
            tx_packages_completed_total: 0,
            tx_packages_partial_total: 0,
            tx_packages_fault_aborted_total: 0,
            tx_packages_transport_failed_total: 0,
            rx_joint_position_incomplete_groups_dropped_total: 0,
            rx_joint_position_control_grade_rejected_total: 0,
            rx_end_pose_incomplete_groups_dropped_total: 0,
            rx_joint_dynamic_groups_dropped_total: 0,
            ..Default::default()
        };

        // 20% 覆盖率（正常情况）
        assert_eq!(snapshot.overwrite_rate(), 20.0);
        assert!(!snapshot.is_overwrite_rate_abnormal());

        // 60% 覆盖率（异常情况）
        let abnormal = MetricsSnapshot {
            tx_realtime_enqueued_total: 1000,
            tx_realtime_overwrites_total: 600,
            ..snapshot
        };
        assert_eq!(abnormal.overwrite_rate(), 60.0);
        assert!(abnormal.is_overwrite_rate_abnormal());
    }

    #[test]
    fn test_overwrite_rate_zero_total() {
        let snapshot = MetricsSnapshot {
            rx_frames_total: 0,
            rx_frames_valid: 0,
            rx_echo_filtered: 0,
            tx_frames_sent_total: 0,
            tx_realtime_enqueued_total: 0,
            tx_realtime_overwrites_total: 0,
            tx_reliable_enqueued_total: 0,
            tx_reliable_queue_full_total: 0,
            tx_shutdown_requests_total: 0,
            tx_shutdown_coalesced_total: 0,
            tx_shutdown_conflicts_total: 0,
            tx_shutdown_sent_total: 0,
            tx_fault_aborts_total: 0,
            device_errors: 0,
            rx_timeouts: 0,
            tx_timeouts: 0,
            tx_packages_completed_total: 0,
            tx_packages_partial_total: 0,
            tx_packages_fault_aborted_total: 0,
            tx_packages_transport_failed_total: 0,
            rx_joint_position_incomplete_groups_dropped_total: 0,
            rx_joint_position_control_grade_rejected_total: 0,
            rx_end_pose_incomplete_groups_dropped_total: 0,
            rx_joint_dynamic_groups_dropped_total: 0,
            ..Default::default()
        };

        // 总数为 0 时，覆盖率应该为 0.0
        assert_eq!(snapshot.overwrite_rate(), 0.0);
        assert!(!snapshot.is_overwrite_rate_abnormal());
    }

    #[test]
    fn test_overwrite_rate_uses_realtime_enqueued_denominator() {
        let snapshot = MetricsSnapshot {
            rx_frames_total: 0,
            rx_frames_valid: 0,
            rx_echo_filtered: 0,
            tx_frames_sent_total: 10,
            tx_realtime_enqueued_total: 100,
            tx_realtime_overwrites_total: 25,
            tx_reliable_enqueued_total: 40,
            tx_reliable_queue_full_total: 3,
            tx_shutdown_requests_total: 2,
            tx_shutdown_coalesced_total: 0,
            tx_shutdown_conflicts_total: 0,
            tx_shutdown_sent_total: 2,
            tx_fault_aborts_total: 7,
            device_errors: 0,
            rx_timeouts: 0,
            tx_timeouts: 0,
            tx_packages_completed_total: 0,
            tx_packages_partial_total: 0,
            tx_packages_fault_aborted_total: 0,
            tx_packages_transport_failed_total: 0,
            rx_joint_position_incomplete_groups_dropped_total: 0,
            rx_joint_position_control_grade_rejected_total: 0,
            rx_end_pose_incomplete_groups_dropped_total: 0,
            rx_joint_dynamic_groups_dropped_total: 0,
            ..Default::default()
        };

        assert_eq!(snapshot.overwrite_rate(), 25.0);
    }

    #[test]
    fn test_overwrite_rate_thresholds() {
        // 测试阈值边界
        let normal = MetricsSnapshot {
            rx_frames_total: 0,
            rx_frames_valid: 0,
            rx_echo_filtered: 0,
            tx_frames_sent_total: 1000,
            tx_realtime_enqueued_total: 1000,
            tx_realtime_overwrites_total: 299, // 29.9% < 30%
            tx_reliable_enqueued_total: 0,
            tx_reliable_queue_full_total: 0,
            tx_shutdown_requests_total: 0,
            tx_shutdown_coalesced_total: 0,
            tx_shutdown_conflicts_total: 0,
            tx_shutdown_sent_total: 0,
            tx_fault_aborts_total: 0,
            device_errors: 0,
            rx_timeouts: 0,
            tx_timeouts: 0,
            tx_packages_completed_total: 0,
            tx_packages_partial_total: 0,
            tx_packages_fault_aborted_total: 0,
            tx_packages_transport_failed_total: 0,
            rx_joint_position_incomplete_groups_dropped_total: 0,
            rx_joint_position_control_grade_rejected_total: 0,
            rx_end_pose_incomplete_groups_dropped_total: 0,
            rx_joint_dynamic_groups_dropped_total: 0,
            ..Default::default()
        };
        assert!(!normal.is_overwrite_rate_abnormal());

        let moderate = MetricsSnapshot {
            tx_realtime_enqueued_total: 1000,
            tx_realtime_overwrites_total: 400, // 40% (30-50%)
            ..normal
        };
        assert!(!moderate.is_overwrite_rate_abnormal()); // 40% < 50%，不算异常

        let abnormal = MetricsSnapshot {
            tx_realtime_enqueued_total: 1000,
            tx_realtime_overwrites_total: 501, // 50.1% > 50%
            ..normal
        };
        assert!(abnormal.is_overwrite_rate_abnormal());
    }
}
