//! CAN 接口状态检查模块
//!
//! 使用 ioctl 系统调用检查 Linux 网络接口是否存在且已启动（UP 状态）。
//!
//! 此模块仅提供检查功能，不进行任何配置操作，因此不需要特殊权限。

use crate::can::CanError;
use libc::{AF_INET, IFF_UP, SIOCGIFFLAGS, SOCK_DGRAM, if_nametoindex, ifreq};
use std::ffi::CString;
use std::io;
use tracing::trace;

/// 检查 CAN 接口是否存在且已启动（管理态 UP）
///
/// 使用 `if_nametoindex()` 检查接口是否存在，使用 `ioctl(SIOCGIFFLAGS)` 检查接口状态。
///
/// # 参数
/// - `interface`: 接口名称（如 "can0"、"vcan0"）
///
/// # 返回值
/// - `Ok(true)`: 接口存在且 IFF_UP 标志位为真
/// - `Ok(false)`: 接口存在但处于 DOWN 状态（IFF_UP 为假）
/// - `Err(CanError::Device)`: 接口不存在或接口名无效
/// - `Err(CanError::Io)`: 系统调用失败（socket/ioctl 错误）
///
/// # 权限要求
/// 此函数只进行读取操作，普通用户即可执行，不需要 root 或 CAP_NET_ADMIN 权限。
pub fn check_interface_status(interface: &str) -> Result<bool, CanError> {
    // 0. 先检查接口名长度（必须在调用 if_nametoindex 之前检查）
    // ifr_name 通常是 IFNAMSIZ = 16 字节，包括结尾的 NUL，所以最大长度是 15
    const MAX_IFACE_NAME_LEN: usize = 15; // IFNAMSIZ - 1
    if interface.len() > MAX_IFACE_NAME_LEN {
        return Err(CanError::Device(format!(
            "Interface name '{}' is too long (max {} characters)",
            interface, MAX_IFACE_NAME_LEN
        )));
    }

    // 1. 检查接口名是否包含 NUL 字符
    let c_iface = CString::new(interface)
        .map_err(|e| CanError::Device(format!("Invalid interface name: {}", e)))?;

    // 2. 检查接口是否存在
    let ifindex = unsafe { if_nametoindex(c_iface.as_ptr()) };
    if ifindex == 0 {
        let errno = io::Error::last_os_error();
        return Err(CanError::Device(format!(
            "CAN interface '{}' does not exist ({}). Please create it first:\n  sudo ip link add dev {} type can",
            interface, errno, interface
        )));
    }

    // 3. 准备 ifreq 结构
    let mut ifr: ifreq = unsafe { std::mem::zeroed() };
    let c_iface_bytes = interface.as_bytes();

    // 安全地复制接口名到 ifr_name
    unsafe {
        std::ptr::copy_nonoverlapping(
            c_iface_bytes.as_ptr(),
            ifr.ifr_name.as_mut_ptr() as *mut u8,
            c_iface_bytes.len(),
        );
        // 确保以 NUL 结尾
        ifr.ifr_name[c_iface_bytes.len()] = 0;
    }

    // 3. 创建 socket 用于 ioctl
    // 使用 RAII 确保 socket 被正确关闭
    struct FdGuard(libc::c_int);
    impl Drop for FdGuard {
        fn drop(&mut self) {
            if self.0 >= 0 {
                unsafe { libc::close(self.0) };
            }
        }
    }

    let sockfd = unsafe { libc::socket(AF_INET, SOCK_DGRAM, 0) };
    if sockfd < 0 {
        return Err(CanError::Io(io::Error::last_os_error()));
    }
    let _guard = FdGuard(sockfd);

    // 4. 执行 ioctl 获取标志位
    let result = unsafe {
        libc::ioctl(
            sockfd,
            SIOCGIFFLAGS,
            &mut ifr as *mut _ as *mut libc::c_void,
        )
    };

    if result < 0 {
        return Err(CanError::Io(io::Error::last_os_error()));
    }

    // 5. 检查 IFF_UP 标志位
    // 注意：ifreq 结构体使用 union，需要通过 ifr_ifru 访问标志位
    // 在 libc crate 中，ifr_ifru 是一个 union，我们需要通过指针访问其第一个字段（ifru_flags）
    // 根据 Linux 内核定义，ifru_flags 是 union 的第一个字段，类型为 c_short (i16)
    let flags = unsafe {
        // 将 ifr_ifru union 的地址转换为 c_short 指针并解引用
        // 这是安全的，因为 ifru_flags 是 union 的第一个字段，对齐和大小都匹配
        *(std::ptr::addr_of!(ifr.ifr_ifru) as *const libc::c_short)
    };
    let is_up = (flags as i32 & IFF_UP) != 0;

    trace!(
        "Interface '{}' status: {}",
        interface,
        if is_up { "UP" } else { "DOWN" }
    );
    Ok(is_up)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    /// 辅助函数：检查接口是否存在
    fn interface_exists(interface: &str) -> bool {
        Command::new("ip")
            .args(["link", "show", interface])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// 辅助函数：启动接口（需要 sudo）
    fn bring_up_interface(interface: &str) -> bool {
        Command::new("sudo")
            .args(["ip", "link", "set", "up", interface])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// 辅助函数：关闭接口（需要 sudo）
    fn bring_down_interface(interface: &str) -> bool {
        Command::new("sudo")
            .args(["ip", "link", "set", "down", interface])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn test_check_interface_status_exists_and_up() {
        let interface = "vcan0";
        if !interface_exists(interface) {
            eprintln!("Skipping test: {} does not exist", interface);
            return;
        }

        // 确保接口是 UP 状态
        let _ = bring_up_interface(interface);
        std::thread::sleep(std::time::Duration::from_millis(100));

        let result = check_interface_status(interface);
        assert!(
            result.is_ok(),
            "check_interface_status should succeed for existing UP interface"
        );
        assert!(result.unwrap(), "Interface should be UP");
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn test_check_interface_status_exists_but_down() {
        let interface = "vcan0";
        if !interface_exists(interface) {
            eprintln!("Skipping test: {} does not exist", interface);
            return;
        }

        // 关闭接口
        let _ = bring_down_interface(interface);
        std::thread::sleep(std::time::Duration::from_millis(100));

        let result = check_interface_status(interface);
        assert!(
            result.is_ok(),
            "check_interface_status should succeed for existing DOWN interface"
        );
        assert!(!result.unwrap(), "Interface should be DOWN");

        // 恢复接口状态
        let _ = bring_up_interface(interface);
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn test_check_interface_status_not_exists() {
        // 使用一个短的不存在的接口名（不超过 15 字符）
        let result = check_interface_status("can999");
        assert!(
            result.is_err(),
            "check_interface_status should fail for non-existent interface"
        );

        if let Err(CanError::Device(msg)) = result {
            // 错误消息应该包含 "does not exist" 或 "not exist"
            assert!(
                msg.contains("does not exist")
                    || msg.contains("not exist")
                    || msg.contains("does not"),
                "Error message should mention interface does not exist, got: {}",
                msg
            );
            assert!(
                msg.contains("ip link add"),
                "Error message should suggest creating interface, got: {}",
                msg
            );
        } else {
            panic!("Expected Device error, got: {:?}", result);
        }
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn test_check_interface_status_invalid_name() {
        // 测试包含 NUL 字符的接口名（应该失败）
        let invalid_name = "can0\0";
        let result = check_interface_status(invalid_name);
        assert!(
            result.is_err(),
            "check_interface_status should fail for invalid name"
        );

        if let Err(CanError::Device(msg)) = result {
            assert!(
                msg.contains("Invalid interface name"),
                "Error message should mention invalid name"
            );
        } else {
            panic!("Expected Device error, got: {:?}", result);
        }
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn test_check_interface_status_too_long_name() {
        // 测试过长的接口名（IFNAMSIZ = 16，包括 NUL）
        let too_long_name = "a".repeat(20);
        let result = check_interface_status(&too_long_name);
        assert!(
            result.is_err(),
            "check_interface_status should fail for too long name"
        );

        if let Err(CanError::Device(msg)) = result {
            // 错误消息应该包含 "too long" 或 "long"
            assert!(
                msg.contains("too long") || msg.contains("long"),
                "Error message should mention name is too long, got: {}",
                msg
            );
        } else {
            panic!("Expected Device error, got: {:?}", result);
        }
    }
}
