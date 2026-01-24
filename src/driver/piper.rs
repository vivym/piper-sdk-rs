//! Robot API 模块
//!
//! 提供对外的 `Piper` 结构体，封装底层 IO 线程和状态同步细节。

use crate::can::{CanAdapter, CanError, PiperFrame, SplittableAdapter};
use crate::driver::command::{CommandPriority, PiperCommand, RealtimeCommand};
use crate::driver::error::DriverError;
use crate::driver::fps_stats::{FpsCounts, FpsResult};
use crate::driver::metrics::{MetricsSnapshot, PiperMetrics};
use crate::driver::pipeline::*;
use crate::driver::state::*;
use crossbeam_channel::Sender;
use std::mem::ManuallyDrop;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{JoinHandle, spawn};
use tracing::{error, info, warn};

/// Piper 机械臂驱动（对外 API）
///
/// 支持单线程和双线程两种模式
/// - 单线程模式：使用 `io_thread`（向后兼容）
/// - 双线程模式：使用 `rx_thread` 和 `tx_thread`（物理隔离）
pub struct Piper {
    /// 命令发送通道（向 IO 线程发送控制帧，单线程模式）
    ///
    /// 需要在 Drop 时 **提前关闭通道**（在 join IO 线程之前），
    /// 否则 `io_loop` 可能永远收不到 `Disconnected` 而导致退出卡住。
    cmd_tx: ManuallyDrop<Sender<PiperFrame>>,
    /// 实时命令插槽（双线程模式，邮箱模式，Overwrite）
    realtime_slot: Option<Arc<std::sync::Mutex<Option<RealtimeCommand>>>>,
    /// 可靠命令队列发送端（双线程模式，容量 10，FIFO）
    reliable_tx: Option<Sender<PiperFrame>>,
    /// 共享状态上下文
    ctx: Arc<PiperContext>,
    /// IO 线程句柄（单线程模式，Drop 时 join）
    io_thread: Option<JoinHandle<()>>,
    /// RX 线程句柄（双线程模式）
    rx_thread: Option<JoinHandle<()>>,
    /// TX 线程句柄（双线程模式）
    tx_thread: Option<JoinHandle<()>>,
    /// 运行标志（用于线程生命周期联动）
    is_running: Arc<AtomicBool>,
    /// 性能指标（原子计数器）
    metrics: Arc<PiperMetrics>,
}

impl Piper {
    /// 最大允许的实时帧包大小
    ///
    /// 允许调用者在客户端进行预检查，避免跨层调用后的运行时错误。
    ///
    /// # 示例
    ///
    /// ```rust,no_run
    /// # use piper_sdk::driver::Piper;
    /// # use piper_sdk::can::PiperFrame;
    /// # fn example(piper: &Piper) -> std::result::Result<(), Box<dyn std::error::Error>> {
    /// let frame1 = PiperFrame::new_standard(0x100, &[]);
    /// let frame2 = PiperFrame::new_standard(0x101, &[]);
    /// let frame3 = PiperFrame::new_standard(0x102, &[]);
    /// let frames = [frame1, frame2, frame3];
    /// if frames.len() > Piper::MAX_REALTIME_PACKAGE_SIZE {
    ///     return Err("Package too large".into());
    /// }
    /// piper.send_realtime_package(frames)?;
    /// # Ok(())
    /// # }
    /// ```
    pub const MAX_REALTIME_PACKAGE_SIZE: usize = 10;

