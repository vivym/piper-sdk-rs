//! SocketCAN 适配器分离实现
//!
//! 提供独立的 RX 和 TX 适配器，支持双线程并发访问。
//! 基于 `CanSocket::try_clone()` 实现，利用 Linux 的 `dup()` 系统调用。
//!
//! # ⚠️ 关键警告：`try_clone()` 的共享状态陷阱
//!
//! `try_clone()` 通过 `dup()` 系统调用复制文件描述符（FD），这意味着：
//!
//! 1. **文件状态标志共享**：`O_NONBLOCK` 等标志保存在"打开文件描述"中，而不是 FD 中。
//!    - **后果**：如果在 RX 线程对 socket 设置了 `set_nonblocking(true)`，TX 线程的 socket **也会瞬间变成非阻塞模式**（反之亦然）。
//!    - **避坑指南**：**严禁在分离后的适配器中使用 `set_nonblocking()`**。必须严格依赖 `SO_RCVTIMEO` 和 `SO_SNDTIMEO` 来实现超时。
//!
//! 2. **过滤器共享**：`SO_CAN_RAW_FILTER` 通常绑定在 Socket 对象上。
//!    - **后果**：RX 适配器设置的硬件过滤器会影响所有共享该打开文件描述的 FD。
//!    - **现状**：当前设计是安全的（TX 只写不读），但需知晓此特性。
//!
//! # 设计原则
//!
//! - **严格使用超时**：使用 `SO_RCVTIMEO` 和 `SO_SNDTIMEO` 实现超时，而非 `O_NONBLOCK` + `poll`/`select`
//! - **FD 生命周期**：通过 RAII 自动管理，无需手动关闭
//! - **线程安全**：RX 和 TX 适配器可以在不同线程中并发使用
//! - **时间戳支持**：使用 `recvmsg` 和 CMSG 提取硬件/软件时间戳（与 `SocketCanAdapter` 一致）

use crate::{
    BackendCapability, CanDeviceError, CanDeviceErrorKind, CanError, CanId, PiperFrame,
    RawTimestampInfo, RawTimestampSample, RealtimeTxAdapter, ReceivedFrame, RxAdapter,
    TimestampProvenance,
};
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use nix::sys::socket::{ControlMessageOwned, MsgFlags, SockaddrStorage, recvmsg};
use piper_protocol::ids::{driver_rx_robot_feedback_ids, is_robot_feedback_id};
use socketcan::{
    BlockingCan, CanFilter, CanFrame, CanSocket, EmbeddedFrame, ExtendedId, Socket, SocketOptions,
    StandardId,
};
use std::collections::VecDeque;
use std::io::IoSliceMut;
use std::mem;
use std::os::fd::BorrowedFd;
use std::os::fd::OwnedFd;
use std::os::unix::io::{AsRawFd, FromRawFd};
use std::time::{Duration, Instant};
use tracing::{trace, warn};

use super::CANFD_MTU;
use super::raw_frame::{ParsedSocketCanFrame, parse_libc_can_frame_bytes};

/// 检查 socket 是否启用了 SO_TIMESTAMPING
///
/// 通过 getsockopt 查询 SO_TIMESTAMPING 选项的值，验证时间戳功能是否已启用。
/// 这是一个运行时检查，确保 dup() 后的 socket 确实继承了时间戳设置。
fn check_timestamping_enabled(socket: &CanSocket) -> bool {
    unsafe {
        let mut flags: u32 = 0;
        let mut len = mem::size_of::<u32>() as libc::socklen_t;

        let ret = libc::getsockopt(
            socket.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_TIMESTAMPING,
            &mut flags as *mut _ as *mut libc::c_void,
            &mut len,
        );

        if ret < 0 {
            // getsockopt 失败，说明可能未启用或系统不支持
            warn!(
                "Failed to query SO_TIMESTAMPING on dup'd socket: {}",
                std::io::Error::last_os_error()
            );
            return false;
        }

        // 检查是否有任何时间戳标志被设置
        let expected_flags = libc::SOF_TIMESTAMPING_RX_HARDWARE
            | libc::SOF_TIMESTAMPING_RAW_HARDWARE
            | libc::SOF_TIMESTAMPING_RX_SOFTWARE
            | libc::SOF_TIMESTAMPING_SOFTWARE;

        flags & expected_flags != 0
    }
}

