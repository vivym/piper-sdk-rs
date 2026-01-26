//! 关节归零确认令牌
//!
//! **安全机制**：关节归零是危险操作，可能导致机械臂撞击限位或损坏设备。
//! 因此引入确认令牌机制，强制用户明确确认此操作。
//!
//! # 三种构造方式
//!
//! 1. **`confirm_from_env()`**（推荐用于 CLI 应用）
//!    - 从环境变量 `PIPER_ZEROING_CONFIRM` 读取确认
//!    - 值必须是 "I_CONFIRM_ZEROING_IS_DANGEROUS"
//!    - 推荐用于生产环境
//!
//! 2. **`unsafe fn new_unchecked()`**（供 GUI 应用使用）
//!    - 不检查任何条件，直接创建令牌
//!    - 用户必须在 UI 中已经确认
//!    - 安全性由 GUI 应用层保证
//!
//! 3. **`confirm_for_test()`**（仅测试可用）
//!    - 仅在 `cfg(test)` 下可用
//!    - 用于单元测试和集成测试
//!
//! # 使用示例
//!
//! ## CLI 应用
//!
//! ```rust,no_run
//! use piper_client::control::ZeroingConfirmToken;
//!
//! // 用户需要在命令行明确确认：
//! // export PIPER_ZEROING_CONFIRM=I_CONFIRM_ZEROING_IS_DANGEROUS
//! let token = ZeroingConfirmToken::confirm_from_env()?;
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! ## GUI 应用
//!
//! ```rust,no_run
//! use piper_client::control::ZeroingConfirmToken;
//! use std::io;
//!
//! fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     // 显示确认对话框，用户点击"我确认"
//!     if show_confirmation_dialog() {
//!         // ⚠️ 用户已在 UI 中确认，这里使用 unsafe 跳过检查
//!         let token = unsafe { ZeroingConfirmToken::new_unchecked() };
//!     } else {
//!         return Err(Box::new(io::Error::new(
//!             io::ErrorKind::Other,
//!             "User cancelled"
//!         )) as Box<dyn std::error::Error>);
//!     }
//!     Ok(())
//! }
//! # fn show_confirmation_dialog() -> bool { true }
//! ```
//!
//! ## 测试代码
//!
//! ```rust,ignore
//! use piper_client::control::ZeroingConfirmToken;
//!
//! #[test]
//! fn test_zeroing() {
//!     let token = ZeroingConfirmToken::confirm_for_test();
//!     // 执行归零操作...
//! }
//! ```

use std::env;
use std::io;

/// 环境变量名称
const ENV_VAR: &str = "PIPPER_ZEROING_CONFIRM";

/// 环境变量期望值（必须完全匹配）
const ENV_VALUE: &str = "I_CONFIRM_ZEROING_IS_DANGEROUS";

/// 关节归零确认令牌（Zero-token 类型模式）
///
/// **安全机制**：此类型只能通过三种方式创建：
/// 1. 从环境变量读取（`confirm_from_env()`）
/// 2. unsafe 创建（`new_unchecked()`）
/// 3. 测试创建（`confirm_for_test()`，仅测试可用）
///
/// 这确保了用户明确确认了归零操作的 danger。
///
/// # 设计说明
///
/// 这是一个 **Zero-cost 类型安全** 模式：
/// - 类型大小：0 字节（ZST，零大小类型）
/// - 运行时开销：0
/// - 编译期检查：✅
///
/// # 示例
///
/// ```rust,no_run
/// # use piper_client::control::ZeroingConfirmToken;
/// // 从环境变量读取（推荐）
/// let token = ZeroingConfirmToken::confirm_from_env()?;
///
/// // 或使用 unsafe（仅用于 GUI）
/// let token = unsafe { ZeroingConfirmToken::new_unchecked() };
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug, Clone, Copy)]
pub struct ZeroingConfirmToken(());

impl ZeroingConfirmToken {
    /// 从环境变量确认（推荐用于 CLI 应用）
    ///
    /// **环境变量**：`PIPPER_ZEROING_CONFIRM`
    /// **期望值**：`I_CONFIRM_ZEROING_IS_DANGEROUS`
    ///
    /// # 错误
    ///
    /// - 环境变量未设置
    /// - 环境变量值不匹配
    ///
    /// # 示例
    ///
    /// ```rust,no_run
    /// # use piper_client::control::ZeroingConfirmToken;
    /// // 用户需要在命令行明确确认：
    /// // export PIPER_ZEROING_CONFIRM=I_CONFIRM_ZEROING_IS_DANGEROUS
    /// let token = ZeroingConfirmToken::confirm_from_env()?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn confirm_from_env() -> Result<Self, ZeroingTokenError> {
        let value = env::var(ENV_VAR).map_err(|_| ZeroingTokenError::EnvNotSet {
            var: ENV_VAR.to_string(),
        })?;

