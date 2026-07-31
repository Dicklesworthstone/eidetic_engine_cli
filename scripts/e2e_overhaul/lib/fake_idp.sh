#!/usr/bin/env bash
# bd-tc-epic-qzk7o.8.7 — deterministic fake OIDC IdP harness for team-confed
# tier-2 SSO tests. Sourced from e2e scripts; wraps scripts/e2e_overhaul/lib/
# fake_idp.py. Never requires a real IdP account or outbound network.
#
# Contract:
#   fake_idp_start <scenario.json>   -> starts the server, exports:
#       FAKE_IDP_BASE   (https://127.0.0.1:<port>)
#       FAKE_IDP_CA     (path to the ephemeral CA the client must trust)
#       FAKE_IDP_DIR    (state dir)
#   fake_idp_control '<json>'         -> POST /_control (mutate at runtime)
#   fake_idp_state                    -> GET /_state (inspect)
#   fake_idp_curl <path> [curl args]  -> CA-pinned curl against the server
#   fake_idp_stop                     -> terminate + reap (idempotent)

set -euo pipefail

FAKE_IDP_PID="${FAKE_IDP_PID:-}"
FAKE_IDP_DIR="${FAKE_IDP_DIR:-}"
FAKE_IDP_BASE="${FAKE_IDP_BASE:-}"
FAKE_IDP_CA="${FAKE_IDP_CA:-}"

_fake_idp_script_dir() {
    cd "$(dirname "${BASH_SOURCE[0]}")" && pwd
}

fake_idp_start() {
    local scenario="${1:-}"
    FAKE_IDP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/fake-idp-XXXXXX")"
    local py="$(_fake_idp_script_dir)/fake_idp.py"
    local scenario_arg=()
    if [ -n "$scenario" ]; then
        scenario_arg=(--scenario "$scenario")
    fi

    python3 "$py" --dir "$FAKE_IDP_DIR" --port 0 "${scenario_arg[@]}" &
    FAKE_IDP_PID=$!

    local ready="$FAKE_IDP_DIR/ready"
    local waited=0
    while [ ! -f "$ready" ]; do
        if ! kill -0 "$FAKE_IDP_PID" 2>/dev/null; then
            echo "fake_idp: server exited before ready" >&2
            return 1
        fi
        sleep 0.1
        waited=$((waited + 1))
        if [ "$waited" -gt 100 ]; then
            echo "fake_idp: server did not become ready within 10s" >&2
            fake_idp_stop
            return 1
        fi
    done

    local port
    port="$(cat "$ready")"
    FAKE_IDP_BASE="https://127.0.0.1:${port}"
    FAKE_IDP_CA="$FAKE_IDP_DIR/ca.pem"
    export FAKE_IDP_PID FAKE_IDP_DIR FAKE_IDP_BASE FAKE_IDP_CA
}

fake_idp_curl() {
    local path="$1"
    shift
    curl --silent --show-error --cacert "$FAKE_IDP_CA" "$@" "${FAKE_IDP_BASE}${path}"
}

fake_idp_control() {
    fake_idp_curl "/_control" -X POST -H "Content-Type: application/json" --data "$1"
}

fake_idp_state() {
    fake_idp_curl "/_state"
}

fake_idp_stop() {
    if [ -n "${FAKE_IDP_PID:-}" ] && kill -0 "$FAKE_IDP_PID" 2>/dev/null; then
        kill "$FAKE_IDP_PID" 2>/dev/null || true
        wait "$FAKE_IDP_PID" 2>/dev/null || true
    fi
    FAKE_IDP_PID=""
    if [ -n "${FAKE_IDP_DIR:-}" ] && [ -d "$FAKE_IDP_DIR" ]; then
        rm -rf "$FAKE_IDP_DIR"
    fi
    FAKE_IDP_DIR=""
}
