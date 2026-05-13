#!/usr/bin/env bash
# J11.4 — Non-destructive bead closeout audit (bd-17c65.10.11.4).
#
# Run BEFORE marking a bead closed to summarize verification
# evidence, retained artifacts, dependencies, and known caveats.
# Reports closure readiness as `ready`, `ready_with_caveats`, or
# `blocked`, with structured reasons an agent can act on or print
# verbatim into a `br close --reason` invocation.
#
# **Non-destructive contract:** this script NEVER mutates beads,
# git, files, agent-mail reservations, or the cargo target.
# It reads, classifies, and reports. Closing the bead is still an
# explicit operator action (`br close <id> --reason ...`).
#
# Usage:
#   scripts/closeout_audit.sh --bead <id> [--json] [--workspace-root <path>]
#
# Exit codes:
#   0  success — readiness emitted (could be any of ready/caveats/blocked)
#   2  usage error (bad args)
#   3  bead not found
#   4  required tool missing (jq, git)
#
# JSON schema: ee.closeout_audit.v1
#
# Wired by tests/closeout_audit_runner_unit.rs which invokes the
# script against three fixture scenarios (ready, ready_with_caveats,
# blocked) and asserts the readiness classification + structural
# shape of the JSON output.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT_DEFAULT="$(cd "$SCRIPT_DIR/.." && pwd)"
SCHEMA_ID="ee.closeout_audit.v1"

usage() {
    cat <<'USAGE'
usage: scripts/closeout_audit.sh --bead <id> [--json] [--workspace-root <path>]

Examples:
  scripts/closeout_audit.sh --bead bd-17c65.4.9 --json
  scripts/closeout_audit.sh --bead bd-17c65.11.3 --workspace-root /tmp/test-ws --json
USAGE
}

# Argument parsing.
BEAD_ID=""
JSON_OUTPUT=0
WORKSPACE_ROOT="$REPO_ROOT_DEFAULT"

while [ "$#" -gt 0 ]; do
    case "$1" in
        --bead)
            BEAD_ID="${2:-}"
            if [ -z "$BEAD_ID" ]; then
                echo "closeout_audit: --bead requires a value" >&2
                usage >&2
                exit 2
            fi
            shift 2
            ;;
        --json)
            JSON_OUTPUT=1
            shift
            ;;
        --workspace-root)
            WORKSPACE_ROOT="${2:-}"
            if [ -z "$WORKSPACE_ROOT" ]; then
                echo "closeout_audit: --workspace-root requires a path" >&2
                usage >&2
                exit 2
            fi
            shift 2
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            echo "closeout_audit: unknown argument: $1" >&2
            usage >&2
            exit 2
            ;;
    esac
done

if [ -z "$BEAD_ID" ]; then
    echo "closeout_audit: --bead is required" >&2
    usage >&2
    exit 2
fi

# Tool preflight. jq is required for JSONL parsing; git is required
# for uncommitted-references scan.
for tool in jq git; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        echo "closeout_audit: required tool missing: $tool" >&2
        exit 4
    fi
done

ISSUES_JSONL="$WORKSPACE_ROOT/.beads/issues.jsonl"
if [ ! -f "$ISSUES_JSONL" ]; then
    echo "closeout_audit: no beads JSONL at $ISSUES_JSONL" >&2
    exit 3
fi

# Extract the bead's JSON line. The JSONL has one issue per line so a
# fixed-string grep + jq filter is reliable + fast.
BEAD_JSON="$(grep -F "\"id\":\"$BEAD_ID\"" "$ISSUES_JSONL" | head -1 || true)"
if [ -z "$BEAD_JSON" ]; then
    echo "closeout_audit: bead not found in $ISSUES_JSONL: $BEAD_ID" >&2
    exit 3
fi

BEAD_STATUS="$(printf '%s' "$BEAD_JSON" | jq -r '.status // "unknown"')"
BEAD_ASSIGNEE="$(printf '%s' "$BEAD_JSON" | jq -r '.assignee // ""')"
BEAD_TITLE="$(printf '%s' "$BEAD_JSON" | jq -r '.title // ""')"

# Collect open dependencies (the bead is blocked by these).
DEPS_JSON="$(printf '%s' "$BEAD_JSON" | jq -c '[.dependencies // [] | .[] | select(.type == "blocks") | .depends_on_id]')"

# For each dep, look up its status; open deps go into a blocker list.
OPEN_DEPS_JSON="[]"
if [ "$DEPS_JSON" != "[]" ] && [ "$DEPS_JSON" != "null" ]; then
    OPEN_DEPS_BUFFER="$(mktemp)"
    trap 'rm -f "$OPEN_DEPS_BUFFER"' EXIT
    printf '%s\n' "$DEPS_JSON" | jq -r '.[]' | while IFS= read -r dep_id; do
        [ -z "$dep_id" ] && continue
        dep_line="$(grep -F "\"id\":\"$dep_id\"" "$ISSUES_JSONL" | head -1 || true)"
        if [ -z "$dep_line" ]; then
            printf '{"id":"%s","status":"missing"}\n' "$dep_id" >> "$OPEN_DEPS_BUFFER"
            continue
        fi
        dep_status="$(printf '%s' "$dep_line" | jq -r '.status // "unknown"')"
        if [ "$dep_status" != "closed" ] && [ "$dep_status" != "deferred" ]; then
            jq -nc --arg id "$dep_id" --arg s "$dep_status" '{id:$id,status:$s}' >> "$OPEN_DEPS_BUFFER"
        fi
    done
    if [ -s "$OPEN_DEPS_BUFFER" ]; then
        OPEN_DEPS_JSON="$(jq -s '.' < "$OPEN_DEPS_BUFFER")"
    fi
