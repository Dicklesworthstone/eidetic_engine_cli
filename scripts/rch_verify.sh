#!/usr/bin/env bash
# RCHVC.1 - stable remote verification wrapper for focused Rust checks.
#
# This script is intentionally repo-local. It makes the explicit RCH path the
# easy path for agents and emits a JSON proof that can be pasted into Beads.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd -P)"

usage() {
    cat <<'EOF'
Usage: scripts/rch_verify.sh [options] -- <verifier command...>

Options:
  --dry-run                 Do not execute; emit the planned explicit rch exec proof
  --allow-raw               Allow non-Cargo commands; still runs through rch exec
  --bead-id <id>            Optional bead id for ledger rows and summaries
  --ledger <path>           Append one derived JSONL evidence row
  --event-log <path>        Append one ee.test_event.v1 command_end event row
  --summary                 Include bead-ready Markdown summary in the JSON proof
  --no-write                Do not write --ledger; render proof/summary only
  --rch-bin <path>          RCH binary (default: $RCH_BIN or rch)
  --project-root <path>     Local project root (default: cwd)
  --env <NAME=VALUE>        Pass an explicit environment override to the remote verifier command
  --skip-build-admission    Skip local ee diag build-admission preflight with proof degradation
  --build-admission-ee-bin <path>
                            ee binary to use for build-admission preflight
  --build-admission-min-free-bytes <bytes>
                            Required local free bytes for build-admission checks
  --artifact-destination <path>
                            Extra local artifact destination checked by build-admission
  --require-clean-tree      Refuse before RCH when the git checkout is dirty
  --committed-tree          Verify the committed --treeish from a generated source export when safe
  --treeish <ref>           Committed-tree ref to prove (default: HEAD)
  --known-blocker-store <path>
                            Override the known RCH blocker cache path
  --known-blocker-override  Run through RCH despite a matching active known blocker
  --skip-known-blocker      Disable known-blocker cache read/write for this run
  --json                    Accepted for symmetry; output is always JSON
  -h, --help                Show this help

Environment:
  RCH_VERIFY_ATTEMPT_TIMEOUT_MS  Live rch exec timeout budget (default: 900000)
  RCH_VERIFY_PREFLIGHT_TIMEOUT_MS  Local helper probe timeout budget (default: 10000)
  RCH_VERIFY_TAIL_BYTES          Diagnostic stdout/stderr tail size (default: 4000)
  RCH_VERIFY_TMPDIR              Retained diagnostic artifact directory (default: /tmp)

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
EVENT_LOG_PATH=""
INCLUDE_SUMMARY=0
NO_WRITE=0
ENV_OVERRIDES=()
BUILD_ADMISSION_ENABLED="${RCH_VERIFY_BUILD_ADMISSION:-1}"
BUILD_ADMISSION_EE_BIN="${RCH_VERIFY_EE_BIN:-${EE_BIN:-${EE_BINARY:-}}}"
BUILD_ADMISSION_MIN_FREE_BYTES="${RCH_VERIFY_BUILD_ADMISSION_MIN_FREE_BYTES:-1073741824}"
BUILD_ADMISSION_ARTIFACT_DESTINATIONS=()
REQUIRE_CLEAN_TREE=0
COMMITTED_TREE=0
TREEISH="HEAD"
KNOWN_BLOCKER_ENABLED="${RCH_VERIFY_KNOWN_BLOCKER_ENABLED:-1}"
KNOWN_BLOCKER_OVERRIDE=0
KNOWN_BLOCKER_STORE="${RCH_VERIFY_KNOWN_BLOCKER_STORE:-}"
KNOWN_BLOCKER_STORE_EXPLICIT=0
if [ -n "${RCH_VERIFY_KNOWN_BLOCKER_STORE:-}" ]; then
    KNOWN_BLOCKER_STORE_EXPLICIT=1
fi
KNOWN_BLOCKER_TTL_SECONDS="${RCH_VERIFY_KNOWN_BLOCKER_TTL_SECONDS:-21600}"
KNOWN_BLOCKER_MAX_ENTRIES="${RCH_VERIFY_KNOWN_BLOCKER_MAX_ENTRIES:-128}"
KNOWN_BLOCKER_JSON="null"
RCH_VERIFY_ATTEMPT_TIMEOUT_MS="${RCH_VERIFY_ATTEMPT_TIMEOUT_MS:-900000}"
RCH_VERIFY_PREFLIGHT_TIMEOUT_MS="${RCH_VERIFY_PREFLIGHT_TIMEOUT_MS:-10000}"
RCH_VERIFY_TAIL_BYTES="${RCH_VERIFY_TAIL_BYTES:-4000}"
RCH_VERIFY_TMPDIR="${RCH_VERIFY_TMPDIR:-/tmp}"
RCH_ATTEMPT_TIMED_OUT=false
RCH_STDOUT_BYTES=0
RCH_STDERR_BYTES=0
RCH_ARTIFACT_KINDS=()
RCH_ARTIFACT_PATHS=()
RCH_ARTIFACT_ATTEMPTS=()
RCH_ATTEMPT_STDOUT_FILE=""
RCH_ATTEMPT_STDERR_FILE=""
RCH_ATTEMPT_META_FILE=""
RCH_RUNTIME_JSON='{"status":"not_checked","client_path":null,"client_version":null,"client_compat":null,"daemon_version":null,"daemon_compat":null,"daemon_socket_path":null,"message":null}'
LOCAL_CARGO_PROCESSES_JSON='{"schema":"ee.rch_local_cargo_tripwire.v1","mode":"probe_processes","status":"not_run","count":0,"processes":[],"detectedLocalBuilds":[],"reason":"not requested"}'
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
        --event-log) EVENT_LOG_PATH="${2:?--event-log requires a value}"; shift 2 ;;
        --summary) INCLUDE_SUMMARY=1; shift ;;
        --no-write) NO_WRITE=1; shift ;;
        --rch-bin) RCH_BIN="${2:?--rch-bin requires a value}"; shift 2 ;;
        --project-root) PROJECT_ROOT="${2:?--project-root requires a value}"; shift 2 ;;
        --env) ENV_OVERRIDES+=("$(validate_env_override "${2:?--env requires NAME=VALUE}")"); shift 2 ;;
        --skip-build-admission) BUILD_ADMISSION_ENABLED=0; shift ;;
        --build-admission-ee-bin) BUILD_ADMISSION_EE_BIN="${2:?--build-admission-ee-bin requires a value}"; shift 2 ;;
        --build-admission-min-free-bytes) BUILD_ADMISSION_MIN_FREE_BYTES="${2:?--build-admission-min-free-bytes requires a value}"; shift 2 ;;
        --artifact-destination) BUILD_ADMISSION_ARTIFACT_DESTINATIONS+=("${2:?--artifact-destination requires a value}"); shift 2 ;;
        --require-clean-tree) REQUIRE_CLEAN_TREE=1; shift ;;
        --committed-tree) COMMITTED_TREE=1; shift ;;
        --treeish) TREEISH="${2:?--treeish requires a value}"; shift 2 ;;
        --known-blocker-store) KNOWN_BLOCKER_STORE="${2:?--known-blocker-store requires a value}"; KNOWN_BLOCKER_STORE_EXPLICIT=1; shift 2 ;;
        --known-blocker-override) KNOWN_BLOCKER_OVERRIDE=1; shift ;;
        --skip-known-blocker) KNOWN_BLOCKER_ENABLED=0; shift ;;
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

if [ -z "$KNOWN_BLOCKER_STORE" ]; then
    KNOWN_BLOCKER_STORE="$PROJECT_ROOT/.ee/derived/rch/known_blockers.jsonl"
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

positive_integer_or_die() {
    local name="$1"
    local value="$2"
    case "$value" in
        ''|*[!0-9]*)
            echo "rch_verify: $name must be a positive integer, got: $value" >&2
            exit 2
            ;;
        0)
            echo "rch_verify: $name must be greater than zero" >&2
            exit 2
            ;;
    esac
}

json_file_field() {
    local path="$1"
    local field="$2"
    python3 - "$path" "$field" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as handle:
    payload = json.load(handle)
value = payload.get(sys.argv[2])
if isinstance(value, bool):
    print("true" if value else "false")
elif value is None:
    print("")
else:
    print(value)
PY
}

json_text_field() {
    local payload="$1"
    local field="$2"
    JSON_INPUT="$payload" JSON_FIELD="$field" python3 - <<'PY'
import json
import os

payload = json.loads(os.environ["JSON_INPUT"])
value = payload.get(os.environ["JSON_FIELD"])
if isinstance(value, bool):
    print("true" if value else "false")
elif value is None:
    print("")
else:
    print(value)
PY
}

file_bytes() {
    local path="$1"
    if [ -z "$path" ] || [ ! -e "$path" ]; then
        printf '0'
        return 0
    fi
    wc -c <"$path" | tr -d ' '
}

capture_command_with_timeout() {
    local timeout_ms="$1"
    local cwd="$2"
    shift 2
    python3 - "$timeout_ms" "$cwd" "$@" <<'PY'
import json
import os
import signal
import subprocess
import sys
import time

timeout_ms = int(sys.argv[1])
cwd = sys.argv[2]
argv = sys.argv[3:]
started = time.monotonic()
timed_out = False
status = 0
output = b""

try:
    process = subprocess.Popen(
        argv,
        cwd=cwd,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        start_new_session=True,
    )
except OSError as error:
    elapsed_ms = int((time.monotonic() - started) * 1000)
    print(json.dumps({
        "status": 126,
        "timed_out": False,
        "elapsed_ms": elapsed_ms,
        "output": f"{type(error).__name__}: {error}",
    }, sort_keys=True, separators=(",", ":")))
    raise SystemExit(0)
try:
    output, _ = process.communicate(timeout=timeout_ms / 1000)
    status = process.returncode
except subprocess.TimeoutExpired:
    timed_out = True
    try:
        os.killpg(process.pid, signal.SIGTERM)
    except ProcessLookupError:
        pass
    except Exception:
        process.terminate()
    try:
        output, _ = process.communicate(timeout=1)
    except subprocess.TimeoutExpired:
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        except Exception:
            process.kill()
        output, _ = process.communicate()
    status = 124

elapsed_ms = int((time.monotonic() - started) * 1000)
print(json.dumps({
    "status": status,
    "timed_out": timed_out,
    "elapsed_ms": elapsed_ms,
    "output": output.decode("utf-8", "replace"),
}, sort_keys=True, separators=(",", ":")))
PY
}

prepare_attempt_artifacts() {
    local attempt="$1"
    RCH_ATTEMPT_STDOUT_FILE="$(mktemp "$RCH_VERIFY_TMPDIR/rch-verify-${attempt}-stdout.XXXXXX")" || exit 2
    RCH_ATTEMPT_STDERR_FILE="$(mktemp "$RCH_VERIFY_TMPDIR/rch-verify-${attempt}-stderr.XXXXXX")" || exit 2
    RCH_ATTEMPT_META_FILE="$(mktemp "$RCH_VERIFY_TMPDIR/rch-verify-${attempt}-meta.XXXXXX")" || exit 2
}

record_attempt_artifacts() {
    local attempt="$1"
    if [ -n "$RCH_ATTEMPT_STDOUT_FILE" ]; then
        RCH_ARTIFACT_KINDS+=("stdout")
        RCH_ARTIFACT_PATHS+=("$RCH_ATTEMPT_STDOUT_FILE")
        RCH_ARTIFACT_ATTEMPTS+=("$attempt")
    fi
    if [ -n "$RCH_ATTEMPT_STDERR_FILE" ]; then
        RCH_ARTIFACT_KINDS+=("stderr")
        RCH_ARTIFACT_PATHS+=("$RCH_ATTEMPT_STDERR_FILE")
        RCH_ARTIFACT_ATTEMPTS+=("$attempt")
    fi
}

attempt_artifacts_json() {
    python3 - "$@" <<'PY'
import json
import sys

items = []
args = sys.argv[1:]
for index in range(0, len(args), 3):
    try:
        kind, path, attempt = args[index:index + 3]
    except ValueError:
        continue
    if path:
        items.append({"kind": kind, "path": path, "attempt": attempt})
print(json.dumps(items, sort_keys=True, separators=(",", ":")))
PY
}

artifact_tail() {
    local kind="$1"
    local index
    for index in "${!RCH_ARTIFACT_KINDS[@]}"; do
        if [ "${RCH_ARTIFACT_KINDS[$index]}" = "$kind" ] && [ -r "${RCH_ARTIFACT_PATHS[$index]}" ]; then
            cat "${RCH_ARTIFACT_PATHS[$index]}"
        fi
    done | tail_text
}

