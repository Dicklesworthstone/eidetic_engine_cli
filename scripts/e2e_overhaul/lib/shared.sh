#!/usr/bin/env bash
# J3 — shared helpers for per-epic e2e scripts under scripts/e2e_overhaul/.
#
# Sources J1's e2e_logger.sh and exposes:
#   - EE_BINARY            path to the ee binary (default: Cargo target/release/ee)
#   - REPO_ROOT            absolute repo root
#   - CORPUS_SEED          path to J2's corpus_2026_05_10_seed.sh
#   - epic_setup           shared setup: bounded tmp workspace + bounded init + trap
#   - epic_teardown        called via trap; emits e2e_log_end and retains
#                          workspaces unless deletion is explicitly allowed
#   - require_jq           bail out early if jq is missing
#   - run_capture          runs `$EE …`, captures stdout+stderr, logs via J1,
#                          and propagates exit code so set -e fires on failure
#
# The intent: every epic script reads the same way. Boilerplate stays in here.

set -o pipefail

SHARED_SH_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SHARED_SH_DIR/../../.." && pwd)"
export REPO_ROOT

DEFAULT_AGENT_BUILD_ROOT="/Volumes/USBNVME16TB/temp_agent_space"

if [ -d "${DEFAULT_AGENT_BUILD_ROOT}" ]; then
    mkdir -p "${DEFAULT_AGENT_BUILD_ROOT}/cargo-target" "${DEFAULT_AGENT_BUILD_ROOT}/tmp" 2>/dev/null || true
    export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-${DEFAULT_AGENT_BUILD_ROOT}/cargo-target}"
    export TMPDIR="${EE_AGENT_TMPDIR:-${DEFAULT_AGENT_BUILD_ROOT}/tmp}"
fi

# shellcheck source=scripts/lib/ee_binary_resolution.sh
source "$REPO_ROOT/scripts/lib/ee_binary_resolution.sh"
EE_BINARY="$(ee_resolve_binary release)"
export EE_BINARY

CORPUS_SEED="$REPO_ROOT/tests/fixtures/corpus/corpus_2026_05_10_seed.sh"
export CORPUS_SEED

# Source J1 logger. This makes e2e_log_* helpers available unconditionally:
# when EE_TEST_LOG_PATH is unset they no-op silently (per J1's design).
# shellcheck source=/dev/null
source "$REPO_ROOT/scripts/lib/e2e_logger.sh"

# ---------------------------------------------------------------------------
# Preflight
# ---------------------------------------------------------------------------

require_jq() {
    if ! command -v jq >/dev/null 2>&1; then
        echo "j3: jq is required but was not found in PATH" >&2
        exit 2
    fi
}

require_ee_binary() {
    if [ ! -x "$EE_BINARY" ]; then
        echo "j3: ee binary not executable at $EE_BINARY" >&2
        echo "    set EE_BINARY or run: cargo build --release" >&2
        exit 2
    fi
}

# ---------------------------------------------------------------------------
# Workspace lifecycle
# ---------------------------------------------------------------------------

# Globals populated by epic_setup. Read-only after the call.
EPIC_WORKSPACE=""
EPIC_NAME=""
EPIC_SETUP_BASHPID=""
EPIC_TMP_ROOT=""
EPIC_RETENTION_MANIFEST=""
EPIC_WORKSPACE_META=""
EPIC_INIT_STDOUT=""
EPIC_INIT_STDERR=""
EPIC_INIT_META=""
MESH_SCENARIO_NAME=""
MESH_SCENARIO_ROOT=""
MESH_NODE_COUNT=0

_epic_keep_workspace_enabled() {
    [ "${EE_E2E_KEEP_WORKSPACE:-0}" = "1" ] || ! _epic_workspace_delete_enabled
}

_epic_workspace_delete_enabled() {
    [ "${EE_E2E_ALLOW_WORKSPACE_DELETE:-0}" = "1" ]
}

_epic_keep_artifacts_enabled() {
    [ "${EE_E2E_KEEP_ARTIFACTS:-${EE_E2E_KEEP_WORKSPACE:-0}}" = "1" ]
}

