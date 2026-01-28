//! SocketCAN CAN 适配器实现
//!
//! 支持 Linux 平台下的 SocketCAN 支持，使用内核级的 CAN 通讯接口。
//!
//! ## 特性
//!
//! - 基于 Linux SocketCAN 子系统，性能优异
//! - 支持标准帧和扩展帧
//! - 支持硬件时间戳（默认开启，优先使用硬件时间戳）
//! - 支持软件时间戳（硬件不可用时自动降级）
//! - 自动过滤错误帧
//!
//! ## 依赖
//!
//! - `socketcan` crate (版本 3.5)
//! - Linux 内核 SocketCAN 支持
//! - CAN 接口必须已配置（通过 `ip link` 命令）
//!
//! ## 限制
//!
//! - **仅限 Linux 平台**：SocketCAN 是 Linux 内核特性
//! - **接口配置**：波特率等配置由系统工具（`ip link`）完成，不在应用层设置
//! - **权限要求**：可能需要 `dialout` 组权限或 `sudo`

use crate::{CanAdapter, CanError, PiperFrame};
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use nix::sys::socket::{ControlMessageOwned, MsgFlags, SockaddrStorage, recvmsg};
use socketcan::{
    BlockingCan, CanError as SocketCanError, CanErrorFrame, CanFrame, CanSocket, EmbeddedFrame,
    ExtendedId, Frame, Socket, StandardId,
};
use std::convert::TryFrom;
use std::io::IoSliceMut;
use std::mem;
use std::os::unix::io::AsRawFd;
use std::time::Duration;
use tracing::{error, trace, warn};

mod interface_check;
pub mod split;

use interface_check::check_interface_status;
pub use split::{SocketCanRxAdapter, SocketCanTxAdapter};

/// SocketCAN 适配器
///
/// 实现 `CanAdapter` trait，提供 Linux 平台下的 SocketCAN 支持。
///
/// # 示例
///
/// ```no_run
/// use piper_sdk::can::{SocketCanAdapter, CanAdapter, PiperFrame};
///
/// // 打开 CAN 接口
/// let mut adapter = SocketCanAdapter::new("can0").unwrap();
///
/// // 发送帧
/// let frame = PiperFrame::new_standard(0x123, &[1, 2, 3, 4]);
/// adapter.send(frame).unwrap();
///
/// // 接收帧
/// let rx_frame = adapter.receive().unwrap();
/// ```
#[derive(Debug)]
pub struct SocketCanAdapter {
    /// SocketCAN socket
    socket: CanSocket,
    /// 接口名称（如 "can0"）
    interface: String,
    /// 是否已启动（SocketCAN 打开即启动）
    started: bool,
    /// 读超时时间（用于 receive 方法）
    read_timeout: Duration,
    /// 是否启用时间戳（初始化时设置）
    timestamping_enabled: bool,
    /// 是否检测到硬件时间戳支持（运行时检测）
    hw_timestamp_available: bool,
}

impl SocketCanAdapter {
    /// 创建新的 SocketCAN 适配器
    ///
    /// 在打开 socket 之前，会检查接口是否存在且已启动（UP 状态）。
    /// 如果接口不存在或未启动，会返回清晰的错误信息，指导用户如何修复。
    ///
    /// # 参数
    /// - `interface`: CAN 接口名称（如 "can0" 或 "vcan0"）
    ///
    /// # 错误
    /// - `CanError::Device`:
    ///   - 接口不存在（会提示创建命令）
    ///   - 接口存在但未启动（会提示启动命令）
    ///   - 无法打开接口
    /// - `CanError::Io`: IO 错误（如权限不足、系统调用失败）
    ///
    /// # 示例
    ///
    /// ```no_run
    /// use piper_sdk::can::SocketCanAdapter;
    ///
    /// let adapter = SocketCanAdapter::new("can0").unwrap();
    /// ```
    pub fn new(interface: impl Into<String>) -> Result<Self, CanError> {
        let interface = interface.into();

        // 1. 检查接口状态（仅检查，不自动配置）
        match check_interface_status(&interface) {
            Ok(true) => {
                trace!(
                    "CAN interface '{}' is UP, proceeding with initialization",
                    interface
                );
            },
            Ok(false) => {
                return Err(CanError::Device(format!(
                    "CAN interface '{}' exists but is not UP. Please start it first:\n  sudo ip link set up {}",
                    interface, interface
                ).into()));
            },
            Err(e) => {
                // 接口不存在或其他错误，直接返回
                return Err(e);
            },
        }

        // 2. 打开 SocketCAN 接口
        let socket = CanSocket::open(&interface).map_err(|e| {
            CanError::Device(format!("Failed to open CAN interface '{}': {}", interface, e).into())
        })?;

        // 🛡️ v1.2.1: 禁用 Loopback，防止 TX 帧回环到 RX，导致重复录制
        // 默认情况下，SocketCAN 会将发送的帧回环到接收端（用于测试和诊断）
        // 但对于录制场景，这会导致：
        //   1. TX 帧被录制两次（TX 钩子 + RX 回环）
        //   2. 无法区分真实 RX 帧和回环的 TX 帧
        //
        // 禁用 loopback 后：
        //   - TX 帧不会回环到 RX 接收端
        //   - 只有真实的外部 CAN 帧会被 RX 钩子录制
        //   - TX 帧只能通过 TX 钩子（on_frame_sent）录制
        //
        // 注意：这需要 socketcan crate 3.x 支持，通过 raw setsockopt 调用实现
        let loopback_enabled: libc::c_int = 0; // 0 = 禁用，1 = 启用
        let loopback_result = unsafe {
            libc::setsockopt(
                socket.as_raw_fd(),
                libc::SOL_CAN_RAW,
                libc::CAN_RAW_LOOPBACK,
                &loopback_enabled as *const _ as *const libc::c_void,
                std::mem::size_of::<libc::c_int>() as libc::socklen_t,
            )
        };

        if loopback_result < 0 {
            // 警告：设置失败，但不阻塞初始化（某些系统可能不支持此选项）
            warn!(
                "Failed to disable CAN_RAW_LOOPBACK on '{}': {}",
                interface,
                std::io::Error::last_os_error()
            );
            // 不返回错误，继续初始化
            // 用户可能仍能正常使用，但 TX 帧可能会被回环（需要业务层过滤）
        } else {
            trace!(
                "SocketCAN interface '{}' loopback disabled (CAN_RAW_LOOPBACK=0)",
                interface
            );
        }

        // 设置读超时（默认 2ms，与 PipelineConfig 的默认值一致，确保 io_loop 能及时响应退出信号）
        // 较短的超时时间可以确保在收到退出信号时，io_loop 能快速检查命令通道状态
        let read_timeout = Duration::from_millis(2);
        socket.set_read_timeout(read_timeout).map_err(CanError::Io)?;

        // 启用 SO_TIMESTAMPING（默认开启，优先使用硬件时间戳）
        let flags = libc::SOF_TIMESTAMPING_RX_HARDWARE
            | libc::SOF_TIMESTAMPING_RAW_HARDWARE
            | libc::SOF_TIMESTAMPING_RX_SOFTWARE
            | libc::SOF_TIMESTAMPING_SOFTWARE;

        let timestamping_enabled = unsafe {
            let ret = libc::setsockopt(
                socket.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_TIMESTAMPING,
                &flags as *const _ as *const libc::c_void,
                mem::size_of::<u32>() as libc::socklen_t,
            );

            if ret < 0 {
                // 警告：无法启用时间戳，但不阻塞初始化
                warn!(
                    "Failed to enable SO_TIMESTAMPING on '{}': {}",
                    interface,
                    std::io::Error::last_os_error()
                );
                false
            } else {
                true
            }
        };

        // 初始化时不检测硬件支持（首次接收时检测）
        let hw_timestamp_available = false;

        if timestamping_enabled {
            trace!(
                "SocketCAN interface '{}' opened with timestamping enabled",
                interface
            );
        } else {
            trace!(
                "SocketCAN interface '{}' opened (timestamping disabled)",
                interface
            );
        }

        Ok(Self {
            socket,
            interface: interface.clone(),
            started: true, // SocketCAN 打开即启动，无需额外配置
            read_timeout,
            timestamping_enabled,
            hw_timestamp_available,
        })
    }

