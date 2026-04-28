//! Strict recording v3 wire format.

use super::{MAGIC, PiperRecording, RecordedFrameDirection, RecordingMetadata, TimestampedFrame};
use crate::timestamp::TimestampSource;
use anyhow::{Context, Result, bail};
use bincode::Options;
use piper_protocol::frame::PiperFrame;
use serde::de::{DeserializeSeed, SeqAccess, Visitor};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;

pub const RECORDING_VERSION: u8 = 3;
pub const MAX_RECORDING_BODY_BYTES: u64 = 1_073_741_824;
pub const MAX_RECORDING_FRAMES: usize = 20_000_000;
pub const MAX_METADATA_STRING_BYTES: usize = 16_384;
const RECORDING_FILE_HEADER_BYTES: u64 = 9;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecordingLimits {
    pub max_body_bytes: u64,
    pub max_frames: usize,
    pub max_metadata_string_bytes: usize,
}

impl Default for RecordingLimits {
    fn default() -> Self {
        Self {
            max_body_bytes: MAX_RECORDING_BODY_BYTES,
            max_frames: MAX_RECORDING_FRAMES,
            max_metadata_string_bytes: MAX_METADATA_STRING_BYTES,
        }
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct BincodePiperRecordingV3<'a> {
    version: u8,
    metadata: BincodeRecordingMetadata<'a>,
    frames: Vec<BincodeRecordedFrameV3>,
}

impl<'a> From<&'a PiperRecording> for BincodePiperRecordingV3<'a> {
    fn from(recording: &'a PiperRecording) -> Self {
        Self {
            version: recording.version,
            metadata: BincodeRecordingMetadata {
                start_time: recording.metadata.start_time,
                interface: &recording.metadata.interface,
                bus_speed: recording.metadata.bus_speed,
                platform: &recording.metadata.platform,
                operator: &recording.metadata.operator,
                notes: &recording.metadata.notes,
            },
            frames: recording.frames.iter().map(BincodeRecordedFrameV3::from).collect(),
        }
    }
}

#[derive(Debug, Serialize)]
struct BincodeRecordingMetadata<'a> {
    start_time: u64,
    interface: &'a str,
    bus_speed: u32,
    platform: &'a str,
    operator: &'a str,
    notes: &'a str,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
struct BincodeRecordedFrameV3 {
    frame: PiperFrame,
    direction: u8,
    timestamp_source: u8,
}

impl From<&TimestampedFrame> for BincodeRecordedFrameV3 {
    fn from(frame: &TimestampedFrame) -> Self {
        Self {
            frame: frame.frame,
            direction: encode_direction(frame.direction),
            timestamp_source: encode_timestamp_source(frame.timestamp_source),
        }
    }
}

impl TryFrom<BincodeRecordedFrameV3> for TimestampedFrame {
    type Error = anyhow::Error;

    fn try_from(frame: BincodeRecordedFrameV3) -> Result<Self> {
        Ok(Self {
            frame: frame.frame,
            direction: decode_direction(frame.direction)?,
            timestamp_source: decode_timestamp_source(frame.timestamp_source)?,
        })
    }
}

fn encode_direction(direction: RecordedFrameDirection) -> u8 {
    match direction {
        RecordedFrameDirection::Rx => 0,
        RecordedFrameDirection::Tx => 1,
    }
}

fn decode_direction(direction: u8) -> Result<RecordedFrameDirection> {
    match direction {
        0 => Ok(RecordedFrameDirection::Rx),
        1 => Ok(RecordedFrameDirection::Tx),
        other => bail!("invalid recorded frame direction: {other}"),
    }
}

fn encode_timestamp_source(source: Option<TimestampSource>) -> u8 {
    match source {
        None => 0,
        Some(TimestampSource::Hardware) => 1,
        Some(TimestampSource::Kernel) => 2,
        Some(TimestampSource::Userspace) => 3,
    }
}

fn decode_timestamp_source(source: u8) -> Result<Option<TimestampSource>> {
    match source {
        0 => Ok(None),
        1 => Ok(Some(TimestampSource::Hardware)),
        2 => Ok(Some(TimestampSource::Kernel)),
        3 => Ok(Some(TimestampSource::Userspace)),
        other => bail!("invalid recorded frame timestamp source: {other}"),
    }
}