_epic_workspace_owned_by_setup() {
    if [ -z "$EPIC_WORKSPACE" ] || [ -z "$EPIC_NAME" ] || [ -z "$EPIC_TMP_ROOT" ]; then
        return 1
    fi
    local expected_prefix
    expected_prefix="${EPIC_TMP_ROOT%/}/ee-e2e-${EPIC_NAME}."
    case "$EPIC_WORKSPACE" in
        "$expected_prefix"*) return 0 ;;
        *) return 1 ;;
    esac
}

_epic_write_retention_manifest() {
    local cleanup_policy="${1:?cleanup policy required}"
    local phase="${2:?phase required}"
    if [ -z "${EPIC_RETENTION_MANIFEST:-}" ]; then
        return 0
    fi
    python3 - "$EPIC_RETENTION_MANIFEST" "$EPIC_NAME" "$phase" \
        "$EPIC_WORKSPACE" "${EE_TEST_LOG_PATH:-}" "$EE_BINARY" \
        "${EE_E2E_KEEP_WORKSPACE:-0}" \
        "${EE_E2E_KEEP_ARTIFACTS:-${EE_E2E_KEEP_WORKSPACE:-0}}" \
        "$cleanup_policy" "${EPIC_SETUP_BASHPID:-}" "${BASHPID:-$$}" \
        "${EPIC_WORKSPACE_META:-}" "${EPIC_INIT_STDOUT:-}" "${EPIC_INIT_STDERR:-}" "${EPIC_INIT_META:-}" <<'PY'
import json
import os
import sys
from datetime import datetime, timezone

(
    path,
    epic_name,
    phase,
    workspace,
    test_log_path,
    ee_binary,
    keep_workspace,
    keep_artifacts,
    cleanup_policy,
    setup_pid,
    current_pid,
    workspace_meta,
    init_stdout,
    init_stderr,
    init_meta,
) = sys.argv[1:]

payload = {
    "schema": "ee.e2e.retention_manifest.v1",
    "generated_at": datetime.now(timezone.utc).isoformat(timespec="microseconds").replace("+00:00", "Z"),
    "epic_name": epic_name,
    "phase": phase,
    "workspace": workspace,
    "test_log_path": test_log_path or None,
    "ee_binary": ee_binary,
    "keep_workspace": keep_workspace == "1",
    "keep_artifacts": keep_artifacts == "1",
    "cleanup_policy": cleanup_policy,
    "retained": cleanup_policy.startswith("retained"),
    "setup_pid": setup_pid or None,
    "current_pid": current_pid or None,
    "workspace_meta_path": workspace_meta or None,
    "init_stdout_path": init_stdout or None,
    "init_stderr_path": init_stderr or None,
    "init_meta_path": init_meta or None,
    "artifact_paths": [
        value
        for value in [
            workspace,
            test_log_path or None,
            workspace_meta or None,
            init_stdout or None,
            init_stderr or None,
            init_meta or None,
        ]
        if value
    ],
}

os.makedirs(os.path.dirname(path) or ".", exist_ok=True)
with open(path, "w", encoding="utf-8") as handle:
    json.dump(payload, handle, indent=2, sort_keys=True)
    handle.write("\n")
PY
}

