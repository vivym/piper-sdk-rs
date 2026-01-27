//! 高级诊断接口（逃生舱）
//!
//! 本模块提供对底层 driver 的受限访问，用于高级诊断、调试和性能分析场景。
//!
//! # 设计理念
//!
//! 这是一个**受限的逃生舱**（Escape Hatch），暴露了底层 driver 的部分功能：
//! - ✅ 可以访问 context.hooks（注册自定义回调）
//! - ✅ 可以访问 send_frame（发送原始 CAN 帧）
//! - ❌ 不能直接调用 enable/disable（保持状态机安全）
//!
//! # 线程安全
//!
//! `PiperDiagnostics` 持有 `Arc<piper_driver::Piper>`，可以安全地跨线程传递：
//! - ✅ **独立生命周期**：不受原始 `Piper` 实例生命周期约束
//! - ✅ **跨线程使用**：可以在诊断线程中长期持有
//! - ✅ **`'static`**：可以存储在 `static` 变量或线程局部存储中
//!
//! # 权衡说明
//!
//! 由于持有 `Arc` 而非引用，`PiperDiagnostics` **脱离了 TypeState 的直接保护**。
//! 这是逃生舱设计的**有意权衡**：
//! - 优点：灵活性极高，适合复杂的诊断场景
//! - 缺点：无法在编译时保证关联的 `Piper` 仍然处于特定状态
//! - 缓解：通过运行时检查和文档警告来保证安全
//!
//! # 使用场景
//!
//! - 自定义诊断工具
//! - 高级抓包和调试
//! - 性能分析和优化
//! - 非标准回放逻辑
//! - 后台监控线程
//!
//! # 安全注意事项
//!
//! 此接口提供的底层能力**可能破坏状态机的不变性**。
//! 使用时需注意：
//! 1. **不要在 Active 状态下发送控制指令**（会导致双控制流冲突）
//! 2. **不要手动调用 `disable()`**（应该通过 `Piper` 的 `Drop` 来处理）
//! 3. **确保回调执行时间 <1μs**（否则会影响实时性能）
//! 4. **注意生命周期**：即使持有 `Arc`，也要确保关联的 `Piper` 实例未被销毁
//!
//! # 示例
//!
//! ## 基础使用
//!
//! ```rust,no_run
//! use piper_client::{PiperBuilder};
//! use piper_driver::recording::AsyncRecordingHook;
//! use std::sync::Arc;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let robot = PiperBuilder::new()
//!     .interface("can0")
//!     .build()?;
//!
//! let active = robot.enable_position_mode(Default::default())?;
//!
//! // 获取诊断接口（持有 Arc，独立生命周期）
//! let diag = active.diagnostics();
//!
//! // 创建自定义录制钩子
//! let (hook, rx) = AsyncRecordingHook::new();
//!
//! // 注册钩子
//! diag.register_callback(Arc::new(hook))?;
//!
//! // 在后台线程处理录制数据
//! std::thread::spawn(move || {
//!     while let Ok(frame) = rx.recv() {
//!         println!("Received CAN frame: 0x{:03X}", frame.id);
//!     }
//! });
//! # Ok(())
//! # }
//! ```
//!
//! ## 跨线程长期持有
//!
//! ```rust,no_run
//! use piper_client::{PiperBuilder};
//! use std::thread;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let robot = PiperBuilder::new()
//!     .interface("can0")
//!     .build()?;
//!
//! let active = robot.enable_position_mode(Default::default())?;
//!
//! // 获取诊断接口（可以安全地移动到其他线程）
//! let diag = active.diagnostics();
//!
//! // 在另一个线程中长期持有
//! thread::spawn(move || {
//!     // diag 在这里完全独立，不受主线程影响
//!     loop {
//!         // 执行诊断逻辑...
//!         std::thread::sleep(std::time::Duration::from_secs(1));
//!     }
//! });
//!
//! // 主线程可以继续使用 active
//! // active.send_position_command(&target)?;
//!
//! # Ok(())
//! # }
//! ```

use piper_can::PiperFrame;
use piper_driver::FrameCallback;
use std::sync::Arc;

// 使用 Result 类型别名（使用 crate 的 RobotError）
pub type Result<T> = std::result::Result<T, crate::RobotError>;

/// 高级诊断接口（逃生舱）
///
/// # 持有 Arc 引用计数指针
///
/// `PiperDiagnostics` 持有 `Arc<piper_driver::Piper>`：
/// - 轻量级克隆（仅增加引用计数）
/// - 独立生命周期，不受原始 `Piper` 实例约束
/// - 可以安全地跨线程传递
///
/// # 参考设计
///
/// 这与 Rust 社区成熟库的逃生舱设计一致：
/// - `reqwest::Client`：持有 `Arc<ClientInner>`，可跨线程
/// - `tokio::runtime::Handle`：持有 `Arc<Runtime>`，独立生命周期
pub struct PiperDiagnostics {
    /// 持有 driver 的 Arc 克隆
    ///
    /// **设计权衡**：
    /// - 使用 `Arc` 而非引用 → 独立生命周期，可跨线程
    /// - 脱离 TypeState 保护 → 依赖运行时检查
    driver: Arc<piper_driver::Piper>,
}

