#!/usr/bin/env bash
# bd-1prrl.1.5 - streaming context e2e driver.
#
# This script intentionally retains its workspace and log artifacts. AGENTS.md
# forbids agent-side file deletion, and retained evidence is useful for closeout.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

require_jq() {
    if ! command -v jq >/dev/null 2>&1; then
        echo "sx1_stream: jq is required" >&2
        exit 2
    fi
}

resolve_ee_binary() {
    if [ -n "${EE_BINARY:-}" ]; then
        printf '%s\n' "$EE_BINARY"
        return 0
    fi
    if [ -n "${CARGO_TARGET_DIR:-}" ] && [ -x "${CARGO_TARGET_DIR%/}/debug/ee" ]; then
        printf '%s\n' "${CARGO_TARGET_DIR%/}/debug/ee"
        return 0
    fi
    if [ -n "${CARGO_TARGET_DIR:-}" ] && [ -x "${CARGO_TARGET_DIR%/}/release/ee" ]; then
        printf '%s\n' "${CARGO_TARGET_DIR%/}/release/ee"
        return 0
    fi
    echo "sx1_stream: set EE_BINARY or CARGO_TARGET_DIR to an ee binary" >&2
    exit 2
}

validate_ee_binary() {
    if [ ! -x "$EE_BINARY" ]; then
        echo "sx1_stream: resolved EE_BINARY is not executable: $EE_BINARY" >&2
        exit 2
    fi
    local version_output
    if version_output="$(env -u EE_WORKSPACE -u EE_WORKSPACE_REGISTRY "$EE_BINARY" --version 2>&1)"; then
        return 0
    else
        rc=$?
    fi
    echo "sx1_stream: resolved EE_BINARY is not runnable: $EE_BINARY (exit $rc)" >&2
    printf '%s\n' "$version_output" >&2
    exit 2
}

json_event() {
    local kind="${1:?kind required}"
    shift
    [ -z "${EE_TEST_LOG_PATH:-}" ] && return 0
    python3 - "$EE_TEST_LOG_PATH" "$kind" "$@" <<'PY'
import json
import os
import sys
from datetime import datetime, timezone

path = sys.argv[1]
event = {
    "schema": "ee.test_event.v1",
    "ts": datetime.now(timezone.utc).isoformat(timespec="microseconds").replace("+00:00", "Z"),
    "test_id": "sx1_stream",
    "kind": sys.argv[2],
}
fields = {}
args = sys.argv[3:]
for index in range(0, len(args), 2):
    fields[args[index]] = args[index + 1]
event["fields"] = fields
os.makedirs(os.path.dirname(path) or ".", exist_ok=True)
with open(path, "a", encoding="utf-8") as handle:
    handle.write(json.dumps(event, sort_keys=True) + "\n")
PY
}

assert_eq() {
    local got="${1:-}"
    local want="${2:-}"
    local label="${3:?label required}"
    if [ "$got" = "$want" ]; then
        ASSERTS_PASS=$((ASSERTS_PASS + 1))
        json_event "assert_ok" "label" "$label"
    else
        ASSERTS_FAIL=$((ASSERTS_FAIL + 1))
        json_event "assert_fail" "label" "$label" "expected" "$want" "actual" "$got"
    fi
}

run_ee_json() {
    local label="${1:?label required}"
    shift
    json_event "command_start" "label" "$label" "command" "$EE_BINARY $* --workspace $WORKSPACE"
    local output
    if output="$(env -u EE_WORKSPACE -u EE_WORKSPACE_REGISTRY "$EE_BINARY" "$@" --workspace "$WORKSPACE" 2>&1)"; then
        json_event "command_end" "label" "$label" "exit_code" "0"
        printf '%s\n' "$output"
        return 0
    else
        rc=$?
    fi
    json_event "command_end" "label" "$label" "exit_code" "$rc" "stderr_excerpt" "$output"
    printf '%s\n' "$output" >&2
    exit "$rc"
}