_epic_create_workspace_with_timeout() {
    if [ -z "${EPIC_NAME:-}" ] || [ -z "${EPIC_TMP_ROOT:-}" ]; then
        echo "j3: EPIC_NAME or EPIC_TMP_ROOT is unset before workspace creation" >&2
        return 2
    fi

    if [ -z "${EPIC_WORKSPACE_META:-}" ]; then
        local meta_root
        meta_root="${EE_E2E_SETUP_META_DIR:-/tmp}"
        mkdir -p "$meta_root"
        EPIC_WORKSPACE_META="$meta_root/ee-e2e-${EPIC_NAME}-workspace-create-${EPIC_SETUP_BASHPID:-$$}.json"
        export EPIC_WORKSPACE_META
    fi

    python3 - "$EPIC_NAME" "$EPIC_TMP_ROOT" "$EPIC_WORKSPACE_META" \
        "${EE_E2E_WORKSPACE_CREATE_TIMEOUT_SECONDS:-${EE_E2E_SETUP_TIMEOUT_SECONDS:-15}}" <<'PY'
import json
import os
import signal
import subprocess
import sys
import time
from datetime import datetime, timezone

(
    epic_name,
    tmp_root,
    meta_path,
    raw_timeout,
) = sys.argv[1:]

try:
    timeout_seconds = float(raw_timeout)
    if timeout_seconds <= 0:
        raise ValueError("timeout must be positive")
except (TypeError, ValueError):
    timeout_seconds = 15.0

meta_dir = os.path.dirname(meta_path) or "."
os.makedirs(meta_dir, exist_ok=True)
stdout_path = f"{meta_path}.stdout"
stderr_path = f"{meta_path}.stderr"
workspace_path = None
timed_out = False
kill_escalated = False
child_still_running = False
exit_code = 125
error = None
termination_error = None
started_at = datetime.now(timezone.utc).isoformat(timespec="microseconds").replace("+00:00", "Z")
started = time.monotonic()

child_code = r"""
import os
import sys
import tempfile
import time

epic_name, tmp_root = sys.argv[1:]
if os.environ.get("EE_E2E_WORKSPACE_CREATE_FAKE_HANG") == "1":
    time.sleep(86400)
os.makedirs(tmp_root, exist_ok=True)
path = tempfile.mkdtemp(prefix=f"ee-e2e-{epic_name}.", dir=tmp_root)
print(path)
"""

def send_process_group(pid, sig):
    try:
        os.killpg(pid, sig)
    except ProcessLookupError:
        return None
    except OSError as exc:
        return f"{type(exc).__name__}: {exc}"
    return None

try:
    with open(stdout_path, "wb") as stdout_handle, open(stderr_path, "wb") as stderr_handle:
        process = subprocess.Popen(
            [sys.executable, "-c", child_code, epic_name, tmp_root],
            stdout=stdout_handle,
            stderr=stderr_handle,
            start_new_session=True,
        )
        try:
            process.wait(timeout=timeout_seconds)
        except subprocess.TimeoutExpired:
            timed_out = True
            termination_error = send_process_group(process.pid, signal.SIGTERM)
            try:
                process.wait(timeout=3)
            except subprocess.TimeoutExpired:
                kill_escalated = True
                termination_error = send_process_group(process.pid, signal.SIGKILL) or termination_error
                try:
                    process.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    child_still_running = True
                    try:
                        process.kill()
                    except OSError as exc:
                        termination_error = f"{type(exc).__name__}: {exc}"
            exit_code = 124
        else:
            exit_code = process.returncode
except FileNotFoundError as exc:
    exit_code = 127
    error = f"{type(exc).__name__}: {exc}"
except PermissionError as exc:
    exit_code = 126
    error = f"{type(exc).__name__}: {exc}"
except OSError as exc:
    exit_code = 125
    error = f"{type(exc).__name__}: {exc}"

elapsed_ms = int((time.monotonic() - started) * 1000)
if isinstance(exit_code, int) and exit_code < 0:
    exit_code = 128 + abs(exit_code)

if exit_code == 0:
    try:
        with open(stdout_path, "r", encoding="utf-8") as handle:
            first_line = handle.readline().strip()
        if first_line:
            workspace_path = first_line
    except OSError as exc:
        exit_code = 125
        error = f"{type(exc).__name__}: {exc}"
    if not workspace_path:
        exit_code = 125
        error = error or "workspace creation produced no workspace path"

def path_size(path):
    try:
        return os.path.getsize(path)
    except OSError:
        return 0

meta = {
    "schema": "ee.e2e.workspace_create.v1",
    "generatedAt": datetime.now(timezone.utc).isoformat(timespec="microseconds").replace("+00:00", "Z"),
    "startedAt": started_at,
    "epicName": epic_name,
    "tmpRoot": tmp_root,
    "timeoutSeconds": timeout_seconds,
    "timedOut": timed_out,
    "killEscalated": kill_escalated,
    "childStillRunning": child_still_running,
    "exitCode": exit_code,
    "elapsedMs": elapsed_ms,
    "workspacePath": workspace_path,
    "stdoutPath": stdout_path,
    "stderrPath": stderr_path,
    "stdoutBytes": path_size(stdout_path),
    "stderrBytes": path_size(stderr_path),
    "fakeHang": os.environ.get("EE_E2E_WORKSPACE_CREATE_FAKE_HANG") == "1",
    "error": error,
    "terminationError": termination_error,
}

with open(meta_path, "w", encoding="utf-8") as handle:
    json.dump(meta, handle, indent=2, sort_keys=True)
    handle.write("\n")

if exit_code == 0 and workspace_path:
    print(workspace_path)
    raise SystemExit(0)
if exit_code == 0:
    raise SystemExit(125)
raise SystemExit(exit_code)
PY
}

