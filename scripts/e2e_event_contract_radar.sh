#!/usr/bin/env bash
# e2e_event_contract_radar.sh -- static E2E evidence-contract scanner.
#
# Cargo-free scanner for shell scripts that emit ee.test_event.v1 rows. It
# reports line-referenced advisory gaps for failure paths that can exit before
# an assert_fail/assert_result row with diagnosis and artifact evidence.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUTPUT_PATH="$ROOT/.e2e-event-contract-radar-report.json"
ALLOWLIST_PATH="${EE_E2E_EVENT_CONTRACT_RADAR_ALLOWLIST:-}"
ALLOWLIST_JSON="[]"
MODE="advisory"
JSON_FLAG=0
QUIET=0
STRICT=0
GENERATED_AT="$(date -u +"%Y-%m-%dT%H:%M:%SZ")"

declare -a INPUTS=()
declare -a SCRIPT_PATHS=()

usage() {
  cat <<'USAGE'
Usage: scripts/e2e_event_contract_radar.sh [--json] [--quiet] [--strict] [--mode advisory|blocking] [--output <path>] [--allowlist <path>] [--scripts-root <path>] [script ...]

  --json                 Emit the JSON report to stdout.
  --quiet                Suppress the human-readable summary.
  --strict               Exit 4 when any advisory/failing gap is detected.
  --mode <mode>          advisory (default) or blocking.
  --output <path>        Override .e2e-event-contract-radar-report.json.
  --allowlist <path>     Optional JSON array of known gaps with scriptPath,
                         reason, owner, and expiresAt fields.
  --scripts-root <path>  Recursively scan shell scripts under a directory.

With no script arguments, scans scripts/e2e_test.sh, top-level e2e scripts,
and scripts/e2e_overhaul/*.sh. The report schema is
ee.e2e_event_contract_radar.v1.
USAGE
}

require_flag_value() {
  local flag="$1"
  if [ "$#" -lt 2 ] || [ -z "${2:-}" ] || [[ "${2:-}" == --* ]]; then
    printf 'e2e-event-contract-radar: %s requires a value\n' "$flag" >&2
    exit 2
  fi
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --json) JSON_FLAG=1; shift ;;
    --quiet) QUIET=1; shift ;;
    --strict) STRICT=1; shift ;;
    --mode)
      require_flag_value "$@"
      MODE="$2"
      shift 2
      ;;
    --output)
      require_flag_value "$@"
      OUTPUT_PATH="$2"
      shift 2
      ;;
    --allowlist)
      require_flag_value "$@"
      ALLOWLIST_PATH="$2"
      shift 2
      ;;
    --scripts-root)
      require_flag_value "$@"
      INPUTS+=("$2")
      shift 2
      ;;
    --help|-h)
      usage
      exit 0
      ;;
    --)
      shift
      while [ "$#" -gt 0 ]; do
        INPUTS+=("$1")
        shift
      done
      ;;
    -*)
      printf 'unknown argument: %s\n' "$1" >&2
      usage >&2
      exit 1
      ;;
    *)
      INPUTS+=("$1")
      shift
      ;;
  esac
done

case "$MODE" in
  advisory|blocking) ;;
  *)
    printf 'invalid mode: %s\n' "$MODE" >&2
    exit 1
    ;;
esac

if ! command -v jq >/dev/null 2>&1; then
  printf 'e2e-event-contract-radar: jq required but not found\n' >&2
  exit 2
fi

load_allowlist() {
  local path="$ALLOWLIST_PATH"
  local abs
  if [ -z "$path" ]; then
    printf '[]'
    return 0
  fi
  case "$path" in
    /*) abs="$path" ;;
    *) abs="$ROOT/$path" ;;
  esac
  if [ ! -f "$abs" ]; then
    printf 'e2e-event-contract-radar: allowlist not found: %s\n' "$path" >&2
    return 1
  fi
  jq -c '
    if type == "array" then .
    elif type == "object" and (.entries | type == "array") then .entries
    else error("allowlist must be a JSON array or an object with entries[]")
    end
    | map({
        scriptPath: (.scriptPath // error("allowlist entry missing scriptPath")),
        reason: (.reason // error("allowlist entry missing reason")),
        owner: (.owner // error("allowlist entry missing owner")),
        expiresAt: (.expiresAt // error("allowlist entry missing expiresAt"))
      })
  ' "$abs"
}

ALLOWLIST_JSON="$(load_allowlist)"

add_script_path() {
  local path="$1"
  local abs rel existing
  if [ ! -f "$path" ]; then
    return
  fi
  case "$path" in
    /*) abs="$path" ;;
    *) abs="$ROOT/$path" ;;
  esac
  if [ ! -f "$abs" ]; then
    return
  fi
  rel="${abs#"$ROOT"/}"
  case "$rel" in
    scripts/lib/*|scripts/e2e_overhaul/lib/*) return ;;
    scripts/*|tests/*) ;;
    *) return ;;
  esac
  case "$rel" in
    *.sh) ;;
    *) return ;;
  esac
  for existing in "${SCRIPT_PATHS[@]}"; do
    [ "$existing" = "$rel" ] && return
  done
  SCRIPT_PATHS+=("$rel")
}

scan_directory() {
  local dir="$1"
  local abs
  case "$dir" in
    /*) abs="$dir" ;;
    *) abs="$ROOT/$dir" ;;
  esac
  [ -d "$abs" ] || return
  while IFS= read -r f; do
    add_script_path "$f"
  done < <(find "$abs" -type f -name '*.sh' -print | sort)
}

if [ "${#INPUTS[@]}" -eq 0 ]; then
  add_script_path "scripts/e2e_test.sh"
  scan_directory "scripts"
else
  for input in "${INPUTS[@]}"; do
    if [ -d "$input" ] || [ -d "$ROOT/$input" ]; then
      scan_directory "$input"
    else
      add_script_path "$input"
    fi
  done
fi

if [ "${#SCRIPT_PATHS[@]}" -gt 0 ]; then
  sorted_paths="$(printf '%s\n' "${SCRIPT_PATHS[@]}" | sort -u)"
  SCRIPT_PATHS=()
  while IFS= read -r rel; do
    [ -n "$rel" ] && SCRIPT_PATHS+=("$rel")
  done <<<"$sorted_paths"
fi

contains_regex() {
  local path="$1"
  local regex="$2"
  grep -Eq "$regex" < <(awk '!/^[[:space:]]*#/' "$path")
}

schema_ids_for() {
  local path="$1"
  awk '!/^[[:space:]]*#/' "$path" |
    grep -hoE 'ee\.[A-Za-z0-9_.-]+\.v[0-9]+' ||
    true
}

emits_test_event() {
  local path="$1"
  contains_regex "$path" \
    '("schema"[[:space:]]*:[[:space:]]*"ee\.test_event\.v1"|schema:[[:space:]]*"ee\.test_event\.v1"|--arg[[:space:]]+schema[[:space:]]+"ee\.test_event\.v1")'
}

coverage_status() {
  local has_event="$1"
  local present="$2"
  if [ "$has_event" -eq 0 ]; then
    printf 'not_applicable'
  elif [ "$present" -eq 1 ]; then
    printf 'present'
  else
    printf 'missing'
  fi
}

line_window() {
  local path="$1"
  local line="$2"
  local start=$((line - 8))
  local end=$((line + 2))
  [ "$start" -lt 1 ] && start=1
  awk -v start="$start" -v end="$end" 'NR >= start && NR <= end && $0 !~ /^[[:space:]]*#/ { print }' "$path"
}

status_from_window() {
  local text="$1"
  local regex="$2"
  if printf '%s\n' "$text" | grep -Eiq "$regex"; then
    printf 'present'
  else
    printf 'missing'
  fi
}

artifact_status_from_window() {
  local text="$1"
  if printf '%s\n' "$text" | grep -Eiq 'stdout[_A-Za-z]*artifact|stdout[_-]?file|stdout[_-]?path|stdoutArtifact' &&
     printf '%s\n' "$text" | grep -Eiq 'stderr[_A-Za-z]*artifact|stderr[_-]?file|stderr[_-]?path|stderrArtifact'; then
    printf 'present'
  elif printf '%s\n' "$text" | grep -Eiq 'artifact[_-]?(path|dir)|artifacts/'; then
    printf 'present'
  else
    printf 'missing'
  fi
}

branch_id_for() {
  local kind="$1"
  local line="$2"
  printf '%s_line_%s' "$kind" "$line" |
    tr '[:upper:]' '[:lower:]' |
    sed -E 's/[^a-z0-9_:-]+/_/g; s/^[^a-z]+/branch_/'
}

trim_trigger() {
  printf '%s' "$1" |
    sed -E 's/^[[:space:]]+//; s/[[:space:]]+$//' |
    cut -c 1-180
}

allowlist_for() {
  local rel="$1"
  jq -cn \
    --argjson entries "$ALLOWLIST_JSON" \
    --arg scriptPath "$rel" \
    --arg now "$GENERATED_AT" \
    'def none: {status: "none", reason: "", owner: "", expiresAt: null};
     (
       $entries
       | map(select(.scriptPath == $scriptPath))
       | sort_by(.expiresAt)
       | last
     ) as $entry
     | if $entry == null then none
       elif $entry.expiresAt <= $now then {
         status: "expired",
         reason: $entry.reason,
         owner: $entry.owner,
         expiresAt: $entry.expiresAt
       }
       else {
         status: "active",
         reason: $entry.reason,
         owner: $entry.owner,
         expiresAt: $entry.expiresAt
       }
       end'
}

failure_path_object() {
  local rel="$1"
  local abs="$2"
  local line="$3"
  local kind="$4"
  local trigger="$5"
  local window assert_status diagnosis_status artifact_status branch_id
  window="$(line_window "$abs" "$line")"
  assert_status="$(status_from_window "$window" 'assert_fail|assert_result|failure[_-]?verdict|emit_assert_fail|emit_assert_result')"
  diagnosis_status="$(status_from_window "$window" 'first_failure_diagnosis|firstFailureDiagnosis|FIRST_FAILURE|emit_assert_fail|emit_assert_result')"
  artifact_status="$(artifact_status_from_window "$window")"
  branch_id="$(branch_id_for "$kind" "$line")"
  jq -cn \
    --arg branchId "$branch_id" \
    --argjson line "$line" \
    --arg trigger "$(trim_trigger "$trigger")" \
    --arg assert "$assert_status" \
    --arg diagnosis "$diagnosis_status" \
    --arg artifacts "$artifact_status" \
    '{
      branchId: $branchId,
      line: $line,
      trigger: $trigger,
      assertFailOrResult: $assert,
      firstFailureDiagnosis: $diagnosis,
      artifactPaths: $artifacts
    }'
}

scan_failure_paths() {
  local rel="$1"
  local abs="$2"
  local has_set_e="$3"
  local paths="[]"
  local seen_lines=""
  local line_no text obj

  while IFS=: read -r line_no text; do
    [ -n "$line_no" ] || continue
    if printf '%s\n' "$text" | grep -Eq '^[[:space:]]*#'; then
      continue
    fi
    case "$seen_lines" in *",$line_no,"*) continue ;; esac
    seen_lines="${seen_lines},${line_no},"
    obj="$(failure_path_object "$rel" "$abs" "$line_no" "exit" "$text")"
    paths="$(printf '%s' "$paths" | jq --argjson obj "$obj" '. + [$obj]')"
  done < <(grep -nE '^[[:space:]]*(exit|return)[[:space:]]+([1-9][0-9]*|78|101|124)\b|[;&|][[:space:]]*(exit|return)[[:space:]]+([1-9][0-9]*|78|101|124)\b' "$abs" 2>/dev/null || true)

  if [ "$has_set_e" -eq 1 ]; then
    while IFS=: read -r line_no text; do
      [ -n "$line_no" ] || continue
      if printf '%s\n' "$text" | grep -Eq '^[[:space:]]*#'; then
        continue
      fi
      if printf '%s\n' "$text" | grep -Eq '(^|[[:space:]])if[[:space:]]+!?|(^|[[:space:]])while[[:space:]]+|[|][|]|&&|set[[:space:]]+\+e'; then
        continue
      fi
      case "$seen_lines" in *",$line_no,"*) continue ;; esac
      seen_lines="${seen_lines},${line_no},"
      obj="$(failure_path_object "$rel" "$abs" "$line_no" "jq_e" "$text")"
      paths="$(printf '%s' "$paths" | jq --argjson obj "$obj" '. + [$obj]')"
    done < <(grep -nE '(^|[[:space:];|])jq[[:space:]][^#]*-e\b' "$abs" 2>/dev/null || true)
  fi

  printf '%s' "$paths"
}

scan_script() {
  local rel="$1"
  local abs="$ROOT/$rel"
  local declared has_event has_set_e failure_paths path_hash
  local command_start command_end assert_ok assert_fail schema_status redaction_status diagnosis_status stdout_status stderr_status env_status
  local coverage status missing_count branch_missing allowlist allowlist_status

  declared="$(schema_ids_for "$abs" | sort -u | jq -R . | jq -s 'unique')"
  has_event=0
  if emits_test_event "$abs"; then
    has_event=1
  fi
  has_set_e=0
  if contains_regex "$abs" '(^|[[:space:]])set[[:space:]]+-[^#]*e'; then
    has_set_e=1
  fi

  failure_paths="[]"
  if [ "$has_event" -eq 1 ]; then
    failure_paths="$(scan_failure_paths "$rel" "$abs" "$has_set_e")"
  fi

  command_start=0; command_end=0; assert_ok=0; assert_fail=0; schema_status=0
  redaction_status=0; diagnosis_status=0; stdout_status=0; stderr_status=0; env_status=0
  contains_regex "$abs" 'command_start|emit_command_start' && command_start=1
  contains_regex "$abs" 'command_end|emit_command_end' && command_end=1
  contains_regex "$abs" 'assert_ok|emit_assert_ok' && assert_ok=1
  contains_regex "$abs" 'assert_fail|assert_result|failure[_-]?verdict|emit_assert_fail|emit_assert_result' && assert_fail=1
  contains_regex "$abs" 'schema_validation_status|schemaValidationStatus|SCHEMA_VALIDATION_STATUS' && schema_status=1
  contains_regex "$abs" 'redaction_status|redactionStatus|REDACTION_STATUS' && redaction_status=1
  contains_regex "$abs" 'first_failure_diagnosis|firstFailureDiagnosis|FIRST_FAILURE' && diagnosis_status=1
  contains_regex "$abs" 'stdout[_A-Za-z]*artifact|stdout_artifact_path|stdoutArtifact|stdout[_-]?file|stdout[_-]?path' && stdout_status=1
  contains_regex "$abs" 'stderr[_A-Za-z]*artifact|stderr_artifact_path|stderrArtifact|stderr[_-]?file|stderr[_-]?path' && stderr_status=1
  contains_regex "$abs" 'sanitized_env|sanitizedEnv|sanitize[_A-Za-z-]*env|scrubbed_env' && env_status=1

  if [ "$has_event" -eq 1 ] && [ "$(printf '%s' "$failure_paths" | jq 'length')" -gt 0 ]; then
    if printf '%s' "$failure_paths" | jq -e 'all(.[]; .assertFailOrResult == "present")' >/dev/null; then
      assert_fail=1
    else
      assert_fail=0
    fi
    if printf '%s' "$failure_paths" | jq -e 'all(.[]; .firstFailureDiagnosis == "present")' >/dev/null; then
      diagnosis_status=1
    else
      diagnosis_status=0
    fi
  fi

  coverage="$(jq -cn \
    --arg commandStart "$(coverage_status "$has_event" "$command_start")" \
    --arg commandEnd "$(coverage_status "$has_event" "$command_end")" \
    --arg assertOk "$(coverage_status "$has_event" "$assert_ok")" \
    --arg assertFailOrResult "$(coverage_status "$has_event" "$assert_fail")" \
    --arg schemaValidationStatus "$(coverage_status "$has_event" "$schema_status")" \
    --arg redactionStatus "$(coverage_status "$has_event" "$redaction_status")" \
    --arg firstFailureDiagnosis "$(coverage_status "$has_event" "$diagnosis_status")" \
    --arg stdoutArtifactPath "$(coverage_status "$has_event" "$stdout_status")" \
    --arg stderrArtifactPath "$(coverage_status "$has_event" "$stderr_status")" \
    --arg sanitizedEnv "$(coverage_status "$has_event" "$env_status")" \
    '{
      commandStart: $commandStart,
      commandEnd: $commandEnd,
      assertOk: $assertOk,
      assertFailOrResult: $assertFailOrResult,
      schemaValidationStatus: $schemaValidationStatus,
      redactionStatus: $redactionStatus,
      firstFailureDiagnosis: $firstFailureDiagnosis,
      stdoutArtifactPath: $stdoutArtifactPath,
      stderrArtifactPath: $stderrArtifactPath,
      sanitizedEnv: $sanitizedEnv
    }')"

  if [ "$has_event" -eq 0 ]; then
    status="not_applicable"
  else
    missing_count="$(printf '%s' "$coverage" | jq '[to_entries[] | select(.value == "missing")] | length')"
    branch_missing="$(printf '%s' "$failure_paths" | jq '[.[] | select(.assertFailOrResult == "missing" or .firstFailureDiagnosis == "missing" or .artifactPaths == "missing")] | length')"
    if [ "$missing_count" -eq 0 ] && [ "$branch_missing" -eq 0 ]; then
      status="pass"
    elif [ "$MODE" = "blocking" ]; then
      status="fail"
    else
      status="advisory_gap"
    fi
  fi

  allowlist="$(allowlist_for "$rel")"
  allowlist_status="$(printf '%s' "$allowlist" | jq -r '.status')"
  if [ "$allowlist_status" = "active" ]; then
    case "$status" in
      advisory_gap|fail) status="known_gap" ;;
    esac
  fi

  path_hash="sha256:$(printf '%s' "$rel" | shasum -a 256 | awk '{print $1}')"
  jq -cn \
    --arg scriptPath "$rel" \
    --arg scriptPathHash "$path_hash" \
    --arg status "$status" \
    --argjson declared "$declared" \
    --argjson coverage "$coverage" \
    --argjson failurePaths "$failure_paths" \
    --argjson allowlist "$allowlist" \
    '{
      scriptPath: $scriptPath,
      scriptPathHash: $scriptPathHash,
      declaredEventSchemas: $declared,
      status: $status,
      coverage: $coverage,
      failurePaths: $failurePaths,
      allowlist: $allowlist
    }'
}

matrix="[]"
for rel in "${SCRIPT_PATHS[@]}"; do
  row="$(scan_script "$rel")"
  matrix="$(printf '%s' "$matrix" | jq --argjson row "$row" '. + [$row]')"
done

summary="$(printf '%s' "$matrix" | jq -c '. as $matrix | {
  scriptCount: length,
  passCount: (map(select(.status == "pass")) | length),
  advisoryGapCount: (map(select(.status == "advisory_gap")) | length),
  knownGapCount: (map(select(.status == "known_gap")) | length),
  failCount: (map(select(.status == "fail")) | length),
  notApplicableCount: (map(select(.status == "not_applicable")) | length),
  failurePathCount: (map(.failurePaths | length) | add // 0),
  missingFailureVerdictCount: (
    .
    | map(.failurePaths[]? | select(.assertFailOrResult == "missing"))
    | length
  ),
  allowlistedGapCount: (
    .
    | map(select(.allowlist.status == "active"))
    | length
  )
}')"

degraded="[]"
if [ "$(printf '%s' "$summary" | jq '.scriptCount')" -eq 0 ]; then
  degraded='[{"code":"no_e2e_scripts_found","severity":"warning","message":"No shell scripts were selected for e2e event-contract scanning."}]'
fi

overall="$(jq -nr --argjson summary "$summary" '
  if $summary.failCount > 0 then "fail"
  elif $summary.advisoryGapCount > 0 then "advisory_gap"
  elif $summary.knownGapCount > 0 then "known_gap"
  elif $summary.scriptCount == 0 then "not_applicable"
  elif $summary.passCount == 0 and $summary.notApplicableCount > 0 then "not_applicable"
  else "pass"
  end
')"

requirements='[
  {
    "id": "command_lifecycle",
    "level": "must",
    "description": "Scripts that invoke ee commands log command_start and command_end evidence."
  },
  {
    "id": "failure_verdicts",
    "level": "must",
    "description": "Every early failure path emits assert_fail or an equivalent assert_result row before exit."
  },
  {
    "id": "failure_diagnosis",
    "level": "must",
    "description": "Every failure verdict includes first_failure_diagnosis suitable for handoff."
  },
  {
    "id": "support_bundle_paths",
    "level": "should",
    "description": "Command and failure evidence records redaction-safe stdout and stderr artifact paths or hashes."
  }
]'

report="$({
  printf '%s\n' "$summary"
  printf '%s\n' "$requirements"
  printf '%s\n' "$matrix"
  printf '%s\n' "$degraded"
} | jq -cn \
  --arg generatedAt "$GENERATED_AT" \
  --arg mode "$MODE" \
  --arg verdict "$overall" \
  '(input) as $summary
  | (input) as $requirements
  | (input) as $matrix
  | (input) as $degraded
  | {
    schema: "ee.e2e_event_contract_radar.v1",
    generatedAt: $generatedAt,
    mode: $mode,
    verdict: $verdict,
    summary: $summary,
    requirements: $requirements,
    matrix: $matrix,
    degraded: $degraded
  }')"

printf '%s\n' "$report" >"$OUTPUT_PATH"

if [ "$JSON_FLAG" -eq 1 ]; then
  printf '%s\n' "$report"
elif [ "$QUIET" -eq 0 ]; then
  jq -r '
    "e2e event contract radar: verdict=\(.verdict) scripts=\(.summary.scriptCount) pass=\(.summary.passCount) advisory_gap=\(.summary.advisoryGapCount) fail=\(.summary.failCount) failure_paths=\(.summary.failurePathCount) missing_failure_verdicts=\(.summary.missingFailureVerdictCount)"
  ' "$OUTPUT_PATH"
  printf 'report: %s\n' "$OUTPUT_PATH"
fi

if [ "$STRICT" -eq 1 ] && [ "$overall" != "pass" ] && [ "$overall" != "not_applicable" ]; then
  exit 4
fi
