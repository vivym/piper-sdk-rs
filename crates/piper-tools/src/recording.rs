//! # Recording format definitions
//!
//! Piper tools persist recordings as strict version 3 files. Historical v1/v2
//! files and segmented legacy shapes are intentionally rejected.

pub mod v3;

use crate::timestamp::TimestampSource;
use anyhow::Result;
use piper_protocol::frame::PiperFrame;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::Duration;

/// Piper recording file.
#[derive(Debug, Clone, Serialize)]
pub struct PiperRecording {
    /// Body format version. New recordings always use version 3.
    pub version: u8,

    /// Recording metadata.
    pub metadata: RecordingMetadata,

    /// Timestamped CAN frames.
    pub frames: Vec<TimestampedFrame>,
}

impl PiperRecording {
    /// Creates a new v3 recording.
    pub fn new(metadata: RecordingMetadata) -> Self {
        Self {
            version: v3::RECORDING_VERSION,
            metadata,
            frames: Vec::new(),
        }
    }

    /// Adds a frame to the recording.
    pub fn add_frame(&mut self, frame: TimestampedFrame) {
        self.frames.push(frame);
    }

    /// Returns the number of recorded frames.
    pub fn frame_count(&self) -> usize {
        self.frames.len()
    }

    /// Returns the recording duration from first to last frame timestamp.
    pub fn duration(&self) -> Option<Duration> {
        let first = self.frames.first()?.timestamp_us();
        let last = self.frames.last()?.timestamp_us();
        Some(Duration::from_micros(last.saturating_sub(first)))
    }

    /// Filters frames by timestamp range, inclusive.
    pub fn filter_by_time(&self, start_us: u64, end_us: u64) -> PiperRecording {
        let mut filtered = PiperRecording::new(self.metadata.clone());

        for frame in &self.frames {
            let timestamp_us = frame.timestamp_us();
            if timestamp_us >= start_us && timestamp_us <= end_us {
                filtered.add_frame(frame.clone());
            }
        }

        filtered
    }

    /// Filters frames by timestamp source.
    pub fn filter_by_source(&self, source: TimestampSource) -> PiperRecording {
        let mut filtered = PiperRecording::new(self.metadata.clone());

        for frame in &self.frames {
            if frame.timestamp_source == Some(source) {
                filtered.add_frame(frame.clone());
            }
        }

        filtered
    }

    /// Saves the recording as a strict v3 file.
    pub fn save<P: AsRef<Path>>(&self, path: P) -> Result<()> {
        v3::save_path(self, path.as_ref())
    }

    /// Loads a strict v3 recording file.
    pub fn load<P: AsRef<Path>>(path: P) -> Result<Self> {
        v3::load_path(path.as_ref())
    }

    /// Loads a strict v3 recording file with caller-supplied limits.
    pub fn load_with_limits<P: AsRef<Path>>(path: P, limits: v3::RecordingLimits) -> Result<Self> {
        v3::load_path_with_limits(path.as_ref(), limits)
    }
}

/// Recording metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordingMetadata {
    /// Recording start time as Unix seconds.
    pub start_time: u64,

    /// CAN interface name.
    pub interface: String,

    /// CAN bus speed in bps.
    pub bus_speed: u32,

    /// Platform information.
    pub platform: String,

    /// Operator name.
    pub operator: String,

    /// Free-form notes.
    pub notes: String,
}

