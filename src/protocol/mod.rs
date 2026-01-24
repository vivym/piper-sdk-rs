//! Piper 协议层模块
//!
//! 负责将 CAN 帧的原始字节数据解析为类型安全的 Rust 结构体，
//! 以及将 Rust 结构体编码为 CAN 帧数据。

pub mod config;
pub mod constants; // ✅ 新增
pub mod control;
pub mod feedback;
pub mod ids;

pub use config::*;
pub use constants::*; // ✅ 新增
pub use control::*;
pub use feedback::*;
pub use ids::*;

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
