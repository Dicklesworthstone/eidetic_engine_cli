#!/usr/bin/env bash
# bead-obsolescence.sh — advisory pass that flags stale open Beads
# and proposes non-mutating follow-up actions (bd-3usjw.41).
#
# Candidate rule:
#   status=open
#   days_since_update > threshold (default: 14)
#   no recent comments inside the threshold window
#   no in_progress sibling under the same parent
#   not present on the current bv critical-path advisory set
#
# Output: .bead-obsolescence-report.json with schema
# 'ee.bead.obsolescence.v1'. Exit code is always 0 when a report can
# be generated; the script never closes, demotes, retargets, or edits
# any Beads.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BEADS_JSONL="${ROOT}/.beads/issues.jsonl"
OUTPUT_PATH="${ROOT}/.bead-obsolescence-report.json"

JSON_FLAG=""
QUIET_FLAG=""
NO_BV_FLAG=""
STALE_DAYS="14"

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
    --no-bv)
      NO_BV_FLAG="1"
      shift
      ;;
    --stale-days)
      if [ "${2:-}" = "" ]; then
        echo "error: --stale-days requires a positive integer" >&2
        exit 1
      fi
      STALE_DAYS="$2"
      shift 2
      ;;
    --help)
      cat <<'USAGE'
Usage: scripts/bead-obsolescence.sh [--json] [--quiet] [--no-bv] [--stale-days N]

  --json          Emit only the JSON report to stdout; diagnostics on stderr.
  --quiet         Suppress the human-readable summary.
  --no-bv         Skip bv critical-path inspection and use only Beads state.
  --stale-days N  Candidate age threshold. Default: 14.

Reads:
  .beads/issues.jsonl
  bv --robot-insights (best effort unless --no-bv)

Writes:
  .bead-obsolescence-report.json

Exit code:
  0 when the advisory report is generated.
USAGE
      exit 0
      ;;
    *)
      echo "error: unknown argument: $1" >&2
      exit 1
      ;;
  esac
done

if ! [[ "$STALE_DAYS" =~ ^[0-9]+$ ]] || [ "$STALE_DAYS" -eq 0 ]; then
  echo "error: --stale-days must be a positive integer" >&2
  exit 1
fi

if ! command -v jq >/dev/null 2>&1; then
  echo "error: jq is required" >&2
  exit 1
fi

generated_at=$(date -u +"%Y-%m-%dT%H:%M:%SZ")

critical_path_source="unavailable"
critical_ids_json="[]"
bv_data_hash=null

