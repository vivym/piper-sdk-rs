# Piper CLI

Piper 机器人臂命令行工具。

## 概述

`piper-cli` 是 Piper 机器人臂的命令行接口，支持两种运行模式：

- **One-shot 模式**：每个命令独立执行（推荐用于 CI/脚本）
- **REPL 模式**：交互式 Shell（推荐用于调试）

## 安装

```bash
cargo install --path apps/cli
```

## 快速开始

### 1. 使用示例脚本

我们提供了示例脚本来快速体验 CLI 功能：

```bash
# 执行示例移动序列
piper-cli run --script examples/move_sequence.json

# 查看更多示例
ls examples/
```

更多示例脚本说明请参考 [examples/README.md](examples/README.md)

### 2. 监控机器人状态

```bash
# 实时监控（默认 10 Hz）
piper-cli monitor

# 指定刷新频率
piper-cli monitor --frequency 20
```

### 3. 录制和回放

```bash
# 录制 10 秒
piper-cli record --output test.bin --duration 10

# 回放录制
piper-cli replay --input test.bin
```

## 使用方法

### One-shot 模式

```bash
# 1. 配置默认接口
piper-cli config set --interface can0

# 2. 移动到目标位置
piper-cli move --joints 0.1,0.2,0.3,0.4,0.5,0.6

# 3. 查询当前位置
piper-cli position

# 4. 急停
piper-cli stop
```

### REPL 模式

```bash
$ piper-cli shell
piper> move --joints 0.1,0.2,0.3
piper> position
piper> stop
piper> exit
```

## 命令

### config - 配置管理

```bash
piper-cli config set --interface can0
piper-cli config get interface
piper-cli config check
```

### move - 移动关节

```bash
# 移动到目标位置（弧度）
piper-cli move --joints 0.1,0.2,0.3,0.4,0.5,0.6

# 跳过确认提示
piper-cli move --joints ... --force

# 指定接口
piper-cli move --joints ... --interface gs-usb --serial 0001:1234
```

### position - 查询位置

```bash
piper-cli position
```

### stop - 急停

```bash
piper-cli stop
```

### home - 回零位

```bash
piper-cli home
```

### shell - REPL 模式

```bash
piper-cli shell
```

### monitor - 实时监控

实时监控机器人状态（关节位置、速度、电流等）：

```bash
# 默认 10 Hz 刷新
piper-cli monitor

# 指定刷新频率（20 Hz）
piper-cli monitor --frequency 20
```

输出包括：
- 状态更新频率（FPS）
- 关节角度（度）
- 末端位姿（位置和姿态）
- 关节速度和电流
- 夹爪状态

### record - 录制 CAN 总线数据

录制 CAN 总线数据到文件：

```bash
# 录制 10 秒
piper-cli record --output recording.bin --duration 10

# 无限录制（手动停止）
piper-cli record --output recording.bin

# 在接收到特定 CAN ID 时停止
piper-cli record --output recording.bin --stop-on-id 0x2A5

# 指定接口
piper-cli record --output recording.bin --interface can0
```

### run - 执行脚本

执行 JSON 脚本文件：

```bash
# 执行脚本
piper-cli run --script move_sequence.json

# 失败时继续执行
piper-cli run --script move_sequence.json --continue-on-error

# 指定接口
piper-cli run --script move_sequence.json --interface can0
```

脚本格式（JSON）：
```json
{
  "name": "测试脚本",
  "description": "测试脚本描述",
  "commands": [
    {
      "type": "Home"
    },
    {
      "type": "Move",
      "joints": [0.1, 0.2, 0.3, 0.4, 0.5, 0.6],
      "force": false
    },
    {
      "type": "Wait",
      "duration_ms": 1000
    },
    {
      "type": "Position"
    }
  ]
}
```

### replay - 回放录制

回放之前录制的 CAN 总线数据：

```bash
# 正常速度回放
piper-cli replay --input recording.bin

# 2 倍速回放
piper-cli replay --input recording.bin --speed 2.0

# 回放前确认
piper-cli replay --input recording.bin --confirm

# 指定接口
piper-cli replay --input recording.bin --interface can0
```

## 安全特性

### 移动确认

大幅移动（> 10°）会要求用户确认：

```bash
$ piper-cli move --joints 1.0,1.0,1.0
⚠️  大幅移动检测（最大角度: 57.3°）
确定要继续吗？[y/N]: y
✅ 移动完成
```

### E-Stop（急停）

**Linux (SocketCAN)**：
```bash
# Terminal 1
piper-cli move --joints ...

# Terminal 2
piper-cli stop  ✅ 可用
```

**Windows/macOS (GS-USB)**：
```bash
# 推荐使用 REPL 模式
$ piper-cli shell
piper> move --joints ...
[按 Ctrl+C 进行急停]  ✅ 唯一可靠方式
```

## 配置文件

配置文件位置：
- Linux/macOS: `~/.config/piper/config.toml`
- Windows: `%APPDATA%\piper\config.toml`

示例配置：
```toml
[default]
interface = "can0"
serial = "0001:1234"
```

## 依赖

- `piper-sdk` - Piper SDK 完整功能（驱动层和客户端层）
- `piper-client` - Piper SDK 客户端
- `piper-tools` - 共享数据结构（features = ["full"]）
- `clap` - 命令行解析
- `rustyline` - REPL 支持
- `tokio` - 异步运行时

## 架构

### 双模式设计

**One-shot 模式**（推荐用于 CI/脚本）：
- 每个命令独立连接
- 自动管理连接生命周期
- 适合自动化脚本

**REPL 模式**（推荐用于调试）：
- 保持连接常驻
- 历史记录支持（上下箭头）
- Ctrl+C 急停响应
- 错误隔离（不会因用户错误崩溃）

### 方案 B：专用输入线程

REPL 模式使用专用输入线程 + mpsc 通道：
- ✅ 保留历史记录
- ✅ 不阻塞 tokio
- ✅ Ctrl+C 及时响应

## 开发

```bash
# 编译
cargo build --release --bin piper-cli

# 运行
cargo run --bin piper-cli -- --help

# 测试
cargo test --package piper-cli

# 检查编译
cargo check --package piper-cli --all-targets
```

### 测试覆盖

当前测试覆盖：
- ✅ 单元测试：15 个测试用例
  - move 命令：关节解析、验证
  - position 命令：格式选项
  - stop 命令：参数创建
  - record 命令：参数创建
  - run 命令：参数创建
  - replay 命令：参数创建、速度控制
  - script 系统：JSON 序列化/反序列化
  - safety：位置检查、确认逻辑

## 许可证

MIT OR Apache-2.0
