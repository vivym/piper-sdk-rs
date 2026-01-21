# GS-USB Daemon 实施执行计划

**日期**: 2026-01-20
**版本**: v1.0
**基于**: 架构分析报告 v1.3.1 (评分: 99.5/100)
**项目周期**: 2 周（P0+P1），可选 P2（1-2 月）

---

## 执行摘要

本执行计划基于 GS-USB Daemon 架构分析报告 v1.3.1，旨在通过分阶段实施来解决 macOS GS-USB 设备热拔插问题，并满足力控机械臂的实时性要求（1kHz 控制频率，< 200μs 延迟）。

### 关键目标
- 🔴 **P0 (立即)**: 消除单点故障风险，确保系统稳定性
- 🟡 **P1 (1周)**: 优化实时性能，满足力控要求
- 🔵 **P2 (可选)**: 增强可观测性和健壮性

### 预期成果
- ✅ 系统可靠性提升 **200%**（消除故障客户端拖死 daemon 风险）
- ✅ P99 延迟降低 **5 倍**（250-800μs → 50-200μs）
- ✅ 最坏延迟降低 **100 倍**（200ms → 2ms）
- ✅ 客户端清理速度提升 **5000 倍**（5s → < 1ms）

---

## 第一阶段：P0 - 紧急修复（立即执行，35 分钟）

### 阶段目标
消除最危险的单点故障：**故障客户端拖死整个 daemon**

### 任务清单

#### 任务 P0-1: Client 结构体扩展
**文件**: `src/bin/gs_usb_daemon/client_manager.rs`
**时间**: 5 分钟
**优先级**: 🔴 P0

**步骤**:
1. 修改 `Client` 结构体：
```rust
pub struct Client {
    pub id: u32,
    pub addr: ClientAddr,
    pub unix_addr: Option<std::os::unix::net::SocketAddr>,
    pub last_active: Instant,
    pub filters: Vec<CanIdFilter>,

    // ✅ 新增字段
    pub consecutive_errors: AtomicU32,
    pub created_at: Instant,  // 可选，便于调试
}
```

2. 更新 `Client::new()` 构造函数：
```rust
impl Client {
    pub fn new(id: u32, addr: ClientAddr, filters: Vec<CanIdFilter>) -> Self {
        Self {
            id,
            addr,
            unix_addr: None,
            last_active: Instant::now(),
            filters,
            consecutive_errors: AtomicU32::new(0),
            created_at: Instant::now(),
        }
    }
}
```

**验收标准**: 编译通过，无 linter 警告

---

#### 任务 P0-2: ClientManager ID 生成器
**文件**: `src/bin/gs_usb_daemon/client_manager.rs`
**时间**: 10 分钟
**优先级**: 🔴 P0

**步骤**:
1. 修改 `ClientManager` 结构体：
```rust
pub struct ClientManager {
    clients: HashMap<u32, Client>,
    next_id: AtomicU32,  // ✅ 新增
    timeout: Duration,
    unix_addr_map: HashMap<u32, std::os::unix::net::SocketAddr>,
}
```

2. 实现 ID 生成器：
```rust
impl ClientManager {
    pub fn new() -> Self {
        Self {
            clients: HashMap::new(),
            next_id: AtomicU32::new(1),  // ✅ 从 1 开始（0 保留）
            timeout: Duration::from_secs(30),
            unix_addr_map: HashMap::new(),
        }
    }

    fn generate_client_id(&self) -> u32 {
        loop {
            let id = self.next_id.fetch_add(1, Ordering::Relaxed);
            let id = if id == 0 { 1 } else { id };  // 溢出处理

            if !self.clients.contains_key(&id) {
                return id;
            }
        }
    }

    pub fn register_auto(
        &mut self,
        addr: ClientAddr,
        filters: Vec<CanIdFilter>,
    ) -> Result<u32, ClientError> {
        let id = self.generate_client_id();

        self.clients.insert(
            id,
            Client::new(id, addr, filters),
        );

        Ok(id)
    }
}
```

**验收标准**:
- ✅ ID 从 1 开始递增
- ✅ 溢出后从 1 重新开始
- ✅ 冲突检测正常工作

