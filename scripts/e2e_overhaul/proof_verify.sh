#!/usr/bin/env bash
# bd-nnfq4 — mechanized proof artifact verification smoke.
#
# This driver validates the committed Lean4/TLA+ proof artifact surface and,
# when the external proof tools are installed, runs them in a temporary copy so
# repository sources stay untouched. Missing tools are reported as an explicit
# degradation and do not fail the default verification gate.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
WORK_DIR="${EE_PROOF_VERIFY_WORK_DIR:-$(mktemp -d /tmp/ee-proof-verify.XXXXXX)}"
EVENT_LOG="${EE_PROOF_VERIFY_EVENT_LOG:-}"

FAILED=0

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

emit_event() {
    local phase="${1:?phase required}"
    local status="${2:?status required}"
    local elapsed_ms="${3:-0}"
    local artifact_path="${4:-}"
    local artifact_kind="${5:-}"
    local command="${6:-}"
    local degraded_codes_json="${7:-[]}"
    local first_failure_diagnosis="${8:-}"

    local event
    event="$(
        PHASE="$phase" \
            STATUS="$status" \
            ELAPSED_MS="$elapsed_ms" \
            ARTIFACT_PATH="$artifact_path" \
            ARTIFACT_KIND="$artifact_kind" \
            COMMAND_TEXT="$command" \
            DEGRADED_CODES_JSON="$degraded_codes_json" \
            FIRST_FAILURE_DIAGNOSIS="$first_failure_diagnosis" \
            python3 - <<'PY'
import json
import os

print(json.dumps({
    "schema": "ee.test_event.v1",
    "surface": "proof_verify",
    "bead_id": "bd-nnfq4",
    "phase": os.environ["PHASE"],
    "status": os.environ["STATUS"],
    "elapsed_ms": int(os.environ["ELAPSED_MS"]),
    "artifact_path": os.environ["ARTIFACT_PATH"],
    "artifact_kind": os.environ["ARTIFACT_KIND"],
    "command": os.environ["COMMAND_TEXT"],
    "degraded_codes": json.loads(os.environ["DEGRADED_CODES_JSON"]),
    "first_failure_diagnosis": os.environ["FIRST_FAILURE_DIAGNOSIS"],
}, sort_keys=True, separators=(",", ":")))
PY
    )"
    printf '%s\n' "$event"
    if [ -n "$EVENT_LOG" ]; then
        printf '%s\n' "$event" >> "$EVENT_LOG"
    fi
}

require_file() {
    local path="${1:?path required}"
    local kind="${2:?kind required}"
    if [ -f "$REPO_ROOT/$path" ]; then
        emit_event "artifact" "present" 0 "$path" "$kind" "" "[]" ""
    else
        emit_event "artifact" "missing" 0 "$path" "$kind" "" "[\"proof_violation_detected\"]" "required proof artifact is missing"
        FAILED=1
    fi
}

validate_schema_json() {
    local start
    start="$(started_ms)"
    if python3 - "$REPO_ROOT/docs/schemas/ee.proof_check.v1.json" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as handle:
    schema = json.load(handle)
if schema.get("$id") != "https://eidetic-engine.local/schemas/ee.proof_check.v1.json":
    raise SystemExit("unexpected proof-check schema id")
if "checks" not in schema.get("required", []):
    raise SystemExit("proof-check schema must require checks")
PY
    then
        emit_event "schema" "ok" "$(elapsed_since "$start")" "docs/schemas/ee.proof_check.v1.json" "json_schema" "python3 json schema sanity" "[]" ""
    else
        emit_event "schema" "failed" "$(elapsed_since "$start")" "docs/schemas/ee.proof_check.v1.json" "json_schema" "python3 json schema sanity" "[\"proof_violation_detected\"]" "proof-check schema sanity check failed"
        FAILED=1
    fi
}

run_lean_check() {
    local start
    if ! command -v lake >/dev/null 2>&1; then
        emit_event "tool_check" "degraded" 0 "proofs/lean4/pack_determinism.lean" "lean4" "lake build" "[\"proof_tool_missing\"]" "lake not found in PATH"
        return 0
    fi

    mkdir -p "$WORK_DIR/lean4"
    cp "$REPO_ROOT/proofs/lean4/lakefile.toml" "$WORK_DIR/lean4/"
    cp "$REPO_ROOT/proofs/lean4/pack_determinism.lean" "$WORK_DIR/lean4/"

    start="$(started_ms)"
    if (cd "$WORK_DIR/lean4" && lake build); then
        emit_event "proof_check" "proved" "$(elapsed_since "$start")" "proofs/lean4/pack_determinism.lean" "lean4" "lake build" "[]" ""
    else
        emit_event "proof_check" "failed" "$(elapsed_since "$start")" "proofs/lean4/pack_determinism.lean" "lean4" "lake build" "[\"proof_violation_detected\"]" "Lean4 proof check failed"
        FAILED=1
    fi
}

run_tla_check() {
    local start
    if ! command -v tlc >/dev/null 2>&1; then
        emit_event "tool_check" "degraded" 0 "proofs/tla/agent_mail_coordination.tla" "tla+" "tlc -workers 8 -config MC.cfg agent_mail_coordination.tla" "[\"proof_tool_missing\"]" "tlc not found in PATH"
        return 0
    fi

    mkdir -p "$WORK_DIR/tla"
    cp "$REPO_ROOT/proofs/tla/MC.cfg" "$WORK_DIR/tla/"
    cp "$REPO_ROOT/proofs/tla/agent_mail_coordination.tla" "$WORK_DIR/tla/"

    start="$(started_ms)"
    if (cd "$WORK_DIR/tla" && tlc -workers 8 -config MC.cfg agent_mail_coordination.tla); then
        emit_event "proof_check" "model_checked" "$(elapsed_since "$start")" "proofs/tla/agent_mail_coordination.tla" "tla+" "tlc -workers 8 -config MC.cfg agent_mail_coordination.tla" "[]" ""
    else
        emit_event "proof_check" "failed" "$(elapsed_since "$start")" "proofs/tla/agent_mail_coordination.tla" "tla+" "tlc -workers 8 -config MC.cfg agent_mail_coordination.tla" "[\"proof_violation_detected\"]" "TLA+ model check failed"
        FAILED=1
    fi
}

emit_event "setup" "ok" 0 "" "" "mkdir -p $WORK_DIR" "[]" "temporary proof-check workspace retained"
mkdir -p "$WORK_DIR"

require_file "proofs/lean4/lakefile.toml" "lean4"
require_file "proofs/lean4/pack_determinism.lean" "lean4"
require_file "proofs/tla/MC.cfg" "tla+"
require_file "proofs/tla/agent_mail_coordination.tla" "tla+"
require_file "docs/schemas/ee.proof_check.v1.json" "json_schema"
validate_schema_json

if [ "$FAILED" -eq 0 ]; then
    run_lean_check
    run_tla_check
fi

if [ "$FAILED" -eq 0 ]; then
    emit_event "summary" "pass" 0 "" "" "scripts/e2e_overhaul/proof_verify.sh" "[]" "proof verification smoke completed"
else
    emit_event "summary" "failed" 0 "" "" "scripts/e2e_overhaul/proof_verify.sh" "[\"proof_violation_detected\"]" "proof verification smoke failed"
fi

exit "$FAILED"
