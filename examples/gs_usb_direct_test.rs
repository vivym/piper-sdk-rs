//! 直接使用 rusb 和我们的结构体，完全按照 Python 实现 GS-USB 协议
//!
//! 这个脚本完全对齐 Python 的 gs_usb.py 实现，用于调试和对比
//!
//! 运行方式：
//!   sudo cargo run --example gs_usb_direct_test

use piper_sdk::can::gs_usb::protocol::*;
use rusb::{DeviceHandle, GlobalContext};
use std::time::Duration;

// 常量定义（与 Python 完全一致）
const GS_USB_ID_VENDOR: u16 = 0x1D50;
const GS_USB_ID_PRODUCT: u16 = 0x606F;
const GS_USB_CANDLELIGHT_VENDOR_ID: u16 = 0x1209;
const GS_USB_CANDLELIGHT_PRODUCT_ID: u16 = 0x2323;

// USB 端点
const GS_USB_ENDPOINT_IN: u8 = 0x81;

// USB 控制请求类型
const GS_USB_REQ_OUT: u8 = 0x41; // Host to Device | Vendor | Interface
const GS_USB_REQ_IN: u8 = 0xC1; // Device to Host | Vendor | Interface

// 控制请求代码
const GS_USB_BREQ_BITTIMING: u8 = 1;
const GS_USB_BREQ_MODE: u8 = 2;
const GS_USB_BREQ_BT_CONST: u8 = 4;

// 模式标志
const GS_CAN_MODE_NORMAL: u32 = 0;
const GS_CAN_MODE_LISTEN_ONLY: u32 = 1 << 0;
const GS_CAN_MODE_LOOP_BACK: u32 = 1 << 1;
const GS_CAN_MODE_ONE_SHOT: u32 = 1 << 3;
const GS_CAN_MODE_HW_TIMESTAMP: u32 = 1 << 4;

// 模式值
const GS_CAN_MODE_RESET: u32 = 0;
const GS_CAN_MODE_START: u32 = 1;

// 帧大小
const GS_USB_FRAME_SIZE: usize = 20;
const GS_USB_FRAME_SIZE_HW_TIMESTAMP: usize = 24;

// CAN ID 掩码
const CAN_EFF_MASK: u32 = 0x1FFFFFFF; // Extended frame format mask

// 简化的帧结构（用于解析）
#[derive(Debug, Default)]
struct SimpleFrame {
    echo_id: u32,
    can_id: u32,
    can_dlc: u8,
    channel: u8,
    flags: u8,
    reserved: u8,
    data: [u8; 8],
    timestamp_us: u32,
}

fn is_gs_usb_device(vendor_id: u16, product_id: u16) -> bool {
    matches!(
        (vendor_id, product_id),
        (GS_USB_ID_VENDOR, GS_USB_ID_PRODUCT)
            | (GS_USB_CANDLELIGHT_VENDOR_ID, GS_USB_CANDLELIGHT_PRODUCT_ID)
    )
}

fn find_device() -> Option<DeviceHandle<GlobalContext>> {
    for device in rusb::devices().ok()?.iter() {
        let desc = device.device_descriptor().ok()?;
        if is_gs_usb_device(desc.vendor_id(), desc.product_id()) {
            return device.open().ok();
        }
    }
    None
}

fn get_device_capability(
    handle: &DeviceHandle<GlobalContext>,
) -> Result<DeviceCapability, rusb::Error> {
    let mut buf = vec![0u8; 40];
    let len = handle.read_control(
        GS_USB_REQ_IN,
        GS_USB_BREQ_BT_CONST,
        0,
        0,
        &mut buf,
        Duration::from_millis(1000),
    )?;

    if len < 40 {
        return Err(rusb::Error::Other);
    }

    Ok(DeviceCapability::unpack(&buf))
}

