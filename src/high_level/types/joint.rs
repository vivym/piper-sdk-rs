//! 关节索引和数组
//!
//! 提供编译期安全的关节索引，防止越界和索引错误。
//!
//! # 设计目标
//!
//! - **编译期安全**: 使用枚举防止无效索引
//! - **零开销**: 编译后与直接数组访问性能相同
//! - **类型友好**: 支持泛型和迭代器
//!
//! # 示例
//!
//! ```rust
//! use piper_sdk::high_level::types::{Joint, JointArray, Rad};
//!
//! let positions = JointArray::new([
//!     Rad(0.0), Rad(0.1), Rad(0.2),
//!     Rad(0.3), Rad(0.4), Rad(0.5),
//! ]);
//!
//! // 类型安全的索引访问
//! let j1_pos = positions[Joint::J1];
//! assert_eq!(j1_pos, Rad(0.0));
//!
//! // 迭代器
//! for (joint, pos) in Joint::ALL.iter().zip(positions.iter()) {
//!     println!("{:?}: {}", joint, pos);
//! }
//!
//! // 映射转换
//! let deg_positions = positions.map(|r| r.to_deg());
//! ```

use super::units::Rad;
use std::fmt;
use std::ops::{Index, IndexMut};

/// 关节枚举
///
/// 表示 Piper 机械臂的 6 个关节。使用枚举提供编译期类型安全。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Joint {
    /// 关节 1（基座旋转）
    J1 = 0,
    /// 关节 2（肩部俯仰）
    J2 = 1,
    /// 关节 3（肘部俯仰）
    J3 = 2,
    /// 关节 4（腕部旋转）
    J4 = 3,
    /// 关节 5（腕部俯仰）
    J5 = 4,
    /// 关节 6（末端旋转）
    J6 = 5,
}

impl Joint {
    /// 所有关节的数组
    pub const ALL: [Joint; 6] = [
        Joint::J1,
        Joint::J2,
        Joint::J3,
        Joint::J4,
        Joint::J5,
        Joint::J6,
    ];

    /// 获取关节索引（0-5）
    #[inline]
    pub const fn index(self) -> usize {
        self as usize
    }

    /// 从索引创建关节（范围检查）
    pub fn from_index(index: usize) -> Option<Self> {
        match index {
            0 => Some(Joint::J1),
            1 => Some(Joint::J2),
            2 => Some(Joint::J3),
            3 => Some(Joint::J4),
            4 => Some(Joint::J5),
            5 => Some(Joint::J6),
            _ => None,
        }
    }

    /// 获取关节名称
    pub const fn name(self) -> &'static str {
        match self {
            Joint::J1 => "J1",
            Joint::J2 => "J2",
            Joint::J3 => "J3",
            Joint::J4 => "J4",
            Joint::J5 => "J5",
            Joint::J6 => "J6",
        }
    }
}

impl fmt::Display for Joint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.name())
    }
}

/// 关节数组
///
/// 类型安全的 6 关节数组容器，支持索引、迭代和映射操作。
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct JointArray<T> {
    data: [T; 6],
}

// 如果 T 实现了 Copy，则 JointArray<T> 也实现 Copy
impl<T: Copy> Copy for JointArray<T> {}

impl<T> JointArray<T> {
    /// 创建新的关节数组
    #[inline]
    pub const fn new(data: [T; 6]) -> Self {
        JointArray { data }
    }

    /// 获取内部数组的引用
    #[inline]
    pub fn as_array(&self) -> &[T; 6] {
        &self.data
    }

    /// 获取内部数组的可变引用
    #[inline]
    pub fn as_array_mut(&mut self) -> &mut [T; 6] {
        &mut self.data
    }

    /// 获取内部数组（消耗 self）
    #[inline]
    pub fn into_array(self) -> [T; 6] {
        self.data
    }

