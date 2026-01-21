## Piper SDK CAN IO 线程模型改进方案报告

日期：2026-01-20
仓库：`piper-sdk-rs`
关联文档：
- `docs/v0/can_io_threading_investigation_report.md`（现状与问题分析）
- `docs/v0/survey.md`（方案评审与进一步建议）

---

### 0. 目标与约束

**目标：**

- 提升 Piper SDK 在 **力控 / 高带宽闭环控制（500Hz–1kHz）** 场景下的实时性与可预期性。
- 避免单个 IO 操作（尤其是 GS-USB 写）导致整条控制链路长时间卡死。
- 在不大幅破坏现有 API 的前提下，逐步演进到 **收发解耦 + 全链路有超时预算** 的架构。

**约束：**

- 保持当前对外核心 API（`Piper`、`PiperBuilder`、`CanAdapter` trait）在短期内尽量兼容。
- 以 `std::thread + crossbeam-channel` 为主，不引入 Tokio 等 async 运行时（参见 survey 对 Tokio 实时性的担忧）。
- 优先支持两类后端：SocketCAN（Linux）与 GS-USB（直连 / 守护进程）。

---

### 1. 问题回顾（来自 investigation + survey）

**1.1 单线程 IO Loop 的 Head-of-Line Blocking**

- 当前 `io_loop` 模型：
  `loop { can.receive() -> 解析&状态更新 -> drain cmd_rx & can.send() }`
- 收、发、解析、状态更新都在 **同一个 OS 线程** 上运行。
- 一旦 `can.send()` 或 `can.receive()` 中任何一个调用长时间阻塞，**整个循环停转**：
  - 传感器反馈（关节位置 / 速度 / 力矩）不再更新。
  - 控制命令无法发出或严重延迟。
  - 上层控制逻辑无法及时感知真实状态，导致力控场景下潜在危险（survey 中称为“盲目失控”）。

**1.2 GS-USB 写路径的 1000ms 超时**

- GS-USB 实现中，`send_raw()` 的 USB Bulk OUT 超时为 **1000ms**。
- 在设备忙、总线拥塞、端点 STALL 或 USB 控制器异常时，单次写调用可能阻塞接近 1 秒。
- 因为写在 `io_loop` 同一线程里，这种阻塞会直接将 **状态读取和命令发送一并卡死**。

**1.3 GS-USB 读超时过大，影响命令时效**

- `GsUsbCanAdapter` 默认 `rx_timeout = 50ms`，为了避免 macOS 上的热循环。
- 对于高频控制而言，这会导致：
  - 无帧/低频帧时，`io_loop` 可能 50ms 才醒一次。
  - 命令 drain 逻辑在 `receive()` 之后执行，导致在“安静总线”情况下，命令也会被动延迟到 50ms 级别。

**1.4 trait/配置层没有统一抽象“超时预算”**

- `PipelineConfig.receive_timeout_ms` 在 `io_loop` 中目前未真正下沉到各 adapter。
- SocketCAN/GS-USB/GsUsbUdp 各自有不同的超时配置 API，trait 层未统一表达：
  - 难以在一个地方整体配置控制链路的延迟预算。

**1.5 survey 的关键建议**

- **不要把希望寄托在 non-blocking receive 上**：只要 send 和 receive 在同一线程，send 一旦卡住，receive 也会跟着挂起。
- **把收发物理隔离到两个线程**，从“串联”变成“并联”，确保 RX 的“状态新鲜度”不被 TX 故障拖累。
- 对所有 IO 操作引入**明确的 Timeout 预算**，并在负载过高或设备异常时 **宁可丢包，不要长阻塞**。
- 发送侧要设计合适的 **丢弃策略（只保留最新命令）**，避免排队导致延迟累积。

---

### 2. 总体改进思路

整体演进路线分三个阶段：

- **阶段 0（架构不改 / 风险最小）：**
  在现有单线程 IO Loop 模型下，引入和收敛超时配置，先降低“最坏情况延迟”，为后续改造铺路。

