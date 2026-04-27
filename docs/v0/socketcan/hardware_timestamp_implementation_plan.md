# SocketCAN 硬件时间戳实现方案

**日期**：2024-12-19
**目标**：为 SocketCAN 适配器实现硬件时间戳支持，默认开启
**优先级**：🔴 **最高** - 对高频力控场景至关重要

---

## 1. 背景与需求

### 1.1 当前状态

- ✅ `PiperFrame` 已包含 `timestamp_us: u64` 字段（微秒精度）
- ✅ GS-USB 适配器已支持硬件时间戳
- ❌ SocketCAN 适配器的 `timestamp_us` 目前硬编码为 `0`
- ✅ Pipeline 已正确使用 `frame.timestamp_us`（`src/robot/pipeline.rs:199`）

### 1.2 需求分析

**业务需求**：
- **高频力控**：500Hz 控制循环（2ms 周期）需要精确的时间测量
- **时间同步**：多关节反馈帧的时间戳对齐
- **性能分析**：精确测量帧收发延迟

**技术要求**：
- 默认启用硬件时间戳（如果硬件支持）
- 硬件不支持时自动降级到软件时间戳
- 时间戳精度：微秒级（`u64` 微秒）
- 向后兼容：如果时间戳不可用，返回 0

---

## 2. 技术方案

### 2.1 核心原理

根据调研文档（`docs/v0/socketcan/survey.md`），Linux SocketCAN 支持硬件时间戳通过以下机制：

1. **Socket 选项**：使用 `SO_TIMESTAMPING` 启用时间戳
2. **接收方式**：必须使用 `recvmsg()` 而非 `read()`，因为时间戳通过 CMSG（Control Message）传递
3. **时间戳来源**：`SCM_TIMESTAMPING` 返回 3 个 `timespec`：
   - `timestamps[0]`：软件时间戳（System Time）
   - `timestamps[1]`：转换后的硬件时间戳（Hardware Time → System Time）
   - `timestamps[2]`：原始硬件时间戳（Raw Hardware Time）

### 2.2 方案设计

#### 2.2.1 架构变更

**当前架构**：
```rust
// 使用 socketcan crate 的高级 API
let can_frame = self.socket.read_frame_timeout(self.read_timeout)?;
// timestamp_us = 0 (硬编码)
```

**新架构**：
```rust
// 使用 recvmsg 接收原始数据 + CMSG
let (can_frame, timestamp_us) = self.receive_with_timestamp()?;
```

#### 2.2.2 实现策略

**策略 A：混合模式（推荐）**
- 初始化时检测硬件时间戳支持
- 如果支持：使用 `recvmsg()` 获取硬件时间戳
- 如果不支持：使用 `recvmsg()` 获取软件时间戳
- **优点**：统一使用 `recvmsg()`，代码路径一致
- **缺点**：需要维护两套解析逻辑

**策略 B：统一使用 recvmsg（最终方案）**
- 始终使用 `recvmsg()` 接收帧
- 从 CMSG 中提取时间戳（硬件优先，软件备选）
- 同时保留 `socketcan` crate 的 Frame 解析能力
- **优点**：代码路径单一，性能最优
- **缺点**：需要手动解析 CAN 帧格式

**策略 C：双重检查模式**
- 先用 `read_frame_timeout()` 读取帧（兼容现有代码）
- 再用 `recvmsg()` 提取时间戳（仅时间戳）
- **优点**：最小化代码变更
- **缺点**：两次系统调用，性能开销大（❌ 不推荐）

**最终选择：策略 B（统一使用 recvmsg）**

---

## 3. 详细实现方案

### 3.1 SocketCAN 适配器结构修改

#### 3.1.1 添加时间戳支持状态

```rust
pub struct SocketCanAdapter {
    socket: CanSocket,
    interface: String,
    started: bool,
    read_timeout: Duration,

    // 新增：时间戳支持状态
    /// 是否启用时间戳（初始化时设置）
    timestamping_enabled: bool,
    /// 是否检测到硬件时间戳支持（运行时检测）
    hw_timestamp_available: bool,
}
```

