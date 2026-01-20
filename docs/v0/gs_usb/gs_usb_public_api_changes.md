# GS-USB 重构：Public API 变更清单与迁移指南

## 1. 设备枚举/打开（两段式）

### 变更点

- 删除：`GsUsbDevice::scan()`、`GsUsbDevice::scan_with_filter(...)`
- 新增：
  - `GsUsbDevice::scan_info()` / `scan_info_with_filter(...)`（轻量枚举，不持有 handle）
  - `GsUsbDeviceSelector` + `GsUsbDevice::open(&selector)`（真正打开并持有 handle）

### 迁移建议

- 仅列设备/打印信息：使用 `scan_info()`。
- 真正要打开设备：先 `scan_info` 做决策，再 `open(selector)` 打开。

## 2. 历史遗留接口清理

已删除（避免误用/无调用点）：

- `send_host_format()`
- `prepare_interface()`
- `clear_usb_endpoints()`
- `receive_raw()`

## 3. 接收超时与批量接收

- `GsUsbCanAdapter`：
  - 新增：`set_receive_timeout(Duration)`（默认 50ms）
  - 新增：`receive_batch_frames()`（一次读取并返回多帧，便于后续优化锁竞争/吞吐）

## 4. 错误域结构化

- `CanError::Device(String)` 升级为 `CanError::Device(CanDeviceError{ kind, message })`。
- 上层如需要取具体错误字符串，请改为读取 `message` 字段。


