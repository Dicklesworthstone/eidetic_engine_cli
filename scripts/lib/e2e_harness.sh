#!/usr/bin/env bash
# bd-1n0np.15.1 — general feature-e2e harness for the dueling-wizards initiative.
#
# Builds ON the canonical structured logger (scripts/lib/e2e_logger.sh,
# companion to src/obs/test_log.rs / docs/schemas/test_event_v1.json). It does
# NOT reimplement logging/hashing; it adds the per-feature ergonomics every new
# e2e script needs:
#   - EE_BIN resolution (real binary; honors CARGO_TARGET_DIR / RCH artifacts)
#   - with_temp_workspace: an isolated EE_DATABASE_PATH + index dir per test
#   - assert_eq / assert_contains / assert_jq / assert_exit (+ PASS/FAIL counters)
#   - log_drop: the no-silent-cap rule — any truncation/sampling/top-N/abstention
#     a test observes MUST be logged with its dropped-count + reason
#   - harness_summary: per-run summary.json + human summary; nonzero exit on FAIL
#
# Usage:
#   SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
#   source "$SCRIPT_DIR/../lib/e2e_harness.sh"   # adjust relative depth as needed
#   harness_init "why_not"
#   with_temp_workspace ws
#     "$EE_BIN" --workspace "$ws" init --json >/dev/null
#     out="$("$EE_BIN" --workspace "$ws" remember "x" --json)"
#     assert_jq "$out" '.success == true' "remember succeeded"
#   end_temp_workspace
#   harness_summary   # prints summary, writes summary.json, exits nonzero on FAIL
#
# The harness is opt-in/no-op-safe: if EE_TEST_LOG_PATH is unset, harness_init
# sets one under LOG_DIR so events are always captured.

set -o pipefail

E2E_HARNESS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$E2E_HARNESS_DIR/../.." && pwd)}"
export REPO_ROOT

# shellcheck source=scripts/lib/e2e_logger.sh
# shellcheck disable=SC1091
source "$E2E_HARNESS_DIR/e2e_logger.sh"

# ---------------------------------------------------------------------------
# Counters / state
# ---------------------------------------------------------------------------
HARNESS_TEST_NAME=""
HARNESS_PASS=0
HARNESS_FAIL=0
HARNESS_STEP=0
HARNESS_DROPS=0
HARNESS_FAILURES=()
HARNESS_START_NS=0
HARNESS_TMP_WORKSPACES=()

# ---------------------------------------------------------------------------
# EE_BIN resolution: explicit override wins; else the cargo target dir's release
# binary (this repo redirects CARGO_TARGET_DIR to external storage on some
# hosts); else `ee` on PATH. We never build here — e2e runs a prebuilt binary.
# ---------------------------------------------------------------------------
_harness_resolve_ee_bin() {
    if [ -n "${EE_BIN:-}" ]; then printf '%s' "$EE_BIN"; return 0; fi
    local target_dir
    target_dir="$(cd "$REPO_ROOT" && cargo metadata --no-deps --format-version 1 2>/dev/null \
        | python3 -c 'import sys,json; print(json.load(sys.stdin).get("target_directory",""))' 2>/dev/null)"
    if [ -n "$target_dir" ] && [ -x "$target_dir/release/ee" ]; then
        printf '%s' "$target_dir/release/ee"; return 0
    fi
    if [ -n "$target_dir" ] && [ -x "$target_dir/debug/ee" ]; then
        printf '%s' "$target_dir/debug/ee"; return 0
    fi
    printf 'ee'
}

_harness_now_ns() { python3 -c 'import time; print(time.time_ns())'; }

# harness_init <test_name>
harness_init() {
    HARNESS_TEST_NAME="${1:?harness_init: test_name required}"
    HARNESS_PASS=0; HARNESS_FAIL=0; HARNESS_STEP=0; HARNESS_DROPS=0; HARNESS_FAILURES=()
    EE_BIN="$(_harness_resolve_ee_bin)"
    export EE_BIN
    local run_id="${EE_E2E_RUN_ID:-$(python3 -c 'from datetime import datetime,timezone; print(datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ"))')}"
    LOG_DIR="${LOG_DIR:-$REPO_ROOT/tests/logs/wizard_e2e/${HARNESS_TEST_NAME}.${run_id}.${BASHPID:-$$}}"
    mkdir -p "$LOG_DIR"
    export LOG_DIR
    EE_TEST_LOG_PATH="${EE_TEST_LOG_PATH:-$LOG_DIR/events.jsonl}"
    export EE_TEST_LOG_PATH
    HARNESS_START_NS="$(_harness_now_ns)"
    e2e_log_start "$HARNESS_TEST_NAME"
    e2e_log_note "harness_init test=$HARNESS_TEST_NAME ee_bin=$EE_BIN log_dir=$LOG_DIR"
    printf '[harness] %s starting (ee=%s, logs=%s)\n' "$HARNESS_TEST_NAME" "$EE_BIN" "$LOG_DIR" >&2
}

# step <name> — group subsequent asserts under a named step.
step() {
    HARNESS_STEP=$((HARNESS_STEP + 1))
    e2e_log_note "step ${HARNESS_STEP}: ${1:-}"
    printf '[harness] step %d: %s\n' "$HARNESS_STEP" "${1:-}" >&2
}

_harness_pass() {
    HARNESS_PASS=$((HARNESS_PASS + 1))
    printf '  [PASS] %s\n' "${1:-}" >&2
}
_harness_fail() {
    HARNESS_FAIL=$((HARNESS_FAIL + 1))
    HARNESS_FAILURES+=("${1:-}")
    printf '  [FAIL] %s\n' "${1:-}" >&2
}

