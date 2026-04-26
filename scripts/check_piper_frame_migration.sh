#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

shopt -s nullglob
TARGETS=()
for path in crates apps addons tests docs README*.md QUICKSTART*.md; do
  [[ -e "$path" ]] && TARGETS+=("$path")
done
EXCLUDES=(
  -g '!docs/superpowers/specs/2026-04-26-piper-frame-type-safety-design.md'
  -g '!docs/superpowers/plans/*.md'
)
FAILURES=()
TMPFILES=()

cleanup() {
  rm -f "${TMPFILES[@]}"
}
trap cleanup EXIT

run_check() {
  local name="$1"
  local pattern="$2"
  local exclude_pattern="${3:-}"
  local matches
  local filtered
  local rg_status
  local grep_status

  matches="$(mktemp)"
  filtered="$(mktemp)"
  TMPFILES+=("$matches" "$filtered")

  echo "== $name =="
  if rg -n "$pattern" "${TARGETS[@]}" -g '*.rs' -g '*.md' "${EXCLUDES[@]}" >"$matches"; then
    rg_status=0
  else
    rg_status=$?
  fi

  if ((rg_status > 1)); then
    cat "$matches" >&2
    echo "ERROR: $name search failed" >&2
    exit "$rg_status"
  fi

  if ((rg_status == 1)); then
    return
  fi

  if [[ -n "$exclude_pattern" ]]; then
    if grep -Ev "$exclude_pattern" "$matches" >"$filtered"; then
      grep_status=0
    else
      grep_status=$?
    fi

    if ((grep_status > 1)); then
      echo "ERROR: $name exclude filter failed" >&2
      exit "$grep_status"
    fi
  else
    cp "$matches" "$filtered"
  fi

  if [[ -s "$filtered" ]]; then
    cat "$filtered"
    echo "FAILED: $name matched forbidden migration pattern" >&2
    FAILURES+=("$name")
  fi
}

run_candidate_search() {
  local name="$1"
  local pattern="$2"
  local rg_status

  echo "== $name =="
  if rg -n "$pattern" "${TARGETS[@]}" -g '*.rs' -g '*.md' "${EXCLUDES[@]}"; then
    rg_status=0
  else
    rg_status=$?
  fi

  if ((rg_status > 1)); then
    echo "ERROR: $name search failed" >&2
    exit "$rg_status"
  fi
}

run_check \
  'PiperFrame struct literals' \
  'PiperFrame\s*\{' \
  '(pub[[:space:]]+)?struct[[:space:]]+PiperFrame[[:space:]]*\{|impl([[:space:]]*<[^>]+>)?[[:space:]]+PiperFrame[[:space:]]*\{|impl.*for[[:space:]]+PiperFrame[[:space:]]*\{|->[[:space:]]*PiperFrame[[:space:]]*\{'
run_check 'legacy recording readers' 'LegacyPiperRecording'
run_check 'extended-format inference from raw id' 'can_id\s*>\s*0x7[Ff]{2}'
run_check 'replay construction from ambiguous can_id' 'new_standard\([^\n]*can_id'

run_candidate_search 'direct field access candidates' '\.(timestamp_us|is_extended|len|data|id)\b'
echo "Review any direct field access candidates above. The script does not fail this broad search automatically because unrelated structs can have the same field names."

run_candidate_search 'legacy conversion candidates' 'data\.to_vec\(\)|TimestampedFrame\s*\{'
echo "Review candidates above and verify they are not old can_id + Vec<u8> frame conversions."

run_candidate_search 'raw classifier candidates' 'FrameType::from_id|is_robot_feedback_id|DRIVER_RX_ROBOT_FEEDBACK_IDS|raw_id\(\)\s*=='
echo "Review candidates above and verify they use typed CanId / StandardCanId, not raw u32 classification."

run_candidate_search 'ambiguous raw CAN ID collection candidates' 'StopCondition::OnCanId\([^C]|HashMap<\s*u32|BTreeMap<\s*u32|frequency\([^)]*can_id|add_frame\([^)]*can_id'
echo "Review candidates above and verify they are format-aware or unrelated false positives."

if ((${#FAILURES[@]})); then
  echo "== hard-fail summary =="
  printf 'FAILED: %s\n' "${FAILURES[@]}"
  exit 1
fi

echo "PiperFrame migration guardrails passed."
