#!/usr/bin/env bash
# Driver for F1-F5 agent-ergonomics e2e scripts.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
DEFAULT_AGENT_BUILD_ROOT="/Volumes/USBNVME16TB/temp_agent_space"

if [ "${VERIFY_AGENT_ERGONOMICS_E2E:-1}" = "0" ]; then
    echo "agent-ergonomics e2e: VERIFY_AGENT_ERGONOMICS_E2E=0, skipping"
    exit 0
fi

if [ -d "$DEFAULT_AGENT_BUILD_ROOT" ]; then
    mkdir -p "$DEFAULT_AGENT_BUILD_ROOT/cargo-target" "$DEFAULT_AGENT_BUILD_ROOT/tmp" 2>/dev/null || true
    export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$DEFAULT_AGENT_BUILD_ROOT/cargo-target}"
    export TMPDIR="${EE_AGENT_TMPDIR:-$DEFAULT_AGENT_BUILD_ROOT/tmp}"
fi

RUN_ID="$(python3 -c 'from datetime import datetime, timezone; print(datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ"))' 2>/dev/null || date -u +%Y%m%dT%H%M%SZ)"
LOG_ROOT="${AGENT_ERGONOMICS_E2E_LOG_ROOT:-$REPO_ROOT/tests/logs/agent_ergonomics_${RUN_ID}.${BASHPID:-$$}}"
WORKSPACE_ROOT="${AGENT_ERGONOMICS_E2E_WORKSPACE_ROOT:-$LOG_ROOT/workspaces}"
SUMMARY_JSONL="$LOG_ROOT/results.jsonl"
SUMMARY_JSON="$LOG_ROOT/summary.json"
REQUIRE_ALL="${AGENT_ERGONOMICS_E2E_REQUIRE_ALL:-0}"
SCRIPT_BUDGET_SECONDS="${AGENT_ERGONOMICS_E2E_SCRIPT_BUDGET_SECONDS:-60}"
TOTAL_BUDGET_SECONDS="${AGENT_ERGONOMICS_E2E_TOTAL_BUDGET_SECONDS:-300}"

mkdir -p "$LOG_ROOT" "$WORKSPACE_ROOT"

SCRIPTS=(
    e2e_curate_reject_with_reason.sh
    e2e_pack_budget_too_small.sh
    e2e_harmful_burst_quarantine.sh
    e2e_embed_model_unavailable.sh
    e2e_rule_validation_counter.sh
)

append_record() {
    local script="$1"
    local status="$2"
    local exit_code="$3"
    local elapsed_seconds="$4"
    local log_dir="$5"
    local note="$6"
    python3 - "$SUMMARY_JSONL" "$script" "$status" "$exit_code" "$elapsed_seconds" "$log_dir" "$note" <<'PY'
import json
import sys
from datetime import datetime, timezone

path, script, status, exit_code, elapsed_seconds, log_dir, note = sys.argv[1:]
record = {
    "schema": "ee.agent_ergonomics_e2e.result.v1",
    "ts": datetime.now(timezone.utc).isoformat(timespec="microseconds").replace("+00:00", "Z"),
    "script": script,
    "status": status,
    "exitCode": int(exit_code),
    "elapsedSeconds": int(elapsed_seconds),
    "logDir": log_dir,
    "note": note or None,
}
with open(path, "a", encoding="utf-8") as handle:
    handle.write(json.dumps(record, sort_keys=True) + "\n")
PY
}

started=$(date +%s)
pass=0
fail=0
skip=0

for script in "${SCRIPTS[@]}"; do
    path="$SCRIPT_DIR/$script"
    name="${script%.sh}"
    log_dir="$LOG_ROOT/$name"
    workspace="$WORKSPACE_ROOT/$name"
    mkdir -p "$log_dir" "$workspace"

    if [ ! -e "$path" ]; then
        echo "[SKIP] $script is not present yet"
        append_record "$script" "skip" 0 0 "$log_dir" "missing future script"
        skip=$((skip + 1))
        continue
    fi

    if [ ! -x "$path" ]; then
        echo "[FAIL] $script exists but is not executable"
        append_record "$script" "fail" 126 0 "$log_dir" "script is not executable"
        fail=$((fail + 1))
        continue
    fi

    echo "[RUN ] $script"
    script_started=$(date +%s)
    set +e
    WORKSPACE="$workspace" \
        LOG_DIR="$log_dir" \
        EE_TEST_LOG_PATH="$log_dir/events.jsonl" \
        EE_BIN="${EE_BIN:-ee}" \
        "$path" >"$log_dir/stdout.txt" 2>"$log_dir/stderr.txt"
    rc=$?
    set -e
    script_ended=$(date +%s)
    elapsed=$((script_ended - script_started))

    if [ "$rc" -eq 0 ] && [ "$elapsed" -le "$SCRIPT_BUDGET_SECONDS" ]; then
        echo "[PASS] $script (${elapsed}s)"
        append_record "$script" "pass" "$rc" "$elapsed" "$log_dir" ""
        pass=$((pass + 1))
    elif [ "$rc" -eq 0 ]; then
        echo "[FAIL] $script exceeded ${SCRIPT_BUDGET_SECONDS}s budget (${elapsed}s)"
        append_record "$script" "fail" 124 "$elapsed" "$log_dir" "script budget exceeded"
        fail=$((fail + 1))
    else
        echo "[FAIL] $script exit=$rc (${elapsed}s)"
        append_record "$script" "fail" "$rc" "$elapsed" "$log_dir" "script failed"
        fail=$((fail + 1))
    fi

    total_elapsed=$((script_ended - started))
    if [ "$total_elapsed" -gt "$TOTAL_BUDGET_SECONDS" ]; then
        echo "[FAIL] agent-ergonomics suite exceeded ${TOTAL_BUDGET_SECONDS}s budget (${total_elapsed}s)"
        fail=$((fail + 1))
        break
    fi
done

ended=$(date +%s)
total_elapsed=$((ended - started))

python3 - "$SUMMARY_JSONL" "$SUMMARY_JSON" "$pass" "$fail" "$skip" "$total_elapsed" "$LOG_ROOT" <<'PY'
import json
import sys

jsonl_path, summary_path, passed, failed, skipped, elapsed, log_root = sys.argv[1:]
records = []
try:
    with open(jsonl_path, encoding="utf-8") as handle:
        records = [json.loads(line) for line in handle if line.strip()]
except FileNotFoundError:
    records = []

summary = {
    "schema": "ee.agent_ergonomics_e2e.summary.v1",
    "logRoot": log_root,
    "totals": {
        "pass": int(passed),
        "fail": int(failed),
        "skip": int(skipped),
        "elapsedSeconds": int(elapsed),
    },
    "scripts": records,
}
with open(summary_path, "w", encoding="utf-8") as handle:
    json.dump(summary, handle, indent=2, sort_keys=True)
    handle.write("\n")
PY

echo "agent-ergonomics e2e summary: pass=$pass fail=$fail skip=$skip elapsed=${total_elapsed}s"
echo "Artifacts: $LOG_ROOT"

if [ "$REQUIRE_ALL" = "1" ] && [ "$skip" -gt 0 ]; then
    echo "agent-ergonomics e2e: missing scripts are fatal because AGENT_ERGONOMICS_E2E_REQUIRE_ALL=1" >&2
    exit 1
fi

if [ "$fail" -gt 0 ]; then
    exit 1
fi

exit 0
