#!/usr/bin/env bash
# plan-drift.sh - advisory gate for CLOSE_THE_GAP plan/bead drift
# (bd-3usjw.43 / CLOSE_THE_GAP §47).
#
# The gate scans implements-surface beads in the bd-3usjw tree, reads their
# plan_doc_section:<section> label, extracts the matching section from
# CLOSE_THE_GAP_PLAN.md, and emits plan_drift_warning hints when the plan file
# changed after the bead was created and the bead description has weak overlap
# with the current section text.
#
# Output: .plan-drift-report.json with schema 'ee.plan_drift.v1'.
# Exit code: always 0 (advisory only). Non-blocking by design.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PLAN_PATH="${ROOT}/CLOSE_THE_GAP_PLAN.md"
BEADS_JSONL="${ROOT}/.beads/issues.jsonl"
OUTPUT_PATH="${ROOT}/.plan-drift-report.json"

JSON_FLAG=""
QUIET_FLAG=""
BEAD_FILTER=""

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
    --bead)
      BEAD_FILTER="${2:-}"
      shift 2
      ;;
    --plan)
      PLAN_PATH="${2:-}"
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
Usage: scripts/plan-drift.sh [--json] [--quiet] [--bead <id>] [--plan <path>] [--beads <path>] [--output <path>]

  --json          Emit only the JSON report to stdout; diagnostics on stderr.
  --quiet         Suppress human-readable summary (still writes JSON to disk).
  --bead <id>     Restrict the scan to one bead ID.
  --plan <path>   Read plan sections from this markdown file.
  --beads <path>  Read bead records from this JSONL file.
  --output <path> Write the JSON report to this path.

Reads:
  CLOSE_THE_GAP_PLAN.md by default
  .beads/issues.jsonl by default

Writes:
  .plan-drift-report.json by default

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

generated_at=$(date -u +"%Y-%m-%dT%H:%M:%SZ")
now_epoch=$(date +%s)

mtime_epoch() {
  local path="$1"
  local candidate=""
  if candidate=$(stat -f %m "$path" 2>/dev/null) && [[ "$candidate" =~ ^[0-9]+$ ]]; then
    printf '%s' "$candidate"
  elif candidate=$(stat -c %Y "$path" 2>/dev/null) && [[ "$candidate" =~ ^[0-9]+$ ]]; then
    printf '%s' "$candidate"
  else
    printf '%s' "$now_epoch"
  fi
}

rfc3339_epoch() {
  local timestamp="$1"
  if [[ "$timestamp" == *.* ]]; then
    timestamp="${timestamp%%.*}Z"
  fi
  if parsed=$(date -u -j -f "%Y-%m-%dT%H:%M:%SZ" "$timestamp" +%s 2>/dev/null); then
    printf '%s' "$parsed"
  elif parsed=$(date -u -d "$timestamp" +%s 2>/dev/null); then
    printf '%s' "$parsed"
  else
    printf '0'
  fi
}

canonical_section() {
  local raw="$1"
  raw="${raw#plan_doc_section:}"
  raw="${raw#§}"
  raw="${raw#part_i_}"
  raw="${raw#part_ii_}"
  raw="${raw//_/.}"
  printf '%s' "$raw"
}

