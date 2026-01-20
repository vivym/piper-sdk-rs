# GS-USB（`src/can/gs_usb`）实现架构与接口设计调研报告

> 目标：系统梳理当前 `gs_usb` 模块的实现分层、关键数据流/控制流、对外 API 设计特点；逐个点评现有接口的价值与问题；识别明显的历史遗留/冗余接口，并给出“删掉/改造/合并”的可执行方案与迁移路径。

---

## 1. 范围与入口（对外可见的“承诺接口”）

### 1.1 模块位置与编译条件

- `gs_usb` 仅在 **非 Linux** 平台作为默认 CAN 后端启用（Linux 下是 `socketcan`）。

```13:22:src/can/mod.rs
#[cfg(not(target_os = "linux"))]
pub mod gs_usb;

// Re-export gs_usb 类型
#[cfg(not(target_os = "linux"))]
pub use gs_usb::GsUsbCanAdapter;
```

### 1.2 “真正的外部 API”在哪里

从代码引用关系看，外部（SDK/daemon/robot builder）使用的主要是：

- `GsUsbCanAdapter::{new,new_with_serial,configure}`（创建与启动）
- `CanAdapter::{send,receive}`（收发统一抽象）
- 以及 daemon 中为了“打印扫描信息”使用 `GsUsbDevice::scan_info()`（轻量枚举，不持有 handle）

```280:326:src/bin/gs_usb_daemon/daemon.rs
fn try_connect_device(config: &DaemonConfig) -> Result<GsUsbCanAdapter, DaemonError> {
    // 1. 扫描设备
    use piper_sdk::can::gs_usb::device::GsUsbDevice;
    match GsUsbDevice::scan_info() {
        Ok(infos) => {
            eprintln!("[Daemon] Found {} GS-USB device(s):", infos.len());
            for (i, info) in infos.iter().enumerate() {
                eprintln!(
                    "  [{}] VID:PID={:04x}:{:04x} bus={} addr={} serial={:?}",
                    i,
                    info.vendor_id,
                    info.product_id,
                    info.bus_number,
                    info.address,
                    info.serial_number.as_deref()
                );
            }
        }
        Err(e) => { /* ... */ }
    }

    let mut adapter = GsUsbCanAdapter::new_with_serial(config.serial_number.as_deref())?;
    adapter.configure(config.bitrate)?;
    Ok(adapter)
}
```

---

## 2. 整体架构分层（现状）

当前 `gs_usb` 的实现可以清晰分成 4 层：

### 2.1 Adapter 层：`GsUsbCanAdapter`（对齐 `CanAdapter` 抽象）

文件：`src/can/gs_usb/mod.rs`

- **职责**：
  - 承担对外统一 CAN API（实现 `CanAdapter`）
  - 负责“启动流程编排”（`configure_with_mode`）
  - 处理 **Echo 过滤**、**Buffer overflow**、以及 **USB 一个包内多帧** 的拆包缓存（`rx_queue`）
- **核心状态**：
  - `started: bool`
  - `mode: u32`（用于判断 loopback 与 echo 处理）
  - `rx_queue: VecDeque<PiperFrame>`（处理“批量读取但逐帧返回”的语义落差）

### 2.2 Device 层：`GsUsbDevice`（USB 资源与控制/批量传输）

文件：`src/can/gs_usb/device.rs`

- **职责**：
  - USB 设备枚举与打开（`scan_info(_with_filter)` + `open(&GsUsbDeviceSelector)` 两段式）
  - interface claim/detach/reset 的处理
  - GS-USB vendor control 请求（bit timing / mode / capability）
  - bulk in/out 的 read/write（`send_raw/receive_batch`）
  - 在 macOS 相关问题上做了较多防御：reset、延迟、clear_halt、interface_release 等
- **核心状态**：
  - `interface_claimed: bool`（资源释放正确性）
  - `capability: Option<DeviceCapability>`（缓存）
  - `hw_timestamp: bool`（决定帧大小与解析逻辑）
  - `serial_number: Option<String>`（设备识别）

### 2.3 Frame 层：`GsUsbFrame`（二进制编解码）

文件：`src/can/gs_usb/frame.rs`

- **职责**：
  - GS-USB host frame 的固定格式 pack/unpack
  - 处理是否包含硬件时间戳（20/24 bytes）
  - 提供 `is_tx_echo/is_rx_frame/has_overflow` 等判定

### 2.4 Protocol & Error 层：常量/结构体定义与错误域

文件：`src/can/gs_usb/protocol.rs`, `src/can/gs_usb/error.rs`

