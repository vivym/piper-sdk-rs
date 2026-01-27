//! Driver 模式定义
//!
//! 定义 Piper 驱动的工作模式，用于控制 TX 线程行为。

use std::sync::atomic::{AtomicU8, Ordering};

/// Driver 工作模式
///
/// # 模式说明
///
/// - **Normal**: 正常模式，TX 线程按周期发送控制指令（MIT/Position）
/// - **Replay**: 回放模式，TX 线程暂停周期性发送，仅发送显式指令
///
/// # 设计目的
///
/// Replay 模式用于安全地回放预先录制的 CAN 帧，避免双控制流冲突：
/// - 正常模式：TX 线程每 1ms 发送一次控制指令
/// - 回放模式：暂停 TX 线程，由回放逻辑控制帧发送时机
///
/// # 线程安全
///
/// 使用原子操作确保模式切换的线程安全性。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum DriverMode {
    /// 正常模式（默认）
    ///
    /// TX 线程按周期发送控制指令（MIT/Position 模式）。
    #[default]
    Normal = 0,

    /// 回放模式
    ///
    /// TX 线程暂停周期性发送，仅发送显式指令。
    /// 用于安全回放预先录制的 CAN 帧。
    Replay = 1,
}

impl DriverMode {
    /// 从 u8 转换
    ///
    /// 如果值无效，返回 Normal 模式。
    pub fn from_u8(value: u8) -> Self {
        match value {
            0 => Self::Normal,
            1 => Self::Replay,
            _ => Self::Normal, // 无效值默认为 Normal
        }
    }

    /// 转换为 u8
    pub fn as_u8(self) -> u8 {
        self as u8
    }

    /// 是否为回放模式
    pub fn is_replay(self) -> bool {
        self == Self::Replay
    }

    /// 是否为正常模式
    pub fn is_normal(self) -> bool {
        self == Self::Normal
    }
}

/// Driver 模式（原子版本，用于线程间共享）
///
/// # 使用场景
///
/// - TX 线程读取模式决定是否发送周期性指令
/// - 主线程通过 `set_mode()` 切换模式
///
/// # 示例
///
/// ```rust,no_run
/// use piper_driver::mode::{AtomicDriverMode, DriverMode};
/// use std::sync::atomic::Ordering;
///
/// let mode = AtomicDriverMode::new(DriverMode::Normal);
///
/// // 读取模式
/// let current = mode.get(Ordering::Relaxed);
///
/// // 切换到回放模式
/// mode.set(DriverMode::Replay, Ordering::Relaxed);
/// ```
#[derive(Debug)]
pub struct AtomicDriverMode {
    inner: AtomicU8,
}

impl AtomicDriverMode {
    /// 创建新的原子模式
    pub fn new(mode: DriverMode) -> Self {
        Self {
            inner: AtomicU8::new(mode.as_u8()),
        }
    }

    /// 获取当前模式
    ///
    /// # 参数
    ///
    /// - `ordering`: 内存序（通常使用 Relaxed 即可）
    pub fn get(&self, ordering: Ordering) -> DriverMode {
        DriverMode::from_u8(self.inner.load(ordering))
    }

    /// 设置模式
    ///
    /// # 参数
    ///
    /// - `mode`: 新模式
    /// - `ordering`: 内存序（通常使用 Relaxed 即可）
    pub fn set(&self, mode: DriverMode, ordering: Ordering) {
        self.inner.store(mode.as_u8(), ordering);
    }

    /// 比较并交换（Compare-and-Swap）
    ///
    /// # 参数
    ///
    /// - `current`: 期望的当前值
    /// - `new`: 新值
    /// - `ordering`: 内存序
    ///
    /// # 返回
    ///
    /// 如果当前值等于 `current`，则设置为 `new` 并返回 true
    /// 否则返回 false
    pub fn compare_exchange(
        &self,
        current: DriverMode,
        new: DriverMode,
        success: Ordering,
        failure: Ordering,
    ) -> bool {
        self.inner
            .compare_exchange(current.as_u8(), new.as_u8(), success, failure)
            .is_ok()
    }
}

impl Clone for AtomicDriverMode {
    fn clone(&self) -> Self {
        Self::new(self.get(Ordering::Relaxed))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_driver_mode_conversions() {
        let normal = DriverMode::Normal;
        let replay = DriverMode::Replay;

        assert_eq!(normal.as_u8(), 0);
        assert_eq!(replay.as_u8(), 1);

        assert!(normal.is_normal());
        assert!(!normal.is_replay());

        assert!(replay.is_replay());
        assert!(!replay.is_normal());
    }

    #[test]
    fn test_from_u8() {
        assert_eq!(DriverMode::from_u8(0), DriverMode::Normal);
        assert_eq!(DriverMode::from_u8(1), DriverMode::Replay);
        assert_eq!(DriverMode::from_u8(255), DriverMode::Normal); // 无效值
    }

    #[test]
    fn test_atomic_driver_mode() {
        let mode = AtomicDriverMode::new(DriverMode::Normal);

        assert_eq!(mode.get(Ordering::Relaxed), DriverMode::Normal);

        mode.set(DriverMode::Replay, Ordering::Relaxed);
        assert_eq!(mode.get(Ordering::Relaxed), DriverMode::Replay);

        // 测试 compare_exchange
        assert!(mode.compare_exchange(
            DriverMode::Replay,
            DriverMode::Normal,
            Ordering::Relaxed,
            Ordering::Relaxed
        ));
        assert_eq!(mode.get(Ordering::Relaxed), DriverMode::Normal);

        // 失败情况
        assert!(!mode.compare_exchange(
            DriverMode::Replay, // 期望值是 Replay，但实际是 Normal
            DriverMode::Replay,
            Ordering::Relaxed,
            Ordering::Relaxed
        ));
    }

    #[test]
    fn test_default() {
        let mode: DriverMode = Default::default();
        assert_eq!(mode, DriverMode::Normal);
    }
}
