#!/usr/bin/env bash
# RCHVC.1 - stable remote verification wrapper for focused Rust checks.
#
# This script is intentionally repo-local. It makes the explicit RCH path the
# easy path for agents and emits a JSON proof that can be pasted into Beads.

set -euo pipefail

if [ "${RCH_VERIFY_STABLE_REEXEC:-0}" != "1" ]; then
    RCH_VERIFY_ORIGINAL_SCRIPT_PATH="${RCH_VERIFY_ORIGINAL_SCRIPT_PATH:-$0}"
    export RCH_VERIFY_ORIGINAL_SCRIPT_PATH
    RCH_VERIFY_STABLE_REEXEC=1
    export RCH_VERIFY_STABLE_REEXEC
    if ! RCH_VERIFY_SCRIPT_SOURCE="$(< "$0")"; then
        echo "rch_verify: failed to read stable script source from $0" >&2
        exit 2
    fi
    exec bash -s -- "$@" <<<"$RCH_VERIFY_SCRIPT_SOURCE"
fi

SCRIPT_PATH="${RCH_VERIFY_ORIGINAL_SCRIPT_PATH:-$0}"
SCRIPT_DIR="$(cd "$(dirname "$SCRIPT_PATH")" && pwd -P)"

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
  --require-clean-tree      Refuse before RCH when the working tree is dirty
  --committed-tree          Verify the committed --treeish from a generated source export when safe
  --pinned-franken-stack    Verify --treeish with sibling path dependencies materialized
                            at the exact franken-stack.lock revisions; implies
                            --committed-tree and requires Cargo --locked
  --treeish <ref>           Committed-tree ref to prove (default: HEAD)
  --known-blocker-store <path>
                            Override the known RCH blocker cache path
  --known-blocker-override  Run through RCH despite a matching active known blocker
  --skip-known-blocker      Disable known-blocker cache read/write for this run
  --proof-broker-ledger <path>
                            Opt into read-only ee proof admit before RCH dispatch
  --proof-broker-ee-bin <path>
                            ee binary to use for proof-broker admission
  --proof-broker-bypass <reason>
                            Continue despite a non-dispatch broker verdict, with degraded evidence
  --worker-root-canary      Run a bounded read-only RCH worker topology canary; no verifier command required
  --json                    Accepted for symmetry; output is always JSON
  -h, --help                Show this help

Environment:
  RCH_VERIFY_ATTEMPT_TIMEOUT_MS  Live rch exec timeout budget (default: 1800000)
  RCH_VERIFY_PREFLIGHT_TIMEOUT_MS  Local helper probe timeout budget (default: 10000)
  RCH_VERIFY_TAIL_BYTES          Diagnostic stdout/stderr tail size (default: 4000)
  RCH_VERIFY_TMPDIR              Retained diagnostic artifact directory (default: /tmp)
  RCH_BUILD_TIMEOUT_SEC          Remote build timeout forwarded to rch exec
                                 (default: 900 for cargo build/check/bench/clippy)
  RCH_TEST_TIMEOUT_SEC           Remote test timeout forwarded to rch exec
                                 (default: 1800 for cargo test)
  RCH_VERIFY_DEFAULT_BUILD_TIMEOUT_SEC
                                 Default RCH_BUILD_TIMEOUT_SEC when unset (default: 900)
  RCH_VERIFY_DEFAULT_TEST_TIMEOUT_SEC
                                 Default RCH_TEST_TIMEOUT_SEC when unset (default: 1800)

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
PINNED_FRANKEN_STACK=0
FRANKEN_STACK_PREFLIGHT_ENABLED="${RCH_VERIFY_FRANKEN_STACK_PREFLIGHT:-1}"
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
PROOF_BROKER_LEDGER="${RCH_VERIFY_PROOF_BROKER_LEDGER:-}"
PROOF_BROKER_EE_BIN="${RCH_VERIFY_PROOF_BROKER_EE_BIN:-}"
PROOF_BROKER_BYPASS_REASON="${RCH_VERIFY_PROOF_BROKER_BYPASS_REASON:-}"
PROOF_BROKER_JSON="null"
WORKER_ROOT_CANARY=0
RCH_VERIFY_ATTEMPT_TIMEOUT_MS="${RCH_VERIFY_ATTEMPT_TIMEOUT_MS:-1800000}"
RCH_VERIFY_PREFLIGHT_TIMEOUT_MS="${RCH_VERIFY_PREFLIGHT_TIMEOUT_MS:-10000}"
RCH_VERIFY_TAIL_BYTES="${RCH_VERIFY_TAIL_BYTES:-4000}"
RCH_VERIFY_TMPDIR="${RCH_VERIFY_TMPDIR:-/tmp}"
RCH_VERIFY_DEFAULT_BUILD_TIMEOUT_SEC="${RCH_VERIFY_DEFAULT_BUILD_TIMEOUT_SEC:-900}"
RCH_VERIFY_DEFAULT_TEST_TIMEOUT_SEC="${RCH_VERIFY_DEFAULT_TEST_TIMEOUT_SEC:-1800}"
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
CARGO_CONFIG_PROVENANCE_JSON='{"schema":"ee.rch.cargo_config_provenance.v1","status":"not_computed","source_attested":false,"command_locked":false,"sources":[],"external_resolution_sources":[],"blocking_sources":[],"provenance_hash":null,"refusal_reason":null,"repair":null}'
FRANKEN_STACK_JSON='{"schema":"ee.rch.franken_stack.v1","status":"not_computed","mode":"live","applicable":false,"command_locked":false,"remote_source_verified":false,"repositories":[],"blocking_codes":[],"manifest_hash":null,"repair":null}'
PINNED_BUNDLE_CACHE_STATUS="not_applicable"
PINNED_BUNDLE_CONTENT_HASH=""
PINNED_BUNDLE_FINAL_ROOT=""
PINNED_BUNDLE_REUSED=0
COMMITTED_TREE_EXPORT_BASE=""
host_can_run_executable() {
    local candidate="${1:-}"
    [ -n "$candidate" ] || return 1
    [ -x "$candidate" ] || return 1
    command -v file >/dev/null 2>&1 || return 0

    local host_kind file_info
    host_kind="$(uname -s 2>/dev/null || printf unknown)"
    file_info="$(file -b "$candidate" 2>/dev/null || true)"
    case "$host_kind:$file_info" in
        Darwin:*Mach-O*) return 0 ;;
        Darwin:*script*|Darwin:*text*) return 0 ;;
        Darwin:*) return 1 ;;
        Linux:*ELF*) return 0 ;;
        Linux:*script*|Linux:*text*) return 0 ;;
        Linux:*) return 1 ;;
        *) return 0 ;;
    esac
}

RCH_MANIFEST_FIX_SIDECAR_BIN="/Users/jemanuel/.local/bin/rch-manifestfix-20260605-5"
RCH_E327_SIDECAR_BIN="/Users/jemanuel/.local/bin/rch-33720a8"
RCH_MACOS_SOURCE_BIN="/Volumes/USBNVME16TB/temp_agent_space/rch-macos-target/debug/rch"
DEFAULT_RCH_BIN="/Users/jemanuel/projects/remote_compilation_helper/target-local/release/rch"
RCH_LOCAL_BIN="/Users/jemanuel/.local/bin/rch"
RCH_PATH_BIN="$(command -v rch 2>/dev/null || true)"
if [ -z "${RCH_BIN:-}" ]; then
    # Prefer the currently installed client. Older sidecar clients are kept as
    # last-resort fallbacks because their worker wrappers may lag daemon fixes.
    for rch_candidate in \
        "$RCH_LOCAL_BIN" \
        "$RCH_PATH_BIN" \
        "$DEFAULT_RCH_BIN" \
        "$RCH_MACOS_SOURCE_BIN" \
        "$RCH_MANIFEST_FIX_SIDECAR_BIN" \
        "$RCH_E327_SIDECAR_BIN"
    do
        if host_can_run_executable "$rch_candidate"; then
            RCH_BIN="$rch_candidate"
            break
        fi
    done
fi
if [ -z "${RCH_BIN:-}" ]; then
    RCH_BIN="rch"
fi
PROJECT_ROOT="$PWD"
DEFAULT_RCH_ALIAS_PROJECT_ROOT="/tmp/rch-users-jemanuel"

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
        --pinned-franken-stack) PINNED_FRANKEN_STACK=1; COMMITTED_TREE=1; shift ;;
        --treeish) TREEISH="${2:?--treeish requires a value}"; shift 2 ;;
        --known-blocker-store) KNOWN_BLOCKER_STORE="${2:?--known-blocker-store requires a value}"; KNOWN_BLOCKER_STORE_EXPLICIT=1; shift 2 ;;
        --known-blocker-override) KNOWN_BLOCKER_OVERRIDE=1; shift ;;
        --skip-known-blocker) KNOWN_BLOCKER_ENABLED=0; shift ;;
        --proof-broker-ledger) PROOF_BROKER_LEDGER="${2:?--proof-broker-ledger requires a value}"; shift 2 ;;
        --proof-broker-ee-bin) PROOF_BROKER_EE_BIN="${2:?--proof-broker-ee-bin requires a value}"; shift 2 ;;
        --proof-broker-bypass) PROOF_BROKER_BYPASS_REASON="${2:?--proof-broker-bypass requires a value}"; shift 2 ;;
        --worker-root-canary) WORKER_ROOT_CANARY=1; shift ;;
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

if [ "$#" -eq 0 ] && [ "$WORKER_ROOT_CANARY" -ne 1 ]; then
    echo "rch_verify: verifier command is required after --" >&2
    usage >&2
    exit 2
fi

if [ -z "$KNOWN_BLOCKER_STORE" ]; then
    KNOWN_BLOCKER_STORE="$PROJECT_ROOT/.ee/derived/rch/known_blockers.jsonl"
fi

SOURCE_PROJECT_ROOT="$PROJECT_ROOT"
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
        build) printf 'cargo_build' ;;
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

command_uses_locked() {
    local index arg
    for ((index = 2; index < ${#COMMAND[@]}; index++)); do
        arg="${COMMAND[$index]}"
        if [ "$arg" = "--" ]; then
            break
        fi
        if [ "$arg" = "--locked" ]; then
            return 0
        fi
    done
    return 1
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

positive_integer_if_set_or_die() {
    local name="$1"
    local value="${2:-}"
    if [ -z "$value" ]; then
        return 0
    fi
    positive_integer_or_die "$name" "$value"
}

apply_default_remote_timeouts() {
    case "$COMMAND_KIND" in
        cargo_build|cargo_check|cargo_bench|cargo_clippy)
            if [ -z "${RCH_BUILD_TIMEOUT_SEC:-}" ]; then
                RCH_BUILD_TIMEOUT_SEC="$RCH_VERIFY_DEFAULT_BUILD_TIMEOUT_SEC"
            fi
            ;;
        cargo_test)
            if [ -z "${RCH_TEST_TIMEOUT_SEC:-}" ]; then
                RCH_TEST_TIMEOUT_SEC="$RCH_VERIFY_DEFAULT_TEST_TIMEOUT_SEC"
            fi
            ;;
    esac
}

remote_timeout_fingerprint() {
    printf 'build:%s,test:%s' "${RCH_BUILD_TIMEOUT_SEC:-unset}" "${RCH_TEST_TIMEOUT_SEC:-unset}"
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
    if [ "$INCLUDE_SUMMARY" -ne 1 ] && [ "${RCH_VERIFY_LOCAL_CARGO_SCAN:-0}" != "1" ] && [ -z "$PROOF_BROKER_LEDGER" ]; then
        local_cargo_processes_not_run_json "only scanned for --summary unless RCH_VERIFY_LOCAL_CARGO_SCAN=1"
        return 0
    fi
    if [ -n "${RCH_VERIFY_FAKE_OUTPUT:-}" ] && [ "${RCH_VERIFY_LOCAL_CARGO_SCAN:-0}" != "1" ]; then
        local_cargo_processes_not_run_json "fake RCH transcript without explicit local Cargo scan"
        return 0
    fi
    if [ "$DRY_RUN" -eq 1 ] && [ "${RCH_VERIFY_LOCAL_CARGO_SCAN:-0}" != "1" ]; then
        local_cargo_processes_not_run_json "dry-run skips active process scan because wrapper argv contains the planned remote cargo payload"
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
import re

raw = os.environ.get("LOCAL_CARGO_PROCESSES_OUTPUT", "")
exit_code = int(os.environ.get("LOCAL_CARGO_PROCESSES_EXIT_CODE") or "0")
user_path = re.compile(r"/Users/[^\\s\"'`,;:]+")

def redact_local_paths(value):
    if isinstance(value, str):
        return user_path.sub("<redacted-user-path>", value)
    if isinstance(value, list):
        return [redact_local_paths(item) for item in value]
    if isinstance(value, dict):
        return {key: redact_local_paths(item) for key, item in value.items()}
    return value

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
payload = redact_local_paths(payload)
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

proof_broker_request_fields_json() {
    SOURCE_STATE_JSON_INPUT="${SOURCE_STATE_JSON:-}" \
    BUILD_ADMISSION_JSON_INPUT="${BUILD_ADMISSION_JSON:-}" \
    RCH_RUNTIME_JSON_INPUT="${RCH_RUNTIME_JSON:-}" \
    LOCAL_CARGO_PROCESSES_JSON_INPUT="${LOCAL_CARGO_PROCESSES_JSON:-}" \
    REQUESTED_WORKERS_VALUE="${REQUESTED_WORKERS_CSV:-}" \
    CONFIGURED_WORKERS_VALUE="${CONFIGURED_WORKERS_CSV:-}" \
    COMMAND_KIND_VALUE="$COMMAND_KIND" \
    COMMAND_JSON="$(json_array "${ENV_OVERRIDES[@]}" "${COMMAND[@]}")" \
    python3 - <<'PY'
import hashlib
import json
import os
import re

def load_env_json(name, default):
    try:
        return json.loads(os.environ.get(name) or default)
    except Exception:
        return json.loads(default)

def class_fragment(value):
    text = str(value or "").strip()
    if not text:
        return "unknown"
    text = re.sub(r"[^A-Za-z0-9_.:-]+", "_", text)
    return text.strip("_") or "unknown"

source_state = load_env_json("SOURCE_STATE_JSON_INPUT", "{}")
build_admission = load_env_json("BUILD_ADMISSION_JSON_INPUT", "{}")
runtime = load_env_json("RCH_RUNTIME_JSON_INPUT", "{}")
tripwire = load_env_json("LOCAL_CARGO_PROCESSES_JSON_INPUT", "{}")
command = load_env_json("COMMAND_JSON", "[]")

source_hash = (
    source_state.get("source_bundle_hash")
    or source_state.get("source_manifest_hash")
    or source_state.get("dirty_status_hash")
    or source_state.get("git_tree")
    or "class:unknown_source"
)
source_materialization = (
    source_state.get("source_materialization")
    or "class:unknown_materialization"
)
dirty_status_hash = (
    source_state.get("dirty_status_hash")
    or "class:clean_or_unknown_dirty_state"
)

runtime_status = runtime.get("status") or "unknown"
client_compat = runtime.get("client_compat")
daemon_compat = runtime.get("daemon_compat")
client_version = runtime.get("client_version")
daemon_version = runtime.get("daemon_version")
if runtime_status == "checked" and client_compat and daemon_compat:
    if client_compat == daemon_compat:
        rch_runtime_class = f"class:rch_compat_{class_fragment(client_compat)}"
    else:
        rch_runtime_class = (
            f"class:rch_mismatch_client_{class_fragment(client_compat)}"
            f"_daemon_{class_fragment(daemon_compat)}"
        )
elif runtime_status == "skipped":
    rch_runtime_class = "class:rch_runtime_skipped_fake_transcript"
elif client_version or daemon_version:
    rch_runtime_class = (
        f"class:rch_partial_client_{class_fragment(client_version)}"
        f"_daemon_{class_fragment(daemon_version)}"
    )
else:
    rch_runtime_class = "class:unknown_rch_runtime"

requested_workers = [
    item.strip()
    for item in os.environ.get("REQUESTED_WORKERS_VALUE", "").split(",")
    if item.strip()
]
configured_workers = [
    item.strip()
    for item in os.environ.get("CONFIGURED_WORKERS_VALUE", "").split(",")
    if item.strip()
]
if requested_workers:
    worker_requirement = "workers:" + ",".join(requested_workers)
elif configured_workers:
    worker_requirement = "class:any_configured_worker"
else:
    worker_requirement = "class:any_worker"

tripwire_status = str(tripwire.get("status") or "unknown")
try:
    tripwire_count = int(tripwire.get("count") or 0)
except (TypeError, ValueError):
    tripwire_count = 0
if tripwire_status == "ok" and tripwire_count == 0:
    local_cargo_tripwire_class = "class:tripwire_clean"
elif (
    tripwire_count > 0
    or "bypass" in tripwire_status
    or "blocked" in tripwire_status
):
    local_cargo_tripwire_class = "class:local_cargo_bypass_detected"
else:
    local_cargo_tripwire_class = "class:tripwire_unknown"

admission_status = str(build_admission.get("status") or "unknown")
if admission_status == "passed":
    build_admission_posture = "class:admission_passed"
elif admission_status == "denied":
    build_admission_posture = "class:admission_blocked"
elif admission_status == "skipped":
    build_admission_posture = "class:admission_skipped"
else:
    build_admission_posture = "class:admission_unknown"

normalized_argv_hash = "sha256:" + hashlib.sha256(
    json.dumps(command, separators=(",", ":")).encode("utf-8")
).hexdigest()

payload = {
    "command_class": os.environ.get("COMMAND_KIND_VALUE") or "class:unknown_command",
    "normalized_argv_hash": normalized_argv_hash,
    "source_hash": source_hash,
    "source_materialization": source_materialization,
    "dirty_status_hash": dirty_status_hash,
    "env_fingerprint_class": "class:rch_verify_wrapper",
    "target_profile": "debug",
    "execution_substrate": "rch",
    "rch_runtime_class": rch_runtime_class,
    "worker_requirement": worker_requirement,
    "local_cargo_tripwire_class": local_cargo_tripwire_class,
    "build_admission_posture": build_admission_posture,
}
print(json.dumps(payload, sort_keys=True, separators=(",", ":")))
PY
}

proof_broker_error_json() {
    local status="${1:?proof broker status required}"
    local message="${2:?proof broker message required}"
    PROOF_BROKER_STATUS="$status" \
    PROOF_BROKER_MESSAGE="$message" \
    PROOF_BROKER_BYPASS_REASON_VALUE="$PROOF_BROKER_BYPASS_REASON" \
    python3 - <<'PY'
import hashlib
import json
import os

message = os.environ.get("PROOF_BROKER_MESSAGE") or "proof broker unavailable"
payload = {
    "enabled": True,
    "status": os.environ.get("PROOF_BROKER_STATUS") or "unavailable",
    "verdict": "unknown_insufficient_evidence",
    "reasonCodes": ["proof_broker_unavailable"],
    "nextAction": "rerun_without_broker_only_with_explicit_bypass",
    "nextCommand": None,
    "remoteCargoLaunched": False,
    "readOnly": True,
    "message": message,
    "rawHash": "sha256:" + hashlib.sha256(message.encode("utf-8")).hexdigest(),
}
bypass = os.environ.get("PROOF_BROKER_BYPASS_REASON_VALUE") or None
if bypass:
    payload["bypassReason"] = bypass
print(json.dumps(payload, sort_keys=True, separators=(",", ":")))
PY
}

proof_broker_wrap_json() {
    local raw="${1:-}"
    local exit_code="${2:-0}"
    local timed_out="${3:-false}"
    local elapsed_ms="${4:-0}"
    PROOF_BROKER_RAW="$raw" \
    PROOF_BROKER_EXIT_CODE="$exit_code" \
    PROOF_BROKER_TIMED_OUT="$timed_out" \
    PROOF_BROKER_ELAPSED_MS="$elapsed_ms" \
    PROOF_BROKER_BYPASS_REASON_VALUE="$PROOF_BROKER_BYPASS_REASON" \
    python3 - <<'PY'
import hashlib
import json
import os
import re

raw = os.environ.get("PROOF_BROKER_RAW") or ""
try:
    exit_code = int(os.environ.get("PROOF_BROKER_EXIT_CODE") or "0")
except ValueError:
    exit_code = 0
try:
    elapsed_ms = int(os.environ.get("PROOF_BROKER_ELAPSED_MS") or "0")
except ValueError:
    elapsed_ms = 0
timed_out = os.environ.get("PROOF_BROKER_TIMED_OUT") == "true"

def redact(text):
    text = re.sub(r"\x1b\[[0-9;]*m", "", text or "")
    text = re.sub(r"/Users/[^/\s]+", "/Users/<redacted>", text)
    text = re.sub(r"(?i)(token|secret|password|api[_-]?key)=\S+", r"\1=<redacted>", text)
    return text[-1600:]

payload = {
    "enabled": True,
    "status": "checked",
    "exitCode": exit_code,
    "timedOut": timed_out,
    "elapsedMs": elapsed_ms,
    "verdict": None,
    "reasonCodes": [],
    "nextAction": None,
    "nextCommand": None,
    "reuseRunId": None,
    "waitOwner": None,
    "remoteCargoLaunched": False,
    "readOnly": None,
    "rawHash": "sha256:" + hashlib.sha256(raw.encode("utf-8", "replace")).hexdigest(),
}
bypass = os.environ.get("PROOF_BROKER_BYPASS_REASON_VALUE") or None
if bypass:
    payload["bypassReason"] = bypass
try:
    response = json.loads(raw)
except Exception:
    payload.update({
        "status": "unavailable",
        "verdict": "unknown_insufficient_evidence",
        "reasonCodes": ["proof_broker_invalid_json"],
        "nextAction": "repair_proof_broker_response",
        "message": "ee proof admit did not emit valid JSON: " + redact(raw),
    })
else:
    payload["response"] = response
    if exit_code != 0 or response.get("success") is not True or timed_out:
        payload["status"] = "unavailable"
        payload["verdict"] = "unknown_insufficient_evidence"
        payload["reasonCodes"] = ["proof_broker_command_failed"]
        payload["nextAction"] = "repair_proof_broker_before_dispatch"
    data = response.get("data") if isinstance(response, dict) else None
    if isinstance(data, dict):
        admission = data.get("admission") if isinstance(data.get("admission"), dict) else {}
        payload["schema"] = data.get("schema")
        payload["fingerprint"] = data.get("fingerprint")
        payload["ledger"] = data.get("ledger")
        payload["matchedRecord"] = data.get("matchedRecord")
        payload["freshness"] = data.get("freshness")
        payload["verdict"] = admission.get("verdict") or payload["verdict"]
        payload["reasonCodes"] = admission.get("reasonCodes") or admission.get("reason_codes") or payload["reasonCodes"]
        payload["nextAction"] = admission.get("nextAction") or admission.get("next_action") or payload["nextAction"]
        payload["reuseRunId"] = admission.get("reuseRunId") or admission.get("reuse_run_id")
        payload["waitOwner"] = admission.get("waitOwner") or admission.get("wait_owner")
        payload["nextCommand"] = data.get("nextCommand")
        payload["readOnly"] = data.get("readOnly")
    if not payload.get("verdict"):
        payload["status"] = "unavailable"
        payload["verdict"] = "unknown_insufficient_evidence"
        payload["reasonCodes"] = ["proof_broker_verdict_missing"]
        payload["nextAction"] = "repair_proof_broker_response"
print(json.dumps(payload, sort_keys=True, separators=(",", ":")))
PY
}

proof_broker_json_field() {
    local field="${1:?proof broker field required}"
    JSON_INPUT="${PROOF_BROKER_JSON:-null}" JSON_FIELD="$field" python3 - <<'PY'
import json
import os

try:
    payload = json.loads(os.environ.get("JSON_INPUT") or "null")
except Exception:
    payload = None
value = payload.get(os.environ["JSON_FIELD"]) if isinstance(payload, dict) else None
if isinstance(value, bool):
    print("true" if value else "false")
elif value is None:
    print("")
else:
    print(value)
PY
}

proof_broker_mark_json() {
    local remote_launched="${1:?remote launched bool required}"
    local bypass_reason="${2:-}"
    JSON_INPUT="${PROOF_BROKER_JSON:-null}" \
    PROOF_BROKER_REMOTE_LAUNCHED="$remote_launched" \
    PROOF_BROKER_BYPASS_REASON_MARK="$bypass_reason" \
    python3 - <<'PY'
import json
import os

try:
    payload = json.loads(os.environ.get("JSON_INPUT") or "null")
except Exception:
    payload = None
if not isinstance(payload, dict):
    payload = {"enabled": False}
payload["remoteCargoLaunched"] = os.environ.get("PROOF_BROKER_REMOTE_LAUNCHED") == "true"
bypass = os.environ.get("PROOF_BROKER_BYPASS_REASON_MARK") or None
if bypass:
    payload["bypassReason"] = bypass
print(json.dumps(payload, sort_keys=True, separators=(",", ":")))
PY
}

proof_broker_degraded_code() {
    case "${1:-}" in
        dispatch_allowed) printf '' ;;
        reuse_existing) printf 'rch_verify_proof_broker_reuse_existing' ;;
        wait_for_inflight) printf 'rch_verify_proof_broker_wait_for_inflight' ;;
        source_state_mismatch) printf 'rch_verify_proof_broker_source_state_mismatch' ;;
        environment_blocked) printf 'rch_verify_proof_broker_environment_blocked' ;;
        proof_unusable) printf 'rch_verify_proof_broker_proof_unusable' ;;
        unknown_insufficient_evidence) printf 'rch_verify_proof_broker_unknown_insufficient_evidence' ;;
        *) printf 'rch_verify_proof_broker_unavailable' ;;
    esac
}