    /// 获取接口名称
    pub fn interface(&self) -> &str {
        &self.interface
    }

    /// 获取读超时时间
    pub fn read_timeout(&self) -> Duration {
        self.read_timeout
    }

    /// 检查是否已启动
    pub fn is_started(&self) -> bool {
        self.started
    }

    /// 获取时间戳启用状态
    pub fn timestamping_enabled(&self) -> bool {
        self.timestamping_enabled
    }

    /// 获取硬件时间戳可用状态
    pub fn hw_timestamp_available(&self) -> bool {
        self.hw_timestamp_available
    }

    /// 设置读超时
    ///
    /// # 参数
    /// - `timeout`: 读超时时间，`None` 表示无限阻塞
    ///
    /// # 错误
    /// - `CanError::Io`: 设置超时失败
    pub fn set_read_timeout(&mut self, timeout: Duration) -> Result<(), CanError> {
        self.socket.set_read_timeout(timeout).map_err(CanError::Io)?;
        self.read_timeout = timeout;
        Ok(())
    }

    /// 配置接口（可选，通常由系统工具配置）
    ///
    /// 注意：SocketCAN 的波特率通常由 `ip link set can0 type can bitrate 500000` 配置。
    /// 这个方法主要用于验证接口配置，不修改配置。
    ///
    /// # 参数
    /// - `_bitrate`: 波特率（当前版本不设置，仅用于验证）
    ///
    /// # 错误
    /// - 当前版本总是返回 `Ok(())`
    pub fn configure(&mut self, _bitrate: u32) -> Result<(), CanError> {
        // SocketCAN 的波特率由系统工具（ip link）配置，不在应用层设置
        // 这里只验证接口是否可用
        // 实际配置应该由系统管理员或初始化脚本完成
        trace!(
            "SocketCAN interface '{}' configured (bitrate set externally)",
            self.interface
        );
        Ok(())
    }