/// 使用 dup() 复制底层 FD，生成新的 `CanSocket`
fn dup_socket(socket: &CanSocket) -> Result<CanSocket, CanError> {
    let fd = unsafe { libc::dup(socket.as_raw_fd()) };
    if fd < 0 {
        return Err(CanError::Io(std::io::Error::last_os_error()));
    }

    // SAFETY: dup 返回全新的 FD，调用方负责关闭，`OwnedFd::from_raw_fd`
    // 接管所有权，RAII 关闭。
    let owned: OwnedFd = unsafe { OwnedFd::from_raw_fd(fd) };
    Ok(CanSocket::from(owned))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TimestampSource {
    None,
    Software,
    Hardware,
}

#[derive(Debug, Clone, Copy)]
struct TimestampInfo {
    timestamp_us: u64,
    source: TimestampSource,
    system_ts_us: Option<u64>,
    hw_trans_us: Option<u64>,
    hw_raw_us: Option<u64>,
}

#[derive(Debug, Clone)]
struct SocketCanReceivedFrame {
    frame: PiperFrame,
    timestamp_source: TimestampSource,
    timestamp_provenance: TimestampProvenance,
    raw_timestamp: Option<RawTimestampInfo>,
    raw_sample: RawTimestampSample,
}

fn classify_startup_probe_frame(
    frame: &PiperFrame,
    timestamp_source: TimestampSource,
) -> Option<BackendCapability> {
    if !is_robot_feedback_id(frame.id()) {
        return None;
    }

    match timestamp_source {
        TimestampSource::Hardware => Some(BackendCapability::StrictRealtime),
        TimestampSource::Software => Some(BackendCapability::SoftRealtime),
        TimestampSource::None => None,
    }
}

fn hardware_filter_ids() -> &'static [piper_protocol::StandardCanId] {
    driver_rx_robot_feedback_ids()
}

fn should_buffer_bootstrap_frame(frame: &PiperFrame) -> bool {
    frame.is_standard() && is_robot_feedback_id(frame.id())
}

fn timestamp_provenance_for_source(source: TimestampSource) -> TimestampProvenance {
    match source {
        TimestampSource::Hardware => TimestampProvenance::Hardware,
        TimestampSource::Software => TimestampProvenance::Kernel,
        TimestampSource::None => TimestampProvenance::None,
    }
}

/// 只读适配器（用于 RX 线程）
///
/// 独立的 RX 适配器，持有 `CanSocket` 的克隆，
/// 可以在不同线程中与 `SocketCanTxAdapter` 并发使用。
///
/// # 关键设计
/// - **硬件过滤器**：在初始化时配置，只接收相关的 CAN ID（如 0x251-0x256）
/// - **读超时**：使用 `SO_RCVTIMEO` 实现，避免阻塞
/// - **FD 共享**：通过 `try_clone()` 共享同一个打开的文件描述，共享文件状态标志
/// - **时间戳支持**：使用 `recvmsg` 和 CMSG 提取硬件/软件时间戳（与 `SocketCanAdapter` 一致）
pub struct SocketCanRxAdapter {
    socket: CanSocket,
    iface: String,
    read_timeout: Duration,
    /// 是否启用时间戳（从原始 socket 继承，SocketCanAdapter 初始化时已启用）
    timestamping_enabled: bool,
    /// 是否检测到硬件时间戳支持（运行时检测）
    hw_timestamp_available: bool,
    /// Startup probe drains frames before worker threads exist; replay them first once RX starts.
    bootstrap_frames: VecDeque<ReceivedFrame>,
    /// Final capability resolved by startup probing. Defaults to the safe soft-realtime posture.
    backend_capability: BackendCapability,
    startup_probe_resolved: bool,
}

