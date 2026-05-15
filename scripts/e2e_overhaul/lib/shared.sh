#!/usr/bin/env bash
# J3 — shared helpers for per-epic e2e scripts under scripts/e2e_overhaul/.
#
# Sources J1's e2e_logger.sh and exposes:
#   - EE_BINARY            path to the ee binary (default: Cargo target/release/ee)
#   - REPO_ROOT            absolute repo root
#   - CORPUS_SEED          path to J2's corpus_2026_05_10_seed.sh
#   - epic_setup           shared setup: tmp workspace + init + trap
#   - epic_teardown        called via trap; emits e2e_log_end and handles
#                          workspace cleanup or retention
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

_epic_keep_workspace_enabled() {
    [ "${EE_E2E_KEEP_WORKSPACE:-0}" = "1" ]
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
        "$cleanup_policy" "${EPIC_SETUP_BASHPID:-}" "${BASHPID:-$$}" <<'PY'
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
    "artifact_paths": [
        value for value in [workspace, test_log_path or None] if value
    ],
}

os.makedirs(os.path.dirname(path) or ".", exist_ok=True)
with open(path, "w", encoding="utf-8") as handle:
    json.dump(payload, handle, indent=2, sort_keys=True)
    handle.write("\n")
PY
}

# Usage: epic_setup <epic_name>
#   Creates a temp workspace, calls `ee init`, and arms a teardown trap.
#   The trap reports the asserts_pass/asserts_fail counters via J1 and either
#   retains the workspace or removes only the temp workspace created here.
#
# Note: `set -e` is intentionally relaxed inside per-epic scripts because the
#   J1 assert helpers return non-zero on failure and we want every assertion
#   to run regardless of earlier failures. Critical setup steps (init) bail
#   out explicitly with `exit 3` instead of relying on errexit.
epic_setup() {
    EPIC_NAME="${1:?epic name required}"
    EPIC_SETUP_BASHPID="${BASHPID:-$$}"
    require_ee_binary

    EPIC_TMP_ROOT="${EE_E2E_TMPDIR:-${TMPDIR:-/tmp}}"
    mkdir -p "$EPIC_TMP_ROOT"
    EPIC_WORKSPACE="$(mktemp -d "${EPIC_TMP_ROOT%/}/ee-e2e-${EPIC_NAME}.XXXXXX")"
    EPIC_RETENTION_MANIFEST="${EE_E2E_RETENTION_MANIFEST:-$EPIC_WORKSPACE/e2e_retention_manifest.json}"
    export EPIC_WORKSPACE
    export EPIC_RETENTION_MANIFEST

    e2e_log_start "$EPIC_NAME"
    e2e_log_note "epic_setup workspace=$EPIC_WORKSPACE binary=$EE_BINARY"
    e2e_log_note "epic_retention_manifest path=$EPIC_RETENTION_MANIFEST"
    _epic_write_retention_manifest "pending_teardown" "setup"

    # Initialize the workspace. Bail out loudly on failure: every other
    # assertion presupposes a usable workspace.
    if ! "$EE_BINARY" init --workspace "$EPIC_WORKSPACE" --json >/dev/null; then
        echo "j3: ee init failed for $EPIC_WORKSPACE" >&2
        e2e_log_note "epic_setup_init_failed workspace=$EPIC_WORKSPACE"
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
            _epic_write_retention_manifest "retained_by_keep_workspace" "teardown"
            e2e_log_note "epic_teardown_keep_workspace workspace=$EPIC_WORKSPACE"
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
