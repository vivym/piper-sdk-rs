# 机械臂反馈接收示例实现方案

## 1. 目标

编写一个示例程序，用于：
- 连接到松灵 Piper 机械臂
- **不发送任何控制指令**，仅被动监听
- 接收并显示机械臂的反馈信息（状态、位置、速度等）

## 2. 库架构概述

### 2.1 核心组件

- **`PiperBuilder`**: 使用 Builder 模式创建 `Piper` 实例
- **`Piper`**: 对外 API，提供状态读取方法
- **IO 线程**: 后台自动运行的线程，负责 CAN 帧的接收和解析
- **状态同步**: 使用 `ArcSwap` 实现无锁状态共享，支持高频读取

### 2.2 反馈信息类型

根据协议文档 (`docs/v0/protocol.md`)，机械臂会主动发送以下反馈：

1. **机械臂状态反馈** (ID: 0x2A1-0x2A8)
   - `0x2A1`: 控制模式、机器人状态、运动状态等
   - `0x2A2-0x2A4`: 末端位姿 (X, Y, Z, Rx, Ry, Rz)
   - `0x2A5-0x2A7`: 关节角度 (J1-J6)
   - `0x2A8`: 夹爪状态

2. **关节驱动器信息反馈** (ID: 0x251-0x256 / 0x261-0x266)
   - `0x251-0x256`: 高速反馈（20ms 周期）- 转速、电流、位置
   - `0x261-0x266`: 低速反馈（100ms 周期）- 电压、温度、状态

### 2.3 状态访问方法

`Piper` 提供以下无锁读取方法：

- `get_core_motion()`: 核心运动状态（关节位置 + 末端位姿）
- `get_joint_dynamic()`: 关节动态状态（速度 + 电流）
- `get_control_status()`: 控制状态（模式、状态码、夹爪）
- `get_diagnostic_state()`: 诊断状态（温度、电压、保护等级）
- `get_config_state()`: 配置状态（参数查询结果）

## 3. 实现方案

### 3.1 程序流程

```
1. 初始化 (PiperBuilder)
   ↓
2. 连接到机械臂 (build())
   ↓
3. 等待反馈信息
   ↓
3. 循环读取状态 (get_* 方法)
   ↓
4. 格式化并打印反馈信息
   ↓
5. 按 Ctrl+C 优雅退出
```

### 3.2 代码结构

```rust
use piper_sdk::robot::PiperBuilder;
use std::time::Duration;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. 创建 Piper 实例（使用默认配置）
    let piper = PiperBuilder::new()
        .baud_rate(1_000_000)  // CAN 波特率 1M (协议要求)
        .build()?;

    println!("✓ 已连接到机械臂");
    println!("正在监听反馈信息...\n");

    // 2. 等待初始反馈（可选，给设备一点时间建立连接）
    std::thread::sleep(Duration::from_millis(100));

    // 3. 主循环：定期读取并打印反馈
    loop {
        // 读取各种状态
        let core_motion = piper.get_core_motion();
        let joint_dynamic = piper.get_joint_dynamic();
        let control_status = piper.get_control_status();

        // 打印反馈信息
        print_feedback(&core_motion, &joint_dynamic, &control_status);

        // 控制刷新频率（1Hz，每秒打印一次）
        std::thread::sleep(Duration::from_secs(1));
    }
}
```

### 3.3 反馈信息显示

#### 3.3.1 核心运动状态

```rust
// 关节位置（弧度 -> 度）
println!("关节角度: J1={:.2}° J2={:.2}° ...", ...);

// 末端位姿（米）
println!("末端位置: X={:.3}m Y={:.3}m Z={:.3}m", ...);
println!("末端姿态: Rx={:.3} Ry={:.3} Rz={:.3}", ...);
```

#### 3.3.2 关节动态状态

```rust
// 关节速度（rad/s）
println!("关节速度: J1={:.3} rad/s J2={:.3} rad/s ...", ...);

// 关节电流（A）
println!("关节电流: J1={:.3} A J2={:.3} A ...", ...);

// 有效性检查
if joint_dynamic.is_complete() {
    println!("✓ 所有关节数据完整");
} else {
    println!("⚠ 缺失关节: {:?}", joint_dynamic.missing_joints());
}
```

#### 3.3.3 控制状态

```rust
// 控制模式
println!("控制模式: {}", mode_to_string(control_status.control_mode));

// 机器人状态
println!("机器人状态: {}", status_to_string(control_status.robot_status));

// 运动状态
println!("运动状态: {}", ...);

// 夹爪状态
println!("夹爪行程: {:.2} mm, 扭矩: {:.3} N·m", ...);
```

#### 3.3.4 诊断状态（可选，低频读取）