fn v3_options() -> impl Options {
    bincode::DefaultOptions::new().with_little_endian().with_fixint_encoding()
}

fn v3_limited_options(limit: u64) -> impl Options {
    v3_options().with_limit(limit)
}

pub fn serialize_body(recording: &PiperRecording) -> Result<Vec<u8>> {
    serialize_body_with_limits(recording, RecordingLimits::default())
}

pub fn serialize_body_with_limits(
    recording: &PiperRecording,
    limits: RecordingLimits,
) -> Result<Vec<u8>> {
    validate_recording(recording, limits)?;

    let body = BincodePiperRecordingV3::from(recording);
    let data = v3_limited_options(limits.max_body_bytes)
        .serialize(&body)
        .context("serialize recording v3 body")?;

    if data.len() as u64 > limits.max_body_bytes {
        bail!(
            "recording body is {} bytes, limit is {}",
            data.len(),
            limits.max_body_bytes
        );
    }

    Ok(data)
}

pub fn deserialize_body(body: &[u8]) -> Result<PiperRecording> {
    deserialize_body_with_limits(body, RecordingLimits::default())
}

pub fn deserialize_body_with_limits(
    body: &[u8],
    limits: RecordingLimits,
) -> Result<PiperRecording> {
    if body.len() as u64 > limits.max_body_bytes {
        bail!(
            "recording body is {} bytes, limit is {}",
            body.len(),
            limits.max_body_bytes
        );
    }

    v3_limited_options(limits.max_body_bytes)
        .reject_trailing_bytes()
        .deserialize_seed(RecordingBodySeed { limits }, body)
        .context("deserialize recording v3 body")
}

pub fn save_path(recording: &PiperRecording, path: &Path) -> Result<()> {
    let data = serialize_body(recording)?;
    let file = File::create(path).context("create recording file")?;
    let mut writer = BufWriter::new(file);

    writer.write_all(MAGIC).context("write recording magic")?;
    writer.write_all(&[RECORDING_VERSION]).context("write recording version")?;
    writer.write_all(&data).context("write recording body")?;
    writer.flush().context("flush recording file")?;

    Ok(())
}

/// Incrementally writes a strict v3 recording without buffering all frames in memory.
///
/// The writer emits the file header and metadata immediately, reserves the v3
/// frame-count field, streams encoded frames as they arrive, and patches the
/// final frame count in [`StreamingRecordingWriter::finish`].
pub struct StreamingRecordingWriter<W> {
    writer: W,
    frame_count_offset: u64,
    frame_count: u64,
    limits: RecordingLimits,
}

impl<W: Write + Seek> StreamingRecordingWriter<W> {
    pub fn new(writer: W, metadata: &RecordingMetadata) -> Result<Self> {
        Self::new_with_limits(writer, metadata, RecordingLimits::default())
    }

    pub fn new_with_limits(
        mut writer: W,
        metadata: &RecordingMetadata,
        limits: RecordingLimits,
    ) -> Result<Self> {
        validate_metadata_string("interface", &metadata.interface, limits)?;
        validate_metadata_string("platform", &metadata.platform, limits)?;
        validate_metadata_string("operator", &metadata.operator, limits)?;
        validate_metadata_string("notes", &metadata.notes, limits)?;

        writer.write_all(MAGIC).context("write recording magic")?;
        writer.write_all(&[RECORDING_VERSION]).context("write recording version")?;
        v3_options()
            .serialize_into(&mut writer, &RECORDING_VERSION)
            .context("write recording body version")?;
        v3_options()
            .serialize_into(
                &mut writer,
                &BincodeRecordingMetadata {
                    start_time: metadata.start_time,
                    interface: &metadata.interface,
                    bus_speed: metadata.bus_speed,
                    platform: &metadata.platform,
                    operator: &metadata.operator,
                    notes: &metadata.notes,
                },
            )
            .context("write recording metadata")?;

        let frame_count_offset = writer.stream_position().context("locate frame count field")?;
        v3_options()
            .serialize_into(&mut writer, &0_u64)
            .context("write placeholder frame count")?;

        let body_len = writer
            .stream_position()
            .context("measure recording body")?
            .saturating_sub(RECORDING_FILE_HEADER_BYTES);
        if body_len > limits.max_body_bytes {
            bail!(
                "recording body is {} bytes, limit is {}",
                body_len,
                limits.max_body_bytes
            );
        }

        Ok(Self {
            writer,
            frame_count_offset,
            frame_count: 0,
            limits,
        })
    }

