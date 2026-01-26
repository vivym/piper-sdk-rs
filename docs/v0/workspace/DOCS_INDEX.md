# 📚 Apps 开发文档索引

**最后更新**: 2026-01-26
**状态**: ✅ v2.1 最终版（含代码级建议），可开始实施

---

## 🎯 快速导航

### 实施者必读（按顺序）

1. **[APPS_QUICK_REFERENCE.md](APPS_QUICK_REFERENCE.md)** ⭐ 入门首选
   - 快速概览整个规划
   - 了解双模式架构
   - 查看实施坑点摘要

2. **[APPS_IMPLEMENTATION_GUIDE.md](APPS_IMPLEMENTATION_GUIDE.md)** ⭐⭐ 实施必读
   - 详细代码示例
   - 3个关键实施坑点解决方案
   - rustyline + tokio 异步集成
   - 平台检测和 E-Stop 限制
   - 依赖管理最佳实践

3. **[APPS_DEVELOPMENT_PLAN_V2.md](APPS_DEVELOPMENT_PLAN_V2.md)** 📋 完整规划
   - 架构决策详解
   - 完整时间表
   - 验收标准
   - 架构决策记录 (ADR)

### 审查者参考

4. **[TECHNICAL_REVIEW_SUMMARY.md](TECHNICAL_REVIEW_SUMMARY.md)**
   - 技术审查过程
   - 发现的5个关键问题
   - 修正方案对比

---

## 📊 文档版本对照

| 文档 | 版本 | 用途 | 读者 |
|------|------|------|------|
| **APPS_QUICK_REFERENCE.md** | v2.1 | 快速参考 | 所有人 |
| **APPS_IMPLEMENTATION_GUIDE.md** | v2.1 | 实施指南 | ⭐ 开发者 |
| **APPS_DEVELOPMENT_PLAN_V2.md** | v2.0→v2.1 | 完整规划 | 架构师 |
| **TECHNICAL_REVIEW_SUMMARY.md** | v2.0 | 审查总结 | 审查者 |

---

## 🚀 实施路线图

### Phase 0: 共享基础设施（Day 1）⭐ 最高优先级

**目标**: 定义共享数据结构，避免后期不兼容

**任务**:
- [ ] 创建 `crates/piper-tools`
- [ ] 定义录制格式（`recording.rs`）
- [ ] 定义统计工具（`statistics.rs`）
- [ ] 定义安全配置（`safety.rs`）

**验收**: 单元测试通过，文档完成

**参考**: APPS_IMPLEMENTATION_GUIDE.md - Phase 0 章节

---

### Phase 1: apps/cli（Week 1-3）

**Week 1**: One-shot 模式 + 安全
**Week 2**: REPL 模式（⭐ 注意 rustyline + tokio 冲突）
**Week 3**: 扩展功能

**参考**: APPS_IMPLEMENTATION_GUIDE.md - CLI 实施章节

**工作量**: 7-10 天（保守）/ 14 天（完整）

---

### Phase 2: tools/can-sniffer（Week 4-5）

**Week 4**: TUI + 内核级过滤（⭐ SocketCAN 性能优化）
**Week 5**: 统计 + 录制

**参考**: APPS_IMPLEMENTATION_GUIDE.md - CAN Sniffer 章节

**工作量**: 8-11 天

---

### Phase 3: tools/protocol-analyzer（Week 6）

**Week 6**: 日志分析 + 时间戳处理

**参考**: APPS_IMPLEMENTATION_GUIDE.md - Protocol Analyzer 章节

**工作量**: 6-8 天

---

## ⚠️ 关键实施坑点（v2.1 新增）

### 1. rustyline 与 tokio 冲突 ⭐⭐⭐

**问题**: `rustyline::readline()` 阻塞，影响后台任务

**解决**: **方案 B（推荐）** - 专用输入线程 + mpsc 通道

```rust
// ⭐ 关键：专用线程保留 Editor 历史记录
pub struct ReplInput {
    command_tx: Sender<String>,
    _input_thread: thread::JoinHandle<()>,
}

impl ReplInput {
    pub fn new() -> Self {
        let (command_tx, command_rx) = bounded::<String>(10);
        let input_thread = thread::spawn(move || {
            let mut rl = Editor::<()>::new()?;
            rl.load_history(".piper_history").ok(); // ⭐ 历史持久化

            loop {
                let line = rl.readline("piper> ")?;
                rl.add_history_entry(line.clone()); // ⭐ 添加到历史
                let _ = command_tx.send(line);
            }
        });
        // ...
    }
}
```

