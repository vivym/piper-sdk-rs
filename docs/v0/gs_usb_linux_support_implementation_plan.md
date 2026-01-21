# GS-USB Linux 支持实现方案

## 执行摘要

本文档是基于 `gs_usb_linux_conditional_compilation_analysis.md` 分析报告制定的**具体实施计划**。

**目标**：在 Linux 平台启用 GS-USB 支持，允许用户选择使用 SocketCAN 或 GS-USB 两种后端方案。

**推荐方案**：
- ✅ 方案 A：允许 Linux 同时支持两种方案（运行时选择）
- ✅ 启用 `vendored` 特性：静态编译 libusb，避免运行时依赖
- ✅ Smart Default 机制：自动选择后端，开箱即用
- ✅ udev 规则自动化：解决权限问题

**预计工作量**：2-3 天
**风险等级**：低（代码已支持，主要是移除条件编译限制）

---

## 1. 目标与范围

### 1.1 目标

1. **功能目标**：
   - Linux 平台可以编译和使用 GS-USB 适配器
   - Linux 平台可以编译和使用 GS-USB 守护进程
   - 通过 Smart Default 机制，自动选择合适的后端

2. **质量目标**：
   - 保持向后兼容（默认行为优先使用 SocketCAN）
   - CI/CD 环境无需额外配置即可编译
   - 生成的二进制文件无需系统依赖即可运行

3. **用户体验目标**：
   - 开箱即用，无需手动选择后端
   - 清晰的错误提示和文档
   - 一键安装 udev 规则

### 1.2 范围

**包含**：
- ✅ 修复条件编译限制
- ✅ 实现 Smart Default 机制
- ✅ 添加 udev 规则支持
- ✅ 更新文档和示例

**不包含**：
- ❌ 修改底层 GS-USB 协议实现（已正确支持 Linux）
- ❌ 性能优化（后续工作）
- ❌ Windows/macOS 的功能变更

---

## 2. 技术方案

### 2.1 核心策略

1. **移除条件编译限制**：
   - 删除 `src/can/mod.rs` 中的 `#[cfg(not(target_os = "linux"))]`
   - 允许 Linux 平台编译 `gs_usb` 模块

2. **启用 vendored 特性**：
   - 修改 `Cargo.toml`：`rusb = { version = "0.9.4", features = ["vendored"] }`
   - 静态编译 libusb，避免运行时依赖

3. **实现 Smart Default**：
   - Linux：接口名为 "can0"/"can1" 时优先 SocketCAN，其他情况使用 GS-USB
   - 自动降级：SocketCAN 不可用时 fallback 到 GS-USB
   - 显式控制：提供 `with_driver_type()` 方法

4. **udev 规则自动化**：
   - 提供标准 udev 规则文件
   - 提供安装脚本
   - 改进错误提示

### 2.2 架构设计

```
┌─────────────────────────────────────────┐
│         PiperBuilder (用户 API)          │
└─────────────────────────────────────────┘
                    │
        ┌───────────┴───────────┐
        │   Smart Default       │
        │   (运行时选择)         │
        └───────────┬───────────┘
                    │
    ┌───────────────┼───────────────┐
    │               │               │
┌───▼────┐   ┌──────▼──────┐   ┌───▼────────┐
│SocketCAN│   │  GS-USB     │   │GS-USB      │
│(Linux)  │   │  Direct     │   │Daemon      │
│         │   │(All Platform)│  │(All Platform)│
└─────────┘   └─────────────┘   └────────────┘
```

---

## 3. 详细实施步骤

### 阶段 1：依赖配置修复（P0 - 必须完成）

#### 步骤 1.1：修改 `Cargo.toml`

**文件**：`Cargo.toml`

**操作**：
```toml
# 修改前：
rusb = "0.9.4"

# 修改后：
rusb = { version = "0.9.4", features = ["vendored"] }
```

**验证**：
```bash
# 在干净的 Docker 环境中测试（无 libusb 开发包）
docker run --rm -v $(pwd):/work -w /work rust:latest \
  cargo build --target x86_64-unknown-linux-gnu

# 检查编译后的二进制文件依赖
ldd target/x86_64-unknown-linux-gnu/release/gs_usb_daemon
# 应该不包含 libusb-1.0.so
```

**验收标准**：
- ✅ CI 环境无需安装 `libusb-1.0-0-dev` 即可编译
- ✅ 二进制文件不依赖系统的 `libusb-1.0.so`

