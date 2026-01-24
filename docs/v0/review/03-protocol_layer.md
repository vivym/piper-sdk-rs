# Protocol Layer Review

## Overview
Review of the protocol layer (`src/protocol/`), responsible for type-safe CAN message encoding/decoding using `bilge`.

---

## Critical Issues

### 1. Default Values in `From<u8>` Implementations Lose Information (High Severity)
**Location**: `src/protocol/feedback.rs:49-125`

```rust
impl From<u8> for ControlMode {
    fn from(value: u8) -> Self {
        match value {
            0x00 => ControlMode::Standby,
            0x01 => ControlMode::CanControl,
            // ...
            _ => ControlMode::Standby, // 默认值，或使用 TryFrom 处理错误
        }
    }
}
```

**Issue**: Using `From<u8>` with a default fallback silently loses information. If the device sends an unknown control mode (e.g., future firmware version with new modes), it will be parsed as `Standby`.

This is **dangerous** because:
1. Unknown mode should be an error, not silently converted
2. Application might incorrectly believe robot is in standby mode
3. Safety-critical systems should fail-fast on unknown states

**Recommendation**: Use `TryFrom` instead:

```rust
impl TryFrom<u8> for ControlMode {
    type Error = ProtocolError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0x00 => Ok(ControlMode::Standby),
            0x01 => Ok(ControlMode::CanControl),
            // ...
            _ => Err(ProtocolError::InvalidEnumValue {
                field: "ControlMode",
                value
            }),
        }
    }
}
```

This affects:
- `ControlMode` (feedback.rs:49-63)
- `RobotStatus` (feedback.rs:103-125)
- `MoveMode` (feedback.rs:145-157)
- `TeachStatus` (feedback.rs:181-195)
- `MotionStatus` (feedback.rs:207-215)

---

### 2. Bilge Bit Field Endianness Ambiguity (Medium Severity)
**Location**: `src/protocol/feedback.rs:221-267`

```rust
/// 故障码位域（Byte 6: 角度超限位）
///
/// 协议定义（Motorola MSB 高位在前）：
/// - Bit 0: 1号关节角度超限位
/// ...
/// 注意：协议使用 Motorola (MSB) 高位在前，这是指**字节序**（多字节整数）。
/// 对于**单个字节内的位域**，协议明确 Bit 0 对应 1号关节，这是 LSB first（小端位序）。
/// bilge 默认使用 LSB first 位序，与协议要求一致。
#[bitsize(8)]
#[derive(FromBits, DebugBits, Clone, Copy, Default)]
pub struct FaultCodeAngleLimit {
    pub joint1_limit: bool, // Bit 0
    pub joint2_limit: bool, // Bit 1
    // ...
}
```

**Issue**: The comment tries to clarify endianness but is confusing. The key point is:
- **Multi-byte integers**: Big-endian (Motorola)
- **Bit fields within a byte**: LSB-first (little-endian bit order)

However, `bilge` crate's default behavior needs verification. The comment says "bilge 默认使用 LSB first 位序" but this should be **asserted at compile time**.

**Recommendation**: Add explicit tests to verify bit field ordering:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fault_code_bit_ordering() {
        // Create raw bytes with bit 0 set (0x01)
        let raw = 0x01u8;
        let parsed = FaultCodeAngleLimit::try_from_bytes([raw]).unwrap();
        assert!(parsed.joint1_limit());
        assert!(!parsed.joint2_limit());

        // Create raw bytes with bit 5 set (0x20)
        let raw = 0x20u8;
        let parsed = FaultCodeAngleLimit::try_from_bytes([raw]).unwrap();
        assert!(!parsed.joint1_limit());
        assert!(parsed.joint6_limit());
    }
}
```

---

## Design Issues

### 3. Inconsistent Error Handling Between Protocol Types (Low Severity)
**Location**: `src/protocol/feedback.rs:289-300`

```rust
impl TryFrom<PiperFrame> for RobotStatusFeedback {
    type Error = ProtocolError;

