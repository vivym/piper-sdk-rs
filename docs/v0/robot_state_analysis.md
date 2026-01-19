# Robot State 扩展分析报告

## 新增反馈帧类型分析

本次新增的反馈帧类型包括：
1. **FirmwareReadFeedback** (0x4AF) - 固件版本读取
2. **ControlModeCommandFeedback** (0x151) - 主从模式控制模式指令反馈
3. **JointControl12Feedback, JointControl34Feedback, JointControl56Feedback** (0x155-0x157) - 主从模式关节控制指令反馈
4. **GripperControlFeedback** (0x159) - 主从模式夹爪控制指令反馈

---

## 一、当前 Robot State 架构

### 1.1 状态分类（按更新频率）

**热数据（500Hz，高频运动数据）** - 使用 `ArcSwap`（无锁读取）
- `JointPositionState` (0x2A5-0x2A7) - 关节位置
- `EndPoseState` (0x2A2-0x2A4) - 末端位姿
- `JointDynamicState` (0x251-0x256) - 关节速度和电流

**温数据（200Hz，控制状态）** - 使用 `ArcSwap`
- `RobotControlState` (0x2A1) - 机器人控制状态
- `GripperState` (0x2A8) - 夹爪状态

**温数据（40Hz，诊断数据）** - 使用 `ArcSwap`
- `JointDriverLowSpeedState` (0x261-0x266) - 关节驱动器低速反馈

**冷数据（10Hz 或按需，诊断和配置）** - 使用 `RwLock`
- `CollisionProtectionState` (0x47B) - 碰撞保护状态
- `JointLimitConfigState` (0x473) - 关节限制配置
- `JointAccelConfigState` (0x47C) - 关节加速度限制配置
- `EndLimitConfigState` (0x478) - 末端限制配置状态

---

## 二、新增反馈帧是否需要添加到 Robot State？

### 2.1 FirmwareReadFeedback (0x4AF) - 固件版本读取

**特性分析**：
- **更新频率**：按需查询（非周期性）
- **数据特性**：
  - 需要累积多个 CAN 帧才能组成完整的版本字符串（类似 Python SDK 的实现）
  - 版本信息在系统运行期间通常不会变化
  - 数据量大（可能超过单个 CAN 帧 8 字节限制）

**建议**：
- ✅ **应该添加状态**
- **分类**：**冷数据**（`RwLock`）
- **理由**：
  1. 版本信息可能需要被用户查询（通过 API）
  2. 需要累积多个帧，需要一个缓冲区存储
  3. 更新频率低，使用 `RwLock` 开销可接受

**状态结构设计**：
```rust
/// 固件版本状态
///
/// 更新频率：按需查询
/// CAN ID：0x4AF（多帧累积）
/// 同步机制：RwLock（冷数据，更新频率低）
#[derive(Debug, Clone, Default)]
pub struct FirmwareVersionState {
    /// 硬件时间戳（微秒，最后一帧的时间）
    pub hardware_timestamp_us: u64,

    /// 系统接收时间戳（微秒）
    pub system_timestamp_us: u64,

    /// 累积的固件数据（字节数组）
    /// 注意：版本字符串需要从累积数据中解析
    pub firmware_data: Vec<u8>,

    /// 是否已收到完整数据
    /// 注意：判断条件需要根据实际情况确定（例如收到特定结束标记）
    pub is_complete: bool,

    /// 解析后的版本字符串（缓存）
    /// 如果 firmware_data 中包含有效的版本字符串，这里存储解析结果
    pub version_string: Option<String>,
}

impl FirmwareVersionState {
    /// 尝试从累积数据中解析版本字符串
    pub fn parse_version(&mut self) -> Option<String> {
        if let Some(version) = FirmwareReadFeedback::parse_version_string(&self.firmware_data) {
            self.version_string = Some(version.clone());
            Some(version)
        } else {
            None
        }
    }

    /// 获取版本字符串（如果已解析）
    pub fn version_string(&self) -> Option<&String> {
        self.version_string.as_ref()
    }
}
```

---

### 2.2 主从模式控制指令反馈（0x151, 0x155-0x157, 0x159）

**特性分析**：
- **更新频率**：取决于主臂发送频率（可能是 500Hz 或更高）
- **数据特性**：
  - 主从模式下，从臂需要实时跟踪主臂发送的控制指令
  - 用于实现主从同步控制
  - 数据实时性要求高

