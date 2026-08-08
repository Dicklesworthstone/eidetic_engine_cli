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
#   fake_idp_restart                  -> real process loss, same durable state
#   fake_idp_stop                     -> terminate + reap; retain evidence

set -euo pipefail

FAKE_IDP_PID="${FAKE_IDP_PID:-}"
FAKE_IDP_DIR="${FAKE_IDP_DIR:-}"
FAKE_IDP_BASE="${FAKE_IDP_BASE:-}"
FAKE_IDP_CA="${FAKE_IDP_CA:-}"
FAKE_IDP_SCENARIO="${FAKE_IDP_SCENARIO:-}"
FAKE_IDP_RETAINED_DIR="${FAKE_IDP_RETAINED_DIR:-}"
FAKE_IDP_PROCESS_GENERATION="${FAKE_IDP_PROCESS_GENERATION:-}"

_fake_idp_script_dir() {
    cd "$(dirname "${BASH_SOURCE[0]}")" && pwd
}

_fake_idp_launch() {
    local scenario="${FAKE_IDP_SCENARIO:-}"
    local py
    py="$(_fake_idp_script_dir)/fake_idp.py"
    local scenario_arg=()
    if [ -n "$scenario" ]; then
        scenario_arg=(--scenario "$scenario")
    fi

    python3 "$py" --dir "$FAKE_IDP_DIR" --port 0 "${scenario_arg[@]}" &
    FAKE_IDP_PID=$!

    local ready="$FAKE_IDP_DIR/ready"
    local waited=0
    local port="" ready_pid="" process_generation=""
    while :; do
        if [ -f "$ready" ]; then
            read -r port ready_pid process_generation < "$ready" || true
            if [ "$ready_pid" = "$FAKE_IDP_PID" ] && [ -n "$port" ]; then
                break
            fi
        fi
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

    FAKE_IDP_BASE="https://127.0.0.1:${port}"
    FAKE_IDP_CA="$FAKE_IDP_DIR/ca.pem"
    FAKE_IDP_PROCESS_GENERATION="$process_generation"
    export FAKE_IDP_PID FAKE_IDP_DIR FAKE_IDP_BASE FAKE_IDP_CA
    export FAKE_IDP_SCENARIO FAKE_IDP_RETAINED_DIR FAKE_IDP_PROCESS_GENERATION
}

fake_idp_start() {
    FAKE_IDP_SCENARIO="${1:-}"
    FAKE_IDP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/fake-idp-XXXXXX")"
    FAKE_IDP_RETAINED_DIR="$FAKE_IDP_DIR"
    _fake_idp_launch
}

fake_idp_curl() {
    local path="$1"
    shift
    local arg
    local value_kind=""
    for arg in "$@"; do
        if [ -n "$value_kind" ]; then
            if [ "$value_kind" = "request" ]; then
                case "$arg" in
                    GET|POST) ;;
                    *)
                        echo "fake_idp_curl: unsupported request method: $arg" >&2
                        return 2
                        ;;
                esac
            fi
            value_kind=""
            continue
        fi
        case "$arg" in
            -X|--request)
                value_kind="request"
                ;;
            -H|--header)
                value_kind="header"
                ;;
            -d|--data|--data-raw|--data-binary|--data-urlencode)
                value_kind="data"
                ;;
            --request=GET|--request=POST|--header=*|--data=*|--data-raw=*|\
            --data-binary=*|--data-urlencode=*|--fail|--fail-with-body)
                ;;
            *)
                echo "fake_idp_curl: unsafe routing/credential option rejected: $arg" >&2
                return 2
                ;;
        esac
    done
    if [ -n "$value_kind" ]; then
        echo "fake_idp_curl: option is missing its value" >&2
        return 2
    fi
    env \
        -u ALL_PROXY -u all_proxy \
        -u HTTPS_PROXY -u https_proxy \
        -u HTTP_PROXY -u http_proxy \
        -u NO_PROXY -u no_proxy \
        -u CURL_CA_BUNDLE -u SSL_CERT_FILE -u SSL_CERT_DIR \
        -u SSLKEYLOGFILE -u CURL_SSL_BACKEND \
        -u NETRC -u CURL_HOME \
        curl -q --silent --show-error "$@" \
        --noproxy '*' --proxy '' --netrc-file /dev/null \
        --proto '=https' --proto-redir '=https' --max-redirs 0 \
        --connect-timeout 3 --max-time 10 \
        --cacert "$FAKE_IDP_CA" "${FAKE_IDP_BASE}${path}"
}

fake_idp_control() {
    fake_idp_curl "/_control" -X POST -H "Content-Type: application/json" --data "$1"
}

fake_idp_state() {
    fake_idp_curl "/_state"
}

fake_idp_reap() {
    if [ -n "${FAKE_IDP_PID:-}" ]; then
        if kill -0 "$FAKE_IDP_PID" 2>/dev/null; then
            if [ -n "${FAKE_IDP_BASE:-}" ] && [ -f "${FAKE_IDP_CA:-}" ]; then
                fake_idp_control '{"action":"cancel_lifecycle_trap"}' \
                    >/dev/null 2>&1 || true
            fi
            kill "$FAKE_IDP_PID" 2>/dev/null || true
        fi
        wait "$FAKE_IDP_PID" 2>/dev/null || true
    fi
    FAKE_IDP_PID=""
    export FAKE_IDP_PID
}

fake_idp_restart() {
    if [ -z "${FAKE_IDP_DIR:-}" ] || [ ! -d "$FAKE_IDP_DIR" ]; then
        echo "fake_idp: cannot restart without retained state" >&2
        return 1
    fi
    fake_idp_reap
    _fake_idp_launch
}

fake_idp_stop() {
    fake_idp_reap
    if [ -n "${FAKE_IDP_DIR:-}" ]; then
        FAKE_IDP_RETAINED_DIR="$FAKE_IDP_DIR"
        export FAKE_IDP_RETAINED_DIR
        if [ "${EE_E2E_KEEP_WORKSPACE:-1}" = "1" ]; then
            echo "fake_idp: retained evidence at $FAKE_IDP_RETAINED_DIR" >&2
        fi
    fi
}
