# Protocol Layer Fixes - Solution Plan

## Executive Summary

This document provides detailed solutions for **critical protocol parsing issues** that could lead to silent data corruption, security vulnerabilities, or system failures.

**Priority**: CRITICAL - Must fix before production use

**Estimated Effort**: 2-3 days

---

## Issue Summary

| Issue | Severity | Impact | Location |
|-------|----------|--------|----------|
| `From<u8>` with default fallback | CRITICAL | Unknown states silently converted | `src/protocol/feedback.rs` (5 enums) |
| Array index without bounds validation | HIGH | Potential panic/corruption | `src/driver/pipeline.rs` (6 locations) |
| No NaN/Inf validation on float data | MEDIUM | Invalid sensor data propagates | `src/driver/pipeline.rs` (all parsing) |
| **⚠️ CRITICAL: `?` operator in `rx_loop`** | **CRITICAL** | **Single bad frame kills driver thread** | **`src/driver/pipeline.rs` (all parsing)** |
| Frame length validation inconsistent | LOW | May accept malformed frames | `src/protocol/feedback.rs` |

---

## ⚠️ CRITICAL RISK: Error Propagation in `rx_loop`

### Problem

**DO NOT** use the `?` operator in `rx_loop` for error propagation. Using `?` will cause the **entire RX thread to terminate** on a single bad frame, leaving the robot without feedback updates.

**Wrong Example** (DO NOT DO THIS):
```rust
// ❌ WRONG: Using ? in rx_loop
let control_mode = ControlMode::try_from(feedback.control_mode as u8)?; // Thread terminates!
pending_joint_pos[0] = validate_joint_rad(feedback.j1_rad())?; // Thread terminates!
```

**Impact**: If a single CAN frame contains invalid data, the driver thread crashes, resulting in:
- Loss of all robot feedback
- No position/velocity updates
- Robot becomes uncontrollable
- Requires application restart to recover

**Correct Approach** (See Fix 3 for implementation):
```rust
// ✅ CORRECT: Explicit match, continue on error
match ControlMode::try_from(feedback.control_mode as u8) {
    Ok(mode) => state.control_mode = mode,
    Err(e) => {
        warn!("Invalid control mode in frame, skipping: {}", e);
        continue; // Skip THIS frame only, keep processing
    }
}
```

---

## Optimization: Reduce Boilerplate with `num_enum`

### Recommendation

Instead of manually implementing `TryFrom<u8>` for 5 enums (100+ lines of repetitive code), use the `num_enum` crate to auto-generate it.

**Add to `Cargo.toml`**:
```toml
[dependencies]
num_enum = "0.7"
```

**Usage**:
```rust
use num_enum::TryFromPrimitive;

#[derive(Debug, Clone, Copy, PartialEq, Eq, TryFromPrimitive)]
#[repr(u8)]
pub enum ControlMode {
    Standby = 0x00,
    CanControl = 0x01,
    Teach = 0x02,
    Ethernet = 0x03,
    Wifi = 0x04,
    Remote = 0x05,
    LinkTeach = 0x06,
    OfflineTrajectory = 0x07,
}

// TryFrom<u8> is auto-generated!
// No manual match statement needed
```

**Benefits**:
- Eliminates ~100 lines of boilerplate code
- Reduce human error risk (typos in hex values)
- Single source of truth for enum values

**See "Fix 1b: Using num_enum" below for implementation details.**

---

## Fix 1: Replace `From<u8>` with `TryFrom<u8>` for Enums

### Problem

All protocol enums use `From<u8>` with a default fallback, which silently loses information:

**Current Code** (`src/protocol/feedback.rs:49-63`):
```rust
impl From<u8> for ControlMode {
    fn from(value: u8) -> Self {
        match value {
            0x00 => ControlMode::Standby,
            0x01 => ControlMode::CanControl,
            0x02 => ControlMode::Teach,
            0x03 => ControlMode::Ethernet,
            0x04 => ControlMode::Wifi,
            0x05 => ControlMode::Remote,
            0x06 => ControlMode::LinkTeach,
            0x07 => ControlMode::OfflineTrajectory,
            _ => ControlMode::Standby, // ❌ Loses information!
        }
    }
}
```

**Risk**:
- Future firmware with new modes → silently parsed as `Standby`
- Application incorrectly believes robot is in standby mode
- Safety-critical systems should fail-fast on unknown states

### Solution

Replace `From<u8>` with `TryFrom<u8>` that returns an error for unknown values:

**Step 1: Add error variant to `ProtocolError`**

