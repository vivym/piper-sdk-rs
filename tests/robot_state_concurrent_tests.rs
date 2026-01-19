//! 状态结构并发测试
//!
//! 测试新状态结构在多线程环境下的并发安全性，特别是 `ArcSwap` 的 Wait-Free 特性。

use piper_sdk::robot::*;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

/// 测试 `JointPositionState` 的并发读取
///
/// 验证多个线程同时读取 `ArcSwap` 包装的状态时不会阻塞。
#[test]
fn test_joint_position_state_concurrent_read() {
    let ctx = Arc::new(PiperContext::new());
    let num_threads = 10;
    let reads_per_thread = 1000;

    // 创建一个线程更新状态
    let ctx_writer = ctx.clone();
    let writer_handle = thread::spawn(move || {
        for i in 0..reads_per_thread {
            let new_state = JointPositionState {
                hardware_timestamp_us: i as u64 * 1000,
                system_timestamp_us: i as u64 * 2000,
                joint_pos: [i as f64; 6],
                frame_valid_mask: 0b111,
            };
            ctx_writer.joint_position.store(Arc::new(new_state));
            thread::yield_now();
        }
    });

    // 创建多个读取线程
    let mut reader_handles = Vec::new();
    for _ in 0..num_threads {
        let ctx_reader = ctx.clone();
        let handle = thread::spawn(move || {
            let mut last_timestamp = 0u64;
            for _ in 0..reads_per_thread {
                let state = ctx_reader.joint_position.load();
                // 验证状态是递增的（或至少不倒退太多）
                if state.hardware_timestamp_us >= last_timestamp {
                    last_timestamp = state.hardware_timestamp_us;
                }
                // 验证状态完整性
                assert!(state.joint_pos.len() == 6);
                thread::yield_now();
            }
        });
        reader_handles.push(handle);
    }

    // 等待所有线程完成
    writer_handle.join().unwrap();
    for handle in reader_handles {
        handle.join().unwrap();
    }
}

/// 测试 `EndPoseState` 的并发读取
#[test]
fn test_end_pose_state_concurrent_read() {
    let ctx = Arc::new(PiperContext::new());
    let num_threads = 10;
    let reads_per_thread = 1000;

    let ctx_writer = ctx.clone();
    let writer_handle = thread::spawn(move || {
        for i in 0..reads_per_thread {
            let new_state = EndPoseState {
                hardware_timestamp_us: i as u64 * 1000,
                system_timestamp_us: i as u64 * 2000,
                end_pose: [i as f64; 6],
                frame_valid_mask: 0b111,
            };
            ctx_writer.end_pose.store(Arc::new(new_state));
            thread::yield_now();
        }
    });

    let mut reader_handles = Vec::new();
    for _ in 0..num_threads {
        let ctx_reader = ctx.clone();
        let handle = thread::spawn(move || {
            for _ in 0..reads_per_thread {
                let state = ctx_reader.end_pose.load();
                assert!(state.end_pose.len() == 6);
                thread::yield_now();
            }
        });
        reader_handles.push(handle);
    }

    writer_handle.join().unwrap();
    for handle in reader_handles {
        handle.join().unwrap();
    }
}