**建议**：
- ✅ **应该添加状态**
- **分类**：**温数据**（`ArcSwap`）
- **理由**：
  1. 主从模式下需要实时访问主臂指令状态
  2. 更新频率可能较高（500Hz+）
  3. 需要使用 `ArcSwap` 实现无锁读取，适合高频访问

**状态结构设计**：

由于不同 CAN ID 之间可能不同步，需要拆分为三个独立的状态：

#### 1. 控制模式指令状态 (0x151)

```rust
/// 主从模式控制模式指令状态
///
/// 更新频率：~200Hz（取决于主臂发送频率）
/// CAN ID：0x151
/// 同步机制：ArcSwap（温数据，高频访问）
#[derive(Debug, Clone, Default)]
pub struct MasterSlaveControlModeState {
    /// 硬件时间戳（微秒）
    pub hardware_timestamp_us: u64,

    /// 系统接收时间戳（微秒）
    pub system_timestamp_us: u64,

    /// 控制模式指令（来自 0x151）
    pub control_mode: ControlModeCommand,
    pub move_mode: MoveMode,
    pub speed_percent: u8,
    pub mit_mode: MitMode,
    pub trajectory_stay_time: u8,
    pub install_position: InstallPosition,

    /// 是否有效（已收到至少一帧）
    pub is_valid: bool,
}
```

#### 2. 关节控制指令状态 (0x155-0x157) - 帧组同步

```rust
/// 主从模式关节控制指令状态（帧组同步）
///
/// 更新频率：~500Hz（取决于主臂发送频率）
/// CAN ID：0x155-0x157（帧组：J1-J2, J3-J4, J5-J6）
/// 同步机制：ArcSwap（温数据，帧组同步）
///
/// **注意**：这是一个帧组，类似于 `JointPositionState`，需要集齐 3 帧后一起提交
#[derive(Debug, Clone, Default)]
pub struct MasterSlaveJointControlState {
    /// 硬件时间戳（微秒，来自完整帧组的最后一帧）
    pub hardware_timestamp_us: u64,

    /// 系统接收时间戳（微秒，系统接收到完整帧组的时间）
    pub system_timestamp_us: u64,

    /// 关节目标角度（度，0.001°单位）[J1, J2, J3, J4, J5, J6]
    pub joint_target_deg: [i32; 6],

    /// 帧组有效性掩码（Bit 0-2 对应 0x155, 0x156, 0x157）
    /// - 1 表示该CAN帧已收到
    /// - 0 表示该CAN帧未收到（可能丢包）
    pub frame_valid_mask: u8,
}

impl MasterSlaveJointControlState {
    /// 检查是否接收到了完整的帧组 (0x155, 0x156, 0x157)
    pub fn is_fully_valid(&self) -> bool {
        self.frame_valid_mask == 0b0000_0111 // Bit 0-2 全部为 1
    }

    /// 获取丢失的CAN帧索引（用于调试）
    pub fn missing_frames(&self) -> Vec<usize> {
        (0..3).filter(|&i| (self.frame_valid_mask & (1 << i)) == 0).collect()
    }

    /// 获取关节目标角度（度）
    pub fn joint_target_deg(&self, joint_index: usize) -> Option<f64> {
        if joint_index < 6 {
            Some(self.joint_target_deg[joint_index] as f64 / 1000.0)
        } else {
            None
        }
    }

    /// 获取关节目标角度（弧度）
    pub fn joint_target_rad(&self, joint_index: usize) -> Option<f64> {
        self.joint_target_deg(joint_index)
            .map(|deg| deg * std::f64::consts::PI / 180.0)
    }
}
```

#### 3. 夹爪控制指令状态 (0x159)

