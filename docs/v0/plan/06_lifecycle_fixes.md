# Lifecycle Management Fixes - Solution Plan

## Executive Summary

This document provides detailed solutions for **resource lifecycle issues** related to thread joins, shutdown sequences, and connection recovery.

**Priority**: MEDIUM-HIGH

**Estimated Effort**: 2-3 days

**Document Status**: REVISED (2025-01-25)
- ✅ Fix 1: Optimized to use `mpsc` channel for true blocking wait (no polling)
- ✅ Fix 2: Confirmed sleep is appropriate - added explanatory comment
- ✅ Fix 5: Fixed monotonic time issue - use App Start Relative Time pattern

---

## Issue Summary

| Issue | Severity | Impact | Location |
|-------|----------|--------|----------|
| Thread join can block forever | HIGH | Application hangs on shutdown | `src/driver/piper.rs` (Drop impl) |
| No graceful shutdown sequence | MEDIUM | Unsafe robot state on exit | `src/client/state/machine.rs` |
| No reconnect mechanism | MEDIUM | No recovery after disconnect | `src/client/state/machine.rs` |
| Channel disconnect not detected | LOW | Thread leaks | `src/driver/pipeline.rs` |

---

## Fix 1: Add Timeout to Thread Joins (OPTIMIZED)

### Problem

The `Drop` impl blocks forever waiting for threads to join:

**Current Code** (inferred in `piper.rs` Drop impl):
```rust
impl Drop for Piper {
    fn drop(&mut self) {
        self.is_running.store(false, Ordering::Relaxed);

        // ❌ Blocks forever if thread is deadlocked or panicked
        if let Some(handle) = self.io_thread.take() {
            let _ = handle.join();
        }

        if let Some(handle) = self.rx_thread.take() {
            let _ = handle.join();
        }

        if let Some(handle) = self.tx_thread.take() {
            let _ = handle.join();
        }
    }
}
```

**Risk**: If thread is deadlocked on RwLock or in spin loop, application hangs and requires SIGKILL.

### Solution (OPTIMIZED)

**Use `std::sync::mpsc` channel for true blocking wait with timeout**:

**Previous Approach (Polling)**:
```rust
// ❌ Inefficient: polls every 10ms, up to 10ms latency
while std::time::Instant::now() < deadline {
    if handle.is_finished() { ... }
    std::thread::sleep(Duration::from_millis(10));
}
```

**Optimized Approach (Channel-based)**:

**Add to `src/driver/piper.rs`**:
```rust
use std::sync::mpsc;
use std::time::Duration;

/// Extension trait for timeout-capable thread joins
trait JoinTimeout {
    fn join_timeout(self, timeout: Duration) -> std::thread::Result<()>;
}

impl<T> JoinTimeout for std::thread::JoinHandle<T> {
    fn join_timeout(self, timeout: Duration) -> std::thread::Result<()> {
        // Create a channel for signaling completion
        let (tx, rx) = mpsc::channel();

        // Spawn a watchdog thread that joins the target thread
        std::thread::spawn(move || {
            let result = self.join();
            // Send result (ignore send errors - receiver may have timed out)
            let _ = tx.send(result);
        });

        // Block with timeout - no busy waiting!
        match rx.recv_timeout(timeout) {
            Ok(join_result) => join_result.map(|_| ()), // Thread finished
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // Timeout: watchdog thread continues running
                // This is acceptable - OS will clean up on process exit
                Err(std::boxed::Box::new(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "Thread join timeout",
                )))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                // Channel disconnected unexpectedly - thread panicked
                Err(std::boxed::Box::new(std::io::Error::new(
                    std::io::ErrorKind::ConnectionReset,
                    "Thread panicked during join",
                )))
            }
        }
    }
}
```