fn set_bitrate(handle: &DeviceHandle<GlobalContext>, bitrate: u32) -> Result<(), rusb::Error> {
    // **完全对齐 Python**：set_bitrate() 在 start() 之前调用
    // Python 的 USB 库在发送控制请求时自动处理接口 claim
    // Rust 需要显式处理：如果 kernel driver 是 active 的，先 detach 再 claim
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        if handle.kernel_driver_active(0).unwrap_or(false) {
            handle.detach_kernel_driver(0)?;
        }
    }

    // 然后 claim interface（Python 的 USB 库自动处理，Rust 需要显式处理）
    handle.claim_interface(0)?;

    // 获取设备能力（需要接口已 claim）
    let capability = get_device_capability(handle)?;

    // 根据时钟频率选择位定时参数（与 Python 完全一致）
    let timing = match capability.fclk_can {
        48_000_000 => match bitrate {
            10_000 => Some((1, 12, 2, 1, 300)),
            20_000 => Some((1, 12, 2, 1, 150)),
            50_000 => Some((1, 12, 2, 1, 60)),
            100_000 => Some((1, 12, 2, 1, 30)),
            125_000 => Some((1, 12, 2, 1, 24)),
            250_000 => Some((1, 12, 2, 1, 12)),
            500_000 => Some((1, 12, 2, 1, 6)),
            800_000 => Some((1, 11, 2, 1, 4)),
            1_000_000 => Some((1, 12, 2, 1, 3)),
            _ => None,
        },
        80_000_000 => match bitrate {
            10_000 => Some((1, 12, 2, 1, 500)),
            20_000 => Some((1, 12, 2, 1, 250)),
            50_000 => Some((1, 12, 2, 1, 100)),
            100_000 => Some((1, 12, 2, 1, 50)),
            125_000 => Some((1, 12, 2, 1, 40)),
            250_000 => Some((1, 12, 2, 1, 20)),
            500_000 => Some((1, 12, 2, 1, 10)),
            800_000 => Some((1, 7, 1, 1, 10)),
            1_000_000 => Some((1, 12, 2, 1, 5)),
            _ => None,
        },
        _ => None,
    };

    match timing {
        Some((prop_seg, phase_seg1, phase_seg2, sjw, brp)) => {
            let bit_timing = DeviceBitTiming::new(prop_seg, phase_seg1, phase_seg2, sjw, brp);
            handle.write_control(
                GS_USB_REQ_OUT,
                GS_USB_BREQ_BITTIMING,
                0,
                0,
                &bit_timing.pack(),
                Duration::from_millis(1000),
            )?;
            Ok(())
        },
        None => Err(rusb::Error::Other),
    }
}

fn start_device(handle: &DeviceHandle<GlobalContext>, flags: u32) -> Result<u32, rusb::Error> {
    // **完全对齐 Python 的 start() 方法**：
    // 1. reset() - 重置设备（最前面）
    handle.reset()?;

    // 2. detach_kernel_driver() - 在 reset 之后（如果 kernel driver active）
    // 注意：Python 的 USB 库可能自动处理接口 claim，但 Rust 需要显式处理
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        if handle.kernel_driver_active(0).unwrap_or(false) {
            handle.detach_kernel_driver(0)?;
        }
    }

    // 3. Claim interface（Python 的 USB 库自动处理，Rust 需要显式处理）
    // 注意：必须在 detach_kernel_driver 之后，否则会失败
    handle.claim_interface(0)?;

    // 4. 获取设备能力（需要接口已 claim）
    let capability = get_device_capability(handle)?;

    // 5. 过滤 flags：只保留设备支持的功能
    let mut flags = flags & capability.feature;

    // 6. 过滤 flags：只保留驱动支持的功能（与 Python 完全一致）
    flags &= GS_CAN_MODE_LISTEN_ONLY
        | GS_CAN_MODE_LOOP_BACK
        | GS_CAN_MODE_ONE_SHOT
        | GS_CAN_MODE_HW_TIMESTAMP;

    // 7. 打印调试信息（与 Python 一致）
    let hw_timestamp = (flags & GS_CAN_MODE_HW_TIMESTAMP) != 0;
    eprintln!(
        "[RUST DEBUG] Device capability.feature: 0x{:08X}, requested flags: 0x{:08X}, final flags: 0x{:08X}, hw_timestamp: {}",
        capability.feature, flags, flags, hw_timestamp
    );

    // 8. 发送 MODE 命令
    let mode = DeviceMode::new(GS_CAN_MODE_START, flags);
    handle.write_control(
        GS_USB_REQ_OUT,
        GS_USB_BREQ_MODE,
        0,
        0,
        &mode.pack(),
        Duration::from_millis(1000),
    )?;

    Ok(flags)
}

