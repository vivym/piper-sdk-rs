//! GS-USB 设备扫描诊断工具
//!
//! 用于诊断设备扫描和初始化问题
//!
//! 运行方式：`cargo test --test gs_usb_debug_scan -- --ignored --nocapture`

use rusb::{Context, UsbContext};

#[test]
#[ignore]
fn debug_scan_all_usb_devices() {
    println!("=== Scanning all USB devices ===");

    let context = Context::new().expect("Failed to create USB context");

    for device in context.devices().expect("Failed to get device list").iter() {
        let desc = match device.device_descriptor() {
            Ok(d) => d,
            Err(e) => {
                println!("⚠ Failed to read device descriptor: {}", e);
                continue;
            },
        };

        let vid = desc.vendor_id();
        let pid = desc.product_id();

        println!("\nDevice:");
        println!("  VID: 0x{:04X}, PID: 0x{:04X}", vid, pid);
        println!(
            "  Class: 0x{:02X}, SubClass: 0x{:02X}",
            desc.class_code(),
            desc.sub_class_code()
        );

        // 检查是否是GS-USB设备
        let is_gs_usb = matches!(
            (vid, pid),
            (0x1D50, 0x606F)   // GS-USB
                | (0x1209, 0x2323)  // Candlelight
                | (0x1CD2, 0x606F)  // CES CANext FD
                | (0x16D0, 0x10B8) // ABE CANdebugger FD
        );

        if is_gs_usb {
            println!("  ✅ GS-USB device detected!");

            // 尝试打开设备
            match device.open() {
                Ok(handle) => {
                    println!("  ✅ Device opened successfully");

                    // 尝试读取配置描述符
                    match device.config_descriptor(0) {
                        Ok(config) => {
                            println!("  ✅ Configuration descriptor read");

                            for interface in config.interfaces() {
                                println!("    Interface: {}", interface.number());
                                for iface_desc in interface.descriptors() {
                                    println!(
                                        "      Class: 0x{:02X}, SubClass: 0x{:02X}",
                                        iface_desc.class_code(),
                                        iface_desc.sub_class_code()
                                    );

                                    // 检查端点
                                    for endpoint in iface_desc.endpoint_descriptors() {
                                        let addr = endpoint.address();
                                        let dir = if endpoint.direction() == rusb::Direction::In {
                                            "IN"
                                        } else {
                                            "OUT"
                                        };
                                        let transfer = format!("{:?}", endpoint.transfer_type());
                                        println!(
                                            "        Endpoint: 0x{:02X} ({}, {})",
                                            addr, dir, transfer
                                        );
                                    }
                                }
                            }

                            // 尝试声明接口
                            match handle.claim_interface(0) {
                                Ok(_) => {
                                    println!("  ✅ Interface 0 claimed successfully");
                                    let _ = handle.release_interface(0);
                                },
                                Err(e) => {
                                    println!("  ❌ Failed to claim interface 0: {}", e);
                                },
                            }
                        },
                        Err(e) => {
                            println!("  ❌ Failed to read config descriptor: {}", e);
                        },
                    }
                },
                Err(e) => {
                    println!("  ❌ Failed to open device: {}", e);
                    if e.to_string().contains("Access denied")
                        || e.to_string().contains("permission")
                    {
                        println!("  💡 This is likely a permissions issue on macOS.");
                        println!("  💡 You may need to:");
                        println!(
                            "     1. Grant Terminal/iTerm2 permission in System Settings > Privacy & Security > USB"
                        );
                        println!("     2. Or run with sudo (not recommended for security)");
                    }
                },
            }
        }
    }
}
