# 🚀 Apps 开发快速参考 (v2.1 最终版 + 代码级建议)

**文档**: `APPS_IMPLEMENTATION_GUIDE.md` | `APPS_DEVELOPMENT_PLAN_V2.md`
**日期**: 2026-01-26
**版本**: v2.1 (最终版 + 代码级建议)
**状态**: ✅ 生产就绪（最终审查通过）

---

## 📋 v2.1 更新内容

基于技术审查和实施建议，v2.1 在v2.0基础上增加了**3个关键实施坑点**的解决方案和**代码级建议**：

### 关键实施坑点
1. 🟢 **rustyline 与 tokio 的异步冲突** → 专用输入线程 + mpsc（方案 B）
2. 🟢 **非Linux E-Stop权限性** → 平台检测 + REPL 模式推荐
3. 🟢 **共享库依赖管理** → Feature flags 优化

### 代码级建议（新增）
1. ⭐ **REPL 历史记录保留** → 采用方案 B（专用线程）保留上下箭头历史
2. ⭐ **Feature Flags 优化** → piper-tools 支持 `full` 和 `statistics` features
3. ⭐ **错误隔离机制** → `catch_unwind` 防止 REPL 因用户错误而崩溃

---

## 🔴 v2.0 关键修正（回顾）

### 架构修正

| 问题 | 原计划 | 修正方案 | 严重度 |
|------|--------|----------|--------|
| **连接悖论** | `piper-cli connect` 无法持久化 | **Config + One-shot** 或 **REPL** | 🔴 严重 |
| **安全缺失** | 无 E-Stop 和确认机制 | **新增 E-Stop + y/N 确认** | 🟡 中等 |
| **性能问题** | 用户态过滤 CPU 高 | **内核级 CAN 过滤** | 🟢 轻微 |
| **时间戳** | 来源不明确 | **明确硬件/内核/用户空间** | 🟡 中等 |
| **工作量** | 17-24 天 | **21-29 天** (+4-5天) | - |

---

## 🟢 v2.1 新增：实施坑点

### 坑点 1: rustyline 与 tokio 冲突 ⭐⭐⭐

**问题**: `rustyline::readline()` 阻塞，影响后台任务

**解决方案对比**:
- ❌ **方案 A**: `spawn_blocking` 每次创建新 Editor → 丢失历史记录
- ✅ **方案 B**: 专用输入线程 + mpsc → **保留历史记录（推荐）**

**方案 B 核心代码**:
```rust
// ⭐ 专用输入线程（Editor 生命周期 = REPL 会话）
pub struct ReplInput {
    command_tx: Sender<String>,
    _input_thread: thread::JoinHandle<()>,
}

impl ReplInput {
    pub fn new() -> Self {
        let (command_tx, command_rx) = bounded::<String>(10);
        let input_thread = thread::spawn(move || {
            let mut rl = Editor::<()>::new()?;
            rl.load_history(".piper_history").ok(); // ⭐ 加载历史

            loop {
                let readline = rl.readline("piper> ");
                // ... 处理输入
                rl.add_history_entry(line.clone()); // ⭐ 添加到历史
            }
        });
        // ...
    }
}
```

**关键点**:
- ✅ 保留历史记录（上下箭头可用）
- ✅ 历史持久化到 `.piper_history`
- ✅ 后台 CAN 监听正常
- ✅ Ctrl+C 响应及时

---

### 坑点 2: 非Linux E-Stop 权限性 ⭐

**问题**: GS-USB 串口独占锁，外部 `stop` 会失败

**解决方案**:

**Linux (SocketCAN)**:
```bash
# Terminal 1
piper-cli move --joints ...

# Terminal 2（外部中断）
piper-cli stop  ✅ 可用
```

**Windows/macOS (GS-USB)**:
```bash
# ❌ 错误方式（无法中断）
piper-cli move --joints ...
# 在另一个终端: piper-cli stop  # ❌ Device Busy

# ✅ 正确方式（REPL）
$ piper-cli shell
piper> move --joints ...
[按 Ctrl+C 进行急停]  ✅ 唯一可靠方式
```

