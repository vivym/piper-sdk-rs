# Piper SDK

[![Crates.io](https://img.shields.io/crates/v/piper-sdk)](https://crates.io/crates/piper-sdk)
[![Documentation](https://docs.rs/piper-sdk/badge.svg)](https://docs.rs/piper-sdk)
[![License](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

**High-performance, cross-platform (Linux/Windows/macOS), zero-abstraction-overhead** Rust SDK for AgileX Piper Robot Arm with support for high-frequency force control (500Hz).

[中文版 README](README.zh-CN.md)

## ✨ Core Features

- 🚀 **Zero Abstraction Overhead**: Compile-time polymorphism, no virtual function table (vtable) overhead at runtime
- ⚡ **High-Performance Reads**: Lock-free state reading based on `ArcSwap`, nanosecond-level response
- 🔄 **Lock-Free Concurrency**: RCU (Read-Copy-Update) mechanism for efficient state sharing
- 🎯 **Type Safety**: Bit-level protocol parsing using `bilge`, compile-time data correctness guarantees
- 🌍 **Cross-Platform Support (Linux/Windows/macOS)**:
  - **Linux**: Supports both SocketCAN (kernel-level performance) and GS-USB (userspace via libusb)
  - **Windows/macOS**: GS-USB driver implementation using `rusb` (driver-free/universal)
- 📊 **Advanced Health Monitoring** (gs_usb_daemon):
  - **CAN Bus Off Detection**: Detects CAN Bus Off events (critical system failure) with debounce mechanism
  - **Error Passive Monitoring**: Monitors Error Passive state (pre-Bus Off warning) for early detection
  - **USB STALL Tracking**: Tracks USB endpoint STALL errors for USB communication health
  - **Performance Baseline**: Dynamic FPS baseline tracking with EWMA for anomaly detection
  - **Health Score**: Comprehensive health scoring (0-100) based on multiple metrics

## 🛠️ Tech Stack

| Module | Crates | Purpose |
|--------|--------|---------|
| CAN Interface | Custom `CanAdapter` | Lightweight CAN adapter Trait (no embedded burden) |
| Linux Backend | `socketcan` | Native Linux CAN support (SocketCAN interface) |
| USB Backend | `rusb` | USB device operations on all platforms, implementing GS-USB protocol |
| Protocol Parsing | `bilge` | Bit operations, unaligned data processing, alternative to serde |
| Concurrency Model | `crossbeam-channel` | High-performance MPSC channel for sending control commands |
| State Sharing | `arc-swap` | RCU mechanism for lock-free reading of latest state |
| Error Handling | `thiserror` | Precise error enumeration within SDK |
| Logging | `tracing` | Structured logging |

## 📦 Installation

Add the dependency to your `Cargo.toml`:

```toml
[dependencies]
piper-sdk = "0.0.1"
```

### Optional: Real-time Thread Priority Support

For high-frequency control scenarios (500Hz-1kHz), you can enable the `realtime` feature to set RX thread to maximum priority:

```toml
[dependencies]
piper-sdk = { version = "0.0.1", features = ["realtime"] }
```

**Note**: On Linux, setting thread priority requires appropriate permissions:
- Run with `CAP_SYS_NICE` capability: `sudo setcap cap_sys_nice+ep /path/to/your/binary`
- Or use `rtkit` (RealtimeKit) for user-space priority elevation
- Or run as root (not recommended for production)

If permission is insufficient, the SDK will log a warning but continue to function normally.

See [Real-time Configuration Guide](docs/v0/realtime_configuration.md) for detailed setup instructions.

## 🚀 Quick Start

### Basic Usage (Client API - Recommended)

Most users should use the high-level client API for type-safe, easy-to-use control:

```rust
use piper_sdk::prelude::*;

fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    // Connect using Builder API (automatically handles platform differences)
    let robot = PiperBuilder::new()
        .interface("can0")
        .baud_rate(1_000_000)
        .build()?;
    let robot = robot.enable_mit_mode()?;

    // Get motion commander and observer
    let motion = robot.Piper;
    let observer = robot.observer();

    // Read state (lock-free, nanosecond-level response)
    let joint_pos = observer.get_joint_position();
    println!("Joint positions: {:?}", joint_pos);

    // Send position command with type-safe units
    let target = JointArray::from([Rad(0.5), Rad(0.0), Rad(0.0), Rad(0.0), Rad(0.0), Rad(0.0)]);
    motion.command_positions(target)?;

    Ok(())
}
```

### Advanced Usage (Driver API)

For direct CAN frame control or maximum performance, use the driver API:

```rust
use piper_sdk::driver::PiperBuilder;

fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    // Create driver instance
    let robot = PiperBuilder::new()
        .interface("can0")?  // Linux: SocketCAN interface name (or GS-USB device serial)
        .baud_rate(1_000_000)?  // CAN baud rate
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
- **Client Layer** (`client`): Type-safe, user-friendly control interface

### Core Components

```
piper-rs/
├── src/
│   ├── lib.rs              # Library entry, Facade Pattern exports
│   ├── prelude.rs          # Convenient imports for common types
│   ├── can/                # CAN communication adapter layer
│   │   ├── mod.rs          # CAN adapter Trait and common types
│   │   └── gs_usb/         # [Win/Mac] GS-USB protocol implementation
│   ├── protocol/           # Protocol definitions (business-agnostic, pure data)
│   │   ├── ids.rs          # CAN ID constants/enums
│   │   ├── feedback.rs     # Robot arm feedback frames (bilge)
│   │   ├── control.rs      # Control command frames (bilge)
│   │   └── config.rs       # Configuration frames (bilge)
│   ├── driver/             # Driver layer (IO management, state sync)
│   │   ├── mod.rs          # Driver module entry
│   │   ├── piper.rs        # Driver-level Piper object (API)
│   │   ├── pipeline.rs     # IO Loop, ArcSwap update logic
│   │   ├── state.rs        # State structure definitions (hot/cold data splitting)
│   │   ├── builder.rs      # PiperBuilder (fluent construction)
│   │   └── error.rs        # DriverError (error types)
│   └── client/             # Client layer (type-safe, user-friendly API)
│       ├── mod.rs          # Client module entry
│       ├── motion.rs        # Piper (command interface)
│       ├── observer.rs      # Observer (read-only state access)
│       ├── state/           # Type State Pattern state machine
│       ├── control/         # Controllers and trajectory planning
│       └── types/           # Type system (units, joints, errors)
```

### Concurrency Model

Adopts **asynchronous IO concepts but implemented with synchronous threads** (ensuring deterministic latency):

1. **IO Thread**: Responsible for CAN frame transmission/reception and state updates
2. **Control Thread**: Lock-free reading of latest state via `ArcSwap`, sending commands via `crossbeam-channel`
3. **Frame Commit Mechanism**: Ensures the state read by control threads is a consistent snapshot at a specific time point

## 📚 Examples

Check the `examples/` directory for more examples:

> **Note**: Example code is under development. See [examples/](examples/) directory for more examples.

Available examples:
- `state_api_demo.rs` - Simple state reading and printing
- `realtime_control_demo.rs` - Real-time control with dual-threaded architecture
- `robot_monitor.rs` - Robot state monitoring
- `timestamp_verification.rs` - Timestamp synchronization verification

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

## 🔗 Related Links

- [AgileX Robotics](https://www.agilex.ai/)
- [bilge](https://docs.rs/bilge/)
- [rusb](https://docs.rs/rusb/)

---

**Note**: This project is under active development. APIs may change. Please test carefully before using in production.