run_process_with_timeout() {
    local stdout_file="$1"
    local stderr_file="$2"
    local meta_file="$3"
    local cwd="$4"
    shift 4
    python3 - "$RCH_VERIFY_ATTEMPT_TIMEOUT_MS" "$stdout_file" "$stderr_file" "$meta_file" "$cwd" "$@" <<'PY'
import json
import os
import signal
import subprocess
import sys
import time

timeout_ms = int(sys.argv[1])
stdout_path = sys.argv[2]
stderr_path = sys.argv[3]
meta_path = sys.argv[4]
cwd = sys.argv[5]
argv = sys.argv[6:]
started = time.monotonic()
timed_out = False
status = 0

with open(stdout_path, "wb") as stdout_file, open(stderr_path, "wb") as stderr_file:
    process = subprocess.Popen(
        argv,
        cwd=cwd,
        stdout=stdout_file,
        stderr=stderr_file,
        start_new_session=True,
    )
    try:
        status = process.wait(timeout=timeout_ms / 1000)
    except subprocess.TimeoutExpired:
        timed_out = True
        try:
            os.killpg(process.pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
        except Exception:
            process.terminate()
        try:
            process.wait(timeout=1)
        except subprocess.TimeoutExpired:
            # Kill only the process group this wrapper created; never scan for peers.
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            except Exception:
                process.kill()
            process.wait()
        status = 124

elapsed_ms = int((time.monotonic() - started) * 1000)
meta = {
    "status": status,
    "timed_out": timed_out,
    "elapsed_ms": elapsed_ms,
    "stdout_bytes": os.path.getsize(stdout_path) if os.path.exists(stdout_path) else 0,
    "stderr_bytes": os.path.getsize(stderr_path) if os.path.exists(stderr_path) else 0,
}
with open(meta_path, "w", encoding="utf-8") as handle:
    json.dump(meta, handle, sort_keys=True, separators=(",", ":"))
sys.exit(status)
PY
}

json_object_not_run() {
    printf '{"status":"not_run","admitted":null,"ee_bin":null,"min_free_bytes":null,"checks":[],"degraded_codes":[],"message":null}'
}

local_cargo_processes_not_run_json() {
    local reason="${1:-not requested}"
    LOCAL_CARGO_PROCESSES_REASON="$reason" python3 - <<'PY'
import json
import os

payload = {
    "schema": "ee.rch_local_cargo_tripwire.v1",
    "mode": "probe_processes",
    "status": "not_run",
    "count": 0,
    "processes": [],
    "detectedLocalBuilds": [],
    "reason": os.environ.get("LOCAL_CARGO_PROCESSES_REASON") or "not requested",
}
print(json.dumps(payload, sort_keys=True, separators=(",", ":")))
PY
}

local_cargo_processes_unavailable_json() {
    local reason="${1:-local cargo tripwire unavailable}"
    LOCAL_CARGO_PROCESSES_REASON="$reason" python3 - <<'PY'
import json
import os

payload = {
    "schema": "ee.rch_local_cargo_tripwire.v1",
    "mode": "probe_processes",
    "status": "unavailable",
    "count": 0,
    "processes": [],
    "detectedLocalBuilds": [],
    "reason": os.environ.get("LOCAL_CARGO_PROCESSES_REASON") or "local cargo tripwire unavailable",
}
print(json.dumps(payload, sort_keys=True, separators=(",", ":")))
PY
}

compute_local_cargo_processes_json() {
    if [ -n "${RCH_VERIFY_LOCAL_CARGO_PROCESSES_JSON:-}" ]; then
        printf '%s\n' "$RCH_VERIFY_LOCAL_CARGO_PROCESSES_JSON"
        return 0
    fi
    if [ "${RCH_VERIFY_LOCAL_CARGO_SCAN:-1}" = "0" ]; then
        local_cargo_processes_not_run_json "disabled by RCH_VERIFY_LOCAL_CARGO_SCAN=0"
        return 0
    fi
    if [ "$INCLUDE_SUMMARY" -ne 1 ] && [ "${RCH_VERIFY_LOCAL_CARGO_SCAN:-0}" != "1" ]; then
        local_cargo_processes_not_run_json "only scanned for --summary unless RCH_VERIFY_LOCAL_CARGO_SCAN=1"
        return 0
    fi
    if [ -n "${RCH_VERIFY_FAKE_OUTPUT:-}" ] && [ "${RCH_VERIFY_LOCAL_CARGO_SCAN:-0}" != "1" ]; then
        local_cargo_processes_not_run_json "fake RCH transcript without explicit local Cargo scan"
        return 0
    fi

    local tripwire="$SCRIPT_DIR/check-local-cargo-tripwire.sh"
    if [ ! -r "$tripwire" ]; then
        local_cargo_processes_unavailable_json "check-local-cargo-tripwire.sh is not readable"
        return 0
    fi

    local output exit_code
    set +e
    output="$(bash "$tripwire" --probe-processes --json 2>/dev/null)"
    exit_code=$?
    set -e
    if [ -z "$output" ]; then
        local_cargo_processes_unavailable_json "check-local-cargo-tripwire.sh emitted no JSON"
        return 0
    fi
    LOCAL_CARGO_PROCESSES_OUTPUT="$output" \
    LOCAL_CARGO_PROCESSES_EXIT_CODE="$exit_code" \
    python3 - <<'PY'
import json
import os

raw = os.environ.get("LOCAL_CARGO_PROCESSES_OUTPUT", "")
exit_code = int(os.environ.get("LOCAL_CARGO_PROCESSES_EXIT_CODE") or "0")
try:
    payload = json.loads(raw)
except Exception:
    payload = {
        "schema": "ee.rch_local_cargo_tripwire.v1",
        "mode": "probe_processes",
        "status": "unavailable",
        "count": 0,
        "processes": [],
        "detectedLocalBuilds": [],
        "reason": "check-local-cargo-tripwire.sh emitted invalid JSON",
        "exit_code": exit_code,
    }
else:
    payload.setdefault("schema", "ee.rch_local_cargo_tripwire.v1")
    payload.setdefault("mode", "probe_processes")
    payload.setdefault("processes", [])
    payload.setdefault("detectedLocalBuilds", [])
    payload["exit_code"] = exit_code
print(json.dumps(payload, sort_keys=True, separators=(",", ":")))
PY
}

csv_json_array() {
    CSV_INPUT="${1:-}" python3 - <<'PY'
import json
import os

seen = []
for item in os.environ.get("CSV_INPUT", "").split(","):
    item = item.strip()
    if item and item not in seen:
        seen.append(item)
print(json.dumps(seen, separators=(",", ":")))
PY
}

build_admission_skipped_json() {
    local reason="${1:?skip reason required}"
    local ee_bin="${2:-}"
    BUILD_ADMISSION_REASON="$reason" \
    BUILD_ADMISSION_EE_BIN_VALUE="$ee_bin" \
    BUILD_ADMISSION_MIN_FREE_BYTES_VALUE="$BUILD_ADMISSION_MIN_FREE_BYTES" \
    python3 - <<'PY'
import json
import os

payload = {
    "status": "skipped",
    "admitted": None,
    "ee_bin": os.environ.get("BUILD_ADMISSION_EE_BIN_VALUE") or None,
    "min_free_bytes": int(os.environ.get("BUILD_ADMISSION_MIN_FREE_BYTES_VALUE") or 0),
    "checks": [],
    "degraded_codes": [],
    "message": os.environ.get("BUILD_ADMISSION_REASON") or None,
}
print(json.dumps(payload, sort_keys=True, separators=(",", ":")))
PY
}

candidate_ee_bin() {
    if [ -n "$BUILD_ADMISSION_EE_BIN" ]; then
        printf '%s' "$BUILD_ADMISSION_EE_BIN"
        return 0
    fi

    local candidate version_probe version_output version_timed_out
    for candidate in \
        "${CARGO_TARGET_DIR:-}/debug/ee" \
        "${CARGO_TARGET_DIR:-}/release/ee" \
        "$PROJECT_ROOT/target/debug/ee" \
        "$PROJECT_ROOT/target/release/ee"
    do
        if [ -n "$candidate" ] && [ -x "$candidate" ]; then
            version_probe="$(capture_command_with_timeout "$RCH_VERIFY_PREFLIGHT_TIMEOUT_MS" "$PROJECT_ROOT" "$candidate" --version)"
            version_timed_out="$(json_text_field "$version_probe" timed_out)"
            version_output="$(json_text_field "$version_probe" output)"
            if [ "$version_timed_out" != "true" ] && [ -n "${version_output//[[:space:]]/}" ]; then
                printf '%s' "$candidate"
                return 0
            fi
        fi
    done
    return 1
}

compute_build_admission_json() {
    if [ "$BUILD_ADMISSION_ENABLED" != "1" ]; then
        build_admission_skipped_json "disabled by --skip-build-admission or RCH_VERIFY_BUILD_ADMISSION=0" ""
        return 0
    fi
    if [ "$DRY_RUN" -eq 1 ]; then
        build_admission_skipped_json "dry-run does not execute build-admission" ""
        return 0
    fi
    if [ -n "${RCH_VERIFY_FAKE_OUTPUT:-}" ] && [ -z "$BUILD_ADMISSION_EE_BIN" ]; then
        build_admission_skipped_json "fake RCH transcript without explicit ee binary" ""
        return 0
    fi

    local ee_bin
    if ! ee_bin="$(candidate_ee_bin)"; then
        BUILD_ADMISSION_MESSAGE="no executable ee binary found for build-admission preflight" \
        BUILD_ADMISSION_MIN_FREE_BYTES_VALUE="$BUILD_ADMISSION_MIN_FREE_BYTES" \
        python3 - <<'PY'
import json
import os

payload = {
    "status": "unavailable",
    "admitted": None,
    "ee_bin": None,
    "min_free_bytes": int(os.environ.get("BUILD_ADMISSION_MIN_FREE_BYTES_VALUE") or 0),
    "checks": [],
    "degraded_codes": [],
    "message": os.environ.get("BUILD_ADMISSION_MESSAGE"),
}
print(json.dumps(payload, sort_keys=True, separators=(",", ":")))
PY
        return 0
    fi

    local args=(
        "$ee_bin" "--workspace" "$PROJECT_ROOT" "diag" "build-admission" "--json"
        "--min-free-bytes" "$BUILD_ADMISSION_MIN_FREE_BYTES"
    )
    local destination
    for destination in "${BUILD_ADMISSION_ARTIFACT_DESTINATIONS[@]}"; do
        args+=("--artifact-destination" "$destination")
    done

    local output exit_code admission_probe admission_timed_out
    admission_probe="$(capture_command_with_timeout "$RCH_VERIFY_PREFLIGHT_TIMEOUT_MS" "$PROJECT_ROOT" "${args[@]}")"
    output="$(json_text_field "$admission_probe" output)"
    exit_code="$(json_text_field "$admission_probe" status)"
    admission_timed_out="$(json_text_field "$admission_probe" timed_out)"
    if [ "$admission_timed_out" = "true" ]; then
        output="${output}
[RCH_VERIFY] build-admission preflight timed out after ${RCH_VERIFY_PREFLIGHT_TIMEOUT_MS}ms"
    fi

    BUILD_ADMISSION_OUTPUT="$output" \
    BUILD_ADMISSION_EXIT_CODE="$exit_code" \
    BUILD_ADMISSION_EE_BIN_VALUE="$ee_bin" \
    BUILD_ADMISSION_MIN_FREE_BYTES_VALUE="$BUILD_ADMISSION_MIN_FREE_BYTES" \
    python3 - <<'PY'
import hashlib
import json
import os
import re

raw = os.environ.get("BUILD_ADMISSION_OUTPUT", "")
exit_code = int(os.environ.get("BUILD_ADMISSION_EXIT_CODE") or 0)
ee_bin = os.environ.get("BUILD_ADMISSION_EE_BIN_VALUE") or None
min_free = int(os.environ.get("BUILD_ADMISSION_MIN_FREE_BYTES_VALUE") or 0)

def redact(text):
    text = re.sub(r"\x1b\[[0-9;]*m", "", text or "")
    text = re.sub(r"/Users/[^/\s]+", "/Users/<redacted>", text)
    text = re.sub(r"(?i)(token|secret|password|api[_-]?key)=\S+", r"\1=<redacted>", text)
    return text[-1200:]

base = {
    "status": "unavailable",
    "admitted": None,
    "ee_bin": ee_bin,
    "min_free_bytes": min_free,
    "checks": [],
    "degraded_codes": [],
    "message": None,
    "exit_code": exit_code,
    "raw_hash": "sha256:" + hashlib.sha256(raw.encode("utf-8", "replace")).hexdigest(),
}

try:
    payload = json.loads(raw)
except Exception:
    base["message"] = "ee diag build-admission did not emit valid JSON: " + redact(raw)
    print(json.dumps(base, sort_keys=True, separators=(",", ":")))
    raise SystemExit(0)

data = payload.get("data") if isinstance(payload, dict) else None
if exit_code != 0 or not isinstance(data, dict) or payload.get("success") is not True:
    base["message"] = "ee diag build-admission did not return a successful response"
    print(json.dumps(base, sort_keys=True, separators=(",", ":")))
    raise SystemExit(0)

checks = []
for check in data.get("checks") or []:
    if not isinstance(check, dict):
        continue
    checks.append({
        "label": check.get("label"),
        "path": check.get("path"),
        "bytes_available": check.get("bytesAvailable"),
        "min_free_bytes": check.get("minFreeBytes"),
        "admitted": check.get("admitted"),
        "external_required": check.get("externalRequired"),
        "external": check.get("external"),
    })

degraded_codes = []
for item in data.get("degraded") or []:
    if isinstance(item, dict) and item.get("code"):
        degraded_codes.append(str(item["code"]))

admitted = data.get("admitted")
base.update({
    "status": "passed" if admitted is True else "denied",
    "admitted": admitted,
    "checks": checks,
    "degraded_codes": degraded_codes,
    "message": None if admitted is True else "ee diag build-admission denied local verification admission",
})
print(json.dumps(base, sort_keys=True, separators=(",", ":")))
PY
}

build_admission_status() {
    BUILD_ADMISSION_JSON_INPUT="${1:?build-admission JSON required}" python3 - <<'PY'
import json
import os

payload = json.loads(os.environ["BUILD_ADMISSION_JSON_INPUT"])
print(payload.get("status") or "")
PY
}

csv_contains() {
    CSV_INPUT="${1:-}" CSV_NEEDLE="${2:-}" python3 - <<'PY'
import os
import sys

needle = os.environ.get("CSV_NEEDLE", "").strip()
items = {
    item.strip()
    for item in os.environ.get("CSV_INPUT", "").split(",")
    if item.strip()
}
sys.exit(0 if needle and needle in items else 1)
PY
}

tail_text() {
    RCH_VERIFY_TAIL_BYTES="$RCH_VERIFY_TAIL_BYTES" python3 -c 'import os, sys; text=sys.stdin.read(); print(text[-int(os.environ["RCH_VERIFY_TAIL_BYTES"]):])'
}

extract_worker_id() {
    sed -n \
        -e 's/^.*Selected worker: \([A-Za-z0-9_.-][A-Za-z0-9_.-]*\) .*/\1/p' \
        -e 's/^\[RCH\] remote \([A-Za-z0-9_.-][A-Za-z0-9_.-]*\) (.*/\1/p' \
        -e 's/^\[RCH\] remote \([A-Za-z0-9_.-][A-Za-z0-9_.-]*\) failed.*/\1/p' \
        | tail -n 1
}

extract_dependency_planner_worker_id() {
    sed -n \
        -e 's/^.*Dependency planner fail-open on \([A-Za-z0-9_.-][A-Za-z0-9_.-]*\) \[RCH-E[0-9][0-9][0-9]\].*/\1/p' \
        | tail -n 1
}

is_worker_disk_full_output() {
    grep -Eiq "No space left on device|disk full|ENOSPC"
}

is_cargo_workspace_inheritance_output() {
    grep -Eiq "error inheriting .+ from workspace root manifest|workspace\.package\.[A-Za-z0-9_.-]+.*was not defined"
}

is_cargo_path_dependency_version_output() {
    local text
    text="$(cat)"
    printf '%s' "$text" | grep -Eiq "failed to select a version for the requirement" &&
        printf '%s' "$text" | grep -Eiq "candidate versions found which didn't match:" &&
        printf '%s' "$text" | grep -Eiq "location searched: /data/projects/"
}

is_all_workers_preflight_failed_output() {
    grep -Eiq "all workers failed preflight checks|all workers failed preflight|no worker selected.*all workers failed preflight"
}

configured_workers() {
    CONFIGURED_WORKERS="${RCH_VERIFY_CONFIGURED_WORKERS:-}" \
    FAKE_OUTPUT_PRESENT="${RCH_VERIFY_FAKE_OUTPUT:+1}" \
    RCH_BIN_PATH="$RCH_BIN" \
    python3 - <<'PY'
import json
import os
import subprocess

explicit = os.environ.get("CONFIGURED_WORKERS", "")

def emit(ids):
    seen = []
    for item in ids:
        item = item.strip()
        if item and item not in seen:
            seen.append(item)
    print(",".join(seen))

if explicit:
    emit(explicit.split(","))
    raise SystemExit(0)

if os.environ.get("FAKE_OUTPUT_PRESENT"):
    print("")
    raise SystemExit(0)

try:
    listed = subprocess.run(
        [os.environ["RCH_BIN_PATH"], "workers", "list", "--json"],
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

daemon_workers() {
    DAEMON_WORKERS="${RCH_VERIFY_DAEMON_WORKERS:-}" \
    FAKE_OUTPUT_PRESENT="${RCH_VERIFY_FAKE_OUTPUT:+1}" \
    RCH_BIN_PATH="$RCH_BIN" \
    python3 - <<'PY'
import json
import os
import subprocess

explicit = os.environ.get("DAEMON_WORKERS", "")

def emit(ids):
    seen = []
    for item in ids:
        item = item.strip()
        if item and item not in seen:
            seen.append(item)
    print(",".join(seen))

if explicit:
    emit(explicit.split(","))
    raise SystemExit(0)

if os.environ.get("FAKE_OUTPUT_PRESENT"):
    print("")
    raise SystemExit(0)

try:
    status = subprocess.run(
        [os.environ["RCH_BIN_PATH"], "status", "--workers", "--jobs", "--json"],
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        text=True,
        timeout=10,
    )
    payload = json.loads(status.stdout)
    workers = payload.get("data", {}).get("daemon", {}).get("workers", [])
    emit(worker.get("id", "") for worker in workers)
except Exception:
    print("")
PY
}

rch_runtime_json() {
    FAKE_OUTPUT_PRESENT="${RCH_VERIFY_FAKE_OUTPUT:+1}" \
    RCH_BIN_PATH="$RCH_BIN" \
    STATUS_JSON="${RCH_VERIFY_STATUS_JSON:-}" \
    python3 - <<'PY'
import json
import os
import re
import subprocess

client_path = os.environ.get("RCH_BIN_PATH") or None
base = {
    "status": "not_checked",
    "client_path": client_path,
    "client_version": None,
    "client_compat": None,
    "daemon_version": None,
    "daemon_compat": None,
    "daemon_socket_path": None,
    "message": None,
}

def compat(version):
    if not version:
        return None
    match = re.search(r"(\d+)\.(\d+)(?:\.\d+)?", str(version))
    if not match:
        return None
    return f"{match.group(1)}.{match.group(2)}"

def emit(payload):
    print(json.dumps(payload, sort_keys=True, separators=(",", ":")))

if os.environ.get("FAKE_OUTPUT_PRESENT"):
    base["status"] = "skipped"
    base["message"] = "fake RCH transcript without live client/daemon inspection"
    emit(base)
    raise SystemExit(0)

try:
    version = subprocess.run(
        [os.environ["RCH_BIN_PATH"], "--version"],
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        timeout=10,
    )
    if version.returncode == 0:
        words = version.stdout.strip().split()
        base["client_version"] = words[-1] if words else None
        base["client_compat"] = compat(base["client_version"])
except Exception as error:
    base["status"] = "unavailable"
    base["message"] = f"client version unavailable: {error}"
    emit(base)
    raise SystemExit(0)

try:
    status_json = os.environ.get("STATUS_JSON", "")
    if status_json:
        payload = json.loads(status_json)
    else:
        status = subprocess.run(
            [os.environ["RCH_BIN_PATH"], "status", "--workers", "--jobs", "--json"],
            check=False,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            timeout=10,
        )
        payload = json.loads(status.stdout)
    daemon_container = payload.get("data", {}).get("daemon", {})
    daemon = daemon_container.get("daemon") if isinstance(daemon_container.get("daemon"), dict) else daemon_container
    base["daemon_version"] = daemon.get("version") or daemon_container.get("version")
    base["daemon_compat"] = compat(base["daemon_version"])
    base["daemon_socket_path"] = daemon.get("socket_path") or daemon_container.get("socket_path")
except Exception as error:
    base["status"] = "unavailable"
    base["message"] = f"daemon status unavailable: {error}"
    emit(base)
    raise SystemExit(0)

if base["client_compat"] and base["daemon_compat"]:
    base["status"] = "checked"
elif base["client_version"] or base["daemon_version"]:
    base["status"] = "partial"
    base["message"] = "client or daemon version was present but not parseable"
else:
    base["status"] = "unavailable"
    base["message"] = "client and daemon versions unavailable"

emit(base)
PY
}

rch_runtime_skew_code() {
    RCH_RUNTIME_JSON_INPUT="${1:?runtime JSON required}" python3 - <<'PY'
import json
import os

runtime = json.loads(os.environ["RCH_RUNTIME_JSON_INPUT"])
if (
    runtime.get("status") == "checked"
    and runtime.get("client_compat")
    and runtime.get("daemon_compat")
    and runtime.get("client_compat") != runtime.get("daemon_compat")
):
    print("rch_verify_client_daemon_version_skew")
PY
}

known_blocker_lookup_json() {
    if [ "$KNOWN_BLOCKER_ENABLED" != "1" ]; then
        printf 'null'
        return 0
    fi
    if [ -n "${RCH_VERIFY_FAKE_OUTPUT:-}" ] && [ "$KNOWN_BLOCKER_STORE_EXPLICIT" != "1" ]; then
        printf 'null'
        return 0
    fi
    KNOWN_BLOCKER_STORE_PATH="$KNOWN_BLOCKER_STORE" \
    SOURCE_STATE_JSON_INPUT="${1:?source state JSON required}" \
    COMMAND_KIND_VALUE="$COMMAND_KIND" \
    COMMAND_TEXT_VALUE="$(command_string "${ENV_OVERRIDES[@]}" "${COMMAND[@]}")" \
    REQUESTED_WORKERS_VALUE="${REQUESTED_WORKERS_CSV:-}" \
    CONFIGURED_WORKERS_VALUE="${CONFIGURED_WORKERS_CSV:-}" \
    RCH_RUNTIME_JSON_INPUT="${RCH_RUNTIME_JSON:-}" \
    KNOWN_BLOCKER_NOW="${RCH_VERIFY_NOW:-}" \
    python3 - <<'PY'
import datetime as dt
import hashlib
import json
import os
from pathlib import Path

def csv_items(raw):
    seen = []
    for item in (raw or "").split(","):
        item = item.strip()
        if item and item not in seen:
            seen.append(item)
    return seen

def parse_time(value):
    if not value:
        return None
    text = str(value)
    if text.endswith("Z"):
        text = text[:-1] + "+00:00"
    try:
        parsed = dt.datetime.fromisoformat(text)
    except ValueError:
        return None
    if parsed.tzinfo is None:
        parsed = parsed.replace(tzinfo=dt.timezone.utc)
    return parsed.astimezone(dt.timezone.utc)

def now_utc():
    explicit = parse_time(os.environ.get("KNOWN_BLOCKER_NOW"))
    return explicit or dt.datetime.now(dt.timezone.utc)

def emit(payload):
    print(json.dumps(payload, sort_keys=True, separators=(",", ":")))

store_path = os.environ.get("KNOWN_BLOCKER_STORE_PATH", "")
if not store_path:
    print("null")
    raise SystemExit(0)

path = Path(store_path)
if not path.exists():
    print("null")
    raise SystemExit(0)

try:
    source_state = json.loads(os.environ.get("SOURCE_STATE_JSON_INPUT") or "{}")
except Exception:
    source_state = {}
try:
    runtime = json.loads(os.environ.get("RCH_RUNTIME_JSON_INPUT") or "{}")
except Exception:
    runtime = {}

command_text = os.environ.get("COMMAND_TEXT_VALUE", "")
command_hash = hashlib.sha256(command_text.encode("utf-8")).hexdigest()
source_state_hash = (
    source_state.get("source_manifest_hash")
    or source_state.get("dirty_status_hash")
    or ""
)
verifier_source_mode = source_state.get("verification_attribution") or None
requested_workers = csv_items(os.environ.get("REQUESTED_WORKERS_VALUE", ""))
configured_workers = csv_items(os.environ.get("CONFIGURED_WORKERS_VALUE", ""))
runtime_fingerprint = {
    "client_compat": runtime.get("client_compat"),
    "daemon_compat": runtime.get("daemon_compat"),
    "status": runtime.get("status"),
}
current = {
    "command_kind": os.environ.get("COMMAND_KIND_VALUE") or None,
    "command_hash": command_hash,
    "source_state_hash": source_state_hash,
    "verifier_source_mode": verifier_source_mode,
    "requested_workers": requested_workers,
    "configured_workers": configured_workers,
    "runtime_fingerprint": runtime_fingerprint,
}

now = now_utc()
matches = []
try:
    lines = path.read_text(encoding="utf-8").splitlines()
except OSError:
    print("null")
    raise SystemExit(0)

for line in lines:
    if not line.strip():
        continue
    try:
        entry = json.loads(line)
    except Exception:
        continue
    expires_at = parse_time(entry.get("expires_at"))
    if expires_at is None or expires_at <= now:
        continue
    if entry.get("command_kind") != current["command_kind"]:
        continue
    if entry.get("command_hash") != current["command_hash"]:
        continue
    if entry.get("source_state_hash") != current["source_state_hash"]:
        continue
    if entry.get("verifier_source_mode") != current["verifier_source_mode"]:
        continue
    if (entry.get("requested_workers") or []) != current["requested_workers"]:
        continue
    if (entry.get("configured_workers") or []) != current["configured_workers"]:
        continue
    if (entry.get("runtime_fingerprint") or {}) != current["runtime_fingerprint"]:
        continue
    matches.append(entry)

if not matches:
    print("null")
    raise SystemExit(0)

matches.sort(key=lambda item: item.get("last_seen") or item.get("first_seen") or "")
matched = dict(matches[-1])
matched["matched_at"] = now.isoformat(timespec="microseconds").replace("+00:00", "Z")
matched["override_used"] = False
emit(matched)
PY
}

known_blocker_override_json() {
    KNOWN_BLOCKER_JSON_INPUT="${1:?known blocker JSON required}" python3 - <<'PY'
import json
import os

payload = json.loads(os.environ["KNOWN_BLOCKER_JSON_INPUT"])
payload["override_used"] = True
print(json.dumps(payload, sort_keys=True, separators=(",", ":")))
PY
}

stale_disk_full_daemon_workers() {
    CONFIGURED_WORKERS="${1:-}" \
    DAEMON_WORKERS="${2:-}" \
    DISK_FULL_WORKERS="${3:-}" \
    python3 - <<'PY'
import os

configured = {
    item.strip()
    for item in os.environ.get("CONFIGURED_WORKERS", "").split(",")
    if item.strip()
}
daemon = [
    item.strip()
    for item in os.environ.get("DAEMON_WORKERS", "").split(",")
    if item.strip()
]
disk_full = {
    item.strip()
    for item in os.environ.get("DISK_FULL_WORKERS", "").split(",")
    if item.strip()
}
stale = [
    item
    for item in daemon
    if item not in configured and item in disk_full
]
print(",".join(dict.fromkeys(stale)))
PY
}

recent_failed_excluded_daemon_workers() {
    CONFIGURED_WORKERS="${1:-}" \
    DAEMON_WORKERS="${2:-}" \
    RECENT_FAILURE_MAX_MS="${3:-${RCH_VERIFY_RECENT_FAILURE_MAX_MS:-10000}}" \
    STATUS_JSON="${RCH_VERIFY_STATUS_JSON:-}" \
    FAKE_OUTPUT_PRESENT="${RCH_VERIFY_FAKE_OUTPUT:+1}" \
    RCH_BIN_PATH="$RCH_BIN" \
    python3 - <<'PY'
import json
import os
import subprocess

configured = {
    item.strip()
    for item in os.environ.get("CONFIGURED_WORKERS", "").split(",")
    if item.strip()
}
daemon = {
    item.strip()
    for item in os.environ.get("DAEMON_WORKERS", "").split(",")
    if item.strip()
}

try:
    max_duration_ms = int(os.environ.get("RECENT_FAILURE_MAX_MS") or "10000")
except ValueError:
    max_duration_ms = 10000

status_json = os.environ.get("STATUS_JSON", "")
if not status_json and os.environ.get("FAKE_OUTPUT_PRESENT"):
    print("")
    raise SystemExit(0)

try:
    if status_json:
        payload = json.loads(status_json)
    else:
        result = subprocess.run(
            [os.environ["RCH_BIN_PATH"], "status", "--workers", "--jobs", "--json"],
            check=False,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
            timeout=10,
        )
        payload = json.loads(result.stdout)
except Exception:
    print("")
    raise SystemExit(0)

recent = payload.get("data", {}).get("daemon", {}).get("recent_builds", [])
stale = []
for build in recent:
    worker = str(build.get("worker_id") or "").strip()
    if not worker or worker in configured or worker not in daemon:
        continue
    exit_code = build.get("exit_code")
    try:
        duration_ms = int(build.get("duration_ms") or 0)
    except (TypeError, ValueError):
        duration_ms = 0
    if exit_code not in (None, 0) and 0 < duration_ms <= max_duration_ms:
        if worker not in stale:
            stale.append(worker)

print(",".join(stale))
PY
}

healthy_alternate_workers() {
    local failed_worker="${1:?failed worker required}"
    local allowed_workers="${2:-}"
    HEALTHY_WORKERS="${RCH_VERIFY_HEALTHY_WORKERS:-}" \
    ALLOWED_WORKERS="$allowed_workers" \
    FAILED_WORKER="$failed_worker" \
    RCH_BIN_PATH="$RCH_BIN" \
    python3 - <<'PY'
import json
import os
import subprocess

failed = os.environ["FAILED_WORKER"]
explicit = os.environ.get("HEALTHY_WORKERS", "")
allowed_raw = os.environ.get("ALLOWED_WORKERS", "")
allowed = [
    item.strip()
    for item in allowed_raw.split(",")
    if item.strip()
]

def emit(ids):
    seen = []
    for item in ids:
        item = item.strip()
        if allowed and item not in allowed:
            continue
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

critical_checkout_manifest() {
    GIT_LS_FILES="${RCH_VERIFY_GIT_LS_FILES:-}" \
    PROJECT_ROOT_PATH="$PROJECT_ROOT" \
    python3 - <<'PY'
import os
import subprocess

explicit = os.environ.get("GIT_LS_FILES", "")
project_root = os.environ["PROJECT_ROOT_PATH"]

if explicit:
    tracked = explicit.splitlines()
else:
    try:
        result = subprocess.run(
            ["git", "-C", project_root, "ls-files"],
            check=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
            timeout=10,
        )
        tracked = result.stdout.splitlines()
    except Exception:
        tracked = []

critical = set()
for path in tracked:
    path = path.strip()
    if not path:
        continue
    if path in {"src/lib.rs", "src/main.rs"}:
        critical.add(path)
        continue
    if path.startswith("src/") and path.endswith(".rs"):
        parts = path.split("/")
        if len(parts) == 2 or (len(parts) == 3 and parts[2] == "mod.rs"):
            critical.add(path)

for path in sorted(critical):
    print(path)
PY
}

compute_source_state_json() {
    PROJECT_ROOT_PATH="$PROJECT_ROOT" \
    REQUIRE_CLEAN_TREE="$REQUIRE_CLEAN_TREE" \
    python3 - <<'PY'
import hashlib
import json
import os
import subprocess

project_root = os.environ["PROJECT_ROOT_PATH"]
require_clean = os.environ.get("REQUIRE_CLEAN_TREE") == "1"

def git(args):
    try:
        return subprocess.run(
            ["git", "-C", project_root, *args],
            check=False,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
            timeout=10,
        )
    except Exception:
        return None

def git_stdout(args):
    result = git(args)
    if result is None or result.returncode != 0:
        return None
    return result.stdout.strip()

def path_from_porcelain_v2(line):
    if line.startswith("? ") or line.startswith("! "):
        return line[2:].strip()
    if line.startswith("#"):
        return ""
    if "\t" in line:
        return line.rsplit("\t", 1)[-1].strip()
    parts = line.split()
    return parts[-1] if parts else ""

def status_kind(line, path):
    if path == ".beads/issues.jsonl" or path.startswith(".beads/"):
        return "beads"
    if line.startswith("? "):
        name = path.rsplit("/", 1)[-1]
        if (
            path in {"--help", ".plan-drift-report.json", "critical.json", "functions.txt"}
            or name.startswith("ubs")
            or name.startswith("test_ln_")
            or name.startswith("test_multibyte")
        ):
            return "scratch"
        if any(token in path.lower() for token in ("secret", "token", "credential", "password")):
            return "secret_risk"
        return "untracked"
    if line.startswith("! "):
        return "ignored"
    return "tracked"

def tracked_state(line):
    if not (line.startswith("1 ") or line.startswith("2 ") or line.startswith("u ")):
        return False, False
    xy = line[2:4]
    if len(xy) != 2:
        return False, False
    return xy[0] != ".", xy[1] != "."

head = git_stdout(["rev-parse", "HEAD"])
tree = git_stdout(["rev-parse", "HEAD^{tree}"]) if head else None
status = git(["status", "--porcelain=v2", "--untracked-files=all", "--ignored=no"])
status_lines = []
if status is not None and status.returncode == 0:
    status_lines = [line.rstrip("\n") for line in status.stdout.splitlines() if line.strip()]

normalized = "\n".join(sorted(status_lines))
dirty_hash = "sha256:" + hashlib.sha256(normalized.encode("utf-8")).hexdigest()
summary = {
    "total": 0,
    "tracked": 0,
    "tracked_staged": 0,
    "tracked_unstaged": 0,
    "untracked": 0,
    "beads": 0,
    "scratch": 0,
    "secret_risk": 0,
    "ignored": 0,
    "unknown": 0,
}

sample = []
for line in sorted(status_lines):
    path = path_from_porcelain_v2(line)
    if not path:
        continue
    kind = status_kind(line, path)
    if kind not in summary:
        kind = "unknown"
    summary["total"] += 1
    summary[kind] += 1
    staged, unstaged = tracked_state(line)
    if kind == "tracked" and staged:
        summary["tracked_staged"] += 1
    if kind == "tracked" and unstaged:
        summary["tracked_unstaged"] += 1
    if len(sample) < 12:
        item = {"path": path, "kind": kind}
        if kind == "tracked":
            item["staged"] = staged
            item["unstaged"] = unstaged
        sample.append(item)

source_codes = []
if require_clean and summary["total"]:
    source_codes.append("rch_verify_dirty_tree_refused")
    if summary["tracked"]:
        source_codes.append("rch_verify_dirty_tracked_paths")
    if summary["tracked_staged"]:
        source_codes.append("rch_verify_dirty_staged_paths")
    if summary["tracked_unstaged"]:
        source_codes.append("rch_verify_dirty_unstaged_paths")
    if summary["beads"]:
        source_codes.append("rch_verify_dirty_beads_metadata")
    if summary["scratch"]:
        source_codes.append("rch_verify_dirty_untracked_scratch")
    if summary["untracked"] or summary["secret_risk"] or summary["unknown"]:
        source_codes.append("rch_verify_dirty_untracked_paths")

print(json.dumps({
    "verification_attribution": "strict_clean_tree" if require_clean and not summary["total"] else "live_dirty_checkout",
    "git_head": head,
    "git_tree": tree,
    "dirty_status_hash": dirty_hash,
    "dirty_summary": summary,
    "dirty_paths_sample": sample,
    "source_state_degraded_codes": source_codes,
}, sort_keys=True, separators=(",", ":")))
PY
}

compute_committed_tree_state_json() {
    PROJECT_ROOT_PATH="$PROJECT_ROOT" \
    REQUESTED_TREEISH="$TREEISH" \
    python3 - <<'PY'
import hashlib
import json
import os
import subprocess

project_root = os.environ["PROJECT_ROOT_PATH"]
treeish = os.environ.get("REQUESTED_TREEISH") or "HEAD"

def git(args):
    try:
        return subprocess.run(
            ["git", "-C", project_root, *args],
            check=False,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            timeout=10,
        )
    except Exception as error:
        return subprocess.CompletedProcess(args, 1, "", str(error))

def empty_state(codes):
    return {
        "verification_attribution": "committed_tree",
        "git_head": None,
        "git_tree": None,
        "dirty_status_hash": "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        "dirty_summary": {
            "total": 0,
            "tracked": 0,
            "untracked": 0,
            "beads": 0,
            "scratch": 0,
            "secret_risk": 0,
            "ignored": 0,
            "unknown": 0,
        },
        "dirty_paths_sample": [],
        "source_state_degraded_codes": codes,
        "requested_treeish": treeish,
        "resolved_commit": None,
        "source_manifest_hash": None,
        "source_manifest_file_count": 0,
        "source_manifest_byte_count": 0,
        "source_manifest_excluded_path_classes": ["dirty_tracked", "untracked", "ignored"],
    }

commit_result = git(["rev-parse", "--verify", "--quiet", f"{treeish}^{{commit}}"])
if commit_result.returncode != 0:
    print(json.dumps(empty_state([
        "rch_verify_committed_tree_ref_unresolved",
        "rch_verify_committed_tree_unsupported",
    ]), sort_keys=True, separators=(",", ":")))
    raise SystemExit(0)

commit = commit_result.stdout.strip()
tree_result = git(["rev-parse", "--verify", "--quiet", f"{commit}^{{tree}}"])
if tree_result.returncode != 0:
    print(json.dumps(empty_state([
        "rch_verify_committed_tree_ref_unresolved",
        "rch_verify_committed_tree_unsupported",
    ]), sort_keys=True, separators=(",", ":")))
    raise SystemExit(0)
tree = tree_result.stdout.strip()

ls_tree = subprocess.run(
    ["git", "-C", project_root, "ls-tree", "-r", "-l", "-z", "--full-tree", commit],
    check=False,
    stdout=subprocess.PIPE,
    stderr=subprocess.DEVNULL,
    timeout=10,
)

entries = []
byte_count = 0
if ls_tree.returncode == 0:
    for raw in ls_tree.stdout.split(b"\0"):
        if not raw:
            continue
        meta, _, raw_path = raw.partition(b"\t")
        parts = meta.decode("utf-8", "replace").split()
        if len(parts) < 4:
            continue
        mode, kind, object_id, size_text = parts[:4]
        path = raw_path.decode("utf-8", "replace")
        try:
            size = int(size_text)
        except ValueError:
            size = 0
        byte_count += max(size, 0)
        entries.append((path, mode, kind, object_id, size))

manifest = "\n".join(
    f"{path}\0{mode}\0{kind}\0{object_id}\0{size}"
    for path, mode, kind, object_id, size in sorted(entries)
)
manifest_hash = "sha256:" + hashlib.sha256(manifest.encode("utf-8")).hexdigest()

codes = []
show_cargo = git(["show", f"{commit}:Cargo.toml"])
if show_cargo.returncode == 0 and "path" in show_cargo.stdout and "path =" in show_cargo.stdout:
    codes.append("rch_verify_committed_tree_unsupported")
    codes.append("rch_verify_committed_tree_path_deps_unsupported")

print(json.dumps({
    "verification_attribution": "committed_tree",
    "git_head": commit,
    "git_tree": tree,
    "dirty_status_hash": "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
    "dirty_summary": {
        "total": 0,
        "tracked": 0,
        "untracked": 0,
        "beads": 0,
        "scratch": 0,
        "secret_risk": 0,
        "ignored": 0,
        "unknown": 0,
    },
    "dirty_paths_sample": [],
    "source_state_degraded_codes": codes,
    "requested_treeish": treeish,
    "resolved_commit": commit,
    "source_manifest_hash": manifest_hash,
    "source_manifest_file_count": len(entries),
    "source_manifest_byte_count": byte_count,
    "source_manifest_excluded_path_classes": ["dirty_tracked", "untracked", "ignored"],
}, sort_keys=True, separators=(",", ":")))
PY
}

json_field_string() {
    JSON_INPUT="${1:?json input required}" \
    JSON_FIELD="${2:?json field required}" \
    python3 - <<'PY'
import json
import os

payload = json.loads(os.environ["JSON_INPUT"])
value = payload.get(os.environ["JSON_FIELD"])
print("" if value is None else str(value))
PY
}

materialize_committed_tree() {
    local commit export_base export_root short_commit
    commit="$(json_field_string "$SOURCE_STATE_JSON" "resolved_commit")"
    if [ -z "$commit" ]; then
        echo "rch_verify: committed-tree materialization missing resolved commit" >&2
        return 1
    fi

    short_commit="${commit:0:12}"
    export_base="${RCH_VERIFY_COMMITTED_TREE_BASE:-${TMPDIR:-/tmp}/ee-rch-committed-tree}"
    mkdir -p "$export_base"
    export_root="$(mktemp -d "$export_base/$short_commit.XXXXXX")"

    git -C "$PROJECT_ROOT" archive --format=tar "$commit" | tar -x -f - -C "$export_root"
    PROJECT_ROOT="$export_root"
    REMOTE_PROJECT_ROOT="/data/projects/$(basename "$PROJECT_ROOT")"
    REMOTE_PROJECT_ROOT_JSON="$(json_quote "$REMOTE_PROJECT_ROOT")"
}

remote_checkout_missing_tracked_paths() {
    CHECKOUT_OUTPUT="${1:-}" \
    CRITICAL_MANIFEST="$(critical_checkout_manifest)" \
    python3 - <<'PY'
import os
import re

ansi = re.compile(r"\x1b\[[0-9;]*m")
text = ansi.sub("", os.environ.get("CHECKOUT_OUTPUT", ""))
manifest = {
    line.strip()
    for line in os.environ.get("CRITICAL_MANIFEST", "").splitlines()
    if line.strip()
}

if "E0583" not in text:
    raise SystemExit(0)

candidates = []
for match in re.finditer(r'"(src/[^"]+\.rs)"', text):
    candidates.append(match.group(1))

missing = []
for path in candidates:
    if path in manifest and path not in missing:
        missing.append(path)

print(",".join(missing))
PY
}

run_rch_invocation_once() {
    if [ -n "${RCH_VERIFY_FAKE_OUTPUT:-}" ]; then
        printf '%s' "$RCH_VERIFY_FAKE_OUTPUT"
        return "${RCH_VERIFY_FAKE_EXIT_CODE:-0}"
    fi

    run_process_with_timeout \
        "$RCH_ATTEMPT_STDOUT_FILE" \
        "$RCH_ATTEMPT_STDERR_FILE" \
        "$RCH_ATTEMPT_META_FILE" \
        "$PROJECT_ROOT" \
        env \
        "RCH_WORKERS=${RCH_WORKERS:-}" \
        "RCH_COMPRESSION=${RCH_COMPRESSION:-0}" \
        "RCH_REQUIRE_REMOTE=1" \
        "RCH_QUEUE_WHEN_BUSY=${RCH_QUEUE_WHEN_BUSY:-1}" \
        "RCH_TEST_SLOTS=${RCH_TEST_SLOTS:-2}" \
        "RCH_DAEMON_WAIT_RESPONSE_TIMEOUT_SECS=${RCH_DAEMON_WAIT_RESPONSE_TIMEOUT_SECS:-900}" \
        "RCH_DAEMON_RESPONSE_TIMEOUT_SECS=${RCH_DAEMON_RESPONSE_TIMEOUT_SECS:-900}" \
        "RCH_CANONICAL_PROJECT_ROOT=${RCH_CANONICAL_PROJECT_ROOT:-$(dirname "$PROJECT_ROOT")}" \
        "RCH_ALIAS_PROJECT_ROOT=${RCH_ALIAS_PROJECT_ROOT:-/data/projects}" \
        "RCH_VISIBILITY=${RCH_VISIBILITY:-summary}" \
        "${RCH_INVOCATION[@]}"
    local status=$?
    cat "$RCH_ATTEMPT_STDOUT_FILE"
    cat "$RCH_ATTEMPT_STDERR_FILE"
    return "$status"
}

run_rch_invocation_retry() {
    local preferred_workers="${1:?preferred workers required}"
    if [ -n "${RCH_VERIFY_FAKE_RETRY_OUTPUT:-}" ]; then
        printf '%s' "$RCH_VERIFY_FAKE_RETRY_OUTPUT"
        return "${RCH_VERIFY_FAKE_RETRY_EXIT_CODE:-0}"
    fi

    run_process_with_timeout \
        "$RCH_ATTEMPT_STDOUT_FILE" \
        "$RCH_ATTEMPT_STDERR_FILE" \
        "$RCH_ATTEMPT_META_FILE" \
        "$PROJECT_ROOT" \
        env \
        "RCH_WORKERS=$preferred_workers" \
        "RCH_COMPRESSION=${RCH_COMPRESSION:-0}" \
        "RCH_REQUIRE_REMOTE=1" \
        "RCH_QUEUE_WHEN_BUSY=${RCH_QUEUE_WHEN_BUSY:-1}" \
        "RCH_TEST_SLOTS=${RCH_TEST_SLOTS:-2}" \
        "RCH_DAEMON_WAIT_RESPONSE_TIMEOUT_SECS=${RCH_DAEMON_WAIT_RESPONSE_TIMEOUT_SECS:-900}" \
        "RCH_DAEMON_RESPONSE_TIMEOUT_SECS=${RCH_DAEMON_RESPONSE_TIMEOUT_SECS:-900}" \
        "RCH_CANONICAL_PROJECT_ROOT=${RCH_CANONICAL_PROJECT_ROOT:-$(dirname "$PROJECT_ROOT")}" \
        "RCH_ALIAS_PROJECT_ROOT=${RCH_ALIAS_PROJECT_ROOT:-/data/projects}" \
        "RCH_VISIBILITY=${RCH_VISIBILITY:-summary}" \
        "${RCH_INVOCATION[@]}"
    local status=$?
    cat "$RCH_ATTEMPT_STDOUT_FILE"
    cat "$RCH_ATTEMPT_STDERR_FILE"
    return "$status"
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
    local command_json rch_invocation_json command_text_json remote_env_json stdout_json stderr_json requested_workers_json configured_workers_json daemon_workers_json build_admission_json rch_runtime_json known_blocker_json local_cargo_processes_json
    command_json="$(json_array "${COMMAND[@]}")"
    rch_invocation_json="$(json_array "${RCH_INVOCATION[@]}")"
    remote_env_json="$(json_array "${ENV_OVERRIDES[@]}")"
    command_text_json="$(json_quote "$(command_string "${ENV_OVERRIDES[@]}" "${COMMAND[@]}")")"
    stdout_json="$(json_quote "$stdout_tail")"
    stderr_json="$(json_quote "$stderr_tail")"
    requested_workers_json="$(csv_json_array "${REQUESTED_WORKERS_CSV:-}")"
    configured_workers_json="$(csv_json_array "${CONFIGURED_WORKERS_CSV:-}")"
    daemon_workers_json="$(csv_json_array "${DAEMON_WORKERS_CSV:-}")"
    build_admission_json="${BUILD_ADMISSION_JSON:-$(json_object_not_run)}"
    local_cargo_processes_json="${LOCAL_CARGO_PROCESSES_JSON:-$(local_cargo_processes_not_run_json)}"
    if [ -n "${RCH_RUNTIME_JSON:-}" ]; then
        rch_runtime_json="$RCH_RUNTIME_JSON"
    else
        rch_runtime_json='{"status":"not_checked","client_path":null,"client_version":null,"client_compat":null,"daemon_version":null,"daemon_compat":null,"daemon_socket_path":null,"message":null}'
    fi
    known_blocker_json="${KNOWN_BLOCKER_JSON:-null}"
    local source_state_json
    if [ -n "${SOURCE_STATE_JSON:-}" ]; then
        source_state_json="$SOURCE_STATE_JSON"
    else
        source_state_json='{"verification_attribution":"live_dirty_checkout","git_head":null,"git_tree":null,"dirty_status_hash":"sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855","dirty_summary":{"total":0,"tracked":0,"untracked":0,"beads":0,"scratch":0,"secret_risk":0,"ignored":0,"unknown":0},"dirty_paths_sample":[],"source_state_degraded_codes":[]}'
    fi
    local json_payload
    local artifacts_json artifact_args=() artifact_index
    for artifact_index in "${!RCH_ARTIFACT_KINDS[@]}"; do
        artifact_args+=(
            "${RCH_ARTIFACT_KINDS[$artifact_index]}"
            "${RCH_ARTIFACT_PATHS[$artifact_index]}"
            "${RCH_ARTIFACT_ATTEMPTS[$artifact_index]}"
        )
    done
    artifacts_json="$(attempt_artifacts_json "${artifact_args[@]}")"
    json_payload="$(cat <<EOF
{"schema":"ee.rch.verify.v1","success":$success,"generated_at":"$(now_iso)","command":$command_json,"command_text":$command_text_json,"command_kind":"$COMMAND_KIND","remote_env":$remote_env_json,"remote_required":true,"would_offload":$WOULD_OFFLOAD,"worker_id":$WORKER_ID_JSON,"requested_workers":$requested_workers_json,"configured_workers":$configured_workers_json,"daemon_workers":$daemon_workers_json,"remote_project_root":$REMOTE_PROJECT_ROOT_JSON,"remote_target_dir":$REMOTE_TARGET_DIR_JSON,"exit_code":$exit_code_json,"elapsed_ms":$elapsed_ms,"attempt_timeout_ms":$RCH_VERIFY_ATTEMPT_TIMEOUT_MS,"timed_out":$RCH_ATTEMPT_TIMED_OUT,"stdout_bytes":$RCH_STDOUT_BYTES,"stderr_bytes":$RCH_STDERR_BYTES,"stdout_tail":$stdout_json,"stderr_tail":$stderr_json,"artifacts":$artifacts_json,"degraded_codes":$degraded_codes_json,"rch_invocation":$rch_invocation_json,"build_admission":$build_admission_json,"rch_runtime":$rch_runtime_json,"known_blocker":$known_blocker_json,"local_cargo_processes":$local_cargo_processes_json,"source_state":$source_state_json}
EOF
)"
    JSON_PAYLOAD="$json_payload" \
    BEAD_ID="$BEAD_ID" \
    LEDGER_PATH="$LEDGER_PATH" \
    EVENT_LOG_PATH="$EVENT_LOG_PATH" \
    INCLUDE_SUMMARY="$INCLUDE_SUMMARY" \
    NO_WRITE="$NO_WRITE" \
    KNOWN_BLOCKER_ENABLED="$KNOWN_BLOCKER_ENABLED" \
    KNOWN_BLOCKER_STORE_PATH="$KNOWN_BLOCKER_STORE" \
    KNOWN_BLOCKER_STORE_EXPLICIT="$KNOWN_BLOCKER_STORE_EXPLICIT" \
    KNOWN_BLOCKER_TTL_SECONDS="$KNOWN_BLOCKER_TTL_SECONDS" \
    KNOWN_BLOCKER_MAX_ENTRIES="$KNOWN_BLOCKER_MAX_ENTRIES" \
    KNOWN_BLOCKER_FAKE_OUTPUT_PRESENT="${RCH_VERIFY_FAKE_OUTPUT:+1}" \
    RUN_STARTED_AT="$RUN_STARTED_AT" \
    python3 - <<'PY'
import datetime as dt
import hashlib
import json
import os
import re
from pathlib import Path

proof = json.loads(os.environ["JSON_PAYLOAD"])
source_state = proof.pop("source_state", {})
for key in (
    "verification_attribution",
    "git_head",
    "git_tree",
    "dirty_status_hash",
    "dirty_summary",
    "dirty_paths_sample",
    "source_state_degraded_codes",
    "requested_treeish",
    "resolved_commit",
    "source_manifest_hash",
    "source_manifest_file_count",
    "source_manifest_byte_count",
    "source_manifest_excluded_path_classes",
):
    proof[key] = source_state.get(key)
bead_id = os.environ.get("BEAD_ID", "")
ledger_path = os.environ.get("LEDGER_PATH", "")
event_log_path = os.environ.get("EVENT_LOG_PATH", "")
include_summary = os.environ.get("INCLUDE_SUMMARY") == "1"
no_write = os.environ.get("NO_WRITE") == "1"
started_at = os.environ.get("RUN_STARTED_AT") or proof.get("generated_at")

def redact(text):
    if not text:
        return text
    text = re.sub(r"\x1b\[[0-9;]*m", "", text)
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

def cargo_path_dependency_version_details(text):
    if not text:
        return None
    requirement = re.search(
        r"failed to select a version for the requirement `([^`=]+?)\s*=\s*\"([^\"]+)\"`",
        text,
    )
    candidates = re.search(
        r"candidate versions found which didn't match:\s*([^\n]+)",
        text,
    )
    location = re.search(r"location searched:\s*([^\n]+)", text)
    if not (requirement and candidates and location):
        return None
    candidate_versions = [
        item.strip()
        for item in candidates.group(1).split(",")
        if item.strip()
    ]
    return {
        "crate": requirement.group(1).strip(),
        "required": requirement.group(2).strip(),
        "candidate_versions": candidate_versions,
        "location_searched": redact(location.group(1).strip()),
    }

def cargo_workspace_inheritance_details(text):
    if not text:
        return None
    dependency = re.search(
        r"failed to load manifest for dependency `([^`]+)`",
        text,
    )
    manifest = re.search(
        r"failed to parse manifest at `([^`]+)`",
        text,
    )
    inherited = re.search(
        r"error inheriting `([^`]+)` from workspace root manifest's `([^`]+)`",
        text,
    )
    missing = re.search(
        r"`(workspace\.package\.[^`]+)` was not defined",
        text,
    )
    if not (inherited and missing):
        return None
    details = {
        "inherited_field": inherited.group(1).strip(),
        "workspace_field": inherited.group(2).strip(),
        "missing_workspace_field": missing.group(1).strip(),
    }
    if dependency:
        details["dependency"] = dependency.group(1).strip()
    if manifest:
        details["manifest_path"] = redact(manifest.group(1).strip())
    return details

def sync_closure_root_counts(text):
    if not text:
        return []
    counts = []
    for line in text.splitlines():
        match = re.search(
            r"Prepared dependency sync manifest for\s+(\d+)\s+roots?\b",
            line,
            flags=re.IGNORECASE,
        )
        if match:
            counts.append({
                "root_count": int(match.group(1)),
                "line": redact(line.strip()),
            })
    return counts

def parse_time(value):
    if not value:
        return None
    text = str(value)
    if text.endswith("Z"):
        text = text[:-1] + "+00:00"
    try:
        parsed = dt.datetime.fromisoformat(text)
    except ValueError:
        return None
    if parsed.tzinfo is None:
        parsed = parsed.replace(tzinfo=dt.timezone.utc)
    return parsed.astimezone(dt.timezone.utc)

def format_time(value):
    return value.astimezone(dt.timezone.utc).isoformat(timespec="microseconds").replace("+00:00", "Z")

def csv_fingerprint(values):
    return [str(item) for item in values or []]

def blocker_kind_for(degraded_codes):
    if "rch_verify_cargo_workspace_inheritance_blocked" in degraded_codes:
        return "cargo_workspace_inheritance"
    if "rch_verify_cargo_path_dependency_version_blocked" in degraded_codes:
        return "cargo_path_dependency_version"
    if "rch_verify_client_daemon_version_skew" in degraded_codes:
        return "client_daemon_version_skew"
    if "rch_verify_remote_checkout_incomplete" in degraded_codes:
        return "remote_checkout_incomplete"
    if "rch_verify_worker_disk_full" in degraded_codes:
        return "worker_disk_full"
    if "rch_verify_all_workers_preflight_failed" in degraded_codes:
        return "all_workers_preflight_failed"
    if "rch_verify_capacity_or_timeout" in degraded_codes:
        return "capacity_or_timeout"
    if "rch_verify_topology_blocked" in degraded_codes:
        return "topology_blocked"
    if "rch_verify_local_fallback_refused" in degraded_codes:
        return "local_fallback_refused"
    return None

def selector_admission_probe(proof, degraded_codes, combined_tail):
    command_kind = proof.get("command_kind") or ""
    required_runtime = "Rust" if command_kind.startswith("cargo_") else None
    workers_reported = [str(item) for item in proof.get("configured_workers") or []]
    daemon_workers_reported = [str(item) for item in proof.get("daemon_workers") or []]
    selected_worker = proof.get("worker_id")
    local_fallback_refused = (
        "rch_verify_local_fallback_refused" in degraded_codes
        or "remote required; refusing local fallback" in combined_tail
    )
    path_warning = None
    for line in combined_tail.splitlines():
        lowered = line.lower()
        if (
            ("normaliz" in lowered or "canonical" in lowered or "alias" in lowered or "project root" in lowered)
            and ("rch" in lowered or "project" in lowered or "path" in lowered)
        ):
            path_warning = redact(line.strip())
            break

    selection_failure_reason = None
    if required_runtime is None:
        status = "not_applicable"
    elif selected_worker:
        status = "selected"
    else:
        status = "selection_failed"
        lowered_tail = combined_tail.lower()
        if "no workers with rust installed" in lowered_tail:
            selection_failure_reason = "no_workers_with_rust_installed"
        elif "rch_verify_topology_blocked" in degraded_codes or "RCH-E327" in combined_tail:
            selection_failure_reason = "topology_blocked"
        elif "rch_verify_all_workers_preflight_failed" in degraded_codes:
            selection_failure_reason = "all_workers_preflight_failed"
        elif "rch_verify_capacity_or_timeout" in degraded_codes:
            selection_failure_reason = "capacity_or_timeout"
        elif "rch_verify_not_offloaded" in degraded_codes:
            selection_failure_reason = "command_not_offloaded"
        elif "rch_verify_remote_marker_missing" in degraded_codes:
            selection_failure_reason = "remote_marker_missing"
        else:
            selection_failure_reason = "no_worker_selected"

    return {
        "schema": "ee.rch.selector_admission_probe.v1",
        "status": status,
        "required_runtime": required_runtime,
        "workers_reported": workers_reported,
        "daemon_workers_reported": daemon_workers_reported,
        "workers_reported_count": len(workers_reported),
        "daemon_workers_reported_count": len(daemon_workers_reported),
        "selected_worker": selected_worker,
        "selection_failure_reason": selection_failure_reason,
        "workers_vs_selection_contradiction": bool(
            required_runtime
            and not selected_worker
            and (workers_reported or daemon_workers_reported)
            and selection_failure_reason in {
                "no_workers_with_rust_installed",
                "no_worker_selected",
                "remote_marker_missing",
            }
        ),
        "path_normalization_warning": path_warning,
        "remote_required": proof.get("remote_required") is True,
        "local_fallback_refused": bool(local_fallback_refused),
    }

def remediation_bead_for(blocker_kind):
    mapping = {
        "cargo_workspace_inheritance": "bd-17c65.10.17.1.3",
        "cargo_path_dependency_version": "bd-17c65.10.17.1.3",
        "client_daemon_version_skew": "bd-17c65.10.17.1.4",
        "remote_checkout_incomplete": "bd-17c65.10.17.1.3",
        "worker_disk_full": "bd-17c65.10.17",
        "all_workers_preflight_failed": "bd-17c65.10.19",
        "capacity_or_timeout": "bd-17c65.10.17",
        "topology_blocked": "bd-17c65.10.17.1.2",
        "local_fallback_refused": "bd-17c65.10.17.1",
    }
    return mapping.get(blocker_kind, "bd-17c65.10.17.1")

def known_blocker_entry(blocker_kind, degraded_codes, command_hash):
    source_state_hash = proof.get("source_manifest_hash") or proof.get("dirty_status_hash")
    runtime = proof.get("rch_runtime") or {}
    details = proof.get("cargo_workspace_inheritance") or proof.get("cargo_path_dependency_version") or {}
    normalized_argv_hash = "sha256:" + hashlib.sha256(
        json.dumps(proof.get("command") or [], separators=(",", ":")).encode("utf-8")
    ).hexdigest()
    runtime_fingerprint = {
        "client_compat": runtime.get("client_compat"),
        "daemon_compat": runtime.get("daemon_compat"),
        "status": runtime.get("status"),
    }
    fingerprint_inputs = {
        "blocker_kind": blocker_kind,
        "degraded_codes": sorted(
            code
            for code in degraded_codes
            if code != "rch_verify_remote_command_failed"
        ),
        "source_state_hash": source_state_hash,
        "source_manifest_hash": proof.get("source_manifest_hash"),
        "verifier_source_mode": proof.get("verification_attribution"),
        "command_kind": proof.get("command_kind"),
        "command_hash": command_hash,
        "normalized_argv_hash": normalized_argv_hash,
        "requested_workers": csv_fingerprint(proof.get("requested_workers")),
        "configured_workers": csv_fingerprint(proof.get("configured_workers")),
        "runtime_fingerprint": runtime_fingerprint,
        "dependency": details.get("dependency") or details.get("crate"),
        "manifest_path": details.get("manifest_path") or details.get("location_searched"),
    }
    fingerprint_payload = json.dumps(fingerprint_inputs, sort_keys=True, separators=(",", ":"))
    now = parse_time(proof.get("generated_at")) or dt.datetime.now(dt.timezone.utc)
    try:
        ttl_seconds = int(os.environ.get("KNOWN_BLOCKER_TTL_SECONDS") or "21600")
    except ValueError:
        ttl_seconds = 21600
    if ttl_seconds < 60:
        ttl_seconds = 60
    expires_at = now + dt.timedelta(seconds=ttl_seconds)
    return {
        "schema": "ee.rch.known_blocker.v1",
        "blocker_fingerprint": "sha256:" + hashlib.sha256(fingerprint_payload.encode("utf-8")).hexdigest(),
        "blocker_kind": blocker_kind,
        "degraded_codes": sorted(dict.fromkeys(degraded_codes)),
        "source_state_hash": source_state_hash,
        "source_manifest_hash": proof.get("source_manifest_hash"),
        "verifier_source_mode": proof.get("verification_attribution"),
        "command_kind": proof.get("command_kind"),
        "command_hash": command_hash,
        "normalized_argv_hash": normalized_argv_hash,
        "requested_workers": csv_fingerprint(proof.get("requested_workers")),
        "configured_workers": csv_fingerprint(proof.get("configured_workers")),
        "runtime_fingerprint": runtime_fingerprint,
        "dependency": details.get("dependency") or details.get("crate"),
        "manifest_path": details.get("manifest_path") or details.get("location_searched"),
        "first_seen": format_time(now),
        "last_seen": format_time(now),
        "expires_at": format_time(expires_at),
        "retry_after": format_time(expires_at),
        "remediation_bead": remediation_bead_for(blocker_kind),
        "override_used": False,
    }

def persist_known_blocker(entry):
    if os.environ.get("KNOWN_BLOCKER_ENABLED") != "1":
        return entry
    if os.environ.get("NO_WRITE") == "1":
        entry = dict(entry)
        entry["write_suppressed"] = True
        return entry
    if os.environ.get("KNOWN_BLOCKER_FAKE_OUTPUT_PRESENT") and not os.environ.get("KNOWN_BLOCKER_STORE_EXPLICIT"):
        return entry
    store_path = os.environ.get("KNOWN_BLOCKER_STORE_PATH") or ""
    if not store_path:
        return entry
    path = Path(store_path)
    now = parse_time(entry.get("last_seen")) or dt.datetime.now(dt.timezone.utc)
    try:
        max_entries = int(os.environ.get("KNOWN_BLOCKER_MAX_ENTRIES") or "128")
    except ValueError:
        max_entries = 128
    if max_entries < 1:
        max_entries = 1
    records = []
    if path.exists():
        try:
            for line in path.read_text(encoding="utf-8").splitlines():
                if not line.strip():
                    continue
                try:
                    record = json.loads(line)
                except Exception:
                    continue
                expires_at = parse_time(record.get("expires_at"))
                if expires_at is not None and expires_at > now:
                    records.append(record)
        except OSError:
            return entry
    merged = []
    prior_first_seen = None
    for record in records:
        if record.get("blocker_fingerprint") == entry.get("blocker_fingerprint"):
            prior_first_seen = record.get("first_seen") or prior_first_seen
            continue
        merged.append(record)
    if prior_first_seen:
        entry = dict(entry)
        entry["first_seen"] = prior_first_seen
    merged.append(entry)
    merged.sort(key=lambda item: item.get("last_seen") or item.get("first_seen") or "")
    merged = merged[-max_entries:]
    try:
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(
            "".join(json.dumps(record, sort_keys=True, separators=(",", ":")) + "\n" for record in merged),
            encoding="utf-8",
        )
    except OSError as error:
        entry = dict(entry)
        entry["write_error"] = redact(str(error))
    return entry

raw_stdout_tail = proof.get("stdout_tail") or ""
raw_stderr_tail = proof.get("stderr_tail") or ""
combined_tail = "\n".join(part for part in [raw_stdout_tail, raw_stderr_tail] if part)
proof["stdout_tail"] = redact(raw_stdout_tail)
proof["stderr_tail"] = redact(raw_stderr_tail)
first_error_file, first_error_line = first_error_location(combined_tail)
codes = error_codes(combined_tail)
cargo_workspace_inheritance = cargo_workspace_inheritance_details(combined_tail)
cargo_path_dependency_version = cargo_path_dependency_version_details(combined_tail)
sync_closure_counts = sync_closure_root_counts(combined_tail)
if cargo_workspace_inheritance:
    proof["cargo_workspace_inheritance"] = cargo_workspace_inheritance
if cargo_path_dependency_version:
    proof["cargo_path_dependency_version"] = cargo_path_dependency_version
if sync_closure_counts:
    proof["sync_closure"] = {
        "source": "rch_transcript",
        "last_root_count": sync_closure_counts[-1]["root_count"],
        "root_counts": sync_closure_counts,
    }

exit_code = proof.get("exit_code")
degraded = list(proof.get("degraded_codes") or [])
source_state_degraded = list(proof.get("source_state_degraded_codes") or [])
source_state_code_set = set(source_state_degraded)
local_cargo_processes = proof.get("local_cargo_processes") or {}
try:
    local_cargo_process_count = int(local_cargo_processes.get("count") or 0)
except (TypeError, ValueError):
    local_cargo_process_count = 0
if local_cargo_process_count > 0 and "rch_verify_local_cargo_processes_present" not in degraded:
    degraded.append("rch_verify_local_cargo_processes_present")
    proof["degraded_codes"] = degraded

worker_state_code_set = {
    "rch_verify_known_blocker_active",
    "rch_verify_capacity_or_timeout",
    "rch_verify_local_fallback_refused",
    "rch_verify_not_offloaded",
    "rch_verify_remote_checkout_incomplete",
    "rch_verify_remote_marker_missing",
    "rch_verify_retry_after_worker_disk_full",
    "rch_verify_topology_blocked",
    "rch_verify_all_workers_preflight_failed",
    "rch_verify_worker_disk_full",
    "rch_verify_worker_filter_ignored",
    "rch_verify_worker_quarantine_ignored",
    "rch_verify_cargo_workspace_inheritance_blocked",
    "rch_verify_cargo_path_dependency_version_blocked",
    "rch_verify_client_daemon_version_skew",
}
worker_state_degraded = [
    code
    for code in degraded
    if code in worker_state_code_set and code not in source_state_code_set
]
proof["selector_admission_probe"] = selector_admission_probe(proof, degraded, combined_tail)
if proof.get("success") is not True:
    status = "refused"
elif "rch_verify_known_blocker_active" in degraded:
    status = "known_blocker_refused"
    proof["verification_attribution"] = "not_run_known_blocker"
elif exit_code is None:
    status = "dry_run"
elif exit_code == 0 and proof.get("worker_id"):
    status = "remote_pass"
elif exit_code == 0:
    status = "pass_without_remote_marker"
elif "rch_verify_committed_tree_unsupported" in degraded:
    status = "committed_tree_unsupported"
elif "rch_verify_build_admission_denied" in degraded:
    status = "build_admission_refused"
elif (
    "rch_verify_dirty_tree_refused" in degraded
):
    status = "source_state_refused"
elif (
    "rch_verify_topology_blocked" in degraded
    or "rch_verify_cargo_workspace_inheritance_blocked" in degraded
    or "rch_verify_cargo_path_dependency_version_blocked" in degraded
    or "rch_verify_client_daemon_version_skew" in degraded
    or "rch_verify_local_fallback_refused" in degraded
    or "rch_verify_all_workers_preflight_failed" in degraded
    or "rch_verify_worker_disk_full" in degraded
    or "rch_verify_worker_quarantine_ignored" in degraded
    or "rch_verify_worker_filter_ignored" in degraded
    or "rch_verify_remote_checkout_incomplete" in degraded
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
proof["worker_state_degraded_codes"] = worker_state_degraded
if bead_id:
    proof["bead_id"] = bead_id
build_admission = proof.get("build_admission") or {}

if proof.get("known_blocker") in (None, "null"):
    proof["known_blocker"] = None
known_blocker = proof.get("known_blocker")
if status == "rch_environment_failure" and not isinstance(known_blocker, dict):
    blocker_kind = blocker_kind_for(degraded)
    if blocker_kind:
        proof["known_blocker"] = persist_known_blocker(
            known_blocker_entry(blocker_kind, degraded, command_hash)
        )

summary_lines = [
    f"RCH verifier `{command_text}` => `{status}`.",
    f"- command_kind: `{proof.get('command_kind')}`",
    f"- verification_attribution: `{proof.get('verification_attribution')}`",
    f"- git_head: `{proof.get('git_head') or 'unknown'}`",
    f"- git_tree: `{proof.get('git_tree') or 'unknown'}`",
    f"- dirty_status_hash: `{proof.get('dirty_status_hash') or 'unknown'}`",
    f"- remote_env: `{', '.join(proof.get('remote_env') or []) or 'none'}`",
    f"- remote_required: `{str(proof.get('remote_required')).lower()}`",
    f"- would_offload: `{str(proof.get('would_offload')).lower()}`",
    f"- worker_id: `{proof.get('worker_id') or 'unknown'}`",
    f"- exit_code: `{exit_code if exit_code is not None else 'not_run'}`",
    f"- elapsed_ms: `{proof.get('elapsed_ms')}`",
    f"- command_hash: `{command_hash}`",
]
if build_admission.get("status") not in (None, "not_run"):
    summary_lines.append(
        f"- build_admission: `{build_admission.get('status')}`"
        f" admitted=`{build_admission.get('admitted')}`"
    )
runtime = proof.get("rch_runtime") or {}
if runtime.get("status") not in (None, "not_checked"):
    summary_lines.append(
        f"- rch_runtime: `{runtime.get('status')}`"
        f" client=`{runtime.get('client_version') or 'unknown'}`"
        f" daemon=`{runtime.get('daemon_version') or 'unknown'}`"
    )
local_cargo_status = local_cargo_processes.get("status")
if local_cargo_status not in (None, "not_run"):
    local_cargo_lock_count = sum(
        1
        for process in local_cargo_processes.get("processes") or []
        if process.get("packageCacheLockHeld") is True
        or process.get("packageCacheLockState") == "held"
    )
    summary_lines.append(
        f"- local_cargo_processes: `{local_cargo_status}`"
        f" count=`{local_cargo_process_count}`"
        f" package_cache_locks=`{local_cargo_lock_count}`"
    )
for key in ("requested_workers", "configured_workers", "daemon_workers"):
    workers = proof.get(key) or []
    if workers:
        summary_lines.append(f"- {key}: `{', '.join(workers)}`")
selector_probe = proof.get("selector_admission_probe") or {}
if selector_probe.get("status") not in (None, "not_applicable"):
    summary_lines.append(
        f"- selector_admission: `{selector_probe.get('status')}`"
        f" required_runtime=`{selector_probe.get('required_runtime') or 'none'}`"
        f" selected_worker=`{selector_probe.get('selected_worker') or 'none'}`"
        f" failure_reason=`{selector_probe.get('selection_failure_reason') or 'none'}`"
        f" local_fallback_refused=`{str(bool(selector_probe.get('local_fallback_refused'))).lower()}`"
    )
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
if source_state_degraded:
    summary_lines.append("- source_state_degraded_codes: `" + "`, `".join(source_state_degraded) + "`")
if worker_state_degraded:
    summary_lines.append("- worker_state_degraded_codes: `" + "`, `".join(worker_state_degraded) + "`")
if proof.get("requested_treeish"):
    summary_lines.append(f"- requested_treeish: `{proof.get('requested_treeish')}`")
if proof.get("source_manifest_hash"):
    summary_lines.append(f"- source_manifest_hash: `{proof.get('source_manifest_hash')}`")
known_blocker = proof.get("known_blocker") or {}
if isinstance(known_blocker, dict) and known_blocker.get("blocker_fingerprint"):
    summary_lines.append(f"- known_blocker: `{known_blocker.get('blocker_fingerprint')}`")
    summary_lines.append(f"- remediation_bead: `{known_blocker.get('remediation_bead') or 'unknown'}`")
    summary_lines.append(f"- retry_after: `{known_blocker.get('retry_after') or 'unknown'}`")
    summary_lines.append(f"- known_blocker_override_used: `{str(bool(known_blocker.get('override_used'))).lower()}`")
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
            "source_state_degraded_codes": proof.get("source_state_degraded_codes") or [],
            "worker_state_degraded_codes": proof.get("worker_state_degraded_codes") or [],
            "known_blocker": proof.get("known_blocker"),
            "error_codes": codes,
            "summary_markdown": summary,
        }
        path = Path(ledger_path)
        path.parent.mkdir(parents=True, exist_ok=True)
        with path.open("a", encoding="utf-8") as handle:
            handle.write(json.dumps(row, sort_keys=True, separators=(",", ":")) + "\n")

proof_json = json.dumps(proof, sort_keys=True, separators=(",", ":"))

if event_log_path:
    fake_invocation_count = 0
    fake_invocations_path = os.environ.get("FAKE_RCH_INVOCATIONS", "")
    if fake_invocations_path:
        fake_path = Path(fake_invocations_path)
        if fake_path.exists():
            fake_invocation_count = len(fake_path.read_text(encoding="utf-8").splitlines())

    def artifact_path(kind):
        for artifact in proof.get("artifacts") or []:
            if artifact.get("kind") == kind:
                return artifact.get("path")
        return None

    event = {
        "schema": "ee.test_event.v1",
        "ts": proof.get("generated_at"),
        "test_id": bead_id or "rch_verify",
        "kind": "command_end",
        "command": "scripts/rch_verify.sh",
        "args": proof.get("command") or [],
        "stdout_hash": "sha256:" + hashlib.sha256(proof_json.encode("utf-8")).hexdigest(),
        "stderr_excerpt": proof.get("stderr_tail") or "",
        "exit_code": int(proof.get("exit_code") or 0),
        "elapsed_ms": proof.get("elapsed_ms") or 0,
        "fields": {
            "bead_id": bead_id or None,
            "status": status,
            "command_hash": command_hash,
            "cwd": redact(os.getcwd()),
            "git_head": proof.get("git_head"),
            "git_tree": proof.get("git_tree"),
            "dirty_status_hash": proof.get("dirty_status_hash"),
            "verification_attribution": proof.get("verification_attribution"),
            "source_state_degraded_codes": proof.get("source_state_degraded_codes") or [],
            "worker_state_degraded_codes": proof.get("worker_state_degraded_codes") or [],
            "build_admission_status": build_admission.get("status"),
            "build_admission_admitted": build_admission.get("admitted"),
            "rch_runtime": proof.get("rch_runtime"),
            "selector_admission_probe": proof.get("selector_admission_probe"),
            "local_cargo_process_status": local_cargo_processes.get("status"),
            "local_cargo_process_count": local_cargo_process_count,
            "fake_rch_invoked": fake_invocation_count > 0,
            "fake_rch_invocation_count": fake_invocation_count,
            "source_manifest_hash": proof.get("source_manifest_hash"),
            "known_blocker": proof.get("known_blocker"),
            "stdout_artifact_path": artifact_path("stdout"),
            "stderr_artifact_path": artifact_path("stderr"),
            "schema_validation_status": "not_run",
            "deterministic_rerun_hash": proof.get("source_manifest_hash") or proof.get("dirty_status_hash"),
            "first_failure_diagnosis": status,
        },
    }
    path = Path(event_log_path)
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("a", encoding="utf-8") as handle:
        handle.write(json.dumps(event, sort_keys=True, separators=(",", ":")) + "\n")

print(proof_json)
PY
}

positive_integer_or_die "RCH_VERIFY_ATTEMPT_TIMEOUT_MS" "$RCH_VERIFY_ATTEMPT_TIMEOUT_MS"
positive_integer_or_die "RCH_VERIFY_PREFLIGHT_TIMEOUT_MS" "$RCH_VERIFY_PREFLIGHT_TIMEOUT_MS"
positive_integer_or_die "RCH_VERIFY_TAIL_BYTES" "$RCH_VERIFY_TAIL_BYTES"

COMMAND_KIND="$(classify_command)"
WOULD_OFFLOAD=false
WORKER_ID_JSON=null
REMOTE_PROJECT_ROOT="/data/projects/eidetic_engine_cli"
REMOTE_TARGET_DIR="/tmp/ee-rch-verify-target"
REMOTE_PROJECT_ROOT_JSON="$(json_quote "$REMOTE_PROJECT_ROOT")"
REMOTE_TARGET_DIR_JSON="$(json_quote "$REMOTE_TARGET_DIR")"
REQUESTED_WORKERS_CSV="${RCH_WORKERS:-}"
CONFIGURED_WORKERS_CSV=""
DAEMON_WORKERS_CSV=""
BUILD_ADMISSION_JSON="$(json_object_not_run)"
LOCAL_CARGO_PROCESSES_JSON="$(compute_local_cargo_processes_json)"

if contains_forbidden_text "${COMMAND[@]}"; then
    RCH_INVOCATION=()
    emit_json false null 0 "" "refused forbidden command text" "rch_verify_refused_forbidden_command"
    exit 2
fi

if [ "${RCH_VERIFY_PRINT_CRITICAL_MANIFEST:-0}" = "1" ]; then
    critical_checkout_manifest
    exit 0
fi

if [ "$COMMAND_KIND" = "rejected" ]; then
    RCH_INVOCATION=()
    emit_json false null 0 "" "unsupported verification command; pass --allow-raw for an explicitly raw remote command" "rch_verify_refused_unknown_command"
    exit 2
fi

if [ "$COMMITTED_TREE" -eq 1 ] && [ "$REQUIRE_CLEAN_TREE" -eq 1 ]; then
    RCH_INVOCATION=()
    emit_json false null 0 "" "choose either --committed-tree or --require-clean-tree, not both" "rch_verify_refused_conflicting_source_modes"
    exit 2
fi

if [ "$COMMITTED_TREE" -eq 1 ]; then
    SOURCE_STATE_JSON="$(compute_committed_tree_state_json)"
else
    SOURCE_STATE_JSON="$(compute_source_state_json)"
fi
SOURCE_STATE_DEGRADED_CODES="$(
    SOURCE_STATE_JSON="$SOURCE_STATE_JSON" python3 - <<'PY'
import json
import os
state = json.loads(os.environ["SOURCE_STATE_JSON"])
for code in state.get("source_state_degraded_codes") or []:
    print(code)
PY
)"
if [ "$REQUIRE_CLEAN_TREE" -eq 1 ] && [ -n "$SOURCE_STATE_DEGRADED_CODES" ]; then
    RCH_INVOCATION=()
    mapfile -t source_degraded_array <<<"$SOURCE_STATE_DEGRADED_CODES"
    emit_json true 1 0 "strict clean-tree preflight refused dirty checkout" "" "${source_degraded_array[@]}"
    exit 1
fi
if [ "$COMMITTED_TREE" -eq 1 ]; then
    if [ -n "$SOURCE_STATE_DEGRADED_CODES" ]; then
        RCH_INVOCATION=()
        mapfile -t source_degraded_array <<<"$SOURCE_STATE_DEGRADED_CODES"
        emit_json true 1 0 "committed-tree preflight computed source manifest but cannot safely materialize it for RCH" "" "${source_degraded_array[@]}"
        exit 1
    fi
    materialize_committed_tree
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

if [ "$DRY_RUN" -eq 0 ]; then
    RCH_RUNTIME_JSON="$(rch_runtime_json)"
    if [ "${RCH_VERIFY_FAIL_FAST_VERSION_SKEW:-1}" = "1" ]; then
        RCH_RUNTIME_SKEW_CODE="$(rch_runtime_skew_code "$RCH_RUNTIME_JSON")"
        if [ -n "$RCH_RUNTIME_SKEW_CODE" ]; then
            emit_json true 1 0 "RCH client/daemon version skew; refusing before remote Cargo" "" \
                "$RCH_RUNTIME_SKEW_CODE"
            exit 1
        fi
    fi
fi

BUILD_ADMISSION_JSON="$(compute_build_admission_json)"
BUILD_ADMISSION_STATUS="$(build_admission_status "$BUILD_ADMISSION_JSON")"

if [ "$DRY_RUN" -eq 1 ]; then
    dry_run_degraded=("rch_verify_dry_run")
    if [ "$COMMAND_KIND" = "raw" ]; then
        dry_run_degraded+=("rch_verify_raw_command_may_not_offload")
    fi
    emit_json true null 0 "dry run: explicit rch exec planned" "" "${dry_run_degraded[@]}"
    exit 0
fi

if [ "$BUILD_ADMISSION_STATUS" = "denied" ]; then
    emit_json true 1 0 "build-admission preflight denied RCH execution" "" \
        "rch_verify_build_admission_denied"
    exit 1
fi

build_admission_degraded=()
case "$BUILD_ADMISSION_STATUS" in
    unavailable)
        build_admission_degraded+=("rch_verify_build_admission_unavailable")
        ;;
    skipped)
        build_admission_degraded+=("rch_verify_build_admission_skipped")
        ;;
