#!/bin/bash
# Piper Bridge Host 性能测试脚本

set -e

echo "=========================================="
echo "Piper Bridge Host 性能测试"
echo "=========================================="
echo ""

# 检查 bridge host 是否运行
if [ ! -S "/tmp/piper_bridge.sock" ]; then
    echo "❌ Bridge host 未运行或 socket 文件不存在"
    echo ""
    echo "请先启动 bridge host:"
    echo "  cargo run --bin piper_bridge_host"
    echo ""
    exit 1
fi

echo "✅ Bridge host socket 文件存在"
echo ""

# 运行性能测试
echo "开始运行性能测试..."
echo ""

cargo run --example bridge_latency_bench

echo ""
echo "=========================================="
echo "测试完成"
echo "=========================================="
