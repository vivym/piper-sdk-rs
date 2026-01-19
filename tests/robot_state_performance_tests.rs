//! 状态结构性能基准测试
//!
//! 测试新状态结构的性能指标，包括：
//! - 内存占用对比（位掩码优化效果）
//! - 读取延迟（ArcSwap vs RwLock）
//! - 写入延迟（ArcSwap vs RwLock）

use piper_sdk::robot::*;
use std::sync::Arc;
use std::time::Instant;

/// 测试结构体大小（位掩码优化效果）
#[test]
fn test_state_struct_sizes() {
    use std::mem::size_of;

    // 测试位掩码优化效果
    println!("\n=== 结构体大小对比 ===");

    // JointPositionState
    let joint_pos_size = size_of::<JointPositionState>();
    println!("JointPositionState: {} bytes", joint_pos_size);

    // EndPoseState
    let end_pose_size = size_of::<EndPoseState>();
    println!("EndPoseState: {} bytes", end_pose_size);

    // GripperState
    let gripper_size = size_of::<GripperState>();
    println!("GripperState: {} bytes", gripper_size);

    // RobotControlState（位掩码优化）
    let robot_control_size = size_of::<RobotControlState>();
    println!("RobotControlState: {} bytes", robot_control_size);

    // JointDriverLowSpeedState（位掩码优化）
    let joint_driver_size = size_of::<JointDriverLowSpeedState>();
    println!("JointDriverLowSpeedState: {} bytes", joint_driver_size);

    // CollisionProtectionState
    let collision_protection_size = size_of::<CollisionProtectionState>();
    println!(
        "CollisionProtectionState: {} bytes",
        collision_protection_size
    );

    // JointLimitConfigState
    let joint_limit_config_size = size_of::<JointLimitConfigState>();
    println!("JointLimitConfigState: {} bytes", joint_limit_config_size);

    // JointAccelConfigState
    let joint_accel_config_size = size_of::<JointAccelConfigState>();
    println!("JointAccelConfigState: {} bytes", joint_accel_config_size);

    // EndLimitConfigState
    let end_limit_config_size = size_of::<EndLimitConfigState>();
    println!("EndLimitConfigState: {} bytes", end_limit_config_size);

    // 验证位掩码优化效果
    // RobotControlState: fault_angle_limit_mask 和 fault_comm_error_mask 是 u8 (1 byte each)
    // 如果使用 [bool; 6]，每个需要 6 bytes，总共 12 bytes
    // 优化后只需要 2 bytes，节省 10 bytes
    assert!(
        robot_control_size < 200,
        "RobotControlState 应该小于 200 bytes"
    );

    // JointDriverLowSpeedState: 8 个位掩码字段，每个 u8 (1 byte) = 8 bytes
    // 如果使用 [bool; 6]，每个需要 6 bytes，总共 48 bytes
    // 优化后只需要 8 bytes，节省 40 bytes
    assert!(
        joint_driver_size < 300,
        "JointDriverLowSpeedState 应该小于 300 bytes"
    );
}

/// 测试 ArcSwap 读取延迟
#[test]
fn test_arcswap_read_latency() {
    let ctx = Arc::new(PiperContext::new());
    let iterations = 1_000_000;

    // 预热
    for _ in 0..1000 {
        let _ = ctx.joint_position.load();
    }

    // 测试读取延迟
    let start = Instant::now();
    for _ in 0..iterations {
        let _ = ctx.joint_position.load();
    }
    let elapsed = start.elapsed();

    let avg_ns = elapsed.as_nanos() / iterations as u128;
    println!("\n=== ArcSwap 读取延迟 ===");
    println!("迭代次数: {}", iterations);
    println!("总耗时: {:?}", elapsed);
    println!("平均延迟: {} ns", avg_ns);

    // ArcSwap::load() 应该是纳秒级的（通常 < 100ns）
    assert!(avg_ns < 1000, "ArcSwap 读取延迟应该小于 1000ns");
}

