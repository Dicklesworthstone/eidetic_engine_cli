#!/usr/bin/env bash
# F4.networkx - NetworkX differential test driver.
#
# Runs the heavyweight Python NetworkX comparison behind the
# `differential-networkx` Cargo feature and emits structured e2e log events so
# nightly CI and agent closeouts can distinguish missing Python dependencies
# from real fnx-vs-NetworkX ranking divergence.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
export REPO_ROOT

# shellcheck source=scripts/lib/e2e_logger.sh
source "$REPO_ROOT/scripts/lib/e2e_logger.sh"

NETWORKX_WATCHDOG_PID=""

start_networkx_watchdog() {
    local max_seconds="${EE_NETWORKX_DIFFERENTIAL_MAX_SECONDS:-600}"
    (
        sleep "$max_seconds"
        echo "networkx_differential: timed out after ${max_seconds}s" >&2
        kill -TERM "$$" 2>/dev/null || true
    ) &
    NETWORKX_WATCHDOG_PID="$!"
}

stop_networkx_watchdog() {
    if [ -n "$NETWORKX_WATCHDOG_PID" ]; then
        kill "$NETWORKX_WATCHDOG_PID" 2>/dev/null || true
    fi
}

preflight_networkx() {
    python3 - <<'PY'
import networkx as nx
print(nx.__version__)
PY
}

run_networkx_differential() {
    local cargo_cmd=(
        cargo test
        --features differential-networkx
        --test networkx_differential
        -- --nocapture
    )
    e2e_log_note "networkx_differential_command=${cargo_cmd[*]}"
    e2e_log_assert_eq "true" "true" "networkx_differential_preflight_networkx_imported"
    "${cargo_cmd[@]}"
}

e2e_log_start "networkx_differential"
start_networkx_watchdog
trap 'stop_networkx_watchdog; e2e_log_end' EXIT

if NETWORKX_VERSION="$(preflight_networkx 2>&1)"; then
    e2e_log_note "networkx_differential_environment python_networkx=${NETWORKX_VERSION}"
else
    e2e_log_note "networkx_differential_networkx_missing output=${NETWORKX_VERSION}"
    e2e_log_assert_eq "missing_python_networkx" "available_python_networkx" \
        "networkx_differential_preflight_networkx_imported"
    exit 3
fi

if run_networkx_differential; then
    e2e_log_assert_eq "true" "true" "networkx_differential_cargo_test_passed"
else
    e2e_log_assert_eq "cargo_test_failed" "cargo_test_passed" \
        "networkx_differential_cargo_test_passed"
    exit 3
fi
