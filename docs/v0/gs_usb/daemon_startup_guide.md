# GS-USB 守护进程启动指南

## 快速开始

### 1. 编译守护进程

```bash
cd /Users/viv/projs/piper-sdk-rs
cargo build --release --bin gs_usb_daemon
```

编译后的二进制文件位于：`target/release/gs_usb_daemon`

### 2. 启动守护进程

#### 方式 1: 使用默认配置（最简单）

```bash
# 使用默认配置启动
./target/release/gs_usb_daemon
```

**默认配置**：
- UDS Socket: `/tmp/gs_usb_daemon.sock`
- UDP Socket: 未启用
- CAN 波特率: 1,000,000 bps
- 锁文件: `/var/run/gs_usb_daemon.lock`
- 重连间隔: 1 秒
- 重连去抖动: 500ms
- 客户端超时: 30 秒

#### 方式 2: 使用命令行参数

```bash
# 查看所有可用参数
./target/release/gs_usb_daemon --help

# 自定义 UDS 路径和波特率
./target/release/gs_usb_daemon \
    --uds /tmp/my_gs_usb_daemon.sock \
    --bitrate 500000

# 启用 UDP 支持（用于跨机器调试）
./target/release/gs_usb_daemon \
    --uds /tmp/gs_usb_daemon.sock \
    --udp 127.0.0.1:8888

# 指定设备序列号（多设备场景）
./target/release/gs_usb_daemon \
    --serial ABC123456

# 完整配置示例
./target/release/gs_usb_daemon \
    --uds /tmp/gs_usb_daemon.sock \
    --udp 127.0.0.1:8888 \
    --bitrate 1000000 \
    --serial ABC123456 \
    --reconnect-interval 2 \
    --reconnect-debounce 500 \
    --client-timeout 60 \
    --lock-file /var/run/gs_usb_daemon.lock
```

**命令行参数说明**：

| 参数 | 说明 | 默认值 |
|------|------|--------|
| `--uds <PATH>` | UDS Socket 路径 | `/tmp/gs_usb_daemon.sock` |
| `--udp <ADDR>` | UDP 监听地址（可选） | 未启用 |
| `--bitrate <RATE>` | CAN 波特率（bps） | `1000000` |
| `--serial <SERIAL>` | 设备序列号（可选） | 自动检测 |
| `--lock-file <PATH>` | 锁文件路径 | `/var/run/gs_usb_daemon.lock` |
| `--reconnect-interval <SEC>` | 重连间隔（秒） | `1` |
| `--reconnect-debounce <MS>` | 重连去抖动时间（毫秒） | `500` |
| `--client-timeout <SEC>` | 客户端超时时间（秒） | `30` |

### 3. 验证守护进程运行

```bash
# 检查进程是否运行
ps aux | grep gs_usb_daemon

# 检查 UDS Socket 是否创建
ls -l /tmp/gs_usb_daemon.sock

# 检查锁文件
cat /var/run/gs_usb_daemon.lock
```

### 4. 停止守护进程

```bash
# 方式 1: 发送 Ctrl+C（如果在前台运行）
# 方式 2: 查找并终止进程
pkill gs_usb_daemon

# 方式 3: 使用锁文件中的 PID
PID=$(cat /var/run/gs_usb_daemon.lock | head -1)
kill $PID
```

## 后台运行

### 方式 1: 使用 nohup

```bash
nohup ./target/release/gs_usb_daemon > /tmp/gs_usb_daemon.log 2>&1 &
```

### 方式 2: 使用 macOS launchd（推荐）

创建 `~/Library/LaunchAgents/com.piper.gs_usb_daemon.plist`：

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.piper.gs_usb_daemon</string>
    <key>ProgramArguments</key>
    <array>
        <string>/Users/viv/projs/piper-sdk-rs/target/release/gs_usb_daemon</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardOutPath</key>
    <string>/tmp/gs_usb_daemon.log</string>
    <key>StandardErrorPath</key>
    <string>/tmp/gs_usb_daemon.error.log</string>