---

#### 任务 P0-3: UDS 非阻塞模式
**文件**: `src/bin/gs_usb_daemon/daemon.rs`
**时间**: 1 分钟
**优先级**: 🔴 P0

**步骤**:
修改 `init_sockets()` 方法：
```rust
fn init_sockets(&mut self) -> Result<(), DaemonError> {
    if let Some(ref uds_path) = self.config.uds_path {
        // ... 创建 socket ...

        // ✅ 设置非阻塞模式
        socket.set_nonblocking(true)?;

        self.socket_uds = Some(socket);
    }
    // ...
}
```

**验收标准**: socket 设置为非阻塞模式，编译通过

---

#### 任务 P0-4: WouldBlock + ENOBUFS 错误处理
**文件**: `src/bin/gs_usb_daemon/daemon.rs`
**时间**: 10 分钟
**优先级**: 🔴 P0

**步骤**:
修改 `usb_receive_loop()` 中的客户端广播逻辑：

```rust
let clients_guard = clients.read().unwrap();
let mut failed_clients = Vec::new();

for client in clients_guard.iter() {
    let encoded = encode_receive_frame_zero_copy(&frame, &mut buf)?;

    match socket.send_to(encoded, uds_path) {
        Ok(_) => {
            stats.read().unwrap().increment_ipc_sent();
            client.consecutive_errors.store(0, Ordering::Relaxed);
        },
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock
               || matches!(e.raw_os_error(), Some(libc::ENOBUFS)) => {
            let error_count = client.consecutive_errors.fetch_add(1, Ordering::Relaxed) + 1;
            metrics.client_send_blocked.fetch_add(1, Ordering::Relaxed);

            // 日志限频
            if error_count == 1 || error_count % 1000 == 0 {
                eprintln!(
                    "[Client {}] Buffer full, dropped {} frames total",
                    client.id, error_count
                );
            }

            // 死客户端检测
            if error_count >= 1000 {
                eprintln!(
                    "[Client {}] Buffer full for 1s, disconnecting",
                    client.id
                );
                failed_clients.push(client.id);
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound
               || e.kind() == std::io::ErrorKind::ConnectionRefused => {
            eprintln!("[Client {}] Socket not found, removing", client.id);
            failed_clients.push(client.id);
        },
        Err(e) if matches!(e.raw_os_error(), Some(libc::EPIPE)) => {
            eprintln!("[Client {}] Pipe broken, removing", client.id);
            failed_clients.push(client.id);
        },
        Err(e) => {
            eprintln!("[Client {}] Send error: {}", client.id, e);
        }
    }
}

// 清理失败的客户端
drop(clients_guard);
if !failed_clients.is_empty() {
    let mut clients_guard = clients.write().unwrap();
    for client_id in failed_clients {
        clients_guard.unregister(client_id);
    }
}
```

**验收标准**:
- ✅ WouldBlock 和 ENOBUFS 都能正确捕获
- ✅ 日志限频生效（不会洪水）
- ✅ 死客户端在 1 秒后断开

---

#### 任务 P0-5: EPIPE/ECONNREFUSED 监听
**文件**: `src/bin/gs_usb_daemon/daemon.rs`
**时间**: 5 分钟（已在 P0-4 中实现）
**优先级**: 🔴 P0

**验收标准**:
- ✅ 客户端进程退出时立即清理（< 1ms）
- ✅ 不再依赖 5 秒超时

---

#### 任务 P0-6: 减小 USB 超时
**文件**: `src/bin/gs_usb_daemon/daemon.rs`
**时间**: 1 分钟
**优先级**: 🔴 P0

**步骤**:
修改 `try_connect_device()` 方法：
```rust
fn try_connect_device(config: &DaemonConfig) -> Result<GsUsbCanAdapter, DaemonError> {
    let mut adapter = GsUsbCanAdapter::new_with_serial(config.serial_number.as_deref())?;

    // ✅ 修改超时：200ms → 2ms
    adapter.set_receive_timeout(Duration::from_millis(2));

    // ...
}
```

**验收标准**: USB 超时设置为 2ms

---