In `src/protocol/mod.rs`:
```rust
#[derive(Debug, thiserror::Error)]
pub enum ProtocolError {
    // ... existing variants ...

    #[error("Invalid enum value for {enum_name}: {value}")]
    InvalidEnumValue {
        enum_name: &'static str,
        value: u8,
    },
}
```

**Step 2: Replace `From<u8>` with `TryFrom<u8>`**

**Fixed Code**:
```rust
// Remove the old impl
// impl From<u8> for ControlMode { ... }

// Add TryFrom implementation
impl TryFrom<u8> for ControlMode {
    type Error = ProtocolError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0x00 => Ok(ControlMode::Standby),
            0x01 => Ok(ControlMode::CanControl),
            0x02 => Ok(ControlMode::Teach),
            0x03 => Ok(ControlMode::Ethernet),
            0x04 => Ok(ControlMode::Wifi),
            0x05 => Ok(ControlMode::Remote),
            0x06 => Ok(ControlMode::LinkTeach),
            0x07 => Ok(ControlMode::OfflineTrajectory),
            _ => Err(ProtocolError::InvalidEnumValue {
                enum_name: "ControlMode",
                value,
            }),
        }
    }
}
```

**Step 3: Update all call sites**

**⚠️ CRITICAL: In `rx_loop`, use `match` NOT `?`**

```rust
// ✅ CORRECT for rx_loop (keep thread alive):
match ControlMode::try_from(feedback.control_mode as u8) {
    Ok(mode) => state.control_mode = mode,
    Err(e) => {
        warn!("Invalid control mode in frame 0x{:X}: {}, skipping",
              frame.id, e);
        continue; // Skip THIS frame, keep processing
    }
}

// ✅ CORRECT for one-time operations (can use ?):
let control_mode = ControlMode::try_from(value)?;
```

---

## Fix 1b: Using `num_enum` (Recommended Optimization)

### Implementation

**Step 1: Add dependency**

In `Cargo.toml`:
```toml
[dependencies]
num_enum = "0.7"
```

**Step 2: Update enum definitions**

In `src/protocol/feedback.rs`:

```rust
use num_enum::TryFromPrimitive;

#[derive(Debug, Clone, Copy, PartialEq, Eq, TryFromPrimitive)]
#[repr(u8)]
pub enum ControlMode {
    Standby = 0x00,
    CanControl = 0x01,
    Teach = 0x02,
    Ethernet = 0x03,
    Wifi = 0x04,
    Remote = 0x05,
    LinkTeach = 0x06,
    OfflineTrajectory = 0x07,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, TryFromPrimitive)]
#[repr(u8)]
pub enum RobotStatus {
    Normal = 0x00,
    EmergencyStop = 0x01,
    NoSolution = 0x02,
    Singularity = 0x03,
    AngleLimitExceeded = 0x04,
    JointCommError = 0x05,
    JointBrakeNotOpen = 0x06,
    Collision = 0x07,
    TeachOverspeed = 0x08,
    JointStatusError = 0x09,
    OtherError = 0x0A,
    TeachRecord = 0x0B,
    TeachExecute = 0x0C,
    TeachPause = 0x0D,
    MainControlOverTemp = 0x0E,
    ResistorOverTemp = 0x0F,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, TryFromPrimitive)]
#[repr(u8)]
pub enum MoveMode {
    MoveP = 0x00,
    MoveJ = 0x01,
    MoveL = 0x02,
    MoveC = 0x03,
    MoveM = 0x04,
    MoveCpv = 0x05,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, TryFromPrimitive)]
#[repr(u8)]
pub enum TeachStatus {
    Closed = 0x00,
    StartRecord = 0x01,
    EndRecord = 0x02,
    Execute = 0x03,
    Pause = 0x04,
    Continue = 0x05,
    Terminate = 0x06,
    MoveToStart = 0x07,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, TryFromPrimitive)]
#[repr(u8)]
pub enum MotionStatus {
    Arrived = 0x00,
    NotArrived = 0x01,
}
```

**Step 3: Add custom error type (if needed)**

```rust
// In src/protocol/mod.rs
#[derive(Debug, thiserror::Error)]
pub enum ProtocolError {
    #[error("Invalid enum value for {enum_name}: {value}")]
    InvalidEnumValue {
        enum_name: &'static str,
        value: u8,
    },

    // ... existing variants ...
}
```

**Step 4: Update call sites in `rx_loop`**

