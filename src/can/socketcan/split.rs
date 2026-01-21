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

use crate::can::{CanError, PiperFrame, RxAdapter, TxAdapter};
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use nix::sys::socket::{ControlMessageOwned, MsgFlags, SockaddrStorage, recvmsg};
use socketcan::{
    CanError as SocketCanError, CanErrorFrame, CanFilter, CanFrame, CanSocket, EmbeddedFrame,
    ExtendedId, Frame, StandardId,
};
use std::io::IoSliceMut;
use std::os::fd::BorrowedFd;
use std::os::unix::io::AsRawFd;
use std::time::Duration;
use tracing::{error, trace, warn};

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
    read_timeout: Duration,
    /// 是否启用时间戳（从原始 socket 继承，SocketCanAdapter 初始化时已启用）
    timestamping_enabled: bool,
    /// 是否检测到硬件时间戳支持（运行时检测）
    hw_timestamp_available: bool,
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
        // 克隆 socket（使用 dup() 系统调用）
        let rx_socket = socket.try_clone().map_err(|e| {
            CanError::Io(std::io::Error::other(format!(
                "Failed to clone SocketCAN socket for RX: {}",
                e
            )))
        })?;

        // 设置读超时（使用 SO_RCVTIMEO，避免依赖 O_NONBLOCK）
        rx_socket.set_read_timeout(read_timeout).map_err(|e| {
            CanError::Io(std::io::Error::other(format!(
                "Failed to set read timeout on RX socket: {}",
                e
            )))
        })?;

        // 配置硬件过滤器（降低 CPU 占用）
        Self::configure_hardware_filters(&rx_socket)?;

        // 检查时间戳是否已启用（从原始 socket 继承）
        // 注意：SocketCanAdapter 在初始化时已启用 SO_TIMESTAMPING
        // 由于 dup() 共享 socket 选项，这里假设已启用
        // 实际检查需要查询 socket 选项，但为了简化，我们假设已启用
        // 如果时间戳不可用，提取时会返回 0
        let timestamping_enabled = true; // 假设已启用（SocketCanAdapter 默认启用）
        let hw_timestamp_available = false; // 运行时检测

        Ok(Self {
            socket: rx_socket,
            read_timeout,
            timestamping_enabled,
            hw_timestamp_available,
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
    fn configure_hardware_filters(socket: &CanSocket) -> Result<(), CanError> {
        // 定义需要接收的 CAN ID 列表
        // 这些是机械臂反馈帧的典型 ID 范围（根据实际协议调整）
        let feedback_ids: Vec<u32> = (0x251..=0x256).collect();

        // 创建过滤器（精确匹配）
        let filters: Vec<CanFilter> = feedback_ids
            .iter()
            .map(|&id| {
                // 使用 0x7FF 作为掩码，实现精确匹配（标准帧）
                // 如果需要支持扩展帧，使用 0x1FFFFFFF
                CanFilter::new(id, 0x7FF)
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
            "SocketCAN hardware filters configured: {} IDs (0x251-0x256)",
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
}

impl RxAdapter for SocketCanRxAdapter {
    fn receive(&mut self) -> Result<PiperFrame, CanError> {
        let fd = self.socket.as_raw_fd();

        // 使用 poll 实现超时（与 SocketCanAdapter::receive_with_timestamp 一致）
        let pollfd = PollFd::new(unsafe { BorrowedFd::borrow_raw(fd) }, PollFlags::POLLIN);
        let timeout_ms = self.read_timeout.as_millis().min(65535) as u16;
        match poll(&mut [pollfd], PollTimeout::from(timeout_ms)) {
            Ok(0) => {
                // 超时
                return Err(CanError::Timeout);
            },
            Ok(_) => {
                // 有数据可用，继续
            },
            Err(e) => {
                return Err(CanError::Io(std::io::Error::other(format!(
                    "poll failed: {}",
                    e
                ))));
            },
        }

        // 准备缓冲区（与 SocketCanAdapter 一致）
        const CAN_FRAME_LEN: usize = std::mem::size_of::<libc::can_frame>();
        let mut frame_buf = [0u8; CAN_FRAME_LEN];
        let mut cmsg_buf = [0u8; 1024]; // CMSG 缓冲区

        // 构建 IO 向量
        let mut iov = [IoSliceMut::new(&mut frame_buf)];

        // 调用 recvmsg 获取帧和控制消息（CMSG）
        let msg = match recvmsg::<SockaddrStorage>(
            fd,
            &mut iov,
            Some(&mut cmsg_buf),
            MsgFlags::empty(),
        ) {
            Ok(msg) => msg,
            Err(nix::errno::Errno::EAGAIN) => {
                // 超时（虽然 poll 已检查，但作为防御性编程保留）
                return Err(CanError::Timeout);
            },
            Err(e) => {
                return Err(CanError::Io(std::io::Error::other(format!(
                    "recvmsg failed: {}",
                    e
                ))));
            },
        };

        // 验证数据长度
        if msg.bytes < CAN_FRAME_LEN {
            return Err(CanError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "Incomplete CAN frame: {} bytes (expected at least {})",
                    msg.bytes, CAN_FRAME_LEN
                ),
            )));
        }

        // 先提取时间戳（在解析 CAN 帧之前，避免生命周期冲突）
        let timestamp_us = self.extract_timestamp_from_cmsg(&msg)?;

        // 解析 CAN 帧
        let can_frame = self.parse_raw_can_frame(&frame_buf[..msg.bytes])?;

        // 过滤错误帧（与 SocketCanAdapter 一致）
        if can_frame.is_error_frame() {
            if let Ok(error_frame) = CanErrorFrame::try_from(can_frame) {
                let socketcan_error = SocketCanError::from(error_frame);
                match &socketcan_error {
                    SocketCanError::BusOff => {
                        error!("CAN Bus Off error detected");
                        return Err(CanError::BusOff);
                    },
                    SocketCanError::ControllerProblem(problem) => {
                        let problem_str = format!("{}", problem);
                        if problem_str.contains("overflow") || problem_str.contains("Overflow") {
                            error!("CAN Buffer Overflow detected: {}", problem);
                            return Err(CanError::BufferOverflow);
                        } else {
                            warn!("CAN Controller Problem: {}, ignoring", problem);
                            // 递归调用，尝试接收下一个帧
                            return self.receive();
                        }
                    },
                    _ => {
                        warn!("CAN Error Frame received: {}, ignoring", socketcan_error);
                        // 递归调用，尝试接收下一个帧
                        return self.receive();
                    },
                }
            } else {
                warn!("Received CAN error frame but failed to parse, ignoring");
                // 递归调用，尝试接收下一个帧
                return self.receive();
            }
        }

        // 转换 CanFrame -> PiperFrame
        let piper_frame = PiperFrame {
            id: can_frame.raw_id(),
            data: {
                let mut data = [0u8; 8];
                let frame_data = can_frame.data();
                let len = frame_data.len().min(8);
                data[..len].copy_from_slice(&frame_data[..len]);
                data
            },
            len: can_frame.dlc() as u8,
            is_extended: can_frame.is_extended(),
            timestamp_us, // ✅ 使用提取的时间戳
        };

        trace!(
            "RX: Received CAN frame: ID=0x{:X}, len={}, timestamp_us={}",
            piper_frame.id, piper_frame.len, piper_frame.timestamp_us
        );

        Ok(piper_frame)
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
    /// - `Ok(u64)`: 时间戳（微秒），如果不可用则返回 `0`
    ///
    /// # 时间戳优先级
    /// 1. `timestamps.hw_trans` (Hardware-Transformed) - 首选（硬件时间同步到系统时钟）
    /// 2. `timestamps.system` (Software) - 次选（软件中断时间戳）
    /// 3. `0` - 如果都不可用
    fn extract_timestamp_from_cmsg(
        &mut self,
        msg: &nix::sys::socket::RecvMsg<'_, '_, SockaddrStorage>,
    ) -> Result<u64, CanError> {
        if !self.timestamping_enabled {
            return Ok(0); // 未启用时间戳
        }

        // 遍历所有 CMSG
        match msg.cmsgs() {
            Ok(cmsgs) => {
                for cmsg in cmsgs {
                    if let ControlMessageOwned::ScmTimestampsns(timestamps) = cmsg {
                        // ✅ 优先级 1：硬件时间戳（已同步到系统时钟）
                        let hw_trans_ts = timestamps.hw_trans;
                        if hw_trans_ts.tv_sec() != 0 || hw_trans_ts.tv_nsec() != 0 {
                            if !self.hw_timestamp_available {
                                trace!("Hardware timestamp (system-synced) detected and enabled");
                                self.hw_timestamp_available = true;
                            }

                            let timestamp_us = Self::timespec_to_micros(
                                hw_trans_ts.tv_sec(),
                                hw_trans_ts.tv_nsec(),
                            );
                            return Ok(timestamp_us);
                        }

                        // ✅ 优先级 2：软件时间戳（系统中断时间）
                        let sw_ts = timestamps.system;
                        if sw_ts.tv_sec() != 0 || sw_ts.tv_nsec() != 0 {
                            if !self.hw_timestamp_available {
                                trace!(
                                    "Hardware timestamp not available, using software timestamp"
                                );
                            }

                            let timestamp_us =
                                Self::timespec_to_micros(sw_ts.tv_sec(), sw_ts.tv_nsec());
                            return Ok(timestamp_us);
                        }
                    }
                }
            },
            Err(e) => {
                // CMSG 解析失败（如缓冲区截断），返回 0 而非错误
                warn!("Failed to parse CMSG: {}, returning timestamp 0", e);
                return Ok(0);
            },
        }

        // 没有找到时间戳
        Ok(0)
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

    /// 解析原始 CAN 帧数据（与 SocketCanAdapter 一致）
    ///
    /// 从 `recvmsg` 接收的原始字节数组解析为 `CanFrame`。
    ///
    /// # 参数
    /// - `data`: 原始 CAN 帧数据（`libc::can_frame` 的字节表示）
    ///
    /// # 返回值
    /// - `Ok(CanFrame)`: 成功解析
    /// - `Err(CanError::Io)`: 数据不完整或格式错误
    fn parse_raw_can_frame(&self, data: &[u8]) -> Result<CanFrame, CanError> {
        const CAN_FRAME_LEN: usize = std::mem::size_of::<libc::can_frame>();

        // 验证数据长度
        if data.len() < CAN_FRAME_LEN {
            return Err(CanError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "Incomplete CAN frame data: {} bytes (expected at least {})",
                    data.len(),
                    CAN_FRAME_LEN
                ),
            )));
        }

        // 使用安全的内存拷贝
        let mut raw_frame: libc::can_frame = unsafe { std::mem::zeroed() };
        unsafe {
            std::ptr::copy_nonoverlapping(
                data.as_ptr(),
                &mut raw_frame as *mut _ as *mut u8,
                CAN_FRAME_LEN.min(data.len()),
            );
        }

        // 解析 CAN ID（处理 EFF/RTR/ERR 标志位）
        let can_id = raw_frame.can_id;
        let is_extended = (can_id & libc::CAN_EFF_FLAG) != 0;
        let is_rtr = (can_id & libc::CAN_RTR_FLAG) != 0;

        // 提取实际的 ID（去除标志位）
        let id_bits = if is_extended {
            can_id & libc::CAN_EFF_MASK
        } else {
            can_id & libc::CAN_SFF_MASK
        };

        // 获取数据长度
        let dlc = raw_frame.can_dlc as usize;
        if dlc > 8 {
            return Err(CanError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("Invalid DLC: {} (max 8)", dlc),
            )));
        }

        // 提取数据
        let data_slice = &raw_frame.data[..dlc.min(8)];

        // 构造 socketcan::CanFrame
        if is_rtr {
            // RTR 帧暂不支持
            return Err(CanError::Io(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "RTR frames not yet supported",
            )));
        }

        if is_extended {
            // 扩展帧
            let id = ExtendedId::new(id_bits).ok_or_else(|| {
                CanError::Device(format!("Invalid extended ID: 0x{:X}", id_bits).into())
            })?;
            CanFrame::new(id, data_slice).ok_or_else(|| {
                CanError::Device(format!(
                    "Failed to create extended frame with ID 0x{:X}",
                    id_bits
                ))
                .into()
            })
        } else {
            // 标准帧
            let id = StandardId::new(id_bits as u16).ok_or_else(|| {
                CanError::Device(format!("Invalid standard ID: 0x{:X}", id_bits).into())
            })?;
            CanFrame::new(id, data_slice).ok_or_else(|| {
                CanError::Device(format!(
                    "Failed to create standard frame with ID 0x{:X}",
                    id_bits
                ))
                .into()
            })
        }
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
        let tx_socket = socket.try_clone().map_err(|e| {
            CanError::Io(std::io::Error::other(format!(
                "Failed to clone SocketCAN socket for TX: {}",
                e
            )))
        })?;

        // 设置发送超时（5ms，快速失败）
        // 关键：避免 TX 线程在总线错误（Error Passive/Bus Off）或缓冲区满时无限阻塞
        tx_socket.set_write_timeout(Duration::from_millis(5)).map_err(|e| {
            CanError::Io(std::io::Error::other(format!(
                "Failed to set write timeout on TX socket: {}",
                e
            )))
        })?;

        trace!("SocketCanTxAdapter created with 5ms write timeout");

        Ok(Self { socket: tx_socket })
    }
}