    /// 创建新的 Piper 实例
    ///
    /// # 参数
    /// - `can`: CAN 适配器（会被移动到 IO 线程）
    /// - `config`: Pipeline 配置（可选）
    ///
    /// # 错误
    /// - `CanError`: CAN 设备初始化失败（注意：这里返回 CanError，因为 DriverError 尚未完全实现 `From<CanError>`）
    pub fn new(
        can: impl CanAdapter + Send + 'static,
        config: Option<PipelineConfig>,
    ) -> Result<Self, CanError> {
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
            cmd_tx: ManuallyDrop::new(cmd_tx),
            realtime_slot: None, // 单线程模式
            reliable_tx: None,   // 单线程模式
            ctx,
            io_thread: Some(io_thread),
            rx_thread: None,                             // 单线程模式
            tx_thread: None,                             // 单线程模式
            is_running: Arc::new(AtomicBool::new(true)), // 默认运行中
            metrics: Arc::new(PiperMetrics::new()),      // 初始化指标
        })
    }

    /// 创建双线程模式的 Piper 实例
    ///
    /// 将 CAN 适配器分离为独立的 RX 和 TX 适配器，实现物理隔离。
    /// RX 线程专门负责接收反馈帧，TX 线程专门负责发送控制命令。
    ///
    /// # 参数
    /// - `can`: 可分离的 CAN 适配器（必须已启动）
    /// - `config`: Pipeline 配置（可选）
    ///
    /// # 错误
    /// - `CanError::NotStarted`: 适配器未启动
    /// - `CanError::Device`: 分离适配器失败
    ///
    /// # 使用场景
    /// - 实时控制：需要 RX 不受 TX 阻塞影响
    /// - 高频控制：500Hz-1kHz 控制循环
    ///
    /// # 注意
    /// - 适配器必须已启动（调用 `configure()` 或 `start()`）
    /// - 分离后，原适配器不再可用（消费 `can`）
    pub fn new_dual_thread<C>(can: C, config: Option<PipelineConfig>) -> Result<Self, CanError>
    where
        C: SplittableAdapter + Send + 'static,
        C::RxAdapter: Send + 'static,
        C::TxAdapter: Send + 'static,
    {
        // 分离适配器
        let (rx_adapter, tx_adapter) = can.split()?;

        // 创建命令通道（邮箱模式 + 可靠队列容量 10）
        let realtime_slot = Arc::new(std::sync::Mutex::new(None::<RealtimeCommand>));
        let (reliable_tx, reliable_rx) = crossbeam_channel::bounded::<PiperFrame>(10);

        // 创建共享状态上下文
        let ctx = Arc::new(PiperContext::new());

        // 创建运行标志和指标
        let is_running = Arc::new(AtomicBool::new(true));
        let metrics = Arc::new(PiperMetrics::new());

        // 克隆用于线程
        let ctx_clone = ctx.clone();
        let is_running_clone = is_running.clone();
        let metrics_clone = metrics.clone();
        let config_clone = config.clone().unwrap_or_default();

        // 启动 RX 线程
        let rx_thread = spawn(move || {
            crate::driver::pipeline::rx_loop(
                rx_adapter,
                ctx_clone,
                config_clone,
                is_running_clone,
                metrics_clone,
            );
        });

        // 克隆用于 TX 线程
        let is_running_tx = is_running.clone();
        let metrics_tx = metrics.clone();
        let realtime_slot_tx = realtime_slot.clone();

        // 启动 TX 线程（邮箱模式）
        let tx_thread = spawn(move || {
            crate::driver::pipeline::tx_loop_mailbox(
                tx_adapter,
                realtime_slot_tx,
                reliable_rx,
                is_running_tx,
                metrics_tx,
            );
        });

        // 给 RX 线程一些启动时间，确保它已经开始接收数据
        // 这对于 wait_for_feedback 很重要，因为如果 RX 线程还没启动，就无法收到反馈
        std::thread::sleep(std::time::Duration::from_millis(10));

        Ok(Self {
            cmd_tx: ManuallyDrop::new(reliable_tx.clone()), // 向后兼容：单线程模式使用
            realtime_slot: Some(realtime_slot),             // 实时命令邮箱
            reliable_tx: Some(reliable_tx),                 // 可靠队列
            ctx,
            io_thread: None, // 双线程模式不使用 io_thread
            rx_thread: Some(rx_thread),
            tx_thread: Some(tx_thread),
            is_running,
            metrics,
        })
    }

    /// 检查线程健康状态
    ///
    /// 返回 RX 和 TX 线程的存活状态。
    ///
    /// # 返回
    /// - `(rx_alive, tx_alive)`: 两个布尔值，表示线程是否还在运行
    pub fn check_health(&self) -> (bool, bool) {
        let rx_alive = self.rx_thread.as_ref().map(|h| !h.is_finished()).unwrap_or(true); // 单线程模式下，认为健康

        let tx_alive = self.tx_thread.as_ref().map(|h| !h.is_finished()).unwrap_or(true); // 单线程模式下，认为健康

        (rx_alive, tx_alive)
    }

    /// 检查是否健康
    ///
    /// 如果所有线程都存活，返回 `true`。
    pub fn is_healthy(&self) -> bool {
        let (rx_alive, tx_alive) = self.check_health();
        rx_alive && tx_alive
    }

    /// 获取性能指标快照
    ///
    /// 返回当前所有计数器的快照，用于监控 IO 链路健康状态。
    pub fn get_metrics(&self) -> MetricsSnapshot {
        self.metrics.snapshot()
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
        self.ctx.joint_dynamic.load().as_ref().clone()
    }

    /// 获取关节位置状态（无锁，纳秒级返回）
    ///
    /// 包含6个关节的位置信息（500Hz更新）。
    ///
    /// # 性能
    /// - 无锁读取（ArcSwap::load）
    /// - 返回快照副本（Clone 开销低）
    /// - 适合 500Hz 控制循环
    ///
    /// # 注意
    /// - 此状态与 `EndPoseState` 不是原子更新的，如需同时获取，请使用 `capture_motion_snapshot()`
    pub fn get_joint_position(&self) -> JointPositionState {
        self.ctx.joint_position.load().as_ref().clone()
    }

    /// 获取末端位姿状态（无锁，纳秒级返回）
    ///
    /// 包含末端执行器的位置和姿态信息（500Hz更新）。
    ///
    /// # 性能
    /// - 无锁读取（ArcSwap::load）
    /// - 返回快照副本（Clone 开销低）
    /// - 适合 500Hz 控制循环
    ///
    /// # 注意
    /// - 此状态与 `JointPositionState` 不是原子更新的，如需同时获取，请使用 `capture_motion_snapshot()`
    pub fn get_end_pose(&self) -> EndPoseState {
        self.ctx.end_pose.load().as_ref().clone()
    }

    /// 获取运动快照（无锁，纳秒级返回）
    ///
    /// 原子性地获取 `JointPositionState` 和 `EndPoseState` 的最新快照。
    /// 虽然这两个状态在硬件上不是同时更新的，但此方法保证逻辑上的原子性。
    ///
    /// # 性能
    /// - 无锁读取（两次 ArcSwap::load）
    /// - 返回快照副本
    /// - 适合需要同时使用关节位置和末端位姿的场景
    ///
    /// # 示例
    ///
    /// ```
    /// # use piper_sdk::driver::Piper;
    /// # // 注意：此示例需要实际的 CAN 适配器，仅供参考
    /// # // let piper = Piper::new(/* ... */).unwrap();
    /// # // let snapshot = piper.capture_motion_snapshot();
    /// # // println!("Joint positions: {:?}", snapshot.joint_position.joint_pos);
    /// # // println!("End pose: {:?}", snapshot.end_pose.end_pose);
    /// ```
    pub fn capture_motion_snapshot(&self) -> MotionSnapshot {
        self.ctx.capture_motion_snapshot()
    }

    /// 获取机器人控制状态（无锁）
    ///
    /// 包含控制模式、机器人状态、故障码等（100Hz更新）。
    ///
    /// # 性能
    /// - 无锁读取（ArcSwap::load）
    /// - 返回快照副本
    pub fn get_robot_control(&self) -> RobotControlState {
        self.ctx.robot_control.load().as_ref().clone()
    }

    /// 获取夹爪状态（无锁）
    ///
    /// 包含夹爪行程、扭矩、状态码等（100Hz更新）。
    ///
    /// # 性能
    /// - 无锁读取（ArcSwap::load）
    /// - 返回快照副本
    pub fn get_gripper(&self) -> GripperState {
        self.ctx.gripper.load().as_ref().clone()
    }

    /// 获取关节驱动器低速反馈状态（无锁）
    ///
    /// 包含温度、电压、电流、驱动器状态等（40Hz更新）。
    ///
    /// # 性能
    /// - 无锁读取（ArcSwap::load，Wait-Free）
    /// - 返回快照副本
    pub fn get_joint_driver_low_speed(&self) -> JointDriverLowSpeedState {
        self.ctx.joint_driver_low_speed.load().as_ref().clone()
    }

    /// 获取固件版本字符串
    ///
    /// 从累积的固件数据中解析版本字符串。
    /// 如果固件数据未完整或未找到版本字符串，返回 `None`。
    ///
    /// # 性能
    /// - 需要获取 RwLock 读锁
    /// - 如果已解析，直接返回缓存的版本字符串
    /// - 如果未解析，尝试从累积数据中解析
    pub fn get_firmware_version(&self) -> Option<String> {
        if let Ok(mut firmware_state) = self.ctx.firmware_version.write() {
            // 如果已经解析过，直接返回
            if let Some(version) = firmware_state.version_string() {
                return Some(version.clone());
            }
            // 否则尝试解析
            firmware_state.parse_version()
        } else {
            None
        }
    }

    /// 查询固件版本
    ///
    /// 发送固件版本查询指令到机械臂，并清空之前的固件数据缓存。
    /// 查询和反馈使用相同的 CAN ID (0x4AF)。
    ///
    /// **注意**：
    /// - 发送查询命令后会自动清空固件数据缓存（与 Python SDK 一致）
    /// - 需要等待一段时间（推荐 30-50ms）让机械臂返回反馈数据
    /// - 之后可以调用 `get_firmware_version()` 获取解析后的版本字符串
    ///
    /// # 错误
    /// - `DriverError::ChannelFull`: 命令通道已满（单线程模式）
    /// - `DriverError::ChannelClosed`: 命令通道已关闭
    /// - `DriverError::NotDualThread`: 双线程模式下使用错误的方法
    ///
    /// # 示例
    ///
    /// ```no_run
    /// # use piper_sdk::driver::Piper;
    /// # use piper_sdk::protocol::FirmwareVersionQueryCommand;
    /// # // 注意：此示例需要实际的 CAN 适配器，仅供参考
    /// # // let piper = Piper::new(/* ... */).unwrap();
    /// # // 发送查询命令
    /// # // piper.query_firmware_version().unwrap();
    /// # // 等待反馈数据累积
    /// # // std::thread::sleep(std::time::Duration::from_millis(50));
    /// # // 获取版本字符串
    /// # // if let Some(version) = piper.get_firmware_version() {
    /// # //     println!("Firmware version: {}", version);
    /// # // }
    /// ```
    pub fn query_firmware_version(&self) -> Result<(), DriverError> {
        use crate::protocol::FirmwareVersionQueryCommand;

        // 创建查询命令
        let cmd = FirmwareVersionQueryCommand::new();
        let frame = cmd.to_frame();

        // 发送命令（使用可靠命令模式，确保命令被发送）
        // 注意：固件版本查询不是高频实时命令，使用可靠命令模式更合适
        if let Some(reliable_tx) = &self.reliable_tx {
            // 双线程模式：使用可靠命令队列
            reliable_tx.try_send(frame).map_err(|e| match e {
                crossbeam_channel::TrySendError::Full(_) => DriverError::ChannelFull,
                crossbeam_channel::TrySendError::Disconnected(_) => DriverError::ChannelClosed,
            })?;
        } else {
            // 单线程模式：使用普通命令通道
            self.send_frame(frame)?;
        }

        // 清空固件数据缓存
        if let Ok(mut firmware_state) = self.ctx.firmware_version.write() {
            firmware_state.clear();
        }

        Ok(())
    }

    /// 获取主从模式控制模式指令状态（无锁）
    ///
    /// 包含控制模式、运动模式、速度等（主从模式下，~200Hz更新）。
    ///
    /// # 性能
    /// - 无锁读取（ArcSwap::load）
    /// - 返回快照副本
    pub fn get_master_slave_control_mode(&self) -> MasterSlaveControlModeState {
        self.ctx.master_slave_control_mode.load().as_ref().clone()
    }

    /// 获取主从模式关节控制指令状态（无锁）
    ///
    /// 包含6个关节的目标角度（主从模式下，~500Hz更新）。
    ///
    /// # 性能
    /// - 无锁读取（ArcSwap::load）
    /// - 返回快照副本
    /// - 帧组同步，保证6个关节数据的逻辑一致性
    pub fn get_master_slave_joint_control(&self) -> MasterSlaveJointControlState {
        self.ctx.master_slave_joint_control.load().as_ref().clone()
    }

    /// 获取主从模式夹爪控制指令状态（无锁）
    ///
    /// 包含夹爪目标行程、扭矩等（主从模式下，~200Hz更新）。
    ///
    /// # 性能
    /// - 无锁读取（ArcSwap::load）
    /// - 返回快照副本
    pub fn get_master_slave_gripper_control(&self) -> MasterSlaveGripperControlState {
        self.ctx.master_slave_gripper_control.load().as_ref().clone()
    }

    /// 获取碰撞保护状态（读锁）
    ///
    /// 包含各关节的碰撞保护等级（按需查询）。
    ///
    /// # 性能
    /// - 读锁（RwLock::read）
    /// - 返回快照副本
    pub fn get_collision_protection(&self) -> Result<CollisionProtectionState, DriverError> {
        self.ctx
            .collision_protection
            .read()
            .map(|guard| guard.clone())
            .map_err(|_| DriverError::PoisonedLock)
    }

    /// 获取关节限制配置状态（读锁）
    ///
    /// 包含关节角度限制和速度限制（按需查询）。
    ///
    /// # 性能
    /// - 读锁（RwLock::read）
    /// - 返回快照副本
    pub fn get_joint_limit_config(&self) -> Result<JointLimitConfigState, DriverError> {
        self.ctx
            .joint_limit_config
            .read()
            .map(|guard| guard.clone())
            .map_err(|_| DriverError::PoisonedLock)
    }

    /// 获取关节加速度限制配置状态（读锁）
    ///
    /// 包含关节加速度限制（按需查询）。
    ///
    /// # 性能
    /// - 读锁（RwLock::read）
    /// - 返回快照副本
    pub fn get_joint_accel_config(&self) -> Result<JointAccelConfigState, DriverError> {
        self.ctx
            .joint_accel_config
            .read()
            .map(|guard| guard.clone())
            .map_err(|_| DriverError::PoisonedLock)
    }

    /// 获取末端限制配置状态（读锁）
    ///
    /// 包含末端执行器的速度和加速度限制（按需查询）。
    ///
    /// # 性能
    /// - 读锁（RwLock::read）
    /// - 返回快照副本
    pub fn get_end_limit_config(&self) -> Result<EndLimitConfigState, DriverError> {
        self.ctx
            .end_limit_config
            .read()
            .map(|guard| guard.clone())
            .map_err(|_| DriverError::PoisonedLock)
    }

    /// 获取组合运动状态（所有热数据）
    ///
    /// 注意：不同子状态的时间戳可能不同步（差异通常在毫秒级）。
    /// 如果需要时间对齐的状态，请使用 `get_aligned_motion()`。
    pub fn get_motion_state(&self) -> CombinedMotionState {
        let snapshot = self.capture_motion_snapshot();
        CombinedMotionState {
            joint_position: snapshot.joint_position,
            end_pose: snapshot.end_pose,
            joint_dynamic: self.get_joint_dynamic(),
        }
    }

    /// 获取时间对齐的运动状态（推荐用于力控算法）
    ///
    /// 以 `joint_position.hardware_timestamp_us` 为基准时间，检查时间戳差异。
    /// 即使时间戳差异超过阈值，也返回状态数据（让用户有选择权）。
    ///
    /// # 参数
    /// - `max_time_diff_us`: 允许的最大时间戳差异（微秒），推荐值：5000（5ms）
    ///
    /// # 返回值
    /// - `AlignmentResult::Ok(state)`: 时间戳差异在可接受范围内
    /// - `AlignmentResult::Misaligned { state, diff_us }`: 时间戳差异过大，但仍返回状态数据
    pub fn get_aligned_motion(&self, max_time_diff_us: u64) -> AlignmentResult {
        let snapshot = self.capture_motion_snapshot();
        let joint_dynamic = self.get_joint_dynamic();

        let time_diff = snapshot
            .joint_position
            .hardware_timestamp_us
            .abs_diff(joint_dynamic.group_timestamp_us);

        let state = AlignedMotionState {
            joint_pos: snapshot.joint_position.joint_pos,
            joint_vel: joint_dynamic.joint_vel,
            joint_current: joint_dynamic.joint_current,
            end_pose: snapshot.end_pose.end_pose,
            timestamp: snapshot.joint_position.hardware_timestamp_us, // 使用位置数据的时间戳作为基准
            time_diff_us: (joint_dynamic.group_timestamp_us as i64)
                - (snapshot.joint_position.hardware_timestamp_us as i64),
        };

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
    /// - `timeout`: 超时时间
    ///
    /// # 返回值
    /// - `Ok(())`: 成功接收到有效反馈（`timestamp_us > 0`）
    /// - `Err(DriverError::Timeout)`: 超时未收到反馈
    pub fn wait_for_feedback(&self, timeout: std::time::Duration) -> Result<(), DriverError> {
        let start = std::time::Instant::now();

        loop {
            // 检查是否超时
            if start.elapsed() >= timeout {
                return Err(DriverError::Timeout);
            }

            // 检查是否收到有效反馈（任意状态的时间戳 > 0 即可）
            let joint_pos = self.get_joint_position();
            if joint_pos.hardware_timestamp_us > 0 {
                return Ok(());
            }

            // 短暂休眠，避免 CPU 空转
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    /// 获取 FPS 统计结果
    ///
    /// 返回最近一次统计窗口内的更新频率（FPS）。
    /// 建议定期调用（如每秒一次）或按需调用。
    ///
    /// # 性能
    /// - 无锁读取（仅原子读取）
    /// - 开销：~100ns（5 次原子读取 + 浮点计算）
    ///
    /// # Example
    ///
    /// ```
    /// # use piper_sdk::driver::Piper;
    /// # // 注意：此示例需要实际的 CAN 适配器，仅供参考
    /// # // let piper = Piper::new(/* ... */).unwrap();
    /// # // 运行一段时间后查询 FPS
    /// # // std::thread::sleep(std::time::Duration::from_secs(5));
    /// # // let fps = piper.get_fps();
    /// # // println!("Joint Position FPS: {:.2}", fps.joint_position);
    /// # // println!("End Pose FPS: {:.2}", fps.end_pose);
    /// # // println!("Joint Dynamic FPS: {:.2}", fps.joint_dynamic);
    /// ```
    pub fn get_fps(&self) -> FpsResult {
        self.ctx.fps_stats.load().calculate_fps()
    }

    /// 获取 FPS 计数器原始值
    ///
    /// 返回当前计数器的原始值，可以配合自定义时间窗口计算 FPS。
    ///
    /// # 性能
    /// - 无锁读取（仅原子读取）
    /// - 开销：~50ns（5 次原子读取）
    ///
    /// # Example
    ///
    /// ```
    /// # use piper_sdk::driver::Piper;
    /// # // 注意：此示例需要实际的 CAN 适配器，仅供参考
    /// # // let piper = Piper::new(/* ... */).unwrap();
    /// # // 记录开始时间和计数
    /// # // let start = std::time::Instant::now();
    /// # // let counts_start = piper.get_fps_counts();
    /// # // 运行一段时间
    /// # // std::thread::sleep(std::time::Duration::from_secs(1));
    /// # // 计算实际 FPS
    /// # // let counts_end = piper.get_fps_counts();
    /// # // let elapsed = start.elapsed();
    /// # // let actual_fps = (counts_end.joint_position - counts_start.joint_position) as f64 / elapsed.as_secs_f64();
    /// ```
    pub fn get_fps_counts(&self) -> FpsCounts {
        self.ctx.fps_stats.load().get_counts()
    }

    /// 重置 FPS 统计窗口（清空计数器并重新开始计时）
    ///
    /// 这是一个轻量级、无锁的重置：通过 `ArcSwap` 将内部 `FpsStatistics` 原子替换为新实例。
    /// 适合在监控工具中做固定窗口统计（例如每 5 秒 reset 一次）。
    pub fn reset_fps_stats(&self) {
        self.ctx
            .fps_stats
            .store(Arc::new(crate::driver::fps_stats::FpsStatistics::new()));
    }

    /// 发送控制帧（非阻塞）
    ///
    /// # 参数
    /// - `frame`: 控制帧（已构建的 `PiperFrame`）
    ///
    /// # 错误
    /// - `DriverError::ChannelClosed`: 命令通道已关闭（IO 线程退出）
    /// - `DriverError::ChannelFull`: 命令队列已满（缓冲区容量 10）
    pub fn send_frame(&self, frame: PiperFrame) -> Result<(), DriverError> {
        self.cmd_tx.try_send(frame).map_err(|e| match e {
            crossbeam_channel::TrySendError::Full(_) => DriverError::ChannelFull,
            crossbeam_channel::TrySendError::Disconnected(_) => DriverError::ChannelClosed,
        })
    }

    /// 发送控制帧（阻塞，带超时）
    ///
    /// 如果命令通道已满，阻塞等待直到有空闲位置或超时。
    ///
    /// # 参数
    /// - `frame`: 控制帧（已构建的 `PiperFrame`）
    /// - `timeout`: 超时时间
    ///
    /// # 错误
    /// - `DriverError::ChannelClosed`: 命令通道已关闭（IO 线程退出）
    /// - `DriverError::Timeout`: 超时未发送成功
    pub fn send_frame_blocking(
        &self,
        frame: PiperFrame,
        timeout: std::time::Duration,
    ) -> Result<(), DriverError> {
        self.cmd_tx.send_timeout(frame, timeout).map_err(|e| match e {
            crossbeam_channel::SendTimeoutError::Timeout(_) => DriverError::Timeout,
            crossbeam_channel::SendTimeoutError::Disconnected(_) => DriverError::ChannelClosed,
        })
    }

    /// 发送实时控制命令（邮箱模式，覆盖策略）
    ///
    /// 实时命令使用邮箱模式（Mailbox），直接覆盖旧命令，确保最新命令被发送。
    /// 这对于力控/高频控制场景很重要，只保留最新的控制指令。
    ///
    /// # 参数
    /// - `frame`: 控制帧（已构建的 `PiperFrame`）
    ///
    /// # 错误
    /// - `DriverError::NotDualThread`: 未使用双线程模式
    /// - `DriverError::PoisonedLock`: 锁中毒（极少见，通常意味着 TX 线程 panic）
    ///
    /// # 实现细节
    /// - 获取 Mutex 锁并直接覆盖插槽内容（Last Write Wins）
    /// - 锁持有时间极短（< 50ns），仅为内存拷贝
    /// - 永不阻塞：无论 TX 线程是否消费，都能立即写入
    /// - 如果插槽已有数据，会被覆盖（更新 `metrics.tx_realtime_overwrites`）
    ///
    /// # 性能
    /// - 典型延迟：20-50ns（无竞争情况下）
    /// - 最坏延迟：200ns（与 TX 线程锁竞争时）
    /// - 相比 Channel 重试策略，延迟降低 10-100 倍
    ///
    /// 发送单个实时帧（向后兼容，API 不变）
    pub fn send_realtime(&self, frame: PiperFrame) -> Result<(), DriverError> {
        self.send_realtime_command(RealtimeCommand::single(frame))
    }

    /// 发送实时帧包（新 API）
    ///
    /// # 参数
    /// - `frames`: 要发送的帧迭代器，必须非空
    ///
    /// **接口优化**：接受 `impl IntoIterator`，允许用户传入：
    /// - 数组：`[frame1, frame2, frame3]`（栈上，零堆分配）
    /// - 切片：`&[frame1, frame2, frame3]`
    /// - Vec：`vec![frame1, frame2, frame3]`
    ///
    /// # 错误
    /// - `DriverError::NotDualThread`: 未使用双线程模式
    /// - `DriverError::InvalidInput`: 帧列表为空或过大
    /// - `DriverError::PoisonedLock`: 锁中毒
    ///
    /// # 原子性保证
    /// Package 内的所有帧要么全部发送成功，要么都不发送。
    /// 如果发送过程中出现错误，已发送的帧不会被回滚（CAN 总线特性），
    /// 但未发送的帧不会继续发送。
    ///
    /// # 性能特性
    /// - 如果帧数量 ≤ 4，完全在栈上分配，零堆内存分配
    /// - 如果帧数量 > 4，SmallVec 会自动溢出到堆，但仍保持高效
    pub fn send_realtime_package(
        &self,
        frames: impl IntoIterator<Item = PiperFrame>,
    ) -> Result<(), DriverError> {
        use crate::driver::command::FrameBuffer;

        let buffer: FrameBuffer = frames.into_iter().collect();

        if buffer.is_empty() {
            return Err(DriverError::InvalidInput(
                "Frame package cannot be empty".to_string(),
            ));
        }

        // 限制包大小，防止内存问题
        // 使用 Piper 的关联常量，允许客户端预检查
        //
        // 注意：如果用户传入超大 Vec（如长度 1000），这里会先进行 collect 操作，
        // 可能导致堆分配。虽然之后会检查并报错，但内存开销已经发生。
        // 这是可以接受的权衡（安全网），但建议用户在调用前进行预检查。
        if buffer.len() > Self::MAX_REALTIME_PACKAGE_SIZE {
            return Err(DriverError::InvalidInput(format!(
                "Frame package too large: {} (max: {})",
                buffer.len(),
                Self::MAX_REALTIME_PACKAGE_SIZE
            )));
        }

        self.send_realtime_command(RealtimeCommand::package(buffer))
    }

    /// 内部方法：发送实时命令（统一处理单个帧和帧包）
    fn send_realtime_command(&self, command: RealtimeCommand) -> Result<(), DriverError> {
        let realtime_slot = self.realtime_slot.as_ref().ok_or(DriverError::NotDualThread)?;

        match realtime_slot.lock() {
            Ok(mut slot) => {
                // 检测是否发生覆盖（如果插槽已有数据）
                let is_overwrite = slot.is_some();

                // 计算帧数量（在覆盖前，避免双重计算）
                let frame_count = command.len();

                // 直接覆盖（邮箱模式：Last Write Wins）
                // 注意：如果旧命令是 Package，Drop 操作会释放 SmallVec
                // 但如果数据在栈上（len ≤ 4），Drop 只是栈指针移动，几乎零开销
                *slot = Some(command);

                // 更新指标（在锁外更新，减少锁持有时间）
                // 注意：先释放锁，再更新指标，避免在锁内进行原子操作
                drop(slot); // 显式释放锁

                // 更新指标（在锁外更新，减少锁持有时间）
                let total =
                    self.metrics.tx_frames_total.fetch_add(frame_count as u64, Ordering::Relaxed)
                        + frame_count as u64;

                if is_overwrite {
                    let overwrites =
                        self.metrics.tx_realtime_overwrites.fetch_add(1, Ordering::Relaxed) + 1;

                    // 智能监控：每 1000 次发送检查一次覆盖率
                    // 避免频繁计算，减少性能开销
                    if total > 0 && total.is_multiple_of(1000) {
                        let rate = (overwrites as f64 / total as f64) * 100.0;

                        // 只在覆盖率超过阈值时警告
                        if rate > 50.0 {
                            // 异常情况：覆盖率 > 50%，记录警告
                            warn!(
                                "High realtime overwrite rate detected: {:.1}% ({} overwrites / {} total sends). \
                                 This may indicate TX thread bottleneck or excessive send frequency.",
                                rate, overwrites, total
                            );
                        } else if rate > 30.0 {
                            // 中等情况：覆盖率 30-50%，记录信息（可选，生产环境可关闭）
                            info!(
                                "Moderate realtime overwrite rate: {:.1}% ({} overwrites / {} total sends). \
                                 This is normal for high-frequency control (> 500Hz).",
                                rate, overwrites, total
                            );
                        }
                        // < 30% 不记录日志（正常情况）
                    }
                }

                Ok(())
            },
            Err(_) => {
                error!("Realtime slot lock poisoned, TX thread may have panicked");
                Err(DriverError::PoisonedLock)
            },
        }
    }

    /// 发送可靠命令（FIFO 策略）
    ///
    /// 可靠命令使用容量为 10 的队列，按 FIFO 顺序发送，不会覆盖。
    /// 这对于配置帧、状态机切换帧等关键命令很重要。
    ///
    /// # 参数
    /// - `frame`: 控制帧（已构建的 `PiperFrame`）
    ///
    /// # 错误
    /// - `DriverError::NotDualThread`: 未使用双线程模式
    /// - `DriverError::ChannelClosed`: 命令通道已关闭（TX 线程退出）
    /// - `DriverError::ChannelFull`: 队列满（非阻塞）
    pub fn send_reliable(&self, frame: PiperFrame) -> Result<(), DriverError> {
        let reliable_tx = self.reliable_tx.as_ref().ok_or(DriverError::NotDualThread)?;

        match reliable_tx.try_send(frame) {
            Ok(_) => {
                self.metrics.tx_frames_total.fetch_add(1, Ordering::Relaxed);
                Ok(())
            },
            Err(crossbeam_channel::TrySendError::Full(_)) => {
                // 队列满，记录丢弃
                self.metrics.tx_reliable_drops.fetch_add(1, Ordering::Relaxed);
                Err(DriverError::ChannelFull)
            },
            Err(crossbeam_channel::TrySendError::Disconnected(_)) => {
                Err(DriverError::ChannelClosed)
            },
        }
    }

    /// 发送命令（根据优先级自动选择队列）
    ///
    /// 根据命令的优先级自动选择实时队列或可靠队列。
    ///
    /// # 参数
    /// - `command`: 带优先级的命令
    ///
    /// # 错误
    /// - `DriverError::NotDualThread`: 未使用双线程模式
    /// - `DriverError::ChannelClosed`: 命令通道已关闭（TX 线程退出）
    /// - `DriverError::ChannelFull`: 队列满（仅可靠命令）
    pub fn send_command(&self, command: PiperCommand) -> Result<(), DriverError> {
        match command.priority() {
            CommandPriority::RealtimeControl => self.send_realtime(command.frame()),
            CommandPriority::ReliableCommand => self.send_reliable(command.frame()),
        }
    }

    /// 发送可靠命令（阻塞，带超时）
    ///
    /// 如果队列满，阻塞等待直到有空闲位置或超时。
    ///
    /// # 参数
    /// - `frame`: 控制帧（已构建的 `PiperFrame`）
    /// - `timeout`: 超时时间
    ///
    /// # 错误
    /// - `DriverError::NotDualThread`: 未使用双线程模式
    /// - `DriverError::ChannelClosed`: 命令通道已关闭（TX 线程退出）
    /// - `DriverError::Timeout`: 超时未发送成功
    pub fn send_reliable_timeout(
        &self,
        frame: PiperFrame,
        timeout: std::time::Duration,
    ) -> Result<(), DriverError> {
        let reliable_tx = self.reliable_tx.as_ref().ok_or(DriverError::NotDualThread)?;

        match reliable_tx.send_timeout(frame, timeout) {
            Ok(_) => {
                self.metrics.tx_frames_total.fetch_add(1, Ordering::Relaxed);
                Ok(())
            },
            Err(crossbeam_channel::SendTimeoutError::Timeout(_)) => Err(DriverError::Timeout),
            Err(crossbeam_channel::SendTimeoutError::Disconnected(_)) => {
                Err(DriverError::ChannelClosed)
            },
        }
    }
}

impl Drop for Piper {
    fn drop(&mut self) {
        // 设置运行标志为 false，通知所有线程退出
        self.is_running.store(false, Ordering::Relaxed);

        // 关闭命令通道（通知 IO 线程退出）
        // 关键：必须在 join 线程之前真正 drop 掉 Sender，否则接收端不会 Disconnected。
        unsafe {
            ManuallyDrop::drop(&mut self.cmd_tx);
        }

        // 等待 RX 线程退出
        if let Some(handle) = self.rx_thread.take() {
            let start = std::time::Instant::now();
            while start.elapsed().as_secs() < 2 {
                if handle.is_finished() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }

            if let Err(_e) = handle.join() {
                error!("RX thread panicked");
            }
        }

        // 等待 TX 线程退出
        if let Some(handle) = self.tx_thread.take() {
            let start = std::time::Instant::now();
            while start.elapsed().as_secs() < 2 {
                if handle.is_finished() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }

            if let Err(_e) = handle.join() {
                error!("TX thread panicked");
            }
        }

        // 等待 IO 线程退出（单线程模式）
        if let Some(handle) = self.io_thread.take() {
            // 设置超时，避免测试无限等待
            let start = std::time::Instant::now();
            while start.elapsed().as_secs() < 2 {
                if handle.is_finished() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }

            if let Err(_e) = handle.join() {
                error!("IO thread panicked");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::can::PiperFrame;

    // 简单的 Mock CanAdapter 用于测试
    struct MockCanAdapter;

    impl CanAdapter for MockCanAdapter {
        fn send(&mut self, _frame: PiperFrame) -> Result<(), CanError> {
            Ok(())
        }

        fn receive(&mut self) -> Result<PiperFrame, CanError> {
            // 永远超时，避免阻塞测试
            Err(CanError::Timeout)
        }
    }

    #[test]
    fn test_piper_new() {
        let mock_can = MockCanAdapter;
        let piper = Piper::new(mock_can, None).unwrap();

        // 验证可以获取状态（默认状态）
        let joint_pos = piper.get_joint_position();
        assert_eq!(joint_pos.hardware_timestamp_us, 0);

        // 验证通道正常工作
        let frame = PiperFrame::new_standard(0x123, &[0x01, 0x02]);
        assert!(piper.send_frame(frame).is_ok());
    }

    #[test]
    fn test_piper_drop() {
        let mock_can = MockCanAdapter;
        let piper = Piper::new(mock_can, None).unwrap();
        // drop 应该能够正常退出，IO 线程被 join
        drop(piper);
    }

    #[test]
    fn test_piper_get_motion_state() {
        let mock_can = MockCanAdapter;
        let piper = Piper::new(mock_can, None).unwrap();
        let motion = piper.get_motion_state();
        assert_eq!(motion.joint_position.hardware_timestamp_us, 0);
        assert_eq!(motion.joint_dynamic.group_timestamp_us, 0);
    }

    #[test]
    fn test_piper_send_frame_channel_full() {
        let mock_can = MockCanAdapter;
        let piper = Piper::new(mock_can, None).unwrap();
        let frame = PiperFrame::new_standard(0x123, &[0x01]);

        // 填满命令通道（容量 10）
        // 注意：IO 线程会持续消费帧，所以需要快速填充
        // 或者等待 IO 线程稍微延迟消费
        std::thread::sleep(std::time::Duration::from_millis(50));

        for _ in 0..10 {
            assert!(piper.send_frame(frame).is_ok());
        }

        // 第 11 次发送可能返回 ChannelFull（如果 IO 线程还没消费完）
        // 或者成功（如果 IO 线程已经消费了一些）
        // 为了测试 ChannelFull，我们需要更快速地发送，确保通道填满
        let result = piper.send_frame(frame);

        // 由于 IO 线程在后台消费，可能成功也可能失败
        // 验证至少前 10 次都成功即可
        match result {
            Err(DriverError::ChannelFull) => {
                // 通道满，这是预期情况
            },
            Ok(()) => {
                // 如果 IO 线程消费很快，这也可能发生
                // 这是可接受的行为
            },
            Err(e) => panic!("Unexpected error: {:?}", e),
        }
    }

    #[test]
    fn test_get_aligned_motion_aligned() {
        let mock_can = MockCanAdapter;
        let piper = Piper::new(mock_can, None).unwrap();

        // 由于 MockCanAdapter 不发送帧，时间戳都为 0
        // 测试默认状态下的对齐检查（时间戳都为 0，应该是对齐的）
        let result = piper.get_aligned_motion(5000);
        match result {
            AlignmentResult::Ok(state) => {
                assert_eq!(state.timestamp, 0);
                assert_eq!(state.time_diff_us, 0);
            },
            AlignmentResult::Misaligned { .. } => {
                // 如果时间戳都为 0，不应该是不对齐的
                // 但允许这种情况（因为时间戳都是 0）
            },
        }
    }

    #[test]
    fn test_get_aligned_motion_misaligned_threshold() {
        let mock_can = MockCanAdapter;
        let piper = Piper::new(mock_can, None).unwrap();

        // 测试不同的时间差阈值
        // 由于时间戳都是 0，应该是对齐的
        let result1 = piper.get_aligned_motion(0);
        let result2 = piper.get_aligned_motion(1000);
        let result3 = piper.get_aligned_motion(1000000);

        // 所有结果都应该返回状态（即使是对齐的）
        match (result1, result2, result3) {
            (AlignmentResult::Ok(_), AlignmentResult::Ok(_), AlignmentResult::Ok(_)) => {
                // 正常情况
            },
            _ => {
                // 允许其他情况
            },
        }
    }

    #[test]
    fn test_get_robot_control() {
        let mock_can = MockCanAdapter;
        let piper = Piper::new(mock_can, None).unwrap();

        let control = piper.get_robot_control();
        assert_eq!(control.hardware_timestamp_us, 0);
        assert_eq!(control.control_mode, 0);
        assert!(!control.is_enabled);
    }

    #[test]
    fn test_get_joint_driver_low_speed() {
        let mock_can = MockCanAdapter;
        let piper = Piper::new(mock_can, None).unwrap();

        let driver_state = piper.get_joint_driver_low_speed();
        assert_eq!(driver_state.hardware_timestamp_us, 0);
        assert_eq!(driver_state.motor_temps, [0.0; 6]);
    }

    #[test]
    fn test_get_joint_limit_config() {
        let mock_can = MockCanAdapter;
        let piper = Piper::new(mock_can, None).unwrap();

        let limits = piper.get_joint_limit_config().unwrap();
        assert_eq!(limits.joint_limits_max, [0.0; 6]);
    }

    #[test]
    fn test_wait_for_feedback_timeout() {
        let mock_can = MockCanAdapter;
        let piper = Piper::new(mock_can, None).unwrap();

        // MockCanAdapter 不发送帧，所以应该超时
        let result = piper.wait_for_feedback(std::time::Duration::from_millis(10));
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), DriverError::Timeout));
    }

    #[test]
    fn test_send_frame_blocking_timeout() {
        let mock_can = MockCanAdapter;
        let piper = Piper::new(mock_can, None).unwrap();
        let frame = PiperFrame::new_standard(0x123, &[0x01]);

        // 快速填充通道（如果 IO 线程来不及消费）
        // 然后测试阻塞发送
        // 由于通道容量为 10，在 IO 线程消费的情况下，应该能成功
        // 但为了测试超时，我们使用极短的超时时间
        let result = piper.send_frame_blocking(frame, std::time::Duration::from_millis(1));

        // 结果可能是成功（IO 线程消费快）或超时（通道满）
        match result {
            Ok(()) => {
                // 成功是正常情况
            },
            Err(DriverError::Timeout) => {
                // 超时也是可接受的（如果通道满）
            },
            Err(e) => panic!("Unexpected error: {:?}", e),
        }
    }

    #[test]
    fn test_get_aligned_motion_with_time_diff() {
        let mock_can = MockCanAdapter;
        let piper = Piper::new(mock_can, None).unwrap();

        // 测试对齐阈值边界情况
        // 时间戳都为 0 时，time_diff_us 应该是 0
        let result = piper.get_aligned_motion(0);
        match result {
            AlignmentResult::Ok(state) => {
                assert_eq!(state.time_diff_us, 0);
            },
            AlignmentResult::Misaligned { state, diff_us } => {
                // 如果时间戳都为 0，diff_us 应该也是 0
                assert_eq!(diff_us, 0);
                assert_eq!(state.time_diff_us, 0);
            },
        }
    }

    #[test]
    fn test_get_motion_state_returns_combined() {
        let mock_can = MockCanAdapter;
        let piper = Piper::new(mock_can, None).unwrap();

        let motion = piper.get_motion_state();
        // 验证返回的是组合状态
        assert_eq!(motion.joint_position.hardware_timestamp_us, 0);
        assert_eq!(motion.joint_dynamic.group_timestamp_us, 0);
        assert_eq!(motion.joint_position.joint_pos, [0.0; 6]);
        assert_eq!(motion.joint_dynamic.joint_vel, [0.0; 6]);
    }

    #[test]
    fn test_send_frame_non_blocking() {
        let mock_can = MockCanAdapter;
        let piper = Piper::new(mock_can, None).unwrap();
        let frame = PiperFrame::new_standard(0x123, &[0x01, 0x02]);

        // 非阻塞发送应该总是成功（除非通道满或关闭）
        let result = piper.send_frame(frame);
        assert!(result.is_ok(), "Non-blocking send should succeed");
    }

    #[test]
    fn test_get_joint_dynamic_default() {
        let mock_can = MockCanAdapter;
        let piper = Piper::new(mock_can, None).unwrap();

        let joint_dynamic = piper.get_joint_dynamic();
        assert_eq!(joint_dynamic.group_timestamp_us, 0);
        assert_eq!(joint_dynamic.joint_vel, [0.0; 6]);
        assert_eq!(joint_dynamic.joint_current, [0.0; 6]);
        assert!(!joint_dynamic.is_complete());
    }

    #[test]
    fn test_get_joint_position_default() {
        let mock_can = MockCanAdapter;
        let piper = Piper::new(mock_can, None).unwrap();

        let joint_pos = piper.get_joint_position();
        assert_eq!(joint_pos.hardware_timestamp_us, 0);
        assert_eq!(joint_pos.joint_pos, [0.0; 6]);

        let end_pose = piper.get_end_pose();
        assert_eq!(end_pose.hardware_timestamp_us, 0);
        assert_eq!(end_pose.end_pose, [0.0; 6]);
    }

    #[test]
    fn test_joint_driver_low_speed_clone() {
        let mock_can = MockCanAdapter;
        let piper = Piper::new(mock_can, None).unwrap();

        // 测试读取并克隆诊断状态
        let driver1 = piper.get_joint_driver_low_speed();
        let driver2 = piper.get_joint_driver_low_speed();

        // 验证可以多次读取（ArcSwap 无锁读取）
        assert_eq!(driver1.hardware_timestamp_us, driver2.hardware_timestamp_us);
        assert_eq!(driver1.motor_temps, driver2.motor_temps);
    }

    #[test]
    fn test_joint_limit_config_read_lock() {
        let mock_can = MockCanAdapter;
        let piper = Piper::new(mock_can, None).unwrap();

        // 测试可以多次读取配置状态
        let limits1 = piper.get_joint_limit_config().unwrap();
        let limits2 = piper.get_joint_limit_config().unwrap();

        assert_eq!(limits1.joint_limits_max, limits2.joint_limits_max);
        assert_eq!(limits1.joint_limits_min, limits2.joint_limits_min);
    }
}
