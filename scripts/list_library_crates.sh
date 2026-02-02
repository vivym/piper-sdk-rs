#!/bin/bash
# 列出 workspace 中的所有 library crates（排除 apps/）

set -euo pipefail

# 读取 Cargo.toml 获取成员列表
members=$(grep -A 20 '^members' Cargo.toml | grep '^    "' | sed 's/.*"\(.*\)".*/\1/')

# 收集 crate 名称
crates=""

# 过滤掉 apps/ 目录，只保留 crates/
for member in $members; do
    if [[ "$member" == crates/* ]]; then
        # 提取 crate 名称（crates/piper-driver -> piper-driver）
        crate_name=$(basename "$member")
        crates="$crates-p $crate_name "
    fi
done

# 输出结果（去除末尾空格）
echo -n "${crates% }"
