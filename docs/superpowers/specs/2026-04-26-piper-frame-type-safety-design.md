# PiperFrame Type-Safety Redesign

## Goal

Redesign `PiperFrame` so it is always a valid classic CAN 2.0 data frame once constructed.

The current `PiperFrame` is a public-field struct:

```rust
pub struct PiperFrame {
    pub id: u32,
    pub data: [u8; 8],
    pub len: u8,
    pub is_extended: bool,
    pub timestamp_us: u64,
}
```

That shape allows invalid states that the type claims to abstract away:

- standard frames with IDs above `0x7FF`
- extended frames with IDs above `0x1FFF_FFFF`
- DLC values above 8
- mismatch between `len` and the accessible payload
- silent truncation when constructors receive more than 8 bytes
- ambiguous frame format inference from `id > 0x7FF`

The new design must move these checks into the type boundary. CAN adapters, protocol parsers, recording, bridge code, and user code should be able to trust a `PiperFrame` value without re-validating the same invariants.

## Explicit Constraints

This is an intentional breaking API change.

The redesign may break:

- `PiperFrame { ... }` struct literals
- direct field access such as `frame.id`, `frame.data`, `frame.len`, `frame.is_extended`, and `frame.timestamp_us`
- existing `new_standard` / `new_extended` call sites if they assume infallible construction
- existing recording files and bridge protocol payloads
- serde JSON shape for `PiperFrame`

No historical recording or bridge compatibility is required. The new format should be clear and strict rather than backward compatible.

## Non-Goals

This work should not add CAN FD support.

This work should not split timestamp metadata out of `PiperFrame`. Keeping `timestamp_us` inside the frame is less semantically pure than a separate `TimestampedFrame`, but it keeps the change focused on eliminating invalid frame states.

Because timestamp stays inside `PiperFrame`, every recording and bridge-facing model that carries a `PiperFrame` must treat `frame.timestamp_us()` as the authoritative timestamp. It must not duplicate timestamp state beside the frame.

This work should not refactor unrelated protocol parsing or motion-control behavior except where direct `PiperFrame` API usage must change.

## Chosen Approach

Use a strong value-object model:

```rust
pub struct PiperFrame {
    id: CanId,
    data: CanData,
    timestamp_us: u64,
}

pub enum CanId {
    Standard(StandardCanId),
    Extended(ExtendedCanId),
}

pub struct StandardCanId(u16);
pub struct ExtendedCanId(u32);

pub struct CanData {
    bytes: [u8; 8],
    len: u8,
}
```

All fields are private. The only way to construct a frame is through constructors that enforce classic CAN limits.

The frame value types must live in a dedicated module, such as `piper-protocol/src/frame.rs`, and be re-exported from `lib.rs`. They must not be defined directly in `lib.rs` or another ancestor module of protocol submodules, because Rust privacy would allow child modules to construct private fields directly.

Do not expose `pub(crate)`, `pub(super)`, or unchecked constructors for these types. Protocol-owned infallible construction must be explicit and audited:

- Protocol CAN IDs used by `to_frame()` implementations must be typed constants exported by the audited frame module, not values built by sibling protocol modules through a helper.
- The only unchecked or assert-based construction code allowed is private to `frame.rs`.
- Do not expose a public, `pub(crate)`, or `pub(super)` const-validation helper. Rust cannot enforce "only used for constants" once such a helper is visible to sibling modules.
- Existing raw `u32` protocol ID constants must stop being the builder/classifier source of truth. Prefer deleting public raw constants. If raw constants remain public for diagnostics or docs, name and document them as raw diagnostics only and do not use them in frame builders, parsers, or driver filters.
- Dynamic protocol IDs that are statically bounded by validated inputs must be represented by audited accessors exported by the frame module, not by public arrays indexed directly. For example, MIT control IDs should use `mit_control_id(joint: JointIndex) -> StandardCanId`, where `JointIndex` is a validated newtype.
- Fixed-size protocol payloads should use a const-generic payload constructor such as `CanData::from_exact<const N: usize>(bytes: [u8; N]) -> Self`, with `N > 8` rejected at compile time.
- Dynamic payloads and user-provided IDs must remain fallible.

The design must not solve protocol ergonomics by adding a crate-wide unchecked escape hatch. Infallible protocol support is limited to typed constants, typed dynamic-ID accessors exported by `frame.rs`, and fixed-size payload arrays built through `CanData::from_exact`.

The public `piper_protocol::ids` module may re-export these typed constants and accessors for ergonomic API compatibility, but it must not define separate raw constants that become an alternate source of truth. If raw diagnostic aliases remain, they should be clearly named, such as `RAW_ID_ROBOT_STATUS`, and derived from the typed constants.

Required module layout:

```text
piper-protocol/src/
  frame.rs                 # generic CAN value types, private const constructors, and `pub(crate) mod protocol_ids;`
  frame/protocol_ids.rs    # only child module allowed to use frame.rs private constructors
  ids.rs                   # public re-export facade for protocol IDs and classifiers
```

`frame.rs` must keep the unchecked/const construction primitives private and declare `pub(crate) mod protocol_ids;`. The `frame::protocol_ids` module path itself is not public API, but items intentionally exposed from that module should be `pub` so `ids.rs` can re-export them. The `frame::protocol_ids` child module may use `frame.rs` private primitives because it is a descendant of `frame`; sibling protocol modules such as `control.rs`, `feedback.rs`, and `config.rs` must not be able to call them. `ids.rs` should be a facade that re-exports typed constants, dynamic-ID accessors, typed ranges, and classifiers from `frame::protocol_ids`. This keeps generic CAN invariants in one audited privacy boundary while preventing each protocol sibling from constructing IDs independently.

## Type Invariants

`StandardCanId` must only contain IDs in `0x000..=0x7FF`.

`ExtendedCanId` must only contain IDs in `0x00000000..=0x1FFF_FFFF`.

`CanData` must only contain payloads with length `0..=8`.

`PiperFrame` represents classic CAN data frames only. Remote/RTR frames, error frames, CAN FD frames, and backend-specific control/status frames must not be decoded into `PiperFrame`.

`PiperFrame::data()` must always return exactly the valid payload slice.

`PiperFrame::data_padded()` must always return the fixed 8-byte padded storage for backend encoders.

`PiperFrame::dlc()` must always return a value in `0..=8`.

Unused padded bytes must always be zeroed. This makes `data_padded()` deterministic and prevents stale bytes from leaking into backends, bridge payloads, recording files, or tests that compare padded storage.

`CanData::from_padded(bytes, len)` should normalize padded storage: validate `len <= 8`, copy only the active `0..len` bytes, and zero unused bytes. It should not reject a backend buffer only because bytes beyond DLC are nonzero.

Frame format must be explicit. Code must not infer standard versus extended from the numeric ID.

## Public API

The protocol crate should expose these core types:

```rust
pub struct PiperFrame;
pub enum CanId {
    Standard(StandardCanId),
    Extended(ExtendedCanId),
}
pub struct StandardCanId;
pub struct ExtendedCanId;
pub struct CanData;
pub enum FrameError;
pub struct JointIndex;
```

ID construction:

```rust
StandardCanId::new(raw: u32) -> Result<Self, FrameError>
ExtendedCanId::new(raw: u32) -> Result<Self, FrameError>
StandardCanId::raw(self) -> u16
ExtendedCanId::raw(self) -> u32
CanId::standard(raw: u32) -> Result<Self, FrameError>
CanId::extended(raw: u32) -> Result<Self, FrameError>
CanId::raw(self) -> u32
CanId::is_standard(&self) -> bool
CanId::is_extended(&self) -> bool
CanId::as_standard(&self) -> Option<StandardCanId>
CanId::as_extended(&self) -> Option<ExtendedCanId>
impl From<StandardCanId> for CanId
impl From<ExtendedCanId> for CanId
```

`StandardCanId::new` and `CanId::standard` take `u32` rather than `u16` so backend and bridge callers validate raw wire IDs before narrowing. `StandardCanId::raw()` may still return `u16` because the stored value is already validated.