extract_section() {
  local section="$1"
  local plan_path="$2"
  awk -v target="$section" '
    function heading_number(line, parts) {
      if (match(line, /^#{1,6}[ \t]+[0-9]+(\.[0-9]+)*/)) {
        return substr(line, RSTART, RLENGTH)
      }
      return ""
    }
    function clean_number(raw) {
      sub(/^#{1,6}[ \t]+/, "", raw)
      return raw
    }
    function in_target(num) {
      return num == target || index(num, target ".") == 1
    }
    {
      raw = heading_number($0)
      if (raw != "") {
        num = clean_number(raw)
        if (collecting && !in_target(num)) {
          exit
        }
        if (!collecting && in_target(num)) {
          collecting = 1
        }
      }
      if (collecting) {
        print
      }
    }
  ' "$plan_path"
}

sha256_text() {
  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 | awk '{print $1}'
  elif command -v sha256sum >/dev/null 2>&1; then
    sha256sum | awk '{print $1}'
  else
    awk '{ total = total length($0) } END { printf "len:%s", total }'
  fi
}

word_set_json() {
  jq -Rn --arg text "$1" '
    $text
    | ascii_downcase
    | gsub("[^a-z0-9]+"; " ")
    | split(" ")
    | map(select(length >= 4))
    | unique
  '
}

overlap_ratio() {
  local section_text="$1"
  local description="$2"
  local section_words
  local description_words
  section_words=$(word_set_json "$section_text")
  description_words=$(word_set_json "$description")
  jq -n --argjson section "$section_words" --argjson description "$description_words" '
    if ($section | length) == 0 then 0
    else
      ([ $section[] as $word | select($description | index($word)) ] | length) / ($section | length)
    end
  '
}

warning_json="[]"
candidate_count=0
missing_metadata_count=0
missing_section_count=0
drift_warning_count=0
plan_present=false
beads_present=false
plan_epoch=0

if [ -f "$PLAN_PATH" ]; then
  plan_present=true
  plan_epoch=$(mtime_epoch "$PLAN_PATH")
fi
if [ -f "$BEADS_JSONL" ]; then
  beads_present=true
fi

if [ "$plan_present" = true ] && [ "$beads_present" = true ]; then
  candidates=$(jq -s -c --arg bead_filter "$BEAD_FILTER" '
    [
      .[]
      | select((.id // "") | startswith("bd-3usjw."))
      | select((.status // "") != "closed")
      | select((.labels // []) | any(startswith("implements-surface:")))
      | select($bead_filter == "" or .id == $bead_filter)
      | {
          id,
          title,
          status,
          createdAt: (.created_at // ""),
          updatedAt: (.updated_at // ""),
          planDocSection: (((.labels // []) | map(select(startswith("plan_doc_section:"))) | first) // null),
          description: (.description // "")
        }
    ]
  ' "$BEADS_JSONL")
  candidate_count=$(printf '%s' "$candidates" | jq 'length')

  while IFS= read -r candidate; do
    [ -n "$candidate" ] || continue
    bead_id=$(printf '%s' "$candidate" | jq -r '.id')
    title=$(printf '%s' "$candidate" | jq -r '.title // ""')
    status=$(printf '%s' "$candidate" | jq -r '.status // ""')
    created_at=$(printf '%s' "$candidate" | jq -r '.createdAt // ""')
    description=$(printf '%s' "$candidate" | jq -r '.description // ""')
    raw_section=$(printf '%s' "$candidate" | jq -r '.planDocSection // empty')

    if [ -z "$raw_section" ]; then
      missing_metadata_count=$((missing_metadata_count + 1))
      warning=$(jq -n \
        --arg bead_id "$bead_id" \
        --arg title "$title" \
        --arg status "$status" \
        '{
          code: "missing_plan_doc_section",
          severity: "medium",
          beadId: $bead_id,
          title: $title,
          status: $status,
          message: "implements-surface bead is missing a plan_doc_section label",
          repair: "Add a plan_doc_section:<section> label that points to the controlling plan section.",
          details: {}
        }')
      warning_json=$(jq -c --argjson warning "$warning" '. + [$warning]' <<<"$warning_json")
      continue
    fi

    section=$(canonical_section "$raw_section")
    section_text=$(extract_section "$section" "$PLAN_PATH")
    if [ -z "$section_text" ]; then
      missing_section_count=$((missing_section_count + 1))
      warning=$(jq -n \
        --arg bead_id "$bead_id" \
        --arg title "$title" \
        --arg status "$status" \
        --arg plan_doc_section "$raw_section" \
        --arg section "$section" \
        '{
          code: "plan_doc_section_missing",
          severity: "medium",
          beadId: $bead_id,
          title: $title,
          status: $status,
          message: "plan_doc_section label points at a section not found in CLOSE_THE_GAP_PLAN.md",
          repair: "Update the label to the current section number or restore the missing plan section.",
          details: {planDocSection: $plan_doc_section, canonicalSection: $section}
        }')
      warning_json=$(jq -c --argjson warning "$warning" '. + [$warning]' <<<"$warning_json")
      continue
    fi

    created_epoch=$(rfc3339_epoch "$created_at")
    section_hash=$(printf '%s' "$section_text" | sha256_text)
    description_hash=$(printf '%s' "$description" | sha256_text)
    overlap=$(overlap_ratio "$section_text" "$description")
    plan_changed_after_creation=false
    if [ "$created_epoch" -gt 0 ] && [ "$plan_epoch" -gt "$created_epoch" ]; then
      plan_changed_after_creation=true
    fi

    if [ "$plan_changed_after_creation" = true ] && awk "BEGIN { exit !($overlap < 0.35) }"; then
      drift_warning_count=$((drift_warning_count + 1))
      warning=$(jq -n \
        --arg bead_id "$bead_id" \
        --arg title "$title" \
        --arg status "$status" \
        --arg plan_doc_section "$raw_section" \
        --arg section "$section" \
        --arg section_hash "$section_hash" \
        --arg description_hash "$description_hash" \
        --argjson overlap "$overlap" \
        --argjson plan_epoch "$plan_epoch" \
        --argjson created_epoch "$created_epoch" \
        '{
          code: "plan_drift_warning",
          severity: "low",
          beadId: $bead_id,
          title: $title,
          status: $status,
          message: "plan section changed after bead creation and the bead description weakly overlaps current plan text",
          repair: "Re-read the plan section before claiming the bead; refresh the bead description if the section changed materially.",
          details: {
            planDocSection: $plan_doc_section,
            canonicalSection: $section,
            overlapRatio: $overlap,
            sectionHash: $section_hash,
            descriptionHash: $description_hash,
            planMtimeEpoch: $plan_epoch,
            beadCreatedEpoch: $created_epoch
          }
        }')
      warning_json=$(jq -c --argjson warning "$warning" '. + [$warning]' <<<"$warning_json")
    fi
  done < <(printf '%s' "$candidates" | jq -c '.[]')
fi

hint_json=$(jq -c '
  map(select(.code == "plan_drift_warning")
    | {
        beadId,
        warning: .code,
        message,
        planDocSection: .details.planDocSection,
        overlapRatio: .details.overlapRatio
      })
' <<<"$warning_json")

data_hash_input=$(jq -n \
  --argjson candidate_count "$candidate_count" \
  --argjson missing_metadata_count "$missing_metadata_count" \
  --argjson missing_section_count "$missing_section_count" \
  --argjson drift_warning_count "$drift_warning_count" \
  --argjson warnings "$warning_json" \
  '{candidateCount: $candidate_count, missingMetadataCount: $missing_metadata_count, missingSectionCount: $missing_section_count, driftWarningCount: $drift_warning_count, warnings: $warnings}')
data_hash=$(printf '%s' "$data_hash_input" | sha256_text)

report=$(jq -n \
  --arg schema "ee.plan_drift.v1" \
  --arg generated_at "$generated_at" \
  --arg data_hash "$data_hash" \
  --arg plan_path "$(realpath "$PLAN_PATH" 2>/dev/null || printf '%s' "$PLAN_PATH")" \
  --arg beads_path "$(realpath "$BEADS_JSONL" 2>/dev/null || printf '%s' "$BEADS_JSONL")" \
  --argjson plan_present "$( [ "$plan_present" = true ] && echo true || echo false )" \
  --argjson beads_present "$( [ "$beads_present" = true ] && echo true || echo false )" \
  --argjson plan_mtime_epoch "$plan_epoch" \
  --argjson candidate_count "$candidate_count" \
  --argjson missing_metadata_count "$missing_metadata_count" \
  --argjson missing_section_count "$missing_section_count" \
  --argjson drift_warning_count "$drift_warning_count" \
  --argjson warnings "$warning_json" \
  --argjson hints "$hint_json" \
  '{
    schema: $schema,
    generatedAt: $generated_at,
    dataHash: $data_hash,
    inputs: {
      planPath: $plan_path,
      planPresent: $plan_present,
      planMtimeEpoch: $plan_mtime_epoch,
      beadsPath: $beads_path,
      beadsPresent: $beads_present,
      candidateCount: $candidate_count,
      missingMetadataCount: $missing_metadata_count,
      missingSectionCount: $missing_section_count,
      driftWarningCount: $drift_warning_count
    },
    warnings: $warnings,
    bvRobotTriageHints: $hints
  }')

printf '%s\n' "$report" > "$OUTPUT_PATH"

if [ -n "$JSON_FLAG" ]; then
  printf '%s\n' "$report"
  exit 0
fi

if [ -z "$QUIET_FLAG" ]; then
  echo "Plan drift report -> $OUTPUT_PATH" >&2
  echo "  candidates: $candidate_count" >&2
  echo "  plan_drift_warning: $drift_warning_count" >&2
  echo "  missing_plan_doc_section: $missing_metadata_count" >&2
  echo "  plan_doc_section_missing: $missing_section_count" >&2
fi
