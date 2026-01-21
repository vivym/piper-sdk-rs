//! CAN 适配层核心定义
//!
//! 提供统一的 CAN 接口抽象，支持 SocketCAN（Linux）和 GS-USB（Linux/macOS/Windows）两种后端。

use std::time::Duration;
use thiserror::Error;

#[cfg(target_os = "linux")]
pub mod socketcan;

#[cfg(target_os = "linux")]
pub use socketcan::SocketCanAdapter;

#[cfg(target_os = "linux")]
pub use socketcan::split::{SocketCanRxAdapter, SocketCanTxAdapter};

pub mod gs_usb;

// Re-export gs_usb 类型
pub use gs_usb::GsUsbCanAdapter;

// GS-USB 守护进程客户端库（UDS/UDP）
pub mod gs_usb_udp;

// 导出 split 相关的类型（如果可用）
pub use gs_usb::split::{GsUsbRxAdapter, GsUsbTxAdapter};

/// SDK 通用的 CAN 帧定义（只针对 CAN 2.0）
///
/// 设计要点：
/// - Copy trait：零成本复制，适合高频场景
/// - 固定 8 字节数据：避免堆分配
/// - 无生命周期：简化 API
/// - 支持硬件时间戳：用于精确的时间测量（实时控制场景）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PiperFrame {
    /// CAN ID（标准帧或扩展帧）
    pub id: u32,

    /// 帧数据（固定 8 字节，未使用部分为 0）
    pub data: [u8; 8],

    /// 有效数据长度 (0-8)
    pub len: u8,

    /// 是否为扩展帧（29-bit ID）
    pub is_extended: bool,

    /// 硬件时间戳（微秒），0 表示不可用
    ///
    /// 当启用硬件时间戳模式（GS_CAN_MODE_HW_TIMESTAMP）时，
    /// 此字段包含设备硬件提供的时间戳，用于精确测量帧收发时间。
    /// 对于力控机械臂等实时控制系统，这是关键信息。
    ///
    /// **类型说明**：使用 `u64` 而非 `u32`，原因：
    /// - 支持绝对时间戳（Unix 纪元开始），无需基准时间管理
    /// - 支持相对时间戳（从适配器启动开始），可覆盖更长的时间范围（584,000+ 年）
    /// - 与状态层设计一致（`JointPositionState.hardware_timestamp_us: u64`）
    /// - 内存对齐后大小相同（24 字节），无额外开销
    pub timestamp_us: u64,
}

impl PiperFrame {
    /// 创建标准帧
    pub fn new_standard(id: u16, data: &[u8]) -> Self {
        Self::new(id as u32, data, false)
    }

    /// 创建扩展帧
    pub fn new_extended(id: u32, data: &[u8]) -> Self {
        Self::new(id, data, true)
    }

    /// 通用构造器
    fn new(id: u32, data: &[u8], is_extended: bool) -> Self {
        let mut fixed_data = [0u8; 8];
        let len = data.len().min(8);
        fixed_data[..len].copy_from_slice(&data[..len]);

        Self {
            id,
            data: fixed_data,
            len: len as u8,
            is_extended,
            timestamp_us: 0, // 默认无时间戳
        }
    }

    /// 获取数据切片（只包含有效数据）
    pub fn data_slice(&self) -> &[u8] {
        &self.data[..self.len as usize]
    }
}