#### 3.1.2 初始化时启用 SO_TIMESTAMPING

在 `SocketCanAdapter::new()` 中添加：

```rust
// 启用时间戳（默认开启）
let flags = libc::SOF_TIMESTAMPING_RX_HARDWARE
          | libc::SOF_TIMESTAMPING_RAW_HARDWARE
          | libc::SOF_TIMESTAMPING_RX_SOFTWARE
          | libc::SOF_TIMESTAMPING_SOFTWARE;

unsafe {
    let ret = libc::setsockopt(
        socket.as_raw_fd(),
        libc::SOL_SOCKET,
        libc::SO_TIMESTAMPING,
        &flags as *const _ as *const libc::c_void,
        std::mem::size_of::<u32>() as libc::socklen_t,
    );

    if ret < 0 {
        // 警告：无法启用时间戳，但不阻塞初始化
        warn!("Failed to enable SO_TIMESTAMPING: {}", std::io::Error::last_os_error());
        timestamping_enabled = false;
    } else {
        timestamping_enabled = true;
        // 初始化时不检测硬件支持（首次接收时检测）
        hw_timestamp_available = false;
    }
}
```

### 3.2 接收方法重构

#### 3.2.1 新的接收方法签名

```rust
/// 使用 recvmsg 接收 CAN 帧并提取时间戳
fn receive_with_timestamp(&mut self) -> Result<(CanFrame, u64), CanError> {
    // 实现详见下文
}
```

#### 3.2.2 recvmsg 实现

```rust
use nix::sys::socket::{recvmsg, ControlMessageOwned, MsgFlags};
use nix::sys::uio::IoVec;
use std::os::unix::io::AsRawFd;

fn receive_with_timestamp(&mut self) -> Result<(CanFrame, u64), CanError> {
    let fd = self.socket.as_raw_fd();

    // 准备缓冲区（防御性编程：使用编译时计算的大小，避免平台差异）
    // CAN 2.0 帧在 64位 Linux 上通常是 16 字节，但使用 size_of 确保跨平台正确性
    const CAN_FRAME_LEN: usize = std::mem::size_of::<libc::can_frame>();
    let mut frame_buf = [0u8; CAN_FRAME_LEN];

    // CMSG 缓冲区（1024 字节足够大，通常只需要 ~64 字节）
    // 注意：如果追求极致优化，可以使用 nix::cmsg_space! 宏计算精确大小
    // 但对于栈分配，固定大小数组通常更方便
    let mut cmsg_buf = [0u8; 1024];

    // 构建 IO 向量
    let mut iov = [IoVec::from_mut_slice(&mut frame_buf)];

    // 调用 recvmsg（带超时）
    // 注意：recvmsg 本身不直接支持超时，需要配合 poll/epoll
    // 这里先简化，使用 read_frame_timeout 的超时逻辑
    // TODO: 后续可以优化为使用 poll + recvmsg

    // 使用超时读取（简化版，实际需要使用 poll/epoll）
    let msg = match recvmsg::<nix::sys::socket::SockaddrStorage>(
        fd,
        &mut iov,
        Some(&mut cmsg_buf),
        MsgFlags::empty(),
    ) {
        Ok(msg) => msg,
        Err(nix::errno::Errno::EAGAIN) | Err(nix::errno::Errno::EWOULDBLOCK) => {
            // 超时
            return Err(CanError::Timeout);
        }
        Err(e) => {
            return Err(CanError::Io(std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("recvmsg failed: {}", e)
            )));
        }
    };

    // 解析 CAN 帧
    if msg.bytes < 16 {
        return Err(CanError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("Incomplete CAN frame: {} bytes", msg.bytes)
        )));
    }

    // 将原始数据转换为 CanFrame
    // 注意：这里需要手动解析 can_frame 结构（使用 libc::can_frame）
    // 或者使用 socketcan crate 的 FromRawCanFrame trait（如果可用）
    let can_frame = self.parse_raw_can_frame(&frame_buf[..msg.bytes])?;

    // 提取时间戳
    let timestamp_us = self.extract_timestamp_from_cmsg(&msg)?;

    Ok((can_frame, timestamp_us))
}
```

