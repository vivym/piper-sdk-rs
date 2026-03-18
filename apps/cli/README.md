# Piper CLI

`piper-cli` 是 Piper 机械臂的命令行控制工具，提供：

- one-shot 命令：适合 CI、批处理、脚本
- REPL shell：适合人工调试和同进程急停
- 录制 / 回放 / 监控 / 脚本执行

## 安装

```bash
cargo install --path apps/cli
```

## 连接目标

CLI 和配置文件统一使用强类型 target spec：

- `auto`
- `socketcan:can0`
- `gs-usb-auto`
- `gs-usb-serial:ABC123`
- `gs-usb-bus-address:1:8`
- `daemon-udp:127.0.0.1:18888`
- `daemon-uds:/tmp/gs_usb.sock`

默认值是 `auto`。

## 快速开始

### 配置默认 target

```bash
piper-cli config set-target socketcan:can0
piper-cli config check
```

### One-shot 模式

```bash
# 移动到目标位置（1~6 个值依次映射到 J1..Jn，剩余关节保持当前位置）
piper-cli move --joints 0.1,0.2,0.3

# 查询当前位置
piper-cli position

# 回到零关节位
piper-cli home

# 前往安全停靠位
piper-cli park

# 写入零点
piper-cli set-zero --joints 1,2,3 --force

# 主动查询碰撞保护等级
piper-cli collision-protection get

# 设置并校验碰撞保护等级
piper-cli collision-protection set --level 5
```

### REPL 模式

```bash
$ piper-cli shell
piper> connect socketcan:can0
piper> enable
piper> move --joints 0.1,0.2,0.3
piper> stop
piper> exit
```

REPL 现在使用“前台输入 + 后台命令 worker”模型：

- `move` / `home` / `park` 在后台执行
- 运行期间主线程仍能处理 `stop` 和 `Ctrl+C`
- 当前运动会被取消，然后统一执行 `disable_all()`
- 连接会保留在 `Standby`

## 配置文件

配置文件位置：

- Linux/macOS: `~/.config/piper/config.toml`
- Windows: `%APPDATA%\\piper\\config.toml`

示例：

```toml
[target]
kind = "auto"

[park]
orientation = "upright"
rest_pose_override = [0.0, 0.0, 0.0, 0.02, 0.5, 0.0]

[safety]
confirm_large_motion = true
confirmation_threshold_deg = 10.0

[motion]
threshold_rad = 0.02
poll_interval_ms = 50
republish_interval_ms = 200
timeout_ms = 5000
```

旧版 `interface/serial` 配置已经废弃；未发布阶段不做兼容迁移，检测到旧 schema 会直接报错。

## 命令语义

### `move`

- 输入 1~6 个关节值，依次映射到 `J1..Jn`
- 未指定的关节保持当前位姿
- 大幅移动确认基于“当前位姿 -> 有效目标”的实际位移，而不是目标绝对值
- one-shot / REPL / script 共享同一套 workflow

### `home`

- 固定发送零关节目标 `[0.0; 6]`
- 会阻塞等待达到阈值或超时

### `park`

- 前往配置中的安全停靠位
- 默认由 `orientation` 决定
- 若配置了 `rest_pose_override`，优先使用自定义停靠位

### `set-zero`

- 只允许在 `Standby` 状态执行
- 作用是“把当前位置写成零点”，不会先移动

### `collision-protection get`

- 这是主动查询，不是读取 observer 缓存
- 会发送 query，并等待本次 query 之后的新反馈
- 超时会报错，不会伪造 `[0; 6]`

### `collision-protection set`

- 会发送写入命令
- 然后按“post-write 新状态”做校验
- 只要观察到写入后的新缓存与目标一致，就算成功

## 急停语义

### REPL 内急停

`stop` 和 `Ctrl+C` 统一执行：

1. 取消当前 `move/home/park`（如果正在运行）
2. 发送 `disable_all()`
3. 保持连接
4. 最终回到 `Standby`

这是推荐的交互式急停路径。

### 外部 one-shot `piper-cli stop`

```bash
piper-cli stop --target socketcan:can0
```

它会新建一个 one-shot 连接并发送 `disable_all()`。  
是否可靠取决于底层后端是否允许并发访问：

- SocketCAN / daemon：通常可作为外部 stop 路径
- 独占式 GS-USB：不应把跨进程 `stop` 当成 REPL 急停的替代方案

## 监控 / 录制 / 回放 / 脚本

```bash
# 监控
piper-cli monitor --frequency 20

# 录制
piper-cli record --output recording.bin --duration 10

# 回放
piper-cli replay --input recording.bin --speed 2.0

# 执行脚本
piper-cli run --script examples/move_sequence.json
```

脚本中的 `move` / `home` / `park` / `set-zero` 与 CLI one-shot 共享同一套控制 workflow。

## 开发

```bash
cargo check -p piper-cli --all-targets
cargo test -p piper-cli
cargo clippy -p piper-cli --all-targets --all-features -- -D warnings
```