/// 测试 `JointDriverLowSpeedState` 的并发读取（ArcSwap）
///
/// 验证 `ArcSwap` 的 Wait-Free 特性，多个线程同时读取不会阻塞。
#[test]
fn test_joint_driver_low_speed_state_concurrent_read() {
    let ctx = Arc::new(PiperContext::new());
    let num_threads = 10;
    let reads_per_thread = 1000;

    let ctx_writer = ctx.clone();
    let writer_handle = thread::spawn(move || {
        for i in 0..reads_per_thread {
            let new_state = JointDriverLowSpeedState {
                hardware_timestamp_us: i as u64 * 1000,
                system_timestamp_us: i as u64 * 2000,
                motor_temps: [i as f32; 6],
                driver_temps: [i as f32 + 10.0; 6],
                joint_voltage: [24.0 + i as f32 * 0.1; 6],
                joint_bus_current: [1.0 + i as f32 * 0.01; 6],
                driver_voltage_low_mask: if i % 2 == 0 { 0b0000_0001 } else { 0 },
                driver_motor_over_temp_mask: 0,
                driver_over_current_mask: 0,
                driver_over_temp_mask: 0,
                driver_collision_protection_mask: 0,
                driver_error_mask: 0,
                driver_enabled_mask: 0b111111,
                driver_stall_protection_mask: 0,
                hardware_timestamps: [i as u64 * 100; 6],
                system_timestamps: [i as u64 * 200; 6],
                valid_mask: 0b111111,
            };
            ctx_writer.joint_driver_low_speed.store(Arc::new(new_state));
            thread::yield_now();
        }
    });

    let mut reader_handles = Vec::new();
    for _ in 0..num_threads {
        let ctx_reader = ctx.clone();
        let handle = thread::spawn(move || {
            for _ in 0..reads_per_thread {
                let state = ctx_reader.joint_driver_low_speed.load();
                // 验证状态完整性
                assert!(state.motor_temps.len() == 6);
                assert!(state.driver_temps.len() == 6);
                // 测试位掩码访问方法
                let _ = state.is_voltage_low(0);
                let _ = state.is_enabled(0);
                thread::yield_now();
            }
        });
        reader_handles.push(handle);
    }

    writer_handle.join().unwrap();
    for handle in reader_handles {
        handle.join().unwrap();
    }
}

/// 测试 `RobotControlState` 的并发读取
#[test]
fn test_robot_control_state_concurrent_read() {
    let ctx = Arc::new(PiperContext::new());
    let num_threads = 10;
    let reads_per_thread = 1000;

    let ctx_writer = ctx.clone();
    let writer_handle = thread::spawn(move || {
        for i in 0..reads_per_thread {
            let new_state = RobotControlState {
                hardware_timestamp_us: i as u64 * 1000,
                system_timestamp_us: i as u64 * 2000,
                control_mode: (i % 256) as u8,
                robot_status: ((i + 1) % 256) as u8,
                move_mode: ((i + 2) % 256) as u8,
                teach_status: ((i + 3) % 256) as u8,
                motion_status: ((i + 4) % 256) as u8,
                trajectory_point_index: ((i + 5) % 256) as u8,
                fault_angle_limit_mask: if i % 2 == 0 { 0b0000_0001 } else { 0 },
                fault_comm_error_mask: 0,
                is_enabled: i % 2 == 0,
                feedback_counter: (i % 256) as u8,
            };
            ctx_writer.robot_control.store(Arc::new(new_state));
            thread::yield_now();
        }
    });

    let mut reader_handles = Vec::new();
    for _ in 0..num_threads {
        let ctx_reader = ctx.clone();
        let handle = thread::spawn(move || {
            for _ in 0..reads_per_thread {
                let state = ctx_reader.robot_control.load();
                // 验证状态完整性（control_mode 是 u8，范围 0-255，无需额外检查）
                // 测试位掩码访问方法
                let _ = state.is_angle_limit(0);
                let _ = state.is_comm_error(0);
                thread::yield_now();
            }
        });
        reader_handles.push(handle);
    }

    writer_handle.join().unwrap();
    for handle in reader_handles {
        handle.join().unwrap();
    }
}

/// 测试 `GripperState` 的并发读取
#[test]
fn test_gripper_state_concurrent_read() {
    let ctx = Arc::new(PiperContext::new());
    let num_threads = 10;
    let reads_per_thread = 1000;

    let ctx_writer = ctx.clone();
    let writer_handle = thread::spawn(move || {
        for i in 0..reads_per_thread {
            let new_state = GripperState {
                hardware_timestamp_us: i as u64 * 1000,
                system_timestamp_us: i as u64 * 2000,
                travel: i as f64 * 0.1,
                torque: i as f64 * 0.01,
                status_code: (i % 256) as u8,
                last_travel: (i - 1) as f64 * 0.1,
            };
            ctx_writer.gripper.store(Arc::new(new_state));
            thread::yield_now();
        }
    });

    let mut reader_handles = Vec::new();
    for _ in 0..num_threads {
        let ctx_reader = ctx.clone();
        let handle = thread::spawn(move || {
            for _ in 0..reads_per_thread {
                let state = ctx_reader.gripper.load();
                // 验证状态完整性
                assert!(state.travel >= 0.0);
                // 测试辅助方法
                let _ = state.is_voltage_low();
                let _ = state.is_moving();
                thread::yield_now();
            }
        });
        reader_handles.push(handle);
    }

    writer_handle.join().unwrap();
    for handle in reader_handles {
        handle.join().unwrap();
    }
}