impl PiperDiagnostics {
    pub(super) fn new<M>(inner: &crate::state::Piper<crate::state::Active<M>>) -> Self {
        // 克隆 Arc（轻量级操作，仅增加引用计数）
        Self {
            driver: Arc::clone(&inner.driver),
        }
    }

    /// 注册自定义 FrameCallback
    ///
    /// # 性能要求
    ///
    /// 回调会在 Driver 层的 RX 线程中执行，必须保证：
    /// - 执行时间 <1μs
    /// - 不阻塞
    /// - 线程安全（Send + Sync）
    ///
    /// # 示例
    ///
    /// ```rust,no_run
    /// # use piper_client::PiperBuilder;
    /// # use piper_driver::recording::AsyncRecordingHook;
    /// # use std::sync::Arc;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let robot = PiperBuilder::new()
    ///     .interface("can0")
    ///     .build()?;
    ///
    /// let active = robot.enable_position_mode(Default::default())?;
    /// let diag = active.diagnostics();
    ///
    /// let (hook, _rx) = AsyncRecordingHook::new();
    /// diag.register_callback(Arc::new(hook))?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn register_callback(&self, callback: Arc<dyn FrameCallback>) -> Result<()> {
        self.driver
            .hooks()
            .write()
            .map_err(|_e| {
                crate::RobotError::Infrastructure(piper_driver::DriverError::PoisonedLock)
            })?
            .add_callback(callback);
        Ok(())
    }

    /// 发送原始 CAN 帧
    ///
    /// # ⚠️ 安全警告
    ///
    /// **严禁在 Active 状态下发送控制指令帧（0x1A1-0x1FF）**。
    /// 这会导致与驱动层的周期性发送任务产生双控制流冲突。
    ///
    /// # 允许的使用场景
    ///
    /// - ✅ Standby 状态：发送配置帧（0x5A1-0x5FF）
    /// - ✅ ReplayMode：回放预先录制的帧
    /// - ✅ 调试：发送测试帧
    ///
    /// # 禁止的使用场景
    ///
    /// - ❌ Active<MIT>：发送 0x1A1-0x1A6（位置/速度/力矩指令）
    /// - ❌ Active<Position>: 发送 0x1A1-0x1A6
    ///
    /// # 示例
    ///
    /// ```rust,no_run
    /// # use piper_client::PiperBuilder;
    /// # use piper_can::PiperFrame;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let robot = PiperBuilder::new()
    ///     .interface("can0")
    ///     .build()?;
    ///
    /// let active = robot.enable_position_mode(Default::default())?;
    /// let diag = active.diagnostics();
    ///
    /// // 发送配置帧（安全）
    /// let frame = PiperFrame {
    ///     id: 0x5A1,
    ///     data: [0, 1, 2, 3, 4, 5, 6, 7],
    ///     len: 8,
    ///     is_extended: false,
    ///     timestamp_us: 0,
    /// };
    /// diag.send_frame(&frame)?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn send_frame(&self, frame: &PiperFrame) -> Result<()> {
        self.driver.send_frame(*frame)?;
        Ok(())
    }

    /// 获取 driver 实例的 Arc 克隆（完全访问）
    ///
    /// # ⚠️ 高级逃生舱
    ///
    /// 此方法提供对底层 `piper_driver::Piper` 的完全访问。
    /// 仅用于**极端特殊场景**，99% 的情况下应该使用上面的 `register_callback` 和 `send_frame`。
    ///
    /// # 使用前提
    ///
    /// 你必须完全理解以下文档：
    /// - `piper_driver` 模块文档
    /// - 类型状态机设计
    /// - Driver 层 IO 线程模型
    ///
    /// # 安全保证
    ///
    /// 返回的是 `Arc` 引用计数指针，而非不可变引用：
    /// - ✅ 可以跨线程传递
    /// - ✅ 可以长期持有
    /// - ❌ 无法直接调用 `enable/disable`（这些方法需要 `&mut self`）
    ///
    /// # 示例
    ///
    /// ```rust,no_run
    /// # use piper_client::PiperBuilder;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let robot = PiperBuilder::new()
    ///     .interface("can0")
    ///     .build()?;
    ///
    /// let active = robot.enable_position_mode(Default::default())?;
    /// let diag = active.diagnostics();
    ///
    /// // 获取完全访问权限（仅在极端特殊场景使用）
    /// let driver = diag.driver();
    ///
    /// // 访问底层 hooks
    /// let hooks = driver.hooks();
    /// # Ok(())
    /// # }
    /// ```
    pub fn driver(&self) -> Arc<piper_driver::Piper> {
        Arc::clone(&self.driver)
    }
}

// SAFETY: PiperDiagnostics 持有 Arc，可以安全地在线程间传递
unsafe impl Send for PiperDiagnostics {}
unsafe impl Sync for PiperDiagnostics {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_diagnostics_send_sync() {
        // 确保 PiperDiagnostics 实现 Send + Sync
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<PiperDiagnostics>();
    }
}
