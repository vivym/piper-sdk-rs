//! 强类型单位系统
//!
//! 使用 NewType 模式防止单位混淆，在编译期保证类型安全。
//!
//! # 设计目标
//!
//! - **编译期类型安全**: 防止 `Rad` 与 `Deg` 混用
//! - **零开销抽象**: NewType 编译后与原始类型性能相同
//! - **符合人体工程学**: 支持运算符重载和链式调用
//!
//! # 示例
//!
//! ```rust
//! use piper_sdk::high_level::types::{Rad, Deg};
//!
//! let angle_rad = Rad(std::f64::consts::PI);
//! let angle_deg = angle_rad.to_deg();
//! assert!((angle_deg.0 - 180.0).abs() < 1e-6);
//!
//! // 类型安全：以下代码无法编译
//! // let _ = Rad(1.0) + Deg(1.0);  // ❌ 类型不匹配
//! ```

use std::fmt;
use std::ops::{Add, AddAssign, Div, DivAssign, Mul, MulAssign, Neg, Sub, SubAssign};

/// 弧度（NewType）
///
/// 表示角度的弧度值。使用 NewType 模式防止与角度值混淆。
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Rad(pub f64);

impl Rad {
    /// 零弧度常量
    pub const ZERO: Self = Rad(0.0);

    /// π 弧度（180度）
    pub const PI: Self = Rad(std::f64::consts::PI);

    /// 2π 弧度（360度）
    pub const TAU: Self = Rad(std::f64::consts::TAU);

    /// π/2 弧度（90度）
    pub const FRAC_PI_2: Self = Rad(std::f64::consts::FRAC_PI_2);

    /// π/4 弧度（45度）
    pub const FRAC_PI_4: Self = Rad(std::f64::consts::FRAC_PI_4);

    /// 创建新的弧度值
    #[inline]
    pub const fn new(value: f64) -> Self {
        Rad(value)
    }

    /// 转换为角度
    #[inline]
    pub fn to_deg(self) -> Deg {
        Deg(self.0.to_degrees())
    }

    /// 获取原始值
    #[inline]
    pub fn value(self) -> f64 {
        self.0
    }

    /// 计算正弦值
    #[inline]
    pub fn sin(self) -> f64 {
        self.0.sin()
    }

    /// 计算余弦值
    #[inline]
    pub fn cos(self) -> f64 {
        self.0.cos()
    }

    /// 计算正切值
    #[inline]
    pub fn tan(self) -> f64 {
        self.0.tan()
    }

    /// 取绝对值
    #[inline]
    pub fn abs(self) -> Self {
        Rad(self.0.abs())
    }

    /// 归一化到 [-π, π] 范围
    pub fn normalize(self) -> Self {
        let mut angle = self.0 % std::f64::consts::TAU;
        if angle > std::f64::consts::PI {
            angle -= std::f64::consts::TAU;
        } else if angle < -std::f64::consts::PI {
            angle += std::f64::consts::TAU;
        }
        Rad(angle)
    }

    /// 限制范围
    #[inline]
    pub fn clamp(self, min: Self, max: Self) -> Self {
        Rad(self.0.clamp(min.0, max.0))
    }
}

impl fmt::Display for Rad {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:.4} rad", self.0)
    }
}

// 运算符重载
impl Add for Rad {
    type Output = Self;
    #[inline]
    fn add(self, rhs: Self) -> Self {
        Rad(self.0 + rhs.0)
    }
}

impl Sub for Rad {
    type Output = Self;
    #[inline]
    fn sub(self, rhs: Self) -> Self {
        Rad(self.0 - rhs.0)
    }
}

impl Mul<f64> for Rad {
    type Output = Self;
    #[inline]
    fn mul(self, rhs: f64) -> Self {
        Rad(self.0 * rhs)
    }
}

impl Mul<Rad> for f64 {
    type Output = Rad;
    #[inline]
    fn mul(self, rhs: Rad) -> Rad {
        Rad(self * rhs.0)
    }
}