**收益**: 保留上下箭头历史记录（用户体验大幅提升）

**详见**: APPS_IMPLEMENTATION_GUIDE.md - 坑点 1（方案 B）

---

### 2. 非 Linux E-Stop 限制 ⭐⭐

**问题**: GS-USB 串口独占锁，外部 `stop` 会失败

**Linux (SocketCAN)**:
```bash
piper-cli move --joints ...
# 另一终端: piper-cli stop  ✅ 可用
```

**Windows/macOS (GS-USB)**:
```bash
piper-cli shell  # ✅ 必须 REPL 模式
piper> move --joints ...
[按 Ctrl+C 进行急停]  # 唯一可靠方式
```

**详见**: APPS_IMPLEMENTATION_GUIDE.md - 坑点 2

---

### 3. 共享库依赖管理 ⭐

**原则**: `piper-tools` 只依赖 `piper-protocol`

**依赖层级**:
```
apps/cli → piper-client → piper-protocol
tools/ → piper-protocol ✅ (不依赖 client)
```

**Feature Flags 优化**（v2.1 代码级建议）:
```toml
# crates/piper-tools/Cargo.toml
[features]
default = []
full = ["statistics"]
statistics = ["dep:statrs"]

[dependencies]
statrs = { version = "0.16", optional = true }
```

**收益**: 编译时间 60s → 15s，可选依赖管理清晰

**详见**: APPS_IMPLEMENTATION_GUIDE.md - 坑点 3（Feature Flags）

---

## ⚠️ 代码级建议（v2.1 新增）

### 1. REPL 历史记录保留 ⭐⭐⭐
**问题**: 方案 A（spawn_blocking）每次创建新 Editor，丢失历史
**解决**: 方案 B（专用线程）- 上下箭头可用
**详见**: APPS_IMPLEMENTATION_GUIDE.md - 错误隔离章节

### 2. Feature Flags 优化 ⭐⭐
**问题**: 所有工具都链接 statrs，编译慢
**解决**: piper-tools 支持 `full` 和 `statistics` features
**详见**: APPS_IMPLEMENTATION_GUIDE.md - piper-tools 依赖配置

### 3. 错误隔离机制 ⭐⭐⭐
**问题**: 用户错误命令导致 REPL panic 崩溃
**解决**: `std::panic::catch_unwind` + 多层防御
**详见**: APPS_IMPLEMENTATION_GUIDE.md - 错误隔离章节

**原则**: "Shell 不应该因为用户输错指令而崩溃"

---

## ✅ 实施前检查清单

### 环境准备
- [ ] Rust 工具链最新版
- [ ] 已读 APPS_IMPLEMENTATION_GUIDE.md ⭐⭐ 必读
- [ ] 已读实施坑点章节
- [ ] 已读代码级建议章节
- [ ] 理解双模式架构设计

### Phase 0 准备
- [ ] 创建 `crates/piper-tools` 目录
- [ ] 编写 `Cargo.toml`（只依赖 piper-protocol + feature flags）
- [ ] 准备单元测试框架

### CLI 开发准备
- [ ] rustyline 文档已读
- [ ] 理解专用输入线程（方案 B）vs spawn_blocking（方案 A）
- [ ] 理解 One-shot vs REPL 模式区别
- [ ] 理解错误隔离（catch_unwind）

---

## 📞 遇到问题？

1. **编译错误**: 检查依赖层级，tools 不应依赖 client
2. **REPL 无历史记录**: 检查是否使用方案 B（专用线程）而非方案 A
3. **REPL 因错误崩溃**: 检查是否使用 catch_unwind
4. **E-Stop 不工作**: 检查平台，非 Linux 需要 REPL 模式
5. **性能问题**: 检查是否使用内核级 CAN 过滤
6. **编译慢**: 检查是否正确使用 feature flags

---

**状态**: ✅ v2.1 最终版（含代码级建议），所有规划文档已完成
**下一步**: 开始 Phase 0 实施准备
**预计完成**: 4-5 周（21-29 工作日）

---

**文档版本**: v1.1（代码级建议）
**最后更新**: 2026-01-26
**维护者**: Piper SDK Team
