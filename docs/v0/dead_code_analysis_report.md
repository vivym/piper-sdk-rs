# `allow(dead_code)` 全面分析报告（修订版）

> **生成时间**: 2025-01-28
> **修订时间**: 2025-01-28（基于专业反馈修订）
> **分析范围**: 整个 piper-sdk-rs 工作区
> **分析方法**: 静态代码分析 + grep 搜索
> **特殊视角**: 机器人 SDK 安全性 + Rust 工程规范

---

## 执行摘要

本报告分析了 Piper SDK（机器人 SDK）代码库中所有 `#[allow(dead_code)]` 属性的使用情况。

### 统计概览

| 类别 | 数量 | 说明 |
|------|------|------|
| **生产代码** | 27 个 | 分布在 8 个 crate/app 中 |
| **测试代码** | 16 个 | 测试辅助方法和模拟对象 |
| **测试模块全局** | 1 个 | `#![allow(dead_code)]` |
| **总计** | 44 个 | |

### 优先级分类（修订版）

| 优先级 | 数量 | 说明 | 典型案例 |
|--------|------|------|----------|
| 🔴 **P0 - 安全/质量** | **10 个** | **安全漏洞或严重代码异味** | validation 未启用、遗留代码污染 |
| 🟡 **P1 - 优化建议** | 6 个 | 应该用更合适的属性替代 | 过时代码、未完成功能 |
| 🟢 **P2 - 保留合理** | 28 个 | 平台特定、未来 API、测试 | QoS 常量、预留功能 |

---

## 🚨 重大发现：安全漏洞

### 漏洞等级：P0 - 严重

**问题**：`apps/cli/src/validation.rs` 包含 8 个安全验证函数，**全部未启用**。

**影响**：
- ❌ 关节位置限制验证（`validate_joints`）未调用
- ❌ 路径验证（`PathValidator`）未调用
- ❌ 机器人可能执行超出物理限制的动作
- ❌ 输出文件路径错误可能导致数据丢失

**风险等级**：🔴 **高危** - 机器人 SDK 的安全验证被绕过

**立即行动**：
1. ✅ 在命令执行前**必须**调用 `validate_joints()`
2. ✅ 在录制前**必须**调用路径验证
3. ❌ **绝不能**因为"没人调用"就删除安全逻辑

---

## 第一部分：P0 - 安全和质量问题（10 个）

### 1.1 🔴 apps/cli/src/validation.rs (8 个) - **安全漏洞**

#### 1.1.1-1.1.2 关节验证器

```rust
#[allow(dead_code)]
pub fn validate_joints(&self, positions: &[f64]) -> Result<()> {
    if positions.len() != 6 {
        anyhow::bail!("需要 6 个关节位置，得到 {} 个", positions.len());
    }

    for (i, &pos) in positions.iter().enumerate() {
        // 检查 NaN 和无穷大
        if !pos.is_finite() {
            anyhow::bail!("关节 J{} 位置无效: {}", i + 1,
                if pos.is_nan() { "NaN" } else { "无穷大" });
        }

        self.validate_joint(i, pos)?;
    }

    Ok(())
}

#[allow(dead_code)]
pub fn clamp_joints(&self, positions: &mut [f64]) -> Result<()> {
    if positions.len() != 6 {
        anyhow::bail!("需要 6 个关节位置，得到 {} 个", positions.len());
    }

    for (i, pos) in positions.iter_mut().enumerate() {
        if !pos.is_finite() {
            anyhow::bail!("关节 J{} 位置无效", i + 1);
        }

        if *pos < self.min_angle {
            *pos = self.min_angle;
        } else if *pos > self.max_angle {
            *pos = self.max_angle;
        }
    }

    Ok(())
}
```

**问题分析**：
- **用途**: 关节位置安全验证（NaN 检查、范围限制）
- **当前状态**: ❌ **未调用** - CLI 代码中没有任何地方调用这些验证
- **安全影响**: 🔴 **严重** - 机器人可能执行超出物理限制的动作
- **风险评估**:
  - 关节角度超过 ±π 可能导致机械碰撞
  - NaN 输入可能导致控制器崩溃
  - 无限制的位置命令可能损坏硬件

**行动方案**：
```rust
// ✅ 在所有位置命令中启用验证
// apps/cli/src/commands/move.rs

pub async fn execute(&self) -> Result<()> {
    let positions = self.parse_joints()?;

    // 🔴 P0 安全修复：必须验证关节位置
    let validator = JointValidator::default_range();
    validator.validate_joints(&positions)
        .context("关节位置安全检查失败")?;

    // 验证通过后继续执行
    println!("✅ 安全检查通过");

    // ... 继续执行移动命令
}
```