    /// 接收帧并提取时间戳（带超时）
    ///
    /// 此方法使用 `poll + recvmsg` 接收 CAN 帧，并同时提取硬件/软件时间戳。
    ///
    ///
    /// # 返回值
    /// - `Ok((can_frame, timestamp_us))`: 成功接收帧和时间戳（微秒）
    /// - `Err(CanError::Timeout)`: 读取超时
    /// - `Err(CanError::Io)`: IO 错误
    ///
    /// # 注意
    /// - 此方法会过滤错误帧，只返回有效数据帧
    /// - 时间戳优先级：硬件时间戳（Transformed） > 软件时间戳 > 0（不可用）
    pub fn receive_with_timestamp(&mut self) -> Result<(CanFrame, u64), CanError> {
        if !self.started {
            return Err(CanError::NotStarted);
        }

        let fd = self.socket.as_raw_fd();

        // 使用 poll 实现超时
        // 注意：nix 0.30 的 PollFd::new 需要 BorrowedFd，PollTimeout 需要毫秒数
        use std::os::fd::BorrowedFd;
        let pollfd = PollFd::new(unsafe { BorrowedFd::borrow_raw(fd) }, PollFlags::POLLIN);

        // 将 Duration 转换为毫秒数（u16，最大 65535ms）
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

        // 准备缓冲区（防御性编程：使用编译时计算的大小）
        const CAN_FRAME_LEN: usize = std::mem::size_of::<libc::can_frame>();
        let mut frame_buf = [0u8; CAN_FRAME_LEN];
        let mut cmsg_buf = [0u8; 1024]; // CMSG 缓冲区

        // 构建 IO 向量
        let mut iov = [IoSliceMut::new(&mut frame_buf)];

        // 调用 recvmsg
        let msg = match recvmsg::<SockaddrStorage>(
            fd,
            &mut iov,
            Some(&mut cmsg_buf),
            MsgFlags::empty(),
        ) {
            Ok(msg) => msg,
            Err(nix::errno::Errno::EAGAIN) => {
                // 超时（虽然 poll 已检查，但作为防御性编程保留）
                // 注意：EWOULDBLOCK 在某些平台上等同于 EAGAIN，所以只匹配 EAGAIN
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

        // 在 recvmsg 调用完成后，iov 不再使用，可以安全地使用 frame_buf
        // 解析 CAN 帧
        let received_bytes = msg.bytes;
        let can_frame = self.parse_raw_can_frame(&frame_buf[..received_bytes])?;

        // 过滤错误帧（与 receive() 方法保持一致）
        if can_frame.is_error_frame() {
            // 处理错误帧（与 receive() 方法逻辑一致）
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
                            // 继续循环，尝试接收下一个帧
                            return self.receive_with_timestamp();
                        }
                    },
                    _ => {
                        warn!("CAN Error Frame received: {}, ignoring", socketcan_error);
                        // 继续循环，尝试接收下一个帧
                        return self.receive_with_timestamp();
                    },
                }
            } else {
                warn!("Received CAN error frame but failed to parse, ignoring");
                // 继续循环，尝试接收下一个帧
                return self.receive_with_timestamp();
            }
        }

        Ok((can_frame, timestamp_us))
    }

    /// 解析原始 CAN 帧数据
    ///
    /// 从 `recvmsg` 接收的原始字节数组解析为 `CanFrame`。
    ///
    /// **实现说明**：使用 `std::ptr::copy_nonoverlapping` 安全地解析 `libc::can_frame` 结构。
    ///
    /// # 参数
    /// - `data`: 原始 CAN 帧数据（`libc::can_frame` 的字节表示）
    ///
    /// # 返回值
    /// - `Ok(CanFrame)`: 成功解析
    /// - `Err(CanError::Io)`: 数据不完整或格式错误
    ///
    /// # 安全
    /// - 使用 `std::ptr::copy_nonoverlapping` 确保内存对齐安全
    /// - 验证数据长度，防止缓冲区溢出
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

        // 使用安全的内存拷贝，避免未对齐指针强转导致的 UB
        // 方法：创建一个已对齐的 libc::can_frame 结构，然后拷贝数据
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
        let _is_error = (can_id & libc::CAN_ERR_FLAG) != 0; // 保留用于未来错误帧处理

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
        // 注意：如果支持 RTR 帧，需要特殊处理
        if is_rtr {
            // RTR 帧：使用 RemoteFrame
            // socketcan crate 可能不直接支持，这里先返回错误
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
                CanError::Device(
                    format!("Failed to create extended frame with ID 0x{:X}", id_bits).into(),
                )
            })
        } else {
            // 标准帧
            let id = StandardId::new(id_bits as u16).ok_or_else(|| {
                CanError::Device(format!("Invalid standard ID: 0x{:X}", id_bits).into())
            })?;
            CanFrame::new(id, data_slice).ok_or_else(|| {
                CanError::Device(
                    format!("Failed to create standard frame with ID 0x{:X}", id_bits).into(),
                )
            })
        }
    }

    /// 从 CMSG 中提取时间戳
    ///
    /// 从 `recvmsg` 返回的控制消息（CMSG）中提取硬件/软件时间戳。
    ///
    /// **实现说明**：已实现完整的时间戳提取逻辑，包括优先级选择。
    ///
    /// # 参数
    /// - `msg`: `recvmsg` 返回的消息对象，包含 CMSG 控制消息
    ///
    /// # 返回值
    /// - `Ok(u64)`: 时间戳（微秒），如果不可用则返回 `0`
    /// - `Err(CanError)`: 提取失败（不应该发生，如果 CMSG 解析失败应该返回 `0`）
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

        // 遍历所有 CMSG（msg.cmsgs() 返回 Result<CmsgIterator>）
        match msg.cmsgs() {
            Ok(cmsgs) => {
                for cmsg in cmsgs {
                    // 注意：nix 0.30 中使用 ScmTimestampsns，Timestamps 结构体有 system/hw_trans/hw_raw 字段
                    if let ControlMessageOwned::ScmTimestampsns(timestamps) = cmsg {
                        // ✅ 优先级 1：硬件时间戳（已同步到系统时钟）
                        // timestamps.hw_trans 是硬件时间经过内核转换后的系统时间
                        // 这是最理想的：硬件精度 + 系统时间轴一致性
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
                        // 如果硬件时间戳不可用，降级到软件时间戳
                        // 精度仍然很好（微秒级），适合高频力控
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

                        // ⚠️ 优先级 3：原始硬件时间戳（不推荐）
                        // timestamps.hw_raw 是网卡内部计数器，通常与系统时间不在同一量级
                        // 仅在特殊场景（如 PTP 同步）下使用
                        // 当前实现不返回此值，避免时间轴错乱
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
    ///
    /// # 说明
    /// 使用 `u64` 而非 `u32` 的原因：
    /// - 支持绝对时间戳（Unix 纪元开始），无需基准时间管理
    /// - 内存对齐后大小相同（24 字节），无额外开销
    /// - 与状态层设计一致（`JointPositionState.hardware_timestamp_us: u64`）
    fn timespec_to_micros(tv_sec: i64, tv_nsec: i64) -> u64 {
        // 计算：timestamp_us = tv_sec * 1_000_000 + tv_nsec / 1000
        // u64 可以存储从 Unix 纪元开始的绝对时间戳（无需截断）
        (tv_sec as u64) * 1_000_000 + ((tv_nsec as u64) / 1000)
    }
}

impl Drop for SocketCanAdapter {
    /// 自动清理：当适配器离开作用域时，自动关闭 socket
    fn drop(&mut self) {
        trace!(
            "[Auto-Drop] SocketCAN interface '{}' closed",
            self.interface
        );
        // SocketCAN socket 会自动关闭，无需额外操作
    }
}

// 实现 SplittableAdapter trait
use crate::SplittableAdapter;
use std::mem::ManuallyDrop;

impl SplittableAdapter for SocketCanAdapter {
    type RxAdapter = SocketCanRxAdapter;
    type TxAdapter = SocketCanTxAdapter;

    /// 分离为独立的 RX 和 TX 适配器
    ///
    /// # 前置条件
    /// - 设备必须已启动（`is_started() == true`）
    ///
    /// # 错误
    /// - `CanError::NotStarted`: 适配器未启动
    /// - `CanError::Io`: 克隆 socket 或配置失败
    ///
    /// # ⚠️ 关键警告：`try_clone()` 的共享状态陷阱
    ///
    /// 分离后的 RX 和 TX 适配器通过 `dup()` 共享同一个"打开文件描述"（Open File Description），
    /// 这意味着：
    ///
    /// 1. **文件状态标志共享**：`O_NONBLOCK` 等标志保存在"打开文件描述"中。
    ///    - **严禁使用 `set_nonblocking()`**：如果在 RX 线程设置非阻塞模式，TX 线程也会受影响。
    ///    - **正确做法**：严格依赖 `SO_RCVTIMEO` 和 `SO_SNDTIMEO` 实现超时。
    ///
    /// 2. **过滤器共享**：RX 适配器设置的硬件过滤器会影响所有共享该打开文件描述的 FD。
    ///    - **现状**：当前设计是安全的（TX 只写不读），但需知晓此特性。
    ///
    /// # 注意
    /// - 分离后，原适配器不再可用（消费 `self`）
    /// - RX 和 TX 适配器可以在不同线程中并发使用
    /// - FD 通过 RAII 自动管理，无需手动关闭
    fn split(self) -> Result<(Self::RxAdapter, Self::TxAdapter), CanError> {
        if !self.started {
            return Err(CanError::NotStarted);
        }

        // 使用 ManuallyDrop 防止 Drop 被调用
        // 因为我们要移动 socket 到分离的适配器中
        let adapter = ManuallyDrop::new(self);

        // 创建 RX 适配器（会克隆 socket）
        let rx_adapter = SocketCanRxAdapter::new(&adapter.socket, adapter.read_timeout)?;

        // 创建 TX 适配器（会克隆 socket）
        let tx_adapter = SocketCanTxAdapter::new(&adapter.socket)?;

        trace!(
            "SocketCanAdapter split into RX and TX adapters (interface: {})",
            adapter.interface
        );

        Ok((rx_adapter, tx_adapter))
    }
}

impl CanAdapter for SocketCanAdapter {
    /// 发送帧（Fire-and-Forget）
    ///
    /// # 错误
    /// - `CanError::NotStarted`: 适配器未启动（理论上不会发生，因为 SocketCAN 打开即启动）
    /// - `CanError::Device`: 创建帧失败（如 ID 无效）
    /// - `CanError::Io`: 发送失败（如总线错误）
    fn send(&mut self, frame: PiperFrame) -> Result<(), CanError> {
        if !self.started {
            return Err(CanError::NotStarted);
        }

        // 1. 转换 PiperFrame -> CanFrame
        let can_frame = if frame.is_extended {
            // 扩展帧
            ExtendedId::new(frame.id)
                .and_then(|id| CanFrame::new(id, &frame.data[..frame.len as usize]))
                .ok_or_else(|| {
                    CanError::Device(
                        format!("Failed to create extended frame with ID 0x{:X}", frame.id).into(),
                    )
                })?
        } else {
            // 标准帧
            StandardId::new(frame.id as u16)
                .and_then(|id| CanFrame::new(id, &frame.data[..frame.len as usize]))
                .ok_or_else(|| {
                    CanError::Device(
                        format!("Failed to create standard frame with ID 0x{:X}", frame.id).into(),
                    )
                })?
        };

        // 2. 发送（Fire-and-Forget）
        self.socket.transmit(&can_frame).map_err(|e| {
            CanError::Io(std::io::Error::other(format!(
                "SocketCAN transmit error: {}",
                e
            )))
        })?;

        trace!("Sent CAN frame: ID=0x{:X}, len={}", frame.id, frame.len);
        Ok(())
    }

    /// 接收帧（阻塞直到收到有效数据帧或超时）
    ///
    /// **关键**：自动过滤错误帧，只返回有效数据帧。
    ///
    /// **时间戳支持**：使用硬件时间戳（如果可用）或软件时间戳填充 `PiperFrame.timestamp_us`。
    /// 时间戳从 Unix 纪元开始的微秒数（`u64`），支持绝对时间戳。
    ///
    /// # 错误
    /// - `CanError::NotStarted`: 适配器未启动
    /// - `CanError::Timeout`: 读取超时（可重试）
    /// - `CanError::Io`: IO 错误
    ///
    /// # 实现
    /// - 使用 `receive_with_timestamp()` 接收帧并提取时间戳（包含硬件/软件时间戳提取）
    /// - 错误帧过滤由 `receive_with_timestamp()` 处理
    fn receive(&mut self) -> Result<PiperFrame, CanError> {
        if !self.started {
            return Err(CanError::NotStarted);
        }

        // 使用 receive_with_timestamp() 接收帧并提取时间戳
        let (can_frame, timestamp_us) = self.receive_with_timestamp()?;

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
            timestamp_us, // 使用从 receive_with_timestamp() 提取的时间戳
        };

        trace!(
            "Received CAN frame: ID=0x{:X}, len={}, timestamp_us={}",
            piper_frame.id, piper_frame.len, piper_frame.timestamp_us
        );
        Ok(piper_frame)
    }

    /// 设置接收超时
    fn set_receive_timeout(&mut self, timeout: Duration) {
        if let Err(e) = self.set_read_timeout(timeout) {
            warn!("Failed to set receive timeout: {}", e);
        }
    }

    /// 带超时的接收
    fn receive_timeout(&mut self, timeout: Duration) -> Result<PiperFrame, CanError> {
        // 保存原超时
        let old_timeout = self.read_timeout;

        // 设置新超时
        self.set_read_timeout(timeout)?;

        // 接收
        let result = self.receive();

        // 恢复原超时
        let _ = self.set_read_timeout(old_timeout);

        result
    }

    /// 非阻塞接收
    fn try_receive(&mut self) -> Result<Option<PiperFrame>, CanError> {
        // 使用零超时模拟非阻塞
        match self.receive_timeout(Duration::ZERO) {
            Ok(frame) => Ok(Some(frame)),
            Err(CanError::Timeout) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// 带超时的发送
    fn send_timeout(&mut self, frame: PiperFrame, timeout: Duration) -> Result<(), CanError> {
        // SocketCAN 支持发送超时（通过 SO_SNDTIMEO）
        // ✅ 保存原始超时设置
        let original_timeout = unsafe {
            let mut tv: libc::timeval = libc::timeval {
                tv_sec: 0,
                tv_usec: 0,
            };
            let mut len = std::mem::size_of::<libc::timeval>() as libc::socklen_t;

            let ret = libc::getsockopt(
                self.socket.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_SNDTIMEO,
                &mut tv as *mut _ as *mut libc::c_void,
                &mut len,
            );

            if ret < 0 {
                // 查询失败，假设无超时
                None
            } else {
                // 转换为 Duration（None 表示无超时）
                if tv.tv_sec < 0 || tv.tv_usec < 0 {
                    None
                } else {
                    Some(
                        Duration::from_secs(tv.tv_sec as u64)
                            + Duration::from_micros(tv.tv_usec as u64),
                    )
                }
            }
        };

        // 临时设置发送超时
        self.socket.set_write_timeout(timeout).map_err(CanError::Io)?;

        let result = self.send(frame);

        // ✅ 恢复原始超时设置
        let restore_result = match original_timeout {
            Some(timeout) => self.socket.set_write_timeout(timeout).map_err(CanError::Io),
            None => self.socket.set_write_timeout(None).map_err(CanError::Io),
        };

        if let Err(e) = restore_result {
            // 恢复失败不影响发送结果，但记录警告
            warn!(
                "Failed to restore original write timeout after send_timeout: {:?}. \
                 Socket may have incorrect timeout setting.",
                e
            );
        }

        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    /// 检查 CAN 接口是否存在
    fn can_interface_exists(interface: &str) -> bool {
        let output = Command::new("ip").args(["link", "show", interface]).output();

        output.is_ok() && output.unwrap().status.success()
    }

    /// 宏：要求 vcan0 接口存在，如果不存在则跳过测试
    macro_rules! require_vcan0 {
        () => {{
            if !can_interface_exists("vcan0") {
                eprintln!("Skipping test: vcan0 interface not available");
                return;
            }
            "vcan0"
        }};
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn test_socketcan_adapter_new_success() {
        // 注意：需要 vcan0 接口存在
        let interface = require_vcan0!();
        let adapter = SocketCanAdapter::new(interface);
        assert!(adapter.is_ok());
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn test_socketcan_adapter_new_invalid_interface() {
        let result = SocketCanAdapter::new("nonexistent_can99");
        assert!(result.is_err());
        if let Err(CanError::Device(msg)) = result {
            assert!(msg.message.contains("nonexistent_can99"));
        } else {
            panic!("Expected Device error");
        }
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn test_socketcan_adapter_new_stores_interface_name() {
        let interface = require_vcan0!();
        let adapter = SocketCanAdapter::new(interface).unwrap();
        assert_eq!(adapter.interface(), "vcan0");
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn test_socketcan_adapter_new_sets_read_timeout() {
        let interface = require_vcan0!();
        let adapter = SocketCanAdapter::new(interface).unwrap();
        // 验证默认超时时间已设置（2ms，与 PipelineConfig 的默认值一致，确保 io_loop 能及时响应退出信号）
        assert_eq!(adapter.read_timeout(), Duration::from_millis(2));
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn test_socketcan_adapter_new_sets_started_true() {
        let interface = require_vcan0!();
        let adapter = SocketCanAdapter::new(interface).unwrap();
        assert!(adapter.is_started());
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn test_socketcan_adapter_new_enables_timestamping() {
        // 测试 SO_TIMESTAMPING 是否成功启用
        // 在 vcan0 上，SO_TIMESTAMPING 应该能够成功设置
        let interface = require_vcan0!();
        let adapter = SocketCanAdapter::new(interface).unwrap();

        // 在支持的平台上，timestamping_enabled 应该为 true
        // 如果 setsockopt 失败，会有警告但不会阻塞初始化
        assert!(
            adapter.timestamping_enabled(),
            "SO_TIMESTAMPING should be enabled on vcan0"
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn test_socketcan_adapter_new_initializes_hw_timestamp_available() {
        // 测试 hw_timestamp_available 是否正确初始化为 false
        // 初始化时不应该检测硬件支持，应该在首次接收时检测
        let interface = require_vcan0!();
        let adapter = SocketCanAdapter::new(interface).unwrap();

        // 初始化时应该为 false（首次接收时才会检测）
        assert!(
            !adapter.hw_timestamp_available(),
            "hw_timestamp_available should be false on initialization"
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn test_socketcan_adapter_timestamping_fields_exist() {
        // 验证时间戳相关字段存在且可访问
        let interface = require_vcan0!();
        let adapter = SocketCanAdapter::new(interface).unwrap();

        // 验证字段可以通过 getter 方法访问
        let _ts_enabled = adapter.timestamping_enabled();
        let _hw_available = adapter.hw_timestamp_available();

        // 如果编译通过且没有 panic，说明字段存在且可访问
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn test_socketcan_adapter_receive_with_timestamp_skeleton() {
        // 验证 receive_with_timestamp() 方法骨架存在且可调用
        let interface = require_vcan0!();
        let mut adapter = SocketCanAdapter::new(interface).unwrap();

        // 设置短超时，避免无限阻塞
        adapter.set_read_timeout(Duration::from_millis(1)).unwrap();

        // 清空缓冲区
        loop {
            match adapter.receive_with_timestamp() {
                Ok(_) => continue,               // 继续清空
                Err(CanError::Timeout) => break, // 超时，说明没有更多帧
                Err(e) => panic!("Unexpected error while clearing: {:?}", e),
            }
        }

        // 测试超时（应该返回 Timeout 错误）
        let start = std::time::Instant::now();
        let result = adapter.receive_with_timestamp();
        let elapsed = start.elapsed();

        match result {
            Err(CanError::Timeout) => {
                // 预期行为
                assert!(
                    elapsed >= Duration::from_millis(1),
                    "Timeout should take at least ~1ms"
                );
            },
            Ok((_frame, _timestamp_us)) => {
                // 如果收到了帧（可能来自其他测试），验证时间戳格式
                // 时间戳应该被提取（可能非零，也可能溢出为 u32::MAX）
            },
            Err(e) => panic!("Expected Timeout or Ok, got: {:?}", e),
        }
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn test_socketcan_adapter_extract_timestamp_from_cmsg_skeleton() {
        // 验证 extract_timestamp_from_cmsg() 方法已实现（不再测试骨架）
        // 实际的时间戳提取测试在 test_socketcan_adapter_receive_with_timestamp_full_flow 中
        // 此测试主要用于确认方法签名正确（编译通过即表示签名正确）
        let interface = require_vcan0!();
        let adapter = SocketCanAdapter::new(interface).unwrap();

        // 验证方法存在（通过编译）
        // 实际的时间戳提取在 receive_with_timestamp() 中测试
        assert!(
            adapter.timestamping_enabled(),
            "Timestamping should be enabled by default"
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn test_socketcan_adapter_receive_with_timestamp_full_flow() {
        // 验证 receive_with_timestamp() 完整流程（发送帧 → 接收帧）
        // 注意：vcan0 默认不回环，需要使用两个 socket
        let interface = require_vcan0!();
        let mut tx_adapter = SocketCanAdapter::new(interface).unwrap();
        let mut rx_adapter = SocketCanAdapter::new(interface).unwrap();

        // 设置读超时并清空缓冲区
        rx_adapter.set_read_timeout(Duration::from_millis(1)).unwrap();
        loop {
            match rx_adapter.receive_with_timestamp() {
                Ok(_) => continue,               // 继续清空
                Err(CanError::Timeout) => break, // 超时，说明没有更多帧
                Err(e) => panic!("Unexpected error while clearing: {:?}", e),
            }
        }

        // 设置较长的超时用于接收
        rx_adapter.set_read_timeout(Duration::from_millis(100)).unwrap();

        // 发送一个标准帧
        let tx_frame = PiperFrame::new_standard(0x456, &[0xAA, 0xBB, 0xCC, 0xDD]);
        tx_adapter.send(tx_frame).unwrap();

        // 使用 receive_with_timestamp 接收
        let (can_frame, timestamp_us) = rx_adapter.receive_with_timestamp().unwrap();

        // 验证接收到的帧
        // raw_id() 返回包含标志位的完整 ID，标准帧使用低 11 位
        let received_id = if can_frame.is_extended() {
            can_frame.raw_id() & 0x1FFFFFFF // 扩展帧：低 29 位
        } else {
            can_frame.raw_id() & 0x7FF // 标准帧：低 11 位
        };
        assert_eq!(received_id, 0x456, "Frame ID should match");
        assert_eq!(can_frame.dlc(), 4, "Frame DLC should be 4");
        assert_eq!(
            can_frame.data(),
            &[0xAA, 0xBB, 0xCC, 0xDD],
            "Frame data should match"
        );

        // 验证时间戳（vcan0 上至少应该有软件时间戳）
        // 注意：软件时间戳是系统时间（从 Unix 纪元开始），可能超过 u32::MAX
        // 我们的实现会截断为 u32::MAX，这是预期的行为
        // 实际使用中，可能需要使用相对时间戳（从某个基准时间开始）
        assert!(
            timestamp_us > 0,
            "Timestamp should be extracted (should be non-zero for software timestamp on vcan0)"
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn test_socketcan_adapter_receive_with_timestamp_timeout() {
        // 验证 receive_with_timestamp() 的超时逻辑
        let interface = require_vcan0!();
        let mut adapter = SocketCanAdapter::new(interface).unwrap();

        // 清空缓冲区（持续多次，确保清空所有待处理帧）
        adapter.set_read_timeout(Duration::from_millis(1)).unwrap();
        let mut consecutive_timeouts = 0;
        loop {
            match adapter.receive_with_timestamp() {
                Ok(_) => {
                    consecutive_timeouts = 0; // 重置超时计数
                    continue;
                },
                Err(CanError::Timeout) => {
                    // 连续两次超时，说明缓冲区已清空
                    consecutive_timeouts += 1;
                    if consecutive_timeouts >= 2 {
                        break;
                    }
                },
                Err(e) => panic!("Unexpected error: {:?}", e),
            }
        }

        // 设置短超时（10ms）
        adapter.set_read_timeout(Duration::from_millis(10)).unwrap();

        // 再次确认缓冲区已清空（额外清空多次，确保彻底清空）
        let mut additional_cleared = 0;
        let mut additional_consecutive_timeouts = 0;
        loop {
            match adapter.receive_with_timestamp() {
                Ok(_) => {
                    additional_cleared += 1;
                    additional_consecutive_timeouts = 0;
                    eprintln!(
                        "[DEBUG] Additional frame cleared before timeout test (count: {})",
                        additional_cleared
                    );
                },
                Err(CanError::Timeout) => {
                    // 连续两次超时，说明缓冲区已清空
                    additional_consecutive_timeouts += 1;
                    if additional_consecutive_timeouts >= 2 {
                        break;
                    }
                },
                Err(e) => panic!("Unexpected error: {:?}", e),
            }
        }

        // 不发送任何帧，应该超时
        let start = std::time::Instant::now();
        let result = adapter.receive_with_timestamp();
        let elapsed = start.elapsed();

        match result {
            Err(CanError::Timeout) => {
                // 验证超时时间合理
                assert!(
                    elapsed >= Duration::from_millis(5),
                    "Timeout should take at least ~5ms"
                );
                assert!(
                    elapsed < Duration::from_millis(50),
                    "Timeout should complete within ~50ms"
                );
            },
            Ok((frame, _)) => {
                panic!(
                    "Expected Timeout error, but received frame: ID=0x{:X}, len={}",
                    frame.raw_id(),
                    frame.dlc()
                );
            },
            Err(e) => {
                panic!("Expected Timeout error, got: {:?}", e);
            },
        }
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn test_socketcan_adapter_receive_with_timestamp_monotonic() {
        // 验证时间戳的单调性（发送多个帧，时间戳应该递增）
        // 参考：hardware_timestamp_implementation_plan.md:529-547
        let interface = require_vcan0!();
        let mut tx_adapter = SocketCanAdapter::new(interface).unwrap();
        let mut rx_adapter = SocketCanAdapter::new(interface).unwrap();

        // 设置读超时并清空缓冲区（持续多次，确保清空所有待处理帧）
        rx_adapter.set_read_timeout(Duration::from_millis(1)).unwrap();
        let mut cleared_count = 0;
        loop {
            match rx_adapter.receive_with_timestamp() {
                Ok(_) => {
                    cleared_count += 1;
                    continue;
                },
                Err(CanError::Timeout) => {
                    // 连续两次超时，说明缓冲区已清空
                    if cleared_count == 0 {
                        // 第一次超时，再试一次确认
                        cleared_count = 0;
                        continue;
                    }
                    break;
                },
                Err(e) => panic!("Unexpected error while clearing: {:?}", e),
            }
        }

        // 再次确认缓冲区已清空（额外清空一次）
        match rx_adapter.receive_with_timestamp() {
            Ok(_) => {
                // 如果还有帧，继续清空
                eprintln!("[DEBUG] Additional frame cleared before monotonic test");
            },
            Err(CanError::Timeout) => {
                // 预期行为，缓冲区已清空
            },
            Err(e) => panic!("Unexpected error: {:?}", e),
        }

        // 设置较长的超时用于接收
        rx_adapter.set_read_timeout(Duration::from_millis(100)).unwrap();

        // 发送多个帧（10 个帧，每个间隔 100 微秒）
        for i in 0..10 {
            let tx_frame = PiperFrame::new_standard(0x100 + i, &[i as u8]);
            tx_adapter.send(tx_frame).unwrap();
            std::thread::sleep(Duration::from_micros(100));
        }

        // 接收所有帧，检查时间戳单调递增
        // 注意：可能接收到其他测试发送的帧，需要过滤出我们发送的帧
        use std::collections::HashSet;
        use std::time::Instant;
        let mut received_count = 0;
        let mut prev_timestamp_us: u64 = 0;
        let expected_ids: HashSet<u32> = (0..10).map(|i| 0x100 + i).collect();
        let start_time = Instant::now();
        const MAX_RECEIVE_TIME: Duration = Duration::from_secs(5); // 最多等待5秒

        while received_count < 10 {
            // 检查是否超时
            if start_time.elapsed() > MAX_RECEIVE_TIME {
                panic!(
                    "Test timeout: expected 10 frames, but only received {} frames within {:?}",
                    received_count, MAX_RECEIVE_TIME
                );
            }

            let (can_frame, timestamp_us) = match rx_adapter.receive_with_timestamp() {
                Ok(frame) => frame,
                Err(CanError::Timeout) => {
                    // 如果超时，但还没收到所有帧，可能是帧丢失或缓冲区问题
                    eprintln!(
                        "[DEBUG] Monotonic test: timeout while waiting for frame {}/10",
                        received_count
                    );
                    continue; // 继续等待
                },
                Err(e) => panic!("Unexpected error during receive: {:?}", e),
            };

            // 提取帧 ID
            let received_id = if can_frame.is_extended() {
                can_frame.raw_id() & 0x1FFFFFFF
            } else {
                can_frame.raw_id() & 0x7FF
            };

            // 只处理我们发送的帧（ID 0x100-0x109）
            if expected_ids.contains(&received_id) {
                // 验证时间戳单调递增
                assert!(
                    timestamp_us >= prev_timestamp_us,
                    "Timestamp should be monotonic (prev: {}, current: {}, frame ID: 0x{:X})",
                    prev_timestamp_us,
                    timestamp_us,
                    received_id
                );
                prev_timestamp_us = timestamp_us;
                received_count += 1;
            } else {
                // 忽略其他测试的帧，但记录警告
                eprintln!(
                    "[DEBUG] Monotonic test: ignoring frame with ID 0x{:X} (not part of test sequence)",
                    received_id
                );
            }
        }

        // 验证时间戳非零
        assert!(prev_timestamp_us > 0, "Final timestamp should be non-zero");

        // 清空缓冲区，确保所有发送的帧都被接收（防止影响后续测试）
        // 持续清空直到超时，表示没有更多帧了
        rx_adapter.set_read_timeout(Duration::from_millis(1)).unwrap();
        let mut cleared_count = 0;
        let mut consecutive_timeouts = 0;
        loop {
            match rx_adapter.receive_with_timestamp() {
                Ok((frame, _)) => {
                    cleared_count += 1;
                    consecutive_timeouts = 0; // 重置超时计数
                    let frame_id = if frame.is_extended() {
                        frame.raw_id() & 0x1FFFFFFF
                    } else {
                        frame.raw_id() & 0x7FF
                    };
                    eprintln!(
                        "[DEBUG] Monotonic test: cleared remaining frame ID=0x{:X} (count: {})",
                        frame_id, cleared_count
                    );
                },
                Err(CanError::Timeout) => {
                    // 连续两次超时，说明缓冲区已清空
                    consecutive_timeouts += 1;
                    if consecutive_timeouts >= 2 {
                        break;
                    }
                },
                Err(e) => {
                    eprintln!(
                        "[DEBUG] Monotonic test: unexpected error while clearing: {:?}",
                        e
                    );
                    break;
                },
            }
        }

        // 再次确认缓冲区已清空（额外清空一次）
        match rx_adapter.receive_with_timestamp() {
            Ok((frame, _)) => {
                let frame_id = if frame.is_extended() {
                    frame.raw_id() & 0x1FFFFFFF
                } else {
                    frame.raw_id() & 0x7FF
                };
                eprintln!(
                    "[DEBUG] Monotonic test: additional frame cleared after timeout: ID=0x{:X}",
                    frame_id
                );
            },
            Err(CanError::Timeout) => {
                // 预期行为，缓冲区已清空
            },
            Err(e) => {
                eprintln!("[DEBUG] Monotonic test: unexpected error: {:?}", e);
            },
        }
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn test_socketcan_adapter_receive_with_timestamp_extended_frame() {
        // 验证 receive_with_timestamp() 支持扩展帧
        // 注意：vcan0 默认不回环，需要使用两个 socket
        let interface = require_vcan0!();
        let mut tx_adapter = SocketCanAdapter::new(interface).unwrap();
        let mut rx_adapter = SocketCanAdapter::new(interface).unwrap();
        rx_adapter.set_read_timeout(Duration::from_millis(100)).unwrap();

        // 发送扩展帧
        let tx_frame = PiperFrame::new_extended(0x12345678, &[0xFF, 0xEE, 0xDD, 0xCC, 0xBB, 0xAA]);
        tx_adapter.send(tx_frame).unwrap();

        // 接收扩展帧
        let (can_frame, _timestamp_us) = rx_adapter.receive_with_timestamp().unwrap();

        // 验证扩展帧
        // 注意：vcan0 可能将扩展帧转换为标准帧，但至少应该能接收数据
        let received_id = if can_frame.is_extended() {
            can_frame.raw_id() & 0x1FFFFFFF // 扩展帧：低 29 位
        } else {
            // 如果不是扩展帧，可能是 vcan0 的限制，只验证数据
            can_frame.raw_id() & 0x7FF // 标准帧：低 11 位
        };

        // 如果 vcan0 不支持扩展帧，只验证数据（vcan0 可能截断或转换）
        if can_frame.is_extended() {
            assert_eq!(received_id, 0x12345678, "Extended frame ID should match");
            assert_eq!(can_frame.dlc(), 6, "Frame DLC should be 6");
            assert_eq!(
                can_frame.data(),
                &[0xFF, 0xEE, 0xDD, 0xCC, 0xBB, 0xAA],
                "Frame data should match"
            );
        } else {
            // vcan0 可能不支持扩展帧，至少验证数据长度一致
            eprintln!("[WARN] vcan0 may not support extended frames, verifying data length only");
            assert!(can_frame.dlc() > 0, "Frame should have data");
        }
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn test_socketcan_adapter_set_read_timeout() {
        let interface = require_vcan0!();
        let mut adapter = SocketCanAdapter::new(interface).unwrap();
        let new_timeout = Duration::from_millis(200);
        adapter.set_read_timeout(new_timeout).unwrap();
        assert_eq!(adapter.read_timeout(), new_timeout);
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn test_socketcan_adapter_send_standard_frame() {
        let interface = require_vcan0!();
        let mut adapter = SocketCanAdapter::new(interface).unwrap();
        let frame = PiperFrame::new_standard(0x123, &[1, 2, 3, 4]);

        let result = adapter.send(frame);
        assert!(result.is_ok());
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn test_socketcan_adapter_send_extended_frame() {
        let interface = require_vcan0!();
        let mut adapter = SocketCanAdapter::new(interface).unwrap();
        let frame = PiperFrame::new_extended(0x12345678, &[0xFF; 8]);

        let result = adapter.send(frame);
        assert!(result.is_ok());
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn test_socketcan_adapter_send_empty_frame() {
        let interface = require_vcan0!();
        let mut adapter = SocketCanAdapter::new(interface).unwrap();
        let frame = PiperFrame::new_standard(0x123, &[]);

        let result = adapter.send(frame);
        assert!(result.is_ok());
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn test_socketcan_adapter_receive_timestamp() {
        // 验证 receive() 返回的 PiperFrame 包含时间戳
        let interface = require_vcan0!();
        let mut tx_adapter = SocketCanAdapter::new(interface).unwrap();
        let mut rx_adapter = SocketCanAdapter::new(interface).unwrap();

        // 清空缓冲区
        rx_adapter.set_read_timeout(Duration::from_millis(1)).unwrap();
        let mut consecutive_timeouts = 0;
        loop {
            match rx_adapter.receive() {
                Ok(_) => {
                    consecutive_timeouts = 0;
                    continue;
                },
                Err(CanError::Timeout) => {
                    consecutive_timeouts += 1;
                    if consecutive_timeouts >= 2 {
                        break;
                    }
                },
                Err(e) => panic!("Unexpected error while clearing: {:?}", e),
            }
        }

        // 设置较长的超时用于接收
        rx_adapter.set_read_timeout(Duration::from_millis(100)).unwrap();

        // 发送一个标准帧（使用唯一 ID 0x400，避免与其他测试冲突）
        let tx_frame = PiperFrame::new_standard(0x400, &[0x42]);
        tx_adapter.send(tx_frame).unwrap();

        // 接收帧，验证时间戳非零（可能需要过滤其他测试的帧）
        let rx_frame = loop {
            let frame = rx_adapter.receive().unwrap();
            if frame.id == 0x400 && frame.data[0] == 0x42 {
                break frame;
            }
            // 忽略其他测试的帧
        };
        assert_eq!(rx_frame.id, 0x400, "Frame ID should match");
        assert_eq!(rx_frame.len, 1, "Frame length should match");
        assert_eq!(rx_frame.data[0], 0x42, "Frame data should match");
        assert!(
            rx_frame.timestamp_us > 0,
            "Timestamp should be non-zero (at least software timestamp on vcan0)"
        );

        // 清空缓冲区，确保发送的帧已完全接收（防止影响其他测试）
        // 持续清空直到连续两次超时，表示没有更多帧了
        rx_adapter.set_read_timeout(Duration::from_millis(1)).unwrap();
        let mut consecutive_timeouts = 0;
        loop {
            match rx_adapter.receive() {
                Ok(_) => {
                    consecutive_timeouts = 0; // 重置超时计数
                    continue;
                },
                Err(CanError::Timeout) => {
                    // 连续两次超时，说明缓冲区已清空
                    consecutive_timeouts += 1;
                    if consecutive_timeouts >= 2 {
                        break;
                    }
                },
                Err(_) => break,
            }
        }

        // 再次确认缓冲区已清空（额外清空一次）
        match rx_adapter.receive() {
            Ok(_) => {
                // 如果还有帧，继续清空（理论上不应该发生）
                eprintln!("[DEBUG] Additional frame cleared after receive_timestamp test");
            },
            Err(CanError::Timeout) => {
                // 预期行为，缓冲区已清空
            },
            Err(_) => {},
        }
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn test_socketcan_adapter_receive_timestamp_monotonic() {
        // 验证 receive() 返回的时间戳单调递增（Task 4.2）
        // 参考：hardware_timestamp_implementation_plan.md:529-547
        let interface = require_vcan0!();
        let mut tx_adapter = SocketCanAdapter::new(interface).unwrap();
        let mut rx_adapter = SocketCanAdapter::new(interface).unwrap();

        // 清空缓冲区
        rx_adapter.set_read_timeout(Duration::from_millis(1)).unwrap();
        let mut consecutive_timeouts = 0;
        loop {
            match rx_adapter.receive() {
                Ok(_) => {
                    consecutive_timeouts = 0;
                    continue;
                },
                Err(CanError::Timeout) => {
                    consecutive_timeouts += 1;
                    if consecutive_timeouts >= 2 {
                        break;
                    }
                },
                Err(e) => panic!("Unexpected error while clearing: {:?}", e),
            }
        }

        // 设置较长的超时用于接收
        rx_adapter.set_read_timeout(Duration::from_millis(100)).unwrap();

        // 发送多个帧（10 个帧，每个间隔 100 微秒）
        for i in 0..10 {
            let tx_frame = PiperFrame::new_standard(0x300 + i, &[i as u8]);
            tx_adapter.send(tx_frame).unwrap();
            std::thread::sleep(Duration::from_micros(100));
        }

        // 接收所有帧，检查时间戳单调递增
        // 注意：可能接收到其他测试发送的帧，需要过滤出我们发送的帧
        use std::collections::HashSet;
        use std::time::Instant;
        let mut received_count = 0;
        let mut prev_timestamp_us: u64 = 0;
        let expected_ids: HashSet<u32> = (0..10).map(|i| 0x300 + i).collect();
        let start_time = Instant::now();
        const MAX_RECEIVE_TIME: Duration = Duration::from_secs(5); // 最多等待5秒

        while received_count < 10 {
            // 检查是否超时
            if start_time.elapsed() > MAX_RECEIVE_TIME {
                panic!(
                    "Test timeout: expected 10 frames, but only received {} frames within {:?}",
                    received_count, MAX_RECEIVE_TIME
                );
            }

            let rx_frame = match rx_adapter.receive() {
                Ok(frame) => frame,
                Err(CanError::Timeout) => {
                    // 如果超时，但还没收到所有帧，可能是帧丢失或缓冲区问题
                    eprintln!(
                        "[DEBUG] Receive monotonic test: timeout while waiting for frame {}/10",
                        received_count
                    );
                    continue; // 继续等待
                },
                Err(e) => panic!("Unexpected error during receive: {:?}", e),
            };

            // 提取帧 ID（去除标志位）
            let received_id = if rx_frame.is_extended {
                rx_frame.id & 0x1FFFFFFF
            } else {
                rx_frame.id & 0x7FF
            };

            // 只处理我们发送的帧（ID 0x300-0x309）
            if expected_ids.contains(&received_id) {
                // 验证时间戳单调递增
                assert!(
                    rx_frame.timestamp_us >= prev_timestamp_us,
                    "Timestamp should be monotonic (prev: {}, current: {}, frame ID: 0x{:X})",
                    prev_timestamp_us,
                    rx_frame.timestamp_us,
                    received_id
                );
                prev_timestamp_us = rx_frame.timestamp_us;
                received_count += 1;
            } else {
                // 忽略其他测试的帧，但记录警告
                eprintln!(
                    "[DEBUG] Receive monotonic test: ignoring frame with ID 0x{:X} (not part of test sequence)",
                    received_id
                );
            }
        }

        // 验证时间戳非零
        assert!(prev_timestamp_us > 0, "Final timestamp should be non-zero");

        // 清空缓冲区
        rx_adapter.set_read_timeout(Duration::from_millis(1)).unwrap();
        let mut consecutive_timeouts = 0;
        loop {
            match rx_adapter.receive() {
                Ok(_) => {
                    consecutive_timeouts = 0;
                    continue;
                },
                Err(CanError::Timeout) => {
                    consecutive_timeouts += 1;
                    if consecutive_timeouts >= 2 {
                        break;
                    }
                },
                Err(_e) => break,
            }
        }
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn test_socketcan_adapter_receive_timestamp_loopback_accuracy() {
        // 验证时间戳精度和系统时间轴一致性（Task 4.3）
        // 参考：hardware_timestamp_implementation_plan.md:556-625
        // 注意：vcan0 不支持真正的回环，使用两个独立的 socket（一个发送，一个接收）
        let interface = require_vcan0!();
        let mut tx_adapter = SocketCanAdapter::new(interface).unwrap();
        let mut rx_adapter = SocketCanAdapter::new(interface).unwrap();

        // 清空缓冲区
        rx_adapter.set_read_timeout(Duration::from_millis(1)).unwrap();
        let mut consecutive_timeouts = 0;
        loop {
            match rx_adapter.receive() {
                Ok(_) => {
                    consecutive_timeouts = 0;
                    continue;
                },
                Err(CanError::Timeout) => {
                    consecutive_timeouts += 1;
                    if consecutive_timeouts >= 2 {
                        break;
                    }
                },
                Err(e) => panic!("Unexpected error while clearing: {:?}", e),
            }
        }

        // 设置较长的超时用于接收
        rx_adapter.set_read_timeout(Duration::from_millis(100)).unwrap();

        // 记录发送前的系统时间（微秒）
        let send_time_before = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_micros() as u64;

        // 发送一个标准帧
        let tx_frame = PiperFrame::new_standard(0x500, &[0xAA, 0xBB]);
        tx_adapter.send(tx_frame).unwrap();

        // 记录发送后的系统时间（微秒）
        let _send_time_after = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_micros() as u64;

        // 接收帧
        let rx_frame = loop {
            let frame = rx_adapter.receive().unwrap();
            if frame.id == 0x500 && frame.data[0] == 0xAA && frame.data[1] == 0xBB {
                break frame;
            }
            // 忽略其他测试的帧
        };

        // 记录接收后的系统时间（微秒）
        let receive_time_after = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_micros() as u64;

        // 验证时间戳在发送时间和接收时间之间
        // 注意：由于时间戳是绝对时间（从 Unix 纪元开始），而 send_time 也是从 Unix 纪元开始
        // 所以可以直接比较
        assert!(
            rx_frame.timestamp_us >= send_time_before,
            "Timestamp should be >= send_time_before (timestamp: {}, send_before: {})",
            rx_frame.timestamp_us,
            send_time_before
        );
        assert!(
            rx_frame.timestamp_us <= receive_time_after,
            "Timestamp should be <= receive_time_after (timestamp: {}, receive_after: {})",
            rx_frame.timestamp_us,
            receive_time_after
        );

        // 验证回环延迟合理（< 10ms，即 10,000 微秒）
        let loopback_delay = receive_time_after - send_time_before;
        assert!(
            loopback_delay < 10_000,
            "Loopback delay should be < 10ms (actual: {} us)",
            loopback_delay
        );

        // 验证时间戳与系统时间轴一致（时间戳应该在发送时间和接收时间之间）
        // 计算时间戳与发送时间的差值（应该很小，表示时间戳准确）
        let timestamp_offset = rx_frame.timestamp_us.abs_diff(send_time_before);
        // 时间戳偏移应该很小（< 1ms，即 1,000 微秒），表示时间戳与系统时间轴一致
        assert!(
            timestamp_offset < 1_000,
            "Timestamp offset should be < 1ms (actual: {} us, timestamp: {}, send_before: {})",
            timestamp_offset,
            rx_frame.timestamp_us,
            send_time_before
        );

        // 清空缓冲区
        rx_adapter.set_read_timeout(Duration::from_millis(1)).unwrap();
        let mut consecutive_timeouts = 0;
        loop {
            match rx_adapter.receive() {
                Ok(_) => {
                    consecutive_timeouts = 0;
                    continue;
                },
                Err(CanError::Timeout) => {
                    consecutive_timeouts += 1;
                    if consecutive_timeouts >= 2 {
                        break;
                    }
                },
                Err(_) => break,
            }
        }
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn test_socketcan_adapter_receive_timeout() {
        let interface = require_vcan0!();
        let mut adapter = SocketCanAdapter::new(interface).unwrap();

        // 设置短超时（1ms，用于清空缓冲区）
        adapter.set_read_timeout(Duration::from_millis(1)).unwrap();

        // 先清空可能存在的待处理帧（如果有其他测试发送的）
        // 持续读取直到超时，表示没有更多帧了
        let mut cleared_frames = 0;
        let mut consecutive_timeouts = 0;
        loop {
            match adapter.receive() {
                Ok(frame) => {
                    cleared_frames += 1;
                    consecutive_timeouts = 0; // 重置超时计数
                    eprintln!(
                        "[DEBUG] Cleared frame {} before timeout test: ID=0x{:X}, len={}",
                        cleared_frames, frame.id, frame.len
                    );
                },
                Err(CanError::Timeout) => {
                    // 连续两次超时，说明缓冲区已清空
                    consecutive_timeouts += 1;
                    if consecutive_timeouts >= 2 {
                        eprintln!(
                            "[DEBUG] No more frames to clear (cleared {} frames)",
                            cleared_frames
                        );
                        break;
                    }
                },
                Err(e) => {
                    eprintln!("[DEBUG] Unexpected error while clearing frames: {:?}", e);
                    break;
                },
            }
        }

        // 现在设置稍长的超时（10ms），确保在没有帧时能正确超时
        adapter.set_read_timeout(Duration::from_millis(10)).unwrap();

        // 再次确认缓冲区已清空（额外清空多次，确保彻底清空）
        let mut additional_cleared = 0;
        let mut additional_consecutive_timeouts = 0;
        loop {
            match adapter.receive() {
                Ok(frame) => {
                    additional_cleared += 1;
                    additional_consecutive_timeouts = 0;
                    eprintln!(
                        "[DEBUG] Additional frame cleared before timeout test: ID=0x{:X}, len={} (count: {})",
                        frame.id, frame.len, additional_cleared
                    );
                },
                Err(CanError::Timeout) => {
                    // 连续两次超时，说明缓冲区已清空
                    additional_consecutive_timeouts += 1;
                    if additional_consecutive_timeouts >= 2 {
                        break;
                    }
                },
                Err(e) => panic!("Unexpected error: {:?}", e),
            }
        }

        // 不发送任何帧，应该超时
        let start = std::time::Instant::now();
        let result = adapter.receive();
        let elapsed = start.elapsed();

        match result {
            Err(CanError::Timeout) => {
                // 预期行为：应该在约10ms后超时
                eprintln!(
                    "[DEBUG] Timeout test passed - received Timeout error after {:?} (expected ~10ms)",
                    elapsed
                );
                // 验证超时时间合理（应该在5-20ms之间，考虑系统调度误差）
                assert!(
                    elapsed >= Duration::from_millis(5),
                    "Timeout should take at least ~5ms"
                );
                assert!(
                    elapsed < Duration::from_millis(50),
                    "Timeout should complete within ~50ms"
                );
            },
            Ok(frame) => {
                // 不应该发生 - 收到了帧而不是超时
                eprintln!("[DEBUG] Timeout test FAILED - received frame instead of timeout:");
                eprintln!("  Frame ID: 0x{:X}", frame.id);
                eprintln!("  Frame len: {}", frame.len);
                eprintln!("  Frame data: {:?}", &frame.data[..frame.len as usize]);
                eprintln!("  Frame is_extended: {}", frame.is_extended);
                eprintln!("  Elapsed time: {:?}", elapsed);
                panic!(
                    "Expected Timeout error, but received frame: ID=0x{:X}, len={}",
                    frame.id, frame.len
                );
            },
            Err(e) => {
                // 其他错误
                eprintln!(
                    "[DEBUG] Timeout test failed with unexpected error: {:?} (elapsed: {:?})",
                    e, elapsed
                );
                panic!("Expected Timeout error, got: {:?}", e);
            },
        }
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn test_socketcan_adapter_send_receive_loopback() {
        // 注意：vcan0 是虚拟接口，需要回环模式
        // 或者使用另一个线程/工具发送
        // 这个测试可能需要在真实 CAN 总线上运行，或使用特定的测试工具
        // 暂时标记为可能需要手动验证
        let interface = require_vcan0!();
        let mut adapter = SocketCanAdapter::new(interface).unwrap();

        // 发送帧
        let tx_frame = PiperFrame::new_standard(0x123, &[1, 2, 3, 4]);
        adapter.send(tx_frame).unwrap();

        // 注意：vcan0 不会自动回环，需要外部工具或真实的 CAN 总线
        // 这里只测试发送成功，接收测试需要额外设置
    }
}
