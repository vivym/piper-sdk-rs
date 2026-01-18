//! SocketCAN 硬件时间戳验证程序
//!
//! 此程序用于验证 Linux SocketCAN 是否支持硬件时间戳（SO_TIMESTAMPING）。
//! 在集成到 SocketCanAdapter 之前，先使用此程序验证环境。
//!
//! 使用方法：
//! ```bash
//! # 在另一个终端发送帧
//! cansend vcan0 123#DEADBEEF
//!
//! # 运行验证程序
//! cargo run --example timestamp_verification --target x86_64-unknown-linux-gnu
//! ```

use nix::sys::socket::{ControlMessageOwned, MsgFlags, SockaddrStorage, recvmsg};
use socketcan::{CanSocket, Socket};
use std::io::IoSliceMut;
use std::mem;
use std::os::unix::io::AsRawFd;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("SocketCAN 硬件时间戳验证程序");
    println!("正在打开 vcan0 接口...");

    // 1. 打开 CAN Socket
    let socket = CanSocket::open("vcan0").map_err(|e| format!("Failed to open vcan0: {}", e))?;
    let fd = socket.as_raw_fd();
    println!("✓ vcan0 已打开 (fd: {})", fd);

    // 2. 启用 SO_TIMESTAMPING
    let flags = libc::SOF_TIMESTAMPING_RX_HARDWARE
        | libc::SOF_TIMESTAMPING_RAW_HARDWARE
        | libc::SOF_TIMESTAMPING_RX_SOFTWARE
        | libc::SOF_TIMESTAMPING_SOFTWARE;

    let result = unsafe {
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_TIMESTAMPING,
            &flags as *const _ as *const libc::c_void,
            mem::size_of::<u32>() as libc::socklen_t,
        )
    };

    if result < 0 {
        return Err(format!(
            "Failed to set SO_TIMESTAMPING: {}",
            std::io::Error::last_os_error()
        )
        .into());
    }
    println!("✓ SO_TIMESTAMPING 已启用");

    // 3. 准备缓冲区
    // CAN 2.0 帧最大 16 字节（使用 size_of 确保跨平台正确性）
    const CAN_FRAME_LEN: usize = std::mem::size_of::<libc::can_frame>();
    let mut frame_buf = [0u8; CAN_FRAME_LEN];
    let mut cmsg_buf = [0u8; 1024]; // CMSG 缓冲区

    println!("\n等待接收 CAN 帧（按 Ctrl+C 退出）...");
    println!("提示：在另一个终端运行: cansend vcan0 123#DEADBEEF\n");

    // 4. 接收帧（阻塞）
    loop {
        // 构建 IO 向量（必须在循环内，因为需要可变引用）
        let mut iov = [IoSliceMut::new(&mut frame_buf)];

        let msg = recvmsg::<SockaddrStorage>(fd, &mut iov, Some(&mut cmsg_buf), MsgFlags::empty())
            .map_err(|e| format!("recvmsg failed: {}", e))?;

        println!("=== 接收到 CAN 帧 ===");
        println!("数据长度: {} 字节", msg.bytes);

        // 5. 打印时间戳（验证是否能正确提取）
        // 注意：nix 0.30 中使用 ScmTimestampsns，Timestamps 结构体有 system/hw_trans/hw_raw 字段
        // msg.cmsgs() 返回 Result<CmsgIterator>，需要先处理错误
        let mut found_timestamp = false;
        match msg.cmsgs() {
            Ok(cmsgs) => {
                for cmsg in cmsgs {
                    if let ControlMessageOwned::ScmTimestampsns(timestamps) = cmsg {
                        found_timestamp = true;
                        println!("\n时间戳信息 (SCM_TIMESTAMPING):");
                        println!("  Timestamps 结构体字段: system, hw_trans, hw_raw");

                        // timestamps.system - Software (System Time) - 对应 timestamps[0]
                        let sw_ts = timestamps.system;
                        let sw_us =
                            sw_ts.tv_sec() as u64 * 1_000_000 + sw_ts.tv_nsec() as u64 / 1000;
                        println!(
                            "  system (Software):       sec={}, nsec={} ({} us)",
                            sw_ts.tv_sec(),
                            sw_ts.tv_nsec(),
                            sw_us
                        );

                        // timestamps.hw_trans - Hardware-Transformed (System Time) - 对应 timestamps[1]
                        let hw_trans_ts = timestamps.hw_trans;
                        let hw_trans_us = if hw_trans_ts.tv_sec() != 0 || hw_trans_ts.tv_nsec() != 0
                        {
                            hw_trans_ts.tv_sec() as u64 * 1_000_000
                                + hw_trans_ts.tv_nsec() as u64 / 1000
                        } else {
                            0
                        };
                        println!(
                            "  hw_trans (HW-Trans):     sec={}, nsec={} ({} us)",
                            hw_trans_ts.tv_sec(),
                            hw_trans_ts.tv_nsec(),
                            hw_trans_us
                        );

                        // timestamps.hw_raw - Hardware-Raw (Device Clock) - 对应 timestamps[2]
                        let hw_raw_ts = timestamps.hw_raw;
                        let hw_raw_us = if hw_raw_ts.tv_sec() != 0 || hw_raw_ts.tv_nsec() != 0 {
                            hw_raw_ts.tv_sec() as u64 * 1_000_000
                                + hw_raw_ts.tv_nsec() as u64 / 1000
                        } else {
                            0
                        };
                        println!(
                            "  hw_raw (HW-Raw):         sec={}, nsec={} ({} us)",
                            hw_raw_ts.tv_sec(),
                            hw_raw_ts.tv_nsec(),
                            hw_raw_us
                        );

                        // 验证结果
                        if sw_us > 0 {
                            println!("\n✓ 软件时间戳可用 (system)");
                        }
                        if hw_trans_us > 0 {
                            println!("✓ 硬件时间戳可用 (hw_trans - Transformed)");
                        } else {
                            println!("  (硬件时间戳不可用，这是正常的，因为 vcan0 是虚拟接口)");
                        }
                        if hw_raw_us > 0 {
                            println!("✓ 原始硬件时间戳可用 (hw_raw - Raw)");
                        }
                    }
                }
            },
            Err(e) => {
                println!("⚠ 警告：CMSG 迭代失败: {}", e);
            },
        }

        if !found_timestamp {
            println!("⚠ 警告：未找到 SCM_TIMESTAMPING 控制消息");
        }

        println!("====================\n");
    }
}