- `protocol.rs`：request code / mode flag / CAN flag / frame size / `DeviceCapability/DeviceBitTiming/DeviceMode/...`
- `error.rs`：`GsUsbError`（rusb 错误包装 + 超时/格式/不支持波特率等）

---

## 3. 关键数据流与控制流（现状行为）

### 3.1 启动/配置控制流（`GsUsbCanAdapter::configure_with_mode`）

当前 Adapter 把启动流程“定死”为：

1. `device.claim_interface_only()`（必要前置：确保后续控制请求可发）
2. `device.set_bitrate(bitrate)`（推荐：先 set_bitrate 再 start）
3. `device.start(mode_flags)`（内部会 reset + detach/claim + capability 过滤 + MODE 控制请求）

```81:141:src/can/gs_usb/mod.rs
fn configure_with_mode(&mut self, bitrate: u32, mode: u32) -> Result<(), CanError> {
    self.device.claim_interface_only()?;
    self.device.set_bitrate(bitrate)?;
    self.device.start(mode)?;
    self.started = true;
    self.mode = mode;
    self.rx_queue.clear();
    Ok(())
}
```

**特点**：

- 这是一个“强编排”的启动流程，调用方只需 `configure(bitrate)` 即可。
- 该流程在历史上曾与外部参考实现强绑定（现已改为“推荐流程”表述，避免特定语言/实现名称进入长期维护面）。

### 3.2 发送数据流（`CanAdapter::send`）

- `PiperFrame -> GsUsbFrame` 做一次映射
- bulk out `send_raw` “fire-and-forget”，不等待 echo

**特点**：适合 SDK 场景（低开销/低延迟），但会丢失“发送确认”的语义（需要在接口层明确）。

### 3.3 接收数据流（`CanAdapter::receive`）

关键点：**USB Bulk IN 一个包可能包含多个 GS-USB frame**。该实现用 `rx_queue` 把“批量读取”转成“逐帧返回”：

- 若 `rx_queue` 非空，直接 pop 返回
- 否则 `receive_batch(timeout=2ms)` 读一个包，拆出 N 帧：
  - 非 loopback 时过滤 `tx echo`
  - 遇到 overflow 立即报 `CanError::BufferOverflow`
  - 否则转换为 `PiperFrame` 入队
  - 队列 pop 出第一个返回

**特点**：

- 这是一个比较“底层事实驱动”的设计：用队列补齐 API 语义差（trait 只支持“单帧 receive”）。
- 但 **2ms 的超时强耦合** 到实现：在 macOS 调度/USB microframe 抖动下，可能放大为大量 `Timeout`（daemon 里也将其视为“正常超时”并 continue）。

---

## 4. 对外接口设计点评（逐个模块/逐个函数）

下面的点评把接口分为三类：

- **稳定对外接口**：外部调用方（SDK/daemon）依赖，建议保持稳定
- **内部实现接口**：当前是 `pub` 但实际上只有内部用/未来才用，建议收敛可见性或搬迁
- **疑似历史遗留/可删除接口**：目前无调用点，且与当前“推荐流程/单一路径”的设计取向冲突，建议删除或改造

---

## 4.1 `GsUsbCanAdapter`（`src/can/gs_usb/mod.rs`）

### 4.1.1 `new()` / `new_with_serial(...)`

- **现状特点**：
  - `new()` 默认选取扫描到的第一个设备；多个设备时 warn
  - `new_with_serial` 支持按序列号过滤，失败路径较清晰
- **潜在问题**：
  - 选择“第一个设备”的策略可预期性较弱（多设备场景下容易连错）
  - “序列号过滤”依赖 USB descriptor 且大小写敏感；对用户而言可能不够友好
- **建议改进**：
  - 保留 `new_with_serial`，但建议新增：
    - `new_with_selector(Selector)`：支持 VID/PID 白名单、序列号正则/忽略大小写、或按 bus/address 指定
  - `new()` 的行为建议在文档上明确“不可控，生产不建议用”

### 4.1.2 `configure(...) / configure_loopback(...) / configure_listen_only(...)`

- **现状特点**：
  - 把启动流程显式分成三种模式：NORMAL/LOOPBACK/LISTEN_ONLY，并默认启用 HW_TIMESTAMP
  - 对调用方非常友好（少数 API + 开箱即用）
- **潜在问题**：
  - `CanAdapter` trait **并不包含 configure**：这意味着“统一抽象”在启动阶段被破坏（Builder 需要分别处理 SocketCAN 与 GS-USB）
  - 对外暴露了 3 个“模式专用函数”，但模式组合实际上是 bitflag，可以更通用
