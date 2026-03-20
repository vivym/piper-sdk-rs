# Piper SDK

[![Crates.io](https://img.shields.io/crates/v/piper-sdk)](https://crates.io/crates/piper-sdk)
[![Documentation](https://docs.rs/piper-sdk/badge.svg)](https://docs.rs/piper-sdk)
[![License](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

**High-performance, cross-platform (Linux/Windows/macOS), zero-abstraction-overhead** Rust SDK for AgileX Piper Robot Arm with support for high-frequency force control (500Hz) and async CAN frame recording.

[中文版 README](README.zh-CN.md)

> **⚠️ IMPORTANT NOTICE**
> **This project is under active development. APIs may change. Please test carefully before using in production.**
>
> **Version Status**: The current version is **pre-0.1.0** (alpha quality). The SDK has **NOT been fully tested on real robotic arms** and may not work correctly or safely.
>
> **⚠️ SAFETY WARNING**: Do NOT use this SDK in production or with real robotic arms without comprehensive testing. The software may send incorrect commands that could damage the robot or cause safety hazards.

## ✨ Core Features

- 🚀 **Zero Abstraction Overhead**: Compile-time polymorphism, no virtual function table (vtable) overhead at runtime
- ⚡ **High-Performance Reads**: Lock-free state reading based on `ArcSwap`, nanosecond-level response
- 🔄 **Lock-Free Concurrency**: RCU (Read-Copy-Update) mechanism for efficient state sharing
- 🎯 **Type Safety**: Bit-level protocol parsing using `bilge`, compile-time data correctness guarantees
- 🌍 **Cross-Platform Support (Linux/Windows/macOS)**:
  - **Linux**: Supports both SocketCAN (kernel-level performance) and GS-USB (userspace via libusb)
  - **Windows/macOS**: GS-USB driver implementation using `rusb` (driver-free/universal)
- 🎬 **Async CAN Frame Recording**:
  - **Non-blocking hooks**: <1μs overhead per frame using `try_send`
  - **Bounded queues**: 10,000 frame capacity prevents OOM at 1kHz
  - **Hardware timestamps**: Direct use of kernel/driver interrupt timestamps
  - **TX safety**: Only records frames after successful `send()`
  - **Drop monitoring**: Built-in `dropped_frames` counter for loss tracking
- 📊 **Advanced Health Monitoring** (`embedded_bridge_host`, controller-embedded non-realtime bridge/debug path):
  - **CAN Bus Off Detection**: Detects CAN Bus Off events (critical system failure) with debounce mechanism
  - **Error Passive Monitoring**: Monitors Error Passive state (pre-Bus Off warning) for early detection
  - **USB STALL Tracking**: Tracks USB endpoint STALL errors for USB communication health
  - **Performance Baseline**: Dynamic FPS baseline tracking with EWMA for anomaly detection
  - **Health Score**: Comprehensive health scoring (0-100) based on multiple metrics

## 🏗️ Architecture

Piper SDK uses a modular workspace architecture with clear separation of concerns:

```
piper-sdk-rs/
├── crates/
│   ├── piper-protocol/    # Protocol layer (bit-level CAN protocol)
│   ├── piper-can/         # CAN abstraction (SocketCAN/GS-USB)
│   ├── piper-driver/      # Driver layer (I/O threads, state sync, hooks)
│   ├── piper-client/      # Client layer (type-safe user API)
│   ├── piper-tools/       # Recording and analysis tools
│   └── piper-sdk/         # Compatibility layer (re-exports all)
└── apps/
    └── cli/               # Command-line interface
```

### Layer Overview

| Layer | Crate | Purpose | Test Coverage |
|-------|-------|---------|---------------|
| Protocol | `piper-protocol` | Type-safe CAN protocol encoding/decoding | 214 tests ✅ |
| CAN | `piper-can` | Hardware abstraction for CAN adapters | 97 tests ✅ |
| Driver | `piper-driver` | I/O management, state sync, hooks | 149 tests ✅ |
| Client | `piper-client` | High-level type-safe API | 105 tests ✅ |
| Tools | `piper-tools` | Recording, statistics, safety | 23 tests ✅ |
| SDK | `piper-sdk` | Compatibility layer (re-exports) | 588 tests ✅ |

**Benefits**:
- ✅ **Faster compilation**: Only recompile modified layers (up to 88% faster)
- ✅ **Flexible dependencies**: Depend on specific layers to reduce bloat
- ✅ **Clear boundaries**: Each layer has well-defined responsibilities
- ✅ **100% backward compatible**: Existing code requires zero changes

See [Workspace Migration Guide](docs/v0/workspace/USER_MIGRATION_GUIDE.md) for details.

## 🛠️ Tech Stack

| Module | Crates | Purpose |
|--------|--------|---------|
| CAN Interface | Custom `CanAdapter` | Lightweight CAN adapter Trait (no embedded burden) |
| Linux Backend | `socketcan` | Native Linux CAN support (SocketCAN interface) |
| USB Backend | `rusb` | USB device operations on all platforms, implementing GS-USB protocol |
| Protocol Parsing | `bilge` | Bit operations, unaligned data processing, alternative to serde |
| Concurrency Model | `crossbeam-channel` | High-performance MPSC channel for sending control commands |
| State Sharing | `arc-swap` | RCU mechanism for lock-free reading of latest state |
| Frame Hooks | `hooks` + `recording` | Non-blocking async recording with bounded queues |
| Error Handling | `thiserror` | Precise error enumeration within SDK |
| Logging | `tracing` | Structured logging |

## 📦 Installation

Add the dependency to your `Cargo.toml`:

```toml
[dependencies]
piper-sdk = "0.1"
```

### Optional Features

#### Serde Serialization Support

Enable serialization/deserialization for data types:

```toml
[dependencies]
piper-sdk = { version = "0.1", features = ["serde"] }
```

This adds `Serialize` and `Deserialize` implementations to:
- Type units (`Rad`, `Deg`, `NewtonMeter`, etc.)
- Joint arrays and joint indices
- Cartesian pose and quaternion types
- **CAN frames (`PiperFrame`, `GsUsbFrame`)** - for frame dump/replay

Example usage:

```rust
use piper_sdk::prelude::*;
use serde_json;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Serialize joint positions
    let positions = JointArray::from([
        Rad(0.0), Rad(0.5), Rad(0.0),
        Rad(0.0), Rad(0.0), Rad(0.0)
    ]);

    let json = serde_json::to_string(&positions)?;
    println!("Serialized: {}", json);

    // Deserialize back
    let deserialized: JointArray<Rad> = serde_json::from_str(&json)?;

    Ok(())
}
```

#### Frame Dump Example

For CAN frame recording and replay:

```bash
# Run frame dump example
cargo run -p piper-sdk --example frame_dump --features serde
```

This demonstrates:
- Recording CAN frames to JSON
- Saving/loading frame data
- Debugging CAN bus communication

See [examples/frame_dump.rs](../crates/piper-sdk/examples/frame_dump.rs) for details.

### Platform-Specific Features

Features are automatically selected based on your target platform:
- **Linux**: `socketcan` (SocketCAN support)
- **Linux/macOS/Windows**: `gs_usb` (GS-USB USB adapter)

No manual feature configuration needed for platform selection!

### Advanced Usage: Depending on Specific Layers

For reduced dependencies, you can depend on specific layers directly:

```toml
# Use only the client layer (most common)
[dependencies]
piper-client = "0.1"

# Use only the driver layer (for advanced users)
[dependencies]
piper-driver = "0.1"

# Use only tools (for recording/analysis)
[dependencies]
piper-tools = "0.1"
```

**Note**: When using specific layers, update your imports:
- `piper_sdk::Piper` → `piper_client::Piper`
- `piper_sdk::Driver` → `piper_driver::Piper`

See [Workspace Migration Guide](docs/v0/workspace/USER_MIGRATION_GUIDE.md) for migration details.

## 🚀 Quick Start

### Basic Usage (Client API - Recommended)

Most users should use the high-level client API for type-safe, easy-to-use control:

```rust
use piper_sdk::prelude::*;

fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    // Connect using an explicit SocketCAN target on Linux.
    // On macOS/Windows, use `.gs_usb_auto()` or `.gs_usb_serial(...)`.
    let robot = PiperBuilder::new()
        .socketcan("can0")
        .baud_rate(1_000_000)
        .build()?;
    let robot = robot.enable_position_mode(PositionModeConfig::default())?;

    // Get observer for reading state
    let observer = robot.observer();

    // Read state (lock-free, nanosecond-level response)
    let joint_pos = observer.joint_positions();
    println!("Joint positions: {:?}", joint_pos);

    // Send position command with type-safe units (methods are directly on robot)
    let target = JointArray::from([Rad(0.5), Rad(0.0), Rad(0.0), Rad(0.0), Rad(0.0), Rad(0.0)]);
    robot.send_position_command(&target)?;

    Ok(())
}
```

### CAN Frame Recording

Record CAN frames asynchronously with non-blocking hooks:

```rust
use piper_driver::recording::AsyncRecordingHook;
use piper_driver::hooks::FrameCallback;
use piper_sdk::prelude::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::Duration;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Create recording hook
    let (hook, rx) = AsyncRecordingHook::new();
    let dropped_counter = hook.dropped_frames().clone();

    // Register as callback
    let callback = Arc::new(hook) as Arc<dyn FrameCallback>;

    // Connect robot
    let robot = PiperBuilder::new()
        .socketcan("can0")
        .build()?;

    // Register hook in driver layer
    // (Note: This is advanced usage - see driver API docs)
    robot.context.hooks.write()?.add_callback(callback);

    // Spawn recording thread
    let handle = thread::spawn(move || {
        let mut file = std::fs::File::create("recording.bin")?;
        while let Ok(frame) = rx.recv() {
            // Process frame: write to file, analyze, etc.
            println!("Received frame: ID=0x{:03X}, timestamp={}us",
                     frame.id, frame.timestamp_us);
        }
        Ok::<_, Box<dyn std::error::Error>>(())
    });

    // Run for 5 seconds
    thread::sleep(Duration::from_secs(5));

    // Check dropped frames
    let dropped = dropped_counter.load(Ordering::Relaxed);
    println!("Dropped frames: {}", dropped);

    handle.join().ok();
    Ok(())
}
```

**Key Features**:
- ✅ **Non-blocking**: `<1μs` overhead per frame
- ✅ **OOM-safe**: Bounded queue (10,000 frames @ 1kHz = 10s buffer)
- ✅ **Hardware timestamps**: Microsecond precision from kernel/driver
- ✅ **TX safe**: Only records successfully sent frames
- ✅ **Loss tracking**: Built-in `dropped_frames` counter

## 🎬 Recording and Replay

Piper SDK provides three complementary APIs for CAN frame recording and replay:

| API | Use Case | Complexity | Safety |
|-----|----------|------------|--------|
| **Standard Recording** | Simple record-and-save workflows | ⭐ Low | ✅ Type-safe |
| **Custom Diagnostics** | Real-time frame analysis & custom processing | ⭐⭐ Medium | ✅ Thread-safe |
| **ReplayMode** | Safe replay of recorded sessions | ⭐⭐ Medium | ✅ Type-safe + Driver-level protection |

### 1. Standard Recording API

The simplest way to record CAN frames to a file:

```rust
use piper_client::{PiperBuilder, recording::{RecordingConfig, RecordingMetadata, StopCondition}};
use std::time::Duration;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Connect to robot
    let robot = PiperBuilder::new()
        .socketcan("can0")
        .build()?;

    // Start recording with metadata
    let (robot, handle) = robot.start_recording(RecordingConfig {
        output_path: "demo_recording.bin".into(),
        stop_condition: StopCondition::Duration(10), // Record for 10 seconds
        metadata: RecordingMetadata {
            notes: "Standard recording example".to_string(),
            operator: "DemoUser".to_string(),
        },
    })?;

    // Perform operations (all CAN frames are recorded)
    tokio::time::sleep(Duration::from_secs(10)).await;

    // Stop recording and get statistics
    let (robot, stats) = robot.stop_recording(handle)?;

    println!("Recorded {} frames in {:.2}s", stats.frame_count, stats.duration.as_secs_f64());
    println!("Dropped frames: {}", stats.dropped_frames);

    Ok(())
}
```

**Features**:
- ✅ **Automatic stop conditions**: Duration, frame count, or manual
- ✅ **Rich metadata**: Record operator, notes, timestamps
- ✅ **Statistics**: Frame count, duration, dropped frames
- ✅ **Type-safe**: Recording handle prevents misuse

See [examples/standard_recording.rs](examples/standard_recording.rs) for full example.

### 2. Custom Diagnostics API

Advanced users can register custom frame callbacks for real-time analysis:

```rust
use piper_client::PiperBuilder;
use piper_driver::recording::AsyncRecordingHook;
use std::sync::Arc;
use std::thread;

fn main() -> anyhow::Result<()> {
    // Connect and enable robot
    let robot = PiperBuilder::new()
        .socketcan("can0")
        .build()?;
    let active = robot.enable_position_mode(Default::default())?;

    // Get diagnostics interface
    let diag = active.diagnostics();

    // Create custom recording hook
    let (hook, rx) = AsyncRecordingHook::new();
    let dropped_counter = hook.dropped_frames().clone();

    // Register hook
    let callback = Arc::new(hook) as Arc<dyn piper_driver::FrameCallback>;
    diag.register_callback(callback)?;

    // Process frames in background thread
    thread::spawn(move || {
        let mut frame_count = 0;
        while let Ok(frame) = rx.recv() {
            frame_count += 1;

            // Custom analysis: e.g., CAN ID distribution, timing analysis
            if frame_count % 1000 == 0 {
                println!("Received frame: ID=0x{:03X}", frame.id);
            }
        }

        println!("Total frames: {}", frame_count);
        println!("Dropped: {}", dropped_counter.load(std::sync::atomic::Ordering::Relaxed));
    });

    // Run operations...
    thread::sleep(std::time::Duration::from_secs(5));

    // Shutdown
    let _standby = active.shutdown()?;

    Ok(())
}
```

**Features**:
- ✅ **Real-time processing**: Analyze frames as they arrive
- ✅ **Custom logic**: Implement any analysis algorithm
- ✅ **Background threading**: Non-blocking main thread
- ✅ **Loss tracking**: Monitor dropped frames

See [examples/custom_diagnostics.rs](examples/custom_diagnostics.rs) for full example.

### 3. ReplayMode API

Safely replay previously recorded sessions with driver-level protection:

```rust
use piper_client::PiperBuilder;

fn main() -> anyhow::Result<()> {
    // Connect to robot
    let robot = PiperBuilder::new()
        .socketcan("can0")
        .build()?;

    // Enter ReplayMode (Driver TX thread pauses automatically)
    let replay = robot.enter_replay_mode()?;

    // Replay recording at 2.0x speed
    let robot = replay.replay_recording("demo_recording.bin", 2.0)?;

    // Automatically exits ReplayMode (TX thread resumes)
    println!("Replay completed!");

    Ok(())
}
```

**Safety Features**:
- ✅ **Driver-level protection**: TX thread pauses during replay (no dual control flow)
- ✅ **Speed limits**: Maximum 5.0x, recommended ≤ 2.0x with warnings
- ✅ **Type-safe transitions**: Cannot call enable/disable in ReplayMode
- ✅ **Automatic cleanup**: Always returns to Standby state

**Speed Guidelines**:
- **1.0x**: Original speed (recommended for most use cases)
- **0.1x ~ 2.0x**: Safe range for testing/debugging
- **> 2.0x**: Use with caution - ensure safe environment
- **Maximum**: 5.0x (hard limit for safety)

See [examples/replay_mode.rs](examples/replay_mode.rs) for full example with speed validation.

### CLI Usage

The `piper-cli` tool provides convenient commands for recording and replay:

```bash
# Record CAN frames
piper-cli record -o demo.bin --duration 10

# Replay recording (normal speed)
piper-cli replay -i demo.bin

# Replay at 2.0x speed
piper-cli replay -i demo.bin --speed 2.0

# Replay without confirmation prompt
piper-cli replay -i demo.bin --confirm
```

### Complete Workflow Example

```bash
# Step 1: Record a session
cargo run --example standard_recording

# Step 2: Analyze the recording
cargo run --example custom_diagnostics

# Step 3: Replay the recording safely
cargo run --example replay_mode
```

### Architecture Highlights

#### Why Three APIs?

Each API serves a distinct purpose:

1. **Standard Recording**: For users who want "just record this" without complexity
2. **Custom Diagnostics**: For researchers developing custom analysis tools
3. **ReplayMode**: For test engineers reproducing bugs or testing sequences

#### Type Safety Through Type States

The ReplayMode API uses Rust's type system for compile-time safety:

```rust
// ✅ Compile-time error: cannot enable in ReplayMode
let replay = robot.enter_replay_mode()?;
let active = replay.enable_position_mode(...);  // ERROR!

// ✅ Must exit ReplayMode first
let robot = replay.replay_recording(...)?;
let active = robot.enable_position_mode(...);  // OK!
```

#### Driver-Level Protection

ReplayMode switches the driver to `DriverMode::Replay`, which:

- **Pauses periodic TX**: Driver stops sending automatic control commands
- **Allows explicit frames**: Only replay frames are sent to CAN bus
- **Prevents conflicts**: No dual control flow (Driver vs Replay)

This design is documented in [architecture analysis](docs/architecture/piper-driver-client-mixing-analysis.md).

### Advanced Usage (Driver API)

For direct CAN frame control or maximum performance, use the driver API:

```rust
use piper_sdk::driver::PiperBuilder;

fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    // Create driver instance
    let robot = PiperBuilder::new()
        .socketcan("can0")  // Linux: explicit SocketCAN target
        .baud_rate(1_000_000)  // CAN baud rate
        .build()?;

    // Get current state (lock-free, nanosecond-level response)
    let joint_pos = robot.get_joint_position();
    println!("Joint positions: {:?}", joint_pos.joint_pos);

    // Send control frame
    let frame = piper_sdk::PiperFrame::new_standard(0x1A1, &[0x01, 0x02, 0x03]);
    robot.send_frame(frame)?;

    Ok(())
}
```

## 🏗️ Architecture Design

### Hot/Cold Data Splitting

For performance optimization, state data is divided into two categories:

- **High-frequency Data (200Hz)**:
  - `JointPositionState`: Joint positions (6 joints)
  - `EndPoseState`: End-effector pose (position and orientation)
  - `JointDynamicState`: Joint dynamic state (joint velocities, currents)
  - `RobotControlState`: Robot control status (control mode, robot status, fault codes, etc.)
  - `GripperState`: Gripper status (travel, torque, status codes, etc.)
  - Uses `ArcSwap` for lock-free reading, optimized for high-frequency control loops

- **Low-frequency Data (40Hz)**:
  - `JointDriverLowSpeedState`: Joint driver diagnostic state (temperatures, voltages, currents, driver status)
  - `CollisionProtectionState`: Collision protection levels (on-demand)
  - `JointLimitConfigState`: Joint angle and velocity limits (on-demand)
  - `JointAccelConfigState`: Joint acceleration limits (on-demand)
  - `EndLimitConfigState`: End-effector velocity and acceleration limits (on-demand)
  - Uses `ArcSwap` for diagnostic data, `RwLock` for configuration data

### Architecture Layers

The SDK uses a layered architecture from low-level to high-level:

- **CAN Layer** (`can`): CAN hardware abstraction, supports SocketCAN and GS-USB
- **Protocol Layer** (`protocol`): Type-safe protocol encoding/decoding
- **Driver Layer** (`driver`): IO thread management, state synchronization, frame parsing
  - **Hooks System**: Runtime callback registration for frame recording
  - **Recording Module**: Async non-blocking recording with bounded queues
- **Client Layer** (`client`): Type-safe, user-friendly control interface
- **Tools Layer** (`tools`): Recording formats, statistics, safety validation

### Core Components

```
piper-sdk-rs/
├── crates/
│   ├── piper-protocol/
│   │   └── src/
│   │       ├── lib.rs          # Protocol module entry
│   │       ├── ids.rs          # CAN ID constants/enums
│   │       ├── feedback.rs     # Robot arm feedback frames (bilge)
│   │       ├── control.rs      # Control command frames (bilge)
│   │       └── config.rs       # Configuration frames (bilge)
│   ├── piper-can/
│   │   └── src/
│   │       ├── lib.rs          # CAN module entry
│   │       ├── socketcan/      # [Linux] SocketCAN implementation
│   │       └── gs_usb/         # [Win/Mac/Linux] GS-USB protocol
│   ├── piper-driver/
│   │   └── src/
│   │       ├── mod.rs          # Driver module entry
│   │       ├── piper.rs        # Driver-level Piper object (API)
│   │       ├── pipeline.rs     # IO Loop, ArcSwap update logic
│   │       ├── state.rs        # State structure definitions
│   │       ├── hooks.rs        # Hook system for frame callbacks
│   │       ├── recording.rs    # Async recording with bounded queues
│   │       ├── builder.rs      # PiperBuilder (fluent construction)
│   │       └── metrics.rs      # Performance metrics
│   ├── piper-client/
│   │   └── src/
│   │       ├── mod.rs          # Client module entry
│   │       ├── observer.rs      # Observer (read-only state access)
│   │       ├── state/           # Type State Pattern state machine
│   │       ├── motion.rs       # Piper command interface
│   │       └── types/           # Type system (units, joints, errors)
│   └── piper-tools/
│       └── src/
│           ├── recording.rs    # Recording formats and tools
│           ├── statistics.rs    # CAN statistics analysis
│           └── safety.rs        # Safety validation
└── apps/
    └── cli/
        └── src/
            ├── commands/       # CLI commands
            └── modes/          # CLI modes (repl, oneshot)
```

### Concurrency Model

Adopts **asynchronous IO concepts but implemented with synchronous threads** (ensuring deterministic latency):

1. **IO Thread**: Responsible for CAN frame transmission/reception and state updates
2. **Control Thread**: Lock-free reading of latest state via `ArcSwap`, sending commands via `crossbeam-channel`
3. **Frame Commit Mechanism**: Ensures the state read by control threads is a consistent snapshot at a specific time point
4. **Hook System**: Non-blocking callbacks triggered on RX/TX frames for recording

## 📚 Examples

Check the `examples/` directory for more examples:

> **Note**: Example code is under development. See [examples/](examples/) directory for more examples.

Available examples:
- `state_api_demo.rs` - Simple state reading and printing
- `realtime_control_demo.rs` - Real-time control with dual-threaded architecture
- `robot_monitor.rs` - Robot state monitoring
- `timestamp_verification.rs` - Timestamp synchronization verification
- `standard_recording.rs` - 📼 Standard recording API usage (record CAN frames to file)
- `custom_diagnostics.rs` - 🔧 Custom diagnostics interface (real-time frame analysis)
- `replay_mode.rs` - 🔄 ReplayMode API (safe CAN frame replay)

Planned examples:
- `torque_control.rs` - Force control demonstration
- `configure_can.rs` - CAN baud rate configuration tool

## 🤝 Contributing

Contributions are welcome! Please see [CONTRIBUTING.md](CONTRIBUTING.md) for details.

## 📄 License

This project is licensed under the MIT License. See the [LICENSE](LICENSE) file for details.

## 📖 Documentation

For detailed design documentation, see:
- [Architecture Design Document](docs/v0/TDD.md)
- [Protocol Document](docs/v0/protocol.md)
- [Real-time Configuration Guide](docs/v0/realtime_configuration.md)
- [Real-time Optimization Guide](docs/v0/realtime_optimization.md)
- [Migration Guide](docs/v0/MIGRATION_GUIDE.md) - Guide for migrating from v0.1.x to v0.2.0+
- [Position Control & MOVE Mode User Guide](docs/v0/position_control_user_guide.md) - Complete guide for position control and motion types
- **[Hooks System Code Review](docs/architecture/code-review-v1.2.1-hooks-system.md)** - Deep dive into the recording system design
- **[Full Repository Code Review](docs/architecture/code-review-full-repo-v1.2.1.md)** - Comprehensive codebase analysis

## 🔗 Related Links

- [AgileX Robotics](https://www.agilex.ai/)
- [bilge](https://docs.rs/bilge/)
- [rusb](https://docs.rs/rusb/)
