# GS-USB 重构执行 TODO_LIST（问卷版）

> 用法：逐条回答“是/否/不确定”，并填上 owner/优先级/验收标准。
> 目标：把 `gs_usb` 的 public API 收敛、启动路径单一化、超时可配置、错误域更可恢复，且不破坏 daemon/robot builder 的现有行为。

---

## 进度记录（持续更新）

- **2026-01-20**
  - **已完成**
    - **P0/API 收敛（第一阶段）**：将 `GsUsbDevice::{send_host_format,prepare_interface,clear_usb_endpoints,receive_raw,stop}` 从 `pub` 降级为 `pub(crate)`，避免外部形成长期依赖。
    - **P0/停止语义统一**：`GsUsbCanAdapter::Drop` 改为调用 `GsUsbDevice::stop()`（不再用 `start(RESET)` 这种隐式语义）。
    - **P1/接收超时可配置**：`GsUsbCanAdapter` 新增 `rx_timeout` 字段（默认 50ms）与 `set_receive_timeout(Duration)`，`receive()` 不再硬编码 2ms。
    - **P0/API 收敛（第二阶段：直接删除遗留接口）**：
      - 已从 `GsUsbDevice` **删除**：`send_host_format()`、`prepare_interface()`、`clear_usb_endpoints()`、`receive_raw()`
      - 保留并明确：`stop()` 作为语义化停止/复位入口（供 Drop 调用）
    - **P1/启动协商结果可见（第一阶段）**：
      - `GsUsbDevice::start()` 现在返回 `StartResult { effective_flags, capability, hw_timestamp }`
      - `GsUsbCanAdapter::configure_with_mode()` 会在日志中输出 `effective_flags` / `fclk_can` / `hw_timestamp`
    - **P2/枚举与打开分离（第一阶段）**：
      - 新增 `GsUsbDeviceInfo` 与 `GsUsbDevice::scan_info(_with_filter)`：扫描阶段不再返回持有 handle 的 `GsUsbDevice`
      - `gs_usb_daemon` 的“打印设备列表”改用 `scan_info()`，输出 `VID:PID/bus/addr/serial`
    - **P2/枚举与打开分离（第二阶段：两段式 open）**：
      - 新增 `GsUsbDeviceSelector` 与 `GsUsbDevice::open(&selector)`：支持按 serial 或 bus/address 打开设备
      - `GsUsbCanAdapter::new_with_serial()` 改为 `scan_info_with_filter -> open` 的两段式流程（枚举阶段不再持有 handle）
    - **P2/旧代码清理（scan/scan_with_filter）**：
      - 已删除 `GsUsbDevice::scan()` 与 `GsUsbDevice::scan_with_filter()`（返回持有 handle 的设备列表的旧模式）
      - 统一使用：`scan_info(_with_filter)`（枚举）+ `open(&selector)`（打开）
    - **P2/错误域结构化（第一阶段）**：
      - `CanError::Device(String)` 升级为 `CanError::Device(CanDeviceError{kind,message})`，新增 `CanDeviceErrorKind`
      - `gs_usb`：把 `GsUsbError` 映射到结构化 kind（如 `NoDevice/AccessDenied/NotFound/UnsupportedConfig/InvalidFrame/...`）
      - `gs_usb_daemon`：按 kind 决定是否进入 `Disconnected`（例如 `NoDevice/NotFound/AccessDenied` 立即断开重连）
  - **待验证**
    - daemon 在“无帧”时 CPU 是否显著下降（已将 daemon receive timeout 调大；需要现场验证）
    - loopback/高吞吐下 timeout 行为是否符合预期（建议用 direct_test/daemon 压测复核）
    - capability/flags 输出对诊断是否足够（是否还需要打印 serial/VIDPID/bus/address）
    - 直连/daemon 启动日志：确认已包含 serial/VIDPID/bus/addr（现场验证）

---

## 0. 元信息（必填）

- **负责人（Owner）**：
- **评审人（Reviewer）**：
- **预计迭代（Milestone / Sprint）**：
- **影响平台**：macOS / Windows / Linux（daemon 模式）：
- **主要使用场景**：
  - 直连 `GsUsbCanAdapter`
  - `gs_usb_daemon` + `GsUsbUdpAdapter`
  - 仅用于设备枚举/诊断

---

## 1. API 收敛与可见性（P0）

### 1.1 `GsUsbDevice::send_host_format()`（疑似历史遗留）

- **问题**：是否存在真实设备/固件必须依赖 HOST_FORMAT 握手才能正常工作？
  - 证据来源（设备型号/VIDPID/固件版本/复现步骤）：
- **选择**：
  - [ ] 删除（完全移除 public API）
  - [ ] 降级为 `pub(crate)`（仅内部/测试可用）
  - [ ] 保留 public，但改为 `StartOptions.handshake` 显式开关（默认关闭）
