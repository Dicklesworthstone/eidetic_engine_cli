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
    if command -v b3sum >/dev/null 2>&1; then
        printf 'blake3:%s' "$(printf '%s' "$str" | b3sum | awk '{print $1}')"
    elif python3 -c "import blake3" >/dev/null 2>&1; then
        printf '%s' "$str" | python3 -c "import sys,blake3; print('blake3:'+blake3.blake3(sys.stdin.buffer.read()).hexdigest())"
    else
        printf 'sha256:%s' "$(printf '%s' "$str" | shasum -a 256 | awk '{print $1}')"
    fi
}

_e2e_source_hash() {
    local root="${REPO_ROOT:-$(pwd)}"
    local payload=""
    local rel
    for rel in Cargo.lock Cargo.toml rust-toolchain.toml; do
        if [ -f "$root/$rel" ]; then
            payload="${payload}${rel}=$(_e2e_hash_file "$root/$rel")"$'\n'
        fi
    done
    if [ -z "$payload" ]; then
        printf 'unavailable'
    else
        _e2e_hash_string "$payload"
    fi
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
            case "$kind" in command_end|assert_fail|golden_compare|artifact_manifest) :;; *) return 0;; esac ;;
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

_e2e_tmp_root() {
    printf '%s\n' "${EE_E2E_ARTIFACT_TMPDIR:-${EE_E2E_TMPDIR:-${TMPDIR:-/tmp}}}"
}

_e2e_mktemp_file() {
    local label="${1:-artifact}"
    local root
    root="$(_e2e_tmp_root)"
    mkdir -p "$root"
    mktemp "${root%/}/ee-e2e-${label}.XXXXXX"
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

# Emit a deterministic manifest for the artifact exercised by a verification
# command. Raw output stays out of the log; paths and hashes are enough for
# closeout tooling to locate retained evidence and detect binary confusion.
# Usage: e2e_log_artifact_manifest <phase> <binary_path> [argv...]
e2e_log_artifact_manifest() {
    local phase="${1:-manual}"
    local binary_path="${2:-${EE_BINARY:-}}"
    shift 2 || true

    local args_str=""
    local arg
    for arg in "$@"; do
        if [ -z "$args_str" ]; then args_str="$arg"; else args_str="$args_str"$'\x01'"$arg"; fi
    done

    local binary_hash="unavailable"
    local binary_hash_status="missing"
    if [ -n "$binary_path" ] && [ -f "$binary_path" ]; then
        binary_hash="$(_e2e_hash_file "$binary_path")"
        binary_hash_status="available"
    elif [ -n "$binary_path" ]; then
        binary_hash_status="not_file"
    fi

    local command_hash source_hash manifest_hash execution_substrate host_name
    command_hash="$(_e2e_hash_string "$binary_path"$'\n'"$args_str")"
    source_hash="$(_e2e_source_hash)"
    execution_substrate="${EE_TEST_EXECUTION_SUBSTRATE:-local}"
    if [ -n "${RCH_WORKER_ID:-}${RCH_WORKER_HOST:-}" ]; then
        execution_substrate="rch"
    fi
    host_name="$(hostname 2>/dev/null || printf 'unknown')"
    manifest_hash="$(_e2e_hash_string "$phase"$'\n'"$binary_path"$'\n'"$binary_hash"$'\n'"$command_hash"$'\n'"${CARGO_TARGET_DIR:-}"$'\n'"${EE_E2E_FIXTURE_FILTER:-${EE_TEST_FILTER:-}}"$'\n'"${EPIC_RETENTION_MANIFEST:-${EE_E2E_RETENTION_MANIFEST:-}}")"

    _e2e_emit_event "artifact_manifest" \
        "manifest_schema" "ee.test_artifact_manifest.v1" \
        "phase" "$phase" \
        "binary_path" "$binary_path" \
        "binary_hash" "$binary_hash" \
        "binary_hash_status" "$binary_hash_status" \
        "source_hash" "$source_hash" \
        "command_hash" "$command_hash" \
        "command_arg_count" "$#" \
        "execution_substrate" "$execution_substrate" \
        "local_host" "$host_name" \
        "worker_host" "${RCH_WORKER_HOST:-${RCH_WORKER_ID:-}}" \
        "target_directory" "${CARGO_TARGET_DIR:-}" \
        "fixture_filter" "${EE_E2E_FIXTURE_FILTER:-${EE_TEST_FILTER:-}}" \
        "log_path" "${EE_TEST_LOG_PATH:-}" \
        "retention_manifest_path" "${EPIC_RETENTION_MANIFEST:-${EE_E2E_RETENTION_MANIFEST:-}}" \
        "artifact_manifest_hash" "$manifest_hash"
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
    out_file=$(_e2e_mktemp_file stdout)
    err_file=$(_e2e_mktemp_file stderr)
    local started
    started=$(python3 -c "import time; print(time.monotonic_ns())")
    "$@" >"$out_file" 2>"$err_file"
    local rc=$?
    local ended
    ended=$(python3 -c "import time; print(time.monotonic_ns())")
    local elapsed_ms
    elapsed_ms=$(python3 -c "print(($ended - $started) / 1_000_000.0)")
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
    e2e_log_artifact_manifest "command_end" "$label" "$@"
    cat "$out_file"
    if [ "${EE_E2E_KEEP_ARTIFACTS:-${EE_E2E_KEEP_WORKSPACE:-0}}" = "1" ]; then
        e2e_log_note "e2e_log_command_keep_artifacts stdout=$out_file stderr=$err_file"
    else
        rm -f "$out_file" "$err_file"
    fi
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
    local matched="false"
    case "$op" in
        -le) [ "$got" -le "$want" ] 2>/dev/null && matched="true" ;;
        -lt) [ "$got" -lt "$want" ] 2>/dev/null && matched="true" ;;
        -ge) [ "$got" -ge "$want" ] 2>/dev/null && matched="true" ;;
        -gt) [ "$got" -gt "$want" ] 2>/dev/null && matched="true" ;;
        -eq) [ "$got" -eq "$want" ] 2>/dev/null && matched="true" ;;
        -ne) [ "$got" -ne "$want" ] 2>/dev/null && matched="true" ;;
        *) matched="false" ;;
    esac
    if [ "$matched" = "true" ]; then
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