esac

CONFIGURED_WORKERS_CSV="$(configured_workers)"
DAEMON_WORKERS_CSV="$(daemon_workers)"
REQUESTED_WORKERS_CSV="${RCH_WORKERS:-}"

if [ "$KNOWN_BLOCKER_ENABLED" = "1" ]; then
    KNOWN_BLOCKER_JSON="$(known_blocker_lookup_json "$SOURCE_STATE_JSON")"
    if [ "$KNOWN_BLOCKER_JSON" != "null" ]; then
        if [ "$KNOWN_BLOCKER_OVERRIDE" -eq 1 ]; then
            KNOWN_BLOCKER_JSON="$(known_blocker_override_json "$KNOWN_BLOCKER_JSON")"
        else
            RCH_INVOCATION=()
            emit_json true 1 0 "known RCH blocker matched; refusing before remote Cargo" "" \
                "rch_verify_known_blocker_active"
            exit 1
        fi
    fi
fi

if [ "${RCH_VERIFY_FAIL_FAST_STALE_WORKER:-1}" = "1" ]; then
    allowed_workers_csv="${REQUESTED_WORKERS_CSV:-$CONFIGURED_WORKERS_CSV}"
    allowed_workers_note="configured"
    recent_failure_max_ms="${RCH_VERIFY_RECENT_FAILURE_MAX_MS:-10000}"
    if [ -n "$REQUESTED_WORKERS_CSV" ]; then
        allowed_workers_note="requested"
        recent_failure_max_ms="${RCH_VERIFY_REQUESTED_RECENT_FAILURE_MAX_MS:-120000}"
    fi
    stale_disk_full_workers="$(stale_disk_full_daemon_workers "$allowed_workers_csv" "$DAEMON_WORKERS_CSV" "${RCH_VERIFY_DISK_FULL_WORKERS:-}")"
    stale_recent_failed_workers="$(recent_failed_excluded_daemon_workers "$allowed_workers_csv" "$DAEMON_WORKERS_CSV" "$recent_failure_max_ms")"
    if [ -n "$stale_disk_full_workers" ]; then
        first_stale_worker="${stale_disk_full_workers%%,*}"
        WORKER_ID_JSON="$(json_quote "$first_stale_worker")"
        preflight_note="[RCH_VERIFY] stale daemon worker(s) excluded from $allowed_workers_note workers and recently disk-full: $stale_disk_full_workers"
        emit_json true 1 0 "$preflight_note" "" \
            "${build_admission_degraded[@]}" \
            "rch_verify_remote_command_failed" \
            "rch_verify_worker_disk_full" \
            "rch_verify_worker_filter_ignored"
        exit 1
    elif [ -n "$stale_recent_failed_workers" ]; then
        first_stale_worker="${stale_recent_failed_workers%%,*}"
        WORKER_ID_JSON="$(json_quote "$first_stale_worker")"
        preflight_note="[RCH_VERIFY] stale daemon worker(s) excluded from $allowed_workers_note workers and recently failed fast: $stale_recent_failed_workers"
        emit_json true 1 0 "$preflight_note" "" \
            "${build_admission_degraded[@]}" \
            "rch_verify_remote_command_failed" \
            "rch_verify_worker_filter_ignored"
        exit 1
    fi