    pub fn push_frame(&mut self, frame: &TimestampedFrame) -> Result<()> {
        if self.frame_count as usize >= self.limits.max_frames {
            bail!(
                "recording contains more than {} frames",
                self.limits.max_frames
            );
        }

        v3_options()
            .serialize_into(&mut self.writer, &BincodeRecordedFrameV3::from(frame))
            .context("write recording frame")?;
        self.frame_count += 1;

        let body_len = self
            .writer
            .stream_position()
            .context("measure recording body")?
            .saturating_sub(RECORDING_FILE_HEADER_BYTES);
        if body_len > self.limits.max_body_bytes {
            bail!(
                "recording body is {} bytes, limit is {}",
                body_len,
                self.limits.max_body_bytes
            );
        }

        Ok(())
    }

    pub fn frame_count(&self) -> u64 {
        self.frame_count
    }

    pub fn finish(mut self) -> Result<W> {
        let end_offset = self.writer.stream_position().context("locate recording end")?;
        self.writer
            .seek(SeekFrom::Start(self.frame_count_offset))
            .context("seek to frame count field")?;
        v3_options()
            .serialize_into(&mut self.writer, &self.frame_count)
            .context("write final frame count")?;
        self.writer
            .seek(SeekFrom::Start(end_offset))
            .context("seek back to recording end")?;
        self.writer.flush().context("flush recording stream")?;
        Ok(self.writer)
    }
}

pub fn load_path(path: &Path) -> Result<PiperRecording> {
    load_path_with_limits(path, RecordingLimits::default())
}

pub fn load_path_with_limits(path: &Path, limits: RecordingLimits) -> Result<PiperRecording> {
    let file = File::open(path).context("open recording file")?;
    let metadata_len = file.metadata().ok().map(|metadata| metadata.len());
    let mut reader = BufReader::new(file);

    let mut magic = [0u8; 8];
    reader.read_exact(&mut magic).context("read recording magic")?;
    if &magic != MAGIC {
        bail!("invalid recording file magic");
    }

    let mut version = [0u8; 1];
    reader.read_exact(&mut version).context("read recording header version")?;
    if version[0] != RECORDING_VERSION {
        bail!("unsupported recording file version: {}", version[0]);
    }

    if let Some(file_len) = metadata_len {
        let body_len =
            file_len.checked_sub(9).context("recording file is shorter than v3 header")?;
        if body_len > limits.max_body_bytes {
            bail!(
                "recording body is {} bytes, limit is {}",
                body_len,
                limits.max_body_bytes
            );
        }
    }

    let body = read_body_bounded(&mut reader, limits.max_body_bytes)?;
    deserialize_body_with_limits(&body, limits)
}

fn read_body_bounded<R: Read>(reader: &mut R, max_body_bytes: u64) -> Result<Vec<u8>> {
    let mut body = Vec::new();
    let mut limited = reader.take(max_body_bytes.saturating_add(1));
    limited.read_to_end(&mut body).context("read recording body")?;

    if body.len() as u64 > max_body_bytes {
        bail!("recording body exceeds limit of {} bytes", max_body_bytes);
    }

    Ok(body)
}

fn validate_recording(recording: &PiperRecording, limits: RecordingLimits) -> Result<()> {
    if recording.version != RECORDING_VERSION {
        bail!(
            "recording body version {} does not match v3",
            recording.version
        );
    }

    if recording.frames.len() > limits.max_frames {
        bail!(
            "recording contains {} frames, limit is {}",
            recording.frames.len(),
            limits.max_frames
        );
    }

    validate_metadata_string("interface", &recording.metadata.interface, limits)?;
    validate_metadata_string("platform", &recording.metadata.platform, limits)?;
    validate_metadata_string("operator", &recording.metadata.operator, limits)?;
    validate_metadata_string("notes", &recording.metadata.notes, limits)?;

    Ok(())
}