---

### 阶段 2：模块编译修复（P0 - 必须完成）

#### 步骤 2.1：修复 `src/can/mod.rs`

**文件**：`src/can/mod.rs`

**操作**：
```rust
// 删除以下行的 #[cfg(not(target_os = "linux"))] 属性：

// 修改前（第 68 行）：
#[cfg(not(target_os = "linux"))]
pub mod gs_usb;

// 修改后：
pub mod gs_usb;

// 修改前（第 72 行）：
#[cfg(not(target_os = "linux"))]
pub use gs_usb::GsUsbCanAdapter;

// 修改后：
pub use gs_usb::GsUsbCanAdapter;

// 修改前（第 79 行）：
#[cfg(not(target_os = "linux"))]
pub use gs_usb::split::{GsUsbRxAdapter, GsUsbTxAdapter};

// 修改后：
pub use gs_usb::split::{GsUsbRxAdapter, GsUsbTxAdapter};
```

**验证**：
```bash
cargo build --target x86_64-unknown-linux-gnu
# 应该成功编译，无错误
```

**验收标准**：
- ✅ Linux 平台可以编译 `gs_usb` 模块
- ✅ 所有导出类型在 Linux 上可用

---

### 阶段 3：Builder 模式修复（P0 - 必须完成）

#### 步骤 3.1：修复导入语句

**文件**：`src/robot/builder.rs`

**操作**：
```rust
// 修改前（第 7-10 行）：
#[cfg(target_os = "linux")]
use crate::can::SocketCanAdapter;
#[cfg(not(target_os = "linux"))]
use crate::can::gs_usb::GsUsbCanAdapter;
#[cfg(not(target_os = "linux"))]
use crate::can::gs_usb_udp::GsUsbUdpAdapter;

// 修改后：
#[cfg(target_os = "linux")]
use crate::can::SocketCanAdapter;
use crate::can::gs_usb::GsUsbCanAdapter;
use crate::can::gs_usb_udp::GsUsbUdpAdapter;
```

#### 步骤 3.2：添加 DriverType 枚举

**位置**：`src/robot/builder.rs`，在 `PiperBuilder` 结构体之前

**操作**：
```rust
/// 驱动类型选择
#[derive(Debug, Clone, Copy)]
pub enum DriverType {
    /// 自动探测（默认）
    /// - Linux: 如果 interface 是 "can0"/"can1" 等，使用 SocketCAN；否则尝试 GS-USB
    /// - 其他平台: 使用 GS-USB
    Auto,
    /// 强制使用 SocketCAN（仅 Linux）
    SocketCan,
    /// 强制使用 GS-USB（所有平台）
    GsUsb,
}
```

#### 步骤 3.3：修改 PiperBuilder 结构体

**位置**：`src/robot/builder.rs`

**操作**：
```rust
pub struct PiperBuilder {
    /// CAN 接口名称（Linux: "can0", macOS/Windows: 用作设备序列号）
    interface: Option<String>,
    /// CAN 波特率（1M, 500K, 250K 等）
    baud_rate: Option<u32>,
    /// Pipeline 配置
    pipeline_config: Option<PipelineConfig>,
    /// 守护进程地址（如果设置，使用守护进程模式）
    daemon_addr: Option<String>,
    /// 驱动类型选择（新增）
    driver_type: DriverType,  // 新增字段
}

impl PiperBuilder {
    pub fn new() -> Self {
        Self {
            interface: None,
            baud_rate: None,
            pipeline_config: None,
            daemon_addr: None,
            driver_type: DriverType::Auto,  // 默认 Auto
        }
    }

    /// 显式指定驱动类型（可选，默认 Auto）
    pub fn with_driver_type(mut self, driver_type: DriverType) -> Self {
        self.driver_type = driver_type;
        self
    }

    // ... 其他方法保持不变
}
```

#### 步骤 3.4：修复 `with_daemon` 方法

**位置**：`src/robot/builder.rs` 第 137 行

**操作**：
```rust
// 修改前：
#[cfg(not(target_os = "linux"))]
pub fn with_daemon(mut self, daemon_addr: impl Into<String>) -> Self {
    self.daemon_addr = Some(daemon_addr.into());
    self
}

// 修改后（移除 cfg 限制）：
pub fn with_daemon(mut self, daemon_addr: impl Into<String>) -> Self {
    self.daemon_addr = Some(daemon_addr.into());
    self
}
```

