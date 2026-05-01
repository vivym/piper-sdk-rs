#!/bin/bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

cd "${REPO_ROOT}"

MODEL="${MODEL:-}"
SAMPLES_GLOB="${SAMPLES_GLOB:-}"
SKIP_BUILD="${SKIP_BUILD:-0}"
PRINT_ONLY="${PRINT_ONLY:-0}"

if [[ -z "${MODEL}" ]]; then
    echo "Usage: MODEL=model.toml $0 sample-001.jsonl [sample-002.jsonl ...]" >&2
    exit 2
fi

samples=("$@")
if [[ "${#samples[@]}" -eq 0 && -n "${SAMPLES_GLOB}" ]]; then
    while IFS= read -r sample; do
        samples+=("${sample}")
    done < <(compgen -G "${SAMPLES_GLOB}" | sort || true)
fi

if [[ "${#samples[@]}" -eq 0 ]]; then
    echo "Pass sample files as arguments or set SAMPLES_GLOB." >&2
    exit 2
fi

STEM="${MODEL%.toml}"
BUILD_LOG="${BUILD_LOG:-${STEM}.eval.build.log}"
RUN_LOG="${RUN_LOG:-${STEM}.eval.log}"
COMMAND_FILE="${COMMAND_FILE:-${STEM}.eval.command.txt}"
ENV_FILE="${ENV_FILE:-${STEM}.eval.environment.txt}"

build_cmd=(cargo build -p piper-cli)
cmd=(
    target/debug/piper-cli gravity eval
    --model "${MODEL}"
)
for sample in "${samples[@]}"; do
    cmd+=(--samples "${sample}")
done

{
    echo "model=${MODEL}"
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

echo "Gravity model evaluation"
echo "Model: ${MODEL}"
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

if [[ ! -f "${MODEL}" ]]; then
    echo "Model file does not exist: ${MODEL}" >&2
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
