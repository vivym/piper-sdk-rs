# GS-USB CAN 适配层实现 TODO List（TDD 范式）

> 基于 `gs_usb_implementation_plan_v3.md` 的测试驱动开发清单

## 📊 整体进度

| Phase | 状态 | 进度 | 备注 |
|-------|------|------|------|
| Phase 1: 核心框架 | ✅ 已完成 | 100% | 8/8 测试通过，代码格式化完成 |
| Phase 2: GS-USB 协议层 | ✅ 已完成 | 100% | 18/18 测试通过 |
| Phase 3: GS-USB 设备层 | ✅ 已完成 | 100% | 2/2 测试通过 |
| Phase 4: GS-USB 适配层 | ✅ 已完成 | 100% | 实现完成 |
| Phase 5: 集成测试 | ✅ 代码完成 | 100% | 测试代码已实现（4 个测试用例），待硬件验证 |
| Phase 6: 性能测试 | ✅ 代码完成 | 100% | 测试代码已实现（4 个测试用例），待硬件验证 |

**最后更新**：Phase 1-6 代码已完成（28/28 单元测试通过，8 个集成/性能测试用例已实现）

---

## 开发原则

1. **TDD 流程**：红-绿-重构（Red-Green-Refactor）
   - 🔴 Red: 先写失败的测试
   - 🟢 Green: 实现最简代码使测试通过
   - 🔵 Refactor: 重构优化代码

2. **测试优先级**：
   - 单元测试 > 集成测试 > 端到端测试
   - 协议层 > 设备层 > 适配层

3. **参考文档**：
   - 实现方案：`docs/v0/gs_usb_implementation_plan_v3.md`
   - 问题修正：`docs/v0/gs_usb_implementation_plan_v3_fixes.md`
   - 参考实现：`tmp/gs_usb_rs/src/`

---

## Phase 1: 核心框架（1-2 天）✅ **已完成**

### Task 1.1: 定义核心类型和 Trait ✅

**目标**：建立 `PiperFrame` 和 `CanAdapter` trait

**状态**：✅ 已完成（2024-12-XX）
- ✅ `PiperFrame` 结构体已实现
- ✅ `CanError` 错误类型已实现
- ✅ `CanAdapter` trait 已定义
- ✅ 所有单元测试通过（8/8）

#### 1.1.1 定义 `PiperFrame` 结构体

**文件**：`src/can/mod.rs`

**测试优先**：
```rust
#[cfg(test)]
mod tests {
    use super::PiperFrame;

    #[test]
    fn test_piper_frame_new_standard() {
        let data = [0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08];
        let frame = PiperFrame::new_standard(0x123, &data[..4]).unwrap();

        assert_eq!(frame.raw_id(), 0x123);
        assert_eq!(frame.dlc(), 4);
        assert_eq!(frame.data(), &data[..4]);
        assert!(!frame.is_extended());
    }

    #[test]
    fn test_piper_frame_new_extended() {
        let data = [0xFF; 8];
        let frame = PiperFrame::new_extended(0x12345678, &data).unwrap();

        assert_eq!(frame.raw_id(), 0x12345678);
        assert_eq!(frame.dlc(), 8);
        assert!(frame.is_extended());
    }

    #[test]
    fn test_piper_frame_rejects_long_payload() {
        // 超过 8 字节的数据应该被拒绝
        let data = [0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A];

        assert!(PiperFrame::new_standard(0x123, &data).is_err());
    }

    #[test]
    fn test_piper_frame_data_slice() {
        let data = [0x01, 0x02, 0x03];
        let frame = PiperFrame::new_standard(0x123, &data).unwrap();

        let slice = frame.data();
        assert_eq!(slice.len(), 3);
        assert_eq!(slice, &[0x01, 0x02, 0x03]);
    }

    #[test]
    fn test_piper_frame_copy_trait() {
        // 验证 Copy trait（零成本复制）
        let frame1 = PiperFrame::new_standard(0x123, &[0x01, 0x02]).unwrap();
        let frame2 = frame1; // 应该复制，不是移动

        assert_eq!(frame1.id(), frame2.id()); // frame1 仍然可用
    }
}
```