#### 步骤 3.5：重构 `build` 方法

**位置**：`src/robot/builder.rs` 第 165 行

**操作**：完全重写 `build` 方法，实现 Smart Default 逻辑

**完整代码**：
```rust
pub fn build(self) -> Result<Piper, RobotError> {
    // 1. 守护进程模式（所有平台，优先级最高）
    if let Some(daemon_addr) = self.daemon_addr {
        return self.build_gs_usb_daemon(daemon_addr);
    }

    // 2. 根据 driver_type 和 interface 自动选择后端
    match self.driver_type {
        DriverType::Auto => {
            // Linux: Smart Default 逻辑
            #[cfg(target_os = "linux")]
            {
                if let Some(ref interface) = self.interface {
                    // 如果接口名是 "can0", "can1" 等，尝试 SocketCAN
                    if interface.starts_with("can") && interface.len() <= 5 {
                        // 尝试 SocketCAN（可能失败，例如接口不存在）
                        if let Ok(piper) = self.build_socketcan(interface.as_str()) {
                            return Ok(piper);
                        }
                        // 如果 SocketCAN 失败，fallback 到 GS-USB
                        tracing::info!(
                            "SocketCAN interface '{}' not available, falling back to GS-USB",
                            interface
                        );
                    }
                }
                // 其他情况（未指定接口、USB 总线号等）：使用 GS-USB
                self.build_gs_usb_direct()
            }

            // 其他平台：默认使用 GS-USB
            #[cfg(not(target_os = "linux"))]
            {
                self.build_gs_usb_direct()
            }
        }
        DriverType::SocketCan => {
            #[cfg(target_os = "linux")]
            {
                let interface = self.interface.as_deref().unwrap_or("can0");
                self.build_socketcan(interface)
            }
            #[cfg(not(target_os = "linux"))]
            {
                Err(RobotError::Can(CanError::Device(
                    CanDeviceError::new(
                        CanDeviceErrorKind::UnsupportedConfig,
                        "SocketCAN is only available on Linux"
                    )
                )))
            }
        }
        DriverType::GsUsb => {
            self.build_gs_usb_direct()
        }
    }
}

/// 构建 SocketCAN 适配器（Linux only）
#[cfg(target_os = "linux")]
fn build_socketcan(&self, interface: &str) -> Result<Piper, RobotError> {
    let mut can = SocketCanAdapter::new(interface).map_err(RobotError::Can)?;

    // SocketCAN 的波特率由系统配置，但可以调用 configure 验证接口状态
    if let Some(bitrate) = self.baud_rate {
        can.configure(bitrate).map_err(RobotError::Can)?;
    }

    let config = self.pipeline_config.clone().unwrap_or_default();
    can.set_read_timeout(std::time::Duration::from_millis(config.receive_timeout_ms))
        .map_err(RobotError::Can)?;

    Piper::new(can, self.pipeline_config).map_err(RobotError::Can)
}

/// 构建 GS-USB 直连适配器
fn build_gs_usb_direct(&self) -> Result<Piper, RobotError> {
    // interface 可能是：
    // - 设备序列号（如 "ABC123456"）
    // - USB 总线号（如 "1:12"，表示 bus 1, address 12）
    // - None（自动选择第一个设备）

    let mut can = match &self.interface {
        Some(serial) if serial.contains(':') => {
            // USB 总线号格式：bus:address
            let parts: Vec<&str> = serial.split(':').collect();
            if parts.len() == 2 {
                if let (Ok(bus), Ok(addr)) = (parts[0].parse::<u8>(), parts[1].parse::<u8>()) {
                    use crate::can::gs_usb::device::GsUsbDeviceSelector;
                    let selector = GsUsbDeviceSelector::by_bus_address(bus, addr);
                    let device = crate::can::gs_usb::device::GsUsbDevice::open(&selector)
                        .map_err(|e| RobotError::Can(CanError::Device(
                            CanDeviceError::new(
                                CanDeviceErrorKind::Backend,
                                format!("Failed to open GS-USB device at {}:{}: {}", bus, addr, e)
                            )
                        )))?;
                    // 注意：这里需要从 device 创建 adapter
                    // 简化处理：暂时 fallback 到序列号方式
                    GsUsbCanAdapter::new_with_serial(Some(serial.as_str()))
                        .map_err(RobotError::Can)?
                } else {
                    GsUsbCanAdapter::new_with_serial(Some(serial.as_str()))
                        .map_err(RobotError::Can)?
                }
            } else {
                GsUsbCanAdapter::new_with_serial(Some(serial.as_str()))
                    .map_err(RobotError::Can)?
            }
        }
        Some(serial) => {
            GsUsbCanAdapter::new_with_serial(Some(serial.as_str()))
                .map_err(RobotError::Can)?
        }
        None => {
            GsUsbCanAdapter::new().map_err(RobotError::Can)?
        }
    };

    let bitrate = self.baud_rate.unwrap_or(1_000_000);
    can.configure(bitrate).map_err(RobotError::Can)?;

    let config = self.pipeline_config.clone().unwrap_or_default();
    can.set_receive_timeout(std::time::Duration::from_millis(config.receive_timeout_ms));

    Piper::new(can, self.pipeline_config).map_err(RobotError::Can)
}

/// 构建 GS-USB 守护进程适配器
fn build_gs_usb_daemon(&self, daemon_addr: String) -> Result<Piper, RobotError> {
    let mut can = if daemon_addr.starts_with('/') || daemon_addr.starts_with("unix:") {
        // UDS 模式
        let path = daemon_addr.strip_prefix("unix:").unwrap_or(&daemon_addr);
        GsUsbUdpAdapter::new_uds(path).map_err(RobotError::Can)?
    } else {
        // UDP 模式
        GsUsbUdpAdapter::new_udp(&daemon_addr).map_err(RobotError::Can)?
    };

    // 连接到守护进程（使用空的过滤规则，接收所有帧）
    can.connect(vec![]).map_err(RobotError::Can)?;

    Piper::new(can, self.pipeline_config).map_err(RobotError::Can)
}
```

