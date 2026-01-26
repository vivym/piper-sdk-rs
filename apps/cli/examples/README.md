# Piper CLI 示例脚本

本目录包含用于演示 Piper CLI 脚本系统的示例 JSON 脚本。

## 示例脚本

### move_sequence.json
一个简单的移动序列示例，演示基本命令：
- Home（回零位）
- Wait（等待）
- Move（移动到指定位置）
- Position（查询位置）

### test_sequence.json
更完整的测试序列，包含多次移动和位置查询。

## 使用方法

### 1. 执行脚本

```bash
# 基本执行
piper-cli run --script examples/move_sequence.json

# 失败时继续执行
piper-cli run --script examples/move_sequence.json --continue-on-error

# 指定接口
piper-cli run --script examples/move_sequence.json --interface can0
```

### 2. 创建自定义脚本

你可以基于这些示例创建自己的脚本。脚本格式：

```json
{
  "name": "脚本名称",
  "description": "脚本描述",
  "commands": [
    {
      "type": "CommandType",
      "参数": "值"
    }
  ]
}
```

### 支持的命令类型

#### Home
回到零位：
```json
{ "type": "Home" }
```

#### Move
移动到目标位置（弧度）：
```json
{
  "type": "Move",
  "joints": [0.1, 0.2, 0.3, 0.4, 0.5, 0.6],
  "force": false
}
```

#### Wait
等待指定毫秒数：
```json
{
  "type": "Wait",
  "duration_ms": 1000
}
```

#### Position
查询当前位置：
```json
{ "type": "Position" }
```

#### Stop
紧急停止：
```json
{ "type": "Stop" }
```

## 注意事项

1. **角度单位**：Move 命令中的关节角度使用弧度制
2. **关节顺序**：joints 数组按 [J1, J2, J3, J4, J5, J6] 顺序
3. **安全确认**：大幅移动（>10°）会要求确认，除非 force 设置为 true
4. **错误处理**：默认情况下，任何命令失败都会停止脚本执行。使用 `--continue-on-error` 可在失败时继续

## 示例工作流程

```bash
# 1. 监控机器人状态
piper-cli monitor --frequency 10

# 2. 执行测试脚本
piper-cli run --script examples/test_sequence.json

# 3. 查询当前位置
piper-cli position

# 4. 回到零位
piper-cli home
```

## 进阶用法

### 条件等待
```json
{
  "type": "Wait",
  "duration_ms": 2000
}
```

### 多段移动
```json
{
  "type": "Move",
  "joints": [0.1, 0.2, 0.3, 0.4, 0.5, 0.6],
  "force": false
},
{
  "type": "Wait",
  "duration_ms": 1000
},
{
  "type": "Move",
  "joints": [0.2, 0.3, 0.4, 0.5, 0.6, 0.7],
  "force": false
}
```

### 录制和回放
```bash
# 录制机器人运动
piper-cli record --output my_recording.bin --duration 30

# 回放录制
piper-cli replay --input my_recording.bin --speed 1.0
```

## 故障排除

### 脚本解析失败
检查 JSON 格式是否正确，可以使用在线 JSON 验证工具。

### 连接失败
确保 CAN 接口配置正确：
```bash
piper-cli config check
```

### 移动被阻止
检查是否需要确认：
```bash
piper-cli run --script examples/move_sequence.json
```
如果提示确认，输入 `y` 继续。

## 更多信息

参考主 README.md 了解更多命令和功能。
