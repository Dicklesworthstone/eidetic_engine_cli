#!/usr/bin/env bash
# RCHVC.1 - stable remote verification wrapper for focused Rust checks.
#
# This script is intentionally repo-local. It makes the explicit RCH path the
# easy path for agents and emits a JSON proof that can be pasted into Beads.

set -euo pipefail

usage() {
    cat <<'EOF'
Usage: scripts/rch_verify.sh [options] -- <verifier command...>

Options:
  --dry-run                 Do not execute; emit the planned explicit rch exec proof
  --allow-raw               Allow non-Cargo commands; still runs through rch exec
  --rch-bin <path>          RCH binary (default: $RCH_BIN or rch)
  --project-root <path>     Local project root (default: cwd)
  --json                    Accepted for symmetry; output is always JSON
  -h, --help                Show this help

Accepted Cargo verifier shapes:
  cargo check ...
  cargo test ...
  cargo bench ...
  cargo clippy ...
  cargo fmt --check ...
EOF
}

DRY_RUN=0
ALLOW_RAW=0
DEFAULT_RCH_BIN="/Users/jemanuel/projects/remote_compilation_helper/target-local/release/rch"
if [ -z "${RCH_BIN:-}" ] && [ -x "$DEFAULT_RCH_BIN" ]; then
    RCH_BIN="$DEFAULT_RCH_BIN"
elif [ -z "${RCH_BIN:-}" ]; then
    RCH_BIN="rch"
fi
PROJECT_ROOT="$PWD"

while [ "$#" -gt 0 ]; do
    case "$1" in
        --dry-run) DRY_RUN=1; shift ;;
        --allow-raw) ALLOW_RAW=1; shift ;;
        --rch-bin) RCH_BIN="${2:?--rch-bin requires a value}"; shift 2 ;;
        --project-root) PROJECT_ROOT="${2:?--project-root requires a value}"; shift 2 ;;
        --json) shift ;;
        -h|--help) usage; exit 0 ;;
        --) shift; break ;;
        *)
            echo "rch_verify: unknown argument: $1" >&2
            usage >&2
            exit 2
            ;;
    esac
done

if [ "$#" -eq 0 ]; then
    echo "rch_verify: verifier command is required after --" >&2
    usage >&2
    exit 2
fi

COMMAND=("$@")

command_string() {
    local out="" arg
    for arg in "$@"; do
        if [ -z "$out" ]; then
            out="$arg"
        else
            out="$out $arg"
        fi
    done
    printf '%s' "$out"
}

contains_forbidden_text() {
    local text
    text="$(command_string "$@")"
    case "$text" in
        *"rm -rf"*|*"rm -f"*|*"git reset"*|*"git clean"*|*"git checkout"*|*"git stash"*|*"mkfs"*|*" dd "*|*"drop database"*)
            return 0
            ;;
        *)
            return 1
            ;;
    esac
}

classify_command() {
    if [ "${COMMAND[0]}" != "cargo" ]; then
        if [ "$ALLOW_RAW" -eq 1 ]; then
            printf 'raw'
            return 0
        fi
        printf 'rejected'
        return 0
    fi

    local subcommand="${COMMAND[1]:-}"
    case "$subcommand" in
        check) printf 'cargo_check' ;;
        test) printf 'cargo_test' ;;
        bench) printf 'cargo_bench' ;;
        clippy) printf 'cargo_clippy' ;;
        fmt)
            local arg
            for arg in "${COMMAND[@]}"; do
                if [ "$arg" = "--check" ]; then
                    printf 'cargo_fmt_check'
                    return 0
                fi
            done
            printf 'rejected'
            ;;
        *) printf 'rejected' ;;
    esac
}

json_array() {
    python3 -c 'import json, sys; print(json.dumps(sys.argv[1:], separators=(",", ":")))' "$@"
}

json_quote() {
    python3 -c 'import json, sys; print(json.dumps(sys.argv[1]))' "$1"
}

tail_text() {
    python3 -c 'import sys; text=sys.stdin.read(); print(text[-4000:])'
}

now_iso() {
    if [ -n "${RCH_VERIFY_NOW:-}" ]; then
        printf '%s' "$RCH_VERIFY_NOW"
    else
        python3 -c 'from datetime import datetime, timezone; print(datetime.now(timezone.utc).isoformat(timespec="microseconds").replace("+00:00","Z"))'
    fi
}

now_ms() {
    python3 -c 'import time; print(int(time.time() * 1000))'
}

