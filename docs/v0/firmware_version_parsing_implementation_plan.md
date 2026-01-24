# 固件版本解析实现方案报告

## 1. 调研总结

### 1.1 Python SDK 实现分析

#### 1.1.1 数据结构
- **存储方式**：使用 `bytearray()` 累积固件数据
- **线程安全**：使用 `threading.Lock()` 保护数据访问
- **初始化**：在 `__init__` 中初始化为空 `bytearray()`

```python
# 位置：piper_interface_v2.py:482-483
self.__firmware_data_mtx = threading.Lock()
self.__firmware_data = bytearray()
```

#### 1.1.2 数据累积逻辑
- **更新方法**：`__UpdatePiperFirmware(msg: PiperMessage)`
- **累积方式**：直接将新数据追加到现有数据
- **触发条件**：收到 `ArmMsgType.PiperMsgFirmwareRead` 类型的消息

```python
# 位置：piper_interface_v2.py:2334-2344
def __UpdatePiperFirmware(self, msg:PiperMessage):
    with self.__firmware_data_mtx:
        if(msg.type_ == ArmMsgType.PiperMsgFirmwareRead):
            self.__firmware_data = self.__firmware_data + msg.firmware_data
        return self.__firmware_data
```

#### 1.1.3 版本解析逻辑
- **解析方法**：`GetPiperFirmwareVersion()`
- **查找标记**：`b'S-V'`（3 字节）
- **提取长度**：固定 8 字节（从 S-V 开始，包括 S-V）
- **解码方式**：`decode('utf-8', errors='ignore')`
- **失败返回值**：`-0x4AF`（CAN ID 的负值）

```python
# 位置：piper_interface_v2.py:1589-1613
def GetPiperFirmwareVersion(self):
    with self.__firmware_data_mtx:
        # 查找固件版本信息
        version_start = self.__firmware_data.find(b'S-V')
        if version_start == -1:
            return -0x4AF  # 没有找到以 S-V 开头的字符串
        # 固定长度为 8
        version_length = 8
        # 确保不会超出 bytearray 的长度
        version_end = min(version_start + version_length, len(self.__firmware_data))
        # 提取版本信息，截取固定长度的字节数据
        firmware_version = self.__firmware_data[version_start:version_end].decode('utf-8', errors='ignore')
        return firmware_version  # 返回找到的固件版本字符串
```

**关键点**：
- 固定长度为 8 字节（包括 "S-V" 前缀）
- 如果找不到 `S-V` 标记，返回 `-0x4AF`
- 使用 `errors='ignore'` 处理无效 UTF-8 字符

#### 1.1.4 查询命令
- **查询方法**：`SearchPiperFirmwareVersion()`
- **CAN ID**：`0x4AF`
- **数据负载**：`[0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]`
- **清空数据**：发送查询前清空累积数据

```python
# 位置：piper_interface_v2.py:3511-3530
def SearchPiperFirmwareVersion(self):
    tx_can = Message()
    tx_can.arbitration_id = 0x4AF
    tx_can.data = [0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]
    feedback = self.__arm_can.SendCanMessage(tx_can.arbitration_id, tx_can.data)
    if feedback is not self.__arm_can.CAN_STATUS.SEND_MESSAGE_SUCCESS:
        self.logger.error("SearchPiperFirmwareVersion send failed: SendCanMessage(%s)", feedback)
    self.__firmware_data = bytearray()  # 清空数据
```

#### 1.1.5 使用示例
```python
# 位置：piper_read_firmware.py
piper = C_PiperInterface_V2()
piper.ConnectPort()
time.sleep(0.03)  # 需要时间去读取固件反馈帧，否则会反馈-0x4AF
print(piper.GetPiperFirmwareVersion())
```

**关键观察**：
- 需要等待时间让固件数据累积（通常 0.03 秒）
- 如果数据不完整，会返回 `-0x4AF`

### 1.2 Rust SDK 当前实现分析

#### 1.2.1 数据结构
```rust
// 位置：src/robot/state.rs:673-691
#[derive(Debug, Clone, Default)]
pub struct FirmwareVersionState {
    pub hardware_timestamp_us: u64,
    pub system_timestamp_us: u64,
    pub firmware_data: Vec<u8>,
    pub is_complete: bool,  // TODO: 未实现
    pub version_string: Option<String>,
}
```

