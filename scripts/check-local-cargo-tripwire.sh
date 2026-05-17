#!/usr/bin/env bash
# bd-1h8ji.2 — Local Cargo tripwire / RCH hook-bypass detector.
#
# Classifies a candidate cargo invocation against the bd-1h8ji.1 verifier
# contract: direct `cargo build/check/test/bench/clippy` in this repo
# fails-closed unless wrapped through `rch exec -- ... cargo ...`. Also
# detects already-running local `cargo`/`rustc` processes that are
# writing into Mac-local USB target dirs without an RCH wrapper visible
# in their parent chain — the exact failure the bead body cites where
# a direct `cargo bench` with `RCH_REQUIRE_REMOTE=1` set still started
# local Darwin work.
#
# This is the READ-ONLY DETECTION half of bd-1h8ji.2. It never deletes,
# kills, or otherwise mutates state. The active PreToolUse hook that
# refuses to spawn the underlying process before it forks is the
# explicit follow-up child slice.
#
# Usage:
#   scripts/check-local-cargo-tripwire.sh --cmd '<command-line>' [--json]
#   scripts/check-local-cargo-tripwire.sh --probe-processes [--json]
#   scripts/check-local-cargo-tripwire.sh --self-test
#
# Exit codes: 0 = allowed/clean, 1 = bypass detected, 2 = usage error.

set -eu

REPORT_SCHEMA="ee.rch_local_cargo_tripwire.v1"
JSON_OUTPUT=false
SELF_TEST=false
MODE="cmd_classify"
CMD=""

usage() {
    sed -n '2,21p' "$0" | sed 's/^# \{0,1\}//'
}

while [ $# -gt 0 ]; do
    case "$1" in
        --json) JSON_OUTPUT=true; shift ;;
        --self-test) SELF_TEST=true; shift ;;
        --probe-processes) MODE="probe_processes"; shift ;;
        --cmd)
            shift
            if [ $# -eq 0 ]; then
                printf -- '--cmd requires a value\n' >&2
                usage >&2
                exit 2
            fi
            CMD="$1"
            shift
            ;;
        -h|--help) usage; exit 0 ;;
        --) shift; break ;;
        -*) printf 'unknown flag: %s\n' "$1" >&2; usage >&2; exit 2 ;;
        *) printf 'unexpected positional arg: %s\n' "$1" >&2; usage >&2; exit 2 ;;
    esac
done

# Cargo subcommands that trigger compilation. Plain `cargo metadata`,
# `cargo locate-project`, etc. are NOT compile commands and don't trip
# the wire.
FORBIDDEN_CARGO_SUBCOMMANDS="build check test bench clippy run install rustc fix"

# Repo-specific path tokens that indicate the cargo command is
# operating on this checkout (not some sibling crate). When detection
# walks live processes, we use these to bound false positives to the
# eidetic_engine_cli tree.
REPO_PATH_HINTS="eidetic_engine_cli /data/projects/eidetic_engine_cli /Users/jemanuel/projects/eidetic_engine_cli"

classify_command() {
    # Returns a single line "<allowed>\t<reason>\t<subcommand>\t<detail>"
    # where allowed is "allowed" or "denied". The detail field carries
    # a short example/anomaly fingerprint for human/JSON output.
    local cmd="$1"
    local subcommand=""
    local detail=""

    # Empty command can't be a tripwire violation.
    if [ -z "$cmd" ]; then
        printf 'allowed\tempty command\t-\t-\n'
        return
    fi

    # Whitelist: anything wrapped through `rch exec` is allowed. The
    # canonical shape is `... rch exec -- env ... cargo <sub> ...`; we
    # also accept `rch exec --json -- ...` and similar flag variants.
    if printf '%s' "$cmd" | grep -Eq '(^|[[:space:]/])rch([[:space:]]+--json)?[[:space:]]+exec([[:space:]]|--)'; then
        printf 'allowed\tcargo wrapped through rch exec\t-\t-\n'
        return
    fi

    # Detect the bare `cargo <forbidden-subcommand>` shape with no rch
    # prefix anywhere.
    for sub in $FORBIDDEN_CARGO_SUBCOMMANDS; do
        # Match "cargo <sub>" at start of line, after whitespace, or
        # after env-prefix tokens like `FOO=bar`, but NOT inside a
        # path-component such as "/usr/local/bin/cargo-test".
        if printf '%s' "$cmd" | grep -Eq "(^|[[:space:]]|^[A-Z_]+=[^[:space:]]+([[:space:]]+[A-Z_]+=[^[:space:]]+)*[[:space:]]+)cargo[[:space:]]+${sub}([[:space:]]|$)"; then
            subcommand="$sub"
            detail="cargo $sub invocation has no rch exec wrapper in the command string"
            break
        fi
    done

    if [ -z "$subcommand" ]; then
        printf 'allowed\tnot a forbidden cargo compilation subcommand\t-\t-\n'
        return
    fi

    # Bonus diagnostic: the bead body specifically cites the failure
    # where the caller set RCH_REQUIRE_REMOTE=1 but did NOT prefix with
    # `rch exec`. Surface that case with a more specific detail line.
    if printf '%s' "$cmd" | grep -Eq 'RCH_REQUIRE_REMOTE[[:space:]]*=[[:space:]]*1'; then
        detail="$detail (RCH_REQUIRE_REMOTE=1 was set but rch exec wrapper is absent — exact bd-1h8ji.2 failure mode)"
    fi

    printf 'denied\tdirect cargo %s invocation bypasses the RCH wrapper\t%s\t%s\n' \
        "$subcommand" "$subcommand" "$detail"
}