    fn try_from(frame: PiperFrame) -> Result<Self, Self::Error> {
        if frame.id != ID_ROBOT_STATUS {
            return Err(ProtocolError::InvalidCanId { id: frame.id });
        }

        if frame.len < 8 {
            return Err(ProtocolError::InvalidLength {
                expected: 8,
                actual: frame.len,
            });
        }
        // ...
    }
}
```

**Issue**: The error handling is good, but different protocol types have inconsistent validation:
- Some check `frame.len < 8`
- Some check `frame.len != 8`
- Some don't check length at all

**Recommendation**: Standardize validation logic through a helper function:

```rust
fn validate_frame(frame: &PiperFrame, expected_id: u32, expected_len: u8) -> Result<(), ProtocolError> {
    if frame.id != expected_id {
        return Err(ProtocolError::InvalidCanId { id: frame.id });
    }
    if frame.len < expected_len {
        return Err(ProtocolError::InvalidLength {
            expected: expected_len,
            actual: frame.len
        });
    }
    Ok(())
}
```

---

### 4. Physical Unit Conversion Scattered Across Code (Low Severity)
**Location**: `src/protocol/feedback.rs` and `src/driver/state.rs`

**Issue**: Unit conversions are done in multiple places:
1. Protocol layer: Converts raw bytes to engineering units
2. State layer: Stores values in f64
3. Application layer: Uses typed units (`Rad`, `Deg`, etc.)

This creates multiple conversion points and potential for precision loss.

**Example from comments in state.rs**:
```rust
/// - X, Y, Z: 位置（米）
///   - **注意**：`EndPoseFeedback1.x()`, `.y()`, `.z()` 返回的是**毫米**，需要除以 1000.0 转换为米
```

**Recommendation**: Create a dedicated units module with type-safe conversions:

```rust
pub mod units {
    #[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
    pub struct Millimeters(f64);

    impl Millimeters {
        pub fn to_meters(self) -> Meters {
            Meters(self.0 / 1000.0)
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
    pub struct Meters(f64);

    impl Meters {
        pub fn from_mm(mm: f64) -> Self {
            Self(mm / 1000.0)
        }
    }
}
```

---

## Potential Issues

### 5. Missing Bounds Checking in Protocol Parsing (Low Severity)
**Location**: `src/protocol/feedback.rs` (inferred)

**Issue**: When parsing protocol frames, array indexing like `frame.data[0]` is used. While `frame.len` is validated, the bilge-generated `try_from_bytes` might not do bounds checking.

**Recommendation**: Verify that bilge generates bounds-safe code, or add explicit checks:

```rust
// Verify bilge's behavior
#[test]
fn test_bilge_bounds_checking() {
    // Too short data
    let short_data = [0u8; 4];
    let result = FaultCodeAngleLimit::try_from_bytes(short_data);
    assert!(result.is_err());
}
```

---

### 6. No Protocol Version Compatibility Mechanism (Low Severity)
**Location**: `src/protocol/feedback.rs`

**Issue**: If the robot firmware is updated and adds new fields to feedback frames, the SDK will:
1. Silently ignore unknown bytes (safe)
2. Default unknown enum values to first variant (unsafe, see Issue #1)

There's no mechanism to:
- Detect protocol version mismatch
- Handle new fields gracefully
- Warn about unknown values

**Recommendation**: Add a version field to the protocol:

```rust
pub struct ProtocolVersion {
    pub major: u8,
    pub minor: u8,
}

impl RobotStatusFeedback {
    pub fn protocol_version() -> ProtocolVersion {
        ProtocolVersion { major: 1, minor: 0 }
    }
}
```

And handle unknown enum values explicitly:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlMode {
    Known(KnownControlMode),
    Unknown(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum KnownControlMode {
    #[default]
    Standby = 0x00,
    CanControl = 0x01,
    // ...
}
```

---

### 7. Hardcoded CAN IDs Throughout Protocol (Low Severity)
**Location**: `src/protocol/ids.rs`

**Issue**: CAN IDs are defined as constants, but there's no verification that they match the actual robot protocol. If the robot uses different IDs, there's no way to configure them at runtime.

**Recommendation**: Consider making CAN IDs configurable via build-time features or runtime config:

```rust
// Build-time feature
#[cfg(feature = "custom_can_ids")]
pub const ID_ROBOT_STATUS: u32 = 0x2A1;

#[cfg(not(feature = "custom_can_ids"))]
pub const ID_ROBOT_STATUS: u32 = 0x2A1; // Default
```

---

## Positive Observations

1. **Excellent use of bilge**: Bit-level protocol parsing is type-safe and efficient.
2. **Clear enum definitions**: `ControlMode`, `RobotStatus`, etc. are well-documented.
3. **Comprehensive protocol coverage**: All feedback frames are defined.
4. **Good separation of concerns**: Protocol layer is independent of CAN layer.
5. **Physical unit documentation**: Comments explain unit conversions clearly.

---

## Summary Table

| Issue | Severity | File | Lines |
|-------|----------|------|-------|
| From<u8> default fallback | High | feedback.rs | 49-215 |
| Bilge endianness ambiguity | Medium | feedback.rs | 221-267 |
| Inconsistent validation | Low | feedback.rs | 289-300 |
| Scattered unit conversions | Low | feedback.rs, state.rs | Throughout |
| Missing bounds checks | Low | feedback.rs | Inferred |
| No version compatibility | Low | feedback.rs | Throughout |
| Hardcoded CAN IDs | Low | ids.rs | Throughout |
