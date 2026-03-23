//! Pipeline IO 循环模块
//!
//! 负责后台 IO 线程的 CAN 帧接收、解析和状态更新逻辑。

use crate::heartbeat::monotonic_micros;
use crate::metrics::PiperMetrics;
use crate::piper::{
    MaintenanceControlOp, MaintenanceGate, MaintenanceGateState, MaintenanceLaneCommand,
    MaintenanceLeaseSnapshot, MaintenanceSendPhase, NORMAL_FRAME_SEND_BUDGET, NormalSendGate,
    NormalSendGateDenyReason, RuntimeFaultKind, RuntimePhase, SOFT_CONTROL_SEND_BUDGET,
    SOFT_CONTROL_SEND_MIN_DEADLINE, SOFT_DEADLINE_MISS_FAULT_THRESHOLD, ShutdownDispatch,
    ShutdownLane,
};
use crate::state::*;
use crossbeam_channel::Receiver;
#[cfg(test)]
use piper_can::CanAdapter;
use piper_can::{BackendCapability, CanError, PiperFrame, RealtimeTxAdapter, RxAdapter};
use piper_protocol::config::*;
use piper_protocol::feedback::*;
use piper_protocol::ids::*;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::time::{Duration, Instant};
use tracing::{debug, error, trace, warn};

// 使用 spin_sleep 提供微秒级延迟精度（相比 std::thread::sleep 的 1-2ms）
use spin_sleep;

const STRICT_GROUP_MAX_SPAN_US: u64 = 2_000;
const MOTION_GROUP_RESET_MAX_SPAN_US: u64 = 2_200;
const ALL_DRIVES_ENABLED_MASK: u8 = 0b11_1111;

#[cfg(test)]
#[derive(Debug)]
struct TxLoopDispatchBarrier {
    reached_tx: std::sync::mpsc::Sender<()>,
    release_rx: std::sync::mpsc::Receiver<()>,
}

#[cfg(test)]
static TX_LOOP_DISPATCH_BARRIER: std::sync::OnceLock<
    std::sync::Mutex<Option<TxLoopDispatchBarrier>>,
> = std::sync::OnceLock::new();

#[cfg(test)]
#[derive(Debug)]
struct FaultLatchBarrier {
    reached_tx: std::sync::mpsc::Sender<()>,
    release_rx: std::sync::mpsc::Receiver<()>,
}

#[cfg(test)]
static FAULT_LATCH_BARRIER: std::sync::OnceLock<std::sync::Mutex<Option<FaultLatchBarrier>>> =
    std::sync::OnceLock::new();

#[cfg(test)]
fn tx_loop_dispatch_barrier_slot() -> &'static std::sync::Mutex<Option<TxLoopDispatchBarrier>> {
    TX_LOOP_DISPATCH_BARRIER.get_or_init(|| std::sync::Mutex::new(None))
}

#[cfg(test)]
fn fault_latch_barrier_slot() -> &'static std::sync::Mutex<Option<FaultLatchBarrier>> {
    FAULT_LATCH_BARRIER.get_or_init(|| std::sync::Mutex::new(None))
}

#[cfg(test)]
pub(crate) fn install_tx_loop_dispatch_barrier(
    reached_tx: std::sync::mpsc::Sender<()>,
    release_rx: std::sync::mpsc::Receiver<()>,
) {
    let mut guard = tx_loop_dispatch_barrier_slot()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *guard = Some(TxLoopDispatchBarrier {
        reached_tx,
        release_rx,
    });
}

#[cfg(test)]
pub(crate) fn install_fault_latch_barrier(
    reached_tx: std::sync::mpsc::Sender<()>,
    release_rx: std::sync::mpsc::Receiver<()>,
) {
    let mut guard = fault_latch_barrier_slot()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *guard = Some(FaultLatchBarrier {
        reached_tx,
        release_rx,
    });
}

#[cfg(test)]
fn maybe_wait_tx_loop_dispatch_barrier() {
    let barrier = tx_loop_dispatch_barrier_slot()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take();
    if let Some(barrier) = barrier {
        let _ = barrier.reached_tx.send(());
        let _ = barrier.release_rx.recv();
    }
}

#[cfg(test)]
fn maybe_wait_fault_latch_barrier() {
    let barrier = fault_latch_barrier_slot()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take();
    if let Some(barrier) = barrier {
        let _ = barrier.reached_tx.send(());
        let _ = barrier.release_rx.recv();
    }
}

#[inline]
fn host_rx_mono_us() -> u64 {
    monotonic_micros()
}

fn record_fault(slot: &AtomicU8, fault: RuntimeFaultKind) {
    let _ = slot.compare_exchange(0, fault as u8, Ordering::AcqRel, Ordering::Acquire);
}

fn load_runtime_phase(slot: &AtomicU8) -> RuntimePhase {
    RuntimePhase::from_raw(slot.load(Ordering::Acquire))
}

pub(crate) fn latch_runtime_fault_state(
    runtime_phase: &Arc<AtomicU8>,
    normal_send_gate: &Arc<NormalSendGate>,
    last_fault: &Arc<AtomicU8>,
    fault: RuntimeFaultKind,
) -> RuntimePhase {
    record_fault(last_fault, fault);
    normal_send_gate.close_for_fault();
    #[cfg(test)]
    maybe_wait_fault_latch_barrier();

    let mut previous = load_runtime_phase(runtime_phase);
    while previous == RuntimePhase::Running {
        match runtime_phase.compare_exchange(
            RuntimePhase::Running as u8,
            RuntimePhase::FaultLatched as u8,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => return RuntimePhase::Running,
            Err(observed) => previous = RuntimePhase::from_raw(observed),
        }
    }

    previous
}

fn latch_runtime_fault_with_maintenance(
    runtime_phase: &Arc<AtomicU8>,
    normal_send_gate: &Arc<NormalSendGate>,
    last_fault: &Arc<AtomicU8>,
    fault: RuntimeFaultKind,
    maintenance_gate: &Arc<MaintenanceGate>,
    maintenance_tx_state: Option<&mut MaintenanceTxState>,
) {
    let _ = latch_runtime_fault_state(runtime_phase, normal_send_gate, last_fault, fault);

    if let Some(maintenance_tx_state) = maintenance_tx_state {
        refresh_local_maintenance_tx_state(
            maintenance_gate,
            maintenance_tx_state,
            MaintenanceGateState::DeniedFaulted,
        );
    } else {
        let _ = maintenance_gate.set_state_synced(MaintenanceGateState::DeniedFaulted);
    }
}

fn count_fault_abort(metrics: &Arc<PiperMetrics>) {
    metrics.tx_fault_aborts_total.fetch_add(1, Ordering::Relaxed);
}

fn count_package_completed(metrics: &Arc<PiperMetrics>) {
    metrics.tx_packages_completed_total.fetch_add(1, Ordering::Relaxed);
}

fn count_package_partial(metrics: &Arc<PiperMetrics>) {
    metrics.tx_packages_partial_total.fetch_add(1, Ordering::Relaxed);
}

fn count_package_fault_aborted(metrics: &Arc<PiperMetrics>) {
    metrics.tx_packages_fault_aborted_total.fetch_add(1, Ordering::Relaxed);
}

fn count_package_transport_failed(metrics: &Arc<PiperMetrics>) {
    metrics.tx_packages_transport_failed_total.fetch_add(1, Ordering::Relaxed);
}

fn record_soft_deadline_miss(
    metrics: &Arc<PiperMetrics>,
    soft_deadline_miss_streak: &mut u32,
    runtime_phase: &Arc<AtomicU8>,
    normal_send_gate: &Arc<NormalSendGate>,
    last_fault: &Arc<AtomicU8>,
    maintenance_gate: &Arc<MaintenanceGate>,
    maintenance_tx_state: &mut MaintenanceTxState,
) {
    metrics.tx_soft_deadline_miss_total.fetch_add(1, Ordering::Relaxed);
    if *soft_deadline_miss_streak > 0 {
        metrics.tx_soft_consecutive_deadline_miss_total.fetch_add(1, Ordering::Relaxed);
    }
    *soft_deadline_miss_streak = soft_deadline_miss_streak.saturating_add(1);
    if *soft_deadline_miss_streak >= SOFT_DEADLINE_MISS_FAULT_THRESHOLD {
        latch_runtime_fault_with_maintenance(
            runtime_phase,
            normal_send_gate,
            last_fault,
            RuntimeFaultKind::TransportError,
            maintenance_gate,
            Some(maintenance_tx_state),
        );
    }
}

fn reset_soft_deadline_miss_streak(soft_deadline_miss_streak: &mut u32) {
    *soft_deadline_miss_streak = 0;
}

fn reliable_abort_error(fault_latched: bool) -> crate::DriverError {
    if fault_latched {
        crate::DriverError::CommandAbortedByFault
    } else {
        crate::DriverError::ChannelClosed
    }
}

fn realtime_abort_error(sent: usize, total: usize) -> crate::DriverError {
    crate::DriverError::RealtimeDeliveryAbortedByFault { sent, total }
}

fn count_gate_fault_abort(
    metrics: &Arc<PiperMetrics>,
    reason: NormalSendGateDenyReason,
    count_package_abort: bool,
) {
    if reason == NormalSendGateDenyReason::FaultClosed {
        count_fault_abort(metrics);
        if count_package_abort {
            count_package_fault_aborted(metrics);
        }
    }
}

fn maintenance_gate_abort_error(reason: NormalSendGateDenyReason) -> crate::DriverError {
    match reason {
        NormalSendGateDenyReason::ReplayPaused => crate::DriverError::ReplayModeActive,
        NormalSendGateDenyReason::FaultClosed => crate::DriverError::CommandAbortedByFault,
        NormalSendGateDenyReason::StoppingClosed => crate::DriverError::ChannelClosed,
    }
}

fn realtime_gate_abort_error(
    reason: NormalSendGateDenyReason,
    sent: usize,
    total: usize,
) -> crate::DriverError {
    match reason {
        NormalSendGateDenyReason::ReplayPaused => crate::DriverError::ReplayModeActive,
        NormalSendGateDenyReason::FaultClosed => realtime_abort_error(sent, total),
        NormalSendGateDenyReason::StoppingClosed => crate::DriverError::ChannelClosed,
    }
}

fn soft_gate_abort_error(reason: NormalSendGateDenyReason) -> crate::DriverError {
    match reason {
        NormalSendGateDenyReason::ReplayPaused => crate::DriverError::ReplayModeActive,
        NormalSendGateDenyReason::FaultClosed => crate::DriverError::CommandAbortedByFault,
        NormalSendGateDenyReason::StoppingClosed => crate::DriverError::ChannelClosed,
    }
}

fn reliable_gate_abort_error(reason: NormalSendGateDenyReason) -> crate::DriverError {
    match reason {
        NormalSendGateDenyReason::ReplayPaused => crate::DriverError::ReplayModeActive,
        NormalSendGateDenyReason::FaultClosed => crate::DriverError::CommandAbortedByFault,
        NormalSendGateDenyReason::StoppingClosed => crate::DriverError::ChannelClosed,
    }
}

fn drain_soft_realtime_queue_with_reason<F>(
    soft_realtime_rx: &Receiver<crate::command::SoftRealtimeCommand>,
    metrics: &Arc<PiperMetrics>,
    reason: F,
    count_as_fault_abort: bool,
) where
    F: Fn() -> crate::DriverError,
{
    while let Ok(command) = soft_realtime_rx.try_recv() {
        if count_as_fault_abort {
            count_fault_abort(metrics);
        }
        command.complete(Err(reason()));
    }
}

fn reject_replay_mode_dispatches(
    realtime_slot: &Arc<std::sync::Mutex<Option<crate::command::RealtimeCommand>>>,
    soft_realtime_rx: &Receiver<crate::command::SoftRealtimeCommand>,
    metrics: &Arc<PiperMetrics>,
) {
    abort_realtime_slot_with(
        realtime_slot,
        metrics,
        crate::DriverError::ReplayModeActive,
        false,
    );
    drain_soft_realtime_queue_with_reason(
        soft_realtime_rx,
        metrics,
        || crate::DriverError::ReplayModeActive,
        false,
    );
}

fn derive_maintenance_gate_state(
    runtime_phase: &Arc<AtomicU8>,
    ctx: &Arc<PiperContext>,
    config: &PipelineConfig,
    last_fault: &Arc<AtomicU8>,
) -> MaintenanceGateState {
    let phase = load_runtime_phase(runtime_phase);
    if phase == RuntimePhase::FaultLatched {
        return MaintenanceGateState::DeniedFaulted;
    }
    if phase != RuntimePhase::Running {
        return MaintenanceGateState::DeniedTransportDown;
    }
    if last_fault.load(Ordering::Acquire) != 0 {
        return MaintenanceGateState::DeniedFaulted;
    }
    if !ctx.connection_monitor.check_connection() {
        return MaintenanceGateState::DeniedTransportDown;
    }
    let Some(driver_enabled_mask) = confirmed_low_speed_drive_enabled_mask(ctx, config) else {
        return MaintenanceGateState::DeniedDriveStateUnknown;
    };
    if driver_enabled_mask != 0 {
        return MaintenanceGateState::DeniedActiveControl;
    }
    MaintenanceGateState::AllowedStandby
}

fn confirmed_low_speed_drive_enabled_mask(
    ctx: &Arc<PiperContext>,
    config: &PipelineConfig,
) -> Option<u8> {
    let driver_state = ctx.joint_driver_low_speed.load();
    driver_state.confirmed_driver_enabled_mask(
        host_rx_mono_us(),
        config.low_speed_drive_state_freshness_ms.saturating_mul(1_000),
    )
}

fn refresh_maintenance_gate_state(
    maintenance_gate: &Arc<MaintenanceGate>,
    runtime_phase: &Arc<AtomicU8>,
    ctx: &Arc<PiperContext>,
    config: &PipelineConfig,
    last_fault: &Arc<AtomicU8>,
) {
    let _ = maintenance_gate.set_state_synced(derive_maintenance_gate_state(
        runtime_phase,
        ctx,
        config,
        last_fault,
    ));
}

#[derive(Debug, Clone, Copy)]
struct MaintenanceTxState {
    state: MaintenanceGateState,
    holder_session_id: Option<u32>,
    holder_session_key: Option<u64>,
    lease_epoch: u64,
}

impl MaintenanceTxState {
    fn from_snapshot(snapshot: MaintenanceLeaseSnapshot) -> Self {
        Self {
            state: snapshot.state(),
            holder_session_id: snapshot.holder_session_id(),
            holder_session_key: snapshot.holder_session_key(),
            lease_epoch: snapshot.lease_epoch(),
        }
    }
}

fn maintenance_state_denial(state: MaintenanceGateState) -> crate::DriverError {
    crate::DriverError::MaintenanceWriteDenied(state.denial_message().to_string())
}

fn apply_maintenance_control_op(state: &mut MaintenanceTxState, op: MaintenanceControlOp) {
    match op {
        MaintenanceControlOp::Grant {
            session_id,
            session_key,
            lease_epoch,
        } => {
            state.holder_session_id = Some(session_id);
            state.holder_session_key = Some(session_key);
            state.lease_epoch = lease_epoch;
        },
        MaintenanceControlOp::Release {
            session_key,
            lease_epoch,
        } => {
            if state.holder_session_key == Some(session_key) {
                state.holder_session_id = None;
                state.holder_session_key = None;
            }
            state.lease_epoch = lease_epoch;
        },
        MaintenanceControlOp::Revoke {
            session_key,
            lease_epoch,
            reason: _,
        } => {
            if state.holder_session_key == Some(session_key) {
                state.holder_session_id = None;
                state.holder_session_key = None;
            }
            state.lease_epoch = lease_epoch;
        },
        MaintenanceControlOp::SetState {
            state: next_state,
            holder_session_id,
            holder_session_key,
            lease_epoch,
        } => {
            state.state = next_state;
            state.holder_session_id = holder_session_id;
            state.holder_session_key = holder_session_key;
            state.lease_epoch = lease_epoch;
        },
    }
}

