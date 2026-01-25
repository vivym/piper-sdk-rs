# Client Layer Fixes - Solution Plan

## Executive Summary

This document provides detailed solutions for **critical client layer issues** related to resource management in the Type State Pattern implementation. These fixes address memory leaks, panic safety, and proper use of unsafe code.

**Priority**: CRITICAL - Must fix before production use

**Estimated Effort**: 2-3 days

**Document Status**: REVISED (2025-01-25)
- ✅ Core ManuallyDrop approach is correct
- ✅ Panic Guard implementation corrected (now owns Arc, no leak)
- ✅ Added proper SAFETY comments for unsafe blocks
- ✅ Added MockPiper trait suggestion for CI/CD testing

---

## Issue Summary

| Issue | Severity | Impact | Location |
|-------|----------|--------|----------|
| `std::mem::forget` in state transitions | CRITICAL | Resource leaks, panic-unsafe | `src/client/state/machine.rs` (5 occurrences) |
| Double Arc clone before forget | HIGH | Memory leak per transition | `src/client/state/machine.rs` (5 occurrences) |
| No panic safety after operations | HIGH | Resource leaks on panic | `src/client/state/machine.rs` |

---

## Problem Analysis

### Current Implementation (Problematic)

The Type State Pattern uses `std::mem::forget` to prevent the `Drop` impl from running during state transitions:

**Current Code** (`machine.rs:310-353`):
```rust
pub fn enable_mit_mode(self, config: MitModeConfig) -> Result<Piper<Active<MitMode>>> {
    // 1. Send enable command (may panic)
    let enable_cmd = MotorEnableCommand::enable_all();
    self.driver.send_reliable(enable_cmd.to_frame())?;

    // 2. Wait for enable completion (may panic)
    self.wait_for_enabled(
        config.timeout,
        config.debounce_threshold,
        config.poll_interval,
    )?;

    // 3. Set MIT mode (may panic)
    let control_cmd = ControlModeCommandFrame::new(...);
    self.driver.send_reliable(control_cmd.to_frame())?;

    // 4. Clone Arc references (increments ref count)
    let driver = self.driver.clone();  // ❌ Increments Arc count
    let observer = self.observer.clone(); // ❌ Increments Arc count

    // 5. Forget self (prevents Drop, but doesn't decrement Arc)
    std::mem::forget(self); // ❌ Memory leak!

    // 6. Construct new state
    Ok(Piper {
        driver,
        observer,
        _state: PhantomData,
    })
}
```

**Problems**:
1. **Memory leak**: Every `clone()` + `forget()` cycle leaks one Arc reference
2. **Panic-unsafe**: If panic occurs before `forget()`, resources may leak
3. **Unnecessary clone**: We don't need to increment ref count just to move ownership

---

## Solution: Use ManuallyDrop Pattern

### Fixed Implementation

The correct approach is to use `ManuallyDrop` to extract fields without cloning:

**Fixed Code**:
```rust
pub fn enable_mit_mode(self, config: MitModeConfig) -> Result<Piper<Active<MitMode>>> {
    // === PHASE 1: All operations that can panic ===

    // 1. Send enable command
    let enable_cmd = MotorEnableCommand::enable_all();
    self.driver.send_reliable(enable_cmd.to_frame())?;

    // 2. Wait for enable completion
    self.wait_for_enabled(
        config.timeout,
        config.debounce_threshold,
        config.poll_interval,
    )?;

    // 3. Set MIT mode
    let control_cmd = ControlModeCommandFrame::new(
        ControlModeCommand::CanControl,
        MoveMode::MoveM,
        config.speed_percent,
        MitMode::Mit,
        0,
        InstallPosition::Invalid,
    );
    self.driver.send_reliable(control_cmd.to_frame())?;

    // === PHASE 2: No-panic zone starts here ===

    // Use ManuallyDrop to prevent Drop, then extract fields without cloning
    let mut this = std::mem::ManuallyDrop::new(self);

    // SAFETY: `this.driver` is a valid Arc<crate::driver::Piper>.
    // We're moving it out of ManuallyDrop, which prevents the original
    // `self` from being dropped. This is safe because:
    // 1. `this.driver` is immediately moved into the returned Piper
    // 2. No other access to `this.driver` occurs after this read
    // 3. The original `self` is never dropped (ManuallyDrop guarantees this)
    let driver = unsafe { std::ptr::read(&this.driver) };

    // SAFETY: `this.observer` is a valid Arc<Observer>.
    // Same safety reasoning as driver above.
    let observer = unsafe { std::ptr::read(&this.observer) };

    // `this` is dropped here, but since it's ManuallyDrop,
    // the inner `self` is NOT dropped, preventing double-disable

    // Construct new state (no Arc ref count increase!)
    Ok(Piper {
        driver,
        observer,
        _state: PhantomData,
    })
}
```

