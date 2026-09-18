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

# shellcheck source=scripts/lib/ee_binary_resolution.sh
# shellcheck disable=SC1091
source "$E2E_HARNESS_DIR/ee_binary_resolution.sh"

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
    if [ -n "${EE_BINARY:-}" ]; then printf '%s' "$EE_BINARY"; return 0; fi
    local target_dir
    target_dir="$(cd "$REPO_ROOT" && cargo metadata --locked --no-deps --format-version 1 2>/dev/null \
        | python3 -c 'import sys,json; print(json.load(sys.stdin).get("target_directory",""))' 2>/dev/null)"
    if [ -n "$target_dir" ] && [ -x "$target_dir/release/ee" ]; then
        printf '%s' "$target_dir/release/ee"; return 0
    fi
    if [ -n "$target_dir" ] && [ -x "$target_dir/debug/ee" ]; then
        printf '%s' "$target_dir/debug/ee"; return 0
    fi
    # REFUSE. This used to be a bare `printf 'ee'`, which resolved through
    # PATH to whatever ee happened to be installed. A suite that cannot find
    # the binary under test then reports the INSTALLED binary's behaviour as
    # its own result: e2e_field_report_suite.sh emitted five false
    # assert_fails against a stale 0.14.2 that way, and verify.sh gate
    # 6.12698a-j has to pin EE_BIN at all ten of its call sites to avoid
    # reproducing it. A pin at every caller is a workaround that only holds
    # while every future caller remembers; refusing here is the guarantee.
    printf 'e2e_harness: cannot locate the ee binary under test.\n' >&2
    printf '  Set EE_BIN or EE_BINARY, or build one: cargo build --locked --bin ee\n' >&2
    printf '  Refusing to fall back to PATH -- a suite that silently tests a\n' >&2
    printf '  different binary reports that binary'"'"'s failures as its own.\n' >&2
    return 1
}

_harness_now_ns() { python3 -c 'import time; print(time.time_ns())'; }

# harness_init <test_name>
harness_init() {
    HARNESS_TEST_NAME="${1:?harness_init: test_name required}"
    HARNESS_PASS=0; HARNESS_FAIL=0; HARNESS_STEP=0; HARNESS_DROPS=0; HARNESS_FAILURES=()
    # The resolver RETURNS non-zero rather than exiting: it runs inside a
    # command substitution, where `exit` would terminate only the subshell and
    # let this function carry on with an empty EE_BIN -- the same fail-open in
    # a new costume (bd-ry56h). The caller is where the refusal has to land.
    if ! EE_BIN="$(_harness_resolve_ee_bin)"; then
        exit 2
    fi
    # Resolving a path and testing `-x` proves the executable BIT, not the
    # executable FORMAT. Measured 2026-09-18: the shared Cargo target
    # directory held Linux x86-64 ELF `debug/ee` written by an RCH run, and
    # scripts/e2e_session_budget.sh reported 15 assert_fails against it whose
    # one real cause was `exec format error` behind exit 126. Fifteen false
    # product defects is a worse outcome than one honest refusal, and the
    # refusal belongs here for the same reason the PATH fallback's did: a
    # check every future caller has to remember is not a guarantee.
    if ! ee_binary_executes_here "$EE_BIN"; then
        printf 'e2e_harness: %s cannot execute on this host (%s/%s).\n' \
            "$EE_BIN" "$(uname -s)" "$(uname -m)" >&2
        printf 'e2e_harness:   file(1): %s\n' \
            "$(file -b "$EE_BIN" 2>/dev/null || printf 'unavailable')" >&2
        printf 'e2e_harness: refusing -- every assertion would fail for this one\n' >&2
        printf 'e2e_harness: reason. Build a native binary and pin EE_BIN to it.\n' >&2
        exit 2
    fi
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
#
# `jq -e` sets four distinct non-zero exits and this helper used to render all
# of them as one line:
#
#     result="$(... jq -e "$filter" >/dev/null 2>&1 && echo true || echo false)"
#     else _harness_fail "$label: jq filter false [$filter]"
#
# so exit 1 (the property is false), exit 3 (the filter does not compile),
# exit 4 (no output to test) and exit 5 (the output is not JSON) all printed
# "jq filter false", and `2>&1` threw away jq's own explanation. Only exit 1
# says anything about the product; the other three say the TEST or the COMMAND
# is broken, and reading them as product failures is how bd-kyeyw's two
# uncompilable filters looked like ee returning nothing. bd-82aq1.
#
# WHAT IS DELIBERATELY UNCHANGED, because 1,972 call sites depend on it:
#   - the PASS path, byte for byte;
#   - the exit-1 text, so the local copies of this pattern in
#     e2e_typed_fields_decide.sh and e2e_embedding_native.sh stay consistent;
#   - the return status, which is whatever the reporter returns (0) in every
#     case. This harness ACCUMULATES and lets harness_summary decide the exit
#     code; scripts/e2e_read_coalescing.sh runs under `set -euo pipefail` and
#     would abort mid-suite if a failed assertion started returning non-zero.
#   - every byte of output going to stderr through the reporters.
#     scripts/e2e_overhaul/workspace_hygiene.sh:1474 captures this function's
#     STDOUT into a variable, so anything printed there changes its meaning.
#   - the "true"/"false" pair handed to e2e_log_assert_eq, which the event log
#     records.
# Only the text of the three non-property failures is new.
assert_jq() {
    local json="$1" filter="$2" label="${3:-assert_jq}" jq_err rc
    # `2>&1 >/dev/null` captures stderr and discards stdout: jq's diagnostic is
    # the thing worth keeping, and the boolean is carried by the exit code.
    jq_err="$(printf '%s' "$json" | jq -e "$filter" 2>&1 >/dev/null)"
    rc=$?
    if [ "$rc" -eq 0 ]; then
        e2e_log_assert_eq "true" "true" "$label"
        _harness_pass "$label ($filter)"
        return
    fi
    e2e_log_assert_eq "false" "true" "$label"
    case "$rc" in
        1) _harness_fail "$label: jq filter false [$filter]" ;;
        3) _harness_fail "$label: HARNESS ERROR -- jq filter did not compile [$filter]: $(printf '%s' "$jq_err" | tr '\n' ' ' | cut -c1-300)" ;;
        4) _harness_fail "$label: HARNESS ERROR -- no output to test; the command produced nothing for [$filter]" ;;
        5) _harness_fail "$label: HARNESS ERROR -- input is not JSON for [$filter]: $(printf '%s' "$jq_err" | tr '\n' ' ' | cut -c1-300)" ;;
        *) _harness_fail "$label: HARNESS ERROR -- jq exited $rc for [$filter]: $(printf '%s' "$jq_err" | tr '\n' ' ' | cut -c1-300)" ;;
    esac
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
