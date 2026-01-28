//! macOS Quality of Service (QoS) 设置
//!
//! 用于设置线程优先级，确保关键线程运行在 P-core（性能核）上
//!
//! 参考：`daemon_implementation_plan.md` 第 4.1.4 节

#[cfg(target_os = "macos")]
mod imp {
    use std::os::raw::{c_int, c_void};

    #[allow(non_camel_case_types)]
    type pthread_t = *mut c_void;
    #[allow(non_camel_case_types)]
    type qos_class_t = c_int;

    // macOS QoS 级别定义
    const QOS_CLASS_USER_INTERACTIVE: qos_class_t = 0x21; // 最高实时性优先级（用于 USB Rx/Tx 线程）

    // 备用 QoS 级别，当前未使用但保留以备将来需要
    #[allow(dead_code)]
    const QOS_CLASS_USER_INITIATED: qos_class_t = 0x19; // 用户发起的任务（中等优先级）
    #[allow(dead_code)]
    const QOS_CLASS_DEFAULT: qos_class_t = 0x15; // 默认优先级
    const QOS_CLASS_UTILITY: qos_class_t = 0x11; // 低优先级（用于设备管理线程）
    #[allow(dead_code)]
    const QOS_CLASS_BACKGROUND: qos_class_t = 0x09; // 后台任务（最低优先级）

    unsafe extern "C" {
        fn pthread_self() -> pthread_t;
        fn pthread_set_qos_class_np(
            thread: pthread_t,
            qos_class: qos_class_t,
            relative_priority: c_int,
        ) -> c_int;
    }

    /// 设置当前线程为高优先级（User Interactive）
    ///
    /// **作用**：
    /// - 告诉 macOS 调度器："这个线程在处理实时硬件通信，必须放在 P-core (大核)"
    /// - 避免被调度到 E-core (能效核)，导致延迟波动
    ///
    /// **调用时机**：在每个 IO 线程（USB Rx, IPC Rx）的开头调用
    pub fn set_high_priority() {
        unsafe {
            let result = pthread_set_qos_class_np(
                pthread_self(),
                QOS_CLASS_USER_INTERACTIVE,
                0, // relative_priority: 0 = 默认相对优先级
            );

            if result != 0 {
                eprintln!("Warning: Failed to set thread QoS (error: {})", result);
            }
        }
    }

    /// 设置当前线程为低优先级（Utility）
    ///
    /// **用途**：设备管理线程等非实时任务
    pub fn set_low_priority() {
        unsafe {
            let _ = pthread_set_qos_class_np(pthread_self(), QOS_CLASS_UTILITY, 0);
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    /// 非 macOS 平台，QoS 设置为空操作
    pub fn set_high_priority() {}
    pub fn set_low_priority() {}
}

pub use imp::{set_high_priority, set_low_priority};

#[cfg(test)]
#[cfg(target_os = "macos")]
mod tests {
    use super::*;

    #[test]
    fn test_set_high_priority() {
        // 验证函数可以调用（不 panic）
        set_high_priority();
        // 注意：无法直接验证优先级，但可以验证函数执行成功
    }

    #[test]
    fn test_set_low_priority() {
        // 验证函数可以调用（不 panic）
        set_low_priority();
    }
}

#[cfg(test)]
#[cfg(not(target_os = "macos"))]
mod tests {
    use super::*;

    #[test]
    fn test_set_high_priority_noop() {
        // 在非 macOS 平台上，应该是空操作
        set_high_priority();
    }

    #[test]
    fn test_set_low_priority_noop() {
        // 在非 macOS 平台上，应该是空操作
        set_low_priority();
    }
}