impl SocketCanRxAdapter {
    /// 创建新的 RX 适配器
    ///
    /// # 参数
    /// - `socket`: SocketCAN socket（将被克隆）
    /// - `read_timeout`: 读超时时间
    ///
    /// # 错误
    /// - `CanError::Io`: 克隆 socket 或配置过滤器失败
    pub fn new(socket: &CanSocket, read_timeout: Duration) -> Result<Self, CanError> {
        Self::new_with_iface(socket, read_timeout, "unknown")
    }

    pub(crate) fn new_with_iface(
        socket: &CanSocket,
        read_timeout: Duration,
        iface: impl Into<String>,
    ) -> Result<Self, CanError> {
        // 克隆 socket（使用 dup() 系统调用）
        let rx_socket = dup_socket(socket)?;

        // 设置读超时（使用 SO_RCVTIMEO，避免依赖 O_NONBLOCK）
        rx_socket.set_read_timeout(read_timeout).map_err(|e| {
            CanError::Io(std::io::Error::other(format!(
                "Failed to set read timeout on RX socket: {}",
                e
            )))
        })?;

        // 在 split RX 路径上默认启用 robot-only kernel filter，避免共享总线噪声
        // 进入实时反馈/启动探测路径。
        Self::configure_hardware_filters(&rx_socket)?;

        // ✅ 检查时间戳是否已启用（从原始 socket 继承）
        // SocketCanAdapter 在初始化时已启用 SO_TIMESTAMPING
        // 由于 dup() 共享 socket 选项，这里需要实际查询验证
        let timestamping_enabled = check_timestamping_enabled(&rx_socket);

        if timestamping_enabled {
            trace!("SocketCanRxAdapter: SO_TIMESTAMPING verified on dup'd socket");
        } else {
            return Err(CanError::Device(CanDeviceError::new(
                CanDeviceErrorKind::UnsupportedConfig,
                "SO_TIMESTAMPING is not available on the dup'd SocketCAN RX socket; strict realtime requires trusted CAN timestamps",
            )));
        }

        let hw_timestamp_available = false; // 运行时检测

        Ok(Self {
            socket: rx_socket,
            iface: iface.into(),
            read_timeout,
            timestamping_enabled,
            hw_timestamp_available,
            bootstrap_frames: VecDeque::new(),
            backend_capability: BackendCapability::SoftRealtime,
            startup_probe_resolved: false,
        })
    }

    /// 配置硬件过滤器
    ///
    /// 只接收与机械臂相关的 CAN ID，过滤掉无关帧。
    /// 这能显著降低在繁忙总线上的 CPU 占用。
    ///
    /// # 参数
    /// - `socket`: SocketCAN socket
    ///
    /// # 错误
    /// - `CanError::Io`: 设置过滤器失败
    ///
    fn configure_hardware_filters(socket: &CanSocket) -> Result<(), CanError> {
        // 创建过滤器（精确匹配）
        let filters: Vec<CanFilter> = hardware_filter_ids()
            .iter()
            .map(|&id| {
                // 使用 0x7FF 作为掩码，实现精确匹配（标准帧）
                // 如果需要支持扩展帧，使用 0x1FFFFFFF
                CanFilter::new(id.raw() as u32, 0x7FF)
            })
            .collect();

        // 设置过滤器
        socket.set_filters(&filters).map_err(|e| {
            CanError::Io(std::io::Error::other(format!(
                "Failed to set hardware filters: {}",
                e
            )))
        })?;

        trace!(
            "SocketCAN hardware filters configured for {} robot feedback IDs",
            filters.len()
        );

        Ok(())
    }

    /// 获取读超时时间
    pub fn read_timeout(&self) -> Duration {
        self.read_timeout
    }

    /// 设置读超时
    pub fn set_read_timeout(&mut self, timeout: Duration) -> Result<(), CanError> {
        self.socket.set_read_timeout(timeout).map_err(|e| {
            CanError::Io(std::io::Error::other(format!(
                "Failed to set read timeout: {}",
                e
            )))
        })?;
        self.read_timeout = timeout;
        Ok(())
    }

