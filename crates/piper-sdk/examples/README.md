# 示例代码

本目录包含 Piper SDK 的使用示例。

## 📋 可用示例

### 核心示例

- **`state_api_demo`** - 状态 API 使用演示
  - 一次性读取所有机器人状态
  - 展示 `Observation` 读取、按需查询、诊断快照/订阅和 observation metrics
  - 未查询的配置状态会明确显示为 `Unavailable`，不会打印伪造的零值
  - 适合学习 API 用法和作为参考文档
  - 支持 `--interface` / `--baud-rate`
  - Linux 默认 `can0`；macOS/Windows 需要显式传 GS-USB serial
  - 运行：`cargo run -p piper-sdk --example state_api_demo -- --interface can0`

- **`robot_monitor`** - 机器人实时监控工具
  - 持续循环读取状态（1Hz 刷新频率）
  - 显示关节位置、速度、电流等实时数据
  - 包含中文状态转换函数，便于理解
  - 支持 Ctrl+C 优雅退出
  - 实时 backend 仅支持 SocketCAN（Linux）和 GS-USB direct（跨平台）
  - 运行：`cargo run -p piper-sdk --example robot_monitor`

### 工具示例

- **`standard_recording`** - 标准录制 API 示例
  - 通过 `RecordingConfig` 录制 CAN 帧到文件
  - 支持 `--interface` / `--output` / `--duration`
  - Linux 默认 `can0`；macOS/Windows 需要显式传 GS-USB serial
  - 运行：`cargo run -p piper-sdk --example standard_recording -- --interface can0`

- **`custom_diagnostics`** - 自定义诊断回调示例
  - 展示如何注册原始 frame callback 并在后台线程分析 CAN 流
  - 支持 `--interface` / `--seconds`
  - Linux 默认 `can0`；macOS/Windows 需要显式传 GS-USB serial
  - 运行：`cargo run -p piper-sdk --example custom_diagnostics -- --interface can0`

- **`replay_mode`** - ReplayMode API 示例
  - 安全进入 replay 模式并按指定速度回放录制文件
  - 支持 `--interface` / `--recording-file` / `--speed`
  - Linux 默认 `can0`；macOS/Windows 需要显式传 GS-USB serial
  - 运行：`cargo run -p piper-sdk --example replay_mode -- --interface can0`

- **`timestamp_verification`** - SocketCAN 硬件时间戳验证程序
  - 验证 Linux SocketCAN 是否支持硬件时间戳（SO_TIMESTAMPING）
  - 支持 `--interface`
  - 仅支持 Linux；在其他平台会直接以失败退出
  - 运行：`cargo run -p piper-sdk --example timestamp_verification -- --interface vcan0`

- **`realtime_control_demo`** - 驱动层调度 / 指标演示
  - 展示 realtime queue、reliable queue 和线程健康指标
  - 默认不会向总线注入 raw frame
  - 只有传 `--allow-raw-tx` 才会在隔离总线上发送 demo 帧
  - Linux 默认 `can0`；macOS/Windows 需要显式传 GS-USB serial
  - 运行：`cargo run -p piper-sdk --example realtime_control_demo -- --interface vcan0 --allow-raw-tx`

- **`bridge_test`** - controller-owned bridge 调试工具
  - 用于 bridge/debug/replay 场景
  - 基于 UDS/TCP-TLS stream，会话由 logical session token 管理
  - 明确是非实时链路，不用于 MIT / 双臂 / fault-stop 主控制链
  - Unix 默认连 `/tmp/piper_bridge.sock`
  - 非 Unix 平台必须显式传 `--endpoint`
  - TCP/TLS endpoint 需要同时传 `--tls-ca` / `--tls-client-cert` / `--tls-client-key` / `--tls-server-name`
  - 运行（Unix UDS）：`cargo run -p piper-sdk --example bridge_test`
  - 运行（TCP/TLS）：`cargo run -p piper-sdk --example bridge_test -- --endpoint 127.0.0.1:18888 --tls-ca ca.pem --tls-client-cert client.pem --tls-client-key client.key --tls-server-name bridge.local`

- **`bridge_latency_bench`** - bridge 链路延迟基准
  - 评估非实时 bridge/debug 链路的 host-side request/event 开销
  - Unix 默认连 `/tmp/piper_bridge.sock`
  - 非 Unix 平台必须显式传 `--endpoint`
  - TCP/TLS endpoint 需要同时传 `--tls-ca` / `--tls-client-cert` / `--tls-client-key` / `--tls-server-name`
  - 运行（Unix UDS）：`cargo run -p piper-sdk --example bridge_latency_bench`
  - 运行（TCP/TLS）：`cargo run -p piper-sdk --example bridge_latency_bench -- --endpoint 127.0.0.1:18888 --tls-ca ca.pem --tls-client-cert client.pem --tls-client-key client.key --tls-server-name bridge.local`

- **`embedded_bridge_host`** - 控制进程内嵌 bridge host 示例
  - 先创建控制进程，再把非实时 bridge host attach 到控制面
  - 默认不开 raw frame tap，只有客户端显式订阅时才启用
  - Linux 默认连接 `socketcan:can0`；其他平台默认自动扫描 GS-USB
  - Unix 默认启用 UDS listener
  - 非 Unix 平台必须显式传 `--tcp-tls`，并同时提供 `--tls-server-cert` / `--tls-server-key` / `--tls-client-ca`
  - 运行（Unix UDS）：`cargo run -p piper-sdk --example embedded_bridge_host`
  - 运行（TCP/TLS）：`cargo run -p piper-sdk --example embedded_bridge_host -- --tcp-tls 127.0.0.1:18888 --tls-server-cert server.pem --tls-server-key server.key --tls-client-ca ca.pem`