**验证**：
```bash
# 编译测试
cargo build --target x86_64-unknown-linux-gnu

# 运行单元测试
cargo test --target x86_64-unknown-linux-gnu
```

**验收标准**：
- ✅ Linux 平台可以使用 GS-USB 适配器
- ✅ Smart Default 机制正常工作
- ✅ 守护进程模式在 Linux 上可用

---

### 阶段 4：设备层改进（P1 - 重要）

#### 步骤 4.1：添加 detach 日志提示

**文件**：`src/can/gs_usb/device.rs`

**位置**：第 375-396 行（`start` 方法中）

**操作**：
```rust
// 在 detach_kernel_driver 之前添加日志
#[cfg(any(target_os = "linux", target_os = "macos"))]
{
    let kernel_driver_active =
        self.handle.kernel_driver_active(self.interface_number).unwrap_or(false);

    if kernel_driver_active {
        tracing::info!(
            "Detaching kernel driver for GS-USB device to enable userspace mode. \
             Note: CAN network interface (can0) will temporarily disappear."
        );
        self.interface_claimed = false;
        self.handle
            .detach_kernel_driver(self.interface_number)
            .map_err(GsUsbError::Usb)?;
    }
    // ... 其余代码保持不变
}
```

#### 步骤 4.2：改进权限错误提示

**文件**：`src/can/gs_usb/device.rs`

**位置**：`open` 方法中（第 106 行附近）

**操作**：在错误处理中添加友好的提示

```rust
// 在 GsUsbError 转换为 CanError 的地方
match e {
    crate::can::gs_usb::error::GsUsbError::Usb(rusb::Error::Access) => {
        let error_msg = format!(
            "Permission denied accessing GS-USB device. \
             Please install udev rules: sudo cp scripts/99-piper-gs-usb.rules /etc/udev/rules.d/ && \
             sudo udevadm control --reload-rules && sudo udevadm trigger. \
             See docs/v0/gs_usb_linux_conditional_compilation_analysis.md for details."
        );
        Err(CanError::Device(CanDeviceError::new(
            CanDeviceErrorKind::AccessDenied,
            error_msg
        )))
    }
    // ... 其他错误处理
}
```

**验证**：
```bash
# 在无权限的环境下测试（应该看到友好的错误提示）
cargo run --example gs_usb_direct_test
```

---

### 阶段 5：udev 规则支持（P1 - 重要）

#### 步骤 5.1：创建 udev 规则文件

**文件**：`scripts/99-piper-gs-usb.rules`（新建）

