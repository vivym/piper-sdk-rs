# Real-time Optimization Guide

This guide explains how to use Phase 1+ real-time optimization features for high-frequency control.

## Overview

Phase 1 introduced a dual-threaded IO architecture that eliminates head-of-line blocking and enables high-frequency control (500Hz-1kHz).

### Key Features

- **Dual-threaded Architecture**: Separate RX and TX threads for physical isolation
- **Command Priority**: Distinguish between real-time and reliable commands
- **Performance Metrics**: Zero-overhead atomic counters for monitoring
- **Thread Lifecycle Management**: Automatic health monitoring and graceful shutdown

## Architecture

### Single-threaded Mode (Legacy)

```
Control Thread → [Command Queue] → IO Thread (RX + TX)
```

**Limitations**:
- Head-of-line blocking: Slow TX operations block RX
- Limited throughput for high-frequency control

### Dual-threaded Mode (Phase 1+)

```
Control Thread → [Realtime Queue] → TX Thread
                → [Reliable Queue] → TX Thread

RX Thread → [State Updates] → Control Thread (lock-free)
```

**Advantages**:
- Physical isolation: RX and TX operations don't block each other
- Priority scheduling: Real-time commands take precedence
- Higher throughput: Supports 500Hz-1kHz control loops

## Usage

### Creating a Dual-threaded Instance

Use `PiperBuilder` with `dual_thread()`:

```rust
use piper_sdk::PiperBuilder;

let robot = PiperBuilder::new()
    .interface("can0")?
    .baud_rate(1_000_000)?
    .dual_thread()  // Enable dual-threaded mode
    .build()?;
```

### Sending Commands

#### Real-time Commands (Overwritable)

For high-frequency control commands (500Hz-1kHz), use `send_realtime()`:

```rust
// Real-time command: If queue is full, new command overwrites old one
let frame = piper_sdk::PiperFrame::new_standard(0x1A1, &[0x01, 0x02, 0x03]);
robot.send_realtime(frame)?;
```

**Characteristics**:
- Queue capacity: 1 (single frame)
- Strategy: Overwrite (latest command always sent)
- Use case: Joint position control, force control

#### Reliable Commands (FIFO)

For configuration frames and state machine transitions, use `send_reliable()`:

```rust
// Reliable command: FIFO queue, no overwriting
let frame = piper_sdk::PiperFrame::new_standard(0x1A2, &[0x04, 0x05, 0x06]);
robot.send_reliable(frame)?;
```

**Characteristics**:
- Queue capacity: 10 (multiple frames)
- Strategy: FIFO (first-in-first-out)
- Use case: Configuration updates, mode switches

#### Using Command Types (Phase 2+)

For type-safe command sending, use `PiperCommand`:

```rust
use piper_sdk::robot::command::{CommandPriority, PiperCommand};

// Create real-time command
let frame = piper_sdk::PiperFrame::new_standard(0x1A1, &[0x01, 0x02, 0x03]);
let cmd = PiperCommand::realtime(frame);
robot.send_command(cmd)?;

// Create reliable command
let frame = piper_sdk::PiperFrame::new_standard(0x1A2, &[0x04, 0x05, 0x06]);
let cmd = PiperCommand::reliable(frame);
robot.send_command(cmd)?;
```

### Monitoring Performance

Use the metrics API to monitor IO health:

```rust
let metrics = robot.get_metrics();
println!("RX frames total: {}", metrics.rx_frames_total);
println!("TX frames total: {}", metrics.tx_frames_total);
println!("RX timeouts: {}", metrics.rx_timeouts);
println!("TX timeouts: {}", metrics.tx_timeouts);
println!("Realtime overwrites: {}", metrics.tx_realtime_overwrites);
println!("Reliable drops: {}", metrics.tx_reliable_drops);
```

### Health Checking

Check if RX and TX threads are healthy:

```rust
if robot.is_healthy() {
    println!("All threads are running normally");
} else {
    let (rx_alive, tx_alive) = robot.check_health();
    if !rx_alive {
        eprintln!("RX thread has stopped!");
    }
    if !tx_alive {
        eprintln!("TX thread has stopped!");
    }
}
```