impl TxAdapter for SocketCanTxAdapter {
    fn send(&mut self, frame: PiperFrame) -> Result<(), CanError> {
        // 转换 PiperFrame -> CanFrame
        let can_frame = if frame.is_extended {
            // 扩展帧
            ExtendedId::new(frame.id)
                .and_then(|id| CanFrame::new(id, &frame.data[..frame.len as usize]))
                .ok_or_else(|| {
                    CanError::Device(format!(
                        "Failed to create extended frame with ID 0x{:X}",
                        frame.id
                    ))
                })?
        } else {
            // 标准帧
            StandardId::new(frame.id as u16)
                .and_then(|id| CanFrame::new(id, &frame.data[..frame.len as usize]))
                .ok_or_else(|| {
                    CanError::Device(format!(
                        "Failed to create standard frame with ID 0x{:X}",
                        frame.id
                    ))
                })?
        };

        // 发送（带超时，由 SO_SNDTIMEO 控制）
        self.socket.transmit(&can_frame).map_err(|e| {
            // 检查是否为超时错误
            if e.kind() == std::io::ErrorKind::TimedOut {
                return CanError::Timeout;
            }
            CanError::Io(std::io::Error::other(format!(
                "SocketCAN transmit error: {}",
                e
            )))
        })?;

        trace!("TX: Sent CAN frame: ID=0x{:X}, len={}", frame.id, frame.len);
        Ok(())
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