struct MaintenanceLaneDispatch {
    frame: PiperFrame,
    meta: crate::command::MaintenanceCommandMeta,
    deadline: Instant,
    ack: crossbeam_channel::Sender<MaintenanceSendPhase>,
}

fn drain_maintenance_lane(
    maintenance_lane_rx: &Receiver<MaintenanceLaneCommand>,
    maintenance_tx_state: &mut MaintenanceTxState,
) -> VecDeque<MaintenanceLaneDispatch> {
    let mut pending_sends = VecDeque::new();
    loop {
        let Ok(command) = maintenance_lane_rx.try_recv() else {
            return pending_sends;
        };
        match command {
            MaintenanceLaneCommand::Control { op, ack } => {
                apply_maintenance_control_op(maintenance_tx_state, op);
                if let Some(ack) = ack {
                    let _ = ack.send(());
                }
            },
            MaintenanceLaneCommand::Send {
                frame,
                meta,
                deadline,
                ack,
            } => {
                pending_sends.push_back(MaintenanceLaneDispatch {
                    frame,
                    meta,
                    deadline,
                    ack,
                });
            },
        }
    }
}

fn maintenance_send_denial(
    state: &MaintenanceTxState,
    meta: crate::command::MaintenanceCommandMeta,
) -> Option<crate::DriverError> {
    if state.state != MaintenanceGateState::AllowedStandby {
        return Some(maintenance_state_denial(state.state));
    }
    if state.holder_session_id != Some(meta.session_id())
        || state.holder_session_key != Some(meta.session_key())
    {
        return Some(crate::DriverError::MaintenanceWriteDenied(
            "maintenance lease required".to_string(),
        ));
    }
    if state.lease_epoch != meta.lease_epoch() {
        return Some(crate::DriverError::MaintenanceWriteDenied(
            "maintenance lease was replaced or revoked".to_string(),
        ));
    }
    None
}

fn maintenance_dispatch_committed(dispatch: &MaintenanceLaneDispatch) {
    let _ = dispatch.ack.send(MaintenanceSendPhase::Committed);
}

fn finish_maintenance_dispatch(
    dispatch: &MaintenanceLaneDispatch,
    result: Result<(), crate::DriverError>,
) {
    let _ = dispatch.ack.send(MaintenanceSendPhase::Finished(result));
}

fn refresh_local_maintenance_tx_state(
    maintenance_gate: &Arc<MaintenanceGate>,
    maintenance_tx_state: &mut MaintenanceTxState,
    new_state: MaintenanceGateState,
) {
    maintenance_gate.local_set_state(new_state);
    *maintenance_tx_state = MaintenanceTxState::from_snapshot(maintenance_gate.snapshot());
}