fn validate_metadata_string(name: &str, value: &str, limits: RecordingLimits) -> Result<()> {
    if value.len() > limits.max_metadata_string_bytes {
        bail!(
            "metadata {name} is {} bytes, limit is {}",
            value.len(),
            limits.max_metadata_string_bytes
        );
    }
    Ok(())
}

struct RecordingBodySeed {
    limits: RecordingLimits,
}

impl<'de> DeserializeSeed<'de> for RecordingBodySeed {
    type Value = PiperRecording;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_tuple(
            3,
            RecordingBodyVisitor {
                limits: self.limits,
            },
        )
    }
}

struct RecordingBodyVisitor {
    limits: RecordingLimits,
}

impl<'de> Visitor<'de> for RecordingBodyVisitor {
    type Value = PiperRecording;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("recording v3 body")
    }

    fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let version: u8 = next_field(&mut seq, "version")?;
        if version != RECORDING_VERSION {
            return Err(serde::de::Error::custom(format!(
                "recording body version {version} does not match v3"
            )));
        }

        let metadata = next_seed_field(
            &mut seq,
            "metadata",
            MetadataSeed {
                limits: self.limits,
            },
        )?;
        let frames = next_seed_field(
            &mut seq,
            "frames",
            FrameVecSeed {
                limits: self.limits,
            },
        )?;

        Ok(PiperRecording {
            version,
            metadata,
            frames,
        })
    }
}

struct MetadataSeed {
    limits: RecordingLimits,
}

impl<'de> DeserializeSeed<'de> for MetadataSeed {
    type Value = RecordingMetadata;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_tuple(
            6,
            MetadataVisitor {
                limits: self.limits,
            },
        )
    }
}

struct MetadataVisitor {
    limits: RecordingLimits,
}

impl<'de> Visitor<'de> for MetadataVisitor {
    type Value = RecordingMetadata;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("recording metadata")
    }

    fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let start_time = next_field(&mut seq, "start_time")?;
        let interface = next_seed_field(
            &mut seq,
            "interface",
            BoundedStringSeed {
                field: "interface",
                max_len: self.limits.max_metadata_string_bytes,
            },
        )?;
        let bus_speed = next_field(&mut seq, "bus_speed")?;
        let platform = next_seed_field(
            &mut seq,
            "platform",
            BoundedStringSeed {
                field: "platform",
                max_len: self.limits.max_metadata_string_bytes,
            },
        )?;
        let operator = next_seed_field(
            &mut seq,
            "operator",
            BoundedStringSeed {
                field: "operator",
                max_len: self.limits.max_metadata_string_bytes,
            },
        )?;
        let notes = next_seed_field(
            &mut seq,
            "notes",
            BoundedStringSeed {
                field: "notes",
                max_len: self.limits.max_metadata_string_bytes,
            },
        )?;

        Ok(RecordingMetadata {
            start_time,
            interface,
            bus_speed,
            platform,
            operator,
            notes,
        })
    }
}

struct BoundedStringSeed {
    field: &'static str,
    max_len: usize,
}

impl<'de> DeserializeSeed<'de> for BoundedStringSeed {
    type Value = String;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_seq(BoundedStringVisitor {
            field: self.field,
            max_len: self.max_len,
        })
    }
}

struct BoundedStringVisitor {
    field: &'static str,
    max_len: usize,
}

impl<'de> Visitor<'de> for BoundedStringVisitor {
    type Value = String;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "bounded metadata string {}", self.field)
    }

    fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let len = seq.size_hint().ok_or_else(|| {
            serde::de::Error::custom(format!("missing byte length for {}", self.field))
        })?;
        if len > self.max_len {
            return Err(serde::de::Error::custom(format!(
                "metadata {} is {len} bytes, limit is {}",
                self.field, self.max_len
            )));
        }

        let mut bytes = Vec::with_capacity(len);
        while let Some(byte) = seq.next_element::<u8>()? {
            bytes.push(byte);
        }

        String::from_utf8(bytes).map_err(serde::de::Error::custom)
    }
}