```rust
// In rx_loop, with num_enum:
ID_ROBOT_STATUS => {
    if let Ok(feedback) = RobotStatusFeedback::try_from(frame) {
        // Parse enums with num_enum's TryFromPrimitive
        match RobotStatus::try_from_primitive(feedback.robot_status) {
            Ok(status) => state.robot_status = status,
            Err(_) => {
                // Note: TryFromPrimitiveError doesn't provide the bad value
                // So we log the raw value for debugging instead
                warn!(
                    "Invalid robot_status {} in frame 0x{:X}, skipping",
                    feedback.robot_status, frame.id
                );
                continue;
            }
        }
        // ... rest of parsing
    } else {
        warn!("Failed to parse RobotStatusFeedback: {:?}", frame);
        continue;
    }
}
```

**Note on Error Types**: When using `num_enum`, `TryFromPrimitiveError` is a different type than `ProtocolError`. Since we don't print the error `e` and instead log the raw `feedback.robot_status` value, this is fine. The raw value provides more debugging information than the generic error message.

**Benefits**:
- ✅ Eliminates ~100 lines of repetitive `match` code
- ✅ Compile-time guaranteed correctness
- ✅ Single source of truth for enum ↔ value mapping
- ✅ Easier to add new enum variants in future

---

## Optimization: Define Physical Constants

### Problem

Magic numbers scattered throughout the code:

```rust
if joint_idx < 6 { ... }           // What is 6?
if value.abs() > 10.0 { ... }      // What is 10.0?
if value.abs() > 5.0 { ... }       // What is 5.0?
```

### Solution

Create `src/config/constants.rs`:

```rust
/// Physical constants for Piper robot arm
pub mod constants {
    /// Number of joints in Piper arm
    pub const PIPER_JOINT_COUNT: usize = 6;

    /// Maximum joint angle (radians)
    /// Typical robot arms have ±π range, this provides safety margin
    pub const MAX_JOINT_RAD: f64 = 10.0;

    /// Maximum end effector position from origin (meters)
    /// Desktop arms typically reach < 1m, 5m provides large safety margin
    pub const MAX_END_POSE_M: f64 = 5.0;

    /// Minimum reasonable joint angle (radians)
    pub const MIN_JOINT_RAD: f64 = -10.0;

    /// End effector position conversion: millimeters to meters
    pub const MM_TO_M: f64 = 1.0 / 1000.0;

    /// Millimeter precision for end effector
    pub const END_POSE_MM_PRECISION: f64 = 0.001;
}
```

**IMPORTANT**: Ensure constants are accessible in driver:

In `src/lib.rs`, verify:
```rust
pub mod config; // Must be present for driver to access constants

// Or re-export constants for convenience
pub use config::constants::*;
```

Then in `src/driver/pipeline.rs`:
```rust
use crate::config::constants::*;

// Now constants are accessible
if joint_index == 0 || joint_index > PIPER_JOINT_COUNT {
    // ...
}
```

### Updated Usage

```rust
use crate::config::constants::*;

// Array index validation
if joint_index == 0 || joint_index > PIPER_JOINT_COUNT {
    warn!("Invalid joint_index: {} (expected 1-{})",
          joint_index, PIPER_JOINT_COUNT);
    continue;
}

// Float validation
pub fn validate_joint_rad(value: f64) -> Result<f64, ProtocolError> {
    if !value.is_finite() {
        return Err(ProtocolError::InvalidValue {
            field: "joint_position",
            value,
            reason: "NaN or Inf",
        });
    }

    if value < MIN_JOINT_RAD || value > MAX_JOINT_RAD {
        warn!("Suspicious joint angle: {} rad (range: ±{})",
              value, MAX_JOINT_RAD);
    }

    Ok(value)
}

pub fn validate_end_pose_meters(value: f64) -> Result<f64, ProtocolError> {
    if !value.is_finite() {
        return Err(ProtocolError::InvalidValue {
            field: "end_pose",
            value,
            reason: "NaN or Inf",
        });
    }

    if value.abs() > MAX_END_POSE_M {
        return Err(ProtocolError::InvalidValue {
            field: "end_pose",
            value,
            reason: "Out of physical range",
        });
    }

    Ok(value)
}
```

### Affected Enums

All these enums need the same fix:

| Enum | Location | Variants |
|------|----------|----------|
| `ControlMode` | feedback.rs:49-63 | 8 variants (0x00-0x07) |
| `RobotStatus` | feedback.rs:103-125 | 16 variants (0x00-0x0F) |
| `MoveMode` | feedback.rs:145-157 | 6 variants (0x00-0x05) |
| `TeachStatus` | feedback.rs:181-195 | 8 variants (0x00-0x07) |
| `MotionStatus` | feedback.rs:207-215 | 2 variants (0x00-0x01) |

### Migration Strategy