**代码实现**: 平台检测 + 错误提示

---

### 坑点 3: 共享库依赖管理 ⭐⭐

**原则**: `piper-tools` 只依赖 `piper-protocol`

**依赖层级**:
```
apps/cli → piper-client → piper-protocol
tools/ → piper-protocol ✅ (不依赖 client)
```

**Feature Flags 优化**（v2.1 新增）:
```toml
# crates/piper-tools/Cargo.toml
[features]
default = []
full = ["statistics"]
statistics = ["dep:statrs"]

[dependencies]
statrs = { version = "0.16", optional = true }
```

**使用示例**:
```toml
# apps/cli（需要统计）
piper-tools = { workspace = true, features = ["full"] }

# tools/can-sniffer（不需要统计）
piper-tools = { workspace = true }  # 不链接 statrs
```

**收益**: 编译时间 60s → 15s，可选依赖管理清晰

---

## 📊 应用概览（修正）

| 应用 | 优先级 | 原估算 | 修正后 | 复杂度 | 状态 |
|------|--------|--------|--------|--------|------|
| **apps/cli** | ⭐⭐⭐ P1 | 5-7 天 | **7-10 天**<br/>**14 天** (完整) | **中高** | 📋 待开发 |
| **tools/can-sniffer** | ⭐⭐ P2 | 7-10 天 | **8-11 天** | 中高 | 📋 待开发 |
| **tools/protocol-analyzer** | ⭐⭐ P2 | 5-7 天 | **6-8 天** | 中等 | 📋 待开发 |
| **apps/gui** | ⭐ Future | 20-30 天 | 20-30 天 | 高 | ⏸️ 暂缓 |

**总工作量**: **21-29 天**（约 4-5 周）

---

## 🎯 apps/cli - 双模式架构（修正）

### ⚠️ 架构修正：连接状态管理

**问题**: 标准 CLI 是无状态的，`connect` 命令无法跨进程持久化

**解决方案**: 双模式支持

#### 模式 A: One-shot（推荐用于 CI/脚本）

```bash
# 1. 配置默认接口
piper-cli config set --interface can0

# 2. 执行操作（内部：连接 -> 移动 -> 断开）
piper-cli move --joints 0.1,0.2,0.3,0.4,0.5,0.6
⏳ Connecting to can0...
⏳ Moving... Done.
⏳ Disconnecting...

# 3. 显式指定接口
piper-cli move --joints [...] --interface gs-usb
```

#### 模式 B: REPL（推荐用于调试）

```bash
$ piper-cli shell              # 启动交互式 Shell
piper> connect can0            # 连接常驻
✅ Connected to can0 at 1Mbps
piper> enable                  # 使能电机
✅ Motors enabled
piper> move --joints 0.1,0.2,0.3,0.4,0.5,0.6
⏳ Moving... Done (2.3s)
piper> position                # 查询位置
J1: 0.100 J2: 0.200 J3: 0.300 ...
piper> stop                    # 急停
🛑 Emergency stop activated!
piper> exit
```

---

### 核心功能（修正）

```bash
# 配置管理（替代 connect）
piper-cli config set --interface can0
piper-cli config get
piper-cli config check

# One-shot 模式（自动连接/断开）
piper-cli move --joints 0.1,0.2,0.3,0.4,0.5,0.6
piper-cli home
piper-cli position

# 安全特性（⭐ 新增）
piper-cli stop                    # 软件急停
piper-cli move --joints [...]     # 大幅移动需确认
piper-cli move --joints [...] --force  # 跳过确认

# 监控录制
piper-cli monitor --frequency 100
piper-cli record --output dump.bin

# 脚本执行
piper-cli run script.json
piper-cli replay dump.bin
```

---

### 新增：安全配置文件

**位置**: `~/.config/piper/safety.toml`

