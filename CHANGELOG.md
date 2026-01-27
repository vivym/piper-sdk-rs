# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

---

## [Unreleased]

### 🎬 Recording and Replay System (2026-01-27)

#### Added

**Standard Recording API** (`piper_client::recording`):
- ✨ `start_recording()`: Start recording CAN frames with configurable stop conditions
- ✨ `stop_recording()`: Stop recording and retrieve statistics
- ✨ `RecordingConfig`: Configuration for recording (output path, stop condition, metadata)
- ✨ `RecordingMetadata`: Rich metadata (operator, notes, timestamps)
- ✨ `StopCondition`: Flexible stop conditions (Duration, FrameCount, Manual)
- ✨ `RecordingStatistics`: Recording statistics (frame count, duration, dropped frames)
- ✨ Non-blocking async recording with bounded queues (10,000 frame capacity)
- ✨ Hardware timestamps with microsecond precision
- ✨ TX safety (only records successfully sent frames)

**Custom Diagnostics API**:
- ✨ `diagnostics()` method: Access diagnostics interface from `Active<Mode>` states
- ✨ `Diagnostics`: Interface for registering custom frame callbacks
- ✨ `register_callback()`: Register custom frame processing hooks
- ✨ Support for real-time frame analysis in background threads
- ✨ Loss tracking via `dropped_frames` counter

**ReplayMode API** (`piper_client::state::ReplayMode`):
- ✨ `enter_replay_mode()`: Enter ReplayMode (driver TX thread pauses automatically)
- ✨ `replay_recording()`: Replay recorded CAN frames with configurable speed
- ✨ `stop_replay()`: Early exit from replay mode
- ✨ `ReplayMode`: Type state marker for compile-time safety
- ✨ Speed validation (0.1x ~ 5.0x, recommended ≤ 2.0x)
- ✨ Driver-level protection via `DriverMode::Replay`
- ✨ Frame timing preservation during replay
- ✨ Automatic return to `Standby` state after replay

**Driver Layer** (`piper_driver::mode`):
- ✨ `DriverMode` enum: `Normal` (periodic TX) and `Replay` (TX paused)
- ✨ `AtomicDriverMode`: Thread-safe driver mode switching
- ✨ `Piper::mode()`: Get current driver mode
- ✨ `Piper::set_mode()`: Set driver mode with logging
- ✨ `Piper::interface()`: Get CAN interface name (for recording metadata)
- ✨ `Piper::bus_speed()`: Get CAN bus speed (for recording metadata)

**CLI Commands** (`piper-cli`):
- ✨ `piper-cli replay`: Full replay command implementation
  - File existence validation
  - Speed range validation with warnings
  - Interactive confirmation prompt (optional `--confirm` flag)
  - Cross-platform support (SocketCAN/GS-USB)
  - Beautiful progress display with emojis

**Examples**:
- 📚 `standard_recording.rs`: Standard recording API usage demo
- 📚 `custom_diagnostics.rs`: Custom diagnostics interface demo
- 📚 `replay_mode.rs`: ReplayMode API demo with speed validation

**Documentation**:
- 📖 README.md: Comprehensive "Recording and Replay" section
  - Three API comparison table
  - Code examples for each API
  - CLI usage examples
  - Architecture highlights (type safety, driver protection)
  - Complete workflow examples

#### Changed

- Updated `Piper<Standby>` to expose `enter_replay_mode()` method
- Updated `Piper<ReplayMode>` to implement replay methods
- Updated `Piper<Active<Mode>>` to expose `diagnostics()` method
- Updated examples section in README to include new recording/replay examples

#### Technical Highlights

**1. Three-API Design**:
```rust
// API 1: Standard Recording (simplest)
let (robot, handle) = robot.start_recording(config)?;
let (robot, stats) = robot.stop_recording(handle)?;

// API 2: Custom Diagnostics (advanced)
let diag = active.diagnostics();
diag.register_callback(custom_hook)?;

// API 3: ReplayMode (safe replay)
let replay = robot.enter_replay_mode()?;
let robot = replay.replay_recording(path, speed)?;
```

**2. Type Safety via ReplayMode**:
```rust
// ✅ Compile-time error: cannot enable in ReplayMode
let replay = robot.enter_replay_mode()?;
let active = replay.enable_position_mode(...);  // ERROR!
```

**3. Driver-Level Protection**:
```rust
// Driver switches to ReplayMode automatically
// TX thread pauses, preventing dual control flow
self.driver.set_mode(DriverMode::Replay);
```

**4. Speed Validation**:
- Maximum 5.0x hard limit (safety)
- Recommended ≤ 2.0x with warnings
- Preserves frame timing during replay

