//! Mock 硬件接口
//!
//! 用于测试的模拟 CAN 总线和硬件状态。

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// 模拟 CAN 帧
#[derive(Debug, Clone, PartialEq)]
pub struct MockCanFrame {
    pub id: u32,
    pub data: Vec<u8>,
    pub timestamp: Instant,
}

impl MockCanFrame {
    pub fn new(id: u32, data: Vec<u8>) -> Self {
        Self {
            id,
            data,
            timestamp: Instant::now(),
        }
    }
}

/// 模拟硬件状态
#[derive(Debug, Clone, PartialEq)]
pub enum MockArmState {
    Disconnected,
    Standby,
    Enabled,
    Error,
}

/// 模拟硬件状态管理
#[derive(Debug, Clone)]
pub struct MockHardwareState {
    pub arm_state: MockArmState,
    pub joint_positions: [f64; 6],
    pub joint_velocities: [f64; 6],
    pub joint_torques: [f64; 6],
    pub gripper_position: f64,
    pub gripper_effort: f64,
    pub gripper_enabled: bool,
    pub emergency_stop: bool,
    pub last_update: Instant,
}

impl Default for MockHardwareState {
    fn default() -> Self {
        Self {
            arm_state: MockArmState::Disconnected,
            joint_positions: [0.0; 6],
            joint_velocities: [0.0; 6],
            joint_torques: [0.0; 6],
            gripper_position: 0.0,
            gripper_effort: 0.0,
            gripper_enabled: false,
            emergency_stop: false,
            last_update: Instant::now(),
        }
    }
}

/// 模拟 CAN 总线
pub struct MockCanBus {
    /// 发送队列（从控制器发送到硬件）
    tx_queue: Arc<Mutex<VecDeque<MockCanFrame>>>,
    /// 接收队列（从硬件接收到控制器）
    rx_queue: Arc<Mutex<VecDeque<MockCanFrame>>>,
    /// 硬件状态
    hardware_state: Arc<Mutex<MockHardwareState>>,
    /// 模拟延迟（微秒）
    latency_us: u64,
    /// 是否模拟超时
    simulate_timeout: Arc<Mutex<bool>>,
}

impl MockCanBus {
    /// 创建新的模拟 CAN 总线
    pub fn new() -> Self {
        Self {
            tx_queue: Arc::new(Mutex::new(VecDeque::new())),
            rx_queue: Arc::new(Mutex::new(VecDeque::new())),
            hardware_state: Arc::new(Mutex::new(MockHardwareState::default())),
            latency_us: 10, // 默认 10μs 延迟
            simulate_timeout: Arc::new(Mutex::new(false)),
        }
    }

    /// 设置模拟延迟
    pub fn set_latency(&mut self, latency_us: u64) {
        self.latency_us = latency_us;
    }

    /// 发送 CAN 帧（控制器 -> 硬件）
    pub fn send_frame(&self, frame: MockCanFrame) -> Result<(), String> {
        // 检查是否模拟超时
        if *self.simulate_timeout.lock().unwrap() {
            return Err("Timeout".to_string());
        }

        // 模拟延迟
        if self.latency_us > 0 {
            std::thread::sleep(Duration::from_micros(self.latency_us));
        }

        // 添加到发送队列
        self.tx_queue.lock().unwrap().push_back(frame.clone());

        // 处理帧（更新硬件状态）
        self.process_frame(frame);

        Ok(())
    }

    /// 接收 CAN 帧（硬件 -> 控制器）
    pub fn recv_frame(&self, timeout: Duration) -> Result<MockCanFrame, String> {
        let start = Instant::now();

        loop {
            // 检查接收队列
            if let Some(frame) = self.rx_queue.lock().unwrap().pop_front() {
                return Ok(frame);
            }

            // 检查超时
            if start.elapsed() >= timeout {
                return Err("Timeout".to_string());
            }

            // 短暂休眠
            std::thread::sleep(Duration::from_micros(100));
        }
    }

    /// 处理 CAN 帧（更新硬件状态）
    fn process_frame(&self, frame: MockCanFrame) {
        let mut state = self.hardware_state.lock().unwrap();

        // 根据 CAN ID 处理不同的命令
        match frame.id {
            0x01 => {
                // 使能命令
                if !state.emergency_stop {
                    state.arm_state = MockArmState::Enabled;
                }
            },
            0x02 => {
                // 失能命令
                state.arm_state = MockArmState::Standby;
            },
            0x10..=0x15 => {
                // 关节控制命令（示例）
                let joint_idx = (frame.id - 0x10) as usize;
                if joint_idx < 6 && frame.data.len() >= 4 {
                    // 简化：直接设置位置
                    state.joint_positions[joint_idx] = f64::from_le_bytes([
                        frame.data[0],
                        frame.data[1],
                        frame.data[2],
                        frame.data[3],
                        0,
                        0,
                        0,
                        0,
                    ]);
                }
            },
            0x20 => {
                // 夹爪控制
                if frame.data.len() >= 8 {
                    state.gripper_position = f64::from_le_bytes([
                        frame.data[0],
                        frame.data[1],
                        frame.data[2],
                        frame.data[3],
                        0,
                        0,
                        0,
                        0,
                    ]);
                }
            },
            _ => {},
        }

        state.last_update = Instant::now();
    }

