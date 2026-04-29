#!/bin/bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

cd "${REPO_ROOT}"

TIMESTAMP="${TIMESTAMP:-$(date +%Y%m%d-%H%M%S)}"
OUTPUT_DIR="${TELEOP_OUT_DIR:-artifacts/teleop/${TIMESTAMP}}"
RUN_LOG="${OUTPUT_DIR}/run.log"
BUILD_LOG="${OUTPUT_DIR}/build.log"
COMMAND_FILE="${OUTPUT_DIR}/command.txt"
ENV_FILE="${OUTPUT_DIR}/environment.txt"
CALIBRATION_FILE="${CALIBRATION_FILE:-${OUTPUT_DIR}/calib-smoke.toml}"
REPORT_JSON="${REPORT_JSON:-${OUTPUT_DIR}/report-smoke-hold.json}"

MASTER_IFACE="${MASTER_IFACE:-can0}"
SLAVE_IFACE="${SLAVE_IFACE:-can1}"
MODE="${TELEOP_MODE:-master-follower}"
JOINT_MAP="${JOINT_MAP:-identity}"
FREQUENCY_HZ="${FREQUENCY_HZ:-100}"
TRACK_KP="${TRACK_KP:-2.0}"
TRACK_KD="${TRACK_KD:-0.4}"
MASTER_DAMPING="${MASTER_DAMPING:-0.8}"
RAW_CLOCK_WARMUP_SECS="${RAW_CLOCK_WARMUP_SECS:-10}"
# Observed SocketCAN raw-clock residual p95 can approach ~1.5ms during
# longer smoke runs; keep p95 materially below isolated max-spike tolerance.
RAW_CLOCK_RESIDUAL_P95_US="${RAW_CLOCK_RESIDUAL_P95_US:-2000}"
# Long smoke runs can see isolated residual spikes just above 2ms while p95
# remains healthy; keep the max gate below the 6ms inter-arm skew guardrail.
RAW_CLOCK_RESIDUAL_MAX_US="${RAW_CLOCK_RESIDUAL_MAX_US:-3000}"
RAW_CLOCK_RESIDUAL_MAX_CONSECUTIVE_FAILURES="${RAW_CLOCK_RESIDUAL_MAX_CONSECUTIVE_FAILURES:-3}"
# A single missed raw-feedback period can produce an observed ~30ms gap while
# the latest sample remains fresh. Keep this fail-fast gate above that one-gap
# smoke-test tail without relaxing freshness or inter-arm skew checks.
RAW_CLOCK_SAMPLE_GAP_MAX_MS="${RAW_CLOCK_SAMPLE_GAP_MAX_MS:-50}"
RAW_CLOCK_ALIGNMENT_LAG_US="${RAW_CLOCK_ALIGNMENT_LAG_US:-5000}"
RAW_CLOCK_ALIGNMENT_BUFFER_MISS_CONSECUTIVE_FAILURES="${RAW_CLOCK_ALIGNMENT_BUFFER_MISS_CONSECUTIVE_FAILURES:-3}"
# Keep the smoke gate at one 100Hz control tick while allowing observed
# independent-CAN feedback phase tails during damping/large-motion tests.
RAW_CLOCK_SKEW_US="${RAW_CLOCK_SKEW_US:-10000}"
MAX_ITERATIONS="${MAX_ITERATIONS:-300}"
DISABLE_GRIPPER_MIRROR="${DISABLE_GRIPPER_MIRROR:-1}"
DRY_RUN="${DRY_RUN:-0}"
SKIP_BUILD="${SKIP_BUILD:-0}"

mkdir -p "${OUTPUT_DIR}"

build_cmd=(
    cargo build -p piper-cli
)

