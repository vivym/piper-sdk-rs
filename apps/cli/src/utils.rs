//! 用户交互工具
//!
//! 提供用户输入确认等交互功能

use anyhow::Result;
use std::io::{self, Write};

/// 请求用户确认
///
/// # 参数
///
/// - `prompt`: 确认提示信息
/// - `default`: 默认值（true 表示默认确认）
///
/// # 返回
///
/// 返回用户的选择（true 表示确认，false 表示取消）
///
/// # 示例
///
/// ```no_run
/// use crate::utils::prompt_confirmation;
///
/// if prompt_confirmation("确定要继续吗？", false)? {
///     println!("用户确认");
/// } else {
///     println!("用户取消");
/// }
/// ```
#[allow(dead_code)]
pub fn prompt_confirmation(prompt: &str, default: bool) -> Result<bool> {
    let default_text = if default { "[Y/n]" } else { "[y/N]" };

    print!("⚠️  {} {}? ", prompt, default_text);
    io::stdout().flush()?;

    let mut input = String::new();
    io::stdin().read_line(&mut input)?;

    let input = input.trim().to_lowercase();

    // 空输入返回默认值
    if input.is_empty() {
        return Ok(default);
    }

    // 检查输入
    let confirmed = input == "y" || input == "yes" || input == "ye";

    Ok(confirmed)
}

/// 请求用户输入文本
///
/// # 参数
///
/// - `prompt`: 提示信息
/// - `default`: 默认值（可选）
///
/// # 返回
///
/// 返回用户输入的文本
///
/// # 示例
///
/// ```no_run
/// use crate::utils::prompt_input;
///
/// let name = prompt_input("请输入名称", Some("默认名称"))?;
/// println!("名称: {}", name);
/// ```
#[allow(dead_code)]
pub fn prompt_input(prompt: &str, default: Option<&str>) -> Result<String> {
    if let Some(def) = default {
        print!("💬 {} [默认: {}]: ", prompt, def);
    } else {
        print!("💬 {}: ", prompt);
    }
    io::stdout().flush()?;

    let mut input = String::new();
    io::stdin().read_line(&mut input)?;

    let input = input.trim().to_string();

    // 空输入返回默认值
    if input.is_empty()
        && let Some(def) = default
    {
        return Ok(def.to_string());
    }

    Ok(input)
}

#[cfg(test)]
mod tests {
    #[allow(unused_imports)]
    use super::*;

    // 注意：这些测试需要用户输入，在实际环境中可能需要跳过
    // 这里只是作为文档示例
}
