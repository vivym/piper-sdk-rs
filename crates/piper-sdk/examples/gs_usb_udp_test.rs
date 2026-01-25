//! GS-USB UDP/UDS 适配器测试示例
//!
//! 此示例演示如何通过 UDP 或 UDS (Unix Domain Socket) 连接到 gs_usb_daemon 并测试 CAN 总线功能。
//!
//! 使用前请确保：
//! 1. gs_usb_daemon 已经启动（默认 UDP 地址: 127.0.0.1:18888）
//! 2. GS-USB 设备已连接并配置好
//!
//! 运行方式：
//! ```bash
//! # 使用默认 UDP 地址
//! cargo run --example gs_usb_udp_test
//!
//! # 指定 UDP 地址
//! cargo run --example gs_usb_udp_test -- --uds 127.0.0.1:18888
//!
//! # 在 Unix 系统上使用 UDS 路径
//! cargo run --example gs_usb_udp_test -- --uds /tmp/custom_daemon.sock
//! # 或使用 unix: 前缀
//! cargo run --example gs_usb_udp_test -- --uds unix:/tmp/custom_daemon.sock
//! ```
//!

use clap::Parser;
use piper_sdk::can::gs_usb_udp::GsUsbUdpAdapter;
use piper_sdk::can::{CanAdapter, CanError, PiperFrame};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// 命令行参数
#[derive(Parser, Debug)]
#[command(name = "gs_usb_udp_test")]
#[command(about = "测试 GS-USB UDP/UDS 适配器功能")]
struct Args {
    /// UDS Socket 路径或 UDP 地址
    ///
    /// - UDS 路径: 以 `/` 开头（如 `/tmp/gs_usb_daemon.sock`）或使用 `unix:` 前缀
    /// - UDP 地址: IP:PORT 格式（如 `127.0.0.1:18888`）
    ///
    /// 默认: 127.0.0.1:18888 (UDP)
    #[arg(long, default_value = "127.0.0.1:18888")]
    uds: String,

    /// 测试模式
    ///
    /// - send: 只发送测试帧
    /// - receive: 只接收帧（阻塞等待）
    /// - loopback: 发送并接收（需要设备支持 loopback 模式）
    /// - interactive: 交互模式（手动输入命令）
    #[arg(long, default_value = "loopback")]
    mode: String,

    /// 发送帧数量（仅用于 send 和 loopback 模式）
    #[arg(long, default_value = "10")]
    count: u32,

    /// 发送间隔（毫秒）
    #[arg(long, default_value = "100")]
    interval_ms: u64,
}

/// 打印 CAN 帧信息
fn print_frame(label: &str, frame: &PiperFrame) {
    println!(
        "{}: ID=0x{:03X} ({}), Len={}, Data=[{}]",
        label,
        frame.id,
        if frame.is_extended { "EXT" } else { "STD" },
        frame.len,
        frame
            .data_slice()
            .iter()
            .map(|b| format!("{:02X}", b))
            .collect::<Vec<_>>()
            .join(" ")
    );
    if frame.timestamp_us > 0 {
        println!("  时间戳: {} us", frame.timestamp_us);
    }
}

/// 测试发送功能
fn test_send(
    adapter: &mut GsUsbUdpAdapter,
    count: u32,
    interval: Duration,
) -> Result<(), CanError> {
    println!("开始发送测试（发送 {} 帧）...", count);

    for i in 0..count {
        let frame = PiperFrame::new_standard(
            0x123 + (i as u16 % 0x100),
            &[
                i as u8,
                (i * 2) as u8,
                (i * 3) as u8,
                0xAA,
                0xBB,
                0xCC,
                0xDD,
                0xEE,
            ],
        );

        match adapter.send(frame) {
            Ok(_) => {
                print_frame(&format!("发送 #{}", i + 1), &frame);
            },
            Err(e) => {
                eprintln!("发送失败 #{}: {}", i + 1, e);
                return Err(e);
            },
        }

        if i < count - 1 {
            std::thread::sleep(interval);
        }
    }

    println!("✓ 发送测试完成");
    Ok(())
}