# assert_eq <actual> <expected> <label>
assert_eq() {
    local actual="$1" expected="$2" label="${3:-assert_eq}"
    e2e_log_assert_eq "$actual" "$expected" "$label"
    if [ "$actual" = "$expected" ]; then _harness_pass "$label (= $expected)";
    else _harness_fail "$label: expected [$expected] got [$actual]"; fi
}

# assert_contains <haystack> <needle> <label>
assert_contains() {
    local hay="$1" needle="$2" label="${3:-assert_contains}"
    if printf '%s' "$hay" | grep -qF -- "$needle"; then
        e2e_log_assert_eq "contains" "contains" "$label"; _harness_pass "$label (contains '$needle')";
    else
        e2e_log_assert_eq "missing" "contains" "$label"; _harness_fail "$label: '$needle' not found";
    fi
}

# assert_jq <json> <jq-bool-filter> <label> — passes when filter yields true.
assert_jq() {
    local json="$1" filter="$2" label="${3:-assert_jq}" result
    result="$(printf '%s' "$json" | jq -e "$filter" >/dev/null 2>&1 && echo true || echo false)"
    e2e_log_assert_eq "$result" "true" "$label"
    if [ "$result" = "true" ]; then _harness_pass "$label ($filter)";
    else _harness_fail "$label: jq filter false [$filter]"; fi
}

# assert_exit <expected_code> <label> -- <command...>
assert_exit() {
    local expected="$1" label="$2"; shift 2; [ "${1:-}" = "--" ] && shift
    local rc=0
    e2e_log_command "$@" || true
    "$@" >/dev/null 2>&1 || rc=$?
    e2e_log_assert_eq "$rc" "$expected" "$label"
    if [ "$rc" = "$expected" ]; then _harness_pass "$label (exit $rc)";
    else _harness_fail "$label: expected exit $expected got $rc"; fi
}

# log_drop <count> <reason> — the NO-SILENT-CAP rule. Call whenever a test
# observes the system truncating/sampling/capping/abstaining, so a green run
# can never mask partial coverage.
log_drop() {
    local count="${1:-?}" reason="${2:-unspecified}"
    HARNESS_DROPS=$((HARNESS_DROPS + 1))
    e2e_log_note "drop count=$count reason=$reason"
    printf '  [DROP] %s item(s): %s\n' "$count" "$reason" >&2
}

# with_temp_workspace <var> — assign an isolated workspace dir (own DB + index)
# to <var>. Pair with end_temp_workspace. Cleaned up unless EE_E2E_KEEP=1.
with_temp_workspace() {
    local __var="${1:?with_temp_workspace: variable name required}"
    local __root="${EE_E2E_TMPDIR:-${TMPDIR:-/tmp}}"
    local __ws; __ws="$(mktemp -d "${__root%/}/ee-wiz-${HARNESS_TEST_NAME}-XXXXXX")"
    mkdir -p "$__ws/db" "$__ws/index"
    export EE_DATABASE_PATH="$__ws/db/ee.db"
    export EE_INDEX_DIR="$__ws/index"
    HARNESS_TMP_WORKSPACES+=("$__ws")
    e2e_log_note "workspace_open path=$__ws db=$EE_DATABASE_PATH index=$EE_INDEX_DIR"
    printf -v "$__var" '%s' "$__ws"
}

end_temp_workspace() {
    unset EE_DATABASE_PATH EE_INDEX_DIR
    if [ "${EE_E2E_KEEP:-0}" != "1" ]; then
        local ws
        for ws in "${HARNESS_TMP_WORKSPACES[@]}"; do
            case "$ws" in /tmp/*|"${TMPDIR%/}"/*|"${EE_E2E_TMPDIR%/}"/*) rm -rf "$ws" 2>/dev/null || true;; esac
        done
        HARNESS_TMP_WORKSPACES=()
    fi
}

# harness_summary — emit summary.json + human summary; exit 0 only if no FAIL.
harness_summary() {
    local end_ns elapsed_ms
    end_ns="$(_harness_now_ns)"
    elapsed_ms="$(python3 -c "print(round(($end_ns-$HARNESS_START_NS)/1e6,3))")"
    e2e_log_end
    python3 - "$LOG_DIR/summary.json" "$HARNESS_TEST_NAME" "$HARNESS_PASS" "$HARNESS_FAIL" "$HARNESS_STEP" "$HARNESS_DROPS" "$elapsed_ms" "$EE_TEST_LOG_PATH" <<'PYEOF'
import json, sys
path, name, p, f, steps, drops, ms, events = sys.argv[1:9]
doc = {"schema":"ee.test_event.v1.summary","test":name,
       "steps":int(steps),"pass":int(p),"fail":int(f),"drops":int(drops),
       "elapsed_ms":float(ms),"events":events,
       "verdict":("PASS" if int(f)==0 else "FAIL")}
open(path,"w").write(json.dumps(doc,indent=2)+"\n")
print(json.dumps(doc))
PYEOF
    printf '[harness] %s: %d pass, %d fail, %d steps, %d drops, %sms -> %s\n' \
        "$HARNESS_TEST_NAME" "$HARNESS_PASS" "$HARNESS_FAIL" "$HARNESS_STEP" "$HARNESS_DROPS" "$elapsed_ms" \
        "$([ "$HARNESS_FAIL" -eq 0 ] && echo PASS || echo FAIL)" >&2
    if [ "$HARNESS_FAIL" -ne 0 ]; then
        printf '[harness] failures:\n' >&2
        local fmsg; for fmsg in "${HARNESS_FAILURES[@]}"; do printf '  - %s\n' "$fmsg" >&2; done
        return 1
    fi
    return 0
}
