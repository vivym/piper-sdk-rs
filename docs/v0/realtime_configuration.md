# Real-time Configuration Guide

This guide explains how to configure the `realtime` feature for high-frequency control scenarios (500Hz-1kHz).

## Overview

The `realtime` feature enables setting high thread priorities for critical RX/TX threads, which is crucial for low-latency force control applications.

**Performance Impact**:
- **With `realtime` feature**: RX thread latency typically 50-200µs (P95)
- **Without `realtime` feature**: RX thread latency typically 1-10ms (P95)

## Enabling the Feature

Add the `realtime` feature to your `Cargo.toml`:

```toml
[dependencies]
piper-sdk = { version = "0.0.1", features = ["realtime"] }
```

Build with the feature enabled:

```bash
cargo build --release --features realtime
```

## Linux Permissions

On Linux, setting real-time thread priorities (e.g., `ThreadPriority::Max`) typically requires special permissions.

### Option 1: Using `setcap` (Recommended)

Grant `CAP_SYS_NICE` capability to your executable:

```bash
sudo setcap 'cap_sys_nice+ep' /path/to/your/executable
```

**Advantages**:
- No need to run as root
- Persistent across reboots
- Secure (only grants specific capability)

**Verification**:
```bash
getcap /path/to/your/executable
# Should output: /path/to/your/executable = cap_sys_nice+ep
```

### Option 2: Using `rtkit` (RealtimeKit)

Configure `rtkit` for your user or group. This requires system-level configuration:

1. Install `rtkit` (usually pre-installed on modern Linux distributions)
2. Add your user to the `realtime` group (if configured)
3. Configure `rtkit` policy (system-specific)

**Note**: `rtkit` configuration varies by distribution. Consult your distribution's documentation.

### Option 3: Running as Root (Not Recommended)

You can run your application as root, but this is **not recommended** for production:

```bash
sudo ./your_application
```

**Security Risk**: Running as root grants excessive privileges.

## Verification

After configuring permissions, verify that thread priorities are set correctly:

1. **Check Logs**: The SDK will log when thread priority is set:
   ```
   INFO RX thread priority set to MAX (realtime)
   ```

2. **Check System**: Use `chrt` or `top` to verify thread priorities:
   ```bash
   # Find your process PID
   ps aux | grep your_application

   # Check thread priorities
   chrt -p <PID>
   # Or use top/htop and check the PRI column
   ```

3. **Monitor Performance**: Use the SDK's metrics API to monitor latency:
   ```rust
   let metrics = robot.get_metrics();
   println!("RX timeouts: {}", metrics.rx_timeouts);
   println!("TX frames total: {}", metrics.tx_frames_total);
   ```

## Troubleshooting

### Warning: "Failed to set RX thread priority"

If you see this warning, it means the SDK could not set thread priority:

```
WARN Failed to set RX thread priority: Operation not permitted
```

**Solutions**:
1. Check if you have the required permissions (see above)
2. Verify the `realtime` feature is enabled in your build
3. Check system limits (e.g., `ulimit -r` for real-time priority limit)

**Note**: The application will continue to run with default thread priorities, but latency may be higher.

### Permission Denied After `setcap`

If `setcap` doesn't work:

1. **Check file system**: Some file systems (e.g., NFS, FUSE) don't support capabilities
2. **Check SELinux/AppArmor**: Security modules may block capability usage
3. **Verify executable**: Ensure the executable is not a script wrapper

### Thread Priority Not Visible in `top`

Thread priorities may not be visible in all tools. Use `chrt` or check `/proc/<PID>/task/<TID>/stat` for accurate information.

## Performance Tuning

### Thread Priority Levels

The SDK uses `ThreadPriority::Max` for RX threads when the `realtime` feature is enabled. This typically maps to:
- **Linux**: SCHED_FIFO with priority 99 (highest)
- **macOS/Windows**: Highest available priority

### System Configuration

For optimal performance, consider:

1. **CPU Isolation**: Isolate CPU cores for real-time threads
2. **IRQ Affinity**: Bind interrupt handlers to specific CPU cores
3. **Kernel Parameters**: Tune kernel parameters for low latency (e.g., `isolcpus`, `nohz_full`)

**Note**: System-level tuning is beyond the scope of this guide. Consult your Linux distribution's real-time tuning documentation.

## Example: Complete Setup

```bash
# 1. Build with realtime feature
cargo build --release --features realtime

# 2. Set capability
sudo setcap 'cap_sys_nice+ep' target/release/your_application

# 3. Run application
./target/release/your_application

# 4. Verify (in another terminal)
ps aux | grep your_application
chrt -p <PID>
```

## References

- [Linux Capabilities](https://man7.org/linux/man-pages/man7/capabilities.7.html)
- [RealtimeKit Documentation](https://www.freedesktop.org/software/realtimekit/)
- [thread-priority Crate](https://docs.rs/thread-priority/)