    /// 模拟机械臂状态变化
    pub fn simulate_arm_state(&self, new_state: MockArmState) {
        let mut state = self.hardware_state.lock().unwrap();
        state.arm_state = new_state;
        state.last_update = Instant::now();
    }

    /// 模拟急停按下
    pub fn simulate_emergency_stop(&self) {
        let mut state = self.hardware_state.lock().unwrap();
        state.emergency_stop = true;
        state.arm_state = MockArmState::Error;
        state.last_update = Instant::now();
    }

    /// 清除急停
    pub fn clear_emergency_stop(&self) {
        let mut state = self.hardware_state.lock().unwrap();
        state.emergency_stop = false;
        state.arm_state = MockArmState::Standby;
        state.last_update = Instant::now();
    }

    /// 模拟通信超时
    pub fn simulate_timeout(&self, enable: bool) {
        *self.simulate_timeout.lock().unwrap() = enable;
    }

    /// 获取硬件状态快照
    pub fn get_hardware_state(&self) -> MockHardwareState {
        self.hardware_state.lock().unwrap().clone()
    }

    /// 设置关节位置
    pub fn set_joint_position(&self, joint_idx: usize, position: f64) {
        let mut state = self.hardware_state.lock().unwrap();
        if joint_idx < 6 {
            state.joint_positions[joint_idx] = position;
            state.last_update = Instant::now();
        }
    }

    /// 设置夹爪状态
    pub fn set_gripper_state(&self, position: f64, effort: f64, enabled: bool) {
        let mut state = self.hardware_state.lock().unwrap();
        state.gripper_position = position;
        state.gripper_effort = effort;
        state.gripper_enabled = enabled;
        state.last_update = Instant::now();
    }

    /// 生成状态反馈帧（硬件 -> 控制器）
    pub fn generate_feedback_frame(&self) {
        let state = self.hardware_state.lock().unwrap();

        // 生成关节状态反馈
        for i in 0..6 {
            let frame = MockCanFrame::new(
                0x100 + i as u32,
                state.joint_positions[i].to_le_bytes()[0..4].to_vec(),
            );
            self.rx_queue.lock().unwrap().push_back(frame);
        }

        // 生成夹爪状态反馈
        let gripper_frame =
            MockCanFrame::new(0x200, state.gripper_position.to_le_bytes()[0..4].to_vec());
        self.rx_queue.lock().unwrap().push_back(gripper_frame);
    }

    /// 检查是否超时
    pub fn is_timeout(&self) -> bool {
        *self.simulate_timeout.lock().unwrap()
    }

    /// 获取发送队列长度
    pub fn tx_queue_len(&self) -> usize {
        self.tx_queue.lock().unwrap().len()
    }

    /// 清空队列
    pub fn clear_queues(&self) {
        self.tx_queue.lock().unwrap().clear();
        self.rx_queue.lock().unwrap().clear();
    }
}

impl Default for MockCanBus {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mock_can_bus_creation() {
        let bus = MockCanBus::new();
        assert_eq!(bus.tx_queue_len(), 0);
    }

    #[test]
    fn test_send_frame() {
        let bus = MockCanBus::new();
        let frame = MockCanFrame::new(0x01, vec![0x01]);

        bus.send_frame(frame).unwrap();
        assert_eq!(bus.tx_queue_len(), 1);
    }

    #[test]
    fn test_emergency_stop() {
        let bus = MockCanBus::new();

        // 初始状态
        let state = bus.get_hardware_state();
        assert_eq!(state.arm_state, MockArmState::Disconnected);
        assert!(!state.emergency_stop);

        // 模拟急停
        bus.simulate_emergency_stop();
        let state = bus.get_hardware_state();
        assert_eq!(state.arm_state, MockArmState::Error);
        assert!(state.emergency_stop);

        // 清除急停
        bus.clear_emergency_stop();
        let state = bus.get_hardware_state();
        assert_eq!(state.arm_state, MockArmState::Standby);
        assert!(!state.emergency_stop);
    }

    #[test]
    fn test_timeout_simulation() {
        let bus = MockCanBus::new();

        // 正常发送
        let frame = MockCanFrame::new(0x01, vec![0x01]);
        assert!(bus.send_frame(frame.clone()).is_ok());

        // 启用超时模拟
        bus.simulate_timeout(true);
        assert!(bus.send_frame(frame).is_err());

        // 禁用超时模拟
        bus.simulate_timeout(false);
        let frame = MockCanFrame::new(0x01, vec![0x01]);
        assert!(bus.send_frame(frame).is_ok());
    }

    #[test]
    fn test_joint_position_update() {
        let bus = MockCanBus::new();

        bus.set_joint_position(0, 1.5);
        let state = bus.get_hardware_state();
        assert_eq!(state.joint_positions[0], 1.5);
    }

    #[test]
    fn test_gripper_state_update() {
        let bus = MockCanBus::new();

        bus.set_gripper_state(0.05, 10.0, true);
        let state = bus.get_hardware_state();
        assert_eq!(state.gripper_position, 0.05);
        assert_eq!(state.gripper_effort, 10.0);
        assert!(state.gripper_enabled);
    }
}