emit_json() {
    local success="$1"
    local exit_code_json="$2"
    local elapsed_ms="$3"
    local stdout_tail="$4"
    local stderr_tail="$5"
    shift 5
    local degraded_codes_json
    degraded_codes_json="$(json_array "$@")"
    local command_json rch_invocation_json command_text_json stdout_json stderr_json
    command_json="$(json_array "${COMMAND[@]}")"
    rch_invocation_json="$(json_array "${RCH_INVOCATION[@]}")"
    command_text_json="$(json_quote "$(command_string "${COMMAND[@]}")")"
    stdout_json="$(json_quote "$stdout_tail")"
    stderr_json="$(json_quote "$stderr_tail")"
    cat <<EOF
{"schema":"ee.rch.verify.v1","success":$success,"generated_at":"$(now_iso)","command":$command_json,"command_text":$command_text_json,"command_kind":"$COMMAND_KIND","remote_required":true,"would_offload":$WOULD_OFFLOAD,"worker_id":$WORKER_ID_JSON,"remote_project_root":$REMOTE_PROJECT_ROOT_JSON,"remote_target_dir":$REMOTE_TARGET_DIR_JSON,"exit_code":$exit_code_json,"elapsed_ms":$elapsed_ms,"stdout_tail":$stdout_json,"stderr_tail":$stderr_json,"degraded_codes":$degraded_codes_json,"rch_invocation":$rch_invocation_json}
EOF
}

COMMAND_KIND="$(classify_command)"
WOULD_OFFLOAD=false
WORKER_ID_JSON=null
REMOTE_PROJECT_ROOT="/data/projects/eidetic_engine_cli"
REMOTE_TARGET_DIR="/tmp/ee-rch-verify-target"
REMOTE_PROJECT_ROOT_JSON="$(json_quote "$REMOTE_PROJECT_ROOT")"
REMOTE_TARGET_DIR_JSON="$(json_quote "$REMOTE_TARGET_DIR")"

if contains_forbidden_text "${COMMAND[@]}"; then
    RCH_INVOCATION=()
    emit_json false null 0 "" "refused forbidden command text" "rch_verify_refused_forbidden_command"
    exit 2
fi

if [ "$COMMAND_KIND" = "rejected" ]; then
    RCH_INVOCATION=()
    emit_json false null 0 "" "unsupported verification command; pass --allow-raw for an explicitly raw remote command" "rch_verify_refused_unknown_command"
    exit 2
fi

if [ "$COMMAND_KIND" = "raw" ]; then
    WOULD_OFFLOAD=false
else
    WOULD_OFFLOAD=true
fi
RCH_INVOCATION=(
    "$RCH_BIN" "exec" "--"
    "env" "TMPDIR=/tmp" "CARGO_TARGET_DIR=$REMOTE_TARGET_DIR"
    "${COMMAND[@]}"
)

if [ "$DRY_RUN" -eq 1 ]; then
    dry_run_degraded=("rch_verify_dry_run")
    if [ "$COMMAND_KIND" = "raw" ]; then
        dry_run_degraded+=("rch_verify_raw_command_may_not_offload")
    fi
    emit_json true null 0 "dry run: explicit rch exec planned" "" "${dry_run_degraded[@]}"
    exit 0
fi

start_ms="$(now_ms)"
set +e
combined_output="$(
    cd "$PROJECT_ROOT" && \
    RCH_COMPRESSION="${RCH_COMPRESSION:-0}" \
    RCH_REQUIRE_REMOTE=1 \
    RCH_QUEUE_WHEN_BUSY="${RCH_QUEUE_WHEN_BUSY:-1}" \
    RCH_TEST_SLOTS="${RCH_TEST_SLOTS:-2}" \
    RCH_DAEMON_WAIT_RESPONSE_TIMEOUT_SECS="${RCH_DAEMON_WAIT_RESPONSE_TIMEOUT_SECS:-900}" \
    RCH_DAEMON_RESPONSE_TIMEOUT_SECS="${RCH_DAEMON_RESPONSE_TIMEOUT_SECS:-900}" \
    RCH_CANONICAL_PROJECT_ROOT="${RCH_CANONICAL_PROJECT_ROOT:-/Users/jemanuel/projects}" \
    RCH_ALIAS_PROJECT_ROOT="${RCH_ALIAS_PROJECT_ROOT:-/data/projects}" \
    RCH_VISIBILITY="${RCH_VISIBILITY:-summary}" \
    "${RCH_INVOCATION[@]}" 2>&1
)"
exit_code=$?
set -e
end_ms="$(now_ms)"
elapsed_ms=$((end_ms - start_ms))

worker_id="$(
    printf '%s' "$combined_output" \
        | sed -n 's/.*\[RCH\] remote \([^ ]*\).*/\1/p' \
        | tail -n 1
)"
if [ -n "$worker_id" ]; then
    WORKER_ID_JSON="$(json_quote "$worker_id")"
fi

stdout_tail="$(printf '%s' "$combined_output" | tail_text)"
degraded=()
if [ "$exit_code" -ne 0 ]; then
    degraded+=("rch_verify_remote_command_failed")
fi
if [ "$COMMAND_KIND" = "raw" ]; then
    degraded+=("rch_verify_raw_command_may_not_offload")
fi

emit_json true "$exit_code" "$elapsed_ms" "$stdout_tail" "" "${degraded[@]}"
exit "$exit_code"