**实现**：
- 参考：`gs_usb_implementation_plan_v3.md` 第 2.2 节
- 关键点：`Copy` trait、固定 8 字节数据数组

#### 1.1.2 定义 `CanError` 枚举

**文件**：`src/can/mod.rs`

**测试优先**：
```rust
#[cfg(test)]
mod tests {
    use super::CanError;

    #[test]
    fn test_can_error_display() {
        let err = CanError::Timeout;
        assert!(err.to_string().contains("timeout"));
    }

    #[test]
    fn test_can_error_from_io_error() {
        let io_err = std::io::Error::new(std::io::ErrorKind::TimedOut, "test");
        let can_err: CanError = io_err.into();

        match can_err {
            CanError::Io(_) => {},
            _ => panic!("Expected Io variant"),
        }
    }
}
```

**实现**：
- 参考：`gs_usb_implementation_plan_v3.md` 第 2.2 节
- 使用 `thiserror::Error`

#### 1.1.3 定义 `CanAdapter` Trait

**文件**：`src/can/mod.rs`

**测试优先**：
```rust
#[cfg(test)]
mod tests {
    use super::{CanAdapter, PiperFrame, CanError};

    // Mock 实现用于测试 trait 定义
    struct MockCanAdapter {
        sent_frames: Vec<PiperFrame>,
        received_frames: Vec<PiperFrame>,
        receive_index: usize,
    }

    impl CanAdapter for MockCanAdapter {
        fn send(&mut self, frame: PiperFrame) -> Result<(), CanError> {
            self.sent_frames.push(frame);
            Ok(())
        }

        fn receive(&mut self) -> Result<PiperFrame, CanError> {
            if self.receive_index < self.received_frames.len() {
                let frame = self.received_frames[self.receive_index];
                self.receive_index += 1;
                Ok(frame)
            } else {
                Err(CanError::Timeout)
            }
        }
    }

    #[test]
    fn test_can_adapter_send() {
        let mut adapter = MockCanAdapter {
            sent_frames: Vec::new(),
            received_frames: Vec::new(),
            receive_index: 0,
        };

        let frame = PiperFrame::new_standard(0x123, &[0x01, 0x02]).unwrap();
        adapter.send(frame).unwrap();

        assert_eq!(adapter.sent_frames.len(), 1);
        assert_eq!(adapter.sent_frames[0].raw_id(), 0x123);
    }
}
```

**实现**：
- 参考：`gs_usb_implementation_plan_v3.md` 第 2.2 节

---

## Phase 2: GS-USB 协议层（2-3 天）✅ **已完成**

**状态**：✅ 已完成（2024-12-XX）
- ✅ `protocol.rs` 已实现（常量 + 结构体）
- ✅ `frame.rs` 已实现（帧编码/解码，CAN 2.0 only）
- ✅ `error.rs` 已实现（GS-USB 错误类型）
- ✅ 所有单元测试通过（18/18）

### Task 2.1: 实现 `protocol.rs` - 协议常量和结构体

**文件**：`src/can/gs_usb/protocol.rs`

**测试优先**：

