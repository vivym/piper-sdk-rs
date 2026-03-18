# REPL Implementation

## Current Model

REPL 采用“前台输入 + 后台命令 worker”模型：

- 输入线程只负责 readline、历史记录和把命令通过长期存在的 Tokio channel 转发到主循环
- 主循环只负责：
  - 读取输入
  - 提交命令到后台 worker
  - 处理 `stop` / `Ctrl+C`
  - 接收命令完成事件
- 命令 worker 独占 `ReplSession`，因此不会破坏 `Piper` 的 type-state 唯一所有权

## Why This Exists

旧实现把 `move` / `home` / `park` 直接在主循环里同步执行。  
这些命令本身又是阻塞式轮询，所以 REPL 在运动期间无法真正处理 `Ctrl+C`。

当前实现把命令挪到后台执行，并为运动命令增加取消标志：

1. 前台检测到 `stop` 或 `Ctrl+C`
2. 若当前是运动命令，则立即置取消标志
3. 运动 helper 在下一轮 poll / republish 前检查取消标志并返回 `Cancelled`
4. worker 在命令退出后统一执行 `disable_all()`
5. 会话最终回到 `Standby`

## Stop Semantics

`stop` 与 `Ctrl+C` 完全统一：

- 空闲时：直接执行 `disable_all()`
- 运动中：取消当前运动，然后执行 `disable_all()`
- 非运动命令运行中：急停会被排队到当前命令结束后执行

REPL 的急停语义固定为：

- `disable_all()`
- 保持连接
- 最终状态为 `Standby`

## Confirmation Policy

REPL 不支持后台交互式确认。

- `move` 若触发大幅移动保护，必须显式加 `--force`
- `set-zero` 必须显式加 `--force`
- 若需要交互确认，使用 one-shot CLI

## Busy Policy

命令执行期间：

- 接受：`stop`、`Ctrl+C`、`exit`
- 拒绝：其他普通命令

这样可以避免在一个未完成的 type-state 迁移过程中再叠加第二个命令。

输入线程关闭（例如 `Ctrl+D` / EOF）时：

- 空闲状态：shell 立即退出
- 忙碌状态：先请求急停或排队 stop，待当前命令收尾后退出

## Motion Workflow

`move` / `home` / `park` 共享 `piper-control` 的 blocking helper：

- 首次发送目标
- 周期性重发目标
- 轮询当前位置
- 达到阈值则成功
- 超时则失败
- 收到取消标志则返回 `Cancelled`

REPL 只在这一层额外加了取消控制；one-shot 和脚本仍然使用不可取消的 blocking 变体。