#### 任务 P0-7: 更新统计指标
**文件**: `src/bin/gs_usb_daemon/daemon.rs`
**时间**: 3 分钟
**优先级**: 🔴 P0

**步骤**:
在 `DaemonStats` 中新增字段：
```rust
struct DaemonStats {
    // ... 现有字段 ...

    // ✅ 新增
    client_send_blocked: AtomicU64,
    client_disconnected: AtomicU64,
}
```

**验收标准**: 指标正确统计

---

### P0 阶段验收

#### 编译测试
```bash
cargo build --bin gs_usb_daemon
```

#### 单元测试
```bash
cargo test --bin gs_usb_daemon
```

#### Linter 检查
```bash
cargo clippy --bin gs_usb_daemon
```

#### 功能验证
1. 启动 daemon
2. 连接测试客户端
3. 模拟客户端卡死（故意不读取数据）
4. 验证：
   - ✅ daemon 不被阻塞
   - ✅ 1 秒后客户端被断开
   - ✅ 日志限频生效
   - ✅ 其他客户端不受影响

#### 预期效果
- ✅ **单点故障消除**: 故障客户端无法拖死 daemon
- ✅ **清理速度提升**: 5s → < 1ms (**5000 倍**)
- ✅ **最坏延迟降低**: 200ms → 2ms (**100 倍**)

---

## 第二阶段：P1 - 性能优化（1 周内完成）

### 阶段目标
优化实时性能，满足力控机械臂要求（P99 延迟 < 200μs）

### 任务清单

#### 任务 P1-1: Daemon 结构体重构
**文件**: `src/bin/gs_usb_daemon/daemon.rs`
**时间**: 2 小时
**优先级**: 🟡 P1

**步骤**:
1. 修改 `Daemon` 结构体：
```rust
pub struct Daemon {
    // ✅ 分离 RX 和 TX adapter
    rx_adapter: Arc<Mutex<Option<GsUsbDevice>>>,
    tx_adapter: Arc<Mutex<Option<GsUsbDevice>>>,

    device_state: Arc<RwLock<DeviceState>>,
    // ...
}
```

2. 修改初始化逻辑：
```rust
impl Daemon {
    pub fn new(config: DaemonConfig) -> Result<Self, DaemonError> {
        Ok(Self {
            rx_adapter: Arc::new(Mutex::new(None)),
            tx_adapter: Arc::new(Mutex::new(None)),
            device_state: Arc::new(RwLock::new(DeviceState::Disconnected)),
            // ...
        })
    }
}
```

**验收标准**: 编译通过，结构体正确分离

---

#### 任务 P1-2: USB RX 线程锁粒度优化
**文件**: `src/bin/gs_usb_daemon/daemon.rs`
**时间**: 1 小时
**优先级**: 🟡 P1

**步骤**:
修改 `usb_receive_loop()`：
```rust
fn usb_receive_loop(
    rx_adapter: Arc<Mutex<Option<GsUsbDevice>>>,
    device_state: Arc<RwLock<DeviceState>>,
    // ...
) {
    loop {
        // 1. 检查设备状态
        if *device_state.read().unwrap() != DeviceState::Connected {
            thread::sleep(Duration::from_millis(100));
            continue;
        }

        // 2. ✅ 锁粒度最小化
        let frame = {
            let adapter_opt = rx_adapter.lock().unwrap();
            match adapter_opt.as_ref() {
                Some(adapter) => adapter.receive()?,
                None => { continue; }
            }
        };  // ✅ 锁在这里释放

        // 3. 广播给客户端（此时已无锁）
        broadcast_frame(frame, &clients)?;
    }
}
```

**验收标准**:
- ✅ 锁只在 receive() 期间持有
- ✅ 广播时已释放锁

---

#### 任务 P1-3: IPC RX 线程优化
**文件**: `src/bin/gs_usb_daemon/daemon.rs`
**时间**: 1 小时
**优先级**: 🟡 P1