- **验收标准**：
  - [ ] 现有设备在默认配置下可正常收发
  - [ ] 若开启 handshake，能覆盖某类特定设备，并有文档说明何时开启
- **影响面**：
  - 调用方是否有使用该函数？（grep 结果/文件路径）
- **回滚策略**：
  - 若删除后出现兼容性问题，是否能以 feature flag 快速恢复？

### 1.2 `GsUsbDevice::prepare_interface()`（无调用点但副作用重）

- **问题**：它当前解决的具体问题是什么？（例如 macOS reset/claim 顺序、段错误、枚举抖动等）
  - 复现脚本/日志/设备信息：
- **选择**：
  - [ ] 删除
  - [ ] 降级为 `pub(crate)` + 注释说明“仅历史/实验”
  - [ ] 合并到唯一启动路径（例如 `start()` 的内部步骤）
- **验收标准**：
  - [ ] 代码库内不再存在“多套启动语义”
  - [ ] 启动流程可用单元测试/集成测试验证（至少 mock 层面）
- **风险**：
  - reset/claim/detach 时序变更是否影响 macOS？

### 1.3 `GsUsbDevice::clear_usb_endpoints()`

- **问题**：目前靠 `send_raw()` 的超时后 `clear_halt(endpoint_out)` 是否足够？
  - 是否观察到 IN 端点也需要 clear_halt？
  - 是否观察到“Data toggle 不同步”导致设备收不到包？
- **选择**：
  - [ ] 删除（若无实际必要）
  - [ ] 降级为 `pub(crate)`（仅内部恢复流程调用）
  - [ ] 合并到错误恢复（在特定错误条件下自动触发）
- **验收标准**：
  - [ ] 连续超时/强杀进程/热拔插后无需重插即可恢复
  - [ ] 恢复流程有明确日志与指标

### 1.4 `GsUsbDevice::receive_raw()`（危险语义：只读一帧会丢数据）

- **问题**：是否有任何调用方确实需要“只读第一个帧”的低层 API？
- **选择**：
  - [ ] 删除
  - [ ] 降级为 `pub(crate)`（仅测试/调试）
- **验收标准**：
  - [ ] 对外 API 只提供“不会静默丢帧”的接收能力

### 1.5 `GsUsbDevice::stop()` 与 Adapter Drop 语义

- **问题**：Drop 里当前通过 `device.start(GS_CAN_MODE_RESET)` 来“停止”是否应替换为更语义化的 `stop()`？
- **选择**：
  - [ ] 保留 `stop()` 并让 Drop 调用 `stop()`
  - [ ] 删除 `stop()`，提供 `reset()`/`set_mode_reset()` 这样的明确 API
- **验收标准**：
  - [ ] Drop 时不会 panic，不会导致资源泄漏
  - [ ] macOS 下连续运行/重启不会出现 “Access denied / claim 失败 / 状态残留”

### 1.6 字段清理：`last_timing` / `started`（状态重复/只写不读）

- **问题**：
  - `last_timing` 是否有任何诊断/状态查询需求？
  - `GsUsbDevice.started` 与 `GsUsbCanAdapter.started` 哪个才是“真实状态源”？
- **选择**：
  - [ ] 删除 `last_timing`
  - [ ] 将 `last_timing` 纳入 status API（可观测性）
  - [ ] 统一 started 状态源（只保留一处）
- **验收标准**：
  - [ ] 结构体字段与行为一致（无“写了但永远不读”的状态）

---

## 2. 启动路径单一化与协商结果可见（P1）

### 2.1 `start()` 过滤 flags 但不返回 effective_flags

- **问题**：调用者是否需要知道“最终启用的 flags”（尤其 HW_TIMESTAMP 是否真正启用）？
- **行动**：
  - [ ] 让 `GsUsbDevice::start(...)` 返回 `StartResult { effective_flags, capability, hw_timestamp }`
  - [ ] 或提供 `device.effective_flags()`/`device.capability()` 的显式 getter
- **验收标准**：
  - [ ] configure 后日志打印 effective_flags 与 capability.fclk_can
  - [ ] 上层不再“猜测”时间戳字段是否存在

### 2.2 `claim_interface_only()` 与 `start()` 内部 claim/reset 重复

- **问题**：目前“先 claim 再 set_bitrate，再 start（start 内 reset）”是否存在冗余或时序风险？
- **行动**：
  - [ ] 把接口准备收敛到一个内部方法：`ensure_interface_ready_for_control()`
  - [ ] 明确 `start()` 是否允许/默认执行 reset（引入 `StartOptions { reset: bool }`）
- **验收标准**：
  - [ ] 启动流程只有一个入口点（对外）
  - [ ] 任何 reset/detach/claim 逻辑都有单元化封装与注释说明

