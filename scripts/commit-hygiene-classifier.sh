#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
usage: scripts/commit-hygiene-classifier.sh [--workspace <path>] [--strict] [--self-test]

Classify staged commits that mix source changes with .beads/issues.jsonl churn.
The check reads the Git index only. It does not run Cargo, mutate Beads, or
change the working tree.

Verdicts:
  source_only
  tracker_only
  mixed_small_tracker_metadata
  mixed_full_tracker_export_churn
  no_staged_changes

Environment:
  EE_COMMIT_HYGIENE_FULL_CHURN_LINES         default: 200
  EE_COMMIT_HYGIENE_FULL_CHURN_RECORD_DELTA  default: 25
EOF
}

WORKSPACE="."
STRICT=false
SELF_TEST=false
LINE_THRESHOLD="${EE_COMMIT_HYGIENE_FULL_CHURN_LINES:-200}"
RECORD_DELTA_THRESHOLD="${EE_COMMIT_HYGIENE_FULL_CHURN_RECORD_DELTA:-25}"

while [ "$#" -gt 0 ]; do
  case "$1" in
    --workspace)
      if [ "$#" -lt 2 ]; then
        echo "commit-hygiene-classifier: --workspace requires a path" >&2
        exit 2
      fi
      WORKSPACE="$2"
      shift 2
      ;;
    --strict)
      STRICT=true
      shift
      ;;
    --self-test)
      SELF_TEST=true
      shift
      ;;
    --json)
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "commit-hygiene-classifier: unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

require_tool() {
  local tool="${1:?tool required}"
  if ! command -v "$tool" >/dev/null 2>&1; then
    printf 'commit-hygiene-classifier: missing required tool: %s\n' "$tool" >&2
    exit 2
  fi
}

require_integer() {
  local name="${1:?name required}"
  local value="${2:?value required}"
  case "$value" in
    ''|*[!0-9]*)
      printf 'commit-hygiene-classifier: %s must be a non-negative integer\n' "$name" >&2
      exit 2
      ;;
  esac
}

require_tool git
require_tool jq
require_integer EE_COMMIT_HYGIENE_FULL_CHURN_LINES "$LINE_THRESHOLD"
require_integer EE_COMMIT_HYGIENE_FULL_CHURN_RECORD_DELTA "$RECORD_DELTA_THRESHOLD"

json_number_or_null() {
  local value="${1:-}"
  if [ -z "$value" ]; then
    printf 'null'
  else
    printf '%s' "$value"
  fi
}

