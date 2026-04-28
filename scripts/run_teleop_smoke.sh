#!/bin/bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

cd "${REPO_ROOT}"

TIMESTAMP="${TIMESTAMP:-$(date +%Y%m%d-%H%M%S)}"
OUTPUT_DIR="${TELEOP_OUT_DIR:-artifacts/teleop/${TIMESTAMP}}"
RUN_LOG="${OUTPUT_DIR}/run.log"
COMMAND_FILE="${OUTPUT_DIR}/command.txt"
ENV_FILE="${OUTPUT_DIR}/environment.txt"
CALIBRATION_FILE="${CALIBRATION_FILE:-${OUTPUT_DIR}/calib-smoke.toml}"
REPORT_JSON="${REPORT_JSON:-${OUTPUT_DIR}/report-smoke-hold.json}"

MASTER_IFACE="${MASTER_IFACE:-can0}"
SLAVE_IFACE="${SLAVE_IFACE:-can1}"
MODE="${TELEOP_MODE:-master-follower}"
FREQUENCY_HZ="${FREQUENCY_HZ:-100}"
TRACK_KP="${TRACK_KP:-2.0}"
TRACK_KD="${TRACK_KD:-0.4}"
MASTER_DAMPING="${MASTER_DAMPING:-0.8}"
RAW_CLOCK_WARMUP_SECS="${RAW_CLOCK_WARMUP_SECS:-10}"
RAW_CLOCK_SKEW_US="${RAW_CLOCK_SKEW_US:-5000}"
MAX_ITERATIONS="${MAX_ITERATIONS:-300}"
DISABLE_GRIPPER_MIRROR="${DISABLE_GRIPPER_MIRROR:-1}"
DRY_RUN="${DRY_RUN:-0}"

mkdir -p "${OUTPUT_DIR}"

cmd=(
    cargo run -p piper-cli -- teleop dual-arm
    --master-interface "${MASTER_IFACE}"
    --slave-interface "${SLAVE_IFACE}"
    --mode "${MODE}"
    --experimental-calibrated-raw
    --raw-clock-inter-arm-skew-max-us "${RAW_CLOCK_SKEW_US}"
    --frequency-hz "${FREQUENCY_HZ}"
    --track-kp "${TRACK_KP}"
    --track-kd "${TRACK_KD}"
    --master-damping "${MASTER_DAMPING}"
    --raw-clock-warmup-secs "${RAW_CLOCK_WARMUP_SECS}"
    --max-iterations "${MAX_ITERATIONS}"
    --save-calibration "${CALIBRATION_FILE}"
    --report-json "${REPORT_JSON}"
)

if [[ "${DISABLE_GRIPPER_MIRROR}" == "1" ]]; then
    cmd+=(--disable-gripper-mirror)
fi

{
    echo "timestamp=${TIMESTAMP}"
    echo "output_dir=${OUTPUT_DIR}"
    echo "master_iface=${MASTER_IFACE}"
    echo "slave_iface=${SLAVE_IFACE}"
    echo "mode=${MODE}"
    echo "frequency_hz=${FREQUENCY_HZ}"
    echo "track_kp=${TRACK_KP}"
    echo "track_kd=${TRACK_KD}"
    echo "master_damping=${MASTER_DAMPING}"
    echo "raw_clock_warmup_secs=${RAW_CLOCK_WARMUP_SECS}"
    echo "raw_clock_skew_us=${RAW_CLOCK_SKEW_US}"
    echo "max_iterations=${MAX_ITERATIONS}"
    echo "disable_gripper_mirror=${DISABLE_GRIPPER_MIRROR}"
    echo "git_revision=$(git rev-parse HEAD 2>/dev/null || echo unknown)"
} > "${ENV_FILE}"

printf "%q " "${cmd[@]}" > "${COMMAND_FILE}"
printf "\n" >> "${COMMAND_FILE}"

echo "Teleop smoke output: ${OUTPUT_DIR}"
echo "Calibration file: ${CALIBRATION_FILE}"
echo "Report JSON: ${REPORT_JSON}"
echo "Run log: ${RUN_LOG}"
echo
echo "Safety note: support both arms in the intended mirrored zero pose before typing yes."
echo "This script intentionally does not pass --yes; the CLI will require operator confirmation."
echo "After yes, the CLI refreshes raw-clock timing before enabling the arms."
echo
echo "Command:"
cat "${COMMAND_FILE}"
echo

if [[ "${DRY_RUN}" == "1" ]]; then
    echo "DRY_RUN=1 set; command was not executed."
    exit 0
fi

set +e
"${cmd[@]}" 2>&1 | tee "${RUN_LOG}"
status=${PIPESTATUS[0]}
set -e

if [[ -f "${REPORT_JSON}" && -x "$(command -v jq 2>/dev/null)" ]]; then
    echo
    echo "Report summary:"
    jq '{faulted, timing, control, calibration}' "${REPORT_JSON}"
fi

exit "${status}"