cmd=(
    target/debug/piper-cli teleop dual-arm
    --master-interface "${MASTER_IFACE}"
    --slave-interface "${SLAVE_IFACE}"
    --mode "${MODE}"
    --joint-map "${JOINT_MAP}"
    --experimental-calibrated-raw
    --raw-clock-inter-arm-skew-max-us "${RAW_CLOCK_SKEW_US}"
    --frequency-hz "${FREQUENCY_HZ}"
    --track-kp "${TRACK_KP}"
    --track-kd "${TRACK_KD}"
    --master-damping "${MASTER_DAMPING}"
    --raw-clock-warmup-secs "${RAW_CLOCK_WARMUP_SECS}"
    --raw-clock-residual-p95-us "${RAW_CLOCK_RESIDUAL_P95_US}"
    --raw-clock-residual-max-us "${RAW_CLOCK_RESIDUAL_MAX_US}"
    --raw-clock-residual-max-consecutive-failures "${RAW_CLOCK_RESIDUAL_MAX_CONSECUTIVE_FAILURES}"
    --raw-clock-sample-gap-max-ms "${RAW_CLOCK_SAMPLE_GAP_MAX_MS}"
    --raw-clock-alignment-lag-us "${RAW_CLOCK_ALIGNMENT_LAG_US}"
    --raw-clock-alignment-buffer-miss-consecutive-failures "${RAW_CLOCK_ALIGNMENT_BUFFER_MISS_CONSECUTIVE_FAILURES}"
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
    echo "joint_map=${JOINT_MAP}"
    echo "frequency_hz=${FREQUENCY_HZ}"
    echo "track_kp=${TRACK_KP}"
    echo "track_kd=${TRACK_KD}"
    echo "master_damping=${MASTER_DAMPING}"
    echo "raw_clock_warmup_secs=${RAW_CLOCK_WARMUP_SECS}"
    echo "raw_clock_residual_p95_us=${RAW_CLOCK_RESIDUAL_P95_US}"
    echo "raw_clock_residual_max_us=${RAW_CLOCK_RESIDUAL_MAX_US}"
    echo "raw_clock_residual_max_consecutive_failures=${RAW_CLOCK_RESIDUAL_MAX_CONSECUTIVE_FAILURES}"
    echo "raw_clock_sample_gap_max_ms=${RAW_CLOCK_SAMPLE_GAP_MAX_MS}"
    echo "raw_clock_alignment_lag_us=${RAW_CLOCK_ALIGNMENT_LAG_US}"
    echo "raw_clock_alignment_buffer_miss_consecutive_failures=${RAW_CLOCK_ALIGNMENT_BUFFER_MISS_CONSECUTIVE_FAILURES}"
    echo "raw_clock_skew_us=${RAW_CLOCK_SKEW_US}"
    echo "max_iterations=${MAX_ITERATIONS}"
    echo "disable_gripper_mirror=${DISABLE_GRIPPER_MIRROR}"
    echo "skip_build=${SKIP_BUILD}"
    echo "git_revision=$(git rev-parse HEAD 2>/dev/null || echo unknown)"
} > "${ENV_FILE}"

{
    echo "Build command:"
    printf "%q " "${build_cmd[@]}"
    printf "\n\nRun command:\n"
    printf "%q " "${cmd[@]}"
    printf "\n"
} > "${COMMAND_FILE}"

echo "Teleop smoke output: ${OUTPUT_DIR}"
echo "Calibration file: ${CALIBRATION_FILE}"
echo "Report JSON: ${REPORT_JSON}"
echo "Build log: ${BUILD_LOG}"
echo "Run log: ${RUN_LOG}"
echo
echo "Safety note: support both arms in the intended zero pose for joint map '${JOINT_MAP}' before typing yes."
echo "Master-follower input is read from ${MASTER_IFACE}; move that physical arm."
echo "If that is not the physical master arm, swap MASTER_IFACE/SLAVE_IFACE and rerun."
echo "This script intentionally does not pass --yes; the CLI will require operator confirmation."
echo "After yes, the CLI refreshes raw-clock timing before enabling the arms."
echo
echo "Commands:"
cat "${COMMAND_FILE}"
echo

if [[ "${DRY_RUN}" == "1" ]]; then
    echo "Environment:"
    cat "${ENV_FILE}"
    echo
    echo "DRY_RUN=1 set; command was not executed."
    exit 0
fi

if [[ "${SKIP_BUILD}" != "1" ]]; then
    "${build_cmd[@]}" 2>&1 | tee "${BUILD_LOG}"
fi

run_pid=""
interrupt_count=0

handle_interrupt() {
    interrupt_count=$((interrupt_count + 1))
    if [[ -n "${run_pid}" ]] && kill -0 "${run_pid}" 2>/dev/null; then
        if [[ "${interrupt_count}" -eq 1 ]]; then
            echo
            echo "Ctrl-C received; requesting teleop shutdown. Wait for the CLI report..."
            kill -INT "${run_pid}" 2>/dev/null || true
        else
            echo
            echo "Second interrupt received; forcing teleop process to terminate."
            kill -TERM "${run_pid}" 2>/dev/null || true
        fi
    fi
}

set +e
trap handle_interrupt INT TERM
if [[ -r /dev/tty ]]; then
    "${cmd[@]}" < /dev/tty > >(tee "${RUN_LOG}") 2>&1 &
else
    "${cmd[@]}" > >(tee "${RUN_LOG}") 2>&1 &
fi
run_pid=$!
while true; do
    wait "${run_pid}"
    status=$?
    if kill -0 "${run_pid}" 2>/dev/null; then
        continue
    fi
    break
done
trap - INT TERM
run_pid=""
set -e

if [[ -f "${REPORT_JSON}" && -x "$(command -v jq 2>/dev/null)" ]]; then
    echo
    echo "Report summary:"
    jq '{
        faulted,
        timing,
        control,
        calibration,
        j4_motion_delta_rad: (
            if .metrics.joint_motion then {
                master_feedback: .metrics.joint_motion.master_feedback_delta_rad[3],
                slave_command: .metrics.joint_motion.slave_command_delta_rad[3],
                slave_feedback: .metrics.joint_motion.slave_feedback_delta_rad[3]
            } else null end
        ),
        joint_motion: .metrics.joint_motion
    }' "${REPORT_JSON}"
fi

exit "${status}"
