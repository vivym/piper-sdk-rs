#!/bin/bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

cd "${REPO_ROOT}"

TIMESTAMP="${TIMESTAMP:-$(date +%Y%m%d-%H%M%S)}"
ROLE="${ROLE:-slave}"
IFACE="${IFACE:-can1}"
TARGET="${TARGET:-}"
JOINT_MAP="${JOINT_MAP:-identity}"
LOAD_PROFILE="${LOAD_PROFILE:-normal-gripper-d405}"
FREQUENCY_HZ="${FREQUENCY_HZ:-50}"
NOTES="${NOTES:-}"
OUTPUT_DIR="${GRAVITY_OUT_DIR:-artifacts/gravity/${ROLE}}"
OUT="${OUT:-${OUTPUT_DIR}/${TIMESTAMP}.path.jsonl}"
SKIP_BUILD="${SKIP_BUILD:-0}"
PRINT_ONLY="${PRINT_ONLY:-0}"

mkdir -p "$(dirname "${OUT}")"

STEM="${OUT%.jsonl}"
BUILD_LOG="${BUILD_LOG:-${STEM}.build.log}"
RUN_LOG="${RUN_LOG:-${STEM}.run.log}"
COMMAND_FILE="${COMMAND_FILE:-${STEM}.command.txt}"
ENV_FILE="${ENV_FILE:-${STEM}.environment.txt}"

build_cmd=(cargo build -p piper-cli)
cmd=(
    target/debug/piper-cli gravity record-path
    --role "${ROLE}"
    --joint-map "${JOINT_MAP}"
    --load-profile "${LOAD_PROFILE}"
    --out "${OUT}"
    --frequency-hz "${FREQUENCY_HZ}"
)

if [[ -n "${TARGET}" ]]; then
    cmd+=(--target "${TARGET}")
else
    cmd+=(--interface "${IFACE}")
fi

if [[ -n "${NOTES}" ]]; then
    cmd+=(--notes "${NOTES}")
fi

{
    echo "timestamp=${TIMESTAMP}"
    echo "role=${ROLE}"
    echo "iface=${IFACE}"
    echo "target=${TARGET}"
    echo "joint_map=${JOINT_MAP}"
    echo "load_profile=${LOAD_PROFILE}"
    echo "frequency_hz=${FREQUENCY_HZ}"
    echo "notes=${NOTES}"
    echo "out=${OUT}"
    echo "skip_build=${SKIP_BUILD}"
    echo "print_only=${PRINT_ONLY}"
    echo "git_revision=$(git rev-parse HEAD 2>/dev/null || echo unknown)"
} > "${ENV_FILE}"

{
    echo "Build command:"
    printf "%q " "${build_cmd[@]}"
    printf "\n\nRun command:\n"
    printf "%q " "${cmd[@]}"
    printf "\n"
} > "${COMMAND_FILE}"

echo "Gravity path recording"
echo "Output path: ${OUT}"
echo "Run log: ${RUN_LOG}"
echo
echo "Safety note: this is passive recording. Keep the robot in Standby/disabled and move the arm by hand."
echo "Only record poses that can be replayed safely later."
echo "Press Ctrl-C to stop recording."
echo
echo "Commands:"
cat "${COMMAND_FILE}"
echo

if [[ "${PRINT_ONLY}" == "1" ]]; then
    echo "Environment:"
    cat "${ENV_FILE}"
    echo
    echo "PRINT_ONLY=1 set; command was not executed."
    exit 0
fi

if [[ -e "${OUT}" ]]; then
    echo "Refusing to overwrite existing output: ${OUT}" >&2
    exit 1
fi

if [[ "${SKIP_BUILD}" != "1" ]]; then
    "${build_cmd[@]}" 2>&1 | tee "${BUILD_LOG}"
fi

"${cmd[@]}" 2>&1 | tee "${RUN_LOG}"