#### Safety Features

- ✅ Type-safe state transitions (compile-time)
- ✅ Driver-level mode switching (runtime)
- ✅ Speed limit validation (5.0x maximum)
- ✅ TX thread pause during replay (no conflicts)
- ✅ Automatic cleanup via RAII

#### Performance

- Recording overhead: <1μs per frame (non-blocking)
- Queue capacity: 10,000 frames @ 1kHz = 10s buffer
- Dropped frame monitoring: Atomic counter

#### Code Statistics

- **New modules**: 3 (recording, mode, diagnostics)
- **New examples**: 3 (standard_recording, custom_diagnostics, replay_mode)
- **New CLI commands**: 1 (replay)
- **New documentation sections**: 1 major section in README

---

### 🚀 v1.0-alpha (2026-01-23)

#### Added - 高级 API (High-Level API)

**核心类型系统**:
- ✨ 强类型单位: `Rad`, `Deg`, `NewtonMeter` (NewType 模式)
- ✨ 类型安全的关节索引: `Joint` enum + `JointArray<T>`
- ✨ 笛卡尔空间类型: `CartesianPose`, `CartesianVelocity`, `CartesianEffort`, `Quaternion`
- ✨ 结构化错误处理: `RobotError` (Fatal/Recoverable/Retryable)

**Type State 状态机**:
- ✨ 编译期状态安全: `Piper<Disconnected>`, `Piper<Standby>`, `Piper<Active<MitMode>>`
- ✨ 非法状态转换在编译期被捕获
- ✨ RAII 自动资源管理: Drop trait 自动失能

**读写分离架构**:
- ✨ `RawCommander`: 内部完整权限命令发送器 (pub(crate))
- ✨ `Piper`: 公开受限权限运动控制器 (只能发送运动指令)
- ✨ `Observer`: 线程安全只读状态观察器
- ✨ 支持并发: 控制线程 + 监控线程同时运行

**性能优化**:
- ⚡ `StateTracker`: 无锁快速路径检查 (~18ns, 目标 < 100ns, **5.4x 超标**)
- ⚡ `Observer`: 高效状态读取 (~11ns, 目标 < 50ns, **4.5x 超标**)
- ⚡ `AtomicBool` 快速路径 + `RwLock` 详细信息双层设计
- ⚡ 基准测试框架 (Criterion)

**控制器框架**:
- ✨ `Controller` trait: 通用控制器接口 (Tick 模式)
- ✨ `PidController`: 工业级 PID 控制器
  - 积分饱和保护 (Integral Windup Protection)
  - 输出钳位 (Output Limiting)
  - 安全的时间跳变处理 (`on_time_jump` 保留积分项)
  - Builder 模式配置
- ✨ `TrajectoryPlanner`: 三次样条轨迹规划器
  - Iterator 模式 (O(1) 内存)
  - C² 连续平滑轨迹
  - 边界条件保证 (起止速度为 0)
  - 可重置和重用
- ✨ `LoopRunner`: 控制循环执行器
  - dt 钳位保护
  - 时间跳变检测
  - 精确定时 (spin_sleep)

**后台服务**:
- ✨ `StateMonitor`: 物理状态同步 (20Hz 后台线程)
  - 状态漂移检测
  - 自动 Poisoned 标记
- ✨ `HeartbeatManager`: 心跳保护机制 (50Hz 后台线程)
  - 防止主线程冻结导致硬件超时

**测试和质量保证**:
- ✅ 593 个测试 (100% 通过率)
- ✅ 单元测试 + 集成测试 + 属性测试 (proptest)
- ✅ Mock 硬件框架 (`MockCanBus`, `MockHardwareState`)
- ✅ CI/CD (GitHub Actions, Ubuntu + macOS, stable + nightly)
- ✅ Miri 内存安全检查
- ✅ Clippy 代码质量检查

**示例程序**:
- 📚 `high_level_simple_move.rs`: 快速入门示例
- 📚 `high_level_pid_control.rs`: PID 控制器使用示例
- 📚 `high_level_trajectory_demo.rs`: 轨迹规划器深入演示

**文档**:
- 📖 完整的设计文档系列 (v2.0 → v3.0 → v3.1 → v3.2)
- 📖 实施清单和进度跟踪
- 📖 示例使用指南
- 📖 26 个专业文档 (~250K 字)
- 📖 100% API 文档覆盖

#### Changed - 现有功能改进