    pub fn receive_raw_timestamp_sample(
        &mut self,
        timeout: Duration,
    ) -> Result<RawTimestampSample, CanError> {
        let received = self.receive_live(timeout)?;
        Ok(received.raw_sample)
    }

    fn receive_live(&mut self, timeout: Duration) -> Result<SocketCanReceivedFrame, CanError> {
        loop {
            let fd = self.socket.as_raw_fd();

            let pollfd = PollFd::new(unsafe { BorrowedFd::borrow_raw(fd) }, PollFlags::POLLIN);
            let timeout_ms = timeout.as_millis().min(65535) as u16;
            match poll(&mut [pollfd], PollTimeout::from(timeout_ms)) {
                Ok(0) => {
                    return Err(CanError::Timeout);
                },
                Ok(_) => {},
                Err(e) => {
                    return Err(CanError::Io(std::io::Error::other(format!(
                        "poll failed: {}",
                        e
                    ))));
                },
            }

            // Keep a CAN FD-sized receive buffer so the parser sees and rejects
            // non-classic MTUs instead of recvmsg truncating them.
            let mut frame_buf = [0u8; CANFD_MTU];
            let mut cmsg_buf = [0u8; 1024];

            let (msg_bytes, msg_flags, timestamp_info, host_rx_mono_us) = {
                let mut iov = [IoSliceMut::new(&mut frame_buf)];

                let msg = match recvmsg::<SockaddrStorage>(
                    fd,
                    &mut iov,
                    Some(&mut cmsg_buf),
                    MsgFlags::empty(),
                ) {
                    Ok(msg) => msg,
                    Err(nix::errno::Errno::EAGAIN) => {
                        return Err(CanError::Timeout);
                    },
                    Err(e) => {
                        return Err(CanError::Io(std::io::Error::other(format!(
                            "recvmsg failed: {}",
                            e
                        ))));
                    },
                };

                let host_rx_mono_us = crate::monotonic_micros();
                let timestamp_info = self.extract_timestamp_from_cmsg(&msg)?;
                (msg.bytes, msg.flags.bits(), timestamp_info, host_rx_mono_us)
            };

            match parse_libc_can_frame_bytes(&frame_buf, msg_bytes, msg_flags) {
                ParsedSocketCanFrame::Data(frame) => {
                    let timestamp_provenance =
                        timestamp_provenance_for_source(timestamp_info.source);
                    let raw_timestamp = RawTimestampInfo {
                        can_id: frame.raw_id(),
                        host_rx_mono_us,
                        system_ts_us: timestamp_info.system_ts_us,
                        hw_trans_us: timestamp_info.hw_trans_us,
                        hw_raw_us: timestamp_info.hw_raw_us,
                    };
                    let raw_sample = RawTimestampSample {
                        iface: self.iface.clone(),
                        info: raw_timestamp,
                    };
                    return Ok(SocketCanReceivedFrame {
                        frame: frame.with_timestamp_us(timestamp_info.timestamp_us),
                        timestamp_source: timestamp_info.source,
                        timestamp_provenance,
                        raw_timestamp: Some(raw_timestamp),
                        raw_sample,
                    });
                },
                ParsedSocketCanFrame::RecoverableNonData => continue,
                ParsedSocketCanFrame::Fatal(error) => return Err(error),
            }
        }
    }

    fn startup_probe_error(reason: impl Into<String>) -> CanError {
        CanError::Device(CanDeviceError::new(
            CanDeviceErrorKind::UnsupportedConfig,
            format!(
                "SocketCAN startup validation failed: {}; strict realtime requires trusted CAN timestamps",
                reason.into()
            ),
        ))
    }
}