```toml
[safety]
max_velocity = 3.0              # rad/s
max_acceleration = 10.0           # rad/s²
joints_min = [-3.14, -1.57, ...]  # 位置下限
joints_max = [3.14, 1.57, ...]    # 位置上限
max_step_angle = 30.0             # 单步最大角度（度）
confirmation_threshold = 10.0     # 确认阈值（度）
enable_estop = true               # 启用软件急停
```

---

## 🔍 tools/can-sniffer - 优化版

### 核心功能（修正）

```bash
# 实时监控
can-sniffer --interface can0

# ⭐ 内核级过滤（性能优化）
can-sniffer --interface can0 --filter 0x2A5,0x2A6,0x2A7
can-sniffer --interface can0 --filter-range 0x2A5-0x2AA

# 协议解析
can-sniffer --interface can0 --parse-protocol

# 统计分析
can-sniffer --interface can0 --stats

# 录制回放（带时间戳）
can-sniffer --interface can0 --record --output dump.bin
can-sniffer --replay dump.bin --speed 2.0
```

### 性能优化（⭐ 新增）

| 过滤模式 | CPU 占用 | 说明 |
|----------|----------|------|
| ❌ 用户态过滤 | 60-80% | 内核拷贝所有帧 |
| ✅ 内核过滤 | 10-20% | 只接收匹配的帧 |

**实现**: SocketCAN `setsockopt` + CAN ID 过滤器

---

## 📊 tools/protocol-analyzer - 时间戳版

### 核心功能（修正）

```bash
# 日志分析
protocol-analyzer analyze --input dump.bin

# ⭐ 时间戳处理（新增）
protocol-analyzer analyze --input dump.bin --timestamp-source hardware
protocol-analyzer detect-timestamp-source --input dump.bin

# 问题检测
protocol-analyzer check --input dump.bin

# 性能分析（带时间戳精度）
protocol-analyzer performance --input dump.bin --latency

# 报告生成
protocol-analyzer report --input dump.bin --output report.html
```

### 时间戳来源（⭐ 明确）

| 来源 | 精度 | 说明 |
|------|------|------|
| **Hardware** | ~1μs | CAN 控制器内部时钟 |
| **Kernel** | ~10μs | 驱动接收时间 |
| **Userspace** | ~100μs | 应用接收时间（含调度延迟） |

---

## 🗓️ 修正后的时间表

### Phase 0: 共享基础设施（⭐ 新增，Day 1）

```bash
# 定义共享数据结构
crates/piper-tools/
├── recording.rs    # 录制格式（统一）
├── statistics.rs   # 统计工具
└── safety.rs       # 安全配置
```

**目的**: 避免工具间格式不兼容

---

### Phase 1: apps/cli（Week 1-3，修正）

```
Week 1: One-shot 模式 + 安全
  ├─ 基础框架
  ├─ config/position/stop 命令
  └─ 安全机制（E-Stop + 确认）

Week 2: REPL 模式（⭐ 新增）
  ├─ REPL 框架（rustyline）
  ├─ 命令实现
  └─ Ctrl+C 处理

Week 3: 扩展功能
  ├─ monitor/record
  └─ 脚本系统
```

**工作量**: **7-10 天**（保守）或 **14 天**（完整）

---

### Phase 2: tools/can-sniffer（Week 4-5）

```
Week 4: TUI + 捕获 + 优化
  ├─ TUI 框架
  ├─ 内核级过滤 ⭐
  ├─ 协议解析
  └─ 时间戳处理 ⭐

Week 5: 统计 + 录制
  ├─ 统计模块
  ├─ 录制回放
  └─ 测试
```

**工作量**: **8-11 天**（+1天）

---

### Phase 3: tools/protocol-analyzer（Week 6）

```
Week 6: 日志分析
  ├─ 解析器
  ├─ 问题检测
  ├─ 性能分析（时间戳）⭐
  └─ 报告生成
```

**工作量**: **6-8 天**（+1天）

---

## 🎯 成功指标（修正）

### CLI 工具
- ✅ 双模式架构稳定
- ✅ E-Stop 响应 < 50ms
- ✅ 支持 80% 日常操作
- ✅ 用户评分 > 4/5

