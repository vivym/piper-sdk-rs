# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Development Commands

### Building and Checking
```bash
cargo check --all-targets                    # Quick compile check
cargo build --release                         # Optimized build
cargo clippy --all-targets --all-features -- -D warnings   # Linting
cargo fmt --all -- --check                    # Format check
cargo fmt --all                               # Format code
```

### Testing
```bash
# Unit tests (no hardware required, can run concurrently)
cargo test --lib

# Doctests
cargo test --doc

# Hardware tests (GS-USB device required, MUST run serially)
cargo test --test gs_usb_stage1_loopback_tests -- --ignored --test-threads=1
cargo test --test gs_usb_integration_tests -- --ignored --test-threads=1
cargo test --test gs_usb_performance_tests -- --ignored --test-threads=1

# Debug tools
cargo test --test gs_usb_debug_scan -- --ignored --nocapture
cargo test --test gs_usb_debug_step_by_step -- --ignored --nocapture
```

### Running Examples
```bash
cargo run --example state_api_demo            # One-shot state API demo
cargo run --example robot_monitor             # Real-time monitoring tool
cargo run --example timestamp_verification    # Verify SocketCAN hardware timestamps
```

### Building Binaries
```bash
cargo build --release --bin gs_usb_daemon     # Build the GS-USB daemon
```

## Architecture Overview

The SDK is organized in four layers (from low-level to high-level):

```
┌─────────────────────────────────────────────────────────────┐
│                    Client Layer                              │
│    (Type-safe API, Type State Pattern, Observer Pattern)    │
├─────────────────────────────────────────────────────────────┤
│                    Driver Layer                              │
│    (IO Threads, State Sync, Hot/Cold Data Splitting)         │
├─────────────────────────────────────────────────────────────┤
│                   Protocol Layer                             │
│              (Type-safe CAN messages using bilge)           │
├─────────────────────────────────────────────────────────────┤
│                      CAN Layer                              │
│          (SocketCAN on Linux, GS-USB cross-platform)         │
└─────────────────────────────────────────────────────────────┘
```

## Key Architectural Concepts

### Type State Pattern (Client Layer)

The client API uses zero-sized type markers for compile-time state safety:

- `Disconnected` - Initial state, cannot perform operations
- `Standby` - Connected but motor disabled (can read, not command)
- `Active<MitMode>` - Enabled with MIT impedance control
- `Active<PositionMode>` - Enabled with position control

State transitions flow: `Disconnected` → `Standby` → `Active<Mode>` → `Standby` → (Drop auto-disable)

**Important**: The `Active` state consumes the `Standby` instance. Dropping an `Active` instance automatically disables the robot.

### Hot/Cold Data Splitting (Driver Layer)

The driver layer optimizes state updates based on data frequency:

**Cold Data (Synchronized)**: Position data (`0x2A5-0x2A7`) and end-effector pose (`0x2A2-0x2A4`)
- Uses frame-group synchronization with timeout handling
- Only updates when complete frame group received
- Updated at ~500Hz

**Hot Data (Buffered Commit)**: Joint dynamic state (`0x251-0x256`)
- Collects all 6 joint frames before committing
- 6ms timeout forces commit to prevent stale data
- Maintains hardware timestamps for force control

### CAN Adapter Abstraction

Two backend options implementing the same `CanAdapter` trait:

1. **SocketCAN** (Linux only)
   - Kernel-level performance with hardware timestamps
   - Use for production Linux deployments
   - Interface: `"can0"`, `"vcan0"` (virtual), etc.

2. **GS-USB** (Linux/macOS/Windows)
   - Cross-platform via USB (userspace `rusb` driver)
   - Device enumeration by serial number
   - Use for cross-platform development or Windows/macOS

The `PiperBuilder` automatically selects the appropriate backend based on platform.

### Concurrency Model

- **IO Thread**: Dedicated RX/TX threads for CAN frame processing
- **Control Thread**: Lock-free state reading via `ArcSwap`, commands via `crossbeam-channel`
- **Frame Commit**: Ensures consistent snapshots at specific time points

## Module Structure

```
src/
├── lib.rs              # Facade pattern - re-exports public API
├── prelude.rs          # Convenient imports (use `piper_sdk::prelude::*`)
├── can/                # CAN adapter abstraction
│   ├── mod.rs          # CanAdapter trait, PiperFrame
│   ├── socketcan/      # SocketCAN implementation (Linux)
│   └── gs_usb/         # GS-USB implementation (cross-platform)
├── protocol/           # Protocol encoding/decoding (bilge-based)
│   ├── ids.rs          # CAN ID constants
│   ├── feedback.rs     # Feedback frames
│   ├── control.rs      # Control command frames
│   └── config.rs       # Configuration frames
├── driver/             # IO management and state synchronization
│   ├── mod.rs          # Driver module
│   ├── piper.rs        # Driver-level Piper API
│   ├── pipeline.rs     # IO loop, ArcSwap updates
│   ├── state.rs        # State structures (hot/cold splitting)
│   └── builder.rs      # PiperBuilder for fluent construction
├── client/             # High-level type-safe API
│   ├── mod.rs          # Client module
│   ├── motion.rs       # Piper command interface
│   ├── observer.rs     # Observer (read-only state access)
│   ├── state/          # Type state pattern markers
│   └── types/          # Type system (units, joints, errors)
└── bin/
    └── gs_usb_daemon/  # GS-USB daemon binary
```

## Protocol Notes

- **Byte order**: Motor protocol uses big-endian. Helper functions in `protocol/` handle conversion.
- **CAN IDs**: Well-defined constants in `protocol/ids.rs`
- **Frame types**: Control (0x1A1-0x1FF), Feedback (0x2A1-0x2FF), Configuration (0x5A1-0x5FF)

## Platform-Specific Considerations

### Linux
- Supports both SocketCAN and GS-USB backends
- SocketCAN requires `vcan0` for testing (automatically skipped if missing)
- Real-time thread priority requires `CAP_SYS_NICE` or `rtkit`

### Windows/macOS
- Only GS-USB backend available
- No SocketCAN support

## Hardware Testing

- GS-USB devices are **exclusive** - only one process can access at a time
- Always run hardware tests with `--test-threads=1` for serial execution
- Unit tests (no hardware) can run concurrently

## Documentation

- Design docs in `docs/v0/`: architecture, protocol, real-time configuration
- Position control guide: `docs/v0/position_control_user_guide.md`