**Phase 1: Add TryFrom, keep From for compatibility**
```rust
impl From<u8> for ControlMode {
    fn from(value: u8) -> Self {
        Self::try_from(value).unwrap_or(Self::Standby)
    }
}

impl TryFrom<u8> for ControlMode {
    type Error = ProtocolError;
    // ... (as above)
}
```

**Phase 2: Update all call sites to use TryFrom**
```rust
// Gradually update call sites:
let control_mode = ControlMode::try_from(value)?;
```

**Phase 3: Deprecate and remove From**
```rust
#[deprecated(note = "Use TryFrom<u8> instead for proper error handling")]
impl From<u8> for ControlMode { ... }
```

### Testing

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_control_mode_try_from_valid() {
        assert_eq!(ControlMode::try_from(0x00), Ok(ControlMode::Standby));
        assert_eq!(ControlMode::try_from(0x01), Ok(ControlMode::CanControl));
        // ... test all valid variants
    }

    #[test]
    fn test_control_mode_try_from_invalid() {
        assert!(ControlMode::try_from(0xFF).is_err());
        assert!(ControlMode::try_from(0x08).is_err()); // Future firmware value
        assert!(matches!(
            ControlMode::try_from(0x08),
            Err(ProtocolError::InvalidEnumValue { enum_name: "ControlMode", value: 0x08 })
        ));
    }

    #[test]
    fn test_robot_status_all_variants() {
        // Test all 16 valid variants
        for i in 0..=0x0F {
            assert!(RobotStatus::try_from(i).is_ok());
        }
        // Test invalid
        assert!(RobotStatus::try_from(0x10).is_err());
    }
}
```

---

## Optimization: Log Rate Limiting for Validation Errors

### Problem

In high-frequency control loops (500Hz), if a sensor fault persists sending NaN/Inf values, logging every error can cause:

- **Log flooding**: Thousands of log messages per second
- **Disk exhaustion**: Log files grow rapidly
- **CPU spikes**: Logging I/O becomes bottleneck
- **Masked issues**: Important error messages hidden in noise

### Solution

Implement rate limiting for validation errors:

**Option A: Simple counter-based limiting**

```rust
use std::sync::atomic::{AtomicUsize, Ordering};

static INVALID_JOINT_VAL_COUNT: AtomicUsize = AtomicUsize::new(0);
static INVALID_POSE_VAL_COUNT: AtomicUsize = AtomicUsize::new(0);

pub fn validate_joint_rad(value: f64) -> Result<f64, ProtocolError> {
    if !value.is_finite() {
        let count = INVALID_JOINT_VAL_COUNT.fetch_add(1, Ordering::Relaxed);

        // Log every 1000th error (≈2 seconds at 500Hz)
        if count % 1000 == 0 {
            warn!(
                "Invalid joint value (NaN/Inf) - suppressed {} times total",
                count
            );
        }

        return Err(ProtocolError::InvalidValue {
            field: "joint_position",
            value,
            reason: "NaN or Inf",
        });
    }

    if value < MIN_JOINT_RAD || value > MAX_JOINT_RAD {
        let count = INVALID_JOINT_VAL_COUNT.fetch_add(1, Ordering::Relaxed);

        if count % 1000 == 0 {
            warn!(
                "Suspicious joint angle: {} rad - suppressed {} times",
                value, count
            );
        }

        // Still warn, but rate-limited
    }

    Ok(value)
}
```

**Option B: Using `log_once` crate (recommended)**

```toml
[dependencies]
log_once = "0.4"
```

```rust
use log_once::warn_once;

pub fn validate_joint_rad(value: f64) -> Result<f64, ProtocolError> {
    if !value.is_finite() {
        // Each unique error location logged only once per process lifetime
        warn_once!("Invalid joint value (NaN/Inf) detected");

        return Err(ProtocolError::InvalidValue {
            field: "joint_position",
            value,
            reason: "NaN or Inf",
        });
    }

    // ... rest of validation
}
```

**Option C: Time-based rate limiting**

```rust
use std::time::{Duration, Instant};

struct RateLimiter {
    last_log: Instant,
    min_interval: Duration,
}

impl RateLimiter {
    fn new(min_interval: Duration) -> Self {
        Self {
            last_log: Instant::now() - min_interval, // Allow first log immediately
            min_interval,
        }
    }

    fn should_log(&mut self) -> bool {
        let now = Instant::now();
        if now.duration_since(self.last_log) >= self.min_interval {
            self.last_log = now;
            true
        } else {
            false
        }
    }
}

// Usage (make it thread-local for rx_loop)
thread_local! {
    static JOINT_VAL_LIMITER: RateLimiter = RateLimiter::new(Duration::from_secs(1));
}