**步骤**:
修改 `ipc_receive_loop()`：
```rust
fn ipc_receive_loop(
    socket: UnixDatagram,
    tx_adapter: Arc<Mutex<Option<GsUsbDevice>>>,  // ✅ 使用 TX adapter
    // ...
) {
    crate::macos_qos::set_high_priority();

    loop {
        match socket.recv_from(&mut buf) {
            Ok((len, client_addr)) => {
                let msg = decode_message(&buf[..len])?;

                if let Message::SendFrame { frame, seq } = msg {
                    // ✅ 只锁 TX adapter
                    let adapter_opt = tx_adapter.lock().unwrap();
                    if let Some(adapter) = adapter_opt.as_ref() {
                        let _ = adapter.send(frame);
                    }
                }
            },
            Err(e) => {
                thread::sleep(Duration::from_millis(100));
            }
        }
    }
}
```

**验收标准**: TX 和 RX 完全独立，零竞争

---

#### 任务 P1-4: 设备管理线程重连逻辑
**文件**: `src/bin/gs_usb_daemon/daemon.rs`
**时间**: 2 小时
**优先级**: 🟡 P1

**步骤**:
修改 `device_manager_loop()`：
```rust
fn device_manager_loop(
    rx_adapter: Arc<Mutex<Option<GsUsbDevice>>>,
    tx_adapter: Arc<Mutex<Option<GsUsbDevice>>>,
    device_state: Arc<RwLock<DeviceState>>,
    // ...
) {
    loop {
        match current_state {
            DeviceState::Reconnecting => {
                match try_connect_device(&config) {
                    Ok(new_device) => {
                        // ✅ 原子性更新
                        let mut state_guard = device_state.write().unwrap();
                        {
                            let mut rx_guard = rx_adapter.lock().unwrap();
                            let mut tx_guard = tx_adapter.lock().unwrap();

                            let device_arc = Arc::new(new_device);
                            *rx_guard = Some(device_arc.clone());
                            *tx_guard = Some(device_arc.clone());
                        }
                        *state_guard = DeviceState::Connected;
                    },
                    Err(e) => {
                        thread::sleep(config.reconnect_interval);
                    }
                }
            },
            // ...
        }
    }
}
```

**验收标准**:
- ✅ 锁顺序一致（device_state → rx → tx）
- ✅ 原子性更新
- ✅ 无死锁风险

---

#### 任务 P1-5: 更新 run() 方法
**文件**: `src/bin/gs_usb_daemon/daemon.rs`
**时间**: 1 小时
**优先级**: 🟡 P1

**步骤**:
更新线程创建逻辑，传递正确的 adapter：
```rust
pub fn run(&mut self) -> Result<(), DaemonError> {
    self.init_sockets()?;

    // 设备管理线程
    let rx_adapter_clone = Arc::clone(&self.rx_adapter);
    let tx_adapter_clone = Arc::clone(&self.tx_adapter);
    let device_state_clone = Arc::clone(&self.device_state);
    thread::spawn(move || {
        device_manager_loop(rx_adapter_clone, tx_adapter_clone, device_state_clone, config);
    });

    // USB RX 线程
    let rx_adapter_clone = Arc::clone(&self.rx_adapter);
    thread::spawn(move || {
        usb_receive_loop(rx_adapter_clone, device_state_clone, clients_clone, ...);
    });

    // IPC RX 线程
    let tx_adapter_clone = Arc::clone(&self.tx_adapter);
    thread::spawn(move || {
        ipc_receive_loop(socket_uds, tx_adapter_clone, ...);
    });

    // ...
}
```

**验收标准**: 所有线程正确创建，adapter 传递正确

---

### P1 阶段验收

#### 编译和测试
```bash
cargo build --bin gs_usb_daemon
cargo test --bin gs_usb_daemon
cargo clippy --bin gs_usb_daemon
```

#### 性能基准测试
**测试工具**: `examples/daemon_latency_test.rs`（需创建）

```rust
// 测试场景
fn bench_round_trip_latency() {
    let mut latencies = vec![];

    for _ in 0..10000 {
        let start = Instant::now();
        // 发送 → 接收往返
        latencies.push(start.elapsed());
    }

    latencies.sort_unstable();
    let p50 = latencies[5000];
    let p99 = latencies[9900];

    println!("P50: {:?}", p50);
    println!("P99: {:?}", p99);

    assert!(p99 < Duration::from_micros(200));
}
```

