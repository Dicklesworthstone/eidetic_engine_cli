#!/usr/bin/env bash
# bridge-staleness.sh — advisory gate that detects when
# CLOSE_THE_GAP_PLAN.md has drifted into "Part III needed" territory
# (bd-3usjw.33 / CLOSE_THE_GAP §36).
#
# Three signals are evaluated against the live tree and the live
# .vision-coverage-report.json:
#
#   1. plan_mtime_age_days — mtime of CLOSE_THE_GAP_PLAN.md older
#      than 30 days. Severity: medium. Trigger phrase: "bridge plan
#      mtime exceeds 30 day staleness budget".
#
#   2. vision_coverage_gap_low — .vision-coverage-report.json
#      gap_percentage < 2%. Severity: low. Trigger phrase: "bridge
#      substantially closed; consider planning Part III".
#
#   3. in_progress_beads_mtime — beads tagged
#      reality-check-2026-05-14 OR labels containing
#      'reality-check-2026-05-14' OR wave-4 with status=in_progress
#      where last_updated mtime > 7 days. Severity: medium. Trigger
#      phrase: "Part II swarm not eating the bridge".
#
# Output: .bridge-staleness-report.json with schema
# 'ee.bridge.staleness.v1', signals[], generated_at,
# data_hash.
#
# Exit code: always 0 (advisory only). Non-blocking by design.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PLAN_PATH="${ROOT}/CLOSE_THE_GAP_PLAN.md"
VISION_REPORT="${ROOT}/.vision-coverage-report.json"
BEADS_JSONL="${ROOT}/.beads/issues.jsonl"
OUTPUT_PATH="${ROOT}/.bridge-staleness-report.json"

JSON_FLAG=""
QUIET_FLAG=""
for arg in "$@"; do
  case "$arg" in
    --json) JSON_FLAG="1" ;;
    --quiet) QUIET_FLAG="1" ;;
    --help)
      cat <<'USAGE'
Usage: scripts/bridge-staleness.sh [--json] [--quiet]

  --json   Emit only the JSON report to stdout; diagnostics on stderr.
  --quiet  Suppress human-readable summary (still writes JSON to disk).

Reads:
  CLOSE_THE_GAP_PLAN.md            (plan mtime check)
  .vision-coverage-report.json     (gap-percentage check)
  .beads/issues.jsonl              (Part II in-progress mtime check)

Writes:
  .bridge-staleness-report.json    (always, regardless of --json)

Exit code: always 0 (advisory gate).
USAGE
      exit 0 ;;
  esac
done

now_epoch=$(date +%s)
generated_at=$(date -u +"%Y-%m-%dT%H:%M:%SZ")

signals_json=""

# Signal 1: plan mtime age.
plan_present=false
plan_age_days=0
if [ -f "$PLAN_PATH" ]; then
  plan_present=true
  mtime_epoch=""
  if mtime_candidate=$(stat -f %m "$PLAN_PATH" 2>/dev/null) && [[ "$mtime_candidate" =~ ^[0-9]+$ ]]; then
    mtime_epoch="$mtime_candidate"
  elif mtime_candidate=$(stat -c %Y "$PLAN_PATH" 2>/dev/null) && [[ "$mtime_candidate" =~ ^[0-9]+$ ]]; then
    mtime_epoch="$mtime_candidate"
  else
    mtime_epoch="$now_epoch"
  fi
  plan_age_days=$(( (now_epoch - mtime_epoch) / 86400 ))
fi

if [ "$plan_present" = true ] && [ "$plan_age_days" -gt 30 ]; then
  signal_one=$(jq -n \
    --arg code "plan_mtime_age_days" \
    --arg severity "medium" \
    --arg message "bridge plan mtime exceeds 30 day staleness budget" \
    --arg repair "Open the next bridge plan part (Part III) or refresh CLOSE_THE_GAP_PLAN.md with a status block." \
    --argjson plan_age_days "$plan_age_days" \
    --arg plan_path "CLOSE_THE_GAP_PLAN.md" \
    '{code: $code, severity: $severity, message: $message, repair: $repair, details: {planAgeDays: $plan_age_days, planPath: $plan_path}}')
  signals_json="${signals_json}${signal_one},"
fi

# Signal 2: vision-coverage gap percentage.
vision_present=false
gap_percentage=null
if [ -f "$VISION_REPORT" ]; then
  vision_present=true
  gap_percentage=$(jq -r '.gap_percentage // empty' "$VISION_REPORT" 2>/dev/null || true)
fi

if [ "$vision_present" = true ] && [ -n "$gap_percentage" ] && [ "$gap_percentage" != "null" ]; then
  # Compare numerically. awk avoids the bash arithmetic float limitation.
  if awk "BEGIN { exit !($gap_percentage < 2.0) }"; then
    signal_two=$(jq -n \
      --arg code "vision_coverage_gap_low" \
      --arg severity "low" \
      --arg message "bridge substantially closed; consider planning Part III" \
      --arg repair "Author CLOSE_THE_GAP_PLAN Part III with the next 90-day vision targets." \
      --argjson gap "$gap_percentage" \
      '{code: $code, severity: $severity, message: $message, repair: $repair, details: {gapPercentage: $gap}}')
    signals_json="${signals_json}${signal_two},"
  fi