The `raw()` accessors for `StandardCanId`, `ExtendedCanId`, and `CanId` must be `pub const fn` so diagnostics-only raw aliases can be derived from typed constants without duplicating numeric source-of-truth values.

`JointIndex` is a protocol-domain newtype for IDs whose suffix is statically bounded to joints `1..=6`:

```rust
JointIndex::new(raw: u8) -> Result<Self, ProtocolError>
JointIndex::get(self) -> u8
JointIndex::zero_based(self) -> u8
```

`JointIndex::new` must reject `0` and values above `6`. Do not reuse it for protocol domains where `7` means "all motors" or for zero-based public APIs; those domains need separate typed enums or fallible command constructors. If serde is implemented for `JointIndex`, deserialization must enforce the same `1..=6` invariant.

Protocol ID constants used by public APIs should be typed:

```rust
pub const ID_ROBOT_STATUS: StandardCanId;
pub const ID_JOINT_DRIVER_HIGH_SPEED_1: StandardCanId;
pub fn mit_control_id(joint: JointIndex) -> StandardCanId;
```

Callers should compare typed IDs, not raw IDs:

```rust
frame.id().as_standard() == Some(ID_ROBOT_STATUS)
frame.id() == ID_ROBOT_STATUS.into()
```

Code should not use `frame.raw_id() == SOME_U32_CONST` for protocol classification, because that loses frame-format information.

Raw protocol ID replacement matrix:

| Current raw shape | Required typed replacement |
| --- | --- |
| `pub const ID_ROBOT_STATUS: u32` and other fixed IDs | `pub const ID_ROBOT_STATUS: StandardCanId` and equivalent typed constants |
| `FEEDBACK_BASE_ID..=FEEDBACK_END_ID` / `CONTROL_*` / `CONFIG_*` raw ranges | format-aware classifiers such as `FrameType::from_id(id: CanId)` plus typed range helpers where ranges are still needed |
| `ID_JOINT_DRIVER_HIGH_SPEED_BASE + joint_offset` | `joint_driver_high_speed_id(joint: JointIndex) -> StandardCanId` |
| `ID_JOINT_DRIVER_LOW_SPEED_BASE + joint_offset` | `joint_driver_low_speed_id(joint: JointIndex) -> StandardCanId` |
| `ID_JOINT_END_VELOCITY_ACCEL_BASE + joint_offset` | `joint_end_velocity_accel_id(joint: JointIndex) -> StandardCanId` |
| `ID_MIT_CONTROL_BASE + joint_offset` | `mit_control_id(joint: JointIndex) -> StandardCanId` |
| `DRIVER_RX_ROBOT_FEEDBACK_IDS: [u32; N]` | `DRIVER_RX_ROBOT_FEEDBACK_IDS: [StandardCanId; N]` and `driver_rx_robot_feedback_ids() -> &'static [StandardCanId]` |
| `is_robot_feedback_id(id: u32)` | `is_robot_feedback_id(id: CanId) -> bool` or `is_standard_robot_feedback_id(id: StandardCanId) -> bool` |
| `FrameType::from_id(id: u32)` | `FrameType::from_id(id: CanId) -> FrameType` |

Raw aliases may remain only for diagnostics/docs and should be derived from typed constants, for example `pub const RAW_ID_ROBOT_STATUS: u32 = ID_ROBOT_STATUS.raw() as u32;`. Builders, parsers, driver filters, recording stop conditions, statistics, and bridge filters must not consume raw aliases.

Payload construction:

```rust
CanData::new(data: impl AsRef<[u8]>) -> Result<Self, FrameError>
CanData::from_array(bytes: [u8; 8]) -> Self
CanData::from_exact<const N: usize>(bytes: [u8; N]) -> Self
CanData::from_padded(bytes: [u8; 8], len: u8) -> Result<Self, FrameError>
CanData::as_slice(&self) -> &[u8]
CanData::as_padded(&self) -> &[u8; 8]
CanData::len(&self) -> u8
CanData::is_empty(&self) -> bool
```

`CanData::from_array([u8; 8])` represents an active 8-byte payload. It must not be used to mean "padded storage with unknown or inferred length." Backend decode paths that receive fixed storage plus DLC should use `CanData::from_padded(bytes, len)`.

`CanData::from_exact<const N: usize>(bytes: [u8; N])` is for statically sized protocol payloads. The implementation must compile-time reject `N > 8` rather than silently truncating or panicking at runtime.

Use a stable compile-time assertion in the function body, such as:

```rust
const { assert!(N <= 8); }
```

Do not depend on generic const arithmetic trait bounds such as `[(); 8 - N]`; those are not portable on stable Rust. Add a compile-fail test proving `CanData::from_exact([0u8; 9])` fails to compile.

Frame construction:

```rust
PiperFrame::standard(id: StandardCanId, data: CanData) -> Self
PiperFrame::extended(id: ExtendedCanId, data: CanData) -> Self
PiperFrame::new_standard(id: u32, data: impl AsRef<[u8]>) -> Result<Self, FrameError>
PiperFrame::new_extended(id: u32, data: impl AsRef<[u8]>) -> Result<Self, FrameError>
```

All frame constructors initialize `timestamp_us` to `0`. `PiperFrame::with_timestamp_us` is the only public API that sets a nonzero timestamp on an existing frame.

Frame access:

```rust
PiperFrame::id(&self) -> CanId
PiperFrame::raw_id(&self) -> u32
PiperFrame::is_standard(&self) -> bool
PiperFrame::is_extended(&self) -> bool
PiperFrame::dlc(&self) -> u8
PiperFrame::data(&self) -> &[u8]
PiperFrame::data_padded(&self) -> &[u8; 8]
PiperFrame::timestamp_us(&self) -> u64
PiperFrame::with_timestamp_us(self, timestamp_us: u64) -> Self
```

`CanId`, `CanData`, and `PiperFrame` must remain `Copy`, preserving the current zero-allocation hot-path behavior.

The new frame types must preserve the existing ergonomics traits: `Debug`, `Clone`, `Copy`, `PartialEq`, and `Eq`. Losing these traits would expand the breaking change beyond the frame-construction problem and would make tests and diagnostics worse. If implementation discovers a trait is genuinely impossible, that should be treated as a design issue and surfaced before continuing.

`StandardCanId`, `ExtendedCanId`, `CanId`, and `JointIndex` must also implement `Hash`, `PartialOrd`, and `Ord` so they can replace raw integers in allow-lists, maps, statistics, and filter range validation. `CanId::Ord` must use a documented canonical ordering such as `(format_discriminant, raw_id)` with standard and extended IDs in separate format buckets. That ordering is only for stable map/set behavior; protocol ranges and filter ranges must compare `StandardCanId` or `ExtendedCanId` directly, not mixed-format raw IDs.

Protocol-layer command builders that use compile-time CAN ID constants should remain infallible where the IDs and payload sizes are statically known to be valid. Implementations should avoid spreading `unwrap()` or `expect()` through every `to_frame()` method.

The required pattern is:

- Define typed protocol ID constants and typed dynamic-ID accessors once through the audited frame module path.
- Build fixed-size payloads with `CanData::from_exact`.
- Construct frames with `PiperFrame::standard` or `PiperFrame::extended`.

Public constructors for user-provided IDs and payloads must remain fallible.

## Error Model

Add `FrameError` to `piper-protocol`.

Required variants must carry diagnostic payloads, not just human-readable strings:

```rust
pub enum FrameError {
    InvalidStandardId { id: u32 },
    InvalidExtendedId { id: u32 },
    PayloadTooLong { len: usize, max: usize },
    InvalidDlc { dlc: u8 },
    InvalidSerializedFrameFormat { format: u8 },
    NonCanonicalPadding { index: usize, value: u8 },
}
```

Equivalent variant names are acceptable, but implementations must preserve the invalid raw ID, invalid DLC/length, invalid serialized format value, and noncanonical padding byte position/value where applicable.

`FrameError` should be independent from `ProtocolError`. `ProtocolError` is for robot protocol parsing failures after a valid CAN frame exists. `FrameError` is for malformed CAN frame construction or deserialization.