    /// 迭代器
    pub fn iter(&self) -> std::slice::Iter<'_, T> {
        self.data.iter()
    }

    /// 可变迭代器
    pub fn iter_mut(&mut self) -> std::slice::IterMut<'_, T> {
        self.data.iter_mut()
    }

    /// 映射转换
    pub fn map<U, F>(self, mut f: F) -> JointArray<U>
    where
        F: FnMut(T) -> U,
    {
        let [a, b, c, d, e, g] = self.data;
        JointArray::new([f(a), f(b), f(c), f(d), f(e), f(g)])
    }

    /// 带索引的映射转换
    pub fn map_with_joint<U, F>(self, mut f: F) -> JointArray<U>
    where
        F: FnMut(Joint, T) -> U,
    {
        let [a, b, c, d, e, g] = self.data;
        JointArray::new([
            f(Joint::J1, a),
            f(Joint::J2, b),
            f(Joint::J3, c),
            f(Joint::J4, d),
            f(Joint::J5, e),
            f(Joint::J6, g),
        ])
    }

    /// 按关节和另一个数组的元素执行映射
    pub fn map_with<U, V, F>(self, other: JointArray<U>, mut f: F) -> JointArray<V>
    where
        F: FnMut(T, U) -> V,
    {
        let [a1, b1, c1, d1, e1, f1] = self.data;
        let [a2, b2, c2, d2, e2, f2] = other.data;
        JointArray::new([
            f(a1, a2),
            f(b1, b2),
            f(c1, c2),
            f(d1, d2),
            f(e1, e2),
            f(f1, f2),
        ])
    }
}

impl<T: Copy> JointArray<T> {
    /// 创建所有元素相同的数组
    #[inline]
    pub const fn splat(value: T) -> Self {
        JointArray::new([value, value, value, value, value, value])
    }
}

impl<T: Default> Default for JointArray<T> {
    fn default() -> Self {
        JointArray::new([
            T::default(),
            T::default(),
            T::default(),
            T::default(),
            T::default(),
            T::default(),
        ])
    }
}

// 索引访问
impl<T> Index<Joint> for JointArray<T> {
    type Output = T;

    #[inline]
    fn index(&self, joint: Joint) -> &T {
        &self.data[joint.index()]
    }
}

impl<T> IndexMut<Joint> for JointArray<T> {
    #[inline]
    fn index_mut(&mut self, joint: Joint) -> &mut T {
        &mut self.data[joint.index()]
    }
}

impl<T> Index<usize> for JointArray<T> {
    type Output = T;

    #[inline]
    fn index(&self, index: usize) -> &T {
        &self.data[index]
    }
}

impl<T> IndexMut<usize> for JointArray<T> {
    #[inline]
    fn index_mut(&mut self, index: usize) -> &mut T {
        &mut self.data[index]
    }
}

// From/Into 转换
impl<T> From<[T; 6]> for JointArray<T> {
    #[inline]
    fn from(data: [T; 6]) -> Self {
        JointArray::new(data)
    }
}

impl<T> From<JointArray<T>> for [T; 6] {
    #[inline]
    fn from(arr: JointArray<T>) -> Self {
        arr.data
    }
}

// IntoIterator 实现
impl<T> IntoIterator for JointArray<T> {
    type Item = T;
    type IntoIter = std::array::IntoIter<T, 6>;

    fn into_iter(self) -> Self::IntoIter {
        self.data.into_iter()
    }
}

impl<'a, T> IntoIterator for &'a JointArray<T> {
    type Item = &'a T;
    type IntoIter = std::slice::Iter<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.data.iter()
    }
}

impl<'a, T> IntoIterator for &'a mut JointArray<T> {
    type Item = &'a mut T;
    type IntoIter = std::slice::IterMut<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.data.iter_mut()
    }
}

/// 关节位置（弧度）
pub type JointPositions = JointArray<Rad>;

/// 关节速度（弧度/秒）
pub type JointVelocities = JointArray<Rad>;