impl Div<f64> for Rad {
    type Output = Self;
    #[inline]
    fn div(self, rhs: f64) -> Self {
        Rad(self.0 / rhs)
    }
}

impl Div<Rad> for Rad {
    type Output = f64;
    #[inline]
    fn div(self, rhs: Rad) -> f64 {
        self.0 / rhs.0
    }
}

impl Neg for Rad {
    type Output = Self;
    #[inline]
    fn neg(self) -> Self {
        Rad(-self.0)
    }
}

impl AddAssign for Rad {
    #[inline]
    fn add_assign(&mut self, rhs: Self) {
        self.0 += rhs.0;
    }
}

impl SubAssign for Rad {
    #[inline]
    fn sub_assign(&mut self, rhs: Self) {
        self.0 -= rhs.0;
    }
}

impl MulAssign<f64> for Rad {
    #[inline]
    fn mul_assign(&mut self, rhs: f64) {
        self.0 *= rhs;
    }
}

impl DivAssign<f64> for Rad {
    #[inline]
    fn div_assign(&mut self, rhs: f64) {
        self.0 /= rhs;
    }
}

/// 角度（NewType）
///
/// 表示角度值。使用 NewType 模式防止与弧度值混淆。
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Deg(pub f64);

impl Deg {
    /// 零角度常量
    pub const ZERO: Self = Deg(0.0);

    /// 180 度
    pub const DEG_180: Self = Deg(180.0);

    /// 360 度
    pub const DEG_360: Self = Deg(360.0);

    /// 90 度
    pub const DEG_90: Self = Deg(90.0);

    /// 45 度
    pub const DEG_45: Self = Deg(45.0);

    /// 创建新的角度值
    #[inline]
    pub const fn new(value: f64) -> Self {
        Deg(value)
    }

    /// 转换为弧度
    #[inline]
    pub fn to_rad(self) -> Rad {
        Rad(self.0.to_radians())
    }

    /// 获取原始值
    #[inline]
    pub fn value(self) -> f64 {
        self.0
    }

    /// 取绝对值
    #[inline]
    pub fn abs(self) -> Self {
        Deg(self.0.abs())
    }

    /// 归一化到 [-180, 180] 范围
    pub fn normalize(self) -> Self {
        let mut angle = self.0 % 360.0;
        if angle > 180.0 {
            angle -= 360.0;
        } else if angle < -180.0 {
            angle += 360.0;
        }
        Deg(angle)
    }

    /// 限制范围
    #[inline]
    pub fn clamp(self, min: Self, max: Self) -> Self {
        Deg(self.0.clamp(min.0, max.0))
    }
}

impl fmt::Display for Deg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:.2}°", self.0)
    }
}

// 运算符重载
impl Add for Deg {
    type Output = Self;
    #[inline]
    fn add(self, rhs: Self) -> Self {
        Deg(self.0 + rhs.0)
    }
}

impl Sub for Deg {
    type Output = Self;
    #[inline]
    fn sub(self, rhs: Self) -> Self {
        Deg(self.0 - rhs.0)
    }
}

impl Mul<f64> for Deg {
    type Output = Self;
    #[inline]
    fn mul(self, rhs: f64) -> Self {
        Deg(self.0 * rhs)
    }
}

impl Mul<Deg> for f64 {
    type Output = Deg;
    #[inline]
    fn mul(self, rhs: Deg) -> Deg {
        Deg(self * rhs.0)
    }
}

impl Div<f64> for Deg {
    type Output = Self;
    #[inline]
    fn div(self, rhs: f64) -> Self {
        Deg(self.0 / rhs)
    }
}

impl Div<Deg> for Deg {
    type Output = f64;
    #[inline]
    fn div(self, rhs: Deg) -> f64 {
        self.0 / rhs.0
    }
}

impl Neg for Deg {
    type Output = Self;
    #[inline]
    fn neg(self) -> Self {
        Deg(-self.0)
    }
}

impl AddAssign for Deg {
    #[inline]
    fn add_assign(&mut self, rhs: Self) {
        self.0 += rhs.0;
    }
}