**内容**：
```bash
# GS-USB devices (VID:PID pairs)
# GS-USB: 0x1D50:0x606F
# Candlelight: 0x1209:0x2323
# CES CANext FD: 0x1CD2:0x606F
# ABE CANdebugger FD: 0x16D0:0x10B8

SUBSYSTEM=="usb", ATTRS{idVendor}=="1d50", ATTRS{idProduct}=="606f", MODE="0664", GROUP="plugdev", SYMLINK+="gs_usb_%n"
SUBSYSTEM=="usb", ATTRS{idVendor}=="1209", ATTRS{idProduct}=="2323", MODE="0664", GROUP="plugdev", SYMLINK+="candlelight_%n"
SUBSYSTEM=="usb", ATTRS{idVendor}=="1cd2", ATTRS{idProduct}=="606f", MODE="0664", GROUP="plugdev", SYMLINK+="canext_fd_%n"
SUBSYSTEM=="usb", ATTRS{idVendor}=="16d0", ATTRS{idProduct}=="10b8", MODE="0664", GROUP="plugdev", SYMLINK+="candebugger_fd_%n"
```

#### 步骤 5.2：创建安装脚本

**文件**：`scripts/install-udev-rules.sh`（新建）

**内容**：
```bash
#!/bin/bash
set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RULES_FILE="${SCRIPT_DIR}/99-piper-gs-usb.rules"
TARGET="/etc/udev/rules.d/99-piper-gs-usb.rules"

if [ ! -f "$RULES_FILE" ]; then
    echo "Error: Rules file not found: $RULES_FILE"
    exit 1
fi

echo "Installing udev rules for GS-USB devices..."
sudo cp "$RULES_FILE" "$TARGET"
sudo chmod 644 "$TARGET"

echo "Reloading udev rules..."
sudo udevadm control --reload-rules
sudo udevadm trigger

echo "Done! You may need to unplug and replug your GS-USB device."
echo ""
echo "To add your user to the plugdev group (if not already):"
echo "  sudo usermod -aG plugdev $USER"
echo "  (You may need to log out and log back in for this to take effect)"
```

**设置执行权限**：
```bash
chmod +x scripts/install-udev-rules.sh
```

**验证**：
```bash
# 测试安装脚本（需要 sudo 权限）
./scripts/install-udev-rules.sh

# 验证规则已安装
ls -l /etc/udev/rules.d/99-piper-gs-usb.rules

# 检查用户组
groups | grep plugdev
```

---

### 阶段 6：测试用例修复（P1 - 重要）

#### 步骤 6.1：修复 `tests/gs_usb_stage1_loopback_tests.rs`

**操作**：
```rust
// 修改前（第 29 行）：
#[cfg(not(target_os = "linux"))]
mod tests {

// 修改后：
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
mod tests {
```

**注意**：每个测试函数上的 `#[cfg(not(target_os = "linux"))]` 也需要移除。

#### 步骤 6.2：修复其他测试文件

同样修复以下文件：
- `tests/gs_usb_performance_tests.rs`
- `tests/gs_usb_integration_tests.rs`

**验证**：
```bash
cargo test --target x86_64-unknown-linux-gnu gs_usb
```

---

### 阶段 7：示例代码修复（P2 - 可选）

#### 步骤 7.1：修复示例文件

修复以下文件中的条件编译限制：
- `examples/timestamp_verification.rs`
- `examples/robot_monitor.rs`
- `examples/iface_check.rs`

**操作**：移除 `#[cfg(not(target_os = "linux"))]` 限制，或在 Linux 上提供替代实现。

---

### 阶段 8：文档更新（P1 - 重要）

#### 步骤 8.1：更新 README.md

**位置**：`README.md`

**操作**：
1. 更新跨平台支持说明：
   ```markdown
   - 🌍 **Cross-Platform Support (Linux/Windows/macOS)**:
     - **Linux**: Supports both SocketCAN (kernel-level) and GS-USB (userspace via libusb)
     - **Windows/macOS**: GS-USB driver implementation using `rusb` (driver-free/universal)
   ```

2. 添加 Smart Default 说明：
   ```markdown
   ### Backend Selection

   On Linux, the SDK automatically selects the appropriate backend:
   - If interface name is "can0"/"can1" etc., SocketCAN is preferred
   - Otherwise, GS-USB is used
   - You can explicitly specify backend using `with_driver_type()`
   ```

