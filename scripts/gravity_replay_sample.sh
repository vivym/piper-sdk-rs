#!/bin/bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

cd "${REPO_ROOT}"

ROLE="${ROLE:-slave}"
IFACE="${IFACE:-can1}"
TARGET="${TARGET:-}"
PATH_FILE="${PATH_FILE:-${1:-}}"
JOINT_MAP="${JOINT_MAP:-identity}"
LOAD_PROFILE="${LOAD_PROFILE:-normal-gripper-d405}"
MAX_VELOCITY_RAD_S="${MAX_VELOCITY_RAD_S:-0.08}"
MAX_STEP_RAD="${MAX_STEP_RAD:-0.02}"
SETTLE_MS="${SETTLE_MS:-500}"
SAMPLE_MS="${SAMPLE_MS:-300}"
BIDIRECTIONAL="${BIDIRECTIONAL:-1}"
DRY_RUN="${DRY_RUN:-0}"
SKIP_BUILD="${SKIP_BUILD:-0}"
PRINT_ONLY="${PRINT_ONLY:-0}"

if [[ -z "${PATH_FILE}" ]]; then
    echo "Usage: PATH_FILE=path.jsonl $0 or $0 path.jsonl" >&2
    exit 2
fi

default_out="${PATH_FILE%.path.jsonl}.samples.jsonl"
if [[ "${default_out}" == "${PATH_FILE}" ]]; then
    default_out="${PATH_FILE%.jsonl}.samples.jsonl"
fi
OUT="${OUT:-${default_out}}"

mkdir -p "$(dirname "${OUT}")"

STEM="${OUT%.jsonl}"
BUILD_LOG="${BUILD_LOG:-${STEM}.build.log}"
RUN_LOG="${RUN_LOG:-${STEM}.run.log}"
COMMAND_FILE="${COMMAND_FILE:-${STEM}.command.txt}"
ENV_FILE="${ENV_FILE:-${STEM}.environment.txt}"

build_cmd=(cargo build -p piper-cli)
cmd=(
    target/debug/piper-cli gravity replay-sample
    --role "${ROLE}"
    --path "${PATH_FILE}"
    --out "${OUT}"
    --max-velocity-rad-s "${MAX_VELOCITY_RAD_S}"
    --max-step-rad "${MAX_STEP_RAD}"
    --settle-ms "${SETTLE_MS}"
    --sample-ms "${SAMPLE_MS}"
)

if [[ -n "${TARGET}" ]]; then
    cmd+=(--target "${TARGET}")
else
    cmd+=(--interface "${IFACE}")
fi

if [[ "${BIDIRECTIONAL}" != "1" ]]; then
    cmd+=(--no-bidirectional)
fi

if [[ "${DRY_RUN}" == "1" ]]; then
    cmd+=(--dry-run)
fi

{
    echo "role=${ROLE}"
    echo "iface=${IFACE}"
    echo "target=${TARGET}"
    echo "path_file=${PATH_FILE}"
    echo "out=${OUT}"
    echo "joint_map=${JOINT_MAP}"
    echo "load_profile=${LOAD_PROFILE}"
    echo "max_velocity_rad_s=${MAX_VELOCITY_RAD_S}"
    echo "max_step_rad=${MAX_STEP_RAD}"
    echo "settle_ms=${SETTLE_MS}"
    echo "sample_ms=${SAMPLE_MS}"
    echo "bidirectional=${BIDIRECTIONAL}"
    echo "dry_run=${DRY_RUN}"
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

echo "Gravity replay sampling"
echo "Path file: ${PATH_FILE}"
echo "Samples output: ${OUT}"
echo "Run log: ${RUN_LOG}"
echo
if [[ "${DRY_RUN}" == "1" ]]; then
    echo "DRY_RUN=1: replay will be planned but the robot will not move."
else
    echo "Safety note: this step moves the robot. Confirm the replay path is collision-free before accepting the CLI prompt."
fi
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

if [[ ! -f "${PATH_FILE}" ]]; then
    echo "Path file does not exist: ${PATH_FILE}" >&2
    exit 1
fi

if [[ "${DRY_RUN}" != "1" && -e "${OUT}" ]]; then
    echo "Refusing to overwrite existing output: ${OUT}" >&2
    exit 1
fi

if [[ "${SKIP_BUILD}" != "1" ]]; then
    "${build_cmd[@]}" 2>&1 | tee "${BUILD_LOG}"
fi

if [[ -r /dev/tty && "${DRY_RUN}" != "1" ]]; then
    "${cmd[@]}" < /dev/tty 2>&1 | tee "${RUN_LOG}"
else
    "${cmd[@]}" 2>&1 | tee "${RUN_LOG}"
fi