```rust
/// 主从模式夹爪控制指令状态
///
/// 更新频率：~200Hz（取决于主臂发送频率）
/// CAN ID：0x159
/// 同步机制：ArcSwap（温数据，高频访问）
#[derive(Debug, Clone, Default)]
pub struct MasterSlaveGripperControlState {
    /// 硬件时间戳（微秒）
    pub hardware_timestamp_us: u64,

    /// 系统接收时间戳（微秒）
    pub system_timestamp_us: u64,

    /// 夹爪目标行程（mm，0.001mm单位）
    pub gripper_target_travel_mm: i32,

    /// 夹爪目标扭矩（N·m，0.001N·m单位）
    pub gripper_target_torque_nm: i16,

    /// 夹爪状态码
    pub gripper_status_code: u8,

    /// 夹爪回零设置
    pub gripper_set_zero: u8,

    /// 是否有效（已收到至少一帧）
    pub is_valid: bool,
}

impl MasterSlaveGripperControlState {
    /// 获取夹爪目标行程（mm）
    pub fn gripper_target_travel(&self) -> f64 {
        self.gripper_target_travel_mm as f64 / 1000.0
    }

    /// 获取夹爪目标扭矩（N·m）
    pub fn gripper_target_torque(&self) -> f64 {
        self.gripper_target_torque_nm as f64 / 1000.0
    }
}
```
```

---

## 三、实现建议

### 3.1 优先级

1. **高优先级**：主从模式控制指令状态（拆分后）
   - `MasterSlaveJointControlState` (0x155-0x157) - 关节控制指令，帧组同步
   - `MasterSlaveControlModeState` (0x151) - 控制模式指令
   - `MasterSlaveGripperControlState` (0x159) - 夹爪控制指令
   - 主从模式是核心功能之一，实时性要求高

2. **中优先级**：`FirmwareVersionState`
   - 功能相对独立
   - 更新频率低
   - 可以通过 API 按需查询

### 3.2 实现步骤

#### Step 1: 添加状态结构体定义
- 在 `src/robot/state.rs` 中添加 `FirmwareVersionState` 和 `MasterSlaveControlState`

#### Step 2: 在 `PiperContext` 中添加状态字段
```rust
pub struct PiperContext {
    // ... 现有字段 ...

    // === 冷数据 ===
    /// 固件版本状态（按需查询：0x4AF）
    pub firmware_version: Arc<RwLock<FirmwareVersionState>>,

    // === 温数据（主从模式）===
    /// 主从模式控制模式指令状态（主从模式：0x151）
    pub master_slave_control_mode: Arc<ArcSwap<MasterSlaveControlModeState>>,

    /// 主从模式关节控制指令状态（主从模式：0x155-0x157，帧组同步）
    pub master_slave_joint_control: Arc<ArcSwap<MasterSlaveJointControlState>>,

    /// 主从模式夹爪控制指令状态（主从模式：0x159）
    pub master_slave_gripper_control: Arc<ArcSwap<MasterSlaveGripperControlState>>,
}
```
```

#### Step 3: 在 `pipeline.rs` 中添加反馈帧处理逻辑
- 处理 `FirmwareReadFeedback` (0x4AF)：累积数据到 `FirmwareVersionState`
- 处理 `ControlModeCommandFeedback` (0x151)：更新 `MasterSlaveControlModeState`
- 处理 `JointControl12Feedback` 等 (0x155-0x157)：
  - 类似 `JointPositionState` 的帧组同步机制
  - 使用 `pending_joint_target_deg` 缓存
  - 收到 0x157（最后一帧）时提交 `MasterSlaveJointControlState`
- 处理 `GripperControlFeedback` (0x159)：更新 `MasterSlaveGripperControlState`

#### Step 4: 更新 FPS 统计
- 在 `FpsStatistics` 中添加相应的更新计数器

#### Step 5: 在 `Piper` API 中添加访问方法
- `get_firmware_version() -> Option<String>`
- `get_master_slave_control_mode() -> Arc<MasterSlaveControlModeState>`
- `get_master_slave_joint_control() -> Arc<MasterSlaveJointControlState>`
- `get_master_slave_gripper_control() -> Arc<MasterSlaveGripperControlState>`

---

## 四、实现完成 ✅

### 4.1 实现状态总结

| 步骤 | 状态 | 说明 |
|------|------|------|
| Step 1: 状态结构体定义 | ✅ 完成 | 所有4个状态结构体已实现 |
| Step 2: PiperContext 集成 | ✅ 完成 | 所有状态字段已添加并初始化 |
| Step 3: Pipeline 处理逻辑 | ✅ 完成 | 所有反馈帧处理逻辑已实现（包括帧组同步和超时处理） |
| Step 4: FPS 统计 | ✅ 完成 | 所有计数器已添加 |
| Step 5: API 访问方法 | ✅ 完成 | 所有公开方法已实现 |