fi

start_ms="$(now_ms)"
primary_has_artifacts=0
if [ -z "${RCH_VERIFY_FAKE_OUTPUT:-}" ]; then
    prepare_attempt_artifacts "primary"
    primary_has_artifacts=1
fi
set +e
combined_output="$(run_rch_invocation_once)"
exit_code=$?
set -e
end_ms="$(now_ms)"
elapsed_ms=$((end_ms - start_ms))
if [ -n "${RCH_VERIFY_FAKE_ELAPSED_MS:-}" ]; then
    elapsed_ms="${RCH_VERIFY_FAKE_ELAPSED_MS}"
fi
if [ "$primary_has_artifacts" -eq 1 ]; then
    if [ -s "$RCH_ATTEMPT_META_FILE" ]; then
        attempt_timed_out="$(json_file_field "$RCH_ATTEMPT_META_FILE" timed_out)"
        if [ "$attempt_timed_out" = "true" ]; then
            RCH_ATTEMPT_TIMED_OUT=true
        fi
        RCH_STDOUT_BYTES=$((RCH_STDOUT_BYTES + $(json_file_field "$RCH_ATTEMPT_META_FILE" stdout_bytes)))
        RCH_STDERR_BYTES=$((RCH_STDERR_BYTES + $(json_file_field "$RCH_ATTEMPT_META_FILE" stderr_bytes)))
    else
        RCH_STDOUT_BYTES=$((RCH_STDOUT_BYTES + $(file_bytes "$RCH_ATTEMPT_STDOUT_FILE")))
        RCH_STDERR_BYTES=$((RCH_STDERR_BYTES + $(file_bytes "$RCH_ATTEMPT_STDERR_FILE")))
    fi
    record_attempt_artifacts "primary"
