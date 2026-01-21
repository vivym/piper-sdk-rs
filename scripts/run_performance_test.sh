#!/bin/bash
# GS-USB Daemon 性能测试脚本

set -e

echo "=========================================="
echo "GS-USB Daemon 性能测试"
echo "=========================================="
echo ""

# 检查 daemon 是否运行
if [ ! -S "/tmp/gs_usb_daemon.sock" ]; then
    echo "❌ Daemon 未运行或 socket 文件不存在"
    echo ""
    echo "请先启动 daemon:"
    echo "  cargo run --bin gs_usb_daemon"
    echo ""
    exit 1
fi

echo "✅ Daemon socket 文件存在"
echo ""

# 运行性能测试
echo "开始运行性能测试..."
echo ""

cargo run --example daemon_latency_bench

echo ""
echo "=========================================="
echo "测试完成"
echo "=========================================="