run_proof_broker_admission() {
    if [ -z "$PROOF_BROKER_LEDGER" ]; then
        PROOF_BROKER_JSON="null"
        return 0
    fi

    local ee_bin
    if [ -n "$PROOF_BROKER_EE_BIN" ]; then
        ee_bin="$PROOF_BROKER_EE_BIN"
    elif ! ee_bin="$(candidate_ee_bin)"; then
        PROOF_BROKER_JSON="$(proof_broker_error_json "unavailable" "no executable ee binary found for proof-broker admission")"
        return 1
    fi

    local fields command_class normalized_argv_hash source_hash source_materialization dirty_status_hash env_fingerprint_class target_profile execution_substrate rch_runtime_class worker_requirement local_cargo_tripwire_class build_admission_posture
    fields="$(proof_broker_request_fields_json)"
    command_class="$(json_text_field "$fields" command_class)"
    normalized_argv_hash="$(json_text_field "$fields" normalized_argv_hash)"
    source_hash="$(json_text_field "$fields" source_hash)"
    source_materialization="$(json_text_field "$fields" source_materialization)"
    dirty_status_hash="$(json_text_field "$fields" dirty_status_hash)"
    env_fingerprint_class="$(json_text_field "$fields" env_fingerprint_class)"
    target_profile="$(json_text_field "$fields" target_profile)"
    execution_substrate="$(json_text_field "$fields" execution_substrate)"
    rch_runtime_class="$(json_text_field "$fields" rch_runtime_class)"
    worker_requirement="$(json_text_field "$fields" worker_requirement)"
    local_cargo_tripwire_class="$(json_text_field "$fields" local_cargo_tripwire_class)"
    build_admission_posture="$(json_text_field "$fields" build_admission_posture)"

    local args=(
        "$ee_bin" "--workspace" "$PROJECT_ROOT" "--json" "proof" "admit"
        "--ledger-json" "$PROOF_BROKER_LEDGER"
        "--command-class" "$command_class"
        "--normalized-argv-hash" "$normalized_argv_hash"
        "--source-hash" "$source_hash"
        "--source-materialization" "$source_materialization"
        "--dirty-status-hash" "$dirty_status_hash"
        "--env-fingerprint-class" "$env_fingerprint_class"
        "--target-profile" "$target_profile"
        "--execution-substrate" "$execution_substrate"
        "--rch-runtime-class" "$rch_runtime_class"
        "--worker-requirement" "$worker_requirement"
        "--local-cargo-tripwire-class" "$local_cargo_tripwire_class"
        "--build-admission-posture" "$build_admission_posture"
    )
    if [ -n "$BEAD_ID" ]; then
        args+=("--bead-id" "$BEAD_ID")
    fi
    args+=("--" "${ENV_OVERRIDES[@]}" "${COMMAND[@]}")

    local broker_probe broker_output broker_exit broker_timed_out broker_elapsed
    broker_probe="$(capture_command_with_timeout "$RCH_VERIFY_PREFLIGHT_TIMEOUT_MS" "$PROJECT_ROOT" "${args[@]}")"
    broker_output="$(json_text_field "$broker_probe" output)"
    broker_exit="$(json_text_field "$broker_probe" status)"
    broker_timed_out="$(json_text_field "$broker_probe" timed_out)"
    broker_elapsed="$(json_text_field "$broker_probe" elapsed_ms)"
    PROOF_BROKER_JSON="$(proof_broker_wrap_json "$broker_output" "$broker_exit" "$broker_timed_out" "$broker_elapsed")"

    if [ "$broker_timed_out" = "true" ] || [ "$broker_exit" != "0" ]; then
        return 1
    fi
    case "$(proof_broker_json_field verdict)" in
        dispatch_allowed|reuse_existing|wait_for_inflight|source_state_mismatch|environment_blocked|proof_unusable|unknown_insufficient_evidence)
            return 0
            ;;
        *)
            return 1
            ;;
    esac
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

is_no_workers_passed_health_output() {
    grep -Eiq "no workers passed health thresholds|no_workers_passed_health"
}

is_active_project_exclusion_output() {
    grep -Eiq '^[[:space:]]*\[RCH\][[:space:]]+.*(active_project_exclusion[[:space:]]*[=:]|active project exclusion([[:space:]]*[=:]|[[:space:]]|$))'
}

is_client_daemon_unknown_variant_output() {
    grep -Eiq "Failed to parse daemon response: unknown variant"
}

is_remote_transport_timeout_output() {
    grep -Eiq "RCH-E104|SSH command timed out|Remote execution failed .*SSH timeout"
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
        version_text = "\n".join((version.stdout, version.stderr))
        match = re.search(
            r"(?:^|\s)v?(\d+\.\d+(?:\.\d+)?(?:[-+][0-9A-Za-z.-]+)?)",
            version_text,
        )
        base["client_version"] = match.group(1) if match else None
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
    REMOTE_TIMEOUT_FINGERPRINT_VALUE="$(remote_timeout_fingerprint)" \
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
    source_state.get("source_bundle_hash")
    or source_state.get("source_manifest_hash")
    or source_state.get("dirty_status_hash")
    or ""
)
verifier_source_mode = source_state.get("verification_attribution") or None
requested_workers = csv_items(os.environ.get("REQUESTED_WORKERS_VALUE", ""))
configured_workers = csv_items(os.environ.get("CONFIGURED_WORKERS_VALUE", ""))
remote_timeout_fingerprint = (
    os.environ.get("REMOTE_TIMEOUT_FINGERPRINT_VALUE")
    or "build:unset,test:unset"
)
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
    "remote_timeout_fingerprint": remote_timeout_fingerprint,
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
    if entry.get("remote_timeout_fingerprint") != current["remote_timeout_fingerprint"]:
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

def add_dirty_shape_codes():
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

source_codes = []
if require_clean and summary["total"]:
    attribution = "source_state_refused"
    source_codes.append("rch_verify_dirty_tree_refused")
    add_dirty_shape_codes()
elif require_clean:
    attribution = "strict_clean_tree"
else:
    attribution = "local_checkout_observed_remote_source_unknown"
    if summary["total"]:
        source_codes.append("rch_verify_dirty_source_not_materialized")
        add_dirty_shape_codes()

print(json.dumps({
    "verification_attribution": attribution,
    "git_head": head,
    "git_tree": tree,
    "dirty_status_hash": dirty_hash,
    "dirty_summary": summary,
    "dirty_paths_sample": sample,
    "remote_source_materialized": False,
    "source_materialization": "remote_checkout_unverified",
    "source_state_degraded_codes": source_codes,
}, sort_keys=True, separators=(",", ":")))
PY
}

compute_committed_tree_state_json() {
    PROJECT_ROOT_PATH="$PROJECT_ROOT" \
    REQUESTED_TREEISH="$TREEISH" \
    PINNED_FRANKEN_STACK="$PINNED_FRANKEN_STACK" \
    python3 - <<'PY'
import hashlib
import json
import os
import subprocess

project_root = os.environ["PROJECT_ROOT_PATH"]
treeish = os.environ.get("REQUESTED_TREEISH") or "HEAD"
pinned_franken_stack = os.environ.get("PINNED_FRANKEN_STACK") == "1"

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
        "remote_source_materialized": False,
        "source_materialization": "none",
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
    lock_result = git(["show", f"{commit}:franken-stack.lock"])
    if not pinned_franken_stack or lock_result.returncode != 0:
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
    "remote_source_materialized": True,
    "source_materialization": "git_archive",
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

pinned_bundle_content_hash() {
    PINNED_BUNDLE_ROOT_PATH="${1:?pinned bundle root required}" \
    python3 - <<'PY'
import hashlib
import os
import stat
from pathlib import Path

root = Path(os.environ["PINNED_BUNDLE_ROOT_PATH"])
excluded = {
    ".ee-rch-franken-stack.tsv",
    ".ee-rch-pinned-bundle.json",
}
entries = []
for current, directory_names, file_names in os.walk(root, followlinks=False):
    directory_names.sort()
    file_names.sort()
    current_path = Path(current)
    for name in [*directory_names, *file_names]:
        path = current_path / name
        relative = path.relative_to(root).as_posix()
        if relative in excluded:
            continue
        try:
            metadata = path.lstat()
        except OSError as error:
            raise SystemExit(f"could not stat pinned bundle entry {relative}: {error}")
        executable = stat.S_IMODE(metadata.st_mode) & 0o111
        if stat.S_ISLNK(metadata.st_mode):
            entries.append(("L", relative, executable, os.readlink(path)))
        elif stat.S_ISDIR(metadata.st_mode):
            entries.append(("D", relative, executable, None))
        elif stat.S_ISREG(metadata.st_mode):
            entries.append(("F", relative, executable, path))
        else:
            raise SystemExit(f"unsupported pinned bundle entry type: {relative}")

digest = hashlib.sha256()
for entry_kind, relative, executable, value in sorted(
    entries,
    key=lambda item: (item[1], item[0]),
):
    digest.update(entry_kind.encode("ascii"))
    digest.update(b"\0")
    digest.update(relative.encode("utf-8", "surrogateescape"))
    digest.update(b"\0")
    digest.update(str(executable).encode("ascii"))
    digest.update(b"\0")
    if entry_kind == "L":
        digest.update(value.encode("utf-8", "surrogateescape"))
    elif entry_kind == "F":
        with value.open("rb") as handle:
            while chunk := handle.read(1024 * 1024):
                digest.update(chunk)
    digest.update(b"\0")
print("sha256:" + digest.hexdigest())
PY
}

pinned_bundle_is_valid() {
    local candidate_root="${1:?pinned bundle candidate required}"
    local ready_path expected_content_hash observed_content_hash
    ready_path="$candidate_root/.ee-rch-pinned-bundle.json"
    [ -f "$ready_path" ] || return 1
    expected_content_hash="$(
        PINNED_BUNDLE_READY_PATH="$ready_path" \
        SOURCE_STATE_JSON_INPUT="$SOURCE_STATE_JSON" \
        python3 - <<'PY'
import json
import os
from pathlib import Path

try:
    ready = json.loads(
        Path(os.environ["PINNED_BUNDLE_READY_PATH"]).read_text(encoding="utf-8")
    )
    source = json.loads(os.environ["SOURCE_STATE_JSON_INPUT"])
except (OSError, UnicodeDecodeError, json.JSONDecodeError):
    raise SystemExit(1)

if (
    ready.get("schema") != "ee.rch.pinned_bundle.v1"
    or ready.get("resolved_commit") != source.get("resolved_commit")
    or ready.get("git_tree") != source.get("git_tree")
    or ready.get("source_manifest_hash") != source.get("source_manifest_hash")
):
    raise SystemExit(1)
content_hash = ready.get("content_hash")
if not isinstance(content_hash, str) or not content_hash.startswith("sha256:"):
    raise SystemExit(1)
print(content_hash)
PY
    )" || return 1
    observed_content_hash="$(pinned_bundle_content_hash "$candidate_root")" || return 1
    [ "$observed_content_hash" = "$expected_content_hash" ] || return 1
    PINNED_BUNDLE_CONTENT_HASH="$observed_content_hash"
}

materialize_committed_tree() {
    local commit export_base export_root project_parent short_commit
    commit="$(json_field_string "$SOURCE_STATE_JSON" "resolved_commit")"
    if [ -z "$commit" ]; then
        echo "rch_verify: committed-tree materialization missing resolved commit" >&2
        return 1
    fi

    short_commit="${commit:0:12}"
    if [ -n "${RCH_VERIFY_COMMITTED_TREE_BASE:-}" ]; then
        export_base="$RCH_VERIFY_COMMITTED_TREE_BASE"
    elif [ "$PINNED_FRANKEN_STACK" -eq 1 ]; then
        project_parent="$(
            cd "$(dirname "$SOURCE_PROJECT_ROOT")"
            pwd -P
        )"
        export_base="$project_parent/.ee-rch-committed-tree"
    else
        export_base="${TMPDIR:-/tmp}/ee-rch-committed-tree"
    fi
    mkdir -p "$export_base"
    COMMITTED_TREE_EXPORT_BASE="$(
        cd "$export_base"
        pwd -P
    )"
    export_base="$COMMITTED_TREE_EXPORT_BASE"
    if [ "$PINNED_FRANKEN_STACK" -eq 1 ]; then
        local bundle_cache_root bundle_index bundle_root
        bundle_cache_root="$export_base/pinned-v1"
        mkdir -p "$bundle_cache_root"
        bundle_index=0
        while [ "$bundle_index" -lt 32 ]; do
            if [ "$bundle_index" -eq 0 ]; then
                bundle_root="$bundle_cache_root/$commit"
            else
                bundle_root="$bundle_cache_root/$commit.recovery-$bundle_index"
            fi
            if [ -d "$bundle_root" ] && pinned_bundle_is_valid "$bundle_root"; then
                PINNED_BUNDLE_CACHE_STATUS="reused"
                PINNED_BUNDLE_FINAL_ROOT="$bundle_root"
                PINNED_BUNDLE_REUSED=1
                PROJECT_ROOT="$bundle_root/eidetic_engine_cli"
                REMOTE_PROJECT_ROOT="/data/projects/$(basename "$PROJECT_ROOT")"
                REMOTE_PROJECT_ROOT_JSON="$(json_quote "$REMOTE_PROJECT_ROOT")"
                return 0
            fi
            if [ ! -e "$bundle_root" ]; then
                PINNED_BUNDLE_FINAL_ROOT="$bundle_root"
                break
            fi
            bundle_index=$((bundle_index + 1))
        done
        if [ -z "$PINNED_BUNDLE_FINAL_ROOT" ]; then
            echo "rch_verify: no trustworthy pinned bundle cache slot is available" >&2
            return 1
        fi
        bundle_root="$(mktemp -d "$export_base/.pinned-staging.$short_commit.XXXXXX")"
        export_root="$bundle_root/eidetic_engine_cli"
        mkdir "$export_root"
        PINNED_BUNDLE_CACHE_STATUS="materializing"
    else
        export_root="$(mktemp -d "$export_base/$short_commit.XXXXXX")"
    fi

    git -C "$PROJECT_ROOT" archive --format=tar "$commit" | tar -x -f - -C "$export_root"
    PROJECT_ROOT="$export_root"
    REMOTE_PROJECT_ROOT="/data/projects/$(basename "$PROJECT_ROOT")"
    REMOTE_PROJECT_ROOT_JSON="$(json_quote "$REMOTE_PROJECT_ROOT")"
}