#### 1.2.2 数据累积逻辑
```rust
// 位置：src/robot/pipeline.rs:727-740
if let Ok(mut firmware_state) = ctx.firmware_version.write() {
    // 累积数据
    firmware_state.firmware_data.extend_from_slice(feedback.firmware_data());

    // 更新时间戳
    firmware_state.hardware_timestamp_us = frame.timestamp_us;
    firmware_state.system_timestamp_us = system_timestamp_us;

    // 尝试解析版本字符串
    firmware_state.parse_version();

    // TODO: 判断数据是否完整的逻辑
    // firmware_state.is_complete = ...
}
```

#### 1.2.3 版本解析逻辑（当前实现）
```rust
// 位置：src/protocol/feedback.rs:2380-2396
pub fn parse_version_string(accumulated_data: &[u8]) -> Option<String> {
    // 查找 "S-V" 标记
    if let Some(version_start) = accumulated_data.windows(3).position(|w| w == b"S-V") {
        // 从 "S-V" 后开始，查找版本字符串的结束位置（通常是换行符或字符串结束）
        let version_start = version_start + 3;
        let version_end = accumulated_data[version_start..]
            .iter()
            .position(|&b| b == b'\n' || b == b'\r' || b == 0)
            .map(|pos| version_start + pos)
            .unwrap_or(accumulated_data.len().min(version_start + 20)); // 最多读取20个字符

        let version_bytes = &accumulated_data[version_start..version_end];
        String::from_utf8(version_bytes.to_vec()).ok().map(|s| s.trim().to_string())
    } else {
        None
    }
}
```

#### 1.2.4 问题分析

**与 Python SDK 的差异**：

1. **解析长度不一致**：
   - Python SDK：固定 8 字节（从 S-V 开始，包括 S-V）
   - Rust SDK：从 S-V 后开始，查找换行符或最多 20 字符

2. **缺少数据清空机制**：
   - Python SDK：在 `SearchPiperFirmwareVersion()` 时清空数据
   - Rust SDK：没有查询命令接口，也没有清空机制

3. **完整性判断未实现**：
   - Python SDK：没有明确的完整性判断，只是查找 S-V 标记
   - Rust SDK：有 `is_complete` 字段但未实现

4. **错误处理不一致**：
   - Python SDK：找不到时返回 `-0x4AF`
   - Rust SDK：找不到时返回 `None`

## 2. 实现方案

### 2.1 目标

1. **对齐 Python SDK 的解析逻辑**：固定 8 字节长度（包括 S-V）
2. **实现数据清空机制**：在查询时清空累积数据
3. **实现完整性判断**：基于是否找到 S-V 标记和是否有足够数据
4. **保持 Rust 风格**：使用 `Option<String>` 而不是负数错误码

### 2.2 修改计划

#### 2.2.1 修改 `parse_version_string` 方法

**位置**：`src/protocol/feedback.rs`

**修改内容**：
- 将解析逻辑改为固定 8 字节长度（从 S-V 开始，包括 S-V）
- 与 Python SDK 保持一致

**新实现**：
```rust
pub fn parse_version_string(accumulated_data: &[u8]) -> Option<String> {
    // 查找 "S-V" 标记
    if let Some(version_start) = accumulated_data.windows(3).position(|w| w == b"S-V") {
        // 固定长度为 8 字节（从 S-V 开始，包括 S-V）
        let version_length = 8;
        // 确保不会超出数组长度
        let version_end = (version_start + version_length).min(accumulated_data.len());

        // 提取版本信息，截取固定长度的字节数据
        let version_bytes = &accumulated_data[version_start..version_end];

        // 使用 UTF-8 解码，忽略错误（与 Python SDK 的 errors='ignore' 对应）
        String::from_utf8_lossy(version_bytes).trim().to_string().into()
    } else {
        None
    }
}
```

**注意**：
- 使用 `from_utf8_lossy` 而不是 `from_utf8`，以处理无效 UTF-8 字符（对应 Python 的 `errors='ignore'`）
- 固定长度为 8 字节，与 Python SDK 一致
- 返回 `Option<String>` 而不是负数错误码（Rust 风格）

#### 2.2.2 增强 `FirmwareVersionState` 方法

**位置**：`src/robot/state.rs`

**新增方法**：