**Update Drop impl**:
```rust
impl Drop for Piper {
    fn drop(&mut self) {
        // Use Release ordering for visibility
        self.is_running.store(false, Ordering::Release);

        // Give threads 2 seconds to exit cleanly
        let join_timeout = Duration::from_secs(2);

        // Try to join threads with timeout (true blocking wait)
        if let Some(handle) = self.io_thread.take() {
            let _ = handle.join_timeout(join_timeout);
        }

        if let Some(handle) = self.rx_thread.take() {
            let _ = handle.join_timeout(join_timeout);
        }

        if let Some(handle) = self.tx_thread.take() {
            let _ = handle.join_timeout(join_timeout);
        }
    }
}
```

### Key Benefits of Channel Approach

| Aspect | Previous (Polling) | Optimized (Channel) |
|--------|-------------------|---------------------|
| CPU usage | Busy waiting (every 10ms) | True blocking wait |
| Latency | Up to 10ms | Immediate response |
| Elegance | Manual loop | Uses `recv_timeout` |
| Rust idiomatic | Less idiomatic | More idiomatic ✅ |

**Trade-off**: One additional thread per join (watchdog thread), but this is acceptable for shutdown path.

---

## Fix 2: Add Graceful Shutdown Sequence (CONFIRMED)

### Problem

When `Drop` runs, robot may be in arbitrary state:
- May be in `Active<Mode>` (motors enabled)
- May be mid-trajectory
- May have pending commands

### Solution

Add explicit shutdown method:

**Add to `Piper<Active<_>>`**:
```rust
impl<M> Piper<Active<M>> {
    /// Gracefully shutdown the robot
    ///
    /// Performs a clean shutdown sequence:
    /// 1. Cancels any pending trajectories
    /// 2. Stops motion
    /// 3. Disables motors
    /// 4. Returns to Standby state
    ///
    /// # Example
    /// ```rust,ignore
    /// let robot = robot.enable_mit_mode(...)?;
    /// // ... use robot ...
    /// let standby_robot = robot.shutdown()?;
    /// // Robot now safely in Standby state
    /// ```
    pub fn shutdown(mut self) -> Result<Piper<Standby>> {
        info!("Starting graceful robot shutdown");

        // 1. Stop any motion
        trace!("Sending stop command");
        let raw = RawCommander::new(&self.driver);
        raw.stop_motion()?;

        // 2. Wait for robot to stop
        //
        // ⚠️ INTENTIONAL HARD WAIT:
        // This 100ms sleep allows CAN commands to propagate through the bus
        // and for the robot hardware to process the stop command before we
        // disable motors. In shutdown contexts, hard waits are acceptable
        // because:
        // - Shutdown is not performance-critical
        // - We need to ensure hardware reached a safe state
        // - Alternative (polling for "stopped" state) is unreliable
        trace!("Waiting for robot to stop (allowing CAN command propagation)");
        std::thread::sleep(Duration::from_millis(100));

        // 3. Disable motors
        trace!("Disabling motors");
        let disable_cmd = MotorDisableCommand::disable_all();
        self.driver.send_reliable(disable_cmd.to_frame())?;

        // 4. Wait for disable confirmation
        trace!("Waiting for disable confirmation");
        self.wait_for_disabled(
            Duration::from_secs(1),
            1, // debounce_threshold
            Duration::from_millis(10),
        )?;

        info!("Robot shutdown complete");

        // Transition to Standby state (using ManuallyDrop pattern)
        let mut this = std::mem::ManuallyDrop::new(self);
        let driver = unsafe { std::ptr::read(&this.driver) };
        let observer = unsafe { std::ptr::read(&this.observer) };

        Ok(Piper {
            driver,
            observer,
            _state: PhantomData,
        })
    }
}
```

**Update `Drop` to try graceful shutdown**:
```rust
impl<State> Drop for Piper<State> {
    fn drop(&mut self) {
        // Try to disable (ignore errors, may already be disabled)
        use crate::protocol::control::MotorDisableCommand;
        let _ = self.driver.send_reliable(MotorDisableCommand::disable_all().to_frame());

        // Stop threads with timeout
        self.is_running.store(false, Ordering::Release);
        let join_timeout = Duration::from_secs(2);

        for handle in [self.io_thread, self.rx_thread, self.tx_thread]
            .into_iter()
            .flatten()
        {
            let _ = handle.join_timeout(join_timeout);
        }
    }
}
```

**Note**: For `Active<Mode>`, the Drop still sends disable, but `shutdown()` provides a more controlled sequence.

**Expert Review**: ✅ Confirmed - The 100ms sleep is appropriate for this context. Hard waits are acceptable in shutdown where reliability > performance.

---

## Fix 3: Add Reconnect Mechanism

### Problem

Once connection is lost, there's no way to reconnect without creating a new `Piper` instance.

### Solution

Add reconnect capability:

**Add to `Piper<Disconnected>`**:
```rust
impl Piper<Disconnected> {
    /// Reconnect to the robot after connection loss
    ///
    /// # Parameters
    /// - `can_adapter`: New CAN adapter (or reuse existing)
    /// - `config`: Connection configuration
    ///
    /// # Returns
    /// - `Ok(Piper<Standby>)`: Successfully reconnected
    /// - `Err(RobotError)`: Reconnection failed
    ///
    /// # Example
    /// ```rust,ignore
    /// let robot = Piper::connect(can_adapter, config)?;
    /// // ... connection lost ...
    /// let robot = robot.reconnect(new_can_adapter, config)?;
    /// ```
    pub fn reconnect<C>(
        self,
        can_adapter: C,
        config: ConnectionConfig,
    ) -> Result<Piper<Standby>>
    where
        C: SplittableAdapter + Send + 'static,
        C::RxAdapter: Send + 'static,
        C::TxAdapter: Send + 'static,
    {
        info!("Attempting to reconnect to robot");

        // 1. Create new driver instance
        use crate::driver::Piper as RobotPiper;
        let driver = Arc::new(RobotPiper::new_dual_thread(can_adapter, None)?);

        // 2. Wait for feedback
        driver.wait_for_feedback(config.timeout)?;

        // 3. Create observer
        let observer = Observer::new(driver.clone());

        // 4. Return to Standby state
        info!("Reconnection successful");
        Ok(Piper {
            driver,
            observer,
            _state: PhantomData,
        })
    }
}
```

**Note**: Since `Disconnected` is a ZST, the `self` parameter is essentially a marker.

---

## Fix 4: Detect Channel Disconnect

### Problem

In single-threaded mode, if command channel is never empty, `Disconnected` is never detected.

### Solution

Add periodic disconnect check:

**Add to `io_loop`**:
```rust
pub fn io_loop(
    mut can: impl CanAdapter,
    cmd_rx: Receiver<PiperFrame>,
    ctx: Arc<PiperContext>,
    config: PipelineConfig,
) {
    // ... initialize ...

    let disconnect_check_interval = Duration::from_secs(1);
    let mut last_check = Instant::now();

    loop {
        // ... existing frame processing ...

        // Periodic disconnect check
        if last_check.elapsed() > disconnect_check_interval {
            if cmd_rx.is_empty() && cmd_rx.is_disconnected() {
                info!("Command channel disconnected and empty, exiting IO loop");
                break;
            }
            last_check = Instant::now();
        }

        // ... rest of loop ...
    }
}
```

---

## Fix 5: Add Connection Heartbeat (MONOTONIC TIME FIX)

### Problem

No way to detect if:
- Robot has powered off
- CAN cable unplugged
- Robot firmware crashed

### Previous Implementation (HAD BUGS)

The previous implementation mixed `Instant` and `UNIX_EPOCH`:

```rust
// ❌ BUG: Instant cannot directly calculate duration_since(UNIX_EPOCH)
Instant::now()
    .duration_since(std::time::UNIX_EPOCH)  // Won't compile!
    .unwrap()
    .as_micros() as u64