compute_franken_stack_json() {
    case "$COMMAND_KIND" in
        cargo_build|cargo_check|cargo_test|cargo_bench|cargo_clippy)
            ;;
        *)
            printf '%s\n' \
                '{"schema":"ee.rch.franken_stack.v1","status":"not_applicable","mode":"live","applicable":false,"command_locked":false,"remote_source_verified":false,"repositories":[],"blocking_codes":[],"degraded_codes":[],"manifest_hash":null,"repair":null}'
            return 0
            ;;
    esac
    if [ "$FRANKEN_STACK_PREFLIGHT_ENABLED" != "1" ] \
        && [ "$PINNED_FRANKEN_STACK" -ne 1 ]; then
        printf '%s\n' \
            '{"schema":"ee.rch.franken_stack.v1","status":"not_applicable","mode":"live","applicable":false,"command_locked":false,"remote_source_verified":false,"repositories":[],"blocking_codes":[],"degraded_codes":[],"manifest_hash":null,"repair":null}'
        return 0
    fi
    local command_locked=0
    if command_uses_locked; then
        command_locked=1
    fi

    PROJECT_ROOT_PATH="$PROJECT_ROOT" \
    SOURCE_PROJECT_ROOT_PATH="$SOURCE_PROJECT_ROOT" \
    PINNED_FRANKEN_STACK="$PINNED_FRANKEN_STACK" \
    COMMAND_LOCKED="$command_locked" \
    python3 - <<'PY'
import hashlib
import json
import os
import re
import subprocess
from pathlib import Path

try:
    import tomllib
except ModuleNotFoundError:
    tomllib = None
if os.environ.get("RCH_VERIFY_FORCE_TOML_FALLBACK") == "1":
    tomllib = None

project_root = Path(os.environ["PROJECT_ROOT_PATH"]).resolve(strict=False)
source_project_root = Path(os.environ["SOURCE_PROJECT_ROOT_PATH"]).resolve(strict=False)
dependency_root = source_project_root.parent
pinned = os.environ.get("PINNED_FRANKEN_STACK") == "1"
command_locked = os.environ.get("COMMAND_LOCKED") == "1"
lock_path = project_root / "franken-stack.lock"
cargo_lock_path = project_root / "Cargo.lock"
known = (
    "asupersync",
    "franken_agent_detection",
    "franken_networkx",
    "frankensearch",
    "frankensqlite",
    "sqlmodel_rust",
    "toon_rust",
)
package_manifests = {
    "asupersync": (("asupersync", "Cargo.toml"),),
    "franken_agent_detection": (("franken-agent-detection", "Cargo.toml"),),
    "franken_networkx": (
        ("fnx-algorithms", "crates/fnx-algorithms/Cargo.toml"),
        ("fnx-classes", "crates/fnx-classes/Cargo.toml"),
        ("fnx-runtime", "crates/fnx-runtime/Cargo.toml"),
    ),
    "frankensearch": (("frankensearch", "frankensearch/Cargo.toml"),),
    "frankensqlite": (
        ("fsqlite", "crates/fsqlite/Cargo.toml"),
        ("fsqlite-core", "crates/fsqlite-core/Cargo.toml"),
        ("fsqlite-error", "crates/fsqlite-error/Cargo.toml"),
    ),
    "sqlmodel_rust": (
        ("sqlmodel-core", "crates/sqlmodel-core/Cargo.toml"),
        ("sqlmodel-frankensqlite", "crates/sqlmodel-frankensqlite/Cargo.toml"),
    ),
    "toon_rust": (("tru", "Cargo.toml"),),
}

def sha256_bytes(value):
    return "sha256:" + hashlib.sha256(value).hexdigest()

def run_git(path, args):
    try:
        return subprocess.run(
            ["git", "-C", str(path), *args],
            check=False,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
            timeout=10,
        )
    except Exception:
        return None

def git_text(path, args):
    result = run_git(path, args)
    if result is None or result.returncode != 0:
        return None
    return result.stdout.strip()

def canonical_origin(repository):
    return f"https://github.com/Dicklesworthstone/{repository}.git"

def origin_matches(repository, actual):
    expected = canonical_origin(repository)
    return actual in {
        expected,
        expected.removesuffix(".git"),
        f"git@github.com:Dicklesworthstone/{repository}.git",
    }

def fallback_manifest_version(text):
    section = None
    package_version_value = None
    workspace_version_value = None
    package_uses_workspace = False
    for raw_line in text.splitlines():
        line = raw_line.split("#", 1)[0].strip()
        if not line:
            continue
        section_match = re.fullmatch(r"\[([A-Za-z0-9_.-]+)\]", line)
        if section_match:
            section = section_match.group(1)
            continue
        if section == "package":
            direct = re.fullmatch(r'version\s*=\s*"([^"]+)"', line)
            if direct:
                package_version_value = direct.group(1)
                continue
            if re.fullmatch(r"version\.workspace\s*=\s*true", line):
                package_uses_workspace = True
                continue
            if re.fullmatch(
                r"version\s*=\s*\{\s*workspace\s*=\s*true\s*,?\s*\}",
                line,
            ):
                package_uses_workspace = True
                continue
        if section == "workspace.package":
            workspace = re.fullmatch(r'version\s*=\s*"([^"]+)"', line)
            if workspace:
                workspace_version_value = workspace.group(1)
    if package_version_value is not None:
        return package_version_value, "ok_fallback"
    if package_uses_workspace and workspace_version_value is not None:
        return workspace_version_value, "ok_fallback"
    return None, (
        "workspace_version_unavailable"
        if package_uses_workspace
        else "version_missing"
    )

def package_version(checkout, manifest_relative):
    manifest_path = checkout / manifest_relative
    try:
        manifest_text = manifest_path.read_text(encoding="utf-8")
    except FileNotFoundError:
        return None, "missing"
    except (OSError, UnicodeDecodeError):
        return None, "invalid"
    if tomllib is None:
        version, status = fallback_manifest_version(manifest_text)
        if status == "workspace_version_unavailable":
            try:
                root_text = (checkout / "Cargo.toml").read_text(encoding="utf-8")
            except (OSError, UnicodeDecodeError):
                return None, status
            root_version, root_status = fallback_manifest_version(
                "[package]\nversion.workspace = true\n" + root_text
            )
            return root_version, root_status
        return version, status
    try:
        payload = tomllib.loads(manifest_text)
    except tomllib.TOMLDecodeError:
        return None, "invalid"
    package = payload.get("package")
    if not isinstance(package, dict):
        return None, "package_missing"
    version = package.get("version")
    if isinstance(version, str):
        return version, "ok"
    if isinstance(version, dict) and version.get("workspace") is True:
        try:
            root_payload = tomllib.loads(
                (checkout / "Cargo.toml").read_text(encoding="utf-8")
            )
        except (OSError, UnicodeDecodeError, tomllib.TOMLDecodeError):
            return None, "workspace_version_unavailable"
        workspace_version = (
            root_payload.get("workspace", {})
            .get("package", {})
            .get("version")
        )
        if isinstance(workspace_version, str):
            return workspace_version, "ok"
        return None, "workspace_version_unavailable"
    return None, "version_missing"

def base_payload(status, applicable, blocking_codes, repair):
    return {
        "schema": "ee.rch.franken_stack.v1",
        "status": status,
        "mode": "pinned" if pinned else "live",
        "applicable": applicable,
        "command_locked": command_locked,
        "lock_path": "<project>/franken-stack.lock",
        "lock_hash": None,
        "cargo_lock_hash": None,
        "expected_repository_count": len(known),
        "observed_repository_count": 0,
        "remote_source_verified": False,
        "repositories": [],
        "blocking_codes": sorted(set(blocking_codes)),
        "degraded_codes": [],
        "manifest_hash": None,
        "repair": repair,
    }

if not lock_path.exists():
    if pinned:
        print(json.dumps(base_payload(
            "blocked",
            True,
            ["rch_verify_franken_stack_lock_missing"],
            "Commit a valid franken-stack.lock before using --pinned-franken-stack.",
        ), sort_keys=True, separators=(",", ":")))
    else:
        print(json.dumps(base_payload(
            "not_applicable",
            False,
            [],
            None,
        ), sort_keys=True, separators=(",", ":")))
    raise SystemExit(0)

try:
    lock_bytes = lock_path.read_bytes()
except OSError:
    print(json.dumps(base_payload(
        "blocked",
        True,
        ["rch_verify_franken_stack_lock_unreadable"],
        "Restore a readable franken-stack.lock and rerun.",
    ), sort_keys=True, separators=(",", ":")))
    raise SystemExit(0)

rows = {}
lock_invalid = False
for raw_line in lock_bytes.decode("utf-8", "replace").splitlines():
    line = raw_line.strip()
    if not line or line.startswith("#"):
        continue
    parts = raw_line.split("\t")
    if (
        len(parts) != 2
        or parts[0] not in known
        or parts[0] in rows
        or re.fullmatch(r"[0-9a-f]{40}", parts[1]) is None
    ):
        lock_invalid = True
        continue
    rows[parts[0]] = parts[1]
if set(rows) != set(known):
    lock_invalid = True

payload = base_payload(
    "blocked" if lock_invalid else "inspecting",
    True,
    ["rch_verify_franken_stack_lock_invalid"] if lock_invalid else [],
    "Repair franken-stack.lock to contain each supported repository exactly once."
    if lock_invalid else None,
)
payload["lock_hash"] = sha256_bytes(lock_bytes)
if lock_invalid:
    print(json.dumps(payload, sort_keys=True, separators=(",", ":")))
    raise SystemExit(0)

expected_versions = {}
if not cargo_lock_path.exists():
    payload["blocking_codes"].append("rch_verify_franken_stack_cargo_lock_missing")
else:
    try:
        cargo_lock_bytes = cargo_lock_path.read_bytes()
        cargo_lock_text = cargo_lock_bytes.decode("utf-8")
    except (OSError, UnicodeDecodeError):
        payload["blocking_codes"].append("rch_verify_franken_stack_cargo_lock_invalid")
    else:
        packages = []
        if tomllib is None:
            current = None
            for raw_line in cargo_lock_text.splitlines():
                line = raw_line.split("#", 1)[0].strip()
                if line == "[[package]]":
                    if current:
                        packages.append(current)
                    current = {}
                    continue
                if current is None:
                    continue
                assignment = re.fullmatch(
                    r'(name|version)\s*=\s*"([^"]+)"',
                    line,
                )
                if assignment:
                    current[assignment.group(1)] = assignment.group(2)
            if current:
                packages.append(current)
        else:
            try:
                cargo_lock = tomllib.loads(cargo_lock_text)
            except tomllib.TOMLDecodeError:
                payload["blocking_codes"].append(
                    "rch_verify_franken_stack_cargo_lock_invalid"
                )
                cargo_lock = {}
            packages = cargo_lock.get("package", [])
        payload["cargo_lock_hash"] = sha256_bytes(cargo_lock_bytes)
        for package in packages:
            name = package.get("name")
            version = package.get("version")
            if isinstance(name, str) and isinstance(version, str):
                expected_versions.setdefault(name, set()).add(version)

repositories = []
for repository in known:
    checkout = dependency_root / repository
    expected_revision = rows[repository]
    expected_origin = canonical_origin(repository)
    codes = []
    actual_head = None
    actual_tree = None
    actual_origin = None
    dirty = None
    dirty_count = None
    dirty_status_hash = None
    git_status = "missing"
    exists = checkout.is_dir()
    if not exists:
        codes.append("rch_verify_franken_stack_repository_missing")
    elif not (checkout / ".git").exists():
        git_status = "not_git"
        codes.append("rch_verify_franken_stack_repository_not_git")
    else:
        git_status = "ok"
        actual_head = git_text(checkout, ["rev-parse", "HEAD"])
        actual_tree = git_text(checkout, ["rev-parse", "HEAD^{tree}"])
        actual_origin = git_text(checkout, ["remote", "get-url", "origin"])
        status_result = run_git(
            checkout,
            ["status", "--porcelain=v1", "--untracked-files=normal"],
        )
        if status_result is None or status_result.returncode != 0:
            codes.append("rch_verify_franken_stack_repository_unreadable")
        else:
            normalized_status = "\n".join(sorted(status_result.stdout.splitlines()))
            dirty_count = len([line for line in normalized_status.splitlines() if line])
            dirty = dirty_count > 0
            dirty_status_hash = sha256_bytes(normalized_status.encode("utf-8"))
            if dirty:
                codes.append("rch_verify_franken_stack_repository_dirty")
        if actual_head != expected_revision:
            codes.append("rch_verify_franken_stack_revision_mismatch")
        if not origin_matches(repository, actual_origin):
            codes.append("rch_verify_franken_stack_origin_mismatch")

    packages = []
    for package_name, manifest_relative in package_manifests[repository]:
        expected = sorted(expected_versions.get(package_name, set()))
        actual = None
        version_status = "checkout_missing"
        if exists:
            actual, version_status = package_version(checkout, manifest_relative)
        matches = actual is not None and actual in expected and bool(expected)
        if not matches:
            codes.append("rch_verify_franken_stack_version_mismatch")
        packages.append({
            "name": package_name,
            "manifest": manifest_relative,
            "expected_versions": expected,
            "actual_version": actual,
            "status": version_status,
            "matches": matches,
        })

    path_material = str(checkout.resolve(strict=False)).encode("utf-8", "replace")
    origin_display = expected_origin if origin_matches(repository, actual_origin) else (
        "<unexpected>" if actual_origin else None
    )
    repository_payload = {
        "name": repository,
        "canonical_path": f"<dependency_root>/{repository}",
        "canonical_path_hash": sha256_bytes(path_material),
        "expected_origin": expected_origin,
        "origin": origin_display,
        "origin_hash": sha256_bytes(actual_origin.encode("utf-8", "replace"))
        if actual_origin else None,
        "origin_matches": origin_matches(repository, actual_origin),
        "expected_revision": expected_revision,
        "head": actual_head,
        "tree": actual_tree,
        "revision_matches": actual_head == expected_revision,
        "exists": exists,
        "git_status": git_status,
        "dirty": dirty,
        "dirty_entry_count": dirty_count,
        "dirty_status_hash": dirty_status_hash,
        "packages": packages,
        "state": "clean" if not codes else "mismatch",
        "codes": sorted(set(codes)),
    }
    repositories.append(repository_payload)

payload["repositories"] = repositories
payload["observed_repository_count"] = sum(1 for item in repositories if item["exists"])
observed_codes = sorted({
    code
    for item in repositories
    for code in item["codes"]
})
payload["observed_codes"] = observed_codes
structural_codes = list(payload["blocking_codes"])
if pinned:
    if not command_locked:
        structural_codes.append("rch_verify_franken_stack_locked_required")
    payload["blocking_codes"] = sorted(set(structural_codes))
    payload["status"] = "blocked" if payload["blocking_codes"] else "materialization_required"
    payload["repair"] = (
        "Use --pinned-franken-stack with a Cargo verifier command containing --locked."
        if "rch_verify_franken_stack_locked_required" in payload["blocking_codes"]
        else None
    )
else:
    payload["blocking_codes"] = sorted(set([*structural_codes, *observed_codes]))
    payload["status"] = "blocked" if payload["blocking_codes"] else "clean_remote_unverified"
    payload["repair"] = (
        "Use --pinned-franken-stack --treeish <commit> with Cargo --locked; "
        "the managed lane leaves live sibling checkouts untouched."
        if payload["blocking_codes"] else
        "Use --pinned-franken-stack for remote-verified dependency source attribution."
    )
    if not payload["blocking_codes"]:
        payload["degraded_codes"] = [
            "rch_verify_franken_stack_remote_source_unverified"
        ]

manifest_material = {
    "lock_hash": payload["lock_hash"],
    "cargo_lock_hash": payload["cargo_lock_hash"],
    "mode": payload["mode"],
    "repositories": [
        {
            "name": item["name"],
            "expected_revision": item["expected_revision"],
            "head": item["head"],
            "tree": item["tree"],
            "dirty_status_hash": item["dirty_status_hash"],
            "packages": [
                {
                    "name": package["name"],
                    "manifest": package["manifest"],
                    "expected_versions": package["expected_versions"],
                    "actual_version": package["actual_version"],
                    "matches": package["matches"],
                }
                for package in item["packages"]
            ],
        }
        for item in repositories
    ],
}
payload["manifest_hash"] = sha256_bytes(
    json.dumps(manifest_material, sort_keys=True, separators=(",", ":")).encode("utf-8")
)
print(json.dumps(payload, sort_keys=True, separators=(",", ":")))
PY
}

franken_stack_rows() {
    FRANKEN_STACK_JSON_INPUT="$FRANKEN_STACK_JSON" \
    python3 - <<'PY'
import json
import os

payload = json.loads(os.environ["FRANKEN_STACK_JSON_INPUT"])
for repository in payload.get("repositories") or []:
    print("\t".join((
        repository["name"],
        repository["expected_revision"],
        repository["expected_origin"],
    )))
PY
}

mark_franken_stack_materialization_failed() {
    local message="${1:-pinned Franken-stack materialization failed}"
    FRANKEN_STACK_JSON_INPUT="$FRANKEN_STACK_JSON" \
    FRANKEN_STACK_FAILURE_MESSAGE="$message" \
    python3 - <<'PY'
import json
import os

payload = json.loads(os.environ["FRANKEN_STACK_JSON_INPUT"])
codes = list(payload.get("blocking_codes") or [])
codes.append("rch_verify_franken_stack_materialization_failed")
payload["blocking_codes"] = sorted(set(codes))
payload["status"] = "materialization_failed"
payload["remote_source_verified"] = False
payload["repair"] = (
    "Ensure the locked revisions are available from canonical origins or the "
    "managed pinned-stack cache, then rerun. Existing sibling checkouts were not changed."
)
payload["materialization_error"] = os.environ.get(
    "FRANKEN_STACK_FAILURE_MESSAGE",
    "pinned Franken-stack materialization failed",
)
print(json.dumps(payload, sort_keys=True, separators=(",", ":")))
PY
}