- **建议改进**：
  - 方案 A（更 Rust/更抽象）：把 `configure` 纳入 trait，例如 `fn configure(&mut self, config: CanConfig) -> Result<(), CanError>`
  - 方案 B（更保守/兼容）：保留现有 `configure_*`，同时新增一个统一接口：
    - `configure_with_options(bitrate, options: GsUsbOptions)`（或 `GsUsbConfig`）
  - 并让 `PiperBuilder` 只调用“统一 configure”，减少平台分支

### 4.1.3 `Drop` 行为

- **现状特点**：
  - drop 时若 `started` 则发送 RESET（通过 `device.start(GS_CAN_MODE_RESET)` 实现），并释放 interface
  - 对“避免 macOS 状态残留”非常重要
- **潜在问题**：
  - 这里调用 `device.start(GS_CAN_MODE_RESET)` 在语义上有点怪：`start()` 同时承担 reset/detach/claim/MODE 等逻辑，传 RESET 只是“凑巧可用”
- **建议改进**：
  - 在 device 层提供专门的 `set_mode_reset()` 或 `stop()`（但要确认 stop 的实现/命名与语义一致），由 drop 调用
  - 把“释放接口”做成明确的 RAII 组件（见第 6 节改造方案）

### 4.1.4 `receive()` 的超时与语义

- **现状特点**：内部使用 `receive_batch(Duration::from_millis(2))`
- **风险**：
  - 2ms 在 macOS 下很容易造成大量超时，daemon 目前把 Timeout 当作正常路径，会导致“热循环”与 CPU 消耗
  - `CanAdapter::receive()` 的注释语义是“阻塞直到收到有效数据帧或超时”，但超时阈值对调用方不可控
- **建议改进**：
  - 把 `receive` 超时变成配置项（默认建议 >= 50ms，daemon 场景可用 0/阻塞）
  - 或者分离 API：
    - `receive_blocking()`：不超时（或由外部取消）
    - `receive_timeout(dur)`：明确由调用者指定

---

## 4.2 `GsUsbDevice`（`src/can/gs_usb/device.rs`）

### 4.2.1 `scan_info(_with_filter)` / `open(&GsUsbDeviceSelector)` / `serial_number()`

- **现状特点**：
  - `scan_info(_with_filter)` 用于枚举信息（不持有 handle），`open(&selector)` 负责真正打开设备（持有 handle）
  - 能满足 daemon 的“列出可见设备”需求，同时避免枚举阶段占用设备资源
- **潜在问题**：
  - 选择器策略需要明确（serial 大小写敏感、bus/address 稳定性等）
- **建议改进**：
  - 保持“两段式”并逐步增强 selector（例如忽略大小写/支持更多定位字段）

### 4.2.2 `claim_interface_only()` vs `prepare_interface()`

- **现状特点**：
  - `claim_interface_only()` 被 `GsUsbCanAdapter::configure_with_mode()` 依赖，是当前启动流程的关键一步
  - `prepare_interface()` 做了更多事情：detach/claim/reset/sleep，但目前 **没有调用点**
- **结论（强烈）**：
  - `prepare_interface()` 属于 **“历史遗留/实验性接口”**：当前设计路线是“推荐流程/单一路径”，而该函数引入多套语义
- **建议处置**：
  - 若确实不需要：**删除** `prepare_interface()`（或至少降为 `pub(crate)` 并加 `#[allow(dead_code)]` 注释说明原因）
  - 若是为了解决 macOS 的“data toggle/STALL”问题：应把它的能力**合并到单一且被调用的启动路径**（见第 6 节）

### 4.2.3 `clear_usb_endpoints()`

- **现状**：无调用点
- **设计意图**：解决超时后 endpoint halt/data toggle 不同步
- **问题**：
  - 如果它是必要的恢复手段，就不应该“永远不调用”
  - 如果它不是必要的，就不应作为 public API 长期暴露
- **建议处置**：
  - 方案 A：合并到 `send_raw`/`receive_batch` 的错误恢复里（例如遇到特定错误自动 clear_halt）
  - 方案 B：保留但降级为 `pub(crate)` 并在 README/文档明确何时使用

### 4.2.4 `send_host_format()`

- **现状**：无调用点
- **明显冲突**：
  - `protocol.rs` 注释写“必须发送，但可忽略错误”
  - 但历史注释曾写“外部实现没有发送 HOST_FORMAT…”，现已统一为“默认不发送（见兼容性策略）”