### 高层 API / 离线教学示例

- **`position_control_demo`** - 完整的位置模式控制流程
  - 从连接、使能、移动、回位到显式失能的全流程演示
  - 这是教学示例，使用固定等待来展示流程，不把 `sleep` 视为生产级到位确认
  - 生产代码请参考 `piper-control::workflow` 的按误差阈值阻塞确认模式
  - Linux 默认 `can0`；macOS/Windows 需要显式传 GS-USB serial
  - 运行：`cargo run -p piper-sdk --example position_control_demo -- --interface can0`

### 实用硬件辅助

HIL 手册请参考：[docs/v0/piper_hil_handbook.md](/home/viv/projs/piper-sdk-rs/docs/v0/piper_hil_handbook.md)
这两个 HIL 工具是手册支持入口，不是完整替代人工验收流程。

- **`socketcan_raw_clock_probe`** - Linux SocketCAN only，只读双接口 raw timestamp calibration probe
  - 运行：`cargo run -p piper-sdk --example socketcan_raw_clock_probe -- --left-interface can0 --right-interface can1 --duration-secs 300 --out artifacts/teleop/raw-clock-probe.json`

- **`client_monitor_hil_check`** - 只读客户端监控 HIL 辅助工具
  - 验证连接时间、首个完整 monitor snapshot 和只读观测窗口
  - 主要用于手册中的 Phase 1
  - 运行：`cargo run -p piper-sdk --example client_monitor_hil_check -- --interface can0 --baud-rate 1000000 --observation-window-secs 900`

- **`hil_joint_position_check`** - 安全关节位置 HIL 辅助工具
  - 验证 `PositionMode + MotionType::Joint` 的低风险位置控制路径
  - 主要用于手册中的 Phase 2 / Phase 3
  - 运行：`cargo run -p piper-sdk --example hil_joint_position_check -- --interface can0 --baud-rate 1000000 --joint 1 --delta-rad 0.02 --speed-percent 10`

### 其他实用示例

- **`multi_threaded_demo`** - 多线程共享 Piper 的示例
  - 展示 `Arc<Mutex<Piper>>`、监控线程和显式 disable 收尾
  - Linux 默认 `can0`；macOS/Windows 需要显式传 GS-USB serial
  - 运行：`cargo run -p piper-sdk --example multi_threaded_demo -- --interface can0`

- **`dual_arm_bilateral_control`** - 双臂主从 / 镜像控制示例
  - 用于双臂桥接与双向跟随实验
  - 运行：`cargo run -p piper-sdk --example dual_arm_bilateral_control -- --left-interface can0 --right-interface can1`

- **`high_level_simple_move`** - 轨迹规划快速入门
  - 离线展示 `TrajectoryPlanner` 和位置命令发送模式
  - 运行：`cargo run -p piper-sdk --example high_level_simple_move`

- **`high_level_trajectory_demo`** - 轨迹规划器高级特性
  - 展示重置、平滑性分析、边界条件验证
  - 运行：`cargo run -p piper-sdk --example high_level_trajectory_demo`

- **`high_level_pid_control`** - PID 控制器离线演示
  - 展示闭环控制器参数和更新模式
  - 运行：`cargo run -p piper-sdk --example high_level_pid_control`

- **`high_level_gripper_control`** - 夹爪高层 API 教学示例
  - 离线展示 `open_gripper/close_gripper/set_gripper`
  - 运行：`cargo run -p piper-sdk --example high_level_gripper_control`

- **`frame_dump`** - PiperFrame 序列化 / 转储示例
  - 展示 JSON 序列化、保存和加载
  - 运行：`cargo run -p piper-sdk --example frame_dump --features serde`

- **`iface_check`** - SocketCAN 接口状态检查工具
  - 用于检查接口是否 up / bitrate 是否匹配
  - 运行：`cargo run -p piper-sdk --example iface_check -- can0`

- **`gs_usb_direct_test`** - GS-USB 直连探测示例
  - 用于调试底层 GS-USB 枚举和收发
  - 运行：`cargo run -p piper-sdk --example gs_usb_direct_test`

### 计划中的示例

- `torque_control` - 力控演示
- `configure_can` - CAN 波特率配置工具

## 🚀 运行示例

```bash
# 运行状态 API 演示
cargo run -p piper-sdk --example state_api_demo -- --interface can0

# 运行标准录制示例
cargo run -p piper-sdk --example standard_recording -- --interface can0

# 运行实时监控工具
cargo run -p piper-sdk --example robot_monitor

# 运行时间戳验证程序
cargo run -p piper-sdk --example timestamp_verification -- --interface vcan0

# 在隔离总线上运行 raw TX 调度演示
cargo run -p piper-sdk --example realtime_control_demo -- --interface vcan0 --allow-raw-tx
```

## 📝 示例说明

### `state_api_demo` vs `robot_monitor`

- **`state_api_demo`**：适合学习 API 用法，一次性展示所有功能
- **`robot_monitor`**：适合实际监控场景，持续显示实时数据

两者功能有重叠，但定位不同：
- `state_api_demo` 更注重 API 的完整性和教学性
- `robot_monitor` 更注重实用性和实时性

## ⚠️ 注意事项

- 部分示例需要连接硬件设备
- `realtime_control_demo --allow-raw-tx` 只应在隔离总线（如 `vcan0`）上运行
- 请确保具有适当的系统权限（USB 设备访问权限等）
- 详细说明请参考各示例文件中的注释

---

**注意**：示例代码正在积极开发中。如有问题或建议，欢迎提交 Issue 或 PR。