### Why This Works

1. **No memory leak**: We move the `Arc` references instead of cloning
   - Old behavior: `clone()` → ref count 1→2, `forget()` → never decrements
   - New behavior: `ptr::read()` → moves Arc, ref count stays 1

2. **Panic-safe**: All panic-able operations complete before `ManuallyDrop`
   - If panic occurs in Phase 1, `self` is dropped normally (sends disable)
   - Phase 2 is guaranteed not to panic (only ptr::read, struct construction)

3. **Zero overhead**: No extra Arc increment/decrement operations

---

## Implementation Steps

### Step 1: Update All State Transition Methods

All these methods need the same fix:

| Method | Location | Lines |
|--------|----------|-------|
| `enable_mit_mode()` | machine.rs | 310-353 |
| `enable_position_mode()` | machine.rs | 361-408 |
| `emergency_stop()` | machine.rs | 590-606 |
| `disable()` (MitMode) | machine.rs | 784-813 |
| `disable()` (PositionMode) | machine.rs | 1043-1072 |

### Step 2: Apply the Fix Template

**Template**:
```rust
pub fn transition_method(self, ...) -> Result<Piper<NewState>> {
    // === PHASE 1: Panic-able operations ===
    // All operations that can fail or panic go here
    self.driver.send_reliable(cmd.to_frame())?;
    self.wait_for_enabled(...)?;

    // === PHASE 2: No-panic zone ===
    // Extract fields using ManuallyDrop
    let mut this = std::mem::ManuallyDrop::new(self);

    // SAFETY: `this.driver` is a valid Arc<crate::driver::Piper>.
    // We're moving it out of ManuallyDrop. Safe because:
    // 1. Field is immediately moved into returned value
    // 2. No other access occurs after this read
    // 3. Original self is never dropped (ManuallyDrop)
    let driver = unsafe { std::ptr::read(&this.driver) };

    // SAFETY: `this.observer` is a valid Arc<Observer>.
    // Same safety reasoning as driver.
    let observer = unsafe { std::ptr::read(&this.observer) };

    // Construct new state
    Ok(Piper {
        driver,
        observer,
        _state: PhantomData,
    })
}
```

### Step 3: Add Safety Documentation

Add documentation explaining the safety invariant:

```rust
// State Transition Safety Invariant:
//
// All state transitions follow this two-phase pattern:
//
// PHASE 1 (Panic-able):
//   - Send CAN commands
//   - Wait for hardware responses
//   - Perform any I/O operations
//   - Any operation here may panic, in which case `self` is
//     dropped normally via the Drop impl, sending disable command
//
// PHASE 2 (No-panic zone):
//   - Wrap `self` in ManuallyDrop to prevent Drop
//   - Extract fields using ptr::read (moves Arc, doesn't clone)
//   - Construct new state
//   - This phase MUST NOT panic - if it does, we leak the robot
//     in enabled state (but Arc references are not leaked)
//
// The key insight: Arc pointer reads are safe as long as the
// original value isn't dropped, which ManuallyDrop guarantees.
```

---

## Enhanced Solution: Panic Guard (CORRECTED)

For extra safety, we can add a panic guard to ensure cleanup on panic:

**IMPORTANT**: The guard must own an `Arc` (not hold a reference) to avoid lifetime conflicts with `ManuallyDrop::new(self)`.

### Implementation

```rust
/// RAII guard that sends disable command if dropped before being committed
///
/// The guard owns an Arc clone to avoid lifetime issues when used with
/// ManuallyDrop. If dropped without being committed, it sends a disable
/// command to safely shut down the robot.
struct EnableGuard {
    driver: Arc<crate::driver::Piper>,  // ✅ Owns Arc (not borrowed)
    committed: bool,
}

impl EnableGuard {
    fn new(driver: &Arc<crate::driver::Piper>) -> Self {
        Self {
            driver: driver.clone(),  // ✅ Clone Arc for ownership
            committed: false,
        }
    }

    fn commit(mut self) {
        self.committed = true;
        // ✅ No mem::forget - let Drop run normally
        // Drop will see committed=true and do nothing
    }
}

impl Drop for EnableGuard {
    fn drop(&mut self) {
        if !self.committed {
            // Rollback: send disable command on panic/drop
            error!("State transition panicked, sending emergency disable");
            let _ = self.driver.send_reliable(
                MotorDisableCommand::disable_all().to_frame()
            );
        }
    }
}
```