if [ -z "$NO_BV_FLAG" ] && command -v bv >/dev/null 2>&1; then
  bv_json=$(BV_INSIGHTS_MAP_LIMIT=50 bv --robot-insights 2>/dev/null || true)
  if printf '%s\n' "$bv_json" | jq -e . >/dev/null 2>&1; then
    critical_path_source="bv_robot_insights"
    critical_ids_json=$(printf '%s\n' "$bv_json" | jq -c '
      [
        (.advanced_insights.k_paths.paths // [] | .[]? | .issue_ids[]?),
        (.Articulation // [] | .[]?),
        (.full_stats.articulation_points // [] | .[]?)
      ]
      | unique
    ')
    bv_data_hash=$(printf '%s\n' "$bv_json" | jq -c '.data_hash // null')
  fi
elif [ -n "$NO_BV_FLAG" ]; then
  critical_path_source="skipped"
fi

if [ ! -f "$BEADS_JSONL" ]; then
  report=$(jq -n \
    --arg schema "ee.bead.obsolescence.v1" \
    --arg generated_at "$generated_at" \
    --arg output_path ".bead-obsolescence-report.json" \
    --argjson stale_days "$STALE_DAYS" \
    '{
      schema: $schema,
      generatedAt: $generated_at,
      dataHash: "unavailable",
      outputPath: $output_path,
      inputs: {
        beadsJsonlPresent: false,
        staleDaysThreshold: $stale_days,
        criticalPathSource: "unavailable",
        criticalPathIssueCount: 0
      },
      summary: {
        openBeadCount: 0,
        staleOpenBeadCount: 0,
        candidateCount: 0
      },
      candidates: [],
      bvRobotInsightsAdvisoryRows: []
    }')
  printf '%s\n' "$report" > "$OUTPUT_PATH"
  [ -n "$JSON_FLAG" ] && printf '%s\n' "$report"
  exit 0
fi

data_hash_input=$(printf 'stale_days=%s\ncritical_ids=%s\n' "$STALE_DAYS" "$critical_ids_json"; cat "$BEADS_JSONL")
if command -v shasum >/dev/null 2>&1; then
  data_hash=$(printf '%s' "$data_hash_input" | shasum -a 256 | awk '{print $1}')
elif command -v sha256sum >/dev/null 2>&1; then
  data_hash=$(printf '%s' "$data_hash_input" | sha256sum | awk '{print $1}')
else
  data_hash="unavailable"
fi

analysis_json=$(jq -s \
  --argjson stale_days "$STALE_DAYS" \
  --argjson critical_ids "$critical_ids_json" \
  '
  def ts_epoch($ts):
    try (
      ($ts // "" | tostring)
      | if length == 0 then null
        else
          sub("\\.[0-9]+Z$"; "Z")
          | sub("Z$"; "+0000")
          | strptime("%Y-%m-%dT%H:%M:%S%z")
          | mktime
        end
    ) catch null;

  def age_days($ts):
    (ts_epoch($ts)) as $epoch
    | if $epoch == null then null else ((now - $epoch) / 86400 | floor) end;

  def parent_id:
    ([.dependencies[]? | select(.type == "parent-child") | .depends_on_id][0] // null);

  def action_for($issue; $parent_status):
    ($issue.labels // []) as $labels
    | ($issue.title // "") as $title
    | ($issue.priority // 4) as $priority
    | if (
        ($labels | any(test("housekeeping|cleanup|obsolete|archive|old"; "i")))
        or ($title | test("obsolete|orphan|duplicate|archive|cleanup"; "i"))
      ) then
        {
          kind: "close_as_obsolete_with_user_confirmation",
          reason: "stale low-activity housekeeping or obsolete-looking item",
          command: ("br close " + $issue.id + " --reason <human-confirmed-obsolete-reason>")
        }
      elif $parent_status == "closed" or $parent_status == null then
        {
          kind: "retarget_to_current_epic",
          reason: "stale item is not attached to a live parent epic",
          command: ("br update " + $issue.id + " --parent <current-epic-id>")
        }
      elif $priority < 4 then
        {
          kind: "priority_demote",
          reason: "stale item remains attached to a live parent but has no recent activity",
          command: ("br update " + $issue.id + " --priority " + (($priority + 1) | tostring))
        }
      else
        {
          kind: "close_as_obsolete_with_user_confirmation",
          reason: "stale lowest-priority item with no recent activity",
          command: ("br close " + $issue.id + " --reason <human-confirmed-obsolete-reason>")
        }
      end;

  . as $issues
  | ($issues | map({key: .id, value: .status}) | from_entries) as $status_by_id
  | ($issues | map({id, status, parent: parent_id})) as $parent_rows
  | ($issues | map(select(.status == "open"))) as $open_issues
  | ($open_issues | map(select((age_days(.updated_at // .created_at) // -1) > $stale_days))) as $stale_open_issues
  | [
      $open_issues[]
      | . as $issue
      | (parent_id) as $parent
      | (age_days($issue.updated_at // $issue.created_at)) as $days_since_update
      | ([($issue.comments // [])[]? | age_days(.created_at)] | map(select(. != null)) | min) as $last_comment_age_days
      | (if $last_comment_age_days == null then false else $last_comment_age_days <= $stale_days end) as $has_recent_comment
      | ([$parent_rows[] | select(.parent == $parent and .status == "in_progress" and .id != $issue.id) | .id]) as $in_progress_siblings
      | (($critical_ids | index($issue.id)) != null) as $is_critical_path
      | select($days_since_update != null)
      | select($days_since_update > $stale_days)
      | select($has_recent_comment | not)
      | select($is_critical_path | not)
      | select(($in_progress_siblings | length) == 0)
      | ($status_by_id[$parent] // null) as $parent_status
      | (action_for($issue; $parent_status)) as $action
      | {
          id: $issue.id,
          title: $issue.title,
          priority: $issue.priority,
          issueType: $issue.issue_type,
          parent: $parent,
          parentStatus: $parent_status,
          labels: ($issue.labels // []),
          daysSinceUpdate: $days_since_update,
          lastCommentAgeDays: $last_comment_age_days,
          inProgressSiblingCount: ($in_progress_siblings | length),
          inProgressSiblingIds: $in_progress_siblings,
          criticalPath: false,
          proposedAction: $action,
          guardrails: [
            "advisory_only",
            "requires_human_confirmation_for_close",
            "do_not_delete_files"
          ]
        }
    ] as $candidates
  | {
      openBeadCount: ($open_issues | length),
      staleOpenBeadCount: ($stale_open_issues | length),
      candidateCount: ($candidates | length),
      candidates: $candidates,
      advisoryRows: (
        $candidates
        | map({
            source: "bead_obsolescence_pass",
            issue_id: .id,
            severity: (if .daysSinceUpdate >= 90 then "medium" else "low" end),
            title: .title,
            days_since_update: .daysSinceUpdate,
            proposed_action: .proposedAction.kind,
            reason: .proposedAction.reason
          })
      )
    }
  ' "$BEADS_JSONL")

open_count=$(printf '%s\n' "$analysis_json" | jq -r '.openBeadCount')
stale_count=$(printf '%s\n' "$analysis_json" | jq -r '.staleOpenBeadCount')
candidate_count=$(printf '%s\n' "$analysis_json" | jq -r '.candidateCount')
candidates_json=$(printf '%s\n' "$analysis_json" | jq -c '.candidates')
advisory_rows_json=$(printf '%s\n' "$analysis_json" | jq -c '.advisoryRows')
critical_path_issue_count=$(printf '%s\n' "$critical_ids_json" | jq 'length')

report=$(jq -n \
  --arg schema "ee.bead.obsolescence.v1" \
  --arg generated_at "$generated_at" \
  --arg data_hash "$data_hash" \
  --arg output_path ".bead-obsolescence-report.json" \
  --arg critical_path_source "$critical_path_source" \
  --argjson stale_days "$STALE_DAYS" \
  --argjson critical_path_issue_count "$critical_path_issue_count" \
  --argjson bv_data_hash "$bv_data_hash" \
  --argjson open_count "$open_count" \
  --argjson stale_count "$stale_count" \
  --argjson candidate_count "$candidate_count" \
  --argjson candidates "$candidates_json" \
  --argjson advisory_rows "$advisory_rows_json" \
  '{
    schema: $schema,
    generatedAt: $generated_at,
    dataHash: $data_hash,
    outputPath: $output_path,
    inputs: {
      beadsJsonlPresent: true,
      staleDaysThreshold: $stale_days,
      criticalPathSource: $critical_path_source,
      criticalPathIssueCount: $critical_path_issue_count,
      bvDataHash: $bv_data_hash
    },
    summary: {
      openBeadCount: $open_count,
      staleOpenBeadCount: $stale_count,
      candidateCount: $candidate_count
    },
    candidates: $candidates,
    bvRobotInsightsAdvisoryRows: $advisory_rows
  }')

printf '%s\n' "$report" > "$OUTPUT_PATH"

if [ -n "$JSON_FLAG" ]; then
  printf '%s\n' "$report"
  exit 0
fi

if [ -z "$QUIET_FLAG" ]; then
  echo "Bead obsolescence report -> $OUTPUT_PATH" >&2
  echo "  open_beads: $open_count" >&2
  echo "  stale_open_beads: $stale_count (threshold=${STALE_DAYS}d)" >&2
  echo "  candidates: $candidate_count" >&2
  echo "  critical_path_source: $critical_path_source" >&2
fi

exit 0