#### 2.1.1 测试常量定义

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_control_request_codes() {
        assert_eq!(GS_USB_BREQ_HOST_FORMAT, 0);
        assert_eq!(GS_USB_BREQ_BITTIMING, 1);
        assert_eq!(GS_USB_BREQ_MODE, 2);
        assert_eq!(GS_USB_BREQ_BT_CONST, 4);
        assert_eq!(GS_USB_BREQ_DEVICE_CONFIG, 5);
    }

    #[test]
    fn test_mode_flags() {
        assert_eq!(GS_CAN_MODE_NORMAL, 0);
        assert_eq!(GS_CAN_MODE_LISTEN_ONLY, 1 << 0);
        assert_eq!(GS_CAN_MODE_LOOP_BACK, 1 << 1);
        assert_eq!(GS_CAN_MODE_TRIPLE_SAMPLE, 1 << 2);
        assert_eq!(GS_CAN_MODE_ONE_SHOT, 1 << 3);
    }

    #[test]
    fn test_frame_constants() {
        assert_eq!(GS_USB_ECHO_ID, 0);
        assert_eq!(GS_USB_RX_ECHO_ID, 0xFFFF_FFFF);
        assert_eq!(GS_USB_FRAME_SIZE, 20);
    }

    #[test]
    fn test_endpoints() {
        assert_eq!(GS_USB_ENDPOINT_OUT, 0x02);
        assert_eq!(GS_USB_ENDPOINT_IN, 0x81);
    }

    #[test]
    fn test_can_id_flags_and_masks() {
        // 验证标志位的正确性
        assert_eq!(CAN_EFF_FLAG, 0x8000_0000);
        assert_eq!(CAN_RTR_FLAG, 0x4000_0000);
        assert_eq!(CAN_EFF_MASK, 0x1FFF_FFFF);
        assert_eq!(CAN_SFF_MASK, 0x0000_07FF);
    }
}
```

#### 2.1.2 测试 `DeviceBitTiming` 结构体

```rust
#[cfg(test)]
mod tests {
    use super::DeviceBitTiming;

    #[test]
    fn test_device_bit_timing_pack() {
        let timing = DeviceBitTiming::new(1, 12, 2, 1, 6);
        let packed = timing.pack();

        // 验证 little-endian 编码
        assert_eq!(packed[0..4], [1, 0, 0, 0]);   // prop_seg
        assert_eq!(packed[4..8], [12, 0, 0, 0]);  // phase_seg1
        assert_eq!(packed[8..12], [2, 0, 0, 0]);  // phase_seg2
        assert_eq!(packed[12..16], [1, 0, 0, 0]); // sjw
        assert_eq!(packed[16..20], [6, 0, 0, 0]); // brp

        assert_eq!(packed.len(), 20);
    }

    #[test]
    fn test_device_bit_timing_roundtrip() {
        let original = DeviceBitTiming::new(1, 12, 2, 1, 6);
        let packed = original.pack();

        // 解包验证（如果需要的话）
        let prop_seg = u32::from_le_bytes([packed[0], packed[1], packed[2], packed[3]]);
        assert_eq!(prop_seg, original.prop_seg);
    }
}
```

#### 2.1.3 测试 `DeviceMode` 结构体

```rust
#[cfg(test)]
mod tests {
    use super::{DeviceMode, GS_CAN_MODE_START, GS_CAN_MODE_NORMAL};

    #[test]
    fn test_device_mode_pack() {
        let mode = DeviceMode::new(GS_CAN_MODE_START, GS_CAN_MODE_NORMAL);
        let packed = mode.pack();

        // 验证 little-endian 编码
        assert_eq!(packed[0..4], [1, 0, 0, 0]); // mode = START
        assert_eq!(packed[4..8], [0, 0, 0, 0]); // flags = NORMAL
        assert_eq!(packed.len(), 8);
    }
}
```

#### 2.1.4 测试 `DeviceCapability` 结构体

```rust
#[cfg(test)]
mod tests {
    use super::DeviceCapability;

    #[test]
    fn test_device_capability_unpack() {
        // 构造测试数据（40 字节，10 x u32）
        let mut data = vec![0u8; 40];

        // feature = 0x00000001
        data[0..4].copy_from_slice(&1u32.to_le_bytes());
        // fclk_can = 80_000_000
        data[4..8].copy_from_slice(&80_000_000u32.to_le_bytes());
        // tseg1_min = 1
        data[8..12].copy_from_slice(&1u32.to_le_bytes());

        let cap = DeviceCapability::unpack(&data);

        assert_eq!(cap.feature, 1);
        assert_eq!(cap.fclk_can, 80_000_000);
        assert_eq!(cap.tseg1_min, 1);
    }
}
```

**实现参考**：
- `tmp/gs_usb_rs/src/constants.rs` - 常量定义
- `tmp/gs_usb_rs/src/structures.rs` - 结构体定义
- `gs_usb_implementation_plan_v3.md` 第 3.2 节

---

### Task 2.2: 实现 `frame.rs` - GS-USB 帧编码/解码

**文件**：`src/can/gs_usb/frame.rs`

**测试优先**：

#### 2.2.1 测试帧编码（Pack）

```rust
#[cfg(test)]
mod tests {
    use super::GsUsbFrame;
    use crate::can::gs_usb::protocol::*;
    use bytes::BytesMut;