/// 测试 ArcSwap 写入延迟
#[test]
fn test_arcswap_write_latency() {
    let ctx = Arc::new(PiperContext::new());
    let iterations = 100_000;

    // 预热
    let initial_state = JointPositionState {
        hardware_timestamp_us: 0,
        system_timestamp_us: 0,
        joint_pos: [0.0; 6],
        frame_valid_mask: 0b111,
    };
    for _ in 0..1000 {
        ctx.joint_position.store(Arc::new(initial_state.clone()));
    }

    // 测试写入延迟
    let start = Instant::now();
    for i in 0..iterations {
        let new_state = JointPositionState {
            hardware_timestamp_us: i as u64,
            system_timestamp_us: i as u64 * 2,
            joint_pos: [i as f64; 6],
            frame_valid_mask: 0b111,
        };
        ctx.joint_position.store(Arc::new(new_state));
    }
    let elapsed = start.elapsed();

    let avg_ns = elapsed.as_nanos() / iterations as u128;
    println!("\n=== ArcSwap 写入延迟 ===");
    println!("迭代次数: {}", iterations);
    println!("总耗时: {:?}", elapsed);
    println!("平均延迟: {} ns", avg_ns);

    // ArcSwap::store() 应该是纳秒级的（通常 < 200ns）
    assert!(avg_ns < 2000, "ArcSwap 写入延迟应该小于 2000ns");
}

/// 测试 RwLock 读取延迟（对比）
#[test]
fn test_rwlock_read_latency() {
    let ctx = Arc::new(PiperContext::new());
    let iterations = 1_000_000;

    // 预热
    for _ in 0..1000 {
        drop(ctx.collision_protection.read());
    }

    // 测试读取延迟
    let start = Instant::now();
    for _ in 0..iterations {
        drop(ctx.collision_protection.read());
    }
    let elapsed = start.elapsed();

    let avg_ns = elapsed.as_nanos() / iterations as u128;
    println!("\n=== RwLock 读取延迟 ===");
    println!("迭代次数: {}", iterations);
    println!("总耗时: {:?}", elapsed);
    println!("平均延迟: {} ns", avg_ns);

    // RwLock::read() 通常比 ArcSwap::load() 稍慢，但应该仍然很快
    assert!(avg_ns < 2000, "RwLock 读取延迟应该小于 2000ns");
}

/// 测试 RwLock 写入延迟（对比）
#[test]
fn test_rwlock_write_latency() {
    let ctx = Arc::new(PiperContext::new());
    let iterations = 100_000;

    // 预热
    for _ in 0..1000 {
        if let Ok(mut state) = ctx.collision_protection.write() {
            state.hardware_timestamp_us = 0;
        }
    }

    // 测试写入延迟
    let start = Instant::now();
    for i in 0..iterations {
        if let Ok(mut state) = ctx.collision_protection.write() {
            state.hardware_timestamp_us = i as u64;
            state.system_timestamp_us = i as u64 * 2;
            state.protection_levels = [(i % 9) as u8; 6];
        }
    }
    let elapsed = start.elapsed();

    let avg_ns = elapsed.as_nanos() / iterations as u128;
    println!("\n=== RwLock 写入延迟 ===");
    println!("迭代次数: {}", iterations);
    println!("总耗时: {:?}", elapsed);
    println!("平均延迟: {} ns", avg_ns);

    // RwLock::write() 通常比 ArcSwap::store() 稍慢
    assert!(avg_ns < 5000, "RwLock 写入延迟应该小于 5000ns");
}

/// 测试 capture_motion_snapshot() 延迟
#[test]
fn test_capture_motion_snapshot_latency() {
    let ctx = Arc::new(PiperContext::new());
    let iterations = 1_000_000;

    // 预热
    for _ in 0..1000 {
        let _ = ctx.capture_motion_snapshot();
    }

    // 测试延迟
    let start = Instant::now();
    for _ in 0..iterations {
        let _ = ctx.capture_motion_snapshot();
    }
    let elapsed = start.elapsed();

    let avg_ns = elapsed.as_nanos() / iterations as u128;
    println!("\n=== capture_motion_snapshot() 延迟 ===");
    println!("迭代次数: {}", iterations);
    println!("总耗时: {:?}", elapsed);
    println!("平均延迟: {} ns", avg_ns);

    // capture_motion_snapshot() 需要读取两个 ArcSwap，应该仍然很快
    assert!(
        avg_ns < 2000,
        "capture_motion_snapshot() 延迟应该小于 2000ns"
    );
}