finalize_franken_stack_json() {
    local metadata_path="${1:?materialization metadata path required}"
    FRANKEN_STACK_JSON_INPUT="$FRANKEN_STACK_JSON" \
    FRANKEN_STACK_METADATA_PATH="$metadata_path" \
    PINNED_DEPENDENCY_ROOT="$(dirname "$PROJECT_ROOT")" \
    python3 - <<'PY'
import hashlib
import json
import os
from pathlib import Path

try:
    import tomllib
except ModuleNotFoundError:
    tomllib = None
if os.environ.get("RCH_VERIFY_FORCE_TOML_FALLBACK") == "1":
    tomllib = None

payload = json.loads(os.environ["FRANKEN_STACK_JSON_INPUT"])
dependency_root = Path(os.environ["PINNED_DEPENDENCY_ROOT"])
metadata = {}
for line in Path(os.environ["FRANKEN_STACK_METADATA_PATH"]).read_text(
    encoding="utf-8"
).splitlines():
    if not line:
        continue
    name, revision, tree, file_count, byte_count, source = line.split("\t")
    metadata[name] = {
        "revision": revision,
        "tree": tree,
        "file_count": int(file_count),
        "byte_count": int(byte_count),
        "source": source,
    }

def fallback_manifest_version(text):
    import re

    section = None
    package_version_value = None
    workspace_version_value = None
    package_uses_workspace = False
    for raw_line in text.splitlines():
        line = raw_line.split("#", 1)[0].strip()
        if not line:
            continue
        section_match = re.fullmatch(r"\[([A-Za-z0-9_.-]+)\]", line)
        if section_match:
            section = section_match.group(1)
            continue
        if section == "package":
            direct = re.fullmatch(r'version\s*=\s*"([^"]+)"', line)
            if direct:
                package_version_value = direct.group(1)
                continue
            if re.fullmatch(r"version\.workspace\s*=\s*true", line):
                package_uses_workspace = True
                continue
            if re.fullmatch(
                r"version\s*=\s*\{\s*workspace\s*=\s*true\s*,?\s*\}",
                line,
            ):
                package_uses_workspace = True
                continue
        if section == "workspace.package":
            workspace = re.fullmatch(r'version\s*=\s*"([^"]+)"', line)
            if workspace:
                workspace_version_value = workspace.group(1)
    if package_version_value is not None:
        return package_version_value, "ok_fallback"
    if package_uses_workspace and workspace_version_value is not None:
        return workspace_version_value, "ok_fallback"
    return None, (
        "workspace_version_unavailable"
        if package_uses_workspace
        else "version_missing"
    )

def package_version(checkout, manifest_relative):
    try:
        manifest_text = (checkout / manifest_relative).read_text(encoding="utf-8")
    except FileNotFoundError:
        return None, "missing"
    except (OSError, UnicodeDecodeError):
        return None, "invalid"
    if tomllib is None:
        version, status = fallback_manifest_version(manifest_text)
        if status == "workspace_version_unavailable":
            try:
                root_text = (checkout / "Cargo.toml").read_text(encoding="utf-8")
            except (OSError, UnicodeDecodeError):
                return None, status
            return fallback_manifest_version(
                "[package]\nversion.workspace = true\n" + root_text
            )
        return version, status
    try:
        payload = tomllib.loads(manifest_text)
    except tomllib.TOMLDecodeError:
        return None, "invalid"
    package = payload.get("package")
    if not isinstance(package, dict):
        return None, "package_missing"
    version = package.get("version")
    if isinstance(version, str):
        return version, "ok"
    if isinstance(version, dict) and version.get("workspace") is True:
        try:
            root_payload = tomllib.loads(
                (checkout / "Cargo.toml").read_text(encoding="utf-8")
            )
        except (OSError, UnicodeDecodeError, tomllib.TOMLDecodeError):
            return None, "workspace_version_unavailable"
        workspace_version = (
            root_payload.get("workspace", {})
            .get("package", {})
            .get("version")
        )
        if isinstance(workspace_version, str):
            return workspace_version, "ok"
        return None, "workspace_version_unavailable"
    return None, "version_missing"

blocking = list(payload.get("blocking_codes") or [])
manifest_repositories = []
for repository in payload.get("repositories") or []:
    name = repository["name"]
    record = metadata.get(name)
    if record is None:
        blocking.append("rch_verify_franken_stack_materialization_incomplete")
        continue
    if record["revision"] != repository.get("expected_revision"):
        blocking.append("rch_verify_franken_stack_materialization_incomplete")
        blocking.append("rch_verify_franken_stack_revision_mismatch")
    checkout = dependency_root / name
    materialized_packages = []
    for package in repository.get("packages") or []:
        actual, status = package_version(checkout, package["manifest"])
        expected = package.get("expected_versions") or []
        matches = actual is not None and actual in expected and bool(expected)
        if not matches:
            blocking.append("rch_verify_franken_stack_version_mismatch")
        materialized_packages.append({
            **package,
            "actual_version": actual,
            "status": status,
            "matches": matches,
        })
    repository["materialized"] = {
        "revision": record["revision"],
        "tree": record["tree"],
        "file_count": record["file_count"],
        "byte_count": record["byte_count"],
        "source": record["source"],
        "packages": materialized_packages,
    }
    identity_packages = [
        {
            "name": package["name"],
            "manifest": package["manifest"],
            "expected_versions": package["expected_versions"],
            "actual_version": package["actual_version"],
            "matches": package["matches"],
        }
        for package in materialized_packages
    ]
    manifest_repositories.append({
        "name": name,
        "revision": record["revision"],
        "tree": record["tree"],
        "packages": identity_packages,
    })

payload["blocking_codes"] = sorted(set(blocking))
payload["status"] = "pinned" if not payload["blocking_codes"] else "blocked"
payload["remote_source_verified"] = not payload["blocking_codes"]
payload["repair"] = None if not payload["blocking_codes"] else (
    "The archived locked dependency graph does not match Cargo.lock; repair "
    "franken-stack.lock or Cargo.lock before retrying."
)
manifest_material = {
    "lock_hash": payload.get("lock_hash"),
    "cargo_lock_hash": payload.get("cargo_lock_hash"),
    "mode": "pinned",
    "repositories": manifest_repositories,
}
payload["manifest_hash"] = "sha256:" + hashlib.sha256(
    json.dumps(manifest_material, sort_keys=True, separators=(",", ":")).encode("utf-8")
).hexdigest()
print(json.dumps(payload, sort_keys=True, separators=(",", ":")))
PY
}

attach_pinned_bundle_cache_json() {
    FRANKEN_STACK_JSON_INPUT="$FRANKEN_STACK_JSON" \
    PINNED_BUNDLE_CACHE_STATUS_VALUE="$PINNED_BUNDLE_CACHE_STATUS" \
    PINNED_BUNDLE_CONTENT_HASH_VALUE="$PINNED_BUNDLE_CONTENT_HASH" \
    python3 - <<'PY'
import json
import os

payload = json.loads(os.environ["FRANKEN_STACK_JSON_INPUT"])
payload["bundle_cache"] = {
    "schema": "ee.rch.pinned_bundle_cache.v1",
    "status": os.environ.get("PINNED_BUNDLE_CACHE_STATUS_VALUE") or "unknown",
    "content_hash": os.environ.get("PINNED_BUNDLE_CONTENT_HASH_VALUE") or None,
    "validation": "full_content_hash",
}
print(json.dumps(payload, sort_keys=True, separators=(",", ":")))
PY
}

pinned_bundle_ready_matches_franken_stack() {
    PINNED_BUNDLE_READY_PATH="$PINNED_BUNDLE_FINAL_ROOT/.ee-rch-pinned-bundle.json" \
    PINNED_BUNDLE_CONTENT_HASH_VALUE="$PINNED_BUNDLE_CONTENT_HASH" \
    FRANKEN_STACK_JSON_INPUT="$FRANKEN_STACK_JSON" \
    python3 - <<'PY'
import json
import os
from pathlib import Path

try:
    ready = json.loads(
        Path(os.environ["PINNED_BUNDLE_READY_PATH"]).read_text(encoding="utf-8")
    )
    stack = json.loads(os.environ["FRANKEN_STACK_JSON_INPUT"])
except (OSError, UnicodeDecodeError, json.JSONDecodeError):
    raise SystemExit(1)

if (
    ready.get("dependency_manifest_hash") != stack.get("manifest_hash")
    or ready.get("cargo_lock_hash") != stack.get("cargo_lock_hash")
    or ready.get("content_hash")
        != os.environ.get("PINNED_BUNDLE_CONTENT_HASH_VALUE")
):
    raise SystemExit(1)
PY
}

publish_pinned_bundle() {
    local staging_root ready_path publish_result
    staging_root="$(dirname "$PROJECT_ROOT")"
    ready_path="$staging_root/.ee-rch-pinned-bundle.json"
    PINNED_BUNDLE_CONTENT_HASH="$(pinned_bundle_content_hash "$staging_root")" || return 1
    PINNED_BUNDLE_READY_PATH="$ready_path" \
    PINNED_BUNDLE_CONTENT_HASH_VALUE="$PINNED_BUNDLE_CONTENT_HASH" \
    SOURCE_STATE_JSON_INPUT="$SOURCE_STATE_JSON" \
    FRANKEN_STACK_JSON_INPUT="$FRANKEN_STACK_JSON" \
    python3 - <<'PY'
import json
import os
from pathlib import Path

source = json.loads(os.environ["SOURCE_STATE_JSON_INPUT"])
stack = json.loads(os.environ["FRANKEN_STACK_JSON_INPUT"])
payload = {
    "schema": "ee.rch.pinned_bundle.v1",
    "resolved_commit": source.get("resolved_commit"),
    "git_tree": source.get("git_tree"),
    "source_manifest_hash": source.get("source_manifest_hash"),
    "dependency_manifest_hash": stack.get("manifest_hash"),
    "cargo_lock_hash": stack.get("cargo_lock_hash"),
    "content_hash": os.environ["PINNED_BUNDLE_CONTENT_HASH_VALUE"],
}
Path(os.environ["PINNED_BUNDLE_READY_PATH"]).write_text(
    json.dumps(payload, sort_keys=True, separators=(",", ":")) + "\n",
    encoding="utf-8",
)
PY
    publish_result="$(
        PINNED_BUNDLE_STAGING_ROOT="$staging_root" \
        PINNED_BUNDLE_FINAL_ROOT_VALUE="$PINNED_BUNDLE_FINAL_ROOT" \
        python3 - <<'PY'
import errno
import os

source = os.environ["PINNED_BUNDLE_STAGING_ROOT"]
destination = os.environ["PINNED_BUNDLE_FINAL_ROOT_VALUE"]
try:
    os.rename(source, destination)
except OSError as error:
    if error.errno in {errno.EEXIST, errno.ENOTEMPTY}:
        print("destination_exists")
    else:
        raise
else:
    print("published")
PY
    )" || return 1
    case "$publish_result" in
        published)
            PINNED_BUNDLE_CACHE_STATUS="created"
            ;;
        destination_exists)
            if ! pinned_bundle_is_valid "$PINNED_BUNDLE_FINAL_ROOT"; then
                return 1
            fi
            PINNED_BUNDLE_CACHE_STATUS="reused_after_race"
            ;;
        *)
            return 1
            ;;
    esac
    PROJECT_ROOT="$PINNED_BUNDLE_FINAL_ROOT/eidetic_engine_cli"
    REMOTE_PROJECT_ROOT="/data/projects/$(basename "$PROJECT_ROOT")"
    REMOTE_PROJECT_ROOT_JSON="$(json_quote "$REMOTE_PROJECT_ROOT")"
}

materialize_pinned_franken_stack() {
    local bundle_root cache_root metadata_path repository revision expected_origin
    local source_repository archive_repository destination tree file_count byte_count source_kind
    bundle_root="$(dirname "$PROJECT_ROOT")"
    cache_root="${RCH_VERIFY_PINNED_STACK_CACHE:-$COMMITTED_TREE_EXPORT_BASE/git-cache}"
    metadata_path="$bundle_root/.ee-rch-franken-stack.tsv"

    if [ "$PINNED_BUNDLE_REUSED" -eq 1 ]; then
        if [ ! -f "$metadata_path" ]; then
            FRANKEN_STACK_JSON="$(mark_franken_stack_materialization_failed "reused pinned bundle metadata is missing")"
            return 1
        fi
        FRANKEN_STACK_JSON="$(finalize_franken_stack_json "$metadata_path")"
        if [ "$(json_text_field "$FRANKEN_STACK_JSON" status)" != "pinned" ] \
            || ! pinned_bundle_ready_matches_franken_stack; then
            FRANKEN_STACK_JSON="$(mark_franken_stack_materialization_failed "reused pinned bundle evidence does not match the requested source graph")"
            return 1
        fi
        FRANKEN_STACK_JSON="$(attach_pinned_bundle_cache_json)"
        [ "$(json_text_field "$FRANKEN_STACK_JSON" status)" = "pinned" ]
        return
    fi
    [ ! -e "$metadata_path" ] || return 1
    : > "$metadata_path"

    while IFS=$'\t' read -r repository revision expected_origin; do
        [ -n "$repository" ] || continue
        source_repository="$(dirname "$SOURCE_PROJECT_ROOT")/$repository"
        archive_repository=""
        source_kind=""

        if [ -d "$source_repository/.git" ]; then
            local source_origin
            source_origin="$(git -C "$source_repository" remote get-url origin 2>/dev/null || true)"
            case "$source_origin" in
                "$expected_origin"|"${expected_origin%.git}"|"git@github.com:Dicklesworthstone/${repository}.git")
                    if git -C "$source_repository" cat-file -e "$revision^{commit}" 2>/dev/null; then
                        archive_repository="$source_repository"
                        source_kind="canonical_sibling_object"
                    fi
                    ;;
            esac
        fi

        if [ -z "$archive_repository" ]; then
            local cache_repository marker cache_origin
            cache_repository="$cache_root/$repository.git"
            marker="$cache_repository/ee-franken-stack-cache"
            if [ -e "$cache_repository" ]; then
                if [ ! -d "$cache_repository" ] \
                    || [ "$(git -C "$cache_repository" rev-parse --is-bare-repository 2>/dev/null || true)" != "true" ] \
                    || [ ! -f "$marker" ] \
                    || [ "$(< "$marker")" != "$repository"$'\t'"$expected_origin" ]; then
                    FRANKEN_STACK_JSON="$(mark_franken_stack_materialization_failed "managed cache path has unexpected provenance: $repository")"
                    return 1
                fi
            else
                mkdir -p "$cache_root"
                git init --bare -q "$cache_repository"
                git -C "$cache_repository" remote add origin "$expected_origin"
                printf '%s\t%s\n' "$repository" "$expected_origin" > "$marker"
            fi
            cache_origin="$(git -C "$cache_repository" remote get-url origin 2>/dev/null || true)"
            case "$cache_origin" in
                "$expected_origin"|"${expected_origin%.git}"|"git@github.com:Dicklesworthstone/${repository}.git")
                    ;;
                *)
                    FRANKEN_STACK_JSON="$(mark_franken_stack_materialization_failed "managed cache origin mismatch: $repository")"
                    return 1
                    ;;
            esac
            if ! git -C "$cache_repository" cat-file -e "$revision^{commit}" 2>/dev/null; then
                if ! git -C "$cache_repository" fetch --depth 1 origin "$revision"; then
                    FRANKEN_STACK_JSON="$(mark_franken_stack_materialization_failed "could not fetch locked revision for $repository")"
                    return 1
                fi
            fi
            archive_repository="$cache_repository"
            source_kind="canonical_managed_cache"
        fi

        destination="$bundle_root/$repository"
        if [ -e "$destination" ]; then
            FRANKEN_STACK_JSON="$(mark_franken_stack_materialization_failed "fresh pinned bundle destination already exists: $repository")"
            return 1
        fi
        mkdir "$destination"
        if ! git -C "$archive_repository" archive --format=tar "$revision" \
            | tar -x -f - -C "$destination"; then
            FRANKEN_STACK_JSON="$(mark_franken_stack_materialization_failed "could not archive locked revision for $repository")"
            return 1
        fi
        tree="$(git -C "$archive_repository" rev-parse "$revision^{tree}")"
        file_count="$(git -C "$archive_repository" ls-tree -r --name-only "$revision" | wc -l | tr -d ' ')"
        byte_count="$(git -C "$archive_repository" ls-tree -r -l "$revision" \
            | awk '{ if ($4 ~ /^[0-9]+$/) total += $4 } END { print total + 0 }')"
        printf '%s\t%s\t%s\t%s\t%s\t%s\n' \
            "$repository" "$revision" "$tree" "$file_count" "$byte_count" "$source_kind" \
            >> "$metadata_path"
    done < <(franken_stack_rows)

    FRANKEN_STACK_JSON="$(finalize_franken_stack_json "$metadata_path")"
    if [ "$(json_text_field "$FRANKEN_STACK_JSON" status)" != "pinned" ]; then
        return 1
    fi
    if ! publish_pinned_bundle; then
        FRANKEN_STACK_JSON="$(mark_franken_stack_materialization_failed "could not publish content-addressed pinned bundle")"
        return 1
    fi
    FRANKEN_STACK_JSON="$(attach_pinned_bundle_cache_json)"
    [ "$(json_text_field "$FRANKEN_STACK_JSON" status)" = "pinned" ]
}

merge_franken_stack_source_state_json() {
    SOURCE_STATE_JSON_INPUT="$SOURCE_STATE_JSON" \
    FRANKEN_STACK_JSON_INPUT="$FRANKEN_STACK_JSON" \
    python3 - <<'PY'
import hashlib
import json
import os

source = json.loads(os.environ["SOURCE_STATE_JSON_INPUT"])
stack = json.loads(os.environ["FRANKEN_STACK_JSON_INPUT"])
if not stack.get("applicable"):
    print(json.dumps(source, sort_keys=True, separators=(",", ":")))
    raise SystemExit(0)

dependency_hash = stack.get("manifest_hash")
source_identity = (
    source.get("source_manifest_hash")
    or source.get("dirty_status_hash")
    or "source:unknown"
)
bundle_material = json.dumps({
    "source": source_identity,
    "dependencies": dependency_hash,
    "cargo_lock": stack.get("cargo_lock_hash"),
}, sort_keys=True, separators=(",", ":"))
source["dependency_manifest_hash"] = dependency_hash
source["source_bundle_hash"] = "sha256:" + hashlib.sha256(
    bundle_material.encode("utf-8")
).hexdigest()
codes = list(source.get("source_state_degraded_codes") or [])
codes.extend(stack.get("blocking_codes") or [])
codes.extend(stack.get("degraded_codes") or [])
source["source_state_degraded_codes"] = sorted(set(codes))
if stack.get("mode") == "pinned" and stack.get("status") == "pinned":
    source["verification_attribution"] = "pinned_franken_stack"
    source["remote_source_materialized"] = True
    source["source_materialization"] = "git_archive_with_pinned_franken_stack"
print(json.dumps(source, sort_keys=True, separators=(",", ":")))
PY
}

franken_stack_blocking_codes() {
    FRANKEN_STACK_JSON_INPUT="$FRANKEN_STACK_JSON" \
    python3 - <<'PY'
import json
import os

payload = json.loads(os.environ["FRANKEN_STACK_JSON_INPUT"])
for code in payload.get("blocking_codes") or []:
    print(code)
PY
}

refresh_franken_stack_cargo_lock_json() {
    FRANKEN_STACK_JSON_INPUT="$FRANKEN_STACK_JSON" \
    PROJECT_ROOT_PATH="$PROJECT_ROOT" \
    python3 - <<'PY'
import hashlib
import json
import os
from pathlib import Path

payload = json.loads(os.environ["FRANKEN_STACK_JSON_INPUT"])
if not payload.get("applicable"):
    print(json.dumps(payload, sort_keys=True, separators=(",", ":")))
    raise SystemExit(0)

path = Path(os.environ["PROJECT_ROOT_PATH"]) / "Cargo.lock"
try:
    current_hash = "sha256:" + hashlib.sha256(path.read_bytes()).hexdigest()
except OSError:
    current_hash = None
payload["cargo_lock_hash_after"] = current_hash
expected_hash = payload.get("cargo_lock_hash")
payload["cargo_lock_unchanged"] = (
    None if expected_hash is None else
    current_hash is not None and current_hash == expected_hash
)
if payload["cargo_lock_unchanged"] is False:
    codes = list(payload.get("blocking_codes") or [])
    codes.append("rch_verify_franken_stack_cargo_lock_changed")
    payload["blocking_codes"] = sorted(set(codes))
    payload["status"] = "blocked"
    payload["remote_source_verified"] = False
    payload["repair"] = (
        "Restore Cargo.lock to the source-attested hash and rerun with --locked."
    )
print(json.dumps(payload, sort_keys=True, separators=(",", ":")))
PY
}

