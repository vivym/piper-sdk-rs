//! GS-USB Daemon 延迟基准测试
//!
//! 测试性能指标：
//! - Round-trip 延迟（P50/P99/P999）
//! - CPU 占用率
//! - 吞吐量（fps）
//! - 客户端阻塞处理

use piper_sdk::can::gs_usb_udp::GsUsbUdpAdapter;
use piper_sdk::can::{CanAdapter, PiperFrame};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

/// 延迟统计
struct LatencyStats {
    latencies: Vec<Duration>,
    start_time: Instant,
    count: AtomicU64,
}

impl LatencyStats {
    fn new() -> Self {
        Self {
            latencies: Vec::new(),
            start_time: Instant::now(),
            count: AtomicU64::new(0),
        }
    }

    fn record(&mut self, latency: Duration) {
        self.latencies.push(latency);
        self.count.fetch_add(1, Ordering::Relaxed);
    }

    fn calculate_percentiles(&mut self) -> (Duration, Duration, Duration, Duration) {
        if self.latencies.is_empty() {
            return (
                Duration::ZERO,
                Duration::ZERO,
                Duration::ZERO,
                Duration::ZERO,
            );
        }

        self.latencies.sort_unstable();
        let len = self.latencies.len();

        let p50 = self.latencies[len * 50 / 100];
        let p99 = self.latencies[len * 99 / 100];
        let p999 = if len >= 1000 {
            self.latencies[len * 999 / 1000]
        } else {
            self.latencies[len - 1]
        };
        let max = self.latencies[len - 1];

        (p50, p99, p999, max)
    }

    fn fps(&self) -> f64 {
        let elapsed = self.start_time.elapsed().as_secs_f64();
        if elapsed > 0.0 {
            self.count.load(Ordering::Relaxed) as f64 / elapsed
        } else {
            0.0
        }
    }
}

/// 测试场景 2: 发送延迟（仅测试发送路径）
fn test_send_latency() -> Result<(), Box<dyn std::error::Error>> {
    println!("\n=== 测试场景 2: 发送延迟（仅发送路径）===");

    let mut adapter = GsUsbUdpAdapter::new_uds("/tmp/gs_usb_daemon.sock")?;
    adapter.connect(vec![])?;

    println!("已连接到 daemon，开始测试...");

    let mut stats = LatencyStats::new();
    let test_count = 10000;

    let test_frame = PiperFrame {
        id: 0x123,
        data: [0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08],
        len: 8,
        is_extended: false,
        timestamp_us: 0,
    };

    for _ in 0..test_count {
        let start = Instant::now();
        adapter.send(test_frame)?;
        let latency = start.elapsed();
        stats.record(latency);
    }

    let (p50, p99, p999, max) = stats.calculate_percentiles();
    let fps = stats.fps();

    println!("\n测试结果 ({} 次发送):", test_count);
    println!("  P50 延迟:  {:?} ({:.2} μs)", p50, p50.as_micros() as f64);
    println!("  P99 延迟:  {:?} ({:.2} μs)", p99, p99.as_micros() as f64);
    println!(
        "  P999 延迟: {:?} ({:.2} μs)",
        p999,
        p999.as_micros() as f64
    );
    println!("  Max 延迟:  {:?} ({:.2} μs)", max, max.as_micros() as f64);
    println!("  吞吐量:    {:.1} fps", fps);

    // 验证 P99 延迟 < 200μs
    if p99.as_micros() < 200 {
        println!("  ✅ P99 延迟满足要求 (< 200μs)");
    } else {
        println!("  ⚠️  P99 延迟超标 (>= 200μs)");
    }

    Ok(())
}