impl RecordingMetadata {
    /// Creates metadata using the current platform and wall-clock start time.
    pub fn new(interface: String, bus_speed: u32) -> Self {
        use std::time::{SystemTime, UNIX_EPOCH};

        Self {
            start_time: SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs(),
            interface,
            bus_speed,
            platform: std::env::consts::OS.to_string(),
            operator: String::new(),
            notes: String::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RecordedFrameDirection {
    Rx,
    Tx,
}

/// Timestamped typed CAN frame persisted in recordings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimestampedFrame {
    pub frame: PiperFrame,
    pub direction: RecordedFrameDirection,
    pub timestamp_source: Option<TimestampSource>,
}

impl TimestampedFrame {
    pub fn new(
        frame: PiperFrame,
        direction: RecordedFrameDirection,
        timestamp_source: Option<TimestampSource>,
    ) -> Self {
        Self {
            frame,
            direction,
            timestamp_source,
        }
    }

    pub fn timestamp_us(&self) -> u64 {
        self.frame.timestamp_us()
    }

    pub fn raw_id(&self) -> u32 {
        self.frame.raw_id()
    }

    pub fn data(&self) -> &[u8] {
        self.frame.data()
    }
}

/// Recording file magic.
pub const MAGIC: &[u8; 8] = b"PIPERV1\0";

#[cfg(test)]
mod tests {
    use super::*;

    fn metadata() -> RecordingMetadata {
        RecordingMetadata {
            start_time: 42,
            interface: "can0".to_string(),
            bus_speed: 1_000_000,
            platform: "linux".to_string(),
            operator: "op".to_string(),
            notes: "note".to_string(),
        }
    }

    fn standard_frame(timestamp_us: u64) -> TimestampedFrame {
        TimestampedFrame::new(
            PiperFrame::new_standard(0x123, [1, 2, 3])
                .unwrap()
                .with_timestamp_us(timestamp_us),
            RecordedFrameDirection::Rx,
            Some(TimestampSource::Hardware),
        )
    }

    #[test]
    fn recording_metadata_new_sets_requested_fields() {
        let metadata = RecordingMetadata::new("can0".to_string(), 1_000_000);
        assert_eq!(metadata.interface, "can0");
        assert_eq!(metadata.bus_speed, 1_000_000);
        assert_eq!(metadata.operator, "");
    }

    #[test]
    fn timestamped_frame_exposes_frame_properties() {
        let frame = TimestampedFrame::new(
            PiperFrame::new_extended(0x123, [1, 2, 3, 4])
                .unwrap()
                .with_timestamp_us(1_234_567),
            RecordedFrameDirection::Tx,
            None,
        );

        assert_eq!(frame.timestamp_us(), 1_234_567);
        assert_eq!(frame.raw_id(), 0x123);
        assert_eq!(frame.data(), &[1, 2, 3, 4]);
        assert!(frame.frame.is_extended());
        assert_eq!(frame.direction, RecordedFrameDirection::Tx);
        assert_eq!(frame.timestamp_source, None);
    }

    #[test]
    fn piper_recording_tracks_count_duration_and_filters() {
        let mut recording = PiperRecording::new(metadata());
        assert_eq!(recording.version, v3::RECORDING_VERSION);
        assert_eq!(recording.frame_count(), 0);
        assert!(recording.duration().is_none());

        recording.add_frame(standard_frame(1000));
        recording.add_frame(TimestampedFrame::new(
            PiperFrame::new_standard(0x124, [4, 5]).unwrap().with_timestamp_us(1500),
            RecordedFrameDirection::Tx,
            Some(TimestampSource::Userspace),
        ));
        recording.add_frame(standard_frame(2000));

        assert_eq!(recording.frame_count(), 3);
        assert_eq!(recording.duration().unwrap().as_micros(), 1000);

        let time_filtered = recording.filter_by_time(1200, 1800);
        assert_eq!(time_filtered.frame_count(), 1);
        assert_eq!(time_filtered.frames[0].timestamp_us(), 1500);

        let source_filtered = recording.filter_by_source(TimestampSource::Hardware);
        assert_eq!(source_filtered.frame_count(), 2);
        assert!(
            source_filtered
                .frames
                .iter()
                .all(|frame| frame.timestamp_source == Some(TimestampSource::Hardware))
        );
    }

    #[test]
    fn save_and_load_roundtrip_uses_v3() {
        let mut recording = PiperRecording::new(metadata());
        recording.add_frame(standard_frame(1000));
        recording.add_frame(TimestampedFrame::new(
            PiperFrame::new_extended(0x1ABCDE, [9, 10]).unwrap().with_timestamp_us(2000),
            RecordedFrameDirection::Tx,
            Some(TimestampSource::Kernel),
        ));

        let temp_file = tempfile::NamedTempFile::new().unwrap();
        recording.save(temp_file.path()).unwrap();

        let loaded = PiperRecording::load(temp_file.path()).unwrap();
        assert_eq!(loaded.version, 3);
        assert_eq!(loaded.metadata, recording.metadata);
        assert_eq!(loaded.frames, recording.frames);
    }

    #[test]
    fn load_rejects_v1_and_v2_headers() {
        for version in [1u8, 2u8] {
            let temp_file = tempfile::NamedTempFile::new().unwrap();
            std::fs::write(
                temp_file.path(),
                [MAGIC.as_slice(), &[version], &[0]].concat(),
            )
            .unwrap();

            let result = PiperRecording::load(temp_file.path());
            assert!(result.is_err());
        }
    }
}