pub fn validate_joint_rad(value: f64) -> Result<f64, ProtocolError> {
    if !value.is_finite() {
        if JOINT_VAL_LIMITER.with(|lim| lim.should_log()) {
            warn!("Invalid joint value (NaN/Inf) - logging rate-limited");
        }
        return Err(...);
    }
    // ...
}
```

**Recommendation**: Use **Option A (simple counter)** for most cases. Use `log_once` if you prefer the crate approach.

---

## Fix 2: Add Array Index Bounds Validation

### Problem

Joint index from CAN frames is used for array indexing without proper validation:

**Current Code** (`pipeline.rs:469`):
```rust
let joint_idx = (feedback.joint_index as usize).saturating_sub(1);
if joint_idx < 6 {
    // Update state[joint_idx]
}
```

**Issues**:
1. If `joint_index = 0`, saturating_sub gives `0`, but joint 0 is invalid (joints are 1-6)
2. If `joint_index = 255`, saturating_sub gives `254`, which fails `< 6` check
3. No explicit error logging for invalid indices

### Solution

Add explicit validation before array access:

**Fixed Code**:
```rust
// Explicit validation
let joint_index = feedback.joint_index as usize;
if joint_index == 0 || joint_index > 6 {
    warn!(
        "Invalid joint_index in feedback: {} (expected 1-6), skipping frame",
        joint_index
    );
    continue; // Skip this frame
}
let joint_idx = joint_index - 1; // Now safe to subtract

// Update state
if joint_idx < 6 {
    // ... update state[joint_idx]
}
```

### Affected Locations

| Line | Context | Validation Needed |
|------|---------|-------------------|
| 469 | `JointDriverLowSpeedFeedback` | Joint index 1-6 |
| 594 | `MotorLimitFeedback` | Joint index 1-6 |
| 641 | `MotorMaxAccelFeedback` | Joint index 1-6 |
| 1595 | `JointDriverLowSpeedFeedback` (rx_loop) | Joint index 1-6 |
| 1700 | `MotorLimitFeedback` (rx_loop) | Joint index 1-6 |
| 1736 | `MotorMaxAccelFeedback` (rx_loop) | Joint index 1-6 |

### Testing

```rust
#[test]
fn test_joint_index_validation() {
    // Valid indices
    assert!(validate_joint_index(1).is_ok()); // Returns Ok(0)
    assert!(validate_joint_index(6).is_ok()); // Returns Ok(5)

    // Invalid indices
    assert!(validate_joint_index(0).is_err());
    assert!(validate_joint_index(7).is_err());
    assert!(validate_joint_index(255).is_err());
}

fn validate_joint_index(idx: u8) -> Result<usize, ()> {
    let joint_index = idx as usize;
    if joint_index == 0 || joint_index > 6 {
        Err(())
    } else {
        Ok(joint_index - 1)
    }
}
```

---

## Fix 3: Add NaN/Inf Validation on Float Data

### Problem

CAN frame data is converted to floats without validating for NaN/Inf values:

**Current Code** (`pipeline.rs:189-195`):
```rust
// Direct assignment without validation
pending_joint_pos[0] = feedback.j1_rad();
pending_joint_pos[1] = feedback.j2_rad();
pending_end_pose[0] = feedback.x() / 1000.0;
```

**Risk**:
- CAN frame corruption produces NaN/Inf
- Invalid data propagates to control system
- Trajectory planning produces NaN

### Solution

Add validation helper function for float values:

**Step 1: Add validation helpers**

Create `src/driver/validation.rs`:
```rust
use crate::protocol::ProtocolError;

/// Validates that a float value is finite and within plausible range
pub fn validate_joint_rad(value: f64) -> Result<f64, ProtocolError> {
    if !value.is_finite() {
        return Err(ProtocolError::InvalidValue {
            field: "joint_position",
            value,
            reason: "NaN or Inf",
        });
    }

    // Joint angles should be within ±2π (typical robot arm range)
    if value.abs() > 10.0 {
        warn!("Suspicious joint angle: {} rad (possible corruption)", value);
        // Don't error, just warn - some robots may have larger ranges
    }

    Ok(value)
}

pub fn validate_end_pose_meters(value: f64) -> Result<f64, ProtocolError> {
    if !value.is_finite() {
        return Err(ProtocolError::InvalidValue {
            field: "end_pose",
            value,
            reason: "NaN or Inf",
        });
    }

    // End effector should be within ±5m from origin (reasonable for desktop arm)
    if value.abs() > 5.0 {
        warn!("Implausible end effector position: {}m", value);
        return Err(ProtocolError::InvalidValue {
            field: "end_pose",
            value,
            reason: "Out of physical range",
        });
    }

    Ok(value)
}
```

**Step 2: Use validation in pipeline**

**Fixed Code**:
```rust
use crate::driver::validation::{validate_joint_rad, validate_end_pose_meters};