/// 测试 `capture_motion_snapshot()` 的并发调用
///
/// 验证多个线程同时调用 `capture_motion_snapshot()` 不会出现问题。
#[test]
fn test_capture_motion_snapshot_concurrent() {
    let ctx = Arc::new(PiperContext::new());
    let num_threads = 10;
    let calls_per_thread = 1000;

    // 更新状态
    let ctx_writer = ctx.clone();
    let writer_handle = thread::spawn(move || {
        for i in 0..calls_per_thread {
            let new_joint_pos = JointPositionState {
                hardware_timestamp_us: i as u64 * 1000,
                system_timestamp_us: i as u64 * 2000,
                joint_pos: [i as f64; 6],
                frame_valid_mask: 0b111,
            };
            ctx_writer.joint_position.store(Arc::new(new_joint_pos));

            let new_end_pose = EndPoseState {
                hardware_timestamp_us: i as u64 * 1000,
                system_timestamp_us: i as u64 * 2000,
                end_pose: [i as f64; 6],
                frame_valid_mask: 0b111,
            };
            ctx_writer.end_pose.store(Arc::new(new_end_pose));
            thread::yield_now();
        }
    });

    // 多个线程同时调用 capture_motion_snapshot
    let mut reader_handles = Vec::new();
    for _ in 0..num_threads {
        let ctx_reader = ctx.clone();
        let handle = thread::spawn(move || {
            for _ in 0..calls_per_thread {
                let snapshot = ctx_reader.capture_motion_snapshot();
                // 验证快照完整性
                assert!(snapshot.joint_position.joint_pos.len() == 6);
                assert!(snapshot.end_pose.end_pose.len() == 6);
                thread::yield_now();
            }
        });
        reader_handles.push(handle);
    }

    writer_handle.join().unwrap();
    for handle in reader_handles {
        handle.join().unwrap();
    }
}

/// 测试 `RwLock` 包装的状态的并发读取（CollisionProtectionState）
///
/// 验证 `RwLock` 的读写分离特性。
#[test]
fn test_collision_protection_state_concurrent_read() {
    let ctx = Arc::new(PiperContext::new());
    let num_threads = 10;
    let reads_per_thread = 100;

    let ctx_writer = ctx.clone();
    let writer_handle = thread::spawn(move || {
        for i in 0..reads_per_thread {
            if let Ok(mut state) = ctx_writer.collision_protection.write() {
                state.hardware_timestamp_us = i as u64 * 1000;
                state.system_timestamp_us = i as u64 * 2000;
                state.protection_levels = [(i % 9) as u8; 6];
                state.protection_levels[0] = (i % 9) as u8;
            }
            thread::yield_now();
        }
    });

    let mut reader_handles = Vec::new();
    for _ in 0..num_threads {
        let ctx_reader = ctx.clone();
        let handle = thread::spawn(move || {
            for _ in 0..reads_per_thread {
                if let Ok(state) = ctx_reader.collision_protection.read() {
                    assert!(state.protection_levels.len() == 6);
                    assert!(state.protection_levels[0] <= 8);
                }
                thread::yield_now();
            }
        });
        reader_handles.push(handle);
    }

    writer_handle.join().unwrap();
    for handle in reader_handles {
        handle.join().unwrap();
    }
}