emit_report() {
  local workspace_root="${1:?workspace root required}"
  local staged_paths_json="${2:?staged paths json required}"
  local additions_json="${3:?additions json required}"
  local deletions_json="${4:?deletions json required}"
  local binary_json="${5:?binary json required}"
  local head_record_count_json="${6:?head record count json required}"
  local staged_record_count_json="${7:?staged record count json required}"

  jq -cn \
    --arg workspace "$workspace_root" \
    --argjson staged_paths "$staged_paths_json" \
    --argjson additions "$additions_json" \
    --argjson deletions "$deletions_json" \
    --argjson binary "$binary_json" \
    --argjson head_record_count "$head_record_count_json" \
    --argjson staged_record_count "$staged_record_count_json" \
    --argjson line_threshold "$LINE_THRESHOLD" \
    --argjson record_delta_threshold "$RECORD_DELTA_THRESHOLD" '
      def abs_value: if . < 0 then -. else . end;

      ($staged_paths | map(select(startswith(".beads/")))) as $tracker_paths
      | ($staged_paths | map(select((startswith(".beads/") | not)))) as $source_paths
      | ($staged_paths | any(. == ".beads/issues.jsonl")) as $beads_jsonl_staged
      | (($additions // 0) + ($deletions // 0)) as $line_churn
      | (
          if $head_record_count == null or $staged_record_count == null then
            null
          else
            ($staged_record_count - $head_record_count)
          end
        ) as $record_delta
      | (
          $binary
          or ($line_churn >= $line_threshold)
          or (($record_delta // 0 | abs_value) >= $record_delta_threshold)
        ) as $full_tracker_churn
      | (
          if ($staged_paths | length) == 0 then
            "no_staged_changes"
          elif ($source_paths | length) == 0 and ($tracker_paths | length) > 0 then
            "tracker_only"
          elif ($source_paths | length) > 0 and ($tracker_paths | length) == 0 then
            "source_only"
          elif $full_tracker_churn then
            "mixed_full_tracker_export_churn"
          else
            "mixed_small_tracker_metadata"
          end
        ) as $verdict
      | (
          if $verdict == "mixed_full_tracker_export_churn" then "high"
          elif $verdict == "mixed_small_tracker_metadata" then "warning"
          else "info"
          end
        ) as $severity
      | (
          if $verdict == "mixed_full_tracker_export_churn" then "fail"
          elif $verdict == "mixed_small_tracker_metadata" then "warn"
          else "pass"
          end
        ) as $status
      | (
          if $verdict == "mixed_full_tracker_export_churn" then
            "Split source and tracker commits before committing; do not mix full .beads/issues.jsonl export churn with source changes."
          elif $verdict == "mixed_small_tracker_metadata" then
            "Review the staged .beads/issues.jsonl diff and split tracker metadata unless it is intentionally part of this source commit."
          elif $verdict == "tracker_only" then
            "Proceed only if this is an intentional tracker sync commit."
          elif $verdict == "source_only" then
            "Proceed with an explicit source pathspec; keep tracker sync separate if needed."
          else
            "Stage the intended source or tracker paths before committing."
          end
        ) as $recommended_action
      | {
          schema: "ee.commit_hygiene_classifier.v1",
          status: $status,
          verdict: $verdict,
          severity: $severity,
          workspace: $workspace,
          pathRedaction: "project_relative",
          summary: {
            stagedPathCount: ($staged_paths | length),
            stagedSourcePathCount: ($source_paths | length),
            stagedTrackerPathCount: ($tracker_paths | length),
            beadsJsonlStaged: $beads_jsonl_staged,
            beadsLineAdditions: $additions,
            beadsLineDeletions: $deletions,
            beadsLineChurn: $line_churn,
            beadsRecordCountDelta: $record_delta,
            fullChurnLineThreshold: $line_threshold,
            fullChurnRecordDeltaThreshold: $record_delta_threshold
          },
          beadsNumstat: {
            path: ".beads/issues.jsonl",
            additions: $additions,
            deletions: $deletions,
            binary: $binary
          },
          stagedSourcePaths: {
            count: ($source_paths | length),
            truncated: (($source_paths | length) > 25),
            paths: ($source_paths | sort | .[0:25])
          },
          stagedTrackerPaths: {
            count: ($tracker_paths | length),
            truncated: (($tracker_paths | length) > 25),
            paths: ($tracker_paths | sort | .[0:25])
          },
          recommendedAction: $recommended_action
        }
    '
}

assert_report() {
  local label="${1:?label required}"
  local expected_verdict="${2:?expected verdict required}"
  local expected_severity="${3:?expected severity required}"
  local expected_status="${4:?expected status required}"
  local staged_paths_json="${5:?staged paths json required}"
  local additions_json="${6:?additions json required}"
  local deletions_json="${7:?deletions json required}"
  local binary_json="${8:?binary json required}"
  local head_record_count_json="${9:?head record count json required}"
  local staged_record_count_json="${10:?staged record count json required}"
  local report

  report="$(emit_report "/fixture/$label" "$staged_paths_json" "$additions_json" "$deletions_json" "$binary_json" "$head_record_count_json" "$staged_record_count_json")"
  printf '%s\n' "$report" | jq -e \
    --arg verdict "$expected_verdict" \
    --arg severity "$expected_severity" \
    --arg status "$expected_status" \
    '.schema == "ee.commit_hygiene_classifier.v1"
      and .status == $status
      and .verdict == $verdict
      and .severity == $severity
      and .pathRedaction == "project_relative"
      and (.summary | has("stagedPathCount")
        and has("stagedSourcePathCount")
        and has("stagedTrackerPathCount")
        and has("beadsJsonlStaged")
        and has("beadsLineAdditions")
        and has("beadsLineDeletions")
        and has("beadsLineChurn")
        and has("beadsRecordCountDelta")
        and has("fullChurnLineThreshold")
        and has("fullChurnRecordDeltaThreshold"))
      and (.beadsNumstat | has("path") and has("additions") and has("deletions") and has("binary"))
      and (.summary.stagedPathCount == (.stagedSourcePaths.count + .stagedTrackerPaths.count))
      and (.stagedSourcePaths.truncated | type == "boolean")
      and (.stagedTrackerPaths.truncated | type == "boolean")
      and (.stagedSourcePaths.paths | all(type == "string"))
      and (.stagedTrackerPaths.paths | all(type == "string"))
      and (.recommendedAction | type == "string" and length > 0)' >/dev/null || {
        printf 'commit-hygiene-classifier: self-test failed: %s\n%s\n' "$label" "$report" >&2
        exit 1
      }
}

run_self_test() {
  assert_report \
    "source-only" \
    "source_only" \
    "info" \
    "pass" \
    '["src/lib.rs","docs/guide.md"]' \
    null null false null null

  assert_report \
    "tracker-only" \
    "tracker_only" \
    "info" \
    "pass" \
    '[".beads/issues.jsonl"]' \
    2 1 false 10 11

  assert_report \
    "mixed-small" \
    "mixed_small_tracker_metadata" \
    "warning" \
    "warn" \
    '["src/lib.rs",".beads/issues.jsonl"]' \
    4 3 false 100 100

  assert_report \
    "mixed-full-lines" \
    "mixed_full_tracker_export_churn" \
    "high" \
    "fail" \
    '["src/lib.rs",".beads/issues.jsonl"]' \
    250 0 false 100 100

  assert_report \
    "mixed-full-record-delta" \
    "mixed_full_tracker_export_churn" \
    "high" \
    "fail" \
    '["docs/agent.md",".beads/issues.jsonl"]' \
    2 2 false 100 160

  assert_report \
    "mixed-binary-numstat" \
    "mixed_full_tracker_export_churn" \
    "high" \
    "fail" \
    '["src/lib.rs",".beads/issues.jsonl"]' \
    null null true null null

  assert_report \
    "no-staged-changes" \
    "no_staged_changes" \
    "info" \
    "pass" \
    '[]' \
    null null false null null

  echo "commit-hygiene-classifier: self-test passed"
}

if [ "$SELF_TEST" = true ]; then
  run_self_test
  exit 0
fi

if [ ! -d "$WORKSPACE" ]; then
  printf 'commit-hygiene-classifier: workspace not found: %s\n' "$WORKSPACE" >&2
  exit 2
fi

REPO_ROOT="$(git -C "$WORKSPACE" rev-parse --show-toplevel 2>/dev/null || true)"
if [ -z "$REPO_ROOT" ]; then
  printf 'commit-hygiene-classifier: workspace is not inside a git repository: %s\n' "$WORKSPACE" >&2
  exit 2
fi

staged_paths_json="$(git -C "$REPO_ROOT" diff --cached --name-only -z | jq -Rs 'split("\u0000")[:-1]')"

numstat_line="$(git -C "$REPO_ROOT" diff --cached --numstat -- .beads/issues.jsonl)"
additions_json="null"
deletions_json="null"
binary_json="false"
if [ -n "$numstat_line" ]; then
  additions_field="$(printf '%s\n' "$numstat_line" | awk 'NR == 1 {print $1}')"
  deletions_field="$(printf '%s\n' "$numstat_line" | awk 'NR == 1 {print $2}')"
  if [ "$additions_field" = "-" ] || [ "$deletions_field" = "-" ]; then
    binary_json="true"
  else
    additions_json="$additions_field"
    deletions_json="$deletions_field"
  fi
fi

head_record_count=""
staged_record_count=""
if printf '%s\n' "$staged_paths_json" | jq -e 'index(".beads/issues.jsonl") != null' >/dev/null; then
  if ! head_record_count="$(git -C "$REPO_ROOT" show HEAD:.beads/issues.jsonl 2>/dev/null | awk 'END {print NR}')"; then
    head_record_count=""
  fi
  if ! staged_record_count="$(git -C "$REPO_ROOT" show :.beads/issues.jsonl 2>/dev/null | awk 'END {print NR}')"; then
    staged_record_count=""
  fi
fi

report="$(emit_report \
  "$REPO_ROOT" \
  "$staged_paths_json" \
  "$additions_json" \
  "$deletions_json" \
  "$binary_json" \
  "$(json_number_or_null "$head_record_count")" \
  "$(json_number_or_null "$staged_record_count")")"

printf '%s\n' "$report"

if [ "$STRICT" = true ] && printf '%s\n' "$report" | jq -e '.verdict == "mixed_full_tracker_export_churn"' >/dev/null; then
  exit 1
fi
