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
# Active bridge plan lives at CLOSE_THE_GAP_PLAN.md. When archived
# (e.g. docs/archive/close_the_gap_<YYYY-MM>.md), the active slot is
# missing — bridge-staleness then degrades signal 1 cleanly and the
# vision-coverage / Part II signals continue to advise authorship of
# the next bridge part.
PLAN_PATH="${ROOT}/CLOSE_THE_GAP_PLAN.md"
VISION_REPORT="${ROOT}/.vision-coverage-report.json"
BEADS_JSONL="${ROOT}/.beads/issues.jsonl"
OUTPUT_PATH="${ROOT}/.bridge-staleness-report.json"

JSON_FLAG=""
QUIET_FLAG=""
SELF_TEST=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --json)
      JSON_FLAG="1"
      shift
      ;;
    --quiet)
      QUIET_FLAG="1"
      shift
      ;;
    --self-test)
      SELF_TEST="1"
      shift
      ;;
    --plan)
      PLAN_PATH="${2:-}"
      shift 2
      ;;
    --vision)
      VISION_REPORT="${2:-}"
      shift 2
      ;;
    --beads)
      BEADS_JSONL="${2:-}"
      shift 2
      ;;
    --output)
      OUTPUT_PATH="${2:-}"
      shift 2
      ;;
    --help)
      cat <<'USAGE'
Usage: scripts/bridge-staleness.sh [--json] [--quiet] [--self-test] [--plan <path>] [--vision <path>] [--beads <path>] [--output <path>]

  --json   Emit only the JSON report to stdout; diagnostics on stderr.
  --quiet  Suppress human-readable summary (still writes JSON to disk).
  --self-test Run synthetic bridge-staleness fixture checks without reading the workspace.
  --plan <path>   Read bridge plan mtime from this path.
  --vision <path> Read vision coverage JSON from this path.
  --beads <path>  Read bead records from this JSONL path.
  --output <path> Write the JSON report to this path.

Reads:
  CLOSE_THE_GAP_PLAN.md            (plan mtime check)
  .vision-coverage-report.json     (gap-percentage check)
  .beads/issues.jsonl              (Part II in-progress mtime check)

Writes:
  .bridge-staleness-report.json    (always, regardless of --json)

Exit code: always 0 (advisory gate).
USAGE
      exit 0
      ;;
    *)
      echo "unknown argument: $1" >&2
      exit 1
      ;;
  esac
done

now_epoch=$(date +%s)
generated_at=$(date -u +"%Y-%m-%dT%H:%M:%SZ")

assert_report_jq() {
  local report="$1"
  local filter="$2"
  local message="$3"

  if ! printf '%s\n' "$report" | jq -e "$filter" >/dev/null; then
    echo "error: bridge-staleness self-test failed: $message" >&2
    echo "       jq filter: $filter" >&2
    return 1
  fi
}

run_self_test() {
  local report
  report=$(bash "${BASH_SOURCE[0]}" --json --quiet --output /dev/null \
    --plan <(cat <<'PLAN'
# Synthetic Active Bridge

Fresh plan content is enough for the self-test because process substitutions
carry a current mtime and should not trigger plan_mtime_age_days.
PLAN
    ) \
    --vision <(printf '%s\n' '{"gap_percentage":1.5}') \
    --beads <(cat <<'JSONL'
{"id":"bd-bridge.self-open","status":"open","labels":["wave-4"],"created_at":"2000-01-01T00:00:00Z","updated_at":"2000-01-01T00:00:00Z"}
{"id":"bd-bridge.self-ignored","status":"closed","labels":["wave-4"],"created_at":"2000-01-01T00:00:00Z","updated_at":"2000-01-01T00:00:00Z"}
JSONL
    ))

  assert_report_jq "$report" '.schema == "ee.bridge.staleness.v1"' "schema mismatch"
  assert_report_jq "$report" '.inputs.planPresent == true' "synthetic plan should be present"
  assert_report_jq "$report" '.inputs.visionCoverageReportPresent == true' "synthetic vision report should be present"
  assert_report_jq "$report" '.inputs.partIIOpenCount == 1' "expected one stale open Part II bead"
  assert_report_jq "$report" '.inputs.partIIInProgressCount == 0' "expected no in-progress Part II beads"
  assert_report_jq "$report" '.inputs.partIIMaxStaleDays > 7' "expected stale Part II age above threshold"
  assert_report_jq "$report" '(.signals | map(.code) | index("vision_coverage_gap_low")) != null' "missing low vision-gap signal"
  assert_report_jq "$report" '(.signals | map(.code) | index("in_progress_beads_mtime")) != null' "missing Part II inactivity signal"
  assert_report_jq "$report" '(.signals | map(.severity) | index("low")) != null' "missing low-severity signal"
  assert_report_jq "$report" '(.signals | map(.severity) | index("medium")) != null' "missing medium-severity signal"

  local quiet_report
  quiet_report=$(bash "${BASH_SOURCE[0]}" --json --quiet --output /dev/null \
    --plan <(printf '%s\n' '# Synthetic Active Bridge') \
    --vision <(printf '%s\n' '{"gap_percentage":5.0}') \
    --beads <(cat <<'JSONL'
{"id":"bd-bridge.self-active","status":"in_progress","labels":["reality-check-2026-05-14"],"created_at":"2000-01-01T00:00:00Z","updated_at":"2000-01-01T00:00:00Z"}
JSONL
    ))
  assert_report_jq "$quiet_report" '.signals | length == 0' "active Part II or high vision gap should not emit advisory signals"

  echo "ok: bridge-staleness self-test passed"
}

if [ -n "$SELF_TEST" ]; then
  run_self_test
  exit 0
fi

signals_json=""

# Signal 1: plan mtime age.
plan_present=false
plan_age_days=0
if [ -r "$PLAN_PATH" ]; then
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
if [ -r "$VISION_REPORT" ]; then
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
if [ -r "$BEADS_JSONL" ]; then
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