## Performance Tuning

### Thread Priority

Enable the `realtime` feature for maximum thread priority:

```toml
[dependencies]
piper-sdk = { version = "0.0.1", features = ["realtime"] }
```

See [Real-time Configuration Guide](realtime_configuration.md) for setup instructions.

### Timeout Configuration

Configure receive and send timeouts for your use case:

```rust
use std::time::Duration;

// Set receive timeout (default: 5ms for real-time mode)
robot.set_receive_timeout(Duration::from_millis(2))?;

// Set send timeout (default: 5ms for real-time mode)
robot.set_send_timeout(Duration::from_millis(2))?;
```

### Monitoring Metrics

Regularly check metrics to identify performance bottlenecks:

```rust
let metrics = robot.get_metrics();

// High timeout rate indicates RX thread is too slow
if metrics.rx_timeouts > threshold {
    // Consider: Increase thread priority, reduce processing time
}

// High overwrite rate indicates TX thread is too slow
if metrics.tx_realtime_overwrites > threshold {
    // Consider: Optimize TX path, reduce send delay
}

// High drop rate indicates reliable queue is full
if metrics.tx_reliable_drops > threshold {
    // Consider: Use send_reliable_timeout() for blocking send
}
```

## Best Practices

### 1. Use Real-time Commands for High-Frequency Control

For 500Hz-1kHz control loops, always use `send_realtime()`:

```rust
// Control loop (500Hz)
loop {
    let state = robot.get_joint_position();
    let command = compute_control(state);
    robot.send_realtime(command)?;
    thread::sleep(Duration::from_millis(2));
}
```

### 2. Use Reliable Commands for Configuration

For one-time configuration updates, use `send_reliable()`:

```rust
// Configuration update
let config_frame = build_config_frame();
robot.send_reliable(config_frame)?;
```

### 3. Monitor Health Regularly

Check thread health in your control loop:

```rust
if !robot.is_healthy() {
    eprintln!("IO threads unhealthy, stopping control loop");
    break;
}
```

### 4. Handle Errors Gracefully

Real-time commands may fail if TX thread is stuck:

```rust
match robot.send_realtime(frame) {
    Ok(_) => {},
    Err(RobotError::ChannelFull) => {
        // TX thread may be stuck, log and continue
        eprintln!("Warning: TX thread may be stuck");
    },
    Err(e) => {
        // Other errors, handle appropriately
        return Err(e);
    },
}
```

## Troubleshooting

### High RX Timeout Rate

**Symptoms**: `metrics.rx_timeouts` is high

**Possible Causes**:
1. RX thread priority too low
2. System load too high
3. CAN bus errors

**Solutions**:
1. Enable `realtime` feature and configure permissions
2. Reduce system load (isolate CPU cores)
3. Check CAN bus health

### High Realtime Overwrite Rate

**Symptoms**: `metrics.tx_realtime_overwrites` is high

**Possible Causes**:
1. TX thread too slow
2. Send timeout too long
3. CAN bus errors

**Solutions**:
1. Optimize TX path (reduce processing time)
2. Reduce send timeout
3. Check CAN bus health

### Thread Health Check Fails

**Symptoms**: `robot.is_healthy()` returns `false`

**Possible Causes**:
1. Fatal CAN error (device disconnected)
2. Thread panic
3. System resource exhaustion

**Solutions**:
1. Check CAN device connection
2. Review error logs
3. Check system resources (memory, file descriptors)

## Performance Benchmarks

Typical performance in mock test environment:

- **RX Interval P95**: < 5ms (500Hz), < 3ms (1kHz)
- **TX Latency P95**: < 1ms
- **Send Duration P95**: < 500µs

**Note**: Real hardware performance may vary. Use the metrics API to monitor actual performance.

## References

- [CAN IO Threading Improvement Plan](can_io_threading_improvement_plan_v2.md)
- [Phase 1 Technical Summary](phase1_technical_summary.md)
- [Real-time Configuration Guide](realtime_configuration.md)