</dict>
</plist>
```

加载服务：

```bash
launchctl load ~/Library/LaunchAgents/com.piper.gs_usb_daemon.plist
```

卸载服务：

```bash
launchctl unload ~/Library/LaunchAgents/com.piper.gs_usb_daemon.plist
```

## 配置选项

守护进程支持通过命令行参数进行配置，无需修改代码。

### 默认配置

- **UDS 路径**: `/tmp/gs_usb_daemon.sock`
- **UDP 地址**: 未启用（`None`）
- **CAN 波特率**: 1,000,000 bps
- **设备序列号**: 自动检测第一个设备
- **重连间隔**: 1 秒
- **重连去抖动**: 500ms
- **客户端超时**: 30 秒

### 常用配置示例

#### 示例 1: 仅使用 UDS（推荐，延迟最低）

```bash
./target/release/gs_usb_daemon --uds /tmp/gs_usb_daemon.sock
```

#### 示例 2: 启用 UDP（用于跨机器调试）

```bash
./target/release/gs_usb_daemon \
    --uds /tmp/gs_usb_daemon.sock \
    --udp 127.0.0.1:8888
```

#### 示例 3: 自定义波特率

```bash
./target/release/gs_usb_daemon --bitrate 500000
```

#### 示例 4: 指定设备序列号

```bash
./target/release/gs_usb_daemon --serial ABC123456
```

## 使用客户端连接

### 客户端 ID 分配（自动模式）

**重要更新**：所有客户端（UDS 和 UDP）现在统一使用**自动 ID 分配**模式：

- ✅ **自动分配**：客户端发送 `client_id = 0` 请求守护进程自动分配唯一 ID
- ✅ **无冲突**：守护进程保证分配的 ID 唯一性，特别适合 UDP 跨网络场景
- ✅ **向后兼容**：仍支持手动指定 ID（不推荐）

客户端会自动从 `ConnectAck` 获取守护进程分配的 ID，无需手动管理。

### 使用 UDS（推荐，延迟最低）

```rust
use piper_sdk::robot::PiperBuilder;

let robot = PiperBuilder::new()
    .with_daemon("/tmp/gs_usb_daemon.sock")  // UDS 路径
    .build()
    .unwrap();
```

**注意**：客户端会自动请求 ID 分配，无需手动指定。

### 使用 UDP（用于跨机器调试）

```rust
use piper_sdk::robot::PiperBuilder;

let robot = PiperBuilder::new()
    .with_daemon("127.0.0.1:8888")  // UDP 地址
    .build()
    .unwrap();
```

**注意**：
- UDP 场景下**必须**使用自动 ID 分配（跨网络，进程 ID 可能冲突）
- 客户端自动处理 ID 分配，无需手动指定

## 故障排除

### 问题 1: 无法获取锁文件

```
Failed to acquire singleton lock: ...
Another instance of gs_usb_daemon may be running.
```

**解决方案**：
1. 检查是否有其他实例在运行：`ps aux | grep gs_usb_daemon`
2. 如果确认没有运行，删除锁文件：`sudo rm /var/run/gs_usb_daemon.lock`
3. 重新启动守护进程

### 问题 2: UDS Socket 创建失败

```
Socket init error: Failed to bind UDS socket: ...
```

**解决方案**：
1. 检查路径权限：`ls -l /tmp/gs_usb_daemon.sock`
2. 删除旧的 Socket 文件：`rm /tmp/gs_usb_daemon.sock`
3. 确保 `/tmp` 目录可写

### 问题 3: 设备连接失败

```
Device init error: No GS-USB device found
```

**解决方案**：
1. 检查 USB 设备是否连接：`system_profiler SPUSBDataType | grep -i "gs\|can"`
2. 检查设备权限（macOS 可能需要授权）
3. 尝试重新插拔 USB 设备

### 问题 4: 客户端无法连接

**检查清单**：
1. 守护进程是否运行：`ps aux | grep gs_usb_daemon`
2. UDS Socket 是否存在：`ls -l /tmp/gs_usb_daemon.sock`
3. Socket 权限是否正确
4. 查看守护进程日志

## 日志

当前实现将日志输出到标准输出/错误。后台运行时建议重定向到文件：

```bash
./target/release/gs_usb_daemon > /tmp/gs_usb_daemon.log 2>&1 &
```

查看日志：

```bash
tail -f /tmp/gs_usb_daemon.log
```

## 下一步

- ✅ 命令行参数支持（已完成）
- 添加配置文件支持（TOML/YAML）
- 添加更详细的日志输出（使用 tracing）
- 添加健康检查接口（GetStatus 消息）