- **阶段 1（关键架构升级）：**
  引入 **双线程 IO 模型**（RX 线程和 TX 线程分离），彻底消除发送阻塞对反馈状态的连锁影响。

- **阶段 2（长期演进）：**
  在 trait/API 层统一超时/非阻塞能力，建立可重复的延迟&抖动测试基准，使 Piper SDK 在实时性方面可配置、可验证、可回归。

下面分别展开这三阶段的具体方案。

---

### 3. 阶段 0：在现有单线程模型上“止血”

> 目标：不改变线程结构，仅通过合理的超时和 Loop 调度顺序，降低最坏延迟和平均抖动。
> 适合作为“近期在主分支落地”的小步改进。

#### 3.1 收敛 receive 超时配置到 `PipelineConfig`

**设计：**

- 将 `PipelineConfig.receive_timeout_ms` 显式用于配置各后端适配器的读超时。
- 在 `PiperBuilder::build()` 或 `Piper::new()` 完成 CAN adapter 初始化后，根据后端类型调用适配器特定 API：
  - SocketCAN：`set_read_timeout(Duration::from_millis(config.receive_timeout_ms))`
  - GS-USB：`set_receive_timeout(Duration::from_millis(config.receive_timeout_ms))`
  - GsUsbUdp：设置 socket 读超时 / 轮询超时时间为 `receive_timeout_ms`

**建议的默认值调整：**

- 力控 / 高频控制默认配置：
  - `receive_timeout_ms = 2`（与 SocketCAN 当前默认保持一致）
  - `frame_group_timeout_ms` 保持 10ms 或按实际协议调整。
- 普通监控 / 低频控制可以通过 Builder 显式设置较大的 receive_timeout（例如 20–50ms），以减少 CPU 占用。

**收益：**

- 让 `io_loop` 的调度周期对所有后端统一到一个量级（毫秒级，而不是 SocketCAN 2ms vs GS-USB 50ms 的混乱状态）。
- 避免控制线程命令在 GS-USB 场景下因为“安静总线”而被动延迟到 50ms。

#### 3.2 调整 `io_loop` 中命令 drain 的顺序与策略

**当前模式：**

- `receive()` 返回（数据/Timeout/错误）之后，循环末尾用 `while let Ok(cmd_frame) = cmd_rx.try_recv()` 把所有待发送命令全部发完。

**改进建议：**

- 在每轮循环的 **开始** 就先尝试发送一轮命令：

  - 在 `receive()` 之前执行一小段：
    - `for _ in 0..N { if let Ok(frame) = cmd_rx.try_recv() { can.send(frame)? } else { break } }`
  - N 可配置（如 1 或 2），避免在命令洪峰时 send 过长占用时间。

- 仍然保留在循环末尾的 drain 逻辑，但可以限制最大发送帧数和/或累计耗时：
  - 例如：每轮最多发送 32 帧，或者最多花 200µs。

**考虑：**

- 这样做可以减少“无反馈时命令长时间得不到发送”的情况。
- 但也要意识到：这会略微增加“发送阻塞影响接收”的概率，所以这是在阶段 0 的“折中措施”，**必须配合合理的 send 超时**（见 3.3）。

#### 3.3 限制 GS-USB 写超时（临时策略）

**现状：**

- GS-USB `send_raw()` 使用 1000ms Bulk OUT 超时，以提升在高负载/loopback 情况下的可靠性。

**阶段 0 临时策略（仅在“实时模式”下启用）：**

- 增加一个“实时模式”或“严格实时标志”（例如通过 Builder 或配置标记）。
- 在实时模式下：
  - 将 Bulk OUT 超时从 1000ms 降到 2–5ms。
  - 如果连续出现写超时：
    - 记录计数；超出阈值后向上层报告错误（例如通过状态、回调或日志）。
    - 可选择直接关闭适配器 / 通知上层触发安全停机。

**收益与代价：**