3. 添加 udev 规则安装说明：
   ```markdown
   ### Linux Permissions Setup

   To use GS-USB on Linux, install udev rules:

   ```bash
   sudo cp scripts/99-piper-gs-usb.rules /etc/udev/rules.d/
   sudo udevadm control --reload-rules && sudo udevadm trigger
   ```

   Or use the installation script:
   ```bash
   ./scripts/install-udev-rules.sh
   ```
   ```

#### 步骤 8.2：更新 API 文档

确保 `DriverType` 和 `with_driver_type()` 方法的文档注释完整。

---

## 4. 验证计划

### 4.1 编译验证

| 测试项 | 命令 | 预期结果 |
|--------|------|---------|
| Linux 编译 | `cargo build --target x86_64-unknown-linux-gnu` | ✅ 成功 |
| macOS 编译 | `cargo build --target x86_64-apple-darwin` | ✅ 成功 |
| Windows 编译 | `cargo build --target x86_64-pc-windows-msvc` | ✅ 成功 |
| CI 环境编译 | Docker 容器（无 libusb） | ✅ 成功 |
| 依赖检查 | `ldd target/.../gs_usb_daemon` | ✅ 无 libusb 依赖 |

### 4.2 功能验证

| 测试项 | 步骤 | 预期结果 |
|--------|------|---------|
| Smart Default (Linux) | `PiperBuilder::new().interface("can0").build()` | ✅ 使用 SocketCAN |
| Smart Default (Linux) | `PiperBuilder::new().interface("ABC123").build()` | ✅ 使用 GS-USB |
| 显式指定 | `PiperBuilder::new().with_driver_type(DriverType::GsUsb).build()` | ✅ 使用 GS-USB |
| 守护进程 | `PiperBuilder::new().with_daemon("/tmp/sock").build()` | ✅ 使用守护进程 |
| 自动降级 | SocketCAN 不可用时 | ✅ 自动 fallback 到 GS-USB |

### 4.3 运行时验证

| 测试项 | 步骤 | 预期结果 |
|--------|------|---------|
| 无权限提示 | 无 udev 规则时访问设备 | ✅ 友好的错误提示 |
| Kernel driver detach | 有内核驱动时使用 GS-USB | ✅ 自动 detach，有日志提示 |
| 权限修复 | 安装 udev 规则后 | ✅ 可以正常访问设备 |

### 4.4 回归测试

| 测试项 | 步骤 | 预期结果 |
|--------|------|---------|
| 向后兼容 | 现有代码无需修改 | ✅ 行为保持一致 |
| macOS/Windows | 现有功能 | ✅ 不受影响 |
| SocketCAN (Linux) | 现有功能 | ✅ 不受影响 |

---

## 5. 风险评估与缓解

### 5.1 技术风险

| 风险项 | 严重程度 | 可能性 | 缓解措施 | 状态 |
|--------|---------|--------|---------|------|
| 编译失败 | 🟢 低 | 低 | 启用 vendored 特性，逐步测试 | 已处理 |
| 运行时依赖 | 🟢 低 | 低 | vendored 特性静态链接 | 已处理 |
| Kernel driver 冲突 | 🟡 中 | 中 | 代码已处理 detach，添加日志 | 已处理 |
| 权限问题 | 🟡 中 | 高 | 提供 udev 规则和安装脚本 | 已处理 |
| Smart Default 逻辑错误 | 🟡 中 | 低 | 充分测试，提供显式覆盖 | 测试中 |

### 5.2 兼容性风险

| 风险项 | 影响 | 缓解措施 |
|--------|------|---------|
| API 变更 | 无 | 保持现有 API，只添加新方法 |
| 默认行为变更 | 低 | 保持向后兼容，Linux 仍优先 SocketCAN |
| 二进制兼容性 | 无 | 不涉及 ABI 变更 |

### 5.3 回滚计划

如果出现严重问题，可以快速回滚：

1. **Git 回滚**：
   ```bash
   git revert <commit-hash>
   ```

2. **部分回滚**：
   - 保留 `vendored` 特性（解决依赖问题）
   - 恢复条件编译限制（暂时禁用 Linux GS-USB 支持）

3. **功能开关**：
   - 通过 feature flag 控制（如果采用了方案 B）

---

## 6. 时间估算

