# Driver Layer Fixes - Solution Plan

## Executive Summary

This document provides detailed solutions for **driver layer issues** related to frame group synchronization, timeout handling, and timestamp management.

**Priority**: MEDIUM

**Estimated Effort**: 1-2 days

**Document Status**: REVISED (2025-01-25)
- ✅ Fix 1: Simple solution (log + reset) approved and sufficient
- ✅ Fix 2: Fixed first frame drop bug - now allows immediate commit on first complete frame group
- ✅ Fix 4: Enhanced to recommend `Instant` for internal watchdog, `SystemTime` for display

---

## Issue Summary

| Issue | Severity | Impact | Location |
|-------|----------|--------|----------|
| TOCTTOU race in timeout handling | MEDIUM | Lost CAN data | `src/driver/pipeline.rs:108-118` |
| Timestamp overflow on first frame | MEDIUM | First frame rejected | `src/driver/pipeline.rs:321-326` |
| Frame group timeout inconsistency | LOW | Inconsistent state | `src/driver/pipeline.rs` |
| No explicit stale data API | LOW | Difficult to detect CAN loss | `src/driver/state.rs` |

---

## Fix 1: TOCTTOU Race in Timeout Handling (APPROVED)

### Problem

The timeout check and buffer reset create a time-of-check-to-time-of-use race:

**Current Code** (`pipeline.rs:108-118`):
```rust
let elapsed = last_frame_time.elapsed();
if elapsed > frame_group_timeout {
    // ❌ RACE: New frame might arrive between check and reset
    pending_joint_pos = [0.0; 6];
    pending_end_pose = [0.0; 6];
    joint_pos_frame_mask = 0;
    end_pose_frame_mask = 0;
    // ...
}
```

**Race Scenario**:
1. Thread checks `elapsed > timeout` → TRUE
2. **New CAN frame (0x2A5) arrives** → updates `pending_joint_pos[0]`
3. Thread resets to `[0.0; 6]` → **loses the newly received data**

### Solution (APPROVED)

**Use the simplified solution (log + reset)**:

```rust
let elapsed = last_frame_time.elapsed();
if elapsed > frame_group_timeout {
    warn!(
        "Frame group timeout after {:?}, resetting pending buffers",
        elapsed
    );

    // Reset buffers (any frame arriving between timeout check and here
    // will be processed in next iteration)
    pending_joint_pos = [0.0; 6];
    pending_end_pose = [0.0; 6];
    joint_pos_frame_mask = 0;
    end_pose_frame_mask = 0;

    last_frame_time = Instant::now();
}
```

**Why This Is Sufficient**:

1. **Single-threaded IO loop**: The "race" is actually a logic ordering issue, not multi-threaded competition
2. **Tiny window**: The time between check and reset is microseconds
3. **Low impact**: Missing one frame is acceptable for this use case
4. **Rust ownership model**: The sequence counter approach is unnecessary complexity
5. **Next iteration recovery**: Any frame arriving during reset is processed in the next loop iteration

**Expert Review**: ✅ Approved - Simple solution is correct and sufficient for this context.

---

## Fix 2: Timestamp Overflow on First Frame (CORRECTED)

### Problem

The first velocity frame may be incorrectly rejected due to timestamp comparison:

**Current Code** (`pipeline.rs:321-326`):
```rust
let time_since_last_commit =
    frame.timestamp_us.saturating_sub(last_vel_commit_time_us);

let timeout_threshold_us = 6000; // 6ms timeout
if all_received || time_since_last_commit > timeout_threshold_us {
    // Commit
}
```

**Issue**: If `last_vel_commit_time_us == 0` (initial state), then:
- First frame: `time_since_last_commit = timestamp_us - 0 = timestamp_us`
- If `timestamp_us < 6000`, timeout triggers immediately, **rejecting valid first frame**

### Previous Fix (HAD BUG)

The previous fix used `continue` after handling the first frame:

```rust
if last_vel_commit_time_us == 0 {
    // First frame ever received, accept unconditionally
    pending_joint_dynamic.group_timestamp_us = frame.timestamp_us;
    pending_joint_dynamic.valid_mask = vel_update_mask;
    ctx.joint_dynamic.store(Arc::new(pending_joint_dynamic.clone()));

    // Reset for next cycle
    vel_update_mask = 0;
    last_vel_commit_time_us = frame.timestamp_us;
    last_vel_packet_instant = None;
    continue; // ❌ BUG: Skips commit logic even if all_received == true!
}
```