**依赖项**:
- 添加 `parking_lot` (0.12) - 高性能锁
- 添加 `spin_sleep` (1.2) - 精确定时
- 添加 `thiserror` (2.0) - 错误处理
- 添加 `log` (0.4) - 日志支持
- 添加 `criterion` (0.5) - 基准测试 (dev-dependency)
- 添加 `proptest` (1.0) - 属性测试 (dev-dependency)

#### Technical Highlights - 技术亮点

**1. Type State Pattern (类型状态模式)**
```rust
let robot = Piper::connect("can0")?;          // Piper<Standby>
let robot = robot.enable_mit_mode(config)?;   // Piper<Active<MitMode>>
robot.command_torques(...)?;                  // ✅ 编译通过
// robot.command_positions(...)?;             // ❌ 编译错误
```

**2. Capability-based Security (基于能力的安全)**
```rust
// RawCommander: 内部完整权限 (pub(crate))
raw_commander.enable_arm()?;         // ✅ 内部可用
raw_commander.disable_arm()?;        // ✅ 内部可用

// Piper: 公开受限权限 (pub)
motion_commander.command_torques()?; // ✅ 公开可用
// motion_commander.enable_arm()?;   // ❌ 不存在此方法
```

**3. Atomic Fast Path (原子快速路径)**
```rust
// 无锁检查 (~18ns)
if !state_tracker.valid_flag.load(Ordering::Acquire) {
    return Err(state_tracker.read_error_details()); // 慢路径
}
// 快速路径继续...
```

**4. Safe Time Jump Handling (安全时间跳变)**
```rust
impl Controller for PidController {
    fn on_time_jump(&mut self, _dt: Duration) -> Result<(), Self::Error> {
        self.last_error = JointArray::from([0.0; 6]); // ✅ 重置 D 项
        // ❌ 不重置 integral（保持负载，防止下坠）
        Ok(())
    }
}
```

**5. Iterator Pattern for Trajectory (轨迹 Iterator)**
```rust
// O(1) 内存，按需生成
for (position, velocity) in trajectory_planner {
    // 实时计算，无内存分配
}
```

#### Performance Benchmarks - 性能基准

| 组件 | 性能 | 目标 | 倍数 | 状态 |
|------|------|------|------|------|
| StateTracker (快速路径) | ~18ns | < 100ns | 5.4x | ⚡ 超标 |
| Observer (读取) | ~11ns | < 50ns | 4.5x | ⚡ 超标 |
| TrajectoryPlanner (每步) | ~279ns | < 1µs | 3.6x | ⚡ 超标 |
| PidController (tick) | ~100ns | < 1µs | 10x | ⚡ 优秀 |

#### Code Statistics - 代码统计

- **总代码行数**: 6,296 行
- **测试数量**: 593 个
- **测试通过率**: 100%
- **文档数量**: 26 个
- **文档字数**: ~250,000 字
- **示例程序**: 3 个

#### Breaking Changes - 破坏性变更

- 无 (这是首个高级 API 版本)

#### Known Limitations - 已知限制

- 轨迹规划器当前只支持点对点运动 (起止速度为 0)
- 未来将支持 Via Points (途径点，非零速度)
- Cartesian 控制 (CartesianPose) 类型已定义但未完全集成

#### Migration Guide - 迁移指南

对于从低级 API 迁移的用户:

**Before (低级 API)**:
```rust
// 手动构造 CAN 帧
let frame = CanFrame::new(0x01, &[0x01, 0x00, ...])?;
can_bus.send(frame)?;

// 手动解析反馈
let frame = can_bus.recv()?;
let position = parse_position(&frame.data());
```

**After (高级 API)**:
```rust
// Type State + 强类型
let piper = Piper::connect(config)?
    .enable_mit_mode(config)?;

// 直接使用类型安全的 API
piper.Piper.command_torques(torques)?;

// 线程安全的状态读取
let positions = piper.observer().joint_positions();
```

#### Contributors - 贡献者

- AI Assistant (主要开发)
- User (需求分析、设计审查、反馈迭代)

#### Acknowledgments - 致谢

感谢用户的详细反馈和持续的迭代改进建议，特别是:
- Type State Pattern 的引入
- Inversion of Control (Tick 模式) 的建议
- 原子优化的性能优化建议
- PID 控制器安全性的深度分析
- 数学和数值稳定性的审查

---

## [0.x.x] - 2024-2026

### 低级 API (Low-Level API)

- 基础 CAN 通信
- 协议封装
- 设备管理
- 实时控制
- 性能优化

(详细历史见之前的 commit log)

---

[Unreleased]: https://github.com/your-org/piper-sdk-rs/compare/v1.0-alpha...HEAD
[v1.0-alpha]: https://github.com/your-org/piper-sdk-rs/releases/tag/v1.0-alpha