### Usage

```rust
pub fn enable_mit_mode(self, config: MitModeConfig) -> Result<Piper<Active<MitMode>>> {
    // Create guard that will rollback on panic
    // ✅ Guard owns its own Arc clone - no lifetime conflict
    let _guard = EnableGuard::new(&self.driver);

    // All panic-able operations
    self.driver.send_reliable(enable_cmd.to_frame())?;
    self.wait_for_enabled(...)?;
    self.driver.send_reliable(control_cmd.to_frame())?;

    // Commit: disable rollback (guard's Drop will see committed=true)
    _guard.commit();

    // Safe extraction (no panic possible after commit)
    let mut this = std::mem::ManuallyDrop::new(self);

    // SAFETY: `this.driver` is a valid Arc<crate::driver::Piper>.
    // We're moving it out of ManuallyDrop, which prevents the original
    // `self` from being dropped. This is safe because:
    // 1. `this.driver` is immediately moved into the returned Piper
    // 2. No other access to `this.driver` occurs after this read
    // 3. The original `self` is never dropped (ManuallyDrop guarantees this)
    let driver = unsafe { std::ptr::read(&this.driver) };

    // SAFETY: Same reasoning as driver above.
    // `this.observer` is a valid Arc and we're moving it out.
    let observer = unsafe { std::ptr::read(&this.observer) };

    // `this` is dropped here, but since it's ManuallyDrop,
    // the inner `self` is NOT dropped, preventing double-disable

    Ok(Piper { driver, observer, _state: PhantomData })
}
```

### Key Differences from Previous Version

| Aspect | Previous (Broken) | Fixed (Correct) |
|--------|------------------|-----------------|
| Guard field type | `&'a Arc<Piper>` (borrowed) | `Arc<Piper>` (owned) |
| Lifetime parameter | `<'a>` required | No lifetime needed |
| Conflicts with ManuallyDrop | ❌ Won't compile | ✅ Compiles |
| commit() implementation | `mem::forget(self)` ❌ Leaks Arc | Just sets `committed = true` ✅ |
| Arc reference count | +1 (guard) +1 (original) = 2 | +1 (guard) only |

### Why This Works

1. **No lifetime conflict**: Guard owns its Arc, so it doesn't borrow from `self`
2. **No Arc leak**: The guard's Arc is properly dropped when guard's Drop runs
3. **Panic safety**: If panic occurs before commit, guard's Drop sends disable
4. **Commit efficiency**: After commit, guard's Drop sees `committed=true` and does nothing

---

## Alternative: Restructure to Avoid forget

An alternative is to restructure the state machine to avoid `forget` entirely:

### Option A: Builder Pattern

```rust
impl Piper<Standby> {
    pub fn enable_mit_mode(
        &self,
        config: MitModeConfig,
    ) -> Result<Piper<Active<MitMode>>> {
        // Don't consume self, just send commands
        self.driver.send_reliable(enable_cmd.to_frame())?;
        self.wait_for_enabled(...)?;
        self.driver.send_reliable(control_cmd.to_frame())?;

        // Return new state with cloned Arcs
        Ok(Piper {
            driver: self.driver.clone(),
            observer: self.observer.clone(),
            _state: PhantomData,
        })
    }
}
```

**Pros**: No unsafe code, no forget
**Cons**: Loses compile-time state exclusivity (could have both Standby and Active instances)

### Option B: Consume Builder

```rust
impl Piper<Standby> {
    pub fn enable_mit_mode(
        self,
        config: MitModeConfig,
    ) -> Result<Piper<Active<MitMode>>> {
        // Send commands using self
        self.driver.send_reliable(enable_cmd.to_frame())?;
        self.wait_for_enabled(...)?;
        self.driver.send_reliable(control_cmd.to_frame())?;

        // Deconstruct self without triggering Drop
        // Move fields out (requires std::mem::take or similar)
        let driver = self.driver.clone();
        let observer = self.observer.clone();

        // Allow self to drop (sends disable)
        drop(self);

        // Re-enable immediately (inefficient but safe)
        // ... this approach doesn't work well

        unimplemented!()
    }
}
```

