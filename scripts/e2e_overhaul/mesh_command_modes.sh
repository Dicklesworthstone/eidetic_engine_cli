#!/usr/bin/env bash
# bd-3omr5 - explicit mesh command-mode contract e2e driver.
#
# This script intentionally retains its workspace and log artifacts. AGENTS.md
# forbids agent-side file deletion, and retained artifacts are more useful for
# closeout evidence anyway.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

require_jq() {
    if ! command -v jq >/dev/null 2>&1; then
        echo "mesh_command_modes: jq is required" >&2
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
    echo "mesh_command_modes: set EE_BINARY or CARGO_TARGET_DIR to an ee binary" >&2
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
    "test_id": "mesh_command_modes",
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

json_codes() {
    jq -r '
        [
          .degraded[]?.code,
          .data.degraded[]?.code,
          .. | objects | .code? // empty
        ]
        | map(select(type == "string"))
        | unique
        | join(",")
    ' 2>/dev/null
}

assert_no_mesh_degradation() {
    local json="${1:?json required}"
    local label="${2:?label required}"
    local codes
    codes="$(printf '%s' "$json" | json_codes || true)"
    if printf '%s\n' "$codes" | grep -Eiq 'mesh|tailscale|peer'; then
        assert_eq "$codes" "no mesh/tailscale/peer degradation" "$label"
    else
        assert_eq "true" "true" "$label"
    fi
}

listener_snapshot() {
    if command -v lsof >/dev/null 2>&1; then
        lsof -nP -iTCP -sTCP:LISTEN 2>/dev/null | awk 'NR > 1 {print $1 " " $2 " " $9}' | sort -u
    elif [ -x /usr/sbin/lsof ]; then
        /usr/sbin/lsof -nP -iTCP -sTCP:LISTEN 2>/dev/null | awk 'NR > 1 {print $1 " " $2 " " $9}' | sort -u
    else
        return 127
    fi
}

run_ee_json() {
    local label="${1:?label required}"
    shift
    json_event "command_start" "label" "$label" "command" "$EE_BINARY $* --workspace $WORKSPACE"
    local output
    if output="$(env -u EE_MESH_MODE -u EE_WORKSPACE -u EE_WORKSPACE_REGISTRY EE_MESH_ENABLED=0 "$EE_BINARY" "$@" --workspace "$WORKSPACE" 2>&1)"; then
        json_event "command_end" "label" "$label" "exit_code" "0"
        printf '%s\n' "$output"
        return 0
    fi
    local rc=$?
    json_event "command_end" "label" "$label" "exit_code" "$rc" "stderr_excerpt" "$output"
    printf '%s\n' "$output" >&2
    exit "$rc"
}

require_jq
EE_BINARY="$(resolve_ee_binary)"
ASSERTS_PASS=0
ASSERTS_FAIL=0
TMP_ROOT="${EE_E2E_TMPDIR:-${TMPDIR:-/tmp}}"
case "$TMP_ROOT" in
    /Volumes/*) TMP_ROOT="/tmp" ;;
esac
WORKSPACE="${TMP_ROOT%/}/ee-e2e-mesh-command-modes.$$"
mkdir -p "$WORKSPACE"
export EE_TEST_LOG_PATH="${EE_TEST_LOG_PATH:-$WORKSPACE/mesh_command_modes.jsonl}"

json_event "note" \
    "message" "mesh_command_modes_start" \
    "workspace" "$WORKSPACE" \
    "binary" "$EE_BINARY" \
    "bead_id" "bd-3omr5"

run_ee_json "init" init --json >/dev/null
REMEMBER_JSON="$(run_ee_json "remember" remember --level procedural --kind rule "Mesh command-mode e2e fixture memory." --json)"
assert_eq "$(printf '%s' "$REMEMBER_JSON" | jq -r '.success // false')" "true" "mesh_command_modes_remember_success"
MEMORY_ID="$(printf '%s' "$REMEMBER_JSON" | jq -r '.data.memory_id // empty')"
if [ -z "$MEMORY_ID" ]; then
    assert_eq "<empty>" "memory id" "mesh_command_modes_memory_id_present"
fi

if BEFORE_LISTENERS="$(listener_snapshot)"; then
    HAVE_LISTENER_SNAPSHOT=1
else
    HAVE_LISTENER_SNAPSHOT=0
    BEFORE_LISTENERS=""
    json_event "note" "message" "listener snapshot skipped; lsof unavailable"
fi

OFF_IDS=""
for mode in off cache revisable; do
    SEARCH_JSON="$(run_ee_json "search_$mode" search "mesh command-mode e2e fixture" --mesh "$mode" --json)"
    assert_eq "$(printf '%s' "$SEARCH_JSON" | jq -r '.success // false')" "true" "mesh_command_modes_search_${mode}_success"
    assert_no_mesh_degradation "$SEARCH_JSON" "mesh_command_modes_search_${mode}_no_mesh_degraded"
    IDS="$(printf '%s' "$SEARCH_JSON" | jq -r '[.data.results[]?.docId] | join(",")')"
    if [ "$mode" = "off" ]; then
        OFF_IDS="$IDS"
    else
        assert_eq "$IDS" "$OFF_IDS" "mesh_command_modes_search_${mode}_matches_off_ids"
    fi

    for surface in context pack why status; do
        case "$surface" in
            context)
                JSON="$(run_ee_json "context_$mode" context "mesh command-mode e2e fixture" --max-tokens 500 --mesh "$mode" --json)"
                ;;
            pack)
                JSON="$(run_ee_json "pack_$mode" pack "mesh command-mode e2e fixture" --max-tokens 500 --mesh "$mode" --json)"
                ;;
            why)
                JSON="$(run_ee_json "why_$mode" why "$MEMORY_ID" --mesh "$mode" --json)"
                ;;
            status)
                JSON="$(run_ee_json "status_$mode" status --mesh "$mode" --json)"
                ;;
        esac
        assert_eq "$(printf '%s' "$JSON" | jq -r '.success // false')" "true" "mesh_command_modes_${surface}_${mode}_success"
        assert_no_mesh_degradation "$JSON" "mesh_command_modes_${surface}_${mode}_no_mesh_degraded"
    done
done

if [ "$HAVE_LISTENER_SNAPSHOT" = "1" ]; then
    AFTER_LISTENERS="$(listener_snapshot || true)"
    NEW_MESH_LISTENERS="$(comm -13 <(printf '%s\n' "$BEFORE_LISTENERS") <(printf '%s\n' "$AFTER_LISTENERS") | grep -E 'ee|eidetic|mesh|tailscale' || true)"
    if [ -z "$NEW_MESH_LISTENERS" ]; then
        assert_eq "true" "true" "mesh_command_modes_open_no_mesh_listener"
    else
        assert_eq "$NEW_MESH_LISTENERS" "no new ee/mesh/tailscale listeners" "mesh_command_modes_open_no_mesh_listener"
    fi
fi

json_event "note" \
    "message" "mesh_command_modes_summary" \
    "workspace" "$WORKSPACE" \
    "asserts_pass" "$ASSERTS_PASS" \
    "asserts_fail" "$ASSERTS_FAIL"

echo "mesh_command_modes workspace retained: $WORKSPACE" >&2
echo "mesh_command_modes log: $EE_TEST_LOG_PATH" >&2

if [ "$ASSERTS_FAIL" -gt 0 ]; then
    exit 1
fi