fi

# Count uncommitted files in git that reference the bead id. Many
# beads are referenced in commit messages on closure; if there are
# uncommitted files still mentioning the bead, the work may not be
# finished.
UNCOMMITTED_REFS_JSON="[]"
if cd "$WORKSPACE_ROOT" 2>/dev/null; then
    UNCOMMITTED_REFS_RAW="$(git status --porcelain 2>/dev/null \
        | awk 'NF >= 2 && $1 != "??" { sub(/^...\W*/, ""); print }' \
        | xargs -I{} sh -c 'grep -lF "'"$BEAD_ID"'" "{}" 2>/dev/null || true' \
        | sort -u || true)"
    if [ -n "$UNCOMMITTED_REFS_RAW" ]; then
        UNCOMMITTED_REFS_JSON="$(printf '%s\n' "$UNCOMMITTED_REFS_RAW" \
            | jq -Rn '[inputs | select(length > 0)]')"
    fi
fi

# Check rch readiness. `rch check` returns 0 when workers reachable.
# We don't probe whether a specific build was offloaded — that's a
# per-invocation property — only whether rch as a system is healthy.
RCH_STATUS="unknown"
if command -v rch >/dev/null 2>&1; then
    if rch check >/dev/null 2>&1; then
        RCH_STATUS="ready"
    else
        RCH_STATUS="local_fallback_likely"
    fi
fi

# Check agent mail reachability. Same liveness probe used in other
# scripts. Note: an unreachable agent-mail server does NOT block
# closure — many beads have no agent-mail evidence — it just feeds
# into the caveat list.
AGENT_MAIL_STATUS="unknown"
AGENT_MAIL_HOST_PORT="${AGENT_MAIL_HOST:-127.0.0.1}:${AGENT_MAIL_PORT:-8765}"
if command -v curl >/dev/null 2>&1; then
    if curl -fsS --connect-timeout 2 --max-time 4 \
            "http://${AGENT_MAIL_HOST_PORT}/health" >/dev/null 2>&1; then
        AGENT_MAIL_STATUS="reachable"
    else
        AGENT_MAIL_STATUS="unreachable"
    fi
fi

# Check J1 log presence — the structured-test-log path agents emit
# evidence into. Optional but useful if the bead's verification
# captured timed events.
J1_LOG_PRESENT=false
J1_LOG_PATH=""
if [ -d "$WORKSPACE_ROOT/tests/logs/active" ]; then
    if compgen -G "$WORKSPACE_ROOT/tests/logs/active/*.jsonl" > /dev/null; then
        J1_LOG_PRESENT=true
        J1_LOG_PATH="$WORKSPACE_ROOT/tests/logs/active"
    fi
fi

# Aggregate readiness.
BLOCKERS=()
CAVEATS=()
NEXT_ACTIONS=()

# Blockers
OPEN_DEPS_COUNT="$(printf '%s' "$OPEN_DEPS_JSON" | jq 'length')"
if [ "$OPEN_DEPS_COUNT" -gt 0 ]; then
    BLOCKERS+=("open_dependencies: ${OPEN_DEPS_COUNT} dep(s) not yet closed")
    NEXT_ACTIONS+=("close or force-close the open dependencies; review each via 'br show <id>'")
fi
UNCOMMITTED_REFS_COUNT="$(printf '%s' "$UNCOMMITTED_REFS_JSON" | jq 'length')"
if [ "$UNCOMMITTED_REFS_COUNT" -gt 0 ]; then
    BLOCKERS+=("uncommitted_files_reference_bead: ${UNCOMMITTED_REFS_COUNT} file(s) still mention ${BEAD_ID}")
    NEXT_ACTIONS+=("commit or revert the uncommitted files that reference ${BEAD_ID}")
fi

# Caveats
if [ "$RCH_STATUS" = "local_fallback_likely" ]; then
    CAVEATS+=("rch_health_check_failed: cargo evidence captured this session may have been local fallback rather than offloaded; verify before closure if the bead required remote builds")
    NEXT_ACTIONS+=("re-run cargo verification with explicit rch routing OR document the local-fallback context in the close_reason")
fi
if [ "$AGENT_MAIL_STATUS" = "unreachable" ]; then
    CAVEATS+=("agent_mail_unreachable: reservation/inbox evidence could not be captured at audit time; rely on commit-message coordination")