// ❌ BUG: Used SystemTime (not monotonic)
// SystemTime can jump backward due to NTP or manual adjustments
// This causes false "timeout" or "connection alive" detections
```

**Problems**:
1. `Instant::now()` doesn't have `duration_since(UNIX_EPOCH)` - that's `SystemTime` only
2. `SystemTime` is **not monotonic** - affected by clock changes
3. Complex conversions with `checked_sub` are error-prone

### Solution: App Start Relative Time Pattern

**Use monotonic time anchored to app start**:

**Create `src/driver/heartbeat.rs`**:
```rust
use std::sync::OnceLock;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Global anchor point for monotonic time
/// Set once on first access, never changes
static APP_START: OnceLock<Instant> = OnceLock::new();

/// Get monotonic time as microseconds since app start
///
/// This is guaranteed to be:
/// - Monotonic (always increases)
/// - Unaffected by system clock changes
/// - Safe to store in AtomicU64
fn get_monotonic_micros() -> u64 {
    let start = APP_START.get_or_init(|| Instant::now());
    start.elapsed().as_micros() as u64
}

pub struct ConnectionMonitor {
    last_feedback: Arc<AtomicU64>,
    timeout: Duration,
}

impl ConnectionMonitor {
    pub fn new(timeout: Duration) -> Self {
        // Initialize with current time (app start relative)
        let now = get_monotonic_micros();
        Self {
            last_feedback: Arc::new(AtomicU64::new(now)),
            timeout,
        }
    }

    /// Check if connection is still alive
    ///
    /// Returns true if feedback received within timeout window
    pub fn check_connection(&self) -> bool {
        let last_us = self.last_feedback.load(Ordering::Relaxed);
        let now_us = get_monotonic_micros();

        // Safe subtraction: now_us is always >= last_us (monotonic)
        let elapsed_us = now_us.saturating_sub(last_us);
        let elapsed = Duration::from_micros(elapsed_us);

        elapsed < self.timeout
    }

    /// Register that we received feedback from the robot
    ///
    /// Call this after processing each CAN frame
    pub fn register_feedback(&self) {
        let now = get_monotonic_micros();
        self.last_feedback.store(now, Ordering::Relaxed);
    }

    /// Get time since last feedback
    pub fn time_since_last_feedback(&self) -> Duration {
        let last_us = self.last_feedback.load(Ordering::Relaxed);
        let now_us = get_monotonic_micros();
        Duration::from_micros(now_us.saturating_sub(last_us))
    }
}
```

### Why This Works

**App Start Relative Time**:
```
App startup (t=0):          APP_START = Instant::now()
After 1 hour:               get_monotonic_micros() = 3,600,000,000 us
After system clock change:  get_monotonic_micros() = 3,600,100,000 us (still monotonic!)
```

**Comparison**:

| Approach | Monotonic? | Affected by NTP? | Complexity |
|----------|-----------|------------------|------------|
| `SystemTime::now()` | ❌ No | ✅ Yes | Simple but wrong |
| `Instant + UNIX_EPOCH` | ✅ Yes | ❌ No | ❌ Doesn't compile |
| App Start Relative | ✅ Yes | ❌ No | Simple and correct ✅ |

### Integration

**Add to `PiperContext`**:
```rust
pub struct PiperContext {
    // ... existing fields ...
    pub connection_monitor: ConnectionMonitor,
}

// In io_loop, after processing each frame:
ctx.connection_monitor.register_feedback();
```

**Add to `Observer`**:
```rust
impl Observer {
    /// Check if robot is still connected
    ///
    /// Returns true if feedback received within timeout window
    pub fn is_connected(&self) -> bool {
        self.ctx.connection_monitor.check_connection()
    }

