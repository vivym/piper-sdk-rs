//! # 录制格式定义
//!
//! 统一的录制文件格式，所有工具共用

use crate::timestamp::TimestampSource;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::Path;
use std::time::Duration;

/// Piper 录制文件 v1.0
///
/// 格式：使用 bincode 序列化
///
/// ```text
/// [Header: 8 bytes magic]
/// [Version: 1 byte]
/// [Metadata length: 4 bytes]
/// [Metadata: JSON]
/// [Frame count: 8 bytes]
/// [Frames...]
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PiperRecording {
    /// 格式版本
    pub version: u8,

    /// 元数据
    pub metadata: RecordingMetadata,

    /// 时间戳的 CAN 帧序列
    pub frames: Vec<TimestampedFrame>,
}

impl PiperRecording {
    /// 创建新的录制
    pub fn new(metadata: RecordingMetadata) -> Self {
        Self {
            version: 1,
            metadata,
            frames: Vec::new(),
        }
    }

    /// 添加帧
    pub fn add_frame(&mut self, frame: TimestampedFrame) {
        self.frames.push(frame);
    }

    /// 获取帧数量
    pub fn frame_count(&self) -> usize {
        self.frames.len()
    }

    /// 获取时间跨度
    pub fn duration(&self) -> Option<Duration> {
        if self.frames.is_empty() {
            return None;
        }

        let first = self.frames.first()?.timestamp_us;
        let last = self.frames.last()?.timestamp_us;

        Some(Duration::from_micros(last - first))
    }

    /// 按时间范围过滤
    pub fn filter_by_time(&self, start_us: u64, end_us: u64) -> PiperRecording {
        let mut filtered = PiperRecording::new(self.metadata.clone());

        for frame in &self.frames {
            if frame.timestamp_us >= start_us && frame.timestamp_us <= end_us {
                filtered.add_frame(frame.clone());
            }
        }

        filtered
    }

    /// 按时间戳来源过滤
    pub fn filter_by_source(&self, source: TimestampSource) -> PiperRecording {
        let mut filtered = PiperRecording::new(self.metadata.clone());

        for frame in &self.frames {
            if frame.source == source {
                filtered.add_frame(frame.clone());
            }
        }

        filtered
    }

    /// 保存到文件
    ///
    /// 文件格式：
    /// ```text
    /// [MAGIC: 8 bytes]
    /// [Version: 1 byte]
    /// [Data: bincode serialized PiperRecording]
    /// ```
    pub fn save<P: AsRef<Path>>(&self, path: P) -> Result<()> {
        let file = File::create(path.as_ref()).context("创建录制文件失败")?;

        let mut writer = BufWriter::new(file);

        // 写入魔数
        writer.write_all(MAGIC).context("写入魔数失败")?;

        // 写入版本
        writer.write_all(&[self.version]).context("写入版本失败")?;

        // 序列化数据
        let data = bincode::serialize(self).context("序列化录制失败")?;

        // 写入数据
        writer.write_all(&data).context("写入录制数据失败")?;

        writer.flush().context("刷新缓冲区失败")?;

        Ok(())
    }

    /// 从文件加载
    ///
    /// 文件格式：
    /// ```text
    /// [MAGIC: 8 bytes]
    /// [Version: 1 byte]
    /// [Data: bincode serialized PiperRecording]
    /// ```
    pub fn load<P: AsRef<Path>>(path: P) -> Result<Self> {
        let file = File::open(path.as_ref()).context("打开录制文件失败")?;

        let mut reader = BufReader::new(file);

        // 读取并验证魔数
        let mut magic = [0u8; 8];
        reader.read_exact(&mut magic).context("读取魔数失败")?;

        if &magic != MAGIC {
            anyhow::bail!("无效的录制文件格式（魔数不匹配）");
        }

        // 读取版本
        let mut version = [0u8; 1];
        reader.read_exact(&mut version).context("读取版本失败")?;

        if version[0] != 1 {
            anyhow::bail!("不支持的录制文件版本: {}", version[0]);
        }

        // 读取剩余数据
        let mut data = Vec::new();
        reader.read_to_end(&mut data).context("读取录制数据失败")?;

        // 反序列化
        let recording: PiperRecording = bincode::deserialize(&data).context("反序列化录制失败")?;

        Ok(recording)
    }
}

/// 录制元数据
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordingMetadata {
    /// 录制开始时间（Unix 时间戳，秒）
    pub start_time: u64,

    /// CAN 接口名称
    pub interface: String,

    /// CAN 总线速度（bps）
    pub bus_speed: u32,

    /// 平台信息
    pub platform: String,

    /// 备注
    pub notes: String,
}

impl RecordingMetadata {
    /// 创建新的元数据
    pub fn new(interface: String, bus_speed: u32) -> Self {
        use std::time::{SystemTime, UNIX_EPOCH};

        Self {
            start_time: SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs(),
            interface,
            bus_speed,
            platform: std::env::consts::OS.to_string(),
            notes: String::new(),
        }
    }
}

/// 时间戳的 CAN 帧
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimestampedFrame {
    /// 时间戳（微秒）
    pub timestamp_us: u64,

    /// CAN ID
    pub can_id: u32,

    /// CAN 数据（最多 8 字节）
    pub data: Vec<u8>,

    /// 时间戳来源
    pub source: TimestampSource,
}