#### 3.2.3 时间戳提取逻辑（修正版）

**⚠️ 重要修正**：时间戳的优先级必须严格区分：

- **`timestamps[1]` (Hardware-Transformed/System)**: 硬件时间已同步到系统时钟（UTC/Boot time）。这是**首选**，可直接与系统时间对比。
- **`timestamps[0]` (Software/System)**: 软件中断时间戳（内核记录）。精度也很好，微秒级抖动。这是**次选**。
- **`timestamps[2]` (Hardware-Raw)**: 网卡内部计数器，零点可能是上电时刻。**不应使用**（除非特殊场景）。

```rust
/// 从 CMSG 中提取时间戳（硬件-系统时间优先，软件备选）
///
/// 优先级顺序：
/// 1. timestamps[1] (Hardware-Transformed) - 硬件时间同步到系统时钟
/// 2. timestamps[0] (Software) - 软件中断时间戳
/// 3. timestamps[2] (Hardware-Raw) - 不推荐使用，可能导致时间轴错乱
fn extract_timestamp_from_cmsg(&mut self, msg: &nix::sys::socket::RecvMsg<...>) -> Result<u64, CanError> {
    if !self.timestamping_enabled {
        return Ok(0);  // 未启用时间戳
    }

    // 遍历所有 CMSG
    for cmsg in msg.cmsgs() {
        if let ControlMessageOwned::ScmTimestamping(timestamps) = cmsg {
            // ✅ 优先级 1：硬件时间戳（已同步到系统时钟）
            // timestamps[1] 是硬件时间经过内核转换后的系统时间
            // 这是最理想的：硬件精度 + 系统时间轴一致性
            if timestamps[1].tv_sec != 0 || timestamps[1].tv_nsec != 0 {
                if !self.hw_timestamp_available {
                    trace!("Hardware timestamp (system-synced) detected and enabled");
                    self.hw_timestamp_available = true;
                }

                let timestamp_us = timespec_to_micros(&timestamps[1]);
                return Ok(timestamp_us);
            }

            // ✅ 优先级 2：软件时间戳（系统中断时间）
            // 如果硬件时间戳不可用，降级到软件时间戳
            // 精度仍然很好（微秒级），适合高频力控
            if timestamps[0].tv_sec != 0 || timestamps[0].tv_nsec != 0 {
                if !self.hw_timestamp_available {
                    trace!("Hardware timestamp not available, using software timestamp");
                }

                let timestamp_us = timespec_to_micros(&timestamps[0]);
                return Ok(timestamp_us);
            }

            // ⚠️ 优先级 3：原始硬件时间戳（不推荐）
            // timestamps[2] 是网卡内部计数器，通常与系统时间不在同一量级
            // 仅在特殊场景（如 PTP 同步）下使用
            // 当前实现不返回此值，避免时间轴错乱
            // 如果需要，可以在这里添加警告和可选返回
        }
    }

    // 没有找到时间戳
    Ok(0)
}

/// 将 timespec (秒+纳秒) 转换为微秒（u64）
///
/// # 参数
/// - `tv_sec`: 秒数（i64）
/// - `tv_nsec`: 纳秒数（i64）
///
/// # 返回值
/// - `u64`: 微秒数（支持绝对时间戳，从 Unix 纪元开始）
///
/// # 说明
/// 使用 `u64` 而非 `u32` 的原因：
/// - 支持绝对时间戳（Unix 纪元开始），无需基准时间管理
/// - 内存对齐后大小相同（24 字节），无额外开销
/// - 与状态层设计一致（`CoreMotionState.timestamp_us: u64`）
fn timespec_to_micros(tv_sec: i64, tv_nsec: i64) -> u64 {
    // 计算：timestamp_us = tv_sec * 1_000_000 + tv_nsec / 1000
    // u64 可以存储从 Unix 纪元开始的绝对时间戳（无需截断）
    (tv_sec as u64) * 1_000_000 + ((tv_nsec as u64) / 1000)
}
```