**建议**: 🔴 **P0 - 必须立即启用**，不要给"删除"选项

---

#### 1.1.3-1.1.6 路径验证器

```rust
#[allow(dead_code)]
pub struct PathValidator {
    check_exists: bool,
    check_readable: bool,
}

#[allow(dead_code)]
pub fn validate_path(&self, path: &str) -> Result<()> {
    let path = Path::new(path);

    if path.as_os_str().is_empty() {
        anyhow::bail!("文件路径为空");
    }

    if self.check_exists && !path.exists() {
        anyhow::bail!("文件不存在: {}", path.display());
    }

    if self.check_readable {
        if !path.exists() {
            anyhow::bail!("文件不存在，无法读取: {}", path.display());
        }

        std::fs::File::open(path)
            .with_context(|| format!("无法读取文件: {}", path.display()))?;
    }

    Ok(())
}

#[allow(dead_code)]
pub fn validate_output_path(&self, path: &str) -> Result<()> {
    let path = Path::new(path);

    if path.as_os_str().is_empty() {
        anyhow::bail!("文件路径为空");
    }

    // 检查父目录是否存在
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
        && !parent.exists()
    {
        anyhow::bail!("输出目录不存在: {}", parent.display());
    }

    Ok(())
}
```

**问题分析**：
- **用途**: 文件路径验证（输入/输出路径安全检查）
- **当前状态**: ❌ **未调用** - 录制命令不验证路径
- **安全影响**: 🟡 **中等** - 数据丢失风险
- **实际风险**:
  - 录制到不存在的目录 → 静默失败或数据丢失
  - 读取不存在的文件 → 运行时错误

**行动方案**：
```rust
// ✅ 在录制命令中启用路径验证
// apps/cli/src/commands/record.rs

pub async fn execute(&self) -> Result<()> {
    let output_path = PathBuf::from(&self.output);

    // 🔴 P0 安全修复：验证输出路径
    let validator = PathValidator::new();
    validator.validate_output_path(&self.output)
        .context("输出路径验证失败")?;

    // ... 继续执行录制
}
```

**建议**: 🔴 **P0 - 必须启用**

---

#### 1.1.7-1.1.8 CAN ID 验证器

```rust
#[allow(dead_code)]
pub struct CanIdValidator;

#[allow(dead_code)]
impl CanIdValidator {
    pub fn validate_standard(id: u32) -> Result<()> {
        if id > 0x7FF {
            anyhow::bail!("标准 CAN ID 必须小于 0x7FF，得到: 0x{:03X}", id);
        }
        Ok(())
    }
    // ...
}
```

**问题分析**：
- **用途**: CAN ID 格式验证
- **当前状态**: ❌ **未使用**
- **价值评估**: ❓ **低价值** - 应用层不应该关心 CAN ID 格式
- **架构判断**: CAN ID 验证应该在协议层或驱动层

**建议**: 🟡 **删除** - 应用层的抽象泄漏，不符合分层架构原则

---

### 1.2 🔴 crates/piper-driver/src/pipeline.rs (1 个) - **代码异味**

#### 1.2.1 遗留的 `tx_loop()` 函数 (Line 679)

```rust
#[allow(dead_code)]
pub fn tx_loop(
    mut tx: impl TxAdapter,
    realtime_rx: Receiver<PiperFrame>,
    reliable_rx: Receiver<PiperFrame>,
    is_running: Arc<AtomicBool>,
    metrics: Arc<PiperMetrics>,
    ctx: Arc<PiperContext>,
)
```

**问题分析**：
- **用途**: TX 线程主循环
- **当前状态**: ✅ **已被替代** - 使用 `tx_loop_mailbox()` 替代
- **标记原因**: 保留作为"参考实现"

**❌ 错误做法** - "代码库不是历史博物馆"

**严重性分析**：
- 🟡 **代码异味** - 两套 TX loop 并存会：
  - 增加维护心智负担
  - 误导新开发者（"为什么有两个 loop？"）
  - 干扰 IDE 重构工具
  - 容易导致错误的调用

**✅ 正确做法** - 相信 Git 历史

