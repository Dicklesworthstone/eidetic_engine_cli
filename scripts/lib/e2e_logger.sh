#!/usr/bin/env bash
# J1 — bash side of the structured test logging harness.
# Companion to src/obs/test_log.rs. Both follow docs/schemas/test_event_v1.json.
#
# Usage:
#   source "$(dirname "$0")/../../scripts/lib/e2e_logger.sh"
#   e2e_log_start "epic_a_pack_format"
#   e2e_log_command "$EE" remember "hello world" --workspace . --json
#   e2e_log_assert_eq "$ITEM_COUNT" "13" "item_count"
#   e2e_log_end
#
# The harness is opt-in: when EE_TEST_LOG_PATH is unset (and no -start call set
# it), every helper no-ops silently. This lets shared scripts run both inside
# and outside the per-epic driver harness.

set -o pipefail

# ============================================================================
# Globals
# ============================================================================

EE_TEST_LOG_TEST_ID="${EE_TEST_LOG_TEST_ID:-}"
EE_TEST_LOG_LEVEL="${EE_TEST_LOG_LEVEL:-normal}"
EE_TEST_LOG_STDERR_CAP="${EE_TEST_LOG_STDERR_CAP:-4096}"
EE_TEST_LOG_ASSERTS_PASS=0
EE_TEST_LOG_ASSERTS_FAIL=0
EE_TEST_LOG_SCHEMA="ee.test_event.v1"

# ============================================================================
# Internals
# ============================================================================

_e2e_now_iso() {
    # Subsecond RFC 3339 UTC. Coreutils date doesn't support %N on macOS, so we
    # use python3 (always present on the platforms we target).
    python3 -c "from datetime import datetime, timezone; print(datetime.now(timezone.utc).isoformat(timespec='microseconds').replace('+00:00','Z'))"
}

# Use Python for BLAKE3 if available; otherwise fall back to a SHA-256-prefixed
# placeholder (we still mark it with a `sha256:` prefix so consumers can tell).
_e2e_hash_file() {
    local file="$1"
    if command -v b3sum >/dev/null 2>&1; then
        printf 'blake3:%s' "$(b3sum "$file" | awk '{print $1}')"
    elif python3 -c "import blake3" >/dev/null 2>&1; then
        python3 -c "import sys,blake3; print('blake3:'+blake3.blake3(open(sys.argv[1],'rb').read()).hexdigest())" "$file"
    else
        printf 'sha256:%s' "$(shasum -a 256 "$file" | awk '{print $1}')"
    fi
}

_e2e_hash_string() {
    local str="$1"
    local tmp
    tmp=$(mktemp)
    printf '%s' "$str" > "$tmp"
    _e2e_hash_file "$tmp"
    rm -f "$tmp"
}

# Emit a single JSON-line event. Uses python3 for JSON encoding so embedded
# quotes/newlines/UTF-8 are handled correctly. No-op when log path unset.
_e2e_emit_event() {
    [ -z "${EE_TEST_LOG_PATH:-}" ] && return 0
    # Filter by level: quiet drops everything except command_end / assert_fail /
    # golden_compare; normal drops timer_lap; verbose keeps all.
    local kind="$1"
    case "$EE_TEST_LOG_LEVEL" in
        quiet)
            case "$kind" in command_end|assert_fail|golden_compare) :;; *) return 0;; esac ;;
        normal)
            case "$kind" in timer_lap) return 0;; esac ;;
    esac
    shift
    local json_args=()
    while [ $# -gt 0 ]; do
        json_args+=("$1" "$2")
        shift 2
    done
    python3 - "$EE_TEST_LOG_PATH" "$EE_TEST_LOG_SCHEMA" "$(_e2e_now_iso)" "$EE_TEST_LOG_TEST_ID" "$kind" "${json_args[@]}" <<'PYEOF'
import json, sys, os
log_path = sys.argv[1]
event = {
    "schema": sys.argv[2],
    "ts": sys.argv[3],
    "test_id": sys.argv[4],
    "kind": sys.argv[5],
}
fields = {}
i = 6
while i + 1 < len(sys.argv):
    k = sys.argv[i]
    v = sys.argv[i+1]
    # Top-level columns vs free-form fields.
    if k in ("command", "stdin_hash", "stdout_hash", "stderr_excerpt"):
        event[k] = v
    elif k == "exit_code":
        try: event[k] = int(v)
        except ValueError: pass
    elif k == "elapsed_ms":
        try: event[k] = float(v)
        except ValueError: pass
    elif k == "args":
        # Comma-separated arg list -> JSON array
        event[k] = [s for s in v.split("") if s != ""]
    else:
        fields[k] = v
    i += 2
if fields:
    event["fields"] = fields
os.makedirs(os.path.dirname(log_path) or ".", exist_ok=True)
with open(log_path, "a", encoding="utf-8") as f:
    f.write(json.dumps(event) + "\n")
PYEOF
}

# ============================================================================
# Public API
# ============================================================================

