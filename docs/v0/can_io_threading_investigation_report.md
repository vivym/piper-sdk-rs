# CAN 收发线程模型与阻塞行为对力控场景的影响——调查报告

日期：2026-01-20
仓库：`piper-sdk-rs`
范围：`src/robot/pipeline.rs` + `src/can/*`（SocketCAN / GS-USB / GS-USB Daemon）

## 结论摘要（TL;DR）

- **当前 SDK 的 CAN“接收、解析/状态更新、发送命令”是在同一个 IO 线程里完成的**：`Piper::new()` 启动一个线程执行 `io_loop(...)`，该 loop 内部先 `receive()` 收帧，再在循环末尾把 `cmd_rx` 中的命令帧 `send()` 出去。
- **`CanAdapter::receive()` 在 trait 语义上是“阻塞直到收到或超时”**，但各后端并非“无限阻塞”：
  - **SocketCAN**：默认设置 **2ms 读超时**，使得 `receive()` 周期性返回 `Timeout`。
  - **GS-USB**：默认 `rx_timeout = 50ms`（USB Bulk IN 的超时），也会周期性返回 `Timeout`。
  - **GS-USB UDP（daemon）**：基于 socket 超时/轮询，亦会返回 `Timeout`。
- **对力控/高频闭环更危险的不是 receive 的“阻塞语义”，而是 send 的潜在长阻塞**：
  - GS-USB 的 `send_raw()` 使用 **1000ms 的 USB Bulk OUT 超时**。一旦设备忙、NAK、端点 STALL 或系统调度导致写入等待，`can.send()` 可能在 IO 线程里卡住很久。
  - 因为收发在同一线程：**发送侧卡住会直接阻塞接收、解析、状态更新**，从而造成力控闭环的输入状态“长时间不更新”、命令时序抖动、甚至控制不稳定。
- **是否需要“non-blocking receive 接口”？**：
  - 从工程效果看：当前后端已经通过“短超时的阻塞读取”实现了近似“可轮询”的行为；单纯把 receive 改成 non-blocking 并不能解决“发送侧长阻塞”这一更关键风险。
  - 更推荐的演进方向是：**把发送从 IO 线程的关键路径隔离**（独立发送线程/队列，或确保 send 在任何后端都具备严格上界的超时），并把 `PipelineConfig.receive_timeout_ms` 真正下沉到 adapter 的 receive 超时配置上，形成一致、可控的时序模型。

## 1. 现状梳理：线程模型与调用链

### 1.1 IO 线程模型

- `Piper::new()` 创建 `cmd_tx/cmd_rx`（容量 10 的有界队列），并 spawn 一个 IO 线程运行：
  - `io_loop(can, cmd_rx, ctx, config)`

### 1.2 `io_loop` 的执行顺序（关键）

`io_loop` 主循环结构可以概括为：

1) `can.receive()`（阻塞直到“收到帧”或“超时/错误”返回）
2) 若收到帧：按 CAN ID 解析，更新共享状态（ArcSwap / RwLock / Atomics）
3) 循环末尾：`while let Ok(cmd_frame) = cmd_rx.try_recv() { can.send(cmd_frame) }`

也就是说：

- **命令发送不是独立线程**；命令发送由 IO 线程在“每次 receive 返回后”批量 drain `cmd_rx` 执行。
- 如果 `receive()` 长时间不返回，则 **命令发送也无法执行**（虽然 `cmd_tx` 端可写入队列）。
- 如果 `send()` 卡住，则 **后续的 receive/解析/状态更新也都会卡住**。

### 1.3 上层命令 API 的语义

上层提供两种发送方式：

- `send_frame()`：`try_send`，**不阻塞**，队列满则报 `ChannelFull`。
- `send_frame_blocking(timeout)`：`send_timeout`，队列满会等待到超时。

注意：这只是“把帧送进 IO 线程命令队列”，并不代表实际 CAN 总线发送已经完成。

## 2. `receive()` 是不是“阻塞到不利于力控”？

### 2.1 trait 语义：阻塞 + 超时

