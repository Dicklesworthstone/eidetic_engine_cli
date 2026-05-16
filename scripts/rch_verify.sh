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
  --bead-id <id>            Optional bead id for ledger rows and summaries
  --ledger <path>           Append one derived JSONL evidence row
  --summary                 Include bead-ready Markdown summary in the JSON proof
  --no-write                Do not write --ledger; render proof/summary only
  --rch-bin <path>          RCH binary (default: $RCH_BIN or rch)
  --project-root <path>     Local project root (default: cwd)
  --env <NAME=VALUE>        Pass an explicit environment override to the remote verifier command
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
BEAD_ID=""
LEDGER_PATH=""
INCLUDE_SUMMARY=0
NO_WRITE=0
ENV_OVERRIDES=()
DEFAULT_RCH_BIN="/Users/jemanuel/projects/remote_compilation_helper/target-local/release/rch"
if [ -z "${RCH_BIN:-}" ] && [ -x "$DEFAULT_RCH_BIN" ]; then
    RCH_BIN="$DEFAULT_RCH_BIN"
elif [ -z "${RCH_BIN:-}" ]; then
    RCH_BIN="rch"
fi
PROJECT_ROOT="$PWD"

validate_env_override() {
    local item="${1:?environment override required}"
    local name="${item%%=*}"
    if [ "$item" = "$name" ] || [ -z "$name" ]; then
        echo "rch_verify: --env requires NAME=VALUE, got: $item" >&2
        exit 2
    fi
    case "$name" in
        [A-Za-z_]*)
            case "$name" in
                *[!A-Za-z0-9_]*)
                    echo "rch_verify: invalid --env name: $name" >&2
                    exit 2
                    ;;
            esac
            ;;
        *)
            echo "rch_verify: invalid --env name: $name" >&2
            exit 2
            ;;
    esac
    printf '%s' "$item"
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --dry-run) DRY_RUN=1; shift ;;
        --allow-raw) ALLOW_RAW=1; shift ;;
        --bead-id) BEAD_ID="${2:?--bead-id requires a value}"; shift 2 ;;
        --ledger) LEDGER_PATH="${2:?--ledger requires a value}"; shift 2 ;;
        --summary) INCLUDE_SUMMARY=1; shift ;;
        --no-write) NO_WRITE=1; shift ;;
        --rch-bin) RCH_BIN="${2:?--rch-bin requires a value}"; shift 2 ;;
        --project-root) PROJECT_ROOT="${2:?--project-root requires a value}"; shift 2 ;;
        --env) ENV_OVERRIDES+=("$(validate_env_override "${2:?--env requires NAME=VALUE}")"); shift 2 ;;
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

extract_worker_id() {
    sed -n \
        -e 's/^.*Selected worker: \([A-Za-z0-9_.-][A-Za-z0-9_.-]*\) .*/\1/p' \
        -e 's/^\[RCH\] remote \([A-Za-z0-9_.-][A-Za-z0-9_.-]*\) (.*/\1/p' \
        -e 's/^\[RCH\] remote \([A-Za-z0-9_.-][A-Za-z0-9_.-]*\) failed.*/\1/p' \
        | tail -n 1
}

is_worker_disk_full_output() {
    grep -Eiq "No space left on device|disk full|ENOSPC"
}

healthy_alternate_workers() {
    local failed_worker="${1:?failed worker required}"
    HEALTHY_WORKERS="${RCH_VERIFY_HEALTHY_WORKERS:-}" \
    FAILED_WORKER="$failed_worker" \
    RCH_BIN_PATH="$RCH_BIN" \
    python3 - <<'PY'
import json
import os
import subprocess

failed = os.environ["FAILED_WORKER"]
explicit = os.environ.get("HEALTHY_WORKERS", "")

def emit(ids):
    seen = []
    for item in ids:
        item = item.strip()
        if item and item != failed and item not in seen:
            seen.append(item)
    print(",".join(seen))

if explicit:
    emit(explicit.split(","))
    raise SystemExit(0)

rch_bin = os.environ["RCH_BIN_PATH"]
try:
    status = subprocess.run(
        [rch_bin, "status", "--workers", "--jobs", "--json"],
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        text=True,
        timeout=10,
    )
    payload = json.loads(status.stdout)
    workers = payload.get("data", {}).get("daemon", {}).get("workers", [])
    healthy = [
        worker.get("id", "")
        for worker in workers
        if worker.get("status") == "healthy"
    ]
    if healthy:
        emit(healthy)
        raise SystemExit(0)
except Exception:
    pass

try:
    listed = subprocess.run(
        [rch_bin, "workers", "list", "--json"],
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        text=True,
        timeout=10,
    )
    payload = json.loads(listed.stdout)
    workers = payload.get("data", {}).get("workers", [])
    emit(worker.get("id", "") for worker in workers)
except Exception:
    print("")
PY
}

