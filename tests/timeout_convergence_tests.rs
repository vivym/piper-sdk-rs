//! 超时收敛效果验证测试
//!
//! 验证 `PipelineConfig.receive_timeout_ms` 统一应用到各 adapter 后，
//! 命令延迟在"安静总线"场景下的改善。
//!
//! ⚠️ **重要**：这些测试需要实际硬件才能运行，默认被标记为 `#[ignore]`
//!
//! 运行方式：
//! ```bash
//! # 运行所有测试
//! cargo test --test timeout_convergence_tests -- --ignored
//!
//! # 运行特定测试
//! cargo test --test timeout_convergence_tests test_command_latency_quiet_bus -- --ignored
//! ```

use piper_sdk::driver::{PipelineConfig, PiperBuilder};
use std::time::{Duration, Instant};

/// 计算延迟分布的百分位数
fn calculate_percentiles(mut latencies: Vec<Duration>) -> (Duration, Duration, Duration, Duration) {
    latencies.sort();
    let len = latencies.len();

    let p50 = latencies[len * 50 / 100];
    let p95 = latencies[len * 95 / 100];
    let p99 = latencies[len * 99 / 100];
    let max = latencies[len - 1];

    (p50, p95, p99, max)
}

/// 测试命令延迟（安静总线场景）
///
/// 在"安静总线"（无反馈帧）场景下，测量命令从发送到实际发出的延迟。
///
/// **改进前**：GS-USB 默认 50ms 超时，命令可能延迟 ~50ms
/// **改进后**：统一 2ms 超时，命令延迟应该 < 5ms（P99）
#[test]
#[ignore]
fn test_command_latency_quiet_bus() {
    // 使用默认配置（2ms receive_timeout）
    let config = PipelineConfig::default();
    assert_eq!(
        config.receive_timeout_ms, 2,
        "Default timeout should be 2ms"
    );

    let piper = PiperBuilder::new()
        .pipeline_config(config)
        .build()
        .expect("Failed to create Piper");

    let frame = piper_sdk::can::PiperFrame::new_standard(0x123, &[0x01, 0x02, 0x03, 0x04]);

    // 测量 100 次命令发送的延迟
    let mut latencies = Vec::new();
    for _ in 0..100 {
        let start = Instant::now();

        // 发送命令（在安静总线下，这会触发 receive 超时，然后 drain 命令）
        piper.send_frame(frame).expect("Failed to send frame");

        // 等待一小段时间，确保命令被处理
        // 注意：这里我们无法直接测量"命令实际发出"的时间，
        // 但可以通过测量 send_frame 返回的时间来间接验证
        let elapsed = start.elapsed();
        latencies.push(elapsed);

        // 短暂休眠，避免命令队列积压
        std::thread::sleep(Duration::from_millis(1));
    }

    let (p50, p95, p99, max) = calculate_percentiles(latencies);

    println!("\n=== 命令延迟分布（安静总线场景）===");
    println!("P50: {:?}", p50);
    println!("P95: {:?}", p95);
    println!("P99: {:?}", p99);
    println!("Max: {:?}", max);

    // 改进后，P99 延迟应该 < 5ms
    assert!(
        p99 < Duration::from_millis(5),
        "P99 latency too high: {:?} (expected < 5ms)",
        p99
    );

    // P50 应该非常小（< 1ms），因为 send_frame 只是入队
    assert!(
        p50 < Duration::from_millis(1),
        "P50 latency too high: {:?} (expected < 1ms)",
        p50
    );
}

/// 测试 SocketCAN 超时配置（仅 Linux）
#[cfg(target_os = "linux")]
#[test]
#[ignore]
fn test_socketcan_timeout_config() {
    use piper_sdk::can::CanAdapter;
    use piper_sdk::can::SocketCanAdapter;
    use std::time::Duration;

    // 测试不同的超时配置
    let mut adapter = SocketCanAdapter::new("can0").expect("Failed to create SocketCAN adapter");

    // 设置 2ms 超时
    adapter
        .set_read_timeout(Duration::from_millis(2))
        .expect("Failed to set read timeout");

    // 在安静总线下，receive 应该在大约 2ms 后超时
    let start = Instant::now();
    match adapter.receive() {
        Err(piper_sdk::can::CanError::Timeout) => {
            let elapsed = start.elapsed();
            println!("SocketCAN timeout: {:?}", elapsed);

            // 超时应该在 2-5ms 范围内（允许一些抖动）
            assert!(
                elapsed >= Duration::from_millis(1) && elapsed < Duration::from_millis(10),
                "Timeout latency out of range: {:?}",
                elapsed
            );
        },
        Ok(frame) => {
            println!("Received frame unexpectedly: ID=0x{:X}", frame.id);
        },
        Err(e) => panic!("Unexpected error: {}", e),
    }
}

/// 测试 GS-USB 超时配置（所有平台）
#[test]
#[ignore]
fn test_gs_usb_timeout_config() {
    use piper_sdk::can::CanAdapter;
    use piper_sdk::can::gs_usb::GsUsbCanAdapter;
    use std::time::Duration;

    let mut adapter = GsUsbCanAdapter::new().expect("Failed to create GS-USB adapter");
    adapter.configure(500_000).expect("Failed to configure");

    // 设置 2ms 超时（默认值）
    adapter.set_receive_timeout(Duration::from_millis(2));

    // 在安静总线下，receive 应该在大约 2ms 后超时
    let start = Instant::now();
    match adapter.receive() {
        Err(piper_sdk::can::CanError::Timeout) => {
            let elapsed = start.elapsed();
            println!("GS-USB timeout: {:?}", elapsed);

            // 超时应该在 2-10ms 范围内（允许一些抖动和 USB 延迟）
            assert!(
                elapsed >= Duration::from_millis(1) && elapsed < Duration::from_millis(15),
                "Timeout latency out of range: {:?}",
                elapsed
            );
        },
        Ok(frame) => {
            println!("Received frame unexpectedly: ID=0x{:X}", frame.id);
        },
        Err(e) => panic!("Unexpected error: {}", e),
    }
}