| 阶段 | 任务 | 预计时间 | 优先级 |
|------|------|---------|--------|
| 阶段 1 | 依赖配置修复 | 30 分钟 | P0 |
| 阶段 2 | 模块编译修复 | 30 分钟 | P0 |
| 阶段 3 | Builder 模式修复 | 2-3 小时 | P0 |
| 阶段 4 | 设备层改进 | 1 小时 | P1 |
| 阶段 5 | udev 规则支持 | 1 小时 | P1 |
| 阶段 6 | 测试用例修复 | 1 小时 | P1 |
| 阶段 7 | 示例代码修复 | 1 小时 | P2 |
| 阶段 8 | 文档更新 | 1-2 小时 | P1 |
| 总计 | | **8-10 小时** | |

**预计完成时间**：1-2 个工作日

---

## 7. 验收标准

### 7.1 功能验收

- [x] Linux 平台可以编译 `gs_usb` 模块
- [ ] Linux 平台可以使用 GS-USB 适配器
- [ ] Linux 平台可以使用 GS-USB 守护进程
- [ ] Smart Default 机制正常工作
- [ ] 守护进程模式在 Linux 上可用

### 7.2 质量验收

- [ ] CI 环境无需额外配置即可编译
- [ ] 二进制文件无需系统依赖即可运行
- [ ] 所有测试用例通过
- [ ] 向后兼容性验证通过

### 7.3 用户体验验收

- [ ] 开箱即用（Smart Default 正常工作）
- [ ] 友好的错误提示（权限、设备未找到等）
- [ ] udev 规则一键安装
- [ ] 文档完整清晰

---

## 8. 后续工作

### 8.1 性能优化（后续）

- [ ] SocketCAN vs GS-USB 性能对比测试
- [ ] 优化 GS-USB 在 Linux 上的性能（如果需要）

### 8.2 功能增强（后续）

- [ ] 支持 USB 总线号格式的接口选择
- [ ] 改进 Smart Default 的探测逻辑
- [ ] 添加更多测试用例

### 8.3 文档完善（后续）

- [ ] 添加 Linux 使用场景说明
- [ ] 添加故障排除指南
- [ ] 添加性能对比数据

---

## 附录 A：代码修改清单

### A.1 需要修改的文件

| 文件 | 修改类型 | 行号/位置 | 优先级 |
|------|---------|----------|--------|
| `Cargo.toml` | 启用 vendored | 第 19 行 | P0 |
| `src/can/mod.rs` | 删除 cfg | 第 68, 72, 79 行 | P0 |
| `src/robot/builder.rs` | 重构 build 方法 | 多处 | P0 |
| `src/can/gs_usb/device.rs` | 添加日志 | 第 375 行 | P1 |
| `tests/gs_usb_*.rs` | 删除 cfg | 多处 | P1 |
| `examples/*.rs` | 删除 cfg | 多处 | P2 |
| `README.md` | 更新文档 | 多处 | P1 |

### A.2 需要创建的文件

| 文件 | 类型 | 优先级 |
|------|------|--------|
| `scripts/99-piper-gs-usb.rules` | udev 规则 | P1 |
| `scripts/install-udev-rules.sh` | 安装脚本 | P1 |

---

## 附录 B：测试命令清单

```bash
# 1. 编译验证
cargo build --target x86_64-unknown-linux-gnu
cargo build --target x86_64-apple-darwin
cargo build --target x86_64-pc-windows-msvc

# 2. CI 环境验证（无 libusb）
docker run --rm -v $(pwd):/work -w /work rust:latest \
  cargo build --target x86_64-unknown-linux-gnu

# 3. 依赖检查
ldd target/x86_64-unknown-linux-gnu/release/gs_usb_daemon

# 4. 单元测试
cargo test --target x86_64-unknown-linux-gnu

# 5. GS-USB 测试
cargo test --target x86_64-unknown-linux-gnu gs_usb

# 6. 示例测试
cargo run --example gs_usb_direct_test --target x86_64-unknown-linux-gnu

# 7. 守护进程测试
cargo build --bin gs_usb_daemon --target x86_64-unknown-linux-gnu
cargo run --bin gs_usb_daemon --target x86_64-unknown-linux-gnu

# 8. udev 规则安装
./scripts/install-udev-rules.sh
```

---

**文档版本**：v1.0
**创建日期**：2024
**基于分析报告**：`gs_usb_linux_conditional_compilation_analysis.md`
**状态**：待实施

