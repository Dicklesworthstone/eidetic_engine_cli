#!/usr/bin/env bash
# Emit a JSON health event for Agent Mail backed swarm coordination.

set -uo pipefail

SCHEMA="ee.swarm.coordination_health.v1"
HEALTH_URL="${AGENT_MAIL_HEALTH_URL:-http://127.0.0.1:8765/health}"
AM_BIN="${AGENT_MAIL_AM_BIN:-am}"
PROJECT="${AGENT_MAIL_PROJECT:-${PWD}}"
FROM_AGENT="${AGENT_MAIL_FROM:-${AGENT_NAME:-CoordinationHealth}}"
SINGLE_TO="${AGENT_MAIL_SINGLE_TO:-$FROM_AGENT}"
MULTI_TO="${AGENT_MAIL_MULTI_TO:-${FROM_AGENT},CoordinationHealthPeer}"
SUBJECT="${AGENT_MAIL_HEALTH_SUBJECT:-coordination-health-ping}"
BODY="${AGENT_MAIL_HEALTH_BODY:-ping}"

run_joined() {
    local prefix="${1:?prefix required}"
    shift
    local output status
    output="$("$@" 2>&1)"
    status=$?
    printf -v "${prefix}_status" "%s" "$status"
    printf -v "${prefix}_output" "%s" "$output"
}

bool_from_status() {
    if [ "${1:-1}" -eq 0 ]; then
        printf 'true'
    else
        printf 'false'
    fi
}

extract_panic() {
    local text="${1:-}"
    if printf '%s' "$text" | grep -Fq "RefCell already borrowed"; then
        printf '%s' "RefCell already borrowed"
        return 0
    fi
    if printf '%s' "$text" | grep -Fq "panicked at"; then
        printf '%s' "$(printf '%s' "$text" | grep -F "panicked at" | head -n 1)"
        return 0
    fi
    printf '%s' ""
}

if command -v curl >/dev/null 2>&1; then
    run_joined mcp curl -fsS --max-time 2 "$HEALTH_URL"
else
    mcp_status=127
    mcp_output="curl not found"
fi

if command -v "$AM_BIN" >/dev/null 2>&1; then
    run_joined agents "$AM_BIN" agents list --project "$PROJECT" --json
    run_joined single "$AM_BIN" mail send \
        --project "$PROJECT" \
        --from "$FROM_AGENT" \
        --to "$SINGLE_TO" \
        --subject "$SUBJECT" \
        --body "$BODY" \
        --json
    run_joined multi "$AM_BIN" mail send \
        --project "$PROJECT" \
        --from "$FROM_AGENT" \
        --to "$MULTI_TO" \
        --subject "$SUBJECT" \
        --body "$BODY" \
        --json
else
    agents_status=127
    agents_output="$AM_BIN not found"
    single_status=127
    single_output="$AM_BIN not found"
    multi_status=127
    multi_output="$AM_BIN not found"
fi

mcp_ok="$(bool_from_status "$mcp_status")"
agents_ok="$(bool_from_status "$agents_status")"
single_ok="$(bool_from_status "$single_status")"
multi_ok="$(bool_from_status "$multi_status")"
observed_panic="$(extract_panic "${multi_output:-}")"
fallback_active=false
if [ "$mcp_ok" != "true" ] || [ "$agents_ok" != "true" ] || \
    [ "$single_ok" != "true" ] || [ "$multi_ok" != "true" ]; then
    fallback_active=true
fi

timestamp="$(date -u +"%Y-%m-%dT%H:%M:%SZ")"

export SCHEMA timestamp HEALTH_URL AM_BIN PROJECT FROM_AGENT SINGLE_TO MULTI_TO
export mcp_ok agents_ok single_ok multi_ok observed_panic fallback_active
export mcp_status agents_status single_status multi_status

python3 - <<'PY'
import json
import os

def env_bool(name: str) -> bool:
    return os.environ.get(name) == "true"

def env_int(name: str) -> int:
    try:
        return int(os.environ.get(name, "0"))
    except ValueError:
        return 0

panic = os.environ.get("observed_panic", "")
event = {
    "schema": os.environ["SCHEMA"],
    "timestamp": os.environ["timestamp"],
    "mcp_http_reachable": env_bool("mcp_ok"),
    "am_agents_list_ok": env_bool("agents_ok"),
    "am_send_single_recipient_ok": env_bool("single_ok"),
    "am_send_multi_recipient_ok": env_bool("multi_ok"),
    "observed_panic": panic or None,
    "fallback_active": env_bool("fallback_active"),
    "checks": {
        "mcp_http": {
            "url": os.environ["HEALTH_URL"],
            "exit_code": env_int("mcp_status"),
        },
        "am_agents_list": {
            "binary": os.environ["AM_BIN"],
            "project": os.environ["PROJECT"],
            "exit_code": env_int("agents_status"),
        },
        "am_send_single_recipient": {
            "from": os.environ["FROM_AGENT"],
            "to": os.environ["SINGLE_TO"],
            "exit_code": env_int("single_status"),
        },
        "am_send_multi_recipient": {
            "from": os.environ["FROM_AGENT"],
            "to": os.environ["MULTI_TO"],
            "exit_code": env_int("multi_status"),
        },
    },
}
print(json.dumps(event, sort_keys=True))
PY