```bash
# 如果需要参考，使用 Git
git log --all --oneline --grep="tx_loop"
git show <commit-hash>:crates/piper-driver/src/pipeline.rs | grep -A 50 "fn tx_loop"

# 或移动到 examples/
mkdir -p examples/legacy
mv tx_loop_reference.rs examples/legacy/
```

**行动方案**：
```diff
- /// TX 线程主循环
- #[allow(dead_code)]
- pub fn tx_loop(...) {
-     // ...
- }
+ // ⚠️ tx_loop 已移除，使用 tx_loop_mailbox() 替代
+ // 参考 Git 历史或 docs/v0/mailbox_pattern_implementation.md
```

**建议**: 🔴 **P0 - 立即删除**，不要保留在 `src/` 目录

---

### 1.3 🔴 crates/piper-client/src/raw_commander.rs (1 个) - **死代码**

#### 1.3.1 `send_pose_with_index()` (Line 312)

```rust
#[allow(dead_code)] // 保留用于向后兼容或特殊场景
pub(crate) fn send_pose_with_index(
    &self,
    position: Position3D,
    orientation: EulerAngles,
    index: u8,
) -> Result<()>
```

**问题分析**：
- **可见性**: `pub(crate)` - **包内可见**，不是公开 API
- **当前状态**: ❌ **无调用者** - grep 确认没有内部调用
- **替代方案**: ✅ **存在** - `send_circular_motion()` 提供完整功能

**❌ 报告原建议的问题**

原报告建议添加 `#[deprecated]`，但这是**错误的做法**：

> **对内部（Internal）未使用的代码使用 `#[deprecated]` 是没有意义的，因为你完全控制所有调用方。**

**行动方案**：
```diff
- #[allow(dead_code)] // 保留用于向后兼容或特殊场景
- pub(crate) fn send_pose_with_index(...) -> Result<()>
+ // send_pose_with_index 已删除，使用 send_circular_motion() 代替
```

**建议**: 🔴 **P0 - 立即删除**，不需要 deprecation 流程

---

## 第二部分：P0 - 误标记修复（2 个）

### 2.1 apps/cli/src/safety.rs (3 个) - **误标记**

#### 2.1.1-2.1.3 SafetyChecker 结构体和方法

```rust
#[allow(dead_code)]
pub struct SafetyChecker { ... }

#[allow(dead_code)]
impl SafetyChecker { ... }

#[allow(dead_code)]
pub fn show_confirmation_prompt(&self, positions: &[f64]) -> Result<bool>
```

**问题分析**：
- **实际使用**: ✅ **被使用** - `apps/cli/src/modes/oneshot.rs:80-81` 调用
- **标记原因**: 误标记为 dead_code

**行动方案**：
```diff
- #[allow(dead_code)]
  pub struct SafetyChecker {

- #[allow(dead_code)]
  impl SafetyChecker {

- #[allow(dead_code)]
  pub fn show_confirmation_prompt(&self, positions: &[f64]) -> Result<bool> {
```

**建议**: 🔴 **P0 - 移除标记**

---

### 2.2 apps/daemon/src/client_manager.rs (2 个) - **测试专用代码**

#### 2.2.1 `created_at` 字段 (Line 50)

```rust
pub struct Client {
    pub id: u32,
    pub addr: ClientAddr,
    pub last_active: Instant,
    pub filters: Vec<CanIdFilter>,
    pub consecutive_errors: AtomicU32,
    #[cfg_attr(not(unix), allow(dead_code))]
    pub send_frequency_level: AtomicU32,

    #[allow(dead_code)]
    pub created_at: Instant,  // ← 这里
}
```

#### 2.2.2 `client_age()` 方法 (Line 70)

```rust
#[allow(dead_code)]
pub fn client_age(&self) -> Duration {
    self.created_at.elapsed()
}
```

**问题分析**：

| 方面 | 分析 |
|------|------|
| **created_at 的用途** | 仅用于 `client_age()` 计算 |
| **client_age() 的用途** | 仅用于测试 `test_client_age()` (Line 458) |
| **生产环境是否读取** | ❌ **只写不读** |
| **编译器警告** | 如果移除 `allow`，会报 "field is never read" |

**⚠️ 细化建议**

选项 A: **保留用于调试**
```rust
/// 调试信息：客户端创建时间（用于连接追踪和故障排查）
#[allow(dead_code)]  // 仅用于 client_age() 调试工具
pub created_at: Instant,
```

选项 B: **改为测试专用**
```rust
#[cfg(test)]  // 仅在测试中包含此字段
pub created_at: Instant,
```

**选项对比**：

