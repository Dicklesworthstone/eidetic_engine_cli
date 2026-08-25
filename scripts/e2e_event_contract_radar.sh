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
if ! command -v python3 >/dev/null 2>&1; then
  printf 'e2e-event-contract-radar: python3 required but not found\n' >&2
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

scan_scripts() {
  python3 - "$ROOT" "$MODE" "$GENERATED_AT" "$ALLOWLIST_JSON" "$@" <<'PY'
import hashlib
import json
import re
import sys
from pathlib import Path

root = Path(sys.argv[1])
mode = sys.argv[2]
generated_at = sys.argv[3]
allowlist_entries = json.loads(sys.argv[4])
script_paths = sys.argv[5:]

comment = re.compile(r"^\s*#")
schema_id = re.compile(r"ee\.[A-Za-z0-9_.-]+\.v[0-9]+")
event_schema = re.compile(
    r'("schema"\s*:\s*"ee\.test_event\.v1"|schema:\s*"ee\.test_event\.v1"|--arg\s+schema\s+"ee\.test_event\.v1")'
)
set_e = re.compile(r"(^|\s)set\s+-[^#]*e")
explicit_failure = re.compile(r"^\s*(exit|return)\s+[1-9][0-9]*\b|[;&|]\s*(exit|return)\s+[1-9][0-9]*\b")
jq_e_failure = re.compile(r"(^|[\s;|])jq\s+[^#]*-e\b")
jq_e_guard = re.compile(r"(^|\s)if\s+!?|(^|\s)while\s+|\|\||&&|set\s+\+e")

coverage_patterns = {
    "commandStart": re.compile(r"command_start|emit_command_start"),
    "commandEnd": re.compile(r"command_end|emit_command_end"),
    "assertOk": re.compile(r"assert_ok|emit_assert_ok"),
    "assertFailOrResult": re.compile(r"assert_fail|assert_result|failure[_-]?verdict|emit_assert_fail|emit_assert_result"),
    "schemaValidationStatus": re.compile(r"schema_validation_status|schemaValidationStatus|SCHEMA_VALIDATION_STATUS"),
    "redactionStatus": re.compile(r"redaction_status|redactionStatus|REDACTION_STATUS"),
    "firstFailureDiagnosis": re.compile(r"first_failure_diagnosis|firstFailureDiagnosis|FIRST_FAILURE"),
    "stdoutArtifactPath": re.compile(r"stdout[_A-Za-z]*artifact|stdout_artifact_path|stdoutArtifact|stdout[_-]?file|stdout[_-]?path"),
    "stderrArtifactPath": re.compile(r"stderr[_A-Za-z]*artifact|stderr_artifact_path|stderrArtifact|stderr[_-]?file|stderr[_-]?path"),
    "sanitizedEnv": re.compile(r"sanitized_env|sanitizedEnv|sanitize[_A-Za-z-]*env|scrubbed_env"),
}
assert_evidence = re.compile(r"assert_fail|assert_result|failure[_-]?verdict|emit_assert_fail|emit_assert_result", re.IGNORECASE)
diagnosis_evidence = re.compile(r"first_failure_diagnosis|firstFailureDiagnosis|FIRST_FAILURE|emit_assert_fail|emit_assert_result", re.IGNORECASE)
stdout_evidence = re.compile(r"stdout[_A-Za-z]*artifact|stdout[_-]?file|stdout[_-]?path|stdoutArtifact", re.IGNORECASE)
stderr_evidence = re.compile(r"stderr[_A-Za-z]*artifact|stderr[_-]?file|stderr[_-]?path|stderrArtifact", re.IGNORECASE)
generic_artifact = re.compile(r"artifact[_-]?(path|dir)|artifacts/", re.IGNORECASE)


def allowlist_for(relative: str) -> dict:
    entries = sorted(
        (entry for entry in allowlist_entries if entry["scriptPath"] == relative),
        key=lambda entry: entry["expiresAt"],
    )
    if not entries:
        return {"status": "none", "reason": "", "owner": "", "expiresAt": None}
    entry = entries[-1]
    status = "expired" if entry["expiresAt"] <= generated_at else "active"
    return {
        "status": status,
        "reason": entry["reason"],
        "owner": entry["owner"],
        "expiresAt": entry["expiresAt"],
    }


def presence(found: bool) -> str:
    return "present" if found else "missing"


def failure_path(lines: list[str], line_number: int, kind: str) -> dict:
    start = max(0, line_number - 9)
    end = min(len(lines), line_number + 2)
    window = "\n".join(line for line in lines[start:end] if not comment.match(line))
    artifacts_present = (
        bool(stdout_evidence.search(window)) and bool(stderr_evidence.search(window))
    ) or bool(generic_artifact.search(window))
    branch_id = re.sub(r"[^a-z0-9_:-]+", "_", f"{kind}_line_{line_number}".lower())
    branch_id = re.sub(r"^[^a-z]+", "branch_", branch_id)
    return {
        "branchId": branch_id,
        "line": line_number,
        "trigger": lines[line_number - 1].strip()[:180],
        "assertFailOrResult": presence(bool(assert_evidence.search(window))),
        "firstFailureDiagnosis": presence(bool(diagnosis_evidence.search(window))),
        "artifactPaths": presence(artifacts_present),
    }


def scan_script(relative: str) -> dict:
    lines = (root / relative).read_text(encoding="utf-8", errors="replace").splitlines()
    active_lines = [line for line in lines if not comment.match(line)]
    active_text = "\n".join(active_lines)
    declared = sorted(set(schema_id.findall(active_text)))
    has_event = bool(event_schema.search(active_text))
    has_set_e = bool(set_e.search(active_text))

    failure_paths = []
    if has_event:
        seen = set()
        for line_number, line in enumerate(lines, start=1):
            if comment.match(line) or not explicit_failure.search(line):
                continue
            seen.add(line_number)
            failure_paths.append(failure_path(lines, line_number, "exit"))
        if has_set_e:
            for line_number, line in enumerate(lines, start=1):
                if (
                    line_number in seen
                    or comment.match(line)
                    or not jq_e_failure.search(line)
                    or jq_e_guard.search(line)
                ):
                    continue
                seen.add(line_number)
                failure_paths.append(failure_path(lines, line_number, "jq_e"))

    present = {
        field: bool(pattern.search(active_text))
        for field, pattern in coverage_patterns.items()
    }
    if has_event and failure_paths:
        present["assertFailOrResult"] = all(
            path["assertFailOrResult"] == "present" for path in failure_paths
        )
        present["firstFailureDiagnosis"] = all(
            path["firstFailureDiagnosis"] == "present" for path in failure_paths
        )

    coverage = {
        field: ("not_applicable" if not has_event else presence(value))
        for field, value in present.items()
    }
    if not has_event:
        status = "not_applicable"
    else:
        missing_count = sum(value == "missing" for value in coverage.values())
        branch_missing = sum(
            path["assertFailOrResult"] == "missing"
            or path["firstFailureDiagnosis"] == "missing"
            or path["artifactPaths"] == "missing"
            for path in failure_paths
        )
        if missing_count == 0 and branch_missing == 0:
            status = "pass"
        elif mode == "blocking":
            status = "fail"
        else:
            status = "advisory_gap"

    allowlist = allowlist_for(relative)
    if allowlist["status"] == "active" and status in {"advisory_gap", "fail"}:
        status = "known_gap"

    return {
        "scriptPath": relative,
        "scriptPathHash": "sha256:" + hashlib.sha256(relative.encode()).hexdigest(),
        "declaredEventSchemas": declared,
        "status": status,
        "coverage": coverage,
        "failurePaths": failure_paths,
        "allowlist": allowlist,
    }


print(json.dumps([scan_script(path) for path in script_paths], separators=(",", ":")))
PY
}

matrix="$(scan_scripts "${SCRIPT_PATHS[@]}")"

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