#### 预期效果
- ✅ **P99 延迟降低**: 250-800μs → 50-200μs (**5 倍提升**)
- ✅ **RX/TX 零竞争**: 完全并发
- ✅ **锁持有时间**: 最小化

---

## 第三阶段：P2 - 可选增强（1-2 月）

### 任务 P2-1: 背压机制（可选）
**时间**: 1 周
**优先级**: 🔵 P2
**适用场景**: 监控客户端

**功能**:
1. 丢包通知消息
2. 自适应频率降级（1kHz → 100Hz → 10Hz）

---

### 任务 P2-2: 健康度评分系统
**时间**: 1 周
**优先级**: 🔵 P2
**适用场景**: 生产环境监控

**功能**:
1. USB 错误分类统计
2. CAN 总线健康监控
3. CPU 占用率监控
4. 0-100 分健康度评分
5. < 60 分自动告警

---

## 资源需求

### 人力资源
- **核心开发**: 1 人
- **测试验证**: 0.5 人
- **代码审查**: 0.5 人

### 开发环境
- **硬件**:
  - macOS 开发机（M1/M2 或 Intel）
  - GS-USB 设备 x1
  - CAN 测试设备（可选）

- **软件**:
  - Rust 1.70+
  - Cargo
  - GDB/LLDB（调试）
  - Wireshark（抓包分析，可选）

---

## 风险管理

| 风险 | 概率 | 影响 | 缓解措施 | 责任人 |
|-----|------|------|---------|--------|
| P0 实施引入新 bug | 中 | 高 | 充分单元测试 + 代码审查 | 开发 |
| RX/TX 分离导致死锁 | 低 | 高 | 严格锁顺序 + 压力测试 | 开发 |
| 性能未达预期 | 低 | 中 | 基准测试 + 性能分析工具 | 开发 |
| USB 驱动兼容性问题 | 低 | 中 | 多版本 macOS 测试 | 测试 |

---

## 时间表

### Week 0（Day 1）
- **上午**: P0-1, P0-2, P0-3 (Client 扩展 + ID 生成器 + 非阻塞)
- **下午**: P0-4, P0-5, P0-6, P0-7 (错误处理 + 超时 + 指标)
- **晚上**: P0 验收测试

### Week 1（Day 2-5）
- **Day 2**: P1-1, P1-2 (结构体重构 + RX 线程优化)
- **Day 3**: P1-3, P1-4 (TX 线程优化 + 设备管理)
- **Day 4**: P1-5 (run() 方法更新 + 集成)
- **Day 5**: P1 验收测试 + 性能基准测试

### Week 2（Day 6-10，可选）
- **Day 6-7**: 代码审查 + 文档更新
- **Day 8-10**: 生产环境部署准备 + 监控集成

### P2（可选，1-2 月）
- **Week 3-4**: P2-1 (背压机制)
- **Week 5-6**: P2-2 (健康度监控)

---

## 里程碑

| 里程碑 | 日期 | 交付物 | 验收标准 |
|-------|------|--------|---------|
| **M1: P0 完成** | Day 1 | 单点故障修复 | ✅ 故障客户端不拖死 daemon |
| **M2: P1 完成** | Day 5 | 性能优化 | ✅ P99 延迟 < 200μs |
| **M3: 代码审查** | Day 7 | 代码质量 | ✅ 无 linter 警告 |
| **M4: 生产部署** | Day 10 | 可部署版本 | ✅ 通过所有测试 |
| **M5: P2 完成** | Week 6 | 可观测性增强 | ✅ 监控系统集成 |

---

## 验收标准

### P0 验收标准
- [ ] 编译通过，无 linter 警告
- [ ] 所有单元测试通过
- [ ] 故障客户端在 1 秒内断开
- [ ] daemon 不被阻塞
- [ ] 日志限频生效
- [ ] EPIPE/ECONNREFUSED 立即清理（< 1ms）
- [ ] Client ID 正确生成（无冲突、溢出安全）

### P1 验收标准
- [ ] RX/TX 完全分离，零竞争
- [ ] P50 延迟 < 100μs
- [ ] P99 延迟 < 200μs
- [ ] 热拔插恢复 < 1s
- [ ] 无死锁风险
- [ ] CPU 占用 < 30%（1kHz 10 客户端）