struct FrameVecSeed {
    limits: RecordingLimits,
}

impl<'de> DeserializeSeed<'de> for FrameVecSeed {
    type Value = Vec<TimestampedFrame>;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_seq(FrameVecVisitor {
            limits: self.limits,
        })
    }
}

struct FrameVecVisitor {
    limits: RecordingLimits,
}

impl<'de> Visitor<'de> for FrameVecVisitor {
    type Value = Vec<TimestampedFrame>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("bounded timestamped frame vector")
    }

    fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let len = seq.size_hint().ok_or_else(|| serde::de::Error::custom("missing frame count"))?;
        if len > self.limits.max_frames {
            return Err(serde::de::Error::custom(format!(
                "recording contains {len} frames, limit is {}",
                self.limits.max_frames
            )));
        }

        let mut frames = Vec::with_capacity(len);
        while let Some(frame) = seq.next_element::<BincodeRecordedFrameV3>()? {
            frames.push(TimestampedFrame::try_from(frame).map_err(serde::de::Error::custom)?);
        }

        Ok(frames)
    }
}

fn next_field<'de, A, T>(seq: &mut A, field: &'static str) -> Result<T, A::Error>
where
    A: SeqAccess<'de>,
    T: Deserialize<'de>,
{
    seq.next_element()?.ok_or_else(|| serde::de::Error::missing_field(field))
}