/// CAN 适配层统一错误类型
#[derive(Error, Debug)]
pub enum CanError {
    /// USB/IO 底层错误
    #[error("IO Error: {0}")]
    Io(#[from] std::io::Error),

    /// 设备相关错误（设备未找到、未启动、配置失败等）
    #[error("Device Error: {0}")]
    Device(#[from] CanDeviceError),

    /// 读取超时（非致命，可以重试）
    #[error("Read timeout")]
    Timeout,

    /// 缓冲区溢出（致命错误）
    #[error("Buffer overflow")]
    BufferOverflow,

    /// 总线关闭（致命错误，需要重启）
    #[error("Bus off")]
    BusOff,

    /// 设备未启动
    #[error("Device not started")]
    NotStarted,
}

/// 设备/后端错误的结构化分类（不绑定具体后端实现）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CanDeviceErrorKind {
    Unknown,
    /// 设备未找到/不存在（热拔插或枚举不到）
    NotFound,
    /// 设备已断开
    NoDevice,
    /// 权限不足/被拒绝
    AccessDenied,
    /// 资源忙/被占用
    Busy,
    /// 不支持的波特率/配置
    UnsupportedConfig,
    /// 设备返回无效响应
    InvalidResponse,
    /// 解析到无效帧
    InvalidFrame,
    /// 其他 IO/后端错误
    Backend,
}

/// 结构化设备错误：kind + message（保留人类可读信息，供日志/上层策略判断）
#[derive(Error, Debug, Clone)]
#[error("{kind:?}: {message}")]
pub struct CanDeviceError {
    pub kind: CanDeviceErrorKind,
    pub message: String,
}

impl CanDeviceError {
    pub fn new(kind: CanDeviceErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    /// 判断是否为致命错误
    ///
    /// 致命错误表示设备已不可用，需要重新初始化或停止操作。
    /// 非致命错误可以重试或忽略。
    ///
    /// # 返回
    /// - `true`：致命错误（设备不可用）
    /// - `false`：非致命错误（可以重试）
    pub fn is_fatal(&self) -> bool {
        matches!(
            self.kind,
            CanDeviceErrorKind::NoDevice
                | CanDeviceErrorKind::AccessDenied
                | CanDeviceErrorKind::NotFound
        )
    }
}

impl From<String> for CanDeviceError {
    fn from(message: String) -> Self {
        Self::new(CanDeviceErrorKind::Unknown, message)
    }
}

impl From<&str> for CanDeviceError {
    fn from(message: &str) -> Self {
        Self::new(CanDeviceErrorKind::Unknown, message)
    }
}

/// CAN 适配器 Trait
///
/// 语义：
/// - `send()`: Fire-and-Forget，USB 写入成功即返回
/// - `receive()`: 阻塞直到收到有效数据帧或超时
///
/// 增加了超时和非阻塞方法，提高 API 一致性和灵活性。
pub trait CanAdapter {
    /// 发送一帧
    ///
    /// # 语义
    /// - **Fire-and-Forget**：将帧放入发送缓冲区即返回
    /// - **不等待 Echo**：不阻塞等待 USB echo 确认
    /// - **返回条件**：USB Bulk OUT 写入成功
    ///
    /// # 错误处理
    /// - 设备未启动 → `CanError::NotStarted`
    /// - USB 写入失败 → `CanError::Io` 或 `CanError::Device`
    fn send(&mut self, frame: PiperFrame) -> Result<(), CanError>;

    /// 接收一帧
    ///
    /// # 语义
    /// - **阻塞读取**：直到收到有效数据帧或超时
    /// - **自动过滤**：内部过滤 Echo 帧和瞬态错误
    /// - **只返回有效数据**：过滤后的 CAN 总线数据
    ///
    /// # 错误处理
    /// - 超时 → `CanError::Timeout`（可重试）
    /// - 缓冲区溢出 → `CanError::BufferOverflow`（致命）
    /// - 总线关闭 → `CanError::BusOff`（致命）
    /// - 设备未启动 → `CanError::NotStarted`
    fn receive(&mut self) -> Result<PiperFrame, CanError>;

    /// 设置接收超时
    ///
    /// 设置后续 `receive()` 调用的超时时间。
    /// 如果适配器不支持动态设置超时，此方法可能无效。
    ///
    /// # 参数
    /// - `timeout`: 超时时间
    ///
    /// # 默认实现
    /// 默认实现为空操作（no-op），适配器可以使用默认超时或初始化时设置的超时。
    fn set_receive_timeout(&mut self, _timeout: Duration) {
        // 默认实现：空操作
        // 具体适配器可以覆盖此方法以实现动态超时设置
    }

    /// 带超时的接收
    ///
    /// 使用指定的超时时间接收一帧，不影响后续 `receive()` 调用的超时设置。
    ///
    /// # 参数
    /// - `timeout`: 超时时间
    ///
    /// # 返回
    /// - `Ok(frame)`: 成功接收到帧
    /// - `Err(CanError::Timeout)`: 超时
    /// - `Err(e)`: 其他错误
    ///
    /// # 默认实现
    /// 默认实现临时设置超时，调用 `receive()`，然后恢复原超时。
    /// 如果适配器不支持动态超时，使用默认超时。
    fn receive_timeout(&mut self, timeout: Duration) -> Result<PiperFrame, CanError> {
        // 默认实现：临时设置超时，调用 receive，然后恢复
        // 注意：这个实现可能不够精确，具体适配器应该覆盖此方法
        self.set_receive_timeout(timeout);
        // 恢复原超时（如果适配器支持）
        // 注意：这里无法恢复，因为不知道原超时值
        // 具体适配器应该覆盖此方法以实现精确的超时控制
        self.receive()
    }