impl RxAdapter for SocketCanRxAdapter {
    fn receive(&mut self) -> Result<ReceivedFrame, CanError> {
        if let Some(received) = self.bootstrap_frames.pop_front() {
            return Ok(received);
        }

        self.receive_live(self.read_timeout).map(|received| {
            let mut frame = ReceivedFrame::new(received.frame, received.timestamp_provenance);
            if let Some(raw_timestamp) = received.raw_timestamp {
                frame = frame.with_raw_timestamp(raw_timestamp);
            }
            frame
        })
    }

    fn backend_capability(&self) -> BackendCapability {
        self.backend_capability
    }

    fn startup_probe_until(
        &mut self,
        deadline: Instant,
    ) -> Result<Option<BackendCapability>, CanError> {
        if self.startup_probe_resolved {
            return Ok(Some(self.backend_capability));
        }

        loop {
            let now = Instant::now();
            if now >= deadline {
                return Err(Self::startup_probe_error(
                    "no robot feedback with usable SocketCAN timestamps arrived before the validation deadline",
                ));
            }

            let remaining = deadline.saturating_duration_since(now);
            let timeout = remaining.min(self.read_timeout.max(Duration::from_millis(1)));
            match self.receive_live(timeout) {
                Ok(received) => {
                    let frame = received.frame;
                    if should_buffer_bootstrap_frame(&frame) {
                        let mut buffered = ReceivedFrame::new(frame, received.timestamp_provenance);
                        if let Some(raw_timestamp) = received.raw_timestamp {
                            buffered = buffered.with_raw_timestamp(raw_timestamp);
                        }
                        self.bootstrap_frames.push_back(buffered);
                    }
                    let Some(capability) =
                        classify_startup_probe_frame(&frame, received.timestamp_source)
                    else {
                        continue;
                    };
                    self.backend_capability = capability;
                    self.startup_probe_resolved = true;
                    return Ok(Some(capability));
                },
                Err(CanError::Timeout) => {
                    if Instant::now() >= deadline {
                        return Err(Self::startup_probe_error(
                            "no robot feedback with usable SocketCAN timestamps arrived before the validation deadline",
                        ));
                    }
                },
                Err(error) => return Err(error),
            }
        }
    }
}