_epic_init_workspace_with_timeout() {
    if [ -z "${EPIC_WORKSPACE:-}" ]; then
        echo "j3: EPIC_WORKSPACE is unset before ee init" >&2
        return 2
    fi

    EPIC_INIT_STDOUT="$EPIC_WORKSPACE/e2e_init_stdout.json"
    EPIC_INIT_STDERR="$EPIC_WORKSPACE/e2e_init_stderr.log"
    EPIC_INIT_META="$EPIC_WORKSPACE/e2e_init_meta.json"
    export EPIC_INIT_STDOUT
    export EPIC_INIT_STDERR
    export EPIC_INIT_META

    python3 - "$EE_BINARY" "$EPIC_WORKSPACE" "$EPIC_INIT_STDOUT" "$EPIC_INIT_STDERR" \
        "$EPIC_INIT_META" "${EE_E2E_INIT_TIMEOUT_SECONDS:-${EE_E2E_SETUP_TIMEOUT_SECONDS:-60}}" <<'PY'
import json
import os
import signal
import subprocess
import sys
import time
from datetime import datetime, timezone

(
    ee_binary,
    workspace,
    stdout_path,
    stderr_path,
    meta_path,
    raw_timeout,
) = sys.argv[1:]

try:
    timeout_seconds = float(raw_timeout)
    if timeout_seconds <= 0:
        raise ValueError("timeout must be positive")
except (TypeError, ValueError):
    timeout_seconds = 60.0

command = [ee_binary, "init", "--workspace", workspace, "--json"]
started_at = datetime.now(timezone.utc).isoformat(timespec="microseconds").replace("+00:00", "Z")
started = time.monotonic()
timed_out = False
kill_escalated = False
exit_code = 125
error = None
termination_error = None

def send_process_group(pid, sig):
    try:
        os.killpg(pid, sig)
    except ProcessLookupError:
        return None
    except OSError as exc:
        return f"{type(exc).__name__}: {exc}"
    return None

try:
    os.makedirs(os.path.dirname(stdout_path) or ".", exist_ok=True)
    with open(stdout_path, "wb") as stdout_handle, open(stderr_path, "wb") as stderr_handle:
        process = subprocess.Popen(
            command,
            stdout=stdout_handle,
            stderr=stderr_handle,
            start_new_session=True,
        )
        try:
            process.wait(timeout=timeout_seconds)
        except subprocess.TimeoutExpired:
            timed_out = True
            termination_error = send_process_group(process.pid, signal.SIGTERM)
            try:
                process.wait(timeout=3)
            except subprocess.TimeoutExpired:
                kill_escalated = True
                termination_error = send_process_group(process.pid, signal.SIGKILL) or termination_error
                try:
                    process.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    try:
                        process.kill()
                    except OSError as exc:
                        termination_error = f"{type(exc).__name__}: {exc}"
            exit_code = 124
        else:
            exit_code = process.returncode
except FileNotFoundError as exc:
    exit_code = 127
    error = f"{type(exc).__name__}: {exc}"
except PermissionError as exc:
    exit_code = 126
    error = f"{type(exc).__name__}: {exc}"
except OSError as exc:
    exit_code = 125
    error = f"{type(exc).__name__}: {exc}"

elapsed_ms = int((time.monotonic() - started) * 1000)

if isinstance(exit_code, int) and exit_code < 0:
    exit_code = 128 + abs(exit_code)

def path_size(path):
    try:
        return os.path.getsize(path)
    except OSError:
        return 0

meta = {
    "schema": "ee.e2e.setup_init.v1",
    "generatedAt": datetime.now(timezone.utc).isoformat(timespec="microseconds").replace("+00:00", "Z"),
    "startedAt": started_at,
    "command": command,
    "workspace": workspace,
    "eeBinary": ee_binary,
    "timeoutSeconds": timeout_seconds,
    "timedOut": timed_out,
    "killEscalated": kill_escalated,
    "exitCode": exit_code,
    "elapsedMs": elapsed_ms,
    "stdoutPath": stdout_path,
    "stderrPath": stderr_path,
    "stdoutBytes": path_size(stdout_path),
    "stderrBytes": path_size(stderr_path),
    "error": error,
    "terminationError": termination_error,
}

os.makedirs(os.path.dirname(meta_path) or ".", exist_ok=True)
with open(meta_path, "w", encoding="utf-8") as handle:
    json.dump(meta, handle, indent=2, sort_keys=True)
    handle.write("\n")

raise SystemExit(exit_code)
PY
}