/// 关节加速度（弧度/秒²）
pub type JointAccelerations = JointArray<Rad>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_joint_index() {
        assert_eq!(Joint::J1.index(), 0);
        assert_eq!(Joint::J6.index(), 5);
    }

    #[test]
    fn test_joint_from_index() {
        assert_eq!(Joint::from_index(0), Some(Joint::J1));
        assert_eq!(Joint::from_index(5), Some(Joint::J6));
        assert_eq!(Joint::from_index(6), None);
    }

    #[test]
    fn test_joint_name() {
        assert_eq!(Joint::J1.name(), "J1");
        assert_eq!(format!("{}", Joint::J3), "J3");
    }

    #[test]
    fn test_joint_all() {
        assert_eq!(Joint::ALL.len(), 6);
        assert_eq!(Joint::ALL[0], Joint::J1);
        assert_eq!(Joint::ALL[5], Joint::J6);
    }

    #[test]
    fn test_joint_array_creation() {
        let arr = JointArray::new([1, 2, 3, 4, 5, 6]);
        assert_eq!(arr[Joint::J1], 1);
        assert_eq!(arr[Joint::J6], 6);
    }

    #[test]
    fn test_joint_array_indexing() {
        let positions =
            JointArray::new([Rad(0.0), Rad(0.1), Rad(0.2), Rad(0.3), Rad(0.4), Rad(0.5)]);

        assert_eq!(positions[Joint::J1], Rad(0.0));
        assert_eq!(positions[Joint::J6], Rad(0.5));

        // 使用 usize 索引
        assert_eq!(positions[0], Rad(0.0));
        assert_eq!(positions[5], Rad(0.5));
    }

    #[test]
    fn test_joint_array_iteration() {
        let positions = JointArray::new([Rad(1.0); 6]);
        let sum: f64 = positions.iter().map(|r| r.0).sum();
        assert!((sum - 6.0).abs() < 1e-10);
    }

    #[test]
    fn test_joint_array_map() {
        let rad = JointArray::new([Rad(std::f64::consts::PI); 6]);
        let deg = rad.map(|r| r.to_deg());
        assert!((deg[Joint::J1].0 - 180.0).abs() < 1e-6);
    }

    #[test]
    fn test_joint_array_map_with_joint() {
        let positions = JointArray::new([Rad(1.0); 6]);
        let scaled = positions.map_with_joint(|joint, rad| Rad(rad.0 * (joint.index() + 1) as f64));

        assert_eq!(scaled[Joint::J1], Rad(1.0));
        assert_eq!(scaled[Joint::J2], Rad(2.0));
        assert_eq!(scaled[Joint::J6], Rad(6.0));
    }

    #[test]
    fn test_joint_array_map_with() {
        let a = JointArray::new([1.0, 2.0, 3.0, 4.0, 5.0, 6.0]);
        let b = JointArray::new([0.5, 0.5, 0.5, 0.5, 0.5, 0.5]);
        let c = a.map_with(b, |x, y| x * y);

        assert_eq!(c[Joint::J1], 0.5);
        assert_eq!(c[Joint::J2], 1.0);
    }

    #[test]
    fn test_joint_array_splat() {
        let arr = JointArray::splat(Rad(0.5));
        for joint in Joint::ALL {
            assert_eq!(arr[joint], Rad(0.5));
        }
    }

    #[test]
    fn test_joint_array_mut() {
        let mut positions = JointArray::new([Rad(0.0); 6]);
        positions[Joint::J3] = Rad(1.5);
        assert_eq!(positions[Joint::J3], Rad(1.5));
    }

    #[test]
    fn test_joint_array_into_iter() {
        let arr = JointArray::new([1, 2, 3, 4, 5, 6]);
        let vec: Vec<_> = arr.into_iter().collect();
        assert_eq!(vec, vec![1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn test_joint_array_default() {
        let arr: JointArray<i32> = JointArray::default();
        for i in 0..6 {
            assert_eq!(arr[i], 0);
        }
    }

    #[test]
    fn test_joint_positions_type() {
        let positions: JointPositions = JointArray::new([Rad(0.0); 6]);
        assert_eq!(positions[Joint::J1], Rad(0.0));
    }

    #[test]
    fn test_from_into_array() {
        let data = [1, 2, 3, 4, 5, 6];
        let joint_array = JointArray::from(data);
        let back: [i32; 6] = joint_array.into();
        assert_eq!(data, back);
    }
}