ID_JOINT_FEEDBACK_12 => {
    if let Ok(feedback) = JointFeedback12::try_from(frame) {
        // Validate before storing
        pending_joint_pos[0] = validate_joint_rad(feedback.j1_rad())?;
        pending_joint_pos[1] = validate_joint_rad(feedback.j2_rad())?;
        // ...
    }
}

ID_END_POSE_1 => {
    if let Ok(feedback) = EndPoseFeedback1::try_from(frame) {
        // Validate before storing
        pending_end_pose[0] = validate_end_pose_meters(feedback.x() / 1000.0)?;
        pending_end_pose[1] = validate_end_pose_meters(feedback.y() / 1000.0)?;
        pending_end_pose[2] = validate_end_pose_meters(feedback.z() / 1000.0)?;
        // ...
    }
}
```

**Step 3: Add error variant to `ProtocolError`**

In `src/protocol/mod.rs`:
```rust
#[derive(Debug, thiserror::Error)]
pub enum ProtocolError {
    // ... existing variants ...

    #[error("Invalid value for {field}: {value} ({reason})")]
    InvalidValue {
        field: &'static str,
        value: f64,
        reason: &'static str,
    },
}
```

### Testing

```rust
#[test]
fn test_validate_joint_rad() {
    use crate::driver::validation::validate_joint_rad;

    // Valid values
    assert!(validate_joint_rad(0.0).is_ok());
    assert!(validate_joint_rad(1.57).is_ok());
    assert!(validate_joint_rad(-1.57).is_ok());

    // Invalid values
    assert!(validate_joint_rad(f64::NAN).is_err());
    assert!(validate_joint_rad(f64::INFINITY).is_err());
    assert!(validate_joint_rad(f64::NEG_INFINITY).is_err());
}

#[test]
fn test_validate_end_pose_meters() {
    use crate::driver::validation::validate_end_pose_meters;

    // Valid values
    assert!(validate_end_pose_meters(0.3).is_ok());
    assert!(validate_end_pose_meters(-0.2).is_ok());

    // Invalid values
    assert!(validate_end_pose_meters(f64::NAN).is_err());
    assert!(validate_end_pose_meters(10.0).is_err()); // Out of range
}
```

---

## Fix 4: Improve Frame Length Validation

### Problem

Protocol parsing uses inconsistent length validation:
- Some check `frame.len < 8`
- Some check `frame.len != 8`
- Some don't check length at all

### Solution

Standardize validation through a helper function:

**Add to `src/protocol/mod.rs`**:
```rust
use crate::can::PiperFrame;

/// Validates CAN frame ID and length
pub fn validate_frame(
    frame: &PiperFrame,
    expected_id: u32,
    expected_len: u8,
) -> Result<(), ProtocolError> {
    if frame.id != expected_id {
        return Err(ProtocolError::InvalidCanId { id: frame.id });
    }

    // Strict: must be exactly expected length
    if frame.len != expected_len {
        return Err(ProtocolError::InvalidLength {
            expected: expected_len,
            actual: frame.len,
        });
    }

    Ok(())
}
```

**Usage in `TryFrom` implementations**:
```rust
impl TryFrom<PiperFrame> for RobotStatusFeedback {
    type Error = ProtocolError;

    fn try_from(frame: PiperFrame) -> Result<Self, Self::Error> {
        // Use helper
        validate_frame(&frame, ID_ROBOT_STATUS, 8)?;

        // Parse bilge structures...
        let control_mode = ControlMode::try_from(frame.data[0])?;
        // ...

        Ok(Self { /* ... */ })
    }
}
```

---

## Implementation Checklist (REVISED with Optimizations)

### Critical Fixes

- [ ] **CRITICAL: Fix error propagation in `rx_loop`**
  - [ ] Ensure all `TryFrom` calls use `match` NOT `?`
  - [ ] Add `continue` on all error paths in rx_loop
  - [ ] Add `continue` on all validation errors in rx_loop
  - [ ] **DO NOT use `?` operator in rx_loop**
  - [ ] Add test: "Loop Resilience" - verify thread doesn't crash on bad frames

### Protocol Layer Fixes

- [ ] **Fix 1a: Manual TryFrom** OR **Fix 1b: Use `num_enum`**
  - [ ] Choose approach: Manual (step 1) OR num_enum (step 1b, **recommended**)
  - [ ] Add `InvalidEnumValue` error variant
  - [ ] Update all 5 enums with `TryFrom<u8>` or `TryFromPrimitive`
  - [ ] Update all call sites in `rx_loop` with explicit `match`
  - [ ] Run tests

### Optimizations (Recommended)

- [ ] **Optimization 1: Use `num_enum`** (strongly recommended)
  - [ ] Add `num_enum = "0.7"` to Cargo.toml
  - [ ] Add `#[repr(u8)]` and `#[derive(TryFromPrimitive)]`
  - [ ] Remove manual `TryFrom` implementations
  - [ ] Verify tests still pass