**Bug**: Using `continue` skips the subsequent commit logic. Even if the first frame group is complete (`all_received == true`), it would be discarded, causing a "startup delay" or "first frame drop" issue.

### Corrected Solution

**Calculate time_since_last_commit = 0 for first frame, let it flow to commit logic naturally**:

```rust
// Calculate time since last commit (handle initial state)
let time_since_last_commit = if last_vel_commit_time_us == 0 {
    // First frame ever - treat as if no time has elapsed
    // This allows the first complete frame group to be committed immediately
    0
} else {
    // Normal wrap-around subtraction for subsequent frames
    frame.timestamp_us.wrapping_sub(last_vel_commit_time_us)
};

// Normal commit logic: either all received OR timeout
if all_received || time_since_last_commit > timeout_threshold_us {
    // Commit the frame group
    pending_joint_dynamic.group_timestamp_us = frame.timestamp_us;
    pending_joint_dynamic.valid_mask = vel_update_mask;
    ctx.joint_dynamic.store(Arc::new(pending_joint_dynamic.clone()));

    // Log first commit for debugging
    if last_vel_commit_time_us == 0 {
        info!("First velocity frame group committed immediately");
    }

    // Reset for next cycle
    vel_update_mask = 0;
    last_vel_commit_time_us = frame.timestamp_us;
    last_vel_packet_instant = None;
}
```

### Why This Works

**Scenario 1: First frame group is complete**
```
- last_vel_commit_time_us == 0 (initial state)
- all_received == true (all 6 joints received)
- time_since_last_commit == 0 (first frame)
- Result: Commits immediately ✅ (no startup delay)
```

**Scenario 2: First frame group incomplete**
```
- last_vel_commit_time_us == 0 (initial state)
- all_received == false (only 3 of 6 joints)
- time_since_last_commit == 0
- Result: Waits for more frames OR timeout ✅
```

**Scenario 3: Subsequent frame groups**
```
- last_vel_commit_time_us != 0 (normal operation)
- all_received OR timeout check works normally ✅
```

### Key Differences

| Approach | Previous (Bug) | Fixed (Correct) |
|----------|----------------|-----------------|
| First frame handling | Special case with `continue` | Treated as `time_since = 0` |
| Flow control | Skips commit logic | Flows through to commit logic |
| First complete frame | Discarded ❌ | Committed immediately ✅ |
| Startup behavior | Delay until second group | Immediate data available ✅ |

---

## Fix 3: Unify Frame Group Timeouts

### Problem

Two different timeout mechanisms exist:

| Timeout | Location | Default |
|---------|----------|---------|
| Frame group timeout | `frame_group_timeout_ms` | 10ms |
| Velocity buffer timeout | Hard-coded | 6ms |

This creates inconsistent state freshness guarantees.

### Solution

Unify timeouts through configuration:

**Add to `PipelineConfig`**:
```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PipelineConfig {
    pub receive_timeout_ms: u64,
    pub frame_group_timeout_ms: u64,
    pub velocity_buffer_timeout_us: u64, // ✅ NEW
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            receive_timeout_ms: 2,
            frame_group_timeout_ms: 10,
            velocity_buffer_timeout_us: 10_000, // 10ms (consistent with frame group)
        }
    }
}
```

**Update velocity buffer processing** (using corrected Fix 2 logic):
```rust
let timeout_threshold_us = config.velocity_buffer_timeout_us;

// Calculate time since last commit (handles initial state from Fix 2)
let time_since_last_commit = if last_vel_commit_time_us == 0 {
    0 // First frame - no time elapsed
} else {
    frame.timestamp_us.wrapping_sub(last_vel_commit_time_us)
};

if all_received || time_since_last_commit > timeout_threshold_us {
    // Commit logic from Fix 2...
}
```

---

## Fix 4: Add Explicit Stale Data API (ENHANCED)

### Problem

When CAN bus stops sending frames, state simply stops updating. No mechanism exists to detect stale data.

### Solution: Recommended Approach

**Use `Instant` for internal watchdog, `SystemTime` for display**:

#### Rationale

| Time Type | Monotonic? | Affected by clock changes? | Use Case |
|-----------|-----------|---------------------------|----------|
| `std::time::Instant` | ✅ Yes (guaranteed) | ❌ No | Internal watchdog/control logic |
| `std::time::SystemTime` | ❌ No | ✅ Yes (NTP, manual adjustment) | Display, logging, cross-process sync |

**Why Use `Instant` for Watchdog**:
- Monotonically increasing - never goes backward
- Not affected by system clock changes (NTP jumps, manual adjustments)
- Reliable for timeout-based control decisions
- Prevents false positives (data marked as stale due to clock jump)