fi

# Signal 3: open Part II beads with no in_progress activity for > 7 days.
part_ii_open_count=0
part_ii_in_progress_count=0
part_ii_max_stale_days=0
if [ -f "$BEADS_JSONL" ]; then
  # Filter beads to those tagged reality-check-2026-05-14 OR wave-4.
  part_ii_stats=$(jq -s '
    [.[]
      | select(.labels // [] | any(test("reality-check-2026-05-14|wave-4")))
      | select(.status == "open" or .status == "in_progress")]
    | {
        open_count: ([.[] | select(.status == "open")] | length),
        in_progress_count: ([.[] | select(.status == "in_progress")] | length),
        max_stale_days: (
          [.[]
            | select(.status == "open")
            | (.updated_at // .created_at // "")
            | select(length > 0)
            | sub("\\.[0-9]+Z$"; "Z")
            | sub("Z$"; "+0000")
            | strptime("%Y-%m-%dT%H:%M:%S%z")
            | mktime]
          | if length == 0 then 0
            else (max | ((now - .) / 86400) | floor)
            end
        )
      }
  ' "$BEADS_JSONL" 2>/dev/null || echo '{"open_count":0,"in_progress_count":0,"max_stale_days":0}')
  part_ii_open_count=$(echo "$part_ii_stats" | jq -r '.open_count // 0')
  part_ii_in_progress_count=$(echo "$part_ii_stats" | jq -r '.in_progress_count // 0')
  part_ii_max_stale_days=$(echo "$part_ii_stats" | jq -r '.max_stale_days // 0')
fi

if [ "$part_ii_open_count" -gt 0 ] && [ "$part_ii_in_progress_count" -eq 0 ] && [ "$part_ii_max_stale_days" -gt 7 ]; then
  signal_three=$(jq -n \
    --arg code "in_progress_beads_mtime" \
    --arg severity "medium" \
    --arg message "Part II swarm not eating the bridge" \
    --arg repair "Triage at least one reality-check-2026-05-14 bead per day or close the bridge plan." \
    --argjson open_count "$part_ii_open_count" \
    --argjson in_progress_count "$part_ii_in_progress_count" \
    --argjson max_stale_days "$part_ii_max_stale_days" \
    '{code: $code, severity: $severity, message: $message, repair: $repair, details: {partIIOpenCount: $open_count, partIIInProgressCount: $in_progress_count, partIIMaxStaleDays: $max_stale_days}}')
  signals_json="${signals_json}${signal_three},"
fi

# Trim trailing comma and wrap in a JSON array.
signals_array="[${signals_json%,}]"

# Compute deterministic data hash of input state for the report.
data_hash_input=$(printf 'plan=%s|gap=%s|open=%s|inprog=%s|stale=%s' \
  "$plan_age_days" "$gap_percentage" \
  "$part_ii_open_count" "$part_ii_in_progress_count" "$part_ii_max_stale_days")
if command -v shasum >/dev/null 2>&1; then
  data_hash=$(printf '%s' "$data_hash_input" | shasum -a 256 | awk '{print $1}')
elif command -v sha256sum >/dev/null 2>&1; then
  data_hash=$(printf '%s' "$data_hash_input" | sha256sum | awk '{print $1}')
else
  data_hash="unavailable"
fi

report=$(jq -n \
  --arg schema "ee.bridge.staleness.v1" \
  --arg generated_at "$generated_at" \
  --arg data_hash "$data_hash" \
  --argjson signals "$signals_array" \
  --argjson plan_present "$( [ "$plan_present" = true ] && echo true || echo false )" \
  --argjson plan_age_days "$plan_age_days" \
  --argjson vision_present "$( [ "$vision_present" = true ] && echo true || echo false )" \
  --argjson part_ii_open_count "$part_ii_open_count" \
  --argjson part_ii_in_progress_count "$part_ii_in_progress_count" \
  --argjson part_ii_max_stale_days "$part_ii_max_stale_days" \
  '{
    schema: $schema,
    generatedAt: $generated_at,
    dataHash: $data_hash,
    inputs: {
      planPresent: $plan_present,
      planAgeDays: $plan_age_days,
      visionCoverageReportPresent: $vision_present,
      partIIOpenCount: $part_ii_open_count,
      partIIInProgressCount: $part_ii_in_progress_count,
      partIIMaxStaleDays: $part_ii_max_stale_days
    },
    signals: $signals
  }')

printf '%s\n' "$report" > "$OUTPUT_PATH"

if [ -n "$JSON_FLAG" ]; then
  printf '%s\n' "$report"
  exit 0
fi

if [ -z "$QUIET_FLAG" ]; then
  signal_count=$(printf '%s' "$signals_array" | jq 'length')
  echo "Bridge staleness report → $OUTPUT_PATH" >&2
  echo "  signals: $signal_count" >&2
  echo "  plan_mtime_age_days: $plan_age_days" >&2
  echo "  vision_coverage_gap_percentage: ${gap_percentage:-unknown}" >&2
  echo "  part_ii_open_count: $part_ii_open_count (in_progress=$part_ii_in_progress_count, max_stale_days=$part_ii_max_stale_days)" >&2
fi

exit 0
