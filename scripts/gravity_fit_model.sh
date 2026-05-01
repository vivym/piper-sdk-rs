#!/bin/bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

cd "${REPO_ROOT}"

ROLE="${ROLE:-slave}"
LOAD_PROFILE="${LOAD_PROFILE:-normal-gripper-d405}"
OUTPUT_DIR="${GRAVITY_OUT_DIR:-artifacts/gravity/${ROLE}}"
MODEL="${MODEL:-${OUT:-${OUTPUT_DIR}/${ROLE}-${LOAD_PROFILE}.model.toml}}"
BASIS="${BASIS:-trig-v1}"
RIDGE_LAMBDA="${RIDGE_LAMBDA:-0.0001}"
HOLDOUT_RATIO="${HOLDOUT_RATIO:-0.2}"
SAMPLES_GLOB="${SAMPLES_GLOB:-}"
SKIP_BUILD="${SKIP_BUILD:-0}"
PRINT_ONLY="${PRINT_ONLY:-0}"

samples=("$@")
if [[ "${#samples[@]}" -eq 0 && -n "${SAMPLES_GLOB}" ]]; then
    while IFS= read -r sample; do
        samples+=("${sample}")
    done < <(compgen -G "${SAMPLES_GLOB}" | sort || true)
fi

if [[ "${#samples[@]}" -eq 0 ]]; then
    echo "Usage: MODEL=out.model.toml $0 sample-001.jsonl [sample-002.jsonl ...]" >&2
    echo "Or set SAMPLES_GLOB='artifacts/gravity/slave/*.samples.jsonl'." >&2
    exit 2
fi

mkdir -p "$(dirname "${MODEL}")"

STEM="${MODEL%.toml}"
BUILD_LOG="${BUILD_LOG:-${STEM}.fit.build.log}"
RUN_LOG="${RUN_LOG:-${STEM}.fit.log}"
COMMAND_FILE="${COMMAND_FILE:-${STEM}.fit.command.txt}"
ENV_FILE="${ENV_FILE:-${STEM}.fit.environment.txt}"

build_cmd=(cargo build -p piper-cli)
cmd=(
    target/debug/piper-cli gravity fit
    --out "${MODEL}"
    --basis "${BASIS}"
    --ridge-lambda "${RIDGE_LAMBDA}"
    --holdout-ratio "${HOLDOUT_RATIO}"
)
for sample in "${samples[@]}"; do
    cmd+=(--samples "${sample}")
done

{
    echo "role=${ROLE}"
    echo "load_profile=${LOAD_PROFILE}"
    echo "model=${MODEL}"
    echo "basis=${BASIS}"
    echo "ridge_lambda=${RIDGE_LAMBDA}"
    echo "holdout_ratio=${HOLDOUT_RATIO}"
    echo "sample_count=${#samples[@]}"
    printf "samples="
    printf "%q " "${samples[@]}"
    printf "\n"
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

echo "Gravity model fitting"
echo "Model output: ${MODEL}"
echo "Samples:"
printf "  %s\n" "${samples[@]}"
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

if [[ -e "${MODEL}" ]]; then
    echo "Refusing to overwrite existing model: ${MODEL}" >&2
    exit 1
fi
for sample in "${samples[@]}"; do
    if [[ ! -f "${sample}" ]]; then
        echo "Sample file does not exist: ${sample}" >&2
        exit 1
    fi
done

if [[ "${SKIP_BUILD}" != "1" ]]; then
    "${build_cmd[@]}" 2>&1 | tee "${BUILD_LOG}"
fi

"${cmd[@]}" 2>&1 | tee "${RUN_LOG}"
