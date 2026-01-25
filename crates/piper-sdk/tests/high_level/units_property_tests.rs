//! 单位类型的属性测试
//!
//! 使用 proptest 验证数学属性。

#[path = "common/mod.rs"]
mod common;

use piper_sdk::client::types::{Rad, Deg, NewtonMeter};
use proptest::prelude::*;

proptest! {
    /// 测试弧度到角度的往返转换
    #[test]
    fn rad_deg_roundtrip(rad in -100.0..100.0f64) {
        let r = Rad(rad);
        let d = r.to_deg();
        let r2 = d.to_rad();
        prop_assert!((r.0 - r2.0).abs() < 1e-10);
    }

    /// 测试角度到弧度的往返转换
    #[test]
    fn deg_rad_roundtrip(deg in -360.0..360.0f64) {
        let d = Deg(deg);
        let r = d.to_rad();
        let d2 = r.to_deg();
        prop_assert!((d.0 - d2.0).abs() < 1e-10);
    }

    /// 测试弧度加法交换律
    #[test]
    fn rad_addition_commutative(a in -10.0..10.0f64, b in -10.0..10.0f64) {
        let r1 = Rad(a);
        let r2 = Rad(b);
        prop_assert_eq!(r1 + r2, r2 + r1);
    }

    /// 测试弧度加法结合律
    #[test]
    fn rad_addition_associative(a in -10.0..10.0f64, b in -10.0..10.0f64, c in -10.0..10.0f64) {
        let r1 = Rad(a);
        let r2 = Rad(b);
        let r3 = Rad(c);
        let left = (r1 + r2) + r3;
        let right = r1 + (r2 + r3);
        prop_assert!((left.0 - right.0).abs() < 1e-10);
    }

    /// 测试弧度乘法分配律
    #[test]
    fn rad_distributive(a in -10.0..10.0f64, b in -10.0..10.0f64, c in -10.0..10.0f64) {
        let r1 = Rad(a);
        let r2 = Rad(b);
        let left = (r1 + r2) * c;
        let right = r1 * c + r2 * c;
        prop_assert!((left.0 - right.0).abs() < 1e-9);
    }

    /// 测试弧度归一化幂等性
    #[test]
    fn rad_normalize_idempotent(rad in -1000.0..1000.0f64) {
        let r = Rad(rad);
        let n1 = r.normalize();
        let n2 = n1.normalize();
        prop_assert_eq!(n1, n2);
    }

    /// 测试角度归一化幂等性
    #[test]
    fn deg_normalize_idempotent(deg in -3600.0..3600.0f64) {
        let d = Deg(deg);
        let n1 = d.normalize();
        let n2 = n1.normalize();
        prop_assert_eq!(n1, n2);
    }

    /// 测试弧度归一化范围
    #[test]
    fn rad_normalize_range(rad in -1000.0..1000.0f64) {
        let r = Rad(rad).normalize();
        prop_assert!(r.0 >= -std::f64::consts::PI && r.0 <= std::f64::consts::PI);
    }

    /// 测试角度归一化范围
    #[test]
    fn deg_normalize_range(deg in -3600.0..3600.0f64) {
        let d = Deg(deg).normalize();
        prop_assert!(d.0 >= -180.0 && d.0 <= 180.0);
    }

    /// 测试力矩加法交换律
    #[test]
    fn nm_addition_commutative(a in -100.0..100.0f64, b in -100.0..100.0f64) {
        let nm1 = NewtonMeter(a);
        let nm2 = NewtonMeter(b);
        prop_assert_eq!(nm1 + nm2, nm2 + nm1);
    }

    /// 测试力矩标量乘法结合律
    #[test]
    fn nm_scalar_multiplication_associative(nm in -100.0..100.0f64, a in -10.0..10.0f64, b in -10.0..10.0f64) {
        let nm1 = NewtonMeter(nm);
        let left = (nm1 * a) * b;
        let right = nm1 * (a * b);
        prop_assert!((left.0 - right.0).abs() < 1e-9);
    }

    /// 测试取反幂等性
    #[test]
    fn rad_negation_involution(rad in -100.0..100.0f64) {
        let r = Rad(rad);
        prop_assert_eq!(r, --r);
    }

    /// 测试零元素性质
    #[test]
    fn rad_additive_identity(rad in -100.0..100.0f64) {
        let r = Rad(rad);
        prop_assert_eq!(r + Rad::ZERO, r);
        prop_assert_eq!(Rad::ZERO + r, r);
    }

    /// 测试乘法单位元
    #[test]
    fn rad_multiplicative_identity(rad in -100.0..100.0f64) {
        let r = Rad(rad);
        prop_assert_eq!(r * 1.0, r);
    }

    /// 测试clamp下界
    #[test]
    fn rad_clamp_lower_bound(rad in -100.0..100.0f64, min in -50.0..-10.0f64, max in 10.0..50.0f64) {
        let r = Rad(rad);
        let clamped = r.clamp(Rad(min), Rad(max));
        prop_assert!(clamped.0 >= min);
    }

    /// 测试clamp上界
    #[test]
    fn rad_clamp_upper_bound(rad in -100.0..100.0f64, min in -50.0..-10.0f64, max in 10.0..50.0f64) {
        let r = Rad(rad);
        let clamped = r.clamp(Rad(min), Rad(max));
        prop_assert!(clamped.0 <= max);
    }
}