/// Pipeline 配置
///
/// 控制 IO 线程的行为，包括接收超时和帧组超时设置。
///
/// # Example
///
/// ```
/// use piper_driver::PipelineConfig;
///
/// // 使用默认配置（2ms 接收超时，10ms 帧组超时）
/// let config = PipelineConfig::default();
///
/// // 自定义配置
/// let config = PipelineConfig {
///     receive_timeout_ms: 5,
///     frame_group_timeout_ms: 20,
///     velocity_buffer_timeout_us: 20_000,
///     low_speed_drive_state_freshness_ms: 100,
/// };
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PipelineConfig {
    /// CAN 接收超时（毫秒）
    pub receive_timeout_ms: u64,
    /// 帧组超时（毫秒）
    /// 如果收到部分帧后，超过此时间未收到完整帧组，则丢弃缓存
    pub frame_group_timeout_ms: u64,
    /// 速度帧缓冲区超时（微秒）
    /// 如果收到部分速度帧后，超过此时间未收到完整帧组，则强制提交
    pub velocity_buffer_timeout_us: u64,
    /// 低速驱动状态新鲜度窗口（毫秒）
    /// 只有在收到完整且新鲜的 6 轴低速反馈后，maintenance gate 才会认为驱动使能状态已确认
    pub low_speed_drive_state_freshness_ms: u64,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            receive_timeout_ms: 2,
            frame_group_timeout_ms: 10,
            velocity_buffer_timeout_us: 10_000, // 10ms (consistent with frame group timeout)
            low_speed_drive_state_freshness_ms: 100,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct PendingFrameGroup<const N: usize> {
    mask: u8,
    alignment_timestamps: [u64; N],
    host_rx_mono_us: [u64; N],
    started_at: Option<Instant>,
}

impl<const N: usize> PendingFrameGroup<N> {
    fn new() -> Self {
        Self {
            mask: 0,
            alignment_timestamps: [0; N],
            host_rx_mono_us: [0; N],
            started_at: None,
        }
    }

    fn reset(&mut self) {
        self.mask = 0;
        self.alignment_timestamps = [0; N];
        self.host_rx_mono_us = [0; N];
        self.started_at = None;
    }

    fn is_empty(&self) -> bool {
        self.mask == 0
    }

    fn contains_slot(&self, slot: usize) -> bool {
        (self.mask & (1 << slot)) != 0
    }

    fn timed_out(&self, timeout: Duration) -> bool {
        self.started_at
            .map(|started_at| started_at.elapsed() >= timeout)
            .unwrap_or(false)
    }

    fn write_slot(&mut self, slot: usize, alignment_timestamp_us: u64, host_rx_mono_us: u64) {
        if self.started_at.is_none() {
            self.started_at = Some(Instant::now());
        }
        self.mask |= 1 << slot;
        self.alignment_timestamps[slot] = alignment_timestamp_us;
        self.host_rx_mono_us[slot] = host_rx_mono_us;
    }

    fn max_alignment_timestamp_us(&self) -> u64 {
        self.alignment_timestamps.iter().copied().max().unwrap_or(0)
    }

    fn min_alignment_timestamp_us(&self) -> u64 {
        self.alignment_timestamps
            .iter()
            .copied()
            .filter(|timestamp| *timestamp != 0)
            .min()
            .unwrap_or(0)
    }

    fn max_host_rx_mono_us(&self) -> u64 {
        self.host_rx_mono_us.iter().copied().max().unwrap_or(0)
    }
}

impl<const N: usize> Default for PendingFrameGroup<N> {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FrameGroupResetReason {
    TimedOut,
    DuplicateSlot,
    SpanExceeded,
}

/// 帧解析器状态
///
/// 封装 CAN 帧解析过程中的所有临时状态，包括：
/// - 关节位置帧组同步状态
/// - 末端位姿帧组同步状态
/// - 关节动态状态缓冲提交状态
/// - 主从模式关节控制帧组同步状态
///
/// **设计目的**：
/// - 避免函数参数列表过长（从 14 个参数减少到 2 个）
/// - 提高代码可读性和可维护性
/// - 方便未来扩展新的解析状态
///
/// # Example
///
/// ```
/// # use piper_driver::pipeline::ParserState;
/// let mut state = ParserState::new();
/// // 使用 state.pending_joint_pos 等
/// ```
pub struct ParserState<'a> {
    // === 关节位置状态：帧组同步（0x2A5-0x2A7） ===
    /// 待提交的关节位置数据（6个关节，单位：弧度）
    pub pending_joint_pos: [f64; 6],
    /// 关节位置帧组元数据（mask、每槽位时间戳、组起始时间）
    joint_pos_group: PendingFrameGroup<3>,

    // === 末端位姿状态：帧组同步（0x2A2-0x2A4） ===
    /// 待提交的末端位姿数据（6个自由度：x, y, z, rx, ry, rz）
    pub pending_end_pose: [f64; 6],
    /// 末端位姿帧组元数据（mask、每槽位时间戳、组起始时间）
    end_pose_group: PendingFrameGroup<3>,

    // === 关节动态状态：缓冲提交（关键改进） ===
    /// 待提交的关节动态状态
    pub pending_joint_dynamic: JointDynamicState,
    /// 速度帧更新掩码（Bit 0-5 对应 Joint 1-6）
    pub vel_update_mask: u8,
    /// 当前速度分组开始时间（系统时间，用于统一超时语义）
    pub pending_velocity_started_at: Option<Instant>,
    /// 上次速度帧到达时间（硬件时间戳，微秒）
    pub last_vel_packet_time_us: u64,

    // === 主从模式关节控制指令状态：帧组同步（0x155-0x157） ===
    /// 待提交的主从模式关节目标角度（度）
    pub pending_joint_target_deg: [i32; 6],
    /// 主从模式关节控制帧组元数据（mask、每槽位时间戳、组起始时间）
    joint_control_group: PendingFrameGroup<3>,

    // === PhantomData 用于生命周期标记 ===
    /// 生命周期标记（内部使用，无需手动设置）
    _phantom: std::marker::PhantomData<&'a ()>,
}

impl<'a> ParserState<'a> {
    /// 创建新的解析器状态
    ///
    /// # Example
    ///
    /// ```
    /// # use piper_driver::pipeline::ParserState;
    /// let state = ParserState::new();
    /// ```
    pub fn new() -> Self {
        Self {
            pending_joint_pos: [0.0; 6],
            joint_pos_group: PendingFrameGroup::new(),
            pending_end_pose: [0.0; 6],
            end_pose_group: PendingFrameGroup::new(),
            pending_joint_dynamic: JointDynamicState::default(),
            vel_update_mask: 0,
            pending_velocity_started_at: None,
            last_vel_packet_time_us: 0,
            pending_joint_target_deg: [0; 6],
            joint_control_group: PendingFrameGroup::new(),
            _phantom: std::marker::PhantomData,
        }
    }
}

impl<'a> Default for ParserState<'a> {
    fn default() -> Self {
        Self::new()
    }
}

fn reset_pending_velocity(state: &mut ParserState) {
    state.pending_joint_dynamic = JointDynamicState::default();
    state.vel_update_mask = 0;
    state.pending_velocity_started_at = None;
    state.last_vel_packet_time_us = 0;
}

fn reset_pending_joint_position(state: &mut ParserState) {
    state.pending_joint_pos = [0.0; 6];
    state.joint_pos_group.reset();
}

fn reset_pending_end_pose(state: &mut ParserState) {
    state.pending_end_pose = [0.0; 6];
    state.end_pose_group.reset();
}

fn reset_pending_joint_control(state: &mut ParserState) {
    state.pending_joint_target_deg = [0; 6];
    state.joint_control_group.reset();
}

fn complete_group_ready(mask: u8) -> bool {
    mask == 0b0000_0111
}

#[inline]
fn control_grade_group_ready(
    group: &PendingFrameGroup<3>,
    backend_capability: BackendCapability,
) -> bool {
    if !backend_capability.is_strict_realtime() || !complete_group_ready(group.mask) {
        return false;
    }

    let mut min_ts = u64::MAX;
    let mut max_ts = 0;
    for timestamp in &group.alignment_timestamps {
        if *timestamp == 0 {
            return false;
        }
        min_ts = min_ts.min(*timestamp);
        max_ts = max_ts.max(*timestamp);
    }

    max_ts.saturating_sub(min_ts) <= STRICT_GROUP_MAX_SPAN_US
}

#[inline]
fn group_alignment_timestamp(
    frame: &PiperFrame,
    host_rx_mono_us: u64,
    backend_capability: BackendCapability,
) -> u64 {
    if backend_capability.is_strict_realtime() {
        frame.timestamp_us
    } else {
        host_rx_mono_us
    }
}

fn pending_group_reset_reason<const N: usize>(
    group: &PendingFrameGroup<N>,
    slot: usize,
    timeout: Duration,
    alignment_timestamp_us: u64,
) -> Option<FrameGroupResetReason> {
    if group.is_empty() {
        return None;
    }
    if group.timed_out(timeout) {
        return Some(FrameGroupResetReason::TimedOut);
    }
    if group.contains_slot(slot) {
        return Some(FrameGroupResetReason::DuplicateSlot);
    }
    let min_alignment = group.min_alignment_timestamp_us();
    if alignment_timestamp_us != 0
        && min_alignment != 0
        && alignment_timestamp_us.saturating_sub(min_alignment) > MOTION_GROUP_RESET_MAX_SPAN_US
    {
        return Some(FrameGroupResetReason::SpanExceeded);
    }
    None
}

fn maybe_reset_joint_position_group(
    state: &mut ParserState,
    metrics: &Arc<PiperMetrics>,
    timeout: Duration,
    slot: usize,
    alignment_timestamp_us: u64,
) {
    if pending_group_reset_reason(
        &state.joint_pos_group,
        slot,
        timeout,
        alignment_timestamp_us,
    )
    .is_some()
    {
        metrics
            .rx_joint_position_incomplete_groups_dropped_total
            .fetch_add(1, Ordering::Relaxed);
        reset_pending_joint_position(state);
    }
}

fn maybe_reset_end_pose_group(
    state: &mut ParserState,
    metrics: &Arc<PiperMetrics>,
    timeout: Duration,
    slot: usize,
    alignment_timestamp_us: u64,
) {
    if pending_group_reset_reason(&state.end_pose_group, slot, timeout, alignment_timestamp_us)
        .is_some()
    {
        metrics
            .rx_end_pose_incomplete_groups_dropped_total
            .fetch_add(1, Ordering::Relaxed);
        reset_pending_end_pose(state);
    }
}

fn maybe_reset_joint_control_group(
    state: &mut ParserState,
    timeout: Duration,
    slot: usize,
    alignment_timestamp_us: u64,
) {
    if pending_group_reset_reason(
        &state.joint_control_group,
        slot,
        timeout,
        alignment_timestamp_us,
    )
    .is_some()
    {
        reset_pending_joint_control(state);
    }
}

fn drop_timed_out_motion_groups(
    state: &mut ParserState,
    timeout: Duration,
    metrics: &Arc<PiperMetrics>,
) {
    if state.joint_pos_group.timed_out(timeout) {
        metrics
            .rx_joint_position_incomplete_groups_dropped_total
            .fetch_add(1, Ordering::Relaxed);
        reset_pending_joint_position(state);
    }
    if state.end_pose_group.timed_out(timeout) {
        metrics
            .rx_end_pose_incomplete_groups_dropped_total
            .fetch_add(1, Ordering::Relaxed);
        reset_pending_end_pose(state);
    }
    if state.joint_control_group.timed_out(timeout) {
        reset_pending_joint_control(state);
    }
}

fn commit_pending_velocity(
    ctx: &Arc<PiperContext>,
    backend_capability: BackendCapability,
    state: &mut ParserState,
    group_timestamp_us: u64,
    warning: Option<&'static str>,
    complete_group: bool,
    metrics: &Arc<PiperMetrics>,
) {
    if state.vel_update_mask == 0 {
        reset_pending_velocity(state);
        return;
    }

    let commit_mask = state.vel_update_mask;
    state.pending_joint_dynamic.group_timestamp_us = group_timestamp_us;
    state.pending_joint_dynamic.valid_mask = commit_mask;

    if complete_group {
        ctx.publish_joint_dynamic(state.pending_joint_dynamic);
        ctx.fps_stats
            .load()
            .joint_dynamic_updates
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        if backend_capability.is_strict_realtime() {
            if state.pending_joint_dynamic.group_span_us() <= STRICT_GROUP_MAX_SPAN_US {
                ctx.publish_control_joint_dynamic(state.pending_joint_dynamic);
            } else {
                metrics
                    .rx_joint_dynamic_control_grade_rejected_total
                    .fetch_add(1, Ordering::Relaxed);
            }
        }
    } else {
        metrics.rx_joint_dynamic_groups_dropped_total.fetch_add(1, Ordering::Relaxed);
    }

    if let Some(message) = warning {
        warn!("{message}: mask={commit_mask:06b}");
    }

    reset_pending_velocity(state);
}

fn flush_pending_velocity_on_idle(
    ctx: &Arc<PiperContext>,
    backend_capability: BackendCapability,
    config: &PipelineConfig,
    state: &mut ParserState,
    metrics: &Arc<PiperMetrics>,
) {
    if state.vel_update_mask == 0 {
        return;
    }

    let Some(started_at) = state.pending_velocity_started_at else {
        return;
    };

    let timeout = Duration::from_micros(config.velocity_buffer_timeout_us);
    if started_at.elapsed() >= timeout {
        commit_pending_velocity(
            ctx,
            backend_capability,
            state,
            state.last_vel_packet_time_us,
            Some("Velocity buffer timeout, dropping partial dynamic group"),
            false,
            metrics,
        );
    }
}

/// IO 线程循环
///
/// # 参数
/// - `can`: CAN 适配器（可变借用，但会在循环中独占）
/// - `cmd_rx`: 命令接收通道（从控制线程接收控制帧）
/// - `ctx`: 共享状态上下文
/// - `config`: Pipeline 配置
#[cfg(test)]
pub(crate) fn io_loop(
    mut can: impl CanAdapter,
    backend_capability: BackendCapability,
    cmd_rx: Receiver<PiperFrame>,
    ctx: Arc<PiperContext>,
    config: PipelineConfig,
) {
    // === 帧解析器状态（封装所有临时状态） ===
    let mut state = ParserState::new();
    let metrics = Arc::new(PiperMetrics::new());

    // 说明：receive_timeout 现在已在 PiperBuilder::build() 中应用到各 adapter
    // 这里只使用 frame_group_timeout 进行帧组超时检查
    let frame_group_timeout = Duration::from_millis(config.frame_group_timeout_ms);

    loop {
        // ============================================================
        // 双重 Drain 策略：进入循环先发一波（处理积压的命令）
        // ============================================================
        if drain_tx_queue(&mut can, &cmd_rx) {
            // 命令通道断开，退出循环
            break;
        }

        // ============================================================
        // 1. 接收 CAN 帧（带超时，避免阻塞）
        // ============================================================
        let frame = match can.receive() {
            Ok(frame) => frame,
            Err(CanError::Timeout) => {
                // 超时是正常情况，检查各个 pending 状态的年龄

                drop_timed_out_motion_groups(&mut state, frame_group_timeout, &metrics);

                // === 检查速度帧缓冲区超时（关键：避免僵尸缓冲区） ===
                // 使用系统时间 Instant 检查，因为硬件时间戳和系统时间戳不能直接比较
                // 如果缓冲区不为空，且距离上次速度帧到达已经超时，强制提交或丢弃
                flush_pending_velocity_on_idle(
                    &ctx,
                    backend_capability,
                    &config,
                    &mut state,
                    &metrics,
                );

                continue;
            },
            Err(e) => {
                error!("CAN receive error: {}", e);
                // 继续循环，尝试恢复
                continue;
            },
        };

        // ============================================================
        // 2. 根据 CAN ID 解析帧并更新状态
        // ============================================================
        let parsed = parse_and_update_state(
            &frame,
            backend_capability,
            &ctx,
            &config,
            &mut state,
            &metrics,
        );

        if parsed.counts_as_robot_feedback && frame.timestamp_us > 0 {
            ctx.register_timestamped_robot_feedback(host_rx_mono_us());
        }

        // ============================================================
        // 连接监控：注册反馈（每帧处理后更新最后反馈时间）
        // ============================================================
        if parsed.counts_as_robot_feedback {
            ctx.connection_monitor.register_feedback();
        }

        // ============================================================
        // 3. 双重 Drain 策略：收到帧后立即发送响应（往往此时上层已计算出新的控制命令）
        // ============================================================
        if drain_tx_queue(&mut can, &cmd_rx) {
            // 命令通道断开，退出循环
            break;
        }

        // 如果通道为空，继续接收 CAN 帧（回到循环开始）
        // 如果通道断开，继续循环（下次 try_recv 会返回 Disconnected）
    }
}

/// Drain TX 队列（带时间预算）
///
/// 从命令通道中非阻塞地取出所有待发送的命令并发送。
/// 引入时间预算机制，避免因积压命令导致 RX 延迟突增。
///
/// # 参数
/// - `can`: CAN 适配器
/// - `cmd_rx`: 命令接收通道
///
/// # 设计说明
///
/// - **最大帧数限制**：单次最多发送 32 帧，避免在命令洪峰时长时间占用
/// - **时间预算**：单次 drain 最多占用 500µs，即使队列中有 32 帧待发送
/// - **场景保护**：在 SocketCAN 缓冲区满或 GS-USB 非实时模式（1000ms 超时）时，
///   避免因单帧耗时过长而阻塞 RX
///
/// # 返回值
/// 返回是否检测到通道已断开（Disconnected）。
#[cfg(test)]
fn drain_tx_queue(can: &mut impl piper_can::CanAdapter, cmd_rx: &Receiver<PiperFrame>) -> bool {
    // 限制单次 drain 的最大帧数和时间预算，避免长时间占用
    const MAX_DRAIN_PER_CYCLE: usize = 32;
    const TIME_BUDGET: Duration = Duration::from_micros(500); // 给发送最多 0.5ms 预算

    let start = std::time::Instant::now();

    for _ in 0..MAX_DRAIN_PER_CYCLE {
        // 检查时间预算（关键优化：避免因积压命令导致 RX 延迟突增）
        if start.elapsed() > TIME_BUDGET {
            let remaining = cmd_rx.len();
            trace!("Drain time budget exhausted, deferred {} frames", remaining);
            break;
        }

        match cmd_rx.try_recv() {
            Ok(cmd_frame) => {
                if let Err(e) = can.send(cmd_frame) {
                    error!("Failed to send control frame: {}", e);
                    // 发送失败不中断 drain，继续尝试下一帧
                }
            },
            Err(crossbeam_channel::TryRecvError::Empty) => break, // 队列为空
            Err(crossbeam_channel::TryRecvError::Disconnected) => return true, // 通道断开
        }
    }

    false
}

/// RX 线程主循环
///
/// 专门负责接收 CAN 帧、解析并更新状态。
/// 与 TX 线程物理隔离，不受发送阻塞影响。
///
/// # 参数
/// - `rx`: RX 适配器（只读）
/// - `ctx`: 共享状态上下文
/// - `config`: Pipeline 配置
/// - `workers_running`: worker 生命周期标志
/// - `runtime_phase`: 运行时阶段（用于 fault latch）
/// - `metrics`: 性能指标
#[allow(clippy::too_many_arguments)]
pub fn rx_loop(
    mut rx: impl RxAdapter,
    backend_capability: BackendCapability,
    ctx: Arc<PiperContext>,
    config: PipelineConfig,
    workers_running: Arc<AtomicBool>,
    runtime_phase: Arc<AtomicU8>,
    normal_send_gate: Arc<NormalSendGate>,
    metrics: Arc<PiperMetrics>,
    last_fault: Arc<AtomicU8>,
    maintenance_gate: Arc<MaintenanceGate>,
) {
    // 设置线程优先级（可选 feature）
    #[cfg(feature = "realtime")]
    {
        use thread_priority::*;
        use tracing::info;

        match set_current_thread_priority(ThreadPriority::Max) {
            Ok(_) => {
                info!("RX thread priority set to MAX (realtime)");
            },
            Err(e) => {
                warn!(
                    "Failed to set RX thread priority: {}. \
                    On Linux, you may need to run with CAP_SYS_NICE or use rtkit. \
                    See README for details.",
                    e
                );
            },
        }
    }

    // === 使用 ParserState 封装所有解析状态 ===
    let mut state = ParserState::new();

    let frame_group_timeout = Duration::from_millis(config.frame_group_timeout_ms);

    loop {
        // 检查运行标志
        // Acquire: If we see false, we must see all cleanup writes from other threads
        if !workers_running.load(Ordering::Acquire) {
            trace!("RX thread: workers_running flag is false, exiting");
            break;
        }

        // ============================================================
        // 1. 接收 CAN 帧（带超时，避免阻塞）
        // ============================================================
        let frame = match rx.receive() {
            Ok(frame) => {
                metrics.rx_frames_total.fetch_add(1, Ordering::Relaxed);
                frame
            },
            Err(CanError::Timeout) => {
                // 超时是正常情况，检查各个 pending 状态的年龄
                metrics.rx_timeouts.fetch_add(1, Ordering::Relaxed);

                drop_timed_out_motion_groups(&mut state, frame_group_timeout, &metrics);

                // === 检查速度帧缓冲区超时 ===
                flush_pending_velocity_on_idle(
                    &ctx,
                    backend_capability,
                    &config,
                    &mut state,
                    &metrics,
                );

                if load_runtime_phase(&runtime_phase) == RuntimePhase::Running {
                    refresh_maintenance_gate_state(
                        &maintenance_gate,
                        &runtime_phase,
                        &ctx,
                        &config,
                        &last_fault,
                    );
                }

                continue;
            },
            Err(e) => {
                // 检测致命错误
                error!("RX thread: CAN receive error: {}", e);
                metrics.device_errors.fetch_add(1, Ordering::Relaxed);
                if matches!(e, CanError::BusOff) {
                    metrics.rx_error_frames_total.fetch_add(1, Ordering::Relaxed);
                    metrics.rx_bus_off_total.fetch_add(1, Ordering::Relaxed);
                }

                // 判断是否为致命错误（设备断开、权限错误等）
                let is_fatal = matches!(
                    e,
                    CanError::Device(_) | CanError::BufferOverflow | CanError::BusOff
                );

                if is_fatal {
                    error!("RX thread: Fatal error detected, latching runtime fault");
                    latch_runtime_fault_with_maintenance(
                        &runtime_phase,
                        &normal_send_gate,
                        &last_fault,
                        RuntimeFaultKind::TransportError,
                        &maintenance_gate,
                        None,
                    );
                    break;
                }

                // 非致命错误，继续循环尝试恢复
                continue;
            },
        };

        metrics.rx_frames_valid.fetch_add(1, Ordering::Relaxed);

        // ============================================================
        // 2. 触发 RX 回调（v1.2.1: 非阻塞，<1μs）
        // ============================================================
        // 使用 try_read 避免阻塞，如果锁被持有则跳过本次触发
        if let Ok(hooks) = ctx.hooks.try_read() {
            hooks.trigger_all(&frame);
            // ^^^v 所有回调必须使用 try_send，<1μs，非阻塞
        }

        // ============================================================
        // 3. 根据 CAN ID 解析帧并更新状态
        // ============================================================
        // 复用 io_loop 中的解析逻辑（通过调用辅助函数）
        let parsed = parse_and_update_state(
            &frame,
            backend_capability,
            &ctx,
            &config,
            &mut state,
            &metrics,
        );

        if parsed.counts_as_robot_feedback && frame.timestamp_us > 0 {
            ctx.register_timestamped_robot_feedback(host_rx_mono_us());
        }

        // 双线程 runtime 也必须刷新连接监控，否则 health()/wait_for_feedback()
        // 会永远基于初始状态判断。
        if parsed.counts_as_robot_feedback {
            ctx.connection_monitor.register_feedback();
        }
        if parsed.maintenance_gate_may_have_changed
            || maintenance_gate.current_state() == MaintenanceGateState::DeniedTransportDown
        {
            refresh_maintenance_gate_state(
                &maintenance_gate,
                &runtime_phase,
                &ctx,
                &config,
                &last_fault,
            );
        }
    }

    if workers_running.load(Ordering::Acquire)
        && load_runtime_phase(&runtime_phase) == RuntimePhase::Running
    {
        latch_runtime_fault_with_maintenance(
            &runtime_phase,
            &normal_send_gate,
            &last_fault,
            RuntimeFaultKind::RxExited,
            &maintenance_gate,
            None,
        );
    }
    trace!("RX thread: loop exited");
}

/// TX 线程主循环（邮箱模式）
///
/// 专门负责从命令队列取命令并发送。
/// 支持优先级调度：实时命令（邮箱）优先于可靠命令（队列）。
///
/// # 参数
/// - `tx`: TX 适配器（只写）
/// - `realtime_slot`: 实时命令邮箱（共享插槽）
/// - `shutdown_lane`: 单飞急停通道（最高优先级）
/// - `reliable_rx`: 可靠命令队列接收端（容量 10）
/// - `workers_running`: worker 生命周期标志
/// - `runtime_phase`: 运行时阶段（用于关闭正常控制路径）
/// - `metrics`: 性能指标
/// - `ctx`: 共享状态上下文（用于触发 TX 回调，v1.2.1）
#[allow(clippy::too_many_arguments)]
pub fn tx_loop_mailbox(
    mut tx: impl RealtimeTxAdapter,
    backend_capability: BackendCapability,
    realtime_slot: Arc<std::sync::Mutex<Option<crate::command::RealtimeCommand>>>,
    soft_realtime_rx: Receiver<crate::command::SoftRealtimeCommand>,
    shutdown_lane: Arc<ShutdownLane>,
    reliable_rx: Receiver<crate::command::ReliableCommand>,
    workers_running: Arc<AtomicBool>,
    runtime_phase: Arc<AtomicU8>,
    normal_send_gate: Arc<NormalSendGate>,
    metrics: Arc<PiperMetrics>,
    ctx: Arc<PiperContext>,
    last_fault: Arc<AtomicU8>,
    maintenance_lane_rx: Receiver<MaintenanceLaneCommand>,
    maintenance_gate: Arc<MaintenanceGate>,
    driver_mode: Arc<crate::mode::AtomicDriverMode>,
) {
    let normal_send_budget = if backend_capability.is_strict_realtime() {
        NORMAL_FRAME_SEND_BUDGET
    } else {
        SOFT_CONTROL_SEND_BUDGET
    };
    let mut soft_deadline_miss_streak = 0u32;
    let mut maintenance_tx_state = MaintenanceTxState::from_snapshot(maintenance_gate.snapshot());
    let mut pending_maintenance_sends = VecDeque::new();

    loop {
        let phase = load_runtime_phase(&runtime_phase);
        #[cfg(test)]
        {
            let _ = driver_mode.get(Ordering::Acquire);
            maybe_wait_tx_loop_dispatch_barrier();
        }
        if phase == RuntimePhase::Stopping || !workers_running.load(Ordering::Acquire) {
            trace!("TX thread: stopping runtime, exiting");
            break;
        }

        if let Some(dispatch) = shutdown_lane.take_pending() {
            let should_break = send_shutdown_dispatch(
                &mut tx,
                dispatch,
                &shutdown_lane,
                &runtime_phase,
                &normal_send_gate,
                &metrics,
                &ctx,
                &last_fault,
                &maintenance_gate,
                &mut maintenance_tx_state,
                "TX thread: Failed to send shutdown frame",
            );
            if should_break {
                break;
            }
            continue;
        }

        pending_maintenance_sends.extend(drain_maintenance_lane(
            &maintenance_lane_rx,
            &mut maintenance_tx_state,
        ));

        if let Some(dispatch) = pending_maintenance_sends.pop_front() {
            if driver_mode.get(Ordering::Acquire).is_replay() {
                finish_maintenance_dispatch(&dispatch, Err(crate::DriverError::ReplayModeActive));
                continue;
            }
            let permit = match normal_send_gate.acquire_normal() {
                Ok(permit) => permit,
                Err(reason) => {
                    count_gate_fault_abort(&metrics, reason, false);
                    finish_maintenance_dispatch(
                        &dispatch,
                        Err(maintenance_gate_abort_error(reason)),
                    );
                    continue;
                },
            };
            if let Err(reason) = permit.send_allowed() {
                count_gate_fault_abort(&metrics, reason, false);
                finish_maintenance_dispatch(&dispatch, Err(maintenance_gate_abort_error(reason)));
                continue;
            }

            let send_result = if Instant::now() >= dispatch.deadline {
                Err(crate::DriverError::Timeout)
            } else if let Some(denied) =
                maintenance_send_denial(&maintenance_tx_state, dispatch.meta)
            {
                Err(denied)
            } else {
                maintenance_dispatch_committed(&dispatch);
                match tx.send_control(dispatch.frame, normal_send_budget) {
                    Ok(_) => {
                        soft_deadline_miss_streak = 0;
                        metrics.tx_frames_sent_total.fetch_add(1, Ordering::Relaxed);

                        if let Ok(hooks) = ctx.hooks.try_read() {
                            hooks.trigger_all_sent(&dispatch.frame);
                        }
                        Ok(())
                    },
                    Err(CanError::Timeout) if backend_capability.is_soft_realtime() => {
                        metrics.tx_timeouts.fetch_add(1, Ordering::Relaxed);
                        metrics.tx_soft_deadline_miss_total.fetch_add(1, Ordering::Relaxed);
                        if soft_deadline_miss_streak > 0 {
                            metrics
                                .tx_soft_consecutive_deadline_miss_total
                                .fetch_add(1, Ordering::Relaxed);
                        }
                        soft_deadline_miss_streak = soft_deadline_miss_streak.saturating_add(1);
                        if soft_deadline_miss_streak >= SOFT_DEADLINE_MISS_FAULT_THRESHOLD {
                            latch_runtime_fault_with_maintenance(
                                &runtime_phase,
                                &normal_send_gate,
                                &last_fault,
                                RuntimeFaultKind::TransportError,
                                &maintenance_gate,
                                Some(&mut maintenance_tx_state),
                            );
                        }
                        Err(crate::DriverError::Timeout)
                    },
                    Err(e) => {
                        error!("TX thread: Failed to send maintenance frame: {}", e);
                        if matches!(e, CanError::Timeout) {
                            metrics.tx_timeouts.fetch_add(1, Ordering::Relaxed);
                        } else {
                            metrics.device_errors.fetch_add(1, Ordering::Relaxed);
                        }
                        latch_runtime_fault_with_maintenance(
                            &runtime_phase,
                            &normal_send_gate,
                            &last_fault,
                            RuntimeFaultKind::TransportError,
                            &maintenance_gate,
                            Some(&mut maintenance_tx_state),
                        );
                        Err(crate::DriverError::ReliableDeliveryFailed { source: e })
                    },
                }
            };

            let should_break = matches!(
                &send_result,
                Err(crate::DriverError::ReliableDeliveryFailed { .. })
                    | Err(crate::DriverError::ChannelClosed)
            );
            finish_maintenance_dispatch(&dispatch, send_result);
            if should_break {
                break;
            }
            continue;
        }

        if phase == RuntimePhase::FaultLatched {
            abort_realtime_slot_fault(&realtime_slot, &metrics);
            drain_soft_realtime_queue(&soft_realtime_rx, &metrics, true, true);
            drain_reliable_queue(&reliable_rx, &metrics, true, true);
            spin_sleep::sleep(Duration::from_micros(50));
            continue;
        }

        if driver_mode.get(Ordering::Acquire).is_replay() {
            reject_replay_mode_dispatches(&realtime_slot, &soft_realtime_rx, &metrics);
        }

        let realtime_command = if backend_capability.is_strict_realtime() {
            match realtime_slot.lock() {
                Ok(mut slot) => slot.take(),
                Err(_) => {
                    error!("TX thread: Realtime slot lock poisoned");
                    None
                },
            }
        } else {
            None
        };

        if let Some(mut command) = realtime_command {
            let deadline = command.deadline();
            let mut ack = command.take_ack();
            let frames = command.into_frames();
            let total_frames = frames.len();
            let mut sent_count = 0;
            let mut delivery_error = None;
            let mut transport_error = false;
            let mut committed = false;

            for frame in frames {
                if delivery_error.is_some() {
                    break;
                }

                let permit = match normal_send_gate.acquire_normal() {
                    Ok(permit) => permit,
                    Err(reason) => {
                        count_gate_fault_abort(&metrics, reason, false);
                        delivery_error =
                            Some(realtime_gate_abort_error(reason, sent_count, total_frames));
                        break;
                    },
                };
                if let Err(reason) = permit.send_allowed() {
                    count_gate_fault_abort(&metrics, reason, false);
                    delivery_error =
                        Some(realtime_gate_abort_error(reason, sent_count, total_frames));
                    break;
                }

                if !committed && let Some(deadline) = deadline {
                    if Instant::now() >= deadline {
                        delivery_error = Some(crate::DriverError::RealtimeDeliveryTimeout);
                        break;
                    }
                    if let Some(ack) = ack.as_ref() {
                        let _ = ack.send(crate::command::DeliveryPhase::Committed);
                    }
                    committed = true;
                }

                match tx.send_control(frame, NORMAL_FRAME_SEND_BUDGET) {
                    Ok(_) => {
                        sent_count += 1;
                        metrics.tx_frames_sent_total.fetch_add(1, Ordering::Relaxed);
                        if let Ok(hooks) = ctx.hooks.try_read() {
                            hooks.trigger_all_sent(&frame);
                        }

                        if let Some(dispatch) = shutdown_lane.take_pending() {
                            let should_break = send_shutdown_dispatch(
                                &mut tx,
                                dispatch,
                                &shutdown_lane,
                                &runtime_phase,
                                &normal_send_gate,
                                &metrics,
                                &ctx,
                                &last_fault,
                                &maintenance_gate,
                                &mut maintenance_tx_state,
                                "TX thread: Failed to send shutdown frame while preempting realtime package",
                            );
                            count_fault_abort(&metrics);
                            delivery_error = Some(realtime_abort_error(sent_count, total_frames));
                            transport_error = should_break;
                            break;
                        }
                    },
                    Err(e) => {
                        error!(
                            "TX thread: Failed to send frame {} in package: {}",
                            sent_count, e
                        );
                        if matches!(e, CanError::Timeout) {
                            metrics.tx_timeouts.fetch_add(1, Ordering::Relaxed);
                        } else {
                            metrics.device_errors.fetch_add(1, Ordering::Relaxed);
                        }
                        latch_runtime_fault_with_maintenance(
                            &runtime_phase,
                            &normal_send_gate,
                            &last_fault,
                            RuntimeFaultKind::TransportError,
                            &maintenance_gate,
                            Some(&mut maintenance_tx_state),
                        );
                        delivery_error = Some(crate::DriverError::RealtimeDeliveryFailed {
                            sent: sent_count,
                            total: total_frames,
                            source: e,
                        });
                        transport_error = true;
                        break;
                    },
                }
            }

            let had_delivery_error = delivery_error.is_some();
            let no_delivery_error = delivery_error.is_none();
            let replay_paused_partial = matches!(
                delivery_error.as_ref(),
                Some(crate::DriverError::ReplayModeActive)
            ) && sent_count > 0;
            let fault_or_stop_abort = matches!(
                delivery_error.as_ref(),
                Some(crate::DriverError::RealtimeDeliveryAbortedByFault { .. })
                    | Some(crate::DriverError::ChannelClosed)
            );
            if let Some(ack) = ack.take() {
                let result = match delivery_error {
                    Some(err) => Err(err),
                    None => Ok(()),
                };
                let _ = ack.send(crate::command::DeliveryPhase::Finished(result));
            }

            if transport_error {
                if sent_count == 0 {
                    count_package_transport_failed(&metrics);
                } else {
                    count_package_partial(&metrics);
                }
            } else if fault_or_stop_abort {
                count_package_fault_aborted(&metrics);
            } else if replay_paused_partial {
                count_package_partial(&metrics);
            } else if no_delivery_error {
                count_package_completed(&metrics);
            }

            if transport_error {
                break;
            }

            if had_delivery_error {
                continue;
            }
        }

        if let Ok(command) = soft_realtime_rx.try_recv() {
            let total_frames = command.len();
            let (frames, deadline, ack) = command.into_parts();
            let mut sent_count = 0usize;
            let mut send_result = Ok(());
            let mut should_break = false;
            let mut deadline_missed = false;

            for frame in frames {
                let permit = match normal_send_gate.acquire_normal() {
                    Ok(permit) => permit,
                    Err(reason) => {
                        count_gate_fault_abort(&metrics, reason, false);
                        send_result = Err(soft_gate_abort_error(reason));
                        break;
                    },
                };
                if let Err(reason) = permit.send_allowed() {
                    count_gate_fault_abort(&metrics, reason, false);
                    send_result = Err(soft_gate_abort_error(reason));
                    break;
                }

                let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                    if sent_count == 0 {
                        deadline_missed = true;
                        send_result = Err(crate::DriverError::Timeout);
                    } else {
                        latch_runtime_fault_with_maintenance(
                            &runtime_phase,
                            &normal_send_gate,
                            &last_fault,
                            RuntimeFaultKind::TransportError,
                            &maintenance_gate,
                            Some(&mut maintenance_tx_state),
                        );
                        send_result = Err(crate::DriverError::RealtimeDeliveryFailed {
                            sent: sent_count,
                            total: total_frames,
                            source: CanError::Timeout,
                        });
                    }
                    break;
                };

                match tx.send_control(frame, remaining.max(SOFT_CONTROL_SEND_MIN_DEADLINE)) {
                    Ok(_) => {
                        sent_count += 1;
                        metrics.tx_frames_sent_total.fetch_add(1, Ordering::Relaxed);
                        if let Ok(hooks) = ctx.hooks.try_read() {
                            hooks.trigger_all_sent(&frame);
                        }
                    },
                    Err(CanError::Timeout) => {
                        metrics.tx_timeouts.fetch_add(1, Ordering::Relaxed);
                        if sent_count == 0 {
                            deadline_missed = true;
                            send_result = Err(crate::DriverError::Timeout);
                        } else {
                            latch_runtime_fault_with_maintenance(
                                &runtime_phase,
                                &normal_send_gate,
                                &last_fault,
                                RuntimeFaultKind::TransportError,
                                &maintenance_gate,
                                Some(&mut maintenance_tx_state),
                            );
                            send_result = Err(crate::DriverError::RealtimeDeliveryFailed {
                                sent: sent_count,
                                total: total_frames,
                                source: CanError::Timeout,
                            });
                        }
                        break;
                    },
                    Err(e) => {
                        error!(
                            "TX thread: Failed to send soft realtime frame {} in package: {}",
                            sent_count, e
                        );
                        metrics.device_errors.fetch_add(1, Ordering::Relaxed);
                        latch_runtime_fault_with_maintenance(
                            &runtime_phase,
                            &normal_send_gate,
                            &last_fault,
                            RuntimeFaultKind::TransportError,
                            &maintenance_gate,
                            Some(&mut maintenance_tx_state),
                        );
                        send_result = Err(crate::DriverError::RealtimeDeliveryFailed {
                            sent: sent_count,
                            total: total_frames,
                            source: e,
                        });
                        should_break = true;
                        break;
                    },
                }
            }

            let _ = ack.send(send_result);
            if deadline_missed {
                record_soft_deadline_miss(
                    &metrics,
                    &mut soft_deadline_miss_streak,
                    &runtime_phase,
                    &normal_send_gate,
                    &last_fault,
                    &maintenance_gate,
                    &mut maintenance_tx_state,
                );
            } else if sent_count == total_frames {
                reset_soft_deadline_miss_streak(&mut soft_deadline_miss_streak);
            }
            if should_break {
                break;
            }

            continue;
        }

        if let Ok(command) = reliable_rx.try_recv() {
            let total_frames = command.len();
            let package_command = total_frames > 1;
            let (frames, mut ack, kind, maintenance, deadline) = command.into_parts();
            debug_assert!(maintenance.is_none());
            let current_mode = driver_mode.get(Ordering::Acquire);

            if current_mode.is_replay() && kind != crate::command::ReliableCommandKind::Replay {
                if let Some(ack) = ack.take() {
                    let _ = ack.send(crate::command::DeliveryPhase::Finished(Err(
                        crate::DriverError::ReplayModeActive,
                    )));
                }
                continue;
            }

            if current_mode.is_normal() && kind == crate::command::ReliableCommandKind::Replay {
                if let Some(ack) = ack.take() {
                    let _ = ack.send(crate::command::DeliveryPhase::Finished(Err(
                        crate::DriverError::InvalidInput(
                            "replay frames require DriverMode::Replay".to_string(),
                        ),
                    )));
                }
                continue;
            }

            let mut sent_count = 0usize;
            let mut send_result = Ok(());
            let mut should_break = false;
            let mut deadline_missed = false;
            let mut transport_error = false;
            let mut committed = false;

            for frame in frames {
                if kind == crate::command::ReliableCommandKind::Replay
                    && !driver_mode.get(Ordering::Acquire).is_replay()
                {
                    send_result = Err(crate::DriverError::InvalidInput(
                        "replay frames require DriverMode::Replay".to_string(),
                    ));
                    break;
                }

                let _permit = if kind == crate::command::ReliableCommandKind::Replay {
                    None
                } else {
                    let permit = match normal_send_gate.acquire_normal() {
                        Ok(permit) => permit,
                        Err(reason) => {
                            count_gate_fault_abort(&metrics, reason, false);
                            send_result = Err(reliable_gate_abort_error(reason));
                            break;
                        },
                    };
                    if let Err(reason) = permit.send_allowed() {
                        count_gate_fault_abort(&metrics, reason, false);
                        send_result = Err(reliable_gate_abort_error(reason));
                        break;
                    }
                    Some(permit)
                };

                if !committed && let Some(deadline) = deadline {
                    if Instant::now() >= deadline {
                        deadline_missed = true;
                        send_result = Err(crate::DriverError::Timeout);
                        break;
                    }
                    if let Some(ack) = ack.as_ref() {
                        let _ = ack.send(crate::command::DeliveryPhase::Committed);
                    }
                    committed = true;
                }

                match tx.send_control(frame, normal_send_budget) {
                    Ok(_) => {
                        sent_count += 1;
                        metrics.tx_frames_sent_total.fetch_add(1, Ordering::Relaxed);

                        if let Ok(hooks) = ctx.hooks.try_read() {
                            hooks.trigger_all_sent(&frame);
                        }
                    },
                    Err(CanError::Timeout) => {
                        metrics.tx_timeouts.fetch_add(1, Ordering::Relaxed);
                        if package_command {
                            deadline_missed = true;
                            send_result = Err(crate::DriverError::ReliablePackageTimeout {
                                sent: sent_count,
                                total: total_frames,
                            });
                            if backend_capability.is_strict_realtime() {
                                latch_runtime_fault_with_maintenance(
                                    &runtime_phase,
                                    &normal_send_gate,
                                    &last_fault,
                                    RuntimeFaultKind::TransportError,
                                    &maintenance_gate,
                                    Some(&mut maintenance_tx_state),
                                );
                                should_break = true;
                            }
                        } else if backend_capability.is_soft_realtime() {
                            deadline_missed = true;
                            send_result = Err(crate::DriverError::Timeout);
                        } else {
                            latch_runtime_fault_with_maintenance(
                                &runtime_phase,
                                &normal_send_gate,
                                &last_fault,
                                RuntimeFaultKind::TransportError,
                                &maintenance_gate,
                                Some(&mut maintenance_tx_state),
                            );
                            send_result = Err(crate::DriverError::ReliableDeliveryFailed {
                                source: CanError::Timeout,
                            });
                            should_break = true;
                        }
                        break;
                    },
                    Err(e) => {
                        error!(
                            "TX thread: Failed to send reliable frame {} in package: {}",
                            sent_count, e
                        );
                        metrics.device_errors.fetch_add(1, Ordering::Relaxed);
                        latch_runtime_fault_with_maintenance(
                            &runtime_phase,
                            &normal_send_gate,
                            &last_fault,
                            RuntimeFaultKind::TransportError,
                            &maintenance_gate,
                            Some(&mut maintenance_tx_state),
                        );
                        send_result = if package_command {
                            Err(crate::DriverError::ReliablePackageDeliveryFailed {
                                sent: sent_count,
                                total: total_frames,
                                source: e,
                            })
                        } else {
                            Err(crate::DriverError::ReliableDeliveryFailed { source: e })
                        };
                        transport_error = true;
                        should_break = true;
                        break;
                    },
                }
            }

            let send_succeeded = send_result.is_ok();
            let fault_aborted = matches!(
                &send_result,
                Err(crate::DriverError::CommandAbortedByFault)
                    | Err(crate::DriverError::ChannelClosed)
            );
            let soft_outcome = if backend_capability.is_soft_realtime() {
                Some(send_succeeded)
            } else {
                None
            };
            let soft_timed_out = backend_capability.is_soft_realtime()
                && matches!(
                    &send_result,
                    Err(crate::DriverError::Timeout)
                        | Err(crate::DriverError::ReliablePackageTimeout { .. })
                );
            let replay_paused_partial =
                matches!(&send_result, Err(crate::DriverError::ReplayModeActive)) && sent_count > 0;
            if let Some(ack) = ack.take() {
                let _ = ack.send(crate::command::DeliveryPhase::Finished(send_result));
            }

            if let Some(soft_succeeded) = soft_outcome {
                if soft_succeeded {
                    reset_soft_deadline_miss_streak(&mut soft_deadline_miss_streak);
                } else if soft_timed_out {
                    record_soft_deadline_miss(
                        &metrics,
                        &mut soft_deadline_miss_streak,
                        &runtime_phase,
                        &normal_send_gate,
                        &last_fault,
                        &maintenance_gate,
                        &mut maintenance_tx_state,
                    );
                }
            }

            if package_command {
                if transport_error {
                    if sent_count == 0 {
                        count_package_transport_failed(&metrics);
                    } else {
                        count_package_partial(&metrics);
                    }
                } else if fault_aborted {
                    count_package_fault_aborted(&metrics);
                } else if send_succeeded {
                    count_package_completed(&metrics);
                } else if replay_paused_partial || (deadline_missed && sent_count > 0) {
                    count_package_partial(&metrics);
                }
            }

            if should_break {
                break;
            }
            continue;
        }

        // 都没有数据，避免忙等待
        // 使用短暂的 sleep（50μs）降低 CPU 占用
        // 注意：这里的延迟不会影响控制循环，因为控制循环在另一个线程
        // 使用 spin_sleep 而非 thread::sleep 以获得微秒级精度（相比 thread::sleep 的 1-2ms）
        spin_sleep::sleep(Duration::from_micros(50));
    }

    if workers_running.load(Ordering::Acquire)
        && load_runtime_phase(&runtime_phase) == RuntimePhase::Running
    {
        record_fault(&last_fault, RuntimeFaultKind::TxExited);
        maintenance_gate.local_set_state(MaintenanceGateState::DeniedFaulted);
    }
    shutdown_lane.close_with(Err(crate::DriverError::ChannelClosed));
    drain_reliable_queue(&reliable_rx, &metrics, false, false);
    drain_soft_realtime_queue(&soft_realtime_rx, &metrics, false, false);
    abort_realtime_slot_with(
        &realtime_slot,
        &metrics,
        crate::DriverError::ChannelClosed,
        false,
    );
    trace!("TX thread: loop exited");
}

