//! CAN 适配层核心定义
//!
//! 提供统一的 CAN 接口抽象，支持 SocketCAN（Linux）和 GS-USB（跨平台）两种后端。

use thiserror::Error;

#[cfg(target_os = "linux")]
pub mod socketcan;

#[cfg(target_os = "linux")]
pub use socketcan::SocketCanAdapter;

#[cfg(not(target_os = "linux"))]
pub mod gs_usb;

// Re-export gs_usb 类型
#[cfg(not(target_os = "linux"))]
pub use gs_usb::GsUsbCanAdapter;

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
    /// - 与状态层设计一致（`CoreMotionState.timestamp_us: u64`）
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
    Device(String),

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

/// CAN 适配器 Trait
///
/// 语义：
/// - `send()`: Fire-and-Forget，USB 写入成功即返回
/// - `receive()`: 阻塞直到收到有效数据帧或超时
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
}

#[cfg(test)]
mod tests {
    use super::*;

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

        let device = CanError::Device("test error".to_string());
        assert!(device.to_string().contains("test error"));
    }
}
