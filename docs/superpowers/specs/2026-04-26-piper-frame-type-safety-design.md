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
pub enum CanId;
pub struct StandardCanId;
pub struct ExtendedCanId;
pub struct CanData;
pub enum FrameError;
```

ID construction:

```rust
StandardCanId::new(raw: u16) -> Result<Self, FrameError>
ExtendedCanId::new(raw: u32) -> Result<Self, FrameError>
StandardCanId::raw(&self) -> u16
ExtendedCanId::raw(&self) -> u32
CanId::standard(raw: u16) -> Result<Self, FrameError>
CanId::extended(raw: u32) -> Result<Self, FrameError>
CanId::raw(&self) -> u32
CanId::is_standard(&self) -> bool
CanId::is_extended(&self) -> bool
```

Payload construction:

```rust
CanData::new(data: impl AsRef<[u8]>) -> Result<Self, FrameError>
CanData::from_array(bytes: [u8; 8]) -> Self
CanData::from_padded(bytes: [u8; 8], len: u8) -> Result<Self, FrameError>
CanData::as_slice(&self) -> &[u8]
CanData::as_padded(&self) -> &[u8; 8]
CanData::len(&self) -> u8
CanData::is_empty(&self) -> bool
```

`CanData::from_array([u8; 8])` represents an active 8-byte payload. It must not be used to mean "padded storage with unknown or inferred length." Backend decode paths that receive fixed storage plus DLC should use `CanData::from_padded(bytes, len)`.

Frame construction:

```rust
PiperFrame::standard(id: StandardCanId, data: CanData) -> Self
PiperFrame::extended(id: ExtendedCanId, data: CanData) -> Self
PiperFrame::new_standard(id: u16, data: impl AsRef<[u8]>) -> Result<Self, FrameError>
PiperFrame::new_extended(id: u32, data: impl AsRef<[u8]>) -> Result<Self, FrameError>
```

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

Protocol-layer command builders that use compile-time CAN ID constants should remain infallible where the IDs and payload sizes are statically known to be valid. Implementations should avoid spreading `unwrap()` or `expect()` through every `to_frame()` method. Acceptable patterns include pre-validated ID constants, private infallible helpers for protocol-owned constants, or a clearly documented conversion layer that proves the constants are valid once. Public constructors for user-provided IDs and payloads must remain fallible.

## Error Model

Add `FrameError` to `piper-protocol`.

Required variants:

- invalid standard ID
- invalid extended ID
- payload too long
- invalid serialized frame format

`FrameError` should be independent from `ProtocolError`. `ProtocolError` is for robot protocol parsing failures after a valid CAN frame exists. `FrameError` is for malformed CAN frame construction or deserialization.

Existing higher-level errors can wrap `FrameError` where needed. For example, replay conversion can map `FrameError` to `RobotError::ConfigError` with context about the recording frame.

## Serde Format

When the `serde` feature is enabled, `PiperFrame` should use a strict explicit shape:

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

The serialized `data` field should contain only the active payload bytes, not the padded 8-byte storage.

The serde implementation should not accept the old field shape. Historical recording compatibility is intentionally out of scope.

The same strict serde model applies to every serializer used by the workspace. JSON examples document the logical shape, but recording files use `bincode`; implementation must verify that bincode serialization and deserialization enforce the same invariants.

The constrained subtypes also need safe serde behavior. `StandardCanId`, `ExtendedCanId`, `CanId`, and `CanData` must not derive unchecked `Deserialize` implementations that can bypass their constructors. Either they do not expose serde implementations directly, or their deserializers must enforce the same invariants as their constructors.

## Recording Format

Both recording boundary types must migrate:

- `piper-tools::TimestampedFrame`
- `piper_driver::recording::TimestampedFrame`

Neither type should store an ambiguous `can_id + Vec<u8>` model that infers extended frames from `can_id > 0x7FF`.

Preferred shape:

```rust
pub struct TimestampedFrame {
    pub frame: PiperFrame,
    pub source: TimestampSource,
}
```

This makes recording files use the same explicit serde format as live frames. The recorded timestamp is `frame.timestamp_us()`. There must not be a second top-level `timestamp_us` field when the type already stores a `PiperFrame`.

If `piper-tools::TimestampedFrame` stores `PiperFrame` directly, `piper-tools` must enable the `piper-protocol/serde` feature for its recording serialization dependency path. Recording serialization should not rely on a local duplicate frame representation just to avoid enabling serde on `piper-protocol`.

For `piper_driver::recording::TimestampedFrame`, the preferred shape is:

```rust
pub struct TimestampedFrame {
    pub frame: PiperFrame,
}
```

If the driver recording hook needs convenience accessors, they should delegate to the inner frame:

```rust
TimestampedFrame::timestamp_us(&self) -> u64
TimestampedFrame::raw_id(&self) -> u32
TimestampedFrame::data(&self) -> &[u8]
```

Both recording types must store `PiperFrame` directly. They must not keep a local duplicate frame representation with `format`, `id`, and `data` fields, because that would create a second frame validation model outside the canonical `PiperFrame` type.

Historical recording compatibility is intentionally removed. Existing legacy recording conversion paths, including `LegacyPiperRecording`-style readers that deserialize old `can_id + Vec<u8>` frames, should be deleted. The recording file format version should be bumped, and tests should prove old-shape serialized data is rejected rather than silently converted.

## Bridge Protocol Format

The bridge protocol must encode explicit frame format.

The binary bridge protocol already carries `id`, `is_extended`, `len`, and 8 data bytes. The decoder should construct a `PiperFrame` through the checked constructors instead of a struct literal.

The bridge decoder must treat `is_extended` as a canonical boolean field. Accept only `0` and `1`; reject any other value as malformed protocol data.

Bridge send-frame requests do not encode or trust a timestamp from the client. A request decoded into `PiperFrame` should have `timestamp_us() == 0`. Receive-frame events carry the host/device timestamp and should apply it through `with_timestamp_us()` when constructing the event frame.

The encoder should read from frame methods:

- `raw_id()`
- `is_extended()`
- `dlc()`
- `data_padded()`
- `timestamp_us()`

Malformed bridge payloads should fail decode at the protocol boundary, not produce invalid `PiperFrame` values.

## CAN Backend Impact

SocketCAN and GS-USB TX paths should stop defensively slicing with unchecked `frame.len`.

Backend encoding should use:

- `frame.raw_id()`
- `frame.is_extended()`
- `frame.dlc()`
- `frame.data()`
- `frame.data_padded()` where the backend protocol requires fixed storage

RX conversion should validate backend-provided IDs and DLC values through `PiperFrame` constructors. Invalid frames received from a device or raw socket should become backend errors instead of invalid SDK values.

RX conversion must also reject non-data frames before constructing `PiperFrame`. The invariant is that remote/RTR frames, error frames, CAN FD frames, and backend control/status frames are never exposed as `PiperFrame`. The backend may preserve its current strategy for those frames: return an error for fatal conditions, ignore/log recoverable conditions, or map them to backend-specific events. This refactor should not force every non-data frame to become a hard receive error.

## Protocol Layer Impact

Protocol parsing should replace direct field access with methods.

Examples:

- `frame.id` becomes `frame.raw_id()`
- `frame.len` becomes `frame.dlc()`
- `frame.data[0]` becomes `frame.data()[0]` after length checks
- `frame.data[..copy_len]` becomes `frame.data()[..copy_len]`

Tests that currently use invalid standard CAN IDs such as `0x999` for "unknown robot protocol ID" should use a legal CAN ID outside the robot protocol set, such as `0x700`, or explicitly construct an extended frame when the test needs an extended ID.

## Driver And Client Impact

Driver and client code should treat `PiperFrame` as immutable.

Timestamp updates should use:

```rust
frame = frame.with_timestamp_us(timestamp_us);
```

Tests should use small helper builders instead of struct literals. This keeps test setup clear and prevents test-only invalid frames from re-entering the codebase.

## Public Re-Exports

`piper-can` and `piper-sdk` should re-export the new frame types alongside `PiperFrame`:

```rust
pub use piper_protocol::{
    CanData, CanId, ExtendedCanId, FrameError, PiperFrame, StandardCanId,
};
```

This keeps downstream users from needing to depend on `piper-protocol` only to construct frames.

## Migration Strategy

The implementation should be done as a single focused refactor rather than compatibility layers.

Recommended order:

1. Add RED tests in `piper-protocol` for invalid IDs, oversized payloads, timestamp preservation, and serde rejection.
2. Implement the new frame value types and private `PiperFrame` fields.
3. Migrate `piper-protocol` internals and tests.
4. Migrate `piper-can` backend encode/decode and bridge protocol.
5. Migrate `piper-tools` recording format.
6. Migrate `piper-driver`, `piper-client`, `piper-sdk`, examples, and addon tests.
7. Remove all remaining direct field access and struct literals.

The refactor should not keep deprecated constructors or public fields. Compile errors should guide migration.

## Verification Plan

Required checks:

```bash
cargo fmt --all -- --check
cargo check --all-targets
cargo test -p piper-protocol --features serde
cargo test -p piper-tools
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo doc --workspace --no-deps --document-private-items
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

