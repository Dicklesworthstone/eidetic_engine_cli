#!/usr/bin/env bash
# J3 — shared helpers for per-epic e2e scripts under scripts/e2e_overhaul/.
#
# Sources J1's e2e_logger.sh and exposes:
#   - EE_BINARY            path to the ee binary (default: target/release/ee)
#   - REPO_ROOT            absolute repo root
#   - CORPUS_SEED          path to J2's corpus_2026_05_10_seed.sh
#   - epic_setup           shared setup: tmp workspace + init + trap
#   - epic_teardown        called via trap; emits e2e_log_end and rms workspace
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
    if [ -z "${CARGO_TARGET_DIR:-}" ]; then
        export CARGO_TARGET_DIR="${DEFAULT_AGENT_BUILD_ROOT}/cargo-target"
    fi
    if [ -z "${TMPDIR:-}" ]; then
        export TMPDIR="${DEFAULT_AGENT_BUILD_ROOT}/tmp"
    fi
fi

if [ -n "${CARGO_TARGET_DIR:-}" ]; then
    EE_BINARY="${EE_BINARY:-${CARGO_TARGET_DIR%/}/release/ee}"
else
    EE_BINARY="${EE_BINARY:-$REPO_ROOT/target/release/ee}"
fi
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

# Usage: epic_setup <epic_name>
#   Creates a temp workspace, calls `ee init`, and arms a teardown trap.
#   The trap reports the asserts_pass/asserts_fail counters via J1 and rms
#   the workspace.
#
# Note: `set -e` is intentionally relaxed inside per-epic scripts because the
#   J1 assert helpers return non-zero on failure and we want every assertion
#   to run regardless of earlier failures. Critical setup steps (init) bail
#   out explicitly with `exit 3` instead of relying on errexit.
epic_setup() {
    EPIC_NAME="${1:?epic name required}"
    require_ee_binary

    EPIC_WORKSPACE="$(mktemp -d "/tmp/ee-e2e-${EPIC_NAME}.XXXXXX")"
    export EPIC_WORKSPACE

    e2e_log_start "$EPIC_NAME"
    e2e_log_note "epic_setup workspace=$EPIC_WORKSPACE binary=$EE_BINARY"

    # Initialize the workspace. Bail out loudly on failure: every other
    # assertion presupposes a usable workspace.
    if ! "$EE_BINARY" init --workspace "$EPIC_WORKSPACE" --json >/dev/null; then
        echo "j3: ee init failed for $EPIC_WORKSPACE" >&2
        e2e_log_note "epic_setup_init_failed workspace=$EPIC_WORKSPACE"
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
    e2e_log_end
    if [ -n "$EPIC_WORKSPACE" ] && [ -d "$EPIC_WORKSPACE" ]; then
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