**Why Keep `SystemTime` for Display**:
- Unix timestamps are meaningful to humans
- Can be correlated across processes/machines
- Suitable for UI display and log analysis

#### Implementation

**Add `Instant` field to state structures**:

```rust
// In state structures
pub struct JointPositionState {
    pub position: [f64; 6],
    pub system_timestamp_us: u64,  // For display/logging (SystemTime)
    pub monotonic_timestamp: std::time::Instant,  // For watchdog (Instant)
    pub valid_mask: u8,
}
```

**Update pipeline to record both timestamps**:

```rust
// In io_loop when committing state
let now_instant = std::time::Instant::now();
let now_system = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .unwrap()
    .as_micros() as u64;

pending_joint_position.system_timestamp_us = now_system;
pending_joint_position.monotonic_timestamp = now_instant;
// ... commit ...
```

**Add monotonic freshness check to `PiperContext`**:

```rust
impl PiperContext {
    /// Check if state data is fresh (using monotonic time)
    ///
    /// This is the RECOMMENDED method for control logic and watchdogs.
    /// Uses `Instant` which is not affected by system clock changes.
    ///
    /// # Parameters
    /// - `max_age`: Maximum acceptable data age
    ///
    /// # Returns
    /// - `true`: Data is fresh (age < max_age)
    /// - `false`: Data is stale (age >= max_age)
    pub fn is_joint_position_fresh_instant(&self, max_age: std::time::Duration) -> bool {
        let pos = self.joint_position.load();
        pos.monotonic_timestamp.elapsed() <= max_age
    }

    pub fn is_joint_dynamic_fresh_instant(&self, max_age: std::time::Duration) -> bool {
        let vel = self.joint_dynamic.load();
        vel.monotonic_timestamp.elapsed() <= max_age
    }

    pub fn is_end_pose_fresh_instant(&self, max_age: std::time::Duration) -> bool {
        let pose = self.end_pose.load();
        pose.monotonic_timestamp.elapsed() <= max_age
    }

    /// Check freshness using system time (for display/logging)
    ///
    /// ⚠️ WARNING: This method is affected by system clock changes.
    /// Use `*_fresh_instant()` methods for control logic.
    pub fn is_joint_position_fresh(&self, max_age_us: u64) -> bool {
        let pos = self.joint_position.load();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_micros() as u64;
        now.saturating_sub(pos.system_timestamp_us) <= max_age_us
    }
}
```

**Add to `Observer`**:

```rust
impl Observer {
    /// Check if all motion state is fresh (using monotonic time)
    ///
    /// This is the RECOMMENDED method for watchdogs and control logic.
    pub fn is_state_fresh_instant(&self, max_age: std::time::Duration) -> bool {
        self.ctx.is_joint_position_fresh_instant(max_age)
            && self.ctx.is_joint_dynamic_fresh_instant(max_age)
            && self.ctx.is_end_pose_fresh_instant(max_age)
    }

    /// Get age of oldest state data (using monotonic time)
    pub fn state_age(&self) -> std::time::Duration {
        let pos = self.ctx.joint_position.load();
        let vel = self.ctx.joint_dynamic.load();
        let pose = self.ctx.end_pose.load();

        let pos_age = pos.monotonic_timestamp.elapsed();
        let vel_age = vel.monotonic_timestamp.elapsed();
        let pose_age = pose.monotonic_timestamp.elapsed();

        pos_age.max(vel_age).max(pose_age)
    }

    /// Legacy method using system time (for display only)
    ///
    /// ⚠️ WARNING: Affected by system clock changes.
    pub fn is_state_fresh(&self, max_age_us: u64) -> bool {
        self.ctx.is_joint_position_fresh(max_age_us)
            && self.ctx.is_joint_dynamic_fresh(max_age_us)
            && self.ctx.is_end_pose_fresh(max_age_us)
    }

    pub fn state_age_us(&self) -> u64 {
        let pos = self.ctx.joint_position.load();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_micros() as u64;
        now.saturating_sub(pos.system_timestamp_us)
    }
}
```

#### Usage Examples

**Recommended: Using `Instant` for control logic**:
```rust
// In control loop (RECOMMENDED)
const MAX_STATE_AGE: std::time::Duration = std::time::Duration::from_millis(50);

if !robot.observer().is_state_fresh_instant(MAX_STATE_AGE) {
    warn!(
        "State data is stale (age: {:?}), stopping control",
        robot.observer().state_age()
    );
    return Err(RobotError::StaleData);
}
```