#[cfg(test)]
mod additional_tests {
    use super::*;

    /// 测试特殊值
    #[test]
    fn test_special_values() {
        // π 弧度 = 180 度
        let pi_rad = Rad::PI;
        let deg_180 = pi_rad.to_deg();
        assert!((deg_180.0 - 180.0).abs() < 1e-10);

        // 2π 弧度 = 360 度
        let tau_rad = Rad::TAU;
        let deg_360 = tau_rad.to_deg();
        assert!((deg_360.0 - 360.0).abs() < 1e-10);

        // π/2 弧度 = 90 度
        let pi_2_rad = Rad::FRAC_PI_2;
        let deg_90 = pi_2_rad.to_deg();
        assert!((deg_90.0 - 90.0).abs() < 1e-10);
    }

    /// 测试三角函数值
    #[test]
    fn test_trig_values() {
        // sin(π/2) = 1
        assert!((Rad::FRAC_PI_2.sin() - 1.0).abs() < 1e-10);

        // cos(0) = 1
        assert!((Rad::ZERO.cos() - 1.0).abs() < 1e-10);

        // sin(π) ≈ 0
        assert!(Rad::PI.sin().abs() < 1e-10);

        // cos(π) = -1
        assert!((Rad::PI.cos() + 1.0).abs() < 1e-10);
    }

    /// 测试归一化边界情况
    #[test]
    fn test_normalize_edge_cases() {
        // 正好 π
        assert_eq!(Rad::PI.normalize(), Rad::PI);

        // 正好 -π
        assert_eq!(Rad(-std::f64::consts::PI).normalize(), Rad(-std::f64::consts::PI));

        // 刚好超过 π
        let just_over = Rad(std::f64::consts::PI + 0.1);
        let normalized = just_over.normalize();
        assert!(normalized.0 < 0.0); // 应该回绕到负数

        // 正好 180 度
        assert_eq!(Deg::DEG_180.normalize(), Deg(180.0));

        // 正好 -180 度
        assert_eq!(Deg(-180.0).normalize(), Deg(-180.0));
    }

    /// 测试绝对值
    #[test]
    fn test_abs() {
        assert_eq!(Rad(-1.5).abs(), Rad(1.5));
        assert_eq!(Deg(-90.0).abs(), Deg(90.0));
        assert_eq!(NewtonMeter(-10.5).abs(), NewtonMeter(10.5));
    }
}

