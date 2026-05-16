#!/usr/bin/env bash
# bd-1h8ji.6 — CI-safe RCH verification control-plane e2e driver.
#
# Default mode uses the verifier's deterministic fake-transcript hook so the
# e2e proves JSON/log contracts without starting an expensive remote build.
# Set RCH_VERIFY_CONTROL_PLANE_LONG_BENCH=1 to run the optional heavy bench lane.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
RCH_VERIFY="$REPO_ROOT/scripts/rch_verify.sh"
WORK_DIR="$(mktemp -d /tmp/ee-rch-control-plane.XXXXXX)"

emit_event() {
    local phase="${1:?phase required}"
    local status="${2:?status required}"
    local elapsed_ms="${3:-0}"
    local command_hash="${4:-}"
    local worker_id="${5:-}"
    local degraded_codes_json="${6:-[]}"
    local note="${7:-}"
    PHASE="$phase" \
    STATUS="$status" \
    ELAPSED_MS="$elapsed_ms" \
    COMMAND_HASH="$command_hash" \
    WORKER_ID="$worker_id" \
    DEGRADED_CODES_JSON="$degraded_codes_json" \
    NOTE="$note" \
    python3 - <<'PY'
import json
import os

print(json.dumps({
    "schema": "ee.test_event.v1",
    "surface": "rch_verification_control_plane",
    "bead_id": "bd-1h8ji.6",
    "phase": os.environ["PHASE"],
    "status": os.environ["STATUS"],
    "elapsed_ms": int(os.environ["ELAPSED_MS"]),
    "command_hash": os.environ["COMMAND_HASH"],
    "worker_id": os.environ["WORKER_ID"],
    "degraded_codes": json.loads(os.environ["DEGRADED_CODES_JSON"]),
    "note": os.environ["NOTE"],
}, sort_keys=True, separators=(",", ":")))
PY
}

assert_json() {
    local path="${1:?path required}"
    local expected_status="${2:?expected status required}"
    local expected_worker="${3:-}"
    python3 - "$path" "$expected_status" "$expected_worker" <<'PY'
import json
import sys

path, expected_status, expected_worker = sys.argv[1:4]
with open(path, encoding="utf-8") as handle:
    report = json.load(handle)
if report.get("schema") != "ee.rch.verify.v1":
    raise SystemExit(f"unexpected schema: {report}")
if report.get("status") != expected_status:
    raise SystemExit(f"expected status {expected_status}, got {report.get('status')}: {report}")
if expected_worker and report.get("worker_id") != expected_worker:
    raise SystemExit(f"expected worker {expected_worker}, got {report.get('worker_id')}: {report}")
invocation = report.get("rch_invocation") or []
if "exec" not in invocation:
    raise SystemExit(f"missing explicit rch exec invocation: {report}")
if "/Users/jemanuel" in json.dumps(report):
    raise SystemExit("private user path leaked into proof")
if "token=" in json.dumps(report).lower() or "secret=" in json.dumps(report).lower():
    raise SystemExit("secret-shaped text leaked into proof")
print(json.dumps({
    "command_hash": report.get("command_hash", ""),
    "worker_id": report.get("worker_id") or "",
    "degraded_codes": report.get("degraded_codes") or [],
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

emit_event "setup" "ok" 0 "" "" "[]" "fixture work dir allocated without cleanup deletion"

start="$(started_ms)"
dry_run_json="$WORK_DIR/dry-run.json"
RCH_BIN="${RCH_BIN:-rch}" \
RCH_VERIFY_NOW="2026-05-16T06:40:00.000000Z" \
bash "$RCH_VERIFY" \
    --dry-run \
    --bead-id bd-1h8ji.6 \
    --summary \
    -- \
    cargo test --test rch_verify_contract -- --nocapture > "$dry_run_json"
dry_assert="$(assert_json "$dry_run_json" "dry_run" "")"
emit_event \
    "action" \
    "dry_run_proof_generated" \
    "$(elapsed_since "$start")" \
    "$(printf '%s' "$dry_assert" | python3 -c 'import json,sys; print(json.load(sys.stdin)["command_hash"])')" \
    "" \
    "$(printf '%s' "$dry_assert" | python3 -c 'import json,sys; print(json.dumps(json.load(sys.stdin)["degraded_codes"]))')" \
    "explicit rch exec proof rendered"

start="$(started_ms)"
fake_pass_json="$WORK_DIR/fake-pass.json"
RCH_BIN="${RCH_BIN:-rch}" \
RCH_VERIFY_NOW="2026-05-16T06:40:01.000000Z" \
RCH_VERIFY_FAKE_OUTPUT=$'running 1 test\ntest rch_control_plane ... ok\n[RCH] remote css (0.1s)\n' \
RCH_VERIFY_FAKE_EXIT_CODE=0 \
RCH_VERIFY_FAKE_ELAPSED_MS=100 \
bash "$RCH_VERIFY" \
    --bead-id bd-1h8ji.6 \
    --summary \
    -- \
    cargo test --test rch_verify_control_plane -- --nocapture > "$fake_pass_json"
pass_assert="$(assert_json "$fake_pass_json" "remote_pass" "css")"
emit_event \
    "assert" \
    "fake_remote_pass_validated" \
    "$(elapsed_since "$start")" \
    "$(printf '%s' "$pass_assert" | python3 -c 'import json,sys; print(json.load(sys.stdin)["command_hash"])')" \
    "css" \
    "[]" \
    "remote proof and summary contract validated"

if [ "${RCH_VERIFY_CONTROL_PLANE_LONG_BENCH:-0}" = "1" ]; then
    start="$(started_ms)"
    bench_json="$WORK_DIR/optional-bench.json"
    RCH_BIN="${RCH_BIN:-rch}" \
    RCH_VERIFY_NOW="2026-05-16T06:40:02.000000Z" \
    bash "$RCH_VERIFY" \
        --bead-id bd-1h8ji.6 \
        --summary \
        -- \
        cargo bench --bench graph_minhash_rank > "$bench_json"
    bench_assert="$(assert_json "$bench_json" "remote_pass" "")"
    emit_event \
        "action" \
        "optional_bench_validated" \
        "$(elapsed_since "$start")" \
        "$(printf '%s' "$bench_assert" | python3 -c 'import json,sys; print(json.load(sys.stdin)["command_hash"])')" \
        "$(printf '%s' "$bench_assert" | python3 -c 'import json,sys; print(json.load(sys.stdin)["worker_id"])')" \
        "$(printf '%s' "$bench_assert" | python3 -c 'import json,sys; print(json.dumps(json.load(sys.stdin)["degraded_codes"]))')" \
        "optional heavy remote bench lane completed"
else
    emit_event "action" "optional_bench_skipped" 0 "" "" "[\"manual_heavy_strategy\"]" "set RCH_VERIFY_CONTROL_PLANE_LONG_BENCH=1 to run"
fi

emit_event "cleanup" "no_delete_by_policy" 0 "" "" "[]" "left temporary proof directory in /tmp"
emit_event "summary" "pass" 0 "" "" "[]" "rch verification control-plane e2e completed"
