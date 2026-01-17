# 协议实现计划第二次审查报告

## 发现的错误和不一致

### 1. ControlMode 枚举值不一致 ⚠️

**问题**：
- `protocol.md` 中 **0x2A1（反馈帧）** 的 ControlMode 有完整值：0x00-0x07
  - 0x05: 遥控器控制模式
  - 0x06: 联动示教输入模式
- `protocol.md` 中 **0x151（控制指令）** 的 ControlMode 只有部分值：0x00, 0x01, 0x02, 0x03, 0x04, 0x07
  - **缺少 0x05 和 0x06**

**计划中的问题**：
- 计划中只定义了一个 `ControlMode` 枚举，没有区分反馈帧和控制指令的差异
- 0x151 指令中未列出的值（0x05, 0x06）应该使用 `TryFrom` 或 `#[fallback]` 处理

**修复建议**：
```rust
// 反馈帧的 ControlMode（完整定义）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlMode {
    Standby = 0x00,
    CanControl = 0x01,
    Teach = 0x02,
    Ethernet = 0x03,
    Wifi = 0x04,
    Remote = 0x05,        // 仅反馈帧有
    LinkTeach = 0x06,     // 仅反馈帧有
    OfflineTrajectory = 0x07,
}

// 控制指令的 ControlMode（部分值）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlModeCommand {
    Standby = 0x00,
    CanControl = 0x01,
    Teach = 0x02,
    Ethernet = 0x03,
    Wifi = 0x04,
    // 0x05, 0x06 未定义
    OfflineTrajectory = 0x07,
}

impl TryFrom<u8> for ControlModeCommand {
    type Error = ProtocolError;
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0x00 => Ok(ControlModeCommand::Standby),
            0x01 => Ok(ControlModeCommand::CanControl),
            0x02 => Ok(ControlModeCommand::Teach),
            0x03 => Ok(ControlModeCommand::Ethernet),
            0x04 => Ok(ControlModeCommand::Wifi),
            0x07 => Ok(ControlModeCommand::OfflineTrajectory),
            _ => Err(ProtocolError::InvalidValue { field: "ControlMode", value }),
        }
    }
}
```

---

### 2. 0x151 指令 Byte 3 MIT 模式值特殊 ⚠️

**问题**：
- `protocol.md` 中 0x151 指令 Byte 3: mit模式
  - 0x00: 位置速度模式（默认）
  - **0xAD: MIT模式**（用于主从模式）

**计划中的问题**：
- 计划中只提到 `mit_mode: u8`，没有说明 0xAD 这个特殊值

**修复建议**：
```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MitMode {
    PositionVelocity = 0x00,  // 位置速度模式（默认）
    Mit = 0xAD,               // MIT模式（用于主从模式）
}

impl TryFrom<u8> for MitMode {
    type Error = ProtocolError;
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0x00 => Ok(MitMode::PositionVelocity),
            0xAD => Ok(MitMode::Mit),
            _ => Err(ProtocolError::InvalidValue { field: "MitMode", value }),
        }
    }
}
```

---

### 3. 0x2A8 夹爪反馈 Bit 6 定义矛盾 ⚠️

**问题**：
- `protocol.md` 中 0x2A8 Byte 6 Bit 6: 驱动器使能状态
  - **（1：使能 0：失能）** - 注意：1 表示使能，0 表示失能（与通常逻辑相反）

**计划中的问题**：
- 计划中的 `GripperStatus` 位域定义可能没有明确说明这个反向逻辑

**修复建议**：
```rust
#[bitsize(8)]
#[derive(FromBits, DebugBits, Clone, Copy)]
pub struct GripperStatus {
    pub voltage_low: bool,          // Bit 0: 0正常 1过低
    pub motor_over_temp: bool,      // Bit 1: 0正常 1过温
    pub driver_over_current: bool, // Bit 2: 0正常 1过流
    pub driver_over_temp: bool,    // Bit 3: 0正常 1过温
    pub sensor_error: bool,        // Bit 4: 0正常 1异常
    pub driver_error: bool,        // Bit 5: 0正常 1错误
    pub enabled: bool,             // Bit 6: **1使能 0失能**（注意：反向逻辑）
    pub homed: bool,               // Bit 7: 0没有回零 1已经回零
}
```

---

### 4. 0x476 应答指令索引说明不完整 ⚠️

**问题**：
- `protocol.md` 中 0x476 Byte 0: 应答指令索引
  - "取设置指令id最后一个字节，例如：应答0x471设置指令时此位填充0x71"
  - 但 protocol.md 中也提到"应答 `0x476 byte0 = 0x50; byte 2=N`"（来自 0x150 指令说明）

**计划中的问题**：
- 计划中只说明了"取设置指令id最后一个字节"，但没有说明 0x50 这个特殊值
  - 0x50 可能是 0x150 的最后一个字节（但 0x150 的最后一个字节应该是 0x50，这似乎不对）
  - 实际上 0x150 的最后一个字节是 0x50（十六进制），但作为十进制是 80
  - 更可能是：0x50 = 80，对应某个特殊含义