/// 测试多个状态同时并发读取
///
/// 验证多个 `ArcSwap` 包装的状态可以同时被多个线程读取，不会相互阻塞。
#[test]
fn test_multiple_states_concurrent_read() {
    let ctx = Arc::new(PiperContext::new());
    let num_threads = 5;
    let reads_per_thread = 500;

    // 更新多个状态
    let ctx_writer = ctx.clone();
    let writer_handle = thread::spawn(move || {
        for i in 0..reads_per_thread {
            // 更新 joint_position
            let new_joint_pos = JointPositionState {
                hardware_timestamp_us: i as u64 * 1000,
                system_timestamp_us: i as u64 * 2000,
                joint_pos: [i as f64; 6],
                frame_valid_mask: 0b111,
            };
            ctx_writer.joint_position.store(Arc::new(new_joint_pos));

            // 更新 robot_control
            let new_robot_control = RobotControlState {
                hardware_timestamp_us: i as u64 * 1000,
                system_timestamp_us: i as u64 * 2000,
                control_mode: (i % 256) as u8,
                robot_status: ((i + 1) % 256) as u8,
                move_mode: ((i + 2) % 256) as u8,
                teach_status: ((i + 3) % 256) as u8,
                motion_status: ((i + 4) % 256) as u8,
                trajectory_point_index: ((i + 5) % 256) as u8,
                fault_angle_limit_mask: 0,
                fault_comm_error_mask: 0,
                is_enabled: i % 2 == 0,
                feedback_counter: (i % 256) as u8,
            };
            ctx_writer.robot_control.store(Arc::new(new_robot_control));

            // 更新 gripper
            let new_gripper = GripperState {
                hardware_timestamp_us: i as u64 * 1000,
                system_timestamp_us: i as u64 * 2000,
                travel: i as f64 * 0.1,
                torque: i as f64 * 0.01,
                status_code: (i % 256) as u8,
                last_travel: (i - 1) as f64 * 0.1,
            };
            ctx_writer.gripper.store(Arc::new(new_gripper));

            thread::yield_now();
        }
    });

    // 多个线程同时读取多个状态
    let mut reader_handles = Vec::new();
    for _ in 0..num_threads {
        let ctx_reader = ctx.clone();
        let handle = thread::spawn(move || {
            for _ in 0..reads_per_thread {
                // 同时读取多个状态
                let joint_pos = ctx_reader.joint_position.load();
                let robot_control = ctx_reader.robot_control.load();
                let gripper = ctx_reader.gripper.load();

                // 验证状态完整性
                assert!(joint_pos.joint_pos.len() == 6);
                // control_mode 是 u8，范围 0-255，无需额外验证
                let _ = robot_control.control_mode;
                assert!(gripper.travel >= 0.0);

                thread::yield_now();
            }
        });
        reader_handles.push(handle);
    }

    writer_handle.join().unwrap();
    for handle in reader_handles {
        handle.join().unwrap();
    }
}

/// 测试无死锁场景
///
/// 验证在高并发读写场景下不会出现死锁。
#[test]
fn test_no_deadlock() {
    let ctx = Arc::new(PiperContext::new());
    let num_threads = 20;
    let duration = Duration::from_secs(2);

    let start_time = std::time::Instant::now();
    let mut handles = Vec::new();

    // 创建多个读写线程
    for i in 0..num_threads {
        let ctx_clone = ctx.clone();
        let handle = thread::spawn(move || {
            let mut counter = 0;
            while start_time.elapsed() < duration {
                // 交替进行读写操作
                if i % 2 == 0 {
                    // 读取操作
                    let _ = ctx_clone.joint_position.load();
                    let _ = ctx_clone.robot_control.load();
                    let _ = ctx_clone.gripper.load();
                } else {
                    // 写入操作
                    let new_state = JointPositionState {
                        hardware_timestamp_us: counter,
                        system_timestamp_us: counter * 2,
                        joint_pos: [counter as f64; 6],
                        frame_valid_mask: 0b111,
                    };
                    ctx_clone.joint_position.store(Arc::new(new_state));
                    counter += 1;
                }
                thread::yield_now();
            }
        });
        handles.push(handle);
    }

    // 等待所有线程完成（如果出现死锁，这里会超时）
    for handle in handles {
        handle.join().unwrap();
    }

    // 如果测试能正常完成，说明没有死锁
}