fi
if [ "$J1_LOG_PRESENT" = "false" ]; then
    CAVEATS+=("j1_log_absent: no tests/logs/active/*.jsonl found; this is fine for beads that didn't run e2e drivers but means no structured timing evidence is retained")
fi

# Readiness classification:
#  - blocked: any blocker present (open deps, uncommitted files)
#  - ready_with_caveats: no blockers but ≥1 caveat
#  - ready: clean
if [ "${#BLOCKERS[@]}" -gt 0 ]; then
    READINESS="blocked"
elif [ "${#CAVEATS[@]}" -gt 0 ]; then
    READINESS="ready_with_caveats"
else
    READINESS="ready"
fi
NEXT_ACTIONS+=("review the audit JSON, then run: br close ${BEAD_ID} --reason '<close reason citing this audit>'")

# Build output JSON via jq for safe escaping.
BLOCKERS_JSON="$(printf '%s\n' "${BLOCKERS[@]:-}" | jq -Rn '[inputs | select(length > 0)]')"
CAVEATS_JSON="$(printf '%s\n' "${CAVEATS[@]:-}" | jq -Rn '[inputs | select(length > 0)]')"
NEXT_ACTIONS_JSON="$(printf '%s\n' "${NEXT_ACTIONS[@]:-}" | jq -Rn '[inputs | select(length > 0)]')"

RESULT_JSON="$(jq -nc \
    --arg schema "$SCHEMA_ID" \
    --arg bead_id "$BEAD_ID" \
    --arg readiness "$READINESS" \
    --arg bead_status "$BEAD_STATUS" \
    --arg bead_assignee "$BEAD_ASSIGNEE" \
    --arg bead_title "$BEAD_TITLE" \
    --argjson open_deps "$OPEN_DEPS_JSON" \
    --argjson uncommitted_refs "$UNCOMMITTED_REFS_JSON" \
    --arg rch_status "$RCH_STATUS" \
    --arg agent_mail_status "$AGENT_MAIL_STATUS" \
    --argjson j1_log_present "$J1_LOG_PRESENT" \
    --arg j1_log_path "$J1_LOG_PATH" \
    --argjson blockers "$BLOCKERS_JSON" \
    --argjson caveats "$CAVEATS_JSON" \
    --argjson next_actions "$NEXT_ACTIONS_JSON" \
    '{
        schema: $schema,
        bead_id: $bead_id,
        readiness: $readiness,
        evidence: {
            bead_status: $bead_status,
            bead_assignee: $bead_assignee,
            bead_title: $bead_title,
            open_dependencies: $open_deps,
            uncommitted_files_referencing_bead: $uncommitted_refs,
            rch_status: $rch_status,
            agent_mail_status: $agent_mail_status,
            j1_log_present: $j1_log_present,
            j1_log_path: $j1_log_path
        },
        blockers: $blockers,
        caveats: $caveats,
        next_actions: $next_actions
    }')"

if [ "$JSON_OUTPUT" -eq 1 ]; then
    printf '%s\n' "$RESULT_JSON"
else
    # Human-readable summary that still surfaces every field the JSON
    # carries, just in flowing prose. Useful when the operator is
    # running ad-hoc without --json.
    printf 'Closeout audit for %s\n' "$BEAD_ID"
    printf '  readiness: %s\n' "$READINESS"
    printf '  status: %s\n' "$BEAD_STATUS"
    if [ -n "$BEAD_ASSIGNEE" ]; then
        printf '  assignee: %s\n' "$BEAD_ASSIGNEE"
    fi
    printf '  rch: %s\n' "$RCH_STATUS"
    printf '  agent_mail: %s\n' "$AGENT_MAIL_STATUS"
    printf '  j1_log: %s\n' "$J1_LOG_PRESENT"
    if [ "$OPEN_DEPS_COUNT" -gt 0 ]; then
        printf '  open_dependencies (%d):\n' "$OPEN_DEPS_COUNT"
        printf '%s' "$OPEN_DEPS_JSON" | jq -r '.[] | "    - \(.id) [\(.status)]"'
    fi
    if [ "$UNCOMMITTED_REFS_COUNT" -gt 0 ]; then
        printf '  uncommitted files referencing bead (%d):\n' "$UNCOMMITTED_REFS_COUNT"
        printf '%s' "$UNCOMMITTED_REFS_JSON" | jq -r '.[] | "    - \(.)"'
    fi
    if [ "${#BLOCKERS[@]}" -gt 0 ]; then
        printf '  blockers:\n'
        for b in "${BLOCKERS[@]}"; do printf '    - %s\n' "$b"; done
    fi
    if [ "${#CAVEATS[@]}" -gt 0 ]; then
        printf '  caveats:\n'
        for c in "${CAVEATS[@]}"; do printf '    - %s\n' "$c"; done
    fi
    if [ "${#NEXT_ACTIONS[@]}" -gt 0 ]; then
        printf '  next actions:\n'
        for a in "${NEXT_ACTIONS[@]}"; do printf '    - %s\n' "$a"; done
    fi
fi

exit 0