compute_cargo_config_provenance_json() {
    local command_locked=0
    local source_attested=0
    if command_uses_locked; then
        command_locked=1
    fi
    if [ "$COMMITTED_TREE" -eq 1 ] || [ "$REQUIRE_CLEAN_TREE" -eq 1 ]; then
        source_attested=1
    fi

    PROJECT_ROOT_PATH="$PROJECT_ROOT" \
    CARGO_HOME_VALUE="${CARGO_HOME:-}" \
    HOME_VALUE="${HOME:-}" \
    COMMAND_KIND_VALUE="$COMMAND_KIND" \
    COMMAND_JSON="$(json_array "${COMMAND[@]}")" \
    COMMAND_LOCKED="$command_locked" \
    SOURCE_ATTESTED="$source_attested" \
    python3 - <<'PY'
import hashlib
import json
import os
import re
from pathlib import Path

try:
    import tomllib
except ModuleNotFoundError:
    tomllib = None
if os.environ.get("RCH_VERIFY_FORCE_TOML_FALLBACK") == "1":
    tomllib = None

project_root = Path(os.environ["PROJECT_ROOT_PATH"]).resolve(strict=False)
home_value = os.environ.get("HOME_VALUE", "")
home = Path(home_value).expanduser().resolve(strict=False) if home_value else None
cargo_home_value = os.environ.get("CARGO_HOME_VALUE", "")
if cargo_home_value:
    cargo_home_candidate = Path(cargo_home_value).expanduser()
    if not cargo_home_candidate.is_absolute():
        cargo_home_candidate = project_root / cargo_home_candidate
    cargo_home = cargo_home_candidate.resolve(strict=False)
elif home is not None:
    cargo_home = (home / ".cargo").resolve(strict=False)
else:
    cargo_home = (project_root / ".cargo").resolve(strict=False)

command_kind = os.environ.get("COMMAND_KIND_VALUE", "")
command_locked = os.environ.get("COMMAND_LOCKED") == "1"
source_attested = os.environ.get("SOURCE_ATTESTED") == "1"
command = json.loads(os.environ.get("COMMAND_JSON") or "[]")
sources = []
seen_effective = {}

def is_within(path, root):
    try:
        path.relative_to(root)
        return True
    except ValueError:
        return False

def display_path(path):
    path = path.resolve(strict=False)
    if is_within(path, project_root):
        relative = path.relative_to(project_root)
        return "<project>" if not relative.parts else f"<project>/{relative.as_posix()}"
    if is_within(path, cargo_home):
        relative = path.relative_to(cargo_home)
        return "<cargo_home>" if not relative.parts else f"<cargo_home>/{relative.as_posix()}"
    if home is not None and is_within(path, home):
        relative = path.relative_to(home)
        return "<home>" if not relative.parts else f"<home>/{relative.as_posix()}"
    return str(path)

def path_hash(path):
    return "sha256:" + hashlib.sha256(
        display_path(path).encode("utf-8", "replace")
    ).hexdigest()

def resolution_controls(payload):
    controls = []
    paths = payload.get("paths")
    if isinstance(paths, list) and paths:
        controls.append("paths")

    patch = payload.get("patch")
    if isinstance(patch, dict):
        for source_name, entries in patch.items():
            if entries:
                controls.append(f"patch.{source_name}")

    replace = payload.get("replace")
    if isinstance(replace, dict) and replace:
        controls.append("replace")

    source = payload.get("source")
    if isinstance(source, dict):
        for source_name, settings in source.items():
            if not isinstance(settings, dict):
                continue
            for key in (
                "replace-with",
                "registry",
                "local-registry",
                "directory",
                "git",
                "branch",
                "tag",
                "rev",
            ):
                if settings.get(key):
                    controls.append(f"source.{source_name}.{key}")
    return sorted(set(controls))

def include_specs(payload):
    raw = payload.get("include")
    if raw is None:
        return []
    if isinstance(raw, (str, dict)):
        raw = [raw]
    if not isinstance(raw, list):
        return []
    specs = []
    for item in raw:
        if isinstance(item, str):
            specs.append((item, False))
        elif isinstance(item, dict) and isinstance(item.get("path"), str):
            specs.append((item["path"], bool(item.get("optional"))))
    return specs

def strip_toml_comment(line):
    quote = None
    escaped = False
    output = []
    for character in line:
        if quote == '"':
            output.append(character)
            if escaped:
                escaped = False
            elif character == "\\":
                escaped = True
            elif character == '"':
                quote = None
            continue
        if quote == "'":
            output.append(character)
            if character == "'":
                quote = None
            continue
        if character in {'"', "'"}:
            quote = character
            output.append(character)
            continue
        if character == "#":
            break
        output.append(character)
    return "".join(output).strip()

def value_is_complete(value):
    quote = None
    escaped = False
    square_depth = 0
    brace_depth = 0
    for character in value:
        if quote == '"':
            if escaped:
                escaped = False
            elif character == "\\":
                escaped = True
            elif character == '"':
                quote = None
            continue
        if quote == "'":
            if character == "'":
                quote = None
            continue
        if character in {'"', "'"}:
            quote = character
        elif character == "[":
            square_depth += 1
        elif character == "]":
            square_depth -= 1
        elif character == "{":
            brace_depth += 1
        elif character == "}":
            brace_depth -= 1
        if square_depth < 0 or brace_depth < 0:
            return None
    if quote is not None:
        return False
    return square_depth == 0 and brace_depth == 0

def split_top_level(value):
    items = []
    current = []
    quote = None
    escaped = False
    square_depth = 0
    brace_depth = 0
    for character in value:
        if quote == '"':
            current.append(character)
            if escaped:
                escaped = False
            elif character == "\\":
                escaped = True
            elif character == '"':
                quote = None
            continue
        if quote == "'":
            current.append(character)
            if character == "'":
                quote = None
            continue
        if character in {'"', "'"}:
            quote = character
            current.append(character)
            continue
        if character == "[":
            square_depth += 1
        elif character == "]":
            square_depth -= 1
        elif character == "{":
            brace_depth += 1
        elif character == "}":
            brace_depth -= 1
        if (
            character == ","
            and square_depth == 0
            and brace_depth == 0
        ):
            items.append("".join(current).strip())
            current = []
        else:
            current.append(character)
        if square_depth < 0 or brace_depth < 0:
            return None
    if quote is not None or square_depth != 0 or brace_depth != 0:
        return None
    items.append("".join(current).strip())
    return [item for item in items if item]

def parse_fallback_string(value):
    value = value.strip()
    if len(value) < 2:
        return None
    if value.startswith('"') and value.endswith('"'):
        try:
            parsed = json.loads(value)
        except json.JSONDecodeError:
            return None
        return parsed if isinstance(parsed, str) else None
    if value.startswith("'") and value.endswith("'"):
        return value[1:-1]
    return None

def parse_fallback_include_item(value):
    string_value = parse_fallback_string(value)
    if string_value is not None:
        return (string_value, False)
    value = value.strip()
    if not (value.startswith("{") and value.endswith("}")):
        return None
    fields = split_top_level(value[1:-1])
    if fields is None:
        return None
    parsed_fields = {}
    for field in fields:
        match = re.fullmatch(r"([A-Za-z0-9_-]+)\s*=\s*(.+)", field, re.DOTALL)
        if match is None:
            return None
        parsed_fields[match.group(1)] = match.group(2).strip()
    path = parse_fallback_string(parsed_fields.get("path", ""))
    if path is None:
        return None
    optional_value = parsed_fields.get("optional", "false")
    if optional_value not in {"true", "false"}:
        return None
    return (path, optional_value == "true")

def parse_fallback_include(value):
    value = value.strip()
    if value.startswith("[") and value.endswith("]"):
        items = split_top_level(value[1:-1])
        if items is None:
            return None
        parsed = [parse_fallback_include_item(item) for item in items]
        return None if any(item is None for item in parsed) else parsed
    item = parse_fallback_include_item(value)
    return None if item is None else [item]

def fallback_resolution_scan(text):
    if '"""' in text or "'''" in text:
        return "unsupported_toml_fallback", [], []
    controls = []
    includes = []
    section = ""
    lines = text.splitlines()
    index = 0
    while index < len(lines):
        line = strip_toml_comment(lines[index])
        index += 1
        if not line:
            continue
        if line.startswith("["):
            match = re.fullmatch(r"\[\s*([^\[\]]+)\s*\]", line)
            if match is None:
                return "unsupported_toml_fallback", [], []
            section = re.sub(r"[\"']", "", match.group(1).strip())
            continue
        assignment = re.fullmatch(
            r"([A-Za-z0-9_.-]+)\s*=\s*(.*)",
            line,
            re.DOTALL,
        )
        if assignment is None:
            return "unsupported_toml_fallback", [], []
        key = assignment.group(1)
        value = assignment.group(2).strip()
        complete = value_is_complete(value)
        while complete is False and index < len(lines):
            continuation = strip_toml_comment(lines[index])
            index += 1
            value = f"{value}\n{continuation}"
            complete = value_is_complete(value)
        if complete is not True or not value:
            return "unsupported_toml_fallback", [], []

        full_key = f"{section}.{key}" if section else key
        key_parts = full_key.split(".")
        value_present = value not in {"false", '""', "''", "[]", "{}"}
        if full_key == "include":
            parsed_includes = parse_fallback_include(value)
            if parsed_includes is None:
                return "unsupported_toml_fallback", [], []
            includes.extend(parsed_includes)
        elif key_parts[0] == "paths" and value_present:
            controls.append("paths")
        elif key_parts[0] == "patch" and len(key_parts) >= 2 and value_present:
            controls.append(f"patch.{key_parts[1]}")
        elif key_parts[0] == "replace" and value_present:
            controls.append("replace")
        elif (
            key_parts[0] == "source"
            and len(key_parts) >= 3
            and key_parts[-1] in {
                "replace-with",
                "registry",
                "local-registry",
                "directory",
                "git",
                "branch",
                "tag",
                "rev",
            }
            and value_present
        ):
            controls.append(
                f"source.{'.'.join(key_parts[1:-1])}.{key_parts[-1]}"
            )
    return "ok_fallback", sorted(set(controls)), includes

def shadowed_record(path, origin, precedence):
    resolved = path.resolve(strict=False)
    record = {
        "path": display_path(resolved),
        "path_hash": path_hash(resolved),
        "origin": origin,
        "precedence": precedence,
        "effective": False,
        "external": not is_within(resolved, project_root),
        "included_by": None,
        "optional": False,
        "parse_status": "shadowed_by_legacy_config",
        "content_hash": None,
        "byte_count": None,
        "resolution_controls": [],
    }
    try:
        raw = resolved.read_bytes()
    except OSError:
        pass
    else:
        record["content_hash"] = "sha256:" + hashlib.sha256(raw).hexdigest()
        record["byte_count"] = len(raw)
    sources.append(record)

def process_config(
    path,
    origin,
    precedence,
    *,
    included_by=None,
    optional=False,
    inherited_external=False,
):
    resolved = path.resolve(strict=False)
    key = str(resolved)
    external = inherited_external or not is_within(resolved, project_root)
    if key in seen_effective:
        record = sources[seen_effective[key]]
        if external:
            record["external"] = True
        return

    record = {
        "path": display_path(resolved),
        "path_hash": path_hash(resolved),
        "origin": origin,
        "precedence": precedence,
        "effective": True,
        "external": external,
        "included_by": included_by,
        "optional": optional,
        "parse_status": "not_read",
        "content_hash": None,
        "byte_count": None,
        "resolution_controls": [],
    }
    seen_effective[key] = len(sources)
    sources.append(record)

    if not resolved.exists():
        record["parse_status"] = "optional_missing" if optional else "required_missing"
        return
    try:
        raw = resolved.read_bytes()
    except OSError:
        record["parse_status"] = "unreadable"
        return
    record["content_hash"] = "sha256:" + hashlib.sha256(raw).hexdigest()
    record["byte_count"] = len(raw)
    try:
        config_text = raw.decode("utf-8")
    except UnicodeDecodeError:
        record["parse_status"] = "invalid_toml"
        return
    if tomllib is None:
        parse_status, controls, parsed_includes = fallback_resolution_scan(
            config_text
        )
        record["parse_status"] = parse_status
        record["resolution_controls"] = controls
        if parse_status != "ok_fallback":
            return
    else:
        try:
            payload = tomllib.loads(config_text)
        except tomllib.TOMLDecodeError:
            record["parse_status"] = "invalid_toml"
            return
        record["parse_status"] = "ok"
        record["resolution_controls"] = resolution_controls(payload)
        parsed_includes = include_specs(payload)

    for include_path, include_optional in parsed_includes:
        candidate = Path(include_path).expanduser()
        if not candidate.is_absolute():
            candidate = resolved.parent / candidate
        process_config(
            candidate,
            "include",
            precedence,
            included_by=record["path"],
            optional=include_optional,
            inherited_external=external,
        )

def select_config(config_dir, origin, precedence):
    legacy = config_dir / "config"
    preferred = config_dir / "config.toml"
    if legacy.exists():
        process_config(legacy, origin, precedence)
        if preferred.exists():
            shadowed_record(preferred, origin, precedence)
    elif preferred.exists():
        process_config(preferred, origin, precedence)

cursor = project_root
depth = 0
while True:
    origin = "project" if depth == 0 else "ancestor"
    select_config(cursor / ".cargo", origin, f"hierarchy:{depth}")
    if cursor.parent == cursor:
        break
    cursor = cursor.parent
    depth += 1

select_config(cargo_home, "cargo_home", "cargo_home:lowest")

config_values = []
index = 2
while index < len(command):
    argument = command[index]
    if argument == "--":
        break
    if argument == "--config" and index + 1 < len(command):
        config_values.append(command[index + 1])
        index += 2
        continue
    if isinstance(argument, str) and argument.startswith("--config="):
        config_values.append(argument.split("=", 1)[1])
    index += 1

for config_index, value in enumerate(config_values, start=1):
    candidate = Path(value).expanduser()
    if not candidate.is_absolute():
        candidate = project_root / candidate
    if candidate.exists():
        process_config(candidate, "command_file", f"command:{config_index}")
        continue

    raw = value.encode("utf-8", "replace")
    record = {
        "path": f"<command-line:{config_index}>",
        "path_hash": None,
        "origin": "command_inline",
        "precedence": f"command:{config_index}",
        "effective": True,
        "external": False,
        "included_by": None,
        "optional": False,
        "parse_status": "not_read",
        "content_hash": "sha256:" + hashlib.sha256(raw).hexdigest(),
        "byte_count": len(raw),
        "resolution_controls": [],
    }
    if tomllib is None:
        parse_status, controls, _ = fallback_resolution_scan(value)
        record["parse_status"] = parse_status
        record["resolution_controls"] = controls
    else:
        try:
            payload = tomllib.loads(value)
        except tomllib.TOMLDecodeError:
            record["parse_status"] = "invalid_toml"
        else:
            record["parse_status"] = "ok"
            record["resolution_controls"] = resolution_controls(payload)
    sources.append(record)

external_controls = [
    source
    for source in sources
    if source["effective"]
    and source["external"]
    and source["resolution_controls"]
]
indeterminate_statuses = {
    "required_missing",
    "unreadable",
    "invalid_toml",
    "parser_unavailable",
    "unsupported_toml_fallback",
}
external_indeterminate = [
    source
    for source in sources
    if source["effective"]
    and source["external"]
    and source["parse_status"] in indeterminate_statuses
]
should_block = (
    source_attested
    and command_locked
    and bool(external_controls or external_indeterminate)
)
if not command_kind.startswith("cargo_"):
    status = "not_applicable"
elif should_block:
    status = "blocked"
elif external_indeterminate:
    status = "indeterminate"
elif external_controls:
    status = "observed"
else:
    status = "clean"

external_resolution_sources = []
for source in [*external_controls, *external_indeterminate]:
    summary = {
        "path": source["path"],
        "path_hash": source["path_hash"],
        "content_hash": source["content_hash"],
        "parse_status": source["parse_status"],
        "resolution_controls": source["resolution_controls"],
    }
    if summary not in external_resolution_sources:
        external_resolution_sources.append(summary)
blocking_sources = external_resolution_sources if should_block else []

provenance_material = json.dumps(
    {
        "project_root": "<project>",
        "cargo_home": display_path(cargo_home),
        "sources": sources,
        "source_attested": source_attested,
        "command_locked": command_locked,
    },
    sort_keys=True,
    separators=(",", ":"),
)
provenance_hash = "sha256:" + hashlib.sha256(
    provenance_material.encode("utf-8")
).hexdigest()
refusal_reason = None
repair = None
if should_block:
    refusal_reason = (
        "source-attested --locked verification depends on external Cargo "
        "configuration that can alter dependency resolution"
    )
    repair = (
        "Use an isolated CARGO_HOME and project/export ancestry with registry/git "
        "cache access but no resolution-altering Cargo config, then rerun. For a "
        "checkout below HOME, committed-tree mode can materialize outside HOME."
    )

payload = {
    "schema": "ee.rch.cargo_config_provenance.v1",
    "status": status,
    "source_attested": source_attested,
    "command_locked": command_locked,
    "project_root": "<project>",
    "cargo_home": display_path(cargo_home),
    "cargo_home_explicit": bool(cargo_home_value),
    "sources": sources,
    "external_resolution_sources": external_resolution_sources,
    "blocking_sources": blocking_sources,
    "provenance_hash": provenance_hash,
    "refusal_reason": refusal_reason,
    "repair": repair,
}
print(json.dumps(payload, sort_keys=True, separators=(",", ":")))
PY
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

rch_canonical_project_root() {
    local configured_root="${RCH_CANONICAL_PROJECT_ROOT:-}"
    local topology_root
    if [ -n "$configured_root" ]; then
        topology_root="$configured_root"
    elif [ "$PINNED_FRANKEN_STACK" -eq 1 ]; then
        topology_root="$(dirname "$PROJECT_ROOT")"
    else
        topology_root="$(dirname "$(dirname "$PROJECT_ROOT")")"
    fi
    if [ -d "$topology_root" ]; then
        (
            cd "$topology_root"
            pwd -P
        )
    else
        printf '%s\n' "$topology_root"
    fi
}

rch_alias_project_root() {
    local configured_root="${RCH_ALIAS_PROJECT_ROOT:-}"
    local topology_hash topology_root
    if [ -n "$configured_root" ]; then
        printf '%s\n' "$configured_root"
        return
    fi
    if [ "$PINNED_FRANKEN_STACK" -ne 1 ]; then
        printf '%s\n' "$DEFAULT_RCH_ALIAS_PROJECT_ROOT"
        return
    fi

    topology_root="$(rch_canonical_project_root)"
    topology_hash="$(
        RCH_PINNED_TOPOLOGY_ROOT="$topology_root" python3 - <<'PY'
import hashlib
import os

root = os.environ["RCH_PINNED_TOPOLOGY_ROOT"].encode("utf-8")
print(hashlib.sha256(root).hexdigest()[:16])
PY
    )"
    printf '/tmp/ee-rch-pinned-%s\n' "$topology_hash"
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
        "RCH_WORKER=${RCH_WORKER:-}" \
        "RCH_WORKERS=${RCH_WORKERS:-}" \
        "RCH_COMPRESSION=${RCH_COMPRESSION:-0}" \
        "RCH_ENV_ALLOWLIST=$(rch_env_allowlist)" \
        "RCH_REQUIRE_REMOTE=1" \
        "RCH_QUEUE_WHEN_BUSY=${RCH_QUEUE_WHEN_BUSY:-1}" \
        "RCH_TEST_SLOTS=${RCH_TEST_SLOTS:-2}" \
        "RCH_BUILD_TIMEOUT_SEC=${RCH_BUILD_TIMEOUT_SEC:-}" \
        "RCH_TEST_TIMEOUT_SEC=${RCH_TEST_TIMEOUT_SEC:-}" \
        "RCH_DAEMON_WAIT_RESPONSE_TIMEOUT_SECS=${RCH_DAEMON_WAIT_RESPONSE_TIMEOUT_SECS:-900}" \
        "RCH_DAEMON_RESPONSE_TIMEOUT_SECS=${RCH_DAEMON_RESPONSE_TIMEOUT_SECS:-900}" \
        "RCH_CANONICAL_PROJECT_ROOT=$(rch_canonical_project_root)" \
        "RCH_ALIAS_PROJECT_ROOT=$(rch_alias_project_root)" \
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
        "RCH_WORKER=" \
        "RCH_WORKERS=$preferred_workers" \
        "RCH_COMPRESSION=${RCH_COMPRESSION:-0}" \
        "RCH_ENV_ALLOWLIST=$(rch_env_allowlist)" \
        "RCH_REQUIRE_REMOTE=1" \
        "RCH_QUEUE_WHEN_BUSY=${RCH_QUEUE_WHEN_BUSY:-1}" \
        "RCH_TEST_SLOTS=${RCH_TEST_SLOTS:-2}" \
        "RCH_BUILD_TIMEOUT_SEC=${RCH_BUILD_TIMEOUT_SEC:-}" \
        "RCH_TEST_TIMEOUT_SEC=${RCH_TEST_TIMEOUT_SEC:-}" \
        "RCH_DAEMON_WAIT_RESPONSE_TIMEOUT_SECS=${RCH_DAEMON_WAIT_RESPONSE_TIMEOUT_SECS:-900}" \
        "RCH_DAEMON_RESPONSE_TIMEOUT_SECS=${RCH_DAEMON_RESPONSE_TIMEOUT_SECS:-900}" \
        "RCH_CANONICAL_PROJECT_ROOT=$(rch_canonical_project_root)" \
        "RCH_ALIAS_PROJECT_ROOT=$(rch_alias_project_root)" \
        "RCH_VISIBILITY=${RCH_VISIBILITY:-summary}" \
        "${RCH_INVOCATION[@]}"
    local status=$?
    cat "$RCH_ATTEMPT_STDOUT_FILE"
    cat "$RCH_ATTEMPT_STDERR_FILE"
    return "$status"
}