| 选项 | 优点 | 缺点 |
|------|------|------|
| 保留 `allow` | 调试时可用 | 增加生产内存（虽然很小） |
| `cfg(test)` | 零运行时开销 | 调试时无法使用 |

**建议**: 🟡 **P1 - 根据调试需求选择**
- 如果需要生产调试 → 保留 `allow` + 添加注释
- 如果不需要 → 改为 `cfg(test)`

---

## 第三部分：P1 - 优化建议（6 个）

### 3.1 apps/cli/src/utils.rs (2 个) - **过时代码**

#### 3.1.1 `prompt_confirmation()` (Line 30)

```rust
#[allow(dead_code)]
pub fn prompt_confirmation(prompt: &str, default: bool) -> Result<bool>
```

**问题分析**：
- **当前状态**: ❌ **未使用**
- **替代方案**: ✅ **存在** - `inquire::Confirm` 在 `safety.rs` 中使用
- **过时原因**: `inquire` crate 提供更现代的交互体验

**建议**: 🟡 **P1 - 删除**，不要添加 deprecated（内部函数）

---

#### 3.1.2 `prompt_input()` (Line 72)

```rust
#[allow(dead_code)]
pub fn prompt_input(prompt: &str, default: Option<&str>) -> Result<String>
```

**问题分析**：
- **当前状态**: ❌ **未使用**
- **替代方案**: `inquire::Text`

**建议**: 🟡 **P1 - 删除**

---

### 3.2 apps/cli/src/modes/oneshot.rs + script.rs (2 个) - **未完成功能**

#### 3.2.1 `OneShotConfig::serial` (Line 30)

#### 3.2.2 `ScriptConfig::serial` (Line 67)

```rust
pub struct OneShotConfig {
    pub interface: Option<String>,
    #[allow(dead_code)]
    pub serial: Option<String>,  // ← 未生效
    pub safety: SafetyConfig,
}
```

**问题分析**：
- **预期功能**: 通过序列号连接特定 GS-USB 设备
- **实际状态**: ⚠️ **配置读取了，但未传递给下层**

**Bug 示例**：
```rust
// ❌ 当前的实现
let builder = if let Some(interface) = &self.config.interface {
    PiperBuilder::new().interface(interface)
} else {
    PiperBuilder::new()
};
// serial 字段被忽略了！

// ✅ 应该是
let builder = if let Some(serial) = &self.config.serial {
    PiperBuilder::new().with_serial(serial)
} else if let Some(interface) = &self.config.interface {
    PiperBuilder::new().interface(interface)
} else {
    PiperBuilder::new()
};
```

**建议**: 🟡 **P1 - 完成实现或删除字段**

---

### 3.3 apps/cli/src/script.rs (1 个) - **不完整的 API**

#### 3.3.1 `save_script()` (Line 112)

```rust
#[allow(dead_code)]
pub fn save_script<P: AsRef<std::path::Path>>(path: P, script: &Script) -> Result<()>
```

**问题分析**：
- **用途**: 保存脚本到文件
- **当前状态**: ❌ **未使用**
- **API 对称性**: `load_script()` 存在且被使用

**建议**: 🟢 **P2 - 保留**，添加注释说明这是预留的脚本创建功能

---

### 3.4 crates/piper-driver/src/pipeline.rs (1 个) - **测试辅助方法**

#### 3.4.1 `take_sent_frames()` (Line 1389)

```rust
#[allow(dead_code)]
fn take_sent_frames(&mut self) -> Vec<PiperFrame> {
    std::mem::take(&mut self.sent_frames)
}
```

**问题分析**：
- **用途**: 测试辅助方法
- **当前状态**: ❌ **测试中未使用**

**建议**: 🟡 **P1 - 删除**（如果测试不需要）

---

## 第四部分：P2 - 保留合理（28 个）

### 4.1 平台特定代码 (2 个)

#### 4.1.1 apps/daemon/src/macos_qos.rs (3 个)

```rust
const QOS_CLASS_USER_INITIATED: qos_class_t = 0x19;
const QOS_CLASS_DEFAULT: qos_class_t = 0x15;
const QOS_CLASS_BACKGROUND: qos_class_t = 0x09;
```

**用途**: macOS 线程优先级常量（备用方案）

**建议**: 🟢 **保留**，添加注释：
```rust
// 备用 QoS 级别，当前未使用但保留以备将来需要
#[allow(dead_code)]
const QOS_CLASS_USER_INITIATED: qos_class_t = 0x19;
```

---

