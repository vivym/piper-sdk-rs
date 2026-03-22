#!/bin/bash

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

cd "${REPO_ROOT}" || exit 1

MODE="${1:-all}"
SOCKETCAN_IFACE="${PIPER_TEST_SOCKETCAN_IFACE:-can0}"
TIMESTAMP="$(date +%Y%m%d-%H%M%S)"
OUTPUT_DIR="${PIPER_ACCEPTANCE_OUT_DIR:-artifacts/realtime_acceptance/${TIMESTAMP}}"
CONTINUE_ON_FAILURE="${PIPER_ACCEPTANCE_CONTINUE_ON_FAILURE:-0}"
SUMMARY_FILE="${OUTPUT_DIR}/summary.md"
LOG_DIR="${OUTPUT_DIR}/logs"
LAST_STATUS=0
STOP_REQUESTED=0

declare -a FAILURES=()

mkdir -p "${LOG_DIR}"

write_summary_header() {
    cat > "${SUMMARY_FILE}" <<EOF
# Realtime Acceptance Run

- Timestamp: ${TIMESTAMP}
- Mode: ${MODE}
- SocketCAN iface: ${SOCKETCAN_IFACE}
- Output dir: ${OUTPUT_DIR}
- Host: $(uname -a)
- Git revision: $(git rev-parse HEAD 2>/dev/null || echo unknown)

## Commands

EOF
}

capture_environment() {
    {
        echo "timestamp=${TIMESTAMP}"
        echo "mode=${MODE}"
        echo "socketcan_iface=${SOCKETCAN_IFACE}"
        echo "pwd=$(pwd)"
        echo "host=$(uname -a)"
        echo "rustc=$(rustc --version 2>/dev/null || echo unavailable)"
        echo "cargo=$(cargo --version 2>/dev/null || echo unavailable)"
        echo "git_revision=$(git rev-parse HEAD 2>/dev/null || echo unknown)"
    } > "${OUTPUT_DIR}/environment.txt"

    git status --short > "${OUTPUT_DIR}/git-status.txt" 2>/dev/null || true
    git diff --stat > "${OUTPUT_DIR}/git-diff-stat.txt" 2>/dev/null || true
    git diff --name-only > "${OUTPUT_DIR}/git-diff-files.txt" 2>/dev/null || true
}

append_summary_result() {
    local name="$1"
    local status="$2"
    local log_file="$3"

    # shellcheck disable=SC2006
    printf -- "- `%s`: %s ([log](%s))\n" "${name}" "${status}" "${log_file}" >> "${SUMMARY_FILE}"
}

run_logged() {
    local name="$1"
    local command="$2"
    local log_file="${LOG_DIR}/${name}.log"

    echo "=========================================="
    echo "${name}"
    echo "=========================================="
    echo "\$ ${command}"
    echo ""

    {
        echo "\$ ${command}"
        echo ""
        bash -lc "${command}"
    } 2>&1 | tee "${log_file}"
    local status=${PIPESTATUS[0]}

    if [[ ${status} -eq 0 ]]; then
        append_summary_result "${name}" "PASS" "logs/${name}.log"
    else
        append_summary_result "${name}" "FAIL (${status})" "logs/${name}.log"
        FAILURES+=("${name}")
    fi
    LAST_STATUS=${status}
    return 0
}

run_optional_logged() {
    local name="$1"
    local command="$2"
    local log_file="${LOG_DIR}/${name}.log"

    echo "=========================================="
    echo "${name}"
    echo "=========================================="
    echo "\$ ${command}"
    echo ""

    {
        echo "\$ ${command}"
        echo ""
        bash -lc "${command}"
    } 2>&1 | tee "${log_file}"
    local status=${PIPESTATUS[0]}

    if [[ ${status} -eq 0 ]]; then
        append_summary_result "${name}" "PASS" "logs/${name}.log"
    else
        append_summary_result "${name}" "INFO (${status})" "logs/${name}.log"
    fi
    LAST_STATUS=${status}
    return 0
}

