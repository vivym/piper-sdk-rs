//! 录制命令
//!
//! 录制 CAN 总线数据到文件

use anyhow::Result;
use clap::Args;

/// 录制命令参数
#[derive(Args, Debug)]
pub struct RecordCommand {
    /// 输出文件路径
    #[arg(short, long)]
    pub output: String,

    /// CAN 接口（覆盖配置）
    #[arg(short, long)]
    pub interface: Option<String>,

    /// 设备序列号（GS-USB）
    #[arg(short, long)]
    pub serial: Option<String>,

    /// 录制时长（秒），0 表示无限
    #[arg(short, long, default_value_t = 0)]
    pub duration: u64,

    /// 自动停止（接收到特定 CAN ID 时停止）
    #[arg(short, long)]
    pub stop_on_id: Option<u32>,
}

impl RecordCommand {
    /// 执行录制
    pub async fn execute(&self, _config: &crate::modes::oneshot::OneShotConfig) -> Result<()> {
        // ⚠️ 架构限制：当前录制功能暂未实现
        //
        // 详细分析和实施计划请参见：
        // docs/architecture/piper-driver-client-mixing-analysis.md
        anyhow::bail!(
            "❌ 录制功能暂未实现\n\
             \n\
             原因：piper_client 当前未暴露底层 CAN 帧访问接口。\n\
             直接混用 piper_driver 会导致 SocketCAN/GS-USB 接口独占冲突。\n\
             \n\
             计划实施（2026 Q1）:\n\
             • 方案 A: 标准录制 API（易于使用）\n\
             • 方案 B: 高级诊断接口（灵活定制）\n\
             • 方案 C: ReplayMode（回放专用状态）\n\
             \n\
             参考文档:\n\
             • docs/architecture/piper-driver-client-mixing-analysis.md\n\
             \n\
             临时方案：如需紧急使用 CAN 录制，请参考 piper_driver 层的\n\
             AsyncRecordingHook（需手动管理生命周期）。"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_record_command_creation() {
        let cmd = RecordCommand {
            output: "test.bin".to_string(),
            interface: Some("can0".to_string()),
            serial: None,
            duration: 10,
            stop_on_id: Some(0x2A5),
        };

        assert_eq!(cmd.output, "test.bin");
        assert_eq!(cmd.duration, 10);
        assert_eq!(cmd.stop_on_id, Some(0x2A5));
    }
}