**Alternative: Using `SystemTime` for display**:
```rust
// For UI display or logging (safe even if clock changes)
let state_age_us = robot.observer().state_age_us();
info!("State age: {}us", state_age_us);
```

#### Benefits of This Approach

1. **Reliable Watchdog**: `Instant` guarantees monotonic behavior, unaffected by NTP jumps or manual clock adjustments
2. **Cross-Process Timestamps**: `SystemTime` still available for correlation across processes/machines
3. **Backward Compatibility**: Old `is_state_fresh()` methods still work (with documented limitations)
4. **Clear Intent**: Method names make it obvious which to use for control vs display

---

## Implementation Checklist

- [ ] **Fix 1**: Add logging to timeout reset (APPROVED)
  - [ ] Add warn log for timeout
  - [ ] Document why simple solution is sufficient
  - [ ] Add tests

- [ ] **Fix 2**: Handle initial timestamp state (CORRECTED)
  - [ ] Calculate `time_since_last_commit = 0` for first frame
  - [ ] Let first complete frame group flow to commit logic naturally
  - [ ] Add info log for first commit
  - [ ] Add tests for first frame immediate commit

- [ ] **Fix 3**: Unify frame group timeouts
  - [ ] Add `velocity_buffer_timeout_us` to `PipelineConfig`
  - [ ] Update velocity buffer processing (use corrected Fix 2 logic)
  - [ ] Update defaults
  - [ ] Add tests

- [ ] **Fix 4**: Add stale data API (ENHANCED)
  - [ ] Add `monotonic_timestamp: Instant` to state structures
  - [ ] Update pipeline to record both timestamps (Instant + SystemTime)
  - [ ] Add `is_*_fresh_instant()` methods to `PiperContext`
  - [ ] Add `is_state_fresh_instant()` to `Observer`
  - [ ] Add `state_age()` (Duration) to `Observer`
  - [ ] Keep legacy `is_state_fresh()` for backward compatibility
  - [ ] Document when to use Instant vs SystemTime
  - [ ] Add usage examples

---

## Testing

### Unit Tests

```rust
#[test]
fn test_first_velocity_frame_immediate_commit() {
    // Create mock CAN frame group with all 6 joints
    // Verify first complete frame group commits immediately
    // Verify no startup delay
}

#[test]
fn test_first_incomplete_frame_waits() {
    // Create mock CAN frame with only 3 of 6 joints
    // Verify it waits for timeout OR remaining frames
}

#[test]
fn test_stale_data_detection_instant() {
    // Create context with old Instant timestamp
    // Verify is_state_fresh_instant() returns false
}

#[test]
fn test_stale_data_detection_systemtime() {
    // Create context with old SystemTime timestamp
    // Verify is_state_fresh() returns false
}

#[test]
fn test_state_age_calculation() {
    // Create context with known Instant timestamp
    // Verify state_age() returns correct Duration
}
```

### Integration Tests

```rust
#[test]
#[ignore]
fn test_can_bus_disconnect_detection() {
    // Start robot
    // Stop CAN bus
    // Verify is_state_fresh_instant() returns false within 50ms
}

#[test]
#[ignore]
fn test_clock_jump_does_not_affect_watchdog() {
    // Start robot
    // Simulate system clock jump (adjust SystemTime)
    // Verify is_state_fresh_instant() still works correctly
    // Verify watchdog doesn't false positive
}
```

---

## Rollback Plan

If unified timeout causes issues:

1. **Make velocity timeout configurable only**:
```rust
pub struct PipelineConfig {
    pub velocity_buffer_timeout_us: Option<u64>, // None = use 6ms default
}
```

2. **Keep old behavior as fallback**:
```rust
let timeout_threshold_us = config.velocity_buffer_timeout_us
    .unwrap_or(6000); // 6ms default
```

---

## References

- [Frame Group Synchronization](docs/v0/frame_group_synchronization.md)
- [Timestamp Handling](docs/v0/timestamp_handling.md)

---

**Changelog**:
- 2025-01-25: Initial version (with logic bug in Fix 2)
- 2025-01-25: **REVISED** - Fixed critical issues based on expert review
  - Fix 1: Simple solution (log + reset) approved as sufficient
  - Fix 2: Fixed first frame drop bug - removed `continue`, let flow to commit logic naturally
  - Fix 4: Enhanced to recommend `Instant` for watchdog, `SystemTime` for display

---

**Next Steps**: After implementing these fixes, proceed to `06_lifecycle_fixes.md`
