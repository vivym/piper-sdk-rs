//! Robot 性能测试
//!
//! 测试高频读取性能（500Hz 控制循环）

use piper_sdk::can::{CanAdapter, CanError, PiperFrame};
use piper_sdk::driver::*;
use std::time::Instant;

// Mock CanAdapter 用于性能测试
struct MockCanAdapter;

impl CanAdapter for MockCanAdapter {
    fn send(&mut self, _frame: PiperFrame) -> Result<(), CanError> {
        Ok(())
    }

    fn receive(&mut self) -> Result<PiperFrame, CanError> {
        Err(CanError::Timeout)
    }
}

/// 测试高频读取性能（500Hz）
///
/// 验证能够达到至少 450 Hz（允许 10% 误差）
#[test]
fn test_high_frequency_read_performance() {
    let mock_can = MockCanAdapter;
    let piper = Piper::new(mock_can, None).unwrap();

    let start = Instant::now();
    let mut count = 0;

    // 运行 1 秒，目标是 500Hz
    while start.elapsed().as_millis() < 1000 {
        let _state = piper.get_motion_state();
        count += 1;
    }

    let elapsed = start.elapsed();
    let hz = count as f64 / elapsed.as_secs_f64();

    println!(
        "High frequency read: {} calls in {:?} ({:.1} Hz)",
        count, elapsed, hz
    );

    // 验证：能够达到至少 450 Hz（允许 10% 误差）
    assert!(hz >= 450.0, "Failed to achieve 450 Hz: {:.1} Hz", hz);

    // 验证：单次读取延迟合理（应该远小于 2ms）
    let avg_latency_us = elapsed.as_micros() as f64 / count as f64;
    assert!(
        avg_latency_us < 2500.0,
        "Average latency too high: {:.1} μs",
        avg_latency_us
    );
}

/// 测试无锁读取性能（ArcSwap）
#[test]
fn test_lock_free_read_performance() {
    let mock_can = MockCanAdapter;
    let piper = Piper::new(mock_can, None).unwrap();

    let start = Instant::now();
    let mut count = 0;

    // 运行 100ms，测量高频读取性能
    while start.elapsed().as_millis() < 100 {
        let _joint_pos = piper.get_joint_position();
        let _end_pose = piper.get_end_pose();
        let _joint = piper.get_joint_dynamic();
        let _control = piper.get_robot_control();
        let _gripper = piper.get_gripper();
        count += 1;
    }

    let elapsed = start.elapsed();
    let ops_per_sec = (count as f64) / elapsed.as_secs_f64();

    println!(
        "Lock-free read: {} operations in {:?} ({:.0} ops/s)",
        count * 3,
        elapsed,
        ops_per_sec * 3.0
    );

    // 验证：无锁读取应该非常快（至少 10K ops/s）
    assert!(
        ops_per_sec >= 10000.0,
        "Lock-free read too slow: {:.0} ops/s",
        ops_per_sec
    );
}