Existing higher-level errors can wrap `FrameError` where needed. For example, replay conversion can map `FrameError` to `RobotError::ConfigError` with context about the recording frame.

Backend and bridge boundaries must preserve enough context to diagnose malformed wire frames:

- `piper-can` should add a `CanError::Frame(FrameError)` variant or map `FrameError` to `CanDeviceErrorKind::InvalidFrame` with the original raw ID, format, DLC, and backend name in the message.
- SocketCAN and GS-USB RX conversion must not collapse all frame construction failures into generic I/O errors.
- Bridge decode should map frame construction failures to `ProtocolError::InvalidData` with field-specific context such as invalid standard ID, invalid extended ID, invalid DLC, or invalid frame format.
- Replay conversion may wrap `FrameError` in client/robot config errors, but the recording frame index and direction should be included.

## Serde Format

When the `serde` feature is enabled, `PiperFrame` must implement custom serde. It must not derive `Serialize` or `Deserialize`.

Human-readable serializers, identified with `Serializer::is_human_readable()` and `Deserializer::is_human_readable()`, must use a strict explicit shape:

```json
{
  "id": 291,
  "format": "standard",
  "data": [1, 2, 3, 4],
  "timestamp_us": 12345
}
```

`format` must be either `"standard"` or `"extended"`.

Deserialization must fail when:

- `format` is missing
- `format` is not recognized
- `format` is `"standard"` and `id > 0x7FF`
- `format` is `"extended"` and `id > 0x1FFF_FFFF`
- `data` has more than 8 bytes
- `id`, `data`, or `timestamp_us` is missing
- duplicate fields are present
- unknown fields are present, including old-shape fields such as `len` or `is_extended`

The serialized `data` field should contain only the active payload bytes, not the padded 8-byte storage.

The serde implementation should not accept the old field shape. Historical recording compatibility is intentionally out of scope.

The strict field-presence model above applies to self-describing serializers such as JSON. For those formats, use a custom visitor or equivalent `deny_unknown_fields` behavior so stale fields cannot be ignored.

Non-human-readable serializers, including `bincode`, must use the bounded binary helper schema below. The serde implementation must branch on `is_human_readable()` so JSON and bincode do not accidentally share one incompatible helper shape.

`bincode` is not self-describing and cannot enforce JSON-style duplicate or unknown field behavior. For `bincode`, strictness means decoding a canonical bounded helper schema and validating it before constructing `PiperFrame`.

The canonical bincode frame schema is:

```rust
struct BincodePiperFrame {
    id: u32,
    format: u8,
    data_len: u8,
    data: [u8; 8],
    timestamp_us: u64,
}
```

The recording v3 body must also have a canonical helper schema. Do not serialize the public Rust recording structs directly if that would leave enum discriminants or option shapes implicit.

```rust
struct BincodePiperRecordingV3 {
    version: u8,
    metadata: BincodeRecordingMetadataV3,
    frames: Vec<BincodeRecordedFrameV3>,
}

struct BincodeRecordingMetadataV3 {
    start_time: u64,
    interface: String,
    bus_speed: u32,
    platform: String,
    operator: String,
    notes: String,
}

struct BincodeRecordedFrameV3 {
    frame: BincodePiperFrame,
    direction: u8,
    timestamp_source: u8,
}
```

`direction` wire values are fixed:

- `0` means RX.
- `1` means TX.
- every other value is invalid.

`timestamp_source` wire values are fixed:

- `0` means none/unavailable.
- `1` means hardware.
- `2` means kernel.
- `3` means userspace.
- every other value is invalid.

The implementation may use public enums such as `RecordedFrameDirection` and `TimestampSource`, but it must convert through the fixed helper schema above before bincode serialization. Tests must lock representative bytes for the full v3 body, not only for individual `PiperFrame` values.

Version `3` recordings must use the current workspace `bincode = "1.3"` fixed-int little-endian wire encoding for this helper schema, with fields encoded in the declaration order shown above. `bincode::serialize` is acceptable for writing because it uses that encoding. Recording reads must not use top-level `bincode::deserialize` directly, because bincode 1.3's top-level helper allows trailing bytes. Use explicit bincode 1.3 options equivalent to `DefaultOptions::new().with_fixint_encoding().with_limit(MAX_RECORDING_BODY_BYTES).reject_trailing_bytes()` for recording bodies so appended bytes and oversized bodies are rejected. Do not switch to bincode 2.x, varint encoding, or a hand-written binary layout without bumping the recording version again. Tests should lock representative v3 byte payloads so the wire contract is explicit.

The v3 body reader must not allow unbounded allocation through `String` or `Vec` length prefixes. Define explicit limits such as `MAX_RECORDING_BODY_BYTES`, `MAX_RECORDING_FRAMES`, and `MAX_METADATA_STRING_BYTES`; apply the bincode byte limit during decode and validate `frames.len()` plus every metadata string byte length after decode. If future recordings need to exceed the default limits, expose that as an explicit reader option rather than using unlimited bincode deserialization. Tests must include oversized body limits, oversized frame-count prefixes, and oversized metadata string lengths.

`format` values are fixed:

- `0` means standard.
- `1` means extended.
- every other value is invalid.

`data_len` must be `0..=8`. Deserialization must validate `format`, `id`, and `data_len` before constructing the canonical `PiperFrame`. The bincode helper must not use `Vec<u8>` for frame payloads because an invalid or malicious encoded length could allocate before the frame length validation runs.

For persisted bincode frames, unused bytes in `data[data_len..]` must be zero. Reject nonzero trailing bytes during recording deserialization. Backend decode paths may still normalize nonzero bytes beyond DLC with `CanData::from_padded`, but persisted recordings should have one canonical byte representation per frame.

Both serde branches must validate by constructing `CanId`, `CanData`, and `PiperFrame` through their canonical constructors. Neither branch may deserialize directly into private fields.

Recording files must reject historical formats using the recording version before attempting to deserialize old frame bodies. The new recording version is `3`; existing version `1` and `2` files must be rejected, not converted. Keep the existing `PIPERV1\0` magic unless there is a separate file-container migration; the version byte is the compatibility boundary for this refactor.

`PiperRecording::version` may remain in the serialized body, but if it remains it must also be `3`. A file with header version `3` and body `recording.version != 3` must be rejected after deserializing the v3 body. If the body version field is removed instead, the header version is the single source of truth and tests must prove there is no second conflicting version field.

Tests should include crafted invalid bincode payloads for bad format values, invalid IDs, `data_len > 8`, nonzero trailing data bytes beyond `data_len`, appended top-level trailing bytes in recording bodies, mismatched header/body versions, and old recording version/shape rejection.

The constrained subtypes also need safe serde behavior. `StandardCanId`, `ExtendedCanId`, `CanId`, and `CanData` must not derive unchecked `Deserialize` implementations that can bypass their constructors. Either they do not expose serde implementations directly, or their deserializers must enforce the same invariants as their constructors.

## Recording Format

Both recording boundary types must migrate:

- `piper-tools::TimestampedFrame`
- `piper_driver::recording::TimestampedFrame`

Neither type should store an ambiguous `can_id + Vec<u8>` model that infers extended frames from `can_id > 0x7FF`.

Preferred shape:

```rust
pub enum RecordedFrameDirection {
    Rx,
    Tx,
}

pub struct TimestampedFrame {
    pub frame: PiperFrame,
    pub direction: RecordedFrameDirection,
    pub timestamp_source: Option<TimestampSource>,
}
```

This makes recording files use the same explicit serde format as live frames. The recorded timestamp is `frame.timestamp_us()`. There must not be a second top-level `timestamp_us` field when the type already stores a `PiperFrame`.

For persisted recordings, `frame.timestamp_us()` must be normalized to a single recording-local monotonic microsecond timebase: elapsed microseconds since the recording session start instant. `metadata.start_time` may remain wall-clock metadata, but replay, duration stop conditions, and recording duration calculations must use the normalized frame timestamps, not wall-clock time and not raw hardware/kernel counters. If an adapter-provided hardware or kernel timestamp cannot be mapped into this recording-local monotonic domain, the recorder must use a userspace monotonic timestamp captured at the receive/send boundary and mark the persisted timestamp provenance accordingly. A v3 recording must not mix incomparable hardware, kernel, and userspace timestamp domains in `frame.timestamp_us()`.