- **建议处置**：
  - 结论建议：把 `send_host_format()` 标记为 **兼容性钩子**，默认不调用
  - 实际改造：
    - 降级为 `pub(crate)` 或 `pub(super)`（避免外部依赖）
    - 或改为 `StartOptions { handshake: Option<HostFormatHandshake> }`
      - 默认 `None`
      - 仅在特定设备/固件检测到需要时启用

### 4.2.5 `start(flags: u32)` / `stop()`

- **现状特点**：
  - `start()` 做了很多事：reset +（可能）detach/claim + delay + capability + flags 过滤 + 设置 `hw_timestamp` + MODE 控制请求
  - `stop()` 发送 RESET 模式并设置 `started=false`
- **问题**：
  - `start()` 的语义过重：它既是“启动”，又包含“reset + 重建接口状态 + 能力协商”
  - 它还会**过滤 flags**，但外部无法拿到“最终生效 flags”，只能猜
- **建议改进**：
  - 让 `start()` 返回一个结构，明确“协商结果”：
    - `StartResult { effective_flags, capability, hw_timestamp_enabled }`
  - 或者拆分：
    - `reset()`、`ensure_interface_claimed()`、`negotiate_capability()`、`set_mode(...)`

### 4.2.6 `receive_raw()` vs `receive_batch()`

- **现状**：
  - `receive_raw()` 文档明确警告“只读第一个帧，其余丢弃”
  - `receive_batch()` 才是正确的高吞吐路径
- **建议处置**：
  - `receive_raw()` 很容易被误用，且当前没有调用点：
    - 建议 **删除** 或降为 `pub(crate)`，仅用于测试/调试
  - 对外应只保留 `receive_batch()`（或在 adapter 层只暴露“单帧语义”，但内部统一用 batch）

### 4.2.7 `last_timing` / `started` 字段

- **现状**：`last_timing` 仅被写入未被读取；`started` 在 device 层与 adapter 层重复
- **建议处置**：
  - `last_timing` 若无明确用途建议删除（或用于 debug/status API）
  - `started` 建议只保留一处作为真实状态源（通常是 session/adapter 层）

---

## 4.3 `GsUsbFrame`（`src/can/gs_usb/frame.rs`）

### 4.3.1 编解码策略

- **优点**：
  - 不依赖 `repr(packed)`，完全手动 pack/unpack，跨平台稳健
  - 对 timestamp 的可选字段处理清晰
- **可改进点**：
  - `is_tx_echo()` 当前实现是“echo_id != RX_ECHO_ID”，语义略宽泛（任何非 `0xFFFF_FFFF` 都算 echo）
    - 若未来协议扩展（或设备 bug）导致 echo_id 出现其他值，过滤策略可能误伤
  - 建议把 echo_id 语义明确化：
    - 例如：`echo_id == GS_USB_ECHO_ID (0)` 表示“本端 tx”，其余为“echo token”

---

## 4.4 `protocol.rs`（协议常量与结构体）

### 4.4.1 常量与注释一致性

- 目前最明显的不一致是 `HOST_FORMAT` 的注释立场（见上）。
- 建议统一策略：以“允许兼容，但默认不需要”为主线，注释与实现一致。

### 4.4.2 结构体 pack/unpack

- `DeviceCapability::unpack` / `DeviceBitTiming::pack` 等实现简洁可靠
- 建议增强边界校验（仅在 debug/assert 层也可）：
  - unpack 前检查输入长度（虽然 caller 已检查，但分散在多处）

---

## 4.5 `error.rs`（错误域）

### 4.5.1 `GsUsbError` vs `CanError` 的映射问题

- 目前 Adapter 往往把 `GsUsbError` 直接转成 `CanError::Device(String)`，导致：
  - 结构化信息丢失（Timeout/NoDevice/AccessDenied 等）
  - 上层很难做策略性恢复（例如“热拔插重连” vs “bitrate 不支持”）
- 建议改造：
  - 在 `CanError` 增加更细粒度枚举（或用 `thiserror` + source 保留底层错误）
  - 至少可以保留 `GsUsbError` 作为 `source`，避免只剩字符串

---

## 5. 明确指出：哪些接口“可能已经不需要”（建议删/改/合并）

结合全仓库调用点搜索，以下接口当前属于“对外 public 但几乎无人调用”的高风险集合（容易误用/容易形成长期 API 负债）：

- `GsUsbDevice::send_host_format()`：**无调用点**；与“默认不发送”的策略相冲突
  - **建议**：降级为 `pub(crate)` 或删除；若保留则放入 `StartOptions` 作为兼容开关
- `GsUsbDevice::prepare_interface()`：**无调用点**；与当前启动路径并存造成“多套启动语义”
  - **建议**：删除或合并进 `start()` 的内部步骤（并只保留一条启动路径）
