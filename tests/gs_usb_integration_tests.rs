//! GS-USB 集成测试
//!
//! 需要实际 GS-USB 硬件才能运行。
//!
//! ⚠️ **重要**：设备是独占的，必须串行运行测试（--test-threads=1）
//!
//! 运行方式：
//! ```bash
//! # 串行运行所有测试
//! cargo test --test gs_usb_integration_tests -- --ignored --test-threads=1
//! ```

use piper_sdk::can::gs_usb::GsUsbCanAdapter;
use piper_sdk::can::{CanAdapter, PiperFrame};

/// 测试 CAN 适配器基本功能
#[test]
#[ignore]
fn test_can_adapter_basic() {
    // 1. 创建适配器
    let mut adapter = GsUsbCanAdapter::new().expect("Failed to create adapter");
    println!("✓ Adapter created");

    // 2. 配置并启动
    adapter.configure(250_000).expect("Failed to configure adapter");
    println!("✓ Adapter configured (250 kbps)");

    // 3. 发送测试帧
    let tx_frame = PiperFrame::new_standard(0x123, &[0x01, 0x02, 0x03, 0x04]);
    adapter.send(tx_frame).expect("Failed to send frame");
    println!("✓ Frame sent via CanAdapter");

    println!("✓ Basic test completed");
}

/// 测试 Fire-and-Forget 语义（发送不阻塞）
#[test]
#[ignore]
fn test_send_fire_and_forget() {
    let mut adapter = GsUsbCanAdapter::new().expect("Failed to create adapter");
    adapter.configure(250_000).expect("Failed to configure");

    let frame = PiperFrame::new_standard(0x123, &[0x01, 0x02]);

    // 连续快速发送，验证不会阻塞
    let start = std::time::Instant::now();
    for _ in 0..100 {
        adapter.send(frame).expect("Send failed");
    }
    let elapsed = start.elapsed();

    println!("Sent 100 frames in {:?}", elapsed);
    // Fire-and-Forget 应该在毫秒级完成（不等待 Echo）
    assert!(elapsed.as_millis() < 1000, "Send blocked too long");
}

/// 测试三层过滤漏斗（接收逻辑）
#[test]
#[ignore]
fn test_receive_filter_funnel() {
    let mut adapter = GsUsbCanAdapter::new().expect("Failed to create adapter");
    adapter.configure(250_000).expect("Failed to configure");

    // 测试：receive() 应该过滤掉 TX Echo
    // 注意：这需要设备实际发送 Echo 回显
    // 在没有外部 CAN 信号的情况下，主要测试超时处理

    let start = std::time::Instant::now();
    match adapter.receive() {
        Ok(frame) => {
            println!("Received frame: ID=0x{:X}, len={}", frame.id, frame.len);
        },
        Err(piper_sdk::can::CanError::Timeout) => {
            println!("✓ Timeout as expected (no CAN traffic)");
            // 超时是正常的，因为没有实际的 CAN 总线数据
        },
        Err(e) => panic!("Unexpected error: {}", e),
    }

    let elapsed = start.elapsed();
    println!("Receive attempt took {:?}", elapsed);
    // 应该快速超时（约 2ms，基于代码中的超时设置）
    assert!(elapsed.as_millis() < 100, "Receive blocked too long");
}

/// 测试错误处理：设备未启动时发送
#[test]
#[ignore]
fn test_send_not_started() {
    let mut adapter = GsUsbCanAdapter::new().expect("Failed to create adapter");
    // 不调用 configure()

    let frame = PiperFrame::new_standard(0x123, &[0x01]);
    let result = adapter.send(frame);

    match result {
        Err(piper_sdk::can::CanError::NotStarted) => {
            println!("✓ Correctly returned NotStarted error");
        },
        Ok(_) => panic!("Expected NotStarted error, but send succeeded"),
        Err(e) => panic!("Expected NotStarted, got: {}", e),
    }
}