`TimestampSource` remains recording metadata about the persisted timestamp value, not frame validation state. Its semantics must be explicit:

- RX frames should map from adapter-reported timestamp provenance, not from `frame.timestamp_us() != 0`.
- `frame.timestamp_us() != 0` only means a timestamp value is present. It does not prove hardware provenance.
- The source model must preserve the current `Hardware`, `Kernel`, and `Userspace` distinctions.
- TX frames must not be labeled as hardware timestamped solely because they pass through the recording hook. Tools recordings must include explicit direction metadata, such as `RecordedFrameDirection::{Rx, Tx}`, and source should describe timestamp provenance only.
- Replayable TX recordings must stamp the recorded copy with a post-send timestamp converted to the recording-local monotonic timebase before enqueueing it, and should record `timestamp_source: Some(TimestampSource::Userspace)` unless the backend provides a more precise TX timestamp source that is also normalized to the same timebase. Non-replayable TX audit events without a scheduling timestamp may use `timestamp_source: None`, but replay must reject TX frames with `frame.timestamp_us() == 0`.
- `piper-client` must not hardcode every converted driver frame as `TimestampSource::Hardware`.

Existing tools APIs that filter by timestamp source must handle missing source explicitly. For example, `filter_by_source` should either accept `Option<TimestampSource>` or be split into named methods for concrete sources and source-less frames. It must not coerce `None` to `Hardware`, `Kernel`, or `Userspace`.

If `piper-tools::TimestampedFrame` stores `PiperFrame` directly, `piper-tools` must enable the `piper-protocol/serde` feature for its recording serialization dependency path. Concretely, `crates/piper-tools/Cargo.toml` should use an always-enabled serialization dependency such as:

```toml
piper-protocol = { workspace = true, features = ["serde"] }
```

Recording serialization should not rely on a local duplicate frame representation just to avoid enabling serde on `piper-protocol`, because `piper-tools` always serializes recordings.

For `piper_driver::recording::TimestampedFrame`, the preferred shape is:

```rust
pub enum RecordedFrameDirection {
    Rx,
    Tx,
}

pub enum TimestampProvenance {
    Hardware,
    Kernel,
    Userspace,
    None,
}

pub struct TimestampedFrame {
    pub frame: PiperFrame,
    pub direction: RecordedFrameDirection,
    pub timestamp_provenance: TimestampProvenance,
}
```

Driver/CAN receive APIs and hook APIs must carry provenance explicitly. `TimestampProvenance` and `ReceivedFrame` should be owned by `piper-can` so every backend reports the same metadata shape; driver recording can reuse that provenance without depending on `piper-tools`. Required shapes:

```rust
pub struct ReceivedFrame {
    pub frame: PiperFrame,
    pub timestamp_provenance: TimestampProvenance,
}

pub struct RecordedFrameEvent {
    pub frame: PiperFrame,
    pub direction: RecordedFrameDirection,
    pub timestamp_provenance: TimestampProvenance,
}
```

The primary receive traits must migrate to metadata-carrying return values:

```rust
pub trait CanAdapter {
    fn send(&mut self, frame: PiperFrame) -> Result<(), CanError>;
    fn receive(&mut self) -> Result<ReceivedFrame, CanError>;
    fn receive_timeout(&mut self, timeout: Duration) -> Result<ReceivedFrame, CanError>;
    fn try_receive(&mut self) -> Result<Option<ReceivedFrame>, CanError>;
}

pub trait RxAdapter {
    fn receive(&mut self) -> Result<ReceivedFrame, CanError>;
}
```

Driver hooks should receive `RecordedFrameEvent` or separate `frame + direction + provenance` arguments. A hook API that only receives `&PiperFrame` is insufficient because it forces timestamp-source inference from `timestamp_us`. Do not keep the old `receive() -> PiperFrame` as the driver/recording path; if a convenience method that discards metadata is retained for diagnostics, it must be explicitly named so it cannot be confused with the canonical receive API.

If the driver recording hook needs convenience accessors, they should delegate to the inner frame:

```rust
TimestampedFrame::timestamp_us(&self) -> u64
TimestampedFrame::raw_id(&self) -> u32
TimestampedFrame::data(&self) -> &[u8]
```

Both recording types must store `PiperFrame` directly. They must not keep a local duplicate frame representation with `format`, `id`, and `data` fields, because that would create a second frame validation model outside the canonical `PiperFrame` type. Direction and timestamp-provenance metadata may sit beside the frame because they are not frame validity state.

`piper-driver` should not depend on `piper-tools` just to reuse `TimestampSource`. Use driver-local metadata and have `piper-client` map it into the tools recording metadata when saving.

`piper-client` is part of the recording boundary migration, not just a downstream compile fix. The `stop_recording` paths that currently convert driver frames into tools frames must pass through the canonical `PiperFrame` shape without splitting it into `can_id`, padded data, and a separate timestamp. Replay paths that currently construct a TX frame from `can_id + Vec<u8>` and infer extended format from `can_id > 0x7FF` must instead deserialize recorded `Tx` frames and send the recorded `PiperFrame` with explicit frame format.

Stop-recording semantics must be explicit:

- Manual stop detaches the recording hook first, then drains all frames already accepted by the hook.
- All stop modes share one synchronized hook state machine. Each callback computes the normalized recording-local timestamp, evaluates stop eligibility, and either accepts or rejects the frame under the same synchronization boundary.
- Once a stop condition closes acceptance, future callbacks must be suppressed or rejected before enqueueing. In-flight callbacks that already passed the accept decision must be drained before `stop_recording` returns.
- Duration-based stop includes frames whose normalized recording-local timestamp is at or before the deadline, then atomically closes acceptance.
- Frame-count stop includes exactly the first `N` accepted frames and closes acceptance immediately after accepting frame `N`.
- CAN-ID stop conditions must be format-aware, such as `StopCondition::OnCanId(CanId)`, include the trigger frame, and close acceptance in the same callback that accepts the trigger frame.
- Frames accepted after detach must not appear in the saved recording.

Existing tests and semantics that save `frame.data.to_vec()` from padded storage must be rewritten. Recording files should contain only the active payload bytes in JSON/self-describing views and the canonical `data_len + [u8; 8]` bincode shape in binary recordings. Tests must not assert or depend on padded trailing zero bytes as part of the active payload.

Historical recording compatibility is intentionally removed. Existing legacy recording conversion paths, including `LegacyPiperRecording`-style readers that deserialize old `can_id + Vec<u8>` frames, should be deleted. The recording file format version should be bumped to `3`, and tests should prove old-shape serialized data is rejected rather than silently converted.

Replay must treat the recorded frame timestamp as scheduling metadata only. Default replay sends only frames whose `direction == Tx`; RX frames are skipped for transmission and may only be used for diagnostics. If a future mode re-injects RX frames, it must be an explicit opt-in mode with a name that makes the risk clear. Scheduling is relative to the first selected TX frame, not the first frame in the file. Selected TX frames must be processed in file order; replay must reject decreasing selected-TX timestamps, allow equal timestamps as zero-delay sends, and compute each delay from the previous selected TX frame. Before sending each replayed TX frame, replay must clear the timestamp with `with_timestamp_us(0)` so capture timestamps do not leak into TX hooks, TX recordings, or backend send paths.

## Bridge Protocol Format

The bridge protocol must encode explicit frame format.

The binary bridge protocol already carries `id`, `is_extended`, `len`, and 8 data bytes. The decoder should construct a `PiperFrame` through the checked constructors instead of a struct literal.

The bridge decoder must treat every boolean field as canonical. Accept only `0` and `1`; reject any other value as malformed protocol data. This includes frame `is_extended`, `SetRawFrameTap.enabled`, `LeaseDenied.has_holder`, and any future boolean fields.

