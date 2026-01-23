//! 测试基础设施验证
//!
//! 验证 Mock 硬件和测试辅助函数是否正常工作。

#[path = "high_level/common/mod.rs"]
mod common;

use std::time::Duration;

use common::helpers::{
    assert_array_eq, assert_float_eq, setup_enabled_test_environment, setup_test_environment,
    test_joint_positions, test_joint_velocities, wait_for_condition,
};
use common::mock_hardware::{MockArmState, MockCanBus, MockCanFrame};

#[test]
fn test_mock_can_bus_basic() {
    let bus = MockCanBus::new();

    // 发送帧
    let frame = MockCanFrame::new(0x01, vec![0x01, 0x02, 0x03]);
    assert!(bus.send_frame(frame).is_ok());

    // 检查队列
    assert_eq!(bus.tx_queue_len(), 1);
}

#[test]
fn test_mock_can_bus_timeout() {
    let bus = MockCanBus::new();
    bus.simulate_timeout(true);

    let frame = MockCanFrame::new(0x01, vec![0x01]);
    let result = bus.send_frame(frame);

    assert!(result.is_err());
    assert_eq!(result.unwrap_err(), "Timeout");
}

#[test]
fn test_hardware_state_updates() {
    let bus = MockCanBus::new();

    // 初始状态
    let state = bus.get_hardware_state();
    assert_eq!(state.arm_state, MockArmState::Disconnected);

    // 模拟状态变化
    bus.simulate_arm_state(MockArmState::Enabled);
    let state = bus.get_hardware_state();
    assert_eq!(state.arm_state, MockArmState::Enabled);

    // 模拟急停
    bus.simulate_emergency_stop();
    let state = bus.get_hardware_state();
    assert_eq!(state.arm_state, MockArmState::Error);
    assert!(state.emergency_stop);
}

#[test]
fn test_joint_position_control() {
    let bus = MockCanBus::new();

    // 设置关节位置
    for i in 0..6 {
        bus.set_joint_position(i, i as f64 * 0.5);
    }

    let state = bus.get_hardware_state();
    for i in 0..6 {
        assert_eq!(state.joint_positions[i], i as f64 * 0.5);
    }
}

#[test]
fn test_gripper_control() {
    let bus = MockCanBus::new();

    bus.set_gripper_state(0.05, 10.0, true);

    let state = bus.get_hardware_state();
    assert_eq!(state.gripper_position, 0.05);
    assert_eq!(state.gripper_effort, 10.0);
    assert!(state.gripper_enabled);
}

#[test]
fn test_feedback_frame_generation() {
    let bus = MockCanBus::new();

    // 设置状态
    bus.set_joint_position(0, 1.5);
    bus.set_gripper_state(0.05, 10.0, true);

    // 生成反馈帧
    bus.generate_feedback_frame();

    // 尝试接收反馈（应该在队列中）
    let result = bus.recv_frame(Duration::from_millis(100));
    assert!(result.is_ok());
}

#[test]
fn test_setup_helper() {
    let bus = setup_test_environment();
    let state = bus.get_hardware_state();
    assert_eq!(state.arm_state, MockArmState::Standby);
}

#[test]
fn test_setup_enabled_helper() {
    let bus = setup_enabled_test_environment();
    let state = bus.get_hardware_state();
    assert_eq!(state.arm_state, MockArmState::Enabled);
}

#[test]
fn test_wait_for_condition_helper() {
    let bus = setup_test_environment();

    // 启动一个线程来改变状态
    let bus_clone = bus.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(50));
        bus_clone.simulate_arm_state(MockArmState::Enabled);
    });

    // 等待状态变化
    let result = wait_for_condition(
        || {
            let state = bus.get_hardware_state();
            state.arm_state == MockArmState::Enabled
        },
        1000,
        10,
    );

    assert!(result.is_ok());
}

#[test]
fn test_joint_helpers() {
    let positions = test_joint_positions(1.0);
    let velocities = test_joint_velocities(0.5);

    assert_eq!(positions.len(), 6);
    assert_eq!(velocities.len(), 6);

    assert_eq!(positions[0], 1.0);
    assert_eq!(velocities[0], 0.5);
}

#[test]
fn test_float_comparison_helpers() {
    assert_float_eq(1.0, 1.0001, 0.001);

    let a = [1.0; 6];
    let b = [1.0001; 6];
    assert_array_eq(&a, &b, 0.001);
}

#[test]
fn test_queue_management() {
    let bus = MockCanBus::new();

    // 发送多个帧
    for i in 0..5 {
        let frame = MockCanFrame::new(i, vec![i as u8]);
        bus.send_frame(frame).unwrap();
    }

    assert_eq!(bus.tx_queue_len(), 5);

    // 清空队列
    bus.clear_queues();
    assert_eq!(bus.tx_queue_len(), 0);
}

#[test]
fn test_emergency_stop_recovery() {
    let bus = MockCanBus::new();

    // 正常状态
    bus.simulate_arm_state(MockArmState::Enabled);
    let state = bus.get_hardware_state();
    assert_eq!(state.arm_state, MockArmState::Enabled);

    // 急停
    bus.simulate_emergency_stop();
    let state = bus.get_hardware_state();
    assert!(state.emergency_stop);
    assert_eq!(state.arm_state, MockArmState::Error);

    // 恢复
    bus.clear_emergency_stop();
    let state = bus.get_hardware_state();
    assert!(!state.emergency_stop);
    assert_eq!(state.arm_state, MockArmState::Standby);
}

#[test]
fn test_latency_simulation() {
    let mut bus = MockCanBus::new();
    bus.set_latency(1000); // 1ms

    let start = std::time::Instant::now();
    let frame = MockCanFrame::new(0x01, vec![0x01]);
    bus.send_frame(frame).unwrap();
    let elapsed = start.elapsed();

    // 应该至少有 1ms 的延迟
    assert!(elapsed >= Duration::from_micros(1000));
}