/// 测试接收功能
fn test_receive(adapter: &mut GsUsbUdpAdapter, timeout: Duration) -> Result<(), CanError> {
    println!("开始接收测试（超时: {:?}）...", timeout);
    println!("等待接收 CAN 帧...");

    let start = Instant::now();
    let mut received_count = 0;

    loop {
        if start.elapsed() > timeout {
            println!("接收超时（已接收 {} 帧）", received_count);
            break;
        }

        match adapter.receive() {
            Ok(frame) => {
                received_count += 1;
                print_frame(&format!("接收 #{}", received_count), &frame);
            },
            Err(CanError::Timeout) => {
                // 超时是正常的，继续等待
                continue;
            },
            Err(e) => {
                eprintln!("接收错误: {}", e);
                return Err(e);
            },
        }
    }

    if received_count > 0 {
        println!("✓ 接收测试完成（共接收 {} 帧）", received_count);
    } else {
        println!("⚠ 未接收到任何帧");
    }

    Ok(())
}

/// 测试回环功能（发送并接收）
fn test_loopback(
    adapter: &mut GsUsbUdpAdapter,
    count: u32,
    interval: Duration,
    running: Arc<AtomicBool>,
) -> Result<(), CanError> {
    println!("开始回环测试（发送 {} 帧并尝试接收）...", count);
    println!("注意：此测试需要设备支持 loopback 模式或连接到实际的 CAN 总线");

    // 发送并接收帧
    let adapter_for_receive = adapter; // 注意：Rust 不允许同时借用，这里简化处理
    let mut received_frames = Vec::new();

    // 发送帧
    for i in 0..count {
        if !running.load(Ordering::SeqCst) {
            break;
        }

        let frame = PiperFrame::new_standard(
            0x200 + (i as u16 % 0x100),
            &[
                i as u8,
                (i * 2) as u8,
                (i * 3) as u8,
                0x11,
                0x22,
                0x33,
                0x44,
                0x55,
            ],
        );

        match adapter_for_receive.send(frame) {
            Ok(_) => {
                print_frame(&format!("发送 #{}", i + 1), &frame);
            },
            Err(e) => {
                eprintln!("发送失败 #{}: {}", i + 1, e);
                return Err(e);
            },
        }

        // 尝试接收（非阻塞，带超时）
        match adapter_for_receive.receive() {
            Ok(rx_frame) => {
                received_frames.push(rx_frame);
                print_frame(&format!("接收 #{}", received_frames.len()), &rx_frame);
            },
            Err(CanError::Timeout) => {
                // 超时是正常的，继续
            },
            Err(e) => {
                eprintln!("接收错误: {}", e);
            },
        }

        if i < count - 1 {
            std::thread::sleep(interval);
        }
    }

    // 等待额外的时间以接收可能的延迟帧
    println!("\n等待额外帧（最多 2 秒）...");
    let extra_wait_start = Instant::now();
    while extra_wait_start.elapsed() < Duration::from_secs(2) && running.load(Ordering::SeqCst) {
        match adapter_for_receive.receive() {
            Ok(rx_frame) => {
                received_frames.push(rx_frame);
                print_frame(&format!("接收 #{}", received_frames.len()), &rx_frame);
            },
            Err(CanError::Timeout) => {
                // 超时，继续等待
                continue;
            },
            Err(e) => {
                eprintln!("接收错误: {}", e);
                break;
            },
        }
    }

    println!("\n✓ 回环测试完成");
    println!("  发送: {} 帧", count);
    println!("  接收: {} 帧", received_frames.len());

    Ok(())
}