impl SocketCanRxAdapter {
    /// 从 CMSG 中提取时间戳（与 SocketCanAdapter 一致）
    ///
    /// 从 `recvmsg` 返回的控制消息（CMSG）中提取硬件/软件时间戳。
    ///
    /// # 参数
    /// - `msg`: `recvmsg` 返回的消息对象，包含 CMSG 控制消息
    ///
    /// # 返回值
    /// - `Ok(TimestampInfo)`: 时间戳（微秒）与来源，如果不可用则返回 `0/None`
    ///
    /// # 时间戳优先级
    /// 1. `timestamps.hw_trans` (Hardware-Transformed) - 首选（硬件时间同步到系统时钟）
    /// 2. `timestamps.system` (Software) - 次选（软件中断时间戳）
    /// 3. `0` - 如果都不可用
    fn extract_timestamp_from_cmsg(
        &mut self,
        msg: &nix::sys::socket::RecvMsg<'_, '_, SockaddrStorage>,
    ) -> Result<TimestampInfo, CanError> {
        if !self.timestamping_enabled {
            return Ok(TimestampInfo {
                timestamp_us: 0,
                source: TimestampSource::None,
                system_ts_us: None,
                hw_trans_us: None,
                hw_raw_us: None,
            });
        }

        // 遍历所有 CMSG
        match msg.cmsgs() {
            Ok(cmsgs) => {
                for cmsg in cmsgs {
                    if let ControlMessageOwned::ScmTimestampsns(timestamps) = cmsg {
                        let system_ts_us = Self::timespec_to_micros_if_nonzero(
                            timestamps.system.tv_sec(),
                            timestamps.system.tv_nsec(),
                        );
                        let hw_trans_us = Self::timespec_to_micros_if_nonzero(
                            timestamps.hw_trans.tv_sec(),
                            timestamps.hw_trans.tv_nsec(),
                        );
                        let hw_raw_us = Self::timespec_to_micros_if_nonzero(
                            timestamps.hw_raw.tv_sec(),
                            timestamps.hw_raw.tv_nsec(),
                        );

                        // ✅ 优先级 1：硬件时间戳（已同步到系统时钟）
                        if let Some(timestamp_us) = hw_trans_us {
                            if !self.hw_timestamp_available {
                                trace!("Hardware timestamp (system-synced) detected and enabled");
                                self.hw_timestamp_available = true;
                            }

                            return Ok(TimestampInfo {
                                timestamp_us,
                                source: TimestampSource::Hardware,
                                system_ts_us,
                                hw_trans_us,
                                hw_raw_us,
                            });
                        }

                        // ✅ 优先级 2：软件时间戳（系统中断时间）
                        if let Some(timestamp_us) = system_ts_us {
                            if !self.hw_timestamp_available {
                                trace!(
                                    "Hardware timestamp not available, using software timestamp"
                                );
                            }

                            return Ok(TimestampInfo {
                                timestamp_us,
                                source: TimestampSource::Software,
                                system_ts_us,
                                hw_trans_us,
                                hw_raw_us,
                            });
                        }

                        if hw_raw_us.is_some() {
                            return Ok(TimestampInfo {
                                timestamp_us: 0,
                                source: TimestampSource::None,
                                system_ts_us,
                                hw_trans_us,
                                hw_raw_us,
                            });
                        }
                    }
                }
            },
            Err(e) => {
                // CMSG 解析失败（如缓冲区截断），返回 0 而非错误
                warn!("Failed to parse CMSG: {}, returning timestamp 0", e);
                return Ok(TimestampInfo {
                    timestamp_us: 0,
                    source: TimestampSource::None,
                    system_ts_us: None,
                    hw_trans_us: None,
                    hw_raw_us: None,
                });
            },
        }

        // 没有找到时间戳
        Ok(TimestampInfo {
            timestamp_us: 0,
            source: TimestampSource::None,
            system_ts_us: None,
            hw_trans_us: None,
            hw_raw_us: None,
        })
    }

    /// 将 timespec (秒+纳秒) 转换为微秒（u64）
    ///
    /// # 参数
    /// - `tv_sec`: 秒数（i64）
    /// - `tv_nsec`: 纳秒数（i64）
    ///
    /// # 返回值
    /// - `u64`: 微秒数（支持绝对时间戳，从 Unix 纪元开始）
    fn timespec_to_micros(tv_sec: i64, tv_nsec: i64) -> u64 {
        (tv_sec as u64) * 1_000_000 + ((tv_nsec as u64) / 1000)
    }

    fn timespec_to_micros_if_nonzero(tv_sec: i64, tv_nsec: i64) -> Option<u64> {
        (tv_sec != 0 || tv_nsec != 0).then(|| Self::timespec_to_micros(tv_sec, tv_nsec))
    }
}

impl Drop for SocketCanRxAdapter {
    fn drop(&mut self) {
        trace!(
            "SocketCanRxAdapter dropped (FD: {})",
            self.socket.as_raw_fd()
        );
        // SocketCAN socket 会自动关闭，无需额外操作
    }
}

/// 只写适配器（用于 TX 线程）
///
/// 独立的 TX 适配器，持有 `CanSocket` 的克隆，
/// 可以在不同线程中与 `SocketCanRxAdapter` 并发使用。
///
/// # 关键设计
/// - **发送超时**：使用 `SO_SNDTIMEO` 实现，避免在总线错误时无限阻塞
/// - **FD 共享**：通过 `try_clone()` 共享同一个打开的文件描述，共享文件状态标志
pub struct SocketCanTxAdapter {
    socket: CanSocket,
    current_write_timeout: Duration,
}