    #[test]
    fn test_frame_pack_to() {
        let frame = GsUsbFrame {
            echo_id: GS_USB_ECHO_ID,
            can_id: 0x123,
            can_dlc: 4,
            channel: 0,
            flags: 0,
            reserved: 0,
            data: [0x01, 0x02, 0x03, 0x04, 0, 0, 0, 0],
        };

        let mut buf = BytesMut::new();
        frame.pack_to(&mut buf);

        assert_eq!(buf.len(), GS_USB_FRAME_SIZE);

        // 验证 Header
        assert_eq!(buf[0..4], [0, 0, 0, 0]); // echo_id
        assert_eq!(buf[4..8], [0x23, 0x01, 0, 0]); // can_id (little-endian)
        assert_eq!(buf[8], 4); // can_dlc
        assert_eq!(buf[9], 0); // channel
        assert_eq!(buf[10], 0); // flags
        assert_eq!(buf[11], 0); // reserved

        // 验证 Data
        assert_eq!(buf[12..16], [0x01, 0x02, 0x03, 0x04]);
    }

    #[test]
    fn test_frame_pack_extended_id() {
        let frame = GsUsbFrame {
            echo_id: GS_USB_ECHO_ID,
            can_id: 0x12345678 | CAN_EFF_FLAG, // 扩展帧
            can_dlc: 8,
            channel: 0,
            flags: 0,
            reserved: 0,
            data: [0xFF; 8],
        };

        let mut buf = BytesMut::new();
        frame.pack_to(&mut buf);

        // 验证扩展 ID（包含 EFF_FLAG）
        let can_id = u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]);
        assert_eq!(can_id & CAN_EFF_FLAG, CAN_EFF_FLAG);
        assert_eq!(can_id & CAN_EFF_MASK, 0x12345678);
    }
}
```

#### 2.2.2 测试帧解码（Unpack）

```rust
#[cfg(test)]
mod tests {
    use super::GsUsbFrame;
    use crate::can::gs_usb::protocol::*;
    use bytes::Bytes;

    #[test]
    fn test_frame_unpack_from_bytes() {
        // 构造测试数据
        let mut data = vec![0u8; GS_USB_FRAME_SIZE];

        // echo_id = 0xFFFF_FFFF (RX 帧)
        data[0..4].copy_from_slice(&GS_USB_RX_ECHO_ID.to_le_bytes());
        // can_id = 0x123
        data[4..8].copy_from_slice(&0x123u32.to_le_bytes());
        data[8] = 4; // can_dlc
        data[9] = 0; // channel
        data[10] = 0; // flags
        data[11] = 0; // reserved
        data[12..16].copy_from_slice(&[0x01, 0x02, 0x03, 0x04]);

        let mut frame = GsUsbFrame::default();
        frame.unpack_from_bytes(Bytes::from(data), false).unwrap();

        assert_eq!(frame.echo_id, GS_USB_RX_ECHO_ID);
        assert_eq!(frame.can_id, 0x123);
        assert_eq!(frame.can_dlc, 4);
        assert_eq!(frame.data(), &[0x01, 0x02, 0x03, 0x04]);
        assert!(frame.is_rx_frame());
    }

    #[test]
    fn test_frame_unpack_too_short() {
        let mut frame = GsUsbFrame::default();
        let data = Bytes::from(vec![0u8; 10]); // 太短

        let result = frame.unpack_from_bytes(data, false);
        assert!(result.is_err());
    }

    #[test]
    fn test_frame_is_tx_echo() {
        let frame = GsUsbFrame {
            echo_id: 0x1234, // 非 RX_ECHO_ID
            ..Default::default()
        };

        assert!(frame.is_tx_echo());
        assert!(!frame.is_rx_frame());
    }