`CanAdapter::receive()` 的 trait 文档明确写的是：

- “阻塞读取：直到收到有效数据帧或超时”

这意味着设计意图是：**IO loop 依赖 receive 的超时返回来避免永久阻塞**，从而有机会做其他工作（比如检查命令队列、检查退出信号、做超时清理等）。

### 2.2 各后端实际行为：并非无限阻塞

#### SocketCAN（Linux）

- 初始化时设置 `socket.set_read_timeout(Duration::from_millis(2))`
- 这使得 `receive()` 以 **2ms 为粒度**返回 `Timeout`，IO loop 可以以 2ms 频率进入“超时分支”并继续循环。

#### GS-USB（macOS/Windows）

- `GsUsbCanAdapter` 默认 `rx_timeout = 50ms`（注释说明避免 macOS 上频繁超时导致热循环）。
- `receive()` 内部调用 `device.receive_batch(rx_timeout)`，超时返回 `CanError::Timeout`。

> 对力控而言，50ms 这个粒度非常大：如果总线上“没有反馈帧”，IO 线程就可能 50ms 才醒一次，这会显著影响“命令发送的及时性”（因为命令 drain 在 receive 之后），也会影响某些“基于超时的内部逻辑刷新”。

#### GS-USB UDP（daemon）

- `receive()` 先检查内部 `rx_buffer`，为空则批量 `recv_from_daemon` 填充（并有最多 100 次循环的上限）。
- 读不到数据会返回 `Timeout`。

### 2.3 对力控闭环的真实影响：关键在“上界”与“抖动”

力控/阻抗控制/高带宽伺服通常关注：

- **状态更新延迟**（传感/反馈到控制的延迟）
- **控制命令输出延迟**（控制输出到执行器）
- **抖动（jitter）**：延迟是否稳定，还是偶发尖峰

在当前模型里，`receive()` 的“阻塞”本身并不可怕——只要它具有**足够小、可控的超时上界**，IO 线程就能以固定节奏醒来处理命令与 housekeeping。

因此：

- 若后端 `receive()` 的超时是 1~2ms，IO 线程的“调度粒度”相对适合高频控制；
- 若后端 `receive()` 的超时是 50ms，则即使控制线程 1kHz 产生命令，命令也可能在 IO 线程里 **最多积压 50ms 才开始发送**（更糟的是队列只有 10，1kHz 下 10ms 就满了）。

结论：**是否“不利于力控”取决于 receive 超时设置是否足够小，以及 IO loop 是否把命令发送放在 receive 之后这一点。**

## 3. 更关键的风险：`send()` 的长阻塞会“拖死整条链路”

### 3.1 GS-USB 的 send 超时设置非常大

GS-USB `send_raw()` 的实现显式把 USB Bulk OUT 超时设为 **1000ms**（并在 Timeout 后尝试 clear_halt，再 sleep 50ms）。

这在“设备忙/loopback/USB 端点偶发状态异常”时很实用（提高成功率），但对“实时控制链路”意味着：

- **一次 send() 的最坏阻塞时间可达 ~1s**（外加恢复 sleep 等），远超力控允许的任何抖动预算。
- 因为 `io_loop` 在同一线程内执行 send：**一旦 send 卡住，receive 也停止，状态更新也停止，命令队列也无法继续 drain**。

> 这会形成“正反馈式恶化”：状态不更新 → 控制层可能发送更多纠正命令 → 命令队列更快满 → 进一步加剧拥塞与丢弃。

### 3.2 SocketCAN 的 send 通常很快，但仍建议显式上界

Linux SocketCAN 的 `transmit` 一般不会出现秒级阻塞，但在系统高负载、socket 缓冲满等情况下也可能产生可见延迟。对力控来说，最好同样做到：

- **发送不影响接收**（解耦），或
- **发送具备严格的超时上界**（例如几毫秒级）。

## 4. 是否应该改为 non-blocking receive？

### 4.1 直接回答