- [ ] **Optimization 2: Define physical constants**
  - [ ] Create `src/config/constants.rs`
  - [ ] Define `PIPER_JOINT_COUNT`, `MAX_JOINT_RAD`, `MAX_END_POSE_M`
  - [ ] Replace all magic numbers with constants
  - [ ] Update validation functions to use constants

- [ ] **Optimization 3: Add log rate limiting**
  - [ ] Choose approach: counter-based (Option A) OR `log_once` (Option B)
  - [ ] Add rate limiting to `validate_joint_rad()`
  - [ ] Add rate limiting to `validate_end_pose_meters()`
  - [ ] Test with continuous NaN input to verify no log flooding

- [ ] **Fix 2**: Add joint index validation
  - [ ] Update `pipeline.rs:469` (and rx_loop equivalents)
  - [ ] Update `pipeline.rs:594` (and rx_loop equivalents)
  - [ ] Update `pipeline.rs:641` (and rx_loop equivalents)
  - [ ] Use `PIPER_JOINT_COUNT` constant instead of hardcoded `6`
  - [ ] Add tests

- [ ] **Fix 3**: Add NaN/Inf validation
  - [ ] Create `src/driver/validation.rs`
  - [ ] Add `validate_joint_rad()` helper with rate limiting
  - [ ] Add `validate_end_pose_meters()` helper with rate limiting
  - [ ] Update all float assignments in pipeline.rs (use `match` NOT `?`)
  - [ ] Add tests

- [ ] **Fix 4**: Standardize frame length validation
  - [ ] Add `validate_frame()` helper
  - [ ] Update all `TryFrom<PiperFrame>` implementations
  - [ ] Add tests

### Test Requirements

- [ ] **Unit test: Enum validation**
  - [ ] Test all valid enum values are accepted
  - [ ] Test all invalid enum values are rejected
  - [ ] Test with num_enum (if used)

- [ ] **Unit test: rx_loop error handling**
  - [ ] Send malformed frames
  - [ ] Verify loop continues processing
  - [ ] Verify thread ID doesn't change (thread doesn't restart)

- [ ] **Unit test: Float validation**
  - [ ] Test NaN/Inf rejection
  - [ ] Test out-of-range rejection
  - [ ] Test rate limiting (simulate 1000 NaN values)

- [ ] **Integration test: CAN bus fault tolerance**
  - [ ] Simulate sensor sending continuous NaN
  - [ ] Verify logs are rate-limited
  - [ ] Verify robot continues operating

---

## Testing Strategy

### Unit Tests

```rust
#[cfg(test)]
mod protocol_tests {
    use super::*;

    #[test]
    fn test_enum_try_from_all_valid() {
        // Test all enums accept only valid values
    }

    #[test]
    fn test_enum_try_from_invalid_rejects() {
        // Test all enums reject invalid values
    }

    #[test]
    fn test_joint_index_bounds() {
        // Test joint index validation
    }

    #[test]
    fn test_float_validation_rejects_nan() {
        // Test NaN/Inf rejection
    }

    #[test]
    fn test_rate_limiting_works() {
        // Simulate 1000 NaN values
        // Verify only ~1 log message appears
    }
}
```

### Critical Test: Loop Resilience (CRITICAL)

**Purpose**: Verify that `rx_loop` continues processing even when receiving continuous bad frames.

**WRONG APPROACH** (DO NOT DO THIS):
```rust
// ❌ WRONG: This checks test runner thread ID, not RX thread ID
let initial_tid = std::thread::current().id();
// ... send bad frames ...
let current_tid = std::thread::current().id();
assert_eq!(initial_tid, current_tid, "..."); // Always passes!
```

**Problem**: `rx_loop` runs in a background thread spawned by the driver. Checking `current().id()` only gives you the test runner's thread ID, which never changes.

**CORRECT APPROACH**: Functional verification - send bad frames, then a good frame, verify the good frame is processed.

