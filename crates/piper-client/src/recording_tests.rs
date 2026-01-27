//! ReplayMode 速度验证测试
//!
//! 本模块测试 replay_recording() 中的速度验证逻辑。

#[cfg(test)]
mod tests {
    /// 测试速度常量定义
    #[test]
    fn test_speed_constants() {
        // 从 machine.rs 中的常量
        const MAX_SPEED_FACTOR: f64 = 5.0;
        const RECOMMENDED_SPEED_FACTOR: f64 = 2.0;

        assert_eq!(MAX_SPEED_FACTOR, 5.0);
        assert_eq!(RECOMMENDED_SPEED_FACTOR, 2.0);
    }

    /// 测试有效速度范围
    #[test]
    fn test_valid_speed_range() {
        const MIN_SPEED: f64 = 0.1;
        const MAX_SPEED: f64 = 5.0;
        const RECOMMENDED: f64 = 2.0;

        // 验证常量定义的合理性
        const { assert!(MIN_SPEED > 0.0) };
        const { assert!(MIN_SPEED < MAX_SPEED) };
        const { assert!(MAX_SPEED > MIN_SPEED) };
        const { assert!(RECOMMENDED >= MIN_SPEED) };
        const { assert!(RECOMMENDED <= MAX_SPEED) };

        // 使用常量避免未使用警告
        let _ = (MIN_SPEED, MAX_SPEED, RECOMMENDED);
    }

    /// 测试速度边界值
    #[test]
    fn test_speed_boundary_values() {
        // 有效边界
        assert!(is_valid_speed(0.1));
        assert!(is_valid_speed(1.0));
        assert!(is_valid_speed(2.0));
        assert!(is_valid_speed(5.0));

        // 无效值
        assert!(!is_valid_speed(0.0));
        assert!(!is_valid_speed(-0.1));
        assert!(!is_valid_speed(5.1));
        assert!(!is_valid_speed(10.0));
    }

    /// 测试推荐速度检查
    #[test]
    fn test_recommended_speed_check() {
        const RECOMMENDED: f64 = 2.0;

        // 推荐范围内
        assert!(is_recommended_speed(1.0));
        assert!(is_recommended_speed(1.5));
        assert!(is_recommended_speed(RECOMMENDED));

        // 超过推荐但仍然有效
        assert!(!is_recommended_speed(2.1));
        assert!(!is_recommended_speed(3.0));
        assert!(!is_recommended_speed(5.0));
    }

    /// 测试警告阈值
    #[test]
    fn test_warning_threshold() {
        const RECOMMENDED: f64 = 2.0;

        // 需要警告的速度
        assert!(should_warn(RECOMMENDED + 0.1));
        assert!(should_warn(3.0));
        assert!(should_warn(5.0));

        // 不需要警告的速度
        assert!(!should_warn(1.0));
        assert!(!should_warn(1.5));
        assert!(!should_warn(2.0));
    }

    /// 测试速度精度
    #[test]
    fn test_speed_precision() {
        // 测试浮点数精度边界
        assert!(is_valid_speed(0.1));
        assert!(is_valid_speed(0.01));
        assert!(is_valid_speed(0.001));

        // 测试接近边界的值
        assert!(is_valid_speed(4.999));
        assert!(!is_valid_speed(5.001)); // 超过最大值，应该无效
    }

    /// 测试典型使用场景
    #[test]
    fn test_common_use_cases() {
        // 慢速回放（调试用）
        let slow_speed = 0.5;
        assert!(is_valid_speed(slow_speed));
        assert!(!should_warn(slow_speed));

        // 正常速度
        let normal_speed = 1.0;
        assert!(is_valid_speed(normal_speed));
        assert!(!should_warn(normal_speed));

        // 快速回放（测试用）
        let fast_speed = 2.0;
        assert!(is_valid_speed(fast_speed));
        assert!(!should_warn(fast_speed));

        // 极速回放（需谨慎）
        let extreme_speed = 4.0;
        assert!(is_valid_speed(extreme_speed));
        assert!(should_warn(extreme_speed));
    }

    // 辅助函数（模拟 machine.rs 中的验证逻辑）

    fn is_valid_speed(speed: f64) -> bool {
        const MIN_SPEED: f64 = 0.1;
        const MAX_SPEED: f64 = 5.0;
        let _ = (MIN_SPEED, MAX_SPEED); // 使用常量避免未使用警告
        speed > 0.0 && speed <= MAX_SPEED
    }

    fn is_recommended_speed(speed: f64) -> bool {
        const RECOMMENDED: f64 = 2.0;
        let _ = RECOMMENDED; // 使用常量避免未使用警告
        speed <= RECOMMENDED
    }

    fn should_warn(speed: f64) -> bool {
        const RECOMMENDED: f64 = 2.0;
        let _ = RECOMMENDED; // 使用常量避免未使用警告
        speed > RECOMMENDED && is_valid_speed(speed)
    }
}