# Usage: epic_setup <epic_name>
#   Creates a temp workspace, calls `ee init`, and arms a teardown trap.
#   The trap reports the asserts_pass/asserts_fail counters via J1 and retains
#   the workspace by default. Set EE_E2E_ALLOW_WORKSPACE_DELETE=1, with
#   EE_E2E_KEEP_WORKSPACE unset/0, to remove only the temp workspace created here.
#
# Note: `set -e` is intentionally relaxed inside per-epic scripts because the
#   J1 assert helpers return non-zero on failure and we want every assertion
#   to run regardless of earlier failures. Critical setup steps (init) bail
#   out explicitly with `exit 3` instead of relying on errexit.
epic_setup() {
    EPIC_NAME="${1:?epic name required}"
    EPIC_SETUP_BASHPID="${BASHPID:-$$}"

    e2e_log_start "$EPIC_NAME"
    e2e_log_note "epic_setup_begin binary=$EE_BINARY"
    require_ee_binary

    EPIC_TMP_ROOT="${EE_E2E_TMPDIR:-${TMPDIR:-/tmp}}"
    e2e_log_note "epic_setup_tmp_root tmp_root=$EPIC_TMP_ROOT"
    local workspace_meta_root
    workspace_meta_root="${EE_E2E_SETUP_META_DIR:-/tmp}"
    mkdir -p "$workspace_meta_root"
    EPIC_WORKSPACE_META="$workspace_meta_root/ee-e2e-${EPIC_NAME}-workspace-create-${EPIC_SETUP_BASHPID:-$$}.json"
    EPIC_RETENTION_MANIFEST="${EE_E2E_RETENTION_MANIFEST:-$workspace_meta_root/ee-e2e-${EPIC_NAME}-retention-${EPIC_SETUP_BASHPID:-$$}.json}"
    export EPIC_WORKSPACE_META
    export EPIC_RETENTION_MANIFEST
    e2e_log_note "epic_workspace_meta path=$EPIC_WORKSPACE_META retention_manifest=$EPIC_RETENTION_MANIFEST"
    if EPIC_WORKSPACE="$(_epic_create_workspace_with_timeout)"; then
        export EPIC_WORKSPACE
    else
        workspace_create_status=$?
        echo "j3: e2e workspace creation failed for $EPIC_NAME (status=$workspace_create_status)" >&2
        echo "j3: workspace meta: $EPIC_WORKSPACE_META" >&2
        e2e_log_note "epic_setup_workspace_create_failed epic=$EPIC_NAME status=$workspace_create_status meta=$EPIC_WORKSPACE_META tmp_root=$EPIC_TMP_ROOT"
        _epic_write_retention_manifest "retained_after_workspace_create_failure" "workspace_create_failed"
        exit 3
    fi
    EPIC_RETENTION_MANIFEST="${EE_E2E_RETENTION_MANIFEST:-$EPIC_WORKSPACE/e2e_retention_manifest.json}"
    export EPIC_RETENTION_MANIFEST

    e2e_log_note "epic_setup workspace=$EPIC_WORKSPACE binary=$EE_BINARY"
    e2e_log_note "epic_retention_manifest path=$EPIC_RETENTION_MANIFEST"

    # Initialize the workspace with a bounded child process. Bail out loudly on
    # failure: every other assertion presupposes a usable workspace.
    if _epic_init_workspace_with_timeout; then
        e2e_log_note "epic_setup_init_ok workspace=$EPIC_WORKSPACE meta=$EPIC_INIT_META stdout=$EPIC_INIT_STDOUT stderr=$EPIC_INIT_STDERR"
        _epic_write_retention_manifest "pending_teardown" "init_ok"
    else
        init_status=$?
        echo "j3: ee init failed for $EPIC_WORKSPACE (status=$init_status)" >&2
        echo "j3: init stdout: $EPIC_INIT_STDOUT" >&2
        echo "j3: init stderr: $EPIC_INIT_STDERR" >&2
        echo "j3: init meta: $EPIC_INIT_META" >&2
        e2e_log_note "epic_setup_init_failed workspace=$EPIC_WORKSPACE status=$init_status meta=$EPIC_INIT_META stdout=$EPIC_INIT_STDOUT stderr=$EPIC_INIT_STDERR"
        _epic_write_retention_manifest "retained_after_init_failure" "init_failed"
        exit 3
    fi

    # Arm teardown. Use `_epic_teardown` (not `epic_teardown`) to avoid stomping
    # on per-script trap handlers; scripts that need a custom trap can call
    # `_epic_teardown` themselves.
    trap _epic_teardown EXIT

    # Relax errexit inside assertion bodies so a single assert_fail doesn't
    # abort the rest of the script. Drivers retain `set -u` and `pipefail`.
    set +e
}