---

## 3. 接收超时与阻塞语义（P1）

### 3.1 `GsUsbCanAdapter::receive()` 内部 2ms 超时硬编码

- **问题**：对你当前业务（机械臂/实时控制/daemon）来说，“默认 receive 超时”应该是多少？
  - 建议值（ms）：
  - 是否需要“永不超时（阻塞）”模式：
- **行动**：
  - [ ] 引入 `GsUsbConfig { rx_timeout: Duration, ... }`
  - [ ] 或新增 `receive_timeout(Duration)`（调用者指定）
  - [ ] daemon 侧使用阻塞（或较大超时），避免热循环
- **验收标准**：
  - [ ] daemon CPU 占用在无帧时保持低水平
  - [ ] SDK 的 `receive()` 语义与文档一致

### 3.2 “单帧 trait”与“批量包现实”的语义对齐

- **问题**：是否需要对外提供批量接收 API（减少锁竞争、提高吞吐）？
- **行动**：
  - [ ] 在 trait 之外提供 `receive_batch_frames()`（返回 Vec<PiperFrame>）
  - [ ] 或在 daemon 内部直接消费 batch（减少 `RwLock` 写锁持有时间）
- **验收标准**：
  - [ ] 高吞吐场景无丢包（已有 `rx_queue`，需验证）
  - [ ] 锁竞争与延迟满足目标

---

## 4. 设备枚举与打开分离（P2）

### 4.1 `scan_with_filter` 返回已打开 handle 的 `GsUsbDevice`

- **问题**：daemon 的“打印设备列表”是否需要真正打开 handle？
- **行动**：
  - [ ] 新增 `scan_info() -> Vec<GsUsbDeviceInfo>`（不持有 handle）
  - [ ] 新增 `open(selector) -> GsUsbDevice`
  - [ ] daemon 改用 `scan_info`
- **验收标准**：
  - [ ] 枚举阶段不占用设备资源
  - [ ] 多设备场景选择逻辑可控（序列号/VIDPID/bus/address）

---

## 5. 错误域与可恢复性（P2）

### 5.1 `GsUsbError -> CanError` 字符串化导致信息丢失

- **问题**：上层需要区分哪些错误来做策略恢复？
  - [ ] USB Timeout
  - [ ] NoDevice（热拔插）
  - [ ] AccessDenied/Busy（接口被占用）
  - [ ] UnsupportedBitrate（配置错误）
  - [ ] InvalidFrame（协议/解析异常）
- **行动**：
  - [ ] 在 `CanError` 增加结构化枚举（或把 `GsUsbError` 作为 source）
  - [ ] daemon 根据错误类型进入不同状态机分支（例如立即重连 vs 延迟重试）
- **验收标准**：
  - [ ] 日志能准确显示错误根因（非仅字符串）
  - [ ] 热拔插恢复稳定，且不会误判为“总线错误”

---

## 6. 文档与注释一致性（P2）

### 6.1 HOST_FORMAT 注释冲突

- **问题**：最终立场是什么？（必须/可选/默认不需要）
- **行动**：
  - [ ] 统一 `protocol.rs` 与 `device.rs/mod.rs` 中相关注释
  - [ ] 在 `docs/v0/gs_usb/` 增加一段“兼容性策略”说明
- **验收标准**：
  - [ ] 注释与行为一致，且新人阅读不会误解

---

## 7. 测试与验证（P1/P2）

### 7.1 回归用例（最小集）

- **必须通过**：
  - [ ] `gs_usb_daemon` 启动 + 配置 + 持续接收（无帧时 CPU 低）
  - [ ] 直连 `GsUsbCanAdapter` 收发（含 HW_TIMESTAMP）
  - [ ] loopback 模式 echo 行为符合预期（是否过滤 echo 的规则明确）
- **建议补充**：
  - [ ] 多设备选择（序列号过滤/默认策略）
  - [ ] 热拔插恢复（NoDevice -> 重连成功）
  - [ ] 连续超时后的 endpoint 恢复（无需重插）

### 7.2 观测指标（建议）

- **指标**：
  - [ ] RX/TX 帧计数、超时次数、overflow 次数
  - [ ] 平均/尾延迟（receive 到上层回调）
  - [ ] daemon CPU 使用率（无帧/有帧）
- **日志**：
  - [ ] 启动时打印 effective_flags / capability.fclk_can / serial

---

## 8. 最终输出物（交付检查）

- [ ] public API 变更清单（破坏性变更需要迁移指南）
- [ ] 新/改配置结构体与默认值说明
- [ ] 更新后的 `docs/v0/gs_usb/*` 文档索引（新增文件链接）
- [ ] daemon/robot builder 的调用面已同步