**修复建议**：
- 需要明确 0x476 的应答逻辑：
  - 对于设置指令（0x471, 0x474, 0x475 等）：取指令 ID 最后一个字节（0x71, 0x74, 0x75）
  - 对于 0x150 轨迹传输：使用 0x50（可能是固定值或特殊编码）
- 在文档中说明这个特殊情况

---

### 5. 0x150 指令应答说明缺失 ⚠️

**问题**：
- `protocol.md` 中 0x150 指令说明：
  - "主控收到后会应答 `0x476 byte0 = 0x50; byte 2=N`（详见0x476），未收到应答需要重传。"

**计划中的问题**：
- 计划中 0x150 指令的设计没有提到应答机制
- 0x476 的设计也没有明确说明如何应答 0x150 指令

**修复建议**：
- 在 0x150 指令的说明中添加应答机制说明
- 在 0x476 的设计中明确说明如何应答 0x150 指令

---

### 6. 0x151 指令 Byte 0 枚举值不完整 ⚠️

**问题**：
- `protocol.md` 中 0x151 指令 Byte 0 只列出了部分 ControlMode 值
- 但实际实现中可能需要处理所有值（包括未列出的）

**计划中的问题**：
- 计划中 `ControlModeCommand` 应该使用 `TryFrom` 处理未定义值

**修复建议**：
- 已在问题 1 中说明

---

### 7. 0x470 指令长度说明 ⚠️

**问题**：
- `protocol.md` 中 0x470 指令：Len: 4（只有 4 字节有效数据）
- 但 CAN 帧固定 8 字节

**计划中的问题**：
- 计划中的实现是正确的（填充 0），但可以更明确说明

**修复建议**：
- 已在计划中说明，无需修改

---

### 8. 0x158 指令长度说明 ⚠️

**问题**：
- `protocol.md` 中 0x158 指令：Len: 1（只有 Byte 0 有效）
- 但 CAN 帧固定 8 字节

**计划中的问题**：
- 计划中的实现是正确的（其他字节填充 0），但可以更明确说明

**修复建议**：
- 已在计划中说明，无需修改

---

### 9. 0x422 指令长度说明 ⚠️

**问题**：
- `protocol.md` 中 0x422 指令：数据长度: 0x01（只有 Byte 0 有效）
- 但 CAN 帧固定 8 字节

**计划中的问题**：
- 计划中没有详细设计 0x422 指令

**修复建议**：
- 如果实现 0x422，需要说明其他字节填充 0

---

### 10. 0x477 指令 Byte 0 和 Byte 1 的互斥性 ⚠️

**问题**：
- `protocol.md` 中 0x477 指令：
  - Byte 0: 参数查询（0x01-0x04）
  - Byte 1: 参数设置（0x01-0x02）
- 这两个字段似乎是互斥的（要么查询，要么设置）

**计划中的问题**：
- 计划中使用 `Option<ParameterQuery>` 和 `Option<ParameterSet>`，这是正确的
- 但需要说明这两个字段不能同时设置

**修复建议**：
- 在结构体中添加验证方法，确保不同时设置查询和设置

---

### 11. 0x476 应答逻辑的完整说明缺失 ⚠️

**问题**：
- `protocol.md` 中 0x476 的应答逻辑比较复杂：
  - Byte 0: 应答指令索引（取设置指令 ID 最后一个字节）
  - Byte 1: 零点是否设置成功（仅关节设置指令）
  - Byte 2: 轨迹点传输成功应答（仅 0x150 轨迹传输）
  - Byte 3: 轨迹包传输完成应答（仅 0x150 轨迹传输）
  - Byte 4-7: NameIndex 和 CRC（仅 0x150 轨迹传输）

**计划中的问题**：
- 计划中的 `SettingResponse` 结构体包含了所有字段，但没有说明哪些字段在什么情况下有效

**修复建议**：
- 添加文档说明，说明不同指令对应的有效字段
- 或者使用枚举区分不同类型的应答

---

### 12. 0x151 指令 Byte 2~7 的默认值说明 ⚠️

**问题**：
- `protocol.md` 中 0x151 指令说明：
  - "作为模式切换指令时，Byte2~Byte7全部填充默认值0x0即可。"

**计划中的问题**：
- 计划中没有明确说明这个特殊情况

**修复建议**：
- 在 `ControlModeCommand` 的 `to_frame()` 方法中添加注释说明
- 或者提供专门的模式切换方法

---

## 总结

### 需要修复的问题：

1. **ControlMode 枚举区分**：区分反馈帧和控制指令的枚举值
2. **MitMode 枚举**：添加 0xAD 特殊值处理
3. **GripperStatus Bit 6 说明**：明确反向逻辑
4. **0x476 应答逻辑**：完善应答机制的说明
5. **0x150 应答说明**：添加应答机制说明
6. **0x477 字段互斥性**：添加验证说明

### 建议改进：

1. 为不同指令类型提供专门的构造方法
2. 添加字段有效性验证
3. 完善文档注释