**Recommendation**: Use the **ManuallyDrop pattern** (primary solution) as it maintains the type state guarantees while fixing the memory leak.

---

## Testing

### Unit Test: Memory Leak Detection

```rust
#[test]
fn test_no_arc_leak_on_state_transition() {
    use std::sync::Arc;

    // Create a Piper in Standby state
    // Note: This test needs a mock driver
    let driver = Arc::new(MockPiper::new());
    let observer = Observer::new(driver.clone());
    let robot = Piper {
        driver: driver.clone(),
        observer,
        _state: PhantomData,
    };

    // Get initial Arc ref count
    let initial_count = Arc::strong_count(&driver);

    // Perform state transition
    let robot_active = robot.enable_mit_mode(MitModeConfig::default()).unwrap();

    // Verify ref count didn't increase
    let final_count = Arc::strong_count(&driver);
    assert_eq!(
        final_count, initial_count,
        "Arc ref count leaked: {} -> {}",
        initial_count, final_count
    );

    // Clean up
    drop(robot_active);
}
```

### Stress Test: Many Transitions

```rust
#[test]
fn test_many_transitions_no_leak() {
    // This test needs hardware or mock
    // Perform 1000 enable/disable cycles
    // Verify Arc ref count returns to original value
}
```

### Panic Test: Panic Safety

```rust
#[test]
#[should_panic]
fn test_panic_during_transition_sends_disable() {
    // Mock driver that panics during send_reliable
    // Verify that:
    // 1. Panic is propagated
    // 2. Disable command is sent (via Drop or guard)
}
```

### Mock Driver for CI/CD Testing

For testing without hardware, define a mock driver trait:

**Create `src/driver/mock.rs`**:
```rust
use crate::can::PiperFrame;
use std::sync::Arc;
use crate::protocol::control::MotorDisableCommand;

#[derive(Clone)]
pub struct MockPiper {
    pub send_count: Arc<std::sync::atomic::AtomicUsize>,
    pub last_frame: Arc<std::sync::Mutex<Option<PiperFrame>>>,
}

impl MockPiper {
    pub fn new() -> Self {
        Self {
            send_count: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            last_frame: Arc::new(std::sync::Mutex::new(None)),
        }
    }

    pub fn send_reliable(&self, frame: PiperFrame) -> Result<(), RobotError> {
        self.send_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        *self.last_frame.lock().unwrap() = Some(frame);
        Ok(())
    }
}

// Minimal trait needed for client layer tests
pub trait PiperLike {
    fn send_reliable(&self, frame: PiperFrame) -> Result<(), RobotError>;
}

impl PiperLike for MockPiper {
    fn send_reliable(&self, frame: PiperFrame) -> Result<(), RobotError> {
        self.send_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        *self.last_frame.lock().unwrap() = Some(frame);
        Ok(())
    }
}
```

**Usage in tests**:
```rust
#[test]
fn test_no_arc_leak_on_state_transition() {
    use std::sync::Arc;

    // Create mock driver
    let driver = Arc::new(MockPiper::new());
    let observer = Observer::new(driver.clone());

    // Create robot in Standby state
    let robot = Piper {
        driver: driver.clone(),
        observer,
        _state: PhantomData,
    };

    // Get initial Arc ref count
    let initial_count = Arc::strong_count(&driver);

    // Perform state transition
    let robot_active = robot.enable_mit_mode(MitModeConfig::default()).unwrap();

    // Verify ref count didn't increase
    let final_count = Arc::strong_count(&driver);
    assert_eq!(
        final_count, initial_count,
        "Arc ref count leaked: {} -> {}",
        initial_count, final_count
    );

    // Verify disable was sent (via Drop)
    drop(robot_active);
    assert_eq!(driver.send_count.load(std::sync::atomic::Ordering::Relaxed), 2);
    // First send: enable command
    // Second send: disable command (via Drop)
}
```

**Benefits**:
- ✅ Fast unit tests without hardware
- ✅ CI/CD friendly
- ✅ Can test error conditions by making mock return errors
- ✅ Can verify exact command sequences

---

## Implementation Checklist

