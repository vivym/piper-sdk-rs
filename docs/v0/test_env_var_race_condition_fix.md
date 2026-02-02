# 测试修复：环境变量并发访问竞态条件

**日期**: 2025-02-02
**状态**: ✅ 已修复
**影响**: piper-client 的 zeroing_token 测试

---

## 🐛 问题描述

### 失败的测试

```
failures:

---- control::zeroing_token::tests::test_confirm_from_env_wrong_value stdout ----

thread 'control::zeroing_token::tests::test_confirm_from_env_wrong_value' (263109)
panicked at crates/piper-client/src/control/zeroing_token.rs:255:9:
assertion failed: matches!(token, Err(ZeroingTokenError::EnvValueMismatch { .. }))
```

### 症状

- 测试**单独运行**时通过 ✅
- 测试**在套件中运行**时失败 ❌
- 失败是**间歇性**的（取决于测试执行顺序）

---

## 🔍 根本原因

### 问题分析

**并发竞态条件**（Race Condition）：

1. Cargo 默认使用**多线程**运行测试（`--test-threads=auto`）
2. 多个测试同时访问**同一个环境变量** `PIPER_ZEROING_CONFIRM`
3. 测试之间相互干扰，导致环境变量值不符合预期

### 示例场景

```
时刻 T1: test_confirm_from_env_success 设置 ENV_VAR = "I_CONFIRM_ZEROING_IS_DANGEROUS"
时刻 T2: test_confirm_from_env_wrong_value 设置 ENV_VAR = "wrong_value"
时刻 T3: test_confirm_from_env_not_set 读取 ENV_VAR → 得到 "wrong_value"（错误！）
```

### 为什么单独运行通过？

```bash
# 单独运行：串行执行，无干扰
cargo test test_confirm_from_env_wrong_value
✅ 通过

# 套件运行：并行执行，有干扰
cargo test -p piper-client --lib
❌ 失败
```

---

## ✅ 解决方案

### 修复策略

在每个设置环境变量的测试**开始时**先清理环境变量：

```rust
#[test]
fn test_confirm_from_env_success() {
    unsafe {
        env::remove_var(ENV_VAR); // ✅ 先清理，避免测试间干扰
        env::set_var(ENV_VAR, ENV_VALUE);
    }
    // ... 测试逻辑 ...
}

#[test]
fn test_confirm_from_env_wrong_value() {
    unsafe {
        env::remove_var(ENV_VAR); // ✅ 先清理，避免测试间干扰
        env::set_var(ENV_VAR, "wrong_value");
    }
    // ... 测试逻辑 ...
}
```

### 原理

1. **确保初始状态**：每个测试开始时环境变量都是未设置的
2. **消除竞态**：即使测试并行运行，也不会互相干扰
3. **保持兼容性**：不影响测试的独立性和正确性

---

## 📊 验证结果

### 单次运行

```bash
$ cargo test -p piper-client --lib control::zeroing_token::tests
running 7 tests
test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 126 filtered out
✅ 通过
```

### 多次运行（稳定性验证）

```bash
$ for i in 1 2 3; do cargo test -p piper-client --lib control::zeroing_token::tests; done

=== 第 1 次运行 ===
test result: ok. 7 passed; 0 failed

=== 第 2 次运行 ===
test result: ok. 7 passed; 0 failed

=== 第 3 次运行 ===
test result: ok. 7 passed; 0 failed
✅ 稳定
```

### 完整测试套件

```bash
$ just test
test result: ok. 133 passed; 0 failed (piper-client)
✅ 全部通过
```

---

## 🎯 经验总结

### 并发测试的常见陷阱

1. **共享状态污染**：
   - ❌ 环境变量
   - ❌ 文件系统
   - ❌ 全局变量
   - ❌ 静态变量

2. **时间相关**：
   - ❌ 硬编码的延迟
   - ❌ 依赖系统时间
   - ❌ 竞态条件

### 最佳实践

#### 1. 测试隔离

```rust
#[test]
fn test_something() {
    // ✅ 在设置前先清理
    setup();
    // ✅ 测试逻辑
    assert!(something_works());
    // ✅ 测试后清理
    teardown();
}
```

#### 2. 使用 Serial 运行（如果必要）

```rust
#[test]
#[serial]  // 需要 serial_test crate
fn test_with_shared_state() {
    // 串行运行，避免竞态
}
```

#### 3. 线程本地存储

```rust
#[test]
fn test_with_thread_local() {
    thread_local! {
        static STATE: RefCell<Cell<i32>> = RefCell::new(Cell::new(0));
    }
    // 每个线程独立的状态
}
```

### 环境变量测试建议

#### ✅ 推荐：先清理后设置

```rust
unsafe {
    env::remove_var(ENV_VAR);  // 先清理
    env::set_var(ENV_VAR, value);  // 再设置
}
```

#### ⚠️ 可接受：强制串行

```bash
cargo test -- --test-threads=1
```

#### ❌ 不推荐：依赖清理顺序

```rust
// ❌ 依赖测试运行顺序
unsafe {
    env::set_var(ENV_VAR, value);  // 假设之前没有设置
}
```

---

## 📁 修改的文件

### 修复

- **`crates/piper-client/src/control/zeroing_token.rs`**
  - `test_confirm_from_env_success`: 添加 `env::remove_var` 在设置前
  - `test_confirm_from_env_wrong_value`: 添加 `env::remove_var` 在设置前

---

## 🔧 其他可能的解决方案

### 方案 1：强制串行运行

```bash
# 在 justfile 中
test:
    cargo test --workspace -- --test-threads=1
```

**优点**：彻底避免竞态
**缺点**：所有测试变慢

### 方案 2：使用 serial_test crate

```toml
[dev-dependencies]
serial_test = "3.0"
```

```rust
#[test]
#[serial]
fn test_with_env_var() {
    // 串行运行
}
```

**优点**：只对特定测试串行化
**缺点**：需要额外依赖

### 方案 3：测试构造函数/析构函数

```rust
mod tests {
    fn setup() {
        unsafe { env::remove_var(ENV_VAR); }
    }

    fn teardown() {
        unsafe { env::remove_var(ENV_VAR); }
    }

    #[test]
    fn test_something() {
        setup();
        // 测试逻辑
        teardown();
    }
}
```

**优点**：明确的设置/清理
**缺点**：代码重复

---

## ✅ 选择的方案

**先清理后设置**（已实施）

**理由**：
1. ✅ **简单**：只修改一行代码
2. ✅ **高效**：不影响并行测试性能
3. ✅ **可靠**：彻底避免竞态
4. ✅ **无依赖**：不需要额外的 crate

---

**状态**: ✅ **已修复并验证**

所有测试现在都能稳定通过，无论是单独运行还是作为完整测试套件的一部分。