impl SocketCanTxAdapter {
    /// 创建新的 TX 适配器
    ///
    /// # 参数
    /// - `socket`: SocketCAN socket（将被克隆）
    ///
    /// # 错误
    /// - `CanError::Io`: 克隆 socket 或设置写超时失败
    pub fn new(socket: &CanSocket) -> Result<Self, CanError> {
        // 克隆 socket（使用 dup() 系统调用）
        let tx_socket = dup_socket(socket)?;

        // 设置发送超时（5ms，快速失败）
        // 关键：避免 TX 线程在总线错误（Error Passive/Bus Off）或缓冲区满时无限阻塞
        tx_socket.set_write_timeout(Duration::from_millis(5)).map_err(|e| {
            CanError::Io(std::io::Error::other(format!(
                "Failed to set write timeout on TX socket: {}",
                e
            )))
        })?;

        trace!("SocketCanTxAdapter created with 5ms write timeout");

        Ok(Self {
            socket: tx_socket,
            current_write_timeout: Duration::from_millis(5),
        })
    }

    fn set_write_timeout_if_needed(&mut self, timeout: Duration) -> Result<(), CanError> {
        if self.current_write_timeout == timeout {
            return Ok(());
        }

        self.socket.set_write_timeout(timeout).map_err(|e| {
            CanError::Io(std::io::Error::other(format!(
                "Failed to set write timeout on TX socket: {}",
                e
            )))
        })?;
        self.current_write_timeout = timeout;
        Ok(())
    }

    fn build_can_frame(frame: PiperFrame) -> Result<CanFrame, CanError> {
        let payload = &frame.data_padded()[..frame.dlc() as usize];
        match frame.id() {
            CanId::Extended(id) => ExtendedId::new(id.raw())
                .and_then(|id| CanFrame::new(id, payload))
                .ok_or_else(|| {
                    CanError::Device(
                        format!(
                            "Failed to create extended frame with ID 0x{:X}",
                            frame.raw_id()
                        )
                        .into(),
                    )
                }),
            CanId::Standard(id) => StandardId::new(id.raw())
                .and_then(|id| CanFrame::new(id, payload))
                .ok_or_else(|| {
                    CanError::Device(
                        format!(
                            "Failed to create standard frame with ID 0x{:X}",
                            frame.raw_id()
                        )
                        .into(),
                    )
                }),
        }
    }

    fn transmit_with_timeout(
        &mut self,
        frame: PiperFrame,
        timeout: Duration,
    ) -> Result<(), CanError> {
        self.set_write_timeout_if_needed(timeout)?;
        let can_frame = Self::build_can_frame(frame)?;

        self.socket.transmit(&can_frame).map_err(|e| {
            if let socketcan::Error::Io(io_err) = &e
                && (io_err.kind() == std::io::ErrorKind::TimedOut
                    || io_err.kind() == std::io::ErrorKind::WouldBlock)
            {
                return CanError::Timeout;
            }
            CanError::Io(std::io::Error::other(format!(
                "SocketCAN transmit error: {}",
                e
            )))
        })?;

        Ok(())
    }
}

impl RealtimeTxAdapter for SocketCanTxAdapter {
    fn send_control(&mut self, frame: PiperFrame, budget: Duration) -> Result<(), CanError> {
        if budget.is_zero() {
            return Err(CanError::Timeout);
        }
        self.transmit_with_timeout(frame, budget)
    }

    fn send_shutdown_until(
        &mut self,
        frame: PiperFrame,
        deadline: Instant,
    ) -> Result<(), CanError> {
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            return Err(CanError::Timeout);
        };
        self.transmit_with_timeout(frame, remaining)
    }
}