**关键修正**：
- ❌ **错误**：将 `timestamps[1]` 和 `timestamps[2]` 视为同等优先级
- ✅ **正确**：严格优先级：`timestamps[1]` (Transformed) > `timestamps[0]` (Software) > 不使用 `timestamps[2]` (Raw)

**原因**：
- `timestamps[1]` 是硬件时间同步到系统时钟，可直接与系统时间对比（多传感器融合）
- `timestamps[2]` 是原始硬件计数器，零点可能是上电时刻，无法直接对比（除非运行 PTP）

### 3.3 CAN 帧解析

#### 3.3.1 手动解析 can_frame 结构（安全实现）

由于 `recvmsg` 返回原始字节，需要手动解析 `libc::can_frame`。**关键**：不能直接指针强转，必须使用安全的内存拷贝，确保结构体对齐。

```rust
use libc::can_frame;
use std::mem;

fn parse_raw_can_frame(&self, data: &[u8]) -> Result<CanFrame, CanError> {
    if data.len() < std::mem::size_of::<can_frame>() {
        return Err(CanError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "Frame too short"
        )));
    }

    // ✅ 安全的内存拷贝：先创建已对齐的结构体，再拷贝数据
    // 这样可以避免未对齐访问导致的 Bus Error (SIGBUS) 崩溃
    let mut raw_frame: can_frame = unsafe { std::mem::zeroed() };

    unsafe {
        std::ptr::copy_nonoverlapping(
            data.as_ptr(),
            &mut raw_frame as *mut _ as *mut u8,
            std::mem::size_of::<can_frame>()
        );
    }

    // 转换为 socketcan::CanFrame
    // 方案 A：使用 socketcan crate 的 From trait（如果可用）
    // let can_frame = CanFrame::from(raw_frame);

    // 方案 B：手动构造 CanFrame（备选方案，如果 socketcan 没有 From trait）
    // 注意：需要处理 EFF/RTR/ERR 标志位
    let id = socketcan::Id::from_bits(raw_frame.can_id);  // 处理 EFF/RTR/ERR 掩码
    let data_len = raw_frame.can_dlc as usize;
    let data = &raw_frame.data[..data_len.min(8)];

    let can_frame = CanFrame::new(id, data)
        .map_err(|e| CanError::Device(format!("Failed to create CanFrame: {}", e)))?;

    Ok(can_frame)
}
```

**关键改进**：
- ❌ **错误**：`std::ptr::read(data.as_ptr() as *const can_frame)` - 未对齐访问风险
- ✅ **正确**：`std::ptr::copy_nonoverlapping` - 安全的内存拷贝，确保对齐

**版本兼容性**：
- 优先尝试 `socketcan` crate 的 `From<libc::can_frame>` trait（如果 3.5 版本支持）
- 如果不支持，使用备选方案：手动从 `raw_frame.can_id`、`raw_frame.can_dlc`、`raw_frame.data` 构造 `CanFrame`
- 备选方案代码已提供，确保跨版本兼容性

#### 3.3.2 备选方案：混合使用 read_frame 和 recvmsg

如果手动解析复杂，可以：
1. 使用 `recvmsg` 接收数据和 CMSG（获取时间戳）
2. 同时使用 `socketcan` 的 `CanFrame::from()` 解析数据部分
3. 缺点：需要确保数据对齐和格式匹配

### 3.4 超时处理

#### 3.4.1 问题

`recvmsg` 本身不支持超时。需要使用 `poll`/`epoll` 实现超时。

#### 3.4.2 解决方案

**方案 A：使用 poll + recvmsg**