- **“把 receive 改成 non-blocking”不是首要矛盾**。
- 当前设计已经依赖“短超时阻塞”来实现类似轮询；真正的大风险是 **发送侧可能长阻塞**，以及 **GS-USB 默认 receive 超时过大（50ms）导致命令发送被动延迟**。

### 4.2 什么时候 non-blocking receive 有价值？

当你希望 IO 线程实现更精细的调度（例如固定 1kHz tick）时，non-blocking / poll-based 方式会更自然：

- 以固定周期：
  - 尽可能多 drain RX（直到没有数据）
  - 尽可能多 drain TX（直到达到本周期预算）
  - sleep/park 到下一 tick

但即使如此，**如果 send 仍可能长阻塞**，non-blocking receive 依然救不了“单线程收发”的最坏情况。

## 5. 设计建议（按优先级）

### 5.1 优先级 A：把发送从 IO 线程关键路径隔离

推荐方案（从简单到复杂）：

- **A1. 独立 TX 线程**：
  - IO RX 线程只做 receive+解析+状态更新。
  - TX 线程阻塞式 `recv()` 命令队列并 `can.send()`（允许其偶发长阻塞而不影响 RX）。
  - 风险/代价：需要考虑设备/后端是否允许“并发读写同一 handle”（对 GS-USB `rusb::DeviceHandle` 未必线程安全，需要在 adapter 内部用锁或把底层拆成“读写分离句柄/专用线程 + 单线程拥有 handle”）。

- **A2. Adapter 内部实现“单线程拥有设备句柄 + 收发队列”**（推荐长期演进）：
  - 外部仍是 `send()/receive()` API，但 adapter 内部有专用线程拥有硬件资源，提供非阻塞/有界的队列接口。
  - 这样可以在 adapter 内部控制 send 的超时、丢弃策略、以及 USB 错误恢复，而不会把秒级阻塞泄露到 pipeline。

- **A3. 在 pipeline 内限制 send 的时间预算**：
  - 每次循环最多发送 N 帧或最多耗时 X 微秒，超出则留到下一轮。
  - 这依然无法避免单次 `send()` 自身阻塞很久的问题，因此必须配合“send 可中断/有严格超时”才能有效。

### 5.2 优先级 B：把 `PipelineConfig.receive_timeout_ms` 真正落地

当前 `PipelineConfig.receive_timeout_ms` 在 `io_loop` 中被注释为“当前未使用”，但后端其实都有“读超时”概念：

- SocketCAN：`set_read_timeout(Duration)`
- GS-USB：`set_receive_timeout(Duration)`（影响 USB bulk in timeout）
- GsUsbUdp：socket read timeout / 非阻塞轮询

建议：

- 在 `PiperBuilder::build()` 或 `Piper::new()` 初始化 can adapter 后，**统一设置 adapter 的 receive 超时为 `PipelineConfig.receive_timeout_ms`**（例如力控场景 1~2ms，非实时场景可更大）。

并给出明确推荐：

- **力控/高频控制**：receive 超时 1~2ms（让命令发送和退出/housekeeping 粒度足够细）
- **普通状态监控/低频控制**：10~50ms（更省 CPU）

### 5.3 优先级 C：重新排序 loop 或双向轮询

在不改架构的前提下，最小改动也应该考虑：

- 在每次 `receive()` 前先 `try_recv` drain 一次命令队列（至少发一轮），避免“无数据时命令卡在队列里直到 receive 超时”。

但注意：这会增加“发送阻塞影响接收”的概率，因此仍建议配合 5.1 的解耦。

### 5.4 优先级 D：扩展 trait，显式表达时序需求（可选）

当前 trait 只有：

- `send(frame)`
- `receive() -> Result<frame, Timeout>`

这会带来两个问题：

- **超时/非阻塞能力不统一**：SocketCAN/GS-USB/UDP 各自有自己的 “set_*_timeout” 方法，但 trait 层没有表达，导致 `PipelineConfig.receive_timeout_ms` 很难“一次配置到所有后端”。
- **缺少“带预算的发送”语义**：实时场景需要 `send` 可中断/可限制最坏耗时，否则单线程模型下会放大抖动。