- `GsUsbDevice::clear_usb_endpoints()`：**无调用点**；若确实用于恢复就应该自动化
  - **建议**：合并到 `send_raw/receive_batch` 的错误恢复里；否则降级/删除
- `GsUsbDevice::stop()`：**无调用点**；drop 走的是“`start(RESET)`”这种非直观方式
  - **建议**：保留 `stop()`，并让 `Drop` 调用 `stop()`；或反过来删掉 stop 并提供更语义化的 reset API
- `GsUsbDevice::receive_raw()`：**无调用点** 且带“只读一帧会丢数据”的危险语义
  - **建议**：删除或降级为 `pub(crate)`（仅用于测试/调试）
- 字段层面：
  - `GsUsbDevice.last_timing`：只写不读 → **建议删除**或用于 status API
  - `GsUsbDevice.started` 与 `GsUsbCanAdapter.started` 重复 → **建议统一状态源**

---

## 6. 推荐的改造方向（把接口收敛为“难误用、可扩展”的形态）

### 6.1 设计目标

- **单一启动路径**：避免 `claim_interface_only/prepare_interface/start` 多套并存
- **显式配置对象**：把超时、模式、bitrate、是否 reset、是否 handshake 等显式化
- **显式协商结果**：返回 effective_flags/capability，避免调用方猜测
- **RAII 资源管理清晰**：interface claim/release 与“started”生命周期更明确

### 6.2 建议的 API 形态（示意）

- `GsUsbDevice` 更聚焦于“USB 传输与控制请求”：
  - `GsUsbDevice::open(selector) -> GsUsbDevice`
  - `device.ensure_claimed()`（内部处理 detach/claim）
  - `device.reset_if_needed(...)`
  - `device.get_capability()`
  - `device.set_bitrate(...)`
  - `device.set_mode(...)`
  - `device.bulk_send/ bulk_receive_packet(...)`

- 新增 `GsUsbSession`（或把现有 adapter 做成 session）：
  - `GsUsbSession::start(device, config) -> (session, StartResult)`
  - `session.send(frame)` / `session.receive(...)`
  - `Drop` 做 stop + release_interface

### 6.3 渐进式迁移策略（避免一次性大重构）

1. **第一步（低风险）**：把“无调用点的 public API”先降级为 `pub(crate)`，并补充文档解释
2. **第二步（中风险）**：引入 `GsUsbConfig`（至少包括 rx_timeout），Adapter 的 `configure_*` 改为调用统一的 `configure_with_config`
3. **第三步（中风险）**：`start()` 返回 `StartResult`（有效 flags/capability），并在日志里打印
4. **第四步（较大改动）**：拆分 `scan_info` 与 `open`，让 daemon 不再持有多个已打开 handle
5. **第五步（可选）**：把 `configure` 纳入 `CanAdapter` trait 或引入更高层的 `Transport` 抽象，消除平台分支

---

## 7. 总结（现有设计的优点与主要短板）

### 7.1 优点（保留并强化）

- **分层明确**：adapter/device/frame/protocol/error 的边界清晰
- **推荐流程**：启动流程采用“推荐流程/协议约束”叙述，避免引入外部实现名称
- **吞吐正确性**：通过 `receive_batch + rx_queue` 解决了 USB 单包多帧导致的丢包风险
- **资源清理意识强**：Drop 中释放 interface 的方向正确（尤其 macOS）

### 7.2 主要短板（优先改）

- **public API 漫溢**：device 层暴露了多组未使用/易误用的接口（明显历史遗留）
- **启动语义过重且不透明**：`start()` 既 reset 又协商又设置模式，且过滤 flags 的结果不返回
- **超时硬编码**：`receive()` 内部 2ms 直接影响 daemon/SDK 行为且调用方不可控
- **错误映射信息丢失**：字符串化导致上层难以做策略性恢复

---

## 8. 建议的后续行动（可执行清单）

- **A（接口收敛）**：将 `GsUsbDevice::{send_host_format,prepare_interface,clear_usb_endpoints,receive_raw}` 降级为 `pub(crate)` 或删除
- **B（可配置性）**：为 `GsUsbCanAdapter::receive()` 引入可配置超时（默认值也要在文档里说明）
- **C（协商结果可见）**：`start()` 返回 effective_flags/capability，并在 `configure` 后日志打印
- **D（枚举/打开分离）**：为 daemon 提供 `scan_info`，避免扫描阶段就打开/持有设备句柄
- **E（错误域改造）**：保留底层 `GsUsbError` 作为 `source`，减少字符串化