fn next_seed_field<'de, A, S>(
    seq: &mut A,
    field: &'static str,
    seed: S,
) -> Result<S::Value, A::Error>
where
    A: SeqAccess<'de>,
    S: DeserializeSeed<'de>,
{
    seq.next_element_seed(seed)?
        .ok_or_else(|| serde::de::Error::missing_field(field))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::OpenOptions;
    use std::io::Cursor;

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

    fn recording_with_locked_frames() -> PiperRecording {
        PiperRecording {
            version: RECORDING_VERSION,
            metadata: metadata(),
            frames: vec![
                TimestampedFrame::new(
                    PiperFrame::new_standard(0x123, [1, 2, 3]).unwrap().with_timestamp_us(1000),
                    RecordedFrameDirection::Rx,
                    None,
                ),
                TimestampedFrame::new(
                    PiperFrame::new_extended(0x1ABCDE, [9, 10]).unwrap().with_timestamp_us(2000),
                    RecordedFrameDirection::Tx,
                    Some(TimestampSource::Kernel),
                ),
            ],
        }
    }

    fn expected_locked_body_bytes() -> Vec<u8> {
        vec![
            0x03, 0x2A, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, b'c', b'a', b'n', b'0', 0x40, 0x42, 0x0F, 0x00, 0x05, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, b'l', b'i', b'n', b'u', b'x', 0x02, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, b'o', b'p', 0x04, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            b'n', b'o', b't', b'e', 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x23, 0x01,
            0x00, 0x00, 0x00, 0x03, 0x01, 0x02, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0xE8, 0x03,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xDE, 0xBC, 0x1A, 0x00, 0x01, 0x02,
            0x09, 0x0A, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xD0, 0x07, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x01, 0x02,
        ]
    }

    fn expected_locked_file_bytes() -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(MAGIC);
        bytes.push(RECORDING_VERSION);
        bytes.extend_from_slice(&expected_locked_body_bytes());
        bytes
    }

    fn write_file(bytes: &[u8]) -> tempfile::NamedTempFile {
        let temp_file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(temp_file.path(), bytes).unwrap();
        temp_file
    }

    #[test]
    fn standard_and_extended_frames_roundtrip_with_direction_and_sources() {
        let mut recording = PiperRecording::new(metadata());
        recording.frames = vec![
            TimestampedFrame::new(
                PiperFrame::new_standard(0x123, [1]).unwrap().with_timestamp_us(1),
                RecordedFrameDirection::Rx,
                None,
            ),
            TimestampedFrame::new(
                PiperFrame::new_extended(0x123, [2]).unwrap().with_timestamp_us(2),
                RecordedFrameDirection::Tx,
                Some(TimestampSource::Hardware),
            ),
            TimestampedFrame::new(
                PiperFrame::new_standard(0x124, [3]).unwrap().with_timestamp_us(3),
                RecordedFrameDirection::Rx,
                Some(TimestampSource::Kernel),
            ),
            TimestampedFrame::new(
                PiperFrame::new_extended(0x124, [4]).unwrap().with_timestamp_us(4),
                RecordedFrameDirection::Tx,
                Some(TimestampSource::Userspace),
            ),
        ];

        let body = serialize_body(&recording).unwrap();
        let decoded = deserialize_body(&body).unwrap();

        assert_eq!(decoded.version, 3);
        assert_eq!(decoded.frames, recording.frames);
        assert!(decoded.frames[0].frame.is_standard());
        assert!(decoded.frames[1].frame.is_extended());
        assert_eq!(decoded.frames[0].direction, RecordedFrameDirection::Rx);
        assert_eq!(decoded.frames[1].direction, RecordedFrameDirection::Tx);
        assert_eq!(decoded.frames[0].timestamp_source, None);
        assert_eq!(
            decoded.frames[1].timestamp_source,
            Some(TimestampSource::Hardware)
        );
        assert_eq!(
            decoded.frames[2].timestamp_source,
            Some(TimestampSource::Kernel)
        );
        assert_eq!(
            decoded.frames[3].timestamp_source,
            Some(TimestampSource::Userspace)
        );
    }

    #[test]
    fn locked_body_bytes_are_little_endian_fixint_in_field_order() {
        let body = serialize_body(&recording_with_locked_frames()).unwrap();
        assert_eq!(body, expected_locked_body_bytes());
        assert_eq!(body.len(), 116);
        assert_eq!(body[90], 0); // Rx
        assert_eq!(body[91], 0); // timestamp source None
        assert_eq!(body[114], 1); // Tx
        assert_eq!(body[115], 2); // Kernel

        let decoded = deserialize_body(&expected_locked_body_bytes()).unwrap();
        assert_eq!(decoded.frames, recording_with_locked_frames().frames);
    }

    #[test]
    fn locked_file_bytes_include_magic_and_header_version() {
        let temp_file = tempfile::NamedTempFile::new().unwrap();
        save_path(&recording_with_locked_frames(), temp_file.path()).unwrap();
        let bytes = std::fs::read(temp_file.path()).unwrap();

        assert_eq!(&bytes[..9], b"PIPERV1\0\x03");
        assert_eq!(bytes, expected_locked_file_bytes());

        let loaded = load_path(temp_file.path()).unwrap();
        assert_eq!(loaded.frames, recording_with_locked_frames().frames);
    }

    #[test]
    fn streaming_writer_matches_locked_v3_file_bytes() {
        let recording = recording_with_locked_frames();
        let mut writer =
            StreamingRecordingWriter::new(Cursor::new(Vec::new()), &recording.metadata).unwrap();
        for frame in &recording.frames {
            writer.push_frame(frame).unwrap();
        }
        assert_eq!(writer.frame_count(), 2);

        let bytes = writer.finish().unwrap().into_inner();

        assert_eq!(bytes, expected_locked_file_bytes());
        let body = &bytes[RECORDING_FILE_HEADER_BYTES as usize..];
        let decoded = deserialize_body(body).unwrap();
        assert_eq!(decoded.frames, recording.frames);
    }

    #[test]
    fn body_version_mismatch_is_rejected() {
        let mut body = expected_locked_body_bytes();
        body[0] = 4;

        let result = deserialize_body(&body);
        assert!(result.is_err());
    }

    #[test]
    fn v1_and_v2_headers_are_rejected() {
        for version in [1u8, 2u8] {
            let mut bytes = Vec::new();
            bytes.extend_from_slice(MAGIC);
            bytes.push(version);
            bytes.extend_from_slice(&expected_locked_body_bytes());

            let file = write_file(&bytes);
            assert!(load_path(file.path()).is_err());
        }
    }

    #[test]
    fn historical_segmented_shape_is_rejected() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(MAGIC);
        bytes.push(RECORDING_VERSION);
        bytes.extend_from_slice(&4u32.to_le_bytes());
        bytes.extend_from_slice(b"{}");
        bytes.extend_from_slice(&0u64.to_le_bytes());

        let file = write_file(&bytes);
        assert!(load_path(file.path()).is_err());
    }

    #[test]
    fn appended_trailing_bytes_are_rejected() {
        let mut body = expected_locked_body_bytes();
        body.push(0);

        assert!(deserialize_body(&body).is_err());
    }

    #[test]
    fn nonzero_persisted_frame_padding_is_rejected() {
        let mut body = expected_locked_body_bytes();
        let frame_data_padding_offset = 77;
        body[frame_data_padding_offset] = 0xAA;

        assert!(deserialize_body(&body).is_err());
    }

    #[test]
    fn invalid_direction_source_and_format_are_rejected() {
        let body = expected_locked_body_bytes();

        let mut invalid_direction = body.clone();
        invalid_direction[90] = 9;
        assert!(deserialize_body(&invalid_direction).is_err());

        let mut invalid_source = body.clone();
        invalid_source[91] = 9;
        assert!(deserialize_body(&invalid_source).is_err());

        let mut invalid_format = body;
        invalid_format[96] = 9;
        assert!(deserialize_body(&invalid_format).is_err());
    }

    #[test]
    fn small_limits_reject_body_frame_and_string_one_over() {
        let recording = recording_with_locked_frames();
        let body = expected_locked_body_bytes();

        let body_limit = RecordingLimits {
            max_body_bytes: body.len() as u64 - 1,
            ..RecordingLimits::default()
        };
        assert!(serialize_body_with_limits(&recording, body_limit).is_err());
        assert!(deserialize_body_with_limits(&body, body_limit).is_err());

        let frame_limit = RecordingLimits {
            max_frames: recording.frames.len() - 1,
            ..RecordingLimits::default()
        };
        assert!(serialize_body_with_limits(&recording, frame_limit).is_err());
        assert!(deserialize_body_with_limits(&body, frame_limit).is_err());

        let string_limit = RecordingLimits {
            max_metadata_string_bytes: recording.metadata.interface.len() - 1,
            ..RecordingLimits::default()
        };
        assert!(serialize_body_with_limits(&recording, string_limit).is_err());
        assert!(deserialize_body_with_limits(&body, string_limit).is_err());
    }

    #[test]
    fn production_body_limit_prefix_failure_uses_metadata_without_large_allocation() {
        let temp_file = tempfile::NamedTempFile::new().unwrap();
        {
            let mut file = OpenOptions::new().write(true).open(temp_file.path()).unwrap();
            file.write_all(MAGIC).unwrap();
            file.write_all(&[RECORDING_VERSION]).unwrap();
            file.set_len(9 + MAX_RECORDING_BODY_BYTES + 1).unwrap();
        }

        let result = load_path(temp_file.path());
        assert!(result.is_err());
    }

    #[test]
    fn production_frame_count_prefix_failure_does_not_allocate_frames() {
        let mut body = Vec::new();
        body.push(RECORDING_VERSION);
        body.extend_from_slice(&0u64.to_le_bytes());
        body.extend_from_slice(&0u64.to_le_bytes());
        body.extend_from_slice(&0u32.to_le_bytes());
        body.extend_from_slice(&0u64.to_le_bytes());
        body.extend_from_slice(&0u64.to_le_bytes());
        body.extend_from_slice(&0u64.to_le_bytes());
        body.extend_from_slice(&((MAX_RECORDING_FRAMES as u64) + 1).to_le_bytes());

        let result = deserialize_body(&body);
        assert!(result.is_err());
    }

    #[test]
    fn production_metadata_string_prefix_failure_does_not_allocate_string() {
        let mut body = Vec::new();
        body.push(RECORDING_VERSION);
        body.extend_from_slice(&0u64.to_le_bytes());
        body.extend_from_slice(&((MAX_METADATA_STRING_BYTES as u64) + 1).to_le_bytes());

        let result = deserialize_body(&body);
        assert!(result.is_err());
    }
}