    /// Get time since last feedback
    pub fn connection_age(&self) -> Duration {
        self.ctx.connection_monitor.time_since_last_feedback()
    }
}
```

### Benefits

1. **Monotonic**: Guaranteed to always increase, never affected by clock changes
2. **Simple**: No complex conversions or `checked_sub` logic
3. **Safe**: Safe to store in `AtomicU64` for lock-free access
4. **Efficient**: Simple integer subtraction for age calculation

---

## Implementation Checklist

- [ ] **Fix 1**: Add thread join timeout (OPTIMIZED)
  - [ ] Implement `JoinTimeout` trait using `mpsc` channel
  - [ ] Use `recv_timeout` for true blocking wait (no polling)
  - [ ] Update `Drop` impl
  - [ ] Add tests

- [ ] **Fix 2**: Add graceful shutdown (CONFIRMED)
  - [ ] Implement `shutdown()` method
  - [ ] Add `stop_motion()` to RawCommander
  - [ ] Keep 100ms sleep with explanatory comment (INTENTIONAL HARD WAIT)
  - [ ] Update Drop impl
  - [ ] Add tests

- [ ] **Fix 3**: Add reconnect mechanism
  - [ ] Implement `reconnect()` for `Disconnected`
  - [ ] Add documentation
  - [ ] Add tests

- [ ] **Fix 4**: Add disconnect detection
  - [ ] Add periodic check to io_loop
  - [ ] Add tests

- [ ] **Fix 5**: Add heartbeat monitoring (MONOTONIC TIME FIX)
  - [ ] Implement App Start Relative Time pattern with `OnceLock`
  - [ ] Create `ConnectionMonitor` with `get_monotonic_micros()`
  - [ ] Integrate into pipeline
  - [ ] Add API to Observer
  - [ ] Add tests for monotonic behavior

---

## Testing

### Unit Tests

```rust
#[test]
fn test_thread_join_timeout_no_polling() {
    // Create thread that sleeps forever
    // Verify join_timeout returns Err after timeout
    // Verify no busy waiting occurred (should be fast)
}

#[test]
fn test_graceful_shutdown_sequence() {
    // Mock driver that tracks commands
    // Verify shutdown sends stop + disable
    // Verify 100ms wait occurs (intentional)
}

#[test]
fn test_reconnect_creates_new_driver() {
    // Verify reconnect creates new driver instance
}

#[test]
fn test_heartbeat_monotonic_time() {
    // Create ConnectionMonitor
    // Register feedback
    // Verify check_connection() works correctly
    // Verify time_since_last_feedback() is monotonic
}

#[test]
fn test_heartbeat_survives_clock_jump() {
    // This test verifies monotonic behavior
    // In real scenario, NTP jumps won't affect the monitor
    // (difficult to test automatically, but document the behavior)
}
```

### Integration Tests

```rust
#[test]
#[ignore]
fn test_shutdown_with_hardware() {
    // Connect to real robot
    // Enable motors
    // Call shutdown()
    // Verify motors are disabled
}

#[test]
#[ignore]
fn test_can_disconnect_detection() {
    // Start robot
    // Unplug CAN cable
    // Verify is_connected() returns false within timeout
    // Verify detection survives system clock changes
}
```

---

## Rollback Plan

If thread join timeout causes issues:

1. **Make timeout configurable**:
```rust
pub struct Piper {
    join_timeout: Duration,
    // ...
}

impl Piper {
    pub fn set_join_timeout(&mut self, timeout: Duration) {
        self.join_timeout = timeout;
    }
}
```

2. **Add feature flag**:
```toml
[features]
default = ["thread_timeout"]
thread_timeout = []
```

---

## References

- [Rust Thread Join](https://doc.rust-lang.org/std/thread/struct.JoinHandle.html)
- [Graceful Shutdown Patterns](https://blog.yoshuawuyts.com/shutdown/)
- [OnceLock Documentation](https://doc.rust-lang.org/std/sync/struct.OnceLock.html)

---

**Changelog**:
- 2025-01-25: Initial version (with technical issues)
- 2025-01-25: **REVISED** - Fixed critical issues based on expert review
  - Fix 1: Optimized to use `mpsc` channel for true blocking wait (no polling overhead)
  - Fix 2: Confirmed sleep is appropriate - added explanatory comment about intentional hard wait
  - Fix 5: Fixed monotonic time - use App Start Relative Time pattern with OnceLock

---

**Next Steps**: After implementing all fixes, review the implementation summary in `00_implementation_summary.md`