rch_queue_snapshot_json() {
    local queue_output queue_status queue_timed_out queue_elapsed queue_probe
    if [ -n "${RCH_VERIFY_FAKE_QUEUE_JSON:-}" ]; then
        queue_output="$RCH_VERIFY_FAKE_QUEUE_JSON"
        queue_status=0
        queue_timed_out=false
        queue_elapsed=0
    elif [ -n "${RCH_VERIFY_FAKE_OUTPUT:-}" ]; then
        printf 'null'
        return 0
    else
        queue_probe="$(capture_command_with_timeout "$RCH_VERIFY_PREFLIGHT_TIMEOUT_MS" "$PROJECT_ROOT" "$RCH_BIN" queue --json)"
        queue_output="$(json_text_field "$queue_probe" output)"
        queue_status="$(json_text_field "$queue_probe" status)"
        queue_timed_out="$(json_text_field "$queue_probe" timed_out)"
        queue_elapsed="$(json_text_field "$queue_probe" elapsed_ms)"
    fi

    RCH_QUEUE_OUTPUT="$queue_output" \
    RCH_QUEUE_STATUS="$queue_status" \
    RCH_QUEUE_TIMED_OUT="$queue_timed_out" \
    RCH_QUEUE_ELAPSED="$queue_elapsed" \
    python3 - <<'PY'
import json
import os

def parse_int(value, default=0):
    try:
        return int(value)
    except (TypeError, ValueError):
        return default

status = parse_int(os.environ.get("RCH_QUEUE_STATUS"))
timed_out = (os.environ.get("RCH_QUEUE_TIMED_OUT") or "").lower() == "true"
elapsed_ms = parse_int(os.environ.get("RCH_QUEUE_ELAPSED"))
raw_output = os.environ.get("RCH_QUEUE_OUTPUT") or ""

try:
    parsed = json.loads(raw_output)
except Exception:
    parsed = None

if isinstance(parsed, dict):
    data = parsed.get("data") if isinstance(parsed.get("data"), dict) else parsed
else:
    data = {}

payload = {
    "status": "ok" if status == 0 and isinstance(data, dict) else "unavailable",
    "exit_code": status,
    "timed_out": timed_out,
    "elapsed_ms": elapsed_ms,
    "data": data if isinstance(data, dict) else {},
}
print(json.dumps(payload, sort_keys=True, separators=(",", ":")))
PY
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

emit_worker_root_canary_json() {
    WORKER_ROOT_CANARY_NOW="$(now_iso)" \
    PROJECT_ROOT_PATH="$PROJECT_ROOT" \
    RCH_BIN_PATH="$RCH_BIN" \
    CANARY_TIMEOUT_MS="$RCH_VERIFY_PREFLIGHT_TIMEOUT_MS" \
    CANARY_STATUS_JSON="${RCH_VERIFY_WORKER_ROOT_CANARY_STATUS_JSON:-}" \
    CANARY_DIAGNOSE_JSON="${RCH_VERIFY_WORKER_ROOT_CANARY_DIAGNOSE_JSON:-}" \
    CANARY_STATUS_TIMEOUT="${RCH_VERIFY_WORKER_ROOT_CANARY_STATUS_TIMEOUT:-0}" \
    CANARY_DIAGNOSE_TIMEOUT="${RCH_VERIFY_WORKER_ROOT_CANARY_DIAGNOSE_TIMEOUT:-0}" \
    python3 - <<'PY'
import json
import os
import re
import subprocess
import time

now = os.environ["WORKER_ROOT_CANARY_NOW"]
project_root = os.environ.get("PROJECT_ROOT_PATH") or ""
rch_bin = os.environ.get("RCH_BIN_PATH") or "rch"
timeout_ms = int(os.environ.get("CANARY_TIMEOUT_MS") or "10000")

def redact(text):
    if text is None:
        return None
    text = str(text)
    text = re.sub(r"\x1b\[[0-9;]*m", "", text)
    text = re.sub(r"/Users/[^\\s\"'`,;:]+", "/Users/<redacted>", text)
    return text

def probe(name, argv, env_var, timeout_var):
    started = time.monotonic()
    forced = os.environ.get(timeout_var) == "1"
    injected = os.environ.get(env_var)
    if forced:
        return {
            "status": "timeout",
            "exit_code": 124,
            "timed_out": True,
            "elapsed_ms": timeout_ms,
            "payload": None,
            "raw_excerpt": "",
        }
    if injected:
        try:
            payload = json.loads(injected)
        except Exception as error:
            return {
                "status": "unavailable",
                "exit_code": 0,
                "timed_out": False,
                "elapsed_ms": int((time.monotonic() - started) * 1000),
                "payload": None,
                "raw_excerpt": redact(f"{name} fixture JSON parse error: {error}"),
            }
        return {
            "status": "ok",
            "exit_code": 0,
            "timed_out": False,
            "elapsed_ms": int((time.monotonic() - started) * 1000),
            "payload": payload,
            "raw_excerpt": "",
        }
    try:
        result = subprocess.run(
            argv,
            check=False,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            timeout=timeout_ms / 1000,
        )
    except subprocess.TimeoutExpired as error:
        return {
            "status": "timeout",
            "exit_code": 124,
            "timed_out": True,
            "elapsed_ms": timeout_ms,
            "payload": None,
            "raw_excerpt": redact((error.stdout or "") + (error.stderr or ""))[-1200:],
        }
    except OSError as error:
        return {
            "status": "unavailable",
            "exit_code": 126,
            "timed_out": False,
            "elapsed_ms": int((time.monotonic() - started) * 1000),
            "payload": None,
            "raw_excerpt": redact(f"{type(error).__name__}: {error}")[-1200:],
        }
    raw = result.stdout or ""
    try:
        payload = json.loads(raw)
    except Exception:
        return {
            "status": "unavailable",
            "exit_code": result.returncode,
            "timed_out": False,
            "elapsed_ms": int((time.monotonic() - started) * 1000),
            "payload": None,
            "raw_excerpt": redact((raw + (result.stderr or ""))[-1200:]),
        }
    return {
        "status": "ok" if result.returncode == 0 else "unavailable",
        "exit_code": result.returncode,
        "timed_out": False,
        "elapsed_ms": int((time.monotonic() - started) * 1000),
        "payload": payload,
        "raw_excerpt": "" if result.returncode == 0 else redact((raw + (result.stderr or ""))[-1200:]),
    }

status_probe = probe(
    "status",
    [rch_bin, "status", "--workers", "--jobs", "--json"],
    "CANARY_STATUS_JSON",
    "CANARY_STATUS_TIMEOUT",
)
diagnose_probe = probe(
    "diagnose",
    [rch_bin, "diagnose", "--dry-run", "--json", "cargo", "check", "--lib"],
    "CANARY_DIAGNOSE_JSON",
    "CANARY_DIAGNOSE_TIMEOUT",
)

def payload_data(probe_payload):
    payload = probe_payload.get("payload")
    if not isinstance(payload, dict):
        return {}
    data = payload.get("data")
    return data if isinstance(data, dict) else payload

status_data = payload_data(status_probe)
diagnose_data = payload_data(diagnose_probe)
daemon_container = status_data.get("daemon") if isinstance(status_data.get("daemon"), dict) else {}
daemon_workers = daemon_container.get("workers")
if not isinstance(daemon_workers, list):
    daemon_workers = []

selection = diagnose_data.get("worker_selection") if isinstance(diagnose_data.get("worker_selection"), dict) else {}
diagnostics = selection.get("diagnostics") if isinstance(selection.get("diagnostics"), dict) else {}
diag_workers = diagnostics.get("workers") if isinstance(diagnostics.get("workers"), list) else []
selected = selection.get("worker")
if isinstance(selected, dict):
    selected_worker = selected.get("id") or selected.get("worker_id")
elif selected:
    selected_worker = str(selected)
else:
    selected_worker = None

workers_by_id = {}
for worker in daemon_workers:
    if isinstance(worker, dict) and worker.get("id"):
        workers_by_id[str(worker.get("id"))] = {
            "worker_id": str(worker.get("id")),
            "status": worker.get("status"),
            "pressure_state": worker.get("pressure_state"),
            "available_slots": max(int(worker.get("total_slots") or 0) - int(worker.get("used_slots") or 0), 0),
            "reason_codes": [],
            "final_decision": None,
            "final_reason": None,
        }

for worker in diag_workers:
    if not isinstance(worker, dict):
        continue
    worker_id = str(worker.get("worker_id") or worker.get("id") or "")
    if not worker_id:
        continue
    item = workers_by_id.setdefault(worker_id, {"worker_id": worker_id})
    item["status"] = item.get("status") or worker.get("status")
    item["pressure_state"] = item.get("pressure_state") or worker.get("pressure_state")
    item["final_decision"] = worker.get("final_decision")
    item["final_reason"] = redact(worker.get("final_reason"))
    item["reason_codes"] = [
        str(code)
        for code in (worker.get("reason_codes") or [])
        if code is not None
    ]
    if "available_slots" not in item:
        try:
            item["available_slots"] = int(worker.get("available_slots") or 0)
        except (TypeError, ValueError):
            item["available_slots"] = None

def root_status(root_id, path):
    return {
        "id": root_id,
        "path": path,
        "status": "unknown",
        "accepted_by_policy": None,
        "worker_ids": [],
        "evidence": [],
    }

roots = {
    "projects_root": root_status("projects_root", "/data/projects"),
    "dp_root": root_status("dp_root", "/dp"),
    "isolated_sync_parent": root_status("isolated_sync_parent", "<worker-isolated-sync-parent>"),
}
degraded = []

def mark(root_id, status, worker_id, evidence, accepted=None):
    root = roots[root_id]
    precedence = {
        "unknown": 0,
        "accepted": 1,
        "active_project_exclusion": 2,
        "missing_root": 3,
        "outer_workspace_shadowed": 4,
        "permission_denied": 5,
        "timeout": 6,
        "unavailable": 7,
    }
    if precedence.get(status, 0) >= precedence.get(root["status"], 0):
        root["status"] = status
        root["accepted_by_policy"] = accepted
    if worker_id and worker_id not in root["worker_ids"]:
        root["worker_ids"].append(worker_id)
    if evidence and evidence not in root["evidence"]:
        root["evidence"].append(evidence)

if status_probe["timed_out"] or diagnose_probe["timed_out"]:
    for root_id in roots:
        mark(root_id, "timeout", None, "bounded canary probe timed out", False)
    degraded.append("rch_worker_root_canary_timeout")
elif status_probe["status"] != "ok" or diagnose_probe["status"] != "ok":
    for root_id in roots:
        mark(root_id, "unavailable", None, "RCH status or diagnose JSON unavailable", False)
    degraded.append("rch_worker_root_canary_unavailable")
elif selected_worker:
    for root_id in roots:
        mark(root_id, "accepted", selected_worker, "dry-run selected a worker for cargo check --lib", True)

for worker_id, item in workers_by_id.items():
    reason = item.get("final_reason") or ""
    reason_lower = reason.lower()
    codes = set(item.get("reason_codes") or [])
    evidence = reason or ",".join(sorted(codes)) or "worker diagnostic row"
    if "active_project_exclusion" in codes:
        mark("isolated_sync_parent", "active_project_exclusion", worker_id, evidence, False)
        degraded.append("rch_worker_root_canary_active_project_exclusion")
    if "topology.preflight_failed" in codes or "topology" in reason_lower:
        target = "dp_root" if "/dp" in reason_lower else "projects_root"
        if "permission" in reason_lower or "denied" in reason_lower:
            mark(target, "permission_denied", worker_id, evidence, False)
            degraded.append("rch_worker_root_canary_permission_denied")
        elif "missing" in reason_lower or "not found" in reason_lower or "no such" in reason_lower:
            mark(target, "missing_root", worker_id, evidence, False)
            degraded.append("rch_worker_root_canary_missing_root")
        elif "alias_wrong_target" in reason_lower or "workspace" in reason_lower or "/users/" in reason_lower:
            mark(target, "outer_workspace_shadowed", worker_id, evidence, False)
            degraded.append("rch_worker_root_canary_outer_workspace_shadowed")
        else:
            mark(target, "missing_root", worker_id, evidence, False)
            degraded.append("rch_worker_root_canary_missing_root")

degraded = sorted(dict.fromkeys(degraded))
root_values = [roots[key] for key in ("projects_root", "dp_root", "isolated_sync_parent")]

if any(root["status"] in {"timeout"} for root in root_values):
    status = "timeout"
elif any(root["status"] in {"unavailable"} for root in root_values):
    status = "unavailable"
elif selected_worker and not degraded:
    status = "healthy"
else:
    status = "blocked"

repair_actions = []
if "rch_worker_root_canary_missing_root" in degraded:
    repair_actions.append({"kind": "operator", "command": "rch workers probe --all", "summary": "Confirm worker root inventory before retrying remote proof."})
if "rch_worker_root_canary_outer_workspace_shadowed" in degraded:
    repair_actions.append({"kind": "operator", "command": "rch diagnose --dry-run --json cargo check --lib", "summary": "Inspect alias/root preflight details; do not run local Cargo."})
if "rch_worker_root_canary_active_project_exclusion" in degraded:
    repair_actions.append({"kind": "wait", "command": "rch queue --json", "summary": "Wait for or inspect the active same-project remote build before launching another proof."})
if "rch_worker_root_canary_timeout" in degraded:
    repair_actions.append({"kind": "retry", "command": "scripts/rch_verify.sh --worker-root-canary --json", "summary": "Retry the bounded canary after RCH responds."})

payload = {
    "schema": "ee.rch.worker_root_canary.v1",
    "success": True,
    "generated_at": now,
    "status": status,
    "mode": "read_only_no_cargo",
    "project_root": redact(project_root),
    "timeout_ms": timeout_ms,
    "selected_worker": selected_worker,
    "required_roots": root_values,
    "workers": [workers_by_id[key] for key in sorted(workers_by_id)],
    "probes": {
        "status": {key: status_probe[key] for key in ("status", "exit_code", "timed_out", "elapsed_ms", "raw_excerpt")},
        "diagnose": {key: diagnose_probe[key] for key in ("status", "exit_code", "timed_out", "elapsed_ms", "raw_excerpt")},
    },
    "degraded_codes": degraded,
    "repair_actions": repair_actions,
}
print(json.dumps(payload, sort_keys=True, separators=(",", ":")))
PY
}

rch_env_allowlist() {
    local required="CARGO_TARGET_DIR,TMPDIR"
    if [ -n "${RCH_ENV_ALLOWLIST:-}" ]; then
        printf '%s,%s' "$required" "$RCH_ENV_ALLOWLIST"
    else
        printf '%s' "$required"
    fi
}

RUN_STARTED_AT="$(now_iso)"

emit_json() {
    local success="$1"
    local exit_code_json="$2"
    local elapsed_ms="$3"
    local stdout_tail="$4"
    local stderr_tail="$5"
    shift 5
    FRANKEN_STACK_JSON="$(refresh_franken_stack_cargo_lock_json)"
    if [ "$(json_text_field "$FRANKEN_STACK_JSON" cargo_lock_unchanged)" = "False" ]; then
        set -- "$@" "rch_verify_franken_stack_cargo_lock_changed"
    fi
    local degraded_codes_json
    degraded_codes_json="$(json_array "$@")"
    local command_json rch_invocation_json command_text_json remote_env_json stdout_json stderr_json requested_workers_json configured_workers_json daemon_workers_json build_admission_json rch_runtime_json known_blocker_json local_cargo_processes_json proof_broker_json cargo_config_provenance_json franken_stack_json
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
    proof_broker_json="${PROOF_BROKER_JSON:-null}"
    cargo_config_provenance_json="$CARGO_CONFIG_PROVENANCE_JSON"
    franken_stack_json="$FRANKEN_STACK_JSON"
    local source_state_json
    if [ -n "${SOURCE_STATE_JSON:-}" ]; then
        source_state_json="$SOURCE_STATE_JSON"
    else
        source_state_json='{"verification_attribution":"source_state_not_computed","git_head":null,"git_tree":null,"dirty_status_hash":"sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855","dirty_summary":{"total":0,"tracked":0,"untracked":0,"beads":0,"scratch":0,"secret_risk":0,"ignored":0,"unknown":0},"dirty_paths_sample":[],"remote_source_materialized":false,"source_materialization":"none","source_state_degraded_codes":["rch_verify_source_state_not_computed"]}'
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
{"schema":"ee.rch.verify.v1","success":$success,"generated_at":"$(now_iso)","command":$command_json,"command_text":$command_text_json,"command_kind":"$COMMAND_KIND","remote_env":$remote_env_json,"remote_required":true,"would_offload":$WOULD_OFFLOAD,"worker_id":$WORKER_ID_JSON,"requested_workers":$requested_workers_json,"configured_workers":$configured_workers_json,"daemon_workers":$daemon_workers_json,"remote_project_root":$REMOTE_PROJECT_ROOT_JSON,"remote_target_dir":$REMOTE_TARGET_DIR_JSON,"exit_code":$exit_code_json,"elapsed_ms":$elapsed_ms,"attempt_timeout_ms":$RCH_VERIFY_ATTEMPT_TIMEOUT_MS,"timed_out":$RCH_ATTEMPT_TIMED_OUT,"stdout_bytes":$RCH_STDOUT_BYTES,"stderr_bytes":$RCH_STDERR_BYTES,"stdout_tail":$stdout_json,"stderr_tail":$stderr_json,"artifacts":$artifacts_json,"degraded_codes":$degraded_codes_json,"rch_invocation":$rch_invocation_json,"build_admission":$build_admission_json,"rch_runtime":$rch_runtime_json,"known_blocker":$known_blocker_json,"proof_broker":$proof_broker_json,"local_cargo_processes":$local_cargo_processes_json,"cargo_config_provenance":$cargo_config_provenance_json,"franken_stack":$franken_stack_json,"source_state":$source_state_json}
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
    PROOF_BROKER_LEDGER_PATH="$PROOF_BROKER_LEDGER" \
    RCH_QUEUE_SNAPSHOT_JSON="${RCH_QUEUE_SNAPSHOT_JSON:-null}" \
    REMOTE_TIMEOUT_FINGERPRINT_VALUE="$(remote_timeout_fingerprint)" \
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
    "remote_source_materialized",
    "source_materialization",
    "source_state_degraded_codes",
    "requested_treeish",
    "resolved_commit",
    "source_manifest_hash",
    "dependency_manifest_hash",
    "source_bundle_hash",
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
    current_diagnostic = None
    for line in text.splitlines():
        stripped = line.lstrip()
        if re.match(r"error(?:\[[^\]]+\])?:", stripped):
            current_diagnostic = "error"
        elif re.match(r"warning(?:\[[^\]]+\])?:", stripped):
            current_diagnostic = "warning"
        match = re.search(r"-->\s+([^:\s][^:]*):(\d+):\d+", line)
        if match and current_diagnostic == "error":
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

def remote_setup_kill_details(proof, combined_tail):
    def int_value(value, default=0):
        if isinstance(value, bool) or value is None:
            return default
        try:
            return int(value)
        except (TypeError, ValueError):
            return default

    exit_code = int_value(proof.get("exit_code"), None)
    stdout_bytes = int_value(proof.get("stdout_bytes"))
    if exit_code != 241 or stdout_bytes != 0:
        return None

    text = combined_tail or ""
    verifier_output_patterns = (
        r"\bCompiling\b",
        r"\bChecking\b",
        r"\bFinished\b",
        r"\bRunning\b",
        r"\bDoc-tests?\b",
        r"\brunning\s+\d+\s+tests?\b",
        r"\btest result:",
        r"\berror(?:\[[^\]]+\])?:",
        r"\bwarning(?:\[[^\]]+\])?:",
        r"\bcould not compile\b",
        r"\bfailed to compile\b",
    )
    if any(re.search(pattern, text, flags=re.IGNORECASE) for pattern in verifier_output_patterns):
        return None

    elapsed_ms = int_value(proof.get("elapsed_ms"))
    stderr_bytes = int_value(proof.get("stderr_bytes"))
    elapsed_seconds = round(elapsed_ms / 1000, 3)
    return {
        "schema": "ee.rch.remote_setup_kill.v1",
        "signature": "exit_241_no_verifier_stdout",
        "stage": "rch_sync_or_setup",
        "exit_code": exit_code,
        "elapsed_ms": elapsed_ms,
        "elapsed_seconds": elapsed_seconds,
        "stdout_bytes": stdout_bytes,
        "stderr_bytes": stderr_bytes,
        "worker_id": proof.get("worker_id"),
        "message": (
            f"remote killed at {elapsed_seconds:g}s with exit 241 and "
            "0 verifier stdout bytes; RCH ended before Cargo/test output, "
            "likely during rsync or dependency-sync setup"
        ),
        "next_action": (
            "inspect RCH client/worker sync setup, worker disk, and daemon "
            "timeout posture; do not treat this as source verification failure"
        ),
    }

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
    selector_probe = proof.get("selector_admission_probe") if isinstance(proof, dict) else {}
    admission_blocker = (
        selector_probe.get("admission_blocker")
        if isinstance(selector_probe, dict)
        else None
    )
    if (
        isinstance(admission_blocker, dict)
        and admission_blocker.get("kind") == "active_project_exclusion"
    ):
        return "active_project_exclusion"
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
    if "rch_verify_worker_health_threshold_blocked" in degraded_codes:
        return "worker_health_threshold"
    if "rch_verify_remote_transport_timeout" in degraded_codes:
        return "remote_transport_timeout"
    if "rch_verify_capacity_or_timeout" in degraded_codes:
        return "capacity_or_timeout"
    if "rch_verify_topology_blocked" in degraded_codes:
        return "topology_blocked"
    if "rch_verify_local_fallback_refused" in degraded_codes:
        return "local_fallback_refused"
    return None

def parse_queue_snapshot():
    raw = os.environ.get("RCH_QUEUE_SNAPSHOT_JSON") or "null"
    try:
        snapshot = json.loads(raw)
    except Exception:
        return {}
    if not isinstance(snapshot, dict):
        return {}
    data = snapshot.get("data")
    if isinstance(data, dict):
        return data
    return snapshot

def int_or_none(value):
    if isinstance(value, bool) or value is None:
        return None
    if isinstance(value, int):
        return value
    if isinstance(value, str):
        try:
            return int(value)
        except ValueError:
            return None
    return None

def active_project_build_ids_from_tail(combined_tail):
    ordered_ids = []
    seen = set()
    for pattern in (
        r"\bactive_build(?:_id)?\s*[=:]\s*(\d+)\b",
        r"\bactive build(?: id)?\s*[=:]\s*(\d+)\b",
    ):
        for match in re.finditer(pattern, combined_tail, flags=re.IGNORECASE):
            parsed = int_or_none(match.group(1))
            if parsed is not None and parsed not in seen:
                seen.add(parsed)
                ordered_ids.append(parsed)
    return ordered_ids

def active_project_exclusion_count_from_tail(combined_tail):
    for pattern in (
        r"\bactive_project_exclusion\s*[=:]\s*(\d+)\b",
        r"\bactive project exclusion\s*[=:]\s*(\d+)\b",
    ):
        match = re.search(pattern, combined_tail, flags=re.IGNORECASE)
        if match:
            return int_or_none(match.group(1))
    return None

def active_project_queue_details(combined_tail):
    tail_build_ids = active_project_build_ids_from_tail(combined_tail)
    queue_data = parse_queue_snapshot()
    details = {}
    if tail_build_ids:
        details["active_build_id"] = tail_build_ids[0]
    exclusion_count = active_project_exclusion_count_from_tail(combined_tail)
    if exclusion_count is not None:
        details["active_project_exclusion_count"] = exclusion_count

    global_numeric_fields = {
        "workers_healthy": queue_data.get("workers_healthy") if isinstance(queue_data, dict) else None,
        "workers_total": queue_data.get("workers_total") if isinstance(queue_data, dict) else None,
        "slots_available": queue_data.get("slots_available") if isinstance(queue_data, dict) else None,
        "slots_total": queue_data.get("slots_total") if isinstance(queue_data, dict) else None,
    }
    for key, value in global_numeric_fields.items():
        parsed = int_or_none(value)
        if parsed is not None:
            details[key] = parsed

    active_builds = queue_data.get("active_builds") if isinstance(queue_data, dict) else None
    if not isinstance(active_builds, list) or not active_builds:
        return details

    build = None
    if tail_build_ids:
        tail_build_id_set = set(tail_build_ids)
        build = next(
            (
                item
                for item in active_builds
                if isinstance(item, dict) and int_or_none(item.get("id")) in tail_build_id_set
            ),
            None,
        )
    else:
        build = next((item for item in active_builds if isinstance(item, dict)), None)
    if not isinstance(build, dict):
        return details

    numeric_fields = {
        "active_build_id": build.get("id"),
        "heartbeat_age_secs": build.get("heartbeat_age_secs"),
        "progress_age_secs": build.get("progress_age_secs"),
        "build_age_secs": build.get("detector_build_age_secs"),
        "slots_owned": build.get("detector_slots_owned") if build.get("detector_slots_owned") is not None else build.get("slots"),
    }
    for key, value in numeric_fields.items():
        parsed = int_or_none(value)
        if parsed is not None:
            details[key] = parsed

    command = build.get("command")
    if isinstance(command, str) and command.strip():
        details["active_command_preview"] = redact(command.strip())[:180]
        details["active_command_hash"] = "sha256:" + hashlib.sha256(command.encode("utf-8")).hexdigest()
    worker_id = build.get("worker_id")
    if isinstance(worker_id, str) and worker_id.strip():
        details["worker_id"] = worker_id.strip()

    if build.get("detector_heartbeat_stale") is True:
        worker_posture = "heartbeat_stale"
    elif build.get("detector_progress_stale") is True:
        worker_posture = "progress_stale"
    elif build.get("detector_hook_alive") is False:
        worker_posture = "hook_inactive"
    else:
        worker_posture = "active"
    details["worker_posture"] = worker_posture
    details["retry_after_hint"] = "after_active_build_completes"
    details["next_action"] = "wait_for_active_build_or_contact_owner_before_retry"
    details["owner_escalation"] = "identify_or_contact_active_build_owner_before_cancelling_or_retrying"
    return details

def selector_admission_probe(proof, degraded_codes, combined_tail):
    command_kind = proof.get("command_kind") or ""
    required_runtime = "Rust" if command_kind.startswith("cargo_") else None
    workers_reported = [str(item) for item in proof.get("configured_workers") or []]
    daemon_workers_reported = [str(item) for item in proof.get("daemon_workers") or []]
    selected_worker = proof.get("worker_id")
    lowered_tail = combined_tail.lower()
    known_blocker_active = "rch_verify_known_blocker_active" in degraded_codes
    local_fallback_refused = (
        "rch_verify_local_fallback_refused" in degraded_codes
        or "remote required; refusing local fallback" in combined_tail
    )
    active_project_match = re.search(
        r"(?im)^\s*(?:\x1b\[[0-9;]*m)*\[RCH\].*(?:active_project_exclusion\s*[=:]|active project exclusion(?:\s*[=:]|\s|$)).*$",
        combined_tail,
    )
    active_project_exclusion = (
        proof.get("exit_code") not in (None, 0)
        and not selected_worker
        and active_project_match is not None
    )
    admission_blocker = None
    if active_project_exclusion:
        evidence_line = redact(active_project_match.group(0).strip())[:320]
        admission_blocker = {
            "kind": "active_project_exclusion",
            "retry_guidance": "wait_for_active_build_or_coordinate_with_owner",
            "evidence": evidence_line,
            "retry_after_hint": "after_active_build_completes",
            "next_action": "wait_for_active_build_or_contact_owner_before_retry",
            "owner_escalation": "identify_or_contact_active_build_owner_before_cancelling_or_retrying",
        }
        admission_blocker.update(active_project_queue_details(combined_tail))
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
    if known_blocker_active or "rch_verify_dry_run" in degraded_codes:
        status = "not_applicable"
    elif required_runtime is None:
        status = "not_applicable"
    elif selected_worker:
        status = "selected"
    else:
        status = "selection_failed"
        if "no workers with rust installed" in lowered_tail:
            selection_failure_reason = "no_workers_with_rust_installed"
        elif "no workers passed health thresholds" in lowered_tail or "no_workers_passed_health" in lowered_tail:
            selection_failure_reason = "no_workers_passed_health"
        elif active_project_exclusion:
            selection_failure_reason = "active_project_exclusion"
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
                "no_workers_passed_health",
                "no_worker_selected",
                "remote_marker_missing",
            }
        ),
        "path_normalization_warning": path_warning,
        "remote_required": proof.get("remote_required") is True,
        "local_fallback_refused": bool(local_fallback_refused),
        "admission_blocker": admission_blocker,
    }