_epic_teardown() {
    local code=$?
    if [ -n "$EPIC_SETUP_BASHPID" ] && [ "${BASHPID:-$$}" != "$EPIC_SETUP_BASHPID" ]; then
        return "$code"
    fi
    e2e_log_end
    if [ -n "$EPIC_WORKSPACE" ] && [ -d "$EPIC_WORKSPACE" ]; then
        if _epic_keep_workspace_enabled; then
            local retention_policy="retained_by_default_no_delete_policy"
            if [ "${EE_E2E_KEEP_WORKSPACE:-0}" = "1" ]; then
                retention_policy="retained_by_keep_workspace"
            fi
            _epic_write_retention_manifest "$retention_policy" "teardown"
            e2e_log_note "epic_teardown_keep_workspace workspace=$EPIC_WORKSPACE policy=$retention_policy"
            echo "j3: retained e2e workspace: $EPIC_WORKSPACE" >&2
            echo "j3: retention manifest: $EPIC_RETENTION_MANIFEST" >&2
            return "$code"
        fi
        if ! _epic_workspace_owned_by_setup; then
            _epic_write_retention_manifest "retained_cleanup_refused_unowned_path" "teardown"
            e2e_log_note "epic_teardown_refuse_cleanup workspace=$EPIC_WORKSPACE tmp_root=$EPIC_TMP_ROOT"
            return "$code"
        fi
        _epic_write_retention_manifest "removed_by_default_cleanup" "teardown"
        rm -rf "$EPIC_WORKSPACE"
    fi
    return "$code"
}

# Seed the 2026-05-10 reference corpus into $EPIC_WORKSPACE. Returns 0 even
# when individual memories were rejected by pre-overhaul detectors, because
# many per-epic scripts intentionally exercise the partial-rejection state.
seed_corpus() {
    if [ ! -x "$CORPUS_SEED" ]; then
        e2e_log_note "seed_corpus_unavailable path=$CORPUS_SEED"
        return 1
    fi
    CORPUS_TOLERATE_REJECT=1 "$CORPUS_SEED" "$EPIC_WORKSPACE" >/dev/null 2>&1 || true
}

# ---------------------------------------------------------------------------
# Mesh scenario helpers
# ---------------------------------------------------------------------------

mesh_phase_log() {
    local phase="${1:?phase required}"
    local node="${2:?node or scenario required}"
    local message="${3:?message required}"
    _e2e_emit_event "note" \
        "phase" "$phase" \
        "meshScenario" "${MESH_SCENARIO_NAME:-$EPIC_NAME}" \
        "meshNode" "$node" \
        "message" "$message"
}

