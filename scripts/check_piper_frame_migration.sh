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

run_check() {
  local name="$1"
  local pattern="$2"
  echo "== $name =="
  if rg -n "$pattern" "${TARGETS[@]}" -g '*.rs' -g '*.md' "${EXCLUDES[@]}"; then
    echo "FAILED: $name matched forbidden migration pattern" >&2
    return 1
  fi
}

run_check 'PiperFrame struct literals' 'PiperFrame\s*\{'
run_check 'legacy recording readers' 'LegacyPiperRecording'
run_check 'extended-format inference from raw id' 'can_id\s*>\s*0x7[Ff]{2}'
run_check 'replay construction from ambiguous can_id' 'new_standard\([^\n]*can_id'

echo "== direct field access candidates =="
rg -n '\.(timestamp_us|is_extended|len|data|id)\b' "${TARGETS[@]}" -g '*.rs' -g '*.md' "${EXCLUDES[@]}" || true
echo "Review any direct field access candidates above. The script does not fail this broad search automatically because unrelated structs can have the same field names."

echo "== legacy conversion candidates =="
rg -n 'data\.to_vec\(\)|TimestampedFrame\s*\{' "${TARGETS[@]}" -g '*.rs' -g '*.md' "${EXCLUDES[@]}" || true
echo "Review candidates above and verify they are not old can_id + Vec<u8> frame conversions."

echo "== raw classifier candidates =="
rg -n 'FrameType::from_id|is_robot_feedback_id|DRIVER_RX_ROBOT_FEEDBACK_IDS|raw_id\(\)\s*==' "${TARGETS[@]}" -g '*.rs' -g '*.md' "${EXCLUDES[@]}" || true
echo "Review candidates above and verify they use typed CanId / StandardCanId, not raw u32 classification."

echo "== ambiguous raw CAN ID collection candidates =="
rg -n 'StopCondition::OnCanId\([^C]|HashMap<\s*u32|BTreeMap<\s*u32|frequency\([^)]*can_id|add_frame\([^)]*can_id' "${TARGETS[@]}" -g '*.rs' -g '*.md' "${EXCLUDES[@]}" || true
echo "Review candidates above and verify they are format-aware or unrelated false positives."
