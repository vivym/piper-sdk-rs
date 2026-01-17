//! 顶层导出测试
//!
//! 验证所有核心类型可以从 `piper_sdk` 直接导入（不需要 `robot::` 前缀）

use piper_sdk::{CanError, PiperBuilder, ProtocolError, RobotError};

/// 测试顶层导出是否正常工作
#[test]
fn test_top_level_exports() {
    // 验证可以直接从顶层导入
    let _builder: PiperBuilder = PiperBuilder::new();
    let _error: RobotError = RobotError::Timeout;
    let _can_error: CanError = CanError::Timeout;
    let _protocol_error: ProtocolError = ProtocolError::InvalidCanId { id: 0x123 };

    // 验证成功导入（编译通过即表示导入成功）
}