impl SubAssign for Deg {
    #[inline]
    fn sub_assign(&mut self, rhs: Self) {
        self.0 -= rhs.0;
    }
}

impl MulAssign<f64> for Deg {
    #[inline]
    fn mul_assign(&mut self, rhs: f64) {
        self.0 *= rhs;
    }
}

impl DivAssign<f64> for Deg {
    #[inline]
    fn div_assign(&mut self, rhs: f64) {
        self.0 /= rhs;
    }
}

/// 牛顿·米（力矩单位）
///
/// 表示力矩值。使用 NewType 模式提供类型安全。
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct NewtonMeter(pub f64);

impl NewtonMeter {
    /// 零力矩常量
    pub const ZERO: Self = NewtonMeter(0.0);

    /// 创建新的力矩值
    #[inline]
    pub const fn new(value: f64) -> Self {
        NewtonMeter(value)
    }

    /// 获取原始值
    #[inline]
    pub fn value(self) -> f64 {
        self.0
    }

    /// 取绝对值
    #[inline]
    pub fn abs(self) -> Self {
        NewtonMeter(self.0.abs())
    }

    /// 限制范围
    #[inline]
    pub fn clamp(self, min: Self, max: Self) -> Self {
        NewtonMeter(self.0.clamp(min.0, max.0))
    }
}

impl fmt::Display for NewtonMeter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:.3} N·m", self.0)
    }
}

// 运算符重载
impl Add for NewtonMeter {
    type Output = Self;
    #[inline]
    fn add(self, rhs: Self) -> Self {
        NewtonMeter(self.0 + rhs.0)
    }
}

impl Sub for NewtonMeter {
    type Output = Self;
    #[inline]
    fn sub(self, rhs: Self) -> Self {
        NewtonMeter(self.0 - rhs.0)
    }
}

impl Mul<f64> for NewtonMeter {
    type Output = Self;
    #[inline]
    fn mul(self, rhs: f64) -> Self {
        NewtonMeter(self.0 * rhs)
    }
}

impl Mul<NewtonMeter> for f64 {
    type Output = NewtonMeter;
    #[inline]
    fn mul(self, rhs: NewtonMeter) -> NewtonMeter {
        NewtonMeter(self * rhs.0)
    }
}

impl Div<f64> for NewtonMeter {
    type Output = Self;
    #[inline]
    fn div(self, rhs: f64) -> Self {
        NewtonMeter(self.0 / rhs)
    }
}

impl Div<NewtonMeter> for NewtonMeter {
    type Output = f64;
    #[inline]
    fn div(self, rhs: NewtonMeter) -> f64 {
        self.0 / rhs.0
    }
}

impl Neg for NewtonMeter {
    type Output = Self;
    #[inline]
    fn neg(self) -> Self {
        NewtonMeter(-self.0)
    }
}

impl AddAssign for NewtonMeter {
    #[inline]
    fn add_assign(&mut self, rhs: Self) {
        self.0 += rhs.0;
    }
}

impl SubAssign for NewtonMeter {
    #[inline]
    fn sub_assign(&mut self, rhs: Self) {
        self.0 -= rhs.0;
    }
}

impl MulAssign<f64> for NewtonMeter {
    #[inline]
    fn mul_assign(&mut self, rhs: f64) {
        self.0 *= rhs;
    }
}