impl TimestampedFrame {
    /// 创建新的帧
    pub fn new(timestamp_us: u64, can_id: u32, data: Vec<u8>, source: TimestampSource) -> Self {
        Self {
            timestamp_us,
            can_id,
            data,
            source,
        }
    }

    /// 获取帧长度（DLC）
    pub fn dlc(&self) -> u8 {
        self.data.len() as u8
    }

    /// 是否为扩展帧
    pub fn is_extended(&self) -> bool {
        self.can_id > 0x7FF
    }
}

/// 录制文件魔数（用于文件格式识别）
pub const MAGIC: &[u8; 8] = b"PIPERV1\0";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_recording_metadata() {
        let metadata = RecordingMetadata::new("can0".to_string(), 1_000_000);
        assert_eq!(metadata.interface, "can0");
        assert_eq!(metadata.bus_speed, 1_000_000);
    }

    #[test]
    fn test_timestamped_frame() {
        let frame = TimestampedFrame::new(
            1234567890,
            0x123,
            vec![1, 2, 3, 4],
            TimestampSource::Hardware,
        );

        assert_eq!(frame.timestamp_us, 1234567890);
        assert_eq!(frame.can_id, 0x123);
        assert_eq!(frame.data, vec![1, 2, 3, 4]);
        assert_eq!(frame.dlc(), 4);
        assert!(!frame.is_extended());
    }

    #[test]
    fn test_piper_recording() {
        let metadata = RecordingMetadata::new("can0".to_string(), 1_000_000);
        let mut recording = PiperRecording::new(metadata);

        assert_eq!(recording.frame_count(), 0);

        recording.add_frame(TimestampedFrame::new(
            1000,
            0x100,
            vec![1, 2],
            TimestampSource::Hardware,
        ));

        assert_eq!(recording.frame_count(), 1);
    }

    #[test]
    fn test_recording_duration() {
        let metadata = RecordingMetadata::new("can0".to_string(), 1_000_000);
        let mut recording = PiperRecording::new(metadata);

        // 空录制
        assert!(recording.duration().is_none());

        // 添加帧
        recording.add_frame(TimestampedFrame::new(
            1000,
            0x100,
            vec![1],
            TimestampSource::Hardware,
        ));
        recording.add_frame(TimestampedFrame::new(
            2000,
            0x100,
            vec![2],
            TimestampSource::Hardware,
        ));

        let duration = recording.duration().unwrap();
        assert_eq!(duration.as_micros(), 1000);
    }

    #[test]
    fn test_filter_by_time() {
        let metadata = RecordingMetadata::new("can0".to_string(), 1_000_000);
        let mut recording = PiperRecording::new(metadata);

        recording.add_frame(TimestampedFrame::new(
            1000,
            0x100,
            vec![1],
            TimestampSource::Hardware,
        ));
        recording.add_frame(TimestampedFrame::new(
            1500,
            0x100,
            vec![2],
            TimestampSource::Hardware,
        ));
        recording.add_frame(TimestampedFrame::new(
            2000,
            0x100,
            vec![3],
            TimestampSource::Hardware,
        ));

        let filtered = recording.filter_by_time(1200, 1800);
        assert_eq!(filtered.frame_count(), 1);
        assert_eq!(filtered.frames[0].timestamp_us, 1500);
    }

    #[test]
    fn test_filter_by_source() {
        let metadata = RecordingMetadata::new("can0".to_string(), 1_000_000);
        let mut recording = PiperRecording::new(metadata);

        recording.add_frame(TimestampedFrame::new(
            1000,
            0x100,
            vec![1],
            TimestampSource::Hardware,
        ));
        recording.add_frame(TimestampedFrame::new(
            2000,
            0x100,
            vec![2],
            TimestampSource::Userspace,
        ));

        let filtered = recording.filter_by_source(TimestampSource::Hardware);
        assert_eq!(filtered.frame_count(), 1);
        assert_eq!(filtered.frames[0].source, TimestampSource::Hardware);
    }

    #[test]
    fn test_save_and_load() {
        let metadata = RecordingMetadata::new("can0".to_string(), 1_000_000);
        let mut recording = PiperRecording::new(metadata);

        recording.add_frame(TimestampedFrame::new(
            1000,
            0x100,
            vec![1, 2, 3, 4],
            TimestampSource::Hardware,
        ));
        recording.add_frame(TimestampedFrame::new(
            2000,
            0x200,
            vec![5, 6, 7, 8],
            TimestampSource::Userspace,
        ));

        // 保存到临时文件
        let temp_file = "/tmp/test_recording.bin";
        recording.save(temp_file).unwrap();

        // 加载文件
        let loaded = PiperRecording::load(temp_file).unwrap();

        // 验证数据
        assert_eq!(loaded.version, 1);
        assert_eq!(loaded.frame_count(), 2);
        assert_eq!(loaded.frames[0].timestamp_us, 1000);
        assert_eq!(loaded.frames[0].can_id, 0x100);
        assert_eq!(loaded.frames[0].data, vec![1, 2, 3, 4]);
        assert_eq!(loaded.frames[1].timestamp_us, 2000);
        assert_eq!(loaded.frames[1].can_id, 0x200);
        assert_eq!(loaded.frames[1].data, vec![5, 6, 7, 8]);

        // 清理
        std::fs::remove_file(temp_file).ok();
    }
}