run_common_preflight() {
    run_optional_logged "preflight-rust-toolchain" "rustc --version && cargo --version"
    run_optional_logged "preflight-git-head" "git rev-parse HEAD && git status --short"
    run_optional_logged \
        "preflight-runtime-limits" \
        "ulimit -a && if command -v chrt >/dev/null 2>&1; then echo && chrt -m; else echo && echo 'chrt command not available'; fi"
}

run_socketcan_preflight() {
    run_optional_logged \
        "socketcan-preflight-ip-link" \
        "if command -v ip >/dev/null 2>&1; then ip -details link show \"${SOCKETCAN_IFACE}\"; else echo 'ip command not available'; fi"
    run_optional_logged \
        "socketcan-preflight-sysfs" \
        "if [[ -d \"/sys/class/net/${SOCKETCAN_IFACE}\" ]]; then ls -la \"/sys/class/net/${SOCKETCAN_IFACE}\" && cat \"/sys/class/net/${SOCKETCAN_IFACE}/operstate\" 2>/dev/null || true; else echo 'sysfs entry not available'; fi"
}

run_gs_usb_preflight() {
    run_optional_logged \
        "gs-usb-preflight-lsusb" \
        "if command -v lsusb >/dev/null 2>&1; then lsusb; else echo 'lsusb command not available'; fi"
    run_optional_logged \
        "gs-usb-preflight-system-profiler" \
        "if command -v system_profiler >/dev/null 2>&1; then system_profiler SPUSBDataType; else echo 'system_profiler command not available'; fi"
}

run_socketcan_diagnostics() {
    run_logged \
        "socketcan-ip-link" \
        "if command -v ip >/dev/null 2>&1; then ip -details link show \"${SOCKETCAN_IFACE}\"; else echo 'ip command not available'; fi"
    run_optional_logged \
        "socketcan-ip-stats" \
        "if command -v ip >/dev/null 2>&1; then ip -statistics link show \"${SOCKETCAN_IFACE}\"; else echo 'ip command not available'; fi"
    run_optional_logged \
        "socketcan-kernel-log" \
        "if command -v journalctl >/dev/null 2>&1; then journalctl -k -n 200 --no-pager; elif command -v dmesg >/dev/null 2>&1; then dmesg | tail -n 200; else echo 'kernel log command not available'; fi"
}

run_gs_usb_diagnostics() {
    run_logged \
        "gs-usb-debug-scan" \
        "cargo test --test gs_usb_debug_scan -- --ignored --nocapture --test-threads=1"
    run_optional_logged \
        "gs-usb-lsusb" \
        "if command -v lsusb >/dev/null 2>&1; then lsusb -v; else echo 'lsusb command not available'; fi"
    run_optional_logged \
        "gs-usb-kernel-log" \
        "if command -v journalctl >/dev/null 2>&1; then journalctl -k -n 200 --no-pager; elif command -v dmesg >/dev/null 2>&1; then dmesg | tail -n 200; else echo 'kernel log command not available'; fi"
}