mesh_scenario_setup() {
    local scenario="${1:?scenario required}"
    local node_count="${2:?node count required}"
    MESH_SCENARIO_NAME="$scenario"
    MESH_NODE_COUNT="$node_count"
    MESH_SCENARIO_ROOT="$EPIC_WORKSPACE/mesh/$scenario"
    export MESH_SCENARIO_NAME
    export MESH_NODE_COUNT
    export MESH_SCENARIO_ROOT

    mkdir -p "$MESH_SCENARIO_ROOT"
    mesh_phase_log "setup" "$scenario" "scenario_root=$MESH_SCENARIO_ROOT node_count=$MESH_NODE_COUNT"

    local index node
    index=1
    while [ "$index" -le "$node_count" ]; do
        node="$(printf 'node%02d' "$index")"
        mkdir -p \
            "$MESH_SCENARIO_ROOT/$node/workspace" \
            "$MESH_SCENARIO_ROOT/$node/config" \
            "$MESH_SCENARIO_ROOT/$node/logs" \
            "$MESH_SCENARIO_ROOT/$node/goldens"
        mesh_phase_log "setup" "$node" "node_workspace=$MESH_SCENARIO_ROOT/$node/workspace"
        index=$((index + 1))
    done
}

mesh_node_workspace() {
    local node="${1:?node id required}"
    local path="$MESH_SCENARIO_ROOT/$node/workspace"
    mkdir -p "$path"
    printf '%s\n' "$path"
}

# ---------------------------------------------------------------------------
# Command helpers
# ---------------------------------------------------------------------------

# Run an `ee …` invocation against $EPIC_WORKSPACE and print its stdout. The
# command is automatically pointed at --workspace "$EPIC_WORKSPACE" unless the
# caller already passed --workspace or a positional that clearly overrides it.
# Errors propagate via set -e from the caller.
ee_workspace() {
    e2e_log_command "$EE_BINARY" "$@" --workspace "$EPIC_WORKSPACE"
}

# Run `ee …` with no implicit workspace. Use for global commands like
# `ee --help`, `ee capabilities`, etc.
ee_global() {
    e2e_log_command "$EE_BINARY" "$@"
}

# ---------------------------------------------------------------------------
# Assertion helpers
# ---------------------------------------------------------------------------

# Assert that a JSON path (jq filter) returns a value matching `want`. Use the
# raw output, no quoting. Counts toward the J1 pass/fail tally.
# Usage: assert_jq <json> <jq-filter> <want> <label>
assert_jq() {
    local json="${1:-}"
    local filter="${2:?filter required}"
    local want="${3:-}"
    local label="${4:?label required}"
    local got
    got="$(printf '%s' "$json" | jq -r "$filter" 2>/dev/null || true)"
    e2e_log_assert_eq "$got" "$want" "$label"
}

# Assert that a JSON path returns a non-empty value.
# Usage: assert_jq_nonempty <json> <jq-filter> <label>
assert_jq_nonempty() {
    local json="${1:-}"
    local filter="${2:?filter required}"
    local label="${3:?label required}"
    local got
    got="$(printf '%s' "$json" | jq -r "$filter" 2>/dev/null || true)"
    if [ -z "$got" ] || [ "$got" = "null" ]; then
        EE_TEST_LOG_ASSERTS_FAIL=$((EE_TEST_LOG_ASSERTS_FAIL + 1))
        _e2e_emit_event "assert_fail" "label" "$label" \
            "expected" "non-empty" "actual" "${got:-<empty>}"
        return 1
    fi
    EE_TEST_LOG_ASSERTS_PASS=$((EE_TEST_LOG_ASSERTS_PASS + 1))
    _e2e_emit_event "assert_ok" "label" "$label"
}

# Note an assertion that is *expected to fail* under the current binary because
# the corresponding bead is not yet shipped. The script still completes; the
# failure is recorded structurally so callers can detect "pre-implementation"
# vs "fully fixed" without flipping exit codes.
todo_assert() {
    local label="${1:?label required}"
    local bead="${2:?bead id required}"
    local description="${3:?description required}"
    e2e_log_note "todo_assert bead=$bead label=$label description=$description"
}