fi

worker_id="$(printf '%s' "$combined_output" | extract_worker_id)"
planner_worker_id="$(printf '%s' "$combined_output" | extract_dependency_planner_worker_id)"
disk_full_worker=""
retried_after_disk_full=0
retry_worker=""
worker_filter_ignored=0
if [ "$exit_code" -ne 0 ] \
    && printf '%s' "$combined_output" | is_worker_disk_full_output \
    && [ -n "$worker_id" ] \
    && [ "${RCH_VERIFY_DISABLE_DISK_FULL_RETRY:-0}" != "1" ]; then
    disk_full_worker="$worker_id"
    alternate_workers="$(healthy_alternate_workers "$disk_full_worker" "${REQUESTED_WORKERS_CSV:-$CONFIGURED_WORKERS_CSV}")"
    if [ -n "$alternate_workers" ]; then
        retried_after_disk_full=1
        retry_note="[RCH_VERIFY] worker $disk_full_worker hit disk-full transfer failure; retrying once with RCH_WORKERS=$alternate_workers"
        start_retry_ms="$(now_ms)"
        retry_has_artifacts=0
        if [ -z "${RCH_VERIFY_FAKE_RETRY_OUTPUT:-}" ]; then
            prepare_attempt_artifacts "retry"
            retry_has_artifacts=1
        fi
        set +e
        retry_output="$(run_rch_invocation_retry "$alternate_workers")"
        retry_exit_code=$?
        set -e
        end_retry_ms="$(now_ms)"
        elapsed_ms=$((elapsed_ms + end_retry_ms - start_retry_ms))
        if [ "$retry_has_artifacts" -eq 1 ]; then
            if [ -s "$RCH_ATTEMPT_META_FILE" ]; then
                attempt_timed_out="$(json_file_field "$RCH_ATTEMPT_META_FILE" timed_out)"
                if [ "$attempt_timed_out" = "true" ]; then
                    RCH_ATTEMPT_TIMED_OUT=true
                fi
                RCH_STDOUT_BYTES=$((RCH_STDOUT_BYTES + $(json_file_field "$RCH_ATTEMPT_META_FILE" stdout_bytes)))
                RCH_STDERR_BYTES=$((RCH_STDERR_BYTES + $(json_file_field "$RCH_ATTEMPT_META_FILE" stderr_bytes)))
            else
                RCH_STDOUT_BYTES=$((RCH_STDOUT_BYTES + $(file_bytes "$RCH_ATTEMPT_STDOUT_FILE")))
                RCH_STDERR_BYTES=$((RCH_STDERR_BYTES + $(file_bytes "$RCH_ATTEMPT_STDERR_FILE")))
            fi
            record_attempt_artifacts "retry"
        fi
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
    allowed_workers_csv="${REQUESTED_WORKERS_CSV:-$CONFIGURED_WORKERS_CSV}"
    if [ -n "$allowed_workers_csv" ] && ! csv_contains "$allowed_workers_csv" "$worker_id"; then
        worker_filter_ignored=1
    fi