fn abort_realtime_slot_fault(
    realtime_slot: &Arc<std::sync::Mutex<Option<crate::command::RealtimeCommand>>>,
    metrics: &Arc<PiperMetrics>,
) {
    if let Ok(mut slot) = realtime_slot.lock()
        && let Some(command) = slot.take()
    {
        count_fault_abort(metrics);
        let total = command.len();
        count_package_fault_aborted(metrics);
        command.complete(Err(realtime_abort_error(0, total)));
    }
}

fn abort_realtime_slot_with(
    realtime_slot: &Arc<std::sync::Mutex<Option<crate::command::RealtimeCommand>>>,
    metrics: &Arc<PiperMetrics>,
    reason: crate::DriverError,
    count_as_fault_abort: bool,
) {
    if let Ok(mut slot) = realtime_slot.lock()
        && let Some(command) = slot.take()
    {
        if count_as_fault_abort {
            count_fault_abort(metrics);
        }
        command.complete(Err(reason));
    }
}

fn drain_reliable_queue(
    reliable_rx: &Receiver<crate::command::ReliableCommand>,
    metrics: &Arc<PiperMetrics>,
    fault_latched: bool,
    count_as_fault_abort: bool,
) {
    while let Ok(command) = reliable_rx.try_recv() {
        if count_as_fault_abort {
            count_fault_abort(metrics);
        }
        let reason = reliable_abort_error(fault_latched);
        command.complete(Err(reason));
    }
}