fn read_frame(
    handle: &DeviceHandle<GlobalContext>,
    hw_timestamp: bool,
    timeout: Duration,
) -> Result<SimpleFrame, rusb::Error> {
    let frame_size = if hw_timestamp {
        GS_USB_FRAME_SIZE_HW_TIMESTAMP
    } else {
        GS_USB_FRAME_SIZE
    };

    let mut buf = vec![0u8; frame_size];
    let len = handle.read_bulk(GS_USB_ENDPOINT_IN, &mut buf, timeout)?;

    if len < frame_size {
        return Err(rusb::Error::Other);
    }

    // **调试：打印原始字节数据**
    eprintln!(
        "[RUST DEBUG] Raw frame bytes (first {}): {:02x?} (hw_timestamp={}, expected_size={}, actual_len={})",
        frame_size.min(24),
        &buf[..frame_size.min(24)],
        hw_timestamp,
        frame_size,
        len
    );

    // 解析帧（与 Python 完全一致：Little-Endian）
    let mut data = [0u8; 8];
    data.copy_from_slice(&buf[12..20]);
    let timestamp_us = if hw_timestamp {
        u32::from_le_bytes([buf[20], buf[21], buf[22], buf[23]])
    } else {
        0
    };
    let frame = SimpleFrame {
        echo_id: u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]),
        can_id: u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]),
        can_dlc: buf[8],
        channel: buf[9],
        flags: buf[10],
        reserved: buf[11],
        data,
        timestamp_us,
    };

    // **调试：打印解析结果**
    eprintln!(
        "[RUST DEBUG] Parsed frame: echo_id=0x{:08X}, can_id=0x{:08X}, can_dlc={}, channel={}, flags={}, reserved={}, data={:02x?}, timestamp_us={}",
        frame.echo_id,
        frame.can_id,
        frame.can_dlc,
        frame.channel,
        frame.flags,
        frame.reserved,
        frame.data,
        frame.timestamp_us
    );

    Ok(frame)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("{}", "=".repeat(60));
    println!("GS USB 直接测试（完全对齐 Python 实现）");
    println!("{}", "=".repeat(60));

    // 1. 查找设备
    println!("\n[1] 正在查找 GS USB 设备...");
    let handle = find_device().ok_or("未找到 GS-USB 设备")?;
    println!("✓ 找到设备");

    // 2. 设置波特率（在 start() 之前，与 Python 完全一致）
    println!("\n[2] 设置波特率为 1000000 bps...");
    set_bitrate(&handle, 1_000_000)?;
    println!("✓ 波特率设置成功");

    // 3. 启动设备（与 Python 完全一致）
    println!("\n[3] 启动设备...");
    let flags = start_device(&handle, GS_CAN_MODE_NORMAL | GS_CAN_MODE_HW_TIMESTAMP)?;
    let hw_timestamp = (flags & GS_CAN_MODE_HW_TIMESTAMP) != 0;
    println!("✓ 设备已启动 (hw_timestamp={})", hw_timestamp);

    // 4. 等待设备稳定
    println!("\n[4] 等待设备稳定...");
    std::thread::sleep(Duration::from_millis(200));

    // 5. 读取 20 个 CAN 帧
    println!("\n[5] 开始读取 CAN 帧（目标：20 帧）...");
    println!("{}", "-".repeat(60));

    let mut frame_count = 0;
    let target_frames = 20;

    while frame_count < target_frames {
        match read_frame(&handle, hw_timestamp, Duration::from_millis(1000)) {
            Ok(frame) => {
                frame_count += 1;

                // 提取 CAN ID（移除标志位，与 Python 的 arbitration_id 一致）
                // Python: arbitration_id = can_id & CAN_EFF_MASK
                let can_id = frame.can_id & CAN_EFF_MASK;

                // 格式化数据
                let data_hex: Vec<String> = frame.data[..frame.can_dlc.min(8) as usize]
                    .iter()
                    .map(|b| format!("{:02x}", b))
                    .collect();

                println!(
                    "帧 #{:2} | ID: 0x{:03X} | DLC: {} | Data: {}",
                    frame_count,
                    can_id,
                    frame.can_dlc,
                    data_hex.join(" ")
                );
            },
            Err(rusb::Error::Timeout) => {
                println!("等待中... (已收到 {}/{})", frame_count, target_frames);
            },
            Err(e) => {
                eprintln!("读取错误: {:?}", e);
                break;
            },
        }
    }

    println!("{}", "-".repeat(60));
    println!("\n✓ 成功读取 {} 个 CAN 帧", frame_count);

    // 6. 停止设备
    println!("\n[6] 正在停止设备...");
    let mode = DeviceMode::new(GS_CAN_MODE_RESET, 0);
    let _ = handle.write_control(
        GS_USB_REQ_OUT,
        GS_USB_BREQ_MODE,
        0,
        0,
        &mode.pack(),
        Duration::from_millis(1000),
    );
    println!("✓ 设备已停止");

    println!("\n{}", "=".repeat(60));
    println!("测试完成");
    println!("{}", "=".repeat(60));

    Ok(())
}
