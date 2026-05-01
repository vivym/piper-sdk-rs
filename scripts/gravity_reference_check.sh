#!/bin/bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

cd "${REPO_ROOT}"

MODEL="${MODEL:-}"
SAMPLES_GLOB="${SAMPLES_GLOB:-}"
COEFFICIENT_ATOL="${COEFFICIENT_ATOL:-1e-7}"
RESIDUAL_ATOL="${RESIDUAL_ATOL:-1e-7}"
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

OUT="${OUT:-${MODEL%.toml}.reference-check.json}"
mkdir -p "$(dirname "${OUT}")"

STEM="${OUT%.json}"
RUN_LOG="${RUN_LOG:-${STEM}.log}"
COMMAND_FILE="${COMMAND_FILE:-${STEM}.command.txt}"
ENV_FILE="${ENV_FILE:-${STEM}.environment.txt}"

cmd=(
    uv run --project tools/gravity-reference
    python tools/gravity-reference/gravity_fit_reference.py
    --rust-model "${MODEL}"
    --out "${OUT}"
    --coefficient-atol "${COEFFICIENT_ATOL}"
    --residual-atol "${RESIDUAL_ATOL}"
)
for sample in "${samples[@]}"; do
    cmd+=(--samples "${sample}")
done

{
    echo "model=${MODEL}"
    echo "out=${OUT}"
    echo "coefficient_atol=${COEFFICIENT_ATOL}"
    echo "residual_atol=${RESIDUAL_ATOL}"
    echo "sample_count=${#samples[@]}"
    printf "samples="
    printf "%q " "${samples[@]}"
    printf "\n"
    echo "print_only=${PRINT_ONLY}"
    echo "git_revision=$(git rev-parse HEAD 2>/dev/null || echo unknown)"
} > "${ENV_FILE}"

{
    echo "Run command:"
    printf "%q " "${cmd[@]}"
    printf "\n"
} > "${COMMAND_FILE}"

echo "Gravity Python reference check"
echo "Model: ${MODEL}"
echo "Report: ${OUT}"
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

"${cmd[@]}" 2>&1 | tee "${RUN_LOG}"
