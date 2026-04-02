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

- `auto-strict`
- `auto-any`
- `socketcan:can0`
- `gs-usb-auto`
- `gs-usb-serial:ABC123`
- `gs-usb-bus-address:1:8`

默认值是 `auto-strict`。

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

# JSON 输出
piper-cli position --format json

# 回到零关节位
piper-cli home

# 前往安全停靠位并在完成后 disable
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
- REPL 的 `park` 只会移动到当前配置的 `park_pose()`，不会自动 disable
- 运行期间主线程仍能处理 `stop` 和 `Ctrl+C`
- 当前运动会被取消，然后统一执行 `disable_all()`
- 连接会保留在 `Standby`
- shell 中不做交互式确认；需要确认的 `move` / `set-zero` 必须显式加 `--force`
- 若需要交互确认，请使用 one-shot CLI
- `Ctrl+D` 会直接退出 shell；若当前命令仍在运行，会先请求急停并在命令收尾后退出

## 配置文件

配置文件位置：

- Linux/macOS: `~/.config/piper/config.toml`
- Windows: `%APPDATA%\\piper\\config.toml`

示例：

```toml
[target]
kind = "auto-strict"

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
- REPL 中若触发大幅移动保护，必须显式加 `--force`

### `home`

- 固定发送零关节目标 `[0.0; 6]`
- 会阻塞等待达到阈值或超时

### `park`

- one-shot CLI `park` 和脚本里的 `Park` 会先进入停靠流程，再 disable
- REPL 的 `park` 只前往配置中的安全停靠位，不会自动 disable
- 默认由 `orientation` 决定
- 若配置了 `rest_pose_override`，优先使用自定义停靠位

### `disable`

- 只发送 raw disable，不附带任何移动或停靠动作
- REPL 中的 `disable` 会保持连接，但把机器人回到失能状态

### `set-zero`

- 只允许在 `Standby` 状态执行
- 作用是“把当前位置写成零点”，不会先移动
- REPL 中必须显式加 `--force`；若需要交互确认，请使用 one-shot CLI

### `collision-protection get`

- 这是主动查询，不是读取 observer 缓存
- 会发送 query，并等待本次 query 之后的新反馈
- 超时会报错，不会伪造 `[0; 6]`

### `collision-protection set`

- 会发送写入命令
- 然后发送主动 query，并等待这次 query 之后返回的确认结果
- 不会仅凭旧缓存或写入前的历史反馈判定成功

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

- SocketCAN：通常可作为外部 stop 路径
- 独占式 GS-USB：不应把跨进程 `stop` 当成 REPL 急停的替代方案

## 监控 / 录制 / 回放 / 脚本

```bash
# 监控
piper-cli monitor --frequency 20

# 录制
piper-cli record --output recording.bin --duration 10

# 接收到指定 CAN ID 后停止（优先级高于 --duration）
piper-cli record --output recording.bin --duration 30 --stop-on-id 0x2A4

# 回放
piper-cli replay --input recording.bin --speed 2.0

# 跳过确认提示
piper-cli replay --input recording.bin --speed 2.0 --yes

# 执行脚本
piper-cli run --script examples/move_sequence.json
```

脚本中的 `move` / `home` / `park` / `set-zero` 与 CLI one-shot 共享同一套控制 workflow。
其中 `Park` 会走与 one-shot `piper-cli park` 相同的 standby-entry park 流程，然后再 disable。

## 开发

```bash
cargo check -p piper-cli --all-targets
cargo test -p piper-cli
cargo clippy -p piper-cli --all-targets --all-features -- -D warnings
```
