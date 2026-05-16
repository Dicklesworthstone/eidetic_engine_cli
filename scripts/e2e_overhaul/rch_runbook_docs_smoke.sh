#!/usr/bin/env bash
# bd-1h8ji.8 — CI-safe RCH runbook docs-lint and copy-paste smoke.
#
# This driver extracts a real documented dry-run command and executes that exact
# command text. The command goes through scripts/rch_verify.sh --dry-run, so it
# proves the RCH invocation shape without starting local Cargo or a remote build.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
DOC_LINT="$REPO_ROOT/scripts/check-rch-doc-examples.py"
WORK_DIR="$(mktemp -d /tmp/ee-rch-doc-examples.XXXXXX)"

emit_event() {
    local phase="${1:?phase required}"
    local status="${2:?status required}"
    local elapsed_ms="${3:-0}"
    local command_hash="${4:-}"
    local source_file="${5:-}"
    local block_index="${6:-}"
    local normalized_command="${7:-}"
    local degraded_codes_json="${8:-[]}"
    local first_failure_diagnosis="${9:-}"
    PHASE="$phase" \
    STATUS="$status" \
    ELAPSED_MS="$elapsed_ms" \
    COMMAND_HASH="$command_hash" \
    SOURCE_FILE="$source_file" \
    BLOCK_INDEX="$block_index" \
    NORMALIZED_COMMAND="$normalized_command" \
    DEGRADED_CODES_JSON="$degraded_codes_json" \
    FIRST_FAILURE_DIAGNOSIS="$first_failure_diagnosis" \
    python3 - <<'PY'
import json
import os

print(json.dumps({
    "schema": "ee.test_event.v1",
    "surface": "rch_doc_examples",
    "bead_id": "bd-1h8ji.8",
    "phase": os.environ["PHASE"],
    "status": os.environ["STATUS"],
    "elapsed_ms": int(os.environ["ELAPSED_MS"]),
    "command_hash": os.environ["COMMAND_HASH"],
    "source_file": os.environ["SOURCE_FILE"],
    "fenced_block_index": os.environ["BLOCK_INDEX"],
    "normalized_command": os.environ["NORMALIZED_COMMAND"],
    "degraded_codes": json.loads(os.environ["DEGRADED_CODES_JSON"]),
    "first_failure_diagnosis": os.environ["FIRST_FAILURE_DIAGNOSIS"],
}, sort_keys=True, separators=(",", ":")))
PY
}

started_ms() {
    python3 - <<'PY'
import time
print(int(time.time() * 1000))
PY
}

elapsed_since() {
    local start="${1:?start ms required}"
    python3 - "$start" <<'PY'
import sys
import time
print(max(0, int(time.time() * 1000) - int(sys.argv[1])))
PY
}

json_field() {
    local path="${1:?path required}"
    local field="${2:?field required}"
    python3 - "$path" "$field" <<'PY'
import json
import sys

path, field = sys.argv[1:3]
with open(path, encoding="utf-8") as handle:
    payload = json.load(handle)
value = payload
for part in field.split("."):
    value = value[part]
print(value)
PY
}

assert_lint_report() {
    local path="${1:?path required}"
    python3 - "$path" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as handle:
    report = json.load(handle)
if report.get("schema") != "ee.rch_doc_examples.v1":
    raise SystemExit(f"unexpected lint schema: {report}")
if report.get("status") != "ok" or report.get("denied_count") != 0:
    raise SystemExit(f"RCH doc examples lint failed: {report}")
required = {"docs/rch_runbook.md", "docs/rch_verification.md", "AGENTS.md", "README.md"}
seen = {entry.get("path") for entry in report.get("checked_files", [])}
missing = sorted(required - seen)
if missing:
    raise SystemExit(f"lint did not cover required files: {missing}")
if report.get("command_count", 0) <= 0:
    raise SystemExit(f"lint did not inspect any commands: {report}")
PY
}

assert_dry_run_report() {
    local path="${1:?path required}"
    python3 - "$path" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as handle:
    report = json.load(handle)
if report.get("schema") != "ee.rch.verify.v1":
    raise SystemExit(f"unexpected RCH proof schema: {report}")
if report.get("status") != "dry_run":
    raise SystemExit(f"expected dry_run proof, got {report.get('status')}: {report}")
if report.get("remote_required") is not True:
    raise SystemExit(f"remote_required was not true: {report}")
invocation = report.get("rch_invocation") or []
if "exec" not in invocation:
    raise SystemExit(f"RCH invocation did not include exec: {report}")
if report.get("exit_code") is not None:
    raise SystemExit(f"dry-run proof unexpectedly has an exit code: {report}")
if "rch_verify_dry_run" not in (report.get("degraded_codes") or []):
    raise SystemExit(f"dry-run proof did not carry rch_verify_dry_run: {report}")
PY
}

emit_event "setup" "ok" 0 "" "" "" "" "[]" "retained temp directory allocated"

start="$(started_ms)"
lint_json="$WORK_DIR/rch-doc-examples.json"
python3 "$DOC_LINT" --repo-root "$REPO_ROOT" --json > "$lint_json"
assert_lint_report "$lint_json"
emit_event \
    "action" \
    "lint_passed" \
    "$(elapsed_since "$start")" \
    "" \
    "" \
    "" \
    "python3 scripts/check-rch-doc-examples.py --json" \
    "[]" \
    "all scanned command examples allowed"

start="$(started_ms)"
smoke_json="$WORK_DIR/smoke-command.json"
python3 "$DOC_LINT" --repo-root "$REPO_ROOT" --extract-smoke-command --json > "$smoke_json"
smoke_command="$(json_field "$smoke_json" command)"
smoke_hash="$(json_field "$smoke_json" command_hash)"
smoke_file="$(json_field "$smoke_json" source_file)"
smoke_block="$(json_field "$smoke_json" block_index)"
emit_event \
    "action" \
    "smoke_command_extracted" \
    "$(elapsed_since "$start")" \
    "$smoke_hash" \
    "$smoke_file" \
    "$smoke_block" \
    "$smoke_command" \
    "[]" \
    "real docs command selected"

start="$(started_ms)"
proof_json="$WORK_DIR/smoke-proof.json"
(
    cd "$REPO_ROOT"
    RCH_BIN="${RCH_BIN:-rch}" \
    RCH_VERIFY_NOW="2026-05-16T07:20:00.000000Z" \
    bash -lc "$smoke_command" > "$proof_json"
)
assert_dry_run_report "$proof_json"
emit_event \
    "assert" \
    "dry_run_proof_validated" \
    "$(elapsed_since "$start")" \
    "$smoke_hash" \
    "$smoke_file" \
    "$smoke_block" \
    "$smoke_command" \
    "[\"rch_verify_dry_run\"]" \
    "copy-pasted docs command reached RCH verifier dry-run mode"

emit_event "cleanup" "no_delete_by_policy" 0 "" "" "" "" "[]" "left temporary docs-smoke directory in /tmp"
emit_event "summary" "pass" 0 "$smoke_hash" "$smoke_file" "$smoke_block" "$smoke_command" "[]" "rch docs lint and smoke completed"