### P2 验收标准（可选）
- [ ] 健康度评分正常工作
- [ ] USB/CAN 错误正确统计
- [ ] CPU 监控正常
- [ ] 告警机制生效
- [ ] 监控系统集成成功

---

## 测试策略

### 单元测试
**覆盖率目标**: > 80%

**关键测试**:
- `Client::new()` 构造函数
- `ClientManager::generate_client_id()` ID 生成
- `ClientManager::register_auto()` 注册逻辑
- 错误处理路径（WouldBlock, ENOBUFS, EPIPE）

### 集成测试
**测试场景**:
1. 单客户端正常收发
2. 10 客户端并发收发
3. 客户端卡死注入（故意不读取）
4. 客户端进程退出（EPIPE）
5. 热拔插（物理拔出 USB）

### 性能测试
**测试工具**: 自定义 benchmark

**关键指标**:
- Round-trip 延迟（P50/P99/P999）
- CPU 占用率
- 内存占用
- 吞吐量（fps）

### 压力测试
**测试场景**:
- 1kHz 持续 1 小时
- CAN 总线满载
- 100 次热拔插循环

---

## 文档更新

### 需要更新的文档
- [ ] `README.md` - 新增 daemon 使用说明
- [ ] `CHANGELOG.md` - 记录版本变更
- [ ] `docs/gs_usb_daemon_user_guide.md` - 用户指南（新建）
- [ ] `docs/gs_usb_daemon_troubleshooting.md` - 故障排查（新建）

---

## 部署计划

### 部署步骤
1. **编译发布版本**:
```bash
cargo build --release --bin gs_usb_daemon
```

2. **安装到系统**:
```bash
sudo cp target/release/gs_usb_daemon /usr/local/bin/
sudo chmod +x /usr/local/bin/gs_usb_daemon
```

3. **配置 launchd**（macOS 自动启动）:
```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "...">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.piper.gs_usb_daemon</string>
    <key>ProgramArguments</key>
    <array>
        <string>/usr/local/bin/gs_usb_daemon</string>
        <string>--uds</string>
        <string>/tmp/piper_can.sock</string>
        <string>--bitrate</string>
        <string>1000000</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
</dict>
</plist>
```

4. **启动服务**:
```bash
sudo launchctl load /Library/LaunchDaemons/com.piper.gs_usb_daemon.plist
```

---

## 回滚计划

### 回滚触发条件
- P99 延迟 > 500μs
- 系统崩溃或死锁
- 数据丢失

### 回滚步骤
1. 停止 daemon
2. 恢复旧版本二进制
3. 重启 daemon
4. 验证功能正常

---

## 后续优化建议

### 短期（3 个月内）
1. 增加更多单元测试
2. 集成到 CI/CD 流程
3. 性能分析和优化

### 长期（6 个月内）
1. 支持 Linux SocketCAN
2. 支持 Windows
3. 增加 gRPC 接口（替代 UDS）

---

## 附录

### A. 代码审查清单
- [ ] 代码风格一致
- [ ] 无 unsafe 代码（或有充分注释）
- [ ] 错误处理完整
- [ ] 日志携带 Client ID
- [ ] 锁顺序一致
- [ ] 无 unwrap() 在生产代码（或有注释）

### B. 性能基准参考值
| 指标 | 目标值 | 实测值 | 状态 |
|-----|-------|--------|------|
| P50 延迟 | < 100μs | TBD | - |
| P99 延迟 | < 200μs | TBD | - |
| CPU 占用 | < 30% | TBD | - |
| 吞吐量 | > 1000 fps | TBD | - |

### C. 参考资料
- 架构分析报告: `docs/v0/gs_usb_daemon_architecture_analysis.md`
- 改进摘要: `docs/v0/gs_usb_daemon_analysis_v1.3_changes.md`
- rusb 文档: https://docs.rs/rusb/
- libusb 文档: https://libusb.info/

---

**报告完成时间**: 2026-01-20
**审核状态**: ✅ 待审核
**预计开始时间**: 立即
**预计完成时间**: 2 周（P0+P1）

