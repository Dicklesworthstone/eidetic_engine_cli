#!/usr/bin/env bash
# Stable feature-E2E entrypoint for the dueling-wizards initiative.
#
# The actual harness lives in scripts/lib/e2e_harness.sh. Keep this file as the
# compatibility surface named by the Beads acceptance text so feature scripts can
# source one stable path while the harness implementation remains centralized.

set -o pipefail

E2E_LIB_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# shellcheck source=scripts/lib/e2e_harness.sh
# shellcheck disable=SC1091
source "$E2E_LIB_DIR/lib/e2e_harness.sh"

# log_event <kind> [key value]...
# Emits an ee.test_event.v1 JSON event through the shared logger and mirrors a
# compact human-readable line to stderr for interactive runs.
log_event() {
    local kind="${1:?log_event: kind required}"
    shift
    if [ $(( $# % 2 )) -ne 0 ]; then
        _harness_fail "log_event $kind: expected key/value pairs"
        return 1
    fi
    _e2e_emit_event "$kind" "$@"
    printf '[harness] event %s' "$kind" >&2
    while [ $# -gt 0 ]; do
        printf ' %s=%s' "$1" "$2" >&2
        shift 2
    done
    printf '\n' >&2
}

# assert_json <json> <jq-filter> <expected> <label>
# Extracts a scalar via jq -r and compares it with assert_eq.
assert_json() {
    local json="${1:?assert_json: json required}"
    local filter="${2:?assert_json: jq filter required}"
    local expected="${3:-}"
    local label="${4:-assert_json}"
    local actual
    if ! actual="$(printf '%s' "$json" | jq -r "$filter" 2>/dev/null)"; then
        e2e_log_assert_eq "jq_error" "$expected" "$label" || true
        _harness_fail "$label: jq filter failed [$filter]"
        return 0
    fi
    assert_eq "$actual" "$expected" "$label"
}

# summary is the name used in the bd-1n0np.15.1 acceptance text.
summary() {
    harness_summary
}