- 收益：把最坏情况 send 阻塞从 1 秒级压缩到几毫秒级，更符合力控需求。
- 代价：在总线或 USB 真的很糟糕时，可能会频繁丢包甚至持续发不出去，但这是“可观测且可处理”的故障，而不是“完全假死”的故障。

---

### 4. 阶段 1：双线程 IO 架构（核心改造）

> 目标：根本上解除 send 对 receive 的连坐影响，让 RX 线程在任何 TX 故障下仍能持续更新状态。
> 这是从“盲目失控”到“可观测故障”的质变。

#### 4.1 目标架构概览

- **RX 线程（高优先级）**
  - 职责：`can.receive()` → 解析 → 更新 `PiperContext` 各种状态。
  - 从不执行任何发送逻辑。
  - 读路径可以有短超时（2ms），或者阻塞但支持外部打断（终止信号）。

- **TX 线程（中优先级）**
  - 职责：阻塞或带超时地从 `cmd_rx` 中取命令 → `can.send()`。
  - 允许在发送时长时间阻塞（由阶段 0 中的超时机制约束上界）。
  - 任何发送异常只影响 TX 线程自身，不影响 RX 的状态更新。

#### 4.2 设备句柄并发访问问题与方案

**难点：** 不同后端对“并发读写同一个底层 handle”的支持情况不同：

- SocketCAN 的 `CanSocket` 通常可以在多个线程中 clone 使用，每个线程独立 read/write（需要确认 crate 文档与实际行为）。
- GS-USB 的 `rusb::DeviceHandle` 通常不是 `Sync`，不应跨线程并发读写。

**建议方案：**

- 对支持并发句柄的后端（如 SocketCAN）：
  - 在 adapter 内部提供 `split()` 方法：
    - 返回 `(RxAdapter, TxAdapter)`，内部分别持有各自的句柄或同一个句柄的 clone。
    - `RxAdapter` 实现 `CanAdapterRx` trait（只读）。
    - `TxAdapter` 实现 `CanAdapterTx` trait（只写）。

- 对不支持并发句柄的后端（如 GS-USB）：
  - 在 adapter 内部实现一个“设备工作线程”（Device Worker）：
    - 单线程独占 `DeviceHandle`。
    - 对外暴露两个通道：
      - `rx_out: Receiver<PiperFrame>`：把从设备读到的帧推送出去。
      - `tx_in: Sender<PiperFrame>`：接收待发送命令。
    - Device Worker 内部循环：
      - 从 `tx_in` 非阻塞/带预算地取若干命令并写出。
      - 调用底层 `read` 或 `receive_batch` 获取帧并推入 `rx_out`。
  - `Piper` 层的 RX/TX 线程只与这些通道交互，而不直接操作 `DeviceHandle`。

这样可以保证：

- 底层设备访问始终在单线程完成，避免 libusb 线程安全风险。
- 上层逻辑依然获得“逻辑上的双线程 RX/TX 分离”，实现故障域隔离。

#### 4.3 Piper 层改造思路

**接口层保持不变：**

- 对使用者而言，`Piper::new(...)` 与 `PiperBuilder::build()` 的 API 不变。
- 仍然只暴露一个 `Piper` 对象，内部封装多线程与状态。

**内部结构调整：**

- 当前：
  - `Piper::new()` 只创建 `cmd_tx`，并 spawn 一个 IO 线程跑 `io_loop(can, cmd_rx, ctx, config)`。

- 目标：
  - `Piper::new()` 创建：
    - `cmd_tx` / `cmd_rx`：从上层到 TX 线程的命令队列。
    - （如需额外队列）`raw_rx`：从底层 Device Worker 到 RX 线程的原始帧通道。
  - 启动：
    - RX 线程：从 `raw_rx` 或 adapter 的 receive 接口获取帧，更新状态。
    - TX 线程：从 `cmd_rx` 获取命令并发送。

**过渡策略：**