/// 交互模式
fn interactive_mode(
    adapter: &mut GsUsbUdpAdapter,
    running: Arc<AtomicBool>,
) -> Result<(), CanError> {
    println!("进入交互模式");
    println!("可用命令:");
    println!("  send <id> <data...>  - 发送标准帧（例如: send 0x123 01 02 03）");
    println!("  receive              - 接收一帧（阻塞）");
    println!("  status               - 显示连接状态");
    println!("  quit                 - 退出");

    use std::io::{self, BufRead};

    let stdin = io::stdin();
    let mut lines = stdin.lock().lines();

    while running.load(Ordering::SeqCst) {
        print!("> ");
        io::Write::flush(&mut io::stdout()).unwrap();

        let line = match lines.next() {
            Some(Ok(line)) => line,
            Some(Err(e)) => {
                eprintln!("读取输入错误: {}", e);
                continue;
            },
            None => break,
        };

        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.is_empty() {
            continue;
        }

        match parts[0] {
            "send" => {
                if parts.len() < 2 {
                    println!("用法: send <id> [data...]");
                    continue;
                }

                let id = if parts[1].starts_with("0x") || parts[1].starts_with("0X") {
                    u16::from_str_radix(&parts[1][2..], 16)
                } else {
                    parts[1].parse::<u16>()
                };

                let id = match id {
                    Ok(id) => id,
                    Err(e) => {
                        println!("无效的 ID: {}", e);
                        continue;
                    },
                };

                let mut data = Vec::new();
                for part in parts.iter().skip(2) {
                    let byte = if part.starts_with("0x") || part.starts_with("0X") {
                        u8::from_str_radix(&part[2..], 16)
                    } else {
                        part.parse::<u8>()
                    };

                    match byte {
                        Ok(b) => data.push(b),
                        Err(e) => {
                            println!("无效的数据字节: {}", e);
                            continue;
                        },
                    }
                }

                let frame = PiperFrame::new_standard(id, &data);
                match adapter.send(frame) {
                    Ok(_) => {
                        print_frame("发送", &frame);
                    },
                    Err(e) => {
                        eprintln!("发送失败: {}", e);
                    },
                }
            },
            "receive" => {
                println!("等待接收...");
                match adapter.receive() {
                    Ok(frame) => {
                        print_frame("接收", &frame);
                    },
                    Err(CanError::Timeout) => {
                        println!("接收超时");
                    },
                    Err(e) => {
                        eprintln!("接收错误: {}", e);
                    },
                }
            },
            "status" => {
                println!(
                    "连接状态: {}",
                    if adapter.is_connected() {
                        "已连接"
                    } else {
                        "未连接"
                    }
                );
            },
            "quit" | "exit" => {
                println!("退出交互模式");
                break;
            },
            _ => {
                println!("未知命令: {}", parts[0]);
            },
        }
    }

    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    // 设置 Ctrl+C 处理
    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();
    ctrlc::set_handler(move || {
        r.store(false, Ordering::SeqCst);
        println!("\n收到退出信号，正在关闭...");
    })?;

    println!("GS-USB UDP/UDS 适配器测试");
    println!("================================");
    println!(
        "地址: {} ({})",
        args.uds,
        if args.uds.starts_with('/') || args.uds.starts_with("unix:") {
            "UDS"
        } else {
            "UDP"
        }
    );
    println!("测试模式: {}", args.mode);
    println!();

    // 1. 创建适配器（根据地址格式自动选择 UDS 或 UDP）
    println!("正在创建适配器...");
    let mut adapter = if args.uds.starts_with('/') || args.uds.starts_with("unix:") {
        // UDS 模式
        #[cfg(unix)]
        {
            let path = args.uds.strip_prefix("unix:").unwrap_or(&args.uds);
            GsUsbUdpAdapter::new_uds(path).map_err(|e| format!("创建 UDS 适配器失败: {}", e))?
        }
        #[cfg(not(unix))]
        {
            return Err("Unix Domain Sockets are not supported on this platform. Please use UDP address format (e.g., 127.0.0.1:18888)".into());
        }
    } else {
        // UDP 模式
        GsUsbUdpAdapter::new_udp(&args.uds).map_err(|e| format!("创建 UDP 适配器失败: {}", e))?
    };
    println!("✓ 适配器创建成功");

    // 2. 连接到守护进程
    println!("正在连接到守护进程...");
    match adapter.connect(vec![]) {
        Ok(_) => {
            println!("✓ 连接成功");
        },
        Err(e) => {
            eprintln!("✗ 连接失败: {}", e);
            eprintln!();
            eprintln!("请确保:");
            eprintln!("  1. gs_usb_daemon 已经启动");
            eprintln!("  2. 地址正确: {}", args.uds);
            eprintln!("  3. 守护进程正在监听该地址");
            return Err(Box::new(e));
        },
    }

    // 3. 检查连接状态
    if !adapter.is_connected() {
        return Err("连接状态异常".into());
    }

    println!();

    // 4. 根据模式执行测试
    let interval = Duration::from_millis(args.interval_ms);
    match args.mode.as_str() {
        "send" => {
            test_send(&mut adapter, args.count, interval)?;
        },
        "receive" => {
            test_receive(&mut adapter, Duration::from_secs(10))?;
        },
        "loopback" => {
            test_loopback(&mut adapter, args.count, interval, running.clone())?;
        },
        "interactive" => {
            interactive_mode(&mut adapter, running.clone())?;
        },
        _ => {
            return Err(format!("未知的测试模式: {}", args.mode).into());
        },
    }

    println!();
    println!("✓ 测试完成");

    Ok(())
}