require_jq
EE_BINARY="$(resolve_ee_binary)"
validate_ee_binary
ASSERTS_PASS=0
ASSERTS_FAIL=0
QUERY="stream release guardrail"
TMP_ROOT="${EE_E2E_TMPDIR:-${TMPDIR:-/tmp}}"
case "$TMP_ROOT" in
    /Volumes/*) TMP_ROOT="/tmp" ;;
esac
WORKSPACE="${TMP_ROOT%/}/ee-e2e-sx1-stream.$$"
mkdir -p "$WORKSPACE"
export EE_TEST_LOG_PATH="${EE_TEST_LOG_PATH:-$WORKSPACE/sx1_stream.jsonl}"
STREAM_PATH="$WORKSPACE/context_stream.ndjson"

json_event "note" \
    "message" "sx1_stream_start" \
    "workspace" "$WORKSPACE" \
    "binary" "$EE_BINARY" \
    "bead_id" "bd-1prrl.1.5"

run_ee_json "init" init --json >/dev/null
for content in \
    "Use streaming context frames when agents need incremental release guardrails." \
    "The stream trailer pack hash must equal the batch context pack hash." \
    "A partial context stream without a terminal frame must not be treated as a complete pack." \
    "Item frames must preserve rank and sequence ordering from the batch pack." \
    "The batch context response remains the canonical selection source until direct streaming lands."
do
    run_ee_json "remember" remember --level procedural --kind rule "$content" --json >/dev/null
done

BATCH_JSON="$(run_ee_json "context_batch" --format json context "$QUERY" --max-tokens 900)"
BATCH_HASH="$(printf '%s' "$BATCH_JSON" | jq -r '.data.pack.hash // empty')"
BATCH_IDS="$(printf '%s' "$BATCH_JSON" | jq -r '[.data.pack.items[]?.memoryId] | join(",")')"
BATCH_COUNT="$(printf '%s' "$BATCH_JSON" | jq -r '.data.pack.items | length')"
assert_eq "$(printf '%s' "$BATCH_JSON" | jq -r '.success // false')" "true" "sx1_stream_batch_success"
if [ -z "$BATCH_HASH" ]; then
    assert_eq "<empty>" "batch hash" "sx1_stream_batch_hash_present"
fi
if [ "$BATCH_COUNT" = "0" ]; then
    assert_eq "0" "nonzero batch item count" "sx1_stream_batch_items_present"
fi

run_ee_json "context_stream" --format json context "$QUERY" --max-tokens 900 --stream >"$STREAM_PATH"
FRAME_COUNT="$(wc -l <"$STREAM_PATH" | tr -d ' ')"
HEADER_KIND="$(sed -n '1p' "$STREAM_PATH" | jq -r '.kind // empty')"
TRAILER_KIND="$(tail -n 1 "$STREAM_PATH" | jq -r '.kind // empty')"
TRAILER_HASH="$(tail -n 1 "$STREAM_PATH" | jq -r '.packHash // empty')"
TRAILER_TOTAL="$(tail -n 1 "$STREAM_PATH" | jq -r '.totalItems // -1')"
STREAM_IDS="$(sed '1d;$d' "$STREAM_PATH" | jq -rs '[.[] | .memoryId] | join(",")')"
BAD_SEQ_COUNT="$(sed '1d;$d' "$STREAM_PATH" | jq -rs '[to_entries[] | select(.value.seq != .key or .value.rank != (.key + 1))] | length')"
TERMINAL_COUNT="$(jq -r '.kind' "$STREAM_PATH" | grep -Ec '^(trailer|error|cancelled)$' || true)"

assert_eq "$HEADER_KIND" "header" "sx1_stream_header_first"
assert_eq "$TRAILER_KIND" "trailer" "sx1_stream_trailer_last"
assert_eq "$TRAILER_HASH" "$BATCH_HASH" "sx1_stream_trailer_hash_matches_batch"
assert_eq "$TRAILER_TOTAL" "$BATCH_COUNT" "sx1_stream_trailer_total_matches_batch"
assert_eq "$STREAM_IDS" "$BATCH_IDS" "sx1_stream_item_order_matches_batch"
assert_eq "$BAD_SEQ_COUNT" "0" "sx1_stream_item_seq_rank_monotone"
assert_eq "$TERMINAL_COUNT" "1" "sx1_stream_exactly_one_terminal_frame"
assert_eq "$FRAME_COUNT" "$((BATCH_COUNT + 2))" "sx1_stream_header_items_trailer_count"

json_event "note" \
    "message" "sx1_stream_summary" \
    "workspace" "$WORKSPACE" \
    "stream_path" "$STREAM_PATH" \
    "asserts_pass" "$ASSERTS_PASS" \
    "asserts_fail" "$ASSERTS_FAIL"

echo "sx1_stream workspace retained: $WORKSPACE" >&2
echo "sx1_stream stream: $STREAM_PATH" >&2
echo "sx1_stream log: $EE_TEST_LOG_PATH" >&2

if [ "$ASSERTS_FAIL" -gt 0 ]; then
    exit 1
fi
