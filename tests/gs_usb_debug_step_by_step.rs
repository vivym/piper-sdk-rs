//! 逐步调试 GS-USB 设备初始化

use piper_sdk::can::gs_usb::device::GsUsbDevice;

#[test]
#[ignore]
fn debug_initialization_step_by_step() {
    println!("=== Step-by-Step Initialization Debug ===\n");

    // Step 1: Scan devices
    println!("Step 1: Scanning devices...");
    let mut devices = match GsUsbDevice::scan() {
        Ok(devs) => {
            println!("  ✓ Found {} device(s)", devs.len());
            devs
        }
        Err(e) => {
            println!("  ✗ Scan failed: {}", e);
            return;
        }
    };

    if devices.is_empty() {
        println!("  ✗ No devices found");
        return;
    }

    let mut device = devices.remove(0);
    println!("  ✓ Device opened\n");

    // Step 2: Prepare interface
    println!("Step 2: Preparing interface...");
    match device.prepare_interface() {
        Ok(_) => println!("  ✓ Interface prepared (detached driver, claimed interface)"),
        Err(e) => {
            println!("  ✗ Failed to prepare interface: {}", e);
            return;
        }
    }

    // Step 3: Send HOST_FORMAT
    println!("\nStep 3: Sending HOST_FORMAT...");
    match device.send_host_format() {
        Ok(_) => println!("  ✓ HOST_FORMAT sent (errors ignored)"),
        Err(e) => println!("  ⚠ HOST_FORMAT error (expected, ignored): {}", e),
    }

    // Step 4: Get device capability (this is called inside set_bitrate)
    println!("\nStep 4: Getting device capability...");
    match device.device_capability() {
        Ok(cap) => {
            println!("  ✓ Capability retrieved:");
            println!("    - Clock: {} Hz", cap.fclk_can);
            println!("    - Features: 0x{:08X}", cap.feature);
        }
        Err(e) => {
            println!("  ✗ Failed to get capability: {}", e);
            println!("    This is where 'Pipe error' occurs!");
            return;
        }
    }

    // Step 5: Set bitrate
    println!("\nStep 5: Setting bitrate (250000 bps)...");
    match device.set_bitrate(250_000) {
        Ok(_) => println!("  ✓ Bitrate set successfully"),
        Err(e) => {
            println!("  ✗ Failed to set bitrate: {}", e);
            return;
        }
    }

    // Step 6: Start device
    println!("\nStep 6: Starting device (LOOP_BACK mode)...");
    match device.start(piper_sdk::can::gs_usb::protocol::GS_CAN_MODE_LOOP_BACK) {
        Ok(_) => println!("  ✓ Device started successfully"),
        Err(e) => {
            println!("  ✗ Failed to start device: {}", e);
            return;
        }
    }

    println!("\n=== All steps completed successfully! ===");
}

