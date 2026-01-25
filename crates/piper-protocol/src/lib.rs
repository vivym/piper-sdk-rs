//! # Piper Protocol
//!
//! 机械臂 CAN 总线协议定义（无硬件依赖）
//!
//! ## 模块
//!
//! - `ids`: CAN ID 常量定义
//! - `constants`: 协议常量定义
//! - `feedback`: 反馈帧解析
//! - `control`: 控制帧构建
//! - `config`: 配置帧处理
//!
//! ## 字节序
//!
//! 协议使用 Motorola (MSB) 高位在前（大端字节序）。
//! 本模块提供了字节序转换工具函数。

pub mod config;
pub mod constants;
pub mod control;
pub mod feedback;
pub mod ids;

// 重新导出常用类型
pub use config::*;
pub use constants::*;
pub use control::*;
pub use feedback::*;
pub use ids::*;

/// 临时的 CAN 帧定义（用于迁移期间，仅支持 CAN 2.0）
///
/// TODO: 移除这个定义，让协议层只返回字节数据，
/// 转换为 PiperFrame 的逻辑应该在 can 层或更高层实现。
///
/// 设计要点：
/// - Copy trait：零成本复制，适合高频场景
/// - 固定 8 字节数据：避免堆分配
/// - 无生命周期：简化 API
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
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

    /// 获取 CAN ID
    pub fn id(&self) -> u32 {
        self.id
    }

    /// 获取完整数据（8字节固定数组）
    pub fn data(&self) -> &[u8; 8] {
        &self.data
    }
}

pub mod can {
    pub use super::PiperFrame;
}

use thiserror::Error;

/// 协议解析错误类型
#[derive(Error, Debug)]
pub enum ProtocolError {
    #[error("Invalid frame length: expected {expected}, got {actual}")]
    InvalidLength { expected: usize, actual: usize },

    #[error("Invalid CAN ID: 0x{id:X}")]
    InvalidCanId { id: u32 },

    #[error("Parse error: {0}")]
    ParseError(String),

    #[error("Invalid value for field {field}: {value}")]
    InvalidValue { field: String, value: u8 },
}

/// 字节序转换工具函数
///
/// 协议使用 Motorola (MSB) 高位在前（大端字节序），
/// 这些函数用于在协议层进行字节序转换。
///
/// 大端字节序转 i32
pub fn bytes_to_i32_be(bytes: [u8; 4]) -> i32 {
    i32::from_be_bytes(bytes)
}

/// 大端字节序转 i16
pub fn bytes_to_i16_be(bytes: [u8; 2]) -> i16 {
    i16::from_be_bytes(bytes)
}

/// i32 转大端字节序
pub fn i32_to_bytes_be(value: i32) -> [u8; 4] {
    value.to_be_bytes()
}

/// i16 转大端字节序
pub fn i16_to_bytes_be(value: i16) -> [u8; 2] {
    value.to_be_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bytes_to_i32_be() {
        let bytes = [0x12, 0x34, 0x56, 0x78];
        let value = bytes_to_i32_be(bytes);
        assert_eq!(value, 0x12345678);
    }

    #[test]
    fn test_bytes_to_i32_be_negative() {
        let bytes = [0xFF, 0xFF, 0xFF, 0xFF];
        let value = bytes_to_i32_be(bytes);
        assert_eq!(value, -1);
    }

    #[test]
    fn test_bytes_to_i16_be() {
        let bytes = [0x12, 0x34];
        let value = bytes_to_i16_be(bytes);
        assert_eq!(value, 0x1234);
    }

    #[test]
    fn test_bytes_to_i16_be_negative() {
        let bytes = [0xFF, 0xFF];
        let value = bytes_to_i16_be(bytes);
        assert_eq!(value, -1);
    }

    #[test]
    fn test_i32_to_bytes_be() {
        let value = 0x12345678;
        let bytes = i32_to_bytes_be(value);
        assert_eq!(bytes, [0x12, 0x34, 0x56, 0x78]);
    }

    #[test]
    fn test_i32_to_bytes_be_negative() {
        let value = -1;
        let bytes = i32_to_bytes_be(value);
        assert_eq!(bytes, [0xFF, 0xFF, 0xFF, 0xFF]);
    }

    #[test]
    fn test_i16_to_bytes_be() {
        let value = 0x1234;
        let bytes = i16_to_bytes_be(value);
        assert_eq!(bytes, [0x12, 0x34]);
    }

    #[test]
    fn test_i16_to_bytes_be_negative() {
        let value = -1;
        let bytes = i16_to_bytes_be(value);
        assert_eq!(bytes, [0xFF, 0xFF]);
    }

    #[test]
    fn test_roundtrip_i32() {
        let original = 0x12345678;
        let bytes = i32_to_bytes_be(original);
        let decoded = bytes_to_i32_be(bytes);
        assert_eq!(original, decoded);
    }

    #[test]
    fn test_roundtrip_i16() {
        let original = 0x1234;
        let bytes = i16_to_bytes_be(original);
        let decoded = bytes_to_i16_be(bytes);
        assert_eq!(original, decoded);
    }
}