- `piper-protocol` serde tests for valid/invalid `PiperFrame` JSON shapes.
- `piper-protocol` tests proving constrained subtypes cannot be deserialized into invalid values when serde is enabled.
- `piper-protocol` tests for `CanData::from_padded`: reject `len > 8`, preserve active bytes, and zero nonzero bytes beyond DLC.
- `piper-tools` bincode roundtrip tests for recordings containing standard and extended frames.
- `piper-tools` rejection tests for old recording frame shapes that lack explicit frame format.
- `piper-can` bridge protocol tests for invalid frame format booleans, invalid IDs, invalid DLC, request timestamp zeroing, and event timestamp preservation.
- Backend decode tests for RTR/error frame rejection where such frames can be constructed without hardware.

## Acceptance Criteria

No public API can construct a `PiperFrame` with invalid standard ID, invalid extended ID, or DLC above 8.

No `PiperFrame` constructor silently truncates payload data.

No code in the repository uses `PiperFrame { ... }` struct literals.

No code in the repository directly reads or writes `PiperFrame` fields.

Serde deserialization rejects malformed frames.

Recording and bridge boundaries preserve explicit standard versus extended frame format.

Recording code has no remaining legacy conversion path from ambiguous `can_id + Vec<u8>` frames.

SocketCAN and GS-USB TX paths do not have unchecked indexing or slicing based on untrusted frame length.

All required verification commands pass.
