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

/// 从 CAN 帧中提取时间戳
///
/// 根据平台和 CAN 接口类型自动检测时间戳来源
pub fn extract_timestamp(
    can_frame: &[u8],
    platform_hint: Option<TimestampSource>,
) -> (u64, TimestampSource) {
    // ⚠️ 注意：实际实现需要根据具体的 CAN 帧格式提取时间戳
    // 这里提供一个简化的实现框架

    // 如果提供了平台提示，使用它
    if let Some(source) = platform_hint {
        // 实际时间戳应该从 CAN 帧的某个字段提取
        // 这里使用当前时间作为占位符
        let timestamp = current_time_us();
        return (timestamp, source);
    }

    // 否则，自动检测
    // 注意：这需要查看实际的 CAN 帧格式
    let source = detect_timestamp_source_from_frame(can_frame);
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

/// 从 CAN 帧检测时间戳来源
fn detect_timestamp_source_from_frame(_frame: &[u8]) -> TimestampSource {
    // ⚠️ 实际实现需要检查 CAN 帧的格式
    // 例如：某些 CAN 帧可能包含硬件时间戳字段

    #[cfg(target_os = "linux")]
    {
        TimestampSource::Hardware
    }

    #[cfg(not(target_os = "linux"))]
    {
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