```rust
// 每 1 秒读取一次诊断信息
if let Ok(diag) = piper.get_diagnostic_state() {
    println!("温度: 电机={:.1}°C 驱动器={:.1}°C", ...);
    println!("电压: {:.1} V", ...);
}
```

### 3.4 错误处理

```rust
// 连接失败
match PiperBuilder::new().build() {
    Ok(piper) => { /* ... */ }
    Err(e) => {
        eprintln!("❌ 连接失败: {}", e);
        return Err(e.into());
    }
}

// 诊断状态读取失败（RwLock 可能被污染）
match piper.get_diagnostic_state() {
    Ok(diag) => { /* ... */ }
    Err(e) => {
        eprintln!("⚠ 无法读取诊断状态: {}", e);
    }
}
```

### 3.5 优雅退出

```rust
use ctrlc;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 设置 Ctrl+C 处理
    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();

    ctrlc::set_handler(move || {
        r.store(false, Ordering::SeqCst);
        println!("\n收到退出信号，正在关闭...");
    })?;

    // 主循环中检查标志
    while running.load(Ordering::SeqCst) {
        // ... 读取和打印反馈 ...
    }

    println!("✓ 已关闭");
    Ok(())
}
```

## 4. 预期行为

### 4.1 正常情况

1. **连接成功**: 程序启动后立即显示 "✓ 已连接到机械臂"
2. **接收反馈**: 如果机械臂已上电，应该在 100-200ms 内开始接收到反馈信息
3. **数据更新**: 每 1 秒打印一次状态，显示：
   - 关节角度（6 个关节）
   - 末端位姿（位置和姿态）
   - 关节速度（如果机械臂在运动）
   - 控制状态（模式、状态码）

### 4.2 异常情况

1. **设备未连接**: `build()` 返回 `RobotError::Can(CanError::Device(...))`
   - 提示用户检查 USB 连接或 GS-USB 设备

2. **设备未上电**: 可以连接成功，但不会收到反馈
   - 状态会保持默认值（例如关节角度全为 0）
   - 可以显示提示信息："等待机械臂反馈..."

3. **部分关节丢帧**: `joint_dynamic.valid_mask` 不全为 1
   - 显示警告信息，列出缺失的关节

4. **无反馈超时**: 长时间没有接收到有效数据
   - 可以添加超时检测，提示用户检查 CAN 总线连接

## 5. 实现要点

### 5.1 不需要发送指令

- **仅使用读取方法**: 只调用 `get_*()` 方法，不调用 `send_frame()`
- **IO 线程自动工作**: 后台线程会自动接收 CAN 帧并更新状态
- **无需显式启动**: `PiperBuilder::build()` 会自动启动 IO 线程

### 5.2 状态读取性能

- **无锁设计**: `get_core_motion()` 等方法使用 `ArcSwap::load()`，纳秒级返回
- **快照机制**: 返回的是状态快照的副本（Clone），不影响后台更新
- **适合高频**: 可以在 1kHz 控制循环中使用（本示例使用 1Hz 用于显示）

### 5.3 时间戳说明

**重要**: 状态中的 `timestamp_us` 是**硬件时间戳**（相对时间），不是 UNIX 时间戳。

- 用于计算帧间时间差（例如两次读取的时间差）
- 不能直接与系统时间戳比较
- 如果需要系统时间戳，使用 `std::time::Instant::now()`

### 5.4 配置选项

```rust
// 可以自定义 Pipeline 配置
let config = PipelineConfig {
    receive_timeout_ms: 5,           // CAN 帧接收超时
    frame_group_timeout_ms: 20,      // 帧组超时（Buffered Commit）
};

let piper = PiperBuilder::new()
    .baud_rate(1_000_000)
    .pipeline_config(config)
    .build()?;
```

## 6. 文件组织

建议的文件结构：

```
examples/
├── README.md
└── feedback_receiver.rs          # 主示例文件
```

## 7. 依赖项

需要在 `Cargo.toml` 中添加（如果还未添加）：

```toml
[dependencies]
piper-sdk = { path = "../" }     # 或使用已发布的版本
ctrlc = "3.4"                    # 用于 Ctrl+C 处理（可选）
```

## 8. 运行方式

```bash
# 运行示例
cargo run --example feedback_receiver

# 编译但不运行
cargo build --example feedback_receiver

# 发布模式编译（优化）
cargo build --example feedback_receiver --release
```

## 9. 下一步

实现此示例后，可以进一步扩展：

1. **保存日志**: 将反馈信息保存到文件（CSV 格式）
2. **数据可视化**: 使用 `plotters` 库实时绘制关节角度曲线
3. **状态告警**: 检测异常状态（例如关节超限、通信错误）并发出警告
4. **性能统计**: 统计帧接收率、丢帧率等指标
5. **交互式命令**: 添加简单的命令行界面，允许用户切换显示模式