def remediation_bead_for(blocker_kind):
    mapping = {
        "cargo_workspace_inheritance": "bd-17c65.10.17.1.3",
        "cargo_path_dependency_version": "bd-17c65.10.17.1.3",
        "client_daemon_version_skew": "bd-17c65.10.17.1.4",
        "remote_checkout_incomplete": "bd-17c65.10.17.1.3",
        "worker_disk_full": "bd-17c65.10.17",
        "all_workers_preflight_failed": "bd-17c65.10.19",
        "worker_health_threshold": "bd-37ugy",
        "remote_transport_timeout": "bd-37ugy",
        "active_project_exclusion": "bd-1n3x1.13",
        "capacity_or_timeout": "bd-17c65.10.17",
        "topology_blocked": "bd-17c65.10.17.1.2",
        "local_fallback_refused": "bd-17c65.10.17.1",
    }
    return mapping.get(blocker_kind, "bd-17c65.10.17.1")

def known_blocker_entry(blocker_kind, degraded_codes, command_hash):
    source_state_hash = (
        proof.get("source_bundle_hash")
        or proof.get("source_manifest_hash")
        or proof.get("dirty_status_hash")
    )
    runtime = proof.get("rch_runtime") or {}
    details = proof.get("cargo_workspace_inheritance") or proof.get("cargo_path_dependency_version") or {}
    selector_probe = proof.get("selector_admission_probe") if isinstance(proof, dict) else {}
    admission_blocker = (
        selector_probe.get("admission_blocker")
        if isinstance(selector_probe, dict)
        else None
    )
    active_project_details = {}
    if blocker_kind == "active_project_exclusion" and isinstance(admission_blocker, dict):
        for key in (
            "active_project_exclusion_count",
            "active_build_id",
            "active_command_preview",
            "active_command_hash",
            "worker_id",
            "worker_posture",
            "heartbeat_age_secs",
            "progress_age_secs",
            "build_age_secs",
            "slots_owned",
            "workers_healthy",
            "workers_total",
            "slots_available",
            "slots_total",
            "retry_after_hint",
            "next_action",
            "owner_escalation",
        ):
            value = admission_blocker.get(key)
            if value is not None:
                active_project_details[key] = value
    active_project_fingerprint = {}
    for key in (
        "active_build_id",
        "active_command_hash",
        "worker_id",
        "worker_posture",
    ):
        value = active_project_details.get(key)
        if value is not None:
            active_project_fingerprint[key] = value
    remote_timeout_fingerprint = (
        os.environ.get("REMOTE_TIMEOUT_FINGERPRINT_VALUE")
        or "build:unset,test:unset"
    )
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
        "remote_timeout_fingerprint": remote_timeout_fingerprint,
        "runtime_fingerprint": runtime_fingerprint,
        "dependency": details.get("dependency") or details.get("crate"),
        "manifest_path": details.get("manifest_path") or details.get("location_searched"),
        "active_project_exclusion": active_project_fingerprint or None,
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
    entry = {
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
        "remote_timeout_fingerprint": remote_timeout_fingerprint,
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
    if active_project_details:
        entry["active_project_exclusion"] = active_project_details
    return entry

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

def proof_broker_run_id(command_hash, completed_at):
    payload = f"{command_hash}\0{completed_at or ''}"
    return "vrun_" + hashlib.sha256(payload.encode("utf-8")).hexdigest()[:24]

def proof_broker_row_id(fingerprint_id, run_id):
    payload = f"{fingerprint_id or ''}\0{run_id or ''}"
    return "prow_" + hashlib.sha256(payload.encode("utf-8")).hexdigest()[:24]

def persist_proof_broker_ledger(proof, status, command_hash):
    proof_broker = proof.get("proof_broker")
    if not isinstance(proof_broker, dict) or proof_broker.get("remoteCargoLaunched") is not True:
        return
    ledger_path = os.environ.get("PROOF_BROKER_LEDGER_PATH") or ""
    if not ledger_path:
        return
    if no_write:
        proof_broker["ledgerWrite"] = {"status": "suppressed", "reason": "--no-write"}
        return
    fingerprint = proof_broker.get("fingerprint")
    if not isinstance(fingerprint, dict) or not fingerprint.get("fingerprintId"):
        proof_broker["ledgerWrite"] = {
            "status": "skipped",
            "reason": "proof broker admission response did not include a fingerprint",
        }
        return
    exit_code = proof.get("exit_code")
    completed = exit_code == 0
    run_id = proof_broker_run_id(command_hash, proof.get("generated_at"))
    row = {
        "schema": "ee.proof_broker.v1",
        "rowId": proof_broker_row_id(fingerprint.get("fingerprintId"), run_id),
        "fingerprint": fingerprint,
        "state": "completed" if completed else "rejected",
        "admission": {
            "verdict": "reuse_existing" if completed else "proof_unusable",
            "reasonCodes": ["completed_remote_proof"] if completed else ["remote_proof_failed"],
            "nextAction": "cite_existing_proof" if completed else "inspect_failure_before_rerun",
            "reuseRunId": run_id if completed else None,
            "waitOwner": None,
        },
        "runId": run_id,
        "owner": None,
        "createdAt": proof.get("generated_at"),
        "startedAt": started_at,
        "completedAt": proof.get("generated_at"),
        "expiresAt": None,
        "sourceStateValidUntil": None,
        "invalidationReasons": proof.get("source_state_degraded_codes") or [],
        "evidenceRefs": [
            {
                "kind": "rch_verify",
                "id": proof.get("generated_at") or run_id,
                "contentHash": "sha256:" + hashlib.sha256(
                    json.dumps(
                        {
                            "command_hash": command_hash,
                            "status": status,
                            "exit_code": exit_code,
                            "worker_id": proof.get("worker_id"),
                            "completed_at": proof.get("generated_at"),
                        },
                        sort_keys=True,
                        separators=(",", ":"),
                    ).encode("utf-8")
                ).hexdigest(),
                "redacted": True,
            }
        ],
        "rawOutputIncluded": False,
    }
    path = Path(ledger_path)
    try:
        if path.exists():
            existing = json.loads(path.read_text(encoding="utf-8") or "[]")
        else:
            existing = []
        if not isinstance(existing, list):
            existing = []
        fingerprint_id = fingerprint.get("fingerprintId")
        records = [
            item
            for item in existing
            if not (
                isinstance(item, dict)
                and isinstance(item.get("fingerprint"), dict)
                and item["fingerprint"].get("fingerprintId") == fingerprint_id
            )
        ]
        records.append(row)
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(
            json.dumps(records, sort_keys=True, separators=(",", ":")) + "\n",
            encoding="utf-8",
        )
    except OSError as error:
        proof_broker["ledgerWrite"] = {"status": "failed", "message": redact(str(error))}
    except Exception as error:
        proof_broker["ledgerWrite"] = {"status": "failed", "message": redact(str(error))}
    else:
        proof_broker["ledgerWrite"] = {
            "status": "updated",
            "state": row["state"],
            "rowId": row["rowId"],
            "runId": run_id,
        }

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
remote_setup_kill = remote_setup_kill_details(proof, combined_tail)
if remote_setup_kill:
    proof["remote_setup_kill"] = remote_setup_kill
    for code in (
        "rch_verify_remote_command_failed",
        "rch_verify_remote_transport_timeout",
        "rch_verify_capacity_or_timeout",
    ):
        if code not in degraded:
            degraded.append(code)
    proof["degraded_codes"] = degraded
proof_broker = proof.get("proof_broker") if isinstance(proof.get("proof_broker"), dict) else {}
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
    "rch_verify_worker_health_threshold_blocked",
    "rch_verify_remote_transport_timeout",
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
proof_broker_bypassed = "rch_verify_proof_broker_bypassed" in degraded
proof_broker_refusal_codes = set()
if not proof_broker_bypassed:
    proof_broker_refusal_codes = {
        code
        for code in degraded
        if code.startswith("rch_verify_proof_broker_")
        and code not in {
            "rch_verify_proof_broker_reuse_existing",
            "rch_verify_proof_broker_bypassed",
        }
    }

if proof.get("success") is not True:
    status = "refused"
elif "rch_verify_known_blocker_active" in degraded:
    status = "known_blocker_refused"
    proof["verification_attribution"] = "not_run_known_blocker"
elif "rch_verify_proof_broker_reuse_existing" in degraded:
    status = "proof_broker_reuse"
    proof["verification_attribution"] = "not_run_proof_broker_reuse"
elif proof_broker_refusal_codes:
    status = "proof_broker_refused"
    proof["verification_attribution"] = "not_run_proof_broker_refused"
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
    or any(code.startswith("rch_verify_franken_stack_") for code in degraded)
):
    status = "source_state_refused"