    #[test]
    fn test_frame_has_overflow() {
        let frame = GsUsbFrame {
            flags: GS_CAN_FLAG_OVERFLOW,
            ..Default::default()
        };

        assert!(frame.has_overflow());
    }
}
```

**实现参考**：
- `tmp/gs_usb_rs/src/frame.rs` - 帧编码/解码逻辑
- `gs_usb_implementation_plan_v3.md` 第 3.3 节

---

### Task 2.3: 实现 `error.rs` - GS-USB 错误类型

**文件**：`src/can/gs_usb/error.rs`

**测试优先**：

```rust
#[cfg(test)]
mod tests {
    use super::GsUsbError;

    #[test]
    fn test_gs_usb_error_from_rusb_error() {
        let rusb_err = rusb::Error::NotFound;
        let gs_err: GsUsbError = rusb_err.into();

        match gs_err {
            GsUsbError::Usb(_) => {},
            _ => panic!("Expected Usb variant"),
        }
    }

    #[test]
    fn test_gs_usb_error_is_timeout() {
        let err1 = GsUsbError::ReadTimeout;
        let err2 = GsUsbError::WriteTimeout;

        assert!(err1.is_timeout());
        assert!(err2.is_timeout());
    }
}
```

**实现参考**：
- `tmp/gs_usb_rs/src/error.rs`

---

## Phase 3: GS-USB 设备层（2-3 天）

### Task 3.1: 实现 `device.rs` - USB 设备操作

**文件**：`src/can/gs_usb/device.rs`

**测试优先**：

#### 3.1.1 测试设备扫描

```rust
#[cfg(test)]
mod tests {
    use super::GsUsbDevice;

    #[test]
    fn test_is_gs_usb_device() {
        // 测试已知的 VID/PID
        assert!(GsUsbDevice::is_gs_usb_device(0x1D50, 0x606F)); // GS-USB
        assert!(GsUsbDevice::is_gs_usb_device(0x1209, 0x2323)); // Candlelight
        assert!(!GsUsbDevice::is_gs_usb_device(0x1234, 0x5678)); // 未知设备
    }

    // 注意：scan() 的集成测试需要实际硬件，放在集成测试中
}
```

#### 3.1.2 测试控制传输

```rust
#[cfg(test)]
mod tests {
    use super::GsUsbDevice;

    // Mock USB 设备的测试需要使用 rusb 的 mock 或者集成测试
    // 这里只测试逻辑，实际 USB 操作需要硬件

    #[test]
    fn test_send_host_format_format() {
        // 验证 HOST_FORMAT 的数据格式
        let val: u32 = 0x0000_BEEF;
        let data = val.to_le_bytes();

        // 在 little-endian 系统上应该是 [0xEF, 0xBE, 0x00, 0x00]
        assert_eq!(data[0], 0xEF);
        assert_eq!(data[1], 0xBE);
        assert_eq!(data[2], 0x00);
        assert_eq!(data[3], 0x00);
    }
}
```

#### 3.1.3 测试波特率设置（使用预定义表）

```rust
#[cfg(test)]
mod tests {
    use super::GsUsbDevice;

    // 注意：需要 mock USB 设备或使用集成测试
    // 这里测试波特率计算的逻辑（如果拆分出来）

    #[test]
    fn test_bitrate_predefined_table_80mhz() {
        // 验证 80MHz 时钟的波特率映射表
        // 这个测试确保参考实现的预定义表被正确移植
        // 具体值参考 device.rs 中的预定义表
    }
}
```

**实现参考**：
- `tmp/gs_usb_rs/src/device.rs` - 完整的设备操作实现
- `gs_usb_implementation_plan_v3.md` 第 3.4 节
- `gs_usb_implementation_plan_v3_fixes.md` - 关键修正点

**关键实现点**：
1. ✅ `send_host_format()` 使用 `value = 0`（不是 1）
2. ✅ `start()` 中包含 `reset()`、`detach_kernel_driver()`、`claim_interface()`
3. ✅ 设备匹配使用 `is_gs_usb_device()` 方法

---

## Phase 4: GS-USB 适配层（1-2 天）✅ **已完成**

**状态**：✅ 已完成（2024-12-XX）
- ✅ `GsUsbCanAdapter` 已实现（实现 `CanAdapter` trait）
- ✅ `send()` 实现（Fire-and-Forget，不等待 Echo）
- ✅ `receive()` 实现（三层过滤漏斗：过滤 Echo、错误帧）
- ✅ `configure()` 实现（HOST_FORMAT + 波特率 + 启动）

### Task 4.1: 实现 `mod.rs` - `GsUsbCanAdapter` ✅

**文件**：`src/can/gs_usb/mod.rs`

**测试优先**：

#### 4.1.1 测试 `configure()` 方法

```rust
#[cfg(test)]
mod tests {
    use super::GsUsbCanAdapter;

