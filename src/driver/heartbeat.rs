//! Connection Monitor - Monitors incoming feedback to detect connection aliveness
//!
//! **Purpose**: Detect if the robot is still responding (powered on, CAN cable connected).
//!
//! **App Start Relative Time Pattern**:
//! - Uses monotonic time anchored to application start
//! - Unaffected by system clock changes (NTP, manual adjustments)
//! - Safe to store in AtomicU64 for lock-free access

use std::sync::OnceLock;
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
    let start = APP_START.get_or_init(Instant::now);
    start.elapsed().as_micros() as u64
}

/// Connection health monitor
///
/// Tracks the time since last feedback was received from the robot.
pub struct ConnectionMonitor {
    last_feedback: AtomicU64,
    timeout: Duration,
}

impl ConnectionMonitor {
    /// Create a new connection monitor
    ///
    /// # Parameters
    /// - `timeout`: Maximum duration without feedback before considering connection lost
    ///
    /// # Example
    /// ```
    /// # use piper_sdk::driver::heartbeat::ConnectionMonitor;
    /// # use std::time::Duration;
    /// let monitor = ConnectionMonitor::new(Duration::from_secs(1));
    /// ```
    pub fn new(timeout: Duration) -> Self {
        // Initialize with current time (app start relative)
        let now = get_monotonic_micros();
        Self {
            last_feedback: AtomicU64::new(now),
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
    /// Call this after processing each CAN frame to update the last feedback time.
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn test_monotonic_time_always_increases() {
        let t1 = get_monotonic_micros();
        thread::sleep(Duration::from_millis(10));
        let t2 = get_monotonic_micros();

        assert!(t2 > t1, "Monotonic time should always increase");
    }

    #[test]
    fn test_connection_monitor_initially_alive() {
        let monitor = ConnectionMonitor::new(Duration::from_secs(1));
        assert!(
            monitor.check_connection(),
            "Connection should be alive initially"
        );
    }

    #[test]
    fn test_connection_monitor_timeout_after_delay() {
        let monitor = ConnectionMonitor::new(Duration::from_millis(50));

        // Initially alive
        assert!(monitor.check_connection());

        // Wait for timeout
        thread::sleep(Duration::from_millis(100));

        // Should be timed out
        assert!(
            !monitor.check_connection(),
            "Connection should timeout after delay"
        );
    }

    #[test]
    fn test_connection_monitor_feedback_resets_timer() {
        let monitor = ConnectionMonitor::new(Duration::from_millis(100));

        // Wait half of timeout
        thread::sleep(Duration::from_millis(50));

        // Register feedback (resets timer)
        monitor.register_feedback();

        // Wait another 50ms (total 100ms from start)
        thread::sleep(Duration::from_millis(50));

        // Should still be alive because timer was reset
        assert!(
            monitor.check_connection(),
            "Feedback should reset timeout timer"
        );
    }

    #[test]
    fn test_time_since_last_feedback() {
        let monitor = ConnectionMonitor::new(Duration::from_secs(1));

        thread::sleep(Duration::from_millis(10));
        let elapsed = monitor.time_since_last_feedback();

        assert!(elapsed >= Duration::from_millis(10));
        assert!(elapsed < Duration::from_millis(50)); // Should be close to 10ms
    }

    #[test]
    fn test_monotonic_micros_no_panic_on_system_clock_change() {
        // This test verifies that get_monotonic_micros doesn't panic
        // and continues to work correctly (monotonically increasing)
        // even if system clock changes (we can't actually test NTP changes,
        // but we verify the function doesn't panic and returns increasing values)

        let mut last = get_monotonic_micros();

        for _ in 0..100 {
            thread::sleep(Duration::from_micros(100));
            let current = get_monotonic_micros();
            assert!(
                current >= last,
                "Monotonic time should never decrease (current={}, last={})",
                current,
                last
            );
            last = current;
        }
    }
}