probe_processes() {
    # Scan running cargo/rustc processes for ones that target this
    # repo's worktree paths without an `rch exec` ancestor visible in
    # their command lines. We rely on ps -eo command rather than the
    # process tree because ps -eo ppid is racy on macOS during fork.
    #
    # Output rows: <pid>\t<short-command>\t<flagged-reason>
    local ps_output
    ps_output=$(ps -eo pid=,command= 2>/dev/null || true)
    if [ -z "$ps_output" ]; then
        return 0
    fi
    # `ps` on macOS prints PID with leading spaces; normalize.
    printf '%s\n' "$ps_output" | while IFS= read -r line; do
        local pid
        local cmd
        pid=$(printf '%s' "$line" | awk '{print $1}')
        cmd=$(printf '%s' "$line" | sed -E 's/^[[:space:]]*[0-9]+[[:space:]]+//')
        [ -n "$pid" ] || continue
        [ -n "$cmd" ] || continue
        # Skip lines that are not cargo/rustc invocations.
        case "$cmd" in
            *cargo*|*rustc*) ;;
            *) continue ;;
        esac
        # Skip our own shell + the ps invocation above.
        case "$cmd" in
            *check-local-cargo-tripwire*|*ps[[:space:]]-eo*) continue ;;
        esac
        # Only flag processes operating on this repo.
        local matches_repo=false
        for hint in $REPO_PATH_HINTS; do
            case "$cmd" in
                *"$hint"*) matches_repo=true; break ;;
            esac
        done
        [ "$matches_repo" = true ] || continue
        # Skip if rch exec appears anywhere in the command (this is the
        # remote-execution local launcher process).
        if printf '%s' "$cmd" | grep -Eq '(^|[[:space:]/])rch[[:space:]]+exec'; then
            continue
        fi
        printf '%s\t%s\tlocal cargo/rustc process targeting this repo without rch exec\n' \
            "$pid" "$(printf '%s' "$cmd" | cut -c1-200)"
    done
}

emit_human_cmd() {
    local allowed="$1"
    local reason="$2"
    local subcommand="$3"
    local detail="$4"
    if [ "$allowed" = "allowed" ]; then
        printf '[rch tripwire] allowed: %s\n' "$reason"
        return 0
    fi
    printf '[rch tripwire] DENIED: %s\n' "$reason"
    if [ "$detail" != "-" ] && [ -n "$detail" ]; then
        printf '  detail: %s\n' "$detail"
    fi
    printf '  fix: prefix with `/Users/jemanuel/projects/remote_compilation_helper/target-local/release/rch exec -- env TMPDIR=/tmp <cargo command>`\n'
}

emit_json_cmd() {
    local allowed="$1"
    local reason="$2"
    local subcommand="$3"
    local detail="$4"
    if command -v jq >/dev/null 2>&1; then
        jq -cn \
            --arg schema "$REPORT_SCHEMA" \
            --arg mode "cmd_classify" \
            --arg allowed "$allowed" \
            --arg reason "$reason" \
            --arg subcommand "$subcommand" \
            --arg detail "$detail" \
            '{schema:$schema,mode:$mode,allowed:$allowed,reason:$reason,subcommand:$subcommand,detail:$detail}'
    else
        printf '{"schema":"%s","mode":"cmd_classify","allowed":"%s","reason":"%s","subcommand":"%s","detail":"%s"}\n' \
            "$REPORT_SCHEMA" "$allowed" "$reason" "$subcommand" "$detail"
    fi
}

emit_human_probe() {
    local body="$1"
    local count="$2"
    if [ "$count" -eq 0 ]; then
        printf '[rch tripwire] clean: no local cargo/rustc processes targeting this repo without rch exec.\n'
        return 0
    fi
    printf '[rch tripwire] %d local cargo/rustc process(es) running without rch exec wrapper:\n' "$count"
    printf '%s' "$body" | while IFS=$(printf '\t') read -r pid short_cmd reason; do
        [ -n "$pid" ] || continue
        printf '  - pid=%s reason=%s\n      command: %s\n' "$pid" "$reason" "$short_cmd"
    done
    printf '  suggestion: investigate the offending shell; never automatically kill processes here.\n'
}