    // Mock 测试或集成测试
    #[test]
    fn test_configure_calls_host_format() {
        // 验证 configure() 调用了 send_host_format()
        // 需要使用 mock 或 spy 模式
    }
}
```

#### 4.1.2 测试 `send()` - Fire-and-Forget

```rust
#[cfg(test)]
mod tests {
    use super::GsUsbCanAdapter;
    use crate::can::{PiperFrame, CanError};

    // 需要 mock GsUsbDevice
    #[test]
    fn test_send_fire_and_forget() {
        // 验证 send() 不等待 Echo
        // 1. 调用 send()
        // 2. 立即返回（不阻塞）
        // 3. 验证 USB write 被调用
    }

    #[test]
    fn test_send_not_started() {
        let mut adapter = GsUsbCanAdapter::new(...);
        // adapter 未调用 configure()

        let frame = PiperFrame::new_standard(0x123, &[0x01]).unwrap();
        let result = adapter.send(frame);

        assert!(matches!(result, Err(CanError::NotStarted)));
    }
}
```

#### 4.1.3 测试 `receive()` - 三层过滤漏斗

```rust
#[cfg(test)]
mod tests {
    use super::GsUsbCanAdapter;

    #[test]
    fn test_receive_filters_tx_echo() {
        // Mock 设备返回 TX Echo 帧
        // 验证 receive() 自动跳过 Echo，继续读取下一帧
    }

    #[test]
    fn test_receive_filters_overflow() {
        // Mock 设备返回带有 OVERFLOW 标志的帧
        // 验证 receive() 返回 CanError::BufferOverflow
    }

    #[test]
    fn test_receive_returns_valid_frame() {
        // Mock 设备返回有效的 RX 帧
        // 验证 receive() 正确转换并返回 PiperFrame
    }

    #[test]
    fn test_receive_timeout() {
        // Mock 设备超时
        // 验证 receive() 返回 CanError::Timeout
    }
}
```

**实现参考**：
- `gs_usb_implementation_plan_v3.md` 第 3.5 节
- `gs_usb_implementation_plan_v2_comment.md` - 三层过滤漏斗逻辑

---

## Phase 5: 集成测试（1 天）✅ **已完成**

### Task 5.1: 硬件集成测试 ✅

**文件**：`tests/gs_usb_integration_tests.rs`

**实现状态**：✅ **已完成**

**测试用例**：

1. ✅ `test_can_adapter_basic()` - 测试基本适配器创建、配置、发送
2. ✅ `test_send_fire_and_forget()` - 验证 Fire-and-Forget 语义（100 帧快速发送，不阻塞）
3. ✅ `test_receive_filter_funnel()` - 测试三层过滤漏斗（接收超时处理）
4. ✅ `test_send_not_started()` - 测试错误处理（设备未启动时发送）

**文件**：`tests/gs_usb_integration_tests.rs`

**运行方式**：
```bash
# 运行所有测试（包括需要硬件的）
cargo test --test gs_usb_integration_tests -- --ignored

# 仅检查测试代码（不需要硬件）
cargo test --test gs_usb_integration_tests
```

---

## Phase 6: 性能测试（1 天）✅ **已完成**

### Task 6.1: 1kHz 收发压力测试 ✅

**文件**：`tests/gs_usb_performance_tests.rs`

**实现状态**：✅ **已完成**

**测试用例**：

1. ✅ `test_1khz_send_performance()` - 测试 1kHz 发送性能（目标：>= 800 fps，Fire-and-Forget）
2. ✅ `test_send_latency()` - 测试单帧发送延迟（目标：< 1ms 平均延迟）
3. ✅ `test_receive_timeout_latency()` - 测试接收超时延迟（验证超时机制）
4. ✅ `test_batch_send_performance()` - 测试批量发送性能（1000 帧连续发送）

**运行方式**：
```bash
# 运行性能测试（需要硬件）
cargo test --test gs_usb_performance_tests -- --ignored