    /// 非阻塞接收
    ///
    /// 尝试接收一帧，如果当前没有可用数据，立即返回 `Ok(None)`。
    ///
    /// # 返回
    /// - `Ok(Some(frame))`: 成功接收到帧
    /// - `Ok(None)`: 当前没有可用数据（非阻塞）
    /// - `Err(e)`: 错误（设备错误、总线错误等）
    ///
    /// # 默认实现
    /// 默认实现使用 `receive_timeout(Duration::ZERO)` 模拟非阻塞行为。
    /// 如果适配器支持真正的非阻塞模式，应该覆盖此方法。
    fn try_receive(&mut self) -> Result<Option<PiperFrame>, CanError> {
        // 默认实现：使用零超时模拟非阻塞
        match self.receive_timeout(Duration::ZERO) {
            Ok(frame) => Ok(Some(frame)),
            Err(CanError::Timeout) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// 带超时的发送
    ///
    /// 使用指定的超时时间发送一帧。
    ///
    /// # 参数
    /// - `frame`: 要发送的帧
    /// - `timeout`: 超时时间
    ///
    /// # 返回
    /// - `Ok(())`: 成功发送
    /// - `Err(CanError::Timeout)`: 超时（发送缓冲区满或设备无响应）
    /// - `Err(e)`: 其他错误
    ///
    /// # 默认实现
    /// 默认实现直接调用 `send()`，忽略超时参数。
    /// 如果适配器支持发送超时，应该覆盖此方法。
    fn send_timeout(&mut self, frame: PiperFrame, _timeout: Duration) -> Result<(), CanError> {
        // 默认实现：直接调用 send，忽略超时
        // 具体适配器可以覆盖此方法以实现发送超时
        self.send(frame)
    }
}

/// RX 适配器 Trait（用于双线程模式）
///
/// 只读适配器，专门用于接收 CAN 帧。
/// 在双线程模式下，RX 线程使用此 trait 接收反馈帧。
pub trait RxAdapter {
    /// 接收一帧
    ///
    /// # 语义
    /// - **阻塞读取**：直到收到有效数据帧或超时
    /// - **自动过滤**：内部过滤 Echo 帧和瞬态错误
    /// - **只返回有效数据**：过滤后的 CAN 总线数据
    ///
    /// # 错误处理
    /// - 超时 → `CanError::Timeout`（可重试）
    /// - 缓冲区溢出 → `CanError::BufferOverflow`（致命）
    /// - 总线关闭 → `CanError::BusOff`（致命）
    fn receive(&mut self) -> Result<PiperFrame, CanError>;
}

/// TX 适配器 Trait（用于双线程模式）
///
/// 只写适配器，专门用于发送 CAN 帧。
/// 在双线程模式下，TX 线程使用此 trait 发送控制命令。
pub trait TxAdapter {
    /// 发送一帧
    ///
    /// # 语义
    /// - **Fire-and-Forget**：将帧放入发送缓冲区即返回
    /// - **不等待 Echo**：不阻塞等待 USB echo 确认
    /// - **返回条件**：USB Bulk OUT 写入成功
    ///
    /// # 错误处理
    /// - USB 写入失败 → `CanError::Io` 或 `CanError::Device`
    fn send(&mut self, frame: PiperFrame) -> Result<(), CanError>;
}

/// 可分离适配器 Trait
///
/// 支持将适配器分离为独立的 RX 和 TX 适配器，实现双线程并发访问。
///
/// # 使用场景
/// - 双线程 IO 架构：RX 和 TX 线程物理隔离，避免相互阻塞
/// - 实时控制：RX 线程不受 TX 阻塞影响，保证状态更新及时性
///
/// # 实现要求
/// - 设备必须已启动（`started == true`）才能分离
/// - 分离后，原适配器不再可用（消费 `self`）
/// - RX 和 TX 适配器可以在不同线程中并发使用
pub trait SplittableAdapter: CanAdapter {
    /// RX 适配器类型
    type RxAdapter: RxAdapter;

    /// TX 适配器类型
    type TxAdapter: TxAdapter;

    /// 分离为独立的 RX 和 TX 适配器
    ///
    /// # 前置条件
    /// - 设备必须已启动
    ///
    /// # 返回
    /// - `Ok((rx_adapter, tx_adapter))`：成功分离
    /// - `Err(CanError::NotStarted)`：设备未启动
    ///
    /// # 注意
    /// 此方法会消费 `self`，分离后不能再使用原适配器。
    fn split(self) -> Result<(Self::RxAdapter, Self::TxAdapter), CanError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_can_device_error_is_fatal() {
        // 测试致命错误
        let fatal_errors = vec![
            CanDeviceError::new(CanDeviceErrorKind::NoDevice, "Device not found"),
            CanDeviceError::new(CanDeviceErrorKind::AccessDenied, "Access denied"),
            CanDeviceError::new(CanDeviceErrorKind::NotFound, "Device not found"),
        ];

        for error in fatal_errors {
            assert!(error.is_fatal(), "Error should be fatal: {:?}", error);
        }

        // 测试非致命错误
        let non_fatal_errors = vec![
            CanDeviceError::new(CanDeviceErrorKind::Backend, "Backend error"),
            CanDeviceError::new(CanDeviceErrorKind::Busy, "Device busy"),
            CanDeviceError::new(CanDeviceErrorKind::InvalidFrame, "Invalid frame"),
            CanDeviceError::new(CanDeviceErrorKind::InvalidResponse, "Invalid response"),
            CanDeviceError::new(CanDeviceErrorKind::UnsupportedConfig, "Unsupported config"),
            CanDeviceError::new(CanDeviceErrorKind::Unknown, "Unknown error"),
        ];

        for error in non_fatal_errors {
            assert!(!error.is_fatal(), "Error should not be fatal: {:?}", error);
        }
    }

    #[test]
    fn test_piper_frame_new_standard() {
        let data = [0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08];
        let frame = PiperFrame::new_standard(0x123, &data[..4]);

        assert_eq!(frame.id, 0x123);
        assert_eq!(frame.len, 4);
        assert_eq!(frame.data[..4], data[..4]);
        assert!(!frame.is_extended);
    }

    #[test]
    fn test_piper_frame_new_extended() {
        let data = [0xFF; 8];
        let frame = PiperFrame::new_extended(0x12345678, &data);

        assert_eq!(frame.id, 0x12345678);
        assert_eq!(frame.len, 8);
        assert!(frame.is_extended);
    }

    #[test]
    fn test_piper_frame_data_truncation() {
        // 超过 8 字节的数据应该被截断
        let data = [0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A];
        let frame = PiperFrame::new_standard(0x123, &data);

        assert_eq!(frame.len, 8); // 应该截断到 8
        assert_eq!(frame.data[7], 0x08);
    }

    #[test]
    fn test_piper_frame_data_slice() {
        let data = [0x01, 0x02, 0x03];
        let frame = PiperFrame::new_standard(0x123, &data);

        let slice = frame.data_slice();
        assert_eq!(slice.len(), 3);
        assert_eq!(slice, &[0x01, 0x02, 0x03]);
    }

    #[test]
    fn test_piper_frame_copy_trait() {
        // 验证 Copy trait（零成本复制）
        let frame1 = PiperFrame::new_standard(0x123, &[0x01, 0x02]);
        let frame2 = frame1; // 应该复制，不是移动

        assert_eq!(frame1.id, frame2.id); // frame1 仍然可用
    }

    #[test]
    fn test_can_error_display() {
        let err = CanError::Timeout;
        assert!(err.to_string().contains("timeout"));
    }

    #[test]
    fn test_can_error_from_io_error() {
        let io_err = std::io::Error::new(std::io::ErrorKind::TimedOut, "test");
        let can_err: CanError = io_err.into();

        match can_err {
            CanError::Io(_) => {},
            _ => panic!("Expected Io variant"),
        }
    }

    // Mock 实现用于测试 trait 定义
    struct MockCanAdapter {
        sent_frames: Vec<PiperFrame>,
        received_frames: Vec<PiperFrame>,
        receive_index: usize,
    }

    impl CanAdapter for MockCanAdapter {
        fn send(&mut self, frame: PiperFrame) -> Result<(), CanError> {
            self.sent_frames.push(frame);
            Ok(())
        }

        fn receive(&mut self) -> Result<PiperFrame, CanError> {
            if self.receive_index < self.received_frames.len() {
                let frame = self.received_frames[self.receive_index];
                self.receive_index += 1;
                Ok(frame)
            } else {
                Err(CanError::Timeout)
            }
        }
    }

    #[test]
    fn test_can_adapter_send() {
        let mut adapter = MockCanAdapter {
            sent_frames: Vec::new(),
            received_frames: Vec::new(),
            receive_index: 0,
        };

        let frame = PiperFrame::new_standard(0x123, &[0x01, 0x02]);
        adapter.send(frame).unwrap();

        assert_eq!(adapter.sent_frames.len(), 1);
        assert_eq!(adapter.sent_frames[0].id, 0x123);
    }

    #[test]
    fn test_piper_frame_timestamp_default() {
        let frame = PiperFrame::new_standard(0x123, &[0x01, 0x02]);
        assert_eq!(frame.timestamp_us, 0); // 默认无时间戳
    }

    #[test]
    fn test_piper_frame_timestamp_preservation() {
        // 测试时间戳字段的保留（通过手动构造）
        let frame = PiperFrame {
            id: 0x123,
            data: [0x01, 0x02, 0, 0, 0, 0, 0, 0],
            len: 2,
            is_extended: false,
            timestamp_us: 12345,
        };
        assert_eq!(frame.timestamp_us, 12345);
    }

    #[test]
    fn test_piper_frame_eq_trait() {
        let frame1 = PiperFrame::new_standard(0x123, &[0x01, 0x02]);
        let frame2 = PiperFrame::new_standard(0x123, &[0x01, 0x02]);
        assert_eq!(frame1, frame2); // 相同内容应该相等

        // 时间戳不同时，应该不相等（PartialEq 会比较所有字段）
        let frame3 = PiperFrame {
            timestamp_us: 1000,
            ..frame1
        };
        assert_ne!(frame1, frame3);
    }

    #[test]
    fn test_piper_frame_with_timestamp() {
        let frame = PiperFrame {
            id: 0x123,
            data: [0x01, 0x02, 0x03, 0x04, 0, 0, 0, 0],
            len: 4,
            is_extended: false,
            timestamp_us: 12345678, // 12.345678 秒
        };

        assert_eq!(frame.id, 0x123);
        assert_eq!(frame.len, 4);
        assert_eq!(frame.timestamp_us, 12345678);
    }

    #[test]
    fn test_piper_frame_empty_data() {
        let frame = PiperFrame::new_standard(0x123, &[]);
        assert_eq!(frame.len, 0);
        assert_eq!(frame.data, [0u8; 8]);
        assert_eq!(frame.timestamp_us, 0);
    }

    #[test]
    fn test_piper_frame_data_slice_empty() {
        let frame = PiperFrame::new_standard(0x123, &[]);
        let slice = frame.data_slice();
        assert_eq!(slice.len(), 0);
    }

    #[test]
    fn test_piper_frame_data_slice_full() {
        let data = [0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08];
        let frame = PiperFrame::new_standard(0x123, &data);
        let slice = frame.data_slice();
        assert_eq!(slice.len(), 8);
        assert_eq!(slice, &data);
    }

    #[test]
    fn test_can_adapter_receive() {
        let frame1 = PiperFrame::new_standard(0x123, &[0x01, 0x02]);
        let frame2 = PiperFrame::new_extended(0x456, &[0x03, 0x04]);

        let mut adapter = MockCanAdapter {
            sent_frames: Vec::new(),
            received_frames: vec![frame1, frame2],
            receive_index: 0,
        };

        let rx_frame1 = adapter.receive().unwrap();
        assert_eq!(rx_frame1.id, 0x123);

        let rx_frame2 = adapter.receive().unwrap();
        assert_eq!(rx_frame2.id, 0x456);

        // 第三次接收应该超时
        assert!(adapter.receive().is_err());
    }

    #[test]
    fn test_can_error_variants() {
        // 测试所有错误变体
        let timeout = CanError::Timeout;
        assert!(timeout.to_string().to_lowercase().contains("timeout"));

        let buffer_overflow = CanError::BufferOverflow;
        assert!(buffer_overflow.to_string().to_lowercase().contains("overflow"));

        let bus_off = CanError::BusOff;
        assert!(bus_off.to_string().to_lowercase().contains("bus"));

        let not_started = CanError::NotStarted;
        assert!(not_started.to_string().to_lowercase().contains("start"));

        let device = CanError::Device("test error".into());
        assert!(device.to_string().contains("test error"));
    }
}