- [ ] Update all state transition methods to use ManuallyDrop
  - [ ] `enable_mit_mode()` - Add proper SAFETY comments
  - [ ] `enable_position_mode()` - Add proper SAFETY comments
  - [ ] `emergency_stop()` - Add proper SAFETY comments
  - [ ] `disable()` (MitMode) - Add proper SAFETY comments
  - [ ] `disable()` (PositionMode) - Add proper SAFETY comments
- [ ] Add safety documentation to all transition methods
  - [ ] Document Phase 1/Phase 2 pattern
  - [ ] Explain ManuallyDrop safety invariants
- [ ] (Optional) Implement corrected EnableGuard
  - [ ] Change to hold `Arc<crate::driver::Piper>` (owned, not borrowed)
  - [ ] Remove `mem::forget` from commit()
  - [ ] Add proper SAFETY comments
- [ ] Add unit tests for Arc leak detection
  - [ ] Implement MockPiper for CI/CD
  - [ ] Test ref count before/after transitions
- [ ] Add stress test for many transitions
- [ ] Add panic safety tests
- [ ] Run full test suite

---

## Verification

### Memory Leak Check

Run the stress test with Valgrind or ASAN:

```bash
# On Linux
cargo test --test stress_test -- --ignored
valgrind --leak-check=full --show-leak-kinds=all ./target/debug/stress_test

# Or use heap profiling
export MALLOC_STATS=1
cargo test --test stress_test -- --ignored
```

### Reference Count Monitoring

Add debug logging to track Arc ref counts:

```rust
impl<State> Piper<State> {
    fn debug_ref_count(&self) -> usize {
        Arc::strong_count(&self.driver)
    }
}

// In tests
let robot = connect(...);
trace!("Initial ref count: {}", robot.debug_ref_count());
let robot = robot.enable_mit_mode(...)?;
trace!("After transition: {}", robot.debug_ref_count());
```

---

## Additional Improvement: Speed Percent Validation

While fixing the client layer, also add validation for `speed_percent` parameter:

**Current Code** (`machine.rs:130-141, 163-172`):
```rust
pub struct MitModeConfig {
    pub speed_percent: u8, // Default: 100
    // Note: value of 0 could lock joints
}
```

**Fix**:
```rust
pub struct MitModeConfig {
    speed_percent: NonZeroU8, // Enforces > 0 at compile time
}

impl MitModeConfig {
    pub fn with_speed_percent(mut self, percent: u8) -> Self {
        self.speed_percent = NonZeroU8::new(percent)
            .expect("speed_percent must be > 0");
        self
    }
}
```

Or with runtime validation:
```rust
impl MitModeConfig {
    pub fn new(config: MitModeConfigBuilder) -> Result<Self, RobotError> {
        if config.speed_percent == 0 {
            return Err(RobotError::ConfigError(
                "speed_percent must be > 0".to_string()
            ));
        }
        Ok(config)
    }
}
```

---

## Rollback Plan

If ManuallyDrop approach proves problematic:

1. **Accept the memory leak** as temporary workaround:
   - Document that each transition leaks 1 Arc reference
   - Estimate ~100 bytes per transition
   - For 1 million transitions: 100MB leak (unacceptable long-term)

2. **Switch to builder pattern**:
   - Sacrifice type state exclusivity
   - Add runtime checks to prevent concurrent access

3. **Use RefCell pattern**:
   - Inner state wrapped in `RefCell<PiperState>`
   - Type state moved to runtime check

**Recommendation**: ManuallyDrop is the correct solution. Do not roll back.

---

## References

- [Rust ManuallyDrop Documentation](https://doc.rust-lang.org/std/mem/struct.ManuallyDrop.html)
- [Rust Type State Pattern](https://docs.rs/rust-typestate/latest/rusttypestate/)
- [Arc Reference Counting](https://doc.rust-lang.org/std/sync/struct.Arc.html)

---

**Changelog**:
- 2025-01-25: Initial version
- 2025-01-25: **REVISED** - Fixed Panic Guard implementation based on expert review
  - Changed EnableGuard to hold `Arc<Piper>` (owned) instead of `&'a Arc<Piper>` (borrowed)
  - Removed `mem::forget(self)` from commit() to prevent Arc leak
  - Added proper `// SAFETY:` comments explaining ptr::read safety
  - Added MockPiper trait suggestion for CI/CD testing

---

**Next Steps**: After implementing these fixes, proceed to `04_can_adapter_fixes.md`