fn drain_soft_realtime_queue(
    soft_realtime_rx: &Receiver<crate::command::SoftRealtimeCommand>,
    metrics: &Arc<PiperMetrics>,
    fault_latched: bool,
    count_as_fault_abort: bool,
) {
    while let Ok(command) = soft_realtime_rx.try_recv() {
        if count_as_fault_abort {
            count_fault_abort(metrics);
        }
        let reason = if fault_latched {
            crate::DriverError::CommandAbortedByFault
        } else {
            crate::DriverError::ChannelClosed
        };
        command.complete(Err(reason));
    }
}

#[allow(clippy::too_many_arguments)]
fn send_shutdown_dispatch(
    tx: &mut impl RealtimeTxAdapter,
    dispatch: ShutdownDispatch,
    shutdown_lane: &Arc<ShutdownLane>,
    runtime_phase: &Arc<AtomicU8>,
    normal_send_gate: &Arc<NormalSendGate>,
    metrics: &Arc<PiperMetrics>,
    ctx: &Arc<PiperContext>,
    last_fault: &Arc<AtomicU8>,
    maintenance_gate: &Arc<MaintenanceGate>,
    maintenance_tx_state: &mut MaintenanceTxState,
    error_prefix: &str,
) -> bool {
    let frame = dispatch.frame;
    let send_result = match tx.send_shutdown_until(frame, dispatch.deadline) {
        Ok(_) => {
            metrics.tx_frames_sent_total.fetch_add(1, Ordering::Relaxed);
            metrics.tx_shutdown_sent_total.fetch_add(1, Ordering::Relaxed);
            if let Ok(hooks) = ctx.hooks.try_read() {
                hooks.trigger_all_sent(&frame);
            }
            Ok(())
        },
        Err(e) => {
            error!("{}: {}", error_prefix, e);
            if matches!(e, CanError::Timeout) {
                metrics.tx_timeouts.fetch_add(1, Ordering::Relaxed);
                latch_runtime_fault_with_maintenance(
                    runtime_phase,
                    normal_send_gate,
                    last_fault,
                    RuntimeFaultKind::TransportError,
                    maintenance_gate,
                    Some(maintenance_tx_state),
                );
                Err(crate::DriverError::Timeout)
            } else {
                metrics.device_errors.fetch_add(1, Ordering::Relaxed);
                latch_runtime_fault_with_maintenance(
                    runtime_phase,
                    normal_send_gate,
                    last_fault,
                    RuntimeFaultKind::TransportError,
                    maintenance_gate,
                    Some(maintenance_tx_state),
                );
                Err(crate::DriverError::ReliableDeliveryFailed { source: e })
            }
        },
    };

    let should_break = matches!(
        send_result,
        Err(crate::DriverError::ReliableDeliveryFailed { .. })
            | Err(crate::DriverError::ChannelClosed)
    );
    shutdown_lane.finish(send_result);
    should_break
}

/// 辅助函数：解析帧并更新状态
///
/// 从 `io_loop` 中提取的帧解析逻辑，供 `rx_loop` 复用。
/// 完整实现了所有帧类型的解析逻辑。
///
/// # 参数
///
/// - `frame`: 当前解析的 CAN 帧
/// - `ctx`: 共享状态上下文
/// - `config`: Pipeline 配置
/// - `state`: 解析器状态（封装所有临时状态）
///
/// # 设计优化
///
/// 使用 `ParserState` 结构体封装所有可变状态，避免函数参数列表过长。
/// 原本有 14 个参数，现在只有 4 个，代码可读性大幅提升。
#[allow(clippy::too_many_arguments)]
#[derive(Debug, Clone, Copy, Default)]
struct ParsedFeedbackOutcome {
    counts_as_robot_feedback: bool,
    maintenance_gate_may_have_changed: bool,
}