建议的 trait 演进（示意）：

- `fn receive_timeout(&mut self, timeout: Duration) -> Result<PiperFrame, CanError>`
  - 让上层（pipeline）显式控制超时，而不是由后端隐藏配置。
  - `receive()` 可以保留为默认超时（或调用 `receive_timeout`）。
- `fn try_receive(&mut self) -> Result<Option<PiperFrame>, CanError>`
  - 统一“非阻塞读取”语义（无数据返回 `Ok(None)`）。
- `fn send_timeout(&mut self, frame: PiperFrame, timeout: Duration) -> Result<(), CanError>`
  - 明确给出 `send` 的最坏等待上界；对 GS-USB 可把 Bulk OUT 超时从 1000ms 下沉为可配置。

> 备注：是否要扩展 trait 取决于 API 稳定性诉求；如果暂时不想破坏 API，也可以先在 `robot` 层做“后端类型分支设置超时”，但长期维护成本会更高。

## 6. 建议的验证/度量方法（用于把结论量化）

### 6.1 必要指标

- **RX 状态更新周期**：`JointDynamicState`/`JointPositionState` 的实际更新频率与抖动（已有 `fps_stats` 可用）。
- **命令端到端延迟**：
  - 控制线程调用 `send_frame()` 的时间点
  - IO 线程真正调用 `can.send()` 的时间点
  - （如果协议支持）硬件端回传 ACK/回显的时间点
- **send 阻塞尖峰**：统计 `can.send()` 单次耗时分布（P50/P95/P99/max）。

### 6.2 最小侵入的测量手段

- 在 `io_loop` 内对以下路径加 `tracing` 计时（采样/开关控制）：
  - `receive()` 耗时
  - `send()` 单次耗时 + 本轮 drain 发送了多少帧
  - `cmd_rx` 队列积压情况（例如 drain 前 `len` 不可直接取，但可用计数器/失败率侧写）
- 在控制线程侧（调用 `send_frame` 的 API）记录发送失败率（`ChannelFull`）与重试次数。

## 7. 推荐落地路线（分阶段）

- **阶段 0（立即可做，低风险）**
  - 把 **GS-USB 的 `rx_timeout`** 在力控场景降到 **1~2ms**（并明确写入文档/示例）。
  - 把 `PipelineConfig.receive_timeout_ms` 真正用于配置后端 receive 超时（至少在 Builder/初始化处对不同后端分别调用 set_*_timeout）。
  - 在 pipeline 中考虑“每轮先 drain 一次命令再 receive”（或在超时分支也 drain），缓解“无反馈时命令延迟”。

- **阶段 1（中等改动，收益最大）**
  - 解决 **GS-USB 发送侧 1000ms 阻塞**：
    - 方案 1：发送线程/发送队列解耦（推荐）
    - 方案 2：把 Bulk OUT 超时做成可配置，并在实时模式下设置为几毫秒（同时定义失败策略：丢弃/重试/降级）

- **阶段 2（长期演进）**
  - 抽象出“后端统一的超时/非阻塞 API”（trait 演进），并建立一套可重复的延迟与抖动基准测试。

## 8. 回答提问（对齐你的原始问题）

- **“目前 sdk 对 can 帧的读取和发送是不是在一个线程里？”**
  - 是的，`io_loop` 在单线程里执行 `receive()` + 解析更新 + `cmd_rx.try_recv()` + `send()`。

- **“目前是 blocking 的 receive 的接口，是不是不利于力控？”**
  - `receive()` 的阻塞并不是根本问题，关键在于它是否有足够小的超时上界（SocketCAN 默认 2ms 是合理的；GS-USB 默认 50ms 对力控偏大）。

- **“是不是应该换成 non-blocking 的接口？”**
  - 仅把 receive 改成 non-blocking 不能解决最大风险；更应该优先解决：
    - GS-USB 发送侧可能出现的 **1000ms 级阻塞**（会拖死整条链路）
    - 以及把 receive 超时统一收敛到 `PipelineConfig.receive_timeout_ms`，让 IO 线程调度粒度可控。


