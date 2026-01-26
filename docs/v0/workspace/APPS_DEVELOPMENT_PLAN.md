# Piper SDK 扩展应用开发规划

**日期**: 2026-01-26
**版本**: v1.0
**状态**: 规划阶段
**作者**: Claude Code

---

## 📋 目录

1. [执行摘要](#执行摘要)
2. [应用优先级矩阵](#应用优先级矩阵)
3. [apps/cli - 命令行工具](#apps-cli---命令行工具)
4. [tools/can-sniffer - CAN 总线监控](#toolscan-sniffer---can-总线监控)
5. [tools/protocol-analyzer - 协议分析器](#toolsprotocol-analyzer---协议分析器)
6. [apps/gui - 上位机 GUI（未来规划）](#appsgui---上位机-guifuture)
7. [共享基础设施](#共享基础设施)
8. [实施时间表](#实施时间表)
9. [资源需求](#资源需求)

---

## 执行摘要

### 规划目标

基于已完成的 workspace 重构，规划三个核心工具的开发：

1. **apps/cli** - 命令行工具（高优先级）
2. **tools/can-sniffer** - CAN 总线监控工具（中优先级）
3. **tools/protocol-analyzer** - 协议分析器（中优先级）

**暂缓**: apps/gui（上位机 GUI，复杂度高，建议后续实施）

### 预期收益

- ✅ 提升开发者体验（CLI 工具）
- ✅ 简化调试过程（CAN sniffer）
- ✅ 加速问题诊断（协议分析器）
- ✅ 验证 workspace 架构的可扩展性
- ✅ 为未来 GUI 应用积累经验

### 总工作量估算

| 应用 | 预估工作量 | 优先级 | 复杂度 |
|------|-----------|--------|--------|
| apps/cli | 5-7 天 | ⭐⭐⭐ 高 | 中等 |
| tools/can-sniffer | 7-10 天 | ⭐⭐ 中 | 中高 |
| tools/protocol-analyzer | 5-7 天 | ⭐⭐ 中 | 中等 |
| apps/gui | 20-30 天 | ⭐ 低 | 高 |

**总计**: 约 17-24 天（不包括 GUI）

---

## 应用优先级矩阵

### 价值 vs 复杂度分析

```
高复杂度
    │
    │     ┌─────────────┐
    │     │   GUI       │  (未来)
    │     │   (暂缓)    │
    │     └─────────────┘
    │
中  │  ┌─────────┐  ┌──────────┐
    │  │Sniffer  │  │ Analyzer │
复  │  │ (P2)    │  │  (P2)    │
杂  │  └─────────┘  └──────────┘
度  │
    │     ┌─────────┐
    │     │   CLI   │  (P1)
    │     │  (P1)   │
    │     └─────────┘
    │
    └───────────────────────────────────→ 高价值
```

**优先级说明**:
- **P1 (Phase 1)**: apps/cli - 立即开发，高频使用
- **P2 (Phase 2)**: can-sniffer, protocol-analyzer - 第二批
- **Future**: GUI - 等待前面工具稳定后再考虑

---

## apps/cli - 命令行工具

### 📊 概述

**目标**: 提供快速、强大的命令行接口，用于机械臂的日常操作、调试和测试

**用户**: 开发者、测试工程师、运维人员

**技术栈**:
- Rust 2024 Edition
- `clap` 4.x - CLI 框架
- `piper-client` - 核心依赖
- `anyhow` - 错误处理
- `tracing` + `tracing-subscriber` - 日志

---

### 🎯 核心功能模块

#### 1. 连接管理模块

```bash
# 连接到机械臂
piper-cli connect --interface can0
piper-cli connect --interface gs-usb --serial 0001:1234
piper-cli connect --interface socketcan --name can0

# 显示连接状态
piper-cli status

# 显示详细信息
piper-cli info

# 断开连接
piper-cli disconnect
```

**功能点**:
- ✅ 支持多种接口（SocketCAN, GS-USB）
- ✅ 自动检测可用接口
- ✅ 连接状态持久化（配置文件）
- ✅ 超时和重试机制

**实现**:
```rust
// src/commands/connect.rs
use clap::{Parser, Subcommand};
use piper_client::{PiperBuilder, state::*};

#[derive(Parser, Debug)]
struct ConnectArgs {
    /// 接口类型 (can0, gs-usb, socketcan)
    #[arg(short, long)]
    interface: String,

    /// GS-USB 设备序列号（仅 GS-USB）
    #[arg(long)]
    serial: Option<String>,

    /// SocketCAN 接口名称
    #[arg(long, default_value = "can0")]
    name: String,
}

async fn handle_connect(args: ConnectArgs) -> anyhow::Result<()> {
    let piper = PiperBuilder::new()?
        .connect(&args.interface)?
        .enable_mit_mode(MitModeConfig::default())?;

    println!("✅ Connected to Piper robot");
    Ok(())
}
```

---

#### 2. 关节控制模块

```bash
# 使能/失能电机
piper-cli enable
piper-cli disable

# 回到零位
piper-cli home

# 关节位置控制
piper-cli move --joints 0.5,0.7,-0.4,0.2,0.3,0.5
piper-cli move --joints "[0.5, 0.7, -0.4, 0.2, 0.3, 0.5]"

# 单关节控制
piper-cli move --joint 0 --position 0.5
piper-cli move --joint 1 --position 0.7

# 获取当前位置
piper-cli position
piper-cli position --json

# 关节速度限制
piper-cli move --joints 0,0,0,0,0,0 --velocity-limit 1.0
```

**功能点**:
- ✅ 支持多关节和单关节控制
- ✅ 速度限制和加速度限制
- ✅ 位置单位（弧度/度）切换
- ✅ JSON 输出格式（便于脚本集成）

**实现**:
```rust
// src/commands/move.rs
#[derive(Parser, Debug)]
struct MoveArgs {
    /// 目标关节位置（6个值，逗号分隔）
    #[arg(short, long, value_delimiter = ',')]
    joints: Option<Vec<f64>>,

    /// 单关节索引（0-5）
    #[arg(long)]
    joint: Option<usize>,

    /// 单关节位置
    #[arg(long)]
    position: Option<f64>,

    /// 速度限制（rad/s）
    #[arg(long)]
    velocity_limit: Option<f64>,
}

async fn handle_move(args: MoveArgs, piper: &mut Piper<Active<MitMode>>)
    -> anyhow::Result<()>
{
    if let Some(joint_idx) = args.joint {
        if let Some(pos) = args.position {
            // 单关节控制
            // ...
        }
    } else if let Some(positions) = args.joints {
        // 多关节控制
        // ...
    }
    Ok(())
}
```

---

#### 3. 夹爪控制模块

```bash
# 打开/关闭夹爪
piper-cli gripper open
piper-cli gripper close

# 精确位置控制
piper-cli gripper --position 0.5
piper-cli gripper --position 0.0  # 完全打开
piper-cli gripper --position 1.0  # 完全关闭

# 力度控制
piper-cli gripper --force 10.0

# 获取夹爪状态
piper-cli gripper --status
```

---

#### 4. 监控和录制模块

```bash
# 实时监控（100Hz）
piper-cli monitor --frequency 100
piper-cli monitor --frequency 1000 --format json

# 监控特定数据
piper-cli monitor --fields position,velocity,torque

# 录制 CAN 流量
piper-cli record --output can_dump.bin --duration 60

# 录制带时间戳
piper-cli record --output session_$(date +%Y%m%d_%H%M%S).bin

# 录制并实时显示
piper-cli record --output test.bin --verbose
```

**功能点**:
- ✅ 可配置监控频率（1-1000Hz）
- ✅ 多种输出格式（人类可读、JSON、CSV）
- ✅ 字段选择（只监控需要的）
- ✅ 录制到文件（二进制格式）
- ✅ 自动文件命名（时间戳）

**数据格式**:
```rust
// 二进制录制格式（使用 serde + bincode）
#[derive(Serialize, Deserialize)]
struct CANFrameDump {
    timestamp_us: u64,
    can_id: u32,
    data: Vec<u8>,
    dlc: u8,
}

// CSV 输出格式
timestamp,can_id,dlc,data
1706234567890123,0x2A5,8,00,01,02,03,04,05,06,07
```

---

#### 5. 脚本执行模块

```bash
# 执行脚本文件
piper-cli run script.json
piper-cli run --replay script.json

# 验证脚本（不执行）
piper-cli run --validate script.json

# 从标准输入读取
echo '{"move": {"joints": [0,0,0,0,0,0]}}' | piper-cli run -

# 回放 CAN 日志
piper-cli replay can_dump.bin
piper-cli replay can_dump.bin --speed 2.0  # 2倍速
```

**脚本格式**:
```json
{
  "version": "1.0",
  "description": "Pick and place demo",
  "steps": [
    {
      "type": "move",
      "joints": [0.5, 0.7, -0.4, 0.2, 0.3, 0.5],
      "velocity_limit": 1.0
    },
    {
      "type": "wait",
      "duration_ms": 1000
    },
    {
      "type": "gripper",
      "position": 1.0
    },
    {
      "type": "move",
      "joints": [0.0, 0.0, 0.0, 0.0, 0.0, 0.0]
    }
  ]
}
```

---

### 📁 项目结构

```
apps/cli/
├── Cargo.toml
├── src/
│   ├── main.rs                 # 入口
│   ├── cli.rs                 # clap 配置
│   ├── config.rs              # 配置文件管理
│   ├── commands/
│   │   ├── mod.rs
│   │   ├── connect.rs         # 连接管理
│   │   ├── move.rs            # 关节控制
│   │   ├── gripper.rs         # 夹爪控制
│   │   ├── monitor.rs         # 监控
│   │   ├── record.rs          # 录制
│   │   ├── run.rs             # 脚本执行
│   │   └── replay.rs          # 回放
│   ├── format/
│   │   ├── mod.rs
│   │   ├── json.rs            # JSON 输出
│   │   ├── human.rs           # 人类可读
│   │   └── csv.rs             # CSV 输出
│   └── script/
│       ├── mod.rs
│       ├── parser.rs          # 脚本解析
│       ├── validator.rs       # 脚本验证
│       └── executor.rs        # 脚本执行
└── examples/
    └── scripts/
        ├── demo_pick_and_place.json
        └── calibration.json
```

---

### 📦 依赖关系

```toml
[package]
name = "piper-cli"
version.workspace = true
edition.workspace = true

[[bin]]
name = "piper-cli"
path = "src/main.rs"

[dependencies]
piper-client = { workspace = true }
piper-driver = { workspace = true }

# CLI 框架
clap = { workspace = true }

# 错误处理
anyhow = "1.0"
thiserror = { workspace = true }

# 日志
tracing = { workspace = true }
tracing-subscriber = { workspace = true }

# 序列化
serde = { workspace = true }
serde_json = { workspace = true }
bincode = "1.3"  # 二进制序列化

# 并发
tokio = { workspace = true }

# 文件 I/O
dirs = "5.0"  # 配置目录
```

---

### 🗓️ 开发阶段

#### 阶段 1: 基础框架（2天）

- [ ] 项目结构搭建
- [ ] clap 命令行框架
- [ ] 配置文件管理
- [ ] 日志系统
- [ ] 错误处理

#### 阶段 2: 核心命令（3天）

- [ ] `connect` / `disconnect` / `status` / `info`
- [ ] `enable` / `disable` / `home`
- [ ] `move` (关节控制)
- [ ] `position` (状态查询)

#### 阶段 3: 扩展功能（2天）

- [ ] `gripper` 命令
- [ ] `monitor` 命令
- [ ] `record` 命令
- [ ] 多种输出格式（JSON, CSV, 人类可读）

#### 阶段 4: 高级功能（2天）

- [ ] 脚本系统
- [ ] `run` 命令
- [ ] `replay` 命令
- [ ] 脚本验证

**总计**: 9 天

---

## tools/can-sniffer - CAN 总线监控

### 📊 概述

**目标**: 实时监控和分析 CAN 总线流量，用于调试和诊断

**用户**: 开发者、硬件工程师、测试工程师

**技术栈**:
- Rust 2024 Edition
- `ratatui` - TUI 终端界面
- `piper-can` - CAN 接口
- `piper-protocol` - 协议解析
- `tokio` - 异步运行时

---

### 🎯 核心功能模块

#### 1. 实时监控界面

```bash
# 启动实时监控
can-sniffer --interface can0

# 指定过滤器
can-sniffer --interface can0 --filter 0x2A5,0x2A6,0x2A7

# 显示协议解析
can-sniffer --interface can0 --parse-protocol

# 只显示错误帧
can-sniffer --interface can0 --errors-only
```

**TUI 界面布局**:
```
┌─────────────────────────────────────────────────────────────┐
│ Piper CAN Sniffer v0.1.0                    can0 @ 1000 fps │
├─────────────────────────────────────────────────────────────┤
│ │ Frame │ CAN ID   │ Type    │ Data (hex)              │ Parsed │
├─┼────────┼──────────┼─────────┼─────────────────────────┼─────────┤
│↑│ 12345  │ 0x2A5    │ Feedback│ 00 12 34 56 ...         │ J1:0.12 │
│ │ 12346  │ 0x1A1    │ Control │ 01 00 00 00 ...         │ Cmd:01  │
│ │ 12347  │ 0x2A6    │ Feedback│ 00 23 45 67 ...         │ J2:0.23 │
│ │        │          │         │                         │         │
└─┴────────┴──────────┴─────────┴─────────────────────────┴─────────┘
│ Statistics:                                                 │
│   Frames: 12,345 | Errors: 2 | Bandwidth: 123 KB/s         │
│   FPS: 1000    | Lost: 0  │ Load: 15%                      │
└─────────────────────────────────────────────────────────────┘
```

**功能点**:
- ✅ 实时滚动显示（可配置速度）
- ✅ 颜色高亮（错误帧红色、控制帧蓝色）
- ✅ 自动滚动/暂停
- ✅ 支持搜索和过滤
- ✅ 协议解析注释

---

#### 2. 协议解析模块

```rust
// src/parser/mod.rs
use piper_protocol::{feedback, control};

#[derive(Debug, Clone)]
enum ParsedFrame {
    Feedback {
        joint_index: usize,
        position: f64,
        velocity: f64,
        torque: f64,
    },
    Control {
        joint_index: usize,
        mode: ControlMode,
    },
    Unknown {
        can_id: u32,
        data: Vec<u8>,
    },
}

fn parse_frame(frame: &PiperFrame) -> ParsedFrame {
    match frame.id() {
        0x2A5..=0x2AA => {
            // 反馈帧
            let feedback = feedback::JointDriverHighSpeedFeedback::from_raw(&frame);
            ParsedFrame::Feedback {
                joint_index: (frame.id() - 0x2A5) as usize,
                position: feedback.position().into(),
                velocity: feedback.velocity().into(),
                torque: feedback.torque().into(),
            }
        }
        0x1A1..=0x1A6 => {
            // 控制帧
            ParsedFrame::Control {
                joint_index: (frame.id() - 0x1A1) as usize,
                mode: ControlMode::Mit,
            }
        }
        _ => ParsedFrame::Unknown {
            can_id: frame.id(),
            data: frame.data().to_vec(),
        }
    }
}
```

---

#### 3. 统计分析模块

```bash
# 实时统计
can-sniffer --interface can0 --stats

# 生成报告
can-sniffer --interface can0 --stats --output stats.json

# 统计特定时间段
can-sniffer --interface can0 --stats --duration 60
```

**统计指标**:
- **流量统计**:
  - 总帧数
  - FPS (帧/秒)
  - 带宽利用率 (KB/s)
  - 峰值/平均/谷值

- **错误统计**:
  - 错误帧数量
  - 错误率 (%)
  - 错误类型分布

- **延迟统计**:
  - 最小/最大/平均延迟
  - 抖动 (Jitter)
  - 丢帧率

- **协议分布**:
  - 反馈帧占比
  - 控制帧占比
  - 配置帧占比

**输出格式**:
```json
{
  "timestamp_us": 1706234567890123,
  "duration_s": 60,
  "total_frames": 60000,
  "fps": 1000,
  "bandwidth_kbps": 784,
  "errors": {
    "total": 2,
    "rate": 0.0033,
    "by_type": {
      "crc": 1,
      "stuff": 1
    }
  },
  "latency_us": {
    "min": 45,
    "max": 123,
    "avg": 67,
    "jitter": 12
  }
}
```

---

#### 4. 录制和回放模块

```bash
# 录制 CAN 流量
can-sniffer --interface can0 --record --output dump.bin

# 录制带时间戳
can-sniffer --interface can0 --record --format full --output dump.bin

# 回放（实时速度）
can-sniffer --replay dump.bin

# 回放（指定速度）
can-sniffer --replay dump.bin --speed 2.0

# 回放（循环）
can-sniffer --replay dump.bin --loop
```

**录制格式**:
```rust
#[derive(Serialize, Deserialize)]
struct CANRecording {
    version: u8,
    timestamp_start_us: u64,
    frames: Vec<CANFrameEntry>,
}

#[derive(Serialize, Deserialize)]
struct CANFrameEntry {
    timestamp_us: u64,
    can_id: u32,
    data: Vec<u8>,
    dlc: u8,
    is_extended: bool,
    is_error: bool,
}
```

---

### 📁 项目结构

```
tools/can-sniffer/
├── Cargo.toml
├── src/
│   ├── main.rs                 # 入口
│   ├── cli.rs                 # 命令行解析
│   ├── tui/
│   │   ├── mod.rs             # TUI 入口
│   │   ├── ui.rs              # 界面布局
│   │   ├── app.rs             # 应用状态
│   │   └── widgets/
│   │       ├── mod.rs
│   │       ├── frame_table.rs # 帧表格
│   │       ├── stats.rs       # 统计面板
│   │       └── help.rs        # 帮助面板
│   ├── capture.rs             # CAN 捕获
│   ├── parser/
│   │   ├── mod.rs
│   │   ├── protocol.rs        # 协议解析
│   │   └── annotations.rs     # 注释生成
│   ├── statistics.rs          # 统计计算
│   ├── recorder.rs            # 录制功能
│   ├── replayer.rs            # 回放功能
│   └── filter.rs              # 过滤器
└── README.md
```

---

### 📦 依赖关系

```toml
[package]
name = "can-sniffer"
version.workspace = true
edition.workspace = true

[dependencies]
piper-can = { workspace = true }
piper-protocol = { workspace = true }

# TUI 框架
ratatui = "0.26"
crossterm = "0.27"

# 协议解析
serde = { workspace = true }
bincode = "1.3"

# 异步
tokio = { workspace = true, features = ["full"] }

# 错误处理
anyhow = "1.0"
thiserror = { workspace = true }

# 日志
tracing = { workspace = true }
tracing-subscriber = { workspace = true }
```

---

### 🗓️ 开发阶段

#### 阶段 1: 基础捕获（3天）

- [ ] CAN 接口集成
- [ ] 异步帧接收
- [ ] 基础 TUI 框架
- [ ] 帧表格显示

#### 阶段 2: 协议解析（2天）

- [ ] 反馈帧解析
- [ ] 控制帧解析
- [ ] 注释生成
- [ ] 错误帧处理

#### 阶段 3: 统计分析（2天）

- [ ] 实时统计计算
- [ ] FPS/带宽监控
- [ ] 错误率计算
- [ ] 延迟分析

#### 阶段 4: 录制回放（2天）

- [ ] 二进制录制格式
- [ ] 回放引擎
- [ ] 速度控制
- [ ] 循环播放

#### 阶段 5: 高级功能（1天）

- [ ] 过滤器系统
- [ ] 搜索功能
- [ ] 导出功能（CSV, JSON）
- [ ] 配置持久化

**总计**: 10 天

---

## tools/protocol-analyzer - 协议分析器

### 📊 概述

**目标**: 离线分析 CAN 日志文件，检测问题、生成报告

**用户**: 开发者、测试工程师、质量保证

**技术栈**:
- Rust 2024 Edition
- `piper-protocol` - 协议定义
- `plotters` - 图表生成
- `serde_json` - JSON 处理

---

### 🎯 核心功能模块

#### 1. 日志解析模块

```bash
# 解析日志文件
protocol-analyzer analyze --input can_dump.bin

# 解析多种格式
protocol-analyzer analyze --input dump.log --format can-utils
protocol-analyzer analyze --input dump.txt --format candump

# 输出格式
protocol-analyzer analyze --input dump.bin --output report.json
protocol-analyzer analyze --input dump.bin --output report.md
```

**支持格式**:
1. **二进制格式** (can-sniffer 录制)
2. **can-utils 格式** (candump)
3. **文本格式** (自定义)

**示例**:
```
# can-utils candump 格式
(000.000000) can0 2A5#0102030405060708
(000.001234) can0 2A6#0102030405060708
```

---

#### 2. 问题检测模块

```bash
# 检测协议违规
protocol-analyzer check --input dump.bin

# 检测特定问题
protocol-analyzer check --input dump.bin --check missed-frames
protocol-analyzer check --input dump.bin --check timing-violations
protocol-analyzer check --input dump.bin --check sequence-errors

# 生成详细报告
protocol-analyzer check --input dump.bin --verbose --output issues.json
```

**检测类型**:

1. **丢帧检测**:
   - 识别缺失的序列号
   - 检测反馈帧间隙
   - 统计丢帧率

2. **时序违规**:
   - 帧间隔异常（太长/太短）
   - FPS 偏差检测
   - 抖动分析

3. **序列错误**:
   - 控制帧序列不连续
   - 状态机异常
   - 未预期的模式转换

4. **数据异常**:
   - 位置/速度/力矩超限
   - NaN 或 Inf 值
   - 数据一致性检查

**输出格式**:
```json
{
  "analysis_time": "2026-01-26T12:34:56Z",
  "input_file": "can_dump.bin",
  "total_frames": 60000,
  "issues": {
    "missed_frames": {
      "count": 5,
      "rate": 0.0083,
      "locations": [
        { "frame_id": 1234, "expected_seq": 5, "actual_seq": 7 }
      ]
    },
    "timing_violations": {
      "count": 2,
      "details": [
        { "frame_id": 5678, "interval_us": 15000, "expected_us": 10000 }
      ]
    }
  }
}
```

---

#### 3. 性能分析模块

```bash
# 性能统计
protocol-analyzer performance --input dump.bin

# FPS 分析
protocol-analyzer performance --input dump.bin --fps

# 带宽分析
protocol-analyzer performance --input dump.bin --bandwidth

# 延迟分析
protocol-analyzer performance --input dump.bin --latency
```

**分析维度**:

1. **FPS 分析**:
   - 实际 FPS vs 理论 FPS (200Hz)
   - FPS 稳定性（标准差）
   - FPS 分布直方图

2. **带宽分析**:
   - 总带宽利用率
   - 峰值/平均带宽
   - 带宽按帧类型分布

3. **延迟分析**:
   - 控制命令到反馈的延迟
   - 延迟分布
   - 延迟抖动

**图表生成**:
```rust
// 使用 plotters 生成图表
use plotters::prelude::*;

fn draw_fps_chart(data: &[FPSData], output: &str) -> Result<(), Box<dyn std::error::Error>> {
    let root = BitMapBackend::new(output, (800, 600)).into_drawing_area();
    root.fill(&WHITE);

    let mut chart = ChartBuilder::on(&root)
        .caption("FPS Over Time", ("sans-serif", 40))
        .x_label_area_size(40)
        .y_label_area_size(40)
        .build_cartesian_2d(0..data.len(), 190..210)?;

    // 绘制曲线...
    Ok(())
}
```

---

#### 4. 报告生成模块

```bash
# 生成 HTML 报告
protocol-analyzer report --input dump.bin --output report.html

# 生成 PDF 报告
protocol-analyzer report --input dump.bin --output report.pdf

# 生成 Markdown 报告
protocol-analyzer report --input dump.bin --output report.md
```

**报告内容**:
1. **摘要**:
   - 总帧数
   - 录制时长
   - 平均 FPS
   - 问题概述

2. **详细分析**:
   - 每种问题的详细列表
   - 时间线分析
   - 趋势图表

3. **建议**:
   - 发现的问题
   - 改进建议
   - 优化方向

---

### 📁 项目结构

```
tools/protocol-analyzer/
├── Cargo.toml
├── src/
│   ├── main.rs                 # 入口
│   ├── cli.rs                 # 命令行
│   ├── parser/
│   │   ├── mod.rs
│   │   ├── binary.rs          # 二进制格式
│   │   ├── candump.rs         # can-utils 格式
│   │   └── custom.rs          # 自定义格式
│   ├── analyzer/
│   │   ├── mod.rs
│   │   ├── missed_frames.rs   # 丢帧检测
│   │   ├── timing.rs          # 时序分析
│   │   ├── sequence.rs        # 序列检测
│   │   └── data_anomaly.rs    # 数据异常
│   ├── statistics/
│   │   ├── mod.rs
│   │   ├── fps.rs             # FPS 统计
│   │   ├── bandwidth.rs       # 带宽统计
│   │   └── latency.rs         # 延迟统计
│   ├── chart/
│   │   ├── mod.rs
│   │   ├── drawer.rs          # 图表绘制
│   │   └── templates.rs       # 图表模板
│   ├── report/
│   │   ├── mod.rs
│   │   ├── html.rs            # HTML 报告
│   │   ├── markdown.rs        # Markdown 报告
│   │   └── json.rs            # JSON 报告
│   └── models.rs              # 数据结构
└── examples/
    └── reports/
        └── template.html
```

---

### 📦 依赖关系

```toml
[package]
name = "protocol-analyzer"
version.workspace = true
edition.workspace = true

[dependencies]
piper-protocol = { workspace = true }

# 序列化
serde = { workspace = true }
serde_json = { workspace = true }
bincode = "1.3"

# 图表生成
plotters = "0.3"

# 时间处理
chrono = "0.4"

# 统计
statrs = "0.16"

# 报告生成
handlebars = "5.0"  # HTML 模板

# 错误处理
anyhow = "1.0"
thiserror = { workspace = true }
```

---

### 🗓️ 开发阶段

#### 阶段 1: 日志解析（2天）

- [ ] 二进制格式解析
- [ ] can-utils 格式解析
- [ ] 自定义格式支持
- [ ] 错误处理

#### 阶段 2: 问题检测（2天）

- [ ] 丢帧检测
- [ ] 时序违规检测
- [ ] 序列错误检测
- [ ] 数据异常检测

#### 阶段 3: 性能分析（2天）

- [ ] FPS 分析
- [ ] 带宽分析
- [ ] 延迟分析
- [ ] 统计计算

#### 阶段 4: 报告生成（1天）

- [ ] JSON 报告
- [ ] Markdown 报告
- [ ] HTML 报告（带图表）

**总计**: 7 天

---

## apps/gui - 上位机 GUI（未来规划）

### ⏸️ 暂缓原因

1. **复杂度高**:
   - 需要学习 Tauri 框架
   - 前端开发（React/Vue）
   - 3D 可视化（Three.js）
   - 实时数据绑定

2. **依赖前面的工具**:
   - CLI 工具提供命令行接口
   - can-sniffer 提供调试经验
   - protocol-analyzer 提供诊断能力

3. **用户体验积累**:
   - 通过 CLI 工具了解用户需求
   - 通过 sniffer 了解常见问题
   - 通过 analyzer 了解性能瓶颈

### 📋 未来规划（Phase 4）

**预计工作量**: 20-30 天

**技术选型**: Tauri + React + Three.js

**核心模块**:
1. 连接管理
2. 3D 可视化
3. 关节控制
4. 数据监控
5. 脚本编辑器
6. 设置面板

详细规划待前面工具稳定后制定。

---

## 共享基础设施

### 1. 共享库 crate

考虑创建 `crates/piper-tools` 共享库:

```
crates/piper-tools/
├── Cargo.toml
└── src/
    ├── lib.rs
    ├── recording.rs           # 录制格式定义
    ├── statistics.rs          # 统计工具
    └── chart.rs               # 图表工具
```

**用途**:
- 统一的录制格式
- 共享的统计算法
- 通用的图表生成

---

### 2. 配置文件格式

所有工具共享配置文件 `~/.config/piper/config.toml`:

```toml
[default]
interface = "can0"
baudrate = 1000000

[cli]
output_format = "json"
log_level = "info"

[sniffer]
max_fps = 1000
auto_scroll = true

[analyzer]
output_dir = "~/piper/logs"
```

---

### 3. 错误处理统一

定义统一的错误类型:

```rust
// crates/piper-tools/src/error.rs
#[derive(thiserror::Error, Debug)]
pub enum ToolError {
    #[error("CAN error: {0}")]
    Can(#[from] piper_can::CanError),

    #[error("Protocol error: {0}")]
    Protocol(#[from] piper_protocol::ProtocolError),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Serialization error: {0}")]
    Serialization(String),
}
```

---

## 实施时间表

### Phase 1: CLI 工具（Week 1-2）

```
Week 1: 基础框架 + 核心命令
  Day 1-2: 项目搭建 + clap 框架
  Day 3-4: connect/move/position 命令
  Day 5: 测试和文档

Week 2: 扩展功能
  Day 1-2: monitor/record 命令
  Day 3-4: 脚本系统
  Day 5: 测试和文档
```

### Phase 2: CAN Sniffer（Week 3-4）

```
Week 3: TUI + 捕获
  Day 1-2: TUI 框架 + CAN 接口
  Day 3-4: 协议解析 + 显示
  Day 5: 测试

Week 4: 统计 + 录制
  Day 1-2: 统计模块
  Day 3-4: 录制回放
  Day 5: 测试和文档
```

### Phase 3: Protocol Analyzer（Week 5）

```
Week 5: 日志分析
  Day 1-2: 解析器
  Day 3-4: 问题检测
  Day 5: 报告生成
```

### Phase 4: GUI 应用（Week 8+）暂缓

---

## 资源需求

### 开发资源

| 角色 | 工作量 | 技能要求 |
|------|--------|----------|
| Rust 开发 | 全程 | Rust, Tokio, CAN 协议 |
| 前端开发 | GUI (Phase 4) | React, Vue, Three.js |
| 测试工程师 | 兼职 | 测试用例设计, 自动化 |
| 文档编写 | 兼职 | 技术写作, 示例代码 |

### 硬件需求

- Piper 机械臂（用于测试）
- CAN 接口（SocketCAN 或 GS-USB）
- 开发机（Linux/macOS/Windows）

### 软件工具

- Rust 工具链
- Git
- CAN 分析工具（对比测试）
- 文档生成工具

---

## 成功指标

### Phase 1: CLI 工具

- ✅ 支持 80% 的日常操作
- ✅ 响应时间 < 100ms
- ✅ 内存占用 < 50MB
- ✅ 用户反馈评分 > 4/5

### Phase 2: CAN Sniffer

- ✅ 支持 1000Hz 稳定监控
- ✅ CPU 占用 < 30%
- ✅ 协议解析准确率 100%
- ✅ 检测到至少 5 个实际问题

### Phase 3: Protocol Analyzer

- ✅ 分析 1GB 日志 < 30s
- ✅ 问题检测准确率 > 95%
- ✅ 生成报告时间 < 5s
- ✅ 帮助解决 3+ 个实际问题

---

## 风险与缓解

### 风险 1: TUI 学习曲线

**影响**: can-sniffer 开发延迟

**缓解**:
- 提前学习 ratatui 框架
- 参考 ratatui 示例项目
- 简化初始功能，逐步增加

### 风险 2: 性能问题

**影响**: 监控工具无法稳定运行

**缓解**:
- 异步架构（tokio）
- 性能测试和优化
- 降级方案（降低频率）

### 风险 3: 兼容性问题

**影响**: 不同平台行为不一致

**缓解**:
- 跨平台测试
- 抽象接口层
- 完善的单元测试

---

## 总结

### 开发路线图

```
Phase 1 (Week 1-2): apps/cli
    ↓
Phase 2 (Week 3-4): tools/can-sniffer
    ↓
Phase 3 (Week 5): tools/protocol-analyzer
    ↓
Phase 4 (Week 8+): apps/gui (未来)
```

### 总工作量

- **Phase 1-3**: 17-24 天（约 3-4 周）
- **Phase 4**: 20-30 天（未来）
- **总计**: 约 6-8 周（完成所有工具）

### 下一步行动

1. ✅ **立即开始**: apps/cli 开发
2. ⏳ **两周后**: 开始 tools/can-sniffer
3. ⏳ **一个月后**: 开始 tools/protocol-analyzer
4. 📅 **两个月后**: 评估 GUI 应用需求

---

**最后更新**: 2026-01-26
**作者**: Claude Code
**状态**: ✅ 规划完成，等待审核
**下一步**: 开始 apps/cli 开发