```rust
use nix::poll::{poll, PollFd, PollFlags, PollTimeout};

fn receive_with_timestamp(&mut self) -> Result<(CanFrame, u64), CanError> {
    let fd = self.socket.as_raw_fd();

    // 先 poll 检查是否有数据（带超时）
    let pollfd = PollFd::new(
        unsafe { nix::sys::socket::sockopt::BorrowedFd::borrow_raw(fd) },
        PollFlags::POLLIN,
    );

    match poll(&mut [pollfd], PollTimeout::from(self.read_timeout))? {
        0 => return Err(CanError::Timeout),  // 超时
        _ => {}  // 有数据，继续
    }

    // 现在可以安全地调用 recvmsg（不会阻塞）
    // ... recvmsg 逻辑 ...
}
```

**方案 B：保持现有超时逻辑**

- 继续使用 `read_frame_timeout()` 的超时
- 仅在需要时间戳时使用 `recvmsg`
- **缺点**：无法同时使用，需要统一接口

**推荐：方案 A**（使用 poll + recvmsg）

### 3.5 receive() 方法更新

```rust
impl CanAdapter for SocketCanAdapter {
    fn receive(&mut self) -> Result<PiperFrame, CanError> {
        if !self.started {
            return Err(CanError::NotStarted);
        }

        // 循环读取，直到收到有效数据帧（跳过错误帧）
        loop {
            // 使用 recvmsg 接收帧和时间戳
            let (can_frame, timestamp_us) = match self.receive_with_timestamp() {
                Ok(result) => result,
                Err(CanError::Timeout) => return Err(CanError::Timeout),
                Err(e) => return Err(e),
            };

            // 1. 过滤并解析错误帧（现有逻辑）
            if can_frame.is_error_frame() {
                // ... 错误帧处理 ...
                continue;
            }

            // 2. 转换 CanFrame -> PiperFrame（使用提取的时间戳）
            let piper_frame = PiperFrame {
                id: can_frame.raw_id(),
                data: {
                    let mut data = [0u8; 8];
                    let frame_data = can_frame.data();
                    let len = frame_data.len().min(8);
                    data[..len].copy_from_slice(&frame_data[..len]);
                    data
                },
                len: can_frame.dlc() as u8,
                is_extended: can_frame.is_extended(),
                timestamp_us,  // ✅ 使用提取的时间戳
            };

            trace!(
                "Received CAN frame: ID=0x{:X}, len={}, timestamp_us={}",
                piper_frame.raw_id(), piper_frame.len, piper_frame.timestamp_us
            );

            return Ok(piper_frame);
        }
    }
}
```

---

## 4. 实现步骤

### Phase 3.1: 基础框架（1-2 小时）

1. ✅ 添加 `timestamping_enabled` 和 `hw_timestamp_available` 字段
2. ✅ 在 `new()` 中启用 `SO_TIMESTAMPING`
3. ✅ 添加 `receive_with_timestamp()` 骨架（暂时返回 `(CanFrame, 0)`）
4. ✅ 测试编译通过

### Phase 3.2: 实现 recvmsg 接收（2-3 小时）

1. ✅ 实现 `receive_with_timestamp()` 的 `recvmsg` 部分
2. ✅ 实现超时处理（`poll` + `recvmsg`）
3. ✅ 实现 CAN 帧解析（`parse_raw_can_frame`）
4. ✅ 单元测试：验证能接收帧（时间戳暂时为 0）

### Phase 3.3: 时间戳提取（1-2 小时）

1. ✅ 实现 `extract_timestamp_from_cmsg()`
2. ✅ 实现硬件/软件时间戳优先级逻辑
3. ✅ 实现时间戳单位转换（纳秒 → 微秒）
4. ✅ 单元测试：验证时间戳提取

### Phase 3.4: 集成与测试（2-3 小时）

1. ✅ 更新 `receive()` 使用新的时间戳提取
2. ✅ 集成测试：验证硬件时间戳（如果有硬件）
3. ✅ 集成测试：验证软件时间戳降级（vcan0）
4. ✅ 性能测试：验证无性能回归

---

## 5. 关键技术细节

### 5.1 依赖检查

当前依赖已满足：
- ✅ `nix = "0.30"` - 提供 `recvmsg`、`poll`、CMSG 解析
- ✅ `libc = "0.2"` - 提供 `SO_TIMESTAMPING` 常量
- ✅ `socketcan = "3.5"` - CAN 帧解析（可能需要手动解析）