1. **清空数据方法**：
```rust
impl FirmwareVersionState {
    /// 清空累积的固件数据（用于开始新的查询）
    pub fn clear(&mut self) {
        self.firmware_data.clear();
        self.version_string = None;
        self.is_complete = false;
        self.hardware_timestamp_us = 0;
        self.system_timestamp_us = 0;
    }

    /// 检查数据是否完整（是否找到 S-V 标记且有足够数据）
    pub fn check_completeness(&mut self) -> bool {
        if self.firmware_data.windows(3).any(|w| w == b"S-V") {
            // 找到 S-V 标记，检查是否有足够的数据（至少 8 字节）
            if let Some(version_start) = self.firmware_data.windows(3).position(|w| w == b"S-V") {
                let required_length = version_start + 8;
                self.is_complete = self.firmware_data.len() >= required_length;
            } else {
                self.is_complete = false;
            }
        } else {
            self.is_complete = false;
        }
        self.is_complete
    }
}
```

2. **修改 `parse_version` 方法**：
```rust
impl FirmwareVersionState {
    /// 尝试从累积数据中解析版本字符串
    pub fn parse_version(&mut self) -> Option<String> {
        use crate::protocol::feedback::FirmwareReadFeedback;
        if let Some(version) = FirmwareReadFeedback::parse_version_string(&self.firmware_data) {
            self.version_string = Some(version.clone());
            // 同时更新完整性状态
            self.check_completeness();
            Some(version)
        } else {
            self.version_string = None;
            self.is_complete = false;
            None
        }
    }
}
```

#### 2.2.3 更新 pipeline 中的处理逻辑

**位置**：`src/robot/pipeline.rs`

**修改内容**：
- 在解析版本后更新 `is_complete` 状态
- 添加注释说明数据累积逻辑

```rust
ID_FIRMWARE_READ => {
    // FirmwareReadFeedback (0x4AF) - 累积固件版本数据
    if let Ok(feedback) = FirmwareReadFeedback::try_from(frame) {
        let system_timestamp_us = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_micros() as u64;

        if let Ok(mut firmware_state) = ctx.firmware_version.write() {
            // 累积数据
            firmware_state.firmware_data.extend_from_slice(feedback.firmware_data());

            // 更新时间戳
            firmware_state.hardware_timestamp_us = frame.timestamp_us;
            firmware_state.system_timestamp_us = system_timestamp_us;

            // 尝试解析版本字符串（会自动更新 is_complete）
            firmware_state.parse_version();
        }

        ctx.fps_stats
            .load()
            .firmware_version_updates
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        trace!("FirmwareVersionState updated");
    }
},
```

#### 2.2.4 添加查询命令接口（可选）

如果需要实现完整的查询功能，可以在高等级 API 中添加查询方法。但这不是本次任务的重点，可以后续实现。

**建议位置**：`src/high_level/client/mod.rs` 或新建 `src/high_level/commands.rs`

**接口设计**：
```rust
impl Robot {
    /// 查询固件版本
    ///
    /// 发送查询命令并清空之前的累积数据
    pub fn query_firmware_version(&self) -> Result<(), Error> {
        // 1. 清空累积数据
        if let Ok(mut firmware_state) = self.ctx.firmware_version.write() {
            firmware_state.clear();
        }

        // 2. 发送查询命令（CAN ID: 0x4AF, Data: [0x01, 0x00, ...]）
        // TODO: 需要实现命令发送接口
        // self.send_command(ID_FIRMWARE_READ, &[0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00])?;

        Ok(())
    }

    /// 获取固件版本字符串
    pub fn get_firmware_version(&self) -> Option<String> {
        self.ctx.firmware_version.read()
            .ok()
            .and_then(|state| state.version_string.clone())
    }
}
```

### 2.3 测试计划

#### 2.3.1 单元测试

**位置**：`src/protocol/feedback.rs` 的测试模块

**测试用例**：

1. **测试固定 8 字节长度解析**：
```rust
#[test]
fn test_firmware_read_feedback_parse_version_string_fixed_length() {
    // 测试固定 8 字节长度（包括 S-V）
    let accumulated_data = b"Some prefix S-V1.6-3\nOther data";
    let version = FirmwareReadFeedback::parse_version_string(accumulated_data);
    // 应该返回 "S-V1.6-3"（8 字节，包括 S-V）
    assert_eq!(version, Some("S-V1.6-3".to_string()));
}
```

2. **测试数据不足 8 字节的情况**：
```rust
#[test]
fn test_firmware_read_feedback_parse_version_string_short() {
    // 测试数据不足 8 字节的情况
    let accumulated_data = b"S-V1.6";
    let version = FirmwareReadFeedback::parse_version_string(accumulated_data);
    // 应该返回 "S-V1.6"（实际长度，不超过 8 字节）
    assert_eq!(version, Some("S-V1.6".to_string()));
}
```