impl DivAssign<f64> for NewtonMeter {
    #[inline]
    fn div_assign(&mut self, rhs: f64) {
        self.0 /= rhs;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Rad 测试
    #[test]
    fn test_rad_to_deg() {
        let rad = Rad(std::f64::consts::PI);
        let deg = rad.to_deg();
        assert!((deg.0 - 180.0).abs() < 1e-6);
    }

    #[test]
    fn test_deg_to_rad() {
        let deg = Deg(180.0);
        let rad = deg.to_rad();
        assert!((rad.0 - std::f64::consts::PI).abs() < 1e-6);
    }

    #[test]
    fn test_rad_operations() {
        let r1 = Rad(1.0);
        let r2 = Rad(2.0);

        assert_eq!(r1 + r2, Rad(3.0));
        assert_eq!(r2 - r1, Rad(1.0));
        assert_eq!(r1 * 2.0, Rad(2.0));
        assert_eq!(r2 / 2.0, Rad(1.0));
        assert_eq!(-r1, Rad(-1.0));
    }

    #[test]
    fn test_deg_operations() {
        let d1 = Deg(90.0);
        let d2 = Deg(180.0);

        assert_eq!(d1 + d2, Deg(270.0));
        assert_eq!(d2 - d1, Deg(90.0));
        assert_eq!(d1 * 2.0, Deg(180.0));
        assert_eq!(d2 / 2.0, Deg(90.0));
        assert_eq!(-d1, Deg(-90.0));
    }

    #[test]
    fn test_newton_meter_operations() {
        let nm1 = NewtonMeter(10.0);
        let nm2 = NewtonMeter(5.0);

        assert_eq!(nm1 + nm2, NewtonMeter(15.0));
        assert_eq!(nm1 - nm2, NewtonMeter(5.0));
        assert_eq!(nm1 * 2.0, NewtonMeter(20.0));
        assert_eq!(nm1 / 2.0, NewtonMeter(5.0));
        assert_eq!(-nm1, NewtonMeter(-10.0));
    }

    #[test]
    fn test_rad_normalize() {
        use std::f64::consts::PI;

        assert_eq!(Rad(0.0).normalize(), Rad(0.0));
        assert_eq!(Rad(PI).normalize(), Rad(PI));
        assert_eq!(Rad(-PI).normalize(), Rad(-PI));

        // 测试归一化：3π 应该归一化到 -π（因为 3π % 2π = π，然后 π > π 不成立，π < -π 不成立，所以保持 π）
        // 实际上 3π = π（mod 2π），所以结果应该是 π 附近
        let normalized = Rad(3.0 * PI).normalize();
        // 3π % 2π = π，所以归一化后应该是 π
        assert!((normalized.0 - PI).abs() < 1e-10);
    }

    #[test]
    fn test_deg_normalize() {
        assert_eq!(Deg(0.0).normalize(), Deg(0.0));
        assert_eq!(Deg(180.0).normalize(), Deg(180.0));
        assert_eq!(Deg(-180.0).normalize(), Deg(-180.0));

        // 测试归一化
        let normalized = Deg(540.0).normalize();
        assert!((normalized.0 - 180.0).abs() < 1e-10);
    }

    #[test]
    fn test_rad_trig_functions() {
        let rad = Rad(std::f64::consts::FRAC_PI_2);
        assert!((rad.sin() - 1.0).abs() < 1e-10);
        assert!(rad.cos().abs() < 1e-10);
    }

    #[test]
    fn test_display() {
        let rad = Rad(std::f64::consts::FRAC_PI_2);
        let deg = Deg(90.0);
        let nm = NewtonMeter(10.5);

        assert_eq!(format!("{}", rad), "1.5708 rad");
        assert_eq!(format!("{}", deg), "90.00°");
        assert_eq!(format!("{}", nm), "10.500 N·m");
    }

    #[test]
    fn test_clamp() {
        let rad = Rad(5.0);
        assert_eq!(rad.clamp(Rad(-1.0), Rad(1.0)), Rad(1.0));

        let deg = Deg(200.0);
        assert_eq!(deg.clamp(Deg(-90.0), Deg(90.0)), Deg(90.0));

        let nm = NewtonMeter(-100.0);
        assert_eq!(
            nm.clamp(NewtonMeter(-50.0), NewtonMeter(50.0)),
            NewtonMeter(-50.0)
        );
    }

    #[test]
    fn test_assign_operators() {
        let mut rad = Rad(1.0);
        rad += Rad(2.0);
        assert_eq!(rad, Rad(3.0));

        rad -= Rad(1.0);
        assert_eq!(rad, Rad(2.0));

        rad *= 2.0;
        assert_eq!(rad, Rad(4.0));

        rad /= 2.0;
        assert_eq!(rad, Rad(2.0));
    }
}
