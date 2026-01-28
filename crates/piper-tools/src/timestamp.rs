//! # 时间戳处理
//!
//! 明确三种时间戳来源及其精度

use serde::{Deserialize, Serialize};

/// 时间戳来源
///
/// | 来源 | 精度 | 说明 |
/// |------|------|------|
/// | Hardware | ~1μs | CAN 控制器内部时钟（最佳） |
/// | Kernel | ~10μs | 驱动接收时间 |
/// | Userspace | ~100μs | 应用接收时间（含调度延迟） |
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TimestampSource {
    /// 硬件时间戳（~1μs）
    /// CAN 控制器内部时钟，精度最高
    Hardware,

    /// 内核时间戳（~10μs）
    /// 驱动接收时间，精度中等
    Kernel,

    /// 用户空间时间戳（~100μs）
    /// 应用接收时间，含调度延迟
    Userspace,
}

impl TimestampSource {
    /// 获取时间戳精度（微秒）
    pub fn precision_us(&self) -> u64 {
        match self {
            TimestampSource::Hardware => 1,
            TimestampSource::Kernel => 10,
            TimestampSource::Userspace => 100,
        }
    }

    /// 获取时间戳描述
    pub fn description(&self) -> &'static str {
        match self {
            TimestampSource::Hardware => "Hardware timestamp (~1μs)",
            TimestampSource::Kernel => "Kernel timestamp (~10μs)",
            TimestampSource::Userspace => "Userspace timestamp (~100μs)",
        }
    }
}

/// 从 CAN 帧数据中提取时间戳（预留接口，暂未使用）
///
/// **注意**: 此函数是预留接口，当前未被代码库使用。
///
/// **实际时间戳提取**:
/// - 录制和回放功能使用 `piper-can` 层的 `extract_timestamp_from_cmsg()` 方法
/// - 该方法从 SocketCAN 的控制消息（CMSG）中提取硬件/软件时间戳
/// - 参见: `crates/piper-can/src/socketcan/mod.rs` 或 `split.rs`
///
/// **此函数的预期用途**:
/// - 未来可能用于从已保存的 CAN 帧数据中解析时间戳
/// - 或用于特殊场景下需要从帧数据本身提取时间戳的情况
///
/// **当前实现**:
/// - 使用当前系统时间作为占位符
/// - 如果需要实现，请根据实际 CAN 帧格式解析时间戳字段
#[deprecated(
    since = "0.1.0",
    note = "This function is a stub and not used in production. Timestamp extraction is handled by piper-can layer."
)]
pub fn extract_timestamp(
    _can_frame: &[u8],
    platform_hint: Option<TimestampSource>,
) -> (u64, TimestampSource) {
    // 如果提供了平台提示，使用它
    if let Some(source) = platform_hint {
        // ⚠️ 当前实现：使用系统时间作为占位符
        // 实际应该从 can_frame 中解析时间戳字段（如果帧数据包含时间戳）
        let timestamp = current_time_us();
        return (timestamp, source);
    }

    // 否则，自动检测平台并使用当前时间
    let source = detect_timestamp_source();
    let timestamp = current_time_us();

    (timestamp, source)
}

/// 检测时间戳来源（基于平台）
///
/// Linux (SocketCAN) -> Hardware or Kernel
/// 其他平台 (GS-USB) -> Userspace
pub fn detect_timestamp_source() -> TimestampSource {
    #[cfg(target_os = "linux")]
    {
        // Linux SocketCAN 可能支持硬件时间戳
        // 实际检测需要检查 SocketCAN 配置
        TimestampSource::Hardware
    }

    #[cfg(not(target_os = "linux"))]
    {
        // 其他平台（GS-USB）通常是用户空间时间戳
        TimestampSource::Userspace
    }
}

/// 获取当前时间（微秒）
fn current_time_us() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};

    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_micros() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_timestamp_precision() {
        assert_eq!(TimestampSource::Hardware.precision_us(), 1);
        assert_eq!(TimestampSource::Kernel.precision_us(), 10);
        assert_eq!(TimestampSource::Userspace.precision_us(), 100);
    }

    #[test]
    fn test_timestamp_description() {
        assert!(TimestampSource::Hardware.description().contains("1μs"));
        assert!(TimestampSource::Kernel.description().contains("10μs"));
        assert!(TimestampSource::Userspace.description().contains("100μs"));
    }

    #[test]
    fn test_detect_timestamp_source() {
        let source = detect_timestamp_source();
        // 至少应该返回一个有效的来源
        match source {
            TimestampSource::Hardware | TimestampSource::Kernel | TimestampSource::Userspace => {},
        }
    }
}
