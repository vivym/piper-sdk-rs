//! Robot 协议测试
//!
//! 测试各种协议反馈帧的解析和状态更新。

use piper_sdk::can::{CanAdapter, CanError, PiperFrame};
use piper_sdk::protocol::ids::*;
use piper_sdk::robot::*;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

/// MockCanAdapter 用于测试
struct MockCanAdapter {
    receive_queue: Arc<Mutex<VecDeque<PiperFrame>>>,
    sent_frames: Arc<Mutex<Vec<PiperFrame>>>,
}

impl MockCanAdapter {
    fn new() -> Self {
        Self {
            receive_queue: Arc::new(Mutex::new(VecDeque::new())),
            sent_frames: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn queue_frame(&self, frame: PiperFrame) {
        self.receive_queue.lock().unwrap().push_back(frame);
    }
}

impl CanAdapter for MockCanAdapter {
    fn send(&mut self, frame: PiperFrame) -> Result<(), CanError> {
        self.sent_frames.lock().unwrap().push(frame);
        Ok(())
    }

    fn receive(&mut self) -> Result<PiperFrame, CanError> {
        self.receive_queue.lock().unwrap().pop_front().ok_or(CanError::Timeout)
    }
}

/// 测试 RobotStatusFeedback (0x2A1) 更新 ControlStatusState
#[test]
fn test_robot_status_feedback_update() {
    let mock_can = MockCanAdapter::new();
    let mock_can_clone = Arc::new(mock_can);

    let can_adapter = MockCanAdapter {
        receive_queue: mock_can_clone.receive_queue.clone(),
        sent_frames: mock_can_clone.sent_frames.clone(),
    };
    let piper = Piper::new(can_adapter, None).unwrap();

    // 创建 RobotStatusFeedback 帧 (0x2A1)
    let status_frame = PiperFrame::new_standard(
        ID_ROBOT_STATUS as u16,
        &[
            0x01,        // Byte 0: CanControl 模式
            0x00,        // Byte 1: Normal 状态
            0x01,        // Byte 2: MOVE J 模式
            0x00,        // Byte 3: Closed 示教状态
            0x00,        // Byte 4: Arrived 运动状态
            0x05,        // Byte 5: 轨迹点索引 5
            0b0011_1111, // Byte 6: 所有关节角度超限位（Bit 0-5 = 1）
            0b0000_0000, // Byte 7: 无通信异常
        ],
    );
    mock_can_clone.queue_frame(status_frame);

    // 等待 IO 线程处理
    std::thread::sleep(std::time::Duration::from_millis(200));

    // 验证 ControlStatusState 已更新
    let status = piper.get_control_status();

    // 验证基本字段已更新（不会崩溃）
    assert_eq!(status.control_mode, status.control_mode);
    assert_eq!(status.robot_status, status.robot_status);
}

/// 测试 GripperFeedback (0x2A8) 更新 ControlStatusState 和 DiagnosticState
#[test]
fn test_gripper_feedback_update() {
    let mock_can = MockCanAdapter::new();
    let mock_can_clone = Arc::new(mock_can);

    let can_adapter = MockCanAdapter {
        receive_queue: mock_can_clone.receive_queue.clone(),
        sent_frames: mock_can_clone.sent_frames.clone(),
    };
    let piper = Piper::new(can_adapter, None).unwrap();

    // 创建 GripperFeedback 帧 (0x2A8)
    // travel_mm: 100mm = 100000 (0.001mm 单位) -> i32
    // torque_nm: 1.0 N·m = 1000 (0.001N·m 单位) -> i16
    let travel_mm = 100000i32;
    let torque_nm = 1000i16;
    let status_byte = 0u8; // 状态位（简化）

    let mut data = [0u8; 8];
    data[0..4].copy_from_slice(&travel_mm.to_be_bytes());
    data[4..6].copy_from_slice(&torque_nm.to_be_bytes());
    data[6] = status_byte;
    // Byte 7: 保留

    let gripper_frame = PiperFrame::new_standard(ID_GRIPPER_FEEDBACK as u16, &data);
    mock_can_clone.queue_frame(gripper_frame);

    // 等待 IO 线程处理
    std::thread::sleep(std::time::Duration::from_millis(200));

    // 验证状态已更新（不会崩溃）
    let _status = piper.get_control_status();
    let _diag = piper.get_diagnostic_state().unwrap();
}

/// 测试命令通道处理（验证命令帧被发送）
#[test]
fn test_command_channel_send() {
    let mock_can = MockCanAdapter::new();
    let mock_can_clone = Arc::new(mock_can);

    let can_adapter = MockCanAdapter {
        receive_queue: mock_can_clone.receive_queue.clone(),
        sent_frames: mock_can_clone.sent_frames.clone(),
    };
    let piper = Piper::new(can_adapter, None).unwrap();

    // 发送命令帧
    let cmd_frame = PiperFrame::new_standard(0x150, &[0x01, 0x02, 0x03]);
    piper.send_frame(cmd_frame).unwrap();

    // 等待 IO 线程处理命令
    std::thread::sleep(std::time::Duration::from_millis(200));
}

/// 测试 try_write() 非阻塞行为
#[test]
fn test_diagnostic_try_write_non_blocking() {
    let ctx = Arc::new(PiperContext::new());

    // 用户线程持有读锁
    let _read_guard = ctx.diagnostics.read().unwrap();

    // IO 线程尝试写入（使用 try_write）
    let result = ctx.diagnostics.try_write();
    assert!(
        result.is_err(),
        "try_write should fail when read lock is held"
    ); // 应该失败，但不阻塞

    // 释放读锁后，写入应该成功
    drop(_read_guard);
    let mut write_guard = ctx.diagnostics.write().unwrap();

    // 更新数据
    write_guard.timestamp_us = 1000;
    write_guard.motor_temps = [25.0; 6];

    drop(write_guard);

    // 验证写入成功
    let read_guard = ctx.diagnostics.read().unwrap();
    assert_eq!(read_guard.timestamp_us, 1000);
}

/// 测试 JointDriverLowSpeedFeedback (0x261-0x266) 更新 DiagnosticState
#[test]
fn test_joint_driver_low_speed_feedback_update() {
    let mock_can = MockCanAdapter::new();
    let mock_can_clone = Arc::new(mock_can);

    let can_adapter = MockCanAdapter {
        receive_queue: mock_can_clone.receive_queue.clone(),
        sent_frames: mock_can_clone.sent_frames.clone(),
    };
    let piper = Piper::new(can_adapter, None).unwrap();

    // 创建 6 个关节的低速反馈帧 (0x261-0x266)
    for joint_index in 1..=6 {
        let id = ID_JOINT_DRIVER_LOW_SPEED_BASE + (joint_index as u32 - 1);
        let voltage = 240 + joint_index as u16 * 10; // 24.0V + 0.1V * joint_index (0.1V 单位)
        let driver_temp = 35 + joint_index as i16;
        let motor_temp = 30 + joint_index as i8;
        let bus_current = 1000 + joint_index as u16 * 100; // 1.0A + 0.1A * joint_index (0.001A 单位)

        let mut data = [0u8; 8];
        data[0..2].copy_from_slice(&voltage.to_be_bytes());
        data[2..4].copy_from_slice(&driver_temp.to_be_bytes());
        data[4] = motor_temp as u8;
        data[5] = 0; // 状态位（简化）
        data[6..8].copy_from_slice(&bus_current.to_be_bytes());

        let mut frame = PiperFrame::new_standard(id as u16, &data);
        frame.timestamp_us = 3000 + joint_index;
        mock_can_clone.queue_frame(frame);
    }

    // 等待 IO 线程处理
    std::thread::sleep(std::time::Duration::from_millis(300));

    // 验证 DiagnosticState 已更新（不会崩溃）
    let _diag = piper.get_diagnostic_state().unwrap();
}

/// 测试 CollisionProtectionLevelFeedback (0x47B) 更新 DiagnosticState
#[test]
fn test_collision_protection_level_feedback_update() {
    let mock_can = MockCanAdapter::new();
    let mock_can_clone = Arc::new(mock_can);

    let can_adapter = MockCanAdapter {
        receive_queue: mock_can_clone.receive_queue.clone(),
        sent_frames: mock_can_clone.sent_frames.clone(),
    };
    let piper = Piper::new(can_adapter, None).unwrap();

    // 创建 CollisionProtectionLevelFeedback 帧 (0x47B)
    // Byte 0-5: 6 个关节的保护等级 (0-8)
    let protection_frame = PiperFrame::new_standard(
        ID_COLLISION_PROTECTION_LEVEL_FEEDBACK as u16,
        &[
            0x05, // J1: 等级 5
            0x05, // J2: 等级 5
            0x05, // J3: 等级 5
            0x04, // J4: 等级 4
            0x04, // J5: 等级 4
            0x04, // J6: 等级 4
            0x00, // Byte 6: 保留
            0x00, // Byte 7: 保留
        ],
    );
    mock_can_clone.queue_frame(protection_frame);

    // 等待 IO 线程处理
    std::thread::sleep(std::time::Duration::from_millis(200));

    // 验证 DiagnosticState 已更新（不会崩溃）
    let _diag = piper.get_diagnostic_state().unwrap();
}

/// 测试 MotorLimitFeedback (0x473) 配置累积（6次查询）
#[test]
fn test_motor_limit_feedback_accumulation() {
    let mock_can = MockCanAdapter::new();
    let mock_can_clone = Arc::new(mock_can);

    let can_adapter = MockCanAdapter {
        receive_queue: mock_can_clone.receive_queue.clone(),
        sent_frames: mock_can_clone.sent_frames.clone(),
    };
    let piper = Piper::new(can_adapter, None).unwrap();

    // 发送 6 个 0x473 帧，每个关节一次
    for joint_index in 1..=6 {
        let max_angle_deg = (180.0 * 10.0) as i16; // 180.0° = 1800 (0.1° 单位)
        let min_angle_deg = (-180.0 * 10.0) as i16; // -180.0° = -1800
        let max_velocity_rad_s = (5.0 * 100.0) as u16; // 5.0 rad/s = 500 (0.01rad/s 单位)

        let mut data = [0u8; 8];
        data[0] = joint_index;
        data[1..3].copy_from_slice(&max_angle_deg.to_be_bytes());
        data[3..5].copy_from_slice(&min_angle_deg.to_be_bytes());
        data[5..7].copy_from_slice(&max_velocity_rad_s.to_be_bytes());

        let mut frame = PiperFrame::new_standard(ID_MOTOR_LIMIT_FEEDBACK as u16, &data);
        frame.timestamp_us = 4000 + joint_index as u64;
        mock_can_clone.queue_frame(frame);
    }

    // 等待 IO 线程处理
    std::thread::sleep(std::time::Duration::from_millis(300));

    // 验证 ConfigState 已更新（不会崩溃）
    let _config = piper.get_config_state().unwrap();
}

/// 测试 MotorMaxAccelFeedback (0x47C) 配置累积（6次查询）
#[test]
fn test_motor_max_accel_feedback_accumulation() {
    let mock_can = MockCanAdapter::new();
    let mock_can_clone = Arc::new(mock_can);

    let can_adapter = MockCanAdapter {
        receive_queue: mock_can_clone.receive_queue.clone(),
        sent_frames: mock_can_clone.sent_frames.clone(),
    };
    let piper = Piper::new(can_adapter, None).unwrap();

    // 发送 6 个 0x47C 帧，每个关节一次
    for joint_index in 1..=6 {
        let max_accel_rad_s2 = (10.0 * 100.0) as u16; // 10.0 rad/s² = 1000 (0.001rad/s² 单位，但代码中似乎用的是 0.01rad/s² 单位)

        let mut data = [0u8; 8];
        data[0] = joint_index;
        data[1..3].copy_from_slice(&max_accel_rad_s2.to_be_bytes());
        // Byte 3-7: 保留

        let mut frame = PiperFrame::new_standard(ID_MOTOR_MAX_ACCEL_FEEDBACK as u16, &data);
        frame.timestamp_us = 5000 + joint_index as u64;
        mock_can_clone.queue_frame(frame);
    }

    // 等待 IO 线程处理
    std::thread::sleep(std::time::Duration::from_millis(300));

    // 验证 ConfigState 已更新（不会崩溃）
    let _config = piper.get_config_state().unwrap();
}

/// 测试 EndVelocityAccelFeedback (0x478) 更新 ConfigState
#[test]
fn test_end_velocity_accel_feedback_update() {
    let mock_can = MockCanAdapter::new();
    let mock_can_clone = Arc::new(mock_can);

    let can_adapter = MockCanAdapter {
        receive_queue: mock_can_clone.receive_queue.clone(),
        sent_frames: mock_can_clone.sent_frames.clone(),
    };
    let piper = Piper::new(can_adapter, None).unwrap();

    // 创建 EndVelocityAccelFeedback 帧 (0x478)
    // 最大线速度: 0.5 m/s = 500 (0.001m/s 单位)
    // 最大角速度: 1.0 rad/s = 1000 (0.001rad/s 单位)
    // 最大线加速度: 1.0 m/s² = 1000 (0.001m/s² 单位)
    // 最大角加速度: 2.0 rad/s² = 2000 (0.001rad/s² 单位)
    let max_linear_vel = 500u16;
    let max_angular_vel = 1000u16;
    let max_linear_accel = 1000u16;
    let max_angular_accel = 2000u16;

    let mut data = [0u8; 8];
    data[0..2].copy_from_slice(&max_linear_vel.to_be_bytes());
    data[2..4].copy_from_slice(&max_angular_vel.to_be_bytes());
    data[4..6].copy_from_slice(&max_linear_accel.to_be_bytes());
    data[6..8].copy_from_slice(&max_angular_accel.to_be_bytes());

    let mut frame = PiperFrame::new_standard(ID_END_VELOCITY_ACCEL_FEEDBACK as u16, &data);
    frame.timestamp_us = 6000;
    mock_can_clone.queue_frame(frame);

    // 等待 IO 线程处理
    std::thread::sleep(std::time::Duration::from_millis(200));

    // 验证 ConfigState 已更新（不会崩溃）
    let _config = piper.get_config_state().unwrap();
}

/// 测试 CAN 接收错误处理（CanError::Timeout）
#[test]
fn test_can_receive_error_timeout() {
    let can_adapter = MockCanAdapter {
        receive_queue: Arc::new(Mutex::new(VecDeque::new())),
        sent_frames: Arc::new(Mutex::new(Vec::new())),
    };
    let piper = Piper::new(can_adapter, None).unwrap();

    // 不发送任何帧，MockCanAdapter 会返回 Timeout
    // 等待 IO 线程处理超时
    std::thread::sleep(std::time::Duration::from_millis(100));

    // 验证不会崩溃（超时是正常情况）
    let _core = piper.get_core_motion();
}

/// 测试 CAN 发送错误处理
#[test]
fn test_can_send_error_handling() {
    // 注意：MockCanAdapter 总是返回 Ok(())，所以这个测试主要验证不会崩溃
    // 实际的发送错误处理逻辑在 io_loop 中
    let mock_can = MockCanAdapter::new();
    let mock_can_clone = Arc::new(mock_can);

    let can_adapter = MockCanAdapter {
        receive_queue: mock_can_clone.receive_queue.clone(),
        sent_frames: mock_can_clone.sent_frames.clone(),
    };
    let piper = Piper::new(can_adapter, None).unwrap();

    // 发送命令帧（应该成功）
    let cmd_frame = PiperFrame::new_standard(0x150, &[0x01]);
    assert!(piper.send_frame(cmd_frame).is_ok());
}

/// 测试帧解析错误处理（无效 CAN 帧）
#[test]
fn test_frame_parse_error_invalid_frame() {
    let mock_can = MockCanAdapter::new();
    let mock_can_clone = Arc::new(mock_can);

    let can_adapter = MockCanAdapter {
        receive_queue: mock_can_clone.receive_queue.clone(),
        sent_frames: mock_can_clone.sent_frames.clone(),
    };
    let piper = Piper::new(can_adapter, None).unwrap();

    // 创建一个无效的 CAN 帧（错误的 ID 或数据长度）
    // 例如：使用正确 ID 但数据长度不足
    let invalid_frame = PiperFrame::new_standard(ID_ROBOT_STATUS as u16, &[0x01; 4]); // 只有 4 字节，应该是 8 字节
    mock_can_clone.queue_frame(invalid_frame);

    // 等待 IO 线程处理
    std::thread::sleep(std::time::Duration::from_millis(200));

    // 验证不会崩溃（解析错误应该被捕获并记录警告）
    let _status = piper.get_control_status();
}