3. **测试未找到 S-V 标记**：
```rust
#[test]
fn test_firmware_read_feedback_parse_version_string_not_found() {
    let accumulated_data = b"Some data without version";
    let version = FirmwareReadFeedback::parse_version_string(accumulated_data);
    assert_eq!(version, None);
}
```

4. **测试无效 UTF-8 字符处理**：
```rust
#[test]
fn test_firmware_read_feedback_parse_version_string_invalid_utf8() {
    // 测试包含无效 UTF-8 字符的情况
    let mut data = vec![b'S', b'-', b'V', 0xFF, 0xFE, b'1', b'.', b'6'];
    let version = FirmwareReadFeedback::parse_version_string(&data);
    // 应该使用 lossy 解码，不会 panic
    assert!(version.is_some());
}
```

#### 2.3.2 集成测试

**位置**：`tests/` 目录

**测试场景**：

1. **数据累积测试**：模拟接收多个 CAN 帧，验证数据正确累积
2. **解析测试**：验证版本字符串正确解析
3. **清空测试**：验证 `clear()` 方法正确清空数据
4. **完整性判断测试**：验证 `is_complete` 状态正确更新

### 2.4 实现步骤

1. **第一步**：修改 `parse_version_string` 方法（`src/protocol/feedback.rs`）
   - 改为固定 8 字节长度
   - 使用 `from_utf8_lossy` 处理无效字符

2. **第二步**：增强 `FirmwareVersionState`（`src/robot/state.rs`）
   - 添加 `clear()` 方法
   - 添加 `check_completeness()` 方法
   - 修改 `parse_version()` 方法

3. **第三步**：更新 pipeline 处理逻辑（`src/robot/pipeline.rs`）
   - 移除 TODO 注释
   - 确保 `is_complete` 状态正确更新

4. **第四步**：更新单元测试（`src/protocol/feedback.rs`）
   - 添加新的测试用例
   - 更新现有测试用例

5. **第五步**：验证与 Python SDK 的一致性
   - 使用相同的测试数据
   - 验证解析结果一致

## 3. 关键差异总结

| 特性 | Python SDK | Rust SDK (当前) | Rust SDK (目标) |
|------|-----------|----------------|----------------|
| 解析长度 | 固定 8 字节（包括 S-V） | 从 S-V 后开始，最多 20 字符 | 固定 8 字节（包括 S-V） |
| 错误处理 | 返回 `-0x4AF` | 返回 `None` | 返回 `None`（Rust 风格） |
| UTF-8 处理 | `errors='ignore'` | `from_utf8`（可能失败） | `from_utf8_lossy` |
| 数据清空 | `SearchPiperFirmwareVersion()` 时清空 | 无 | 提供 `clear()` 方法 |
| 完整性判断 | 无明确判断 | 有字段但未实现 | 基于 S-V 标记和数据长度 |

## 4. 注意事项

1. **向后兼容性**：修改解析逻辑可能影响现有代码，需要更新相关测试
2. **性能考虑**：每次收到数据都调用 `parse_version()` 和 `check_completeness()`，但固件版本更新频率低，性能影响可忽略
3. **数据完整性**：Python SDK 没有明确的完整性判断，我们的实现基于是否找到 S-V 标记和有足够数据，这是合理的增强
4. **错误处理**：Rust SDK 使用 `Option<String>` 而不是负数错误码，更符合 Rust 习惯

## 5. 参考代码位置

### Python SDK
- 数据累积：`tmp/piper_sdk/piper_sdk/interface/piper_interface_v2.py:2334-2344`
- 版本解析：`tmp/piper_sdk/piper_sdk/interface/piper_interface_v2.py:1589-1613`
- 查询命令：`tmp/piper_sdk/piper_sdk/interface/piper_interface_v2.py:3511-3530`

### Rust SDK
- 状态定义：`src/robot/state.rs:673-710`
- 数据累积：`src/robot/pipeline.rs:719-748`
- 版本解析：`src/protocol/feedback.rs:2380-2396`

## 6. 总结

本方案旨在将 Rust SDK 的固件版本解析逻辑与 Python SDK 对齐，主要改进包括：

1. **解析逻辑对齐**：固定 8 字节长度（包括 S-V）
2. **错误处理改进**：使用 `from_utf8_lossy` 处理无效字符
3. **功能增强**：添加数据清空和完整性判断功能
4. **保持 Rust 风格**：使用 `Option<String>` 而不是负数错误码

实现后，Rust SDK 的固件版本解析功能将与 Python SDK 保持一致，同时提供更好的类型安全和错误处理。