run_rch_invocation_once() {
    if [ -n "${RCH_VERIFY_FAKE_OUTPUT:-}" ]; then
        printf '%s' "$RCH_VERIFY_FAKE_OUTPUT"
        return "${RCH_VERIFY_FAKE_EXIT_CODE:-0}"
    fi

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
}

run_rch_invocation_retry() {
    local preferred_workers="${1:?preferred workers required}"
    if [ -n "${RCH_VERIFY_FAKE_RETRY_OUTPUT:-}" ]; then
        printf '%s' "$RCH_VERIFY_FAKE_RETRY_OUTPUT"
        return "${RCH_VERIFY_FAKE_RETRY_EXIT_CODE:-0}"
    fi

    cd "$PROJECT_ROOT" && \
        RCH_WORKERS="$preferred_workers" \
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

RUN_STARTED_AT="$(now_iso)"

emit_json() {
    local success="$1"
    local exit_code_json="$2"
    local elapsed_ms="$3"
    local stdout_tail="$4"
    local stderr_tail="$5"
    shift 5
    local degraded_codes_json
    degraded_codes_json="$(json_array "$@")"
    local command_json rch_invocation_json command_text_json remote_env_json stdout_json stderr_json
    command_json="$(json_array "${COMMAND[@]}")"
    rch_invocation_json="$(json_array "${RCH_INVOCATION[@]}")"
    remote_env_json="$(json_array "${ENV_OVERRIDES[@]}")"
    command_text_json="$(json_quote "$(command_string "${ENV_OVERRIDES[@]}" "${COMMAND[@]}")")"
    stdout_json="$(json_quote "$stdout_tail")"
    stderr_json="$(json_quote "$stderr_tail")"
    local json_payload
    json_payload="$(cat <<EOF
{"schema":"ee.rch.verify.v1","success":$success,"generated_at":"$(now_iso)","command":$command_json,"command_text":$command_text_json,"command_kind":"$COMMAND_KIND","remote_env":$remote_env_json,"remote_required":true,"would_offload":$WOULD_OFFLOAD,"worker_id":$WORKER_ID_JSON,"remote_project_root":$REMOTE_PROJECT_ROOT_JSON,"remote_target_dir":$REMOTE_TARGET_DIR_JSON,"exit_code":$exit_code_json,"elapsed_ms":$elapsed_ms,"stdout_tail":$stdout_json,"stderr_tail":$stderr_json,"degraded_codes":$degraded_codes_json,"rch_invocation":$rch_invocation_json}
EOF
)"
    JSON_PAYLOAD="$json_payload" \
    BEAD_ID="$BEAD_ID" \
    LEDGER_PATH="$LEDGER_PATH" \
    INCLUDE_SUMMARY="$INCLUDE_SUMMARY" \
    NO_WRITE="$NO_WRITE" \
    RUN_STARTED_AT="$RUN_STARTED_AT" \
    python3 - <<'PY'
import hashlib
import json
import os
import re
from pathlib import Path

proof = json.loads(os.environ["JSON_PAYLOAD"])
bead_id = os.environ.get("BEAD_ID", "")
ledger_path = os.environ.get("LEDGER_PATH", "")
include_summary = os.environ.get("INCLUDE_SUMMARY") == "1"
no_write = os.environ.get("NO_WRITE") == "1"
started_at = os.environ.get("RUN_STARTED_AT") or proof.get("generated_at")

def redact(text):
    if not text:
        return text
    text = re.sub(r"/Users/[^/\s]+", "/Users/<redacted>", text)
    text = re.sub(r"(?i)(token|secret|password|api[_-]?key)=\S+", r"\1=<redacted>", text)
    return text

def first_error_location(text):
    if not text:
        return (None, None)
    for line in text.splitlines():
        match = re.search(r"-->\s+([^:\s][^:]*):(\d+):\d+", line)
        if match:
            return (redact(match.group(1)), int(match.group(2)))
    return (None, None)

def error_codes(text):
    if not text:
        return []
    return sorted(set(re.findall(r"\bE\d{4}\b|RCH-E\d{3}\b", text)))

raw_stdout_tail = proof.get("stdout_tail") or ""
raw_stderr_tail = proof.get("stderr_tail") or ""
combined_tail = "\n".join(part for part in [raw_stdout_tail, raw_stderr_tail] if part)
proof["stdout_tail"] = redact(raw_stdout_tail)
proof["stderr_tail"] = redact(raw_stderr_tail)
first_error_file, first_error_line = first_error_location(combined_tail)
codes = error_codes(combined_tail)

exit_code = proof.get("exit_code")
degraded = list(proof.get("degraded_codes") or [])
if proof.get("success") is not True:
    status = "refused"
elif exit_code is None:
    status = "dry_run"
elif exit_code == 0 and proof.get("worker_id"):
    status = "remote_pass"
elif exit_code == 0:
    status = "pass_without_remote_marker"
elif (
    "rch_verify_topology_blocked" in degraded
    or "rch_verify_local_fallback_refused" in degraded
    or "rch_verify_worker_disk_full" in degraded
    or "rch_verify_worker_quarantine_ignored" in degraded
):
    status = "rch_environment_failure"
elif "rch_verify_capacity_or_timeout" in degraded:
    status = "capacity_or_timeout"
else:
    status = "remote_failure"

command_text = proof.get("command_text", "")
command_hash = hashlib.sha256(command_text.encode("utf-8")).hexdigest()
proof["status"] = status
proof["command_hash"] = command_hash
proof["started_at"] = started_at
proof["completed_at"] = proof.get("generated_at")
proof["first_error_file"] = first_error_file
proof["first_error_line"] = first_error_line
proof["error_codes"] = codes
if bead_id:
    proof["bead_id"] = bead_id

summary_lines = [
    f"RCH verifier `{command_text}` => `{status}`.",
    f"- command_kind: `{proof.get('command_kind')}`",
    f"- remote_env: `{', '.join(proof.get('remote_env') or []) or 'none'}`",
    f"- remote_required: `{str(proof.get('remote_required')).lower()}`",
    f"- would_offload: `{str(proof.get('would_offload')).lower()}`",
    f"- worker_id: `{proof.get('worker_id') or 'unknown'}`",
    f"- exit_code: `{exit_code if exit_code is not None else 'not_run'}`",
    f"- elapsed_ms: `{proof.get('elapsed_ms')}`",
    f"- command_hash: `{command_hash}`",
]
if bead_id:
    summary_lines.insert(1, f"- bead_id: `{bead_id}`")
if first_error_file:
    summary_lines.append(f"- first_error: `{first_error_file}:{first_error_line}`")
if codes:
    summary_lines.append("- error_codes: `" + "`, `".join(codes) + "`")
if degraded:
    summary_lines.append("- degraded_codes: `" + "`, `".join(degraded) + "`")
else:
    summary_lines.append("- degraded_codes: none")
summary = "\n".join(summary_lines)

if include_summary:
    proof["summary_markdown"] = summary

if ledger_path:
    proof["ledger_path"] = ledger_path
    if no_write:
        proof.setdefault("degraded_codes", []).append("rch_verify_ledger_write_suppressed")
    else:
        row = {
            "schema": "ee.rch.verify.ledger.v1",
            "verifier_id": proof.get("generated_at"),
            "bead_id": bead_id or None,
            "command": proof.get("command"),
            "command_text": proof.get("command_text"),
            "command_hash": command_hash,
            "command_kind": proof.get("command_kind"),
            "remote_env": proof.get("remote_env") or [],
            "started_at": started_at,
            "completed_at": proof.get("generated_at"),
            "elapsed_ms": proof.get("elapsed_ms"),
            "worker_id": proof.get("worker_id"),
            "remote_project_root": proof.get("remote_project_root"),
            "remote_target_dir": proof.get("remote_target_dir"),
            "rch_location": "explicit_rch_exec",
            "exit_code": proof.get("exit_code"),
            "status": status,
            "first_error_file": first_error_file,
            "first_error_line": first_error_line,
            "stdout_tail": proof.get("stdout_tail"),
            "stderr_tail": proof.get("stderr_tail"),
            "transcript_path": None,
            "degraded_codes": proof.get("degraded_codes") or [],
            "error_codes": codes,
            "summary_markdown": summary,
        }
        path = Path(ledger_path)
        path.parent.mkdir(parents=True, exist_ok=True)
        with path.open("a", encoding="utf-8") as handle:
            handle.write(json.dumps(row, sort_keys=True, separators=(",", ":")) + "\n")

print(json.dumps(proof, sort_keys=True, separators=(",", ":")))
PY
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

if [ "$COMMAND_KIND" = "raw" ] || [ "$COMMAND_KIND" = "cargo_fmt_check" ]; then
    WOULD_OFFLOAD=false
else
    WOULD_OFFLOAD=true
fi
RCH_INVOCATION=(
    "$RCH_BIN" "exec" "--"
    "env" "TMPDIR=/tmp" "CARGO_TARGET_DIR=$REMOTE_TARGET_DIR"
    "${ENV_OVERRIDES[@]}"
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
combined_output="$(run_rch_invocation_once)"
exit_code=$?
set -e
end_ms="$(now_ms)"
elapsed_ms=$((end_ms - start_ms))
if [ -n "${RCH_VERIFY_FAKE_ELAPSED_MS:-}" ]; then
    elapsed_ms="${RCH_VERIFY_FAKE_ELAPSED_MS}"
fi

worker_id="$(printf '%s' "$combined_output" | extract_worker_id)"
disk_full_worker=""
retried_after_disk_full=0
retry_worker=""
if [ "$exit_code" -ne 0 ] \
    && printf '%s' "$combined_output" | is_worker_disk_full_output \
    && [ -n "$worker_id" ] \
    && [ "${RCH_VERIFY_DISABLE_DISK_FULL_RETRY:-0}" != "1" ]; then
    disk_full_worker="$worker_id"
    alternate_workers="$(healthy_alternate_workers "$disk_full_worker")"
    if [ -n "$alternate_workers" ]; then
        retried_after_disk_full=1
        retry_note="[RCH_VERIFY] worker $disk_full_worker hit disk-full transfer failure; retrying once with RCH_WORKERS=$alternate_workers"
        start_retry_ms="$(now_ms)"
        set +e
        retry_output="$(run_rch_invocation_retry "$alternate_workers")"
        retry_exit_code=$?
        set -e
        end_retry_ms="$(now_ms)"
        elapsed_ms=$((elapsed_ms + end_retry_ms - start_retry_ms))
        combined_output="${combined_output}
${retry_note}
${retry_output}"
        exit_code="$retry_exit_code"
        retry_worker="$(printf '%s' "$retry_output" | extract_worker_id)"
        if [ -n "$retry_worker" ]; then
            worker_id="$retry_worker"
        fi
    fi
fi
if [ -n "$worker_id" ]; then
    WORKER_ID_JSON="$(json_quote "$worker_id")"
fi

stdout_tail="$(printf '%s' "$combined_output" | tail_text)"
degraded=()
if [ "$exit_code" -ne 0 ]; then
    degraded+=("rch_verify_remote_command_failed")
fi
if [ -n "$disk_full_worker" ] || printf '%s' "$combined_output" | is_worker_disk_full_output; then
    degraded+=("rch_verify_worker_disk_full")
fi
if [ "$retried_after_disk_full" -eq 1 ]; then
    degraded+=("rch_verify_retry_after_worker_disk_full")
fi
if [ -n "$disk_full_worker" ] && [ "${retry_worker:-}" = "$disk_full_worker" ]; then
    degraded+=("rch_verify_worker_quarantine_ignored")
fi
if [ "$COMMAND_KIND" = "raw" ]; then
    degraded+=("rch_verify_raw_command_may_not_offload")
fi
if printf '%s' "$combined_output" | grep -q "RCH-E327"; then
    degraded+=("rch_verify_topology_blocked")
fi
if printf '%s' "$combined_output" | grep -q "remote required; refusing local fallback"; then
    degraded+=("rch_verify_local_fallback_refused")
fi
if [ "$exit_code" -ne 0 ] && [ -z "$worker_id" ] && printf '%s' "$combined_output" | grep -Eiq "timed out|timeout|capacity|busy|no workers|workers_healthy: 0|all_workers_offline"; then
    degraded+=("rch_verify_capacity_or_timeout")
fi
if printf '%s' "$combined_output" | grep -q "non-compilation command"; then
    degraded+=("rch_verify_not_offloaded")
elif [ "$WOULD_OFFLOAD" = true ] && [ -z "$worker_id" ]; then
    degraded+=("rch_verify_remote_marker_missing")
fi

emit_json true "$exit_code" "$elapsed_ms" "$stdout_tail" "" "${degraded[@]}"
exit "$exit_code"