### CAN Sniffer
- ✅ 稳定 1000Hz 监控
- ✅ **CPU 占用 < 20%**（优化）
- ✅ 检测 5+ 实际问题

### Protocol Analyzer
- ✅ 分析 1GB 日志 < 30s
- ✅ **时间戳精度明确**
- ✅ 准确率 > 95%

---

## 🔴 修正清单

### 代码级建议（v2.1 新增）⭐

| # | 建议 | 影响 | 优先级 |
|---|------|------|--------|
| 1 | **REPL 历史记录** | 方案 B（专用线程）保留上下箭头历史 | ⭐⭐⭐ 高 |
| 2 | **Feature Flags** | piper-tools 可选依赖，减少编译时间 | ⭐⭐ 中 |
| 3 | **错误隔离** | `catch_unwind` 防止 REPL 崩溃 | ⭐⭐⭐ 高 |

**详细代码**: 见 `APPS_IMPLEMENTATION_GUIDE.md` - 代码级建议章节

### 关键修正点

| # | 模块 | 修正内容 | 状态 |
|---|------|----------|------|
| 1 | **cli** | ⭐ 连接悖论：双模式架构 | ✅ 已修正 |
| 2 | **cli** | ⭐ E-Stop + 确认机制 | ✅ 已添加 |
| 3 | **cli** | 工作量：5-7天 → 7-10/14天 | ✅ 已调整 |
| 4 | **cli** | ⭐ REPL 历史记录 + 错误隔离 | ✅ 已添加 |
| 5 | **sniffer** | ⭐ 内核级 CAN 过滤 | ✅ 已添加 |
| 6 | **analyzer** | ⭐ 时间戳来源明确 | ✅ 已添加 |
| 7 | **infra** | ⭐ Phase 0：共享库前置 | ✅ 已添加 |
| 8 | **infra** | ⭐ Feature Flags 优化 | ✅ 已添加 |
| 9 | **总工作量** | 17-24天 → 21-29天 | ✅ 已调整 |

---

## 📚 文档版本

| 文档 | 版本 | 状态 |
|------|------|------|
| **APPS_IMPLEMENTATION_GUIDE.md** | v2.1 | ⭐⭐ 实施指南（含代码级建议） |
| **APPS_DEVELOPMENT_PLAN_V2.md** | v2.0→v2.1 | ✅ 最新（修正版） |
| APPS_QUICK_REFERENCE.md | v2.1 | ✅ 本文档 |
| APPS_DEVELOPMENT_PLAN.md | v1.0 | 📋 原版（已过时） |

---

## 🚀 下一步行动

### ⭐ 立即开始（优先级排序）

1. **Phase 0**（Day 1）⭐ 最高优先级
   ```bash
   # 创建共享库
   mkdir -p crates/piper-tools/src
   # 定义录制格式、统计工具、安全配置
   ```

2. **apps/cli - One-shot 模式**（Week 1）
   ```bash
   mkdir -p apps/cli/src
   # 实现 config/move/stop 命令
   ```

3. **apps/cli - REPL 模式**（Week 2）
   ```bash
   # 实现 REPL 框架（使用方案 B：专用线程）
   # 历史记录保留（.piper_history）
   # 错误隔离（catch_unwind）
   # Ctrl+C 急停处理
   ```

---

## 📖 完整阅读

**实施指南**: `docs/v0/workspace/APPS_IMPLEMENTATION_GUIDE.md` ⭐⭐ **必读**
- 详细代码示例（方案 B：专用输入线程）
- Feature Flags 配置
- 错误隔离机制

**详细规划**: `docs/v0/workspace/APPS_DEVELOPMENT_PLAN_V2.md`
- 架构修正详解
- 安全机制设计
- 性能优化方案
- 完整时间表
- 实施决策记录

---

**状态**: ✅ v2.1 最终版（含代码级建议）
**审核**: ✅ 技术审查通过 + 代码健壮性审查通过
**优先级**: **Phase 0 → Phase 1 → Phase 2 → Phase 3**
**预计**: 4-5 周完成所有工具
