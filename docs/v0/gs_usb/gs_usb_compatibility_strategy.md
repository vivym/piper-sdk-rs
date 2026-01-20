# GS-USB 兼容性策略（HOST_FORMAT 等历史行为）

## 结论（当前默认行为）

- **默认不发送** `HOST_FORMAT (GS_USB_BREQ_HOST_FORMAT / 0xBEEF)`。
- 原因：
  - 默认实现通常不发送该请求；
  - 绝大多数固件默认按 little-endian 解析；
  - 把“是否握手”作为隐式副作用会扩大排障面，且容易形成长期 API 负债。

## 什么时候需要考虑启用/恢复 HOST_FORMAT

仅当满足以下条件之一时才考虑：

- 某个特定设备/固件在未发送 HOST_FORMAT 时 **稳定无法工作**，且能提供：
  - 设备型号/VIDPID/固件版本
  - 复现步骤与日志
  - 发送 HOST_FORMAT 后问题消失的对比证据

## 推荐实现方式（未来扩展点）

- 不建议恢复为“默认发送”，而是：
  - 引入显式配置项（例如 `StartOptions { handshake: bool }`），默认关闭
  - 或者针对特定 VID/PID/固件版本做白名单兼容（并写清楚文档）