#### 4.1.2 apps/daemon/src/daemon.rs (1 个)

```rust
#[cfg_attr(not(unix), allow(dead_code))]
client_degraded: AtomicU64,
```

**建议**: 🟢 **保留** - `cfg_attr` 的正确用法

---

### 4.2 未来 API (3 个)

#### 4.2.1 crates/piper-protocol/src/control.rs (1 个)

```rust
/// 注意：此函数目前仅用于测试，保留作为公共 API 以便将来可能需要解析 MIT 控制反馈。
#[allow(dead_code)]
pub fn uint_to_float(x_int: u32, x_min: f32, x_max: f32, bits: u32) -> f32
```

**建议**: 🟢 **保留** - 添加清晰注释

---

#### 4.2.2 crates/piper-can/src/socketcan/split.rs (1 个)

```rust
/// 此函数当前未使用（硬件过滤器默认关闭），但保留以备将来需要时使用。
#[allow(dead_code)]
fn configure_hardware_filters(socket: &CanSocket) -> Result<(), CanError>
```

**用途**: SocketCAN 硬件过滤器（性能优化预留）

**建议**: 🟢 **保留**，已有清晰注释

---

### 4.3 测试代码 (16 个)

所有测试辅助方法和结构体（见原报告）

**建议**: 🟢 **保留** - 测试模块的标准做法

---

### 4.4 未完成但低优先级 (6 个)

`script.rs` 中的 `script_name` 字段、`save_script()` 等

**建议**: 🟢 **保留**，未来可能需要

---

## 第五部分：宏生成代码分析

### 5.1 Serde 反序列化字段

**检查点**：如果一个结构体用于反序列化 JSON/配置，某些字段可能"只写不读"。

**示例**：
```rust
#[derive(Deserialize)]
pub struct CliConfig {
    pub interface: Option<String>,
    pub serial: Option<String>,  // Rust 编译器可能认为"未读取"
}

// 实际使用：
let config: CliConfig = serde_json::from_str(json)?;
// serial 字段被 Serde 填充，但 Rust 代码可能没读取
```

**建议**：
- 如果字段确实用于反序列化 → `#[allow(dead_code)]` 是合理的
- 添加注释：`// Used by Serde deserialization`

---

### 5.2 Debug/Display 派生

**检查点**：`#[derive(Debug)]` 生成的 `fmt` 方法可能未被调用。

**建议**：
- 如果结构体用于调试日志 → 保留 Debug derive
- 如果完全不需要 → 移除 derive

---

## 第六部分：行动清单（修正版）

### 🔴 P0 - 立即执行（安全漏洞和代码异味）

| 优先级 | 文件/项目 | 操作 | 预计时间 | 理由 |
|--------|-----------|------|----------|------|
| P0 | **validation.rs** | **必须启用**安全验证 | 30 分钟 | 🔴 安全漏洞 |
| P0 | **pipeline.rs** `tx_loop` | **删除** | 5 分钟 | 🔴 代码异味 |
| P0 | **raw_commander.rs** `send_pose_with_index` | **删除** | 5 分钟 | 🔴 死代码 |
| P0 | **safety.rs** | 移除误标记 | 5 分钟 | 🔴 误标记 |

**总计**: ~45 分钟

---

### 🟡 P1 - 短期优化（1-2 小时）

| 优先级 | 文件/项目 | 操作 | 理由 |
|--------|-----------|------|------|
| P1 | utils.rs | 删除过时函数 | 被 inquire 替代 |
| P1 | oneshot.rs + script.rs | 完成序列号支持 | 未完成的功能 |
| P1 | client_manager.rs | 调试字段处理 | cfg(test) 或保留 |
| P1 | pipeline.rs `take_sent_frames` | 删除或启用测试辅助 |
| P1 | validation.rs `CanIdValidator` | 删除 | 应用层抽象泄漏 |

---

### 🟢 P2 - 长期维护（按需）

| 类别 | 数量 | 行动 |
|------|------|------|
| 平台特定 | 2 个 | 添加注释说明 |
| 未来 API | 3 个 | 添加文档说明用途 |
| 测试代码 | 16 个 | 保持现状 |
| 低优先级 | 6 个 | 根据需求决定 |

---

## 第七部分：团队规范建议

### 7.1 何时使用 `#[allow(dead_code)]`