fi
if [ -n "$planner_worker_id" ]; then
    allowed_workers_csv="${REQUESTED_WORKERS_CSV:-$CONFIGURED_WORKERS_CSV}"
    if [ -n "$allowed_workers_csv" ] && ! csv_contains "$allowed_workers_csv" "$planner_worker_id"; then
        worker_filter_ignored=1
    fi
fi

remote_checkout_missing_paths="$(remote_checkout_missing_tracked_paths "$combined_output")"
if [ -n "$remote_checkout_missing_paths" ]; then
    combined_output="${combined_output}
[RCH_VERIFY] remote checkout missing tracked files: $remote_checkout_missing_paths"
fi

if [ "${#RCH_ARTIFACT_KINDS[@]}" -gt 0 ]; then
    stdout_tail="$(artifact_tail stdout)"
    stderr_tail="$(artifact_tail stderr)"
else
    stdout_tail="$(printf '%s' "$combined_output" | tail_text)"
    stderr_tail=""
fi
degraded=("${build_admission_degraded[@]}")
if [ "$exit_code" -ne 0 ]; then
    degraded+=("rch_verify_remote_command_failed")
fi
if [ "$RCH_ATTEMPT_TIMED_OUT" = true ]; then
    degraded+=("rch_verify_capacity_or_timeout")