Bridge send-frame requests do not encode or trust a timestamp from the client. A request decoded into `PiperFrame` should have `timestamp_us() == 0`. Receive-frame events carry the host/device timestamp and should apply it through `with_timestamp_us()` when constructing the event frame.

The request encoder should not include `timestamp_us()`. The event encoder should read from frame methods:

- `raw_id()`
- `is_extended()`
- `dlc()`
- `data_padded()`
- `timestamp_us()`

Malformed bridge payloads should fail decode at the protocol boundary, not produce invalid `PiperFrame` values.

Bridge filtering must use a format-aware filter model. `CanIdFilter` should match on `CanId`, not raw numeric ID alone, so `standard 0x123` and `extended 0x123` can be treated differently.

Bridge v3 should use new request tags for every request that carries filters:

- `TAG_HELLO_V3 = 0x09` replaces old `TAG_HELLO = 0x01`.
- `TAG_SET_FILTERS_V3 = 0x0A` replaces old `TAG_SET_FILTERS = 0x03`.
- Old `TAG_HELLO` and `TAG_SET_FILTERS` wire tags must be rejected, not decoded as v3.
- Other request tags may keep their existing values if their payloads are unchanged, but canonical boolean decoding still applies to all tags.
- Tests must lock representative byte vectors for `TAG_HELLO_V3` and `TAG_SET_FILTERS_V3`.

The public bridge filter type should be typed and hard to misuse:

```rust
pub enum CanIdFilter {
    Standard {
        min: StandardCanId,
        max: StandardCanId,
    },
    Extended {
        min: ExtendedCanId,
        max: ExtendedCanId,
    },
}

CanIdFilter::standard(min: StandardCanId, max: StandardCanId) -> Result<Self, CanIdFilterError>
CanIdFilter::extended(min: ExtendedCanId, max: ExtendedCanId) -> Result<Self, CanIdFilterError>
CanIdFilter::matches(&self, id: CanId) -> bool
```

Filter constructors must reject `min > max`. `CanIdFilterError` may be a bridge-local public error or an existing bridge protocol error, but it must distinguish invalid range from malformed wire decode. If public fields are kept instead, the encoder must validate them before writing and return an error for invalid local values; it must not serialize invalid filters.

The v3 filter wire schema is:

```text
filter_count: u16
repeated filter_count times:
  format: u8
  min_id: u32
  max_id: u32
```

`format` values are fixed:

- `0` means standard IDs only.
- `1` means extended IDs only.
- every other value is invalid.

There is no `both` value. A caller that wants the same numeric range for both formats must send two filters. Filter decoding must validate `min_id <= max_id`, the selected format, and the ID range for that format. Invalid filters should fail request decode at the bridge protocol boundary.

## CAN Backend Impact

SocketCAN and GS-USB TX paths should stop defensively slicing with unchecked `frame.len`.

Backend encoding should use:

- `frame.raw_id()`
- `frame.is_extended()`
- `frame.dlc()`
- `frame.data()`
- `frame.data_padded()` where the backend protocol requires fixed storage

RX conversion should validate backend-provided IDs and DLC values through `PiperFrame` constructors. Malformed data frames received from a device or raw socket, including invalid IDs or DLC above 8, should become backend errors instead of invalid SDK values. Backend-specific non-data frames are classified separately below and may be recoverable or fatal depending on their semantics.

RX conversion must also reject non-data frames before constructing `PiperFrame`. The invariant is that remote/RTR frames, error frames, CAN FD frames, and backend control/status frames are never exposed as `PiperFrame`. The backend may preserve its current strategy for those frames: return an error for fatal conditions, ignore/log recoverable conditions, or map them to backend-specific events. This refactor should not force every non-data frame to become a hard receive error.

SocketCAN split and unsplit paths must validate raw `libc::can_frame` flags before any conversion that could mask those flags. Extract this into a hardware-independent raw parser shared by split and unsplit paths, such as:

```rust
enum ParsedSocketCanFrame {
    Data(PiperFrame),
    RecoverableNonData,
    Fatal(CanError),
}

parse_libc_can_frame_bytes(
    bytes: &[u8],
    msg_len: usize,
    msg_flags: i32,
) -> ParsedSocketCanFrame
```

SocketCAN RX must read into a `CANFD_MTU`-sized buffer, pass the actual received byte count and message flags to the parser, and reject `MSG_TRUNC`. The raw parser must require the received message length to be exactly `CAN_MTU` for classic CAN data frames. It must reject `CANFD_MTU`, truncated MTUs, oversized MTUs, `CAN_RTR_FLAG`, and DLC above 8 before constructing a `PiperFrame`. `CAN_ERR_FLAG` frames must never become `PiperFrame`. Tests should directly construct raw byte buffers and call this pure parser without requiring `vcan0`, SocketCAN sockets, or hardware.

SocketCAN error-frame mapping must be explicit and shared by split and unsplit paths:

| Raw error condition | Parser result |
| --- | --- |
| `CAN_ERR_FLAG` with malformed MTU or DLC not equal to Linux `CAN_ERR_DLC` | `Fatal(CanError::Frame(...))` or `Fatal(CanError::Device(InvalidFrame))` with raw context |
| `CAN_ERR_BUSOFF` error class, or controller payload with `CAN_ERR_CRTL_TX_BUS_OFF` / `CAN_ERR_CRTL_RX_BUS_OFF` | `Fatal(CanError::BusOff)` |
| controller payload with `CAN_ERR_CRTL_RX_OVERFLOW` / `CAN_ERR_CRTL_TX_OVERFLOW` | `Fatal(CanError::BufferOverflow)` |
| warning/passive/restarted/ACK/protocol/bus-error classes without bus-off or overflow | `RecoverableNonData` with log/counter |
| unknown but well-formed error-frame class | `RecoverableNonData` with raw error class logged |

This table replaces split/unsplit divergence and avoids string-matching parsed SocketCAN error descriptions.

All RX conversion surfaces must reject non-data and malformed frames before constructing `PiperFrame`, including split and unsplit SocketCAN paths, split and unsplit GS-USB paths, GS-USB batch receive paths, and bridge protocol decode paths. SocketCAN MTU and `libc::can_frame` parsing requirements apply only to SocketCAN.

GS-USB migration should use one shared conversion function for device frames wherever possible. That function must cover:

- unsplit direct construction points that currently use `can_dlc.min(8)`
- split receive reconstruction in `gs_usb/split.rs`
- batch receive behavior when a batch contains mixed valid and invalid frames
- invalid DLC values above 8
- RTR, error, CAN FD, control, and status frames

GS-USB batch behavior must use one state machine across unsplit `receive_batch_frames()`, unsplit `receive()` queueing, and split `receive()`.

Classify every parsed GS-USB device frame before conversion:

- `ValidData`: classic CAN data frame, valid format, valid ID, DLC `0..=8`.
- `RecoverableNonData`: echo frames and benign RTR/error/CAN FD/control/status frames that do not indicate adapter state loss and are not malformed data frames.
- `FatalMalformedData`: data-looking frames with invalid IDs, invalid format flags, or DLC above 8.
- `FatalDeviceStatus`: overflow, bus-off, device status/error frames that indicate adapter state loss, including an overflow flag attached to an otherwise valid-looking data frame.
- `FatalTransport`: NoDevice, AccessDenied, NotFound, invalid USB response shape, incomplete frame packet, or other unrecoverable transport errors returned by the transport layer.

Receive timeout/no-data is not a device-frame class and must not be treated as `FatalTransport`. A read timeout returns the existing timeout/no-data error for that receive attempt and must not poison queued valid frames from an earlier successfully parsed batch. Fatal transport is reserved for conditions where the adapter state or USB stream can no longer be trusted.

The shared `classify_gs_usb_frame` contract must be defined in terms of `echo_id`, `can_id` flags/masks, `can_dlc`, `channel`, `flags`, `reserved`, and payload bytes:

- `echo_id != GS_USB_RX_ECHO_ID` is `RecoverableNonData` unless the frame also carries a fatal device-status flag.
- `flags & GS_CAN_FLAG_OVERFLOW != 0` is `FatalDeviceStatus`, even if the rest of the frame looks like valid data.
- Unsupported nonzero `reserved` bits or unsupported frame flags are not silently ignored; classify them as `FatalMalformedData` for data-looking frames or `RecoverableNonData` for explicit non-data/control/status frames.
- `CAN_ERR_FLAG` uses the same bus-off/overflow/recoverable mapping as SocketCAN error frames where the payload exposes Linux-compatible error bytes.
- `CAN_RTR_FLAG` and CAN FD/control/status frames are `RecoverableNonData` unless they also carry fatal device-status information.
- For standard data frames, any ID bits outside `CAN_SFF_MASK` are `FatalMalformedData`; for extended data frames, validate the `CAN_EFF_FLAG` format and `CAN_EFF_MASK` ID through `ExtendedCanId`.
- `can_dlc > 8` is always `FatalMalformedData` for data-looking frames; do not clamp with `.min(8)`.

Required observable behavior:

- `RecoverableNonData` frames are skipped and logged/counted. They never become `PiperFrame` and do not make the receive call fail.
- `FatalMalformedData`, `FatalDeviceStatus`, or `FatalTransport` makes the current receive call return `Err`.
- If a device batch contains any fatal frame, discard all valid frames from that device batch and return `Err`; do not partially return or queue them.
- If a device batch contains only valid and recoverable frames, preserve order among returned valid frames.
- For `[valid A, recoverable X, valid B]`, `receive_batch_frames()` returns `Ok([A, B])`, unsplit `receive()` returns `A` then `B`, and split `receive()` returns `A` then `B`.
- For `[valid A, fatal X, valid B]` or `[fatal X, valid B]`, every API returns `Err` for that device batch and none of `A` or `B` is returned later.

Tests must exercise these exact sequences for unsplit batch, unsplit single receive queueing, and split receive.

## Protocol Layer Impact

Protocol parsing should replace direct field access with methods.

Examples:

- `frame.id` becomes `frame.raw_id()`
- `frame.len` becomes `frame.dlc()`
- `frame.data[0]` becomes `frame.data()[0]` after length checks
- `frame.data[..copy_len]` becomes `frame.data()[..copy_len]`

Tests that currently use invalid standard CAN IDs such as `0x999` for "unknown robot protocol ID" should use a legal CAN ID outside the robot protocol set, such as `0x700`, or explicitly construct an extended frame when the test needs an extended ID.

Protocol parsers must check frame format, not just `raw_id()`. Add tests with the same numeric ID in both formats, such as `standard 0x251` versus `extended 0x251` and `standard 0x123` versus `extended 0x123`, proving robot standard-frame messages do not parse from extended frames.

Protocol classification and driver filtering APIs must also become format-aware. Raw-ID helpers such as `FrameType::from_id(u32)`, `is_robot_feedback_id(u32)`, and raw `DRIVER_RX_ROBOT_FEEDBACK_IDS: [u32; ...]` style tables should be removed or replaced with APIs that take `CanId` or `StandardCanId`. Extended frames with the same numeric IDs as robot standard-frame messages must classify as `Unknown` or fail parsing before they can enter robot feedback pipelines.

The same format-aware rule applies to non-protocol callers that classify, count, filter, or stop on CAN IDs:

- recording stop conditions such as `StopCondition::OnCanId`
- piper-tools statistics and ID frequency aggregation
- bridge filters and raw frame taps
- client/driver diagnostics that group by CAN ID

These APIs should store `CanId` or typed standard/extended IDs. If a human-facing report needs a numeric key, it should display both format and raw ID, such as `standard:0x251` or `extended:0x251`.

## Driver And Client Impact

Driver and client code should treat `PiperFrame` as immutable.

Timestamp updates should use:

```rust
frame = frame.with_timestamp_us(timestamp_us);
```

TX code must treat `timestamp_us` as receive/recording metadata, not command metadata. Replay, bridge send requests, adapter send hooks, TX recordings, and backend encoders must either ignore TX timestamps or explicitly clear them to `0` before sending. Tests should prove commands observed at TX boundaries have `timestamp_us() == 0` unless a specific test-only hook is intentionally inspecting replay scheduling metadata before send normalization.

Tests should use small helper builders instead of struct literals. This keeps test setup clear and prevents test-only invalid frames from re-entering the codebase.

## Public Re-Exports

`piper-can` and `piper-sdk` should re-export the new frame types alongside `PiperFrame`:

```rust
pub use piper_protocol::{
    CanData, CanId, ExtendedCanId, FrameError, JointIndex, PiperFrame, StandardCanId,
};
```

Required public paths:

- `piper_protocol::{CanData, CanId, ExtendedCanId, FrameError, JointIndex, PiperFrame, StandardCanId}`
- `piper_protocol::frame::{CanData, CanId, ExtendedCanId, FrameError, PiperFrame, StandardCanId}`
- `piper_protocol::ids::{...}` for typed protocol ID constants, typed dynamic-ID accessors, `JointIndex`, and format-aware classifiers
- `piper_can::{CanData, CanId, ExtendedCanId, FrameError, PiperFrame, ReceivedFrame, StandardCanId, TimestampProvenance}`
- `piper_sdk::{CanData, CanId, ExtendedCanId, FrameError, JointIndex, PiperFrame, ReceivedFrame, StandardCanId, TimestampProvenance}`
- `piper_sdk::can::{CanData, CanId, ExtendedCanId, FrameError, PiperFrame, ReceivedFrame, StandardCanId, TimestampProvenance}`
- `piper_sdk::prelude::{CanData, CanId, ExtendedCanId, FrameError, JointIndex, PiperFrame, ReceivedFrame, StandardCanId, TimestampProvenance}`
- `piper_sdk::ids::{...}` for typed protocol ID constants, typed dynamic-ID accessors, `JointIndex`, and format-aware classifiers
- `piper_driver::recording::{RecordedFrameDirection, RecordedFrameEvent, TimestampProvenance, TimestampedFrame}`
- `piper_tools::{PiperRecording, RecordedFrameDirection, RecordingMetadata, TimestampSource, TimestampedFrame}`

This keeps downstream users from needing to depend on `piper-protocol` only to construct frames and preserves ergonomic imports for examples and addons. `JointIndex` is intentionally not required from `piper_can` because it is a robot-protocol domain type, not a generic CAN adapter type.

Do not add a `piper_protocol::can::{...}` compatibility path unless a current public API already exposes it. If such a path exists during implementation, either migrate it to the typed frame API explicitly or remove it as part of the breaking change; do not leave an old raw-field compatibility module behind.

## Migration Strategy

The implementation should be done as a single focused refactor rather than compatibility layers.

This migration sequence is allowed to be temporarily non-green. Once `PiperFrame` fields become private and constructors become fallible, downstream crates will not compile until their migration steps are complete. Do not add compatibility fields, deprecated constructors, or unchecked escape hatches just to make intermediate steps compile. Full workspace verification is expected after step 9.

Recommended order:

1. Add RED tests in `piper-protocol` for invalid IDs, oversized payloads, no truncation, format preservation, timestamp preservation, serde rejection, and same-raw-ID standard/extended behavior.
2. Implement the new frame value types, private `PiperFrame` fields, `JointIndex`, typed protocol ID constants/accessors, and `CanData::from_exact`.
3. Migrate `piper-protocol` internals and tests to method access and explicit frame format checks.
4. Migrate `piper-can` backend encode/decode and bridge protocol, including `ReceivedFrame`-style RX metadata, unified GS-USB RX conversion, bridge protocol version/tag changes, and typed bridge filters.
5. Enable `piper-protocol = { workspace = true, features = ["serde"] }` for `piper-tools`, migrate the full recording body schema to version `3`, and delete legacy `can_id + Vec<u8>` readers.
6. Migrate `piper-driver` recording hooks to store `PiperFrame` directly and preserve active-payload, direction, and timestamp-provenance semantics, including userspace post-send timestamps for replayable TX recordings.
7. Migrate `piper-client` stop-recording and replay boundaries so they stop splitting frames into `can_id`, padded data, inferred format, and duplicate timestamp state. Replay should select TX frames only by default.
8. Migrate remaining workspace crates: `piper-control`, `piper-sdk`, `apps/cli`, examples, rustdoc examples, README snippets, and addon tests.
9. Remove all remaining direct field access, struct literals, legacy recording types, and ambiguous `can_id + Vec<u8>` frame conversion helpers.