- 在第一步，可以只针对 GS-USB 实现 Device Worker + 双线程，其他后端仍使用原先单线程模型。
- 等 GS-USB 路径稳定后，再统一抽象成一个通用的“双线程 IO 模板”，方便其他后端迁移。

#### 4.4 发送侧丢弃策略（QoS）

双线程后，TX 线程可能面对命令洪峰，需要设计丢弃策略：

- 建议：
  - 命令队列仍然使用有界队列（如长度 10–32）。
  - 当队列满时，新命令策略：
    - 丢弃最旧命令，保留最新（典型的“只发最新控制量”策略）。
    - 或者由上层逻辑根据错误返回决定如何重试/降级。

这样可以防止：

- 在命令产生速率高于发送带宽时，队列积压导致“延迟慢慢堆积到几百毫秒甚至秒级”的情况。

---

### 5. 阶段 2：trait 与测试体系的统一演进

> 目标：让“超时 / 非阻塞 / 双线程 IO”在接口层可见且可配置，并建立可回归的实时性测试体系。

#### 5.1 trait 扩展（API 方向）

在保持现有 `CanAdapter` 不立刻破坏的前提下，设计未来演进方向：

- 在新 trait 或同一 trait 上增加可选方法：
  - `fn set_receive_timeout(&mut self, timeout: Duration)`（已经在部分 adapter 中存在，可以纳入 trait）。
  - `fn receive_timeout(&mut self, timeout: Duration) -> Result<PiperFrame, CanError>`。
  - `fn try_receive(&mut self) -> Result<Option<PiperFrame>, CanError>`。
  - `fn send_timeout(&mut self, frame: PiperFrame, timeout: Duration) -> Result<(), CanError>`。

这可以让：

- `PipelineConfig` 从“注释型配置”变成真正的全链路超时配置入口。
- 上层可以针对不同模式（实时 / 非实时）选择不同的超时和退避策略。

#### 5.2 实时性与抖动测试

设计一套简单但可重复的测试/benchmark：

- 场景：
  - 模拟 500Hz/1kHz 的控制回路，周期性调用 `send_frame()`，并持续读取 `JointDynamicState` 与 `JointPositionState`。
  - 同时在下层制造各种干扰：
    - USB 故障模拟（例如守护进程故意 sleep / 丢包）。
    - CAN 总线高负载 / ACK 缺失模拟（在仿真或硬件测试平台上）。

- 度量：
  - RX 状态更新周期的 histogram（P50/P95/P99/max）。
  - 命令从 API 调用到实际 `can.send()` 的延迟分布。
  - `can.send()` 单次耗时分布，特别是最大值。

通过这些度量，可以客观验证：

- 阶段 0 的超时调整是否显著降低最坏情况延迟。
- 阶段 1 的双线程 IO 架构是否将 RX 的抖动从“受 TX 影响”变成“基本独立”。

---

### 6. 小结与推荐执行顺序

**短期（可以尽快在主分支推进）：**

- 将 `PipelineConfig.receive_timeout_ms` 真正落到各 adapter 的读超时配置上，统一到 2ms 级别的默认值。
- 调整 `io_loop` 内命令 drain 的顺序与次数，对 GS-USB 写路径在“实时模式”下降低 Bulk OUT 超时。

**中期（需要设计与重构，但收益巨大）：**

- 为 GS-USB 实现 Device Worker + 双线程 IO，完成收发的物理解耦。
- 总结模式后推广到 SocketCAN 与 GsUsbUdp，使整套 SDK 具备统一的“双线程 IO”骨架。

**长期（API 与生态层面）：**

- 在 trait 与 Builder/API 层暴露超时与实时性配置，形成一套自描述的“实时 profile”。
- 引入系统化的延迟与抖动基准测试，将 Piper SDK 的实时行为纳入 CI / 回归测试。

从实时控制和安全性角度看，**阶段 1 的双线程解耦是整个方案的“硬要求”**；阶段 0 是低风险止血，阶段 2 则让这套设计长期可维护、可验证。建议按照本报告的阶段顺序推进实现。