### 5.2 内存安全

- 使用 `nix` crate 的安全封装（避免直接使用 `libc::recvmsg`）
- CAN 帧解析时确保数据对齐（使用 `std::ptr::read`）
- CMSG 缓冲区足够大（1024 字节，通常足够）

### 5.3 性能考虑

- **额外开销**：`recvmsg` 相比 `read_frame` 的开销很小（主要是 CMSG 解析）
- **超时处理**：`poll` + `recvmsg` 与 `read_frame_timeout` 性能相当
- **时间戳提取**：CMSG 解析的开销可忽略（微秒级）

### 5.4 兼容性

- **虚拟 CAN (vcan0)**：只返回软件时间戳（硬件时间戳为 0）
- **真实硬件**：如果支持硬件时间戳，自动使用硬件时间戳
- **旧代码**：`timestamp_us = 0` 表示时间戳不可用（向后兼容）

---

## 6. 测试策略

### 6.1 单元测试

```rust
#[test]
#[cfg(target_os = "linux")]
fn test_socketcan_timestamp_extraction() {
    let mut adapter = SocketCanAdapter::new("vcan0").unwrap();

    // 发送帧
    adapter.send(PiperFrame::new_standard(0x123, &[1, 2, 3])).unwrap();

    // 接收帧，检查时间戳
    let frame = adapter.receive().unwrap();
    assert!(frame.timestamp_us > 0, "Timestamp should be set (software timestamp)");
}

#[test]
#[cfg(target_os = "linux")]
fn test_socketcan_timestamp_monotonic() {
    let mut adapter = SocketCanAdapter::new("vcan0").unwrap();

    // 发送多个帧
    for i in 0..10 {
        adapter.send(PiperFrame::new_standard(0x100 + i, &[i as u8])).unwrap();
        std::thread::sleep(Duration::from_micros(100));
    }

    // 接收所有帧，检查时间戳单调递增
    let mut prev_ts: u64 = 0;
    for _ in 0..10 {
        let frame = adapter.receive().unwrap();
        assert!(frame.timestamp_us >= prev_ts, "Timestamp should be monotonic");
        prev_ts = frame.timestamp_us;
    }
}
```

### 6.2 集成测试

- **vcan0 测试**：验证软件时间戳工作正常
- **真实硬件测试**（如果有）：验证硬件时间戳提取
- **Pipeline 集成测试**：验证时间戳正确传递到状态更新

---

## 7. 风险评估与应对

### 7.1 风险点

1. **性能风险**：`recvmsg` 相比 `read_frame` 可能有额外开销
   - **应对**：基准测试验证，如果性能下降，考虑优化
   - **预期**：开销很小（主要是 CMSG 解析），可忽略

2. **兼容性风险**：某些旧硬件可能不支持 `SO_TIMESTAMPING`
   - **应对**：失败时降级到软件时间戳或返回 0
   - **实现**：`setsockopt` 失败时设置 `timestamping_enabled = false`

3. **代码复杂度**：手动解析 CAN 帧增加复杂度
   - **应对**：充分测试，考虑提取为独立函数
   - **注意**：使用安全的 `copy_nonoverlapping` 避免未对齐访问

4. **内存对齐风险**：直接指针强转可能导致未对齐访问（SIGBUS）
   - **应对**：使用 `std::ptr::copy_nonoverlapping` 而非指针强转
   - **已验证**：已在 3.3.1 中修正

5. **时间戳语义风险**：混用 `timestamps[1]` 和 `timestamps[2]` 可能导致时间轴错乱
   - **应对**：严格优先级：`timestamps[1]` (Transformed) > `timestamps[0]` (Software) > 不使用 `timestamps[2]` (Raw)
   - **已验证**：已在 3.2.3 中修正

### 7.2 回滚方案

如果实现出现问题：
- 可以暂时禁用时间戳提取（`timestamp_us = 0`）
- 保留 `read_frame_timeout` 作为备选路径
- 通过配置选项控制是否启用时间戳