The refactor should not keep deprecated constructors or public fields. Compile errors should guide migration.

Workspace feature wiring must be part of the migration, not deferred to verification. Every internal dependency on `piper-can` from crates such as `piper-sdk`, `piper-driver`, and `piper-client` must use `default-features = false` and forward `mock`, `serde`, `socketcan`, `gs_usb`, or `auto-backend` explicitly. `cargo check -p piper-sdk --no-default-features` is not meaningful if a transitive dependency still enables `piper-can/default` or `piper-can/auto-backend`.

## Verification Plan

Required checks:

```bash
cargo fmt --all -- --check
cargo check --all-targets
cargo check --workspace --all-targets --no-default-features
cargo test --doc --workspace
cargo test -p piper-protocol --features serde
cargo test -p piper-tools
cargo test -p piper-can --no-default-features --features mock
cargo test -p piper-can --no-default-features --features serde
cargo test -p piper-sdk --no-default-features
cargo test --workspace --all-targets
cargo test --workspace --all-targets --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo doc --workspace --no-deps --document-private-items
cargo doc --workspace --all-features --no-deps
```

Feature graph checks must prove no backend/default features leak into no-default builds:

```bash
cargo tree -p piper-sdk -e features --no-default-features
cargo tree -p piper-driver -e features --no-default-features
cargo tree -p piper-client -e features --no-default-features
```

The expected result is that none of those trees contains `piper-can feature "default"` or `piper-can feature "auto-backend"` unless that feature was explicitly requested by the command being tested.

On Linux, also check backend-specific feature paths:

```bash
cargo test -p piper-can --no-default-features --features socketcan
cargo test -p piper-can --no-default-features --features gs_usb
```

Addon checks should also run because examples currently use `PiperFrame` through public API paths:

```bash
cargo fmt --manifest-path addons/piper-physics-mujoco/Cargo.toml -- --check
just check-physics
just test-physics
just clippy-physics
```

Hardware ignored tests are not required for this refactor unless hardware is explicitly available.

Required targeted tests:

- `piper-protocol` JSON serde tests for valid frames and malformed shapes: missing fields, duplicate fields, unknown fields, old `len` / `is_extended` fields, invalid `format`, invalid IDs, oversized data, and same raw ID represented as standard versus extended.
- `piper-protocol` serde tests proving human-readable serializers use the JSON helper shape and non-human-readable serializers use `BincodePiperFrame`.
- `piper-protocol` bincode tests with crafted payloads for invalid `format` values, invalid standard and extended IDs, `data_len > 8`, nonzero trailing data bytes inside `BincodePiperFrame`, and same raw ID represented as standard versus extended.
- `piper-protocol` constructor tests proving `CanData::new`, `PiperFrame::new_standard`, and `PiperFrame::new_extended` reject oversized payloads without truncation.
- `piper-protocol` constructor tests proving every frame constructor initializes `timestamp_us()` to `0` and `with_timestamp_us` is the only timestamp-setting API.
- `piper-protocol` error tests proving `FrameError` preserves invalid raw ID, invalid DLC/length, serialized format value, and noncanonical padding byte context.
- `piper-protocol` compile-fail or trybuild tests proving external crates cannot use direct field access, invalid construction paths, unchecked/const ID helpers, or `CanData::from_exact([0u8; 9])`.
- `piper-protocol` tests proving `JointIndex` accepts only `1..=6`, exposes `get()` / `zero_based()`, and is not used for protocol domains where `7` means "all motors".
- `piper-protocol` tests proving typed ID types and `JointIndex` implement `Hash`, `PartialOrd`, and `Ord` for maps, sets, and range validation.
- `piper-protocol` tests proving protocol-owned infallible construction paths build valid typed constants, dynamic-ID accessors, and fixed payloads without `unwrap()` / `expect()` in every `to_frame()`.
- `piper-protocol` tests proving raw numeric protocol ID constants are not used by builders/classifiers, or are only diagnostics-only aliases derived from typed constants.
- `piper-protocol` and `piper-driver` tests proving classification/filter APIs take `CanId` or `StandardCanId`, accept `standard 0x251`, and reject/classify as unknown `extended 0x251`.
- tests for recording stop conditions, statistics, diagnostics, and ID aggregation proving `standard 0x123` and `extended 0x123` are distinct.
- `piper-protocol` tests proving constrained subtypes cannot be deserialized into invalid values when serde is enabled.
- `piper-protocol` tests for `CanData::from_padded`: reject `len > 8`, preserve active bytes, and zero nonzero bytes beyond DLC.
- `piper-tools` bincode roundtrip tests for version `3` recordings containing standard and extended frames using direct `PiperFrame` storage, plus locked representative byte payloads for the full recording-body bincode 1.3 fixed-int wire contract.
- `piper-tools` rejection tests for version `1` and `2` recordings, header/body version mismatch, appended top-level bincode trailing bytes, oversized body/frame-count/metadata string limits, nonzero trailing bytes inside frame payload storage, invalid direction values, invalid timestamp source values, and old recording frame shapes that lack explicit frame format.
- `piper-tools` and `piper-driver` structure tests or static checks proving recording types do not duplicate timestamp, ID, or data beside `PiperFrame`.
- `piper-driver` recording tests proving `TimestampedFrame` stores `PiperFrame` directly, records normalized recording-local monotonic timestamps, records only active payload semantics, and preserves RX/TX direction plus `Hardware` / `Kernel` / `Userspace` / `None` timestamp provenance through receive metadata and hook APIs.
- `piper-driver` TX recording tests proving a replayable TX recording stamps a copy with a post-send userspace timestamp normalized into the recording-local timebase while the frame observed by backend send remains `timestamp_us() == 0`.
- `piper-client` stop-recording tests proving client recording boundaries preserve the canonical `PiperFrame` format without `can_id + Vec<u8>` conversion, map driver direction/timestamp-provenance metadata into tools recording metadata without inferring source from nonzero timestamps or hardcoding every frame as hardware sourced, and apply deterministic synchronized accept/close/drain/trigger-frame semantics for manual, duration, frame-count, and CAN-ID stops.
- replay tests proving default replay sends only `Tx` frames, schedules relative to the first selected TX frame, rejects TX frames with timestamp `0`, rejects decreasing selected-TX timestamps, treats equal timestamps as zero-delay sends, preserves selected TX file order, and clears timestamps to `0` before TX, including adapter-observed frames, TX hooks, TX recordings, and backend send paths.
- `piper-can` bridge protocol tests for invalid canonical booleans across all boolean fields, invalid IDs, invalid DLC, request timestamp zeroing, event timestamp preservation, same raw ID standard/extended behavior, concrete `TAG_HELLO_V3 = 0x09` / `TAG_SET_FILTERS_V3 = 0x0A` locked byte vectors, old tag rejection, typed filter construction, encoder rejection of invalid local filter values, format-aware filter matching, invalid filter format values, and invalid filter ranges.
- `piper-client` bridge host/session dispatch tests proving standard and extended frames with the same raw numeric ID route differently through filters, raw taps, and diagnostics, and proving no host-side raw-`u32` filter path remains.
- Backend decode tests for invalid IDs, DLC above 8, CAN FD, RTR, error, GS-USB control/status frame rejection where such frames can be constructed without hardware. Cover SocketCAN parser classification before flag loss, the explicit SocketCAN error-frame mapping table, `CANFD_MTU`-sized receive buffers, `MSG_TRUNC` rejection, split and unsplit SocketCAN use of that parser, split and unsplit GS-USB, unsplit GS-USB `receive()` queueing, `receive_batch_frames()`, GS-USB split receive paths, and timeout/no-data behavior as non-fatal control flow.
- GS-USB tests for exact mixed-batch sequences: `[valid A, recoverable X, valid B]` returns `A` then `B`; `[valid A, malformed X, valid B]` returns `Err` and returns neither `A` nor `B` later; `[valid A, fatal X, valid B]` returns `Err` and returns neither `A` nor `B` later; `[fatal X, valid B]` returns `Err` and returns neither `X` nor `B` later. Cover unsplit batch, unsplit queued receive, and split receive for each sequence.
- Static repository searches for `LegacyPiperRecording`, `can_id + Vec<u8>` frame conversions, direct `PiperFrame` field access, and `PiperFrame { ... }` struct literals.