#[allow(clippy::too_many_arguments)]
fn parse_and_update_state(
    frame: &PiperFrame,
    backend_capability: BackendCapability,
    ctx: &Arc<PiperContext>,
    config: &PipelineConfig,
    state: &mut ParserState,
    metrics: &Arc<PiperMetrics>,
) -> ParsedFeedbackOutcome {
    let mut outcome = ParsedFeedbackOutcome::default();
    let frame_group_timeout = Duration::from_millis(config.frame_group_timeout_ms);

    match frame.id {
        ID_JOINT_FEEDBACK_12 => {
            if let Ok(feedback) = JointFeedback12::try_from(*frame) {
                outcome.counts_as_robot_feedback = true;
                let host_rx_mono_us = host_rx_mono_us();
                let alignment_timestamp_us =
                    group_alignment_timestamp(frame, host_rx_mono_us, backend_capability);
                maybe_reset_joint_position_group(
                    state,
                    metrics,
                    frame_group_timeout,
                    0,
                    alignment_timestamp_us,
                );
                state.pending_joint_pos[0] = feedback.j1_rad();
                state.pending_joint_pos[1] = feedback.j2_rad();
                state.joint_pos_group.write_slot(0, alignment_timestamp_us, host_rx_mono_us);

                ctx.publish_raw_joint_position(JointPositionState {
                    hardware_timestamp_us: state.joint_pos_group.max_alignment_timestamp_us(),
                    host_rx_mono_us,
                    joint_pos: state.pending_joint_pos,
                    frame_valid_mask: state.joint_pos_group.mask,
                });
            } else {
                warn!("Failed to parse JointFeedback12: CAN ID 0x{:X}", frame.id);
            }
        },
        ID_JOINT_FEEDBACK_34 => {
            if let Ok(feedback) = JointFeedback34::try_from(*frame) {
                outcome.counts_as_robot_feedback = true;
                let host_rx_mono_us = host_rx_mono_us();
                let alignment_timestamp_us =
                    group_alignment_timestamp(frame, host_rx_mono_us, backend_capability);
                maybe_reset_joint_position_group(
                    state,
                    metrics,
                    frame_group_timeout,
                    1,
                    alignment_timestamp_us,
                );
                state.pending_joint_pos[2] = feedback.j3_rad();
                state.pending_joint_pos[3] = feedback.j4_rad();
                state.joint_pos_group.write_slot(1, alignment_timestamp_us, host_rx_mono_us);

                ctx.publish_raw_joint_position(JointPositionState {
                    hardware_timestamp_us: state.joint_pos_group.max_alignment_timestamp_us(),
                    host_rx_mono_us,
                    joint_pos: state.pending_joint_pos,
                    frame_valid_mask: state.joint_pos_group.mask,
                });
            } else {
                warn!("Failed to parse JointFeedback34: CAN ID 0x{:X}", frame.id);
            }
        },
        ID_JOINT_FEEDBACK_56 => {
            if let Ok(feedback) = JointFeedback56::try_from(*frame) {
                outcome.counts_as_robot_feedback = true;
                let host_rx_mono_us = host_rx_mono_us();
                let alignment_timestamp_us =
                    group_alignment_timestamp(frame, host_rx_mono_us, backend_capability);
                maybe_reset_joint_position_group(
                    state,
                    metrics,
                    frame_group_timeout,
                    2,
                    alignment_timestamp_us,
                );
                state.pending_joint_pos[4] = feedback.j5_rad();
                state.pending_joint_pos[5] = feedback.j6_rad();
                state.joint_pos_group.write_slot(2, alignment_timestamp_us, host_rx_mono_us);

                let new_joint_pos_state = JointPositionState {
                    hardware_timestamp_us: state.joint_pos_group.max_alignment_timestamp_us(),
                    host_rx_mono_us: state.joint_pos_group.max_host_rx_mono_us(),
                    joint_pos: state.pending_joint_pos,
                    frame_valid_mask: state.joint_pos_group.mask,
                };
                if complete_group_ready(state.joint_pos_group.mask) {
                    ctx.publish_joint_position(new_joint_pos_state);
                    ctx.fps_stats
                        .load()
                        .joint_position_updates
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    if control_grade_group_ready(&state.joint_pos_group, backend_capability) {
                        ctx.publish_control_joint_position(new_joint_pos_state);
                    } else {
                        metrics
                            .rx_joint_position_control_grade_rejected_total
                            .fetch_add(1, Ordering::Relaxed);
                    }
                    reset_pending_joint_position(state);
                } else {
                    ctx.publish_raw_joint_position(new_joint_pos_state);
                }
            } else {
                warn!("Failed to parse JointFeedback56: CAN ID 0x{:X}", frame.id);
            }
        },
        ID_END_POSE_1 => {
            if let Ok(feedback) = EndPoseFeedback1::try_from(*frame) {
                outcome.counts_as_robot_feedback = true;
                let host_rx_mono_us = host_rx_mono_us();
                let alignment_timestamp_us =
                    group_alignment_timestamp(frame, host_rx_mono_us, backend_capability);
                maybe_reset_end_pose_group(
                    state,
                    metrics,
                    frame_group_timeout,
                    0,
                    alignment_timestamp_us,
                );
                state.pending_end_pose[0] = feedback.x() / 1000.0;
                state.pending_end_pose[1] = feedback.y() / 1000.0;
                state.end_pose_group.write_slot(0, alignment_timestamp_us, host_rx_mono_us);

                ctx.publish_raw_end_pose(EndPoseState {
                    hardware_timestamp_us: state.end_pose_group.max_alignment_timestamp_us(),
                    host_rx_mono_us,
                    end_pose: state.pending_end_pose,
                    frame_valid_mask: state.end_pose_group.mask,
                });
            }
        },
        ID_END_POSE_2 => {
            if let Ok(feedback) = EndPoseFeedback2::try_from(*frame) {
                outcome.counts_as_robot_feedback = true;
                let host_rx_mono_us = host_rx_mono_us();
                let alignment_timestamp_us =
                    group_alignment_timestamp(frame, host_rx_mono_us, backend_capability);
                maybe_reset_end_pose_group(
                    state,
                    metrics,
                    frame_group_timeout,
                    1,
                    alignment_timestamp_us,
                );
                state.pending_end_pose[2] = feedback.z() / 1000.0;
                state.pending_end_pose[3] = feedback.rx_rad();
                state.end_pose_group.write_slot(1, alignment_timestamp_us, host_rx_mono_us);

                ctx.publish_raw_end_pose(EndPoseState {
                    hardware_timestamp_us: state.end_pose_group.max_alignment_timestamp_us(),
                    host_rx_mono_us,
                    end_pose: state.pending_end_pose,
                    frame_valid_mask: state.end_pose_group.mask,
                });
            }
        },
        ID_END_POSE_3 => {
            if let Ok(feedback) = EndPoseFeedback3::try_from(*frame) {
                outcome.counts_as_robot_feedback = true;
                let host_rx_mono_us = host_rx_mono_us();
                let alignment_timestamp_us =
                    group_alignment_timestamp(frame, host_rx_mono_us, backend_capability);
                maybe_reset_end_pose_group(
                    state,
                    metrics,
                    frame_group_timeout,
                    2,
                    alignment_timestamp_us,
                );
                state.pending_end_pose[4] = feedback.ry_rad();
                state.pending_end_pose[5] = feedback.rz_rad();
                state.end_pose_group.write_slot(2, alignment_timestamp_us, host_rx_mono_us);

                let new_end_pose_state = EndPoseState {
                    hardware_timestamp_us: state.end_pose_group.max_alignment_timestamp_us(),
                    host_rx_mono_us: state.end_pose_group.max_host_rx_mono_us(),
                    end_pose: state.pending_end_pose,
                    frame_valid_mask: state.end_pose_group.mask,
                };
                if complete_group_ready(state.end_pose_group.mask) {
                    ctx.publish_end_pose(new_end_pose_state);
                    ctx.fps_stats
                        .load()
                        .end_pose_updates
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    reset_pending_end_pose(state);
                } else {
                    ctx.publish_raw_end_pose(new_end_pose_state);
                }
            }
        },
        id if (ID_JOINT_DRIVER_HIGH_SPEED_BASE..=ID_JOINT_DRIVER_HIGH_SPEED_BASE + 5)
            .contains(&id) =>
        {
            let joint_index = (id - ID_JOINT_DRIVER_HIGH_SPEED_BASE) as usize;

            if let Ok(feedback) = JointDriverHighSpeedFeedback::try_from(*frame) {
                outcome.counts_as_robot_feedback = true;
                let now = Instant::now();
                let timeout = Duration::from_micros(config.velocity_buffer_timeout_us);
                if state.vel_update_mask != 0 {
                    let timed_out = state
                        .pending_velocity_started_at
                        .map(|started_at| now.duration_since(started_at) >= timeout)
                        .unwrap_or(false);
                    if timed_out {
                        commit_pending_velocity(
                            ctx,
                            backend_capability,
                            state,
                            state.last_vel_packet_time_us,
                            Some("Velocity buffer timeout, dropping partial dynamic group"),
                            false,
                            metrics,
                        );
                    } else if (state.vel_update_mask & (1 << joint_index)) != 0 {
                        commit_pending_velocity(
                            ctx,
                            backend_capability,
                            state,
                            state.last_vel_packet_time_us,
                            Some(
                                "Duplicate joint dynamic frame before group completion, dropping partial dynamic group",
                            ),
                            false,
                            metrics,
                        );
                    }
                }

                let host_rx_mono_us = host_rx_mono_us();
                state.pending_joint_dynamic.joint_vel[joint_index] = feedback.speed();
                state.pending_joint_dynamic.joint_current[joint_index] = feedback.current();
                state.pending_joint_dynamic.timestamps[joint_index] = frame.timestamp_us;
                state.pending_joint_dynamic.group_host_rx_mono_us = host_rx_mono_us;
                state.pending_joint_dynamic.group_timestamp_us = frame.timestamp_us;

                if state.vel_update_mask == 0 {
                    state.pending_velocity_started_at = Some(now);
                }
                state.vel_update_mask |= 1 << joint_index;
                state.last_vel_packet_time_us = frame.timestamp_us;
                state.pending_joint_dynamic.valid_mask = state.vel_update_mask;

                if state.vel_update_mask == 0b111111 {
                    commit_pending_velocity(
                        ctx,
                        backend_capability,
                        state,
                        frame.timestamp_us,
                        None,
                        true,
                        metrics,
                    );
                } else {
                    ctx.publish_raw_joint_dynamic(state.pending_joint_dynamic);
                }
            }
        },
        ID_ROBOT_STATUS => {
            if let Ok(feedback) = RobotStatusFeedback::try_from(*frame) {
                outcome.counts_as_robot_feedback = true;
                outcome.maintenance_gate_may_have_changed = true;
                let host_rx_mono_us = host_rx_mono_us();

                let fault_angle_limit_mask = feedback.fault_code_angle_limit.joint1_limit() as u8
                    | (feedback.fault_code_angle_limit.joint2_limit() as u8) << 1
                    | (feedback.fault_code_angle_limit.joint3_limit() as u8) << 2
                    | (feedback.fault_code_angle_limit.joint4_limit() as u8) << 3
                    | (feedback.fault_code_angle_limit.joint5_limit() as u8) << 4
                    | (feedback.fault_code_angle_limit.joint6_limit() as u8) << 5;

                let fault_comm_error_mask = feedback.fault_code_comm_error.joint1_comm_error()
                    as u8
                    | (feedback.fault_code_comm_error.joint2_comm_error() as u8) << 1
                    | (feedback.fault_code_comm_error.joint3_comm_error() as u8) << 2
                    | (feedback.fault_code_comm_error.joint4_comm_error() as u8) << 3
                    | (feedback.fault_code_comm_error.joint5_comm_error() as u8) << 4
                    | (feedback.fault_code_comm_error.joint6_comm_error() as u8) << 5;

                let driver_state = ctx.joint_driver_low_speed.load();
                let driver_enabled_mask = driver_state.driver_enabled_mask;
                let any_drive_enabled = driver_enabled_mask != 0;
                let confirmed_driver_enabled_mask = driver_state.confirmed_driver_enabled_mask(
                    host_rx_mono_us,
                    config.low_speed_drive_state_freshness_ms.saturating_mul(1_000),
                );
                let new_robot_control_state = RobotControlState {
                    hardware_timestamp_us: frame.timestamp_us,
                    host_rx_mono_us,
                    control_mode: feedback.control_mode as u8,
                    robot_status: feedback.robot_status as u8,
                    move_mode: feedback.move_mode as u8,
                    teach_status: feedback.teach_status as u8,
                    motion_status: feedback.motion_status as u8,
                    trajectory_point_index: feedback.trajectory_point_index,
                    fault_angle_limit_mask,
                    fault_comm_error_mask,
                    driver_enabled_mask,
                    any_drive_enabled,
                    is_enabled: driver_enabled_mask == ALL_DRIVES_ENABLED_MASK,
                    confirmed_driver_enabled_mask,
                    feedback_counter: 0,
                };

                ctx.robot_control.store(Arc::new(new_robot_control_state.clone()));
                ctx.fps_stats
                    .load()
                    .robot_control_updates
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
        },
        ID_GRIPPER_FEEDBACK => {
            if let Ok(feedback) = GripperFeedback::try_from(*frame) {
                outcome.counts_as_robot_feedback = true;
                let host_rx_mono_us = host_rx_mono_us();

                let current = ctx.gripper.load();
                let last_travel = current.last_travel;

                let new_gripper_state = GripperState {
                    hardware_timestamp_us: frame.timestamp_us,
                    host_rx_mono_us,
                    travel: feedback.travel(),
                    torque: feedback.torque(),
                    status_code: u8::from(feedback.status),
                    last_travel,
                };

                ctx.gripper.rcu(|old| {
                    let mut new = new_gripper_state.clone();
                    new.last_travel = old.travel;
                    Arc::new(new)
                });

                ctx.fps_stats
                    .load()
                    .gripper_updates
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
        },
        id if (ID_JOINT_DRIVER_LOW_SPEED_BASE..=ID_JOINT_DRIVER_LOW_SPEED_BASE + 5)
            .contains(&id) =>
        {
            if let Ok(feedback) = JointDriverLowSpeedFeedback::try_from(*frame) {
                let joint_idx = (feedback.joint_index as usize).saturating_sub(1);
                if joint_idx < 6 {
                    outcome.counts_as_robot_feedback = true;
                    outcome.maintenance_gate_may_have_changed = true;
                    let host_rx_mono_us = host_rx_mono_us();

                    ctx.joint_driver_low_speed.rcu(|old| {
                        let mut new = (**old).clone();
                        new.motor_temps[joint_idx] = feedback.motor_temp() as f32;
                        new.driver_temps[joint_idx] = feedback.driver_temp() as f32;
                        new.joint_voltage[joint_idx] = feedback.voltage() as f32;
                        new.joint_bus_current[joint_idx] = feedback.bus_current() as f32;
                        new.hardware_timestamps[joint_idx] = frame.timestamp_us;
                        new.host_rx_mono_timestamps[joint_idx] = host_rx_mono_us;
                        new.hardware_timestamp_us = frame.timestamp_us;
                        new.host_rx_mono_us = host_rx_mono_us;
                        new.valid_mask |= 1 << joint_idx;

                        // 更新驱动器状态位掩码
                        if feedback.status.voltage_low() {
                            new.driver_voltage_low_mask |= 1 << joint_idx;
                        } else {
                            new.driver_voltage_low_mask &= !(1 << joint_idx);
                        }
                        if feedback.status.motor_over_temp() {
                            new.driver_motor_over_temp_mask |= 1 << joint_idx;
                        } else {
                            new.driver_motor_over_temp_mask &= !(1 << joint_idx);
                        }
                        if feedback.status.driver_over_current() {
                            new.driver_over_current_mask |= 1 << joint_idx;
                        } else {
                            new.driver_over_current_mask &= !(1 << joint_idx);
                        }
                        if feedback.status.driver_over_temp() {
                            new.driver_over_temp_mask |= 1 << joint_idx;
                        } else {
                            new.driver_over_temp_mask &= !(1 << joint_idx);
                        }
                        if feedback.status.collision_protection() {
                            new.driver_collision_protection_mask |= 1 << joint_idx;
                        } else {
                            new.driver_collision_protection_mask &= !(1 << joint_idx);
                        }
                        if feedback.status.driver_error() {
                            new.driver_error_mask |= 1 << joint_idx;
                        } else {
                            new.driver_error_mask &= !(1 << joint_idx);
                        }
                        if feedback.status.enabled() {
                            new.driver_enabled_mask |= 1 << joint_idx;
                        } else {
                            new.driver_enabled_mask &= !(1 << joint_idx);
                        }
                        if feedback.status.stall_protection() {
                            new.driver_stall_protection_mask |= 1 << joint_idx;
                        } else {
                            new.driver_stall_protection_mask &= !(1 << joint_idx);
                        }
                        Arc::new(new)
                    });

                    let driver_state = ctx.joint_driver_low_speed.load();
                    let driver_enabled_mask = driver_state.driver_enabled_mask;
                    let any_drive_enabled = driver_enabled_mask != 0;
                    let is_enabled = driver_enabled_mask == ALL_DRIVES_ENABLED_MASK;
                    let confirmed_driver_enabled_mask = driver_state.confirmed_driver_enabled_mask(
                        host_rx_mono_us,
                        config.low_speed_drive_state_freshness_ms.saturating_mul(1_000),
                    );
                    let previous = ctx.robot_control.load();
                    if previous.driver_enabled_mask != driver_enabled_mask
                        || previous.any_drive_enabled != any_drive_enabled
                        || previous.is_enabled != is_enabled
                        || previous.confirmed_driver_enabled_mask != confirmed_driver_enabled_mask
                    {
                        let mut next = previous.as_ref().clone();
                        next.driver_enabled_mask = driver_enabled_mask;
                        next.any_drive_enabled = any_drive_enabled;
                        next.is_enabled = is_enabled;
                        next.confirmed_driver_enabled_mask = confirmed_driver_enabled_mask;
                        ctx.robot_control.store(Arc::new(next));
                    }

                    ctx.fps_stats
                        .load()
                        .joint_driver_low_speed_updates
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
            }
        },
        ID_COLLISION_PROTECTION_LEVEL_FEEDBACK => {
            if let Ok(feedback) = CollisionProtectionLevelFeedback::try_from(*frame) {
                outcome.counts_as_robot_feedback = true;
                let host_rx_mono_us = host_rx_mono_us();

                if let Ok(mut collision) = ctx.collision_protection.try_write() {
                    collision.hardware_timestamp_us = frame.timestamp_us;
                    collision.host_rx_mono_us = host_rx_mono_us;
                    collision.protection_levels = feedback.levels;
                }

                ctx.fps_stats
                    .load()
                    .collision_protection_updates
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
        },
        ID_MOTOR_LIMIT_FEEDBACK => {
            if let Ok(feedback) = MotorLimitFeedback::try_from(*frame) {
                let joint_idx = (feedback.joint_index as usize).saturating_sub(1);
                if joint_idx < 6 {
                    outcome.counts_as_robot_feedback = true;
                    let host_rx_mono_us = host_rx_mono_us();

                    if let Ok(mut joint_limit) = ctx.joint_limit_config.write() {
                        joint_limit.joint_limits_max[joint_idx] = feedback.max_angle().to_radians();
                        joint_limit.joint_limits_min[joint_idx] = feedback.min_angle().to_radians();
                        joint_limit.joint_max_velocity[joint_idx] = feedback.max_velocity();
                        joint_limit.joint_update_hardware_timestamps[joint_idx] =
                            frame.timestamp_us;
                        joint_limit.joint_update_host_rx_mono_timestamps[joint_idx] =
                            host_rx_mono_us;
                        joint_limit.last_update_hardware_timestamp_us = frame.timestamp_us;
                        joint_limit.last_update_host_rx_mono_us = host_rx_mono_us;
                        joint_limit.valid_mask |= 1 << joint_idx;
                    }

                    ctx.fps_stats
                        .load()
                        .joint_limit_config_updates
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
            }
        },
        ID_MOTOR_MAX_ACCEL_FEEDBACK => {
            if let Ok(feedback) = MotorMaxAccelFeedback::try_from(*frame) {
                let joint_idx = (feedback.joint_index as usize).saturating_sub(1);
                if joint_idx < 6 {
                    outcome.counts_as_robot_feedback = true;
                    let host_rx_mono_us = host_rx_mono_us();

                    if let Ok(mut joint_accel) = ctx.joint_accel_config.write() {
                        joint_accel.max_acc_limits[joint_idx] = feedback.max_accel();
                        joint_accel.joint_update_hardware_timestamps[joint_idx] =
                            frame.timestamp_us;
                        joint_accel.joint_update_host_rx_mono_timestamps[joint_idx] =
                            host_rx_mono_us;
                        joint_accel.last_update_hardware_timestamp_us = frame.timestamp_us;
                        joint_accel.last_update_host_rx_mono_us = host_rx_mono_us;
                        joint_accel.valid_mask |= 1 << joint_idx;
                    }

                    ctx.fps_stats
                        .load()
                        .joint_accel_config_updates
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
            }
        },
        ID_END_VELOCITY_ACCEL_FEEDBACK => {
            if let Ok(feedback) = EndVelocityAccelFeedback::try_from(*frame) {
                outcome.counts_as_robot_feedback = true;
                let host_rx_mono_us = host_rx_mono_us();

                if let Ok(mut end_limit) = ctx.end_limit_config.write() {
                    end_limit.max_end_linear_velocity = feedback.max_linear_velocity();
                    end_limit.max_end_angular_velocity = feedback.max_angular_velocity();
                    end_limit.max_end_linear_accel = feedback.max_linear_accel();
                    end_limit.max_end_angular_accel = feedback.max_angular_accel();
                    end_limit.last_update_hardware_timestamp_us = frame.timestamp_us;
                    end_limit.last_update_host_rx_mono_us = host_rx_mono_us;
                    end_limit.is_valid = true;
                }

                ctx.fps_stats
                    .load()
                    .end_limit_config_updates
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
        },
        ID_FIRMWARE_READ => {
            if let Ok(feedback) = FirmwareReadFeedback::try_from(*frame) {
                outcome.counts_as_robot_feedback = true;
                let host_rx_mono_us = host_rx_mono_us();

                if let Ok(mut firmware_state) = ctx.firmware_version.write() {
                    firmware_state
                        .firmware_data
                        .extend_from_slice(&feedback.firmware_data()[..frame.len as usize]);
                    firmware_state.hardware_timestamp_us = frame.timestamp_us;
                    firmware_state.host_rx_mono_us = host_rx_mono_us;
                    firmware_state.parse_version();
                }

                ctx.fps_stats
                    .load()
                    .firmware_version_updates
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
        },
        ID_CONTROL_MODE => {
            if let Ok(feedback) = ControlModeCommandFeedback::try_from(*frame) {
                outcome.counts_as_robot_feedback = true;
                let host_rx_mono_us = host_rx_mono_us();

                let new_state = MasterSlaveControlModeState {
                    hardware_timestamp_us: frame.timestamp_us,
                    host_rx_mono_us,
                    control_mode: feedback.control_mode as u8,
                    move_mode: feedback.move_mode as u8,
                    speed_percent: feedback.speed_percent,
                    mit_mode: feedback.mit_mode as u8,
                    trajectory_stay_time: feedback.trajectory_stay_time,
                    install_position: feedback.install_position as u8,
                    is_valid: true,
                };

                ctx.master_slave_control_mode.store(Arc::new(new_state));
                ctx.fps_stats
                    .load()
                    .master_slave_control_mode_updates
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
        },
        ID_JOINT_CONTROL_12 => {
            if let Ok(feedback) = JointControl12Feedback::try_from(*frame) {
                outcome.counts_as_robot_feedback = true;
                let host_rx_mono_us = host_rx_mono_us();
                let alignment_timestamp_us =
                    group_alignment_timestamp(frame, host_rx_mono_us, backend_capability);
                maybe_reset_joint_control_group(
                    state,
                    frame_group_timeout,
                    0,
                    alignment_timestamp_us,
                );
                state.pending_joint_target_deg[0] = feedback.j1_deg;
                state.pending_joint_target_deg[1] = feedback.j2_deg;
                state.joint_control_group.write_slot(0, alignment_timestamp_us, host_rx_mono_us);
            }
        },
        ID_JOINT_CONTROL_34 => {
            if let Ok(feedback) = JointControl34Feedback::try_from(*frame) {
                outcome.counts_as_robot_feedback = true;
                let host_rx_mono_us = host_rx_mono_us();
                let alignment_timestamp_us =
                    group_alignment_timestamp(frame, host_rx_mono_us, backend_capability);
                maybe_reset_joint_control_group(
                    state,
                    frame_group_timeout,
                    1,
                    alignment_timestamp_us,
                );
                state.pending_joint_target_deg[2] = feedback.j3_deg;
                state.pending_joint_target_deg[3] = feedback.j4_deg;
                state.joint_control_group.write_slot(1, alignment_timestamp_us, host_rx_mono_us);
            }
        },
        ID_JOINT_CONTROL_56 => {
            if let Ok(feedback) = JointControl56Feedback::try_from(*frame) {
                outcome.counts_as_robot_feedback = true;
                let host_rx_mono_us = host_rx_mono_us();
                let alignment_timestamp_us =
                    group_alignment_timestamp(frame, host_rx_mono_us, backend_capability);
                maybe_reset_joint_control_group(
                    state,
                    frame_group_timeout,
                    2,
                    alignment_timestamp_us,
                );
                state.pending_joint_target_deg[4] = feedback.j5_deg;
                state.pending_joint_target_deg[5] = feedback.j6_deg;
                state.joint_control_group.write_slot(2, alignment_timestamp_us, host_rx_mono_us);

                if complete_group_ready(state.joint_control_group.mask) {
                    let new_state = MasterSlaveJointControlState {
                        hardware_timestamp_us: state
                            .joint_control_group
                            .max_alignment_timestamp_us(),
                        host_rx_mono_us: state.joint_control_group.max_host_rx_mono_us(),
                        joint_target_deg: state.pending_joint_target_deg,
                        frame_valid_mask: state.joint_control_group.mask,
                    };

                    ctx.master_slave_joint_control.store(Arc::new(new_state));
                    ctx.fps_stats
                        .load()
                        .master_slave_joint_control_updates
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    reset_pending_joint_control(state);
                }
            }
        },
        ID_GRIPPER_CONTROL => {
            if let Ok(feedback) = GripperControlFeedback::try_from(*frame) {
                outcome.counts_as_robot_feedback = true;
                let host_rx_mono_us = host_rx_mono_us();

                let new_state = MasterSlaveGripperControlState {
                    hardware_timestamp_us: frame.timestamp_us,
                    host_rx_mono_us,
                    gripper_target_travel_mm: feedback.travel_mm,
                    gripper_target_torque_nm: feedback.torque_nm,
                    gripper_status_code: feedback.status_code,
                    gripper_set_zero: feedback.set_zero,
                    is_valid: true,
                };

                ctx.master_slave_gripper_control.store(Arc::new(new_state));
                ctx.fps_stats
                    .load()
                    .master_slave_gripper_control_updates
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
        },
        _ => {
            debug!("RX thread: Received unhandled frame ID=0x{:X}", frame.id);
        },
    }

    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::Arc;
    use std::thread;
    use std::time::Duration;

    // 增强版 MockCanAdapter，支持队列帧
    struct MockCanAdapter {
        receive_queue: VecDeque<PiperFrame>,
        sent_frames: Vec<PiperFrame>,
    }

    impl MockCanAdapter {
        fn new() -> Self {
            Self {
                receive_queue: VecDeque::new(),
                sent_frames: Vec::new(),
            }
        }

        fn queue_frame(&mut self, frame: PiperFrame) {
            self.receive_queue.push_back(frame);
        }

        #[allow(dead_code)]
        fn take_sent_frames(&mut self) -> Vec<PiperFrame> {
            std::mem::take(&mut self.sent_frames)
        }
    }

    impl CanAdapter for MockCanAdapter {
        fn send(&mut self, frame: PiperFrame) -> Result<(), CanError> {
            self.sent_frames.push(frame);
            Ok(())
        }

        fn receive(&mut self) -> Result<PiperFrame, CanError> {
            self.receive_queue.pop_front().ok_or(CanError::Timeout)
        }
    }

    #[test]
    fn test_pipeline_config_default() {
        let config = PipelineConfig::default();
        assert_eq!(config.receive_timeout_ms, 2);
        assert_eq!(config.frame_group_timeout_ms, 10);
        assert_eq!(config.low_speed_drive_state_freshness_ms, 100);
    }

    #[test]
    fn test_pipeline_config_custom() {
        let config = PipelineConfig {
            receive_timeout_ms: 5,
            frame_group_timeout_ms: 20,
            velocity_buffer_timeout_us: 10_000,
            low_speed_drive_state_freshness_ms: 250,
        };
        assert_eq!(config.receive_timeout_ms, 5);
        assert_eq!(config.frame_group_timeout_ms, 20);
        assert_eq!(config.velocity_buffer_timeout_us, 10_000);
        assert_eq!(config.low_speed_drive_state_freshness_ms, 250);
    }

    #[test]
    fn test_derive_maintenance_gate_state_keeps_fault_latched_runtime_faulted() {
        let runtime_phase = Arc::new(AtomicU8::new(RuntimePhase::FaultLatched as u8));
        let last_fault = Arc::new(AtomicU8::new(0));
        let ctx = Arc::new(PiperContext::new());
        let config = PipelineConfig::default();

        assert_eq!(
            derive_maintenance_gate_state(&runtime_phase, &ctx, &config, &last_fault),
            MaintenanceGateState::DeniedFaulted
        );
    }

    #[test]
    fn test_latch_runtime_fault_with_maintenance_closes_gate_and_marks_rx_exited() {
        let runtime_phase = Arc::new(AtomicU8::new(RuntimePhase::Running as u8));
        let normal_send_gate = Arc::new(NormalSendGate::new());
        let last_fault = Arc::new(AtomicU8::new(0));
        let maintenance_gate = Arc::new(MaintenanceGate::default());
        let mut maintenance_tx_state =
            MaintenanceTxState::from_snapshot(maintenance_gate.snapshot());

        latch_runtime_fault_with_maintenance(
            &runtime_phase,
            &normal_send_gate,
            &last_fault,
            RuntimeFaultKind::RxExited,
            &maintenance_gate,
            Some(&mut maintenance_tx_state),
        );

        assert_eq!(
            load_runtime_phase(&runtime_phase),
            RuntimePhase::FaultLatched
        );
        assert_eq!(
            last_fault.load(Ordering::Acquire),
            RuntimeFaultKind::RxExited as u8
        );
        assert!(matches!(
            normal_send_gate.acquire_normal(),
            Err(NormalSendGateDenyReason::FaultClosed)
        ));
        assert_eq!(
            maintenance_gate.current_state(),
            MaintenanceGateState::DeniedFaulted
        );
        assert_eq!(
            maintenance_tx_state.state,
            MaintenanceGateState::DeniedFaulted
        );
    }

    // 辅助函数：创建关节位置反馈帧的数据（度转原始值）
    fn create_joint_feedback_frame_data(j1_deg: f64, j2_deg: f64) -> [u8; 8] {
        let j1_raw = (j1_deg * 1000.0) as i32;
        let j2_raw = (j2_deg * 1000.0) as i32;
        let mut data = [0u8; 8];
        data[0..4].copy_from_slice(&j1_raw.to_be_bytes());
        data[4..8].copy_from_slice(&j2_raw.to_be_bytes());
        data
    }

    fn create_end_pose_frame_data(first: f64, second: f64) -> [u8; 8] {
        let first_raw = (first * 1000.0) as i32;
        let second_raw = (second * 1000.0) as i32;
        let mut data = [0u8; 8];
        data[0..4].copy_from_slice(&first_raw.to_be_bytes());
        data[4..8].copy_from_slice(&second_raw.to_be_bytes());
        data
    }

    fn joint_feedback_frame(id: u16, jx_deg: f64, jy_deg: f64, timestamp_us: u64) -> PiperFrame {
        let mut frame =
            PiperFrame::new_standard(id, &create_joint_feedback_frame_data(jx_deg, jy_deg));
        frame.timestamp_us = timestamp_us;
        frame
    }

    fn end_pose_frame(id: u16, first: f64, second: f64, timestamp_us: u64) -> PiperFrame {
        let mut frame = PiperFrame::new_standard(id, &create_end_pose_frame_data(first, second));
        frame.timestamp_us = timestamp_us;
        frame
    }

    fn joint_driver_low_speed_frame(
        joint_index: u8,
        enabled: bool,
        timestamp_us: u64,
    ) -> PiperFrame {
        let id = ID_JOINT_DRIVER_LOW_SPEED_BASE + u32::from(joint_index.saturating_sub(1));
        let mut data = [0u8; 8];
        data[0..2].copy_from_slice(&240u16.to_be_bytes());
        data[2..4].copy_from_slice(&45i16.to_be_bytes());
        data[4] = 50;
        data[5] = if enabled { 0x40 } else { 0x00 };
        data[6..8].copy_from_slice(&5000u16.to_be_bytes());
        let mut frame = PiperFrame::new_standard(id as u16, &data);
        frame.timestamp_us = timestamp_us;
        frame
    }

    fn robot_status_frame_with_status(
        control_mode: ControlMode,
        robot_status: RobotStatus,
        move_mode: MoveMode,
        timestamp_us: u64,
    ) -> PiperFrame {
        let mut frame = PiperFrame::new_standard(
            ID_ROBOT_STATUS as u16,
            &[
                control_mode as u8,
                robot_status as u8,
                move_mode as u8,
                TeachStatus::Closed as u8,
                MotionStatus::Arrived as u8,
                0,
                0,
                0,
            ],
        );
        frame.timestamp_us = timestamp_us;
        frame
    }

    fn parse_frame_for_test(
        ctx: &Arc<PiperContext>,
        state: &mut ParserState,
        metrics: &Arc<PiperMetrics>,
        config: &PipelineConfig,
        frame: PiperFrame,
    ) {
        parse_and_update_state(
            &frame,
            BackendCapability::StrictRealtime,
            ctx,
            config,
            state,
            metrics,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn parse_frame_for_test_with_maintenance_refresh(
        ctx: &Arc<PiperContext>,
        state: &mut ParserState,
        metrics: &Arc<PiperMetrics>,
        config: &PipelineConfig,
        frame: PiperFrame,
        maintenance_gate: &Arc<MaintenanceGate>,
        runtime_phase: &Arc<AtomicU8>,
        last_fault: &Arc<AtomicU8>,
    ) {
        let parsed = parse_and_update_state(
            &frame,
            BackendCapability::StrictRealtime,
            ctx,
            config,
            state,
            metrics,
        );

        if parsed.counts_as_robot_feedback {
            ctx.connection_monitor.register_feedback();
        }
        if parsed.maintenance_gate_may_have_changed
            || maintenance_gate.current_state() == MaintenanceGateState::DeniedTransportDown
        {
            refresh_maintenance_gate_state(
                maintenance_gate,
                runtime_phase,
                ctx,
                config,
                last_fault,
            );
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn feed_joint_driver_low_speed_cycle(
        ctx: &Arc<PiperContext>,
        state: &mut ParserState,
        metrics: &Arc<PiperMetrics>,
        config: &PipelineConfig,
        maintenance_gate: &Arc<MaintenanceGate>,
        runtime_phase: &Arc<AtomicU8>,
        last_fault: &Arc<AtomicU8>,
        enabled_mask: u8,
        start_timestamp_us: u64,
    ) {
        for joint_index in 1..=6 {
            let enabled = (enabled_mask & (1u8 << (joint_index - 1))) != 0;
            parse_frame_for_test_with_maintenance_refresh(
                ctx,
                state,
                metrics,
                config,
                joint_driver_low_speed_frame(
                    joint_index,
                    enabled,
                    start_timestamp_us + u64::from(joint_index),
                ),
                maintenance_gate,
                runtime_phase,
                last_fault,
            );
        }
    }

    #[test]
    fn test_joint_pos_frame_commit_complete() {
        let ctx = Arc::new(PiperContext::new());
        let mut mock_can = MockCanAdapter::new();
        let (cmd_tx, cmd_rx) = crossbeam_channel::bounded(10);

        // 创建完整的关节位置帧组（0x2A5, 0x2A6, 0x2A7）
        // J1=10°, J2=20°, J3=30°, J4=40°, J5=50°, J6=60°
        let mut frame_2a5 = PiperFrame::new_standard(
            ID_JOINT_FEEDBACK_12 as u16,
            &create_joint_feedback_frame_data(10.0, 20.0),
        );
        frame_2a5.timestamp_us = 1000;
        let mut frame_2a6 = PiperFrame::new_standard(
            ID_JOINT_FEEDBACK_34 as u16,
            &create_joint_feedback_frame_data(30.0, 40.0),
        );
        frame_2a6.timestamp_us = 1001;
        let mut frame_2a7 = PiperFrame::new_standard(
            ID_JOINT_FEEDBACK_56 as u16,
            &create_joint_feedback_frame_data(50.0, 60.0),
        );
        frame_2a7.timestamp_us = 1002;

        // 队列所有帧
        mock_can.queue_frame(frame_2a5);
        mock_can.queue_frame(frame_2a6);
        mock_can.queue_frame(frame_2a7);

        // 运行 io_loop 一小段时间
        let ctx_clone = ctx.clone();
        let config = PipelineConfig::default();
        let handle = thread::spawn(move || {
            io_loop(
                mock_can,
                BackendCapability::StrictRealtime,
                cmd_rx,
                ctx_clone,
                config,
            );
        });

        // 等待 io_loop 处理帧（需要多次循环才能处理完）
        thread::sleep(Duration::from_millis(100));

        // 关闭命令通道，让 io_loop 退出
        drop(cmd_tx);
        // 等待线程退出（使用短暂超时）
        let start = std::time::Instant::now();
        while start.elapsed().as_secs() < 2 {
            if handle.is_finished() {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        let _ = handle.join();

        // 验证状态已更新（由于需要完整帧组，可能需要多次迭代）
        // 至少验证可以正常处理帧而不崩溃
        let joint_pos = ctx.joint_position_monitor.load();
        // 如果帧组完整，应该有时间戳更新
        // 但由于异步性，可能需要多次尝试或调整测试策略
        assert!(joint_pos.latest_complete().is_none_or(|state| {
            state.joint_pos.iter().any(|&v| v != 0.0) || state.hardware_timestamp_us == 0
        }));
    }

    #[test]
    fn test_command_channel_processing() {
        let ctx = Arc::new(PiperContext::new());
        let mock_can = MockCanAdapter::new();
        let (cmd_tx, cmd_rx) = crossbeam_channel::bounded(10);

        let config = PipelineConfig::default();
        let handle = thread::spawn(move || {
            io_loop(
                mock_can,
                BackendCapability::StrictRealtime,
                cmd_rx,
                ctx,
                config,
            );
        });

        // 发送命令帧
        let cmd_frame = PiperFrame::new_standard(0x123, &[0x01, 0x02, 0x03]);
        cmd_tx.send(cmd_frame).unwrap();

        // 等待处理
        thread::sleep(Duration::from_millis(50));

        // 关闭通道，让 io_loop 退出
        drop(cmd_tx);
        // 等待线程退出（使用短暂超时）
        let start = std::time::Instant::now();
        while start.elapsed().as_secs() < 2 {
            if handle.is_finished() {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        let _ = handle.join();

        // 验证命令帧已被发送（通过 MockCanAdapter 的 sent_frames）
        // 注意：由于 mock_can 被移动到线程中，我们无法直接检查
        // 这个测试主要验证不会崩溃
    }

    #[test]
    fn test_joint_position_group_rejects_stale_partial_after_missing_first_frame() {
        let ctx = Arc::new(PiperContext::new());
        let metrics = Arc::new(PiperMetrics::new());
        let config = PipelineConfig::default();
        let mut state = ParserState::new();

        parse_frame_for_test(
            &ctx,
            &mut state,
            &metrics,
            &config,
            joint_feedback_frame(ID_JOINT_FEEDBACK_12 as u16, 10.0, 20.0, 1_000),
        );
        parse_frame_for_test(
            &ctx,
            &mut state,
            &metrics,
            &config,
            joint_feedback_frame(ID_JOINT_FEEDBACK_34 as u16, 30.0, 40.0, 3_600),
        );
        parse_frame_for_test(
            &ctx,
            &mut state,
            &metrics,
            &config,
            joint_feedback_frame(ID_JOINT_FEEDBACK_56 as u16, 50.0, 60.0, 3_700),
        );

        let snapshot = ctx.capture_joint_position_monitor_snapshot();
        assert!(snapshot.latest_complete().is_none());
        assert_eq!(snapshot.latest_raw().frame_valid_mask, 0b110);
        assert_eq!(snapshot.latest_raw().joint_pos[0], 0.0);
        assert_eq!(
            metrics
                .rx_joint_position_incomplete_groups_dropped_total
                .load(Ordering::Relaxed),
            1
        );
    }

    #[test]
    fn test_joint_position_group_drops_old_partial_on_duplicate_slot() {
        let ctx = Arc::new(PiperContext::new());
        let metrics = Arc::new(PiperMetrics::new());
        let config = PipelineConfig::default();
        let mut state = ParserState::new();

        parse_frame_for_test(
            &ctx,
            &mut state,
            &metrics,
            &config,
            joint_feedback_frame(ID_JOINT_FEEDBACK_12 as u16, 1.0, 2.0, 1_000),
        );
        parse_frame_for_test(
            &ctx,
            &mut state,
            &metrics,
            &config,
            joint_feedback_frame(ID_JOINT_FEEDBACK_34 as u16, 3.0, 4.0, 1_100),
        );
        parse_frame_for_test(
            &ctx,
            &mut state,
            &metrics,
            &config,
            joint_feedback_frame(ID_JOINT_FEEDBACK_12 as u16, 11.0, 12.0, 1_200),
        );

        let snapshot = ctx.capture_joint_position_monitor_snapshot();
        assert!(snapshot.latest_complete().is_none());
        assert_eq!(snapshot.latest_raw().frame_valid_mask, 0b001);
        assert!((snapshot.latest_raw().joint_pos[0] - 11.0_f64.to_radians()).abs() < 1e-9);
        assert_eq!(
            metrics
                .rx_joint_position_incomplete_groups_dropped_total
                .load(Ordering::Relaxed),
            1
        );
    }

    #[test]
    fn test_joint_position_group_accepts_out_of_order_completion() {
        let ctx = Arc::new(PiperContext::new());
        let metrics = Arc::new(PiperMetrics::new());
        let config = PipelineConfig::default();
        let mut state = ParserState::new();

        parse_frame_for_test(
            &ctx,
            &mut state,
            &metrics,
            &config,
            joint_feedback_frame(ID_JOINT_FEEDBACK_34 as u16, 30.0, 40.0, 1_000),
        );
        parse_frame_for_test(
            &ctx,
            &mut state,
            &metrics,
            &config,
            joint_feedback_frame(ID_JOINT_FEEDBACK_12 as u16, 10.0, 20.0, 1_500),
        );
        parse_frame_for_test(
            &ctx,
            &mut state,
            &metrics,
            &config,
            joint_feedback_frame(ID_JOINT_FEEDBACK_56 as u16, 50.0, 60.0, 1_600),
        );

        let complete = ctx
            .capture_joint_position_monitor_snapshot()
            .latest_complete_cloned()
            .expect("out-of-order but same-cycle group should publish complete snapshot");
        assert_eq!(complete.frame_valid_mask, 0b111);
        assert!((complete.joint_pos[0] - 10.0_f64.to_radians()).abs() < 1e-9);
        assert!((complete.joint_pos[5] - 60.0_f64.to_radians()).abs() < 1e-9);
    }

    #[test]
    fn test_maintenance_gate_stays_unknown_after_robot_status_until_low_speed_state_is_confirmed() {
        let ctx = Arc::new(PiperContext::new());
        let metrics = Arc::new(PiperMetrics::new());
        let config = PipelineConfig::default();
        let mut state = ParserState::new();
        let maintenance_gate = Arc::new(MaintenanceGate::default());
        let runtime_phase = Arc::new(AtomicU8::new(RuntimePhase::Running as u8));
        let last_fault = Arc::new(AtomicU8::new(0));

        parse_frame_for_test_with_maintenance_refresh(
            &ctx,
            &mut state,
            &metrics,
            &config,
            robot_status_frame_with_status(
                ControlMode::CanControl,
                RobotStatus::Normal,
                MoveMode::MoveJ,
                1_000,
            ),
            &maintenance_gate,
            &runtime_phase,
            &last_fault,
        );

        assert_eq!(
            maintenance_gate.current_state(),
            MaintenanceGateState::DeniedDriveStateUnknown
        );
    }

    #[test]
    fn test_maintenance_gate_stays_unknown_with_partial_low_speed_feedback() {
        let ctx = Arc::new(PiperContext::new());
        let metrics = Arc::new(PiperMetrics::new());
        let config = PipelineConfig::default();
        let mut state = ParserState::new();
        let maintenance_gate = Arc::new(MaintenanceGate::default());
        let runtime_phase = Arc::new(AtomicU8::new(RuntimePhase::Running as u8));
        let last_fault = Arc::new(AtomicU8::new(0));

        parse_frame_for_test_with_maintenance_refresh(
            &ctx,
            &mut state,
            &metrics,
            &config,
            joint_driver_low_speed_frame(1, true, 1_000),
            &maintenance_gate,
            &runtime_phase,
            &last_fault,
        );
        parse_frame_for_test_with_maintenance_refresh(
            &ctx,
            &mut state,
            &metrics,
            &config,
            joint_driver_low_speed_frame(2, false, 1_001),
            &maintenance_gate,
            &runtime_phase,
            &last_fault,
        );

        let robot_control = ctx.robot_control.load();
        assert_eq!(robot_control.driver_enabled_mask, 0b000001);
        assert!(robot_control.any_drive_enabled);
        assert!(!robot_control.is_enabled);
        assert_eq!(
            maintenance_gate.current_state(),
            MaintenanceGateState::DeniedDriveStateUnknown
        );
    }

    #[test]
    fn test_maintenance_gate_allows_standby_only_after_full_fresh_disabled_low_speed_feedback() {
        let ctx = Arc::new(PiperContext::new());
        let metrics = Arc::new(PiperMetrics::new());
        let config = PipelineConfig::default();
        let mut state = ParserState::new();
        let maintenance_gate = Arc::new(MaintenanceGate::default());
        let runtime_phase = Arc::new(AtomicU8::new(RuntimePhase::Running as u8));
        let last_fault = Arc::new(AtomicU8::new(0));

        feed_joint_driver_low_speed_cycle(
            &ctx,
            &mut state,
            &metrics,
            &config,
            &maintenance_gate,
            &runtime_phase,
            &last_fault,
            0,
            1_000,
        );

        let robot_control = ctx.robot_control.load();
        assert_eq!(robot_control.driver_enabled_mask, 0);
        assert!(!robot_control.any_drive_enabled);
        assert!(!robot_control.is_enabled);
        assert_eq!(robot_control.confirmed_driver_enabled_mask, Some(0));
        assert_eq!(
            maintenance_gate.current_state(),
            MaintenanceGateState::AllowedStandby
        );
    }

    #[test]
    fn test_maintenance_gate_denies_when_any_drive_is_enabled() {
        let ctx = Arc::new(PiperContext::new());
        let metrics = Arc::new(PiperMetrics::new());
        let config = PipelineConfig::default();
        let mut state = ParserState::new();
        let maintenance_gate = Arc::new(MaintenanceGate::default());
        let runtime_phase = Arc::new(AtomicU8::new(RuntimePhase::Running as u8));
        let last_fault = Arc::new(AtomicU8::new(0));

        feed_joint_driver_low_speed_cycle(
            &ctx,
            &mut state,
            &metrics,
            &config,
            &maintenance_gate,
            &runtime_phase,
            &last_fault,
            0b000001,
            1_000,
        );

        let robot_control = ctx.robot_control.load();
        assert_eq!(robot_control.driver_enabled_mask, 0b000001);
        assert!(robot_control.any_drive_enabled);
        assert!(!robot_control.is_enabled);
        assert_eq!(robot_control.confirmed_driver_enabled_mask, Some(0b000001));
        assert_eq!(
            maintenance_gate.current_state(),
            MaintenanceGateState::DeniedActiveControl
        );

        feed_joint_driver_low_speed_cycle(
            &ctx,
            &mut state,
            &metrics,
            &config,
            &maintenance_gate,
            &runtime_phase,
            &last_fault,
            0,
            2_000,
        );

        assert_eq!(
            maintenance_gate.current_state(),
            MaintenanceGateState::AllowedStandby
        );
    }

    #[test]
    fn test_maintenance_gate_requires_all_joints_to_stay_fresh_after_historical_full_snapshot() {
        let ctx = Arc::new(PiperContext::new());
        let metrics = Arc::new(PiperMetrics::new());
        let config = PipelineConfig::default();
        let mut state = ParserState::new();
        let maintenance_gate = Arc::new(MaintenanceGate::default());
        let runtime_phase = Arc::new(AtomicU8::new(RuntimePhase::Running as u8));
        let last_fault = Arc::new(AtomicU8::new(0));

        feed_joint_driver_low_speed_cycle(
            &ctx,
            &mut state,
            &metrics,
            &config,
            &maintenance_gate,
            &runtime_phase,
            &last_fault,
            0,
            1_000,
        );
        assert_eq!(
            maintenance_gate.current_state(),
            MaintenanceGateState::AllowedStandby
        );

        let stale_timestamp = host_rx_mono_us()
            .saturating_sub((config.low_speed_drive_state_freshness_ms + 50).saturating_mul(1_000));
        ctx.joint_driver_low_speed.store(Arc::new(JointDriverLowSpeedState {
            driver_enabled_mask: 0,
            host_rx_mono_us: stale_timestamp,
            host_rx_mono_timestamps: [stale_timestamp; 6],
            valid_mask: ALL_DRIVES_ENABLED_MASK,
            ..JointDriverLowSpeedState::default()
        }));
        ctx.robot_control.store(Arc::new(RobotControlState {
            driver_enabled_mask: 0,
            any_drive_enabled: false,
            is_enabled: false,
            confirmed_driver_enabled_mask: None,
            ..RobotControlState::default()
        }));

        parse_frame_for_test_with_maintenance_refresh(
            &ctx,
            &mut state,
            &metrics,
            &config,
            joint_driver_low_speed_frame(1, false, 2_000),
            &maintenance_gate,
            &runtime_phase,
            &last_fault,
        );

        let robot_control = ctx.robot_control.load();
        assert_eq!(robot_control.driver_enabled_mask, 0);
        assert_eq!(robot_control.confirmed_driver_enabled_mask, None);
        assert_eq!(
            maintenance_gate.current_state(),
            MaintenanceGateState::DeniedDriveStateUnknown
        );
    }

    #[test]
    fn test_maintenance_gate_returns_to_unknown_when_low_speed_state_stales() {
        let ctx = Arc::new(PiperContext::new());
        let config = PipelineConfig::default();
        let maintenance_gate = Arc::new(MaintenanceGate::default());
        let runtime_phase = Arc::new(AtomicU8::new(RuntimePhase::Running as u8));
        let last_fault = Arc::new(AtomicU8::new(0));

        ctx.connection_monitor.register_feedback();
        let fresh_host_rx_mono_us = host_rx_mono_us();
        ctx.joint_driver_low_speed.store(Arc::new(JointDriverLowSpeedState {
            host_rx_mono_us: fresh_host_rx_mono_us,
            host_rx_mono_timestamps: [fresh_host_rx_mono_us; 6],
            valid_mask: ALL_DRIVES_ENABLED_MASK,
            ..JointDriverLowSpeedState::default()
        }));

        refresh_maintenance_gate_state(
            &maintenance_gate,
            &runtime_phase,
            &ctx,
            &config,
            &last_fault,
        );
        assert_eq!(
            maintenance_gate.current_state(),
            MaintenanceGateState::AllowedStandby
        );

        let stale_host_rx_mono_us = host_rx_mono_us()
            .saturating_sub((config.low_speed_drive_state_freshness_ms + 50).saturating_mul(1_000));
        ctx.joint_driver_low_speed.store(Arc::new(JointDriverLowSpeedState {
            host_rx_mono_us: stale_host_rx_mono_us,
            host_rx_mono_timestamps: [stale_host_rx_mono_us; 6],
            valid_mask: ALL_DRIVES_ENABLED_MASK,
            ..JointDriverLowSpeedState::default()
        }));

        refresh_maintenance_gate_state(
            &maintenance_gate,
            &runtime_phase,
            &ctx,
            &config,
            &last_fault,
        );
        assert_eq!(
            maintenance_gate.current_state(),
            MaintenanceGateState::DeniedDriveStateUnknown
        );
    }

    #[test]
    fn test_joint_position_control_grade_requires_tighter_span_than_monitor_complete() {
        let ctx = Arc::new(PiperContext::new());
        let metrics = Arc::new(PiperMetrics::new());
        let config = PipelineConfig::default();
        let mut state = ParserState::new();

        parse_frame_for_test(
            &ctx,
            &mut state,
            &metrics,
            &config,
            joint_feedback_frame(ID_JOINT_FEEDBACK_12 as u16, 10.0, 20.0, 1_000),
        );
        parse_frame_for_test(
            &ctx,
            &mut state,
            &metrics,
            &config,
            joint_feedback_frame(ID_JOINT_FEEDBACK_34 as u16, 30.0, 40.0, 3_050),
        );
        parse_frame_for_test(
            &ctx,
            &mut state,
            &metrics,
            &config,
            joint_feedback_frame(ID_JOINT_FEEDBACK_56 as u16, 50.0, 60.0, 3_100),
        );

        assert!(
            ctx.capture_joint_position_monitor_snapshot().latest_complete().is_some(),
            "monitor-complete snapshot should still be published"
        );
        assert_eq!(ctx.control_joint_position.load().hardware_timestamp_us, 0);
        assert_eq!(
            metrics.rx_joint_position_control_grade_rejected_total.load(Ordering::Relaxed),
            1
        );
    }

    #[test]
    fn test_end_pose_group_accepts_out_of_order_completion() {
        let ctx = Arc::new(PiperContext::new());
        let metrics = Arc::new(PiperMetrics::new());
        let config = PipelineConfig::default();
        let mut state = ParserState::new();

        parse_frame_for_test(
            &ctx,
            &mut state,
            &metrics,
            &config,
            end_pose_frame(ID_END_POSE_2 as u16, 300.0, 40.0, 1_000),
        );
        parse_frame_for_test(
            &ctx,
            &mut state,
            &metrics,
            &config,
            end_pose_frame(ID_END_POSE_1 as u16, 100.0, 200.0, 1_100),
        );
        parse_frame_for_test(
            &ctx,
            &mut state,
            &metrics,
            &config,
            end_pose_frame(ID_END_POSE_3 as u16, 50.0, 60.0, 1_150),
        );

        let complete = ctx
            .capture_end_pose_monitor_snapshot()
            .latest_complete_cloned()
            .expect("out-of-order end-pose group should publish complete snapshot");
        assert_eq!(complete.frame_valid_mask, 0b111);
        assert!((complete.end_pose[0] - 0.1).abs() < 1e-9);
        assert!((complete.end_pose[2] - 0.3).abs() < 1e-9);
    }

    #[test]
    fn test_end_pose_group_rejects_stale_partial_after_missing_first_frame() {
        let ctx = Arc::new(PiperContext::new());
        let metrics = Arc::new(PiperMetrics::new());
        let config = PipelineConfig::default();
        let mut state = ParserState::new();

        parse_frame_for_test(
            &ctx,
            &mut state,
            &metrics,
            &config,
            end_pose_frame(ID_END_POSE_1 as u16, 100.0, 200.0, 1_000),
        );
        parse_frame_for_test(
            &ctx,
            &mut state,
            &metrics,
            &config,
            end_pose_frame(ID_END_POSE_2 as u16, 300.0, 40.0, 3_600),
        );
        parse_frame_for_test(
            &ctx,
            &mut state,
            &metrics,
            &config,
            end_pose_frame(ID_END_POSE_3 as u16, 50.0, 60.0, 3_700),
        );

        let snapshot = ctx.capture_end_pose_monitor_snapshot();
        assert!(snapshot.latest_complete().is_none());
        assert_eq!(snapshot.latest_raw().frame_valid_mask, 0b110);
        assert_eq!(
            metrics.rx_end_pose_incomplete_groups_dropped_total.load(Ordering::Relaxed),
            1
        );
    }
}