---

## 8. 后续优化方向

1. **硬件时间戳检测优化**：在初始化时检测硬件支持（而非运行时）
2. **性能优化**：如果 `recvmsg` 开销大，考虑批量接收
3. ~~**时间戳精度**：如果需要纳秒精度，考虑扩展 `timestamp_us` 为 `u64`~~ ✅ **已实现**：`timestamp_us` 已使用 `u64` 类型，支持绝对时间戳

---

## 9. 总结

本方案通过使用 Linux `SO_TIMESTAMPING` 和 `recvmsg` API，实现了 SocketCAN 适配器的硬件时间戳支持。关键点：

1. **默认开启**：初始化时自动启用时间戳
2. **硬件优先**：优先使用硬件时间戳，不支持时降级到软件时间戳
3. **向后兼容**：时间戳不可用时返回 0
4. **性能保证**：使用 `poll` + `recvmsg` 保持超时性能

该方案对高频力控场景至关重要，能够提供微秒级精度的时间戳，满足 500Hz 控制循环的需求。时间戳与系统时间轴一致，支持多传感器融合和闭环控制。

---

## 10. 开发执行建议

### 10.1 验证顺序（推荐）

**Step 1: 环境验证**（重要）

在集成到 `SocketCanAdapter` 之前，建议先写一个极小的独立 `main.rs` 验证环境：

```rust
// examples/timestamp_verification.rs
use nix::sys::socket::{recvmsg, ControlMessageOwned, MsgFlags};
use nix::sys::uio::IoVec;
use socketcan::{CanSocket, Socket};
use std::os::unix::io::AsRawFd;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let socket = CanSocket::open("vcan0")?;
    let fd = socket.as_raw_fd();

    // 启用 SO_TIMESTAMPING
    // ... (省略代码，参考 3.1.2)

    let mut frame_buf = [0u8; 16];
    let mut cmsg_buf = [0u8; 1024];
    let mut iov = [IoVec::from_mut_slice(&mut frame_buf)];

    let msg = recvmsg::<nix::sys::socket::SockaddrStorage>(
        fd, &mut iov, Some(&mut cmsg_buf), MsgFlags::empty()
    )?;

    // 打印时间戳（验证是否能正确提取）
    for cmsg in msg.cmsgs() {
        if let ControlMessageOwned::ScmTimestamping(timestamps) = cmsg {
            println!("[1] Transformed: {:?}", timestamps[1]);
            println!("[0] Software: {:?}", timestamps[0]);
        }
    }

    Ok(())
}
```

**原因**：
- 确认 Linux 内核配置支持 `SO_TIMESTAMPING`（大部分发行版都支持）
- 验证 `nix` crate 的 CMSG 解析是否正常工作
- 独立于主代码库，便于调试和验证

**Step 2: 集成到 SocketCanAdapter**

确认环境验证通过后，将代码集成到 `SocketCanAdapter`。

**Step 3: 测试验证**

1. **vcan0 测试**：验证软件时间戳降级逻辑
2. **回环测试**：验证时间戳精度和系统时间轴一致性
3. **真实硬件测试**（如果有）：验证硬件时间戳提取

### 10.2 防御性编程要点

1. **缓冲区大小**：使用 `std::mem::size_of::<libc::can_frame>()` 而非硬编码
2. **错误处理**：`setsockopt` 失败时降级到软件时间戳，不阻塞初始化
3. **版本兼容**：如果 `socketcan` 没有 `From` trait，使用备选手动构造方案

### 10.3 代码审查清单

- [ ] 内存安全：使用 `copy_nonoverlapping` 而非指针强转
- [ ] 时间戳优先级：`timestamps[1]` > `timestamps[0]` > 不使用 `timestamps[2]`
- [ ] 缓冲区大小：使用 `size_of` 计算
- [ ] 错误处理：所有错误路径都有适当的降级策略
- [ ] 测试覆盖：vcan0 测试 + 回环测试 + 集成测试