fi
if [ -n "$disk_full_worker" ] || printf '%s' "$combined_output" | is_worker_disk_full_output; then
    degraded+=("rch_verify_worker_disk_full")
fi
if printf '%s' "$combined_output" | is_cargo_workspace_inheritance_output; then
    degraded+=("rch_verify_cargo_workspace_inheritance_blocked")
fi
if printf '%s' "$combined_output" | is_cargo_path_dependency_version_output; then
    degraded+=("rch_verify_cargo_path_dependency_version_blocked")
fi
if [ "$retried_after_disk_full" -eq 1 ]; then
    degraded+=("rch_verify_retry_after_worker_disk_full")
fi
if [ -n "$disk_full_worker" ] && [ "${retry_worker:-}" = "$disk_full_worker" ]; then
    degraded+=("rch_verify_worker_quarantine_ignored")
fi
if [ "$worker_filter_ignored" -eq 1 ]; then
    degraded+=("rch_verify_worker_filter_ignored")
fi
if [ -n "$remote_checkout_missing_paths" ]; then
    degraded+=("rch_verify_remote_checkout_incomplete")
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
if printf '%s' "$combined_output" | is_all_workers_preflight_failed_output; then
    degraded+=("rch_verify_all_workers_preflight_failed")
fi
if [ "$exit_code" -ne 0 ] && [ -z "$worker_id" ] && printf '%s' "$combined_output" | grep -Eiq "timed out|timeout|capacity|busy|no workers|workers_healthy: 0|all_workers_offline"; then
    degraded+=("rch_verify_capacity_or_timeout")
fi
if printf '%s' "$combined_output" | grep -q "non-compilation command"; then
    degraded+=("rch_verify_not_offloaded")
elif [ "$WOULD_OFFLOAD" = true ] && [ -z "$worker_id" ]; then
    degraded+=("rch_verify_remote_marker_missing")
fi

emit_json true "$exit_code" "$elapsed_ms" "$stdout_tail" "$stderr_tail" "${degraded[@]}"
exit "$exit_code"