emit_json_probe() {
    local body="$1"
    local count="$2"
    local processes_json="[]"
    if [ -n "$body" ] && command -v jq >/dev/null 2>&1; then
        # Use BEGIN{FS="\t"} so the field separator is portable across
        # dash (POSIX sh on Linux RCH workers) and bash — the `$'\t'`
        # ANSI-C escape was bash-only and silently misparsed under dash.
        processes_json=$(printf '%s' "$body" |
            awk 'BEGIN{FS="\t"} NF>=3 {
                gsub(/"/, "\\\"", $2); gsub(/"/, "\\\"", $3)
                printf "{\"pid\":\"%s\",\"command\":\"%s\",\"reason\":\"%s\"}\n", $1, $2, $3
            }' |
            jq -s '.')
    fi
    local status="ok"
    if [ "$count" -gt 0 ]; then status="bypass_detected"; fi
    if command -v jq >/dev/null 2>&1; then
        jq -cn \
            --arg schema "$REPORT_SCHEMA" \
            --arg mode "probe_processes" \
            --arg status "$status" \
            --argjson count "$count" \
            --argjson processes "$processes_json" \
            '{schema:$schema,mode:$mode,status:$status,count:$count,processes:$processes}'
    else
        printf '{"schema":"%s","mode":"probe_processes","status":"%s","count":%d,"processes":[]}\n' \
            "$REPORT_SCHEMA" "$status" "$count"
    fi
}

run_self_test() {
    # Direct cargo test → DENIED.
    local result
    result=$(classify_command "cargo test --lib happy_path")
    case "$result" in
        denied*) ;;
        *) printf 'self-test FAILED: direct cargo test must be denied; got %s\n' "$result" >&2; exit 1 ;;
    esac
    # Direct cargo build with env prefix → DENIED.
    result=$(classify_command "RCH_REQUIRE_REMOTE=1 cargo build --release")
    case "$result" in
        denied*) ;;
        *) printf 'self-test FAILED: env-prefixed cargo build must be denied; got %s\n' "$result" >&2; exit 1 ;;
    esac
    # Wrapped through rch exec → ALLOWED.
    result=$(classify_command "rch exec -- env TMPDIR=/tmp cargo test --lib foo")
    case "$result" in
        allowed*) ;;
        *) printf 'self-test FAILED: rch exec wrapper must be allowed; got %s\n' "$result" >&2; exit 1 ;;
    esac
    # cargo metadata is not a compile subcommand → ALLOWED.
    result=$(classify_command "cargo metadata --format-version 1")
    case "$result" in
        allowed*) ;;
        *) printf 'self-test FAILED: cargo metadata must be allowed; got %s\n' "$result" >&2; exit 1 ;;
    esac
    # Absolute path wrapped rch exec → ALLOWED.
    result=$(classify_command "/Users/jemanuel/projects/remote_compilation_helper/target-local/release/rch exec -- env TMPDIR=/tmp cargo bench --bench foo")
    case "$result" in
        allowed*) ;;
        *) printf 'self-test FAILED: absolute-path rch exec must be allowed; got %s\n' "$result" >&2; exit 1 ;;
    esac
    # Empty command → ALLOWED.
    result=$(classify_command "")
    case "$result" in
        allowed*) ;;
        *) printf 'self-test FAILED: empty command must be allowed; got %s\n' "$result" >&2; exit 1 ;;
    esac
    printf 'self-test PASSED: 6 classifier cases produced expected outcomes\n'
    exit 0
}

if [ "$SELF_TEST" = true ]; then
    run_self_test
fi

case "$MODE" in
    cmd_classify)
        # An explicit `--cmd ""` is treated as a classifier query for the
        # empty command and returns allowed (the classifier already handles
        # empty input). Only complain when --cmd was never passed at all,
        # which is detectable here only via $MODE staying at the default
        # AND no positional fallback being supplied. For practical use,
        # the harness always passes --cmd, so allow the empty-string path
        # to flow through classify_command rather than hard-fail.
        RESULT=$(classify_command "$CMD")
        ALLOWED=$(printf '%s' "$RESULT" | awk -F'\t' '{print $1}')
        REASON=$(printf '%s' "$RESULT" | awk -F'\t' '{print $2}')
        SUBCOMMAND=$(printf '%s' "$RESULT" | awk -F'\t' '{print $3}')
        DETAIL=$(printf '%s' "$RESULT" | awk -F'\t' '{print $4}')
        if [ "$JSON_OUTPUT" = true ]; then
            emit_json_cmd "$ALLOWED" "$REASON" "$SUBCOMMAND" "$DETAIL"
        else
            emit_human_cmd "$ALLOWED" "$REASON" "$SUBCOMMAND" "$DETAIL"
        fi
        if [ "$ALLOWED" = "denied" ]; then exit 1; fi
        exit 0
        ;;
    probe_processes)
        BODY=$(probe_processes || true)
        if [ -n "$BODY" ]; then
            COUNT=$(printf '%s' "$BODY" | grep -c . || true)
        else
            COUNT=0
        fi
        if [ "$JSON_OUTPUT" = true ]; then
            emit_json_probe "$BODY" "$COUNT"
        else
            emit_human_probe "$BODY" "$COUNT"
        fi
        if [ "$COUNT" -gt 0 ]; then exit 1; fi
        exit 0
        ;;
esac
