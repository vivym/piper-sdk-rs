# 变更日志

本文档记录本项目的所有重要变更。

格式基于 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.0.0/)，
版本号遵循 [Semantic Versioning](https://semver.org/lang/zh-CN/)。

## [未发布]

### 新增
- 初始项目结构
- CAN 通讯适配层（GS-USB）
- 基础错误处理体系
- **客户端自动 ID 分配**（gs_usb_daemon）：
  - 守护进程支持自动分配客户端 ID（`client_id = 0` 表示自动分配）
  - 统一 UDS 和 UDP 客户端使用自动 ID 分配，解决 UDP 跨网络场景下的 ID 冲突问题
  - 向后兼容：仍支持手动指定 ID（`client_id != 0`）
  - 客户端从 `ConnectAck` 获取守护进程分配的 ID

### 变更

- **客户端 ID 分配策略统一**（gs_usb_daemon）：
  - **UDS/UDP 统一模式**：所有客户端（UDS 和 UDP）统一使用自动 ID 分配
  - **UDP 跨网络支持**：解决 UDP 跨网络场景下进程 ID 可能冲突的问题
  - **向后兼容**：保留手动指定 ID 支持，但推荐使用自动分配
  - **协议变更**：`Connect` 消息中 `client_id = 0` 表示请求自动分配，`ConnectAck` 返回实际使用的 ID
  - **客户端变更**：客户端统一发送 `client_id = 0`，从 `ConnectAck` 获取分配的 ID
- **Phase 0 改进：CAN IO 线程模型优化**
  - 统一 receive 超时配置：`PipelineConfig.receive_timeout_ms` 现在真正应用到所有后端适配器
    - GS-USB 默认超时从 50ms 改为 2ms（与 SocketCAN 一致）
    - SocketCAN 和 GS-USB 都通过 `PiperBuilder` 统一配置超时
  - 双重 Drain 策略：在 `io_loop` 的 `receive()` 前后都执行命令 drain，降低命令延迟
    - 引入时间预算机制（500µs），避免积压命令导致 RX 延迟突增
    - 限制单次 drain 最大帧数（32 帧）
  - GS-USB 实时模式（可选）：支持快速失败模式
    - 实时模式：写超时 5ms（适合力控场景）
    - 默认模式：写超时 1000ms（更可靠）
    - 连续超时计数和阈值警告（10 次）

- **Phase 1 改进：双线程架构（根治方案）**
  - **核心架构改进**：实现 RX/TX 线程物理隔离，彻底解决 Head-of-Line Blocking
    - 引入 `SplittableAdapter` trait，支持将适配器分离为独立的 RX 和 TX 适配器
    - GS-USB：利用 `rusb::DeviceHandle` 的 `Sync` 特性，使用 `Arc` 共享句柄实现真正的并行
    - SocketCAN：使用 `try_clone()` 复制文件描述符，实现 RX/TX 独立超时配置
    - 支持单线程模式（向后兼容）和双线程模式（高性能）
  - **GS-USB 双线程支持**：
    - `GsUsbDevice` 使用 `Arc<DeviceHandle>` 共享 USB 句柄
    - `GsUsbRxAdapter` 和 `GsUsbTxAdapter` 实现独立的接收和发送逻辑
    - `GsUsbRxAdapter` 自动过滤 Echo 帧（GS-USB 协议特性）
    - 预分配 `VecDeque` 容量，减少内存分配抖动
  - **SocketCAN 双线程支持**：
    - `SocketCanRxAdapter` 和 `SocketCanTxAdapter` 实现独立的接收和发送逻辑
    - 硬件过滤器配置（CAN ID 0x251-0x256），降低 CPU 占用
    - 发送超时配置（`SO_SNDTIMEO` = 5ms），防止总线错误时永久阻塞
    - 关键警告：`try_clone()` 共享文件状态标志，严禁使用 `set_nonblocking()`
  - **线程生命周期管理**：
    - 引入 `Arc<AtomicBool>` (`is_running`) 实现线程健康监控
    - RX/TX 线程能感知对方的故障并自动退出
    - `Piper::check_health()` 和 `Piper::is_healthy()` 方法用于监控线程状态
  - **命令优先级队列（⚠️ 已重构为邮箱模式）**：
    - 实时命令邮箱（Mailbox）：用于高频控制命令（500Hz-1kHz），支持真正的 Overwrite 策略
    - 可靠命令队列（容量 10，FIFO）：用于配置和状态查询命令
    - `send_realtime()` 使用邮箱模式实现真正的覆盖（Last Write Wins），延迟降低至 20-50ns
    - `send_reliable()` 和 `send_reliable_timeout()` 实现可靠传输
  - **⚡ 邮箱模式重构（2026-01-20）**：
    - **问题修复**：Channel 无法实现真正的 Overwrite，原有实现使用"sleep + 重试"伪装覆盖
    - **架构变更**：用 `Arc<Mutex<Option<PiperFrame>>>` 替换 `realtime_tx/rx` Channel
    - **性能提升**：发送延迟从 100-200μs 降至 20-50ns（**降低 2000-10000 倍**）
    - **语义增强**：真正的 Last Write Wins 覆盖策略，无阻塞、无重试
    - **向后兼容**：100% API 兼容，用户代码无需修改
    - 详见 `docs/v0/mailbox_pattern_implementation.md`
  - **性能指标（Metrics）**：
    - `PiperMetrics` 提供零开销的原子计数器
    - 监控指标：RX/TX 帧数、超时次数、错误次数、Overwrite 次数等
    - `MetricsSnapshot` 提供一次性读取所有指标的接口
    - 支持计算过滤率、有效帧率、覆盖率等衍生指标
  - **线程优先级支持**（可选 `realtime` feature）：
    - 使用 `thread-priority` crate（v3.0.0）设置 RX 线程为最高优先级
    - Linux 需要 `CAP_SYS_NICE` 或 `rtkit` 配置
    - 更新 README.md 说明权限配置方法
  - **帧解析逻辑重构**：
    - 从 `io_loop` 提取完整的帧解析逻辑到 `parse_and_update_state()` 函数
    - 支持所有帧类型：关节位置、末端位姿、关节动态、控制状态、诊断状态、配置状态等
    - 实现帧组同步逻辑和缓冲提交策略
  - **测试体系**：
    - 线程隔离测试：验证 RX 不受 TX 故障影响，TX 能感知 RX 故障
    - 性能测试：测量 RX 状态更新周期分布（P50/P95/P99/max）和 TX 命令延迟分布
    - Metrics 准确性测试：验证计数器与实际发送/接收帧数一致
    - 所有测试通过（7 个测试，424 个单元测试）

### 修复

### 移除

---

## [0.1.0] - 2026-01-XX

### 新增
- 初始版本发布
- GS-USB 协议支持（Windows/macOS）
- 基础 CAN 通讯接口
- 错误处理框架
- 文档和示例

[未发布]: https://github.com/YOUR_USERNAME/piper-sdk-rs/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/YOUR_USERNAME/piper-sdk-rs/releases/tag/v0.1.0