        if value == ENV_VALUE {
            Ok(Self(()))
        } else {
            Err(ZeroingTokenError::EnvValueMismatch {
                expected: ENV_VALUE.to_string(),
                found: value,
            })
        }
    }

    /// 不安全创建（供 GUI 应用使用）
    ///
    /// **⚠️ 安全契约**：
    ///
    /// 调用此方法前，必须确保：
    /// 1. 用户已在 UI 中明确确认归零操作的 danger
    /// 2. 显示了清晰的警告信息
    /// 3. 用户主动点击了"确认"按钮（或其他明确的确认动作）
    ///
    /// # Safety
    ///
    /// 调用者必须保证用户已经明确确认了归零操作的 danger。
    /// 此函数绕过了环境变量检查，因此调用者有责任确保用户同意。
    ///
    /// # 示例
    ///
    /// ```rust,no_run
    /// # use piper_client::control::ZeroingConfirmToken;
    /// # use std::io;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// // 显示确认对话框
    /// if show_confirmation_dialog() {
    ///     // ⚠️ 用户已确认，使用 unsafe 跳过检查
    ///     let token = unsafe { ZeroingConfirmToken::new_unchecked() };
    /// } else {
    ///     return Err(Box::new(io::Error::new(
    ///         io::ErrorKind::Other,
    ///         "User cancelled"
    ///     )) as Box<dyn std::error::Error>);
    /// }
    /// # Ok(())
    /// # }
    /// # fn show_confirmation_dialog() -> bool { true }
    /// ```
    pub unsafe fn new_unchecked() -> Self {
        Self(())
    }

    /// 测试用创建（仅测试可用）
    ///
    /// **⚠️ 注意**：此方法仅在 `cfg(test)` 下可用。
    ///
    /// # 示例
    ///
    /// ```rust,ignore
    /// # use piper_client::control::ZeroingConfirmToken;
    /// #[test]
    /// fn test_zeroing() {
    ///     let token = ZeroingConfirmToken::confirm_for_test();
    ///     // 执行归零操作...
    /// }
    /// ```
    #[cfg(test)]
    pub fn confirm_for_test() -> Self {
        Self(())
    }
}

/// ZeroingToken 错误
#[derive(Debug, thiserror::Error)]
pub enum ZeroingTokenError {
    /// 环境变量未设置
    #[error(
        "Environment variable not set. \
            Please export {var}='I_CONFIRM_ZEROING_IS_DANGEROUS' to confirm you understand the risks."
    )]
    EnvNotSet { var: String },

    /// 环境变量值不匹配
    #[error(
        "Environment variable has incorrect value. \
            Expected: 'I_CONFIRM_ZEROING_IS_DANGEROUS', Found: '{found}'"
    )]
    EnvValueMismatch { expected: String, found: String },

    /// IO 错误（用于兼容）
    #[error("IO error: {0}")]
    Io(#[from] io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    #[test]
    fn test_confirm_from_env_success() {
        unsafe {
            env::set_var(ENV_VAR, ENV_VALUE);
        }
        let token = ZeroingConfirmToken::confirm_from_env();
        assert!(token.is_ok());
        unsafe {
            env::remove_var(ENV_VAR);
        }
    }

    #[test]
    fn test_confirm_from_env_not_set() {
        unsafe {
            env::remove_var(ENV_VAR);
        }
        let token = ZeroingConfirmToken::confirm_from_env();
        assert!(matches!(token, Err(ZeroingTokenError::EnvNotSet { .. })));
    }

    #[test]
    fn test_confirm_from_env_wrong_value() {
        unsafe {
            env::set_var(ENV_VAR, "wrong_value");
        }
        let token = ZeroingConfirmToken::confirm_from_env();
        assert!(matches!(
            token,
            Err(ZeroingTokenError::EnvValueMismatch { .. })
        ));
        unsafe {
            env::remove_var(ENV_VAR);
        }
    }

    #[test]
    fn test_confirm_for_test() {
        let token = ZeroingConfirmToken::confirm_for_test();
        // Zero-size type，无法直接比较，但至少可以创建
        let _ = token;
    }

    #[test]
    fn test_new_unchecked() {
        // unsafe 块内的代码可以编译
        let token = unsafe { ZeroingConfirmToken::new_unchecked() };
        let _ = token;
    }

    #[test]
    fn test_token_is_zero_sized() {
        assert_eq!(std::mem::size_of::<ZeroingConfirmToken>(), 0);
    }

    #[test]
    fn test_token_is_copy() {
        let token1 = unsafe { ZeroingConfirmToken::new_unchecked() };
        let token2 = token1; // Copy，token1 仍然有效
        let _ = (token1, token2);
    }
}