elif (
    "rch_verify_topology_blocked" in degraded
    or "rch_verify_cargo_workspace_inheritance_blocked" in degraded
    or "rch_verify_cargo_path_dependency_version_blocked" in degraded
    or "rch_verify_cargo_config_provenance_blocked" in degraded
    or "rch_verify_client_daemon_version_skew" in degraded
    or "rch_verify_local_fallback_refused" in degraded
    or "rch_verify_all_workers_preflight_failed" in degraded
    or "rch_verify_worker_health_threshold_blocked" in degraded
    or "rch_verify_remote_transport_timeout" in degraded
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
persist_proof_broker_ledger(proof, status, command_hash)

summary_lines = [
    f"RCH verifier `{command_text}` => `{status}`.",
    f"- command_kind: `{proof.get('command_kind')}`",
    f"- verification_attribution: `{proof.get('verification_attribution')}`",
    f"- git_head: `{proof.get('git_head') or 'unknown'}`",
    f"- git_tree: `{proof.get('git_tree') or 'unknown'}`",
    f"- dirty_status_hash: `{proof.get('dirty_status_hash') or 'unknown'}`",
    f"- source_materialization: `{proof.get('source_materialization') or 'unknown'}`",
    f"- remote_source_materialized: `{str(bool(proof.get('remote_source_materialized'))).lower()}`",
    f"- remote_env: `{', '.join(proof.get('remote_env') or []) or 'none'}`",
    f"- remote_required: `{str(proof.get('remote_required')).lower()}`",
    f"- would_offload: `{str(proof.get('would_offload')).lower()}`",
    f"- worker_id: `{proof.get('worker_id') or 'unknown'}`",
    f"- exit_code: `{exit_code if exit_code is not None else 'not_run'}`",
    f"- elapsed_ms: `{proof.get('elapsed_ms')}`",
    f"- command_hash: `{command_hash}`",
]
cargo_config_provenance = proof.get("cargo_config_provenance") or {}
if cargo_config_provenance.get("status") not in (None, "not_computed"):
    summary_lines.append(
        f"- cargo_config_provenance: `{cargo_config_provenance.get('status')}`"
        f" source_attested=`{str(bool(cargo_config_provenance.get('source_attested'))).lower()}`"
        f" command_locked=`{str(bool(cargo_config_provenance.get('command_locked'))).lower()}`"
        f" blocking_sources=`{len(cargo_config_provenance.get('blocking_sources') or [])}`"
        f" provenance_hash=`{cargo_config_provenance.get('provenance_hash') or 'unknown'}`"
    )
franken_stack = proof.get("franken_stack") or {}
if franken_stack.get("status") not in (None, "not_computed", "not_applicable"):
    pinned_bundle_cache = franken_stack.get("bundle_cache") or {}
    summary_lines.append(
        f"- franken_stack: `{franken_stack.get('status')}`"
        f" mode=`{franken_stack.get('mode') or 'unknown'}`"
        f" remote_source_verified=`{str(bool(franken_stack.get('remote_source_verified'))).lower()}`"
        f" repositories=`{len(franken_stack.get('repositories') or [])}`"
        f" manifest_hash=`{franken_stack.get('manifest_hash') or 'unknown'}`"
        f" bundle_cache=`{pinned_bundle_cache.get('status') or 'none'}`"
        f" bundle_content_hash=`{pinned_bundle_cache.get('content_hash') or 'none'}`"
    )
if build_admission.get("status") not in (None, "not_run"):
    admitted = build_admission.get("admitted")
    if isinstance(admitted, bool):
        admitted = str(admitted).lower()
    elif admitted is None:
        admitted = "unknown"
    summary_lines.append(
        f"- build_admission: `{build_admission.get('status')}`"
        f" admitted=`{admitted}`"
    )
if proof_broker:
    summary_lines.append(
        f"- proof_broker: `{proof_broker.get('verdict') or proof_broker.get('status') or 'unknown'}`"
        f" remote_cargo_launched=`{str(bool(proof_broker.get('remoteCargoLaunched'))).lower()}`"
        f" next_action=`{proof_broker.get('nextAction') or 'unknown'}`"
    )
runtime = proof.get("rch_runtime") or {}
if runtime.get("status") not in (None, "not_checked"):
    summary_lines.append(
        f"- rch_runtime: `{runtime.get('status')}`"
        f" client=`{runtime.get('client_version') or 'unknown'}`"
        f" daemon=`{runtime.get('daemon_version') or 'unknown'}`"
    )
remote_setup_kill = proof.get("remote_setup_kill") or {}
if isinstance(remote_setup_kill, dict) and remote_setup_kill.get("signature"):
    summary_lines.append(
        f"- remote_setup_kill: `{remote_setup_kill.get('signature')}`"
        f" stage=`{remote_setup_kill.get('stage') or 'unknown'}`"
        f" elapsed_s=`{remote_setup_kill.get('elapsed_seconds')}`"
        f" stdout_bytes=`{remote_setup_kill.get('stdout_bytes')}`"
        f" stderr_bytes=`{remote_setup_kill.get('stderr_bytes')}`"
        f" next_action=`{remote_setup_kill.get('next_action') or 'unknown'}`"
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
    admission_blocker = selector_probe.get("admission_blocker") or {}
    if isinstance(admission_blocker, dict) and admission_blocker.get("kind"):
        detail_parts = [
            f"retry_guidance=`{admission_blocker.get('retry_guidance') or 'unknown'}`",
        ]
        for field in (
            "active_project_exclusion_count",
            "active_build_id",
            "worker_id",
            "worker_posture",
            "heartbeat_age_secs",
            "progress_age_secs",
            "next_action",
        ):
            value = admission_blocker.get(field)
            if value is not None:
                detail_parts.append(f"{field}=`{value}`")
        summary_lines.append(
            f"- selector_blocker: `{admission_blocker.get('kind')}` "
            + " ".join(detail_parts)
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
if proof.get("dependency_manifest_hash"):
    summary_lines.append(
        f"- dependency_manifest_hash: `{proof.get('dependency_manifest_hash')}`"
    )
if proof.get("source_bundle_hash"):
    summary_lines.append(f"- source_bundle_hash: `{proof.get('source_bundle_hash')}`")
known_blocker = proof.get("known_blocker") or {}
if isinstance(known_blocker, dict) and known_blocker.get("blocker_fingerprint"):
    summary_lines.append(f"- known_blocker: `{known_blocker.get('blocker_fingerprint')}`")
    summary_lines.append(f"- remediation_bead: `{known_blocker.get('remediation_bead') or 'unknown'}`")
    summary_lines.append(f"- retry_after: `{known_blocker.get('retry_after') or 'unknown'}`")
    summary_lines.append(f"- known_blocker_override_used: `{str(bool(known_blocker.get('override_used'))).lower()}`")
    active_project = known_blocker.get("active_project_exclusion") or {}
    if (
        known_blocker.get("blocker_kind") == "active_project_exclusion"
        and isinstance(active_project, dict)
    ):
        detail_parts = []
        for field in (
            "active_project_exclusion_count",
            "active_build_id",
            "worker_id",
            "worker_posture",
            "progress_age_secs",
            "next_action",
        ):
            value = active_project.get(field)
            if value is not None:
                detail_parts.append(f"{field}=`{value}`")
        summary_lines.append(
            "- known_blocker_selector: `active_project_exclusion`"
            + (" " + " ".join(detail_parts) if detail_parts else "")
        )
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
            "cargo_config_provenance": proof.get("cargo_config_provenance"),
            "franken_stack": proof.get("franken_stack"),
            "known_blocker": proof.get("known_blocker"),
            "proof_broker": proof.get("proof_broker"),
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
            fake_invocation_count = sum(
                1
                for line in fake_path.read_text(encoding="utf-8").splitlines()
                if "exec --" in line
            )

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
            "source_materialization": proof.get("source_materialization"),
            "remote_source_materialized": proof.get("remote_source_materialized"),
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
            "dependency_manifest_hash": proof.get("dependency_manifest_hash"),
            "source_bundle_hash": proof.get("source_bundle_hash"),
            "known_blocker": proof.get("known_blocker"),
            "proof_broker": proof.get("proof_broker"),
            "cargo_config_provenance_status": cargo_config_provenance.get("status"),
            "cargo_config_provenance_hash": cargo_config_provenance.get("provenance_hash"),
            "cargo_config_blocking_source_count": len(
                cargo_config_provenance.get("blocking_sources") or []
            ),
            "franken_stack_status": franken_stack.get("status"),
            "franken_stack_manifest_hash": franken_stack.get("manifest_hash"),
            "franken_stack_remote_source_verified": franken_stack.get(
                "remote_source_verified"
            ),
            "pinned_bundle_cache_status": (
                (franken_stack.get("bundle_cache") or {}).get("status")
            ),
            "pinned_bundle_content_hash": (
                (franken_stack.get("bundle_cache") or {}).get("content_hash")
            ),
            "stdout_artifact_path": artifact_path("stdout"),
            "stderr_artifact_path": artifact_path("stderr"),
            "schema_validation_status": "not_run",
            "deterministic_rerun_hash": (
                proof.get("source_bundle_hash")
                or proof.get("source_manifest_hash")
                or proof.get("dirty_status_hash")
            ),
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
positive_integer_or_die "RCH_VERIFY_DEFAULT_BUILD_TIMEOUT_SEC" "$RCH_VERIFY_DEFAULT_BUILD_TIMEOUT_SEC"
positive_integer_or_die "RCH_VERIFY_DEFAULT_TEST_TIMEOUT_SEC" "$RCH_VERIFY_DEFAULT_TEST_TIMEOUT_SEC"

if [ "$WORKER_ROOT_CANARY" -eq 1 ]; then
    emit_worker_root_canary_json
    exit 0
fi

COMMAND_KIND="$(classify_command)"
apply_default_remote_timeouts
positive_integer_if_set_or_die "RCH_BUILD_TIMEOUT_SEC" "${RCH_BUILD_TIMEOUT_SEC:-}"
positive_integer_if_set_or_die "RCH_TEST_TIMEOUT_SEC" "${RCH_TEST_TIMEOUT_SEC:-}"
WOULD_OFFLOAD=false
WORKER_ID_JSON=null
REMOTE_PROJECT_ROOT="/data/projects/eidetic_engine_cli"
REMOTE_TARGET_DIR="/tmp/ee-rch-verify-target"
REMOTE_PROJECT_ROOT_JSON="$(json_quote "$REMOTE_PROJECT_ROOT")"
REMOTE_TARGET_DIR_JSON="$(json_quote "$REMOTE_TARGET_DIR")"
REQUESTED_WORKERS_CSV="${RCH_WORKERS:-${RCH_WORKER:-}}"
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

if [ "$PINNED_FRANKEN_STACK" -eq 1 ]; then
    case "$COMMAND_KIND" in
        cargo_build|cargo_check|cargo_test|cargo_bench|cargo_clippy)
            ;;
        *)
            RCH_INVOCATION=()
            emit_json false null 0 "" \
                "--pinned-franken-stack requires Cargo build, check, test, bench, or clippy" \
                "rch_verify_franken_stack_cargo_required"
            exit 2
            ;;
    esac
    if ! command_uses_locked; then
        FRANKEN_STACK_JSON='{"schema":"ee.rch.franken_stack.v1","status":"blocked","mode":"pinned","applicable":true,"command_locked":false,"remote_source_verified":false,"repositories":[],"blocking_codes":["rch_verify_franken_stack_locked_required"],"degraded_codes":[],"manifest_hash":null,"repair":"Use --pinned-franken-stack only with a Cargo verifier command containing --locked."}'
        RCH_INVOCATION=()
        emit_json true 1 0 \
            "pinned Franken-stack preflight requires Cargo --locked before source materialization" \
            "" \
            "rch_verify_franken_stack_locked_required"
        exit 1
    fi
fi

if [ "$COMMITTED_TREE" -eq 1 ]; then
    SOURCE_STATE_JSON="$(compute_committed_tree_state_json)"
else
    SOURCE_STATE_JSON="$(compute_source_state_json)"
fi
INITIAL_SOURCE_STATE_DEGRADED_CODES="$(
    SOURCE_STATE_JSON="$SOURCE_STATE_JSON" python3 - <<'PY'
import json
import os
state = json.loads(os.environ["SOURCE_STATE_JSON"])
for code in state.get("source_state_degraded_codes") or []:
    print(code)
PY
)"
if [ "$COMMITTED_TREE" -eq 1 ]; then
    if [ -n "$INITIAL_SOURCE_STATE_DEGRADED_CODES" ]; then
        RCH_INVOCATION=()
        mapfile -t source_degraded_array <<<"$INITIAL_SOURCE_STATE_DEGRADED_CODES"
        emit_json true 1 0 "committed-tree preflight computed source manifest but cannot safely materialize it for RCH" "" "${source_degraded_array[@]}"
        exit 1
    fi
    materialize_committed_tree
fi

FRANKEN_STACK_JSON="$(compute_franken_stack_json)"
FRANKEN_STACK_BLOCKING_CODES="$(franken_stack_blocking_codes)"
if [ "$PINNED_FRANKEN_STACK" -eq 1 ] && [ -z "$FRANKEN_STACK_BLOCKING_CODES" ]; then
    if ! materialize_pinned_franken_stack; then
        FRANKEN_STACK_BLOCKING_CODES="$(franken_stack_blocking_codes)"
    else
        FRANKEN_STACK_BLOCKING_CODES="$(franken_stack_blocking_codes)"
    fi
fi
SOURCE_STATE_JSON="$(merge_franken_stack_source_state_json)"
SOURCE_STATE_DEGRADED_CODES="$(
    SOURCE_STATE_JSON="$SOURCE_STATE_JSON" python3 - <<'PY'
import json
import os
state = json.loads(os.environ["SOURCE_STATE_JSON"])
for code in state.get("source_state_degraded_codes") or []:
    print(code)
PY
)"
if [ -n "$FRANKEN_STACK_BLOCKING_CODES" ]; then
    RCH_INVOCATION=()
    mapfile -t franken_stack_blocking_array <<<"$FRANKEN_STACK_BLOCKING_CODES"
    emit_json true 1 0 \
        "Franken-stack source preflight refused before RCH dispatch" \
        "" \
        "${franken_stack_blocking_array[@]}"
    exit 1
fi
if [ "$REQUIRE_CLEAN_TREE" -eq 1 ] && [ -n "$SOURCE_STATE_DEGRADED_CODES" ]; then
    RCH_INVOCATION=()
    mapfile -t source_degraded_array <<<"$SOURCE_STATE_DEGRADED_CODES"
    emit_json true 1 0 "strict clean-tree preflight refused dirty or remotely unverified source" "" "${source_degraded_array[@]}"
    exit 1
fi

if [ "$COMMAND_KIND" = "raw" ] || [ "$COMMAND_KIND" = "cargo_fmt_check" ]; then
    WOULD_OFFLOAD=false
else
    WOULD_OFFLOAD=true
fi
RCH_INVOCATION=(
    "$RCH_BIN" "exec" "--"
    "${ENV_OVERRIDES[@]}"
    "${COMMAND[@]}"
)

CARGO_CONFIG_PROVENANCE_JSON="$(compute_cargo_config_provenance_json)"
if [ "$(json_text_field "$CARGO_CONFIG_PROVENANCE_JSON" status)" = "blocked" ]; then
    RCH_INVOCATION=()
    emit_json true 1 0 \
        "Cargo config provenance preflight refused source-attested --locked verification before RCH" \
        "" \
        "rch_verify_cargo_config_provenance_blocked"
    exit 1
fi

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

CONFIGURED_WORKERS_CSV="$(configured_workers)"
DAEMON_WORKERS_CSV="$(daemon_workers)"
REQUESTED_WORKERS_CSV="${RCH_WORKERS:-${RCH_WORKER:-}}"

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

proof_broker_degraded=()
if [ -n "$PROOF_BROKER_LEDGER" ]; then
    proof_broker_status=0
    run_proof_broker_admission || proof_broker_status=$?
    proof_broker_verdict="$(proof_broker_json_field verdict)"
    proof_broker_code="$(proof_broker_degraded_code "$proof_broker_verdict")"
    if [ "$proof_broker_status" -ne 0 ] && [ -z "$proof_broker_code" ]; then
        proof_broker_code="rch_verify_proof_broker_unavailable"
    fi
    case "$proof_broker_verdict" in
        dispatch_allowed)
            ;;
        reuse_existing)
            RCH_INVOCATION=()
            PROOF_BROKER_JSON="$(proof_broker_mark_json false "$PROOF_BROKER_BYPASS_REASON")"
            emit_json true 0 0 "proof broker admission reused existing proof; remote Cargo not launched" "" \
                "${build_admission_degraded[@]}" \
                "rch_verify_proof_broker_reuse_existing"
            exit 0
            ;;
        *)
            if [ -n "$PROOF_BROKER_BYPASS_REASON" ]; then
                proof_broker_degraded+=("rch_verify_proof_broker_bypassed")
                if [ -n "$proof_broker_code" ]; then
                    proof_broker_degraded+=("$proof_broker_code")
                fi
                PROOF_BROKER_JSON="$(proof_broker_mark_json false "$PROOF_BROKER_BYPASS_REASON")"
            else
                RCH_INVOCATION=()
                if [ -z "$proof_broker_code" ]; then
                    proof_broker_code="rch_verify_proof_broker_unavailable"
                fi
                PROOF_BROKER_JSON="$(proof_broker_mark_json false "")"
                emit_json true 1 0 "proof broker admission refused RCH dispatch; remote Cargo not launched" "" \
                    "${build_admission_degraded[@]}" \
                    "$proof_broker_code"
                exit 1
            fi
            ;;
    esac
fi

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

if [ -n "$PROOF_BROKER_LEDGER" ]; then
    PROOF_BROKER_JSON="$(proof_broker_mark_json true "$PROOF_BROKER_BYPASS_REASON")"
fi

start_ms="$(now_ms)"
primary_has_artifacts=0
if [ -z "${RCH_VERIFY_FAKE_OUTPUT:-}" ]; then
    prepare_attempt_artifacts "primary"
    primary_has_artifacts=1
fi
combined_output=""
exit_code=127
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
        retry_output=""
        retry_exit_code=127
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
degraded=("${build_admission_degraded[@]}" "${proof_broker_degraded[@]}")
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
if printf '%s' "$combined_output" | is_no_workers_passed_health_output; then
    degraded+=("rch_verify_worker_health_threshold_blocked")
fi
if [ "$exit_code" -ne 0 ] && [ -z "$worker_id" ] &&
    printf '%s' "$combined_output" | is_active_project_exclusion_output; then
    degraded+=("rch_verify_capacity_or_timeout")
    RCH_QUEUE_SNAPSHOT_JSON="$(rch_queue_snapshot_json)"
fi
if printf '%s' "$combined_output" | is_client_daemon_unknown_variant_output; then
    degraded+=("rch_verify_client_daemon_version_skew")
fi
if printf '%s' "$combined_output" | is_remote_transport_timeout_output; then
    degraded+=("rch_verify_remote_transport_timeout")
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