/// 测试位掩码访问方法的性能
#[test]
fn test_bitmask_access_performance() {
    let state = RobotControlState {
        hardware_timestamp_us: 0,
        system_timestamp_us: 0,
        control_mode: 0,
        robot_status: 0,
        move_mode: 0,
        teach_status: 0,
        motion_status: 0,
        trajectory_point_index: 0,
        fault_angle_limit_mask: 0b0011_0001, // J1, J5, J6
        fault_comm_error_mask: 0b0000_0010,  // J2
        is_enabled: true,
        feedback_counter: 0,
    };

    let iterations = 10_000_000;

    // 预热
    for _ in 0..1000 {
        let _ = state.is_angle_limit(0);
        let _ = state.is_comm_error(1);
    }

    // 测试位掩码访问性能
    let start = Instant::now();
    for _ in 0..iterations {
        let _ = state.is_angle_limit(0);
        let _ = state.is_angle_limit(1);
        let _ = state.is_angle_limit(2);
        let _ = state.is_angle_limit(3);
        let _ = state.is_angle_limit(4);
        let _ = state.is_angle_limit(5);
        let _ = state.is_comm_error(0);
        let _ = state.is_comm_error(1);
        let _ = state.is_comm_error(2);
        let _ = state.is_comm_error(3);
        let _ = state.is_comm_error(4);
        let _ = state.is_comm_error(5);
    }
    let elapsed = start.elapsed();

    let avg_ns = elapsed.as_nanos() / (iterations * 12) as u128;
    println!("\n=== 位掩码访问方法性能 ===");
    println!("迭代次数: {} (每个关节访问 12 次)", iterations);
    println!("总耗时: {:?}", elapsed);
    println!("平均延迟: {} ns (每次访问)", avg_ns);

    // 位掩码访问应该是非常快的（通常 < 10ns）
    assert!(avg_ns < 100, "位掩码访问延迟应该小于 100ns");
}

/// 测试状态克隆性能
#[test]
fn test_state_clone_performance() {
    let joint_pos = JointPositionState {
        hardware_timestamp_us: 1234567890,
        system_timestamp_us: 1234567890 * 2,
        joint_pos: [1.0, 2.0, 3.0, 4.0, 5.0, 6.0],
        frame_valid_mask: 0b111,
    };

    let iterations = 1_000_000;

    // 预热
    for _ in 0..1000 {
        let _ = joint_pos.clone();
    }

    // 测试克隆性能
    let start = Instant::now();
    for _ in 0..iterations {
        let _ = joint_pos.clone();
    }
    let elapsed = start.elapsed();

    let avg_ns = elapsed.as_nanos() / iterations as u128;
    println!("\n=== 状态克隆性能 ===");
    println!("迭代次数: {}", iterations);
    println!("总耗时: {:?}", elapsed);
    println!("平均延迟: {} ns", avg_ns);

    // 克隆应该很快（通常 < 100ns）
    assert!(avg_ns < 500, "状态克隆延迟应该小于 500ns");
}

/// 测试多个状态同时读取的性能
#[test]
fn test_multiple_states_read_performance() {
    let ctx = Arc::new(PiperContext::new());
    let iterations = 100_000;

    // 预热
    for _ in 0..1000 {
        let _ = ctx.joint_position.load();
        let _ = ctx.robot_control.load();
        let _ = ctx.gripper.load();
        let _ = ctx.joint_driver_low_speed.load();
    }

    // 测试多个状态同时读取的性能
    let start = Instant::now();
    for _ in 0..iterations {
        let _ = ctx.joint_position.load();
        let _ = ctx.robot_control.load();
        let _ = ctx.gripper.load();
        let _ = ctx.joint_driver_low_speed.load();
    }
    let elapsed = start.elapsed();

    let avg_ns = elapsed.as_nanos() / (iterations * 4) as u128;
    println!("\n=== 多个状态同时读取性能 ===");
    println!("迭代次数: {} (每次读取 4 个状态)", iterations);
    println!("总耗时: {:?}", elapsed);
    println!("平均延迟: {} ns (每个状态)", avg_ns);

    // 每个状态读取应该仍然很快
    assert!(avg_ns < 1000, "每个状态读取延迟应该小于 1000ns");
}