Required static searches should be scripted or documented with concrete commands. At minimum:

```bash
rg -n 'PiperFrame\s*\{' crates apps addons tests README*.md QUICKSTART*.md docs -g '*.rs' -g '*.md' -g '!docs/superpowers/specs/2026-04-26-piper-frame-type-safety-design.md'
rg -n '\.(timestamp_us|is_extended|len|data|id)\b' crates apps addons tests README*.md QUICKSTART*.md docs -g '*.rs' -g '*.md' -g '!docs/superpowers/specs/2026-04-26-piper-frame-type-safety-design.md'
rg -n 'can_id\s*>\s*0x7[Ff]{2}|data\.to_vec\(\)|TimestampedFrame\s*\{|new_standard\([^\n]*can_id|LegacyPiperRecording' crates apps addons tests README*.md QUICKSTART*.md docs -g '*.rs' -g '*.md' -g '!docs/superpowers/specs/2026-04-26-piper-frame-type-safety-design.md'
rg -n 'FrameType::from_id|is_robot_feedback_id|DRIVER_RX_ROBOT_FEEDBACK_IDS|raw_id\(\)\s*==' crates apps addons tests README*.md QUICKSTART*.md docs -g '*.rs' -g '*.md' -g '!docs/superpowers/specs/2026-04-26-piper-frame-type-safety-design.md'
rg -n 'StopCondition::OnCanId\([^C]|HashMap<\s*u32|BTreeMap<\s*u32|frequency\([^)]*can_id|add_frame\([^)]*can_id' crates apps addons tests README*.md QUICKSTART*.md docs -g '*.rs' -g '*.md' -g '!docs/superpowers/specs/2026-04-26-piper-frame-type-safety-design.md'
```

The searches intentionally exclude this design spec because it documents the old shape and the forbidden patterns. The direct-field search will still have false positives; the verification step must review hits and prove no hit is `PiperFrame` direct field access or legacy recording/replay conversion. Public docs, README snippets, rustdoc examples, and addon examples must be migrated when they demonstrate user-facing API usage; archived design docs may keep historical examples only if clearly marked as historical.

## Acceptance Criteria

No public API can construct a `PiperFrame` with invalid standard ID, invalid extended ID, or DLC above 8.

No `PiperFrame` constructor silently truncates payload data.

All `PiperFrame` constructors initialize `timestamp_us()` to `0`; nonzero timestamps are applied only through `with_timestamp_us`.

No code in the repository uses `PiperFrame { ... }` struct literals.

No code in the repository directly reads or writes `PiperFrame` fields.

Implementation verification should include repository searches for remaining Rust direct-access patterns, at minimum over `*.rs` files. Documentation snippets, READMEs, and examples should be migrated when they show public API usage.

Serde deserialization rejects malformed frames.

Serde uses human-readable JSON shape for human-readable serializers and bounded `BincodePiperFrame` shape for non-human-readable serializers.

Recording and bridge boundaries preserve explicit standard versus extended frame format.

The frame module privacy layout prevents sibling protocol modules from constructing typed IDs through unchecked helpers; typed protocol IDs are created only through the audited `frame::protocol_ids` child module.

`JointIndex` exists for `1..=6` dynamic protocol IDs and is not reused for other joint-selection domains.

Typed ID values support the collection traits needed to replace raw integers in maps, sets, ranges, filters, and statistics.

Typed protocol ID constants and dynamic-ID accessors are the builder/classifier source of truth. Raw numeric protocol IDs, if any remain, are diagnostics-only and not used by frame builders, protocol classifiers, or driver filters.

Protocol classification, driver filtering, recording stop conditions, statistics, diagnostics, and bridge filtering APIs are format-aware and take `CanId` or typed standard/extended IDs, not raw `u32` IDs.

Protocol parsers reject or ignore extended frames whose raw numeric IDs match standard robot message IDs.

Recording code has no remaining legacy conversion path from ambiguous `can_id + Vec<u8>` frames.

Recording files use version `3`; version `1` and `2` files are rejected before old frame bodies are deserialized.

Recording bincode uses the declared full-body bincode 1.3 fixed-int wire contract, rejects appended top-level body bytes, enforces explicit body/frame-count/metadata string limits, rejects nonzero unused frame payload bytes, rejects invalid direction/source values, and rejects header/body version mismatches.

Recording types store `PiperFrame` directly and do not duplicate timestamp, ID, format, or data state.

Persisted recording frame timestamps use one recording-local monotonic microsecond timebase. Timestamp source/provenance describes that persisted timestamp value and is never inferred from `timestamp_us() != 0`.

Driver receive and hook APIs carry timestamp provenance explicitly. Driver recording metadata preserves RX/TX direction and `Hardware` / `Kernel` / `Userspace` / `None` timestamp provenance without making `piper-driver` depend on `piper-tools`.

Client recording conversion maps timestamp provenance explicitly and does not infer `TimestampSource` from `frame.timestamp_us() != 0`.

Recording tests and serialized data use active-payload semantics, not padded 8-byte payloads as active data.

Replay sends only TX frames by default, schedules relative to the first selected TX frame, rejects replayable TX frames with timestamp `0`, rejects decreasing selected-TX timestamps, treats equal selected-TX timestamps as zero-delay sends, and clears timestamps before send. Bridge send requests, TX hooks, TX recordings, and backend send paths do not leak recorded or receive timestamps into transmitted frames.

Replayable TX recording stamps the recorded copy with a post-send userspace timestamp normalized to the recording-local timebase while preserving `timestamp_us() == 0` at backend send boundaries.

Stop-recording behavior has deterministic synchronized accept, close, drain, trigger-frame, and post-detach exclusion semantics across manual, duration, frame-count, and CAN-ID stop modes.

Bridge decoding rejects non-canonical boolean values for every bridge boolean field.

Bridge v3 uses concrete filter-carrying request tags `0x09` and `0x0A`, rejects old ambiguous filter tags, and decodes the explicit `format + min_id + max_id` filter schema. Bridge filters are typed, format-aware, validate ID ranges, and cannot serialize invalid local filter values.

SocketCAN and GS-USB TX paths do not have unchecked indexing or slicing based on untrusted frame length.

SocketCAN and GS-USB RX paths never expose remote/RTR, error, CAN FD, or backend control/status frames as `PiperFrame`.

SocketCAN split and unsplit RX paths read into `CANFD_MTU`-sized buffers, reject `MSG_TRUNC`, and share a pure raw parser that classifies data, recoverable non-data, and fatal frames before any flag-masking conversion. SocketCAN error frames follow the explicit bus-off, overflow, malformed, and recoverable mapping table.

GS-USB split, unsplit, and batch receive paths share the same validation behavior or have tests proving equivalent validation. Receive timeout/no-data is non-fatal control flow and does not discard previously queued valid frames.

GS-USB mixed-batch behavior is deterministic: recoverable non-data frames are skipped without failing the call, malformed data frames and fatal status/transport frames fail the whole device batch, and no valid frames from a fatal batch are returned later.

Repository searches find no `LegacyPiperRecording`, no direct `PiperFrame` field access, no `PiperFrame { ... }` struct literals, and no ambiguous replay conversion from `can_id + Vec<u8>`.

Required public re-export paths exist, and no stale raw-field `piper_protocol::can` compatibility module remains.

Required doctests and feature-matrix checks pass. No-default feature graph checks prove `piper-can/default` and `piper-can/auto-backend` are not enabled unless explicitly requested.

All required verification commands pass.