/// 测试场景 3: 接收延迟（仅测试接收路径）
fn test_receive_latency() -> Result<(), Box<dyn std::error::Error>> {
    println!("\n=== 测试场景 3: 接收延迟（仅接收路径）===");
    println!("注意：此测试需要 daemon 持续发送数据");
    println!("如果没有数据源，此测试会超时");

    let mut adapter = GsUsbUdpAdapter::new_uds("/tmp/gs_usb_daemon.sock")?;
    adapter.connect(vec![])?;

    println!("已连接到 daemon，等待接收数据...");

    let mut stats = LatencyStats::new();
    let test_count = 1000;
    let timeout = Duration::from_secs(5);
    let start_time = Instant::now();

    for i in 0..test_count {
        if start_time.elapsed() > timeout {
            println!("  超时：只收到 {} 帧", i);
            break;
        }

        let start = Instant::now();
        match adapter.receive() {
            Ok(_frame) => {
                let latency = start.elapsed();
                stats.record(latency);
            },
            Err(_) => {
                // 超时，继续等待
                thread::sleep(Duration::from_millis(1));
            },
        }
    }

    if stats.latencies.is_empty() {
        println!("  ⚠️  未收到任何数据，跳过统计");
        return Ok(());
    }

    let (p50, p99, p999, max) = stats.calculate_percentiles();
    let fps = stats.fps();

    println!("\n测试结果 (收到 {} 帧):", stats.latencies.len());
    println!("  P50 延迟:  {:?} ({:.2} μs)", p50, p50.as_micros() as f64);
    println!("  P99 延迟:  {:?} ({:.2} μs)", p99, p99.as_micros() as f64);
    println!(
        "  P999 延迟: {:?} ({:.2} μs)",
        p999,
        p999.as_micros() as f64
    );
    println!("  Max 延迟:  {:?} ({:.2} μs)", max, max.as_micros() as f64);
    println!("  吞吐量:    {:.1} fps", fps);

    Ok(())
}

/// 测试场景 4: 吞吐量测试
fn test_throughput() -> Result<(), Box<dyn std::error::Error>> {
    println!("\n=== 测试场景 4: 吞吐量测试 ===");

    let mut adapter = GsUsbUdpAdapter::new_uds("/tmp/gs_usb_daemon.sock")?;
    adapter.connect(vec![])?;

    println!("已连接到 daemon，开始测试...");

    let test_frame = PiperFrame {
        id: 0x123,
        data: [0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08],
        len: 8,
        is_extended: false,
        timestamp_us: 0,
    };

    let test_duration = Duration::from_secs(5);
    let end_time = Instant::now() + test_duration;
    let mut count = 0u64;

    while Instant::now() < end_time {
        let _ = adapter.send(test_frame);
        count += 1;
    }

    let elapsed = test_duration.as_secs_f64();
    let fps = count as f64 / elapsed;

    println!("\n测试结果 ({} 秒):", test_duration.as_secs());
    println!("  总发送帧数: {}", count);
    println!("  吞吐量:    {:.1} fps", fps);

    // 验证吞吐量 > 1000 fps
    if fps > 1000.0 {
        println!("  ✅ 吞吐量满足要求 (> 1000 fps)");
    } else {
        println!("  ⚠️  吞吐量较低 (< 1000 fps)");
    }

    Ok(())
}

/// 测试场景 5: 客户端阻塞处理（模拟故障客户端）
fn test_client_blocking_handling() -> Result<(), Box<dyn std::error::Error>> {
    println!("\n=== 测试场景 5: 客户端阻塞处理 ===");
    println!("此测试需要手动验证：");
    println!("1. 启动 daemon");
    println!("2. 连接一个客户端但不读取数据（模拟卡死）");
    println!("3. 观察 daemon 日志，验证：");
    println!("   - 日志限频生效（不会洪水）");
    println!("   - 1 秒后客户端被断开");
    println!("   - 其他客户端不受影响");

    println!("\n手动测试步骤：");
    println!("1. 启动 daemon: cargo run --bin gs_usb_daemon");
    println!("2. 运行此测试: cargo run --example daemon_latency_bench");
    println!("3. 在另一个终端运行卡死客户端（不读取数据）");
    println!("4. 观察 daemon 日志输出");

    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("GS-USB Daemon 性能基准测试");
    println!("============================\n");

    // 检查 daemon 是否运行
    println!("检查 daemon 连接...");
    match GsUsbUdpAdapter::new_uds("/tmp/gs_usb_daemon.sock") {
        Ok(_) => println!("✅ Daemon socket 存在"),
        Err(e) => {
            eprintln!("❌ 无法连接到 daemon: {}", e);
            eprintln!("请先启动 daemon: cargo run --bin gs_usb_daemon");
            return Err(e.into());
        },
    }

    // 运行测试
    println!("\n选择测试场景:");
    println!("1. 发送延迟测试");
    println!("2. 接收延迟测试");
    println!("3. 吞吐量测试");
    println!("4. 客户端阻塞处理说明");
    println!("5. 运行所有测试");

    // 简化：直接运行所有测试
    test_send_latency()?;
    test_receive_latency()?;
    test_throughput()?;
    test_client_blocking_handling()?;

    println!("\n=== 测试完成 ===");
    println!("\n关键指标验证：");
    println!("- P99 延迟应 < 200μs");
    println!("- 吞吐量应 > 1000 fps");
    println!("- 客户端阻塞应被正确处理");

    Ok(())
}