```rust
#[test]
#[ignore] // Requires hardware or mock
fn test_rx_loop_resilience_bad_frames() {
    use crate::can::{PiperFrame};
    use std::time::Duration;

    // 1. Send 100 bad frames (invalid enum values)
    for _ in 0..100 {
        let bad_frame = PiperFrame::new_standard(
            0x2A1,  // Robot Status ID
            &[0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]  // Invalid robot_status
        );
        // Send bad_frame to robot...
    }

    // 2. Wait for bad frames to be processed
    std::thread::sleep(Duration::from_millis(200));

    // 3. Send 1 good frame (valid ControlMode::CanControl = 0x01)
    // RobotStatusFeedback with:
    // - control_mode: 0x01 (CanControl)
    // - robot_status: 0x00 (Normal)
    // - Other fields: 0x00
    let good_frame = PiperFrame::new_standard(
        0x2A1,
        &[0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]
    );
    // Send good_frame to robot...

    // 4. Wait for good frame to be processed
    std::thread::sleep(Duration::from_millis(100));

    // 5. CRITICAL CHECK: If RX thread is alive, it should process the good frame
    let state = /* get robot state */;
    assert_eq!(
        state.control_mode,
        ControlMode::CanControl,
        "RX thread died! Failed to process valid frame after bad frames. \
        This indicates use of `?` operator in rx_loop instead of `match` + `continue`."
    );
}
```

**Why This Test Works**:
- **Bad scenario**: 100 consecutive frames with invalid `robot_status` values (0xFF)
- **Expected behavior**: RX thread logs warnings but continues processing
- **Verification**: A subsequent valid frame with `robot_status = 0x00` (Normal) is correctly parsed and state updates
- **Wrong behavior**: If `?` was used, RX thread would terminate on first bad frame, good frame would never be processed

**Alternative Test** (with mock):
```rust
#[test]
fn test_rx_loop_resilience_mock() {
    use crate::driver::PiperContext;
    use crate::protocol::feedback::RobotStatusFeedback;

    // Mock rx_loop behavior
    let ctx = PiperContext::new();

    // Simulate 100 bad frames
    for _ in 0..100 {
        let bad_frame = PiperFrame::new_standard(0x2A1, &[0xFF; 8]);
        let result = RobotStatusFeedback::try_from(bad_frame);

        match result {
            Ok(feedback) => match RobotStatus::try_from_primitive(feedback.robot_status) {
                Ok(status) => {
                    // Should NOT happen with 0xFF
                    panic!("Unexpected success parsing invalid value");
                }
                Err(_) => {
                    // Expected: skip frame, continue processing
                }
            },
            Err(_) => {
                // Expected: frame parse failed, skip
            }
        }
    }

    // Verify context is still accessible (thread didn't crash)
    let _ = ctx.robot_status.load();
}
```

**Key Point**: Test must verify **functional behavior** (frame processing continues), not implementation details (thread IDs).

### Integration Tests

```rust
#[test]
fn test_malformed_can_frame_handling() {
    // Create malformed CAN frames
    // Verify they are rejected with appropriate errors
}
```

### Protocol Fuzzing

Consider adding fuzz tests:
```toml
[dependencies]
cargo-fuzz = "0.11"
```

```rust
// fuzz/fuzz_protocol_parser.rs
#![no_main]
use libfuzzer_sys::fuzz_target;
use piper_sdk::protocol::feedback::*;

fuzz_target!(|data: &[u8]| {
    if data.len() >= 8 {
        let frame = PiperFrame::new_standard(0x2A1, &data[..8]);
        let _ = RobotStatusFeedback::try_from(frame);
    }
});
```

---

## Rollback Plan

If `TryFrom` conversion is too disruptive:

1. **Keep `From<u8>` but add logging**:
```rust
impl From<u8> for ControlMode {
    fn from(value: u8) -> Self {
        let result = match value {
            0x00 => ControlMode::Standby,
            // ...
            unknown => {
                warn!("Unknown ControlMode value: 0x{:02X}, using Standby", unknown);
                ControlMode::Standby
            }
        };
        result
    }
}
```

2. **Add compile-time feature flag**:
```toml
[features]
default = ["strict_protocol_parsing"]
strict_protocol_parsing = []
```

```rust
#[cfg(feature = "strict_protocol_parsing")]
impl TryFrom<u8> for ControlMode { /* ... */ }

#[cfg(not(feature = "strict_protocol_parsing"))]
impl From<u8> for ControlMode { /* ... with logging */ }
```

---

## References

- [Rust TryFrom Trait](https://doc.rust-lang.org/std/convert/trait.TryFrom.html)
- [IEEE 754 Floating Point](https://en.wikipedia.org/wiki/IEEE_754)
- [CAN Frame Specification](docs/v0/can_protocol_specification.md)

---

**Next Steps**: After implementing these fixes, proceed to `03_client_layer_fixes.md`
