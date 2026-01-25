//! 示例：测试 SocketCAN iface 状态检测能力
//!
//! 用法：
//! - 默认检查 can0：
//!   cargo run --example iface_check
//! - 指定接口名：
//!   cargo run --example iface_check -- vcan0
//! - 指定轮询间隔（毫秒）：
//!   cargo run --example iface_check -- vcan0 500
//!
//! 你可以在另一个终端手动切换接口状态，然后观察此程序输出：
//!   sudo ip link set up can0
//!   sudo ip link set down can0

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("该示例仅支持 Linux（SocketCAN）。");
}

#[cfg(target_os = "linux")]
fn main() {
    use piper_sdk::can::SocketCanAdapter;
    use std::env;
    use std::time::Duration;

    let mut args = env::args().skip(1);
    let iface = args.next().unwrap_or_else(|| "can0".to_string());
    let interval_ms: u64 = args.next().as_deref().unwrap_or("1000").parse().unwrap_or(1000);

    println!("=== SocketCAN iface 检测示例 ===");
    println!("- iface: {}", iface);
    println!("- interval: {} ms", interval_ms);
    println!();
    println!("提示：在另一个终端执行：");
    println!("  sudo ip link set up {}", iface);
    println!("  sudo ip link set down {}", iface);
    println!();

    loop {
        // 关键点：SocketCanAdapter::new() 内部会先做 iface 存在性 + UP 状态检查
        // 这正是我们要验证的能力。
        match SocketCanAdapter::new(&iface) {
            Ok(_adapter) => {
                // 仅用于检测：创建成功即可，立刻 drop
                println!("[OK ] iface '{}' is UP (adapter created)", iface);
            },
            Err(e) => {
                println!("[ERR] iface '{}' not ready: {}", iface, e);
            },
        }

        std::thread::sleep(Duration::from_millis(interval_ms));
    }
}