| 场景 | 推荐做法 | 示例 |
|------|----------|------|
| **平台特定代码** | `#[cfg_attr(not(platform), allow(dead_code))]` | `client_degraded` 字段 |
| **测试专用字段** | `#[cfg(test)]` 或 `#[allow(dead_code)]` | 测试 mock 对象 |
| **未来 API** | `#[allow(dead_code)]` + 详细注释 | `uint_to_float()` |
| **Serde 字段** | `#[allow(dead_code)]` + "Used by Serde" | 配置结构体字段 |
| **遗留代码** | ❌ **删除**，相信 Git 历史 | - |
| **内部未使用** | ❌ **删除**，无需 deprecated | `send_pose_with_index` |
| **安全验证** | ❌ **必须启用**，绝不能删除 | `validate_joints` |

---

### 7.2 何时不应使用

| 场景 | 错误示例 | 正确做法 |
|------|----------|----------|
| 实际被使用 | `created_at` + `client_age` | 移除标记 |
| 应该删除的代码 | `prompt_input()` | 直接删除 |
| 应该启用的安全功能 | `validate_joints` | **立即启用** |
| 遗留实现 | `tx_loop` | **删除**，查 Git 历史 |
| 内部死代码 | `pub(crate)` 未使用 | 直接删除 |

---

### 7.3 安全验证规范（机器人 SDK 特殊要求）

**原则**：
> 🔴 **安全验证绝不能因为"未调用"就被删除**

**清单**：
- ✅ 关节位置限制（防止碰撞）
- ✅ NaN/无穷大检查（防止控制器崩溃）
- ✅ 速度限制（防止机械损坏）
- ✅ 力矩限制（防止过载）
- ✅ 路径验证（防止数据丢失）
- ✅ 配置验证（防止无效参数）

**实施要求**：
1. 所有安全验证**必须**在调用链中启用
2. 不能提供"跳过验证"的选项
3. 验证失败**必须**阻止执行，不能静默通过

---

## 第八部分：总结

### 关键修正

1. **tx_loop** - 从"保留参考"改为"立即删除"
2. **send_pose_with_index** - 从"deprecated"改为"立即删除"
3. **validation.rs** - 从"启用或删除"改为"必须启用"（安全漏洞）
4. **created_at** - 细化为 cfg(test) 或保留（调试需求）

### 优先级对比

| 优先级 | 原报告数量 | **修正后数量** | 变化 |
|--------|-----------|--------------|------|
| 🔴 P0 | 4 个 | **10 个** | +6（安全漏洞+遗留代码） |
| 🟡 P1 | 8 个 | **6 个** | -2（部分改为 P0 或 P2） |
| 🟢 P2 | 32 个 | **28 个** | -4（优先级提升） |

### 核心理念

1. **Git 是历史博物馆** - 不要在 src/ 中保留"参考实现"
2. **安全验证不可删除** - 机器人 SDK 的特殊要求
3. **内部代码无需 deprecated** - 你完全控制调用方
4. **测试专用用 cfg(test)** - 更清晰的表达意图

---

## 附录 A: 快速参考

### A.1 立即执行的修复（复制即用）

#### 1. 启用安全验证

```rust
// apps/cli/src/commands/move.rs (或对应的移动命令文件)

use crate::validation::JointValidator;

pub async fn execute(&self) -> Result<()> {
    let positions = self.parse_joints()?;

    // 🔴 P0 安全修复：必须验证关节位置
    let validator = JointValidator::default_range();
    validator.validate_joints(&positions)
        .context("关节位置安全检查失败")?;

    println!("✅ 安全检查通过");

    // ... 继续执行
}
```

#### 2. 删除遗留代码

```bash
# 删除 tx_loop
# crates/piper-driver/src/pipeline.rs

# 删除整个函数（~100 行）
# 在文档中添加 Git 引用：
# 参考：commit <hash> 或 docs/v0/mailbox_pattern_implementation.md
```

#### 3. 删除内部死代码

```bash
# crates/piper-client/src/raw_commander.rs

# 删除 send_pose_with_index 函数（~40 行）
```

---

### A.2 检查命令

```bash
# 搜索所有 allow(dead_code)
grep -rn "allow(dead_code)" --include="*.rs" apps/ crates/

# 检查函数是否真的未被使用
grep -rn "function_name" --include="*.rs" .

# 检查 Serde 相关的死代码
grep -rn "derive.*Deserialize" --include="*.rs" -A 10 apps/ crates/
```

---

**报告生成**: Claude Code（修订版）
**最后更新**: 2025-01-28
**主要修订**: 基于专业反馈，强化安全视角和工程规范