### 4.2 编译和测试状态

✅ **所有代码已通过编译检查**
✅ **所有单元测试通过** (55 tests passed)

```bash
$ cargo check
# 编译成功，无错误

$ cargo test --lib robot::state
# 55 tests passed
```

### 4.3 功能特性

#### ✅ 固件版本读取（0x4AF）
- 支持多帧累积（固件数据可能超过单个 CAN 帧 8 字节）
- 自动解析版本字符串（查找 "S-V" 标记）
- 版本字符串缓存（避免重复解析）

#### ✅ 主从模式控制指令反馈
- **控制模式指令（0x151）**：独立更新，实时跟踪主臂控制模式
- **关节控制指令（0x155-0x157）**：
  - 帧组同步机制（类似 `JointPositionState`）
  - 支持帧组有效性检查
  - 支持帧组超时重置
  - 保证6个关节数据的逻辑一致性
- **夹爪控制指令（0x159）**：独立更新，实时跟踪主臂夹爪指令

### 4.4 性能特性

- **无锁读取**：所有主从模式状态使用 `ArcSwap`，支持高频无锁读取
- **帧组同步**：关节控制指令（0x155-0x157）使用帧组同步，保证数据一致性
- **超时处理**：帧组超时自动重置，避免数据过期
- **FPS 统计**：所有新状态都支持更新频率统计

### 4.5 已实现的 API 方法

```rust
// 固件版本
pub fn get_firmware_version(&self) -> Option<String>

// 主从模式控制指令状态
pub fn get_master_slave_control_mode(&self) -> MasterSlaveControlModeState
pub fn get_master_slave_joint_control(&self) -> MasterSlaveJointControlState
pub fn get_master_slave_gripper_control(&self) -> MasterSlaveGripperControlState
```

---

## 五、总结

| 反馈帧 | CAN ID | 是否需要状态 | 优先级 | 数据分类 | 同步机制 | 状态结构 |
|--------|--------|-------------|--------|---------|----------|----------|
| FirmwareReadFeedback | 0x4AF | ✅ 是 | 中 | 冷数据 | RwLock | `FirmwareVersionState` |
| ControlModeCommandFeedback | 0x151 | ✅ 是 | 高 | 温数据 | ArcSwap | `MasterSlaveControlModeState` |
| JointControl12/34/56Feedback | 0x155-0x157 | ✅ 是 | 高 | 温数据 | ArcSwap（帧组同步） | `MasterSlaveJointControlState` |
| GripperControlFeedback | 0x159 | ✅ 是 | 高 | 温数据 | ArcSwap | `MasterSlaveGripperControlState` |

**结论**：所有新增的反馈帧都应该添加到 robot state 中，以支持：
1. **固件版本查询**：用户可能需要查询固件版本
2. **主从模式支持**：从臂需要跟踪主臂的控制指令状态
   - **0x151**：控制模式指令（独立更新）
   - **0x155-0x157**：关节控制指令（帧组同步，类似 `JointPositionState`）
   - **0x159**：夹爪控制指令（独立更新）

**设计原则**：
- 不同 CAN ID 的状态应该独立管理，因为它们可能不同步
- 帧组（如 0x155-0x157）需要帧组同步机制，确保数据一致性

---

## 五、注意事项

1. **固件版本数据累积**：
   - 需要处理多帧累积逻辑
   - 需要判断何时数据完整（例如收到结束标记）
   - 考虑缓冲区大小限制（避免内存泄漏）

2. **主从模式状态同步**：
   - **0x155-0x157** 关节控制指令需要帧组同步机制（类似 `JointPositionState`）
     - 使用 `pending_joint_target_deg` 缓存三帧数据
     - 收到 0x157（最后一帧）时一起提交
   - **0x151** 和 **0x159** 是独立帧，可以单独更新
   - 不同状态之间不需要同步（它们可能来自不同的主臂传输周期）

3. **状态有效性**：
   - 如果长时间未收到主臂指令，状态可能过期
   - 需要添加时间戳检查机制

4. **性能考虑**：
   - `MasterSlaveControlState` 使用 `ArcSwap` 确保无锁读取
   - `FirmwareVersionState` 使用 `RwLock` 因为更新频率低
