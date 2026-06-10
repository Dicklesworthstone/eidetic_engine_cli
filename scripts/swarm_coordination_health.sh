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
    run_joined mcp curl -sS --max-time 2 \
        --write-out $'\n__EE_HTTP_STATUS__:%{http_code}' \
        "$HEALTH_URL"
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
    single_status=127
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
export mcp_output

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

def split_http_output(value):
    if not isinstance(value, str):
        return "", None
    marker = "\n__EE_HTTP_STATUS__:"
    if marker not in value:
        return value, None
    body, status_text = value.rsplit(marker, 1)
    try:
        status = int(status_text.strip().splitlines()[0])
    except (IndexError, ValueError):
        status = None
    return body, status

def http_status_requires_fallback(status):
    return isinstance(status, int) and not (200 <= status < 400)

def bounded_health_level(value):
    if not isinstance(value, str):
        return None
    normalized = value.lower()
    if normalized in {"green", "yellow", "red"}:
        return normalized
    return None

def bounded_semantic_readiness_status(value):
    if not isinstance(value, str):
        return None
    normalized = value.lower()
    if normalized == "ok":
        return "pass"
    if normalized in {"pass", "fail"}:
        return normalized
    return None

def semantic_readiness_reason_class(value):
    if not isinstance(value, str):
        return "unknown"
    normalized = value.lower()
    if (
        normalized == "malformed_sqlite"
        or ("sqlite" in normalized and "malformed" in normalized)
        or "database disk image is malformed" in normalized
    ):
        return "malformed_sqlite"
    if (
        normalized == "archive_corruption"
        or (
            "archive" in normalized
            and (
                "corrupt" in normalized
                or "parse" in normalized
                or "jsonl" in normalized
            )
        )
    ):
        return "archive_corruption"
    if (
        normalized == "index_rebuild_required"
        or ("index" in normalized and ("missing" in normalized or "stale" in normalized))
    ):
        return "index_rebuild_required"
    if normalized == "permission_denied" or "permission denied" in normalized:
        return "permission_denied"
    return "unknown"

def bounded_recovery_mode(value):
    if not isinstance(value, str):
        return None
    normalized = value.lower()
    if normalized in {"", "ok", "none", "normal", "clean", "idle"}:
        return "ok"
    if normalized == "corrupt":
        return "corrupt"
    if normalized in {"repair", "repairing", "recovering", "restore", "restoring", "reconstruct"}:
        return "repair_required"
    return "unknown_recovery"

def recovery_reason_class(recovery):
    if not isinstance(recovery, dict):
        return "unknown"
    mode = bounded_recovery_mode(recovery.get("mode") or recovery.get("status"))
    if mode == "corrupt":
        return "archive_corruption"
    text = " ".join(
        str(recovery.get(key, ""))
        for key in ("next_action", "nextAction", "detail", "message", "bundle_path", "bundlePath")
    ).lower()
    if "doctor repair" in text or "restore" in text or "reconstruct" in text:
        return "storage_recovery_required"
    if "permission denied" in text:
        return "permission_denied"
    return "unknown"

def recovery_summary_from_mode(mode, recovery):
    recovery_mode = bounded_recovery_mode(mode)
    if not recovery_mode or recovery_mode == "ok":
        return None
    return {
        "mode": recovery_mode,
        "reason": recovery_reason_class(recovery),
    }

def mcp_health_summary():
    if env_int("mcp_status") != 0:
        return {}
    body, _http_status = split_http_output(os.environ.get("mcp_output", ""))
    try:
        value = json.loads(body)
    except json.JSONDecodeError:
        return {}
    if not isinstance(value, dict):
        return {}

    summary = {}
    health_level = bounded_health_level(
        value.get("health_level") or value.get("healthLevel")
    )
    if health_level:
        summary["health_level"] = health_level

    semantic = value.get("semantic_readiness") or value.get("semanticReadiness")
    if isinstance(semantic, str):
        semantic_status = semantic
        semantic_reason = (
            value.get("semantic_readiness_reason")
            or value.get("semanticReadinessReason")
        )
    elif isinstance(semantic, dict):
        semantic_status = semantic.get("status")
        semantic_reason = (
            semantic.get("reason")
            or semantic.get("detail")
            or semantic.get("message")
            or value.get("semantic_readiness_reason")
            or value.get("semanticReadinessReason")
        )
    else:
        semantic_status = None
        semantic_reason = None

    semantic_status = bounded_semantic_readiness_status(semantic_status)
    if semantic_status:
        readiness = {"status": semantic_status}
        if semantic_status == "fail":
            readiness["reason"] = semantic_readiness_reason_class(semantic_reason)
        summary["semantic_readiness"] = readiness

    recovery = value.get("recovery")
    if isinstance(recovery, dict):
        recovery_summary = recovery_summary_from_mode(
            recovery.get("mode") or recovery.get("status"),
            recovery,
        )
        if recovery_summary:
            summary["recovery"] = recovery_summary

    if "recovery" not in summary:
        durability_state = value.get("durability_state") or value.get("durabilityState")
        recovery_summary = recovery_summary_from_mode(
            durability_state,
            {
                "mode": durability_state,
                "status": value.get("status"),
                "detail": value.get("detail") or value.get("message"),
            },
        )
        if recovery_summary:
            summary["recovery"] = recovery_summary

    return summary

panic = os.environ.get("observed_panic", "")
health_summary = mcp_health_summary()
_mcp_body, mcp_http_status = split_http_output(os.environ.get("mcp_output", ""))
semantic_readiness_failed = (
    health_summary.get("semantic_readiness", {}).get("status") == "fail"
)
recovery_requires_fallback = (
    health_summary.get("recovery", {}).get("mode") not in {None, "ok"}
)
mcp_http_status_failed = http_status_requires_fallback(mcp_http_status)
event = {
    "schema": os.environ["SCHEMA"],
    "timestamp": os.environ["timestamp"],
    "mcp_http_reachable": env_bool("mcp_ok"),
    "am_agents_list_ok": env_bool("agents_ok"),
    "am_send_single_recipient_ok": env_bool("single_ok"),
    "am_send_multi_recipient_ok": env_bool("multi_ok"),
    "observed_panic": panic or None,
    "fallback_active": env_bool("fallback_active")
    or mcp_http_status_failed
    or semantic_readiness_failed
    or recovery_requires_fallback,
    "checks": {
        "mcp_http": {
            "url": os.environ["HEALTH_URL"],
            "exit_code": env_int("mcp_status"),
            "http_status": mcp_http_status,
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
event.update(health_summary)
print(json.dumps(event, sort_keys=True))
PY