# 仅检查测试代码
cargo test --test gs_usb_performance_tests
```

**之前的测试用例模板**（已实现）：

```rust
#[test]
#[ignore]
fn test_1khz_send_performance() {
    let mut adapter = GsUsbCanAdapter::new().unwrap();
    adapter.configure(500_000).unwrap();

    let frame = PiperFrame::new_standard(0x123, &[0x01, 0x02, 0x03, 0x04]).unwrap();

    let start = std::time::Instant::now();
    let mut count = 0;

    // 发送 1000 帧（1秒内，1kHz）
    while start.elapsed().as_millis() < 1000 {
        adapter.send(frame).unwrap();
        count += 1;
    }

    println!("Sent {} frames in 1 second", count);
    assert!(count >= 800, "Performance too low: {} fps", count); // 允许 20% 误差
}

#[test]
#[ignore]
fn test_receive_latency() {
    // 测试接收延迟（需要硬件支持 loopback）
}
```

---

## 测试覆盖率目标

- **单元测试覆盖率**：> 80%
- **协议层**：100%（所有常量和结构体）
- **帧编码/解码**：100%（所有边界情况）
- **错误处理**：主要错误路径覆盖

---

## 执行检查清单

### Phase 1 完成标准 ✅ **已完成**
- [x] 所有单元测试通过（8/8 测试通过）
- [x] `PiperFrame` 和 `CanAdapter` 定义完整
- [x] 代码通过 `cargo clippy` 和 `cargo fmt`

### Phase 2 完成标准 ✅
- [x] 协议常量与参考实现一致
- [x] 结构体 `pack()`/`unpack()` 测试通过
- [x] 帧编码/解码 roundtrip 测试通过（测试 `test_frame_roundtrip`）

### Phase 3 完成标准 ✅
- [x] 设备扫描逻辑正确（VID/PID 匹配）
- [x] 控制传输参数正确（`value = 0`，`wIndex = 0`）
- [x] `start()` 流程完整（reset、detach、claim）

### Phase 4 完成标准 ✅
- [x] `send()` 不阻塞（Fire-and-Forget）
- [x] `receive()` 正确过滤 Echo 和错误帧
- [x] 三层过滤漏斗逻辑正确

### Phase 5 完成标准 ✅ **代码已完成**
- [x] 集成测试代码已创建（`tests/gs_usb_integration_tests.rs`）
- [x] 所有测试用例已实现（4 个测试用例）
- [ ] 实际硬件测试通过（需要硬件，待运行）
- [ ] 端到端测试通过（需要硬件，待运行）

### Phase 6 完成标准 ✅ **代码已完成**
- [x] 性能测试代码已创建（`tests/gs_usb_performance_tests.rs`）
- [x] 所有测试用例已实现（4 个测试用例）
- [ ] 1kHz 收发性能满足要求（需要硬件，待运行）
- [ ] 延迟测试通过（需要硬件，待运行）

---

## 参考资源

1. **实现方案**：`docs/v0/gs_usb_implementation_plan_v3.md`
2. **问题修正**：`docs/v0/gs_usb_implementation_plan_v3_fixes.md`
3. **专家评价**：`docs/v0/gs_usb_implementation_plan_v2_comment.md`
4. **参考实现**：`tmp/gs_usb_rs/src/`
5. **TDD 文档**：`docs/v0/TDD.md`

---

## 备注

- **Mock 策略**：对于 USB 操作，可以使用 trait 抽象，便于单元测试
- **集成测试**：需要实际硬件，使用 `#[ignore]` 标记，手动运行
- **性能测试**：在真实硬件上运行，记录基准数据
- **代码审查**：每个 Phase 完成后进行代码审查，确保符合方案要求
