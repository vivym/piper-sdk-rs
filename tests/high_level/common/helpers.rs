//! 测试辅助函数
//!
//! 提供快速创建测试环境的工具函数。

use super::mock_hardware::{MockArmState, MockCanBus};
use std::sync::Arc;

/// 测试配置
#[derive(Debug, Clone)]
pub struct TestConfig {
    /// 模拟延迟（微秒）
    pub latency_us: u64,
    /// 初始机械臂状态
    pub initial_arm_state: MockArmState,
    /// 是否启用急停
    pub emergency_stop: bool,
}

impl Default for TestConfig {
    fn default() -> Self {
        Self {
            latency_us: 10,
            initial_arm_state: MockArmState::Standby,
            emergency_stop: false,
        }
    }
}

/// 快速创建标准测试环境
///
/// # 返回
///
/// 返回配置好的 MockCanBus
///
/// # 示例
///
/// ```rust,ignore
/// let bus = setup_test_environment();
/// // 开始测试...
/// ```
pub fn setup_test_environment() -> Arc<MockCanBus> {
    setup_test_environment_with_config(TestConfig::default())
}

/// 使用自定义配置创建测试环境
pub fn setup_test_environment_with_config(config: TestConfig) -> Arc<MockCanBus> {
    let mut bus = MockCanBus::new();
    bus.set_latency(config.latency_us);

    let bus = Arc::new(bus);

    // 设置初始状态
    bus.simulate_arm_state(config.initial_arm_state);

    if config.emergency_stop {
        bus.simulate_emergency_stop();
    }

    bus
}

/// 创建已使能的测试环境
pub fn setup_enabled_test_environment() -> Arc<MockCanBus> {
    setup_test_environment_with_config(TestConfig {
        initial_arm_state: MockArmState::Enabled,
        ..Default::default()
    })
}

/// 创建模拟超时的测试环境
pub fn setup_timeout_test_environment() -> Arc<MockCanBus> {
    let bus = setup_test_environment();
    bus.simulate_timeout(true);
    bus
}

/// 等待条件满足（带超时）
///
/// # 参数
///
/// - `condition`: 检查条件的闭包
/// - `timeout_ms`: 超时时间（毫秒）
/// - `check_interval_ms`: 检查间隔（毫秒）
///
/// # 返回
///
/// 如果条件在超时前满足返回 Ok(())，否则返回 Err
pub fn wait_for_condition<F>(
    mut condition: F,
    timeout_ms: u64,
    check_interval_ms: u64,
) -> Result<(), String>
where
    F: FnMut() -> bool,
{
    let start = std::time::Instant::now();
    let timeout = std::time::Duration::from_millis(timeout_ms);
    let interval = std::time::Duration::from_millis(check_interval_ms);

    while start.elapsed() < timeout {
        if condition() {
            return Ok(());
        }
        std::thread::sleep(interval);
    }

    Err(format!("Timeout after {}ms", timeout_ms))
}

/// 生成测试用的关节位置数组
pub fn test_joint_positions(base_value: f64) -> [f64; 6] {
    [
        base_value,
        base_value + 0.1,
        base_value + 0.2,
        base_value + 0.3,
        base_value + 0.4,
        base_value + 0.5,
    ]
}

/// 生成测试用的关节速度数组
pub fn test_joint_velocities(base_value: f64) -> [f64; 6] {
    [
        base_value,
        base_value * 1.1,
        base_value * 1.2,
        base_value * 1.3,
        base_value * 1.4,
        base_value * 1.5,
    ]
}

/// 断言浮点数相等（带误差容忍）
pub fn assert_float_eq(a: f64, b: f64, epsilon: f64) {
    assert!(
        (a - b).abs() < epsilon,
        "Values not equal: {} vs {} (epsilon: {})",
        a,
        b,
        epsilon
    );
}

/// 断言数组相等（带误差容忍）
pub fn assert_array_eq(a: &[f64; 6], b: &[f64; 6], epsilon: f64) {
    for i in 0..6 {
        assert_float_eq(a[i], b[i], epsilon);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_setup_test_environment() {
        let bus = setup_test_environment();
        let state = bus.get_hardware_state();
        assert_eq!(state.arm_state, MockArmState::Standby);
        assert!(!state.emergency_stop);
    }

    #[test]
    fn test_setup_enabled_environment() {
        let bus = setup_enabled_test_environment();
        let state = bus.get_hardware_state();
        assert_eq!(state.arm_state, MockArmState::Enabled);
    }

    #[test]
    fn test_wait_for_condition_success() {
        let mut counter = 0;
        let result = wait_for_condition(
            || {
                counter += 1;
                counter >= 3
            },
            1000,
            10,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_wait_for_condition_timeout() {
        let result = wait_for_condition(|| false, 100, 10);
        assert!(result.is_err());
    }

    #[test]
    fn test_joint_positions_generation() {
        let positions = test_joint_positions(1.0);
        assert_eq!(positions[0], 1.0);
        assert_eq!(positions[5], 1.5);
    }

    #[test]
    fn test_float_eq_assertion() {
        assert_float_eq(1.0, 1.0001, 0.001);
    }

    #[test]
    #[should_panic]
    fn test_float_eq_assertion_fails() {
        assert_float_eq(1.0, 1.1, 0.01);
    }

    #[test]
    fn test_array_eq_assertion() {
        let a = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0];
        let b = [1.0001, 2.0001, 3.0001, 4.0001, 5.0001, 6.0001];
        assert_array_eq(&a, &b, 0.001);
    }
}