run_socketcan_strict() {
    local failures_before=${#FAILURES[@]}
    run_socketcan_preflight

    run_logged \
        "socketcan-timeout-config" \
        "PIPER_TEST_SOCKETCAN_IFACE=\"${SOCKETCAN_IFACE}\" cargo test --test timeout_convergence_tests test_socketcan_timeout_config -- --ignored --nocapture"
    if [[ ${LAST_STATUS} -ne 0 && "${CONTINUE_ON_FAILURE}" != "1" ]]; then
        STOP_REQUESTED=1
    fi
    if [[ "${STOP_REQUESTED}" == "1" ]]; then
        run_socketcan_diagnostics || true
        return 0
    fi

    run_logged \
        "socketcan-rx-500hz-benchmark" \
        "cargo test --test realtime_benchmark_tests test_500hz_realtime_benchmark -- --ignored --nocapture"
    if [[ ${LAST_STATUS} -ne 0 && "${CONTINUE_ON_FAILURE}" != "1" ]]; then
        STOP_REQUESTED=1
    fi
    if [[ "${STOP_REQUESTED}" == "1" ]]; then
        run_socketcan_diagnostics || true
        return 0
    fi

    run_logged \
        "socketcan-tx-latency-benchmark" \
        "cargo test --test realtime_benchmark_tests test_tx_latency_benchmark -- --ignored --nocapture"
    if [[ ${LAST_STATUS} -ne 0 && "${CONTINUE_ON_FAILURE}" != "1" ]]; then
        STOP_REQUESTED=1
    fi
    if [[ "${STOP_REQUESTED}" == "1" ]]; then
        run_socketcan_diagnostics || true
        return 0
    fi

    run_logged \
        "socketcan-send-duration-benchmark" \
        "cargo test --test realtime_benchmark_tests test_send_duration_benchmark -- --ignored --nocapture"
    if [[ ${LAST_STATUS} -ne 0 && "${CONTINUE_ON_FAILURE}" != "1" ]]; then
        STOP_REQUESTED=1
    fi

    if [[ ${#FAILURES[@]} -gt ${failures_before} ]]; then
        run_socketcan_diagnostics || true
    fi
}

run_gs_usb_soft() {
    local failures_before=${#FAILURES[@]}
    run_gs_usb_preflight

    run_logged \
        "gs-usb-timeout-config" \
        "cargo test --test timeout_convergence_tests test_gs_usb_timeout_config -- --ignored --nocapture --test-threads=1"
    if [[ ${LAST_STATUS} -ne 0 && "${CONTINUE_ON_FAILURE}" != "1" ]]; then
        STOP_REQUESTED=1
    fi
    if [[ "${STOP_REQUESTED}" == "1" ]]; then
        run_gs_usb_diagnostics || true
        return 0
    fi

    run_logged \
        "gs-usb-performance-suite" \
        "cargo test --test gs_usb_performance_tests -- --ignored --nocapture --test-threads=1"
    if [[ ${LAST_STATUS} -ne 0 && "${CONTINUE_ON_FAILURE}" != "1" ]]; then
        STOP_REQUESTED=1
    fi

    if [[ ${#FAILURES[@]} -gt ${failures_before} ]]; then
        run_gs_usb_diagnostics || true
    fi
}

finalize_summary() {
    {
        echo ""
        echo "## Result"
        if [[ ${#FAILURES[@]} -eq 0 ]]; then
            echo ""
            echo "- Overall: PASS"
        else
            echo ""
            echo "- Overall: FAIL"
            echo "- Failed steps: ${FAILURES[*]}"
        fi
    } >> "${SUMMARY_FILE}"
}

write_summary_header
capture_environment
run_common_preflight

case "${MODE}" in
    socketcan-strict)
        run_socketcan_strict
        ;;
    gs-usb-soft)
        run_gs_usb_soft
        ;;
    all)
        run_socketcan_strict
        if [[ "${STOP_REQUESTED}" != "1" ]]; then
            run_gs_usb_soft
        fi
        ;;
    *)
        echo "用法: $0 [socketcan-strict|gs-usb-soft|all]"
        echo "环境变量:"
        echo "  PIPER_TEST_SOCKETCAN_IFACE          SocketCAN 接口名，默认 can0"
        echo "  PIPER_ACCEPTANCE_OUT_DIR            验收产物目录，默认 artifacts/realtime_acceptance/<timestamp>"
        echo "  PIPER_ACCEPTANCE_CONTINUE_ON_FAILURE 设为 1 时，失败后继续执行后续命令"
        exit 1
        ;;
esac

finalize_summary

if [[ ${#FAILURES[@]} -ne 0 ]]; then
    echo ""
    echo "Acceptance failed. Artifacts: ${OUTPUT_DIR}"
    exit 1
fi

echo ""
echo "Acceptance passed. Artifacts: ${OUTPUT_DIR}"