# Start a test scenario. Sets test_id + opens the log file.
# Usage: e2e_log_start <test_id> [log_path]
e2e_log_start() {
    EE_TEST_LOG_TEST_ID="${1:?test_id required}"
    if [ -n "${2:-}" ]; then
        export EE_TEST_LOG_PATH="$2"
    elif [ -z "${EE_TEST_LOG_PATH:-}" ]; then
        export EE_TEST_LOG_PATH="${TMPDIR:-/tmp}/ee-test-log.jsonl"
    fi
    EE_TEST_LOG_ASSERTS_PASS=0
    EE_TEST_LOG_ASSERTS_FAIL=0
    _e2e_emit_event "note" "message" "test_start: $EE_TEST_LOG_TEST_ID"
}

# Free-form note event.
# Usage: e2e_log_note "<message>"
e2e_log_note() {
    _e2e_emit_event "note" "message" "${1:-}"
}

# Wrap a command: capture stdout/stderr/exit, emit start+end events, AND
# write stdout to a temp file so callers can use it after.
# Usage:  e2e_log_command "$EE" remember "hello" ...
# Prints stdout (so $(e2e_log_command ...) captures it). Exit code propagates.
e2e_log_command() {
    local label="${1:?command required}"
    local args_str=""
    local arg
    for arg in "$@"; do
        if [ -z "$args_str" ]; then args_str="$arg"; else args_str="$args_str"$'\x01'"$arg"; fi
    done
    _e2e_emit_event "command_start" "command" "$label" "args" "$args_str"
    local out_file err_file
    out_file=$(mktemp)
    err_file=$(mktemp)
    local started=$(python3 -c "import time; print(time.monotonic_ns())")
    "$@" >"$out_file" 2>"$err_file"
    local rc=$?
    local ended=$(python3 -c "import time; print(time.monotonic_ns())")
    local elapsed_ms=$(python3 -c "print(($ended - $started) / 1_000_000.0)")
    local stdout_hash stderr_excerpt
    stdout_hash=$(_e2e_hash_file "$out_file")
    stderr_excerpt=$(head -c "$EE_TEST_LOG_STDERR_CAP" "$err_file")
    _e2e_emit_event "command_end" \
        "command" "$label" \
        "args" "$args_str" \
        "stdout_hash" "$stdout_hash" \
        "stderr_excerpt" "$stderr_excerpt" \
        "exit_code" "$rc" \
        "elapsed_ms" "$elapsed_ms"
    cat "$out_file"
    rm -f "$out_file" "$err_file"
    return $rc
}

# Assert two strings equal. Emits assert_ok or assert_fail.
# Usage: e2e_log_assert_eq "$got" "$want" "label"
e2e_log_assert_eq() {
    local got="${1:-}"
    local want="${2:-}"
    local label="${3:?label required}"
    if [ "$got" = "$want" ]; then
        EE_TEST_LOG_ASSERTS_PASS=$((EE_TEST_LOG_ASSERTS_PASS + 1))
        _e2e_emit_event "assert_ok" "label" "$label"
    else
        EE_TEST_LOG_ASSERTS_FAIL=$((EE_TEST_LOG_ASSERTS_FAIL + 1))
        _e2e_emit_event "assert_fail" "label" "$label" "expected" "$want" "actual" "$got"
        return 1
    fi
}

# Assert numeric comparison. op ∈ {-le, -lt, -ge, -gt, -eq, -ne}.
# Usage: e2e_log_assert_num "$got" -le "$want" "label"
e2e_log_assert_num() {
    local got="${1:?got required}"
    local op="${2:?op required}"
    local want="${3:?want required}"
    local label="${4:?label required}"
    if [ "$got" "$op" "$want" ] 2>/dev/null; then
        EE_TEST_LOG_ASSERTS_PASS=$((EE_TEST_LOG_ASSERTS_PASS + 1))
        _e2e_emit_event "assert_ok" "label" "$label"
    else
        EE_TEST_LOG_ASSERTS_FAIL=$((EE_TEST_LOG_ASSERTS_FAIL + 1))
        _e2e_emit_event "assert_fail" "label" "$label" "expected" "$op $want" "actual" "$got"
        return 1
    fi
}

# Golden compare two files. Emits golden_compare event with matched=true|false.
# Usage: e2e_log_golden_compare <generated> <expected> <name>
e2e_log_golden_compare() {
    local generated="${1:?generated path required}"
    local expected="${2:?expected path required}"
    local name="${3:?name required}"
    local matched="false"
    if diff -q "$generated" "$expected" >/dev/null 2>&1; then
        matched="true"
        EE_TEST_LOG_ASSERTS_PASS=$((EE_TEST_LOG_ASSERTS_PASS + 1))
    else
        EE_TEST_LOG_ASSERTS_FAIL=$((EE_TEST_LOG_ASSERTS_FAIL + 1))
    fi
    _e2e_emit_event "golden_compare" \
        "name" "$name" \
        "generated_path" "$generated" \
        "expected_path" "$expected" \
        "matched" "$matched"
    [ "$matched" = "true" ]
}

# Close the scenario. Writes a summary note and (if outer script wants) the
# pass/fail counters via globals.
e2e_log_end() {
    _e2e_emit_event "note" \
        "message" "test_end: $EE_TEST_LOG_TEST_ID" \
        "asserts_pass" "$EE_TEST_LOG_ASSERTS_PASS" \
        "asserts_fail" "$EE_TEST_LOG_ASSERTS_FAIL"
}