impl Drop for SocketCanTxAdapter {
    fn drop(&mut self) {
        trace!(
            "SocketCanTxAdapter dropped (FD: {})",
            self.socket.as_raw_fd()
        );
        // SocketCAN socket 会自动关闭，无需额外操作
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use piper_protocol::ids::{
        ID_JOINT_FEEDBACK_12, ID_JOINT_FEEDBACK_34, driver_rx_robot_feedback_ids,
    };

    #[test]
    fn test_classify_startup_probe_frame_accepts_hardware_timestamped_robot_feedback() {
        let frame = PiperFrame::new_standard(ID_JOINT_FEEDBACK_12.raw() as u32, [0; 8]).unwrap();
        assert_eq!(
            classify_startup_probe_frame(&frame, TimestampSource::Hardware),
            Some(BackendCapability::StrictRealtime)
        );
    }

    #[test]
    fn test_classify_startup_probe_frame_accepts_software_timestamped_robot_feedback() {
        let frame = PiperFrame::new_standard(ID_JOINT_FEEDBACK_34.raw() as u32, [0; 8]).unwrap();
        assert_eq!(
            classify_startup_probe_frame(&frame, TimestampSource::Software),
            Some(BackendCapability::SoftRealtime)
        );
    }

    #[test]
    fn test_classify_startup_probe_frame_rejects_noise_and_missing_timestamps() {
        let robot_frame =
            PiperFrame::new_standard(ID_JOINT_FEEDBACK_12.raw() as u32, [0; 8]).unwrap();
        let noise_frame = PiperFrame::new_standard(0x7FF, [0; 8]).unwrap();

        assert_eq!(
            classify_startup_probe_frame(&robot_frame, TimestampSource::None),
            None
        );
        assert_eq!(
            classify_startup_probe_frame(&noise_frame, TimestampSource::Hardware),
            None
        );
    }

    #[test]
    fn test_hardware_filter_ids_match_protocol_driver_feedback_surface() {
        assert_eq!(hardware_filter_ids(), driver_rx_robot_feedback_ids());
    }

    #[test]
    fn test_startup_probe_buffers_only_robot_feedback_frames() {
        let robot_frame =
            PiperFrame::new_standard(ID_JOINT_FEEDBACK_12.raw() as u32, [0; 8]).unwrap();
        let noise_frame = PiperFrame::new_standard(0x7FF, [0; 8]).unwrap();

        assert!(should_buffer_bootstrap_frame(&robot_frame));
        assert!(!should_buffer_bootstrap_frame(&noise_frame));
    }

    #[test]
    fn startup_probe_does_not_treat_raw_only_timestamp_as_strict() {
        let frame = PiperFrame::new_standard(ID_JOINT_FEEDBACK_12.raw() as u32, [0; 8]).unwrap();

        assert_eq!(
            classify_startup_probe_frame(&frame, TimestampSource::None),
            None
        );
    }

    #[test]
    fn timestamp_info_with_hw_raw_only_is_not_hardware_source() {
        let info = TimestampInfo {
            timestamp_us: 123,
            source: TimestampSource::Software,
            system_ts_us: Some(123),
            hw_trans_us: None,
            hw_raw_us: Some(100),
        };

        assert_eq!(info.source, TimestampSource::Software);
        assert!(info.hw_raw_us.is_some());
        assert!(info.hw_trans_us.is_none());
    }

    #[test]
    fn received_frame_can_carry_raw_timestamp_without_changing_provenance() {
        let frame = PiperFrame::new_standard(ID_JOINT_FEEDBACK_12.raw() as u32, [0; 8]).unwrap();
        let raw = crate::RawTimestampInfo {
            can_id: ID_JOINT_FEEDBACK_12.raw() as u32,
            host_rx_mono_us: 123,
            system_ts_us: Some(123),
            hw_trans_us: None,
            hw_raw_us: Some(100),
        };

        let received =
            ReceivedFrame::new(frame, TimestampProvenance::Kernel).with_raw_timestamp(raw);

        assert_eq!(received.timestamp_provenance, TimestampProvenance::Kernel);
        assert_eq!(received.raw_timestamp, Some(raw));
    }

    #[test]
    fn socketcan_split_hw_trans_source_maps_to_hardware_provenance() {
        assert_eq!(
            timestamp_provenance_for_source(TimestampSource::Hardware),
            TimestampProvenance::Hardware
        );
        assert_eq!(
            timestamp_provenance_for_source(TimestampSource::Software),
            TimestampProvenance::Kernel
        );
        assert_eq!(
            timestamp_provenance_for_source(TimestampSource::None),
            TimestampProvenance::None
        );
    }
}
