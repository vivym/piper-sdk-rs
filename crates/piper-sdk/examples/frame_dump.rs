//! Frame Dump 示例
//!
//! 演示如何使用 serde 序列化 PiperFrame，
//! 用于记录、保存和加载 CAN 帧数据。
//!
//! 使用场景：
//! - 记录 CAN 总线通信
//! - 调试和问题诊断
//! - 帧回放功能
//! - 网络传输帧数据

#[cfg(feature = "serde")]
fn frame_dump_output_path() -> std::path::PathBuf {
    std::env::temp_dir().join("piper_sdk_can_frames.json")
}

fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    // 初始化日志
    piper_sdk::init_logger!();

    println!("🎯 Piper Frame Dump Example\n");

    // 只有启用 serde feature 时才能运行此演示
    #[cfg(feature = "serde")]
    {
        use piper_sdk::can::PiperFrame;
        use serde_json;
        use std::fs::File;
        use std::io::{BufRead, BufReader, BufWriter, Write};

        // 创建一些示例帧
        let frames = vec![
            PiperFrame::new_standard(0x123, [0x01, 0x02, 0x03, 0x04])?,
            PiperFrame::new_standard(0x456, [0x05, 0x06, 0x07, 0x08])?,
            PiperFrame::new_extended(0x12345678, [0xFF, 0xFF, 0xFF, 0xFF])?,
        ];

        println!("📝 Original frames:");
        for (i, frame) in frames.iter().enumerate() {
            println!(
                "  Frame {}: ID=0x{:03X}, len={}, data={:?}",
                i,
                frame.raw_id(),
                frame.dlc(),
                frame.data()
            );
        }

        // 1. JSON 序列化示例
        println!("\n🔄 Serializing to JSON...");
        let json = serde_json::to_string_pretty(&frames)?;
        println!("{}", json);

        // 2. 反序列化示例
        println!("\n🔄 Deserializing from JSON...");
        let deserialized: Vec<PiperFrame> = serde_json::from_str(&json)?;
        println!("✅ Deserialized {} frames", deserialized.len());
        assert_eq!(frames.len(), deserialized.len());

        // 3. 保存到文件
        let output_path = frame_dump_output_path();
        let file = File::create(&output_path)?;
        let mut writer = BufWriter::new(file);

        for frame in frames.iter() {
            let json = serde_json::to_string(frame)?;
            writeln!(writer, "{}", json)?;
        }
        println!(
            "✅ Saved {} frames to {}",
            frames.len(),
            output_path.display()
        );

        // 4. 从文件加载并验证
        let file = File::open(&output_path)?;
        let reader = BufReader::new(file);
        let loaded_count = reader.lines().count();
        println!(
            "✅ Loaded {} frames from {}",
            loaded_count,
            output_path.display()
        );
    }

    #[cfg(not(feature = "serde"))]
    {
        println!("⚠️  Serde feature not enabled.");
        println!("   Run with:");
        println!("   cargo run -p piper-sdk --example frame_dump --features serde");
        println!("\n✅ Frame serde support includes:");
        println!("   - PiperFrame (protocol layer)");
        println!("   - GsUsbFrame (GS-USB layer)");
        println!("   - All type units (Rad, Deg, etc.)");
        println!("   - JointArray and CartesianPose");
    }

    Ok(())
}

#[cfg(all(test, feature = "serde"))]
mod tests {
    use super::*;

    #[test]
    fn frame_dump_uses_platform_temp_directory() {
        let output = frame_dump_output_path();
        assert_eq!(
            output.file_name().and_then(|name| name.to_str()),
            Some("piper_sdk_can_frames.json")
        );
        assert!(output.starts_with(std::env::temp_dir()));
    }
}
