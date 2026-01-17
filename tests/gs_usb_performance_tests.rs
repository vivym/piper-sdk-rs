//! GS-USB 性能测试
//!
//! 需要实际 GS-USB 硬件才能运行。
//!
//! ⚠️ **重要**：设备是独占的，必须串行运行测试（--test-threads=1）
//!
//! 运行方式：
//! ```bash
//! # 串行运行所有测试
//! cargo test --test gs_usb_performance_tests -- --ignored --test-threads=1
//! ```

use piper_sdk::can::gs_usb::GsUsbCanAdapter;
use piper_sdk::can::{CanAdapter, PiperFrame};

/// 测试 1kHz 发送性能（Fire-and-Forget）
#[test]
#[ignore]
fn test_1khz_send_performance() {
    let mut adapter = GsUsbCanAdapter::new().expect("Failed to create adapter");
    adapter.configure(500_000).expect("Failed to configure");

    let frame = PiperFrame::new_standard(0x123, &[0x01, 0x02, 0x03, 0x04]);

    let start = std::time::Instant::now();
    let mut count = 0;

    // 发送帧（1秒内，目标是 1kHz = 1000 fps）
    while start.elapsed().as_millis() < 1000 {
        adapter.send(frame).expect("Send failed");
        count += 1;
    }

    let elapsed = start.elapsed();
    let fps = count as f64 / elapsed.as_secs_f64();

    println!("Sent {} frames in {:?} ({:.1} fps)", count, elapsed, fps);

    // Fire-and-Forget 应该能达到至少 800 fps（允许 20% 误差）
    // 如果等待 Echo，这个数字会大幅下降
    assert!(
        count >= 800,
        "Performance too low: {} fps (expected >= 800)",
        count
    );
}

/// 测试发送延迟（单帧发送时间）
#[test]
#[ignore]
fn test_send_latency() {
    let mut adapter = GsUsbCanAdapter::new().expect("Failed to create adapter");
    adapter.configure(500_000).expect("Failed to configure");

    let frame = PiperFrame::new_standard(0x123, &[0x01, 0x02, 0x03, 0x04]);

    // 测量单帧发送时间（Fire-and-Forget）
    let mut latencies = Vec::new();
    for _ in 0..100 {
        let start = std::time::Instant::now();
        adapter.send(frame).expect("Send failed");
        let elapsed = start.elapsed();
        latencies.push(elapsed.as_micros());
    }

    let avg_latency = latencies.iter().sum::<u128>() as f64 / latencies.len() as f64;
    let min_latency = *latencies.iter().min().unwrap();
    let max_latency = *latencies.iter().max().unwrap();

    println!(
        "Send latency: avg={:.2}µs, min={}µs, max={}µs",
        avg_latency, min_latency, max_latency
    );

    // Fire-and-Forget 应该在微秒级（< 1ms）
    assert!(
        avg_latency < 1000.0,
        "Average latency too high: {:.2}µs",
        avg_latency
    );
}

/// 测试接收延迟（超时场景）
#[test]
#[ignore]
fn test_receive_timeout_latency() {
    let mut adapter = GsUsbCanAdapter::new().expect("Failed to create adapter");
    adapter.configure(250_000).expect("Failed to configure");

    // 测量超时延迟（应该是约 2ms，基于代码中的超时设置）
    let start = std::time::Instant::now();
    match adapter.receive() {
        Err(piper_sdk::can::CanError::Timeout) => {
            let elapsed = start.elapsed();
            println!("Timeout latency: {:?}", elapsed);

            // 超时应该在 2-10ms 范围内（允许一些抖动）
            assert!(
                elapsed.as_millis() >= 1 && elapsed.as_millis() < 100,
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

/// 测试批量发送性能（连续发送 1000 帧）
#[test]
#[ignore]
fn test_batch_send_performance() {
    let mut adapter = GsUsbCanAdapter::new().expect("Failed to create adapter");
    adapter.configure(500_000).expect("Failed to configure");

    let frame = PiperFrame::new_standard(0x123, &[0x01, 0x02, 0x03, 0x04]);
    let batch_size = 1000;

    let start = std::time::Instant::now();
    for _ in 0..batch_size {
        adapter.send(frame).expect("Send failed");
    }
    let elapsed = start.elapsed();

    let fps = batch_size as f64 / elapsed.as_secs_f64();
    println!(
        "Sent {} frames in {:?} ({:.1} fps)",
        batch_size, elapsed, fps
    );

    // 批量发送应该能达到较高的吞吐量
    assert!(
        fps >= 500.0,
        "Batch send performance too low: {:.1} fps",
        fps
    );
}
